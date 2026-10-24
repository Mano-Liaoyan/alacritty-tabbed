//! Context menus, opened by a right click: a pane's, to split, zoom, move or close it or reset its
//! text size, and a tab's, to rename, ungroup, move or close it.

use std::cmp::Ordering;
use std::rc::Rc;
use std::time::Instant;

use winit::dpi::PhysicalPosition;
use winit::event::{KeyEvent, MouseButton};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, WindowId};

use alacritty_terminal::term::TermMode;

use crate::config::UiConfig;
use crate::event::Event;
use crate::window_context::WindowContext;

use super::anim::{Curve, Tween};
use super::bar::{Hit, ITEM_RADIUS, Layout, MARGIN, TRACK_RADIUS};
use super::dnd::{Source, Target};
use super::paint::{BLACK, Layer, Palette, Rect, UNZOOM_GLYPH, ZOOM_GLYPH, mix};
use super::split::{Axis, Split};
use super::tear::{can_tear, tear_off};
use super::{
    FontChange, Press, Tab, Tabs, close as close_tab, close_pane, focus_pane, move_pane, redraw,
    relayout, rename, split, toggle_zoom, zoom_live,
};

/// Something a menu offers to do with the pane or the tab it was opened on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    /// Put a new pane beside this one, with a vertical divider between them.
    SplitVertically,
    /// Put a new pane below this one, with a horizontal divider between them.
    SplitHorizontally,
    /// Show only this pane, over the whole tab.
    Zoom,
    Unzoom,
    /// Give the pane's text the size of its configuration again.
    ResetTextSize,
    MoveToNewTab,
    /// Move the pane, or the tab, to a window of its own.
    NewWindow,
    /// Name the tab, which is a group of panes or a single one.
    Rename {
        group: bool,
    },
    ResetName,
    /// Make each pane of the tab a tab of its own.
    Ungroup,
    Close,
}

impl Item {
    /// The item's label, in a tab's menu or else a pane's.
    fn label(self, tab: bool) -> &'static str {
        match self {
            Item::SplitVertically => "Split Vertically",
            Item::SplitHorizontally => "Split Horizontally",
            Item::Zoom => "Zoom Pane",
            Item::Unzoom => "Unzoom Pane",
            Item::ResetTextSize => "Reset Text Size",
            Item::MoveToNewTab => "Move Pane to New Tab",
            Item::NewWindow if tab => "Move Tab to New Window",
            Item::NewWindow => "Move Pane to New Window",
            Item::Rename { group: true } => "Rename Group…",
            Item::Rename { group: false } => "Rename Tab…",
            Item::ResetName => "Reset Name",
            Item::Ungroup => "Ungroup",
            Item::Close if tab => "Close Tab",
            Item::Close => "Close Pane",
        }
    }

    /// Strokes of the item's glyph, in units of half its size.
    fn glyph(self) -> &'static [[f32; 4]] {
        const BOX: [[f32; 4]; 4] =
            [[-1., -1., 1., -1.], [1., -1., 1., 1.], [1., 1., -1., 1.], [-1., 1., -1., -1.]];
        match self {
            Item::SplitVertically => &[BOX[0], BOX[1], BOX[2], BOX[3], [0., -1., 0., 1.]],
            Item::SplitHorizontally => &[BOX[0], BOX[1], BOX[2], BOX[3], [-1., 0., 1., 0.]],
            Item::Zoom => ZOOM_GLYPH,
            Item::Unzoom => UNZOOM_GLYPH,
            // A magnifying glass.
            Item::ResetTextSize => &[
                [0.45, -0.2, 0.26, 0.26],
                [0.26, 0.26, -0.2, 0.45],
                [-0.2, 0.45, -0.66, 0.26],
                [-0.66, 0.26, -0.85, -0.2],
                [-0.85, -0.2, -0.66, -0.66],
                [-0.66, -0.66, -0.2, -0.85],
                [-0.2, -0.85, 0.26, -0.66],
                [0.26, -0.66, 0.45, -0.2],
                [0.26, 0.26, 1., 1.],
            ],
            // An arrow leaving a box by its open corner.
            Item::MoveToNewTab => &[
                [0., -1., -1., -1.],
                BOX[3],
                BOX[2],
                [1., 1., 1., 0.],
                [-0.2, 0.2, 1., -1.],
                [0.2, -1., 1., -1.],
                [1., -1., 1., -0.6],
            ],
            // A window in front of another.
            Item::NewWindow => &[
                [-1., -0.4, 0.4, -0.4],
                [0.4, -0.4, 0.4, 1.],
                [0.4, 1., -1., 1.],
                [-1., 1., -1., -0.4],
                [-0.4, -0.4, -0.4, -1.],
                [-0.4, -1., 1., -1.],
                [1., -1., 1., 0.4],
                [1., 0.4, 0.4, 0.4],
            ],
            // A pencil, writing down to the left.
            Item::Rename { .. } => &[
                [-1., 1., -0.8, 0.4],
                [-1., 1., -0.4, 0.8],
                [-0.8, 0.4, 0.6, -1.],
                [-0.4, 0.8, 1., -0.6],
                [0.6, -1., 1., -0.6],
            ],
            // An arrow turning back around.
            Item::ResetName => &[
                [0., -0.8, 0.57, -0.57],
                [0.57, -0.57, 0.8, 0.],
                [0.8, 0., 0.57, 0.57],
                [0.57, 0.57, 0., 0.8],
                [0., 0.8, -0.57, 0.57],
                [-0.57, 0.57, -0.8, 0.],
                [-0.8, 0., -0.57, -0.57],
                [-0.57, -0.57, -0.95, -0.6],
                [-0.57, -0.57, -0.5, -0.95],
            ],
            // Two boxes apart.
            Item::Ungroup => &[
                [-1., -0.5, -0.2, -0.5],
                [-0.2, -0.5, -0.2, 0.5],
                [-0.2, 0.5, -1., 0.5],
                [-1., 0.5, -1., -0.5],
                [0.2, -0.5, 1., -0.5],
                [1., -0.5, 1., 0.5],
                [1., 0.5, 0.2, 0.5],
                [0.2, 0.5, 0.2, -0.5],
            ],
            Item::Close => &[[-0.7, -0.7, 0.7, 0.7], [-0.7, 0.7, 0.7, -0.7]],
        }
    }

    /// Whether a separator divides this item from `previous`.
    fn separated(self, previous: Item) -> bool {
        let section = |item| match item {
            Item::SplitVertically | Item::SplitHorizontally => 0,
            Item::Rename { .. } | Item::ResetName => 0,
            Item::Zoom | Item::Unzoom | Item::ResetTextSize => 1,
            Item::MoveToNewTab | Item::NewWindow | Item::Ungroup => 2,
            Item::Close => 3,
        };
        section(self) != section(previous)
    }
}

/// What the menu offers for a pane of a tab with `panes` panes, `zoomed` or not, whose text is
/// `sized` away from its configuration's or not, and which can `tear` off into a window of its
/// own or not.
fn pane_items(panes: usize, zoomed: bool, sized: bool, tear: bool) -> Vec<Item> {
    let mut items = vec![Item::SplitVertically, Item::SplitHorizontally];
    if panes > 1 {
        items.push(if zoomed { Item::Unzoom } else { Item::Zoom });
    }
    if sized {
        items.push(Item::ResetTextSize);
    }
    if panes > 1 {
        items.push(Item::MoveToNewTab);
    }
    if tear {
        items.push(Item::NewWindow);
    }
    items.push(Item::Close);
    items
}

/// What the menu offers for a tab with `panes` panes, `named` or not, which can `tear` off into a
/// window of its own or not.
fn tab_items(panes: usize, named: bool, tear: bool) -> Vec<Item> {
    let mut items = vec![Item::Rename { group: panes > 1 }];
    if named {
        items.push(Item::ResetName);
    }
    if panes > 1 {
        items.push(Item::Ungroup);
    }
    if tear {
        items.push(Item::NewWindow);
    }
    items.push(Item::Close);
    items
}

/// What a menu was opened on, by the id of a pane, so it follows the tab as tabs move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// The live pane.
    Pane(WindowId),
    /// The tab holding this pane.
    Tab(WindowId),
}

/// Sizes of the menu's parts, in pixels.
#[derive(Clone, Copy)]
struct Metrics {
    width: f32,
    row: f32,
    /// Height of a separator.
    gap: f32,
    /// Space around the rows.
    pad: f32,
}

/// The menu with `items` for a pointer at `at`, kept inside `bounds`, and the rect of each row.
///
/// It opens below and right of the pointer, or above and left of it where there is no room.
fn geometry(items: &[Item], at: (f32, f32), metrics: Metrics, bounds: Rect) -> (Rect, Vec<Rect>) {
    let Metrics { width, row, gap, pad } = metrics;
    let mut rows = Vec::with_capacity(items.len());
    let mut y = pad;
    for (index, item) in items.iter().enumerate() {
        if index > 0 && item.separated(items[index - 1]) {
            y += gap;
        }
        rows.push(Rect::new(pad, y, width - 2. * pad, row));
        y += row;
    }
    let height = y + pad;

    let open = |at: f32, size: f32, low: f32, high: f32| {
        let start = if at + size > high { at - size } else { at };
        start.min(high - size).max(low).round()
    };
    let x = open(at.0, width, bounds.x, bounds.right());
    let y = open(at.1, height, bounds.y, bounds.bottom());
    let rows = rows.into_iter().map(|r| Rect::new(r.x + x, r.y + y, r.w, r.h)).collect();
    (Rect::new(x, y, width, height), rows)
}

/// The row of `rows` at `point`.
fn row_at(rows: &[Rect], point: PhysicalPosition<f64>) -> Option<usize> {
    rows.iter().position(|row| row.contains(point))
}

/// The open context menu.
pub struct Menu {
    items: Vec<Item>,
    /// What it was opened on, which it acts on.
    owner: Owner,
    /// Where the pointer opened it.
    at: (f32, f32),
    /// The row under the pointer or chosen by the keyboard.
    selected: Option<usize>,
    /// How far it has popped in, from 0 to 1.
    pop: Tween<f32>,
}

impl Menu {
    fn new(items: Vec<Item>, owner: Owner, at: (f32, f32)) -> Self {
        let mut pop = Tween::new(0.);
        pop.to(1., Instant::now(), 140, Curve::EASE_OUT);
        Self { items, owner, at, selected: None, pop }
    }

    /// What the menu acts on: the live pane, or the tab at an index; none once that is gone, such
    /// as after the pane exited.
    fn source(&self, tabs: &Tabs) -> Option<Source> {
        match self.owner {
            Owner::Pane(id) => {
                let live = tabs.tabs.get(tabs.active).is_some_and(|tab| tab.pane().id == id);
                live.then(|| Source::Pane(tabs.active, tabs.live().1))
            },
            Owner::Tab(id) => {
                tabs.tabs.iter().position(|tab| tab.position(id).is_some()).map(Source::Tab)
            },
        }
    }

    fn stale(&self, tabs: &Tabs) -> bool {
        self.source(tabs).is_none()
    }

    fn label(&self, item: Item) -> &'static str {
        item.label(matches!(self.owner, Owner::Tab(_)))
    }

    /// Whether the menu is still animating at `now`.
    pub fn running(&self, now: Instant) -> bool {
        self.pop.running(now)
    }

    /// The menu's rect and its rows, as laid out by `layout`.
    fn geometry(&self, layout: &Layout) -> (Rect, Vec<Rect>) {
        let font = &layout.font;
        let row = layout.px(32.).max(layout.line + layout.px(12.));
        let text =
            self.items.iter().map(|&item| font.width(self.label(item), false)).fold(0., f32::max);
        let width = (text + row + layout.px(36.)).max(layout.px(200.));
        let metrics = Metrics { width, row, gap: layout.px(9.), pad: layout.px(6.) };
        let margin = layout.px(MARGIN);
        let bounds = Rect::new(0., 0., layout.width, layout.height).inset(margin);
        geometry(&self.items, self.at, metrics, bounds)
    }
}

/// Handle a mouse button press, returning whether the menu used it.
///
/// A right click in a pane opens the menu, unless the terminal takes the mouse and Shift is not
/// held, or Control is held to expand the selection. A click on an unfocused pane focuses it
/// first.
pub fn press(
    ctx: &mut WindowContext,
    button: MouseButton,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) -> bool {
    close_stale(ctx);
    let tabs = &mut ctx.display.tabs;
    let pointer = tabs.pointer;
    let layout = Layout::new(tabs);
    if let Some(menu) = &tabs.context {
        let row = row_at(&menu.geometry(&layout).1, pointer);
        tabs.press = Press::Swallowed;
        match row {
            Some(index) if button == MouseButton::Left => choose(ctx, index, config, proxy),
            Some(_) => (),
            None => close(ctx),
        }
        // A right click elsewhere opens the menu again there.
        if row.is_some() || button != MouseButton::Right {
            return true;
        }
    }

    let tabs = &mut ctx.display.tabs;
    let modifiers = tabs.modifiers;
    let pane = tabs.pane_at(pointer).filter(|_| layout.hit(pointer) == Hit::Terminal);
    let (Some(pane), MouseButton::Right, None) = (pane, button, &tabs.menu) else { return false };
    // Not while another button is down, such as for dragging a tab.
    if modifiers.control_key() || tabs.press != Press::None {
        return false;
    }
    let (active, focus) = tabs.live();
    if pane != focus {
        focus_pane(ctx, active, pane, config);
    }
    // The terminal takes the mouse like it does for the bindings, which vi mode keeps.
    let mode = *ctx.terminal.lock().mode();
    let mouse = mode.intersects(TermMode::MOUSE_MODE) && !mode.contains(TermMode::VI);
    let tabs = &mut ctx.display.tabs;
    if mouse && !modifiers.shift_key() {
        if pane == focus {
            return false;
        }
        // The press only focused the pane.
        tabs.press = Press::Swallowed;
        return true;
    }

    let scale = ctx.display.window.scale_factor as f32;
    let sized = ctx.display.font_size != ctx.config.font.size().scale(scale);
    let tabs = &ctx.display.tabs;
    let Some(tab) = tabs.tabs.get(tabs.active) else { return false };
    let tear = can_tear(tabs, Source::Pane(tabs.active, tab.focus));
    let items = pane_items(tab.panes.len(), tab.zoom, sized, tear);
    let owner = Owner::Pane(tab.pane().id);
    open(ctx, items, owner);
    true
}

/// Open the menu of the tab at `index` at the pointer.
pub fn open_tab(ctx: &mut WindowContext, index: usize) {
    let tabs = &ctx.display.tabs;
    let Some(tab) = tabs.tabs.get(index) else { return };
    let tear = can_tear(tabs, Source::Tab(index));
    let (items, owner) =
        (tab_items(tab.panes.len(), tab.name.is_some(), tear), Owner::Tab(tab.pane().id));
    open(ctx, items, owner);
}

/// Open a menu of `items` for `owner` at the pointer.
fn open(ctx: &mut WindowContext, items: Vec<Item>, owner: Owner) {
    let tabs = &mut ctx.display.tabs;
    let at = (tabs.pointer.x as f32, tabs.pointer.y as f32);
    tabs.context = Some(Menu::new(items, owner, at));
    tabs.press = Press::Swallowed;
    ctx.display.window.set_mouse_cursor(CursorIcon::Default);
    repaint(ctx);
}

/// Handle a key press, returning whether the menu used it; it takes every key while open.
pub fn key(
    ctx: &mut WindowContext,
    key: &KeyEvent,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) -> bool {
    close_stale(ctx);
    let Some(menu) = &mut ctx.display.tabs.context else { return false };
    let rows = menu.items.len();
    match key.logical_key {
        Key::Named(NamedKey::ArrowDown) => {
            menu.selected = Some(menu.selected.map_or(0, |index| (index + 1) % rows));
        },
        Key::Named(NamedKey::ArrowUp) => {
            menu.selected = Some(menu.selected.map_or(rows - 1, |index| (index + rows - 1) % rows));
        },
        Key::Named(NamedKey::Enter) => {
            match menu.selected {
                Some(index) => choose(ctx, index, config, proxy),
                None => close(ctx),
            }
            return true;
        },
        Key::Named(NamedKey::Escape) => {
            close(ctx);
            return true;
        },
        _ => return true,
    }
    repaint(ctx);
    true
}

/// Follow the pointer, returning whether the menu used the movement.
pub fn moved(ctx: &mut WindowContext, position: PhysicalPosition<f64>) -> bool {
    close_stale(ctx);
    let tabs = &mut ctx.display.tabs;
    if tabs.context.is_none() {
        return false;
    }
    let layout = Layout::new(tabs);
    let Some(menu) = &mut tabs.context else { return false };
    let row = row_at(&menu.geometry(&layout).1, position);
    ctx.display.window.set_mouse_cursor(CursorIcon::Default);
    // The keyboard's choice stays while the pointer is outside the menu.
    if row.is_some() && row != menu.selected {
        menu.selected = row;
        repaint(ctx);
    }
    true
}

/// Do what the row `index` of the open menu offers, and close it.
fn choose(
    ctx: &mut WindowContext,
    index: usize,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) {
    let tabs = &ctx.display.tabs;
    let Some(menu) = tabs.context.as_ref() else { return };
    let (item, source) = (menu.items.get(index).copied(), menu.source(tabs));
    close(ctx);
    let (Some(item), Some(source)) = (item, source) else { return };
    let (tab, pane) = match source {
        Source::Pane(tab, pane) => (tab, pane),
        Source::Tab(tab) => (tab, ctx.display.tabs.tabs[tab].focus),
    };
    match item {
        Item::SplitVertically => split(ctx, Axis::Row, config, proxy),
        Item::SplitHorizontally => split(ctx, Axis::Column, config, proxy),
        Item::Zoom | Item::Unzoom => toggle_zoom(ctx),
        Item::ResetTextSize => zoom_live(ctx, FontChange::Reset),
        Item::MoveToNewTab => move_pane(ctx, (tab, pane), Target::Detach(tab + 1), config),
        Item::NewWindow => {
            tear_off(ctx, source, None, config);
        },
        Item::Rename { .. } => rename::start(ctx, tab),
        Item::ResetName => rename(ctx, tab, None),
        Item::Ungroup => {
            let tabs = &mut ctx.display.tabs;
            if ungroup(&mut tabs.tabs, &mut tabs.active, tab) {
                ctx.display.damage_tracker.frame().mark_fully_damaged();
                relayout(ctx);
            }
        },
        Item::Close if source == Source::Tab(tab) => close_tab(ctx, tab, config),
        Item::Close => close_pane(ctx, tab, pane, config),
    }
}

/// Make each pane of the tab at `index` a tab of its own, in reading order: the first pane keeps
/// the tab, and the others follow it. The live pane stays live, in its new tab. Returns whether
/// the tab was a group.
fn ungroup<S>(tabs: &mut Vec<Tab<S>>, active: &mut usize, index: usize) -> bool {
    let Some(tab) = tabs.get_mut(index).filter(|tab| tab.panes.len() > 1) else { return false };
    let focus = tab.pane().id;
    let others = tab.panes.split_off(1);
    tab.layout = Split::Pane(tab.panes[0].id);
    (tab.focus, tab.zoom) = (0, false);
    let count = others.len();
    let at = others.iter().position(|pane| pane.id == focus).map_or(0, |at| at + 1);
    tabs.splice(index + 1..index + 1, others.into_iter().map(Tab::new));
    match (*active).cmp(&index) {
        Ordering::Equal => *active += at,
        Ordering::Greater => *active += count,
        Ordering::Less => (),
    }
    true
}

fn close(ctx: &mut WindowContext) {
    ctx.display.tabs.context = None;
    repaint(ctx);
}

/// Close the menu once what it was opened on is gone, or the pane no longer live.
fn close_stale(ctx: &mut WindowContext) {
    let tabs = &ctx.display.tabs;
    if tabs.context.as_ref().is_some_and(|menu| menu.stale(tabs)) {
        close(ctx);
    }
}

/// Close the menu as the window loses the focus, like a native one.
pub fn unfocused(ctx: &mut WindowContext) {
    if ctx.display.tabs.context.is_some() {
        close(ctx);
    }
}

/// Redraw the whole window, since the menu covers the terminal, which only repaints its changes.
fn repaint(ctx: &mut WindowContext) {
    ctx.display.damage_tracker.frame().mark_fully_damaged();
    redraw(ctx);
}

/// Draw the open menu, above everything but the settings.
///
/// It pops in from the pointer, growing from 92% while it fades in.
pub fn draw(tabs: &Tabs, layout: &Layout, palette: &Palette, now: Instant) -> Option<Layer> {
    let menu = tabs.context.as_ref().filter(|menu| !menu.stale(tabs))?;
    let (rect, rows) = menu.geometry(layout);
    let shown = menu.pop.get(now).clamp(0., 1.);
    let scale = 0.92 + 0.08 * shown;
    let origin = (menu.at.0.clamp(rect.x, rect.right()), menu.at.1.clamp(rect.y, rect.bottom()));
    let pop = |r: Rect| {
        let (x, y) = (origin.0 + (r.x - origin.0) * scale, origin.1 + (r.y - origin.1) * scale);
        Rect::new(x, y, r.w * scale, r.h * scale)
    };
    let fade = |color| mix(palette.popup, color, shown);
    let px = |value: f32| layout.px(value);
    let line = px(1.).max(1.);
    let (font, rect) = (&*layout.font, pop(rect));
    let mut layer = Layer::default();

    // Shadow, outline and background, like the new-tab menu's.
    let radius = layout.radius(TRACK_RADIUS);
    let shadow = Rect::new(rect.x, rect.y + px(2.), rect.w, rect.h);
    layer.rounded_alpha(shadow, radius + layout.radius(2.), BLACK, 0.3 * shown);
    layer.rounded_alpha(rect, radius, palette.popup_outline, shown);
    layer.rounded_alpha(rect.inset(line), radius - line, palette.popup, shown);

    for (index, (&item, &row)) in menu.items.iter().zip(&rows).enumerate() {
        let row = pop(row);
        if index > 0 && item.separated(menu.items[index - 1]) {
            let y = (row.y - px(5.)).round();
            layer
                .rect(Rect::new(row.x + px(8.), y, row.w - px(16.), line), fade(palette.separator));
        }
        if menu.selected == Some(index) {
            layer.rounded_alpha(row, layout.radius(ITEM_RADIUS), palette.hover, shown);
        }

        let glyph = Rect::new(row.x + px(8.), row.y, px(18.), row.h).square(px(18.));
        let width = (1.3 * layout.scale).max(1.);
        let color = if item == Item::Close { palette.danger } else { palette.muted };
        layer.icon(glyph.center(), px(6.), width, item.glyph(), fade(color));
        let y = row.center().1 - layout.line / 2.;
        let text = font.fit(menu.label(item), row.right() - glyph.right() - px(16.), false);
        layer.text(glyph.right() + px(10.), y, text, fade(palette.foreground), false);
    }
    Some(layer)
}

#[cfg(test)]
mod tests {
    use super::super::Pane;
    use super::super::split::Side;
    use super::*;

    const METRICS: Metrics = Metrics { width: 200., row: 30., gap: 10., pad: 5. };
    const BOUNDS: Rect = Rect { x: 0., y: 0., w: 800., h: 600. };

    #[test]
    fn pane_menu_follows_the_tab() {
        use Item::*;
        // A tab's only pane, with other tabs or alone in the window.
        let lone = [SplitVertically, SplitHorizontally, NewWindow, Close];
        assert_eq!(pane_items(1, false, false, true), lone);
        assert_eq!(pane_items(1, false, false, false), [SplitVertically, SplitHorizontally, Close]);
        // A pane of a group.
        let group = [SplitVertically, SplitHorizontally, Zoom, MoveToNewTab, NewWindow, Close];
        assert_eq!(pane_items(2, false, false, true), group);
        assert_eq!(pane_items(5, false, false, true), group);
        let zoomed = [SplitVertically, SplitHorizontally, Unzoom, MoveToNewTab, NewWindow, Close];
        assert_eq!(pane_items(3, true, false, true), zoomed);
        // A pane whose text was made larger or smaller, beside its zoom.
        let sized = [SplitVertically, SplitHorizontally, ResetTextSize, Close];
        assert_eq!(pane_items(1, false, true, false), sized);
        let items = pane_items(2, true, true, true);
        assert_eq!(items[2..5], [Unzoom, ResetTextSize, MoveToNewTab]);
        assert!(!ResetTextSize.separated(Unzoom) && MoveToNewTab.separated(ResetTextSize));
        assert!(ResetTextSize.separated(SplitHorizontally));
        assert_eq!(ResetTextSize.label(false), "Reset Text Size");
    }

    #[test]
    fn tab_menu_follows_the_tab() {
        use Item::*;
        let tab = Rename { group: false };
        assert_eq!(tab_items(1, false, true), [tab, NewWindow, Close]);
        assert_eq!(tab_items(1, true, true), [tab, ResetName, NewWindow, Close]);
        // The only tab of the window stays in it.
        assert_eq!(tab_items(1, false, false), [tab, Close]);
        let group = Rename { group: true };
        assert_eq!(tab_items(3, false, true), [group, Ungroup, NewWindow, Close]);
        assert_eq!(tab_items(3, true, false), [group, ResetName, Ungroup, Close]);
        assert_eq!(group.label(true), "Rename Group…");
        assert_eq!((Close.label(true), Close.label(false)), ("Close Tab", "Close Pane"));
    }

    #[test]
    fn ungroup_keeps_reading_order_and_the_live_pane() {
        let pane = |id: u64| Pane {
            id: WindowId::from(id),
            name: String::new(),
            title: String::new(),
            badge: String::new(),
            icon: None,
            profile: None,
            session: Some(id),
        };
        let id = |pane: &Pane<u64>| u64::from(pane.id);
        // Tabs [1] [2 | 3 over 4] [5], with the pane 3 focused.
        let mut group = Tab::new(pane(2));
        group.insert(WindowId::from(2), pane(3), Axis::Row, Side::After);
        group.insert(WindowId::from(3), pane(4), Axis::Column, Side::After);
        group.focus = 1;
        group.zoom = true;
        group.name = Some("build".into());
        let tabs = vec![Tab::new(pane(1)), group, Tab::new(pane(5))];

        let ids = |tabs: &[Tab<u64>]| -> Vec<Vec<u64>> {
            tabs.iter().map(|tab| tab.panes.iter().map(id).collect()).collect()
        };
        let mut split = tabs;
        let mut active = 1;
        assert!(ungroup(&mut split, &mut active, 1));
        assert_eq!(ids(&split), [[1], [2], [3], [4], [5]]);
        assert_eq!(active, 2, "the focused pane stays active");
        assert!(split.iter().all(|tab| tab.layout == Split::Pane(tab.panes[0].id) && !tab.zoom));
        assert_eq!(split[1].name.as_deref(), Some("build"));

        // Tabs after the group move along, and tabs before it stay.
        let tabs = || {
            let mut tabs = vec![Tab::new(pane(1)), Tab::new(pane(2)), Tab::new(pane(4))];
            tabs[1].insert(WindowId::from(2), pane(3), Axis::Row, Side::After);
            tabs
        };
        for (active, moved) in [(0, 0), (1, 1), (2, 3)] {
            let (mut split, mut active) = (tabs(), active);
            assert!(ungroup(&mut split, &mut active, 1));
            assert_eq!((ids(&split), active), (vec![vec![1], vec![2], vec![3], vec![4]], moved));
        }
        let (mut split, mut active) = (tabs(), 0);
        assert!(!ungroup(&mut split, &mut active, 0), "a lone pane is no group");
        assert!(!ungroup(&mut split, &mut active, 9));
        assert_eq!(ids(&split), [vec![1], vec![2, 3], vec![4]]);
    }

    #[test]
    fn rows_are_hit_below_their_separator() {
        let items = pane_items(1, false, false, false);
        let (menu, rows) = geometry(&items, (100., 100.), METRICS, BOUNDS);
        assert_eq!(menu, Rect::new(100., 100., 200., 3. * 30. + 10. + 10.));
        let at = |y: f64| row_at(&rows, PhysicalPosition::new(150., y));
        assert_eq!(at(104.), None);
        assert_eq!(at(106.), Some(0));
        assert_eq!(at(140.), Some(1));
        // The separator between the splits and the rest is not a row.
        assert_eq!(at(168.), None);
        assert_eq!(at(176.), Some(2));
        assert_eq!(at(206.), None);
        assert_eq!(row_at(&rows, PhysicalPosition::new(102., 140.)), None);
    }

    #[test]
    fn opens_inside_the_window() {
        let items = pane_items(1, false, false, false);
        let inside = |menu: Rect| {
            menu.x >= BOUNDS.x
                && menu.y >= BOUNDS.y
                && menu.right() <= BOUNDS.right()
                && menu.bottom() <= BOUNDS.bottom()
        };

        // Room below and right of the pointer.
        let (menu, _) = geometry(&items, (10., 20.), METRICS, BOUNDS);
        assert_eq!((menu.x, menu.y), (10., 20.));

        // Near the bottom right corner it opens above and left of the pointer.
        let (menu, _) = geometry(&items, (790., 590.), METRICS, BOUNDS);
        assert_eq!((menu.right(), menu.bottom()), (790., 590.));

        // Where neither side has room, it is pushed inside.
        let small = Rect::new(0., 0., 250., 120.);
        for at in [(0., 0.), (125., 60.), (250., 120.), (-50., 500.)] {
            let (menu, _) = geometry(&items, at, METRICS, small);
            assert!(menu.x >= 0. && menu.right() <= 250. && menu.y >= 0., "{at:?}: {menu:?}");
        }
        for at in [(0., 0.), (400., 300.), (799., 0.), (0., 599.), (799., 599.)] {
            assert!(inside(geometry(&items, at, METRICS, BOUNDS).0), "{at:?}");
        }
    }
}

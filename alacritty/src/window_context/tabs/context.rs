//! The context menu of a pane, opened by a right click in it: split the pane, or move or close it.

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
use super::dnd::Target;
use super::paint::{BLACK, Layer, Palette, Rect, mix};
use super::split::{Axis, Side};
use super::{Press, Tabs, close_pane, focus_pane, move_pane, redraw, split};

/// Something the menu offers to do with the pane it was opened on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    /// Put a new pane beside this one, with a vertical divider between them.
    SplitVertically,
    /// Put a new pane below this one, with a horizontal divider between them.
    SplitHorizontally,
    MoveToNewTab,
    Close,
}

impl Item {
    fn label(self) -> &'static str {
        match self {
            Item::SplitVertically => "Split Vertically",
            Item::SplitHorizontally => "Split Horizontally",
            Item::MoveToNewTab => "Move Pane to New Tab",
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
            Item::Close => &[[-0.7, -0.7, 0.7, 0.7], [-0.7, 0.7, 0.7, -0.7]],
        }
    }

    /// Whether a separator divides this item from `previous`.
    fn separated(self, previous: Item) -> bool {
        let splits = |item| matches!(item, Item::SplitVertically | Item::SplitHorizontally);
        splits(self) != splits(previous)
    }
}

/// What the menu offers for a pane of a tab with `panes` panes.
fn items(panes: usize) -> Vec<Item> {
    let mut items = vec![Item::SplitVertically, Item::SplitHorizontally];
    if panes > 1 {
        items.push(Item::MoveToNewTab);
    }
    items.push(Item::Close);
    items
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
    /// Id of the pane it was opened on, which it acts on.
    pane: WindowId,
    /// Where the pointer opened it.
    at: (f32, f32),
    /// The row under the pointer or chosen by the keyboard.
    selected: Option<usize>,
    /// How far it has popped in, from 0 to 1.
    pop: Tween<f32>,
}

impl Menu {
    fn new(items: Vec<Item>, pane: WindowId, at: (f32, f32)) -> Self {
        let mut pop = Tween::new(0.);
        pop.to(1., Instant::now(), 140, Curve::EASE_OUT);
        Self { items, pane, at, selected: None, pop }
    }

    /// Whether the pane it was opened on is no longer live, such as after it exited.
    fn stale(&self, tabs: &Tabs) -> bool {
        tabs.tabs.get(tabs.active).is_none_or(|tab| tab.pane().id != self.pane)
    }

    /// Whether the menu is still animating at `now`.
    pub fn running(&self, now: Instant) -> bool {
        self.pop.running(now)
    }

    /// The menu's rect and its rows, as laid out by `layout`.
    fn geometry(&self, layout: &Layout) -> (Rect, Vec<Rect>) {
        let font = &layout.font;
        let row = layout.px(32.).max(layout.line + layout.px(12.));
        let text = self.items.iter().map(|item| font.width(item.label(), false)).fold(0., f32::max);
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

    let Some(tab) = tabs.tabs.get(tabs.active) else { return false };
    let (items, at) = (items(tab.panes.len()), (pointer.x as f32, pointer.y as f32));
    tabs.context = Some(Menu::new(items, tab.pane().id, at));
    tabs.press = Press::Swallowed;
    ctx.display.window.set_mouse_cursor(CursorIcon::Default);
    repaint(ctx);
    true
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
    let Some(menu) = ctx.display.tabs.context.as_ref() else { return };
    let item = menu.items.get(index).copied();
    close(ctx);
    let (tab, pane) = ctx.display.tabs.live();
    match item {
        Some(Item::SplitVertically) => {
            split(ctx, Axis::Row, Side::After, config, proxy);
        },
        Some(Item::SplitHorizontally) => {
            split(ctx, Axis::Column, Side::After, config, proxy);
        },
        Some(Item::MoveToNewTab) => move_pane(ctx, (tab, pane), Target::Detach(tab + 1), config),
        Some(Item::Close) => close_pane(ctx, tab, pane, config),
        None => (),
    }
}

fn close(ctx: &mut WindowContext) {
    ctx.display.tabs.context = None;
    repaint(ctx);
}

/// Close the menu once the pane it was opened on is no longer live.
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
        let text = font.fit(item.label(), row.right() - glyph.right() - px(16.), false);
        layer.text(glyph.right() + px(10.), y, text, fade(palette.foreground), false);
    }
    Some(layer)
}

#[cfg(test)]
mod tests {
    use super::*;

    const METRICS: Metrics = Metrics { width: 200., row: 30., gap: 10., pad: 5. };
    const BOUNDS: Rect = Rect { x: 0., y: 0., w: 800., h: 600. };

    #[test]
    fn items_follow_pane_count() {
        use Item::*;
        assert_eq!(items(1), [SplitVertically, SplitHorizontally, Close]);
        assert_eq!(items(2), [SplitVertically, SplitHorizontally, MoveToNewTab, Close]);
        assert_eq!(items(5), items(2));
    }

    #[test]
    fn rows_are_hit_below_their_separator() {
        let items = items(2);
        let (menu, rows) = geometry(&items, (100., 100.), METRICS, BOUNDS);
        assert_eq!(menu, Rect::new(100., 100., 200., 4. * 30. + 10. + 10.));
        let at = |y: f64| row_at(&rows, PhysicalPosition::new(150., y));
        assert_eq!(at(104.), None);
        assert_eq!(at(106.), Some(0));
        assert_eq!(at(140.), Some(1));
        // The separator between the splits and the rest is not a row.
        assert_eq!(at(168.), None);
        assert_eq!(at(176.), Some(2));
        assert_eq!(at(220.), Some(3));
        assert_eq!(at(250.), None);
        assert_eq!(row_at(&rows, PhysicalPosition::new(102., 140.)), None);
    }

    #[test]
    fn opens_inside_the_window() {
        let items = items(1);
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

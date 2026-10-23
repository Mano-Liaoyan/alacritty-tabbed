//! The caption: logo, tabs, buttons and window controls in one bar, above a panel that holds the
//! terminal.
//!
//! The layout is the same on every platform; only the window controls follow the system's look.
//!
//! Corners are all round or all square. Round ones nest like Apple's: a surface inside another is
//! rounded by the outer radius less the space between them, so their curves run parallel, from
//! the window's corners to the panel, the tab track, the tabs and the buttons.

use std::collections::HashMap;
use std::iter;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use glutin::surface::Rect as DamageRect;
use winit::dpi::PhysicalPosition;
use winit::window::{ResizeDirection, WindowId};

use crate::config::UiConfig;
use crate::config::window::Corners;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::window::Window;
use crate::renderer::Renderer;

use super::anim::{Curve, Tween};
use super::dnd::{Source, Target};
use super::paint::{self, BLACK, Image, Layer, Palette, Rect, WHITE, mix};
use super::text::UiFont;
use super::{Frame, Pane, Tab, Tabs, context, icons, panes, peek, rename};

/// Height of the bar in logical pixels, on every platform.
pub const BAR: f32 = 48.;
/// Space between the window's edge and the terminal's panel.
pub const MARGIN: f32 = 8.;
/// Space between the panel's edge and the terminal.
pub const INSET: f32 = 4.;

/// Radius of the window's corners, which the other radii follow.
pub const RADIUS: f32 = 18.;
/// Radius of the terminal's panel, and of the panels in the settings.
pub const PANEL_RADIUS: f32 = RADIUS - MARGIN;
/// Space between the top of the window and the tab track.
const TRACK_TOP: f32 = 6.;
/// Space between the tab track and its tabs.
const TRACK_PAD: f32 = 3.;
/// Height of the buttons in the bar.
const CONTROL: f32 = 32.;
/// Radius of the tab track, and of floating menus.
pub const TRACK_RADIUS: f32 = RADIUS - TRACK_TOP;
/// Radius of the tabs, and of rows like them.
pub const TAB_RADIUS: f32 = TRACK_RADIUS - TRACK_PAD;
/// Radius of the buttons in the bar.
const BUTTON_RADIUS: f32 = RADIUS - (BAR - CONTROL) / 2.;
/// Radius of rows inside a menu, set in from its edge.
pub const ITEM_RADIUS: f32 = TRACK_RADIUS - 6.;

/// Radius of GNOME's and KDE's round window buttons.
const CONTROL_DOT: f32 = 12.;

/// Last entry of the new-tab menu.
const EDIT_PROFILES: &str = "Edit profiles…";

/// Most panes a grouped tab shows as a deck of cards at rest; more show `DECK_SHOWN` cards and
/// a count of the others.
const DECK_MAX: usize = 4;
const DECK_SHOWN: usize = 3;
/// Most icons in a row of a fanned deck; more wrap into rows below it, a shelf over the terminal.
const ROW_MAX: usize = 8;
/// Distance between the icons of a fanned deck, along its rows and between them.
const PITCH: f32 = 26.;
/// How opaque the cards at rest are, from the front of a deck to its back.
const DEPTH_FADE: [f32; DECK_MAX] = [1., 0.8, 0.6, 0.45];

/// Where the icons of a tab's panes are drawn, by the panes' indices.
type Icons = Vec<(usize, Rect)>;

/// macOS keeps its own red, yellow and green window buttons at the left.
const MAC: bool = cfg!(target_os = "macos");

/// What the pointer is over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hit {
    /// Chrome around the terminal.
    #[default]
    None,
    Terminal,
    /// Empty space of the bar, which moves the window.
    Drag,
    Resize(ResizeDirection),
    Tab(usize),
    /// Icon of a pane of a grouped tab: the tab's index, then the pane's.
    Icon(usize, usize),
    Close(usize),
    New,
    Menu,
    Settings,
    Previous,
    Next,
    Minimize,
    Maximize,
    CloseWindow,
    /// Row of the open menu.
    Item(usize),
}

/// Geometry of the bar, the terminal's panel and the menu, in window pixels.
pub struct Layout {
    pub scale: f32,
    pub font: Rc<UiFont>,
    /// Height of a line of text.
    pub line: f32,
    pub bar: f32,
    pub width: f32,
    pub first: usize,
    /// Whether corners are round.
    round: bool,
    border: f32,
    /// Whether the terminal is held in a panel, with a margin around it.
    pub panel: bool,
    pub height: f32,
    /// Distance between the left edges of neighbouring tabs.
    step: f32,
    brand: Option<Rect>,
    mark: Rect,
    track: Rect,
    tabs: Vec<(usize, Rect)>,
    /// Icons of the tabs' panes fanned out, by tab, with the area that picks one of them: the
    /// deck at rest, or all of it fanned out while it is.
    stacks: Vec<(usize, Rect, Icons)>,
    buttons: Vec<(Hit, Rect)>,
    terminal: Rect,
    /// The menu and the height of its rows.
    menu: Option<(Rect, f32)>,
}

impl Layout {
    pub fn new(tabs: &Tabs) -> Self {
        let scale = tabs.scale as f32;
        let px = |value: f32| (value * scale).round();
        let font = tabs.font.clone().expect("the bar is only shown with its font");
        let line = font.line;
        let (width, height) = (tabs.window.width as f32, tabs.window.height as f32);
        let (bar, count, fullscreen) = (px(BAR), tabs.len(), tabs.frame == Frame::Fullscreen);
        let mac = MAC;
        let border = if tabs.frame == Frame::Normal && !mac { px(4.) } else { 0. };
        let (margin, control, chevron, pad, nav) =
            (px(MARGIN), px(CONTROL), px(22.), px(TRACK_PAD), px(28.));
        let caption = px(if cfg!(windows) { 40. } else { 34. });
        let button =
            |x: f32, width: f32| Rect::new(x, ((bar - control) / 2.).round(), width, control);

        let lights = if mac { px(72.) } else { 0. };
        // GNOME's and KDE's round window buttons end as far from the window's edge as the logo
        // begins, so the bar's two ends mirror each other.
        let trailing = if mac || cfg!(windows) {
            margin
        } else {
            px(MARGIN + INSET) - (caption / 2. - px(CONTROL_DOT))
        };
        let controls = width - trailing - if mac { 0. } else { 3. * caption };
        let settings = controls - px(4.) - control;

        // The logo is set in from the window's edge as far as from its top, which keeps it clear
        // of the rounded corner and lines it up with the terminal's text below. macOS puts it
        // beside its window buttons instead.
        let logo = px(24.);
        let left = if mac { lights + px(7.) } else { px(MARGIN + INSET) };
        let mark = Rect::new(left, 0., logo, bar).square(logo);

        // Fullscreen gives the label's space to the tabs; the logo stays.
        let label = !fullscreen && width >= px(780.);
        let label_width = px(114.);
        let brand = mark.right() + if label { px(15.) + label_width } else { px(21.) };
        let min_tab = px(124.);
        let available = settings - px(48.) - brand - 2. * pad - px(6.) - control - chevron;
        let overflow = count as f32 > (available / min_tab).floor().max(1.);
        let available = (available - if overflow { 2. * nav + px(6.) } else { 0. }).max(1.);
        let capacity = ((available / min_tab) as usize).max(1);
        let shown = count.min(capacity);
        let step = (available / shown as f32).min(px(236.)).floor();
        let first = tabs
            .first
            .min(tabs.active)
            .max((tabs.active + 1).saturating_sub(capacity))
            .min(count - shown);

        let (top, bottom) = (px(TRACK_TOP), bar - px(TRACK_TOP));
        let tab_rects: Vec<_> = (0..shown)
            .map(|slot| {
                let x = brand + pad + slot as f32 * step;
                (first + slot, Rect::new(x, top + pad, step - px(4.), bottom - top - 2. * pad))
            })
            .collect();
        let right = tab_rects.last().map_or(brand, |(_, rect)| rect.right() + pad);
        let track = Rect::new(brand, top, right - brand, bottom - top);

        let mut buttons = Vec::new();
        let mut x = track.right() + px(6.);
        if overflow {
            buttons.push((Hit::Previous, button(x, nav)));
            buttons.push((Hit::Next, button(x + nav, nav)));
            x += 2. * nav;
        }
        buttons.push((Hit::New, button(x, control)));
        buttons.push((Hit::Menu, button(x + control, chevron)));
        buttons.push((Hit::Settings, button(settings, control)));
        if !mac {
            for (index, hit) in
                [Hit::Minimize, Hit::Maximize, Hit::CloseWindow].into_iter().enumerate()
            {
                buttons.push((hit, button(controls + index as f32 * caption, caption)));
            }
        }

        let brand = label.then(|| Rect::new(mark.right() + px(11.), 0., label_width, bar));

        let panel = tabs.panel && !fullscreen;
        let card = if panel {
            Rect::new(margin, bar, width - 2. * margin, height - bar - margin)
        } else {
            Rect::new(0., bar, width, height - bar)
        };
        let terminal = if panel { card.inset(px(INSET)) } else { card };

        // The menu drops from the new-tab button over the terminal.
        let new = buttons.iter().find(|(hit, _)| *hit == Hit::New).map_or(track, |(_, rect)| *rect);
        let menu = tabs.menu.as_ref().map(|menu| {
            let row = px(34.).max(line + px(12.));
            let names = menu.profiles.iter().map(|profile| font.width(&profile.name, false));
            let text = names.fold(font.width(EDIT_PROFILES, false), f32::max);
            let w = (text + row + px(36.)).max(px(220.)).min(width - 2. * margin);
            let x = new.x.min(width - margin - w).max(margin);
            let rows = menu.profiles.len() + 1;
            (Rect::new(x, bar - px(4.), w, rows as f32 * row + px(12.)), row)
        });

        let mut layout = Self {
            scale,
            font,
            line,
            bar,
            width,
            first,
            round: tabs.corners.unwrap_or_default() == Corners::Round,
            border,
            panel,
            height,
            step,
            brand,
            mark,
            track,
            tabs: tab_rects,
            stacks: Vec::new(),
            buttons,
            terminal,
            menu,
        };

        // A deck is entered at rest and fans out under the pointer; its icons are told apart as
        // they are fanned, so the icon picked stays under a pointer that does not move.
        layout.stacks = layout
            .tabs
            .iter()
            .filter_map(|&(index, rect)| {
                let tab = tabs.tabs.get(index)?;
                let (front, count) = (layout.badge(rect), tab.panes.len());
                // A lone pane's icon is its tab's badge, which does not fan out.
                if count == 1 {
                    return Some((index, front, vec![(0, front)]));
                }
                let (row, chip) = (layout.row(front), layout.chip_size(count));
                let deck = |fan| deck(front, count, tab.focus, scale, row, chip, |_| (fan, 0.));
                let fanned = deck(1.).icons;
                let area = if matches!(tabs.hover, Hit::Icon(i, _) if i == index) {
                    plate(rect, &fanned, count.div_ceil(row), 1., bar, scale).0
                } else {
                    Rect::new(front.x, rect.y, deck(0.).end() - front.x, rect.h)
                };
                Some((index, area, fanned))
            })
            .collect();
        layout
    }

    pub fn px(&self, value: f32) -> f32 {
        (value * self.scale).round()
    }

    /// A corner radius of `value` logical pixels, or none while corners are square.
    pub fn radius(&self, value: f32) -> f32 {
        self.curve(value * self.scale)
    }

    /// A corner radius of `pixels`, or none while corners are square.
    pub fn curve(&self, pixels: f32) -> f32 {
        if self.round { pixels } else { 0. }
    }

    /// A circle, or a square around it while corners are square.
    pub fn dot(&self, layer: &mut Layer, (x, y): (f32, f32), radius: f32, color: Rgb) {
        let rect = Rect::new(x - radius, y - radius, 2. * radius, 2. * radius);
        layer.rounded(rect, if self.round { radius } else { 0. }, color);
    }

    pub fn hit(&self, point: PhysicalPosition<f64>) -> Hit {
        let (x, y) = (point.x as f32, point.y as f32);
        if let Some((menu, row)) = self.menu {
            if !menu.contains(point) {
                return Hit::None;
            }
            let rows = ((menu.h - self.px(12.)) / row).round() as usize;
            let index = ((y - menu.y - self.px(6.)) / row).max(0.) as usize;
            return Hit::Item(index.min(rows - 1));
        }

        // Below the bar the whole margin around the panel resizes, and each corner reaches along
        // both edges, so diagonal resizing is as easy to grab as the edges.
        let border = self.border;
        let margin = if self.panel { border.max(self.px(MARGIN)) } else { border };
        let edge = if border > 0. && y >= self.bar { margin } else { border };
        let (left, right) = (x < edge, x >= self.width - edge);
        let (top, bottom) = (y < border, y >= self.height - margin && border > 0.);
        let corner = if border > 0. { self.px(24.) } else { 0. };
        let (near_left, near_right) = (x < corner, x >= self.width - corner);
        let (near_top, near_bottom) = (y < corner, y >= self.height - corner);
        let direction = if (left && near_top) || (top && near_left) {
            Some(ResizeDirection::NorthWest)
        } else if (right && near_top) || (top && near_right) {
            Some(ResizeDirection::NorthEast)
        } else if (left && near_bottom) || (bottom && near_left) {
            Some(ResizeDirection::SouthWest)
        } else if (right && near_bottom) || (bottom && near_right) {
            Some(ResizeDirection::SouthEast)
        } else if left {
            Some(ResizeDirection::West)
        } else if right {
            Some(ResizeDirection::East)
        } else if top {
            Some(ResizeDirection::North)
        } else if bottom {
            Some(ResizeDirection::South)
        } else {
            None
        };
        if let Some(direction) = direction {
            return Hit::Resize(direction);
        }

        // A fanned deck covers what is beside it, the end of its tab and the buttons after it, and
        // its shelf what is below the bar.
        for (index, area, icons) in &self.stacks {
            // The plate's padding picks the icon nearest it.
            let (left, right) = extent(icons);
            let x = x.clamp(left, right - 0.5);
            if let Some(pane) = icon_at(icons, (x, y)).filter(|_| area.contains(point)) {
                return Hit::Icon(*index, pane);
            }
        }
        if let Some(&(hit, _)) = self.buttons.iter().find(|(_, rect)| rect.contains(point)) {
            return hit;
        }
        if y >= self.bar {
            return if self.terminal.contains(point) { Hit::Terminal } else { Hit::None };
        }
        match self.tabs.iter().find(|(_, rect)| rect.contains(point)) {
            Some(&(index, rect)) if self.close(rect).contains(point) => Hit::Close(index),
            Some(&(index, _)) => Hit::Tab(index),
            None => Hit::Drag,
        }
    }

    pub fn tab(&self, index: usize) -> Option<Rect> {
        self.tabs.iter().find(|(i, _)| *i == index).map(|(_, rect)| *rect)
    }

    /// Where the icon of the pane `pane` of the tab at `index` is once its deck is fanned out.
    pub fn icon(&self, index: usize, pane: usize) -> Option<Rect> {
        let (.., icons) = self.stacks.iter().find(|(i, ..)| *i == index)?;
        icons.iter().find(|(p, _)| *p == pane).map(|(_, rect)| *rect)
    }

    /// What the card of the pane `pane` of the tab at `index` hangs from: its icon, fanned out,
    /// reaching down to the bottom of its tab, or of the shelf of icons it lies on.
    pub fn anchor(&self, index: usize, pane: usize) -> Option<Rect> {
        let icon = self.icon(index, pane)?;
        let (_, area, _) = self.stacks.iter().find(|(i, ..)| *i == index)?;
        let bottom = area.bottom().max(self.tab(index).map_or(0., |tab| tab.bottom()));
        Some(Rect::new(icon.x, icon.y, icon.w, bottom - icon.y))
    }

    fn close(&self, tab: Rect) -> Rect {
        let size = self.px(26.);
        Rect::new(tab.right() - size, tab.y, size, tab.h)
    }

    fn badge(&self, tab: Rect) -> Rect {
        let size = self.px(18.);
        Rect::new(tab.x + self.px(9.), (tab.y + (tab.h - size) / 2.).round(), size, size)
    }

    /// Where the title of a tab with `panes` panes begins, after its badge or deck of icons.
    fn title(&self, tab: Rect, panes: usize) -> f32 {
        let front = self.badge(tab);
        if panes > DECK_MAX {
            return chip(front, self.chip_size(panes), self.scale).right() + self.px(6.);
        }
        let behind = (panes.max(1) - 1) as f32;
        (front.right() + behind * 6. * self.scale).round() + self.px(8.)
    }

    /// How many icons a fanned row that begins at `front` holds within the window.
    fn row(&self, front: Rect) -> usize {
        let room = self.width - self.px(MARGIN) - front.x;
        ((room / (PITCH * self.scale)) as usize).clamp(1, ROW_MAX)
    }

    /// Size of the capsule counting the panes of a deck of `panes` left out of its cards.
    fn chip_size(&self, panes: usize) -> (f32, f32) {
        let height = self.px(16.).max(self.line + self.px(2.)).round();
        let text = self.font.width(&count_label(panes), false);
        ((text + self.px(10.)).max(height).round(), height)
    }

    /// Where the gap before the tab at `index` is, or after the last tab shown.
    fn gap(&self, index: usize) -> Option<f32> {
        let half = self.px(2.);
        match self.tab(index) {
            Some(rect) => Some(rect.x - half),
            None => self
                .tabs
                .last()
                .filter(|(last, _)| last + 1 == index)
                .map(|(_, r)| r.right() + half),
        }
    }
}

/// A tab's icons, from the back of the deck to its front, and the count of the panes left out.
struct Deck {
    icons: Icons,
    /// The capsule counting the panes behind the cards shown at rest, and how many there are.
    chip: Option<(Rect, usize)>,
}

impl Deck {
    /// Where the deck ends, at its last icon or its count.
    fn end(&self) -> f32 {
        let chip = self.chip.map_or(f32::MIN, |(chip, _)| chip.right());
        extent(&self.icons).1.max(chip)
    }
}

/// How many of a tab's `count` panes its deck shows as cards at rest.
fn shown(count: usize) -> usize {
    if count > DECK_MAX { DECK_SHOWN } else { count }
}

/// The count of the panes of a deck of `count` that are not shown as cards.
fn count_label(count: usize) -> String {
    format!("+{}", count.saturating_sub(DECK_SHOWN))
}

/// Where to draw the icons of a tab's `count` panes, from the back of the deck to its front.
///
/// The focused pane's icon fills the badge `front`. Behind it, to the right, the others peek out
/// like cards, each smaller than the one before: all of them up to `DECK_MAX`, or else
/// `DECK_SHOWN` and a capsule of `chip_size` counting the rest, so the deck never grows long.
///
/// `motion(pane)` says how far the deck is fanned out and how far that pane's icon is lifted
/// from it, each from 0 to 1: fanned out, the icons grow and spread apart into rows of `row`, and a
/// lifted one grows more. The panes left out of the deck come out from behind its last card.
fn deck(
    front: Rect,
    count: usize,
    focus: usize,
    scale: f32,
    row: usize,
    chip_size: (f32, f32),
    motion: impl Fn(usize) -> (f32, f32),
) -> Deck {
    let shown = shown(count);
    let behind = (0..count).filter(|&pane| pane != focus);
    let (left, middle) = (front.x, front.center().1);
    let mut icons: Vec<_> = iter::once(focus)
        .chain(behind)
        .enumerate()
        .filter_map(|(order, pane)| {
            let (fan, lift) = motion(pane);
            if order >= shown && fan <= 0. {
                return None;
            }
            let depth = order.min(shown - 1);
            let (column, line) = ((order % row) as f32, (order / row) as f32);
            let base = front.w + 4. * fan * scale;
            let shrunk = base * (0.9 + 0.1 * fan).powi(depth as i32);
            let size = (shrunk + 4. * lift * scale).round();
            let spread = 6. * (1. - fan) * depth as f32 + PITCH * fan * column;
            let right = left + base + spread * scale;
            let (x, y) = (right - shrunk / 2., middle + (PITCH * fan * line - 2. * lift) * scale);
            Some((pane, Rect::new((x - size / 2.).round(), (y - size / 2.).round(), size, size)))
        })
        .collect();
    icons.reverse();
    let chip = (count > shown).then(|| (chip(front, chip_size, scale), count - shown));
    Deck { icons, chip }
}

/// Where the capsule counting the panes left out of a deck with the badge `front` is: tucked
/// behind its last card like one more, centered on the badge, showing `size` beside the card.
fn chip(front: Rect, (width, height): (f32, f32), scale: f32) -> Rect {
    let (edge, tuck) = (front.right() + 6. * (DECK_SHOWN - 1) as f32 * scale, 4. * scale);
    let y = front.center().1 - height / 2.;
    Rect::new((edge - tuck).round(), y.round(), width + tuck.round(), height)
}

/// How opaque the icon `order`th from the front of a deck of `count` is, fanned out by `fan`: the
/// cards fade with their depth, and the panes left out of them show only as the deck fans out.
fn opacity(order: usize, count: usize, fan: f32) -> f32 {
    let rest = if order < shown(count) { DEPTH_FADE[order] } else { 0. };
    rest + (1. - rest) * fan.clamp(0., 1.)
}

/// The plate under the fanned `icons` of the tab `tab`, grown down by `fan` to hold their `rows`,
/// and how far it floats like a popover: once it reaches past its tab or the `bar`, it takes more
/// padding, an outline and a shadow, so it reads as lying over what is beside it.
fn plate(
    tab: Rect,
    icons: &[(usize, Rect)],
    rows: usize,
    fan: f32,
    bar: f32,
    scale: f32,
) -> (Rect, f32) {
    let px = |value: f32| (value * scale).round();
    let (left, right) = extent(icons);
    let below = (rows.max(1) - 1) as f32 * PITCH * scale * fan;
    let flat =
        Rect::new(left - px(3.), tab.y + px(2.), right - left + px(6.), tab.h - px(4.) + below);
    let past = (flat.right() - tab.right()).max(flat.bottom() - bar);
    let float = fan * (past / px(4.)).clamp(0., 1.);
    (flat.inset(-(px(2.) * float).round()), float)
}

/// The left and right edges of `icons`.
fn extent(icons: &[(usize, Rect)]) -> (f32, f32) {
    let left = icons.iter().map(|(_, rect)| rect.x).fold(f32::MAX, f32::min);
    let right = icons.iter().map(|(_, rect)| rect.right()).fold(f32::MIN, f32::max);
    (left, right)
}

/// The pane whose icon in `icons`, drawn back to front, is at `(x, y)`: in the row nearest `y`,
/// the frontmost spanning `x`, or else the nearest while `x` is within the icons.
fn icon_at(icons: &[(usize, Rect)], (x, y): (f32, f32)) -> Option<usize> {
    let (left, right) = extent(icons);
    if x < left || x >= right {
        return None;
    }
    let distance = |from: f32, to: f32, at: f32| (from - at).max(at - to).max(0.);
    let across = |rect: &Rect| distance(rect.y, rect.bottom(), y);
    let along = |rect: &Rect| distance(rect.x, rect.right(), x);
    let front_first = icons.iter().rev();
    let (_, nearest) = front_first.clone().min_by(|a, b| across(&a.1).total_cmp(&across(&b.1)))?;
    let row = front_first
        .filter(|(_, rect)| (rect.center().1 - nearest.center().1).abs() < nearest.h / 2.);
    row.min_by(|a, b| along(&a.1).total_cmp(&along(&b.1))).map(|(pane, _)| *pane)
}

/// How the bar's tabs and icons move, by the panes they show.
///
/// A tab follows its focused pane, so a tab joining another slides from where it was.
#[derive(Default)]
pub struct Effects {
    panes: HashMap<WindowId, Motion>,
}

/// Where a pane's tab is, by its index, and how far its icon is fanned out and lifted.
struct Motion {
    slot: Tween<f32>,
    fan: Tween<f32>,
    lift: Tween<f32>,
}

impl Effects {
    /// Follow the tabs to their places and the icon under the pointer, at `now`; a tab `held` by
    /// a drag stays at its slot there, to slide on from it once dropped.
    fn update(&mut self, tabs: &[Tab], hover: Hit, held: Option<(usize, f32)>, now: Instant) {
        self.panes.retain(|id, _| tabs.iter().any(|tab| tab.position(*id).is_some()));
        for (index, tab) in tabs.iter().enumerate() {
            // A lone pane's icon is its tab's badge, which does not fan out.
            let fanned = tab.panes.len() > 1 && matches!(hover, Hit::Icon(i, _) if i == index);
            for (position, pane) in tab.panes.iter().enumerate() {
                let held = held.filter(|&(tab, _)| tab == index).map(|(_, slot)| slot);
                let slot = held.unwrap_or(index as f32);
                let motion = self.panes.entry(pane.id).or_insert_with(|| Motion {
                    slot: Tween::new(slot),
                    fan: Tween::new(0.),
                    lift: Tween::new(0.),
                });
                let lifted = fanned && hover == Hit::Icon(index, position);
                match held {
                    Some(_) => motion.slot = Tween::new(slot),
                    None => motion.slot.to(slot, now, 200, Curve::SNAP),
                }
                motion.fan.to(f32::from(fanned), now, 160, Curve::EASE_OUT);
                motion.lift.to(f32::from(lifted), now, 160, Curve::EASE_OUT);
            }
        }
    }

    /// Where the tab showing `pane` is at `now`, by index, and how far the pane's icon is fanned
    /// out and lifted.
    fn get(&self, pane: WindowId, now: Instant) -> (f32, f32, f32) {
        let motion = self.panes.get(&pane);
        motion.map_or((0., 0., 0.), |m| (m.slot.get(now), m.fan.get(now), m.lift.get(now)))
    }

    /// Whether something in the bar is still moving at `now`.
    pub fn running(&self, now: Instant) -> bool {
        let mut tweens = self.panes.values().flat_map(|m| [&m.slot, &m.fan, &m.lift]);
        tweens.any(|tween| tween.running(now))
    }
}

/// Draw an icon, or else a badge with text, in `rect`.
pub fn badge(
    layer: &mut Layer,
    layout: &Layout,
    rect: Rect,
    image: Option<&Arc<Image>>,
    text: &str,
    font: &UiFont,
    colors: (Rgb, Rgb, Rgb),
) {
    match image {
        Some(image) => layer.image(rect, image, colors.2),
        None => {
            let (surface, accent, _) = colors;
            layer.rounded(rect, layout.curve(rect.w / 4.), mix(surface, accent, 0.3));
            layer.label(rect, text, font, accent, true);
        },
    }
}

impl Tabs {
    /// Add the bar, which lies above the terminal, to the damage of a frame.
    ///
    /// Panes beside the live terminal, and anything moving, damage the whole window.
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
    pub fn damage(&self, mut damage: Vec<DamageRect>, _size: &SizeInfo) -> Vec<DamageRect> {
        let (width, height) = (self.window.width as i32, self.window.height as i32);
        let panes = self.tabs.get(self.active).map_or(1, |tab| tab.panes.len());
        // A fanned deck may reach over the terminal; a lone tab's badge does not fan out.
        let fanned = match self.hover {
            Hit::Icon(index, _) => self.tabs.get(index).is_some_and(|tab| tab.panes.len() > 1),
            _ => false,
        };
        let now = Instant::now();
        if panes > 1 || fanned || self.animating(now) || self.peeking.visible(now) {
            return vec![DamageRect::new(0, 0, width, height)];
        }
        if self.height > 0 {
            let bar = (BAR * self.scale as f32).round() as i32;
            damage.push(DamageRect::new(0, height - bar, width, bar));
        }
        damage
    }

    /// Size of the active terminal moved to its pane, for placing the input method's popup.
    pub fn ime(&self, size: &SizeInfo) -> SizeInfo {
        let Rect { x: left, y: top, .. } = self.focused_rect();
        let (cell_width, cell_height) = (size.cell_width(), size.cell_height());
        let (x, y) = (size.padding_x() + left, size.padding_y() + top);
        let (width, height) = (size.width() + left, size.height() + top);
        SizeInfo::new(width, height, cell_width, cell_height, x, y, false)
    }

    /// Draw the caption and the panel around the terminal, and the menu or settings over them.
    pub fn draw(
        &mut self,
        renderer: &mut Renderer,
        window: &Window,
        size: &SizeInfo,
        config: &UiConfig,
    ) {
        if self.height == 0 {
            return;
        }

        let layout = Layout::new(self);
        self.first = layout.first;
        let now = Instant::now();
        let mut palette = Palette::new(&config.colors);
        palette.highlight = config.window.pane_highlight.unwrap_or(palette.primary);
        let (font, line_height) = (&*layout.font, layout.line);
        let (bar, width) = (layout.bar, layout.width);
        let px = |value: f32| layout.px(value);
        let line = px(1.).max(1.);
        let icon = (1.3 * layout.scale).max(1.);
        let mut base = Layer::default();
        let mut top = Layer::default();
        // What a drag holds, which fades while it would leave the window.
        let mut lifted = Layer::default();

        // Chrome around the panels that hold the terminals.
        base.rect(Rect::new(0., 0., width, bar), palette.chrome);
        panes::draw(self, &layout, &palette, &mut base, now);

        // Logo, with the application and computer name beside it.
        base.image(layout.mark, &icons::logo(), palette.foreground);
        if let Some(brand) = layout.brand {
            let middle = (bar / 2.).round();
            let (label, host) = ("Alacritty", font.fit(&self.host, brand.w, false));
            if 2. * line_height <= bar - px(8.) {
                let y = middle - line_height + px(1.);
                base.text(brand.x, y, label.into(), palette.foreground, true);
                base.text(brand.x, middle + px(1.), host, palette.muted, false);
            } else {
                let y = middle - line_height / 2.;
                base.text(brand.x, y, label.into(), palette.foreground, true);
            }
        }

        // Tabs on a track, or on the bar itself, the active one raised from it.
        let radius = layout.radius(TRACK_RADIUS);
        let on_track = config.window.tab_track;
        let track = if on_track { palette.track } else { palette.chrome };
        if on_track && !layout.tabs.is_empty() {
            base.outlined(layout.track, radius, track, palette.track_outline, line);
        }
        let lit = |index: usize| {
            index == self.active
                || matches!(self.hover, Hit::Tab(i) | Hit::Icon(i, _) | Hit::Close(i) if i == index)
        };
        for pair in layout.tabs.windows(2) {
            let ((index, rect), (next, _)) = (pair[0], pair[1]);
            if !lit(index) && !lit(next) {
                let x = rect.right() + px(2.) - line / 2.;
                base.rect(
                    Rect::new(x, rect.y + rect.h * 0.3, line, rect.h * 0.4),
                    palette.separator,
                );
            }
        }
        let dragging = self.drag.as_ref().filter(|drag| drag.moving);
        let source = dragging.map(|drag| drag.source);
        let target = dragging.and_then(|drag| drag.target);
        let torn = target == Some(Target::Window);
        // Tabs slide along the track, the dragged one following the pointer.
        let (first, last) = match (layout.tabs.first(), layout.tabs.last()) {
            (Some((_, first)), Some((_, last))) => (first.x, last.x),
            _ => (0., 0.),
        };
        let held = match (source, dragging) {
            (Some(Source::Tab(index)), Some(drag)) => {
                let x = ((self.pointer.x - drag.grab) as f32).clamp(first, last);
                Some((index, layout.first as f32 + (x - first) / layout.step))
            },
            _ => None,
        };
        self.bar_effects.update(&self.tabs, self.hover, held, now);
        // Stacks fanned out cover what is beside them.
        let mut fanned = Layer::default();
        for &(index, mut rect) in &layout.tabs {
            let tab = &self.tabs[index];
            let active = index == self.active;
            let (slot, ..) = self.bar_effects.get(tab.pane().id, now);
            let x = rect.x + (slot - index as f32) * layout.step;
            rect.x = x.clamp(first, last).round();
            let layer = match held {
                // The dragged tab is above the others.
                Some((held, _)) if held == index => {
                    let shadow = Rect::new(rect.x, rect.y + px(1.), rect.w, rect.h + px(2.));
                    let radius = layout.radius(TAB_RADIUS + 1.);
                    lifted.rounded(shadow, radius, mix(palette.chrome, BLACK, 0.5));
                    &mut lifted
                },
                _ => &mut base,
            };

            let radius = layout.radius(TAB_RADIUS);
            // A ring around the tab that what is dragged would join.
            if target == Some(Target::Merge(index)) {
                let ring = px(2.);
                layer.rounded(rect.inset(-ring), radius + layout.curve(ring), palette.primary);
                layer.rounded(rect, radius, track);
            }
            let surface = if active {
                layer.outlined(rect, radius, palette.raised, palette.raised_outline, line);
                palette.raised
            } else if lit(index) {
                layer.rounded(rect, radius, palette.hover);
                palette.hover
            } else {
                track
            };

            let title = |pane: usize| match &tab.panes[pane] {
                _ if active && pane == tab.focus => window.title(),
                pane => &pane.title,
            };
            // Colors of a pane's badge, `alpha` of the way out of the tab.
            let colors = |pane, alpha| {
                let accent = palette.accent(title(pane));
                (surface, mix(surface, accent, alpha), mix(surface, palette.foreground, alpha))
            };
            let front = layout.badge(rect);
            let count = tab.panes.len();
            // The title fades out under a stack fanned over it.
            let mut fade = 0.;
            if count == 1 {
                let pane = tab.pane();
                badge(layer, &layout, front, pane.icon.as_ref(), &pane.badge, font, colors(0, 1.));
            } else {
                let motion = |pane: usize| {
                    let (_, fan, lift) = self.bar_effects.get(tab.panes[pane].id, now);
                    (fan, lift)
                };
                let (row, chip_size) = (layout.row(front), layout.chip_size(count));
                let deck = deck(front, count, tab.focus, layout.scale, row, chip_size, motion);
                let fan = motion(tab.focus).0.clamp(0., 1.);
                let fanned_out = fan > 0.001;
                fade = fan;
                let into = if fanned_out { &mut fanned } else { &mut *layer };
                if fanned_out {
                    let rows = count.div_ceil(row);
                    let (plate, float) = plate(rect, &deck.icons, rows, fan, bar, layout.scale);
                    let radius =
                        layout.radius(TAB_RADIUS - 2.) + layout.curve(rect.y + px(2.) - plate.y);
                    if float > 0. {
                        // The buttons it lies over give way to it rather than show half cut.
                        let under = layout.buttons.iter().filter(|(_, b)| b.intersects(&plate));
                        for &(_, button) in under {
                            let corner = layout.radius(BUTTON_RADIUS);
                            into.rounded_alpha(button, corner, palette.chrome, float);
                        }
                        let shadow = Rect::new(plate.x, plate.y + px(2.), plate.w, plate.h);
                        into.rounded_alpha(shadow, radius + layout.radius(2.), BLACK, 0.3 * float);
                        into.rounded_alpha(plate, radius, palette.popup_outline, float);
                        into.rounded_alpha(plate.inset(line), radius - line, surface, fan);
                    } else {
                        into.rounded_alpha(plate, radius, surface, fan);
                    }
                }
                let card = |into: &mut Layer, order: usize, pane: usize, icon: Rect| {
                    let (fan, lift) = motion(pane);
                    // A zoom hides the panes behind the focused one, and dims their icons.
                    let hidden = tab.zoom && pane != tab.focus;
                    let alpha = opacity(order, count, fan) * if hidden { 0.45 } else { 1. };
                    // Icons are cards, each cut out of the ones behind it.
                    if order + 1 < deck.icons.len() || deck.chip.is_some() {
                        let cut = icon.inset(-px(1.));
                        let backed = if order < shown(count) { 1. } else { fan.min(1.) };
                        into.rounded_alpha(cut, layout.curve(cut.w / 4.), surface, backed);
                    }
                    let Pane { icon: image, badge: text, .. } = &tab.panes[pane];
                    match image {
                        Some(image) => {
                            let card = mix(surface, palette.foreground, 0.16 + 0.1 * lift);
                            into.rounded_alpha(icon, layout.curve(icon.w / 4.), card, alpha);
                            let inner = icon.inset((icon.w / 9.).round());
                            into.image_alpha(inner, image, palette.foreground, alpha);
                        },
                        None => badge(into, &layout, icon, None, text, font, colors(pane, alpha)),
                    }
                };
                // From the back: the panes left out of the deck, which come out from under the
                // count of them, then the cards.
                let back_to_front = deck.icons.iter().rev().enumerate().rev();
                let dragged = |pane: usize| source == Some(Source::Pane(index, pane));
                let (cards, hidden): (Vec<_>, Vec<_>) = back_to_front
                    .filter(|&(_, &(pane, _))| !dragged(pane))
                    .partition(|&(order, _)| order < shown(count));
                for (order, &(pane, icon)) in hidden {
                    card(into, order, pane, icon);
                }
                if let Some((chip, _)) = deck.chip.filter(|_| fan < 1.) {
                    let fill = mix(surface, palette.foreground, 0.12);
                    into.rounded_alpha(chip, layout.curve(chip.h / 2.), fill, 1. - fan);
                    let tuck = px(4.) + line;
                    let label = Rect::new(chip.x + tuck, chip.y, chip.w - tuck, chip.h);
                    let color = if lit(index) { palette.foreground } else { palette.muted };
                    let color = mix(fill, color, 1. - fan);
                    into.label(label, &count_label(count), font, color, false);
                }
                for (order, &(pane, icon)) in cards {
                    card(into, order, pane, icon);
                }
            }

            let close = layout.close(rect);
            let left = layout.title(rect, count);
            let field = Rect::new(left, rect.y, close.x - px(2.) - left, rect.h);
            if rename::draw(self, &layout, &palette, index, field, layer) {
                continue;
            }
            if lit(index) {
                if self.hover == Hit::Close(index) {
                    let radius = layout.radius(TAB_RADIUS - 4.);
                    layer.rounded(close.square(px(20.)), radius, palette.pressed);
                }
                let lines = [[-1., -1., 1., 1.], [-1., 1., 1., -1.]];
                layer.icon(close.center(), px(4.), icon, &lines, palette.muted);
            }

            let right = if lit(index) { close.x + px(2.) } else { rect.right() - px(10.) };
            let name = tab.name.as_deref().unwrap_or_else(|| title(tab.focus));
            let title = font.fit(name, (right - left).max(0.), active);
            let color = if lit(index) { palette.foreground } else { palette.muted };
            let color = mix(color, surface, fade);
            layer.text(left, rect.center().1 - line_height / 2., title, color, active);
        }

        // Where a tab, or a pane let out of its tab, would land.
        let gap = match (target, source) {
            (Some(Target::Detach(index)), _) => Some(index),
            (Some(Target::Reorder(index)), Some(Source::Tab(from))) if index != from => {
                Some(if index < from { index } else { index + 1 })
            },
            _ => None,
        };
        if let (Some(x), Some(&(_, rect))) =
            (gap.and_then(|gap| layout.gap(gap)), layout.tabs.first())
        {
            let caret = Rect::new(x - px(1.), rect.y + px(4.), px(2.), rect.h - px(8.));
            top.rounded(caret, layout.curve(px(1.)), palette.primary);
        }

        // The icon of a dragged pane follows the pointer.
        if let Some(Source::Pane(index, pane)) = source {
            if let Some(pane) = self.tabs.get(index).and_then(|tab| tab.panes.get(pane)) {
                let (x, y) = (self.pointer.x as f32, self.pointer.y as f32);
                let size = px(22.);
                let icon = Rect::new((x - size / 2.).round(), (y - size / 2.).round(), size, size);
                let shadow = Rect::new(icon.x, icon.y + px(2.), size, size).inset(-px(2.));
                lifted.rounded_alpha(shadow, layout.curve(shadow.w / 4.), BLACK, 0.3);
                let colors = (palette.raised, palette.accent(&pane.title), palette.foreground);
                lifted.rounded(icon.inset(-px(1.)), layout.curve(size / 4.), palette.raised);
                badge(&mut lifted, &layout, icon, pane.icon.as_ref(), &pane.badge, font, colors);
            }
        }

        for &(hit, rect) in &layout.buttons {
            self.draw_button(&mut base, &layout, &palette, hit, rect);
        }

        if let (Some(menu), Some((rect, row))) = (&self.menu, layout.menu) {
            let radius = layout.radius(TRACK_RADIUS);
            let shadow = Rect::new(rect.x, rect.y + px(2.), rect.w, rect.h);
            top.rounded_alpha(shadow, radius + layout.radius(2.), BLACK, 0.3);
            top.outlined(rect, radius, palette.popup, palette.popup_outline, line);
            let highlighted = match self.hover {
                Hit::Item(index) => index,
                _ => menu.selected,
            };
            let names = menu.profiles.iter().map(|profile| profile.name.as_str());
            for (index, name) in names.chain([EDIT_PROFILES]).enumerate() {
                let item = Rect::new(
                    rect.x + px(6.),
                    rect.y + px(6.) + index as f32 * row,
                    rect.w - px(12.),
                    row,
                );
                let surface = if index == highlighted { palette.hover } else { palette.popup };
                if index == highlighted {
                    top.rounded(item, layout.radius(ITEM_RADIUS), surface);
                }

                let icon_rect = Rect::new(item.x + px(8.), item.y, px(18.), row).square(px(18.));
                match menu.profiles.get(index) {
                    Some(profile) => {
                        let image = profile.image(config);
                        let colors = (surface, palette.accent(name), palette.foreground);
                        let (image, text) = (image.as_ref(), &profile.badge());
                        badge(&mut top, &layout, icon_rect, image, text, font, colors);
                    },
                    None => top.gear(icon_rect.center(), px(8.), palette.muted, surface),
                }
                let y = item.center().1 - line_height / 2.;
                top.text(icon_rect.right() + px(10.), y, name.into(), palette.foreground, false);
            }
        }

        if torn {
            lifted.fade(0.5, track);
        }
        let card = peek::draw(self, &layout, &palette, window.title(), now);
        let mut layers = vec![base, fanned, lifted];
        layers.extend(card);
        layers.push(top);
        layers.extend(context::draw(self, &layout, &palette, now));
        if let Some(settings) = &self.settings {
            layers.extend(settings.draw(&layout, &palette, config, window));
        }

        // Draw over the whole window, then give the terminal its own viewport back.
        let window = SizeInfo::new(width, layout.height, 1., 1., 0., 0., false);
        Layer::draw(layers, renderer, font, &window);

        // Round the window by cutting its corners out of everything drawn there.
        if self.cut {
            let radius = layout.radius(RADIUS);
            let mask = paint::mask(Rect::new(0., 0., width, layout.height), radius);
            renderer.mask_rects(&window, &font.metrics, mask);
        }
        renderer.resize(size);
    }

    fn draw_button(
        &self,
        layer: &mut Layer,
        layout: &Layout,
        palette: &Palette,
        hit: Hit,
        rect: Rect,
    ) {
        let px = |value: f32| layout.px(value);
        let hovered = self.hover == hit;
        let width = (layout.scale).max(1.);
        let center = rect.center();
        let restore = self.frame != Frame::Normal;
        let square = |layer: &mut Layer, (x, y): (f32, f32), size: f32, color: Rgb| {
            let lines =
                [[-1., -1., 1., -1.], [1., -1., 1., 1.], [1., 1., -1., 1.], [-1., 1., -1., -1.]];
            layer.icon((x, y), size, width, &lines, color);
        };
        let window_icon = |layer: &mut Layer, size: f32, color: Rgb| match hit {
            Hit::Minimize => layer.icon(center, size, width, &[[-1., 0., 1., 0.]], color),
            Hit::Maximize if restore => {
                square(layer, (center.0 - size * 0.2, center.1 + size * 0.2), size * 0.8, color);
                let lines = [[-0.6, -1., 1., -1.], [1., -1., 1., 0.6]];
                layer.icon(
                    (center.0 + size * 0.1, center.1 - size * 0.1),
                    size * 0.9,
                    width,
                    &lines,
                    color,
                );
            },
            Hit::Maximize => square(layer, center, size, color),
            _ => layer.icon(center, size, width, &[[-1., -1., 1., 1.], [-1., 1., 1., -1.]], color),
        };

        let control = matches!(hit, Hit::Minimize | Hit::Maximize | Hit::CloseWindow);
        match hit {
            // Flat buttons, red when closing, like Windows' own.
            _ if control && cfg!(windows) => {
                let danger = hovered && hit == Hit::CloseWindow;
                if hovered {
                    let color = if danger { palette.danger } else { palette.hover };
                    layer.rounded(rect, layout.radius(BUTTON_RADIUS), color);
                }
                let color = if danger { WHITE } else { palette.muted };
                window_icon(layer, px(5.), color);
            },
            // Round buttons, like GNOME's and KDE's, that only show their shape under the pointer.
            _ if control => {
                let danger = hovered && hit == Hit::CloseWindow;
                if hovered {
                    let background = if danger {
                        palette.danger
                    } else {
                        mix(palette.chrome, palette.foreground, 0.16)
                    };
                    layout.dot(layer, center, px(CONTROL_DOT), background);
                }
                let color = match (danger, hovered) {
                    (true, _) => WHITE,
                    (_, true) => palette.foreground,
                    _ => palette.muted,
                };
                window_icon(layer, px(4.), color);
            },
            _ => {
                if hovered {
                    layer.rounded(rect, layout.radius(BUTTON_RADIUS), palette.hover);
                }
                let color =
                    if hovered || hit == Hit::New { palette.foreground } else { palette.muted };
                let (size, lines): (f32, &[[f32; 4]]) = match hit {
                    Hit::New => (5., &[[-1., 0., 1., 0.], [0., -1., 0., 1.]]),
                    Hit::Menu => (4., &[[-1., -0.5, 0., 0.5], [0., 0.5, 1., -0.5]]),
                    Hit::Previous => (4., &[[0.5, -1., -0.5, 0.], [-0.5, 0., 0.5, 1.]]),
                    Hit::Next => (4., &[[-0.5, -1., 0.5, 0.], [0.5, 0., -0.5, 1.]]),
                    _ => {
                        let background = if hovered { palette.hover } else { palette.chrome };
                        return layer.gear(center, px(8.), color, background);
                    },
                };
                layer.icon(center, px(size), (1.3 * layout.scale).max(1.), lines, color);
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRONT: Rect = Rect { x: 100., y: 15., w: 18., h: 18. };
    const MIDDLE: f32 = 24.;

    fn at_rest(_: usize) -> (f32, f32) {
        (0., 0.)
    }

    fn fanned(_: usize) -> (f32, f32) {
        (1., 0.)
    }

    /// The size of the count of a deck, in a font of 7 pixels a character and a line of 17.
    fn chip_size(count: usize) -> (f32, f32) {
        (count_label(count).len() as f32 * 7. + 10., 19.)
    }

    fn stack(
        front: Rect,
        count: usize,
        focus: usize,
        scale: f32,
        motion: impl Fn(usize) -> (f32, f32),
    ) -> Icons {
        deck(front, count, focus, scale, ROW_MAX, chip_size(count), motion).icons
    }

    #[test]
    fn single_pane_fills_the_badge() {
        assert_eq!(stack(FRONT, 1, 0, 1., at_rest), vec![(0, FRONT)]);
    }

    #[test]
    fn stack_puts_the_focused_pane_in_front_of_the_others() {
        let icons = stack(FRONT, 3, 1, 1., at_rest);
        // Drawn from the back: the other panes in reading order behind the focused one.
        let order: Vec<_> = icons.iter().map(|&(pane, _)| pane).collect();
        assert_eq!(order, [2, 0, 1]);
        assert_eq!(icons[2].1, FRONT);
        for pair in icons.windows(2) {
            let ((_, back), (_, front)) = (pair[0], pair[1]);
            // Each card is smaller than the one in front of it and peeks out to its right.
            assert!(back.w < front.w && back.h < front.h);
            assert!(back.x > front.x && back.right() > front.right());
            assert!((back.center().1 - front.center().1).abs() <= 0.5);
        }
    }

    #[test]
    fn deck_of_four_shows_every_card() {
        let deck = deck(FRONT, 4, 3, 1., ROW_MAX, chip_size(4), at_rest);
        assert_eq!(deck.icons.len(), 4);
        assert_eq!(deck.icons.last().unwrap().0, 3);
        assert_eq!(deck.chip, None);
    }

    #[test]
    fn larger_decks_show_three_cards_and_count_the_others() {
        for count in [5, 9, 40] {
            let deck = deck(FRONT, count, 4, 1., ROW_MAX, chip_size(count), at_rest);
            let order: Vec<_> = deck.icons.iter().map(|&(pane, _)| pane).collect();
            assert_eq!(order, [1, 0, 4]);
            let (chip, hidden) = deck.chip.unwrap();
            assert_eq!(hidden, count - 3);
            // The count overlaps the last card and is centered on the badge.
            let last = deck.icons[0].1;
            assert!(chip.x < last.right() && chip.right() > last.right(), "{chip:?} {last:?}");
            assert!((chip.center().1 - FRONT.center().1).abs() <= 0.5);
            // However many panes, the deck leaves the title its room.
            assert!(deck.end() <= FRONT.x + 64., "{count}: {}", deck.end());
        }
    }

    #[test]
    fn count_never_stands_for_a_single_pane() {
        for count in 1..=100 {
            let deck = deck(FRONT, count, 0, 1., ROW_MAX, chip_size(count), at_rest);
            if let Some((_, hidden)) = deck.chip {
                assert!(hidden >= 2, "{count}");
            }
            assert_eq!(deck.icons.len() + deck.chip.map_or(0, |(_, hidden)| hidden), count);
        }
    }

    #[test]
    fn cards_fade_with_depth_until_fanned_out() {
        let rest: Vec<_> = (0..4).map(|order| opacity(order, 4, 0.)).collect();
        assert_eq!(rest, DEPTH_FADE);
        assert_eq!(opacity(3, 6, 0.), 0.);
        assert!((0..12).all(|order| opacity(order, 12, 1.) == 1.));
        assert_eq!(opacity(5, 12, 0.5), 0.5);
    }

    #[test]
    fn panes_left_out_come_from_behind_the_last_card() {
        let rest = deck(FRONT, 6, 0, 1., ROW_MAX, chip_size(6), at_rest).icons;
        let opening = deck(FRONT, 6, 0, 1., ROW_MAX, chip_size(6), |_| (0.01, 0.)).icons;
        assert_eq!(opening.len(), 6);
        let last = rest[0].1;
        for &(_, icon) in &opening[..3] {
            assert!((icon.center().0 - last.center().0).abs() <= 1., "{icon:?} {last:?}");
        }
    }

    #[test]
    fn fanned_stack_spreads_icons_apart_and_lifts_one() {
        let icons = stack(FRONT, 4, 0, 1., |pane| (1., f32::from(pane == 2)));
        for pair in icons.windows(2) {
            let ((_, back), (_, front)) = (pair[0], pair[1]);
            assert!(front.right() <= back.x, "{front:?} {back:?}");
        }
        let icon = |pane: usize| icons.iter().find(|(p, _)| *p == pane).unwrap().1;
        assert_eq!((icon(0).w, icon(1).w, icon(2).w, icon(3).w), (22., 22., 26., 22.));
        assert!(icon(2).center().1 < icon(1).center().1);
    }

    #[test]
    fn fanned_rows_wrap_into_a_shelf() {
        for (count, row) in [(6, 8), (12, 8), (40, 8), (12, 3), (5, 1)] {
            let icons = deck(FRONT, count, 0, 1., row, chip_size(count), fanned).icons;
            assert_eq!(icons.len(), count);
            let mut rows: Vec<Vec<Rect>> = Vec::new();
            // From the front, in reading order.
            for &(_, icon) in icons.iter().rev() {
                match rows.last_mut().filter(|line| line[0].y == icon.y) {
                    Some(line) => line.push(icon),
                    None => rows.push(vec![icon]),
                }
            }
            assert_eq!(rows.len(), count.div_ceil(row), "{count} {row}");
            for (index, line) in rows.iter().enumerate() {
                assert!(line.len() <= row);
                assert_eq!(line[0].x, FRONT.x);
                assert_eq!(line[0].y - rows[0][0].y, index as f32 * PITCH);
                assert!(line.windows(2).all(|pair| pair[1].x - pair[0].x == PITCH));
            }
        }
    }

    #[test]
    fn stack_scales() {
        let front = Rect::new(100., 30., 36., 36.);
        let icons = stack(front, 2, 0, 2., at_rest);
        assert_eq!(icons[1].1, front);
        assert_eq!(icons[0].1.right() - front.right(), 12.);
    }

    #[test]
    fn icons_are_hit_front_first() {
        let icons = stack(FRONT, 3, 0, 1., at_rest);
        let at = |x: f32| icon_at(&icons, (x, MIDDLE));
        assert_eq!(at(99.), None);
        assert_eq!(at(100.), Some(0));
        assert_eq!(at(117.), Some(0));
        assert_eq!(at(120.), Some(1));
        assert_eq!(at(128.), Some(2));
        assert_eq!(at(130.), None);
    }

    #[test]
    fn icon_entered_at_rest_stays_under_a_still_pointer() {
        for count in [4, 6, 12] {
            for scale in [1f32, 1.25, 2.] {
                let front = Rect::new(100., 15., (18. * scale).round(), 18.);
                let middle = front.center().1;
                let size = (chip_size(count).0 * scale, chip_size(count).1 * scale);
                let rest = deck(front, count, 1, scale, ROW_MAX, size, at_rest);
                let fanned = stack(front, count, 1, scale, fanned);
                let left = rest.icons.last().unwrap().1.x;
                for x in (left as i32..rest.end() as i32).map(|x| x as f32 + 0.5) {
                    let pane = icon_at(&fanned, (x, middle)).unwrap();
                    // Lifting the icon picked keeps it the one under the pointer.
                    let lifted = stack(front, count, 1, scale, |p| (1., f32::from(p == pane)));
                    assert_eq!(icon_at(&lifted, (x, middle)), Some(pane), "{count} {scale} {x}");
                }
            }
        }
    }

    #[test]
    fn fanned_icons_are_hit_without_gaps() {
        let icons = stack(FRONT, 3, 0, 1., fanned);
        let at = |x: f32| icon_at(&icons, (x, MIDDLE));
        let (back, middle, front) = (icons[0].1, icons[1].1, icons[2].1);
        assert_eq!(at(front.x), Some(0));
        // Between two icons the nearer one is hit.
        assert_eq!(at(front.right() + 1.), Some(0));
        assert_eq!(at(middle.x - 1.), Some(1));
        assert_eq!(at(back.right() - 1.), Some(2));
        assert_eq!(at(back.right()), None);
    }

    #[test]
    fn shelf_icons_are_hit_without_gaps() {
        let icons =
            deck(FRONT, 12, 0, 1., ROW_MAX, chip_size(12), |pane| (1., f32::from(pane == 9))).icons;
        // Each icon is hit at its center, the lifted one too.
        for &(pane, icon) in &icons {
            assert_eq!(icon_at(&icons, icon.center()), Some(pane));
        }
        // Every point of the shelf picks an icon of the row nearest it, short rows included.
        let (left, right) = extent(&icons);
        let bottom = icons.iter().map(|(_, icon)| icon.bottom()).fold(f32::MIN, f32::max);
        for y in (9..bottom as i32 + 4).map(|y| y as f32 + 0.5) {
            for x in (left as i32..right as i32).map(|x| x as f32 + 0.5) {
                let pane = icon_at(&icons, (x, y)).unwrap();
                let row = if y < MIDDLE + PITCH / 2. { 0 } else { 1 };
                // The lifted icon reaches a little further into the gap between the rows.
                if (y - MIDDLE - PITCH / 2.).abs() > 3. {
                    assert_eq!(pane / ROW_MAX, row, "{x} {y}");
                }
            }
        }
    }

    #[test]
    fn plate_floats_once_past_its_tab_or_the_bar() {
        let tab = Rect::new(91., 9., 150., 30.);
        let plate = |count: usize, row: usize| {
            let icons = deck(FRONT, count, 0, 1., row, chip_size(count), fanned).icons;
            (icons.clone(), plate(tab, &icons, count.div_ceil(row), 1., 48., 1.))
        };
        // Within its tab it lies flat, as before.
        let (icons, (flat, float)) = plate(4, ROW_MAX);
        assert_eq!(float, 0.);
        assert_eq!(flat, Rect::new(97., 11., 106., 26.));
        assert!(icons.iter().all(|(_, icon)| flat.x < icon.x && icon.right() < flat.right()));
        // Past its tab's end, or below the bar, it floats with more room around its icons.
        for (count, row) in [(6, ROW_MAX), (12, ROW_MAX), (4, 2)] {
            let (icons, (plate, float)) = plate(count, row);
            assert_eq!(float, 1., "{count} {row}");
            for (_, icon) in icons {
                assert!(icon.x - plate.x >= 5. && plate.right() - icon.right() >= 5.);
                assert!(icon.y - plate.y >= 4. && plate.bottom() - icon.bottom() >= 4.);
            }
        }
    }
}

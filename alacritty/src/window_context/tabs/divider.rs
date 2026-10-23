//! Dividers between the panes of a tab, dragged to share the space differently.
//!
//! Hovering a divider shows a grabber pill under the pointer; dragging it resizes the panes live,
//! keeping every pane at its smallest size and snapping to the middle. A double click shares the
//! space evenly again, and escape puts the divider back where the drag started.

use std::cell::Cell;
use std::time::{Duration, Instant};

use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton};
use winit::keyboard::{Key, NamedKey};
use winit::window::CursorIcon;

use crate::window_context::WindowContext;

use super::anim::{Curve, Tween};
use super::bar::{Layout, PANEL_RADIUS};
use super::paint::{Layer, Palette, Rect, mix};
use super::split::{Axis, Divider};
use super::{Press, Tabs, panes, redraw, set_ratio};

/// Thinnest band around a divider that takes the pointer, in logical pixels.
const BAND: f32 = 8.;
/// Distance from the middle within which a dragged divider snaps to it.
const SNAP: f32 = 6.;
/// The pill's thickness and its length at rest, at most a quarter of the divider.
const PILL_WIDTH: f32 = 4.;
const PILL_LENGTH: f32 = 40.;
/// How much longer the pill grows while dragged.
const PILL_GROWTH: f32 = 1.6;
/// How long the pill takes to show, hide or grow.
const PILL_MS: u64 = 140;
/// Longest time between the presses of a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// The dividers' hover and drag.
pub struct State {
    /// The divider under the pointer, by its index.
    hovered: Option<usize>,
    drag: Option<Drag>,
    /// The divider the pill was last shown on, and the pointer's place along it.
    pill_at: Option<(usize, f32)>,
    /// How shown the pill is: 0 hidden, 1 hovered, 2 dragged; updated as it is drawn.
    pill: Cell<Tween<f32>>,
    /// The divider last pressed, and when, to reset it on a double click.
    last_click: Option<(usize, Instant)>,
}

/// A divider being dragged.
struct Drag {
    index: usize,
    /// Pointer offset from the divider's middle at the press.
    grab: f32,
    /// The pair's ratio at the press, which escape restores.
    ratio: f32,
}

impl Default for State {
    fn default() -> Self {
        let pill = Cell::new(Tween::new(0.));
        Self { hovered: None, drag: None, pill_at: None, pill, last_click: None }
    }
}

impl State {
    /// Whether the pill is still moving at `now`.
    pub fn running(&self, now: Instant) -> bool {
        self.pill.get().running(now)
    }
}

/// Follow the pointer over the dividers, or drag one; returns whether the pointer is theirs.
pub fn moved(ctx: &mut WindowContext, position: PhysicalPosition<f64>) -> bool {
    let tabs = &mut ctx.display.tabs;
    let point = (position.x as f32, position.y as f32);
    if let Some((index, grab)) = tabs.divider.drag.as_ref().map(|drag| (drag.index, drag.grab)) {
        let Some(divider) = tabs.dividers().into_iter().find(|d| d.index == index) else {
            return true;
        };
        let snap = SNAP * tabs.scale as f32;
        let ratio = ratio(&divider, across(divider.axis, point), grab, tabs.spacing(), snap);
        tabs.divider.pill_at = Some((index, along(divider.axis, point)));
        set_ratio(ctx, index, ratio);
        redraw(ctx);
        return true;
    }

    let hovered = under(tabs, point);
    let changed = tabs.divider.hovered != hovered;
    tabs.divider.hovered = hovered;
    let Some(index) = hovered else {
        if changed {
            redraw(ctx);
        }
        return false;
    };
    let divider = tabs.dividers().into_iter().find(|d| d.index == index);
    let axis = divider.map_or(Axis::Row, |divider| divider.axis);
    tabs.divider.pill_at = Some((index, along(axis, point)));
    // The pointer left any handle it was on for the divider.
    tabs.handle = None;
    ctx.display.window.set_mouse_cursor(cursor(axis));
    redraw(ctx);
    true
}

/// Press a divider, which sets [`super::Press::Divider`]; returns whether one was pressed.
pub fn press(ctx: &mut WindowContext, button: MouseButton) -> bool {
    let tabs = &mut ctx.display.tabs;
    let point = (tabs.pointer.x as f32, tabs.pointer.y as f32);
    let Some(divider) = under(tabs, point).and_then(|i| tabs.dividers().get(i).copied()) else {
        return false;
    };
    if button != MouseButton::Left {
        // Other buttons do nothing on a divider, rather than reach a terminal beside it.
        tabs.press = Press::Swallowed;
        return true;
    }

    let now = Instant::now();
    let last = tabs.divider.last_click.replace((divider.index, now));
    if double_click(last, divider.index, now) {
        // Sliding back to the middle animates, which a drag does not.
        tabs.divider.last_click = None;
        tabs.divider.pill_at = None;
        tabs.press = Press::Swallowed;
        set_ratio(ctx, divider.index, 0.5);
        return true;
    }

    let spacing = tabs.spacing();
    let grab = across(divider.axis, point) - divider.at;
    let ratio = ratio(&divider, divider.at, 0., spacing, 0.);
    tabs.divider.drag = Some(Drag { index: divider.index, grab, ratio });
    tabs.press = Press::Divider;
    redraw(ctx);
    true
}

/// End a drag where it is.
pub fn release(ctx: &mut WindowContext) {
    let tabs = &mut ctx.display.tabs;
    let Some(drag) = tabs.divider.drag.take() else { return };
    // A press that moved the divider starts no double click.
    let moved = tabs.dividers().into_iter().find(|d| d.index == drag.index).is_some_and(|d| {
        (ratio(&d, d.at, 0., tabs.spacing(), 0.) - drag.ratio).abs() > f32::EPSILON
    });
    if moved {
        tabs.divider.last_click = None;
    }
    redraw(ctx);
}

/// Handle a key while dragging, returning whether it was used: escape restores the ratio.
pub fn key(ctx: &mut WindowContext, key: &KeyEvent) -> bool {
    let tabs = &mut ctx.display.tabs;
    if tabs.divider.drag.is_none() || key.state != ElementState::Pressed {
        return false;
    }
    if key.logical_key == Key::Named(NamedKey::Escape) {
        let drag = tabs.divider.drag.take().unwrap();
        // The divider slides back, and the release that follows is nobody's.
        tabs.press = Press::Swallowed;
        (tabs.divider.last_click, tabs.divider.pill_at) = (None, None);
        set_ratio(ctx, drag.index, drag.ratio);
    }
    // Keys wait for the drag to end.
    true
}

/// Draw the pill on the hovered or dragged divider, fading as it shows and hides.
pub fn draw(tabs: &Tabs, layout: &Layout, palette: &Palette, layer: &mut Layer, now: Instant) {
    let state = &tabs.divider;
    let modal = tabs.settings.is_some()
        || tabs.menu.is_some()
        || tabs.context.is_some()
        || tabs.rename.is_some()
        || tabs.drag.is_some();
    let point = (tabs.pointer.x as f32, tabs.pointer.y as f32);
    // The hover is only trusted while the pointer is still there.
    let hovered = state.hovered.filter(|&index| !modal && under(tabs, point) == Some(index));
    let target = match (&state.drag, hovered) {
        (Some(_), _) => 2.,
        (None, Some(_)) => 1.,
        (None, None) => 0.,
    };
    let mut pill = state.pill.get();
    pill.to(target, now, PILL_MS, Curve::EASE_OUT);
    state.pill.set(pill);
    let level = pill.get(now);

    let Some((index, along)) = state.pill_at else { return };
    let Some(divider) = tabs.dividers().into_iter().find(|d| d.index == index) else { return };
    if level <= 0. {
        return;
    }
    let grow = (level - 1.).clamp(0., 1.);
    let (width, ends) = (layout.px(PILL_WIDTH), layout.px(PANEL_RADIUS));
    let (start, span) = match divider.axis {
        Axis::Row => (divider.area.y, divider.area.h),
        Axis::Column => (divider.area.x, divider.area.w),
    };
    let length = layout.px(PILL_LENGTH).min(span / 4.) * (1. + (PILL_GROWTH - 1.) * grow);
    let middle = along.clamp(start + ends + length / 2., start + span - ends - length / 2.);
    let (x, y, w, h) = match divider.axis {
        Axis::Row => (divider.at - width / 2., middle - length / 2., width, length),
        Axis::Column => (middle - length / 2., divider.at - width / 2., length, width),
    };
    let rest = mix(palette.chrome, palette.foreground, 0.4);
    let color = mix(rest, palette.highlight, grow);
    layer.rounded_alpha(Rect::new(x, y, w, h), layout.curve(width / 2.), color, level.min(1.));
}

/// The divider of the active tab under `point`, unless a pane's drag handle is there.
fn under(tabs: &Tabs, point: (f32, f32)) -> Option<usize> {
    let scale = tabs.scale as f32;
    let position = PhysicalPosition::new(f64::from(point.0), f64::from(point.1));
    if panes::handle(tabs, position, scale).is_some() {
        return None;
    }
    // The band is as thick as the gap between the cards, but never too thin to hit.
    let gap = tabs.spacing() + 2. * tabs.margin();
    let reach = gap.max(BAND * scale) / 2.;
    at(&tabs.dividers(), point, reach, (PANEL_RADIUS * scale).round())
}

/// The divider of `dividers` within `reach` of `point`, outside `ends` from either end of it: the
/// nearest, or the deepest of those as near.
fn at(dividers: &[Divider], (x, y): (f32, f32), reach: f32, ends: f32) -> Option<usize> {
    dividers
        .iter()
        .filter_map(|divider| {
            let area = divider.area;
            let (across, along, start, end) = match divider.axis {
                Axis::Row => (x, y, area.y, area.bottom()),
                Axis::Column => (y, x, area.x, area.right()),
            };
            let distance = (across - divider.at).abs();
            let inside = along >= start + ends && along <= end - ends;
            (distance <= reach && inside).then_some((distance, divider.index))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)))
        .map(|(_, index)| index)
}

/// The ratio of `divider` for the pointer at `pointer` along its axis, kept `grab` from its middle,
/// with `spacing` between its parts; the middle within `snap` of it.
fn ratio(divider: &Divider, pointer: f32, grab: f32, spacing: f32, snap: f32) -> f32 {
    let (start, len) = match divider.axis {
        Axis::Row => (divider.area.x, divider.area.w),
        Axis::Column => (divider.area.y, divider.area.h),
    };
    let at = pointer - grab;
    if (at - (start + len / 2.)).abs() <= snap {
        return 0.5;
    }
    ((at - spacing / 2. - start) / (len - spacing)).clamp(0., 1.)
}

/// Whether a press of the divider `index` at `now` follows the `last` one as a double click.
fn double_click(last: Option<(usize, Instant)>, index: usize, now: Instant) -> bool {
    last.is_some_and(|(last, at)| last == index && now.saturating_duration_since(at) < DOUBLE_CLICK)
}

/// The coordinate of `point` that moves a divider of `axis`.
fn across(axis: Axis, (x, y): (f32, f32)) -> f32 {
    if axis == Axis::Row { x } else { y }
}

/// The coordinate of `point` along a divider of `axis`.
fn along(axis: Axis, (x, y): (f32, f32)) -> f32 {
    if axis == Axis::Row { y } else { x }
}

fn cursor(axis: Axis) -> CursorIcon {
    match axis {
        Axis::Row => CursorIcon::ColResize,
        Axis::Column => CursorIcon::RowResize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_context::tabs::split::{Side, Split};

    const AREA: Rect = Rect { x: 0., y: 40., w: 800., h: 600. };

    /// Two panes side by side, the second of which is two stacked panes.
    fn dividers(spacing: f32) -> Vec<Divider> {
        let stacked = Split::Pane(2).join(Split::Pane(3), Axis::Column, Side::After);
        let layout = Split::Pane(1).join(stacked, Axis::Row, Side::After);
        layout.dividers(AREA, spacing)
    }

    #[test]
    fn bands_take_the_pointer_near_their_line() {
        let dividers = dividers(16.);
        let (row, column) = (dividers[0], dividers[1]);
        assert_eq!((row.axis, column.axis), (Axis::Row, Axis::Column));
        assert_eq!(at(&dividers, (row.at, 200.), 8., 4.), Some(0));
        assert_eq!(at(&dividers, (row.at + 8., 200.), 8., 4.), Some(0));
        assert_eq!(at(&dividers, (row.at + 9., 200.), 8., 4.), None);
        assert_eq!(at(&dividers, (600., column.at - 3.), 8., 4.), Some(1));
        assert_eq!(at(&dividers, (200., column.at), 8., 4.), None, "the stack is on the right");
        assert_eq!(at(&dividers, (100., 100.), 8., 4.), None);
    }

    #[test]
    fn corners_are_left_free() {
        let dividers = dividers(16.);
        let row = dividers[0];
        assert_eq!(at(&dividers, (row.at, AREA.y + 3.), 8., 4.), None);
        assert_eq!(at(&dividers, (row.at, AREA.y + 4.), 8., 4.), Some(0));
        assert_eq!(at(&dividers, (row.at, AREA.bottom() - 3.), 8., 4.), None);
    }

    #[test]
    fn junction_goes_to_the_nearest_then_the_deeper() {
        let dividers = dividers(16.);
        let (row, column) = (dividers[0], dividers[1]);
        // Where the stack's divider meets the row's, in the gap of the row.
        let x = column.area.x;
        assert_eq!(x, row.at + 8.);
        assert_eq!(at(&dividers, (x, column.at + 2.), 8., 0.), Some(1));
        assert_eq!(at(&dividers, (x + 1., column.at + 7.), 8., 0.), Some(1));
        assert_eq!(at(&dividers, (x - 1., column.at + 2.), 8., 0.), Some(0), "before the stack");
        let tie = (x, column.at + 8.);
        assert_eq!((tie.0 - row.at).abs(), (tie.1 - column.at).abs());
        assert_eq!(at(&dividers, tie, 8., 0.), Some(1));
    }

    #[test]
    fn ratio_keeps_the_grab_snaps_and_clamps() {
        let row = dividers(16.)[0];
        assert_eq!(ratio(&row, row.at, 0., 16., 0.), 0.5);
        // Grabbed 5 px right of the middle: the pointer 100 px further gives 100 px more.
        let r = ratio(&row, row.at + 105., 5., 16., 6.);
        assert!((r * (800. - 16.) - (392. + 100.)).abs() < 0.01, "{r}");
        // Within 6 px of the middle it snaps there.
        assert_eq!(ratio(&row, 400. + 6., 0., 16., 6.), 0.5);
        assert_ne!(ratio(&row, 400. + 7., 0., 16., 6.), 0.5);
        assert_eq!(ratio(&row, -50., 0., 16., 6.), 0.);
        assert_eq!(ratio(&row, 900., 0., 16., 6.), 1.);
    }

    #[test]
    fn ratio_round_trips_through_the_layout() {
        let layout = Split::Pane(1).join(Split::Pane(2), Axis::Row, Side::After);
        let mut layout = layout;
        layout.set_ratio(0, 0.3);
        let divider = layout.dividers(AREA, 16.)[0];
        let ratio = ratio(&divider, divider.at, 0., 16., 0.);
        let mut copy = layout.clone();
        copy.set_ratio(0, ratio);
        assert_eq!(copy.rects(AREA, 16.), layout.rects(AREA, 16.));
    }

    #[test]
    fn double_clicks() {
        let now = Instant::now();
        assert!(!double_click(None, 0, now));
        assert!(double_click(Some((0, now)), 0, now + Duration::from_millis(300)));
        assert!(!double_click(Some((1, now)), 0, now + Duration::from_millis(300)));
        assert!(!double_click(Some((0, now)), 0, now + DOUBLE_CLICK));
    }
}

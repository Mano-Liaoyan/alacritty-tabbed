//! The chrome around the panes of the active tab: each sits in a panel of its own, set in from
//! its part of the window by the margin, so neighbouring panels are as far apart as the window's
//! edges. The focused pane of a grouped tab has a lit border, like Hyprland's active window.
//!
//! Panes slide to their new places and new ones pop in; only where they are drawn moves, their
//! terminals are resized once. Panes no longer shown, as a zoomed pane grows over them, stay
//! drawn under it until it covers them.
//!
//! Each pane of a group has a handle on the top edge of its card, which drags it.

use std::cell::Cell;
use std::mem;
use std::time::{Duration, Instant};

use winit::dpi::PhysicalPosition;
use winit::window::WindowId;

use super::anim::{Curve, Tween, scale};
use super::bar::{Hit, Layout, MARGIN, PANEL_RADIUS};
use super::dnd::{Source, Target};
use super::paint::{Layer, Palette, Rect, mix};
use super::split::{Axis, Side, Split};
use super::{Tabs, divider};

/// How long panes take to slide to their places, or to pop in, in milliseconds.
const MOVE_MS: u64 = 260;
/// How long a border takes to light up or fade.
const LIGHT_MS: u64 = 180;
/// How long the drop preview takes to follow the pointer.
const DROP_MS: u64 = 140;
/// How large a new pane starts, from its place.
const POP: f32 = 0.85;
/// Opacity of the tint over a pane whose icon is hovered, and of the drop preview.
const TINT: f32 = 0.06;
const DROP: f32 = 0.22;
/// Opacity of the chrome over a pane being dragged.
const VEIL: f32 = 0.4;
/// Size of a drag handle's plate, and of the zone around it that grabs it, in logical pixels.
const HANDLE: (f32, f32) = (32., 10.);
const HANDLE_ZONE: (f32, f32) = (48., 18.);
/// Size of the dots on a handle, and the distance between their centres.
const DOT: f32 = 3.;
const DOT_STEP: f32 = 5.;
/// How long a handle takes to light up as it is hovered.
const HANDLE_MS: u64 = 120;

/// Where the panes are drawn while they move to their places, and how lit their borders are.
#[derive(Default)]
pub struct Effects {
    /// The panes of the active tab.
    panes: Vec<Motion>,
    /// Panes no longer shown, drawn under the others until the moment they are covered.
    leaving: Vec<(Motion, Instant)>,
    /// What the panes were last placed in.
    area: Option<Rect>,
    /// Where a dragged pane would drop, and how visible that is; updated as it is drawn.
    drop: Cell<Option<(Tween<Rect>, Tween<f32>)>>,
    /// The moment the terminals are drawn for, which their chrome is drawn for too.
    clock: Cell<Option<Instant>>,
}

struct Motion {
    id: WindowId,
    frame: Tween<Rect>,
    /// How lit the border is, from 0 to 1, and how tinted the pane; updated as it is drawn.
    light: Cell<Tween<f32>>,
    tint: Cell<Tween<f32>>,
    /// How lit the drag handle is.
    handle: Cell<Tween<f32>>,
}

impl Effects {
    /// Follow the panes of the active tab to their new places `rects`, at `now`.
    ///
    /// Panes that were shown slide to their place, new ones pop in beside them, and the ones no
    /// longer placed stay under them while they move, when they `animate`; everything jumps into
    /// place when the window changes size or another tab's panes are shown.
    pub fn place(&mut self, rects: &[(WindowId, Rect)], now: Instant, animate: bool) {
        let area = rects.iter().map(|&(_, rect)| rect).reduce(union);
        let shown = rects.iter().any(|&(id, _)| self.motion(id).is_some());
        let animate = animate && shown && area == self.area;
        self.area = area;

        // Panes still leaving keep the moment they are covered; covered ones are gone.
        let leaving = mem::take(&mut self.leaving).into_iter().filter(|&(_, until)| until > now);
        let leaving = leaving.map(|(motion, until)| (motion, Some(until)));
        let mut old: Vec<_> = mem::take(&mut self.panes).into_iter().map(|m| (m, None)).collect();
        old.extend(leaving);
        self.panes = rects
            .iter()
            .map(|&(id, rect)| {
                let motion = old.iter().position(|(motion, _)| motion.id == id);
                let mut motion = motion.map(|i| old.remove(i).0).unwrap_or_else(|| Motion {
                    id,
                    frame: Tween::new(if animate { popped(rect) } else { rect }),
                    light: Cell::new(Tween::new(0.)),
                    tint: Cell::new(Tween::new(0.)),
                    handle: Cell::new(Tween::new(0.)),
                });
                if animate {
                    motion.frame.to(rect, now, MOVE_MS, Curve::SNAP);
                } else {
                    motion.frame = Tween::new(rect);
                }
                motion
            })
            .collect();

        if animate {
            let until = now + Duration::from_millis(MOVE_MS).mul_f32(scale());
            let old = old.into_iter().map(|(motion, since)| (motion, since.unwrap_or(until)));
            self.leaving = old.filter(|&(_, until)| until > now).collect();
        }
    }

    /// The panes no longer shown that are still drawn at `now`, under the others, and where.
    pub fn leaving(&self, now: Instant) -> Vec<(WindowId, Rect)> {
        let leaving = self.leaving.iter().filter(|&&(_, until)| until > now);
        leaving.map(|(motion, _)| (motion.id, motion.frame.get(now))).collect()
    }

    /// Where to draw the pane `id`, whose place is `rect`, at `now`.
    pub fn frame(&self, id: WindowId, rect: Rect, now: Instant) -> Rect {
        self.motion(id).map_or(rect, |motion| motion.frame.get(now))
    }

    /// Whether anything is still moving at `now`.
    pub fn running(&self, now: Instant) -> bool {
        let drop =
            self.drop.get().is_some_and(|(rect, alpha)| rect.running(now) || alpha.running(now));
        let leaving = self.leaving.iter().any(|&(_, until)| until > now);
        drop || leaving
            || self.panes.iter().any(|motion| {
                motion.frame.running(now)
                    || motion.light.get().running(now)
                    || motion.tint.get().running(now)
                    || motion.handle.get().running(now)
            })
    }

    /// Draw the chrome for `now`, the moment the terminals of the next frame are drawn for.
    pub fn hold(&self, now: Instant) {
        self.clock.set(Some(now));
    }

    fn motion(&self, id: WindowId) -> Option<&Motion> {
        self.panes.iter().find(|motion| motion.id == id)
    }

    /// How lit the border of the pane `id` is at `now`, and how tinted the pane, moving to
    /// whether it is `lit` and `tinted`.
    fn light(&self, id: WindowId, lit: bool, tinted: bool, now: Instant) -> (f32, f32) {
        let Some(motion) = self.motion(id) else { return (0., 0.) };
        (level(&motion.light, lit, LIGHT_MS, now), level(&motion.tint, tinted, LIGHT_MS, now))
    }

    /// How lit the drag handle of the pane `id` is at `now`, moving to whether it is `hovered`.
    fn handle(&self, id: WindowId, hovered: bool, now: Instant) -> f32 {
        self.motion(id).map_or(0., |motion| level(&motion.handle, hovered, HANDLE_MS, now))
    }

    /// Where to draw the drop preview at `now`, and how visible it is, moving to `target`.
    fn preview(&self, target: Option<Rect>, now: Instant) -> Option<(Rect, f32)> {
        // A preview that faded out pops in afresh.
        let mut drop = self.drop.get().filter(|(_, alpha)| alpha.get(now) > 0.);
        match (&mut drop, target) {
            (Some((rect, alpha)), target) => {
                if let Some(target) = target {
                    rect.to(target, now, DROP_MS, Curve::EASE_OUT);
                }
                alpha.to(if target.is_some() { 1. } else { 0. }, now, DROP_MS, Curve::EASE_OUT);
            },
            (None, Some(target)) => {
                let (mut rect, mut alpha) = (Tween::new(popped(target)), Tween::new(0.));
                rect.to(target, now, DROP_MS, Curve::EASE_OUT);
                alpha.to(1., now, DROP_MS, Curve::EASE_OUT);
                drop = Some((rect, alpha));
            },
            (None, None) => (),
        }
        self.drop.set(drop);
        drop.map(|(rect, alpha)| (rect.get(now), alpha.get(now))).filter(|&(_, alpha)| alpha > 0.)
    }
}

/// The level of `cell` at `now`, moving to 1 when `on` and to 0 otherwise in `ms`.
fn level(cell: &Cell<Tween<f32>>, on: bool, ms: u64, now: Instant) -> f32 {
    let mut tween = cell.get();
    tween.to(if on { 1. } else { 0. }, now, ms, Curve::EASE_OUT);
    cell.set(tween);
    tween.get(now)
}

/// The pane of the active tab whose drag handle is at `point`.
pub fn handle_at(tabs: &Tabs, layout: &Layout, point: PhysicalPosition<f64>) -> Option<usize> {
    handle(tabs, point, layout.scale)
}

/// The pane of the active tab whose drag handle is at `point`, at `scale`: panes have handles
/// while several are shown.
pub fn handle(tabs: &Tabs, point: PhysicalPosition<f64>, scale: f32) -> Option<usize> {
    let rects = tabs.pane_rects();
    if rects.len() < 2 || tabs.drag.is_some() {
        return None;
    }
    let cards: Vec<_> = rects.into_iter().map(|(index, rect)| (index, tabs.card(rect))).collect();
    grabbed(&cards, tabs.area().y, point, scale)
}

/// The pane of `cards`, by index, whose handle's zone holds `point`, below `top`.
fn grabbed(
    cards: &[(usize, Rect)],
    top: f32,
    point: PhysicalPosition<f64>,
    scale: f32,
) -> Option<usize> {
    let hit = cards.iter().find(|&&(_, card)| handle_rects(card, top, scale).1.contains(point));
    hit.map(|&(index, _)| index)
}

/// The plate of the drag handle on the top edge of `card`, at `scale`, and the zone around it
/// that grabs it; neither reaches above `top`.
fn handle_rects(card: Rect, top: f32, scale: f32) -> (Rect, Rect) {
    let px = |value: f32| (value * scale).round();
    let center = |w: f32, h: f32, y: f32| {
        let rect = Rect::new((card.x + (card.w - w) / 2.).round(), (y - h / 2.).round(), w, h);
        // Cut off above the top, so the bar keeps the pointer.
        let cut = (top - rect.y).max(0.);
        Rect::new(rect.x, rect.y + cut, rect.w, (rect.h - cut).max(0.))
    };
    let (w, h) = (px(HANDLE.0), px(HANDLE.1));
    // The plate sits on the edge, or just below the top where the card has no margin.
    let plate = center(w, h, card.y.max(top + h / 2.));
    let zone = center(px(HANDLE_ZONE.0), px(HANDLE_ZONE.1), plate.y + h / 2.);
    (plate, zone)
}

/// Draw the panel of each pane of the active tab, with the chrome around it, the focused pane's
/// border lit when the tab has several, and where a dragged pane would drop.
pub fn draw(tabs: &Tabs, layout: &Layout, palette: &Palette, layer: &mut Layer, now: Instant) {
    let effects = &tabs.pane_effects;
    let now = effects.clock.take().unwrap_or(now);
    let Some(tab) = tabs.tabs.get(tabs.active) else { return };
    let grouped = tab.panes.len() > 1;
    // Without a panel, panes are only outlined once there are several.
    let (margin, radius) = match layout.panel {
        true => (layout.px(MARGIN), layout.radius(PANEL_RADIUS).round()),
        false => (0., 0.),
    };
    let cards: Vec<_> = tabs
        .pane_rects()
        .into_iter()
        .map(|(index, rect)| (index, effects.frame(tab.panes[index].id, rect, now).inset(margin)))
        .collect();
    // Panes no longer shown, under the others until they are covered.
    let leaving: Vec<_> = effects
        .leaving(now)
        .into_iter()
        .filter_map(|(id, frame)| Some((tab.position(id)?, frame.inset(margin))))
        .collect();

    if layout.panel {
        let holes: Vec<_> = leaving.iter().chain(&cards).map(|&(_, card)| card).collect();
        for rect in around(tabs.area(), &holes) {
            layer.rect(rect, palette.chrome);
        }
    }
    let hovered = match tabs.hover {
        // A lone pane's icon is its tab's badge, which does not point at the pane.
        Hit::Icon(tab, pane) if tab == tabs.active && grouped => Some(pane),
        _ => None,
    };
    let line = layout.px(1.).max(1.);
    let widths = (line, layout.px(2.).round().max(line));
    // The focused pane is drawn last, over the others that may slide under it.
    let focused = cards.iter().find(|&&(index, _)| index == tab.focus).map(|&(_, card)| card);
    let (front, back): (Vec<_>, Vec<_>) = cards.iter().partition(|&&(index, _)| index == tab.focus);
    // The pane being dragged is veiled, as lifted out of its place.
    let dragged =
        tabs.drag.as_ref().filter(|drag| drag.moving).and_then(|drag| match drag.source {
            Source::Pane(from, pane) if from == tabs.active => Some(pane),
            _ => None,
        });
    for &(index, card) in leaving.iter().chain(back).chain(front) {
        let lit = grouped && (index == tab.focus || hovered == Some(index));
        let (light, tint) = effects.light(tab.panes[index].id, lit, hovered == Some(index), now);
        if !layout.panel && !grouped {
            continue;
        }
        if tint > 0. {
            layer.rounded_alpha(card, radius, palette.highlight, TINT * tint);
        }
        if dragged == Some(index) {
            layer.rounded_alpha(card, radius, palette.chrome, VEIL);
        }
        let above = focused.filter(|_| index != tab.focus);
        border(layer, card, radius, palette, light, widths, above);
    }

    // A handle on each shown pane's top edge, while there are several.
    if cards.len() > 1 {
        for &(index, card) in &cards {
            let level = effects.handle(tab.panes[index].id, tabs.handle == Some(index), now);
            draw_handle(layer, layout, palette, card, tabs.area().y, level);
        }
    }
    divider::draw(tabs, layout, palette, layer, now);

    // Where the dragged pane would go, beside one of the panes, once it has left its place.
    let target = tabs.drag.as_ref().filter(|drag| drag.moving).and_then(|drag| match drag.target {
        Some(Target::Dock { pane, axis, side }) => {
            let moving = match drag.source {
                Source::Pane(from, index) => Split::Pane(tabs.tabs.get(from)?.panes[index].id),
                Source::Tab(from) => tabs.tabs.get(from)?.layout.clone(),
            };
            let beside = tab.panes.get(pane)?.id;
            let (area, spacing) = (tabs.area(), tabs.spacing());
            let rect = docked(&tab.layout, moving, beside, (axis, side), area, spacing)?;
            Some(rect.inset(margin + layout.px(MARGIN / 2.)))
        },
        _ => None,
    });
    if let Some((rect, alpha)) = effects.preview(target, now) {
        let radius = layout.radius(PANEL_RADIUS);
        layer.rounded_alpha(rect, radius, palette.highlight, DROP * alpha);
        layer.rounded_alpha(rect.inset(-line), radius + line, palette.highlight, DROP * alpha / 2.);
    }
}

/// Draw the drag handle on the top edge of `card`, below `top`, lit by `level` as it is hovered:
/// a notch of chrome holding three dots.
fn draw_handle(
    layer: &mut Layer,
    layout: &Layout,
    palette: &Palette,
    card: Rect,
    top: f32,
    level: f32,
) {
    let (plate, _) = handle_rects(card, top, layout.scale);
    let fill = mix(palette.chrome, palette.hover, level);
    layer.rounded(plate, layout.curve(plate.h / 2.), fill);
    let dots = mix(mix(fill, palette.muted, 0.7), palette.foreground, level);
    let (x, y) = plate.center();
    let (radius, step) = (layout.px(DOT) / 2., layout.px(DOT_STEP));
    for offset in [-step, 0., step] {
        layout.dot(layer, (x + offset, y), radius, dots);
    }
}

/// The border of a pane's `card`, from the panel's outline to the accent and from the `widths`'
/// first to second as it is `light`, hidden where the card is under `above`.
fn border(
    layer: &mut Layer,
    card: Rect,
    radius: f32,
    palette: &Palette,
    light: f32,
    (thin, thick): (f32, f32),
    above: Option<Rect>,
) {
    let color = mix(palette.card_outline, palette.highlight, light);
    // A lit border grows by whole pixels.
    let width = if light > 0. { (thin + (thick - thin) * light).round() } else { thin };
    if above.is_none_or(|above| !overlap(card, above)) {
        layer.corners(card, radius, palette.chrome, color, width);
    }
    let (w, h) = (card.w - 2. * radius, card.h - 2. * radius);
    let edges = [
        Rect::new(card.x + radius, card.y, w, width),
        Rect::new(card.x + radius, card.bottom() - width, w, width),
        Rect::new(card.x, card.y + radius, width, h),
        Rect::new(card.right() - width, card.y + radius, width, h),
    ];
    let above: Vec<_> = above.into_iter().collect();
    for edge in edges {
        for part in around(edge, &above) {
            layer.rect(part, color);
        }
    }
}

fn overlap(a: Rect, b: Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

/// The smallest rectangle holding both `a` and `b`.
fn union(a: Rect, b: Rect) -> Rect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Rect::new(x, y, a.right().max(b.right()) - x, a.bottom().max(b.bottom()) - y)
}

/// Where a pane placed at `rect` starts popping in from: smaller, about the same center.
fn popped(rect: Rect) -> Rect {
    let (w, h) = (rect.w * POP, rect.h * POP);
    Rect::new(rect.x + (rect.w - w) / 2., rect.y + (rect.h - h) / 2., w, h)
}

/// Where the panes of `moving` are placed in `area` once docked beside the pane `beside` of
/// `layout`, on `side` along `axis`, having left their places in it first, as a drop does.
fn docked<P: Copy + PartialEq>(
    layout: &Split<P>,
    moving: Split<P>,
    beside: P,
    (axis, side): (Axis, Side),
    area: Rect,
    spacing: f32,
) -> Option<Rect> {
    let ids = moving.panes();
    let mut rest = ids.iter().try_fold(layout.clone(), |rest, &id| rest.remove(id))?;
    if !rest.insert(beside, moving, axis, side) {
        return None;
    }
    let rects = rest.rects(area, spacing).into_iter().filter(|(id, _)| ids.contains(id));
    rects.map(|(_, rect)| rect).reduce(union)
}

/// Rectangles covering `area` wherever none of `holes` is, in bands from the top.
fn around(area: Rect, holes: &[Rect]) -> Vec<Rect> {
    let clamp = |y: f32| y.clamp(area.y, area.bottom());
    let mut edges: Vec<f32> =
        holes.iter().flat_map(|hole| [hole.y, hole.bottom()]).map(clamp).collect();
    edges.extend([area.y, area.bottom()]);
    edges.sort_by(f32::total_cmp);
    edges.dedup();

    let mut rects = Vec::new();
    for band in edges.windows(2) {
        let (top, bottom) = (band[0], band[1]);
        let mut spans: Vec<_> = holes
            .iter()
            .filter(|hole| hole.y <= top && hole.bottom() >= bottom)
            .map(|hole| (hole.x, hole.right()))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut x = area.x;
        for (left, right) in spans.into_iter().chain([(area.right(), area.right())]) {
            if left.min(area.right()) > x {
                rects.push(Rect::new(x, top, left.min(area.right()) - x, bottom - top));
            }
            x = x.max(right);
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::window_context::tabs::anim::scale;

    fn id(id: u64) -> WindowId {
        WindowId::from(id)
    }

    /// Instants `ms` milliseconds of animation, as slowed down, after `now`.
    fn after(now: Instant, ms: u64) -> Instant {
        now + Duration::from_millis(ms).mul_f32(scale())
    }

    const LEFT: Rect = Rect { x: 0., y: 40., w: 400., h: 600. };
    const RIGHT: Rect = Rect { x: 400., y: 40., w: 400., h: 600. };
    const WHOLE: Rect = Rect { x: 0., y: 40., w: 800., h: 600. };

    #[test]
    fn popped_is_smaller_about_the_same_center() {
        let rect = popped(Rect::new(100., 50., 200., 100.));
        assert_eq!(rect, Rect::new(115., 57.5, 170., 85.));
        assert_eq!(rect.center(), Rect::new(100., 50., 200., 100.).center());
    }

    #[test]
    fn split_slides_and_pops_in() {
        let now = Instant::now();
        let mut effects = Effects::default();
        effects.place(&[(id(1), WHOLE)], now, true);
        // The first placement is not animated.
        assert_eq!(effects.frame(id(1), WHOLE, now), WHOLE);
        assert!(!effects.running(now));

        effects.place(&[(id(1), LEFT), (id(2), RIGHT)], now, true);
        assert_eq!(effects.running(now), scale() > 0.);
        if scale() > 0. {
            assert_eq!(effects.frame(id(1), LEFT, now), WHOLE);
            assert_eq!(effects.frame(id(2), RIGHT, now), popped(RIGHT));
            let middle = effects.frame(id(1), LEFT, after(now, MOVE_MS / 2));
            assert!(middle.w < WHOLE.w && middle.w > LEFT.w * 0.9, "{middle:?}");
        }
        let end = after(now, MOVE_MS);
        assert_eq!(effects.frame(id(1), LEFT, end), LEFT);
        assert_eq!(effects.frame(id(2), RIGHT, end), RIGHT);
        assert!(!effects.running(end));
    }

    #[test]
    fn closing_slides_the_neighbour_over() {
        let now = Instant::now();
        let mut effects = Effects::default();
        effects.place(&[(id(1), LEFT), (id(2), RIGHT)], now, true);
        effects.place(&[(id(2), WHOLE)], now, true);
        assert_eq!(effects.frame(id(2), WHOLE, now), if scale() > 0. { RIGHT } else { WHOLE });
        assert_eq!(effects.frame(id(1), LEFT, now), LEFT, "closed panes are forgotten");
    }

    #[test]
    fn other_tabs_and_resizes_jump_into_place() {
        let now = Instant::now();
        let mut effects = Effects::default();
        effects.place(&[(id(1), LEFT), (id(2), RIGHT)], now, true);
        // Another tab's panes.
        effects.place(&[(id(3), LEFT), (id(4), RIGHT)], now, true);
        assert!(!effects.running(now));
        // The window grows.
        let wider = Rect { w: 500., ..RIGHT };
        effects.place(&[(id(3), LEFT), (id(4), wider)], now, true);
        assert_eq!(effects.frame(id(4), wider, now), wider);
        assert!(!effects.running(now));
    }

    #[test]
    fn docking_previews_where_the_panes_end_up() {
        let pair = Split::Pane(1).join(Split::Pane(2), Axis::Row, Side::After);
        let below = (Axis::Column, Side::After);
        // A pane from elsewhere takes half of the one it docks beside.
        assert_eq!(
            docked(&pair, Split::Pane(3), 2, below, WHOLE, 0.),
            Some(Rect::new(400., 340., 400., 300.))
        );
        // A pane of the group leaves its place first: its neighbour has the whole area to share.
        assert_eq!(
            docked(&pair, Split::Pane(1), 2, below, WHOLE, 0.),
            Some(Rect { y: 340., h: 300., ..WHOLE })
        );
        assert_eq!(
            docked(&pair, Split::Pane(1), 2, (Axis::Row, Side::After), WHOLE, 0.),
            Some(RIGHT)
        );
        // A group docks as a whole, and only beside a pane that stays.
        let group = Split::Pane(3).join(Split::Pane(4), Axis::Column, Side::After);
        assert_eq!(
            docked(&pair, group, 1, (Axis::Row, Side::Before), WHOLE, 0.),
            Some(Rect { w: 200., ..WHOLE })
        );
        assert_eq!(docked(&pair, Split::Pane(1), 1, below, WHOLE, 0.), None);
        assert_eq!(docked(&Split::Pane(1), Split::Pane(1), 1, below, WHOLE, 0.), None);
    }

    #[test]
    fn faded_preview_pops_in_afresh() {
        let now = Instant::now();
        let effects = Effects::default();
        effects.preview(Some(LEFT), now);
        let end = after(now, DROP_MS);
        effects.preview(None, end);
        let later = after(end, DROP_MS) + Duration::from_millis(1);
        assert_eq!(effects.preview(None, later), None);
        effects.preview(Some(RIGHT), later);
        let (rect, _) = effects.drop.get().unwrap();
        assert_eq!(rect.get(later), if scale() > 0. { popped(RIGHT) } else { RIGHT });
    }

    #[test]
    fn overlaps() {
        assert!(overlap(LEFT, Rect { x: 399., ..RIGHT }));
        assert!(!overlap(LEFT, RIGHT), "touching is not overlapping");
    }

    /// Covered area, by counting whole pixels.
    fn covered(area: Rect, rects: &[Rect]) -> usize {
        let mut count = 0;
        for y in area.y as i32..area.bottom() as i32 {
            for x in area.x as i32..area.right() as i32 {
                let inside = |r: &Rect| {
                    (x as f32) >= r.x
                        && (x as f32) < r.right()
                        && (y as f32) >= r.y
                        && (y as f32) < r.bottom()
                };
                let n = rects.iter().filter(|r| inside(r)).count();
                assert!(n <= 1, "({x}, {y}) covered twice");
                count += n;
            }
        }
        count
    }

    #[test]
    fn around_covers_what_holes_leave() {
        let area = Rect::new(0., 10., 100., 60.);
        // One card inset by the margin: the frame around it.
        let card = area.inset(8.);
        let rects = around(area, &[card]);
        assert_eq!(rects.len(), 4);
        assert_eq!(covered(area, &rects), 100 * 60 - 84 * 44);

        // Overlapping, partly outside and empty holes.
        let holes = [
            Rect::new(10., 20., 30., 30.),
            Rect::new(30., 30., 30., 30.),
            Rect::new(90., 0., 40., 20.),
            Rect::new(50., 15., 0., 0.),
        ];
        let rects = around(area, &holes);
        let holes_area = 30 * 30 * 2 - 10 * 20 + 10 * 10;
        assert_eq!(covered(area, &rects), 100 * 60 - holes_area);
        assert_eq!(around(area, &[]), vec![area]);
    }

    #[test]
    fn zoom_keeps_leaving_panes_until_covered() {
        let now = Instant::now();
        let mut effects = Effects::default();
        effects.place(&[(id(1), LEFT), (id(2), RIGHT)], now, true);
        // Zooming the first pane: it grows over the second, which stays drawn until then.
        effects.place(&[(id(1), WHOLE)], now, true);
        if scale() > 0. {
            assert_eq!(effects.leaving(now), vec![(id(2), RIGHT)]);
            assert!(effects.running(after(now, MOVE_MS) - Duration::from_millis(1)));
            // Placing again before then keeps the moment it is covered.
            let later = after(now, MOVE_MS / 2);
            effects.place(&[(id(1), WHOLE)], later, true);
            assert_eq!(effects.leaving(later), vec![(id(2), RIGHT)]);
        }
        let end = after(now, MOVE_MS);
        assert_eq!(effects.leaving(end), vec![]);
        assert!(!effects.running(end));

        // Unzooming: the second pane pops in again.
        effects.place(&[(id(1), LEFT), (id(2), RIGHT)], end, true);
        let popping = if scale() > 0. { popped(RIGHT) } else { RIGHT };
        assert_eq!(effects.frame(id(2), RIGHT, end), popping);
        assert_eq!(effects.leaving(end), vec![]);
    }

    #[test]
    fn jumps_leave_nothing_behind() {
        let now = Instant::now();
        let mut effects = Effects::default();
        effects.place(&[(id(1), LEFT), (id(2), RIGHT)], now, true);
        effects.place(&[(id(1), WHOLE)], now, false);
        assert_eq!(effects.leaving(now), vec![]);
    }

    #[test]
    fn handles_sit_on_the_top_edge() {
        let card = Rect::new(100., 48., 300., 200.);
        let (plate, zone) = handle_rects(card, 40., 1.);
        assert_eq!(plate, Rect::new(234., 43., 32., 10.));
        assert_eq!(zone, Rect::new(226., 40., 48., 17.));
        assert_eq!(plate.center(), (250., 48.));
        // At twice the scale.
        let (plate, zone) = handle_rects(Rect::new(0., 96., 600., 400.), 60., 2.);
        assert_eq!((plate.w, plate.h, zone.w, zone.h), (64., 20., 96., 36.));
        // A card without a margin keeps its handle below the bar.
        let (plate, zone) = handle_rects(Rect::new(0., 40., 300., 200.), 40., 1.);
        assert_eq!((plate.y, zone.y), (40., 40.));
    }

    #[test]
    fn handles_grab_within_their_zone() {
        let cards = [(0, Rect::new(8., 48., 384., 584.)), (1, Rect::new(408., 48., 384., 584.))];
        let at = |x: f64, y: f64| grabbed(&cards, 40., PhysicalPosition::new(x, y), 1.);
        assert_eq!(at(200., 48.), Some(0));
        assert_eq!(at(600., 56.), Some(1));
        assert_eq!(at(600. + 23., 48.), Some(1));
        assert_eq!(at(600. + 25., 48.), None);
        assert_eq!(at(600., 58.), None);
        assert_eq!(at(600., 39.), None, "the bar keeps the pointer");
        assert_eq!(at(400., 48.), None);
    }

    #[test]
    fn cards_are_the_gap_apart() {
        let layout = Split::Pane(1).join(Split::Pane(2), Axis::Row, Side::After);
        for (gap, scale) in [(0., 1.), (16., 1.), (48., 1.), (16., 2.), (48., 1.5)] {
            let tabs = Tabs { panel: true, gap, scale, ..Tabs::default() };
            let rects = layout.rects(WHOLE, tabs.spacing());
            let (left, right) = (tabs.card(rects[0].1), tabs.card(rects[1].1));
            assert_eq!(
                right.x - left.right(),
                (gap * scale as f32).round(),
                "gap {gap} at {scale}"
            );
            // The window's edges keep their margin.
            assert_eq!(left.x, (MARGIN * scale as f32).round());
        }
        let tabs = Tabs { panel: false, gap: 48., scale: 1., ..Tabs::default() };
        assert_eq!(tabs.spacing(), 0.);
    }
}

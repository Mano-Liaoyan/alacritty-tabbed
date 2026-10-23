//! The chrome around the panes of the active tab: each sits in a panel of its own, set in from
//! its part of the window by the margin, so neighbouring panels are as far apart as the window's
//! edges. The focused pane of a grouped tab has a lit border, like Hyprland's active window.
//!
//! Panes slide to their new places and new ones pop in; only where they are drawn moves, their
//! terminals are resized once.

use std::cell::Cell;
use std::time::Instant;

use winit::window::WindowId;

use super::Tabs;
use super::anim::{Curve, Tween};
use super::bar::{Hit, Layout, MARGIN, PANEL_RADIUS};
use super::dnd::Target;
use super::paint::{Layer, Palette, Rect, mix};
use super::split::{Axis, Side};

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

/// Where the panes are drawn while they move to their places, and how lit their borders are.
#[derive(Default)]
pub struct Effects {
    /// The panes of the active tab.
    panes: Vec<Motion>,
    /// What the panes were last placed in.
    area: Option<Rect>,
    /// Where a dragged pane would drop, and how visible that is; updated as it is drawn.
    drop: Cell<Option<(Tween<Rect>, Tween<f32>)>>,
}

struct Motion {
    id: WindowId,
    frame: Tween<Rect>,
    /// How lit the border is, from 0 to 1, and how tinted the pane; updated as it is drawn.
    light: Cell<Tween<f32>>,
    tint: Cell<Tween<f32>>,
}

impl Effects {
    /// Follow the panes of the active tab to their new places `rects`, at `now`.
    ///
    /// Panes that were shown slide to their place, new ones pop in beside them; everything
    /// jumps into place when the window changes size or another tab's panes are shown.
    pub fn place(&mut self, rects: &[(WindowId, Rect)], now: Instant) {
        let area = rects.iter().map(|&(_, rect)| rect).reduce(union);
        let shown = rects.iter().any(|&(id, _)| self.motion(id).is_some());
        let animate = shown && area == self.area;
        self.area = area;

        let mut old = std::mem::take(&mut self.panes);
        self.panes = rects
            .iter()
            .map(|&(id, rect)| {
                let motion = old.iter().position(|motion| motion.id == id).map(|i| old.remove(i));
                let mut motion = motion.unwrap_or_else(|| Motion {
                    id,
                    frame: Tween::new(if animate { popped(rect) } else { rect }),
                    light: Cell::new(Tween::new(0.)),
                    tint: Cell::new(Tween::new(0.)),
                });
                if animate {
                    motion.frame.to(rect, now, MOVE_MS, Curve::SNAP);
                } else {
                    motion.frame = Tween::new(rect);
                }
                motion
            })
            .collect();
    }

    /// Where to draw the pane `id`, whose place is `rect`, at `now`.
    pub fn frame(&self, id: WindowId, rect: Rect, now: Instant) -> Rect {
        self.motion(id).map_or(rect, |motion| motion.frame.get(now))
    }

    /// Whether anything is still moving at `now`.
    pub fn running(&self, now: Instant) -> bool {
        let drop =
            self.drop.get().is_some_and(|(rect, alpha)| rect.running(now) || alpha.running(now));
        drop || self.panes.iter().any(|motion| {
            motion.frame.running(now)
                || motion.light.get().running(now)
                || motion.tint.get().running(now)
        })
    }

    fn motion(&self, id: WindowId) -> Option<&Motion> {
        self.panes.iter().find(|motion| motion.id == id)
    }

    /// How lit the border of the pane `id` is at `now`, and how tinted the pane, moving to
    /// whether it is `lit` and `tinted`.
    fn light(&self, id: WindowId, lit: bool, tinted: bool, now: Instant) -> (f32, f32) {
        let Some(motion) = self.motion(id) else { return (0., 0.) };
        let level = |cell: &Cell<Tween<f32>>, on: bool| {
            let mut tween = cell.get();
            tween.to(if on { 1. } else { 0. }, now, LIGHT_MS, Curve::EASE_OUT);
            cell.set(tween);
            tween.get(now)
        };
        (level(&motion.light, lit), level(&motion.tint, tinted))
    }

    /// Where to draw the drop preview at `now`, and how visible it is, moving to `target`.
    fn preview(&self, target: Option<Rect>, now: Instant) -> Option<(Rect, f32)> {
        let mut drop = self.drop.get();
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

/// Draw the panel of each pane of the active tab, with the chrome around it, the focused pane's
/// border lit when the tab has several, and where a dragged pane would drop.
pub fn draw(tabs: &Tabs, layout: &Layout, palette: &Palette, layer: &mut Layer, now: Instant) {
    let Some(tab) = tabs.tabs.get(tabs.active) else { return };
    let effects = &tabs.pane_effects;
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

    if layout.panel {
        let holes: Vec<_> = cards.iter().map(|&(_, card)| card).collect();
        for rect in around(tabs.area(), &holes) {
            layer.rect(rect, palette.chrome);
        }
    }
    let hovered = match tabs.hover {
        Hit::Icon(tab, pane) if tab == tabs.active => Some(pane),
        _ => None,
    };
    let line = layout.px(1.).max(1.);
    for &(index, card) in &cards {
        let lit = grouped && (index == tab.focus || hovered == Some(index));
        let (light, tint) = effects.light(tab.panes[index].id, lit, hovered == Some(index), now);
        if !layout.panel && !grouped {
            continue;
        }
        if tint > 0. {
            layer.rounded_alpha(card, radius, palette.primary, TINT * tint);
        }
        border(layer, card, radius, palette, light, (line, layout.px(2.).round().max(line)));
    }

    // Where the dragged pane would go, beside one of the panes.
    let target = tabs.drag.as_ref().filter(|drag| drag.moving).and_then(|drag| match drag.target {
        Some(Target::Dock { pane, axis, side }) => {
            let card = cards.iter().find(|&&(index, _)| index == pane)?.1;
            Some(half(card, axis, side).inset(layout.px(MARGIN / 2.)))
        },
        _ => None,
    });
    if let Some((rect, alpha)) = effects.preview(target, now) {
        let radius = layout.radius(PANEL_RADIUS);
        layer.rounded_alpha(rect, radius, palette.primary, DROP * alpha);
        layer.rounded_alpha(rect.inset(-line), radius + line, palette.primary, DROP * alpha / 2.);
    }
}

/// The border of a pane's `card`, from the panel's outline to the accent and from the `widths`'
/// first to second as it is `light`, with a glow around it.
fn border(
    layer: &mut Layer,
    card: Rect,
    radius: f32,
    palette: &Palette,
    light: f32,
    widths: (f32, f32),
) {
    let color = mix(palette.card_outline, palette.primary, light);
    let width = (widths.0 + (widths.1 - widths.0) * light).round();
    let (w, h) = (card.w - 2. * radius, card.h - 2. * radius);
    layer.corners(card, radius, palette.chrome, color, width);
    for (rect, alpha) in
        [(card, 1.), (card.inset(-width), 0.3 * light), (card.inset(-2. * width), 0.12 * light)]
    {
        if alpha <= 0. {
            continue;
        }
        let edges = [
            Rect::new(rect.x + radius, rect.y, w, width),
            Rect::new(rect.x + radius, rect.bottom() - width, w, width),
            Rect::new(rect.x, rect.y + radius, width, h),
            Rect::new(rect.right() - width, rect.y + radius, width, h),
        ];
        for edge in edges {
            if alpha >= 1. {
                layer.rect(edge, color);
            } else {
                layer.rounded_alpha(edge, 0., palette.primary, alpha);
            }
        }
    }
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

/// The half of `rect` on `side` along `axis`.
fn half(rect: Rect, axis: Axis, side: Side) -> Rect {
    let (w, h) = match axis {
        Axis::Row => (rect.w / 2., rect.h),
        Axis::Column => (rect.w, rect.h / 2.),
    };
    match side {
        Side::Before => Rect::new(rect.x, rect.y, w, h),
        Side::After => Rect::new(rect.right() - w, rect.bottom() - h, w, h),
    }
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
        effects.place(&[(id(1), WHOLE)], now);
        // The first placement is not animated.
        assert_eq!(effects.frame(id(1), WHOLE, now), WHOLE);
        assert!(!effects.running(now));

        effects.place(&[(id(1), LEFT), (id(2), RIGHT)], now);
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
        effects.place(&[(id(1), LEFT), (id(2), RIGHT)], now);
        effects.place(&[(id(2), WHOLE)], now);
        assert_eq!(effects.frame(id(2), WHOLE, now), if scale() > 0. { RIGHT } else { WHOLE });
        assert_eq!(effects.frame(id(1), LEFT, now), LEFT, "closed panes are forgotten");
    }

    #[test]
    fn other_tabs_and_resizes_jump_into_place() {
        let now = Instant::now();
        let mut effects = Effects::default();
        effects.place(&[(id(1), LEFT), (id(2), RIGHT)], now);
        // Another tab's panes.
        effects.place(&[(id(3), LEFT), (id(4), RIGHT)], now);
        assert!(!effects.running(now));
        // The window grows.
        let wider = Rect { w: 500., ..RIGHT };
        effects.place(&[(id(3), LEFT), (id(4), wider)], now);
        assert_eq!(effects.frame(id(4), wider, now), wider);
        assert!(!effects.running(now));
    }

    #[test]
    fn halves() {
        let rect = Rect::new(10., 20., 100., 50.);
        assert_eq!(half(rect, Axis::Row, Side::Before), Rect::new(10., 20., 50., 50.));
        assert_eq!(half(rect, Axis::Row, Side::After), Rect::new(60., 20., 50., 50.));
        assert_eq!(half(rect, Axis::Column, Side::Before), Rect::new(10., 20., 100., 25.));
        assert_eq!(half(rect, Axis::Column, Side::After), Rect::new(10., 45., 100., 25.));
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
}

//! The chrome around the panes of the active tab: each sits in a panel of its own, set in from
//! its part of the window by the margin, so neighbouring panels are as far apart as the window's
//! edges. The focused pane of a grouped tab has a lit border, like Hyprland's active window.
//!
//! Panes slide to their new places and new ones pop in; only where they are drawn moves, their
//! terminals are resized once. Panes no longer shown, as a zoomed pane grows over them, stay
//! drawn under it until it covers them.
//!
//! Each pane of a group has controls at the top of its card, shown as the pointer nears them: a
//! grip, which drags it, and a button that zooms it or back. The live pane's text size shows over
//! it as it changes.

use std::cell::Cell;
use std::mem;
use std::time::{Duration, Instant};

use winit::dpi::PhysicalPosition;
use winit::window::WindowId;

use super::anim::{Curve, Tween, scale};
use super::bar::{BAR, Hit, Layout, MARGIN, PANEL_RADIUS, RADIUS};
use super::dnd::{Source, Target};
use super::paint::{BLACK, Layer, Palette, Rect, UNZOOM_GLYPH, ZOOM_GLYPH, mix};
use super::split::{Axis, Side, Split};
use super::{Press, Tabs, divider};

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
/// Size of a pane's controls, how far below the top of its card, and the width of their grip, in
/// logical pixels.
const CLUSTER: (f32, f32) = (58., 22.);
const CLUSTER_TOP: f32 = 6.;
const GRIP: f32 = 30.;
/// Size of the grip's dots, and the distance between their centres.
const DOT: f32 = 3.;
const DOT_STEP: f32 = 5.;
/// How far beside and below the controls the pointer shows them, in logical pixels.
const REVEAL_SIDE: f32 = 56.;
const REVEAL_BELOW: f32 = 28.;
/// Room the controls need beside them in their card, in logical pixels, and above, in rows.
const CLUSTER_ROOM: f32 = 16.;
const CLUSTER_ROWS: f32 = 3.;
/// How long the controls take to appear, stay once the pointer left them, and fade.
const REVEAL_MS: u64 = 140;
const LINGER_MS: u64 = 350;
const HIDE_MS: u64 = 180;
/// How long a control takes to light up as it is hovered.
const HOVER_MS: u64 = 120;
/// How long the text size takes to show over the pane, stays after the last change, and fades.
const HUD_IN_MS: u64 = 120;
const HUD_HOLD_MS: u64 = 700;
const HUD_OUT_MS: u64 = 240;

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
    /// How visible the live pane's text size is, and until when it stays.
    hud: Cell<Option<(Tween<f32>, Instant)>>,
}

struct Motion {
    id: WindowId,
    frame: Tween<Rect>,
    /// How lit the border is, from 0 to 1, and how tinted the pane; updated as it is drawn.
    light: Cell<Tween<f32>>,
    tint: Cell<Tween<f32>>,
    /// How visible the controls are.
    reveal: Cell<Reveal>,
    /// How lit the grip and the zoom button are.
    hover: Cell<[Tween<f32>; 2]>,
}

/// How visible a pane's controls are: they appear as the pointer nears them, and stay a moment
/// after it left before they fade.
#[derive(Clone, Copy)]
struct Reveal {
    level: Tween<f32>,
    /// When the pointer left them.
    left_at: Option<Instant>,
}

impl Default for Reveal {
    fn default() -> Self {
        Self { level: Tween::new(0.), left_at: None }
    }
}

impl Reveal {
    /// How visible the controls are at `now`, moving to whether the pointer is `near`.
    fn level(&mut self, near: bool, now: Instant) -> f32 {
        if near {
            self.left_at = None;
            self.level.to(1., now, REVEAL_MS, Curve::EASE_OUT);
        } else if self.left_at.is_none() && self.level.get(now) > 0. {
            self.left_at = Some(now);
        }
        if let Some(left) = self.left_at.map(|left| left + linger()) {
            if now >= left {
                self.level.to(0., left, HIDE_MS, Curve::EASE_OUT);
            }
        }
        self.level.get(now)
    }

    fn running(&self, now: Instant) -> bool {
        self.level.running(now) || self.left_at.is_some_and(|left| now < left + linger())
    }
}

/// How long the controls stay once the pointer left them: a wait, not a motion, so it is not
/// slowed down nor skipped with the animations.
fn linger() -> Duration {
    Duration::from_millis(LINGER_MS)
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
                    reveal: Cell::default(),
                    hover: Cell::new([Tween::new(0.); 2]),
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
        let hud = self.hud.get().is_some_and(|(hud, until)| hud.running(now) || now < until);
        drop || leaving
            || hud
            || self.panes.iter().any(|motion| {
                motion.frame.running(now)
                    || motion.light.get().running(now)
                    || motion.tint.get().running(now)
                    || motion.reveal.get().running(now)
                    || motion.hover.get().iter().any(|hover| hover.running(now))
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

    /// How visible the controls of the pane `id` are at `now`, moving to whether they are `near`
    /// the pointer, or at once hidden while they are `quiet`.
    fn reveal(&self, id: WindowId, near: bool, quiet: bool, now: Instant) -> f32 {
        let Some(motion) = self.motion(id) else { return 0. };
        if quiet {
            motion.reveal.set(Reveal::default());
            return 0.;
        }
        let mut reveal = motion.reveal.get();
        let level = reveal.level(near, now);
        motion.reveal.set(reveal);
        level
    }

    /// How lit the grip and the zoom button of the pane `id` are at `now`, moving to whether the
    /// pointer is on `hovered`.
    fn hover(&self, id: WindowId, hovered: Option<Control>, now: Instant) -> [f32; 2] {
        let Some(motion) = self.motion(id) else { return [0.; 2] };
        let mut tweens = motion.hover.get();
        let levels = [Control::Grip, Control::Zoom].map(|control| {
            let tween = &mut tweens[control as usize];
            let target = if hovered == Some(control) { 1. } else { 0. };
            tween.to(target, now, HOVER_MS, Curve::EASE_OUT);
            tween.get(now)
        });
        motion.hover.set(tweens);
        levels
    }

    /// How visible the text size is at `now`, shown while it is `until` then.
    fn hud(&self, until: Instant, now: Instant) -> f32 {
        let mut tween = self.hud.get().map_or(Tween::new(0.), |(tween, _)| tween);
        let (target, ms) = if now < until { (1., HUD_IN_MS) } else { (0., HUD_OUT_MS) };
        tween.to(target, now, ms, Curve::EASE_OUT);
        self.hud.set(Some((tween, until)));
        tween.get(now)
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

/// A control at the top of each shown pane of a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Drags the pane.
    Grip,
    /// Zooms the pane, or unzooms it.
    Zoom,
}

/// The controls of a pane: a capsule holding the grip, then the zoom button.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cluster {
    capsule: Rect,
    grip: Rect,
    zoom: Rect,
}

impl Cluster {
    /// The control at `point`.
    fn at(&self, point: PhysicalPosition<f64>) -> Option<Control> {
        if self.grip.contains(point) {
            Some(Control::Grip)
        } else {
            self.zoom.contains(point).then_some(Control::Zoom)
        }
    }
}

/// The controls of the pane in `card`, at `scale`: centred at its top, a little inside it, so they
/// never sit on its border.
fn cluster(card: Rect, scale: f32) -> Cluster {
    let px = |value: f32| (value * scale).round();
    let (w, h) = (px(CLUSTER.0), px(CLUSTER.1));
    let capsule = Rect::new((card.x + (card.w - w) / 2.).round(), card.y + px(CLUSTER_TOP), w, h);
    let grip = Rect { w: px(GRIP), ..capsule };
    let zoom = Rect::new(grip.right(), capsule.y, w - grip.w, h);
    Cluster { capsule, grip, zoom }
}

/// Where the pointer shows the controls of the pane in `card`, at `scale`: around them, from
/// `reach` above the card, the middle of the gap there, to a couple of rows below them.
fn reveal_zone(card: Rect, reach: f32, scale: f32) -> Rect {
    let capsule = cluster(card, scale).capsule;
    let (side, below) = ((REVEAL_SIDE * scale).round(), (REVEAL_BELOW * scale).round());
    let top = card.y - reach;
    Rect::new(capsule.x - side, top, capsule.w + 2. * side, capsule.bottom() + below - top)
}

/// Whether `card` has room for its controls, at `scale`, with rows of `row` pixels.
fn fits(card: Rect, row: f32, scale: f32) -> bool {
    card.w >= ((CLUSTER.0 + CLUSTER_ROOM) * scale).round() && card.h >= CLUSTER_ROWS * row
}

/// Whether the controls stay hidden: while something is dragged, a selection made, or a menu,
/// a name or the settings edited.
fn quiet(tabs: &Tabs) -> bool {
    tabs.drag.as_ref().is_some_and(|drag| drag.moving)
        || matches!(tabs.press, Press::Divider | Press::Terminal)
        || tabs.menu.is_some()
        || tabs.context.is_some()
        || tabs.rename.is_some()
        || tabs.settings.is_some()
}

/// The cards of the active tab's shown panes that have controls, by index, at `scale`, and the
/// height above which the pointer is not theirs: those of a tab of several panes, a zoomed one too.
fn with_controls(tabs: &Tabs, scale: f32) -> (Vec<(usize, Rect)>, f32) {
    let grouped = tabs.tabs.get(tabs.active).is_some_and(|tab| tab.panes.len() > 1);
    // A bar shown over the panes keeps the pointer from the controls under it.
    let overlay = !tabs.autohide.pinned && tabs.autohide.live();
    let top = if overlay { (BAR * scale).round() } else { tabs.area().y };
    if !grouped || quiet(tabs) {
        return (Vec::new(), top);
    }
    let cards = tabs.pane_rects().into_iter().map(|(index, rect)| (index, tabs.card(rect)));
    (cards.filter(|&(_, card)| fits(card, tabs.cells.1, scale)).collect(), top)
}

/// The pane of the active tab whose control is at `point`, and which control.
pub fn control_at(
    tabs: &Tabs,
    layout: &Layout,
    point: PhysicalPosition<f64>,
) -> Option<(usize, Control)> {
    control(tabs, point, layout.scale)
}

/// The pane of the active tab whose control is at `point`, at `scale`, and which control.
pub fn control(tabs: &Tabs, point: PhysicalPosition<f64>, scale: f32) -> Option<(usize, Control)> {
    let (cards, top) = with_controls(tabs, scale);
    hit(&cards, top, point, scale)
}

/// The pane of `cards`, by index, whose control is at `point`, below `top`, and which control.
fn hit(
    cards: &[(usize, Rect)],
    top: f32,
    point: PhysicalPosition<f64>,
    scale: f32,
) -> Option<(usize, Control)> {
    if (point.y as f32) < top {
        return None;
    }
    cards.iter().find_map(|&(index, card)| Some((index, cluster(card, scale).at(point)?)))
}

/// The pane of the active tab whose controls the pointer at `point` is near enough to show.
pub fn near(tabs: &Tabs, point: PhysicalPosition<f64>) -> Option<usize> {
    let scale = tabs.scale as f32;
    let (cards, top) = with_controls(tabs, scale);
    // The zone stops at the middle of the gap above, where a divider takes the pointer.
    let reach = (tabs.spacing() / 2. + tabs.margin()).max(0.);
    nearby(&cards, top, reach, point, scale)
}

/// The pane of `cards`, by index, whose controls' zone holds `point`, below `top`, reaching
/// `reach` above its card: the one whose card holds it, if the zones of several do.
fn nearby(
    cards: &[(usize, Rect)],
    top: f32,
    reach: f32,
    point: PhysicalPosition<f64>,
    scale: f32,
) -> Option<usize> {
    if (point.y as f32) < top {
        return None;
    }
    let near: Vec<_> =
        cards.iter().filter(|(_, card)| reveal_zone(*card, reach, scale).contains(point)).collect();
    let held = near.iter().find(|(_, card)| card.contains(point)).or(near.first());
    held.map(|(index, _)| *index)
}

/// The radii of the corners of `card`, from the top left to the bottom right, rounded by
/// `panel` unless it sits in a corner of the `window`, whose corners are rounded by `window_radius`
/// if they are `cut`: it follows the window's curve there.
fn corner_radii(card: Rect, window: Rect, cut: bool, panel: f32, window_radius: f32) -> [f32; 4] {
    let near = |a: f32, b: f32| (a - b).abs() <= 1.;
    let (left, right) = (near(card.x, window.x), near(card.right(), window.right()));
    let (top, bottom) = (near(card.y, window.y), near(card.bottom(), window.bottom()));
    [(top, left), (top, right), (bottom, left), (bottom, right)]
        .map(|(a, b)| if cut && a && b { window_radius } else { panel })
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
    let window = Rect::new(0., 0., layout.width, layout.height);
    let radii = |card| corner_radii(card, window, tabs.cut, radius, layout.radius(RADIUS));
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
        let radii = radii(card);
        if tint > 0. {
            layer.rounded_alpha(card, radius, palette.highlight, TINT * tint);
        }
        if dragged == Some(index) {
            layer.rounded_alpha(card, radius, palette.chrome, VEIL);
        }
        let above = focused.filter(|_| index != tab.focus);
        border(layer, card, radii, palette, light, widths, above);
    }

    // The controls of each shown pane, while the tab has several, as the pointer nears them;
    // holding the modifiers that drag a pane shows them all.
    if grouped {
        let quiet = quiet(tabs);
        let rests = tabs.pane_rects();
        let resting =
            rests.into_iter().find(|&(index, _)| index == tab.focus).map(|(_, r)| r.inset(margin));
        for &(index, card) in &cards {
            let id = tab.panes[index].id;
            let near = tabs.near == Some(index) || tabs.pane_drag();
            let near = near && fits(card, tabs.cells.1, layout.scale);
            let shown = effects.reveal(id, near, quiet, now);
            let hovered = tabs.control.filter(|&(pane, _)| pane == index).map(|(_, c)| c);
            let hover = effects.hover(id, hovered, now);
            let cluster = cluster(card, layout.scale);
            if shown <= 0. || index != tab.focus && hidden(cluster.capsule, focused, resting) {
                continue;
            }
            draw_cluster(layer, layout, palette, cluster, shown, hover, tab.zoom);
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

    // The live pane's text size, as it changes.
    if let Some((id, percent, at)) = tabs.font_hud {
        // Like the controls' linger, how long it stays is not an animation.
        let until = at + Duration::from_millis(HUD_IN_MS + HUD_HOLD_MS);
        let shown = effects.hud(until, now);
        let card = cards.iter().find(|&&(index, _)| tab.panes[index].id == id);
        if let Some(&(_, card)) = card.filter(|_| shown > 0.) {
            draw_hud(layer, layout, palette, card, &format!("{percent} %"), shown);
        }
    }
}

/// Draw a pane's `cluster` of controls, `shown` that far, and each lit by its `hover` level: a
/// floating capsule holding the grip's dots, then the button that zooms the pane or, while it is
/// `zoomed`, back.
fn draw_cluster(
    layer: &mut Layer,
    layout: &Layout,
    palette: &Palette,
    cluster: Cluster,
    shown: f32,
    hover: [f32; 2],
    zoomed: bool,
) {
    // It drops into place as it appears.
    let dy = ((shown - 1.) * layout.px(4.)).round();
    let lower = |rect: Rect| Rect { y: rect.y + dy, ..rect };
    let (capsule, line) = (lower(cluster.capsule), layout.px(1.).max(1.));
    let radius = layout.curve(capsule.h / 2.);
    let shadow = Rect { y: capsule.y + line, ..capsule };
    layer.rounded_alpha(shadow, radius, BLACK, 0.18 * shown);
    layer.rounded_alpha(capsule, radius, palette.popup_outline, 0.92 * shown);
    layer.rounded_alpha(capsule.inset(line), radius - line, palette.popup, 0.92 * shown);

    let (grip, zoom) = (lower(cluster.grip), lower(cluster.zoom));
    let (x, y) = (grip.right(), capsule.center().1 - layout.px(6.));
    layer.rect(
        Rect::new(x, y.round(), line, layout.px(12.)),
        mix(palette.popup, palette.separator, shown),
    );
    for (segment, level) in [grip, zoom].into_iter().zip(hover) {
        let lit = segment.inset(layout.px(2.));
        layer.rounded_alpha(lit, layout.radius(9.), palette.hover, level * shown);
    }

    let color = |level| mix(palette.popup, mix(palette.muted, palette.foreground, level), shown);
    let (radius, step) = (layout.px(DOT) / 2., layout.px(DOT_STEP));
    let (x, y) = grip.center();
    for offset in [-step, 0., step] {
        layout.dot(layer, (x + offset, y), radius, color(hover[0]));
    }
    let glyph = if zoomed { UNZOOM_GLYPH } else { ZOOM_GLYPH };
    let width = (1.3 * layout.scale).max(1.);
    layer.icon(zoom.center(), layout.px(5.), width, glyph, color(hover[1]));
}

/// Draw `text`, the live pane's text size, centred over its `card`, `shown` that far.
fn draw_hud(
    layer: &mut Layer,
    layout: &Layout,
    palette: &Palette,
    card: Rect,
    text: &str,
    shown: f32,
) {
    let font = &layout.font;
    let (w, h) = (font.width(text, true) + layout.px(28.), layout.line + layout.px(14.));
    let (x, y) = card.center();
    let capsule = Rect::new((x - w / 2.).round(), (y - h / 2.).round(), w, h);
    let (radius, line) = (layout.curve(h / 2.), layout.px(1.).max(1.));
    layer.rounded_alpha(capsule, radius, palette.popup_outline, 0.92 * shown);
    layer.rounded_alpha(capsule.inset(line), radius - line, palette.popup, 0.92 * shown);
    layer.label(capsule, text, font, mix(palette.popup, palette.foreground, shown), true);
}

/// The border of a pane's `card`, from the panel's outline to the accent and from the `widths`'
/// first to second as it is `light`, hidden where the card is under `above`.
fn border(
    layer: &mut Layer,
    card: Rect,
    radii: [f32; 4],
    palette: &Palette,
    light: f32,
    (thin, thick): (f32, f32),
    above: Option<Rect>,
) {
    let color = mix(palette.card_outline, palette.highlight, light);
    // A lit border grows by whole pixels.
    let width = if light > 0. { (thin + (thick - thin) * light).round() } else { thin };
    if above.is_none_or(|above| !overlap(card, above)) {
        layer.corners(card, radii, palette.chrome, color, width);
    }
    // The straight edges, between the corners' curves.
    let [tl, tr, bl, br] = radii.map(f32::round);
    let edges = [
        Rect::new(card.x + tl, card.y, card.w - tl - tr, width),
        Rect::new(card.x + bl, card.bottom() - width, card.w - bl - br, width),
        Rect::new(card.x, card.y + tl, width, card.h - tl - bl),
        Rect::new(card.right() - width, card.y + tr, width, card.h - tr - br),
    ];
    let above: Vec<_> = above.into_iter().collect();
    for edge in edges {
        for part in around(edge, &above) {
            layer.rect(part, color);
        }
    }
}

/// Whether controls at `rect` are under the focused pane's card while it moves: drawn at
/// `focused`, away from its place `resting`.
fn hidden(rect: Rect, focused: Option<Rect>, resting: Option<Rect>) -> bool {
    focused.is_some_and(|focused| Some(focused) != resting && overlap(rect, focused))
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
    use crate::window_context::tabs::Frame;
    fn id(id: u64) -> WindowId {
        WindowId::from(id)
    }

    /// Instants `ms` milliseconds after `now`.
    fn after(now: Instant, ms: u64) -> Instant {
        now + Duration::from_millis(ms)
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
        assert!(effects.running(now));
        assert_eq!(effects.frame(id(1), LEFT, now), WHOLE);
        assert_eq!(effects.frame(id(2), RIGHT, now), popped(RIGHT));
        let middle = effects.frame(id(1), LEFT, after(now, MOVE_MS / 2));
        assert!(middle.w < WHOLE.w && middle.w > LEFT.w * 0.9, "{middle:?}");
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
        assert_eq!(effects.frame(id(2), WHOLE, now), RIGHT);
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
        assert_eq!(rect.get(later), popped(RIGHT));
    }

    #[test]
    fn handles_hide_only_under_a_moving_focused_pane() {
        let plate = Rect::new(184., 395., 32., 10.);
        let card = Rect::new(8., 48., 384., 350.);
        assert!(!hidden(plate, Some(card), Some(card)), "at rest, in a narrow gap");
        assert!(hidden(plate, Some(card), Some(Rect { h: 300., ..card })));
        assert!(!hidden(plate, Some(Rect { h: 300., ..card }), Some(card)));
        assert!(!hidden(plate, None, None));
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
        assert_eq!(effects.leaving(now), vec![(id(2), RIGHT)]);
        assert!(effects.running(after(now, MOVE_MS) - Duration::from_millis(1)));
        // Placing again before then keeps the moment it is covered.
        let later = after(now, MOVE_MS / 2);
        effects.place(&[(id(1), WHOLE)], later, true);
        assert_eq!(effects.leaving(later), vec![(id(2), RIGHT)]);
        let end = after(now, MOVE_MS);
        assert_eq!(effects.leaving(end), vec![]);
        assert!(!effects.running(end));

        // Unzooming: the second pane pops in again.
        effects.place(&[(id(1), LEFT), (id(2), RIGHT)], end, true);
        assert_eq!(effects.frame(id(2), RIGHT, end), popped(RIGHT));
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
    fn controls_sit_inside_the_top_of_the_card() {
        let card = Rect::new(100., 48., 300., 200.);
        let cluster = cluster(card, 1.);
        assert_eq!(cluster.capsule, Rect::new(221., 54., 58., 22.));
        assert_eq!(cluster.grip, Rect::new(221., 54., 30., 22.));
        assert_eq!(cluster.zoom, Rect::new(251., 54., 28., 22.));
        assert_eq!(cluster.capsule.center().0, card.center().0);
        // At twice the scale.
        let cluster = super::cluster(Rect::new(0., 96., 600., 400.), 2.);
        assert_eq!(cluster.capsule, Rect::new(242., 108., 116., 44.));
        assert_eq!((cluster.grip.w, cluster.zoom.x), (60., 302.));
    }

    #[test]
    fn controls_are_hit_by_part() {
        let cards = [(0, Rect::new(8., 48., 384., 584.)), (1, Rect::new(408., 48., 384., 584.))];
        let at = |x: f64, y: f64| hit(&cards, 40., PhysicalPosition::new(x, y), 1.);
        // The capsule of the second card spans 571..629, its grip 571..601.
        assert_eq!(at(580., 60.), Some((1, Control::Grip)));
        assert_eq!(at(610., 60.), Some((1, Control::Zoom)));
        assert_eq!(at(200., 70.), Some((0, Control::Grip)));
        assert_eq!(at(570., 60.), None);
        assert_eq!(at(629., 60.), None);
        assert_eq!(at(600., 53.), None, "the border above is not a control");
        assert_eq!(at(600., 76.), None);
        // A bar over the panes keeps the pointer.
        assert_eq!(hit(&cards, 80., PhysicalPosition::new(600., 60.), 1.), None);
    }

    #[test]
    fn controls_show_near_them() {
        let cards = [(0, Rect::new(8., 48., 784., 284.)), (1, Rect::new(8., 348., 784., 284.))];
        let zone = reveal_zone(cards[1].1, 8., 1.);
        assert_eq!(zone, Rect::new(315., 340., 170., 64.));
        let cluster = cluster(cards[1].1, 1.).capsule;
        assert!(zone.contains(PhysicalPosition::new(cluster.x.into(), cluster.y.into())));
        assert!(zone.contains(PhysicalPosition::new(cluster.right() as f64 - 1., 375.)));

        let at = |x: f64, y: f64| nearby(&cards, 40., 8., PhysicalPosition::new(x, y), 1.);
        assert_eq!(at(400., 360.), Some(1));
        assert_eq!(at(316., 393.), Some(1));
        assert_eq!(at(314., 360.), None);
        assert_eq!(at(400., 404.), None, "far below them");
        // Up to the middle of the gap above, where the divider takes over.
        assert_eq!(at(400., 340.), Some(1));
        assert_eq!(at(400., 339.), None);
        assert_eq!(at(400., 48.), Some(0));
        assert_eq!(nearby(&cards, 60., 8., PhysicalPosition::new(400., 50.), 1.), None);

        // Zones reaching over a narrow neighbour show the controls of the pane under the pointer.
        let narrow = [(0, Rect::new(0., 48., 100., 300.)), (1, Rect::new(100., 48., 100., 300.))];
        let at = |x: f64| nearby(&narrow, 40., 0., PhysicalPosition::new(x, 60.), 1.);
        assert_eq!((at(90.), at(110.)), (Some(0), Some(1)));
    }

    #[test]
    fn controls_need_room() {
        let row = 17.;
        assert!(fits(Rect::new(0., 0., 74., 51.), row, 1.));
        assert!(!fits(Rect::new(0., 0., 73., 51.), row, 1.));
        assert!(!fits(Rect::new(0., 0., 74., 50.), row, 1.));
        assert!(fits(Rect::new(0., 0., 148., 51.), row, 2.));
        assert!(!fits(Rect::new(0., 0., 147., 51.), row, 2.));
    }

    #[test]
    fn controls_appear_and_linger() {
        let now = Instant::now();
        let ms = |ms: u64| Duration::from_millis(ms);
        let mut reveal = Reveal::default();
        assert_eq!(reveal.level(false, now), 0.);
        assert!(!reveal.running(now));

        reveal.level(true, now);
        let shown = after(now, REVEAL_MS);
        assert_eq!(reveal.level(true, shown), 1.);
        // The pointer leaves: they stay a while, then fade.
        assert_eq!(reveal.level(false, shown), 1.);
        assert!(reveal.running(shown));
        let lingering = shown + ms(LINGER_MS - 1);
        assert_eq!(reveal.level(false, lingering), 1.);
        assert!(reveal.running(lingering));
        let fading = reveal.level(false, after(shown + ms(LINGER_MS), HIDE_MS / 2));
        assert!(fading > 0. && fading < 1., "{fading}");
        let gone = after(shown + ms(LINGER_MS), HIDE_MS) + ms(1);
        assert_eq!(reveal.level(false, gone), 0.);
        assert!(!reveal.running(gone));

        // Coming back while they linger keeps them.
        let mut reveal = Reveal::default();
        reveal.level(true, now);
        reveal.level(false, after(now, REVEAL_MS));
        let back = after(now, REVEAL_MS) + ms(LINGER_MS / 2);
        assert_eq!(reveal.level(true, back), 1.);
        assert_eq!(reveal.level(true, after(back + ms(LINGER_MS), HIDE_MS)), 1.);
    }

    #[test]
    fn corners_follow_the_window_where_they_meet_it() {
        let window = Rect::new(0., 0., 800., 600.);
        let (panel, round) = (10., 18.);
        // Without a panel, cards touch the window: those in its corners follow its curve.
        let left = Rect::new(0., 48., 400., 552.);
        assert_eq!(corner_radii(left, window, true, 0., round), [0., 0., round, 0.]);
        let right = Rect::new(400., 48., 400., 552.);
        assert_eq!(corner_radii(right, window, true, 0., round), [0., 0., 0., round]);
        // Unless the window is not cut, maximized or with square corners.
        assert_eq!(corner_radii(left, window, false, 0., round), [0.; 4]);
        // A bar that is not pinned leaves the top corners to the cards too.
        let whole = Rect::new(0.5, 0., 799., 600.);
        assert_eq!(corner_radii(whole, window, true, 0., round), [round; 4]);
        // With a panel, cards are set in from the window's corners.
        let card = window.inset(8.);
        assert_eq!(corner_radii(card, window, true, panel, round), [panel; 4]);
    }

    #[test]
    fn cards_are_the_gap_apart() {
        let layout = Split::Pane(1).join(Split::Pane(2), Axis::Row, Side::After);
        let frames = [Frame::Normal, Frame::Maximized, Frame::Fullscreen];
        let sizes = [(0., 1.), (16., 1.), (48., 1.), (16., 2.), (48., 1.5)];
        for ((gap, scale), frame) in sizes.into_iter().flat_map(|size| frames.map(|f| (size, f))) {
            let tabs = Tabs { panel: true, gap, scale, frame, ..Tabs::default() };
            let rects = layout.rects(WHOLE, tabs.spacing());
            let (left, right) = (tabs.card(rects[0].1), tabs.card(rects[1].1));
            assert_eq!(
                right.x - left.right(),
                (gap * scale as f32).round(),
                "gap {gap} at {scale} {frame:?}"
            );
            // The window's edges keep their margin.
            assert_eq!(left.x, (MARGIN * scale as f32).round());
        }
        let tabs = Tabs { panel: false, gap: 48., scale: 1., ..Tabs::default() };
        assert_eq!(tabs.spacing(), 0.);
    }
}

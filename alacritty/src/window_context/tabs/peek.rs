//! The card shown when an icon of a pane is hovered: the pane's own title, which a named tab
//! hides, and a live preview of a pane that is not on screen.
//!
//! Like macOS' tooltips, the first card waits for the pointer to rest on an icon; once one has
//! shown, the cards of other icons show at once, until a moment after the last one hid.

use std::mem;
use std::time::{Duration, Instant};

use winit::dpi::PhysicalPosition;
use winit::window::WindowId;

use crate::config::UiConfig;
use crate::display::SizeInfo;
use crate::renderer::Renderer;

use super::anim::{Curve, Tween};
use super::bar::{self, Hit, Layout, MARGIN, TRACK_RADIUS};
use super::paint::{BLACK, Layer, Palette, Rect, mix};
use super::{PaneDraw, Press, Tabs};

/// How long the pointer rests on an icon before its card shows.
const DWELL: Duration = Duration::from_millis(300);
/// How long after a card hid the next one shows at once.
const WARM: Duration = Duration::from_millis(500);
/// How long a card takes to show, and to hide, in milliseconds.
const SHOW_MS: u64 = 160;
const HIDE_MS: u64 = 120;
/// Largest preview of a pane, in logical pixels, and of the pane's own size.
const THUMBNAIL: (f32, f32) = (320., 200.);
const THUMBNAIL_SCALE: f32 = 0.5;
/// Space around the card's content, and between its header and preview, in logical pixels.
const PAD: f32 = 8.;
/// Widest line of text in a card, in logical pixels.
const TEXT_MAX: f32 = 280.;

/// The hovered icon's card, updated as it is drawn.
pub struct State {
    /// The tab and pane whose card is shown, or fading, or that the pointer rests on.
    target: Option<(usize, usize)>,
    /// The icon under the pointer, and since when.
    hovered: Option<(usize, usize)>,
    since: Instant,
    /// Whether the card is up, and how shown it is, from 0 to 1.
    open: bool,
    shown: Tween<f32>,
    /// Until when another icon shows its card at once.
    warm_until: Option<Instant>,
    /// Where the pointer was when a press or menu hid the card of the icon under it; no card
    /// shows until the pointer moves on, though a click brings another icon under it.
    dismissed: Option<PhysicalPosition<f64>>,
    /// When the bar was last clicked, as last seen, to tell a click between two frames.
    clicked: Option<Instant>,
    /// Where the preview of a pane not on screen is drawn, once the card is.
    thumbnail: Option<Thumbnail>,
}

/// The preview's place in its card, and the scale of the pane in it.
#[derive(Clone, Copy)]
struct Thumbnail {
    rect: Rect,
    scale: f32,
    radius: f32,
    /// Width of its hairline.
    line: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            target: None,
            hovered: None,
            since: Instant::now(),
            open: false,
            shown: Tween::new(0.),
            warm_until: None,
            dismissed: None,
            clicked: None,
            thumbnail: None,
        }
    }
}

impl State {
    /// Whether the card waits to show or fades at `now`, which needs another frame.
    pub fn running(&self, now: Instant) -> bool {
        (self.hovered.is_some() && !self.open) || self.shown.running(now)
    }

    /// Whether any of the card is shown at `now`.
    pub fn visible(&self, now: Instant) -> bool {
        self.open || self.shown.get(now) > 0.
    }

    /// Follow the pointer at `pointer` to the icon `hover`, at `now`; none shows while the
    /// pointer is `busy` with a drag, a press or a menu, nor after that until it moves, as after
    /// a click on an icon.
    fn update(
        &mut self,
        hover: Option<(usize, usize)>,
        pointer: PhysicalPosition<f64>,
        busy: bool,
        now: Instant,
    ) {
        if busy || self.dismissed != Some(pointer) {
            self.dismissed = hover.filter(|_| busy).map(|_| pointer);
        }
        let hovered = hover.filter(|_| self.dismissed.is_none());
        if hovered != self.hovered {
            let warm = self.open || self.warm_until.is_some_and(|until| now < until);
            self.hovered = hovered;
            self.since = now;
            match hovered {
                // Moving from one card to the next swaps them at once.
                Some(pane) if warm => {
                    (self.target, self.open, self.shown) = (Some(pane), true, Tween::new(1.));
                },
                Some(_) => (),
                None if self.open => {
                    self.open = false;
                    self.shown.to(0., now, HIDE_MS, Curve::EASE_OUT);
                    self.warm_until = Some(now + Duration::from_millis(HIDE_MS) + WARM);
                },
                None => self.target = None,
            }
        }
        if let Some(pane) = self.hovered.filter(|_| !self.open && now >= self.since + DWELL) {
            (self.target, self.open) = (Some(pane), true);
            self.shown.to(1., now, SHOW_MS, Curve::EASE_OUT);
        }
    }
}

impl Tabs {
    /// The hovered pane that is not on screen, drawn into its card's thumbnail.
    pub fn peek(&self, now: Instant) -> Option<PaneDraw> {
        let thumbnail = self.peeking.thumbnail.filter(|_| self.peeking.visible(now))?;
        if thumbnail.rect.w < 1. || thumbnail.rect.h < 1. {
            return None;
        }
        let session = self.peeked()?.session.as_ref()?;
        // The pane's card fills the thumbnail; its margin is left out.
        let (margin, size, scale) = (self.margin(), session.size, thumbnail.scale);
        let Rect { x, y, .. } = thumbnail.rect;
        let (width, height) = (size.width() * scale, size.height() * scale);
        let frame = Rect::new(x - margin * scale, y - margin * scale, width, height);
        Some(PaneDraw {
            terminal: session.terminal.clone(),
            config: session.config.clone(),
            size,
            frame,
            clip: thumbnail.rect,
        })
    }

    /// Draw over the thumbnail, once its terminal is drawn: its corners and hairline.
    pub fn draw_over(&self, renderer: &mut Renderer, size: &SizeInfo, config: &UiConfig) {
        let (Some(thumbnail), Some(font)) = (self.peeking.thumbnail, &self.font) else { return };
        let palette = Palette::new(&config.colors);
        let Thumbnail { rect, radius, line, .. } = thumbnail;
        let mut edges = Layer::default();
        edges.corners(rect, radius, palette.popup, palette.card_outline, line);
        let r = radius.round();
        for edge in [
            Rect::new(rect.x + r, rect.y, rect.w - 2. * r, line),
            Rect::new(rect.x + r, rect.bottom() - line, rect.w - 2. * r, line),
            Rect::new(rect.x, rect.y + r, line, rect.h - 2. * r),
            Rect::new(rect.right() - line, rect.y + r, line, rect.h - 2. * r),
        ] {
            edges.rect(edge, palette.card_outline);
        }

        let (width, height) = (self.window.width as f32, self.window.height as f32);
        let window = SizeInfo::new(width, height, 1., 1., 0., 0., false);
        Layer::draw(vec![edges], renderer, font, &window);
        renderer.resize(size);
    }

    /// The pane whose card is shown.
    fn peeked(&self) -> Option<&super::Pane> {
        let (tab, pane) = self.peeking.target?;
        self.tabs.get(tab)?.panes.get(pane)
    }

    /// Whether the pane `pane` of the tab at `tab` is drawn in the window.
    fn on_screen(&self, tab: usize, pane: usize) -> bool {
        tab == self.active && self.tabs.get(tab).is_some_and(|t| !t.zoom || t.focus == pane)
    }
}

/// Draw the card of the hovered icon, without the thumbnail's terminal, following the pointer at
/// `now`; `title` is the window's, which the live pane shows.
pub fn draw(
    tabs: &mut Tabs,
    layout: &Layout,
    palette: &Palette,
    title: &str,
    now: Instant,
) -> Option<Layer> {
    let busy = tabs.drag.is_some()
        || tabs.press != Press::None
        || tabs.menu.is_some()
        || tabs.context.is_some()
        || tabs.settings.is_some()
        || tabs.rename.is_some();
    let hover = match tabs.hover {
        Hit::Icon(tab, pane) => Some((tab, pane)),
        _ => None,
    };
    // A click may come and go between two frames.
    let clicked = tabs.last_click.map(|(_, at)| at);
    let click = mem::replace(&mut tabs.peeking.clicked, clicked) != clicked;
    tabs.peeking.update(hover, tabs.pointer, busy || click, now);
    tabs.peeking.thumbnail = None;

    let shown = tabs.peeking.shown.get(now);
    let (index, position) = tabs.peeking.target.filter(|_| shown > 0.)?;
    let pane = tabs.peeked()?;
    let anchor = layout.anchor(index, position)?;
    let title = if (index, position) == tabs.live() { title } else { &pane.title };
    let profile = pane.profile.as_deref().filter(|profile| *profile != title);
    let preview = !tabs.on_screen(index, position) && pane.session.is_some();

    let px = |value: f32| layout.px(value);
    let (font, line) = (&*layout.font, layout.line);
    let (pad, text_max) = (px(PAD), px(TEXT_MAX));
    let widths = (font.width(title, false), profile.map_or(0., |p| font.width(p, false)));
    let margin = px(MARGIN);
    let window = Rect::new(margin, 0., layout.width - 2. * margin, layout.height - margin);
    let gap = px(6.);

    // A preview has a header of the pane's icon, title and profile; a pane on screen only names
    // itself, its profile below.
    let (icon, header) = (px(16.), line.max(px(16.)));
    let text = if preview {
        let profile = if profile.is_some() { pad + widths.1 } else { 0. };
        (icon + px(6.) + widths.0 + profile).min(text_max)
    } else {
        widths.0.max(widths.1).min(text_max)
    };
    let (thumbnail, size) = if preview {
        let session = pane.session.as_ref()?;
        let content = |length: f32| length - 2. * tabs.margin();
        let (w, h) = (content(session.size.width()), content(session.size.height()));
        let room = window.bottom() - anchor.bottom() - gap - 3. * pad - header;
        let max = (px(THUMBNAIL.0), px(THUMBNAIL.1).min(room));
        let scale = fit((w, h), max, session.size.cell_height());
        let thumbnail = ((w * scale).round(), (h * scale).round(), scale);
        let size = (thumbnail.0.max(text) + 2. * pad, header + thumbnail.1 + 3. * pad);
        (Some(thumbnail), size)
    } else {
        let lines = if profile.is_some() { 2. } else { 1. };
        (None, (text + 2. * px(10.), lines * line + 2. * pad))
    };
    let card = card(anchor, size, window, gap);

    // The card grows a little from below its icon as it shows.
    let origin = (anchor.center().0, anchor.bottom());
    let k = 0.96 + 0.04 * shown;
    let at = |x: f32, y: f32| (origin.0 + (x - origin.0) * k, origin.1 + (y - origin.1) * k);
    let place = |rect: Rect| {
        let (x, y) = at(rect.x, rect.y);
        Rect::new(x.round(), y.round(), (rect.w * k).round(), (rect.h * k).round())
    };
    let ink = |color| mix(palette.popup, color, shown);

    let mut layer = Layer::default();
    let rect = place(card);
    let (radius, stroke) = (layout.radius(TRACK_RADIUS), px(1.).max(1.));
    let shadow = Rect::new(rect.x, rect.y + px(2.), rect.w, rect.h);
    layer.rounded_alpha(shadow, radius + layout.radius(2.), BLACK, 0.3 * shown);
    layer.rounded_alpha(rect, radius, palette.popup_outline, shown);
    layer.rounded_alpha(rect.inset(stroke), (radius - stroke).max(0.), palette.popup, shown);

    let fitted = |text: &str, room: f32| font.fit(text, room.max(0.), false);
    match thumbnail {
        Some((w, h, scale)) => {
            let (x, y) = (card.x + pad, card.y + pad);
            let badge = Rect::new(x, y + ((header - icon) / 2.).round(), icon, icon);
            let accent = palette.accent(title);
            let colors = (palette.popup, ink(accent), ink(palette.foreground));
            match &pane.icon {
                Some(image) => layer.image_alpha(place(badge), image, palette.foreground, shown),
                None => {
                    bar::badge(&mut layer, layout, place(badge), None, &pane.badge, font, colors)
                },
            }
            let text_y = y + (header - line) / 2.;
            let left = badge.right() + px(6.);
            let room = card.right() - pad - left;
            let name = fitted(title, room);
            let after = left + font.width(&name, false) + pad;
            let (x, y) = at(left, text_y);
            layer.text(x, y, name, ink(palette.foreground), false);
            if let Some(profile) = profile.filter(|_| card.right() - pad - after > px(24.)) {
                let (x, y) = at(after, text_y);
                let text = fitted(profile, card.right() - pad - after);
                layer.text(x, y, text, ink(palette.muted), false);
            }
            // The terminal cannot fade, so the preview shows only in the card at rest; the empty
            // well around it carries the card in and out.
            let x = card.x + ((card.w - w) / 2.).round();
            let rect = Rect::new(x, card.y + 2. * pad + header, w, h);
            let radius = layout.radius(TRACK_RADIUS - PAD);
            let well = place(rect);
            layer.rounded_alpha(well, radius, palette.card_outline, shown);
            layer.rounded_alpha(
                well.inset(stroke),
                (radius - stroke).max(0.),
                palette.terminal,
                shown,
            );
            if tabs.peeking.open && shown >= 1. {
                let thumbnail = Thumbnail { rect, scale, radius, line: stroke };
                tabs.peeking.thumbnail = Some(thumbnail);
            }
        },
        None => {
            let (x, y) = at(card.x + px(10.), card.y + pad);
            layer.text(x, y, fitted(title, text), ink(palette.foreground), false);
            if let Some(profile) = profile {
                let (x, y) = at(card.x + px(10.), card.y + pad + line);
                layer.text(x, y, fitted(profile, text), ink(palette.muted), false);
            }
        },
    }
    Some(layer)
}

/// Whether the pane `id` is previewed, so its terminal's updates redraw the window.
pub fn shows(tabs: &Tabs, id: WindowId) -> bool {
    tabs.peeking.thumbnail.is_some() && tabs.peeked().is_some_and(|pane| pane.id == id)
}

/// Scale of the preview of a pane `pane` large, with cells `cell` pixels tall, within `max`: its
/// own shape, at most half its size, each line of cells a whole number of pixels tall so the
/// lines are drawn alike, unless they would be too thin to read either way.
fn fit((width, height): (f32, f32), (max_width, max_height): (f32, f32), cell: f32) -> f32 {
    if width <= 0. || height <= 0. {
        return 0.;
    }
    let scale = (max_width / width).min(max_height / height).clamp(0., THUMBNAIL_SCALE);
    let line = cell * scale;
    if line >= 2. { line.floor() / cell } else { scale }
}

/// Where a card of `size` goes below `anchor`, which reaches from the icon down to whatever the
/// icon lies on, `gap` apart and centered on it, inside `window`.
fn card(anchor: Rect, (width, height): (f32, f32), window: Rect, gap: f32) -> Rect {
    let x = (anchor.center().0 - width / 2.).min(window.right() - width).max(window.x);
    Rect::new(x.round(), (anchor.bottom() + gap).round(), width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Rect = Rect { x: 8., y: 0., w: 984., h: 692. };

    const HERE: PhysicalPosition<f64> = PhysicalPosition::new(300., 20.);

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn card_is_centered_below_its_icon() {
        let icon = Rect::new(300., 15., 22., 22.);
        assert_eq!(card(icon, (200., 40.), WINDOW, 6.), Rect::new(211., 43., 200., 40.));
    }

    #[test]
    fn card_stays_inside_the_window() {
        let left = Rect::new(10., 15., 22., 22.);
        assert_eq!(card(left, (200., 40.), WINDOW, 6.).x, WINDOW.x);
        let right = Rect::new(980., 15., 22., 22.);
        assert_eq!(card(right, (200., 40.), WINDOW, 6.).right(), WINDOW.right());
        // Wider than the window, it starts at its left.
        assert_eq!(card(right, (2000., 40.), WINDOW, 6.).x, WINDOW.x);
    }

    #[test]
    fn card_hangs_below_a_shelf() {
        // An icon in the first row of a shelf of three rows, which reaches below the bar.
        let anchor = Rect::new(300., 15., 22., 22. + 2. * 26. + 6.);
        assert_eq!(card(anchor, (200., 40.), WINDOW, 6.).y, anchor.bottom() + 6.);
    }

    /// Size of the preview of a pane `pane` large, with cells 17 pixels tall.
    fn thumbnail(pane: (f32, f32), max: (f32, f32)) -> (f32, f32) {
        let scale = fit(pane, max, 17.);
        ((pane.0 * scale).round(), (pane.1 * scale).round())
    }

    #[test]
    fn thumbnail_keeps_the_pane_shape_at_most_half_its_size() {
        let max = (320., 200.);
        let (w, h) = thumbnail((1000., 600.), max);
        assert!(w <= 320. && h <= 200.);
        assert!((w / h - 1000. / 600.).abs() < 0.02, "{w} {h}");
        // Tall and wide panes fill the height or width, as far as whole lines allow.
        let (w, h) = thumbnail((400., 1600.), max);
        assert!(h <= 200. && h > 175. && (w / h - 0.25).abs() < 0.01, "{w} {h}");
        let (w, h) = thumbnail((3200., 340.), max);
        assert!(w <= 320. && w > 300., "{w} {h}");
        // Small panes show at half their size, never larger.
        assert_eq!(fit((300., 100.), max, 18.), 0.5);
        assert_eq!(thumbnail((0., 300.), max), (0., 0.));
        // Little room below the card shrinks it.
        assert!(thumbnail((1000., 600.), (320., 60.)).1 <= 60.);
    }

    #[test]
    fn thumbnail_lines_are_whole_pixels_tall() {
        for (pane, cell) in [((990., 600.), 17.), ((480., 580.), 20.), ((1800., 1000.), 35.)] {
            let scale = fit(pane, (320., 200.), cell);
            let line = cell * scale;
            assert!(line >= 1. && (line - line.round()).abs() < 1e-4, "{pane:?} {cell} {line}");
        }
        // Too small to read, it keeps its scale.
        assert_eq!(fit((4000., 4000.), (100., 100.), 20.), 0.025);
        assert_eq!(fit((3200., 340.), (320., 200.), 17.), 0.1);
    }

    #[test]
    fn first_card_waits_for_the_pointer_to_rest() {
        let start = Instant::now();
        let mut state = State::default();
        state.update(Some((1, 0)), HERE, false, start);
        assert!(!state.visible(start) && state.running(start));
        state.update(Some((1, 0)), HERE, false, at(start, 299));
        assert!(!state.open);
        state.update(Some((1, 0)), HERE, false, at(start, 300));
        assert!(state.open && state.target == Some((1, 0)));
        // It pops in, then rests.
        assert!(state.running(at(start, 350)));
        assert!(!state.running(at(start, 300 + 1000)));
        assert_eq!(state.shown.get(at(start, 1300)), 1.);
    }

    #[test]
    fn moving_on_restarts_the_wait() {
        let start = Instant::now();
        let mut state = State::default();
        state.update(Some((1, 0)), HERE, false, start);
        state.update(Some((1, 1)), HERE, false, at(start, 200));
        state.update(Some((1, 1)), HERE, false, at(start, 400));
        assert!(!state.open);
        state.update(Some((1, 1)), HERE, false, at(start, 500));
        assert_eq!(state.target, Some((1, 1)));
        // Leaving before the card shows shows none.
        let mut state = State::default();
        state.update(Some((2, 0)), HERE, false, start);
        state.update(None, HERE, false, at(start, 200));
        state.update(None, HERE, false, at(start, 2000));
        assert!(!state.visible(at(start, 2000)) && !state.running(at(start, 2000)));
        assert_eq!(state.target, None);
    }

    #[test]
    fn once_warm_cards_swap_at_once() {
        let start = Instant::now();
        let mut state = State::default();
        state.update(Some((0, 0)), HERE, false, start);
        state.update(Some((0, 0)), HERE, false, at(start, 300));
        // To another icon, the card shows at once.
        state.update(Some((0, 1)), HERE, false, at(start, 400));
        assert_eq!((state.target, state.shown.get(at(start, 400))), (Some((0, 1)), 1.));
        // Hidden, it fades.
        state.update(None, HERE, false, at(start, 500));
        assert!(!state.open && state.running(at(start, 520)));
        assert!(state.shown.get(at(start, 560)) < 1.);
        assert!(!state.visible(at(start, 800)));
        // Shortly after, the next card shows at once too.
        state.update(Some((2, 0)), HERE, false, at(start, 1000));
        assert_eq!((state.target, state.shown.get(at(start, 1000))), (Some((2, 0)), 1.));
        // Long after, it waits again.
        state.update(None, HERE, false, at(start, 1100));
        state.update(Some((3, 0)), HERE, false, at(start, 3000));
        assert!(!state.open);
    }

    #[test]
    fn no_card_while_busy_nor_after_on_the_same_icon() {
        // A drag, press or menu hides a card that is up.
        let start = Instant::now();
        let mut state = State::default();
        state.update(Some((0, 0)), HERE, false, start);
        state.update(Some((0, 0)), HERE, false, at(start, 300));
        state.update(Some((0, 0)), HERE, true, at(start, 400));
        assert!(!state.open && !state.visible(at(start, 1000)));
        // Once it is over, as after a click, no card shows until the pointer moves, though the
        // click brought another icon under it.
        state.update(Some((0, 1)), HERE, false, at(start, 1000));
        state.update(Some((0, 1)), HERE, false, at(start, 2000));
        assert!(!state.open && !state.running(at(start, 2000)));
        let moved = PhysicalPosition::new(301., 20.);
        state.update(Some((0, 1)), moved, false, at(start, 2100));
        state.update(Some((0, 1)), moved, false, at(start, 2400));
        assert_eq!((state.open, state.target), (true, Some((0, 1))));
    }
}

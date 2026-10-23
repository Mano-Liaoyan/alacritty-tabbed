//! The card shown when an icon of a pane is hovered: the pane's own title, which a named tab
//! hides, and a live preview of a pane that is not on screen.
//!
//! Like macOS' tooltips, the first card waits for the pointer to rest on an icon; once one has
//! shown, the cards of other icons show at once, until a moment after the last one hid.
//!
//! The preview is drawn unscaled in a smaller font, so its glyphs are rasterized for their size
//! and stay crisp; a pane too large to show whole at a readable size shows its tail.

use std::mem;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::{Term, TermMode};
use crossfont::Size as FontSize;
use winit::dpi::PhysicalPosition;
use winit::window::WindowId;

use crate::config::UiConfig;
use crate::config::font::Font;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::renderer::Renderer;

use super::anim::{Curve, Tween};
use super::bar::{self, Hit, Layout, MARGIN, TRACK_RADIUS};
use super::paint::{BLACK, Layer, Palette, Rect, mix};
use super::{PaneDraw, Press, Session, Tabs};

/// How long the pointer rests on an icon before its card shows.
const DWELL: Duration = Duration::from_millis(300);
/// How long after a card hid the next one shows at once.
const WARM: Duration = Duration::from_millis(500);
/// How long a card takes to show, and to hide, in milliseconds.
const SHOW_MS: u64 = 160;
const HIDE_MS: u64 = 120;
/// Largest preview of a pane, in logical pixels.
const THUMBNAIL: (f32, f32) = (360., 220.);
/// Smallest text of a preview, in points and relative to the pane's own.
const MIN_PT: f32 = 9.;
const MIN_SHARE: f32 = 0.6;
/// Fades over the edges of a preview where it cuts the pane, in logical pixels.
const FADE_RIGHT: f32 = 24.;
const FADE_TOP: f32 = 12.;
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
    /// The cells of the last preview font the display measured.
    cells: Option<(Font, (f32, f32))>,
}

/// The preview's place in its card, and the cells of the pane it shows.
struct Thumbnail {
    rect: Rect,
    radius: f32,
    /// Width of its hairline.
    line: f32,
    font: Font,
    cells: (f32, f32),
    plan: Plan,
}

/// The part of a pane a preview shows.
#[derive(Debug, PartialEq)]
struct Plan {
    /// The rows of the pane's screen shown, and how many of its columns, from the left.
    rows: Range<usize>,
    cols: usize,
    /// Size of the shown cells, in pixels.
    size: (f32, f32),
    /// Whether columns are left out on the right, or rows at the top.
    cut_right: bool,
    cut_top: bool,
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
            cells: None,
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
    /// The font and configuration of the pane whose preview shows or is about to, for the
    /// display to measure its cells with [`Self::set_peek_cells`] before the bar is drawn.
    pub fn peek_font(&self, now: Instant) -> Option<(Font, Rc<UiConfig>)> {
        let (tab, pane) = match self.hover {
            Hit::Icon(tab, pane) => (tab, pane),
            _ => self.peeking.target.filter(|_| self.peeking.visible(now))?,
        };
        if self.on_screen(tab, pane) {
            return None;
        }
        let session = self.tabs.get(tab)?.panes.get(pane)?.session.as_ref()?;
        Some((self.preview_font(session), session.config.clone()))
    }

    /// Remember the cells of the preview font `font`.
    pub fn set_peek_cells(&mut self, font: Font, cells: (f32, f32)) {
        self.peeking.cells = Some((font, cells));
    }

    /// The hovered pane that is not on screen, drawn into its card's thumbnail.
    pub fn peek(&self, now: Instant) -> Option<PaneDraw> {
        let thumbnail = self.peeking.thumbnail.as_ref().filter(|_| self.peeking.visible(now))?;
        if thumbnail.rect.w < 1. || thumbnail.rect.h < 1. {
            return None;
        }
        let session = self.peeked()?.session.as_ref()?;
        // The pane's whole screen in the preview's cells, its first shown row at the top.
        let (cell_width, cell_height) = thumbnail.cells;
        let width = session.size.columns() as f32 * cell_width;
        let height = session.size.screen_lines() as f32 * cell_height;
        let size = SizeInfo::new(width, height, cell_width, cell_height, 0., 0., false);
        let Rect { x, y, .. } = thumbnail.rect;
        let top = y - thumbnail.plan.rows.start as f32 * cell_height;
        Some(PaneDraw {
            terminal: session.terminal.clone(),
            config: session.config.clone(),
            size,
            frame: Rect::new(x, top, width, height),
            clip: thumbnail.rect,
            font: thumbnail.font.clone(),
        })
    }

    /// Draw over the thumbnail, once its terminal is drawn: fades where it cuts the pane, its
    /// corners and hairline.
    pub fn draw_over(&self, renderer: &mut Renderer, size: &SizeInfo, config: &UiConfig) {
        let (Some(thumbnail), Some(font)) = (&self.peeking.thumbnail, &self.font) else { return };
        let palette = Palette::new(&config.colors);
        let Thumbnail { rect, radius, line, .. } = *thumbnail;
        let mut edges = Layer::default();
        if let Some(session) = self.peeked().and_then(|pane| pane.session.as_ref()) {
            let background = session.config.colors.primary.background;
            let px = |value: f32| (value * self.scale as f32).round();
            let plan = &thumbnail.plan;
            if plan.cut_right {
                let strip = |i| Rect::new(rect.right() - i - 1., rect.y, 1., rect.h);
                fade(&mut edges, strip, px(FADE_RIGHT).min(rect.w), background);
            }
            if plan.cut_top {
                let strip = |i| Rect::new(rect.x, rect.y + i, rect.w, 1.);
                fade(&mut edges, strip, px(FADE_TOP).min(rect.h), background);
            }
        }
        edges.corners(rect, [radius; 4], palette.popup, palette.card_outline, line);
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

    /// The font of the preview of `session`: as large as shows the whole pane in the largest
    /// thumbnail, but readable.
    fn preview_font(&self, session: &Session) -> Font {
        let scale = self.scale as f32;
        let font = session.font(scale);
        let own = font.size().as_px();
        let min = FontSize::new(MIN_PT).scale(scale).as_px().max(MIN_SHARE * own);
        let pane = (session.size.columns(), session.size.screen_lines());
        let cells = (session.size.cell_width(), session.size.cell_height());
        let max = ((THUMBNAIL.0 * scale).round(), (THUMBNAIL.1 * scale).round());
        let px = preview_px(pane, cells, own, max, min);
        if px == own { font } else { font.with_size(FontSize::from_px(px)) }
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
    // The preview waits for the display to measure its font's cells.
    let preview =
        pane.session.as_ref().filter(|_| !tabs.on_screen(index, position)).and_then(|session| {
            let font = tabs.preview_font(session);
            match &tabs.peeking.cells {
                Some((measured, cells)) if *measured == font => Some((session, font, *cells)),
                _ => None,
            }
        });

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
    let text = if preview.is_some() {
        let profile = if profile.is_some() { pad + widths.1 } else { 0. };
        (icon + px(6.) + widths.0 + profile).min(text_max)
    } else {
        widths.0.max(widths.1).min(text_max)
    };
    let (thumbnail, size) = if let Some((session, preview_font, cells)) = preview {
        let pane = (session.size.columns(), session.size.screen_lines());
        let bottom = bottom(&session.terminal.lock());
        let room = window.bottom() - anchor.bottom() - gap - 3. * pad - header;
        let plan = plan(pane, bottom, cells, (px(THUMBNAIL.0), px(THUMBNAIL.1).min(room)));
        let (w, h) = plan.size;
        let size = (w.max(text) + 2. * pad, header + h + 3. * pad);
        (Some((preview_font, cells, plan)), size)
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
        Some((preview_font, cells, plan)) => {
            let (w, h) = plan.size;
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
                let thumbnail =
                    Thumbnail { rect, radius, line: stroke, font: preview_font, cells, plan };
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

/// Size in pixels of the text of the preview of a pane `pane` cells large, whose cells are `cells`
/// large at its own text size `own`: as large as shows the whole pane within `max`, but no smaller
/// than `min` nor larger than its own; whole pixels, so that previews of panes of any size share
/// a few fonts.
fn preview_px(
    (cols, lines): (usize, usize),
    (cell_width, cell_height): (f32, f32),
    own: f32,
    (max_width, max_height): (f32, f32),
    min: f32,
) -> f32 {
    let fit =
        (max_width / (cols as f32 * cell_width)).min(max_height / (lines as f32 * cell_height));
    if fit >= 1. { own } else { (own * fit).floor().max(min.round()).min(own) }
}

/// What a preview with cells `cells` large shows within `max` of a pane `pane` cells large,
/// whose content ends at the row `bottom`: all of it if it fits, else its left columns and the
/// rows up to its content's end, or its top rows if those reach below.
fn plan(
    (cols, lines): (usize, usize),
    bottom: usize,
    (cell_width, cell_height): (f32, f32),
    (max_width, max_height): (f32, f32),
) -> Plan {
    let fits = |room: f32, cell: f32, count: usize| ((room / cell).max(0.) as usize).min(count);
    let (shown_cols, shown_rows) =
        (fits(max_width, cell_width, cols), fits(max_height, cell_height, lines));
    let end = (bottom + 1).max(shown_rows).min(lines);
    let rows = end - shown_rows..end;
    Plan {
        size: (shown_cols as f32 * cell_width, shown_rows as f32 * cell_height),
        cut_right: shown_cols < cols,
        cut_top: rows.start > 0,
        cols: shown_cols,
        rows,
    }
}

/// The last row of the screen of `term` with content: the lower of its cursor's and of the last
/// that is not blank, or the last in the alternate screen, where programs fill the screen.
fn bottom<T>(term: &Term<T>) -> usize {
    let last = term.screen_lines().saturating_sub(1);
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return last;
    }
    let grid = term.grid();
    let offset = grid.display_offset() as i32;
    let row = |line: Line| ((line.0 + offset).max(0) as usize).min(last);
    let cursor = row(grid.cursor.point.line);
    let filled = (0..=last).rev().find(|&row| !grid[Line(row as i32 - offset)].is_clear());
    cursor.max(filled.unwrap_or(0))
}

/// Fade the edge of `layer` into `color` over `width` pixels, from strips 1 pixel wide: `strip(i)`
/// is the strip `i` pixels in from the edge.
fn fade(layer: &mut Layer, strip: impl Fn(f32) -> Rect, width: f32, color: Rgb) {
    for i in 0..width as usize {
        let Rect { x, y, w, h } = strip(i as f32);
        let alpha = 1. - (i as f32 + 0.5) / width;
        layer.fill(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)], color, alpha);
    }
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

    /// Cells of a font `px` pixels large, as monospace fonts tend to be.
    fn cells(px: f32) -> (f32, f32) {
        ((px * 0.6).floor(), (px * 1.2).floor())
    }

    /// Preview font of a pane at `own` pixels within the thumbnail at scale 1, with the floor
    /// of 9 pt (12 px) and 60 %.
    fn font(pane: (usize, usize), own: f32) -> f32 {
        preview_px(pane, cells(own), own, (360., 220.), (12_f32).max(0.6 * own))
    }

    #[test]
    fn small_pane_shows_whole_at_its_own_size() {
        // 30 × 8 cells of 9 × 18 pixels: 270 × 144.
        assert_eq!(font((30, 8), 15.), 15.);
        let shown = plan((30, 8), 2, cells(15.), (360., 220.));
        let whole =
            Plan { rows: 0..8, cols: 30, size: (270., 144.), cut_right: false, cut_top: false };
        assert_eq!(shown, whole);
    }

    #[test]
    fn pane_shrinks_to_fit_in_whole_pixels() {
        // 30 × 10 cells of 9 × 18 pixels is 270 × 180, 80 % of it fits: 12 px.
        assert_eq!(preview_px((30, 10), (9., 18.), 15., (216., 220.), 6.), 12.);
        let font = font((50, 12), 20.);
        assert_eq!(font, font.floor());
        assert!((12. ..20.).contains(&font), "{font}");
        // A pane at a size between whole pixels previews at whole pixels.
        assert_eq!(preview_px((40, 10), (9., 18.), 15.1, (300., 160.), 6.), 12.);
    }

    #[test]
    fn text_never_gets_smaller_than_9_points_or_60_percent() {
        // A huge pane would need tiny text to show whole.
        assert_eq!(font((300, 90), 15.), 12.);
        assert_eq!(font((300, 90), 40.), 24.);
        // At scale 2, 9 pt is 24 px.
        assert_eq!(preview_px((300, 90), cells(30.), 30., (720., 440.), 24.), 24.);
        // A pane already smaller than the floor keeps its own text.
        assert_eq!(font((300, 90), 10.), 10.);
    }

    #[test]
    fn zoomed_pane_previews_relative_to_its_size() {
        // Zoomed to 200 %, its preview is no smaller than 60 % of that.
        assert_eq!(font((100, 30), 30.), 18.);
        assert_eq!(font((100, 30), 15.), 12.);
    }

    #[test]
    fn huge_pane_shows_its_tail() {
        // 200 × 60 cells at 12 px (7 × 14): 51 columns and 15 rows fit.
        let shown = plan((200, 60), 59, cells(12.), (360., 220.));
        assert_eq!((shown.cols, shown.rows.clone()), (51, 45..60));
        assert_eq!(shown.size, (357., 210.));
        assert!(shown.cut_right && shown.cut_top);
    }

    #[test]
    fn tail_ends_at_the_content_bottom() {
        // A busy log with its cursor halfway: the rows up to it.
        let shown = plan((80, 40), 29, (7., 14.), (360., 220.));
        assert_eq!(shown.rows, 15..30);
        assert!(shown.cut_top);
        // A fresh shell at the top shows its top, nothing cut there.
        let shown = plan((80, 40), 0, (7., 14.), (360., 220.));
        assert_eq!(shown.rows, 0..15);
        assert!(!shown.cut_top && shown.cut_right);
        // Narrow enough, no columns are cut.
        let shown = plan((40, 40), 39, (7., 14.), (360., 220.));
        assert_eq!((shown.cols, shown.cut_right, shown.rows), (40, false, 25..40));
    }

    #[test]
    fn little_room_shows_fewer_rows() {
        let shown = plan((80, 24), 23, (7., 14.), (360., 60.));
        assert_eq!((shown.rows, shown.size.1), (20..24, 56.));
        // No room at all shows nothing.
        let shown = plan((80, 24), 23, (7., 14.), (360., -5.));
        assert_eq!((shown.rows, shown.size.1), (24..24, 0.));
    }

    #[test]
    fn content_bottom_is_the_cursor_or_the_last_text() {
        use alacritty_terminal::event::VoidListener;
        use alacritty_terminal::term::Config;
        use alacritty_terminal::term::test::TermSize;
        use alacritty_terminal::vte::ansi::Handler;

        let mut term = Term::new(Config::default(), &TermSize::new(20, 10), VoidListener);
        assert_eq!(bottom(&term), 0);
        for c in "a\n\r\n\r\n\rb".chars() {
            match c {
                '\n' => term.linefeed(),
                '\r' => term.carriage_return(),
                c => term.input(c),
            }
        }
        assert_eq!(bottom(&term), 3);
        // Text below the cursor, as a program moving it up leaves.
        term.goto(1, 0);
        assert_eq!(bottom(&term), 3);
        term.goto(6, 0);
        assert_eq!(bottom(&term), 6);
        // A full-screen program's tail is its last row.
        term.swap_alt();
        assert_eq!(bottom(&term), 9);
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

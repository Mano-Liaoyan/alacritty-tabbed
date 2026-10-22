//! Layout, hit testing and drawing of the tab bar.
//!
//! The bar looks the same on every platform: shapes are anti-aliased into thin rectangles for the
//! rectangle renderer and labels use the terminal's font, so it needs nothing beyond Alacritty's
//! own renderer. Only its proportions follow the platform, see [`STYLE`].

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};

use glutin::surface::Rect as DamageRect;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use winit::dpi::PhysicalPosition;

use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Flags;

use crate::config::UiConfig;
use crate::config::color::Colors;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::content::RenderableCell;
use crate::display::window::Window;
use crate::renderer::rects::RenderRect;
use crate::renderer::{GlyphCache, Renderer};

use super::Tabs;

/// Last entry of the new-tab menu.
const EDIT_PROFILES: &str = "Edit profiles…";

/// Proportions of the platform's own tab bars, in logical pixels.
struct Style {
    height: f32,
    radius: f32,
    /// Put the close button of tabs on their left, as macOS does.
    close_left: bool,
}

#[cfg(target_os = "macos")]
const STYLE: Style = Style { height: 30., radius: 6., close_left: true };
#[cfg(windows)]
const STYLE: Style = Style { height: 36., radius: 5., close_left: false };
#[cfg(not(any(target_os = "macos", windows)))]
const STYLE: Style = Style { height: 36., radius: 8., close_left: false };

/// Height of the bar in pixels, which grows with the font.
pub(super) fn height(scale: f64, cell_height: f32) -> u32 {
    let scale = scale as f32;
    (STYLE.height * scale).max(cell_height + 12. * scale).round() as u32
}

/// What the pointer is over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Hit {
    #[default]
    None,
    Tab(usize),
    Close(usize),
    New,
    Menu,
    Settings,
    Previous,
    Next,
    /// Row of the open menu.
    Item(usize),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Rect {
    pub(super) x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn right(&self) -> f32 {
        self.x + self.w
    }

    fn bottom(&self) -> f32 {
        self.y + self.h
    }

    fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2., self.y + self.h / 2.)
    }

    fn contains(&self, point: PhysicalPosition<f64>) -> bool {
        let (x, y) = (point.x as f32, point.y as f32);
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    fn inset(self, by: f32) -> Self {
        Self::new(self.x + by, self.y + by, self.w - 2. * by, self.h - 2. * by)
    }
}

/// Geometry of the bar and the open menu, in window pixels.
pub(super) struct Layout {
    scale: f32,
    cell: (f32, f32),
    height: f32,
    first: usize,
    /// Distance between the left edges of neighbouring tabs.
    step: f32,
    track: Rect,
    tabs: Vec<(usize, Rect)>,
    buttons: Vec<(Hit, Rect)>,
    /// The menu and the height of its rows.
    menu: Option<(Rect, f32)>,
}

impl Layout {
    pub(super) fn new(tabs: &Tabs, size: &SizeInfo) -> Self {
        let scale = tabs.scale as f32;
        let px = |value: f32| (value * scale).round();
        let cell = (size.cell_width(), size.cell_height());
        let (width, height) = (size.width(), tabs.height as f32);
        let count = tabs.len();

        let margin = px(6.);
        let pad = px(3.);
        let button = height - 2. * px(4.);
        let chevron = px(18.);
        let nav = px(22.);
        let settings = Rect::new(width - margin - button, px(4.), button, button);

        // Tabs share the width the buttons leave, keeping some space to spare.
        let min = px(96.).max(8. * cell.0);
        let available = settings.x - 3. * margin - 2. * pad - button - chevron - px(24.);
        let overflow = count as f32 * min > available;
        let available = (available - if overflow { 2. * nav } else { 0. }).max(min);
        let capacity = ((available / min) as usize).max(1);
        let shown = count.min(capacity);
        let step = (available / shown as f32).min(px(220.)).floor();
        let first = tabs
            .first
            .min(tabs.active)
            .max((tabs.active + 1).saturating_sub(capacity))
            .min(count - shown);

        let track = Rect::new(margin, px(4.), shown as f32 * step + 2. * pad - px(2.), button);
        let tab = |slot: usize| {
            let x = track.x + pad + slot as f32 * step;
            Rect::new(x, track.y + pad, step - px(2.), track.h - 2. * pad)
        };
        let tab_rects = (first..first + shown).map(|index| (index, tab(index - first))).collect();

        let mut buttons = Vec::new();
        let mut x = track.right() + margin;
        if overflow {
            buttons.push((Hit::Previous, Rect::new(x, settings.y, nav, button)));
            buttons.push((Hit::Next, Rect::new(x + nav, settings.y, nav, button)));
            x += 2. * nav;
        }
        let new = Rect::new(x, settings.y, button, button);
        buttons.push((Hit::New, new));
        buttons.push((Hit::Menu, Rect::new(new.right(), settings.y, chevron, button)));
        buttons.push((Hit::Settings, settings));

        // The menu drops from the new-tab button over the terminal.
        let menu = tabs.menu.as_ref().map(|menu| {
            let row = px(30.).max(cell.1 + px(10.));
            let names = menu.profiles.iter().map(|profile| profile.name.width());
            let chars = names.chain([EDIT_PROFILES.width()]).max().unwrap_or(0);
            let w = (chars as f32 * cell.0 + row + px(28.)).max(px(200.)).min(width - 2. * margin);
            let x = new.x.min(width - margin - w).max(margin);
            let rows = menu.profiles.len() + 1;
            (Rect::new(x, height + px(2.), w, rows as f32 * row + 2. * pad), row)
        });

        Self { scale, cell, height, first, step, track, tabs: tab_rects, buttons, menu }
    }

    fn px(&self, value: f32) -> f32 {
        (value * self.scale).round()
    }

    pub(super) fn hit(&self, point: PhysicalPosition<f64>) -> Hit {
        if let Some((menu, row)) = self.menu {
            if !menu.contains(point) {
                return Hit::None;
            }
            let rows = ((menu.h - 2. * self.px(3.)) / row).round() as usize;
            let index = ((point.y as f32 - menu.y - self.px(3.)) / row).max(0.) as usize;
            return Hit::Item(index.min(rows - 1));
        }

        if point.y as f32 >= self.height {
            return Hit::None;
        }
        if let Some(&(hit, _)) = self.buttons.iter().find(|(_, rect)| rect.contains(point)) {
            return hit;
        }
        match self.tabs.iter().find(|(_, rect)| rect.contains(point)) {
            Some(&(index, rect)) if self.close(rect).contains(point) => Hit::Close(index),
            Some(&(index, _)) => Hit::Tab(index),
            None => Hit::None,
        }
    }

    pub(super) fn tab(&self, index: usize) -> Option<Rect> {
        self.tabs.iter().find(|(i, _)| *i == index).map(|(_, rect)| *rect)
    }

    /// The tab slot under the middle of a tab whose left edge is at `left`.
    pub(super) fn drop_target(&self, left: f32) -> Option<usize> {
        let &(first, rect) = self.tabs.first()?;
        let slot = ((left - rect.x) / self.step).round().clamp(0., (self.tabs.len() - 1) as f32);
        Some(first + slot as usize)
    }

    fn close(&self, tab: Rect) -> Rect {
        let size = self.px(20.);
        let x =
            if STYLE.close_left { tab.x + self.px(5.) } else { tab.right() - self.px(5.) - size };
        Rect::new(x, tab.y + (tab.h - size) / 2., size, size)
    }

    fn badge(&self, tab: Rect) -> Rect {
        let size = self.cell.1.max(self.px(16.)).min(tab.h).round();
        let x = if STYLE.close_left {
            self.close(tab).right() + self.px(2.)
        } else {
            tab.x + self.px(8.)
        };
        Rect::new(x, (tab.y + (tab.h - size) / 2.).round(), size, size)
    }
}

/// Colors of the bar, derived from the terminal's colors.
struct Palette {
    chrome: Rgb,
    track: Rgb,
    track_outline: Rgb,
    raised: Rgb,
    raised_outline: Rgb,
    hover: Rgb,
    separator: Rgb,
    outline: Rgb,
    foreground: Rgb,
    muted: Rgb,
    popup: Rgb,
    popup_outline: Rgb,
    accents: [Rgb; 5],
}

fn mix(from: Rgb, to: Rgb, amount: f32) -> Rgb {
    let mix =
        |from: u8, to: u8| (f32::from(from) + (f32::from(to) - f32::from(from)) * amount) as u8;
    Rgb::new(mix(from.r, to.r), mix(from.g, to.g), mix(from.b, to.b))
}

impl Palette {
    fn new(colors: &Colors) -> Self {
        let (black, white) = (Rgb::new(0, 0, 0), Rgb::new(255, 255, 255));
        let terminal = colors.primary.background;
        let text = colors.primary.foreground;
        let luma = 0.2126 * f32::from(terminal.r)
            + 0.7152 * f32::from(terminal.g)
            + 0.0722 * f32::from(terminal.b);
        let light = luma > 127.5;

        // The bar sits slightly behind the terminal, with raised surfaces closer to the light.
        let chrome = mix(terminal, black, if light { 0.06 } else { 0.3 });
        let raised = if light { mix(terminal, white, 0.7) } else { mix(terminal, text, 0.1) };
        let popup = if light { mix(terminal, white, 0.5) } else { mix(chrome, text, 0.07) };
        let normal = &colors.normal;
        Self {
            chrome,
            track: mix(chrome, text, 0.045),
            track_outline: mix(chrome, text, 0.085),
            raised,
            raised_outline: mix(raised, text, if light { 0.14 } else { 0.1 }),
            hover: mix(chrome, text, 0.09),
            separator: mix(chrome, text, 0.16),
            outline: mix(terminal, text, 0.11),
            foreground: mix(chrome, text, 0.94),
            muted: mix(chrome, text, 0.56),
            popup,
            popup_outline: mix(popup, text, 0.14),
            accents: [normal.green, normal.yellow, normal.blue, normal.magenta, normal.cyan],
        }
    }

    /// A stable badge color for the kind of program a tab runs.
    fn accent(&self, title: &str) -> Rgb {
        let title = title.to_lowercase();
        let has = |words: &[&str]| words.iter().any(|word| title.contains(word));
        let [green, yellow, blue, magenta, cyan] = self.accents;
        if has(&["powershell", "pwsh"]) {
            blue
        } else if has(&["wsl", "ubuntu", "debian", "bash", "zsh", "fish", "linux"]) {
            yellow
        } else if has(&["ssh", "@"]) {
            magenta
        } else if has(&["python", "node", "cargo", "npm", "vim", "edit"]) {
            cyan
        } else if has(&["cmd", "command prompt"]) {
            self.muted
        } else {
            green
        }
    }
}

/// Shapes and labels drawn together; later layers cover earlier ones.
#[derive(Default)]
struct Layer {
    rects: Vec<RenderRect>,
    texts: Vec<(f32, f32, String, Rgb, bool)>,
}

impl Layer {
    fn rect(&mut self, rect: Rect, color: Rgb) {
        self.rects.push(RenderRect::new(rect.x, rect.y, rect.w, rect.h, color, 1.));
    }

    /// Fill a convex polygon, anti-aliased with four samples per pixel row.
    fn fill(&mut self, points: &[(f32, f32)], color: Rgb, alpha: f32) {
        let (top, bottom) = points
            .iter()
            .fold((f32::MAX, f32::MIN), |(top, bottom), p| (top.min(p.1), bottom.max(p.1)));
        let mut previous = Vec::new();
        for y in top.floor() as i32..bottom.ceil() as i32 {
            let spans: Vec<_> =
                (0..4).filter_map(|i| span(points, y as f32 + (i as f32 + 0.5) / 4.)).collect();
            let mut row = Vec::new();
            if let Some(&(left, right)) = spans.first() {
                let (outer, inner) = spans.iter().fold(
                    ((left, right), (left, right)),
                    |((ol, or), (il, ir)), &(l, r)| {
                        ((ol.min(l), or.max(r)), (il.max(l), ir.min(r)))
                    },
                );
                // Columns every sample covers fully are drawn as one rectangle.
                let inner =
                    if spans.len() == 4 { (inner.0.ceil(), inner.1.floor()) } else { (0., 0.) };
                let mut x = outer.0.floor();
                while x < outer.1 {
                    if x == inner.0 && inner.1 > inner.0 {
                        row.push((x, inner.1 - x, 1.));
                        x = inner.1;
                        continue;
                    }
                    let covered: f32 =
                        spans.iter().map(|&(l, r)| (r.min(x + 1.) - l.max(x)).max(0.)).sum();
                    if covered > 0.04 {
                        row.push((x, 1., covered / 4.));
                    }
                    x += 1.;
                }
            }

            // Extend the row above when this one is the same.
            if !row.is_empty() && row == previous {
                let start = self.rects.len() - row.len();
                self.rects[start..].iter_mut().for_each(|rect| rect.height += 1.);
            } else {
                let rects = row
                    .iter()
                    .map(|&(x, w, a)| RenderRect::new(x, y as f32, w, 1., color, a * alpha));
                self.rects.extend(rects);
            }
            previous = row;
        }
    }

    fn rounded(&mut self, rect: Rect, radius: f32, color: Rgb) {
        self.rounded_alpha(rect, radius, color, 1.);
    }

    fn rounded_alpha(&mut self, rect: Rect, radius: f32, color: Rgb, alpha: f32) {
        let r = radius.min(rect.w / 2.).min(rect.h / 2.).max(0.);
        let corners = [
            (rect.right() - r, rect.y + r),
            (rect.right() - r, rect.bottom() - r),
            (rect.x + r, rect.bottom() - r),
            (rect.x + r, rect.y + r),
        ];
        let points: Vec<_> = (0..4)
            .flat_map(|corner| {
                let (x, y) = corners[corner];
                (0..=8).map(move |step| {
                    let angle = (corner as f32 - 1. + step as f32 / 8.) * FRAC_PI_2;
                    (x + r * angle.cos(), y + r * angle.sin())
                })
            })
            .collect();
        self.fill(&points, color, alpha);
    }

    fn outlined(&mut self, rect: Rect, radius: f32, color: Rgb, outline: Rgb, width: f32) {
        self.rounded(rect, radius, outline);
        self.rounded(rect.inset(width), radius - width, color);
    }

    fn circle(&mut self, (x, y): (f32, f32), radius: f32, color: Rgb) {
        self.rounded(Rect::new(x - radius, y - radius, 2. * radius, 2. * radius), radius, color);
    }

    fn stroke(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Rgb) {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let scale = width / 2. / dx.hypot(dy).max(f32::EPSILON);
        let (nx, ny) = (-dy * scale, dx * scale);
        let quad = [(from.0 + nx, from.1 + ny), (to.0 + nx, to.1 + ny), (to.0 - nx, to.1 - ny)];
        self.fill(&[quad[0], quad[1], quad[2], (from.0 - nx, from.1 - ny)], color, 1.);
    }

    /// Strokes through the points, relative to `center` in units of `size`.
    fn icon(&mut self, center: (f32, f32), size: f32, width: f32, lines: &[[f32; 4]], color: Rgb) {
        let at = |x: f32, y: f32| (center.0 + x * size, center.1 + y * size);
        for &[x0, y0, x1, y1] in lines {
            self.stroke(at(x0, y0), at(x1, y1), width, color);
        }
    }

    fn gear(&mut self, (x, y): (f32, f32), radius: f32, color: Rgb, background: Rgb) {
        for tooth in 0..8 {
            let (dy, dx) = (tooth as f32 * FRAC_PI_4).sin_cos();
            let (inner, outer) = (
                (x + dx * radius * 0.5, y + dy * radius * 0.5),
                (x + dx * radius, y + dy * radius),
            );
            self.stroke(inner, outer, radius * 0.4, color);
        }
        self.circle((x, y), radius * 0.74, color);
        self.circle((x, y), radius * 0.32, background);
    }

    fn text(&mut self, x: f32, y: f32, text: String, color: Rgb, bold: bool) {
        self.texts.push((x.round(), y.round(), text, color, bold));
    }

    /// Text centered in `rect`.
    fn label(&mut self, rect: Rect, text: &str, cell: (f32, f32), color: Rgb) {
        let (x, y) = rect.center();
        let x = x - text.width() as f32 * cell.0 / 2.;
        self.text(x, y - cell.1 / 2., text.into(), color, true);
    }
}

/// Horizontal extent of a convex polygon at height `y`.
fn span(points: &[(f32, f32)], y: f32) -> Option<(f32, f32)> {
    let mut span: Option<(f32, f32)> = None;
    for (i, &(x0, y0)) in points.iter().enumerate() {
        let (x1, y1) = points[(i + 1) % points.len()];
        if (y0 <= y) != (y1 <= y) {
            let x = x0 + (y - y0) / (y1 - y0) * (x1 - x0);
            span = Some(span.map_or((x, x), |(left, right)| (left.min(x), right.max(x))));
        }
    }
    span.filter(|(left, right)| right > left)
}

/// `text` cut to `columns` cells, with an ellipsis when it does not fit.
fn fit(text: &str, columns: usize) -> String {
    if text.width() <= columns {
        return text.into();
    }
    let mut width = 0;
    let mut fitted: String = text
        .chars()
        .take_while(|c| {
            width += c.width().unwrap_or(0);
            width < columns
        })
        .collect();
    fitted.push('…');
    fitted
}

/// Draw `text` with its top left corner at `x`, `y` in the window.
fn draw_text(
    renderer: &mut Renderer,
    glyph_cache: &mut GlyphCache,
    window: &SizeInfo,
    (x, y, text, color, bold): (f32, f32, String, Rgb, bool),
) {
    // Place the text with the padding of a grid laid over the window.
    let (cell_width, cell_height) = (window.cell_width(), window.cell_height());
    let (column, line) = ((x / cell_width).floor().max(0.), (y / cell_height).floor().max(0.));
    let (padding_x, padding_y) = (x - column * cell_width, y - line * cell_height);
    let size = SizeInfo::new(
        window.width(),
        window.height(),
        cell_width,
        cell_height,
        padding_x,
        padding_y,
        false,
    );
    renderer.resize(&size);

    let mut column = column as usize;
    let flags = if bold { Flags::BOLD } else { Flags::empty() };
    let cells: Vec<_> = text
        .chars()
        .filter_map(|character| {
            let width = character.width().filter(|&width| width > 0)?;
            let point = Point::new(line as usize, Column(column));
            column += width;
            let flags = if width == 2 { flags | Flags::WIDE_CHAR } else { flags };
            let (fg, bg, underline, bg_alpha, extra) = (color, color, color, 0., None);
            Some(RenderableCell { character, point, fg, bg, bg_alpha, underline, flags, extra })
        })
        .collect();
    renderer.draw_cells(&size, glyph_cache, cells.into_iter());
}

impl Tabs {
    /// Add the bar, which lies above the terminal, to the damage of a frame.
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
    pub fn damage(&self, mut damage: Vec<DamageRect>, size: &SizeInfo) -> Vec<DamageRect> {
        if self.height > 0 {
            let (width, height) = (size.width() as i32, size.height() as i32);
            damage.push(DamageRect::new(0, height, width, self.height as i32));
        }
        damage
    }

    /// Draw the bar above the terminal, and the menu over it.
    pub fn draw(
        &mut self,
        renderer: &mut Renderer,
        glyph_cache: &mut GlyphCache,
        window: &Window,
        size: &SizeInfo,
        config: &UiConfig,
    ) {
        if self.height == 0 {
            return;
        }

        let layout = Layout::new(self, size);
        self.first = layout.first;
        let palette = Palette::new(&config.colors);
        let (cell, height) = (layout.cell, layout.height);
        let px = |value: f32| layout.px(value);
        let line = px(1.).max(1.);
        let icon = (1.3 * layout.scale).max(1.);
        let radius = px(STYLE.radius);
        let mut base = Layer::default();
        let mut top = Layer::default();

        base.rect(Rect::new(0., 0., size.width(), height), palette.chrome);
        base.rect(Rect::new(0., height - line, size.width(), line), palette.outline);
        let track = palette.track;
        base.outlined(layout.track, radius + px(3.), track, palette.track_outline, line);

        let lit = |index: usize| {
            index == self.active || matches!(self.hover, Hit::Tab(i) | Hit::Close(i) if i == index)
        };
        for pair in layout.tabs.windows(2) {
            let ((index, rect), (next, _)) = (pair[0], pair[1]);
            if !lit(index) && !lit(next) {
                let x = rect.right() + px(1.) - line / 2.;
                base.rect(
                    Rect::new(x, rect.y + rect.h * 0.3, line, rect.h * 0.4),
                    palette.separator,
                );
            }
        }

        let dragging = self.drag.as_ref().filter(|drag| drag.moving);
        for &(index, mut rect) in &layout.tabs {
            let active = index == self.active;
            let layer = match dragging {
                // The dragged tab follows the pointer, above the others.
                Some(drag) if active => {
                    let (first, last) =
                        (layout.tabs[0].1.x, layout.tabs[layout.tabs.len() - 1].1.x);
                    rect.x = ((self.pointer.x - drag.grab) as f32).clamp(first, last).round();
                    &mut top
                },
                _ => &mut base,
            };

            let surface = if active {
                layer.outlined(rect, radius, palette.raised, palette.raised_outline, line);
                palette.raised
            } else if lit(index) {
                layer.rounded(rect, radius, palette.hover);
                palette.hover
            } else {
                track
            };

            let tab = self.tabs.get(index);
            let title = if active { window.title() } else { tab.map_or("", |tab| &tab.title) };
            let accent = palette.accent(title);
            let badge = layout.badge(rect);
            layer.rounded(badge, px(4.), mix(surface, accent, 0.3));
            layer.label(badge, tab.map_or(">", |tab| &tab.badge), cell, accent);

            let close = layout.close(rect);
            if lit(index) {
                if self.hover == Hit::Close(index) {
                    layer.rounded(close, px(4.), mix(surface, palette.foreground, 0.15));
                }
                let lines = [[-1., -1., 1., 1.], [-1., 1., 1., -1.]];
                layer.icon(close.center(), px(4.), icon, &lines, palette.muted);
            }

            let left = badge.right() + px(6.);
            let right =
                if lit(index) && !STYLE.close_left { close.x } else { rect.right() - px(8.) };
            let columns = ((right - left) / cell.0).max(0.) as usize;
            let color = if lit(index) { palette.foreground } else { palette.muted };
            layer.text(left, rect.center().1 - cell.1 / 2., fit(title, columns), color, active);
        }

        for &(hit, rect) in &layout.buttons {
            let hovered = self.hover == hit;
            if hovered {
                base.rounded(rect, radius, palette.hover);
            }
            let color = if hovered || hit == Hit::New { palette.foreground } else { palette.muted };
            let lines: &[[f32; 4]] = match hit {
                Hit::New => &[[-1., 0., 1., 0.], [0., -1., 0., 1.]],
                Hit::Menu => &[[-1., -0.5, 0., 0.5], [0., 0.5, 1., -0.5]],
                Hit::Previous => &[[0.5, -1., -0.5, 0.], [-0.5, 0., 0.5, 1.]],
                Hit::Next => &[[-0.5, -1., 0.5, 0.], [0.5, 0., -0.5, 1.]],
                _ => {
                    let background = if hovered { palette.hover } else { palette.chrome };
                    base.gear(rect.center(), px(7.), color, background);
                    continue;
                },
            };
            base.icon(
                rect.center(),
                px(if hit == Hit::Menu { 4. } else { 5. }),
                icon,
                lines,
                color,
            );
        }

        if let (Some(menu), Some((rect, row))) = (&self.menu, layout.menu) {
            let shadow = Rect::new(rect.x, rect.y + px(2.), rect.w, rect.h);
            top.rounded_alpha(shadow, radius + px(2.), Rgb::new(0, 0, 0), 0.3);
            top.outlined(rect, radius, palette.popup, palette.popup_outline, line);
            let highlighted = match self.hover {
                Hit::Item(index) => index,
                _ => menu.selected,
            };
            let names = menu.profiles.iter().map(|profile| profile.name.as_str());
            for (index, name) in names.chain([EDIT_PROFILES]).enumerate() {
                let item = Rect::new(
                    rect.x + px(3.),
                    rect.y + px(3.) + index as f32 * row,
                    rect.w - px(6.),
                    row,
                );
                let surface = if index == highlighted { palette.hover } else { palette.popup };
                if index == highlighted {
                    top.rounded(item, radius - px(1.), surface);
                }

                let accent = palette.accent(name);
                let size = cell.1.max(px(16.));
                let badge = Rect::new(item.x + px(6.), item.y + (row - size) / 2., size, size);
                match menu.profiles.get(index) {
                    Some(profile) => {
                        top.rounded(badge, px(4.), mix(surface, accent, 0.3));
                        top.label(badge, &profile.badge(), cell, accent);
                    },
                    None => top.gear(badge.center(), px(7.), palette.muted, surface),
                }
                let y = item.center().1 - cell.1 / 2.;
                top.text(badge.right() + px(8.), y, name.into(), palette.foreground, false);
            }
        }

        // Draw over the whole window, then give the terminal its own viewport back.
        let window =
            SizeInfo::new(size.width(), size.height() + height, cell.0, cell.1, 0., 0., false);
        let metrics = glyph_cache.font_metrics();
        for layer in [base, top] {
            renderer.draw_rects(&window, &metrics, layer.rects);
            for text in layer.texts {
                draw_text(renderer, glyph_cache, &window, text);
            }
        }
        renderer.resize(size);
    }
}

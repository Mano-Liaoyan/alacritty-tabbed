//! Drawing with Alacritty's renderer: anti-aliased shapes and images become thin rectangles for
//! the rectangle renderer, and text uses the terminal's font.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
use std::sync::{Arc, Mutex};

use resvg::{tiny_skia, usvg};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use winit::dpi::PhysicalPosition;

use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Flags;

use crate::config::color::Colors;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::content::RenderableCell;
use crate::renderer::rects::RenderRect;
use crate::renderer::{GlyphCache, Renderer};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2., self.y + self.h / 2.)
    }

    pub fn contains(&self, point: PhysicalPosition<f64>) -> bool {
        let (x, y) = (point.x as f32, point.y as f32);
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn inset(self, by: f32) -> Self {
        Self::new(self.x + by, self.y + by, self.w - 2. * by, self.h - 2. * by)
    }

    /// A square of `size` centered in this rectangle.
    pub fn square(self, size: f32) -> Self {
        let (x, y) = self.center();
        Self::new((x - size / 2.).round(), (y - size / 2.).round(), size, size)
    }
}

pub fn mix(from: Rgb, to: Rgb, amount: f32) -> Rgb {
    let mix = |from: u8, to: u8| f32::from(from) + (f32::from(to) - f32::from(from)) * amount;
    Rgb::new(mix(from.r, to.r) as u8, mix(from.g, to.g) as u8, mix(from.b, to.b) as u8)
}

pub const WHITE: Rgb = Rgb::new(255, 255, 255);
pub const BLACK: Rgb = Rgb::new(0, 0, 0);

/// Colors of the tabs' chrome, derived from the terminal's colors.
pub struct Palette {
    pub terminal: Rgb,
    pub chrome: Rgb,
    pub track: Rgb,
    pub track_outline: Rgb,
    pub raised: Rgb,
    pub raised_outline: Rgb,
    pub hover: Rgb,
    pub pressed: Rgb,
    pub separator: Rgb,
    pub card_outline: Rgb,
    pub foreground: Rgb,
    pub muted: Rgb,
    pub danger: Rgb,
    pub popup: Rgb,
    pub popup_outline: Rgb,
    pub field: Rgb,
    pub field_outline: Rgb,
    pub focus: Rgb,
    accents: [Rgb; 5],
}

impl Palette {
    pub fn new(colors: &Colors) -> Self {
        let terminal = colors.primary.background;
        let text = colors.primary.foreground;
        let luma = 0.2126 * f32::from(terminal.r)
            + 0.7152 * f32::from(terminal.g)
            + 0.0722 * f32::from(terminal.b);
        let light = luma > 127.5;

        // The chrome sits slightly behind the terminal, with raised surfaces closer to the light.
        let chrome = mix(terminal, BLACK, if light { 0.06 } else { 0.3 });
        let raised = if light { mix(terminal, WHITE, 0.7) } else { mix(terminal, text, 0.1) };
        let popup = if light { mix(terminal, WHITE, 0.5) } else { mix(chrome, text, 0.07) };
        let field = mix(terminal, text, if light { 0.035 } else { 0.05 });
        let normal = &colors.normal;
        Self {
            terminal,
            chrome,
            track: mix(chrome, text, 0.045),
            track_outline: mix(chrome, text, 0.085),
            raised,
            raised_outline: mix(raised, text, if light { 0.14 } else { 0.1 }),
            hover: mix(chrome, text, 0.09),
            pressed: mix(terminal, text, 0.2),
            separator: mix(chrome, text, 0.16),
            card_outline: mix(terminal, text, 0.11),
            foreground: mix(chrome, text, 0.94),
            muted: mix(chrome, text, 0.56),
            danger: Rgb::new(196, 43, 28),
            popup,
            popup_outline: mix(popup, text, 0.14),
            field,
            field_outline: mix(field, text, 0.1),
            focus: mix(terminal, text, 0.42),
            accents: [normal.green, normal.yellow, normal.blue, normal.magenta, normal.cyan],
        }
    }

    /// A stable badge color for the kind of program a tab runs.
    pub fn accent(&self, title: &str) -> Rgb {
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

/// Rows of equal pixels: `(x, y, width, color, alpha)`.
type Runs = Arc<Vec<(u16, u16, u16, Rgb, f32)>>;

/// An image, drawn as rectangles of equal pixels.
pub struct Image {
    source: Source,
    /// Draw the image in one color, for single-color logos.
    tint: bool,
    /// The image fit in squares of a size.
    scaled: Mutex<Vec<(u32, Runs)>>,
}

enum Source {
    /// Pixels with straight alpha.
    Pixels { width: u32, height: u32, rgba: Vec<u8> },
    /// A vector image, rendered at each size it is drawn at.
    Svg(usvg::Tree),
}

impl Image {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Arc<Self>> {
        let valid = width > 0 && height > 0 && rgba.len() == (width * height * 4) as usize;
        let source = Source::Pixels { width, height, rgba };
        valid.then(|| Arc::new(Self { source, tint: false, scaled: Default::default() }))
    }

    pub fn svg(data: &[u8], tint: bool) -> Option<Arc<Self>> {
        let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
        Some(Arc::new(Self { source: Source::Svg(tree), tint, scaled: Default::default() }))
    }

    /// Runs of the image fit in a `size` square, averaging the pixels each one covers.
    fn runs(&self, size: u32) -> Runs {
        let mut scaled = self.scaled.lock().unwrap();
        if let Some((_, runs)) = scaled.iter().find(|(s, _)| *s == size) {
            return runs.clone();
        }

        let rendered;
        let (width, height, rgba) = match &self.source {
            Source::Pixels { width, height, rgba } => (*width, *height, &rgba[..]),
            Source::Svg(tree) => {
                rendered = render(tree, size.max(1));
                (size.max(1), size.max(1), &rendered[..])
            },
        };
        let scale = width.max(height) as f32 / size as f32;
        let (w, h) = ((width as f32 / scale) as u32, (height as f32 / scale) as u32);
        let (left, top) = ((size - w.min(size)) / 2, (size - h.min(size)) / 2);
        let samples = (scale.ceil() as u32).clamp(1, 8);
        let mut runs: Vec<(u16, u16, u16, Rgb, f32)> = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0f32; 4];
                for sy in 0..samples {
                    for sx in 0..samples {
                        let at = |v: u32, s: u32| {
                            ((v as f32 + (s as f32 + 0.5) / samples as f32) * scale) as u32
                        };
                        let (px, py) = (at(x, sx).min(width - 1), at(y, sy).min(height - 1));
                        let pixel = &rgba[((py * width + px) * 4) as usize..][..4];
                        let alpha = f32::from(pixel[3]);
                        for i in 0..3 {
                            sum[i] += f32::from(pixel[i]) * alpha;
                        }
                        sum[3] += alpha;
                    }
                }
                let alpha = sum[3] / (samples * samples) as f32 / 255.;
                if alpha < 0.02 {
                    continue;
                }
                let color = |i: usize| (sum[i] / sum[3]) as u8;
                let color = Rgb::new(color(0), color(1), color(2));
                let alpha = (alpha * 64.).round() / 64.;
                let (x, y) = ((left + x) as u16, (top + y) as u16);
                match runs.last_mut() {
                    Some(run)
                        if run.1 == y && run.0 + run.2 == x && run.3 == color && run.4 == alpha =>
                    {
                        run.2 += 1
                    },
                    _ => runs.push((x, y, 1, color, alpha)),
                }
            }
        }
        let runs = Arc::new(runs);
        scaled.push((size, runs.clone()));
        runs
    }
}

/// Render a vector image centered in a `size` square, as pixels with straight alpha.
fn render(tree: &usvg::Tree, size: u32) -> Vec<u8> {
    let Some(mut pixmap) = tiny_skia::Pixmap::new(size, size) else { return Vec::new() };
    let (width, height) = (tree.size().width(), tree.size().height());
    let scale = size as f32 / width.max(height);
    let (x, y) = ((size as f32 - width * scale) / 2., (size as f32 - height * scale) / 2.);
    resvg::render(
        tree,
        tiny_skia::Transform::from_row(scale, 0., 0., scale, x, y),
        &mut pixmap.as_mut(),
    );
    let pixels = pixmap.pixels().iter().map(|pixel| pixel.demultiply());
    pixels.flat_map(|pixel| [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]).collect()
}

/// Shapes and labels drawn together; later layers cover earlier ones.
#[derive(Default)]
pub struct Layer {
    rects: Vec<RenderRect>,
    texts: Vec<(f32, f32, String, Rgb, bool)>,
}

impl Layer {
    pub fn rect(&mut self, rect: Rect, color: Rgb) {
        self.rects.push(RenderRect::new(rect.x, rect.y, rect.w, rect.h, color, 1.));
    }

    /// Fill a convex polygon, anti-aliased with four samples per pixel row.
    pub fn fill(&mut self, points: &[(f32, f32)], color: Rgb, alpha: f32) {
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

    pub fn rounded(&mut self, rect: Rect, radius: f32, color: Rgb) {
        self.rounded_alpha(rect, radius, color, 1.);
    }

    pub fn rounded_alpha(&mut self, rect: Rect, radius: f32, color: Rgb, alpha: f32) {
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

    pub fn outlined(&mut self, rect: Rect, radius: f32, color: Rgb, outline: Rgb, width: f32) {
        self.rounded(rect, radius, outline);
        self.rounded(rect.inset(width), radius - width, color);
    }

    pub fn circle(&mut self, (x, y): (f32, f32), radius: f32, color: Rgb) {
        self.rounded(Rect::new(x - radius, y - radius, 2. * radius, 2. * radius), radius, color);
    }

    pub fn stroke(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Rgb) {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let scale = width / 2. / dx.hypot(dy).max(f32::EPSILON);
        let (nx, ny) = (-dy * scale, dx * scale);
        let quad = [(from.0 + nx, from.1 + ny), (to.0 + nx, to.1 + ny), (to.0 - nx, to.1 - ny)];
        self.fill(&[quad[0], quad[1], quad[2], (from.0 - nx, from.1 - ny)], color, 1.);
    }

    /// Strokes through the points, relative to `center` in units of `size`.
    pub fn icon(
        &mut self,
        center: (f32, f32),
        size: f32,
        width: f32,
        lines: &[[f32; 4]],
        color: Rgb,
    ) {
        let at = |x: f32, y: f32| (center.0 + x * size, center.1 + y * size);
        for &[x0, y0, x1, y1] in lines {
            self.stroke(at(x0, y0), at(x1, y1), width, color);
        }
    }

    pub fn gear(&mut self, (x, y): (f32, f32), radius: f32, color: Rgb, background: Rgb) {
        for tooth in 0..8 {
            let (dy, dx) = (tooth as f32 * FRAC_PI_4).sin_cos();
            let inner = (x + dx * radius * 0.5, y + dy * radius * 0.5);
            self.stroke(inner, (x + dx * radius, y + dy * radius), radius * 0.4, color);
        }
        self.circle((x, y), radius * 0.74, color);
        self.circle((x, y), radius * 0.32, background);
    }

    /// The outside of a rounded rectangle in `color`, and its outline, near its corners.
    pub fn corners(&mut self, rect: Rect, radius: f32, color: Rgb, outline: Rgb, width: f32) {
        let r = radius.round();
        for (cx, cy, x0, y0) in [
            (rect.x + r, rect.y + r, rect.x, rect.y),
            (rect.right() - r, rect.y + r, rect.right() - r, rect.y),
            (rect.x + r, rect.bottom() - r, rect.x, rect.bottom() - r),
            (rect.right() - r, rect.bottom() - r, rect.right() - r, rect.bottom() - r),
        ] {
            for y in 0..r as u32 {
                for x in 0..r as u32 {
                    let (mut outside, mut ring) = (0., 0.);
                    for s in 0..16 {
                        let sx = x0 + x as f32 + (s % 4) as f32 / 4. + 0.125;
                        let sy = y0 + y as f32 + (s / 4) as f32 / 4. + 0.125;
                        let distance = (sx - cx).hypot(sy - cy);
                        if distance > r {
                            outside += 1. / 16.;
                        } else if distance > r - width {
                            ring += 1. / 16.;
                        }
                    }
                    let at = Rect::new(x0 + x as f32, y0 + y as f32, 1., 1.);
                    for (color, alpha) in [(color, outside), (outline, ring / (1. - outside))] {
                        if alpha > 0.02 {
                            self.rects.push(RenderRect::new(at.x, at.y, 1., 1., color, alpha));
                        }
                    }
                }
            }
        }
    }

    /// Draw `image` fit in the `rect` square.
    /// Draw `image` fit in the `rect` square, in `tint` if it is a single-color image.
    pub fn image(&mut self, rect: Rect, image: &Image, tint: Rgb) {
        let runs = image.runs(rect.w.min(rect.h) as u32);
        self.rects.extend(runs.iter().map(|&(x, y, w, color, alpha)| {
            let (x, y) = (rect.x + f32::from(x), rect.y + f32::from(y));
            let color = if image.tint { tint } else { color };
            RenderRect::new(x, y, f32::from(w), 1., color, alpha)
        }));
    }

    pub fn text(&mut self, x: f32, y: f32, text: String, color: Rgb, bold: bool) {
        self.texts.push((x.round(), y.round(), text, color, bold));
    }

    /// Text centered in `rect`.
    pub fn label(&mut self, rect: Rect, text: &str, cell: (f32, f32), color: Rgb, bold: bool) {
        let (x, y) = rect.center();
        let x = x - text.width() as f32 * cell.0 / 2.;
        self.text(x, y - cell.1 / 2., text.into(), color, bold);
    }

    /// Draw the layers over the window, whose size is `window`.
    pub fn draw(
        layers: Vec<Layer>,
        renderer: &mut Renderer,
        glyph_cache: &mut GlyphCache,
        window: &SizeInfo,
    ) {
        let metrics = glyph_cache.font_metrics();
        for layer in layers {
            renderer.draw_rects(window, &metrics, layer.rects);
            for text in layer.texts {
                draw_text(renderer, glyph_cache, window, text);
            }
        }
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
pub fn fit(text: &str, columns: usize) -> String {
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
    let (width, height) = (window.width(), window.height());
    let size = SizeInfo::new(width, height, cell_width, cell_height, padding_x, padding_y, false);
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

//! Drawing with Alacritty's renderer: anti-aliased shapes and images become thin rectangles for
//! the rectangle renderer, and text uses the interface's font.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
use std::sync::{Arc, Mutex};

use alacritty_tabbed_deps::resvg;
use resvg::{tiny_skia, usvg};
use winit::dpi::PhysicalPosition;

use crate::config::color::Colors;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::renderer::Renderer;
use crate::renderer::rects::RenderRect;

use super::text::{Text, UiFont};

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

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
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

/// Strokes of the zoom glyph, in units of half its size, for [`Layer::icon`]: arrows out to two
/// corners.
pub const ZOOM_GLYPH: &[[f32; 4]] =
    &[[-1., 1., 1., -1.], [0.2, -1., 1., -1.], [1., -1., 1., -0.2], [-1., 0.2, -1., 1.], [
        -1., 1., -0.2, 1.,
    ]];
/// Strokes of the unzoom glyph: arrows back in from the corners.
pub const UNZOOM_GLYPH: &[[f32; 4]] = &[
    [-1., 1., -0.15, 0.15],
    [-0.85, 0.15, -0.15, 0.15],
    [-0.15, 0.15, -0.15, 0.85],
    [1., -1., 0.15, -0.15],
    [0.85, -0.15, 0.15, -0.15],
    [0.15, -0.15, 0.15, -0.85],
];

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
    /// The main action, from the terminal's blue.
    pub primary: Rgb,
    /// Accent of the panes: the focused one's border and where a pane would drop.
    pub highlight: Rgb,
    /// Backing out of changes, from the terminal's red.
    pub caution: Rgb,
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
            primary: colors.normal.blue,
            highlight: colors.normal.blue,
            caution: colors.normal.red,
            accents: [normal.green, normal.yellow, normal.blue, normal.magenta, normal.cyan],
        }
    }

    /// Black or white, whichever contrasts more with `background`, as WCAG measures it.
    pub fn on(background: Rgb) -> Rgb {
        let linear = |channel: u8| {
            let channel = f32::from(channel) / 255.;
            if channel <= 0.04045 { channel / 12.92 } else { ((channel + 0.055) / 1.055).powf(2.4) }
        };
        let luminance = 0.2126 * linear(background.r)
            + 0.7152 * linear(background.g)
            + 0.0722 * linear(background.b);
        if (luminance + 0.05) / 0.05 > 1.05 / (luminance + 0.05) { BLACK } else { WHITE }
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
    Svg(Box<usvg::Tree>),
}

impl Image {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Arc<Self>> {
        let valid = width > 0 && height > 0 && rgba.len() == (width * height * 4) as usize;
        let source = Source::Pixels { width, height, rgba };
        valid.then(|| Arc::new(Self { source, tint: false, scaled: Default::default() }))
    }

    pub fn svg(data: &[u8], tint: bool) -> Option<Arc<Self>> {
        let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
        Some(Arc::new(Self {
            source: Source::Svg(Box::new(tree)),
            tint,
            scaled: Default::default(),
        }))
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
    texts: Vec<Text>,
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

    /// The outside of a rounded rectangle in `color`, and its outline, near its corners, each
    /// rounded by its own of `radii`: top left, top right, bottom left, bottom right.
    pub fn corners(&mut self, rect: Rect, radii: [f32; 4], color: Rgb, outline: Rgb, width: f32) {
        let [tl, tr, bl, br] = radii.map(f32::round);
        for (r, cx, cy, x0, y0) in [
            (tl, rect.x + tl, rect.y + tl, rect.x, rect.y),
            (tr, rect.right() - tr, rect.y + tr, rect.right() - tr, rect.y),
            (bl, rect.x + bl, rect.bottom() - bl, rect.x, rect.bottom() - bl),
            (br, rect.right() - br, rect.bottom() - br, rect.right() - br, rect.bottom() - br),
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

    /// Draw `image` fit in the `rect` square, in `tint` if it is a single-color image.
    pub fn image(&mut self, rect: Rect, image: &Image, tint: Rgb) {
        self.image_alpha(rect, image, tint, 1.);
    }

    /// Draw `image` like [`Self::image`], `opacity` of the way over what is under it.
    pub fn image_alpha(&mut self, rect: Rect, image: &Image, tint: Rgb, opacity: f32) {
        let runs = image.runs(rect.w.min(rect.h) as u32);
        self.rects.extend(runs.iter().map(|&(x, y, w, color, alpha)| {
            let (x, y) = (rect.x + f32::from(x), rect.y + f32::from(y));
            let color = if image.tint { tint } else { color };
            RenderRect::new(x, y, f32::from(w), 1., color, alpha * opacity)
        }));
    }

    pub fn text(&mut self, x: f32, y: f32, text: String, color: Rgb, bold: bool) {
        self.texts.push((x.round(), y.round(), text, color, bold));
    }

    /// Text centered in `rect`, cut to fit it.
    pub fn label(&mut self, rect: Rect, text: &str, font: &UiFont, color: Rgb, bold: bool) {
        let text = font.fit(text, rect.w, bold);
        let (x, y) = rect.center();
        let x = x - font.width(&text, bold) / 2.;
        self.text(x, y - font.line / 2., text, color, bold);
    }

    /// Keep only what lies inside `rect`, keeping or dropping text by whole lines of `line` pixels.
    pub fn clip(&mut self, rect: Rect, line: f32) {
        self.rects.retain_mut(|r| {
            let (left, top) = (r.x.max(rect.x), r.y.max(rect.y));
            let (right, bottom) =
                ((r.x + r.width).min(rect.right()), (r.y + r.height).min(rect.bottom()));
            (r.x, r.y, r.width, r.height) = (left, top, right - left, bottom - top);
            right > left && bottom > top
        });
        let inside = |y: f32| y >= rect.y - 0.5 && y + line <= rect.bottom() + 0.5;
        self.texts.retain(|&(x, y, ..)| inside(y) && x < rect.right());
    }

    /// Show what is drawn only `opacity` of the way over what is under it, its text blended
    /// into `under`.
    pub fn fade(&mut self, opacity: f32, under: Rgb) {
        self.rects.iter_mut().for_each(|rect| rect.alpha *= opacity);
        self.texts.iter_mut().for_each(|text| text.3 = mix(under, text.3, opacity));
    }

    /// Draw what `other` holds over what this one does.
    pub fn extend(&mut self, other: Layer) {
        self.rects.extend(other.rects);
        self.texts.extend(other.texts);
    }

    /// Move everything drawn by `dx`, `dy`, kept on whole pixels so text stays sharp.
    pub fn translate(&mut self, dx: f32, dy: f32) {
        let (dx, dy) = (dx.round(), dy.round());
        self.rects.iter_mut().for_each(|rect| (rect.x, rect.y) = (rect.x + dx, rect.y + dy));
        self.texts.iter_mut().for_each(|text| (text.0, text.1) = (text.0 + dx, text.1 + dy));
    }

    /// Draw the layers over the window, whose size is `window`.
    pub fn draw(layers: Vec<Layer>, renderer: &mut Renderer, font: &UiFont, window: &SizeInfo) {
        for layer in layers {
            renderer.draw_rects(window, &font.metrics, layer.rects);
            font.draw(renderer, window, layer.texts);
        }
    }
}

/// Rectangles whose alpha is how much of each pixel near the corners of `rect` lies inside it,
/// once they are rounded by `radius`; they cut the corners out of what is drawn under them.
pub fn mask(rect: Rect, radius: f32) -> Vec<RenderRect> {
    let r = radius.round();
    let mut rects = Vec::new();
    for (cx, cy, x0, y0) in [
        (rect.x + r, rect.y + r, rect.x, rect.y),
        (rect.right() - r, rect.y + r, rect.right() - r, rect.y),
        (rect.x + r, rect.bottom() - r, rect.x, rect.bottom() - r),
        (rect.right() - r, rect.bottom() - r, rect.right() - r, rect.bottom() - r),
    ] {
        for y in 0..r as u32 {
            // Pixels wholly outside, at either end of the row, are cut as one.
            let mut outside: Option<(f32, f32)> = None;
            for x in 0..r as u32 {
                let (px, py) = (x0 + x as f32, y0 + y as f32);
                let inside = (0..16)
                    .filter(|s| {
                        let sx = px + (s % 4) as f32 / 4. + 0.125;
                        let sy = py + (s / 4) as f32 / 4. + 0.125;
                        (sx - cx).hypot(sy - cy) <= r
                    })
                    .count() as f32
                    / 16.;
                if inside == 0. {
                    let run = outside.get_or_insert((px, 0.));
                    run.1 += 1.;
                    continue;
                }
                if let Some((x, w)) = outside.take() {
                    rects.push(RenderRect::new(x, py, w, 1., BLACK, 0.));
                }
                if inside < 1. {
                    rects.push(RenderRect::new(px, py, 1., 1., BLACK, inside));
                }
            }
            if let Some((x, w)) = outside {
                rects.push(RenderRect::new(x, y0 + y as f32, w, 1., BLACK, 0.));
            }
        }
    }
    rects
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_on_a_color_contrasts_most() {
        assert_eq!(Palette::on(Rgb::new(0x7a, 0xa6, 0xda)), BLACK);
        assert_eq!(Palette::on(Rgb::new(0x0a, 0x3d, 0x91)), WHITE);
        assert_eq!(Palette::on(WHITE), BLACK);
        assert_eq!(Palette::on(BLACK), WHITE);
    }

    #[test]
    fn translate_moves_shapes_and_text_by_whole_pixels() {
        let mut layer = Layer::default();
        layer.rect(Rect::new(10., 20., 30., 4.), WHITE);
        layer.text(5., 6., "a".into(), WHITE, false);
        layer.translate(2., -12.4);
        let rect = &layer.rects[0];
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (12., 8., 30., 4.));
        assert_eq!((layer.texts[0].0, layer.texts[0].1), (7., -6.));
    }
}

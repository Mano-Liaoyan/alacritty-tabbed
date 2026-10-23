//! A color picker that pops over a color well, like the color panel of macOS: a square of
//! saturation and brightness for the hue chosen on a strip below it, swatches of the terminal's
//! colors, and the color as it was beside the color as it is.

use winit::dpi::PhysicalPosition;
use winit::keyboard::NamedKey;

use crate::display::color::Rgb;

use super::super::bar::{Layout, TRACK_RADIUS};
use super::super::paint::{BLACK, Layer, Palette, Rect, WHITE};

/// Number of swatches; the first stands for the automatic color.
pub const SWATCHES: usize = 8;

/// A part of the picker the pointer can take hold of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The square of saturation, from left to right, and brightness, from bottom to top.
    Square,
    /// The strip of hues.
    Hue,
    Swatch(usize),
    /// The color as it was when the picker opened, which takes it back.
    Original,
}

/// Geometry of the picker in window pixels.
#[derive(Clone, Copy, Debug)]
pub struct Parts {
    pub popover: Rect,
    square: Rect,
    hue: Rect,
    swatches: [Rect; SWATCHES],
    /// The color as it was, and as it is, side by side.
    original: Rect,
    current: Rect,
}

pub struct ColorPicker {
    /// Hue, saturation and brightness from 0 to 1, kept apart from the color so that a grey keeps
    /// the hue it was made from.
    hsv: [f32; 3],
    /// Whether the color is the automatic one, the first swatch's.
    auto: bool,
    original: Option<Rgb>,
    swatches: [Rgb; SWATCHES],
    dragging: Option<Part>,
}

impl ColorPicker {
    /// A picker showing `color`, or for none the automatic color, the first of `swatches`.
    pub fn new(color: Option<Rgb>, swatches: [Rgb; SWATCHES]) -> Self {
        let hsv = rgb_to_hsv(color.unwrap_or(swatches[0]));
        Self { hsv, auto: color.is_none(), original: color, swatches, dragging: None }
    }

    pub fn rgb(&self) -> Rgb {
        hsv_to_rgb(self.hsv)
    }

    /// The color chosen, or none for the automatic one.
    pub fn color(&self) -> Option<Rgb> {
        (!self.auto).then(|| self.rgb())
    }

    /// Show `color`, keeping the hue and saturation it leaves open, as a grey or black does.
    pub fn set(&mut self, color: Option<Rgb>) {
        self.auto = color.is_none();
        let rgb = color.unwrap_or(self.swatches[0]);
        if rgb == self.rgb() {
            return;
        }
        let [hue, saturation, value] = rgb_to_hsv(rgb);
        self.hsv = if value == 0. {
            [self.hsv[0], self.hsv[1], 0.]
        } else if saturation == 0. {
            [self.hsv[0], 0., value]
        } else {
            [hue, saturation, value]
        };
    }

    /// Where the parts lie for a picker with its top left corner at `origin`, at `scale`.
    pub fn layout((x, y): (f32, f32), scale: f32) -> Parts {
        let px = |value: f32| (value * scale).round();
        let (x, y, pad, width) = (x.round(), y.round(), px(12.), px(224.));
        let square = Rect::new(x + pad, y + pad, width, px(144.));
        let hue = Rect::new(square.x, square.bottom() + px(10.), width, px(12.));
        let size = px(20.);
        let pitch = (width - size) / (SWATCHES - 1) as f32;
        let top = hue.bottom() + px(12.);
        let swatches = std::array::from_fn(|index| {
            Rect::new((square.x + index as f32 * pitch).round(), top, size, size)
        });
        let original = Rect::new(square.x, top + size + px(12.), px(32.), px(24.));
        let current = Rect { x: original.right(), ..original };
        let popover = Rect::new(x, y, width + 2. * pad, current.bottom() + pad - y);
        Parts { popover, square, hue, swatches, original, current }
    }

    /// The part under `point`.
    pub fn part_at(parts: &Parts, point: PhysicalPosition<f64>) -> Option<Part> {
        // The thin strip of hues also takes the pointer a little above and below it.
        let hue = parts.hue;
        let hue = Rect { y: hue.y - hue.h / 2., h: 2. * hue.h, ..hue };
        let swatch = parts.swatches.iter().position(|swatch| swatch.contains(point));
        if parts.square.contains(point) {
            Some(Part::Square)
        } else if hue.contains(point) {
            Some(Part::Hue)
        } else if let Some(index) = swatch {
            Some(Part::Swatch(index))
        } else {
            parts.original.contains(point).then_some(Part::Original)
        }
    }

    /// Take the color under `point`, returning it, or none for the automatic color, when the
    /// press lands on a part.
    pub fn press(&mut self, parts: &Parts, point: PhysicalPosition<f64>) -> Option<Option<Rgb>> {
        let part = Self::part_at(parts, point)?;
        match part {
            Part::Square | Part::Hue => {
                self.dragging = Some(part);
                return self.drag(parts, point).map(Some);
            },
            Part::Swatch(0) => self.set(None),
            Part::Swatch(index) => self.set(Some(self.swatches[index])),
            Part::Original => self.set(self.original),
        }
        Some(self.color())
    }

    /// Follow the pointer with the part being dragged, returning the color it makes.
    pub fn drag(&mut self, parts: &Parts, point: PhysicalPosition<f64>) -> Option<Rgb> {
        let (x, y) = (point.x as f32, point.y as f32);
        let along = |start: f32, length: f32| ((x - start) / length).clamp(0., 1.);
        match self.dragging? {
            Part::Square => {
                let square = parts.square;
                self.hsv[1] = along(square.x, square.w);
                self.hsv[2] = 1. - ((y - square.y) / square.h).clamp(0., 1.);
            },
            Part::Hue => self.hsv[0] = along(parts.hue.x, parts.hue.w),
            _ => return None,
        }
        self.auto = false;
        Some(self.rgb())
    }

    pub fn release(&mut self) {
        self.dragging = None;
    }

    /// Move through the square with the arrow keys, by a hundredth, or a tenth with `shift`.
    pub fn key(&mut self, key: NamedKey, shift: bool) -> Option<Rgb> {
        let step = if shift { 0.1 } else { 0.01 };
        let (channel, step) = match key {
            NamedKey::ArrowLeft => (1, -step),
            NamedKey::ArrowRight => (1, step),
            NamedKey::ArrowDown => (2, -step),
            NamedKey::ArrowUp => (2, step),
            _ => return None,
        };
        self.hsv[channel] = (self.hsv[channel] + step).clamp(0., 1.);
        self.auto = false;
        Some(self.rgb())
    }

    /// Center of the square's knob.
    fn knob(&self, parts: &Parts) -> (f32, f32) {
        let square = parts.square;
        (square.x + self.hsv[1] * square.w, square.y + (1. - self.hsv[2]) * square.h)
    }

    /// Center of the hue strip's knob.
    fn hue_knob(&self, parts: &Parts) -> (f32, f32) {
        (parts.hue.x + self.hsv[0] * parts.hue.w, parts.hue.center().1)
    }

    /// Swatch of the color shown.
    fn chosen(&self) -> Option<usize> {
        match self.auto {
            true => Some(0),
            false => (1..SWATCHES).find(|&index| self.swatches[index] == self.rgb()),
        }
    }

    pub fn draw(
        &self,
        layer: &mut Layer,
        parts: &Parts,
        palette: &Palette,
        layout: &Layout,
        hovered: Option<Part>,
    ) {
        let px = |value: f32| layout.px(value);
        let line = px(1.).max(1.);
        let disc = |layer: &mut Layer, (x, y): (f32, f32), radius: f32, color: Rgb, alpha: f32| {
            let rect = Rect::new(x - radius, y - radius, 2. * radius, 2. * radius);
            layer.rounded_alpha(rect, layout.curve(radius), color, alpha);
        };

        // A popover like the font list.
        let popover = parts.popover;
        let radius = layout.radius(TRACK_RADIUS);
        let shadow = Rect { y: popover.y + px(2.), ..popover };
        layer.rounded_alpha(shadow, radius + layout.radius(2.), BLACK, 0.3);
        layer.outlined(popover, radius, palette.popup, palette.popup_outline, line);

        // The hue, whitened towards the left and darkened towards the bottom a line of pixels at a
        // time, as the sum of the two ramps is the color at each point.
        let square = parts.square;
        layer.rect(square, hsv_to_rgb([self.hsv[0], 1., 1.]));
        for x in 0..square.w as u32 {
            let column = Rect::new(square.x + x as f32, square.y, 1., square.h);
            band(layer, column, WHITE, 1. - (x as f32 + 0.5) / square.w);
        }
        for y in 0..square.h as u32 {
            let row = Rect::new(square.x, square.y + y as f32, square.w, 1.);
            band(layer, row, BLACK, (y as f32 + 0.5) / square.h);
        }
        layer.corners(square, [layout.radius(6.); 4], palette.popup, palette.popup, 0.);
        let knob = self.knob(parts);
        disc(layer, knob, px(7.), BLACK, 0.3);
        disc(layer, knob, px(6.), WHITE, 1.);
        disc(layer, knob, px(4.), self.rgb(), 1.);

        let hue = parts.hue;
        for x in 0..hue.w as u32 {
            let color = hsv_to_rgb([(x as f32 + 0.5) / hue.w, 1., 1.]);
            layer.rect(Rect::new(hue.x + x as f32, hue.y, 1., hue.h), color);
        }
        layer.corners(hue, [layout.curve(hue.h / 2.); 4], palette.popup, palette.popup, 0.);
        let knob = self.hue_knob(parts);
        disc(layer, knob, px(8.), BLACK, 0.3);
        disc(layer, knob, px(7.), WHITE, 1.);
        disc(layer, knob, px(5.), hsv_to_rgb([self.hsv[0], 1., 1.]), 1.);

        // Swatches, the chosen one ringed and the automatic one struck through.
        let chosen = self.chosen();
        for (index, (swatch, &color)) in parts.swatches.iter().zip(&self.swatches).enumerate() {
            let (center, radius) = (swatch.center(), swatch.w / 2.);
            if chosen == Some(index) {
                disc(layer, center, radius + px(4.), palette.focus, 1.);
                disc(layer, center, radius + px(2.), palette.popup, 1.);
            } else if hovered == Some(Part::Swatch(index)) {
                disc(layer, center, radius + line, palette.focus, 1.);
            }
            disc(layer, center, radius, color, 1.);
            if index == 0 {
                let (x, y, slash) = (center.0, center.1, radius * 0.55);
                let ink = Palette::on(color);
                layer.stroke((x - slash, y + slash), (x + slash, y - slash), line, ink);
            }
        }

        // The color as it was beside the color as it is, and the name of the one pointed at.
        let (original, current) = (parts.original, parts.current);
        let well = Rect { w: current.right() - original.x, ..original };
        let radius = layout.radius(6.);
        layer.rounded(well.inset(-line), radius + line, palette.popup_outline);
        let before = self.original.unwrap_or(self.swatches[0]);
        layer.rounded(well, radius, self.rgb());
        layer.rounded(original, radius, before);
        layer.rect(Rect { x: original.right() - radius, w: radius, ..original }, before);
        let name = |color: Option<Rgb>| color.map_or_else(|| "Auto".into(), |c| c.to_string());
        let (text, color) = match hovered {
            Some(Part::Original) => (format!("Back to {}", name(self.original)), palette.muted),
            _ => (name(self.color()), palette.foreground),
        };
        let font = &layout.font;
        let end = popover.right() - px(12.);
        let text = font.fit(&text, (end - current.right() - px(12.)).max(0.), false);
        let x = end - font.width(&text, false);
        layer.text(x, current.center().1 - layout.line / 2., text, color, false);
    }
}

/// A rectangle of `color` at `alpha`, on whole pixels.
fn band(layer: &mut Layer, rect: Rect, color: Rgb, alpha: f32) {
    let (right, bottom) = (rect.right(), rect.bottom());
    layer.fill(
        &[(rect.x, rect.y), (right, rect.y), (right, bottom), (rect.x, bottom)],
        color,
        alpha,
    );
}

/// The color of hue, saturation and brightness from 0 to 1.
pub fn hsv_to_rgb([hue, saturation, value]: [f32; 3]) -> Rgb {
    let sector = hue.rem_euclid(1.) * 6.;
    let channel = |n: f32| {
        let k = (n + sector) % 6.;
        let channel = value - value * saturation * k.min(4. - k).clamp(0., 1.);
        (channel * 255.).round() as u8
    };
    Rgb::new(channel(5.), channel(3.), channel(1.))
}

/// Hue, saturation and brightness from 0 to 1 of a color; greys have hue 0.
pub fn rgb_to_hsv(color: Rgb) -> [f32; 3] {
    let [r, g, b] = [color.r, color.g, color.b].map(|channel| f32::from(channel) / 255.);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let delta = max - min;
    let sector = if delta == 0. {
        0.
    } else if max == r {
        ((g - b) / delta).rem_euclid(6.)
    } else if max == g {
        (b - r) / delta + 2.
    } else {
        (r - g) / delta + 4.
    };
    let saturation = if max == 0. { 0. } else { delta / max };
    [sector / 6., saturation, max]
}

/// A color written as `#RRGGBB` or `RRGGBB`, in either case.
pub fn parse_hex(text: &str) -> Option<Rgb> {
    let text = text.trim();
    let digits = text.strip_prefix('#').unwrap_or(text);
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let color = u32::from_str_radix(digits, 16).ok()?;
    Some(Rgb::new((color >> 16) as u8, (color >> 8) as u8, color as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SWATCH_COLORS: [Rgb; SWATCHES] = [
        Rgb::new(0x3b, 0x8e, 0xea),
        Rgb::new(0x11, 0xa8, 0xcd),
        Rgb::new(0x0d, 0xbc, 0x79),
        Rgb::new(0xe5, 0xe5, 0x10),
        Rgb::new(0xcd, 0x31, 0x31),
        Rgb::new(0xbc, 0x3f, 0xbc),
        Rgb::new(0x3b, 0x8e, 0xff),
        Rgb::new(0xe5, 0xe5, 0xe5),
    ];

    fn at(x: f32, y: f32) -> PhysicalPosition<f64> {
        PhysicalPosition::new(f64::from(x), f64::from(y))
    }

    fn close(a: Rgb, b: Rgb) -> bool {
        let near = |a: u8, b: u8| a.abs_diff(b) <= 1;
        near(a.r, b.r) && near(a.g, b.g) && near(a.b, b.b)
    }

    #[test]
    fn primaries_and_greys_convert_both_ways() {
        for (color, hsv) in [
            (Rgb::new(255, 0, 0), [0., 1., 1.]),
            (Rgb::new(0, 255, 0), [1. / 3., 1., 1.]),
            (Rgb::new(0, 0, 255), [2. / 3., 1., 1.]),
            (Rgb::new(0, 0, 0), [0., 0., 0.]),
            (Rgb::new(255, 255, 255), [0., 0., 1.]),
            (Rgb::new(128, 128, 128), [0., 0., 128. / 255.]),
        ] {
            let converted = rgb_to_hsv(color);
            for (got, want) in converted.iter().zip(hsv) {
                assert!((got - want).abs() < 1e-4, "{color}: {converted:?}");
            }
            assert_eq!(hsv_to_rgb(hsv), color);
        }
        // The end of the hue strip is red again.
        assert_eq!(hsv_to_rgb([1., 1., 1.]), Rgb::new(255, 0, 0));
    }

    #[test]
    fn sampled_colors_survive_a_round_trip() {
        let mut seed = 0x2545_f491_u32;
        for _ in 0..1000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let color = Rgb::new((seed >> 24) as u8, (seed >> 16) as u8, (seed >> 8) as u8);
            assert!(close(hsv_to_rgb(rgb_to_hsv(color)), color), "{color}");
        }
    }

    #[test]
    fn hex_colors_are_read() {
        let orange = Some(Rgb::new(0xff, 0x95, 0x00));
        assert_eq!(parse_hex("#ff9500"), orange);
        assert_eq!(parse_hex("FF9500"), orange);
        assert_eq!(parse_hex(" #Ff9500 "), orange);
        for wrong in ["", "#", "#f90", "f90", "#ff95001", "#gg9500", "+f9500", "#ff 500", "auto"] {
            assert_eq!(parse_hex(wrong), None, "{wrong}");
        }
    }

    #[test]
    fn the_square_picks_saturation_and_brightness() {
        let parts = ColorPicker::layout((100., 50.), 1.);
        let square = parts.square;
        assert_eq!((square.x, square.y, square.w, square.h), (112., 62., 224., 144.));
        let mut picker = ColorPicker::new(Some(Rgb::new(255, 0, 0)), SWATCH_COLORS);

        // Top left is white, bottom right black, top right the pure hue.
        assert_eq!(picker.press(&parts, at(square.x, square.y)), Some(Some(WHITE)));
        assert_eq!(picker.drag(&parts, at(square.right(), square.bottom())), Some(BLACK));
        assert_eq!(picker.drag(&parts, at(square.right(), square.y)), Some(Rgb::new(255, 0, 0)));
        // The knob follows, and stays on the square past its edges.
        let middle = square.center();
        picker.drag(&parts, at(middle.0, middle.1));
        assert_eq!(picker.knob(&parts), middle);
        assert_eq!(
            picker.drag(&parts, at(square.right() + 50., square.y - 50.)),
            Some(picker.rgb())
        );
        assert_eq!(picker.knob(&parts), (square.right(), square.y));

        picker.release();
        assert_eq!(picker.drag(&parts, at(square.x, square.y)), None);
    }

    #[test]
    fn the_strip_picks_the_hue() {
        let parts = ColorPicker::layout((0., 0.), 2.);
        let hue = parts.hue;
        let mut picker = ColorPicker::new(Some(Rgb::new(0, 0, 255)), SWATCH_COLORS);
        let y = hue.center().1;
        assert_eq!(picker.press(&parts, at(hue.x, y)), Some(Some(Rgb::new(255, 0, 0))));
        assert_eq!(picker.drag(&parts, at(hue.x + hue.w / 3., y)), Some(Rgb::new(0, 255, 0)));
        assert_eq!(picker.drag(&parts, at(hue.right(), y)), Some(Rgb::new(255, 0, 0)));
        assert_eq!(picker.hue_knob(&parts), (hue.right(), y));
        // Just above the thin strip still takes it.
        assert_eq!(ColorPicker::part_at(&parts, at(hue.x + 1., hue.y - 2.)), Some(Part::Hue));
    }

    #[test]
    fn swatches_and_the_original_are_taken() {
        let parts = ColorPicker::layout((10., 10.), 1.);
        let orange = Rgb::new(0xff, 0x95, 0);
        let mut picker = ColorPicker::new(Some(orange), SWATCH_COLORS);
        let swatch = |index: usize| at(parts.swatches[index].center().0, parts.swatches[index].y);
        assert_eq!(picker.press(&parts, swatch(4)), Some(Some(SWATCH_COLORS[4])));
        assert_eq!(picker.chosen(), Some(4));
        // The first stands for the automatic color, shown as the color it is.
        assert_eq!(picker.press(&parts, swatch(0)), Some(None));
        assert_eq!(
            (picker.color(), picker.rgb(), picker.chosen()),
            (None, SWATCH_COLORS[0], Some(0))
        );
        let last = parts.swatches[SWATCHES - 1];
        assert_eq!(last.right(), parts.square.right());
        let original = parts.original.center();
        assert_eq!(picker.press(&parts, at(original.0, original.1)), Some(Some(orange)));
        // Between the parts nothing is taken.
        let gap = at(parts.swatches[0].right() + 2., parts.swatches[0].center().1);
        assert_eq!(picker.press(&parts, gap), None);
        assert_eq!(picker.color(), Some(orange));
    }

    #[test]
    fn greys_keep_their_hue() {
        let mut picker = ColorPicker::new(Some(Rgb::new(0, 255, 0)), SWATCH_COLORS);
        picker.set(Some(Rgb::new(40, 40, 40)));
        assert!((picker.hsv[0] - 1. / 3.).abs() < 1e-4);
        assert_eq!(picker.rgb(), Rgb::new(40, 40, 40));
        picker.set(Some(BLACK));
        assert_eq!((picker.hsv[0], picker.hsv[1]), (1. / 3., 0.));
    }

    #[test]
    fn arrows_move_through_the_square() {
        let mut picker = ColorPicker::new(Some(Rgb::new(255, 0, 0)), SWATCH_COLORS);
        picker.key(NamedKey::ArrowLeft, true);
        picker.key(NamedKey::ArrowDown, false);
        assert!((picker.hsv[1] - 0.9).abs() < 1e-4 && (picker.hsv[2] - 0.99).abs() < 1e-4);
        assert_eq!(picker.key(NamedKey::ArrowUp, true).map(|_| picker.hsv[2]), Some(1.));
        assert_eq!(picker.key(NamedKey::Enter, false), None);
    }
}

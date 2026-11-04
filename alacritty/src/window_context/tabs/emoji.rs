//! Color glyphs for the interface's text on Windows, such as the emoji programs put in their
//! titles.
//!
//! crossfont draws every glyph with DirectWrite as a mask in one color. Where the interface's font
//! has no glyph for a character and Segoe UI Emoji has a color one, as the system's own text falls
//! back to, its layers are drawn here, each in its color, into one image.

use std::ptr;

use alacritty_tabbed_deps::dwrote::{self, FontFace, GlyphOffset, GlyphRunAnalysis};
use alacritty_tabbed_deps::winapi;
use crossfont::{BitmapBuffer, RasterizedGlyph, Size};
use winapi::Interface;
use winapi::shared::minwindef::BOOL;
use winapi::shared::windef::RECT;
use winapi::shared::winerror::S_OK;
use winapi::um::dcommon::DWRITE_MEASURING_MODE_NATURAL;
use winapi::um::dwrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_GLYPH_RUN, DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
    DWRITE_TEXTURE_CLEARTYPE_3x1, DWriteCreateFactory,
};
use winapi::um::dwrite_2::{
    DWRITE_COLOR_GLYPH_RUN, IDWriteColorGlyphRunEnumerator, IDWriteFactory2,
};
use winapi::um::unknwnbase::IUnknown;

/// Family whose color glyphs are drawn.
const EMOJI: &str = "Segoe UI Emoji";

/// Color of layers drawn in the text's color, which a glyph cached for any color cannot know.
const TEXT_COLOR: [f32; 3] = [0.85, 0.85, 0.85];

pub struct Emoji {
    /// Face of the interface's font, which keeps the characters it has.
    primary: Option<FontFace>,
    emoji: Option<FontFace>,
    factory: Option<Factory>,
}

/// DirectWrite's factory, in the version that splits color glyphs into layers.
struct Factory(*mut IDWriteFactory2);

impl Drop for Factory {
    fn drop(&mut self) {
        unsafe { (*self.0).Release() };
    }
}

/// One layer of a color glyph: its coverage, where that lies, and its color.
struct Layer {
    bounds: RECT,
    coverage: Vec<u8>,
    color: [f32; 4],
}

impl Emoji {
    pub fn new(family: &str) -> Self {
        let face = |family: &str| {
            use dwrote::{FontCollection, FontStretch, FontStyle, FontWeight};
            let family = FontCollection::system().font_family_by_name(family).ok()??;
            let font = family.first_matching_font(
                FontWeight::Regular,
                FontStretch::Normal,
                FontStyle::Normal,
            );
            Some(font.ok()?.create_font_face())
        };
        Self { primary: face(family), emoji: face(EMOJI), factory: Factory::new() }
    }

    /// The color glyph for `character` at `size`, unless the interface's font has the character or
    /// Segoe UI Emoji has no color glyph for it.
    pub fn glyph(&self, character: char, size: Size) -> Option<RasterizedGlyph> {
        if character.is_ascii() || character.is_control() {
            return None;
        }
        let index = |face: &FontFace| {
            face.glyph_indices(&[character as u32]).ok()?.first().copied().filter(|&i| i != 0)
        };
        if self.primary.as_ref().and_then(index).is_some() {
            return None;
        }
        let (emoji, factory) = (self.emoji.as_ref()?, self.factory.as_ref()?);
        let glyph = index(emoji)?;
        let em = size.as_px();
        let layers = factory.layers(emoji, glyph, em)?;
        let (bounds, pixels) = composite(&layers)?;

        let metrics = emoji.design_glyph_metrics(&[glyph], false).ok()?;
        let units = f32::from(emoji.metrics().metrics0().designUnitsPerEm);
        let advance = metrics.first().map_or(0., |m| m.advanceWidth as f32 * em / units);
        Some(RasterizedGlyph {
            character,
            width: bounds.right - bounds.left,
            height: bounds.bottom - bounds.top,
            top: -bounds.top,
            left: bounds.left,
            advance: (advance.round() as i32, 0),
            buffer: BitmapBuffer::Rgba(pixels),
        })
    }
}

impl Factory {
    fn new() -> Option<Self> {
        let mut factory: *mut IUnknown = ptr::null_mut();
        let hr = unsafe {
            DWriteCreateFactory(
                DWRITE_FACTORY_TYPE_SHARED,
                &IDWriteFactory2::uuidof(),
                &mut factory,
            )
        };
        (hr == S_OK && !factory.is_null()).then(|| Self(factory.cast()))
    }

    /// The layers of the glyph `index` of `face`, `em` pixels high, or none if it has no color.
    fn layers(&self, face: &FontFace, index: u16, em: f32) -> Option<Vec<Layer>> {
        let run = DWRITE_GLYPH_RUN {
            fontFace: unsafe { face.as_ptr() },
            fontEmSize: em,
            glyphCount: 1,
            glyphIndices: &index,
            glyphAdvances: &0.,
            glyphOffsets: &GlyphOffset::default(),
            isSideways: 0,
            bidiLevel: 0,
        };
        let mut runs: *mut IDWriteColorGlyphRunEnumerator = ptr::null_mut();
        let hr = unsafe {
            (*self.0).TranslateColorGlyphRun(
                0.,
                0.,
                &run,
                ptr::null(),
                DWRITE_MEASURING_MODE_NATURAL,
                ptr::null(),
                0,
                &mut runs,
            )
        };
        // A glyph without color layers fails with `DWRITE_E_NOCOLOR`.
        if hr != S_OK || runs.is_null() {
            return None;
        }

        let mut layers = Vec::new();
        unsafe {
            loop {
                let mut more: BOOL = 0;
                if (*runs).MoveNext(&mut more) != S_OK || more == 0 {
                    break;
                }
                let mut layer: *const DWRITE_COLOR_GLYPH_RUN = ptr::null();
                if (*runs).GetCurrentRun(&mut layer) != S_OK || layer.is_null() {
                    break;
                }
                let layer = &*layer;
                let color = match layer.paletteIndex {
                    0xFFFF => [TEXT_COLOR[0], TEXT_COLOR[1], TEXT_COLOR[2], 1.],
                    _ => {
                        let c = layer.runColor;
                        [c.r, c.g, c.b, c.a]
                    },
                };
                if let Some(layer) =
                    coverage(&layer.glyphRun, layer.baselineOriginX, layer.baselineOriginY, color)
                {
                    layers.push(layer);
                }
            }
            (*runs).Release();
        }
        Some(layers)
    }
}

/// How much of each pixel the glyphs of `run`, on a baseline at `x`, `y`, cover.
fn coverage(run: &DWRITE_GLYPH_RUN, x: f32, y: f32, color: [f32; 4]) -> Option<Layer> {
    let mode = DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC;
    let analysis =
        GlyphRunAnalysis::create(run, 1., None, mode, DWRITE_MEASURING_MODE_NATURAL, x, y).ok()?;
    let bounds = analysis.get_alpha_texture_bounds(DWRITE_TEXTURE_CLEARTYPE_3x1).ok()?;
    if bounds.right <= bounds.left || bounds.bottom <= bounds.top {
        return None;
    }
    let texture = analysis.create_alpha_texture(DWRITE_TEXTURE_CLEARTYPE_3x1, bounds).ok()?;
    // The three subpixels of each pixel, averaged, cover it in grayscale.
    let coverage = texture
        .chunks_exact(3)
        .map(|rgb| ((u16::from(rgb[0]) + u16::from(rgb[1]) + u16::from(rgb[2])) / 3) as u8)
        .collect();
    Some(Layer { bounds, coverage, color })
}

/// The layers painted over each other, back to front, as pixels with premultiplied alpha, and
/// where they lie together.
fn composite(layers: &[Layer]) -> Option<(RECT, Vec<u8>)> {
    let bounds = layers.iter().map(|layer| layer.bounds).reduce(|a, b| RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    })?;
    let width = (bounds.right - bounds.left) as usize;
    let height = (bounds.bottom - bounds.top) as usize;
    let mut pixels = vec![0f32; width * height * 4];
    for layer in layers {
        let layer_width = (layer.bounds.right - layer.bounds.left) as usize;
        let (dx, dy) =
            ((layer.bounds.left - bounds.left) as usize, (layer.bounds.top - bounds.top) as usize);
        for (row, line) in layer.coverage.chunks_exact(layer_width).enumerate() {
            for (column, &covered) in line.iter().enumerate() {
                let alpha = layer.color[3] * f32::from(covered) / 255.;
                if alpha <= 0. {
                    continue;
                }
                let at = ((dy + row) * width + dx + column) * 4;
                let pixel = &mut pixels[at..at + 4];
                for (value, color) in pixel.iter_mut().zip(layer.color).take(3) {
                    *value = color * alpha + *value * (1. - alpha);
                }
                pixel[3] = alpha + pixel[3] * (1. - alpha);
            }
        }
    }
    let pixels = pixels.into_iter().map(|value| (value.clamp(0., 1.) * 255.).round() as u8);
    Some((bounds, pixels.collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(left: i32, top: i32, coverage: Vec<u8>, width: i32, color: [f32; 4]) -> Layer {
        let height = coverage.len() as i32 / width;
        let bounds = RECT { left, top, right: left + width, bottom: top + height };
        Layer { bounds, coverage, color }
    }

    #[test]
    fn layers_paint_over_each_other_in_their_bounds() {
        let red = layer(0, 0, vec![255, 255], 2, [1., 0., 0., 1.]);
        let blue = layer(1, 0, vec![255, 255], 2, [0., 0., 1., 1.]);
        let (bounds, pixels) = composite(&[red, blue]).unwrap();
        assert_eq!((bounds.left, bounds.right, bounds.top, bounds.bottom), (0, 3, 0, 1));
        assert_eq!(pixels, [255, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 255]);
    }

    #[test]
    fn partial_coverage_is_premultiplied() {
        let half = layer(0, 0, vec![128], 1, [1., 1., 1., 1.]);
        let (_, pixels) = composite(&[half]).unwrap();
        assert_eq!(pixels, [128, 128, 128, 128]);
    }

    #[test]
    fn nothing_to_paint_has_no_image() {
        assert!(composite(&[]).is_none());
    }

    #[test]
    fn segoe_ui_emoji_draws_its_emoji_in_color() {
        let emoji = Emoji::new("Segoe UI");
        if emoji.emoji.is_none() {
            return;
        }
        let glyph = emoji.glyph('\u{1F600}', Size::new(16.)).expect("a color glyph");
        let BitmapBuffer::Rgba(pixels) = glyph.buffer else { panic!("not in color") };
        let colored = pixels.chunks_exact(4).any(|p| p[3] > 0 && (p[0] != p[1] || p[1] != p[2]));
        assert!(colored);
        assert!(glyph.advance.0 > 0);
        // Letters the interface's font has stay in it.
        assert!(emoji.glyph('a', Size::new(16.)).is_none());
    }
}

//! The interface's own font: proportional text for the bar, its menus and the settings, apart from
//! the terminal's font.
//!
//! Glyphs are drawn by the terminal's text renderer into its glyph atlas, each placed as a cell of
//! a grid of single pixels, so text can start at any pixel and advance glyph by glyph.

use std::cell::RefCell;
use std::collections::HashMap;

use crossfont::{
    Error as RasterizerError, FontDesc, FontKey, GlyphKey, Metrics, Rasterize, RasterizedGlyph,
    Rasterizer, Size, Slant, Style, Weight,
};
use log::info;
use unicode_width::UnicodeWidthChar;

use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Flags;

use crate::config::tabbed::UiFont as Config;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::content::RenderableCell;
use crate::renderer::{Glyph, LoadGlyph, Renderer};

use super::platform;

/// Advances of a face in pixels, for systems whose rasterizer does not report them.
pub type Advances = Box<dyn Fn(char) -> Option<f32>>;

/// Families tried in order when none is configured or the configured one is missing; the last is
/// always there.
#[cfg(target_os = "macos")]
const FAMILIES: &[&str] =
    &[".AppleSystemUIFont", "SF Pro Text", "SF Pro", "Helvetica Neue", "Helvetica"];
#[cfg(windows)]
const FAMILIES: &[&str] = &["Segoe UI Variable Text", "Segoe UI", "Arial"];
#[cfg(not(any(target_os = "macos", windows)))]
const FAMILIES: &[&str] = &[
    "Inter",
    "Inter Variable",
    "Adwaita Sans",
    "SF Pro Text",
    "Geist",
    "IBM Plex Sans",
    "Noto Sans",
    "Ubuntu",
    "Cantarell",
    "Roboto",
    "Open Sans",
    "DejaVu Sans",
    "sans-serif",
];

/// Styles for emphasis, lighter than bold, in the names families give them.
const STRONG: [&str; 3] = ["SemiBold", "Semibold", "Medium"];

/// Text placed at `x`, `y` (its top left corner), in a color, and strong or not.
pub type Text = (f32, f32, String, Rgb, bool);

pub struct UiFont {
    /// Family the font was loaded from, as people know it.
    pub family: String,
    /// Height of a line of text.
    pub line: f32,
    /// Distance from the top of a line to its baseline.
    ascent: f32,
    pub metrics: Metrics,
    faces: RefCell<Faces>,
}

struct Faces {
    rasterizer: Rasterizer,
    /// Regular and strong faces.
    keys: [FontKey; 2],
    advances: [Option<Advances>; 2],
    size: Size,
    /// Advance of a space, for systems that measure no glyph without ink.
    space: f32,
    glyphs: HashMap<(char, bool), Measured>,
}

struct Measured {
    advance: f32,
    rasterized: RasterizedGlyph,
    /// Where the glyph lies in the atlas, once loaded.
    loaded: Option<Glyph>,
}

impl UiFont {
    pub fn new(config: &Config, scale: f64) -> Result<Self, RasterizerError> {
        let mut rasterizer = Rasterizer::new()?;
        let size = Size::new(config.size.max(1.)).scale(scale as f32);
        let configured = Some(config.family.trim()).filter(|family| !family.is_empty());
        let installed = |family: &&str| platform::installed(family, None);
        let family = configured
            .into_iter()
            .chain(FAMILIES.iter().copied())
            .find(installed)
            .unwrap_or(FAMILIES[FAMILIES.len() - 1]);

        let regular = Style::Description { slant: Slant::Normal, weight: Weight::Normal };
        let regular = rasterizer.load_font(&FontDesc::new(family, regular), size)?;
        let strong_style =
            STRONG.into_iter().find(|style| platform::installed(family, Some(style)));
        let strong = strong_style
            .and_then(|style| {
                let style = Style::Specific(style.into());
                rasterizer.load_font(&FontDesc::new(family, style), size).ok()
            })
            .or_else(|| {
                let bold = Style::Description { slant: Slant::Normal, weight: Weight::Bold };
                rasterizer.load_font(&FontDesc::new(family, bold), size).ok()
            })
            .unwrap_or(regular);
        info!("Interface font: {family} {}", strong_style.unwrap_or("Bold"));

        // Metrics are only known once the face rasterized a glyph.
        rasterizer.get_glyph(GlyphKey { font_key: regular, character: 'm', size })?;
        let metrics = rasterizer.metrics(regular, size)?;
        let line = metrics.line_height.round() as f32;
        let advances = [
            platform::advances(family, None, false, size),
            platform::advances(family, strong_style, true, size),
        ];
        let faces = Faces {
            rasterizer,
            keys: [regular, strong],
            advances,
            size,
            space: (metrics.average_advance * 0.45) as f32,
            glyphs: HashMap::new(),
        };
        Ok(Self {
            // macOS hides its interface font's real name.
            family: if family.starts_with('.') { "San Francisco".into() } else { family.into() },
            line,
            ascent: (line + metrics.descent).round(),
            metrics,
            faces: RefCell::new(faces),
        })
    }

    /// Width of `text` in pixels.
    pub fn width(&self, text: &str, strong: bool) -> f32 {
        let mut faces = self.faces.borrow_mut();
        text.chars().map(|c| faces.measure(c, strong).advance).sum()
    }

    /// Width of an average lowercase letter.
    pub fn average(&self) -> f32 {
        self.width("abcdefghijklmnopqrstuvwxyz", false) / 26.
    }

    /// Width of each character of `text`, with its byte position.
    pub fn advances(&self, text: &str, strong: bool) -> Vec<(usize, f32)> {
        let mut faces = self.faces.borrow_mut();
        text.char_indices().map(|(i, c)| (i, faces.measure(c, strong).advance)).collect()
    }

    /// `text` cut to `width` pixels, with an ellipsis when it does not fit.
    pub fn fit(&self, text: &str, width: f32, strong: bool) -> String {
        if self.width(text, strong) <= width {
            return text.into();
        }
        let room = width - self.width("…", strong);
        let mut used = 0.;
        let mut fitted: String = text
            .chars()
            .zip(self.advances(text, strong))
            .take_while(|(_, (_, advance))| {
                used += advance;
                used <= room
            })
            .map(|(character, _)| character)
            .collect();
        fitted.truncate(fitted.trim_end().len());
        fitted.push('…');
        fitted
    }

    /// Byte position in `text` nearest to `x` pixels from its start.
    pub fn offset(&self, text: &str, x: f32) -> usize {
        let mut start = 0.;
        for (index, advance) in self.advances(text, false) {
            if x < start + advance / 2. {
                return index;
            }
            start += advance;
        }
        text.len()
    }

    /// Forget where glyphs lie in the atlas, after it was cleared.
    pub fn reset(&self) {
        self.faces.borrow_mut().glyphs.values_mut().for_each(|glyph| glyph.loaded = None);
    }

    /// Draw `texts` over the window, whose size is `window`.
    pub fn draw(&self, renderer: &mut Renderer, window: &SizeInfo, texts: Vec<Text>) {
        if texts.is_empty() {
            return;
        }
        let mut faces = self.faces.borrow_mut();
        let mut cells = Vec::new();
        for (x, y, text, color, strong) in texts {
            let mut pen = x;
            let baseline = (y + self.ascent).round().max(0.) as usize;
            for character in text.chars().filter(|c| !c.is_control()) {
                let measured = faces.measure(character, strong);
                let glyph = *measured.loaded.get_or_insert_with(|| {
                    // Each cell is one pixel high, with the baseline at its bottom.
                    let mut rasterized = measured.rasterized.clone();
                    rasterized.top += 1;
                    renderer.with_loader(|mut api| api.load_glyph(&rasterized))
                });
                let point = Point::new(baseline, Column(pen.round().max(0.) as usize));
                pen += measured.advance;
                let (fg, bg, underline, bg_alpha, flags, extra) =
                    (color, color, color, 0., Flags::empty(), None);
                let cell =
                    RenderableCell { character, point, fg, bg, bg_alpha, underline, flags, extra };
                cells.push((cell, glyph));
            }
        }

        let (width, height) = (window.width(), window.height());
        let grid = SizeInfo::new(width, height, 1., 1., 0., 0., false);
        renderer.resize(&grid);
        renderer.draw_glyphs(&grid, cells.into_iter());
    }
}

impl Faces {
    fn measure(&mut self, character: char, strong: bool) -> &mut Measured {
        let Self { rasterizer, keys, advances, size, space, glyphs } = self;
        glyphs.entry((character, strong)).or_insert_with(|| {
            let key = GlyphKey { font_key: keys[strong as usize], character, size: *size };
            let rasterized = match rasterizer.get_glyph(key) {
                Ok(glyph) | Err(RasterizerError::MissingGlyph(glyph)) => glyph,
                Err(_) => RasterizedGlyph::default(),
            };
            let advance = match rasterized.advance.0 {
                _ if character.is_control() || character.width() == Some(0) => 0.,
                0 => advances[strong as usize].as_ref().and_then(|a| a(character)).unwrap_or(
                    match rasterized.width {
                        0 => *space,
                        width => (2 * rasterized.left.max(0) + width) as f32,
                    },
                ),
                advance => advance as f32,
            };
            Measured { advance, rasterized, loaded: None }
        })
    }
}

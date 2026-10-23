use std::collections::HashMap;

use ahash::RandomState;
use crossfont::{
    Error as RasterizerError, FontDesc, FontKey, GlyphKey, Metrics, Rasterize, RasterizedGlyph,
    Rasterizer, Size, Slant, Style, Weight,
};
use log::{error, info};
use unicode_width::UnicodeWidthChar;

use crate::config::font::{Font, FontDescription};
use crate::config::ui_config::Delta;
use crate::gl::types::*;

use super::builtin_font;

/// `LoadGlyph` allows for copying a rasterized glyph into graphics memory.
pub trait LoadGlyph {
    /// Load the rasterized glyph into GPU memory.
    fn load_glyph(&mut self, rasterized: &RasterizedGlyph) -> Glyph;

    /// Clear any state accumulated from previous loaded glyphs.
    ///
    /// This can, for instance, be used to reset the texture Atlas.
    fn clear(&mut self);
}

#[derive(Copy, Clone, Debug)]
pub struct Glyph {
    pub tex_id: GLuint,
    pub multicolor: bool,
    pub top: i16,
    pub left: i16,
    pub width: i16,
    pub height: i16,
    pub uv_bot: f32,
    pub uv_left: f32,
    pub uv_width: f32,
    pub uv_height: f32,
}

/// Fonts kept loaded at once before a font change clears the atlas, which bounds its growth.
const MAX_FACES: usize = 8;

/// Fonts loaded at once before the atlas is cleared regardless, far more than one frame draws.
const MAX_LOADED: usize = 4 * MAX_FACES;

/// The font keys and metrics of one [`Font`].
#[derive(Copy, Clone, Debug)]
struct Faces {
    regular: FontKey,
    bold: FontKey,
    italic: FontKey,
    bold_italic: FontKey,
    metrics: Metrics,
}

/// Naïve glyph cache.
///
/// Glyphs are keyed by face, size and `char`, so several fonts share the cache and its atlas;
/// [`Self::select`] picks the one the next glyphs are drawn with.
pub struct GlyphCache {
    /// Cache of buffered glyphs.
    cache: HashMap<GlyphKey, Glyph, RandomState>,

    /// Rasterizer for loading new glyphs.
    rasterizer: Rasterizer,

    /// Regular font.
    pub font_key: FontKey,

    /// Bold font.
    pub bold_key: FontKey,

    /// Italic font.
    pub italic_key: FontKey,

    /// Bold italic font.
    pub bold_italic_key: FontKey,

    /// Font size.
    pub font_size: crossfont::Size,

    /// Font offset.
    font_offset: Delta<i8>,

    /// Glyph offset.
    glyph_offset: Delta<i8>,

    /// Font metrics.
    metrics: Metrics,

    /// Whether to use the built-in font for box drawing characters.
    builtin_box_drawing: bool,

    /// Every font loaded since the atlas was last cleared; the selected one is at `active`.
    faces: Vec<(Font, Faces)>,
    active: usize,

    /// Fonts that could not be loaded since the atlas was last cleared, not to be retried.
    failed: Vec<Font>,
}

impl GlyphCache {
    pub fn new(mut rasterizer: Rasterizer, font: &Font) -> Result<GlyphCache, crossfont::Error> {
        let faces = Self::load_faces(&mut rasterizer, font)?;
        Ok(Self {
            cache: Default::default(),
            rasterizer,
            font_size: font.size(),
            font_key: faces.regular,
            bold_key: faces.bold,
            italic_key: faces.italic,
            bold_italic_key: faces.bold_italic,
            font_offset: font.offset,
            glyph_offset: font.glyph_offset,
            metrics: faces.metrics,
            builtin_box_drawing: font.builtin_box_drawing,
            faces: vec![(font.clone(), faces)],
            active: 0,
            failed: Vec::new(),
        })
    }

    /// Load the font keys and metrics of `font`.
    fn load_faces(rasterizer: &mut Rasterizer, font: &Font) -> Result<Faces, crossfont::Error> {
        let (regular, bold, italic, bold_italic) = Self::compute_font_keys(font, rasterizer)?;
        let metrics = Self::load_font_metrics(rasterizer, font, regular)?;
        Ok(Faces { regular, bold, italic, bold_italic, metrics })
    }

    /// The font the next glyphs are drawn with.
    pub fn font(&self) -> &Font {
        &self.faces[self.active].0
    }

    /// Draw the next glyphs with `font`, loading it unless it was used before.
    ///
    /// Returns the index of the font selected before, for [`Self::select_index`], or `None` when
    /// `font` cannot be loaded (logged once) and the selection is unchanged.
    ///
    /// The atlas is kept: glyphs of another size or face have their own keys. Fonts that differ
    /// only in `offset`, `glyph_offset` or `builtin_box_drawing` share keys, so the glyphs of
    /// whichever is drawn first at a size are used for all of them until the atlas is cleared.
    pub fn select(&mut self, font: &Font) -> Option<usize> {
        let previous = self.active;
        if self.font() == font {
            return Some(previous);
        }

        let index = match faces_index(&self.faces, font) {
            Some(index) => index,
            None if self.failed.contains(font) => return None,
            None => match Self::load_faces(&mut self.rasterizer, font) {
                Ok(faces) => {
                    self.faces.push((font.clone(), faces));
                    self.faces.len() - 1
                },
                Err(err) => {
                    error!("Could not load font {:?}: {err}", font.normal().family);
                    self.failed.push(font.clone());
                    return None;
                },
            },
        };

        self.select_index(index);
        Some(previous)
    }

    /// Draw the next glyphs with the font at `index`, as returned by [`Self::select`].
    pub fn select_index(&mut self, index: usize) {
        let (font, faces) = &self.faces[index];
        self.font_size = font.size();
        self.font_key = faces.regular;
        self.bold_key = faces.bold;
        self.italic_key = faces.italic;
        self.bold_italic_key = faces.bold_italic;
        self.metrics = faces.metrics;
        self.font_offset = font.offset;
        self.glyph_offset = font.glyph_offset;
        self.builtin_box_drawing = font.builtin_box_drawing;
        self.active = index;
    }

    /// Whether so many fonts are loaded that the atlas should be cleared before the next frame.
    pub fn full(&self) -> bool {
        self.faces.len() > MAX_LOADED
    }

    // Load font metrics and adjust for glyph offset.
    fn load_font_metrics(
        rasterizer: &mut Rasterizer,
        font: &Font,
        key: FontKey,
    ) -> Result<Metrics, crossfont::Error> {
        // Need to load at least one glyph for the face before calling metrics.
        // The glyph requested here ('m' at the time of writing) has no special
        // meaning.
        rasterizer.get_glyph(GlyphKey { font_key: key, character: 'm', size: font.size() })?;

        let mut metrics = rasterizer.metrics(key, font.size())?;
        metrics.strikeout_position += font.glyph_offset.y as f32;
        Ok(metrics)
    }

    fn load_glyphs_for_font<L: LoadGlyph>(&mut self, font: FontKey, loader: &mut L) {
        let size = self.font_size;

        // Cache all ascii characters.
        for i in 32u8..=126u8 {
            self.get(GlyphKey { font_key: font, character: i as char, size }, loader, true);
        }
    }

    /// Computes font keys for (Regular, Bold, Italic, Bold Italic).
    fn compute_font_keys(
        font: &Font,
        rasterizer: &mut Rasterizer,
    ) -> Result<(FontKey, FontKey, FontKey, FontKey), crossfont::Error> {
        let size = font.size();

        // Load regular font.
        let regular_desc = Self::make_desc(font.normal(), Slant::Normal, Weight::Normal);

        let regular = Self::load_regular_font(rasterizer, &regular_desc, size)?;

        // Helper to load a description if it is not the `regular_desc`.
        let mut load_or_regular = |desc: FontDesc| {
            if desc == regular_desc {
                regular
            } else {
                rasterizer.load_font(&desc, size).unwrap_or(regular)
            }
        };

        // Load bold font.
        let bold_desc = Self::make_desc(&font.bold(), Slant::Normal, Weight::Bold);

        let bold = load_or_regular(bold_desc);

        // Load italic font.
        let italic_desc = Self::make_desc(&font.italic(), Slant::Italic, Weight::Normal);

        let italic = load_or_regular(italic_desc);

        // Load bold italic font.
        let bold_italic_desc = Self::make_desc(&font.bold_italic(), Slant::Italic, Weight::Bold);

        let bold_italic = load_or_regular(bold_italic_desc);

        Ok((regular, bold, italic, bold_italic))
    }

    fn load_regular_font(
        rasterizer: &mut Rasterizer,
        description: &FontDesc,
        size: Size,
    ) -> Result<FontKey, crossfont::Error> {
        match rasterizer.load_font(description, size) {
            Ok(font) => Ok(font),
            Err(err) => {
                error!("{err}");

                let fallback_desc =
                    Self::make_desc(Font::default().normal(), Slant::Normal, Weight::Normal);
                rasterizer.load_font(&fallback_desc, size)
            },
        }
    }

    fn make_desc(desc: &FontDescription, slant: Slant, weight: Weight) -> FontDesc {
        let style = if let Some(ref spec) = desc.style {
            Style::Specific(spec.to_owned())
        } else {
            Style::Description { slant, weight }
        };
        FontDesc::new(desc.family.clone(), style)
    }

    /// Get a glyph from the font.
    ///
    /// If the glyph has never been loaded before, it will be rasterized and inserted into the
    /// cache.
    ///
    /// # Errors
    ///
    /// This will fail when the glyph could not be rasterized. Usually this is due to the glyph
    /// not being present in any font.
    pub fn get<L>(&mut self, glyph_key: GlyphKey, loader: &mut L, show_missing: bool) -> Glyph
    where
        L: LoadGlyph + ?Sized,
    {
        // Try to load glyph from cache.
        if let Some(glyph) = self.cache.get(&glyph_key) {
            return *glyph;
        };

        // Rasterize the glyph using the built-in font for special characters or the user's font
        // for everything else.
        let rasterized = self
            .builtin_box_drawing
            .then(|| {
                builtin_font::builtin_glyph(
                    glyph_key.character,
                    &self.metrics,
                    &self.font_offset,
                    &self.glyph_offset,
                )
            })
            .flatten()
            .map_or_else(|| self.rasterizer.get_glyph(glyph_key), Ok);

        let glyph = match rasterized {
            Ok(rasterized) => self.load_glyph(loader, rasterized),
            // Load fallback glyph.
            Err(RasterizerError::MissingGlyph(rasterized)) if show_missing => {
                // Use `\0` as "missing" glyph to cache it only once.
                let missing_key = GlyphKey { character: '\0', ..glyph_key };
                if let Some(glyph) = self.cache.get(&missing_key) {
                    *glyph
                } else {
                    // If no missing glyph was loaded yet, insert it as `\0`.
                    let glyph = self.load_glyph(loader, rasterized);
                    self.cache.insert(missing_key, glyph);

                    glyph
                }
            },
            Err(_) => self.load_glyph(loader, Default::default()),
        };

        // Cache rasterized glyph.
        *self.cache.entry(glyph_key).or_insert(glyph)
    }

    /// Load glyph into the atlas.
    ///
    /// This will apply all transforms defined for the glyph cache to the rasterized glyph before
    pub fn load_glyph<L>(&self, loader: &mut L, mut glyph: RasterizedGlyph) -> Glyph
    where
        L: LoadGlyph + ?Sized,
    {
        glyph.left += i32::from(self.glyph_offset.x);
        glyph.top += i32::from(self.glyph_offset.y);
        glyph.top -= self.metrics.descent as i32;

        // The metrics of zero-width characters are based on rendering
        // the character after the current cell, with the anchor at the
        // right side of the preceding character. Since we render the
        // zero-width characters inside the preceding character, the
        // anchor has been moved to the right by one cell.
        if glyph.character.width() == Some(0) {
            glyph.left += self.metrics.average_advance as i32;
        }

        // Add glyph to cache.
        loader.load_glyph(&glyph)
    }

    /// Reset currently cached data in both GL and the registry to default state.
    pub fn reset_glyph_cache<L: LoadGlyph>(&mut self, loader: &mut L) {
        loader.clear();
        self.cache = Default::default();
        self.faces.swap(0, self.active);
        self.faces.truncate(1);
        self.active = 0;
        self.failed.clear();

        self.load_common_glyphs(loader);
    }

    /// Change the window's font.
    ///
    /// Returns whether the cached glyphs must be dropped with [`Self::reset_glyph_cache`]: when
    /// they were drawn with other offsets, or when too many fonts were loaded since the last reset.
    pub fn update_font(&mut self, font: &Font) -> bool {
        let clear = needs_clear(self.font(), font)
            || self
                .faces
                .iter()
                .any(|(face, _)| face.size() == font.size() && needs_clear(face, font));
        if self.select(font).is_none() {
            return false;
        }

        info!("Font size changed to {:?} px", font.size().as_px());

        clear || self.faces.len() > MAX_FACES
    }

    pub fn font_metrics(&self) -> crossfont::Metrics {
        self.metrics
    }

    /// Prefetch glyphs that are almost guaranteed to be loaded anyways.
    pub fn load_common_glyphs<L: LoadGlyph>(&mut self, loader: &mut L) {
        self.load_glyphs_for_font(self.font_key, loader);
        self.load_glyphs_for_font(self.bold_key, loader);
        self.load_glyphs_for_font(self.italic_key, loader);
        self.load_glyphs_for_font(self.bold_italic_key, loader);
    }
}

/// The index of `font` in `faces`.
fn faces_index<T>(faces: &[(Font, T)], font: &Font) -> Option<usize> {
    faces.iter().position(|(face, _)| face == font)
}

/// Whether glyphs cached for `old` would be wrong for `new` under the same keys.
fn needs_clear(old: &Font, new: &Font) -> bool {
    old.offset != new.offset
        || old.glyph_offset != new.glyph_offset
        || old.builtin_box_drawing != new.builtin_box_drawing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(size: f32) -> Font {
        Font::default().with_size(Size::new(size))
    }

    #[test]
    fn faces_index_matches_whole_font() {
        let faces = [(font(11.), ()), (font(14.), ())];
        assert_eq!(faces_index(&faces, &font(14.)), Some(1));
        assert_eq!(faces_index(&faces, &font(12.)), None);

        let mut offset = font(11.);
        offset.glyph_offset = Delta { x: 1, y: 0 };
        assert_eq!(faces_index(&faces, &offset), None);
    }

    #[test]
    fn needs_clear_only_for_baked_settings() {
        assert!(!needs_clear(&font(11.), &font(11.)));
        assert!(!needs_clear(&font(11.), &font(22.)));

        let family: Font = toml::from_str("normal = { family = \"Other Mono\" }").unwrap();
        assert!(!needs_clear(&Font::default(), &family));

        let mut glyph_offset = font(11.);
        glyph_offset.glyph_offset = Delta { x: 0, y: 2 };
        assert!(needs_clear(&font(11.), &glyph_offset));

        let mut offset = font(11.);
        offset.offset = Delta { x: 1, y: 0 };
        assert!(needs_clear(&font(11.), &offset));

        let mut builtin = font(11.);
        builtin.builtin_box_drawing = false;
        assert!(needs_clear(&font(11.), &builtin));
    }
}

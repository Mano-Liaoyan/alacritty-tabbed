use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fmt, ptr};

use ahash::RandomState;
use crossfont::Metrics;
use glutin::context::{ContextApi, GlContext, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay, GlDisplay};
use log::{LevelFilter, debug, info};
use unicode_width::UnicodeWidthChar;

use alacritty_terminal::index::Point;
use alacritty_terminal::term::cell::Flags;

use crate::config::debug::RendererPreference;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::content::RenderableCell;
use crate::gl;
use crate::renderer::rects::{RectRenderer, RenderRect};
use crate::renderer::shader::ShaderError;

pub mod platform;
pub mod rects;
mod shader;
mod text;

pub use text::glyph_cache::{Glyph, LoadGlyph};
pub use text::{GlyphCache, LoaderApi};

use shader::ShaderVersion;
use text::{Gles2Renderer, Glsl3Renderer, TextRenderApi, TextRenderer};

/// Whether the OpenGL functions have been loaded.
pub static GL_FUNS_LOADED: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub enum Error {
    /// Shader error.
    Shader(ShaderError),

    /// Other error.
    Other(String),
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Shader(err) => err.source(),
            Error::Other(_) => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Shader(err) => {
                write!(f, "There was an error initializing the shaders: {err}")
            },
            Error::Other(err) => {
                write!(f, "{err}")
            },
        }
    }
}

impl From<ShaderError> for Error {
    fn from(val: ShaderError) -> Self {
        Error::Shader(val)
    }
}

impl From<String> for Error {
    fn from(val: String) -> Self {
        Error::Other(val)
    }
}

#[derive(Debug)]
enum TextRendererProvider {
    Gles2(Gles2Renderer),
    Glsl3(Glsl3Renderer),
}

#[derive(Debug)]
pub struct Renderer {
    text_renderer: TextRendererProvider,
    rect_renderer: RectRenderer,
    robustness: bool,
    frame: Option<Frame>,
}

/// Where a terminal is drawn in the window, which its `SizeInfo` is stretched over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Left edge, in window pixels.
    pub x: f32,
    /// Bottom edge, in window pixels from the window's bottom, as OpenGL counts them.
    pub y_bottom: f32,
    /// Scale from the terminal's size to the frame.
    pub sx: f32,
    pub sy: f32,
    /// Scissor rectangle `[x, y, width, height]` outside which nothing is drawn.
    pub clip: [i32; 4],
}

impl Frame {
    /// The window's bottom left corner, unscaled and unclipped, where terminals are drawn
    /// without a frame.
    const ORIGIN: Frame = Frame { x: 0., y_bottom: 0., sx: 1., sy: 1., clip: [0; 4] };

    /// Frame stretching `size` over `[x, y, width, height]`, in window pixels from the top left
    /// of a window `window_height` pixels tall.
    #[cfg_attr(not(test), expect(dead_code, reason = "split panes are not drawn yet"))]
    pub fn new(size: &SizeInfo, [x, y, width, height]: [f32; 4], window_height: f32) -> Self {
        let y_bottom = window_height - y - height;
        let (sx, sy) = (width / size.width(), height / size.height());
        let frame = Self { x, y_bottom, sx, sy, clip: [0; 4] };
        Self { clip: frame.area(size), ..frame }
    }

    /// Viewport `[x, y, width, height]` of the whole of `size`, padding included, with its edges
    /// rounded to whole pixels.
    fn area(&self, size: &SizeInfo) -> [i32; 4] {
        let (left, bottom) = (self.x.round(), self.y_bottom.round());
        let right = (self.x + size.width() * self.sx).round();
        let top = (self.y_bottom + size.height() * self.sy).round();
        [left, bottom, right - left, top - bottom].map(|value| value as i32)
    }
}

/// Viewport `[x, y, width, height]` of the cells of `size`, drawn in `frame`.
fn viewport(size: &SizeInfo, frame: &Frame) -> [i32; 4] {
    let (padding_x, padding_y) =
        ((size.padding_x() * frame.sx) as i32, (size.padding_y() * frame.sy) as i32);
    let [x, y, width, height] = frame.area(size);
    [x + padding_x, y + padding_y, width - 2 * padding_x, height - 2 * padding_y]
}

/// Wrapper around gl::GetString with error checking and reporting.
fn gl_get_string(
    string_id: gl::types::GLenum,
    description: &str,
) -> Result<Cow<'static, str>, Error> {
    unsafe {
        let string_ptr = gl::GetString(string_id);
        match gl::GetError() {
            gl::NO_ERROR if !string_ptr.is_null() => {
                Ok(CStr::from_ptr(string_ptr as *const _).to_string_lossy())
            },
            gl::INVALID_ENUM => {
                Err(format!("OpenGL error requesting {description}: invalid enum").into())
            },
            error_id => Err(format!("OpenGL error {error_id} requesting {description}").into()),
        }
    }
}

impl Renderer {
    /// Create a new renderer.
    ///
    /// This will automatically pick between the GLES2 and GLSL3 renderer based on the GPU's
    /// supported OpenGL version.
    pub fn new(
        context: &PossiblyCurrentContext,
        renderer_preference: Option<RendererPreference>,
    ) -> Result<Self, Error> {
        // We need to load OpenGL functions once per instance, but only after we make our context
        // current due to WGL limitations.
        if !GL_FUNS_LOADED.swap(true, Ordering::Relaxed) {
            let gl_display = context.display();
            gl::load_with(|symbol| {
                let symbol = CString::new(symbol).unwrap();
                gl_display.get_proc_address(symbol.as_c_str()).cast()
            });
        }

        let shader_version = gl_get_string(gl::SHADING_LANGUAGE_VERSION, "shader version")?;
        let gl_version = gl_get_string(gl::VERSION, "OpenGL version")?;
        let renderer = gl_get_string(gl::RENDERER, "renderer version")?;

        info!("Running on {renderer}");
        info!("OpenGL version {gl_version}, shader_version {shader_version}");

        // Check if robustness is supported.
        let robustness = Self::supports_robustness();

        let is_gles_context = matches!(context.context_api(), ContextApi::Gles(_));

        // Use the config option to enforce a particular renderer configuration.
        let (use_glsl3, allow_dsb) = match renderer_preference {
            Some(RendererPreference::Glsl3) => (true, true),
            Some(RendererPreference::Gles2) => (false, true),
            Some(RendererPreference::Gles2Pure) => (false, false),
            None => (shader_version.as_ref() >= "3.3" && !is_gles_context, true),
        };

        let (text_renderer, rect_renderer) = if use_glsl3 {
            let text_renderer = TextRendererProvider::Glsl3(Glsl3Renderer::new()?);
            let rect_renderer = RectRenderer::new(ShaderVersion::Glsl3)?;
            (text_renderer, rect_renderer)
        } else {
            let text_renderer =
                TextRendererProvider::Gles2(Gles2Renderer::new(allow_dsb, is_gles_context)?);
            let rect_renderer = RectRenderer::new(ShaderVersion::Gles2)?;
            (text_renderer, rect_renderer)
        };

        // Enable debug logging for OpenGL as well.
        if log::max_level() >= LevelFilter::Debug && GlExtensions::contains("GL_KHR_debug") {
            debug!("Enabled debug logging for OpenGL");
            unsafe {
                gl::Enable(gl::DEBUG_OUTPUT);
                gl::Enable(gl::DEBUG_OUTPUT_SYNCHRONOUS);
                gl::DebugMessageCallback(Some(gl_debug_log), ptr::null_mut());
            }
        }

        Ok(Self { text_renderer, rect_renderer, robustness, frame: None })
    }

    pub fn draw_cells<I: Iterator<Item = RenderableCell>>(
        &mut self,
        size_info: &SizeInfo,
        glyph_cache: &mut GlyphCache,
        cells: I,
    ) {
        match &mut self.text_renderer {
            TextRendererProvider::Gles2(renderer) => {
                renderer.draw_cells(size_info, glyph_cache, cells)
            },
            TextRendererProvider::Glsl3(renderer) => {
                renderer.draw_cells(size_info, glyph_cache, cells)
            },
        }
    }

    /// Draw glyphs already loaded into the atlas, each placed as in the cell it comes with.
    pub fn draw_glyphs<I: Iterator<Item = (RenderableCell, Glyph)>>(
        &mut self,
        size_info: &SizeInfo,
        glyphs: I,
    ) {
        match &mut self.text_renderer {
            TextRendererProvider::Gles2(renderer) => renderer.with_api(size_info, |mut api| {
                glyphs.for_each(|(cell, glyph)| api.add_render_item(&cell, &glyph, size_info))
            }),
            TextRendererProvider::Glsl3(renderer) => renderer.with_api(size_info, |mut api| {
                glyphs.for_each(|(cell, glyph)| api.add_render_item(&cell, &glyph, size_info))
            }),
        }
    }

    /// Draw a string in a variable location. Used for printing the render timer, warnings and
    /// errors.
    pub fn draw_string(
        &mut self,
        point: Point<usize>,
        fg: Rgb,
        bg: Rgb,
        string_chars: impl Iterator<Item = char>,
        size_info: &SizeInfo,
        glyph_cache: &mut GlyphCache,
    ) {
        let mut wide_char_spacer = false;
        let cells = string_chars.enumerate().filter_map(|(i, character)| {
            let flags = if wide_char_spacer {
                wide_char_spacer = false;
                return None;
            } else if character.width() == Some(2) {
                // The spacer is always following the wide char.
                wide_char_spacer = true;
                Flags::WIDE_CHAR
            } else {
                Flags::empty()
            };

            Some(RenderableCell {
                point: Point::new(point.line, point.column + i),
                character,
                extra: None,
                flags,
                bg_alpha: 1.0,
                fg,
                bg,
                underline: fg,
            })
        });

        self.draw_cells(size_info, glyph_cache, cells);
    }

    pub fn with_loader<F, T>(&mut self, func: F) -> T
    where
        F: FnOnce(LoaderApi<'_>) -> T,
    {
        match &mut self.text_renderer {
            TextRendererProvider::Gles2(renderer) => renderer.with_loader(func),
            TextRendererProvider::Glsl3(renderer) => renderer.with_loader(func),
        }
    }

    /// Draw all rectangles simultaneously to prevent excessive program swaps.
    pub fn draw_rects(&mut self, size_info: &SizeInfo, metrics: &Metrics, rects: Vec<RenderRect>) {
        let blend = [gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA, gl::SRC_ALPHA, gl::ONE];
        self.draw_rects_blended(size_info, metrics, rects, blend);
    }

    /// Multiply what was drawn under the rectangles, alpha included, by their alpha, which makes
    /// the window transparent where it is zero.
    pub fn mask_rects(&mut self, size_info: &SizeInfo, metrics: &Metrics, rects: Vec<RenderRect>) {
        let blend = [gl::ZERO, gl::SRC_ALPHA, gl::ZERO, gl::SRC_ALPHA];
        self.draw_rects_blended(size_info, metrics, rects, blend);
    }

    /// Draw rectangles with the factors of `glBlendFuncSeparate`.
    fn draw_rects_blended(
        &mut self,
        size_info: &SizeInfo,
        metrics: &Metrics,
        rects: Vec<RenderRect>,
        [src_rgb, dst_rgb, src_alpha, dst_alpha]: [u32; 4],
    ) {
        if rects.is_empty() {
            return;
        }

        // Prepare rect rendering state.
        let [x, y, width, height] = self.frame.unwrap_or(Frame::ORIGIN).area(size_info);
        unsafe {
            // Remove padding from viewport.
            gl::Viewport(x, y, width, height);
            gl::BlendFuncSeparate(src_rgb, dst_rgb, src_alpha, dst_alpha);
        }

        self.rect_renderer.draw(size_info, metrics, rects, [x as f32, y as f32]);

        // Activate regular state again.
        unsafe {
            // Reset blending strategy.
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);

            // Restore viewport with padding.
            self.set_viewport(size_info);
        }
    }

    /// Draw terminals into `frame`, or as they fill the window without one.
    ///
    /// Takes effect on the viewport with the next [`Self::set_viewport`] or [`Self::resize`].
    pub fn set_frame(&mut self, frame: Option<Frame>) {
        self.frame = frame;
        Self::scissor(frame);
    }

    /// Fill `frame` with `color` and `alpha`.
    pub fn clear_rect(&self, frame: &Frame, color: Rgb, alpha: f32) {
        Self::scissor(Some(*frame));
        self.clear(color, alpha);
        Self::scissor(self.frame);
    }

    /// Clip drawing to `frame`, or stop clipping without one.
    fn scissor(frame: Option<Frame>) {
        unsafe {
            match frame {
                Some(Frame { clip: [x, y, width, height], .. }) => {
                    gl::Scissor(x, y, width, height);
                    gl::Enable(gl::SCISSOR_TEST);
                },
                None => gl::Disable(gl::SCISSOR_TEST),
            }
        }
    }

    /// Fill the window, or only the frame's clip, with `color` and `alpha`.
    pub fn clear(&self, color: Rgb, alpha: f32) {
        unsafe {
            gl::ClearColor(
                (f32::from(color.r) / 255.0).min(1.0) * alpha,
                (f32::from(color.g) / 255.0).min(1.0) * alpha,
                (f32::from(color.b) / 255.0).min(1.0) * alpha,
                alpha,
            );
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }
    }

    /// Get the context reset status.
    pub fn was_context_reset(&self) -> bool {
        // If robustness is not supported, don't use its functions.
        if !self.robustness {
            return false;
        }

        let status = unsafe { gl::GetGraphicsResetStatus() };
        if status == gl::NO_ERROR {
            false
        } else {
            let reason = match status {
                gl::GUILTY_CONTEXT_RESET_KHR => "guilty",
                gl::INNOCENT_CONTEXT_RESET_KHR => "innocent",
                gl::UNKNOWN_CONTEXT_RESET_KHR => "unknown",
                _ => "invalid",
            };

            info!("GPU reset ({reason})");

            true
        }
    }

    fn supports_robustness() -> bool {
        let mut notification_strategy = 0;
        if GlExtensions::contains("GL_KHR_robustness") {
            unsafe {
                gl::GetIntegerv(gl::RESET_NOTIFICATION_STRATEGY_KHR, &mut notification_strategy);
            }
        } else {
            notification_strategy = gl::NO_RESET_NOTIFICATION_KHR as gl::types::GLint;
        }

        if notification_strategy == gl::LOSE_CONTEXT_ON_RESET_KHR as gl::types::GLint {
            info!("GPU reset notifications are enabled");
            true
        } else {
            info!("GPU reset notifications are disabled");
            false
        }
    }

    pub fn finish(&self) {
        unsafe {
            gl::Finish();
        }
    }

    /// Set the viewport for cell rendering.
    #[inline]
    pub fn set_viewport(&self, size: &SizeInfo) {
        let [x, y, width, height] = viewport(size, &self.frame.unwrap_or(Frame::ORIGIN));
        unsafe {
            gl::Viewport(x, y, width, height);
        }
    }

    /// Resize the renderer.
    pub fn resize(&self, size_info: &SizeInfo) {
        self.set_viewport(size_info);
        match &self.text_renderer {
            TextRendererProvider::Gles2(renderer) => renderer.resize(size_info),
            TextRendererProvider::Glsl3(renderer) => renderer.resize(size_info),
        }
    }
}

struct GlExtensions;

impl GlExtensions {
    /// Check if the given `extension` is supported.
    ///
    /// This function will lazily load OpenGL extensions.
    fn contains(extension: &str) -> bool {
        static OPENGL_EXTENSIONS: OnceLock<HashSet<&'static str, RandomState>> = OnceLock::new();

        OPENGL_EXTENSIONS.get_or_init(Self::load_extensions).contains(extension)
    }

    /// Load available OpenGL extensions.
    fn load_extensions() -> HashSet<&'static str, RandomState> {
        unsafe {
            let extensions = gl::GetString(gl::EXTENSIONS);

            if extensions.is_null() {
                let mut extensions_number = 0;
                gl::GetIntegerv(gl::NUM_EXTENSIONS, &mut extensions_number);

                (0..extensions_number as gl::types::GLuint)
                    .flat_map(|i| {
                        let extension = CStr::from_ptr(gl::GetStringi(gl::EXTENSIONS, i) as *mut _);
                        extension.to_str()
                    })
                    .collect()
            } else {
                match CStr::from_ptr(extensions as *mut _).to_str() {
                    Ok(ext) => ext.split_whitespace().collect(),
                    Err(_) => Default::default(),
                }
            }
        }
    }
}

extern "system" fn gl_debug_log(
    _: gl::types::GLenum,
    _: gl::types::GLenum,
    _: gl::types::GLuint,
    _: gl::types::GLenum,
    _: gl::types::GLsizei,
    msg: *const gl::types::GLchar,
    _: *mut std::os::raw::c_void,
) {
    let msg = unsafe { CStr::from_ptr(msg).to_string_lossy() };
    debug!("[gl_render] {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> SizeInfo {
        SizeInfo::new(800., 600., 10., 20., 5., 7., false)
    }

    #[test]
    fn viewport_without_frame() {
        let size = size();
        assert_eq!(viewport(&size, &Frame::ORIGIN), [5, 7, 790, 586]);
        assert_eq!(Frame::ORIGIN.area(&size), [0, 0, 800, 600]);
    }

    #[test]
    fn viewport_of_terminal_below_bar() {
        // Framed where it is drawn without a frame: below a 40 pixels tall bar.
        let size = size();
        let frame = Frame::new(&size, [0., 40., 800., 600.], 640.);
        assert_eq!(frame, Frame { clip: [0, 0, 800, 600], ..Frame::ORIGIN });
        assert_eq!(viewport(&size, &frame), viewport(&size, &Frame::ORIGIN));
    }

    #[test]
    fn viewport_offset() {
        // Right half of a 1600x640 window.
        let size = size();
        let frame = Frame::new(&size, [800., 40., 800., 600.], 640.);
        assert_eq!(viewport(&size, &frame), [805, 7, 790, 586]);
        assert_eq!(frame.area(&size), [800, 0, 800, 600]);

        // Top half of a 800x1240 window.
        let frame = Frame::new(&size, [0., 40., 800., 600.], 1240.);
        assert_eq!(viewport(&size, &frame), [5, 607, 790, 586]);
        assert_eq!(frame.area(&size), [0, 600, 800, 600]);
    }

    #[test]
    fn viewport_scaled() {
        // Popping in at 85 % about the centre of the window.
        let size = size();
        let frame = Frame::new(&size, [60., 45., 680., 510.], 600.);
        assert_eq!((frame.sx, frame.sy), (0.85, 0.85));
        assert_eq!(viewport(&size, &frame), [64, 50, 672, 500]);
        assert_eq!(frame.area(&size), [60, 45, 680, 510]);
    }

    #[test]
    fn clip() {
        let size = size();
        let frame = Frame::new(&size, [10.4, 20.6, 99.5, 50.2], 100.);
        assert_eq!(frame.clip, [10, 29, 100, 50]);
    }

    #[test]
    fn clip_matches_area_between_pixels() {
        // Halfway through a slide, edges rounded alike so neighbours neither overlap nor gap.
        let size = size();
        let frame = Frame::new(&size, [10.5, 20., 10.5, 50.], 100.);
        assert_eq!(frame.clip, [11, 30, 10, 50]);
        assert_eq!(frame.area(&size), frame.clip);
        let next = Frame::new(&size, [21., 20., 10.5, 50.], 100.);
        assert_eq!(next.clip[0], frame.clip[0] + frame.clip[2]);
    }
}

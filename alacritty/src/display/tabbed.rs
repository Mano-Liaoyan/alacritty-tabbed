//! Drawing a window of several terminals, with its tab bar, for the [`Display`].
//!
//! The display draws its own terminal with [`Display::draw_terminal`], into a frame or over the
//! whole window, any other panes with [`Display::draw_pane`], and shows it all with the tab bar
//! with [`Display::present`].

use std::mem;
use std::num::NonZeroU32;
use std::time::Instant;

use log::debug;
use parking_lot::MutexGuard;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::NamedColor;

use crate::config::UiConfig;
use crate::config::font::Font;
use crate::display::color::{List, Rgb};
use crate::display::content::RenderableContent;
use crate::display::cursor::IntoRects;
use crate::display::hint::HintState;
use crate::display::{Display, SizeInfo, compute_cell_size};
use crate::event::SearchState;
use crate::message_bar::MessageBuffer;
use crate::renderer::Frame;
use crate::renderer::rects::RenderLines;
use crate::scheduler::Scheduler;

impl Display {
    /// Draw the screen.
    ///
    /// A reference to Term whose state is being drawn must be provided.
    ///
    /// This call may block if vsync is enabled.
    pub fn draw<T: EventListener>(
        &mut self,
        terminal: MutexGuard<'_, Term<T>>,
        scheduler: &mut Scheduler,
        message_buffer: &MessageBuffer,
        config: &UiConfig,
        search_state: &mut SearchState,
    ) {
        self.draw_terminal(terminal, message_buffer, config, search_state, None);
        self.present(scheduler, config);
    }

    /// Start drawing a window of several terminals, clearing all of it.
    ///
    /// Not needed when the window's own terminal is drawn without a frame, which clears it.
    pub fn begin(&mut self, config: &UiConfig) {
        // Make sure this window's OpenGL context is active.
        self.make_current();

        self.renderer.set_frame(None);
        self.renderer.clear(config.colors.primary.background, config.window_opacity());
    }

    /// Clear where [`Self::draw_terminal`] draws the terminal: `frame`, or the whole window
    /// without one.
    ///
    /// A [`Frame`] is created from where the terminal goes in the window, with
    /// [`Frame::new`]`(&display.size_info, [x, y, width, height], window_height)`.
    pub(super) fn clear_frame(&mut self, frame: Option<Frame>, background: Rgb, config: &UiConfig) {
        self.renderer.set_frame(frame);
        self.renderer.set_viewport(&self.size_info);
        match &frame {
            Some(frame) => {
                self.tabs.full_damage = true;
                self.renderer.clear_rect(frame, background, config.window_opacity());
            },
            None => self.renderer.clear(background, config.window_opacity()),
        }
    }

    /// Draw a terminal other than the window's own, which is `size` large, into `frame`.
    ///
    /// Only its cells, lines and cursor are drawn, in the colors of `config` and with `font`.
    pub fn draw_pane<T: EventListener>(
        &mut self,
        terminal: MutexGuard<'_, Term<T>>,
        size: SizeInfo,
        config: &UiConfig,
        frame: Frame,
        font: &Font,
    ) {
        // Draw as if the terminal were the window's own, without its hints or input method.
        let colors = List::from(&config.colors);
        let own = (
            mem::replace(&mut self.size_info, size),
            mem::replace(&mut self.colors, colors),
            mem::replace(&mut self.hint_state, HintState::new("")),
            mem::take(&mut self.ime),
            mem::take(&mut self.cursor_hidden),
        );

        let mut search_state = SearchState::default();
        let mut content = RenderableContent::new(config, self, &terminal, &mut search_state);
        let mut lines = RenderLines::new();
        let cells: Vec<_> = (&mut content).inspect(|cell| lines.update(cell)).collect();
        let background = content.color(NamedColor::Background as usize);
        let cursor = content.cursor();

        // Free the terminal's lock before drawing.
        drop(terminal);

        self.tabs.full_damage = true;
        self.renderer.set_frame(Some(frame));
        self.renderer.resize(&size);
        self.renderer.clear_rect(&frame, background, config.window_opacity());
        self.with_font(font, |display| {
            display.renderer.draw_cells(&size, &mut display.glyph_cache, cells.into_iter());

            let metrics = display.glyph_cache.font_metrics();
            let mut rects = lines.rects(&metrics, &size);
            rects.extend(cursor.rects(&size, config.cursor.thickness()));
            display.renderer.draw_rects(&size, &metrics, rects);
        });

        (self.size_info, self.colors, self.hint_state, self.ime, self.cursor_hidden) = own;
        self.renderer.set_frame(None);
        self.renderer.resize(&self.size_info);
    }

    /// Draw the tab bar over everything drawn since [`Self::begin`], and the pane previewed from
    /// it, returning the window font's metrics.
    pub(super) fn draw_tabs(&mut self, config: &UiConfig) -> crossfont::Metrics {
        let metrics = self.glyph_cache.font_metrics();
        let size_info = self.size_info;

        // The cells of a preview's font, which its card's size depends on.
        let now = Instant::now();
        if let Some((font, pane)) = self.tabs.peek_font(now) {
            let cells = self.cell_size(&pane, &font);
            self.tabs.set_peek_cells(font, cells);
        }

        self.renderer.set_frame(None);
        self.renderer.set_viewport(&size_info);
        self.tabs.draw(&mut self.renderer, &self.window, &size_info, config);

        // A pane previewed from its icon, over the bar's card that holds it.
        if let Some(peek) = self.tabs.peek(now) {
            let height = self.window.inner_size().height as f32;
            let (rect, clip) = (peek.frame, peek.clip);
            let frame = Frame::new(&peek.size, [rect.x, rect.y, rect.w, rect.h], height)
                .clipped([clip.x, clip.y, clip.w, clip.h], height);
            let mut terminal = peek.terminal.lock();
            terminal.is_focused = false;
            self.draw_pane(terminal, peek.size, &peek.config, frame, &peek.font);
            self.tabs.draw_over(&mut self.renderer, &size_info, config);
        }

        metrics
    }

    /// Mark the next frame fully damaged after drawing into frames, since drawing without them
    /// next must still replace what was drawn into them.
    pub(super) fn damage_frames(&mut self) {
        if mem::take(&mut self.tabs.full_damage) {
            self.damage_tracker.next_frame().mark_fully_damaged();
        }
    }

    /// Size of the surface, which is the whole window, holding the tab bar and every pane.
    pub(super) fn surface_size(&self) -> (NonZeroU32, NonZeroU32) {
        let size = self.window.inner_size();
        (NonZeroU32::new(size.width.max(1)).unwrap(), NonZeroU32::new(size.height.max(1)).unwrap())
    }

    /// Forget the tab bar's glyphs, after the glyph cache was cleared.
    pub(super) fn reset_tab_glyphs(&mut self) {
        debug!("Clearing the glyph cache");
        self.tabs.reset_glyphs();
    }

    /// Update the window's font and cell dimensions, keeping the glyphs of other sizes.
    ///
    /// This will return a tuple of the cell width and height.
    pub(super) fn update_font(&mut self, config: &UiConfig, font: &Font) -> (f32, f32) {
        if self.glyph_cache.update_font(font) {
            self.pending_renderer_update.get_or_insert(Default::default()).clear_font_cache = true;
        }

        // Compute new cell sizes.
        compute_cell_size(config, &self.glyph_cache.font_metrics())
    }

    /// Cell width and height of `font`, with the cell offsets of `config`.
    pub fn cell_size(&mut self, config: &UiConfig, font: &Font) -> (f32, f32) {
        self.with_font(font, |display| {
            compute_cell_size(config, &display.glyph_cache.font_metrics())
        })
    }

    /// Run `f` with `font` selected in the glyph cache, then select the window's font again.
    fn with_font<R>(&mut self, font: &Font, f: impl FnOnce(&mut Self) -> R) -> R {
        let window = self.glyph_cache.select(font);

        // Bound the atlas, clearing it before the next frame rather than in the middle of this one.
        if self.glyph_cache.full() {
            self.pending_renderer_update.get_or_insert(Default::default()).clear_font_cache = true;
        }

        let result = f(self);

        if let Some(window) = window {
            self.glyph_cache.select_index(window);
        }

        result
    }
}

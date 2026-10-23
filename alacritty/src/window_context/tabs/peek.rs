//! The card shown when an icon of a pane is hovered: the pane's own title, and a live preview of
//! a pane that is not on screen.
//!
//! Seams only for now: the card comes with the bar's own work.
#![allow(dead_code, unused_variables)]

use std::time::Instant;

use winit::window::WindowId;

use crate::config::UiConfig;
use crate::display::SizeInfo;
use crate::renderer::Renderer;

use super::anim::Tween;
use super::bar::Layout;
use super::paint::{Layer, Palette, Rect};
use super::{PaneDraw, Tabs};

/// The hovered icon's card, updated as it is drawn.
pub struct State {
    /// The tab and pane whose icon is hovered.
    target: Option<(usize, usize)>,
    /// When the pointer came to rest on it.
    since: Instant,
    /// How shown the card is, from 0 to 1.
    shown: Tween<f32>,
    /// Until when another icon shows its card at once.
    warm_until: Option<Instant>,
}

impl Default for State {
    fn default() -> Self {
        Self { target: None, since: Instant::now(), shown: Tween::new(0.), warm_until: None }
    }
}

impl State {
    /// Whether the card waits to show or fades at `now`, which needs another frame.
    pub fn running(&self, now: Instant) -> bool {
        self.shown.running(now)
    }
}

impl Tabs {
    /// The hovered pane that is not on screen, drawn into its card's thumbnail.
    pub fn peek(&self, now: Instant) -> Option<PaneDraw> {
        None
    }

    /// Draw over the thumbnail, once its terminal is drawn: its corners and hairline.
    pub fn draw_over(&self, renderer: &mut Renderer, size: &SizeInfo, config: &UiConfig) {}
}

/// Draw the card, without the thumbnail's terminal.
pub fn draw(tabs: &Tabs, layout: &Layout, palette: &Palette, now: Instant) -> Option<Layer> {
    None
}

/// Whether the pane `id` is previewed, so its terminal's updates redraw the window.
pub fn shows(tabs: &Tabs, id: WindowId) -> bool {
    false
}

/// Where a card of `content` size goes below `anchor`, inside `window`.
fn card(anchor: Rect, content: (f32, f32), window: Rect, layout: &Layout) -> Rect {
    anchor
}

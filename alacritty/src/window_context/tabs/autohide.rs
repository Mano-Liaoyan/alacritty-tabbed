//! The tab bar of a window whose bar is not pinned: hidden above the panes, it is shown over them
//! as the pointer reaches the top edge, and hidden again once it leaves the bar.
//!
//! Stub of the foundation: the bar is shown and hidden at once; lane B times and slides it.

use std::time::Instant;

use winit::dpi::PhysicalPosition;

use super::Tabs;
use super::bar::BAR;

/// Space below the bar the pointer may leave it by and still keep it shown, in logical pixels.
const BAND: f32 = 12.;

/// Whether the bar is shown.
pub struct State {
    /// Whether the bar is always shown, which the configuration decides.
    pub pinned: bool,
    revealed: bool,
}

impl Default for State {
    fn default() -> Self {
        Self { pinned: true, revealed: false }
    }
}

impl State {
    /// Whether the bar is shown, and its controls take the pointer.
    pub fn live(&self) -> bool {
        self.pinned || self.revealed
    }

    /// How far the bar of height `bar` is moved up out of the window at `now`.
    pub fn offset(&self, bar: f32, _now: Instant) -> f32 {
        if self.live() { 0. } else { -bar }
    }

    /// Whether the bar is still moving, or waiting to, at `now`.
    pub fn running(&self, _now: Instant) -> bool {
        false
    }
}

/// Follow the pointer to `point`: the top edge shows the bar, which stays while the pointer is on
/// it or just below it, or while something on the bar is `hold`ing it. Returns whether that
/// changed.
pub fn moved(tabs: &mut Tabs, point: PhysicalPosition<f64>, hold: bool, _now: Instant) -> bool {
    let (scale, y) = (tabs.scale as f32, point.y as f32);
    let edge = (6. * scale).round().max(tabs.margin());
    let band = ((BAR + BAND) * scale).round();
    let revealed = hold || y < edge || tabs.autohide.revealed && y < band;
    reveal(tabs, revealed)
}

/// The pointer left the window, which hides the bar. Returns whether that changed.
pub fn left(tabs: &mut Tabs, _now: Instant) -> bool {
    reveal(tabs, false)
}

/// Show the bar for a moment, after a tab was opened, closed or selected from the keyboard.
pub fn flash(tabs: &mut Tabs, _now: Instant) {
    reveal(tabs, true);
}

fn reveal(tabs: &mut Tabs, revealed: bool) -> bool {
    let state = &mut tabs.autohide;
    let changed = !state.pinned && state.revealed != revealed;
    state.revealed = revealed;
    changed
}

//! The tab bar of a window whose bar is not pinned: hidden above the panes, it slides in over them
//! as the pointer rests at the top edge, and slides out again a moment after it leaves the bar.
//!
//! The bar lies over the panes rather than pushing them down, so no terminal is resized as it
//! comes and goes.

use std::time::{Duration, Instant};

use winit::dpi::PhysicalPosition;

use super::anim::{Curve, Tween};
use super::bar::BAR;
use super::{Frame, Tabs};

/// Height of the strip at the top edge the pointer rests in to bring the bar, in logical pixels:
/// enough to stop in, on a screen edge or under the menu bar.
const EDGE: f32 = 10.;
/// Space below the bar the pointer may leave it by and still keep it shown, in logical pixels.
const BAND: f32 = 12.;
/// How long the pointer rests at the top edge before the bar comes, so crossing the edge on the
/// way to another window does not flash it.
const DWELL: Duration = Duration::from_millis(150);
/// How long the bar stays after the pointer left it.
const LINGER: Duration = Duration::from_millis(450);
/// How long a tab opened, closed or selected from the keyboard shows the bar.
const FLASH: Duration = Duration::from_millis(1200);

/// Whether the bar is shown, and where it is on its way in or out.
pub struct State {
    /// Whether the bar is always shown, which the configuration decides.
    pub pinned: bool,
    /// Whether the bar is shown, or sliding in.
    revealed: bool,
    /// Whether the pointer is on the bar or just below it.
    band: bool,
    /// Whether something on the bar is open, which keeps it.
    hold: bool,
    /// Since when the pointer rests at the top edge.
    near_since: Option<Instant>,
    /// When the pointer left the bar.
    left_at: Option<Instant>,
    /// Until when a keyboard action shows the bar.
    flash_until: Option<Instant>,
    /// How far the bar is in, from 0 hidden to 1 shown.
    slide: Tween<f32>,
    /// Whether the last frames drew the bar, which their damage must cover: the last, then the
    /// one before.
    drawn: [bool; 2],
}

impl Default for State {
    fn default() -> Self {
        Self {
            pinned: true,
            revealed: false,
            band: false,
            hold: false,
            near_since: None,
            left_at: None,
            flash_until: None,
            slide: Tween::new(0.),
            drawn: [false; 2],
        }
    }
}

impl State {
    /// Whether the bar is shown, and its controls take the pointer.
    pub fn live(&self) -> bool {
        self.pinned || self.revealed
    }

    /// How far the bar of height `bar` is moved up out of the window at `now`.
    pub fn offset(&self, bar: f32, now: Instant) -> f32 {
        if self.pinned { 0. } else { (-bar * (1. - self.slide.get(now))).round() }
    }

    /// Whether the bar is entirely out of the window at `now`, which hides what rides on it.
    pub fn gone(&self, now: Instant) -> bool {
        !self.pinned && self.slide.get(now) <= 0.
    }

    /// Whether the bar is still moving, or waiting to, at `now`.
    pub fn running(&self, now: Instant) -> bool {
        let waiting = match self.revealed {
            false => self.near_since.is_some(),
            true => {
                !self.band && !self.hold && self.left_at.is_some() || self.flash_until.is_some()
            },
        };
        !self.pinned && (waiting || self.slide.running(now))
    }

    /// Whether frames must be drawn whole, as the bar lies over the panes: while it is shown, was
    /// shown a frame ago, or moves.
    pub fn overlay(&self, now: Instant) -> bool {
        !self.pinned && (self.revealed || self.drawn.contains(&true) || self.running(now))
    }

    /// Note whether the frame being drawn shows the bar.
    pub fn drawn(&mut self, shown: bool) {
        self.drawn = [shown, self.drawn[0]];
    }

    /// Show or hide the bar as its timers say at `now`. Returns whether that changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.flash_until.is_some_and(|until| now >= until) {
            self.flash_until = None;
        }
        // A pinned bar is shown, though it is kept as if not under a pointer on it, so it stays
        // there once unpinned from its own button.
        let revealed = if self.pinned { self.band || self.hold } else { self.target(now) };
        if revealed == self.revealed {
            return false;
        }
        self.revealed = revealed;
        match (self.pinned, revealed) {
            (true, _) => self.slide = Tween::new(f32::from(revealed)),
            (_, true) => self.slide.to(1., now, 220, Curve::EASE_OUT),
            (_, false) => self.slide.to(0., now, 180, Curve::EASE_IN),
        }
        !self.pinned
    }

    /// Whether the bar should be shown at `now`: for a moment after a keyboard action, once the
    /// pointer rested at the top edge, and then while the pointer is on it or only just left it,
    /// or something open keeps it; what is open over the panes never brings it.
    fn target(&self, now: Instant) -> bool {
        let rested = self.near_since.is_some_and(|since| now >= since + DWELL);
        let lingers = self.left_at.is_some_and(|at| now < at + LINGER);
        let flashing = self.flash_until.is_some_and(|until| now < until);
        flashing || rested || self.revealed && (self.band || self.hold || lingers)
    }
}

/// Follow the pointer to `point`: resting at the top edge shows the bar, which stays while the
/// pointer is on it or just below it, or while something on the bar is `hold`ing it. Returns
/// whether the bar changed or is waiting to.
pub fn moved(tabs: &mut Tabs, point: PhysicalPosition<f64>, hold: bool, now: Instant) -> bool {
    let (scale, y) = (tabs.scale as f32, point.y as f32);
    let edge = (EDGE * scale).round().max(tabs.margin());
    let band = ((BAR + BAND) * scale).round();
    let state = &mut tabs.autohide;
    let since = state.near_since.unwrap_or(now);
    state.near_since = (y < edge).then_some(since);
    (state.band, state.hold) = (y < band, hold);
    state.left_at = match state.band || hold {
        true => None,
        false => state.left_at.or(Some(now)),
    };
    state.tick(now) | state.running(now)
}

/// The pointer left the window, which hides the bar a moment later. Returns whether the bar
/// changed or is waiting to.
///
/// A maximized or fullscreen window has nothing above it but the menu bar or the screen's edge:
/// leaving it from near its top is resting at the top edge, which brings the bar and keeps it.
pub fn left(tabs: &mut Tabs, now: Instant) -> bool {
    let top = (BAR * tabs.scale as f32).round();
    let above = tabs.frame != Frame::Normal && (tabs.pointer.y as f32) < top;
    let state = &mut tabs.autohide;
    if above {
        state.near_since = state.near_since.or(Some(now));
        (state.band, state.left_at) = (true, None);
    } else {
        (state.near_since, state.band) = (None, false);
        state.left_at = state.left_at.or(Some(now));
    }
    state.tick(now) | state.running(now)
}

/// Show the bar for a moment, after a tab was opened, closed or selected from the keyboard.
pub fn flash(tabs: &mut Tabs, now: Instant) {
    let state = &mut tabs.autohide;
    state.flash_until = Some(now + FLASH);
    state.tick(now);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn unpinned() -> State {
        State { pinned: false, ..State::default() }
    }

    #[test]
    fn resting_at_the_edge_shows_the_bar_after_a_moment() {
        let now = Instant::now();
        let mut state = State { near_since: Some(now), band: true, ..unpinned() };
        assert!(!state.target(now + ms(149)));
        assert!(state.running(now + ms(149)));
        assert!(state.target(now + DWELL));
        // Crossing the edge without resting there shows nothing.
        state.near_since = None;
        assert!(!state.target(now + ms(500)));
        assert!(!state.running(now + ms(500)));
    }

    #[test]
    fn bar_stays_under_the_pointer_and_lingers_after_it_left() {
        let now = Instant::now();
        let shown = State { revealed: true, band: true, ..unpinned() };
        assert!(shown.target(now));
        let left = State { band: false, left_at: Some(now), ..shown };
        assert!(left.target(now + ms(449)));
        assert!(left.running(now + ms(449)));
        assert!(!left.target(now + LINGER));
        // The band below the bar keeps it only once it is shown.
        let hidden = State { revealed: false, band: true, ..unpinned() };
        assert!(!hidden.target(now));
    }

    #[test]
    fn flash_and_hold_show_the_bar() {
        let now = Instant::now();
        let flashed = State { flash_until: Some(now + FLASH), ..unpinned() };
        assert!(flashed.target(now + ms(1199)));
        assert!(!flashed.target(now + FLASH));
        // Something open keeps a shown bar, but a pane's menu does not bring a hidden one.
        let held = State { hold: true, revealed: true, left_at: Some(now), ..unpinned() };
        assert!(held.target(now + ms(10_000)));
        let hidden = State { hold: true, ..unpinned() };
        assert!(!hidden.target(now));
    }

    #[test]
    fn bar_slides_in_and_out_to_its_ends() {
        let now = Instant::now();
        let mut state = State { near_since: Some(now), ..unpinned() };
        assert_eq!(state.offset(48., now), -48.);
        assert!(state.tick(now + DWELL));
        let start = now + DWELL;
        assert!(state.live());
        assert_eq!(state.offset(48., start + ms(10_000)), 0.);

        (state.near_since, state.band, state.left_at) = (None, false, Some(start + ms(300)));
        assert!(!state.tick(start + ms(700)));
        assert!(state.tick(start + ms(750)));
        assert!(!state.live());
        assert_eq!(state.offset(48., start + ms(10_000)), -48.);
    }

    #[test]
    fn leaving_a_maximized_window_by_its_top_rests_at_the_edge() {
        let now = Instant::now();
        let tabs = |frame| {
            let autohide = unpinned();
            let mut tabs = Tabs { scale: 2., frame, autohide, ..Tabs::default() };
            // The last place the pointer was seen, rushing to the menu bar.
            assert!(!moved(&mut tabs, PhysicalPosition::new(300., 60.), false, now));
            tabs.pointer = PhysicalPosition::new(300., 60.);
            tabs
        };

        // The bar comes without the pointer moving again, and stays while it is above.
        let mut maximized = tabs(Frame::Maximized);
        assert!(left(&mut maximized, now));
        assert!(maximized.autohide.running(now + ms(100)));
        assert!(maximized.autohide.tick(now + DWELL));
        assert!(!maximized.autohide.tick(now + ms(10_000)));
        assert!(maximized.autohide.live());

        // A window with something above it is left for that.
        let mut normal = tabs(Frame::Normal);
        assert!(!left(&mut normal, now));
        assert!(!normal.autohide.tick(now + DWELL));

        // Resting a few pixels from the top brings it too.
        let mut tabs = tabs(Frame::Normal);
        assert!(moved(&mut tabs, PhysicalPosition::new(300., 18.), false, now));
        assert!(tabs.autohide.tick(now + DWELL));
    }

    #[test]
    fn bar_is_gone_only_once_out_of_the_window() {
        let now = Instant::now();
        let mut state = State { near_since: Some(now), ..unpinned() };
        assert!(state.gone(now));
        state.tick(now + DWELL);
        assert!(!state.gone(now + DWELL + ms(10)));
        (state.near_since, state.band, state.left_at) = (None, false, Some(now + DWELL));
        let start = now + DWELL + LINGER;
        state.tick(start);
        assert!(!state.gone(start + ms(10)));
        assert!(state.gone(start + ms(10_000)));
        assert!(!State::default().gone(now));
    }

    #[test]
    fn pinned_bar_never_moves() {
        let now = Instant::now();
        let mut state = State { near_since: Some(now), ..State::default() };
        assert!(state.live());
        assert!(!state.tick(now + DWELL));
        assert_eq!(state.offset(48., now), 0.);
        assert!(!state.running(now));
        assert!(!state.overlay(now));
    }

    #[test]
    fn frames_are_drawn_whole_one_frame_past_the_bar() {
        let now = Instant::now();
        let mut state = unpinned();
        state.drawn(true);
        state.drawn(false);
        assert!(state.overlay(now));
        state.drawn(false);
        assert!(!state.overlay(now));
    }
}

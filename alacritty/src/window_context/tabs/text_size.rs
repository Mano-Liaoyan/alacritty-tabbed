//! The live pane's text size: made larger or smaller by keys, the wheel or the menu, within
//! bounds of its configured size, and shown over the pane when it changes.

use std::time::Instant;

use crossfont::Size as FontSize;
use winit::event::MouseScrollDelta;
use winit::keyboard::ModifiersState;

use crate::input::FONT_SIZE_STEP;
use crate::window_context::WindowContext;

use super::redraw;

/// Modifiers that zoom the focused pane's text with the wheel, exactly; macOS zooms the screen
/// with Control.
#[cfg(not(target_os = "macos"))]
pub const ZOOM_WHEEL: ModifiersState = ModifiersState::CONTROL;
#[cfg(target_os = "macos")]
pub const ZOOM_WHEEL: ModifiersState = ModifiersState::SUPER;

/// Logical pixels a touchpad scrolls for each step of a text zoom.
const ZOOM_PIXELS: f64 = 24.;
/// Smallest and largest text of a zoomed pane, relative to its configured size, and smallest in
/// points.
pub const ZOOM_RANGE: (f32, f32) = (0.5, 4.);
pub const ZOOM_MIN_POINTS: f32 = 4.;

/// A change of the live pane's text size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontChange {
    /// Larger or smaller by steps.
    By(i32),
    /// Back to the size of its configuration.
    Reset,
}

/// Give the live pane text of `size`, in the window's pixels.
pub fn set_live_font(ctx: &mut WindowContext, size: FontSize) {
    if ctx.display.font_size != size {
        ctx.display.font_size = size;
        ctx.display.pending_update.set_font(ctx.config.font.clone().with_size(size));
        note_font(ctx);
        redraw(ctx);
    }
}

/// Make the live pane's text larger or smaller, within its bounds, or as configured again.
pub fn zoom_live(ctx: &mut WindowContext, change: FontChange) {
    let scale = ctx.display.window.scale_factor as f32;
    let font = &ctx.config.font;
    let min = FontSize::new(ZOOM_MIN_POINTS).scale(scale).as_px();
    let size = zoomed(ctx.display.font_size, change, font.size().scale(scale), min);
    set_live_font(ctx, size);
}

/// Text of size `current` after `change`, for text `configured` at that size, never below `min`
/// pixels: a reset gives back the configured size exactly, which pixels would not round-trip.
fn zoomed(current: FontSize, change: FontChange, configured: FontSize, min: f32) -> FontSize {
    match change {
        FontChange::By(steps) => {
            FontSize::from_px(zoomed_px(current.as_px(), steps, configured.as_px(), min))
        },
        FontChange::Reset => configured,
    }
}

/// Size in pixels of text of `current` pixels made larger by `steps`, or smaller by negative
/// ones, within [`ZOOM_RANGE`] of its `configured` size and never below `min`.
fn zoomed_px(current: f32, steps: i32, configured: f32, min: f32) -> f32 {
    let (low, high) = (ZOOM_RANGE.0 * configured, ZOOM_RANGE.1 * configured);
    // Whole pixels, which text looks best at, like the keys that zoom it.
    let px = current.round() + steps as f32 * FONT_SIZE_STEP;
    px.clamp(low.max(min), high.max(min))
}

/// Whole steps of a text zoom that the wheel moved by `delta`, keeping what is short of a step in
/// `rest`: a line a step, or [`ZOOM_PIXELS`] logical pixels of a touchpad at `scale`. Turning the
/// wheel back starts afresh.
pub fn wheel_steps(delta: MouseScrollDelta, scale: f64, rest: &mut f64) -> i32 {
    let steps = match delta {
        MouseScrollDelta::LineDelta(_, lines) => f64::from(lines),
        MouseScrollDelta::PixelDelta(delta) => delta.y / (ZOOM_PIXELS * scale),
    };
    if steps * *rest < 0. {
        *rest = 0.;
    }
    *rest += steps;
    let whole = rest.trunc();
    *rest -= whole;
    whole as i32
}

/// Show the live pane's text size over it when it changed, by any key or the wheel, since last
/// seen.
pub fn note_font(ctx: &mut WindowContext) {
    let scale = ctx.display.window.scale_factor as f32;
    let configured = ctx.config.font.size().scale(scale).as_px();
    let percent = (100. * ctx.display.font_size.as_px() / configured).round() as u16;
    let tabs = &mut ctx.display.tabs;
    let live = (tabs.live_id(ctx.first), percent);
    if tabs.live_font.replace(live).is_some_and(|(id, old)| id == live.0 && old != percent) {
        tabs.font_hud = Some((live.0, percent, Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use winit::dpi::PhysicalPosition;

    use super::*;

    #[test]
    fn reset_text_is_exactly_as_configured() {
        // Sizes whose pixels do not round-trip to the same size.
        for (points, scale) in [(11.4, 1.), (12.1, 1.), (10.9, 2.), (11.25, 1.)] {
            let configured = FontSize::new(points).scale(scale);
            let larger = zoomed(configured, FontChange::By(3), configured, 4.);
            assert_ne!(larger, configured);
            assert_eq!(zoomed(larger, FontChange::Reset, configured, 4.), configured, "{points}");
        }
    }

    #[test]
    fn zoomed_text_stays_in_bounds() {
        // Whole pixels, a step each.
        assert_eq!(zoomed_px(16., 1, 16., 5.), 17.);
        assert_eq!(zoomed_px(16.4, -3, 16., 5.), 13.);
        // From half to four times as large as configured.
        assert_eq!(zoomed_px(9., -5, 16., 5.), 8.);
        assert_eq!(zoomed_px(60., 10, 16., 5.), 64.);
        // Never smaller than the smallest text.
        assert_eq!(zoomed_px(8., -5, 8., 5.), 5.);
        assert_eq!(zoomed_px(8., 0, 8., 5.), 8.);
    }

    #[test]
    fn wheel_zooms_by_whole_steps() {
        let mut rest = 0.;
        let lines = |y| MouseScrollDelta::LineDelta(0., y);
        let pixels = |y| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0., y));
        assert_eq!(wheel_steps(lines(1.), 1., &mut rest), 1);
        assert_eq!(wheel_steps(lines(-2.), 1., &mut rest), -2);
        // A touchpad steps every 24 pixels, keeping the rest.
        assert_eq!(wheel_steps(pixels(20.), 1., &mut rest), 0);
        assert_eq!(wheel_steps(pixels(20.), 1., &mut rest), 1);
        assert_eq!(wheel_steps(pixels(5.), 1., &mut rest), 0);
        // Turning back starts afresh.
        assert_eq!(wheel_steps(pixels(-20.), 1., &mut rest), 0);
        assert_eq!(wheel_steps(pixels(-5.), 1., &mut rest), -1);
        assert_eq!(wheel_steps(lines(0.5), 1., &mut rest), 0);
        assert_eq!(wheel_steps(lines(0.5), 1., &mut rest), 1);
        // Logical pixels: twice as many on a screen of twice the scale.
        assert_eq!(wheel_steps(pixels(30.), 2., &mut rest), 0);
        assert_eq!(wheel_steps(pixels(20.), 2., &mut rest), 1);
    }
}

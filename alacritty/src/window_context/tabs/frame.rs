//! How the window is shown on its screen: normal, maximized or fullscreen, as the platform knows
//! or as the window covers its monitor, and what its restore button does then.

use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::window::Window as WinitWindow;

/// How the window is shown, which decides its resize border and the corners it cuts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Frame {
    #[default]
    Normal,
    Maximized,
    /// Fills the screen; like maximized, without resize borders.
    Fullscreen,
}

/// How `window` lies on its screen, to draw it. Fullscreen also when it covers its whole monitor
/// unmaximized: X11 window managers make windows fullscreen without the application knowing,
/// which Wayland tells it.
pub fn frame(window: &WinitWindow) -> Frame {
    let covering = || {
        let monitor = window.current_monitor();
        let screen = monitor.as_ref().map(|monitor| (monitor.position(), monitor.size()));
        covers(window.outer_position().ok().map(|position| (position, window.outer_size())), screen)
    };
    frame_of(window.fullscreen().is_some(), window.is_maximized(), covering)
}

/// The frame of a window fullscreen or maximized as the platform knows, else fullscreen when it
/// is `covering` its monitor; maximized windows cover it too where nothing else is on screen.
fn frame_of(fullscreen: bool, maximized: bool, covering: impl FnOnce() -> bool) -> Frame {
    match (fullscreen, maximized) {
        (true, _) => Frame::Fullscreen,
        (_, true) => Frame::Maximized,
        _ if covering() => Frame::Fullscreen,
        _ => Frame::Normal,
    }
}

/// What the restore button of a window drawn in `frame` does, when the platform knows it to be
/// `fullscreen` or not.
#[derive(Debug, PartialEq, Eq)]
pub enum Restore {
    LeaveFullscreen,
    /// Leave a fullscreen the window manager made, which the platform did not see.
    LeaveOtherFullscreen,
    ToggleMaximized,
}

pub fn restore(frame: Frame, fullscreen: bool) -> Restore {
    match (fullscreen, frame) {
        (true, _) => Restore::LeaveFullscreen,
        (false, Frame::Fullscreen) => Restore::LeaveOtherFullscreen,
        _ => Restore::ToggleMaximized,
    }
}

/// Whether a window at `outer` is exactly the monitor at `screen`, both known.
fn covers(
    outer: Option<(PhysicalPosition<i32>, PhysicalSize<u32>)>,
    screen: Option<(PhysicalPosition<i32>, PhysicalSize<u32>)>,
) -> bool {
    outer.is_some() && outer == screen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_covering_its_monitor_is_fullscreen() {
        let at = |x, y, w, h| Some((PhysicalPosition::new(x, y), PhysicalSize::new(w, h)));
        let screen = at(1920, 0, 2560, 1440);
        assert!(covers(at(1920, 0, 2560, 1440), screen));
        // Maximized below a panel, or the right size on another monitor, is not.
        assert!(!covers(at(1920, 32, 2560, 1408), screen));
        assert!(!covers(at(0, 0, 2560, 1440), screen));
        // Unknown places, as on Wayland, which reports fullscreen itself.
        assert!(!covers(None, None));
        assert!(!covers(None, screen));
    }

    #[test]
    fn frames_and_what_restore_does() {
        use Frame::*;
        // What the platform knows wins; covering the monitor counts only unmaximized.
        for (fullscreen, maximized, covering, frame) in [
            (true, false, true, Fullscreen),
            (true, true, true, Fullscreen),
            (false, true, true, Maximized),
            (false, true, false, Maximized),
            (false, false, true, Fullscreen),
            (false, false, false, Normal),
        ] {
            assert_eq!(frame_of(fullscreen, maximized, || covering), frame);
        }
        assert_eq!(restore(Fullscreen, true), Restore::LeaveFullscreen);
        assert_eq!(restore(Fullscreen, false), Restore::LeaveOtherFullscreen);
        assert_eq!(restore(Maximized, false), Restore::ToggleMaximized);
        assert_eq!(restore(Normal, false), Restore::ToggleMaximized);
    }
}

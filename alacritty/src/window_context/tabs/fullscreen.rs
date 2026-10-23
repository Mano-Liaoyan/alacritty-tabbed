//! Fullscreen of the tabbed window.
//!
//! On Windows winit keeps a window maximized when it enters fullscreen, re-applying
//! `SW_MAXIMIZE` on every style change, and its `WM_NCCALCSIZE` handler limits a maximized
//! undecorated window's client area to the monitor's work area, leaving the taskbar visible. A
//! maximized window is therefore restored before entering fullscreen and maximized again on
//! leaving it.
//!
//! Done through winit alone that shows as several steps: `SW_RESTORE` plays the restore
//! animation down to the window's normal size, then the window jumps to cover the monitor; on
//! leaving it jumps to its normal size and `SW_MAXIMIZE` animates it back up. Instead the window
//! is restored straight onto the monitor with `SetWindowPlacement`, and maximized again in one
//! placement which also gives it back its own normal position, with the DWM's transitions
//! disabled meanwhile, so either way is a single switch between the two sizes.

use std::cell::Cell;

use winit::window::Window;

/// What the window was before it went fullscreen, for when it leaves.
#[derive(Debug, Default)]
pub struct FullscreenState {
    /// The window is to be maximized when it leaves fullscreen.
    maximized: Cell<bool>,
    /// Where the window restores to from maximized, put aside while fullscreen.
    #[cfg(windows)]
    normal: Cell<Option<Rect>>,
}

impl FullscreenState {
    /// Enter or leave fullscreen, returning whether that is done, or is left to the caller.
    pub fn set(&self, window: &Window, fullscreen: bool) -> bool {
        #[cfg(windows)]
        windows::set(self, window, fullscreen);
        let _ = (window, fullscreen);
        cfg!(windows)
    }

    /// Keep a window's maximized state for when it leaves fullscreen, returning whether it is
    /// kept, or is left to the caller to set now: a window maximized while fullscreen on Windows
    /// would not cover the taskbar.
    pub fn set_maximized(&self, window: &Window, maximized: bool) -> bool {
        let keep = cfg!(windows) && window.fullscreen().is_some();
        if keep {
            self.maximized.set(maximized);
        }
        keep
    }

    /// Whether the window is maximized, or will be again when it leaves fullscreen.
    pub fn is_maximized(&self, window: &Window) -> bool {
        window.is_maximized() || self.maximized.get()
    }
}

/// What to do around entering or leaving fullscreen, for a window that keeps its maximized state
/// aside while fullscreen.
#[cfg(any(windows, test))]
#[derive(Debug, Default, PartialEq, Eq)]
struct Steps {
    /// Restore the window before entering fullscreen.
    unmaximize: bool,
    /// Maximize the window after leaving fullscreen.
    maximize: bool,
    /// Whether the window is to be maximized when it leaves fullscreen.
    remember: bool,
}

/// The steps to make a window `fullscreen` or not, which is `maximized` now or `remembered` to
/// have been before it went fullscreen.
#[cfg(any(windows, test))]
fn steps(fullscreen: bool, maximized: bool, remembered: bool) -> Steps {
    if fullscreen {
        let remember = maximized || remembered;
        Steps { unmaximize: maximized, maximize: false, remember }
    } else {
        Steps { unmaximize: false, maximize: remembered, remember: false }
    }
}

/// A rectangle in screen pixels.
#[cfg(any(windows, test))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// The `monitor` rectangle in the workspace coordinates of window placements, which are screen
/// coordinates shifted by how far the monitor's `work` area starts from its top left, as when a
/// taskbar is docked on the left or top. Should this be off, winit's move onto the monitor
/// afterwards still puts the window right.
#[cfg(any(windows, test))]
fn in_workspace(monitor: Rect, work: Rect) -> Rect {
    let (dx, dy) = (work.left - monitor.left, work.top - monitor.top);
    Rect {
        left: monitor.left - dx,
        top: monitor.top - dy,
        right: monitor.right - dx,
        bottom: monitor.bottom - dy,
    }
}

#[cfg(windows)]
mod windows {
    use std::mem;

    use alacritty_tabbed_deps::windows_sys;
    use windows_sys::Win32::Foundation::{BOOL, HWND, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowPlacement, SW_SHOWMAXIMIZED, SW_SHOWNORMAL, SetWindowPlacement, WINDOWPLACEMENT,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::{Fullscreen, Window};

    use super::{FullscreenState, Rect, in_workspace, steps};

    /// Enter or leave borderless fullscreen in a single switch.
    pub fn set(state: &FullscreenState, window: &Window, fullscreen: bool) {
        let steps = steps(fullscreen, window.is_maximized(), state.maximized.get());
        state.maximized.set(steps.remember);

        let hwnd = hwnd(window);
        let _transitions = hwnd.map(TransitionsOff::new);

        // Restore straight onto the monitor, so winit's move there afterwards changes nothing.
        if steps.unmaximize {
            state.normal.set(hwnd.and_then(restore_onto_monitor));
            window.set_maximized(false);
        }

        window.set_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));

        // Winit put the window back where it was when entering, restored onto the monitor.
        if !fullscreen {
            if let (Some(hwnd), Some(normal)) = (hwnd, state.normal.take()) {
                place(hwnd, normal, steps.maximize);
            }
        }
        if steps.maximize {
            window.set_maximized(true);
        }
    }

    /// The window's handle.
    fn hwnd(window: &Window) -> Option<HWND> {
        match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
            _ => None,
        }
    }

    /// Restore a maximized window to cover its monitor, returning its own normal position.
    fn restore_onto_monitor(hwnd: HWND) -> Option<Rect> {
        let mut placement = placement(hwnd)?;
        let normal = rect(placement.rcNormalPosition);

        let mut info =
            MONITORINFO { cbSize: mem::size_of::<MONITORINFO>() as u32, ..unsafe { mem::zeroed() } };
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return None;
        }

        placement.flags = 0;
        placement.showCmd = SW_SHOWNORMAL as u32;
        placement.rcNormalPosition = win_rect(in_workspace(rect(info.rcMonitor), rect(info.rcWork)));
        unsafe { SetWindowPlacement(hwnd, &placement) };
        Some(normal)
    }

    /// Give the window its `normal` position back, `maximized` or at it.
    fn place(hwnd: HWND, normal: Rect, maximized: bool) {
        let Some(mut placement) = placement(hwnd) else { return };
        placement.flags = 0;
        placement.showCmd = if maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL } as u32;
        placement.rcNormalPosition = win_rect(normal);
        unsafe { SetWindowPlacement(hwnd, &placement) };
    }

    fn placement(hwnd: HWND) -> Option<WINDOWPLACEMENT> {
        let mut placement = WINDOWPLACEMENT {
            length: mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..unsafe { mem::zeroed() }
        };
        (unsafe { GetWindowPlacement(hwnd, &mut placement) } != 0).then_some(placement)
    }

    fn rect(rect: RECT) -> Rect {
        Rect { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom }
    }

    fn win_rect(rect: Rect) -> RECT {
        RECT { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom }
    }

    /// The DWM's minimize, maximize and restore animations, disabled while this lives.
    struct TransitionsOff(HWND);

    impl TransitionsOff {
        fn new(hwnd: HWND) -> Self {
            disable_transitions(hwnd, true);
            Self(hwnd)
        }
    }

    impl Drop for TransitionsOff {
        fn drop(&mut self) {
            disable_transitions(self.0, false);
        }
    }

    fn disable_transitions(hwnd: HWND, disabled: bool) {
        let value = BOOL::from(disabled);
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED as u32,
                &value as *const BOOL as *const _,
                mem::size_of::<BOOL>() as u32,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_from_maximized_restores_first_and_maximizes_after() {
        let enter = steps(true, true, false);
        assert_eq!(enter, Steps { unmaximize: true, maximize: false, remember: true });
        let leave = steps(false, false, enter.remember);
        assert_eq!(leave, Steps { unmaximize: false, maximize: true, remember: false });
    }

    #[test]
    fn fullscreen_from_normal_leaves_maximized_alone() {
        let enter = steps(true, false, false);
        assert_eq!(enter, Steps::default());
        assert_eq!(steps(false, false, enter.remember), Steps::default());
    }

    #[test]
    fn fullscreen_again_keeps_remembered_maximized() {
        assert!(steps(true, false, true).remember);
        assert!(!steps(true, false, true).unmaximize);
    }

    #[test]
    fn monitor_in_workspace_of_bottom_taskbar() {
        let monitor = Rect { left: 0, top: 0, right: 1920, bottom: 1080 };
        let work = Rect { left: 0, top: 0, right: 1920, bottom: 1032 };
        assert_eq!(in_workspace(monitor, work), monitor);
    }

    #[test]
    fn monitor_in_workspace_of_top_left_taskbars() {
        let monitor = Rect { left: 1920, top: 0, right: 3840, bottom: 1080 };
        let work = Rect { left: 1968, top: 40, right: 3840, bottom: 1080 };
        let expected = Rect { left: 1872, top: -40, right: 3792, bottom: 1040 };
        assert_eq!(in_workspace(monitor, work), expected);
    }
}

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
//!
//! A single switch still blinks if the window shows at its new size before it is drawn at it:
//! winit hands the resize over only after the switch, and the frame at the new size follows a
//! turn or more of the event loop later, while the DWM shows the window's old frame cut down or
//! stretched over the new size, or its surface blank where adding or removing the frame rebuilt
//! it. Leaving fullscreen, to a much smaller window, that is most of the screen. The switch is
//! therefore made right before the window is next drawn, just after a composition of the DWM, and
//! the resize it makes taken in at once, so the next composition shows the window at its new size
//! already drawn at it. A window covering its monitor does not take the screen over from the
//! DWM, which could also blink in and out of it: winit's client area of a window without
//! decorations but with a shadow starts a pixel below the window's top, so never matches the
//! monitor.

use std::cell::Cell;
use std::rc::Rc;

use winit::event::{Event as WinitEvent, WindowEvent};
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

use crate::clipboard::Clipboard;
use crate::config::UiConfig;
use crate::event::Event;
use crate::window_context::WindowContext;

/// What the window was before it went fullscreen, for when it leaves.
#[derive(Debug, Default)]
pub struct FullscreenState {
    /// The window is to be maximized when it leaves fullscreen.
    maximized: Cell<bool>,
    /// Where the window restores to from maximized, put aside while fullscreen.
    #[cfg(windows)]
    normal: Cell<Option<Rect>>,
    /// Whether the window is to be fullscreen, switched to right before it is next drawn.
    #[cfg(windows)]
    pending: Cell<Option<bool>>,
}

impl FullscreenState {
    /// Enter or leave fullscreen, returning whether that is done, or is left to the caller. On
    /// Windows the switch is made right before the window is next drawn, which is asked for.
    pub fn set(&self, window: &Window, fullscreen: bool) -> bool {
        #[cfg(windows)]
        if window.is_minimized() == Some(true) {
            // A minimized window is not drawn, and shows nothing of the switch.
            self.pending.set(None);
            windows::set(self, window, fullscreen);
        } else {
            self.pending.set(pending(fullscreen, window.fullscreen().is_some()));
            window.request_redraw();
        }
        let _ = (window, fullscreen);
        cfg!(windows)
    }

    /// Make the switch asked for, right before drawing the window, returning whether it is made.
    fn apply(&self, window: &Window) -> bool {
        #[cfg(windows)]
        if let Some(fullscreen) = self.pending.take() {
            windows::flush();
            windows::set(self, window, fullscreen);
            return true;
        }
        let _ = window;
        false
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

/// Make a switch to or from fullscreen asked for of `ctx` right before it is drawn, and take in
/// the resize that makes now, rather than after winit hands it over, for the frame drawn next to
/// be at the window's new size.
pub fn before_draw(
    ctx: &mut WindowContext,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
    clipboard: &mut Clipboard,
) {
    #[cfg(target_os = "macos")]
    macos::after_transition(ctx, config);
    if !ctx.display.window.fullscreen.apply(ctx.display.window.winit()) {
        return;
    }
    let size = ctx.display.window.inner_size();
    if let Some(event) =
        super::window_event(ctx, WindowEvent::Resized(size), config, proxy, clipboard)
    {
        let window_id = ctx.id();
        ctx.event_queue.push(WinitEvent::WindowEvent { window_id, event });
    }
}

/// The switch to make for a window to be `fullscreen`, which is fullscreen `now` or not: none
/// when it already is as asked, which also takes back a switch asked for before.
#[cfg(any(windows, test))]
fn pending(fullscreen: bool, now: bool) -> Option<bool> {
    (fullscreen != now).then_some(fullscreen)
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

/// The system's animation into and out of fullscreen on macOS.
///
/// Winit keeps the window fullscreen until the animation out of it ends, after the window's last
/// resize, which is when the bar takes in how the window is shown. The system shows the window
/// buttons again as the animation begins though, where it lays them out by default: over the bar's
/// first tab, while the bar still lays out as in fullscreen. The start of either animation is
/// therefore watched for, to make or drop room for the buttons on the bar and place them on it as
/// the window resizes along, and its end, to draw the window again: the system lays the buttons out
/// once more as the animation ends, and the bar takes in how the window is shown then.
#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{OnceCell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;

    use alacritty_tabbed_deps::{objc2, objc2_app_kit, objc2_foundation};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel};
    use objc2_app_kit::{NSView, NSWindow};
    use objc2_foundation::ns_string;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    use super::super::frame::{Frame, frame};
    use crate::config::UiConfig;
    use crate::window_context::WindowContext;

    /// How long after the animation ends the window is drawn once more, should the system lay the
    /// buttons out again after the first frame, in seconds.
    const SETTLE: f64 = 0.25;

    thread_local! {
        static WATCHER: OnceCell<Retained<Watcher>> = const { OnceCell::new() };
        /// Whether each window, by its address, is fullscreen or on its way into it, as the system
        /// last announced.
        static ANNOUNCED: RefCell<HashMap<usize, bool>> = RefCell::new(HashMap::new());
    }

    define_class!(
        /// Told when a window begins and ends its animation into or out of fullscreen.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "AlacrittyTabbedFullscreenWatcher"]
        struct Watcher;

        impl Watcher {
            #[unsafe(method(willEnterFullscreen:))]
            fn will_enter(&self, notification: &AnyObject) {
                announce(notification, true);
            }

            #[unsafe(method(willExitFullscreen:))]
            fn will_exit(&self, notification: &AnyObject) {
                announce(notification, false);
            }

            #[unsafe(method(fullscreenChanged:))]
            fn changed(&self, notification: &AnyObject) {
                let window: Option<Retained<NSWindow>> = unsafe { msg_send![notification, object] };
                let Some(window) = window else { return };
                redisplay(&window);
                let _: () = unsafe {
                    msg_send![
                        self,
                        performSelector: sel!(redisplay:),
                        withObject: &*window,
                        afterDelay: SETTLE,
                    ]
                };
            }

            #[unsafe(method(redisplay:))]
            fn redisplay_later(&self, window: &NSWindow) {
                redisplay(window);
            }
        }
    );

    /// Record that the window of `notification` is going into fullscreen or out of it, and draw it.
    fn announce(notification: &AnyObject, fullscreen: bool) {
        let window: Option<Retained<NSWindow>> = unsafe { msg_send![notification, object] };
        let Some(window) = window else { return };
        let key = Retained::as_ptr(&window) as usize;
        ANNOUNCED.with(|announced| announced.borrow_mut().insert(key, fullscreen));
        redisplay(&window);
    }

    /// Have winit's view hand a redraw over, as the window is next displayed.
    fn redisplay(window: &NSWindow) {
        if let Some(view) = window.contentView() {
            view.setNeedsDisplay(true);
        }
    }

    /// Whether `window` is fullscreen or on its way into it, as the system last announced.
    fn announced(window: &Window) -> Option<bool> {
        let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        // Window events are handled on the main thread.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        let key = Retained::as_ptr(&view.window()?) as usize;
        ANNOUNCED.with(|announced| announced.borrow().get(&key).copied())
    }

    /// Take in that the window began or ended its animation into or out of fullscreen since the
    /// bar last looked, before it is drawn.
    pub fn after_transition(ctx: &mut WindowContext, config: &Rc<UiConfig>) {
        watch();
        let window = ctx.display.window.winit();
        let tabs = &ctx.display.tabs;
        let hidden = announced(window).unwrap_or(tabs.frame == Frame::Fullscreen);
        let fullscreen = window.fullscreen().is_some();
        let stale = fullscreen != (tabs.frame == Frame::Fullscreen) && frame(window) != tabs.frame;
        if hidden == tabs.lights_hidden && !stale {
            return;
        }
        ctx.display.tabs.lights_hidden = hidden;
        super::super::sync(ctx, config, true);
        super::super::redraw(ctx);
    }

    /// Watch every window for its animation into or out of fullscreen, once.
    fn watch() {
        let Some(mtm) = MainThreadMarker::new() else { return };
        WATCHER.with(|watcher| {
            if watcher.get().is_some() {
                return;
            }
            let target: Retained<Watcher> = unsafe { msg_send![Watcher::alloc(mtm), init] };
            let center: Retained<AnyObject> =
                unsafe { msg_send![class!(NSNotificationCenter), defaultCenter] };
            for (name, selector) in [
                (ns_string!("NSWindowWillEnterFullScreenNotification"), sel!(willEnterFullscreen:)),
                (ns_string!("NSWindowWillExitFullScreenNotification"), sel!(willExitFullscreen:)),
                (ns_string!("NSWindowDidEnterFullScreenNotification"), sel!(fullscreenChanged:)),
                (ns_string!("NSWindowDidExitFullScreenNotification"), sel!(fullscreenChanged:)),
            ] {
                let _: () = unsafe {
                    msg_send![
                        &center,
                        addObserver: &*target,
                        selector: selector,
                        name: name,
                        object: None::<&AnyObject>,
                    ]
                };
            }
            let _ = watcher.set(target);
        });
    }
}

#[cfg(windows)]
mod windows {
    use std::mem;

    use alacritty_tabbed_deps::windows_sys;
    use windows_sys::Win32::Foundation::{BOOL, HWND, RECT};
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_TRANSITIONS_FORCEDISABLED, DwmFlush, DwmSetWindowAttribute,
    };
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

    /// Wait for the DWM's next composition, for what follows to be done by the one after.
    pub fn flush() {
        unsafe { DwmFlush() };
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

        let mut info = MONITORINFO {
            cbSize: mem::size_of::<MONITORINFO>() as u32,
            ..unsafe { mem::zeroed() }
        };
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return None;
        }

        placement.flags = 0;
        placement.showCmd = SW_SHOWNORMAL as u32;
        placement.rcNormalPosition =
            win_rect(in_workspace(rect(info.rcMonitor), rect(info.rcWork)));
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
    fn switch_only_to_what_the_window_is_not() {
        assert_eq!(pending(true, false), Some(true));
        assert_eq!(pending(false, true), Some(false));
        assert_eq!(pending(true, true), None);
        assert_eq!(pending(false, false), None);
    }

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

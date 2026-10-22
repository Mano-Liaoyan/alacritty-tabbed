//! Plain Win32 windows for the profile dropdown and the settings panel.
//!
//! These windows live on the winit thread and are pumped by its message loop, but are not
//! winit windows: they need child edit controls, custom hit testing, and synchronous menus.
//! Each window's state is an `Rc<RefCell<T>>` owned by the window and freed on
//! `WM_NCDESTROY`. Messages that arrive while the state is already borrowed, for example a
//! notification sent by a child control during an update, fall back to `DefWindowProcW`.

use std::cell::RefCell;
use std::rc::Rc;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{BeginPaint, BitBlt, EndPaint, PAINTSTRUCT, SRCCOPY};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_DROPSHADOW, CreateWindowExW, DefWindowProcW, GWLP_USERDATA, GetClientRect,
    GetWindowLongPtrW, IDC_ARROW, LoadCursorW, RegisterClassExW, SetWindowLongPtrW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_ERASEBKGND, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WNDCLASSEXW,
};

use super::paint::{self, Canvas, Color, Rect, wide};

/// The behaviour of one kind of window.
pub(super) trait Handler: Sized + 'static {
    /// Handle a message; `None` defers to `DefWindowProcW`.
    fn message(
        &mut self,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT>;

    /// Draw the whole client area: shapes first, then text and icons.
    fn shapes(&self, canvas: &mut Canvas<'_>);
    unsafe fn gdi(&self, dc: windows_sys::Win32::Graphics::Gdi::HDC);

    /// Run work that must not hold the state borrow, such as modal file dialogs.
    fn after(_state: &Rc<RefCell<Self>>, _hwnd: HWND) {}
}

/// Register `class` once and create a window whose state is `state`.
pub(super) unsafe fn create<T: Handler>(
    class: &str,
    drop_shadow: bool,
    ex_style: WINDOW_EX_STYLE,
    style: WINDOW_STYLE,
    owner: HWND,
    rect: Rect,
    state: T,
) -> HWND {
    let class = wide(class);
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let info = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: if drop_shadow { CS_DROPSHADOW } else { 0 },
            lpfnWndProc: Some(procedure::<T>),
            hInstance: instance,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        // Registering an existing class fails harmlessly.
        RegisterClassExW(&info);
        let state = Rc::into_raw(Rc::new(RefCell::new(state)));
        let hwnd = CreateWindowExW(
            ex_style,
            class.as_ptr(),
            std::ptr::null(),
            style,
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            owner,
            std::ptr::null_mut(),
            instance,
            state.cast(),
        );
        if hwnd.is_null() {
            drop(Rc::from_raw(state));
        }
        hwnd
    }
}

/// The state of a window created by [`create`], if it is still alive.
pub(super) fn state<T: Handler>(hwnd: HWND) -> Option<Rc<RefCell<T>>> {
    unsafe {
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<T>;
        if pointer.is_null() {
            return None;
        }
        Rc::increment_strong_count(pointer);
        Some(Rc::from_raw(pointer))
    }
}

unsafe extern "system" fn procedure<T: Handler>(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        // Keep the state alive for this call even if the window is destroyed during it.
        let Some(state) = state::<T>(hwnd) else {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        };
        if message == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Rc::from_raw(Rc::as_ptr(&state)));
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        let result = match message {
            WM_ERASEBKGND => Some(1),
            WM_PAINT => state.try_borrow().ok().map(|state| {
                present(hwnd, &*state);
                0
            }),
            _ => state
                .try_borrow_mut()
                .ok()
                .and_then(|mut state| state.message(hwnd, message, wparam, lparam)),
        };
        T::after(&state, hwnd);
        result.unwrap_or_else(|| DefWindowProcW(hwnd, message, wparam, lparam))
    }
}

unsafe fn present<T: Handler>(hwnd: HWND, state: &T) {
    unsafe {
        let mut paint: PAINTSTRUCT = std::mem::zeroed();
        let target = BeginPaint(hwnd, &mut paint);
        let mut client = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        let view = Rect::new(0, 0, client.right, client.bottom);
        paint::render(
            view,
            |canvas| state.shapes(canvas),
            |dc| state.gdi(dc),
            |dc, _| {
                BitBlt(target, 0, 0, view.width(), view.height(), dc, 0, 0, SRCCOPY);
            },
        );
        EndPaint(hwnd, &paint);
    }
}

/// Pixels per DIP for `hwnd`.
pub(super) fn scale(hwnd: HWND) -> f64 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 1. } else { f64::from(dpi) / 96. }
}

/// Ask DWM for rounded corners and a border in the header's outline color (Windows 11).
pub(super) fn round_corners(hwnd: HWND, preference: i32, border: Color) {
    let border = border.colorref();
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&preference as *const i32).cast(),
            std::mem::size_of_val(&preference) as u32,
        );
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            (&border as *const u32).cast(),
            std::mem::size_of_val(&border) as u32,
        );
    }
}

/// Signed client coordinates packed into a mouse message's `LPARAM`.
pub(super) fn point(lparam: LPARAM) -> (i32, i32) {
    (i32::from(lparam as u16 as i16), i32::from((lparam >> 16) as u16 as i16))
}

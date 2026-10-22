//! The new-tab profile dropdown, drawn in the header's style.
//!
//! Like a native popup menu, [`choose`] shows the list and pumps messages until the user picks
//! an entry or dismisses it, then returns the choice to the click that opened it.

use std::cell::RefCell;
use std::rc::Rc;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DWMWCP_ROUND;
use windows_sys::Win32::Graphics::Gdi::{
    DT_RIGHT, DT_VCENTER, DeleteObject, GetMonitorInfoW, HDC, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_RETURN,
    VK_SPACE, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DI_NORMAL, DestroyWindow, DispatchMessageW, DrawIconEx, GetMessageW, HICON, IsWindow, MSG,
    PostQuitMessage, SW_SHOW, SetForegroundWindow, ShowWindow, TranslateMessage, WA_INACTIVE,
    WM_ACTIVATE, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_QUIT, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::paint::{Canvas, Palette, Rect, draw_text, font, prompt_glyph};
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;

use super::surface::{self, Handler};

/// One row of the dropdown.
pub(super) struct Item {
    pub(super) name: String,
    pub(super) icon: Option<HICON>,
    pub(super) default: bool,
}

struct Dropdown {
    items: Vec<Item>,
    palette: Palette,
    scale: f64,
    width: i32,
    hovered: Option<usize>,
    tracking: bool,
    /// Set once the user chooses or dismisses; the inner value is the chosen row.
    result: Option<Option<usize>>,
}

const PAD: f64 = 6.;
const ROW: f64 = 34.;

impl Dropdown {
    fn px(&self, value: f64) -> i32 {
        (value * self.scale).round() as i32
    }

    fn row(&self, index: usize) -> Rect {
        let top = self.px(PAD) + index as i32 * self.px(ROW);
        Rect::new(self.px(PAD), top, self.width - self.px(PAD), top + self.px(ROW))
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let point = winit::dpi::PhysicalPosition::new(f64::from(x), f64::from(y));
        (0..self.items.len()).find(|&index| self.row(index).contains(point))
    }

    fn icon_rect(&self, row: Rect) -> Rect {
        let size = self.px(18.);
        let left = row.left + self.px(10.);
        let top = row.top + (row.height() - size) / 2;
        Rect::new(left, top, left + size, top + size)
    }
}

impl Handler for Dropdown {
    fn message(
        &mut self,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match message {
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe { TrackMouseEvent(&mut track) };
                    self.tracking = true;
                }
                let (x, y) = surface::point(lparam);
                let hovered = self.row_at(x, y);
                if hovered != self.hovered {
                    self.hovered = hovered;
                    unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
                }
                Some(0)
            },
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.hovered = None;
                unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
                Some(0)
            },
            WM_LBUTTONUP => {
                let (x, y) = surface::point(lparam);
                if let Some(index) = self.row_at(x, y) {
                    self.result = Some(Some(index));
                }
                Some(0)
            },
            WM_KEYDOWN => {
                let last = self.items.len().checked_sub(1)?;
                let hovered = match wparam as u16 {
                    VK_DOWN => self.hovered.map_or(0, |index| (index + 1).min(last)),
                    VK_UP => self.hovered.map_or(last, |index| index.saturating_sub(1)),
                    VK_HOME => 0,
                    VK_END => last,
                    VK_RETURN | VK_SPACE => {
                        self.result = Some(self.hovered);
                        return Some(0);
                    },
                    VK_ESCAPE => {
                        self.result = Some(None);
                        return Some(0);
                    },
                    _ => return None,
                };
                self.hovered = Some(hovered);
                unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
                Some(0)
            },
            // Clicking anywhere else, including the header, dismisses the list.
            WM_ACTIVATE if (wparam & 0xffff) as u32 == WA_INACTIVE => {
                self.result.get_or_insert(None);
                None
            },
            _ => None,
        }
    }

    fn shapes(&self, canvas: &mut Canvas<'_>) {
        let palette = &self.palette;
        let view = canvas.view;
        canvas.clear(palette.popup);
        // Windows 11 rounds the window and draws its border; this keeps older systems framed.
        canvas.stroke(view, 0., self.scale.max(1.).floor(), palette.popup_outline);
        for (index, item) in self.items.iter().enumerate() {
            let row = self.row(index);
            if self.hovered == Some(index) {
                canvas.fill(row, f64::from(self.px(7.)), palette.hover);
            }
            if item.icon.is_none() {
                let badge = self.icon_rect(row);
                let accent = palette.accent(&item.name);
                canvas.fill(badge, f64::from(self.px(5.)), palette.popup.mix(accent, 0.3));
                prompt_glyph(canvas, badge, accent, self.scale);
            }
        }
    }

    unsafe fn gdi(&self, dc: HDC) {
        let palette = &self.palette;
        unsafe {
            let regular = font(12.5 * self.scale, 400);
            let small = font(11. * self.scale, 400);
            SetBkMode(dc, TRANSPARENT as i32);
            let old_font = SelectObject(dc, regular);
            for (index, item) in self.items.iter().enumerate() {
                let row = self.row(index);
                let icon = self.icon_rect(row);
                if let Some(handle) = item.icon {
                    DrawIconEx(
                        dc,
                        icon.left,
                        icon.top,
                        handle,
                        icon.width(),
                        icon.height(),
                        0,
                        std::ptr::null_mut(),
                        DI_NORMAL,
                    );
                }
                let mut right = row.right - self.px(12.);
                if item.default {
                    SelectObject(dc, small);
                    SetTextColor(dc, palette.muted.colorref());
                    let tag = Rect::new(right - self.px(60.), row.top, right, row.bottom);
                    draw_text(dc, "Default", tag, DT_VCENTER | DT_RIGHT);
                    right -= self.px(64.);
                }
                SelectObject(dc, regular);
                SetTextColor(dc, palette.foreground.colorref());
                let label = Rect::new(icon.right + self.px(10.), row.top, right, row.bottom);
                draw_text(dc, &item.name, label, DT_VCENTER);
            }
            SelectObject(dc, old_font);
            DeleteObject(regular);
            DeleteObject(small);
        }
    }
}

/// Show `items` below `anchor` (screen coordinates) and return the chosen row.
pub(super) fn choose(
    owner: HWND,
    anchor: POINT,
    palette: &Palette,
    items: Vec<Item>,
) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    let scale = surface::scale(owner);
    let px = |value: f64| (value * scale).round() as i32;
    let width = px(268.);
    let height = 2 * px(PAD) + items.len() as i32 * px(ROW);
    let state = Dropdown {
        items,
        palette: palette.clone(),
        scale,
        width,
        hovered: None,
        tracking: false,
        result: None,
    };

    // Keep the list on the monitor that holds the button.
    let mut rect = Rect::new(anchor.x, anchor.y, anchor.x + width, anchor.y + height);
    unsafe {
        let monitor = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            let work = info.rcWork;
            let dx = (work.right - rect.right).min(0) + (work.left - rect.left).max(0);
            rect = rect.shift(dx);
            if rect.bottom > work.bottom {
                let height = rect.height();
                rect.bottom = work.bottom;
                rect.top = (work.bottom - height).max(work.top);
            }
        }
    }

    unsafe {
        let hwnd = surface::create(
            "AlacrittyTabsDropdown",
            true,
            WS_EX_TOOLWINDOW,
            WS_POPUP,
            owner,
            rect,
            state,
        );
        if hwnd.is_null() {
            return None;
        }
        surface::round_corners(hwnd, DWMWCP_ROUND, palette.popup_outline);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);

        let state: Rc<RefCell<Dropdown>> = surface::state(hwnd)?;
        let mut message: MSG = std::mem::zeroed();
        while state.borrow().result.is_none() && IsWindow(hwnd) != 0 {
            if GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) <= 0 {
                if message.message == WM_QUIT {
                    PostQuitMessage(message.wParam as i32);
                }
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let result = state.borrow().result.flatten();
        DestroyWindow(hwnd);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows_tabs::profiles::{self, Icons, Profiles};

    /// Draw the dropdown with this machine's profiles for visual inspection.
    #[test]
    #[ignore = "writes ../work/dropdown-preview.bmp"]
    fn render_dropdown_preview() {
        let scale: f64 = 1.25;
        let icons = Icons::default();
        let profiles = Profiles::discover();
        let size = (18. * scale).round() as i32;
        let items: Vec<Item> = profiles
            .list
            .iter()
            .map(|profile| Item {
                name: profile.name.clone(),
                icon: profiles::icon_source(Some(profile), None)
                    .and_then(|path| icons.get(&path, size)),
                default: profile.name == "Command Prompt",
            })
            .collect();
        let width = (268. * scale).round() as i32;
        let height =
            2 * (PAD * scale).round() as i32 + items.len() as i32 * (ROW * scale).round() as i32;
        let palette = Palette::new(&crate::config::color::Colors::default());
        let dropdown = Dropdown {
            items,
            palette,
            scale,
            width,
            hovered: Some(1),
            tracking: false,
            result: None,
        };
        let mut bmp = Vec::new();
        unsafe {
            super::super::paint::render(
                Rect::new(0, 0, width, height),
                |canvas| dropdown.shapes(canvas),
                |dc| dropdown.gdi(dc),
                |_, pixels| bmp = super::super::paint::bitmap_file(width, height, pixels),
            );
        }
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work");
        std::fs::write(output.join("dropdown-preview.bmp"), bmp).unwrap();
    }
}

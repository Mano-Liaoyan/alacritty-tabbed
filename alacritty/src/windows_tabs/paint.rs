//! Drawing primitives shared by the tab header, the profile dropdown, and settings.
//!
//! Shapes are rasterized with anti-aliasing on the CPU into a 32-bit DIB, then GDI draws text
//! and icons on top with ClearType. Everything runs on the winit event thread.

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{
    BITMAPINFO, BITMAPINFOHEADER, CLEARTYPE_QUALITY, CreateCompatibleDC, CreateDIBSection,
    CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE,
    DeleteDC, DeleteObject, DrawTextW, GdiFlush, GetTextFaceW, HDC, HFONT, SelectObject,
    SetViewportOrgEx,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{HICON, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW};
use winit::dpi::PhysicalPosition;

use crate::config::color::Colors;
use crate::display::color::Rgb;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rect {
    pub(super) left: i32,
    pub(super) top: i32,
    pub(super) right: i32,
    pub(super) bottom: i32,
}

impl Rect {
    pub(super) fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }
    pub(super) fn width(self) -> i32 {
        self.right - self.left
    }
    pub(super) fn height(self) -> i32 {
        self.bottom - self.top
    }
    pub(super) fn is_empty(self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }
    pub(super) fn inset(self, amount: i32) -> Self {
        Self::new(self.left + amount, self.top + amount, self.right - amount, self.bottom - amount)
    }
    pub(super) fn center(self) -> (f64, f64) {
        (f64::from(self.left + self.right) / 2., f64::from(self.top + self.bottom) / 2.)
    }
    /// A `size` square sharing this rectangle's center.
    pub(super) fn square(self, size: i32) -> Self {
        let left = self.left + (self.width() - size) / 2;
        let top = self.top + (self.height() - size) / 2;
        Self::new(left, top, left + size, top + size)
    }
    pub(super) fn shift(self, dx: i32) -> Self {
        Self::new(self.left + dx, self.top, self.right + dx, self.bottom)
    }
    pub(super) fn contains(self, point: PhysicalPosition<f64>) -> bool {
        point.x >= f64::from(self.left)
            && point.x < f64::from(self.right)
            && point.y >= f64::from(self.top)
            && point.y < f64::from(self.bottom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Color {
    pub(super) r: f64,
    pub(super) g: f64,
    pub(super) b: f64,
}

impl Color {
    pub(super) const BLACK: Self = Self { r: 0., g: 0., b: 0. };
    pub(super) const WHITE: Self = Self { r: 255., g: 255., b: 255. };

    pub(super) fn rgb(rgb: Rgb) -> Self {
        Self { r: f64::from(rgb.r), g: f64::from(rgb.g), b: f64::from(rgb.b) }
    }

    pub(super) fn mix(self, other: Self, amount: f64) -> Self {
        Self {
            r: self.r + (other.r - self.r) * amount,
            g: self.g + (other.g - self.g) * amount,
            b: self.b + (other.b - self.b) * amount,
        }
    }

    pub(super) fn luma(self) -> f64 {
        (0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b) / 255.
    }

    /// 0x00RRGGBB, the layout of a 32-bit top-down DIB.
    pub(super) fn pixel(self) -> u32 {
        let channel = |value: f64| value.round().clamp(0., 255.) as u32;
        channel(self.r) << 16 | channel(self.g) << 8 | channel(self.b)
    }

    pub(super) fn colorref(self) -> u32 {
        let channel = |value: f64| value.round().clamp(0., 255.) as u32;
        channel(self.r) | channel(self.g) << 8 | channel(self.b) << 16
    }

    pub(super) fn from_pixel(pixel: u32) -> Self {
        Self {
            r: f64::from((pixel >> 16) & 0xff),
            g: f64::from((pixel >> 8) & 0xff),
            b: f64::from(pixel & 0xff),
        }
    }
}

/// Header colors derived from the terminal theme, so the chrome matches any color scheme.
#[derive(Clone)]
pub(super) struct Palette {
    pub(super) terminal: Color,
    pub(super) chrome: Color,
    pub(super) track: Color,
    pub(super) track_outline: Color,
    pub(super) raised: Color,
    pub(super) raised_outline: Color,
    pub(super) hover: Color,
    pub(super) pressed: Color,
    pub(super) separator: Color,
    pub(super) card_outline: Color,
    pub(super) foreground: Color,
    pub(super) muted: Color,
    pub(super) danger: Color,
    /// Floating surfaces such as the profile dropdown.
    pub(super) popup: Color,
    pub(super) popup_outline: Color,
    /// Text inputs on the terminal-colored settings panel.
    pub(super) field: Color,
    pub(super) field_outline: Color,
    pub(super) focus: Color,
    pub(super) accents: [Color; 5],
}

impl Palette {
    pub(super) fn new(colors: &Colors) -> Self {
        let terminal = Color::rgb(colors.primary.background);
        let text = Color::rgb(colors.primary.foreground);
        // Sit the window chrome slightly behind the terminal panel.
        let light = terminal.luma() > 0.5;
        let chrome = terminal.mix(Color::BLACK, if light { 0.06 } else { 0.3 });
        // Raised surfaces read as closer to the light source in either scheme.
        let raised = if light { terminal.mix(Color::WHITE, 0.7) } else { terminal.mix(text, 0.1) };
        let normal = &colors.normal;
        let popup = if light { terminal.mix(Color::WHITE, 0.5) } else { chrome.mix(text, 0.07) };
        let field = terminal.mix(text, if light { 0.035 } else { 0.05 });
        Self {
            terminal,
            chrome,
            track: chrome.mix(text, 0.045),
            track_outline: chrome.mix(text, 0.085),
            raised,
            raised_outline: raised.mix(text, if light { 0.14 } else { 0.1 }),
            hover: chrome.mix(text, 0.09),
            pressed: terminal.mix(text, 0.2),
            separator: chrome.mix(text, 0.16),
            card_outline: terminal.mix(text, 0.11),
            foreground: chrome.mix(text, 0.94),
            muted: chrome.mix(text, 0.56),
            danger: Color { r: 196., g: 43., b: 28. },
            popup,
            popup_outline: popup.mix(text, 0.14),
            field,
            field_outline: field.mix(text, 0.1),
            focus: terminal.mix(text, 0.42),
            accents: [normal.green, normal.yellow, normal.blue, normal.magenta, normal.cyan]
                .map(Color::rgb),
        }
    }

    /// Pick a stable badge color from the kind of program the tab is running.
    pub(super) fn accent(&self, title: &str) -> Color {
        let title = title.to_lowercase();
        let has = |words: &[&str]| words.iter().any(|word| title.contains(word));
        let [green, yellow, blue, magenta, cyan] = self.accents;
        if has(&["powershell", "pwsh"]) {
            blue
        } else if has(&["wsl", "ubuntu", "debian", "bash", "zsh", "fish", "linux"]) {
            yellow
        } else if has(&["ssh", "@"]) {
            magenta
        } else if has(&["python", "node", "bun", "cargo", "npm"]) {
            cyan
        } else if has(&["cmd.exe", "command prompt"]) {
            self.muted
        } else {
            green
        }
    }
}

/// Anti-aliased coverage rasterizer over one strip of the host window.
pub(super) struct Canvas<'a> {
    pub(super) pixels: &'a mut [u32],
    /// Window-space bounds of `pixels`.
    pub(super) view: Rect,
}

impl Canvas<'_> {
    pub(super) fn clear(&mut self, color: Color) {
        self.pixels.fill(color.pixel());
    }

    /// Blend `color` over `bounds` with per-pixel coverage sampled at pixel centers.
    pub(super) fn shade(&mut self, bounds: Rect, color: Color, coverage: impl Fn(f64, f64) -> f64) {
        let view = self.view;
        let (left, right) = (bounds.left.max(view.left), bounds.right.min(view.right));
        let (top, bottom) = (bounds.top.max(view.top), bounds.bottom.min(view.bottom));
        for y in top..bottom {
            let row = ((y - view.top) * view.width() - view.left) as isize;
            for x in left..right {
                let alpha = coverage(f64::from(x) + 0.5, f64::from(y) + 0.5).clamp(0., 1.);
                if alpha <= 0. {
                    continue;
                }
                let pixel = &mut self.pixels[(row + x as isize) as usize];
                *pixel = if alpha >= 1. {
                    color.pixel()
                } else {
                    Color::from_pixel(*pixel).mix(color, alpha).pixel()
                };
            }
        }
    }

    pub(super) fn fill(&mut self, rect: Rect, radius: f64, color: Color) {
        self.shade(rect, color, |x, y| 0.5 - rounded_distance(rect, radius, x, y));
    }

    /// An inside border of `width` pixels following the rounded outline.
    pub(super) fn stroke(&mut self, rect: Rect, radius: f64, width: f64, color: Color) {
        self.shade(rect, color, |x, y| {
            let distance = rounded_distance(rect, radius, x, y);
            (0.5 - distance).clamp(0., 1.) - (0.5 - distance - width).clamp(0., 1.)
        });
    }

    /// A round-capped segment.
    pub(super) fn line(&mut self, from: (f64, f64), to: (f64, f64), width: f64, color: Color) {
        let reach = width / 2. + 1.;
        let bounds = Rect::new(
            (from.0.min(to.0) - reach).floor() as i32,
            (from.1.min(to.1) - reach).floor() as i32,
            (from.0.max(to.0) + reach).ceil() as i32,
            (from.1.max(to.1) + reach).ceil() as i32,
        );
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = (dx * dx + dy * dy).max(f64::EPSILON);
        self.shade(bounds, color, |x, y| {
            let t = (((x - from.0) * dx + (y - from.1) * dy) / length).clamp(0., 1.);
            let distance = (x - from.0 - t * dx).hypot(y - from.1 - t * dy);
            0.5 - (distance - width / 2.)
        });
    }
}

/// Signed distance from a point to a rounded rectangle; negative inside.
pub(super) fn rounded_distance(rect: Rect, radius: f64, x: f64, y: f64) -> f64 {
    let (cx, cy) = rect.center();
    let radius = radius.min(f64::from(rect.width().min(rect.height())) / 2.).max(0.);
    let qx = (x - cx).abs() - (f64::from(rect.width()) / 2. - radius);
    let qy = (y - cy).abs() - (f64::from(rect.height()) / 2. - radius);
    qx.max(0.).hypot(qy.max(0.)) + qx.max(qy).min(0.) - radius
}

/// Alacritty's application icon, embedded by `windows/alacritty.rc`, at `size` pixels.
pub(super) fn load_logo(size: i32) -> HICON {
    const IDI_ICON: u16 = 0x101;
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        // MAKEINTRESOURCEW: integer resource identifiers are passed in the pointer.
        let name = IDI_ICON as usize as *const u16;
        LoadImageW(module, name, IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
    }
}

pub(super) unsafe fn font(height: f64, weight: i32) -> HFONT {
    let name: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
    unsafe {
        CreateFontW(
            -height.round() as i32,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            u32::from(DEFAULT_CHARSET),
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            name.as_ptr(),
        )
    }
}

/// Windows' icon font: Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on Windows 10.
pub(super) unsafe fn symbol_font(dc: HDC, height: f64) -> HFONT {
    for name in ["Segoe Fluent Icons", "Segoe MDL2 Assets"] {
        let face = wide(name);
        unsafe {
            let font = CreateFontW(
                -height.round() as i32,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                u32::from(DEFAULT_CHARSET),
                0,
                0,
                CLEARTYPE_QUALITY as u32,
                0,
                face.as_ptr(),
            );
            // GDI substitutes missing faces silently; keep the font only if it is the real one.
            let previous = SelectObject(dc, font);
            let mut actual = [0u16; 64];
            let len = GetTextFaceW(dc, actual.len() as i32, actual.as_mut_ptr());
            SelectObject(dc, previous);
            if len > 0 && String::from_utf16_lossy(&actual[..len as usize - 1]) == name {
                return font;
            }
            DeleteObject(font);
        }
    }
    std::ptr::null_mut()
}

pub(super) unsafe fn draw_text(dc: HDC, text: &str, rect: Rect, align: u32) {
    if rect.is_empty() {
        return;
    }
    let text: Vec<u16> = text.encode_utf16().collect();
    let mut rect = RECT { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
    unsafe {
        DrawTextW(
            dc,
            text.as_ptr(),
            text.len() as i32,
            &mut rect,
            DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX | align,
        );
    }
}

pub(super) enum Icon {
    Plus,
    Chevron,
    Close,
    Minimize,
    Maximize,
    Restore,
    Previous,
    Next,
}

/// Stroke icons stay centered at fractional DPI and do not depend on font glyph coverage.
pub(super) fn icon(canvas: &mut Canvas<'_>, rect: Rect, icon: Icon, color: Color, scale: f64) {
    let width = scale.round().max(1.);
    // Snap the center so that odd-width horizontal and vertical strokes stay crisp.
    let snap = if width as i32 % 2 == 1 { 0.5 } else { 0. };
    let (cx, cy) = rect.center();
    let (cx, cy) = (cx.floor() + snap, cy.floor() + snap);
    let at = |x: f64, y: f64| (cx + (x * scale).round(), cy + (y * scale).round());
    let mut line = |a: (f64, f64), b: (f64, f64)| {
        canvas.line(at(a.0, a.1), at(b.0, b.1), width, color);
    };
    match icon {
        Icon::Plus => {
            line((-5.5, 0.), (5.5, 0.));
            line((0., -5.5), (0., 5.5));
        },
        Icon::Chevron => {
            line((-3.5, -1.5), (0., 2.));
            line((0., 2.), (3.5, -1.5));
        },
        Icon::Close => {
            line((-4., -4.), (4., 4.));
            line((-4., 4.), (4., -4.));
        },
        Icon::Minimize => line((-5., 0.), (5., 0.)),
        Icon::Maximize => {
            let (x, y) = at(-5., -5.);
            let (x2, y2) = at(5., 5.);
            let rect =
                Rect::new(x.floor() as i32, y.floor() as i32, x2.ceil() as i32, y2.ceil() as i32);
            canvas.stroke(rect, 2. * scale, width, color);
        },
        Icon::Restore => {
            line((-3., -5.), (5., -5.));
            line((5., -5.), (5., 3.));
            let (x, y) = at(-5., -3.);
            let (x2, y2) = at(3., 5.);
            let rect =
                Rect::new(x.floor() as i32, y.floor() as i32, x2.ceil() as i32, y2.ceil() as i32);
            canvas.stroke(rect, 2. * scale, width, color);
        },
        Icon::Previous => {
            line((2., -4.), (-2., 0.));
            line((-2., 0.), (2., 4.));
        },
        Icon::Next => {
            line((-2., -4.), (2., 0.));
            line((2., 0.), (-2., 4.));
        },
    }
}

/// A `>_` prompt, used for the host mark and tab badges.
pub(super) fn prompt_glyph(canvas: &mut Canvas<'_>, rect: Rect, color: Color, scale: f64) {
    let (cx, cy) = rect.center();
    let unit = f64::from(rect.width()) / 18.;
    let width = (1.5 * scale).max(1.);
    let at = |x: f64, y: f64| (cx + x * unit, cy + y * unit);
    canvas.line(at(-4.5, -3.5), at(-1., 0.), width, color);
    canvas.line(at(-1., 0.), at(-4.5, 3.5), width, color);
    canvas.line(at(1., 3.5), at(5., 3.5), width, color);
}

/// Rasterize `view` (in window coordinates) into a DIB-backed DC and hand it to `present`.
///
/// `shapes` runs first on the pixel buffer; `gdi` then draws text and icons into the same
/// bitmap through a DC whose origin matches window coordinates.
pub(super) unsafe fn render(
    view: Rect,
    shapes: impl FnOnce(&mut Canvas<'_>),
    gdi: impl FnOnce(HDC),
    present: impl FnOnce(HDC, &[u32]),
) {
    if view.is_empty() {
        return;
    }
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            return;
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: view.width(),
            biHeight: -view.height(),
            biPlanes: 1,
            biBitCount: 32,
            ..std::mem::zeroed()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap =
            CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            DeleteDC(dc);
            return;
        }
        let old_bitmap = SelectObject(dc, bitmap);
        let len = (view.width() * view.height()) as usize;
        shapes(&mut Canvas {
            pixels: std::slice::from_raw_parts_mut(bits.cast::<u32>(), len),
            view,
        });
        SetViewportOrgEx(dc, -view.left, -view.top, std::ptr::null_mut());
        gdi(dc);
        SetViewportOrgEx(dc, 0, 0, std::ptr::null_mut());
        GdiFlush();
        present(dc, std::slice::from_raw_parts(bits.cast::<u32>(), len));
        SelectObject(dc, old_bitmap);
        DeleteObject(bitmap);
        DeleteDC(dc);
    }
}

/// A NUL-terminated UTF-16 copy of `text` for Win32 calls.
pub(super) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// A 32-bit top-down BMP file of `pixels`, for preview tests.
#[cfg(test)]
pub(super) fn bitmap_file(width: i32, height: i32, pixels: &[u32]) -> Vec<u8> {
    let size = (pixels.len() * 4) as u32;
    let mut bmp = Vec::with_capacity(54 + size as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(54 + size).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&(-height).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    pixels.iter().for_each(|pixel| bmp.extend_from_slice(&pixel.to_le_bytes()));
    bmp
}

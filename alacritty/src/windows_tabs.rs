//! Windows counterpart to AppKit window tabbing.
//!
//! Each terminal keeps its own winit child window, GL surface, PTY, and event
//! identity. Only the enclosing top-level window and tab strip are shared.
//! All HWND access happens on the winit event thread. The processor drops child
//! contexts before removing their host, so GL surfaces never outlive their HWND.

mod dropdown;
mod paint;
mod profiles;
mod settings;
mod surface;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::num::NonZeroIsize;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, ClientToScreen, DT_BOTTOM, DT_CENTER, DT_SINGLELINE, DT_TOP, DT_VCENTER, DeleteObject,
    ExcludeClipRect, GetDC, HDC, ReleaseDC, SRCCOPY, SelectClipRgn, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetDoubleClickTime, SetFocus};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DI_NORMAL, DestroyIcon, DrawIconEx, GWL_STYLE, GetClientRect, GetWindowLongPtrW, HICON,
    MoveWindow, SW_HIDE, SW_SHOWNOACTIVATE, SetWindowLongPtrW, ShowWindow, WM_NCACTIVATE,
    WM_NCDESTROY, WM_NCPAINT, WS_VISIBLE,
};
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::platform::windows::{WindowAttributesExtWindows, WindowExtWindows};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle, Win32WindowHandle};
use winit::window::{CursorIcon, Fullscreen, ResizeDirection, Window, WindowAttributes, WindowId};

use crate::cli::WindowOptions;
use crate::config::UiConfig;
use crate::display::window::Window as TerminalWindow;
use paint::{
    Canvas, Color, Icon, Palette, Rect, draw_text, font, icon, load_logo, prompt_glyph, symbol_font,
};
use profiles::{Icons, Profile, Profiles};

#[derive(Clone, Copy, Debug)]
pub enum TabAction {
    New { wsl: bool },
    Next,
    Previous,
    Index(usize),
    Last,
    ToggleFullscreen,
    ToggleMaximized,
    Minimize,
}

pub enum Request {
    None,
    /// Open a tab with `profile`, or `command`, or else the default profile.
    New {
        parent: isize,
        profile: Option<String>,
        command: Option<Vec<String>>,
    },
    Close(Vec<WindowId>),
}

/// State shared by every tab window, the dropdown, and the settings panel.
struct Shared {
    profiles: RefCell<Profiles>,
    icons: Icons,
    /// Program of the shell configured in `alacritty.toml`, for profiles without a command.
    shell: Option<String>,
    /// The open settings panel, if any.
    settings: Cell<HWND>,
}

/// A tab being dragged to a new position.
#[derive(Clone, Copy, Debug)]
struct TabDrag {
    /// Current position of the dragged tab in `Group::tabs`.
    index: usize,
    /// Pointer offset from the tab's left edge when it was grabbed.
    grab: f64,
    start: f64,
    moving: bool,
}

struct Tab {
    id: WindowId,
    hwnd: isize,
    title: String,
    /// File holding the icon of the profile or program the tab was started with.
    icon: Option<PathBuf>,
}

struct Group {
    window: Window,
    tabs: Vec<Tab>,
    selected: usize,
    first_visible: usize,
    pointer: PhysicalPosition<f64>,
    /// Whether `pointer` is over the host; winit omits repeated moves to the same position.
    hovering: bool,
    pressed: Hit,
    drag: Option<TabDrag>,
    last_drag_click: Option<(Instant, PhysicalPosition<f64>)>,
    palette: Palette,
    host_name: String,
    shared: Rc<Shared>,
    /// When the dropdown last closed, so the click that dismissed it does not reopen it.
    dropdown_closed: Option<Instant>,
    /// Whether leaving fullscreen should maximize the window again.
    restore_maximized: bool,
}

#[derive(Default)]
pub struct TabManager {
    groups: HashMap<WindowId, Group>,
    shared: Option<Rc<Shared>>,
    /// Icon for the terminal currently being created, consumed by `attach`.
    pending_icon: Option<PathBuf>,
}

fn hwnd(window: &Window) -> isize {
    match window.window_handle().expect("live window").as_raw() {
        RawWindowHandle::Win32(handle) => handle.hwnd.get(),
        _ => unreachable!("Windows tab host must have a Win32 handle"),
    }
}

fn strip_height(scale: f64) -> i32 {
    (48.0 * scale).round() as i32
}

/// Ask DWM for Windows 11 rounded corners, or square ones while the window fills a monitor.
///
/// Older systems reject the attribute, which leaves the window square.
fn round_corners(window: &Window, round: bool) {
    let corners = if round { DWMWCP_ROUND } else { DWMWCP_DONOTROUND };
    unsafe {
        DwmSetWindowAttribute(
            hwnd(window) as HWND,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corners as *const i32).cast(),
            std::mem::size_of_val(&corners) as u32,
        )
    };
}

/// Never let `DefWindowProcW` paint the host's native caption.
///
/// winit hides the frame only through `WM_NCCALCSIZE`, so the host keeps its caption styles and
/// `DefWindowProcW` draws a classic caption across the header: on `WM_NCPAINT` from
/// `PrintWindow`, as used by screenshot tools, and, once non-client painting is suppressed, on
/// every `WM_NCACTIVATE`. The caption stays until the header repaints, which cannot happen while
/// the dropdown or a menu runs its own message loop. Hiding the window from `DefWindowProcW`
/// for these messages skips the painting but keeps the default handling; DWM draws the frame.
unsafe extern "system" fn host_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        match message {
            WM_NCPAINT | WM_NCACTIVATE => {
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
                SetWindowLongPtrW(hwnd, GWL_STYLE, style & !(WS_VISIBLE as isize));
                let result = DefSubclassProc(hwnd, message, wparam, lparam);
                SetWindowLongPtrW(hwnd, GWL_STYLE, style);
                result
            },
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(host_procedure), 0);
                DefSubclassProc(hwnd, message, wparam, lparam)
            },
            _ => DefSubclassProc(hwnd, message, wparam, lparam),
        }
    }
}

/// The parent HWND is created and owned by TabManager on the same UI thread.
pub fn child_attributes(attributes: WindowAttributes, parent: isize) -> WindowAttributes {
    let parent_handle = Win32WindowHandle::new(NonZeroIsize::new(parent).expect("live tab host"));
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetClientRect(parent as HWND, &mut rect) };
    // The final DPI-correct placement is applied when the tab is attached.
    let attributes = attributes
        .with_decorations(false)
        .with_maximized(false)
        .with_fullscreen(None)
        .with_position(PhysicalPosition::new(0, 0))
        .with_inner_size(PhysicalSize::new(rect.right.max(1) as u32, rect.bottom.max(1) as u32));
    unsafe { attributes.with_parent_window(Some(RawWindowHandle::Win32(parent_handle))) }
}

impl TabManager {
    fn shared(&mut self, config: &UiConfig) -> Rc<Shared> {
        self.shared
            .get_or_insert_with(|| {
                Rc::new(Shared {
                    profiles: RefCell::new(Profiles::load()),
                    icons: Icons::default(),
                    shell: config.terminal.shell.as_ref().map(|shell| shell.program().to_owned()),
                    settings: Cell::new(std::ptr::null_mut()),
                })
            })
            .clone()
    }

    /// Apply the requested profile, or the default one, to a terminal about to be created.
    ///
    /// Explicit commands, such as `alacritty -e` or the WSL shortcut, are left untouched.
    pub fn launch(&mut self, config: &UiConfig, options: &mut WindowOptions) {
        self.pending_icon = None;
        if !config.window.tabs {
            return;
        }
        let shared = self.shared(config);
        let profiles = shared.profiles.borrow();
        let profile = match options.tab_profile.take() {
            Some(name) => profiles.get(&name),
            None if options.terminal_options.command().is_none() => profiles.default_profile(),
            None => None,
        };
        if let Some(profile) = profile {
            profile.apply(options);
        }
        // Without a profile, the tab still shows the icon of the program it runs.
        let command = options.terminal_options.command().map(|command| {
            std::iter::once(command.program())
                .chain(command.args().iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        });
        let probe = profile
            .cloned()
            .unwrap_or(Profile { command: command.unwrap_or_default(), ..Profile::default() });
        self.pending_icon = profiles::icon_source(Some(&probe), shared.shell.as_deref());
    }

    pub fn prepare(
        &mut self,
        event_loop: &ActiveEventLoop,
        config: &UiConfig,
        options: &mut WindowOptions,
    ) -> Result<(), winit::error::OsError> {
        if !config.window.tabs {
            options.tab_parent = None;
            return Ok(());
        }
        if options
            .tab_parent
            .is_some_and(|parent| self.groups.values().any(|group| hwnd(&group.window) == parent))
        {
            return Ok(());
        }

        let fullscreen = config.window.fullscreen();
        let mut attributes = TerminalWindow::get_platform_window(&config.window.identity, &config.window)
                .with_title("Alacritty Tabs")
                // The tab header is the title bar; winit still manages the native window.
                .with_decorations(false)
                // The shadow moves the client area down a pixel, which fullscreen cannot spare.
                .with_undecorated_shadow(fullscreen.is_none())
                .with_min_inner_size(LogicalSize::new(480., 180.))
                .with_theme(config.window.theme())
                .with_inner_size(LogicalSize::new(1000., 650.))
                .with_maximized(config.window.maximized())
                .with_fullscreen(fullscreen.clone())
                .with_visible(false);
        if let Some(position) = config.window.position {
            attributes = attributes.with_position(PhysicalPosition::new(position.x, position.y));
        }
        let window = event_loop.create_window(attributes)?;
        // Undecorated windows are square by default.
        round_corners(&window, fullscreen.is_none());
        unsafe { SetWindowSubclass(hwnd(&window) as HWND, Some(host_procedure), 0, 0) };
        options.tab_parent = Some(hwnd(&window));
        let shared = self.shared(config);
        self.groups.insert(
            window.id(),
            Group {
                window,
                tabs: Vec::new(),
                selected: 0,
                first_visible: 0,
                pointer: PhysicalPosition::new(0., 0.),
                hovering: false,
                drag: None,
                pressed: Hit::None,
                last_drag_click: None,
                palette: Palette::new(&config.colors),
                host_name: std::env::var("COMPUTERNAME").unwrap_or_default(),
                shared,
                dropdown_closed: None,
                restore_maximized: false,
            },
        );
        Ok(())
    }

    pub fn discard_empty(&mut self) {
        self.groups.retain(|_, group| !group.tabs.is_empty());
    }

    pub fn attach(&mut self, terminal: &TerminalWindow) {
        let Some(parent) = terminal.tab_parent() else { return };
        let Some(group) = self.groups.values_mut().find(|g| hwnd(&g.window) == parent) else {
            return;
        };
        let child_hwnd = match terminal.raw_window_handle() {
            RawWindowHandle::Win32(handle) => handle.hwnd.get(),
            _ => unreachable!(),
        };
        group.tabs.push(Tab {
            id: terminal.id(),
            hwnd: child_hwnd,
            title: terminal.title().to_owned(),
            icon: self.pending_icon.take(),
        });
        group.window.set_visible(true);
        group.select(group.tabs.len() - 1);
        log::info!(
            "Windows tabs: {} terminal(s) in host {:?}",
            group.tabs.len(),
            group.window.id()
        );
    }

    pub fn is_host(&self, id: WindowId) -> bool {
        self.groups.contains_key(&id)
    }

    pub fn remove(&mut self, child: WindowId) {
        for group in self.groups.values_mut() {
            let Some(index) = group.tabs.iter().position(|tab| tab.id == child) else { continue };
            group.tabs.remove(index);
            if !group.tabs.is_empty() {
                let selected = selected_after_removal(group.selected, index, group.tabs.len());
                group.select(selected);
            }
            break;
        }
        self.discard_empty();
    }

    pub fn update_title(&mut self, child: WindowId, title: &str) {
        for group in self.groups.values_mut() {
            if let Some(index) = group.tabs.iter().position(|tab| tab.id == child) {
                if group.tabs[index].title != title {
                    group.tabs[index].title = title.to_owned();
                    if group.selected == index {
                        group.window.set_title(&format!("{title} — Alacritty Tabs"));
                    }
                    group.window.request_redraw();
                }
                break;
            }
        }
    }

    pub fn action(&mut self, child: WindowId, action: TabAction) -> Request {
        let Some(group) =
            self.groups.values_mut().find(|group| group.tabs.iter().any(|tab| tab.id == child))
        else {
            return Request::None;
        };
        match action {
            TabAction::New { wsl } => Request::New {
                parent: hwnd(&group.window),
                profile: None,
                command: wsl
                    .then(|| profiles::split_program(&profiles::wsl_command_line(None)))
                    .flatten(),
            },
            TabAction::Next => {
                group.select((group.selected + 1) % group.tabs.len());
                Request::None
            },
            TabAction::Previous => {
                group.select((group.selected + group.tabs.len() - 1) % group.tabs.len());
                Request::None
            },
            TabAction::Index(index) => {
                if index < group.tabs.len() {
                    group.select(index);
                }
                Request::None
            },
            TabAction::Last => {
                group.select(group.tabs.len() - 1);
                Request::None
            },
            TabAction::ToggleFullscreen => {
                group.set_fullscreen(group.window.fullscreen().is_none());
                Request::None
            },
            TabAction::ToggleMaximized => {
                group.window.set_maximized(!group.window.is_maximized());
                Request::None
            },
            TabAction::Minimize => {
                group.window.set_minimized(true);
                Request::None
            },
        }
    }

    pub fn event(&mut self, host: WindowId, event: WindowEvent) -> Request {
        let Some(group) = self.groups.get_mut(&host) else { return Request::None };
        match event {
            WindowEvent::CloseRequested => {
                return Request::Close(group.tabs.iter().map(|tab| tab.id).collect());
            },
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                group.layout();
                group.window.request_redraw();
            },
            WindowEvent::RedrawRequested => group.paint(),
            WindowEvent::Focused(true) => group.focus(),
            WindowEvent::CursorMoved { position, .. } => {
                let previous = group.hovering.then(|| group.header().hit(group.pointer));
                group.pointer = position;
                group.hovering = true;
                if group.drag_to(position.x) {
                    return Request::None;
                }
                let hit = group.header().hit(position);
                group.window.set_cursor(match hit {
                    Hit::Resize(direction) => direction.into(),
                    _ => CursorIcon::Default,
                });
                if previous != Some(hit) {
                    group.window.request_redraw();
                }
            },
            WindowEvent::CursorLeft { .. } => {
                group.hovering = false;
                group.window.request_redraw();
            },
            WindowEvent::MouseInput { state, button, .. } => {
                return group.click(state, button);
            },
            WindowEvent::MouseWheel { delta, .. }
                if !group.tabs.is_empty()
                    && group.pointer.y < f64::from(strip_height(group.window.scale_factor())) =>
            {
                let y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(position) => position.y,
                };
                let index = if y > 0. {
                    (group.selected + group.tabs.len() - 1) % group.tabs.len()
                } else if y < 0. {
                    (group.selected + 1) % group.tabs.len()
                } else {
                    return Request::None;
                };
                group.select(index);
            },
            _ => (),
        }
        Request::None
    }
}

fn selected_after_removal(selected: usize, removed: usize, remaining: usize) -> usize {
    selected.saturating_sub(usize::from(removed < selected)).min(remaining.saturating_sub(1))
}

impl Group {
    fn focus(&self) {
        if let Some(tab) = self.tabs.get(self.selected) {
            unsafe { SetFocus(tab.hwnd as HWND) };
        }
    }

    fn select(&mut self, index: usize) {
        self.selected = index;
        for (i, tab) in self.tabs.iter().enumerate() {
            unsafe {
                ShowWindow(tab.hwnd as HWND, if i == index { SW_SHOWNOACTIVATE } else { SW_HIDE })
            };
        }
        self.layout();
        self.focus();
        self.window.set_title(&format!("{} — Alacritty Tabs", self.tabs[index].title));
        self.window.request_redraw();
    }

    /// Enter or leave borderless fullscreen, keeping the header and restoring the prior state.
    fn set_fullscreen(&mut self, fullscreen: bool) {
        if fullscreen == self.window.fullscreen().is_some() {
            return;
        }
        if fullscreen {
            // winit keeps a maximized window's client area inside the monitor's work area,
            // which would leave the taskbar's strip unpainted, so enter from the normal state.
            self.restore_maximized = self.window.is_maximized();
            if self.restore_maximized {
                self.window.set_maximized(false);
            }
            self.window.set_undecorated_shadow(false);
            self.window.set_fullscreen(Some(Fullscreen::Borderless(None)));
        } else {
            self.window.set_fullscreen(None);
            self.window.set_undecorated_shadow(true);
            if std::mem::take(&mut self.restore_maximized) {
                self.window.set_maximized(true);
            }
        }
        round_corners(&self.window, !fullscreen);
        // The size may not change, e.g. with an auto-hidden taskbar, so always lay out again.
        self.layout();
        self.window.request_redraw();
    }

    fn header(&self) -> Header {
        let frame = if self.window.fullscreen().is_some() {
            Frame::Fullscreen
        } else if self.window.is_maximized() {
            Frame::Maximized
        } else {
            Frame::Normal
        };
        Header::new(
            self.window.inner_size(),
            self.window.scale_factor(),
            frame,
            self.tabs.len(),
            self.selected,
            self.first_visible,
        )
    }

    fn layout(&mut self) {
        let header = self.header();
        if header.width == 0 || header.height == 0 {
            return;
        }
        self.first_visible = header.first;
        let area = header.terminal;
        for tab in &self.tabs {
            unsafe {
                MoveWindow(
                    tab.hwnd as HWND,
                    area.left,
                    area.top,
                    area.width().max(1),
                    area.height().max(1),
                    1,
                );
            }
        }
    }

    fn click(&mut self, state: ElementState, button: MouseButton) -> Request {
        let hit = self.header().hit(self.pointer);
        if button == MouseButton::Right && state == ElementState::Pressed && hit == Hit::Drag {
            self.window.show_window_menu(self.pointer);
        }
        if button == MouseButton::Middle && state == ElementState::Pressed {
            if let Hit::Tab(index) | Hit::CloseTab(index) = hit {
                return Request::Close(vec![self.tabs[index].id]);
            }
        }
        if button != MouseButton::Left {
            return Request::None;
        }
        if state == ElementState::Pressed {
            self.pressed = hit;
            match hit {
                Hit::Drag => {
                    let now = Instant::now();
                    let double_click = self.last_drag_click.is_some_and(|(time, point)| {
                        now.duration_since(time).as_millis()
                            < u128::from(unsafe { GetDoubleClickTime() })
                            && (point.x - self.pointer.x).abs() < 4. * self.window.scale_factor()
                            && (point.y - self.pointer.y).abs() < 4. * self.window.scale_factor()
                    });
                    self.last_drag_click =
                        if double_click { None } else { Some((now, self.pointer)) };
                    if self.window.fullscreen().is_none() {
                        if double_click {
                            self.window.set_maximized(!self.window.is_maximized());
                        } else {
                            let _ = self.window.drag_window();
                        }
                    }
                },
                Hit::Resize(direction) => {
                    let _ = self.window.drag_resize_window(direction);
                },
                Hit::Tab(index) => {
                    self.select(index);
                    let header = self.header();
                    if let Some(&(_, rect)) = header.tabs.iter().find(|(i, _)| *i == index) {
                        self.drag = Some(TabDrag {
                            index,
                            grab: self.pointer.x - f64::from(rect.left),
                            start: self.pointer.x,
                            moving: false,
                        });
                    }
                },
                _ => self.last_drag_click = None,
            }
            return Request::None;
        }
        if self.drag.take().is_some_and(|drag| drag.moving) {
            self.pressed = Hit::None;
            self.window.request_redraw();
            return Request::None;
        }
        // Activate buttons on release, only if the pointer is still over the pressed target.
        let pressed = std::mem::replace(&mut self.pressed, Hit::None);
        if pressed != hit {
            return Request::None;
        }
        match hit {
            Hit::CloseTab(index) => Request::Close(vec![self.tabs[index].id]),
            Hit::New => Request::New { parent: hwnd(&self.window), profile: None, command: None },
            Hit::Profiles => {
                // The click that dismissed the dropdown would otherwise reopen it.
                let reopening = self
                    .dropdown_closed
                    .is_some_and(|closed| closed.elapsed() < Duration::from_millis(300));
                let profile = if reopening { None } else { self.choose_profile() };
                self.dropdown_closed = Some(Instant::now());
                match profile {
                    Some(name) => Request::New {
                        parent: hwnd(&self.window),
                        profile: Some(name),
                        command: None,
                    },
                    None => Request::None,
                }
            },
            Hit::Settings => {
                settings::open(hwnd(&self.window) as HWND, &self.palette, &self.shared);
                Request::None
            },
            Hit::Previous | Hit::Next if !self.tabs.is_empty() => {
                let offset = if hit == Hit::Previous { self.tabs.len() - 1 } else { 1 };
                self.select((self.selected + offset) % self.tabs.len());
                Request::None
            },
            Hit::Minimize => {
                self.window.set_minimized(true);
                Request::None
            },
            Hit::Maximize => {
                if self.window.fullscreen().is_some() {
                    self.set_fullscreen(false);
                } else {
                    self.window.set_maximized(!self.window.is_maximized());
                }
                Request::None
            },
            Hit::Close => Request::Close(self.tabs.iter().map(|tab| tab.id).collect()),
            _ => Request::None,
        }
    }

    /// Follow a pressed tab with the pointer, reordering as it crosses its neighbours.
    ///
    /// Returns whether a drag is in progress, so hover handling can be skipped.
    fn drag_to(&mut self, x: f64) -> bool {
        let Some(mut drag) = self.drag else { return false };
        if !drag.moving {
            if (x - drag.start).abs() < 4. * self.window.scale_factor() {
                return false;
            }
            drag.moving = true;
        }
        let header = self.header();
        if let Some(target) = header.drop_target(x - drag.grab) {
            if target != drag.index {
                let tab = self.tabs.remove(drag.index);
                self.tabs.insert(target, tab);
                self.selected = target;
                drag.index = target;
            }
        }
        self.drag = Some(drag);
        self.window.request_redraw();
        true
    }

    /// Show the profile dropdown under its button and return the chosen profile.
    fn choose_profile(&self) -> Option<String> {
        let header = self.header();
        let &(_, plus) = header.buttons.iter().find(|(hit, _)| *hit == Hit::New)?;
        let host = hwnd(&self.window) as HWND;
        let scale = header.scale;
        let size = (18. * scale).round() as i32;
        let (names, items): (Vec<_>, Vec<_>) = {
            let profiles = self.shared.profiles.borrow();
            profiles
                .list
                .iter()
                .filter(|profile| !profile.hidden)
                .map(|profile| {
                    let source = profiles::icon_source(Some(profile), self.shared.shell.as_deref());
                    let item = dropdown::Item {
                        name: profile.name.clone(),
                        icon: source.and_then(|path| self.shared.icons.get(&path, size)),
                        default: profile.name == profiles.default,
                    };
                    (profile.name.clone(), item)
                })
                .unzip()
        };
        let mut anchor = POINT { x: plus.left, y: header.bar - (2. * scale).round() as i32 };
        unsafe { ClientToScreen(host, &mut anchor) };
        let choice = dropdown::choose(host, anchor, &self.palette, items);
        self.focus();
        choice.map(|index| names[index].clone())
    }

    fn paint(&self) {
        let header = self.header();
        if header.width <= 0 || header.height <= 0 {
            return;
        }
        let host = hwnd(&self.window) as HWND;
        let hovered = if self.hovering { header.hit(self.pointer) } else { Hit::None };
        let dragged = self.drag.filter(|drag| drag.moving).and_then(|drag| {
            header.tabs.iter().find(|(index, _)| *index == drag.index).map(|&(index, rect)| {
                let (min, max) = header.drag_range();
                let left = (self.pointer.x - drag.grab).round() as i32;
                (index, left.clamp(min, max.max(min)) - rect.left)
            })
        });
        let size = Scene::badge_size(header.scale);
        let tab_icon = |index: usize| {
            let path = self.tabs[index].icon.as_deref()?;
            self.shared.icons.get(path, size)
        };
        let scene = Scene {
            header: &header,
            palette: &self.palette,
            hovered: if dragged.is_some() { Hit::None } else { hovered },
            dragged,
            icon: &tab_icon,
            host_name: &self.host_name,
            title: &|index| self.tabs[index].title.as_str(),
        };
        // Paint only the chrome around the terminal; each terminal keeps its own GL surface.
        unsafe {
            let target = GetDC(host);
            if target.is_null() {
                return;
            }
            for strip in header.chrome() {
                scene.render(strip, |dc, _| {
                    BitBlt(
                        target,
                        strip.left,
                        strip.top,
                        strip.width(),
                        strip.height(),
                        dc,
                        0,
                        0,
                        SRCCOPY,
                    );
                });
            }
            ReleaseDC(host, target);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    None,
    Drag,
    Resize(ResizeDirection),
    Tab(usize),
    CloseTab(usize),
    New,
    Profiles,
    Settings,
    Previous,
    Next,
    Minimize,
    Maximize,
    Close,
}

/// How the host window is shown, which decides its resize border and terminal margins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frame {
    Normal,
    Maximized,
    /// Borderless fullscreen: the header stays and the terminal fills the rest edge to edge.
    Fullscreen,
}

/// One source of geometry for painting, input, overflow, and terminal placement.
struct Header {
    scale: f64,
    frame: Frame,
    selected: usize,
    width: i32,
    height: i32,
    bar: i32,
    border: i32,
    close_width: i32,
    first: usize,
    brand: Rect,
    brand_label: bool,
    track: Rect,
    tabs: Vec<(usize, Rect)>,
    buttons: Vec<(Hit, Rect)>,
    /// Distance between the left edges of neighbouring tabs.
    step: i32,
    /// Rounded panel framing the terminal, and the terminal's own placement inside it.
    card: Rect,
    terminal: Rect,
}

impl Header {
    fn new(
        size: PhysicalSize<u32>,
        scale: f64,
        frame: Frame,
        count: usize,
        selected: usize,
        first: usize,
    ) -> Self {
        let px = |value: f64| (value * scale).round() as i32;
        let width = size.width as i32;
        let height = size.height as i32;
        let bar = strip_height(scale);
        let fullscreen = frame == Frame::Fullscreen;
        let border = if frame == Frame::Normal { px(4.) } else { 0 };
        let margin = px(8.);
        let control = px(32.);
        let caption = px(40.);
        let button = |left: i32, width: i32| {
            let top = (bar - control) / 2;
            Rect::new(left, top, left + width, top + control)
        };

        let controls_start = width - margin - 3 * caption;
        let settings_left = controls_start - px(4.) - control;
        // The new-tab button and its profile dropdown follow the last tab.
        let chevron = px(22.);
        let split = control + chevron;

        // Fullscreen gives the label's space to the tabs; the logo stays.
        let brand_label = !fullscreen && width >= px(780.);
        let brand_width = if brand_label { px(160.) } else { px(52.) };
        let pad = px(3.);
        let gap = px(4.);
        let min_tab = px(124.).max(1);
        // Always leave a blank title-bar drag target between the new-tab and settings buttons.
        let available = (settings_left - px(48.) - brand_width - 2 * pad - px(6.) - split).max(1);
        let overflow = count > (available / min_tab).max(1) as usize;
        let nav = px(28.);
        let available = (available - if overflow { 2 * nav + px(6.) } else { 0 }).max(1);
        let capacity = (available / min_tab).max(1) as usize;
        let step = (available / count.max(1).min(capacity) as i32).min(px(236.));
        let first = first
            .min(selected)
            .max((selected + 1).saturating_sub(capacity))
            .min(count.saturating_sub(capacity));

        let track_top = px(6.);
        let track_bottom = bar - px(6.);
        let mut tabs = Vec::new();
        let mut left = brand_width + pad;
        for index in first..count.min(first + capacity) {
            tabs.push((
                index,
                Rect::new(left, track_top + pad, left + step - gap, track_bottom - pad),
            ));
            left += step;
        }
        let track_right = tabs.last().map_or(brand_width, |(_, rect)| rect.right + pad);
        let track = Rect::new(brand_width, track_top, track_right, track_bottom);

        let mut buttons = Vec::new();
        let mut left = track.right + px(6.);
        if overflow {
            for hit in [Hit::Previous, Hit::Next] {
                buttons.push((hit, button(left, nav)));
                left += nav;
            }
        }
        buttons.push((Hit::New, button(left, control)));
        buttons.push((Hit::Profiles, button(left + control, chevron)));
        buttons.push((Hit::Settings, button(settings_left, control)));
        for (index, hit) in [Hit::Minimize, Hit::Maximize, Hit::Close].into_iter().enumerate() {
            buttons.push((hit, button(controls_start + index as i32 * caption, caption)));
        }

        let (card, terminal) = if fullscreen {
            let terminal = Rect::new(0, bar, width, height);
            (terminal, terminal)
        } else {
            let card = Rect::new(margin, bar, width - margin, height - margin);
            // Keep the square terminal surface inside the card's rounded corners.
            (card, card.inset(px(4.)))
        };

        Self {
            scale,
            frame,
            selected,
            width,
            height,
            bar,
            border,
            close_width: px(26.),
            first,
            brand: Rect::new(border.max(px(6.)), 0, brand_width - px(10.), bar),
            brand_label,
            track,
            tabs,
            buttons,
            step,
            card,
            terminal,
        }
    }

    /// Leftmost and rightmost positions a dragged tab may take.
    fn drag_range(&self) -> (i32, i32) {
        match (self.tabs.first(), self.tabs.last()) {
            (Some(&(_, first)), Some(&(_, last))) => (first.left, last.left),
            _ => (0, 0),
        }
    }

    /// The tab slot under the center of a tab whose left edge is at `left`.
    fn drop_target(&self, left: f64) -> Option<usize> {
        let &(first, first_rect) = self.tabs.first()?;
        let (min, max) = self.drag_range();
        let left = left.clamp(f64::from(min), f64::from(max));
        let center = left + f64::from(first_rect.width()) / 2.;
        let slot = ((center - f64::from(first_rect.left)) / f64::from(self.step.max(1))).floor();
        Some(first + (slot.max(0.) as usize).min(self.tabs.len() - 1))
    }

    fn close_tab(&self, rect: Rect) -> Rect {
        Rect::new(rect.right - self.close_width, rect.top, rect.right, rect.bottom)
    }

    /// Everything outside the terminal surface, as non-overlapping strips.
    fn chrome(&self) -> Vec<Rect> {
        let t = self.terminal;
        [
            Rect::new(0, 0, self.width, t.top),
            Rect::new(0, t.top, t.left, t.bottom),
            Rect::new(t.right, t.top, self.width, t.bottom),
            Rect::new(0, t.bottom, self.width, self.height),
        ]
        .into_iter()
        .map(|rect| {
            Rect::new(
                rect.left.max(0),
                rect.top.max(0),
                rect.right.min(self.width),
                rect.bottom.min(self.height),
            )
        })
        .filter(|rect| !rect.is_empty())
        .collect()
    }

    fn hit(&self, point: PhysicalPosition<f64>) -> Hit {
        if !Rect::new(0, 0, self.width, self.height).contains(point) {
            return Hit::None;
        }
        let left = point.x < f64::from(self.border);
        let right = point.x >= f64::from(self.width - self.border);
        let top = point.y < f64::from(self.border);
        let bottom = point.y >= f64::from(self.height - self.border);
        let direction = match (left, right, top, bottom) {
            (true, _, true, _) => Some(ResizeDirection::NorthWest),
            (_, true, true, _) => Some(ResizeDirection::NorthEast),
            (true, _, _, true) => Some(ResizeDirection::SouthWest),
            (_, true, _, true) => Some(ResizeDirection::SouthEast),
            (true, _, _, _) => Some(ResizeDirection::West),
            (_, true, _, _) => Some(ResizeDirection::East),
            (_, _, true, _) => Some(ResizeDirection::North),
            (_, _, _, true) => Some(ResizeDirection::South),
            _ => None,
        };
        if let Some(direction) = direction {
            return Hit::Resize(direction);
        }
        if point.y >= f64::from(self.bar) {
            return Hit::None;
        }
        for &(hit, rect) in &self.buttons {
            if rect.contains(point) {
                return hit;
            }
        }
        for &(index, rect) in &self.tabs {
            if rect.contains(point) {
                return if self.close_tab(rect).contains(point) {
                    Hit::CloseTab(index)
                } else {
                    Hit::Tab(index)
                };
            }
        }
        Hit::Drag
    }
}

/// Everything needed to draw one frame of the host chrome.
struct Scene<'a> {
    header: &'a Header,
    palette: &'a Palette,
    hovered: Hit,
    /// The tab being dragged and its horizontal offset from its slot.
    dragged: Option<(usize, i32)>,
    host_name: &'a str,
    title: &'a dyn Fn(usize) -> &'a str,
    /// The icon of the tab at an index, when its program or profile has one.
    icon: &'a dyn Fn(usize) -> Option<HICON>,
}

impl Scene<'_> {
    /// Rasterize `view` of the host and hand the bitmap to `present`.
    unsafe fn render(&self, view: Rect, present: impl FnOnce(HDC, &[u32])) {
        let in_bar = view.top < self.header.bar;
        let logo = if in_bar {
            load_logo((24. * self.header.scale).round() as i32)
        } else {
            std::ptr::null_mut()
        };
        unsafe {
            paint::render(
                view,
                |canvas| self.shapes(canvas, !logo.is_null()),
                |dc| {
                    if in_bar {
                        self.text(dc, logo);
                    }
                },
                present,
            );
            if !logo.is_null() {
                DestroyIcon(logo);
            }
        }
    }

    fn shapes(&self, canvas: &mut Canvas<'_>, logo: bool) {
        let header = self.header;
        let palette = self.palette;
        let scale = header.scale;
        let px = |value: f64| (value * scale).round() as i32;
        let hairline = scale.max(1.).floor();

        canvas.clear(palette.chrome);
        // In fullscreen the terminal covers everything below the bar, without a panel.
        if header.frame != Frame::Fullscreen {
            let radius = f64::from(px(10.));
            canvas.fill(header.card, radius, palette.terminal);
            canvas.stroke(header.card, radius, hairline, palette.card_outline);
        }
        if canvas.view.top >= header.bar {
            return;
        }

        // Without the embedded application icon, fall back to a drawn prompt mark.
        if !logo {
            let mark = self.mark();
            canvas.fill(mark, f64::from(px(7.)), palette.raised);
            canvas.stroke(mark, f64::from(px(7.)), hairline, palette.raised_outline);
            prompt_glyph(canvas, mark, palette.foreground, scale);
        }

        if !header.tabs.is_empty() {
            let radius = f64::from(header.track.height()) / 2.8;
            canvas.fill(header.track, radius, palette.track);
            canvas.stroke(header.track, radius, hairline, palette.track_outline);
        }
        let dragged = self.dragged.map(|(index, _)| index);
        let lit = |index: usize| {
            index == header.selected
                || Some(index) == dragged
                || matches!(self.hovered, Hit::Tab(i) | Hit::CloseTab(i) if i == index)
        };
        // Separate neighbouring tabs only where neither one is highlighted.
        for pair in header.tabs.windows(2) {
            let ((index, rect), (next, next_rect)) = (pair[0], pair[1]);
            if !lit(index) && !lit(next) {
                let x = f64::from(rect.right + next_rect.left) / 2.;
                let (_, cy) = rect.center();
                let h = f64::from(px(7.));
                canvas.line((x, cy - h), (x, cy + h), hairline, palette.separator);
            }
        }
        for (index, rect) in self.placed() {
            let active = index == header.selected;
            let radius = f64::from(rect.height()) / 3.2;
            let surface = if active {
                if Some(index) == dragged {
                    // Lift the dragged tab off the track.
                    let shadow =
                        Rect::new(rect.left, rect.top + px(1.), rect.right, rect.bottom + px(2.));
                    canvas.fill(shadow, radius + 1., palette.chrome.mix(Color::BLACK, 0.5));
                }
                canvas.fill(rect, radius, palette.raised);
                canvas.stroke(rect, radius, hairline, palette.raised_outline);
                palette.raised
            } else if lit(index) {
                canvas.fill(rect, radius, palette.hover);
                palette.hover
            } else {
                palette.track
            };

            if (self.icon)(index).is_none() {
                let accent = palette.accent((self.title)(index));
                let badge = self.badge(rect);
                canvas.fill(badge, f64::from(px(5.)), surface.mix(accent, 0.3));
                prompt_glyph(canvas, badge, accent, scale);
            }

            if lit(index) {
                let close = header.close_tab(rect);
                if self.hovered == Hit::CloseTab(index) {
                    canvas.fill(close.square(px(20.)), f64::from(px(6.)), palette.pressed);
                }
                icon(canvas, close, Icon::Close, palette.muted, scale);
            }
        }

        for &(hit, rect) in &header.buttons {
            let hovered = hit == self.hovered;
            let danger = hovered && hit == Hit::Close;
            if hovered {
                let color = if danger { palette.danger } else { palette.hover };
                canvas.fill(rect, f64::from(px(8.)), color);
            }
            let kind = match hit {
                Hit::New => Icon::Plus,
                Hit::Profiles => Icon::Chevron,
                // Drawn as a glyph with the text.
                Hit::Settings => continue,
                Hit::Previous => Icon::Previous,
                Hit::Next => Icon::Next,
                Hit::Minimize => Icon::Minimize,
                Hit::Maximize if header.frame != Frame::Normal => Icon::Restore,
                Hit::Maximize => Icon::Maximize,
                _ => Icon::Close,
            };
            let color = if danger {
                Color::WHITE
            } else if hovered || hit == Hit::New {
                palette.foreground
            } else {
                palette.muted
            };
            icon(canvas, rect, kind, color, scale);
        }
    }

    unsafe fn text(&self, dc: HDC, logo: HICON) {
        let header = self.header;
        let palette = self.palette;
        let scale = header.scale;
        let px = |value: f64| (value * scale).round() as i32;
        unsafe {
            if !logo.is_null() {
                let mark = self.mark().square(px(24.));
                DrawIconEx(
                    dc,
                    mark.left,
                    mark.top,
                    logo,
                    mark.width(),
                    mark.height(),
                    0,
                    std::ptr::null_mut(),
                    DI_NORMAL,
                );
            }

            let regular = font(12.5 * scale, 400);
            let strong = font(12.5 * scale, 600);
            let small = font(11. * scale, 400);
            let old_font = SelectObject(dc, regular);
            SetBkMode(dc, TRANSPARENT as i32);

            if let Some(&(_, gear)) = header.buttons.iter().find(|(hit, _)| *hit == Hit::Settings) {
                let symbols = symbol_font(dc, 14. * scale);
                if !symbols.is_null() {
                    let color = if self.hovered == Hit::Settings {
                        palette.foreground
                    } else {
                        palette.muted
                    };
                    SelectObject(dc, symbols);
                    SetTextColor(dc, color.colorref());
                    draw_text(dc, "\u{e713}", gear, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                    SelectObject(dc, regular);
                    DeleteObject(symbols);
                }
            }

            if header.brand_label {
                let left = self.mark().right + px(10.);
                let middle = header.bar / 2;
                SelectObject(dc, strong);
                SetTextColor(dc, palette.foreground.colorref());
                let rect = Rect::new(left, 0, header.brand.right, middle + px(1.));
                draw_text(dc, "Alacritty", rect, DT_BOTTOM);
                SelectObject(dc, small);
                SetTextColor(dc, palette.muted.colorref());
                let rect = Rect::new(left, middle + px(1.), header.brand.right, header.bar);
                draw_text(dc, self.host_name, rect, DT_TOP);
            }

            let placed = self.placed();
            let cover = self.dragged.and_then(|_| placed.last().copied());
            for &(index, rect) in &placed {
                // Labels are drawn after every shape; keep them from showing through the
                // dragged tab, which is placed last.
                match cover {
                    Some((dragged, cover)) if dragged != index => {
                        ExcludeClipRect(dc, cover.left, cover.top, cover.right, cover.bottom);
                    },
                    _ => {
                        SelectClipRgn(dc, std::ptr::null_mut());
                    },
                }
                let lifted = cover.is_some_and(|(dragged, _)| dragged == index);
                if let Some(handle) = (self.icon)(index) {
                    let badge = self.badge(rect);
                    DrawIconEx(
                        dc,
                        badge.left,
                        badge.top,
                        handle,
                        badge.width(),
                        badge.height(),
                        0,
                        std::ptr::null_mut(),
                        DI_NORMAL,
                    );
                }
                let active = index == header.selected;
                let over = matches!(self.hovered, Hit::Tab(i) | Hit::CloseTab(i) if i == index);
                SelectObject(dc, if active { strong } else { regular });
                let color = if active || over { palette.foreground } else { palette.muted };
                SetTextColor(dc, color.colorref());
                // The close button only takes space from tabs that show it.
                let right = if active || over || lifted {
                    header.close_tab(rect).left + px(2.)
                } else {
                    rect.right - px(10.)
                };
                let label = Rect::new(
                    self.badge(rect).right + px(8.),
                    rect.top,
                    right,
                    rect.bottom - px(1.),
                );
                draw_text(dc, (self.title)(index), label, DT_VCENTER);
            }
            SelectClipRgn(dc, std::ptr::null_mut());

            SelectObject(dc, old_font);
            DeleteObject(regular);
            DeleteObject(strong);
            DeleteObject(small);
        }
    }

    /// Visible tabs in paint order, with the dragged tab offset and painted last.
    fn placed(&self) -> Vec<(usize, Rect)> {
        let mut placed = self.header.tabs.clone();
        if let Some((index, offset)) = self.dragged {
            if let Some(position) = placed.iter().position(|(i, _)| *i == index) {
                let (index, rect) = placed.remove(position);
                placed.push((index, rect.shift(offset)));
            }
        }
        placed
    }

    /// Square area for the application logo at the left of the header.
    fn mark(&self) -> Rect {
        let brand = self.header.brand;
        let size = (26. * self.header.scale).round() as i32;
        Rect::new(brand.left, brand.top, brand.left + size, brand.bottom).square(size)
    }

    fn badge_size(scale: f64) -> i32 {
        (18. * scale).round() as i32
    }

    fn badge(&self, tab: Rect) -> Rect {
        let size = Self::badge_size(self.header.scale);
        let left = tab.left + (9. * self.header.scale).round() as i32;
        let top = tab.top + (tab.height() - size) / 2;
        Rect::new(left, top, left + size, top + size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::color::Colors;
    use crate::display::color::Rgb;

    /// Exercise the production painter without requiring an interactive Windows desktop.
    #[test]
    #[ignore = "writes header previews to ../work/header-preview-*.bmp"]
    fn render_header_preview() {
        let dark = Colors::default();
        let mut light = Colors::default();
        light.primary.background = Rgb::new(0xfa, 0xfa, 0xf8);
        light.primary.foreground = Rgb::new(0x2a, 0x2a, 0x2e);
        let titles = ["PowerShell — C:\\dev", "Ubuntu / WSL", "Build & tests", "ssh rex@studio"];
        // Show the real icons of this machine's shells next to the built-in badges.
        let profiles = Profiles::discover();
        let sources: Vec<_> = profiles
            .list
            .iter()
            .map(|profile| profiles::icon_source(Some(profile), None))
            .collect();
        let icon_of = |name: &str| {
            let index = profiles.list.iter().position(|profile| profile.name.contains(name))?;
            sources[index].as_deref()
        };
        let icons = [icon_of("PowerShell"), None, icon_of("Command Prompt"), None];
        let cache = Icons::default();
        for (name, colors, scale, width, hovered, frame) in [
            ("dark", &dark, 1.25, 1200, Hit::Tab(2), Frame::Normal),
            ("light", &light, 1., 1100, Hit::New, Frame::Normal),
            ("narrow", &dark, 1.5, 760, Hit::Close, Frame::Normal),
            ("drag", &dark, 1.25, 1200, Hit::None, Frame::Normal),
            ("fullscreen", &dark, 1.25, 1200, Hit::Maximize, Frame::Fullscreen),
        ] {
            let height = (190. * scale) as u32;
            let header = Header::new(PhysicalSize::new(width, height), scale, frame, 4, 0, 0);
            let palette = Palette::new(colors);
            let size = Scene::badge_size(scale);
            let scene = Scene {
                header: &header,
                palette: &palette,
                hovered,
                // Lift the selected tab partway over its neighbour.
                dragged: (name == "drag").then_some((0, (90. * scale) as i32)),
                host_name: "WORKSTATION",
                title: &|index| titles[index],
                icon: &|index| icons[index].and_then(|path| cache.get(path, size)),
            };
            let mut bmp = Vec::new();
            unsafe {
                scene.render(Rect::new(0, 0, width as i32, height as i32), |_, pixels| {
                    let size = (pixels.len() * 4) as u32;
                    bmp.extend_from_slice(b"BM");
                    bmp.extend_from_slice(&(54 + size).to_le_bytes());
                    bmp.extend_from_slice(&[0; 4]);
                    bmp.extend_from_slice(&54u32.to_le_bytes());
                    bmp.extend_from_slice(&40u32.to_le_bytes());
                    bmp.extend_from_slice(&(width as i32).to_le_bytes());
                    bmp.extend_from_slice(&(-(height as i32)).to_le_bytes());
                    bmp.extend_from_slice(&1u16.to_le_bytes());
                    bmp.extend_from_slice(&32u16.to_le_bytes());
                    bmp.extend_from_slice(&[0; 24]);
                    pixels.iter().for_each(|pixel| bmp.extend_from_slice(&pixel.to_le_bytes()));
                });
            }
            assert!(!bmp.is_empty());
            let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work");
            std::fs::create_dir_all(&output).unwrap();
            std::fs::write(output.join(format!("header-preview-{name}.bmp")), bmp).unwrap();
        }
    }

    #[test]
    fn removing_tabs_preserves_the_selected_session() {
        assert_eq!(selected_after_removal(3, 0, 4), 2);
        assert_eq!(selected_after_removal(1, 3, 4), 1);
        assert_eq!(selected_after_removal(2, 2, 4), 2);
        assert_eq!(selected_after_removal(4, 4, 4), 3);
        assert_eq!(selected_after_removal(0, 0, 0), 0);
    }

    #[test]
    fn header_keeps_selected_tabs_and_controls_reachable() {
        for frame in [Frame::Normal, Frame::Maximized, Frame::Fullscreen] {
            for scale in [1., 1.25, 1.5, 2.] {
                for width in [480., 800., 1600.] {
                    for count in [1, 3, 30] {
                        for selected in 0..count {
                            let header = Header::new(
                                PhysicalSize::new((width * scale) as u32, (600. * scale) as u32),
                                scale,
                                frame,
                                count,
                                selected,
                                0,
                            );
                            assert!(header.tabs.iter().any(|(index, _)| *index == selected));
                            let mut previous_right = 0;
                            for &(index, rect) in &header.tabs {
                                assert!(rect.width() > header.close_width);
                                assert!(rect.left >= previous_right);
                                assert_eq!(
                                    header.hit(PhysicalPosition::new(
                                        f64::from(rect.left + 2),
                                        f64::from(rect.top + 2)
                                    )),
                                    Hit::Tab(index)
                                );
                                let close = header.close_tab(rect);
                                assert_eq!(
                                    header.hit(PhysicalPosition::new(
                                        f64::from(close.left + 1),
                                        f64::from(close.top + 1)
                                    )),
                                    Hit::CloseTab(index)
                                );
                                previous_right = rect.right;
                            }
                            for &(hit, rect) in &header.buttons {
                                assert!(rect.left >= previous_right);
                                assert!(rect.right <= header.width - header.border);
                                assert_eq!(
                                    header.hit(PhysicalPosition::new(
                                        f64::from((rect.left + rect.right) / 2),
                                        f64::from((rect.top + rect.bottom) / 2)
                                    )),
                                    hit
                                );
                                previous_right = rect.right;
                            }
                            let find = |target: Hit| {
                                header.buttons.iter().find(|(hit, _)| *hit == target).unwrap().1
                            };
                            let (plus, profiles) = (find(Hit::New), find(Hit::Profiles));
                            let last_tab = header.tabs.last().unwrap().1;
                            // The new-tab split button follows the tabs, overflow arrows aside.
                            let arrows = if count > header.tabs.len() { 64. * scale } else { 0. };
                            assert!(f64::from(plus.left - last_tab.right) <= 12. * scale + arrows);
                            assert_eq!(plus.right, profiles.left);
                            assert_eq!(
                                header.hit(PhysicalPosition::new(
                                    f64::from(profiles.right + 2),
                                    f64::from(header.bar / 2)
                                )),
                                Hit::Drag
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fullscreen_terminal_fills_the_window_below_the_header() {
        for scale in [1., 1.25, 1.5, 2.] {
            for (width, height) in [(1280., 720.), (1920., 1080.), (3840., 2160.)] {
                let size = PhysicalSize::new((width * scale) as u32, (height * scale) as u32);
                let header = Header::new(size, scale, Frame::Fullscreen, 3, 0, 0);
                let (width, height) = (size.width as i32, size.height as i32);
                assert_eq!(header.terminal, Rect::new(0, header.bar, width, height));
                assert_eq!(header.bar, strip_height(scale));
                // Only the header strip is painted; the terminal covers the rest.
                assert_eq!(header.chrome(), vec![Rect::new(0, 0, width, header.bar)]);
                assert!(!header.brand_label);
                assert_eq!(header.border, 0);
                for (x, y) in [(0., 0.), (f64::from(width - 1), 0.), (0., f64::from(height - 1))] {
                    assert!(!matches!(header.hit(PhysicalPosition::new(x, y)), Hit::Resize(_)));
                }
                for target in [Hit::New, Hit::Profiles, Hit::Settings, Hit::Maximize, Hit::Close] {
                    let rect = header.buttons.iter().find(|(hit, _)| *hit == target).unwrap().1;
                    let (x, y) = rect.center();
                    assert_eq!(header.hit(PhysicalPosition::new(x, y)), target);
                }
                // The label's space goes to the tabs.
                let windowed = Header::new(size, scale, Frame::Maximized, 3, 0, 0);
                assert!(windowed.brand_label);
                assert!(header.tabs[0].1.left < windowed.tabs[0].1.left);
                assert!(windowed.terminal.left > 0 && windowed.terminal.bottom < height);
            }
        }
    }

    #[test]
    fn dragged_tabs_drop_into_the_slot_under_their_center() {
        let header = Header::new(PhysicalSize::new(1600, 600), 1., Frame::Normal, 4, 0, 0);
        let slot = |index: usize| f64::from(header.tabs[index].1.left);
        let half = f64::from(header.step) / 2.;
        assert_eq!(header.drop_target(slot(0)), Some(0));
        assert_eq!(header.drop_target(slot(0) + half - 2.), Some(0));
        assert_eq!(header.drop_target(slot(0) + half + 2.), Some(1));
        assert_eq!(header.drop_target(slot(2) + 3.), Some(2));
        // Positions beyond either end clamp to the first and last visible slots.
        assert_eq!(header.drop_target(-500.), Some(0));
        assert_eq!(header.drop_target(5000.), Some(3));
        let empty = Header::new(PhysicalSize::new(1600, 600), 1., Frame::Normal, 0, 0, 0);
        assert_eq!(empty.drop_target(10.), None);
    }

    #[test]
    fn wsl_profiles_open_their_distribution_at_home() {
        assert_eq!(profiles::wsl_command_line(None), "wsl.exe --cd ~");
        assert_eq!(profiles::wsl_command_line(Some("Ubuntu")), "wsl.exe -d Ubuntu --cd ~");
        let discovered = Profiles::discover();
        assert!(discovered.list.iter().any(|profile| profile.name == "Command Prompt"));
    }

    #[test]
    fn settings_sits_left_of_the_caption_controls() {
        for scale in [1., 1.5] {
            let width = (1200. * scale) as u32;
            let header = Header::new(PhysicalSize::new(width, 800), scale, Frame::Normal, 3, 0, 0);
            let find =
                |target: Hit| header.buttons.iter().find(|(hit, _)| *hit == target).unwrap().1;
            let (settings, minimize) = (find(Hit::Settings), find(Hit::Minimize));
            assert!(settings.right <= minimize.left);
            assert!(f64::from(minimize.left - settings.right) <= 6. * scale);
            let (x, y) = settings.center();
            assert_eq!(header.hit(PhysicalPosition::new(x, y)), Hit::Settings);
            // A drag target remains between the profile dropdown and the gear.
            let profiles = find(Hit::Profiles);
            assert!(f64::from(settings.left - profiles.right) >= 48. * scale);
        }
    }

    #[test]
    fn border_hit_testing_respects_maximized_and_terminal_areas() {
        let header = Header::new(PhysicalSize::new(1000, 650), 1., Frame::Normal, 2, 0, 0);
        for (x, y, direction) in [
            (0., 0., ResizeDirection::NorthWest),
            (999., 0., ResizeDirection::NorthEast),
            (0., 649., ResizeDirection::SouthWest),
            (999., 649., ResizeDirection::SouthEast),
            (500., 0., ResizeDirection::North),
            (500., 649., ResizeDirection::South),
            (0., 300., ResizeDirection::West),
            (999., 300., ResizeDirection::East),
        ] {
            assert_eq!(header.hit(PhysicalPosition::new(x, y)), Hit::Resize(direction));
        }
        assert_eq!(header.hit(PhysicalPosition::new(500., 300.)), Hit::None);
        let header = Header::new(PhysicalSize::new(1000, 650), 1., Frame::Maximized, 2, 0, 0);
        assert_eq!(header.border, 0);
        assert_eq!(header.hit(PhysicalPosition::new(0., 0.)), Hit::Drag);
        assert_eq!(header.hit(PhysicalPosition::new(999., 649.)), Hit::None);
    }
}

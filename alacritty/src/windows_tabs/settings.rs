//! The profile settings panel, drawn in the header's style.
//!
//! The layout follows Windows Terminal and iTerm2: profiles on the left, the selected
//! profile's details on the right. Text entry uses native edit controls for editing, IME, and
//! clipboard support, painted with the header palette. Everything else is drawn here.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWCP_ROUND, DwmExtendFrameIntoClientArea};
use windows_sys::Win32::Graphics::Gdi::{
    CreateSolidBrush, DT_CENTER, DT_RIGHT, DT_VCENTER, DeleteObject, GetMonitorInfoW, HBRUSH, HDC,
    HFONT, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    ScreenToClient, SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::{MARGINS, SetWindowTheme, WM_MOUSELEAVE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_ESCAPE,
    VK_RETURN, VK_SHIFT, VK_TAB,
};
use windows_sys::Win32::UI::Shell::{
    BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW, SHGetPathFromIDListW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DI_NORMAL, DestroyIcon, DestroyWindow, DrawIconEx,
    ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_WANTRETURN, GWLP_USERDATA, GWLP_WNDPROC,
    GetParent, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, HTCAPTION,
    HTCLIENT, IsWindow, MoveWindow, SW_SHOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT, WM_DPICHANGED,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCALCSIZE,
    WM_NCHITTEST, WM_SETFONT, WNDPROC, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_THICKFRAME, WS_VISIBLE,
};

use super::Shared;
use super::paint::{
    Canvas, Color, Icon, Palette, Rect, draw_text, font, icon, load_logo, prompt_glyph, wide,
};
use super::profiles::{self, Profile, Profiles};
use super::surface::{self, Handler};

/// Logical size of the panel, in DIPs.
const WIDTH: f64 = 920.;
const HEIGHT: f64 = 660.;
const TITLE: f64 = 48.;

/// Edit-control notifications and commands forwarded by the edit subclass.
const EN_SETFOCUS: u32 = 0x0100;
const EN_KILLFOCUS: u32 = 0x0200;
const EN_CHANGE: u32 = 0x0300;
const COMMAND_SAVE: usize = 1;
const COMMAND_CLOSE: usize = 2;
const COMMAND_NEXT: usize = 3;
const COMMAND_PREVIOUS: usize = 4;
const EM_SETSEL: u32 = 0x00b1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Command,
    Directory,
    Icon,
    Env,
    Options,
}

impl Field {
    const ALL: [Field; 6] =
        [Field::Name, Field::Command, Field::Directory, Field::Icon, Field::Env, Field::Options];

    fn index(self) -> usize {
        Self::ALL.iter().position(|field| *field == self).unwrap()
    }

    fn multiline(self) -> bool {
        matches!(self, Field::Env | Field::Options)
    }

    fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Command => "Command line",
            Field::Directory => "Starting directory",
            Field::Icon => "Icon",
            Field::Env => "Environment variables",
            Field::Options => "Configuration overrides",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Field::Name => "",
            Field::Command => "Empty uses the shell from alacritty.toml",
            Field::Directory => "Empty uses Alacritty's working directory",
            Field::Icon => "Empty uses the program's own icon",
            Field::Env => "KEY=value, one per line",
            Field::Options => "e.g. font.size=14",
        }
    }
}

/// A profile being edited, kept as the text the user typed until it is saved.
struct Entry {
    text: [String; 6],
    hidden: bool,
    icon: Option<PathBuf>,
}

impl Entry {
    fn new(profile: &Profile, shell: Option<&str>) -> Self {
        let lines = |lines: Vec<String>| lines.join("\r\n");
        let mut entry = Self {
            text: [
                profile.name.clone(),
                profile.command.clone(),
                profile.directory.clone(),
                profile.icon.clone(),
                lines(profile.env.iter().map(|(key, value)| format!("{key}={value}")).collect()),
                lines(profile.options.clone()),
            ],
            hidden: profile.hidden,
            icon: None,
        };
        entry.refresh_icon(shell);
        entry
    }

    fn text(&self, field: Field) -> &str {
        &self.text[field.index()]
    }

    fn name(&self) -> &str {
        self.text(Field::Name).trim()
    }

    fn lines(&self, field: Field) -> impl Iterator<Item = &str> {
        self.text(field).lines().map(str::trim).filter(|line| !line.is_empty())
    }

    /// The profile as typed, without validation.
    fn draft(&self) -> Profile {
        Profile {
            name: self.name().to_owned(),
            command: self.text(Field::Command).trim().to_owned(),
            directory: self.text(Field::Directory).trim().to_owned(),
            icon: self.text(Field::Icon).trim().to_owned(),
            env: self
                .lines(Field::Env)
                .filter_map(|line| line.split_once('='))
                .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
                .collect(),
            options: self.lines(Field::Options).map(str::to_owned).collect(),
            hidden: self.hidden,
        }
    }

    fn refresh_icon(&mut self, shell: Option<&str>) {
        self.icon = profiles::icon_source(Some(&self.draft()), shell);
    }

    /// The profile, or a message explaining what to fix.
    fn validate(&self) -> Result<Profile, String> {
        let profile = self.draft();
        let name = &profile.name;
        if name.is_empty() {
            return Err("Every profile needs a name".into());
        }
        if !profile.directory.is_empty()
            && !std::path::Path::new(&profiles::expand(&profile.directory)).is_dir()
        {
            return Err(format!("{name}: the starting directory does not exist"));
        }
        if !profile.icon.is_empty()
            && !std::path::Path::new(&profiles::expand(&profile.icon)).is_file()
        {
            return Err(format!("{name}: the icon file does not exist"));
        }
        for line in self.lines(Field::Env) {
            if !line.split_once('=').is_some_and(|(key, _)| !key.trim().is_empty()) {
                return Err(format!("{name}: write environment variables as KEY=value"));
            }
        }
        for line in self.lines(Field::Options) {
            if toml::from_str::<toml::Table>(line).is_err() {
                return Err(format!("{name}: `{line}` is not a key=value option"));
            }
        }
        Ok(profile)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Close,
    Row(usize),
    NewProfile,
    IconTile,
    Browse(Field),
    ResetIcon,
    ToggleDefault,
    ToggleShown,
    Delete,
    Cancel,
    Save,
}

/// Geometry of the panel in client pixels.
struct Layout {
    close: Rect,
    sidebar: Rect,
    rows: Vec<(usize, Rect)>,
    new_profile: Rect,
    card: Rect,
    tile: Rect,
    fields: [Rect; 6],
    labels: [Rect; 6],
    browse: [(Field, Rect); 3],
    reset: Rect,
    toggle_default: Rect,
    toggle_shown: Rect,
    delete: Rect,
    cancel: Rect,
    save: Rect,
    status: Rect,
}

pub(super) struct Settings {
    shared: Rc<Shared>,
    palette: Palette,
    scale: f64,
    width: i32,
    height: i32,
    entries: Vec<Entry>,
    default: Option<usize>,
    selected: usize,
    scroll: usize,
    edits: [HWND; 6],
    focused: Option<Field>,
    edit_font: HFONT,
    name_font: HFONT,
    field_brush: HBRUSH,
    hovered: Option<Target>,
    pressed: Option<Target>,
    tracking: bool,
    dirty: bool,
    confirm_close: bool,
    status: Option<(String, bool)>,
    pending: Option<Field>,
}

/// Show the settings panel for `owner`, or bring the open one forward.
pub(super) fn open(owner: HWND, palette: &Palette, shared: &Rc<Shared>) {
    unsafe {
        let open = shared.settings.get();
        if !open.is_null() && IsWindow(open) != 0 {
            SetForegroundWindow(open);
            return;
        }

        let scale = surface::scale(owner);
        let (width, height) = ((WIDTH * scale).round() as i32, (HEIGHT * scale).round() as i32);
        // Center over the tab window, kept on its monitor.
        let mut frame: RECT = std::mem::zeroed();
        GetWindowRect(owner, &mut frame);
        let mut left = (frame.left + frame.right - width) / 2;
        let mut top = (frame.top + frame.bottom - height) / 2;
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST), &mut info) != 0 {
            let work = info.rcWork;
            left = left.clamp(work.left, (work.right - width).max(work.left));
            top = top.clamp(work.top, (work.bottom - height).max(work.top));
        }

        let profiles = shared.profiles.borrow().clone();
        let shell = shared.shell.as_deref();
        let entries: Vec<Entry> =
            profiles.list.iter().map(|profile| Entry::new(profile, shell)).collect();
        let default = profiles.list.iter().position(|profile| profile.name == profiles.default);
        let state = Settings {
            shared: shared.clone(),
            palette: palette.clone(),
            scale,
            width,
            height,
            entries,
            default,
            selected: 0,
            scroll: 0,
            edits: [std::ptr::null_mut(); 6],
            focused: None,
            edit_font: std::ptr::null_mut(),
            name_font: std::ptr::null_mut(),
            field_brush: CreateSolidBrush(palette.field.colorref()),
            hovered: None,
            pressed: None,
            tracking: false,
            dirty: false,
            confirm_close: false,
            status: None,
            pending: None,
        };

        // A frame keeps DWM's shadow and snapping; WM_NCCALCSIZE gives its area to the client.
        let hwnd = surface::create(
            "AlacrittyTabsSettings",
            false,
            0,
            WS_POPUP | WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_CLIPCHILDREN,
            owner,
            Rect::new(left, top, left + width, top + height),
            state,
        );
        if hwnd.is_null() {
            return;
        }
        let title = wide("Alacritty Settings");
        SetWindowTextW(hwnd, title.as_ptr());
        let margins =
            MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 };
        DwmExtendFrameIntoClientArea(hwnd, &margins);
        surface::round_corners(hwnd, DWMWCP_ROUND, palette.popup_outline);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );

        if let Some(state) = surface::state::<Settings>(hwnd) {
            let mut settings = state.borrow_mut();
            settings.create_edits(hwnd);
            settings.show(0);
        }
        shared.settings.set(hwnd);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

impl Settings {
    fn px(&self, value: f64) -> i32 {
        (value * self.scale).round() as i32
    }

    fn layout(&self) -> Layout {
        let px = |value: f64| self.px(value);
        let (w, h) = (self.width, self.height);
        let sidebar = Rect::new(px(8.), px(TITLE), px(248.), h - px(8.));
        let new_profile =
            Rect::new(px(14.), sidebar.bottom - px(42.), px(242.), sidebar.bottom - px(6.));
        let mut rows = Vec::new();
        let mut top = sidebar.top + px(36.);
        for index in self.scroll..self.entries.len() {
            if top + px(36.) > new_profile.top - px(6.) {
                break;
            }
            rows.push((index, Rect::new(px(14.), top, px(242.), top + px(36.))));
            top += px(38.);
        }

        let card = Rect::new(px(256.), px(TITLE), w - px(8.), h - px(8.));
        let (x0, x1) = (card.left + px(28.), card.right - px(28.));
        let y = |value: f64| card.top + px(value);
        let tile = Rect::new(x0, y(24.), x0 + px(52.), y(76.));
        let name = Rect::new(tile.right + px(16.), y(24.), x1, y(60.));
        let label = |top: f64| Rect::new(x0, y(top), x1, y(top + 18.));
        let mid = (x0 + x1) / 2;
        let button = px(88.);
        let fields = [
            name,
            Rect::new(x0, y(122.), x1 - button - px(8.), y(156.)),
            Rect::new(x0, y(194.), x1 - button - px(8.), y(228.)),
            Rect::new(x0, y(266.), x1 - 2 * (button + px(8.)), y(300.)),
            Rect::new(x0, y(338.), mid - px(8.), y(448.)),
            Rect::new(mid + px(8.), y(338.), x1, y(448.)),
        ];
        let labels = [
            Rect::new(name.left, y(62.), x1, y(78.)),
            label(98.),
            label(170.),
            label(242.),
            Rect::new(x0, y(314.), mid - px(8.), y(332.)),
            Rect::new(mid + px(8.), y(314.), x1, y(332.)),
        ];
        let beside = |field: Rect, offset: i32| {
            Rect::new(
                field.right + px(8.) + offset,
                field.top,
                field.right + px(8.) + offset + button,
                field.bottom,
            )
        };
        let footer = Rect::new(x0, card.bottom - px(58.), x1, card.bottom - px(24.));
        Layout {
            close: Rect::new(w - px(48.), px(8.), w - px(8.), px(40.)),
            sidebar,
            rows,
            new_profile,
            card,
            tile,
            fields,
            labels,
            browse: [
                (Field::Command, beside(fields[1], 0)),
                (Field::Directory, beside(fields[2], 0)),
                (Field::Icon, beside(fields[3], 0)),
            ],
            reset: beside(fields[3], button + px(8.)),
            toggle_default: Rect::new(x0, y(468.), x0 + px(230.), y(500.)),
            toggle_shown: Rect::new(x0 + px(250.), y(468.), x0 + px(480.), y(500.)),
            delete: Rect::new(footer.left, footer.top, footer.left + px(128.), footer.bottom),
            cancel: Rect::new(x1 - px(196.), footer.top, x1 - px(104.), footer.bottom),
            save: Rect::new(x1 - px(96.), footer.top, x1, footer.bottom),
            status: Rect::new(footer.left + px(140.), footer.top, x1 - px(208.), footer.bottom),
        }
    }

    fn target_at(&self, x: i32, y: i32) -> Option<Target> {
        let point = winit::dpi::PhysicalPosition::new(f64::from(x), f64::from(y));
        let layout = self.layout();
        let targets = [
            (Target::Close, layout.close),
            (Target::NewProfile, layout.new_profile),
            (Target::IconTile, layout.tile),
            (Target::ResetIcon, layout.reset),
            (Target::ToggleDefault, layout.toggle_default),
            (Target::ToggleShown, layout.toggle_shown),
            (Target::Delete, layout.delete),
            (Target::Cancel, layout.cancel),
            (Target::Save, layout.save),
        ];
        let rows = layout.rows.iter().map(|&(index, rect)| (Target::Row(index), rect));
        let browse = layout.browse.iter().map(|&(field, rect)| (Target::Browse(field), rect));
        targets
            .into_iter()
            .chain(rows)
            .chain(browse)
            .find(|(target, rect)| rect.contains(point) && self.enabled(*target))
            .map(|(target, _)| target)
    }

    fn enabled(&self, target: Target) -> bool {
        match target {
            Target::Delete => self.entries.len() > 1,
            Target::Save => self.dirty,
            Target::ResetIcon => !self.entry().text(Field::Icon).is_empty(),
            _ => true,
        }
    }

    fn entry(&self) -> &Entry {
        &self.entries[self.selected]
    }

    unsafe fn create_edits(&mut self, hwnd: HWND) {
        let class = wide("EDIT");
        for field in Field::ALL {
            let style = WS_CHILD | WS_VISIBLE | WS_TABSTOP;
            let style = if field.multiline() {
                style | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_WANTRETURN as u32
            } else {
                style | ES_AUTOHSCROLL as u32
            };
            unsafe {
                let edit = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    style,
                    0,
                    0,
                    1,
                    1,
                    hwnd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                // Route Tab, Escape, and Ctrl shortcuts through the panel.
                let previous =
                    SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_procedure as *const () as isize);
                SetWindowLongPtrW(edit, GWLP_USERDATA, previous);
                if self.palette.terminal.luma() < 0.5 {
                    let theme = wide("DarkMode_Explorer");
                    SetWindowTheme(edit, theme.as_ptr(), std::ptr::null());
                }
                self.edits[field.index()] = edit;
            }
        }
        self.update_fonts();
    }

    fn update_fonts(&mut self) {
        unsafe {
            for font in [self.edit_font, self.name_font] {
                if !font.is_null() {
                    DeleteObject(font);
                }
            }
            self.edit_font = font(12.5 * self.scale, 400);
            self.name_font = font(16. * self.scale, 600);
            for field in Field::ALL {
                let font = if field == Field::Name { self.name_font } else { self.edit_font };
                SendMessageW(self.edits[field.index()], WM_SETFONT, font as usize, 1);
            }
        }
        self.place_edits();
    }

    /// Fit each edit control inside its drawn field.
    fn place_edits(&self) {
        let layout = self.layout();
        for field in Field::ALL {
            let rect = layout.fields[field.index()];
            let inner = if field.multiline() {
                Rect::new(
                    rect.left + self.px(10.),
                    rect.top + self.px(7.),
                    rect.right - self.px(4.),
                    rect.bottom - self.px(7.),
                )
            } else {
                let height = self.px(if field == Field::Name { 24. } else { 20. });
                let top = rect.top + (rect.height() - height) / 2;
                Rect::new(rect.left + self.px(10.), top, rect.right - self.px(10.), top + height)
            };
            unsafe {
                MoveWindow(
                    self.edits[field.index()],
                    inner.left,
                    inner.top,
                    inner.width(),
                    inner.height(),
                    1,
                );
            }
        }
    }

    /// Show the profile at `index` in the edit controls.
    /// Keep the current profile's edits and show the profile at `index`.
    fn select(&mut self, index: usize) {
        self.sync();
        self.show(index);
    }

    /// Show the profile at `index` in the edit controls, discarding what they hold.
    fn show(&mut self, index: usize) {
        self.selected = index.min(self.entries.len().saturating_sub(1));
        let visible = self.layout().rows.len().max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
        for field in Field::ALL {
            let text = wide(self.entries[self.selected].text(field));
            // The resulting EN_CHANGE arrives while this state is borrowed and is ignored.
            unsafe { SetWindowTextW(self.edits[field.index()], text.as_ptr()) };
        }
    }

    fn set_field(&mut self, field: Field, text: String) {
        let wide_text = wide(&text);
        unsafe { SetWindowTextW(self.edits[field.index()], wide_text.as_ptr()) };
        self.entries[self.selected].text[field.index()] = text;
        self.changed(field);
    }

    fn changed(&mut self, field: Field) {
        self.dirty = true;
        self.confirm_close = false;
        self.status = None;
        if matches!(field, Field::Name | Field::Command | Field::Icon) {
            let shell = self.shared.shell.clone();
            self.entries[self.selected].refresh_icon(shell.as_deref());
        }
    }

    fn activate(&mut self, hwnd: HWND, target: Target) {
        match target {
            Target::Close | Target::Cancel => self.close(hwnd),
            Target::Row(index) => self.select(index),
            Target::NewProfile => {
                let profile = Profile { name: "New profile".into(), ..Profile::default() };
                let shell = self.shared.shell.clone();
                self.entries.push(Entry::new(&profile, shell.as_deref()));
                self.dirty = true;
                self.select(self.entries.len() - 1);
                let name = self.edits[Field::Name.index()];
                unsafe {
                    SetFocus(name);
                    SendMessageW(name, EM_SETSEL, 0, -1);
                }
            },
            Target::IconTile => self.pending = Some(Field::Icon),
            Target::Browse(field) => self.pending = Some(field),
            Target::ResetIcon => self.set_field(Field::Icon, String::new()),
            Target::ToggleDefault => {
                self.default = (self.default != Some(self.selected)).then_some(self.selected);
                self.changed(Field::Name);
            },
            Target::ToggleShown => {
                self.entries[self.selected].hidden ^= true;
                self.changed(Field::Name);
            },
            Target::Delete => {
                let removed = self.selected;
                self.entries.remove(removed);
                self.default = match self.default {
                    Some(default) if default == removed => None,
                    Some(default) if default > removed => Some(default - 1),
                    default => default,
                };
                self.dirty = true;
                self.show(removed);
            },
            Target::Save => self.save(),
        }
    }

    fn save(&mut self) {
        self.sync();
        let mut list = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            match entry.validate() {
                Ok(profile) if list.iter().any(|other: &Profile| other.name == profile.name) => {
                    self.status = Some((format!("Two profiles are named {}", profile.name), true));
                    self.select(index);
                    return;
                },
                Ok(profile) => list.push(profile),
                Err(message) => {
                    self.status = Some((message, true));
                    self.select(index);
                    return;
                },
            }
        }
        let default = self.default.map(|index| list[index].name.clone()).unwrap_or_default();
        let profiles = Profiles { default, list };
        match profiles.save() {
            Ok(path) => {
                *self.shared.profiles.borrow_mut() = profiles;
                self.dirty = false;
                self.confirm_close = false;
                self.status = Some((format!("Saved to {}", path.display()), false));
            },
            Err(err) => self.status = Some((format!("Could not save: {err}"), true)),
        }
    }

    fn close(&mut self, hwnd: HWND) {
        if self.dirty && !self.confirm_close {
            self.confirm_close = true;
            self.status = Some(("Unsaved changes. Close again to discard them.".into(), true));
            return;
        }
        unsafe { DestroyWindow(hwnd) };
    }

    fn read(&self, field: Field) -> String {
        let edit = self.edits[field.index()];
        unsafe {
            let len = GetWindowTextLengthW(edit).max(0) as usize;
            let mut buffer = vec![0u16; len + 1];
            let len = GetWindowTextW(edit, buffer.as_mut_ptr(), buffer.len() as i32);
            String::from_utf16_lossy(&buffer[..len.max(0) as usize])
        }
    }

    /// Copy the edit controls into the selected entry.
    ///
    /// Multi-line edit controls do not report every change, such as text set by assistive
    /// tools, so the controls are the source of truth when leaving a profile or saving.
    fn sync(&mut self) {
        for field in Field::ALL {
            let text = self.read(field);
            let entry = &mut self.entries[self.selected];
            if entry.text[field.index()] != text {
                entry.text[field.index()] = text;
                self.dirty = true;
            }
        }
    }

    fn edit_field(&self, edit: HWND) -> Option<Field> {
        Field::ALL.into_iter().find(|field| self.edits[field.index()] == edit)
    }

    fn move_focus(&self, forward: bool) {
        let order = Field::ALL;
        let current = self.focused.map_or(0, Field::index);
        let next = if forward {
            (current + 1) % order.len()
        } else {
            (current + order.len() - 1) % order.len()
        };
        unsafe {
            SetFocus(self.edits[next]);
            SendMessageW(self.edits[next], EM_SETSEL, 0, -1);
        }
    }

    fn row_icon(
        &self,
        entry: &Entry,
        size: i32,
    ) -> Option<windows_sys::Win32::UI::WindowsAndMessaging::HICON> {
        entry.icon.as_deref().and_then(|path| self.shared.icons.get(path, size))
    }

    fn button(&self, canvas: &mut Canvas<'_>, rect: Rect, target: Target) {
        let palette = &self.palette;
        let radius = f64::from(self.px(8.));
        let hovered = self.hovered == Some(target) && self.enabled(target);
        let hairline = self.scale.max(1.).floor();
        match target {
            Target::Save if self.dirty => {
                let color = if hovered {
                    palette.foreground.mix(palette.terminal, 0.12)
                } else {
                    palette.foreground
                };
                canvas.fill(rect, radius, color);
            },
            Target::Delete => {
                if hovered {
                    canvas.fill(rect, radius, palette.terminal.mix(palette.danger, 0.18));
                }
            },
            _ => {
                let color = if hovered {
                    palette.raised.mix(palette.foreground, 0.06)
                } else {
                    palette.raised
                };
                canvas.fill(rect, radius, color);
                canvas.stroke(rect, radius, hairline, palette.raised_outline);
            },
        }
    }

    fn toggle(&self, canvas: &mut Canvas<'_>, rect: Rect, on: bool) {
        let palette = &self.palette;
        let track = Rect::new(
            rect.left,
            rect.top + (rect.height() - self.px(22.)) / 2,
            rect.left + self.px(40.),
            0,
        );
        let track = Rect::new(track.left, track.top, track.right, track.top + self.px(22.));
        let radius = f64::from(track.height()) / 2.;
        let knob_size = self.px(14.);
        let knob_left =
            if on { track.right - self.px(4.) - knob_size } else { track.left + self.px(4.) };
        let knob = Rect::new(
            knob_left,
            track.top + self.px(4.),
            knob_left + knob_size,
            track.top + self.px(4.) + knob_size,
        );
        if on {
            canvas.fill(track, radius, palette.foreground);
            canvas.fill(knob, f64::from(knob_size) / 2., palette.terminal);
        } else {
            canvas.fill(track, radius, palette.field);
            canvas.stroke(track, radius, self.scale.max(1.).floor(), palette.focus);
            canvas.fill(knob, f64::from(knob_size) / 2., palette.muted);
        }
    }
}

impl Handler for Settings {
    fn message(
        &mut self,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        let redraw = || unsafe {
            InvalidateRect(hwnd, std::ptr::null(), 0);
        };
        match message {
            // The whole window is client area; the frame only provides DWM's shadow.
            WM_NCCALCSIZE if wparam != 0 => Some(0),
            WM_NCHITTEST => {
                let (x, y) = surface::point(lparam);
                let mut point = POINT { x, y };
                unsafe { ScreenToClient(hwnd, &mut point) };
                let caption =
                    point.y < self.px(TITLE) && self.target_at(point.x, point.y).is_none();
                Some(if caption { HTCAPTION as isize } else { HTCLIENT as isize })
            },
            WM_CTLCOLOREDIT => unsafe {
                let dc = wparam as HDC;
                SetTextColor(dc, self.palette.foreground.colorref());
                SetBkColor(dc, self.palette.field.colorref());
                Some(self.field_brush as isize)
            },
            WM_COMMAND => {
                let (code, id) = ((wparam >> 16) as u32 & 0xffff, wparam & 0xffff);
                if lparam == 0 {
                    match id {
                        COMMAND_SAVE => self.save(),
                        COMMAND_CLOSE => self.close(hwnd),
                        COMMAND_NEXT => self.move_focus(true),
                        COMMAND_PREVIOUS => self.move_focus(false),
                        _ => return None,
                    }
                    redraw();
                    return Some(0);
                }
                let field = self.edit_field(lparam as HWND)?;
                match code {
                    EN_CHANGE => {
                        self.entries[self.selected].text[field.index()] = self.read(field);
                        self.changed(field);
                    },
                    EN_SETFOCUS => self.focused = Some(field),
                    EN_KILLFOCUS => self.focused = None,
                    _ => return None,
                }
                redraw();
                Some(0)
            },
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
                let hovered = self.target_at(x, y);
                if hovered != self.hovered {
                    self.hovered = hovered;
                    redraw();
                }
                Some(0)
            },
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.hovered = None;
                redraw();
                Some(0)
            },
            WM_LBUTTONDOWN => {
                let (x, y) = surface::point(lparam);
                self.pressed = self.target_at(x, y);
                Some(0)
            },
            WM_LBUTTONUP => {
                let (x, y) = surface::point(lparam);
                let target = self.target_at(x, y);
                if let Some(target) = target.filter(|target| self.pressed.take() == Some(*target)) {
                    self.activate(hwnd, target);
                    redraw();
                }
                Some(0)
            },
            WM_MOUSEWHEEL => {
                let delta = (wparam >> 16) as u16 as i16;
                let visible = self.layout().rows.len();
                let last = self.entries.len().saturating_sub(visible);
                self.scroll = if delta > 0 {
                    self.scroll.saturating_sub(1)
                } else {
                    (self.scroll + 1).min(last)
                };
                redraw();
                Some(0)
            },
            WM_KEYDOWN => {
                let control = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;
                match wparam as u16 {
                    VK_ESCAPE => self.close(hwnd),
                    0x53 if control => self.save(),
                    _ => return None,
                }
                redraw();
                Some(0)
            },
            WM_CLOSE => {
                self.close(hwnd);
                redraw();
                Some(0)
            },
            WM_DPICHANGED => {
                self.scale = f64::from((wparam & 0xffff) as u32) / 96.;
                let rect = unsafe { &*(lparam as *const RECT) };
                self.width = rect.right - rect.left;
                self.height = rect.bottom - rect.top;
                unsafe {
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        rect.left,
                        rect.top,
                        self.width,
                        self.height,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                self.update_fonts();
                redraw();
                Some(0)
            },
            _ => None,
        }
    }

    fn shapes(&self, canvas: &mut Canvas<'_>) {
        let palette = &self.palette;
        let layout = self.layout();
        let px = |value: f64| self.px(value);
        let hairline = self.scale.max(1.).floor();
        canvas.clear(palette.chrome);

        if self.hovered == Some(Target::Close) {
            canvas.fill(layout.close, f64::from(px(8.)), palette.danger);
        }
        let close_color =
            if self.hovered == Some(Target::Close) { Color::WHITE } else { palette.muted };
        icon(canvas, layout.close, Icon::Close, close_color, self.scale);

        // Profiles sidebar, styled like the tab track.
        let radius = f64::from(px(12.));
        canvas.fill(layout.sidebar, radius, palette.track);
        canvas.stroke(layout.sidebar, radius, hairline, palette.track_outline);
        for &(index, rect) in &layout.rows {
            let radius = f64::from(px(9.));
            let surface = if index == self.selected {
                canvas.fill(rect, radius, palette.raised);
                canvas.stroke(rect, radius, hairline, palette.raised_outline);
                palette.raised
            } else if self.hovered == Some(Target::Row(index)) {
                canvas.fill(rect, radius, palette.hover);
                palette.hover
            } else {
                palette.track
            };
            let entry = &self.entries[index];
            if self.row_icon(entry, px(18.)).is_none() {
                let badge = row_icon_rect(rect, self.scale);
                let accent = palette.accent(entry.text(Field::Command));
                canvas.fill(badge, f64::from(px(5.)), surface.mix(accent, 0.3));
                prompt_glyph(canvas, badge, accent, self.scale);
            }
        }
        if self.hovered == Some(Target::NewProfile) {
            canvas.fill(layout.new_profile, f64::from(px(9.)), palette.hover);
        }
        let plus = Rect::new(
            layout.new_profile.left,
            layout.new_profile.top,
            layout.new_profile.left + px(38.),
            layout.new_profile.bottom,
        );
        icon(canvas, plus, Icon::Plus, palette.foreground, self.scale);

        // Profile details, on the same panel as the terminal.
        let radius = f64::from(px(10.));
        canvas.fill(layout.card, radius, palette.terminal);
        canvas.stroke(layout.card, radius, hairline, palette.card_outline);

        let tile_radius = f64::from(px(12.));
        canvas.fill(layout.tile, tile_radius, palette.raised);
        let tile_outline = if self.hovered == Some(Target::IconTile) {
            palette.focus
        } else {
            palette.raised_outline
        };
        canvas.stroke(layout.tile, tile_radius, hairline, tile_outline);
        if self.row_icon(self.entry(), px(32.)).is_none() {
            let badge = layout.tile.square(px(32.));
            let accent = palette.accent(self.entry().text(Field::Command));
            canvas.fill(badge, f64::from(px(8.)), palette.raised.mix(accent, 0.3));
            prompt_glyph(canvas, badge, accent, self.scale * 1.6);
        }

        for field in Field::ALL {
            let rect = layout.fields[field.index()];
            let radius = f64::from(px(7.));
            let outline =
                if self.focused == Some(field) { palette.focus } else { palette.field_outline };
            canvas.fill(rect, radius, palette.field);
            canvas.stroke(rect, radius, hairline, outline);
        }
        for &(field, rect) in &layout.browse {
            self.button(canvas, rect, Target::Browse(field));
        }
        if self.enabled(Target::ResetIcon) {
            self.button(canvas, layout.reset, Target::ResetIcon);
        }
        self.toggle(canvas, layout.toggle_default, self.default == Some(self.selected));
        self.toggle(canvas, layout.toggle_shown, !self.entry().hidden);
        if self.enabled(Target::Delete) {
            self.button(canvas, layout.delete, Target::Delete);
        }
        self.button(canvas, layout.cancel, Target::Cancel);
        self.button(canvas, layout.save, Target::Save);
    }

    unsafe fn gdi(&self, dc: HDC) {
        let palette = &self.palette;
        let layout = self.layout();
        let px = |value: f64| self.px(value);
        unsafe {
            let regular = font(12.5 * self.scale, 400);
            let strong = font(12.5 * self.scale, 600);
            let small = font(11. * self.scale, 400);
            let old_font = SelectObject(dc, regular);
            SetBkMode(dc, TRANSPARENT as i32);
            let text = |font: HFONT, color: Color, value: &str, rect: Rect, align: u32| {
                SelectObject(dc, font);
                SetTextColor(dc, color.colorref());
                draw_text(dc, value, rect, align);
            };
            let draw_icon = |handle, rect: Rect| {
                DrawIconEx(
                    dc,
                    rect.left,
                    rect.top,
                    handle,
                    rect.width(),
                    rect.height(),
                    0,
                    std::ptr::null_mut(),
                    DI_NORMAL,
                );
            };

            // Title bar, matching the header's host label.
            let logo_size = px(24.);
            let logo = load_logo(logo_size);
            let mark = Rect::new(
                px(14.),
                (px(TITLE) - logo_size) / 2,
                px(14.) + logo_size,
                (px(TITLE) + logo_size) / 2,
            );
            if !logo.is_null() {
                draw_icon(logo, mark);
                DestroyIcon(logo);
            }
            let middle = px(TITLE) / 2;
            let title_left = mark.right + px(10.);
            text(
                strong,
                palette.foreground,
                "Settings",
                Rect::new(title_left, 0, px(400.), middle + px(1.)),
                windows_sys::Win32::Graphics::Gdi::DT_BOTTOM,
            );
            text(
                small,
                palette.muted,
                "Profiles",
                Rect::new(title_left, middle + px(1.), px(400.), px(TITLE)),
                windows_sys::Win32::Graphics::Gdi::DT_TOP,
            );

            // Sidebar.
            let section = Rect::new(
                layout.sidebar.left + px(14.),
                layout.sidebar.top + px(8.),
                layout.sidebar.right - px(14.),
                layout.sidebar.top + px(32.),
            );
            text(small, palette.muted, "PROFILES", section, DT_VCENTER);
            for &(index, rect) in &layout.rows {
                let entry = &self.entries[index];
                let badge = row_icon_rect(rect, self.scale);
                if let Some(handle) = self.row_icon(entry, badge.width()) {
                    draw_icon(handle, badge);
                }
                let mut right = rect.right - px(10.);
                if self.default == Some(index) {
                    text(
                        small,
                        palette.muted,
                        "Default",
                        Rect::new(right - px(56.), rect.top, right, rect.bottom),
                        DT_VCENTER | DT_RIGHT,
                    );
                    right -= px(60.);
                }
                let selected = index == self.selected;
                let color =
                    if selected || entry.hidden { palette.muted } else { palette.foreground };
                let color = if selected { palette.foreground } else { color };
                let name = if entry.name().is_empty() { "Untitled" } else { entry.name() };
                let label = Rect::new(badge.right + px(10.), rect.top, right, rect.bottom);
                text(if selected { strong } else { regular }, color, name, label, DT_VCENTER);
            }
            let new_label = Rect::new(
                layout.new_profile.left + px(38.),
                layout.new_profile.top,
                layout.new_profile.right,
                layout.new_profile.bottom,
            );
            text(regular, palette.foreground, "New profile", new_label, DT_VCENTER);

            // Details.
            if let Some(handle) = self.row_icon(self.entry(), px(32.)) {
                draw_icon(handle, layout.tile.square(px(32.)));
            }
            for field in Field::ALL {
                let label = layout.labels[field.index()];
                if field == Field::Name {
                    text(
                        small,
                        palette.muted,
                        "Click the icon to choose your own logo",
                        label,
                        DT_VCENTER,
                    );
                    continue;
                }
                text(strong, palette.foreground, field.label(), label, DT_VCENTER);
                text(small, palette.muted, field.hint(), label, DT_VCENTER | DT_RIGHT);
            }
            for &(_, rect) in &layout.browse {
                text(regular, palette.foreground, "Browse…", rect, DT_VCENTER | DT_CENTER);
            }
            if self.enabled(Target::ResetIcon) {
                text(regular, palette.foreground, "Reset", layout.reset, DT_VCENTER | DT_CENTER);
            }
            for (rect, title, detail) in [
                (layout.toggle_default, "Default profile", "Opens with the new-tab button"),
                (layout.toggle_shown, "Show in dropdown", "Listed in the new-tab menu"),
            ] {
                let left = rect.left + px(52.);
                let middle = (rect.top + rect.bottom) / 2;
                text(
                    regular,
                    palette.foreground,
                    title,
                    Rect::new(left, rect.top - px(4.), rect.right, middle),
                    windows_sys::Win32::Graphics::Gdi::DT_BOTTOM,
                );
                text(
                    small,
                    palette.muted,
                    detail,
                    Rect::new(left, middle, rect.right, rect.bottom + px(4.)),
                    windows_sys::Win32::Graphics::Gdi::DT_TOP,
                );
            }
            if self.enabled(Target::Delete) {
                text(
                    regular,
                    palette.danger.mix(palette.foreground, 0.25),
                    "Delete profile",
                    layout.delete,
                    DT_VCENTER | DT_CENTER,
                );
            }
            text(regular, palette.foreground, "Cancel", layout.cancel, DT_VCENTER | DT_CENTER);
            let save_color = if self.dirty { palette.terminal } else { palette.muted };
            text(strong, save_color, "Save", layout.save, DT_VCENTER | DT_CENTER);
            if let Some((message, error)) = &self.status {
                let color = if *error {
                    palette.danger.mix(palette.foreground, 0.35)
                } else {
                    palette.muted
                };
                text(small, color, message, layout.status, DT_VCENTER | DT_RIGHT);
            }

            SelectObject(dc, old_font);
            DeleteObject(regular);
            DeleteObject(strong);
            DeleteObject(small);
        }
    }

    fn after(state: &Rc<RefCell<Self>>, hwnd: HWND) {
        let Some(field) = state.try_borrow_mut().ok().and_then(|mut state| state.pending.take())
        else {
            return;
        };
        // Modal dialogs pump messages, so the state must not be borrowed while they run.
        let chosen = match field {
            Field::Directory => browse_folder(hwnd),
            Field::Command => browse_file(hwnd, "Programs\0*.exe;*.cmd;*.bat\0All files\0*.*\0\0"),
            _ => browse_file(
                hwnd,
                "Icons and images\0*.ico;*.png;*.jpg;*.jpeg;*.bmp;*.gif;*.exe;*.dll\0All files\0*.*\0\0",
            ),
        };
        let Some(path) = chosen else { return };
        let Ok(mut settings) = state.try_borrow_mut() else { return };
        let text = match field {
            // Keep the arguments when replacing the program.
            Field::Command => {
                let program = if path.contains(' ') { format!("\"{path}\"") } else { path };
                let current = settings.entry().text(Field::Command).to_owned();
                match profiles::split_program(&current).and_then(|parts| parts.get(1).cloned()) {
                    Some(arguments) => format!("{program} {arguments}"),
                    None => program,
                }
            },
            _ => path,
        };
        settings.set_field(field, text);
        unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        self.shared.settings.set(std::ptr::null_mut());
        unsafe {
            for handle in [self.edit_font, self.name_font] {
                if !handle.is_null() {
                    DeleteObject(handle);
                }
            }
            DeleteObject(self.field_brush);
        }
    }
}

fn row_icon_rect(row: Rect, scale: f64) -> Rect {
    let size = (18. * scale).round() as i32;
    let left = row.left + (10. * scale).round() as i32;
    let top = row.top + (row.height() - size) / 2;
    Rect::new(left, top, left + size, top + size)
}

fn browse_file(owner: HWND, filter: &str) -> Option<String> {
    let filter: Vec<u16> = filter.encode_utf16().collect();
    let mut buffer = vec![0u16; 1024];
    unsafe {
        let mut dialog: OPENFILENAMEW = std::mem::zeroed();
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.hwndOwner = owner;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR;
        if GetOpenFileNameW(&mut dialog) == 0 {
            return None;
        }
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

fn browse_folder(owner: HWND) -> Option<String> {
    let title = wide("Choose the starting directory");
    let mut name = vec![0u16; 260];
    unsafe {
        let info = BROWSEINFOW {
            hwndOwner: owner,
            pidlRoot: std::ptr::null_mut(),
            pszDisplayName: name.as_mut_ptr(),
            lpszTitle: title.as_ptr(),
            ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
            lpfn: None,
            lParam: 0,
            iImage: 0,
        };
        let list = SHBrowseForFolderW(&info);
        if list.is_null() {
            return None;
        }
        let mut path = vec![0u16; 1024];
        let found = SHGetPathFromIDListW(list, path.as_mut_ptr()) != 0;
        CoTaskMemFree(list.cast());
        let len = path.iter().position(|&c| c == 0).unwrap_or(0);
        (found && len > 0).then(|| String::from_utf16_lossy(&path[..len]))
    }
}

/// Edit-control subclass that hands navigation and shortcuts to the panel.
unsafe extern "system" fn edit_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        let previous: WNDPROC = std::mem::transmute(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
        let command = |id: usize| {
            SendMessageW(GetParent(hwnd), WM_COMMAND, id, 0);
            0
        };
        let control = GetKeyState(VK_CONTROL as i32) < 0;
        let multiline =
            GetWindowLongPtrW(hwnd, windows_sys::Win32::UI::WindowsAndMessaging::GWL_STYLE) as u32
                & ES_MULTILINE as u32
                != 0;
        match message {
            WM_KEYDOWN => match wparam as u16 {
                VK_TAB => {
                    let shift = GetKeyState(VK_SHIFT as i32) < 0;
                    return command(if shift { COMMAND_PREVIOUS } else { COMMAND_NEXT });
                },
                VK_ESCAPE => return command(COMMAND_CLOSE),
                0x53 if control => return command(COMMAND_SAVE),
                0x41 if control => {
                    SendMessageW(hwnd, EM_SETSEL, 0, -1);
                    return 0;
                },
                VK_RETURN if !multiline => return command(COMMAND_SAVE),
                _ => (),
            },
            // Swallow the characters of handled keys so the control does not beep.
            WM_CHAR
                if matches!(wparam, 0x01 | 0x09 | 0x13 | 0x1b)
                    || (wparam == 0x0d && !multiline) =>
            {
                return 0;
            },
            _ => (),
        }
        CallWindowProcW(previous, hwnd, message, wparam, lparam)
    }
}

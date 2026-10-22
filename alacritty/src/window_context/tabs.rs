//! Tabs: several terminals sharing one window, with a tab bar drawn by Alacritty's renderer.
//!
//! The active tab's terminal lives in the [`WindowContext`] like that of any window, while the
//! inactive tabs keep theirs in a [`Session`] that is swapped in when the tab is selected. Each
//! terminal reports its events under its own id, so events of inactive tabs are handled here.
//!
//! The bar takes the top of the window: the terminal is told the window is shorter by the bar's
//! height, which places it below the bar, and pointer events are moved up accordingly.

mod bar;
mod profiles;

use std::collections::HashMap;
use std::error::Error;
use std::hash::BuildHasher;
use std::mem;
#[cfg(not(windows))]
use std::os::unix::io::{AsRawFd, RawFd};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use log::error;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{
    ElementState, Event as WinitEvent, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, WindowId};

use alacritty_terminal::event::{Event as TerminalEvent, Notify, OnResize};
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::NamedColor;

use crate::cli::ParsedOptions;
use crate::config::{Action, BindingKey, BindingMode, UiConfig};
use crate::event::{Event, EventProxy, EventType, InlineSearchState, SearchState};
use crate::window_context::WindowContext;

use bar::{Hit, Layout};
use profiles::{Profile, Profiles};

/// Ids for the terminals of new tabs, far from the platform's window ids.
static NEXT_ID: AtomicU64 = AtomicU64::new(1 << 62);

/// Tabs of one window.
#[derive(Default)]
pub struct Tabs {
    /// Height of the tab bar in pixels, zero while it is hidden.
    pub height: u32,
    /// All tabs, or none while the window only ever had its first terminal.
    tabs: Vec<Tab>,
    active: usize,
    /// First tab shown when they do not all fit.
    first: usize,
    scale: f64,
    modifiers: ModifiersState,
    pointer: PhysicalPosition<f64>,
    hover: Hit,
    press: Press,
    drag: Option<Drag>,
    menu: Option<Menu>,
    /// Close the window instead of a tab once the active terminal exits.
    closing: bool,
}

struct Tab {
    /// Id the tab's terminal sends its events with.
    id: WindowId,
    /// Title the terminal resets to.
    name: String,
    /// Title of an inactive tab; the active tab shows the window's title.
    title: String,
    badge: String,
    /// Terminal of an inactive tab; the active tab's lives in the window context.
    session: Option<Session>,
}

/// Where the mouse button that is down was pressed.
#[derive(Default, PartialEq, Eq)]
enum Press {
    #[default]
    None,
    Bar,
    Terminal,
}

/// A tab being dragged to a new position.
struct Drag {
    /// Pointer offset from the tab's left edge.
    grab: f64,
    start: f64,
    moving: bool,
}

/// The open new-tab menu: profiles, then an entry to edit them.
struct Menu {
    profiles: Vec<Profile>,
    selected: usize,
}

/// Per-terminal state of a [`WindowContext`], kept aside while its tab is inactive.
struct Session {
    terminal: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    search_state: SearchState,
    inline_search_state: InlineSearchState,
    cursor_blink_timed_out: bool,
    prev_bell_cmd: Option<Instant>,
    preserve_title: bool,
    window_config: ParsedOptions,
    #[cfg(not(windows))]
    master_fd: RawFd,
    #[cfg(not(windows))]
    shell_pid: u32,
}

/// Something to do with the tabs, from the bar or the keyboard.
#[derive(Clone, Copy)]
enum Command {
    New,
    Next,
    Previous,
    Select(usize),
    Last,
    Close,
    Fullscreen,
    #[cfg(windows)]
    Wsl,
}

impl Tabs {
    fn len(&self) -> usize {
        self.tabs.len().max(1)
    }

    /// Index of the tab whose terminal reports events as `id`, in the window `window`.
    fn position(&self, window: WindowId, id: WindowId) -> Option<usize> {
        if self.tabs.is_empty() {
            (id == window).then_some(0)
        } else {
            self.tabs.iter().position(|tab| tab.id == id)
        }
    }
}

impl Session {
    /// Start a terminal for a new tab, with the settings of `profile`.
    fn spawn(
        ctx: &WindowContext,
        config: &Rc<UiConfig>,
        proxy: &EventLoopProxy<Event>,
        profile: Option<&Profile>,
    ) -> Result<(WindowId, Self), Box<dyn Error>> {
        let options = profile.map_or(&[][..], |profile| &profile.options);
        let mut window_config = ParsedOptions::from_options(options);
        let config = window_config.override_config_rc(config.clone());
        let mut pty_config = config.pty_config();
        if let Some(profile) = profile {
            profile.apply(&mut pty_config);
        }

        let id = WindowId::from(NEXT_ID.fetch_add(1, Ordering::Relaxed));
        let event_proxy = EventProxy::new(proxy.clone(), id);
        let size = ctx.display.size_info;
        let terminal = Term::new(config.term_options(), &size, event_proxy.clone());
        let terminal = Arc::new(FairMutex::new(terminal));

        // Shells and `alacritty msg` refer to the window, not the tab.
        let pty = tty::new(&pty_config, size.into(), ctx.id().into())?;
        #[cfg(not(windows))]
        let (master_fd, shell_pid) = (pty.file().as_raw_fd(), pty.child().id());
        let event_loop =
            PtyEventLoop::new(terminal.clone(), event_proxy, pty, pty_config.drain_on_exit, false)?;
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();

        Ok((id, Self {
            terminal,
            notifier,
            window_config,
            search_state: Default::default(),
            inline_search_state: Default::default(),
            cursor_blink_timed_out: false,
            prev_bell_cmd: None,
            preserve_title: false,
            #[cfg(not(windows))]
            master_fd,
            #[cfg(not(windows))]
            shell_pid,
        }))
    }

    /// Exchange this terminal with the window's active one.
    fn swap(&mut self, ctx: &mut WindowContext) {
        mem::swap(&mut self.terminal, &mut ctx.terminal);
        mem::swap(&mut self.notifier, &mut ctx.notifier);
        mem::swap(&mut self.search_state, &mut ctx.search_state);
        mem::swap(&mut self.inline_search_state, &mut ctx.inline_search_state);
        mem::swap(&mut self.cursor_blink_timed_out, &mut ctx.cursor_blink_timed_out);
        mem::swap(&mut self.prev_bell_cmd, &mut ctx.prev_bell_cmd);
        mem::swap(&mut self.preserve_title, &mut ctx.preserve_title);
        mem::swap(&mut self.window_config, &mut ctx.window_config);
        #[cfg(not(windows))]
        {
            mem::swap(&mut self.master_fd, &mut ctx.master_fd);
            mem::swap(&mut self.shell_pid, &mut ctx.shell_pid);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

/// Handle a window event for the tab bar, returning the event the terminal should get.
pub fn window_event(
    ctx: &mut WindowContext,
    event: WindowEvent,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) -> Option<WindowEvent> {
    sync(ctx);
    let height = f64::from(ctx.display.tabs.height);
    if height == 0. {
        return Some(event);
    }

    let tabs = &mut ctx.display.tabs;
    match event {
        WindowEvent::Resized(size) => {
            let height = size.height.saturating_sub(tabs.height);
            return Some(WindowEvent::Resized(PhysicalSize::new(size.width, height)));
        },
        WindowEvent::ModifiersChanged(modifiers) => tabs.modifiers = modifiers.state(),
        WindowEvent::CloseRequested => tabs.closing = true,
        WindowEvent::KeyboardInput { event: ref key, is_synthetic: false, .. }
            if key.state == ElementState::Pressed =>
        {
            if tabs.menu.is_some() {
                menu_key(ctx, key, config, proxy);
                return None;
            }
            if let Some(command) = shortcut(ctx, key) {
                run(ctx, command, config, proxy);
                return None;
            }
        },
        WindowEvent::KeyboardInput { .. } if tabs.menu.is_some() => return None,
        WindowEvent::CursorMoved { device_id, position } => {
            tabs.pointer = position;
            if tabs.drag.is_some() {
                drag(ctx);
                return None;
            }
            if tabs.press == Press::Terminal || (position.y >= height && tabs.menu.is_none()) {
                hover(ctx, Hit::None);
                let position = PhysicalPosition::new(position.x, position.y - height);
                return Some(WindowEvent::CursorMoved { device_id, position });
            }
            let hit = Layout::new(&ctx.display.tabs, &ctx.display.size_info).hit(position);
            hover(ctx, hit);
            ctx.display.window.set_mouse_cursor(CursorIcon::Default);
            return Some(WindowEvent::CursorLeft { device_id });
        },
        WindowEvent::CursorLeft { .. } => hover(ctx, Hit::None),
        WindowEvent::MouseInput { state: ElementState::Released, .. } => {
            if mem::take(&mut tabs.press) == Press::Bar {
                tabs.drag = None;
                let hit = Layout::new(tabs, &ctx.display.size_info).hit(tabs.pointer);
                hover(ctx, hit);
                return None;
            }
        },
        WindowEvent::MouseInput { button, .. } => {
            if tabs.menu.is_none() && tabs.pointer.y >= height {
                tabs.press = Press::Terminal;
            } else {
                tabs.press = Press::Bar;
                click(ctx, button, config, proxy);
                return None;
            }
        },
        WindowEvent::MouseWheel { delta, .. } if tabs.pointer.y < height => {
            let delta = match delta {
                MouseScrollDelta::LineDelta(_, delta) => f64::from(delta),
                MouseScrollDelta::PixelDelta(delta) => delta.y,
            };
            if delta != 0. {
                run(ctx, if delta > 0. { Command::Previous } else { Command::Next }, config, proxy);
            }
            return None;
        },
        WindowEvent::Touch(mut touch) => {
            touch.location.y -= height;
            return Some(WindowEvent::Touch(touch));
        },
        _ => (),
    }
    Some(event)
}

/// Route a terminal event to its tab, returning the event the window should handle.
pub fn route<S: BuildHasher>(
    windows: &mut HashMap<WindowId, WindowContext, S>,
    config: &Rc<UiConfig>,
    payload: EventType,
    window_id: Option<WindowId>,
) -> Option<Event> {
    let (event, id) = match (payload, window_id) {
        (EventType::Terminal(event), Some(id)) => (event, id),
        (payload, window_id) => return Some(Event::new(payload, window_id)),
    };

    // Most events come from a window's own terminal, so look there first.
    let window = match windows.get(&id) {
        Some(ctx) if ctx.display.tabs.position(id, id).is_some() => id,
        _ => {
            *windows
                .iter()
                .find(|(window, ctx)| ctx.display.tabs.position(**window, id).is_some())?
                .0
        },
    };
    let ctx = windows.get_mut(&window)?;
    let index = ctx.display.tabs.position(window, id)?;
    let tabs = &ctx.display.tabs;

    if index == tabs.active {
        let event = match event {
            // The last tab, or a window being closed, closes like a window without tabs.
            TerminalEvent::Exit if tabs.tabs.len() > 1 && !tabs.closing => {
                if !ctx.display.window.hold {
                    close(ctx, index, config);
                }
                return None;
            },
            TerminalEvent::ResetTitle if !tabs.tabs.is_empty() => {
                TerminalEvent::Title(tabs.tabs[index].name.clone())
            },
            event => event,
        };
        return Some(Event::new(event.into(), window));
    }

    // Inactive tabs only answer the terminal and keep their title.
    let tab = &mut ctx.display.tabs.tabs[index];
    let session = tab.session.as_ref()?;
    let write = |text: String| session.notifier.notify(text.into_bytes());
    let window_config = &ctx.config.window;
    match event {
        TerminalEvent::Title(title) if !session.preserve_title && window_config.dynamic_title => {
            tab.title = title
        },
        TerminalEvent::ResetTitle if !session.preserve_title && window_config.dynamic_title => {
            tab.title = tab.name.clone()
        },
        TerminalEvent::PtyWrite(text) => write(text),
        TerminalEvent::TextAreaSizeRequest(format) => write(format(ctx.display.size_info.into())),
        TerminalEvent::ColorRequest(index, format) => {
            let color = match session.terminal.lock().colors()[index] {
                Some(color) => color,
                None if index == NamedColor::Cursor as usize => return None,
                None => ctx.display.colors[index].0,
            };
            write(format(color));
        },
        TerminalEvent::Exit if !ctx.display.window.hold => close(ctx, index, config),
        _ => return None,
    }
    redraw(ctx);
    None
}

/// Show or hide the bar and fit the terminal below it.
fn sync(ctx: &mut WindowContext) {
    let tabs = &mut ctx.display.tabs;
    tabs.scale = ctx.display.window.scale_factor;
    let shown = ctx.config.window.tabs || tabs.tabs.len() > 1;
    let height =
        if shown { bar::height(tabs.scale, ctx.display.size_info.cell_height()) } else { 0 };
    if height == tabs.height {
        return;
    }

    tabs.height = height;
    let size = ctx.display.window.inner_size();
    let size = PhysicalSize::new(size.width, size.height.saturating_sub(height));
    let window_id = ctx.id();
    ctx.event_queue.push(WinitEvent::WindowEvent { window_id, event: WindowEvent::Resized(size) });
    redraw(ctx);
}

fn redraw(ctx: &mut WindowContext) {
    // The menu covers the terminal, which only repaints what changed in it.
    if ctx.display.tabs.menu.is_some() {
        ctx.display.damage_tracker.frame().mark_fully_damaged();
    }
    ctx.dirty = true;
    if ctx.display.window.has_frame {
        ctx.display.window.request_redraw();
    }
}

fn hover(ctx: &mut WindowContext, hit: Hit) {
    if ctx.display.tabs.hover != hit {
        ctx.display.tabs.hover = hit;
        redraw(ctx);
    }
}

/// The tab command bound to a key: by the configuration, or else by default.
fn shortcut(ctx: &WindowContext, key: &KeyEvent) -> Option<Command> {
    let modifiers = ctx.display.tabs.modifiers;
    let logical_key = match key.logical_key.as_ref() {
        Key::Character(ch) => Key::Character(ch.to_lowercase().into()),
        _ => key.logical_key.clone(),
    };
    let trigger = BindingKey::Keycode { key: logical_key.clone(), location: key.location.into() };
    let search = ctx.search_state.history_index.is_some();
    let mode = BindingMode::new(ctx.terminal.lock().mode(), search);

    let mut bound = false;
    for binding in ctx.config.key_bindings() {
        if binding.is_triggered_by(mode, modifiers, &trigger) {
            bound = true;
            let command = match binding.action {
                Action::CreateNewTab => Command::New,
                Action::SelectNextTab => Command::Next,
                Action::SelectPreviousTab => Command::Previous,
                Action::SelectTab1 => Command::Select(0),
                Action::SelectTab2 => Command::Select(1),
                Action::SelectTab3 => Command::Select(2),
                Action::SelectTab4 => Command::Select(3),
                Action::SelectTab5 => Command::Select(4),
                Action::SelectTab6 => Command::Select(5),
                Action::SelectTab7 => Command::Select(6),
                Action::SelectTab8 => Command::Select(7),
                Action::SelectTab9 => Command::Select(8),
                Action::SelectLastTab => Command::Last,
                _ => continue,
            };
            return Some(command);
        }
    }

    // Keys bound by the configuration keep their meaning.
    if bound || cfg!(target_os = "macos") {
        return None;
    }

    let control = ModifiersState::CONTROL;
    let shifted = ModifiersState::CONTROL | ModifiersState::SHIFT;
    Some(match (modifiers, logical_key.as_ref()) {
        (m, Key::Character("t")) if m == shifted => Command::New,
        (m, Key::Character("w")) if m == shifted => Command::Close,
        #[cfg(windows)]
        (m, Key::Character("u")) if m == shifted => Command::Wsl,
        (m, Key::Named(NamedKey::Tab)) if m == control => Command::Next,
        (m, Key::Named(NamedKey::Tab)) if m == shifted => Command::Previous,
        (m, Key::Character("9")) if m == control => Command::Last,
        (m, Key::Character(digit)) if m == control && ("1"..="8").contains(&digit) => {
            Command::Select(digit.parse::<usize>().ok()? - 1)
        },
        (m, Key::Named(NamedKey::F11)) if m.is_empty() => Command::Fullscreen,
        _ => return None,
    })
}

fn run(
    ctx: &mut WindowContext,
    command: Command,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) {
    let (active, len) = (ctx.display.tabs.active, ctx.display.tabs.len());
    match command {
        Command::New => {
            let profiles = Profiles::load(config);
            open(ctx, config, proxy, profiles.default_profile());
        },
        Command::Next => select(ctx, (active + 1) % len, config),
        Command::Previous => select(ctx, (active + len - 1) % len, config),
        Command::Select(index) => select(ctx, index, config),
        Command::Last => select(ctx, len - 1, config),
        Command::Close => close(ctx, active, config),
        Command::Fullscreen => ctx.display.window.toggle_fullscreen(),
        #[cfg(windows)]
        Command::Wsl => open(ctx, config, proxy, Some(&Profile::new("WSL", "wsl.exe --cd ~"))),
    }
}

/// Handle a mouse button press on the bar or the open menu.
fn click(
    ctx: &mut WindowContext,
    button: MouseButton,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) {
    let layout = Layout::new(&ctx.display.tabs, &ctx.display.size_info);
    let hit = layout.hit(ctx.display.tabs.pointer);
    if ctx.display.tabs.menu.is_some() {
        match hit {
            Hit::Item(index) if button == MouseButton::Left => choose(ctx, index, config, proxy),
            Hit::Item(_) => (),
            _ => close_menu(ctx),
        }
        return;
    }

    match (hit, button) {
        (Hit::Tab(index), MouseButton::Left) => {
            select(ctx, index, config);
            let tabs = &mut ctx.display.tabs;
            let left = layout.tab(index).map_or(0., |rect| f64::from(rect.x));
            let start = tabs.pointer.x;
            tabs.drag = Some(Drag { grab: start - left, start, moving: false });
        },
        (Hit::Close(index), MouseButton::Left)
        | (Hit::Tab(index) | Hit::Close(index), MouseButton::Middle) => close(ctx, index, config),
        (Hit::New, MouseButton::Left) => run(ctx, Command::New, config, proxy),
        (Hit::Menu, MouseButton::Left) => {
            let profiles = Profiles::load(config).list.into_iter().filter(|p| !p.hidden).collect();
            ctx.display.tabs.menu = Some(Menu { profiles, selected: 0 });
            redraw(ctx);
        },
        (Hit::Settings, MouseButton::Left) => edit_profiles(ctx, config, proxy),
        (Hit::Previous, MouseButton::Left) => run(ctx, Command::Previous, config, proxy),
        (Hit::Next, MouseButton::Left) => run(ctx, Command::Next, config, proxy),
        _ => (),
    }
}

/// Move the dragged tab to the slot under the pointer.
fn drag(ctx: &mut WindowContext) {
    let layout = Layout::new(&ctx.display.tabs, &ctx.display.size_info);
    let tabs = &mut ctx.display.tabs;
    let Some(drag) = &mut tabs.drag else { return };
    let x = tabs.pointer.x;
    drag.moving |= (x - drag.start).abs() >= 4. * tabs.scale;
    if !drag.moving {
        return;
    }
    tabs.hover = Hit::None;

    let target = layout.drop_target((x - drag.grab) as f32);
    if let Some(target) = target.filter(|&target| target != tabs.active && !tabs.tabs.is_empty()) {
        let tab = tabs.tabs.remove(tabs.active);
        tabs.tabs.insert(target, tab);
        tabs.active = target;
    }
    redraw(ctx);
}

fn menu_key(
    ctx: &mut WindowContext,
    key: &KeyEvent,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) {
    let Some(menu) = &mut ctx.display.tabs.menu else { return };
    let rows = menu.profiles.len() + 1;
    match key.logical_key {
        Key::Named(NamedKey::ArrowDown) => menu.selected = (menu.selected + 1) % rows,
        Key::Named(NamedKey::ArrowUp) => menu.selected = (menu.selected + rows - 1) % rows,
        Key::Named(NamedKey::Enter) => {
            let selected = menu.selected;
            return choose(ctx, selected, config, proxy);
        },
        Key::Named(NamedKey::Escape) => return close_menu(ctx),
        _ => return,
    }
    redraw(ctx);
}

/// Open the profile in row `index` of the menu, or the profiles file after them.
fn choose(
    ctx: &mut WindowContext,
    index: usize,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) {
    let Some(menu) = ctx.display.tabs.menu.take() else { return };
    close_menu(ctx);
    match menu.profiles.get(index) {
        Some(profile) => open(ctx, config, proxy, Some(profile)),
        None => edit_profiles(ctx, config, proxy),
    }
}

fn close_menu(ctx: &mut WindowContext) {
    ctx.display.tabs.menu = None;
    // Repaint the terminal the menu covered.
    ctx.display.damage_tracker.frame().mark_fully_damaged();
    redraw(ctx);
}

/// Open the profiles file in the user's editor, in a new tab, or else the system's.
fn edit_profiles(ctx: &mut WindowContext, config: &Rc<UiConfig>, proxy: &EventLoopProxy<Event>) {
    let path = match Profiles::create(config) {
        Ok(path) => path,
        Err(err) => return error!("Could not create the profiles file: {err}"),
    };

    let editor = ["VISUAL", "EDITOR"].into_iter().find_map(|name| std::env::var(name).ok());
    if let Some(editor) = editor.filter(|editor| !editor.trim().is_empty()) {
        let command = format!("{editor} \"{}\"", path.display());
        return open(ctx, config, proxy, Some(&Profile::new("profiles.toml", &command)));
    }

    #[cfg(windows)]
    let (program, args) = ("notepad.exe", [path.as_os_str()]);
    #[cfg(target_os = "macos")]
    let (program, args) = ("open", [std::ffi::OsStr::new("-t"), path.as_os_str()]);
    #[cfg(not(any(windows, target_os = "macos")))]
    let (program, args) = ("xdg-open", [path.as_os_str()]);
    #[cfg(windows)]
    let result = crate::daemon::spawn_daemon(program, args);
    #[cfg(not(windows))]
    let result = crate::daemon::spawn_daemon(program, args, ctx.master_fd, ctx.shell_pid);
    if let Err(err) = result {
        error!("Could not open {}: {err}", path.display());
    }
}

/// Open a new tab after the others and select it.
fn open(
    ctx: &mut WindowContext,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
    profile: Option<&Profile>,
) {
    let (id, session) = match Session::spawn(ctx, config, proxy, profile) {
        Ok(tab) => tab,
        Err(err) => return error!("Could not open tab: {err}"),
    };

    let (window_id, title) = (ctx.id(), &config.window.identity.title);
    let tabs = &mut ctx.display.tabs;
    if tabs.tabs.is_empty() {
        let (name, title, badge) = (title.clone(), String::new(), ">".into());
        tabs.tabs.push(Tab { id: window_id, name, title, badge, session: None });
    }
    let name = profile.map_or_else(|| title.clone(), |profile| profile.name.clone());
    let badge = profile.map_or_else(|| ">".into(), Profile::badge);
    tabs.tabs.push(Tab { id, title: name.clone(), name, badge, session: Some(session) });
    select(ctx, ctx.display.tabs.tabs.len() - 1, config);
}

/// Make the tab at `index` the active one.
fn select(ctx: &mut WindowContext, index: usize, config: &Rc<UiConfig>) {
    let tabs = &mut ctx.display.tabs;
    let old = tabs.active;
    let Some(mut session) = tabs.tabs.get_mut(index).and_then(|tab| tab.session.take()) else {
        return;
    };
    tabs.tabs[old].title = ctx.display.window.title().to_owned();
    let focused = ctx.terminal.lock().is_focused;
    session.swap(ctx);
    let tabs = &mut ctx.display.tabs;
    tabs.tabs[old].session = Some(session);
    tabs.active = index;
    let title = tabs.tabs[index].title.clone();

    // Apply the tab's configuration overrides, then its title and the current size.
    ctx.update_config(config.clone());
    ctx.display.window.set_title(title);
    let size = ctx.display.size_info;
    let mut terminal = ctx.terminal.lock();
    terminal.is_focused = focused;
    if terminal.columns() != size.columns() || terminal.screen_lines() != size.screen_lines() {
        terminal.resize(size);
        ctx.notifier.on_resize(size.into());
    }
    drop(terminal);

    ctx.display.highlighted_hint = None;
    ctx.display.vi_highlighted_hint = None;
    ctx.display.pending_update.dirty = true;
    ctx.display.damage_tracker.frame().mark_fully_damaged();
    redraw(ctx);
}

/// Close the tab at `index`, or the window with its last tab.
fn close(ctx: &mut WindowContext, index: usize, config: &Rc<UiConfig>) {
    let len = ctx.display.tabs.tabs.len();
    if len <= 1 {
        ctx.display.tabs.closing = true;
        ctx.display.window.hold = false;
        ctx.terminal.lock().exit();
        return;
    }

    if index == ctx.display.tabs.active {
        select(ctx, if index + 1 < len { index + 1 } else { index - 1 }, config);
    }
    let tabs = &mut ctx.display.tabs;
    tabs.tabs.remove(index);
    if tabs.active > index {
        tabs.active -= 1;
    }
    sync(ctx);
    redraw(ctx);
}

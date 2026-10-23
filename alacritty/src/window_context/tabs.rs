//! Tabs: several terminals sharing one window, under a caption drawn by Alacritty's renderer.
//!
//! The active tab's terminal lives in the [`WindowContext`] like that of any window, while the
//! inactive tabs keep theirs in a [`Session`] that is swapped in when the tab is selected. Each
//! terminal reports its events under its own id, so events of inactive tabs are handled here.
//!
//! The caption replaces the system's: the terminal is told the window is shorter by the bar's
//! height, which places it below the bar, and its padding keeps it inside a rounded panel. Pointer
//! events are moved up accordingly.

mod bar;
mod fonts;
mod icons;
mod paint;
mod profiles;
mod settings;
mod text;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(windows))]
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(any(windows, target_os = "macos")))]
use unix as platform;
#[cfg(windows)]
use windows as platform;

use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::hash::BuildHasher;
use std::mem;
#[cfg(not(windows))]
use std::os::unix::io::{AsRawFd, RawFd};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use log::error;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{
    ElementState, Event as WinitEvent, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, WindowAttributes, WindowId};

use alacritty_terminal::event::{Event as TerminalEvent, Notify, OnResize};
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{ClipboardType, Term};
use alacritty_terminal::tty::{self, Options as PtyOptions};
use alacritty_terminal::vte::ansi::NamedColor;

use crate::cli::{ParsedOptions, WindowOptions};
use crate::clipboard::Clipboard;
use crate::config::window::UiFont as UiFontConfig;
use crate::config::{Action, BindingKey, BindingMode, UiConfig};
use crate::event::{Event, EventProxy, EventType, InlineSearchState, SearchState};
use crate::window_context::WindowContext;

use bar::{BAR, Hit, INSET, Layout, MARGIN};
use paint::Image;
use profiles::{Profile, Profiles};
use settings::Settings;
use text::UiFont;

/// Ids for the terminals of new tabs, far from the platform's window ids.
static NEXT_ID: AtomicU64 = AtomicU64::new(1 << 62);

/// How the window is shown, which decides its resize border and the terminal's panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Frame {
    #[default]
    Normal,
    Maximized,
    /// The terminal fills the window below the bar, without a panel.
    Fullscreen,
}

/// Tabs of one window.
#[derive(Default)]
pub struct Tabs {
    /// Pixels above the terminal's area; zero without tabs.
    pub height: u32,
    /// All tabs, or none while tabs are off.
    tabs: Vec<Tab>,
    active: usize,
    /// First tab shown when they do not all fit.
    first: usize,
    scale: f64,
    frame: Frame,
    /// Computer name shown beside the logo.
    host: String,
    /// The configuration before the terminal was fit in the panel, and after.
    configs: Option<(Rc<UiConfig>, Rc<UiConfig>)>,
    /// Padding added to fit the terminal in the panel, in logical pixels.
    inset: u8,
    /// Profile the window's first terminal was started with.
    first_profile: RefCell<Option<Profile>>,
    modifiers: ModifiersState,
    pointer: PhysicalPosition<f64>,
    hover: Hit,
    press: Press,
    last_click: Option<Instant>,
    /// Where the bar was pressed to move the window, once the pointer moves.
    grab: Option<PhysicalPosition<f64>>,
    drag: Option<Drag>,
    menu: Option<Menu>,
    settings: Option<Settings>,
    /// Close the window instead of a tab once the active terminal exits.
    closing: bool,
    /// Font of the bar, menus and settings, and what it was last loaded for.
    font: Option<Rc<UiFont>>,
    font_source: Option<(UiFontConfig, f64)>,
}

struct Tab {
    /// Id the tab's terminal sends its events with.
    id: WindowId,
    /// Title the terminal resets to.
    name: String,
    /// Title of an inactive tab; the active tab shows the window's title.
    title: String,
    badge: String,
    icon: Option<Arc<Image>>,
    /// Name of the profile the tab was opened with.
    profile: Option<String>,
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
    /// Forget the interface's glyphs, after the glyph atlas was cleared.
    pub fn reset_glyphs(&self) {
        if let Some(font) = &self.font {
            font.reset();
        }
    }

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
        let options = profile.map_or_else(Vec::new, Profile::overrides);
        let mut window_config = ParsedOptions::from_options(&options);
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

/// Let the bar replace the system's caption.
pub fn attributes(attributes: WindowAttributes, config: &UiConfig) -> WindowAttributes {
    if config.window.tabs { platform::attributes(attributes) } else { attributes }
}

/// Start a window's first terminal with the default profile, unless a command was given.
pub fn launch(config: &UiConfig, options: &WindowOptions, pty: &mut PtyOptions, tabs: &Tabs) {
    if !config.window.tabs || options.terminal_options.command().is_some() {
        return;
    }
    let Some(profile) = Profiles::load(config).default_profile().cloned() else { return };
    let directory = pty.working_directory.take();
    profile.apply(pty);
    if options.terminal_options.working_directory.is_some() {
        pty.working_directory = directory;
    }
    *tabs.first_profile.borrow_mut() = Some(profile);
}

/// Handle a window event for the caption, returning the event the terminal should get.
pub fn window_event(
    ctx: &mut WindowContext,
    event: WindowEvent,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
    clipboard: &mut Clipboard,
) -> Option<WindowEvent> {
    sync(ctx, config, matches!(event, WindowEvent::Resized(_)));
    let (window_id, offset) = (ctx.id(), f64::from(ctx.display.tabs.height));
    if offset == 0. {
        return Some(event);
    }

    if ctx.display.tabs.settings.as_mut().is_some_and(Settings::poll) {
        redraw(ctx);
    }
    let layout = Layout::new(&ctx.display.tabs, &ctx.display.size_info);
    let tabs = &mut ctx.display.tabs;
    let modal = tabs.settings.is_some() || tabs.menu.is_some();
    match event {
        WindowEvent::Resized(size) => {
            let height = size.height.saturating_sub(tabs.height);
            return Some(WindowEvent::Resized(PhysicalSize::new(size.width, height)));
        },
        WindowEvent::ModifiersChanged(modifiers) => tabs.modifiers = modifiers.state(),
        WindowEvent::CloseRequested => tabs.closing = true,
        WindowEvent::Ime(ime) if tabs.settings.is_some() => {
            let settings = tabs.settings.as_mut()?;
            match ime {
                Ime::Commit(text) => settings.insert(&text),
                Ime::Preedit(text, _) => settings.preedit(text),
                _ => (),
            }
            redraw(ctx);
            return None;
        },
        WindowEvent::KeyboardInput { event: ref key, is_synthetic: false, .. }
            if key.state == ElementState::Pressed =>
        {
            if let Some(settings) = &mut tabs.settings {
                let paste = || clipboard.load(ClipboardType::Clipboard);
                let close = settings.key(key, tabs.modifiers, paste, config, &layout);
                restyle(ctx, config);
                if close {
                    close_settings(ctx);
                }
                redraw(ctx);
                return None;
            }
            if tabs.menu.is_some() {
                menu_key(ctx, key, config, proxy);
                return None;
            }
            if let Some(command) = shortcut(ctx, key) {
                run(ctx, command, config, proxy);
                return None;
            }
        },
        WindowEvent::KeyboardInput { .. } if modal => return None,
        WindowEvent::CursorMoved { device_id, position } => {
            tabs.pointer = position;
            if let Some(settings) = &mut tabs.settings {
                if settings.moved(&layout, position) {
                    redraw(ctx);
                }
                ctx.display.window.set_mouse_cursor(CursorIcon::Default);
                return None;
            }
            if tabs.drag.is_some() {
                drag(ctx, &layout);
                return None;
            }
            let moved = |g: &PhysicalPosition<f64>| (g.x - position.x).hypot(g.y - position.y) > 4.;
            if tabs.grab.take_if(|grab| moved(grab)).is_some() {
                let _ = ctx.display.window.winit().drag_window();
                return None;
            }
            let hit = layout.hit(position);
            if tabs.press == Press::Terminal || hit == Hit::Terminal && tabs.menu.is_none() {
                hover(ctx, Hit::None);
                let position = PhysicalPosition::new(position.x, position.y - offset);
                return Some(WindowEvent::CursorMoved { device_id, position });
            }
            hover(ctx, hit);
            let cursor = match hit {
                Hit::Resize(direction) => direction.into(),
                _ => CursorIcon::Default,
            };
            ctx.display.window.set_mouse_cursor(cursor);
            return Some(WindowEvent::CursorLeft { device_id });
        },
        WindowEvent::CursorLeft { .. } => hover(ctx, Hit::None),
        WindowEvent::MouseInput { state: ElementState::Released, .. } => {
            if let Some(settings) = &mut tabs.settings {
                settings.release();
                redraw(ctx);
                return None;
            }
            if mem::take(&mut tabs.press) == Press::Bar {
                tabs.drag = None;
                tabs.grab = None;
                hover(ctx, layout.hit(ctx.display.tabs.pointer));
                return None;
            }
        },
        WindowEvent::MouseInput { button, .. } => {
            let pointer = tabs.pointer;
            if let Some(settings) = &mut tabs.settings {
                let wake = waker(tabs.tabs.get(tabs.active).map_or(window_id, |tab| tab.id), proxy);
                let close = settings.press(&layout, pointer, button, config, wake);
                restyle(ctx, config);
                if close {
                    close_settings(ctx);
                }
                redraw(ctx);
                return None;
            }
            if tabs.menu.is_none() && layout.hit(pointer) == Hit::Terminal {
                tabs.press = Press::Terminal;
            } else {
                tabs.press = Press::Bar;
                click(ctx, &layout, button, config, proxy);
                return None;
            }
        },
        WindowEvent::MouseWheel { delta, .. } if modal || tabs.pointer.y < layout.bar as f64 => {
            let delta = match delta {
                MouseScrollDelta::LineDelta(_, delta) => f64::from(delta),
                MouseScrollDelta::PixelDelta(delta) => delta.y,
            };
            if let Some(settings) = &mut tabs.settings {
                settings.scroll(&layout, delta < 0.);
                redraw(ctx);
            } else if delta != 0. && tabs.menu.is_none() {
                run(ctx, if delta > 0. { Command::Previous } else { Command::Next }, config, proxy);
            }
            return None;
        },
        WindowEvent::Touch(mut touch) => {
            touch.location.y -= offset;
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

/// Show or hide the caption, and fit the terminal in its panel below the bar.
///
/// The window's state is only queried on `resize`, since that is slow on some platforms.
fn sync(ctx: &mut WindowContext, config: &Rc<UiConfig>, resize: bool) {
    let id = ctx.id();
    let tabs = &mut ctx.display.tabs;
    tabs.scale = ctx.display.window.scale_factor;

    // The interface's font follows the configuration and the window's scale.
    let source = (config.window.ui_font.clone(), tabs.scale);
    if tabs.font_source.as_ref() != Some(&source) {
        match UiFont::new(&source.0, source.1) {
            Ok(font) => tabs.font = Some(Rc::new(font)),
            Err(err) => error!("Could not load the interface font: {err}"),
        }
        tabs.font_source = Some(source);
        redraw(ctx);
    }
    let tabs = &mut ctx.display.tabs;
    let shown = (ctx.config.window.tabs || tabs.tabs.len() > 1) && tabs.font.is_some();

    // The window's own terminal becomes the first tab.
    if shown && tabs.tabs.is_empty() {
        tabs.host = platform::hostname().to_uppercase();
        let profile = tabs.first_profile.borrow_mut().take();
        let name = profile
            .as_ref()
            .map_or_else(|| config.window.identity.title.clone(), |p| p.name.clone());
        let badge = profile.as_ref().map_or_else(|| ">".into(), Profile::badge);
        let icon = match &profile {
            Some(profile) => profile.image(config),
            None => icons::get(None, &profiles::shell(config)),
        };
        if profile.is_some() && !ctx.preserve_title {
            ctx.display.window.set_title(name.clone());
        }
        let tabs = &mut ctx.display.tabs;
        let (title, session, opened) =
            (String::new(), None, profile.as_ref().map(|p| p.name.clone()));
        tabs.tabs.push(Tab { id, name, title, badge, icon, profile: opened, session });
        let options = profile.as_ref().map_or_else(Vec::new, Profile::overrides);
        if !options.is_empty() {
            ctx.window_config = ParsedOptions::from_options(&options);
            ctx.update_config(config.clone());
        }
    }

    let window = ctx.display.window.winit();
    let frame = match (resize || ctx.display.tabs.configs.is_none(), window) {
        (false, _) => ctx.display.tabs.frame,
        (_, window) if window.fullscreen().is_some() => Frame::Fullscreen,
        (_, window) if window.is_maximized() => Frame::Maximized,
        _ => Frame::Normal,
    };
    let px = |value: f32| (value * ctx.display.window.scale_factor as f32).round() as u32;
    let (offset, inset) = match (shown, frame) {
        (false, _) => (0, 0),
        (_, Frame::Fullscreen) => (px(BAR), 0),
        _ => (px(BAR) - px(MARGIN), (MARGIN + INSET) as u8),
    };

    // Pad the terminal into its panel, again whenever its configuration was replaced.
    let tabs = &mut ctx.display.tabs;
    tabs.frame = frame;
    let unpadded = match &tabs.configs {
        Some((unpadded, padded)) if Rc::ptr_eq(padded, &ctx.config) => unpadded.clone(),
        _ => ctx.config.clone(),
    };
    let fresh = Rc::ptr_eq(&unpadded, &ctx.config);
    let mut changed = false;
    if fresh || inset != tabs.inset {
        let (x, y) = unpadded.window.padding(1.);
        let padding = format!("window.padding={{x={},y={}}}", x as u8 + inset, y as u8 + inset);
        let padded = ParsedOptions::from_options(&[padding]).override_config_rc(unpadded.clone());
        changed = padded.window.padding(1.) != ctx.config.window.padding(1.);
        tabs.inset = inset;
        tabs.configs = Some((unpadded, padded.clone()));
        ctx.config = padded;
    }

    if offset != ctx.display.tabs.height || changed {
        ctx.display.tabs.height = offset;
        let size = ctx.display.window.inner_size();
        let size = PhysicalSize::new(size.width, size.height.saturating_sub(offset));
        let window_id = ctx.id();
        ctx.event_queue
            .push(WinitEvent::WindowEvent { window_id, event: WindowEvent::Resized(size) });
        ctx.display.pending_update.dirty = true;
        redraw(ctx);
    }
}

fn redraw(ctx: &mut WindowContext) {
    // Menus and settings cover the terminal, which only repaints what changed in it.
    let tabs = &ctx.display.tabs;
    if tabs.menu.is_some() || tabs.settings.is_some() {
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

/// Handle a mouse button press on the caption or the open menu.
fn click(
    ctx: &mut WindowContext,
    layout: &Layout,
    button: MouseButton,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) {
    let pointer = ctx.display.tabs.pointer;
    let hit = layout.hit(pointer);
    if ctx.display.tabs.menu.is_some() {
        match hit {
            Hit::Item(index) if button == MouseButton::Left => choose(ctx, index, config, proxy),
            Hit::Item(_) => (),
            _ => close_menu(ctx),
        }
        return;
    }

    let window = ctx.display.window.winit();
    match (hit, button) {
        (Hit::Tab(index), MouseButton::Left) => {
            select(ctx, index, config);
            let left = layout.tab(index).map_or(0., |rect| f64::from(rect.x));
            let tabs = &mut ctx.display.tabs;
            tabs.drag = Some(Drag { grab: pointer.x - left, start: pointer.x, moving: false });
        },
        (Hit::Close(index), MouseButton::Left)
        | (Hit::Tab(index) | Hit::Close(index), MouseButton::Middle) => close(ctx, index, config),
        (Hit::New, MouseButton::Left) => run(ctx, Command::New, config, proxy),
        (Hit::Menu, MouseButton::Left) => {
            let profiles = Profiles::load(config).list.into_iter().filter(|p| !p.hidden).collect();
            ctx.display.tabs.menu = Some(Menu { profiles, selected: 0 });
            redraw(ctx);
        },
        (Hit::Settings, MouseButton::Left) => open_settings(ctx, config, proxy),
        (Hit::Previous, MouseButton::Left) => run(ctx, Command::Previous, config, proxy),
        (Hit::Next, MouseButton::Left) => run(ctx, Command::Next, config, proxy),
        (Hit::Minimize, MouseButton::Left) => ctx.display.window.set_minimized(true),
        // The restore button leaves fullscreen.
        (Hit::Maximize, MouseButton::Left) if ctx.display.tabs.frame == Frame::Fullscreen => {
            ctx.display.window.toggle_fullscreen()
        },
        (Hit::Maximize, MouseButton::Left) => ctx.display.window.toggle_maximized(),
        (Hit::CloseWindow, MouseButton::Left) => close(ctx, usize::MAX, config),
        (Hit::Resize(direction), MouseButton::Left) => {
            let _ = window.drag_resize_window(direction);
        },
        (Hit::Drag, MouseButton::Left) => {
            // A second click soon after the first maximizes instead.
            let now = Instant::now();
            let last = ctx.display.tabs.last_click.replace(now);
            if last.is_some_and(|last| now - last < Duration::from_millis(500)) {
                ctx.display.tabs.last_click = None;
                ctx.display.window.toggle_maximized();
            } else {
                ctx.display.tabs.grab = Some(pointer);
            }
        },
        (Hit::Drag, MouseButton::Right) => window.show_window_menu(pointer),
        _ => (),
    }
}

/// Move the dragged tab to the slot under the pointer.
fn drag(ctx: &mut WindowContext, layout: &Layout) {
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

/// Open the profile in row `index` of the menu, or the settings after them.
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
        None => open_settings(ctx, config, proxy),
    }
}

fn close_menu(ctx: &mut WindowContext) {
    ctx.display.tabs.menu = None;
    // Repaint the terminal the menu covered.
    ctx.display.damage_tracker.frame().mark_fully_damaged();
    redraw(ctx);
}

fn open_settings(ctx: &mut WindowContext, config: &UiConfig, proxy: &EventLoopProxy<Event>) {
    let tabs = &ctx.display.tabs;
    let wake = waker(tabs.tabs.get(tabs.active).map_or(ctx.id(), |tab| tab.id), proxy);
    ctx.display.tabs.settings = Some(Settings::new(config, wake));
    redraw(ctx);
}

/// Wake the window from another thread, through the terminal that reports events as `id`.
fn waker(id: WindowId, proxy: &EventLoopProxy<Event>) -> impl Fn() + Send + 'static {
    let proxy = proxy.clone();
    move || {
        let _ = proxy.send_event(Event::new(TerminalEvent::Wakeup.into(), id));
    }
}

/// Give the tabs opened with the profiles just saved their new look and configuration.
fn restyle(ctx: &mut WindowContext, config: &Rc<UiConfig>) {
    let saved = ctx.display.tabs.settings.as_mut().map(Settings::take_saved).unwrap_or_default();
    let mut active = false;
    for tab in &mut ctx.display.tabs.tabs {
        let Some((_, profile)) = saved.iter().find(|(name, _)| tab.profile.as_ref() == Some(name))
        else {
            continue;
        };
        tab.profile = Some(profile.name.clone());
        tab.badge = profile.badge();
        tab.icon = profile.image(config);
        if tab.title == tab.name {
            tab.title = profile.name.clone();
        }
        tab.name = profile.name.clone();
        let window_config = ParsedOptions::from_options(&profile.overrides());
        match &mut tab.session {
            Some(session) => session.window_config = window_config,
            None => {
                ctx.window_config = window_config;
                active = true;
            },
        }
    }
    if active {
        ctx.update_config(config.clone());
        sync(ctx, config, false);
    }
    if !saved.is_empty() {
        redraw(ctx);
    }
}

fn close_settings(ctx: &mut WindowContext) {
    ctx.display.tabs.settings = None;
    ctx.display.damage_tracker.frame().mark_fully_damaged();
    redraw(ctx);
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

    let (name, badge, icon) = match profile {
        Some(profile) => (profile.name.clone(), profile.badge(), profile.image(config)),
        None => {
            let icon = icons::get(None, &profiles::shell(config));
            (config.window.identity.title.clone(), ">".into(), icon)
        },
    };
    let (title, opened) = (name.clone(), profile.map(|profile| profile.name.clone()));
    let tab = Tab { id, title, name, badge, icon, profile: opened, session: Some(session) };
    ctx.display.tabs.tabs.push(tab);
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
    sync(ctx, config, false);
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

/// Close the tab at `index`, or the window with its last tab or an index past the tabs.
fn close(ctx: &mut WindowContext, index: usize, config: &Rc<UiConfig>) {
    let len = ctx.display.tabs.tabs.len();
    if len <= 1 || index >= len {
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
    sync(ctx, config, false);
    redraw(ctx);
}

//! Calls from the window context, which upstream owns, into the tabs, the panes and the sessions.
//!
//! Each function stands in for, or wraps, a bit of [`WindowContext`] marked `tabbed:` there, so
//! that the window context keeps upstream's code.

use std::error::Error;
use std::mem;
use std::rc::Rc;
use std::time::Instant;

use glutin::config::Config as GlutinConfig;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::WindowId;

use alacritty_terminal::event::OnResize;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Term;
use alacritty_terminal::tty::Options as PtyOptions;
#[cfg(windows)]
use alacritty_terminal::tty::Pty;

use crate::cli::{ParsedOptions, WindowOptions};
use crate::config::UiConfig;
use crate::config::window::StartupMode;
use crate::display::window::Window;
use crate::display::{Display, SizeInfo};
use crate::event::{Event, EventProxy};
use crate::renderer::Frame;
use crate::scheduler::Scheduler;
use crate::window_context::WindowContext;

#[cfg(windows)]
use super::cwd;
use super::{Live, Rect, Torn, adopt, launch, restore, set_live_font};

/// Start the shell of a new window with `pty`: that of the live pane of the window `saved`, or
/// of the default profile.
pub fn pty_config(
    display: &Display,
    config: &UiConfig,
    options: &WindowOptions,
    saved: Option<&restore::Window>,
    pty: &mut PtyOptions,
) {
    match saved {
        Some(saved) => restore::prepare(config, &display.tabs, saved, pty),
        None => launch(config, options, pty, &display.tabs),
    }
    #[cfg(windows)]
    cwd::mark(pty);
}

/// The new terminal `terminal`, with the text of the live pane of the window `saved` written in.
pub fn replay(mut terminal: Term<EventProxy>, saved: Option<&restore::Window>) -> Term<EventProxy> {
    if let Some(text) = saved.and_then(restore::first_text) {
        restore::replay(&mut terminal, text);
    }
    terminal
}

/// The process id of the shell `pty` runs, tied to the marker its `options` carry.
#[cfg(windows)]
pub fn shell_pid(pty: &Pty, options: &PtyOptions) -> u32 {
    let shell_pid = pty.child_watcher().pid().map_or(0, |pid| pid.get());
    if let Some(marker) = cwd::marked(options) {
        cwd::register(shell_pid, marker);
    }
    shell_pid
}

/// Give the new window `ctx` the place, the tabs and the panes of the window `saved`, if any.
pub fn restore_window(
    mut ctx: WindowContext,
    saved: Option<restore::Window>,
    proxy: &EventLoopProxy<Event>,
) -> WindowContext {
    if let Some(saved) = saved {
        let config = ctx.config.clone();
        restore::install(&mut ctx, saved, &config, proxy);
    }
    ctx
}

/// Set the config `overrides` of the new window `ctx`, before those of a restored pane's profile.
pub fn config_overrides(ctx: &mut WindowContext, overrides: ParsedOptions) {
    let profile = mem::replace(&mut ctx.window_config, overrides);
    ctx.window_config.extend_from_slice(&profile);
}

/// Draw the window `ctx` with the other panes of the active tab beside the live terminal.
///
/// A lone terminal at rest is left to the window to draw as without tabs, then to [`animate`] at
/// the time returned.
pub fn draw(ctx: &mut WindowContext, scheduler: &mut Scheduler) -> Option<Instant> {
    let now = Instant::now();
    let tabs = &ctx.display.tabs;
    let panes = tabs.background_panes(now);
    let rect = tabs.focused_frame(now);
    let size = ctx.display.size_info;
    // A lone terminal at rest fills the window below the bar, as without tabs.
    if panes.is_empty() && rect == Rect::new(0., tabs.height as f32, size.width(), size.height()) {
        return Some(now);
    }

    let height = ctx.display.window.inner_size().height as f32;
    // Each terminal stays inside its card, wherever its neighbours' places overlap it.
    let framed = |size: &SizeInfo, frame: Rect, clip: Rect| {
        let frame = Frame::new(size, [frame.x, frame.y, frame.w, frame.h], height);
        frame.clipped([clip.x, clip.y, clip.w, clip.h], height)
    };
    ctx.display.begin(&ctx.config);
    for pane in panes {
        let frame = framed(&pane.size, pane.frame, pane.clip);
        // Only the live terminal has the focus, so the others show a hollow cursor.
        let mut terminal = pane.terminal.lock();
        terminal.is_focused = false;
        ctx.display.draw_pane(terminal, pane.size, &pane.config, frame, &pane.font);
    }
    let frame = framed(&size, rect, ctx.display.tabs.focused_clip(now));
    ctx.display.draw_terminal(
        ctx.terminal.lock(),
        &ctx.message_buffer,
        &ctx.config,
        &mut ctx.search_state,
        Some(frame),
    );
    ctx.display.present(scheduler, &ctx.config);

    animate(ctx, now);
    None
}

/// Keep drawing the window `ctx`, drawn at `now`, while the panes or the tabs move, which drawing
/// may have started, until a frame is drawn at rest.
pub fn animate(ctx: &mut WindowContext, now: Instant) {
    if ctx.display.tabs.animating(now) {
        if ctx.display.window.has_frame {
            ctx.display.window.request_redraw();
        } else {
            ctx.dirty = true;
        }
    }
}

/// What a new window is made from, besides its config.
pub enum Origin<'a> {
    /// A window of its own, restored from the last session if one is saved.
    Saved(Option<restore::Window>),
    /// A tab torn off another window, whose focused terminal the window adopts.
    Torn(&'a mut Torn),
}

/// Place the window of a tab torn off another where it was dropped, at the size of the window it
/// was torn off, before it is shown.
pub fn prepare_window(window: &Window, origin: &Origin<'_>) {
    if let Origin::Torn(torn) = origin {
        if let Some(position) = torn.position {
            window.winit().set_outer_position(position);
        }
        window.request_inner_size(torn.size);
    }
}

/// Create the context of the new window around `display`: with a shell of its own, or around the
/// focused terminal of a torn-off tab.
pub fn context(
    display: Display,
    config: Rc<UiConfig>,
    options: WindowOptions,
    proxy: EventLoopProxy<Event>,
    origin: Origin<'_>,
) -> Result<WindowContext, Box<dyn Error>> {
    match origin {
        Origin::Saved(saved) => WindowContext::new(display, config, options, proxy, saved),
        Origin::Torn(torn) => {
            // The source window hands over a tab whose focused pane is not live.
            let Some((id, live)) = torn.live() else {
                return Err("the torn tab has no terminal to show".into());
            };
            Ok(assemble(display, config, id, live))
        },
    }
}

/// Create a window for a tab torn off another, which adopts its running terminals.
///
/// Without a window, the tab is handed back with its terminals untouched.
pub fn torn_window(
    gl_config: &GlutinConfig,
    event_loop: &ActiveEventLoop,
    proxy: EventLoopProxy<Event>,
    config: Rc<UiConfig>,
    mut torn: Torn,
) -> Result<WindowContext, (Box<dyn Error>, Box<Torn>)> {
    let zoom = torn.zoom();
    // The window takes the size of the one it was torn off, rather than the startup mode's.
    let mut windowed = (*config).clone();
    windowed.window.startup_mode = StartupMode::Windowed;
    let window_context = WindowContext::additional(
        gl_config,
        event_loop,
        proxy,
        Rc::new(windowed),
        Default::default(),
        Default::default(),
        Origin::Torn(&mut torn),
    );

    // From here on the terminal belongs to the new window, whatever happens.
    let mut window_context = match window_context {
        Ok(window_context) => window_context,
        Err(err) => return Err((err, Box::new(torn))),
    };
    // A configured size gives way to the size of the window it was torn off.
    window_context.display.window.request_inner_size(torn.size);
    window_context.display.window.hold = torn.hold;
    // Adopting the tab also brings in the config the other windows share.
    adopt(&mut window_context, torn, &config);
    // A zoomed pane keeps its text size.
    if let Some(size) = zoom {
        set_live_font(&mut window_context, size);
    }
    window_context.display.window.winit().focus_window();
    Ok(window_context)
}

/// Create the context of a window around its running terminal `live`, whose events come with the
/// id `first`.
fn assemble(
    display: Display,
    config: Rc<UiConfig>,
    first: WindowId,
    mut live: Live,
) -> WindowContext {
    // The terminal fills the display, whatever it was sized for before.
    let size = display.size_info;
    let mut terminal = live.terminal.lock();
    if terminal.columns() != size.columns() || terminal.screen_lines() != size.screen_lines() {
        terminal.resize(size);
        live.notifier.on_resize(size.into());
    }
    drop(terminal);

    // Create context for the Alacritty window.
    WindowContext {
        first,
        preserve_title: live.preserve_title,
        terminal: live.terminal,
        display,
        #[cfg(not(windows))]
        master_fd: live.master_fd,
        shell_pid: live.shell_pid,
        config,
        notifier: live.notifier,
        cursor_blink_timed_out: Default::default(),
        prev_bell_cmd: Default::default(),
        inline_search_state: live.inline_search_state,
        message_buffer: Default::default(),
        window_config: live.window_config,
        search_state: live.search_state,
        event_queue: Default::default(),
        modifiers: Default::default(),
        occluded: Default::default(),
        mouse: Default::default(),
        touch: Default::default(),
        dirty: Default::default(),
    }
}

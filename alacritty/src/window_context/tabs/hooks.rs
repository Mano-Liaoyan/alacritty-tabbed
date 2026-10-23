//! Calls from the window context, which upstream owns, into the tabs, the panes and the sessions.
//!
//! Each function stands in for, or wraps, a bit of [`WindowContext`] marked `tabbed:` there, so
//! that the window context keeps upstream's code.

use std::error::Error;
use std::mem;
use std::rc::Rc;
use std::time::Instant;

use glutin::config::Config as GlutinConfig;
use glutin::display::GetGlDisplay;
#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
use glutin::platform::x11::X11GlConfigExt;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::WindowId;

use alacritty_terminal::event::OnResize;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Term;
#[cfg(windows)]
use alacritty_terminal::tty::Pty;
use alacritty_terminal::tty::Options as PtyOptions;

use crate::cli::{ParsedOptions, WindowOptions};
use crate::config::UiConfig;
use crate::config::window::StartupMode;
use crate::display::window::Window;
use crate::display::{Display, SizeInfo};
use crate::event::{Event, EventProxy};
use crate::renderer::{self, Frame};
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
pub fn replay(
    mut terminal: Term<EventProxy>,
    saved: Option<&restore::Window>,
) -> Term<EventProxy> {
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

/// Create a window for a tab torn off another, which adopts its running terminals.
///
/// Without a window, the tab is handed back with its terminals untouched.
pub fn torn_window(
    gl_config: &GlutinConfig,
    event_loop: &ActiveEventLoop,
    config: Rc<UiConfig>,
    mut torn: Torn,
) -> Result<WindowContext, (Box<dyn Error>, Box<Torn>)> {
    let (position, size) = (torn.position, torn.size);
    // The window takes the size of the one it was torn off, rather than the startup mode's.
    let mut windowed = (*config).clone();
    windowed.window.startup_mode = StartupMode::Windowed;
    let display = display(gl_config, event_loop, &windowed, &mut Default::default(), |w| {
        if let Some(position) = position {
            w.winit().set_outer_position(position);
        }
        w.request_inner_size(size);
    });
    let display = match display {
        Ok(display) => display,
        Err(err) => return Err((err, Box::new(torn))),
    };
    // The source window hands over a tab whose focused pane is not live.
    let Some((id, live)) = torn.live() else {
        return Err(("the torn tab has no terminal to show".into(), Box::new(torn)));
    };

    // From here on the terminal belongs to the new window, whatever happens.
    let zoom = live.zoom;
    let mut window_context = assemble(display, config.clone(), id, live);
    // A configured size gives way to the size of the window it was torn off.
    window_context.display.window.request_inner_size(size);
    window_context.display.window.hold = torn.hold;
    adopt(&mut window_context, torn, &config);
    // A zoomed pane keeps its text size.
    if let Some(size) = zoom {
        set_live_font(&mut window_context, size);
    }
    window_context.display.window.winit().focus_window();
    Ok(window_context)
}

/// Create a window and its display with the graphics platform other windows are using,
/// letting `prepare` adjust the window before it is shown, like [`WindowContext::additional`].
fn display(
    gl_config: &GlutinConfig,
    event_loop: &ActiveEventLoop,
    config: &UiConfig,
    options: &mut WindowOptions,
    prepare: impl FnOnce(&Window),
) -> Result<Display, Box<dyn Error>> {
    let gl_display = gl_config.display();

    let mut identity = config.window.identity.clone();
    options.window_identity.override_identity_config(&mut identity);

    // Check if new window will be opened as a tab.
    // This must be done before `Window::new()`, which unsets `window_tabbing_id`.
    #[cfg(target_os = "macos")]
    let tabbed = options.window_tabbing_id.is_some();
    #[cfg(not(target_os = "macos"))]
    let tabbed = false;

    let window = Window::new(
        event_loop,
        config,
        &identity,
        options,
        #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
        gl_config.x11_visual(),
    )?;
    prepare(&window);

    // Create context.
    let raw_window_handle = window.raw_window_handle();
    let gl_context =
        renderer::platform::create_gl_context(&gl_display, gl_config, Some(raw_window_handle))?;

    Ok(Display::new(window, gl_context, config, tabbed)?)
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

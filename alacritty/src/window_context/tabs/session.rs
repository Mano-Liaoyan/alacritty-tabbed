//! The terminal of a pane: started for a new pane, swapped with the window's live terminal when
//! the pane is focused, and handed to another window when its tab is torn off.

use std::error::Error;
use std::mem;
#[cfg(not(windows))]
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use crossfont::Size as FontSize;
use winit::event_loop::EventLoopProxy;
use winit::window::WindowId;

use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::tty;

use crate::cli::ParsedOptions;
use crate::config::UiConfig;
use crate::config::font::Font;
use crate::display::SizeInfo;
use crate::event::{Event, EventProxy, InlineSearchState, SearchState};
use crate::window_context::WindowContext;

#[cfg(windows)]
use super::cwd;
use super::profiles::Profile;
use super::{Live, next_id, pane_config, restore};

/// Per-terminal state of a [`WindowContext`], kept aside while its tab is inactive.
pub struct Session {
    pub(super) terminal: Arc<FairMutex<Term<EventProxy>>>,
    pub(super) notifier: Notifier,
    search_state: SearchState,
    inline_search_state: InlineSearchState,
    cursor_blink_timed_out: bool,
    prev_bell_cmd: Option<Instant>,
    pub(super) preserve_title: bool,
    pub(super) window_config: ParsedOptions,
    #[cfg(not(windows))]
    pub(super) master_fd: RawFd,
    pub(super) shell_pid: u32,
    /// The pane's configuration, with its profile's overrides and padding.
    pub(super) config: Rc<UiConfig>,
    /// Size of the pane's terminal.
    pub(super) size: SizeInfo,
    /// Size of the pane's text, in the window's pixels, while it is zoomed away from its
    /// configuration's.
    pub(super) zoom: Option<FontSize>,
    /// Whether another window took over the terminal, which then keeps running.
    adopted: bool,
}

impl Session {
    /// Start a terminal for a new pane, with the settings of `profile`, in `directory` or else
    /// the profile's, showing the text `history` saved from a pane before its shell starts.
    pub(super) fn spawn(
        ctx: &WindowContext,
        config: &Rc<UiConfig>,
        proxy: &EventLoopProxy<Event>,
        profile: Option<&Profile>,
        directory: Option<PathBuf>,
        history: Option<&str>,
    ) -> Result<(WindowId, Self), Box<dyn Error>> {
        let options = profile.map_or_else(Vec::new, Profile::overrides);
        let mut window_config = ParsedOptions::from_options(&options);
        let config = pane_config(config, &mut window_config, ctx.display.tabs.inset);
        let mut pty_config = config.pty_config();
        if let Some(profile) = profile {
            profile.apply(&mut pty_config);
        }
        if directory.is_some() {
            pty_config.working_directory = directory;
        }

        let id = next_id();
        let event_proxy = EventProxy::new(proxy.clone(), id);
        let size = ctx.display.size_info;
        let mut terminal = Term::new(config.term_options(), &size, event_proxy.clone());
        if let Some(history) = history {
            restore::replay(&mut terminal, history);
        }
        let terminal = Arc::new(FairMutex::new(terminal));

        #[cfg(windows)]
        let marker = cwd::mark(&mut pty_config);
        // Shells and `alacritty msg` refer to the window, not the tab.
        let pty = tty::new(&pty_config, size.into(), ctx.id().into())?;
        #[cfg(not(windows))]
        let (master_fd, shell_pid) = (pty.file().as_raw_fd(), pty.child().id());
        #[cfg(windows)]
        let shell_pid = pty.child_watcher().pid().map_or(0, |pid| pid.get());
        #[cfg(windows)]
        cwd::register(shell_pid, marker);
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
            shell_pid,
            config,
            size,
            zoom: None,
            adopted: false,
        }))
    }

    /// The pane's font at its size, in a window of `scale`.
    pub(super) fn font(&self, scale: f32) -> Font {
        let size = self.zoom.unwrap_or_else(|| self.config.font.size().scale(scale));
        self.config.font.clone().with_size(size)
    }

    /// Exchange this terminal with the window's live one, and its text size, which the window's
    /// display takes.
    pub(super) fn swap(&mut self, ctx: &mut WindowContext) {
        let scale = ctx.display.window.scale_factor as f32;
        let incoming = self.font(scale).size();
        mem::swap(&mut self.terminal, &mut ctx.terminal);
        mem::swap(&mut self.notifier, &mut ctx.notifier);
        mem::swap(&mut self.search_state, &mut ctx.search_state);
        mem::swap(&mut self.inline_search_state, &mut ctx.inline_search_state);
        mem::swap(&mut self.cursor_blink_timed_out, &mut ctx.cursor_blink_timed_out);
        mem::swap(&mut self.prev_bell_cmd, &mut ctx.prev_bell_cmd);
        mem::swap(&mut self.preserve_title, &mut ctx.preserve_title);
        mem::swap(&mut self.window_config, &mut ctx.window_config);
        #[cfg(not(windows))]
        mem::swap(&mut self.master_fd, &mut ctx.master_fd);
        mem::swap(&mut self.shell_pid, &mut ctx.shell_pid);
        self.config = ctx.config.clone();
        self.size = ctx.display.size_info;
        let live = mem::replace(&mut ctx.display.font_size, incoming);
        self.zoom = (live != self.config.font.size().scale(scale)).then_some(live);
    }

    /// Hand the terminal to another window, which keeps it running.
    pub(super) fn into_live(mut self) -> Live {
        self.adopted = true;
        Live {
            terminal: self.terminal.clone(),
            notifier: Notifier(self.notifier.0.clone()),
            search_state: mem::take(&mut self.search_state),
            inline_search_state: mem::take(&mut self.inline_search_state),
            window_config: mem::take(&mut self.window_config),
            preserve_title: self.preserve_title,
            #[cfg(not(windows))]
            master_fd: self.master_fd,
            shell_pid: self.shell_pid,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.adopted {
            let _ = self.notifier.0.send(Msg::Shutdown);
        }
    }
}

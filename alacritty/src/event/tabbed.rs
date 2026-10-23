//! Calls from the event processor, which upstream owns, into the tabs and the sessions.
//!
//! Each function stands in for, or wraps, a bit of [`Processor`] marked `tabbed:` there, so that
//! the processor keeps upstream's code.

use std::error::Error;
use std::mem;
use std::time::Instant;

use log::{error, warn};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

use crate::cli::{Options as CliOptions, WindowOptions};
use crate::config::window::tabbed::RestoreSession;
use crate::event::{Event, Processor};
use crate::window_context::WindowContext;
use crate::window_context::tabs::hooks::torn_window;
use crate::window_context::tabs::{self, Torn, restore};

pub use crate::window_context::tabs::reconfigure;

/// The state of the session, which the processor keeps for the tabs.
pub struct State {
    /// The window of the last session the next window created restores.
    pub saved: Option<restore::Window>,
    /// Windows closed as windows, and when, to save with the last one.
    closed: Vec<(Instant, restore::Window)>,
    /// Whether Alacritty was started plainly, which alone restores and saves sessions.
    plain: bool,
}

impl State {
    pub fn new(cli_options: &CliOptions) -> Self {
        Self { saved: None, closed: Default::default(), plain: restore::plain(cli_options) }
    }
}

impl Processor {
    /// Create the initial window, like the first window of the last session when it is restored,
    /// then the other windows of that session.
    pub(super) fn create_initial_windows(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_options: WindowOptions,
    ) -> Result<(), Box<dyn Error>> {
        // The windows of the last session, when it is restored.
        let mut saved = restore::take(&self.config, &self.cli_options).into_iter();
        let first = saved.next();
        let restoring = first.is_some();
        self.tabbed.saved = first;
        let mut created = self.create_initial_window(event_loop, window_options.clone());
        // A saved window that cannot open, such as with a shell since removed, gives way to a
        // window as without a saved session.
        if let (Err(err), true) = (&created, restoring) {
            warn!("Could not restore the first window: {err}");
            self.tabbed.saved = None;
            created = self.create_initial_window(event_loop, window_options.clone());
        }
        created?;
        for window in saved {
            for window_context in self.windows.values_mut() {
                window_context.display.make_not_current();
            }
            self.tabbed.saved = Some(window);
            if let Err(err) = self.create_window(event_loop, window_options.clone()) {
                error!("Could not restore a window: {err:?}");
            }
        }
        self.tabbed.saved = None;
        // Its text is no longer kept on disk once restored.
        if restoring {
            restore::restored();
        }
        Ok(())
    }

    /// Hand the window event `event` of the window `window_id` to its tabs, returning the event
    /// the terminal should get.
    pub(super) fn tab_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) -> Option<WindowEvent> {
        let window_context = self.windows.get_mut(&window_id)?;
        let (config, proxy, clipboard) = (&self.config, &self.proxy, &mut self.clipboard);
        let event = tabs::window_event(window_context, event, config, proxy, clipboard);
        if event.is_none() {
            self.tear_off(event_loop, window_id);
        }
        event
    }

    /// Give a tab torn off the window `window_id`, by a drag or a menu, a window of its own.
    pub(super) fn tear_off(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId) {
        let Some(window_context) = self.windows.get_mut(&window_id) else { return };
        if let Some(torn) = tabs::take_torn(window_context) {
            self.create_torn_window(event_loop, window_id, torn);
        }
    }

    /// Create a window for the tab `torn` off the window `source`, which gets it back on failure.
    fn create_torn_window(&mut self, event_loop: &ActiveEventLoop, source: WindowId, torn: Torn) {
        // No context may be current while a window is created, as for `CreateWindow`.
        for window_context in self.windows.values_mut() {
            window_context.display.make_not_current();
        }

        let gl_config = self.gl_config.as_ref().expect("windows share the first one's config");
        match torn_window(gl_config, event_loop, self.proxy.clone(), self.config.clone(), torn) {
            Ok(window_context) => {
                self.windows.insert(window_context.id(), window_context);
            },
            Err((err, torn)) => {
                error!("Could not open a window for the tab: {err:?}");
                if let Some(window_context) = self.windows.get_mut(&source) {
                    torn.restore(window_context, &self.config);
                }
            },
        }
    }

    /// Hand the events of the terminals of other panes than the live ones to their panes,
    /// returning those left for the processor.
    pub(super) fn route(&mut self, event: Event) -> Option<Event> {
        tabs::route(&mut self.windows, &self.config, event.payload, event.window_id)
    }

    /// Keep the state of `window_context`, which is closing, for the session saved once the last
    /// window closes: windows closed as windows within a moment are saved together, while a
    /// session ended by closing its tabs or shells is not saved.
    pub(super) fn remember(&mut self, window_context: &WindowContext) {
        let level = self.config.window.tabbed.restore_session;
        let whole = window_context.display.tabs.closed_whole();
        let now = Instant::now();
        let saved = self.tabbed.plain && level != RestoreSession::Off && whole;
        let window = saved.then(|| restore::capture(window_context, level));
        restore::remember(&mut self.tabbed.closed, now, window);
        if self.windows.is_empty() {
            let windows = restore::kept(mem::take(&mut self.tabbed.closed), now, whole);
            self.quit(windows);
        }
    }

    /// Forget the saved session at once when the new config turns restoring off.
    pub(super) fn forget_session(&mut self) {
        if self.tabbed.plain && self.config.window.tabbed.restore_session == RestoreSession::Off {
            self.tabbed.closed.clear();
            restore::forget();
        }
    }

    /// Save all the windows, as Alacritty quits with windows open, such as by
    /// `alacritty msg shutdown`.
    pub(super) fn save_session(&mut self) {
        let level = self.config.window.tabbed.restore_session;
        if self.tabbed.plain && level != RestoreSession::Off && !self.windows.is_empty() {
            let now = Instant::now();
            let mut windows = restore::kept(mem::take(&mut self.tabbed.closed), now, true);
            windows.extend(self.windows.values().map(|ctx| restore::capture(ctx, level)));
            self.quit(windows);
        }
    }

    /// Save the session of `windows` as Alacritty quits, or forget it, as the setting asks.
    fn quit(&self, windows: Vec<restore::Window>) {
        match restore::quit(self.tabbed.plain, self.config.window.tabbed.restore_session, windows) {
            restore::Quit::Save(windows) => {
                if let Err(err) = restore::save(windows) {
                    error!("Could not save the session: {err}");
                }
            },
            restore::Quit::Forget => restore::forget(),
            restore::Quit::Leave => (),
        }
    }
}

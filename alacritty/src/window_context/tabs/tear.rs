//! Tearing a tab, or a pane of a group, off into a window of its own.
//!
//! The terminals keep running: the torn tab is taken out of its window as it is, and the window
//! made for it adopts it, its focused pane becoming that window's live terminal.
//!
//! The model and the hand-over are here; taking the tab out and the new window come with the
//! windows' own work.
#![allow(dead_code, unused_variables)]

#[cfg(not(windows))]
use std::os::unix::io::RawFd;
use std::rc::Rc;
use std::sync::Arc;

use winit::dpi::{PhysicalPosition, PhysicalSize};

use alacritty_terminal::event_loop::Notifier;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;

use crate::cli::ParsedOptions;
use crate::config::UiConfig;
use crate::event::{EventProxy, InlineSearchState, SearchState};
use crate::window_context::WindowContext;

use super::dnd::Source;
use super::{Session, Tab, Tabs};

/// A running terminal handed to a new window, which makes it its live one.
pub struct Live {
    pub terminal: Arc<FairMutex<Term<EventProxy>>>,
    pub notifier: Notifier,
    pub search_state: SearchState,
    pub inline_search_state: InlineSearchState,
    /// The terminal's configuration overrides.
    pub window_config: ParsedOptions,
    pub preserve_title: bool,
    #[cfg(not(windows))]
    pub master_fd: RawFd,
    #[cfg(not(windows))]
    pub shell_pid: u32,
}

/// A tab taken out of its window, waiting for a window of its own.
pub struct Torn {
    pub(super) tab: Tab,
    /// Where to put the new window, unless the system places it.
    pub position: Option<PhysicalPosition<i32>>,
    /// Inner size of the new window.
    pub size: PhysicalSize<u32>,
}

impl Torn {
    /// Take the terminal of the focused pane, which becomes the new window's live one.
    pub fn live(&mut self) -> Option<Live> {
        let focus = self.tab.focus;
        self.tab.panes[focus].session.take().map(Session::into_live)
    }
}

/// Whether `source` can leave for a window of its own without emptying this one.
pub fn can_tear(tabs: &Tabs, source: Source) -> bool {
    tearable(&tabs.tabs, source)
}

/// Whether `source` can leave `tabs` with a pane left behind.
pub(super) fn tearable<S>(tabs: &[Tab<S>], source: Source) -> bool {
    match source {
        Source::Pane(tab, pane) if tabs.get(tab).is_some_and(|tab| tab.panes.len() > 1) => {
            pane < tabs[tab].panes.len()
        },
        // A tab's only pane stands for the tab.
        Source::Tab(tab) | Source::Pane(tab, _) => tab < tabs.len() && tabs.len() > 1,
    }
}

/// Take `source` out of the window for a window of its own, placed for the pointer at `pointer`
/// or else beside this one; returns whether it was taken.
pub fn tear_off(
    ctx: &mut WindowContext,
    source: Source,
    pointer: Option<PhysicalPosition<f64>>,
    config: &Rc<UiConfig>,
) -> bool {
    false
}

/// The tab torn off the window, once, for the event loop to give it a window.
pub fn take(ctx: &mut WindowContext) -> Option<Torn> {
    ctx.display.tabs.torn.take()
}

/// Install `torn` in the new window `ctx`, whose live terminal is already its focused pane's.
pub fn adopt(ctx: &mut WindowContext, torn: Torn, config: &Rc<UiConfig>) {}

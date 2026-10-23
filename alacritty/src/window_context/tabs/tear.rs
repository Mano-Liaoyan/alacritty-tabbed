//! Tearing a tab, or a pane of a group, off into a window of its own.
//!
//! The terminals keep running: the torn tab is taken out of its window as it is, and the window
//! made for it adopts it, its focused pane becoming that window's live terminal.
//!
//! Known limitation: a shell keeps the `ALACRITTY_WINDOW_ID` of the window it was started in.

#[cfg(not(windows))]
use std::os::unix::io::RawFd;
use std::rc::Rc;
use std::sync::Arc;

use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::window::WindowId;

use alacritty_terminal::event_loop::Notifier;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;

use crate::cli::ParsedOptions;
use crate::config::UiConfig;
use crate::event::{EventProxy, InlineSearchState, SearchState};
use crate::window_context::WindowContext;

use super::bar::Layout;
use super::dnd::Source;
use super::{Tab, Tabs, focus_pane, platform, relayout, sync};

/// Where the pointer holds a tab torn off by dragging: this far into the new window's first tab,
/// in logical pixels.
const GRAB: f32 = 24.;

/// How far down and right of its window a tab torn off from a menu opens, in logical pixels.
const CASCADE: f64 = 32.;

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
    pub shell_pid: u32,
    /// Size of the terminal's text, in its window's pixels, while it is zoomed away from its
    /// configuration's.
    pub zoom: Option<crossfont::Size>,
}

/// A tab taken out of its window, waiting for a window of its own.
pub struct Torn {
    pub(super) tab: Tab,
    /// Where to put the new window, unless the system places it.
    pub position: Option<PhysicalPosition<i32>>,
    /// Inner size of the new window.
    pub size: PhysicalSize<u32>,
    /// Whether the window it was torn off keeps terminals open after they exit.
    pub hold: bool,
}

impl Torn {
    /// Take the terminal of the focused pane, which becomes the new window's live one, with the
    /// id it reports its events with.
    pub fn live(&mut self) -> Option<(WindowId, Live)> {
        let focus = self.tab.focus;
        let pane = &mut self.tab.panes[focus];
        Some((pane.id, pane.session.take()?.into_live()))
    }

    /// Give the tab back to the window `ctx` it was torn off, as its last tab, when no window
    /// could be made for it; dropping it would end its terminals.
    pub fn restore(self, ctx: &mut WindowContext, config: &Rc<UiConfig>) {
        ctx.display.tabs.tabs.push(self.tab);
        sync(ctx, config, false);
        relayout(ctx);
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
    let tabs = &ctx.display.tabs;
    if tabs.torn.is_some() || !can_tear(tabs, source) {
        return false;
    }
    // The live terminal stays in this window, so its pane hands the focus over first.
    if let Some((tab, pane)) = stand_in(&tabs.tabs, tabs.active, source) {
        focus_pane(ctx, tab, pane, config);
    }

    // The new window's first tab starts where this window's first shown tab does.
    let tabs = &ctx.display.tabs;
    let grab = tabs.font.is_some().then(|| Layout::new(tabs)).and_then(|layout| {
        let tab = layout.tab(layout.first)?;
        let x = tab.x + (tab.w / 2.).min(GRAB * layout.scale);
        Some((f64::from(x), f64::from(tab.y + tab.h / 2.)))
    });
    let window = ctx.display.window.winit();
    let cascade = CASCADE * window.scale_factor();
    let origin = window.outer_position().ok();
    let position = placement(origin, pointer, grab.unwrap_or_default(), cascade);
    let size = window.inner_size();

    let tabs = &mut ctx.display.tabs;
    // A drag refers to tabs and panes by their place, which this changes.
    tabs.drag = None;
    let Some(tab) = detach(&mut tabs.tabs, &mut tabs.active, source) else { return false };
    tabs.torn = Some(Torn { tab, position, size, hold: ctx.display.window.hold });
    sync(ctx, config, false);
    relayout(ctx);
    true
}

/// The tab torn off the window, once, for the event loop to give it a window.
pub fn take(ctx: &mut WindowContext) -> Option<Torn> {
    ctx.display.tabs.torn.take()
}

/// Install `torn` in the new window `ctx`, whose live terminal is already its focused pane's.
pub fn adopt(ctx: &mut WindowContext, torn: Torn, config: &Rc<UiConfig>) {
    let title = torn.tab.pane().title.clone();
    let tabs = &mut ctx.display.tabs;
    // In place before the window first shows its tabs, which would make a pane of the window.
    tabs.host = platform::hostname().to_uppercase();
    tabs.tabs = vec![torn.tab];
    tabs.active = 0;
    // The live terminal's overrides, then its place and its title.
    ctx.update_config(config.clone());
    sync(ctx, config, true);
    ctx.display.window.set_title(title);
}

/// The pane to focus before `source` leaves, when it holds the live terminal: its neighbour in
/// its group, or else the next tab, or the previous one after the last.
fn stand_in<S>(tabs: &[Tab<S>], active: usize, source: Source) -> Option<(usize, usize)> {
    let tab = tabs.get(active)?;
    match source {
        Source::Pane(index, pane) if index == active && tab.panes.len() > 1 => {
            (pane == tab.focus).then(|| (active, tab.neighbour(pane).unwrap_or_default()))
        },
        Source::Tab(index) | Source::Pane(index, _) if index == active => {
            let next = if active + 1 < tabs.len() { active + 1 } else { active.checked_sub(1)? };
            Some((next, tabs[next].focus))
        },
        _ => None,
    }
}

/// Take `source` out of `tabs`: a pane of a group as a tab of its own, or else a whole tab.
///
/// Nothing is taken that would empty `tabs`, nor the live terminal, which must be focused away
/// from first.
fn detach<S>(tabs: &mut Vec<Tab<S>>, active: &mut usize, source: Source) -> Option<Tab<S>> {
    if !tearable(tabs, source) {
        return None;
    }
    match source {
        Source::Pane(tab, pane) if tabs[tab].panes.len() > 1 => {
            let live = (tab, pane) == (*active, tabs[tab].focus);
            (!live).then(|| Tab::new(tabs[tab].remove(pane)))
        },
        Source::Tab(tab) | Source::Pane(tab, _) => {
            if tab == *active {
                return None;
            }
            if *active > tab {
                *active -= 1;
            }
            Some(tabs.remove(tab))
        },
    }
}

/// Outer position of a window torn off a window at `origin`: so that its point `grab` lies under
/// `pointer`, both relative to the windows' insides, or else `cascade` pixels down and right.
///
/// None where the system does not tell where windows are, and places them itself.
fn placement(
    origin: Option<PhysicalPosition<i32>>,
    pointer: Option<PhysicalPosition<f64>>,
    grab: (f64, f64),
    cascade: f64,
) -> Option<PhysicalPosition<i32>> {
    let origin = origin?;
    let (dx, dy) = pointer.map_or((cascade, cascade), |at| (at.x - grab.0, at.y - grab.1));
    Some(PhysicalPosition::new(origin.x + dx.round() as i32, origin.y + dy.round() as i32))
}

#[cfg(test)]
mod tests {

    use super::super::Pane;
    use super::super::split::{Axis, Side};
    use super::*;

    /// A pane whose terminal, kept aside unless it is live, is its id.
    fn pane(id: u64, live: bool) -> Pane<u64> {
        let (name, title, badge) = (String::new(), String::new(), String::new());
        let session = (!live).then_some(id);
        Pane { id: WindowId::from(id), name, title, badge, icon: None, profile: None, session }
    }

    /// A group of panes 1 and 2, focused on 1 and live, and a tab of pane 3.
    fn tabs() -> Vec<Tab<u64>> {
        let mut group = Tab::new(pane(1, true));
        group.insert(WindowId::from(1), pane(2, false), Axis::Row, Side::After);
        vec![group, Tab::new(pane(3, false))]
    }

    fn ids<S>(tab: &Tab<S>) -> Vec<u64> {
        tab.panes.iter().map(|pane| u64::from(pane.id)).collect()
    }

    #[test]
    fn detach_pane_of_group() {
        let (mut tabs, mut active) = (tabs(), 0);
        let torn = detach(&mut tabs, &mut active, Source::Pane(0, 1)).unwrap();
        assert_eq!((ids(&torn), torn.panes[0].session), (vec![2], Some(2)));
        assert_eq!((ids(&tabs[0]), tabs.len(), active), (vec![1], 2, 0));
    }

    #[test]
    fn detach_whole_tab() {
        let (mut tabs, mut active) = (tabs(), 1);
        // The live pane is focused away first: here pane 3 of the tab before.
        tabs[0].panes[0].session = Some(1);
        tabs[1].panes[0].session = None;
        let torn = detach(&mut tabs, &mut active, Source::Tab(0)).unwrap();
        assert_eq!((ids(&torn), tabs.len(), active), (vec![1, 2], 1, 0));
        assert_eq!(ids(&tabs[0]), vec![3]);
        // A tab's only pane stands for its tab.
        let (mut tabs, mut active) = (self::tabs(), 0);
        let torn = detach(&mut tabs, &mut active, Source::Pane(1, 0)).unwrap();
        assert_eq!((ids(&torn), tabs.len(), active), (vec![3], 1, 0));
    }

    #[test]
    fn live_source_refocuses_first() {
        let (mut tabs, mut active) = (tabs(), 0);
        // Not while it is live.
        assert!(detach(&mut tabs, &mut active, Source::Pane(0, 0)).is_none());
        assert!(detach(&mut tabs, &mut active, Source::Tab(0)).is_none());
        // The live pane of a group hands over to its neighbour, a live tab to the next one, or
        // the previous one after the last; other sources need nothing.
        assert_eq!(stand_in(&tabs, 0, Source::Pane(0, 0)), Some((0, 1)));
        assert_eq!(stand_in(&tabs, 0, Source::Tab(0)), Some((1, 0)));
        assert_eq!(stand_in(&tabs, 1, Source::Tab(1)), Some((0, 0)));
        assert_eq!(stand_in(&tabs, 1, Source::Pane(1, 0)), Some((0, 0)));
        assert_eq!(stand_in(&tabs, 0, Source::Pane(0, 1)), None);
        assert_eq!(stand_in(&tabs, 0, Source::Tab(1)), None);

        // Focused on its neighbour, the pane leaves with its terminal.
        tabs[0].focus = 1;
        tabs[0].panes[0].session = Some(1);
        tabs[0].panes[1].session = None;
        let torn = detach(&mut tabs, &mut active, Source::Pane(0, 0)).unwrap();
        assert_eq!((ids(&torn), torn.panes[0].session), (vec![1], Some(1)));
        assert_eq!((ids(&tabs[0]), tabs[0].focus), (vec![2], 0));
    }

    #[test]
    fn detach_refuses_last_pane() {
        let mut tabs = vec![Tab::new(pane(1, false))];
        let mut active = 0;
        assert!(detach(&mut tabs, &mut active, Source::Tab(0)).is_none());
        assert!(detach(&mut tabs, &mut active, Source::Pane(0, 0)).is_none());
        assert!(detach(&mut tabs, &mut active, Source::Tab(5)).is_none());
        assert_eq!(tabs.len(), 1);
    }

    #[test]
    fn window_placement() {
        let origin = Some(PhysicalPosition::new(100, 50));
        let pointer = Some(PhysicalPosition::new(-40.4, 320.6));
        // The grabbed point of the first tab lands under the pointer.
        let at = placement(origin, pointer, (60., 20.), 64.);
        assert_eq!(at, Some(PhysicalPosition::new(0, 351)));
        // Without a pointer, cascaded from the window.
        assert_eq!(placement(origin, None, (60., 20.), 64.), Some(PhysicalPosition::new(164, 114)));
        // Where windows are not known, the system places them.
        assert_eq!(placement(None, pointer, (60., 20.), 64.), None);
    }
}

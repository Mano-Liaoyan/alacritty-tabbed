//! Tabs: several terminals sharing one window, under a caption drawn by Alacritty's renderer.
//!
//! Each tab is a group of panes, which split its area between them like tmux's; a tab of one pane
//! is a classic tab. Each pane runs a terminal.
//!
//! The focused pane of the active tab is live: its terminal lives in the [`WindowContext`] like
//! that of any window, while every other pane keeps its own in a [`Session`] that is swapped in
//! when the pane is focused. Each terminal reports its events under its own id, so events of the
//! other panes are handled here.
//!
//! The caption replaces the system's: the live terminal is told the window is the size of its
//! pane, below the bar, and its padding keeps it inside a rounded panel. Pointer events are moved
//! accordingly.

mod anim;
mod app_settings;
mod bar;
mod context;
mod divider;
mod dnd;
mod fonts;
mod icons;
mod paint;
mod panes;
mod peek;
mod profiles;
mod rename;
mod settings;
mod split;
mod tear;
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
use std::path::PathBuf;
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
use crate::config::window::{Corners, UiFont as UiFontConfig};
use crate::config::{Action, BindingKey, BindingMode, UiConfig};
use crate::display::SizeInfo;
use crate::event::{Event, EventProxy, EventType, InlineSearchState, SearchState};
use crate::window_context::WindowContext;

use bar::{BAR, Hit, INSET, Layout, MARGIN};
use dnd::{Drag, Source, Target};
use paint::Image;
pub use paint::Rect;
use profiles::{Profile, Profiles};
use settings::{Page, Settings};
use split::{Axis, Divider, Side, Split};
#[allow(unused_imports, reason = "torn tabs get windows next")]
pub use tear::{Live, Torn, adopt, take as take_torn};
use text::UiFont;

/// Ids for the terminals of new tabs, far from the platform's window ids.
static NEXT_ID: AtomicU64 = AtomicU64::new(1 << 62);

/// Modifiers that drag a pane from anywhere in it, exactly; Control alone selects a block.
#[cfg(not(target_os = "macos"))]
const PANE_DRAG: ModifiersState = ModifiersState::CONTROL.union(ModifiersState::ALT);
#[cfg(target_os = "macos")]
const PANE_DRAG: ModifiersState = ModifiersState::SUPER.union(ModifiersState::ALT);

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
    /// Size of the window.
    window: PhysicalSize<u32>,
    /// Size of the cells the panes were last laid out with.
    cells: (f32, f32),
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
    /// What the left button last pressed on the bar, and when, to tell double clicks.
    last_click: Option<(Hit, Instant)>,
    /// Wheel movement in the settings short of a whole step, in lines.
    wheel: f64,
    /// Where the bar was pressed to move the window, once the pointer moves.
    grab: Option<PhysicalPosition<f64>>,
    drag: Option<Drag>,
    menu: Option<Menu>,
    /// The open context menu of a pane.
    context: Option<context::Menu>,
    settings: Option<Settings>,
    pane_effects: panes::Effects,
    bar_effects: bar::Effects,
    /// Space between the cards of a tab's panes, in logical pixels.
    gap: f32,
    /// The pane whose drag handle is under the pointer.
    handle: Option<usize>,
    divider: divider::State,
    /// The tab whose name is being edited.
    rename: Option<rename::Edit>,
    /// The hovered icon's card.
    peeking: peek::State,
    /// A tab taken out of the window, waiting for a window of its own.
    torn: Option<Torn>,
    /// Close the window instead of a tab once the active terminal exits.
    closing: bool,
    /// Font of the bar, menus and settings, and what it was last loaded for.
    font: Option<Rc<UiFont>>,
    font_source: Option<(UiFontConfig, f64)>,
    /// Corners drawn, once they have been applied to the window.
    corners: Option<Corners>,
    /// Whether the window's corners are cut out of it, which needs a transparent window.
    cut: bool,
    /// Whether the terminal is held in a panel, rather than filling the window.
    panel: bool,
}

/// A group of panes sharing the window, shown as one entry in the bar.
///
/// What a pane keeps aside while it is not live is generic, so the bookkeeping can be tested
/// without terminals.
struct Tab<S = Session> {
    layout: Split<WindowId>,
    /// The panes, in the order of `layout.panes()`.
    panes: Vec<Pane<S>>,
    /// Index of the pane that has the focus, which the tab shows in the bar.
    focus: usize,
    /// Whether only the focused pane is shown, over the whole area.
    zoom: bool,
    /// Name given to the tab, shown instead of its focused pane's title.
    #[allow(dead_code, reason = "the bar shows it next")]
    name: Option<String>,
}

/// A terminal in a tab.
struct Pane<S = Session> {
    /// Id the pane's terminal sends its events with.
    id: WindowId,
    /// Title the terminal resets to.
    name: String,
    /// Title of a pane that is not live; the live one shows the window's title.
    title: String,
    badge: String,
    icon: Option<Arc<Image>>,
    /// Name of the profile the pane was opened with.
    profile: Option<String>,
    /// Terminal of a pane that is not live; the live one's lives in the window context.
    session: Option<S>,
}

/// A pane that is not live, to be drawn beside the live one.
pub struct PaneDraw {
    pub terminal: Arc<FairMutex<Term<EventProxy>>>,
    /// The pane's configuration, with its profile's overrides and padding.
    pub config: Rc<UiConfig>,
    pub size: SizeInfo,
    /// Where to draw it in the window, in pixels from the top left.
    pub frame: Rect,
    /// The pane's card, which nothing of it is drawn outside of.
    #[allow(dead_code, reason = "panes are clipped to it next")]
    pub clip: Rect,
}

/// Where the mouse button that is down was pressed.
#[derive(Default, PartialEq, Eq)]
enum Press {
    #[default]
    None,
    Bar,
    Terminal,
    /// On a divider between panes, which it drags.
    Divider,
    /// Over something that used the press, such as a pane it focused, so the terminal gets
    /// neither it nor its release.
    Swallowed,
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
    /// The pane's configuration, with its profile's overrides and padding.
    config: Rc<UiConfig>,
    /// Size of the pane's terminal.
    size: SizeInfo,
    /// Whether another window took over the terminal, which then keeps running.
    adopted: bool,
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
    Zoom,
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

    /// Whether the window has transparent corners.
    pub fn transparent(&self) -> bool {
        self.cut
    }

    fn len(&self) -> usize {
        self.tabs.len().max(1)
    }

    /// Number of panes in all tabs.
    fn count(&self) -> usize {
        self.tabs.iter().map(|tab| tab.panes.len()).sum()
    }

    /// The tab and the pane whose terminal reports events as `id`, in the window `window`.
    fn locate(&self, window: WindowId, id: WindowId) -> Option<(usize, usize)> {
        if self.tabs.is_empty() {
            return (id == window).then_some((0, 0));
        }
        self.tabs.iter().enumerate().find_map(|(index, tab)| Some((index, tab.position(id)?)))
    }

    /// The tab and the pane whose terminal is live.
    fn live(&self) -> (usize, usize) {
        (self.active, self.tabs.get(self.active).map_or(0, |tab| tab.focus))
    }

    /// Id of the live terminal, in the window `window`.
    fn live_id(&self, window: WindowId) -> WindowId {
        self.tabs.get(self.active).map_or(window, |tab| tab.pane().id)
    }

    /// The window below the bar, which the active tab's panes share.
    fn area(&self) -> Rect {
        let (width, height) = (self.window.width as f32, self.window.height as f32);
        let top = (self.height as f32).min(height);
        Rect::new(0., top, width, height - top)
    }

    /// Pixels between the places of neighbouring panes, beyond the margins of their cards; it
    /// may be negative, where the margins overlap.
    fn spacing(&self) -> f32 {
        if !self.panel || self.frame == Frame::Fullscreen {
            return 0.;
        }
        ((self.gap - 2. * MARGIN) * self.scale as f32).round()
    }

    /// The card a pane placed at `rect` is drawn in, inside its margin.
    pub fn card(&self, rect: Rect) -> Rect {
        rect.inset(self.margin())
    }

    /// Margin around each pane's card, in pixels.
    fn margin(&self) -> f32 {
        let panel = self.panel && self.frame != Frame::Fullscreen;
        if panel { (MARGIN * self.scale as f32).round() } else { 0. }
    }

    /// Where each shown pane of the active tab is placed, by its index: only the focused one
    /// while the tab is zoomed.
    pub fn pane_rects(&self) -> Vec<(usize, Rect)> {
        let Some(tab) = self.tabs.get(self.active) else { return Vec::new() };
        if tab.zoom {
            return vec![(tab.focus, self.area())];
        }
        let rects = tab.layout.rects(self.area(), self.spacing()).into_iter();
        rects.enumerate().map(|(index, (_, rect))| (index, rect)).collect()
    }

    /// The dividers between the active tab's panes; none while it is zoomed.
    #[allow(dead_code, reason = "dividers are dragged next")]
    pub fn dividers(&self) -> Vec<Divider> {
        match self.tabs.get(self.active) {
            Some(tab) if !tab.zoom => tab.layout.dividers(self.area(), self.spacing()),
            _ => Vec::new(),
        }
    }

    /// The pane of the active tab at `point`.
    fn pane_at(&self, point: PhysicalPosition<f64>) -> Option<usize> {
        nearest(&self.pane_rects(), point, self.margin())
    }

    /// Where the live terminal's pane is placed.
    fn focused_rect(&self) -> Rect {
        let focus = self.live().1;
        let rects = self.pane_rects();
        let rect = rects.into_iter().find(|&(index, _)| index == focus);
        rect.map_or_else(|| self.area(), |(_, rect)| rect)
    }

    /// Where to draw the live terminal at `now`.
    pub fn focused_frame(&self, now: Instant) -> Rect {
        // The panes' chrome is drawn for the same moment as their terminals.
        self.pane_effects.hold(now);
        let rect = self.focused_rect();
        let id = self.tabs.get(self.active).map(|tab| tab.pane().id);
        id.map_or(rect, |id| self.pane_effects.frame(id, rect, now))
    }

    /// The card of the live terminal at `now`, which nothing of it is drawn outside of.
    #[allow(dead_code, reason = "panes are clipped to it next")]
    pub fn focused_clip(&self, now: Instant) -> Rect {
        self.card(self.focused_frame(now))
    }

    /// The shown panes of the active tab other than the live one, and where to draw them at
    /// `now`.
    pub fn background_panes(&self, now: Instant) -> Vec<PaneDraw> {
        let Some(tab) = self.tabs.get(self.active) else { return Vec::new() };
        self.pane_rects()
            .into_iter()
            .filter_map(|(index, rect)| {
                let pane = &tab.panes[index];
                let session = pane.session.as_ref()?;
                let frame = self.pane_effects.frame(pane.id, rect, now);
                Some(PaneDraw {
                    terminal: session.terminal.clone(),
                    config: session.config.clone(),
                    size: session.size,
                    frame,
                    clip: self.card(frame),
                })
            })
            .collect()
    }

    /// Whether anything is still moving at `now`, which needs another frame.
    pub fn animating(&self, now: Instant) -> bool {
        self.pane_effects.running(now)
            || self.bar_effects.running(now)
            || self.context.as_ref().is_some_and(|menu| menu.running(now))
            || self.divider.running(now)
            || self.peeking.running(now)
    }

    /// Every terminal kept aside.
    fn sessions(&mut self) -> impl Iterator<Item = &mut Session> {
        let panes = self.tabs.iter_mut().flat_map(|tab| &mut tab.panes);
        panes.filter_map(|pane| pane.session.as_mut())
    }
}

impl<S> Tab<S> {
    fn new(pane: Pane<S>) -> Self {
        Self { layout: Split::Pane(pane.id), panes: vec![pane], focus: 0, zoom: false, name: None }
    }

    /// The focused pane.
    fn pane(&self) -> &Pane<S> {
        &self.panes[self.focus]
    }

    /// Index of the pane `id`.
    fn position(&self, id: WindowId) -> Option<usize> {
        self.panes.iter().position(|pane| pane.id == id)
    }

    /// Put `pane` beside the pane `target`, returning its index.
    ///
    /// Changing the panes, here and in `join` and `remove`, shows them all again.
    fn insert(&mut self, target: WindowId, pane: Pane<S>, axis: Axis, side: Side) -> usize {
        self.zoom = false;
        let id = pane.id;
        let placed = self.layout.insert(target, Split::Pane(id), axis, side);
        debug_assert!(placed, "panes go beside a pane of their tab");
        self.panes.push(pane);
        self.order();
        self.position(id).unwrap_or_default()
    }

    /// Put all the panes of `other` beside the pane `beside`, or else beside all of this tab's,
    /// keeping this tab's focus.
    fn join(&mut self, other: Tab<S>, beside: Option<WindowId>, axis: Axis, side: Side) {
        self.zoom = false;
        match beside {
            Some(target) => {
                let placed = self.layout.insert(target, other.layout, axis, side);
                debug_assert!(placed, "groups dock beside a pane of their tab");
            },
            None => {
                let focus = self.pane().id;
                let layout = mem::replace(&mut self.layout, Split::Pane(focus));
                self.layout = layout.join(other.layout, axis, side);
            },
        }
        self.panes.extend(other.panes);
        self.order();
    }

    /// Index of the pane that takes the space of the pane at `index` once it closes.
    fn neighbour(&self, index: usize) -> Option<usize> {
        self.layout.neighbour(self.panes[index].id).and_then(|id| self.position(id))
    }

    /// Take out the pane at `index`, of two or more; its neighbour takes its space, and the focus
    /// if it had it.
    fn remove(&mut self, index: usize) -> Pane<S> {
        self.zoom = false;
        let id = self.panes[index].id;
        let focus = if index == self.focus { self.neighbour(index) } else { Some(self.focus) };
        let focus = focus.map(|focus| self.panes[focus].id);
        let layout = mem::replace(&mut self.layout, Split::Pane(id));
        self.layout = layout.remove(id).expect("a tab keeps a pane");
        let pane = self.panes.remove(index);
        self.focus = focus.and_then(|focus| self.position(focus)).unwrap_or_default();
        pane
    }

    /// Sort the panes in the order of the layout, keeping the focus on its pane.
    fn order(&mut self) {
        let focus = self.pane().id;
        let order = self.layout.panes();
        self.panes.sort_by_key(|pane| order.iter().position(|&id| id == pane.id));
        self.focus = self.position(focus).unwrap_or_default();
    }
}

impl<S> Pane<S> {
    /// A pane opened with `profile`, or else with the configuration's shell.
    fn new(id: WindowId, profile: Option<&Profile>, config: &UiConfig, session: Option<S>) -> Self {
        let (name, badge, icon) = match profile {
            Some(profile) => (profile.name.clone(), profile.badge(), profile.image(config)),
            None => {
                let icon = icons::get(None, &profiles::shell(config));
                (config.window.identity.title.clone(), ">".into(), icon)
            },
        };
        let (title, profile) = (name.clone(), profile.map(|profile| profile.name.clone()));
        Self { id, name, title, badge, icon, profile, session }
    }
}

/// Take out the terminal of the pane `(tab, pane)`, to exchange it with the live one.
fn take<S>(tabs: &mut [Tab<S>], (tab, pane): (usize, usize)) -> Option<S> {
    tabs.get_mut(tab)?.panes.get_mut(pane)?.session.take()
}

/// Make the pane `(tab, pane)`, whose terminal was taken out, the live one, and give the pane that
/// was live its terminal `parked`.
fn relive<S>(tabs: &mut [Tab<S>], active: &mut usize, (tab, pane): (usize, usize), parked: S) {
    let old = &mut tabs[*active];
    let focus = old.focus;
    old.panes[focus].session = Some(parked);
    *active = tab;
    tabs[tab].focus = pane;
}

/// The pane of `rects`, places by pane index, at `point`: of places that overlap there, the one
/// whose card, `margin` inside it, is nearest.
fn nearest(rects: &[(usize, Rect)], point: PhysicalPosition<f64>, margin: f32) -> Option<usize> {
    let (x, y) = (point.x as f32, point.y as f32);
    let distance = |rect: &Rect| {
        let card = rect.inset(margin);
        let dx = (card.x - x).max(x - card.right()).max(0.);
        let dy = (card.y - y).max(y - card.bottom()).max(0.);
        dx.hypot(dy)
    };
    let rects = rects.iter().filter(|(_, rect)| rect.contains(point));
    let nearest = rects.min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)));
    nearest.map(|&(index, _)| index)
}

/// The name a tab is given for `name`: trimmed, and none when that leaves nothing.
#[allow(dead_code, reason = "tabs are renamed next")]
fn tidy(name: Option<String>) -> Option<String> {
    let name = name?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Move the live pane to `target`, where it stays live; `Dock` refers to the panes of the tab
/// `shown`. Returns whether it moved.
fn place<S>(
    tabs: &mut Vec<Tab<S>>,
    active: &mut usize,
    shown: usize,
    target: Target,
    area: Rect,
) -> bool {
    let (from, pane) = (*active, tabs[*active].focus);
    let alone = tabs[from].panes.len() == 1;
    // The pane to dock beside, found before the moving pane leaves its place.
    let mut beside = tabs[from].panes[pane].id;
    match target {
        Target::Dock { pane: index, .. } => {
            match tabs.get(shown).and_then(|t| t.panes.get(index)) {
                Some(dock) if (shown, index) != (from, pane) => beside = dock.id,
                _ => return false,
            }
        },
        Target::Merge(tab) if tab == from || tab >= tabs.len() => return false,
        Target::Window => return false,
        _ => (),
    }

    // The pane leaves its tab, and the tab goes when it was its only pane.
    let moved = if alone { tabs.remove(from).panes.remove(0) } else { tabs[from].remove(pane) };
    let shift = |tab: usize| if alone && tab > from { tab - 1 } else { tab };
    let (tab, index) = match target {
        Target::Merge(tab) => {
            let (tab, id) = (shift(tab), moved.id);
            tabs[tab].join(Tab::new(moved), None, split::auto_axis(area), Side::After);
            (tab, tabs[tab].position(id).unwrap_or_default())
        },
        Target::Dock { axis, side, .. } => {
            let tab = shift(shown);
            (tab, tabs[tab].insert(beside, moved, axis, side))
        },
        Target::Reorder(tab) | Target::Detach(tab) => {
            let tab = tab.min(tabs.len());
            tabs.insert(tab, Tab::new(moved));
            (tab, 0)
        },
        Target::Window => unreachable!("refused above"),
    };
    *active = tab;
    tabs[tab].focus = index;
    true
}

/// Move the active tab to `to`, or the end when past it.
fn reorder<S>(tabs: &mut Vec<Tab<S>>, active: &mut usize, to: usize) {
    let tab = tabs.remove(*active);
    *active = to.min(tabs.len());
    tabs.insert(*active, tab);
}

/// Group the active tab with the tab `into`, which takes its place and its focus: docked beside
/// the pane of `dock`, or else beside all of its panes. Returns whether they were grouped.
fn group<S>(
    tabs: &mut Vec<Tab<S>>,
    active: &mut usize,
    into: usize,
    dock: Option<(usize, Axis, Side)>,
    area: Rect,
) -> bool {
    let from = *active;
    if into == from || into >= tabs.len() {
        return false;
    }
    let beside = match dock {
        Some((pane, ..)) => match tabs[into].panes.get(pane) {
            Some(pane) => Some(pane.id),
            None => return false,
        },
        None => None,
    };
    let (axis, side) = dock.map_or((split::auto_axis(area), Side::After), |(_, a, s)| (a, s));
    let tab = tabs.remove(from);
    let into = if into > from { into - 1 } else { into };
    let focus = tab.pane().id;
    tabs[into].join(tab, beside, axis, side);
    tabs[into].focus = tabs[into].position(focus).unwrap_or_default();
    *active = into;
    true
}

impl Session {
    /// Start a terminal for a new pane, with the settings of `profile`, in `directory` or else
    /// the profile's.
    fn spawn(
        ctx: &WindowContext,
        config: &Rc<UiConfig>,
        proxy: &EventLoopProxy<Event>,
        profile: Option<&Profile>,
        directory: Option<PathBuf>,
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
            config,
            size,
            adopted: false,
        }))
    }

    /// Exchange this terminal with the window's live one.
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
        self.config = ctx.config.clone();
        self.size = ctx.display.size_info;
    }

    /// Hand the terminal to another window, which keeps it running.
    fn into_live(mut self) -> Live {
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
            #[cfg(not(windows))]
            shell_pid: self.shell_pid,
        }
    }
}

/// The configuration of a pane: the window's with the pane's `overrides`, padded by `inset`
/// logical pixels to fit the terminal in its panel.
fn pane_config(config: &Rc<UiConfig>, overrides: &mut ParsedOptions, inset: u8) -> Rc<UiConfig> {
    pad(overrides.override_config_rc(config.clone()), inset)
}

/// `config` with the terminal's padding grown by `inset` logical pixels.
fn pad(config: Rc<UiConfig>, inset: u8) -> Rc<UiConfig> {
    let (x, y) = config.window.padding(1.);
    let padding = format!("window.padding={{x={},y={}}}", x as u8 + inset, y as u8 + inset);
    ParsedOptions::from_options(&[padding]).override_config_rc(config)
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.adopted {
            let _ = self.notifier.0.send(Msg::Shutdown);
        }
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
    if let WindowEvent::Resized(size) = event {
        // Minimizing reports an empty size, which the terminal ignores too.
        if size.width == 0 || size.height == 0 {
            return Some(event);
        }
        ctx.display.tabs.window = size;
    }
    sync(ctx, config, matches!(event, WindowEvent::Resized(_)));
    let window_id = ctx.id();
    if ctx.display.tabs.height == 0 {
        return Some(event);
    }

    if ctx.display.tabs.settings.as_mut().is_some_and(Settings::poll) {
        redraw(ctx);
    }
    let layout = Layout::new(&ctx.display.tabs);
    let tabs = &mut ctx.display.tabs;
    let modal = tabs.settings.is_some()
        || tabs.menu.is_some()
        || tabs.context.is_some()
        || tabs.rename.is_some();
    // The live terminal's pane, which pointer positions are given relative to.
    let pane = tabs.focused_rect();
    let (left, top) = (f64::from(pane.x), f64::from(pane.y));
    let over_other_pane = tabs.pane_at(tabs.pointer).is_some_and(|index| index != tabs.live().1);
    match event {
        WindowEvent::Resized(_) => {
            return Some(WindowEvent::Resized(PhysicalSize::new(pane.w as u32, pane.h as u32)));
        },
        WindowEvent::ModifiersChanged(modifiers) => tabs.modifiers = modifiers.state(),
        WindowEvent::CloseRequested => tabs.closing = true,
        WindowEvent::Ime(ime) if tabs.rename.is_some() => {
            rename::ime(ctx, ime);
            return None;
        },
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
            if tabs.rename.is_some() {
                rename::key(ctx, key, clipboard);
                return None;
            }
            if let Some(settings) = &mut tabs.settings {
                let paste = || clipboard.load(ClipboardType::Clipboard);
                let close = settings.key(key, tabs.modifiers, paste, config, &layout);
                restyle(ctx, config, proxy);
                if close {
                    close_settings(ctx);
                }
                redraw(ctx);
                return None;
            }
            if divider::key(ctx, key)
                || context::key(ctx, key, config, proxy)
                || dnd::key(ctx, key, config)
            {
                return None;
            }
            if ctx.display.tabs.menu.is_some() {
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
            if context::moved(ctx, position) {
                return None;
            }
            let tabs = &mut ctx.display.tabs;
            if tabs.drag.is_some() {
                dnd::moved(ctx, &layout, config);
                return None;
            }
            if divider::moved(ctx, position) {
                return Some(WindowEvent::CursorLeft { device_id });
            }
            let tabs = &mut ctx.display.tabs;
            let moved = |g: &PhysicalPosition<f64>| (g.x - position.x).hypot(g.y - position.y) > 4.;
            if tabs.grab.take_if(|grab| moved(grab)).is_some() {
                let _ = ctx.display.window.winit().drag_window();
                return None;
            }

            // A pane is grabbed by its handle, or anywhere with the modifiers that drag it.
            let free = tabs.press != Press::Terminal && tabs.menu.is_none();
            let handle = panes::handle_at(tabs, &layout, position).filter(|_| free);
            if tabs.handle != handle {
                tabs.handle = handle;
                redraw(ctx);
            }
            let tabs = &ctx.display.tabs;
            let over_pane =
                layout.hit(position) == Hit::Terminal && tabs.pane_at(position).is_some();
            if handle.is_some() || free && over_pane && tabs.modifiers == PANE_DRAG {
                hover(ctx, Hit::None);
                ctx.display.window.set_mouse_cursor(CursorIcon::Grab);
                return Some(WindowEvent::CursorLeft { device_id });
            }

            let tabs = &ctx.display.tabs;
            let mut hit = layout.hit(position);
            if hit == Hit::Terminal && tabs.pane_at(position) != Some(tabs.live().1) {
                // Other panes only take the focus.
                hit = Hit::None;
            }
            if tabs.press == Press::Terminal || hit == Hit::Terminal && tabs.menu.is_none() {
                hover(ctx, Hit::None);
                let position = PhysicalPosition::new(position.x - left, position.y - top);
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
        // The release that would end a drag may never come once the window loses focus.
        WindowEvent::Focused(false) => {
            context::unfocused(ctx);
            dnd::cancel(ctx, config);
            divider::release(ctx);
            rename::commit(ctx);
        },
        // Only the left button, which drags, drops.
        WindowEvent::MouseInput { state: ElementState::Released, button, .. }
            if button != MouseButton::Left
                && tabs.drag.as_ref().is_some_and(|drag| drag.moving) =>
        {
            return None;
        },
        WindowEvent::MouseInput { state: ElementState::Released, .. } => {
            if let Some(settings) = &mut tabs.settings {
                settings.release();
                redraw(ctx);
                return None;
            }
            match mem::take(&mut tabs.press) {
                Press::Bar => {
                    tabs.grab = None;
                    // A press that dragged starts no double click.
                    if tabs.drag.as_ref().is_some_and(|drag| drag.moving) {
                        tabs.last_click = None;
                    }
                    dnd::release(ctx, config);
                    hover(ctx, layout.hit(ctx.display.tabs.pointer));
                    return None;
                },
                Press::Divider => {
                    divider::release(ctx);
                    return None;
                },
                Press::Swallowed => return None,
                _ => (),
            }
        },
        // Other buttons wait for the one that drags to be released.
        WindowEvent::MouseInput { .. } if tabs.drag.as_ref().is_some_and(|drag| drag.moving) => {
            return None;
        },
        WindowEvent::MouseInput { button, .. } => {
            if rename::press(ctx) {
                return None;
            }
            let tabs = &mut ctx.display.tabs;
            let pointer = tabs.pointer;
            let wake = waker(tabs.live_id(window_id), proxy);
            if let Some(settings) = &mut tabs.settings {
                let close = settings.press(&layout, pointer, button, config, wake);
                restyle(ctx, config, proxy);
                if close {
                    close_settings(ctx);
                }
                redraw(ctx);
                return None;
            }
            if context::press(ctx, button, config, proxy) || divider::press(ctx, button) {
                return None;
            }
            let tabs = &mut ctx.display.tabs;
            let terminal = tabs.menu.is_none() && layout.hit(pointer) == Hit::Terminal;
            let dragged = terminal && tabs.modifiers == PANE_DRAG;
            let grabbed = tabs.handle.or_else(|| tabs.pane_at(pointer).filter(|_| dragged));
            if let (Some(pane), MouseButton::Left) = (grabbed, button) {
                // Like its icon in the bar, a pane is focused as it is pressed to drag it.
                tabs.press = Press::Bar;
                let active = tabs.active;
                focus_pane(ctx, active, pane, config);
                dnd::start(ctx, &layout, Source::Pane(active, pane), active);
                return None;
            }
            if terminal && over_other_pane {
                tabs.press = Press::Swallowed;
                let (active, pane) = (tabs.active, tabs.pane_at(pointer).unwrap_or_default());
                focus_pane(ctx, active, pane, config);
                return None;
            } else if terminal {
                tabs.press = Press::Terminal;
            } else {
                tabs.press = Press::Bar;
                click(ctx, &layout, button, config, proxy);
                return None;
            }
        },
        WindowEvent::MouseWheel { delta, .. } if modal || tabs.pointer.y < layout.bar as f64 => {
            let delta_kind = delta;
            let delta = match delta {
                MouseScrollDelta::LineDelta(_, delta) => f64::from(delta),
                MouseScrollDelta::PixelDelta(delta) => delta.y,
            };
            if let Some(settings) = &mut tabs.settings {
                // Touchpads scroll by pixels; the settings move by whole steps of 20.
                let lines = match delta_kind {
                    MouseScrollDelta::LineDelta(..) => delta,
                    MouseScrollDelta::PixelDelta(_) => delta / 20.,
                };
                tabs.wheel += lines;
                let steps = tabs.wheel.trunc();
                tabs.wheel -= steps;
                for _ in 0..steps.abs() as usize {
                    settings.scroll(&layout, steps < 0.);
                }
                redraw(ctx);
            } else if delta != 0. && !modal {
                run(ctx, if delta > 0. { Command::Previous } else { Command::Next }, config, proxy);
            }
            return None;
        },
        // Only the live terminal scrolls.
        WindowEvent::MouseWheel { .. } if over_other_pane => return None,
        WindowEvent::Touch(mut touch) => {
            touch.location.x -= left;
            touch.location.y -= top;
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
        Some(ctx) if ctx.display.tabs.locate(id, id).is_some() => id,
        _ => {
            *windows.iter().find(|(window, ctx)| ctx.display.tabs.locate(**window, id).is_some())?.0
        },
    };
    let ctx = windows.get_mut(&window)?;
    let (tab, index) = ctx.display.tabs.locate(window, id)?;
    let tabs = &ctx.display.tabs;

    if (tab, index) == tabs.live() {
        let event = match event {
            // The last pane, or a window being closed, closes like a window without tabs.
            TerminalEvent::Exit if tabs.count() > 1 && !tabs.closing => {
                if !ctx.display.window.hold {
                    close_pane(ctx, tab, index, config);
                }
                return None;
            },
            TerminalEvent::ResetTitle if !tabs.tabs.is_empty() => {
                TerminalEvent::Title(tabs.tabs[tab].panes[index].name.clone())
            },
            event => event,
        };
        return Some(Event::new(event.into(), window));
    }

    // Other panes only answer the terminal, keep their title and are redrawn when shown: beside
    // the live one, which they are drawn with, or in a preview.
    let shown = tab == tabs.active || peek::shows(tabs, id);
    let pane = &mut ctx.display.tabs.tabs[tab].panes[index];
    let session = pane.session.as_ref()?;
    let write = |text: String| session.notifier.notify(text.into_bytes());
    let window_config = &ctx.config.window;
    match event {
        TerminalEvent::Title(title) if !session.preserve_title && window_config.dynamic_title => {
            pane.title = title
        },
        TerminalEvent::ResetTitle if !session.preserve_title && window_config.dynamic_title => {
            pane.title = pane.name.clone()
        },
        TerminalEvent::Wakeup if shown => (),
        TerminalEvent::PtyWrite(text) => write(text),
        TerminalEvent::TextAreaSizeRequest(format) => write(format(session.size.into())),
        TerminalEvent::ColorRequest(index, format) => {
            let color = match session.terminal.lock().colors()[index] {
                Some(color) => color,
                None if index == NamedColor::Cursor as usize => return None,
                None => ctx.display.colors[index].0,
            };
            write(format(color));
        },
        TerminalEvent::Exit if !ctx.display.window.hold => close_pane(ctx, tab, index, config),
        _ => return None,
    }
    redraw(ctx);
    None
}

/// Show or hide the caption, and fit the terminals in their panels below the bar.
///
/// The window's state is only queried on `resize`, since that is slow on some platforms.
fn sync(ctx: &mut WindowContext, config: &Rc<UiConfig>, resize: bool) {
    let id = ctx.id();
    let tabs = &mut ctx.display.tabs;
    tabs.scale = ctx.display.window.scale_factor;
    if tabs.window == PhysicalSize::default() {
        tabs.window = ctx.display.window.inner_size();
    }

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
    let shown = (ctx.config.window.tabs || tabs.count() > 1) && tabs.font.is_some();

    // The window's own terminal becomes the first tab.
    if shown && tabs.tabs.is_empty() {
        tabs.host = platform::hostname().to_uppercase();
        let profile = tabs.first_profile.borrow_mut().take();
        let pane = Pane::new(id, profile.as_ref(), config, None);
        if profile.is_some() && !ctx.preserve_title {
            ctx.display.window.set_title(pane.name.clone());
        }
        ctx.display.tabs.tabs.push(Tab::new(pane));
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
    // Without its panel the terminal fills the window, its text still lined up with the logo.
    ctx.display.tabs.panel = config.window.panel;
    ctx.display.tabs.gap = 2. * MARGIN;
    let (offset, inset) = match (shown, frame) {
        (false, _) => (0, 0),
        (_, Frame::Fullscreen) => (px(BAR), 0),
        _ if !config.window.panel => (px(BAR), (MARGIN + INSET) as u8),
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
        let padded = pad(unpadded.clone(), inset);
        changed = padded.window.padding(1.) != ctx.config.window.padding(1.);
        tabs.inset = inset;
        tabs.configs = Some((unpadded, padded.clone()));
        ctx.config = padded;
        for session in tabs.sessions() {
            session.config = pane_config(config, &mut session.window_config, inset);
        }
    }

    shape(ctx, config, shown && frame == Frame::Normal);

    let size = &ctx.display.size_info;
    let cells = (size.cell_width(), size.cell_height());
    let tabs = &mut ctx.display.tabs;
    if offset != tabs.height || changed || resize || cells != tabs.cells {
        (tabs.height, tabs.cells) = (offset, cells);
        relayout(ctx);
        // The live terminal's cells move with its padding, though its size stays.
        if changed {
            resize_live(ctx);
        }
    }
}

/// Fit each pane of the active tab in its place, after they or the window changed; the panes of
/// other tabs are fitted once their tab is selected.
///
/// The live terminal is resized with the window context's other updates, the others at once.
pub fn relayout(ctx: &mut WindowContext) {
    let size = ctx.display.size_info;
    let cells = (size.cell_width(), size.cell_height());
    let tabs = &mut ctx.display.tabs;
    let (area, scale) = (tabs.area(), tabs.scale as f32);
    // A minimized window leaves no room for the panes.
    let active = tabs.active;
    if tabs.tabs.get(active).is_none() || area.w < 1. || area.h < 1. {
        return;
    }

    // Panes hidden by a zoom keep their size.
    let rects = tabs.pane_rects();
    let Some(tab) = tabs.tabs.get_mut(active) else { return };
    for &(index, rect) in &rects {
        let Some(session) = &mut tab.panes[index].session else { continue };
        let new = fit(rect, cells, &session.config, scale);
        let old = mem::replace(&mut session.size, new);
        if new.columns() != old.columns() || new.screen_lines() != old.screen_lines() {
            session.terminal.lock().resize(new);
            session.notifier.on_resize(new.into());
        }
    }
    let placed: Vec<_> = rects.iter().map(|&(index, rect)| (tab.panes[index].id, rect)).collect();
    // A dragged divider moves the panes with the pointer.
    let animate = tabs.press != Press::Divider;
    tabs.pane_effects.place(&placed, Instant::now(), animate);

    let rect = tabs.focused_rect();
    if (rect.w, rect.h) != (size.width(), size.height()) {
        resize_live(ctx);
    }
    redraw(ctx);
}

/// Size of a terminal with `config` in a pane placed at `rect`, with cells of `cells` pixels.
fn fit(rect: Rect, cells: (f32, f32), config: &UiConfig, scale: f32) -> SizeInfo {
    let (x, y) = config.window.padding(scale);
    let dynamic = config.window.dynamic_padding;
    SizeInfo::new(rect.w, rect.h, cells.0, cells.1, x, y, dynamic)
}

/// Resize the live terminal to its pane.
fn resize_live(ctx: &mut WindowContext) {
    let rect = ctx.display.tabs.focused_rect();
    let size = PhysicalSize::new(rect.w as u32, rect.h as u32);
    let window_id = ctx.id();
    ctx.event_queue.push(WinitEvent::WindowEvent { window_id, event: WindowEvent::Resized(size) });
    ctx.display.pending_update.dirty = true;
}

/// Follow the interface's corners, which a configuration reload may have changed.
///
/// Systems that round windows themselves are told which corners to give the window; elsewhere
/// the corners of a window that is not maximized are cut out, which makes it transparent there.
fn shape(ctx: &mut WindowContext, config: &UiConfig, framed: bool) {
    let corners = config.window.corners;
    let round = corners == Corners::Round;
    let cut = framed && round && cfg!(not(any(windows, target_os = "macos")));
    let tabs = &mut ctx.display.tabs;
    if tabs.corners == Some(corners) && tabs.cut == cut {
        return;
    }
    if tabs.cut != cut {
        tabs.cut = cut;
        let window = &ctx.display.window;
        window.set_transparent(cut || ctx.config.window_opacity() < 1.);
    }
    #[cfg(windows)]
    if ctx.display.tabs.corners != Some(corners) {
        platform::round(ctx.display.window.winit(), round);
    }
    ctx.display.tabs.corners = Some(corners);
    ctx.display.damage_tracker.frame().mark_fully_damaged();
    redraw(ctx);
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
                Action::TogglePaneZoom => Command::Zoom,
                _ => continue,
            };
            return Some(command).filter(|&command| applies(ctx, command));
        }
    }

    // Keys bound by the configuration keep their meaning.
    if bound || cfg!(target_os = "macos") {
        return None;
    }

    let control = ModifiersState::CONTROL;
    let shifted = ModifiersState::CONTROL | ModifiersState::SHIFT;
    let command = match (modifiers, logical_key.as_ref()) {
        (m, Key::Character("t")) if m == shifted => Command::New,
        (m, Key::Character("w")) if m == shifted => Command::Close,
        (m, Key::Character("z")) if m == shifted => Command::Zoom,
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
    };
    Some(command).filter(|&command| applies(ctx, command))
}

/// Whether `command` does something, which only a zoom may not: the key then goes to the
/// terminal.
fn applies(ctx: &WindowContext, command: Command) -> bool {
    let tabs = &ctx.display.tabs;
    let panes = tabs.tabs.get(tabs.active).map_or(0, |tab| tab.panes.len());
    !matches!(command, Command::Zoom) || panes > 1
}

fn run(
    ctx: &mut WindowContext,
    command: Command,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) {
    let (active, len) = (ctx.display.tabs.active, ctx.display.tabs.len());
    let focus = ctx.display.tabs.live().1;
    match command {
        Command::New => {
            let profiles = Profiles::load(config);
            open(ctx, config, proxy, profiles.default_profile());
        },
        Command::Next => select(ctx, (active + 1) % len, config),
        Command::Previous => select(ctx, (active + len - 1) % len, config),
        Command::Select(index) => select(ctx, index, config),
        Command::Last => select(ctx, len - 1, config),
        Command::Close => close_pane(ctx, active, focus, config),
        Command::Fullscreen => ctx.display.window.toggle_fullscreen(),
        Command::Zoom => toggle_zoom(ctx),
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

    let double = button == MouseButton::Left && double_click(&mut ctx.display.tabs, hit);
    let window = ctx.display.window.winit();
    match (hit, button) {
        (Hit::Tab(index), MouseButton::Left) if double => rename::start(ctx, index),
        (Hit::Tab(index), MouseButton::Left) => {
            let previous = ctx.display.tabs.active;
            select(ctx, index, config);
            dnd::start(ctx, layout, Source::Tab(index), previous);
        },
        (Hit::Icon(index, pane), MouseButton::Left) => {
            let previous = ctx.display.tabs.active;
            focus_pane(ctx, index, pane, config);
            dnd::start(ctx, layout, Source::Pane(index, pane), previous);
        },
        (Hit::Close(index), MouseButton::Left)
        | (Hit::Tab(index) | Hit::Icon(index, _) | Hit::Close(index), MouseButton::Middle) => {
            close(ctx, index, config)
        },
        (Hit::Tab(index) | Hit::Icon(index, _) | Hit::Close(index), MouseButton::Right) => {
            context::open_tab(ctx, index, config, proxy)
        },
        (Hit::New, MouseButton::Left) => run(ctx, Command::New, config, proxy),
        (Hit::Menu, MouseButton::Left) => {
            let profiles = Profiles::load(config).list.into_iter().filter(|p| !p.hidden).collect();
            ctx.display.tabs.menu = Some(Menu { profiles, selected: 0 });
            redraw(ctx);
        },
        (Hit::Settings, MouseButton::Left) => open_settings(ctx, config, proxy, Page::Appearance),
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
        // A second click soon after the first maximizes instead.
        (Hit::Drag, MouseButton::Left) if double => ctx.display.window.toggle_maximized(),
        (Hit::Drag, MouseButton::Left) => ctx.display.tabs.grab = Some(pointer),
        (Hit::Drag, MouseButton::Right) => window.show_window_menu(pointer),
        _ => (),
    }
}

/// Whether a press of the left button on `hit` makes a double click with the press before it.
fn double_click(tabs: &mut Tabs, hit: Hit) -> bool {
    let now = Instant::now();
    let last = tabs.last_click.replace((hit, now));
    let double =
        last.is_some_and(|(last, at)| last == hit && now - at < Duration::from_millis(500));
    if double {
        tabs.last_click = None;
    }
    double
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
        None => open_settings(ctx, config, proxy, Page::Profile),
    }
}

fn close_menu(ctx: &mut WindowContext) {
    ctx.display.tabs.menu = None;
    // Repaint the terminal the menu covered.
    ctx.display.damage_tracker.frame().mark_fully_damaged();
    redraw(ctx);
}

fn open_settings(
    ctx: &mut WindowContext,
    config: &UiConfig,
    proxy: &EventLoopProxy<Event>,
    page: Page,
) {
    let wake = waker(ctx.display.tabs.live_id(ctx.id()), proxy);
    ctx.display.tabs.settings = Some(Settings::new(config, page, wake));
    redraw(ctx);
}

/// Wake the window from another thread, through the terminal that reports events as `id`.
fn waker(id: WindowId, proxy: &EventLoopProxy<Event>) -> impl Fn() + Send + 'static {
    let proxy = proxy.clone();
    move || {
        let _ = proxy.send_event(Event::new(TerminalEvent::Wakeup.into(), id));
    }
}

/// Give the tabs opened with the profiles just saved their new look and configuration, and every
/// window the application's settings just saved.
fn restyle(ctx: &mut WindowContext, config: &Rc<UiConfig>, proxy: &EventLoopProxy<Event>) {
    // Reloading here applies them whether or not the configuration file is watched.
    if let Some(path) = ctx.display.tabs.settings.as_mut().and_then(Settings::take_reload) {
        let _ = proxy.send_event(Event::new(EventType::ConfigReload(path), None));
    }
    let saved = ctx.display.tabs.settings.as_mut().map(Settings::take_saved).unwrap_or_default();
    let mut active = false;
    let inset = ctx.display.tabs.inset;
    for pane in ctx.display.tabs.tabs.iter_mut().flat_map(|tab| &mut tab.panes) {
        let Some((_, profile)) = saved.iter().find(|(name, _)| pane.profile.as_ref() == Some(name))
        else {
            continue;
        };
        pane.profile = Some(profile.name.clone());
        pane.badge = profile.badge();
        pane.icon = profile.image(config);
        if pane.title == pane.name {
            pane.title = profile.name.clone();
        }
        pane.name = profile.name.clone();
        let mut window_config = ParsedOptions::from_options(&profile.overrides());
        match &mut pane.session {
            Some(session) => {
                session.config = pane_config(config, &mut window_config, inset);
                session.window_config = window_config;
            },
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
        relayout(ctx);
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
    let (id, session) = match Session::spawn(ctx, config, proxy, profile, None) {
        Ok(pane) => pane,
        Err(err) => return error!("Could not open tab: {err}"),
    };
    let pane = Pane::new(id, profile, config, Some(session));
    ctx.display.tabs.tabs.push(Tab::new(pane));
    select(ctx, ctx.display.tabs.tabs.len() - 1, config);
}

/// Split the live pane, putting a new one with the same profile beside it, on `side` along
/// `axis`, and focus the new pane. Returns whether there was room for it.
pub fn split(
    ctx: &mut WindowContext,
    axis: Axis,
    side: Side,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) -> bool {
    let tabs = &ctx.display.tabs;
    let Some(tab) = tabs.tabs.get(tabs.active) else { return false };
    let min = min_pane(&ctx.display.size_info);
    let (rect, spacing) = (tabs.focused_rect(), tabs.spacing());
    if !split::fits(rect, axis, side, &Split::Pane(()), min, spacing) {
        return false;
    }

    let name = tab.pane().profile.clone();
    let profiles = Profiles::load(config).list;
    let profile = profiles.into_iter().find(|profile| Some(&profile.name) == name.as_ref());
    // The new shell starts where the focused one is.
    #[cfg(not(windows))]
    let directory = crate::daemon::foreground_process_path(ctx.master_fd, ctx.shell_pid).ok();
    #[cfg(windows)]
    let directory = None;
    let (id, session) = match Session::spawn(ctx, config, proxy, profile.as_ref(), directory) {
        Ok(pane) => pane,
        Err(err) => {
            error!("Could not split pane: {err}");
            return false;
        },
    };

    let pane = Pane::new(id, profile.as_ref(), config, Some(session));
    let (active, focus) = ctx.display.tabs.live();
    let tab = &mut ctx.display.tabs.tabs[active];
    let target = tab.panes[focus].id;
    let index = tab.insert(target, pane, axis, side);
    focus_pane(ctx, active, index, config);
    true
}

/// Smallest size of a pane, in pixels, for terminals of `size`'s cells and padding.
pub fn min_pane(size: &SizeInfo) -> (f32, f32) {
    (
        split::MIN_COLUMNS * size.cell_width() + 2. * size.padding_x(),
        split::MIN_LINES * size.cell_height() + 2. * size.padding_y(),
    )
}

/// Give the pair of panes `index` of the active tab, in the order of its tree, `ratio` of their
/// space, as far as every pane keeps its smallest size.
#[allow(dead_code, reason = "dividers are dragged next")]
pub fn set_ratio(ctx: &mut WindowContext, index: usize, ratio: f32) {
    let min = min_pane(&ctx.display.size_info);
    let tabs = &mut ctx.display.tabs;
    let (area, spacing) = (tabs.area(), tabs.spacing());
    let Some(tab) = tabs.tabs.get_mut(tabs.active) else { return };
    let Some((lo, hi)) = tab.layout.ratio_range(index, area, min, spacing) else { return };
    if tab.layout.set_ratio(index, ratio.clamp(lo, hi)) {
        relayout(ctx);
    }
}

/// Show only the focused pane of the active tab over its whole area, or all its panes again.
pub fn toggle_zoom(ctx: &mut WindowContext) {
    let tabs = &mut ctx.display.tabs;
    let Some(tab) = tabs.tabs.get_mut(tabs.active).filter(|tab| tab.panes.len() > 1) else {
        return;
    };
    tab.zoom = !tab.zoom;
    ctx.display.damage_tracker.frame().mark_fully_damaged();
    relayout(ctx);
}

/// Name the tab at `tab`, or give it back its focused pane's title with none.
#[allow(dead_code, reason = "tabs are renamed next")]
pub fn rename(ctx: &mut WindowContext, tab: usize, name: Option<String>) {
    let Some(tab) = ctx.display.tabs.tabs.get_mut(tab) else { return };
    tab.name = tidy(name);
    redraw(ctx);
}

/// Make the tab at `index` the active one.
fn select(ctx: &mut WindowContext, index: usize, config: &Rc<UiConfig>) {
    let pane = ctx.display.tabs.tabs.get(index).map_or(0, |tab| tab.focus);
    focus_pane(ctx, index, pane, config);
}

/// Focus the pane `pane` of the tab `tab`, which makes the tab active.
pub fn focus_pane(ctx: &mut WindowContext, tab: usize, pane: usize, config: &Rc<UiConfig>) {
    let tabs = &mut ctx.display.tabs;
    let Some(mut session) = take(&mut tabs.tabs, (tab, pane)) else { return };
    let (old_tab, old_pane) = tabs.live();
    tabs.tabs[old_tab].panes[old_pane].title = ctx.display.window.title().to_owned();
    let focused = ctx.terminal.lock().is_focused;
    session.swap(ctx);
    let tabs = &mut ctx.display.tabs;
    relive(&mut tabs.tabs, &mut tabs.active, (tab, pane), session);
    let title = tabs.tabs[tab].panes[pane].title.clone();

    // Apply the pane's configuration overrides, then its title and the current size.
    ctx.update_config(config.clone());
    sync(ctx, config, false);
    ctx.display.window.set_title(title);
    // The terminal is fit to its own pane, which the display follows with its next update.
    let (rect, display) = (ctx.display.tabs.focused_rect(), ctx.display.size_info);
    let size =
        if rect.w < 1. || rect.h < 1. || (rect.w, rect.h) == (display.width(), display.height()) {
            display
        } else {
            let cells = (display.cell_width(), display.cell_height());
            fit(rect, cells, &ctx.config, ctx.display.tabs.scale as f32)
        };
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
    relayout(ctx);
    // Input reads the terminal through the display's size before the queued resize reaches it.
    // Only after `relayout`, which queues that resize by comparing with the old size.
    if size != ctx.display.size_info {
        ctx.display.size_info = size;
        ctx.display.damage_tracker.resize(size.screen_lines(), size.columns());
    }
}

/// Move the pane `from`, a tab and a pane index, to `target`, and focus it there.
pub fn move_pane(
    ctx: &mut WindowContext,
    (tab, pane): (usize, usize),
    target: Target,
    config: &Rc<UiConfig>,
) {
    // Docking refers to the panes shown before the moving one is focused, and so do the
    // animations: those panes slide aside and only the moving one pops in.
    let shown = ctx.display.tabs.active;
    let effects = mem::take(&mut ctx.display.tabs.pane_effects);
    focus_pane(ctx, tab, pane, config);
    ctx.display.tabs.pane_effects = effects;
    let tabs = &mut ctx.display.tabs;
    let area = tabs.area();
    if tabs.tabs.is_empty() || tabs.live() != (tab, pane) {
        return;
    }
    if place(&mut tabs.tabs, &mut tabs.active, shown, target, area) {
        relayout(ctx);
    }
}

/// Group the tab `from` with the tab `into`: its panes join the others, beside the pane of `dock`
/// or else beside them all, keeping the focus.
pub fn merge(
    ctx: &mut WindowContext,
    from: usize,
    into: usize,
    dock: Option<(usize, Axis, Side)>,
    config: &Rc<UiConfig>,
) {
    // The panes on screen slide aside and only the arriving ones pop in.
    let effects = mem::take(&mut ctx.display.tabs.pane_effects);
    select(ctx, from, config);
    ctx.display.tabs.pane_effects = effects;
    let tabs = &mut ctx.display.tabs;
    let area = tabs.area();
    if tabs.active != from || from >= tabs.tabs.len() {
        return;
    }
    if group(&mut tabs.tabs, &mut tabs.active, into, dock, area) {
        relayout(ctx);
    }
}

/// Close the pane `pane` of the tab `tab`, or the whole tab when it is its only pane.
pub fn close_pane(ctx: &mut WindowContext, tab: usize, pane: usize, config: &Rc<UiConfig>) {
    // A drag refers to tabs and panes by their place, which closing changes.
    ctx.display.tabs.drag = None;
    let tabs = &ctx.display.tabs;
    if tabs.tabs.get(tab).is_none_or(|t| t.panes.len() < 2) {
        return close(ctx, tab, config);
    }
    // The live pane passes the focus to its neighbour first.
    if (tab, pane) == tabs.live() {
        let next = tabs.tabs[tab].neighbour(pane).unwrap_or_default();
        focus_pane(ctx, tab, next, config);
    }
    drop(ctx.display.tabs.tabs[tab].remove(pane));
    relayout(ctx);
}

/// Close the tab at `index`, or the window with its last tab or an index past the tabs.
fn close(ctx: &mut WindowContext, index: usize, config: &Rc<UiConfig>) {
    ctx.display.tabs.drag = None;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Tabs whose panes keep their own id aside, so it is easy to see where each terminal is.
    struct Model {
        tabs: Vec<Tab<u64>>,
        active: usize,
        /// The live terminal.
        live: u64,
    }

    fn pane(id: u64, session: Option<u64>) -> Pane<u64> {
        let (name, title, badge) = (String::new(), String::new(), String::new());
        Pane { id: WindowId::from(id), name, title, badge, icon: None, profile: None, session }
    }

    impl Model {
        /// Tabs of one pane each, with the first active.
        fn new(count: u64) -> Self {
            let tabs = (1..=count).map(|id| Tab::new(pane(id, (id > 1).then_some(id)))).collect();
            Self { tabs, active: 0, live: 1 }
        }

        fn focus(&mut self, to: (usize, usize)) {
            if let Some(mut session) = take(&mut self.tabs, to) {
                mem::swap(&mut session, &mut self.live);
                relive(&mut self.tabs, &mut self.active, to, session);
            }
            self.check();
        }

        fn split(&mut self, id: u64, axis: Axis, side: Side) {
            let tab = &mut self.tabs[self.active];
            let target = tab.pane().id;
            let index = tab.insert(target, pane(id, Some(id)), axis, side);
            let active = self.active;
            self.focus((active, index));
        }

        /// Close a pane of a tab that has others, like `close_pane`.
        fn close(&mut self, (tab, index): (usize, usize)) {
            if (tab, index) == (self.active, self.tabs[tab].focus) {
                let next = self.tabs[tab].neighbour(index).unwrap();
                self.focus((tab, next));
            }
            let pane = self.tabs[tab].remove(index);
            assert!(pane.session.is_some());
            self.check();
        }

        /// Move a pane like `move_pane`.
        fn place(&mut self, from: (usize, usize), target: Target) -> bool {
            let shown = self.active;
            self.focus(from);
            let area = Rect::new(0., 40., 1000., 600.);
            let placed = place(&mut self.tabs, &mut self.active, shown, target, area);
            self.check();
            placed
        }

        fn ids(&self, tab: usize) -> Vec<u64> {
            self.tabs[tab].panes.iter().map(|pane| u64::from(pane.id)).collect()
        }

        /// Exactly one pane, the focused one of the active tab, has no terminal aside, because
        /// its terminal is live; every other pane keeps its own.
        fn check(&self) {
            let mut missing = Vec::new();
            for (t, tab) in self.tabs.iter().enumerate() {
                assert_eq!(
                    tab.panes.iter().map(|pane| pane.id).collect::<Vec<_>>(),
                    tab.layout.panes()
                );
                assert!(tab.focus < tab.panes.len());
                for (p, pane) in tab.panes.iter().enumerate() {
                    match pane.session {
                        Some(session) => assert_eq!(session, u64::from(pane.id)),
                        None => missing.push((t, p)),
                    }
                }
            }
            assert_eq!(missing, [(self.active, self.tabs[self.active].focus)]);
            assert_eq!(self.live, u64::from(self.tabs[self.active].pane().id));
        }
    }

    #[test]
    fn one_live_terminal_across_focus_and_close() {
        let mut model = Model::new(2);
        model.check();

        // Three panes: 1 beside 3, which is above 4.
        model.split(3, Axis::Row, Side::After);
        model.split(4, Axis::Column, Side::After);
        assert_eq!(model.ids(0), [1, 3, 4]);
        assert_eq!(model.live, 4);

        model.focus((0, 0));
        model.focus((1, 0));
        assert_eq!(model.live, 2);
        model.focus((0, 1));
        model.focus((0, 1));
        assert_eq!(model.live, 3);

        // Closing the live pane focuses its neighbour; closing another keeps the focus.
        model.close((0, 1));
        assert_eq!((model.ids(0), model.live), (vec![1, 4], 4));
        model.split(5, Axis::Row, Side::Before);
        assert_eq!(model.ids(0), [1, 5, 4]);
        model.close((0, 0));
        assert_eq!((model.ids(0), model.live), (vec![5, 4], 5));
        model.focus((1, 0));
        model.close((0, 1));
        assert_eq!((model.ids(0), model.live), (vec![5], 2));
    }

    #[test]
    fn one_live_terminal_across_moves() {
        let mut model = Model::new(3);
        model.focus((1, 0));
        model.split(4, Axis::Row, Side::After);
        model.split(5, Axis::Column, Side::After);
        assert_eq!((model.ids(1), model.live), (vec![2, 4, 5], 5));

        // Dragging a pane out makes it a tab of its own, then back in groups it again.
        assert!(model.place((1, 1), Target::Detach(0)));
        assert_eq!((model.ids(0), model.ids(2), model.live), (vec![4], vec![2, 5], 4));
        assert!(model.place((0, 0), Target::Merge(2)));
        assert_eq!((model.active, model.ids(1), model.live), (1, vec![2, 5, 4], 4));

        // Docking beside a pane of the shown tab, from another tab whose only pane it was.
        assert!(model.place((0, 0), Target::Dock {
            pane: 0,
            axis: Axis::Column,
            side: Side::Before
        }));
        assert_eq!((model.tabs.len(), model.active, model.ids(0)), (2, 0, vec![1, 2, 5, 4]));

        // Nothing moves onto itself.
        assert!(!model.place((0, 0), Target::Dock { pane: 0, axis: Axis::Row, side: Side::After }));
        assert!(!model.place((0, 0), Target::Merge(0)));

        // Reordering a tab of one pane keeps it whole.
        assert!(model.place((1, 0), Target::Reorder(0)));
        assert_eq!((model.ids(0), model.ids(1), model.active), (vec![3], vec![1, 2, 5, 4], 0));

        // A whole tab moves to another place, or the end when past it.
        reorder(&mut model.tabs, &mut model.active, 1);
        assert_eq!((model.active, model.ids(0), model.ids(1)), (1, vec![1, 2, 5, 4], vec![3]));
        reorder(&mut model.tabs, &mut model.active, 5);
        assert_eq!(model.active, 1);
        reorder(&mut model.tabs, &mut model.active, 0);
        model.check();

        // Grouping whole tabs keeps the focus of the tab that moved.
        let area = Rect::new(0., 0., 800., 600.);
        model.tabs.push(Tab::new(pane(7, Some(7))));
        assert!(group(&mut model.tabs, &mut model.active, 2, None, area));
        assert_eq!((model.tabs.len(), model.ids(1), model.live), (2, vec![7, 3], 3));
        model.check();

        // A whole group docks as it is beside one pane of another, keeping the focus it had.
        model.split(6, Axis::Column, Side::After);
        let dock = |pane| Some((pane, Axis::Row, Side::After));
        assert!(!group(&mut model.tabs, &mut model.active, 0, dock(9), area));
        assert!(group(&mut model.tabs, &mut model.active, 0, dock(0), area));
        assert_eq!((model.tabs.len(), model.ids(0), model.live), (1, vec![1, 7, 3, 6, 2, 5, 4], 6));
        model.check();
    }

    #[test]
    fn changing_panes_unzooms() {
        let mut model = Model::new(2);
        model.split(3, Axis::Row, Side::After);
        let zoom = |model: &mut Model| model.tabs[0].zoom = true;

        zoom(&mut model);
        model.split(4, Axis::Column, Side::After);
        assert!(!model.tabs[0].zoom, "insert");

        zoom(&mut model);
        model.close((0, 0));
        assert!(!model.tabs[0].zoom, "remove");

        zoom(&mut model);
        let area = Rect::new(0., 0., 800., 600.);
        model.focus((1, 0));
        assert!(group(&mut model.tabs, &mut model.active, 0, None, area));
        assert!(!model.tabs[0].zoom, "join");

        // Focusing another pane keeps the zoom, which then shows that pane.
        zoom(&mut model);
        model.focus((0, 0));
        assert!(model.tabs[0].zoom);
    }

    #[test]
    fn names_are_trimmed() {
        assert_eq!(tidy(Some("  Build  ".into())), Some("Build".into()));
        assert_eq!(tidy(Some(" \t ".into())), None);
        assert_eq!(tidy(Some(String::new())), None);
        assert_eq!(tidy(None), None);
    }

    #[test]
    fn tearing_never_empties_the_window() {
        use tear::tearable;
        let mut model = Model::new(1);
        // The only pane of the only tab.
        assert!(!tearable(&model.tabs, Source::Tab(0)));
        assert!(!tearable(&model.tabs, Source::Pane(0, 0)));
        // A pane of a group, but not the group, which is the only tab.
        model.split(2, Axis::Row, Side::After);
        assert!(tearable(&model.tabs, Source::Pane(0, 1)));
        assert!(!tearable(&model.tabs, Source::Pane(0, 2)));
        assert!(!tearable(&model.tabs, Source::Tab(0)));
        // With another tab, any tab, or a tab's only pane.
        model.tabs.push(Tab::new(pane(3, Some(3))));
        assert!(tearable(&model.tabs, Source::Tab(0)));
        assert!(tearable(&model.tabs, Source::Tab(1)));
        assert!(tearable(&model.tabs, Source::Pane(1, 0)));
        assert!(!tearable(&model.tabs, Source::Tab(2)));
    }

    #[test]
    fn pane_at_prefers_nearest_card() {
        let at = |x: f64, y: f64| PhysicalPosition::new(x, y);
        // Places overlapping by 12 px, their cards 2 px apart with a margin of 8.
        let rects = [(0, Rect::new(0., 0., 106., 100.)), (1, Rect::new(94., 0., 106., 100.))];
        assert_eq!(nearest(&rects, at(50., 50.), 8.), Some(0));
        assert_eq!(nearest(&rects, at(97., 50.), 8.), Some(0));
        assert_eq!(nearest(&rects, at(98.5, 50.), 8.), Some(0));
        assert_eq!(nearest(&rects, at(101., 50.), 8.), Some(1));
        assert_eq!(nearest(&rects, at(105., 50.), 8.), Some(1));
        assert_eq!(nearest(&rects, at(250., 50.), 8.), None);
        // Without overlap, the place at the point.
        let rects = [(0, Rect::new(0., 0., 100., 100.)), (1, Rect::new(100., 0., 100., 100.))];
        assert_eq!(nearest(&rects, at(99., 2.), 8.), Some(0));
        assert_eq!(nearest(&rects, at(100., 2.), 8.), Some(1));
    }
}

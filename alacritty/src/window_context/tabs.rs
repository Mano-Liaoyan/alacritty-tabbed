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
mod autohide;
mod bar;
mod confirm;
mod context;
mod cwd;
mod divider;
mod dnd;
mod fonts;
mod frame;
mod fullscreen;
pub mod hooks;
mod icons;
mod job;
mod paint;
mod panes;
mod peek;
mod profiles;
mod rename;
pub mod restore;
mod session;
mod settings;
mod split;
mod tear;
mod text;
mod text_size;

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
use std::hash::BuildHasher;
use std::mem;
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
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{ClipboardType, Term};
use alacritty_terminal::tty::Options as PtyOptions;
use alacritty_terminal::vte::ansi::NamedColor;

use crate::cli::{ParsedOptions, WindowOptions};
use crate::clipboard::Clipboard;
use crate::config::font::Font;
use crate::config::tabbed::{Corners, MAX_PANE_GAP, UiFont as UiFontConfig};
use crate::config::{Action, BindingKey, BindingMode, UiConfig};
use crate::display::SizeInfo;
use crate::event::{Event, EventProxy, EventType};
use crate::window_context::WindowContext;

use app_settings::AppSettings;
use bar::{BAR, Hit, INSET, Layout, MARGIN};
use dnd::{Drag, Source, Target};
pub use frame::Frame;
use frame::{Restore, frame, restore};
pub use fullscreen::FullscreenState;
use paint::Image;
pub use paint::Rect;
use panes::Control;
use profiles::{Profile, Profiles};
use session::Session;
use settings::{Page, Settings};
use split::{Axis, Divider, Side, Split};
pub use tear::{Live, Torn, adopt, take as take_torn};
use text::UiFont;
pub use text_size::{FontChange, set_live_font, zoom_live};
use text_size::{ZOOM_MIN_POINTS, ZOOM_RANGE, ZOOM_WHEEL, note_font, wheel_steps};

/// Ids for the terminals of new tabs, far from the platform's window ids.
static NEXT_ID: AtomicU64 = AtomicU64::new(1 << 62);

/// A new id for a terminal to report its events with: never a window's, which the system may
/// give the next window once its own is closed, while the terminal lives on in another.
pub fn next_id() -> WindowId {
    WindowId::from(NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

/// Modifiers that drag a pane from anywhere in it, exactly; Control alone selects a block.
#[cfg(not(target_os = "macos"))]
const PANE_DRAG: ModifiersState = ModifiersState::CONTROL.union(ModifiersState::ALT);
#[cfg(target_os = "macos")]
const PANE_DRAG: ModifiersState = ModifiersState::SUPER.union(ModifiersState::ALT);

/// How a window's terminals are coming to an end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The window was closed as a window, by the system or its close button.
    Window,
    /// Its last tab was closed.
    Tabs,
}

/// Tabs of one window.
#[derive(Default)]
pub struct Tabs {
    /// Pixels above the terminal's area; zero without tabs, or with a bar that is not pinned.
    pub height: u32,
    /// Present the whole window this frame and the next, since the terminal's damage alone does
    /// not cover it; set while drawing into frames, reset once presented.
    pub full_damage: bool,
    /// Whether the bar is shown, or slides in when it is not pinned.
    caption: bool,
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
    /// Where the window was in the normal frame, and its size there: before the last move or
    /// resize, and after it, as a window manager may move it to the screen's corner just before
    /// it makes it fullscreen. Once out of the normal frame, the first is where it was.
    placed: [Option<(PhysicalPosition<i32>, PhysicalSize<u32>)>; 2],
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
    /// The open popup asking whether to close a pane running a job.
    confirm: Option<confirm::Dialog>,
    settings: Option<Settings>,
    pane_effects: panes::Effects,
    bar_effects: bar::Effects,
    /// Space between the cards of a tab's panes, in logical pixels.
    gap: f32,
    /// The control of a pane under the pointer, by the pane's index.
    control: Option<(usize, Control)>,
    /// The pane whose controls the pointer is near, which shows them.
    near: Option<usize>,
    divider: divider::State,
    /// The tab whose name is being edited.
    rename: Option<rename::Edit>,
    /// The hovered icon's card.
    peeking: peek::State,
    /// A tab taken out of the window, waiting for a window of its own.
    torn: Option<Torn>,
    /// How the window is ending, which closes it instead of a tab once the live terminal exits.
    ending: Option<Ending>,
    /// Whether the bar is shown when it is not pinned.
    autohide: autohide::State,
    /// The live pane whose text size last changed, the size relative to its configuration in
    /// percent, and when, for the size to show over it.
    font_hud: Option<(WindowId, u16, Instant)>,
    /// The live pane and its text size relative to its configuration, as last seen.
    live_font: Option<(WindowId, u16)>,
    /// Wheel movement with the zoom modifiers short of a whole step, in steps.
    zoom_wheel: f64,
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
    pub clip: Rect,
    /// The pane's font, at its own size.
    pub font: Font,
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
    /// On the close button of the tab at this index, which closes it if released there.
    Close(usize),
    /// On the zoom or close control of the pane at this index, which acts if released there.
    Control(usize, Control),
    /// Over something that used the press, such as a pane it focused, so the terminal gets
    /// neither it nor its release.
    Swallowed,
}

/// The open new-tab menu: profiles, then an entry to edit them.
struct Menu {
    profiles: Vec<Profile>,
    selected: usize,
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

    /// The tab and the pane whose terminal reports events as `id`, in a window whose first
    /// terminal reports them as `first`.
    fn locate(&self, first: WindowId, id: WindowId) -> Option<(usize, usize)> {
        if self.tabs.is_empty() {
            return (id == first).then_some((0, 0));
        }
        self.tabs.iter().enumerate().find_map(|(index, tab)| Some((index, tab.position(id)?)))
    }

    /// The tab and the pane whose terminal is live.
    fn live(&self) -> (usize, usize) {
        (self.active, self.tabs.get(self.active).map_or(0, |tab| tab.focus))
    }

    /// Id of the live terminal, in a window whose first terminal has the id `first`.
    fn live_id(&self, first: WindowId) -> WindowId {
        self.tabs.get(self.active).map_or(first, |tab| tab.pane().id)
    }

    /// The window below the bar, which the active tab's panes share.
    fn area(&self) -> Rect {
        let (width, height) = (self.window.width as f32, self.window.height as f32);
        let top = (self.height as f32).min(height);
        Rect::new(0., top, width, height - top)
    }

    /// Pixels between the places of neighbouring panes, beyond the margins of their cards; it
    /// may be negative, where the margins overlap, though never so much that the cards do.
    fn spacing(&self) -> f32 {
        if !self.panel {
            return 0.;
        }
        ((self.gap - 2. * MARGIN) * self.scale as f32).round().max(-2. * self.margin())
    }

    /// Whether the modifiers that drag a pane are held, where there is anything to drag it to.
    fn pane_drag(&self) -> bool {
        self.modifiers == PANE_DRAG && self.count() > 1
    }

    /// The card a pane placed at `rect` is drawn in, inside its margin.
    pub fn card(&self, rect: Rect) -> Rect {
        rect.inset(self.margin())
    }

    /// Margin around each pane's card, in pixels.
    fn margin(&self) -> f32 {
        if self.panel { (MARGIN * self.scale as f32).round() } else { 0. }
    }

    /// Whether the window was closed as a window, rather than by closing its tabs.
    pub fn closed_whole(&self) -> bool {
        self.ending == Some(Ending::Window)
    }

    /// Where each shown pane of the active tab is placed, by its index: only the focused one
    /// while the tab is zoomed.
    pub fn pane_rects(&self) -> Vec<(usize, Rect)> {
        match self.tabs.get(self.active) {
            Some(tab) if tab.zoom => vec![(tab.focus, self.area())],
            _ => self.placed(),
        }
    }

    /// Where each pane of the active tab is placed by its layout, by its index, zoomed or not:
    /// where a split or dock would cut it.
    fn placed(&self) -> Vec<(usize, Rect)> {
        let Some(tab) = self.tabs.get(self.active) else { return Vec::new() };
        let rects = tab.layout.rects(self.area(), self.spacing()).into_iter();
        rects.enumerate().map(|(index, (_, rect))| (index, rect)).collect()
    }

    /// The dividers between the active tab's panes; none while it is zoomed.
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
    pub fn focused_clip(&self, now: Instant) -> Rect {
        self.card(self.focused_frame(now))
    }

    /// The shown panes of the active tab other than the live one, and where to draw them at
    /// `now`: first those no longer shown that stay under the others until they are covered.
    pub fn background_panes(&self, now: Instant) -> Vec<PaneDraw> {
        let Some(tab) = self.tabs.get(self.active) else { return Vec::new() };
        let leaving =
            self.pane_effects.leaving(now).into_iter().filter_map(|(id, frame)| {
                Some((tab.panes.iter().find(|pane| pane.id == id)?, frame))
            });
        let shown = self.pane_rects().into_iter().map(|(index, rect)| {
            let pane = &tab.panes[index];
            (pane, self.pane_effects.frame(pane.id, rect, now))
        });
        leaving
            .chain(shown)
            .filter_map(|(pane, frame)| {
                let session = pane.session.as_ref()?;
                Some(PaneDraw {
                    terminal: session.terminal.clone(),
                    config: session.config.clone(),
                    size: session.size,
                    frame,
                    clip: self.card(frame),
                    font: session.font(self.scale as f32),
                })
            })
            .collect()
    }

    /// Whether anything is still moving at `now`, which needs another frame.
    pub fn animating(&self, now: Instant) -> bool {
        self.pane_effects.running(now)
            || self.bar_effects.running(now)
            || self.context.as_ref().is_some_and(|menu| menu.running(now))
            || self.confirm.as_ref().is_some_and(|dialog| dialog.running(now))
            || self.divider.running(now)
            || self.peeking.running(now)
            || self.autohide.running(now)
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
fn tidy(name: Option<String>) -> Option<String> {
    let name = name?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Move the live pane to `target`, where it stays live; `Dock` refers to the panes of the tab
/// `shown`, and a `Merge` finds it a place where each pane of the tab keeps `min` in `area`, as
/// `join_spot` does. Returns whether it moved.
fn place<S>(
    tabs: &mut Vec<Tab<S>>,
    active: &mut usize,
    shown: usize,
    target: Target,
    (area, min, spacing): (Rect, (f32, f32), f32),
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
            let (beside, axis) = join_spot(&tabs[tab].layout, &Split::Pane(id), area, min, spacing);
            tabs[tab].join(Tab::new(moved), beside, axis, Side::After);
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

/// Where `new` joins the panes of `group` in `area`, after the pane or all of them, each keeping
/// `min`, `spacing` apart, as [`Split::spot`] finds; beside them all when it fits nowhere, as
/// before there was a check.
fn join_spot(
    group: &Split<WindowId>,
    new: &Split<WindowId>,
    area: Rect,
    min: (f32, f32),
    spacing: f32,
) -> (Option<WindowId>, Axis) {
    group.spot(new, area, min, spacing).unwrap_or((None, split::auto_axis(area)))
}

/// Group the active tab with the tab `into`, which takes its place and its focus: docked beside
/// the pane of `dock`, or else where `join_spot` finds room in `area`. Returns whether they were
/// grouped.
fn group<S>(
    tabs: &mut Vec<Tab<S>>,
    active: &mut usize,
    into: usize,
    dock: Option<(usize, Axis, Side)>,
    (area, min, spacing): (Rect, (f32, f32), f32),
) -> bool {
    let from = *active;
    if into == from || into >= tabs.len() {
        return false;
    }
    let (beside, axis, side) = match dock {
        Some((pane, axis, side)) => match tabs[into].panes.get(pane) {
            Some(pane) => (Some(pane.id), axis, side),
            None => return false,
        },
        None => {
            let (beside, axis) =
                join_spot(&tabs[into].layout, &tabs[from].layout, area, min, spacing);
            (beside, axis, Side::After)
        },
    };
    let tab = tabs.remove(from);
    let into = if into > from { into - 1 } else { into };
    let focus = tab.pane().id;
    tabs[into].join(tab, beside, axis, side);
    tabs[into].focus = tabs[into].position(focus).unwrap_or_default();
    *active = into;
    true
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

/// Let the bar replace the system's caption.
pub fn attributes(attributes: WindowAttributes, config: &UiConfig) -> WindowAttributes {
    if config.tabbed.tabs { platform::attributes(attributes) } else { attributes }
}

/// Start a window's first terminal with the default profile, unless a command was given.
pub fn launch(config: &UiConfig, options: &WindowOptions, pty: &mut PtyOptions, tabs: &Tabs) {
    if !config.tabbed.tabs || options.terminal_options.command().is_some() {
        return;
    }
    let Some(profile) = Profiles::load(config).default_profile().cloned() else { return };
    profile.launch(pty, options.terminal_options.working_directory.is_some());
    *tabs.first_profile.borrow_mut() = Some(profile);
}

/// Pad the terminals again right after a new configuration replaced the padded one.
pub fn reconfigure(ctx: &mut WindowContext, config: &Rc<UiConfig>) {
    sync(ctx, config, false);
}

/// Handle a window event for the caption, returning the event the terminal should get.
pub fn window_event(
    ctx: &mut WindowContext,
    event: WindowEvent,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
    clipboard: &mut Clipboard,
) -> Option<WindowEvent> {
    if let WindowEvent::RedrawRequested = event {
        fullscreen::before_draw(ctx, config, proxy, clipboard);
    }
    if let WindowEvent::Resized(size) = event {
        // Minimizing reports an empty size, which the terminal ignores too.
        if size.width == 0 || size.height == 0 {
            return Some(event);
        }
        ctx.display.tabs.window = size;
    }
    if let WindowEvent::CloseRequested = event {
        ctx.display.tabs.ending = Some(Ending::Window);
    }
    // Only for window managers that make the window fill its screen without resizing it, as by
    // moving it there.
    let resize = match event {
        WindowEvent::Resized(_) => true,
        WindowEvent::Moved(_) | WindowEvent::ScaleFactorChanged { .. } => {
            frame(ctx.display.window.winit()) != ctx.display.tabs.frame
        },
        _ => false,
    };
    sync(ctx, config, resize);
    if matches!(event, WindowEvent::Resized(_) | WindowEvent::Moved(_)) {
        let window = ctx.display.window.winit();
        let (tabs, here) = (&mut ctx.display.tabs, window.outer_position().ok());
        tabs.placed = match (tabs.frame, tabs.placed[1]) {
            (Frame::Normal, _) => [tabs.placed[1], here.map(|at| (at, window.inner_size()))],
            // Out of it, the last place, unless the window manager just moved it to where it is.
            (_, Some(last)) if Some(last.0) != here => [Some(last), None],
            _ => [tabs.placed[0], None],
        };
    }
    // Only the caption follows the window's place.
    if let WindowEvent::Moved(_) = event {
        return None;
    }
    let first = ctx.first;
    if !ctx.display.tabs.caption {
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
        || tabs.confirm.is_some()
        || tabs.rename.is_some();
    // The live terminal's pane, which pointer positions are given relative to.
    let pane = tabs.focused_rect();
    let (left, top) = (f64::from(pane.x), f64::from(pane.y));
    let over_other_pane = tabs.pane_at(tabs.pointer).is_some_and(|index| index != tabs.live().1);
    match event {
        WindowEvent::Resized(_) => {
            return Some(WindowEvent::Resized(PhysicalSize::new(pane.w as u32, pane.h as u32)));
        },
        // Holding the modifiers that drag a pane shows what they would drag.
        WindowEvent::ModifiersChanged(modifiers) => {
            let held = tabs.pane_drag();
            tabs.modifiers = modifiers.state();
            if tabs.pane_drag() != held {
                redraw(ctx);
            }
        },
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
                || confirm::key(ctx, key, config)
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
            if autohide::moved(tabs, position, modal, Instant::now()) {
                redraw(ctx);
            }
            let tabs = &mut ctx.display.tabs;
            if let Some(settings) = &mut tabs.settings {
                if settings.moved(&layout, position) {
                    redraw(ctx);
                }
                ctx.display.window.set_mouse_cursor(CursorIcon::Default);
                return None;
            }
            if confirm::moved(ctx, position) || context::moved(ctx, position) {
                return None;
            }
            let tabs = &mut ctx.display.tabs;
            if tabs.drag.is_some() {
                dnd::moved(ctx, &layout, config);
                return None;
            }
            // Panes take the pointer only where nothing above them, nor a selection, has it; a
            // fanned shelf covers the panes' controls under it.
            let free = tabs.press != Press::Terminal && tabs.menu.is_none();
            let under = layout.hit(position);
            let over_panes = free && under == Hit::Terminal;
            let shelf = matches!(under, Hit::Icon(..));
            let near = panes::near(tabs, position).filter(|_| free && !shelf);
            if mem::replace(&mut tabs.near, near) != near {
                redraw(ctx);
            }
            let tabs = &mut ctx.display.tabs;
            if (tabs.press == Press::Divider || over_panes) && divider::moved(ctx, position) {
                return Some(WindowEvent::CursorLeft { device_id });
            }
            let tabs = &mut ctx.display.tabs;
            let moved = |g: &PhysicalPosition<f64>| (g.x - position.x).hypot(g.y - position.y) > 4.;
            if tabs.grab.take_if(|grab| moved(grab)).is_some() {
                let _ = ctx.display.window.winit().drag_window();
                return None;
            }

            // A pane is grabbed by its grip, or anywhere with the modifiers that drag it. The
            // window's resize borders keep the pointer over the controls near them.
            let resize = matches!(under, Hit::Resize(_));
            let control =
                panes::control(tabs, position, layout.scale).filter(|_| free && !shelf && !resize);
            if mem::replace(&mut tabs.control, control) != control {
                redraw(ctx);
            }
            let tabs = &ctx.display.tabs;
            let over_pane = over_panes && tabs.pane_at(position).is_some();
            if control.is_some() || over_pane && tabs.pane_drag() {
                hover(ctx, Hit::None);
                let cursor = match control {
                    Some((_, Control::Zoom | Control::Close)) => CursorIcon::Pointer,
                    _ => CursorIcon::Grab,
                };
                ctx.display.window.set_mouse_cursor(cursor);
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
        WindowEvent::CursorLeft { .. } => {
            // The hovered control or divider is no longer lit, nor the bar shown.
            let shown = tabs.control.take().is_some() | tabs.near.take().is_some();
            if autohide::left(tabs, Instant::now()) | shown {
                redraw(ctx);
            }
            divider::left(ctx);
            hover(ctx, Hit::None);
        },
        // The release that would end a drag may never come once the window loses focus.
        WindowEvent::Focused(false) => {
            context::unfocused(ctx);
            if let Some(settings) = &mut ctx.display.tabs.settings {
                settings.release();
            }
            dnd::cancel(ctx, config);
            divider::release(ctx);
            if ctx.display.tabs.press == Press::Divider {
                ctx.display.tabs.press = Press::None;
            }
            rename::commit(ctx);
        },
        // Other buttons wait for the one that drags a divider to be released.
        WindowEvent::MouseInput { button, .. } if divider::holds(tabs, button) => return None,
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
                Press::Close(index) => {
                    if layout.hit(tabs.pointer) == Hit::Close(index) {
                        close(ctx, index, config);
                    }
                    return None;
                },
                // Like native buttons, the zoom and close controls act on release.
                Press::Control(index, control) => {
                    let active = tabs.active;
                    if panes::control(tabs, tabs.pointer, layout.scale) == Some((index, control)) {
                        match control {
                            Control::Zoom => {
                                focus_pane(ctx, active, index, config);
                                toggle_zoom(ctx);
                            },
                            Control::Close => confirm::request(ctx, active, index, config),
                            Control::Grip => (),
                        }
                    }
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
            let wake = waker(tabs.live_id(first), proxy);
            if let Some(settings) = &mut tabs.settings {
                let close = settings.press(&layout, pointer, button, config, wake);
                restyle(ctx, config, proxy);
                if close {
                    close_settings(ctx);
                }
                redraw(ctx);
                return None;
            }
            if confirm::press(ctx, button, config) || context::press(ctx, button, config, proxy) {
                return None;
            }
            let tabs = &mut ctx.display.tabs;
            let terminal = tabs.menu.is_none() && layout.hit(pointer) == Hit::Terminal;
            if terminal && divider::press(ctx, button) {
                return None;
            }
            let tabs = &mut ctx.display.tabs;
            match (tabs.control, button) {
                (Some((index, control @ (Control::Zoom | Control::Close))), MouseButton::Left) => {
                    tabs.press = Press::Control(index, control);
                    return None;
                },
                // Other buttons on a button do nothing, rather than reach the terminal under it.
                (Some((_, Control::Zoom | Control::Close)), _) => {
                    tabs.press = Press::Swallowed;
                    return None;
                },
                _ => (),
            }
            let dragged = terminal && tabs.pane_drag();
            let grip = tabs.control.filter(|&(_, c)| c == Control::Grip).map(|(index, _)| index);
            let grabbed = grip.or_else(|| tabs.pane_at(pointer).filter(|_| dragged));
            if let (Some(pane), MouseButton::Left) = (grabbed, button) {
                // Like its icon in the bar, a pane is focused as it is pressed to drag it.
                tabs.press = Press::Bar;
                let active = tabs.active;
                focus_pane(ctx, active, pane, config);
                dnd::start(ctx, &layout, Source::Pane(active, pane), active);
                return None;
            }
            // The space between panes belongs to none of them.
            if terminal && tabs.pane_at(pointer).is_none() {
                tabs.press = Press::Swallowed;
                return None;
            }
            if terminal && over_other_pane {
                tabs.press = Press::Swallowed;
                let (active, pane) = (tabs.active, tabs.pane_at(pointer).unwrap_or_default());
                focus_pane(ctx, active, pane, config);
                return None;
            } else if terminal {
                tabs.press = Press::Terminal;
            } else if let (Hit::Close(index), MouseButton::Left) = (layout.hit(pointer), button) {
                // Like a native button, × acts on release, so a press there that moves away
                // closes nothing.
                tabs.press = Press::Close(index);
                tabs.last_click = None;
                return None;
            } else {
                tabs.press = Press::Bar;
                click(ctx, &layout, button, config, proxy);
                return None;
            }
        },
        // The wheel with the zoom modifiers sizes the text of the live pane, under the pointer.
        WindowEvent::MouseWheel { delta, .. }
            if !modal
                && config.tabbed.wheel_zoom
                && tabs.modifiers == ZOOM_WHEEL
                && layout.hit(tabs.pointer) == Hit::Terminal
                && tabs.pane_at(tabs.pointer) == Some(tabs.live().1) =>
        {
            let steps = wheel_steps(delta, tabs.scale, &mut tabs.zoom_wheel);
            if steps != 0 {
                zoom_live(ctx, FontChange::By(steps));
            }
            return None;
        },
        WindowEvent::MouseWheel { delta, .. }
            if modal || layout.bar_live && tabs.pointer.y < layout.bar as f64 =>
        {
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
        // Only the live terminal scrolls, and not under a fanned shelf.
        WindowEvent::MouseWheel { .. }
            if over_other_pane || matches!(layout.hit(tabs.pointer), Hit::Icon(..)) =>
        {
            return None;
        },
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

    let window =
        *windows.iter().find(|(_, ctx)| ctx.display.tabs.locate(ctx.first, id).is_some())?.0;
    let ctx = windows.get_mut(&window)?;
    let (tab, index) = ctx.display.tabs.locate(ctx.first, id)?;
    let tabs = &ctx.display.tabs;

    if (tab, index) == tabs.live() {
        let event = match event {
            // The last pane, or a window being closed, closes like a window without tabs.
            TerminalEvent::Exit if tabs.count() > 1 && tabs.ending.is_none() => {
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
    // the live one, which they are drawn with, unless a zoom hides them, or in a preview.
    let shown = tab == tabs.active && !tabs.tabs[tab].zoom || peek::shows(tabs, id);
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
    let id = ctx.first;
    let tabs = &mut ctx.display.tabs;
    let scale = ctx.display.window.scale_factor;
    let old = mem::replace(&mut tabs.scale, scale);
    // Zoomed panes keep their text as large on a screen of another scale.
    if old > 0. && old != scale {
        for session in tabs.sessions() {
            session.zoom = session.zoom.map(|size| size.scale((scale / old) as f32));
        }
    }
    if tabs.window == PhysicalSize::default() {
        tabs.window = ctx.display.window.inner_size();
    }

    // The interface's font follows the configuration and the window's scale.
    let source = (config.tabbed.ui_font.clone(), tabs.scale);
    if tabs.font_source.as_ref() != Some(&source) {
        match UiFont::new(&source.0, source.1) {
            Ok(font) => tabs.font = Some(Rc::new(font)),
            Err(err) => error!("Could not load the interface font: {err}"),
        }
        tabs.font_source = Some(source);
        redraw(ctx);
    }
    let tabs = &mut ctx.display.tabs;
    let shown = (ctx.config.tabbed.tabs || tabs.count() > 1) && tabs.font.is_some();

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

    let frame = match resize || ctx.display.tabs.configs.is_none() {
        true => frame(ctx.display.window.winit()),
        false => ctx.display.tabs.frame,
    };
    let px = |value: f32| (value * ctx.display.window.scale_factor as f32).round() as u32;
    // Without its panel the terminal fills the window, its text still lined up with the logo.
    let tabs = &mut ctx.display.tabs;
    tabs.panel = config.tabbed.panel;
    tabs.caption = shown;
    // A bar that is not pinned is drawn over the panes, which take the whole window.
    tabs.autohide.pinned = config.tabbed.pin_tab_bar;
    let gap = f32::from(config.tabbed.pane_gap.min(MAX_PANE_GAP));
    let regap = mem::replace(&mut tabs.gap, gap) != gap;
    let (offset, inset) = match shown {
        false => (0, 0),
        _ if !config.tabbed.pin_tab_bar => (0, (MARGIN + INSET) as u8),
        _ if !config.tabbed.panel => (px(BAR), (MARGIN + INSET) as u8),
        _ => (px(BAR) - px(MARGIN), (MARGIN + INSET) as u8),
    };
    note_font(ctx);

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
    if offset != tabs.height || changed || resize || regap || cells != tabs.cells {
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
    let tabs = &mut ctx.display.tabs;
    let (area, scale) = (tabs.area(), tabs.scale as f32);
    // A minimized window leaves no room for the panes.
    let active = tabs.active;
    if tabs.tabs.get(active).is_none() || area.w < 1. || area.h < 1. {
        return;
    }

    // Panes hidden by a zoom keep their size; the others are fit with cells of their own font.
    let rects = tabs.pane_rects();
    let tab = &tabs.tabs[active];
    let fonts: Vec<_> = rects
        .iter()
        .filter_map(|&(index, _)| {
            let session = tab.panes[index].session.as_ref()?;
            Some((index, session.config.clone(), session.font(scale)))
        })
        .collect();
    let cells: Vec<_> = fonts
        .into_iter()
        .map(|(index, config, font)| (index, ctx.display.cell_size(&config, &font)))
        .collect();
    let tabs = &mut ctx.display.tabs;
    let Some(tab) = tabs.tabs.get_mut(active) else { return };
    for &(index, rect) in &rects {
        let Some(session) = &mut tab.panes[index].session else { continue };
        let Some(&(_, cells)) = cells.iter().find(|&&(i, _)| i == index) else { continue };
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

    // The pointer, still where it was, may be on other panes' controls now.
    if tabs.near.is_some() || tabs.control.is_some() {
        tabs.near = panes::near(tabs, tabs.pointer);
        tabs.control = panes::control(tabs, tabs.pointer, scale);
    }

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
    let corners = config.tabbed.corners;
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
    if !matches!(command, Command::Fullscreen | Command::Zoom) {
        autohide::flash(&mut ctx.display.tabs, Instant::now());
    }
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
        (Hit::Tab(index) | Hit::Icon(index, _) | Hit::Close(index), MouseButton::Middle) => {
            close(ctx, index, config)
        },
        (Hit::Tab(index) | Hit::Icon(index, _) | Hit::Close(index), MouseButton::Right) => {
            context::open_tab(ctx, index)
        },
        (Hit::New, MouseButton::Left) => run(ctx, Command::New, config, proxy),
        (Hit::Menu, MouseButton::Left) => {
            let profiles = Profiles::load(config).list.into_iter().filter(|p| !p.hidden).collect();
            ctx.display.tabs.menu = Some(Menu { profiles, selected: 0 });
            redraw(ctx);
        },
        (Hit::Settings, MouseButton::Left) => open_settings(ctx, config, proxy, Page::General),
        (Hit::Pin, MouseButton::Left) => toggle_pin(config, proxy),
        (Hit::Previous, MouseButton::Left) => run(ctx, Command::Previous, config, proxy),
        (Hit::Next, MouseButton::Left) => run(ctx, Command::Next, config, proxy),
        (Hit::Minimize, MouseButton::Left) => ctx.display.window.set_minimized(true),
        // The restore button leaves fullscreen, else maximizes or restores.
        (Hit::Maximize, MouseButton::Left) => {
            let display = &ctx.display.window;
            match restore(ctx.display.tabs.frame, window.fullscreen().is_some()) {
                Restore::LeaveFullscreen => display.set_fullscreen(false),
                // The platform leaves only a fullscreen it entered itself, and the window goes
                // back to where it was before.
                Restore::LeaveOtherFullscreen => {
                    display.set_fullscreen(true);
                    display.set_fullscreen(false);
                    if let Some((position, size)) = ctx.display.tabs.placed[0] {
                        let _ = window.request_inner_size(size);
                        window.set_outer_position(position);
                    }
                },
                Restore::ToggleMaximized => display.toggle_maximized(),
            }
        },
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
    let wake = waker(ctx.display.tabs.live_id(ctx.first), proxy);
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
    let (id, session) = match Session::spawn(ctx, config, proxy, profile, None, None) {
        Ok(pane) => pane,
        Err(err) => return error!("Could not open tab: {err}"),
    };
    let pane = Pane::new(id, profile, config, Some(session));
    ctx.display.tabs.tabs.push(Tab::new(pane));
    select(ctx, ctx.display.tabs.tabs.len() - 1, config);
}

/// Split the live pane, putting a new one with the same profile beside it, after it along
/// `axis`, and focus the new pane, if there is room for it.
pub fn split(
    ctx: &mut WindowContext,
    axis: Axis,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) {
    let tabs = &ctx.display.tabs;
    let Some(tab) = tabs.tabs.get(tabs.active) else { return };
    let min = min_pane(&ctx.display.size_info);
    // A split unzooms the tab, so the pane is cut where its layout places it.
    let rect = tabs.placed().get(tab.focus).map_or_else(|| tabs.area(), |&(_, rect)| rect);
    if !split::fits(rect, axis, Side::After, &Split::Pane(()), min, tabs.spacing()) {
        return;
    }

    let name = tab.pane().profile.clone();
    let profiles = Profiles::load(config).list;
    let profile = profiles.into_iter().find(|profile| Some(&profile.name) == name.as_ref());
    // The new shell starts where the focused one is.
    let reported = ctx.terminal.lock().tabbed.reported_directory.clone();
    #[cfg(not(windows))]
    let process = || crate::daemon::foreground_process_path(ctx.master_fd, ctx.shell_pid).ok();
    #[cfg(windows)]
    let command = profile.as_ref().map(|profile| profile.command.clone());
    #[cfg(windows)]
    let process = || cwd::process_directory(ctx.shell_pid, command.as_deref());
    let (profile, directory) = cwd::inherit(profile, reported, process);
    let spawned = Session::spawn(ctx, config, proxy, profile.as_ref(), directory, None);
    let (id, session) = match spawned {
        Ok(pane) => pane,
        Err(err) => {
            error!("Could not split pane: {err}");
            return;
        },
    };

    let pane = Pane::new(id, profile.as_ref(), config, Some(session));
    let (active, focus) = ctx.display.tabs.live();
    let tab = &mut ctx.display.tabs.tabs[active];
    let target = tab.panes[focus].id;
    let index = tab.insert(target, pane, axis, Side::After);
    focus_pane(ctx, active, index, config);
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

/// Pin the bar, or let it slide in only when the pointer nears it, in every window: the setting
/// is saved and the configuration reloaded.
fn toggle_pin(config: &UiConfig, proxy: &EventLoopProxy<Event>) {
    let before = AppSettings::new(config);
    let after = AppSettings { pin_tab_bar: !before.pin_tab_bar, ..before.clone() };
    match after.save(&before, config) {
        Ok(path) => {
            let _ = proxy.send_event(Event::new(EventType::ConfigReload(path), None));
        },
        Err(err) => error!("Could not save the tab bar's pin: {err}"),
    }
}

/// How a window ends whose tab at `index`, of `len`, is closed: an index past the tabs closes the
/// window itself.
fn ending(len: usize, index: usize) -> Ending {
    if index >= len { Ending::Window } else { Ending::Tabs }
}

/// Name the tab at `tab`, or give it back its focused pane's title with none.
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
    let before = ctx.config.font.clone().with_size(ctx.display.font_size);
    session.swap(ctx);
    let tabs = &mut ctx.display.tabs;
    relive(&mut tabs.tabs, &mut tabs.active, (tab, pane), session);
    let title = tabs.tabs[tab].panes[pane].title.clone();

    // Apply the pane's configuration overrides and its text size, then its title and the
    // current size.
    let font_size = ctx.display.font_size;
    ctx.update_config(config.clone());
    let font = ctx.config.font.clone().with_size(font_size);
    // The configuration may have asked for its own size instead.
    ctx.display.font_size = font_size;
    if font != before || ctx.display.pending_update.font().is_some() {
        ctx.display.pending_update.set_font(font.clone());
    }
    sync(ctx, config, false);
    ctx.display.window.set_title(title);
    // The terminal is fit to its own pane in its own font, which the display follows with its
    // next update.
    let (rect, display) = (ctx.display.tabs.focused_rect(), ctx.display.size_info);
    let same = font == before && (rect.w, rect.h) == (display.width(), display.height());
    let size = if rect.w < 1. || rect.h < 1. || same {
        display
    } else {
        let cells = ctx.display.cell_size(&ctx.config, &font);
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
    let min = min_pane(&ctx.display.size_info);
    let tabs = &mut ctx.display.tabs;
    let room = (tabs.area(), min, tabs.spacing());
    if tabs.tabs.is_empty() || tabs.live() != (tab, pane) {
        return;
    }
    if place(&mut tabs.tabs, &mut tabs.active, shown, target, room) {
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
    let min = min_pane(&ctx.display.size_info);
    let tabs = &mut ctx.display.tabs;
    let room = (tabs.area(), min, tabs.spacing());
    if tabs.active != from || from >= tabs.tabs.len() {
        return;
    }
    if group(&mut tabs.tabs, &mut tabs.active, into, dock, room) {
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
        ctx.display.tabs.ending = Some(ending(len, index));
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
            let room = (Rect::new(0., 40., 1000., 600.), (0., 0.), 0.);
            let placed = place(&mut self.tabs, &mut self.active, shown, target, room);
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
        let area = (Rect::new(0., 0., 800., 600.), (0., 0.), 0.);
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
        let area = (Rect::new(0., 0., 800., 600.), (0., 0.), 0.);
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
    fn fullscreen_keeps_the_panel_like_maximized() {
        for (panel, gap, scale) in
            [(true, 16., 1.), (true, 0., 2.), (true, 48., 1.5), (false, 16., 1.)]
        {
            let tabs = |frame| Tabs { panel, gap, scale, frame, ..Tabs::default() };
            let (maximized, fullscreen) = (tabs(Frame::Maximized), tabs(Frame::Fullscreen));
            assert_eq!(fullscreen.margin(), maximized.margin());
            assert_eq!(fullscreen.spacing(), maximized.spacing());
            assert_eq!(fullscreen.margin() > 0., panel);
        }
    }

    #[test]
    fn closing_the_last_tab_or_the_window() {
        assert_eq!(ending(1, 0), Ending::Tabs);
        assert_eq!(ending(0, 0), Ending::Window, "without tabs");
        assert_eq!(ending(3, usize::MAX), Ending::Window);
        assert_eq!(ending(1, usize::MAX), Ending::Window);
        let tabs = Tabs { ending: Some(Ending::Window), ..Tabs::default() };
        assert!(tabs.closed_whole());
        assert!(!Tabs { ending: Some(Ending::Tabs), ..Tabs::default() }.closed_whole());
        assert!(!Tabs::default().closed_whole(), "the last shell exited");
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

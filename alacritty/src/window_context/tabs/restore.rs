//! Reopening the windows, tabs and panes open when Alacritty last quit.
//!
//! The session is saved when Alacritty quits by closing its windows, as JSON in the state
//! directory, and restored by a plain launch: each pane gets a new shell in its saved directory,
//! below the text it last showed when the text is saved too. Running programs are not resumed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use std::{fs, io};

use crossfont::Size as FontSize;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event_loop::EventLoopProxy;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Grid, Row};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::tty::Options as PtyOptions;
use alacritty_terminal::vte::ansi::{self, Color, NamedColor, StdSyncHandler};

use crate::cli::{Options as CliOptions, ParsedOptions};
use crate::config::UiConfig;
use crate::config::window::tabbed::RestoreSession;
use crate::display::window::Window as DisplayWindow;
use crate::event::Event;
use crate::window_context::WindowContext;

use super::profiles::{Profile, Profiles};
use super::split::Split;
use super::{
    Pane, Session, Tab, Tabs, ZOOM_MIN_POINTS, ZOOM_RANGE, next_id, platform, set_live_font, sync,
};

/// Version of the file's format, which a file of another version is not restored from.
const VERSION: u32 = 1;

/// Lines of text kept of each pane, and the most bytes they take.
const LINES: usize = 2_000;
const BYTES: usize = 1 << 20;

/// Largest session file read, far above what panes of [`BYTES`] make.
const MAX_FILE: u64 = 64 << 20;

/// How long before the last window windows closed with it are saved with it, as when logging out
/// closes them one after another.
const BURST: Duration = Duration::from_secs(2);

/// Written below a pane's restored text, above its new shell.
const SEPARATOR: &str = "\x1b[0m\r\n\x1b[2m── Restored session ──\x1b[0m\r\n";

/// Attributes of text that are saved, with their SGR parameters.
const STYLES: [(Flags, u8); 7] = [
    (Flags::BOLD, 1),
    (Flags::DIM, 2),
    (Flags::ITALIC, 3),
    (Flags::UNDERLINE, 4),
    (Flags::INVERSE, 7),
    (Flags::HIDDEN, 8),
    (Flags::STRIKEOUT, 9),
];

/// The saved session.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct File {
    version: u32,
    windows: Vec<Window>,
}

/// A saved window.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Window {
    /// Inner size, in logical pixels.
    size: (f32, f32),
    /// Outer position, in physical pixels, where the system tells it.
    position: Option<(i32, i32)>,
    maximized: bool,
    fullscreen: bool,
    /// Index of the active tab.
    active: usize,
    tabs: Vec<TabState>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct TabState {
    name: Option<String>,
    /// Index of the focused pane.
    focus: usize,
    zoom: bool,
    /// The panes' places, by their index.
    layout: Split<usize>,
    panes: Vec<PaneState>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
struct PaneState {
    /// Name of the profile it was opened with.
    profile: Option<String>,
    title: String,
    directory: Option<PathBuf>,
    /// Size of its text in points, while zoomed away from its configuration's.
    font: Option<f32>,
    /// Its text, with escape sequences for its colours and attributes.
    text: Option<String>,
}

impl Window {
    /// The tab and the pane that become the window's live terminal.
    fn live(&self) -> &PaneState {
        let tab = &self.tabs[self.active];
        &tab.panes[tab.focus]
    }

    /// Whether every index in it points at something, so it can be restored as it is.
    fn valid(&self) -> bool {
        let tab_valid = |tab: &TabState| {
            let mut leaves = tab.layout.panes();
            leaves.sort_unstable();
            tab.focus < tab.panes.len()
                && leaves.into_iter().eq(0..tab.panes.len())
                && ratios_valid(&tab.layout)
        };
        self.active < self.tabs.len() && self.tabs.iter().all(tab_valid)
    }
}

/// Whether every split of `layout` gives each part some of the space.
fn ratios_valid(layout: &Split<usize>) -> bool {
    match layout {
        Split::Pane(_) => true,
        Split::Pair { ratio, first, second, .. } => {
            *ratio > 0. && *ratio < 1. && ratios_valid(first) && ratios_valid(second)
        },
    }
}

/// Where the session is saved.
pub fn path() -> Option<PathBuf> {
    #[cfg(not(windows))]
    return xdg::BaseDirectories::with_prefix("alacritty").place_state_file("session.json").ok();
    #[cfg(windows)]
    return Some(dirs::data_local_dir()?.join("alacritty").join("session.json"));
}

/// Whether Alacritty was started plainly, without a command, directory, hold or daemon of its
/// own: only such a process restores, saves or forgets the session.
pub fn plain(options: &CliOptions) -> bool {
    let terminal = &options.window_options.terminal_options;
    !options.daemon
        && !terminal.hold
        && terminal.working_directory.is_none()
        && terminal.command().is_none()
}

/// The windows to restore at startup, taken out of the saved session, which is moved aside.
///
/// None unless the setting is on and Alacritty was started plainly; with the setting off, any
/// saved session is forgotten.
pub fn take(config: &UiConfig, options: &CliOptions) -> Vec<Window> {
    if !plain(options) {
        return Vec::new();
    }
    if config.window.tabbed.restore_session == RestoreSession::Off {
        forget();
        return Vec::new();
    }
    path().map_or_else(Vec::new, |path| load(&path))
}

/// The windows saved at `path`, which is moved aside first: a second Alacritty started meanwhile
/// does not restore them again, nor does a restart after a crash while restoring. An invalid file
/// stays aside.
fn load(path: &Path) -> Vec<Window> {
    let aside = aside(path);
    match fs::rename(path, &aside) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            warn!("Not restoring the session, which could not be moved to {aside:?}: {err}");
            return Vec::new();
        },
        Ok(()) => (),
    }
    if fs::metadata(&aside).is_ok_and(|metadata| metadata.len() > MAX_FILE) {
        warn!("Ignoring the saved session, which is too large, kept as {aside:?}");
        return Vec::new();
    }
    let text = match fs::read_to_string(&aside) {
        Ok(text) => text,
        Err(err) => {
            warn!("Ignoring the unreadable saved session, kept as {aside:?}: {err}");
            return Vec::new();
        },
    };
    match serde_json::from_str::<File>(&text) {
        Ok(file) if file.version == VERSION && file.windows.iter().all(Window::valid) => {
            file.windows
        },
        Ok(file) => {
            warn!("Ignoring the saved session of version {}, kept as {aside:?}", file.version);
            Vec::new()
        },
        Err(err) => {
            warn!("Ignoring the invalid saved session, kept as {aside:?}: {err}");
            Vec::new()
        },
    }
}

/// Where a session is moved aside to.
fn aside(path: &Path) -> PathBuf {
    path.with_extension("json.prev")
}

/// Forget the saved session, and the one set aside.
pub fn forget() {
    if let Some(path) = path() {
        remove(&path);
        remove(&aside(&path));
    }
}

/// Forget the session set aside once it was restored, which may hold secrets typed in it.
pub fn restored() {
    if let Some(path) = path() {
        remove(&aside(&path));
    }
}

/// Remove the file at `path`, if any.
fn remove(path: &Path) {
    match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => {
            warn!("Could not remove {path:?}: {err}");
        },
        _ => (),
    }
}

/// What a quitting Alacritty does with the session file.
#[derive(Debug, PartialEq)]
pub enum Quit<W> {
    Save(Vec<W>),
    /// Remove it, and the one set aside.
    Forget,
    /// Leave it as it is, which may be another Alacritty's.
    Leave,
}

/// What an Alacritty started plainly or not quits with: saving the `windows` saved at the
/// `level` of the setting, forgetting the session when the setting is off, else leaving it.
pub fn quit<W>(plain: bool, level: RestoreSession, windows: Vec<W>) -> Quit<W> {
    match level {
        _ if !plain => Quit::Leave,
        RestoreSession::Off => Quit::Forget,
        _ if windows.is_empty() => Quit::Leave,
        _ => Quit::Save(windows),
    }
}

/// Save `windows` as the session.
pub fn save(windows: Vec<Window>) -> io::Result<()> {
    write(&path().ok_or(io::ErrorKind::NotFound)?, windows)
}

/// Write `windows` to `path` at once, readable only by the user.
fn write(path: &Path, windows: Vec<Window>) -> io::Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    // A temporary file is created readable only by its owner, and replaces the old one whole.
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut file, &File { version: VERSION, windows })?;
    file.persist(path).map_err(|err| err.error)?;
    Ok(())
}

/// Keep `window`, closed as a window at `now`, in `closed`, forgetting those closed too long
/// before to be saved with the last window.
pub fn remember<W>(closed: &mut Vec<(Instant, W)>, now: Instant, window: Option<W>) {
    closed.retain(|(at, _)| now.duration_since(*at) <= BURST);
    closed.extend(window.map(|window| (now, window)));
}

/// Of the windows `closed` as windows, with when, those saved once the last one is closed at
/// `now`: those closed shortly before it, when it was closed as a window itself too.
pub fn kept<W>(closed: Vec<(Instant, W)>, now: Instant, last_whole: bool) -> Vec<W> {
    if !last_whole {
        return Vec::new();
    }
    let recent = closed.into_iter().filter(|(at, _)| now.duration_since(*at) <= BURST);
    recent.map(|(_, window)| window).collect()
}

/// The state of the window `ctx`, with the text of its panes at the `level` of text.
pub fn capture(ctx: &WindowContext, level: RestoreSession) -> Window {
    let window = &ctx.display.window;
    let scale = window.scale_factor;
    let size = window.inner_size().to_logical::<f32>(scale);
    let winit = window.winit();
    let tabs = &ctx.display.tabs;
    let points = |size: FontSize| size.as_pt() / scale as f32;
    let text = |grid: &Grid<Cell>| (level == RestoreSession::Text).then(|| history(grid));

    // Where the program in the foreground of a terminal is.
    #[cfg(not(windows))]
    let cwd = |fd, pid| crate::daemon::foreground_process_path(fd, pid).ok();

    // The live terminal's state is the window's.
    let configured = ctx.config.font.size().scale(scale as f32);
    let profile = match tabs.tabs.get(tabs.active) {
        Some(tab) => tab.pane().profile.clone(),
        None => tabs.first_profile.borrow().as_ref().map(|profile| profile.name.clone()),
    };
    let live = PaneState {
        profile,
        title: window.title().to_owned(),
        #[cfg(not(windows))]
        directory: cwd(ctx.master_fd, ctx.shell_pid),
        #[cfg(windows)]
        directory: None,
        font: (ctx.display.font_size != configured).then(|| points(ctx.display.font_size)),
        text: text(ctx.terminal.lock().primary_grid()),
    };

    let saved_tabs = if tabs.tabs.is_empty() {
        let panes = vec![live];
        vec![TabState { name: None, focus: 0, zoom: false, layout: Split::Pane(0), panes }]
    } else {
        let mut live = Some(live);
        let tabs = tabs.tabs.iter().map(|tab| TabState {
            name: tab.name.clone(),
            focus: tab.focus,
            zoom: tab.zoom,
            layout: indices(tab),
            panes: tab
                .panes
                .iter()
                .map(|pane| match &pane.session {
                    Some(session) => PaneState {
                        profile: pane.profile.clone(),
                        title: pane.title.clone(),
                        #[cfg(not(windows))]
                        directory: cwd(session.master_fd, session.shell_pid),
                        #[cfg(windows)]
                        directory: None,
                        font: session.zoom.map(points),
                        text: text(session.terminal.lock().primary_grid()),
                    },
                    None => live.take().unwrap_or_default(),
                })
                .collect(),
        });
        tabs.collect()
    };

    Window {
        size: (size.width, size.height),
        position: winit.outer_position().ok().map(|position| (position.x, position.y)),
        maximized: window.is_maximized(),
        fullscreen: winit.fullscreen().is_some(),
        active: tabs.active.min(saved_tabs.len() - 1),
        tabs: saved_tabs,
    }
}

/// The layout of `tab` by the index of its panes, rather than their ids.
fn indices<S>(tab: &Tab<S>) -> Split<usize> {
    tab.layout.clone().map(&mut |id| tab.position(id).unwrap_or_default())
}

/// Start the window's first terminal, which becomes its live one, like the live pane of `saved`:
/// with its profile, when the window has tabs, and in its directory.
pub fn prepare(config: &UiConfig, tabs: &Tabs, saved: &Window, pty: &mut PtyOptions) {
    let live = saved.live();
    if config.window.tabbed.tabs {
        let profile = Profiles::load(config).list.into_iter().find(|p| is(p, &live.profile));
        if let Some(profile) = &profile {
            profile.apply(pty);
        }
        *tabs.first_profile.borrow_mut() = profile;
    }
    if let Some(directory) = directory(live) {
        pty.working_directory = Some(directory);
    }
}

/// The saved text of the window's live pane.
pub fn first_text(saved: &Window) -> Option<&str> {
    saved.live().text.as_deref()
}

/// Give the new window `ctx`, whose live terminal was started by [`prepare`], the place, the tabs
/// and the panes of `saved`, starting a terminal for each other pane.
pub fn install(
    ctx: &mut WindowContext,
    saved: Window,
    config: &Rc<UiConfig>,
    proxy: &EventLoopProxy<Event>,
) {
    apply_geometry(&ctx.display.window, &saved);
    let scale = ctx.display.window.scale_factor as f32;
    let size = |points: f32| FontSize::new(points).scale(scale);
    let live = saved.live().clone();
    let first_profile = ctx.display.tabs.first_profile.borrow_mut().take();

    if config.window.tabbed.tabs {
        let profiles = Profiles::load(config).list;
        let mut tabs = Vec::new();
        let mut active = 0;
        for (index, tab) in saved.tabs.into_iter().enumerate() {
            let live_pane = (index == saved.active).then_some(tab.focus);
            let (mut panes, mut ids, mut failed) = (Vec::new(), Vec::new(), Vec::new());
            for (pane, state) in tab.panes.into_iter().enumerate() {
                let profile = profiles.iter().find(|p| is(p, &state.profile));
                let spawned = if live_pane == Some(pane) {
                    Ok((ctx.first, None))
                } else {
                    let (directory, text) = (directory(&state), state.text.as_deref());
                    Session::spawn(ctx, config, proxy, profile, directory, text).map(
                        |(id, mut session)| {
                            let configured = session.config.font.size().as_pt();
                            session.zoom =
                                state.font.and_then(|pt| zoomed(pt, configured)).map(size);
                            (id, Some(session))
                        },
                    )
                };
                match spawned {
                    Ok((id, session)) => {
                        let mut pane = Pane::new(id, profile, config, session);
                        pane.title = state.title;
                        panes.push(pane);
                        ids.push(id);
                    },
                    Err(err) => {
                        error!("Could not restore a pane: {err}");
                        let id = next_id();
                        failed.push(id);
                        ids.push(id);
                    },
                }
            }

            // A pane that could not start leaves its place to its neighbour.
            let mut layout = Some(tab.layout.map(&mut |pane| ids[pane]));
            for id in failed {
                layout = layout.and_then(|layout| layout.remove(id));
            }
            let Some(layout) = layout else { continue };
            if index == saved.active {
                active = tabs.len();
            }
            let focus = ids[tab.focus];
            let zoom = tab.zoom && panes.len() > 1;
            let mut tab = Tab { layout, panes, focus: 0, zoom, name: tab.name };
            tab.focus = tab.position(focus).unwrap_or_default();
            tabs.push(tab);
        }

        // In place before the window first shows its tabs, which would make a pane of the window.
        let tabs_state = &mut ctx.display.tabs;
        tabs_state.host = platform::hostname().to_uppercase();
        tabs_state.tabs = tabs;
        tabs_state.active = active;
        let options = first_profile.as_ref().map_or_else(Vec::new, Profile::overrides);
        ctx.window_config = ParsedOptions::from_options(&options);
        ctx.update_config(config.clone());
    } else {
        info!("Restoring only the focused pane of each window, since tabs are off");
    }

    let configured = ctx.config.font.size().as_pt();
    if let Some(points) = live.font.and_then(|points| zoomed(points, configured)) {
        set_live_font(ctx, size(points));
    }
    if config.window.tabbed.tabs {
        sync(ctx, config, true);
    }
    if !ctx.preserve_title {
        ctx.display.window.set_title(live.title);
    }
}

/// Give `window` the size, place and state of `saved`.
fn apply_geometry(window: &DisplayWindow, saved: &Window) {
    let size = LogicalSize::new(saved.size.0, saved.size.1);
    window.request_inner_size(size.to_physical(window.scale_factor));
    if let Some((x, y)) = saved.position {
        window.winit().set_outer_position(PhysicalPosition::new(x, y));
    }
    window.set_maximized(saved.maximized);
    window.set_fullscreen(saved.fullscreen);
}

/// A saved text size of `points`, within the bounds of a zoom of text `configured` points large;
/// none for a size that is no number.
fn zoomed(points: f32, configured: f32) -> Option<f32> {
    let (low, high) = (ZOOM_RANGE.0 * configured, ZOOM_RANGE.1 * configured);
    let (low, high) = (low.max(ZOOM_MIN_POINTS), high.max(ZOOM_MIN_POINTS));
    points.is_finite().then(|| points.clamp(low, high))
}

/// Whether `profile` is the one named `name`.
fn is(profile: &Profile, name: &Option<String>) -> bool {
    Some(&profile.name) == name.as_ref()
}

/// The saved directory of `pane`, while it is still there.
fn directory(pane: &PaneState) -> Option<PathBuf> {
    pane.directory.clone().filter(|directory| directory.is_dir())
}

/// Write `text`, saved from a pane, into its new terminal `term` before its shell starts, with a
/// line telling it apart from what the shell writes.
pub fn replay<T: EventListener>(term: &mut Term<T>, text: &str) {
    if text.is_empty() {
        return;
    }
    let mut parser = ansi::Processor::<StdSyncHandler>::new();
    parser.advance(term, inert(text).as_bytes());
    parser.advance(term, SEPARATOR.as_bytes());
}

/// `text` with nothing but what [`history`] writes: printable characters, line breaks, tabs and
/// SGR sequences; any other control or escape sequence, which could ask the terminal to answer,
/// set a title or the clipboard, is dropped.
fn inert(text: &str) -> String {
    let mut inert = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        // What follows an escape, or its single character form.
        let kind = match c {
            '\x1b' => chars.next(),
            '\u{80}'..='\u{9f}' => char::from_u32(u32::from(c) - 0x40),
            '\r' | '\n' | '\t' => {
                inert.push(c);
                continue;
            },
            c if !c.is_control() => {
                inert.push(c);
                continue;
            },
            _ => continue,
        };
        match kind {
            // A control sequence: parameters, intermediates, then a final character.
            Some('[') => {
                let mut sequence = String::new();
                while let Some(c) = chars.next_if(|c| ('\x20'..='\x3f').contains(c)) {
                    sequence.push(c);
                }
                let last = chars.next_if(|c| ('\x40'..='\x7e').contains(c));
                if last == Some('m') && sequence.chars().all(|c| c.is_ascii_digit() || c == ';') {
                    let _ = write!(inert, "\x1b[{sequence}m");
                }
            },
            // A string, such as a title, a clipboard or a device query, up to its terminator.
            Some(']' | 'P' | 'X' | '^' | '_') => {
                while let Some(c) = chars.next() {
                    if c == '\x07'
                        || c == '\u{9c}'
                        || (c == '\x1b' && chars.next_if_eq(&'\\').is_some())
                    {
                        break;
                    }
                }
            },
            // Any other escape: intermediates, then a final character.
            _ => {
                while chars.next_if(|c| ('\x20'..='\x2f').contains(c)).is_some() {}
                chars.next_if(|c| ('\x30'..='\x7e').contains(c));
            },
        }
    }
    inert
}

/// The text of `grid`, with escape sequences for its colours and attributes: its last [`LINES`]
/// lines up to the last one with anything on it, within [`BYTES`].
fn history(grid: &Grid<Cell>) -> String {
    text(grid, LINES, BYTES)
}

/// The text of the last `lines` lines of `grid` with anything on them, within `bytes`.
fn text(grid: &Grid<Cell>, lines: usize, bytes: usize) -> String {
    let (top, bottom) = (grid.topmost_line().0, grid.bottommost_line().0);
    let Some(last) = (top..=bottom).rev().find(|&line| !grid[Line(line)].is_clear()) else {
        return String::new();
    };
    let first = top.max(last + 1 - lines as i32);
    let mut rows = Vec::new();
    let mut total = 0;
    for line in (first..=last).rev() {
        let row = encode(&grid[Line(line)]);
        total += row.len();
        if total > bytes {
            break;
        }
        rows.push(row);
    }
    rows.reverse();
    let mut text = rows.concat();
    if text.ends_with("\r\n") {
        text.truncate(text.len() - 2);
    }
    text
}

/// One row of text: its characters, with SGR sequences where their style changes, without blanks
/// at its end, and a line break unless it wraps.
fn encode(row: &Row<Cell>) -> String {
    let wraps = row.last().is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE));
    let end = match wraps {
        true => row.len(),
        false => row.into_iter().rposition(|cell| !blank(cell)).map_or(0, |end| end + 1),
    };
    let plain = (Color::Named(NamedColor::Foreground), Color::Named(NamedColor::Background));
    let plain = (plain.0, plain.1, Flags::empty());
    let mut style = plain;
    let mut text = String::new();
    let spacers = Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
    for cell in row[..Column(end)].iter().filter(|cell| !cell.flags.intersects(spacers)) {
        let flags = STYLES.iter().fold(Flags::empty(), |f, &(flag, _)| f | (cell.flags & flag));
        let next = (cell.fg, cell.bg, flags);
        if next != style {
            style = next;
            sgr(&mut text, style);
        }
        text.push(if cell.c == '\t' { ' ' } else { cell.c });
        text.extend(cell.zerowidth().into_iter().flatten());
    }
    if style != plain {
        text.push_str("\x1b[0m");
    }
    if !wraps {
        text.push_str("\r\n");
    }
    text
}

/// Whether `cell` shows nothing but the background.
fn blank(cell: &Cell) -> bool {
    let shown = Flags::INVERSE | Flags::ALL_UNDERLINES | Flags::STRIKEOUT;
    cell.c == ' '
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell.flags.intersects(shown)
        && cell.zerowidth().is_none()
}

/// Append the SGR sequence that sets `style` from scratch to `text`.
fn sgr(text: &mut String, (fg, bg, flags): (Color, Color, Flags)) {
    text.push_str("\x1b[0");
    for (flag, code) in STYLES {
        if flags.contains(flag) {
            let _ = write!(text, ";{code}");
        }
    }
    color(text, fg, 30);
    color(text, bg, 40);
    text.push('m');
}

/// Append the SGR parameters of `color` to `text`, `base` being 30 for the foreground and 40 for
/// the background; nothing for the default colours.
fn color(text: &mut String, color: Color, base: u8) {
    let _ = match color {
        Color::Named(named) => {
            let named = named as usize;
            let dim = NamedColor::DimBlack as usize;
            let index = match named {
                0..=15 => named,
                _ if (dim..dim + 8).contains(&named) => named - dim,
                _ => return,
            };
            let code =
                if index < 8 { base as usize + index } else { base as usize + 60 + index - 8 };
            write!(text, ";{code}")
        },
        Color::Indexed(index) => write!(text, ";{};5;{index}", base + 8),
        Color::Spec(rgb) => write!(text, ";{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b),
    };
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use winit::window::WindowId;

    use super::super::split::{Axis, Side};
    use super::*;

    fn term(columns: usize, lines: usize) -> Term<VoidListener> {
        Term::new(Config::default(), &TermSize::new(columns, lines), VoidListener)
    }

    /// `term` after `input` was written to it.
    fn written(mut term: Term<VoidListener>, input: &str) -> Term<VoidListener> {
        ansi::Processor::<StdSyncHandler>::new().advance(&mut term, input.as_bytes());
        term
    }

    fn pane(text: &str) -> PaneState {
        PaneState {
            profile: Some("zsh".into()),
            title: "~/src".into(),
            directory: Some("/tmp".into()),
            font: Some(13.),
            text: Some(text.into()),
        }
    }

    fn window() -> Window {
        let layout = Split::Pane(0).join(Split::Pane(1), Axis::Row, Side::After);
        let tab = TabState {
            name: Some("Build".into()),
            focus: 1,
            zoom: false,
            layout,
            panes: vec![pane("\x1b[32m$\x1b[0m make"), pane("")],
        };
        let single = TabState {
            name: None,
            focus: 0,
            zoom: false,
            layout: Split::Pane(0),
            panes: vec![pane("x")],
        };
        Window {
            size: (1280., 760.),
            position: Some((40, 30)),
            maximized: false,
            fullscreen: false,
            active: 0,
            tabs: vec![tab, single],
        }
    }

    #[test]
    fn colours_and_attributes() {
        let input = "a\x1b[31mb\x1b[1;92mc\x1b[0m\x1b[38;5;200;48;2;1;2;3md\x1b[0;3;4;7;9me\x1b[0m";
        let term = written(term(20, 4), input);
        assert_eq!(
            history(term.grid()),
            "a\x1b[0;31mb\x1b[0;1;92mc\x1b[0;38;5;200;48;2;1;2;3md\x1b[0;3;4;7;9me\x1b[0m"
        );
        // Dim and hidden text, and a background reaching past the last character.
        let term = written(self::term(10, 2), "\x1b[2;8mz\x1b[0m\x1b[44m  \x1b[0m");
        assert_eq!(history(term.grid()), "\x1b[0;2;8mz\x1b[0;44m  \x1b[0m");
    }

    #[test]
    fn wide_and_zero_width_characters() {
        let term = written(term(10, 2), "漢字e\u{301}");
        assert_eq!(history(term.grid()), "漢字e\u{301}");
    }

    #[test]
    fn wrapped_lines_join_and_blanks_are_trimmed() {
        let term = written(term(4, 5), "abcdef  \r\ngh \r\n\r\n");
        // The first line wraps after four columns; trailing blanks and empty lines go.
        assert_eq!(history(term.grid()), "abcdef\r\ngh");
        assert_eq!(history(self::term(4, 4).grid()), "");
    }

    #[test]
    fn caps_lines_and_bytes() {
        let input = (1..=2_500).fold(String::new(), |mut input, n| {
            let _ = write!(input, "{n}\r\n");
            input
        });
        let term = written(term(10, 5), &input);
        let all = text(term.grid(), LINES, BYTES);
        let lines: Vec<_> = all.split("\r\n").collect();
        assert_eq!((lines.len(), lines[0], lines[1_999]), (2_000, "501", "2500"));
        let small = text(term.grid(), LINES, 20);
        assert!(small.len() <= 20 && small.ends_with("2499\r\n2500"), "{small:?}");
    }

    #[test]
    fn replay_round_trip() {
        let input = "\x1b[1;31mred\x1b[0m plain\r\n\x1b[48;5;17m 漢 \x1b[0m\r\n\x1b[3mit";
        let source = written(term(30, 8), input);
        let text = history(source.grid());
        let mut target = term(30, 8);
        replay(&mut target, &text);
        for line in 0..3 {
            let (a, b) = (&source.grid()[Line(line)], &target.grid()[Line(line)]);
            for (a, b) in a.into_iter().zip(b) {
                assert_eq!((a.c, a.fg, a.bg, a.flags), (b.c, b.fg, b.bg, b.flags), "line {line}");
            }
        }
        // The separator follows, then the new shell's line.
        let separator: String = target.grid()[Line(3)].into_iter().map(|cell| cell.c).collect();
        assert!(separator.starts_with("── Restored session ──"));
        assert_eq!(target.grid().cursor.point.line, Line(4));
    }

    /// Records the events a terminal sends, such as answers for its shell.
    #[derive(Clone, Default)]
    struct Recorder(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

    impl EventListener for Recorder {
        fn send_event(&self, event: alacritty_terminal::event::Event) {
            self.0.borrow_mut().push(format!("{event:?}"));
        }
    }

    #[test]
    fn replay_is_inert() {
        let recorder = Recorder::default();
        let mut term = Term::new(Config::default(), &TermSize::new(30, 5), recorder.clone());
        // A title, the clipboard, a cursor report, a colour query, a DECRQSS request, a mode,
        // a hyperlink and C1 controls.
        let hostile = "a\x1b]0;t\x07\x1b]52;c;eA==\x07\x1b[6n\x1b]11;?\x07\x1bP$qm\x1b\\\
                       \x1b[?1049h\x1b]8;;http://x\x1b\\\u{9b}6n\x1b[31mb\x1b[0m";
        replay(&mut term, hostile);
        assert_eq!(*recorder.0.borrow(), Vec::<String>::new());
        let row = &term.grid()[Line(0)];
        let text: String = row.into_iter().take(2).map(|cell| cell.c).collect();
        assert_eq!(text, "ab");
        assert_eq!(row[Column(1)].fg, Color::Named(NamedColor::Red));
        assert!(!term.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN));
        assert_eq!(inert("x\x1b[1;38;5;20my\x1b[2Jz\r\n\tw"), "x\x1b[1;38;5;20myz\r\n\tw");
    }

    #[test]
    fn restored_sizes_stay_in_bounds() {
        assert_eq!(zoomed(13., 10.), Some(13.));
        assert_eq!(zoomed(100_000., 10.), Some(40.));
        assert_eq!(zoomed(0., 10.), Some(5.));
        assert_eq!(zoomed(1., 6.), Some(4.));
        assert_eq!(zoomed(f32::NAN, 10.), None);
    }

    #[test]
    fn only_plain_launches_touch_the_file() {
        use clap::Parser;
        let parse = |args: &[&str]| CliOptions::parse_from(args);
        assert!(plain(&parse(&["alacritty"])));
        assert!(plain(&parse(&["alacritty", "-o", "font.size=12"])));
        for args in [
            &["alacritty", "-e", "true"][..],
            &["alacritty", "--hold"],
            &["alacritty", "--working-directory", "/"],
            &["alacritty", "--daemon"],
        ] {
            assert!(!plain(&parse(args)), "{args:?}");
        }

        let (on, off) = (RestoreSession::Text, RestoreSession::Off);
        assert_eq!(quit(false, on, vec![1]), Quit::Leave, "-e sh never overwrites it");
        assert_eq!(quit(false, off, vec![1]), Quit::Leave);
        assert_eq!(quit(false, on, Vec::<i32>::new()), Quit::Leave, "-e true never removes it");
        assert_eq!(quit(true, on, vec![1]), Quit::Save(vec![1]));
        assert_eq!(quit(true, RestoreSession::Layout, vec![1]), Quit::Save(vec![1]));
        assert_eq!(quit(true, on, Vec::<i32>::new()), Quit::Leave, "ended by its tabs");
        assert_eq!(quit(true, off, vec![1]), Quit::Forget);
    }

    #[test]
    fn alternate_screen_keeps_the_normal_text() {
        let term = written(term(10, 3), "shell\x1b[?1049hfull screen");
        assert_eq!(history(term.primary_grid()), "shell");
    }

    #[test]
    fn file_round_trip() {
        let file = File { version: VERSION, windows: vec![window()] };
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains(r#""layout":{"Pair":{"axis":"Row","ratio":0.5"#), "{json}");
        assert_eq!(serde_json::from_str::<File>(&json).unwrap(), file);
    }

    #[test]
    fn layout_by_index_and_back() {
        let id = |id: u64| WindowId::from(id);
        let new = |id| Pane::<()>::new(id, None, &UiConfig::default(), None);
        let mut tab = Tab::new(new(id(7)));
        tab.insert(id(7), new(id(3)), Axis::Column, Side::Before);
        tab.insert(id(7), new(id(9)), Axis::Row, Side::After);
        let saved = indices(&tab);
        assert_eq!(saved.panes(), vec![0, 1, 2]);
        let ids: Vec<_> = tab.panes.iter().map(|pane| pane.id).collect();
        assert_eq!(saved.map(&mut |index| ids[index]), tab.layout);
    }

    #[test]
    fn only_valid_windows() {
        assert!(window().valid());
        let mut broken = window();
        broken.active = 2;
        assert!(!broken.valid());
        let mut broken = window();
        broken.tabs[0].layout = Split::Pane(0).join(Split::Pane(0), Axis::Row, Side::After);
        assert!(!broken.valid());
        let mut broken = window();
        broken.tabs[0].focus = 2;
        assert!(!broken.valid());
        let mut broken = window();
        let layout = Split::Pane(0).join(Split::Pane(1), Axis::Row, Side::After);
        let Split::Pair { first, second, axis, .. } = layout else { unreachable!() };
        broken.tabs[0].layout = Split::Pair { axis, ratio: 1.5, first, second };
        assert!(!broken.valid());
    }

    #[test]
    fn take_rules() {
        let mut config = UiConfig::default();
        config.window.tabbed.restore_session = RestoreSession::Text;
        for args in [&["alacritty", "-e", "true"][..], &["alacritty", "--hold"], &[
            "alacritty",
            "--working-directory",
            "/",
        ]] {
            use clap::Parser;
            let options = CliOptions::parse_from(args);
            assert!(take(&config, &options).is_empty(), "{args:?}");
        }
        let mut options = CliOptions::default();
        options.daemon = true;
        assert!(take(&config, &options).is_empty());
    }

    #[test]
    fn load_moves_the_file_aside() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.json");
        let aside = directory.path().join("session.json.prev");
        assert!(load(&path).is_empty(), "missing");

        write(&path, vec![window()]).unwrap();
        assert_eq!(load(&path), vec![window()]);
        assert!(!path.exists() && aside.exists());
        assert!(load(&path).is_empty(), "restored only once");

        for text in ["{not json", r#"{"version":2,"windows":[]}"#, "\u{0}\u{ff}"] {
            fs::write(&path, text).unwrap();
            assert!(load(&path).is_empty());
            assert_eq!(fs::read_to_string(&aside).unwrap(), text);
        }
        fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(load(&path).is_empty(), "not UTF-8");
        assert!(!path.exists() && aside.exists());
    }

    #[test]
    fn atomic_private_save() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state/alacritty/session.json");
        write(&path, vec![window()]).unwrap();
        let file: File = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file.windows, vec![window()]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Only the session is left in the directory.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        remove(&path);
        assert!(!path.exists());
        remove(&path);
    }

    #[test]
    fn windows_closed_together_are_kept() {
        let now = Instant::now();
        let closed = || vec![(now - Duration::from_secs(5), 1), (now - Duration::from_secs(1), 2)];
        assert_eq!(kept(closed(), now, true), vec![2]);
        assert_eq!(kept(closed(), now, false), Vec::<i32>::new());
        assert_eq!(kept(vec![(now, 3)], now, true), vec![3]);

        // Windows closed long before the next are forgotten at once.
        let mut closed = closed();
        remember(&mut closed, now, Some(3));
        assert_eq!(closed.iter().map(|(_, w)| *w).collect::<Vec<_>>(), vec![2, 3]);
        remember(&mut closed, now + Duration::from_secs(10), None);
        assert!(closed.is_empty());
    }
}

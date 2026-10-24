//! Alacritty's own settings, as the settings edit them: every key of `alacritty.toml` that holds a
//! value, under the name `alacritty(5)` documents, read from the configuration as loaded and
//! written back key by key, so the file stays their source of truth.
//!
//! The lists of key and mouse bindings, of hints and of indexed colors are left to the file.

use toml_edit::{Array, InlineTable, Item, Table, Value, value};

use crate::config::UiConfig;

use super::super::profiles;
use super::{commented, format, section};

/// Where a key is shown: the page, by what the settings of VS Code and Zed group together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    General,
    Window,
    Font,
    Colors,
    Cursor,
    Terminal,
    Advanced,
}

/// The systems a key works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Only {
    All,
    MacOs,
    /// Linux and the BSDs.
    Free,
    /// All but Windows.
    Unix,
}

/// A number a field holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Number {
    /// Least and most it can be.
    pub min: f32,
    pub max: f32,
    /// What its buttons and the arrow keys add or take away.
    pub step: f32,
    pub whole: bool,
    /// Whether it can be left empty, leaving the key out.
    pub optional: bool,
}

const fn whole(min: f32, max: f32, step: f32) -> Number {
    Number { min, max, step, whole: true, optional: false }
}

const fn optional(min: f32, max: f32) -> Number {
    Number { min, max, step: 1., whole: true, optional: true }
}

/// What a key holds, and so the control that edits it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Switch,
    /// One of the names, written as the first of each pair and shown as the second; an empty name
    /// leaves the key out.
    Choice(&'static [(&'static str, &'static str)]),
    Number(Number),
    /// Two numbers, in the keys `parts` of a table.
    Pair([&'static str; 2], Number),
    /// Text, shown as the placeholder while empty, which leaves the key out.
    Text(&'static str),
    /// A font's family and style, with the style's placeholder.
    Face(&'static str),
    /// A color, which may also name the color of the cell under it.
    Color {
        cells: bool,
    },
    /// Two colors, in the keys `parts` of a table, which may name the cell's.
    Colors([&'static str; 2], bool),
    /// A command line, written as its program and arguments.
    Command,
    /// A string on each line, written as an array.
    List,
    /// `NAME=value` on each line, written as a table.
    Map,
}

/// A key of `alacritty.toml`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    /// Its dotted path.
    pub path: &'static str,
    pub area: Area,
    /// Title of the group of rows it is shown in.
    pub section: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    pub only: Only,
    /// The key that has to be on, or set, for this one to do anything.
    pub needs: Option<&'static str>,
}

const fn key(
    area: Area,
    section: &'static str,
    path: &'static str,
    name: &'static str,
    description: &'static str,
    kind: Kind,
) -> Key {
    Key { path, area, section, name, description, kind, only: Only::All, needs: None }
}

impl Key {
    const fn only(self, only: Only) -> Self {
        Key { only, ..self }
    }

    const fn needs(self, needs: &'static str) -> Self {
        Key { needs: Some(needs), ..self }
    }

    /// Whether the key works on this system.
    pub fn available(&self) -> bool {
        match self.only {
            Only::All => true,
            Only::MacOs => cfg!(target_os = "macos"),
            Only::Free => cfg!(not(any(target_os = "macos", windows))),
            Only::Unix => cfg!(not(windows)),
        }
    }

    /// Number of fields editing the key.
    pub fn parts(&self) -> usize {
        match self.kind {
            Kind::Switch | Kind::Choice(_) => 0,
            Kind::Pair(..) | Kind::Face(_) | Kind::Colors(..) => 2,
            _ => 1,
        }
    }
}

const SHAPES: &[(&str, &str)] = &[("Block", "Block"), ("Underline", "Underline"), ("Beam", "Beam")];
const BLINKING: &[(&str, &str)] =
    &[("Never", "Never"), ("Off", "Off"), ("On", "On"), ("Always", "Always")];
#[cfg(target_os = "macos")]
const STARTUP_MODES: &[(&str, &str)] = &[
    ("Windowed", "Windowed"),
    ("Maximized", "Maximized"),
    ("Fullscreen", "Fullscreen"),
    ("SimpleFullscreen", "Simple"),
];
#[cfg(not(target_os = "macos"))]
const STARTUP_MODES: &[(&str, &str)] =
    &[("Windowed", "Windowed"), ("Maximized", "Maximized"), ("Fullscreen", "Fullscreen")];
const ANIMATIONS: &[(&str, &str)] = &[
    ("Linear", "Linear"),
    ("Ease", "Ease"),
    ("EaseOut", "Ease out"),
    ("EaseOutSine", "Ease out sine"),
    ("EaseOutQuad", "Ease out quad"),
    ("EaseOutCubic", "Ease out cubic"),
    ("EaseOutQuart", "Ease out quart"),
    ("EaseOutQuint", "Ease out quint"),
    ("EaseOutExpo", "Ease out expo"),
    ("EaseOutCirc", "Ease out circ"),
];
const LOG_LEVELS: &[(&str, &str)] = &[
    ("Off", "Off"),
    ("Error", "Error"),
    ("Warn", "Warn"),
    ("Info", "Info"),
    ("Debug", "Debug"),
    ("Trace", "Trace"),
];

/// The eight colors of a palette at `paths`, under the section `section`.
const fn palette(paths: [&'static str; 8], section: &'static str, auto: bool) -> [Key; 8] {
    const NAMES: [&str; 8] =
        ["Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White"];
    let description = if auto { "Empty: worked out from the normal color" } else { "" };
    const fn color(
        path: &'static str,
        name: &'static str,
        section: &'static str,
        description: &'static str,
    ) -> Key {
        key(Area::Colors, section, path, name, description, Kind::Color { cells: false })
    }
    [
        color(paths[0], NAMES[0], section, description),
        color(paths[1], NAMES[1], section, description),
        color(paths[2], NAMES[2], section, description),
        color(paths[3], NAMES[3], section, description),
        color(paths[4], NAMES[4], section, description),
        color(paths[5], NAMES[5], section, description),
        color(paths[6], NAMES[6], section, description),
        color(paths[7], NAMES[7], section, description),
    ]
}

const NORMAL: [Key; 8] = palette(
    [
        "colors.normal.black",
        "colors.normal.red",
        "colors.normal.green",
        "colors.normal.yellow",
        "colors.normal.blue",
        "colors.normal.magenta",
        "colors.normal.cyan",
        "colors.normal.white",
    ],
    "Normal colors",
    false,
);
const BRIGHT: [Key; 8] = palette(
    [
        "colors.bright.black",
        "colors.bright.red",
        "colors.bright.green",
        "colors.bright.yellow",
        "colors.bright.blue",
        "colors.bright.magenta",
        "colors.bright.cyan",
        "colors.bright.white",
    ],
    "Bright colors",
    false,
);
const DIM: [Key; 8] = palette(
    [
        "colors.dim.black",
        "colors.dim.red",
        "colors.dim.green",
        "colors.dim.yellow",
        "colors.dim.blue",
        "colors.dim.magenta",
        "colors.dim.cyan",
        "colors.dim.white",
    ],
    "Dim colors",
    true,
);

use Area::{Advanced, Colors, Cursor, Font, General, Terminal, Window};

/// Every key the settings edit, in the order they are shown.
#[rustfmt::skip]
pub const KEYS: &[Key] = &[
    // General.
    key(General, "Startup", "window.startup_mode", "Startup mode", "How new windows open", Kind::Choice(STARTUP_MODES)),
    key(General, "Startup", "window.dimensions", "Window size", "Columns and lines; empty lets the system choose", Kind::Pair(["columns", "lines"], optional(1., 1000.))),
    key(General, "Startup", "window.position", "Window position", "Pixels from the top left; not on Wayland", Kind::Pair(["x", "y"], Number { min: -100_000., max: 100_000., ..optional(0., 0.) })),
    key(General, "Startup", "general.working_directory", "Working directory", "Of new shells; empty keeps the one started from", Kind::Text("Inherited")),
    key(General, "Behaviour", "window.dynamic_title", "Programs set titles", "Programs may rename their tabs", Kind::Switch),
    key(General, "Configuration", "general.live_config_reload", "Reload on change", "Apply edits to alacritty.toml at once", Kind::Switch),
    key(General, "Configuration", "general.ipc_socket", "IPC socket", "Lets alacritty msg reach running windows", Kind::Switch).only(Only::Unix),
    key(General, "Configuration", "general.import", "Imports", "Files read first, one on each line; this file wins", Kind::List),
    key(General, "Environment", "env", "Environment variables", "NAME=value, one on each line, for every shell", Kind::Map),
    // Window.
    key(Window, "Window", "window.title", "Title", "Of windows whose programs set none", Kind::Text("Alacritty")),
    key(Window, "Window", "window.class.instance", "Class instance", "WM_CLASS instance; unused on Wayland", Kind::Text("Alacritty")).only(Only::Free),
    key(Window, "Window", "window.class.general", "Class", "WM_CLASS class, or the Wayland app ID", Kind::Text("Alacritty")).only(Only::Free),
    key(Window, "Window", "window.level", "Level", "Whether windows stay above others", Kind::Choice(&[("Normal", "Normal"), ("AlwaysOnTop", "On top")])),
    key(Window, "Window", "window.decorations", "Decorations", "The system's title bar, used while tabs are off", Kind::Choice(&[("Full", "Full"), ("None", "None"), ("Transparent", "Clear"), ("Buttonless", "No buttons")])),
    key(Window, "Window", "window.decorations_theme_variant", "Decorations theme", "Light or dark system decorations", Kind::Choice(&[("", "System"), ("Light", "Light"), ("Dark", "Dark")])),
    key(Window, "Window", "window.resize_increments", "Resize by cells", "Windows grow and shrink a cell at a time", Kind::Switch),
    key(Window, "Background", "window.blur", "Background blur", "Behind a see-through background; macOS and KDE", Kind::Switch),
    key(Window, "Background", "colors.transparent_background_colors", "See-through cell colors", "The opacity also applies to colored cells", Kind::Switch),
    key(Window, "Padding", "window.padding", "Padding", "Around the text, across and down, in pixels", Kind::Pair(["x", "y"], whole(0., 500., 1.))),
    key(Window, "Padding", "window.dynamic_padding", "Spread extra padding", "Share leftover space evenly around the text", Kind::Switch),
    // Font.
    key(Font, "Font", "font.normal", "Font", "Family and style of the terminal's text", Kind::Face("Regular")),
    key(Font, "Font", "font.size", "Font size", "In points", Kind::Number(Number { min: 1., max: 200., step: 1., whole: false, optional: false })),
    key(Font, "Styles", "font.bold", "Bold", "Empty family: the font's own", Kind::Face("Bold")),
    key(Font, "Styles", "font.italic", "Italic", "Empty family: the font's own", Kind::Face("Italic")),
    key(Font, "Styles", "font.bold_italic", "Bold italic", "Empty family: the font's own", Kind::Face("Bold Italic")),
    key(Font, "Spacing", "font.offset", "Cell spacing", "Extra pixels between letters and between lines", Kind::Pair(["x", "y"], whole(-50., 100., 1.))),
    key(Font, "Spacing", "font.glyph_offset", "Glyph offset", "Moves each letter in its cell, in pixels", Kind::Pair(["x", "y"], whole(-50., 100., 1.))),
    key(Font, "Rendering", "font.builtin_box_drawing", "Built-in box drawing", "Lines and blocks drawn, not the font's", Kind::Switch),
    key(Font, "Rendering", "colors.draw_bold_text_with_bright_colors", "Bold in bright colors", "Bold text takes the bright colors", Kind::Switch),
    // Colors.
    key(Colors, "Primary", "colors.primary.foreground", "Foreground", "Of the text", Kind::Color { cells: false }),
    key(Colors, "Primary", "colors.primary.background", "Background", "Behind the text", Kind::Color { cells: false }),
    key(Colors, "Primary", "colors.primary.dim_foreground", "Dim foreground", "Empty: worked out from the foreground", Kind::Color { cells: false }),
    key(Colors, "Primary", "colors.primary.bright_foreground", "Bright foreground", "Of bold text; empty: the foreground", Kind::Color { cells: false }).needs("colors.draw_bold_text_with_bright_colors"),
    NORMAL[0], NORMAL[1], NORMAL[2], NORMAL[3], NORMAL[4], NORMAL[5], NORMAL[6], NORMAL[7],
    BRIGHT[0], BRIGHT[1], BRIGHT[2], BRIGHT[3], BRIGHT[4], BRIGHT[5], BRIGHT[6], BRIGHT[7],
    DIM[0], DIM[1], DIM[2], DIM[3], DIM[4], DIM[5], DIM[6], DIM[7],
    key(Colors, "Cursor and selection", "colors.cursor", "Cursor", "Text under it, and the cursor", Kind::Colors(["text", "cursor"], true)),
    key(Colors, "Cursor and selection", "colors.vi_mode_cursor", "Vi mode cursor", "Text under it, and the cursor", Kind::Colors(["text", "cursor"], true)),
    key(Colors, "Cursor and selection", "colors.selection", "Selection", "Selected text, and behind it", Kind::Colors(["text", "background"], true)),
    key(Colors, "Search and hints", "colors.search.matches", "Search matches", "Text, and behind it", Kind::Colors(["foreground", "background"], true)),
    key(Colors, "Search and hints", "colors.search.focused_match", "Focused match", "Text, and behind it", Kind::Colors(["foreground", "background"], true)),
    key(Colors, "Search and hints", "colors.hints.start", "Hint start", "The first letter of a hint's label", Kind::Colors(["foreground", "background"], true)),
    key(Colors, "Search and hints", "colors.hints.end", "Hint end", "The rest of a hint's label", Kind::Colors(["foreground", "background"], true)),
    key(Colors, "Bars", "colors.line_indicator", "Line indicator", "Place in history; empty: the opposite primary", Kind::Colors(["foreground", "background"], false)),
    key(Colors, "Bars", "colors.footer_bar", "Footer bar", "Search and link bar; empty: the opposite primary", Kind::Colors(["foreground", "background"], false)),
    // Cursor.
    key(Cursor, "Cursor", "cursor.style.shape", "Shape", "Programs may change it", Kind::Choice(SHAPES)),
    key(Cursor, "Cursor", "cursor.style.blinking", "Blinking", "Off and On let programs change it", Kind::Choice(BLINKING)),
    key(Cursor, "Cursor", "cursor.thickness", "Thickness", "Of beams, underlines and outlines, from 0 to 1", Kind::Number(Number { min: 0., max: 1., step: 0.05, whole: false, optional: false })),
    key(Cursor, "Cursor", "cursor.unfocused_hollow", "Hollow when unfocused", "An outline while the window is in the back", Kind::Switch),
    key(Cursor, "Blinking", "cursor.blink_interval", "Blink interval", "Milliseconds between blinks", Kind::Number(whole(10., 10_000., 50.))),
    key(Cursor, "Blinking", "cursor.blink_timeout", "Stop blinking after", "Seconds without input; 0 blinks on", Kind::Number(whole(0., 255., 1.))),
    key(Cursor, "Vi mode", "cursor.vi_mode_style.shape", "Vi mode shape", "Of the cursor in vi mode", Kind::Choice(&[("", "Cursor's"), ("Block", "Block"), ("Underline", "Underline"), ("Beam", "Beam")])),
    key(Cursor, "Vi mode", "cursor.vi_mode_style.blinking", "Vi mode blinking", "Of the cursor in vi mode", Kind::Choice(BLINKING)).needs("cursor.vi_mode_style.shape"),
    // Terminal.
    key(Terminal, "Shell", "terminal.shell", "Shell", "Command line of new shells; profiles can set their own", Kind::Command),
    key(Terminal, "Scrolling", "scrolling.history", "Scrollback", "Lines of history, up to 100000", Kind::Number(whole(0., 100_000., 1000.))),
    key(Terminal, "Scrolling", "scrolling.multiplier", "Scroll speed", "Lines for each step of the wheel", Kind::Number(whole(0., 100., 1.))),
    key(Terminal, "Selection", "selection.save_to_clipboard", "Copy on select", "Selected text goes to the clipboard", Kind::Switch),
    key(Terminal, "Selection", "selection.semantic_escape_chars", "Word separators", "End a double-click selection", Kind::Text(",│`|:\"' ()[]{}<>")),
    key(Terminal, "Selection", "terminal.osc52", "Clipboard access", "What programs may do with OSC 52", Kind::Choice(&[("Disabled", "None"), ("OnlyCopy", "Copy"), ("OnlyPaste", "Paste"), ("CopyPaste", "Both")])),
    key(Terminal, "Mouse and keyboard", "mouse.hide_when_typing", "Hide pointer while typing", "It comes back when moved", Kind::Switch),
    key(Terminal, "Mouse and keyboard", "window.option_as_alt", "Option as Alt", "Which Option keys type Alt", Kind::Choice(&[("None", "None"), ("OnlyLeft", "Left"), ("OnlyRight", "Right"), ("Both", "Both")])).only(Only::MacOs),
    key(Terminal, "Mouse and keyboard", "hints.alphabet", "Hint keys", "Letters that label hints", Kind::Text("jfkdls;ahgurieowpq")),
    key(Terminal, "Bell", "bell.duration", "Visual bell", "Milliseconds it flashes; 0 for none", Kind::Number(whole(0., 10_000., 50.))),
    key(Terminal, "Bell", "bell.animation", "Animation", "How the flash fades", Kind::Choice(ANIMATIONS)).needs("bell.duration"),
    key(Terminal, "Bell", "bell.color", "Flash color", "Of the visual bell", Kind::Color { cells: false }).needs("bell.duration"),
    key(Terminal, "Bell", "bell.command", "Bell command", "Run at each bell; empty runs none", Kind::Command),
    // Advanced.
    key(Advanced, "Rendering", "debug.renderer", "Renderer", "Empty picks the best the system offers", Kind::Choice(&[("", "Auto"), ("glsl3", "GLSL 3"), ("gles2", "GLES 2"), ("gles2pure", "Pure GLES 2")])),
    key(Advanced, "Rendering", "debug.prefer_egl", "Prefer EGL", "Over the system's own OpenGL", Kind::Switch),
    key(Advanced, "Rendering", "debug.render_timer", "Render timer", "Show how long each frame takes", Kind::Switch),
    key(Advanced, "Rendering", "debug.highlight_damage", "Highlight damage", "Show what each frame redraws", Kind::Switch),
    key(Advanced, "Logging", "debug.log_level", "Log level", "What Alacritty writes to its log", Kind::Choice(LOG_LEVELS)),
    key(Advanced, "Logging", "debug.persistent_logging", "Keep log file", "After Alacritty quits", Kind::Switch),
    key(Advanced, "Logging", "debug.print_events", "Log events", "Every window event, at Info", Kind::Switch),
];

/// The index of the key at `path`.
pub fn index(path: &str) -> Option<usize> {
    KEYS.iter().position(|key| key.path == path)
}

/// What the settings edit for a key: whether a switch is on, or the text of its fields, the
/// first holding a choice's name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub on: bool,
    pub text: [String; 2],
}

impl State {
    /// Whether the key is on, or set to something but nothing.
    pub fn set(&self) -> bool {
        self.on || !matches!(self.text[0].trim(), "" | "0")
    }
}

/// Every key's state in the configuration as loaded.
pub fn read(config: &UiConfig) -> Vec<State> {
    let root = toml::Value::try_from(config).unwrap_or_else(|_| toml::Table::new().into());
    KEYS.iter().map(|key| key.read(&root)).collect()
}

/// Set the keys whose state in `edited` differs from `saved`, or tell the first one wrong and
/// what is wrong with it.
pub fn edit(edited: &[State], saved: &[State], root: &mut Item) -> Result<(), (usize, String)> {
    for (index, key) in KEYS.iter().enumerate() {
        let (Some(edited), Some(saved)) = (edited.get(index), saved.get(index)) else { break };
        if edited != saved {
            key.write(edited, saved, root).map_err(|error| (index, error))?;
        }
    }
    Ok(())
}

/// The first key whose state in `edited` cannot be written, and what is wrong with it.
pub fn check(edited: &[State], saved: &[State]) -> Result<(), (usize, String)> {
    edit(edited, saved, &mut Item::Table(Table::new()))
}

/// The key at `path` in the configuration as it serializes, which spells a few keys otherwise.
fn get<'a>(root: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    let path = match path.rsplit_once('.') {
        Some(("window", "title")) => "window.identity.title".into(),
        Some(("window.class", part)) => format!("window.identity.class.{part}"),
        // Cell colors serialize their text and cursor as foreground and background.
        Some((table @ ("colors.cursor" | "colors.vi_mode_cursor" | "colors.selection"), part)) => {
            let part = match part {
                "text" => "foreground",
                "cursor" => "background",
                part => part,
            };
            format!("{table}.{part}")
        },
        _ => path.to_owned(),
    };
    let mut value = root;
    let mut keys = path.split('.').peekable();
    while let Some(key) = keys.next() {
        value = match value {
            // A cursor style written as its shape alone.
            toml::Value::String(_) if keys.peek().is_none() && key == "shape" => {
                return Some(value);
            },
            toml::Value::String(_) if keys.peek().is_none() && key == "blinking" => return None,
            value => value.get(key)?,
        };
    }
    Some(value)
}

/// A value as its field shows it.
fn show(value: Option<&toml::Value>) -> String {
    match value {
        Some(toml::Value::String(text)) => text.clone(),
        Some(toml::Value::Integer(number)) => number.to_string(),
        Some(toml::Value::Float(number)) => format(*number as f32),
        Some(toml::Value::Boolean(on)) => on.to_string(),
        _ => String::new(),
    }
}

impl Key {
    fn read(&self, root: &toml::Value) -> State {
        let at = |part: &str| get(root, &format!("{}.{part}", self.path));
        let mut state = State::default();
        match self.kind {
            Kind::Switch => {
                state.on = get(root, self.path).and_then(toml::Value::as_bool) == Some(true)
            },
            Kind::Choice(choices) => {
                let name = show(get(root, self.path));
                let name = match self.path {
                    "cursor.style.blinking" if name.is_empty() => "Off".into(),
                    _ => name,
                };
                // The configuration reads names in any case, and writes some in its own.
                let known = choices.iter().find(|(choice, _)| choice.eq_ignore_ascii_case(&name));
                state.text[0] = known.map_or(name, |(choice, _)| (*choice).into());
            },
            Kind::Number(_) | Kind::Text(_) | Kind::Color { .. } => {
                state.text[0] = show(get(root, self.path))
            },
            Kind::Pair(parts, number) => {
                state.text = parts.map(|part| show(at(part)));
                // The window's size is unset at zero.
                if number.optional && state.text.iter().all(|text| text == "0") {
                    state.text = Default::default();
                }
            },
            Kind::Colors(parts, _) => state.text = parts.map(|part| show(at(part))),
            Kind::Face(_) => state.text = ["family", "style"].map(|part| show(at(part))),
            Kind::Command => {
                state.text[0] = match get(root, self.path) {
                    Some(toml::Value::String(program)) => join(program, &[]),
                    Some(command) => {
                        let program = show(command.get("program"));
                        let args = command.get("args").and_then(toml::Value::as_array);
                        let args: Vec<_> =
                            args.into_iter().flatten().map(|arg| show(Some(arg))).collect();
                        join(&program, &args)
                    },
                    None => String::new(),
                }
            },
            Kind::List => {
                let items = get(root, self.path).and_then(toml::Value::as_array);
                let items: Vec<_> =
                    items.into_iter().flatten().map(|item| show(Some(item))).collect();
                state.text[0] = items.join("\n");
            },
            Kind::Map => {
                let table = get(root, self.path).and_then(toml::Value::as_table);
                let mut lines: Vec<_> = table
                    .into_iter()
                    .flatten()
                    .map(|(name, value)| format!("{name}={}", show(Some(value))))
                    .collect();
                lines.sort();
                state.text[0] = lines.join("\n");
            },
        }
        state
    }

    /// Set the key to `state`, changed from `saved`, or tell what is wrong with it.
    fn write(&self, state: &State, saved: &State, root: &mut Item) -> Result<(), String> {
        let path: Vec<&str> = self.path.split('.').collect();
        let below = |part: &'static str| [&path[..], &[part]].concat();
        let changed = |part: usize| state.text[part] != saved.text[part];
        match self.kind {
            Kind::Switch => set(root, &path, value(state.on)),
            Kind::Choice(_) if self.path.starts_with("cursor.") => self.write_style(state, root),
            Kind::Choice(_) => match state.text[0].as_str() {
                "" => remove(root, &path),
                name => set(root, &path, value(name)),
            },
            Kind::Number(number) => match self.number(&state.text[0], number, "")? {
                Some(item) => set(root, &path, item),
                None => remove(root, &path),
            },
            Kind::Pair(parts, number) => {
                let first = self.number(&state.text[0], number, parts[0])?;
                let second = self.number(&state.text[1], number, parts[1])?;
                match (first, second) {
                    (None, None) => remove(root, &path),
                    (Some(first), Some(second)) => {
                        for (part, item) in [first, second].into_iter().enumerate() {
                            if changed(part) || number.optional {
                                set(root, &below(parts[part]), item);
                            }
                        }
                    },
                    _ => return Err(format!("{} needs both of its numbers", self.name)),
                }
            },
            Kind::Text(_) => match state.text[0].as_str() {
                "" => remove(root, &path),
                text => set(root, &path, value(text)),
            },
            Kind::Face(_) => {
                for (part, name) in ["family", "style"].into_iter().enumerate() {
                    match state.text[part].trim() {
                        _ if !changed(part) => (),
                        "" => remove(root, &below(name)),
                        text => set(root, &below(name), value(text)),
                    }
                }
            },
            Kind::Color { cells } => match self.color(&state.text[0], cells)? {
                Some(color) => set(root, &path, value(color)),
                None => remove(root, &path),
            },
            Kind::Colors(parts, cells) => {
                for (part, name) in parts.into_iter().enumerate() {
                    match self.color(&state.text[part], cells)? {
                        _ if !changed(part) => (),
                        Some(color) => set(root, &below(name), value(color)),
                        None => remove(root, &below(name)),
                    }
                }
            },
            Kind::Command => match profiles::split(&state.text[0]) {
                None => remove(root, &path),
                Some((program, args)) if args.is_empty() => set(root, &path, value(program)),
                Some((program, args)) => {
                    let mut command = InlineTable::new();
                    command.insert("program", program.into());
                    command.insert("args", Value::Array(args.iter().collect()));
                    set(root, &path, value(command));
                },
            },
            Kind::List => {
                let items: Array = lines(&state.text[0]).collect();
                match items.is_empty() {
                    true => remove(root, &path),
                    false => set(root, &path, value(items)),
                }
            },
            Kind::Map => {
                let mut entries = Vec::new();
                for line in lines(&state.text[0]) {
                    match line.split_once('=') {
                        Some((name, text)) if !name.trim().is_empty() => {
                            entries.push((name.trim(), text))
                        },
                        _ => return Err(format!("{}: write NAME=value on each line", self.name)),
                    }
                }
                write_map(root, &path, &entries);
            },
        }
        Ok(())
    }

    /// The number `text` of a field, none when it is empty and may be.
    fn number(&self, text: &str, number: Number, part: &str) -> Result<Option<Item>, String> {
        let text = text.trim();
        if text.is_empty() && number.optional {
            return Ok(None);
        }
        let (low, high) = (number.min, number.max);
        let fits = |value: &f64| {
            let whole = !number.whole || value.fract() == 0.;
            (f64::from(low)..=f64::from(high)).contains(value) && whole
        };
        match text.parse::<f64>().ok().filter(fits) {
            Some(value) if number.whole => Ok(Some(toml_edit::value(value as i64))),
            Some(value) => Ok(Some(super::number(value as f32))),
            None => {
                let name = match part {
                    "" => self.name.into(),
                    part => format!("{} ({part})", self.name),
                };
                let kind = if number.whole { "a whole number" } else { "a number" };
                Err(format!("{name} must be {kind} from {low} to {high}"))
            },
        }
    }

    /// The color `text` of a field, none when it is empty.
    fn color(&self, text: &str, cells: bool) -> Result<Option<String>, String> {
        let text = text.trim();
        let cell = ["CellForeground", "CellBackground"]
            .into_iter()
            .find(|cell| cell.eq_ignore_ascii_case(text))
            .filter(|_| cells);
        match (text, cell) {
            ("", _) => Ok(None),
            (_, Some(cell)) => Ok(Some(cell.into())),
            _ => match parse_color(text) {
                Some(color) => Ok(Some(color)),
                None if cells => {
                    Err(format!("{}: write #RRGGBB, CellForeground or CellBackground", self.name))
                },
                None => Err(format!("{}: write a color as #RRGGBB", self.name)),
            },
        }
    }

    /// Set a cursor style's shape or blinking, which its style holds as a table or, for the
    /// shape alone, as its name.
    fn write_style(&self, state: &State, root: &mut Item) {
        let Some((style, part)) = self.path.rsplit_once('.') else { return };
        let style: Vec<&str> = style.split('.').collect();
        let name = state.text[0].as_str();
        let current = style.iter().try_fold(&*root, |item, key| item.get(key));
        match current {
            // An empty shape takes the cursor's style.
            _ if name.is_empty() => remove(root, &style),
            // The name alone keeps being one while only the shape changes.
            Some(Item::Value(Value::String(_))) if part == "shape" => {
                set(root, &style, value(name))
            },
            Some(Item::Value(Value::String(shape))) => {
                let mut table = InlineTable::new();
                table.insert("shape", shape.value().as_str().into());
                table.insert(part, name.into());
                set(root, &style, value(table));
            },
            _ => set(root, &[&style[..], &[part]].concat(), value(name)),
        }
    }
}

/// A color written as `#rrggbb`, from that or `0xrrggbb`.
pub fn parse_color(text: &str) -> Option<String> {
    let digits = text.strip_prefix('#').or_else(|| text.strip_prefix("0x"))?;
    let valid = digits.len() == 6 && digits.chars().all(|c| c.is_ascii_hexdigit());
    valid.then(|| format!("#{}", digits.to_lowercase()))
}

/// The lines of a field with text, without their spaces around.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|line| !line.is_empty())
}

/// A command line of `program` and `args`, quoted as [`profiles::split`] reads it back.
fn join(program: &str, args: &[String]) -> String {
    let quote = |word: &str| {
        let plain = !word.is_empty()
            && !word.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"');
        match plain || cfg!(windows) {
            true => word.to_owned(),
            false if word.contains('\'') => format!("\"{word}\""),
            false => format!("'{word}'"),
        }
    };
    let program = match cfg!(windows) && program.contains(' ') {
        true => format!("\"{program}\""),
        false => quote(program),
    };
    let words = std::iter::once(program).chain(args.iter().map(|arg| quote(arg)));
    words.collect::<Vec<_>>().join(" ")
}

/// Make the table at `path` hold `entries`, keeping the entries and comments it already has for
/// them; an empty one is removed.
fn write_map(root: &mut Item, path: &[&str], entries: &[(&str, &str)]) {
    if entries.is_empty() {
        return remove(root, path);
    }
    let table = path.iter().try_fold(&mut *root, |item, key| item.get_mut(key));
    match table.and_then(Item::as_table_like_mut) {
        Some(table) => {
            let stale: Vec<String> = table
                .iter()
                .map(|(name, _)| name.to_owned())
                .filter(|name| entries.iter().all(|(entry, _)| entry != name))
                .collect();
            for name in stale {
                table.remove(&name);
            }
        },
        None => set(root, path, section()),
    }
    let Some(table) = path.iter().try_fold(&mut *root, |item, key| item.get_mut(key)) else {
        return;
    };
    for (name, text) in entries {
        // A name with dots in it is one key.
        set(table, &[name], value(*text));
    }
}

/// Set the key at `path` to `item`, keeping the comments of a value it replaces.
///
/// Tables missing on the way are made as the file writes their parents: a section under a
/// section, a dotted key under a dotted one, and an inline table inside an inline table.
fn set(root: &mut Item, path: &[&str], mut item: Item) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut table = root;
    for key in parents {
        let child = match &*table {
            Item::Table(table) if table.is_dotted() => {
                let mut dotted = Table::new();
                dotted.set_dotted(true);
                Item::Table(dotted)
            },
            Item::Table(_) => section(),
            _ => value(InlineTable::new()),
        };
        let Some(parent) = table.as_table_like_mut() else { return };
        let entry = parent.entry(key).or_insert(Item::None);
        if entry.as_table_like().is_none() {
            *entry = child;
        }
        table = entry;
    }
    // A table set inside an inline table is inline too.
    if table.is_inline_table() {
        if let Item::Table(section) = item {
            item = value(section.into_inline_table());
        }
    }
    // A key added to an inline table goes before the space that closes it.
    if let (Some(inline), Item::Value(item)) = (table.as_inline_table_mut(), &mut item) {
        if !inline.contains_key(last) {
            if let Some((_, value)) = inline.iter_mut().last() {
                let suffix = value.decor().suffix().cloned();
                value.decor_mut().set_suffix("");
                item.decor_mut().set_prefix(" ");
                item.decor_mut().set_suffix(suffix.unwrap_or_default());
            }
        }
    }
    let Some(table) = table.as_table_like_mut() else { return };
    match (table.get_mut(last), item) {
        (Some(Item::Value(old)), Item::Value(mut new)) => {
            *new.decor_mut() = old.decor().clone();
            *old = new;
        },
        (Some(old), item) => *old = item,
        (None, item) => {
            table.insert(last, item);
        },
    }
}

/// Remove the key at `path`, with the tables it leaves empty but for those with comments.
fn remove(root: &mut Item, path: &[&str]) {
    let Some(table) = root.as_table_like_mut() else { return };
    match path {
        [] => (),
        [key] => {
            table.remove(key);
        },
        [key, rest @ ..] => {
            let Some(child) = table.get_mut(key) else { return };
            remove(child, rest);
            let empty = match child {
                Item::Table(child) => child.is_empty() && !commented(child.decor()),
                child => child.as_table_like().is_some_and(|child| child.is_empty()),
            };
            if empty {
                table.remove(key);
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use toml_edit::DocumentMut;

    use super::*;

    /// `text` with the keys at their paths given the fields' texts, or switched over.
    fn edit_keys(text: &str, changes: &[(&str, [&str; 2])]) -> Result<String, (usize, String)> {
        let saved = read(&read_config(text));
        let mut edited = saved.clone();
        for (path, texts) in changes {
            let index = index(path).unwrap_or_else(|| panic!("{path}"));
            match KEYS[index].kind {
                Kind::Switch => edited[index].on ^= true,
                _ => edited[index].text = texts.map(String::from),
            }
        }
        let mut document: DocumentMut = text.parse().unwrap();
        edit(&edited, &saved, document.as_item_mut())?;
        Ok(document.to_string())
    }

    fn read_config(text: &str) -> UiConfig {
        let value: toml::Value = toml::from_str(text).unwrap();
        UiConfig::deserialize(value).unwrap()
    }

    fn state(config: &UiConfig, path: &str) -> State {
        read(config)[index(path).unwrap()].clone()
    }

    #[test]
    fn keys_are_unique_and_documented() {
        let manual = include_str!("../../../../../extra/man/alacritty.5.scd");
        for (index, key) in KEYS.iter().enumerate() {
            assert!(KEYS[index + 1..].iter().all(|other| other.path != key.path), "{}", key.path);
            let last = key.path.rsplit('.').next().unwrap();
            assert!(manual.contains(last), "{} is not in alacritty(5)", key.path);
            if let Some(needs) = key.needs {
                assert!(super::index(needs).is_some(), "{needs}");
            }
        }
    }

    #[test]
    fn defaults_read_as_the_configuration_has_them() {
        let config = UiConfig::default();
        let empty = [String::new(), String::new()];
        assert_eq!(state(&config, "font.size").text[0], "11.25");
        assert_eq!(state(&config, "window.title").text[0], "Alacritty");
        assert_eq!(state(&config, "window.dimensions").text, empty);
        assert_eq!(state(&config, "cursor.style.shape").text[0], "Block");
        assert_eq!(state(&config, "cursor.style.blinking").text[0], "Off");
        assert_eq!(state(&config, "cursor.vi_mode_style.shape").text[0], "");
        assert_eq!(state(&config, "cursor.thickness").text[0], "0.15");
        assert_eq!(state(&config, "terminal.osc52").text[0], "OnlyCopy");
        assert_eq!(state(&config, "debug.log_level").text[0], "Warn");
        assert_eq!(state(&config, "debug.renderer").text[0], "");
        assert_eq!(state(&config, "colors.cursor").text, ["CellBackground", "CellForeground"]);
        assert_eq!(state(&config, "colors.selection").text, ["CellBackground", "CellForeground"]);
        assert_eq!(state(&config, "colors.normal.red").text[0], "#ac4242");
        assert_eq!(state(&config, "colors.dim.red").text[0], "");
        assert!(state(&config, "general.live_config_reload").on);
        // Every choice read is one the settings offer.
        for (key, state) in KEYS.iter().zip(read(&config)) {
            if let Kind::Choice(choices) = key.kind {
                let known = choices.iter().any(|(name, _)| *name == state.text[0]);
                assert!(known || key.needs.is_some(), "{}: {:?}", key.path, state.text[0]);
            }
        }
    }

    #[test]
    fn keys_are_written_where_the_configuration_reads_them() {
        let changes: &[(&str, [&str; 2])] = &[
            ("window.startup_mode", ["Maximized", ""]),
            ("window.dimensions", ["120", "40"]),
            ("window.title", ["Term", ""]),
            ("window.level", ["AlwaysOnTop", ""]),
            ("window.decorations_theme_variant", ["Dark", ""]),
            ("window.padding", ["6", "4"]),
            ("window.dynamic_padding", ["", ""]),
            ("font.normal", ["Iosevka", "Medium"]),
            ("font.bold", ["", "Heavy"]),
            ("font.size", ["13.5", ""]),
            ("font.offset", ["1", "3"]),
            ("colors.primary.background", ["#101010", ""]),
            ("colors.primary.dim_foreground", ["#808080", ""]),
            ("colors.cursor", ["CellBackground", "#ff0000"]),
            ("colors.selection", ["#000000", "cellforeground"]),
            ("colors.hints.start", ["#111111", "#222222"]),
            ("cursor.style.shape", ["Beam", ""]),
            ("cursor.style.blinking", ["Always", ""]),
            ("cursor.vi_mode_style.shape", ["Underline", ""]),
            ("cursor.thickness", ["0.3", ""]),
            ("cursor.blink_interval", ["500", ""]),
            ("terminal.shell", ["zsh -l -c 'tmux new'", ""]),
            ("terminal.osc52", ["CopyPaste", ""]),
            ("scrolling.history", ["50000", ""]),
            ("selection.save_to_clipboard", ["", ""]),
            ("bell.duration", ["100", ""]),
            ("bell.animation", ["EaseOutExpo", ""]),
            ("bell.command", ["notify-send Bell", ""]),
            ("debug.renderer", ["gles2", ""]),
            ("debug.log_level", ["Info", ""]),
            ("general.import", ["~/a.toml\n\n  ~/b.toml ", ""]),
            ("env", ["TERM=xterm-256color\nA=b=c", ""]),
        ];
        let edited = edit_keys("", changes).unwrap();
        let config = read_config(&edited);
        let (states, defaults) = (read(&config), read(&UiConfig::default()));
        for (path, texts) in changes {
            let index = index(path).unwrap();
            let state = &states[index];
            match (KEYS[index].kind, *path) {
                (Kind::Switch, _) => assert_ne!(state.on, defaults[index].on, "{path}"),
                (_, "general.import") => assert_eq!(state.text[0], "~/a.toml\n~/b.toml"),
                (_, "env") => assert_eq!(state.text[0], "A=b=c\nTERM=xterm-256color"),
                (_, "colors.selection") => assert_eq!(state.text, ["#000000", "CellForeground"]),
                _ => assert_eq!(state.text, texts.map(String::from), "{path}\n{edited}"),
            }
        }
        assert_eq!(config.window.dimensions().map(|d| (d.columns, d.lines)), Some((120, 40)));
        assert_eq!(config.terminal.shell.as_ref().map(|shell| shell.args().len()), Some(3));
        let style = "[cursor.style]\nshape = \"Beam\"\nblinking = \"Always\"\n";
        assert!(edited.contains(style), "{edited}");
        assert!(edited.contains("[env]\nTERM = \"xterm-256color\"\nA = \"b=c\"\n"), "{edited}");
    }

    #[test]
    fn unchanged_keys_leave_the_file_alone() {
        let text =
            "# Mine.\n[font]\nsize = 12 # Big.\n\n[colors.primary]\nbackground = \"0x000000\"\n";
        assert_eq!(edit_keys(text, &[]).unwrap(), text);
    }

    #[test]
    fn values_keep_their_comments_and_places() {
        let text = "# Mine.\n[font]\nsize = 12 # Big.\nnormal = { family = \"Hack\", style = \
                    \"Regular\" }\n\n[window]\npadding.x = 2\n";
        let edited = edit_keys(text, &[
            ("font.size", ["14", ""]),
            ("font.normal", ["Iosevka", "Regular"]),
            ("window.padding", ["8", "8"]),
        ])
        .unwrap();
        assert_eq!(
            edited,
            "# Mine.\n[font]\nsize = 14 # Big.\nnormal = { family = \"Iosevka\", style = \
             \"Regular\" }\n\n[window]\npadding.x = 8\npadding.y = 8\n"
        );
    }

    #[test]
    fn cursor_style_keeps_its_form() {
        let text = "[cursor]\nstyle = \"Underline\" # Mine.\n";
        let edited = edit_keys(text, &[("cursor.style.shape", ["Beam", ""])]).unwrap();
        assert_eq!(edited, "[cursor]\nstyle = \"Beam\" # Mine.\n");
        // Blinking needs the table, which takes the name's place.
        let edited = edit_keys(text, &[("cursor.style.blinking", ["On", ""])]).unwrap();
        let table = "[cursor]\nstyle = { shape = \"Underline\", blinking = \"On\" } # Mine.\n";
        assert_eq!(edited, table);
        assert_eq!(state(&read_config(&edited), "cursor.style.blinking").text[0], "On");
        // The vi mode taking the cursor's style leaves its table out.
        let text = "[cursor.vi_mode_style]\nshape = \"Beam\"\n";
        assert_eq!(edit_keys(text, &[("cursor.vi_mode_style.shape", ["", ""])]).unwrap(), "");
    }

    #[test]
    fn emptied_keys_are_removed() {
        let text = "live_config_reload = false\n\n[window]\nopacity = 1\ntitle = \
                    \"Mine\"\n\n[window.dimensions]\ncolumns = 100\nlines = \
                    30\n\n[font.normal]\nfamily = \"Hack\"\n\n[env]\nA = \"1\" # Kept.\nB = \
                    \"2\"\n";
        let edited = edit_keys(text, &[
            ("window.dimensions", ["", ""]),
            ("window.title", ["", ""]),
            ("font.normal", ["", ""]),
            ("env", ["A=1", ""]),
        ])
        .unwrap();
        let expected =
            "live_config_reload = false\n\n[window]\nopacity = 1\n\n[env]\nA = \"1\" # Kept.\n";
        assert_eq!(edited, expected);
    }

    #[test]
    fn wrong_values_are_told() {
        let wrong = |path: &str, texts: [&str; 2]| edit_keys("", &[(path, texts)]).unwrap_err();
        assert_eq!(wrong("font.size", ["big", ""]).0, index("font.size").unwrap());
        assert!(wrong("scrolling.history", ["2.5", ""]).1.contains("whole number"));
        assert!(wrong("window.dimensions", ["80", ""]).1.contains("both"));
        assert!(wrong("colors.primary.foreground", ["red", ""]).1.contains("#RRGGBB"));
        assert!(wrong("colors.primary.foreground", ["CellForeground", ""]).1.contains("#RRGGBB"));
        assert!(wrong("env", ["NOEQUALS", ""]).1.contains("NAME=value"));
        assert_eq!(parse_color("0xABCDEF").as_deref(), Some("#abcdef"));
        assert_eq!(parse_color("#abc"), None);
    }

    #[test]
    fn missing_tables_follow_their_parents() {
        let edited = edit_keys("font.size = 12\nwindow = { opacity = 1 }\n", &[
            ("font.offset", ["0", "2"]),
            ("window.blur", ["", ""]),
        ])
        .unwrap();
        let expected = "font.size = 12\nfont.offset.y = 2\nwindow = { opacity = 1, blur = true }\n";
        assert_eq!(edited, expected);
    }

    #[test]
    fn commands_read_back_as_written() {
        for line in ["zsh", "zsh -l", "bash -c 'echo a b'"] {
            let (program, args) = profiles::split(line).unwrap();
            assert_eq!(join(&program, &args), line);
        }
    }
}

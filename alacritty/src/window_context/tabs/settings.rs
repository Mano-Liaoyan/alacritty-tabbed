//! Profile settings, drawn as a dialog over the window.

use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use winit::dpi::PhysicalPosition;
use winit::event::{KeyEvent, MouseButton};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use alacritty_terminal::index::{Column, Point};

use crate::config::UiConfig;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::window::Window;

use super::bar::Layout;
use super::fonts::{self, Family};
use super::paint::{BLACK, Layer, Palette, Rect, fit, mix};
use super::profiles::{self, Profile, Profiles};
use super::{icons, platform};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Command,
    Directory,
    Icon,
    Font,
    Env,
    Options,
}

const FIELDS: [Field; 7] = [
    Field::Name,
    Field::Command,
    Field::Directory,
    Field::Icon,
    Field::Font,
    Field::Env,
    Field::Options,
];

/// Most rows the font list shows at once.
const FONT_ROWS: usize = 8;

impl Field {
    fn index(self) -> usize {
        self as usize
    }

    fn multiline(self) -> bool {
        matches!(self, Field::Env | Field::Options)
    }

    fn label(self) -> &'static str {
        [
            "Name",
            "Command line",
            "Starting directory",
            "Icon",
            "Font",
            "Environment variables",
            "Configuration overrides",
        ][self.index()]
    }

    fn hint(self) -> &'static str {
        [
            "",
            "Empty uses the shell from alacritty.toml",
            "Empty starts in your home folder",
            "Empty uses the program's own icon",
            "Empty uses the font from alacritty.toml",
            "KEY=value, one per line",
            "e.g. font.size=14",
        ][self.index()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Close,
    Row(usize),
    NewProfile,
    Duplicate,
    Tile,
    Field(Field),
    Browse(Field),
    Reset,
    AllFonts,
    /// Row of the font list.
    Font(usize),
    ToggleDefault,
    ToggleShown,
    Delete,
    Cancel,
    Save,
}

/// A profile being edited, kept as the text the user typed until it is saved.
#[derive(Clone)]
struct Entry {
    text: [String; 7],
    hidden: bool,
}

impl Entry {
    fn new(profile: &Profile) -> Self {
        let env = profile.env.iter().map(|(key, value)| format!("{key}={value}"));
        let text = [
            profile.name.clone(),
            profile.command.clone(),
            profile.directory.clone(),
            profile.icon.clone(),
            profile.font.clone(),
            env.collect::<Vec<_>>().join("\n"),
            profile.options.join("\n"),
        ];
        Self { text, hidden: profile.hidden }
    }

    fn lines(&self, field: Field) -> impl Iterator<Item = &str> {
        self.text[field.index()].lines().map(str::trim_start).filter(|line| !line.is_empty())
    }

    /// The profile as typed.
    fn draft(&self) -> Profile {
        let text = |field: Field| self.text[field.index()].trim().to_owned();
        let env = self.lines(Field::Env).filter_map(|line| line.split_once('='));
        Profile {
            name: text(Field::Name),
            command: text(Field::Command),
            directory: text(Field::Directory),
            icon: text(Field::Icon),
            font: text(Field::Font),
            env: env.map(|(key, value)| (key.trim().into(), value.into())).collect(),
            options: self.lines(Field::Options).map(str::to_owned).collect(),
            hidden: self.hidden,
        }
    }

    /// The profile, or what to fix.
    fn validate(&self) -> Result<Profile, String> {
        let profile = self.draft();
        let name = &profile.name;
        if name.is_empty() {
            return Err("Every profile needs a name".into());
        }
        let directory = profiles::expand(&profile.directory);
        if !profile.directory.is_empty() && !Path::new(&directory).is_dir() {
            return Err(format!("{name}: the starting directory does not exist"));
        }
        let icon = profiles::expand(&profile.icon);
        if profile.icon.contains(['.', '/', '\\']) && !Path::new(&icon).is_file() {
            return Err(format!("{name}: the icon file does not exist"));
        }
        if self
            .lines(Field::Env)
            .any(|line| line.split_once('=').is_none_or(|(k, _)| k.trim().is_empty()))
        {
            return Err(format!("{name}: write environment variables as KEY=value"));
        }
        if let Some(line) =
            self.lines(Field::Options).find(|line| toml::from_str::<toml::Table>(line).is_err())
        {
            return Err(format!("{name}: `{line}` is not a key=value option"));
        }
        Ok(profile)
    }
}

/// A name for a copy of the profile `name`, such as "Shell (copy)" or "Shell (copy 2)".
fn copy_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let name = name.trim();
    let base = name
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" (copy"))
        .filter(|(_, number)| number.is_empty() || number.trim_start().parse::<u32>().is_ok())
        .map_or(name, |(base, _)| base);
    let base = if base.is_empty() { "Untitled" } else { base };
    let mut candidate = format!("{base} (copy)");
    for number in 2.. {
        if !taken(&candidate) {
            break;
        }
        candidate = format!("{base} (copy {number})");
    }
    candidate
}

/// A sidebar row being dragged to a new position.
struct Drag {
    /// Pointer offset from the row's top edge.
    grab: f32,
    start: f32,
    y: f32,
    moving: bool,
}

/// The list of fonts dropped from the font field.
struct FontList {
    /// Row chosen with the keyboard.
    highlighted: Option<usize>,
    /// First row shown.
    first: usize,
    /// Whether the field's text filters the fonts, once it has been typed.
    filter: bool,
}

/// Geometry of the dialog in window pixels.
struct Panel {
    dialog: Rect,
    close: Rect,
    sidebar: Rect,
    rows: Vec<(usize, Rect)>,
    new_profile: Rect,
    duplicate: Rect,
    card: Rect,
    tile: Rect,
    fields: [Rect; 7],
    labels: [Rect; 7],
    buttons: Vec<(Target, Rect)>,
    status: Rect,
    /// The font list, its rows' area and the height of a row.
    font_list: Option<(Rect, Rect, f32)>,
}

pub struct Settings {
    entries: Vec<Entry>,
    default: Option<usize>,
    selected: usize,
    scroll: usize,
    focus: Option<Field>,
    /// Byte position of the caret in the focused field.
    caret: usize,
    preedit: String,
    hover: Option<Target>,
    drag: Option<Drag>,
    dirty: bool,
    confirm_close: bool,
    status: Option<(String, bool)>,
    /// File picker being shown for a field.
    picking: Option<(Field, Receiver<Option<String>>)>,
    /// Installed font families, and their listing until it is done.
    fonts: Vec<Family>,
    listing: Option<Receiver<Vec<Family>>>,
    /// Offer every font, not only monospaced ones.
    all_fonts: bool,
    font_list: Option<FontList>,
    pointer: PhysicalPosition<f64>,
}

impl Settings {
    pub fn new(config: &UiConfig, wake: impl Fn() + Send + 'static) -> Self {
        let profiles = Profiles::load(config);
        let mut entries: Vec<_> = profiles.list.iter().map(Entry::new).collect();
        if entries.is_empty() {
            entries.push(Entry::new(&Profile::new("New profile", "")));
        }
        Self {
            default: profiles.list.iter().position(|profile| profile.name == profiles.default),
            entries,
            selected: 0,
            scroll: 0,
            focus: None,
            caret: 0,
            preedit: String::new(),
            hover: None,
            drag: None,
            dirty: false,
            confirm_close: false,
            status: None,
            picking: None,
            fonts: Vec::new(),
            listing: Some(fonts::list(wake)),
            all_fonts: false,
            font_list: None,
            pointer: PhysicalPosition::default(),
        }
    }

    fn panel(&self, layout: &Layout) -> Panel {
        let px = |value: f32| layout.px(value);
        let (window, bar) = (layout.width, layout.bar);
        let height = layout.height;
        let (w, h) = (px(920.).min(window - px(24.)), px(700.).min(height - bar - px(12.)));
        let dialog =
            Rect::new(((window - w) / 2.).round(), (bar + (height - bar - h) / 2.).round(), w, h);
        let (x, y) = (dialog.x, dialog.y);
        let title = px(48.);

        // Text takes the terminal's font, so text-bound widths follow its cells.
        let cw = layout.cell.0;
        let side = px(240.).max((20. * cw + px(88.)).round());
        let sidebar = Rect::new(x + px(8.), y + title, side, h - title - px(8.));
        let new_profile =
            Rect::new(x + px(14.), sidebar.bottom() - px(42.), px(40.) + 11. * cw, px(36.));
        let duplicate =
            Rect::new(new_profile.right() + px(4.), new_profile.y, px(40.) + 9. * cw, px(36.));
        let mut rows = Vec::new();
        let mut top = sidebar.y + px(36.);
        for index in self.scroll..self.entries.len() {
            if top + px(36.) > new_profile.y - px(6.) {
                break;
            }
            rows.push((index, Rect::new(x + px(14.), top, side - px(12.), px(36.))));
            top += px(38.);
        }

        let card =
            Rect::new(sidebar.right() + px(8.), y + title, w - side - px(24.), h - title - px(8.));
        let (x0, x1) = (card.x + px(28.), card.right() - px(28.));
        let at = |top: f32| card.y + px(top);
        let tile = Rect::new(x0, at(24.), px(52.), px(52.));
        let mid = ((x0 + x1) / 2.).round();
        let button = px(88.);
        let field = |x: f32, top: f32, right: f32, height: f32| {
            Rect::new(x, at(top), right - x, px(height))
        };
        let fields = [
            field(tile.right() + px(16.), 24., x1, 36.),
            field(x0, 122., x1 - button - px(8.), 34.),
            field(x0, 194., x1 - button - px(8.), 34.),
            field(x0, 266., x1 - 2. * (button + px(8.)), 34.),
            field(x0, 338., x1 - 2. * (button + px(8.)), 34.),
            field(x0, 410., mid - px(8.), 96.),
            field(mid + px(8.), 410., x1, 96.),
        ];
        let label = |x: f32, top: f32, right: f32| field(x, top, right, 20.);
        let labels = [
            label(fields[0].x, 62., x1),
            label(x0, 98., x1),
            label(x0, 170., x1),
            label(x0, 242., x1),
            label(x0, 314., x1),
            label(x0, 386., mid - px(8.)),
            label(mid + px(8.), 386., x1),
        ];
        let beside = |field: Rect, offset: f32| {
            Rect::new(field.right() + px(8.) + offset, field.y, button, field.h)
        };
        let footer = card.bottom() - px(58.);
        let buttons = vec![
            (Target::Close, Rect::new(dialog.right() - px(48.), y + px(8.), px(40.), px(32.))),
            (Target::NewProfile, new_profile),
            (Target::Duplicate, duplicate),
            (Target::Tile, tile),
            (Target::Browse(Field::Command), beside(fields[1], 0.)),
            (Target::Browse(Field::Directory), beside(fields[2], 0.)),
            (Target::Browse(Field::Icon), beside(fields[3], 0.)),
            (Target::Reset, beside(fields[3], button + px(8.))),
            (Target::AllFonts, Rect { w: 2. * button + px(8.), ..beside(fields[4], 0.) }),
            (Target::ToggleDefault, Rect::new(x0, at(524.), mid - x0 - px(8.), px(32.))),
            (Target::ToggleShown, Rect::new(mid + px(8.), at(524.), x1 - mid - px(8.), px(32.))),
            (Target::Delete, Rect::new(x0, footer, px(128.), px(34.))),
            (Target::Cancel, Rect::new(x1 - px(196.), footer, px(92.), px(34.))),
            (Target::Save, Rect::new(x1 - px(96.), footer, px(96.), px(34.))),
        ];
        let status = Rect::new(x0 + px(140.), footer, x1 - px(208.) - x0 - px(140.), px(34.));

        // The font list drops from its field over the fields below.
        let font_list = self.font_list().map(|_| {
            let field = fields[Field::Font.index()];
            let row = px(28.).max(layout.cell.1 + px(8.));
            let space = ((dialog.bottom() - field.bottom() - px(24.)) / row).max(1.) as usize;
            let rows = self.candidates().len().clamp(1, FONT_ROWS.min(space));
            let list =
                Rect::new(field.x, field.bottom() + px(4.), field.w, rows as f32 * row + px(12.));
            (list, list.inset(px(6.)), row)
        });
        Panel {
            dialog,
            close: buttons[0].1,
            sidebar,
            rows,
            new_profile,
            duplicate,
            card,
            tile,
            fields,
            labels,
            buttons,
            status,
            font_list,
        }
    }

    fn target(&self, panel: &Panel, point: PhysicalPosition<f64>) -> Option<Target> {
        if let (Some(list), Some((rect, rows, row))) = (self.font_list(), panel.font_list) {
            if rect.contains(point) {
                let index = list.first + ((point.y as f32 - rows.y) / row).max(0.) as usize;
                return (rows.contains(point) && index < self.candidates().len())
                    .then_some(Target::Font(index));
            }
        }
        let button = panel
            .buttons
            .iter()
            .find(|(target, rect)| rect.contains(point) && self.enabled(*target));
        let row = panel.rows.iter().find(|(_, rect)| rect.contains(point));
        let field = FIELDS.into_iter().find(|field| panel.fields[field.index()].contains(point));
        button
            .map(|(target, _)| *target)
            .or(row.map(|(index, _)| Target::Row(*index)))
            .or(field.map(Target::Field))
    }

    fn enabled(&self, target: Target) -> bool {
        match target {
            Target::Reset => !self.entries[self.selected].text[Field::Icon.index()].is_empty(),
            Target::Delete => self.entries.len() > 1,
            _ => true,
        }
    }

    fn text(&self, field: Field) -> &str {
        &self.entries[self.selected].text[field.index()]
    }

    /// The font list, while the font field has focus.
    fn font_list(&self) -> Option<&FontList> {
        self.font_list.as_ref().filter(|_| self.focus == Some(Field::Font))
    }

    /// Fonts offered in the font list.
    fn candidates(&self) -> Vec<&Family> {
        self.offered(self.font_list.as_ref().is_some_and(|list| list.filter))
    }

    /// Fonts offered, all of them or those matching the field's text when `filter`.
    fn offered(&self, filter: bool) -> Vec<&Family> {
        let query = if filter { self.text(Field::Font) } else { "" };
        fonts::matching(&self.fonts, query, self.all_fonts)
    }

    /// Show every font in the list, from the profile's own.
    fn open_font_list(&mut self) {
        let current = self.text(Field::Font).trim().to_lowercase();
        let index = self.offered(false).iter().position(|font| font.name.to_lowercase() == current);
        let first = index.map_or(0, |index| index.saturating_sub(FONT_ROWS / 2));
        self.font_list = Some(FontList { highlighted: index, first, filter: false });
    }

    /// Show the fonts matching the typed text.
    fn filter_fonts(&mut self) {
        let found = !self.text(Field::Font).trim().is_empty() && !self.offered(true).is_empty();
        self.font_list = Some(FontList { highlighted: found.then_some(0), first: 0, filter: true });
    }

    /// Take the font in row `index` of the list.
    fn pick_font(&mut self, index: usize) {
        let Some(name) = self.candidates().get(index).map(|font| font.name.clone()) else { return };
        self.caret = name.len();
        self.entries[self.selected].text[Field::Font.index()] = name;
        self.font_list = None;
        self.touched();
    }

    /// Highlight row `index` of the font list, scrolling it into view.
    fn highlight_font(&mut self, index: usize, layout: &Layout) {
        let rows = self.panel(layout).font_list.map_or(1, |(_, rows, row)| (rows.h / row) as usize);
        let Some(list) = &mut self.font_list else { return };
        list.highlighted = Some(index);
        list.first = list.first.min(index).max((index + 1).saturating_sub(rows.max(1)));
    }

    /// Note an unsaved change.
    fn touched(&mut self) {
        self.dirty = true;
        self.confirm_close = false;
        self.status = None;
    }

    fn select(&mut self, index: usize, layout: &Layout) {
        self.selected = index.min(self.entries.len() - 1);
        self.focus = None;
        let visible = self.panel(layout).rows.len().max(1);
        self.scroll =
            self.scroll.min(self.selected).max((self.selected + 1).saturating_sub(visible));
    }

    fn focus(&mut self, field: Field) {
        self.focus = Some(field);
        self.caret = self.text(field).len();
        self.preedit.clear();
        if field == Field::Font {
            self.open_font_list();
        }
    }

    /// Handle the pointer moving, returning whether the dialog changed.
    pub fn moved(&mut self, layout: &Layout, point: PhysicalPosition<f64>) -> bool {
        self.pointer = point;
        let panel = self.panel(layout);
        if let Some(drag) = &mut self.drag {
            let y = point.y as f32;
            drag.y = y;
            drag.moving |= (y - drag.start).abs() >= layout.px(4.);
            if !drag.moving {
                return false;
            }
            // Move the dragged profile to the slot under it, scrolling at either end.
            let (Some(&(_, first)), Some(&(_, last))) = (panel.rows.first(), panel.rows.last())
            else {
                return true;
            };
            let pitch = layout.px(38.);
            let top = (y - drag.grab).clamp(first.y - pitch, last.y + pitch);
            let slot = (self.scroll as f32 + ((top - first.y) / pitch).round()).max(0.) as usize;
            let target = slot.min(self.entries.len() - 1);
            if target != self.selected {
                let entry = self.entries.remove(self.selected);
                self.entries.insert(target, entry);
                self.default = self.default.map(|default| match default {
                    d if d == self.selected => target,
                    d if self.selected < d && d <= target => d - 1,
                    d if target <= d && d < self.selected => d + 1,
                    d => d,
                });
                self.select(target, layout);
                self.touched();
            }
            return true;
        }
        let hover = self.target(&panel, point);
        std::mem::replace(&mut self.hover, hover) != hover
    }

    /// Handle a mouse button press, returning whether to close the dialog.
    pub fn press(
        &mut self,
        layout: &Layout,
        point: PhysicalPosition<f64>,
        button: MouseButton,
        config: &UiConfig,
        wake: impl Fn() + Send + 'static,
    ) -> bool {
        let panel = self.panel(layout);
        if button != MouseButton::Left || !panel.dialog.contains(point) {
            return false;
        }
        let Some(target) = self.target(&panel, point) else {
            self.focus = None;
            return false;
        };
        let pick = |settings: &mut Self, field: Field| {
            let (sender, receiver) = channel();
            let title = if field == Field::Directory {
                "Choose the starting directory"
            } else {
                "Choose a file"
            };
            std::thread::spawn(move || {
                let _ = sender.send(platform::pick(field == Field::Directory, title));
                wake();
            });
            settings.picking = Some((field, receiver));
        };

        match target {
            Target::Close | Target::Cancel => return self.close(),
            Target::Row(index) => {
                self.select(index, layout);
                let rect =
                    panel.rows.iter().find(|(i, _)| *i == index).map_or(Rect::default(), |r| r.1);
                let y = point.y as f32;
                self.drag = Some(Drag { grab: y - rect.y, start: y, y, moving: false });
            },
            Target::NewProfile => {
                self.entries.push(Entry::new(&Profile::new("New profile", "")));
                self.select(self.entries.len() - 1, layout);
                self.focus(Field::Name);
                self.touched();
            },
            Target::Duplicate => {
                let mut copy = self.entries[self.selected].clone();
                let name = &copy.text[Field::Name.index()];
                let taken = |name: &str| {
                    self.entries.iter().any(|e| e.text[Field::Name.index()].trim() == name)
                };
                copy.text[Field::Name.index()] = copy_name(name, taken);
                let at = self.selected + 1;
                self.entries.insert(at, copy);
                self.default =
                    self.default.map(|default| if default >= at { default + 1 } else { default });
                self.select(at, layout);
                self.focus(Field::Name);
                self.touched();
            },
            Target::Tile => pick(self, Field::Icon),
            Target::Browse(field) => pick(self, field),
            Target::Reset => {
                self.entries[self.selected].text[Field::Icon.index()].clear();
                self.touched();
            },
            Target::AllFonts => {
                self.all_fonts ^= true;
                match &self.font_list {
                    Some(list) if list.filter => self.filter_fonts(),
                    _ => self.open_font_list(),
                }
            },
            Target::Font(index) => self.pick_font(index),
            Target::Field(field) => {
                self.focus(field);
                self.caret = self.caret_at(&panel, field, point, layout);
            },
            Target::ToggleDefault => {
                self.default = (self.default != Some(self.selected)).then_some(self.selected);
                self.touched();
            },
            Target::ToggleShown => {
                self.entries[self.selected].hidden ^= true;
                self.touched();
            },
            Target::Delete => {
                let removed = self.selected;
                self.entries.remove(removed);
                self.default = match self.default {
                    Some(default) if default == removed => None,
                    Some(default) if default > removed => Some(default - 1),
                    default => default,
                };
                self.select(removed, layout);
                self.touched();
            },
            Target::Save => self.save(config),
        }
        false
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    pub fn scroll(&mut self, layout: &Layout, down: bool) {
        let (panel, count) = (self.panel(layout), self.candidates().len());
        if let (Some(list), Some((rect, rows, row))) = (&mut self.font_list, panel.font_list) {
            if rect.contains(self.pointer) {
                let max = count.saturating_sub((rows.h / row) as usize);
                let first = list.first.min(max);
                list.first = if down { (first + 3).min(max) } else { first.saturating_sub(3) };
                return;
            }
        }
        let visible = panel.rows.len();
        let max = self.entries.len().saturating_sub(visible);
        self.scroll = if down { (self.scroll + 1).min(max) } else { self.scroll.saturating_sub(1) };
    }

    /// Take the listed fonts, or the path chosen in a file picker, returning whether either came.
    pub fn poll(&mut self) -> bool {
        let listed = self.listing.as_ref().map(Receiver::try_recv);
        if let Some(Ok(_) | Err(TryRecvError::Disconnected)) = listed {
            self.fonts = listed.and_then(Result::ok).unwrap_or_default();
            self.listing = None;
            match &self.font_list {
                Some(list) if list.filter => self.filter_fonts(),
                Some(_) => self.open_font_list(),
                None => (),
            }
            return true;
        }
        let Some((field, receiver)) = &self.picking else { return false };
        let Ok(path) = receiver.try_recv() else { return false };
        let field = *field;
        self.picking = None;
        if let Some(path) = path {
            self.entries[self.selected].text[field.index()] = path;
            self.focus(field);
            self.touched();
        }
        true
    }

    /// Byte position in `field` under `point`.
    fn caret_at(
        &self,
        panel: &Panel,
        field: Field,
        point: PhysicalPosition<f64>,
        layout: &Layout,
    ) -> usize {
        let (rect, (cw, ch)) = (panel.fields[field.index()], layout.cell);
        let text = self.text(field);
        let (first_line, first_column) = self.scrolled(field, rect, layout);
        let line = if field.multiline() {
            first_line + ((point.y as f32 - rect.y - layout.px(8.)) / ch).max(0.) as usize
        } else {
            0
        };
        let column = first_column
            + ((point.x as f32 - rect.x - layout.px(10.)) / cw).round().max(0.) as usize;
        let start =
            text.split('\n').take(line).map(|line| line.len() + 1).sum::<usize>().min(text.len());
        let line_text = text[start..].split('\n').next().unwrap_or("");
        let mut width = 0;
        let end = line_text.char_indices().find(|(_, c)| {
            width += c.width().unwrap_or(0);
            width > column
        });
        start + end.map_or(line_text.len(), |(index, _)| index)
    }

    /// Line and column of the caret in the focused field.
    fn caret_position(&self, field: Field) -> (usize, usize) {
        let before = &self.text(field)[..self.caret.min(self.text(field).len())];
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        (before.matches('\n').count(), before[line_start..].width() + self.preedit.width())
    }

    /// First line and column shown, keeping the caret visible.
    fn scrolled(&self, field: Field, rect: Rect, layout: &Layout) -> (usize, usize) {
        if self.focus != Some(field) {
            return (0, 0);
        }
        let (line, column) = self.caret_position(field);
        let columns = ((rect.w - layout.px(20.)) / layout.cell.0).max(1.) as usize;
        let lines = ((rect.h - layout.px(16.)) / layout.cell.1).max(1.) as usize;
        if field.multiline() {
            (line.saturating_sub(lines - 1), 0)
        } else {
            (0, column.saturating_sub(columns - 1))
        }
    }

    /// Insert text at the caret of the focused field.
    pub fn insert(&mut self, text: &str) {
        let Some(field) = self.focus else { return };
        let text: String =
            text.chars().filter(|c| !c.is_control() || (*c == '\n' && field.multiline())).collect();
        self.preedit.clear();
        if !text.is_empty() {
            self.entries[self.selected].text[field.index()].insert_str(self.caret, &text);
            self.caret += text.len();
            self.edited();
        }
    }

    /// Note text typed in the focused field.
    fn edited(&mut self) {
        self.touched();
        if self.focus == Some(Field::Font) {
            self.filter_fonts();
        }
    }

    pub fn preedit(&mut self, text: String) {
        self.preedit = text;
    }

    /// Handle a key press, returning whether to close the dialog.
    pub fn key(
        &mut self,
        event: &KeyEvent,
        modifiers: ModifiersState,
        paste: impl FnOnce() -> String,
        config: &UiConfig,
        layout: &Layout,
    ) -> bool {
        let command = modifiers.control_key() || modifiers.super_key();
        if self.font_key(event, layout) {
            return false;
        }
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => return self.close(),
            Key::Character("s") if command => self.save(config),
            Key::Character("v") if command => self.insert(&paste()),
            Key::Named(NamedKey::Tab) => {
                let step = if modifiers.shift_key() { FIELDS.len() - 1 } else { 1 };
                let next = self.focus.map_or(0, |field| (field.index() + step) % FIELDS.len());
                self.focus(FIELDS[next]);
            },
            _ if command || modifiers.alt_key() => (),
            key => {
                let Some(field) = self.focus else { return false };
                let text = &self.entries[self.selected].text[field.index()];
                let previous = text[..self.caret].chars().next_back().map_or(0, char::len_utf8);
                let next = text[self.caret..].chars().next().map_or(0, char::len_utf8);
                let line_start = text[..self.caret].rfind('\n').map_or(0, |i| i + 1);
                let line_end = text[self.caret..].find('\n').map_or(text.len(), |i| self.caret + i);
                match key {
                    Key::Named(NamedKey::Backspace) if previous > 0 => {
                        self.caret -= previous;
                        self.entries[self.selected].text[field.index()].remove(self.caret);
                        self.edited();
                    },
                    Key::Named(NamedKey::Delete) if next > 0 => {
                        self.entries[self.selected].text[field.index()].remove(self.caret);
                        self.edited();
                    },
                    Key::Named(NamedKey::ArrowLeft) => self.caret -= previous,
                    Key::Named(NamedKey::ArrowRight) => self.caret += next,
                    Key::Named(NamedKey::Home) => self.caret = line_start,
                    Key::Named(NamedKey::End) => self.caret = line_end,
                    Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) if field.multiline() => {
                        let up = key == Key::Named(NamedKey::ArrowUp);
                        let column = text[line_start..self.caret].width();
                        let line = match up {
                            true if line_start > 0 => {
                                text[..line_start - 1].rfind('\n').map_or(0, |i| i + 1)
                            },
                            false if line_end < text.len() => line_end + 1,
                            _ => return false,
                        };
                        let rest = &text[line..];
                        let rest = &rest[..rest.find('\n').unwrap_or(rest.len())];
                        let mut width = 0;
                        let end = rest.char_indices().find(|(_, c)| {
                            width += c.width().unwrap_or(0);
                            width > column
                        });
                        self.caret = line + end.map_or(rest.len(), |(index, _)| index);
                    },
                    Key::Named(NamedKey::Enter) if field.multiline() => self.insert("\n"),
                    Key::Named(NamedKey::Enter) => self.save(config),
                    _ => {
                        if let Some(text) = &event.text {
                            self.insert(&text.clone());
                        }
                    },
                }
            },
        }
        false
    }

    /// Handle a key for the font list, returning whether it was used.
    fn font_key(&mut self, event: &KeyEvent, layout: &Layout) -> bool {
        let Some(list) = self.font_list() else {
            let down = event.logical_key == Key::Named(NamedKey::ArrowDown);
            if down && self.focus == Some(Field::Font) {
                self.open_font_list();
            }
            return down && self.focus == Some(Field::Font);
        };
        let (count, highlighted) = (self.candidates().len(), list.highlighted);
        let page = self.panel(layout).font_list.map_or(1, |(_, rows, row)| (rows.h / row) as usize);
        let last = count.saturating_sub(1);
        let index = match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => {
                self.font_list = None;
                return true;
            },
            Key::Named(NamedKey::Enter) if highlighted.is_some_and(|index| index < count) => {
                self.pick_font(highlighted.unwrap_or_default());
                return true;
            },
            Key::Named(NamedKey::ArrowDown) => highlighted.map_or(0, |index| index + 1),
            Key::Named(NamedKey::ArrowUp) => highlighted.map_or(last, |i| i.saturating_sub(1)),
            Key::Named(NamedKey::PageDown) => highlighted.map_or(0, |index| index + page),
            Key::Named(NamedKey::PageUp) => highlighted.map_or(0, |i| i.saturating_sub(page)),
            _ => return false,
        };
        if count > 0 {
            self.highlight_font(index.min(last), layout);
        }
        true
    }

    fn close(&mut self) -> bool {
        if self.dirty && !self.confirm_close {
            self.confirm_close = true;
            self.status = Some(("Unsaved changes. Close again to discard them.".into(), true));
            return false;
        }
        true
    }

    fn save(&mut self, config: &UiConfig) {
        let mut list: Vec<Profile> = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let error = match entry.validate() {
                Ok(profile) if list.iter().any(|other| other.name == profile.name) => {
                    format!("Two profiles are named {}", profile.name)
                },
                Ok(profile) => {
                    list.push(profile);
                    continue;
                },
                Err(error) => error,
            };
            self.status = Some((error, true));
            self.selected = index;
            return;
        }
        let default = self.default.map(|index| list[index].name.clone()).unwrap_or_default();
        match (Profiles { default, list }).save(config) {
            Ok(path) => {
                self.dirty = false;
                self.confirm_close = false;
                self.status = Some((format!("Saved to {}", path.display()), false));
            },
            Err(err) => self.status = Some((format!("Could not save: {err}"), true)),
        }
    }

    /// Draw the dialog, and place the input method's popup at the caret.
    pub fn draw(
        &self,
        layout: &Layout,
        palette: &Palette,
        config: &UiConfig,
        window: &Window,
    ) -> Vec<Layer> {
        let panel = self.panel(layout);
        let px = |value: f32| layout.px(value);
        let (cw, ch) = layout.cell;
        let line = px(1.).max(1.);
        let mut layer = Layer::default();
        let hover = |target: Target| self.hover == Some(target) && self.enabled(target);
        let center = |layer: &mut Layer, rect: Rect, text: &str, color: Rgb, bold: bool| {
            layer.label(rect, &fit(text, (rect.w / cw) as usize), (cw, ch), color, bold)
        };
        let left = |layer: &mut Layer, rect: Rect, text: &str, color: Rgb, bold: bool| {
            let y = rect.center().1 - ch / 2.;
            layer.text(rect.x, y, fit(text, (rect.w / cw).max(0.) as usize), color, bold)
        };

        // The dialog floats over the dimmed window.
        let dialog = panel.dialog;
        layer.rounded_alpha(Rect::new(0., 0., layout.width, layout.height), 0., BLACK, 0.35);
        layer.rounded_alpha(
            Rect::new(dialog.x, dialog.y + px(4.), dialog.w, dialog.h),
            px(12.),
            BLACK,
            0.35,
        );
        layer.outlined(dialog, px(10.), palette.chrome, palette.popup_outline, line);
        let mark =
            Rect::new(dialog.x + px(12.), dialog.y + px(11.), px(26.), px(26.)).square(px(24.));
        layer.image(mark, &icons::logo(), palette.foreground);
        let title = Rect::new(mark.right() + px(10.), dialog.y + px(4.), px(200.), px(20.));
        layer.text(title.x, dialog.y + px(24.) - ch, "Settings".into(), palette.foreground, true);
        layer.text(title.x, dialog.y + px(25.), "Profiles".into(), palette.muted, false);
        if hover(Target::Close) {
            layer.rounded(panel.close, px(8.), palette.danger);
        }
        let close_color = if hover(Target::Close) { super::paint::WHITE } else { palette.muted };
        layer.icon(
            panel.close.center(),
            px(5.),
            line,
            &[[-1., -1., 1., 1.], [-1., 1., 1., -1.]],
            close_color,
        );

        // Profiles in a sidebar styled like the tab track.
        layer.outlined(panel.sidebar, px(12.), palette.track, palette.track_outline, line);
        let section =
            Rect::new(panel.sidebar.x + px(14.), panel.sidebar.y + px(8.), px(200.), px(22.));
        left(&mut layer, section, "PROFILES", palette.muted, false);
        let lifted = self.drag.as_ref().filter(|drag| drag.moving);
        for &(index, mut rect) in &panel.rows {
            if let (Some(drag), true) = (lifted, index == self.selected) {
                rect.y = drag.y - drag.grab;
            }
            let selected = index == self.selected;
            let surface = if selected {
                layer.outlined(rect, px(9.), palette.raised, palette.raised_outline, line);
                palette.raised
            } else if hover(Target::Row(index)) {
                layer.rounded(rect, px(9.), palette.hover);
                palette.hover
            } else {
                palette.track
            };
            let profile = self.entries[index].draft();
            let icon = Rect::new(rect.x + px(8.), rect.y, px(18.), rect.h).square(px(18.));
            self.icon(&mut layer, icon, &profile, config, palette, surface, (cw, ch));
            let name = if profile.name.is_empty() { "Untitled" } else { &profile.name };
            let tag_width = 7. * cw + px(8.);
            let tag = if self.default == Some(index) { tag_width } else { px(8.) };
            let label = Rect::new(
                icon.right() + px(10.),
                rect.y,
                rect.right() - tag - icon.right() - px(10.),
                rect.h,
            );
            left(
                &mut layer,
                label,
                name,
                if selected { palette.foreground } else { palette.muted },
                selected,
            );
            if self.default == Some(index) {
                let tag = Rect::new(rect.right() - tag_width, rect.y, tag_width - px(6.), rect.h);
                center(&mut layer, tag, "Default", palette.muted, false);
            }
        }
        for (target, rect, label) in [
            (Target::NewProfile, panel.new_profile, "New profile"),
            (Target::Duplicate, panel.duplicate, "Duplicate"),
        ] {
            if hover(target) {
                layer.rounded(rect, px(9.), palette.hover);
            }
            let mark = Rect::new(rect.x, rect.y, px(32.), rect.h).center();
            let lines: &[[f32; 4]] = if target == Target::NewProfile {
                &[[-1., 0., 1., 0.], [0., -1., 0., 1.]]
            } else {
                &[
                    [-1., -0.4, -1., 1.],
                    [-1., 1., 0.4, 1.],
                    [-0.4, -1., 1., -1.],
                    [1., -1., 1., 0.4],
                    [-0.4, -1., -0.4, 0.4],
                    [-0.4, 0.4, 1., 0.4],
                ]
            };
            layer.icon(mark, px(5.), line, lines, palette.foreground);
            left(
                &mut layer,
                Rect::new(rect.x + px(32.), rect.y, rect.w - px(32.), rect.h),
                label,
                palette.foreground,
                false,
            );
        }

        // The selected profile, on a panel like the terminal's.
        layer.outlined(panel.card, px(10.), palette.terminal, palette.card_outline, line);
        let profile = self.entries[self.selected].draft();
        let tile_outline = if hover(Target::Tile) { palette.focus } else { palette.raised_outline };
        layer.outlined(panel.tile, px(12.), palette.raised, tile_outline, line);
        self.icon(
            &mut layer,
            panel.tile.square(px(32.)),
            &profile,
            config,
            palette,
            palette.raised,
            (cw, ch),
        );

        for field in FIELDS {
            let rect = panel.fields[field.index()];
            let focused = self.focus == Some(field);
            let outline = if focused { palette.focus } else { palette.field_outline };
            layer.outlined(rect, px(7.), palette.field, outline, line);

            // Text scrolled to the caret, with any text being composed by the input method.
            let (first_line, first_column) = self.scrolled(field, rect, layout);
            let mut text = self.text(field).to_owned();
            if focused {
                text.insert_str(self.caret.min(text.len()), &self.preedit);
            }
            let columns = ((rect.w - px(20.)) / cw).max(0.) as usize;
            let lines =
                if field.multiline() { ((rect.h - px(16.)) / ch).max(1.) as usize } else { 1 };
            let top = if field.multiline() { rect.y + px(8.) } else { rect.center().1 - ch / 2. };
            for (index, line_text) in text.split('\n').skip(first_line).take(lines).enumerate() {
                let mut width = 0;
                let visible: String = line_text
                    .chars()
                    .skip_while(|c| {
                        width += c.width().unwrap_or(0);
                        width <= first_column
                    })
                    .collect();
                let visible = visible.chars().scan(0, |width, c| {
                    *width += c.width().unwrap_or(0);
                    (*width <= columns).then_some(c)
                });
                layer.text(
                    rect.x + px(10.),
                    top + index as f32 * ch,
                    visible.collect(),
                    palette.foreground,
                    false,
                );
            }

            if focused {
                let (caret_line, caret_column) = self.caret_position(field);
                let x = rect.x + px(10.) + (caret_column - first_column) as f32 * cw;
                let y = top + (caret_line - first_line) as f32 * ch;
                if !self.preedit.is_empty() {
                    let width = self.preedit.width() as f32 * cw;
                    layer
                        .rect(Rect::new(x - width, y + ch - line, width, line), palette.foreground);
                }
                layer.rect(Rect::new(x, y, px(1.5).max(1.), ch), palette.foreground);
                let size = SizeInfo::new(layout.width, layout.bar, cw, ch, x, y, false);
                window.update_ime_position(Point::new(0, Column(0)), &size);
            }

            let label = panel.labels[field.index()];
            if field == Field::Name {
                left(
                    &mut layer,
                    label,
                    "Click the icon to choose your own logo",
                    palette.muted,
                    false,
                );
                continue;
            }
            // Hints show where they fit beside their label.
            let (name, hint) = (field.label(), field.hint());
            left(&mut layer, label, name, palette.foreground, true);
            let hint_width = hint.width() as f32 * cw;
            if (name.width() + 2) as f32 * cw + hint_width <= label.w {
                let hint_rect = Rect::new(label.right() - hint_width, label.y, hint_width, label.h);
                left(&mut layer, hint_rect, hint, palette.muted, false);
            }
        }

        for &(target, rect) in &panel.buttons {
            let hovered = hover(target);
            let (text, color, bold) = match target {
                Target::Browse(_) => ("Browse…", palette.foreground, false),
                Target::Reset if self.enabled(target) => ("Reset", palette.foreground, false),
                Target::Cancel => ("Cancel", palette.foreground, false),
                Target::Save if self.dirty => {
                    let fill = if hovered {
                        mix(palette.foreground, palette.terminal, 0.12)
                    } else {
                        palette.foreground
                    };
                    layer.rounded(rect, px(8.), fill);
                    center(&mut layer, rect, "Save", palette.terminal, true);
                    continue;
                },
                Target::Save => ("Save", palette.muted, true),
                Target::Delete if self.enabled(target) => {
                    if hovered {
                        layer.rounded(rect, px(8.), mix(palette.terminal, palette.danger, 0.18));
                    }
                    center(
                        &mut layer,
                        rect,
                        "Delete profile",
                        mix(palette.danger, palette.foreground, 0.25),
                        false,
                    );
                    continue;
                },
                Target::AllFonts => {
                    let check = Rect::new(rect.x + px(4.), rect.y, px(18.), rect.h).square(px(18.));
                    if self.all_fonts {
                        layer.rounded(check, px(5.), palette.foreground);
                        let mark = [[-1., 0., -0.3, 0.7], [-0.3, 0.7, 1., -0.7]];
                        layer.icon(check.center(), px(5.), px(1.5), &mark, palette.terminal);
                    } else {
                        let outline = if hovered { palette.foreground } else { palette.focus };
                        layer.outlined(check, px(5.), palette.field, outline, line);
                    }
                    let text = Rect::new(check.right() + px(10.), rect.y, rect.w - px(32.), rect.h);
                    left(&mut layer, text, "Show all fonts", palette.foreground, false);
                    continue;
                },
                Target::ToggleDefault | Target::ToggleShown => {
                    let (on, label, hint) = if target == Target::ToggleDefault {
                        (
                            self.default == Some(self.selected),
                            "Default profile",
                            "Opens with the new-tab button",
                        )
                    } else {
                        (
                            !self.entries[self.selected].hidden,
                            "Show in dropdown",
                            "Listed in the new-tab menu",
                        )
                    };
                    let track = Rect::new(rect.x, rect.center().1 - px(11.), px(40.), px(22.));
                    let knob = px(14.) / 2.;
                    let x =
                        if on { track.right() - px(4.) - knob } else { track.x + px(4.) + knob };
                    if on {
                        layer.rounded(track, px(11.), palette.foreground);
                        layer.circle((x, track.center().1), knob, palette.terminal);
                    } else {
                        layer.outlined(track, px(11.), palette.field, palette.focus, line);
                        layer.circle((x, track.center().1), knob, palette.muted);
                    }
                    let text =
                        Rect::new(track.right() + px(12.), rect.y - px(2.), rect.w - px(52.), ch);
                    layer.text(
                        text.x,
                        rect.center().1 - ch,
                        label.into(),
                        palette.foreground,
                        false,
                    );
                    layer.text(
                        text.x,
                        rect.center().1,
                        fit(hint, (text.w / cw) as usize),
                        palette.muted,
                        false,
                    );
                    continue;
                },
                _ => continue,
            };
            let fill = if hovered {
                mix(palette.raised, palette.foreground, 0.06)
            } else {
                palette.raised
            };
            layer.outlined(rect, px(8.), fill, palette.raised_outline, line);
            center(&mut layer, rect, text, color, bold);
        }

        if let Some((message, error)) = &self.status {
            let color =
                if *error { mix(palette.danger, palette.foreground, 0.25) } else { palette.muted };
            let message = fit(message, (panel.status.w / cw) as usize);
            let x = panel.status.right() - message.width() as f32 * cw;
            layer.text(x, panel.status.center().1 - ch / 2., message, color, false);
        }

        // The font list covers the fields below its own, so it is drawn after their text.
        let mut popup = Layer::default();
        if let (Some(list), Some((rect, rows, row))) = (self.font_list(), panel.font_list) {
            let radius = px(8.);
            let shadow = Rect::new(rect.x, rect.y + px(2.), rect.w, rect.h);
            popup.rounded_alpha(shadow, radius + px(2.), BLACK, 0.3);
            popup.outlined(rect, radius, palette.popup, palette.popup_outline, line);

            let candidates = self.candidates();
            let shown = (rows.h / row) as usize;
            let first = list.first.min(candidates.len().saturating_sub(shown));
            let highlighted = match self.hover {
                Some(Target::Font(index)) => Some(index),
                _ => list.highlighted,
            };
            let current = self.text(Field::Font).trim();
            let tag = "Mono";
            for (index, font) in candidates.iter().enumerate().skip(first).take(shown) {
                let item = Rect::new(rows.x, rows.y + (index - first) as f32 * row, rows.w, row);
                if highlighted == Some(index) {
                    popup.rounded(item, px(6.), palette.hover);
                }
                let tagged = self.all_fonts && font.mono;
                let tag_width = if tagged { (tag.width() + 2) as f32 * cw } else { 0. };
                let name = Rect::new(item.x + px(10.), item.y, item.w - px(20.) - tag_width, row);
                let chosen = font.name.eq_ignore_ascii_case(current);
                left(&mut popup, name, &font.name, palette.foreground, chosen);
                if tagged {
                    let x = item.right() - px(10.) - tag.width() as f32 * cw;
                    popup.text(x, item.center().1 - ch / 2., tag.into(), palette.muted, false);
                }
            }

            if candidates.len() > shown {
                let track = Rect::new(rect.right() - px(5.), rows.y, px(3.), rows.h);
                let h = (track.h * shown as f32 / candidates.len() as f32).max(px(16.));
                let y = track.y + (track.h - h) * first as f32 / (candidates.len() - shown) as f32;
                popup.rounded(Rect::new(track.x, y, track.w, h), px(1.5), palette.muted);
            }
            if candidates.is_empty() {
                let message = if self.listing.is_some() {
                    "Listing fonts…"
                } else if self.all_fonts || self.fonts.is_empty() {
                    "No fonts found"
                } else {
                    "No monospaced fonts found"
                };
                let item = Rect::new(rows.x + px(10.), rows.y, rows.w - px(20.), row);
                left(&mut popup, item, message, palette.muted, false);
            }
        }
        vec![layer, popup]
    }

    /// Draw a profile's icon, or a badge with text.
    #[allow(clippy::too_many_arguments)]
    fn icon(
        &self,
        layer: &mut Layer,
        rect: Rect,
        profile: &Profile,
        config: &UiConfig,
        palette: &Palette,
        surface: Rgb,
        cell: (f32, f32),
    ) {
        match profile.image(config) {
            Some(image) => layer.image(rect, &image, palette.foreground),
            None => {
                let accent = palette.accent(&profile.name);
                layer.rounded(rect, rect.w / 4., mix(surface, accent, 0.3));
                layer.label(rect, &profile.badge(), cell, accent, true);
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_get_unique_names() {
        let taken = |name: &str| name == "Shell (copy)";
        assert_eq!(copy_name("Shell", |_| false), "Shell (copy)");
        assert_eq!(copy_name("Shell", taken), "Shell (copy 2)");
        assert_eq!(copy_name("Shell (copy)", taken), "Shell (copy 2)");
    }
}

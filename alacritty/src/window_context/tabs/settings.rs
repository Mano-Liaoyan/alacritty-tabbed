//! Settings, drawn as a dialog over the window: the application's, which live in
//! `alacritty.toml`, and the profiles.
//!
//! Like Windows Terminal's, the sidebar lists the application's pages above the profiles, and
//! changes take effect when they are saved.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use winit::dpi::PhysicalPosition;
use winit::event::{KeyEvent, MouseButton};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use alacritty_terminal::index::{Column, Point};

use crate::config::UiConfig;
use crate::config::window::Corners;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::window::Window;

use super::app_settings::{self, AppSettings};
use super::bar::{ITEM_RADIUS, Layout, PANEL_RADIUS, RADIUS, TRACK_RADIUS};
use super::fonts::{self, Family};
use super::paint::{BLACK, Layer, Palette, Rect, mix};
use super::profiles::{self, Profile, Profiles};
use super::text::UiFont;
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
    UiFont,
    UiFontSize,
}

/// Number of fields, of the profiles and of the application.
const FIELD_COUNT: usize = 9;

/// Fields of a profile.
const FIELDS: [Field; 7] = [
    Field::Name,
    Field::Command,
    Field::Directory,
    Field::Icon,
    Field::Font,
    Field::Env,
    Field::Options,
];

/// Fields of the application's appearance.
const APP_SETTING_FIELDS: [Field; 2] = [Field::UiFont, Field::UiFontSize];

/// Smallest and largest size of the interface's font, in points.
const UI_FONT_SIZES: (f32, f32) = (4., 72.);

/// Most rows the font list shows at once.
const FONT_ROWS: usize = 8;

impl Field {
    fn index(self) -> usize {
        self as usize
    }

    fn multiline(self) -> bool {
        matches!(self, Field::Env | Field::Options)
    }

    /// Whether the field is the application's, not a profile's.
    fn is_app_setting(self) -> bool {
        self.index() >= FIELDS.len()
    }

    /// Whether the field names a font, chosen from a list of them.
    fn lists_fonts(self) -> bool {
        matches!(self, Field::Font | Field::UiFont)
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
            "Interface font",
            "Font size",
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
            "Of the tab bar, menus and settings",
            "In points",
        ][self.index()]
    }
}

/// Page of the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// The application's look, in every window.
    Appearance,
    /// The selected profile.
    Profile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Close,
    /// Row of the sidebar for the application's appearance.
    Appearance,
    Corners(Corners),
    /// Button that makes the interface's font larger, or smaller.
    Step(bool),
    /// Slider of the background's opacity.
    Opacity,
    /// Switch of the terminal's panel, or of the tabs' track.
    Backdrop(Backdrop),
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
    /// Cell of the logo picker: the program's own icon, then the bundled logos.
    Logo(usize),
    ToggleDefault,
    ToggleShown,
    Delete,
    Cancel,
    Save,
}

/// A surface behind part of the interface, which can be left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backdrop {
    /// The panel holding the terminal, set in from the window's edges.
    Panel,
    /// The track behind the tabs.
    TabTrack,
}

/// A profile being edited, kept as the text the user typed until it is saved.
#[derive(Clone)]
struct Entry {
    text: [String; 7],
    hidden: bool,
    /// Name of the saved profile this edits, which tabs opened with it know it by.
    origin: Option<String>,
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
        Self { text, hidden: profile.hidden, origin: None }
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
        match profile.icon.strip_prefix(icons::BUNDLED_PREFIX) {
            Some(logo) if icons::names().all(|known| known != logo) => {
                return Err(format!("{name}: there is no bundled logo named {logo}"));
            },
            Some(_) => (),
            None if profile.icon.contains(['.', '/', '\\']) && !Path::new(&icon).is_file() => {
                return Err(format!("{name}: the icon file does not exist"));
            },
            None => (),
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

/// A path, starting from `~` when it lies in the home folder.
fn home_relative(path: &Path) -> String {
    match home::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(rest) => Path::new("~").join(rest).display().to_string(),
        None => path.display().to_string(),
    }
}

/// A switch on `track`, filled with the terminal's blue while it is `on`.
fn switch(layer: &mut Layer, layout: &Layout, palette: &Palette, track: Rect, on: bool) {
    let px = |value: f32| layout.px(value);
    let knob = px(14.) / 2.;
    let x = if on { track.right() - px(4.) - knob } else { track.x + px(4.) + knob };
    let radius = layout.radius(11.);
    if on {
        layer.rounded(track, radius, palette.primary);
        layout.dot(layer, (x, track.center().1), knob, super::paint::WHITE);
    } else {
        layer.outlined(track, radius, palette.field, palette.focus, px(1.).max(1.));
        layout.dot(layer, (x, track.center().1), knob, palette.muted);
    }
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

/// The bundled logos dropped from the icon tile.
struct LogoPicker {
    /// Cell chosen with the keyboard.
    highlighted: usize,
    /// First row of cells shown.
    first: usize,
}

/// Geometry of the dialog in window pixels.
struct Panel {
    dialog: Rect,
    close: Rect,
    sidebar: Rect,
    appearance_row: Rect,
    rows: Vec<(usize, Rect)>,
    new_profile: Rect,
    duplicate: Rect,
    card: Rect,
    /// Part of the card the profile's fields scroll in, above its buttons.
    viewport: Rect,
    /// How far the fields are scrolled, and can be.
    scrolled: f32,
    scroll_max: f32,
    tile: Rect,
    /// Geometry of the appearance page, while it is shown.
    appearance: Option<AppearancePanel>,
    fields: [Rect; FIELD_COUNT],
    labels: [Rect; FIELD_COUNT],
    buttons: Vec<(Target, Rect)>,
    status: Rect,
    /// The font list, its rows' area and the height of a row.
    font_list: Option<(Rect, Rect, f32)>,
    /// The logo picker, its cells' area, the size of a cell and the cells in a row.
    logos: Option<(Rect, Rect, f32, usize)>,
}

/// The appearance page: settings in a group of rows, each named at the left with its control at
/// the right, like the settings of macOS.
struct AppearancePanel {
    heading: Rect,
    group: Rect,
    /// Rows for the corners, the font, its size, the opacity, the panel and the tabs' track.
    rows: [Rect; 6],
    corner_choice: Rect,
    /// The buttons that end the font size's field.
    stepper: Rect,
    slider: Rect,
    /// Switches of the panel and the tabs' track.
    switches: [Rect; 2],
}

impl AppearancePanel {
    /// Names and descriptions of the rows.
    const ROWS: [(&'static str, &'static str); 6] = [
        ("Corners", "Window, panels, tabs and buttons"),
        ("Interface font", "Tab bar, menus and settings"),
        ("Font size", "Interface text, in points"),
        ("Background opacity", "Behind the terminal's text"),
        ("Terminal panel", "Set in from the window's edges"),
        ("Tab track", "Behind the tabs"),
    ];

    /// Radius of the slider's knob.
    fn knob(layout: &Layout) -> f32 {
        layout.px(9.)
    }

    /// Where the slider's knob can go, from transparent to opaque.
    fn track(&self, layout: &Layout) -> (f32, f32) {
        let knob = Self::knob(layout);
        (self.slider.x + knob, (self.slider.w - 2. * knob).max(1.))
    }
}

impl Target {
    /// Whether the target scrolls with the profile's fields.
    fn scrolls(self) -> bool {
        matches!(
            self,
            Target::Tile
                | Target::Corners(_)
                | Target::Step(_)
                | Target::Opacity
                | Target::Backdrop(_)
                | Target::Field(_)
                | Target::Browse(_)
                | Target::Reset
                | Target::AllFonts
                | Target::ToggleDefault
                | Target::ToggleShown
        )
    }
}

pub struct Settings {
    page: Page,
    /// The application's settings as last saved.
    saved_app_settings: AppSettings,
    /// The application's settings as edited: the text of their fields, the corners, the opacity,
    /// the panel and the tabs' track.
    app_setting_texts: [String; 2],
    edited_corners: Corners,
    edited_opacity: f32,
    edited_panel: bool,
    edited_tab_track: bool,
    /// Whether the opacity's knob is being dragged.
    sliding: bool,
    /// The configuration file just saved, to reload.
    reload: Option<PathBuf>,
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
    /// Profiles just saved, under the names they had before, for their open tabs.
    saved: Vec<(String, Profile)>,
    /// Installed font families, and their listing until it is done.
    fonts: Vec<Family>,
    listing: Option<Receiver<Vec<Family>>>,
    /// Offer every font, not only monospaced ones.
    all_fonts: bool,
    font_list: Option<FontList>,
    logos: Option<LogoPicker>,
    /// Pixels the profile's fields are scrolled by.
    card_scroll: f32,
    pointer: PhysicalPosition<f64>,
}

impl Settings {
    pub fn new(config: &UiConfig, page: Page, wake: impl Fn() + Send + 'static) -> Self {
        let profiles = Profiles::load(config);
        let saved =
            |profile: &Profile| Entry { origin: Some(profile.name.clone()), ..Entry::new(profile) };
        let mut entries: Vec<_> = profiles.list.iter().map(saved).collect();
        if entries.is_empty() {
            entries.push(Entry::new(&Profile::new("New profile", "")));
        }
        let saved = AppSettings::new(config);
        Self {
            page,
            app_setting_texts: [saved.family.clone(), app_settings::format(saved.size)],
            edited_corners: saved.corners,
            edited_opacity: saved.opacity,
            edited_panel: saved.panel,
            edited_tab_track: saved.tab_track,
            sliding: false,
            saved_app_settings: saved,
            reload: None,
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
            saved: Vec::new(),
            fonts: Vec::new(),
            listing: Some(fonts::list(wake)),
            all_fonts: false,
            font_list: None,
            logos: None,
            card_scroll: 0.,
            pointer: PhysicalPosition::default(),
        }
    }

    fn panel(&self, layout: &Layout) -> Panel {
        let px = |value: f32| layout.px(value);
        let (font, ch) = (&*layout.font, layout.line);
        let (window, bar, height) = (layout.width, layout.bar, layout.height);
        let w = px(920.).min(window - px(24.)).max(0.);
        let h = px(700.).min(height - bar - px(12.)).max(0.);
        let dialog =
            Rect::new(((window - w) / 2.).round(), (bar + (height - bar - h) / 2.).round(), w, h);
        let (x, y) = (dialog.x, dialog.y);
        let title = px(48.);

        // Widths bound to text follow the interface's font; a narrow dialog leaves most of its
        // width to the profile.
        let side = px(240.).max((20. * font.average() + px(88.)).round()).min((w * 0.4).round());
        let sidebar = Rect::new(x + px(8.), y + title, side, (h - title - px(8.)).max(0.));
        let new_width = px(48.) + font.width("New profile", false);
        let duplicate_width = px(48.) + font.width("Duplicate", false);
        let shrink = ((side - px(16.)) / (new_width + duplicate_width)).clamp(0., 1.);
        let new_profile = Rect::new(
            x + px(14.),
            sidebar.bottom() - px(42.),
            (new_width * shrink).round(),
            px(36.),
        );
        let duplicate = Rect::new(
            new_profile.right() + px(4.),
            new_profile.y,
            (duplicate_width * shrink).round(),
            px(36.),
        );
        // The application's page, then the profiles, each under its heading.
        let appearance_row = Rect::new(x + px(14.), sidebar.y + px(36.), side - px(12.), px(36.));
        let mut rows = Vec::new();
        let mut top = appearance_row.bottom() + px(38.);
        for index in self.scroll..self.entries.len() {
            if top + px(36.) > new_profile.y - px(6.) {
                break;
            }
            rows.push((index, Rect::new(x + px(14.), top, side - px(12.), px(36.))));
            top += px(38.);
        }

        let card =
            Rect::new(sidebar.right() + px(8.), y + title, (w - side - px(24.)).max(0.), sidebar.h);
        let pad = if card.w < px(440.) { px(16.) } else { px(28.) };
        let (x0, x1) = (card.x + pad, (card.right() - pad).max(card.x + pad));
        let width = x1 - x0;
        let narrow = width < px(440.);
        let button = px(88.).min((width * 0.22).round());
        let all_fonts = (px(40.) + font.width("Show all fonts", false)).min((width * 0.45).round());

        // Delete, Cancel and Save stay at the card's bottom, with the status beside or above them.
        let (save, cancel) =
            (px(96.).min((width * 0.3).round()), px(92.).min((width * 0.28).round()));
        let gap = px(12.);
        let delete = font.width("Delete profile", false).ceil();
        let delete = delete.min(width - save - cancel - gap - px(16.)).max(0.);
        // The status starts the row where no Delete link does.
        let before_status = if self.page == Page::Profile { delete + gap } else { 0. };
        let status_width = width - save - cancel - before_status - 2. * gap;
        let footer = card.bottom() - px(58.);
        let status = if status_width >= px(160.) {
            Rect::new(x0 + before_status, footer, status_width, px(34.))
        } else {
            Rect::new(x0, footer - ch - px(10.), width, ch + px(6.))
        };
        let viewport = Rect::new(card.x, card.y, card.w, (status.y.min(footer) - card.y).max(0.));

        let field =
            |x: f32, y: f32, right: f32, height: f32| Rect::new(x, y, (right - x).max(0.), height);
        let mid = ((x0 + x1) / 2.).round();
        let halves = if narrow { [(x0, x1); 2] } else { [(x0, mid - px(8.)), (mid + px(8.), x1)] };

        // The profile's fields from `top`, with the multiline ones `lines` high, and their end.
        let place = |top: f32, lines: f32| {
            let tile = Rect::new(x0, top + px(24.), px(52.), px(52.));
            let mut fields = [Rect::default(); FIELD_COUNT];
            let mut labels = [Rect::default(); FIELD_COUNT];
            fields[0] = field(tile.right() + px(16.), tile.y, x1, px(36.));
            labels[0] = field(fields[0].x, tile.y + px(38.), x1, px(20.));
            let mut at = tile.y + px(74.);
            let beside = button + px(8.);
            let rights = [x1 - beside, x1 - beside, x1 - 2. * beside, x1 - all_fonts - px(8.)];
            for (index, right) in (1..5).zip(rights) {
                labels[index] = field(x0, at, x1, px(20.));
                fields[index] = field(x0, at + px(24.), right, px(34.));
                at += px(72.);
            }
            for (index, (left, right)) in (5..7).zip(halves) {
                labels[index] = field(left, at, right, px(20.));
                fields[index] = field(left, at + px(24.), right, lines);
                if narrow || index == 6 {
                    at += px(40.) + lines;
                }
            }
            let toggles = if narrow {
                [field(x0, at, x1, px(32.)), field(x0, at + px(40.), x1, px(32.))]
            } else {
                [field(x0, at, mid - px(8.), px(32.)), field(mid + px(8.), at, x1, px(32.))]
            };
            (tile, fields, labels, toggles, toggles[1].bottom() + px(24.) - top)
        };

        // The appearance's settings from `top`: the page's title, then a row for each setting with
        // its control at the right, and their end.
        let place_appearance = |top: f32| {
            let heading = Rect::new(x0, top + px(24.), width, 2. * ch);
            let row = px(56.).max(2. * ch + px(16.));
            let group = Rect::new(x0, heading.bottom() + px(20.), width, 6. * row);
            let rows = [0., 1., 2., 3., 4., 5.]
                .map(|i| Rect::new(group.x, group.y + i * row, group.w, row));
            let mut fields = [Rect::default(); FIELD_COUNT];

            // Controls keep their own widths, giving up room to the names only in narrow cards.
            let right = group.right() - px(16.);
            let room = ((group.w - px(32.)) * 0.6).round();
            let control = px(30.);
            let control_at = |row: Rect, width: f32| {
                let width = width.min(room);
                Rect::new(right - width, (row.y + (row.h - control) / 2.).round(), width, control)
            };
            let words = font.width("Rounded", false).max(font.width("Square", false));
            let corner_choice = control_at(rows[0], 2. * (px(42.) + words) + px(6.));
            fields[Field::UiFont.index()] = control_at(rows[1], px(240.));
            let stepper = control_at(rows[2], px(22.));
            let size_width = px(64.).max(font.width("72.5", false) + px(24.));
            fields[Field::UiFontSize.index()] =
                Rect { x: stepper.x - size_width, w: size_width, ..stepper };
            let value = font.width("100%", false).ceil();
            let slider = control_at(rows[3], px(200.) + px(12.) + value);
            let slider = Rect { w: (slider.w - px(12.) - value).max(px(40.)), ..slider };
            let switches = [control_at(rows[4], px(40.)), control_at(rows[5], px(40.))];

            let panel =
                AppearancePanel { heading, group, rows, corner_choice, stepper, slider, switches };
            (panel, fields, group.bottom() + px(24.) - top)
        };

        let mut buttons = vec![(
            Target::Close,
            Rect::new(dialog.right() - px(48.), y + px(8.), px(40.), px(32.)),
        )];
        let (tile, appearance, fields, labels, scrolled, scroll_max);
        match self.page {
            Page::Appearance => {
                scroll_max = (place_appearance(0.).2 - viewport.h).max(0.);
                scrolled = self.card_scroll.clamp(0., scroll_max);
                let page;
                (page, fields, _) = place_appearance(viewport.y - scrolled);
                (tile, labels) = (Rect::default(), [Rect::default(); FIELD_COUNT]);
                let segments = page.corner_choice.inset(px(3.));
                let half = (segments.w / 2.).round();
                let stepper = page.stepper;
                let upper = (stepper.h / 2.).round();
                buttons.extend([
                    (Target::Corners(Corners::Round), Rect { w: half, ..segments }),
                    (Target::Corners(Corners::Square), Rect {
                        x: segments.x + half,
                        w: segments.w - half,
                        ..segments
                    }),
                    (Target::Step(true), Rect { h: upper, ..stepper }),
                    (Target::Step(false), Rect {
                        y: stepper.y + upper,
                        h: stepper.h - upper,
                        ..stepper
                    }),
                    (Target::Opacity, page.slider),
                    (Target::Backdrop(Backdrop::Panel), page.switches[0]),
                    (Target::Backdrop(Backdrop::TabTrack), page.switches[1]),
                ]);
                appearance = Some(page);
            },
            Page::Profile => {
                // Multiline fields give up height down to two lines before the fields scroll.
                let full = px(96.);
                let deficit = place(0., full).4 - viewport.h;
                let lines = (full - deficit / if narrow { 2. } else { 1. })
                    .clamp((2. * ch + px(16.)).min(full), full);
                scroll_max = (place(0., lines).4 - viewport.h).max(0.);
                scrolled = self.card_scroll.clamp(0., scroll_max);
                let toggles;
                (tile, fields, labels, toggles, _) = place(viewport.y - scrolled, lines);
                appearance = None;

                let beside = |field: Rect, offset: f32| {
                    Rect::new(field.right() + px(8.) + offset, field.y, button, field.h)
                };
                let font = fields[Field::Font.index()];
                buttons.extend([
                    (Target::Tile, tile),
                    (Target::Browse(Field::Command), beside(fields[1], 0.)),
                    (Target::Browse(Field::Directory), beside(fields[2], 0.)),
                    (Target::Browse(Field::Icon), beside(fields[3], 0.)),
                    (Target::Reset, beside(fields[3], button + px(8.))),
                    (
                        Target::AllFonts,
                        Rect::new(
                            font.right() + px(8.),
                            font.y,
                            x1 - font.right() - px(8.),
                            font.h,
                        ),
                    ),
                    (Target::ToggleDefault, toggles[0]),
                    (Target::ToggleShown, toggles[1]),
                    (Target::Delete, Rect::new(x0, footer, delete, px(34.))),
                ]);
            },
        }
        buttons.extend([
            (Target::NewProfile, new_profile),
            (Target::Duplicate, duplicate),
            (Target::Cancel, Rect::new(x1 - save - gap - cancel, footer, cancel, px(34.))),
            (Target::Save, Rect::new(x1 - save, footer, save, px(34.))),
        ]);

        // The font list drops from its field, or rises above it when there is more room there.
        let font = fields[self.font_field().unwrap_or(Field::Font).index()];
        let font_list = self.font_list().map(|_| {
            let row = px(28.).max(ch + px(8.));
            let below = viewport.bottom() - font.bottom() - px(16.);
            let above = font.y - viewport.y - px(16.);
            let space = (below.max(above) / row).max(1.) as usize;
            let rows = self.candidates().len().clamp(1, FONT_ROWS.min(space));
            let list_height = rows as f32 * row + px(12.);
            let top = if below >= list_height || below >= above {
                font.bottom() + px(4.)
            } else {
                font.y - px(4.) - list_height
            };
            let list = Rect::new(font.x, top, font.w, list_height);
            (list, list.inset(px(6.)), row)
        });

        // The logo picker drops from the tile, kept inside the dialog.
        let logos = self.logos.as_ref().map(|_| {
            let cell = px(48.);
            let count = icons::names().count() + 1;
            let columns = ((width - px(12.)) / cell).clamp(1., 12.) as usize;
            let header = ch + px(18.);
            let room = dialog.bottom() - px(8.) - (y + title) - header - px(12.);
            let rows = count.div_ceil(columns).min((room / cell).max(1.) as usize);
            let size = (columns as f32 * cell + px(12.), header + rows as f32 * cell + px(12.));
            let top =
                (tile.bottom() + px(6.)).min(dialog.bottom() - px(8.) - size.1).max(y + title);
            let picker = Rect::new(x0, top, size.0, size.1);
            let cells =
                Rect::new(x0 + px(6.), top + header, columns as f32 * cell, rows as f32 * cell);
            (picker, cells, cell, columns)
        });

        Panel {
            dialog,
            close: buttons[0].1,
            sidebar,
            appearance_row,
            rows,
            new_profile,
            duplicate,
            card,
            viewport,
            scrolled,
            scroll_max,
            tile,
            appearance,
            fields,
            labels,
            buttons,
            status,
            font_list,
            logos,
        }
    }

    fn target(&self, panel: &Panel, point: PhysicalPosition<f64>) -> Option<Target> {
        if let (Some(picker), Some((rect, cells, cell, columns))) = (&self.logos, panel.logos) {
            if rect.contains(point) {
                let column = ((point.x as f32 - cells.x) / cell) as usize;
                let row = picker.first + ((point.y as f32 - cells.y) / cell) as usize;
                let index = row * columns + column;
                let count = icons::names().count() + 1;
                return (cells.contains(point) && column < columns && index < count)
                    .then_some(Target::Logo(index));
            }
        }
        if let (Some(list), Some((rect, rows, row))) = (self.font_list(), panel.font_list) {
            if rect.contains(point) {
                let index = list.first + ((point.y as f32 - rows.y) / row).max(0.) as usize;
                return (rows.contains(point) && index < self.candidates().len())
                    .then_some(Target::Font(index));
            }
        }
        // What scrolls with the fields can only be reached where they show.
        let shown = |target: Target| !target.scrolls() || panel.viewport.contains(point);
        let button = panel
            .buttons
            .iter()
            .find(|(target, rect)| rect.contains(point) && self.enabled(*target) && shown(*target));
        let row = panel.rows.iter().find(|(_, rect)| rect.contains(point));
        let field = self
            .fields()
            .iter()
            .find(|field| panel.fields[field.index()].contains(point))
            .filter(|_| panel.viewport.contains(point));
        let appearance = panel.appearance_row.contains(point).then_some(Target::Appearance);
        button
            .map(|(target, _)| *target)
            .or(appearance)
            .or(row.map(|(index, _)| Target::Row(*index)))
            .or(field.map(|field| Target::Field(*field)))
    }

    fn enabled(&self, target: Target) -> bool {
        match target {
            Target::Reset => !self.entries[self.selected].text[Field::Icon.index()].is_empty(),
            Target::Delete => self.entries.len() > 1,
            _ => true,
        }
    }

    /// Fields of the page.
    fn fields(&self) -> &'static [Field] {
        match self.page {
            Page::Appearance => &APP_SETTING_FIELDS,
            Page::Profile => &FIELDS,
        }
    }

    fn text(&self, field: Field) -> &str {
        match field.is_app_setting() {
            true => &self.app_setting_texts[field.index() - FIELDS.len()],
            false => &self.entries[self.selected].text[field.index()],
        }
    }

    fn text_mut(&mut self, field: Field) -> &mut String {
        match field.is_app_setting() {
            true => &mut self.app_setting_texts[field.index() - FIELDS.len()],
            false => &mut self.entries[self.selected].text[field.index()],
        }
    }

    /// The field fonts are listed for, while it has focus.
    fn font_field(&self) -> Option<Field> {
        self.focus.filter(|field| field.lists_fonts())
    }

    /// The font list, while a font field has focus.
    fn font_list(&self) -> Option<&FontList> {
        self.font_list.as_ref().filter(|_| self.font_field().is_some())
    }

    /// Whether to list every font, not only monospaced ones as for the terminal.
    fn all_fonts(&self) -> bool {
        self.all_fonts || self.font_field() == Some(Field::UiFont)
    }

    /// Fonts offered in the font list.
    fn candidates(&self) -> Vec<&Family> {
        self.offered(self.font_list.as_ref().is_some_and(|list| list.filter))
    }

    /// Fonts offered, all of them or those matching the field's text when `filter`.
    fn offered(&self, filter: bool) -> Vec<&Family> {
        let field = self.font_field().unwrap_or(Field::Font);
        let query = if filter { self.text(field) } else { "" };
        fonts::matching(&self.fonts, query, self.all_fonts())
    }

    /// Show every font in the list, from the field's own.
    fn open_font_list(&mut self) {
        let field = self.font_field().unwrap_or(Field::Font);
        let current = self.text(field).trim().to_lowercase();
        let index = self.offered(false).iter().position(|font| font.name.to_lowercase() == current);
        let first = index.map_or(0, |index| index.saturating_sub(FONT_ROWS / 2));
        self.font_list = Some(FontList { highlighted: index, first, filter: false });
    }

    /// Show the fonts matching the typed text.
    fn filter_fonts(&mut self) {
        let field = self.font_field().unwrap_or(Field::Font);
        let found = !self.text(field).trim().is_empty() && !self.offered(true).is_empty();
        self.font_list = Some(FontList { highlighted: found.then_some(0), first: 0, filter: true });
    }

    /// Take the font in row `index` of the list.
    fn pick_font(&mut self, index: usize) {
        let Some(name) = self.candidates().get(index).map(|font| font.name.clone()) else { return };
        let Some(field) = self.font_field() else { return };
        self.caret = name.len();
        *self.text_mut(field) = name;
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

    /// Cell of the logo picker showing the profile's icon, or the first.
    fn logo_index(&self) -> usize {
        let icon = self.text(Field::Icon).trim();
        let name = icon.strip_prefix(icons::BUNDLED_PREFIX);
        name.and_then(|name| icons::names().position(|known| known == name)).map_or(0, |i| i + 1)
    }

    /// Show the logo picker, at the profile's icon.
    fn open_logos(&mut self, layout: &Layout) {
        self.focus = None;
        self.logos = Some(LogoPicker { highlighted: 0, first: 0 });
        self.highlight_logo(self.logo_index(), layout);
    }

    /// Highlight cell `index` of the logo picker, scrolling it into view.
    fn highlight_logo(&mut self, index: usize, layout: &Layout) {
        let logos = self.panel(layout).logos;
        let (rows, columns) = logos.map_or((1, 1), |(_, cells, cell, columns)| {
            (((cells.h / cell).round() as usize).max(1), columns)
        });
        let Some(picker) = &mut self.logos else { return };
        let row = index / columns;
        picker.highlighted = index;
        picker.first = picker.first.min(row).max((row + 1).saturating_sub(rows));
    }

    /// Take the logo in cell `index`: the program's own icon, or a bundled logo.
    fn pick_logo(&mut self, index: usize) {
        let name = index.checked_sub(1).and_then(|index| icons::names().nth(index));
        let icon = name.map_or_else(String::new, |name| format!("{}{name}", icons::BUNDLED_PREFIX));
        self.entries[self.selected].text[Field::Icon.index()] = icon;
        self.logos = None;
        self.touched();
    }

    /// Scroll the focused field and its label into view.
    fn reveal(&mut self, layout: &Layout) {
        let Some(field) = self.focus else { return };
        let panel = self.panel(layout);
        let (label, rect) = (panel.labels[field.index()], panel.fields[field.index()]);
        let tile = if field == Field::Name { panel.tile } else { rect };
        let top = label.y.min(rect.y).min(tile.y) - panel.viewport.y + panel.scrolled;
        let bottom = label.bottom().max(rect.bottom()).max(tile.bottom()) - panel.viewport.y
            + panel.scrolled;
        let margin = layout.px(12.);
        let scroll = panel.scrolled.max(bottom + margin - panel.viewport.h).min(top - margin);
        self.card_scroll = scroll.clamp(0., panel.scroll_max);
    }

    /// Note an unsaved change.
    fn touched(&mut self) {
        self.dirty = true;
        self.confirm_close = false;
        self.status = None;
    }

    fn select(&mut self, index: usize, layout: &Layout) {
        self.show(Page::Profile);
        self.selected = index.min(self.entries.len() - 1);
        self.logos = None;
        let visible = self.panel(layout).rows.len().max(1);
        self.scroll =
            self.scroll.min(self.selected).max((self.selected + 1).saturating_sub(visible));
    }

    fn show(&mut self, page: Page) {
        if self.page != page {
            self.card_scroll = 0.;
        }
        self.page = page;
        self.focus = None;
        self.logos = None;
        self.font_list = None;
    }

    /// Make the interface's font a point larger, or smaller, from the size typed.
    fn step_font_size(&mut self, larger: bool) {
        let typed = self.text(Field::UiFontSize).trim().parse::<f32>();
        let size = typed.unwrap_or(self.saved_app_settings.size);
        let size = if larger { size.floor() + 1. } else { size.ceil() - 1. };
        let size = app_settings::format(size.clamp(UI_FONT_SIZES.0, UI_FONT_SIZES.1));
        if size != self.text(Field::UiFontSize) {
            self.caret = size.len();
            *self.text_mut(Field::UiFontSize) = size;
            self.touched();
        }
    }

    /// Set the opacity to a whole percent.
    fn set_opacity(&mut self, opacity: f32) {
        let opacity = (opacity.clamp(0., 1.) * 100.).round() / 100.;
        if opacity != self.edited_opacity {
            self.edited_opacity = opacity;
            self.touched();
        }
    }

    /// Set the opacity to where the pointer is along the slider.
    fn slide(&mut self, panel: &Panel, layout: &Layout) {
        let Some(appearance) = &panel.appearance else { return };
        let (start, length) = appearance.track(layout);
        self.set_opacity((self.pointer.x as f32 - start) / length);
    }

    fn focus(&mut self, field: Field) {
        self.focus = Some(field);
        self.caret = self.text(field).len();
        self.preedit.clear();
        if field.lists_fonts() {
            self.open_font_list();
        }
    }

    /// Handle the pointer moving, returning whether the dialog changed.
    pub fn moved(&mut self, layout: &Layout, point: PhysicalPosition<f64>) -> bool {
        self.pointer = point;
        let panel = self.panel(layout);
        if self.sliding {
            self.slide(&panel, layout);
            return true;
        }
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
        let target = self.target(&panel, point);
        if !matches!(target, Some(Target::Logo(_) | Target::Tile)) {
            self.logos = None;
        }
        let Some(target) = target else {
            self.focus = None;
            return false;
        };
        let focus = self.focus;
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
            Target::Appearance => self.show(Page::Appearance),
            Target::Corners(corners) => {
                if corners != self.edited_corners {
                    self.edited_corners = corners;
                    self.touched();
                }
            },
            Target::Step(larger) => self.step_font_size(larger),
            Target::Backdrop(Backdrop::Panel) => {
                self.edited_panel ^= true;
                self.touched();
            },
            Target::Backdrop(Backdrop::TabTrack) => {
                self.edited_tab_track ^= true;
                self.touched();
            },
            Target::Opacity => {
                self.sliding = true;
                self.slide(&panel, layout);
            },
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
                let mut copy = Entry { origin: None, ..self.entries[self.selected].clone() };
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
            Target::Tile if self.logos.is_some() => self.logos = None,
            Target::Tile => self.open_logos(layout),
            Target::Logo(index) => self.pick_logo(index),
            Target::Browse(field) => pick(self, field),
            Target::Reset => {
                self.text_mut(Field::Icon).clear();
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
        if self.focus.is_some() && (self.focus != focus || matches!(target, Target::Field(_))) {
            self.reveal(layout);
        }
        false
    }

    pub fn release(&mut self) {
        self.drag = None;
        self.sliding = false;
    }

    pub fn scroll(&mut self, layout: &Layout, down: bool) {
        let (panel, count) = (self.panel(layout), self.candidates().len());

        // The wheel turns the font's size and the opacity under the pointer.
        let over = |rect: Rect| rect.contains(self.pointer);
        if let Some(appearance) = &panel.appearance {
            let size = panel.fields[Field::UiFontSize.index()];
            if self.font_list().is_none() && (over(size) || over(appearance.stepper)) {
                return self.step_font_size(!down);
            }
            if over(appearance.slider) {
                return self.set_opacity(self.edited_opacity + if down { -0.01 } else { 0.01 });
            }
        }
        if let (Some(picker), Some((rect, cells, cell, columns))) = (&mut self.logos, panel.logos) {
            if rect.contains(self.pointer) {
                let rows = (icons::names().count() + 1).div_ceil(columns);
                let max = rows.saturating_sub((cells.h / cell).round() as usize);
                let first = picker.first.min(max);
                picker.first = if down { (first + 1).min(max) } else { first.saturating_sub(1) };
                return;
            }
        }
        if let (Some(list), Some((rect, rows, row))) = (&mut self.font_list, panel.font_list) {
            if rect.contains(self.pointer) {
                let max = count.saturating_sub((rows.h / row) as usize);
                let first = list.first.min(max);
                list.first = if down { (first + 3).min(max) } else { first.saturating_sub(3) };
                return;
            }
        }
        if panel.viewport.contains(self.pointer) {
            let step = if down { layout.px(48.) } else { -layout.px(48.) };
            self.card_scroll = (panel.scrolled + step).clamp(0., panel.scroll_max);
            return;
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
            *self.text_mut(field) = path;
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
        let rect = panel.fields[field.index()];
        let text = self.text(field);
        let (first_line, skip) = self.scrolled(field, rect, layout);
        let line = if field.multiline() {
            first_line + ((point.y as f32 - rect.y - layout.px(8.)) / layout.line).max(0.) as usize
        } else {
            0
        };
        let start =
            text.split('\n').take(line).map(|line| line.len() + 1).sum::<usize>().min(text.len());
        let line_text = text[start..].split('\n').next().unwrap_or("");
        let indent = self.indent(field, rect, layout);
        start
            + layout
                .font
                .offset(line_text, point.x as f32 - rect.x - layout.px(10.) - indent + skip)
    }

    /// Line of the caret in the focused field, and its distance from the start of the line.
    fn caret_position(&self, field: Field, font: &UiFont) -> (usize, f32) {
        let before = &self.text(field)[..self.caret.min(self.text(field).len())];
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        let x = font.width(&before[line_start..], false) + font.width(&self.preedit, false);
        (before.matches('\n').count(), x)
    }

    /// The field's text with any text being composed by the input method.
    fn composed(&self, field: Field) -> String {
        let mut text = self.text(field).to_owned();
        if self.focus == Some(field) {
            text.insert_str(self.caret.min(text.len()), &self.preedit);
        }
        text
    }

    /// How far the field's text is pushed right: the appearance's values end at the right of their
    /// fields, like the controls they sit beside, while they fit.
    fn indent(&self, field: Field, rect: Rect, layout: &Layout) -> f32 {
        if !field.is_app_setting() {
            return 0.;
        }
        let room = rect.w - layout.px(20.);
        (room - layout.font.width(&self.composed(field), false)).max(0.).round()
    }

    /// First line shown, and the width of text scrolled out at the left, keeping the caret
    /// visible.
    fn scrolled(&self, field: Field, rect: Rect, layout: &Layout) -> (usize, f32) {
        if self.focus != Some(field) {
            return (0, 0.);
        }
        let (line, x) = self.caret_position(field, &layout.font);
        if field.multiline() {
            let lines = ((rect.h - layout.px(16.)) / layout.line).max(1.) as usize;
            return (line.saturating_sub(lines - 1), 0.);
        }
        // Scroll by as few whole characters as show the caret.
        let room = (rect.w - layout.px(20.)).max(1.);
        let mut skip = 0.;
        for (_, advance) in layout.font.advances(&self.composed(field), false) {
            if x - skip <= room {
                break;
            }
            skip += advance;
        }
        (0, skip)
    }

    /// Insert text at the caret of the focused field.
    pub fn insert(&mut self, text: &str) {
        let Some(field) = self.focus else { return };
        let text: String =
            text.chars().filter(|c| !c.is_control() || (*c == '\n' && field.multiline())).collect();
        self.preedit.clear();
        if !text.is_empty() {
            let caret = self.caret;
            self.text_mut(field).insert_str(caret, &text);
            self.caret += text.len();
            self.edited();
        }
    }

    /// Note text typed in the focused field.
    fn edited(&mut self) {
        self.touched();
        if self.font_field().is_some() {
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
        if self.logo_key(event, layout) || self.font_key(event, layout) {
            return false;
        }
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => return self.close(),
            Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown))
                if self.focus == Some(Field::UiFontSize) =>
            {
                self.step_font_size(key == NamedKey::ArrowUp)
            },
            Key::Character("s") if command => self.save(config),
            Key::Character("v") if command => self.insert(&paste()),
            Key::Named(NamedKey::Tab) => {
                let fields = self.fields();
                let step = if modifiers.shift_key() { fields.len() - 1 } else { 1 };
                let at = self.focus.and_then(|focus| fields.iter().position(|f| *f == focus));
                let next = at.map_or(0, |at| (at + step) % fields.len());
                self.focus(fields[next]);
                self.reveal(layout);
            },
            _ if command || modifiers.alt_key() => (),
            key => {
                let Some(field) = self.focus else { return false };
                let text = self.text(field).to_owned();
                let previous = text[..self.caret].chars().next_back().map_or(0, char::len_utf8);
                let next = text[self.caret..].chars().next().map_or(0, char::len_utf8);
                let line_start = text[..self.caret].rfind('\n').map_or(0, |i| i + 1);
                let line_end = text[self.caret..].find('\n').map_or(text.len(), |i| self.caret + i);
                match key {
                    Key::Named(NamedKey::Backspace) if previous > 0 => {
                        self.caret -= previous;
                        let caret = self.caret;
                        self.text_mut(field).remove(caret);
                        self.edited();
                    },
                    Key::Named(NamedKey::Delete) if next > 0 => {
                        let caret = self.caret;
                        self.text_mut(field).remove(caret);
                        self.edited();
                    },
                    Key::Named(NamedKey::ArrowLeft) => self.caret -= previous,
                    Key::Named(NamedKey::ArrowRight) => self.caret += next,
                    Key::Named(NamedKey::Home) => self.caret = line_start,
                    Key::Named(NamedKey::End) => self.caret = line_end,
                    Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) if field.multiline() => {
                        let up = key == Key::Named(NamedKey::ArrowUp);
                        let x = layout.font.width(&text[line_start..self.caret], false);
                        let line = match up {
                            true if line_start > 0 => {
                                text[..line_start - 1].rfind('\n').map_or(0, |i| i + 1)
                            },
                            false if line_end < text.len() => line_end + 1,
                            _ => return false,
                        };
                        let rest = &text[line..];
                        let rest = &rest[..rest.find('\n').unwrap_or(rest.len())];
                        self.caret = line + layout.font.offset(rest, x);
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

    /// Handle a key for the logo picker, returning whether it was used.
    fn logo_key(&mut self, event: &KeyEvent, layout: &Layout) -> bool {
        let Some(picker) = &self.logos else { return false };
        let columns = self.panel(layout).logos.map_or(1, |(.., columns)| columns);
        let (index, last) = (picker.highlighted, icons::names().count());
        let index = match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => {
                self.logos = None;
                return true;
            },
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                self.pick_logo(index);
                return true;
            },
            Key::Named(NamedKey::ArrowRight) => index + 1,
            Key::Named(NamedKey::ArrowLeft) => index.saturating_sub(1),
            Key::Named(NamedKey::ArrowDown) => index + columns,
            Key::Named(NamedKey::ArrowUp) => index.saturating_sub(columns),
            _ => return false,
        };
        self.highlight_logo(index.min(last), layout);
        true
    }

    /// Handle a key for the font list, returning whether it was used.
    fn font_key(&mut self, event: &KeyEvent, layout: &Layout) -> bool {
        let Some(list) = self.font_list() else {
            let down = event.logical_key == Key::Named(NamedKey::ArrowDown);
            if down && self.font_field().is_some() {
                self.open_font_list();
            }
            return down && self.font_field().is_some();
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

    /// The application's settings as typed, or what to fix.
    fn edited_app_settings(&self) -> Result<AppSettings, String> {
        let number = |field: Field, range: std::ops::RangeInclusive<f32>| {
            let text = self.text(field).trim().trim_end_matches('%');
            text.parse::<f32>().ok().filter(|number| range.contains(number)).ok_or_else(|| {
                let (low, high) = (range.start(), range.end());
                format!("{} must be a number from {low} to {high}", field.label())
            })
        };
        Ok(AppSettings {
            corners: self.edited_corners,
            family: self.text(Field::UiFont).trim().into(),
            size: number(Field::UiFontSize, UI_FONT_SIZES.0..=UI_FONT_SIZES.1)?,
            opacity: self.edited_opacity,
            panel: self.edited_panel,
            tab_track: self.edited_tab_track,
        })
    }

    fn save(&mut self, config: &UiConfig) {
        let edited = match self.edited_app_settings() {
            Ok(edited) => edited,
            Err(error) => {
                self.status = Some((error, true));
                self.show(Page::Appearance);
                return;
            },
        };
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
            self.show(Page::Profile);
            self.selected = index;
            return;
        }
        let default = self.default.map(|index| list[index].name.clone()).unwrap_or_default();
        let profiles = Profiles { default, list };

        // Each file is written only when what it keeps changed.
        let mut paths = Vec::new();
        let before = Profiles::load(config);
        if profiles.default != before.default || profiles.list != before.list {
            match profiles.save(config) {
                Ok(path) => paths.push(path),
                Err(err) => return self.status = Some((format!("Could not save: {err}"), true)),
            }
        }
        for (entry, profile) in self.entries.iter_mut().zip(profiles.list) {
            if let Some(origin) = entry.origin.replace(profile.name.clone()) {
                self.saved.push((origin, profile));
            }
        }
        if edited != self.saved_app_settings {
            match edited.save(&self.saved_app_settings, config) {
                Ok(path) => {
                    self.reload = Some(path.clone());
                    paths.push(path);
                    self.saved_app_settings = edited;
                },
                Err(err) => return self.status = Some((format!("Could not save: {err}"), true)),
            }
        }
        self.dirty = false;
        self.confirm_close = false;
        let paths: Vec<_> = paths.iter().map(|path| home_relative(path)).collect();
        let message = if paths.is_empty() {
            "Nothing changed".into()
        } else {
            format!("Saved to {}", paths.join(" and "))
        };
        self.status = Some((message, false));
    }

    /// The configuration file just saved, which every window should load again.
    pub fn take_reload(&mut self) -> Option<PathBuf> {
        self.reload.take()
    }

    /// Profiles saved since this was last asked, under the names they had before.
    pub fn take_saved(&mut self) -> Vec<(String, Profile)> {
        std::mem::take(&mut self.saved)
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
        let (font, ch) = (&*layout.font, layout.line);
        let line = px(1.).max(1.);
        let mut layer = Layer::default();
        let hover = |target: Target| self.hover == Some(target) && self.enabled(target);
        let center = |layer: &mut Layer, rect: Rect, text: &str, color: Rgb, bold: bool| {
            layer.label(rect, text, font, color, bold)
        };
        let left = |layer: &mut Layer, rect: Rect, text: &str, color: Rgb, bold: bool| {
            let y = rect.center().1 - ch / 2.;
            layer.text(rect.x, y, font.fit(text, rect.w.max(0.), bold), color, bold)
        };

        // The dialog floats over the dimmed window.
        let dialog = panel.dialog;
        layer.rounded_alpha(Rect::new(0., 0., layout.width, layout.height), 0., BLACK, 0.35);
        layer.rounded_alpha(
            Rect::new(dialog.x, dialog.y + px(4.), dialog.w, dialog.h),
            layout.radius(RADIUS + 2.),
            BLACK,
            0.35,
        );
        let radius = layout.radius(RADIUS);
        layer.outlined(dialog, radius, palette.chrome, palette.popup_outline, line);
        let mark =
            Rect::new(dialog.x + px(12.), dialog.y + px(11.), px(26.), px(26.)).square(px(24.));
        layer.image(mark, &icons::logo(), palette.foreground);
        let title = Rect::new(mark.right() + px(10.), dialog.y + px(4.), px(200.), px(20.));
        layer.text(title.x, dialog.y + px(24.) - ch, "Settings".into(), palette.foreground, true);
        let page = if self.page == Page::Appearance { "Appearance" } else { "Profiles" };
        layer.text(title.x, dialog.y + px(25.), page.into(), palette.muted, false);
        if hover(Target::Close) {
            layer.rounded(panel.close, layout.radius(PANEL_RADIUS), palette.danger);
        }
        let close_color = if hover(Target::Close) { super::paint::WHITE } else { palette.muted };
        layer.icon(
            panel.close.center(),
            px(5.),
            line,
            &[[-1., -1., 1., 1.], [-1., 1., 1., -1.]],
            close_color,
        );

        // The application's page and the profiles in a sidebar styled like the tab track.
        let radius = layout.radius(PANEL_RADIUS);
        layer.outlined(panel.sidebar, radius, palette.track, palette.track_outline, line);
        let section = |y: f32| Rect::new(panel.sidebar.x + px(14.), y, px(200.), px(22.));
        left(&mut layer, section(panel.sidebar.y + px(8.)), "APPLICATION", palette.muted, false);
        let rect = panel.appearance_row;
        let selected = self.page == Page::Appearance;
        let radius = layout.radius(ITEM_RADIUS);
        let surface = if selected {
            layer.outlined(rect, radius, palette.raised, palette.raised_outline, line);
            palette.raised
        } else if hover(Target::Appearance) {
            layer.rounded(rect, radius, palette.hover);
            palette.hover
        } else {
            palette.track
        };
        let icon = Rect::new(rect.x + px(8.), rect.y, px(18.), rect.h).square(px(18.));
        let color = if selected { palette.foreground } else { palette.muted };
        layer.gear(icon.center(), px(8.), color, surface);
        let label = Rect::new(icon.right() + px(10.), rect.y, rect.w - px(36.), rect.h);
        left(&mut layer, label, "Appearance", color, selected);
        let profiles = section(panel.appearance_row.bottom() + px(8.));
        left(&mut layer, profiles, "PROFILES", palette.muted, false);
        let lifted = self.drag.as_ref().filter(|drag| drag.moving);
        for &(index, mut rect) in &panel.rows {
            if let (Some(drag), true) = (lifted, index == self.selected) {
                rect.y = drag.y - drag.grab;
            }
            let selected = index == self.selected && self.page == Page::Profile;
            let radius = layout.radius(ITEM_RADIUS);
            let surface = if selected {
                layer.outlined(rect, radius, palette.raised, palette.raised_outline, line);
                palette.raised
            } else if hover(Target::Row(index)) {
                layer.rounded(rect, radius, palette.hover);
                palette.hover
            } else {
                palette.track
            };
            let profile = self.entries[index].draft();
            let icon = Rect::new(rect.x + px(8.), rect.y, px(18.), rect.h).square(px(18.));
            self.icon(&mut layer, icon, &profile, config, palette, surface, layout);
            let name = if profile.name.is_empty() { "Untitled" } else { &profile.name };
            let tag_width = font.width("Default", false) + px(16.);
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
                layer.rounded(rect, layout.radius(ITEM_RADIUS), palette.hover);
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
        let radius = layout.radius(PANEL_RADIUS);
        layer.outlined(panel.card, radius, palette.terminal, palette.card_outline, line);
        let mut content = Layer::default();
        match &panel.appearance {
            None => {
                let profile = self.entries[self.selected].draft();
                let tile_outline =
                    if hover(Target::Tile) { palette.focus } else { palette.raised_outline };
                content.outlined(
                    panel.tile,
                    layout.radius(12.),
                    palette.raised,
                    tile_outline,
                    line,
                );
                self.icon(
                    &mut content,
                    panel.tile.square(px(32.)),
                    &profile,
                    config,
                    palette,
                    palette.raised,
                    layout,
                );
            },
            Some(page) => self.draw_appearance(&mut content, page, &panel, layout, palette),
        }

        for &field in self.fields() {
            let rect = panel.fields[field.index()];
            let focused = self.focus == Some(field);
            let outline = if focused { palette.focus } else { palette.field_outline };
            // The font size's field ends in its buttons, in one frame.
            let frame = match (&panel.appearance, field) {
                (Some(page), Field::UiFontSize) => {
                    Rect { w: page.stepper.right() - rect.x, ..rect }
                },
                _ => rect,
            };
            content.outlined(frame, layout.radius(7.), palette.field, outline, line);

            // Text scrolled to the caret, with any text being composed by the input method.
            let (first_line, skip) = self.scrolled(field, rect, layout);
            let text = self.composed(field);
            let room = (rect.w - px(20.)).max(0.);
            let left_edge = rect.x + px(10.) + self.indent(field, rect, layout);
            let lines =
                if field.multiline() { ((rect.h - px(16.)) / ch).max(1.) as usize } else { 1 };
            let top = if field.multiline() { rect.y + px(8.) } else { rect.center().1 - ch / 2. };
            for (index, line_text) in text.split('\n').skip(first_line).take(lines).enumerate() {
                let mut end = 0.;
                let advances = font.advances(line_text, false);
                let visible = line_text.chars().zip(advances).filter_map(|(c, (_, advance))| {
                    let start = end;
                    end += advance;
                    (start >= skip && end - skip <= room).then_some(c)
                });
                content.text(
                    left_edge,
                    top + index as f32 * ch,
                    visible.collect(),
                    palette.foreground,
                    false,
                );
            }

            // An empty field shows what is used in its place.
            if let Some(placeholder) =
                self.placeholder(field, config, layout).filter(|_| text.is_empty())
            {
                let placeholder = font.fit(&placeholder, room, false);
                let x = match field.is_app_setting() {
                    true => rect.right() - px(10.) - font.width(&placeholder, false),
                    false => rect.x + px(10.),
                };
                content.text(x, top, placeholder, palette.muted, false);
            }

            if focused {
                let (caret_line, caret_x) = self.caret_position(field, font);
                let x = (left_edge + caret_x - skip).round();
                let y = top + (caret_line - first_line) as f32 * ch;
                if !self.preedit.is_empty() {
                    let width = font.width(&self.preedit, false);
                    content
                        .rect(Rect::new(x - width, y + ch - line, width, line), palette.foreground);
                }
                content.rect(Rect::new(x, y, px(1.5).max(1.), ch), palette.foreground);
                let size = SizeInfo::new(layout.width, layout.bar, font.average(), ch, x, y, false);
                window.update_ime_position(Point::new(0, Column(0)), &size);
            }

            // The appearance page names its settings in its rows.
            if field.is_app_setting() {
                continue;
            }
            let label = panel.labels[field.index()];
            if field == Field::Name {
                left(&mut content, label, "Click the icon to choose a logo", palette.muted, false);
                continue;
            }
            // Hints show where they fit beside their label.
            let (name, hint) = (field.label(), field.hint());
            left(&mut content, label, name, palette.foreground, true);
            let hint_width = font.width(hint, false);
            if font.width(name, true) + px(16.) + hint_width <= label.w {
                let hint_rect = Rect::new(label.right() - hint_width, label.y, hint_width, label.h);
                left(&mut content, hint_rect, hint, palette.muted, false);
            }
        }

        for &(target, rect) in &panel.buttons {
            let hovered = hover(target);
            let layer = if target.scrolls() { &mut content } else { &mut layer };
            let (text, color, bold) = match target {
                // Each choice of corners pictures its corner.
                Target::Corners(corners) => {
                    let chosen = corners == self.edited_corners;
                    let radius = layout.radius(7.);
                    let surface = if chosen {
                        layer.outlined(rect, radius, palette.raised, palette.raised_outline, line);
                        palette.raised
                    } else if hovered {
                        layer.rounded(rect, radius, palette.hover);
                        palette.hover
                    } else {
                        palette.track
                    };
                    let color = if chosen || hovered { palette.foreground } else { palette.muted };
                    let picture =
                        Rect::new(rect.x + px(12.), rect.y, px(14.), rect.h).square(px(14.));
                    let curve = if corners == Corners::Round { px(5.) } else { 0. };
                    layer.outlined(picture, curve, surface, color, px(1.5).max(1.));
                    let word = if corners == Corners::Round { "Rounded" } else { "Square" };
                    let text =
                        Rect::new(picture.right() + px(8.), rect.y, rect.w - px(34.), rect.h);
                    left(layer, text, word, color, chosen);
                    continue;
                },
                Target::Browse(_) => ("Browse…", palette.foreground, false),
                Target::Reset if self.enabled(target) => ("Reset", palette.foreground, false),
                // Cancel is tinted with the terminal's red, and Save, once there is something to
                // save, filled with its blue, as the main action.
                Target::Cancel => {
                    let caution = palette.caution;
                    let fill = mix(palette.raised, caution, if hovered { 0.2 } else { 0.1 });
                    let outline = mix(palette.raised_outline, caution, 0.35);
                    layer.outlined(rect, layout.radius(8.), fill, outline, line);
                    let color = mix(caution, palette.foreground, 0.35);
                    center(layer, rect, "Cancel", color, false);
                    continue;
                },
                Target::Save if self.dirty => {
                    let primary = palette.primary;
                    let fill =
                        if hovered { mix(primary, super::paint::WHITE, 0.12) } else { primary };
                    layer.rounded(rect, layout.radius(8.), fill);
                    center(layer, rect, "Save", Palette::on(fill), true);
                    continue;
                },
                // The size's buttons, one above the other, set off from its number by a line.
                Target::Step(larger) => {
                    if hovered {
                        let fill = mix(palette.field, palette.foreground, 0.1);
                        layer.rounded(rect.inset(px(2.)), layout.radius(5.), fill);
                    }
                    if larger {
                        let divider =
                            Rect::new(rect.x, rect.y + px(6.), line, 2. * rect.h - px(12.));
                        layer.rect(divider, palette.field_outline);
                    }
                    let (top, bottom) = if larger { (-0.5, 0.5) } else { (0.5, -0.5) };
                    let chevron = [[-1., bottom, 0., top], [0., top, 1., bottom]];
                    let color = if hovered { palette.foreground } else { palette.muted };
                    layer.icon(rect.center(), px(3.5), px(1.3).max(1.), &chevron, color);
                    continue;
                },
                Target::Opacity => continue,
                Target::Save => ("Save", palette.muted, true),
                // Deleting is a link in dark red, starting where the fields do, underlined under
                // the pointer.
                Target::Delete if self.enabled(target) => {
                    let (word, y) = ("Delete profile", rect.center().1 - ch / 2.);
                    let color = if hovered {
                        palette.caution
                    } else {
                        mix(palette.caution, palette.terminal, 0.25)
                    };
                    layer.text(rect.x, y, word.into(), color, false);
                    if hovered {
                        let underline = Rect::new(rect.x, y + ch, font.width(word, false), line);
                        layer.rect(underline, color);
                    }
                    continue;
                },
                Target::Backdrop(backdrop) => {
                    let on = match backdrop {
                        Backdrop::Panel => self.edited_panel,
                        Backdrop::TabTrack => self.edited_tab_track,
                    };
                    let track = Rect::new(rect.x, rect.center().1 - px(11.), px(40.), px(22.));
                    switch(layer, layout, palette, track, on);
                    continue;
                },
                Target::AllFonts => {
                    let check = Rect::new(rect.x + px(4.), rect.y, px(18.), rect.h).square(px(18.));
                    if self.all_fonts {
                        layer.rounded(check, layout.radius(5.), palette.primary);
                        let mark = [[-1., 0., -0.3, 0.7], [-0.3, 0.7, 1., -0.7]];
                        let color = Palette::on(palette.primary);
                        layer.icon(check.center(), px(5.), px(1.5), &mark, color);
                    } else {
                        let outline = if hovered { palette.foreground } else { palette.focus };
                        layer.outlined(check, layout.radius(5.), palette.field, outline, line);
                    }
                    let text = Rect::new(check.right() + px(10.), rect.y, rect.w - px(32.), rect.h);
                    left(layer, text, "Show all fonts", palette.foreground, false);
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
                    switch(layer, layout, palette, track, on);
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
                        font.fit(hint, text.w, false),
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
            layer.outlined(rect, layout.radius(8.), fill, palette.raised_outline, line);
            center(layer, rect, text, color, bold);
        }

        // The fields scroll inside the card, above its buttons.
        if panel.scroll_max > 0. {
            let viewport = panel.viewport;
            let track = Rect::new(
                panel.card.right() - px(7.),
                viewport.y + px(8.),
                px(3.),
                viewport.h - px(16.),
            );
            let h = (track.h * viewport.h / (viewport.h + panel.scroll_max)).max(px(16.));
            let y = track.y + (track.h - h) * panel.scrolled / panel.scroll_max;
            content.rounded(Rect::new(track.x, y, track.w, h), layout.radius(1.5), palette.muted);
        }
        let viewport = panel.viewport;
        content.clip(
            Rect::new(
                viewport.x + line,
                viewport.y + line,
                viewport.w - 2. * line,
                viewport.h - line,
            ),
            ch,
        );

        if let Some((message, error)) = &self.status {
            let color =
                if *error { mix(palette.caution, palette.foreground, 0.35) } else { palette.muted };
            let message = font.fit(message, panel.status.w, false);
            layer.text(panel.status.x, panel.status.center().1 - ch / 2., message, color, false);
        }

        // The font list covers the fields below its own, so it is drawn after their text.
        let mut popup = Layer::default();
        if let (Some(list), Some((rect, rows, row))) = (self.font_list(), panel.font_list) {
            let radius = layout.radius(TRACK_RADIUS);
            let shadow = Rect::new(rect.x, rect.y + px(2.), rect.w, rect.h);
            popup.rounded_alpha(shadow, radius + layout.radius(2.), BLACK, 0.3);
            popup.outlined(rect, radius, palette.popup, palette.popup_outline, line);

            let candidates = self.candidates();
            let shown = (rows.h / row) as usize;
            let first = list.first.min(candidates.len().saturating_sub(shown));
            let highlighted = match self.hover {
                Some(Target::Font(index)) => Some(index),
                _ => list.highlighted,
            };
            let current = self.text(self.font_field().unwrap_or(Field::Font)).trim();
            let tag = "Mono";
            for (index, family) in candidates.iter().enumerate().skip(first).take(shown) {
                let item = Rect::new(rows.x, rows.y + (index - first) as f32 * row, rows.w, row);
                if highlighted == Some(index) {
                    popup.rounded(item, layout.radius(ITEM_RADIUS), palette.hover);
                }
                let tagged = self.all_fonts() && family.mono;
                let tag_width = if tagged { font.width(tag, false) + px(16.) } else { 0. };
                let name = Rect::new(item.x + px(10.), item.y, item.w - px(20.) - tag_width, row);
                let chosen = family.name.eq_ignore_ascii_case(current);
                left(&mut popup, name, &family.name, palette.foreground, chosen);
                if tagged {
                    let x = item.right() - px(10.) - font.width(tag, false);
                    popup.text(x, item.center().1 - ch / 2., tag.into(), palette.muted, false);
                }
            }

            if candidates.len() > shown {
                let track = Rect::new(rect.right() - px(5.), rows.y, px(3.), rows.h);
                let h = (track.h * shown as f32 / candidates.len() as f32).max(px(16.));
                let y = track.y + (track.h - h) * first as f32 / (candidates.len() - shown) as f32;
                popup.rounded(Rect::new(track.x, y, track.w, h), layout.radius(1.5), palette.muted);
            }
            if candidates.is_empty() {
                let message = if self.listing.is_some() {
                    "Listing fonts…"
                } else if self.all_fonts() || self.fonts.is_empty() {
                    "No fonts found"
                } else {
                    "No monospaced fonts found"
                };
                let item = Rect::new(rows.x + px(10.), rows.y, rows.w - px(20.), row);
                left(&mut popup, item, message, palette.muted, false);
            }
        }

        // Logos to choose from, in a grid under the tile.
        if let (Some(picker), Some((rect, cells, cell, columns))) = (&self.logos, panel.logos) {
            let radius = layout.radius(TRACK_RADIUS);
            let shadow = Rect::new(rect.x, rect.y + px(2.), rect.w, rect.h);
            popup.rounded_alpha(shadow, radius + layout.radius(2.), BLACK, 0.3);
            popup.outlined(rect, radius, palette.popup, palette.popup_outline, line);

            let names: Vec<_> = icons::names().collect();
            let count = names.len() + 1;
            let highlighted = match self.hover {
                Some(Target::Logo(index)) => index,
                _ => picker.highlighted,
            };
            let name =
                |index: usize| if index == 0 { "Program's own icon" } else { names[index - 1] };
            let header = Rect::new(
                rect.x + px(12.),
                rect.y + px(4.),
                rect.w - px(24.),
                cells.y - rect.y - px(4.),
            );
            let title = "Logos";
            left(&mut popup, header, title, palette.muted, false);
            let room = header.w - font.width(title, false) - px(16.);
            let hovered = font.fit(name(highlighted), room.max(0.), false);
            let x = header.right() - font.width(&hovered, false);
            popup.text(x, header.center().1 - ch / 2., hovered, palette.foreground, false);

            let rows = ((cells.h / cell).round() as usize).max(1);
            let total = count.div_ceil(columns);
            let first = picker.first.min(total.saturating_sub(rows));
            let profile = self.entries[self.selected].draft();
            let chosen = self.logo_index();
            let custom = !profile.icon.is_empty() && chosen == 0;
            for index in first * columns..count.min((first + rows) * columns) {
                let (row, column) = (index / columns - first, index % columns);
                let x = cells.x + column as f32 * cell;
                let item = Rect::new(x, cells.y + row as f32 * cell, cell, cell).inset(px(3.));
                let surface = if index == highlighted { palette.hover } else { palette.popup };
                let radius = layout.radius(ITEM_RADIUS);
                if index == chosen && !custom {
                    popup.outlined(item, radius, surface, palette.focus, line);
                } else if index == highlighted {
                    popup.rounded(item, radius, surface);
                }
                let icon = item.square(px(26.));
                match index.checked_sub(1).and_then(|index| icons::named(names[index])) {
                    Some(image) => popup.image(icon, &image, palette.foreground),
                    None => {
                        let own = Profile { icon: String::new(), ..profile.clone() };
                        self.icon(&mut popup, icon, &own, config, palette, surface, layout);
                    },
                }
            }
            if total > rows {
                let track = Rect::new(rect.right() - px(5.), cells.y, px(3.), cells.h);
                let h = (track.h * rows as f32 / total as f32).max(px(16.));
                let y = track.y + (track.h - h) * first as f32 / (total - rows) as f32;
                popup.rounded(Rect::new(track.x, y, track.w, h), layout.radius(1.5), palette.muted);
            }
        }
        popup.clip(panel.dialog, ch);
        vec![layer, content, popup]
    }

    /// What an empty field stands for.
    fn placeholder(&self, field: Field, config: &UiConfig, layout: &Layout) -> Option<String> {
        match field {
            Field::Command => Some(profiles::shell(config)),
            Field::Directory => Some("~".into()),
            Field::Font => Some(config.font.normal().family.clone()),
            // The font shown is the system's while none is saved.
            Field::UiFont if self.saved_app_settings.family.trim().is_empty() => {
                Some(format!("System · {}", layout.font.family))
            },
            Field::UiFont => Some("System".into()),
            _ => None,
        }
    }

    /// Draw the appearance page, but for its fields and buttons.
    fn draw_appearance(
        &self,
        layer: &mut Layer,
        page: &AppearancePanel,
        panel: &Panel,
        layout: &Layout,
        palette: &Palette,
    ) {
        let px = |value: f32| layout.px(value);
        let (font, ch) = (&*layout.font, layout.line);
        let line = px(1.).max(1.);

        let heading = page.heading;
        layer.text(heading.x, heading.y, "Appearance".into(), palette.foreground, true);
        let note = font.fit("For every window; saved to alacritty.toml", heading.w, false);
        layer.text(heading.x, heading.y + ch, note, palette.muted, false);

        // The settings in one group, a line between each, named at the left of their controls.
        let group = mix(palette.terminal, palette.field, 0.5);
        layer.outlined(page.group, layout.radius(10.), group, palette.field_outline, line);
        let controls = [
            page.corner_choice.x,
            panel.fields[Field::UiFont.index()].x,
            panel.fields[Field::UiFontSize.index()].x,
            page.slider.x,
            page.switches[0].x,
            page.switches[1].x,
        ];
        for (index, (row, (name, description))) in
            page.rows.iter().zip(AppearancePanel::ROWS).enumerate()
        {
            if index > 0 {
                let rule = Rect::new(row.x + px(16.), row.y, row.w - px(16.), line);
                layer.rect(rule, palette.field_outline);
            }
            let (x, middle) = (row.x + px(16.), row.center().1);
            let room = (controls[index] - px(16.) - x).max(0.);
            layer.text(x, middle - ch, font.fit(name, room, false), palette.foreground, false);
            layer.text(x, middle, font.fit(description, room, false), palette.muted, false);
        }

        // The choice of corners, on a track like the tabs'.
        let radius = layout.radius(10.);
        layer.outlined(page.corner_choice, radius, palette.track, palette.track_outline, line);

        // The opacity: a track filled up to its knob, and the percentage beside it.
        let (start, length) = page.track(layout);
        let (thickness, y) = (px(4.), page.slider.center().1);
        let track = Rect::new(start, (y - thickness / 2.).round(), length, thickness);
        layer.rounded(
            track,
            layout.curve(thickness / 2.),
            mix(palette.field, palette.foreground, 0.15),
        );
        let knob = start + length * self.edited_opacity;
        let filled = Rect { w: knob - start, ..track };
        layer.rounded(filled, layout.curve(thickness / 2.), palette.primary);
        let size = AppearancePanel::knob(layout);
        layout.dot(layer, (knob, y), size, palette.raised_outline);
        layout.dot(layer, (knob, y), size - line, palette.foreground);
        let percent = format!("{}%", (self.edited_opacity * 100.).round());
        let x = page.slider.right() + px(12.);
        layer.text(x, y - ch / 2., percent, palette.foreground, false);
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
        layout: &Layout,
    ) {
        match profile.image(config) {
            Some(image) => layer.image(rect, &image, palette.foreground),
            None => {
                let accent = palette.accent(&profile.name);
                layer.rounded(rect, layout.curve(rect.w / 4.), mix(surface, accent, 0.3));
                layer.label(rect, &profile.badge(), &layout.font, accent, true);
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

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
use crate::config::font::FontDescription;
use crate::config::tabbed::{Corners, MAX_PANE_GAP, RestoreSession};
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::window::Window;

use super::app_settings::upstream::{self, KEYS, Kind, Number};
use super::app_settings::{self, AppSettings};
use super::bar::{ITEM_RADIUS, Layout, PANEL_RADIUS, RADIUS, TRACK_RADIUS};
use super::fonts::{self, Family};
use super::paint::{BLACK, Layer, Palette, Rect, mix};
use super::profiles::{self, Profile, Profiles};
use super::text::UiFont;
use super::{icons, platform};

mod picker;
mod rows;

use picker::{ColorPicker, Part, SWATCHES, parse_hex};
pub use rows::Page;
use rows::{PAGES, RESTORE, Row, Toggle, restore_note};

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
    PaneHighlight,
    BrandTitle,
    BrandSubtitle,
    /// Field `.1` of the key of `alacritty.toml` at index `.0` of [`KEYS`].
    Key(u8, u8),
}

/// The fields of the profiles and the fork's own, in their order.
const OWN_FIELDS: [Field; 12] = [
    Field::Name,
    Field::Command,
    Field::Directory,
    Field::Icon,
    Field::Font,
    Field::Env,
    Field::Options,
    Field::UiFont,
    Field::UiFontSize,
    Field::PaneHighlight,
    Field::BrandTitle,
    Field::BrandSubtitle,
];

/// Number of fields: the profiles', the fork's and two for each key of `alacritty.toml`.
const FIELD_COUNT: usize = OWN_FIELDS.len() + 2 * KEYS.len();

/// Height of the footer's buttons, in logical pixels.
const FOOTER_BUTTON: f32 = 34.;

/// Room between the scrolling content and the footer, in logical pixels.
const FOOTER_GAP: f32 = 16.;

/// Height over which the scrolling content fades out at the edges it is hidden past.
const SCROLL_FADE: f32 = 16.;

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

/// The fork's own fields of the application, whose text the settings keep.
const APP_FIELDS: [Field; 5] = [
    Field::UiFont,
    Field::UiFontSize,
    Field::PaneHighlight,
    Field::BrandTitle,
    Field::BrandSubtitle,
];

/// Most rows the font list shows at once.
const FONT_ROWS: usize = 8;

/// Most rows a menu of choices shows at once.
const MENU_ROWS: usize = 10;

impl Field {
    fn index(self) -> usize {
        match self {
            Field::Key(key, part) => OWN_FIELDS.len() + 2 * usize::from(key) + usize::from(part),
            field => OWN_FIELDS.iter().position(|own| *own == field).unwrap_or_default(),
        }
    }

    /// The key of `alacritty.toml` the field edits.
    fn key(self) -> Option<&'static upstream::Key> {
        match self {
            Field::Key(key, _) => KEYS.get(usize::from(key)),
            _ => None,
        }
    }

    fn multiline(self) -> bool {
        let lines = self.key().is_some_and(|key| matches!(key.kind, Kind::List | Kind::Map));
        lines || matches!(self, Field::Env | Field::Options)
    }

    /// The number the field holds, if it holds one.
    fn number(self) -> Option<Number> {
        match (self, self.key().map(|key| key.kind)) {
            (Field::UiFontSize, _) => {
                Some(Number { min: 4., max: 72., step: 1., whole: false, optional: false })
            },
            (_, Some(Kind::Number(number) | Kind::Pair(_, number))) => Some(number),
            _ => None,
        }
    }

    /// Whether the field ends in buttons that make its number larger and smaller.
    fn stepper(self) -> bool {
        let number = self.key().is_some_and(|key| matches!(key.kind, Kind::Number(_)));
        number || self == Field::UiFontSize
    }

    /// Whether the field is the application's, not a profile's.
    fn is_app_setting(self) -> bool {
        self.index() >= FIELDS.len()
    }

    /// Whether the field's text ends at its right, like the controls it sits beside: the fork's
    /// own settings, and numbers and colors.
    fn right_aligned(self) -> bool {
        match self {
            Field::Key(..) => self.number().is_some() || self.color(),
            field => field.is_app_setting(),
        }
    }

    /// Whether the field names a font, chosen from a list of them.
    fn lists_fonts(self) -> bool {
        let face = matches!(self, Field::Key(_, 0))
            && self.key().is_some_and(|key| matches!(key.kind, Kind::Face(_)));
        face || matches!(self, Field::Font | Field::UiFont)
    }

    /// Whether the field holds a color.
    fn color(self) -> bool {
        self == Field::PaneHighlight
            || matches!(self.key().map(|key| key.kind), Some(Kind::Color | Kind::Colors(..)))
    }

    fn label(self) -> &'static str {
        match self.key() {
            Some(key) => key.name,
            None => [
                "Name",
                "Command line",
                "Starting directory",
                "Icon",
                "Font",
                "Environment variables",
                "Configuration overrides",
                "Interface font",
                "Interface font size",
                "Focus highlight",
                "Logo title",
                "Logo subtitle",
            ][self.index()],
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Field::Command => "Empty uses the shell from alacritty.toml",
            Field::Directory => "Empty starts in your home folder",
            Field::Icon => "Empty uses the program's own icon",
            Field::Font => "Empty uses the font from alacritty.toml",
            Field::Env => "KEY=value, one per line",
            Field::Options => "e.g. font.size=14",
            Field::UiFont => "Of the tab bar, menus and settings",
            Field::UiFontSize => "In points",
            Field::PaneHighlight => "Border of the focused pane",
            _ => "",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Close,
    /// Row of the sidebar for one of the application's pages.
    Page(Page),
    Corners(Corners),
    Restore(RestoreSession),
    /// Button that makes a field's number larger, or smaller.
    Step(Field, bool),
    /// Segment of a row's segmented control.
    Choice(Row, usize),
    /// The control showing a key's choice, which drops its menu.
    Menu(u8),
    /// Choice of the menu dropped.
    Item(usize),
    /// Slider of the background's opacity.
    Opacity,
    /// Slider of the space between panes.
    Gap,
    /// Scrollbar of the card or of a list, by its thumb or its track.
    Scrollbar(Bar),
    /// Well of a color's field, which opens the color picker.
    Well(Field),
    Picker(Part),
    Toggle(Toggle),
    /// Well of the tab bar's logo, which opens the logo picker.
    Brand,
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
    /// Cell of the logo picker: its leading cells, then the bundled logos.
    Logo(usize),
    ToggleDefault,
    ToggleShown,
    Delete,
    Cancel,
    Save,
}

/// The well showing the color of the field at `field`, left of it.
fn well(field: Rect, layout: &Layout) -> Rect {
    let size = layout.px(24.);
    Rect::new(field.x - layout.px(32.), field.center().1 - size / 2., size, size)
}

/// The lines of a row's description, wrapped beside its control, three at most.
fn description_lines(row: Row, rect: Rect, control: Rect, layout: &Layout) -> Vec<String> {
    let room = (control.x - layout.px(32.) - rect.x).max(layout.px(40.));
    let mut lines = wrap(row.description(), room, |text| layout.font.width(text, false));
    if lines.len() > 3 {
        let rest = lines.split_off(2).join(" ");
        lines.push(rest);
    }
    lines
}

/// The menu of a key's choices, dropped from its control.
struct Menu {
    key: u8,
    /// Row chosen with the keyboard or under the pointer.
    highlighted: usize,
    /// First row shown.
    first: usize,
}

/// Segments of a segmented control on `track`, inset by `inset` and sharing its width as `widths`
/// do.
fn segments(track: Rect, widths: &[f32], inset: f32) -> Vec<Rect> {
    let inner = track.inset(inset);
    let scale = inner.w / widths.iter().sum::<f32>().max(1.);
    let mut start = 0.;
    widths
        .iter()
        .map(|width| {
            let (x, end) = ((inner.x + start * scale).round(), start + width);
            start = end;
            Rect { x, w: (inner.x + end * scale).round() - x, ..inner }
        })
        .collect()
}

/// What the logo picker chooses an icon for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Logos {
    /// The selected profile, whose first cell is the program's own icon.
    Profile,
    /// The tab bar's logo, whose first cells are Alacritty's and none.
    Brand,
}

impl Logos {
    /// Cells before the bundled logos: their names, and the icons they stand for.
    fn leading(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Logos::Profile => &[("Program's own icon", "")],
            Logos::Brand => &[("Alacritty", "alacritty"), ("None", "")],
        }
    }

    fn count(self) -> usize {
        self.leading().len() + icons::names().count()
    }

    fn name(self, index: usize) -> &'static str {
        let leading = self.leading();
        match leading.get(index) {
            Some((name, _)) => name,
            None => icons::names().nth(index - leading.len()).unwrap_or_default(),
        }
    }

    /// The icon cell `index` stands for.
    fn icon(self, index: usize) -> String {
        match self.leading().get(index) {
            Some((_, icon)) => (*icon).into(),
            None => format!("{}{}", icons::BUNDLED_PREFIX, self.name(index)),
        }
    }

    /// The cell standing for `icon`; a file has none.
    fn index(self, icon: &str) -> Option<usize> {
        let (icon, leading) = (icon.trim(), self.leading());
        match icon.strip_prefix(icons::BUNDLED_PREFIX) {
            Some(name) => icons::names().position(|known| known == name).map(|i| leading.len() + i),
            None => leading.iter().position(|(_, own)| *own == icon),
        }
    }
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

/// The text of the fork's own fields of the application, for `settings`.
fn own_texts(settings: &AppSettings) -> [String; APP_FIELDS.len()] {
    [
        settings.family.clone(),
        app_settings::format(settings.size),
        settings.pane_highlight.map(|color| color.to_string()).unwrap_or_default(),
        settings.brand.title.clone(),
        settings.brand.subtitle.clone(),
    ]
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

/// A segment of a segmented control, raised while it is `chosen`, and the surface and color of
/// what it shows.
fn segment(
    layer: &mut Layer,
    layout: &Layout,
    palette: &Palette,
    rect: Rect,
    chosen: bool,
    hovered: bool,
) -> (Rgb, Rgb) {
    let radius = layout.radius(7.);
    let surface = if chosen {
        layer.outlined(rect, radius, palette.raised, palette.raised_outline, layout.px(1.).max(1.));
        palette.raised
    } else if hovered {
        layer.rounded(rect, radius, palette.hover);
        palette.hover
    } else {
        palette.track
    };
    (surface, if chosen || hovered { palette.foreground } else { palette.muted })
}

/// A circle crossed out, standing for no icon.
fn none_glyph(layer: &mut Layer, rect: Rect, color: Rgb, surface: Rgb, width: f32) {
    let (center, radius) = (rect.center(), rect.w * 0.36);
    layer.circle(center, radius, color);
    layer.circle(center, radius - width, surface);
    let reach = radius * std::f32::consts::FRAC_1_SQRT_2;
    let (from, to) = ((center.0 - reach, center.1 + reach), (center.0 + reach, center.1 - reach));
    layer.stroke(from, to, width, color);
}

/// The words of `text` in lines no wider than `width`, as `measure` measures them.
fn wrap(text: &str, width: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if measure(&format!("{line} {word}")) <= width => {
                line.push(' ');
                line.push_str(word);
            },
            _ => lines.push(word.into()),
        }
    }
    lines
}

/// How far along a track starting at `start`, `length` long, the pointer at `x` is, from 0 to 1.
fn along(start: f32, length: f32, x: f32) -> f32 {
    ((x - start) / length).clamp(0., 1.)
}

/// The thumb of a scrollbar along `track`, for a view `view` high scrolled by `scrolled` of at
/// most `scroll_max`, and at least `least` high.
fn thumb(track: Rect, view: f32, scrolled: f32, scroll_max: f32, least: f32) -> Rect {
    let h = (track.h * view / (view + scroll_max)).max(least).min(track.h);
    let fraction = if scroll_max > 0. { (scrolled / scroll_max).clamp(0., 1.) } else { 0. };
    Rect { y: track.y + (track.h - h) * fraction, h, ..track }
}

/// How far to scroll, of at most `scroll_max`, for a thumb `height` high with its top at `top`
/// along `track`.
fn scrolled_to(track: Rect, height: f32, top: f32, scroll_max: f32) -> f32 {
    let room = track.h - height;
    if room <= 0. {
        return 0.;
    }
    ((top - track.y) / room).clamp(0., 1.) * scroll_max
}

/// Strips `step` high filling `band`, each with how opaque it covers what is under it: most at
/// the viewport's edge, at its top or its bottom, easing out to nothing away from it.
fn fade(band: Rect, top: bool, step: f32) -> Vec<(Rect, f32)> {
    let step = step.max(1.);
    let count = (band.h / step).ceil() as usize;
    (0..count)
        .map(|index| {
            let from_edge = index as f32 * step;
            let h = step.min(band.h - from_edge);
            let y = if top { band.y + from_edge } else { band.bottom() - from_edge - h };
            // Smoothly from nearly opaque at the edge to clear at the band's far end.
            let t = 1. - (from_edge + h / 2.) / band.h;
            (Rect { y, h, ..band }, 0.92 * t * t * (3. - 2. * t))
        })
        .collect()
}

/// What a scrollbar scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bar {
    /// The card's fields, by pixels.
    Card,
    /// The font list, by rows.
    Fonts,
    /// The logo picker, by rows of cells.
    Logos,
}

/// A scrollbar: its track and thumb, the band along it that takes the pointer for it, and how
/// far its view scrolls at most.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Scrollbar {
    track: Rect,
    thumb: Rect,
    band: Rect,
    max: f32,
}

impl Scrollbar {
    /// The scrollbar along `track`, taking the pointer in a band `band` wide ending at its right
    /// edge `right`, for a view `view` long scrolled by `scrolled` of at most `max`, with a thumb
    /// at least `least` long; none when the view does not scroll.
    fn new(
        track: Rect,
        right: f32,
        band: f32,
        view: f32,
        scrolled: f32,
        max: f32,
        least: f32,
    ) -> Option<Self> {
        (max > 0.).then(|| Scrollbar {
            track,
            thumb: thumb(track, view, scrolled, max, least),
            band: Rect { x: right - band, w: band, ..track },
            max,
        })
    }

    /// Pointer offset from the thumb's top to drag it by, for a press at `y`: where the thumb
    /// was grabbed, or its middle for a press on the track, which brings the thumb there.
    fn grab(&self, y: f32) -> f32 {
        let on_thumb = (self.thumb.y..self.thumb.bottom()).contains(&y);
        if on_thumb { y - self.thumb.y } else { self.thumb.h / 2. }
    }

    /// How far to scroll with the pointer at `y`, `grab` below the top of the dragged thumb.
    fn scrolled(&self, y: f32, grab: f32) -> f32 {
        scrolled_to(self.track, self.thumb.h, y - grab, self.max)
    }

    /// The first of the rows scrolled to with the pointer at `y`, for a list scrolled by rows.
    fn first_row(&self, y: f32, grab: f32) -> usize {
        self.scrolled(y, grab).round() as usize
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

/// The bundled logos dropped from the profile's icon tile, or from the tab bar logo's well.
struct LogoPicker {
    kind: Logos,
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
    /// Rows of the sidebar for the application's pages shown, and the profiles' shown.
    pages: Vec<(Page, Rect)>,
    rows: Vec<(usize, Rect)>,
    /// The headings of the pages and of the profiles, where they show.
    headings: [Option<Rect>; 2],
    /// Rows the sidebar shows at once.
    room: usize,
    new_profile: Rect,
    duplicate: Rect,
    card: Rect,
    /// Part of the card the profile's fields scroll in, above its buttons.
    viewport: Rect,
    /// Bands at the top and the bottom of the viewport, across its content, where what is
    /// scrolled past them fades out.
    fades: [Rect; 2],
    /// How far the fields are scrolled, and can be.
    scrolled: f32,
    scroll_max: f32,
    /// The card's scrollbar, along its edge, where it scrolls.
    scrollbar: Option<Scrollbar>,
    tile: Rect,
    /// Geometry of the application's page shown.
    settings: Option<RowsPanel>,
    fields: [Rect; FIELD_COUNT],
    labels: [Rect; FIELD_COUNT],
    buttons: Vec<(Target, Rect)>,
    status: Rect,
    /// The font list, its rows' area and the height of a row.
    font_list: Option<(Rect, Rect, f32)>,
    /// The font list's scrollbar, where it has more fonts than it shows.
    font_bar: Option<Scrollbar>,
    /// The logo picker, its cells' area, the size of a cell and the cells in a row.
    logos: Option<(Rect, Rect, f32, usize)>,
    /// The logo picker's scrollbar, where it has more rows than it shows.
    logo_bar: Option<Scrollbar>,
    /// The color picker, dropped from its well.
    picker: Option<picker::Parts>,
    /// The menu of choices, its rows' area and the height of a row.
    menu: Option<(Rect, Rect, f32)>,
}

/// An application's page: settings in a group of rows, each named at the left with its control
/// at the right, like the settings of macOS.
struct RowsPanel {
    heading: Rect,
    /// Each section's title, and its group of rows.
    sections: Vec<(Rect, &'static str, Rect)>,
    /// Each row, and its control: the whole of a field and its buttons, a switch's track, a
    /// slider without its value, or a segmented control's track.
    rows: Vec<(Row, Rect, Rect)>,
    /// Text under the choices of restoring the session.
    note: Option<Rect>,
}

impl Panel {
    /// The scrollbar of `bar`, while it shows.
    fn bar(&self, bar: Bar) -> Option<Scrollbar> {
        match bar {
            Bar::Card => self.scrollbar,
            Bar::Fonts => self.font_bar,
            Bar::Logos => self.logo_bar,
        }
    }
}

impl RowsPanel {
    fn control(&self, row: Row) -> Rect {
        self.rows.iter().find(|(r, ..)| *r == row).map_or_else(Rect::default, |(.., c)| *c)
    }

    /// The frame of `field` with the buttons that end it, if it has them.
    fn frame(&self, field: Field) -> Option<Rect> {
        let row = self.rows.iter().find(|(row, ..)| row.fields().contains(&field));
        row.filter(|_| field.stepper()).map(|(.., control)| *control)
    }

    /// The end of the last group of rows.
    fn bottom(&self) -> f32 {
        self.sections.last().map_or(self.heading.bottom(), |(.., group)| group.bottom())
    }

    /// Radius of the slider's knob.
    fn knob(layout: &Layout) -> f32 {
        layout.px(9.)
    }

    /// Where the knob of `slider` can go, from its least to its most.
    fn track(slider: Rect, layout: &Layout) -> (f32, f32) {
        let knob = Self::knob(layout);
        (slider.x + knob, (slider.w - 2. * knob).max(1.))
    }
}

impl Target {
    /// Whether the target scrolls with the profile's fields.
    fn scrolls(self) -> bool {
        matches!(
            self,
            Target::Tile
                | Target::Corners(_)
                | Target::Restore(_)
                | Target::Step(..)
                | Target::Choice(..)
                | Target::Opacity
                | Target::Gap
                | Target::Well(_)
                | Target::Menu(_)
                | Target::Toggle(_)
                | Target::Brand
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
    /// The application's settings as edited, but for those typed in fields, which are kept as
    /// their text until they are saved.
    edited: AppSettings,
    app_setting_texts: [String; APP_FIELDS.len()],
    /// Slider whose knob, or scrollbar whose thumb, is being dragged.
    sliding: Option<Target>,
    /// Pointer offset from the top of the scrollbar's thumb while it is dragged.
    thumb_grab: f32,
    /// The color picker of a color's field, while it is open.
    picker: Option<(Field, ColorPicker)>,
    /// The menu of a key's choices, while it is open.
    menu: Option<Menu>,
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
            app_setting_texts: own_texts(&saved),
            edited: saved.clone(),
            sliding: None,
            thumb_grab: 0.,
            picker: None,
            menu: None,
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
        let card =
            Rect::new(sidebar.right() + px(8.), y + title, (w - side - px(24.)).max(0.), sidebar.h);
        let pad = if card.w < px(440.) { px(16.) } else { px(28.) };
        // New profile and Duplicate share the line of the card's footer buttons.
        let footer_middle = card.bottom() - pad - (px(FOOTER_BUTTON) / 2.).round();
        let new_profile =
            Rect::new(x + px(14.), footer_middle - px(18.), (new_width * shrink).round(), px(36.));
        let duplicate = Rect::new(
            new_profile.right() + px(4.),
            new_profile.y,
            (duplicate_width * shrink).round(),
            px(36.),
        );
        // The application's pages, then the profiles, each under its heading, scrolling as one
        // list above the sidebar's buttons.
        let slot = |index: usize| {
            let top = sidebar.y - px(2.) + index as f32 * px(38.);
            Rect::new(x + px(14.), top, side - px(12.), px(36.))
        };
        let slots = self.slots();
        let room = ((new_profile.y - px(4.) - sidebar.y) / px(38.)).floor().max(1.) as usize;
        let first = self.scroll.min(slots.saturating_sub(room));
        let (mut pages, mut rows, mut headings) = (Vec::new(), Vec::new(), [None; 2]);
        for index in first..slots.min(first + room) {
            let rect = slot(index - first);
            match index.checked_sub(1) {
                None => headings[0] = Some(rect),
                Some(page) if page < PAGES.len() => pages.push((PAGES[page], rect)),
                Some(page) if page == PAGES.len() => headings[1] = Some(rect),
                Some(page) => rows.push((page - PAGES.len() - 1, rect)),
            }
        }

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
        // The footer's buttons are set in from the card's bottom as far as the content is from
        // its sides, so they sit in its corner; the fields end a gap above the footer, the
        // scrolling content fading out there, and never reach under its buttons.
        let footer = card.bottom() - pad - px(FOOTER_BUTTON);
        let status = if status_width >= px(160.) {
            Rect::new(x0 + before_status, footer, status_width, px(FOOTER_BUTTON))
        } else {
            Rect::new(x0, footer - ch - px(10.), width, ch + px(6.))
        };
        let viewport_bottom = status.y.min(footer) - px(FOOTER_GAP);
        let viewport = Rect::new(card.x, card.y, card.w, (viewport_bottom - card.y).max(0.));

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

        // Widths of the segmented controls' segments.
        let words = font.width("Rounded", false).max(font.width("Square", false));
        let corner_widths = [px(42.) + words; 2];
        let restore_widths = RESTORE.map(|(_, name)| font.width(name, false).ceil() + px(22.));
        let choice_widths = |row: Row| -> Vec<f32> {
            row.choices().iter().map(|name| font.width(name, false).ceil() + px(22.)).collect()
        };
        // The note under the choices of restoring the session is as high as its longest text.
        let note_width = width - px(32.);
        let note_room = note_width - ch - px(4.);
        let note_lines = RESTORE
            .iter()
            .map(|(restore, _)| wrap(restore_note(*restore), note_room, |t| font.width(t, false)))
            .map(|lines| lines.len())
            .max()
            .unwrap_or(1);

        // Widths of the controls, following the interface's font.
        let size_width = px(64.).max(font.width("72.5", false) + px(24.));
        let value = font.width("100%", false).max(font.width("48 px", false)).ceil();
        let hex = px(96.).max(font.width("#bbbbbb", false) + px(24.));
        let cell = hex.max(font.width("CellBackground", false) + px(24.));
        let name = px(120.).max(font.width("Alacritty", false) + px(24.));
        let count = px(64.).max(font.width("-10000", false) + px(24.));
        let menu = |row: Row| {
            let widest = row.menu().iter().map(|name| font.width(name, false)).fold(0., f32::max);
            px(160.).max(widest.ceil() + px(44.))
        };
        // Fields of several lines show three of them.
        let lines = 3. * ch + px(16.);

        // An application's page from `top`: its title, then each section's title and a group of
        // rows, a row for each setting with its control at the right, and their end.
        let place_rows = |top: f32, page: Page| {
            let heading = Rect::new(x0, top + px(24.), width, 2. * ch);
            let height = px(56.).max(2. * ch + px(16.));
            let mut fields = [Rect::default(); FIELD_COUNT];

            // Controls keep their own widths, giving up room to the names only in narrow cards.
            let right = x1 - px(16.);
            let room = ((width - px(32.)) * 0.6).round();
            let control_at = |row: Rect, width: f32, control: f32| {
                let width = width.min(room);
                let y = (row.y + (row.h - control) / 2.).round();
                Rect::new(right - width, y, width, control)
            };

            let (mut rows, mut sections, mut note) = (Vec::new(), Vec::new(), None);
            let mut y = heading.bottom() + px(8.);
            for (section, section_rows) in page.sections() {
                let title = Rect::new(x0 + px(4.), y + px(12.), width - px(8.), ch);
                y = title.bottom() + px(6.);
                let group_top = y;
                for row in section_rows {
                    let kind = row.key().map(|key| key.kind);
                    let tall = row.fields().iter().any(|field| field.multiline());
                    let h = if tall { height.max(lines + px(20.)) } else { height };
                    let mut rect = Rect::new(x0, y, width, h);
                    let mut control = match (row, kind) {
                        (Row::Restore, _) => {
                            let text = note_lines as f32 * ch;
                            note = Some(Rect::new(
                                rect.x + px(16.),
                                rect.bottom() - px(6.),
                                note_width,
                                text,
                            ));
                            let control = control_at(
                                rect,
                                restore_widths.iter().sum::<f32>() + px(6.),
                                px(30.),
                            );
                            rect.h += text + px(8.);
                            control
                        },
                        (Row::Corners, _) => {
                            control_at(rect, corner_widths.iter().sum::<f32>() + px(6.), px(30.))
                        },
                        (Row::UiFont, _) => control_at(rect, px(240.), px(30.)),
                        (Row::UiFontSize, _) => control_at(rect, size_width + px(22.), px(30.)),
                        (Row::Opacity | Row::Gap, _) => {
                            let slider = control_at(rect, px(200.) + px(12.) + value, px(30.));
                            Rect { w: (slider.w - px(12.) - value).max(px(40.)), ..slider }
                        },
                        (Row::Highlight, _) => control_at(rect, px(32.) + hex, px(30.)),
                        (Row::LogoIcon, _) => control_at(rect, px(30.), px(30.)),
                        (Row::LogoText, _) => control_at(rect, 2. * name + px(8.), px(30.)),
                        (_, Some(Kind::Choice(_))) if row.menu().is_empty() => {
                            let widths = choice_widths(row).iter().sum::<f32>();
                            control_at(rect, widths + px(6.), px(30.))
                        },
                        (_, Some(Kind::Choice(_))) => control_at(rect, menu(row), px(30.)),
                        (_, Some(Kind::Number(_))) => {
                            control_at(rect, size_width + px(22.), px(30.))
                        },
                        (_, Some(Kind::Pair(..))) => control_at(rect, 2. * count + px(8.), px(30.)),
                        (_, Some(Kind::Text(_))) => control_at(rect, px(240.), px(30.)),
                        (_, Some(Kind::Face(_))) => control_at(rect, px(320.), px(30.)),
                        (_, Some(Kind::Color)) => control_at(rect, px(32.) + hex, px(30.)),
                        (_, Some(Kind::Colors(_, cells))) => {
                            let field = if cells { cell } else { hex };
                            control_at(rect, 2. * (px(32.) + field) + px(16.), px(30.))
                        },
                        (_, Some(Kind::Command)) => control_at(rect, px(320.), px(30.)),
                        (_, Some(Kind::List | Kind::Map)) => control_at(rect, px(320.), lines),
                        // Switches.
                        _ => control_at(rect, px(40.), px(30.)),
                    };
                    // A description longer than the room beside the control wraps, and the row
                    // grows for it.
                    let lines = description_lines(row, rect, control, layout).len() as f32;
                    if lines > 1. && row != Row::Restore {
                        rect.h = rect.h.max((1. + lines) * ch + px(16.));
                        control.y = (rect.y + (rect.h - control.h) / 2.).round();
                    }
                    // Fields fill their controls, but for the buttons and wells beside them, and
                    // two share one.
                    let half = ((control.w - px(8.)) / 2.).round();
                    let fields_of = row.fields();
                    match (fields_of.as_slice(), kind) {
                        // A color's field is beside its well, which is left of it.
                        (&[field], _) if field.color() => {
                            fields[field.index()] =
                                Rect { x: control.x + px(32.), w: control.w - px(32.), ..control }
                        },
                        (&[first, second], Some(Kind::Colors(..))) => {
                            let w = (control.w - px(16.)) / 2. - px(32.);
                            fields[first.index()] = Rect { x: control.x + px(32.), w, ..control };
                            fields[second.index()] = Rect { x: control.right() - w, w, ..control };
                        },
                        // A font's family takes more room than its style.
                        (&[family, style], Some(Kind::Face(_))) => {
                            let w = ((control.w - px(8.)) * 0.6).round();
                            fields[family.index()] = Rect { w, ..control };
                            fields[style.index()] = Rect {
                                x: control.x + w + px(8.),
                                w: control.w - w - px(8.),
                                ..control
                            };
                        },
                        (&[field], _) if field.stepper() => {
                            fields[field.index()] = Rect { w: control.w - px(22.), ..control }
                        },
                        (&[field], _) => fields[field.index()] = control,
                        (&[first, second], _) => {
                            fields[first.index()] = Rect { w: half, ..control };
                            fields[second.index()] =
                                Rect { x: control.right() - half, w: half, ..control };
                        },
                        _ => (),
                    }
                    y = rect.bottom();
                    rows.push((row, rect, control));
                }
                sections.push((title, section, Rect::new(x0, group_top, width, y - group_top)));
            }
            let page = RowsPanel { heading, sections, rows, note };
            let end = page.bottom() + px(24.) - top;
            (page, fields, end)
        };

        let mut buttons = vec![(
            Target::Close,
            Rect::new(dialog.right() - px(48.), y + px(8.), px(40.), px(32.)),
        )];
        let (tile, settings, fields, labels, scrolled, scroll_max);
        match self.page {
            page if page != Page::Profile => {
                scroll_max = (place_rows(0., page).2 - viewport.h).max(0.);
                scrolled = self.card_scroll.clamp(0., scroll_max);
                let page;
                (page, fields, _) = place_rows(viewport.y - scrolled, self.page);
                (tile, labels) = (Rect::default(), [Rect::default(); FIELD_COUNT]);
                for &(row, _, control) in &page.rows {
                    match row {
                        Row::Corners => {
                            let choices = [Corners::Round, Corners::Square].map(Target::Corners);
                            let parts = segments(control, &corner_widths, px(3.));
                            buttons.extend(choices.into_iter().zip(parts));
                        },
                        Row::Restore => {
                            let choices = RESTORE.map(|(restore, _)| Target::Restore(restore));
                            let parts = segments(control, &restore_widths, px(3.));
                            buttons.extend(choices.into_iter().zip(parts));
                        },
                        Row::Opacity => buttons.push((Target::Opacity, control)),
                        Row::Gap => buttons.push((Target::Gap, control)),
                        Row::LogoIcon => buttons.push((Target::Brand, control)),
                        Row::Key(key) if !row.menu().is_empty() => {
                            buttons.push((Target::Menu(key), control))
                        },
                        _ if !row.choices().is_empty() => {
                            let parts = segments(control, &choice_widths(row), px(3.));
                            let choices = (0..parts.len()).map(|index| Target::Choice(row, index));
                            buttons.extend(choices.zip(parts));
                        },
                        _ => {
                            if let Some(toggle) = row.toggle() {
                                buttons.push((Target::Toggle(toggle), control));
                            }
                        },
                    }
                    for field in row.fields() {
                        let rect = fields[field.index()];
                        if field.color() {
                            buttons.push((Target::Well(field), well(rect, layout)));
                        }
                        if field.stepper() {
                            let stepper = Rect { x: rect.right(), w: px(22.), ..rect };
                            let upper = (stepper.h / 2.).round();
                            buttons.extend([
                                (Target::Step(field, true), Rect { h: upper, ..stepper }),
                                (Target::Step(field, false), Rect {
                                    y: stepper.y + upper,
                                    h: stepper.h - upper,
                                    ..stepper
                                }),
                            ]);
                        }
                    }
                }
                settings = Some(page);
            },
            _ => {
                // Multiline fields give up height down to two lines before the fields scroll.
                let full = px(96.);
                let deficit = place(0., full).4 - viewport.h;
                let lines = (full - deficit / if narrow { 2. } else { 1. })
                    .clamp((2. * ch + px(16.)).min(full), full);
                scroll_max = (place(0., lines).4 - viewport.h).max(0.);
                scrolled = self.card_scroll.clamp(0., scroll_max);
                let toggles;
                (tile, fields, labels, toggles, _) = place(viewport.y - scrolled, lines);
                settings = None;

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
                    (Target::Delete, Rect::new(x0, footer, delete, px(FOOTER_BUTTON))),
                ]);
            },
        }
        let button_h = px(FOOTER_BUTTON);
        buttons.extend([
            (Target::NewProfile, new_profile),
            (Target::Duplicate, duplicate),
            (Target::Cancel, Rect::new(x1 - save - gap - cancel, footer, cancel, button_h)),
            (Target::Save, Rect::new(x1 - save, footer, save, button_h)),
        ]);

        // The font list drops from its field, or rises above it when there is more room there.
        let font = fields[self.font_field().unwrap_or(Field::Font).index()];
        let candidates = self.font_list().map(|_| self.candidates().len());
        let font_list = candidates.map(|candidates| {
            let row = px(28.).max(ch + px(8.));
            let below = viewport.bottom() - font.bottom() - px(16.);
            let above = font.y - viewport.y - px(16.);
            let space = (below.max(above) / row).max(1.) as usize;
            let rows = candidates.clamp(1, FONT_ROWS.min(space));
            let list_height = rows as f32 * row + px(12.);
            let top = if below >= list_height || below >= above {
                font.bottom() + px(4.)
            } else {
                font.y - px(4.) - list_height
            };
            let list = Rect::new(font.x, top, font.w, list_height);
            (list, list.inset(px(6.)), row)
        });

        let font_bar = self.font_list().zip(font_list).zip(candidates).and_then(
            |((list, (rect, rows, row)), candidates)| {
                let shown = (rows.h / row) as usize;
                let max = candidates.saturating_sub(shown);
                let track = Rect::new(rect.right() - px(5.), rows.y, px(3.), rows.h);
                let (first, shown) = (list.first.min(max) as f32, shown as f32);
                Scrollbar::new(track, rect.right(), px(12.), shown, first, max as f32, px(16.))
            },
        );

        // The logo picker drops from the profile's tile; the tab bar logo's opens beside its well,
        // left of the rows' controls, so it covers neither. Both are kept inside the dialog.
        let logos = self.logos.as_ref().map(|picker| {
            let (from, room_x) = match (picker.kind, &settings) {
                (Logos::Brand, Some(page)) => {
                    let well = page.control(Row::LogoIcon);
                    let switches = page.control(Row::Logo).x.min(well.x);
                    (Rect { y: well.y - px(6.), h: 0., ..well }, switches - px(8.) - x0)
                },
                _ => (tile, width),
            };
            let cell = px(48.);
            let count = picker.kind.count();
            let columns = ((room_x - px(12.)) / cell).clamp(1., 12.) as usize;
            let header = ch + px(18.);
            let room = dialog.bottom() - px(8.) - (y + title) - header - px(12.);
            let rows = count.div_ceil(columns).min((room / cell).max(1.) as usize);
            let size = (columns as f32 * cell + px(12.), header + rows as f32 * cell + px(12.));
            let top =
                (from.bottom() + px(6.)).min(dialog.bottom() - px(8.) - size.1).max(y + title);
            let left = x0;
            let picker = Rect::new(left, top, size.0, size.1);
            let cells =
                Rect::new(left + px(6.), top + header, columns as f32 * cell, rows as f32 * cell);
            (picker, cells, cell, columns)
        });
        let logo_bar =
            self.logos.as_ref().zip(logos).and_then(|(picker, (rect, cells, cell, columns))| {
                let rows = ((cells.h / cell).round() as usize).max(1);
                let max = picker.kind.count().div_ceil(columns).saturating_sub(rows);
                let track = Rect::new(rect.right() - px(5.), cells.y, px(3.), cells.h);
                let (first, rows) = (picker.first.min(max) as f32, rows as f32);
                Scrollbar::new(track, rect.right(), px(12.), rows, first, max as f32, px(16.))
            });

        // The color picker drops from its well, or rises above it when there is no room below,
        // ending where the hex field does, kept inside the scrolling fields.
        let picked = self.picker.as_ref().filter(|_| settings.is_some()).map(|(field, _)| *field);
        let picker = picked.map(|field| {
            let size = ColorPicker::layout((0., 0.), layout.scale).popover;
            let field = fields[field.index()];
            let x = (field.right() - size.w).max(dialog.x + px(8.));
            let well = well(field, layout);
            let below = well.bottom() + px(6.);
            let y =
                if below + size.h <= viewport.bottom() { below } else { well.y - px(6.) - size.h };
            let y = y.min(viewport.bottom() - size.h).max(viewport.y);
            ColorPicker::layout((x, y), layout.scale)
        });

        // A key's menu drops from its control, or rises above it when there is more room there.
        let menu = self.menu.as_ref().zip(settings.as_ref()).map(|(menu, page)| {
            let control = page.control(Row::Key(menu.key));
            let row = px(28.).max(ch + px(8.));
            let count = Row::Key(menu.key).menu().len().max(1);
            let below = viewport.bottom() - control.bottom() - px(12.);
            let above = control.y - viewport.y - px(12.);
            let space = ((below.max(above) - px(12.)) / row).max(1.) as usize;
            let rows = count.min(MENU_ROWS).min(space);
            let height = rows as f32 * row + px(12.);
            let top = if below >= height || below >= above {
                control.bottom() + px(4.)
            } else {
                control.y - px(4.) - height
            };
            let list = Rect::new(control.x, top, control.w, height);
            (list, list.inset(px(6.)), row)
        });

        // The content fades out at the viewport's edges across its width, clear of the card's
        // corners, its outline and its scrollbar.
        let fade_height = px(SCROLL_FADE).min((viewport.h / 2.).floor());
        let outline = px(1.).max(1.);
        let across = Rect::new(x0 - px(2.), viewport.y + outline, width + px(4.), fade_height);
        let fades = [across, Rect { y: viewport.bottom() - fade_height, ..across }];

        // The fields scroll inside the card, above its buttons, with a scrollbar at its edge.
        let track =
            Rect::new(card.right() - px(7.), viewport.y + px(8.), px(3.), viewport.h - px(16.));
        let scrollbar =
            Scrollbar::new(track, card.right(), px(16.), viewport.h, scrolled, scroll_max, px(16.));

        Panel {
            dialog,
            close: buttons[0].1,
            sidebar,
            pages,
            rows,
            headings,
            room,
            new_profile,
            duplicate,
            card,
            viewport,
            fades,
            scrolled,
            scroll_max,
            scrollbar,
            tile,
            settings,
            fields,
            labels,
            buttons,
            status,
            font_list,
            font_bar,
            logos,
            logo_bar,
            picker,
            menu,
        }
    }

    /// Rows of the sidebar: the pages' heading and the pages, the profiles' heading and the
    /// profiles.
    fn slots(&self) -> usize {
        2 + PAGES.len() + self.entries.len()
    }

    fn target(&self, panel: &Panel, point: PhysicalPosition<f64>) -> Option<Target> {
        if let Some(parts) = panel.picker.filter(|parts| parts.popover.contains(point)) {
            return ColorPicker::part_at(&parts, point).map(Target::Picker);
        }
        if let (Some(picker), Some((rect, cells, cell, columns))) = (&self.logos, panel.logos) {
            if rect.contains(point) {
                if panel.logo_bar.is_some_and(|bar| bar.band.contains(point)) {
                    return Some(Target::Scrollbar(Bar::Logos));
                }
                let column = ((point.x as f32 - cells.x) / cell) as usize;
                let row = picker.first + ((point.y as f32 - cells.y) / cell) as usize;
                let index = row * columns + column;
                return (cells.contains(point) && column < columns && index < picker.kind.count())
                    .then_some(Target::Logo(index));
            }
        }
        if let (Some(menu), Some((rect, rows, row))) = (&self.menu, panel.menu) {
            if rect.contains(point) {
                let count = Row::Key(menu.key).menu().len();
                let first = menu.first.min(count.saturating_sub((rows.h / row).round() as usize));
                let index = first + ((point.y as f32 - rows.y) / row).max(0.) as usize;
                return (rows.contains(point) && index < count).then_some(Target::Item(index));
            }
        }
        if let (Some(list), Some((rect, rows, row))) = (self.font_list(), panel.font_list) {
            if rect.contains(point) {
                if panel.font_bar.is_some_and(|bar| bar.band.contains(point)) {
                    return Some(Target::Scrollbar(Bar::Fonts));
                }
                let index = list.first + ((point.y as f32 - rows.y) / row).max(0.) as usize;
                return (rows.contains(point) && index < self.candidates().len())
                    .then_some(Target::Font(index));
            }
        }
        if panel.scrollbar.is_some_and(|bar| bar.band.contains(point)) {
            return Some(Target::Scrollbar(Bar::Card));
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
            .into_iter()
            .find(|field| panel.fields[field.index()].contains(point))
            .filter(|field| panel.viewport.contains(point) && self.enabled(Target::Field(*field)));
        let page = panel.pages.iter().find(|(_, rect)| rect.contains(point));
        button
            .map(|(target, _)| *target)
            .or(page.map(|(page, _)| Target::Page(*page)))
            .or(row.map(|(index, _)| Target::Row(*index)))
            .or(field.map(Target::Field))
    }

    fn enabled(&self, target: Target) -> bool {
        let row = match target {
            Target::Reset => {
                return !self.entries[self.selected].text[Field::Icon.index()].is_empty();
            },
            Target::Delete => return self.entries.len() > 1,
            Target::Gap => Row::Gap,
            Target::Brand => Row::LogoIcon,
            Target::Field(Field::BrandTitle | Field::BrandSubtitle) => Row::LogoText,
            // A key's controls do nothing while what it needs is off.
            Target::Menu(key)
            | Target::Choice(Row::Key(key), _)
            | Target::Toggle(Toggle::Key(key))
            | Target::Field(Field::Key(key, _))
            | Target::Well(Field::Key(key, _))
            | Target::Step(Field::Key(key, _), _) => Row::Key(key),
            _ => return true,
        };
        !row.dimmed(&self.edited)
    }

    /// Fields of the page, in their order.
    fn fields(&self) -> Vec<Field> {
        match self.page {
            Page::Profile => FIELDS.to_vec(),
            page => page.rows().flat_map(Row::fields).collect(),
        }
    }

    fn text(&self, field: Field) -> &str {
        match field {
            Field::Key(key, part) => {
                &self.edited.upstream[usize::from(key)].text[usize::from(part)]
            },
            field if field.is_app_setting() => {
                &self.app_setting_texts[field.index() - FIELDS.len()]
            },
            field => &self.entries[self.selected].text[field.index()],
        }
    }

    fn text_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Key(key, part) => {
                &mut self.edited.upstream[usize::from(key)].text[usize::from(part)]
            },
            field if field.is_app_setting() => {
                &mut self.app_setting_texts[field.index() - FIELDS.len()]
            },
            field => &mut self.entries[self.selected].text[field.index()],
        }
    }

    /// The text of an application's field as last saved.
    fn saved_text(&self, field: Field) -> String {
        match field {
            Field::Key(key, part) => {
                self.saved_app_settings.upstream[usize::from(key)].text[usize::from(part)].clone()
            },
            field => own_texts(&self.saved_app_settings)
                .get(field.index().wrapping_sub(FIELDS.len()))
                .cloned()
                .unwrap_or_default(),
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

    /// The icon the logo picker chooses.
    fn logo(&self, kind: Logos) -> &str {
        match kind {
            Logos::Profile => self.text(Field::Icon),
            Logos::Brand => &self.edited.brand.icon,
        }
    }

    /// Show the logo picker, at the icon it chooses.
    fn open_logos(&mut self, kind: Logos, layout: &Layout) {
        self.focus = None;
        self.logos = Some(LogoPicker { kind, highlighted: 0, first: 0 });
        self.highlight_logo(kind.index(self.logo(kind)).unwrap_or_default(), layout);
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

    /// Take the logo in cell `index`.
    fn pick_logo(&mut self, index: usize) {
        let Some(picker) = self.logos.take() else { return };
        let icon = picker.kind.icon(index);
        match picker.kind {
            Logos::Profile => self.entries[self.selected].text[Field::Icon.index()] = icon,
            Logos::Brand => self.edited.brand.icon = icon,
        }
        self.touched();
    }

    /// Scroll the focused field and its label into view.
    fn reveal(&mut self, layout: &Layout) {
        let Some(field) = self.focus else { return };
        let panel = self.panel(layout);
        let rect = panel.fields[field.index()];
        // The application's fields are named in their rows, not by labels.
        let label = if field.is_app_setting() { rect } else { panel.labels[field.index()] };
        let tile = if field == Field::Name { panel.tile } else { rect };
        let top = label.y.min(rect.y).min(tile.y) - panel.viewport.y + panel.scrolled;
        let bottom = label.bottom().max(rect.bottom()).max(tile.bottom()) - panel.viewport.y
            + panel.scrolled;
        // A field revealed clears the fades at the viewport's edges.
        let margin = layout.px(SCROLL_FADE + 8.);
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
        // The profile's row of the sidebar shows.
        let (row, room) = (self.selected + PAGES.len() + 2, self.panel(layout).room);
        self.scroll = self.scroll.min(row).max((row + 1).saturating_sub(room));
    }

    fn show(&mut self, page: Page) {
        if self.page != page {
            self.card_scroll = 0.;
        }
        self.page = page;
        self.focus = None;
        self.logos = None;
        self.font_list = None;
        self.picker = None;
        self.menu = None;
    }

    /// Make the number of `field` a step larger, or smaller, from the number typed, to a whole
    /// step.
    fn step(&mut self, field: Field, larger: bool) {
        let Some(number) = field.number() else { return };
        let typed = self.text(field).trim().parse::<f32>();
        let saved = self.saved_text(field).trim().parse();
        let current = typed.or(saved).unwrap_or(number.min) / number.step;
        let steps = if larger { current.floor() + 1. } else { current.ceil() - 1. };
        let text = app_settings::format((steps * number.step).clamp(number.min, number.max));
        if text != self.text(field) {
            if self.focus == Some(field) {
                self.caret = text.len();
            }
            *self.text_mut(field) = text;
            self.touched();
        }
    }

    /// Set the opacity to a whole percent.
    fn set_opacity(&mut self, opacity: f32) {
        let opacity = (opacity.clamp(0., 1.) * 100.).round() / 100.;
        if opacity != self.edited.opacity {
            self.edited.opacity = opacity;
            self.touched();
        }
    }

    /// Set the gap between panes to an even number of pixels.
    fn set_gap(&mut self, gap: f32) {
        let gap = ((gap.clamp(0., f32::from(MAX_PANE_GAP)) / 2.).round() * 2.) as u8;
        if gap != self.edited.pane_gap {
            self.edited.pane_gap = gap;
            self.touched();
        }
    }

    /// Set the value of the slider being dragged to where the pointer is along it, or scroll
    /// the card or the list to where its scrollbar's thumb is dragged.
    fn slide(&mut self, panel: &Panel, layout: &Layout) {
        let (x, y) = (self.pointer.x as f32, self.pointer.y as f32);
        let row = match self.sliding {
            Some(Target::Opacity) => Row::Opacity,
            Some(Target::Gap) => Row::Gap,
            Some(Target::Scrollbar(bar)) => {
                let (Some(scrollbar), grab) = (panel.bar(bar), self.thumb_grab) else { return };
                match bar {
                    Bar::Card => self.card_scroll = scrollbar.scrolled(y, grab),
                    Bar::Fonts => {
                        if let Some(list) = &mut self.font_list {
                            list.first = scrollbar.first_row(y, grab);
                        }
                    },
                    Bar::Logos => {
                        if let Some(picker) = &mut self.logos {
                            picker.first = scrollbar.first_row(y, grab);
                        }
                    },
                }
                return;
            },
            _ => return,
        };
        let Some(page) = &panel.settings else { return };
        let (start, length) = RowsPanel::track(page.control(row), layout);
        let fraction = along(start, length, x);
        match row {
            Row::Gap => self.set_gap(fraction * f32::from(MAX_PANE_GAP)),
            _ => self.set_opacity(fraction),
        }
    }

    /// The focused pane's highlight as typed, or none for the automatic one.
    fn color(&self, field: Field) -> Option<Rgb> {
        parse_hex(self.text(field))
    }

    /// Set the color of the picker's field, or none for the automatic one, as it chose it.
    fn set_color(&mut self, color: Option<Rgb>) {
        let Some(&(field, _)) = self.picker.as_ref() else { return };
        let text = color.map(|color| color.to_string()).unwrap_or_default();
        if self.focus == Some(field) {
            self.caret = text.len();
        }
        if text != self.text(field) {
            *self.text_mut(field) = text;
            self.touched();
        }
    }

    /// Take choice `index` of the menu, and close it.
    fn pick_item(&mut self, index: usize) {
        let Some(menu) = self.menu.take() else { return };
        let row = Row::Key(menu.key);
        if index < row.menu().len() && row.chosen(&self.edited) != Some(index) {
            row.choose(&mut self.edited, index);
            self.touched();
        }
    }

    fn focus(&mut self, field: Field) {
        // Only the picker's own field types alongside it.
        if self.picker.as_ref().is_some_and(|(picked, _)| *picked != field) {
            self.picker = None;
        }
        self.menu = None;
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
        if self.sliding.is_some() {
            self.slide(&panel, layout);
            return true;
        }
        if let (Some(picker), Some(parts)) = (&mut self.picker, panel.picker) {
            if let Some(color) = picker.1.drag(&parts, point) {
                self.set_color(Some(color));
                return true;
            }
        }
        if let Some(drag) = &mut self.drag {
            let y = point.y as f32;
            drag.y = y;
            drag.moving |= (y - drag.start).abs() >= layout.px(4.);
            if !drag.moving {
                return false;
            }
            // Move the dragged profile to the slot under it, scrolling at either end.
            let (Some(&(shown, first)), Some(&(_, last))) = (panel.rows.first(), panel.rows.last())
            else {
                return true;
            };
            let pitch = layout.px(38.);
            let top = (y - drag.grab).clamp(first.y - pitch, last.y + pitch);
            let slot = (shown as f32 + ((top - first.y) / pitch).round()).max(0.) as usize;
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
        if let (Some((_, picker)), Some(parts)) = (&mut self.picker, panel.picker) {
            if parts.popover.contains(point) {
                if let Some(color) = picker.press(&parts, point) {
                    self.set_color(color);
                }
                return false;
            }
        }
        let target = self.target(&panel, point);
        // A click elsewhere closes the menu.
        if !matches!(target, Some(Target::Item(_) | Target::Menu(_))) {
            self.menu = None;
        }
        let logos = matches!(
            target,
            Some(Target::Logo(_) | Target::Scrollbar(Bar::Logos) | Target::Tile | Target::Brand)
        );
        if !logos {
            self.logos = None;
        }
        // A click elsewhere closes the color picker, but for typing its color.
        let picked = self.picker.as_ref().map(|(field, _)| *field);
        if !matches!(target, Some(Target::Well(field) | Target::Field(field)) if Some(field) == picked)
        {
            self.picker = None;
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
            Target::Page(page) => self.show(page),
            Target::Corners(corners) => {
                if corners != self.edited.corners {
                    self.edited.corners = corners;
                    self.touched();
                }
            },
            Target::Restore(restore) => {
                if restore != self.edited.restore_session {
                    self.edited.restore_session = restore;
                    self.touched();
                }
            },
            Target::Step(field, larger) => self.step(field, larger),
            Target::Choice(row, index) => {
                if row.chosen(&self.edited) != Some(index) {
                    row.choose(&mut self.edited, index);
                    self.touched();
                }
            },
            Target::Toggle(toggle) => {
                toggle.flip(&mut self.edited);
                // The logo's text cannot be typed while it is hidden.
                self.focus = self.focus.filter(|field| self.enabled(Target::Field(*field)));
                self.touched();
            },
            Target::Brand if self.logos.is_some() => self.logos = None,
            Target::Brand => self.open_logos(Logos::Brand, layout),
            Target::Opacity | Target::Gap => {
                self.sliding = Some(target);
                self.slide(&panel, layout);
            },
            // The thumb is dragged from where it was pressed; a press on the track moves the
            // thumb's middle there first.
            Target::Scrollbar(bar) => {
                let Some(scrollbar) = panel.bar(bar) else { return false };
                self.thumb_grab = scrollbar.grab(point.y as f32);
                self.sliding = Some(target);
                self.slide(&panel, layout);
            },
            Target::Menu(key) if self.menu.as_ref().is_some_and(|menu| menu.key == key) => {
                self.menu = None
            },
            Target::Menu(key) => {
                let highlighted = Row::Key(key).chosen(&self.edited).unwrap_or_default();
                let first = highlighted.saturating_sub(MENU_ROWS - 1);
                self.menu = Some(Menu { key, highlighted, first });
                self.focus = None;
            },
            Target::Item(index) => self.pick_item(index),
            // The picker is still open only for this well's color.
            Target::Well(_) if self.picker.is_some() => self.picker = None,
            Target::Well(field) => {
                let colors = &config.colors;
                let swatches: [Rgb; SWATCHES] = [
                    colors.normal.blue,
                    colors.normal.cyan,
                    colors.normal.green,
                    colors.normal.yellow,
                    colors.normal.red,
                    colors.normal.magenta,
                    colors.bright.blue,
                    colors.primary.foreground,
                ];
                self.picker = Some((field, ColorPicker::new(self.color(field), swatches)));
                self.focus = None;
            },
            Target::Picker(_) => (),
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
            Target::Tile => self.open_logos(Logos::Profile, layout),
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
        self.sliding = None;
        if let Some(picker) = &mut self.picker {
            picker.1.release();
        }
    }

    pub fn scroll(&mut self, layout: &Layout, down: bool) {
        let (panel, count) = (self.panel(layout), self.candidates().len());
        // The color picker covers what lies under it.
        if panel.picker.is_some_and(|parts| parts.popover.contains(self.pointer)) {
            return;
        }

        if let (Some(menu), Some((rect, rows, row))) = (&mut self.menu, panel.menu) {
            if rect.contains(self.pointer) {
                let count = Row::Key(menu.key).menu().len();
                let max = count.saturating_sub((rows.h / row).round() as usize);
                let first = menu.first.min(max);
                menu.first = if down { (first + 1).min(max) } else { first.saturating_sub(1) };
                return;
            }
        }

        if let (Some(picker), Some((rect, cells, cell, columns))) = (&mut self.logos, panel.logos) {
            if rect.contains(self.pointer) {
                let rows = picker.kind.count().div_ceil(columns);
                let max = rows.saturating_sub((cells.h / cell).round() as usize);
                let first = picker.first.min(max);
                picker.first = if down { (first + 1).min(max) } else { first.saturating_sub(1) };
                return;
            }
        }

        // The wheel turns the font's size and the opacity under the pointer.
        let over = |rect: Rect| rect.contains(self.pointer);
        if let Some(page) = &panel.settings {
            let stepped = page.rows.iter().filter(|(.., control)| over(*control));
            let field = stepped.flat_map(|(row, ..)| row.fields()).find(|field| field.stepper());
            if let Some(field) = field.filter(|_| self.font_list().is_none()) {
                return self.step(field, !down);
            }
            if over(page.control(Row::Opacity)) {
                return self.set_opacity(self.edited.opacity + if down { -0.01 } else { 0.01 });
            }
            if over(page.control(Row::Gap)) && self.enabled(Target::Gap) {
                return self.set_gap(f32::from(self.edited.pane_gap) + if down { -2. } else { 2. });
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
        let max = self.slots().saturating_sub(panel.room);
        let first = self.scroll.min(max);
        self.scroll = if down { (first + 1).min(max) } else { first.saturating_sub(1) };
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
        if !field.right_aligned() {
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
        // The picker shows the color typed, once it is one.
        let Some(&(field, _)) = self.picker.as_ref() else { return };
        let text = self.text(field).trim();
        let typed = if text.is_empty() { Some(None) } else { parse_hex(text).map(Some) };
        if let (Some((_, picker)), Some(color)) = (&mut self.picker, typed) {
            picker.set(color);
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
        if self.picker_key(event, modifiers)
            || self.menu_key(event.logical_key.as_ref(), layout)
            || self.logo_key(event, layout)
            || self.font_key(event, layout)
        {
            return false;
        }
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => return self.close(),
            Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown))
                if self.focus.is_some_and(|field| field.number().is_some()) =>
            {
                if let Some(field) = self.focus {
                    self.step(field, key == NamedKey::ArrowUp);
                }
            },
            Key::Character("s") if command => self.save(config),
            Key::Character("v") if command => self.insert(&paste()),
            Key::Named(NamedKey::Tab) => {
                let enabled = |field: &Field| self.enabled(Target::Field(*field));
                let fields: Vec<_> = self.fields().into_iter().filter(enabled).collect();
                if fields.is_empty() {
                    return false;
                }
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

    /// Handle a key for the color picker, returning whether it was used.
    fn picker_key(&mut self, event: &KeyEvent, modifiers: ModifiersState) -> bool {
        let Some(picker) = &mut self.picker else { return false };
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape | NamedKey::Enter) => self.picker = None,
            // The arrows move through the square while no field has the caret.
            Key::Named(key) if self.focus.is_none() => {
                let Some(color) = picker.1.key(key, modifiers.shift_key()) else { return false };
                self.set_color(Some(color));
            },
            _ => return false,
        }
        true
    }

    /// Handle a key for the menu of choices, returning whether it was used.
    fn menu_key(&mut self, key: Key<&str>, layout: &Layout) -> bool {
        let Some(menu) = &self.menu else { return false };
        let count = Row::Key(menu.key).menu().len();
        let index = match key {
            Key::Named(NamedKey::Escape) => {
                self.menu = None;
                return true;
            },
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                self.pick_item(menu.highlighted);
                return true;
            },
            Key::Named(NamedKey::ArrowDown) => (menu.highlighted + 1).min(count.saturating_sub(1)),
            Key::Named(NamedKey::ArrowUp) => menu.highlighted.saturating_sub(1),
            _ => return false,
        };
        let shown =
            self.panel(layout).menu.map_or(1, |(_, rows, row)| (rows.h / row).round() as usize);
        if let Some(menu) = &mut self.menu {
            menu.highlighted = index;
            menu.first = menu.first.min(index).max((index + 1).saturating_sub(shown.max(1)));
        }
        true
    }

    /// Handle a key for the logo picker, returning whether it was used.
    fn logo_key(&mut self, event: &KeyEvent, layout: &Layout) -> bool {
        let Some(picker) = &self.logos else { return false };
        let columns = self.panel(layout).logos.map_or(1, |(.., columns)| columns);
        let (index, last) = (picker.highlighted, picker.kind.count() - 1);
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

    /// The application's settings as typed, or the page to fix and what to fix there.
    fn edited_app_settings(&self) -> Result<AppSettings, (Page, String)> {
        // Left as it was, the interface's size is kept, also one the settings would not offer.
        let size = match self.text(Field::UiFontSize).trim() {
            text if text == self.saved_text(Field::UiFontSize) => self.saved_app_settings.size,
            text => {
                let number = Field::UiFontSize.number().unwrap_or(Number {
                    min: 0.,
                    max: 0.,
                    step: 1.,
                    whole: false,
                    optional: false,
                });
                let (low, high) = (number.min, number.max);
                let size = text.trim_end_matches('%').parse::<f32>().ok();
                size.filter(|size| (low..=high).contains(size)).ok_or_else(|| {
                    let error = format!("Font size must be a number from {low} to {high}");
                    (Page::Appearance, error)
                })?
            },
        };
        let mut edited = AppSettings {
            family: self.text(Field::UiFont).trim().into(),
            size,
            pane_highlight: match self.text(Field::PaneHighlight).trim() {
                "" => None,
                text => Some(parse_hex(text).ok_or((
                    Page::Appearance,
                    "Focus highlight: write a color as #RRGGBB".into(),
                ))?),
            },
            ..self.edited.clone()
        };
        // The logo's text is kept as typed, so an unedited save leaves it as it was.
        edited.brand.title = self.text(Field::BrandTitle).into();
        edited.brand.subtitle = self.text(Field::BrandSubtitle).into();

        // Keys of `alacritty.toml` keep their text, and are checked as they would be written.
        upstream::check(&edited.upstream, &self.saved_app_settings.upstream)
            .map_err(|(key, error)| (Page::of_key(key), error))?;
        Ok(edited)
    }

    fn save(&mut self, config: &UiConfig) {
        let edited = match self.edited_app_settings() {
            Ok(edited) => edited,
            Err((page, error)) => {
                self.status = Some((error, true));
                self.show(page);
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
        layer.text(title.x, dialog.y + px(25.), self.page.title().into(), palette.muted, false);
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

        // The application's pages and the profiles in a sidebar styled like the tab track.
        let radius = layout.radius(PANEL_RADIUS);
        layer.outlined(panel.sidebar, radius, palette.track, palette.track_outline, line);
        let section =
            |slot: Rect| Rect::new(panel.sidebar.x + px(14.), slot.y + px(10.), px(200.), px(22.));
        for (heading, name) in panel.headings.iter().zip(["APPLICATION", "PROFILES"]) {
            if let Some(slot) = heading {
                left(&mut layer, section(*slot), name, palette.muted, false);
            }
        }
        for &(page, rect) in &panel.pages {
            let selected = self.page == page;
            let radius = layout.radius(ITEM_RADIUS);
            let surface = if selected {
                layer.outlined(rect, radius, palette.raised, palette.raised_outline, line);
                palette.raised
            } else if hover(Target::Page(page)) {
                layer.rounded(rect, radius, palette.hover);
                palette.hover
            } else {
                palette.track
            };
            let icon = Rect::new(rect.x + px(8.), rect.y, px(18.), rect.h).square(px(18.));
            let color = if selected { palette.foreground } else { palette.muted };
            let center = icon.center();
            match page {
                Page::General => layer.gear(center, px(8.), color, surface),
                // A prompt, as at the start of a command line.
                Page::Terminal => {
                    let prompt = [[-1., -0.7, -0.2, 0.], [-0.2, 0., -1., 0.7], [0.1, 0.8, 1., 0.8]];
                    layer.icon(center, px(6.), px(1.5).max(1.), &prompt, color);
                },
                // A window with its title bar.
                Page::Window => {
                    let frame = Rect::new(center.0 - px(7.5), center.1 - px(6.5), px(15.), px(13.));
                    layer.outlined(frame, layout.radius(2.5), surface, color, px(1.5).max(1.));
                    layer.rect(Rect { h: px(3.5), ..frame }, color);
                },
                Page::Font => layer.label(icon, "A", font, color, true),
                // Three dots of paint.
                Page::Colors => {
                    for (dx, dy) in [(-3.5, 2.5), (3.5, 2.5), (0., -3.5)] {
                        layer.circle((center.0 + px(dx), center.1 + px(dy)), px(3.), color);
                    }
                },
                // A beam, as of a text cursor.
                Page::Cursor => {
                    let beam = [[0., -1., 0., 1.], [-0.4, -1., 0.4, -1.], [-0.4, 1., 0.4, 1.]];
                    layer.icon(center, px(7.), px(1.5).max(1.), &beam, color);
                },
                // Two sliders.
                Page::Advanced => {
                    let tracks = [[-1., -0.5, 1., -0.5], [-1., 0.5, 1., 0.5]];
                    layer.icon(center, px(7.), px(1.5).max(1.), &tracks, color);
                    layout.dot(&mut layer, (center.0 - px(3.), center.1 - px(3.5)), px(2.5), color);
                    layout.dot(&mut layer, (center.0 + px(3.), center.1 + px(3.5)), px(2.5), color);
                },
                // A circle half filled, as for light and dark.
                _ => {
                    let radius = px(7.5);
                    layer.circle(center, radius, color);
                    layer.circle(center, radius - px(1.5).max(1.), surface);
                    let half: Vec<_> = (0..=12)
                        .map(|step| std::f32::consts::PI * (0.5 + step as f32 / 12.))
                        .map(|angle| {
                            (center.0 + radius * angle.cos(), center.1 - radius * angle.sin())
                        })
                        .collect();
                    layer.fill(&half, color, 1.);
                },
            }
            let label = Rect::new(icon.right() + px(10.), rect.y, rect.w - px(36.), rect.h);
            left(&mut layer, label, page.title(), color, selected);
        }
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
        match &panel.settings {
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
            Some(page) => self.draw_rows(&mut content, page, layout, palette),
        }

        for field in self.fields() {
            let rect = panel.fields[field.index()];
            let focused = self.focus == Some(field);
            let outline = if focused { palette.focus } else { palette.field_outline };
            // A number's field ends in its buttons, in one frame.
            let frame = panel.settings.as_ref().and_then(|page| page.frame(field)).unwrap_or(rect);
            content.outlined(frame, layout.radius(7.), palette.field, outline, line);
            let color =
                if self.enabled(Target::Field(field)) { palette.foreground } else { palette.muted };

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
                content.text(left_edge, top + index as f32 * ch, visible.collect(), color, false);
            }

            // An empty field shows what is used in its place.
            if let Some(placeholder) =
                self.placeholder(field, config, layout).filter(|_| text.is_empty())
            {
                let placeholder = font.fit(&placeholder, room, false);
                let x = match field.right_aligned() {
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
                    let chosen = corners == self.edited.corners;
                    let (surface, color) = segment(layer, layout, palette, rect, chosen, hovered);
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
                Target::Restore(restore) => {
                    let chosen = restore == self.edited.restore_session;
                    let (_, color) = segment(layer, layout, palette, rect, chosen, hovered);
                    let word = RESTORE.iter().find(|(r, _)| *r == restore).map_or("", |c| c.1);
                    center(layer, rect, word, color, chosen);
                    continue;
                },
                Target::Choice(row, index) => {
                    let chosen = row.chosen(&self.edited) == Some(index);
                    let (_, color) = segment(layer, layout, palette, rect, chosen, hovered);
                    center(layer, rect, row.choices().get(index).unwrap_or(&""), color, chosen);
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
                Target::Step(_, larger) => {
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
                Target::Opacity | Target::Gap => continue,
                // A well of a color, framed like a field: the color, the cell's colors for one
                // that takes them, or none for one left to its default.
                Target::Well(field) => {
                    let picker = self.picker.as_ref().filter(|(picked, _)| *picked == field);
                    let outline = match picker.is_some() || hovered {
                        true => palette.focus,
                        false => palette.field_outline,
                    };
                    layer.outlined(rect, layout.radius(7.), palette.field, outline, line);
                    let inner = rect.inset(px(3.));
                    let text = self.text(field).trim();
                    let color = match picker {
                        Some((_, picker)) => Some(picker.rgb()),
                        None if field == Field::PaneHighlight && text.is_empty() => {
                            Some(palette.primary)
                        },
                        None => self.color(field),
                    };
                    let cell = ["CellForeground", "CellBackground"]
                        .iter()
                        .position(|cell| cell.eq_ignore_ascii_case(text));
                    match (color, cell) {
                        (Some(color), _) => layer.rounded(inner, layout.radius(4.), color),
                        // The cell's colors, the one named on the left.
                        (None, Some(named)) => {
                            let colors = [
                                config.colors.primary.foreground,
                                config.colors.primary.background,
                            ];
                            let half = Rect { w: (inner.w / 2.).round(), ..inner };
                            layer.rounded(inner, layout.radius(4.), colors[1 - named]);
                            layer.rounded(half, layout.radius(4.), colors[named]);
                        },
                        (None, None) => {
                            none_glyph(layer, inner, palette.muted, palette.field, line)
                        },
                    }
                    continue;
                },
                // The control of a key's choices, framed like a field, ending in a chevron.
                Target::Menu(key) => {
                    let row = Row::Key(key);
                    let open = self.menu.as_ref().is_some_and(|menu| menu.key == key);
                    let enabled = !row.dimmed(&self.edited);
                    let outline = match open || (hovered && enabled) {
                        true => palette.focus,
                        false => palette.field_outline,
                    };
                    layer.outlined(rect, layout.radius(7.), palette.field, outline, line);
                    let chosen =
                        row.chosen(&self.edited).and_then(|index| row.menu().get(index).copied());
                    let color = if enabled { palette.foreground } else { palette.muted };
                    let text = Rect { x: rect.x + px(10.), w: rect.w - px(36.), ..rect };
                    left(layer, text, chosen.unwrap_or_default(), color, false);
                    let chevron = [[-1., -0.5, 0., 0.5], [0., 0.5, 1., -0.5]];
                    let at = (rect.right() - px(16.), rect.center().1);
                    layer.icon(at, px(4.), px(1.3).max(1.), &chevron, palette.muted);
                    continue;
                },
                // A well of the tab bar's logo, framed like a field, faded while it is hidden.
                Target::Brand => {
                    let enabled = self.enabled(target);
                    let open =
                        self.logos.as_ref().is_some_and(|picker| picker.kind == Logos::Brand);
                    let outline = match open || (hovered && enabled) {
                        true => palette.focus,
                        false => palette.field_outline,
                    };
                    layer.outlined(rect, layout.radius(7.), palette.field, outline, line);
                    let (icon, opacity) = (rect.square(px(20.)), if enabled { 1. } else { 0.4 });
                    match icons::brand(&self.edited.brand.icon) {
                        Some(image) => layer.image_alpha(icon, &image, palette.foreground, opacity),
                        None => none_glyph(layer, icon, palette.muted, palette.field, line),
                    }
                    continue;
                },
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
                Target::Toggle(toggle) => {
                    let track = Rect::new(rect.x, rect.center().1 - px(11.), px(40.), px(22.));
                    switch(layer, layout, palette, track, toggle.on(&self.edited));
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

        // The fields scroll inside the card, above its buttons; the thumb lights up under the
        // pointer and while it is dragged.
        if let Some(bar) = panel.scrollbar {
            content.rounded(bar.thumb, layout.radius(1.5), self.thumb_color(Bar::Card, palette));
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

        // Where content is scrolled past an edge of the viewport, it fades out into the card
        // there, as in the settings of macOS, rather than being cut off flush. The fade spans
        // the content's width only, clear of the card's corners and its scrollbar.
        let mut edges = Layer::default();
        let hidden = [panel.scrolled > 0., panel.scrolled < panel.scroll_max];
        for (top, hidden) in [true, false].into_iter().zip(hidden) {
            if hidden {
                let edge = panel.fades[usize::from(!top)];
                for (rect, alpha) in fade(edge, top, line) {
                    edges.rounded_alpha(rect, 0., palette.terminal, alpha);
                }
            }
        }

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

            if let Some(bar) = panel.font_bar {
                popup.rounded(bar.thumb, layout.radius(1.5), self.thumb_color(Bar::Fonts, palette));
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

        // A key's choices, in a list under its control.
        if let (Some(menu), Some((rect, rows, row))) = (&self.menu, panel.menu) {
            let radius = layout.radius(TRACK_RADIUS);
            let shadow = Rect::new(rect.x, rect.y + px(2.), rect.w, rect.h);
            popup.rounded_alpha(shadow, radius + layout.radius(2.), BLACK, 0.3);
            popup.outlined(rect, radius, palette.popup, palette.popup_outline, line);
            let key = Row::Key(menu.key);
            let names = key.menu();
            let shown = (rows.h / row).round() as usize;
            let first = menu.first.min(names.len().saturating_sub(shown));
            let highlighted = match self.hover {
                Some(Target::Item(index)) => index,
                _ => menu.highlighted,
            };
            let chosen = key.chosen(&self.edited);
            for (index, name) in names.iter().enumerate().skip(first).take(shown) {
                let item = Rect::new(rows.x, rows.y + (index - first) as f32 * row, rows.w, row);
                if highlighted == index {
                    popup.rounded(item, layout.radius(ITEM_RADIUS), palette.hover);
                }
                let text = Rect::new(item.x + px(10.), item.y, item.w - px(20.), row);
                left(&mut popup, text, name, palette.foreground, chosen == Some(index));
            }
        }

        // Logos to choose from, in a grid under the tile or the well.
        if let (Some(picker), Some((rect, cells, cell, columns))) = (&self.logos, panel.logos) {
            let radius = layout.radius(TRACK_RADIUS);
            let shadow = Rect::new(rect.x, rect.y + px(2.), rect.w, rect.h);
            popup.rounded_alpha(shadow, radius + layout.radius(2.), BLACK, 0.3);
            popup.outlined(rect, radius, palette.popup, palette.popup_outline, line);

            let kind = picker.kind;
            let count = kind.count();
            let highlighted = match self.hover {
                Some(Target::Logo(index)) => index,
                _ => picker.highlighted,
            };
            let header = Rect::new(
                rect.x + px(12.),
                rect.y + px(4.),
                rect.w - px(24.),
                cells.y - rect.y - px(4.),
            );
            let title = "Logos";
            left(&mut popup, header, title, palette.muted, false);
            let room = header.w - font.width(title, false) - px(16.);
            let hovered = font.fit(kind.name(highlighted), room.max(0.), false);
            let x = header.right() - font.width(&hovered, false);
            popup.text(x, header.center().1 - ch / 2., hovered, palette.foreground, false);

            let rows = ((cells.h / cell).round() as usize).max(1);
            let total = count.div_ceil(columns);
            let first = picker.first.min(total.saturating_sub(rows));
            let profile = self.entries[self.selected].draft();
            // An icon from a file has no cell to mark.
            let chosen = kind.index(self.logo(kind));
            for index in first * columns..count.min((first + rows) * columns) {
                let (row, column) = (index / columns - first, index % columns);
                let x = cells.x + column as f32 * cell;
                let item = Rect::new(x, cells.y + row as f32 * cell, cell, cell).inset(px(3.));
                let surface = if index == highlighted { palette.hover } else { palette.popup };
                let radius = layout.radius(ITEM_RADIUS);
                if chosen == Some(index) {
                    popup.outlined(item, radius, surface, palette.focus, line);
                } else if index == highlighted {
                    popup.rounded(item, radius, surface);
                }
                let icon = item.square(px(26.));
                match (kind, icons::brand(&kind.icon(index))) {
                    (_, Some(image)) => popup.image(icon, &image, palette.foreground),
                    (Logos::Profile, None) => {
                        let own = Profile { icon: String::new(), ..profile.clone() };
                        self.icon(&mut popup, icon, &own, config, palette, surface, layout);
                    },
                    (Logos::Brand, None) => {
                        none_glyph(&mut popup, icon, palette.muted, surface, px(1.5).max(1.))
                    },
                }
            }
            if let Some(bar) = panel.logo_bar {
                popup.rounded(bar.thumb, layout.radius(1.5), self.thumb_color(Bar::Logos, palette));
            }
        }
        if let (Some(picker), Some(parts)) = (&self.picker, panel.picker) {
            let hovered = match self.hover {
                Some(Target::Picker(part)) => Some(part),
                _ => None,
            };
            picker.1.draw(&mut popup, &parts, palette, layout, hovered);
        }
        popup.clip(panel.dialog, ch);
        vec![layer, content, edges, popup]
    }

    /// Color of the thumb of `bar`'s scrollbar, lit under the pointer and while it is dragged.
    fn thumb_color(&self, bar: Bar, palette: &Palette) -> Rgb {
        let target = Some(Target::Scrollbar(bar));
        let lit = self.hover == target || self.sliding == target;
        if lit { mix(palette.muted, palette.foreground, 0.5) } else { palette.muted }
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
            Field::PaneHighlight => Some("Auto".into()),
            Field::BrandTitle => Some("No title".into()),
            Field::BrandSubtitle => Some("No subtitle".into()),
            Field::Key(key, part) => self.key_placeholder(key, part, config),
            _ => None,
        }
    }

    /// What an empty field of a key of `alacritty.toml` stands for.
    fn key_placeholder(&self, key: u8, part: u8, config: &UiConfig) -> Option<String> {
        let key = KEYS.get(usize::from(key))?;
        let text = match key.kind {
            Kind::Text(placeholder) => placeholder.into(),
            Kind::Face(_) if part == 0 && key.path == "font.normal" => {
                FontDescription::default().family
            },
            Kind::Face(_) if part == 0 => "Same as normal".into(),
            Kind::Face(style) => style.into(),
            Kind::Number(number) | Kind::Pair(_, number) if number.optional => "Auto".into(),
            Kind::Color { .. } | Kind::Colors(..) => "Auto".into(),
            Kind::Command if key.path == "terminal.shell" => profiles::shell(config),
            Kind::Command | Kind::List | Kind::Map => "None".into(),
            _ => return None,
        };
        Some(text)
    }

    /// Draw an application's page, but for its fields and buttons.
    fn draw_rows(&self, layer: &mut Layer, page: &RowsPanel, layout: &Layout, palette: &Palette) {
        let px = |value: f32| layout.px(value);
        let (font, ch) = (&*layout.font, layout.line);
        let line = px(1.).max(1.);

        let heading = page.heading;
        layer.text(heading.x, heading.y, self.page.title().into(), palette.foreground, true);
        let note = font.fit(self.page.note(), heading.w, false);
        layer.text(heading.x, heading.y + ch, note, palette.muted, false);

        // The settings in groups under their titles, a line between each, named at the left of
        // their controls.
        let group = mix(palette.terminal, palette.field, 0.5);
        for &(title, name, rect) in &page.sections {
            layer.text(title.x, title.y, font.fit(name, title.w, true), palette.muted, true);
            layer.outlined(rect, layout.radius(10.), group, palette.field_outline, line);
        }
        for &(row, rect, control) in &page.rows {
            if page.sections.iter().all(|(.., group)| group.y != rect.y) {
                let rule = Rect::new(rect.x + px(16.), rect.y, rect.w - px(16.), line);
                layer.rect(rule, palette.field_outline);
            }
            // A row is dimmed while what it refines is off.
            let (name_color, description_color) = match row.dimmed(&self.edited) {
                true => (palette.muted, mix(palette.muted, group, 0.4)),
                false => (palette.foreground, palette.muted),
            };
            let (x, middle) = (rect.x + px(16.), control.center().1);
            let room = (control.x - px(16.) - x).max(0.);
            let lines = description_lines(row, rect, control, layout);
            let top = (middle - (1. + lines.len() as f32) * ch / 2.).round();
            layer.text(x, top, font.fit(row.name(), room, false), name_color, false);
            for (index, text) in lines.iter().enumerate() {
                let y = top + (1. + index as f32) * ch;
                layer.text(x, y, font.fit(text, room, false), description_color, false);
            }

            // Segmented controls sit on a track like the tabs'.
            if matches!(row, Row::Corners | Row::Restore) || !row.choices().is_empty() {
                let radius = layout.radius(10.);
                layer.outlined(control, radius, palette.track, palette.track_outline, line);
            }
        }

        // What restoring the session keeps, marked as a warning where it keeps what panes showed.
        if let Some(note) = page.note {
            let restore = self.edited.restore_session;
            let (mark, color, sign) = match restore {
                RestoreSession::Text => {
                    (palette.caution, mix(palette.caution, palette.foreground, 0.35), "!")
                },
                _ => (mix(palette.muted, group, 0.3), palette.muted, "i"),
            };
            let badge = Rect::new(note.x, note.y, ch, ch).square(px(14.));
            layer.circle(badge.center(), badge.w / 2., mark);
            layer.label(badge, sign, font, Palette::on(mark), true);
            let left = note.x + ch + px(4.);
            let room = note.right() - left;
            let lines = wrap(restore_note(restore), room, |text| font.width(text, false));
            for (index, text) in lines.iter().enumerate() {
                let y = note.y + index as f32 * ch;
                if y + ch <= note.bottom() + 1. {
                    layer.text(left, y, font.fit(text, room, false), color, false);
                }
            }
        }

        // The opacity and the gap: a track filled up to its knob, and the value beside it.
        let opacity = format!("{}%", (self.edited.opacity * 100.).round());
        let pixels = format!("{} px", self.edited.pane_gap);
        let gap_fraction = f32::from(self.edited.pane_gap) / f32::from(MAX_PANE_GAP);
        let gap = !Row::Gap.dimmed(&self.edited);
        let sliders = [
            (Row::Opacity, self.edited.opacity, opacity, true),
            (Row::Gap, gap_fraction, pixels, gap),
        ];
        for (row, fraction, value, enabled) in sliders {
            if !page.rows.iter().any(|(r, ..)| *r == row) {
                continue;
            }
            let slider = page.control(row);
            let (start, length) = RowsPanel::track(slider, layout);
            let (thickness, y) = (px(4.), slider.center().1);
            let track = Rect::new(start, (y - thickness / 2.).round(), length, thickness);
            let empty = mix(palette.field, palette.foreground, 0.15);
            layer.rounded(track, layout.curve(thickness / 2.), empty);
            let knob = start + length * fraction;
            let filled = Rect { w: knob - start, ..track };
            let fill = if enabled { palette.primary } else { mix(empty, palette.foreground, 0.15) };
            layer.rounded(filled, layout.curve(thickness / 2.), fill);
            let size = RowsPanel::knob(layout);
            let (face, text) = match enabled {
                true => (palette.foreground, palette.foreground),
                false => (palette.muted, palette.muted),
            };
            layout.dot(layer, (knob, y), size, palette.raised_outline);
            layout.dot(layer, (knob, y), size - line, face);
            layer.text(slider.right() + px(12.), y - ch / 2., value, text, false);
        }
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
    use serde::Deserialize;

    use super::*;

    /// The layout of a window `width` by `height` pixels, unless the interface's font is missing.
    fn layout(width: u32, height: u32) -> Option<Layout> {
        scaled_layout(width, height, 1.)
    }

    /// The layout of a window `width` by `height` pixels at `scale`.
    fn scaled_layout(width: u32, height: u32, scale: f64) -> Option<Layout> {
        let font = UiFont::new(&Default::default(), scale).ok()?;
        let tabs = super::super::Tabs {
            font: Some(std::rc::Rc::new(font)),
            window: winit::dpi::PhysicalSize::new(width, height),
            scale,
            ..Default::default()
        };
        Some(Layout::new(&tabs))
    }

    fn at(x: f32, y: f32) -> PhysicalPosition<f64> {
        PhysicalPosition::new(f64::from(x), f64::from(y))
    }

    #[test]
    fn tracks_and_thumbs_follow_the_pointer() {
        assert_eq!(along(10., 100., 60.), 0.5);
        assert_eq!((along(10., 100., -500.), along(10., 100., 500.)), (0., 1.));

        let track = Rect::new(0., 100., 3., 400.);
        let least = thumb(track, 50., 0., 10_000., 16.);
        assert_eq!((least.y, least.h), (100., 16.));
        // A thumb dragged to where it is drawn scrolls by as much as it shows.
        for scrolled in [0., 150., 300.] {
            let thumb = thumb(track, 300., scrolled, 300., 16.);
            assert_eq!(thumb.h, 200.);
            assert_eq!(scrolled_to(track, thumb.h, thumb.y, 300.), scrolled);
        }
        // Past either end of its track, it scrolls to that end.
        assert_eq!(scrolled_to(track, 200., -1000., 300.), 0.);
        assert_eq!(scrolled_to(track, 200., 1000., 300.), 300.);
        assert_eq!(scrolled_to(track, 400., 200., 300.), 0.);
    }

    #[test]
    fn sliders_follow_a_drag_until_released() {
        let Some(layout) = layout(1200, 1000) else { return };
        let config = UiConfig::default();
        // The opacity is the window's, and the gap between panes the fork's look.
        let mut settings = Settings::new(&config, Page::Window, || ());
        let control = |settings: &Settings, row| {
            settings.panel(&layout).settings.as_ref().unwrap().control(row)
        };
        let opacity = control(&settings, Row::Opacity);
        let press = |settings: &mut Settings, point| {
            settings.moved(&layout, point);
            settings.press(&layout, point, MouseButton::Left, &config, || ());
        };

        // A press on the track moves the knob there.
        let (start, length) = RowsPanel::track(opacity, &layout);
        let y = opacity.center().1;
        press(&mut settings, at(start + length / 4., y));
        assert_eq!(settings.edited.opacity, 0.25);
        // The drag goes on past the slider, the dialog and the window.
        settings.moved(&layout, at(-100., y + 300.));
        assert_eq!(settings.edited.opacity, 0.);
        settings.moved(&layout, at(5000., -50.));
        assert_eq!(settings.edited.opacity, 1.);
        settings.release();
        settings.moved(&layout, at(start, y));
        assert_eq!(settings.edited.opacity, 1.);

        settings.show(Page::Appearance);
        let gap = control(&settings, Row::Gap);
        let (start, length) = RowsPanel::track(gap, &layout);
        press(&mut settings, at(start, gap.center().1));
        assert_eq!(settings.edited.pane_gap, 0);
        settings.moved(&layout, at(start + length, 0.));
        assert_eq!(settings.edited.pane_gap, MAX_PANE_GAP);
        settings.release();
        assert!(settings.dirty);
    }

    #[test]
    fn scrollbar_follows_a_drag_until_released() {
        let Some(layout) = layout(1200, 500) else { return };
        let config = UiConfig::default();
        let mut settings = Settings::new(&config, Page::Appearance, || ());
        let panel = settings.panel(&layout);
        let bar = panel.scrollbar.expect("the page scrolls in a short window");
        let (track, thumb, x) = (bar.track, bar.thumb, bar.band.center().0);
        let target = settings.target(&panel, at(x, thumb.center().1));
        assert_eq!(target, Some(Target::Scrollbar(Bar::Card)));

        // The thumb keeps where it was grabbed, even dragged far off the scrollbar.
        settings.press(&layout, at(x, thumb.y + 2.), MouseButton::Left, &config, || ());
        assert_eq!(settings.card_scroll, 0.);
        settings.moved(&layout, at(x - 600., thumb.y + 2. + (track.h - thumb.h) / 2.));
        assert!((settings.card_scroll - panel.scroll_max / 2.).abs() < 0.01);
        settings.moved(&layout, at(x, 10_000.));
        assert_eq!(settings.card_scroll, panel.scroll_max);
        settings.release();
        settings.moved(&layout, at(x, track.y));
        assert_eq!(settings.card_scroll, panel.scroll_max);

        // A press on the track brings the thumb's middle there.
        settings.press(&layout, at(x, track.y + 1.), MouseButton::Left, &config, || ());
        settings.release();
        assert_eq!(settings.card_scroll, 0.);
    }

    #[test]
    fn scrollbars_take_the_pointer_by_thumb_or_track() {
        let track = Rect::new(0., 100., 3., 400.);
        assert_eq!(Scrollbar::new(track, 10., 12., 50., 0., 0., 16.), None);
        let bar = Scrollbar::new(track, 10., 12., 300., 150., 300., 16.).unwrap();
        assert_eq!((bar.band.x, bar.band.w, bar.band.y, bar.band.h), (-2., 12., 100., 400.));
        assert_eq!((bar.thumb.y, bar.thumb.h), (200., 200.));
        // On the thumb, it keeps where it was grabbed; on the track, the middle comes there.
        assert_eq!(bar.grab(210.), 10.);
        assert_eq!(bar.grab(450.), 100.);
        assert_eq!(bar.scrolled(210., bar.grab(210.)), 150.);
        assert_eq!(bar.scrolled(150., bar.grab(150.)), 0.);
        // Lists scroll by whole rows, and no further than their ends.
        let rows = Scrollbar::new(track, 10., 12., 8., 0., 92., 16.).unwrap();
        assert_eq!(rows.first_row(-1000., 0.), 0);
        assert_eq!(rows.first_row(1000., 0.), 92);
        let halfway = track.y + (track.h - rows.thumb.h) / 2.;
        assert_eq!(rows.first_row(halfway, 0.), 46);
    }

    #[test]
    fn font_list_scrollbar_follows_a_drag_until_released() {
        let Some(layout) = layout(1200, 1000) else { return };
        let config = UiConfig::default();
        let mut settings = Settings::new(&config, Page::Appearance, || ());
        settings.fonts =
            (0..200).map(|i| Family { name: format!("Font {i:03}"), mono: false }).collect();
        settings.focus(Field::UiFont);
        settings.font_list.as_mut().unwrap().first = 0;
        let panel = settings.panel(&layout);
        let bar = panel.font_bar.expect("200 fonts do not fit the list");
        let (x, max) = (bar.band.center().0, bar.max as usize);
        let target = settings.target(&panel, at(x, bar.thumb.center().1));
        assert_eq!(target, Some(Target::Scrollbar(Bar::Fonts)));
        let first = |settings: &Settings| settings.font_list().unwrap().first;

        // The drag goes on past the list and the dialog, and keeps the list open.
        settings.press(&layout, at(x, bar.thumb.y + 2.), MouseButton::Left, &config, || ());
        assert_eq!(first(&settings), 0);
        settings.moved(&layout, at(x - 800., 10_000.));
        assert_eq!(first(&settings), max);
        settings.moved(&layout, at(x + 800., -10_000.));
        assert_eq!(first(&settings), 0);
        settings.release();
        settings.moved(&layout, at(x, bar.track.bottom()));
        assert_eq!(first(&settings), 0);
        assert_eq!(settings.focus, Some(Field::UiFont));

        // A press on the track brings the thumb's middle there.
        settings.press(&layout, at(x, bar.track.bottom() - 1.), MouseButton::Left, &config, || ());
        settings.release();
        assert_eq!(first(&settings), max);
    }

    #[test]
    fn logo_picker_scrollbar_follows_a_drag_until_released() {
        let Some(layout) = layout(1200, 400) else { return };
        let config = UiConfig::default();
        let mut settings = Settings::new(&config, Page::Appearance, || ());
        settings.open_logos(Logos::Brand, &layout);
        settings.logos.as_mut().unwrap().first = 0;
        let panel = settings.panel(&layout);
        let bar = panel.logo_bar.expect("the logos do not fit a short window");
        let (x, max) = (bar.band.center().0, bar.max as usize);
        let target = settings.target(&panel, at(x, bar.thumb.center().1));
        assert_eq!(target, Some(Target::Scrollbar(Bar::Logos)));
        let first = |settings: &Settings| settings.logos.as_ref().unwrap().first;

        settings.press(&layout, at(x, bar.thumb.y + 2.), MouseButton::Left, &config, || ());
        settings.moved(&layout, at(-500., 10_000.));
        assert_eq!(first(&settings), max);
        settings.release();
        settings.moved(&layout, at(x, bar.track.y));
        assert_eq!(first(&settings), max);

        settings.press(&layout, at(x, bar.track.y + 1.), MouseButton::Left, &config, || ());
        settings.release();
        assert_eq!(first(&settings), 0);
    }

    /// Every page in windows short, tall and narrow, at one and two times the scale.
    fn every_page(mut check: impl FnMut(&Layout, &Settings, &Panel, &str)) {
        for scale in [1., 2.] {
            for (width, height) in [(1200, 1000), (1200, 500), (700, 420)] {
                let size = |value: u32| (f64::from(value) * scale) as u32;
                let Some(layout) = scaled_layout(size(width), size(height), scale) else { return };
                for page in PAGES.into_iter().chain([Page::Profile]) {
                    let mut settings = Settings::new(&UiConfig::default(), page, || ());
                    settings.status = Some(("Saved to alacritty.toml".into(), false));
                    for scroll in [0., f32::MAX] {
                        settings.card_scroll = scroll;
                        let panel = settings.panel(&layout);
                        let name = format!("{page:?} {width}x{height} @{scale} scrolled {scroll}");
                        check(&layout, &settings, &panel, &name);
                    }
                }
            }
        }
    }

    #[test]
    fn content_keeps_clear_of_the_footer() {
        every_page(|layout, _, panel, name| {
            let gap = layout.px(FOOTER_GAP);
            let footer = |target| panel.buttons.iter().find(|(t, _)| *t == target).map(|b| b.1);
            let (cancel, save) = (footer(Target::Cancel).unwrap(), footer(Target::Save).unwrap());
            let viewport = panel.viewport;
            // The viewport ends a gap above the footer's status and buttons.
            for rect in [cancel, save, panel.status].into_iter().chain(footer(Target::Delete)) {
                assert!(viewport.bottom() + gap <= rect.y + 0.01, "{name}: {rect:?}");
            }
            // The buttons sit as far in from the card's bottom as from its right, centered
            // in line with each other and with the sidebar's buttons.
            let card = panel.card;
            assert_eq!(card.right() - save.right(), card.bottom() - save.bottom(), "{name}");
            assert_eq!((cancel.y, cancel.h), (save.y, save.h), "{name}");
            let middle = panel.new_profile.center().1;
            assert!((middle - save.center().1).abs() <= 1., "{name}");
            // The scrollbar runs inside the viewport, and the fades lie inside it too.
            if let Some(bar) = panel.scrollbar {
                assert!(bar.track.y >= viewport.y && bar.track.bottom() <= viewport.bottom());
                assert!(bar.band.bottom() <= viewport.bottom(), "{name}");
                for band in panel.fades {
                    assert!(band.right() < bar.track.x, "{name}");
                }
            }
            for band in panel.fades {
                assert!(band.y >= viewport.y && band.bottom() <= viewport.bottom(), "{name}");
                assert!(band.x > card.x && band.right() < card.right(), "{name}");
            }
        });
    }

    #[test]
    fn last_row_scrolls_fully_into_view() {
        every_page(|layout, settings, panel, name| {
            let viewport = panel.viewport;
            // What ends the page: the group of rows, or the profile's switches.
            let (first, last) = match &panel.settings {
                Some(page) => (page.heading.y, page.bottom()),
                None => {
                    let shown = panel.buttons.iter().find(|(t, _)| *t == Target::ToggleShown);
                    (panel.tile.y, shown.unwrap().1.bottom())
                },
            };
            if settings.card_scroll == 0. {
                assert!(first >= viewport.y + layout.px(16.), "{name}");
            } else if panel.scroll_max > 0. {
                // Scrolled to the end, the last row shows whole, with room below it.
                assert_eq!(panel.scrolled, panel.scroll_max, "{name}");
                assert!(last + layout.px(16.) <= viewport.bottom() + 0.01, "{name}");
                assert!(last + layout.px(32.) >= viewport.bottom(), "{name}");
            } else {
                assert!(last <= viewport.bottom(), "{name}");
            }
        });
    }

    #[test]
    fn fades_ease_out_from_the_edge() {
        let band = Rect::new(10., 100., 50., 16.);
        for top in [true, false] {
            let strips = fade(band, top, 2.);
            assert_eq!(strips.len(), 8);
            assert_eq!(strips.iter().map(|(rect, _)| rect.h).sum::<f32>(), band.h);
            assert!(strips.iter().all(|(rect, _)| rect.x == band.x && rect.w == band.w));
            assert!(strips.windows(2).all(|pair| pair[0].1 > pair[1].1));
            let edge = if top { band.y } else { band.bottom() - 2. };
            assert_eq!(strips[0].0.y, edge);
            assert!(strips[0].1 > 0.8 && strips[7].1 < 0.05);
        }
    }

    #[test]
    fn copies_get_unique_names() {
        let taken = |name: &str| name == "Shell (copy)";
        assert_eq!(copy_name("Shell", |_| false), "Shell (copy)");
        assert_eq!(copy_name("Shell", taken), "Shell (copy 2)");
        assert_eq!(copy_name("Shell (copy)", taken), "Shell (copy 2)");
    }

    /// The field of the key at `path`.
    fn key_field(path: &str, part: u8) -> Field {
        Field::Key(upstream::index(path).unwrap() as u8, part)
    }

    /// The row of the key at `path`.
    fn key_row(path: &str) -> Row {
        Row::Key(upstream::index(path).unwrap() as u8)
    }

    #[test]
    fn pages_list_their_rows() {
        let general: Vec<_> = Page::General.rows().collect();
        assert_eq!(general[..2], [Row::Restore, key_row("window.startup_mode")]);
        let appearance: Vec<_> = Page::Appearance.rows().collect();
        assert_eq!(appearance.len(), 11);
        assert_eq!(appearance[8..], [Row::Logo, Row::LogoIcon, Row::LogoText]);
        let background =
            Page::Window.sections().into_iter().find(|(title, _)| *title == "Background");
        assert_eq!(background.map(|(_, rows)| rows[0]), Some(Row::Opacity));
        assert_eq!(Page::Font.rows().next(), Some(key_row("font.normal")));
        assert_eq!(Page::Profile.rows().count(), 0);
        // Every key on this system is on one page, in one row, and every page has sections.
        let rows: Vec<_> = PAGES.iter().flat_map(|page| page.rows()).collect();
        for (index, row) in rows.iter().enumerate() {
            assert!(!rows[index + 1..].contains(row), "{row:?}");
        }
        for (index, key) in KEYS.iter().enumerate() {
            let shown = rows.contains(&Row::Key(index as u8));
            assert_eq!(shown, key.available(), "{}", key.path);
            if key.parts() > 0 && key.available() {
                assert_eq!(Page::of(Field::Key(index as u8, 0)), Page::of_key(index));
            }
        }
        for page in PAGES {
            assert!(
                page.sections().iter().all(|(title, rows)| !title.is_empty() && !rows.is_empty())
            );
        }
        // Every switch flips the setting it shows, and every choice is the one chosen.
        for row in rows {
            let mut settings = AppSettings::new(&UiConfig::default());
            let count = row.choices().len().max(row.menu().len());
            for index in 0..count {
                row.choose(&mut settings, index);
                assert_eq!(row.chosen(&settings), Some(index), "{row:?}");
            }
            if let Some(toggle) = row.toggle() {
                let on = toggle.on(&settings);
                toggle.flip(&mut settings);
                assert_eq!(toggle.on(&settings), !on, "{row:?}");
            }
        }
    }

    #[test]
    fn segments_share_their_track() {
        let track = Rect::new(10., 20., 206., 30.);
        let parts = segments(track, &[20., 40., 40.], 3.);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].x, 13.);
        assert_eq!(parts[2].right(), 213.);
        for pair in parts.windows(2) {
            assert_eq!(pair[0].right(), pair[1].x);
        }
        assert_eq!((parts[0].w, parts[1].w, parts[2].w), (40., 80., 80.));
        assert!(parts.iter().all(|part| part.y == 23. && part.h == 24.));
        // A point picks the segment under it.
        let at = |x: f64| parts.iter().position(|p| p.contains(PhysicalPosition::new(x, 30.)));
        assert_eq!((at(14.), at(100.), at(200.), at(5.)), (Some(0), Some(1), Some(2), None));
    }

    #[test]
    fn logo_cells_stand_for_icons() {
        let first = icons::names().next().unwrap();
        let bundled = format!("{}{first}", icons::BUNDLED_PREFIX);
        assert_eq!(Logos::Brand.icon(0), "alacritty");
        assert_eq!(Logos::Brand.icon(1), "");
        assert_eq!(Logos::Brand.icon(2), bundled);
        assert_eq!(Logos::Brand.name(1), "None");
        assert_eq!(Logos::Profile.icon(0), "");
        assert_eq!(Logos::Profile.icon(1), bundled);
        for kind in [Logos::Brand, Logos::Profile] {
            assert_eq!(kind.count(), kind.leading().len() + icons::names().count());
            for index in 0..kind.count() {
                assert_eq!(kind.index(&kind.icon(index)), Some(index), "{kind:?} {index}");
            }
            // Files and unknown logos have no cell.
            assert_eq!(kind.index("~/logo.svg"), None);
            assert_eq!(kind.index("bundled:nothing"), None);
        }
    }

    #[test]
    fn dimmed_rows_do_nothing() {
        let mut edited = AppSettings::new(&UiConfig::default());
        edited.brand.show = false;
        edited.panel = false;
        let dimmed: Vec<_> = Page::Appearance.rows().filter(|row| row.dimmed(&edited)).collect();
        assert_eq!(dimmed, [Row::Gap, Row::LogoIcon, Row::LogoText]);
        assert!(Page::General.rows().all(|row| !row.dimmed(&edited)));
    }

    #[test]
    fn notes_wrap_and_warn_of_secrets() {
        assert!(restore_note(RestoreSession::Text).contains("secrets"));
        let lines = wrap("one two three four", 9., |text| text.len() as f32);
        assert_eq!(lines, ["one two", "three", "four"]);
        assert_eq!(wrap("", 9., |text| text.len() as f32), Vec::<String>::new());
    }

    #[test]
    fn general_and_appearance_save_their_keys() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("alacritty.toml");
        std::fs::write(&path, "# Mine.\n[window]\nopacity = 1\n").unwrap();
        let mut config = UiConfig::default();
        config.config_paths = vec![path.clone()];

        let mut settings = Settings::new(&config, Page::General, || ());
        settings.edited.restore_session = RestoreSession::Text;
        Toggle::Pin.flip(&mut settings.edited);
        Toggle::WheelZoom.flip(&mut settings.edited);
        settings.show(Page::Appearance);
        assert_eq!(settings.page, Page::Appearance);
        settings.logos = Some(LogoPicker { kind: Logos::Brand, highlighted: 0, first: 0 });
        settings.pick_logo(2);
        assert!(settings.logos.is_none());
        *settings.text_mut(Field::BrandTitle) = "Work".into();
        settings.text_mut(Field::BrandSubtitle).clear();
        // Going back to General keeps what was edited.
        settings.show(Page::General);
        settings.save(&config);
        assert_eq!(settings.reload.as_ref(), Some(&path));

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Mine.\n[window]\nopacity = 1\n"), "{text}");
        let window: toml::Table =
            toml::from_str::<toml::Table>(&text).unwrap()["tabbed"].as_table().unwrap().clone();
        assert_eq!(window["restore_session"].as_str(), Some("text"));
        assert_eq!(window["pin_tab_bar"].as_bool(), Some(false));
        assert_eq!(window["wheel_zoom"].as_bool(), Some(false));
        let brand = &window["brand"];
        let first = icons::names().next().unwrap();
        assert_eq!(brand["icon"].as_str(), Some(format!("bundled:{first}").as_str()));
        assert_eq!(brand["title"].as_str(), Some("Work"));
        assert_eq!(brand["subtitle"].as_str(), Some(""));
        assert!(brand.get("show").is_none());

        // Saving again writes nothing.
        settings.save(&config);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

        // Nor does saving a logo title with spaces around it, unedited.
        let spaced = "[tabbed.brand]\ntitle = \" Work \"\n";
        std::fs::write(&path, spaced).unwrap();
        config.tabbed.brand.title = " Work ".into();
        Settings::new(&config, Page::Appearance, || ()).save(&config);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), spaced);
    }

    #[test]
    fn upstream_settings_save_to_alacritty_keys() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("alacritty.toml");
        std::fs::write(&path, "# Mine.\n[font]\nsize = 12 # Big.\n").unwrap();
        let mut config = UiConfig::default();
        config.config_paths = vec![path.clone()];
        config.font = config.font.clone().with_size(crossfont::Size::new(12.));

        let mut settings = Settings::new(&config, Page::Font, || ());
        assert_eq!(settings.text(key_field("font.size", 0)), "12");
        settings.step(key_field("font.size", 0), true);
        *settings.text_mut(key_field("font.normal", 0)) = "Iosevka".into();
        settings.show(Page::Terminal);
        *settings.text_mut(key_field("scrolling.history", 0)) = "50000".into();
        Toggle::Key(upstream::index("selection.save_to_clipboard").unwrap() as u8)
            .flip(&mut settings.edited);
        key_row("bell.animation").choose(&mut settings.edited, 3);
        settings.show(Page::Cursor);
        key_row("cursor.style.shape").choose(&mut settings.edited, 2);
        settings.show(Page::Colors);
        *settings.text_mut(key_field("colors.primary.background", 0)) = "#000000".into();
        settings.show(Page::General);
        *settings.text_mut(key_field("window.dimensions", 0)) = "120".into();
        *settings.text_mut(key_field("window.dimensions", 1)) = "40".into();
        key_row("window.startup_mode").choose(&mut settings.edited, 1);
        settings.save(&config);
        assert_eq!(settings.status.as_ref().map(|status| status.1), Some(false));

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Mine.\n[font]\nsize = 13 # Big.\n"), "{text}");
        let value: toml::Value = toml::from_str(&text).unwrap();
        let saved = UiConfig::deserialize(value).unwrap();
        assert_eq!(saved.font.normal().family, "Iosevka");
        assert_eq!(saved.scrolling.history(), 50_000);
        assert_eq!(saved.cursor.style().shape, alacritty_terminal::vte::ansi::CursorShape::Beam);
        assert_eq!(saved.colors.primary.background, Rgb::new(0, 0, 0));
        assert!(saved.selection.save_to_clipboard);
        assert!(saved.window.maximized());
        assert_eq!(saved.window.dimensions().map(|d| (d.columns, d.lines)), Some((120, 40)));
        assert!(text.contains("animation = \"EaseOutSine\""), "{text}");
        // Unchanged, nothing more is written.
        settings.save(&config);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn wrong_values_show_their_page() {
        let mut settings = Settings::new(&UiConfig::default(), Page::Terminal, || ());
        let error = |settings: &Settings| settings.edited_app_settings().unwrap_err();
        *settings.text_mut(key_field("scrolling.multiplier", 0)) = "2.5".into();
        assert_eq!(error(&settings).0, Page::Terminal);
        assert!(error(&settings).1.contains("whole number"), "{:?}", error(&settings));
        *settings.text_mut(key_field("scrolling.multiplier", 0)) = "2".into();
        *settings.text_mut(key_field("window.padding", 0)) = "-1".into();
        assert_eq!(error(&settings).0, Page::Window);
        *settings.text_mut(key_field("window.padding", 0)) = "4".into();
        // A window's size needs both its columns and its lines, or neither.
        *settings.text_mut(key_field("window.dimensions", 0)) = "100".into();
        assert_eq!(error(&settings).0, Page::General);
        settings.text_mut(key_field("window.dimensions", 0)).clear();
        *settings.text_mut(key_field("colors.cursor", 1)) = "blue".into();
        assert_eq!(error(&settings).0, Page::Colors);
        *settings.text_mut(key_field("colors.cursor", 1)) = "CellForeground".into();
        assert!(settings.edited_app_settings().is_ok());

        // A failed save shows the page of what to fix.
        *settings.text_mut(key_field("font.size", 0)) = "huge".into();
        settings.save(&UiConfig::default());
        assert_eq!(settings.page, Page::Font);
        assert!(settings.status.as_ref().is_some_and(|status| status.1));
    }

    #[test]
    fn steppers_take_whole_steps_within_bounds() {
        let mut settings = Settings::new(&UiConfig::default(), Page::Terminal, || ());
        let history = key_field("scrolling.history", 0);
        *settings.text_mut(history) = "10500".into();
        settings.step(history, true);
        assert_eq!(settings.text(history), "11000");
        settings.step(history, false);
        assert_eq!(settings.text(history), "10000");
        *settings.text_mut(history) = "100000".into();
        settings.step(history, true);
        assert_eq!(settings.text(history), "100000");
        let thickness = key_field("cursor.thickness", 0);
        settings.step(thickness, true);
        assert_eq!(settings.text(thickness), "0.2");
        // An empty size starts from its least.
        let columns = key_field("window.dimensions", 0);
        settings.step(columns, true);
        assert_eq!(settings.text(columns), "2");
    }

    #[test]
    fn every_page_places_every_control() {
        let Some(layout) = layout(1200, 1000) else { return };
        for page in PAGES {
            let settings = Settings::new(&UiConfig::default(), page, || ());
            let panel = settings.panel(&layout);
            let rows = panel.settings.as_ref().unwrap();
            assert!(!rows.sections.is_empty(), "{page:?}");
            for &(row, rect, control) in &rows.rows {
                assert!(control.w > 0. && control.right() <= rect.right(), "{row:?}");
                assert!(control.y >= rect.y && control.bottom() <= rect.bottom(), "{row:?}");
                for field in row.fields() {
                    let field = panel.fields[field.index()];
                    let inside = control.x <= field.x && field.right() <= control.right() + 0.5;
                    assert!(field.w > 0. && inside, "{row:?}: {field:?} in {control:?}");
                }
            }
        }
    }

    #[test]
    fn menus_choose_with_the_pointer_and_the_keys() {
        let Some(layout) = layout(1200, 1000) else { return };
        let config = UiConfig::default();
        let mut settings = Settings::new(&config, Page::Advanced, || ());
        let row = key_row("debug.log_level");
        let Row::Key(key) = row else { unreachable!() };
        let control = settings.panel(&layout).settings.unwrap().control(row);
        let click = |settings: &mut Settings, point| {
            settings.moved(&layout, point);
            settings.press(&layout, point, MouseButton::Left, &config, || ());
        };
        click(&mut settings, at(control.center().0, control.center().1));
        assert_eq!(settings.menu.as_ref().map(|menu| menu.key), Some(key));
        let (_, rows, height) = settings.panel(&layout).menu.unwrap();
        // The second row is Error.
        click(&mut settings, at(rows.center().0, rows.y + 1.5 * height));
        assert!(settings.menu.is_none());
        assert_eq!(settings.edited.upstream[usize::from(key)].text[0], "Error");
        // The keys move through the choices, and Enter takes one.
        click(&mut settings, at(control.center().0, control.center().1));
        assert!(settings.menu_key(Key::Named(NamedKey::ArrowDown), &layout));
        assert!(settings.menu_key(Key::Named(NamedKey::Enter), &layout));
        assert_eq!(settings.edited.upstream[usize::from(key)].text[0], "Warn");
        assert!(settings.dirty);
    }

    #[test]
    fn color_wells_open_the_picker_for_their_color() {
        let Some(layout) = layout(1200, 1000) else { return };
        let config = UiConfig::default();
        let mut settings = Settings::new(&config, Page::Colors, || ());
        let field = key_field("colors.primary.foreground", 0);
        let well = settings
            .panel(&layout)
            .buttons
            .iter()
            .find(|(t, _)| *t == Target::Well(field))
            .unwrap()
            .1;
        let point = at(well.center().0, well.center().1);
        settings.moved(&layout, point);
        settings.press(&layout, point, MouseButton::Left, &config, || ());
        assert_eq!(settings.picker.as_ref().map(|(field, _)| *field), Some(field));
        assert!(settings.panel(&layout).picker.is_some());
        settings.set_color(Some(Rgb::new(1, 2, 3)));
        assert_eq!(settings.text(field), "#010203");
        // Pressing the well again closes the picker.
        settings.press(&layout, point, MouseButton::Left, &config, || ());
        assert!(settings.picker.is_none());
    }

    #[test]
    fn the_sidebar_scrolls_its_pages_and_profiles() {
        let Some(layout) = layout(700, 420) else { return };
        let mut settings = Settings::new(&UiConfig::default(), Page::General, || ());
        for index in 0..6 {
            settings.entries.push(Entry::new(&Profile::new(&format!("Shell {index}"), "")));
        }
        let panel = settings.panel(&layout);
        assert!(panel.room < settings.slots());
        assert!(panel.headings[0].is_some() && panel.headings[1].is_none());
        // Each slot shown fits above the sidebar's buttons.
        let pages = panel.pages.iter().map(|(_, rect)| rect);
        for rect in pages.chain(panel.rows.iter().map(|(_, rect)| rect)) {
            assert!(rect.bottom() <= panel.new_profile.y, "{rect:?}");
        }
        // Scrolled to its end, the last profile shows.
        settings.pointer = at(panel.sidebar.center().0, panel.sidebar.y + 40.);
        for _ in 0..40 {
            settings.scroll(&layout, true);
        }
        let panel = settings.panel(&layout);
        assert_eq!(panel.rows.last().map(|(index, _)| *index), Some(settings.entries.len() - 1));
        // Selecting a profile brings its row into view.
        settings.select(0, &layout);
        assert!(settings.panel(&layout).rows.iter().any(|(index, _)| *index == 0));
    }
}

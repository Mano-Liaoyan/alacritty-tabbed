//! The application's pages: their sections and rows, the fork's own settings and every key of
//! `alacritty.toml` that [`upstream::KEYS`] lists, each named at the left with its control at the
//! right.

use crate::config::tabbed::RestoreSession;

use super::super::app_settings::AppSettings;
use super::super::app_settings::upstream::{self, KEYS, Kind};
use super::Field;

/// Page of the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// Startup and behaviour, the configuration file and the environment.
    General,
    /// The window: its title, level, decorations, background and padding.
    Window,
    /// The fork's look: corners, panels, the tab bar and its logo.
    Appearance,
    /// The terminal's font, its styles and spacing.
    Font,
    Colors,
    Cursor,
    /// The shell, scrolling, selection, mouse and keyboard, and the bell.
    Terminal,
    /// Rendering and logging.
    Advanced,
    /// The selected profile.
    Profile,
}

/// The application's pages, in the sidebar's order.
pub const PAGES: [Page; 8] = [
    Page::General,
    Page::Window,
    Page::Appearance,
    Page::Font,
    Page::Colors,
    Page::Cursor,
    Page::Terminal,
    Page::Advanced,
];

/// The fork's rows of a page under a title, which keys of the same section join.
struct Section {
    title: &'static str,
    rows: &'static [Row],
}

impl Page {
    /// Where the page's keys of `alacritty.toml` are.
    fn area(self) -> Option<upstream::Area> {
        Some(match self {
            Page::General => upstream::Area::General,
            Page::Window => upstream::Area::Window,
            Page::Font => upstream::Area::Font,
            Page::Colors => upstream::Area::Colors,
            Page::Cursor => upstream::Area::Cursor,
            Page::Terminal => upstream::Area::Terminal,
            Page::Advanced => upstream::Area::Advanced,
            Page::Appearance | Page::Profile => return None,
        })
    }

    /// The fork's sections, before the keys' own; an empty one only places the keys' section of
    /// its title.
    fn own_sections(self) -> &'static [Section] {
        match self {
            Page::General => &[Section { title: "Startup", rows: &[Row::Restore] }, Section {
                title: "Behaviour",
                rows: &[Row::Pin, Row::WheelZoom],
            }],
            Page::Window => &[Section { title: "Window", rows: &[] }, Section {
                title: "Background",
                rows: &[Row::Opacity],
            }],
            Page::Appearance => &[
                Section {
                    title: "Shape",
                    rows: &[Row::Corners, Row::Panel, Row::Gap, Row::Highlight],
                },
                Section {
                    title: "Interface",
                    rows: &[Row::UiFont, Row::UiFontSize, Row::TabTrack, Row::IconTiles],
                },
                Section { title: "Logo", rows: &[Row::Logo, Row::LogoIcon, Row::LogoText] },
            ],
            _ => &[],
        }
    }

    /// Sections of the application's pages: each title, and its rows.
    pub fn sections(self) -> Vec<(&'static str, Vec<Row>)> {
        let own = self.own_sections().iter();
        let mut sections: Vec<_> =
            own.map(|section| (section.title, section.rows.to_vec())).collect();
        let keys = KEYS.iter().enumerate();
        for (index, key) in keys.filter(|(_, key)| Some(key.area) == self.area() && key.available())
        {
            let row = Row::Key(index as u8);
            match sections.iter_mut().find(|(title, _)| *title == key.section) {
                Some((_, rows)) => rows.push(row),
                None => sections.push((key.section, vec![row])),
            }
        }
        sections.retain(|(_, rows)| !rows.is_empty());
        sections
    }

    /// Rows of the application's pages.
    pub fn rows(self) -> impl Iterator<Item = Row> {
        self.sections().into_iter().flat_map(|(_, rows)| rows)
    }

    /// The application's page that has `field`.
    #[cfg(test)]
    pub(super) fn of(field: Field) -> Page {
        let has = |page: &Page| page.rows().any(|row| row.fields().contains(&field));
        PAGES.into_iter().find(has).unwrap_or(Page::Profile)
    }

    /// The application's page that has the key at `index` of [`KEYS`].
    pub fn of_key(index: usize) -> Page {
        let has = |page: &Page| page.rows().any(|row| row == Row::Key(index as u8));
        PAGES.into_iter().find(has).unwrap_or(Page::General)
    }

    pub fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Window => "Window",
            Page::Appearance => "Appearance",
            Page::Font => "Font",
            Page::Colors => "Colors",
            Page::Cursor => "Cursor",
            Page::Terminal => "Terminal",
            Page::Advanced => "Advanced",
            Page::Profile => "Profiles",
        }
    }

    /// The line under the page's title.
    pub fn note(self) -> &'static str {
        match self {
            Page::Advanced => "Bindings, hints and indexed colors are edited in alacritty.toml",
            _ => "For every window; saved to alacritty.toml",
        }
    }
}

/// A setting turned on and off by a switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    /// The panel holding the terminal, set in from the window's edges.
    Panel,
    /// The track behind the tabs.
    TabTrack,
    /// The tiles behind the icons of a tab's panes.
    IconTiles,
    /// The logo at the left of the tab bar.
    Logo,
    Pin,
    WheelZoom,
    /// A key of `alacritty.toml`.
    Key(u8),
}

impl Toggle {
    pub fn on(self, settings: &AppSettings) -> bool {
        match self {
            Toggle::Panel => settings.panel,
            Toggle::TabTrack => settings.tab_track,
            Toggle::IconTiles => settings.icon_tiles,
            Toggle::Logo => settings.brand.show,
            Toggle::Pin => settings.pin_tab_bar,
            Toggle::WheelZoom => settings.wheel_zoom,
            Toggle::Key(key) => settings.upstream[usize::from(key)].on,
        }
    }

    pub fn flip(self, settings: &mut AppSettings) {
        let on = match self {
            Toggle::Panel => &mut settings.panel,
            Toggle::TabTrack => &mut settings.tab_track,
            Toggle::IconTiles => &mut settings.icon_tiles,
            Toggle::Logo => &mut settings.brand.show,
            Toggle::Pin => &mut settings.pin_tab_bar,
            Toggle::WheelZoom => &mut settings.wheel_zoom,
            Toggle::Key(key) => &mut settings.upstream[usize::from(key)].on,
        };
        *on ^= true;
    }
}

/// Choices of restoring the session, and their names.
pub const RESTORE: [(RestoreSession, &str); 3] = [
    (RestoreSession::Off, "Off"),
    (RestoreSession::Layout, "Layout"),
    (RestoreSession::Text, "Layout and text"),
];

/// What restoring the session keeps, told under its choices.
pub fn restore_note(restore: RestoreSession) -> &'static str {
    match restore {
        RestoreSession::Off => "Windows and tabs start afresh each time.",
        RestoreSession::Layout => {
            "Tabs, splits, names, folders and text sizes; each pane starts a new shell."
        },
        RestoreSession::Text => {
            "Also keeps the last 2,000 lines of each pane. They may hold secrets you typed; saved \
             only on this computer."
        },
    }
}

/// Most choices a segmented control offers; more drop from a menu.
const SEGMENTS: usize = 4;

/// A setting on a page of rows, named at the left with its control at the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Restore,
    Pin,
    WheelZoom,
    Corners,
    UiFont,
    UiFontSize,
    Opacity,
    Panel,
    Gap,
    Highlight,
    TabTrack,
    IconTiles,
    Logo,
    LogoIcon,
    LogoText,
    /// The key of `alacritty.toml` at this index of [`KEYS`].
    Key(u8),
}

impl Row {
    pub fn name(self) -> &'static str {
        match self {
            Row::Restore => "Restore session",
            Row::Pin => "Always show tab bar",
            Row::WheelZoom if cfg!(target_os = "macos") => "⌘+scroll zooms text",
            Row::WheelZoom => "Ctrl+scroll zooms text",
            Row::Corners => "Corners",
            Row::UiFont => "Interface font",
            Row::UiFontSize => "Font size",
            Row::Opacity => "Background opacity",
            Row::Panel => "Terminal panel",
            Row::Gap => "Pane gap",
            Row::Highlight => "Focus highlight",
            Row::TabTrack => "Tab track",
            Row::IconTiles => "Icon tiles",
            Row::Logo => "Logo",
            Row::LogoIcon => "Logo icon",
            Row::LogoText => "Logo text",
            Row::Key(key) => KEYS[usize::from(key)].name,
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Row::Restore => "Reopen windows and tabs at start",
            Row::Pin => "Off: it slides in at the top edge",
            Row::WheelZoom => "Of the focused pane; not saved",
            Row::Corners => "Window, panels, tabs and buttons",
            Row::UiFont => "Tab bar, menus and settings",
            Row::UiFontSize => "Interface text, in points",
            Row::Opacity => "Behind the terminal's text",
            Row::Panel => "Set in from the window's edges",
            Row::Gap => "Space between a tab's panes",
            Row::Highlight => "Border of the focused pane",
            Row::TabTrack => "Behind the tabs",
            Row::IconTiles => "Behind the icons in tabs",
            Row::Logo => "Top left of the tab bar",
            Row::LogoIcon => "Alacritty Tabbed's, Alacritty's, a bundled logo or none",
            Row::LogoText => "{host} is this computer's name",
            Row::Key(key) => KEYS[usize::from(key)].description,
        }
    }

    /// The key of `alacritty.toml` the row edits.
    pub fn key(self) -> Option<&'static upstream::Key> {
        match self {
            Row::Key(key) => KEYS.get(usize::from(key)),
            _ => None,
        }
    }

    /// The switch of the row, if it has one.
    pub fn toggle(self) -> Option<Toggle> {
        match self {
            Row::Pin => Some(Toggle::Pin),
            Row::WheelZoom => Some(Toggle::WheelZoom),
            Row::Panel => Some(Toggle::Panel),
            Row::TabTrack => Some(Toggle::TabTrack),
            Row::IconTiles => Some(Toggle::IconTiles),
            Row::Logo => Some(Toggle::Logo),
            Row::Key(key) if self.key().is_some_and(|key| key.kind == Kind::Switch) => {
                Some(Toggle::Key(key))
            },
            _ => None,
        }
    }

    /// The fields of the row, in their order.
    pub(super) fn fields(self) -> Vec<Field> {
        match self {
            Row::UiFont => vec![Field::UiFont],
            Row::UiFontSize => vec![Field::UiFontSize],
            Row::Highlight => vec![Field::PaneHighlight],
            Row::LogoText => vec![Field::BrandTitle, Field::BrandSubtitle],
            Row::Key(key) => {
                let parts = self.key().map_or(0, upstream::Key::parts) as u8;
                (0..parts).map(|part| Field::Key(key, part)).collect()
            },
            _ => Vec::new(),
        }
    }

    /// The names a key offers as choices, and the names they are shown by.
    fn options(self) -> &'static [(&'static str, &'static str)] {
        match self.key().map(|key| key.kind) {
            Some(Kind::Choice(choices)) => choices,
            _ => &[],
        }
    }

    /// Names of the choices of the row's segmented control, which Corners and Restore draw
    /// their own way.
    pub fn choices(self) -> Vec<&'static str> {
        match self.options() {
            options if options.len() <= SEGMENTS => options.iter().map(|(_, name)| *name).collect(),
            _ => Vec::new(),
        }
    }

    /// Names of the choices the row's menu offers, when it has more than a segmented control.
    pub fn menu(self) -> Vec<&'static str> {
        match self.options() {
            options if options.len() > SEGMENTS => options.iter().map(|(_, name)| *name).collect(),
            _ => Vec::new(),
        }
    }

    /// The choice that is chosen; none for a name the row does not offer.
    pub fn chosen(self, edited: &AppSettings) -> Option<usize> {
        let Row::Key(key) = self else { return None };
        let name = &edited.upstream[usize::from(key)].text[0];
        self.options().iter().position(|(option, _)| option == name)
    }

    /// Take choice `index`.
    pub fn choose(self, edited: &mut AppSettings, index: usize) {
        if let (Row::Key(key), Some((name, _))) = (self, self.options().get(index)) {
            edited.upstream[usize::from(key)].text[0] = (*name).into();
        }
    }

    /// Whether the row is dimmed and does nothing, while what it refines is off.
    pub fn dimmed(self, edited: &AppSettings) -> bool {
        match self {
            Row::Gap => !edited.panel,
            Row::LogoIcon | Row::LogoText => !edited.brand.show,
            Row::Key(_) => {
                let needs = self.key().and_then(|key| key.needs).and_then(upstream::index);
                needs.is_some_and(|needs| !edited.upstream[needs].set())
            },
            _ => false,
        }
    }
}

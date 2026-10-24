//! Settings of the whole application, which live in `alacritty.toml`, as the settings edit them.
//!
//! Saving changes only the keys whose values changed, keeping the rest of the file, its comments
//! and its formatting as they are.

use std::path::{Path, PathBuf};
use std::{fs, io};

use toml_edit::{DocumentMut, Item, Table, value};

use crate::config::UiConfig;
use crate::config::tabbed::{Brand, Corners, MAX_PANE_GAP, RestoreSession};
use crate::display::color::Rgb;

use super::profiles;

pub mod upstream;

#[derive(Clone, Debug, PartialEq)]
pub struct AppSettings {
    pub corners: Corners,
    /// Family of the interface's font; empty picks the system's.
    pub family: String,
    /// Size of the interface's font in points.
    pub size: f32,
    /// Opacity of the terminal's background, from 0 to 1.
    pub opacity: f32,
    /// Whether the terminal is held in a panel, and the tabs set on a track.
    pub panel: bool,
    pub tab_track: bool,
    /// Whether the icons of a tab's panes lie on tiles.
    pub icon_tiles: bool,
    /// Space between a tab's panes, in logical pixels.
    pub pane_gap: u8,
    /// Border of the focused pane; none uses the terminal's blue.
    pub pane_highlight: Option<Rgb>,
    /// Whether the tab bar is always shown, rather than sliding in at the top edge.
    pub pin_tab_bar: bool,
    /// Whether Control and the wheel zoom the focused pane's text.
    pub wheel_zoom: bool,
    pub restore_session: RestoreSession,
    /// The logo and names at the left of the tab bar.
    pub brand: Brand,
    /// Alacritty's own settings, under its own keys, as [`upstream::KEYS`] lists them.
    pub upstream: Vec<upstream::State>,
}

impl AppSettings {
    pub fn new(config: &UiConfig) -> Self {
        let window = &config.tabbed;
        Self {
            corners: window.corners,
            family: window.ui_font.family.clone(),
            size: window.ui_font.size,
            opacity: config.window.opacity.as_f32(),
            panel: window.panel,
            tab_track: window.tab_track,
            icon_tiles: window.icon_tiles,
            pane_gap: window.pane_gap.min(MAX_PANE_GAP),
            pane_highlight: window.pane_highlight,
            pin_tab_bar: window.pin_tab_bar,
            wheel_zoom: window.wheel_zoom,
            restore_session: window.restore_session,
            brand: window.brand.clone(),
            upstream: upstream::read(config),
        }
    }

    /// Write the settings that differ from `before` to the configuration file.
    pub fn save(&self, before: &AppSettings, config: &UiConfig) -> io::Result<PathBuf> {
        let path = match config.config_paths.first() {
            Some(path) => path.clone(),
            None => {
                profiles::directory(config).ok_or(io::ErrorKind::NotFound)?.join("alacritty.toml")
            },
        };
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err),
        };
        let text = self.edit(before, &text).map_err(io::Error::other)?;
        fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        fs::write(&path, text)?;
        Ok(path)
    }

    /// The configuration `text` with the settings that differ from `before` set.
    ///
    /// The fork's keys an older version kept in `[window]` are moved to `[tabbed]` first.
    fn edit(&self, before: &AppSettings, text: &str) -> Result<String, String> {
        let mut document: DocumentMut = text.parse().map_err(|err| format!("{err}"))?;
        migrate(&mut document);
        if self.opacity != before.opacity {
            document.entry("window").or_insert_with(section)["opacity"] = number(self.opacity);
        }
        let tabbed = document.entry("tabbed").or_insert_with(section);
        let ui_font = self.family != before.family || self.size != before.size;
        for (table, changed) in [("ui_font", ui_font), ("brand", self.brand != before.brand)] {
            if let Some(tabbed) = tabbed.as_table_mut().filter(|_| changed) {
                tabbed.entry(table).or_insert_with(section);
            }
        }
        if self.corners != before.corners {
            let corners = if self.corners == Corners::Round { "round" } else { "square" };
            tabbed["corners"] = value(corners);
        }
        if self.family != before.family {
            tabbed["ui_font"]["family"] = value(self.family.as_str());
        }
        if self.size != before.size {
            tabbed["ui_font"]["size"] = number(self.size);
        }
        if self.panel != before.panel {
            tabbed["panel"] = value(self.panel);
        }
        if self.tab_track != before.tab_track {
            tabbed["tab_track"] = value(self.tab_track);
        }
        if self.icon_tiles != before.icon_tiles {
            tabbed["icon_tiles"] = value(self.icon_tiles);
        }
        if self.pane_gap != before.pane_gap {
            tabbed["pane_gap"] = value(i64::from(self.pane_gap));
        }
        if self.pin_tab_bar != before.pin_tab_bar {
            tabbed["pin_tab_bar"] = value(self.pin_tab_bar);
        }
        if self.wheel_zoom != before.wheel_zoom {
            tabbed["wheel_zoom"] = value(self.wheel_zoom);
        }
        if self.restore_session != before.restore_session {
            let restore = match self.restore_session {
                RestoreSession::Off => "off",
                RestoreSession::Layout => "layout",
                RestoreSession::Text => "text",
            };
            tabbed["restore_session"] = value(restore);
        }
        let (brand, old) = (&self.brand, &before.brand);
        if brand.show != old.show {
            tabbed["brand"]["show"] = value(brand.show);
        }
        for (key, new, old) in [
            ("icon", &brand.icon, &old.icon),
            ("title", &brand.title, &old.title),
            ("subtitle", &brand.subtitle, &old.subtitle),
        ] {
            if new != old {
                tabbed["brand"][key] = value(new.as_str());
            }
        }
        if self.pane_highlight != before.pane_highlight {
            match self.pane_highlight {
                Some(color) => tabbed["pane_highlight"] = value(color.to_string()),
                // The automatic color is what no color stands for.
                None => {
                    if let Some(tabbed) = tabbed.as_table_like_mut() {
                        tabbed.remove("pane_highlight");
                    }
                },
            }
        }
        let keys = upstream::edit(&self.upstream, &before.upstream, document.as_item_mut());
        keys.map_err(|(_, error)| error)?;
        Ok(document.to_string())
    }
}

/// A table the file lacks, which becomes a section rather than an inline table.
fn section() -> Item {
    let mut table = Table::new();
    table.set_implicit(true);
    Item::Table(table)
}

/// Move the fork's keys an older version read from `[window]` into `[tabbed]`.
///
/// A key already in `[tabbed]` wins over its old copy, which is dropped. Moved keys keep their
/// comments, and a `[window]` left empty and uncommented is dropped.
fn migrate(document: &mut DocumentMut) {
    let Some(window) = document.get_mut("window").and_then(Item::as_table_like_mut) else {
        return;
    };
    // In the file's order, so the moved keys keep theirs.
    let names: Vec<String> = window
        .iter()
        .map(|(name, _)| name.to_owned())
        .filter(|name| OLD_WINDOW_KEYS.contains(&name.as_str()))
        .collect();
    let mut moved = Vec::new();
    for name in names {
        let key = window.key(&name).cloned();
        if let (Some(key), Some(item)) = (key, window.remove(&name)) {
            moved.push((key, item));
        }
    }
    if moved.is_empty() {
        return;
    }
    let empty = match document.get("window") {
        Some(Item::Table(table)) => table.is_empty() && !commented(table.decor()),
        Some(item) => item.as_table_like().is_some_and(|table| table.is_empty()),
        None => false,
    };
    if empty {
        document.remove("window");
    }

    let tabbed = document.entry("tabbed").or_insert_with(section);
    let inline = !tabbed.is_table();
    let Some(tabbed) = tabbed.as_table_like_mut() else { return };
    for (key, mut item) in moved {
        if tabbed.contains_key(key.get()) {
            continue;
        }
        if let Item::Table(table) = &mut item {
            // Printed after the rest of `[tabbed]`, rather than where `[window]` was.
            table.set_position(None);
        }
        if inline {
            item = match item.into_value() {
                Ok(value) => Item::Value(value),
                Err(item) => item,
            };
        }
        tabbed.entry_format(&key).or_insert(item);
    }
}

/// Keys of `[tabbed]` that an older version read from `[window]`.
const OLD_WINDOW_KEYS: [&str; 12] = [
    "tabs",
    "ui_font",
    "corners",
    "panel",
    "tab_track",
    "icon_tiles",
    "pane_gap",
    "pane_highlight",
    "pin_tab_bar",
    "wheel_zoom",
    "restore_session",
    "brand",
];

/// Whether a table's header has a comment before or after it.
fn commented(decor: &toml_edit::Decor) -> bool {
    let has = |raw: Option<&toml_edit::RawString>| {
        raw.and_then(toml_edit::RawString::as_str).is_some_and(|raw| raw.contains('#'))
    };
    has(decor.prefix()) || has(decor.suffix())
}

/// A number, written without a fraction when it has none.
fn number(number: f32) -> Item {
    // Through the shortest decimal, so 0.85 is not written as 0.8500000238418579.
    let number: f64 = number.to_string().parse().unwrap_or_default();
    if number.fract() == 0. { value(number as i64) } else { value(number) }
}

/// A number for a field, without a needless fraction.
pub fn format(number: f32) -> String {
    let rounded = (number * 100.).round() / 100.;
    if rounded.fract() == 0. { format!("{}", rounded as i64) } else { rounded.to_string() }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    fn defaults() -> AppSettings {
        AppSettings {
            corners: Corners::Round,
            family: String::new(),
            size: 10.,
            opacity: 1.,
            panel: true,
            tab_track: true,
            icon_tiles: true,
            pane_gap: 16,
            pane_highlight: None,
            pin_tab_bar: true,
            wheel_zoom: true,
            restore_session: RestoreSession::Off,
            brand: Brand::default(),
            upstream: upstream::read(&UiConfig::default()),
        }
    }

    #[test]
    fn unchanged_settings_leave_the_file_alone() {
        let text = "# Mine.\n[font]\nsize = 12 # Big.\n";
        assert_eq!(defaults().edit(&defaults(), text).unwrap(), text);
        let text = "[window]\nopacity = 1\n\n[tabbed]\npanel = false # Mine.\n";
        assert_eq!(defaults().edit(&defaults(), text).unwrap(), text);
    }

    #[test]
    fn changed_settings_are_written_beside_the_rest() {
        let text = "# Mine.\n[window]\nopacity = 1 # Solid.\ndecorations = \"None\"\n";
        let changed = AppSettings {
            corners: Corners::Square,
            family: "Inter".into(),
            size: 11.5,
            opacity: 0.85,
            panel: false,
            tab_track: true,
            icon_tiles: false,
            pane_gap: 4,
            pane_highlight: Some(Rgb::new(0xff, 0x95, 0)),
            ..defaults()
        };
        let edited = changed.edit(&defaults(), text).unwrap();
        assert!(edited.starts_with("# Mine.\n[window]\n"));
        assert!(edited.contains("decorations = \"None\"\n"));
        let config: toml::Table = toml::from_str(&edited).unwrap();
        let (window, tabbed) = (&config["window"], &config["tabbed"]);
        assert_eq!(window["opacity"].as_float(), Some(0.85));
        assert_eq!(window.as_table().unwrap().len(), 2);
        assert_eq!(tabbed["corners"].as_str(), Some("square"));
        assert_eq!(tabbed["ui_font"]["family"].as_str(), Some("Inter"));
        assert_eq!(tabbed["ui_font"]["size"].as_float(), Some(11.5));
        assert_eq!(tabbed["panel"].as_bool(), Some(false));
        assert!(tabbed.get("tab_track").is_none());
        assert_eq!(tabbed["icon_tiles"].as_bool(), Some(false));
        assert_eq!(tabbed["pane_gap"].as_integer(), Some(4));
        assert_eq!(tabbed["pane_highlight"].as_str(), Some("#ff9500"));
        // What is written is what the configuration reads.
        let config = UiConfig::deserialize(toml::Value::Table(config)).unwrap();
        // The file's own keys of Alacritty, which the settings left alone, are read as they are.
        let read = AppSettings::new(&config);
        assert_eq!(AppSettings { upstream: changed.upstream.clone(), ..read }, changed);
    }

    #[test]
    fn too_wide_gaps_are_narrowed_without_being_rewritten() {
        let mut config = UiConfig::default();
        config.tabbed.pane_gap = 60;
        let settings = AppSettings::new(&config);
        assert_eq!(settings.pane_gap, MAX_PANE_GAP);
        let text = "[tabbed]\npane_gap = 60 # Big.\n";
        assert_eq!(settings.edit(&AppSettings::new(&config), text).unwrap(), text);
    }

    #[test]
    fn automatic_highlight_removes_its_key() {
        let orange = AppSettings { pane_highlight: Some(Rgb::new(0xff, 0x95, 0)), ..defaults() };
        let text = "[tabbed]\npane_highlight = \"#ff9500\" # Orange.\npanel = true\n";
        assert_eq!(defaults().edit(&orange, text).unwrap(), "[tabbed]\npanel = true\n");
        // Nothing is written for a key that is not there.
        assert_eq!(defaults().edit(&orange, "").unwrap(), "");
    }

    #[test]
    fn settings_go_into_dotted_and_inline_tables() {
        let changed = AppSettings { size: 12., ..defaults() };
        for text in ["tabbed.ui_font.family = \"Inter\"\n", "tabbed = { ui_font = { size = 9 } }\n"]
        {
            let edited = changed.edit(&defaults(), text).unwrap();
            let config: toml::Table = toml::from_str(&edited).unwrap();
            assert_eq!(config["tabbed"]["ui_font"]["size"].as_integer(), Some(12), "{edited}");
        }
    }

    #[test]
    fn missing_tables_become_sections() {
        let changed = AppSettings { size: 12., pane_gap: 4, opacity: 0.5, ..defaults() };
        let edited = changed.edit(&defaults(), "").unwrap();
        assert_eq!(
            edited,
            "[window]\nopacity = 0.5\n\n[tabbed]\npane_gap = 4\n\n[tabbed.ui_font]\nsize = 12\n"
        );
    }

    #[test]
    fn bar_session_and_brand_settings_are_written_and_left_alone() {
        let brand = Brand { show: false, icon: "bundled:opensuse".into(), ..Brand::default() };
        let changed = AppSettings {
            pin_tab_bar: false,
            wheel_zoom: false,
            restore_session: RestoreSession::Text,
            brand,
            ..defaults()
        };
        let edited = changed.edit(&defaults(), "").unwrap();
        assert_eq!(
            edited,
            "[tabbed]\npin_tab_bar = false\nwheel_zoom = false\nrestore_session = \
             \"text\"\n\n[tabbed.brand]\nshow = false\nicon = \"bundled:opensuse\"\n"
        );
        // Each level is written by the name the configuration reads.
        for restore in [RestoreSession::Off, RestoreSession::Layout] {
            let before = AppSettings { restore_session: RestoreSession::Text, ..defaults() };
            let after = AppSettings { restore_session: restore, ..defaults() };
            let edited = after.edit(&before, "").unwrap();
            let config: toml::Value = toml::from_str(&edited).unwrap();
            let config = UiConfig::deserialize(config).unwrap();
            assert_eq!(config.tabbed.restore_session, restore);
        }
        // Back to the defaults: each changed key is written again, and nothing else is touched.
        let text = "# Mine.\n[tabbed.brand]\ntitle = \"Work\" # Kept.\n";
        let edited = defaults().edit(&changed, text).unwrap();
        assert!(edited.contains("title = \"Work\" # Kept.\n"), "{edited}");
        for key in ["pin_tab_bar = true", "show = true", "icon = \"alacritty\""] {
            assert!(edited.contains(key), "{key}: {edited}");
        }
        // Unchanged, the file is left byte for byte.
        assert_eq!(changed.edit(&changed, text).unwrap(), text);
    }

    #[test]
    fn old_window_keys_move_to_their_own_table() {
        let text = "# Mine.\n[window]\nopacity = 0.9\n# Wide.\npane_gap = 4\ntabs = \
                    false\n\n[window.ui_font]\nsize = 12 # Big.\n\n[window.brand]\ntitle = \
                    \"Work\"\n\n[font]\nsize = 11\n";
        let changed = AppSettings { corners: Corners::Square, ..defaults() };
        let edited = changed.edit(&defaults(), text).unwrap();
        assert_eq!(
            edited,
            "# Mine.\n[window]\nopacity = 0.9\n\n[font]\nsize = 11\n\n[tabbed]\n# Wide.\npane_gap \
             = 4\ntabs = false\ncorners = \"square\"\n\n[tabbed.ui_font]\nsize = 12 # \
             Big.\n\n[tabbed.brand]\ntitle = \"Work\"\n"
        );
        let config: toml::Value = toml::from_str(&edited).unwrap();
        let config = UiConfig::deserialize(config).unwrap();
        assert_eq!((config.tabbed.pane_gap, config.tabbed.tabs), (4, false));
        assert_eq!((config.tabbed.ui_font.size, &*config.tabbed.brand.title), (12., "Work"));
        assert_eq!(config.tabbed.corners, Corners::Square);
        // Moved once, there is nothing more to move.
        assert_eq!(changed.edit(&changed, &edited).unwrap(), edited);
    }

    #[test]
    fn new_keys_win_over_their_old_copies() {
        let text = "[window]\npane_gap = 4\npanel = false\n\n[tabbed]\npane_gap = 8\n";
        let changed = AppSettings { wheel_zoom: false, ..defaults() };
        let edited = changed.edit(&defaults(), text).unwrap();
        // The `[window]` left empty is dropped, with the old keys.
        assert_eq!(edited, "\n[tabbed]\npane_gap = 8\npanel = false\nwheel_zoom = false\n");
        // A setting changed now wins over both.
        let changed = AppSettings { pane_gap: 2, ..defaults() };
        let edited = changed.edit(&defaults(), text).unwrap();
        assert_eq!(edited, "\n[tabbed]\npane_gap = 2\npanel = false\n");
    }

    #[test]
    fn old_keys_move_out_of_dotted_and_inline_tables() {
        let changed = AppSettings { panel: false, ..defaults() };
        for text in [
            "window.pane_gap = 4\nwindow.opacity = 1\n",
            "window = { pane_gap = 4, opacity = 1 }\n",
            "window = { pane_gap = 4, opacity = 1 }\ntabbed = { tabs = true }\n",
        ] {
            let edited = changed.edit(&defaults(), text).unwrap();
            let config: toml::Table = toml::from_str(&edited).unwrap();
            assert_eq!(config["tabbed"]["pane_gap"].as_integer(), Some(4), "{edited}");
            assert_eq!(config["tabbed"]["panel"].as_bool(), Some(false), "{edited}");
            assert!(config["window"].get("pane_gap").is_none(), "{edited}");
            assert_eq!(config["window"]["opacity"].as_integer(), Some(1), "{edited}");
        }
        // A commented `[window]` left empty is kept.
        let text = "# Window.\n[window]\npanel = true\n";
        let edited = changed.edit(&defaults(), text).unwrap();
        assert_eq!(edited, "# Window.\n[window]\n\n[tabbed]\npanel = false\n");
    }

    #[test]
    fn numbers_drop_needless_fractions() {
        assert_eq!(format(10.), "10");
        assert_eq!(format(10.5), "10.5");
        assert_eq!(format(0.85 * 100.), "85");
    }
}

//! Settings of the whole application, which live in `alacritty.toml`, as the settings edit them.
//!
//! Saving changes only the keys whose values changed, keeping the rest of the file, its comments
//! and its formatting as they are.

use std::path::{Path, PathBuf};
use std::{fs, io};

use toml_edit::{DocumentMut, Item, Table, value};

use crate::config::UiConfig;
use crate::config::window::{Brand, Corners, MAX_PANE_GAP, RestoreSession};
use crate::display::color::Rgb;

use super::profiles;

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
}

impl AppSettings {
    pub fn new(config: &UiConfig) -> Self {
        let window = &config.window;
        Self {
            corners: window.corners,
            family: window.ui_font.family.clone(),
            size: window.ui_font.size,
            opacity: window.opacity.as_f32(),
            panel: window.panel,
            tab_track: window.tab_track,
            icon_tiles: window.icon_tiles,
            pane_gap: window.pane_gap.min(MAX_PANE_GAP),
            pane_highlight: window.pane_highlight,
            pin_tab_bar: window.pin_tab_bar,
            wheel_zoom: window.wheel_zoom,
            restore_session: window.restore_session,
            brand: window.brand.clone(),
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
    fn edit(&self, before: &AppSettings, text: &str) -> Result<String, toml_edit::TomlError> {
        let mut document: DocumentMut = text.parse()?;
        // Tables the file lacks become sections rather than inline tables.
        let section = || {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        };
        let window = document.entry("window").or_insert_with(section);
        let ui_font = self.family != before.family || self.size != before.size;
        for (table, changed) in [("ui_font", ui_font), ("brand", self.brand != before.brand)] {
            if let Some(window) = window.as_table_mut().filter(|_| changed) {
                window.entry(table).or_insert_with(section);
            }
        }
        if self.corners != before.corners {
            let corners = if self.corners == Corners::Round { "round" } else { "square" };
            window["corners"] = value(corners);
        }
        if self.family != before.family {
            window["ui_font"]["family"] = value(self.family.as_str());
        }
        if self.size != before.size {
            window["ui_font"]["size"] = number(self.size);
        }
        if self.opacity != before.opacity {
            window["opacity"] = number(self.opacity);
        }
        if self.panel != before.panel {
            window["panel"] = value(self.panel);
        }
        if self.tab_track != before.tab_track {
            window["tab_track"] = value(self.tab_track);
        }
        if self.icon_tiles != before.icon_tiles {
            window["icon_tiles"] = value(self.icon_tiles);
        }
        if self.pane_gap != before.pane_gap {
            window["pane_gap"] = value(i64::from(self.pane_gap));
        }
        if self.pin_tab_bar != before.pin_tab_bar {
            window["pin_tab_bar"] = value(self.pin_tab_bar);
        }
        if self.wheel_zoom != before.wheel_zoom {
            window["wheel_zoom"] = value(self.wheel_zoom);
        }
        if self.restore_session != before.restore_session {
            let restore = match self.restore_session {
                RestoreSession::Off => "off",
                RestoreSession::Layout => "layout",
                RestoreSession::Text => "text",
            };
            window["restore_session"] = value(restore);
        }
        let (brand, old) = (&self.brand, &before.brand);
        if brand.show != old.show {
            window["brand"]["show"] = value(brand.show);
        }
        for (key, new, old) in [
            ("icon", &brand.icon, &old.icon),
            ("title", &brand.title, &old.title),
            ("subtitle", &brand.subtitle, &old.subtitle),
        ] {
            if new != old {
                window["brand"][key] = value(new.as_str());
            }
        }
        if self.pane_highlight != before.pane_highlight {
            match self.pane_highlight {
                Some(color) => window["pane_highlight"] = value(color.to_string()),
                // The automatic color is what no color stands for.
                None => {
                    if let Some(window) = window.as_table_like_mut() {
                        window.remove("pane_highlight");
                    }
                },
            }
        }
        Ok(document.to_string())
    }
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
    use crate::config::window::WindowConfig;

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
        }
    }

    #[test]
    fn unchanged_settings_leave_the_file_alone() {
        let text = "# Mine.\n[font]\nsize = 12 # Big.\n";
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
        let window = &config["window"];
        assert_eq!(window["corners"].as_str(), Some("square"));
        assert_eq!(window["opacity"].as_float(), Some(0.85));
        assert_eq!(window["ui_font"]["family"].as_str(), Some("Inter"));
        assert_eq!(window["ui_font"]["size"].as_float(), Some(11.5));
        assert_eq!(window["panel"].as_bool(), Some(false));
        assert!(window.get("tab_track").is_none());
        assert_eq!(window["icon_tiles"].as_bool(), Some(false));
        assert_eq!(window["pane_gap"].as_integer(), Some(4));
        assert_eq!(window["pane_highlight"].as_str(), Some("#ff9500"));
    }

    #[test]
    fn too_wide_gaps_are_narrowed_without_being_rewritten() {
        let mut config = UiConfig::default();
        config.window.pane_gap = 60;
        let settings = AppSettings::new(&config);
        assert_eq!(settings.pane_gap, MAX_PANE_GAP);
        let text = "[window]\npane_gap = 60 # Big.\n";
        assert_eq!(settings.edit(&AppSettings::new(&config), text).unwrap(), text);
    }

    #[test]
    fn automatic_highlight_removes_its_key() {
        let orange = AppSettings { pane_highlight: Some(Rgb::new(0xff, 0x95, 0)), ..defaults() };
        let text = "[window]\npane_highlight = \"#ff9500\" # Orange.\npanel = true\n";
        assert_eq!(defaults().edit(&orange, text).unwrap(), "[window]\npanel = true\n");
        // Nothing is written for a key that is not there.
        assert_eq!(defaults().edit(&orange, "").unwrap(), "");
    }

    #[test]
    fn settings_go_into_dotted_and_inline_tables() {
        let changed = AppSettings { size: 12., ..defaults() };
        for text in ["window.ui_font.family = \"Inter\"\n", "window = { ui_font = { size = 9 } }\n"]
        {
            let edited = changed.edit(&defaults(), text).unwrap();
            let config: toml::Table = toml::from_str(&edited).unwrap();
            assert_eq!(config["window"]["ui_font"]["size"].as_integer(), Some(12), "{edited}");
        }
    }

    #[test]
    fn missing_tables_become_sections() {
        let changed = AppSettings { size: 12., pane_gap: 4, ..defaults() };
        let edited = changed.edit(&defaults(), "").unwrap();
        assert_eq!(edited, "[window]\npane_gap = 4\n\n[window.ui_font]\nsize = 12\n");
    }

    #[test]
    fn round_three_settings_are_written_and_left_alone() {
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
            "[window]\npin_tab_bar = false\nwheel_zoom = false\nrestore_session = \
             \"text\"\n\n[window.brand]\nshow = false\nicon = \"bundled:opensuse\"\n"
        );
        // Each level is written by the name the configuration reads.
        for restore in [RestoreSession::Off, RestoreSession::Layout] {
            let before = AppSettings { restore_session: RestoreSession::Text, ..defaults() };
            let after = AppSettings { restore_session: restore, ..defaults() };
            let edited = after.edit(&before, "").unwrap();
            let config: toml::Table = toml::from_str(&edited).unwrap();
            let window = WindowConfig::deserialize(config["window"].clone()).unwrap();
            assert_eq!(window.restore_session, restore);
        }
        // Back to the defaults: each changed key is written again, and nothing else is touched.
        let text = "# Mine.\n[window.brand]\ntitle = \"Work\" # Kept.\n";
        let edited = defaults().edit(&changed, text).unwrap();
        assert!(edited.contains("title = \"Work\" # Kept.\n"), "{edited}");
        for key in ["pin_tab_bar = true", "show = true", "icon = \"alacritty\""] {
            assert!(edited.contains(key), "{key}: {edited}");
        }
        // Unchanged, the file is left byte for byte.
        assert_eq!(changed.edit(&changed, text).unwrap(), text);
    }

    #[test]
    fn numbers_drop_needless_fractions() {
        assert_eq!(format(10.), "10");
        assert_eq!(format(10.5), "10.5");
        assert_eq!(format(0.85 * 100.), "85");
    }
}

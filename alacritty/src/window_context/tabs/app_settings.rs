//! Settings of the whole application, which live in `alacritty.toml`, as the settings edit them.
//!
//! Saving changes only the keys whose values changed, keeping the rest of the file, its comments
//! and its formatting as they are.

use std::path::{Path, PathBuf};
use std::{fs, io};

use toml_edit::{DocumentMut, Item, value};

use crate::config::UiConfig;
use crate::config::window::Corners;
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
    /// Space between a tab's panes, in logical pixels.
    pub pane_gap: u8,
    /// Border of the focused pane; none uses the terminal's blue.
    pub pane_highlight: Option<Rgb>,
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
            pane_gap: window.pane_gap,
            pane_highlight: window.pane_highlight,
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
        let window = &mut document["window"];
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
        if self.pane_gap != before.pane_gap {
            window["pane_gap"] = value(i64::from(self.pane_gap));
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
    use super::*;

    fn defaults() -> AppSettings {
        AppSettings {
            corners: Corners::Round,
            family: String::new(),
            size: 10.,
            opacity: 1.,
            panel: true,
            tab_track: true,
            pane_gap: 16,
            pane_highlight: None,
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
            pane_gap: 4,
            pane_highlight: Some(Rgb::new(0xff, 0x95, 0)),
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
        assert_eq!(window["pane_gap"].as_integer(), Some(4));
        assert_eq!(window["pane_highlight"].as_str(), Some("#ff9500"));
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
    fn numbers_drop_needless_fractions() {
        assert_eq!(format(10.), "10");
        assert_eq!(format(10.5), "10.5");
        assert_eq!(format(0.85 * 100.), "85");
    }
}

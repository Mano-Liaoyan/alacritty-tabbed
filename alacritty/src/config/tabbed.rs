//! The fork's own keys, in a `[tabbed]` table of [`UiConfig`](super::UiConfig) of their own.

use serde::Serialize;

use alacritty_config_derive::ConfigDeserialize;

use crate::config::window::DEFAULT_NAME;
use crate::display::color::Rgb;

/// Widest space between a tab's panes, in logical pixels; wider gaps are narrowed to it.
pub const MAX_PANE_GAP: u8 = 48;

#[derive(ConfigDeserialize, Serialize, Debug, Clone, PartialEq)]
pub struct TabbedConfig {
    /// Show a tab bar and group terminals of a window in tabs.
    pub tabs: bool,

    /// Font of the tab bar, its menus and the settings, apart from the terminal's.
    pub ui_font: UiFont,

    /// Corners of the window, the terminal's panel, the tabs and the controls, all alike.
    pub corners: Corners,

    /// Hold the terminal in a panel set in from the window's edges, instead of filling the window.
    pub panel: bool,

    /// Set the tabs on a track, instead of on the bar itself.
    pub tab_track: bool,

    /// Draw the icons of a tab's panes on tiles, instead of bare and as large as the tiles.
    pub icon_tiles: bool,

    /// Space between the panes of a tab, in logical pixels, up to [`MAX_PANE_GAP`].
    pub pane_gap: u8,

    /// Border of the focused pane; none uses the terminal's blue.
    pub pane_highlight: Option<Rgb>,

    /// Keep the tab bar shown, instead of sliding it in as the pointer reaches the top edge.
    pub pin_tab_bar: bool,

    /// Zoom the focused pane's text with Control and the mouse wheel.
    pub wheel_zoom: bool,

    /// Reopen the windows and tabs open when Alacritty last quit.
    pub restore_session: RestoreSession,

    /// The logo and names at the left of the tab bar.
    pub brand: Brand,
}

impl Default for TabbedConfig {
    fn default() -> Self {
        Self {
            tabs: true,
            ui_font: Default::default(),
            corners: Default::default(),
            panel: true,
            tab_track: true,
            icon_tiles: true,
            pane_gap: 16,
            pane_highlight: None,
            pin_tab_bar: true,
            wheel_zoom: true,
            restore_session: Default::default(),
            brand: Default::default(),
        }
    }
}

/// Font of the interface around the terminal.
#[derive(ConfigDeserialize, Serialize, Debug, Clone, PartialEq)]
pub struct UiFont {
    /// Font family; empty picks the system's interface font.
    pub family: String,

    /// Font size in points.
    pub size: f32,
}

impl Default for UiFont {
    fn default() -> Self {
        // macOS sizes fonts in pixels, other systems in points of 4/3 pixels.
        let size = if cfg!(target_os = "macos") { 13. } else { 10. };
        Self { family: String::new(), size }
    }
}

#[derive(ConfigDeserialize, Serialize, Default, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Corners {
    #[default]
    Round,
    Square,
}

/// What of the windows open when Alacritty quits is reopened when it starts.
#[derive(ConfigDeserialize, Serialize, Default, Debug, Copy, Clone, PartialEq, Eq)]
pub enum RestoreSession {
    #[default]
    Off,
    /// Windows, tabs, panes and their places, directories and text sizes.
    Layout,
    /// The layout and the text last shown in each pane.
    Text,
}

/// The logo and names at the left of the tab bar.
#[derive(ConfigDeserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Brand {
    pub show: bool,
    /// `alacritty`, `bundled:<name>` for a bundled logo, a path to an image, or empty for none.
    pub icon: String,
    /// Names beside the icon, where `{host}` stands for the computer's; empty hides a line.
    pub title: String,
    pub subtitle: String,
}

impl Default for Brand {
    fn default() -> Self {
        Self {
            show: true,
            icon: "alacritty".into(),
            title: DEFAULT_NAME.into(),
            subtitle: "{host}".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use alacritty_config::SerdeReplace;

    use super::*;
    use crate::config::UiConfig;

    fn config(text: &str) -> UiConfig {
        UiConfig::deserialize(toml::from_str::<toml::Value>(text).unwrap()).unwrap()
    }

    #[test]
    fn panes_keep_their_gap_and_highlight_by_default() {
        let config = UiConfig::default();
        assert_eq!(config.tabbed.pane_gap, 16);
        assert_eq!(config.tabbed.pane_highlight, None);
    }

    #[test]
    fn pane_gap_and_highlight_are_read() {
        let config = config(
            "[tabbed]
pane_gap = 4
pane_highlight = \"#FF9500\"",
        );
        assert_eq!(config.tabbed.pane_gap, 4);
        assert_eq!(config.tabbed.pane_highlight, Some(Rgb::new(0xff, 0x95, 0x00)));
    }

    #[test]
    fn round_three_keys_default() {
        let tabbed = UiConfig::default().tabbed;
        assert!(tabbed.pin_tab_bar && tabbed.wheel_zoom && tabbed.icon_tiles);
        assert_eq!(tabbed.restore_session, RestoreSession::Off);
        let brand = &tabbed.brand;
        assert!(brand.show);
        assert_eq!(
            (&*brand.icon, &*brand.title, &*brand.subtitle),
            ("alacritty", "Alacritty", "{host}")
        );
    }

    #[test]
    fn round_three_keys_are_read() {
        for (text, restore) in [
            ("off", RestoreSession::Off),
            ("layout", RestoreSession::Layout),
            ("text", RestoreSession::Text),
        ] {
            let config = config(&format!(
                "[tabbed]
restore_session = \"{text}\""
            ));
            assert_eq!(config.tabbed.restore_session, restore);
        }
        let text = "[tabbed]
pin_tab_bar = false
wheel_zoom = false
[tabbed.brand]
show =                     false
icon = \"bundled:opensuse\"
title = \"\"
";
        let tabbed = config(text).tabbed;
        assert!(!tabbed.pin_tab_bar && !tabbed.wheel_zoom && !tabbed.brand.show);
        assert_eq!((&*tabbed.brand.icon, &*tabbed.brand.title), ("bundled:opensuse", ""));
        // Keys left out keep their defaults.
        assert_eq!(tabbed.brand.subtitle, "{host}");
    }

    #[test]
    fn keys_sit_in_their_own_table() {
        let text = "[tabbed]
tabs = false
[tabbed.ui_font]
size = 12
[window]
title =                     \"Mine\"
dynamic_title = false";
        let config = config(text);
        assert!(!config.tabbed.tabs && !config.window.dynamic_title);
        assert_eq!(config.tabbed.ui_font.size, 12.);
        assert_eq!(config.window.identity.title, "Mine");

        // The old place beside the upstream `[window]` keys is not read.
        let old = self::config(
            "[window]
tabs = false
pane_gap = 4",
        );
        assert!(old.tabbed.tabs);
        assert_eq!(old.tabbed.pane_gap, 16);

        let mut config = UiConfig::default();
        let value = toml::from_str(
            "tabbed.tab_track = false
window.dynamic_padding = true",
        );
        config.replace(value.unwrap()).unwrap();
        assert!(!config.tabbed.tab_track && config.window.dynamic_padding);

        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["tabbed"]["tab_track"], false);
        assert_eq!(json["tabbed"]["ui_font"]["family"], "");
    }
}

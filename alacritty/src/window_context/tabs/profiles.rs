//! Shell profiles for new tabs, kept in `profiles.toml` beside `alacritty.toml`.
//!
//! Until that file exists, the shells installed on the system are offered instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::{env, fs, io};

use log::error;
use serde::{Deserialize, Serialize};

use alacritty_terminal::tty::{Options as PtyOptions, Shell};

use super::paint::Image;
use super::{icons, platform};
use crate::config::UiConfig;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// Command line; empty uses the shell from `alacritty.toml`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// Starting directory; empty starts in the home directory.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub directory: String,
    /// Image file, or text such as an emoji; empty uses the program's own icon.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub icon: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Configuration overrides, in `--option` syntax such as `font.size=14`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// Leave the profile out of the new-tab menu.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Profiles {
    /// Name of the profile for new tabs; empty uses the shell from `alacritty.toml`.
    pub default: String,
    #[serde(rename = "profile")]
    pub list: Vec<Profile>,
}

impl Profiles {
    fn path(config: &UiConfig) -> Option<PathBuf> {
        let directory = match config.config_paths.first().and_then(|path| path.parent()) {
            Some(directory) => directory.to_owned(),
            #[cfg(windows)]
            None => PathBuf::from(env::var_os("APPDATA")?).join("alacritty"),
            #[cfg(not(windows))]
            None => match env::var_os("XDG_CONFIG_HOME") {
                Some(directory) => PathBuf::from(directory),
                None => home::home_dir()?.join(".config"),
            }
            .join("alacritty"),
        };
        Some(directory.join("profiles.toml"))
    }

    /// Saved profiles, or the installed shells while none are saved.
    pub fn load(config: &UiConfig) -> Self {
        let text = Self::path(config).and_then(|path| fs::read_to_string(path).ok());
        match text.map(|text| toml::from_str(&text)) {
            Some(Ok(profiles)) => profiles,
            Some(Err(err)) => {
                error!("Ignoring invalid profiles.toml: {err}");
                Self::installed()
            },
            None => Self::installed(),
        }
    }

    pub fn save(&self, config: &UiConfig) -> io::Result<PathBuf> {
        let path = Self::path(config).ok_or(io::ErrorKind::NotFound)?;
        fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        fs::write(&path, toml::to_string_pretty(self).map_err(io::Error::other)?)?;
        Ok(path)
    }

    pub fn default_profile(&self) -> Option<&Profile> {
        self.list.iter().find(|profile| profile.name == self.default)
    }

    fn installed() -> Self {
        static INSTALLED: OnceLock<Vec<Profile>> = OnceLock::new();
        Self { default: String::new(), list: INSTALLED.get_or_init(platform::shells).clone() }
    }
}

impl Profile {
    pub fn new(name: &str, command: &str) -> Self {
        Self { name: name.into(), command: command.into(), ..Default::default() }
    }

    /// The icon set as text, such as an emoji, rather than as an image file.
    fn text_icon(&self) -> Option<&str> {
        let icon = self.icon.trim();
        (!icon.is_empty() && icon.chars().count() <= 2 && !icon.contains(['.', '/', '\\']))
            .then_some(icon)
    }

    /// Text for the tab's icon when it has no image.
    pub fn badge(&self) -> String {
        if let Some(icon) = self.text_icon() {
            return icon.into();
        }
        let letter = self.name.chars().find(|c| c.is_alphanumeric());
        letter.map_or_else(|| ">".into(), |letter| letter.to_uppercase().collect())
    }

    /// The icon file, or else the icon of the program the profile runs.
    pub fn image(&self, config: &UiConfig) -> Option<Arc<Image>> {
        if self.text_icon().is_some() {
            return None;
        }
        match self.icon.trim() {
            "" if self.command.trim().is_empty() => icons::get(None, &shell(config)),
            "" => icons::get(None, &self.command),
            icon => icons::get(Some(Path::new(&expand(icon))), ""),
        }
    }

    /// Start the terminal with this profile's command, directory and environment.
    ///
    /// The user's own shell, without arguments, starts the way Alacritty starts its default
    /// shell, which on macOS makes it a login shell.
    pub fn apply(&self, pty_config: &mut PtyOptions) {
        let shell = env::var_os("SHELL").and_then(|shell| fs::canonicalize(shell).ok());
        pty_config.shell = match split(&self.command) {
            Some((program, args))
                if !args.is_empty() || fs::canonicalize(&program).ok() != shell =>
            {
                Some(Shell::new(program, args))
            },
            Some(_) => None,
            None => pty_config.shell.take(),
        };
        let directory = self.directory.trim();
        pty_config.working_directory =
            if directory.is_empty() { home::home_dir() } else { Some(expand(directory).into()) };
        pty_config.env.extend(self.env.clone());
    }
}

/// Program of the shell set in `alacritty.toml`, or the system's.
pub fn shell(config: &UiConfig) -> String {
    match &config.terminal.shell {
        Some(shell) => shell.program().into(),
        #[cfg(windows)]
        None => "powershell.exe".into(),
        #[cfg(not(windows))]
        None => env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
    }
}

/// Split a command line into its program and arguments.
#[cfg(windows)]
fn split(command: &str) -> Option<(String, Vec<String>)> {
    // Windows programs parse their own command line, so the arguments are passed verbatim.
    let command = command.trim();
    let end = match command.strip_prefix('"') {
        Some(rest) => rest.find('"').map_or(command.len(), |end| end + 2),
        None => command.find(char::is_whitespace).unwrap_or(command.len()),
    };
    let (program, args) = command.split_at(end);
    let args = Some(args.trim()).filter(|args| !args.is_empty()).map(String::from);
    (!program.is_empty()).then(|| (program.into(), args.into_iter().collect()))
}

/// Split a command line into its program and arguments, like a shell without expansions.
#[cfg(not(windows))]
fn split(command: &str) -> Option<(String, Vec<String>)> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    for c in command.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => {
                quote = Some(c);
                word.get_or_insert_default();
            },
            (None, c) if c.is_whitespace() => words.extend(word.take()),
            (_, c) => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    let mut words = words.into_iter();
    Some((words.next()?, words.collect()))
}

/// Expand `~` and environment variables, as `$NAME`, `${NAME}` or `%NAME%`, in a path.
pub fn expand(path: &str) -> String {
    let mut expanded = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            home::home_dir().map_or_else(String::new, |home| home.display().to_string())
        },
        _ => return expand_variables(path),
    };
    expanded.push_str(&expand_variables(&path[1..]));
    expanded
}

fn expand_variables(path: &str) -> String {
    let mut expanded = String::new();
    let mut rest = path;
    while let Some(start) = rest.find(['$', '%']) {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let (name, end) = match (&rest[start..start + 1], after.strip_prefix('{')) {
            ("%", _) => (after.find('%').map(|end| &after[..end]), 1),
            (_, Some(braced)) => (braced.find('}').map(|end| &braced[..end]), 2),
            _ => (after.split(|c: char| !c.is_alphanumeric() && c != '_').next(), 0),
        };
        let value = name.filter(|name| !name.is_empty()).and_then(|n| Some((n, env::var(n).ok()?)));
        match value {
            Some((name, value)) => {
                expanded.push_str(&value);
                rest = &after[name.len() + end..];
            },
            None => {
                expanded.push_str(&rest[start..start + 1]);
                rest = after;
            },
        }
    }
    expanded.push_str(rest);
    expanded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_round_trip_through_toml() {
        let mut profile = Profile::new("Shell", "sh -l");
        profile.env.insert("NAME".into(), "value".into());
        profile.options.push("font.size=14".into());
        let profiles = Profiles { default: "Shell".into(), list: vec![profile] };
        let text = toml::to_string_pretty(&profiles).unwrap();
        let parsed: Profiles = toml::from_str(&text).unwrap();
        assert_eq!(parsed.list, profiles.list);
        assert_eq!(parsed.default_profile(), profiles.list.first());
    }

    #[test]
    fn badges_use_text_icons_or_initials() {
        assert_eq!(Profile::new("bash", "").badge(), "B");
        let profile = Profile { icon: "🐧".into(), ..Profile::new("Ubuntu", "") };
        assert_eq!(profile.badge(), "🐧");
        let profile = Profile { icon: r"C:\icons\ubuntu.ico".into(), ..Profile::new("Ubuntu", "") };
        assert_eq!(profile.badge(), "U");
    }

    #[test]
    #[cfg(not(windows))]
    fn commands_split_like_a_shell() {
        assert_eq!(split("  "), None);
        let (program, args) = split("ssh -t 'my host' \"a b\" ''").unwrap();
        assert_eq!(program, "ssh");
        assert_eq!(args, ["-t", "my host", "a b", ""]);
    }

    #[test]
    #[cfg(windows)]
    fn commands_keep_their_quoting() {
        let (program, args) = split(r#""C:\Program Files\Git\bin\bash.exe" --login -i"#).unwrap();
        assert_eq!(program, r#""C:\Program Files\Git\bin\bash.exe""#);
        assert_eq!(args, ["--login -i"]);
    }

    #[test]
    fn paths_expand_home_and_variables() {
        let home = home::home_dir().unwrap().display().to_string();
        assert_eq!(expand("~"), home);
        assert_eq!(expand("~/a"), format!("{home}/a"));
        let path = env::var("PATH").unwrap();
        assert_eq!(expand("$PATH/x"), format!("{path}/x"));
        assert_eq!(expand("${PATH}x"), format!("{path}x"));
        assert_eq!(expand("%PATH%x"), format!("{path}x"));
        assert_eq!(expand("100% $"), "100% $");
    }
}

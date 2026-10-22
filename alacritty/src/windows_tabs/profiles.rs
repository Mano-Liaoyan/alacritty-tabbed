//! Shell profiles: discovery, persistence, launch options, and icons.
//!
//! Profiles live in `profiles.toml` beside `alacritty.toml`. Until the user saves from the
//! settings panel, the shells installed on this machine are offered instead.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromFile, GdipCreateBitmapFromScan0, GdipCreateHICONFromBitmap,
    GdipDeleteGraphics, GdipDisposeImage, GdipDrawImageRectI, GdipGetImageGraphicsContext,
    GdipGetImageHeight, GdipGetImageWidth, GdipSetInterpolationMode, GdipSetPixelOffsetMode,
    GdiplusStartup, GdiplusStartupInput, GpBitmap, GpGraphics, GpImage,
    InterpolationModeHighQualityBicubic, PixelOffsetModeHighQuality,
};
use windows_sys::Win32::Storage::FileSystem::SearchPathW;
use windows_sys::Win32::Storage::Packaging::Appx::{
    GetPackagePathByFullName, GetPackagesByPackageFamily,
};
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};
use windows_sys::Win32::UI::Shell::SHDefExtractIconW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, HICON, IMAGE_ICON, LR_LOADFROMFILE, LoadImageW,
};

use super::paint::wide;
use crate::cli::WindowOptions;

/// A shell offered by the new-tab dropdown, in the spirit of Windows Terminal profiles.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Profile {
    pub(super) name: String,
    /// Full command line; empty uses the shell configured in `alacritty.toml`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) command: String,
    /// Starting directory; empty starts in the user's home directory.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) directory: String,
    /// Custom icon file; empty uses the program's own icon.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) icon: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) env: BTreeMap<String, String>,
    /// Alacritty configuration overrides, in `--option` syntax such as `font.size=14`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) options: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) hidden: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Profiles {
    /// Profile used by + and the first tab; empty keeps the shell from `alacritty.toml`.
    pub(super) default: String,
    #[serde(rename = "profile")]
    pub(super) list: Vec<Profile>,
}

impl Profiles {
    fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|path| path.join("alacritty").join("profiles.toml"))
    }

    /// Saved profiles, or the shells installed on this machine when none are saved.
    pub(super) fn load() -> Self {
        let saved = Self::path().and_then(|path| std::fs::read_to_string(path).ok());
        match saved.map(|text| toml::from_str::<Self>(&text)) {
            Some(Ok(profiles)) => profiles,
            Some(Err(err)) => {
                log::error!("Ignoring invalid profiles.toml: {err}");
                Self::discover()
            },
            None => Self::discover(),
        }
    }

    pub(super) fn save(&self) -> Result<PathBuf, String> {
        let path = Self::path().ok_or("no configuration directory")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|err| err.to_string())?;
        std::fs::write(&path, text).map_err(|err| err.to_string())?;
        Ok(path)
    }

    pub(super) fn get(&self, name: &str) -> Option<&Profile> {
        self.list.iter().find(|profile| profile.name == name)
    }

    pub(super) fn default_profile(&self) -> Option<&Profile> {
        self.get(&self.default)
    }

    /// Shells installed on this machine.
    pub(super) fn discover() -> Self {
        let profile = |name: &str, command: &str| Profile {
            name: name.to_owned(),
            command: command.to_owned(),
            ..Profile::default()
        };
        let program_files = program_files();
        let exists = |path: &str| Path::new(path).is_file();

        let mut list = Vec::new();
        let pwsh = format!(r"{program_files}\PowerShell\7\pwsh.exe");
        if exists(&pwsh) {
            list.push(profile("PowerShell", &format!("{} -NoLogo", quote(&pwsh))));
        }
        list.push(profile("Windows PowerShell", "powershell.exe -NoLogo"));
        list.push(profile("Command Prompt", "cmd.exe"));
        let git_bash = format!(r"{program_files}\Git\bin\bash.exe");
        if exists(&git_bash) {
            list.push(profile("Git Bash", &format!("{} --login -i", quote(&git_bash))));
        }
        let distributions = wsl_distributions();
        if distributions.is_empty() && search_path("wsl.exe").is_some() {
            list.push(profile("WSL", &wsl_command_line(None)));
        }
        for distribution in distributions {
            list.push(profile(&distribution.name, &wsl_command_line(Some(&distribution.name))));
        }
        Self { default: String::new(), list }
    }
}

impl Profile {
    /// Start a terminal with this profile's command, directory, environment, and options.
    ///
    /// Like Windows Terminal, a profile without a starting directory starts in the user's home
    /// directory rather than Alacritty's working directory, unless one was requested explicitly.
    pub(super) fn apply(&self, options: &mut WindowOptions) {
        if let Some(command) = split_program(&self.command) {
            options.terminal_options.set_command(command);
        }
        let directory = self.directory.trim();
        let working_directory = &mut options.terminal_options.working_directory;
        if !directory.is_empty() {
            *working_directory = Some(expand(directory).into());
        } else if working_directory.is_none() {
            *working_directory = dirs::home_dir();
        }
        options.terminal_options.env.extend(self.env.clone());
        options
            .add_options(self.options.iter().filter(|option| !option.trim().is_empty()).cloned());
    }
}

/// A name for a copy of the profile `name` that `taken` accepts, such as "PowerShell (copy)".
///
/// Copying a copy numbers it instead of stacking suffixes: "PowerShell (copy 2)".
pub(super) fn copy_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let name = name.trim();
    let base = name
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" (copy"))
        .filter(|(_, number)| number.is_empty() || number.trim_start().parse::<u32>().is_ok())
        .map_or(name, |(base, _)| base);
    let base = if base.is_empty() { "Untitled" } else { base };
    let mut candidate = format!("{base} (copy)");
    let mut number = 2;
    while taken(&candidate) {
        candidate = format!("{base} (copy {number})");
        number += 1;
    }
    candidate
}

fn program_files() -> String {
    std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into())
}

fn quote(path: &str) -> String {
    if path.contains(' ') { format!("\"{path}\"") } else { path.to_owned() }
}

pub(super) fn wsl_command_line(distribution: Option<&str>) -> String {
    match distribution {
        Some(distribution) => format!("wsl.exe -d {} --cd ~", quote(distribution)),
        None => "wsl.exe --cd ~".to_owned(),
    }
}

/// Split a command line into its program and the verbatim remainder.
///
/// Alacritty passes arguments to ConPTY unescaped on Windows, so keeping the user's remainder
/// as one argument preserves their quoting exactly.
pub(super) fn split_program(command: &str) -> Option<Vec<String>> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let end = match command.strip_prefix('"') {
        Some(rest) => rest.find('"').map_or(command.len(), |end| end + 2),
        None => command.find(char::is_whitespace).unwrap_or(command.len()),
    };
    let (program, rest) = command.split_at(end);
    let mut parts = vec![program.to_owned()];
    if !rest.trim().is_empty() {
        parts.push(rest.trim().to_owned());
    }
    Some(parts)
}

/// The program of a command line, without quotes.
fn program_of(command: &str) -> Option<String> {
    split_program(command).map(|parts| parts[0].trim_matches('"').to_owned())
}

/// Expand `~` and `%VARIABLES%` in a path typed by the user.
pub(super) fn expand(path: &str) -> String {
    let path = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['\\', '/']) => {
            format!("%USERPROFILE%{rest}")
        },
        _ => path.to_owned(),
    };
    let source = wide(&path);
    let mut buffer = vec![0u16; 1024];
    let len = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), buffer.as_mut_ptr(), 1024) };
    if len == 0 || len as usize > buffer.len() {
        return path;
    }
    String::from_utf16_lossy(&buffer[..len as usize - 1])
}

/// Resolve a program name through the executable search path.
fn search_path(program: &str) -> Option<PathBuf> {
    let program = expand(program);
    if Path::new(&program).is_absolute() {
        return Path::new(&program).is_file().then(|| program.into());
    }
    let name = wide(&program);
    let extension = wide(".exe");
    let mut buffer = vec![0u16; 1024];
    let len = unsafe {
        SearchPathW(
            std::ptr::null(),
            name.as_ptr(),
            extension.as_ptr(),
            buffer.len() as u32,
            buffer.as_mut_ptr(),
            std::ptr::null_mut(),
        )
    };
    (len > 0 && (len as usize) < buffer.len())
        .then(|| String::from_utf16_lossy(&buffer[..len as usize]).into())
}

/// A registered WSL distribution.
pub(super) struct Distribution {
    pub(super) name: String,
    base_path: String,
    package_family: String,
}

/// Registered WSL distributions, read from the per-user Lxss registry key.
pub(super) fn wsl_distributions() -> Vec<Distribution> {
    let lxss = wide(r"Software\Microsoft\Windows\CurrentVersion\Lxss");
    let mut distributions = Vec::new();
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, lxss.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return distributions;
        }
        for index in 0.. {
            let mut name = [0u16; 128];
            let mut name_len = name.len() as u32;
            let status = RegEnumKeyExW(
                key,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if status != 0 {
                break;
            }
            let value = |value: &str| {
                let value = wide(value);
                let mut data = [0u16; 512];
                let mut size = std::mem::size_of_val(&data) as u32;
                let status = RegGetValueW(
                    key,
                    name.as_ptr(),
                    value.as_ptr(),
                    RRF_RT_REG_SZ,
                    std::ptr::null_mut(),
                    data.as_mut_ptr().cast(),
                    &mut size,
                );
                if status != 0 {
                    return String::new();
                }
                let len = data.iter().position(|&c| c == 0).unwrap_or(data.len());
                String::from_utf16_lossy(&data[..len])
            };
            let distribution = Distribution {
                name: value("DistributionName"),
                base_path: value("BasePath"),
                package_family: value("PackageFamilyName"),
            };
            // Docker's internal distributions are not interactive shells.
            if !distribution.name.is_empty() && !distribution.name.starts_with("docker-desktop") {
                distributions.push(distribution);
            }
        }
        RegCloseKey(key);
    }
    distributions
}

/// The install folder of a packaged (Microsoft Store) application.
fn package_path(family: &str) -> Option<PathBuf> {
    let family = wide(family);
    unsafe {
        let (mut count, mut length) = (0u32, 0u32);
        GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            std::ptr::null_mut(),
            &mut length,
            std::ptr::null_mut(),
        );
        if count == 0 || length == 0 {
            return None;
        }
        let mut names = vec![std::ptr::null_mut(); count as usize];
        let mut buffer = vec![0u16; length as usize];
        let status = GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            names.as_mut_ptr(),
            &mut length,
            buffer.as_mut_ptr(),
        );
        if status != 0 || names[0].is_null() {
            return None;
        }
        let mut path = vec![0u16; 1024];
        let mut path_len = path.len() as u32;
        if GetPackagePathByFullName(names[0], &mut path_len, path.as_mut_ptr()) != 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&path[..path_len.saturating_sub(1) as usize]).into())
    }
}

/// The file whose icon best represents a profile.
///
/// A custom icon wins. Otherwise WSL distributions use their own artwork, Git Bash uses Git's
/// icon, and every other program uses the icon embedded in its executable.
pub(super) fn icon_source(
    profile: Option<&Profile>,
    configured_shell: Option<&str>,
) -> Option<PathBuf> {
    if let Some(icon) = profile.map(|profile| profile.icon.trim()).filter(|icon| !icon.is_empty()) {
        return Some(expand(icon).into());
    }
    let command = profile.map(|profile| profile.command.as_str()).filter(|c| !c.trim().is_empty());
    let command = command.map(str::to_owned).or_else(|| configured_shell.map(quote))?;
    let program = program_of(&command)?;
    let lower = program.to_lowercase();

    if lower == "wsl" || lower.ends_with("wsl.exe") {
        let parts: Vec<&str> = command.split_whitespace().collect();
        let name = parts
            .windows(2)
            .find(|pair| matches!(pair[0], "-d" | "--distribution"))
            .map(|pair| pair[1].trim_matches('"'));
        if let Some(name) = name {
            if let Some(distribution) = wsl_distributions().into_iter().find(|d| d.name == name) {
                if let Some(path) = distribution_icon(&distribution) {
                    return Some(path);
                }
            }
        }
    }
    if lower.ends_with(r"\git\bin\bash.exe") || lower.ends_with(r"\git\usr\bin\bash.exe") {
        let root = Path::new(&program)
            .ancestors()
            .find(|path| path.file_name().is_some_and(|name| name.eq_ignore_ascii_case("git")));
        if let Some(icon) = root.map(|root| root.join("git-bash.exe")).filter(|path| path.is_file())
        {
            return Some(icon);
        }
    }
    search_path(&program)
}

fn distribution_icon(distribution: &Distribution) -> Option<PathBuf> {
    // `wsl --install` places the distribution's artwork beside its disk image.
    let base = distribution.base_path.trim_start_matches(r"\\?\");
    let shortcut = Path::new(base).join("shortcut.ico");
    if shortcut.is_file() {
        return Some(shortcut);
    }
    // Store distributions carry their own launcher, whose icon is the distribution logo.
    let package = package_path(&distribution.package_family)?;
    std::fs::read_dir(package)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("exe")))
}

/// Loaded icons, keyed by source file and pixel size, and destroyed with the cache.
#[derive(Default)]
pub(super) struct Icons {
    cache: RefCell<HashMap<(PathBuf, i32), HICON>>,
}

impl Icons {
    /// The icon in `path` at `size` pixels; failures are remembered as missing.
    pub(super) fn get(&self, path: &Path, size: i32) -> Option<HICON> {
        let key = (path.to_owned(), size);
        let icon = *self.cache.borrow_mut().entry(key).or_insert_with(|| load_icon(path, size));
        (!icon.is_null()).then_some(icon)
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        for icon in self.cache.get_mut().values() {
            if !icon.is_null() {
                unsafe { DestroyIcon(*icon) };
            }
        }
    }
}

fn load_icon(path: &Path, size: i32) -> HICON {
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default();
    let name = wide(&path.to_string_lossy());
    unsafe {
        match extension.to_ascii_lowercase().as_str() {
            "ico" => LoadImageW(
                std::ptr::null_mut(),
                name.as_ptr(),
                IMAGE_ICON,
                size,
                size,
                LR_LOADFROMFILE,
            ),
            "exe" | "dll" => {
                let mut icon = std::ptr::null_mut();
                let size = size as u32;
                let status =
                    SHDefExtractIconW(name.as_ptr(), 0, 0, &mut icon, std::ptr::null_mut(), size);
                if status < 0 { std::ptr::null_mut() } else { icon }
            },
            _ => load_image(&name, size),
        }
    }
}

/// Load a PNG, JPEG, BMP, or GIF through GDI+, scaled to fit a `size` square.
unsafe fn load_image(name: &[u16], size: i32) -> HICON {
    const PIXEL_FORMAT_32BPP_ARGB: i32 = 0x26200a;
    static STARTUP: OnceLock<bool> = OnceLock::new();
    let started = *STARTUP.get_or_init(|| unsafe {
        let mut token = 0;
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 0,
        };
        GdiplusStartup(&mut token, &input, std::ptr::null_mut()) == 0
    });
    if !started {
        return std::ptr::null_mut();
    }
    unsafe {
        let mut source: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromFile(name.as_ptr(), &mut source) != 0 || source.is_null() {
            return std::ptr::null_mut();
        }
        let image = source.cast::<GpImage>();
        let (mut width, mut height) = (0u32, 0u32);
        GdipGetImageWidth(image, &mut width);
        GdipGetImageHeight(image, &mut height);
        let mut icon = std::ptr::null_mut();
        let mut target: *mut GpBitmap = std::ptr::null_mut();
        let status = GdipCreateBitmapFromScan0(
            size,
            size,
            0,
            PIXEL_FORMAT_32BPP_ARGB,
            std::ptr::null(),
            &mut target,
        );
        if status == 0 && !target.is_null() && width > 0 && height > 0 {
            let mut graphics: *mut GpGraphics = std::ptr::null_mut();
            GdipGetImageGraphicsContext(target.cast(), &mut graphics);
            if !graphics.is_null() {
                GdipSetInterpolationMode(graphics, InterpolationModeHighQualityBicubic);
                GdipSetPixelOffsetMode(graphics, PixelOffsetModeHighQuality);
                // Fit inside the square, preserving the aspect ratio.
                let scale = f64::from(size) / f64::from(width.max(height));
                let (w, h) =
                    ((f64::from(width) * scale) as i32, (f64::from(height) * scale) as i32);
                GdipDrawImageRectI(graphics, image, (size - w) / 2, (size - h) / 2, w, h);
                GdipDeleteGraphics(graphics);
                GdipCreateHICONFromBitmap(target, &mut icon);
            }
        }
        if !target.is_null() {
            GdipDisposeImage(target.cast());
        }
        GdipDisposeImage(image);
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_keep_the_users_quoting() {
        assert_eq!(split_program("  "), None);
        assert_eq!(split_program("cmd.exe").unwrap(), ["cmd.exe"]);
        assert_eq!(
            split_program(r#""C:\Program Files\Git\bin\bash.exe" --login -i"#).unwrap(),
            [r#""C:\Program Files\Git\bin\bash.exe""#, "--login -i"]
        );
        assert_eq!(
            split_program(r#"wsl.exe -d "My Distro" --cd ~"#).unwrap(),
            ["wsl.exe", r#"-d "My Distro" --cd ~"#]
        );
        assert_eq!(program_of(r#""C:\a b\c.exe" x"#).unwrap(), r"C:\a b\c.exe");
    }

    #[test]
    fn paths_expand_home_and_variables() {
        let home = std::env::var("USERPROFILE").unwrap();
        assert_eq!(expand("~"), home);
        assert_eq!(expand(r"~\src"), format!(r"{home}\src"));
        assert_eq!(expand("%USERPROFILE%"), home);
        assert_eq!(expand("~user"), "~user");
    }

    #[test]
    fn profiles_round_trip_through_toml() {
        let mut profiles = Profiles::discover();
        profiles.default = "Command Prompt".into();
        let cmd = profiles.list.iter_mut().find(|p| p.name == "Command Prompt").unwrap();
        cmd.env.insert("FOO".into(), "bar".into());
        cmd.options.push("font.size=14".into());
        let text = toml::to_string_pretty(&profiles).unwrap();
        assert_eq!(toml::from_str::<Profiles>(&text).unwrap(), profiles);
        assert_eq!(profiles.default_profile().unwrap().command, "cmd.exe");
    }

    #[test]
    fn applying_a_profile_sets_launch_options() {
        let profile = Profile {
            name: "Test".into(),
            command: r#""C:\Program Files\x.exe" -a"#.into(),
            directory: "~".into(),
            env: [("FOO".to_owned(), "bar".to_owned())].into(),
            options: vec!["font.size=14".into(), " ".into()],
            ..Profile::default()
        };
        let mut options = WindowOptions::default();
        profile.apply(&mut options);
        assert_eq!(
            options.terminal_options.command().unwrap().program(),
            r#""C:\Program Files\x.exe""#
        );
        assert_eq!(
            options.terminal_options.working_directory.as_deref(),
            Some(Path::new(&std::env::var("USERPROFILE").unwrap()))
        );
        assert_eq!(options.terminal_options.env["FOO"], "bar");
        let mut config = crate::config::UiConfig::default();
        options.config_overrides().override_config(&mut config);
        assert_eq!(config.font.size().as_pt(), 14.);
    }

    #[test]
    fn profiles_without_a_directory_start_at_home() {
        let home = std::env::var("USERPROFILE").unwrap();
        let profile = Profile { name: "Test".into(), directory: "  ".into(), ..Profile::default() };
        let mut options = WindowOptions::default();
        profile.apply(&mut options);
        assert_eq!(options.terminal_options.working_directory.as_deref(), Some(Path::new(&home)));

        // An explicitly requested directory is kept.
        let mut options = WindowOptions::default();
        options.terminal_options.working_directory = Some(r"C:\Windows".into());
        profile.apply(&mut options);
        assert_eq!(
            options.terminal_options.working_directory.as_deref(),
            Some(Path::new(r"C:\Windows"))
        );
    }

    #[test]
    fn copies_get_unique_names() {
        let names = ["PowerShell", "PowerShell (copy)", "PowerShell (copy 2)", "cmd (copy)"];
        let taken = |name: &str| names.contains(&name);
        assert_eq!(copy_name("Bash", taken), "Bash (copy)");
        assert_eq!(copy_name("PowerShell", taken), "PowerShell (copy 3)");
        assert_eq!(copy_name("PowerShell (copy 2)", taken), "PowerShell (copy 3)");
        assert_eq!(copy_name("cmd (copy)", taken), "cmd (copy 2)");
        assert_eq!(copy_name("Odd (copyright)", taken), "Odd (copyright) (copy)");
        assert_eq!(copy_name("", taken), "Untitled (copy)");
    }

    #[test]
    fn programs_resolve_to_their_own_icons() {
        let cmd = Profile { command: "cmd.exe".into(), ..Profile::default() };
        let source = icon_source(Some(&cmd), None).unwrap();
        assert!(source.to_string_lossy().to_lowercase().ends_with(r"system32\cmd.exe"));
        let custom = Profile { icon: r"~\logo.png".into(), ..cmd.clone() };
        assert!(icon_source(Some(&custom), None).unwrap().ends_with("logo.png"));
        let icons = Icons::default();
        assert!(icons.get(&source, 32).is_some());
        assert!(icons.get(Path::new(r"C:\missing\nothing.png"), 32).is_none());
    }
}

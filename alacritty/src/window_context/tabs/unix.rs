//! Linux and BSD integration: caption, installed shells, program icons and file pickers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::{env, fs};

use winit::window::WindowAttributes;

use super::paint::Image;
use super::profiles::Profile;

/// Draw the caption in the tab bar.
pub fn attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes.with_decorations(false)
}

pub fn hostname() -> String {
    let mut name = [0u8; 256];
    let len = unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) };
    let name = if len == 0 { &name[..name.iter().position(|&c| c == 0).unwrap_or(0)] } else { &[] };
    String::from_utf8_lossy(name).split('.').next().unwrap_or_default().into()
}

/// Shells installed on this system.
pub fn shells() -> Vec<Profile> {
    let shells = fs::read_to_string("/etc/shells").unwrap_or_default();
    let mut list: Vec<Profile> = Vec::new();
    for path in shells.lines().map(str::trim).filter(|line| line.starts_with('/')) {
        let name = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path);
        let unusable = ["false", "true", "nologin", "passwd", "git-shell"].contains(&name);
        if !unusable && Path::new(path).is_file() && list.iter().all(|profile| profile.name != name)
        {
            list.push(Profile::new(name, path));
        }
    }
    list
}

/// Data directories of the XDG base directory specification.
fn data_dirs() -> Vec<PathBuf> {
    let home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home::home_dir().map(|home| home.join(".local/share")));
    let dirs = env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    home.into_iter().chain(dirs.split(':').map(PathBuf::from)).collect()
}

/// Icon of a program: from its desktop entry, or else named after it.
pub fn program_icon(command: &str) -> Option<Arc<Image>> {
    let program = command.split_whitespace().next()?.trim_matches(['"', '\'']);
    let name = Path::new(program).file_name()?.to_str()?;
    let entry = data_dirs().iter().find_map(|dir| {
        let entries = fs::read_dir(dir.join("applications")).ok()?.flatten();
        entries.filter_map(|entry| fs::read_to_string(entry.path()).ok()).find_map(|entry| {
            let value = |key: &str| entry.lines().find_map(|line| line.strip_prefix(key));
            let runs = |key: &str| {
                let program = value(key)?.split_whitespace().find(|word| !word.contains('='))?;
                Some(Path::new(program).file_name()? == name)
            };
            let matches = runs("Exec=").unwrap_or(false) || runs("TryExec=").unwrap_or(false);
            matches.then(|| value("Icon=").map(str::to_owned)).flatten()
        })
    });
    // Versioned programs, like `python3`, share their icon.
    let unversioned = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    entry.into_iter().chain([name.into(), unversioned.into()]).find_map(|icon| theme_icon(&icon))
}

pub fn terminal_icon() -> Option<Arc<Image>> {
    theme_icon("utilities-terminal")
}

/// An icon of the icon theme, or an icon file.
fn theme_icon(icon: &str) -> Option<Arc<Image>> {
    let path = match Path::new(icon).is_absolute() {
        true => PathBuf::from(icon),
        false => theme().get(icon)?.clone(),
    };
    super::icons::load(&path)
}

/// Application icons of the desktop's icon theme, the themes it inherits, and the fallback
/// theme, by name; each the largest SVG, or else the largest PNG, of the first theme with one.
fn theme() -> &'static HashMap<String, PathBuf> {
    static THEME: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
    THEME.get_or_init(|| {
        let dirs: Vec<_> = data_dirs().iter().map(|dir| dir.join("icons")).collect();
        let config = home::home_dir().unwrap_or_default().join(".config");
        let setting = |file: &str, key: &str| {
            let text = fs::read_to_string(config.join(file)).ok()?;
            text.lines().find_map(|line| Some(line.strip_prefix(key)?.trim().to_owned()))
        };
        let mut themes: Vec<String> =
            [("kdeglobals", "Theme="), ("gtk-3.0/settings.ini", "gtk-icon-theme-name=")]
                .iter()
                .filter_map(|(file, key)| setting(file, key))
                .collect();
        let mut index = 0;
        while let Some(theme) = themes.get(index).cloned() {
            let text = dirs
                .iter()
                .find_map(|dir| fs::read_to_string(dir.join(&theme).join("index.theme")).ok());
            let inherits =
                text.as_deref().and_then(|t| t.lines().find_map(|l| l.strip_prefix("Inherits=")));
            for parent in inherits.unwrap_or_default().split(',').chain(["hicolor"]).map(str::trim)
            {
                if !parent.is_empty() && !themes.iter().any(|theme| theme == parent) {
                    themes.push(parent.into());
                }
            }
            index += 1;
        }
        themes.push("hicolor".into());

        let mut icons: HashMap<String, (usize, u32, PathBuf)> = HashMap::new();
        let roots = themes
            .iter()
            .enumerate()
            .flat_map(|(rank, theme)| dirs.iter().map(move |dir| (rank, dir.join(theme))));
        let pixmaps = data_dirs().into_iter().map(|dir| (themes.len(), dir.join("pixmaps")));
        for (rank, root) in roots.chain(pixmaps) {
            // Themes keep application icons in `<size>/apps` or `apps/<size>`.
            let mut directories = vec![(root, 0)];
            while let Some((dir, depth)) = directories.pop() {
                for path in
                    fs::read_dir(&dir).into_iter().flatten().flatten().map(|entry| entry.path())
                {
                    let Some(file) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    let Some((name, extension)) = file.rsplit_once('.') else {
                        if depth < 2 && path.is_dir() {
                            directories.push((path, depth + 1));
                        }
                        continue;
                    };
                    let apps =
                        rank == themes.len() || path.components().any(|c| c.as_os_str() == "apps");
                    if !apps || !matches!(extension, "svg" | "png") {
                        continue;
                    }
                    // Larger sizes are more detailed; scalable is the largest.
                    let size =
                        path.parent().into_iter().flat_map(|p| p.ancestors()).find_map(|dir| {
                            let dir = dir.file_name()?.to_str()?;
                            let (size, scale) = dir.split_once('@').unwrap_or((dir, "1x"));
                            let size: u32 = if size == "scalable" {
                                1024
                            } else {
                                size.split('x').next()?.parse().ok()?
                            };
                            Some(size * scale.trim_end_matches('x').parse::<u32>().unwrap_or(1))
                        });
                    let score = size.unwrap_or(1) + if extension == "svg" { 1 << 20 } else { 0 };
                    let better = icons
                        .get(name)
                        .is_none_or(|&(r, s, _)| rank < r || (rank == r && score > s));
                    if better {
                        icons.insert(name.into(), (rank, score, path));
                    }
                }
            }
        }
        icons.into_iter().map(|(name, (.., path))| (name, path)).collect()
    })
}

pub fn file_icon(_: &Path) -> Option<Arc<Image>> {
    None
}

/// Ask for a file, or a folder, with the desktop's dialog.
pub fn pick(folder: bool, title: &str) -> Option<String> {
    let zenity = ["--file-selection", "--title", title];
    let kdialog = [if folder { "--getexistingdirectory" } else { "--getopenfilename" }, "."];
    let dialogs: [(&str, &[&str]); 2] = [("zenity", &zenity), ("kdialog", &kdialog)];
    dialogs.into_iter().find_map(|(program, args)| {
        let mut command = Command::new(program);
        command.args(args);
        if folder && program == "zenity" {
            command.arg("--directory");
        }
        let output = command.output().ok()?;
        let path = String::from_utf8(output.stdout).ok()?;
        // Cancelling still means the dialog worked.
        Some(Some(path.trim_end_matches('\n').to_owned()).filter(|path| !path.is_empty()))
    })?
}

/// Font families installed, with whether each is monospaced, as fontconfig lists them.
#[cfg(not(target_os = "macos"))]
pub fn fonts() -> Vec<(String, bool)> {
    use std::ffi::CStr;
    use std::ptr;

    use fontconfig_sys::constants::{FC_DUAL, FC_FAMILY, FC_FAMILYLANG, FC_SPACING};
    use fontconfig_sys::*;

    let mut fonts = Vec::new();
    unsafe {
        let pattern = FcPatternCreate();
        let objects = FcObjectSetCreate();
        for object in [FC_FAMILY, FC_FAMILYLANG, FC_SPACING] {
            FcObjectSetAdd(objects, object.as_ptr());
        }
        let set = FcFontList(ptr::null_mut(), pattern, objects);
        FcObjectSetDestroy(objects);
        FcPatternDestroy(pattern);
        let Some(set) = set.as_mut() else { return fonts };

        let string = |font, object: &CStr, index| {
            let mut value = ptr::null_mut();
            (FcPatternGetString(font, object.as_ptr(), index, &mut value) == FcResultMatch)
                .then(|| CStr::from_ptr(value.cast()).to_string_lossy().into_owned())
        };
        for &font in std::slice::from_raw_parts(set.fonts, set.nfont.max(0) as usize) {
            // Families have a name per language; the English one is the one most people know.
            let names = (0..).map_while(|index| string(font, FC_FAMILY, index));
            let languages = (0..).map(|index| string(font, FC_FAMILYLANG, index));
            let mut names = names.zip(languages).peekable();
            let first = names.peek().map(|(name, _)| name.clone());
            let english = names.find(|(_, language)| language.as_deref() == Some("en"));
            let Some(name) = english.map(|(name, _)| name).or(first) else { continue };
            let mut spacing = 0;
            let mono = FcPatternGetInteger(font, FC_SPACING.as_ptr(), 0, &mut spacing)
                == FcResultMatch
                && spacing >= FC_DUAL;
            fonts.push((name, mono));
        }
        FcFontSetDestroy(set);
    }
    fonts
}

/// Whether fontconfig has the font family, in `style` if given, rather than a substitute.
#[cfg(not(target_os = "macos"))]
pub fn installed(family: &str, style: Option<&str>) -> bool {
    use std::ffi::CString;
    use std::ptr;

    use fontconfig_sys::constants::{FC_FAMILY, FC_STYLE};
    use fontconfig_sys::*;

    let Ok(family) = CString::new(family) else { return false };
    let style = style.map(CString::new);
    unsafe {
        let pattern = FcPatternCreate();
        FcPatternAddString(pattern, FC_FAMILY.as_ptr(), family.as_ptr().cast());
        if let Some(Ok(style)) = &style {
            FcPatternAddString(pattern, FC_STYLE.as_ptr(), style.as_ptr().cast());
        }
        let objects = FcObjectSetCreate();
        FcObjectSetAdd(objects, FC_FAMILY.as_ptr());
        let set = FcFontList(ptr::null_mut(), pattern, objects);
        FcObjectSetDestroy(objects);
        FcPatternDestroy(pattern);
        let Some(set) = set.as_mut() else { return false };
        let found = set.nfont > 0;
        FcFontSetDestroy(set);
        found
    }
}

/// FreeType reports advances itself.
#[cfg(not(target_os = "macos"))]
pub fn advances(
    _family: &str,
    _style: Option<&str>,
    _bold: bool,
    _size: crossfont::Size,
) -> Option<super::text::Advances> {
    None
}

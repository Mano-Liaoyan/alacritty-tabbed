//! Linux and BSD integration: caption, installed shells, program icons and file pickers.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
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

/// Icon of a program: from its desktop entry, or named after it, or a terminal.
pub fn program_icon(command: &str) -> Option<Arc<Image>> {
    let program = command.split_whitespace().next()?.trim_matches(['"', '\'']);
    let name = Path::new(program).file_name()?.to_str()?;
    let dirs = data_dirs();
    let entry = dirs.iter().find_map(|dir| {
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
    let names = entry.into_iter().chain([name.to_owned(), "utilities-terminal".into()]);
    names.filter_map(|icon| theme_icon(&dirs, &icon)).find_map(|path| file_icon(&path))
}

/// A PNG of an icon theme; icons only available as SVG are not supported.
fn theme_icon(dirs: &[PathBuf], icon: &str) -> Option<PathBuf> {
    if Path::new(icon).is_absolute() {
        return Some(icon.into());
    }
    let file = &format!("{icon}.png");
    let sizes = ["256x256", "128x128", "96x96", "64x64", "48x48", "32x32"];
    dirs.iter().find_map(|dir| {
        let themes = fs::read_dir(dir.join("icons")).ok()?.flatten().map(|theme| theme.path());
        // Prefer the fallback theme every icon theme inherits.
        let mut themes: Vec<_> = themes.collect();
        themes.sort_by_key(|theme| !theme.ends_with("hicolor"));
        let candidates = themes.iter().flat_map(|theme| {
            sizes.iter().flat_map(move |size| {
                ["apps", "legacy"].map(|context| theme.join(size).join(context).join(file))
            })
        });
        candidates.chain([dir.join("pixmaps").join(file)]).find(|path| path.is_file())
    })
}

pub fn file_icon(path: &Path) -> Option<Arc<Image>> {
    super::icons::png(&fs::read(path).ok()?)
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

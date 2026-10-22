//! macOS integration: caption, installed shells, application icons and file pickers.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::{env, fs};

use winit::platform::macos::WindowAttributesExtMacOS;
use winit::window::WindowAttributes;

use super::paint::Image;

pub use super::unix::{hostname, shells};

/// Draw the caption in the tab bar, beside the system's window buttons.
pub fn attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes
        .with_title_hidden(true)
        .with_titlebar_transparent(true)
        .with_fullsize_content_view(true)
}

/// Icon of the application bundle a program belongs to, or else the Terminal's.
pub fn program_icon(command: &str) -> Option<Arc<Image>> {
    let program = command.split_whitespace().next()?.trim_matches(['"', '\'']);
    let path = match Path::new(program) {
        path if path.is_absolute() => path.to_owned(),
        path => env::split_paths(&env::var_os("PATH")?)
            .map(|dir| dir.join(path))
            .find(|p| p.is_file())?,
    };
    let bundle = path.ancestors().find(|path| path.extension().is_some_and(|ext| ext == "app"));
    let terminal = Path::new("/System/Applications/Utilities/Terminal.app");
    file_icon(bundle.unwrap_or(terminal))
}

/// Images from PNG files, icon files, and application bundles.
pub fn file_icon(path: &Path) -> Option<Arc<Image>> {
    let icns = match path.extension().and_then(|ext| ext.to_str()) {
        Some("png") => return super::icons::png(&fs::read(path).ok()?),
        Some("app") => bundle_icon(path)?,
        _ => path.to_owned(),
    };

    // `sips` converts the system's icon formats.
    let png = env::temp_dir().join(format!("alacritty-icon-{}.png", std::process::id()));
    let args = ["-s", "format", "png", "-Z", "64"];
    let status = Command::new("sips").args(args).arg(&icns).arg("--out").arg(&png).output();
    let image = status.ok().and_then(|_| super::icons::png(&fs::read(&png).ok()?));
    let _ = fs::remove_file(&png);
    image
}

/// The icon file named in a bundle's `Info.plist`.
fn bundle_icon(bundle: &Path) -> Option<PathBuf> {
    let info = fs::read_to_string(bundle.join("Contents/Info.plist")).ok()?;
    let name = info.split("<key>CFBundleIconFile</key>").nth(1)?;
    let name = name.split("<string>").nth(1)?.split("</string>").next()?.trim();
    let icon = bundle.join("Contents/Resources").join(name);
    Some(if icon.extension().is_some() { icon } else { icon.with_extension("icns") })
}

/// Ask for a file, or a folder, with the system's dialog.
pub fn pick(folder: bool, title: &str) -> Option<String> {
    let kind = if folder { "folder" } else { "file" };
    let title = title.replace('"', "'");
    let script = format!("POSIX path of (choose {kind} with prompt \"{title}\")");
    let output = Command::new("osascript").args(["-e", &script]).output().ok()?;
    let path = String::from_utf8(output.stdout).ok()?;
    Some(path.trim_end_matches('\n').to_owned()).filter(|path| !path.is_empty())
}

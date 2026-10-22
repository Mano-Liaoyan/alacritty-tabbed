//! Icons of tabs and profiles, loaded once per process.
//!
//! An icon is an image file the profile names, or else the logo of the program it runs: found by
//! the platform, else one of the logos bundled for common shells, else a terminal.

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use png::{ColorType, Decoder, Transformations};

use super::paint::Image;
use super::platform;

/// Logos for programs the system has no icon for, by program name; see
/// `alacritty/res/icons/LICENSES.md`.
const BUNDLED: [(&[&str], &[u8]); 3] = [
    (&["bash"], include_bytes!("../../../res/icons/bash.svg")),
    (&["pwsh", "powershell"], include_bytes!("../../../res/icons/powershell.svg")),
    (&["nu"], include_bytes!("../../../res/icons/nushell.png")),
];
const TERMINAL: &[u8] = include_bytes!("../../../res/icons/terminal.svg");

/// Alacritty's logo, shown in the caption.
pub fn logo() -> Arc<Image> {
    static LOGO: OnceLock<Arc<Image>> = OnceLock::new();
    let logo = include_bytes!("../../../extra/logo/compat/alacritty-term.svg");
    LOGO.get_or_init(|| Image::svg(logo).expect("invalid embedded logo")).clone()
}

/// The icon of an image file, or of the program a command line runs.
pub fn get(file: Option<&Path>, command: &str) -> Option<Arc<Image>> {
    type Icons = HashMap<(Option<String>, String), Option<Arc<Image>>>;
    static ICONS: OnceLock<Mutex<Icons>> = OnceLock::new();
    let key = (file.map(|file| file.display().to_string()), command.to_owned());
    let mut icons = ICONS.get_or_init(Default::default).lock().unwrap();
    let icon = icons.entry(key).or_insert_with(|| match file {
        Some(file) => load(file),
        None => program(command),
    });
    icon.clone()
}

/// An image file: SVG and PNG everywhere, and the platform's own formats.
pub fn load(path: &Path) -> Option<Arc<Image>> {
    match path.extension().and_then(|ext| ext.to_str()).map(str::to_lowercase).as_deref() {
        Some("svg") => Image::svg(&fs::read(path).ok()?),
        Some("png") => png(&fs::read(path).ok()?),
        _ => platform::file_icon(path),
    }
}

/// The logo of the program a command line runs.
fn program(command: &str) -> Option<Arc<Image>> {
    let program = command.trim_start().trim_start_matches(['"', '\'']);
    let program = program.split(['"', '\'']).next()?.split_whitespace().next()?;
    let name = Path::new(program).file_stem()?.to_str()?.to_lowercase();
    let bundled = BUNDLED.iter().find(|(names, _)| names.contains(&name.as_str()));
    platform::program_icon(command)
        .or_else(|| bundled.and_then(|(_, data)| image(data)))
        .or_else(platform::terminal_icon)
        .or_else(|| image(TERMINAL))
}

fn image(data: &[u8]) -> Option<Arc<Image>> {
    if data.starts_with(b"\x89PNG") { png(data) } else { Image::svg(data) }
}

pub fn png(bytes: &[u8]) -> Option<Arc<Image>> {
    let mut decoder = Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let pixels = &buffer[..info.buffer_size()];
    let rgba = match info.color_type {
        ColorType::Rgba => pixels.to_vec(),
        ColorType::Rgb => pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        ColorType::GrayscaleAlpha => {
            pixels.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
        },
        ColorType::Grayscale => pixels.iter().flat_map(|&p| [p, p, p, 255]).collect(),
        ColorType::Indexed => return None,
    };
    Image::new(info.width, info.height, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_logos_load() {
        logo();
        for data in BUNDLED.iter().map(|(_, data)| *data).chain([TERMINAL]) {
            assert!(image(data).is_some());
        }
    }
}

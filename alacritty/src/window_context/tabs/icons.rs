//! Icons of tabs and profiles, loaded once per process.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use png::{ColorType, Decoder, Transformations};

use super::paint::Image;
use super::platform;

/// Alacritty's logo, shown in the caption.
pub fn logo() -> Arc<Image> {
    static LOGO: OnceLock<Arc<Image>> = OnceLock::new();
    let logo = include_bytes!("../../../extra/logo/compat/alacritty-term.png");
    LOGO.get_or_init(|| png(logo).expect("invalid embedded logo")).clone()
}

/// The icon of an image file, or of the program a command line runs.
pub fn get(file: Option<&Path>, command: &str) -> Option<Arc<Image>> {
    type Icons = HashMap<(Option<String>, String), Option<Arc<Image>>>;
    static ICONS: OnceLock<Mutex<Icons>> = OnceLock::new();
    let key = (file.map(|file| file.display().to_string()), command.to_owned());
    let mut icons = ICONS.get_or_init(Default::default).lock().unwrap();
    icons
        .entry(key)
        .or_insert_with(|| match file {
            Some(file) => platform::file_icon(file),
            None => platform::program_icon(command),
        })
        .clone()
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

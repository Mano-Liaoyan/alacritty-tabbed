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

/// Logos for programs the system has no icon for: the programs, whether the logo is a single
/// color to tint, and the image. Sources and licences are in `alacritty/res/icons/LICENSES.md`.
const BUNDLED: &[(&[&str], bool, &[u8])] = &[
    // Shells.
    (&["bash"], false, include_bytes!("../../../res/icons/bash.svg")),
    (&["zsh"], false, include_bytes!("../../../res/icons/zsh.svg")),
    (&["fish"], true, include_bytes!("../../../res/icons/fish.svg")),
    (&["nu"], true, include_bytes!("../../../res/icons/nushell.svg")),
    (&["pwsh", "powershell"], false, include_bytes!("../../../res/icons/powershell.svg")),
    (&["cmd"], false, include_bytes!("../../../res/icons/windows.svg")),
    (&["elvish"], false, include_bytes!("../../../res/icons/elvish.svg")),
    (&["xonsh"], false, include_bytes!("../../../res/icons/xonsh.svg")),
    // Multiplexers, remote shells and containers.
    (&["tmux"], true, include_bytes!("../../../res/icons/tmux.svg")),
    (&["zellij"], false, include_bytes!("../../../res/icons/zellij.svg")),
    (&["ssh", "mosh"], true, include_bytes!("../../../res/icons/ssh.svg")),
    (&["docker"], false, include_bytes!("../../../res/icons/docker.svg")),
    (&["podman"], false, include_bytes!("../../../res/icons/podman.svg")),
    (&["kubectl", "k9s"], false, include_bytes!("../../../res/icons/kubernetes.svg")),
    // Languages and their interpreters.
    (&["python", "py"], false, include_bytes!("../../../res/icons/python.svg")),
    (&["ipython"], false, include_bytes!("../../../res/icons/ipython.png")),
    (&["node"], false, include_bytes!("../../../res/icons/nodejs.svg")),
    (&["deno"], true, include_bytes!("../../../res/icons/deno.svg")),
    (&["bun"], false, include_bytes!("../../../res/icons/bun.svg")),
    (&["ruby", "irb"], false, include_bytes!("../../../res/icons/ruby.svg")),
    (&["lua", "luajit"], false, include_bytes!("../../../res/icons/lua.svg")),
    (&["julia"], false, include_bytes!("../../../res/icons/julia.svg")),
    (&["r"], false, include_bytes!("../../../res/icons/r.svg")),
    (&["ghci", "ghc", "stack", "cabal"], false, include_bytes!("../../../res/icons/haskell.svg")),
    (&["psql"], false, include_bytes!("../../../res/icons/postgresql.svg")),
    (&["sqlite"], false, include_bytes!("../../../res/icons/sqlite.svg")),
    // Editors and tools.
    (&["nvim"], false, include_bytes!("../../../res/icons/neovim.svg")),
    (&["vim", "vi"], false, include_bytes!("../../../res/icons/vim.svg")),
    (&["hx", "helix"], true, include_bytes!("../../../res/icons/helix.svg")),
    (&["htop"], true, include_bytes!("../../../res/icons/htop.svg")),
    (&["btop"], false, include_bytes!("../../../res/icons/btop.svg")),
    (&["git", "git-bash"], false, include_bytes!("../../../res/icons/git.svg")),
    // WSL distributions, by name.
    (&["ubuntu"], false, include_bytes!("../../../res/icons/ubuntu.svg")),
    (&["debian"], false, include_bytes!("../../../res/icons/debian.svg")),
    (&["kali"], false, include_bytes!("../../../res/icons/kali.svg")),
    (&["opensuse", "suse"], false, include_bytes!("../../../res/icons/opensuse.svg")),
    (&["fedora"], false, include_bytes!("../../../res/icons/fedora.svg")),
    (&["arch"], false, include_bytes!("../../../res/icons/arch.svg")),
    (&["alpine"], true, include_bytes!("../../../res/icons/alpine.svg")),
    (&["alma"], false, include_bytes!("../../../res/icons/alma.svg")),
];
const TERMINAL: &[u8] = include_bytes!("../../../res/icons/terminal.svg");

/// Alacritty's logo, shown in the caption.
pub fn logo() -> Arc<Image> {
    static LOGO: OnceLock<Arc<Image>> = OnceLock::new();
    let logo = include_bytes!("../../../extra/logo/compat/alacritty-term.svg");
    LOGO.get_or_init(|| Image::svg(logo, false).expect("invalid embedded logo")).clone()
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
        Some("svg") => Image::svg(&fs::read(path).ok()?, false),
        Some("png") => png(&fs::read(path).ok()?),
        _ => platform::file_icon(path),
    }
}

/// The logo of the program a command line runs.
fn program(command: &str) -> Option<Arc<Image>> {
    platform::program_icon(command)
        .or_else(|| bundled(&bundled_name(command)?))
        .or_else(platform::terminal_icon)
        .or_else(|| image(TERMINAL, false))
}

/// Name of the bundled logo for a command line: the program, without a version or extension,
/// or the distribution a WSL command line starts.
fn bundled_name(command: &str) -> Option<String> {
    let words: Vec<_> =
        command.split_whitespace().map(|word| word.trim_matches(['"', '\''])).collect();
    let program = Path::new(words.first()?).file_stem()?.to_str()?.to_lowercase();
    let program = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    let name = match program {
        "wsl" => {
            words.windows(2).find(|w| matches!(w[0], "-d" | "--distribution"))?[1].to_lowercase()
        },
        program => program.into(),
    };
    Some(name)
}

fn bundled(name: &str) -> Option<Arc<Image>> {
    // Distributions are named like `Ubuntu-24.04`.
    let (_, tint, data) = BUNDLED.iter().find(|(names, ..)| {
        names.iter().any(|known| name == *known || name.starts_with(&format!("{known}-")))
    })?;
    image(data, *tint)
}

fn image(data: &[u8], tint: bool) -> Option<Arc<Image>> {
    if data.starts_with(b"\x89PNG") { png(data) } else { Image::svg(data, tint) }
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
        for (names, tint, data) in BUNDLED.iter().chain([&(&[][..], false, TERMINAL)]) {
            assert!(image(data, *tint).is_some(), "{names:?}");
        }
    }

    #[test]
    fn commands_find_their_bundled_logo() {
        assert_eq!(bundled_name("/usr/bin/python3.12 -q").as_deref(), Some("python"));
        assert_eq!(
            bundled_name("wsl.exe -d \"Ubuntu-24.04\" --cd ~").as_deref(),
            Some("ubuntu-24.04")
        );
        assert!(bundled("ubuntu-24.04").is_some());
        assert!(bundled("pwsh").is_some());
        assert!(bundled("unknown").is_none());
    }
}

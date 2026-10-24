//! Icons of tabs and profiles, loaded once per process.
//!
//! An icon is an image file the profile names, or else the logo of the program it runs: found by
//! the platform, else one of the logos bundled for common shells, else a terminal.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use alacritty_tabbed_deps::png;
use log::warn;
use png::{ColorType, Decoder, Transformations};

use super::paint::Image;
use super::{platform, profiles};

/// A bundled logo: its name, its file's extension, the programs it is for, and whether it is a
/// single color to tint.
macro_rules! logo {
    ($name:literal, $ext:ident, [$($program:literal),*], $tint:literal) => {
        Logo {
            name: $name,
            programs: &[$($program),*],
            tint: $tint,
            data: include_bytes!(concat!("../../../res/icons/", $name, ".", stringify!($ext))),
        }
    };
}

struct Logo {
    name: &'static str,
    programs: &'static [&'static str],
    tint: bool,
    data: &'static [u8],
}

/// Prefix of a profile's icon that names a bundled logo, as in `bundled:python`.
pub const BUNDLED_PREFIX: &str = "bundled:";

/// Logos for programs the system has no icon for, which profiles can also choose by name.
/// Sources and licences are in `alacritty/res/icons/LICENSES.md`.
const BUNDLED: &[Logo] = &[
    // Shells.
    logo!("bash", svg, ["bash"], false),
    logo!("zsh", svg, ["zsh"], false),
    logo!("fish", svg, ["fish"], true),
    logo!("nushell", svg, ["nu"], true),
    logo!("powershell", svg, ["pwsh", "powershell"], false),
    logo!("windows", svg, ["cmd"], false),
    logo!("elvish", svg, ["elvish"], false),
    logo!("xonsh", svg, ["xonsh"], false),
    // Multiplexers, remote shells and containers.
    logo!("tmux", svg, ["tmux"], true),
    logo!("zellij", svg, ["zellij"], false),
    logo!("ssh", svg, ["ssh", "mosh"], true),
    logo!("docker", svg, ["docker"], false),
    logo!("podman", svg, ["podman"], false),
    logo!("kubernetes", svg, ["kubectl", "k9s"], false),
    // Languages and their interpreters.
    logo!("python", svg, ["python", "py"], false),
    logo!("ipython", png, ["ipython"], false),
    logo!("nodejs", svg, ["node"], false),
    logo!("deno", svg, ["deno"], true),
    logo!("bun", svg, ["bun"], false),
    logo!("ruby", svg, ["ruby", "irb"], false),
    logo!("lua", svg, ["lua", "luajit"], false),
    logo!("julia", svg, ["julia"], false),
    logo!("r", svg, ["r"], false),
    logo!("haskell", svg, ["ghci", "ghc", "stack", "cabal"], false),
    logo!("postgresql", svg, ["psql"], false),
    logo!("sqlite", svg, ["sqlite"], false),
    // Editors and tools.
    logo!("neovim", svg, ["nvim"], false),
    logo!("vim", svg, ["vim", "vi"], false),
    logo!("helix", svg, ["hx", "helix"], true),
    logo!("htop", svg, ["htop"], true),
    logo!("btop", svg, ["btop"], false),
    logo!("git", svg, ["git", "git-bash"], false),
    // WSL distributions, by name.
    logo!("ubuntu", svg, ["ubuntu"], false),
    logo!("debian", svg, ["debian"], false),
    logo!("kali", svg, ["kali"], false),
    logo!("opensuse", svg, ["opensuse", "suse"], false),
    logo!("fedora", svg, ["fedora"], false),
    logo!("arch", svg, ["arch"], false),
    logo!("alpine", svg, ["alpine"], true),
    logo!("alma", svg, ["alma"], false),
];
const TERMINAL: Logo = logo!("terminal", svg, [], false);

/// Alacritty Tabbed's logo, shown in the caption and the settings.
pub fn logo() -> Arc<Image> {
    static LOGO: OnceLock<Arc<Image>> = OnceLock::new();
    let logo = include_bytes!("../../../extra/logo/tabbed/alacritty-tabbed.svg");
    LOGO.get_or_init(|| Image::svg(logo, false).expect("invalid embedded logo")).clone()
}

/// Upstream Alacritty's logo, which the tab bar can show instead.
pub fn alacritty_logo() -> Arc<Image> {
    static LOGO: OnceLock<Arc<Image>> = OnceLock::new();
    let logo = include_bytes!("../../../extra/logo/compat/alacritty-term.svg");
    LOGO.get_or_init(|| Image::svg(logo, false).expect("invalid embedded logo")).clone()
}

/// The icon at the left of the tab bar, as the configuration names it: Alacritty Tabbed's logo
/// with `tabbed`, Alacritty's with `alacritty`, a bundled logo with `bundled:<name>`, else an
/// image file, `~` expanded; none when empty.
///
/// An image file is read again until it loads, so a file fixed after the fact shows without a
/// restart; each value that shows nothing is reported once.
pub fn brand(icon: &str) -> Option<Arc<Image>> {
    static FILES: OnceLock<Mutex<HashMap<String, Arc<Image>>>> = OnceLock::new();
    static MISSING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let image = match icon.trim() {
        "" => return None,
        "tabbed" => Some(logo()),
        "alacritty" => Some(alacritty_logo()),
        icon => match icon.strip_prefix(BUNDLED_PREFIX) {
            Some(name) => named(name),
            None => {
                let mut files = FILES.get_or_init(Default::default).lock().unwrap();
                match files.get(icon) {
                    Some(image) => Some(image.clone()),
                    None => load(Path::new(&profiles::expand(icon))).inspect(|image| {
                        files.insert(icon.into(), image.clone());
                    }),
                }
            },
        },
    };
    if image.is_none() && MISSING.get_or_init(Default::default).lock().unwrap().insert(icon.into())
    {
        warn!("Could not load the tab bar's logo {icon:?}");
    }
    image
}

/// A pushpin turned `angle` degrees clockwise from upright, its head filled `fill` of the way,
/// in one color to tint: upright and solid while the tab bar is pinned, laid over as an outline
/// while it is not, and every step between them as it changes.
pub fn pin(angle: f32, fill: f32) -> Arc<Image> {
    type Pins = HashMap<(i32, u8), Arc<Image>>;
    static PINS: OnceLock<Mutex<Pins>> = OnceLock::new();
    // Whole degrees and eighths of fill are finer than the eye follows at this size.
    let key = (angle.round() as i32, (fill.clamp(0., 1.) * 8.).round() as u8);
    let mut pins = PINS.get_or_init(Default::default).lock().unwrap();
    let pin = pins.entry(key).or_insert_with(|| {
        let (angle, fill) = (key.0, f32::from(key.1) / 8.);
        // A head and a collar joined by a waist, over a needle, in a 24 unit square: as wide and
        // as heavy as the gear beside it.
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
<g transform="rotate({angle} 12 12)" fill="none" stroke="#000" stroke-width="2"
   stroke-linejoin="round" stroke-linecap="round">
<path fill="#000" fill-opacity="{fill}" d="M7.25 2.5H16.75A1.25 1.25 0 0 1 16.75 5H15V9.75L19 13.5V15.25H5V13.5L9 9.75V5H7.25A1.25 1.25 0 0 1 7.25 2.5Z"/>
<path d="M12 15.75V21.75"/>
</g></svg>"##
        );
        Image::svg(svg.as_bytes(), true).expect("invalid pushpin")
    });
    pin.clone()
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
        .or_else(|| image(TERMINAL.data, TERMINAL.tint))
}

/// Names of the bundled logos, as profiles choose them.
pub fn names() -> impl Iterator<Item = &'static str> {
    BUNDLED.iter().chain([&TERMINAL]).map(|logo| logo.name)
}

/// The bundled logo named `name`.
pub fn named(name: &str) -> Option<Arc<Image>> {
    static LOGOS: OnceLock<Mutex<HashMap<String, Option<Arc<Image>>>>> = OnceLock::new();
    let mut logos = LOGOS.get_or_init(Default::default).lock().unwrap();
    let logo = logos.entry(name.into()).or_insert_with(|| {
        let logo = BUNDLED.iter().chain([&TERMINAL]).find(|logo| logo.name == name)?;
        image(logo.data, logo.tint)
    });
    logo.clone()
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
    let logo = BUNDLED.iter().find(|logo| {
        logo.programs.iter().any(|known| name == *known || name.starts_with(&format!("{known}-")))
    })?;
    image(logo.data, logo.tint)
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
        alacritty_logo();
        for name in names() {
            assert!(named(name).is_some(), "{name}");
        }
        assert!(named("unknown").is_none());
    }

    #[test]
    fn brand_icons_follow_the_configuration() {
        assert!(Arc::ptr_eq(&brand("tabbed").unwrap(), &logo()));
        assert!(Arc::ptr_eq(&brand("alacritty").unwrap(), &alacritty_logo()));
        assert!(brand("").is_none());
        assert!(brand("bundled:opensuse").is_some());
        assert!(brand(" bundled:opensuse ").is_some());
        assert!(brand("bundled:unknown").is_none());
        assert!(brand("/nonexistent/logo.svg").is_none());
    }

    #[test]
    fn pins_are_cached_by_step() {
        let pinned = pin(0., 1.);
        assert!(Arc::ptr_eq(&pinned, &pin(0.2, 0.99)));
        assert!(!Arc::ptr_eq(&pinned, &pin(45., 0.)));
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

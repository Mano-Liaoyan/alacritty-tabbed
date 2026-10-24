//! macOS integration: caption, installed shells, application icons and file pickers.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::{env, fs};

use alacritty_tabbed_deps::core_text;
use alacritty_tabbed_deps::objc2_app_kit::{NSView, NSWindowButton};
use core_text::font_descriptor::CTFontDescriptor;
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowAttributes};

use super::paint::Image;

pub use super::unix::{hostname, shells};

/// Draw the caption in the tab bar, beside the system's window buttons.
pub fn attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes
        .with_title_hidden(true)
        .with_titlebar_transparent(true)
        .with_fullsize_content_view(true)
}

/// Center the system's window buttons on a bar `bar` points tall, as far from the window's left
/// edge as from its top, or leave them to the system without one.
///
/// The bar moves the window itself, from its empty space only: the system's caption under it no
/// longer does, so a tab is dragged rather than the window. The system lays its buttons out again
/// as the window resizes, so this is repeated before each frame.
pub fn place_buttons(window: &Window, bar: Option<f64>) {
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    // Window events are handled on the main thread.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let Some(window) = view.window() else { return };
    window.setMovable(bar.is_none());
    let Some(bar) = bar else { return };

    let buttons = [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ];
    let buttons: Vec<_> =
        buttons.into_iter().filter_map(|b| window.standardWindowButton(b)).collect();
    let Some(container) = buttons.first().and_then(|b| unsafe { b.superview()?.superview() })
    else {
        return;
    };
    let first = buttons[0].frame();
    let step = buttons.get(1).map_or(20., |b| b.frame().origin.x - first.origin.x);

    // The container's height places the buttons, which keep their distance from its bottom.
    let left = (bar - first.size.height) / 2.;
    let height = left + first.origin.y + first.size.height;
    let mut frame = container.frame();
    if frame.size.height != height {
        frame.size.height = height;
        frame.origin.y = window.frame().size.height - height;
        container.setFrame(frame);
    }
    for (index, button) in buttons.iter().enumerate() {
        let mut origin = button.frame().origin;
        origin.x = left + index as f64 * step;
        button.setFrameOrigin(origin);
    }
}

/// Icon of the application bundle a program belongs to.
pub fn program_icon(command: &str) -> Option<Arc<Image>> {
    let program = command.split_whitespace().next()?.trim_matches(['"', '\'']);
    let path = match Path::new(program) {
        path if path.is_absolute() => path.to_owned(),
        path => env::split_paths(&env::var_os("PATH")?)
            .map(|dir| dir.join(path))
            .find(|p| p.is_file())?,
    };
    file_icon(path.ancestors().find(|path| path.extension().is_some_and(|ext| ext == "app"))?)
}

pub fn terminal_icon() -> Option<Arc<Image>> {
    file_icon(Path::new("/System/Applications/Utilities/Terminal.app"))
}

/// Icon files and application bundles, at their largest size.
pub fn file_icon(path: &Path) -> Option<Arc<Image>> {
    let icns = match path.extension().and_then(|ext| ext.to_str()) {
        Some("app") => bundle_icon(path)?,
        _ => path.to_owned(),
    };

    // `sips` converts the system's icon formats.
    let png = env::temp_dir().join(format!("alacritty-icon-{}.png", std::process::id()));
    let args = ["-s", "format", "png", "-Z", "256"];
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

/// Font families installed, with whether each is monospaced, as Core Text lists them.
pub fn fonts() -> Vec<(String, bool)> {
    use core_text::font_collection;
    use core_text::font_descriptor::{SymbolicTraitAccessors, TraitAccessors};

    let Some(descriptors) = font_collection::create_for_all_families().get_descriptors() else {
        return Vec::new();
    };
    let mono =
        |traits: core_text::font_descriptor::CTFontTraits| traits.symbolic_traits().is_monospace();
    descriptors.iter().map(|font| (font.family_name(), mono(font.traits()))).collect()
}

/// The face of a family in `style`, or else bold or regular, as Core Text describes it.
fn face(family: &str, style: Option<&str>, bold: bool) -> Option<CTFontDescriptor> {
    use core_text::font_collection;
    use core_text::font_descriptor::{SymbolicTraitAccessors, TraitAccessors};

    let descriptors = font_collection::create_for_family(family)?.get_descriptors()?;
    let face = descriptors.iter().find(|face| match style {
        Some(style) => face.style_name() == style,
        None => {
            let traits = face.traits().symbolic_traits();
            traits.is_bold() == bold && !traits.is_italic()
        },
    });
    face.map(|face| face.clone())
}

/// Whether Core Text has the font family, in `style` if given.
pub fn installed(family: &str, style: Option<&str>) -> bool {
    match style {
        Some(_) => face(family, style, false).is_some(),
        None => core_text::font_collection::create_for_family(family).is_some(),
    }
}

/// Advances of a face, which the rasterizer does not report on macOS.
pub fn advances(
    family: &str,
    style: Option<&str>,
    bold: bool,
    size: crossfont::Size,
) -> Option<super::text::Advances> {
    use core_text::font_descriptor::kCTFontDefaultOrientation;

    let font =
        core_text::font::new_from_descriptor(&face(family, style, bold)?, size.as_pt().into());
    Some(Box::new(move |character| {
        let mut units = [0; 2];
        let units = character.encode_utf16(&mut units);
        let mut glyphs = [0; 2];
        unsafe {
            let count = units.len() as isize;
            if !font.get_glyphs_for_characters(units.as_ptr(), glyphs.as_mut_ptr(), count) {
                return None;
            }
            let orientation = kCTFontDefaultOrientation;
            let advance =
                font.get_advances_for_glyphs(orientation, glyphs.as_ptr(), std::ptr::null_mut(), 1);
            Some(advance as f32)
        }
    }))
}

/// Radius of the window's corners in points, as the system rounds them.
///
/// AppKit only tells it privately, so where it does not, it is taken from the system's version:
/// macOS 26 rounds windows more than the ones before it.
pub fn corner_radius(window: &Window) -> f64 {
    use alacritty_tabbed_deps::objc2::runtime::NSObjectProtocol;
    use alacritty_tabbed_deps::objc2::{msg_send, sel};
    use alacritty_tabbed_deps::objc2_foundation::NSProcessInfo;

    let told = || {
        let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        // Window events are handled on the main thread.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        let window = view.window()?;
        let selector = sel!(_cornerRadius);
        if !window.respondsToSelector(selector) {
            return None;
        }
        let radius: f64 = unsafe { msg_send![&*window, _cornerRadius] };
        (radius.is_finite() && radius > 0.).then_some(radius)
    };
    told().unwrap_or_else(|| {
        let version = NSProcessInfo::processInfo().operatingSystemVersion();
        if version.majorVersion >= 26 { 16. } else { 10. }
    })
}

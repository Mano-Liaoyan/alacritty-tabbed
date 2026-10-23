//! Windows integration: caption, installed shells, program icons and file pickers.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{env, ptr};

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, ReleaseDC,
};
use windows_sys::Win32::Storage::Packaging::Appx::{
    GetPackagePathByFullName, GetPackagesByPackageFamily,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Shell::{
    BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW, SHDefExtractIconW,
    SHGetPathFromIDListW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, HICON, ICONINFO, IMAGE_ICON, LR_LOADFROMFILE, LoadImageW,
};
use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
use winit::window::WindowAttributes;

use super::paint::Image;
use super::profiles::Profile;

/// Draw the caption in the tab bar, keeping the system's shadow and rounded corners.
pub fn attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes
        .with_decorations(false)
        .with_undecorated_shadow(true)
        .with_corner_preference(CornerPreference::Round)
}

pub fn hostname() -> String {
    env::var("COMPUTERNAME").unwrap_or_default()
}

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain([0]).collect()
}

fn string(text: &[u16]) -> String {
    String::from_utf16_lossy(&text[..text.iter().position(|&c| c == 0).unwrap_or(text.len())])
}

/// Shells installed on this system.
pub fn shells() -> Vec<Profile> {
    let program_files = env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let mut list = Vec::new();
    let pwsh = format!(r"{program_files}\PowerShell\7\pwsh.exe");
    if Path::new(&pwsh).is_file() {
        list.push(Profile::new("PowerShell", &format!("\"{pwsh}\" -NoLogo")));
    }
    list.push(Profile::new("Windows PowerShell", "powershell.exe -NoLogo"));
    list.push(Profile::new("Command Prompt", "cmd.exe"));
    let git_bash = format!(r"{program_files}\Git\bin\bash.exe");
    if Path::new(&git_bash).is_file() {
        list.push(Profile::new("Git Bash", &format!("\"{git_bash}\" --login -i")));
    }
    for (name, ..) in distributions() {
        list.push(Profile::new(&name, &format!("wsl.exe -d \"{name}\" --cd ~")));
    }
    list
}

/// Registered WSL distributions: name, base path and Store package family.
fn distributions() -> Vec<(String, String, String)> {
    let mut distributions = Vec::new();
    let lxss = wide(r"Software\Microsoft\Windows\CurrentVersion\Lxss");
    unsafe {
        let mut key: HKEY = ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, lxss.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return distributions;
        }
        for index in 0.. {
            let mut name = [0u16; 128];
            let mut len = name.len() as u32;
            let (none, class, time) = (ptr::null(), ptr::null_mut(), ptr::null_mut());
            let null = ptr::null_mut();
            if RegEnumKeyExW(key, index, name.as_mut_ptr(), &mut len, none, class, null, time) != 0
            {
                break;
            }
            let value = |value: &str| {
                let mut data = [0u16; 512];
                let mut size = size_of_val(&data) as u32;
                let value = wide(value);
                let (data_ptr, flags) = (data.as_mut_ptr().cast(), RRF_RT_REG_SZ);
                let found = RegGetValueW(
                    key,
                    name.as_ptr(),
                    value.as_ptr(),
                    flags,
                    ptr::null_mut(),
                    data_ptr,
                    &mut size,
                );
                if found == 0 { string(&data) } else { String::new() }
            };
            let name = value("DistributionName");
            // Docker's own distributions are not shells.
            if !name.is_empty() && !name.starts_with("docker-desktop") {
                distributions.push((name, value("BasePath"), value("PackageFamilyName")));
            }
        }
        RegCloseKey(key);
    }
    distributions
}

/// Install folder of a Microsoft Store package.
fn package_path(family: &str) -> Option<PathBuf> {
    let family = wide(family);
    unsafe {
        let (mut count, mut length) = (0u32, 0u32);
        let (null, none) = (ptr::null_mut(), ptr::null_mut());
        GetPackagesByPackageFamily(family.as_ptr(), &mut count, null, &mut length, none);
        let mut names = vec![ptr::null_mut(); count as usize];
        let mut buffer = vec![0u16; length as usize];
        let (names_ptr, buffer_ptr) = (names.as_mut_ptr(), buffer.as_mut_ptr());
        let status = GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            names_ptr,
            &mut length,
            buffer_ptr,
        );
        let name = *names.first().filter(|name| status == 0 && !name.is_null())?;
        let mut path = vec![0u16; 1024];
        let mut len = path.len() as u32;
        (GetPackagePathByFullName(name, &mut len, path.as_mut_ptr()) == 0)
            .then(|| string(&path).into())
    }
}

/// Icon of the program `command` runs: WSL distributions show their artwork, Git Bash Git's.
pub fn program_icon(command: &str) -> Option<Arc<Image>> {
    let program = command.split_whitespace().next()?.trim_matches('"');
    let quoted = command.trim_start().strip_prefix('"');
    let program = quoted.and_then(|rest| rest.split('"').next()).unwrap_or(program);
    let lower = program.to_lowercase();

    if lower == "wsl" || lower.ends_with("wsl.exe") {
        let words: Vec<_> = command.split_whitespace().map(|word| word.trim_matches('"')).collect();
        let name = words.windows(2).find(|w| matches!(w[0], "-d" | "--distribution")).map(|w| w[1]);
        let distribution = distributions().into_iter().find(|(n, ..)| Some(n.as_str()) == name);
        if let Some((_, base, family)) = distribution {
            // `wsl --install` keeps the artwork beside the disk image; Store packages carry it
            // in their launcher.
            let shortcut = Path::new(base.trim_start_matches(r"\\?\")).join("shortcut.ico");
            let launcher = || {
                let files = std::fs::read_dir(package_path(&family)?).ok()?.flatten();
                files.map(|file| file.path()).find(|path| extension(path) == "exe")
            };
            let icon = shortcut.is_file().then_some(shortcut).or_else(launcher);
            if let Some(image) = icon.and_then(|icon| file_icon(&icon)) {
                return Some(image);
            }
        }
    }
    if lower.ends_with(r"\git\bin\bash.exe") || lower.ends_with(r"\git\usr\bin\bash.exe") {
        let root = Path::new(program)
            .ancestors()
            .find(|path| path.file_name().is_some_and(|name| name.eq_ignore_ascii_case("git")));
        if let Some(image) = root.and_then(|root| file_icon(&root.join("git-bash.exe"))) {
            return Some(image);
        }
    }
    file_icon(&search_path(program)?)
}

fn extension(path: &Path) -> String {
    path.extension().map(|ext| ext.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Resolve a program through `PATH`, adding `.exe` when it has no extension.
fn search_path(program: &str) -> Option<PathBuf> {
    let program = Path::new(program);
    let names = [program.to_owned(), program.with_extension("exe")];
    let names = &names[..if program.extension().is_some() { 1 } else { 2 }];
    if program.is_absolute() {
        return names.iter().find(|path| path.is_file()).cloned();
    }
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

pub fn terminal_icon() -> Option<Arc<Image>> {
    None
}

/// Icons, and the icons of programs and libraries, at their largest size.
pub fn file_icon(path: &Path) -> Option<Arc<Image>> {
    let name = wide(path);
    let icon = unsafe {
        match extension(path).as_str() {
            "ico" => {
                LoadImageW(ptr::null_mut(), name.as_ptr(), IMAGE_ICON, 256, 256, LR_LOADFROMFILE)
            },
            "exe" | "dll" => {
                let mut icon = ptr::null_mut();
                SHDefExtractIconW(name.as_ptr(), 0, 0, &mut icon, ptr::null_mut(), 256);
                icon
            },
            _ => return None,
        }
    };
    (!icon.is_null()).then(|| unsafe { icon_image(icon) }).flatten()
}

/// Copy the pixels of an icon, and destroy it.
unsafe fn icon_image(icon: HICON) -> Option<Arc<Image>> {
    unsafe {
        let mut info: ICONINFO = std::mem::zeroed();
        let ok = GetIconInfo(icon, &mut info) != 0;
        DestroyIcon(icon);
        let mut bitmap: BITMAP = std::mem::zeroed();
        let size = size_of::<BITMAP>() as i32;
        let ok = ok && GetObjectW(info.hbmColor, size, (&mut bitmap as *mut BITMAP).cast()) != 0;
        let (width, height) = (bitmap.bmWidth, bitmap.bmHeight);
        let mut pixels = vec![0u8; (width * height * 4).max(0) as usize];
        let mut header: BITMAPINFO = std::mem::zeroed();
        header.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Top-down rows.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let dc = GetDC(ptr::null_mut());
        let (lines, data) = (height as u32, pixels.as_mut_ptr().cast());
        let ok =
            ok && GetDIBits(dc, info.hbmColor, 0, lines, data, &mut header, DIB_RGB_COLORS) != 0;
        ReleaseDC(ptr::null_mut(), dc);
        DeleteObject(info.hbmColor);
        DeleteObject(info.hbmMask);
        if !ok {
            return None;
        }

        // BGRA to RGBA; icons without an alpha channel are opaque.
        let opaque = pixels.chunks_exact(4).all(|pixel| pixel[3] == 0);
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            if opaque {
                pixel[3] = 255;
            }
        }
        Image::new(width as u32, height as u32, pixels)
    }
}

/// Ask for a file, or a folder, with the system's dialog.
pub fn pick(folder: bool, title: &str) -> Option<String> {
    let title = wide(title);
    unsafe {
        if folder {
            let info = BROWSEINFOW {
                lpszTitle: title.as_ptr(),
                ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
                ..std::mem::zeroed()
            };
            let list = SHBrowseForFolderW(&info);
            if list.is_null() {
                return None;
            }
            let mut path = [0u16; 1024];
            let found = SHGetPathFromIDListW(list, path.as_mut_ptr()) != 0;
            CoTaskMemFree(list.cast());
            return found.then(|| string(&path));
        }

        let mut path = [0u16; 1024];
        let filter = wide("All files\0*.*\0");
        let mut dialog = OPENFILENAMEW {
            lStructSize: size_of::<OPENFILENAMEW>() as u32,
            lpstrFilter: filter.as_ptr(),
            lpstrFile: path.as_mut_ptr(),
            nMaxFile: path.len() as u32,
            lpstrTitle: title.as_ptr(),
            Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR,
            ..std::mem::zeroed()
        };
        (GetOpenFileNameW(&mut dialog) != 0).then(|| string(&path))
    }
}

/// Font families installed, with whether each is monospaced, as DirectWrite lists them.
pub fn fonts() -> Vec<(String, bool)> {
    use dwrote::{FontCollection, FontStretch, FontStyle, FontWeight};

    FontCollection::system()
        .families_iter()
        .filter_map(|family| {
            let font = family.first_matching_font(
                FontWeight::Regular,
                FontStretch::Normal,
                FontStyle::Normal,
            );
            let mono = font.ok().and_then(|font| font.is_monospace()).unwrap_or(false);
            Some((family.family_name().ok()?, mono))
        })
        .collect()
}

/// The face of a family in `style`, or else bold or regular, as DirectWrite knows it.
fn face(family: &str, style: Option<&str>, bold: bool) -> Option<dwrote::Font> {
    use dwrote::{FontCollection, FontStretch, FontStyle, FontWeight};

    let family = FontCollection::system().font_family_by_name(family).ok()??;
    match style {
        Some(style) => (0..family.get_font_count())
            .filter_map(|index| family.font(index).ok())
            .find(|font| font.face_name() == style),
        None => {
            let weight = if bold { FontWeight::Bold } else { FontWeight::Regular };
            family.first_matching_font(weight, FontStretch::Normal, FontStyle::Normal).ok()
        },
    }
}

/// Whether DirectWrite has the font family, in `style` if given.
pub fn installed(family: &str, style: Option<&str>) -> bool {
    face(family, style, false).is_some()
}

/// Advances of a face, which the rasterizer does not report on Windows.
pub fn advances(
    family: &str,
    style: Option<&str>,
    bold: bool,
    size: crossfont::Size,
) -> Option<super::text::Advances> {
    let face = face(family, style, bold)?.create_font_face();
    let scale = size.as_px() / f32::from(face.metrics().metrics0().designUnitsPerEm);
    Some(Box::new(move |character| {
        let index = *face.glyph_indices(&[character as u32]).ok()?.first()?;
        let metrics = face.design_glyph_metrics(&[index], false).ok()?;
        (index != 0).then(|| metrics.first().map(|glyph| glyph.advanceWidth as f32 * scale))?
    }))
}

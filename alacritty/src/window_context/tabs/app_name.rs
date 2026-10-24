//! The application's name on macOS, in its menu and wherever the system names the process.

use alacritty_tabbed_deps::objc2_foundation::{NSProcessInfo, ns_string};

/// Name the process Alacritty Tabbed rather than after its executable.
///
/// winit titles the application menu's items, like About, Hide and Quit, with the process name
/// when the event loop starts, so this comes before it. The menu bar's bold title is the
/// application bundle's `CFBundleName`, which `make app` sets, or else the executable's name.
pub fn set() {
    NSProcessInfo::processInfo().setProcessName(ns_string!("Alacritty Tabbed"));
}

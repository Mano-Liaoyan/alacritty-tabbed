//! The dependencies Alacritty Tabbed adds to upstream Alacritty's.
//!
//! The fork's code reaches them through this crate, as `alacritty_tabbed_deps::png`, so that
//! upstream's `Cargo.toml` stays as it is.

pub use png;
pub use resvg;

#[cfg(not(any(target_os = "macos", windows)))]
pub use fontconfig_sys;

#[cfg(target_os = "macos")]
pub use core_text;

#[cfg(windows)]
pub use dwrote;
#[cfg(windows)]
pub use windows_sys;

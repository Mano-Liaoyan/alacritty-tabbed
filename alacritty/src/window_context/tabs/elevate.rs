//! Running as administrator.
//!
//! On Windows elevation belongs to a whole process, so a profile set to run as administrator
//! opens in a new elevated Alacritty, which asks with UAC, unless this one already is; every
//! window of an elevated Alacritty shows a shield at the left of its bar, like Windows Terminal.
//!
//! Elsewhere the administrator is root, and a graphical program is not meant to run as root, so
//! such a profile's shell runs through `sudo` in a tab of its own, whose icon wears the shield.
//! The bar shows the shield too when Alacritty itself runs as root.

use std::sync::OnceLock;

use clap::Args;
use serde::{Deserialize, Serialize};

#[cfg(windows)]
use crate::cli::Options as CliOptions;
#[cfg(windows)]
use crate::config::UiConfig;

#[cfg(windows)]
use super::profiles::{Profile, Profiles};

/// Options of the command line for the first tab, which `alacritty msg create-window` takes too.
#[derive(Serialize, Deserialize, Args, Default, Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// Open the first tab with this profile from profiles.toml.
    #[clap(long, value_name = "NAME")]
    pub profile: Option<String>,

    /// Run as administrator, elevated with UAC on Windows, through sudo elsewhere.
    #[clap(long)]
    pub elevate: bool,
}

/// Whether Alacritty runs as administrator, or as root.
pub fn elevated() -> bool {
    static ELEVATED: OnceLock<bool> = OnceLock::new();
    *ELEVATED.get_or_init(platform::elevated)
}

/// Whether opening `profile` here would not make it administrator, so it opens in a new elevated
/// Alacritty instead.
#[cfg(windows)]
pub fn elsewhere(profile: &Profile) -> bool {
    profile.elevate && !elevated()
}

/// Open `profile` in a new elevated Alacritty, once the user agrees to it, without waiting for
/// them to.
#[cfg(windows)]
pub fn open(profile: &Profile, config: &UiConfig) {
    let mut args = vec!["--profile".into(), profile.name.clone()];
    // A new tab starts in the home folder, while a new window starts where it is started.
    if profile.directory.trim().is_empty() {
        if let Some(home) = home::home_dir() {
            args.extend(["--working-directory".into(), home.to_string_lossy().into_owned()]);
        }
    }
    if let Some(path) = config.config_paths.first() {
        args.extend(["--config-file".into(), path.to_string_lossy().into_owned()]);
    }
    std::thread::spawn(move || {
        if let Err(err) = platform::relaunch(&args) {
            log::warn!("Could not run as administrator: {err}");
        }
    });
}

/// Restart elevated, before the first window opens, when the command line asks to or the
/// profile the first tab opens with runs as administrator; true if this Alacritty should quit.
///
/// The elevated Alacritty gets the same command line less `--elevate`. One started plainly for
/// its default profile restores the last session in this one's place.
#[cfg(windows)]
pub fn startup(config: &UiConfig, options: &CliOptions) -> bool {
    let window = &options.window_options;
    if elevated() || options.daemon || window.terminal_options.command().is_some() {
        return false;
    }
    let asked = window.tabbed.elevate;
    let profiles = Profiles::load(config);
    let profile = match &window.tabbed.profile {
        Some(name) => profiles.list.iter().find(|profile| &profile.name == name),
        None => profiles.default_profile(),
    };
    // A profile asks only where UAC elevates this user, so the elevated Alacritty, whose profile
    // asks too, does not restart again where it would not be elevated after all.
    let wanted = profile.is_some_and(|profile| profile.elevate) && platform::limited();
    if !asked && !wanted {
        return false;
    }
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .filter(|arg| arg != "--elevate")
        .collect();
    match platform::relaunch(&args) {
        Ok(()) => true,
        // Declined, a profile that asks for it opens here as it is; the command line's own
        // request opens nothing.
        Err(err) => {
            log::warn!("Could not run as administrator: {err}");
            asked
        },
    }
}

/// The shell of a profile that runs as root: `command` through `sudo`, or else the user's shell.
#[cfg(not(windows))]
pub fn root(command: Option<(String, Vec<String>)>) -> alacritty_terminal::tty::Shell {
    // `doas` stands in for `sudo` where only it is installed.
    let installed = |tool: &&str| {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
    };
    let tool = ["sudo", "doas"].into_iter().find(installed).unwrap_or("sudo");
    let args = match command {
        Some((program, args)) => ["--".into(), program].into_iter().chain(args).collect(),
        None => vec!["-s".into()],
    };
    alacritty_terminal::tty::Shell::new(tool.into(), args)
}

#[cfg(windows)]
mod platform {
    use std::{io, mem, ptr};

    use alacritty_tabbed_deps::windows_sys;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_ELEVATION_TYPE, TOKEN_INFORMATION_CLASS,
        TOKEN_QUERY, TokenElevation, TokenElevationType, TokenElevationTypeLimited,
    };
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::super::platform::wide;

    pub fn elevated() -> bool {
        token::<TOKEN_ELEVATION>(TokenElevation).is_some_and(|e| e.TokenIsElevated != 0)
    }

    /// Whether this is an administrator's token that UAC filtered, which UAC elevates without
    /// asking for another user, so an elevated Alacritty started for it is never asked to restart
    /// again.
    pub fn limited() -> bool {
        token::<TOKEN_ELEVATION_TYPE>(TokenElevationType) == Some(TokenElevationTypeLimited)
    }

    /// The information `class` of this process's token.
    fn token<T>(class: TOKEN_INFORMATION_CLASS) -> Option<T> {
        unsafe {
            let mut token: HANDLE = ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            let mut value: T = mem::zeroed();
            let mut size = 0;
            let ok = GetTokenInformation(
                token,
                class,
                (&mut value as *mut T).cast(),
                mem::size_of::<T>() as u32,
                &mut size,
            );
            CloseHandle(token);
            (ok != 0).then_some(value)
        }
    }

    /// Start this program again as administrator with `args`, once the user agrees with UAC.
    pub fn relaunch(args: &[String]) -> io::Result<()> {
        let program = wide(std::env::current_exe()?);
        let directory = wide(std::env::current_dir()?);
        let parameters = wide(args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" "));
        let verb = wide("runas");
        unsafe {
            // The shell asks for COM, on whatever thread it is called from.
            CoInitializeEx(ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as _);
            let mut info: SHELLEXECUTEINFOW = mem::zeroed();
            info.cbSize = mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOASYNC;
            info.lpVerb = verb.as_ptr();
            info.lpFile = program.as_ptr();
            info.lpParameters = parameters.as_ptr();
            info.lpDirectory = directory.as_ptr();
            info.nShow = SW_SHOWNORMAL;
            if ShellExecuteExW(&mut info) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// `arg` quoted for a command line that the C runtime splits back into it.
    pub fn quote(arg: &str) -> String {
        if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '"']) {
            return arg.into();
        }
        let mut quoted = String::from('"');
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    quoted.extend(std::iter::repeat_n('\\', 2 * backslashes + 1));
                    quoted.push('"');
                    backslashes = 0;
                },
                c => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes));
                    quoted.push(c);
                    backslashes = 0;
                },
            }
        }
        quoted.extend(std::iter::repeat_n('\\', 2 * backslashes));
        quoted.push('"');
        quoted
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn elevated() -> bool {
        unsafe { libc::geteuid() == 0 }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn arguments_are_quoted_for_the_c_runtime() {
        use super::platform::quote;
        assert_eq!(quote("--profile"), "--profile");
        assert_eq!(quote("Windows PowerShell"), "\"Windows PowerShell\"");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote(r"C:\Program Files\"), r#""C:\Program Files\\""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r#"a\"b c"#), r#""a\\\"b c""#);
        assert_eq!(quote(r"C:\dir"), r"C:\dir");
    }

    #[cfg(windows)]
    #[test]
    fn a_filtered_token_is_not_elevated() {
        assert!(!(super::platform::limited() && super::elevated()));
    }

    #[cfg(not(windows))]
    #[test]
    fn root_runs_the_command_through_sudo_or_the_shell() {
        let shell = super::root(Some(("htop".into(), vec!["-d".into(), "5".into()])));
        let debug = format!("{shell:?}");
        assert!(debug.contains("\"--\", \"htop\", \"-d\", \"5\""), "{debug}");
        assert!(format!("{:?}", super::root(None)).contains("\"-s\""));
    }
}

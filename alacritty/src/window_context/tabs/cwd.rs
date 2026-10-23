//! Where a new pane starts to continue in the directory of the pane it comes from.
//!
//! A shell can tell its directory with `OSC 7` or `OSC 9 ; 9`, which the terminal keeps. Without
//! that, the directory of the shell's process is read: from `/proc` or the system on Unix, and
//! from the process's parameters on Windows, which `cmd` and MSYS shells such as Git Bash keep up
//! to date but PowerShell does not. WSL shells are out of Windows' reach, so they must report it,
//! and the new pane's `wsl.exe` is told where to start with `--cd`.

use std::path::PathBuf;

use alacritty_terminal::cwd::ReportedDirectory;

use super::platform;
use super::profiles::Profile;

/// The profile and directory to start a pane of `profile` in, continuing where another pane is:
/// in the directory its shell `reported`, or that the `process` reading finds.
pub fn inherit(
    profile: Option<Profile>,
    reported: Option<ReportedDirectory>,
    process: impl FnOnce() -> Option<PathBuf>,
) -> (Option<Profile>, Option<PathBuf>) {
    #[cfg(windows)]
    if let Some(wsl) = profile.as_ref().and_then(|profile| wsl::Command::parse(&profile.command)) {
        let Some(place) = reported.and_then(|reported| wsl.place(&reported)) else {
            return (profile, None);
        };
        let (command, directory) = wsl.start_in(place);
        return (profile.map(|profile| Profile { command, ..profile }), directory);
    }

    let local = |reported: ReportedDirectory| {
        let path = native(&reported.path)?;
        (local_host(&reported.host, &platform::hostname()) && path.is_dir()).then_some(path)
    };
    // Unix systems tell where the program in the foreground is, which beats a report that a
    // program which does not report might have made stale; Windows only knows the shell's.
    #[cfg(not(windows))]
    let directory = process().or_else(|| reported.and_then(local));
    #[cfg(windows)]
    let directory = reported.and_then(local).or_else(process);
    (profile, directory)
}

/// Whether `host`, from an `OSC 7` report, is this computer, called `hostname`.
fn local_host(host: &str, hostname: &str) -> bool {
    host.is_empty() || host.eq_ignore_ascii_case("localhost") || host.eq_ignore_ascii_case(hostname)
}

/// The reported `path` on this system.
#[cfg(not(windows))]
fn native(path: &str) -> Option<PathBuf> {
    path.starts_with('/').then(|| path.into())
}

/// The reported `path` on this system: Windows paths as they are, and the drives of MSYS, Cygwin
/// and WSL paths, such as `/c/Users`, `/cygdrive/c/Users`, `/mnt/c/Users` or `/C:/Users`, as
/// Windows paths.
#[cfg(windows)]
fn native(path: &str) -> Option<PathBuf> {
    windows_path(path).map(PathBuf::from)
}

#[cfg(windows)]
fn windows_path(path: &str) -> Option<String> {
    // A drive's letter, followed by a colon when `colon`, then the path on the drive.
    let drive = |path: &'_ str, colon: bool| {
        let mut chars = path.chars();
        let letter = chars.next().filter(char::is_ascii_alphabetic)?;
        let rest = chars.as_str();
        let rest = if colon { rest.strip_prefix(':')? } else { rest };
        (rest.is_empty() || rest.starts_with(['/', '\\'])).then(|| {
            let rest = rest.trim_start_matches(['/', '\\']).replace('/', "\\");
            format!("{}:\\{rest}", letter.to_ascii_uppercase())
        })
    };

    if path.starts_with(r"\\") {
        return Some(path.into());
    }
    // `C:\Users`, `C:/Users`, or `/C:/Users` from a `file:` URL.
    if let Some(path) = drive(path.strip_prefix('/').unwrap_or(path), true) {
        return Some(path);
    }
    let unix = path.strip_prefix('/')?;
    let unix = unix.strip_prefix("mnt/").or_else(|| unix.strip_prefix("cygdrive/")).unwrap_or(unix);
    drive(unix, false)
}

/// Starting WSL shells where another one reported it is.
#[cfg(windows)]
mod wsl {
    use std::path::{Path, PathBuf};

    use super::{local_host, platform, windows_path};
    use crate::window_context::tabs::profiles;
    use alacritty_terminal::cwd::ReportedDirectory;

    /// Options of `wsl.exe` followed by a value.
    const VALUED: [&str; 7] =
        ["-d", "--distribution", "-u", "--user", "--cd", "--shell-type", "--distribution-id"];

    /// Where a WSL shell starts.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Place {
        /// A Linux path, such as `/home/me`.
        Linux(String),
        /// A Windows path, such as `C:\Users\me`, which WSL translates.
        Windows(String),
    }

    /// A command line running `wsl.exe`.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Command {
        program: String,
        /// The words of its arguments, split at spaces, which `wsl.exe` parses itself.
        words: Vec<String>,
    }

    impl Command {
        pub fn parse(command: &str) -> Option<Self> {
            let (program, args) = profiles::split(command)?;
            let lower = program.trim_matches('"').to_lowercase();
            if lower != "wsl" && !lower.ends_with("wsl.exe") {
                return None;
            }
            let args = args.first().map_or("", String::as_str);
            let words = args.split(' ').filter(|word| !word.is_empty()).map(Into::into).collect();
            Some(Self { program, words })
        }

        /// Index of the first word of the Linux command, after `wsl.exe`'s own options.
        fn command_start(&self) -> usize {
            let mut index = 0;
            while let Some(word) = self.words.get(index) {
                // `wsl.exe ~` starts in the home directory, like `--cd ~`.
                let home = index == 0 && word == "~";
                let command = matches!(word.as_str(), "-e" | "--exec" | "--");
                let option = word.starts_with('-') && !command;
                if VALUED.contains(&word.as_str()) {
                    index = self.value_end(index + 1);
                } else if home || option {
                    index += 1;
                } else {
                    break;
                }
            }
            index
        }

        /// Index after the value starting at word `index`, which may be quoted across words.
        fn value_end(&self, index: usize) -> usize {
            let open = |word: &str| {
                word.starts_with('"') && (word.len() == 1 || !word.ends_with('"'))
            };
            match self.words.get(index) {
                Some(word) if open(word) => {
                    let rest = self.words[index + 1..].iter().position(|word| word.ends_with('"'));
                    rest.map_or(self.words.len(), |end| index + end + 2)
                },
                Some(_) => index + 1,
                None => index,
            }
        }

        /// The distribution the command starts, unless it starts the default one.
        fn distribution(&self) -> Option<&str> {
            let end = self.command_start();
            let option = self.words[..end].iter().position(|word| {
                matches!(word.as_str(), "-d" | "--distribution")
            })?;
            self.words.get(option + 1).map(|name| name.trim_matches('"'))
        }

        /// Where a shell reported it is, in the distribution this command starts, if it can be
        /// told to start there.
        pub fn place(&self, reported: &ReportedDirectory) -> Option<Place> {
            let path = reported.path.as_str();
            let distribution = self.distribution();
            // Windows' view of another distribution's files, or of this one's.
            if let Some(rest) = path.strip_prefix(r"\\") {
                let (server, rest) = rest.split_once('\\').unwrap_or((rest, ""));
                let (name, rest) = rest.split_once('\\').unwrap_or((rest, ""));
                let wsl = ["wsl$", "wsl.localhost"].iter().any(|s| server.eq_ignore_ascii_case(s));
                if wsl && distribution.is_some_and(|d| d.eq_ignore_ascii_case(name)) {
                    return self.linux(format!("/{}", rest.replace('\\', "/")));
                }
                return Path::new(path).is_dir().then(|| Place::Windows(path.into()));
            }
            if !path.starts_with('/') || path.get(2..3) == Some(":") {
                let path = windows_path(path)?;
                return Path::new(&path).is_dir().then_some(Place::Windows(path));
            }
            // A distribution may have its own name, but one only Windows' name means elsewhere
            // unless its files show the place.
            let elsewhere = !local_host(&reported.host, &platform::hostname());
            match distribution {
                Some(_) => self.linux(path.into()),
                None if elsewhere => None,
                None => Some(Place::Linux(path.into())),
            }
        }

        /// A Linux `path`, if the distribution's files show it is there.
        fn linux(&self, path: String) -> Option<Place> {
            let distribution = self.distribution()?;
            let shown = self.shared(&path).is_some_and(|shared| shared.is_dir());
            let shown = shown || {
                let old = format!(r"\\wsl$\{distribution}{}", path.replace('/', "\\"));
                Path::new(&old).is_dir()
            };
            shown.then_some(Place::Linux(path))
        }

        /// Windows' path to the Linux `path` in the distribution.
        fn shared(&self, path: &str) -> Option<PathBuf> {
            let distribution = self.distribution()?;
            let path = path.replace('/', "\\");
            Some(format!(r"\\wsl.localhost\{distribution}{path}").into())
        }

        /// The command line starting in `place`, and the directory to start it in.
        ///
        /// `wsl.exe` reads its own command line, so a path without spaces goes to `--cd` as it
        /// is; other paths become the directory `wsl.exe` starts in, which it translates.
        pub fn start_in(&self, place: Place) -> (String, Option<PathBuf>) {
            let plain = |path: &str| !path.contains(char::is_whitespace) && !path.contains('"');
            match place {
                Place::Linux(path) if plain(&path) => (self.with_cd(Some(&path)), None),
                Place::Linux(path) => match self.shared(&path) {
                    Some(shared) => (self.with_cd(None), Some(shared)),
                    None => (self.with_cd(Some("~")), None),
                },
                Place::Windows(path) => (self.with_cd(None), Some(path.into())),
            }
        }

        /// The command line with `cd` as its `--cd`, or without one.
        fn with_cd(&self, cd: Option<&str>) -> String {
            let start = self.command_start();
            let mut words: Vec<&str> = Vec::new();
            let mut index = 0;
            while index < start {
                let word = self.words[index].as_str();
                let valued = VALUED.contains(&word);
                let end = if valued { self.value_end(index + 1) } else { index + 1 };
                // `wsl.exe ~` is short for `--cd ~`.
                if word != "--cd" && !(index == 0 && word == "~") {
                    words.extend(self.words[index..end].iter().map(String::as_str));
                }
                index = end;
            }
            if let Some(cd) = cd {
                words.extend(["--cd", cd]);
            }
            words.extend(self.words[start..].iter().map(String::as_str));
            let mut command = self.program.clone();
            for word in words {
                command.push(' ');
                command.push_str(word);
            }
            command
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn command(command: &str) -> Command {
            Command::parse(command).unwrap()
        }

        #[test]
        fn wsl_commands_are_recognized() {
            assert!(Command::parse("cmd.exe").is_none());
            assert!(Command::parse(r#""C:\Program Files\Git\bin\bash.exe" -l"#).is_none());
            assert_eq!(command("wsl").distribution(), None);
            assert_eq!(command("wsl.exe -d Ubuntu --cd ~").distribution(), Some("Ubuntu"));
            let wsl = command(r#"C:\Windows\System32\WSL.EXE -u me --distribution "Debian" ls"#);
            assert_eq!(wsl.distribution(), Some("Debian"));
            assert_eq!(command("wsl.exe -e sh -c -d x").distribution(), None);
        }

        #[test]
        fn wsl_commands_take_the_directory() {
            let wsl = command("wsl.exe -d openSUSE-Tumbleweed --cd ~");
            let line = "wsl.exe -d openSUSE-Tumbleweed --cd /home/me/containers";
            assert_eq!(wsl.with_cd(Some("/home/me/containers")), line);
            assert_eq!(wsl.with_cd(None), "wsl.exe -d openSUSE-Tumbleweed");

            // `--cd` is added before the Linux command, and an old one dropped, even quoted.
            let wsl = command(r#"wsl.exe --cd "~/a b" -d Ubuntu -e htop -d 1"#);
            assert_eq!(wsl.with_cd(Some("/tmp")), "wsl.exe -d Ubuntu --cd /tmp -e htop -d 1");
            let wsl = command(r#""C:\Windows\System32\wsl.exe" ~ -d Ubuntu"#);
            let line = r#""C:\Windows\System32\wsl.exe" -d Ubuntu --cd /tmp"#;
            assert_eq!(wsl.with_cd(Some("/tmp")), line);
            assert_eq!(wsl.distribution(), Some("Ubuntu"));
            assert_eq!(command("wsl").with_cd(Some("/tmp")), "wsl --cd /tmp");
        }

        #[test]
        fn wsl_starts_in_paths_with_spaces_from_their_windows_path() {
            let wsl = command("wsl.exe -d Ubuntu --cd ~");
            let (line, directory) = wsl.start_in(Place::Linux("/home/me".into()));
            assert_eq!((line.as_str(), directory), ("wsl.exe -d Ubuntu --cd /home/me", None));
            let (line, directory) = wsl.start_in(Place::Linux("/home/me/a b".into()));
            assert_eq!(line, "wsl.exe -d Ubuntu");
            assert_eq!(directory, Some(r"\\wsl.localhost\Ubuntu\home\me\a b".into()));
            let (line, directory) = wsl.start_in(Place::Windows(r"C:\Users\me".into()));
            assert_eq!(line, "wsl.exe -d Ubuntu");
            assert_eq!(directory, Some(r"C:\Users\me".into()));

            // The default distribution's files have no known path.
            let (line, directory) = command("wsl.exe").start_in(Place::Linux("/a b".into()));
            assert_eq!((line.as_str(), directory), ("wsl.exe --cd ~", None));
        }

        #[test]
        fn wsl_places_come_from_linux_and_windows_paths() {
            let reported = |path: &str| ReportedDirectory { host: "".into(), path: path.into() };
            let wsl = command("wsl.exe --cd ~");
            assert_eq!(wsl.place(&reported("/home/me")), Some(Place::Linux("/home/me".into())));
            let other = ReportedDirectory { host: "elsewhere.example".into(), path: "/srv".into() };
            assert_eq!(wsl.place(&other), None);

            let windows = std::env::temp_dir().display().to_string();
            assert_eq!(wsl.place(&reported(&windows)), Some(Place::Windows(windows.clone())));
            let url = format!("/{}", windows.replace('\\', "/"));
            assert_eq!(wsl.place(&reported(&url)), Some(Place::Windows(windows)));
            assert_eq!(wsl.place(&reported(r"C:\no\such\directory")), None);
        }
    }
}

/// Where the program in the foreground of the shell with the process id `pid` is.
///
/// That is the deepest process the shell started, as far as their directory can be read from
/// their process parameters: shells such as Git Bash's `bash.exe` start another one in turn.
#[cfg(windows)]
pub fn process_directory(pid: u32) -> Option<PathBuf> {
    process::directory(pid)
}

#[cfg(windows)]
mod process {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    use std::{mem, ptr};

    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, IsWow64Process, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_VM_READ,
    };

    /// Deepest generation of processes followed below the shell.
    const MAX_DEPTH: usize = 8;

    /// `PROCESS_BASIC_INFORMATION`.
    #[repr(C)]
    struct BasicInformation {
        exit_status: i32,
        peb: usize,
        affinity: usize,
        priority: i32,
        pid: usize,
        parent: usize,
    }

    /// `UNICODE_STRING`.
    #[repr(C)]
    #[derive(Default)]
    struct UnicodeString {
        length: u16,
        maximum: u16,
        buffer: usize,
    }

    /// An open process, closed when dropped.
    struct Process(HANDLE);

    impl Drop for Process {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    impl Process {
        fn open(pid: u32) -> Option<Self> {
            let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ;
            let handle = unsafe { OpenProcess(access, 0, pid) };
            (!handle.is_null()).then_some(Self(handle))
        }

        /// When the process started, in 100 nanoseconds since 1601.
        fn created(&self) -> Option<u64> {
            let mut times = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
            let [created, exited, kernel, user] = &mut times;
            let ok = unsafe { GetProcessTimes(self.0, created, exited, kernel, user) };
            let time = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            (ok != 0).then_some(time)
        }

        /// Read a `T` at `address` of the process.
        fn read<T: Default>(&self, address: usize) -> Option<T> {
            let mut value = T::default();
            let size = mem::size_of::<T>();
            let buffer = (&mut value as *mut T).cast::<c_void>();
            let mut read = 0;
            let ok = unsafe {
                ReadProcessMemory(self.0, address as *const c_void, buffer, size, &mut read)
            };
            (ok != 0 && read == size).then_some(value)
        }

        /// The process's current directory, from its process parameters.
        fn directory(&self) -> Option<PathBuf> {
            // The parameters of 32-bit processes on 64-bit Windows are elsewhere.
            let mut wow64 = 0;
            if unsafe { IsWow64Process(self.0, &mut wow64) } == 0 || wow64 != 0 {
                return None;
            }

            let mut info: BasicInformation = unsafe { mem::zeroed() };
            let size = mem::size_of::<BasicInformation>() as u32;
            let info_ptr = (&mut info as *mut BasicInformation).cast();
            let class = ProcessBasicInformation;
            let null = ptr::null_mut();
            let status = unsafe { NtQueryInformationProcess(self.0, class, info_ptr, size, null) };
            if status < 0 || info.peb == 0 {
                return None;
            }

            // `PEB.ProcessParameters` follows four bytes of flags and three pointers, and
            // `RTL_USER_PROCESS_PARAMETERS.CurrentDirectory.DosPath` sixteen bytes and five
            // pointers, each padded to a pointer's size.
            let pointer = mem::size_of::<usize>();
            let parameters: usize = self.read(info.peb + 4 * pointer)?;
            let path: UnicodeString = self.read(parameters + 16 + 5 * pointer)?;
            let len = usize::from(path.length / 2);
            if path.buffer == 0 || len == 0 {
                return None;
            }
            let mut text = vec![0u16; len];
            let (buffer, bytes, mut read) = (text.as_mut_ptr().cast(), len * 2, 0);
            let address = path.buffer as *const c_void;
            let ok = unsafe { ReadProcessMemory(self.0, address, buffer, bytes, &mut read) };
            if ok == 0 || read != bytes {
                return None;
            }
            Some(trim(OsString::from_wide(&text).into()))
        }
    }

    /// The path without the separator Windows keeps at its end, except at a drive's root.
    fn trim(path: PathBuf) -> PathBuf {
        let text = path.to_string_lossy();
        let trimmed = text.trim_end_matches('\\');
        if trimmed.len() == text.len() || trimmed.ends_with(':') || trimmed.is_empty() {
            return path;
        }
        trimmed.into()
    }

    /// Process ids and their parents' ids, of every running process but console hosts, which
    /// console programs started without a console have as children.
    fn processes() -> Vec<(u32, u32)> {
        let mut list = Vec::new();
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return list;
            }
            let mut entry: PROCESSENTRY32W = mem::zeroed();
            entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snapshot, &mut entry) != 0;
            while more {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
                if name != "conhost.exe" && name != "openconsole.exe" {
                    list.push((entry.th32ProcessID, entry.th32ParentProcessID));
                }
                more = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
        }
        list
    }

    pub fn directory(pid: u32) -> Option<PathBuf> {
        let shell = Process::open(pid)?;
        let mut directory = shell.directory();
        let mut created = shell.created()?;
        let processes = processes();
        let mut parent = pid;
        for _ in 0..MAX_DEPTH {
            // The newest child, of those started after their parent, since an ended parent's id
            // may have been reused.
            let child = processes
                .iter()
                .filter(|&&(child, of)| of == parent && child != parent)
                .filter_map(|&(child, _)| {
                    let process = Process::open(child)?;
                    let started = process.created().filter(|&started| started >= created)?;
                    Some((started, child, process))
                })
                .max_by_key(|&(started, ..)| started);
            let Some((started, child, process)) = child else { break };
            directory = process.directory().or(directory);
            (created, parent) = (started, child);
        }
        directory
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn this_process_has_its_directory() {
            let here = std::env::current_dir().unwrap();
            let process = Process::open(std::process::id()).unwrap();
            assert_eq!(process.directory(), Some(here));
        }

        #[test]
        fn trailing_separators_go_except_at_roots() {
            assert_eq!(trim(r"C:\Users\me\".into()), PathBuf::from(r"C:\Users\me"));
            assert_eq!(trim(r"C:\".into()), PathBuf::from(r"C:\"));
            assert_eq!(trim(r"\\server\share\".into()), PathBuf::from(r"\\server\share"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_from_this_computer_are_local() {
        assert!(local_host("", "BOX"));
        assert!(local_host("localhost", "BOX"));
        assert!(local_host("box", "BOX"));
        assert!(!local_host("server", "BOX"));
    }

    #[test]
    #[cfg(windows)]
    fn reported_paths_become_windows_paths() {
        let path = |path: &str| windows_path(path);
        assert_eq!(path(r"C:\Users\me"), Some(r"C:\Users\me".into()));
        assert_eq!(path("d:/src/a b"), Some(r"D:\src\a b".into()));
        assert_eq!(path("C:"), Some(r"C:\".into()));
        assert_eq!(path("/C:/Users/me"), Some(r"C:\Users\me".into()));
        assert_eq!(path("/c/Users/me"), Some(r"C:\Users\me".into()));
        assert_eq!(path("/c"), Some(r"C:\".into()));
        assert_eq!(path("/mnt/d/src"), Some(r"D:\src".into()));
        assert_eq!(path("/cygdrive/e/"), Some(r"E:\".into()));
        assert_eq!(path(r"\\server\share\x"), Some(r"\\server\share\x".into()));
        assert_eq!(path("/home/me"), None);
        assert_eq!(path("/usr/bin"), None);
        assert_eq!(path("/mnt/wsl"), None);
        assert_eq!(path("relative"), None);
        assert_eq!(path("ab:/x"), None);
    }

    #[test]
    fn reports_lead_where_they_can() {
        let here = std::env::temp_dir();
        let reported = ReportedDirectory { host: String::new(), path: here.display().to_string() };
        #[cfg(windows)]
        let reported = ReportedDirectory { path: reported.path.replace('\\', "/"), ..reported };
        let (_, directory) = inherit(None, Some(reported.clone()), || None);
        assert_eq!(directory.map(|path| path.canonicalize().unwrap()), here.canonicalize().ok());

        // Not on this computer.
        let remote = ReportedDirectory { host: "elsewhere.example".into(), ..reported };
        let (_, directory) = inherit(None, Some(remote), || Some("/process".into()));
        assert_eq!(directory, Some("/process".into()));
    }

    #[test]
    #[cfg(windows)]
    fn wsl_profiles_start_where_their_shell_reported() {
        let profile = Profile::new("WSL", "wsl.exe --cd ~");
        let reported = ReportedDirectory { host: String::new(), path: "/home/me/src".into() };
        let (profile, directory) =
            inherit(Some(profile), Some(reported), || panic!("WSL shells cannot be read"));
        assert_eq!(profile.unwrap().command, "wsl.exe --cd /home/me/src");
        assert_eq!(directory, None);

        let profile = Profile::new("WSL", "wsl.exe -d Ubuntu --cd ~");
        let (profile, directory) = inherit(Some(profile), None, || panic!("not read"));
        assert_eq!(profile.unwrap().command, "wsl.exe -d Ubuntu --cd ~");
        assert_eq!(directory, None);
    }
}

//! Whether a pane's terminal runs a program other than its shell, which closing it would end.
//!
//! On Unix the terminal's foreground process group tells: while the shell waits for a command, it
//! is the shell's own, and while a command runs, the command's. On Windows a console has no
//! foreground job, so any process the shell started that still runs counts; processes inside WSL
//! or across SSH are not Windows processes, and do not.

/// The master side of a terminal, which tells its foreground job on Unix; on Windows none does.
#[cfg(not(windows))]
pub type Terminal = std::os::unix::io::RawFd;
#[cfg(windows)]
pub struct Terminal;

/// A program running in a terminal besides its shell: its name, when known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub name: Option<String>,
}

/// Processes that come with a console rather than run in it, which no job is.
#[cfg_attr(not(windows), allow(dead_code))]
const HOSTS: [&str; 3] = ["conhost.exe", "openconsole.exe", "wslhost.exe"];

/// The job running in the terminal whose shell is `shell`, at its master `fd`.
#[cfg(not(windows))]
pub fn running(fd: Terminal, shell: u32) -> Option<Job> {
    // SAFETY: `tcgetpgrp` only reads the terminal's state; a closed `fd` fails with -1.
    let group = unsafe { libc::tcgetpgrp(fd) };
    if !foreground(group, shell) {
        return None;
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let name = std::fs::read_to_string(format!("/proc/{group}/comm"))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let name = None;
    Some(Job { name })
}

/// The job running in the terminal whose shell is `shell`.
#[cfg(windows)]
pub fn running(_: Terminal, shell: u32) -> Option<Job> {
    if shell == 0 {
        return None;
    }
    let processes = windows::processes()?;
    let older = |pid, parent| match (windows::created(pid), windows::created(parent)) {
        (Some(child), Some(parent)) => child < parent,
        _ => false,
    };
    descendant(shell, &processes, older).map(|name| Job { name: Some(name) })
}

/// Whether the foreground process `group` of a terminal is another than its `shell`'s: none, as
/// when the terminal is gone, is not.
#[cfg_attr(windows, allow(dead_code))]
fn foreground(group: i32, shell: u32) -> bool {
    group > 0 && group as u32 != shell
}

/// A process started by `shell`, directly or not, among `processes`, each as its id, its parent's
/// and its executable's name, that is not a console's host: the name of the one closest to the
/// shell. A process `older` than the one whose id it names as its parent was started by another
/// that is gone, whose id was reused.
#[cfg_attr(not(windows), allow(dead_code))]
fn descendant(
    shell: u32,
    processes: &[(u32, u32, String)],
    older: impl Fn(u32, u32) -> bool,
) -> Option<String> {
    let host = |name: &str| HOSTS.iter().any(|host| host.eq_ignore_ascii_case(name));
    let mut parents = vec![shell];
    let mut seen = vec![shell];
    // Generation by generation, so the shell's own child wins over what it ran in turn.
    while !parents.is_empty() {
        let children: Vec<_> = processes
            .iter()
            .filter(|&&(pid, parent, _)| parents.contains(&parent) && !seen.contains(&pid))
            .filter(|&&(pid, parent, _)| !older(pid, parent))
            .collect();
        if let Some((_, _, name)) = children.iter().find(|(_, _, name)| !host(name)) {
            return Some(name.clone());
        }
        parents = children.iter().map(|&&(pid, ..)| pid).collect();
        seen.extend(&parents);
    }
    None
}

#[cfg(windows)]
pub use windows::processes;

#[cfg(windows)]
mod windows {
    use std::mem;

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// The running processes, as their id, their parent's and their executable's name.
    pub fn processes() -> Option<Vec<(u32, u32, String)>> {
        // SAFETY: a snapshot of the processes, closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut processes = Vec::new();
        // SAFETY: the entry is plain data, sized as the API requires.
        let mut entry: PROCESSENTRY32W = unsafe { mem::zeroed() };
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: `snapshot` is valid and `entry` sized.
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while more {
            let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
            processes.push((entry.th32ProcessID, entry.th32ParentProcessID, name));
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        // SAFETY: the snapshot is no longer used.
        unsafe { CloseHandle(snapshot) };
        Some(processes)
    }

    /// When the process `pid` started, in 100 ns since 1601.
    pub fn created(pid: u32) -> Option<u64> {
        // SAFETY: a handle that can only query, closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return None;
        }
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let [mut created, mut exited, mut kernel, mut user] = [zero; 4];
        // SAFETY: `process` is valid and the times are written to.
        let ok = unsafe {
            GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user)
        } != 0;
        // SAFETY: the handle is no longer used.
        unsafe { CloseHandle(process) };
        ok.then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, parent: u32, name: &str) -> (u32, u32, String) {
        (pid, parent, name.into())
    }

    #[test]
    fn a_job_holds_the_foreground() {
        assert!(!foreground(100, 100), "the shell waits for a command");
        assert!(foreground(140, 100), "a command runs");
        assert!(!foreground(-1, 100), "the terminal is gone");
        assert!(!foreground(0, 100));
    }

    #[test]
    fn a_job_is_what_the_shell_started() {
        let shell = 10;
        let never = |_, _| false;
        let idle = [process(10, 1, "pwsh.exe"), process(11, 1, "conhost.exe"), process(12, 2, "x")];
        assert_eq!(descendant(shell, &idle, never), None);

        // A program the shell runs, and what that program runs in turn.
        let busy = [
            process(10, 1, "pwsh.exe"),
            process(20, 10, "cargo.exe"),
            process(30, 20, "rustc.exe"),
        ];
        assert_eq!(descendant(shell, &busy, never).as_deref(), Some("cargo.exe"));

        // A console's host is no job, though what it hosts is.
        let hosted = [process(20, 10, "OpenConsole.exe"), process(30, 20, "vim.exe")];
        assert_eq!(descendant(shell, &hosted, never).as_deref(), Some("vim.exe"));
        assert_eq!(descendant(shell, &[process(20, 10, "conhost.exe")], never), None);

        // Ids looping back on themselves end.
        let looped = [process(20, 10, "conhost.exe"), process(10, 20, "conhost.exe")];
        assert_eq!(descendant(shell, &looped, never), None);

        // A process left by a parent that is gone, whose id the shell now has, is not its job.
        let orphan = [process(5, 10, "svchost.exe"), process(20, 10, "ping.exe")];
        assert_eq!(descendant(shell, &orphan, |pid, _| pid == 5).as_deref(), Some("ping.exe"));
        assert_eq!(descendant(shell, &orphan[..1], |pid, _| pid == 5), None);
    }
}

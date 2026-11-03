//! Whether a pane's terminal runs a program other than its shell, which closing it would end.
//!
//! On Unix the terminal's foreground process group tells: while the shell waits for a command, it
//! is the shell's own, and while a command runs, the command's. On Windows a console has no
//! foreground job, so any process the shell started that still runs counts; processes inside WSL
//! or across SSH are not Windows processes, and do not. The processes themselves are read with
//! `sysinfo`, alike on every platform.

use std::sync::{Mutex, MutexGuard, OnceLock};

use alacritty_tabbed_deps::sysinfo::{
    Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind,
};

/// The master side of a terminal, which tells its foreground job on Unix; on Windows none does.
#[cfg(not(windows))]
pub type Terminal = std::os::unix::io::RawFd;
#[cfg(windows)]
pub struct Terminal;

/// A program running in a terminal besides its shell: its name and command line, when known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Job {
    pub name: Option<String>,
    pub command: Vec<String>,
}

/// Processes that come with a console rather than run in it, which no job is.
#[cfg_attr(not(windows), allow(dead_code))]
const HOSTS: [&str; 3] = ["conhost.exe", "openconsole.exe", "wslhost.exe"];

/// The job running in the terminal whose shell is `shell`, at its master `fd`.
pub fn running(fd: Terminal, shell: u32) -> Option<Job> {
    let mut system = system();
    let pid = Pid::from_u32(job(&mut system, fd, shell)?);
    let kind = ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always);
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), false, kind);
    let Some(process) = system.process(pid) else { return Some(Job::default()) };
    let text = |text: &std::ffi::OsStr| text.to_string_lossy().into_owned();
    Some(Job {
        name: Some(text(process.name())),
        command: process.cmd().iter().map(|arg| text(arg)).collect(),
    })
}

/// The processes, kept between readings so that each refreshes only what changed.
fn system() -> MutexGuard<'static, System> {
    static SYSTEM: OnceLock<Mutex<System>> = OnceLock::new();
    SYSTEM.get_or_init(|| Mutex::new(System::new())).lock().unwrap_or_else(|e| e.into_inner())
}

/// The process of the job: the leader of the terminal's foreground group.
#[cfg(not(windows))]
fn job(_: &mut System, fd: Terminal, shell: u32) -> Option<u32> {
    // SAFETY: `tcgetpgrp` only reads the terminal's state; a closed `fd` fails with -1.
    let group = unsafe { libc::tcgetpgrp(fd) };
    foreground(group, shell).then_some(group as u32)
}

/// The process of the job: the one the shell started that is closest to it.
#[cfg(windows)]
fn job(system: &mut System, _: Terminal, shell: u32) -> Option<u32> {
    if shell == 0 {
        return None;
    }
    let processes = list(system);
    let started = |pid| system.process(Pid::from_u32(pid)).map(|process| process.start_time());
    let older = |pid, parent| matches!((started(pid), started(parent)), (Some(child), Some(parent)) if child < parent);
    descendant(shell, &processes, older)
}

/// The running processes, as their id, their parent's and their executable's name.
#[cfg(windows)]
pub fn processes() -> Option<Vec<(u32, u32, String)>> {
    Some(list(&mut system()))
}

#[cfg(windows)]
fn list(system: &mut System) -> Vec<(u32, u32, String)> {
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let processes = system.processes().iter();
    processes
        .map(|(pid, process)| {
            let parent = process.parent().map_or(0, Pid::as_u32);
            (pid.as_u32(), parent, process.name().to_string_lossy().into_owned())
        })
        .collect()
}

/// Whether the foreground process `group` of a terminal is another than its `shell`'s: none, as
/// when the terminal is gone, is not.
#[cfg_attr(windows, allow(dead_code))]
fn foreground(group: i32, shell: u32) -> bool {
    group > 0 && group as u32 != shell
}

/// A process started by `shell`, directly or not, among `processes`, each as its id, its parent's
/// and its executable's name, that is not a console's host: the id of the one closest to the
/// shell. A process `older` than the one whose id it names as its parent was started by another
/// that is gone, whose id was reused.
#[cfg_attr(not(windows), allow(dead_code))]
fn descendant(
    shell: u32,
    processes: &[(u32, u32, String)],
    older: impl Fn(u32, u32) -> bool,
) -> Option<u32> {
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
        if let Some((pid, ..)) = children.iter().find(|(_, _, name)| !host(name)) {
            return Some(*pid);
        }
        parents = children.iter().map(|&&(pid, ..)| pid).collect();
        seen.extend(&parents);
    }
    None
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
        assert_eq!(descendant(shell, &busy, never), Some(20));

        // A console's host is no job, though what it hosts is.
        let hosted = [process(20, 10, "OpenConsole.exe"), process(30, 20, "vim.exe")];
        assert_eq!(descendant(shell, &hosted, never), Some(30));
        assert_eq!(descendant(shell, &[process(20, 10, "conhost.exe")], never), None);

        // Ids looping back on themselves end.
        let looped = [process(20, 10, "conhost.exe"), process(10, 20, "conhost.exe")];
        assert_eq!(descendant(shell, &looped, never), None);

        // A process left by a parent that is gone, whose id the shell now has, is not its job.
        let orphan = [process(5, 10, "svchost.exe"), process(20, 10, "ping.exe")];
        assert_eq!(descendant(shell, &orphan, |pid, _| pid == 5), Some(20));
        assert_eq!(descendant(shell, &orphan[..1], |pid, _| pid == 5), None);
    }
}

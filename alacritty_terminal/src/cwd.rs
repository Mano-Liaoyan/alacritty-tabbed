//! Working directories reported by shells, through `OSC 7` (`ESC ] 7 ; file://host/path ST`)
//! and Windows Terminal's `OSC 9 ; 9` (`ESC ] 9 ; 9 ; "C:\path" ST`).
//!
//! The ANSI parser leaves both sequences unhandled, so the PTY's output is also scanned here
//! for them; the scan only looks at escape sequences and keeps no more than one report's bytes.

/// Longest report kept, in bytes; longer sequences are skipped.
const MAX_REPORT: usize = 4096;

/// A working directory reported by the shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportedDirectory {
    /// Host named by `OSC 7`, empty when there was none.
    pub host: String,
    /// The path as the shell knows it: `/home/user` for Unix shells and WSL, `/c/Users` from
    /// MSYS shells, `C:\Users` from `OSC 9 ; 9`.
    pub path: String,
}

impl ReportedDirectory {
    /// Parse the payload of an operating system command, without its `ESC ]` and terminator.
    pub fn parse(payload: &[u8]) -> Option<Self> {
        if let Some(url) = payload.strip_prefix(b"7;") {
            return Self::parse_url(url);
        }
        let path = payload.strip_prefix(b"9;9;")?;
        let path = String::from_utf8_lossy(path);
        let path = path.trim();
        let path = path.strip_prefix('"').and_then(|path| path.strip_suffix('"')).unwrap_or(path);
        (!path.is_empty()).then(|| Self { host: String::new(), path: path.into() })
    }

    /// Parse `file://host/path`, with the path percent-encoded, or else a bare path.
    fn parse_url(url: &[u8]) -> Option<Self> {
        let url = String::from_utf8_lossy(url);
        let url = url.trim();
        let (host, path) = match url.split_once("://") {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("file") => {
                let start = rest.find('/')?;
                (&rest[..start], &rest[start..])
            },
            Some(_) => return None,
            None => ("", url),
        };
        let path = percent_decode(path);
        path.starts_with('/').then(|| Self { host: host.into(), path })
    }
}

/// Decode `%XX` escapes, keeping malformed ones as they are.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes.get(index + 1..index + 3);
        let hex = hex.filter(|hex| hex.iter().all(u8::is_ascii_hexdigit));
        let hex = hex.and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match (bytes[index], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                index += 3;
            },
            (byte, _) => {
                decoded.push(byte);
                index += 1;
            },
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    /// Inside an operating system command which may be a report.
    Command,
    /// Inside an operating system command which is not a report.
    Skip,
    /// `ESC` inside an operating system command, which `\` turns into its terminator.
    CommandEscape,
}

/// Scanner for working directory reports in a terminal's output, across reads.
#[derive(Debug, Default)]
pub struct Scanner {
    state: State,
    /// Whether `CommandEscape` returns to `Skip` rather than `Command`.
    skipping: bool,
    payload: Vec<u8>,
}

impl Scanner {
    /// Scan the next `bytes` of output, returning the last directory they complete a report of.
    pub fn scan(&mut self, bytes: &[u8]) -> Option<ReportedDirectory> {
        let mut reported = None;
        let mut index = 0;
        while index < bytes.len() {
            if self.state == State::Ground {
                match bytes[index..].iter().position(|&byte| byte == 0x1b) {
                    Some(offset) => index += offset,
                    None => break,
                }
            }
            let byte = bytes[index];
            index += 1;
            self.state = match (self.state, byte) {
                (State::Ground, _) => State::Escape,
                (State::Escape, b']') => {
                    self.payload.clear();
                    self.skipping = false;
                    State::Command
                },
                (State::Escape, 0x1b) => State::Escape,
                (State::Escape, _) => State::Ground,
                (State::Command | State::Skip, 0x07) => {
                    reported = self.finish().or(reported);
                    State::Ground
                },
                (State::Command | State::Skip, 0x1b) => State::CommandEscape,
                // CAN and SUB abort the sequence.
                (State::Command | State::Skip, 0x18 | 0x1a) => State::Ground,
                (State::Command, byte) => {
                    self.payload.push(byte);
                    if self.payload.len() > MAX_REPORT || !self.may_report() {
                        self.skipping = true;
                        State::Skip
                    } else {
                        State::Command
                    }
                },
                (State::Skip, _) => State::Skip,
                (State::CommandEscape, b'\\') => {
                    reported = self.finish().or(reported);
                    State::Ground
                },
                // Any other escape ends the command unterminated, and starts another sequence.
                (State::CommandEscape, b']') => {
                    self.payload.clear();
                    self.skipping = false;
                    State::Command
                },
                (State::CommandEscape, 0x1b) => State::Escape,
                (State::CommandEscape, _) => State::Ground,
            };
        }
        reported
    }

    /// Whether the command read so far can still be a report.
    fn may_report(&self) -> bool {
        let prefix = |report: &[u8]| {
            let len = self.payload.len().min(report.len());
            self.payload[..len] == report[..len]
        };
        prefix(b"7;") || prefix(b"9;9;")
    }

    fn finish(&mut self) -> Option<ReportedDirectory> {
        let skipped = std::mem::take(&mut self.skipping);
        let reported = if skipped { None } else { ReportedDirectory::parse(&self.payload) };
        self.payload.clear();
        reported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reported(host: &str, path: &str) -> Option<ReportedDirectory> {
        Some(ReportedDirectory { host: host.into(), path: path.into() })
    }

    #[test]
    fn osc_7_reports_decoded_paths_and_hosts() {
        let parse = |text: &str| ReportedDirectory::parse(text.as_bytes());
        assert_eq!(parse("7;file://box/home/me/a%20b"), reported("box", "/home/me/a b"));
        assert_eq!(parse("7;file:///tmp"), reported("", "/tmp"));
        assert_eq!(parse("7;FILE://box/%E2%9C%93%zz%+f%4"), reported("box", "/\u{2713}%zz%+f%4"));
        assert_eq!(parse("7;file:///C:/Users/me"), reported("", "/C:/Users/me"));
        assert_eq!(parse("7;file://box/c/Users/me"), reported("box", "/c/Users/me"));
        assert_eq!(parse("7;/srv/data"), reported("", "/srv/data"));
        assert_eq!(parse("7;file://box"), None);
        assert_eq!(parse("7;https://box/path"), None);
        assert_eq!(parse("7;relative"), None);
    }

    #[test]
    fn osc_9_9_reports_windows_paths() {
        let parse = |text: &str| ReportedDirectory::parse(text.as_bytes());
        assert_eq!(parse(r#"9;9;"C:\Users\me\a b""#), reported("", r"C:\Users\me\a b"));
        assert_eq!(parse(r"9;9;D:\"), reported("", r"D:\"));
        let unc = r"\\wsl.localhost\Ubuntu\home";
        assert_eq!(parse(&format!("9;9;{unc}")), reported("", unc));
        assert_eq!(parse(r#"9;9;"""#), None);
        assert_eq!(parse("9;4;1;50"), None);
        assert_eq!(parse("0;title"), None);
    }

    #[test]
    fn scanner_finds_reports_across_reads() {
        let mut scanner = Scanner::default();
        assert_eq!(scanner.scan(b"text \x1b[1mbold\x1b]0;title\x07 \x1b]7;file://h/a"), None);
        assert_eq!(scanner.scan(b"%20b\x1b"), None);
        assert_eq!(scanner.scan(b"\\more"), reported("h", "/a b"));

        // The last report wins, with either terminator.
        let output = b"\x1b]9;9;\"C:\\one\"\x07\x1b]7;file:///two\x1b\\";
        assert_eq!(scanner.scan(output), reported("", "/two"));
        let output = b"\x1b]7;file:///two\x1b\\\x1b]9;9;C:\\one\x07";
        assert_eq!(scanner.scan(output), reported("", r"C:\one"));
    }

    #[test]
    fn scanner_skips_other_and_broken_commands() {
        let mut scanner = Scanner::default();
        // Other commands, however long, are not kept.
        let mut clipboard = b"\x1b]52;c;".to_vec();
        clipboard.extend(std::iter::repeat_n(b'A', 3 * MAX_REPORT));
        clipboard.extend(b"\x07");
        assert_eq!(scanner.scan(&clipboard), None);
        assert!(scanner.payload.capacity() < 16);

        // A report too long to keep.
        let mut long = b"\x1b]7;file:///".to_vec();
        long.extend(std::iter::repeat_n(b'a', MAX_REPORT));
        long.extend(b"\x07");
        assert_eq!(scanner.scan(&long), None);

        // Cancelled, or interrupted by another sequence.
        assert_eq!(scanner.scan(b"\x1b]7;file:///a\x18\x07"), None);
        assert_eq!(scanner.scan(b"\x1b]7;file:///a\x1b[0m\x07"), None);
        assert_eq!(scanner.scan(b"\x1b]7;file:///a\x1b\x1b]7;file:///b\x07"), reported("", "/b"));
    }
}

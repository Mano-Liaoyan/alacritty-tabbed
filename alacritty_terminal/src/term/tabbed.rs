//! The fork's own terminal state, kept apart from upstream's [`Term`].

use crate::cwd::{ReportedDirectory, Scanner};
use crate::grid::Grid;
use crate::term::cell::Cell;
use crate::term::{Term, TermMode};

/// State the fork keeps for each terminal.
#[derive(Debug, Default)]
pub struct Tabbed {
    /// Working directory the shell last reported, through `OSC 7` or `OSC 9 ; 9`.
    pub reported_directory: Option<ReportedDirectory>,

    /// Scan of the PTY's output for working directory reports.
    scanner: Scanner,
}

impl Tabbed {
    /// Scan the next `bytes` the PTY wrote for working directory reports.
    pub(crate) fn scan(&mut self, bytes: &[u8]) {
        if let Some(directory) = self.scanner.scan(bytes) {
            self.reported_directory = Some(directory);
        }
    }
}

impl<T> Term<T> {
    /// The grid of the normal screen, also while a program shows the alternate screen.
    pub fn primary_grid(&self) -> &Grid<Cell> {
        if self.mode.contains(TermMode::ALT_SCREEN) { &self.inactive_grid } else { &self.grid }
    }
}

//! Reopening the windows, tabs and panes open when Alacritty last quit.
//!
//! Stub of the foundation: lane X saves and restores sessions here.

use alacritty_terminal::term::Term;

use crate::event::EventProxy;

/// Write `text`, saved from a pane, into its new terminal `term` before its shell starts.
pub fn replay(_term: &mut Term<EventProxy>, _text: &str) {}

//! Renaming a tab in place of its title in the bar.
//!
//! Seams only for now: editing comes with the tab menu's own work.
#![allow(dead_code, unused_variables)]

use winit::event::{Ime, KeyEvent};
use winit::window::WindowId;

use crate::clipboard::Clipboard;
use crate::window_context::WindowContext;

use super::Tabs;
use super::bar::Layout;
use super::paint::{Layer, Palette, Rect};

/// A tab's name being edited.
pub struct Edit {
    /// A pane of the tab, so the edit follows the tab wherever it moves.
    tab: WindowId,
    text: String,
    /// Byte offset of the caret in `text`.
    caret: usize,
    /// Whether the whole text is selected.
    all: bool,
}

/// Start editing the name of the tab at `tab`.
pub fn start(ctx: &mut WindowContext, tab: usize) {}

/// Handle a key while editing; the edit takes every key.
pub fn key(ctx: &mut WindowContext, key: &KeyEvent, clipboard: &mut Clipboard) {}

/// Handle the input method while editing.
pub fn ime(ctx: &mut WindowContext, ime: Ime) {}

/// Commit the edit when the pointer is pressed outside it; returns whether the press was used.
pub fn press(ctx: &mut WindowContext) -> bool {
    false
}

/// End the edit, giving the tab its name.
pub fn commit(ctx: &mut WindowContext) {}

/// Draw the field of the tab at `tab`, whose title is at `rect`; returns whether it did, in which
/// case the bar draws neither the title nor the close button.
pub fn draw(
    tabs: &Tabs,
    layout: &Layout,
    palette: &Palette,
    tab: usize,
    rect: Rect,
    layer: &mut Layer,
) -> bool {
    false
}

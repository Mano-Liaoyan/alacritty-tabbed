//! Renaming a tab in a field in place of its title in the bar, like Finder renames a file.
//!
//! Enter or Tab commits, Escape cancels, and a press anywhere else or losing the focus commits.

use std::cell::Cell;

use winit::event::{Ime, KeyEvent};
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

use alacritty_terminal::term::ClipboardType;

use crate::clipboard::Clipboard;
use crate::window_context::WindowContext;

use super::bar::{Layout, TAB_RADIUS};
use super::paint::{Layer, Palette, Rect, mix};
use super::{Press, Tabs, redraw};

/// Longest name, in characters.
const MAX: usize = 64;

/// A tab's name being edited.
pub struct Edit {
    /// A pane of the tab, so the edit follows the tab wherever it moves.
    tab: WindowId,
    text: String,
    /// Byte offset of the caret in `text`.
    caret: usize,
    /// Whether the whole text is selected.
    all: bool,
    /// Whether the text was changed, so a title left as it was stays automatic.
    edited: bool,
    /// Text the input method is composing at the caret, which takes the keys until it commits.
    preedit: String,
    /// Where the field was last drawn, where its text starts, and the first byte of it shown.
    shown: Cell<Option<(Rect, f32, usize)>>,
}

impl Edit {
    /// An edit of `text`, all selected, cut to the longest name.
    fn new(tab: WindowId, mut text: String) -> Self {
        if let Some((end, _)) = text.char_indices().nth(MAX) {
            text.truncate(end);
        }
        let (caret, shown) = (text.len(), Cell::new(None));
        Self { tab, text, caret, all: true, edited: false, preedit: String::new(), shown }
    }

    /// The text shown, with what the input method is composing, and the caret in it.
    fn display(&self) -> (String, usize) {
        let preedit = &self.preedit;
        match self.all {
            _ if preedit.is_empty() => (self.text.clone(), self.caret),
            true => (preedit.clone(), preedit.len()),
            false => {
                let (before, after) = self.text.split_at(self.caret);
                (format!("{before}{preedit}{after}"), self.caret + preedit.len())
            },
        }
    }

    /// Where the caret goes for a press `x` pixels into the text shown from byte `first`, which
    /// the text may have changed under since it was drawn, by `offset` in that text.
    fn caret_at(&self, first: usize, offset: impl FnOnce(&str) -> usize) -> usize {
        let first = Some(first).filter(|&first| self.text.is_char_boundary(first)).unwrap_or(0);
        first + offset(&self.text[first..])
    }

    /// Type `text` at the caret, or over the selection, without control characters and up to
    /// the longest name.
    fn insert(&mut self, text: &str) {
        let mut text: String = text.chars().filter(|c| !c.is_control()).collect();
        if text.is_empty() {
            return;
        }
        if self.all {
            self.clear();
        }
        let room = MAX.saturating_sub(self.text.chars().count());
        if let Some((end, _)) = text.char_indices().nth(room) {
            text.truncate(end);
        }
        self.text.insert_str(self.caret, &text);
        self.caret += text.len();
        self.edited = true;
    }

    /// Paste the first line of `text` that is not blank.
    fn paste(&mut self, text: &str) {
        self.insert(text.lines().find(|line| !line.trim().is_empty()).unwrap_or_default());
    }

    /// Erase the selection, or else the character before the caret, or after it when `forward`.
    fn erase(&mut self, forward: bool) {
        if self.all {
            return self.clear();
        }
        let range = match forward {
            true => self.caret..self.caret + self.next(),
            false => self.caret - self.previous()..self.caret,
        };
        if !range.is_empty() {
            self.caret = range.start;
            self.text.replace_range(range, "");
            self.edited = true;
        }
    }

    /// Move the caret by a character, or to that end of the selection.
    fn step(&mut self, forward: bool) {
        if self.all {
            return self.jump(forward);
        }
        self.caret = if forward { self.caret + self.next() } else { self.caret - self.previous() };
    }

    /// Move the caret to the start, or the end.
    fn jump(&mut self, end: bool) {
        self.all = false;
        self.caret = if end { self.text.len() } else { 0 };
    }

    fn select_all(&mut self) {
        self.all = true;
        self.caret = self.text.len();
    }

    fn clear(&mut self) {
        self.text.clear();
        (self.caret, self.all, self.edited) = (0, false, true);
    }

    /// Length in bytes of the character before the caret.
    fn previous(&self) -> usize {
        self.text[..self.caret].chars().next_back().map_or(0, char::len_utf8)
    }

    /// Length in bytes of the character after the caret.
    fn next(&self) -> usize {
        self.text[self.caret..].chars().next().map_or(0, char::len_utf8)
    }
}

/// Index of the tab being renamed.
fn index(tabs: &Tabs) -> Option<usize> {
    let edit = tabs.rename.as_ref()?;
    tabs.tabs.iter().position(|tab| tab.position(edit.tab).is_some())
}

/// Start editing the name of the tab at `tab`, from its name or else its title, all selected.
pub fn start(ctx: &mut WindowContext, tab: usize) {
    commit(ctx);
    let tabs = &ctx.display.tabs;
    let Some(entry) = tabs.tabs.get(tab) else { return };
    let text = match &entry.name {
        Some(name) => name.clone(),
        None if tab == tabs.active => ctx.display.window.title().to_owned(),
        None => entry.pane().title.clone(),
    };
    let edit = Edit::new(entry.pane().id, text);
    ctx.display.tabs.rename = Some(edit);
    redraw(ctx);
}

/// The edit, unless its tab is gone, which cancels it.
fn live(ctx: &mut WindowContext) -> Option<&mut Edit> {
    let tabs = &mut ctx.display.tabs;
    if index(tabs).is_none() && tabs.rename.take().is_some() {
        redraw(ctx);
    }
    ctx.display.tabs.rename.as_mut()
}

/// Handle a key while editing; the edit takes every key, and the input method takes them while
/// it composes.
pub fn key(ctx: &mut WindowContext, key: &KeyEvent, clipboard: &mut Clipboard) {
    let modifiers = ctx.display.tabs.modifiers;
    let command = modifiers.control_key() || modifiers.super_key();
    let Some(edit) = live(ctx).filter(|edit| edit.preedit.is_empty()) else { return };
    match key.logical_key.as_ref() {
        Key::Named(NamedKey::Enter | NamedKey::Tab) => return commit(ctx),
        Key::Named(NamedKey::Escape) => ctx.display.tabs.rename = None,
        Key::Named(NamedKey::Backspace) => edit.erase(false),
        Key::Named(NamedKey::Delete) => edit.erase(true),
        Key::Named(NamedKey::ArrowLeft) => edit.step(false),
        Key::Named(NamedKey::ArrowRight) => edit.step(true),
        Key::Named(NamedKey::Home) => edit.jump(false),
        Key::Named(NamedKey::End) => edit.jump(true),
        Key::Character("a") if command => edit.select_all(),
        Key::Character("v") if command => edit.paste(&clipboard.load(ClipboardType::Clipboard)),
        _ if command || modifiers.alt_key() => return,
        _ => edit.insert(key.text.as_deref().unwrap_or_default()),
    }
    redraw(ctx);
}

/// Handle the input method while editing: what it composes is shown, and what it commits typed.
pub fn ime(ctx: &mut WindowContext, ime: Ime) {
    let Some(edit) = live(ctx) else { return };
    match ime {
        Ime::Preedit(text, _) => edit.preedit = text,
        Ime::Commit(text) => {
            edit.preedit.clear();
            edit.insert(&text);
        },
        Ime::Enabled | Ime::Disabled => edit.preedit.clear(),
    }
    redraw(ctx);
}

/// Handle a press while editing: inside the field it moves the caret, which uses the press, and
/// anywhere else it commits the edit. Returns whether the press was used.
pub fn press(ctx: &mut WindowContext) -> bool {
    let tabs = &mut ctx.display.tabs;
    let (pointer, font) = (tabs.pointer, tabs.font.clone());
    let Some(edit) = &mut tabs.rename else { return false };
    match (edit.shown.get(), font) {
        (Some((field, x, first)), Some(font)) if field.contains(pointer) => {
            edit.caret = edit.caret_at(first, |text| font.offset(text, pointer.x as f32 - x));
            edit.all = false;
            tabs.press = Press::Swallowed;
            redraw(ctx);
            true
        },
        _ => {
            commit(ctx);
            false
        },
    }
}

/// End the edit, giving the tab its name, or its automatic title back when the name is empty.
pub fn commit(ctx: &mut WindowContext) {
    let index = index(&ctx.display.tabs);
    let Some(edit) = ctx.display.tabs.rename.take() else { return };
    match index {
        Some(index) if edit.edited => super::rename(ctx, index, Some(edit.text)),
        _ => redraw(ctx),
    }
}

/// Draw the field of the tab at `tab`, whose title is at `rect`; returns whether it did, in which
/// case the bar draws neither the title nor the close button.
#[allow(dead_code, reason = "the bar draws the field next")]
pub fn draw(
    tabs: &Tabs,
    layout: &Layout,
    palette: &Palette,
    tab: usize,
    rect: Rect,
    layer: &mut Layer,
) -> bool {
    let Some(edit) = tabs.rename.as_ref().filter(|_| index(tabs) == Some(tab)) else {
        return false;
    };
    let (font, line) = (&layout.font, layout.line);
    let px = |value: f32| layout.px(value);
    let height = (line + px(8.)).min(rect.h).round();
    let y = (rect.center().1 - height / 2.).round();
    let field = Rect::new(rect.x - px(4.), y, rect.w + px(4.), height);
    let ring = px(1.5);
    let radius = layout.radius(TAB_RADIUS - 4.);
    layer.rounded(field, radius, palette.primary);
    layer.rounded(field.inset(ring), radius - ring, palette.field);

    // The text scrolls by whole characters to keep the caret inside the field.
    let room = (field.w - px(8.)).max(0.);
    let (text, caret) = edit.display();
    let mut first = 0;
    while font.width(&text[first..caret], false) > room {
        first += text[first..].chars().next().map_or(1, char::len_utf8);
    }
    let shown = font.fit(&text[first..], room, false);
    let (x, top) = (rect.x, (rect.center().1 - line / 2.).round());
    let width = |end: usize| font.width(&text[first..end], false);
    let thin = px(1.).max(1.);
    if edit.all && edit.preedit.is_empty() {
        let selection = Rect::new(x, top, font.width(&shown, false), line);
        layer.rounded(selection, px(2.), mix(palette.field, palette.primary, 0.35));
    } else {
        let at = (x + width(caret)).round();
        layer.rect(Rect::new(at, top, thin, line), palette.foreground);
        // What the input method composes is underlined, up to the caret.
        if !edit.preedit.is_empty() {
            let start = (x + width(caret.saturating_sub(edit.preedit.len()).max(first))).round();
            layer.rect(Rect::new(start, top + line - thin, at - start, thin), palette.foreground);
        }
    }
    layer.text(x, top, shown, palette.foreground, false);
    edit.shown.set(Some((field, x, first)));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str) -> Edit {
        Edit::new(WindowId::from(1), text.into())
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut edit = edit("zsh");
        assert!((edit.all, edit.caret, edit.edited) == (true, 3, false));
        edit.insert("build");
        assert_eq!((edit.text.as_str(), edit.caret, edit.all), ("build", 5, false));
        edit.insert("s");
        assert_eq!(edit.text, "builds");
        edit.select_all();
        edit.erase(false);
        assert_eq!((edit.text.as_str(), edit.caret), ("", 0));
        assert!(edit.edited);
    }

    #[test]
    fn caret_moves_and_erases_by_character() {
        let mut edit = edit("añb");
        edit.step(false);
        assert_eq!((edit.caret, edit.all), (0, false), "left goes to the selection's start");
        edit.erase(false);
        assert_eq!((edit.text.as_str(), edit.edited), ("añb", false), "nothing before the start");
        edit.step(true);
        edit.step(true);
        assert_eq!(edit.caret, 3, "past the two bytes of ñ");
        edit.erase(false);
        assert_eq!((edit.text.as_str(), edit.caret), ("ab", 1));
        edit.insert("é");
        edit.step(false);
        edit.erase(true);
        assert_eq!((edit.text.as_str(), edit.caret), ("ab", 1));
        edit.jump(true);
        edit.erase(true);
        assert_eq!((edit.text.as_str(), edit.caret), ("ab", 2));
        edit.jump(false);
        assert_eq!(edit.caret, 0);
        edit.select_all();
        edit.step(true);
        assert_eq!((edit.caret, edit.all), (2, false));
    }

    #[test]
    fn long_titles_start_cut_to_the_longest_name() {
        let edit = edit(&"ü".repeat(100));
        assert_eq!((edit.text.chars().count(), edit.caret), (64, 128));
    }

    #[test]
    fn presses_place_the_caret_on_a_character() {
        let mut edit = edit("añb");
        // Drawn scrolled past "a", then the text changed under it: "ñ" now straddles byte 2.
        assert_eq!(edit.caret_at(1, |text| text.len()), 4);
        edit.insert("éé");
        assert_eq!(edit.caret_at(1, |text| text.len()), 4, "byte 1 is inside é: from the start");
        assert_eq!(edit.caret_at(2, |text| text.len()), 4);
        assert_eq!(edit.caret_at(9, |_| 0), 0, "past the end");
    }

    #[test]
    fn composition_shows_at_the_caret() {
        let mut edit = edit("ab");
        edit.preedit = "に".into();
        assert_eq!(edit.display(), ("に".into(), 3), "it replaces the selection");
        edit.step(false);
        edit.step(true);
        assert_eq!(edit.display(), ("aにb".into(), 4));
        edit.preedit.clear();
        assert_eq!(edit.display(), ("ab".into(), 1));
    }

    #[test]
    fn names_are_one_line_of_at_most_64_characters() {
        let mut edit = edit("");
        edit.paste("first\tline\r\nsecond line");
        assert_eq!(edit.text, "firstline");
        edit.select_all();
        edit.paste("\n  \nnext line\nmore");
        assert_eq!(edit.text, "next line", "blank lines before it are skipped");
        edit.select_all();
        edit.paste("firstline");
        edit.insert("\u{1b}");
        assert_eq!(edit.text, "firstline");

        let mut edit = self::edit("");
        edit.insert(&"é".repeat(70));
        assert_eq!(edit.text.chars().count(), 64);
        assert_eq!(edit.caret, edit.text.len());
        edit.insert("x");
        assert_eq!(edit.text.chars().count(), 64);
        // A paste over the selection has the whole room.
        edit.select_all();
        edit.paste(&"ab".repeat(40));
        assert_eq!(edit.text, "ab".repeat(32));
        // Nothing to type leaves the selection.
        edit.select_all();
        edit.paste("\n");
        assert_eq!((edit.text.len(), edit.all), (64, true));
    }
}

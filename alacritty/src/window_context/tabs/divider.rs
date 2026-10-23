//! Dividers between the panes of a tab, dragged to share the space differently.
//!
//! Seams only for now: the hover pill, the drag and its keys come with the dividers' own work.
#![allow(dead_code, unused_variables)]

use std::cell::Cell;
use std::time::Instant;

use winit::dpi::PhysicalPosition;
use winit::event::{KeyEvent, MouseButton};

use crate::window_context::WindowContext;

use super::anim::Tween;
use super::split::Divider;

/// The dividers' hover and drag.
pub struct State {
    /// The divider under the pointer, by its index.
    hovered: Option<usize>,
    drag: Option<Drag>,
    /// How shown the grabber pill is, from 0 to 1; updated as it is drawn.
    pill: Cell<Tween<f32>>,
    /// When a divider was last pressed, to reset it on a double click.
    last_click: Option<Instant>,
}

/// A divider being dragged.
struct Drag {
    index: usize,
    /// Pointer offset from the divider's middle at the press.
    grab: f32,
    /// The pair's ratio at the press, which escape restores.
    ratio: f32,
}

impl Default for State {
    fn default() -> Self {
        Self { hovered: None, drag: None, pill: Cell::new(Tween::new(0.)), last_click: None }
    }
}

impl State {
    /// Whether the pill is still moving at `now`.
    pub fn running(&self, now: Instant) -> bool {
        self.pill.get().running(now)
    }
}

/// Follow the pointer over the dividers, or drag one; returns whether the pointer is theirs.
pub fn moved(ctx: &mut WindowContext, position: PhysicalPosition<f64>) -> bool {
    false
}

/// Press a divider, which sets [`super::Press::Divider`]; returns whether one was pressed.
pub fn press(ctx: &mut WindowContext, button: MouseButton) -> bool {
    false
}

/// End a drag where it is.
pub fn release(ctx: &mut WindowContext) {}

/// Handle a key while dragging, returning whether it was used: escape restores the ratio.
pub fn key(ctx: &mut WindowContext, key: &KeyEvent) -> bool {
    false
}

/// The divider of `dividers` at `point`, with bands sized for `scale`.
fn at(dividers: &[Divider], point: (f32, f32), scale: f32) -> Option<usize> {
    None
}

/// The ratio of `divider` for the pointer at `pointer` along its axis, kept `grab` from its middle.
fn ratio(divider: &Divider, pointer: f32, grab: f32, spacing: f32, snap: f32) -> f32 {
    0.5
}

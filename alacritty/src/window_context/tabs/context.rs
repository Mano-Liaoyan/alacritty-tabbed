//! The context menu of a pane, opened by a right click in it.
//!
//! Not implemented yet: nothing opens it, so the hooks below leave every event to the others.

use std::rc::Rc;
use std::time::Instant;

use winit::dpi::PhysicalPosition;
use winit::event::{KeyEvent, MouseButton};
use winit::event_loop::EventLoopProxy;

use crate::config::UiConfig;
use crate::event::Event;
use crate::window_context::WindowContext;

use super::Tabs;
use super::bar::Layout;
use super::paint::{Layer, Palette};

/// The open context menu.
pub struct Menu;

impl Menu {
    /// Whether the menu is still animating at `now`.
    pub fn running(&self, _now: Instant) -> bool {
        false
    }
}

/// Handle a mouse button press, returning whether the menu used it.
pub fn press(
    _ctx: &mut WindowContext,
    _button: MouseButton,
    _config: &Rc<UiConfig>,
    _proxy: &EventLoopProxy<Event>,
) -> bool {
    false
}

/// Handle a key press, returning whether the menu used it.
pub fn key(
    _ctx: &mut WindowContext,
    _key: &KeyEvent,
    _config: &Rc<UiConfig>,
    _proxy: &EventLoopProxy<Event>,
) -> bool {
    false
}

/// Follow the pointer, returning whether the menu used the movement.
pub fn moved(_ctx: &mut WindowContext, _position: PhysicalPosition<f64>) -> bool {
    false
}

/// Draw the open menu, above everything but the settings.
pub fn draw(_tabs: &Tabs, _layout: &Layout, _palette: &Palette, _now: Instant) -> Option<Layer> {
    None
}

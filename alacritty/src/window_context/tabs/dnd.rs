//! Dragging tabs, and the panes of grouped tabs, to new places.
//!
//! For now a tab is only reordered along the bar while it is dragged.

use std::rc::Rc;

use winit::dpi::PhysicalPosition;
use winit::event::KeyEvent;

use crate::config::UiConfig;
use crate::window_context::WindowContext;

use super::bar::{Hit, Layout};
use super::redraw;
use super::split::{Axis, Side};

/// What is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A tab, with all its panes.
    Tab(usize),
    /// The pane at index `.1` of the tab at `.0`, by its icon in the bar.
    Pane(usize, usize),
}

/// Where a drag would drop what it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Until dropping is implemented.
pub enum Target {
    /// Move the tab to this index.
    Reorder(usize),
    /// Group with the tab at this index.
    Merge(usize),
    /// Beside the pane at this index of the shown tab.
    Dock { pane: usize, axis: Axis, side: Side },
    /// Make the pane a tab of its own, at this index.
    Detach(usize),
}

/// A drag that started on the bar.
pub struct Drag {
    #[allow(dead_code)] // Until dropping is implemented.
    pub source: Source,
    /// Pointer offset from the dragged tab's left edge.
    pub grab: f64,
    pub start: PhysicalPosition<f64>,
    /// Whether the pointer moved far enough to drag rather than click.
    pub moving: bool,
    /// The tab that was active before the drag started.
    #[allow(dead_code)] // Until dropping is implemented.
    pub previous: usize,
    #[allow(dead_code)] // Until dropping is implemented.
    pub target: Option<Target>,
}

/// Begin dragging `source`, once the pointer moves; `previous` was active until it was pressed.
pub fn start(ctx: &mut WindowContext, layout: &Layout, source: Source, previous: usize) {
    let tabs = &mut ctx.display.tabs;
    let (Source::Tab(tab) | Source::Pane(tab, _)) = source;
    let left = layout.tab(tab).map_or(0., |rect| f64::from(rect.x));
    let start = tabs.pointer;
    let grab = start.x - left;
    tabs.drag = Some(Drag { source, grab, start, moving: false, previous, target: None });
}

/// Follow the pointer: move the dragged tab to the slot under it.
pub fn moved(ctx: &mut WindowContext, layout: &Layout) {
    let tabs = &mut ctx.display.tabs;
    let Some(drag) = &mut tabs.drag else { return };
    let x = tabs.pointer.x;
    drag.moving |= (x - drag.start.x).abs() >= 4. * tabs.scale;
    if !drag.moving {
        return;
    }
    tabs.hover = Hit::None;

    let target = layout.drop_target((x - drag.grab) as f32);
    if let Some(target) = target.filter(|&target| target != tabs.active && !tabs.tabs.is_empty()) {
        let tab = tabs.tabs.remove(tabs.active);
        tabs.tabs.insert(target, tab);
        tabs.active = target;
    }
    redraw(ctx);
}

/// End the drag where the pointer was released.
pub fn release(ctx: &mut WindowContext, _config: &Rc<UiConfig>) {
    ctx.display.tabs.drag = None;
}

/// Handle a key while dragging, returning whether it was used.
pub fn key(_ctx: &mut WindowContext, _key: &KeyEvent) -> bool {
    false
}

//! Dragging tabs, and the panes of grouped tabs, to new places.
//!
//! On the bar, dropping onto the middle of a tab groups with it, and dropping onto its edges puts
//! the dragged tab, or a pane let out of its group, between tabs. Below the bar, dropping onto a
//! pane docks beside it, on the side nearest the pointer. Dropping outside the window tears it
//! off into a window of its own.

use std::mem;
use std::rc::Rc;

use winit::dpi::PhysicalPosition;
use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, WindowId};

use crate::config::UiConfig;
use crate::window_context::WindowContext;

use super::bar::{Hit, Layout};
use super::paint::Rect;
use super::split::{self, Axis, Side, Split};
use super::{merge, min_pane, move_pane, redraw, reorder, select, tear};

/// What is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A tab, with all its panes.
    Tab(usize),
    /// The pane at index `.1` of the tab at `.0`, by its icon in the bar or the pane itself.
    Pane(usize, usize),
}

/// Where a drag would drop what it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Move the tab to this index.
    Reorder(usize),
    /// Group with the tab at this index.
    Merge(usize),
    /// Beside the pane at this index of the shown tab.
    Dock { pane: usize, axis: Axis, side: Side },
    /// Make the pane a tab of its own, at this index.
    Detach(usize),
    /// Out of the window, into a window of its own.
    Window,
}

/// A drag of a tab or a pane, from the bar or from a pane.
pub struct Drag {
    pub source: Source,
    /// Pointer offset from the dragged tab's left edge, where the bar holds it.
    pub grab: f64,
    pub start: PhysicalPosition<f64>,
    /// Whether the pointer moved far enough to drag rather than click.
    pub moving: bool,
    /// The tab that was active before the drag started.
    pub previous: usize,
    pub target: Option<Target>,
}

/// Where things can be dropped: the bar's tabs, the shown tab's panes and outside the window.
struct Scene {
    /// Bottom of the bar.
    bar: f32,
    /// The window below the bar, which a tab's panes share.
    area: Rect,
    /// The tabs shown in the bar, by index, in order.
    tabs: Vec<(usize, Rect)>,
    /// The layout of every tab, by index.
    groups: Vec<Split<WindowId>>,
    /// The tab whose panes are shown.
    shown: usize,
    /// The shown panes a dock may cut.
    panes: Vec<(usize, Rect)>,
    /// The panes a dock would add, as they are laid out.
    docked: Option<Split<WindowId>>,
    /// Smallest size a pane may be split to.
    min: (f32, f32),
    /// Pixels between neighbouring panes.
    spacing: f32,
    /// Whether what is dragged may leave for a window of its own.
    tear: bool,
}

impl Scene {
    /// Where `source` would drop at `pointer`, if anywhere it would change something.
    fn target(&self, source: Source, pointer: PhysicalPosition<f64>) -> Option<Target> {
        let (x, y) = (pointer.x as f32, pointer.y as f32);
        let (from, pane) = match source {
            Source::Tab(tab) => (tab, None),
            Source::Pane(tab, pane) => (tab, Some(pane)),
        };

        let area = self.area;
        if x < area.x || y < 0. || x >= area.right() || y >= area.bottom() {
            return self.tear.then_some(Target::Window);
        }

        if y >= self.bar {
            let &(index, rect) = self.panes.iter().find(|(_, rect)| rect.contains(pointer))?;
            let (axis, side) = split::drop_zone(rect, (x, y));
            let fits = self
                .docked
                .as_ref()
                .is_some_and(|new| split::fits(rect, axis, side, new, self.min, self.spacing));
            return match pane {
                None if self.shown == from => None,
                Some(pane) if (self.shown, index) == (from, pane) => None,
                _ if !fits => None,
                _ => Some(Target::Dock { pane: index, axis, side }),
            };
        }

        // The middle half of a tab groups with it; its quarters are the slots on either side.
        let (first, last) = (self.tabs.first()?, self.tabs.last()?);
        let slot = match self.tabs.iter().find(|(_, rect)| x >= rect.x && x < rect.right()) {
            Some(&(index, rect)) => match (x - rect.x) / rect.w {
                part if (0.25..0.75).contains(&part) => {
                    return (index != from && self.joins(index)).then_some(Target::Merge(index));
                },
                part if part < 0.25 => index,
                _ => index + 1,
            },
            None if x < first.1.x => first.0,
            None => last.0 + 1,
        };
        match pane {
            Some(_) => Some(Target::Detach(slot)),
            // The tab leaves its own slot first.
            None => {
                let slot = if slot > from { slot - 1 } else { slot };
                (slot != from).then_some(Target::Reorder(slot))
            },
        }
    }

    /// Whether what is dragged can group with the tab `into`, each pane keeping the smallest size
    /// once it is placed beside them all, as grouping places it.
    fn joins(&self, into: usize) -> bool {
        let (Some(group), Some(new)) = (self.groups.get(into), &self.docked) else { return false };
        let joined = group.clone().join(new.clone(), split::auto_axis(self.area), Side::After);
        joined.fits_in(self.area, self.min, self.spacing)
    }
}

/// Begin dragging `source`, once the pointer moves; `previous` was active until it was pressed.
pub fn start(ctx: &mut WindowContext, layout: &Layout, source: Source, previous: usize) {
    let tabs = &mut ctx.display.tabs;
    let (Source::Tab(tab) | Source::Pane(tab, _)) = source;
    // The icon of a tab's only pane stands for the tab.
    let alone = tabs.tabs.get(tab).is_none_or(|tab| tab.panes.len() < 2);
    let source = if alone { Source::Tab(tab) } else { source };
    let start = tabs.pointer;
    // A tab dragged from its pane is held by its middle.
    let grab = match layout.tab(tab) {
        Some(rect) if start.y < f64::from(layout.bar) => start.x - f64::from(rect.x),
        Some(rect) => f64::from(rect.w / 2.),
        None => 0.,
    };
    tabs.drag = Some(Drag { source, grab, start, moving: false, previous, target: None });
}

/// Follow the pointer: show the tab it would drop into, and where.
pub fn moved(ctx: &mut WindowContext, layout: &Layout, config: &Rc<UiConfig>) {
    let tabs = &mut ctx.display.tabs;
    let Some(drag) = &mut tabs.drag else { return };
    let pointer = tabs.pointer;
    let distance = (pointer.x - drag.start.x).hypot(pointer.y - drag.start.y);
    drag.moving |= distance >= 4. * tabs.scale;
    if !drag.moving {
        return;
    }
    tabs.hover = Hit::None;

    // A tab dragged below the bar is docked into the tab that was shown before it was pressed.
    let (source, previous) = (drag.source, drag.previous);
    if let Source::Tab(tab) = source {
        let below = pointer.y >= f64::from(layout.bar);
        let shown = if below && previous < tabs.tabs.len() { previous } else { tab };
        if shown != tabs.active {
            select(ctx, shown, config);
        }
    }

    let scene = scene(ctx, layout, source);
    let tabs = &mut ctx.display.tabs;
    let target = scene.target(source, pointer);
    let Some(drag) = &mut tabs.drag else { return };
    let changed = mem::replace(&mut drag.target, target) != target;
    ctx.display.window.set_mouse_cursor(CursorIcon::Grabbing);
    // A drop preview over the terminal repaints more than the bar.
    if changed || pointer.y >= f64::from(layout.bar) {
        ctx.display.damage_tracker.frame().mark_fully_damaged();
    }
    redraw(ctx);
}

/// The places to drop onto, as the window is now.
fn scene(ctx: &WindowContext, layout: &Layout, source: Source) -> Scene {
    let tabs = &ctx.display.tabs;
    let docked = match source {
        Source::Tab(tab) => tabs.tabs.get(tab).map(|tab| tab.layout.clone()),
        Source::Pane(tab, pane) => {
            tabs.tabs.get(tab).and_then(|tab| tab.panes.get(pane)).map(|pane| Split::Pane(pane.id))
        },
    };
    let shown = (layout.first..).map_while(|index| Some((index, layout.tab(index)?)));
    // A zoomed tab shows one pane over the places of the others, so nothing docks into it.
    let zoomed = tabs.tabs.get(tabs.active).is_some_and(|tab| tab.zoom);
    Scene {
        bar: layout.bar,
        area: tabs.area(),
        tabs: shown.collect(),
        groups: tabs.tabs.iter().map(|tab| tab.layout.clone()).collect(),
        shown: tabs.active,
        panes: if zoomed { Vec::new() } else { tabs.pane_rects() },
        docked,
        min: min_pane(&ctx.display.size_info),
        spacing: tabs.spacing(),
        tear: tear::can_tear(tabs, source),
    }
}

/// End the drag where the pointer was released.
pub fn release(ctx: &mut WindowContext, config: &Rc<UiConfig>) {
    let Some(drag) = ctx.display.tabs.drag.take() else { return };
    if drag.moving {
        ctx.display.window.set_mouse_cursor(CursorIcon::Default);
        ctx.display.damage_tracker.frame().mark_fully_damaged();
    }
    // Torn off, it lands where the pointer was released, in a window of its own.
    let pointer = ctx.display.tabs.pointer;
    let torn = drag.target == Some(Target::Window)
        && tear::tear_off(ctx, drag.source, Some(pointer), config);
    let tabs = &mut ctx.display.tabs;
    match (drag.source, drag.target) {
        _ if torn => (),
        (Source::Tab(from), Some(Target::Reorder(to))) if tabs.active == from => {
            reorder(&mut tabs.tabs, &mut tabs.active, to)
        },
        (Source::Tab(from), Some(Target::Merge(into))) => merge(ctx, from, into, None, config),
        // A group docks beside the pane as it is.
        (Source::Tab(from), Some(Target::Dock { pane, axis, side }))
            if tabs.tabs.get(from).is_some_and(|tab| tab.panes.len() > 1) =>
        {
            let shown = tabs.active;
            merge(ctx, from, shown, Some((pane, axis, side)), config)
        },
        (Source::Tab(from), Some(target @ Target::Dock { .. })) => {
            move_pane(ctx, (from, 0), target, config)
        },
        (Source::Pane(tab, pane), Some(target)) if target != Target::Window => {
            move_pane(ctx, (tab, pane), target, config)
        },
        // Dropped nowhere, the pressed tab stays selected, as by a click.
        (Source::Tab(from), _) if tabs.active != from && drag.moving => select(ctx, from, config),
        _ => (),
    }
    redraw(ctx);
}

/// End the drag without dropping anything, leaving the pressed tab selected, as by a click.
pub fn cancel(ctx: &mut WindowContext, config: &Rc<UiConfig>) {
    if let Some(drag) = &mut ctx.display.tabs.drag {
        drag.target = None;
        release(ctx, config);
    }
}

/// Handle a key while dragging, returning whether it was used: escape drops nothing.
pub fn key(ctx: &mut WindowContext, key: &KeyEvent, config: &Rc<UiConfig>) -> bool {
    let dragging = ctx.display.tabs.drag.as_ref().is_some_and(|drag| drag.moving);
    if !dragging || key.logical_key != Key::Named(NamedKey::Escape) {
        return false;
    }
    cancel(ctx, config);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u64) -> Split<WindowId> {
        Split::Pane(WindowId::from(id))
    }

    /// Three tabs of 100 px on a 40 px bar from x = 100 in an 800 × 340 px window, showing tab
    /// 1's two panes side by side.
    fn scene() -> Scene {
        let tab = |index: usize| (index, Rect::new(100. + 100. * index as f32, 5., 100., 30.));
        Scene {
            bar: 40.,
            area: Rect::new(0., 40., 800., 300.),
            tabs: (0..3).map(tab).collect(),
            groups: vec![pane(1), pane(2).join(pane(3), Axis::Row, Side::After), pane(4)],
            shown: 1,
            panes: vec![(0, Rect::new(0., 40., 400., 300.)), (1, Rect::new(400., 40., 400., 300.))],
            docked: Some(pane(9)),
            min: (100., 50.),
            spacing: 0.,
            tear: false,
        }
    }

    fn at(x: f64, y: f64) -> PhysicalPosition<f64> {
        PhysicalPosition::new(x, y)
    }

    #[test]
    fn tabs_on_the_bar() {
        let scene = scene();
        let drop = |source, x| scene.target(source, at(x, 20.));
        // The middle half of another tab groups; its own does nothing.
        assert_eq!(drop(Source::Tab(0), 260.), Some(Target::Merge(1)));
        assert_eq!(drop(Source::Tab(0), 226.), Some(Target::Merge(1)));
        assert_eq!(drop(Source::Tab(0), 150.), None);
        // The quarters are the slots between tabs, counted once the tab has left its own.
        assert_eq!(drop(Source::Tab(0), 290.), Some(Target::Reorder(1)));
        assert_eq!(drop(Source::Tab(0), 210.), None);
        assert_eq!(drop(Source::Tab(2), 210.), Some(Target::Reorder(1)));
        assert_eq!(drop(Source::Tab(2), 105.), Some(Target::Reorder(0)));
        assert_eq!(drop(Source::Tab(2), 395.), None);
        // Past either end of the tabs.
        assert_eq!(drop(Source::Tab(2), 20.), Some(Target::Reorder(0)));
        assert_eq!(drop(Source::Tab(0), 700.), Some(Target::Reorder(2)));
    }

    #[test]
    fn panes_on_the_bar() {
        let scene = scene();
        let drop = |source, x| scene.target(source, at(x, 20.));
        // A pane joins another group, or leaves its own for a slot of its own.
        assert_eq!(drop(Source::Pane(1, 0), 150.), Some(Target::Merge(0)));
        assert_eq!(drop(Source::Pane(1, 0), 250.), None);
        assert_eq!(drop(Source::Pane(1, 0), 210.), Some(Target::Detach(1)));
        assert_eq!(drop(Source::Pane(1, 0), 290.), Some(Target::Detach(2)));
        assert_eq!(drop(Source::Pane(1, 0), 700.), Some(Target::Detach(3)));
    }

    #[test]
    fn docking_in_panes() {
        let scene = scene();
        let dock = |pane, axis, side| Some(Target::Dock { pane, axis, side });
        let drop = |source, x, y| scene.target(source, at(x, y));
        // The pane's diagonals divide it between its four sides, whatever its shape.
        assert_eq!(drop(Source::Tab(0), 30., 190.), dock(0, Axis::Row, Side::Before));
        assert_eq!(drop(Source::Tab(0), 370., 190.), dock(0, Axis::Row, Side::After));
        assert_eq!(drop(Source::Tab(0), 200., 60.), dock(0, Axis::Column, Side::Before));
        assert_eq!(drop(Source::Tab(2), 600., 320.), dock(1, Axis::Column, Side::After));
        // Panes are reordered within their group, but nothing docks beside itself.
        assert_eq!(drop(Source::Pane(1, 1), 30., 190.), dock(0, Axis::Row, Side::Before));
        assert_eq!(drop(Source::Pane(1, 0), 30., 190.), None);
        // A tab never docks into its own panes, outside them or onto too small a half.
        assert_eq!(drop(Source::Tab(1), 30., 190.), None);
        assert_eq!(drop(Source::Tab(0), 900., 190.), None);
        let small = Scene { min: (250., 50.), ..scene };
        assert_eq!(small.target(Source::Tab(0), at(30., 190.)), None);
        assert_eq!(
            small.target(Source::Tab(0), at(200., 60.)),
            dock(0, Axis::Column, Side::Before)
        );
        // A group docks only where each of its panes keeps the smallest size.
        let group = pane(8).join(pane(9), Axis::Row, Side::After);
        let grouped = Scene { docked: Some(group), ..small };
        assert_eq!(grouped.target(Source::Tab(0), at(200., 60.)), None);
        let roomy = Scene { min: (100., 50.), ..grouped };
        assert_eq!(
            roomy.target(Source::Tab(0), at(200., 60.)),
            dock(0, Axis::Column, Side::Before)
        );
    }

    #[test]
    fn merging_keeps_the_smallest_size() {
        let merge = |scene: &Scene, source, x| scene.target(source, at(x, 20.));
        // A pane joins tab 1's two panes as a third, a quarter of the width beside the others.
        let roomy = scene();
        assert_eq!(merge(&roomy, Source::Pane(0, 0), 250.), Some(Target::Merge(1)));
        let small = Scene { min: (250., 50.), ..scene() };
        assert_eq!(merge(&small, Source::Pane(0, 0), 250.), None);
        // Beside a lone pane, each still has half.
        assert_eq!(merge(&small, Source::Pane(1, 0), 150.), Some(Target::Merge(0)));
        // A tab brings all its panes along.
        let group = pane(8).join(pane(9), Axis::Row, Side::After);
        let grouped = Scene { docked: Some(group), ..small };
        assert_eq!(merge(&grouped, Source::Tab(2), 150.), None);
        let grouped = Scene { min: (200., 50.), ..grouped };
        assert_eq!(merge(&grouped, Source::Tab(2), 150.), Some(Target::Merge(0)));
        assert_eq!(merge(&grouped, Source::Tab(0), 250.), Some(Target::Merge(1)));
        // The space between panes counts too.
        let spaced = Scene { spacing: 10., ..grouped };
        assert_eq!(merge(&spaced, Source::Tab(2), 150.), None);
        // Reordering never shrinks anything.
        assert_eq!(merge(&spaced, Source::Tab(2), 105.), Some(Target::Reorder(0)));
    }

    #[test]
    fn leaving_the_window() {
        let torn = Scene { tear: true, ..scene() };
        let outside = [at(-1., 20.), at(300., -1.), at(800., 190.), at(300., 340.)];
        for pointer in outside {
            assert_eq!(torn.target(Source::Tab(0), pointer), Some(Target::Window));
            assert_eq!(torn.target(Source::Pane(1, 0), pointer), Some(Target::Window));
            // Nothing drops outside when it would empty the window.
            assert_eq!(scene().target(Source::Tab(0), pointer), None);
        }
        // Inside, the window's edges are the bar's and the panes' as ever.
        assert_eq!(torn.target(Source::Tab(0), at(0., 20.)), None);
        assert_eq!(torn.target(Source::Tab(2), at(0., 20.)), Some(Target::Reorder(0)));
        assert_eq!(
            torn.target(Source::Pane(1, 0), at(799., 339.)),
            Some(Target::Dock { pane: 1, axis: Axis::Row, side: Side::After })
        );
    }

    #[test]
    fn dragging_a_pane_from_itself() {
        // A pane pressed in the shown tab docks beside its neighbour, never beside itself.
        let scene = scene();
        let drop = |pane, x, y| scene.target(Source::Pane(1, pane), at(x, y));
        assert_eq!(drop(1, 600., 190.), None);
        assert_eq!(
            drop(1, 200., 330.),
            Some(Target::Dock { pane: 0, axis: Axis::Column, side: Side::After })
        );
        assert_eq!(
            drop(0, 790., 190.),
            Some(Target::Dock { pane: 1, axis: Axis::Row, side: Side::After })
        );
        // Or leaves for the bar.
        assert_eq!(drop(0, 150., 20.), Some(Target::Merge(0)));
        assert_eq!(drop(0, 390., 20.), Some(Target::Detach(3)));
    }
}

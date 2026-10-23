//! Asking before a pane's close button ends a program still running in it.
//!
//! A pane whose shell only waits for a command closes at once; one running a job asks first, in a
//! popup over it like the context menu, with Cancel and Close. Enter closes it, and Escape or a
//! click outside the popup keeps it.

use std::rc::Rc;
use std::time::Instant;

use winit::dpi::PhysicalPosition;
use winit::event::{KeyEvent, MouseButton};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, WindowId};

use crate::config::UiConfig;
use crate::window_context::WindowContext;

use super::anim::{Curve, Tween};
use super::bar::{Layout, MARGIN, TRACK_RADIUS};
use super::job::{self, Job};
use super::paint::{BLACK, Layer, Palette, Rect, WHITE, mix};
use super::{Press, Tabs, close_pane, redraw};

/// What the close button of a pane does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Closing {
    /// Close it: nothing but its shell runs in it.
    Now,
    /// Ask first, naming the job running in it, when its name is known.
    Ask(Option<String>),
}

/// What the close button of a pane running `job` does.
pub fn closing(job: Option<Job>) -> Closing {
    match job {
        Some(job) => Closing::Ask(job.name),
        None => Closing::Now,
    }
}

/// A button of the popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Cancel,
    Close,
}

/// The open popup, asking whether to close a pane.
pub struct Dialog {
    /// The pane it asks about.
    pane: WindowId,
    /// The job running in it, when its name is known.
    job: Option<String>,
    /// The button under the pointer.
    hovered: Option<Choice>,
    /// How far it has popped in, from 0 to 1.
    pop: Tween<f32>,
}

impl Dialog {
    /// Whether the popup is still animating at `now`.
    pub fn running(&self, now: Instant) -> bool {
        self.pop.running(now)
    }

    fn title(&self) -> &'static str {
        "Close this pane?"
    }

    fn message(&self) -> String {
        match &self.job {
            Some(name) => format!("{name} is still running in it."),
            None => "A program is still running in it.".into(),
        }
    }

    /// The popup's parts, laid out by `layout` over the pane's `card`.
    fn geometry(&self, layout: &Layout, card: Rect) -> Geometry {
        let font = &layout.font;
        let px = |value: f32| layout.px(value);
        let text = font.width(self.title(), true).max(font.width(&self.message(), false));
        let pad = px(18.);
        let button = (px(88.).max(font.width("Cancel", false) + px(24.)), px(32.));
        let bounds = Rect::new(0., 0., layout.width, layout.height).inset(px(MARGIN));
        let width = (text + 2. * pad).max(px(320.)).min(bounds.w);
        let height = 2. * pad + 2. * layout.line + px(6.) + px(20.) + button.1;
        let panel = place(card, (width, height), bounds);
        let (cancel, close) = buttons(panel, pad, button, px(8.));
        let title = Rect::new(panel.x + pad, panel.y + pad, width - 2. * pad, layout.line);
        let message = Rect { y: title.bottom() + px(6.), ..title };
        Geometry { panel, title, message, cancel, close }
    }
}

/// Where the parts of the popup are.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    panel: Rect,
    title: Rect,
    message: Rect,
    cancel: Rect,
    close: Rect,
}

impl Geometry {
    /// The button at `point`.
    fn at(&self, point: PhysicalPosition<f64>) -> Option<Choice> {
        if self.close.contains(point) {
            Some(Choice::Close)
        } else {
            self.cancel.contains(point).then_some(Choice::Cancel)
        }
    }
}

/// A popup of `size` centred over `over`, pushed inside `bounds`.
fn place(over: Rect, (w, h): (f32, f32), bounds: Rect) -> Rect {
    let (x, y) = over.center();
    let fit = |start: f32, size: f32, low: f32, high: f32| start.min(high - size).max(low).round();
    let x = fit(x - w / 2., w, bounds.x, bounds.right());
    let y = fit(y - h / 2., h, bounds.y, bounds.bottom());
    Rect::new(x, y, w, h)
}

/// Cancel and Close, of `size`, `gap` apart at the bottom right of `panel`, `pad` inside it.
fn buttons(panel: Rect, pad: f32, (w, h): (f32, f32), gap: f32) -> (Rect, Rect) {
    let close = Rect::new(panel.right() - pad - w, panel.bottom() - pad - h, w, h);
    (Rect { x: close.x - gap - w, ..close }, close)
}

/// The card of the pane `id` of the active tab, or the area of the panes.
fn card(tabs: &Tabs, id: WindowId) -> Rect {
    let tab = tabs.tabs.get(tabs.active);
    let index = tab.and_then(|tab| tab.position(id));
    let rect = tabs.pane_rects().into_iter().find(|&(i, _)| Some(i) == index);
    rect.map_or_else(|| tabs.area(), |(_, rect)| tabs.card(rect))
}

/// The tab and the index of the pane `id`.
fn locate(tabs: &Tabs, id: WindowId) -> Option<(usize, usize)> {
    tabs.tabs.iter().enumerate().find_map(|(index, tab)| Some((index, tab.position(id)?)))
}

/// The job running in the pane `pane` of the tab `tab`.
fn running(ctx: &WindowContext, tab: usize, pane: usize) -> Option<Job> {
    let pane = ctx.display.tabs.tabs.get(tab)?.panes.get(pane)?;
    // The live pane's terminal is the window's.
    let session = pane.session.as_ref();
    let shell = session.map_or(ctx.shell_pid, |session| session.shell_pid);
    #[cfg(not(windows))]
    let terminal = session.map_or(ctx.master_fd, |session| session.master_fd);
    #[cfg(windows)]
    let terminal = job::Terminal;
    job::running(terminal, shell)
}

/// Close the pane `pane` of the tab `tab` by its close button: at once, or once confirmed while
/// a job runs in it.
pub fn request(ctx: &mut WindowContext, tab: usize, pane: usize, config: &Rc<UiConfig>) {
    let Some(id) = ctx.display.tabs.tabs.get(tab).and_then(|t| t.panes.get(pane)).map(|p| p.id)
    else {
        return;
    };
    match closing(running(ctx, tab, pane)) {
        Closing::Now => close_pane(ctx, tab, pane, config),
        Closing::Ask(job) => {
            let mut pop = Tween::new(0.);
            pop.to(1., Instant::now(), 140, Curve::EASE_OUT);
            let tabs = &mut ctx.display.tabs;
            tabs.confirm = Some(Dialog { pane: id, job, hovered: None, pop });
            tabs.control = None;
            tabs.near = None;
            ctx.display.window.set_mouse_cursor(CursorIcon::Default);
            repaint(ctx);
        },
    }
}

/// Close the popup, closing its pane too once `confirmed`.
fn answer(ctx: &mut WindowContext, confirmed: bool, config: &Rc<UiConfig>) {
    let Some(dialog) = ctx.display.tabs.confirm.take() else { return };
    repaint(ctx);
    if let Some((tab, pane)) = locate(&ctx.display.tabs, dialog.pane).filter(|_| confirmed) {
        close_pane(ctx, tab, pane, config);
    }
}

/// Close the popup once its pane is gone, such as after its shell exited.
fn close_stale(ctx: &mut WindowContext) {
    let tabs = &ctx.display.tabs;
    if tabs.confirm.as_ref().is_some_and(|dialog| locate(tabs, dialog.pane).is_none()) {
        ctx.display.tabs.confirm = None;
        repaint(ctx);
    }
}

/// Handle a mouse button press, returning whether the popup used it; it takes every press while
/// open.
pub fn press(ctx: &mut WindowContext, button: MouseButton, config: &Rc<UiConfig>) -> bool {
    close_stale(ctx);
    let tabs = &mut ctx.display.tabs;
    let Some(dialog) = &tabs.confirm else { return false };
    let layout = Layout::new(tabs);
    let choice = dialog.geometry(&layout, card(tabs, dialog.pane)).at(tabs.pointer);
    tabs.press = Press::Swallowed;
    match (choice, button) {
        (Some(choice), MouseButton::Left) => answer(ctx, choice == Choice::Close, config),
        (Some(_), _) => (),
        (None, _) => answer(ctx, false, config),
    }
    true
}

/// Handle a key press, returning whether the popup used it; it takes every key while open.
pub fn key(ctx: &mut WindowContext, key: &KeyEvent, config: &Rc<UiConfig>) -> bool {
    close_stale(ctx);
    if ctx.display.tabs.confirm.is_none() {
        return false;
    }
    match key.logical_key {
        Key::Named(NamedKey::Enter) => answer(ctx, true, config),
        Key::Named(NamedKey::Escape) => answer(ctx, false, config),
        _ => (),
    }
    true
}

/// Follow the pointer, returning whether the popup used the movement.
pub fn moved(ctx: &mut WindowContext, position: PhysicalPosition<f64>) -> bool {
    close_stale(ctx);
    let tabs = &mut ctx.display.tabs;
    let Some(dialog) = &tabs.confirm else { return false };
    let layout = Layout::new(tabs);
    let hovered = dialog.geometry(&layout, card(tabs, dialog.pane)).at(position);
    let cursor = if hovered.is_some() { CursorIcon::Pointer } else { CursorIcon::Default };
    ctx.display.window.set_mouse_cursor(cursor);
    let Some(dialog) = &mut ctx.display.tabs.confirm else { return true };
    if dialog.hovered != hovered {
        dialog.hovered = hovered;
        repaint(ctx);
    }
    true
}

/// Redraw the whole window, since the popup covers the terminal, which only repaints its changes.
fn repaint(ctx: &mut WindowContext) {
    ctx.display.damage_tracker.frame().mark_fully_damaged();
    redraw(ctx);
}

/// Draw the open popup, above everything but the settings.
///
/// It pops in about its centre, growing from 96% while it fades in.
pub fn draw(tabs: &Tabs, layout: &Layout, palette: &Palette, now: Instant) -> Option<Layer> {
    let dialog = tabs.confirm.as_ref().filter(|dialog| locate(tabs, dialog.pane).is_some())?;
    let parts = dialog.geometry(layout, card(tabs, dialog.pane));
    let shown = dialog.pop.get(now).clamp(0., 1.);
    let scale = 0.96 + 0.04 * shown;
    let origin = parts.panel.center();
    let pop = |r: Rect| {
        let (x, y) = (origin.0 + (r.x - origin.0) * scale, origin.1 + (r.y - origin.1) * scale);
        Rect::new(x, y, r.w * scale, r.h * scale)
    };
    let px = |value: f32| layout.px(value);
    let line = px(1.).max(1.);
    let font = &*layout.font;
    let mut layer = Layer::default();

    // Shadow, outline and background, like the context menu's.
    let (panel, radius) = (pop(parts.panel), layout.radius(TRACK_RADIUS));
    let shadow = Rect::new(panel.x, panel.y + px(2.), panel.w, panel.h);
    layer.rounded_alpha(shadow, radius + layout.radius(2.), BLACK, 0.3);
    layer.rounded(panel, radius, palette.popup_outline);
    layer.rounded(panel.inset(line), radius - line, palette.popup);

    let (title, message) = (pop(parts.title), pop(parts.message));
    let text = font.fit(dialog.title(), title.w, true);
    layer.text(title.x, title.y, text, palette.foreground, true);
    let text = font.fit(&dialog.message(), message.w, false);
    layer.text(message.x, message.y, text, palette.muted, false);

    let button = layout.radius(8.);
    let (cancel, close) = (pop(parts.cancel), pop(parts.close));
    let lit = |choice| if dialog.hovered == Some(choice) { 1. } else { 0. };
    let fill = mix(palette.raised, palette.foreground, 0.08 * lit(Choice::Cancel));
    layer.outlined(cancel, button, fill, palette.raised_outline, line);
    layer.label(cancel, "Cancel", font, palette.foreground, false);
    // Close is the window's close button's red, the action it confirms.
    layer.rounded(close, button, mix(palette.danger, WHITE, 0.12 * lit(Choice::Close)));
    layer.label(close, "Close", font, WHITE, true);

    layer.fade(shown, palette.popup);
    Some(layer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_only_while_a_job_runs() {
        assert_eq!(closing(None), Closing::Now);
        let job = Job { name: Some("vim".into()) };
        assert_eq!(closing(Some(job)), Closing::Ask(Some("vim".into())));
        assert_eq!(closing(Some(Job { name: None })), Closing::Ask(None));
    }

    #[test]
    fn opens_over_the_pane_inside_the_window() {
        let bounds = Rect::new(8., 8., 784., 584.);
        let card = Rect::new(400., 48., 392., 544.);
        assert_eq!(place(card, (320., 140.), bounds), Rect::new(436., 250., 320., 140.));
        // A pane too near the edge for it, or too small, still has it inside the window.
        let corner = Rect::new(700., 500., 80., 60.);
        assert_eq!(place(corner, (320., 140.), bounds), Rect::new(472., 452., 320., 140.));
    }

    #[test]
    fn buttons_sit_at_the_bottom_right() {
        let panel = Rect::new(100., 100., 320., 140.);
        let (cancel, close) = buttons(panel, 18., (88., 32.), 8.);
        assert_eq!(close, Rect::new(314., 190., 88., 32.));
        assert_eq!(cancel, Rect::new(218., 190., 88., 32.));

        let title = Rect::new(118., 118., 284., 17.);
        let parts = Geometry { panel, title, message: title, cancel, close };
        let at = |x: f64, y: f64| parts.at(PhysicalPosition::new(x, y));
        assert_eq!(at(350., 200.), Some(Choice::Close));
        assert_eq!(at(250., 200.), Some(Choice::Cancel));
        assert_eq!(at(310., 200.), None, "between them");
        assert_eq!(at(350., 150.), None);
    }
}

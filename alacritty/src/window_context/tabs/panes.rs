//! The chrome around the panes of the active tab: each sits in a panel of its own, set in from
//! its part of the window by the margin, so neighbouring panels are as far apart as the window's
//! edges.

use std::time::Instant;

use winit::window::WindowId;

use super::Tabs;
use super::bar::{Layout, MARGIN, PANEL_RADIUS};
use super::paint::{Layer, Palette, Rect};

/// Where the panes are drawn while they move to their places.
#[derive(Default)]
pub struct Effects;

impl Effects {
    /// Follow the panes of the active tab to their new places `rects`, at `now`.
    pub fn place(&mut self, _rects: &[(WindowId, Rect)], _now: Instant) {}

    /// Where to draw the pane `id`, whose place is `rect`, at `now`.
    pub fn frame(&self, _id: WindowId, rect: Rect, _now: Instant) -> Rect {
        rect
    }

    /// Whether a pane is still moving at `now`.
    pub fn running(&self, _now: Instant) -> bool {
        false
    }
}

/// Draw the panel of each pane of the active tab, with the chrome around it.
pub fn draw(tabs: &Tabs, layout: &Layout, palette: &Palette, layer: &mut Layer, now: Instant) {
    if !layout.panel {
        return;
    }
    let (chrome, outline) = (palette.chrome, palette.card_outline);
    let (margin, line) = (layout.px(MARGIN), layout.px(1.).max(1.));
    let radius = layout.radius(PANEL_RADIUS).round();
    let Some(tab) = tabs.tabs.get(tabs.active) else { return };
    for (index, rect) in tabs.pane_rects() {
        let rect = tabs.pane_effects.frame(tab.panes[index].id, rect, now);
        let card = rect.inset(margin);
        layer.rect(Rect::new(rect.x, rect.y, rect.w, margin), chrome);
        layer.rect(Rect::new(rect.x, card.y, margin, rect.bottom() - card.y), chrome);
        layer.rect(Rect::new(card.right(), card.y, margin, rect.bottom() - card.y), chrome);
        layer.rect(Rect::new(card.x, card.bottom(), card.w, margin), chrome);
        layer.corners(card, radius, chrome, outline, line);
        let (w, h) = (card.w - 2. * radius, card.h - 2. * radius);
        layer.rect(Rect::new(card.x + radius, card.y, w, line), outline);
        layer.rect(Rect::new(card.x + radius, card.bottom() - line, w, line), outline);
        layer.rect(Rect::new(card.x, card.y + radius, line, h), outline);
        layer.rect(Rect::new(card.right() - line, card.y + radius, line, h), outline);
    }
}

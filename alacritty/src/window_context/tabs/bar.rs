//! The caption: logo, tabs, buttons and window controls in one bar, above a rounded panel that
//! holds the terminal.
//!
//! The layout is the same on every platform; only the window controls follow the system's look.

use std::sync::Arc;

use glutin::surface::Rect as DamageRect;
use unicode_width::UnicodeWidthStr;
use winit::dpi::PhysicalPosition;
use winit::window::ResizeDirection;

use crate::config::UiConfig;
use crate::display::SizeInfo;
use crate::display::color::Rgb;
use crate::display::window::Window;
use crate::renderer::{GlyphCache, Renderer};

use super::paint::{BLACK, Image, Layer, Palette, Rect, WHITE, fit, mix};
use super::{Frame, Tabs, icons};

/// Height of the bar in logical pixels, on every platform.
pub const BAR: f32 = 48.;
/// Space between the window's edge and the terminal's panel.
pub const MARGIN: f32 = 8.;
/// Space between the panel's edge and the terminal.
pub const INSET: f32 = 4.;

/// Last entry of the new-tab menu.
const EDIT_PROFILES: &str = "Edit profiles…";

/// macOS puts its window controls, red, yellow and green lights, at the left.
const MAC: bool = cfg!(target_os = "macos");

/// What the pointer is over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hit {
    /// Chrome around the terminal.
    #[default]
    None,
    Terminal,
    /// Empty space of the bar, which moves the window.
    Drag,
    Resize(ResizeDirection),
    Tab(usize),
    Close(usize),
    New,
    Menu,
    Settings,
    Previous,
    Next,
    Minimize,
    Maximize,
    CloseWindow,
    /// Row of the open menu.
    Item(usize),
}

/// Geometry of the bar, the terminal's panel and the menu, in window pixels.
pub struct Layout {
    pub scale: f32,
    pub cell: (f32, f32),
    pub bar: f32,
    pub width: f32,
    pub first: usize,
    border: f32,
    pub height: f32,
    /// Distance between the left edges of neighbouring tabs.
    step: f32,
    brand: Option<Rect>,
    mark: Rect,
    track: Rect,
    tabs: Vec<(usize, Rect)>,
    buttons: Vec<(Hit, Rect)>,
    pub card: Rect,
    terminal: Rect,
    /// The menu and the height of its rows.
    menu: Option<(Rect, f32)>,
}

impl Layout {
    pub fn new(tabs: &Tabs, size: &SizeInfo) -> Self {
        let scale = tabs.scale as f32;
        let px = |value: f32| (value * scale).round();
        let cell = (size.cell_width(), size.cell_height());
        let (width, height) = (size.width(), size.height() + tabs.height as f32);
        let (bar, count, fullscreen) = (px(BAR), tabs.len(), tabs.frame == Frame::Fullscreen);
        let mac = MAC;
        let border = if tabs.frame == Frame::Normal && !mac { px(4.) } else { 0. };
        let (margin, control, chevron, pad, nav) = (px(MARGIN), px(32.), px(22.), px(3.), px(28.));
        let caption = px(if cfg!(windows) { 40. } else { 34. });
        let button =
            |x: f32, width: f32| Rect::new(x, ((bar - control) / 2.).round(), width, control);

        let lights = if mac { px(72.) } else { 0. };
        let controls = width - margin - if mac { 0. } else { 3. * caption };
        let settings = controls - px(4.) - control;

        // Fullscreen gives the label's space to the tabs; the logo stays.
        let label = !fullscreen && width >= px(780.);
        let brand = lights + if label { px(160.) } else { px(52.) };
        let min_tab = px(124.);
        let available = settings - px(48.) - brand - 2. * pad - px(6.) - control - chevron;
        let overflow = count as f32 > (available / min_tab).floor().max(1.);
        let available = (available - if overflow { 2. * nav + px(6.) } else { 0. }).max(1.);
        let capacity = ((available / min_tab) as usize).max(1);
        let shown = count.min(capacity);
        let step = (available / shown as f32).min(px(236.)).floor();
        let first = tabs
            .first
            .min(tabs.active)
            .max((tabs.active + 1).saturating_sub(capacity))
            .min(count - shown);

        let (top, bottom) = (px(6.), bar - px(6.));
        let tab_rects: Vec<_> = (0..shown)
            .map(|slot| {
                let x = brand + pad + slot as f32 * step;
                (first + slot, Rect::new(x, top + pad, step - px(4.), bottom - top - 2. * pad))
            })
            .collect();
        let right = tab_rects.last().map_or(brand, |(_, rect)| rect.right() + pad);
        let track = Rect::new(brand, top, right - brand, bottom - top);

        let mut buttons = Vec::new();
        let mut x = track.right() + px(6.);
        if overflow {
            buttons.push((Hit::Previous, button(x, nav)));
            buttons.push((Hit::Next, button(x + nav, nav)));
            x += 2. * nav;
        }
        buttons.push((Hit::New, button(x, control)));
        buttons.push((Hit::Menu, button(x + control, chevron)));
        buttons.push((Hit::Settings, button(settings, control)));
        for (index, hit) in [Hit::CloseWindow, Hit::Minimize, Hit::Maximize].into_iter().enumerate()
        {
            let rect = if mac {
                // Lights are 12 pixels wide, with room around them for the pointer.
                Rect::new(px(10. + 20. * index as f32), (bar - px(16.)) / 2., px(16.), px(16.))
            } else {
                // Close is rightmost.
                button(controls + ((index + 2) % 3) as f32 * caption, caption)
            };
            buttons.push((hit, rect));
        }

        let left = border.max(px(6.)) + lights;
        let mark = Rect::new(left, 0., px(26.), bar).square(px(26.));
        let brand = label.then(|| Rect::new(mark.right() + px(10.), 0., px(160.) - px(46.), bar));

        let card = if fullscreen {
            Rect::new(0., bar, width, height - bar)
        } else {
            Rect::new(margin, bar, width - 2. * margin, height - bar - margin)
        };
        let terminal = if fullscreen { card } else { card.inset(px(INSET)) };

        // The menu drops from the new-tab button over the terminal.
        let new = buttons.iter().find(|(hit, _)| *hit == Hit::New).map_or(track, |(_, rect)| *rect);
        let menu = tabs.menu.as_ref().map(|menu| {
            let row = px(34.).max(cell.1 + px(12.));
            let names = menu.profiles.iter().map(|profile| profile.name.width());
            let chars = names.chain([EDIT_PROFILES.width()]).max().unwrap_or(0);
            let w = (chars as f32 * cell.0 + row + px(36.)).max(px(220.)).min(width - 2. * margin);
            let x = new.x.min(width - margin - w).max(margin);
            let rows = menu.profiles.len() + 1;
            (Rect::new(x, bar - px(4.), w, rows as f32 * row + px(12.)), row)
        });

        Self {
            scale,
            cell,
            bar,
            width,
            first,
            border,
            height,
            step,
            brand,
            mark,
            track,
            tabs: tab_rects,
            buttons,
            card,
            terminal,
            menu,
        }
    }

    pub fn px(&self, value: f32) -> f32 {
        (value * self.scale).round()
    }

    pub fn hit(&self, point: PhysicalPosition<f64>) -> Hit {
        let (x, y) = (point.x as f32, point.y as f32);
        if let Some((menu, row)) = self.menu {
            if !menu.contains(point) {
                return Hit::None;
            }
            let rows = ((menu.h - self.px(12.)) / row).round() as usize;
            let index = ((y - menu.y - self.px(6.)) / row).max(0.) as usize;
            return Hit::Item(index.min(rows - 1));
        }

        let border = self.border;
        let (left, right) = (x < border, x >= self.width - border);
        let (top, bottom) = (y < border, y >= self.height - border);
        let direction = match (left, right, top, bottom) {
            (true, _, true, _) => Some(ResizeDirection::NorthWest),
            (_, true, true, _) => Some(ResizeDirection::NorthEast),
            (true, _, _, true) => Some(ResizeDirection::SouthWest),
            (_, true, _, true) => Some(ResizeDirection::SouthEast),
            (true, ..) => Some(ResizeDirection::West),
            (_, true, ..) => Some(ResizeDirection::East),
            (_, _, true, _) => Some(ResizeDirection::North),
            (.., true) => Some(ResizeDirection::South),
            _ => None,
        };
        if let Some(direction) = direction {
            return Hit::Resize(direction);
        }

        if y >= self.bar {
            return if self.terminal.contains(point) { Hit::Terminal } else { Hit::None };
        }
        if let Some(&(hit, _)) = self.buttons.iter().find(|(_, rect)| rect.contains(point)) {
            return hit;
        }
        match self.tabs.iter().find(|(_, rect)| rect.contains(point)) {
            Some(&(index, rect)) if self.close(rect).contains(point) => Hit::Close(index),
            Some(&(index, _)) => Hit::Tab(index),
            None => Hit::Drag,
        }
    }

    pub fn tab(&self, index: usize) -> Option<Rect> {
        self.tabs.iter().find(|(i, _)| *i == index).map(|(_, rect)| *rect)
    }

    /// The tab slot under the middle of a tab whose left edge is at `left`.
    pub fn drop_target(&self, left: f32) -> Option<usize> {
        let &(first, rect) = self.tabs.first()?;
        let slot = ((left - rect.x) / self.step).round().clamp(0., (self.tabs.len() - 1) as f32);
        Some(first + slot as usize)
    }

    fn close(&self, tab: Rect) -> Rect {
        let size = self.px(26.);
        Rect::new(tab.right() - size, tab.y, size, tab.h)
    }

    fn badge(&self, tab: Rect) -> Rect {
        let size = self.px(18.);
        Rect::new(tab.x + self.px(9.), (tab.y + (tab.h - size) / 2.).round(), size, size)
    }
}

/// Draw an icon, or else a badge with text, in `rect`.
fn badge(
    layer: &mut Layer,
    rect: Rect,
    image: Option<&Arc<Image>>,
    text: &str,
    cell: (f32, f32),
    colors: (Rgb, Rgb),
) {
    match image {
        Some(image) => layer.image(rect, image),
        None => {
            let (surface, accent) = colors;
            layer.rounded(rect, rect.w / 4., mix(surface, accent, 0.3));
            layer.label(rect, text, cell, accent, true);
        },
    }
}

impl Tabs {
    /// Add the bar, which lies above the terminal, to the damage of a frame.
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
    pub fn damage(&self, mut damage: Vec<DamageRect>, size: &SizeInfo) -> Vec<DamageRect> {
        if self.height > 0 {
            let bar = (BAR * self.scale as f32).round() as i32;
            let (width, height) = (size.width() as i32, size.height() as i32);
            damage.push(DamageRect::new(0, height + self.height as i32 - bar, width, bar));
        }
        damage
    }

    /// Size of the terminal moved below the bar, for placing the input method's popup.
    pub fn ime(&self, size: &SizeInfo) -> SizeInfo {
        let height = self.height as f32;
        let (cell_width, cell_height) = (size.cell_width(), size.cell_height());
        let (x, y) = (size.padding_x(), size.padding_y() + height);
        SizeInfo::new(size.width(), size.height() + height, cell_width, cell_height, x, y, false)
    }

    /// Draw the caption and the panel around the terminal, and the menu or settings over them.
    pub fn draw(
        &mut self,
        renderer: &mut Renderer,
        glyph_cache: &mut GlyphCache,
        window: &Window,
        size: &SizeInfo,
        config: &UiConfig,
    ) {
        if self.height == 0 {
            return;
        }

        let layout = Layout::new(self, size);
        self.first = layout.first;
        let palette = Palette::new(&config.colors);
        let (cell, bar, width) = (layout.cell, layout.bar, layout.width);
        let px = |value: f32| layout.px(value);
        let line = px(1.).max(1.);
        let icon = (1.3 * layout.scale).max(1.);
        let mut base = Layer::default();
        let mut top = Layer::default();

        // Chrome around the panel that holds the terminal.
        let card = layout.card;
        base.rect(Rect::new(0., 0., width, bar), palette.chrome);
        if self.frame != Frame::Fullscreen {
            let chrome = palette.chrome;
            base.rect(Rect::new(0., bar, card.x, card.h + px(MARGIN)), chrome);
            base.rect(Rect::new(card.right(), bar, card.x, card.h + px(MARGIN)), chrome);
            base.rect(Rect::new(card.x, card.bottom(), card.w, px(MARGIN)), chrome);
            let radius = px(10.);
            base.corners(card, radius, chrome, palette.card_outline, line);
            let outline = palette.card_outline;
            let (w, h) = (card.w - 2. * radius, card.h - 2. * radius);
            base.rect(Rect::new(card.x + radius, card.y, w, line), outline);
            base.rect(Rect::new(card.x + radius, card.bottom() - line, w, line), outline);
            base.rect(Rect::new(card.x, card.y + radius, line, h), outline);
            base.rect(Rect::new(card.right() - line, card.y + radius, line, h), outline);
        }

        // Logo, with the application and computer name beside it.
        base.image(layout.mark.square(px(24.)), &icons::logo());
        if let Some(brand) = layout.brand {
            let middle = (bar / 2.).round();
            let columns = (brand.w / cell.0) as usize;
            let (label, host) = ("Alacritty", fit(&self.host, columns));
            if 2. * cell.1 <= bar - px(8.) {
                base.text(
                    brand.x,
                    middle - cell.1 + px(1.),
                    label.into(),
                    palette.foreground,
                    true,
                );
                base.text(brand.x, middle + px(1.), host, palette.muted, false);
            } else {
                base.text(brand.x, middle - cell.1 / 2., label.into(), palette.foreground, true);
            }
        }

        // Tabs on a rounded track, the active one raised from it.
        let radius = layout.track.h / 2.8;
        let track = palette.track;
        if !layout.tabs.is_empty() {
            base.outlined(layout.track, radius, track, palette.track_outline, line);
        }
        let lit = |index: usize| {
            index == self.active || matches!(self.hover, Hit::Tab(i) | Hit::Close(i) if i == index)
        };
        for pair in layout.tabs.windows(2) {
            let ((index, rect), (next, _)) = (pair[0], pair[1]);
            if !lit(index) && !lit(next) {
                let x = rect.right() + px(2.) - line / 2.;
                base.rect(
                    Rect::new(x, rect.y + rect.h * 0.3, line, rect.h * 0.4),
                    palette.separator,
                );
            }
        }
        let dragging = self.drag.as_ref().filter(|drag| drag.moving);
        for &(index, mut rect) in &layout.tabs {
            let active = index == self.active;
            let layer = match dragging {
                // The dragged tab follows the pointer, above the others.
                Some(drag) if active => {
                    let (first, last) =
                        (layout.tabs[0].1.x, layout.tabs[layout.tabs.len() - 1].1.x);
                    rect.x = ((self.pointer.x - drag.grab) as f32).clamp(first, last).round();
                    let shadow = Rect::new(rect.x, rect.y + px(1.), rect.w, rect.h + px(2.));
                    top.rounded(shadow, rect.h / 3.2 + 1., mix(palette.chrome, BLACK, 0.5));
                    &mut top
                },
                _ => &mut base,
            };

            let radius = rect.h / 3.2;
            let surface = if active {
                layer.outlined(rect, radius, palette.raised, palette.raised_outline, line);
                palette.raised
            } else if lit(index) {
                layer.rounded(rect, radius, palette.hover);
                palette.hover
            } else {
                track
            };

            let tab = &self.tabs[index];
            let title = if active { window.title() } else { &tab.title };
            let colors = (surface, palette.accent(title));
            badge(layer, layout.badge(rect), tab.icon.as_ref(), &tab.badge, cell, colors);

            let close = layout.close(rect);
            if lit(index) {
                if self.hover == Hit::Close(index) {
                    layer.rounded(close.square(px(20.)), px(6.), palette.pressed);
                }
                let lines = [[-1., -1., 1., 1.], [-1., 1., 1., -1.]];
                layer.icon(close.center(), px(4.), icon, &lines, palette.muted);
            }

            let left = layout.badge(rect).right() + px(8.);
            let right = if lit(index) { close.x + px(2.) } else { rect.right() - px(10.) };
            let columns = ((right - left) / cell.0).max(0.) as usize;
            let color = if lit(index) { palette.foreground } else { palette.muted };
            layer.text(left, rect.center().1 - cell.1 / 2., fit(title, columns), color, active);
        }

        for &(hit, rect) in &layout.buttons {
            self.draw_button(&mut base, &layout, &palette, hit, rect);
        }

        if let (Some(menu), Some((rect, row))) = (&self.menu, layout.menu) {
            let radius = px(8.);
            let shadow = Rect::new(rect.x, rect.y + px(2.), rect.w, rect.h);
            top.rounded_alpha(shadow, radius + px(2.), BLACK, 0.3);
            top.outlined(rect, radius, palette.popup, palette.popup_outline, line);
            let highlighted = match self.hover {
                Hit::Item(index) => index,
                _ => menu.selected,
            };
            let names = menu.profiles.iter().map(|profile| profile.name.as_str());
            for (index, name) in names.chain([EDIT_PROFILES]).enumerate() {
                let item = Rect::new(
                    rect.x + px(6.),
                    rect.y + px(6.) + index as f32 * row,
                    rect.w - px(12.),
                    row,
                );
                let surface = if index == highlighted { palette.hover } else { palette.popup };
                if index == highlighted {
                    top.rounded(item, px(6.), surface);
                }

                let icon_rect = Rect::new(item.x + px(8.), item.y, px(18.), row).square(px(18.));
                match menu.profiles.get(index) {
                    Some(profile) => {
                        let image = profile.image(config);
                        let colors = (surface, palette.accent(name));
                        badge(&mut top, icon_rect, image.as_ref(), &profile.badge(), cell, colors);
                    },
                    None => top.gear(icon_rect.center(), px(8.), palette.muted, surface),
                }
                let y = item.center().1 - cell.1 / 2.;
                top.text(icon_rect.right() + px(10.), y, name.into(), palette.foreground, false);
            }
        }

        let mut layers = vec![base, top];
        if let Some(settings) = &self.settings {
            layers.push(settings.draw(&layout, &palette, config, window));
        }

        // Draw over the whole window, then give the terminal its own viewport back.
        let height = size.height() + self.height as f32;
        let window = SizeInfo::new(size.width(), height, cell.0, cell.1, 0., 0., false);
        Layer::draw(layers, renderer, glyph_cache, &window);
        renderer.resize(size);
    }

    fn draw_button(
        &self,
        layer: &mut Layer,
        layout: &Layout,
        palette: &Palette,
        hit: Hit,
        rect: Rect,
    ) {
        let px = |value: f32| layout.px(value);
        let hovered = self.hover == hit;
        let width = (layout.scale).max(1.);
        let center = rect.center();
        let restore = self.frame != Frame::Normal;
        let square = |layer: &mut Layer, (x, y): (f32, f32), size: f32, color: Rgb| {
            let lines =
                [[-1., -1., 1., -1.], [1., -1., 1., 1.], [1., 1., -1., 1.], [-1., 1., -1., -1.]];
            layer.icon((x, y), size, width, &lines, color);
        };
        let window_icon = |layer: &mut Layer, size: f32, color: Rgb| match hit {
            Hit::Minimize => layer.icon(center, size, width, &[[-1., 0., 1., 0.]], color),
            Hit::Maximize if restore => {
                square(layer, (center.0 - size * 0.2, center.1 + size * 0.2), size * 0.8, color);
                let lines = [[-0.6, -1., 1., -1.], [1., -1., 1., 0.6]];
                layer.icon(
                    (center.0 + size * 0.1, center.1 - size * 0.1),
                    size * 0.9,
                    width,
                    &lines,
                    color,
                );
            },
            Hit::Maximize => square(layer, center, size, color),
            _ => layer.icon(center, size, width, &[[-1., -1., 1., 1.], [-1., 1., 1., -1.]], color),
        };

        let control = matches!(hit, Hit::Minimize | Hit::Maximize | Hit::CloseWindow);
        match hit {
            // Flat buttons, red when closing, like Windows' own.
            _ if control && cfg!(windows) => {
                let danger = hovered && hit == Hit::CloseWindow;
                if hovered {
                    let color = if danger { palette.danger } else { palette.hover };
                    layer.rounded(rect, px(8.), color);
                }
                let color = if danger { WHITE } else { palette.muted };
                window_icon(layer, px(5.), color);
            },
            // Round buttons, like GNOME's and KDE's.
            _ if control && !MAC => {
                let danger = hovered && hit == Hit::CloseWindow;
                let background = match (danger, hovered) {
                    (true, _) => palette.danger,
                    (_, true) => mix(palette.chrome, palette.foreground, 0.2),
                    _ => mix(palette.chrome, palette.foreground, 0.1),
                };
                layer.circle(center, px(12.), background);
                window_icon(layer, px(4.), if danger { WHITE } else { palette.foreground });
            },
            _ if control => {
                let color = match hit {
                    Hit::CloseWindow => Rgb::new(255, 95, 87),
                    Hit::Minimize => Rgb::new(254, 188, 46),
                    _ => Rgb::new(40, 200, 64),
                };
                layer.circle(center, px(6.), mix(color, BLACK, 0.15));
                layer.circle(center, px(6.) - width / 2., color);
                // Symbols show while the pointer is over any of the lights.
                if matches!(self.hover, Hit::Minimize | Hit::Maximize | Hit::CloseWindow) {
                    let lines: &[[f32; 4]] = match hit {
                        Hit::CloseWindow => &[[-1., -1., 1., 1.], [-1., 1., 1., -1.]],
                        Hit::Minimize => &[[-1., 0., 1., 0.]],
                        _ => &[[-1., 1., 0.3, -0.3], [1., -1., -0.3, 0.3]],
                    };
                    layer.icon(center, px(3.), width, lines, mix(color, BLACK, 0.6));
                }
            },
            _ => {
                if hovered {
                    layer.rounded(rect, px(8.), palette.hover);
                }
                let color =
                    if hovered || hit == Hit::New { palette.foreground } else { palette.muted };
                let (size, lines): (f32, &[[f32; 4]]) = match hit {
                    Hit::New => (5., &[[-1., 0., 1., 0.], [0., -1., 0., 1.]]),
                    Hit::Menu => (4., &[[-1., -0.5, 0., 0.5], [0., 0.5, 1., -0.5]]),
                    Hit::Previous => (4., &[[0.5, -1., -0.5, 0.], [-0.5, 0., 0.5, 1.]]),
                    Hit::Next => (4., &[[-0.5, -1., 0.5, 0.], [0.5, 0., -0.5, 1.]]),
                    _ => {
                        let background = if hovered { palette.hover } else { palette.chrome };
                        return layer.gear(center, px(8.), color, background);
                    },
                };
                layer.icon(center, px(size), (1.3 * layout.scale).max(1.), lines, color);
            },
        }
    }
}

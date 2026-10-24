//! Scrolling a view of the dialog: the scrollbar that drags it, and the fades at its edges.

use super::super::paint::Rect;

/// A scrollbar: its track and thumb, the band along it that takes the pointer for it, and how
/// far its view scrolls at most.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scrollbar {
    pub(super) track: Rect,
    pub(super) thumb: Rect,
    pub(super) band: Rect,
    pub(super) max: f32,
}

impl Scrollbar {
    /// The scrollbar along `track`, taking the pointer in a band `band` wide ending at its right
    /// edge `right`, for a view `view` long scrolled by `scrolled` of at most `max`, with a thumb
    /// at least `least` long; none when the view does not scroll.
    pub(super) fn new(
        track: Rect,
        right: f32,
        band: f32,
        view: f32,
        scrolled: f32,
        max: f32,
        least: f32,
    ) -> Option<Self> {
        (max > 0.).then(|| Scrollbar {
            track,
            thumb: thumb(track, view, scrolled, max, least),
            band: Rect { x: right - band, w: band, ..track },
            max,
        })
    }

    /// Pointer offset from the thumb's top to drag it by, for a press at `y`: where the thumb
    /// was grabbed, or its middle for a press on the track, which brings the thumb there.
    pub(super) fn grab(&self, y: f32) -> f32 {
        let on_thumb = (self.thumb.y..self.thumb.bottom()).contains(&y);
        if on_thumb { y - self.thumb.y } else { self.thumb.h / 2. }
    }

    /// How far to scroll with the pointer at `y`, `grab` below the top of the dragged thumb.
    pub(super) fn scrolled(&self, y: f32, grab: f32) -> f32 {
        scrolled_to(self.track, self.thumb.h, y - grab, self.max)
    }

    /// The first of the rows scrolled to with the pointer at `y`, for a list scrolled by rows.
    pub(super) fn first_row(&self, y: f32, grab: f32) -> usize {
        self.scrolled(y, grab).round() as usize
    }
}

/// The thumb of a scrollbar along `track`, for a view `view` high scrolled by `scrolled` of at
/// most `scroll_max`, and at least `least` high.
fn thumb(track: Rect, view: f32, scrolled: f32, scroll_max: f32, least: f32) -> Rect {
    let h = (track.h * view / (view + scroll_max)).max(least).min(track.h);
    let fraction = if scroll_max > 0. { (scrolled / scroll_max).clamp(0., 1.) } else { 0. };
    Rect { y: track.y + (track.h - h) * fraction, h, ..track }
}

/// How far to scroll, of at most `scroll_max`, for a thumb `height` high with its top at `top`
/// along `track`.
fn scrolled_to(track: Rect, height: f32, top: f32, scroll_max: f32) -> f32 {
    let room = track.h - height;
    if room <= 0. {
        return 0.;
    }
    ((top - track.y) / room).clamp(0., 1.) * scroll_max
}

/// Strips `step` high filling `band`, each with how opaque it covers what is under it: most at
/// the viewport's edge, at its top or its bottom, easing out to nothing away from it.
pub fn fade(band: Rect, top: bool, step: f32) -> Vec<(Rect, f32)> {
    let step = step.max(1.);
    let count = (band.h / step).ceil() as usize;
    (0..count)
        .map(|index| {
            let from_edge = index as f32 * step;
            let h = step.min(band.h - from_edge);
            let y = if top { band.y + from_edge } else { band.bottom() - from_edge - h };
            // Smoothly from nearly opaque at the edge to clear at the band's far end.
            let t = 1. - (from_edge + h / 2.) / band.h;
            (Rect { y, h, ..band }, 0.92 * t * t * (3. - 2. * t))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbs_follow_the_pointer() {
        let track = Rect::new(0., 100., 3., 400.);
        let least = thumb(track, 50., 0., 10_000., 16.);
        assert_eq!((least.y, least.h), (100., 16.));
        // A thumb dragged to where it is drawn scrolls by as much as it shows.
        for scrolled in [0., 150., 300.] {
            let thumb = thumb(track, 300., scrolled, 300., 16.);
            assert_eq!(thumb.h, 200.);
            assert_eq!(scrolled_to(track, thumb.h, thumb.y, 300.), scrolled);
        }
        // Past either end of its track, it scrolls to that end.
        assert_eq!(scrolled_to(track, 200., -1000., 300.), 0.);
        assert_eq!(scrolled_to(track, 200., 1000., 300.), 300.);
        assert_eq!(scrolled_to(track, 400., 200., 300.), 0.);
    }

    #[test]
    fn scrollbars_take_the_pointer_by_thumb_or_track() {
        let track = Rect::new(0., 100., 3., 400.);
        assert_eq!(Scrollbar::new(track, 10., 12., 50., 0., 0., 16.), None);
        let bar = Scrollbar::new(track, 10., 12., 300., 150., 300., 16.).unwrap();
        assert_eq!((bar.band.x, bar.band.w, bar.band.y, bar.band.h), (-2., 12., 100., 400.));
        assert_eq!((bar.thumb.y, bar.thumb.h), (200., 200.));
        // On the thumb, it keeps where it was grabbed; on the track, the middle comes there.
        assert_eq!(bar.grab(210.), 10.);
        assert_eq!(bar.grab(450.), 100.);
        assert_eq!(bar.scrolled(210., bar.grab(210.)), 150.);
        assert_eq!(bar.scrolled(150., bar.grab(150.)), 0.);
        // Lists scroll by whole rows, and no further than their ends.
        let rows = Scrollbar::new(track, 10., 12., 8., 0., 92., 16.).unwrap();
        assert_eq!(rows.first_row(-1000., 0.), 0);
        assert_eq!(rows.first_row(1000., 0.), 92);
        let halfway = track.y + (track.h - rows.thumb.h) / 2.;
        assert_eq!(rows.first_row(halfway, 0.), 46);
    }

    #[test]
    fn fades_ease_out_from_the_edge() {
        let band = Rect::new(10., 100., 50., 16.);
        for top in [true, false] {
            let strips = fade(band, top, 2.);
            assert_eq!(strips.len(), 8);
            assert_eq!(strips.iter().map(|(rect, _)| rect.h).sum::<f32>(), band.h);
            assert!(strips.iter().all(|(rect, _)| rect.x == band.x && rect.w == band.w));
            assert!(strips.windows(2).all(|pair| pair[0].1 > pair[1].1));
            let edge = if top { band.y } else { band.bottom() - 2. };
            assert_eq!(strips[0].0.y, edge);
            assert!(strips[0].1 > 0.8 && strips[7].1 < 0.05);
        }
    }
}

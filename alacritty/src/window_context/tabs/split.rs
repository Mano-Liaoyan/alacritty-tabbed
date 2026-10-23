//! How a tab's panes share its area: a binary tree of splits, like tmux's.
//!
//! The tree only knows the panes' ids; the tab keeps the panes themselves.

use super::paint::Rect;

/// Fewest columns and lines a split may leave either pane.
pub const MIN_COLUMNS: f32 = 20.;
pub const MIN_LINES: f32 = 5.;

/// How two panes are placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Side by side, with a vertical divider.
    Row,
    /// Stacked, with a horizontal divider.
    Column,
}

/// Which side of a pane another one goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Left or above.
    Before,
    /// Right or below.
    After,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Split<P> {
    Pane(P),
    /// Two parts, `first` taking `ratio` of the space.
    Pair {
        axis: Axis,
        ratio: f32,
        first: Box<Split<P>>,
        second: Box<Split<P>>,
    },
}

impl<P: Copy + PartialEq> Split<P> {
    /// The panes in the order of the tree: those of the first part, then of the second, so
    /// panes side by side or stacked come left to right or top to bottom.
    pub fn panes(&self) -> Vec<P> {
        self.rects(Rect::default()).into_iter().map(|(pane, _)| pane).collect()
    }

    /// This and `new` side by side or stacked, sharing the space evenly.
    pub fn join(self, new: Split<P>, axis: Axis, side: Side) -> Self {
        let (first, second) = match side {
            Side::Before => (new, self),
            Side::After => (self, new),
        };
        Split::Pair { axis, ratio: 0.5, first: Box::new(first), second: Box::new(second) }
    }

    /// Put `new` beside the pane `target`, returning whether it is there.
    pub fn insert(&mut self, target: P, new: Split<P>, axis: Axis, side: Side) -> bool {
        let Some(leaf) = self.leaf_mut(target) else { return false };
        *leaf = Split::Pane(target).join(new, axis, side);
        true
    }

    /// The tree without `pane`, whose neighbour takes its space; none when it was the only one.
    pub fn remove(self, pane: P) -> Option<Self> {
        match self {
            Split::Pane(p) if p == pane => None,
            Split::Pane(_) => Some(self),
            Split::Pair { axis, ratio, first, second } => {
                match (first.remove(pane), second.remove(pane)) {
                    (Some(first), Some(second)) => Some(Split::Pair {
                        axis,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    (Some(rest), None) | (None, Some(rest)) => Some(rest),
                    (None, None) => None,
                }
            },
        }
    }

    /// Each pane's part of `area`, which they tile in whole pixels.
    pub fn rects(&self, area: Rect) -> Vec<(P, Rect)> {
        let mut rects = Vec::new();
        self.tile(area, &mut rects);
        rects
    }

    /// The pane that takes the space of `pane` once it closes: the nearest one beside it.
    pub fn neighbour(&self, pane: P) -> Option<P> {
        let Split::Pair { first, second, .. } = self else { return None };
        if **first == Split::Pane(pane) {
            Some(second.edge(false))
        } else if **second == Split::Pane(pane) {
            Some(first.edge(true))
        } else {
            first.neighbour(pane).or_else(|| second.neighbour(pane))
        }
    }

    fn leaf_mut(&mut self, pane: P) -> Option<&mut Self> {
        if *self == Split::Pane(pane) {
            return Some(self);
        }
        match self {
            Split::Pane(_) => None,
            Split::Pair { first, second, .. } => {
                first.leaf_mut(pane).or_else(|| second.leaf_mut(pane))
            },
        }
    }

    /// The first pane, or the `last`.
    fn edge(&self, last: bool) -> P {
        match self {
            Split::Pane(pane) => *pane,
            Split::Pair { first, second, .. } => if last { second } else { first }.edge(last),
        }
    }

    fn tile(&self, area: Rect, rects: &mut Vec<(P, Rect)>) {
        match self {
            Split::Pane(pane) => rects.push((*pane, area)),
            Split::Pair { axis, ratio, first, second } => {
                let (a, b) = halves(area, *axis, *ratio);
                first.tile(a, rects);
                second.tile(b, rects);
            },
        }
    }
}

/// `area` cut in two along `axis`, the first part taking `ratio` of it.
fn halves(area: Rect, axis: Axis, ratio: f32) -> (Rect, Rect) {
    match axis {
        Axis::Row => {
            let w = (area.w * ratio).round();
            (
                Rect::new(area.x, area.y, w, area.h),
                Rect::new(area.x + w, area.y, area.w - w, area.h),
            )
        },
        Axis::Column => {
            let h = (area.h * ratio).round();
            (
                Rect::new(area.x, area.y, area.w, h),
                Rect::new(area.x, area.y + h, area.w, area.h - h),
            )
        },
    }
}

/// How to split a pane in two when nothing says which way: along its longer side.
pub fn auto_axis(area: Rect) -> Axis {
    if area.w >= area.h { Axis::Row } else { Axis::Column }
}

/// Where a pane dropped at `point` on `pane` goes: beside the edge nearest to it, measured
/// relative to the pane's size, so the pane's diagonals divide it into the four choices.
pub fn drop_zone(pane: Rect, (x, y): (f32, f32)) -> (Axis, Side) {
    let (cx, cy) = pane.center();
    let (dx, dy) = ((x - cx) / (pane.w / 2.), (y - cy) / (pane.h / 2.));
    let side = |d: f32| if d < 0. { Side::Before } else { Side::After };
    if dx.abs() >= dy.abs() { (Axis::Row, side(dx)) } else { (Axis::Column, side(dy)) }
}

/// Whether `new` can go on `side` of `pane` along `axis` with every pane at least `min` pixels
/// in size, those of `new` included.
pub fn fits<P: Copy + PartialEq>(
    pane: Rect,
    axis: Axis,
    side: Side,
    new: &Split<P>,
    min: (f32, f32),
) -> bool {
    let (a, b) = halves(pane, axis, 0.5);
    let (old, area) = match side {
        Side::Before => (b, a),
        Side::After => (a, b),
    };
    let rects = new.rects(area).into_iter().map(|(_, rect)| rect);
    [old].into_iter().chain(rects).all(|rect| rect.w >= min.0 && rect.h >= min.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(axis: Axis, first: Split<u32>, second: Split<u32>) -> Split<u32> {
        first.join(second, axis, Side::After)
    }

    /// ┌───┬───┐
    /// │ 1 │ 2 │
    /// │   ├───┤
    /// │   │ 3 │
    /// └───┴───┘
    fn three() -> Split<u32> {
        pair(Axis::Row, Split::Pane(1), pair(Axis::Column, Split::Pane(2), Split::Pane(3)))
    }

    #[test]
    fn panes_in_reading_order() {
        assert_eq!(three().panes(), [1, 2, 3]);
        let mut split = Split::Pane(1);
        assert!(split.insert(1, Split::Pane(2), Axis::Row, Side::Before));
        assert!(split.insert(1, Split::Pane(3), Axis::Column, Side::After));
        assert_eq!(split.panes(), [2, 1, 3]);
    }

    #[test]
    fn insert_beside_target() {
        let mut split = three();
        assert!(split.insert(3, Split::Pane(4), Axis::Row, Side::After));
        assert_eq!(split.panes(), [1, 2, 3, 4]);
        let rects = split.rects(Rect::new(0., 0., 400., 200.));
        assert_eq!(rects[2], (3, Rect::new(200., 100., 100., 100.)));
        assert_eq!(rects[3], (4, Rect::new(300., 100., 100., 100.)));
        assert!(!split.insert(9, Split::Pane(5), Axis::Row, Side::After));
    }

    #[test]
    fn remove_gives_space_to_sibling() {
        let split = three().remove(2).unwrap();
        assert_eq!(split, pair(Axis::Row, Split::Pane(1), Split::Pane(3)));
        let split = split.remove(1).unwrap();
        assert_eq!(split, Split::Pane(3));
        assert_eq!(split.remove(3), None);
        assert_eq!(three().remove(7), Some(three()));
    }

    #[test]
    fn neighbour_is_nearest_pane_beside() {
        let split = three();
        assert_eq!(split.neighbour(1), Some(2));
        assert_eq!(split.neighbour(2), Some(3));
        assert_eq!(split.neighbour(3), Some(2));
        let split = pair(Axis::Row, three(), Split::Pane(4));
        assert_eq!(split.neighbour(4), Some(3));
        assert_eq!(Split::Pane(1).neighbour(1), None);
    }

    #[test]
    fn rects_tile_area_exactly() {
        // A small linear congruential generator keeps the trees random but reproducible.
        let mut seed = 7u64;
        let mut next = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for _ in 0..200 {
            let mut split = Split::Pane(0);
            let count = 1 + next(12) as u32;
            for id in 1..count {
                let target = next(u64::from(id)) as u32;
                let axis = if next(2) == 0 { Axis::Row } else { Axis::Column };
                let side = if next(2) == 0 { Side::Before } else { Side::After };
                assert!(split.insert(target, Split::Pane(id), axis, side));
            }
            let area = Rect::new(3., 41., 1000. + next(400) as f32, 500. + next(300) as f32);
            let rects = split.rects(area);
            assert_eq!(rects.len(), count as usize);

            let mut covered = 0.;
            for (i, (_, a)) in rects.iter().enumerate() {
                assert!(a.w >= 0. && a.h >= 0.);
                assert_eq!((a.x.fract(), a.y.fract(), a.w.fract(), a.h.fract()), (0., 0., 0., 0.));
                assert!(a.x >= area.x && a.y >= area.y);
                assert!(a.right() <= area.right() && a.bottom() <= area.bottom());
                for (_, b) in &rects[i + 1..] {
                    let overlap =
                        a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom();
                    assert!(!overlap, "{a:?} overlaps {b:?}");
                }
                covered += a.w * a.h;
            }
            assert_eq!(covered, area.w * area.h);
        }
    }

    #[test]
    fn drop_zone_follows_diagonals() {
        let wide = Rect::new(0., 0., 400., 100.);
        assert_eq!(drop_zone(wide, (10., 50.)), (Axis::Row, Side::Before));
        assert_eq!(drop_zone(wide, (390., 50.)), (Axis::Row, Side::After));
        assert_eq!(drop_zone(wide, (200., 5.)), (Axis::Column, Side::Before));
        assert_eq!(drop_zone(wide, (200., 95.)), (Axis::Column, Side::After));
        // Relative to its size, a point 100 px right of the middle of a wide pane is nearer the
        // middle than one 30 px below it.
        assert_eq!(drop_zone(wide, (300., 85.)), (Axis::Column, Side::After));

        let tall = Rect::new(0., 0., 100., 400.);
        assert_eq!(drop_zone(tall, (5., 200.)), (Axis::Row, Side::Before));
        assert_eq!(drop_zone(tall, (50., 390.)), (Axis::Column, Side::After));
        assert_eq!(drop_zone(tall, (90., 120.)), (Axis::Row, Side::After));
        // On a diagonal the sides win.
        assert_eq!(drop_zone(tall, (100., 400.)), (Axis::Row, Side::After));
        assert_eq!(drop_zone(tall, (0., 0.)), (Axis::Row, Side::Before));
    }

    #[test]
    fn auto_axis_splits_longer_side() {
        assert_eq!(auto_axis(Rect::new(0., 0., 800., 600.)), Axis::Row);
        assert_eq!(auto_axis(Rect::new(0., 0., 600., 600.)), Axis::Row);
        assert_eq!(auto_axis(Rect::new(0., 0., 500., 600.)), Axis::Column);
    }

    #[test]
    fn fits_refuses_small_halves() {
        let pane = Rect::new(0., 0., 400., 200.);
        let new = Split::Pane(9);
        assert!(fits(pane, Axis::Row, Side::After, &new, (200., 200.)));
        assert!(!fits(pane, Axis::Row, Side::After, &new, (201., 10.)));
        assert!(fits(pane, Axis::Column, Side::Before, &new, (400., 100.)));
        assert!(!fits(pane, Axis::Column, Side::Before, &new, (10., 101.)));
    }

    #[test]
    fn fits_refuses_small_panes_of_a_group() {
        let pane = Rect::new(0., 0., 400., 200.);
        let group = pair(Axis::Row, Split::Pane(8), Split::Pane(9));
        // Each half has room for one pane, but not for the group's two side by side.
        assert!(!fits(pane, Axis::Row, Side::After, &group, (150., 50.)));
        assert!(fits(pane, Axis::Row, Side::After, &group, (100., 50.)));
        // Docked above, its panes share the half's whole width.
        assert!(fits(pane, Axis::Column, Side::Before, &group, (150., 50.)));
        assert!(!fits(pane, Axis::Column, Side::Before, &group, (150., 101.)));
    }
}

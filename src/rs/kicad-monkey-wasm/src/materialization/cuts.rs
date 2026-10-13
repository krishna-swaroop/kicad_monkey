//! Which subtracting shapes (holes) reach each add shape and region edge of a
//! slice. A renderer extruding the slice into a slab cuts its faces by the
//! stencil rule, but its side walls only where it tests them against the
//! holes; these lists keep that test to the few holes that can matter.

use super::lower::SHAPE_WORDS;
use std::collections::HashMap;

/// Grid cell for finding nearby holes, board millimetres.
const CELL_MM: f32 = 1.0;

/// For each item, a (first, count) range into `indices`, the subtract
/// shapes' record indices (subtracts come first in a slice's shapes).
#[derive(Debug, Default)]
pub struct Cuts {
    pub ranges: Vec<u32>,
    pub indices: Vec<u32>,
}

/// A shape record's bounds on the board: its local bounds through its affine.
pub fn record_bounds(record: &[u32]) -> [f32; 4] {
    let f = |index: usize| f32::from_bits(record[index]);
    let (m11, m12, m21, m22, tx, ty) = (f(4), f(5), f(6), f(7), f(8), f(9));
    let mut out = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for (x, y) in [
        (f(10), f(11)),
        (f(12), f(11)),
        (f(10), f(13)),
        (f(12), f(13)),
    ] {
        let (bx, by) = (m11 * x + m12 * y + tx, m21 * x + m22 * y + ty);
        out = [
            out[0].min(bx),
            out[1].min(by),
            out[2].max(bx),
            out[3].max(by),
        ];
    }
    out
}

fn overlaps(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

fn cells(bounds: &[f32; 4]) -> impl Iterator<Item = (i32, i32)> {
    let cell = |v: f32| (v / CELL_MM).floor() as i32;
    let (x0, x1, y0, y1) = (
        cell(bounds[0]),
        cell(bounds[2]),
        cell(bounds[1]),
        cell(bounds[3]),
    );
    (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| (x, y)))
}

/// The holes among `subtracts` (shape records) whose bounds overlap each of
/// `items`' bounds.
pub fn cuts(subtracts: &[u32], items: impl IntoIterator<Item = [f32; 4]>) -> Cuts {
    let holes: Vec<[f32; 4]> = subtracts
        .as_chunks::<SHAPE_WORDS>()
        .0
        .iter()
        .map(|record| record_bounds(record))
        .collect();
    let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
    for (index, bounds) in holes.iter().enumerate() {
        for cell in cells(bounds) {
            grid.entry(cell).or_default().push(index as u32);
        }
    }
    let mut out = Cuts::default();
    let mut found = Vec::new();
    for bounds in items {
        found.clear();
        if !grid.is_empty() {
            for cell in cells(&bounds) {
                if let Some(members) = grid.get(&cell) {
                    found.extend(
                        members
                            .iter()
                            .copied()
                            .filter(|&hole| overlaps(&holes[hole as usize], &bounds)),
                    );
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        out.ranges
            .extend([out.indices.len() as u32, found.len() as u32]);
        out.indices.extend_from_slice(&found);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle(x: f32, y: f32, r: f32) -> [u32; SHAPE_WORDS] {
        let mut words = [0u32; SHAPE_WORDS];
        for (slot, value) in words[4..14]
            .iter_mut()
            .zip([1.0, 0.0, 0.0, 1.0, x, y, -r, -r, r, r])
        {
            *slot = f32::to_bits(value);
        }
        words
    }

    #[test]
    fn items_list_only_the_holes_their_bounds_meet() {
        let holes: Vec<u32> = [
            circle(1.0, 1.0, 0.2),
            circle(5.0, 5.0, 0.2),
            circle(5.6, 5.0, 0.2),
        ]
        .concat();
        let found = cuts(
            &holes,
            [
                [0.0, 0.0, 2.0, 2.0],
                [4.9, 4.9, 5.5, 5.1],
                [10.0, 10.0, 11.0, 11.0],
            ],
        );
        assert_eq!(found.ranges, vec![0, 1, 1, 2, 3, 0]);
        assert_eq!(found.indices, vec![0, 1, 2]);
        // A rotated record's bounds follow its affine.
        let mut turned = circle(0.0, 0.0, 0.0);
        for (slot, value) in turned[4..14]
            .iter_mut()
            .zip([0.0, -1.0, 1.0, 0.0, 3.0, 0.0, -2.0, -0.5, 2.0, 0.5])
        {
            *slot = f32::to_bits(value);
        }
        assert_eq!(record_bounds(&turned), [2.5, -2.0, 3.5, 2.0]);
    }
}

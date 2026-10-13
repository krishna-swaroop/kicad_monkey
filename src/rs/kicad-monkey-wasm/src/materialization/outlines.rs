//! Closed polygons around shape records, for a renderer extruding a slice
//! into a slab: each encloses its shape's outline (the zero level of the
//! record's signed distance) within `tolerance` millimetres, so a thin skirt
//! extruded from it holds the shape's side wall, which the renderer then
//! finds exactly by the distance. Board millimetres, through the record's
//! affine.

use super::lower::{FLAG_CHAMFER_SHIFT, kind};
use std::f32::consts::{FRAC_PI_2, PI, TAU};

type Point = (f32, f32);

fn word(record: &[u32], index: usize) -> f32 {
    f32::from_bits(record[index])
}

/// Segments for an arc of `sweep` radians and `radius` whose circumscribed
/// polygon stays within `tolerance` of it.
fn segments(radius: f32, sweep: f32, tolerance: f32) -> usize {
    if radius <= tolerance {
        return ((sweep.abs() / FRAC_PI_2).ceil() as usize).max(1);
    }
    let step = 2.0 * (1.0 - tolerance / radius).clamp(-1.0, 1.0).acos();
    ((sweep.abs() / step.max(1e-3)).ceil() as usize).clamp(1, 256)
}

/// Points along an arc from `start` through `sweep` radians, on the polygon
/// circumscribed about it: its chords touch the arc, its points lie a little
/// outside.
fn arc_out(
    out: &mut Vec<Point>,
    (cx, cy): Point,
    radius: f32,
    start: f32,
    sweep: f32,
    tolerance: f32,
) {
    let n = segments(radius, sweep, tolerance);
    let step = sweep / n as f32;
    let reach = radius / (step / 2.0).cos();
    for k in 0..=n {
        let a = start + step * k as f32;
        out.push((cx + reach * a.cos(), cy + reach * a.sin()));
    }
}

/// Points along an arc on the arc itself: its chords pass inside it (for the
/// inner side of an arc track, which is outside the track).
fn arc_on(
    out: &mut Vec<Point>,
    (cx, cy): Point,
    radius: f32,
    start: f32,
    sweep: f32,
    tolerance: f32,
) {
    let n = segments(radius, sweep, tolerance);
    for k in 0..=n {
        let a = start + sweep * k as f32 / n as f32;
        out.push((cx + radius * a.cos(), cy + radius * a.sin()));
    }
}

/// Move a convex polygon's edges out by `offset` (its points to where the
/// moved edges meet), so it encloses the shape grown by `offset`.
fn inflate(points: &[Point], offset: f32) -> Vec<Point> {
    if offset <= 0.0 || points.len() < 3 {
        return points.to_vec();
    }
    let area: f32 = (0..points.len())
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % points.len()]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    let outward = if area > 0.0 { 1.0 } else { -1.0 };
    let normal = |a: Point, b: Point| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx.hypot(dy).max(1e-12);
        (outward * dy / length, -outward * dx / length)
    };
    let n = points.len();
    (0..n)
        .map(|i| {
            let (prev, here, next) = (points[(i + n - 1) % n], points[i], points[(i + 1) % n]);
            let (a, b) = (normal(prev, here), normal(here, next));
            let scale = offset / (1.0 + a.0 * b.0 + a.1 * b.1).max(0.1);
            (here.0 + (a.0 + b.0) * scale, here.1 + (a.1 + b.1) * scale)
        })
        .collect()
}

/// A rounded, chamfered pad box about `centre` (KiCad corners: top left 1,
/// top right 2, bottom left 4, bottom right 8; y down).
fn pad_box(
    centre: Point,
    half: Point,
    radius: f32,
    chamfer: f32,
    corners: u32,
    tolerance: f32,
) -> Vec<Point> {
    let r = radius.min(half.0).min(half.1).max(0.0);
    let mut out = Vec::new();
    // Corners in turn: bottom right, bottom left, top left, top right.
    for (sx, sy, bit, start) in [
        (1.0, 1.0, 8, 0.0),
        (-1.0, 1.0, 4, FRAC_PI_2),
        (-1.0, -1.0, 1, PI),
        (1.0, -1.0, 2, 1.5 * PI),
    ] {
        let corner = (centre.0 + sx * half.0, centre.1 + sy * half.1);
        if corners & bit != 0 && chamfer > 0.0 {
            // The cut runs between the points `chamfer` in from the corner.
            let along_y = (corner.0, corner.1 - sy * chamfer);
            let along_x = (corner.0 - sx * chamfer, corner.1);
            if sx * sy > 0.0 {
                out.extend([along_y, along_x]);
            } else {
                out.extend([along_x, along_y]);
            }
        } else if r > 0.0 {
            arc_out(
                &mut out,
                (corner.0 - sx * r, corner.1 - sy * r),
                r,
                start,
                FRAC_PI_2,
                tolerance,
            );
        } else {
            out.push(corner);
        }
    }
    out
}

/// The polygon enclosing a shape record's outline within `tolerance`, in
/// board millimetres. An arc track's inner side follows its arc's chords.
pub fn outline(record: &[u32], tolerance: f32) -> Vec<Point> {
    let p = |index: usize| word(record, 14 + index);
    let grow = word(record, 3).max(0.0);
    let local = match record[0] {
        kind::CIRCLE => {
            let mut out = Vec::new();
            arc_out(&mut out, (p(0), p(1)), p(2) + grow, 0.0, TAU, tolerance);
            out.pop();
            out
        }
        kind::BOX => {
            let corners = (record[2] >> FLAG_CHAMFER_SHIFT) & 15;
            inflate(
                &pad_box((p(0), p(1)), (p(2), p(3)), p(4), p(5), corners, tolerance),
                grow,
            )
        }
        kind::TRAPEZOID => {
            let (x, y, hw, hh, dx, dy) = (p(0), p(1), p(2), p(3), p(4), p(5));
            let corners = [
                (-hw + dy, -hh - dx),
                (hw - dy, -hh + dx),
                (hw + dy, hh - dx),
                (-hw - dy, hh + dx),
            ];
            inflate(&corners.map(|(cx, cy)| (x + cx, y + cy)), grow)
        }
        kind::CAPSULE | kind::FLAT_SEGMENT => {
            let (a, b, r) = ((p(0), p(1)), (p(2), p(3)), p(4) + grow);
            let angle = (b.1 - a.1).atan2(b.0 - a.0);
            let mut out = Vec::new();
            if record[0] == kind::CAPSULE {
                arc_out(&mut out, b, r, angle - FRAC_PI_2, PI, tolerance);
                arc_out(&mut out, a, r, angle + FRAC_PI_2, PI, tolerance);
            } else {
                let (nx, ny) = (-angle.sin() * r, angle.cos() * r);
                out.extend([
                    (b.0 - nx, b.1 - ny),
                    (b.0 + nx, b.1 + ny),
                    (a.0 + nx, a.1 + ny),
                    (a.0 - nx, a.1 - ny),
                ]);
            }
            out
        }
        kind::ARC => {
            let (c, radius, half) = ((p(0), p(1)), p(2), p(5) + grow);
            let (mut start, mut sweep) = (p(3), p(4));
            if sweep < 0.0 {
                start += sweep;
                sweep = -sweep;
            }
            let end = start + sweep;
            let at = |a: f32| (c.0 + radius * a.cos(), c.1 + radius * a.sin());
            let mut out = Vec::new();
            arc_out(&mut out, c, radius + half, start, sweep, tolerance);
            arc_out(&mut out, at(end), half, end, PI, tolerance);
            if radius > half {
                arc_on(&mut out, c, radius - half, end, -sweep, tolerance);
            } else {
                out.push(c);
            }
            arc_out(&mut out, at(start), half, start + PI, PI, tolerance);
            out
        }
        _ => Vec::new(),
    };
    let f = |index: usize| word(record, index);
    let (m11, m12, m21, m22, tx, ty) = (f(4), f(5), f(6), f(7), f(8), f(9));
    local
        .into_iter()
        .map(|(x, y)| (m11 * x + m12 * y + tx, m21 * x + m22 * y + ty))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materialization::lower::SHAPE_WORDS;

    fn record(shape_kind: u32, flags: u32, params: [f32; 6]) -> [u32; SHAPE_WORDS] {
        let mut words = [0u32; SHAPE_WORDS];
        words[0] = shape_kind;
        words[2] = flags;
        for (slot, value) in words[4..10]
            .iter_mut()
            .zip([1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0])
        {
            *slot = value.to_bits();
        }
        for (slot, value) in words[14..20].iter_mut().zip(params) {
            *slot = value.to_bits();
        }
        words
    }

    /// The shader's distances for the kinds tested, in the local frame.
    fn distance(r: &[u32], (x, y): Point) -> f32 {
        let p = |index: usize| word(r, 14 + index);
        match r[0] {
            kind::CIRCLE => (x - p(0)).hypot(y - p(1)) - p(2),
            kind::CAPSULE => {
                let (ax, ay, bx, by) = (p(0), p(1), p(2), p(3));
                let (dx, dy) = (bx - ax, by - ay);
                let h = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                (x - ax - dx * h).hypot(y - ay - dy * h) - p(4)
            }
            kind::ARC => {
                let (cx, cy, radius, mut start, mut sweep, half) =
                    (p(0), p(1), p(2), p(3), p(4), p(5));
                if sweep < 0.0 {
                    start += sweep;
                    sweep = -sweep;
                }
                let angle = (y - cy).atan2(x - cx);
                let t = (angle - start).rem_euclid(TAU);
                if t <= sweep {
                    ((x - cx).hypot(y - cy) - radius).abs() - half
                } else {
                    let end = |a: f32| (x - cx - radius * a.cos()).hypot(y - cy - radius * a.sin());
                    end(start).min(end(start + sweep)) - half
                }
            }
            _ => unreachable!(),
        }
    }

    /// Every outline point lies outside its shape, within a little over the
    /// tolerance, and the outline's edges never cross into the shape.
    fn encloses(r: &[u32], tolerance: f32) {
        let points = outline(r, tolerance);
        assert!(points.len() >= 3);
        for (i, &a) in points.iter().enumerate() {
            let b = points[(i + 1) % points.len()];
            for k in 0..=8 {
                let t = k as f32 / 8.0;
                let d = distance(r, (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
                assert!(d >= -1e-5, "edge {i} dips {d} into the shape");
            }
            assert!(
                distance(r, a) <= tolerance * 1.5 + 1e-5,
                "point {i} strays {}",
                distance(r, a)
            );
        }
    }

    #[test]
    fn outlines_hug_their_shapes_from_outside() {
        let tolerance = 0.001;
        encloses(
            &record(kind::CIRCLE, 0, [1.0, 2.0, 0.3, 0.0, 0.0, 0.0]),
            tolerance,
        );
        encloses(
            &record(kind::CAPSULE, 0, [0.0, 0.0, 2.0, 1.0, 0.1, 0.0]),
            tolerance,
        );
        // An arc track's inner side runs on chords, which lie outside the track.
        encloses(
            &record(kind::ARC, 0, [0.0, 0.0, 2.0, 0.3, 2.0, 0.15]),
            tolerance,
        );
        encloses(
            &record(kind::ARC, 0, [0.0, 0.0, 2.0, 2.3, -2.0, 0.15]),
            tolerance,
        );
    }

    #[test]
    fn boxes_keep_their_rounding_chamfers_and_growth() {
        let rounded = outline(&record(kind::BOX, 0, [0.0, 0.0, 1.0, 0.5, 0.2, 0.0]), 0.001);
        assert!(
            rounded
                .iter()
                .all(|&(x, y)| x.abs() <= 1.001 && y.abs() <= 0.501)
        );
        assert!(rounded.len() > 8);
        // A chamfered top left corner: no point near (-1, -0.5).
        let chamfered = outline(
            &record(
                kind::BOX,
                1 << FLAG_CHAMFER_SHIFT,
                [0.0, 0.0, 1.0, 0.5, 0.0, 0.2],
            ),
            0.001,
        );
        assert_eq!(chamfered.len(), 5);
        assert!(
            chamfered
                .iter()
                .all(|&(x, y)| (x + 1.0).hypot(y + 0.5) > 0.1)
        );
        let mut grown = record(kind::BOX, 0, [0.0, 0.0, 1.0, 0.5, 0.0, 0.0]);
        grown[3] = 0.1f32.to_bits();
        let grown = outline(&grown, 0.001);
        assert!(
            grown
                .iter()
                .all(|&(x, y)| (x.abs() - 1.1).abs() < 1e-5 && (y.abs() - 0.6).abs() < 1e-5)
        );
    }
}

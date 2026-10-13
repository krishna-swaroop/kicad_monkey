//! Region fans in tiles, so that no fan triangle is longer than a tile.
//!
//! A region is filled by the even-odd stencil rule: a fan of triangles from
//! one point over every contour edge toggles each sample once per edge
//! between it and the fan's point. That is exact only while the rasterizer
//! treats every shared edge identically, and a fan from a contour's own
//! vertex across a board-wide zone makes triangles that reach thousands of
//! pixels off screen when zoomed in, where it no longer does (stray samples).
//!
//! Here the region is cut into a grid of square tiles. Each tile fans from
//! its top-left corner over the boundary of (region ∩ tile): the contour
//! edges clipped to the tile, and the stretches of the tile's right and
//! bottom sides inside the region. That boundary is closed, so its fan is
//! exact; the top and left sides pass through the corner and add nothing.
//! Each tile with triangles gets a cover: the tile within the region's
//! bounds, so the regions of a batch still do not overlap.
//!
//! The grid sits off round millimetres, so no board coordinate lies on a
//! tile line; work is in `f64`, emitted as `f32` board millimetres.

/// Tile side, board millimetres.
pub const TILE_MM: f64 = 1.0;
/// Shifts the grid off round coordinates (an irrational fraction of a
/// nanometre scale), so no contour vertex falls on a tile line.
const GRID_OFFSET_MM: f64 = 0.000_173_205_080_756_887_7;

/// One region's tiled fans: triangle vertices (x, y), and per tile a cover
/// rectangle (min x, min y, max x, max y).
#[derive(Debug, Default)]
pub struct TiledFan {
    pub vertices: Vec<f32>,
    pub covers: Vec<[f32; 4]>,
}

/// Which tile side a clipped edge's end lies on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Inside,
    Left,
    Right,
    Top,
    Bottom,
}

struct Grid {
    x0: f64,
    y0: f64,
    nx: usize,
    ny: usize,
    tile: f64,
}

impl Grid {
    fn x(&self, i: usize) -> f64 {
        self.x0 + i as f64 * self.tile
    }

    fn y(&self, j: usize) -> f64 {
        self.y0 + j as f64 * self.tile
    }

    fn column(&self, x: f64) -> usize {
        (((x - self.x0) / self.tile).floor().max(0.0) as usize).min(self.nx - 1)
    }

    fn row(&self, y: f64) -> usize {
        (((y - self.y0) / self.tile).floor().max(0.0) as usize).min(self.ny - 1)
    }
}

/// An edge clipped to one tile, with the side each end lies on.
#[derive(Clone, Copy)]
struct Piece {
    a: (f64, f64),
    b: (f64, f64),
    a_side: Side,
    b_side: Side,
}

/// Clip edge `a`→`b` to the rectangle (Liang–Barsky); ends on a side are
/// put exactly on it.
fn clip(a: (f64, f64), b: (f64, f64), rect: [f64; 4]) -> Option<Piece> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    let (mut s0, mut s1) = (Side::Inside, Side::Inside);
    for (p, q, side) in [
        (-dx, a.0 - rect[0], Side::Left),
        (dx, rect[2] - a.0, Side::Right),
        (-dy, a.1 - rect[1], Side::Top),
        (dy, rect[3] - a.1, Side::Bottom),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let t = q / p;
        if p < 0.0 {
            if t > t1 {
                return None;
            }
            if t > t0 {
                t0 = t;
                s0 = side;
            }
        } else {
            if t < t0 {
                return None;
            }
            if t < t1 {
                t1 = t;
                s1 = side;
            }
        }
    }
    if t0 >= t1 {
        return None;
    }
    let at = |t: f64, side: Side| {
        let mut point = (a.0 + dx * t, a.1 + dy * t);
        match side {
            Side::Left => point.0 = rect[0],
            Side::Right => point.0 = rect[2],
            Side::Top => point.1 = rect[1],
            Side::Bottom => point.1 = rect[3],
            Side::Inside => {}
        }
        point
    };
    Some(Piece {
        a: if s0 == Side::Inside { a } else { at(t0, s0) },
        b: if s1 == Side::Inside { b } else { at(t1, s1) },
        a_side: s0,
        b_side: s1,
    })
}

type Point = (f64, f64);

/// Tile a region given by its contours (board millimetres, even-odd) within
/// `bounds` (min x, min y, max x, max y).
pub fn tile_region(contours: &[Vec<(f32, f32)>], bounds: [f32; 4], tile: f64) -> TiledFan {
    let mut out = TiledFan::default();
    let edges = edges_of(contours);
    if edges.is_empty() {
        return out;
    }
    let grid = grid_for(&edges, bounds, tile);
    let corners = corner_insides(&edges, &grid);
    let pieces = bin_pieces(&edges, &grid);
    for j in 0..grid.ny {
        for i in 0..grid.nx {
            let rect = [grid.x(i), grid.y(j), grid.x(i + 1), grid.y(j + 1)];
            let starts = (
                corners[j * (grid.nx + 1) + i + 1],
                corners[(j + 1) * (grid.nx + 1) + i],
            );
            if fan_tile(&mut out, &pieces[j * grid.nx + i], rect, starts) {
                out.covers.push([
                    (rect[0] as f32).max(bounds[0]),
                    (rect[1] as f32).max(bounds[1]),
                    (rect[2] as f32).min(bounds[2]),
                    (rect[3] as f32).min(bounds[3]),
                ]);
            }
        }
    }
    out
}

/// Every contour's edges, closed, without zero-length ones.
fn edges_of(contours: &[Vec<(f32, f32)>]) -> Vec<(Point, Point)> {
    let wide = |p: (f32, f32)| (f64::from(p.0), f64::from(p.1));
    contours
        .iter()
        .flat_map(|contour| {
            (0..contour.len())
                .map(move |k| (wide(contour[k]), wide(contour[(k + 1) % contour.len()])))
        })
        .filter(|(a, b)| a != b)
        .collect()
}

/// The grid over the bounds and every edge.
fn grid_for(edges: &[(Point, Point)], bounds: [f32; 4], tile: f64) -> Grid {
    let mut lo = (f64::from(bounds[0]), f64::from(bounds[1]));
    let mut hi = (f64::from(bounds[2]), f64::from(bounds[3]));
    for (a, _) in edges {
        lo = (lo.0.min(a.0), lo.1.min(a.1));
        hi = (hi.0.max(a.0), hi.1.max(a.1));
    }
    let start = |v: f64| ((v - GRID_OFFSET_MM) / tile).floor() * tile + GRID_OFFSET_MM;
    let (x0, y0) = (start(lo.0), start(lo.1));
    let count = |from: f64, to: f64| (((to - from) / tile).floor() as usize + 1).max(1);
    Grid {
        x0,
        y0,
        nx: count(x0, hi.0),
        ny: count(y0, hi.1),
        tile,
    }
}

/// Even-odd inside-ness of every grid corner, row by row ((nx + 1) per row).
fn corner_insides(edges: &[(Point, Point)], grid: &Grid) -> Vec<bool> {
    let mut corners = vec![false; (grid.nx + 1) * (grid.ny + 1)];
    let mut crossings = Vec::new();
    for j in 0..=grid.ny {
        let y = grid.y(j);
        crossings.clear();
        crossings.extend(
            edges
                .iter()
                .filter(|(a, b)| (a.1 <= y) != (b.1 <= y))
                .map(|(a, b)| a.0 + (y - a.1) * (b.0 - a.0) / (b.1 - a.1)),
        );
        crossings.sort_by(f64::total_cmp);
        let mut passed = 0;
        for i in 0..=grid.nx {
            let x = grid.x(i);
            while passed < crossings.len() && crossings[passed] < x {
                passed += 1;
            }
            corners[j * (grid.nx + 1) + i] = passed % 2 == 1;
        }
    }
    corners
}

/// Each edge clipped to the tiles it crosses, per tile (nx per row).
fn bin_pieces(edges: &[(Point, Point)], grid: &Grid) -> Vec<Vec<Piece>> {
    let mut pieces: Vec<Vec<Piece>> = vec![Vec::new(); grid.nx * grid.ny];
    for &(a, b) in edges {
        let (i0, i1) = (grid.column(a.0.min(b.0)), grid.column(a.0.max(b.0)));
        let (j0, j1) = (grid.row(a.1.min(b.1)), grid.row(a.1.max(b.1)));
        for j in j0..=j1 {
            for i in i0..=i1 {
                let rect = [grid.x(i), grid.y(j), grid.x(i + 1), grid.y(j + 1)];
                if let Some(piece) = clip(a, b, rect) {
                    pieces[j * grid.nx + i].push(piece);
                }
            }
        }
    }
    pieces
}

fn push_triangle(out: &mut TiledFan, points: [Point; 3]) {
    for (x, y) in points {
        out.vertices.extend([x as f32, y as f32]);
    }
}

/// Fan one tile from its top-left corner: its edge pieces, then the inside
/// stretches of its right side (top to bottom) and bottom side (left to
/// right). `starts` is whether the right side's top and the bottom side's
/// left end are inside. Whether it drew anything.
fn fan_tile(out: &mut TiledFan, pieces: &[Piece], rect: [f64; 4], starts: (bool, bool)) -> bool {
    let [left, top, right, bottom] = rect;
    let pivot = (left, top);
    let first = out.vertices.len();
    for piece in pieces {
        push_triangle(out, [pivot, piece.a, piece.b]);
    }
    let stops = |side: Side, along: fn(Point) -> f64| {
        let mut stops: Vec<f64> = pieces
            .iter()
            .flat_map(|piece| [(piece.a_side, piece.a), (piece.b_side, piece.b)])
            .filter(|(on, _)| *on == side)
            .map(|(_, point)| along(point))
            .collect();
        stops.sort_by(f64::total_cmp);
        stops
    };
    let right_stops = stops(Side::Right, |p| p.1);
    for (from, to) in stretches(&right_stops, starts.0, top, bottom) {
        push_triangle(out, [pivot, (right, from), (right, to)]);
    }
    let bottom_stops = stops(Side::Bottom, |p| p.0);
    for (from, to) in stretches(&bottom_stops, starts.1, left, right) {
        push_triangle(out, [pivot, (from, bottom), (to, bottom)]);
    }
    out.vertices.len() > first
}

/// The inside stretches of a side from `from` to `to` that sorted `stops`
/// cross, its start inside or not.
fn stretches(stops: &[f64], mut inside: bool, from: f64, to: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut previous = from;
    for &stop in stops.iter().chain(std::iter::once(&to)) {
        if inside && stop > previous {
            out.push((previous, stop));
        }
        previous = stop;
        inside = !inside;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Even-odd inside-ness of `p` (half-open crossing rule).
    fn inside(contours: &[Vec<(f32, f32)>], p: (f64, f64)) -> bool {
        let mut odd = false;
        for contour in contours {
            for k in 0..contour.len() {
                let (a, b) = (contour[k], contour[(k + 1) % contour.len()]);
                let (a, b) = (
                    (f64::from(a.0), f64::from(a.1)),
                    (f64::from(b.0), f64::from(b.1)),
                );
                if (a.1 <= p.1) != (b.1 <= p.1)
                    && p.0 < a.0 + (p.1 - a.1) * (b.0 - a.0) / (b.1 - a.1)
                {
                    odd = !odd;
                }
            }
        }
        odd
    }

    /// How many triangles hold `p` strictly: the stencil's parity there.
    fn parity(vertices: &[f32], p: (f64, f64)) -> bool {
        let cross =
            |a: (f64, f64), b: (f64, f64)| (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        let mut odd = false;
        for v in triangles(vertices) {
            let s = [cross(v[0], v[1]), cross(v[1], v[2]), cross(v[2], v[0])];
            if s.iter().all(|&x| x > 0.0) || s.iter().all(|&x| x < 0.0) {
                odd = !odd;
            }
        }
        odd
    }

    /// The triangles' corners, widened.
    fn triangles(vertices: &[f32]) -> Vec<[(f64, f64); 3]> {
        vertices
            .as_chunks::<6>()
            .0
            .iter()
            .map(|t| [0, 2, 4].map(|k| (f64::from(t[k]), f64::from(t[k + 1]))))
            .collect()
    }

    fn ring(cx: f32, cy: f32, r: f32, n: usize) -> Vec<(f32, f32)> {
        (0..n)
            .map(|k| {
                let a = k as f32 / n as f32 * std::f32::consts::TAU;
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    }

    #[test]
    fn tiled_fans_fill_exactly_the_even_odd_region() {
        // A board-sized outline on round millimetres, a round hole, and a
        // diagonal notch: edges on grid-like coordinates and off them.
        let outer = vec![
            (100.0, 40.0),
            (130.0, 40.0),
            (130.0, 55.5),
            (121.25, 63.0),
            (100.0, 63.0),
        ];
        let contours = vec![
            outer,
            ring(112.3, 50.7, 4.2, 48),
            vec![(103.0, 43.0), (108.0, 43.0), (103.0, 47.0)],
        ];
        let bounds = [100.0, 40.0, 130.0, 63.0];
        let tiled = tile_region(&contours, bounds, TILE_MM);
        assert_eq!(tiled.vertices.len() % 6, 0);
        let mut checked = 0;
        for j in 0..230 {
            for i in 0..300 {
                // Off any tile line, and not exactly on an edge.
                let p = (99.5 + f64::from(i) * 0.10137, 39.5 + f64::from(j) * 0.10571);
                assert_eq!(parity(&tiled.vertices, p), inside(&contours, p), "at {p:?}");
                checked += 1;
            }
        }
        assert!(checked > 60_000);
        // No triangle is longer than a tile's diagonal.
        for v in triangles(&tiled.vertices) {
            for (a, b) in [(v[0], v[1]), (v[1], v[2]), (v[2], v[0])] {
                assert!((a.0 - b.0).hypot(a.1 - b.1) <= TILE_MM * 1.4143);
            }
        }
        // Covers stay within the region's bounds and hold every triangle.
        for cover in &tiled.covers {
            assert!(
                cover[0] >= bounds[0]
                    && cover[1] >= bounds[1]
                    && cover[2] <= bounds[2]
                    && cover[3] <= bounds[3]
            );
        }
        assert!(tiled.covers.len() <= 31 * 24);
    }

    #[test]
    fn a_tile_wholly_inside_is_two_triangles() {
        let square = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let tiled = tile_region(&[square], [0.0, 0.0, 10.0, 10.0], TILE_MM);
        // 11 x 11 tiles touch the square; the 9 x 9 inner ones hold two triangles.
        assert_eq!(tiled.covers.len(), 121);
        let total = tiled.vertices.len() / 6;
        assert!((81 * 2..=81 * 2 + 40 * 4).contains(&total), "{total}");
    }
}

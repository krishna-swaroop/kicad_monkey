//! Lowering of one stack slice into GPU-ready records for an analytic
//! (signed-distance) renderer.
//!
//! Closed shapes stay analytic: each becomes one instanced record that a
//! fragment shader evaluates exactly in the shape's local frame. Only curve
//! segments the shader has no distance function for are flattened: cubic
//! segments of swept paths, and every segment of planar regions, which are
//! filled by the even-odd stencil rule, in tiles (see `tiles`) so that no
//! fan triangle is long enough to break the rasterizer's exactness. Regions
//! are batched so that no two regions of one batch overlap, which keeps the
//! even-odd fill of a batch equal to the union of its regions.
//!
//! Lengths are board millimetres (`f32`) in KiCad's frame, X right and Y
//! down. A hole whose span crosses the slice is lowered as a subtract.

use super::cuts::{cuts, record_bounds};
use super::outlines::outline;
use super::tiles::{TILE_MM, tile_region};
use kicad_monkey_contracts::generated::materialization_document::{
    Affine2D, Contour, HoleOperation, MaterialAction, MaterialOperation, NmBounds, NmPoint,
    PathCap, PathSegment, RectCorner, Shape2d, SurfaceOperation,
};

/// Words per shape record.
pub const SHAPE_WORDS: usize = 20;
/// Words per region cover record (one per tile a region fills): operation,
/// flags, then the tile within the region's bounds.
pub const COVER_WORDS: usize = 6;
/// Words per region batch: subtract flag, first fan vertex, fan vertex
/// count, first cover, cover count.
pub const BATCH_WORDS: usize = 5;

/// Shape record kinds, read by the shader.
pub mod kind {
    pub const CIRCLE: u32 = 1;
    pub const BOX: u32 = 2;
    pub const TRAPEZOID: u32 = 3;
    pub const CAPSULE: u32 = 4;
    pub const FLAT_SEGMENT: u32 = 5;
    pub const ARC: u32 = 6;
}

/// Contour edge flag: the edge bounds a subtracting region.
pub const EDGE_SUBTRACT: u32 = 1 << 31;

/// Record flag: the operation removes material.
pub const FLAG_SUBTRACT: u32 = 1;
/// Record flags of chamfered corners, from bit 8: top left, top right,
/// bottom left, bottom right (Y down).
pub const FLAG_CHAMFER_SHIFT: u32 = 8;

const NM_PER_MM: f64 = 1_000_000.0;

fn mm(nm: f64) -> f32 {
    (nm / NM_PER_MM) as f32
}

fn point(value: &NmPoint) -> (f64, f64) {
    let coordinate = |index: usize| value.0.get(index).map_or(0.0, |n| f64::from(n.0));
    (coordinate(0), coordinate(1))
}

/// Counts of material the lowering could not draw exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LowerStats {
    /// Regions whose offset was ignored (the stencil fill has none).
    pub region_offsets_ignored: u32,
    /// Flat-capped path segments drawn without their joins.
    pub flat_cap_segments: u32,
    /// Cubic segments flattened at the tolerance.
    pub cubics_flattened: u32,
}

/// One slice lowered for drawing. Subtract shapes come first.
#[derive(Clone, Debug, Default)]
pub struct LoweredSlice {
    pub shapes: Vec<u32>,
    pub subtract_shapes: u32,
    pub fan_vertices: Vec<f32>,
    pub covers: Vec<u32>,
    pub batches: Vec<u32>,
    /// Every region contour's edges, closed, as (x0, y0, x1, y1) board
    /// millimetres: where an extruded slice has side walls. Each runs with
    /// its region's material on its left (x right, y up as written): the
    /// outer contour turns counter-clockwise, holes clockwise.
    pub contour_edges: Vec<f32>,
    /// Per contour edge, its region's operation index, with
    /// `EDGE_SUBTRACT` set for a subtracting region.
    pub contour_edge_operations: Vec<u32>,
    /// Per add shape, then per contour edge, a (first, count) range into
    /// `cut_indices`: the holes (subtract shape records) whose bounds reach it,
    /// where a slab's side walls are cut (see `cuts`).
    pub shape_cuts: Vec<u32>,
    pub edge_cuts: Vec<u32>,
    pub cut_indices: Vec<u32>,
    /// The add shapes' outlines as closed edges (x0, y0, x1, y1), each
    /// enclosing its shape within the tolerance (see `outlines`) and running
    /// with the shape on its left, and per edge its add shape's index among
    /// the adds.
    pub outline_edges: Vec<f32>,
    pub outline_shapes: Vec<u32>,
    pub stats: LowerStats,
}

/// Placement and identity shared by every record of one operation.
#[derive(Clone, Copy)]
struct Item<'a> {
    index: u32,
    subtract: bool,
    affine: &'a Affine2D,
    offset_mm: f32,
}

impl Item<'_> {
    fn flags(&self) -> u32 {
        u32::from(self.subtract) * FLAG_SUBTRACT
    }

    /// Local point to board millimetres.
    fn place(&self, (x, y): (f64, f64)) -> (f32, f32) {
        let a = self.affine;
        let bx = a.m11 * x + a.m12 * y + f64::from(a.tx_nm.0);
        let by = a.m21 * x + a.m22 * y + f64::from(a.ty_nm.0);
        (mm(bx), mm(by))
    }
}

/// A region's flattened contours in board millimetres.
struct Region {
    index: u32,
    subtract: bool,
    contours: Vec<Vec<(f32, f32)>>,
    bounds: [f32; 4],
}

#[derive(Default)]
struct Builder {
    adds: Vec<u32>,
    subtracts: Vec<u32>,
    regions: Vec<Region>,
    stats: LowerStats,
    tolerance_nm: f64,
}

impl Builder {
    fn record(
        &mut self,
        item: &Item<'_>,
        shape_kind: u32,
        extra_flags: u32,
        bounds: [f64; 4],
        params: [f64; 6],
    ) {
        let a = item.affine;
        let grow = f64::from(item.offset_mm.max(0.0)) * NM_PER_MM;
        let mut words = [0u32; SHAPE_WORDS];
        words[0] = shape_kind;
        words[1] = item.index;
        words[2] = item.flags() | extra_flags;
        words[3] = item.offset_mm.to_bits();
        let floats = [
            a.m11 as f32,
            a.m12 as f32,
            a.m21 as f32,
            a.m22 as f32,
            mm(f64::from(a.tx_nm.0)),
            mm(f64::from(a.ty_nm.0)),
            mm(bounds[0] - grow),
            mm(bounds[1] - grow),
            mm(bounds[2] + grow),
            mm(bounds[3] + grow),
        ];
        for (slot, value) in words[4..14].iter_mut().zip(floats) {
            *slot = value.to_bits();
        }
        for (slot, value) in words[14..20].iter_mut().zip(params) {
            *slot = mm(value).to_bits();
        }
        let target = if item.subtract {
            &mut self.subtracts
        } else {
            &mut self.adds
        };
        target.extend_from_slice(&words);
    }

    fn circle(&mut self, item: &Item<'_>, (x, y): (f64, f64), outer: f64, inner: f64) {
        let bounds = [x - outer, y - outer, x + outer, y + outer];
        self.record(
            item,
            kind::CIRCLE,
            0,
            bounds,
            [x, y, outer, inner, 0.0, 0.0],
        );
    }

    fn rect(
        &mut self,
        item: &Item<'_>,
        center: (f64, f64),
        size: (f64, f64),
        radius: f64,
        chamfer: (f64, u32),
    ) {
        let (x, y) = center;
        let (hw, hh) = (size.0 / 2.0, size.1 / 2.0);
        let bounds = [x - hw, y - hh, x + hw, y + hh];
        let flags = chamfer.1 << FLAG_CHAMFER_SHIFT;
        self.record(
            item,
            kind::BOX,
            flags,
            bounds,
            [x, y, hw, hh, radius, chamfer.0],
        );
    }

    fn capsule(&mut self, item: &Item<'_>, a: (f64, f64), b: (f64, f64), radius: f64, flat: bool) {
        let bounds = [
            a.0.min(b.0) - radius,
            a.1.min(b.1) - radius,
            a.0.max(b.0) + radius,
            a.1.max(b.1) + radius,
        ];
        let shape_kind = if flat {
            kind::FLAT_SEGMENT
        } else {
            kind::CAPSULE
        };
        self.record(
            item,
            shape_kind,
            0,
            bounds,
            [a.0, a.1, b.0, b.1, radius, 0.0],
        );
    }

    fn arc(&mut self, item: &Item<'_>, arc: &Arc, half_width: f64) {
        let reach = arc.radius + half_width;
        let (cx, cy) = arc.center;
        let bounds = [cx - reach, cy - reach, cx + reach, cy + reach];
        let params = [cx, cy, arc.radius, arc.start, arc.sweep, half_width];
        // Angles are radians, not lengths: store them unscaled.
        self.record(item, kind::ARC, 0, bounds, params);
        let target = if item.subtract {
            &mut self.subtracts
        } else {
            &mut self.adds
        };
        let base = target.len() - SHAPE_WORDS;
        target[base + 17] = (arc.start as f32).to_bits();
        target[base + 18] = (arc.sweep as f32).to_bits();
    }
}

/// A circular arc through three points.
struct Arc {
    center: (f64, f64),
    radius: f64,
    start: f64,
    sweep: f64,
}

fn arc_through(a: (f64, f64), m: (f64, f64), b: (f64, f64)) -> Option<Arc> {
    let d = 2.0 * (a.0 * (m.1 - b.1) + m.0 * (b.1 - a.1) + b.0 * (a.1 - m.1));
    if d.abs() < 1e-9 {
        return None;
    }
    let sq = |p: (f64, f64)| p.0 * p.0 + p.1 * p.1;
    let cx = (sq(a) * (m.1 - b.1) + sq(m) * (b.1 - a.1) + sq(b) * (a.1 - m.1)) / d;
    let cy = (sq(a) * (b.0 - m.0) + sq(m) * (a.0 - b.0) + sq(b) * (m.0 - a.0)) / d;
    let angle = |p: (f64, f64)| (p.1 - cy).atan2(p.0 - cx);
    let turn = |value: f64| value.rem_euclid(std::f64::consts::TAU);
    let (start, to_mid, to_end) = (
        angle(a),
        turn(angle(m) - angle(a)),
        turn(angle(b) - angle(a)),
    );
    let sweep = if to_mid < to_end {
        to_end
    } else {
        to_end - std::f64::consts::TAU
    };
    Some(Arc {
        center: (cx, cy),
        radius: (a.0 - cx).hypot(a.1 - cy),
        start,
        sweep,
    })
}

impl Arc {
    fn point(&self, angle: f64) -> (f64, f64) {
        (
            self.center.0 + self.radius * angle.cos(),
            self.center.1 + self.radius * angle.sin(),
        )
    }

    /// Points after the start, within `tolerance` of the arc.
    fn flatten(&self, tolerance: f64) -> Vec<(f64, f64)> {
        let ratio = (1.0 - tolerance / self.radius.max(tolerance)).clamp(-1.0, 1.0);
        let step = (2.0 * ratio.acos()).max(1e-3);
        let count = (self.sweep.abs() / step).ceil().clamp(1.0, 4096.0) as usize;
        (1..=count)
            .map(|i| self.point(self.start + self.sweep * i as f64 / count as f64))
            .collect()
    }
}

/// Points after `p0` along a cubic, within about `tolerance` of it.
fn flatten_cubic(p: [(f64, f64); 4], tolerance: f64) -> Vec<(f64, f64)> {
    let second = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
        (a.0 - 2.0 * b.0 + c.0).hypot(a.1 - 2.0 * b.1 + c.1)
    };
    let bend = second(p[0], p[1], p[2]).max(second(p[1], p[2], p[3]));
    let count = (0.75 * bend / tolerance.max(1.0))
        .sqrt()
        .ceil()
        .clamp(1.0, 4096.0) as usize;
    (1..=count)
        .map(|i| {
            let t = i as f64 / count as f64;
            let u = 1.0 - t;
            let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            (
                w0 * p[0].0 + w1 * p[1].0 + w2 * p[2].0 + w3 * p[3].0,
                w0 * p[0].1 + w1 * p[1].1 + w2 * p[2].1 + w3 * p[3].1,
            )
        })
        .collect()
}

/// Local points of a closed contour, flattened.
fn contour_points(contour: &Contour, tolerance: f64, stats: &mut LowerStats) -> Vec<(f64, f64)> {
    let mut points = vec![point(&contour.start_nm)];
    for segment in &contour.segments {
        let last = points[points.len() - 1];
        match segment {
            PathSegment::LineSegment(line) => points.push(point(&line.end_nm)),
            PathSegment::ArcSegment(arc) => {
                let (mid, end) = (point(&arc.mid_nm), point(&arc.end_nm));
                match arc_through(last, mid, end) {
                    Some(arc) => points.extend(arc.flatten(tolerance)),
                    None => points.push(end),
                }
            }
            PathSegment::CubicSegment(cubic) => {
                stats.cubics_flattened += 1;
                let control = [
                    last,
                    point(&cubic.control1_nm),
                    point(&cubic.control2_nm),
                    point(&cubic.end_nm),
                ];
                points.extend(flatten_cubic(control, tolerance));
            }
        }
    }
    points
}

fn corner_bits(corners: &[RectCorner]) -> u32 {
    corners
        .iter()
        .map(|corner| match corner {
            RectCorner::TopLeft => 1,
            RectCorner::TopRight => 2,
            RectCorner::BottomLeft => 4,
            RectCorner::BottomRight => 8,
        })
        .fold(0, |bits, bit| bits | bit)
}

impl Builder {
    fn shape(&mut self, item: &Item<'_>, shape: &Shape2d, bounds: &NmBounds) {
        match shape {
            Shape2d::Disk2d(disk) => {
                self.circle(item, point(&disk.center_nm), *disk.radius_nm as f64, 0.0)
            }
            Shape2d::Annulus2d(ring) => self.circle(
                item,
                point(&ring.center_nm),
                *ring.outer_radius_nm as f64,
                *ring.inner_radius_nm as f64,
            ),
            Shape2d::Rect2d(rect) => {
                let size = (*rect.width_nm as f64, *rect.height_nm as f64);
                self.rect(item, point(&rect.center_nm), size, 0.0, (0.0, 0));
            }
            Shape2d::RoundedRect2d(rect) => {
                let size = (*rect.width_nm as f64, *rect.height_nm as f64);
                self.rect(
                    item,
                    point(&rect.center_nm),
                    size,
                    *rect.radius_nm as f64,
                    (0.0, 0),
                );
            }
            Shape2d::ChamferedRect2d(rect) => {
                let size = (*rect.width_nm as f64, *rect.height_nm as f64);
                let chamfer = (
                    *rect.chamfer_nm as f64,
                    corner_bits(&rect.chamfered_corners),
                );
                self.rect(
                    item,
                    point(&rect.center_nm),
                    size,
                    *rect.radius_nm as f64,
                    chamfer,
                );
            }
            Shape2d::Trapezoid2d(trapezoid) => self.trapezoid(item, trapezoid),
            Shape2d::Capsule2d(capsule) => self.capsule(
                item,
                point(&capsule.start_nm),
                point(&capsule.end_nm),
                *capsule.radius_nm as f64,
                false,
            ),
            Shape2d::SweptPath2d(path) => {
                let flat = path.cap == PathCap::Flat;
                self.path(
                    item,
                    &path.start_nm,
                    &path.segments,
                    *path.width_nm as f64 / 2.0,
                    flat,
                );
            }
            Shape2d::PlanarRegion2d(region) => self.region(item, region, bounds),
        }
    }

    fn trapezoid(
        &mut self,
        item: &Item<'_>,
        trapezoid: &kicad_monkey_contracts::generated::materialization_document::Trapezoid2d,
    ) {
        let (x, y) = point(&trapezoid.center_nm);
        let (hw, hh) = (
            *trapezoid.width_nm as f64 / 2.0,
            *trapezoid.height_nm as f64 / 2.0,
        );
        // KiCad's corners use half of each delta.
        let (dx, dy) = (
            f64::from(trapezoid.delta_x_nm.0) / 2.0,
            f64::from(trapezoid.delta_y_nm.0) / 2.0,
        );
        let (reach_x, reach_y) = (hw + dy.abs(), hh + dx.abs());
        let bounds = [x - reach_x, y - reach_y, x + reach_x, y + reach_y];
        self.record(item, kind::TRAPEZOID, 0, bounds, [x, y, hw, hh, dx, dy]);
    }

    fn path(
        &mut self,
        item: &Item<'_>,
        start: &NmPoint,
        segments: &[PathSegment],
        half: f64,
        flat: bool,
    ) {
        let mut last = point(start);
        for segment in segments {
            match segment {
                PathSegment::LineSegment(line) => {
                    let end = point(&line.end_nm);
                    self.stats.flat_cap_segments += u32::from(flat);
                    self.capsule(item, last, end, half, flat);
                    last = end;
                }
                PathSegment::ArcSegment(arc) => {
                    let (mid, end) = (point(&arc.mid_nm), point(&arc.end_nm));
                    match arc_through(last, mid, end) {
                        Some(arc) => self.arc(item, &arc, half),
                        None => self.capsule(item, last, end, half, flat),
                    }
                    last = end;
                }
                PathSegment::CubicSegment(cubic) => {
                    self.stats.cubics_flattened += 1;
                    let control = [
                        last,
                        point(&cubic.control1_nm),
                        point(&cubic.control2_nm),
                        point(&cubic.end_nm),
                    ];
                    for next in flatten_cubic(control, self.tolerance_nm) {
                        self.capsule(item, last, next, half, flat);
                        last = next;
                    }
                }
            }
        }
    }

    fn region(
        &mut self,
        item: &Item<'_>,
        region: &kicad_monkey_contracts::generated::materialization_document::PlanarRegion2d,
        bounds: &NmBounds,
    ) {
        if item.offset_mm != 0.0 {
            self.stats.region_offsets_ignored += 1;
        }
        let tolerance = self.tolerance_nm;
        let stats = &mut self.stats;
        let contours = std::iter::once(&region.outer)
            .chain(&region.holes)
            .map(|contour| {
                contour_points(contour, tolerance, stats)
                    .into_iter()
                    .map(|p| item.place(p))
                    .collect::<Vec<_>>()
            })
            .filter(|points| points.len() >= 3)
            .collect();
        self.regions.push(Region {
            index: item.index,
            subtract: item.subtract,
            contours,
            bounds: [
                mm(f64::from(bounds.min_x_nm.0)),
                mm(f64::from(bounds.min_y_nm.0)),
                mm(f64::from(bounds.max_x_nm.0)),
                mm(f64::from(bounds.max_y_nm.0)),
            ],
        });
    }
}

fn overlaps(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// Group regions into batches of one action whose regions do not overlap,
/// subtract batches first, in first-fit order.
fn batch_regions(regions: &[Region]) -> Vec<(bool, Vec<usize>)> {
    let mut batches: Vec<(bool, Vec<usize>)> = Vec::new();
    for subtract in [true, false] {
        let first = batches.len();
        for (position, region) in regions
            .iter()
            .enumerate()
            .filter(|(_, r)| r.subtract == subtract)
        {
            let fits = |members: &Vec<usize>| {
                members
                    .iter()
                    .all(|other| !overlaps(&regions[*other].bounds, &region.bounds))
            };
            match batches[first..]
                .iter_mut()
                .find(|(_, members)| fits(members))
            {
                Some((_, members)) => members.push(position),
                None => batches.push((subtract, vec![position])),
            }
        }
    }
    batches
}

impl Builder {
    fn finish(self) -> LoweredSlice {
        let mut out = LoweredSlice {
            subtract_shapes: u32::try_from(self.subtracts.len() / SHAPE_WORDS).unwrap_or(u32::MAX),
            stats: self.stats,
            ..LoweredSlice::default()
        };
        out.shapes = self.subtracts;
        out.shapes.extend(self.adds);
        for region in &self.regions {
            let operation = region.index | (u32::from(region.subtract) * EDGE_SUBTRACT);
            for (number, contour) in region.contours.iter().enumerate() {
                // The outer contour (the first) counter-clockwise, holes clockwise.
                for (x0, y0, x1, y1) in oriented_edges(contour, number == 0) {
                    out.contour_edges.extend([x0, y0, x1, y1]);
                    out.contour_edge_operations.push(operation);
                }
            }
        }
        let (subtracts, adds) = out
            .shapes
            .split_at(out.subtract_shapes as usize * SHAPE_WORDS);
        let shapes = adds
            .as_chunks::<SHAPE_WORDS>()
            .0
            .iter()
            .map(|record| record_bounds(record));
        let edges = out.contour_edges.as_chunks::<4>().0.iter().map(|e| {
            [
                e[0].min(e[2]),
                e[1].min(e[3]),
                e[0].max(e[2]),
                e[1].max(e[3]),
            ]
        });
        let shape_count = adds.len() / SHAPE_WORDS;
        let found = cuts(subtracts, shapes.chain(edges));
        out.shape_cuts = found.ranges[..shape_count * 2].to_vec();
        out.edge_cuts = found.ranges[shape_count * 2..].to_vec();
        out.cut_indices = found.indices;
        let tolerance = (self.tolerance_nm / NM_PER_MM) as f32;
        for (index, record) in adds.as_chunks::<SHAPE_WORDS>().0.iter().enumerate() {
            for (x0, y0, x1, y1) in oriented_edges(&outline(record, tolerance), true) {
                out.outline_edges.extend([x0, y0, x1, y1]);
                out.outline_shapes.push(index as u32);
            }
        }
        for (subtract, members) in batch_regions(&self.regions) {
            let first_vertex = out.fan_vertices.len() / 2;
            let first_cover = out.covers.len() / COVER_WORDS;
            for position in &members {
                let region = &self.regions[*position];
                let tiled = tile_region(&region.contours, region.bounds, TILE_MM);
                out.fan_vertices.extend(tiled.vertices);
                for cover in tiled.covers {
                    out.covers
                        .extend([region.index, u32::from(region.subtract) * FLAG_SUBTRACT]);
                    out.covers.extend(cover.map(f32::to_bits));
                }
            }
            let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
            out.batches.extend([
                u32::from(subtract),
                count(first_vertex),
                count(out.fan_vertices.len() / 2 - first_vertex),
                count(first_cover),
                count(out.covers.len() / COVER_WORDS - first_cover),
            ]);
        }
        out
    }
}

/// A closed polygon's edges, turning counter-clockwise (x right, y up as
/// written) when `counter_clockwise`, else clockwise.
fn oriented_edges(points: &[(f32, f32)], counter_clockwise: bool) -> Vec<(f32, f32, f32, f32)> {
    let n = points.len();
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    let forward = (area > 0.0) == counter_clockwise;
    (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            if forward {
                (a.0, a.1, b.0, b.1)
            } else {
                (b.0, b.1, a.0, a.1)
            }
        })
        .collect()
}

/// Whether the hole crosses the slice at `order`, given each slice key's
/// order and the orders of the outermost conductors: its copper span, and
/// past an outer conductor it reaches every slice on that side (mask, paste,
/// silkscreen), since the drill leaves the board there.
fn hole_crosses(
    hole: &HoleOperation,
    order: u32,
    orders: &impl Fn(&str) -> Option<u32>,
    outer: Option<(u32, u32)>,
) -> bool {
    match (orders(&hole.start_slice_key), orders(&hole.end_slice_key)) {
        (Some(a), Some(b)) => {
            let (mut low, mut high) = (a.min(b), a.max(b));
            if let Some((first, last)) = outer {
                if low <= first {
                    low = 0;
                }
                if high >= last {
                    high = u32::MAX;
                }
            }
            low <= order && order <= high
        }
        _ => false,
    }
}

/// Lower the surface operations of one slice, and as subtracts the holes
/// crossing it. `outer` is the orders of the outermost conductors (see
/// `hole_crosses`). `tolerance_nm` bounds the flattening error.
pub fn lower_slice<'a>(
    surfaces: impl IntoIterator<Item = &'a MaterialOperation>,
    holes: &[HoleOperation],
    slice: (&str, u32),
    orders: &impl Fn(&str) -> Option<u32>,
    outer: Option<(u32, u32)>,
    tolerance_nm: f64,
) -> LoweredSlice {
    let mut builder = Builder {
        tolerance_nm: tolerance_nm.max(1.0),
        ..Builder::default()
    };
    for operation in surfaces {
        if let MaterialOperation::SurfaceOperation(surface) = operation
            && surface.slice_key == slice.0
        {
            builder.surface(surface);
        }
    }
    for hole in holes
        .iter()
        .filter(|hole| hole_crosses(hole, slice.1, orders, outer))
    {
        let item = Item {
            index: hole.index,
            subtract: true,
            affine: &hole.affine,
            offset_mm: 0.0,
        };
        builder.shape(&item, &hole.shape, &hole.bounds);
    }
    builder.finish()
}

impl Builder {
    fn surface(&mut self, surface: &SurfaceOperation) {
        let item = Item {
            index: surface.index,
            subtract: surface.action == MaterialAction::Subtract,
            affine: &surface.affine,
            offset_mm: mm(surface.offset_nm.as_ref().map_or(0.0, |nm| f64::from(nm.0))),
        };
        self.shape(&item, &surface.shape, &surface.bounds);
    }
}

#[cfg(test)]
#[path = "lower_tests.rs"]
mod tests;

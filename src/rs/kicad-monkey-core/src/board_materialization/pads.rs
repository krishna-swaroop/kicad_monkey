//! Pad lands in the pad's local frame under one composed affine.
//!
//! The pad anchor is the footprint-local pad position rotated by the
//! footprint angle and translated to the footprint origin. The pad angle is
//! already a board-frame angle, so the land affine rotates by the pad angle
//! alone (matching the KiCad-Prism producer this was ported from).

use super::emit::{Emitter, Origin};
use super::geometry::{nm, origin, pcb_point, rotate_mm, rotation_translation};
use super::routing::polyline_contour;
use crate::pcb::{
    PcbPad, PcbPadCustomPrimitive, PcbPadPrimitiveGeometry, PcbPoint, PcbPolygonPoint,
    PcbResolvedPadCopperLayer,
};
use kicad_monkey_contracts::generated::materialization_document::{
    Affine2D, Annulus2d, ArcSegment, Capsule2d, ChamferedRect2d, Contour, CubicSegment, Disk2d,
    LineSegment, Nm, NmPoint, NonNegativeNm, PathCap, PathJoin, PathSegment, PlanarRegion2d,
    Rect2d, RectCorner, RoundedRect2d, Shape2d, SweptPath2d, Trapezoid2d,
};

fn len(value_mm: f64) -> NonNegativeNm {
    NonNegativeNm(i64::from(nm(value_mm)).max(0))
}

fn at(x_mm: f64, y_mm: f64) -> NmPoint {
    NmPoint([Nm(nm(x_mm)), Nm(nm(y_mm))])
}

/// Board-frame placement of a pad-local point.
pub(crate) struct PadFrame {
    pub anchor_x: f64,
    pub anchor_y: f64,
    pub angle: f64,
}

impl PadFrame {
    pub(crate) fn new(
        pad: &PcbPad,
        footprint_x: f64,
        footprint_y: f64,
        footprint_angle: f64,
    ) -> Self {
        let (dx, dy) = rotate_mm(pad.at_x, pad.at_y, -footprint_angle);
        Self {
            anchor_x: footprint_x + dx,
            anchor_y: footprint_y + dy,
            angle: -pad.angle,
        }
    }

    /// Affine placing pad-local geometry offset by (`offset_x`, `offset_y`).
    pub(crate) fn affine(&self, offset_x: f64, offset_y: f64) -> Affine2D {
        let (dx, dy) = rotate_mm(offset_x, offset_y, self.angle);
        rotation_translation(self.angle, self.anchor_x + dx, self.anchor_y + dy)
    }
}

/// Emit the copper land of `pad` on one resolved layer.
pub(crate) fn land(
    resolved: &PcbResolvedPadCopperLayer<'_>,
    frame: &PadFrame,
    origin: &Origin,
    slice: &str,
    emitter: &mut Emitter,
) {
    let affine = frame.affine(resolved.offset.x, resolved.offset.y);
    if resolved.shape == "custom" {
        custom_land(resolved, &affine, origin, slice, emitter);
        return;
    }
    match standard_shape(resolved) {
        Some(shape) => emitter.copper(origin, slice, shape, affine),
        None => emitter.warn(
            "unsupported_pad_shape",
            format!(
                "pad shape {} on {slice} is not materialized",
                resolved.shape
            ),
            Some(origin.uid.clone()),
        ),
    }
}

fn standard_shape(resolved: &PcbResolvedPadCopperLayer<'_>) -> Option<Shape2d> {
    let (width, height) = (resolved.size.x, resolved.size.y);
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let short = width.min(height);
    let chamfered = resolved.chamfer_ratio > 0.0 && !resolved.chamfer_corners.is_empty();
    match resolved.shape {
        "roundrect" if chamfered => Some(chamfered_rect(resolved, width, height)),
        "circle" => Some(disk(width / 2.0)),
        "oval" => Some(oval(width, height)),
        "rect" => Some(Shape2d::Rect2d(Rect2d {
            kind: "rect_2d".to_owned(),
            center_nm: origin(),
            width_nm: len(width),
            height_nm: len(height),
        })),
        "roundrect" => Some(Shape2d::RoundedRect2d(RoundedRect2d {
            kind: "rounded_rect_2d".to_owned(),
            center_nm: origin(),
            width_nm: len(width),
            height_nm: len(height),
            radius_nm: len((short * resolved.roundrect_rratio).clamp(0.0, short / 2.0)),
        })),
        "trapezoid" => Some(Shape2d::Trapezoid2d(Trapezoid2d {
            kind: "trapezoid_2d".to_owned(),
            center_nm: origin(),
            width_nm: len(width),
            height_nm: len(height),
            delta_x_nm: Nm(nm(resolved.rect_delta.x)),
            delta_y_nm: Nm(nm(resolved.rect_delta.y)),
        })),
        _ => None,
    }
}

fn disk(radius_mm: f64) -> Shape2d {
    Shape2d::Disk2d(Disk2d {
        kind: "disk_2d".to_owned(),
        center_nm: origin(),
        radius_nm: len(radius_mm),
    })
}

/// Stadium along the longer axis.
pub(crate) fn oval(width: f64, height: f64) -> Shape2d {
    let (half_span, radius) = ((width - height).abs() / 2.0, width.min(height) / 2.0);
    let (start, end) = if width >= height {
        (at(-half_span, 0.0), at(half_span, 0.0))
    } else {
        (at(0.0, -half_span), at(0.0, half_span))
    };
    Shape2d::Capsule2d(Capsule2d {
        kind: "capsule_2d".to_owned(),
        start_nm: start,
        end_nm: end,
        radius_nm: len(radius),
    })
}

fn corner(name: &str) -> Option<RectCorner> {
    match name {
        "top_left" => Some(RectCorner::TopLeft),
        "top_right" => Some(RectCorner::TopRight),
        "bottom_left" => Some(RectCorner::BottomLeft),
        "bottom_right" => Some(RectCorner::BottomRight),
        _ => None,
    }
}

fn chamfered_rect(resolved: &PcbResolvedPadCopperLayer<'_>, width: f64, height: f64) -> Shape2d {
    let short = width.min(height);
    Shape2d::ChamferedRect2d(ChamferedRect2d {
        kind: "chamfered_rect_2d".to_owned(),
        center_nm: origin(),
        width_nm: len(width),
        height_nm: len(height),
        chamfer_nm: len((short * resolved.chamfer_ratio).clamp(0.0, short / 2.0)),
        chamfered_corners: resolved
            .chamfer_corners
            .iter()
            .filter_map(|name| corner(name))
            .collect(),
        radius_nm: len((short * resolved.roundrect_rratio).clamp(0.0, short / 2.0)),
    })
}

/// A custom pad is its anchor shape plus each primitive, all in the pad frame.
fn custom_land(
    resolved: &PcbResolvedPadCopperLayer<'_>,
    affine: &Affine2D,
    origin: &Origin,
    slice: &str,
    emitter: &mut Emitter,
) {
    let (width, height) = (resolved.size.x, resolved.size.y);
    if width > 0.0 && height > 0.0 {
        let anchor = if resolved.anchor == "rect" {
            Shape2d::Rect2d(Rect2d {
                kind: "rect_2d".to_owned(),
                center_nm: self::origin(),
                width_nm: len(width),
                height_nm: len(height),
            })
        } else {
            disk(width.min(height) / 2.0)
        };
        emitter.copper(origin, slice, anchor, affine.clone());
    }
    for primitive in resolved.custom_primitives {
        match primitive_shape(primitive) {
            Some(shape) => emitter.copper(origin, slice, shape, affine.clone()),
            None => emitter.warn(
                "unsupported_custom_pad_primitive",
                format!(
                    "custom pad primitive {} is not materialized",
                    primitive.kind
                ),
                Some(origin.uid.clone()),
            ),
        }
    }
}

fn stroke(start: PcbPoint, segment: PathSegment, width: f64) -> Shape2d {
    Shape2d::SweptPath2d(SweptPath2d {
        kind: "swept_path_2d".to_owned(),
        start_nm: pcb_point(start),
        segments: vec![segment],
        width_nm: len(width),
        cap: PathCap::Round,
        join: PathJoin::Round,
    })
}

fn primitive_shape(primitive: &PcbPadCustomPrimitive) -> Option<Shape2d> {
    let width = primitive.width.unwrap_or(0.0);
    match primitive.geometry.as_ref()? {
        PcbPadPrimitiveGeometry::Line { start, end } if width > 0.0 => {
            Some(stroke(*start, line_to(*end), width))
        }
        PcbPadPrimitiveGeometry::Arc { start, mid, end } if width > 0.0 => {
            Some(stroke(*start, arc_to(*mid, *end), width))
        }
        PcbPadPrimitiveGeometry::Circle { center, end } => circle(
            *center,
            *end,
            width,
            primitive.fill.as_deref() != Some("none"),
        ),
        PcbPadPrimitiveGeometry::Rect { start, end, radius } => rect(*start, *end, *radius),
        PcbPadPrimitiveGeometry::Polygon { points } => polygon(points),
        PcbPadPrimitiveGeometry::Curve { points } if width > 0.0 => Some(bezier(*points, width)),
        _ => None,
    }
}

fn line_to(end: PcbPoint) -> PathSegment {
    PathSegment::LineSegment(LineSegment {
        kind: "line".to_owned(),
        end_nm: pcb_point(end),
    })
}

fn arc_to(mid: PcbPoint, end: PcbPoint) -> PathSegment {
    PathSegment::ArcSegment(ArcSegment {
        kind: "arc".to_owned(),
        mid_nm: pcb_point(mid),
        end_nm: pcb_point(end),
    })
}

fn circle(center: PcbPoint, end: PcbPoint, width: f64, filled: bool) -> Option<Shape2d> {
    let radius = (end.x - center.x).hypot(end.y - center.y);
    let outer = radius + width.max(0.0) / 2.0;
    if outer <= 0.0 {
        return None;
    }
    if filled || width <= 0.0 || radius <= width / 2.0 {
        return Some(Shape2d::Disk2d(Disk2d {
            kind: "disk_2d".to_owned(),
            center_nm: pcb_point(center),
            radius_nm: len(outer),
        }));
    }
    Some(Shape2d::Annulus2d(Annulus2d {
        kind: "annulus_2d".to_owned(),
        center_nm: pcb_point(center),
        outer_radius_nm: len(outer),
        inner_radius_nm: len(radius - width / 2.0),
    }))
}

fn rect(start: PcbPoint, end: PcbPoint, radius: Option<f64>) -> Option<Shape2d> {
    let (width, height) = ((end.x - start.x).abs(), (end.y - start.y).abs());
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let center = at((start.x + end.x) / 2.0, (start.y + end.y) / 2.0);
    let radius = radius.unwrap_or(0.0).clamp(0.0, width.min(height) / 2.0);
    if radius <= 0.0 {
        return Some(Shape2d::Rect2d(Rect2d {
            kind: "rect_2d".to_owned(),
            center_nm: center,
            width_nm: len(width),
            height_nm: len(height),
        }));
    }
    Some(Shape2d::RoundedRect2d(RoundedRect2d {
        kind: "rounded_rect_2d".to_owned(),
        center_nm: center,
        width_nm: len(width),
        height_nm: len(height),
        radius_nm: len(radius),
    }))
}

/// Polygon with any authored arcs kept as arc segments.
fn polygon(points: &[PcbPolygonPoint]) -> Option<Shape2d> {
    let contour = if points
        .iter()
        .any(|point| matches!(point, PcbPolygonPoint::Arc { .. }))
    {
        arc_contour(points)?
    } else {
        let xy = points
            .iter()
            .filter_map(|point| match point {
                PcbPolygonPoint::Xy(value) => Some(pcb_point(*value)),
                PcbPolygonPoint::Arc { .. } => None,
            })
            .collect();
        polyline_contour(xy)?
    };
    Some(Shape2d::PlanarRegion2d(PlanarRegion2d {
        kind: "planar_region_2d".to_owned(),
        outer: contour,
        holes: Vec::new(),
    }))
}

fn arc_contour(points: &[PcbPolygonPoint]) -> Option<Contour> {
    let mut start: Option<NmPoint> = None;
    let mut segments = Vec::new();
    for point in points {
        match point {
            PcbPolygonPoint::Xy(value) if start.is_none() => start = Some(pcb_point(*value)),
            PcbPolygonPoint::Xy(value) => segments.push(line_to(*value)),
            PcbPolygonPoint::Arc {
                start: from,
                mid,
                end,
            } => {
                if start.is_none() {
                    start = Some(pcb_point(*from));
                } else {
                    segments.push(line_to(*from));
                }
                segments.push(arc_to(*mid, *end));
            }
        }
    }
    (!segments.is_empty()).then_some(Contour {
        start_nm: start?,
        segments,
    })
}

/// A cubic Bézier stroke as one swept path with a single cubic segment.
fn bezier(points: [PcbPoint; 4], width: f64) -> Shape2d {
    Shape2d::SweptPath2d(SweptPath2d {
        kind: "swept_path_2d".to_owned(),
        start_nm: pcb_point(points[0]),
        segments: vec![PathSegment::CubicSegment(CubicSegment {
            kind: "cubic".to_owned(),
            control1_nm: pcb_point(points[1]),
            control2_nm: pcb_point(points[2]),
            end_nm: pcb_point(points[3]),
        })],
        width_nm: len(width),
        cap: PathCap::Round,
        join: PathJoin::Round,
    })
}

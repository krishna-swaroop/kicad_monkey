//! Board and footprint graphic shapes straight from the parsed source.
//!
//! The board plot document applies preview pen rules to footprint graphics
//! (a 0.1524 mm default and a 0.0847 mm minimum pen). Physical geometry
//! instead follows KiCad's `parsePCB_SHAPE`: a filled shape may have no
//! stroke, an unfilled shape with no stroke gets 0.10 mm, and authored
//! widths are kept as written. Text is not handled here.
//!
//! Graphics on copper layers are copper; see [`copper_graphics`].

use super::emit::{Emitter, Origin, source_uid};
use super::geometry::{identity, rotation_translation};
use super::graphics::{Drawn, circle, line_to, point, rect_shapes, swept};
use super::nets::net_key;
use super::pads::{polygon, reference};
use super::stack::CopperStack;
use crate::pcb::{PcbFootprint, PcbFootprintGraphic, PcbGraphic, PcbGraphicKind, PcbPoint};
use kicad_monkey_contracts::generated::materialization_document::{
    ArcSegment, BindingSourceKind, CubicSegment, PathSegment, Shape2d,
};

/// KiCad's `DEFAULT_LINE_WIDTH` for unfilled shapes authored without one.
const DEFAULT_LINE_WIDTH_NM: i64 = 100_000;

fn nm(value_mm: f64) -> i64 {
    (value_mm * 1_000_000.0).round() as i64
}

fn xy(value: PcbPoint) -> (i64, i64) {
    (nm(value.x), nm(value.y))
}

/// Concrete layers a graphic is drawn on.
pub(crate) fn graphic_layers(graphic: &PcbGraphic) -> Vec<&str> {
    match (&graphic.layers, &graphic.layer) {
        (Some(layers), _) if !layers.is_empty() => layers.iter().map(String::as_str).collect(),
        (_, Some(layer)) => vec![layer.as_str()],
        _ => Vec::new(),
    }
}

/// Fill and stroke of `graphic` on `layer`, as KiCad resolves them.
fn fill_and_stroke(graphic: &PcbGraphic, layer: &str, drawn: &mut Drawn) -> (bool, i64) {
    let solid = match graphic.effective_fill_for_layer(layer) {
        "none" | "no" => false,
        "hatch" | "reverse_hatch" | "cross_hatch" => {
            drawn.unsupported.push((
                "hatch_fill_not_materialized",
                "hatched fill is drawn as its outline only".to_owned(),
            ));
            false
        }
        _ => true,
    };
    let authored = nm(graphic.stroke_width.unwrap_or(0.0)).max(0);
    let stroke = if authored == 0 && !solid {
        DEFAULT_LINE_WIDTH_NM
    } else {
        authored
    };
    if !matches!(
        graphic.stroke_kind.as_deref(),
        None | Some("default" | "solid")
    ) {
        drawn.unsupported.push((
            "stroke_pattern_drawn_solid",
            format!(
                "{} stroke is drawn solid",
                graphic.stroke_kind.as_deref().unwrap_or_default()
            ),
        ));
    }
    (solid, stroke)
}

/// A closed stroke along the outer contour of a polygon region.
fn outline(region: &Shape2d, stroke: i64) -> Option<Shape2d> {
    let Shape2d::PlanarRegion2d(region) = region else {
        return None;
    };
    let start = region.outer.start_nm.clone();
    let (x, y) = (i64::from(*start.0[0]), i64::from(*start.0[1]));
    let mut segments = region.outer.segments.clone();
    segments.push(line_to(x, y));
    (stroke > 0).then(|| swept(start, segments, stroke))
}

/// Open strokes: lines, arcs, and cubic curves.
fn stroke_shape(graphic: &PcbGraphic, stroke: i64) -> Option<Shape2d> {
    let segment = match graphic.kind {
        PcbGraphicKind::Line => {
            let (ex, ey) = xy(graphic.end?);
            line_to(ex, ey)
        }
        PcbGraphicKind::Arc => {
            let ((mx, my), (ex, ey)) = (xy(graphic.mid?), xy(graphic.end?));
            PathSegment::ArcSegment(ArcSegment {
                kind: "arc".to_owned(),
                mid_nm: point(mx, my),
                end_nm: point(ex, ey),
            })
        }
        PcbGraphicKind::Curve => {
            let [_, p1, p2, p3] = graphic.points[..] else {
                return None;
            };
            let ((x1, y1), (x2, y2), (x3, y3)) = (xy(p1), xy(p2), xy(p3));
            PathSegment::CubicSegment(CubicSegment {
                kind: "cubic".to_owned(),
                control1_nm: point(x1, y1),
                control2_nm: point(x2, y2),
                end_nm: point(x3, y3),
            })
        }
        _ => return None,
    };
    let start = match graphic.kind {
        PcbGraphicKind::Curve => graphic.points.first().copied()?,
        _ => graphic.start?,
    };
    let (sx, sy) = xy(start);
    Some(swept(point(sx, sy), vec![segment], stroke))
}

/// Closed shapes: circles, rectangles, and polygons, filled or outlined.
fn closed_shapes(graphic: &PcbGraphic, solid: bool, stroke: i64) -> Vec<Shape2d> {
    match graphic.kind {
        PcbGraphicKind::Circle => {
            let (Some(center), Some(end)) = (graphic.center, graphic.end) else {
                return Vec::new();
            };
            let ((cx, cy), (ex, ey)) = (xy(center), xy(end));
            let radius = ((ex - cx) as f64).hypot((ey - cy) as f64).round() as i64;
            circle(cx, cy, radius, stroke, solid).into_iter().collect()
        }
        PcbGraphicKind::Rect => {
            let (Some(start), Some(end)) = (graphic.start, graphic.end) else {
                return Vec::new();
            };
            let ((x1, y1), (x2, y2)) = (xy(start), xy(end));
            let radius = nm(graphic.radius.unwrap_or(0.0));
            rect_shapes([x1, y1, x2, y2], radius, stroke, solid)
                .into_iter()
                .collect()
        }
        PcbGraphicKind::Poly => {
            let Some(region) = polygon(&graphic.polygon_points) else {
                return Vec::new();
            };
            let mut shapes: Vec<Shape2d> = outline(&region, stroke).into_iter().collect();
            if solid {
                shapes.push(region);
            }
            shapes
        }
        _ => Vec::new(),
    }
}

/// Shapes of `graphic` on `layer`, in the graphic's own frame.
pub(crate) fn graphic_shapes(graphic: &PcbGraphic, layer: &str, drawn: &mut Drawn) {
    let (solid, stroke) = fill_and_stroke(graphic, layer, drawn);
    drawn.shapes.extend(stroke_shape(graphic, stroke));
    drawn.shapes.extend(closed_shapes(graphic, solid, stroke));
}

/// Board and footprint graphics on copper layers, emitted as copper on the
/// graphic's net (footprint graphics and unconnected shapes use the empty
/// net).
pub(crate) fn copper_graphics(
    board: &[PcbGraphic],
    footprint: &[PcbFootprintGraphic],
    footprints: &[PcbFootprint],
    copper: &CopperStack,
    emitter: &mut Emitter,
) {
    let placed_board = board.iter().enumerate().map(|(position, graphic)| {
        let uid = source_uid("graphic", graphic.uuid.as_deref(), position);
        (
            graphic,
            BindingSourceKind::BoardGraphic,
            uid,
            None,
            identity(),
        )
    });
    let placed_footprint = footprint
        .iter()
        .enumerate()
        .filter_map(|(position, owned)| {
            let footprint = footprints.get(owned.footprint_index)?;
            let graphic = &owned.graphic;
            let uid = source_uid("footprint_graphic", graphic.uuid.as_deref(), position);
            let affine = rotation_translation(
                -footprint.angle.unwrap_or_default(),
                footprint.at_x.unwrap_or_default(),
                footprint.at_y.unwrap_or_default(),
            );
            let kind = BindingSourceKind::FootprintGraphic;
            Some((graphic, kind, uid, Some(reference(footprint)), affine))
        });
    for (graphic, kind, uid, refdes, affine) in placed_board.chain(placed_footprint) {
        for layer in graphic_layers(graphic) {
            let Some(slice) = copper.key(layer) else {
                continue;
            };
            let net = graphic.net.as_ref().map(net_key).unwrap_or_default();
            let mut origin = Origin::routed(kind, uid.clone(), net);
            origin.refdes.clone_from(&refdes);
            let mut drawn = Drawn::default();
            graphic_shapes(graphic, layer, &mut drawn);
            for (code, message) in drawn.unsupported {
                emitter.warn(code, message, Some(uid.clone()));
            }
            for shape in drawn.shapes {
                emitter.copper(&origin, slice, shape, affine.clone());
            }
        }
    }
}

//! Routed copper: tracks, track arcs, via lands, and stored zone fills.

use super::emit::{Emitter, Origin, source_uid};
use super::flash::FlashLayers;
use super::geometry::{circumcircle, identity, nm, nm_point, origin, pcb_point};
use super::nets::net_key;
use super::stack::CopperStack;
use crate::pcb::{PcbRoutingArc, PcbSegment, PcbVia, PcbZone};
use kicad_monkey_contracts::generated::materialization_document::{
    ArcSegment, BindingSourceKind, Contour, Disk2d, LineSegment, NmPoint, NonNegativeNm, PathCap,
    PathJoin, PathSegment, PlanarRegion2d, Shape2d, SweptPath2d,
};

fn swept(start: NmPoint, segment: PathSegment, width_mm: f64) -> Shape2d {
    Shape2d::SweptPath2d(SweptPath2d {
        kind: "swept_path_2d".to_owned(),
        start_nm: start,
        segments: vec![segment],
        width_nm: NonNegativeNm(i64::from(nm(width_mm))),
        cap: PathCap::Round,
        join: PathJoin::Round,
    })
}

fn line(end: NmPoint) -> PathSegment {
    PathSegment::LineSegment(LineSegment {
        kind: "line".to_owned(),
        end_nm: end,
    })
}

pub(crate) fn tracks(segments: &[PcbSegment], stack: &CopperStack, emitter: &mut Emitter) {
    for segment in segments {
        let uid = source_uid("track", segment.uuid.as_deref(), segment.source_range.start);
        let (Some(layer), Some(width)) = (segment.layer.as_deref(), segment.width) else {
            // KiCad writes exposed-mask tracks as `(layers "F.Cu" "F.Mask")`,
            // which the source model does not decode yet.
            emitter.warn(
                "track_layer_not_decoded",
                "track has no single decoded layer and is not materialized".to_owned(),
                Some(uid),
            );
            continue;
        };
        let Some(slice) = stack.key(layer) else {
            continue;
        };
        if width <= 0.0 {
            continue;
        }
        let origin = Origin::routed(BindingSourceKind::Track, uid, net_key(&segment.net));
        let shape = swept(
            nm_point(segment.start_x, segment.start_y),
            line(nm_point(segment.end_x, segment.end_y)),
            width,
        );
        emitter.copper(&origin, slice, shape, identity());
    }
}

pub(crate) fn track_arcs(arcs: &[PcbRoutingArc], stack: &CopperStack, emitter: &mut Emitter) {
    for arc in arcs {
        let (Some(layer), Some(width)) = (arc.layer.as_deref(), arc.width) else {
            emitter.warn(
                "track_layer_not_decoded",
                "track arc has no single decoded layer and is not materialized".to_owned(),
                Some(source_uid(
                    "track_arc",
                    arc.uuid.as_deref(),
                    arc.source_range.start,
                )),
            );
            continue;
        };
        let Some(slice) = stack.key(layer) else {
            continue;
        };
        if width <= 0.0 {
            continue;
        }
        let uid = source_uid("track_arc", arc.uuid.as_deref(), arc.source_range.start);
        let (start, mid, end) = (pcb_point(arc.start), pcb_point(arc.mid), pcb_point(arc.end));
        let segment = if circumcircle(&start, &mid, &end).is_some() {
            PathSegment::ArcSegment(ArcSegment {
                kind: "arc".to_owned(),
                mid_nm: mid,
                end_nm: end,
            })
        } else {
            emitter.warn(
                "degenerate_arc",
                "collinear routing arc materialized as a straight path".to_owned(),
                Some(uid.clone()),
            );
            line(end)
        };
        let origin = Origin::routed(BindingSourceKind::TrackArc, uid, net_key(&arc.net));
        emitter.copper(&origin, slice, swept(start, segment, width), identity());
    }
}

/// Land diameter of a via on `layer`, honouring a per-layer via padstack.
fn via_diameter(via: &PcbVia, layer: &str) -> f64 {
    let Some(stack) = via.padstack.as_ref() else {
        return via.size;
    };
    let inner =
        stack.mode.as_deref() == Some("front_inner_back") && !matches!(layer, "F.Cu" | "B.Cu");
    let selector = if inner { "Inner" } else { layer };
    stack
        .layers
        .iter()
        .find(|row| row.layer == selector)
        .and_then(|row| row.size)
        .unwrap_or(via.size)
}

pub(crate) fn via_lands(
    vias: &[PcbVia],
    stack: &CopperStack,
    flash: &FlashLayers,
    emitter: &mut Emitter,
) {
    for via in vias {
        let uid = source_uid("via", via.uuid.as_deref(), via.source_range.start);
        let layers = flash
            .vias
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| stack.expand(&via.layers));
        let source = Origin::routed(BindingSourceKind::Via, uid, net_key(&via.net));
        for layer in layers {
            let (Some(slice), diameter) = (stack.key(&layer), via_diameter(via, &layer)) else {
                continue;
            };
            if diameter <= 0.0 {
                continue;
            }
            let shape = Shape2d::Disk2d(Disk2d {
                kind: "disk_2d".to_owned(),
                center_nm: origin(),
                radius_nm: NonNegativeNm(i64::from(nm(diameter / 2.0))),
            });
            let affine = super::geometry::rotation_translation(0.0, via.at_x, via.at_y);
            emitter.copper(&source, slice, shape, affine);
        }
    }
}

/// Stored zone fills, one planar region per filled polygon.
pub(crate) fn zone_fills(zones: &[PcbZone], stack: &CopperStack, emitter: &mut Emitter) {
    for zone in zones {
        let uid = source_uid("zone", zone.uuid.as_deref(), zone.source_range.start);
        let origin = Origin::routed(BindingSourceKind::ZoneFill, uid, net_key(&zone.net));
        for polygon in &zone.filled_polygons {
            let layer = match (polygon.layer.is_empty(), zone.layers.as_slice()) {
                (true, [only]) => only.as_str(),
                _ => polygon.layer.as_str(),
            };
            let Some(slice) = stack.key(layer) else {
                continue;
            };
            let points: Vec<_> = polygon
                .points
                .iter()
                .map(|point| pcb_point(*point))
                .collect();
            let Some(contour) = polyline_contour(points) else {
                continue;
            };
            let shape = Shape2d::PlanarRegion2d(PlanarRegion2d {
                kind: "planar_region_2d".to_owned(),
                outer: contour,
                holes: Vec::new(),
            });
            emitter.copper(&origin, slice, shape, identity());
        }
    }
}

/// Closed contour of straight segments; consecutive duplicates and a
/// repeated closing point are dropped. `None` below three distinct points.
pub(crate) fn polyline_contour(mut points: Vec<NmPoint>) -> Option<Contour> {
    let raw = |point: &NmPoint| (*point.0[0], *point.0[1]);
    points.dedup_by(|a, b| raw(a) == raw(b));
    if points.len() > 1 && points.first().map(raw) == points.last().map(raw) {
        points.pop();
    }
    if points.len() < 3 {
        return None;
    }
    let mut iter = points.into_iter();
    let start = iter.next()?;
    Some(Contour {
        start_nm: start,
        segments: iter.map(line).collect(),
    })
}

use super::{
    BATCH_WORDS, COVER_WORDS, EDGE_SUBTRACT, FLAG_CHAMFER_SHIFT, FLAG_SUBTRACT, LoweredSlice,
    SHAPE_WORDS, arc_through, flatten_cubic, hole_crosses, kind,
};
use crate::materialization::AnalyticPack;
use kicad_monkey_core::board_materialization::materialize_board;
use serde_json::json;

/// A track, an arc, a via, pads of every standard shape, a zone fill with a
/// hole, a cubic graphic, and a chamfered pad.
const BOARD: &str = r#"(kicad_pcb
  (version 20241229)
  (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
  (net 0 "")
  (net 1 "GND")
  (gr_rect (start 0 0) (end 40 30) (stroke (width 0.1) (type solid)) (layer "Edge.Cuts"))
  (segment (start 2 2) (end 12 2) (width 0.5) (layer "F.Cu") (net 1) (uuid t1))
  (arc (start 2 6) (mid 7 4) (end 12 6) (width 0.4) (layer "F.Cu") (net 1) (uuid a1))
  (via (at 20 4) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (net 1) (uuid v1))
  (gr_curve (pts (xy 2 26) (xy 4 22) (xy 8 30) (xy 10 26)) (stroke (width 0.2) (type solid))
    (layer "F.Cu") (uuid c1))
  (footprint "Demo:U" (layer "F.Cu") (at 20 15)
    (pad "1" smd rect (at -6 0) (size 1 2) (layers "F.Cu") (net 1 "GND") (uuid p1))
    (pad "2" smd roundrect (at -4 0) (size 1 2) (roundrect_rratio 0.25) (layers "F.Cu")
      (net 1 "GND") (uuid p2))
    (pad "3" smd roundrect (at -2 0) (size 1 2) (chamfer_ratio 0.2) (chamfer top_left)
      (layers "F.Cu") (net 1 "GND") (uuid p3))
    (pad "4" smd trapezoid (at 0 0) (size 1 2) (rect_delta 0 0.4) (layers "F.Cu")
      (net 1 "GND") (uuid p4))
    (pad "5" thru_hole circle (at 3 0) (size 1.6 1.6) (drill 0.8) (layers "*.Cu")
      (net 1 "GND") (uuid p5)))
  (zone (net 1) (net_name "GND") (layer "F.Cu") (uuid z1) (hatch edge 0.5)
    (connect_pads (clearance 0.5)) (min_thickness 0.25) (filled_areas_thickness no)
    (fill yes (thermal_gap 0.5) (thermal_bridge_width 0.5))
    (polygon (pts (xy 25 20) (xy 38 20) (xy 38 28) (xy 25 28)))
    (filled_polygon (layer "F.Cu") (pts (xy 25 20) (xy 38 20) (xy 38 28) (xy 25 28)))
    (filled_polygon (layer "F.Cu") (pts (xy 30 22) (xy 33 22) (xy 33 25) (xy 30 25))))
)
"#;

fn pack() -> AnalyticPack {
    let request = serde_json::from_value(json!({
        "type": "kicad_monkey.materialization.request",
        "version": "a0",
        "products": ["copper", "holes"],
        "tile_size_nm": 10_000_000,
        "max_source_bytes": "1048576",
        "max_operations": 100_000,
        "max_points": 1_000_000
    }))
    .expect("request");
    let document = materialize_board(BOARD, &request).expect("materializes");
    let bytes = kicad_monkey_materialization_pack::encode_pack(&document).expect("packs");
    AnalyticPack::open(bytes).expect("opens")
}

fn records(words: &[u32]) -> Vec<&[u32; SHAPE_WORDS]> {
    words.as_chunks::<SHAPE_WORDS>().0.iter().collect()
}

fn float(word: u32) -> f32 {
    f32::from_bits(word)
}

/// The inner fill lies within the outer one's bounds, so each gets its own
/// batch; each batch's covers are the tiles its region fills.
fn assert_tiled_batches(front: &LoweredSlice) {
    assert_eq!(front.batches.len(), 2 * BATCH_WORDS);
    assert_eq!(front.fan_vertices.len() % 6, 0);
    let batches = front.batches.as_chunks::<BATCH_WORDS>().0;
    let covers = front.covers.as_chunks::<COVER_WORDS>().0;
    assert_eq!(
        batches.iter().map(|b| b[4] as usize).sum::<usize>(),
        covers.len()
    );
    for batch in batches {
        let region = &covers[batch[3] as usize..(batch[3] + batch[4]) as usize];
        assert!(
            region.iter().all(|c| c[0] == region[0][0]),
            "one region per batch here"
        );
        let tile =
            |c: &[u32; COVER_WORDS]| (float(c[4]) - float(c[2])).max(float(c[5]) - float(c[3]));
        assert!(region.iter().all(|c| tile(c) <= 1.0 + 1e-5));
    }
}

/// The via's and the pad's lands each list the hole through them, and every
/// listed cut is a subtract record.
fn assert_hole_cuts(front: &LoweredSlice, subtracts: usize, edges: usize) {
    let adds = records(&front.shapes).len() - subtracts;
    assert_eq!(front.shape_cuts.len(), adds * 2);
    assert_eq!(front.edge_cuts.len(), edges * 2);
    let ranges = front.shape_cuts.as_chunks::<2>().0;
    let lands = ranges.iter().filter(|range| range[1] > 0).count();
    assert!(lands >= 2, "{lands} shapes reach a hole");
    for range in ranges {
        let listed = &front.cut_indices[range[0] as usize..(range[0] + range[1]) as usize];
        assert!(listed.iter().all(|&hole| (hole as usize) < subtracts));
    }
}

#[test]
fn copper_lowers_to_analytic_records_with_holes_as_subtracts() {
    let pack = pack();
    let front = pack.lower("copper.front", 1_000.0).expect("lowers");
    let shapes = records(&front.shapes);
    let kinds: Vec<u32> = shapes.iter().map(|record| record[0]).collect();
    for expected in [
        kind::CIRCLE,
        kind::BOX,
        kind::TRAPEZOID,
        kind::CAPSULE,
        kind::ARC,
    ] {
        assert!(kinds.contains(&expected), "{expected} in {kinds:?}");
    }
    // The via and the pad hole cut front copper, first in the list.
    let subtracts = front.subtract_shapes as usize;
    assert_eq!(subtracts, 2);
    assert!(
        shapes[..subtracts]
            .iter()
            .all(|r| r[2] & FLAG_SUBTRACT != 0)
    );
    assert!(
        shapes[subtracts..]
            .iter()
            .all(|r| r[2] & FLAG_SUBTRACT == 0)
    );
    // The chamfered pad keeps its corner, and the trapezoid half its delta.
    assert!(
        shapes
            .iter()
            .any(|r| r[0] == kind::BOX && (r[2] >> FLAG_CHAMFER_SHIFT) == 1)
    );
    let trapezoid = shapes
        .iter()
        .find(|r| r[0] == kind::TRAPEZOID)
        .expect("trapezoid");
    assert!((float(trapezoid[19]) - 0.2).abs() < 1e-6, "dy/2 = 0.2 mm");
    // The cubic graphic is flattened into capsules.
    assert_eq!(front.stats.cubics_flattened, 1);
    assert_tiled_batches(&front);
    // Each contour closes: the edges' starts are their ends.
    let edges = front.contour_edges.as_chunks::<4>().0;
    // Each edge names its region's operation: the zone's two fills, adding.
    assert_eq!(front.contour_edge_operations.len(), edges.len());
    assert!(
        front
            .contour_edge_operations
            .iter()
            .all(|&op| op & EDGE_SUBTRACT == 0)
    );
    let mut fills = front.contour_edge_operations.clone();
    fills.dedup();
    assert_eq!(fills.len(), 2);
    let key = |x: f32, y: f32| (x.to_bits(), y.to_bits());
    let mut starts: Vec<_> = edges.iter().map(|e| key(e[0], e[1])).collect();
    let mut ends: Vec<_> = edges.iter().map(|e| key(e[2], e[3])).collect();
    starts.sort_unstable();
    ends.sort_unstable();
    assert_eq!(starts, ends);
    assert_hole_cuts(&front, subtracts, edges.len());
    // The back holds only the two through holes.
    let back = pack.lower("copper.back", 1_000.0).expect("lowers");
    assert_eq!(back.subtract_shapes, 2);
    assert!(pack.lower("no.such.slice", 1_000.0).is_err());
}

#[test]
fn records_carry_board_millimetres_and_operation_indices() {
    let pack = pack();
    let front = pack.lower("copper.front", 1_000.0).expect("lowers");
    let track = records(&front.shapes)
        .into_iter()
        .find(|r| {
            r[0] == kind::CAPSULE
                && pack
                    .binding(r[1])
                    .is_some_and(|entry| entry.source_uid == "t1")
        })
        .expect("track record");
    // Identity affine; endpoints in local millimetres; radius half the width.
    assert_eq!([float(track[4]), float(track[7])], [1.0, 1.0]);
    let params: Vec<f32> = track[14..19].iter().map(|w| float(*w)).collect();
    assert_eq!(params, [2.0, 2.0, 12.0, 2.0, 0.25]);
    assert!(pack.header_json().is_ok());
}

#[test]
fn arcs_follow_their_midpoint_and_cubics_flatten_within_tolerance() {
    let arc = arc_through((0.0, 0.0), (1.0, 1.0), (2.0, 0.0)).expect("arc");
    assert!((arc.radius - 1.0).abs() < 1e-12 && (arc.center.0 - 1.0).abs() < 1e-12);
    // In KiCad's Y-down frame the midpoint (1, 1) lies at +90°: sweep runs
    // clockwise from 180° through 90° to 0°.
    assert!((arc.sweep + std::f64::consts::PI).abs() < 1e-12);
    let flipped = arc_through((0.0, 0.0), (1.0, -1.0), (2.0, 0.0)).expect("arc");
    assert!((flipped.sweep - std::f64::consts::PI).abs() < 1e-12);
    assert!(arc_through((0.0, 0.0), (1.0, 0.0), (2.0, 0.0)).is_none());
    let points = flatten_cubic([(0.0, 0.0), (0.0, 1e6), (1e6, 1e6), (1e6, 0.0)], 1_000.0);
    assert!(points.len() > 4 && points.len() < 200, "{}", points.len());
    assert_eq!(points.last().copied(), Some((1e6, 0.0)));
}

#[test]
fn holes_through_an_outer_conductor_cut_the_layers_beyond_it() {
    // Mask 0, copper 1, core 2, copper 3, core 4, copper 5, mask 6.
    let orders = |key: &str| match key {
        "copper.front" => Some(1),
        "copper.in1" => Some(3),
        "copper.back" => Some(5),
        _ => None,
    };
    let hole = |start: &str, end: &str| {
        serde_json::from_value(json!({
            "kind": "hole", "index": 0,
            "affine": {"m11": 1.0, "m12": 0.0, "m21": 0.0, "m22": 1.0, "tx_nm": 0, "ty_nm": 0},
            "bounds": {"min_x_nm": 0, "min_y_nm": 0, "max_x_nm": 1, "max_y_nm": 1},
            "backdrills": [], "post_machining": [], "plating": "unplated",
            "start_slice_key": start, "end_slice_key": end,
            "shape": {"kind": "disk_2d", "center_nm": [0, 0], "radius_nm": 100_000}
        }))
        .expect("hole")
    };
    let crosses = |hole: &_, outer| {
        (0..7)
            .filter(|&order| hole_crosses(hole, order, &orders, outer))
            .collect::<Vec<_>>()
    };
    let through = hole("copper.front", "copper.back");
    assert_eq!(crosses(&through, Some((1, 5))), vec![0, 1, 2, 3, 4, 5, 6]);
    // A blind hole leaves only the side it opens on.
    let blind = hole("copper.front", "copper.in1");
    assert_eq!(crosses(&blind, Some((1, 5))), vec![0, 1, 2, 3]);
    // Without the outer conductors, only the copper span.
    assert_eq!(crosses(&through, None), vec![1, 2, 3, 4, 5]);
}

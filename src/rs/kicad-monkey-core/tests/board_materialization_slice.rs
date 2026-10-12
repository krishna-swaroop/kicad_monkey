//! Slice 0 evidence for `kicad_monkey.materialization.a0`: contract decode
//! and validation, debug-view determinism, the shared chunking rule, the
//! selective-equals-filtered oracle, and the request boundary.

use kicad_monkey_contracts::generated::materialization_document::{
    MaterialProduct, MaterializationDocumentA0, MaterializationScope, NonNegativeNm,
};
use kicad_monkey_contracts::generated::materialization_request::MaterializationRequestA0;
use kicad_monkey_contracts::materialization_contract::{
    decode_materialization_document_a0, validate_materialization_document,
};
use kicad_monkey_core::board_materialization::{
    chunk_operations, filter_document, materialization_sexpr, materialize_board,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const TILE: i64 = 10_000_000;

fn slice(key: &str, role: &str, order: u32) -> Value {
    json!({ "key": key, "role": role, "order": order })
}

fn affine(m11: f64, m12: f64, m21: f64, m22: f64, tx: i32, ty: i32) -> Value {
    json!({ "m11": m11, "m12": m12, "m21": m21, "m22": m22, "tx_nm": tx, "ty_nm": ty })
}

fn bounds(min_x: i32, min_y: i32, max_x: i32, max_y: i32) -> Value {
    json!({ "min_x_nm": min_x, "min_y_nm": min_y, "max_x_nm": max_x, "max_y_nm": max_y })
}

/// The issue #20 worked example: a rotated rounded-rectangle GND land on
/// front copper and a plated GND hole through the board, plus back copper and
/// a front mask opening so filters have something to remove.
fn example_operations() -> Value {
    json!([
        {
            "kind": "surface", "index": 0, "action": "add", "material": "copper",
            "slice_key": "copper.front", "net_key": "GND",
            "shape": { "kind": "rounded_rect_2d", "center_nm": [0, 0],
                       "width_nm": 2_400_000, "height_nm": 1_200_000, "radius_nm": 200_000 },
            "affine": affine(0.0, 1.0, -1.0, 0.0, 17_000_000, 8_000_000),
            "bounds": bounds(16_400_000, 6_800_000, 17_600_000, 9_200_000)
        },
        {
            "kind": "hole", "index": 1, "plating": "plated",
            "start_slice_key": "copper.front", "end_slice_key": "copper.back",
            "net_key": "GND", "backdrills": [],
            "shape": { "kind": "disk_2d", "center_nm": [0, 0], "radius_nm": 300_000 },
            "affine": affine(1.0, 0.0, 0.0, 1.0, 17_000_000, 8_000_000),
            "bounds": bounds(16_700_000, 7_700_000, 17_300_000, 8_300_000)
        },
        {
            "kind": "surface", "index": 2, "action": "add", "material": "copper",
            "slice_key": "copper.back", "net_key": "VCC",
            "shape": { "kind": "swept_path_2d", "start_nm": [0, 0],
                       "segments": [ { "kind": "line", "end_nm": [5_000_000, 0] },
                                     { "kind": "arc", "mid_nm": [6_000_000, 1_000_000],
                                       "end_nm": [5_000_000, 2_000_000] } ],
                       "width_nm": 250_000, "cap": "round", "join": "round" },
            "affine": affine(1.0, 0.0, 0.0, 1.0, 30_000_000, 2_000_000),
            "bounds": bounds(29_875_000, 1_875_000, 36_125_000, 4_125_000)
        },
        {
            "kind": "surface", "index": 3, "action": "subtract", "material": "solder_mask",
            "slice_key": "mask.front",
            "shape": { "kind": "rect_2d", "center_nm": [0, 0],
                       "width_nm": 2_500_000, "height_nm": 1_300_000 },
            "affine": affine(0.0, 1.0, -1.0, 0.0, 17_000_000, 8_000_000),
            "bounds": bounds(16_350_000, 6_750_000, 17_650_000, 9_250_000)
        }
    ])
}

fn example_document() -> MaterializationDocumentA0 {
    let value = json!({
        "schema": "kicad_monkey.materialization.a0",
        "type": "kicad_monkey.materialization.document",
        "version": "a0",
        "producer": { "name": "slice0-test", "version": "0" },
        "source": { "sha256": "0".repeat(64), "source_bytes": "0" },
        "coordinate_system": { "unit": "nm", "x_axis": "board_right", "y_axis": "board_down" },
        "scope": { "products": ["components", "copper", "dielectric", "holes", "paste",
                                "silkscreen", "solder_mask"],
                   "tile_size_nm": TILE, "complete": true },
        "stack": { "slices": [
            slice("mask.front", "solder_mask", 0),
            slice("copper.front", "conductor", 1),
            slice("dielectric.core", "dielectric", 2),
            slice("copper.back", "conductor", 3)
        ] },
        "nets": [ { "key": "", "name": "" }, { "key": "GND", "name": "GND" },
                  { "key": "VCC", "name": "VCC" } ],
        "net_classes": [], "diff_pairs": [], "components": [],
        "operations": example_operations(),
        "binding": [
            { "operation_index": 0, "source_kind": "pad", "source_uid": "pad-uuid",
              "refdes": "U1", "pad_number": "1", "net_key": "GND" },
            { "operation_index": 1, "source_kind": "pad", "source_uid": "pad-uuid",
              "refdes": "U1", "pad_number": "1", "net_key": "GND" },
            { "operation_index": 2, "source_kind": "track", "source_uid": "track-uuid",
              "net_key": "VCC" }
        ],
        "chunks": [], "diagnostics": []
    });
    let mut document: MaterializationDocumentA0 =
        serde_json::from_value(value).expect("example decodes against the a0 contract");
    rechunk(&mut document);
    document
}

/// Apply the shared chunking rule and remap the binding table.
fn rechunk(document: &mut MaterializationDocumentA0) {
    let order: BTreeMap<String, u32> = document
        .stack
        .slices
        .iter()
        .map(|slice| (slice.key.clone(), slice.order))
        .collect();
    let chunked = chunk_operations(std::mem::take(&mut document.operations), &order, TILE);
    for entry in &mut document.binding {
        let input = usize::try_from(entry.operation_index).expect("index fits usize");
        entry.operation_index = chunked.new_index_of_input[input];
    }
    document.binding.sort_by_key(|entry| entry.operation_index);
    document.operations = chunked.operations;
    document.chunks = chunked.chunks;
}

fn scope(products: &[MaterialProduct], slices: &[&str], nets: &[&str]) -> MaterializationScope {
    MaterializationScope {
        products: products.to_vec(),
        slice_keys: slices.iter().map(|key| (*key).to_owned()).collect(),
        net_keys: nets.iter().map(|key| (*key).to_owned()).collect(),
        region: None,
        tile_size_nm: NonNegativeNm(TILE),
        complete: false,
    }
}

#[test]
fn example_decodes_validates_and_round_trips_json() {
    let document = example_document();
    validate_materialization_document(&document).expect("example is a valid a0 document");
    let first = serde_json::to_string(&document).expect("serialize");
    let decoded = decode_materialization_document_a0(first.as_bytes()).expect("decode");
    let second = serde_json::to_string(&decoded).expect("serialize again");
    assert_eq!(
        first, second,
        "JSON debug view is byte-stable across a round trip"
    );
}

#[test]
fn chunking_orders_by_slice_then_tile_and_covers_every_operation() {
    let document = example_document();
    let slices: Vec<&str> = document
        .chunks
        .iter()
        .map(|chunk| chunk.slice_key.as_str())
        .collect();
    assert_eq!(slices, ["mask.front", "copper.front", "copper.back"]);
    let covered: u32 = document
        .chunks
        .iter()
        .map(|chunk| chunk.operation_count)
        .sum();
    assert_eq!(covered, 4);
    let front = &document.chunks[1];
    assert_eq!(
        front.net_keys,
        ["GND"],
        "land and hole share front copper chunk"
    );
    assert_eq!((front.tile_x, front.tile_y), (1, 0));
    assert_eq!(document.binding[0].refdes.as_deref(), Some("U1"));
}

#[test]
fn sexpr_view_is_deterministic_and_matches_the_issue_sketch() {
    let document = example_document();
    let view = materialization_sexpr(&document);
    assert_eq!(view, materialization_sexpr(&document));
    assert!(view.starts_with("(pcb_analytic_materialization"));
    assert!(view.contains("(affine 0 1 -1 0 17000000 8000000)"));
    assert!(view.contains("(rounded_rect_2d"));
    assert!(view.contains("(center_nm 0 0)"));
    assert!(view.contains("(radius_nm 200000)"));
    assert!(view.contains("(net_key \"GND\")"));
}

#[test]
fn filtering_reindexes_rechunks_and_stays_valid() {
    let complete = example_document();
    let copper = filter_document(&complete, &scope(&[MaterialProduct::Copper], &[], &[]));
    validate_materialization_document(&copper).expect("filtered document is valid");
    assert_eq!(copper.operations.len(), 2);
    assert_eq!(
        copper.binding.len(),
        2,
        "hole binding is dropped with the hole"
    );

    let back = filter_document(
        &complete,
        &scope(
            &[MaterialProduct::Holes, MaterialProduct::Copper],
            &["copper.back"],
            &[],
        ),
    );
    validate_materialization_document(&back).expect("slice-filtered document is valid");
    assert_eq!(
        back.operations.len(),
        2,
        "back trace plus the hole crossing the back slice"
    );

    let gnd = filter_document(&complete, &scope(&[MaterialProduct::Holes], &[], &["GND"]));
    assert_eq!(gnd.operations.len(), 1);
}

#[test]
fn filtering_is_idempotent() {
    let complete = example_document();
    let wanted = scope(
        &[MaterialProduct::Copper, MaterialProduct::Holes],
        &[],
        &["GND"],
    );
    let once = filter_document(&complete, &wanted);
    let twice = filter_document(&once, &wanted);
    assert_eq!(
        serde_json::to_value(&once).expect("serialize"),
        serde_json::to_value(&twice).expect("serialize")
    );
}

#[test]
fn validator_rejects_copper_without_a_net() {
    let mut value = serde_json::to_value(example_document()).expect("serialize");
    let operation = value["operations"]
        .as_array_mut()
        .and_then(|operations| {
            operations
                .iter_mut()
                .find(|operation| operation["material"] == "copper")
        })
        .expect("a copper operation");
    operation
        .as_object_mut()
        .expect("operation object")
        .remove("net_key");
    let document: MaterializationDocumentA0 = serde_json::from_value(value).expect("decode");
    let error = validate_materialization_document(&document).expect_err("copper needs a net");
    assert_eq!(error.code, "missing_net");
}

#[test]
fn decoding_rejects_a_mislabelled_shape_kind() {
    let mut value = serde_json::to_value(example_document()).expect("serialize");
    let shape = value["operations"]
        .as_array_mut()
        .and_then(|operations| {
            operations
                .iter_mut()
                .find(|operation| operation["shape"]["kind"] == "disk_2d")
        })
        .expect("a disk");
    shape["shape"]["kind"] = json!("annulus_2d");
    assert!(serde_json::from_value::<MaterializationDocumentA0>(value).is_err());
}

fn request(products: &[&str], max_source_bytes: &str) -> MaterializationRequestA0 {
    serde_json::from_value(json!({
        "type": "kicad_monkey.materialization.request",
        "version": "a0",
        "products": products,
        "tile_size_nm": TILE,
        "max_source_bytes": max_source_bytes,
        "max_operations": 1_000_000,
        "max_points": 10_000_000
    }))
    .expect("request decodes")
}

/// Three-layer board: a rotated footprint with an SMD rect pad and a plated
/// round through-hole pad, a track, a track arc, a through via, and a stored
/// zone fill.
const BOARD: &str = r#"(kicad_pcb
  (version 20240108)
  (generator pcbnew)
  (general (thickness 1.6))
  (layers
    (0 "F.Cu" signal)
    (1 "In1.Cu" signal)
    (31 "B.Cu" signal)
    (36 "B.SilkS" user "b.silkscreen"))
  (net 0 "")
  (net 1 "GND")
  (net 2 "VCC")
  (footprint "Demo:Part"
    (layer "F.Cu")
    (at 10 20 90)
    (property "Reference" "U1")
    (uuid fp-u1)
    (pad "1" smd rect (at 1 0 90) (size 1 0.5) (layers "F.Cu" "F.Paste" "F.Mask") (net 1 "GND")
      (uuid pad-u1-1))
    (pad "2" thru_hole circle (at -1 0 90) (size 1.2 1.2) (drill 0.6) (layers "*.Cu" "*.Mask")
      (net 2 "VCC") (uuid pad-u1-2)))
  (segment (start 1 2) (end 3 2) (width 0.25) (layer "F.Cu") (net 1) (uuid trk-1))
  (arc (start 0 0) (mid 1 1) (end 2 0) (width 0.2) (layer "B.Cu") (net 2) (uuid arc-1))
  (via (at 5 6) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (net 1) (uuid via-1))
  (zone (net 1) (net_name "GND") (layers "In1.Cu") (uuid zone-1)
    (polygon (pts (xy 0 0) (xy 20 0) (xy 20 20) (xy 0 20)))
    (filled_polygon (layer "In1.Cu") (pts (xy 0 0) (xy 20 0) (xy 20 20) (xy 0 20))))
)
"#;

fn complete_request() -> MaterializationRequestA0 {
    request(
        &[
            "components",
            "copper",
            "dielectric",
            "holes",
            "paste",
            "silkscreen",
            "solder_mask",
        ],
        "1048576",
    )
}

#[test]
fn producer_materializes_copper_holes_nets_and_binding() {
    let document = materialize_board(BOARD, &complete_request()).expect("board materializes");
    validate_materialization_document(&document).expect("producer output is valid");
    assert!(document.scope.complete);
    let slices: Vec<&str> = document
        .stack
        .slices
        .iter()
        .map(|slice| slice.key.as_str())
        .collect();
    assert_eq!(slices, ["copper.front", "copper.in1", "copper.back"]);
    let nets: Vec<&str> = document.nets.iter().map(|net| net.key.as_str()).collect();
    assert_eq!(nets, ["", "GND", "VCC"]);

    let value = serde_json::to_value(&document).expect("serialize");
    let operations = value["operations"].as_array().expect("operations");
    let count = |kind: &str, field: &str| {
        operations
            .iter()
            .filter(|operation| operation["kind"] == kind && operation["shape"]["kind"] == field)
            .count()
    };
    // track + arc are swept paths; the via lands on 3 slices; the TH pad on 3.
    assert_eq!(count("surface", "swept_path_2d"), 2);
    assert_eq!(count("surface", "disk_2d"), 3 + 3);
    assert_eq!(count("surface", "rect_2d"), 1);
    assert_eq!(count("surface", "planar_region_2d"), 1);
    assert_eq!(count("hole", "disk_2d"), 2);
    assert_eq!(
        document.binding.len(),
        operations.len(),
        "every operation is bound"
    );
    assert_eq!(document.components.len(), 1);
    assert_eq!(
        document.diagnostics.len(),
        4,
        "four products are not yet materialized"
    );
}

#[test]
fn pads_compose_the_footprint_transform_and_bind_to_their_pins() {
    let document = materialize_board(BOARD, &complete_request()).expect("board materializes");
    let value = serde_json::to_value(&document).expect("serialize");
    let smd = value["operations"]
        .as_array()
        .and_then(|operations| {
            operations
                .iter()
                .find(|operation| operation["shape"]["kind"] == "rect_2d")
        })
        .expect("the SMD land");
    // Local (1, 0) rotated by -90 degrees about the footprint at (10, 20).
    assert_eq!(smd["affine"]["tx_nm"], 10_000_000);
    assert_eq!(smd["affine"]["ty_nm"], 19_000_000);
    assert_eq!(smd["slice_key"], "copper.front");
    assert_eq!(smd["net_key"], "GND");
    let index = smd["index"].as_u64().expect("index");
    let entry = document
        .binding
        .iter()
        .find(|entry| u64::from(entry.operation_index) == index)
        .expect("binding entry");
    assert_eq!(entry.refdes.as_deref(), Some("U1"));
    assert_eq!(entry.pad_number.as_deref(), Some("1"));

    let hole = value["operations"]
        .as_array()
        .and_then(|operations| {
            operations
                .iter()
                .find(|operation| operation["kind"] == "hole" && operation["net_key"] == "VCC")
        })
        .expect("the plated pad hole");
    assert_eq!(hole["start_slice_key"], "copper.front");
    assert_eq!(hole["end_slice_key"], "copper.back");
    assert_eq!(hole["plating"], "plated");
}

#[test]
fn selective_materialization_equals_the_filtered_complete_one() {
    let complete = materialize_board(BOARD, &complete_request()).expect("complete");
    for (products, slices, nets) in [
        (vec!["copper"], vec![], vec![]),
        (vec!["copper", "holes"], vec!["copper.back"], vec![]),
        (vec!["holes"], vec![], vec!["GND"]),
        (
            vec!["copper", "components"],
            vec!["copper.in1"],
            vec!["GND"],
        ),
    ] {
        let mut wanted = request(&products, "1048576");
        wanted.slice_keys = slices.iter().map(|key| (*key).to_owned()).collect();
        wanted.net_keys = nets.iter().map(|key| (*key).to_owned()).collect();
        let selective = materialize_board(BOARD, &wanted).expect("selective");
        validate_materialization_document(&selective).expect("selective output is valid");
        let filtered = filter_document(&complete, &selective.scope);
        assert_eq!(
            serde_json::to_value(&selective).expect("serialize"),
            serde_json::to_value(&filtered).expect("serialize"),
            "rule 2 for products {products:?}, slices {slices:?}, nets {nets:?}"
        );
    }
}

#[test]
fn producer_output_is_byte_deterministic() {
    let first = materialize_board(BOARD, &complete_request()).expect("first");
    let second = materialize_board(BOARD, &complete_request()).expect("second");
    assert_eq!(
        serde_json::to_string(&first).expect("serialize"),
        serde_json::to_string(&second).expect("serialize")
    );
}

#[test]
fn producer_rejects_a_board_without_copper_layers() {
    let error = materialize_board("(kicad_pcb (version 20240108))", &complete_request())
        .expect_err("no copper layers");
    assert_eq!(error.code, "no_copper_layers");
}

#[test]
fn producer_enforces_the_source_limit() {
    let error = materialize_board("(kicad_pcb)", &request(&["copper"], "4"))
        .expect_err("source exceeds the limit");
    assert_eq!(error.code, "source_too_large");
}

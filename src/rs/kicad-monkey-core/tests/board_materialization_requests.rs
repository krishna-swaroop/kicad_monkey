//! Review batch 2 evidence: request boundaries and limits, reported layer
//! drops, extreme margins, footprint knockout text, and footprint text-box
//! borders.

use kicad_monkey_contracts::generated::materialization_request::MaterializationRequestA0;
use kicad_monkey_contracts::materialization_contract::validate_materialization_document;
use kicad_monkey_core::board_materialization::materialize_board;
use serde_json::{Value, json};

fn request(products: &[&str], extra: Value) -> MaterializationRequestA0 {
    let mut value = json!({
        "type": "kicad_monkey.materialization.request",
        "version": "a0",
        "products": products,
        "tile_size_nm": 10_000_000,
        "max_source_bytes": "1048576",
        "max_operations": 1_000_000,
        "max_points": 10_000_000
    });
    if let (Some(target), Some(fields)) = (value.as_object_mut(), extra.as_object()) {
        target.extend(fields.clone());
    }
    serde_json::from_value(value).expect("request decodes")
}

/// Two copper layers. Two GND tracks and one VCC track, a track and a via
/// on a layer the board does not declare, a zero-width track, and a
/// footprint with knockout reference text and a bordered text box.
const BOARD: &str = r#"(kicad_pcb
  (version 20241229)
  (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (37 "F.SilkS" user) (39 "F.Mask" user)
    (44 "Edge.Cuts" user))
  (setup (pad_to_mask_clearance MASK))
  (net 0 "")
  (net 1 "GND")
  (net 2 "VCC")
  (footprint "Demo:K"
    (layer "F.Cu")
    (at 10 10)
    (property "Reference" "K1" (at 0 -3 0) (layer "F.SilkS" knockout) (uuid ref-k1)
      (effects (font (size 1 1) (thickness 0.15))))
    (fp_text_box "Box" (start -3 3) (end 3 6) (margins 0.3 0.3 0.3 0.3) (layer "F.SilkS")
      (uuid box-k1) (effects (font (size 1 1) (thickness 0.15)) (justify left top))
      (border yes) (stroke (width 0.15) (type solid)))
    (pad "1" smd rect (at 0 0) (size 1 2) (layers "F.Cu" "F.Mask") (net 1 "GND") (uuid pad-k1)))
  (gr_rect (start 0 0) (end 40 30) (stroke (width 0.1) (type solid)) (layer "Edge.Cuts"))
  (segment (start 20 5) (end 30 5) (width 0.25) (layer "F.Cu") (net 1) (uuid gnd-a))
  (segment (start 20 6) (end 30 6) (width 0.25) (layer "B.Cu") (net 1) (uuid gnd-b))
  (segment (start 20 7) (end 30 7) (width 0.25) (layer "F.Cu") (net 2) (uuid vcc-a))
  (segment (start 20 8) (end 30 8) (width 0.25) (layer "In3.Cu") (net 2) (uuid stray-track))
  (segment (start 20 9) (end 30 9) (width 0) (layer "F.Cu") (net 2) (uuid zero-track))
  (via (at 35 5) (size 0.6) (drill 0.3) (layers "F.Cu" "In3.Cu") (net 1) (uuid stray-via))
)
"#;

fn board(mask: &str) -> String {
    BOARD.replace("MASK", mask)
}

fn codes(products: &[&str]) -> Vec<String> {
    let document = materialize_board(&board("0.05"), &request(products, json!({}))).expect("ok");
    validate_materialization_document(&document).expect("valid");
    document
        .diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "{}:{}",
                diagnostic.code,
                diagnostic.source_uid.as_deref().unwrap_or("")
            )
        })
        .collect()
}

#[test]
fn an_inverted_region_and_signed_limits_are_rejected() {
    let inverted = json!({ "region": {
        "min_x_nm": 10, "min_y_nm": 10, "max_x_nm": 0, "max_y_nm": 0 } });
    let error = materialize_board(&board("0.05"), &request(&["copper"], inverted))
        .expect_err("inverted region");
    assert_eq!(error.code, "invalid_region");
    let signed = json!({ "max_source_bytes": "+1048576" });
    let error =
        materialize_board(&board("0.05"), &request(&["copper"], signed)).expect_err("signed limit");
    assert_eq!(error.code, "invalid_limit");
}

#[test]
fn max_operations_counts_the_returned_operations() {
    // VCC alone is one track, although copper has five operations.
    let vcc = json!({ "net_keys": ["VCC"], "max_operations": 1 });
    let document = materialize_board(&board("0.05"), &request(&["copper"], vcc)).expect("fits");
    assert_eq!(document.operations.len(), 1);
    let all = json!({ "max_operations": 1 });
    let error = materialize_board(&board("0.05"), &request(&["copper"], all))
        .expect_err("copper exceeds one operation");
    assert_eq!(error.code, "too_many_operations");
}

#[test]
fn max_points_bounds_the_returned_shapes() {
    let tight = json!({ "max_points": 1 });
    let error = materialize_board(&board("0.05"), &request(&["copper"], tight))
        .expect_err("tracks carry two points each");
    assert_eq!(error.code, "too_many_points");
}

#[test]
fn undeclared_layers_and_zero_widths_are_reported_not_dropped() {
    let copper = codes(&["copper"]);
    assert!(copper.contains(&"layer_not_in_stack:stray-track".to_owned()));
    assert!(copper.contains(&"layer_not_in_stack:stray-via".to_owned()));
    assert!(copper.contains(&"track_without_width:zero-track".to_owned()));
    assert!(codes(&["holes"]).contains(&"layer_not_in_stack:stray-via".to_owned()));
}

#[test]
fn extreme_margins_saturate_into_valid_shapes() {
    for mask in ["1e300", "2147.5", "-1e300"] {
        let document = materialize_board(&board(mask), &request(&["solder_mask"], json!({})))
            .expect("materializes");
        validate_materialization_document(&document).expect("saturated shapes stay valid");
    }
}

#[test]
fn footprint_knockout_text_and_text_box_borders_are_drawn() {
    let document =
        materialize_board(&board("0.05"), &request(&["silkscreen"], json!({}))).expect("ok");
    let value = serde_json::to_value(&document).expect("serialize");
    let bound = |uid: &str| -> Vec<Value> {
        let binding = value["binding"].as_array().expect("binding");
        value["operations"]
            .as_array()
            .expect("operations")
            .iter()
            .filter(|op| {
                binding.iter().any(|entry| {
                    entry["operation_index"] == op["index"] && entry["source_uid"] == uid
                })
            })
            .cloned()
            .collect()
    };
    let knockout = bound("ref-k1");
    assert!(knockout.iter().any(|op| op["action"] == "subtract"));
    assert!(knockout.iter().all(|op| op["group_key"] == "ref-k1"));
    let text_box = bound("box-k1");
    assert!(
        text_box
            .iter()
            .any(|op| op["shape"]["kind"] == "swept_path_2d" && op["shape"]["width_nm"] == 150_000),
        "the 0.15 mm border is drawn"
    );
}

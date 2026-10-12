//! Slice 3 evidence: solder mask, paste, and silkscreen, with KiCad's
//! margin precedence, per-shape aperture growth, via tenting, copper
//! graphics, knockout text, and rule 2 across the technical products.

use kicad_monkey_contracts::generated::materialization_request::MaterializationRequestA0;
use kicad_monkey_contracts::materialization_contract::validate_materialization_document;
use kicad_monkey_core::board_materialization::{filter_document, materialize_board};
use serde_json::{Value, json};

const ALL: [&str; 7] = [
    "components",
    "copper",
    "dielectric",
    "holes",
    "paste",
    "silkscreen",
    "solder_mask",
];

fn request(products: &[&str], slices: &[&str]) -> MaterializationRequestA0 {
    serde_json::from_value(json!({
        "type": "kicad_monkey.materialization.request",
        "version": "a0",
        "products": products,
        "slice_keys": slices,
        "tile_size_nm": 10_000_000,
        "max_source_bytes": "1048576",
        "max_operations": 1_000_000,
        "max_points": 10_000_000
    }))
    .expect("request decodes")
}

/// Two copper layers, no stackup. Board mask expansion 0.05 mm, paste
/// margin -0.02 mm with ratio -0.1, front vias tented and back vias not.
/// Footprint U1 at (10, 10) carries a board-margin rect pad, a pad with an
/// explicit zero mask margin, a trapezoid, a through-hole pad whose copper
/// is offset from its drill, reference text, and a zero-width paste polygon.
const TECH: &str = r#"(kicad_pcb
  (version VERSION)
  (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (32 "B.Adhes" user) (34 "B.Paste" user)
    (35 "F.Paste" user) (36 "B.SilkS" user) (37 "F.SilkS" user) (38 "B.Mask" user)
    (39 "F.Mask" user) (44 "Edge.Cuts" user))
  (setup
    (pad_to_mask_clearance 0.05)
    (pad_to_paste_clearance -0.02)
    (pad_to_paste_clearance_ratio -0.1)
    (tenting (front yes) (back no)))
  (net 0 "")
  (net 1 "GND")
  (footprint "Demo:U"
    (layer "F.Cu")
    (at 10 10)
    (property "Reference" "U1" (at 0 -3 0) (layer "F.SilkS") (uuid ref-u1)
      (effects (font (size 1 1) (thickness 0.15))))
    (fp_poly (pts (xy 3 3) (xy 4 3) (xy 4 4) (xy 3 4))
      (stroke (width 0) (type solid)) (fill yes) (layer "F.Paste") (uuid poly-paste))
    (pad "1" smd rect (at -2 0) (size 1 2) (layers "F.Cu" "F.Paste" "F.Mask") (net 1 "GND")
      (uuid pad-rect))
    (pad "2" smd roundrect (at 0 0) (size 1 2) (roundrect_rratio 0.25)
      (layers "F.Cu" "F.Mask") (solder_mask_margin 0) (uuid pad-zero))
    (pad "3" smd trapezoid (at 2 0) (size 1 2) (rect_delta 0 0.4) (layers "F.Cu" "F.Mask")
      (uuid pad-trap))
    (pad "4" thru_hole circle (at 5 0) (size 1.6 1.6) (drill 0.6 (offset 0.3 0))
      (layers "*.Cu" "*.Mask") (net 1 "GND") (uuid pad-offset)))
  (gr_rect (start 0 0) (end 40 30) (stroke (width 0.1) (type solid)) (layer "Edge.Cuts"))
  (gr_line (start 1 1) (end 9 1) (stroke (width 0.15) (type solid)) (layer "F.SilkS")
    (uuid silk-line))
  (gr_rect (start 30 20) (end 34 22) (stroke (width 0) (type solid)) (fill yes)
    (layers "F.Cu" "F.Mask") (net 1 "GND") (uuid copper-rect))
  (gr_text "KO" (at 20 25 0) (layer "F.SilkS" knockout) (uuid knockout-text)
    (effects (font (size 1 1) (thickness 0.15)) (justify left bottom)))
  (via (at 20 5) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (net 1) (uuid via-a))
)
"#;

fn board(version: &str) -> String {
    TECH.replace("VERSION", version)
}

fn materialize(source: &str, products: &[&str]) -> Value {
    let document = materialize_board(source, &request(products, &[])).expect("materializes");
    validate_materialization_document(&document).expect("valid");
    serde_json::to_value(&document).expect("serialize")
}

/// Operations on `slice` bound to `uid`.
fn bound<'a>(document: &'a Value, slice: &str, uid: &str) -> Vec<&'a Value> {
    let binding = document["binding"].as_array().expect("binding");
    document["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .filter(|op| op["slice_key"] == slice)
        .filter(|op| {
            binding
                .iter()
                .any(|entry| entry["operation_index"] == op["index"] && entry["source_uid"] == uid)
        })
        .collect()
}

#[test]
fn mask_openings_follow_kicad_margins_and_shapes() {
    let document = materialize(&board("20241229"), &["solder_mask"]);
    let rect = bound(&document, "mask.front", "pad-rect");
    assert_eq!(rect.len(), 1);
    // A rect grown by a positive margin becomes a rounded rect of that radius.
    assert_eq!(
        rect[0]["shape"],
        json!({ "kind": "rounded_rect_2d", "center_nm": [0, 0], "width_nm": 1_100_000,
                "height_nm": 2_100_000, "radius_nm": 50_000 })
    );
    assert_eq!(rect[0]["action"], "subtract");
    // An explicit zero is an override in format 9.0 files.
    let zero = bound(&document, "mask.front", "pad-zero");
    assert_eq!(zero[0]["shape"]["width_nm"], 1_000_000);
    // A trapezoid keeps its shape and carries a round offset.
    let trap = bound(&document, "mask.front", "pad-trap");
    assert_eq!(trap[0]["shape"]["kind"], "trapezoid_2d");
    assert_eq!(trap[0]["offset_nm"], 50_000);
    // The board profile is the mask before openings, on both sides.
    for slice in ["mask.front", "mask.back"] {
        assert_eq!(bound(&document, slice, "board_profile#0").len(), 1);
    }
}

#[test]
fn zero_margins_inherit_in_pre_nine_files() {
    let document = materialize(&board("20231120"), &["solder_mask"]);
    let zero = bound(&document, "mask.front", "pad-zero");
    assert_eq!(zero[0]["shape"]["width_nm"], 1_100_000);
}

#[test]
fn vias_open_the_mask_only_where_untented() {
    let document = materialize(&board("20241229"), &["solder_mask"]);
    assert!(bound(&document, "mask.front", "via-a").is_empty());
    let back = bound(&document, "mask.back", "via-a");
    assert_eq!(back[0]["shape"]["radius_nm"], 450_000);
    // A legacy `viasonmask` overrides `(tenting ...)`, as KiCad parses it last.
    let legacy = board("20241229").replace(
        "(tenting (front yes) (back no)))",
        "(tenting (front yes) (back no)) (pcbplotparams (viasonmask yes)))",
    );
    let document = materialize(&legacy, &["solder_mask"]);
    assert_eq!(bound(&document, "mask.front", "via-a").len(), 1);
}

#[test]
fn paste_applies_margin_and_ratio_per_axis_and_keeps_zero_width_fills() {
    let document = materialize(&board("20241229"), &["paste"]);
    let rect = bound(&document, "paste.front", "pad-rect");
    // -0.02 mm plus -10 % of each axis: 1.0 -> 0.76 mm, 2.0 -> 1.56 mm.
    assert_eq!(
        rect[0]["shape"],
        json!({ "kind": "rect_2d", "center_nm": [0, 0], "width_nm": 760_000,
                "height_nm": 1_560_000 })
    );
    let poly = bound(&document, "paste.front", "poly-paste");
    let kinds: Vec<&Value> = poly.iter().map(|op| &op["shape"]["kind"]).collect();
    assert_eq!(
        kinds,
        [&json!("planar_region_2d")],
        "no stroke on a zero-width fill"
    );
}

#[test]
fn copper_graphics_are_copper_and_open_the_mask_with_the_board_margin() {
    let document = materialize(&board("20241229"), &["copper", "solder_mask"]);
    let copper = bound(&document, "copper.front", "copper-rect");
    assert_eq!(copper.len(), 1);
    assert_eq!(copper[0]["net_key"], "GND");
    let opening = bound(&document, "mask.front", "copper-rect");
    assert_eq!(
        opening.len(),
        1,
        "one opening, not the raw mask-layer copy too"
    );
    assert_eq!(opening[0]["offset_nm"], 50_000);
}

#[test]
fn pad_holes_sit_at_the_pad_not_at_the_copper_offset() {
    let document = materialize(&board("20241229"), &["copper", "holes"]);
    let operations = document["operations"].as_array().expect("operations");
    let hole = operations
        .iter()
        .find(|op| op["kind"] == "hole" && op["shape"]["radius_nm"] == 300_000)
        .expect("the offset pad's hole");
    assert_eq!(hole["affine"]["tx_nm"], 15_000_000);
    let land = bound(&document, "copper.front", "pad-offset");
    assert_eq!(land[0]["affine"]["tx_nm"], 15_300_000);
}

#[test]
fn silkscreen_draws_lines_text_and_knockout_boxes() {
    let document = materialize(&board("20241229"), &["silkscreen"]);
    let line = bound(&document, "silkscreen.front", "silk-line");
    assert_eq!(line[0]["shape"]["width_nm"], 150_000);
    assert!(!bound(&document, "silkscreen.front", "ref-u1").is_empty());
    let knockout = bound(&document, "silkscreen.front", "knockout-text");
    let adds = knockout.iter().filter(|op| op["action"] == "add").count();
    let cuts = knockout
        .iter()
        .filter(|op| op["action"] == "subtract")
        .count();
    assert_eq!(adds, 1, "one knockout box");
    assert!(cuts > 0, "stroke glyphs are cut from the box");
}

#[test]
fn technical_products_keep_rule_two_and_determinism() {
    let source = board("20241229");
    let complete = materialize_board(&source, &request(&ALL, &[])).expect("complete");
    let again = materialize_board(&source, &request(&ALL, &[])).expect("again");
    assert_eq!(
        serde_json::to_string(&complete).expect("serialize"),
        serde_json::to_string(&again).expect("serialize")
    );
    for products in [
        &["solder_mask"][..],
        &["paste"][..],
        &["silkscreen"][..],
        &["copper", "solder_mask"][..],
    ] {
        for slices in [
            &[][..],
            &["mask.front"][..],
            &["paste.front", "silkscreen.back"][..],
        ] {
            let selective = materialize_board(&source, &request(products, slices)).expect("ok");
            let filtered = filter_document(&complete, &selective.scope);
            assert_eq!(
                serde_json::to_value(&selective).expect("serialize"),
                serde_json::to_value(&filtered).expect("serialize"),
                "rule 2 for {products:?} on {slices:?}"
            );
        }
    }
}

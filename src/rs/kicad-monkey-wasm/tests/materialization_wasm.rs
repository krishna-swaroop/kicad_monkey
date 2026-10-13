//! The browser path end to end in a wasm runtime: materialize a board to a
//! pack, open it, and lower a copper slice for the analytic renderer.

#![cfg(feature = "materialize")]

use kicad_monkey_wasm::materialization::{AnalyticPack, lower, materialize_board_pack};
use wasm_bindgen_test::wasm_bindgen_test;

const BOARD: &str = r#"(kicad_pcb
  (version 20241229)
  (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
  (net 0 "")
  (net 1 "GND")
  (gr_rect (start 0 0) (end 20 10) (stroke (width 0.1) (type solid)) (layer "Edge.Cuts"))
  (segment (start 2 2) (end 12 2) (width 0.5) (layer "F.Cu") (net 1) (uuid t1))
  (via (at 15 5) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (net 1) (uuid v1))
)
"#;

const REQUEST: &str = r#"{"type":"kicad_monkey.materialization.request","version":"a0",
  "products":["copper","holes"],"tile_size_nm":10000000,"max_source_bytes":"1048576",
  "max_operations":1000,"max_points":100000}"#;

#[wasm_bindgen_test]
fn a_board_materializes_packs_opens_and_lowers_in_wasm() {
    let bytes = materialize_board_pack(BOARD, None, REQUEST).unwrap_or_default();
    assert!(bytes.starts_with(b"KMMATPK\0"));
    let pack = AnalyticPack::open(bytes).unwrap_or_else(|error| panic!("{error}"));
    let front = pack
        .lower("copper.front", 1_000.0)
        .unwrap_or_else(|error| panic!("{error}"));
    // The track and the via land, with the via hole cutting first.
    assert_eq!(front.shapes.len(), 3 * lower::SHAPE_WORDS);
    assert_eq!(front.subtract_shapes, 1);
    assert!(pack.header_without_binding().binding.is_empty());
}

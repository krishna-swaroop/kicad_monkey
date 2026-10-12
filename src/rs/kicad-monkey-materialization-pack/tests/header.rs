//! Pack preamble and header round trip and rejection paths.

use kicad_monkey_contracts::generated::materialization_document::MaterializationDocumentA0;
use kicad_monkey_materialization_pack::{
    COLUMN_ALIGNMENT, PACK_MAGIC, PackError, PackLimits, decode_header, encode_header,
};
use serde_json::json;

fn document() -> MaterializationDocumentA0 {
    serde_json::from_value(json!({
        "schema": "kicad_monkey.materialization.a0",
        "type": "kicad_monkey.materialization.document",
        "version": "a0",
        "producer": { "name": "pack-test", "version": "0" },
        "source": { "sha256": "0".repeat(64), "source_bytes": "0" },
        "coordinate_system": { "unit": "nm", "x_axis": "board_right", "y_axis": "board_down" },
        "scope": { "products": ["copper"], "tile_size_nm": 10_000_000, "complete": false },
        "stack": { "slices": [ { "key": "copper.front", "role": "conductor", "order": 0 } ] },
        "nets": [ { "key": "GND", "name": "GND" } ],
        "net_classes": [], "diff_pairs": [], "components": [],
        "operations": [ {
            "kind": "surface", "index": 0, "action": "add", "material": "copper",
            "slice_key": "copper.front", "net_key": "GND",
            "shape": { "kind": "disk_2d", "center_nm": [0, 0], "radius_nm": 100 },
            "affine": { "m11": 1.0, "m12": 0.0, "m21": 0.0, "m22": 1.0, "tx_nm": 0, "ty_nm": 0 },
            "bounds": { "min_x_nm": -100, "min_y_nm": -100, "max_x_nm": 100, "max_y_nm": 100 }
        } ],
        "binding": [],
        "chunks": [ { "slice_key": "copper.front", "tile_x": 0, "tile_y": 0,
                      "bounds": { "min_x_nm": -100, "min_y_nm": -100, "max_x_nm": 100, "max_y_nm": 100 },
                      "net_keys": ["GND"], "first_operation_index": 0, "operation_count": 1 } ],
        "diagnostics": []
    }))
    .expect("document decodes")
}

#[test]
fn header_round_trips_without_operations_and_aligns_columns() {
    let bytes = encode_header(&document()).expect("encode");
    assert_eq!(&bytes[..8], &PACK_MAGIC);
    assert_eq!(bytes.len() % COLUMN_ALIGNMENT, 0);
    let header = decode_header(&bytes, PackLimits::default()).expect("decode");
    assert!(header.document.operations.is_empty());
    assert_eq!(header.document.chunks.len(), 1);
    assert_eq!(header.columns_offset, bytes.len());
    assert_eq!(encode_header(&header.document).expect("re-encode"), bytes);
}

#[test]
fn decoder_rejects_bad_magic_version_truncation_and_oversize() {
    let bytes = encode_header(&document()).expect("encode");
    let mut wrong_magic = bytes.clone();
    wrong_magic[0] = b'X';
    assert!(matches!(
        decode_header(&wrong_magic, PackLimits::default()),
        Err(PackError::BadMagic)
    ));

    let mut wrong_version = bytes.clone();
    wrong_version[8] = 9;
    assert!(matches!(
        decode_header(&wrong_version, PackLimits::default()),
        Err(PackError::UnsupportedVersion(9))
    ));

    assert!(matches!(
        decode_header(&bytes[..20], PackLimits::default()),
        Err(PackError::Truncated)
    ));

    let tight = PackLimits {
        max_header_bytes: 8,
    };
    assert!(matches!(
        decode_header(&bytes, tight),
        Err(PackError::HeaderTooLarge(_))
    ));
}

//! Corpus gate for the analytic board materializer.
//!
//! Usage: `cargo run --release -p kicad-monkey-core --example
//! materialization_gate -- BOARD.kicad_pcb [OUT.json]`
//!
//! Materializes the complete board, validates it against the a0 contract,
//! checks binding coverage and determinism, checks rule 2 (selective equals
//! filtered complete) for one scope per copper slice and per product, and
//! prints a JSON summary. With `OUT.json`, writes the complete document.

use kicad_monkey_contracts::generated::materialization_request::MaterializationRequestA0;
use kicad_monkey_contracts::materialization_contract::validate_materialization_document;
use kicad_monkey_core::board_materialization::{filter_document, materialize_board};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Instant;

const PRODUCTS: [&str; 7] = [
    "components",
    "copper",
    "dielectric",
    "holes",
    "paste",
    "silkscreen",
    "solder_mask",
];

fn request(products: &[&str], slices: &[String], source_bytes: usize) -> MaterializationRequestA0 {
    let value = json!({
        "type": "kicad_monkey.materialization.request",
        "version": "a0",
        "products": products,
        "slice_keys": slices,
        "tile_size_nm": 10_000_000,
        "max_source_bytes": source_bytes.to_string(),
        "max_operations": 50_000_000,
        "max_points": 500_000_000
    });
    match serde_json::from_value(value) {
        Ok(request) => request,
        Err(error) => fail(&format!("request: {error}")),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("materialization gate failed: {message}");
    std::process::exit(1);
}

fn shape_counts(document: &Value) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for operation in document["operations"].as_array().into_iter().flatten() {
        let key = format!(
            "{}/{}",
            operation["kind"].as_str().unwrap_or("?"),
            operation["shape"]["kind"].as_str().unwrap_or("?")
        );
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

/// Regions and cutouts on the first dielectric slice, plus stack shape.
fn profile_summary(document: &Value) -> Value {
    let first = document["stack"]["slices"]
        .as_array()
        .and_then(|slices| slices.iter().find(|slice| slice["role"] == "dielectric"))
        .and_then(|slice| slice["key"].as_str())
        .unwrap_or_default()
        .to_owned();
    let adds: Vec<&Value> = document["operations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|op| op["slice_key"] == first.as_str() && op["action"] == "add")
        .collect();
    let cutouts: usize = adds
        .iter()
        .filter_map(|op| op["shape"]["holes"].as_array().map(Vec::len))
        .sum();
    let known_thickness = document["stack"]["slices"].as_array().map_or(0, |slices| {
        slices
            .iter()
            .filter(|s| s["thickness_nm"].is_number())
            .count()
    });
    json!({ "regions": adds.len(), "cutouts": cutouts, "slices_with_thickness": known_thickness })
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = arguments.first() else {
        fail("usage: materialization_gate BOARD.kicad_pcb [OUT.json]");
    };
    let source = std::fs::read_to_string(path).unwrap_or_else(|error| fail(&error.to_string()));
    let started = Instant::now();
    let complete = materialize_board(&source, &request(&PRODUCTS, &[], source.len()))
        .unwrap_or_else(|error| fail(&error.to_string()));
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    validate_materialization_document(&complete).unwrap_or_else(|error| fail(&error.to_string()));
    if complete.binding.len() != complete.operations.len() {
        fail("binding does not cover every operation");
    }
    let again = materialize_board(&source, &request(&PRODUCTS, &[], source.len()))
        .unwrap_or_else(|error| fail(&error.to_string()));
    let text = serde_json::to_string(&complete).unwrap_or_else(|error| fail(&error.to_string()));
    if serde_json::to_string(&again).ok().as_deref() != Some(text.as_str()) {
        fail("output is not deterministic");
    }
    let mut scopes = 0usize;
    let slice_keys: Vec<String> = complete
        .stack
        .slices
        .iter()
        .map(|slice| slice.key.clone())
        .collect();
    for key in &slice_keys {
        for products in [
            &["copper"][..],
            &["holes"][..],
            &["copper", "holes"][..],
            &["dielectric"][..],
        ] {
            let selective = materialize_board(
                &source,
                &request(products, std::slice::from_ref(key), source.len()),
            )
            .unwrap_or_else(|error| fail(&error.to_string()));
            let filtered = filter_document(&complete, &selective.scope);
            if serde_json::to_value(&selective).ok() != serde_json::to_value(&filtered).ok() {
                fail(&format!("rule 2 failed for {products:?} on {key}"));
            }
            scopes += 1;
        }
    }
    if let Some(out) = arguments.get(1) {
        std::fs::write(out, &text).unwrap_or_else(|error| fail(&error.to_string()));
    }
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let mut diagnostics = BTreeMap::new();
    for diagnostic in &complete.diagnostics {
        *diagnostics.entry(diagnostic.code.clone()).or_insert(0usize) += 1;
    }
    let summary = json!({
        "board": path,
        "materialize_ms": elapsed_ms,
        "operations": complete.operations.len(),
        "chunks": complete.chunks.len(),
        "nets": complete.nets.len(),
        "components": complete.components.len(),
        "slices": slice_keys,
        "shapes": shape_counts(&value),
        "diagnostics": diagnostics,
        "rule2_scopes_checked": scopes,
        "profile": profile_summary(&value),
        "json_bytes": text.len(),
    });
    println!("{summary}");
}

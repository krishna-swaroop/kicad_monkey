//! Analytic board materialization (`kicad_monkey.materialization.a0`).
//!
//! Resolves the physical intent of a KiCad board into analytic material
//! operations: closed 2D shapes with one composed affine, a material role, a
//! stack slice, and a resolved net. The producer never runs Boolean,
//! offsetting, or tessellation work; removed material is expressed as
//! `subtract` operations that consumers resolve. Design:
//! `docs/design/rust-pcb-analytic-materializer.html`.
//!
//! Implemented products: copper (tracks, track arcs, via lands, stored zone
//! fills, pad lands), holes (vias and pads, with protection and
//! back-drill), and dielectric (the closed board profile per dielectric
//! slice, minus crossing holes), with the full stack, nets, binding, and
//! component records. Other products are reported as not yet materialized.

mod chunking;
mod dielectric;
mod emit;
mod flash;
mod geometry;
mod holes;
mod layers;
mod nets;
mod pads;
mod producer;
mod profile;
mod routing;
mod scope;
mod sexpr_view;
mod stack;

pub use chunking::{
    chunk_operations, operation_bounds, operation_net, operation_product, operation_slice,
};
pub use scope::{filter_document, scope_from_request};
pub use sexpr_view::materialization_sexpr;

use kicad_monkey_contracts::generated::materialization_document::{
    DiagnosticSeverity, MaterialProduct, MaterializationCoordinateSystem,
    MaterializationDiagnostic, MaterializationDocumentA0, MaterializationProducer,
    MaterializationSource, Stack,
};
use kicad_monkey_contracts::generated::materialization_request::MaterializationRequestA0;
use sha2::{Digest, Sha256};
use std::fmt;
use std::fmt::Write as _;

/// Schema identifier written into every materialization document.
pub const MATERIALIZATION_SCHEMA: &str = "kicad_monkey.materialization.a0";
/// Document type identifier.
pub const MATERIALIZATION_DOCUMENT_TYPE: &str = "kicad_monkey.materialization.document";
/// Request type identifier.
pub const MATERIALIZATION_REQUEST_TYPE: &str = "kicad_monkey.materialization.request";
/// Producer name recorded in documents.
pub const MATERIALIZATION_PRODUCER: &str = "kicad-monkey-core.board_materialization";

/// Structured materialization failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializationError {
    pub code: &'static str,
    pub message: String,
}

impl fmt::Display for MaterializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MaterializationError {}

pub(crate) fn error(code: &'static str, message: impl Into<String>) -> MaterializationError {
    MaterializationError {
        code,
        message: message.into(),
    }
}

/// Products this producer emits; the rest are reported as diagnostics.
const IMPLEMENTED: [MaterialProduct; 4] = [
    MaterialProduct::Copper,
    MaterialProduct::Holes,
    MaterialProduct::Dielectric,
    MaterialProduct::Components,
];

/// Materialize one board source for `request`. The result is valid against
/// the a0 contract and equals the complete materialization filtered by the
/// request's scope.
pub fn materialize_board(
    source: &str,
    request: &MaterializationRequestA0,
) -> Result<MaterializationDocumentA0, MaterializationError> {
    validate_request(source, request)?;
    let scope = scope_from_request(request);
    let emitted = producer::emit_board(source, &scope.products)?;
    let limit = usize::try_from(request.max_operations).unwrap_or(usize::MAX);
    if emitted.emitter.operations.len() > limit {
        return Err(error(
            "too_many_operations",
            format!("board needs more than max_operations ({limit})"),
        ));
    }
    let mut diagnostics = emitted.emitter.diagnostics;
    diagnostics.extend(
        scope
            .products
            .iter()
            .filter(|product| !IMPLEMENTED.contains(product))
            .map(|product| MaterializationDiagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "product_not_materialized".to_owned(),
                message: format!("product {product} is not materialized by this producer yet"),
                product: Some(*product),
                source_uid: None,
            }),
    );
    let order = emitted
        .stack
        .slices
        .iter()
        .map(|slice| (slice.key.clone(), slice.order))
        .collect();
    let chunked = chunk_operations(emitted.emitter.operations, &order, *scope.tile_size_nm);
    let mut document = empty_document(source, request, diagnostics);
    document.stack = emitted.stack;
    document.nets = emitted.nets;
    document.components = emitted.components;
    document.binding = remap_binding(emitted.emitter.binding, &chunked.new_index_of_input);
    document.operations = chunked.operations;
    document.chunks = chunked.chunks;
    Ok(filter_document(&document, &scope))
}

fn remap_binding(
    mut binding: Vec<kicad_monkey_contracts::generated::materialization_document::BindingEntry>,
    new_index_of_input: &[u32],
) -> Vec<kicad_monkey_contracts::generated::materialization_document::BindingEntry> {
    for entry in &mut binding {
        if let Some(index) = usize::try_from(entry.operation_index)
            .ok()
            .and_then(|input| new_index_of_input.get(input))
        {
            entry.operation_index = *index;
        }
    }
    binding.sort_by_key(|entry| entry.operation_index);
    binding
}

fn validate_request(
    source: &str,
    request: &MaterializationRequestA0,
) -> Result<(), MaterializationError> {
    if request.type_ != MATERIALIZATION_REQUEST_TYPE || request.version != "a0" {
        return Err(error(
            "unsupported_request",
            "materialization request type and version must match a0",
        ));
    }
    if request.products.is_empty() {
        return Err(error("empty_scope", "a request names at least one product"));
    }
    if *request.tile_size_nm <= 0 || *request.tile_size_nm > i64::from(i32::MAX) {
        return Err(error(
            "invalid_tile_size",
            "tile size is a positive 32-bit nanometre length",
        ));
    }
    let limit: u64 = request
        .max_source_bytes
        .parse()
        .map_err(|_| error("invalid_limit", "max_source_bytes is a decimal uint64"))?;
    if u64::try_from(source.len()).map_or(true, |length| length > limit) {
        return Err(error(
            "source_too_large",
            format!("source exceeds max_source_bytes ({limit})"),
        ));
    }
    Ok(())
}

fn empty_document(
    source: &str,
    request: &MaterializationRequestA0,
    diagnostics: Vec<MaterializationDiagnostic>,
) -> MaterializationDocumentA0 {
    MaterializationDocumentA0 {
        schema: MATERIALIZATION_SCHEMA.to_owned(),
        type_: MATERIALIZATION_DOCUMENT_TYPE.to_owned(),
        version: "a0".to_owned(),
        producer: MaterializationProducer {
            name: MATERIALIZATION_PRODUCER.to_owned(),
            version: crate::ENGINE_VERSION.to_owned(),
        },
        source: MaterializationSource {
            sha256: sha256_hex(source.as_bytes()),
            source_bytes: source.len().to_string(),
        },
        coordinate_system: MaterializationCoordinateSystem {
            unit: "nm".to_owned(),
            x_axis: "board_right".to_owned(),
            y_axis: "board_down".to_owned(),
        },
        scope: scope_from_request(request),
        stack: Stack {
            slices: Vec::new(),
            board_thickness_nm: None,
            copper_finish: None,
        },
        nets: Vec::new(),
        net_classes: Vec::new(),
        diff_pairs: Vec::new(),
        components: Vec::new(),
        operations: Vec::new(),
        binding: Vec::new(),
        chunks: Vec::new(),
        diagnostics,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

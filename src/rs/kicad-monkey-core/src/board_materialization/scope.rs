//! Request scope and the selective-materialization oracle.
//!
//! Correctness rule 2: a selective materialization equals the complete
//! materialization filtered by the same scope. [`filter_document`] is that
//! filter. Tests compare selective producer output against it.

use super::chunking::{chunk_operations, operation_bounds, operation_net, operation_product};
use kicad_monkey_contracts::generated::materialization_document::{
    BindingEntry, HoleOperation, MaterialOperation, MaterialProduct, MaterializationDocumentA0,
    MaterializationScope, Nm, NmBounds, NonNegativeNm,
};
use kicad_monkey_contracts::generated::materialization_request::{
    MaterialProduct as RequestProduct, MaterializationRequestA0, NmBounds as RequestBounds,
};
use std::collections::{BTreeMap, BTreeSet};

const ALL_PRODUCTS: [MaterialProduct; 7] = [
    MaterialProduct::Copper,
    MaterialProduct::Holes,
    MaterialProduct::Dielectric,
    MaterialProduct::SolderMask,
    MaterialProduct::Paste,
    MaterialProduct::Silkscreen,
    MaterialProduct::Components,
];

/// Scope recorded for a request. `complete` holds only when every product is
/// requested with no slice, net, or region filter.
pub fn scope_from_request(request: &MaterializationRequestA0) -> MaterializationScope {
    let mut products: Vec<MaterialProduct> = request
        .products
        .iter()
        .copied()
        .map(document_product)
        .collect();
    products.sort();
    products.dedup();
    let mut slice_keys = request.slice_keys.clone();
    slice_keys.sort();
    slice_keys.dedup();
    let mut net_keys = request.net_keys.clone();
    net_keys.sort();
    net_keys.dedup();
    let complete = ALL_PRODUCTS
        .iter()
        .all(|product| products.contains(product))
        && slice_keys.is_empty()
        && net_keys.is_empty()
        && request.region.is_none();
    MaterializationScope {
        products,
        slice_keys,
        net_keys,
        region: request.region.as_ref().map(document_bounds),
        tile_size_nm: NonNegativeNm(*request.tile_size_nm),
        complete,
    }
}

fn document_product(product: RequestProduct) -> MaterialProduct {
    match product {
        RequestProduct::Copper => MaterialProduct::Copper,
        RequestProduct::Holes => MaterialProduct::Holes,
        RequestProduct::Dielectric => MaterialProduct::Dielectric,
        RequestProduct::SolderMask => MaterialProduct::SolderMask,
        RequestProduct::Paste => MaterialProduct::Paste,
        RequestProduct::Silkscreen => MaterialProduct::Silkscreen,
        RequestProduct::Components => MaterialProduct::Components,
    }
}

fn document_bounds(bounds: &RequestBounds) -> NmBounds {
    NmBounds {
        min_x_nm: Nm(*bounds.min_x_nm),
        min_y_nm: Nm(*bounds.min_y_nm),
        max_x_nm: Nm(*bounds.max_x_nm),
        max_y_nm: Nm(*bounds.max_y_nm),
    }
}

struct Filter<'a> {
    products: BTreeSet<MaterialProduct>,
    slices: Option<BTreeSet<u32>>,
    nets: Option<BTreeSet<&'a str>>,
    region: Option<&'a NmBounds>,
    slice_order: &'a BTreeMap<String, u32>,
}

impl Filter<'_> {
    fn keeps(&self, operation: &MaterialOperation) -> bool {
        self.products.contains(&operation_product(operation))
            && self.keeps_slice(operation)
            && self
                .nets
                .as_ref()
                .is_none_or(|nets| operation_net(operation).is_some_and(|net| nets.contains(net)))
            && self
                .region
                .is_none_or(|region| intersects(region, operation_bounds(operation)))
    }

    /// A surface keeps its own slice; a hole is kept when its span crosses
    /// any selected slice.
    fn keeps_slice(&self, operation: &MaterialOperation) -> bool {
        let Some(selected) = &self.slices else {
            return true;
        };
        match operation {
            MaterialOperation::SurfaceOperation(surface) => self
                .slice_order
                .get(&surface.slice_key)
                .is_some_and(|order| selected.contains(order)),
            MaterialOperation::HoleOperation(hole) => self.hole_crosses(hole, selected),
        }
    }

    fn hole_crosses(&self, hole: &HoleOperation, selected: &BTreeSet<u32>) -> bool {
        let start = self.slice_order.get(&hole.start_slice_key);
        let end = self.slice_order.get(&hole.end_slice_key);
        let (Some(&start), Some(&end)) = (start, end) else {
            return false;
        };
        selected
            .range(start.min(end)..=start.max(end))
            .next()
            .is_some()
    }
}

fn intersects(a: &NmBounds, b: &NmBounds) -> bool {
    *a.min_x_nm <= *b.max_x_nm
        && *b.min_x_nm <= *a.max_x_nm
        && *a.min_y_nm <= *b.max_y_nm
        && *b.min_y_nm <= *a.max_y_nm
}

/// Apply `scope` to a complete materialization: keep matching operations,
/// re-index and re-chunk them with the same rules the producer uses, remap
/// the binding table, and record `scope`.
pub fn filter_document(
    complete: &MaterializationDocumentA0,
    scope: &MaterializationScope,
) -> MaterializationDocumentA0 {
    let slice_order: BTreeMap<String, u32> = complete
        .stack
        .slices
        .iter()
        .map(|slice| (slice.key.clone(), slice.order))
        .collect();
    let filter = Filter {
        products: scope.products.iter().copied().collect(),
        slices: (!scope.slice_keys.is_empty()).then(|| {
            scope
                .slice_keys
                .iter()
                .filter_map(|key| slice_order.get(key).copied())
                .collect()
        }),
        nets: (!scope.net_keys.is_empty())
            .then(|| scope.net_keys.iter().map(String::as_str).collect()),
        region: scope.region.as_ref(),
        slice_order: &slice_order,
    };
    let mut kept_inputs = Vec::new();
    let mut kept = Vec::new();
    for (input, operation) in complete.operations.iter().enumerate() {
        if filter.keeps(operation) {
            kept_inputs.push(input);
            kept.push(operation.clone());
        }
    }
    let chunked = chunk_operations(kept, &slice_order, *scope.tile_size_nm);
    let new_index: BTreeMap<usize, u32> = kept_inputs
        .into_iter()
        .zip(chunked.new_index_of_input)
        .collect();
    let mut document = complete.clone();
    document.binding = remap_binding(&complete.binding, &new_index);
    document.operations = chunked.operations;
    document.chunks = chunked.chunks;
    if !filter.products.contains(&MaterialProduct::Components) {
        document.components.clear();
    }
    document.diagnostics.retain(|diagnostic| {
        diagnostic
            .product
            .is_none_or(|product| filter.products.contains(&product))
    });
    document.scope = scope.clone();
    document
}

fn remap_binding(binding: &[BindingEntry], new_index: &BTreeMap<usize, u32>) -> Vec<BindingEntry> {
    let mut remapped: Vec<BindingEntry> = binding
        .iter()
        .filter_map(|entry| {
            let old = usize::try_from(entry.operation_index).ok()?;
            let index = *new_index.get(&old)?;
            let mut entry = entry.clone();
            entry.operation_index = index;
            Some(entry)
        })
        .collect();
    remapped.sort_by_key(|entry| entry.operation_index);
    remapped
}

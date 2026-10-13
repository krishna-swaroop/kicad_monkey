//! Browser access to analytic materializations: produce a pack from KiCad
//! sources, open a pack, and lower its slices for an analytic renderer.

pub mod barrels;
pub mod lower;
pub mod tiles;

use kicad_monkey_contracts::generated::materialization_document::{
    BindingEntry, HoleOperation, MaterialOperation, MaterializationDocumentA0, SliceRole,
};
#[cfg(feature = "materialize")]
use kicad_monkey_contracts::generated::materialization_request::MaterializationRequestA0;
#[cfg(feature = "materialize")]
use kicad_monkey_core::board_materialization::materialize_project;
#[cfg(feature = "materialize")]
use kicad_monkey_materialization_pack::encode_pack;
use kicad_monkey_materialization_pack::{PackLimits, PackReader};
use std::collections::BTreeMap;
use wasm_bindgen::prelude::*;

fn js_error(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

/// Materialize a board (with its optional project file) for a JSON
/// `kicad_monkey.materialization.request` and return the packed encoding.
#[cfg(feature = "materialize")]
#[wasm_bindgen(js_name = materializeBoardPack)]
pub fn materialize_board_pack(
    board: &str,
    project: Option<String>,
    request_json: &str,
) -> Result<Vec<u8>, JsValue> {
    let request: MaterializationRequestA0 = serde_json::from_str(request_json).map_err(js_error)?;
    let document = materialize_project(board, project.as_deref(), &request).map_err(js_error)?;
    encode_pack(&document).map_err(js_error)
}

/// An opened pack: its header, binding, and holes, ready to lower slices.
#[wasm_bindgen]
pub struct AnalyticPack {
    bytes: Vec<u8>,
    header: MaterializationDocumentA0,
    holes: Vec<HoleOperation>,
    orders: BTreeMap<String, u32>,
}

impl AnalyticPack {
    /// Open and validate `bytes`; holes are gathered once, since they cut
    /// every slice their span crosses.
    pub fn open(bytes: Vec<u8>) -> Result<Self, String> {
        let reader = PackReader::open(&bytes, PackLimits::default()).map_err(|e| e.to_string())?;
        let mut holes = Vec::new();
        for position in 0..reader.header().chunks.len() {
            let operations = reader.chunk(position).map_err(|e| e.to_string())?;
            holes.extend(
                operations
                    .into_iter()
                    .filter_map(|operation| match operation {
                        MaterialOperation::HoleOperation(hole) => Some(hole),
                        MaterialOperation::SurfaceOperation(_) => None,
                    }),
            );
        }
        let header = reader.header().clone();
        let orders = header
            .stack
            .slices
            .iter()
            .map(|slice| (slice.key.clone(), slice.order))
            .collect();
        Ok(Self {
            bytes,
            header,
            holes,
            orders,
        })
    }

    /// Lower slice `key` (see [`lower::lower_slice`]).
    pub fn lower(&self, key: &str, tolerance_nm: f64) -> Result<lower::LoweredSlice, String> {
        let order = *self
            .orders
            .get(key)
            .ok_or_else(|| format!("unknown slice {key}"))?;
        let reader =
            PackReader::open(&self.bytes, PackLimits::default()).map_err(|e| e.to_string())?;
        let mut operations = Vec::new();
        for (position, chunk) in self.header.chunks.iter().enumerate() {
            if chunk.slice_key == key {
                operations.extend(reader.chunk(position).map_err(|e| e.to_string())?);
            }
        }
        let orders = |slice: &str| self.orders.get(slice).copied();
        let conductors = || {
            self.header
                .stack
                .slices
                .iter()
                .filter(|slice| slice.role == SliceRole::Conductor)
                .map(|slice| slice.order)
        };
        let outer = conductors().min().zip(conductors().max());
        Ok(lower::lower_slice(
            &operations,
            &self.holes,
            (key, order),
            &orders,
            outer,
            tolerance_nm,
        ))
    }

    /// The barrels of the plated holes (see [`barrels::barrels`]).
    pub fn barrels(&self) -> barrels::Barrels {
        let orders = |slice: &str| self.orders.get(slice).copied();
        barrels::barrels(&self.holes, &orders)
    }

    /// The binding entry of operation `index`, if it has one.
    pub fn binding(&self, index: u32) -> Option<&BindingEntry> {
        let binding = &self.header.binding;
        binding
            .binary_search_by_key(&index, |entry| entry.operation_index)
            .ok()
            .and_then(|position| binding.get(position))
    }

    /// The header without binding: identity, scope, stack, nets, classes,
    /// pairs, components, chunk directory, and diagnostics.
    pub fn header_without_binding(&self) -> MaterializationDocumentA0 {
        let mut header = self.header.clone();
        header.binding.clear();
        header
    }
}

#[wasm_bindgen]
impl AnalyticPack {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: Vec<u8>) -> Result<AnalyticPack, JsValue> {
        Self::open(bytes).map_err(|error| JsValue::from_str(&error))
    }

    /// The header (see [`Self::header_without_binding`]) as JSON.
    #[wasm_bindgen(js_name = headerJson)]
    pub fn header_json(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.header_without_binding()).map_err(js_error)
    }

    /// The binding entry of operation `index` as JSON, or `null`.
    #[wasm_bindgen(js_name = bindingJson)]
    pub fn binding_json(&self, index: u32) -> Result<String, JsValue> {
        serde_json::to_string(&self.binding(index)).map_err(js_error)
    }

    /// Lower slice `key`, flattening curves to within `tolerance_nm`.
    #[wasm_bindgen(js_name = lowerSlice)]
    pub fn lower_slice(
        &self,
        key: &str,
        tolerance_nm: f64,
    ) -> Result<LoweredSliceRecords, JsValue> {
        self.lower(key, tolerance_nm)
            .map(|inner| LoweredSliceRecords { inner })
            .map_err(|error| JsValue::from_str(&error))
    }
}

#[wasm_bindgen]
impl AnalyticPack {
    /// The plated holes' barrels, read by JavaScript as typed arrays.
    #[wasm_bindgen(js_name = barrelRecords)]
    pub fn barrel_records(&self) -> BarrelRecords {
        BarrelRecords {
            inner: self.barrels(),
        }
    }
}

/// The barrels of a pack: `barrels::BARREL_FLOATS` floats and
/// `barrels::BARREL_WORDS` words each.
#[wasm_bindgen]
pub struct BarrelRecords {
    inner: barrels::Barrels,
}

#[wasm_bindgen]
impl BarrelRecords {
    pub fn floats(&self) -> Vec<f32> {
        self.inner.floats.clone()
    }

    pub fn words(&self) -> Vec<u32> {
        self.inner.words.clone()
    }

    /// Plated holes whose shape a barrel cannot take.
    pub fn skipped(&self) -> u32 {
        self.inner.skipped
    }
}

/// One lowered slice, read by JavaScript as typed arrays.
#[wasm_bindgen]
pub struct LoweredSliceRecords {
    inner: lower::LoweredSlice,
}

#[wasm_bindgen]
impl LoweredSliceRecords {
    /// Shape records, `lower::SHAPE_WORDS` words each (subtracts first).
    pub fn shapes(&self) -> Vec<u32> {
        self.inner.shapes.clone()
    }

    #[wasm_bindgen(js_name = subtractShapes)]
    pub fn subtract_shapes(&self) -> u32 {
        self.inner.subtract_shapes
    }

    /// Fan triangle vertices (x, y board millimetres).
    #[wasm_bindgen(js_name = fanVertices)]
    pub fn fan_vertices(&self) -> Vec<f32> {
        self.inner.fan_vertices.clone()
    }

    /// Region cover records, `lower::COVER_WORDS` words each.
    pub fn covers(&self) -> Vec<u32> {
        self.inner.covers.clone()
    }

    /// Region batches, `lower::BATCH_WORDS` words each.
    pub fn batches(&self) -> Vec<u32> {
        self.inner.batches.clone()
    }

    /// Region contour edges, (x0, y0, x1, y1) board millimetres each.
    #[wasm_bindgen(js_name = contourEdges)]
    pub fn contour_edges(&self) -> Vec<f32> {
        self.inner.contour_edges.clone()
    }

    /// Per contour edge, its region's operation index (`lower::EDGE_SUBTRACT`
    /// set for a subtracting region).
    #[wasm_bindgen(js_name = contourEdgeOperations)]
    pub fn contour_edge_operations(&self) -> Vec<u32> {
        self.inner.contour_edge_operations.clone()
    }

    /// Counts of material drawn approximately, as JSON.
    #[wasm_bindgen(js_name = statsJson)]
    pub fn stats_json(&self) -> String {
        let stats = &self.inner.stats;
        format!(
            "{{\"region_offsets_ignored\":{},\"flat_cap_segments\":{},\"cubics_flattened\":{}}}",
            stats.region_offsets_ignored, stats.flat_cap_segments, stats.cubics_flattened
        )
    }
}

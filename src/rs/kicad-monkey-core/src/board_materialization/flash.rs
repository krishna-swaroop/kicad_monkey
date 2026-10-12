//! Effective copper layers of pads and vias with conditional flashing.
//!
//! Pads and vias that remove unused layers, keep end layers, or carry
//! per-zone layer connections flash only where KiCad's board plotter would.
//! Core's board plot facts are the authority; they are consulted only when a
//! board uses those options, and only for pad/via apertures. Presentation
//! forms are blanked first so they cannot fail the plot.

use super::MaterializationError;
use crate::board_plotter_ir::{
    BoardFootprintOperation, BoardPlotLimits, BoardPlotRecord, BoardViaOperationKind,
    board_plot_document,
};
use crate::pcb::{PcbPad, PcbVia, PcbView};
use crate::sexpr_projection::{Selector, scan_form_spans};
use std::collections::HashMap;

/// Effective copper layers keyed by source UID.
#[derive(Default)]
pub(crate) struct FlashLayers {
    pub vias: HashMap<String, Vec<String>>,
    pub pads: HashMap<String, Vec<String>>,
    pub used_plot_facts: bool,
    /// Why conditional flashing could not be resolved, when it could not.
    pub unresolved: Option<String>,
}

const PRESENTATION_HEADS: [&str; 14] = [
    "zone",
    "fp_arc",
    "fp_circle",
    "fp_curve",
    "fp_image",
    "fp_line",
    "fp_poly",
    "fp_rect",
    "fp_text",
    "fp_text_box",
    "model",
    "property",
    "embedded_files",
    "embedded_fonts",
];

fn needs_plot_facts(vias: &[PcbVia], pads: &[PcbPad]) -> bool {
    vias.iter().any(|via| {
        via.remove_unused_layers == Some(true)
            || via.keep_end_layers.is_some()
            || via.start_end_only == Some(true)
            || via.zone_layer_connections.is_some()
    }) || pads.iter().any(|pad| {
        pad.remove_unused_layers == Some(true)
            || pad.keep_end_layers.is_some()
            || pad.zone_layer_connections.is_some()
    })
}

fn blank(bytes: &mut [u8], range: std::ops::Range<usize>) {
    if let Some(span) = bytes.get_mut(range) {
        for byte in span
            .iter_mut()
            .filter(|byte| !matches!(**byte, b'\n' | b'\r'))
        {
            *byte = b' ';
        }
    }
}

fn into_source(bytes: Vec<u8>) -> Result<String, MaterializationError> {
    String::from_utf8(bytes)
        .map_err(|_| super::error("source_parse", "blanked source is not UTF-8"))
}

fn blank_top_level(
    source: &str,
    view: &PcbView<'_>,
    blank_head: impl Fn(&str) -> bool,
) -> Result<String, MaterializationError> {
    let mut bytes = source.as_bytes().to_vec();
    for span in view.top_level_forms() {
        if span.head.as_deref().is_some_and(&blank_head) {
            blank(&mut bytes, span.range.clone());
        }
    }
    into_source(bytes)
}

fn blank_heads(source: &str, heads: &[&str]) -> Result<String, MaterializationError> {
    let selector = Selector {
        heads: Some(heads.iter().map(|head| (*head).to_owned()).collect()),
        ..Selector::default()
    };
    let spans = scan_form_spans(source, &selector)
        .map_err(|error| super::error("source_parse", error.to_string()))?;
    let mut bytes = source.as_bytes().to_vec();
    for span in spans {
        blank(&mut bytes, span.range);
    }
    into_source(bytes)
}

/// Plot limits scaled to the source. Every count in a board is bounded by
/// its byte length, so these never reject a board the parser accepted,
/// while still bounding work by the input size.
fn plot_limits(source_len: usize) -> BoardPlotLimits {
    let defaults = BoardPlotLimits::default();
    let count = |default: usize| default.max(source_len);
    let bytes = |default: usize| default.max(source_len.saturating_mul(2));
    BoardPlotLimits {
        max_source_bytes: source_len.saturating_add(1).max(defaults.max_source_bytes),
        max_depth: defaults.max_depth,
        max_graphics: count(defaults.max_graphics),
        max_operations: count(defaults.max_operations),
        max_points: count(defaults.max_points),
        max_text_bytes: bytes(defaults.max_text_bytes),
        max_net_class_bytes: bytes(defaults.max_net_class_bytes),
        max_metadata_bytes: bytes(defaults.max_metadata_bytes),
        max_parse_nodes: count(defaults.max_parse_nodes),
        max_input_points: count(defaults.max_input_points),
        max_input_polygons: count(defaults.max_input_polygons),
        max_cache_polygons: count(defaults.max_cache_polygons),
        max_cache_contours: count(defaults.max_cache_contours),
    }
}

fn plot(source: &str) -> Result<crate::board_plotter_ir::BoardPlotDocument, crate::sexpr::Error> {
    board_plot_document(source, plot_limits(source.len()))
}

/// Board plot document for technical-layer graphics: routing, zones, pads,
/// and 3D models are blanked first, since they carry no technical-layer
/// graphics here. `None` when the board cannot be plotted.
pub(crate) fn presentation_plot(
    source: &str,
    view: &PcbView<'_>,
) -> Option<crate::board_plotter_ir::BoardPlotDocument> {
    let stripped = blank_top_level(source, view, |head| {
        matches!(head, "segment" | "arc" | "via" | "zone")
    })
    .ok()?;
    let stripped = blank_heads(&stripped, &["pad", "model", "zone"]).ok()?;
    plot(&stripped).ok()
}

/// Resolve flash layers, consulting plot facts only when required.
pub(crate) fn flash_layers(
    source: &str,
    view: &PcbView<'_>,
    vias: &[PcbVia],
    pads: &[PcbPad],
) -> Result<FlashLayers, MaterializationError> {
    if !needs_plot_facts(vias, pads) {
        return Ok(FlashLayers::default());
    }
    let stripped = blank_top_level(source, view, |head| {
        head == "zone"
            || head.starts_with("gr_")
            || matches!(head, "image" | "dimension" | "target")
    })?;
    let stripped = blank_heads(&stripped, &PRESENTATION_HEADS)?;
    let document = plot(&stripped).or_else(|_| {
        let without_boxes = blank_top_level(&stripped, view, |head| head == "gr_text_box")?;
        plot(&without_boxes).map_err(|error| super::error("plot_facts", error.to_string()))
    });
    // A board the plotter cannot read keeps every padstack layer, with a
    // warning, rather than failing the whole materialization.
    Ok(match document {
        Ok(document) => collect(document.records),
        Err(cause) => FlashLayers {
            unresolved: Some(cause.message),
            ..FlashLayers::default()
        },
    })
}

fn copper_only(layers: &[String]) -> Vec<String> {
    layers
        .iter()
        .filter(|layer| layer.ends_with(".Cu"))
        .cloned()
        .collect()
}

fn collect(records: Vec<BoardPlotRecord>) -> FlashLayers {
    let mut layers = FlashLayers {
        used_plot_facts: true,
        ..FlashLayers::default()
    };
    for record in records {
        match record {
            BoardPlotRecord::Via(via) => {
                let apertures = via
                    .operations
                    .iter()
                    .find(|operation| operation.kind == BoardViaOperationKind::Aperture)
                    .map(|operation| copper_only(&operation.layers))
                    .unwrap_or_default();
                layers.vias.insert(via.uuid, apertures);
            }
            BoardPlotRecord::Footprint(footprint) => {
                for operation in footprint.operations {
                    if let BoardFootprintOperation::StartBlock(block) = operation
                        && block.data_ref == "pad"
                    {
                        layers
                            .pads
                            .insert(block.data_uuid, copper_only(&block.layers));
                    }
                }
            }
            _ => {}
        }
    }
    layers
}

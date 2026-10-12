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

fn plot(source: &str) -> Result<crate::board_plotter_ir::BoardPlotDocument, crate::sexpr::Error> {
    let mut limits = BoardPlotLimits::default();
    limits.max_source_bytes = limits.max_source_bytes.max(source.len().saturating_add(1));
    limits.max_operations = limits.max_operations.max(1_000_000);
    board_plot_document(source, limits)
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
    let document = match plot(&stripped) {
        Ok(document) => document,
        Err(_) => {
            let without_boxes = blank_top_level(&stripped, view, |head| head == "gr_text_box")?;
            plot(&without_boxes).map_err(|error| super::error("plot_facts", error.to_string()))?
        }
    };
    Ok(collect(document.records))
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

//! Board materialization driver: parse, emit requested products, chunk.

use super::emit::{Emitter, Origin, source_uid};
use super::flash::{FlashLayers, flash_layers};
use super::geometry::rotation_translation;
use super::holes::{pad_hole, via_holes};
use super::nets::{net_key, net_table};
use super::pads::{PadFrame, land};
use super::routing::{track_arcs, tracks, via_lands, zone_fills};
use super::stack::CopperStack;
use super::{MaterializationError, error};
use crate::pcb::{
    PcbFamily, PcbFootprint, PcbPad, PcbSelection, PcbView, PcbZone, resolve_pad_copper_layer,
};
use kicad_monkey_contracts::generated::materialization_document::{
    BindingSourceKind, BoardSide, Component, MaterialProduct, Net, StackSlice,
};

/// Everything emitted from one board before chunking.
pub(crate) struct Emitted {
    pub slices: Vec<StackSlice>,
    pub nets: Vec<Net>,
    pub components: Vec<Component>,
    pub emitter: Emitter,
}

fn parse_error(cause: impl std::fmt::Display) -> MaterializationError {
    error("source_parse", cause.to_string())
}

fn selection() -> PcbSelection {
    [
        PcbFamily::Layers,
        PcbFamily::Nets,
        PcbFamily::Footprints,
        PcbFamily::Pads,
        PcbFamily::Segments,
        PcbFamily::Arcs,
        PcbFamily::Vias,
        PcbFamily::Zones,
    ]
    .into_iter()
    .fold(PcbSelection::none(), PcbSelection::with)
}

fn collect<T, E: std::fmt::Display>(
    items: impl Iterator<Item = Result<T, E>>,
) -> Result<Vec<T>, MaterializationError> {
    items.collect::<Result<Vec<_>, _>>().map_err(parse_error)
}

/// Copper zones only. Non-copper zones (keepouts, rule areas on technical
/// layers) never contribute copper. A zone that fails to decode is reported,
/// never silently dropped.
fn copper_zones(view: &PcbView<'_>, emitter: &mut Emitter) -> Vec<PcbZone> {
    let mut zones = Vec::new();
    for zone in view.zones() {
        match zone {
            Ok(zone) if zone.layers.iter().any(|layer| layer.ends_with(".Cu")) => zones.push(zone),
            Ok(_) => {}
            Err(cause) => emitter.warn("zone_not_decoded", cause.to_string(), None),
        }
    }
    zones
}

/// Emit `products` of the board in `source`.
pub(crate) fn emit_board(
    source: &str,
    products: &[MaterialProduct],
) -> Result<Emitted, MaterializationError> {
    let view =
        PcbView::parse_selected(source, Default::default(), selection()).map_err(parse_error)?;
    let stack = CopperStack::from_layers(&collect(view.layers())?);
    if stack.names.is_empty() {
        return Err(error("no_copper_layers", "board declares no copper layers"));
    }
    let footprints = collect(view.footprints())?;
    let pads = collect(view.pads())?;
    let segments = collect(view.segments())?;
    let arcs = collect(view.arcs())?;
    let vias = collect(view.vias())?;
    let mut emitter = Emitter {
        product: Some(MaterialProduct::Copper),
        ..Emitter::default()
    };
    let zones = copper_zones(&view, &mut emitter);
    let references = segments
        .iter()
        .map(|item| &item.net)
        .chain(arcs.iter().map(|item| &item.net))
        .chain(vias.iter().map(|item| &item.net))
        .chain(pads.iter().map(|item| &item.net))
        .chain(zones.iter().map(|item| &item.net));
    let nets = net_table(&collect(view.nets())?, references);
    let wants = |product: MaterialProduct| products.contains(&product);
    let flash = if wants(MaterialProduct::Copper) {
        flash_layers(source, &view, &vias, &pads)?
    } else {
        FlashLayers::default()
    };
    emitter.product = Some(MaterialProduct::Copper);
    if flash.used_plot_facts {
        emitter.warn(
            "plot_facts_flash_layers",
            "board plot facts resolved conditional pad and via flashing".to_owned(),
            None,
        );
    }
    if wants(MaterialProduct::Copper) {
        tracks(&segments, &stack, &mut emitter);
        track_arcs(&arcs, &stack, &mut emitter);
        via_lands(&vias, &stack, &flash, &mut emitter);
        zone_fills(&zones, &stack, &mut emitter);
    }
    if wants(MaterialProduct::Holes) {
        emitter.product = Some(MaterialProduct::Holes);
        via_holes(&vias, &stack, &mut emitter);
    }
    let board = Board {
        stack: &stack,
        flash: &flash,
        footprints: &footprints,
        copper: wants(MaterialProduct::Copper),
        holes: wants(MaterialProduct::Holes),
    };
    for pad in &pads {
        board.pad(pad, &mut emitter);
    }
    let components = if wants(MaterialProduct::Components) {
        footprints.iter().map(component).collect()
    } else {
        Vec::new()
    };
    Ok(Emitted {
        slices: stack.slices(),
        nets,
        components,
        emitter,
    })
}

struct Board<'a> {
    stack: &'a CopperStack,
    flash: &'a FlashLayers,
    footprints: &'a [PcbFootprint],
    copper: bool,
    holes: bool,
}

fn reference(footprint: &PcbFootprint) -> String {
    footprint
        .reference
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| footprint.library_link.clone())
}

impl Board<'_> {
    fn pad(&self, pad: &PcbPad, emitter: &mut Emitter) {
        let uid = source_uid("pad", pad.uuid.as_deref(), pad.source_range.start);
        let Some(footprint) = self.footprints.get(pad.footprint_index) else {
            emitter.product = None;
            emitter.warn(
                "missing_footprint",
                format!("pad references missing footprint {}", pad.footprint_index),
                Some(uid),
            );
            return;
        };
        let frame = PadFrame::new(
            pad,
            footprint.at_x.unwrap_or_default(),
            footprint.at_y.unwrap_or_default(),
            footprint.angle.unwrap_or_default(),
        );
        let origin = Origin {
            kind: BindingSourceKind::Pad,
            uid,
            refdes: Some(reference(footprint)),
            pad_number: Some(pad.number.clone()),
            net_key: net_key(&pad.net),
        };
        if self.copper && pad.kind != "np_thru_hole" {
            emitter.product = Some(MaterialProduct::Copper);
            self.lands(pad, &frame, &origin, emitter);
        }
        if self.holes {
            emitter.product = Some(MaterialProduct::Holes);
            pad_hole(pad, &frame, &origin, self.stack, emitter);
        }
    }

    fn lands(&self, pad: &PcbPad, frame: &PadFrame, origin: &Origin, emitter: &mut Emitter) {
        let layers = self
            .flash
            .pads
            .get(&origin.uid)
            .cloned()
            .unwrap_or_else(|| self.stack.expand(&pad.layers));
        for layer in layers {
            let Some(slice) = self.stack.key(&layer) else {
                continue;
            };
            match resolve_pad_copper_layer(pad, &layer) {
                Ok(resolved) => land(&resolved, frame, origin, slice, emitter),
                Err(cause) => emitter.warn(
                    "unsupported_padstack",
                    format!("pad layer {layer}: {cause}"),
                    Some(origin.uid.clone()),
                ),
            }
        }
    }
}

fn component(footprint: &PcbFootprint) -> Component {
    Component {
        refdes: reference(footprint),
        footprint: footprint.library_link.clone(),
        side: if footprint.layer.as_deref() == Some("B.Cu") {
            BoardSide::Back
        } else {
            BoardSide::Front
        },
        affine: rotation_translation(
            -footprint.angle.unwrap_or_default(),
            footprint.at_x.unwrap_or_default(),
            footprint.at_y.unwrap_or_default(),
        ),
        source_uid: source_uid(
            "footprint",
            footprint.uuid.as_deref(),
            footprint.source_range.start,
        ),
        body_bounds: None,
        body_height_nm: None,
    }
}

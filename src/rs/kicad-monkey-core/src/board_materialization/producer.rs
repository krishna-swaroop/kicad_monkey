//! Board materialization driver: parse, emit requested products, chunk.

use super::dielectric::dielectric_regions;
use super::emit::{Emitter, Origin, source_uid};
use super::flash::{FlashLayers, flash_layers};
use super::geometry::rotation_translation;
use super::holes::{HoleSpec, pad_hole, via_holes};
use super::layers::board_stack;
use super::nets::{net_key, net_table};
use super::pads::{PadFrame, land};
use super::profile::close_profile;
use super::routing::{track_arcs, tracks, via_lands, zone_fills};
use super::stack::CopperStack;
use super::{MaterializationError, error};
use crate::pcb::{
    PcbFamily, PcbFootprint, PcbPad, PcbRoutingArc, PcbSegment, PcbSelection, PcbVia, PcbView,
    PcbZone, resolve_pad_copper_layer,
};
use kicad_monkey_contracts::generated::materialization_document::{
    BindingSourceKind, BoardSide, Component, MaterialProduct, Net, Stack,
};

/// Everything emitted from one board before chunking.
pub(crate) struct Emitted {
    pub stack: Stack,
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
        PcbFamily::Setup,
        PcbFamily::Nets,
        PcbFamily::Footprints,
        PcbFamily::Pads,
        PcbFamily::Segments,
        PcbFamily::Arcs,
        PcbFamily::Vias,
        PcbFamily::Zones,
        PcbFamily::Profile,
    ]
    .into_iter()
    .fold(PcbSelection::none(), PcbSelection::with)
}

fn collect<T, E: std::fmt::Display>(
    items: impl Iterator<Item = Result<T, E>>,
) -> Result<Vec<T>, MaterializationError> {
    items.collect::<Result<Vec<_>, _>>().map_err(parse_error)
}

/// Decoded source families used by the producer.
struct Sources {
    footprints: Vec<PcbFootprint>,
    pads: Vec<PcbPad>,
    segments: Vec<PcbSegment>,
    arcs: Vec<PcbRoutingArc>,
    vias: Vec<PcbVia>,
    zones: Vec<PcbZone>,
}

/// Copper zones only. Non-copper zones never contribute copper. A zone that
/// fails to decode is reported, never silently dropped.
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

fn load(view: &PcbView<'_>, emitter: &mut Emitter) -> Result<Sources, MaterializationError> {
    emitter.product = Some(MaterialProduct::Copper);
    Ok(Sources {
        footprints: collect(view.footprints())?,
        pads: collect(view.pads())?,
        segments: collect(view.segments())?,
        arcs: collect(view.arcs())?,
        vias: collect(view.vias())?,
        zones: copper_zones(view, emitter),
    })
}

fn nets(view: &PcbView<'_>, sources: &Sources) -> Result<Vec<Net>, MaterializationError> {
    let references = sources
        .segments
        .iter()
        .map(|item| &item.net)
        .chain(sources.arcs.iter().map(|item| &item.net))
        .chain(sources.vias.iter().map(|item| &item.net))
        .chain(sources.pads.iter().map(|item| &item.net))
        .chain(sources.zones.iter().map(|item| &item.net));
    Ok(net_table(&collect(view.nets())?, references))
}

fn stack_of(
    view: &PcbView<'_>,
    copper: &CopperStack,
    emitter: &mut Emitter,
) -> Result<Stack, MaterializationError> {
    let setup = view.setup().map_err(parse_error)?;
    let thickness = view.metadata().map_err(parse_error)?.thickness;
    let built = board_stack(
        setup.as_ref().and_then(|value| value.stackup.as_ref()),
        copper,
        thickness,
    );
    if let Some((code, message)) = built.warning {
        emitter.product = None;
        emitter.warn(code, message, None);
    }
    Ok(built.stack)
}

/// Emit `products` of the board in `source`.
pub(crate) fn emit_board(
    source: &str,
    products: &[MaterialProduct],
) -> Result<Emitted, MaterializationError> {
    let view =
        PcbView::parse_selected(source, Default::default(), selection()).map_err(parse_error)?;
    let copper = CopperStack::from_layers(&collect(view.layers())?);
    if copper.names.is_empty() {
        return Err(error("no_copper_layers", "board declares no copper layers"));
    }
    let mut emitter = Emitter::default();
    let sources = load(&view, &mut emitter)?;
    let nets = nets(&view, &sources)?;
    let stack = stack_of(&view, &copper, &mut emitter)?;
    let wants = |product: MaterialProduct| products.contains(&product);
    if wants(MaterialProduct::Copper) {
        emit_copper(source, &view, &sources, &copper, &mut emitter)?;
    }
    if wants(MaterialProduct::Holes) || wants(MaterialProduct::Dielectric) {
        let holes = hole_specs(&sources, &copper, &mut emitter);
        if wants(MaterialProduct::Holes) {
            emitter.product = Some(MaterialProduct::Holes);
            holes.iter().for_each(|hole| emitter.hole(hole));
        }
        if wants(MaterialProduct::Dielectric) {
            emit_dielectric(&view, &sources, &holes, &stack, &mut emitter)?;
        }
    }
    let components = if wants(MaterialProduct::Components) {
        sources.footprints.iter().map(component).collect()
    } else {
        Vec::new()
    };
    Ok(Emitted {
        stack,
        nets,
        components,
        emitter,
    })
}

fn emit_copper(
    source: &str,
    view: &PcbView<'_>,
    sources: &Sources,
    copper: &CopperStack,
    emitter: &mut Emitter,
) -> Result<(), MaterializationError> {
    let flash = flash_layers(source, view, &sources.vias, &sources.pads)?;
    emitter.product = Some(MaterialProduct::Copper);
    if flash.used_plot_facts {
        emitter.warn(
            "plot_facts_flash_layers",
            "board plot facts resolved conditional pad and via flashing".to_owned(),
            None,
        );
    }
    tracks(&sources.segments, copper, emitter);
    track_arcs(&sources.arcs, copper, emitter);
    via_lands(&sources.vias, copper, &flash, emitter);
    zone_fills(&sources.zones, copper, emitter);
    let pads = Pads {
        copper,
        flash: &flash,
        footprints: &sources.footprints,
    };
    for pad in &sources.pads {
        if let Some((origin, frame)) = pads.placement(pad, emitter)
            && pad.kind != "np_thru_hole"
        {
            pads.lands(pad, &frame, &origin, emitter);
        }
    }
    Ok(())
}

fn hole_specs(sources: &Sources, copper: &CopperStack, emitter: &mut Emitter) -> Vec<HoleSpec> {
    emitter.product = Some(MaterialProduct::Holes);
    let mut holes = Vec::new();
    via_holes(&sources.vias, copper, &mut holes);
    let flash = FlashLayers::default();
    let pads = Pads {
        copper,
        flash: &flash,
        footprints: &sources.footprints,
    };
    for pad in &sources.pads {
        if let Some((origin, frame)) = pads.placement(pad, emitter) {
            holes.extend(pad_hole(pad, &frame, origin, copper));
        }
    }
    holes
}

fn emit_dielectric(
    view: &PcbView<'_>,
    sources: &Sources,
    holes: &[HoleSpec],
    stack: &Stack,
    emitter: &mut Emitter,
) -> Result<(), MaterializationError> {
    emitter.product = Some(MaterialProduct::Dielectric);
    let primitives = collect(view.profile_primitives())?;
    let profile = close_profile(&primitives, &sources.footprints);
    if profile.unclosed_edges > 0 {
        emitter.warn(
            "outline_not_closed",
            format!(
                "{} Edge.Cuts edges do not close into a loop",
                profile.unclosed_edges
            ),
            None,
        );
    }
    if profile.regions.is_empty() {
        emitter.warn(
            "outline_absent",
            "board has no closed Edge.Cuts profile".to_owned(),
            None,
        );
    }
    dielectric_regions(&profile.regions, holes, stack, emitter);
    Ok(())
}

struct Pads<'a> {
    copper: &'a CopperStack,
    flash: &'a FlashLayers,
    footprints: &'a [PcbFootprint],
}

fn reference(footprint: &PcbFootprint) -> String {
    footprint
        .reference
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| footprint.library_link.clone())
}

impl Pads<'_> {
    /// Origin and board placement of `pad`, or a diagnostic when its
    /// footprint is missing.
    fn placement(&self, pad: &PcbPad, emitter: &mut Emitter) -> Option<(Origin, PadFrame)> {
        let uid = source_uid("pad", pad.uuid.as_deref(), pad.source_range.start);
        let Some(footprint) = self.footprints.get(pad.footprint_index) else {
            emitter.warn(
                "missing_footprint",
                format!("pad references missing footprint {}", pad.footprint_index),
                Some(uid),
            );
            return None;
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
        Some((origin, frame))
    }

    fn lands(&self, pad: &PcbPad, frame: &PadFrame, origin: &Origin, emitter: &mut Emitter) {
        let layers = self
            .flash
            .pads
            .get(&origin.uid)
            .cloned()
            .unwrap_or_else(|| self.copper.expand(&pad.layers));
        for layer in layers {
            let Some(slice) = self.copper.key(&layer) else {
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

//! Board materialization driver: parse, emit requested products, chunk.

use super::apertures::BoardMargins;
use super::dielectric::dielectric_regions;
use super::emit::{Emitter, Origin, source_uid};
use super::flash::{FlashLayers, flash_layers, presentation_plot};
use super::geometry::rotation_translation;
use super::holes::{HoleSpec, pad_hole, via_holes};
use super::layers::board_stack;
use super::nets::{NetNames, net_table};
use super::pads::{PadFrame, land, npth_without_copper, place_pad, reference};
use super::profile::close_profile;
use super::routing::{route_copper, routes, via_lands, zone_fills};
use super::source_graphics::copper_graphics;
use super::stack::CopperStack;
use super::technical::{
    FootprintTextFacts, SourceGraphics, TECHNICAL_LAYERS, TechnicalBoard, TextContext,
    technical_graphics,
};
use super::{MaterializationError, error};
use crate::pcb::{
    PcbFamily, PcbFootprint, PcbFootprintGraphic, PcbGraphic, PcbGraphicKind, PcbPad,
    PcbRoutingArc, PcbSegment, PcbSelection, PcbSetup, PcbVia, PcbView, PcbZone,
    resolve_pad_copper_layer,
};
use crate::sexpr::Sexp;
use kicad_monkey_contracts::generated::materialization_document::{
    BoardSide, Component, MaterialProduct, Net, PlanarRegion2d, Stack,
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
        PcbFamily::FootprintProperties,
        PcbFamily::FootprintGraphics,
        PcbFamily::Graphics,
        PcbFamily::FootprintTexts,
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
    technical_zones: Vec<PcbZone>,
}

/// Copper zones, and zones on technical layers. Other zones never
/// contribute material. A zone that fails to decode is reported, never
/// silently dropped.
fn split_zones(view: &PcbView<'_>, emitter: &mut Emitter) -> (Vec<PcbZone>, Vec<PcbZone>) {
    let (mut copper, mut technical) = (Vec::new(), Vec::new());
    for zone in view.zones() {
        match zone {
            Ok(zone) if zone.layers.iter().any(|layer| layer.ends_with(".Cu")) => copper.push(zone),
            Ok(zone)
                if zone
                    .layers
                    .iter()
                    .any(|layer| TECHNICAL_LAYERS.contains(&layer.as_str())) =>
            {
                technical.push(zone);
            }
            Ok(_) => {}
            Err(cause) => emitter.warn("zone_not_decoded", cause.to_string(), None),
        }
    }
    (copper, technical)
}

fn load(view: &PcbView<'_>, emitter: &mut Emitter) -> Result<Sources, MaterializationError> {
    emitter.product = Some(MaterialProduct::Copper);
    let (zones, technical_zones) = split_zones(view, emitter);
    Ok(Sources {
        footprints: collect(view.footprints())?,
        pads: collect(view.pads())?,
        segments: collect(view.segments())?,
        arcs: collect(view.arcs())?,
        vias: collect(view.vias())?,
        zones,
        technical_zones,
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
    setup: Option<&PcbSetup>,
    copper: &CopperStack,
    emitter: &mut Emitter,
) -> Result<Stack, MaterializationError> {
    let thickness = view.metadata().map_err(parse_error)?.thickness;
    let built = board_stack(
        setup.and_then(|value| value.stackup.as_ref()),
        copper,
        thickness,
    );
    if let Some((code, message)) = built.warning {
        emitter.product = None;
        emitter.warn(code, message, None);
    }
    Ok(built.stack)
}

/// First board format that stores footprint children in the library frame
/// with a `(transform ...)` that may scale or mirror them.
const FIRST_FP_AFFINE_TRANSFORM: i64 = 20_260_616;

/// Boards from that format on may carry footprint scale or mirroring, which
/// the producer does not apply yet; only rotation and translation are.
fn warn_footprint_transforms(view: &PcbView<'_>, emitter: &mut Emitter) {
    let version = view.metadata().map_or(0, |metadata| metadata.version);
    if version >= FIRST_FP_AFFINE_TRANSFORM {
        emitter.product = None;
        emitter.warn(
            "footprint_transform_partial",
            format!(
                "board format {version} may scale or mirror footprints; only rotation and \
                 translation are applied"
            ),
            None,
        );
    }
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
    warn_footprint_transforms(&view, &mut emitter);
    let sources = load(&view, &mut emitter)?;
    let nets = nets(&view, &sources)?;
    let setup = view.setup().map_err(parse_error)?;
    let stack = stack_of(&view, setup.as_ref(), &copper, &mut emitter)?;
    let wants = |product: MaterialProduct| products.contains(&product);
    let graphics = if wants(MaterialProduct::Copper)
        || TECHNICAL_PRODUCTS.iter().any(|product| wants(*product))
    {
        source_graphics(&view)?
    } else {
        (Vec::new(), Vec::new())
    };
    if wants(MaterialProduct::Copper) {
        emit_copper(source, &view, &sources, &copper, &mut emitter)?;
        let names = NetNames::new(&collect(view.nets())?, &nets);
        copper_graphics(
            &graphics.0,
            &graphics.1,
            &sources.footprints,
            &copper,
            &names,
            &mut emitter,
        );
    }
    let regions = profile_regions(&view, &sources, products, &mut emitter)?;
    if wants(MaterialProduct::Holes) || wants(MaterialProduct::Dielectric) {
        let holes = hole_specs(&sources, &copper, &mut emitter);
        if wants(MaterialProduct::Holes) {
            emitter.product = Some(MaterialProduct::Holes);
            holes.iter().for_each(|hole| emitter.hole(hole));
        }
        if wants(MaterialProduct::Dielectric) {
            emitter.product = Some(MaterialProduct::Dielectric);
            dielectric_regions(&regions, &holes, &stack, &mut emitter);
        }
    }
    let board = Board {
        source,
        view: &view,
        sources: &sources,
        setup: setup.as_ref(),
        stack: &stack,
        regions: &regions,
        graphics: &graphics,
    };
    emit_technical(&board, products, &mut emitter);
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
    route_copper(&routes(&sources.segments, &sources.arcs), copper, emitter);
    via_lands(&sources.vias, copper, &flash, emitter);
    zone_fills(&sources.zones, copper, emitter);
    let pads = Pads {
        copper,
        flash: &flash,
        footprints: &sources.footprints,
    };
    for pad in &sources.pads {
        if let Some((origin, frame)) = pads.placement(pad, emitter) {
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

/// Closed board profile, computed once when the dielectric or the solder
/// mask needs it. Outline problems are reported for each of those products.
fn profile_regions(
    view: &PcbView<'_>,
    sources: &Sources,
    products: &[MaterialProduct],
    emitter: &mut Emitter,
) -> Result<Vec<PlanarRegion2d>, MaterializationError> {
    let users: Vec<MaterialProduct> = [MaterialProduct::Dielectric, MaterialProduct::SolderMask]
        .into_iter()
        .filter(|product| products.contains(product))
        .collect();
    if users.is_empty() {
        return Ok(Vec::new());
    }
    let primitives = collect(view.profile_primitives())?;
    let profile = close_profile(&primitives, &sources.footprints);
    for product in users {
        emitter.product = Some(product);
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
    }
    Ok(profile.regions)
}

/// Parsed board state shared by the technical products.
struct Board<'a> {
    source: &'a str,
    view: &'a PcbView<'a>,
    sources: &'a Sources,
    setup: Option<&'a PcbSetup>,
    stack: &'a Stack,
    regions: &'a [PlanarRegion2d],
    graphics: &'a (Vec<PcbGraphic>, Vec<PcbFootprintGraphic>),
}

const TECHNICAL_PRODUCTS: [MaterialProduct; 3] = [
    MaterialProduct::SolderMask,
    MaterialProduct::Paste,
    MaterialProduct::Silkscreen,
];

/// Board and footprint graphics for the technical products.
fn source_graphics(
    view: &PcbView<'_>,
) -> Result<(Vec<PcbGraphic>, Vec<PcbFootprintGraphic>), MaterializationError> {
    Ok((
        collect(view.graphics())?,
        collect(view.footprint_graphics())?,
    ))
}

/// Footprint text facts the plot document does not carry. Texts that fail
/// to decode keep KiCad's default (kept upright).
/// Whether a board text's `(layer ... knockout)` form carries the flag,
/// which the plot document keeps only for cached outline text.
fn layer_has_knockout(source: &str, graphic: &PcbGraphic) -> bool {
    let Some(text) = source.get(graphic.source_range.clone()) else {
        return false;
    };
    let Ok(Sexp::List(items)) = crate::sexpr::parse(text) else {
        return false;
    };
    items.iter().any(|item| match item {
        Sexp::List(layer) => {
            matches!(layer.first(), Some(Sexp::Atom(head)) if head == "layer")
                && layer
                    .iter()
                    .any(|value| matches!(value, Sexp::Atom(flag) if flag == "knockout"))
        }
        _ => false,
    })
}

fn text_context(source: &str, view: &PcbView<'_>, graphics: &[PcbGraphic]) -> TextContext {
    let texts = view.footprint_texts().flatten().filter_map(|text| {
        let facts = FootprintTextFacts {
            unlocked: text.unlocked,
            justify: text.effects.justify,
        };
        Some((text.uuid?, facts))
    });
    let properties = view
        .footprint_properties()
        .flatten()
        .filter_map(|property| {
            let facts = FootprintTextFacts {
                unlocked: property.unlocked,
                justify: property.effects.justify,
            };
            Some((property.uuid?, facts))
        });
    TextContext {
        version: view.metadata().map_or(0, |metadata| metadata.version),
        texts: texts.chain(properties).collect(),
        board_knockout: graphics
            .iter()
            .filter(|graphic| graphic.kind == PcbGraphicKind::Text)
            .filter(|graphic| layer_has_knockout(source, graphic))
            .filter_map(|graphic| graphic.uuid.clone())
            .collect(),
    }
}

fn emit_technical(board: &Board<'_>, products: &[MaterialProduct], emitter: &mut Emitter) {
    let wanted: Vec<MaterialProduct> = TECHNICAL_PRODUCTS
        .into_iter()
        .filter(|product| products.contains(product))
        .collect();
    if wanted.is_empty() {
        return;
    }
    let context = text_context(board.source, board.view, &board.graphics.0);
    let document = presentation_plot(board.source, board.view);
    let margins = BoardMargins::from_setup(board.setup, context.version);
    let graphics = SourceGraphics {
        board: &board.graphics.0,
        footprint: &board.graphics.1,
        footprints: &board.sources.footprints,
        zones: &board.sources.technical_zones,
        mask_margin_nm: margins.mask_nm,
    };
    let placed = technical_graphics(&graphics, document.as_ref(), &context);
    let technical = TechnicalBoard {
        stack: board.stack,
        regions: board.regions,
        margins: &margins,
        footprints: &board.sources.footprints,
        pads: &board.sources.pads,
        vias: &board.sources.vias,
        routes: &routes(&board.sources.segments, &board.sources.arcs),
        graphics: &placed,
        text_plotted: document.is_some(),
    };
    for product in wanted {
        technical.emit(product, emitter);
    }
}

struct Pads<'a> {
    copper: &'a CopperStack,
    flash: &'a FlashLayers,
    footprints: &'a [PcbFootprint],
}

impl Pads<'_> {
    /// Origin and board placement of `pad`, or a diagnostic when its
    /// footprint is missing.
    fn placement(&self, pad: &PcbPad, emitter: &mut Emitter) -> Option<(Origin, PadFrame)> {
        place_pad(pad, self.footprints, emitter).map(|placed| (placed.origin, placed.frame))
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
                Ok(resolved) if npth_without_copper(pad, &resolved) => {}
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

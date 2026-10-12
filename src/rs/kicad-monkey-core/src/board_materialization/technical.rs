//! Solder mask, paste, and silkscreen products.
//!
//! - Solder mask: per side, the closed board profile is added; pad openings
//!   grown by the resolved margin, untented via openings, and mask-layer
//!   graphics, text, and zones are subtracted.
//! - Paste: per side, pad apertures after the resolved margin and ratio,
//!   plus paste-layer graphics, text, and zones.
//! - Silkscreen: per side, silkscreen graphics, text, and zones as
//!   authored. Clipping silkscreen by mask openings is a fabrication step
//!   and is left to consumers.

use super::apertures::{
    BoardMargins, aperture, mask_margin, on_layer, paste_margin, via_opening_nm,
};
use super::emit::{Emitter, Material, Origin, source_uid};
use super::geometry::nm;
use super::geometry::{identity, origin as zero, rotation_translation};
use super::graphics::{Drawn, TextSpec, operation_shapes, text_shapes};
use super::nets::net_key;
use super::pads::reference;
use super::pads::{pad_shapes, place_pad};
use super::routing::polyline_contour;
use super::source_graphics::{graphic_layers, graphic_shapes};
use crate::board_plotter_ir::{
    BoardDimensionOperation, BoardFootprintOperation, BoardFootprintRecord, BoardPlotDocument,
    BoardPlotRecord, BoardTableOperation, BoardTextBoxOperation, BoardTextOperation,
};
use crate::pcb::{
    PcbFootprint, PcbFootprintGraphic, PcbGraphic, PcbPad, PcbVia, PcbZone,
    resolve_pad_copper_layer,
};
use crate::plotter_types::PlotterOperation;
use kicad_monkey_contracts::generated::materialization_document::{
    Affine2D, BindingSourceKind, Disk2d, MaterialAction, MaterialProduct, NonNegativeNm,
    PlanarRegion2d, Shape2d, Stack, SurfaceMaterial,
};
use std::collections::{HashMap, HashSet};

/// Technical layers in KiCad naming, front then back.
pub(crate) const TECHNICAL_LAYERS: [&str; 6] = [
    "F.SilkS", "B.SilkS", "F.Mask", "B.Mask", "F.Paste", "B.Paste",
];

/// One side of one technical product.
struct Side {
    layer: &'static str,
    slice: &'static str,
    copper: &'static str,
    front: bool,
}

fn sides(product: MaterialProduct) -> [Side; 2] {
    let (front_layer, back_layer, front_slice, back_slice) = match product {
        MaterialProduct::SolderMask => ("F.Mask", "B.Mask", "mask.front", "mask.back"),
        MaterialProduct::Paste => ("F.Paste", "B.Paste", "paste.front", "paste.back"),
        _ => ("F.SilkS", "B.SilkS", "silkscreen.front", "silkscreen.back"),
    };
    [
        Side {
            layer: front_layer,
            slice: front_slice,
            copper: "F.Cu",
            front: true,
        },
        Side {
            layer: back_layer,
            slice: back_slice,
            copper: "B.Cu",
            front: false,
        },
    ]
}

fn material(product: MaterialProduct, action: MaterialAction) -> Material {
    Material {
        material: match product {
            MaterialProduct::SolderMask => SurfaceMaterial::SolderMask,
            MaterialProduct::Paste => SurfaceMaterial::Paste,
            _ => SurfaceMaterial::Silkscreen,
        },
        action,
        net_key: None,
    }
}

/// Graphics, text, and zone shapes on technical layers, drawn once per
/// board and shared by every technical product.
pub(crate) struct PlacedGraphic {
    pub layer: String,
    pub origin: Origin,
    pub affine: Affine2D,
    pub drawn: Drawn,
    /// Round offset applied to every shape (copper-shape mask openings).
    pub offset_nm: i32,
}

fn unbound(kind: BindingSourceKind, uid: String) -> Origin {
    Origin::routed(kind, uid, String::new())
}

fn record_uid(kind: &str, uuid: &str, position: usize) -> String {
    if uuid.is_empty() {
        format!("{kind}#{position}")
    } else {
        uuid.to_owned()
    }
}

fn technical(layer: &str) -> bool {
    TECHNICAL_LAYERS.contains(&layer)
}

fn push(
    out: &mut Vec<PlacedGraphic>,
    layer: &str,
    origin: Origin,
    affine: Affine2D,
    draw: impl FnOnce(&mut Drawn),
) {
    let mut drawn = Drawn::default();
    draw(&mut drawn);
    out.push(PlacedGraphic {
        layer: layer.to_owned(),
        origin,
        affine,
        drawn,
        offset_nm: 0,
    });
}

/// Board facts that footprint stroke text placement depends on.
pub(crate) struct TextContext {
    /// Board file format version (`(version ...)`).
    pub version: i64,
    /// Source facts of footprint texts and properties, by UUID.
    pub texts: HashMap<String, FootprintTextFacts>,
    /// UUIDs of board texts authored as knockout.
    pub board_knockout: HashSet<String>,
}

/// What KiCad needs from a footprint text's source to place stroke glyphs.
pub(crate) struct FootprintTextFacts {
    pub unlocked: bool,
    pub justify: Vec<String>,
}

/// First format version whose footprint text angles are footprint-relative;
/// older files store the board-frame angle (`FIRST_FP_AFFINE_TRANSFORM`).
const FIRST_FP_AFFINE_TRANSFORM: i64 = 20_260_616;

/// Footprint text without a render cache, placed in the board frame.
fn footprint_stroke_text(
    operation: &BoardTextOperation,
    record: &BoardFootprintRecord,
    facts: Option<&FootprintTextFacts>,
    context: &TextContext,
    drawn: &mut Drawn,
) {
    let placement = &record.placement;
    let angle = placement.angle_deg.to_radians();
    let (x, y) = (operation.x as f64, operation.y as f64);
    let board_x = x * angle.cos() + y * angle.sin() + placement.x_nm as f64;
    let board_y = y * angle.cos() - x * angle.sin() + placement.y_nm as f64;
    let text_angle = if context.version < FIRST_FP_AFFINE_TRANSFORM {
        operation.orient_deg
    } else {
        operation.orient_deg + placement.angle_deg
    };
    let spec = TextSpec::from_board(operation)
        .format(context.version)
        .on_board(
            board_x.round() as i64,
            board_y.round() as i64,
            text_angle,
            facts.is_some_and(|facts| facts.unlocked),
        )
        .justified(facts.map_or(&[][..], |facts| &facts.justify));
    text_shapes(&spec, drawn);
}

fn footprint_graphics(
    record: &BoardFootprintRecord,
    context: &TextContext,
    out: &mut Vec<PlacedGraphic>,
) {
    let placement = rotation_translation(
        -record.placement.angle_deg,
        record.placement.x_nm as f64 / 1_000_000.0,
        record.placement.y_nm as f64 / 1_000_000.0,
    );
    for (position, operation) in record.operations.iter().enumerate() {
        let BoardFootprintOperation::Text {
            operation,
            metadata,
        } = operation
        else {
            continue;
        };
        let Some(layer) = metadata
            .extra_attrs
            .layer_name
            .as_deref()
            .filter(|layer| technical(layer))
        else {
            continue;
        };
        let uid = if metadata.data_uuid.is_empty() {
            format!("{}#{position}", record.uuid)
        } else {
            metadata.data_uuid.clone()
        };
        let mut origin = unbound(BindingSourceKind::FootprintText, uid);
        origin.refdes = Some(record.reference.clone());
        push(out, layer, origin, placement.clone(), |drawn| {
            if operation.render_cache.is_none() {
                let facts = context.texts.get(&metadata.data_uuid);
                footprint_stroke_text(operation, record, facts, context, drawn);
            } else {
                let spec = TextSpec::from_board(operation).format(context.version);
                text_shapes(&spec, drawn);
            }
        });
    }
}

/// One plot operation of a text box, dimension, or table record.
enum Part<'a> {
    Geometry(&'a PlotterOperation),
    Text(&'a BoardTextOperation),
}

/// Draw a record made of geometry and text parts on `layer`.
fn mixed(
    out: &mut Vec<PlacedGraphic>,
    layer: &str,
    origin: Origin,
    parts: Vec<Part<'_>>,
    version: i64,
) {
    push(out, layer, origin, identity(), |drawn| {
        for part in parts {
            match part {
                Part::Geometry(op) => operation_shapes(op, drawn),
                Part::Text(op) => text_shapes(&TextSpec::from_board(op).format(version), drawn),
            }
        }
    });
}

/// The technical layer, UID prefix, and parts of a text box, dimension, or
/// table record.
fn mixed_parts(record: &BoardPlotRecord) -> Option<(&str, &str, &str, Vec<Part<'_>>)> {
    match record {
        BoardPlotRecord::TextBox(value) if technical(&value.layer) => {
            let parts = value.operations.iter().map(|op| match op {
                BoardTextBoxOperation::Border(op) => Part::Geometry(op),
                BoardTextBoxOperation::Text(op) => Part::Text(op),
            });
            Some((
                value.layer.as_str(),
                "text_box",
                value.uuid.as_str(),
                parts.collect(),
            ))
        }
        BoardPlotRecord::Dimension(value) => {
            let layer = value.layers.iter().find(|layer| technical(layer))?;
            let parts = value.operations.iter().map(|op| match op {
                BoardDimensionOperation::Geometry(op) => Part::Geometry(op),
                BoardDimensionOperation::Text(op) => Part::Text(op),
            });
            Some((
                layer.as_str(),
                "dimension",
                value.uuid.as_str(),
                parts.collect(),
            ))
        }
        BoardPlotRecord::Table(value) => {
            let layer = value.layers.iter().find(|layer| technical(layer))?;
            let parts = value.operations.iter().map(|op| match op {
                BoardTableOperation::Segment(op) => Part::Geometry(op),
                BoardTableOperation::Text(op) => Part::Text(op),
            });
            Some((
                layer.as_str(),
                "table",
                value.uuid.as_str(),
                parts.collect(),
            ))
        }
        _ => None,
    }
}

fn board_graphics(
    record: &BoardPlotRecord,
    position: usize,
    context: &TextContext,
    out: &mut Vec<PlacedGraphic>,
) {
    match record {
        BoardPlotRecord::Text(value) if technical(&value.layer) => {
            let uid = record_uid("text", &value.uuid, position);
            let origin = unbound(BindingSourceKind::BoardText, uid);
            let knockout = context.board_knockout.contains(&value.uuid);
            push(out, &value.layer, origin, identity(), |drawn| {
                for op in &value.operations {
                    let spec = TextSpec::from_board(op)
                        .format(context.version)
                        .knockout(knockout);
                    text_shapes(&spec, drawn);
                }
            });
        }
        BoardPlotRecord::Footprint(value) => footprint_graphics(value, context, out),
        _ => {
            if let Some((layer, kind, uuid, parts)) = mixed_parts(record) {
                let source = if kind == "text_box" {
                    BindingSourceKind::BoardText
                } else {
                    BindingSourceKind::BoardGraphic
                };
                let origin = unbound(source, record_uid(kind, uuid, position));
                mixed(out, layer, origin, parts, context.version);
            }
        }
    }
}

/// Stored fills of zones on technical layers.
fn zone_graphics(zones: &[PcbZone], out: &mut Vec<PlacedGraphic>) {
    for zone in zones {
        let uid = source_uid("zone", zone.uuid.as_deref(), zone.source_range.start);
        for polygon in &zone.filled_polygons {
            let layer = match (polygon.layer.is_empty(), zone.layers.as_slice()) {
                (true, [only]) => only.as_str(),
                _ => polygon.layer.as_str(),
            };
            if !technical(layer) {
                continue;
            }
            let points = polygon
                .points
                .iter()
                .map(|p| super::geometry::pcb_point(*p));
            let origin =
                Origin::routed(BindingSourceKind::ZoneFill, uid.clone(), net_key(&zone.net));
            push(out, layer, origin, identity(), |drawn| {
                if let Some(contour) = polyline_contour(points.collect()) {
                    drawn.shapes.push(Shape2d::PlanarRegion2d(PlanarRegion2d {
                        kind: "planar_region_2d".to_owned(),
                        outer: contour,
                        holes: Vec::new(),
                    }));
                }
            });
        }
    }
}

/// Parsed board and footprint graphics, the source of every non-text
/// technical shape.
pub(crate) struct SourceGraphics<'a> {
    pub board: &'a [PcbGraphic],
    pub footprint: &'a [PcbFootprintGraphic],
    pub footprints: &'a [PcbFootprint],
    pub zones: &'a [PcbZone],
    /// Board solder-mask expansion for copper shapes without their own.
    pub mask_margin_nm: i64,
}

/// Mask side of an outer copper layer.
fn mask_of(layer: &str) -> Option<&'static str> {
    match layer {
        "F.Cu" => Some("F.Mask"),
        "B.Cu" => Some("B.Mask"),
        _ => None,
    }
}

/// One graphic on its technical layers. An outer-copper shape that is also
/// on its mask layer, or carries `solder_mask_margin`, opens the mask once,
/// grown by its own margin or else the board's (`PCB_SHAPE::
/// GetSolderMaskExpansion`); its raw mask-layer copy is not drawn again.
fn source_graphic(
    graphic: &PcbGraphic,
    origin: &Origin,
    affine: &Affine2D,
    mask_margin_nm: i64,
    out: &mut Vec<PlacedGraphic>,
) {
    let layers = graphic_layers(graphic);
    let exposes = |mask: &str| {
        layers.iter().any(|layer| mask_of(layer) == Some(mask))
            && (layers.contains(&mask) || graphic.solder_mask_margin.is_some())
    };
    for &layer in &layers {
        if technical(layer) && !(layer.ends_with(".Mask") && exposes(layer)) {
            push(out, layer, origin.clone(), affine.clone(), |drawn| {
                graphic_shapes(graphic, layer, drawn);
            });
        }
        let Some(mask) = mask_of(layer).filter(|mask| exposes(mask)) else {
            continue;
        };
        let margin = graphic
            .solder_mask_margin
            .map_or(mask_margin_nm, |value| i64::from(nm(value)));
        let mut drawn = Drawn::default();
        graphic_shapes(graphic, layer, &mut drawn);
        out.push(PlacedGraphic {
            layer: mask.to_owned(),
            origin: origin.clone(),
            affine: affine.clone(),
            drawn,
            offset_nm: i32::try_from(margin).unwrap_or_default(),
        });
    }
}

fn source_graphics(graphics: &SourceGraphics<'_>, out: &mut Vec<PlacedGraphic>) {
    for (position, graphic) in graphics.board.iter().enumerate() {
        let uid = record_uid(
            "graphic",
            graphic.uuid.as_deref().unwrap_or_default(),
            position,
        );
        let origin = unbound(BindingSourceKind::BoardGraphic, uid);
        source_graphic(graphic, &origin, &identity(), graphics.mask_margin_nm, out);
    }
    for (position, owned) in graphics.footprint.iter().enumerate() {
        let Some(footprint) = graphics.footprints.get(owned.footprint_index) else {
            continue;
        };
        let graphic = &owned.graphic;
        let uid = record_uid(
            "footprint_graphic",
            graphic.uuid.as_deref().unwrap_or_default(),
            position,
        );
        let mut origin = unbound(BindingSourceKind::FootprintGraphic, uid);
        origin.refdes = Some(reference(footprint));
        let affine = rotation_translation(
            -footprint.angle.unwrap_or_default(),
            footprint.at_x.unwrap_or_default(),
            footprint.at_y.unwrap_or_default(),
        );
        source_graphic(graphic, &origin, &affine, graphics.mask_margin_nm, out);
    }
}

/// Every technical-layer graphic of the board: source graphics, then plot
/// document text, then zones.
pub(crate) fn technical_graphics(
    graphics: &SourceGraphics<'_>,
    document: Option<&BoardPlotDocument>,
    context: &TextContext,
) -> Vec<PlacedGraphic> {
    let mut out = Vec::new();
    source_graphics(graphics, &mut out);
    for (position, record) in document
        .into_iter()
        .flat_map(|doc| doc.records.iter().enumerate())
    {
        board_graphics(record, position, context, &mut out);
    }
    zone_graphics(graphics.zones, &mut out);
    out
}

/// Emit one graphic: its shapes with `action`, and knockout cuts removed
/// from an additive layer. On the mask, where graphics are openings, a
/// knockout would invert, so it is reported instead.
fn emit_graphic(
    graphic: &PlacedGraphic,
    product: MaterialProduct,
    action: MaterialAction,
    slice: &str,
    emitter: &mut Emitter,
) {
    let drawn = &graphic.drawn;
    for (code, message) in &drawn.unsupported {
        emitter.warn(code, message.clone(), Some(graphic.origin.uid.clone()));
    }
    let has_cuts = !drawn.cuts.is_empty() || !drawn.board_cuts.is_empty();
    if has_cuts && action == MaterialAction::Subtract {
        emitter.warn(
            "knockout_on_mask_not_materialized",
            "knockout text on a solder-mask layer is not materialized".to_owned(),
            Some(graphic.origin.uid.clone()),
        );
        return;
    }
    let board = identity();
    let local = |shapes: &'_ [Shape2d]| -> Vec<(Shape2d, Affine2D)> {
        shapes
            .iter()
            .map(|shape| (shape.clone(), graphic.affine.clone()))
            .collect()
    };
    let global = |shapes: &'_ [Shape2d]| -> Vec<(Shape2d, Affine2D)> {
        shapes
            .iter()
            .map(|shape| (shape.clone(), board.clone()))
            .collect()
    };
    let passes = [
        (action, local(&drawn.shapes)),
        (action, global(&drawn.board_shapes)),
        (MaterialAction::Subtract, local(&drawn.cuts)),
        (MaterialAction::Subtract, global(&drawn.board_cuts)),
    ];
    for (action, shapes) in passes {
        for (shape, affine) in shapes {
            emitter.offset_surface(
                &graphic.origin,
                slice,
                material(product, action),
                shape,
                affine,
                graphic.offset_nm,
            );
        }
    }
}

/// Inputs shared by the technical products of one board.
pub(crate) struct TechnicalBoard<'a> {
    pub stack: &'a Stack,
    pub regions: &'a [PlanarRegion2d],
    pub margins: &'a BoardMargins,
    pub footprints: &'a [PcbFootprint],
    pub pads: &'a [PcbPad],
    pub vias: &'a [PcbVia],
    pub graphics: &'a [PlacedGraphic],
    /// Whether board and footprint text could be plotted.
    pub text_plotted: bool,
}

impl TechnicalBoard<'_> {
    fn has_slice(&self, key: &str) -> bool {
        self.stack.slices.iter().any(|slice| slice.key == key)
    }

    /// Emit `product` for both sides.
    pub(crate) fn emit(&self, product: MaterialProduct, emitter: &mut Emitter) {
        emitter.product = Some(product);
        if product == MaterialProduct::SolderMask && self.margins.mask_min_width_nm > 0 {
            emitter.warn(
                "mask_min_width_not_applied",
                format!(
                    "solder_mask_min_width {} nm is a fabrication step; webs are not merged",
                    self.margins.mask_min_width_nm
                ),
                None,
            );
        }
        if !self.text_plotted {
            emitter.warn(
                "text_not_plotted",
                "board text could not be plotted; text is not materialized".to_owned(),
                None,
            );
        }
        for side in sides(product) {
            if !self.has_slice(side.slice) {
                emitter.warn(
                    "slice_absent",
                    format!("the stack has no {} slice", side.slice),
                    None,
                );
                continue;
            }
            self.emit_side(product, &side, emitter);
        }
    }

    fn emit_side(&self, product: MaterialProduct, side: &Side, emitter: &mut Emitter) {
        if product == MaterialProduct::SolderMask {
            self.profile(side, emitter);
        }
        self.pad_openings(product, side, emitter);
        if product == MaterialProduct::SolderMask {
            self.via_openings(side, emitter);
        }
        let action = if product == MaterialProduct::SolderMask {
            MaterialAction::Subtract
        } else {
            MaterialAction::Add
        };
        for graphic in self.graphics {
            if graphic.layer == side.layer {
                emit_graphic(graphic, product, action, side.slice, emitter);
            }
        }
    }

    fn profile(&self, side: &Side, emitter: &mut Emitter) {
        for (position, region) in self.regions.iter().enumerate() {
            let origin = unbound(
                BindingSourceKind::Profile,
                format!("board_profile#{position}"),
            );
            emitter.surface(
                &origin,
                side.slice,
                material(MaterialProduct::SolderMask, MaterialAction::Add),
                Shape2d::PlanarRegion2d(region.clone()),
                identity(),
            );
        }
    }

    fn pad_openings(&self, product: MaterialProduct, side: &Side, emitter: &mut Emitter) {
        let mask = product == MaterialProduct::SolderMask;
        let action = if mask {
            MaterialAction::Subtract
        } else {
            MaterialAction::Add
        };
        for pad in self
            .pads
            .iter()
            .filter(|pad| on_layer(&pad.layers, side.layer))
        {
            let Some(placed) = place_pad(pad, self.footprints, emitter) else {
                continue;
            };
            let resolved = match resolve_pad_copper_layer(pad, side.copper) {
                Ok(resolved) => resolved,
                Err(cause) => {
                    emitter.warn(
                        "unsupported_padstack",
                        format!("pad layer {}: {cause}", side.copper),
                        Some(placed.origin.uid.clone()),
                    );
                    continue;
                }
            };
            let margin = match product {
                MaterialProduct::SolderMask => {
                    mask_margin(pad, placed.footprint, &resolved, self.margins)
                }
                MaterialProduct::Paste => {
                    paste_margin(pad, placed.footprint, &resolved, self.margins)
                }
                // A pad listing a silkscreen layer is printed as its shape.
                _ => (0, 0),
            };
            let Some(opening) = aperture(&resolved, margin) else {
                continue;
            };
            let shapes = pad_shapes(&opening.resolved, &placed.origin, side.slice, emitter);
            if opening.primitive_offset_nm < 0 && !shapes.primitives.is_empty() {
                emitter.warn(
                    "custom_pad_eroded_per_primitive",
                    "a negative margin erodes each custom-pad primitive separately".to_owned(),
                    Some(placed.origin.uid.clone()),
                );
            }
            let affine = placed
                .frame
                .affine(opening.resolved.offset.x, opening.resolved.offset.y);
            let base = shapes.base.map(|shape| (shape, opening.base_offset_nm));
            let primitives = shapes
                .primitives
                .into_iter()
                .map(|shape| (shape, opening.primitive_offset_nm));
            for (shape, offset) in base.into_iter().chain(primitives) {
                emitter.offset_surface(
                    &placed.origin,
                    side.slice,
                    material(product, action),
                    shape,
                    affine.clone(),
                    offset,
                );
            }
        }
    }

    fn via_openings(&self, side: &Side, emitter: &mut Emitter) {
        for via in self.vias {
            let Some(diameter) = via_opening_nm(via, side.front, self.margins) else {
                continue;
            };
            let uid = source_uid("via", via.uuid.as_deref(), via.source_range.start);
            let origin = Origin::routed(BindingSourceKind::Via, uid, net_key(&via.net));
            emitter.surface(
                &origin,
                side.slice,
                material(MaterialProduct::SolderMask, MaterialAction::Subtract),
                Shape2d::Disk2d(Disk2d {
                    kind: "disk_2d".to_owned(),
                    center_nm: zero(),
                    radius_nm: NonNegativeNm(diameter / 2),
                }),
                rotation_translation(0.0, via.at_x, via.at_y),
            );
        }
    }
}

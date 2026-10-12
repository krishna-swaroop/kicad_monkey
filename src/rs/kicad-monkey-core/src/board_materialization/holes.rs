//! Via and pad holes with their slice spans.
//!
//! Protection, back-drill, and plating thickness are added in slice 2.

use super::emit::{Emitter, HoleFacts, Origin, source_uid};
use super::geometry::{nm, origin, rotation_translation};
use super::nets::net_key;
use super::pads::{PadFrame, oval};
use super::stack::CopperStack;
use crate::pcb::{PcbHoleShape, PcbPad, PcbVia};
use kicad_monkey_contracts::generated::materialization_document::{
    BindingSourceKind, Disk2d, HolePlating, NonNegativeNm, Shape2d,
};

fn round_hole(diameter: f64) -> Shape2d {
    Shape2d::Disk2d(Disk2d {
        kind: "disk_2d".to_owned(),
        center_nm: origin(),
        radius_nm: NonNegativeNm(i64::from(nm(diameter / 2.0))),
    })
}

fn facts(plating: HolePlating, layers: &[String], stack: &CopperStack) -> Option<HoleFacts> {
    let start = layers
        .first()
        .and_then(|name| stack.key(name))
        .or_else(|| stack.front_key())?;
    let end = layers
        .last()
        .and_then(|name| stack.key(name))
        .or_else(|| stack.back_key())?;
    Some(HoleFacts {
        plating,
        start_slice_key: start.to_owned(),
        end_slice_key: end.to_owned(),
        plating_thickness_nm: None,
        protection: None,
        backdrills: Vec::new(),
    })
}

pub(crate) fn via_holes(vias: &[PcbVia], stack: &CopperStack, emitter: &mut Emitter) {
    for via in vias.iter().filter(|via| via.drill > 0.0) {
        let uid = source_uid("via", via.uuid.as_deref(), via.source_range.start);
        let span = stack.expand(&via.layers);
        let Some(hole) = facts(HolePlating::Plated, &span, stack) else {
            continue;
        };
        let origin = Origin::routed(BindingSourceKind::Via, uid, net_key(&via.net));
        let affine = rotation_translation(0.0, via.at_x, via.at_y);
        emitter.hole(&origin, hole, round_hole(via.drill), affine);
    }
}

/// Emit the hole of `pad`, if it has one. Plated holes span every copper
/// layer; unplated holes span the pad's own copper layers, or the whole
/// board when it lists none.
pub(crate) fn pad_hole(
    pad: &PcbPad,
    frame: &PadFrame,
    origin: &Origin,
    stack: &CopperStack,
    emitter: &mut Emitter,
) {
    let Some(drill) = pad.drill.as_ref() else {
        return;
    };
    let height = drill.height.unwrap_or(drill.width);
    if drill.width <= 0.0 || height <= 0.0 {
        return;
    }
    let plated = pad.plated.unwrap_or(pad.kind != "np_thru_hole");
    let layers = if plated {
        stack.names.clone()
    } else {
        stack.expand(&pad.layers)
    };
    let plating = if plated {
        HolePlating::Plated
    } else {
        HolePlating::Unplated
    };
    let Some(hole) = facts(plating, &layers, stack) else {
        return;
    };
    let shape = match drill.shape {
        PcbHoleShape::Oval if (drill.width - height).abs() > 1e-9 => oval(drill.width, height),
        _ => round_hole(drill.width.min(height)),
    };
    emitter.hole(
        origin,
        hole,
        shape,
        frame.affine(drill.offset.x, drill.offset.y),
    );
}

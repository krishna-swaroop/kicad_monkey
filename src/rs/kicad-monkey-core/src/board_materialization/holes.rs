//! Via and pad holes with slice spans, protection, and back-drill.
//!
//! Holes are collected as [`HoleSpec`]s so the holes product and the
//! dielectric product (which subtracts them) see the same holes. Plating
//! thickness is not stated by KiCad and stays unknown.

use super::emit::{HoleFacts, Origin, source_uid};
use super::geometry::{nm, origin, rotation_translation};
use super::nets::net_key;
use super::pads::{PadFrame, oval};
use super::stack::CopperStack;
use crate::pcb::{PcbDrillProperties, PcbFrontBackOptionalBool, PcbHoleShape, PcbPad, PcbVia};
use kicad_monkey_contracts::generated::materialization_document::{
    Affine2D, Backdrill, BindingSourceKind, BoardSide, Disk2d, HolePlating, HoleProtection,
    NonNegativeNm, ProtectionState, Shape2d, SideProtection,
};

/// One hole before it becomes an operation.
pub(crate) struct HoleSpec {
    pub origin: Origin,
    pub facts: HoleFacts,
    pub shape: Shape2d,
    pub affine: Affine2D,
}

fn round_hole(diameter: f64) -> Shape2d {
    Shape2d::Disk2d(Disk2d {
        kind: "disk_2d".to_owned(),
        center_nm: origin(),
        radius_nm: NonNegativeNm(i64::from(nm(diameter / 2.0))),
    })
}

fn state(value: Option<bool>) -> ProtectionState {
    match value {
        Some(true) => ProtectionState::Yes,
        Some(false) => ProtectionState::No,
        None => ProtectionState::Unspecified,
    }
}

fn sides(value: Option<&PcbFrontBackOptionalBool>) -> (ProtectionState, ProtectionState) {
    (
        state(value.and_then(|value| value.front)),
        state(value.and_then(|value| value.back)),
    )
}

/// Via protection exactly as authored; `unspecified` defers to the board.
fn via_protection(via: &PcbVia) -> HoleProtection {
    let (tented_front, tented_back) = sides(via.tenting.as_ref());
    let (covered_front, covered_back) = sides(via.covering.as_ref());
    let (plugged_front, plugged_back) = sides(via.plugging.as_ref());
    HoleProtection {
        front: SideProtection {
            tented: tented_front,
            covered: covered_front,
            plugged: plugged_front,
        },
        back: SideProtection {
            tented: tented_back,
            covered: covered_back,
            plugged: plugged_back,
        },
        filled: state(via.filling),
        capped: state(via.capping),
    }
}

/// Back-drill and tertiary drill records. The drilled side is the side of
/// the start layer; `until_slice_key` is the authored end layer.
fn backdrills(drills: [Option<&PcbDrillProperties>; 2], stack: &CopperStack) -> Vec<Backdrill> {
    drills
        .into_iter()
        .flatten()
        .map(|drill| Backdrill {
            side: if drill.layers.start == "B.Cu" {
                BoardSide::Back
            } else {
                BoardSide::Front
            },
            diameter_nm: drill
                .size
                .filter(|size| *size > 0.0)
                .map(|size| NonNegativeNm(i64::from(nm(size)))),
            until_slice_key: stack.key(&drill.layers.end).map(str::to_owned),
        })
        .collect()
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

pub(crate) fn via_holes(vias: &[PcbVia], stack: &CopperStack, holes: &mut Vec<HoleSpec>) {
    for via in vias.iter().filter(|via| via.drill > 0.0) {
        let uid = source_uid("via", via.uuid.as_deref(), via.source_range.start);
        let Some(mut facts) = facts(HolePlating::Plated, &stack.expand(&via.layers), stack) else {
            continue;
        };
        facts.protection = Some(via_protection(via));
        facts.backdrills = backdrills([via.backdrill.as_ref(), via.tertiary_drill.as_ref()], stack);
        holes.push(HoleSpec {
            origin: Origin::routed(BindingSourceKind::Via, uid, net_key(&via.net)),
            facts,
            shape: round_hole(via.drill),
            affine: rotation_translation(0.0, via.at_x, via.at_y),
        });
    }
}

/// The hole of `pad`, if it has one. Plated holes span every copper layer;
/// unplated holes span the pad's copper layers, or the whole board when it
/// lists none.
pub(crate) fn pad_hole(
    pad: &PcbPad,
    frame: &PadFrame,
    origin: Origin,
    stack: &CopperStack,
) -> Option<HoleSpec> {
    let drill = pad.drill.as_ref()?;
    let height = drill.height.unwrap_or(drill.width);
    if drill.width <= 0.0 || height <= 0.0 {
        return None;
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
    let mut facts = facts(plating, &layers, stack)?;
    facts.backdrills = backdrills([pad.backdrill.as_ref(), pad.tertiary_drill.as_ref()], stack);
    let shape = match drill.shape {
        PcbHoleShape::Oval if (drill.width - height).abs() > 1e-9 => oval(drill.width, height),
        _ => round_hole(drill.width.min(height)),
    };
    Some(HoleSpec {
        origin,
        facts,
        shape,
        affine: frame.affine(drill.offset.x, drill.offset.y),
    })
}

//! The full board stack, front to back, from KiCad's physical stackup.
//!
//! Roles come from the stackup layer type. Thickness, material, εr, and
//! tanδ are copied only when KiCad states them; anything else stays absent
//! (unknown). A board without a stackup gets an explicit stack of the same
//! shape with every physical value unknown, and a diagnostic.

use super::geometry::nm;
use super::stack::{CopperStack, slice_key};
use crate::pcb::{PcbStackup, PcbStackupLayer};
use kicad_monkey_contracts::generated::materialization_document::{
    BoardSide, NonNegativeNm, SliceRole, Stack, StackSlice,
};

/// Stack plus the reason it is incomplete, when it is.
pub(crate) struct BoardStack {
    pub stack: Stack,
    pub warning: Option<(&'static str, String)>,
}

fn side_of(type_name: &str) -> Option<BoardSide> {
    if type_name.starts_with("Top ") {
        Some(BoardSide::Front)
    } else if type_name.starts_with("Bottom ") {
        Some(BoardSide::Back)
    } else {
        None
    }
}

fn role_of(layer: &PcbStackupLayer) -> SliceRole {
    let kind = layer.type_name.as_str();
    if kind == "copper" || layer.name.ends_with(".Cu") {
        SliceRole::Conductor
    } else if kind.ends_with("Silk Screen") {
        SliceRole::Silkscreen
    } else if kind.ends_with("Solder Paste") {
        SliceRole::Paste
    } else if kind.ends_with("Solder Mask") {
        SliceRole::SolderMask
    } else {
        SliceRole::Dielectric
    }
}

fn technical_key(role: SliceRole, side: Option<BoardSide>, name: &str) -> String {
    let prefix = match role {
        SliceRole::Silkscreen => "silkscreen",
        SliceRole::Paste => "paste",
        SliceRole::SolderMask => "mask",
        SliceRole::Conductor => return slice_key(name),
        SliceRole::Dielectric => {
            let slug: String = name
                .to_ascii_lowercase()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '.' })
                .collect();
            return slug;
        }
    };
    match side {
        Some(BoardSide::Back) => format!("{prefix}.back"),
        _ => format!("{prefix}.front"),
    }
}

fn known(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("not specified"))
        .then(|| trimmed.to_owned())
}

fn slice_from(layer: &PcbStackupLayer, order: usize) -> StackSlice {
    let role = role_of(layer);
    let side = side_of(&layer.type_name);
    StackSlice {
        key: technical_key(role, side, &layer.name),
        role,
        order: u32::try_from(order).unwrap_or(u32::MAX),
        kicad_layer: Some(layer.name.clone()),
        side,
        thickness_nm: (layer.thickness > 0.0)
            .then(|| NonNegativeNm(i64::from(nm(layer.thickness)))),
        material_name: known(&layer.material),
        relative_permittivity: layer.epsilon_r,
        loss_tangent: layer.loss_tangent,
    }
}

fn technical(role: SliceRole, side: BoardSide, name: &str) -> StackSlice {
    StackSlice {
        key: technical_key(role, Some(side), name),
        role,
        order: 0,
        kicad_layer: Some(name.to_owned()),
        side: Some(side),
        thickness_nm: None,
        material_name: None,
        relative_permittivity: None,
        loss_tangent: None,
    }
}

/// Explicit stack with unknown physical values: technical layers outside,
/// copper and numbered dielectrics alternating inside.
fn default_slices(copper: &CopperStack) -> Vec<StackSlice> {
    let mut slices = vec![
        technical(SliceRole::Silkscreen, BoardSide::Front, "F.SilkS"),
        technical(SliceRole::Paste, BoardSide::Front, "F.Paste"),
        technical(SliceRole::SolderMask, BoardSide::Front, "F.Mask"),
    ];
    for (position, name) in copper.names.iter().enumerate() {
        if position > 0 {
            let dielectric = format!("dielectric {position}");
            let mut slice = technical(SliceRole::Dielectric, BoardSide::Front, &dielectric);
            slice.side = None;
            slices.push(slice);
        }
        let mut slice = technical(SliceRole::Conductor, BoardSide::Front, name);
        slice.side = match position {
            0 => Some(BoardSide::Front),
            p if p + 1 == copper.names.len() => Some(BoardSide::Back),
            _ => None,
        };
        slices.push(slice);
    }
    slices.extend([
        technical(SliceRole::SolderMask, BoardSide::Back, "B.Mask"),
        technical(SliceRole::Paste, BoardSide::Back, "B.Paste"),
        technical(SliceRole::Silkscreen, BoardSide::Back, "B.SilkS"),
    ]);
    slices
}

fn renumber(mut slices: Vec<StackSlice>) -> Vec<StackSlice> {
    for (order, slice) in slices.iter_mut().enumerate() {
        slice.order = u32::try_from(order).unwrap_or(u32::MAX);
    }
    slices
}

/// Build the stack. The stackup is used only when its copper layers are
/// exactly the board's copper layers in the same order.
pub(crate) fn board_stack(
    stackup: Option<&PcbStackup>,
    copper: &CopperStack,
    board_thickness_mm: f64,
) -> BoardStack {
    let finish = stackup
        .and_then(|value| known(&value.copper_finish))
        .filter(|value| !value.eq_ignore_ascii_case("none"));
    let thickness =
        (board_thickness_mm > 0.0).then(|| NonNegativeNm(i64::from(nm(board_thickness_mm))));
    let from_stackup = stackup.map(|value| {
        value
            .layers
            .iter()
            .enumerate()
            .map(|(order, layer)| slice_from(layer, order))
            .collect::<Vec<_>>()
    });
    let stackup_copper: Option<Vec<String>> = from_stackup.as_ref().map(|slices| {
        slices
            .iter()
            .filter(|slice| slice.role == SliceRole::Conductor)
            .filter_map(|slice| slice.kicad_layer.clone())
            .collect()
    });
    let (slices, warning) = match (from_stackup, stackup_copper) {
        (Some(slices), Some(names)) if names == copper.names => (slices, None),
        (Some(_), Some(_)) => (
            default_slices(copper),
            Some((
                "stackup_mismatch",
                "stackup copper layers differ from the board's; physical values are unknown"
                    .to_owned(),
            )),
        ),
        _ => (
            default_slices(copper),
            Some((
                "stackup_absent",
                "board has no stackup; physical values are unknown".to_owned(),
            )),
        ),
    };
    BoardStack {
        stack: Stack {
            slices: renumber(slices),
            board_thickness_nm: thickness,
            copper_finish: finish,
        },
        warning,
    }
}

//! Conductor slices and KiCad copper-layer resolution.
//!
//! Slice 1 emits one conductor slice per copper layer, front to back.
//! Dielectric, mask, paste, and silkscreen slices join in slice 2.

use crate::pcb::PcbLayer;
use kicad_monkey_contracts::generated::materialization_document::{
    BoardSide, SliceRole, StackSlice,
};
use std::collections::BTreeMap;

/// Copper layers of one board in physical order with their slice keys.
pub(crate) struct CopperStack {
    /// KiCad layer names, front to back.
    pub names: Vec<String>,
    /// KiCad layer name to slice key.
    pub keys: BTreeMap<String, String>,
}

/// Physical position of a KiCad copper layer: front, inner by number, back.
fn physical_rank(name: &str) -> Option<u32> {
    match name {
        "F.Cu" => Some(0),
        "B.Cu" => Some(u32::MAX),
        _ => name
            .strip_prefix("In")
            .and_then(|rest| rest.strip_suffix(".Cu"))
            .and_then(|number| number.parse().ok()),
    }
}

fn slice_key(name: &str) -> String {
    match name {
        "F.Cu" => "copper.front".to_owned(),
        "B.Cu" => "copper.back".to_owned(),
        _ => format!(
            "copper.{}",
            name.trim_end_matches(".Cu").to_ascii_lowercase()
        ),
    }
}

impl CopperStack {
    pub(crate) fn from_layers(layers: &[PcbLayer]) -> Self {
        let mut ranked: Vec<(u32, String)> = layers
            .iter()
            .filter_map(|layer| physical_rank(&layer.name).map(|rank| (rank, layer.name.clone())))
            .collect();
        ranked.sort();
        ranked.dedup();
        let names: Vec<String> = ranked.into_iter().map(|(_, name)| name).collect();
        let keys = names
            .iter()
            .map(|name| (name.clone(), slice_key(name)))
            .collect();
        Self { names, keys }
    }

    pub(crate) fn key(&self, name: &str) -> Option<&str> {
        self.keys.get(name).map(String::as_str)
    }

    pub(crate) fn front_key(&self) -> Option<&str> {
        self.names.first().and_then(|name| self.key(name))
    }

    pub(crate) fn back_key(&self) -> Option<&str> {
        self.names.last().and_then(|name| self.key(name))
    }

    /// Expand KiCad layer requests (`*.Cu`, `F&B.Cu`, a two-layer span, or a
    /// list) into concrete copper layer names, front to back.
    pub(crate) fn expand(&self, requested: &[String]) -> Vec<String> {
        if requested.iter().any(|name| name == "*.Cu") {
            return self.names.clone();
        }
        if requested.iter().any(|name| name == "F&B.Cu") {
            return self
                .names
                .iter()
                .filter(|name| matches!(name.as_str(), "F.Cu" | "B.Cu"))
                .cloned()
                .collect();
        }
        if let [first, last] = requested
            && first != last
            && let (Some(a), Some(b)) = (self.position(first), self.position(last))
        {
            return self.names[a.min(b)..=a.max(b)].to_vec();
        }
        self.names
            .iter()
            .filter(|name| requested.contains(name))
            .cloned()
            .collect()
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|candidate| candidate == name)
    }

    pub(crate) fn slices(&self) -> Vec<StackSlice> {
        let last = self.names.len().saturating_sub(1);
        self.names
            .iter()
            .enumerate()
            .map(|(position, name)| StackSlice {
                key: slice_key(name),
                role: SliceRole::Conductor,
                order: u32::try_from(position).unwrap_or(u32::MAX),
                kicad_layer: Some(name.clone()),
                side: match position {
                    0 => Some(BoardSide::Front),
                    p if p == last => Some(BoardSide::Back),
                    _ => None,
                },
                thickness_nm: None,
                material_name: None,
                relative_permittivity: None,
                loss_tangent: None,
            })
            .collect()
    }
}

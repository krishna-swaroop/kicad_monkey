//! Deterministic net table and net-key resolution.

use crate::pcb::{PcbNet, PcbNetRef};
use kicad_monkey_contracts::generated::materialization_document::Net;
use std::collections::BTreeSet;

/// Resolved net key of a source reference: the net name, a `#ordinal` key
/// for legacy unnamed ordinals, or the empty key for no net.
pub(crate) fn net_key(net: &PcbNetRef) -> String {
    if let Some(name) = net.name.as_ref().filter(|name| !name.is_empty()) {
        return name.clone();
    }
    net.ordinal
        .filter(|ordinal| *ordinal != 0)
        .map(|ordinal| format!("#{ordinal}"))
        .unwrap_or_default()
}

/// Every declared and referenced net, sorted by key, always including the
/// empty no-net key.
pub(crate) fn net_table<'a>(
    declared: &[PcbNet],
    references: impl IntoIterator<Item = &'a PcbNetRef>,
) -> Vec<Net> {
    let mut keys: BTreeSet<String> = BTreeSet::from([String::new()]);
    keys.extend(
        declared
            .iter()
            .map(|net| net.name.clone())
            .filter(|name| !name.is_empty()),
    );
    keys.extend(references.into_iter().map(net_key));
    keys.into_iter()
        .map(|key| Net {
            name: key.clone(),
            key,
            net_class: None,
        })
        .collect()
}

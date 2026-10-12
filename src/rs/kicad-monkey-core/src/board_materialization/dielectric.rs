//! Dielectric product: each dielectric slice is the board profile with its
//! cutouts, minus every hole whose span crosses that slice.

use super::emit::{Emitter, Material, Origin};
use super::geometry::identity;
use super::holes::HoleSpec;
use kicad_monkey_contracts::generated::materialization_document::{
    BindingSourceKind, MaterialAction, PlanarRegion2d, Shape2d, SliceRole, Stack, SurfaceMaterial,
};
use std::collections::BTreeMap;

fn dielectric(action: MaterialAction) -> Material {
    Material {
        material: SurfaceMaterial::Dielectric,
        action,
        net_key: None,
    }
}

/// Whether a hole from `start` to `end` (copper slice orders) passes
/// through the slice at `order`.
fn crosses(order: u32, start: u32, end: u32) -> bool {
    start.min(end) < order && order < start.max(end)
}

pub(crate) fn dielectric_regions(
    regions: &[PlanarRegion2d],
    holes: &[HoleSpec],
    stack: &Stack,
    emitter: &mut Emitter,
) {
    if regions.is_empty() {
        return;
    }
    let order: BTreeMap<&str, u32> = stack
        .slices
        .iter()
        .map(|slice| (slice.key.as_str(), slice.order))
        .collect();
    for slice in stack
        .slices
        .iter()
        .filter(|slice| slice.role == SliceRole::Dielectric)
    {
        for (position, region) in regions.iter().enumerate() {
            let origin = Origin::routed(
                BindingSourceKind::Profile,
                format!("board_profile#{position}"),
                String::new(),
            );
            let shape = Shape2d::PlanarRegion2d(region.clone());
            emitter.surface(
                &origin,
                &slice.key,
                dielectric(MaterialAction::Add),
                shape,
                identity(),
            );
        }
        for hole in holes {
            let start = order.get(hole.facts.start_slice_key.as_str());
            let end = order.get(hole.facts.end_slice_key.as_str());
            if let (Some(start), Some(end)) = (start, end)
                && crosses(slice.order, *start, *end)
            {
                emitter.surface(
                    &hole.origin,
                    &slice.key,
                    dielectric(MaterialAction::Subtract),
                    hole.shape.clone(),
                    hole.affine.clone(),
                );
            }
        }
    }
}

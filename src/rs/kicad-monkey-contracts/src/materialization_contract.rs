//! Semantic validation for the analytic board materialization document.
//!
//! Structural decoding already enforces field presence, enum values, and the
//! literal `kind` of every shape, segment, and operation. This module checks
//! the cross-record rules the schema cannot express: dense operation indices,
//! known slice and net references, conductive net ownership, chunk coverage,
//! binding coverage, and shape parameter sanity.
//!
//! Decode documents through [`decode_materialization_document_a0`]. The
//! workspace enables serde_json's `arbitrary_precision`, under which numbers
//! buffered inside untagged unions do not decode straight from text; the
//! decoder parses to a `Value` first, which does.

use crate::generated::materialization_document::{
    Affine2D, ChunkEntry, HoleOperation, MaterialOperation, MaterializationDocumentA0,
    MaterializationScope, NmBounds, Shape2d, SurfaceMaterial, SurfaceOperation,
};
use crate::{ValidationError, validation_error};
use std::collections::BTreeSet;
use std::fmt;

/// Reject a `kind` that differs from the expected literal.
fn expect_kind<'de, D>(deserializer: D, expected: &'static str) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    if value == expected {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(format!(
            "expected kind {expected}"
        )))
    }
}

macro_rules! kind_deserializers {
    ($($name:ident => $expected:literal),+ $(,)?) => {
        $(
            #[doc(hidden)]
            pub fn $name<'de, D>(deserializer: D) -> Result<String, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                expect_kind(deserializer, $expected)
            }
        )+
    };
}

// Exact-kind deserializers referenced by the generated module.
kind_deserializers! {
    deserialize_disk_2d_kind => "disk_2d",
    deserialize_rect_2d_kind => "rect_2d",
    deserialize_rounded_rect_2d_kind => "rounded_rect_2d",
    deserialize_chamfered_rect_2d_kind => "chamfered_rect_2d",
    deserialize_trapezoid_2d_kind => "trapezoid_2d",
    deserialize_capsule_2d_kind => "capsule_2d",
    deserialize_annulus_2d_kind => "annulus_2d",
    deserialize_swept_path_2d_kind => "swept_path_2d",
    deserialize_planar_region_2d_kind => "planar_region_2d",
    deserialize_line_segment_kind => "line",
    deserialize_arc_segment_kind => "arc",
    deserialize_surface_operation_kind => "surface",
    deserialize_hole_operation_kind => "hole",
}

const SCHEMA: &str = "kicad_monkey.materialization.a0";
const TYPE: &str = "kicad_monkey.materialization.document";
const NM_MAX: i64 = i32::MAX as i64;

/// Known slice and net keys of one document.
struct Keys<'a> {
    slices: BTreeSet<&'a str>,
    nets: BTreeSet<&'a str>,
}

/// Decode JSON bytes and enforce the a0 semantic rules.
pub fn decode_materialization_document_a0(
    source: &[u8],
) -> Result<MaterializationDocumentA0, MaterializationDecodeError> {
    let value: serde_json::Value =
        serde_json::from_slice(source).map_err(MaterializationDecodeError::Transport)?;
    let document: MaterializationDocumentA0 =
        serde_json::from_value(value).map_err(MaterializationDecodeError::Transport)?;
    validate_materialization_document(&document).map_err(MaterializationDecodeError::Validation)?;
    Ok(document)
}

/// Failure to decode or validate a materialization document.
#[derive(Debug)]
pub enum MaterializationDecodeError {
    Transport(serde_json::Error),
    Validation(ValidationError),
}

impl fmt::Display for MaterializationDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "materialization transport: {error}"),
            Self::Validation(error) => write!(formatter, "materialization validation: {error}"),
        }
    }
}

impl std::error::Error for MaterializationDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Validation(error) => Some(error),
        }
    }
}

/// Enforce the cross-record rules of `kicad_monkey.materialization.a0`.
pub fn validate_materialization_document(
    document: &MaterializationDocumentA0,
) -> Result<(), ValidationError> {
    validate_identity(document)?;
    validate_scope(&document.scope)?;
    let keys = collect_keys(document)?;
    validate_nets(document, &keys)?;
    for (position, operation) in document.operations.iter().enumerate() {
        validate_operation(position, operation, &keys)?;
    }
    validate_binding(document)?;
    validate_chunks(document)
}

fn validate_identity(document: &MaterializationDocumentA0) -> Result<(), ValidationError> {
    let system = &document.coordinate_system;
    if document.schema != SCHEMA
        || document.type_ != TYPE
        || document.version != "a0"
        || system.unit != "nm"
        || system.x_axis != "board_right"
        || system.y_axis != "board_down"
    {
        return Err(validation_error(
            "unsupported_contract",
            "$",
            "materialization identity and coordinate system must match a0",
        ));
    }
    Ok(())
}

fn validate_scope(scope: &MaterializationScope) -> Result<(), ValidationError> {
    if scope.products.is_empty() {
        return Err(validation_error(
            "empty_scope",
            "$.scope.products",
            "a materialization names at least one product",
        ));
    }
    let filtered = !scope.slice_keys.is_empty() || !scope.net_keys.is_empty();
    if scope.complete && (filtered || scope.region.is_some()) {
        return Err(validation_error(
            "inconsistent_scope",
            "$.scope.complete",
            "a complete materialization has no slice, net, or region filter",
        ));
    }
    if *scope.tile_size_nm <= 0 || *scope.tile_size_nm > NM_MAX {
        return Err(validation_error(
            "invalid_tile_size",
            "$.scope.tile_size_nm",
            "tile size is a positive 32-bit nanometre length",
        ));
    }
    if let Some(region) = &scope.region {
        validate_bounds(region, "$.scope.region")?;
    }
    Ok(())
}

fn collect_keys(document: &MaterializationDocumentA0) -> Result<Keys<'_>, ValidationError> {
    let mut slices = BTreeSet::new();
    for (position, slice) in document.stack.slices.iter().enumerate() {
        if usize::try_from(slice.order).ok() != Some(position) {
            return Err(validation_error(
                "invalid_slice_order",
                format!("$.stack.slices[{position}].order"),
                "slices are listed front to back with dense orders",
            ));
        }
        if !slices.insert(slice.key.as_str()) {
            return Err(validation_error(
                "duplicate_slice",
                format!("$.stack.slices[{position}].key"),
                "slice keys are unique",
            ));
        }
    }
    let mut nets = BTreeSet::new();
    for (position, net) in document.nets.iter().enumerate() {
        if !nets.insert(net.key.as_str()) {
            return Err(validation_error(
                "duplicate_net",
                format!("$.nets[{position}].key"),
                "net keys are unique",
            ));
        }
    }
    Ok(Keys { slices, nets })
}

fn validate_nets(
    document: &MaterializationDocumentA0,
    keys: &Keys<'_>,
) -> Result<(), ValidationError> {
    let classes: BTreeSet<&str> = document
        .net_classes
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    for (position, net) in document.nets.iter().enumerate() {
        if net
            .net_class
            .as_deref()
            .is_some_and(|name| !classes.contains(name))
        {
            return Err(validation_error(
                "unknown_net_class",
                format!("$.nets[{position}].net_class"),
                "a net's class is listed in net_classes",
            ));
        }
    }
    for (position, pair) in document.diff_pairs.iter().enumerate() {
        let known = keys.nets.contains(pair.positive_net_key.as_str())
            && keys.nets.contains(pair.negative_net_key.as_str());
        if !known || pair.positive_net_key == pair.negative_net_key {
            return Err(validation_error(
                "invalid_diff_pair",
                format!("$.diff_pairs[{position}]"),
                "a differential pair names two distinct known nets",
            ));
        }
    }
    Ok(())
}

fn validate_operation(
    position: usize,
    operation: &MaterialOperation,
    keys: &Keys<'_>,
) -> Result<(), ValidationError> {
    let path = format!("$.operations[{position}]");
    match operation {
        MaterialOperation::SurfaceOperation(surface) => {
            check_index(surface.index, position, &path)?;
            validate_surface(surface, keys, &path)
        }
        MaterialOperation::HoleOperation(hole) => {
            check_index(hole.index, position, &path)?;
            validate_hole(hole, keys, &path)
        }
    }
}

fn check_index(index: u32, position: usize, path: &str) -> Result<(), ValidationError> {
    if usize::try_from(index).ok() == Some(position) {
        return Ok(());
    }
    Err(validation_error(
        "invalid_operation_index",
        format!("{path}.index"),
        "operation indices are dense and equal to their position",
    ))
}

fn validate_surface(
    surface: &SurfaceOperation,
    keys: &Keys<'_>,
    path: &str,
) -> Result<(), ValidationError> {
    check_slice(keys, &surface.slice_key, &format!("{path}.slice_key"))?;
    let conductive = matches!(surface.material, SurfaceMaterial::Copper);
    match (&surface.net_key, conductive) {
        (None, true) => {
            return Err(validation_error(
                "missing_net",
                format!("{path}.net_key"),
                "conductive material always carries a net key",
            ));
        }
        (Some(_), false) => {
            return Err(validation_error(
                "unexpected_net",
                format!("{path}.net_key"),
                "only conductive material carries a net key",
            ));
        }
        (Some(net), true) => check_net(keys, net, &format!("{path}.net_key"))?,
        (None, false) => {}
    }
    validate_affine(&surface.affine, &format!("{path}.affine"))?;
    validate_bounds(&surface.bounds, &format!("{path}.bounds"))?;
    validate_shape(&surface.shape, &format!("{path}.shape"))
}

fn validate_hole(hole: &HoleOperation, keys: &Keys<'_>, path: &str) -> Result<(), ValidationError> {
    check_slice(
        keys,
        &hole.start_slice_key,
        &format!("{path}.start_slice_key"),
    )?;
    check_slice(keys, &hole.end_slice_key, &format!("{path}.end_slice_key"))?;
    if let Some(net) = &hole.net_key {
        check_net(keys, net, &format!("{path}.net_key"))?;
    }
    for (position, backdrill) in hole.backdrills.iter().enumerate() {
        if let Some(slice) = &backdrill.until_slice_key {
            check_slice(
                keys,
                slice,
                &format!("{path}.backdrills[{position}].until_slice_key"),
            )?;
        }
    }
    if !matches!(
        hole.shape,
        Shape2d::Disk2d(_)
            | Shape2d::Capsule2d(_)
            | Shape2d::SweptPath2d(_)
            | Shape2d::PlanarRegion2d(_)
    ) {
        return Err(validation_error(
            "invalid_hole_shape",
            format!("{path}.shape"),
            "holes are disks, capsules, swept paths, or planar regions",
        ));
    }
    validate_affine(&hole.affine, &format!("{path}.affine"))?;
    validate_bounds(&hole.bounds, &format!("{path}.bounds"))?;
    validate_shape(&hole.shape, &format!("{path}.shape"))
}

fn check_slice(keys: &Keys<'_>, key: &str, path: &str) -> Result<(), ValidationError> {
    if keys.slices.contains(key) {
        return Ok(());
    }
    Err(validation_error(
        "unknown_slice",
        path.to_owned(),
        "operations reference slices listed in the stack",
    ))
}

fn check_net(keys: &Keys<'_>, key: &str, path: &str) -> Result<(), ValidationError> {
    if keys.nets.contains(key) {
        return Ok(());
    }
    Err(validation_error(
        "unknown_net",
        path.to_owned(),
        "operations reference nets listed in the net table",
    ))
}

fn validate_affine(affine: &Affine2D, path: &str) -> Result<(), ValidationError> {
    let terms = [affine.m11, affine.m12, affine.m21, affine.m22];
    let determinant = affine.m11 * affine.m22 - affine.m12 * affine.m21;
    if terms.iter().all(|term| term.is_finite()) && determinant.abs() > f64::EPSILON {
        return Ok(());
    }
    Err(validation_error(
        "invalid_affine",
        path.to_owned(),
        "affine terms are finite and the matrix is invertible",
    ))
}

fn validate_bounds(bounds: &NmBounds, path: &str) -> Result<(), ValidationError> {
    if *bounds.min_x_nm <= *bounds.max_x_nm && *bounds.min_y_nm <= *bounds.max_y_nm {
        return Ok(());
    }
    Err(validation_error(
        "invalid_bounds",
        path.to_owned(),
        "bounds minima do not exceed their maxima",
    ))
}

/// Parameter sanity per shape; the radius and chamfer limits follow from the
/// shape's own extents.
fn validate_shape(shape: &Shape2d, path: &str) -> Result<(), ValidationError> {
    let lengths = shape_lengths(shape, path)?;
    if lengths.iter().all(|length| (0..=NM_MAX).contains(length)) {
        return Ok(());
    }
    Err(validation_error(
        "invalid_length",
        path.to_owned(),
        "shape lengths are non-negative 32-bit nanometre values",
    ))
}

fn shape_lengths(shape: &Shape2d, path: &str) -> Result<Vec<i64>, ValidationError> {
    Ok(match shape {
        Shape2d::Disk2d(disk) => vec![*disk.radius_nm],
        Shape2d::Rect2d(rect) => vec![*rect.width_nm, *rect.height_nm],
        Shape2d::RoundedRect2d(rect) => {
            check_corner(*rect.radius_nm, *rect.width_nm, *rect.height_nm, path)?;
            vec![*rect.width_nm, *rect.height_nm, *rect.radius_nm]
        }
        Shape2d::ChamferedRect2d(rect) => {
            check_corner(*rect.radius_nm, *rect.width_nm, *rect.height_nm, path)?;
            check_corner(*rect.chamfer_nm, *rect.width_nm, *rect.height_nm, path)?;
            vec![*rect.width_nm, *rect.height_nm]
        }
        Shape2d::Trapezoid2d(trapezoid) => vec![*trapezoid.width_nm, *trapezoid.height_nm],
        Shape2d::Capsule2d(capsule) => vec![*capsule.radius_nm],
        Shape2d::Annulus2d(annulus) => {
            check_annulus(*annulus.inner_radius_nm, *annulus.outer_radius_nm, path)?;
            vec![*annulus.outer_radius_nm, *annulus.inner_radius_nm]
        }
        Shape2d::SweptPath2d(path_shape) => vec![*path_shape.width_nm],
        Shape2d::PlanarRegion2d(_) => Vec::new(),
    })
}

fn check_annulus(inner: i64, outer: i64, path: &str) -> Result<(), ValidationError> {
    if inner < outer {
        return Ok(());
    }
    Err(validation_error(
        "invalid_annulus",
        path.to_owned(),
        "an annulus inner radius is below its outer radius",
    ))
}

fn check_corner(size: i64, width: i64, height: i64, path: &str) -> Result<(), ValidationError> {
    if size.saturating_mul(2) <= width.min(height) {
        return Ok(());
    }
    Err(validation_error(
        "invalid_corner",
        path.to_owned(),
        "corner radius or chamfer is at most half the shorter side",
    ))
}

fn validate_binding(document: &MaterializationDocumentA0) -> Result<(), ValidationError> {
    let count = document.operations.len();
    let mut previous: Option<u32> = None;
    for (position, entry) in document.binding.iter().enumerate() {
        let in_range = usize::try_from(entry.operation_index).is_ok_and(|index| index < count);
        let ascending = previous.is_none_or(|last| entry.operation_index > last);
        if !in_range || !ascending {
            return Err(validation_error(
                "invalid_binding",
                format!("$.binding[{position}].operation_index"),
                "binding entries reference existing operations in ascending order",
            ));
        }
        previous = Some(entry.operation_index);
    }
    Ok(())
}

/// Chunks cover every operation exactly once, in index order, and each
/// operation lies on its chunk's slice inside its chunk's bounds.
fn validate_chunks(document: &MaterializationDocumentA0) -> Result<(), ValidationError> {
    let mut next = 0usize;
    for (position, chunk) in document.chunks.iter().enumerate() {
        let path = format!("$.chunks[{position}]");
        validate_bounds(&chunk.bounds, &format!("{path}.bounds"))?;
        let first = usize::try_from(chunk.first_operation_index).ok();
        let count = usize::try_from(chunk.operation_count).unwrap_or(usize::MAX);
        if first != Some(next) || count == 0 {
            return Err(validation_error(
                "invalid_chunk_range",
                path,
                "chunks are non-empty, contiguous, and in operation order",
            ));
        }
        let end = next.saturating_add(count);
        let operations = document.operations.get(next..end).ok_or_else(|| {
            validation_error(
                "invalid_chunk_range",
                path.clone(),
                "chunk range exceeds operations",
            )
        })?;
        for operation in operations {
            check_chunk_member(chunk, operation, &path)?;
        }
        next = end;
    }
    if next != document.operations.len() {
        return Err(validation_error(
            "uncovered_operations",
            "$.chunks",
            "every operation belongs to exactly one chunk",
        ));
    }
    Ok(())
}

fn check_chunk_member(
    chunk: &ChunkEntry,
    operation: &MaterialOperation,
    path: &str,
) -> Result<(), ValidationError> {
    let (slice, bounds) = match operation {
        MaterialOperation::SurfaceOperation(surface) => (&surface.slice_key, &surface.bounds),
        MaterialOperation::HoleOperation(hole) => (&hole.start_slice_key, &hole.bounds),
    };
    let inside = *chunk.bounds.min_x_nm <= *bounds.min_x_nm
        && *chunk.bounds.min_y_nm <= *bounds.min_y_nm
        && *chunk.bounds.max_x_nm >= *bounds.max_x_nm
        && *chunk.bounds.max_y_nm >= *bounds.max_y_nm;
    if *slice == chunk.slice_key && inside {
        return Ok(());
    }
    Err(validation_error(
        "invalid_chunk_member",
        path.to_owned(),
        "chunk members lie on the chunk slice inside the chunk bounds",
    ))
}

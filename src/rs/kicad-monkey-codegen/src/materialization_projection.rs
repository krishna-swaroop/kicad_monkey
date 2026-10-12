//! Exact-kind projection for the materialization contract's unions.

use crate::{project_kind_deserializer, validate_operation_union};
use anyhow::Result;
use serde_json::Value;

const MATERIALIZATION_SHAPE_KINDS: [(&str, &str, &str); 9] = [
    (
        "Disk2d",
        "disk_2d",
        "materialization_contract::deserialize_disk_2d_kind",
    ),
    (
        "Rect2d",
        "rect_2d",
        "materialization_contract::deserialize_rect_2d_kind",
    ),
    (
        "RoundedRect2d",
        "rounded_rect_2d",
        "materialization_contract::deserialize_rounded_rect_2d_kind",
    ),
    (
        "ChamferedRect2d",
        "chamfered_rect_2d",
        "materialization_contract::deserialize_chamfered_rect_2d_kind",
    ),
    (
        "Trapezoid2d",
        "trapezoid_2d",
        "materialization_contract::deserialize_trapezoid_2d_kind",
    ),
    (
        "Capsule2d",
        "capsule_2d",
        "materialization_contract::deserialize_capsule_2d_kind",
    ),
    (
        "Annulus2d",
        "annulus_2d",
        "materialization_contract::deserialize_annulus_2d_kind",
    ),
    (
        "SweptPath2d",
        "swept_path_2d",
        "materialization_contract::deserialize_swept_path_2d_kind",
    ),
    (
        "PlanarRegion2d",
        "planar_region_2d",
        "materialization_contract::deserialize_planar_region_2d_kind",
    ),
];

const MATERIALIZATION_SEGMENT_KINDS: [(&str, &str, &str); 3] = [
    (
        "LineSegment",
        "line",
        "materialization_contract::deserialize_line_segment_kind",
    ),
    (
        "ArcSegment",
        "arc",
        "materialization_contract::deserialize_arc_segment_kind",
    ),
    (
        "CubicSegment",
        "cubic",
        "materialization_contract::deserialize_cubic_segment_kind",
    ),
];

const MATERIALIZATION_OPERATION_KINDS: [(&str, &str, &str); 2] = [
    (
        "SurfaceOperation",
        "surface",
        "materialization_contract::deserialize_surface_operation_kind",
    ),
    (
        "HoleOperation",
        "hole",
        "materialization_contract::deserialize_hole_operation_kind",
    ),
];

pub(crate) fn validate_materialization_kinds(schema_name: &str, schema: &Value) -> Result<()> {
    validate_operation_union(schema_name, schema, "Shape2d", &MATERIALIZATION_SHAPE_KINDS)?;
    validate_operation_union(
        schema_name,
        schema,
        "PathSegment",
        &MATERIALIZATION_SEGMENT_KINDS,
    )?;
    validate_operation_union(
        schema_name,
        schema,
        "MaterialOperation",
        &MATERIALIZATION_OPERATION_KINDS,
    )
}

pub(crate) fn project_materialization_kinds(
    schema_name: &str,
    mut source: String,
) -> Result<String> {
    if schema_name != "MaterializationDocument.json" {
        return Ok(source);
    }
    for table in [
        &MATERIALIZATION_SHAPE_KINDS[..],
        &MATERIALIZATION_SEGMENT_KINDS[..],
        &MATERIALIZATION_OPERATION_KINDS[..],
    ] {
        for (structure, _, deserializer) in table {
            source = project_kind_deserializer(source, structure, deserializer)?;
        }
    }
    Ok(source)
}

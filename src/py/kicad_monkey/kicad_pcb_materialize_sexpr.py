"""Deterministic S-expression view of an analytic materialization.

This module is deliberately not part of the promoted package API.

The view exists for fixtures, corpus comparison, debugging, and subprocess
boundaries. It is a rendering of the in-memory value objects, not a second
source of truth: the Python types remain the primary interface.

Determinism matters more than compactness here, so numbers go through
:func:`format_float`, which clamps float artifacts that would otherwise print
as ``-0``.
"""

from __future__ import annotations

from typing import Any, Callable

from .kicad_pcb_materialize import (
    HoleOperation2D,
    MaterializedOperation,
    PcbAnalyticMaterialization,
    SurfaceOperation2D,
)
from .kicad_pcb_materialize_geometry import (
    Affine2D,
    Annulus2D,
    ArcSegment2D,
    Capsule2D,
    ChamferedRect2D,
    Disk2D,
    LineSegment2D,
    PlanarRegion2D,
    Rect2D,
    RoundedRect2D,
    Shape2D,
    SweptPath2D,
    Trapezoid2D,
    shape_kind,
)
from .kicad_pcb_materialize_stack import MaterialSlice
from .kicad_sexpr import QuotedString, build_sexp, format_float

SexpNode = list[Any]


def _num(value: float) -> str:
    return format_float(float(value))


def _point(name: str, point: tuple[int, int]) -> SexpNode:
    return [name, str(int(point[0])), str(int(point[1]))]


def _ring(points: tuple[tuple[int, int], ...]) -> SexpNode:
    return ["ring", *[_point("xy", point) for point in points]]


def _disk(shape: Disk2D) -> SexpNode:
    return [
        "disk_2d",
        _point("center_nm", shape.center_nm),
        ["radius_nm", str(shape.radius_nm)],
    ]


def _annulus(shape: Annulus2D) -> SexpNode:
    return [
        "annulus_2d",
        _point("center_nm", shape.center_nm),
        ["outer_radius_nm", str(shape.outer_radius_nm)],
        ["inner_radius_nm", str(shape.inner_radius_nm)],
    ]


def _rect(shape: Rect2D) -> SexpNode:
    return [
        "rect_2d",
        _point("center_nm", shape.center_nm),
        ["width_nm", str(shape.width_nm)],
        ["height_nm", str(shape.height_nm)],
    ]


def _rounded_rect(shape: RoundedRect2D) -> SexpNode:
    return [
        "rounded_rect_2d",
        _point("center_nm", shape.center_nm),
        ["width_nm", str(shape.width_nm)],
        ["height_nm", str(shape.height_nm)],
        ["radius_nm", str(shape.corner_radius_nm)],
    ]


def _chamfered_rect(shape: ChamferedRect2D) -> SexpNode:
    return [
        "chamfered_rect_2d",
        _point("center_nm", shape.center_nm),
        ["width_nm", str(shape.width_nm)],
        ["height_nm", str(shape.height_nm)],
        ["radius_nm", str(shape.corner_radius_nm)],
        ["chamfer_ratio", _num(shape.chamfer_ratio)],
        ["chamfer_corners", *sorted(shape.chamfer_corners)],
    ]


def _trapezoid(shape: Trapezoid2D) -> SexpNode:
    return [
        "trapezoid_2d",
        _point("center_nm", shape.center_nm),
        ["width_nm", str(shape.width_nm)],
        ["height_nm", str(shape.height_nm)],
        ["delta_x_nm", str(shape.delta_x_nm)],
        ["delta_y_nm", str(shape.delta_y_nm)],
    ]


def _capsule(shape: Capsule2D) -> SexpNode:
    return [
        "capsule_2d",
        _point("start_nm", shape.start_nm),
        _point("end_nm", shape.end_nm),
        ["width_nm", str(shape.width_nm)],
    ]


def _path_segment(segment: LineSegment2D | ArcSegment2D) -> SexpNode:
    if isinstance(segment, ArcSegment2D):
        return [
            "arc",
            _point("start_nm", segment.start_nm),
            _point("mid_nm", segment.mid_nm),
            _point("end_nm", segment.end_nm),
        ]
    return [
        "line",
        _point("start_nm", segment.start_nm),
        _point("end_nm", segment.end_nm),
    ]


def _swept_path(shape: SweptPath2D) -> SexpNode:
    return [
        "swept_path_2d",
        ["width_nm", str(shape.width_nm)],
        ["cap", shape.cap.value],
        ["join", shape.join.value],
        ["segments", *[_path_segment(item) for item in shape.segments]],
    ]


def _planar_region(shape: PlanarRegion2D) -> SexpNode:
    node: SexpNode = ["planar_region_2d", ["outer", *[
        _point("xy", point) for point in shape.outer_nm
    ]]]
    for hole in shape.holes_nm:
        node.append(["hole", *[_point("xy", point) for point in hole]])
    return node


_SHAPE_WRITERS: dict[type, Callable[[Any], SexpNode]] = {
    Disk2D: _disk,
    Annulus2D: _annulus,
    Rect2D: _rect,
    RoundedRect2D: _rounded_rect,
    ChamferedRect2D: _chamfered_rect,
    Trapezoid2D: _trapezoid,
    Capsule2D: _capsule,
    SweptPath2D: _swept_path,
    PlanarRegion2D: _planar_region,
}


def shape_sexp(shape: Shape2D) -> SexpNode:
    """Render one analytic shape."""
    writer = _SHAPE_WRITERS.get(type(shape))
    if writer is None:
        raise TypeError(f"no writer for shape {shape_kind(shape)}")
    return writer(shape)


def affine_sexp(affine: Affine2D) -> SexpNode:
    translation = affine.translation_nm
    return [
        "affine",
        _num(affine.a),
        _num(affine.b),
        _num(affine.c),
        _num(affine.d),
        str(translation[0]),
        str(translation[1]),
    ]


def _net_key_node(net_key: str | None) -> list[SexpNode]:
    if net_key is None:
        return []
    return [["net_key", QuotedString(net_key)]]


def _surface_sexp(operation: SurfaceOperation2D) -> SexpNode:
    return [
        "surface_operation_2d",
        ["index", str(operation.index)],
        ["action", operation.action.value],
        ["material", operation.material.value],
        ["slice_key", operation.slice_key],
        *_net_key_node(operation.net_key),
        ["geometry", shape_sexp(operation.geometry)],
        affine_sexp(operation.affine),
    ]


def _hole_sexp(operation: HoleOperation2D) -> SexpNode:
    return [
        "hole_operation_2d",
        ["index", str(operation.index)],
        ["plating", operation.plating.value],
        ["start_slice_key", operation.start_slice_key],
        ["end_slice_key", operation.end_slice_key],
        *_net_key_node(operation.net_key),
        ["geometry", shape_sexp(operation.geometry)],
        affine_sexp(operation.affine),
    ]


def operation_sexp(operation: MaterializedOperation) -> SexpNode:
    if isinstance(operation, SurfaceOperation2D):
        return _surface_sexp(operation)
    return _hole_sexp(operation)


def _slice_sexp(item: MaterialSlice) -> SexpNode:
    node: SexpNode = [
        "slice",
        ["key", item.key],
        ["role", item.role.value],
        ["order", str(item.order)],
    ]
    if item.thickness_nm is not None:
        node.append(["thickness_nm", str(item.thickness_nm)])
    dielectric = item.dielectric
    if dielectric is not None:
        properties: SexpNode = ["dielectric"]
        if dielectric.material_name:
            properties.append(
                ["material_name", QuotedString(dielectric.material_name)]
            )
        if dielectric.relative_permittivity is not None:
            properties.append(
                ["relative_permittivity", _num(dielectric.relative_permittivity)]
            )
        if dielectric.loss_tangent is not None:
            properties.append(["loss_tangent", _num(dielectric.loss_tangent)])
        node.append(properties)
    return node


def _scope_sexp(result: PcbAnalyticMaterialization) -> SexpNode:
    scope = result.scope
    return [
        "scope",
        ["products", *sorted(item.value for item in scope.products)],
        ["slice_keys", *scope.slice_keys],
        ["complete_board", "true" if scope.complete_board else "false"],
    ]


def materialization_sexp(result: PcbAnalyticMaterialization) -> SexpNode:
    """Render a materialization as a nested s-expression list."""
    return [
        "pcb_analytic_materialization",
        _scope_sexp(result),
        [
            "nets",
            *[
                ["net", ["key", QuotedString(net.key)],
                 ["name", QuotedString(net.name)]]
                for net in result.nets
            ],
        ],
        ["slices", *[_slice_sexp(item) for item in result.slices]],
        ["operations", *[operation_sexp(op) for op in result.operations]],
    ]


def write_materialization_sexp(result: PcbAnalyticMaterialization) -> str:
    """Render a materialization to a deterministic s-expression string."""
    return build_sexp(materialization_sexp(result))

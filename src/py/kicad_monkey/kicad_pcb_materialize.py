"""Analytic materialization of a KiCad PCB into physical operations.

This module is deliberately not part of the promoted package API.

Once geometry crosses this boundary it no longer matters whether a piece of
copper was authored as a ``track``, ``pad``, ``via``, ``zone``, or
``filled_polygon``. Those names describe how a board was drawn. A geometry
consumer needs to know what physical material exists, where in the stack it
sits, what it is electrically connected to, and how it is placed.

So the result is a stream of material operations, each carrying an analytic
shape plus one fully composed local-to-board affine. Nothing is polygonized
here: a track stays a capsule, an arc stays a swept path, a round pad stays a
disk. Lowering to polygons, meshes, or apertures belongs in an output adapter,
where the required tolerance is actually known.

Scoping note: the request currently prunes emission by product and by slice,
which is what makes selective output equal the filtered complete output. It
does not yet prune the underlying text scan, because the copper families a
front-copper request needs are the same ones a full-copper request needs.
Pushing scope further back becomes worthwhile once mask, paste, and silkscreen
products are added.
"""

from __future__ import annotations

from dataclasses import dataclass, field, replace
from enum import Enum
from pathlib import Path
from typing import Any, Iterable, Sequence, Union

from .kicad_base import PadShape, PadType
from .kicad_pcb import KiCadPcb
from .kicad_pcb_materialize_geometry import (
    Affine2D,
    ArcSegment2D,
    Capsule2D,
    ChamferedRect2D,
    Disk2D,
    PlanarRegion2D,
    Rect2D,
    RoundedRect2D,
    Shape2D,
    SweptPath2D,
    Trapezoid2D,
)
from .kicad_pcb_materialize_stack import (
    MaterialKind,
    MaterialSlice,
    PcbMaterialStack,
    build_material_stack,
    copper_slice_key,
)
from .kicad_pcb_projection import KiCadPcbProjection
from .kicad_pcb_projection_source import (
    NM_PER_MM,
    BoardSource,
    coerce_source,
    expand_layer_names,
    is_copper_layer,
    net_parts,
    ring_to_nm,
)

_ROUND_RECT_EPSILON_MM = 0.001
_DEFAULT_ROUNDRECT_RRATIO = 0.25


class MaterialProduct(str, Enum):
    """A selectable product of materialization."""

    COPPER = "copper"
    HOLES = "holes"


class SurfaceAction(str, Enum):
    """Whether an operation adds or removes material."""

    ADD = "add"
    SUBTRACT = "subtract"


class HolePlating(str, Enum):
    """Whether a hole is plated through."""

    PLATED = "plated"
    UNPLATED = "unplated"


ALL_PRODUCTS = frozenset({MaterialProduct.COPPER, MaterialProduct.HOLES})


@dataclass(frozen=True, slots=True)
class MaterializationRequest:
    """What to materialize.

    An empty ``slice_keys`` means every slice. Products and slices both prune
    the result; a selective result must equal the complete result filtered the
    same way.
    """

    products: frozenset[MaterialProduct] = ALL_PRODUCTS
    slice_keys: tuple[str, ...] = ()

    def __post_init__(self) -> None:
        object.__setattr__(
            self,
            "products",
            frozenset(MaterialProduct(item) for item in self.products),
        )
        object.__setattr__(self, "slice_keys", tuple(self.slice_keys))
        if not self.products:
            raise ValueError("MaterializationRequest requires a product")

    @classmethod
    def complete(cls) -> "MaterializationRequest":
        return cls()

    @classmethod
    def front_copper(cls) -> "MaterializationRequest":
        return cls(
            products=frozenset({MaterialProduct.COPPER}),
            slice_keys=("copper.front",),
        )

    def wants(self, product: MaterialProduct) -> bool:
        return product in self.products

    def wants_slice(self, slice_key: str) -> bool:
        return not self.slice_keys or slice_key in self.slice_keys


@dataclass(frozen=True, slots=True)
class MaterializationScope:
    """What a result actually covers, so a partial one is never mistaken."""

    products: frozenset[MaterialProduct]
    slice_keys: tuple[str, ...]
    complete_board: bool

    def __post_init__(self) -> None:
        object.__setattr__(self, "products", frozenset(self.products))
        object.__setattr__(self, "slice_keys", tuple(self.slice_keys))
        object.__setattr__(self, "complete_board", bool(self.complete_board))


@dataclass(frozen=True, slots=True)
class MaterializedNet:
    """A referenced net. The resolved name is the stable key."""

    key: str
    name: str


@dataclass(frozen=True, slots=True)
class SurfaceOperation2D:
    """Material added to or removed from one physical slice."""

    action: SurfaceAction
    material: MaterialKind
    slice_key: str
    geometry: Shape2D
    affine: Affine2D
    net_key: str | None = None
    index: int = 0


@dataclass(frozen=True, slots=True)
class HoleOperation2D:
    """A physical hole, with plating intent and a stack span."""

    plating: HolePlating
    start_slice_key: str
    end_slice_key: str
    geometry: Shape2D
    affine: Affine2D
    net_key: str | None = None
    index: int = 0


MaterializedOperation = Union[SurfaceOperation2D, HoleOperation2D]


@dataclass(frozen=True, slots=True)
class PcbAnalyticMaterialization:
    """The materialized board, scoped to what was requested."""

    scope: MaterializationScope
    nets: tuple[MaterializedNet, ...] = ()
    slices: tuple[MaterialSlice, ...] = ()
    operations: tuple[MaterializedOperation, ...] = ()
    source_path: str | None = None

    @property
    def surface_operations(self) -> tuple[SurfaceOperation2D, ...]:
        return tuple(
            op for op in self.operations if isinstance(op, SurfaceOperation2D)
        )

    @property
    def hole_operations(self) -> tuple[HoleOperation2D, ...]:
        return tuple(
            op for op in self.operations if isinstance(op, HoleOperation2D)
        )


def operation_identity(operation: MaterializedOperation) -> tuple[Any, ...]:
    """Content of an operation excluding its positional index.

    Indexes number a result from zero, so a filtered subset renumbers. Compare
    on this when checking that a selective result matches a filtered complete
    one.
    """
    if isinstance(operation, SurfaceOperation2D):
        return (
            "surface",
            operation.action,
            operation.material,
            operation.slice_key,
            operation.geometry,
            operation.affine,
            operation.net_key,
        )
    return (
        "hole",
        operation.plating,
        operation.start_slice_key,
        operation.end_slice_key,
        operation.geometry,
        operation.affine,
        operation.net_key,
    )


@dataclass(slots=True)
class _Context:
    """Mutable emission state for one materialization run."""

    request: MaterializationRequest
    stack: PcbMaterialStack
    copper_layer_names: tuple[str, ...]
    surfaces: list[SurfaceOperation2D] = field(default_factory=list)
    holes: list[HoleOperation2D] = field(default_factory=list)
    net_names: dict[str, str] = field(default_factory=dict)

    def slice_keys_for(self, layer_names: Sequence[str]) -> tuple[str, ...]:
        """Requested conductor keys for a set of KiCad layer names."""
        return tuple(
            key
            for key in (copper_slice_key(name) for name in layer_names)
            if self.request.wants_slice(key)
        )

    def net_key(self, obj: object) -> str | None:
        name, _ordinal = net_parts(obj)
        if not name:
            return None
        self.net_names.setdefault(name, name)
        return name

    def add_copper(
        self,
        *,
        slice_key: str,
        geometry: Shape2D,
        affine: Affine2D,
        net_key: str | None,
    ) -> None:
        self.surfaces.append(
            SurfaceOperation2D(
                action=SurfaceAction.ADD,
                material=MaterialKind.COPPER,
                slice_key=slice_key,
                geometry=geometry,
                affine=affine,
                net_key=net_key,
            )
        )


def _nm(value: object) -> float:
    """Millimetres to nanometres, as a float.

    Affine translations keep this precision so that composing a chain of
    transforms does not accumulate rounding. Shape dimensions round with
    :func:`_nmi` instead, because nanometres are the contract resolution.
    """
    return float(value or 0.0) * NM_PER_MM  # type: ignore[arg-type]


def _point_nm(x: object, y: object) -> tuple[float, float]:
    return (_nm(x), _nm(y))


def _nmi(value: object) -> int:
    """Millimetres to integer nanometres, for shape dimensions."""
    return int(round(_nm(value)))


def _point_nmi(x: object, y: object) -> tuple[int, int]:
    return (_nmi(x), _nmi(y))


def _half_nm(value_nm: float) -> int:
    return int(round(value_nm / 2.0))


def _copper_layer_names(source: BoardSource) -> tuple[str, ...]:
    return tuple(
        str(layer.canonical_name)
        for layer in source.collection("layers")
        if is_copper_layer(str(layer.canonical_name))
    )


# --- copper families -------------------------------------------------------


def _materialize_tracks(context: _Context, source: BoardSource) -> None:
    for segment in source.collection("segments"):
        width = _nm(getattr(segment, "width", 0.0))
        if width <= 0:
            continue
        slice_keys = context.slice_keys_for(
            expand_layer_names([segment.layer], context.copper_layer_names)
        )
        if not slice_keys:
            continue
        geometry = Capsule2D(
            start_nm=_point_nmi(segment.start_x, segment.start_y),
            end_nm=_point_nmi(segment.end_x, segment.end_y),
            width_nm=int(round(width)),
        )
        net_key = context.net_key(segment)
        for slice_key in slice_keys:
            context.add_copper(
                slice_key=slice_key,
                geometry=geometry,
                affine=Affine2D.identity(),
                net_key=net_key,
            )


def _materialize_arcs(context: _Context, source: BoardSource) -> None:
    for arc in source.collection("arcs"):
        width = _nm(getattr(arc, "width", 0.0))
        if width <= 0:
            continue
        slice_keys = context.slice_keys_for(
            expand_layer_names([arc.layer], context.copper_layer_names)
        )
        if not slice_keys:
            continue
        geometry = SweptPath2D(
            segments=(
                ArcSegment2D(
                    start_nm=_point_nmi(arc.start_x, arc.start_y),
                    mid_nm=_point_nmi(arc.mid_x, arc.mid_y),
                    end_nm=_point_nmi(arc.end_x, arc.end_y),
                ),
            ),
            width_nm=int(round(width)),
        )
        net_key = context.net_key(arc)
        for slice_key in slice_keys:
            context.add_copper(
                slice_key=slice_key,
                geometry=geometry,
                affine=Affine2D.identity(),
                net_key=net_key,
            )


def _via_span(slice_keys: Sequence[str]) -> tuple[str, str]:
    return (slice_keys[0], slice_keys[-1])


def _materialize_vias(context: _Context, source: BoardSource) -> None:
    wants_copper = context.request.wants(MaterialProduct.COPPER)
    wants_holes = context.request.wants(MaterialProduct.HOLES)
    for via in source.collection("vias"):
        size = _nm(getattr(via, "size", 0.0))
        if size <= 0:
            continue
        layer_names = expand_layer_names(
            via.layers, context.copper_layer_names
        )
        slice_keys = context.slice_keys_for(layer_names)
        if not slice_keys:
            continue
        net_key = context.net_key(via)
        placement = Affine2D.translation(*_point_nm(via.at_x, via.at_y))
        if wants_copper:
            land = Disk2D(radius_nm=_half_nm(size))
            for slice_key in slice_keys:
                context.add_copper(
                    slice_key=slice_key,
                    geometry=land,
                    affine=placement,
                    net_key=net_key,
                )
        drill = _nm(getattr(via, "drill", 0.0))
        if wants_holes and drill > 0:
            start_key, end_key = _via_span(slice_keys)
            context.holes.append(
                HoleOperation2D(
                    plating=HolePlating.PLATED,
                    start_slice_key=start_key,
                    end_slice_key=end_key,
                    geometry=Disk2D(radius_nm=_half_nm(drill)),
                    affine=placement,
                    net_key=net_key,
                )
            )


def _materialize_zones(context: _Context, source: BoardSource) -> None:
    for zone in source.collection("zones"):
        net_key = context.net_key(zone)
        for filled in getattr(zone, "filled_polygons", ()) or ():
            slice_keys = context.slice_keys_for(
                expand_layer_names(
                    [getattr(filled, "layer", "")], context.copper_layer_names
                )
            )
            if not slice_keys:
                continue
            outer = getattr(filled, "outer_nm", None) or ring_to_nm(
                getattr(filled, "points", ()) or ()
            )
            if len(outer) < 3:
                continue
            geometry = PlanarRegion2D(outer_nm=outer)
            for slice_key in slice_keys:
                context.add_copper(
                    slice_key=slice_key,
                    geometry=geometry,
                    affine=Affine2D.identity(),
                    net_key=net_key,
                )


# --- pads ------------------------------------------------------------------


def _oval_capsule(size_x: float, size_y: float) -> Capsule2D:
    """An oval pad as a stadium in its own unrotated frame.

    KiCad sweeps the shorter axis along the longer one, so the capsule lies
    along X for a wide pad and along Y for a tall one. The placement rotation
    stays in the affine.
    """
    if size_x > size_y:
        delta = _half_nm(size_x - size_y)
        return Capsule2D(
            start_nm=(-delta, 0), end_nm=(delta, 0),
            width_nm=int(round(size_y)),
        )
    delta = _half_nm(size_y - size_x)
    return Capsule2D(
        start_nm=(0, -delta), end_nm=(0, delta),
        width_nm=int(round(size_x)),
    )


def _round_rect_shape(pad: Any, size_x: float, size_y: float) -> Shape2D:
    ratio = pad.roundrect_rratio
    ratio = _DEFAULT_ROUNDRECT_RRATIO if ratio is None else float(ratio)
    radius = min(size_x, size_y) * ratio
    corners = frozenset(getattr(pad, "chamfer_corners", ()) or ())
    chamfer_ratio = float(getattr(pad, "chamfer_ratio", 0.0) or 0.0)
    epsilon = _ROUND_RECT_EPSILON_MM * NM_PER_MM
    width = int(round(size_x))
    height = int(round(size_y))
    if radius < epsilon and corners and chamfer_ratio > 0:
        return ChamferedRect2D(
            width_nm=width,
            height_nm=height,
            corner_radius_nm=0,
            chamfer_ratio=chamfer_ratio,
            chamfer_corners=corners,
        )
    if radius < epsilon:
        return Rect2D(width_nm=width, height_nm=height)
    return RoundedRect2D(
        width_nm=width, height_nm=height, corner_radius_nm=int(round(radius))
    )


def _custom_pad_region(pad: Any) -> Shape2D | None:
    rings: list[tuple[tuple[int, int], ...]] = []
    for primitive in getattr(pad, "custom_primitives", ()) or ():
        points = getattr(primitive, "points", ()) or ()
        # ring_to_nm converts millimetres, so hand it the raw pad-local points.
        ring = ring_to_nm(
            (float(point[0]), float(point[1]))
            for point in points
            if len(point) >= 2
        )
        if len(ring) >= 3:
            rings.append(ring)
    if not rings:
        return None
    return PlanarRegion2D(outer_nm=max(rings, key=len))


def _pad_shape(pad: Any) -> Shape2D | None:
    """The pad land in its own unrotated, origin-centered frame."""
    size_x = _nm(getattr(pad, "size_x", 0.0))
    size_y = _nm(getattr(pad, "size_y", 0.0))
    shape = getattr(pad, "shape", None)
    if shape is PadShape.CUSTOM:
        return _custom_pad_region(pad)
    if size_x <= 0 or size_y <= 0:
        return None
    if shape is PadShape.CIRCLE:
        return Disk2D(radius_nm=_half_nm(size_x))
    if shape is PadShape.RECT:
        return Rect2D(width_nm=int(round(size_x)), height_nm=int(round(size_y)))
    if shape is PadShape.OVAL:
        return _oval_capsule(size_x, size_y)
    if shape is PadShape.TRAPEZOID:
        return Trapezoid2D(
            width_nm=int(round(size_x)),
            height_nm=int(round(size_y)),
            delta_x_nm=_nmi(getattr(pad, "rect_delta_x", 0.0)),
            delta_y_nm=_nmi(getattr(pad, "rect_delta_y", 0.0)),
        )
    if shape is PadShape.ROUNDRECT:
        return _round_rect_shape(pad, size_x, size_y)
    return None


def _footprint_affine(footprint: Any) -> Affine2D:
    """Footprint-local to board. KiCad rotates by the negated placement angle."""
    return Affine2D.rotation(
        -float(getattr(footprint, "at_angle", 0.0) or 0.0)
    ).then(
        Affine2D.translation(
            *_point_nm(
                getattr(footprint, "at_x", 0.0),
                getattr(footprint, "at_y", 0.0),
            )
        )
    )


def _pad_affine(pad: Any, relative_angle: float, to_board: Affine2D) -> Affine2D:
    """Pad-local to board, with rotation carried in the transform.

    Board-embedded pads store absolute orientation, so the caller supplies the
    angle relative to the footprint. Because the rotation lives here rather
    than in the shape, nothing has to be mutated on the pad to place it.
    """
    return (
        Affine2D.rotation(-relative_angle)
        .then(
            Affine2D.translation(
                *_point_nm(
                    getattr(pad, "at_x", 0.0), getattr(pad, "at_y", 0.0)
                )
            )
        )
        .then(to_board)
    )


def _pad_hole_geometry(pad: Any) -> Shape2D | None:
    drill = _nm(getattr(pad, "drill", 0.0))
    width = _nm(getattr(pad, "drill_width", 0.0)) or drill
    height = _nm(getattr(pad, "drill_height", 0.0)) or drill
    if width <= 0 or height <= 0:
        return None
    if abs(width - height) <= 1e-6:
        return Disk2D(radius_nm=_half_nm(width))
    return _oval_capsule(width, height)


def _pad_hole_affine(pad: Any, pad_to_board: Affine2D) -> Affine2D:
    offset = Affine2D.translation(
        *_point_nm(
            getattr(pad, "drill_offset_x", 0.0),
            getattr(pad, "drill_offset_y", 0.0),
        )
    )
    return offset.then(pad_to_board)


def _emit_pad_hole(
    context: _Context,
    pad: Any,
    pad_to_board: Affine2D,
    slice_keys: Sequence[str],
    net_key: str | None,
) -> None:
    geometry = _pad_hole_geometry(pad)
    if geometry is None:
        return
    plated = getattr(pad, "pad_type", None) is not PadType.NP_THRU_HOLE
    context.holes.append(
        HoleOperation2D(
            plating=HolePlating.PLATED if plated else HolePlating.UNPLATED,
            start_slice_key=slice_keys[0],
            end_slice_key=slice_keys[-1],
            geometry=geometry,
            affine=_pad_hole_affine(pad, pad_to_board),
            net_key=net_key if plated else None,
        )
    )


def _materialize_pad(
    context: _Context,
    pad: Any,
    footprint_angle: float,
    to_board: Affine2D,
) -> None:
    layer_names = expand_layer_names(
        getattr(pad, "layers", ()) or (), context.copper_layer_names
    )
    slice_keys = context.slice_keys_for(layer_names)
    if not slice_keys:
        return
    absolute_angle = float(getattr(pad, "at_angle", 0.0) or 0.0)
    pad_to_board = _pad_affine(
        pad, absolute_angle - footprint_angle, to_board
    )
    net_key = context.net_key(pad)
    geometry = _pad_shape(pad)
    if geometry is not None and context.request.wants(MaterialProduct.COPPER):
        for slice_key in slice_keys:
            context.add_copper(
                slice_key=slice_key,
                geometry=geometry,
                affine=pad_to_board,
                net_key=net_key,
            )
    if context.request.wants(MaterialProduct.HOLES):
        _emit_pad_hole(context, pad, pad_to_board, slice_keys, net_key)


def _materialize_pads(context: _Context, source: BoardSource) -> None:
    for footprint in source.collection("footprints"):
        to_board = _footprint_affine(footprint)
        footprint_angle = float(getattr(footprint, "at_angle", 0.0) or 0.0)
        for pad in getattr(footprint, "pads", ()) or ():
            _materialize_pad(context, pad, footprint_angle, to_board)


# --- entry point -----------------------------------------------------------


def _build_scope(
    request: MaterializationRequest,
    stack: PcbMaterialStack,
) -> MaterializationScope:
    conductors = stack.conductor_keys()
    requested = request.slice_keys or conductors
    complete = (
        request.products == ALL_PRODUCTS
        and set(conductors).issubset(set(requested))
    )
    return MaterializationScope(
        products=request.products,
        slice_keys=tuple(requested),
        complete_board=complete,
    )


def _collect_nets(context: _Context) -> tuple[MaterializedNet, ...]:
    return tuple(
        MaterializedNet(key=name, name=name)
        for name in sorted(context.net_names)
    )


def _indexed(
    operations: Iterable[MaterializedOperation],
) -> tuple[MaterializedOperation, ...]:
    return tuple(
        replace(operation, index=position)
        for position, operation in enumerate(operations)
    )


def materialize_pcb(
    source: str | Path | KiCadPcb | KiCadPcbProjection,
    *,
    request: MaterializationRequest | None = None,
) -> PcbAnalyticMaterialization:
    """Materialize a board into analytic physical operations.

    Path inputs open a :class:`KiCadPcbProjection` and read only the families
    needed. Callers holding a :class:`KiCadPcb` or projection may pass those
    directly; both inputs produce identical analytic results.
    """
    request = request or MaterializationRequest.complete()
    board = coerce_source(source)
    copper_layer_names = _copper_layer_names(board)
    stack = build_material_stack(board.stackup(), copper_layer_names)
    context = _Context(
        request=request,
        stack=stack,
        copper_layer_names=copper_layer_names,
    )
    if request.wants(MaterialProduct.COPPER):
        _materialize_tracks(context, board)
        _materialize_arcs(context, board)
        _materialize_zones(context, board)
    _materialize_vias(context, board)
    _materialize_pads(context, board)
    return PcbAnalyticMaterialization(
        scope=_build_scope(request, stack),
        nets=_collect_nets(context),
        slices=stack.slices,
        operations=_indexed([*context.surfaces, *context.holes]),
        source_path=board.source_path,
    )

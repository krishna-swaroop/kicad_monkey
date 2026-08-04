"""Immutable analytic geometry value types for PCB materialization.

This module is deliberately not part of the promoted package API.

The types here form a small closed shape union. They stay analytic: a disk is a
radius, not a sampled polygon; a swept path keeps its line and circular-arc
centerline segments rather than a flattened outline. Polygonization,
tessellation, and boolean flattening are lossy terminal operations, so they
belong in an output adapter, not here.

Coordinates are integer nanometres. Angles are degrees. Every shape is
expressed in its own local frame; placement into board space is carried by a
separate :class:`Affine2D`, so a rotated pad and an unrotated one share one
shape description.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from enum import Enum
from typing import Iterable, Sequence, Union

from .kicad_pcb_projection_source import NmPoint, NmRing

_CHAMFER_CORNERS = frozenset(
    {"top_left", "top_right", "bottom_left", "bottom_right"}
)


class PathCap(str, Enum):
    """End treatment for a swept path."""

    BUTT = "butt"
    ROUND = "round"
    SQUARE = "square"


class PathJoin(str, Enum):
    """Join treatment between adjacent swept-path segments."""

    MITER = "miter"
    ROUND = "round"
    BEVEL = "bevel"


def _as_nm(value: object) -> int:
    return int(round(float(value)))  # type: ignore[arg-type]


def _as_point(value: Sequence[object]) -> NmPoint:
    return (_as_nm(value[0]), _as_nm(value[1]))


def _as_ring(points: Iterable[Sequence[object]]) -> NmRing:
    return tuple(_as_point(point) for point in points)


def _require_non_negative(name: str, value: int) -> None:
    if value < 0:
        raise ValueError(f"{name} must be non-negative, got {value}")


def _require_positive(name: str, value: int) -> None:
    if value <= 0:
        raise ValueError(f"{name} must be positive, got {value}")


@dataclass(frozen=True, slots=True)
class Affine2D:
    """A full 2x3 affine map from a local frame into board space.

    Follows the SVG/PostScript ordering ``matrix(a b c d e f)``::

        x' = a * x + c * y + e
        y' = b * x + d * y + f

    Rotation, reflection, translation, non-uniform scale, and shear are all
    ordinary cases. A disk plus an affine already represents an ellipse
    exactly, which is why there is no separate ellipse root.

    The translation components are held as nanometre floats rather than
    integers. Rounding them at every :meth:`then` would make composition
    non-associative and leave a one-nanometre error trail; rounding happens
    once, at :meth:`apply` and at serialization.
    """

    a: float = 1.0
    b: float = 0.0
    c: float = 0.0
    d: float = 1.0
    e: float = 0.0
    f: float = 0.0

    def __post_init__(self) -> None:
        object.__setattr__(self, "a", float(self.a))
        object.__setattr__(self, "b", float(self.b))
        object.__setattr__(self, "c", float(self.c))
        object.__setattr__(self, "d", float(self.d))
        object.__setattr__(self, "e", float(self.e))
        object.__setattr__(self, "f", float(self.f))

    @classmethod
    def identity(cls) -> "Affine2D":
        return cls()

    @classmethod
    def translation(cls, dx_nm: float, dy_nm: float) -> "Affine2D":
        return cls(1.0, 0.0, 0.0, 1.0, float(dx_nm), float(dy_nm))

    @classmethod
    def rotation(cls, degrees: float) -> "Affine2D":
        radians = math.radians(float(degrees))
        cos = math.cos(radians)
        sin = math.sin(radians)
        return cls(cos, sin, -sin, cos, 0, 0)

    @classmethod
    def scale(cls, sx: float, sy: float | None = None) -> "Affine2D":
        return cls(float(sx), 0.0, 0.0, float(sx if sy is None else sy), 0, 0)

    @classmethod
    def mirror_x(cls) -> "Affine2D":
        """Reflect across the X axis (negate Y)."""
        return cls(1.0, 0.0, 0.0, -1.0, 0, 0)

    @classmethod
    def mirror_y(cls) -> "Affine2D":
        """Reflect across the Y axis (negate X)."""
        return cls(-1.0, 0.0, 0.0, 1.0, 0, 0)

    def then(self, other: "Affine2D") -> "Affine2D":
        """Return the map that applies ``self`` first, then ``other``."""
        return Affine2D(
            a=other.a * self.a + other.c * self.b,
            b=other.b * self.a + other.d * self.b,
            c=other.a * self.c + other.c * self.d,
            d=other.b * self.c + other.d * self.d,
            e=other.a * self.e + other.c * self.f + other.e,
            f=other.b * self.e + other.d * self.f + other.f,
        )

    def apply(self, point: Sequence[float]) -> NmPoint:
        x = float(point[0])
        y = float(point[1])
        return (
            _as_nm(self.a * x + self.c * y + self.e),
            _as_nm(self.b * x + self.d * y + self.f),
        )

    @property
    def determinant(self) -> float:
        return self.a * self.d - self.b * self.c

    @property
    def is_mirrored(self) -> bool:
        return self.determinant < 0.0

    @property
    def translation_nm(self) -> NmPoint:
        """The translation rounded to integer nanometres, for serialization."""
        return (_as_nm(self.e), _as_nm(self.f))

    @property
    def is_identity(self) -> bool:
        return (
            (self.a, self.b, self.c, self.d, self.e, self.f)
            == (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
        )


@dataclass(frozen=True, slots=True)
class Disk2D:
    """A filled circle."""

    radius_nm: int
    center_nm: NmPoint = (0, 0)

    def __post_init__(self) -> None:
        object.__setattr__(self, "radius_nm", _as_nm(self.radius_nm))
        object.__setattr__(self, "center_nm", _as_point(self.center_nm))
        _require_positive("radius_nm", self.radius_nm)


@dataclass(frozen=True, slots=True)
class Annulus2D:
    """A filled ring between two concentric circles."""

    outer_radius_nm: int
    inner_radius_nm: int
    center_nm: NmPoint = (0, 0)

    def __post_init__(self) -> None:
        object.__setattr__(
            self, "outer_radius_nm", _as_nm(self.outer_radius_nm)
        )
        object.__setattr__(
            self, "inner_radius_nm", _as_nm(self.inner_radius_nm)
        )
        object.__setattr__(self, "center_nm", _as_point(self.center_nm))
        _require_positive("outer_radius_nm", self.outer_radius_nm)
        _require_non_negative("inner_radius_nm", self.inner_radius_nm)
        if self.inner_radius_nm >= self.outer_radius_nm:
            raise ValueError("inner_radius_nm must be less than outer_radius_nm")


@dataclass(frozen=True, slots=True)
class Rect2D:
    """An axis-aligned rectangle in its local frame."""

    width_nm: int
    height_nm: int
    center_nm: NmPoint = (0, 0)

    def __post_init__(self) -> None:
        object.__setattr__(self, "width_nm", _as_nm(self.width_nm))
        object.__setattr__(self, "height_nm", _as_nm(self.height_nm))
        object.__setattr__(self, "center_nm", _as_point(self.center_nm))
        _require_positive("width_nm", self.width_nm)
        _require_positive("height_nm", self.height_nm)


@dataclass(frozen=True, slots=True)
class RoundedRect2D:
    """A rectangle with four equal circular corner fillets."""

    width_nm: int
    height_nm: int
    corner_radius_nm: int
    center_nm: NmPoint = (0, 0)

    def __post_init__(self) -> None:
        object.__setattr__(self, "width_nm", _as_nm(self.width_nm))
        object.__setattr__(self, "height_nm", _as_nm(self.height_nm))
        object.__setattr__(
            self, "corner_radius_nm", _as_nm(self.corner_radius_nm)
        )
        object.__setattr__(self, "center_nm", _as_point(self.center_nm))
        _require_positive("width_nm", self.width_nm)
        _require_positive("height_nm", self.height_nm)
        _require_non_negative("corner_radius_nm", self.corner_radius_nm)


@dataclass(frozen=True, slots=True)
class ChamferedRect2D:
    """A rounded rectangle with one or more corners chamfered flat."""

    width_nm: int
    height_nm: int
    corner_radius_nm: int
    chamfer_ratio: float = 0.0
    chamfer_corners: frozenset[str] = frozenset()
    center_nm: NmPoint = (0, 0)

    def __post_init__(self) -> None:
        object.__setattr__(self, "width_nm", _as_nm(self.width_nm))
        object.__setattr__(self, "height_nm", _as_nm(self.height_nm))
        object.__setattr__(
            self, "corner_radius_nm", _as_nm(self.corner_radius_nm)
        )
        object.__setattr__(self, "chamfer_ratio", float(self.chamfer_ratio))
        object.__setattr__(
            self, "chamfer_corners", frozenset(self.chamfer_corners)
        )
        object.__setattr__(self, "center_nm", _as_point(self.center_nm))
        _require_positive("width_nm", self.width_nm)
        _require_positive("height_nm", self.height_nm)
        _require_non_negative("corner_radius_nm", self.corner_radius_nm)
        unknown = self.chamfer_corners - _CHAMFER_CORNERS
        if unknown:
            raise ValueError(f"unknown chamfer corners: {sorted(unknown)}")


@dataclass(frozen=True, slots=True)
class Trapezoid2D:
    """A rectangle sheared by a per-axis delta, as KiCad trapezoid pads are."""

    width_nm: int
    height_nm: int
    delta_x_nm: int = 0
    delta_y_nm: int = 0
    center_nm: NmPoint = (0, 0)

    def __post_init__(self) -> None:
        object.__setattr__(self, "width_nm", _as_nm(self.width_nm))
        object.__setattr__(self, "height_nm", _as_nm(self.height_nm))
        object.__setattr__(self, "delta_x_nm", _as_nm(self.delta_x_nm))
        object.__setattr__(self, "delta_y_nm", _as_nm(self.delta_y_nm))
        object.__setattr__(self, "center_nm", _as_point(self.center_nm))
        _require_positive("width_nm", self.width_nm)
        _require_positive("height_nm", self.height_nm)


@dataclass(frozen=True, slots=True)
class Capsule2D:
    """A stadium: a segment swept by a circle. KiCad tracks and oval pads."""

    start_nm: NmPoint
    end_nm: NmPoint
    width_nm: int

    def __post_init__(self) -> None:
        object.__setattr__(self, "start_nm", _as_point(self.start_nm))
        object.__setattr__(self, "end_nm", _as_point(self.end_nm))
        object.__setattr__(self, "width_nm", _as_nm(self.width_nm))
        _require_positive("width_nm", self.width_nm)


@dataclass(frozen=True, slots=True)
class LineSegment2D:
    """A straight centerline segment."""

    start_nm: NmPoint
    end_nm: NmPoint

    def __post_init__(self) -> None:
        object.__setattr__(self, "start_nm", _as_point(self.start_nm))
        object.__setattr__(self, "end_nm", _as_point(self.end_nm))


@dataclass(frozen=True, slots=True)
class ArcSegment2D:
    """A circular centerline arc, stored three-point as KiCad authors it."""

    start_nm: NmPoint
    mid_nm: NmPoint
    end_nm: NmPoint

    def __post_init__(self) -> None:
        object.__setattr__(self, "start_nm", _as_point(self.start_nm))
        object.__setattr__(self, "mid_nm", _as_point(self.mid_nm))
        object.__setattr__(self, "end_nm", _as_point(self.end_nm))


PathSegment2D = Union[LineSegment2D, ArcSegment2D]


@dataclass(frozen=True, slots=True)
class SweptPath2D:
    """A centerline of line and circular-arc segments swept by a width."""

    segments: tuple[PathSegment2D, ...]
    width_nm: int
    cap: PathCap = PathCap.ROUND
    join: PathJoin = PathJoin.ROUND

    def __post_init__(self) -> None:
        object.__setattr__(self, "segments", tuple(self.segments))
        object.__setattr__(self, "width_nm", _as_nm(self.width_nm))
        object.__setattr__(self, "cap", PathCap(self.cap))
        object.__setattr__(self, "join", PathJoin(self.join))
        _require_positive("width_nm", self.width_nm)
        if not self.segments:
            raise ValueError("SweptPath2D requires at least one segment")


@dataclass(frozen=True, slots=True)
class PlanarRegion2D:
    """An outer ring with optional hole rings. Rings are unclosed.

    This is the escape hatch for geometry that is genuinely not analytic --
    realized zone fills and custom pad primitives -- and the natural input to a
    planar boolean solver.
    """

    outer_nm: NmRing
    holes_nm: tuple[NmRing, ...] = field(default=())

    def __post_init__(self) -> None:
        object.__setattr__(self, "outer_nm", _as_ring(self.outer_nm))
        object.__setattr__(
            self, "holes_nm", tuple(_as_ring(ring) for ring in self.holes_nm)
        )
        if len(self.outer_nm) < 3:
            raise ValueError("PlanarRegion2D outer ring requires 3+ points")


Shape2D = Union[
    Disk2D,
    Annulus2D,
    Rect2D,
    RoundedRect2D,
    ChamferedRect2D,
    Trapezoid2D,
    Capsule2D,
    SweptPath2D,
    PlanarRegion2D,
]

SHAPE_KIND_BY_TYPE: dict[type, str] = {
    Disk2D: "disk_2d",
    Annulus2D: "annulus_2d",
    Rect2D: "rect_2d",
    RoundedRect2D: "rounded_rect_2d",
    ChamferedRect2D: "chamfered_rect_2d",
    Trapezoid2D: "trapezoid_2d",
    Capsule2D: "capsule_2d",
    SweptPath2D: "swept_path_2d",
    PlanarRegion2D: "planar_region_2d",
}


def shape_kind(shape: Shape2D) -> str:
    """Return the contract token for a shape instance."""
    kind = SHAPE_KIND_BY_TYPE.get(type(shape))
    if kind is None:
        raise TypeError(f"not an analytic shape: {type(shape).__name__}")
    return kind

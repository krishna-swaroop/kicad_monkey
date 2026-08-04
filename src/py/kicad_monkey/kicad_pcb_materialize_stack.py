"""Rigid-board material stack model for PCB materialization.

This module is deliberately not part of the promoted package API.

One ordered stack describes the whole board. Every layer in it carries a
canonical key, a physical role, an order, an optional thickness, and optional
dielectric properties. Z positions are derived by walking the thicknesses.

Unknown thickness stays ``None``. KiCad writes ``0`` for silkscreen and paste
because it does not model their thickness; recording that as a real zero would
be a guess dressed up as data, so it is reported as unknown instead.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from enum import Enum
from typing import Any, Iterable, Sequence

from .kicad_pcb_projection_source import NM_PER_MM, is_copper_layer

_INNER_LAYER_PATTERN = re.compile(r"^In(\d+)\.Cu$")
_DIELECTRIC_INDEX_PATTERN = re.compile(r"(\d+)\s*$")


class MaterialRole(str, Enum):
    """The physical role one layer of the stack plays in the board."""

    CONDUCTOR = "conductor"
    DIELECTRIC = "dielectric"
    MASK = "mask"
    PASTE = "paste"
    SILKSCREEN = "silkscreen"


class MaterialKind(str, Enum):
    """The substance an operation deposits or removes.

    Distinct from :class:`MaterialRole`: a stack layer plays the role of
    conductor, and the material laid onto it is copper.
    """

    COPPER = "copper"
    SOLDER_MASK = "solder_mask"
    PASTE = "paste"
    SILKSCREEN = "silkscreen"


_BULK_ROLES = frozenset({MaterialRole.CONDUCTOR, MaterialRole.DIELECTRIC})

_ROLE_BY_TYPE_TOKEN: tuple[tuple[str, MaterialRole], ...] = (
    ("copper", MaterialRole.CONDUCTOR),
    ("core", MaterialRole.DIELECTRIC),
    ("prepreg", MaterialRole.DIELECTRIC),
    ("dielectric", MaterialRole.DIELECTRIC),
    ("solder mask", MaterialRole.MASK),
    ("solder paste", MaterialRole.PASTE),
    ("silk screen", MaterialRole.SILKSCREEN),
    ("silkscreen", MaterialRole.SILKSCREEN),
)


def _role_for_type(type_name: str) -> MaterialRole | None:
    token = str(type_name or "").strip().casefold()
    if not token:
        return None
    for needle, role in _ROLE_BY_TYPE_TOKEN:
        if needle in token:
            return role
    return None


def copper_slice_key(layer_name: str) -> str:
    """Canonical key for a KiCad copper layer name."""
    name = str(layer_name or "")
    if name == "F.Cu":
        return "copper.front"
    if name == "B.Cu":
        return "copper.back"
    inner = _INNER_LAYER_PATTERN.match(name)
    if inner is not None:
        return f"copper.in{int(inner.group(1))}"
    return f"copper.{name.casefold().removesuffix('.cu')}"


def _side_suffix(layer_name: str) -> str:
    name = str(layer_name or "")
    if name.startswith("F."):
        return "front"
    if name.startswith("B."):
        return "back"
    return name.casefold().replace(".", "_")


def _dielectric_key(layer_name: str, ordinal: int) -> str:
    match = _DIELECTRIC_INDEX_PATTERN.search(str(layer_name or ""))
    index = int(match.group(1)) if match is not None else ordinal
    return f"dielectric.{index}"


def _slice_key(layer_name: str, role: MaterialRole, ordinal: int) -> str:
    if role is MaterialRole.CONDUCTOR:
        return copper_slice_key(layer_name)
    if role is MaterialRole.DIELECTRIC:
        return _dielectric_key(layer_name, ordinal)
    return f"{role.value}.{_side_suffix(layer_name)}"


def _thickness_nm(value: object) -> int | None:
    """Millimetre thickness to nanometres, treating non-positive as unknown."""
    try:
        thickness = float(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return None
    if thickness <= 0.0:
        return None
    return int(round(thickness * NM_PER_MM))


@dataclass(frozen=True, slots=True)
class DielectricProperties:
    """Electrical properties of a dielectric slice."""

    material_name: str | None = None
    relative_permittivity: float | None = None
    loss_tangent: float | None = None

    @property
    def is_empty(self) -> bool:
        return (
            not self.material_name
            and self.relative_permittivity is None
            and self.loss_tangent is None
        )


@dataclass(frozen=True, slots=True)
class MaterialSlice:
    """One physical layer of the board stack."""

    key: str
    role: MaterialRole
    order: int
    thickness_nm: int | None = None
    dielectric: DielectricProperties | None = None
    source_layer_name: str = ""

    def __post_init__(self) -> None:
        object.__setattr__(self, "key", str(self.key))
        object.__setattr__(self, "role", MaterialRole(self.role))
        object.__setattr__(self, "order", int(self.order))


def _dielectric_from_layer(layer: Any) -> DielectricProperties | None:
    material = getattr(layer, "material", None) or None
    properties = DielectricProperties(
        material_name=str(material) if material else None,
        relative_permittivity=getattr(layer, "epsilon_r", None),
        loss_tangent=getattr(layer, "loss_tangent", None),
    )
    return None if properties.is_empty else properties


@dataclass(frozen=True, slots=True)
class PcbMaterialStack:
    """An ordered rigid-board stack."""

    slices: tuple[MaterialSlice, ...] = ()

    def __post_init__(self) -> None:
        object.__setattr__(self, "slices", tuple(self.slices))

    @property
    def keys(self) -> tuple[str, ...]:
        return tuple(item.key for item in self.slices)

    def by_key(self, key: str) -> MaterialSlice | None:
        for item in self.slices:
            if item.key == key:
                return item
        return None

    def conductor_keys(self) -> tuple[str, ...]:
        return tuple(
            item.key
            for item in self.slices
            if item.role is MaterialRole.CONDUCTOR
        )

    @property
    def has_complete_thickness(self) -> bool:
        return all(item.thickness_nm is not None for item in self.slices)

    def z_span_nm(self, key: str) -> tuple[int, int] | None:
        """Top and bottom Z of a slice, walking the stack from the front.

        Returns ``None`` when a *bulk* layer above the requested one has
        unknown thickness, because every position below it would then be a
        guess. Surface roles (mask, paste, silkscreen) with unknown thickness
        contribute zero instead: KiCad writes ``0`` for them because it does
        not model their thickness, not because the stack is underspecified.
        """
        offset = 0
        for item in self.slices:
            thickness = item.thickness_nm
            if thickness is None:
                if item.role in _BULK_ROLES:
                    return None
                thickness = 0
            if item.key == key:
                return (offset, offset + thickness)
            offset += thickness
        return None

    @classmethod
    def from_stackup(cls, stackup: Any) -> "PcbMaterialStack":
        """Build from a parsed KiCad ``(stackup ...)`` form."""
        slices: list[MaterialSlice] = []
        for layer in getattr(stackup, "layers", ()) or ():
            role = _role_for_type(getattr(layer, "type_name", ""))
            if role is None:
                continue
            name = str(getattr(layer, "name", "") or "")
            order = len(slices)
            slices.append(
                MaterialSlice(
                    key=_slice_key(name, role, order),
                    role=role,
                    order=order,
                    thickness_nm=_thickness_nm(
                        getattr(layer, "thickness", None)
                    ),
                    dielectric=(
                        _dielectric_from_layer(layer)
                        if role is MaterialRole.DIELECTRIC
                        else None
                    ),
                    source_layer_name=name,
                )
            )
        return cls(slices=tuple(slices))

    @classmethod
    def from_copper_layers(
        cls,
        copper_layer_names: Sequence[str],
    ) -> "PcbMaterialStack":
        """Fallback for boards that declare no stackup: conductors only."""
        return cls(
            slices=tuple(
                MaterialSlice(
                    key=copper_slice_key(name),
                    role=MaterialRole.CONDUCTOR,
                    order=order,
                    thickness_nm=None,
                    source_layer_name=name,
                )
                for order, name in enumerate(copper_layer_names)
            )
        )


def build_material_stack(
    stackup: Any,
    copper_layer_names: Sequence[str],
) -> PcbMaterialStack:
    """Build a stack, falling back to conductors when no stackup is declared."""
    stack = PcbMaterialStack.from_stackup(stackup)
    if stack.conductor_keys():
        return stack
    return PcbMaterialStack.from_copper_layers(copper_layer_names)


def copper_layer_slice_keys(
    copper_layer_names: Iterable[str],
) -> dict[str, str]:
    """Map KiCad copper layer names to canonical conductor keys."""
    return {
        str(name): copper_slice_key(str(name))
        for name in copper_layer_names
        if is_copper_layer(str(name))
    }

"""Internal board-source adapters shared by PCB geometry consumers.

This module is deliberately not part of the promoted package API. It holds the
optimized projection extraction used to read copper-relevant families out of a
``.kicad_pcb`` without hydrating a complete :class:`KiCadPcb`:

* a slim, pads-only footprint parse that skips graphics, text, and 3D models;
* direct nanometre extraction of ``(filled_polygon ...)`` rings, bypassing the
  s-expression object model entirely;
* a uniform :class:`BoardSource` shim so downstream emitters can iterate a
  projection, a slim projection, or a full board with one call shape.

These are implementation details. Callers outside this package should use a
promoted entry point rather than importing from here.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Any, Iterable, Sequence

from .kicad_base import unquote_string
from .kicad_pad import Pad
from .kicad_pcb import KiCadPcb
from .kicad_pcb_footprint import Footprint
from .kicad_pcb_other import Layer, Net, NetRef, Stackup
from .kicad_pcb_projection import KiCadPcbProjection
from .kicad_pcb_routing import Arc, Segment, Via
from .kicad_property import Property
from .kicad_sexpr import SexpFormSpan, SexpSelector, iter_sexp_form_spans

NM_PER_MM = 1_000_000

NmPoint = tuple[int, int]
NmRing = tuple[NmPoint, ...]


def enum_value(value: object) -> str:
    if isinstance(value, Enum):
        return str(value.value)
    return str(value or "")


def mm_to_nm(value: float) -> int:
    return int(round(float(value) * NM_PER_MM))


def ring_to_nm(points: Iterable[tuple[float, float]]) -> NmRing:
    """Convert millimetre points to an unclosed integer-nanometre ring."""
    output: list[NmPoint] = []
    for x, y in points:
        point = (mm_to_nm(x), mm_to_nm(y))
        if not output or output[-1] != point:
            output.append(point)
    if len(output) > 1 and output[0] == output[-1]:
        output.pop()
    return tuple(output)


@dataclass(slots=True)
class SlimFilledPolygon:
    layer: str
    island: bool
    outer_nm: NmRing


@dataclass(slots=True)
class SlimZone:
    net: NetRef
    layers: list[str]
    uuid: str | None
    filled_polygons: list[SlimFilledPolygon]


@dataclass(slots=True)
class SlimProjectionSource:
    source_path: Path | None
    layers: list[Layer]
    nets: list[Net]
    segments: list[Segment]
    arcs: list[Arc]
    vias: list[Via]
    zones: list[SlimZone]
    footprints: list[Footprint]
    stackup: Stackup | None


_NUMBER_PATTERN = r"[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?"
_XY_PATTERN = re.compile(
    rf"\(\s*xy\s+({_NUMBER_PATTERN})\s+({_NUMBER_PATTERN})\s*\)"
)
_FILLED_LAYER_PATTERN = re.compile(
    r'\(\s*layer\s+(?:"((?:\\.|[^"])*)"|([^\s()]+))\s*\)'
)
_ISLAND_PATTERN = re.compile(r"\(\s*island(?:\s|\))")
_AT_PATTERN = re.compile(
    rf"\(\s*at\s+({_NUMBER_PATTERN})\s+({_NUMBER_PATTERN})"
    rf"(?:\s+({_NUMBER_PATTERN}))?"
)
_UUID_PATTERN = re.compile(
    r'\(\s*uuid\s+(?:"((?:\\.|[^"])*)"|([^\s()]+))\s*\)'
)
_PROPERTY_PATTERN = re.compile(
    r'^\(\s*property\s+"((?:\\.|[^"])*)"\s+"((?:\\.|[^"])*)"'
)

_FOOTPRINT_HEADS = ("footprint", "module")
_SPAN_PARENT_HEADS = frozenset({"footprint", "module", "zone"})


def parse_filled_polygon_span(span: SexpFormSpan) -> SlimFilledPolygon:
    """Scrape a ``(filled_polygon ...)`` form straight to nanometre points."""
    text = span.text()
    layer_match = _FILLED_LAYER_PATTERN.search(text)
    quoted_layer = layer_match.group(1) if layer_match is not None else None
    bare_layer = layer_match.group(2) if layer_match is not None else None
    layer = (
        quoted_layer
        if quoted_layer is not None
        else unquote_string(bare_layer or "")
    )
    outer_nm = ring_to_nm(
        (float(match.group(1)), float(match.group(2)))
        for match in _XY_PATTERN.finditer(text)
    )
    return SlimFilledPolygon(
        layer=layer,
        island=_ISLAND_PATTERN.search(text) is not None,
        outer_nm=outer_nm,
    )


def _resolve_net_ref(
    net_ref: NetRef,
    *,
    name_by_ordinal: dict[int, str],
    ordinal_by_name: dict[str, int],
) -> NetRef:
    return net_ref.resolve_name(name_by_ordinal).resolve_ordinal(ordinal_by_name)


def _parse_slim_at(child: SexpFormSpan) -> tuple[float, float, float] | None:
    match = _AT_PATTERN.match(child.text())
    if match is None:
        return None
    angle = float(match.group(3)) if match.group(3) is not None else 0.0
    return float(match.group(1)), float(match.group(2)), angle


def _parse_slim_pad(
    child: SexpFormSpan,
    *,
    name_by_ordinal: dict[int, str],
    ordinal_by_name: dict[str, int],
) -> Pad | None:
    parsed = child.parse()
    if not isinstance(parsed, list) or not parsed:
        return None
    pad = Pad.from_sexp(parsed)
    pad.net = _resolve_net_ref(
        pad.net,
        name_by_ordinal=name_by_ordinal,
        ordinal_by_name=ordinal_by_name,
    )
    return pad


def parse_slim_footprint(
    _parent: SexpFormSpan,
    children: Sequence[SexpFormSpan],
    *,
    name_by_ordinal: dict[int, str],
    ordinal_by_name: dict[str, int],
) -> Footprint:
    """Build a pads-only footprint, skipping graphics, text, and 3D models."""
    at_x = 0.0
    at_y = 0.0
    at_angle = 0.0
    uuid: str | None = None
    properties: list[Property] = []
    pads: list[Pad] = []
    for child in children:
        if child.head == "at":
            placement = _parse_slim_at(child)
            if placement is not None:
                at_x, at_y, at_angle = placement
        elif child.head == "uuid":
            match = _UUID_PATTERN.match(child.text())
            if match is not None:
                uuid = match.group(1) or match.group(2)
        elif child.head == "property":
            match = _PROPERTY_PATTERN.match(child.text())
            if match is not None and match.group(1) == "Reference":
                properties.append(
                    Property(name=match.group(1), value=match.group(2))
                )
        elif child.head == "pad":
            pad = _parse_slim_pad(
                child,
                name_by_ordinal=name_by_ordinal,
                ordinal_by_name=ordinal_by_name,
            )
            if pad is not None:
                pads.append(pad)
    return Footprint(
        library_link="",
        at_x=at_x,
        at_y=at_y,
        at_angle=at_angle,
        uuid=uuid,
        properties=properties,
        pads=pads,
    )


def _apply_slim_zone_child(
    child: SexpFormSpan,
    state: dict[str, Any],
) -> None:
    parsed = child.parse()
    if not isinstance(parsed, list) or not parsed:
        return
    if child.head == "net" and len(parsed) > 1:
        state["raw_net"] = parsed[1]
    elif child.head == "net_name":
        state["explicit_name"] = (
            unquote_string(parsed[1]) if len(parsed) > 1 else ""
        )
    elif child.head == "layers":
        state["layers"] = [unquote_string(value) for value in parsed[1:]]
    elif child.head == "layer" and len(parsed) > 1:
        state["layers"] = [unquote_string(parsed[1])]
    elif child.head == "uuid" and len(parsed) > 1:
        state["uuid"] = unquote_string(parsed[1])


def parse_slim_zone(
    _parent: SexpFormSpan,
    children: Sequence[SexpFormSpan],
    *,
    name_by_ordinal: dict[int, str],
    ordinal_by_name: dict[str, int],
) -> SlimZone:
    """Build a zone carrying only nets, layers, and realized fill rings."""
    state: dict[str, Any] = {
        "raw_net": 0,
        "explicit_name": "",
        "layers": [],
        "uuid": None,
    }
    filled_polygons: list[SlimFilledPolygon] = []
    for child in children:
        if child.head == "filled_polygon":
            filled_polygons.append(parse_filled_polygon_span(child))
            continue
        _apply_slim_zone_child(child, state)
    net = _resolve_net_ref(
        NetRef.from_raw_token(
            state["raw_net"],
            explicit_name=state["explicit_name"],
        ),
        name_by_ordinal=name_by_ordinal,
        ordinal_by_name=ordinal_by_name,
    )
    return SlimZone(
        net=net,
        layers=state["layers"],
        uuid=state["uuid"],
        filled_polygons=filled_polygons,
    )


def _copper_family_selector() -> SexpSelector:
    """Select only the families copper geometry consumers need."""
    root = "kicad_pcb"
    paths: set[tuple[str, ...]] = {
        (root, head)
        for head in (
            "layers",
            "net",
            "segment",
            "arc",
            "via",
            "footprint",
            "module",
            "zone",
        )
    }
    paths.add((root, "setup", "stackup"))
    for footprint_head in _FOOTPRINT_HEADS:
        paths.update(
            {
                (root, footprint_head, "at"),
                (root, footprint_head, "uuid"),
                (root, footprint_head, "property"),
                (root, footprint_head, "pad"),
            }
        )
    paths.update(
        {
            (root, "zone", "net"),
            (root, "zone", "net_name"),
            (root, "zone", "layer"),
            (root, "zone", "layers"),
            (root, "zone", "uuid"),
            (root, "zone", "filled_polygon"),
        }
    )
    return SexpSelector(paths=paths)


def _group_spans(
    spans: Iterable[SexpFormSpan],
) -> tuple[
    dict[str, list[SexpFormSpan]],
    dict[int, list[SexpFormSpan]],
    SexpFormSpan | None,
]:
    top_level: dict[str, list[SexpFormSpan]] = {}
    children_by_parent: dict[int, list[SexpFormSpan]] = {}
    stackup_span: SexpFormSpan | None = None
    current_parent: SexpFormSpan | None = None
    for span in spans:
        if span.path == ("kicad_pcb", "setup", "stackup"):
            stackup_span = span
            continue
        if span.depth == 1:
            top_level.setdefault(str(span.head or ""), []).append(span)
            current_parent = (
                span if span.head in _SPAN_PARENT_HEADS else None
            )
            if current_parent is not None:
                children_by_parent[current_parent.start_offset] = []
            continue
        if (
            current_parent is not None
            and span.depth == 2
            and span.start_offset < current_parent.end_offset
        ):
            children_by_parent[current_parent.start_offset].append(span)
    return top_level, children_by_parent, stackup_span


def _parse_layers(spans: Sequence[SexpFormSpan]) -> list[Layer]:
    if not spans:
        return []
    parsed = spans[0].parse()
    if not isinstance(parsed, list):
        return []
    return [
        Layer.from_sexp(item)
        for item in parsed[1:]
        if isinstance(item, list) and item
    ]


def _parse_stackup(span: SexpFormSpan | None) -> Stackup | None:
    if span is None:
        return None
    parsed = span.parse()
    return Stackup.from_sexp(parsed) if isinstance(parsed, list) else None


def _parse_net_bound(
    spans: Sequence[SexpFormSpan],
    factory: Any,
    *,
    name_by_ordinal: dict[int, str],
    ordinal_by_name: dict[str, int],
) -> list[Any]:
    output: list[Any] = []
    for span in spans:
        parsed = span.parse()
        if not isinstance(parsed, list):
            continue
        item = factory(parsed)
        item.net = _resolve_net_ref(
            item.net,
            name_by_ordinal=name_by_ordinal,
            ordinal_by_name=ordinal_by_name,
        )
        output.append(item)
    return output


def slim_projection_source(
    projection: KiCadPcbProjection,
) -> SlimProjectionSource | None:
    """Extract copper families from a source-text-backed projection.

    Returns ``None`` when the projection is board-backed, in which case the
    caller should use the projection (or board) directly.
    """
    source_text = getattr(projection, "_source_text", None)
    if source_text is None or getattr(projection, "_board", None) is not None:
        return None

    spans = iter_sexp_form_spans(
        source_text,
        _copper_family_selector(),
        source_path=projection.source_path,
    )
    top_level, children_by_parent, stackup_span = _group_spans(spans)

    nets = [
        Net.from_sexp(parsed)
        for span in top_level.get("net", [])
        if isinstance((parsed := span.parse()), list)
    ]
    name_by_ordinal = {net.ordinal: net.name for net in nets}
    ordinal_by_name = {net.name: net.ordinal for net in nets}
    net_keys = {
        "name_by_ordinal": name_by_ordinal,
        "ordinal_by_name": ordinal_by_name,
    }

    footprint_spans = [
        span
        for head in _FOOTPRINT_HEADS
        for span in top_level.get(head, [])
    ]
    footprint_spans.sort(key=lambda span: span.start_offset)

    return SlimProjectionSource(
        source_path=projection.source_path,
        layers=_parse_layers(top_level.get("layers", [])),
        nets=nets,
        segments=_parse_net_bound(
            top_level.get("segment", ()), Segment.from_sexp, **net_keys
        ),
        arcs=_parse_net_bound(
            top_level.get("arc", ()), Arc.from_sexp, **net_keys
        ),
        vias=_parse_net_bound(
            top_level.get("via", ()), Via.from_sexp, **net_keys
        ),
        zones=[
            parse_slim_zone(
                span,
                children_by_parent.get(span.start_offset, ()),
                **net_keys,
            )
            for span in top_level.get("zone", [])
        ],
        footprints=[
            parse_slim_footprint(
                span,
                children_by_parent.get(span.start_offset, ()),
                **net_keys,
            )
            for span in footprint_spans
        ],
        stackup=_parse_stackup(stackup_span),
    )


class BoardSource:
    """Uniform accessor over a projection, a slim projection, or a board."""

    def __init__(
        self,
        source: KiCadPcb | KiCadPcbProjection | SlimProjectionSource,
    ) -> None:
        self.source = source
        self.is_projection = isinstance(source, KiCadPcbProjection)

    @property
    def source_path(self) -> str | None:
        value = getattr(self.source, "source_path", None)
        return str(value) if value is not None else None

    def collection(self, name: str) -> list[Any]:
        value = getattr(self.source, name)
        return list(value() if self.is_projection else value)

    def stackup(self) -> Any:
        value = getattr(self.source, "stackup", None)
        return value() if self.is_projection and callable(value) else value


def coerce_source(
    source: str | Path | KiCadPcb | KiCadPcbProjection,
) -> BoardSource:
    """Normalize any supported input into a :class:`BoardSource`."""
    if isinstance(source, KiCadPcbProjection):
        return BoardSource(slim_projection_source(source) or source)
    if isinstance(source, KiCadPcb):
        return BoardSource(source)
    projection = KiCadPcbProjection.from_file(Path(source))
    return BoardSource(slim_projection_source(projection) or projection)


def is_copper_layer(name: str) -> bool:
    return str(name).endswith(".Cu")


def expand_layer_names(
    requested: Sequence[str],
    copper_layer_names: Sequence[str],
) -> tuple[str, ...]:
    """Resolve KiCad layer tokens (``*.Cu``, ``F&B.Cu``, spans) to real layers."""
    requested_names = [str(name) for name in requested if str(name)]
    if "*.Cu" in requested_names:
        return tuple(copper_layer_names)
    if "F&B.Cu" in requested_names:
        return tuple(
            name for name in copper_layer_names if name in {"F.Cu", "B.Cu"}
        )
    if len(requested_names) == 2 and all(
        name in copper_layer_names for name in requested_names
    ):
        first = copper_layer_names.index(requested_names[0])
        last = copper_layer_names.index(requested_names[1])
        low, high = sorted((first, last))
        return tuple(copper_layer_names[low : high + 1])
    return tuple(name for name in requested_names if name in copper_layer_names)


def net_parts(obj: object) -> tuple[str, int | None]:
    net = getattr(obj, "net", None)
    return (
        str(getattr(net, "name", "") or ""),
        getattr(net, "ordinal", None),
    )


def component_reference(footprint: object) -> str:
    getter = getattr(footprint, "get_property_value", None)
    if callable(getter):
        return str(getter("Reference") or "")
    return ""

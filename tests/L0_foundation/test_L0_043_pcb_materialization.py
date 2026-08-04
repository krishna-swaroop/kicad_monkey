"""
Subtest: Analytic PCB Materialization
Stratum: L0_foundation
Purpose: Materialized geometry stays analytic, immutable, and placed by affine.

The materializer emits physical operations rather than KiCad source objects,
so a track becomes a capsule, an arc becomes a swept path, and a pad becomes
its shape family plus a local-to-board transform. These tests pin the value
types, the transform algebra, the stack derivation, and the deterministic
s-expression view against synthetic boards.
"""

from __future__ import annotations

import dataclasses
import math

import pytest

from kicad_monkey.kicad_pcb_materialize import (
    ALL_PRODUCTS,
    HoleOperation2D,
    HolePlating,
    MaterialProduct,
    MaterializationRequest,
    SurfaceAction,
    SurfaceOperation2D,
    materialize_pcb,
    operation_identity,
)
from kicad_monkey.kicad_pcb_materialize_geometry import (
    Affine2D,
    ArcSegment2D,
    Capsule2D,
    Disk2D,
    PathCap,
    PlanarRegion2D,
    Rect2D,
    RoundedRect2D,
    SweptPath2D,
    Trapezoid2D,
    shape_kind,
)
from kicad_monkey.kicad_pcb_materialize_sexpr import (
    write_materialization_sexp,
)
from kicad_monkey.kicad_pcb_materialize_stack import (
    MaterialKind,
    MaterialRole,
    PcbMaterialStack,
    build_material_stack,
    copper_slice_key,
)
from kicad_monkey.kicad_pcb_projection import KiCadPcbProjection
from kicad_monkey.kicad_sexpr import parse_sexp

NM = 1_000_000

BOARD = """(kicad_pcb (version 20240108) (generator test)
  (layers (0 "F.Cu" signal) (1 "In1.Cu" signal) (31 "B.Cu" signal))
  (setup (stackup
    (layer "F.SilkS" (type "Top Silk Screen") (thickness 0))
    (layer "F.Mask" (type "Top Solder Mask") (thickness 0.01))
    (layer "F.Cu" (type "copper") (thickness 0.035))
    (layer "dielectric 1" (type "core") (thickness 1.51)
      (material "FR4") (epsilon_r 4.5) (loss_tangent 0.02))
    (layer "In1.Cu" (type "copper") (thickness 0.0152))
    (layer "dielectric 2" (type "prepreg") (thickness 0.1))
    (layer "B.Cu" (type "copper") (thickness 0.035))))
  (net 0 "")
  (net 1 "GND")
  (net 2 "VCC")
  (segment (start 10 10) (end 20 10) (width 0.25) (layer "F.Cu")
    (net 1) (uuid "seg-1"))
  (segment (start 10 12) (end 20 12) (width 0.2) (layer "B.Cu")
    (net 2) (uuid "seg-2"))
  (arc (start 30 10) (mid 32 12) (end 34 10) (width 0.3) (layer "F.Cu")
    (net 2) (uuid "arc-1"))
  (via (at 40 40) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu")
    (net 1) (uuid "via-1"))
  (zone (net 1) (net_name "GND") (layer "F.Cu") (uuid "zone-1")
    (polygon (pts (xy 0 0) (xy 5 0) (xy 5 5) (xy 0 5)))
    (filled_polygon (layer "F.Cu")
      (pts (xy 0 0) (xy 5 0) (xy 5 5) (xy 0 5))))
  (footprint "lib:fp" (layer "F.Cu") (at 50 60 90) (uuid "fp-1")
    (property "Reference" "U1" (at 0 0 0) (layer "F.SilkS"))
    (pad "1" smd rect (at 1 0 90) (size 1 0.5) (layers "F.Cu")
      (net 1 "GND") (uuid "pad-rect"))
    (pad "2" smd roundrect (at 2 0 90) (size 1 0.5)
      (roundrect_rratio 0.25) (layers "F.Cu") (net 2 "VCC")
      (uuid "pad-roundrect"))
    (pad "3" thru_hole oval (at 3 0 90) (size 1.6 0.8) (drill 0.8)
      (layers "*.Cu") (net 1 "GND") (uuid "pad-oval"))
    (pad "4" smd trapezoid (at 4 0 90) (size 1 1) (rect_delta 0.2 0)
      (layers "F.Cu") (net 2 "VCC") (uuid "pad-trapezoid"))
    (pad "5" thru_hole circle (at 5 0 90) (size 1.2 1.2) (drill 0.6)
      (layers "*.Cu") (net 1 "GND") (uuid "pad-circle"))
    (pad "" np_thru_hole circle (at 6 0 90) (size 2 2) (drill 2)
      (layers "*.Cu") (uuid "pad-npth"))
    (pad "6" smd rect (at 7 0 180) (size 0.9 0.4) (layers "F.Cu")
      (net 2 "VCC") (uuid "pad-turned")))
)"""


def _materialize(request: MaterializationRequest | None = None):
    return materialize_pcb(
        KiCadPcbProjection(source_text=BOARD), request=request
    )


def _by_uuid_shape(result, shape_type):
    return [
        op
        for op in result.surface_operations
        if isinstance(op.geometry, shape_type)
    ]


class TestAffine2D:
    """The transform algebra that carries placement out of the geometry."""

    def test_identity_is_neutral(self):
        identity = Affine2D.identity()
        assert identity.is_identity
        assert identity.apply((1234, -5678)) == (1234, -5678)

    def test_rotation_then_translation_matches_expected_matrix(self):
        affine = Affine2D.rotation(90).then(
            Affine2D.translation(17 * NM, 8 * NM)
        )
        assert (
            round(affine.a),
            round(affine.b),
            round(affine.c),
            round(affine.d),
        ) == (0, 1, -1, 0)
        assert affine.translation_nm == (17 * NM, 8 * NM)

    def test_composition_is_associative(self):
        first = Affine2D.rotation(37)
        second = Affine2D.scale(2, 3)
        third = Affine2D.translation(11, 22)
        point = (3456, -7890)
        assert (
            first.then(second).then(third).apply(point)
            == first.then(second.then(third)).apply(point)
        )

    def test_composition_matches_exact_float_reference(self):
        # Composing must not round between steps: applying transforms one at a
        # time through integer points loses precision that the composed
        # transform keeps.
        angle = math.radians(37)
        x, y = 3456.0, -7890.0
        rx = x * math.cos(angle) - y * math.sin(angle)
        ry = x * math.sin(angle) + y * math.cos(angle)
        expected = (round(rx * 2 + 11), round(ry * 3 + 22))
        composed = (
            Affine2D.rotation(37)
            .then(Affine2D.scale(2, 3))
            .then(Affine2D.translation(11, 22))
        )
        assert composed.apply((x, y)) == expected

    def test_mirror_reports_negative_determinant(self):
        assert Affine2D.mirror_x().is_mirrored
        assert Affine2D.mirror_y().is_mirrored
        assert not Affine2D.rotation(90).is_mirrored

    def test_is_frozen(self):
        with pytest.raises(dataclasses.FrozenInstanceError):
            Affine2D.identity().a = 2.0  # type: ignore[misc]


class TestShapeValueTypes:
    """Shapes are immutable, normalized to integer nanometres, and validated."""

    def test_dimensions_normalize_to_integer_nanometres(self):
        disk = Disk2D(radius_nm=300000.4)
        assert disk.radius_nm == 300000
        assert isinstance(disk.radius_nm, int)

    def test_shapes_are_frozen(self):
        with pytest.raises(dataclasses.FrozenInstanceError):
            Disk2D(radius_nm=5).radius_nm = 6  # type: ignore[misc]

    def test_shapes_are_hashable_and_compare_by_value(self):
        assert Disk2D(radius_nm=5) == Disk2D(radius_nm=5)
        assert len({Disk2D(radius_nm=5), Disk2D(radius_nm=5)}) == 1

    @pytest.mark.parametrize(
        "factory",
        [
            lambda: Disk2D(radius_nm=0),
            lambda: Rect2D(width_nm=0, height_nm=5),
            lambda: Capsule2D(start_nm=(0, 0), end_nm=(1, 1), width_nm=0),
            lambda: PlanarRegion2D(outer_nm=((0, 0), (1, 1))),
            lambda: SweptPath2D(segments=(), width_nm=5),
        ],
    )
    def test_degenerate_shapes_are_rejected(self, factory):
        with pytest.raises(ValueError):
            factory()

    def test_swept_path_keeps_arc_segments_analytic(self):
        path = SweptPath2D(
            segments=(ArcSegment2D((0, 0), (5, 5), (10, 0)),),
            width_nm=250000,
        )
        assert isinstance(path.segments[0], ArcSegment2D)
        assert path.segments[0].mid_nm == (5, 5)
        assert path.cap is PathCap.ROUND

    def test_shape_kind_tokens(self):
        assert shape_kind(Disk2D(radius_nm=1)) == "disk_2d"
        assert (
            shape_kind(RoundedRect2D(width_nm=2, height_nm=2, corner_radius_nm=1))
            == "rounded_rect_2d"
        )


class TestMaterialStack:
    """Stack derivation, canonical keys, and honest thickness reporting."""

    def test_copper_keys_are_canonical(self):
        assert copper_slice_key("F.Cu") == "copper.front"
        assert copper_slice_key("B.Cu") == "copper.back"
        assert copper_slice_key("In3.Cu") == "copper.in3"

    def test_stack_roles_and_order(self):
        result = _materialize()
        roles = [(item.key, item.role) for item in result.slices]
        assert ("copper.front", MaterialRole.CONDUCTOR) in roles
        assert ("dielectric.1", MaterialRole.DIELECTRIC) in roles
        assert ("mask.front", MaterialRole.MASK) in roles
        assert ("silkscreen.front", MaterialRole.SILKSCREEN) in roles
        assert [item.order for item in result.slices] == list(
            range(len(result.slices))
        )

    def test_dielectric_properties_are_carried(self):
        result = _materialize()
        core = next(
            item for item in result.slices if item.key == "dielectric.1"
        )
        assert core.dielectric is not None
        assert core.dielectric.material_name == "FR4"
        assert core.dielectric.relative_permittivity == 4.5
        assert core.dielectric.loss_tangent == 0.02

    def test_unmodelled_thickness_is_unknown_not_zero(self):
        result = _materialize()
        silk = next(
            item for item in result.slices if item.key == "silkscreen.front"
        )
        assert silk.thickness_nm is None

    def test_z_span_walks_thicknesses(self):
        result = _materialize()
        stack = PcbMaterialStack(slices=result.slices)
        # Silkscreen contributes zero, mask is 10 um, so front copper starts
        # at 10 um and is 35 um thick.
        assert stack.z_span_nm("copper.front") == (10_000, 45_000)

    def test_z_span_refuses_to_guess_past_unknown_bulk(self):
        stack = PcbMaterialStack.from_copper_layers(["F.Cu", "B.Cu"])
        assert stack.conductor_keys() == ("copper.front", "copper.back")
        assert stack.z_span_nm("copper.back") is None

    def test_stack_falls_back_to_copper_layers_without_stackup(self):
        stack = build_material_stack(None, ["F.Cu", "In1.Cu", "B.Cu"])
        assert stack.conductor_keys() == (
            "copper.front",
            "copper.in1",
            "copper.back",
        )
        # A fallback stack knows the order but not the physical thicknesses,
        # and must say so rather than inventing them.
        assert all(item.thickness_nm is None for item in stack.slices)
        assert not stack.has_complete_thickness


class TestCopperMaterialization:
    """Every copper family reaches the result as an analytic shape."""

    def test_track_becomes_a_capsule_in_board_coordinates(self):
        capsules = [
            op
            for op in _by_uuid_shape(_materialize(), Capsule2D)
            if op.affine.is_identity
        ]
        track = next(
            op for op in capsules if op.geometry.width_nm == 250000
        )
        assert track.geometry.start_nm == (10 * NM, 10 * NM)
        assert track.geometry.end_nm == (20 * NM, 10 * NM)
        assert track.slice_key == "copper.front"
        assert track.net_key == "GND"
        assert track.material is MaterialKind.COPPER
        assert track.action is SurfaceAction.ADD

    def test_arc_keeps_its_three_point_centerline(self):
        paths = _by_uuid_shape(_materialize(), SweptPath2D)
        assert len(paths) == 1
        segment = paths[0].geometry.segments[0]
        assert isinstance(segment, ArcSegment2D)
        assert segment.start_nm == (30 * NM, 10 * NM)
        assert segment.mid_nm == (32 * NM, 12 * NM)
        assert segment.end_nm == (34 * NM, 10 * NM)

    def test_via_emits_lands_on_every_spanned_conductor(self):
        result = _materialize()
        lands = [
            op
            for op in result.surface_operations
            if isinstance(op.geometry, Disk2D)
            and op.geometry.radius_nm == 400000
        ]
        assert {op.slice_key for op in lands} == {
            "copper.front",
            "copper.in1",
            "copper.back",
        }

    def test_via_hole_spans_the_stack_and_is_plated(self):
        holes = [
            op
            for op in _materialize().hole_operations
            if op.geometry == Disk2D(radius_nm=200000)
        ]
        assert len(holes) == 1
        assert holes[0].start_slice_key == "copper.front"
        assert holes[0].end_slice_key == "copper.back"
        assert holes[0].plating is HolePlating.PLATED
        assert holes[0].net_key == "GND"

    def test_zone_fill_becomes_a_planar_region(self):
        regions = [
            op
            for op in _by_uuid_shape(_materialize(), PlanarRegion2D)
            if op.affine.is_identity
        ]
        assert len(regions) == 1
        assert regions[0].geometry.outer_nm == (
            (0, 0),
            (5 * NM, 0),
            (5 * NM, 5 * NM),
            (0, 5 * NM),
        )
        assert regions[0].net_key == "GND"

    def test_unplated_hole_carries_no_net(self):
        npth = [
            op
            for op in _materialize().hole_operations
            if op.plating is HolePlating.UNPLATED
        ]
        assert len(npth) == 1
        assert npth[0].net_key is None

    def test_only_referenced_nets_are_reported(self):
        assert [net.key for net in _materialize().nets] == ["GND", "VCC"]


class TestPadShapes:
    """Pad lands map onto the shape union rather than a sampled outline."""

    def _pads(self):
        result = _materialize()
        return [
            op
            for op in result.surface_operations
            if not op.affine.is_identity
        ]

    def test_each_pad_family_maps_to_its_shape(self):
        kinds = {shape_kind(op.geometry) for op in self._pads()}
        assert {
            "rect_2d",
            "rounded_rect_2d",
            "capsule_2d",
            "trapezoid_2d",
            "disk_2d",
        } <= kinds

    def test_rect_pad_is_centered_in_its_own_frame(self):
        rect = next(
            op
            for op in self._pads()
            if isinstance(op.geometry, Rect2D)
        )
        assert rect.geometry.center_nm == (0, 0)
        assert rect.geometry.width_nm == 1 * NM
        assert rect.geometry.height_nm == 500000

    def test_trapezoid_keeps_its_delta(self):
        trapezoid = next(
            op
            for op in self._pads()
            if isinstance(op.geometry, Trapezoid2D)
        )
        assert trapezoid.geometry.delta_x_nm == 200000
        assert trapezoid.geometry.delta_y_nm == 0

    def test_oval_pad_sweeps_the_shorter_axis(self):
        oval = next(
            op
            for op in self._pads()
            if isinstance(op.geometry, Capsule2D)
        )
        # 1.6 x 0.8 mm: the capsule lies along X with the 0.8 mm width.
        assert oval.geometry.width_nm == 800000
        assert oval.geometry.start_nm == (-400000, 0)
        assert oval.geometry.end_nm == (400000, 0)

    def test_rotation_lives_in_the_affine_not_the_shape(self):
        # Every pad in the fixture sits on a footprint placed at 90 degrees
        # with pads at absolute 90, so each land keeps an origin-centered
        # shape and a non-identity transform.
        for op in self._pads():
            assert not op.affine.is_identity
            center = getattr(op.geometry, "center_nm", (0, 0))
            assert center == (0, 0)

    def test_pad_placement_follows_the_footprint_transform(self):
        rect = next(
            op
            for op in self._pads()
            if isinstance(op.geometry, Rect2D)
            and op.geometry.width_nm == 1 * NM
        )
        # Footprint at (50, 60) rotated 90 degrees; pad offset (1, 0) local.
        # KiCad rotates by the negated placement angle, so +x maps to -y.
        assert rect.affine.translation_nm == (50 * NM, 59 * NM)

    @staticmethod
    def _linear(affine):
        return tuple(round(value, 9) + 0.0 for value in
                     (affine.a, affine.b, affine.c, affine.d))

    def test_pad_transform_encodes_the_absolute_orientation(self):
        # Board-embedded pads store absolute orientation, so the emitter has
        # to subtract the footprint angle before composing. Getting that wrong
        # leaves the land correctly positioned but wrongly turned, which only
        # the linear part of the transform reveals.
        rect = next(
            op
            for op in self._pads()
            if isinstance(op.geometry, Rect2D)
            and op.geometry.width_nm == 1 * NM
        )
        # Pad absolute angle 90 on a footprint at 90 => net rotation of -90.
        assert self._linear(rect.affine) == (0.0, -1.0, 1.0, 0.0)

    def test_pad_turned_against_its_footprint_keeps_its_own_angle(self):
        turned = next(
            op
            for op in self._pads()
            if isinstance(op.geometry, Rect2D)
            and op.geometry.width_nm == 900000
        )
        # Pad absolute angle 180 on a footprint at 90 => net rotation of -180.
        assert self._linear(turned.affine) == (-1.0, 0.0, 0.0, -1.0)
        # And it still lands where the footprint places it: local (7, 0)
        # rotated by -90 is (0, -7), offset from the footprint origin.
        assert turned.affine.translation_nm == (50 * NM, 53 * NM)


class TestScopeAndRequest:
    """A partial result must declare itself and must not overreach."""

    def test_complete_request_reports_complete_board(self):
        result = _materialize()
        assert result.scope.complete_board
        assert result.scope.products == ALL_PRODUCTS

    def test_front_copper_request_is_not_complete(self):
        result = _materialize(MaterializationRequest.front_copper())
        assert not result.scope.complete_board
        assert result.scope.slice_keys == ("copper.front",)
        assert all(
            op.slice_key == "copper.front"
            for op in result.surface_operations
        )
        assert result.hole_operations == ()

    def test_holes_only_request_drops_surfaces(self):
        result = _materialize(
            MaterializationRequest(products=frozenset({MaterialProduct.HOLES}))
        )
        assert result.surface_operations == ()
        assert result.hole_operations

    def test_empty_product_request_is_rejected(self):
        with pytest.raises(ValueError):
            MaterializationRequest(products=frozenset())

    def test_operations_are_indexed_positionally(self):
        result = _materialize()
        assert [op.index for op in result.operations] == list(
            range(len(result.operations))
        )

    def test_holes_sort_after_surfaces(self):
        result = _materialize()
        kinds = [
            isinstance(op, HoleOperation2D) for op in result.operations
        ]
        assert kinds == sorted(kinds)
        assert isinstance(result.operations[0], SurfaceOperation2D)


class TestSexprView:
    """The s-expression view is deterministic and parseable."""

    def test_output_is_stable_across_runs(self):
        assert write_materialization_sexp(
            _materialize()
        ) == write_materialization_sexp(_materialize())

    def test_output_parses_back_as_an_s_expression(self):
        parsed = parse_sexp(write_materialization_sexp(_materialize()))
        assert parsed[0] == "pcb_analytic_materialization"
        heads = {item[0] for item in parsed[1:] if isinstance(item, list)}
        assert {"scope", "nets", "slices", "operations"} <= heads

    def test_operations_render_with_material_and_transform(self):
        text = write_materialization_sexp(_materialize())
        assert "(surface_operation_2d" in text
        assert "(hole_operation_2d" in text
        assert "(material copper)" in text
        assert "(role conductor)" in text
        assert "(affine " in text

    def test_transforms_render_without_negative_zero_artifacts(self):
        # A 90 degree rotation produces a cosine of about 6.1e-17, which would
        # print as "-0" without the shared float formatter.
        from kicad_monkey.kicad_pcb_materialize_sexpr import affine_sexp

        rendered = affine_sexp(
            Affine2D.rotation(90).then(Affine2D.translation(1 * NM, 2 * NM))
        )
        assert rendered == ["affine", "0", "1", "-1", "0", "1000000", "2000000"]


class TestInputEquivalence:
    """Both supported in-memory inputs agree, and selection is a pure filter."""

    def test_projection_and_path_inputs_agree(self, tmp_path):
        board_file = tmp_path / "synthetic.kicad_pcb"
        board_file.write_text(BOARD, encoding="utf-8")
        from_path = materialize_pcb(board_file)
        from_projection = _materialize()
        assert [
            operation_identity(op) for op in from_path.operations
        ] == [operation_identity(op) for op in from_projection.operations]

    def test_front_copper_equals_the_filtered_complete_result(self):
        complete = _materialize()
        selective = _materialize(MaterializationRequest.front_copper())
        expected = [
            operation_identity(op)
            for op in complete.operations
            if isinstance(op, SurfaceOperation2D)
            and op.slice_key == "copper.front"
        ]
        assert [
            operation_identity(op) for op in selective.operations
        ] == expected

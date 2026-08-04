"""
Subtest: Analytic PCB Materialization Corpus Equivalence
Stratum: L1_parsing
Purpose: Materialization is input-independent and selection is a pure filter.

Two properties have to hold before an analytic materialization can be trusted
by anything downstream:

1. Reading a board through the optimized projection must produce exactly the
   same analytic result as reading it through the full object model. If the
   fast path drifts, every consumer inherits the drift.

2. A scoped result must equal the complete result filtered the same way.
   Without that, a partial materialization is a different answer rather than a
   subset of one, and progressive loading downstream becomes unsafe.

Both are checked against real public-corpus boards, because synthetic fixtures
do not exercise rotated footprints, multilayer spans, custom pads, or realized
zone fills at scale.
"""

from __future__ import annotations

import os
from functools import lru_cache
from pathlib import Path

import pytest

from _suite_paths import TEST_CORPUS_ROOT
from kicad_monkey.kicad_pcb import KiCadPcb
from kicad_monkey.kicad_pcb_materialize import (
    HoleOperation2D,
    MaterialProduct,
    MaterializationRequest,
    SurfaceOperation2D,
    materialize_pcb,
    operation_identity,
)
from kicad_monkey.kicad_pcb_materialize_sexpr import (
    write_materialization_sexp,
)
from kicad_monkey.kicad_pcb_projection import KiCadPcbProjection

_CORPUS_BOARDS = {
    "4-ch-backplane": Path("kicad/projects/4-ch-backplane/input")
    / "4-ch-backplane.kicad_pcb",
    "speedy_processing_module": Path(
        "kicad/projects/speedy_processing_module/input"
    )
    / "11-10084__speedy_processing_module__B.kicad_pcb",
    "charge_indicator": Path("kicad/projects/charge_indicator/input")
    / "11-10043__charge_indicator__C.kicad_pcb",
    "taillight": Path("kicad/projects/taillight/input")
    / "11-10045__taillight__C.kicad_pcb",
}

# Full-object-model parsing is materially slower than the projection, so the
# cross-input proof runs on the smaller boards and the projection-only proofs
# cover the large ones.
_FULL_MODEL_BOARDS = ("charge_indicator", "taillight")


def _resolve(name: str) -> Path | None:
    relative = _CORPUS_BOARDS[name]
    candidates = [TEST_CORPUS_ROOT / relative]
    env = os.environ.get("WN_TEST_CORPUS")
    if env:
        candidates.insert(0, Path(env) / relative)
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    return None


def _board(name: str) -> Path:
    resolved = _resolve(name)
    if resolved is None:
        pytest.skip(f"corpus board not available: {name}")
    return resolved


# These boards are large, and several proofs need the same result. Materialize
# each distinct (board, request) once instead of once per assertion.
@lru_cache(maxsize=None)
def _materialized(name: str, request: MaterializationRequest | None = None):
    return materialize_pcb(_board(name), request=request)


@lru_cache(maxsize=None)
def _materialized_from_full_model(name: str):
    return materialize_pcb(KiCadPcb.from_file(_board(name)))


@lru_cache(maxsize=None)
def _materialized_from_projection(name: str):
    return materialize_pcb(KiCadPcbProjection.from_file(_board(name)))


def _identities(result) -> list[tuple]:
    return [operation_identity(op) for op in result.operations]


def _filter_complete(result, request: MaterializationRequest) -> list[tuple]:
    """The complete result reduced by the same rules the request applies."""
    kept = []
    for operation in result.operations:
        if isinstance(operation, SurfaceOperation2D):
            if not request.wants(MaterialProduct.COPPER):
                continue
            if not request.wants_slice(operation.slice_key):
                continue
        else:
            if not request.wants(MaterialProduct.HOLES):
                continue
            if not (
                request.wants_slice(operation.start_slice_key)
                or request.wants_slice(operation.end_slice_key)
            ):
                continue
        kept.append(operation_identity(operation))
    return kept


@pytest.mark.parametrize("name", sorted(_CORPUS_BOARDS))
class TestProjectionAndFullModelAgree:
    """Reading a board fast must not change what the board is."""

    def test_path_and_projection_inputs_agree(self, name):
        from_path = _materialized(name)
        from_projection = _materialized_from_projection(name)
        assert _identities(from_path) == _identities(from_projection)
        assert from_path.nets == from_projection.nets
        assert from_path.slices == from_projection.slices

    def test_materialization_is_reproducible(self, name):
        # Deliberately bypasses the cache: repeating the work is the point.
        board = _board(name)
        assert _identities(materialize_pcb(board)) == _identities(
            materialize_pcb(board)
        )


@pytest.mark.parametrize("name", _FULL_MODEL_BOARDS)
class TestFullObjectModelAgrees:
    """The full parser and the projection describe the same physical board."""

    def test_full_model_input_matches_projection_input(self, name):
        from_model = _materialized_from_full_model(name)
        from_projection = _materialized(name)
        assert _identities(from_model) == _identities(from_projection)
        assert from_model.nets == from_projection.nets
        assert from_model.slices == from_projection.slices

    def test_sexpr_view_matches_across_inputs(self, name):
        assert write_materialization_sexp(
            _materialized_from_full_model(name)
        ) == write_materialization_sexp(_materialized(name))


@pytest.mark.parametrize("name", sorted(_CORPUS_BOARDS))
class TestSelectiveEqualsFilteredComplete:
    """A scoped result is a subset of the complete one, not a variant of it."""

    def _requests(self, complete) -> list[MaterializationRequest]:
        conductors = complete.scope.slice_keys
        requests = [
            MaterializationRequest.front_copper(),
            MaterializationRequest(
                products=frozenset({MaterialProduct.COPPER})
            ),
            MaterializationRequest(
                products=frozenset({MaterialProduct.HOLES})
            ),
        ]
        if conductors:
            requests.append(
                MaterializationRequest(
                    products=frozenset({MaterialProduct.COPPER}),
                    slice_keys=(conductors[-1],),
                )
            )
        return requests

    def test_every_scoped_request_is_a_filter(self, name):
        complete = _materialized(name)
        for request in self._requests(complete):
            selective = _materialized(name, request)
            assert _identities(selective) == _filter_complete(
                complete, request
            ), f"{name}: {sorted(p.value for p in request.products)} " \
               f"{request.slice_keys}"

    def test_scope_reports_partial_results_as_partial(self, name):
        assert _materialized(name).scope.complete_board
        partial = _materialized(name, MaterializationRequest.front_copper())
        assert not partial.scope.complete_board


@pytest.mark.parametrize("name", sorted(_CORPUS_BOARDS))
class TestMaterializedInvariants:
    """Physical invariants that must hold on any real board."""

    def test_every_conductive_operation_names_a_conductor(self, name):
        result = _materialized(name)
        conductors = set(result.scope.slice_keys)
        for operation in result.surface_operations:
            assert operation.slice_key in conductors

    def test_hole_spans_reference_declared_conductors(self, name):
        result = _materialized(name)
        conductors = set(result.scope.slice_keys)
        for hole in result.hole_operations:
            assert hole.start_slice_key in conductors
            assert hole.end_slice_key in conductors

    def test_reported_nets_cover_every_referenced_net(self, name):
        result = _materialized(name)
        declared = {net.key for net in result.nets}
        referenced = {
            operation.net_key
            for operation in result.operations
            if operation.net_key is not None
        }
        assert referenced <= declared
        assert declared == referenced

    def test_unplated_holes_carry_no_net(self, name):
        result = _materialized(name)
        for hole in result.hole_operations:
            if hole.plating.value == "unplated":
                assert hole.net_key is None

    def test_board_produces_analytic_operations(self, name):
        result = _materialized(name)
        assert result.operations
        assert any(
            isinstance(op, SurfaceOperation2D) for op in result.operations
        )
        assert any(
            isinstance(op, HoleOperation2D) for op in result.operations
        )

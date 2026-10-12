import "@typespec/json-schema";

using TypeSpec.JsonSchema;

namespace Wavenumber.KicadMonkey.Contracts.MaterializationA0;

/**
 * Board-frame length in integer nanometres. The range matches KiCad's own
 * 32-bit internal-unit limit, so the packed encoding stores it losslessly.
 */
scalar Nm extends int32;

/** Non-negative length in integer nanometres. */
@minValue(0)
scalar NonNegativeNm extends int32;

/** Point in KiCad's board frame: X to the right, Y down, in nanometres. */
@minItems(2)
@maxItems(2)
model NmPoint is Array<Nm>;

/** Axis-aligned bounds in the board frame, inclusive. */
model NmBounds {
  min_x_nm: Nm;
  min_y_nm: Nm;
  max_x_nm: Nm;
  max_y_nm: Nm;
}

/**
 * One fully composed local-to-board transform. A shape's local coordinates
 * map to board coordinates as x' = m11*x + m12*y + tx_nm and
 * y' = m21*x + m22*y + ty_nm. Reflection, rotation, and non-uniform scale are
 * ordinary cases of the same matrix.
 */
model Affine2D {
  m11: float64;
  m12: float64;
  m21: float64;
  m22: float64;
  tx_nm: Nm;
  ty_nm: Nm;
}

// ---------------------------------------------------------------- shapes

/** Straight path segment to `end_nm`. */
model LineSegment {
  kind: "line";
  end_nm: NmPoint;
}

/** Circular arc segment through `mid_nm` to `end_nm`, the form KiCad stores. */
model ArcSegment {
  kind: "arc";
  mid_nm: NmPoint;
  end_nm: NmPoint;
}

union PathSegment {
  line: LineSegment,
  arc: ArcSegment,
}

/** Closed contour of line and arc segments. The last segment returns to `start_nm`. */
model Contour {
  start_nm: NmPoint;
  @minItems(1) segments: PathSegment[];
}

enum PathCap {
  round,
  flat,
}

enum PathJoin {
  round,
  miter,
}

enum RectCorner {
  top_left,
  top_right,
  bottom_left,
  bottom_right,
}

model Disk2d {
  kind: "disk_2d";
  center_nm: NmPoint;
  radius_nm: NonNegativeNm;
}

model Rect2d {
  kind: "rect_2d";
  center_nm: NmPoint;
  width_nm: NonNegativeNm;
  height_nm: NonNegativeNm;
}

model RoundedRect2d {
  kind: "rounded_rect_2d";
  center_nm: NmPoint;
  width_nm: NonNegativeNm;
  height_nm: NonNegativeNm;
  radius_nm: NonNegativeNm;
}

/** Rectangle with the listed corners chamfered; unlisted corners keep `radius_nm`. */
model ChamferedRect2d {
  kind: "chamfered_rect_2d";
  center_nm: NmPoint;
  width_nm: NonNegativeNm;
  height_nm: NonNegativeNm;
  chamfer_nm: NonNegativeNm;
  chamfered_corners: RectCorner[];
  radius_nm: NonNegativeNm;
}

/** KiCad trapezoid: a rectangle whose opposite edges are widened or narrowed by the deltas. */
model Trapezoid2d {
  kind: "trapezoid_2d";
  center_nm: NmPoint;
  width_nm: NonNegativeNm;
  height_nm: NonNegativeNm;
  delta_x_nm: Nm;
  delta_y_nm: Nm;
}

/** Stadium between two centres with a common radius. */
model Capsule2d {
  kind: "capsule_2d";
  start_nm: NmPoint;
  end_nm: NmPoint;
  radius_nm: NonNegativeNm;
}

model Annulus2d {
  kind: "annulus_2d";
  center_nm: NmPoint;
  outer_radius_nm: NonNegativeNm;
  inner_radius_nm: NonNegativeNm;
}

/** Open centreline of line and arc segments swept by a constant width. */
model SweptPath2d {
  kind: "swept_path_2d";
  start_nm: NmPoint;
  @minItems(1) segments: PathSegment[];
  width_nm: NonNegativeNm;
  cap: PathCap;
  join: PathJoin;
}

/** One analytic outer contour with optional hole contours. */
model PlanarRegion2d {
  kind: "planar_region_2d";
  outer: Contour;
  holes: Contour[];
}

/** Closed analytic shape vocabulary. Lowering is a consumer operation. */
union Shape2d {
  disk_2d: Disk2d,
  rect_2d: Rect2d,
  rounded_rect_2d: RoundedRect2d,
  chamfered_rect_2d: ChamferedRect2d,
  trapezoid_2d: Trapezoid2d,
  capsule_2d: Capsule2d,
  annulus_2d: Annulus2d,
  swept_path_2d: SweptPath2d,
  planar_region_2d: PlanarRegion2d,
}

// ---------------------------------------------------------------- stack

/**
 * Selectable products. Every operation belongs to exactly one product. The
 * board profile has no product of its own: it is the add operation of every
 * dielectric and solder-mask slice, with cutouts subtracted there.
 */
enum MaterialProduct {
  copper,
  holes,
  dielectric,
  solder_mask,
  paste,
  silkscreen,
  components,
}

enum SliceRole {
  conductor,
  dielectric,
  solder_mask,
  paste,
  silkscreen,
}

enum BoardSide {
  front,
  back,
}

/**
 * One physical slice of the rigid board stack. `order` is zero at the front
 * outermost slice and increases toward the back. Z positions are derived
 * from the ordered thicknesses and are never stored. Absent values are
 * unknown, never defaulted.
 */
model StackSlice {
  key: string;
  role: SliceRole;
  order: uint32;
  kicad_layer?: string;
  side?: BoardSide;
  thickness_nm?: NonNegativeNm;
  material_name?: string;
  relative_permittivity?: float64;
  loss_tangent?: float64;
}

model Stack {
  slices: StackSlice[];
  board_thickness_nm?: NonNegativeNm;
  copper_finish?: string;
}

// ---------------------------------------------------------------- nets

/** Resolved electrical net. The empty key is the no-net assignment. */
model Net {
  key: string;
  name: string;
  net_class?: string;
}

/** Design rules of one KiCad net class, as the project defines them. */
model NetClass {
  name: string;
  track_width_nm?: NonNegativeNm;
  clearance_nm?: NonNegativeNm;
  diff_pair_width_nm?: NonNegativeNm;
  diff_pair_gap_nm?: NonNegativeNm;
  via_diameter_nm?: NonNegativeNm;
  via_drill_nm?: NonNegativeNm;
}

model DiffPair {
  base_name: string;
  positive_net_key: string;
  negative_net_key: string;
}

// ---------------------------------------------------------------- operations

enum MaterialAction {
  add,
  subtract,
}

enum SurfaceMaterial {
  copper,
  dielectric,
  solder_mask,
  paste,
  silkscreen,
}

/**
 * Add or remove one shape of material on one slice. Conductive material
 * always carries `net_key`; operations on different nets are never merged.
 */
model SurfaceOperation {
  kind: "surface";
  index: uint32;
  action: MaterialAction;
  material: SurfaceMaterial;
  slice_key: string;
  net_key?: string;
  shape: Shape2d;
  affine: Affine2D;
  bounds: NmBounds;
}

enum HolePlating {
  plated,
  unplated,
}

/** KiCad's tri-state for a protection option: set, cleared, or left to the board default. */
enum ProtectionState {
  yes,
  no,
  unspecified,
}

model SideProtection {
  tented: ProtectionState;
  covered: ProtectionState;
  plugged: ProtectionState;
}

model HoleProtection {
  front: SideProtection;
  back: SideProtection;
  filled: ProtectionState;
  capped: ProtectionState;
}

/** Controlled-depth removal of barrel plating from one side. */
model Backdrill {
  side: BoardSide;
  diameter_nm?: NonNegativeNm;
  until_slice_key?: string;
}

/** Drilled or milled hole spanning `start_slice_key` to `end_slice_key` inclusive. */
model HoleOperation {
  kind: "hole";
  index: uint32;
  shape: Shape2d;
  affine: Affine2D;
  plating: HolePlating;
  start_slice_key: string;
  end_slice_key: string;
  net_key?: string;
  plating_thickness_nm?: NonNegativeNm;
  protection?: HoleProtection;
  backdrills: Backdrill[];
  bounds: NmBounds;
}

union MaterialOperation {
  surface: SurfaceOperation,
  hole: HoleOperation,
}

// ---------------------------------------------------------------- components and binding

model Component {
  refdes: string;
  footprint: string;
  side: BoardSide;
  affine: Affine2D;
  source_uid: string;
  /** Body extent in footprint-local coordinates, from courtyard or fabrication outline. */
  body_bounds?: NmBounds;
  body_height_nm?: NonNegativeNm;
}

enum BindingSourceKind {
  track,
  track_arc,
  via,
  pad,
  zone_fill,
  board_graphic,
  footprint_graphic,
  board_text,
  footprint_text,
  profile,
  derived,
}

/**
 * Links one operation to the KiCad object that produced it. Required for
 * simulation and picking; geometry consumers may ignore it.
 */
model BindingEntry {
  operation_index: uint32;
  source_kind: BindingSourceKind;
  source_uid: string;
  refdes?: string;
  pad_number?: string;
  net_key?: string;
}

// ---------------------------------------------------------------- scope, chunks, document

model MaterializationScope {
  products: MaterialProduct[];
  /** Absent means every slice. */
  slice_keys?: string[];
  /** Absent means every net. */
  net_keys?: string[];
  region?: NmBounds;
  tile_size_nm: NonNegativeNm;
  /** True only when every product, slice, and net was materialized without a region. */
  complete: boolean;
}

/**
 * Operations of one (slice, tile) occupy a contiguous index range. An
 * operation belongs to the tile containing the centre of its bounds; the
 * chunk bounds cover every operation in it.
 */
model ChunkEntry {
  slice_key: string;
  tile_x: int32;
  tile_y: int32;
  bounds: NmBounds;
  net_keys: string[];
  first_operation_index: uint32;
  operation_count: uint32;
}

enum DiagnosticSeverity {
  info,
  warning,
  error,
}

/** A finding of the producer. `product` scopes it so selective results filter it like operations. */
model MaterializationDiagnostic {
  severity: DiagnosticSeverity;
  code: string;
  message: string;
  product?: MaterialProduct;
  source_uid?: string;
}

model MaterializationProducer {
  name: string;
  version: string;
}

model MaterializationSource {
  sha256: string;
  source_bytes: uint64;
}

model MaterializationCoordinateSystem {
  unit: "nm";
  x_axis: "board_right";
  y_axis: "board_down";
}

/** Resource-bounded materialization request. Source bytes are out of band. */
@jsonSchema
@id("urn:wavenumber:schema:kicad_monkey.materialization.request:a0")
@extension("title", "Materialization request a0")
model MaterializationRequest {
  type: "kicad_monkey.materialization.request";
  version: "a0";
  products: MaterialProduct[];
  slice_keys?: string[];
  net_keys?: string[];
  region?: NmBounds;
  tile_size_nm: NonNegativeNm;
  max_source_bytes: uint64;
  max_operations: uint32;
  max_points: uint32;
}

/**
 * Analytic materialization of one KiCad board. This JSON form is the
 * deterministic debug view; the packed columnar encoding is canonical.
 */
@jsonSchema
@id("urn:wavenumber:schema:kicad_monkey.materialization.document:a0")
@extension("title", "Materialization document a0")
model MaterializationDocument {
  schema: "kicad_monkey.materialization.a0";
  type: "kicad_monkey.materialization.document";
  version: "a0";
  producer: MaterializationProducer;
  source: MaterializationSource;
  coordinate_system: MaterializationCoordinateSystem;
  scope: MaterializationScope;
  stack: Stack;
  nets: Net[];
  net_classes: NetClass[];
  diff_pairs: DiffPair[];
  components: Component[];
  operations: MaterialOperation[];
  binding: BindingEntry[];
  chunks: ChunkEntry[];
  diagnostics: MaterializationDiagnostic[];
}

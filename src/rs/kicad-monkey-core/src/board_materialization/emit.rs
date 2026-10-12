//! Operation sink shared by every product emitter.
//!
//! Each emitted operation receives a provisional index and a binding entry;
//! final indices are assigned by chunking.

use super::geometry::board_bounds;
use super::holes::HoleSpec;
use kicad_monkey_contracts::generated::materialization_document::{
    Affine2D, Backdrill, BindingEntry, BindingSourceKind, DiagnosticSeverity, HoleOperation,
    HolePlating, HoleProtection, MaterialAction, MaterialOperation, MaterialProduct,
    MaterializationDiagnostic, NonNegativeNm, Shape2d, SurfaceMaterial, SurfaceOperation,
};

/// KiCad object an operation came from.
#[derive(Clone)]
pub(crate) struct Origin {
    pub kind: BindingSourceKind,
    pub uid: String,
    pub refdes: Option<String>,
    pub pad_number: Option<String>,
    pub net_key: String,
}

impl Origin {
    pub(crate) fn routed(kind: BindingSourceKind, uid: String, net_key: String) -> Self {
        Self {
            kind,
            uid,
            refdes: None,
            pad_number: None,
            net_key,
        }
    }
}

/// Physical facts of one hole besides its shape and placement.
#[derive(Clone)]
pub(crate) struct HoleFacts {
    pub plating: HolePlating,
    pub start_slice_key: String,
    pub end_slice_key: String,
    pub plating_thickness_nm: Option<i64>,
    pub protection: Option<HoleProtection>,
    pub backdrills: Vec<Backdrill>,
}

/// Material, action, and owning net of a surface operation.
pub(crate) struct Material {
    pub material: SurfaceMaterial,
    pub action: MaterialAction,
    pub net_key: Option<String>,
}

#[derive(Default)]
pub(crate) struct Emitter {
    pub operations: Vec<MaterialOperation>,
    pub binding: Vec<BindingEntry>,
    pub diagnostics: Vec<MaterializationDiagnostic>,
    /// Product that warnings raised now belong to.
    pub product: Option<MaterialProduct>,
}

/// Stable source identity: the KiCad UUID, or `kind@byte_offset` when the
/// object has none.
pub(crate) fn source_uid(kind: &str, uuid: Option<&str>, source_start: usize) -> String {
    uuid.filter(|value| !value.is_empty())
        .map_or_else(|| format!("{kind}@{source_start}"), str::to_owned)
}

impl Emitter {
    fn next_index(&self) -> u32 {
        u32::try_from(self.operations.len()).unwrap_or(u32::MAX)
    }

    fn bind(&mut self, index: u32, origin: &Origin, net_key: Option<&str>) {
        self.binding.push(BindingEntry {
            operation_index: index,
            source_kind: origin.kind,
            source_uid: origin.uid.clone(),
            refdes: origin.refdes.clone(),
            pad_number: origin.pad_number.clone(),
            net_key: net_key.map(str::to_owned),
        });
    }

    /// Add copper of `origin`'s net on `slice_key`.
    pub(crate) fn copper(
        &mut self,
        origin: &Origin,
        slice_key: &str,
        shape: Shape2d,
        affine: Affine2D,
    ) {
        let material = Material {
            material: SurfaceMaterial::Copper,
            action: MaterialAction::Add,
            net_key: Some(origin.net_key.clone()),
        };
        self.surface(origin, slice_key, material, shape, affine);
    }

    /// Add or subtract `material` on `slice_key`.
    pub(crate) fn surface(
        &mut self,
        origin: &Origin,
        slice_key: &str,
        material: Material,
        shape: Shape2d,
        affine: Affine2D,
    ) {
        let index = self.next_index();
        let bounds = board_bounds(&shape, &affine);
        self.bind(index, origin, material.net_key.as_deref());
        self.operations
            .push(MaterialOperation::SurfaceOperation(SurfaceOperation {
                kind: "surface".to_owned(),
                index,
                action: material.action,
                material: material.material,
                slice_key: slice_key.to_owned(),
                net_key: material.net_key,
                shape,
                affine,
                bounds,
            }));
    }

    /// Add a hole. Plated holes carry the origin's net; unplated holes none.
    pub(crate) fn hole(&mut self, spec: &HoleSpec) {
        let index = self.next_index();
        let bounds = board_bounds(&spec.shape, &spec.affine);
        let facts = spec.facts.clone();
        let net_key =
            matches!(facts.plating, HolePlating::Plated).then(|| spec.origin.net_key.clone());
        self.bind(index, &spec.origin, net_key.as_deref());
        self.operations
            .push(MaterialOperation::HoleOperation(HoleOperation {
                kind: "hole".to_owned(),
                index,
                shape: spec.shape.clone(),
                affine: spec.affine.clone(),
                plating: facts.plating,
                start_slice_key: facts.start_slice_key,
                end_slice_key: facts.end_slice_key,
                net_key,
                plating_thickness_nm: facts.plating_thickness_nm.map(NonNegativeNm),
                protection: facts.protection,
                backdrills: facts.backdrills,
                bounds,
            }));
    }

    pub(crate) fn warn(&mut self, code: &str, message: String, source_uid: Option<String>) {
        self.diagnostics.push(MaterializationDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: code.to_owned(),
            message,
            product: self.product,
            source_uid,
        });
    }
}

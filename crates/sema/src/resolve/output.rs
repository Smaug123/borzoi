//! What the walk publishes ([`Output`]), moved into the
//! [`ResolvedFile`] in one piece when it finishes. The
//! walk reads some of it back (its own definitions and exports, the
//! declaration list) as it goes.

use crate::hash::{HashMap, HashSet};

use borzoi_cst::syntax::TextRange;

use super::contributors::{FileContributions, OpenContributions};
use super::model::{
    DeclaredType, DeclineSite, ExportDecl, ExportedItem, ExportedItems, OpenTrace, Resolution,
    ResolutionTrace, ResolvedFile,
};
use super::prescan::Prescan;
use super::state::ActivePatternShape;
use super::unknowables::Unknowables;
use crate::def::{Def, DefId};
use crate::diagnostics::SemaDiagnostic;
use crate::recovery::SyntaxRecovery;

/// The walk's output; see the [module docs](self).
#[derive(Default)]
pub(super) struct Output {
    pub(super) defs: Vec<Def>,
    pub(super) items: Vec<ExportedItem>,
    pub(super) resolutions: HashMap<TextRange, Resolution>,
    /// The subset of [`Self::resolutions`] recorded for an **or-pattern
    /// alias** — see [`ResolvedFile::is_or_pattern_alias`](super::ResolvedFile).
    pub(super) or_pattern_aliases: HashSet<TextRange>,
    /// See [`ResolvedFile::opaque_binders`](super::ResolvedFile::opaque_binders).
    pub(super) opaque_binders: HashSet<TextRange>,
    /// See [`ResolvedFile::opaque_regions`](super::ResolvedFile::opaque_regions).
    pub(super) opaque_regions: Vec<super::model::OpaqueRegion>,
    /// The reasons raised so far that a file-wide question has no enumerable
    /// answer ([`Unknowables`]): raised, never lowered.
    pub(super) unknowables: Unknowables,
    /// Every declared project namespace path in the file (see
    /// [`ResolvedFile::namespace_paths`](super::model::ResolvedFile::namespace_paths)).
    pub(super) namespace_paths: Vec<Vec<String>>,
    /// EX-2 (`docs/extension-scope-enumeration-plan.md`): the **assembly**
    /// namespace paths an explicit `open <namespace>` brings into scope, unioned
    /// across every `open` in the file. The overload engine's extension-absence
    /// gate ([`crate::infer`]'s `ExtensionScope`) reads these — an extension
    /// declared in an opened assembly namespace is in scope exactly as one in the
    /// file's declared namespace chain is, so the gate asks the *same*
    /// [`AssemblyEnv::extension_named_in_scope`](crate::AssemblyEnv) query about
    /// them. Only `open`s that resolve *entirely* to assembly-namespace readings
    /// with no residual uncertainty land here; anything else raises
    /// [`Unknowable::OpenExtensionSurface`](super::unknowables::Unknowable::OpenExtensionSurface)
    /// instead.
    ///
    /// **File-global, never block-scoped.** The gate is file-global (a single
    /// `ExtensionScope` per file), so an `open` in any nested module is folded in
    /// here for the whole file — an over-approximation that can only *add*
    /// deferrals (an `open` leaking out of its module makes the gate ask about a
    /// namespace it need not), never a wrong commit. Deliberately absent from the
    /// nested-module save/restore in `decls.rs` for that reason.
    pub(super) open_extension_namespaces: Vec<Vec<String>>,
    /// The *type* each written attribute resolved to, keyed by the written
    /// name's range — built by `Resolver::resolve_attribute_lists` and carried
    /// into [`ResolvedFile::attribute_resolutions`](super::model::ResolvedFile).
    pub(super) attribute_resolutions: HashMap<TextRange, Resolution>,
    /// The type definitions the walk placed, in walk order, for
    /// [`ResolvedFile::placed_types`](super::model::ResolvedFile). Only a
    /// definition with a real path — not under an anonymous (headerless)
    /// root — is placed; [`Prescan::own_type_decl_counts`](crate::resolve::prescan::Prescan::own_type_decl_counts) says how many
    /// declarations of the name there are in all.
    pub(super) placed_types: Vec<DeclaredType>,
    /// The **instance** member names this file's `type … with` augmentations
    /// declare (EX-3 §2(a)): an augmentation member joins its own name's
    /// call group, so the gate defers exactly those names instead of every
    /// call. Collected from *every* augmentation shape (intrinsic or optional,
    /// single-ident or dotted head — member names need no head resolution).
    pub(super) augmentation_instance_names: HashSet<String>,
    /// The **static** sibling of [`Self::augmentation_instance_names`]. A
    /// member whose staticness is not walkable lands in both sets.
    pub(super) augmentation_static_names: HashSet<String>,
    /// What this file's declarations contribute to later files through routes
    /// the walk does not model, as far as the walk records it
    /// ([`OpenContributions`]).
    pub(super) contributions: OpenContributions,
    /// The [`ActivePatternShape`] of each same-file active-pattern recognizer,
    /// keyed by each of its per-case *use def ids* (the `Resolution::Local`
    /// identity a case use resolves to). Written once per recognizer by
    /// [`define_active_pattern`](crate::resolve::Resolver::define_active_pattern); read back
    /// through [`ResolvedFile::active_pattern_shape`](super::model::ResolvedFile::active_pattern_shape).
    /// A never-cleared file-lifetime map (the `Resolver` is per-file, def ids
    /// are unique within a file). Read back through
    /// [`ResolvedFile::active_pattern_shape`](super::model::ResolvedFile::active_pattern_shape)
    /// and consumed by the applied-head split in
    /// [`resolve_pat_types`](crate::resolve::Resolver::resolve_pat_types) (Stage 2 of
    /// `docs/parameterized-active-pattern-args-plan.md`).
    pub(super) active_pattern_shape: HashMap<DefId, ActivePatternShape>,
    /// Always-sound semantic diagnostics accumulated during the walk (today
    /// only `use rec`; see [`SemaDiagnostic`]). Source-ordered because the
    /// walk is.
    pub(super) diagnostics: Vec<SemaDiagnostic>,
    /// The resolution-explain trace — one [`OpenTrace`] per `open` declaration,
    /// in source order, recording the opaque-open flags it flipped. Accumulated
    /// across every top-level block (a *file*-level record, deliberately absent
    /// from the per-block open-state reset), and moved into
    /// [`ResolvedFile::resolution_trace`](super::model::ResolvedFile). Purely
    /// diagnostic — nothing the walk consumes reads it.
    pub(super) trace_opens: Vec<OpenTrace>,
    /// The decline census — which guard declined each declined occurrence, keyed
    /// by its source range, moved into
    /// [`ResolvedFile::decline_sites`](super::model::ResolvedFile). Purely
    /// diagnostic on the same terms as [`Self::trace_opens`]: nothing the walk
    /// consumes reads it.
    pub(super) decline_sites: HashMap<TextRange, DeclineSite>,
    /// The file's cross-file declarations, in source order — the single currency
    /// [`ProjectItems::extend_with`](super::model::ProjectItems::extend_with) folds
    /// (`docs/export-decl-model-plan.md` Stage 2). Every cross-file index derives
    /// from this list; a decl is appended at the same program point each legacy
    /// export writer fires, so the derivations reproduce today's ordering.
    pub(super) export_decls: Vec<ExportDecl>,
}

impl Output {
    /// The published file: what the walk recorded, with what the prescan
    /// decided before it.
    pub(super) fn into_resolved_file(
        self,
        prescan: Prescan,
        item_base: u32,
        recovery: &SyntaxRecovery,
    ) -> ResolvedFile {
        // Exhaustive, so a field added to `Output` is a compile error here
        // until it is published.
        let Output {
            defs,
            items,
            resolutions,
            or_pattern_aliases,
            opaque_binders,
            opaque_regions,
            unknowables,
            namespace_paths,
            open_extension_namespaces,
            attribute_resolutions,
            placed_types,
            augmentation_instance_names,
            augmentation_static_names,
            contributions,
            active_pattern_shape,
            diagnostics,
            trace_opens,
            decline_sites,
            export_decls,
        } = self;
        let unplaced_types = unplaced_types(&prescan, &placed_types);
        let OpenContributions {
            root,
            module_opens,
            namespace_opens,
        } = contributions;
        ResolvedFile {
            defs,
            resolutions,
            or_pattern_aliases,
            opaque_binders,
            opaque_regions,
            attribute_resolutions,
            placed_types,
            unplaced_types,
            unplaced_type_places: HashMap::default(),
            own_type_simple_names: prescan.own_type_simple_names,
            own_module_simple_names: prescan.own_module_simple_names,
            own_abbrev_type_simple_names: prescan.own_abbrev_type_simple_names,
            contributions: FileContributions {
                root,
                extension_statics: prescan.extension_statics,
                module_opens,
                namespace_opens,
                custom_operations: prescan.custom_operations,
                declares_custom_operation_alias: prescan.declares_custom_operation_alias,
                declares_slot_attribute_alias: prescan.declares_slot_attribute_alias,
            },
            declares_core_attribute_alias: prescan.declares_core_attribute_alias,
            unknowables,
            augmentation_instance_names,
            augmentation_static_names,
            // Stamped by the Compile-order fold; a single file has no
            // preceding files.
            preceding_augmentation_instance_names: HashSet::default(),
            preceding_augmentation_static_names: HashSet::default(),
            exports: ExportedItems { items },
            item_base,
            namespace_paths,
            // The *cross-file* (preceding-files) signal is set by
            // `resolve_project`; a single file has no preceding files.
            preceding_declares_extension_source: false,
            open_extension_namespaces,
            active_pattern_shape,
            diagnostics,
            resolution_trace: ResolutionTrace { opens: trace_opens },
            decline_sites,
            export_decls,
            sig_screen: None,
            sig_exports: Vec::new(),
            recovery: recovery.clone(),
        }
    }
}

/// [`ResolvedFile::unplaced_types`](super::ResolvedFile::unplaced_types): how
/// many declarations of each name the file makes — a type, an exception, an
/// augmentation head — that the walk did not place. An augmentation head
/// alone declares nothing, but counts as one: it may name a type of another
/// file, and nothing here says which.
fn unplaced_types(prescan: &Prescan, placed_types: &[DeclaredType]) -> HashMap<String, usize> {
    prescan
        .own_type_simple_names
        .iter()
        .filter_map(|name| {
            let placed = placed_types
                .iter()
                .filter(|t| t.path.last() == Some(name))
                .count();
            let declared = prescan
                .own_type_decl_counts
                .get(name)
                .copied()
                .unwrap_or(0)
                .max(1);
            let unplaced = declared.saturating_sub(placed);
            (unplaced > 0).then(|| (name.clone(), unplaced))
        })
        .collect()
}

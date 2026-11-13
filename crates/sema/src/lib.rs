//! `borzoi-sema` — semantic analysis (name resolution, and later type
//! inference) over the [`borzoi_cst`] AST.
//!
//! The crate is built incrementally; see `docs/type-checker-plan.md` for the
//! overall design and `docs/completed/sema-phase1-impl-plan.md` for the
//! finished phase 1 plan.
//!
//! The definition model ([`Def`], [`DefKind`]) and pure pattern *binder
//! extraction* ([`pattern_names`]) provide the name occurrences a pattern
//! contributes. On top of
//! them, [`resolve_file`] builds a position-ordered scope tree from a parsed
//! file and resolves every name *use* the current parser subset can express to
//! its defining binder, returning a [`ResolvedFile`]. [`resolve_project`] folds
//! [`resolve_file`] over the files in Compile order, threading each file's
//! exports forward so a later file's module-qualified reference (`Shared.foo`)
//! resolves to the earlier file's binder; it returns a [`ResolvedProject`].
//! Resolution is differentially tested against FCS (`crates/sema/tests/`).
//!
//! [`AssemblyEnv`] is the complementary environment for names that resolve into
//! *referenced assemblies*: a flattened, name-indexed view over the
//! [`borzoi_assembly`] entity model ([`EntityHandle`] / [`MemberIndex`]).
//! [`resolve_file`] consults it to resolve a fully-qualified path
//! (`System.Console.WriteLine`) to the referenced [`Resolution::Entity`] /
//! [`Resolution::Member`].
//!
//! Type inference is beginning to land on top of resolution (Phase 3 of the
//! plan). [`infer_file`] is the best-effort entry point; today it covers
//! Stage 3.1 — *literal typing* — assigning a [`Ty`] to each literal sitting in
//! a soundness-safe position (the immediate RHS of an unannotated `let`, where
//! no expected type can retarget it) and leaving everything else Deferred (the
//! same say-nothing-when-unsure contract resolution uses). It too is
//! differentially tested against FCS, via the expression-type oracle. Under the
//! hood it runs the plan's generate→solve pipeline (Stage 3.2a): the `unify`
//! module's [`ena`]-backed union-find substrate solves an inert constraint set,
//! the foundation the rest of the HM spine builds on.

mod assembly_env;
mod binders;
mod def;
mod diagnostics;
mod function_ty;
pub mod hash;
mod infer;
mod infer_project;
mod match_reach;
mod member_ty;
mod names;
mod node_span;
mod overload;
mod qnof;
mod recovery;
mod resolve;
#[cfg(feature = "test-support")]
pub mod test_support;
mod ty;
mod unify;

pub use assembly_env::{
    AbbreviationVisibility, AssemblyEnv, AssemblyProjectionInput, EntityHandle, ExtensionMembers,
    IlTypeDefinition, MemberIndex, OpenFoldName, OpenFoldSpace, OpenFoldSurface, OpenFoldTarget,
    StaticLookup, UnionCaseIndex, ValueSurface, fsharp_core_intrinsic_tycons,
};
pub use binders::{BinderRole, PatternName, pattern_names};
pub use def::{Def, DefId, DefKind, SemanticClass};
pub use diagnostics::{SemaDiagnostic, SemaDiagnosticKind};
pub use function_ty::FunctionTyGap;
pub use infer::{
    BuiltinGap, ConstraintGap, Incomplete, InferredFile, OperatorGap, infer_file, infer_named_file,
};
pub use infer_project::{
    InferredProject, PrecedingGap, PrecedingTypes, infer_project, infer_project_reusing,
    infer_project_reusing_with_reuse,
};
pub use overload::{ArityWindow, arity_window};
pub use qnof::{QualifiedNameOfFile, qualified_names};
pub use recovery::SyntaxRecovery;
pub use resolve::{
    ActivePatternShape, CaseKind, DeclineCause, DeclineSite, DeclineTier, DeferredReason,
    ExportedItem, ExportedItems, ItemId, OpaqueRegion, OpenOpacity, OpenTrace, ProjectFile,
    ProjectItems, Resolution, ResolutionTrace, ResolvedFile, ResolvedProject, SourceFile,
    resolve_file, resolve_project, resolve_project_files, resolve_project_files_incremental,
    resolve_project_files_prefix, resolve_project_files_prefix_incremental,
    resolve_project_incremental, resolve_project_incremental_with_reuse, signature_partners,
};
// The path-labelled fold variants exist only in profiling builds; the LSP calls
// them (in place of the prefix fold variants) to tag each file's span with its
// path.
#[cfg(feature = "otel")]
pub use resolve::{
    resolve_project_files_prefix_incremental_labeled, resolve_project_files_prefix_labeled,
};
// `TyVid` is re-exported because it appears in the public `Ty::Var` variant; it
// is an inference-internal handle that never surfaces in `infer_file` output.
pub use ty::{MAX_TYPE_DEPTH, MAX_TYPE_NODES, Ty, TySize, TyVid, TypeBudgetExceeded};
pub use unify::TypeWalks;

/// The stack a thread needs to resolve and infer any tree `borzoi_cst`'s
/// parser returns, with a margin of two: on the tallest, the walks use at most
/// half of it.
///
/// Resolution and inference recurse once per level of the tree, and a stack
/// overflow aborts the process, which `catch_unwind` cannot recover. The
/// parser bounds the tree's height
/// ([`MAX_TREE_HEIGHT`](borzoi_cst::parser::MAX_TREE_HEIGHT)), and so what the
/// walks need: about 8 MiB in a debug build, whose frames are the largest, for
/// an operator chain near the bound under four hundred nested modules. That is
/// all of a platform's default main thread (8 MiB on macOS and Linux), and
/// four times a spawned thread's, so a caller that may hand the walks
/// arbitrary input runs them on a thread of this size, as the language
/// server's request loop does.
/// `tests/all/walk_stack_bound.rs` holds every repetition form of the grammar,
/// alone and under nested modules, to half of it.
pub const WALK_STACK_BYTES: usize = 32 << 20;

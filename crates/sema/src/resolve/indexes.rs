//! What the walk has recorded about the file's declarations so far, for its
//! own lookups ([`Indexes`]). An index lives as long as the file's walk:
//! nothing in it is undone when the walk leaves the scope that recorded it.
//! It grows as the walk passes declarations, and a later declaration of the
//! same name replaces an earlier one where F# reads the latest (a redefined
//! type's cases and members). None of it is published:
//! [`Output`](super::output::Output) is what the walk hands on.

use crate::hash::{HashMap, HashSet};

use borzoi_cst::syntax::TextRange;

use super::model::{DeclineCause, Resolution, TypeSlot};
use super::path_set::PathSet;
use super::state::{DeclKinds, TypeMemberSet, TypeSlotDef};
use crate::def::DefId;
use crate::names::{ChildMap, PathId, PathMap};

/// The walk's indexes; see the [module docs](self).
#[derive(Default)]
pub(super) struct Indexes {
    /// In-file `type` definitions, keyed by their enclosing container
    /// ([`PathId`], interned by [`Lexical::container_path`](crate::resolve::lexical::Lexical::container_path), so a walk along
    /// the enclosing containers probes once per level without hashing a path)
    /// then by `idText` name → defining binder. F#
    /// keeps types in a *separate* namespace from values, so this is queried
    /// only from type-syntactic positions ([`Resolver::resolve_type`](crate::resolve::Resolver::resolve_type)) — never the
    /// value [`Resolver::lookup`](crate::resolve::Resolver::lookup) — and so a type `T` and a value `T` never collide.
    ///
    /// **Container-scoped, not module-block-scoped**: the map holds the types
    /// declared under a container across however many source blocks that
    /// container is split into, but a walk over it must ask how each one
    /// reaches the use ([`Resolver::in_file_type_reach`](crate::resolve::Resolver::in_file_type_reach)): only the current
    /// block's are lexical. A later `namespace N` block's `type B = A` still
    /// resolves `A` to an earlier block's `type A`, through the block's
    /// implicit open of `N`; a `namespace N.Sub` block does not see it at
    /// all. Two *distinct* containers (`namespace A` vs `namespace B`) stay
    /// isolated because their keys differ. Within a
    /// container, a later type may reference an earlier one, and a whole
    /// `type … and …` group is interned before any of its RHSs is resolved, so
    /// the group is mutually recursive. Cross-file type resolution (`A.T` from
    /// another file) is a later slice — those references stay deferred via the
    /// existing [`Resolver::record_project_name_shadow`](crate::resolve::Resolver::record_project_name_shadow).
    pub(super) type_defs: HashMap<PathId, HashMap<String, DefId>>,
    /// Every in-file type definition's position and [`SlotClass`](crate::resolve::model::SlotClass), by
    /// container and simple name — all of them, not only the latest:
    /// same-named types of different arities each take the unqualified slot, and the latest of any of them
    /// is what orders a value ([`Resolver::head_value_slot`](crate::resolve::Resolver::head_value_slot)). Keyed by the
    /// container's interned path and the name's, so a probe for a name the
    /// table never interned costs no hashing of its text past the one
    /// lookup that says so.
    pub(super) type_slot_history: ChildMap<Vec<TypeSlotDef>>,
    /// Each in-file type's **access-root** — keyed by container path, like [`Self::type_defs`], and
    /// populated by the same [`define_type`](crate::resolve::Resolver::define_type) call. `None` =
    /// public; `Some(k)` = accessible only from a site within the `k`-segment
    /// prefix of the type's qualified path (its `private` container plus any
    /// enclosing `private` module). A same-file module-qualified `A.Foo.Red`
    /// resolution reads it so a `private` type inaccessible from the reference site
    /// (e.g. a sibling module) does not resolve its case/member (FCS FS1092).
    pub(super) type_access_roots: HashMap<Vec<String>, HashMap<String, Option<usize>>>,
    /// Each in-file case of a union whose representation is `private`, by its
    /// binder, to the union's container: the case is usable only from within it
    /// ([`Resolver::case_usable_here`](crate::resolve::Resolver::case_usable_here)). Keyed by binder rather than by export
    /// because an anonymous-root file's cases have no export, and still reach a
    /// sibling module through `A.U.X`.
    pub(super) case_repr_roots: HashMap<DefId, Vec<String>>,
    /// The **complete declared-name view of each container** — keyed by the
    /// container's [`PathId`], like [`Self::type_defs`], mapping each
    /// declared name to the namespaces it occupies ([`DeclKinds`]); read by
    /// path through [`declared_in`](crate::resolve::Resolver::declared_in). Populated from the same declaration walk that fills
    /// `type_defs` / `type_cases` / `module_like_names` / the value frames, one mark
    /// per declaration ([`mark_decl`](crate::resolve::Resolver::mark_decl)). Unlike those partial indices
    /// (and unlike `by_qualified_path`, which omits anonymous-root nested members),
    /// this is exhaustive for the modeled subset, so the same-file module-qualified
    /// case resolver
    /// ([`classify_same_file_module_qualified_case`](crate::resolve::Resolver::classify_same_file_module_qualified_case))
    /// can decide a head/segment with *certainty* — emit only when unambiguous, defer
    /// on any contention (Gap A of `docs/type-qualified-case-prefix-plan.md`).
    pub(super) container_decls: HashMap<PathId, HashMap<String, DeclKinds>>,
    /// In-file **type-qualified cases**, keyed by enclosing container path then by
    /// *type* name then by case name → the case's defining binder. Holds every
    /// `enum` case **and** every union case (`[<RequireQualifiedAccess>]` or not),
    /// so a qualified `Color.Red` ([`resolve_long_ident`](crate::resolve::Resolver::resolve_long_ident))
    /// resolves the head `Color` to the [`Self::type_defs`] entry and the whole span
    /// to the case here — for unions and enums uniformly. Enum and RQA-union cases
    /// are reachable *only* this way (never bare `Red` — FCS reports `FS0039`); a
    /// non-RQA union case is *also* a value-namespace member (it lives in
    /// [`Lexical::top_level`](crate::resolve::lexical::Lexical::top_level) too, for bare / `Mod.Case` access), and this index adds
    /// the type-qualified path on top. Container-scoped like `type_defs` (looked up
    /// under the same container as the type). Cross-file `Lib.Color.Red` is resolved
    /// through the separate [`ProjectItems`](crate::resolve::model::ProjectItems) type-qualified-case index.
    ///
    /// Stores each case's **[`Resolution`]** (not a bare [`DefId`]), so `Color.Red`
    /// resolves to the *same* resolution as the case's declaration (one identity for
    /// find-references / rename). In a real-root file every case is exported and so
    /// is a [`Resolution::Item`] — a non-RQA union case reuses its value-namespace
    /// handle, an enum / RQA-union case its dedicated type-qualified handle; only an
    /// anonymous-root case (no cross-file handle) is a [`Resolution::Local`].
    pub(super) type_cases: HashMap<Vec<String>, HashMap<String, HashMap<String, Resolution>>>,
    /// Each in-file union's cases, by its [`Self::type_defs`] binder, in
    /// declaration order: the frame its own members see. FCS puts a type's
    /// union cases above the module scope inside its members and its intrinsic
    /// augmentations' members, `[<RequireQualifiedAccess>]` or not, so they
    /// shadow a same-named module value there even when it is declared later.
    pub(super) own_union_cases: HashMap<DefId, Vec<(String, Resolution)>>,
    /// Each in-file type's generic-parameter frame, by its binder. An intrinsic augmentation's `'T` is the
    /// augmented type's own parameter (FCS declares its uses there), so the
    /// augmentation's members resolve against this frame, not its header's.
    pub(super) type_typar_frames: HashMap<DefId, Vec<(String, DefId)>>,
    /// In-file **type members**, keyed like [`Self::type_cases`] (container path →
    /// *type* name → [`TypeMemberSet`]). Populated for every genuine type
    /// definition from its object-model / trailing members, and *merged into* by
    /// each same-container single-ident augmentation (`type T with …`) — with the
    /// augmentation's own source position as the entries' visibility start, since
    /// augmentation members do not exist before their declaration (FCS `FS0039`,
    /// probe M4a of `docs/project-type-member-plan.md`). Powers the D1 member
    /// *emit* (`Color.Red` where `Red` is a public static — probes M1/M2a/M2d/M4b)
    /// and the owned-name question (an instance member commits the qualifier and
    /// errors rather than backtracking — probe M9); member-*absence* reasoning
    /// (D2) is a later stage. Cross-file member export is a later stage too —
    /// cross-file references keep deferring through the existing shadow indexes.
    pub(super) type_members: HashMap<Vec<String>, HashMap<String, TypeMemberSet>>,
    /// Type names targeted by an augmentation this index could **not** file — a
    /// dotted head (`type A.B with …`) or a head that does not name a type of the
    /// current container (e.g. a module-housed optional extension of an outer
    /// type, whose visibility is scope-dependent). Any name in here suppresses
    /// member *emission* for same-named types file-wide: the unfiled members
    /// could overload or shadow an indexed one, so emitting would risk a wrong
    /// target. Deliberately name-keyed (not path-keyed) — over-suppression only
    /// costs availability.
    pub(super) unindexed_augmented_names: HashSet<String>,
    /// Names of *module-like* declarations — nested `module M = …` and module
    /// abbreviations `module X = …` — keyed by the container they are declared
    /// directly in. Their members are unmodelled, so for **member access** a
    /// module-like name shadows a same-named *type* in an enclosing container:
    /// `Color.Red` inside a nested `module Color` is the module's member (defer),
    /// not an enclosing `enum Color`'s case. [`type_case_path`](crate::resolve::Resolver::type_case_path)
    /// consults this while walking outward so it defers rather than wrongly
    /// reaching the enclosing enum. (Type-position resolution
    /// [`lookup_type_def_at_arity`](crate::resolve::Resolver::lookup_type_def_at_arity) does *not* consult it — a module
    /// does not shadow a type in the type namespace.)
    pub(super) module_like_names: HashMap<PathId, HashSet<String>>,
    /// Every declared named-module path in the file, accumulated as the walk
    /// enters each module (read back by
    /// [`is_project_module_path`](crate::resolve::Resolver::is_project_module_path)).
    pub(super) module_paths: PathSet,
    /// Every nested `[<AutoOpen>]` module this file has declared so far, by
    /// its path under the container chain — the anonymous root's included,
    /// which [`Self::auto_open_module_paths`] leaves out because nothing
    /// outside the file can reach it.
    pub(super) lexical_auto_open_paths: Vec<Vec<String>>,
    /// The same nested modules' **qualified** paths (`module_path` prefix + the
    /// local name, `["Demo", "Calc"]`), exported via [`ResolvedFile`](super::model::ResolvedFile) into the
    /// cross-file shadow index so a *later* file's reference (`Demo.Calc.Answer`)
    /// defers too — see [`ProjectItems::nested_module_paths`](crate::resolve::model::ProjectItems::nested_module_paths).
    pub(super) nested_module_exports: PathSet,
    /// See [`Lexical::augmentation_head_locals`](crate::resolve::lexical::Lexical::augmentation_head_locals).
    pub(super) augmentation_head_exports: PathSet,
    /// Qualified paths of the file's **real** nested `module X = …` definitions —
    /// the module-only subset of [`Self::nested_module_exports`], which (via
    /// [`record_project_name_shadow`](crate::resolve::Resolver::record_project_name_shadow)) conflates
    /// every project-introduced name: types, exceptions, module abbreviations,
    /// and `extern`s too. Folded into [`ProjectItems::real_nested_modules`](crate::resolve::model::ProjectItems::real_nested_modules) so a
    /// later file can ask "is there a genuine module at this path?" without a
    /// type's shadow answering yes — the companion-submodule arm of
    /// [`open_contests_candidate`](crate::resolve::Resolver::open_contests_candidate) (a cross-file
    /// *type* at an open-supplied head is transparent, probes CF12/CF13; a
    /// *module* owning the residual is not, probe CF11). Guarded by
    /// [`Lexical::anonymous_root`](crate::resolve::lexical::Lexical::anonymous_root) like the other cross-file exports.
    pub(super) real_nested_module_exports: PathSet,
    /// Every **type definition**'s qualified export path (`["A", "Pal", "Color"]`
    /// = container + type name) paired with whether its **case set is fully
    /// indexed** in the type-qualified case exports (the `type_qualified` paths on
    /// the case `Item` [`ExportDecl`](crate::resolve::model::ExportDecl)s) — `true` for a genuine
    /// non-abbreviation repr (a union/enum's cases are all exported; a
    /// record/object-model/delegate owns none), `false` for an abbreviation
    /// (whose cases live on its target, which sema does not chase cross-file) or
    /// a bodyless repr. Exported via [`ResolvedFile`](super::model::ResolvedFile)
    /// into [`ProjectItems::type_paths`](crate::resolve::model::ProjectItems::type_paths) — the cross-file type index, which lets a
    /// later file decide whether an open target's segment names a type at all
    /// and, when the flag is `true`, prove the type owns no given case
    /// ([`Resolver::open_contests_candidate`](crate::resolve::Resolver::open_contests_candidate)).
    /// Source-ordered ([`define_type`](crate::resolve::Resolver::define_type)'s last-wins carries
    /// over); empty for an anonymous-root file (no cross-file path). Each
    /// entry also carries the type's [`TypeSlot`], consumed by
    /// [`head_value_slot`](crate::resolve::Resolver::head_value_slot) to decide eviction.
    pub(super) type_path_exports: Vec<(Vec<String>, bool, TypeSlot)>,
    /// Each path in [`Self::type_path_exports`], interned, to the index of
    /// its latest entry there: the entry a later definition at the same path
    /// shadows the earlier ones with.
    pub(super) type_path_export_ids: HashMap<PathId, usize>,
    /// Qualified paths of in-project modules whose `open` may bring **value-space
    /// names we cannot enumerate**: a module declaring union cases / exception
    /// constructors / active patterns (we enumerate only `let` values), or a
    /// module *alias* whose target we could **not** resolve to an in-project
    /// module (an alias of an assembly module — a resolvable alias is canonicalised
    /// instead, see [`Self::module_aliases`]). Opening one bumps
    /// [`OpenScope::open_generation`](crate::resolve::open_scope::OpenScope::open_generation) so it shadows earlier opens. A *file-level*
    /// accumulator (a module is recorded as the walk passes its definition, which
    /// always precedes any `open` of it — same file earlier in source, or an
    /// earlier Compile-order file via [`Preceding`](super::model::ProjectItems)); merged across files like
    /// [`Self::nested_module_exports`].
    pub(super) modules_with_hidden_values: HashSet<Vec<String>>,
    /// Qualified paths of `[<AutoOpen>]` modules declared in this file,
    /// each with its `module private` bit — one record per module, written by
    /// [`Resolver::record_auto_open_module`](super::Resolver) from both
    /// declaration sites (top-level header, nested module). The same-file
    /// shadow walk consults every entry (a `private` module is still visible
    /// within its own file); the cross-file export
    /// ([`ProjectItems::auto_open_module_paths`](super::model::ProjectItems), derived
    /// from the non-`private` `Module` [`ExportDecl`](crate::resolve::model::ExportDecl)s)
    /// filters the `private` ones out, since F# does not bring
    /// a `private` module into scope for another file's `open` of its
    /// namespace.
    pub(super) auto_open_module_paths: Vec<(Vec<String>, bool)>,
    /// Each in-file union case's field names, in source order, keyed by the
    /// case's def — `None` for a case declared in the signature form
    /// (`| Some : Value:'T -> 'T option`), whose fields this does not read. An
    /// anonymous field contributes the name F# generates for it (`Item`, or
    /// `Item<n>` among several). F# reads `Case(x = e)` as a named
    /// field only when `x` is one of these; otherwise `x = e` is an equality
    /// passed positionally ([`Resolver::resolve_method_args`](crate::resolve::Resolver::resolve_method_args)).
    pub(super) case_field_names: HashMap<DefId, Option<Vec<String>>>,
    /// How many type declarations the walk has passed under each simple name
    /// and arity, per container — keyed like [`Self::type_defs`], which keeps
    /// only the latest of them. Summed along the container chain at one
    /// arity, more than one means the latest in-file hit may not be the one
    /// FCS binds (of two same-arity declarations in one scope it binds the
    /// first) ([`DeclineCause::InFileTypeNameReused`]).
    pub(super) type_decl_counts: HashMap<PathId, HashMap<String, HashMap<usize, usize>>>,
    /// Each in-file type's generic arity, by its [`Self::type_defs`] binder.
    /// FCS looks a type name up by name *and* arity first, so `E<int>` beside
    /// `type E` and `type E<'T>` binds the generic one whatever their order,
    /// and may bind a same-named type of another container, file or assembly
    /// that a lookup keyed by name alone never weighs. See
    /// [`Resolver::lookup_type_def_at_arity`](crate::resolve::Resolver::lookup_type_def_at_arity).
    pub(super) type_def_arities: HashMap<DefId, usize>,
    /// [`Self::type_defs`] keyed by name and arity, so a container declaring
    /// a name at two arities keeps both (last-wins per arity).
    pub(super) type_defs_by_arity: HashMap<PathId, HashMap<(String, usize), DefId>>,
    /// The start of every top-level block walked so far, the current one
    /// last. An in-file type declared before the current block's start is
    /// not lexically in scope: FCS checks each block afresh
    /// ([`Resolver::in_file_type_reach`](crate::resolve::Resolver::in_file_type_reach)).
    pub(super) block_starts: Vec<u32>,
    /// The in-file types an augmentation of which may be walked
    /// ([`Resolver::member_scope_of`](crate::resolve::Resolver::member_scope_of)): non-generic, of a kind the scope
    /// tables cover (a class, a union or a record), and certainly not
    /// `[<AutoOpen>]`.
    pub(super) augmentable_types: HashSet<DefId>,
    /// The same-file `[<AutoOpen>]` types, by container path and name, whose
    /// intrinsic augmentations extend what they fold
    /// ([`Resolver::type_group_contributions`](crate::resolve::Resolver::type_group_contributions)).
    pub(super) auto_open_type_paths: HashSet<Vec<String>>,
    /// The bare identifiers a computation expression's custom operations
    /// take by name alone, whatever is bound, and why each declines
    /// ([`Resolver::custom_operation_clause_decline`](crate::resolve::Resolver::custom_operation_clause_decline)).
    pub(super) custom_operation_clauses: HashMap<TextRange, DeclineCause>,
    /// The applications of a bare name that construct a type of this file,
    /// keyed by the name's range ([`Resolver::note_in_file_construction`](crate::resolve::Resolver::note_in_file_construction)):
    /// what a builder bound as `let b = T()` is.
    pub(super) in_file_constructions: HashMap<TextRange, DefId>,
    /// In-project **module abbreviations**: the alias's absolute path
    /// (`[scope…, Alias]`) → the *canonical* fully-qualified path of the module it
    /// abbreviates. The RHS is resolved with the same precedence as an `open` path
    /// ([`resolved_project_module`](crate::resolve::Resolver::resolved_project_module)), and chains are
    /// flattened at definition (resolving `module A2 = A1` follows `A1`), so each
    /// value is already a non-alias module path. An abbreviation is a *bare-head
    /// lexical name*: [`lexical_alias_target`](crate::resolve::Resolver::lexical_alias_target) matches
    /// the alias *name* against a reference head when the alias's scope encloses
    /// the use, and `resolved_project_module` rewrites `Alias[.rest]` →
    /// `Target[.rest]` — so `open Alias`/`open Alias.Sub` resolve to the target,
    /// while a qualified `open N.Alias` does not (the alias is not a member).
    /// Only resolvable (in-project) targets are recorded — an unresolvable alias
    /// stays in [`Self::modules_with_hidden_values`] (conservative). A *file-level*
    /// accumulator keyed by absolute path; not yet merged across files, so an alias
    /// declared in an earlier file stays conservative.
    pub(super) module_aliases: HashMap<Vec<String>, Vec<String>>,
    /// The ident-token ranges of active-pattern *parameter* arguments the
    /// shape-keyed split has resolved as expressions (Stage 2 of
    /// `docs/parameterized-active-pattern-args-plan.md`): in `match n with DivBy
    /// divisor -> …`, `divisor` is a parameter (an outer-value reference), not a
    /// binder, so the resolution-independent [`pattern_names`](crate::pattern_names) walk's
    /// fabricated binder for it must be suppressed. The three binder-interning
    /// loops ([`pattern_locals`](crate::resolve::Resolver::pattern_locals),
    /// [`prepare_binding`](crate::resolve::Resolver::prepare_binding),
    /// [`resolve_local_let`](crate::resolve::Resolver::resolve_local_let)) skip any def whose
    /// range is in here (before the `provisional` branch, so a would-be
    /// provisional case-reference head is dropped too).
    ///
    /// Keyed on the walk's own **ident-token** ranges (`Def::from_token`), not
    /// the argument node's range — `DivBy (divisor)` has a `Paren` node range
    /// that matches no binder, so the exclusion must use the token range or the
    /// binder escapes. A never-cleared file-lifetime set: the `Resolver` is
    /// per-file, byte ranges are unique within a file, and a range excluded as a
    /// parameter is never a legitimate binder elsewhere.
    pub(super) excluded_param_ranges: HashSet<TextRange>,
    /// Member path → how many of [`Output::export_decls`](super::output::Output::export_decls) declare a member
    /// there ([`ExportDecl::member_path`](super::model::ExportDecl::member_path)): the same-file half of
    /// [`Resolver::member_declarations`](crate::resolve::Resolver::member_declarations), kept beside the list by its one writer
    /// ([`Resolver::push_export_decl`](crate::resolve::Resolver::push_export_decl)).
    pub(super) member_decl_counts: PathMap<usize>,
}

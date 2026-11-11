//! The open state of one lexical module scope ([`OpenScope`]): everything an
//! `open` declaration writes that later name resolution in the same scope
//! reads.
//!
//! F# scopes an `open` to the module (or top-level block) it appears in, so
//! this state has exactly two lifecycle events, and both replace the whole
//! value:
//!
//! - each top-level `namespace`/`module` block starts from
//!   [`OpenScope::block_start`] (an `open` in one block does not carry to the
//!   next, even to a same-named `namespace N` block — FCS-verified);
//! - a nested `module M = …` saves the enclosing scope by value on entry and
//!   restores it by value on exit ([`Resolver::nested_module`]), so an `open`
//!   in its body never leaks into the enclosing module.
//!
//! Between those events the state only grows. The fields are private and the
//! only writers are the append and set operations below, so within a scope
//! every list is append-only, the generation only rises, and every flag is
//! write-once: nothing can clear a flag except replacing the whole scope.
//! [`Resolver::open_opacity_since`] relies on that to attribute a change to
//! the one `open` that made it.
//!
//! Two close relatives live on [`Resolver`] instead, because neither follows
//! this lifecycle: [`Resolver::recursive_module_active`] is *or*-ed with the
//! nested module's own `rec` on entry rather than inherited, and
//! [`Resolver::auto_open_type_shadow_names`] starts each block from what
//! earlier same-named blocks left and, on leaving an `[<AutoOpen>]` nested
//! module, merges outward rather than restoring.
//!
//! [`Resolver`]: super::Resolver
//! [`Resolver::nested_module`]: super::Resolver::nested_module
//! [`Resolver::open_opacity_since`]: super::Resolver::open_opacity_since
//! [`Resolver::recursive_module_active`]: super::Resolver::recursive_module_active
//! [`Resolver::auto_open_type_shadow_names`]: super::Resolver::auto_open_type_shadow_names

use std::collections::HashSet;

use crate::assembly_env::AssemblyEnv;

use super::model::ItemId;
use super::state::{
    OpenGroup, ShorteningPrefix, implicit_open_groups, implicit_shortening_prefixes,
};

/// The opens in scope that could supply a **dotted-path head** the walk does
/// not otherwise see ([`OpenScope::dotted_opacity`]).
///
/// Grows only, like the rest of [`OpenScope`]: its writers are
/// [`OpenScope::make_dotted_opaque`] and
/// [`OpenScope::add_dotted_project_module`].
#[derive(Debug, Clone, Default)]
pub(super) struct DottedOpacity {
    /// An open whose dotted heads are unknowable: every
    /// [`OpenScope::opaque_value_open`] open (implied), an opened assembly
    /// module with public nested entities, a group hiding names, or an `open`
    /// of a project module that is not [`Self::project_modules`]-shaped.
    other: bool,
    /// Earlier-file project modules opened in scope whose every name is
    /// enumerable: no hidden-value marker anywhere in the subtree, no
    /// fragment in the file being walked, no referenced-assembly half under
    /// the same path. What such a module puts in scope is exactly what the
    /// earlier files' indices list for it and its `[<AutoOpen>]`
    /// descendants, so a channel that checks a head against those lists
    /// ([`Resolver::opened_project_modules_leave_head`](super::Resolver::opened_project_modules_leave_head))
    /// may resolve through them; every other channel treats them as opaque.
    project_modules: Vec<Vec<String>>,
}

impl DottedOpacity {
    /// Whether any open in scope could supply a dotted head the walk does not
    /// model — the conservative reading every channel but the type-qualified
    /// case takes.
    pub(super) fn any(&self) -> bool {
        self.other || !self.project_modules.is_empty()
    }

    /// Whether an open in scope is opaque beyond enumerable project modules.
    pub(super) fn beyond_project_modules(&self) -> bool {
        self.other
    }

    /// The enumerable project modules opened in scope.
    pub(super) fn project_modules(&self) -> &[Vec<String>] {
        &self.project_modules
    }
}

/// The open state of one lexical module scope (module docs).
#[derive(Debug, Clone)]
pub(super) struct OpenScope {
    /// See [`Self::imports`].
    imports: Vec<OpenGroup>,
    /// See [`Self::shortening_prefixes`].
    shortening_prefixes: Vec<ShorteningPrefix>,
    /// See [`Self::incomplete_prefixes`].
    incomplete_prefixes: Vec<Vec<String>>,
    /// See [`Self::explicit_prefixes`].
    explicit_prefixes: Vec<(u32, Vec<String>)>,
    /// See [`Self::module_prefixes`].
    module_prefixes: Vec<(u32, Vec<String>)>,
    /// See [`Self::assembly_prefixes`].
    assembly_prefixes: Vec<(u32, Vec<String>)>,
    /// See [`Self::generation`].
    generation: usize,
    /// See [`Self::latest_open_pos`].
    latest_open_pos: u32,
    /// See [`Self::is_pattern_suppressed`].
    pattern_suppressed_case_ids: HashSet<ItemId>,
    /// See [`Self::unmodelled_open_active`].
    unmodelled_open_active: bool,
    /// See [`Self::opaque_value_open`].
    opaque_value_open: bool,
    /// See [`Self::dotted_opacity`].
    dotted_opacity: DottedOpacity,
}

impl OpenScope {
    /// The open state at the start of a top-level block: F#'s implicit
    /// auto-opens ([`implicit_open_groups`], [`implicit_shortening_prefixes`])
    /// and nothing else.
    pub(super) fn block_start(assemblies: &AssemblyEnv) -> Self {
        OpenScope {
            imports: implicit_open_groups(assemblies),
            shortening_prefixes: implicit_shortening_prefixes(assemblies),
            incomplete_prefixes: Vec::new(),
            explicit_prefixes: Vec::new(),
            module_prefixes: Vec::new(),
            assembly_prefixes: Vec::new(),
            generation: 0,
            latest_open_pos: 0,
            pattern_suppressed_case_ids: HashSet::new(),
            unmodelled_open_active: false,
            opaque_value_open: false,
            dotted_opacity: DottedOpacity::default(),
        }
    }

    // ---- Reading groups --------------------------------------------------

    /// Opened namespace reading-groups in scope, **one [`OpenGroup`] per
    /// `open`**, in source order — F#'s implicit auto-opens first, then the
    /// file's explicit `open` decls (an `open` is in scope for the decls after
    /// it).
    ///
    /// Name resolution walks these through
    /// [`Resolver::open_reading_prefixes`](super::Resolver::open_reading_prefixes)
    /// **latest-open-first** and, within an open, in the group's priority order:
    /// the latest `open` with a *complete* reading of the name wins (F# is
    /// latest-open-wins, not ambiguity — `open Demo; open Demo.Sub; Calc` is
    /// `Demo.Sub.Calc`, reversed it is `Demo.Calc`). A short path that does not
    /// resolve directly is retried under each reading (`open System` makes
    /// `Console.WriteLine` resolve as `System.Console.WriteLine`).
    ///
    /// The first [`Resolver::implicit_import_count`](super::Resolver::implicit_import_count)
    /// groups are the implicit seed [`Self::block_start`] wrote; every later
    /// one is an explicit source `open`, appended by [`Self::push_import`].
    pub(super) fn imports(&self) -> &[OpenGroup] {
        &self.imports
    }

    /// Append one explicit `open`'s reading group to [`Self::imports`].
    pub(super) fn push_import(&mut self, group: OpenGroup) {
        self.imports.push(group);
    }

    /// Every prefix that can **shorten a later module open**, in source order
    /// (latest last): the implicit auto-opens, each explicit `open <namespace>`,
    /// and each resolved `open <project module>`. The single source-ordered
    /// sequence lets
    /// [`resolved_project_module`](super::Resolver::resolved_project_module)
    /// honour F#'s "latest open wins" *across open kinds* — `open A.Container;
    /// open B; open Shared` resolves `Shared` as `B.Shared` (the later namespace
    /// open) even though a module open (`A.Container`) preceded it, and `open
    /// Shared; open Sub` chains `Sub` to `Shared.Sub`. Kept separate from
    /// [`Self::imports`] (which holds only *namespace* prefixes, for
    /// assembly/qualified-path resolution) so adding module prefixes here does not
    /// pollute that.
    pub(super) fn shortening_prefixes(&self) -> &[ShorteningPrefix] {
        &self.shortening_prefixes
    }

    /// Append to [`Self::shortening_prefixes`].
    pub(super) fn push_shortening_prefix(&mut self, prefix: ShorteningPrefix) {
        self.shortening_prefixes.push(prefix);
    }

    /// Opened **assembly module** paths whose bare-name surface is *not provably
    /// complete* ([`AssemblyEnv::open_fold_surface`]'s residue — projection dropped
    /// a nested type, the pickle is unknowable, a member is undecodable, …).
    ///
    /// A later `open Sub` shortens through such a prefix (`Parent.Sub`), and the module
    /// it names may be exactly the one projection *dropped* — invisible to us, but bound
    /// by FCS at a **higher** priority than any root `Sub`. Resolving the root one's
    /// values would then be a wrong target, so an open that finds nothing under an
    /// incomplete prefix goes opaque instead (review round 9). Block-scoped like the
    /// rest of the scope: an incomplete module opened in one top-level block must not
    /// veto a sibling block's opens (review round 11).
    pub(super) fn incomplete_prefixes(&self) -> &[Vec<String>] {
        &self.incomplete_prefixes
    }

    /// Append to [`Self::incomplete_prefixes`].
    pub(super) fn push_incomplete_prefix(&mut self, prefix: Vec<String>) {
        self.incomplete_prefixes.push(prefix);
    }

    /// Every in-scope **explicit namespace `open`** (project or assembly — the
    /// kinds that do *not* set an opaque/unmodelled flag and so reach the same-file
    /// module-qualified classifier), as `(source position, canonical opened
    /// prefix)`. FCS-probed (r16): the dotted-head environment is one
    /// source-position-ordered latest-wins list over lexical module declarations
    /// and opens — an open declared *after* a same-file `module Pal` outranks it
    /// for the head of `Pal.Color.Red` (residual backtracking included), one
    /// declared *before* it loses. The classifier compares each lexical candidate
    /// ([`DeclKinds::module_pos`](super::state::DeclKinds::module_pos)) against
    /// these: a later open whose prefix could supply a module/namespace/assembly
    /// entity named like the head *contests* the candidate, and the classifier
    /// declines (the open-aware cross-file branches own resolution then).
    /// Implicit auto-opens precede every same-file declaration and so can never
    /// contest — they are not recorded.
    pub(super) fn explicit_prefixes(&self) -> &[(u32, Vec<String>)] {
        &self.explicit_prefixes
    }

    /// Append to [`Self::explicit_prefixes`].
    pub(super) fn push_explicit_prefix(&mut self, pos: u32, prefix: Vec<String>) {
        self.explicit_prefixes.push((pos, prefix));
    }

    /// Every in-scope **project module `open`** (the
    /// [`OpenInterpretation::Module`](super::state::OpenInterpretation::Module)
    /// arm), as `(source position, canonical module path)`. A module open
    /// imports the module's TYPES into FCS's unqualified slot too (probe
    /// M20v, codex round 8 — the opened `M.Color` class evicts an earlier
    /// value exactly like a namespace-open-supplied type), so
    /// [`head_value_slot`](super::Resolver::head_value_slot) consults these
    /// alongside [`Self::explicit_prefixes`].
    pub(super) fn module_prefixes(&self) -> &[(u32, Vec<String>)] {
        &self.module_prefixes
    }

    /// Append to [`Self::module_prefixes`].
    pub(super) fn push_module_prefix(&mut self, pos: u32, module: Vec<String>) {
        self.module_prefixes.push((pos, module));
    }

    /// Every open through which an **assembly namespace's types are reachable**,
    /// as `(source position, canonical namespace path)` — every
    /// [`OpenInterpretation::Reading`](super::state::OpenInterpretation::Reading)
    /// the open produced, captured *before* the `project_readings_only` filter
    /// that keeps a direct project-module / assembly-namespace merge out of
    /// [`Self::explicit_prefixes`]. This is exactly where a referenced-assembly
    /// type occupies FCS's unqualified slot and can evict a same-named local
    /// value: a plain `open System` (A1), a project-namespace open, **and** a
    /// *direct* `open Demo` where `Demo` is a project module merged with the
    /// assembly namespace `Demo` (codex round 2 — FCS binds `Demo.Calc` there).
    /// A module *alias* (`open Alias`) produces no reading —
    /// `open_interpretations` marks its namespaces unreachable — so it is
    /// correctly absent (codex round 1). Consulted by
    /// [`head_value_slot`](super::Resolver::head_value_slot) for the
    /// assembly-eviction check only (`docs/head-slot-assembly-eviction-plan.md`);
    /// the *project* slot checks keep using `explicit`/`module` prefixes.
    pub(super) fn assembly_prefixes(&self) -> &[(u32, Vec<String>)] {
        &self.assembly_prefixes
    }

    /// Append to [`Self::assembly_prefixes`].
    pub(super) fn push_assembly_prefix(&mut self, pos: u32, namespace: Vec<String>) {
        self.assembly_prefixes.push((pos, namespace));
    }

    // ---- Generation ------------------------------------------------------

    /// Monotonic generation, raised each time an `open` brings in names the
    /// walk cannot enumerate (an `open M` of a project module with
    /// **unmodelled value-namespace members**, an opaque open, a fold group
    /// with residue). Each [`ScopeEntry`](super::state::ScopeEntry) is tagged
    /// with the generation at its creation
    /// ([`ScopeEntry::generation`](super::state::ScopeEntry::generation));
    /// [`lookup`](super::Resolver::lookup) treats an entry whose generation is
    /// older than this as *shadowed* by that later open — we cannot enumerate
    /// the new open's union cases / exception constructors / active patterns,
    /// so any of them might shadow an earlier name, and conservatively
    /// dropping every earlier name (F#: the latest open wins) is sound. A
    /// *fully-modelled* open (only `let`s) does not raise it, so earlier opens
    /// stay resolvable.
    pub(super) fn generation(&self) -> usize {
        self.generation
    }

    /// Raise one generation barrier: every entry already in scope goes stale
    /// ([`Self::generation`]).
    pub(super) fn raise_barrier(&mut self) {
        self.raise_barriers(1);
    }

    /// Raise `barriers` generation barriers at once — a replayed fold
    /// re-raising the barriers it recorded.
    pub(super) fn raise_barriers(&mut self, barriers: usize) {
        self.generation += barriers;
    }

    // ---- Opacity flags ---------------------------------------------------

    /// `true` once an open that *could* bring an unqualified **value** we do not
    /// model is in scope: a plain `open <assembly module/class>` (its values /
    /// statics are unmodelled here), or an `open type T` whose target we could not
    /// resolve to an assembly handle or that is project-shadowed. Such an open
    /// might provide — or shadow — a bare name with a value we cannot enumerate,
    /// so while one is in scope [`lookup`](super::Resolver::lookup) skips every
    /// *opened* ([`ScopeEntry::from_open`](super::state::ScopeEntry::from_open))
    /// entry — resolving a modelled opened name could otherwise pick a target
    /// the opaque open shadows (correctness over availability). It also implies
    /// [`Self::dotted_opacity`] (an unknown bare value head could likewise be a
    /// dotted-path head we cannot resolve), so the dotted-path gate in
    /// [`resolve_long_ident`](super::Resolver::resolve_long_ident) checks both.
    /// A plain `open <namespace>` brings only *types*, no unqualified values, so
    /// it does **not** set this.
    ///
    /// Set only by [`Self::make_values_opaque`] (and
    /// [`Self::make_wholly_opaque`]), which raise a generation barrier with it.
    pub(super) fn opaque_value_open(&self) -> bool {
        self.opaque_value_open
    }

    /// `true` once an open that brings *type members* into unqualified scope is
    /// in scope — `open type T`, or `open <module>`/`open <type>` whose path is
    /// itself an assembly type. Its **static members** are modelled (one *opened*
    /// [`ScopeEntry`](super::state::ScopeEntry) per static name was pushed into
    /// the frame by [`Resolver::open_type_statics`](super::Resolver::open_type_statics)),
    /// but its **nested types** are not — so a *qualified* path could still
    /// come from it, and while one is in scope `resolve_long_ident` cannot
    /// soundly resolve a qualified path via the *other* (namespace) opens and
    /// defers instead. Also set for an `open type` whose target we could not
    /// resolve to a handle (an in-project type, an exotic type form): nothing is
    /// modelled then, so every open-based resolution stays conservative.
    pub(super) fn unmodelled_open_active(&self) -> bool {
        self.unmodelled_open_active
    }

    /// Which opens in scope could supply a *dotted-path head* we do not model
    /// (see [`DottedOpacity`]). While [`DottedOpacity::any`] holds, the
    /// dotted-path gate in [`resolve_long_ident`](super::Resolver::resolve_long_ident)
    /// leaves such a head unresolved rather than risk routing it to a
    /// colliding cross-file / referenced-assembly path (correctness over
    /// availability). Unlike [`Self::opaque_value_open`] it does **not** make
    /// [`lookup`](super::Resolver::lookup) skip opened entries — an opened
    /// module's bare values are modelled and must resolve.
    pub(super) fn dotted_opacity(&self) -> &DottedOpacity {
        &self.dotted_opacity
    }

    /// An open brought values we cannot enumerate: set
    /// [`Self::opaque_value_open`], and raise a barrier so every earlier
    /// entry, which one of those values may shadow, goes stale.
    pub(super) fn make_values_opaque(&mut self) {
        self.opaque_value_open = true;
        self.raise_barrier();
    }

    /// An open brought type members or nested types we do not model: set
    /// [`Self::unmodelled_open_active`].
    pub(super) fn make_unmodelled(&mut self) {
        self.unmodelled_open_active = true;
    }

    /// An open may have brought anything at all: both
    /// [`Self::make_values_opaque`] and [`Self::make_unmodelled`].
    pub(super) fn make_wholly_opaque(&mut self) {
        self.make_values_opaque();
        self.make_unmodelled();
    }

    /// An open could supply a dotted head beyond enumerable project modules
    /// ([`DottedOpacity::beyond_project_modules`]).
    pub(super) fn make_dotted_opaque(&mut self) {
        self.dotted_opacity.other = true;
    }

    /// An open brought an enumerable earlier-file project module
    /// ([`DottedOpacity::project_modules`]).
    pub(super) fn add_dotted_project_module(&mut self, module: Vec<String>) {
        self.dotted_opacity.project_modules.push(module);
    }

    // ---- Position and pattern suppression --------------------------------

    /// The start offset of the latest `open` (of any kind) in scope, `0` when
    /// there is none. The positional contest of the attribute and type
    /// resolutions: F# is latest-wins across bindings and opens alike, so an
    /// in-file type commit whose definition *precedes* an open must defer —
    /// the open could supply the name at higher priority (`type
    /// ObsoleteAttribute … ; open System; [<Obsolete>]` binds
    /// `System.ObsoleteAttribute` in FCS).
    pub(super) fn latest_open_pos(&self) -> u32 {
        self.latest_open_pos
    }

    /// Record an `open` of any kind at `pos` ([`Self::latest_open_pos`]).
    pub(super) fn note_open_at(&mut self, pos: u32) {
        self.latest_open_pos = self.latest_open_pos.max(pos);
    }

    /// Whether the opened cross-file case `id` must **not** be trusted in
    /// pattern position — an exported case of a *hidden* module (one whose
    /// `open` also brings unenumerable constructors, e.g. an active pattern,
    /// that could shadow it; see
    /// [`open_module_values`](super::Resolver::open_module_values)). It still
    /// resolves as a value in expression position; only
    /// [`case_reference`](super::Resolver::case_reference) consults this to
    /// defer the pattern use.
    pub(super) fn is_pattern_suppressed(&self, id: ItemId) -> bool {
        self.pattern_suppressed_case_ids.contains(&id)
    }

    /// Mark `ids` as [`Self::is_pattern_suppressed`].
    pub(super) fn suppress_pattern_cases(&mut self, ids: impl IntoIterator<Item = ItemId>) {
        self.pattern_suppressed_case_ids.extend(ids);
    }
}

//! Resolver state: the mutable walk context ([`Resolver`]) and its supporting
//! scope types ([`ScopeEntry`], [`Frame`], [`AssemblyPath`], [`OpenInterpretation`]),
//! plus the implicit auto-open seed ([`implicit_open_namespaces`]). The
//! behaviour — the `impl Resolver` methods that drive the walk — lives in the
//! parent [`resolve`](super) module; the fields here are `pub(super)` so those
//! impl blocks (anywhere in the `resolve` module subtree) can reach them.

use crate::hash::HashMap;
use crate::recovery::SyntaxRecovery;

use borzoi_cst::syntax::TextRange;

use crate::assembly_env::{AssemblyEnv, EntityHandle};
use crate::def::DefId;

use super::indexes::Indexes;
use super::lexical::Lexical;
use super::model::{
    DeclineCause, DeclineSite, DeclineTier, DeferredReason, ItemId, ProjectItems, Resolution,
    SlotClass,
};
use super::open_scope::{InternedPrefix, OpenScope};
use super::output::Output;
use super::prescan::Prescan;
use super::windows::Windows;
use crate::names::{Name, NameKey, NameTable, PathId};

/// The opens in scope that could supply a **dotted-path head** the walk does
/// not otherwise see ([`OpenOpacityState::dotted`](crate::resolve::open_scope::OpenOpacityState::dotted)).
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct DottedOpacity {
    /// An open whose dotted heads are unknowable: an opened assembly
    /// module with public nested entities, a group hiding names, or an `open`
    /// of a project module that is not [`Self::project_modules`]-shaped.
    other: bool,
    /// Earlier-file project modules opened in scope whose every name is
    /// enumerable: no hidden-value marker anywhere in the subtree, no
    /// fragment in the file being walked, no referenced-assembly half under
    /// the same path. What such a module puts in scope is exactly what the
    /// earlier files' indices list for it and its `[<AutoOpen>]`
    /// descendants, so a channel that checks a head against those lists
    /// ([`Resolver::opened_project_modules_leave_head`]) may resolve through
    /// them; every other channel treats them as opaque.
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

    pub(super) fn set_opaque(&mut self) {
        self.other = true;
    }

    pub(super) fn add_project_module(&mut self, module: Vec<String>) {
        self.project_modules.push(module);
    }
}

/// A binding visible in a scope frame. `name` is `idText`-normalised,
/// interned in the env's name table; later entries in a frame shadow earlier
/// ones (position-ordered shadowing, D4).
#[derive(Debug, Clone)]
pub(super) struct ScopeEntry {
    pub(super) name: Name,
    /// `name`'s text, which the frame's string-keyed reference index reads
    /// under `interning-reference` in a debug build.
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    text: String,
    pub(super) resolution: Resolution,
    /// `true` if an `open` brought this name into scope — an opened type's static
    /// member (`open type T`), or (a later slice) an opened module's value
    /// (`open M`) — rather than a source binding (`let`, a union/exception case, a
    /// parameter). Two consequences, both keeping opens sound:
    /// - while an [`opaque_value_open`](crate::resolve::open_scope::OpenOpacityState::value) is in scope,
    ///   [`lookup`](Resolver::lookup) skips it — an open whose contents we cannot
    ///   enumerate might shadow it with a value we do not model, so resolving the
    ///   modelled opened name would risk a wrong target (correctness over
    ///   availability);
    /// - it is dropped when a top-level container frame is stored back
    ///   ([`resolve_file`](super::resolve_file)), so an `open` in one block does not leak into a later
    ///   same-named block (F#: opens are scoped to one block; values merge).
    pub(super) from_open: bool,
    /// The [`open_generation`](crate::resolve::open_scope::OpenScope::open_generation) when this entry was
    /// created. [`lookup`](Resolver::lookup) treats an entry as *stale* (skips
    /// it) once a later `open` with names we cannot list bumped the generation,
    /// conservatively shadowing every earlier name — **source bindings
    /// included** (codex round 22): FCS's environment is latest-wins across
    /// bindings and opens alike, so a hidden opened name can shadow an earlier
    /// module-level `let` exactly as it shadows an earlier open's entry. A
    /// binding *after* the bump carries the new generation and wins, and the
    /// nested-module save/restore un-stales an outer binding when a
    /// block-scoped open's scope ends.
    pub(super) generation: usize,
    /// `true` if this entry is in the **constructor namespace only** — a
    /// value-shadowed cross-file case brought in by [`open_module_values`](Resolver::open_module_values) so a
    /// *pattern* head resolves to it, even though the value index at its path holds
    /// a same-named `let`. [`lookup`](Resolver::lookup) (expression position) skips
    /// it; [`case_reference`](Resolver::case_reference) (pattern position) includes
    /// it. A union/exception case that is *not* value-shadowed is an ordinary entry
    /// (a value too — `let x = Red`), so this is `false` for it.
    pub(super) pattern_only: bool,
    /// For a [`from_open`](Self::from_open) entry, the source offset of the
    /// `open` that brought it into scope (`0` for an implicit auto-open, which
    /// precedes every declaration). FCS enters an opened value in its
    /// unqualified slot **at the open's position** — an `open M` after a
    /// same-named type re-takes the slot for `M.Color`, one before it loses
    /// (probes M20p/M20q) — so the head-slot ordering
    /// ([`head_value_slot`](Resolver::head_value_slot)) must compare this, not
    /// the value's definition position. Unused (`0`) for a source binding,
    /// whose position is its definition's.
    pub(super) open_pos: u32,
    /// `true` when an `open` of an assembly container brought this name in as a
    /// constructor **case** — a union case, exception constructor, or
    /// active-pattern tag ([`OpenFoldName::is_case`](crate::OpenFoldName::is_case)).
    /// [`case_reference`](Resolver::case_reference) accepts such an entry where
    /// it skips plain values; project-side cases are instead classified through
    /// their [`DefKind`](crate::DefKind) (an opened assembly case has no def to classify).
    pub(super) opened_case: bool,
    /// For an opened **assembly active-pattern tag**
    /// ([`OpenFoldName::ap_shape`](crate::OpenFoldName::ap_shape)), the recognizer
    /// [`ActivePatternShape`] demangled from its `|A|B|` IL name; `None` for
    /// every other entry. An assembly AP tag resolves to
    /// [`Resolution::Deferred`], which carries no identity to key a shape on, so
    /// the applied-head split
    /// ([`applied_active_pattern_case`](Resolver::applied_active_pattern_case))
    /// reads it here instead — `docs/export-decl-model-plan.md` Stage 3b.
    pub(super) opened_ap_shape: Option<ActivePatternShape>,
    /// `true` when this **value** entry may be an FCS *constant pattern* — a
    /// `[<Literal>]` (or `decimal`-literal) value. Unlike a plain value, a
    /// literal contests the pattern (constructor) namespace: FCS's `ePatItems`
    /// holds exactly the constructor cases and the literal values, latest-wins,
    /// so `open A; [<Literal>] let Even = 7; match n with Even` binds the
    /// literal where the earlier opened case would otherwise win. Source side
    /// this is attribute-**presence** on the module-level `let` (attribute
    /// *identity* is unverifiable — a shadowing `LiteralAttribute` alias is
    /// undetectable — so any attributed value is maybe-literal, while an
    /// unattributed one provably is not; an attribute on a *local* `let` does
    /// not even parse). Assembly side it is the CLI `Literal` flag / the Q17
    /// `decimal` rule ([`OpenFoldName::constant_pattern`](crate::OpenFoldName::constant_pattern)).
    /// A **bare** pattern-position scan meeting such a value before a case
    /// defers ([`case_reference`](Resolver::case_reference)); an *applied* head
    /// is exempt — FS3191 makes an applied literal pattern illegal, so on a
    /// clean program an applied head is never the literal. `false` for cases,
    /// parameters, and locals.
    pub(super) maybe_constant_pattern: bool,
    /// For an [opened](Self::from_open) entry that **declines** — a
    /// [`Resolution::Deferred`] the open put in scope to shadow by position
    /// while naming nothing — why it declines. A bare use that lands on it
    /// records this as its decline site
    /// ([`ResolvedFile::decline_site`](super::model::ResolvedFile::decline_site)).
    /// `None` for every entry that commits.
    pub(super) decline: Option<DeclineCause>,
    /// `Some` for a **contributor mark**: a name a contributor the walk does
    /// not model may have put in the environment here
    /// ([`Resolver::push_contribution`](super::contributors)). It declines,
    /// and shadows every earlier entry of the name by position, as FCS's
    /// latest-write-wins environment would let the contributor's binding do.
    /// [`MarkReach`] says which namespaces it shadows in. Never stored back
    /// into a top-level container's frame: a mark records a position in one
    /// block's walk.
    pub(super) mark: Option<MarkReach>,
    /// `true` for a source entry standing for a name that may be a union case
    /// the walk cannot decide: `type T = X`'s case `X`, when FCS may read the
    /// abbreviation as a union ([`Resolver::shadow_abbreviation_case`]). It
    /// declines in expression position like any `Deferred` entry, and in
    /// pattern position too ([`Resolver::case_reference`]), where a plain
    /// value would be skipped: the case would shadow an earlier one there.
    pub(super) undecided_case: bool,
}

/// Which namespaces a contributor mark ([`ScopeEntry::mark`]) shadows in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkReach {
    /// Expression and pattern position: the contributor may supply a union
    /// or exception case, an active-pattern case or a literal.
    Both,
    /// Expression position, bare or as a dotted head: the contributor
    /// supplies a value that cannot be a literal, or a constructible type,
    /// neither of which enters FCS's pattern table.
    Expression,
}

impl ScopeEntry {
    /// A name introduced by a *source binding* — a `let` value/parameter, a
    /// union/exception case, an active-pattern case, a `match`/lambda local —
    /// stamped with the current [`open_generation`](crate::resolve::open_scope::OpenScope::open_generation)
    /// so a later residue-bearing `open` can shadow it (see
    /// [`Self::generation`]).
    pub(super) fn binding(
        names: &NameTable,
        name: &str,
        resolution: Resolution,
        generation: usize,
    ) -> Self {
        Self::binding_named(names, names.logical(name), resolution, generation)
    }

    /// [`Self::binding`] of a name already interned: a [`Def`](crate::def::Def)'s.
    pub(super) fn binding_named(
        names: &NameTable,
        name: Name,
        resolution: Resolution,
        generation: usize,
    ) -> Self {
        #[cfg(not(all(feature = "interning-reference", debug_assertions)))]
        let _ = names;
        ScopeEntry {
            name,
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: names.text(name).to_string(),
            resolution,
            from_open: false,
            generation,
            pattern_only: false,
            open_pos: 0,
            opened_case: false,
            opened_ap_shape: None,
            maybe_constant_pattern: false,
            decline: None,
            mark: None,
            undecided_case: false,
        }
    }

    /// A name brought into scope by an `open` (see [`Self::from_open`]), tagged
    /// with the current [`open_generation`](crate::resolve::open_scope::OpenScope::open_generation) so a later
    /// unmodelled open can shadow it (see [`Self::generation`]), and with the
    /// open's source offset (see [`Self::open_pos`]).
    ///
    /// `resolution` is what it commits; an opened entry that declines is made
    /// with [`Self::opened_declined`], which names why.
    pub(super) fn opened(
        names: &NameTable,
        name: &str,
        resolution: Resolution,
        generation: usize,
        open_pos: u32,
    ) -> Self {
        debug_assert!(
            !matches!(resolution, Resolution::Deferred(_)),
            "opened entry {name:?} declines without naming why: use `opened_declined`"
        );
        ScopeEntry::opened_unchecked(names, name, resolution, generation, open_pos)
    }

    /// An opened entry that **declines** for `cause`: in scope, shadowing by
    /// position, naming nothing.
    pub(super) fn opened_declined(
        names: &NameTable,
        name: &str,
        cause: DeclineCause,
        generation: usize,
        open_pos: u32,
    ) -> Self {
        let mut entry = ScopeEntry::opened_unchecked(
            names,
            name,
            Resolution::Deferred(DeferredReason::UnboundName),
            generation,
            open_pos,
        );
        entry.decline = Some(cause);
        entry
    }

    /// A contributor mark ([`Self::mark`]) for `name`, entered at `pos` — the
    /// position FCS adds the contributor's names to the environment, which the
    /// head-slot ordering compares like an `open`'s.
    pub(super) fn contributor_mark(
        names: &NameTable,
        name: &str,
        reach: MarkReach,
        generation: usize,
        pos: u32,
    ) -> Self {
        ScopeEntry {
            name: names.logical(name),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: name.to_string(),
            resolution: Resolution::Deferred(DeferredReason::UnboundName),
            from_open: false,
            generation,
            pattern_only: false,
            open_pos: pos,
            opened_case: false,
            opened_ap_shape: None,
            maybe_constant_pattern: false,
            decline: Some(DeclineCause::UnmodelledContributor),
            mark: Some(reach),
            undecided_case: false,
        }
    }

    /// A source entry for a name that may be a union case the walk cannot
    /// decide ([`Self::undecided_case`]): in scope, shadowing by position in
    /// both namespaces, naming nothing.
    pub(super) fn undecided_case(names: &NameTable, name: &str, generation: usize) -> Self {
        ScopeEntry {
            undecided_case: true,
            ..ScopeEntry::binding(
                names,
                name,
                Resolution::Deferred(DeferredReason::UnboundName),
                generation,
            )
        }
    }

    fn opened_unchecked(
        names: &NameTable,
        name: &str,
        resolution: Resolution,
        generation: usize,
        open_pos: u32,
    ) -> Self {
        ScopeEntry {
            name: names.logical(name),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: name.to_string(),
            resolution,
            from_open: true,
            generation,
            pattern_only: false,
            open_pos,
            opened_case: false,
            opened_ap_shape: None,
            maybe_constant_pattern: false,
            decline: None,
            mark: None,
            undecided_case: false,
        }
    }

    /// A constructor-namespace-only opened entry — a value-shadowed cross-file
    /// case (see [`Self::pattern_only`]).
    pub(super) fn opened_pattern_only(
        names: &NameTable,
        name: &str,
        resolution: Resolution,
        generation: usize,
        open_pos: u32,
    ) -> Self {
        debug_assert!(
            !matches!(resolution, Resolution::Deferred(_)),
            "opened entry {name:?} declines without naming why: use `opened_declined`"
        );
        ScopeEntry {
            name: names.logical(name),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            text: name.to_string(),
            resolution,
            from_open: true,
            generation,
            pattern_only: true,
            open_pos,
            opened_case: false,
            opened_ap_shape: None,
            maybe_constant_pattern: false,
            decline: None,
            mark: None,
            undecided_case: false,
        }
    }
}

/// What one fold wrote into the module frame, kept so the same contents can
/// be written again at a later position: each entry with the number of
/// generation barriers the fold had raised before writing it, and the number
/// it raised in all ([`Lexical::own_namespace_fold`](crate::resolve::lexical::Lexical::own_namespace_fold)).
#[derive(Debug, Clone, Default)]
pub(super) struct RecordedFold {
    pub(super) entries: Vec<(usize, ScopeEntry)>,
    pub(super) barriers: usize,
}

impl RecordedFold {
    /// This fold followed by `next`, as one: `next`'s entries come after every
    /// barrier this one raised.
    pub(super) fn then(mut self, next: RecordedFold) -> RecordedFold {
        let before = self.barriers;
        self.entries.extend(
            next.entries
                .into_iter()
                .map(|(barriers, entry)| (before + barriers, entry)),
        );
        self.barriers += next.barriers;
        self
    }
}

/// One in-file type definition's position and [`SlotClass`]
/// ([`Indexes::type_slot_history`](crate::resolve::indexes::Indexes::type_slot_history)).
pub(super) type TypeSlotDef = (u32, SlotClass);

/// One lexical scope: a flat, source-ordered list of its own bindings. The
/// parent-linked scope *tree* of D4 is realised implicitly as the live stack of
/// frames during the walk — at any use, the frame stack is exactly that use's
/// root-to-node path through the tree.
///
/// A lookup reads the entries of one name, latest first ([`Frame::named_rev`]),
/// so each frame indexes its entries by name: a scan of the whole frame would
/// make a file of `n` bindings that each use a name cost `n²`. The entries
/// are private so that every write goes through a method that keeps the
/// index.
#[derive(Debug, Default)]
pub(super) struct Frame {
    entries: Vec<ScopeEntry>,
    /// Each name's positions in `entries`, ascending.
    by_name: HashMap<Name, Vec<usize>>,
    /// The string-keyed index `by_name` replaced, held to it by every
    /// [`Self::named_rev`] under `interning-reference` in a debug build.
    #[cfg(all(feature = "interning-reference", debug_assertions))]
    by_text: HashMap<String, Vec<usize>>,
}

impl Frame {
    pub(super) fn new(entries: Vec<ScopeEntry>) -> Frame {
        let mut frame = Frame {
            entries,
            by_name: HashMap::default(),
            #[cfg(all(feature = "interning-reference", debug_assertions))]
            by_text: HashMap::default(),
        };
        frame.reindex();
        frame
    }

    /// The entries, in source order.
    pub(super) fn entries(&self) -> &[ScopeEntry] {
        &self.entries
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The entries named `name`, latest first: the order in which the
    /// latest-wins scan over [`Self::entries`] would meet them.
    pub(super) fn named_rev<'a>(
        &'a self,
        name: NameKey<'_>,
    ) -> impl Iterator<Item = &'a ScopeEntry> {
        let positions = name.id.and_then(|n| self.by_name.get(&n));
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        assert_eq!(
            positions,
            self.by_text.get(name.text),
            "a frame's name index disagrees with its reference at {:?}",
            name.text
        );
        positions
            .into_iter()
            .flat_map(|positions| positions.iter().rev())
            .map(|&i| &self.entries[i])
    }

    pub(super) fn push(&mut self, entry: ScopeEntry) {
        self.index(self.entries.len(), &entry);
        self.entries.push(entry);
    }

    /// File `entry`, at position `i`, under its name.
    fn index(&mut self, i: usize, entry: &ScopeEntry) {
        self.by_name.entry(entry.name).or_default().push(i);
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        self.by_text.entry(entry.text.clone()).or_default().push(i);
    }

    pub(super) fn extend(&mut self, entries: impl IntoIterator<Item = ScopeEntry>) {
        for entry in entries {
            self.push(entry);
        }
    }

    /// Move the last `k` entries from `start` on to sit at `start`, ahead of
    /// the rest (`entries[start..].rotate_right(k)`).
    pub(super) fn rotate_tail_right(&mut self, start: usize, k: usize) {
        self.entries[start..].rotate_right(k);
        self.reindex();
    }

    pub(super) fn retain(&mut self, keep: impl FnMut(&ScopeEntry) -> bool) {
        self.entries.retain(keep);
        self.reindex();
    }

    /// Stamp every entry with `generation`.
    pub(super) fn set_generation(&mut self, generation: usize) {
        for entry in &mut self.entries {
            entry.generation = generation;
        }
    }

    fn reindex(&mut self) {
        self.by_name.clear();
        #[cfg(all(feature = "interning-reference", debug_assertions))]
        self.by_text.clear();
        let entries = std::mem::take(&mut self.entries);
        for (i, entry) in entries.iter().enumerate() {
            self.index(i, entry);
        }
        self.entries = entries;
    }
}

/// The outcome of probing a dotted path against the referenced assemblies
/// (see [`Resolver::assembly_path_records`]). Distinguishing the two failure
/// modes is load-bearing: a path F# resolves *within the project* must defer
/// (never fall through to an opened assembly type), whereas a genuine
/// non-match may be retried under the next open.
///
/// Generic in the reading's `payload` so the one precedence walk
/// ([`Resolver::resolve_assembly_path_tiered`]) serves both callers: the
/// value/member path carries `Vec<(TextRange, Resolution)>` (the records to
/// apply), while the *type* path carries **index-keyed**
/// `Vec<(usize, Resolution)>` so a path can be resolved without source
/// tokens — the synthesised `…Attribute` attribute candidate has none
/// (`docs/extension-scope-enumeration-plan.md` §2(d)).
pub(super) enum AssemblyPath<R> {
    /// Rooted at a public assembly type — the reading's `payload`.
    ///
    /// `owns_path` is `true` when the reading resolves — and so *captures* — the
    /// **whole** path: every segment consumed as a nested type, a trailing unique
    /// static member, *or* a trailing static member the rooting type owns but
    /// cannot uniquely select (an overload set, recorded [`Resolution::Deferred`]).
    /// It is `false` for a *partial* reading — a rooting type was found but a
    /// later segment names nothing on it, so the tail is genuinely absent. The
    /// distinction is load-bearing in [`Resolver::resolve_assembly_path_tiered`]:
    /// an owning reading wins its precedence tier outright (a lower tier must not
    /// override it, even though the member itself deferred), whereas a partial one
    /// is only a fallback that a lower tier resolving the whole path supersedes.
    Resolved { payload: R, owns_path: bool },
    /// **Something holds this name here**, and it may satisfy the whole path
    /// invisibly — so the walk decides at this reading and the as-written root
    /// is vetoed preemptively (see
    /// [`Resolver::resolve_assembly_path_tiered`]). Several occupants reach
    /// this verdict: a project entity binding the path, an alias whose target
    /// cannot be chased, an alias that owns its own tail, a case-pattern head
    /// bound by a non-union. They are indistinguishable *to the resolver* —
    /// each stops the walk identically — so the payload is what tells them
    /// apart for the census, and naming it is what a decline site cannot be
    /// constructed without.
    ///
    /// A project module that is only a *proper* prefix does **not** land here —
    /// it merges with the assembly namespace and falls through (see
    /// [`Resolver::assembly_path_records`]).
    Occupied(DeclineCause),
    /// The path is rooted at the **current module's own name**, used as a
    /// self-qualifier from within that module (`List.fold` inside
    /// `module List`), and nothing *else* project-side shadows it. FCS never
    /// binds a module's own name as a self-qualifier — `M.x` inside `M` is
    /// FS0039 — so the name falls through to whatever an `open` / implicit
    /// `[<AutoOpen>]` supplies (bare `List` → `Microsoft.FSharp.Collections.List`).
    ///
    /// Distinct from [`Self::Occupied`] in exactly one way: it does **not**
    /// trip the preemptive as-written-root veto in
    /// [`Resolver::resolve_assembly_path_tiered`] — a higher-priority open (the
    /// auto-open included) must be walked first, unlike a genuine project-bound
    /// head an open cannot redirect. Reached in priority order — i.e. at the
    /// **root** tier, once every open has declined — it defers like
    /// `Occupied` (the module still shadows a same-named *root* namespace,
    /// so the as-written assembly reading must not resolve: `Demo.Calc` inside
    /// `module Demo` beside a referenced-assembly `Demo.Calc` is FS0039, not the
    /// assembly type).
    SelfModuleShadowed,
    /// This reading lands on a **generic type-abbreviation child** of a module
    /// (missed by the arity-0 `nested` walk): the name binds in the module, but
    /// the abbreviation target is unmodelled and FCS's ownership is
    /// target-sensitive (a record/union target falls through, a class target
    /// keeps the module), so this reading can neither resolve nor confidently
    /// disown the path — it **defers**.
    ///
    /// Unlike [`Self::Occupied`] it is **tier-local**: it does *not* trip
    /// the preemptive as-written-root veto in
    /// [`Resolver::resolve_assembly_path_tiered`], because it is a lower-priority
    /// *assembly* reading, not a lexical project-bound head — a higher-priority
    /// `open` that resolves the whole path must still win over it (codex review
    /// 4). Reached in priority order, it defers like `Occupied`.
    AbbreviationOpaque,
    /// The reading's rooting top-level FQN is exported by **more than one
    /// loaded DLL**: FCS merges same-FQN roots across references and binds the
    /// latest *accessible* one (fsi-verified, both reference orders), which
    /// sema does not model — `lookup_type` is first-wins — so this reading can
    /// neither commit a target nor confidently disown the path: it **defers**.
    ///
    /// Tier-local like [`Self::AbbreviationOpaque`], and for the same reason:
    /// it is an *assembly* reading, not a lexical project-bound head, so it
    /// must not trip the preemptive as-written-root veto in
    /// [`Resolver::resolve_assembly_path_tiered`] — a higher-priority `open`
    /// whose rooting is uncontested still wins over a contested root-tier
    /// reading (codex review on the contested-FQN guard). Reached in priority
    /// order, it defers like [`Self::Occupied`]: FCS binds one of the
    /// contestants at that tier, so no lower tier may re-root the path.
    ///
    /// Raised only when some contestant could actually *supply the tail*
    /// ([`Self::may_own_path`]). A contested rooting whose tail is provably
    /// absent from **every** contestant is an ordinary partial reading, not
    /// this: FCS falls through such a rooting to a lower reading that completes
    /// the path (fsi-verified — see the walk's contested combinator).
    ContestedRooting,
    /// Not an assembly path at all.
    NoMatch,
}

impl<T> AssemblyPath<T> {
    /// Whether this reading could **capture the whole path**, so a lower-priority
    /// reading must not be applied over it — every outcome except the two that
    /// prove the path unreachable through this reading: a genuinely-absent tail
    /// (a *partial* [`Self::Resolved`]) and [`Self::NoMatch`].
    ///
    /// The predicate a *merged* rooting is judged by: FCS falls through a
    /// contested rooting exactly when no contestant supplies the tail, so the
    /// walk asks this of each contestant rather than of the first-wins slot
    /// alone (codex review round 2 on the contested-FQN guard).
    pub(super) fn may_own_path(&self) -> bool {
        !matches!(
            self,
            AssemblyPath::NoMatch
                | AssemblyPath::Resolved {
                    owns_path: false,
                    ..
                }
        )
    }

    /// The census cause this reading declines with, or `None` for the two
    /// variants that are not declines ([`Self::Resolved`] — partial or owning —
    /// and [`Self::NoMatch`], neither of which stops the walk).
    ///
    /// Exhaustive rather than a wildcard, so a reading variant added later
    /// cannot silently join the census without a cause of its own.
    pub(super) fn decline_cause(&self) -> Option<DeclineCause> {
        match self {
            AssemblyPath::Occupied(cause) => Some(*cause),
            AssemblyPath::SelfModuleShadowed => Some(DeclineCause::SelfModuleShadowed),
            AssemblyPath::AbbreviationOpaque => Some(DeclineCause::AbbreviationOpaque),
            AssemblyPath::ContestedRooting => Some(DeclineCause::ContestedRooting),
            AssemblyPath::Resolved { .. } | AssemblyPath::NoMatch => None,
        }
    }
}

/// The token-free decision for one reading of a **type** path — the payload
/// [`AssemblyPath::Resolved`] carries for `Resolver::assembly_type_path_core`.
/// The recording shell (`Resolver::resolve_type_path`) maps each `idx_recs`
/// entry back to its source token's range and applies it; the attribute
/// resolution (EX-3 §2(d)) reads `leaf` — the type the whole path names — to
/// key its verdict.
pub(super) struct TypePathReading {
    /// `(segment index, resolution)` — records keyed by index into the source
    /// segment names (a namespace-qualifier or unresolvable-tail segment is
    /// [`Resolution::Deferred`]; the rooting and each nested type an
    /// [`Resolution::Entity`]).
    pub(super) idx_recs: Vec<(usize, Resolution)>,
    /// The concrete leaf type the **whole** path names — `Some` iff the
    /// reading [`owns_path`](AssemblyPath::Resolved); `None` for a partial
    /// reading whose tail deferred (nothing whole-path to inspect).
    pub(super) leaf: Option<EntityHandle>,
}

/// The precedence tier a *project* cross-file `Type.Case` reading won at
/// (`Resolver::cross_file_type_case`) — the one distinction
/// `Resolver::record_qualified_case_pattern` needs to order that reading
/// against a referenced-assembly entity occupying the same head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaseTier {
    /// A lexically in-scope **module alias** as the head. Definitive: the alias
    /// fixes what the head names, so it out-ranks even an `open`ed assembly
    /// entity of the name.
    Alias,
    /// An `open`'s shortening prefix, the enclosing namespace/module nesting, or
    /// the path as written. These yield to an assembly entity in scope through
    /// an `open`: an `open` out-ranks the lower two outright, and two `open`
    /// tiers cannot be ordered against each other here — the project
    /// (`OpenScope::open_shortening_prefixes`) and assembly
    /// (`Resolver::open_reading_prefixes`) prefix lists are built from the same
    /// opens but are neither shared nor index-aligned.
    Namespace,
}

/// The per-prefix shadow verdict a caller of
/// [`Resolver::resolve_assembly_path_tiered`](super::Resolver) supplies for
/// each tier the walk visits — a named strength instead of two positionally
/// swappable boolean closures (wrong wiring is now a type error, and the
/// disabled case reads as what it is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShadowVeto {
    /// Nothing at this prefix can shadow the name.
    None,
    /// Something at this prefix could shadow the name: an exact, name-keyed
    /// match (an in-scope assembly `[<AutoOpen>]` module with an accessible
    /// member of exactly this name), an unmodelled position that the project
    /// declares this name somewhere to fill (a project `[<AutoOpen>]` module,
    /// whose types sema does not enumerate), or a coarse, name-blind one (a
    /// namespace whose assembly's abbreviations are unknowable). Either way it
    /// vetoes
    /// the reading pre-emptively — including a same-tier real match, since
    /// FCS-probed, an auto-open module's contents outrank the same
    /// namespace's own direct members, and a visible entity is no evidence
    /// that an invisible same-named one is absent.
    ///
    /// The payload names *which* of those it was, for the decline census
    /// ([`ResolvedFile::decline_site`](super::model::ResolvedFile::decline_site)).
    /// It is diagnostic only: the walk branches on `Vetoed` versus
    /// [`None`](Self::None) and never on the cause.
    Vetoed(DeclineCause),
}

/// One [`OpenScope::auto_open_type_shadow_names`](crate::resolve::open_scope::OpenScope::auto_open_type_shadow_names) entry — see the field docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AutoOpenTypeShadow {
    /// The auto-open module declaration's start offset — where the name
    /// entered the scope, for the positional contest against in-file types.
    pub(super) import_pos: u32,
    /// The shallowest `container_path` length this name is visible at.
    pub(super) min_depth: usize,
}

/// The outcome of walking a dotted path through the full referenced-assembly
/// precedence order ([`Resolver::resolve_assembly_path_tiered`]). Unlike the
/// per-reading [`AssemblyPath`], the two failure modes here are *terminal*:
/// there is no next reading to retry — but they are still distinguished because
/// the *type* path records a "shadow possible" marker on a shadowed defer and
/// nothing on a clean no-match (see `Resolver::defer_shadowable_type`).
///
/// Generic in the winning reading's `payload`, like [`AssemblyPath`].
pub(super) enum TieredResolution<R> {
    /// A reading resolved — its `payload`. Either the highest-priority reading
    /// that resolves the **whole** path, or (when none does and no project
    /// shadow intervened) the highest-priority *partial* reading (rooting type
    /// resolved, tail deferred).
    ///
    /// `tier` is where in the ladder that reading came from. A caller that
    /// post-filters a resolved reading into a decline — a module leaf is not a
    /// type — needs it: the decline happened at a *specific* tier, and
    /// recording [`DeclineTier::WholeWalk`] there would hide the winner moving
    /// between tiers, which is the one thing the census exists to see.
    Resolved { payload: R, tier: DeclineTier },
    /// Some reading at winning priority is project-shadowed: a project entity
    /// owns the name there and may satisfy the whole path invisibly (sema does
    /// not model project types / nested-module members), so no assembly reading
    /// — complete or partial — may be applied. Defer.
    ///
    /// The payload names which guard spoke and the tier it spoke from, for the
    /// decline census
    /// ([`ResolvedFile::decline_site`](super::model::ResolvedFile::decline_site)).
    /// Diagnostic only — every caller branches on the variant, never on the
    /// site.
    ShadowDeferred(DeclineSite),
    /// No reading matched at all: nothing in the referenced assemblies resolves
    /// *or shadows* this path.
    NoMatch,
}

/// One `open` declaration's **namespace readings** — every base through which
/// its path names a referenced-assembly *or* project namespace — ordered
/// **highest-priority first**: through a prior open (latest first), then the
/// current enclosing namespace, then the as-written root. A relative `open Sub`
/// in `namespace Demo` reads as `[Demo.Sub, Sub]`; a root-level or
/// `global.`-rooted open is a single reading. Assembly and project readings
/// interleave by base proximity in the **one** list — F# opens all of them from
/// the one `open`, and a *project* relative reading out-ranks an assembly root
/// reading just as an assembly one would (FCS).
///
/// Constructed only from [`Resolver::open_interpretations`]' readings (explicit
/// opens) and [`implicit_open_groups`] (the auto-opens), so the ordering
/// invariant lives there; consumed only through
/// [`Resolver::open_reading_prefixes`], which fixes the walk order
/// (latest-open-first, then within an open readings as ordered here) for every
/// consumer.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpenGroup {
    /// The readings, highest-priority first.
    pub(super) readings: Vec<InternedPrefix>,
}

/// One entry in [`OpenScope::open_shortening_prefixes`](crate::resolve::open_scope::OpenScope::open_shortening_prefixes): a container some `open`
/// (explicit or implicit) brought into scope, under which a later `open`'s path
/// may be shortened.
///
/// `namespace_reading` carries forward the `namespaces_reachable` flag
/// `Resolver::open_tier_candidates` already computes per candidate: whether the
/// `open` that pushed this prefix could read the path as a **namespace** at all.
/// The assembly-side index of "auto-open modules folded by opening this
/// namespace" is keyed by path alone, so without the flag a module-only open of
/// a path that is *also* an assembly namespace would pick up that namespace's
/// auto-opens although FCS never opened it. The module-only tiers are the alias
/// tier and the module-segment relative bases, so the shape needs a module
/// abbreviation (or a nested-module base) whose target path is an assembly
/// namespace holding an `[<AutoOpen>]` module — not reachable in the fixture
/// grid, hence carried rather than tested: the flag only ever *removes*
/// prefixes, which is the sound direction.
///
/// `module_reading` is its twin for the module half: an assembly-level
/// `[<AutoOpen>]` names a *namespace*, so a prefix seeded from it must not fold
/// the auto-open descendants of a same-FQN **module** in some other reference —
/// FCS opened the namespace, not that module. An open that really did read a
/// module pushes its own entry with this set.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ShorteningPrefix {
    /// `path`'s id in the env's name table, interned when the prefix is
    /// pushed, so a written path is probed under it by id.
    pub(super) id: PathId,
    pub(super) path: Vec<String>,
    pub(super) namespace_reading: bool,
    pub(super) module_reading: bool,
}

impl ShorteningPrefix {
    /// The prefix `path`, interned in `names`.
    pub(super) fn new(
        names: &NameTable,
        path: Vec<String>,
        namespace_reading: bool,
        module_reading: bool,
    ) -> Self {
        ShorteningPrefix {
            id: names.intern_path(&path),
            path,
            namespace_reading,
            module_reading,
        }
    }
}

/// One interpretation of an `open <path>` clause at one base of the open's
/// walk ([`Resolver::open_interpretations`]), in the **one** proximity-ranked
/// list: the relativeness/nesting of the *path* sets precedence, not the
/// module-vs-reading category (FCS: a relative module out-ranks a same-named
/// root namespace, and a relative namespace out-ranks a root module). A base
/// can yield both kinds at once — a project module *merges* with a same-path
/// referenced-assembly namespace — but never a project module *and* a project
/// namespace (FS0247).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OpenInterpretation {
    /// A project module — `open M` brings its direct values into scope.
    Module(Vec<String>),
    /// An **assembly module** — `open M` brings a referenced module's values into
    /// unqualified scope (`docs/assembly-module-open-plan.md`). Like the project
    /// [`Module`](Self::Module) it is produced by the *tiered* walk, so a relative or
    /// shortened `open` (`namespace A; open M`, or `open A; open M`) reaches `A.M` at
    /// its true priority rather than only the path as written.
    AssemblyModule(Vec<String>),
    /// A namespace **reading** — assembly and/or project; feeds the open's
    /// [`OpenGroup`], its `[<AutoOpen>]` statics, its shortening prefix, and (for
    /// a project namespace) its direct cases/values.
    Reading(Vec<String>),
}

/// The namespaces a single declared name occupies **directly in one container** —
/// the per-name value of [`Indexes::container_decls`](crate::resolve::indexes::Indexes::container_decls), the complete declared-name
/// view that closes Gap A of `docs/type-qualified-case-prefix-plan.md`.
///
/// Only names reachable *bare* or as `Container.Name` are recorded (the
/// value/type/module namespaces): `let` values and `extern` functions, exception
/// constructors, union cases (require-qualified ones apart, since only a pattern
/// reaches them as `Container.Name`), types, nested modules / abbreviations, and
/// active-pattern cases. Enum cases are *not* — they are reachable only as
/// `Type.Case`, so they never collide with a module-qualifier segment.
///
/// The point of the *complete* view: deciding "is the segment `Color` unambiguously
/// a type here, or is something else also named `Color`?" needs certainty, and a
/// *defer* from a partial resolver does not give it (it may just be unmodelled).
/// With every declaration recorded,
/// [`is_clean_module_head`](Self::is_clean_module_head) /
/// [`is_dottable_value`](Self::is_dottable_value) answer it exactly.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct DeclKinds {
    /// A `let`-bound value/function. Kept separate from
    /// [`Self::exception_ctor`] because the two behave differently at a dotted
    /// *head* in expression position: a value commits member access (FCS binds
    /// `let Pal = 0` for `Pal.Color.Red` and fails on `.Color`), while an
    /// exception constructor does not (FCS resolves the module path past it).
    pub(super) value: bool,
    /// An exception constructor (`exception E`). A dottable value at a
    /// *segment* (`E.x` is member access on the constructor), but never a
    /// dotted-head owner: FCS-probed, `exception Pal; Pal.Color.Red` resolves
    /// through a `module Pal` from elsewhere in both expression and pattern
    /// position — and `exception Pal` co-declared *with* `module Pal` is
    /// FS0037 (an exception is a tycon), so head contention cannot legally
    /// arise in one container.
    pub(super) exception_ctor: bool,
    /// A type (union / record / enum / class / abbreviation — every
    /// [`define_type`](Resolver::define_type)).
    pub(super) ty: bool,
    /// A bare-reachable (non-RQA) union case constructor.
    pub(super) union_case: bool,
    /// A `[<RequireQualifiedAccess>]` union case. Neither bare-reachable nor
    /// found by an expression's search through a module, but a pattern's
    /// search through one finds it (`TryFindTypeWithUnionCase`, no RQA
    /// filter) before the module's submodules and types.
    pub(super) qualified_case: bool,
    /// A **real** nested module (`module M = …`). *Not* an abbreviation — those set
    /// [`Self::alias`] instead, since an alias's target may be cross-file.
    pub(super) module: bool,
    /// Source position (range start) of the latest `module M = …` declaration
    /// setting [`Self::module`]. The head environment is **one source-ordered
    /// latest-wins list over lexical module declarations and `open`s** (FCS-probed:
    /// an `open` declared *after* a lexical `module Pal` outranks it for the head
    /// of `Pal.Color.Red`, one declared *before* it loses), so a candidate is
    /// compared positionally against [`OpenScope::explicit_open_prefixes`](crate::resolve::open_scope::OpenScope::explicit_open_prefixes).
    pub(super) module_pos: Option<u32>,
    /// A second `module M = …` of the name in this container: FS0037, and FCS
    /// reads the *first* one where sema keeps the latest, so the container's
    /// view of the name is no longer one module.
    pub(super) module_redeclared: bool,
    /// The (latest) real nested module of the name is `module private`:
    /// reachable as a qualifier segment only from within this container.
    pub(super) module_private: bool,
    /// A module **abbreviation** (`module P = Target`). The target may be cross-file,
    /// so a `P.Color.Red` head must be left to the alias-aware cross-file path rather
    /// than treated as a same-file module — but a nearer alias still **shadows** an
    /// outer real module of the same name, so it must stop the head walk.
    pub(super) alias: bool,
    /// An active-pattern case.
    pub(super) active_pattern: bool,
}

/// The modeled members of one in-file type — the per-type value of
/// [`Indexes::type_members`](crate::resolve::indexes::Indexes::type_members). See that field for the population rules.
#[derive(Debug, Default)]
pub(super) struct TypeMemberSet {
    /// Member name → entry. One entry per *name*: a second same-name member
    /// (method overloads) clears [`MemberEntry::emit`] — FCS picks an overload
    /// by argument type, which sema cannot, so an overloaded name is owned but
    /// never emitted.
    pub(super) entries: HashMap<String, MemberEntry>,
    /// Emission disabled wholesale for this type: it has an `inherit` (base
    /// statics resolve through the derived name and could shadow or be
    /// shadowed — probe M6, unprobed precedence), or a member shape whose name
    /// the walker could not extract (its unknown name could overload an
    /// indexed one). Owned-name lookups still work — suppression only stops
    /// [`MemberEntry::emit`] answers.
    pub(super) emit_suppressed: bool,
}

/// One member name of a [`TypeMemberSet`].
#[derive(Debug)]
pub(super) struct MemberEntry {
    /// The emit target — the member-name binder — when this member is an
    /// unrestricted-access **static** (plain/`val`/get-set property or a
    /// single un-overloaded method). `None` for a name that is owned but not
    /// emittable: an instance member (it *commits* the qualifier — FCS errors
    /// FS0806 rather than backtracking, probe M9), an access-restricted or
    /// abstract member, a `static val` field (forcibly `private`, FS0881 —
    /// probe M3a), or an overloaded name.
    pub(super) emit: Option<DefId>,
    /// The source offset this entry exists from: the type definition's start
    /// for its own members, the augmentation's start for augmentation members
    /// (invisible before it — FCS `FS0039`, probe M4a). A use at an earlier
    /// offset treats the name as absent.
    pub(super) visible_from: borzoi_cst::syntax::TextSize,
}

impl DeclKinds {
    /// Whether a declaration of this shape **stops the dotted-head walk** — i.e.
    /// occupies a namespace that can own (or must redirect) the head `Pal` of
    /// `Pal.Color.Red`. FCS-probed: a dotted head is owned by the **module
    /// namespace** — a real nested module or a module abbreviation — plus, in
    /// **expression position only**, a `let`-bound value (which commits member
    /// access: FCS binds `let Pal = 0` and fails on `.Color`, rather than trying
    /// an outer module). Everything else — a type, union-case constructor,
    /// active pattern, or exception constructor — never hides a farther module
    /// (FCS resolves `Pal.Color.Red` through an *outer* same-file `module Pal`
    /// past any of them, in both positions), so the walk must skip it, not
    /// stop-and-miss (a miss would let the cross-file branches navigate to an
    /// earlier file while FCS binds the outer same-file module).
    pub(super) fn stops_dotted_head(self, in_pattern: bool) -> bool {
        self.module || self.alias || (!in_pattern && self.value)
    }

    /// Whether the name is a **clean module head** for a type-qualifier — a real
    /// nested module that binds the head of `Pal.Color.Red` to *this* file's module.
    /// The module wins for a dotted head over a co-declared **type**, **union-case
    /// constructor**, or **active pattern** (FCS-probed in both expression and
    /// pattern position: `type T = Pal | X; module Pal = …; Pal.Color.Red` resolves
    /// through the module) — so those do *not* disqualify it. A co-declared
    /// `let`-bound **value** wins over the module in *expression* position (member
    /// access — FCS binds the value and errors on `.Color`) but is invisible to a
    /// *pattern* head (FCS resolves the pattern to the module's case past it), so
    /// `value` disqualifies only outside patterns. A module **alias** must follow
    /// its (possibly cross-file) target — never clean. A co-declared exception
    /// constructor is FS0037 (illegal), so on such input we conservatively decline.
    pub(super) fn is_clean_module_head(self, in_pattern: bool) -> bool {
        self.module
            && !self.module_redeclared
            && !self.alias
            && !self.exception_ctor
            && (in_pattern || !self.value)
    }

    /// Whether the name is **only** a real nested module here — the shape a
    /// qualifier segment *after* the head must have for the walk to descend
    /// through it. FCS reads such a segment inside the module the previous
    /// segment named (`ResolveExprLongIdentInModuleOrNamespace` /
    /// `ResolvePatternLongIdentInModuleOrNamespace`), where values, union
    /// cases and exception constructors of the name are tried before the
    /// module and a type of the name beside it; none of that ordering is
    /// modelled, so any co-declaration leaves the segment undecided.
    pub(super) fn is_sole_module(self) -> bool {
        self.module
            && !self.module_redeclared
            && !self.alias
            && !self.value
            && !self.exception_ctor
            && !self.ty
            && !self.union_case
            && !self.qualified_case
            && !self.active_pattern
    }

    /// Whether the name is a **dottable value** — a `let` value, an exception
    /// constructor, or a union-case constructor (`union_case`). As a
    /// type-qualifier *segment* it wins over a co-named type (FCS: `let Color = 0;
    /// type Color = …; Pal.Color.Red` is member access on the value), so `Pal.Color`
    /// is same-file member access and the reference must not navigate cross-file.
    pub(super) fn is_dottable_value(self) -> bool {
        self.value || self.exception_ctor || self.union_case
    }
}

/// What the cross-file type-qualified case index reads a path as — the
/// result of [`Resolver::cross_file_type_case`].
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CrossFileCase {
    /// An earlier file's case reads the path, at this tier.
    Hit(CaseTier, ItemId),
    /// An earlier file's case reads the path, but a namesake of its type may
    /// be what FCS binds: decline, and try no lower tier.
    Declined,
    /// No earlier file's case reads the path.
    Miss,
}

/// How a same-file module-qualified `Pal.Color.Red` classifies — the result of
/// [`Resolver::classify_same_file_module_qualified_case`]. The `DeferStop`/`Miss`
/// split is the soundness keystone (see that method): when the head binds same-file,
/// the reference is same-file-rooted and must not fall through to cross-file
/// resolution.
pub(super) enum SameFileQualified {
    /// The segment is a same-file type with the case — resolve it.
    Emit {
        type_id: DefId,
        case_res: Resolution,
    },
    /// The head binds same-file but the path is contended / unresolvable —
    /// same-file-rooted, so defer **and stop** (do not try cross-file branches).
    DeferStop,
    /// As [`Self::DeferStop`], for a named guard whose cause the path's
    /// decline records.
    Decline(DeclineCause),
    /// Nothing same-file claims the head (or the segment is a submodule a
    /// qualified-value branch resolves) — the caller may try cross-file branches.
    Miss,
}

/// The **shape** of an active-pattern recognizer, computed at its definition
/// and keyed (in `Resolver::active_pattern_shape`) by each per-case *use def
/// id* — the identity `case_reference` returns for a same-file case. The shape
/// a use looks up is therefore exactly the shape of the recognizer the head
/// actually resolves to (it inherits `case_reference`'s scoping / shadowing for
/// free).
///
/// Its purpose is to drive FCS's split of an applied active-pattern use's
/// arguments into *parameters* (expressions, evaluated in the enclosing scope)
/// and the *result sub-pattern* (a binder) — see
/// `docs/parameterized-active-pattern-args-plan.md`. No consumer reads it yet
/// (Stage 1 only stores it); the split lands in Stage 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivePatternShape {
    /// No trailing `|_|` case — a *total* recognizer (`(|Even|Odd|)`,
    /// `(|Scale|)`). A partial one (`(|Parse|_|)`) is `false`.
    pub total: bool,
    /// Exactly one case ident (`(|Scale|)`, `(|Parse|_|)`). A multi-case
    /// recognizer (`(|Even|Odd|)`) is `false`.
    pub single_case: bool,
    /// Curried-parameter count of the **function form** (`args().count() − 1`,
    /// the last curried arg being the matched value): `let (|DivBy|_|) d n = …`
    /// → `Some(1)`, `let (|P|_|) a b n = …` → `Some(2)`. `None` for the
    /// bare-name (point-free) form `let (|P|_|) = …`, whose parameter count is
    /// syntactically invisible (FCS derives it from the inferred type) — *not*
    /// `Some(0)`.
    pub arity: Option<usize>,
}

/// One file's walk, its state split by how long each part lives, so a
/// method can borrow one part while it writes another:
///
/// - the **inputs**, borrowed for the whole walk: `preceding`, `assemblies`,
///   `item_base`, `recovery`, and the [`Prescan`], what is decided before the
///   walk starts;
/// - the [`OpenScope`], which lives as long as the module that wrote it, and
///   the [`Windows`], which live for one call's dynamic extent (D5);
/// - [`Lexical`], where the walk is, pushed and popped as it enters and
///   leaves declarations;
/// - the [`Indexes`], which keep what the walk records for the rest of the
///   file;
/// - the [`Output`], which the walk publishes in one piece when it finishes.
pub(super) struct Resolver<'a> {
    /// Exports of earlier Compile-order files, for cross-file qualified lookup.
    pub(super) preceding: &'a ProjectItems,
    /// Name index over referenced assemblies, for fully-qualified type/member
    /// path resolution.
    pub(super) assemblies: &'a AssemblyEnv,
    /// Project-global id of this file's first item (the `preceding` count).
    pub(super) item_base: u32,
    /// What the parser had to recover from in the file being walked, as the
    /// caller's parse reports it, published as
    /// [`ResolvedFile::recovery`](super::model::ResolvedFile).
    pub(super) recovery: &'a SyntaxRecovery,
    /// What was decided before the walk started ([`Prescan`]), borrowed so
    /// the walk cannot write it.
    pub(super) prescan: &'a Prescan,
    /// Everything the opens and module headers in scope decide, which lives
    /// as long as the module that wrote it ([`OpenScope`]).
    pub(super) scope: OpenScope,
    /// State that holds a value only for one call's dynamic extent
    /// ([`Windows`]), written only by the scoped calls that open it.
    pub(super) windows: Windows,
    /// Where the walk is: the scope stacks, the container and the top-level
    /// block ([`Lexical`]).
    pub(super) lexical: Lexical<'a>,
    /// What the walk has recorded about the file's declarations so far, for
    /// its own lookups, kept for the rest of the file ([`Indexes`]).
    pub(super) index: Indexes,
    /// What the walk publishes, moved into the [`ResolvedFile`](super::ResolvedFile)
    /// in one piece ([`Output`]).
    pub(super) out: Output,
}

/// What the binders interned so far in **one** pattern walk resolved to, so an
/// or-pattern alias can be recorded against the alternative it names.
///
/// [`PatternName::Alias`](crate::PatternName::Alias) carries its canonical
/// binder's *range*; the interning loop that walks the same list has already
/// turned that binder into a [`Resolution`] — a [`Resolution::Local`] in a
/// pattern position, a [`Resolution::Item`] at a module-level `let` head — and
/// parks it here for the alias to pick up.
///
/// Scoped to a single pattern because an alias never names a binder outside its
/// own or-pattern, and the ranges are unique only within one file's walk.
#[derive(Default)]
pub(super) struct AliasTargets(Vec<(TextRange, Resolution)>);

impl AliasTargets {
    /// Note that the binder whose ident token spans `range` was interned as
    /// `res`.
    pub(super) fn interned(&mut self, range: TextRange, res: Resolution) {
        self.0.push((range, res));
    }

    /// What to record at an alias of the binder at `binder`, or `None` when that
    /// binder was never interned — it was excluded as an active-pattern argument,
    /// or dropped as an unresolved provisional head. The alias then records
    /// nothing too, which is exactly what its binder did: a coverage gap, never a
    /// wrong target.
    ///
    /// Linear: an or-pattern binds a handful of names, so a map would cost more
    /// than it saves.
    pub(super) fn resolution_of(&self, binder: TextRange) -> Option<Resolution> {
        self.0
            .iter()
            .find(|(range, _)| *range == binder)
            .map(|(_, res)| *res)
    }
}

/// F#'s implicit auto-opened *namespaces* — opened in every file so their
/// types are reachable unqualified (`List`, `Option`, `Async`) and their
/// `[<AutoOpen>]` modules' statics resolve bare (`printfn`).
///
/// Data-driven from the referenced assemblies' assembly-level
/// `[<assembly: AutoOpen("…")>]` attributes ([how FCS builds the
/// set][AssemblyEnv::implicit_open_namespace_paths]; there is no hardcoded list in
/// the compiler), then a hardcoded FSharp.Core fallback is appended (deduped). The
/// fallback keeps two situations working unchanged:
/// - an env whose FSharp.Core stand-in lacks the manifest attributes (the
///   `autoopen_env` test fixture declares `namespace Microsoft.FSharp.Core`
///   but no assembly-level AutoOpen);
/// - an empty env, where the seeds are inert but a *project-declared*
///   `Microsoft.FSharp.*` namespace still gets its historical implicit open.
///
/// With real FSharp.Core in the env the data-driven list is a superset of the
/// fallback, so appending changes nothing and the order is exactly FCS's
/// (`Microsoft`, `Microsoft.FSharp`, `Microsoft.FSharp.Core`, …).
///
/// This is exactly [`AssemblyEnv::effective_implicit_open_namespace_paths`] — the
/// single source of truth the overload extension gate reads too, so the gate proves
/// a name absent only from namespaces the resolver actually opens.
pub(super) fn implicit_open_namespaces(assemblies: &AssemblyEnv) -> Vec<Vec<String>> {
    assemblies.effective_implicit_open_namespace_paths()
}

/// The implicit auto-opens as [`OpenScope::imports`](crate::resolve::open_scope::OpenScope::imports) entries — each is its own
/// single-reading `open` group (no relative/root split; they are fully qualified),
/// in source order *before* any explicit `open`, so an explicit open shadows them
/// under the latest-open-wins walk.
/// The implicit auto-opens as [`OpenScope::open_shortening_prefixes`](crate::resolve::open_scope::OpenScope::open_shortening_prefixes) entries.
/// Every one is a namespace reading, so each lends the `[<AutoOpen>]` modules it
/// folds as further prefixes ([`Resolver::auto_open_shortening_prefixes`]).
pub(super) fn implicit_shortening_prefixes(assemblies: &AssemblyEnv) -> Vec<ShorteningPrefix> {
    implicit_open_namespaces(assemblies)
        .into_iter()
        .map(|path| ShorteningPrefix::new(assemblies.names(), path, true, false))
        .collect()
}

pub(super) fn implicit_open_groups(assemblies: &AssemblyEnv) -> Vec<OpenGroup> {
    implicit_open_namespaces(assemblies)
        .into_iter()
        .map(|ns| OpenGroup {
            readings: vec![InternedPrefix::new(assemblies.names(), ns)],
        })
        .collect()
}

#[cfg(test)]
mod frame_tests {
    use proptest::prelude::*;

    use super::{DeferredReason, Frame, NameKey, NameTable, Resolution, ScopeEntry};

    const NAMES: [&str; 4] = ["a", "b", "ab", "``a``"];

    #[derive(Debug, Clone)]
    enum Op {
        Push { name: usize, from_open: bool },
        Extend(Vec<(usize, bool)>),
        RotateTailRight { start: usize, k: usize },
        Retain,
        SetGeneration(usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        let entry = (0..NAMES.len(), any::<bool>());
        prop_oneof![
            4 => entry.clone().prop_map(|(name, from_open)| Op::Push { name, from_open }),
            2 => prop::collection::vec(entry, 0..6).prop_map(Op::Extend),
            1 => (0..40usize, 0..40usize).prop_map(|(start, k)| Op::RotateTailRight { start, k }),
            1 => Just(Op::Retain),
            1 => (0..3usize).prop_map(Op::SetGeneration),
        ]
    }

    /// Each entry is told apart by its `open_pos`, which the frame never reads.
    fn entry(names: &NameTable, name: usize, from_open: bool, id: u32) -> ScopeEntry {
        let mut e = ScopeEntry::binding(
            names,
            NAMES[name],
            Resolution::Deferred(DeferredReason::UnboundName),
            0,
        );
        e.from_open = from_open;
        e.open_pos = id;
        e
    }

    fn ids<'a>(entries: impl Iterator<Item = &'a ScopeEntry>) -> Vec<(u32, usize)> {
        entries.map(|e| (e.open_pos, e.generation)).collect()
    }

    proptest! {
        /// The frame's name index answers exactly what a scan of its entries
        /// from the latest back would, after any sequence of writes.
        #[test]
        fn named_rev_is_the_latest_first_scan(
            initial in prop::collection::vec((0..NAMES.len(), any::<bool>()), 0..8),
            ops in prop::collection::vec(op(), 0..40),
        ) {
            let names = NameTable::default();
            let mut next_id = 0u32;
            let mut fresh = |name: usize, from_open: bool| {
                next_id += 1;
                entry(&names, name, from_open, next_id)
            };
            let mut model: Vec<ScopeEntry> =
                initial.iter().map(|&(n, o)| fresh(n, o)).collect();
            let mut frame = Frame::new(model.clone());
            for op in ops {
                match op {
                    Op::Push { name, from_open } => {
                        let e = fresh(name, from_open);
                        model.push(e.clone());
                        frame.push(e);
                    }
                    Op::Extend(entries) => {
                        let es: Vec<ScopeEntry> =
                            entries.iter().map(|&(n, o)| fresh(n, o)).collect();
                        model.extend(es.iter().cloned());
                        frame.extend(es);
                    }
                    Op::RotateTailRight { start, k } => {
                        let start = start.min(model.len());
                        let k = k.min(model.len() - start);
                        model[start..].rotate_right(k);
                        frame.rotate_tail_right(start, k);
                    }
                    Op::Retain => {
                        model.retain(|e| !e.from_open);
                        frame.retain(|e| !e.from_open);
                    }
                    Op::SetGeneration(g) => {
                        for e in &mut model {
                            e.generation = g;
                        }
                        frame.set_generation(g);
                    }
                }
                prop_assert_eq!(frame.len(), model.len());
                prop_assert_eq!(ids(frame.entries().iter()), ids(model.iter()));
                for name in NAMES {
                    let key = NameKey::of(&names, name);
                    prop_assert_eq!(
                        ids(frame.named_rev(key)),
                        ids(model.iter().rev().filter(|e| names.text(e.name) == name)),
                        "entries named {}", name
                    );
                }
            }
        }
    }
}

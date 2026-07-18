//! The name-resolution engine: long-ident / value / case lookup and the project-open machinery.

use std::collections::{HashMap, HashSet};

use borzoi_cst::syntax::SyntaxToken;
use rowan::{TextRange, TextSize};

use borzoi_assembly::EntityKind;

use crate::assembly_env::{EntityHandle, OpenFoldSpace, OpenFoldSurface, OpenFoldTarget};
use crate::def::{DefId, DefKind};

use super::id_text;
use super::model::{DeferredReason, ItemId, Resolution, SlotClass};
use super::state::{
    ActivePatternShape, AssemblyPath, OpenInterpretation, Resolver, SameFileQualified, ScopeEntry,
    ShadowVeto, TieredResolution,
};

/// How FCS's unqualified-name slot reads for a compound head that `lookup`
/// classified as a definite value. FCS's `eUnqualifiedItems` is **one**
/// source-ordered latest-wins slot across the value and type namespaces
/// (`ResolveExprLongIdentPrim`): a compound reference `Color.Red` is member
/// access on the value only while the value is the slot's latest entry
/// (`ValIsInEnv` — total priority); a `type Color` entering the slot later
/// EVICTS it, and modules are then searched before type statics. Probes
/// M20a–M20i, all dotnet-build-verified + fcs-dump-pinned (the §5 follow-up
/// of `docs/project-type-member-plan.md`).
enum HeadSlot {
    /// The value provably holds the slot (no later in-scope type of the
    /// name) — the head is member access on the value, exactly the
    /// pre-eviction behaviour.
    Held,
    /// A later in-scope type provably evicted the value — the head is a
    /// module/type-qualified path: the qualified block may resolve it
    /// (modules first), and on failure the head DEFERS, never re-binding
    /// the evicted value.
    Evicted,
    /// A type of the name is in scope but cannot be ordered against the
    /// value (an opened value has no in-file position) — defer, mirroring
    /// the member branch's positionless-contest arm.
    Unordered,
}

/// The [`SlotClass`] of a **referenced-assembly** type, from FCS's
/// `mayHaveConstruction = isClassTy || isStructTy || isDelegateTy`
/// (`AddPartsOfTyconRefToNameEnv`), the same predicate the project-side
/// [`SlotClass`] classification uses. `is_struct` is the reliable IL
/// value-type signal (the type extends `System.ValueType`) — trustworthy,
/// unlike the project side's *spoofable* source `[<Struct>]` attribute (round
/// 7), so an assembly struct/enum/`[<Struct>]`-record evicts precisely. The
/// mapping errs toward `Evicts`/`Unknown`, never `Keeps`, because
/// under-eviction is the only unsound direction (recording the value where FCS
/// evicts is a wrong target; over-eviction only defers a resolvable head — an
/// availability loss). A referenced F# union/record read as
/// [`EntityKind::Class`] (its signature pickle undecoded) therefore
/// over-evicts safely (probes A1–A6/Aenum/Ageneric/Amodule of
/// `docs/head-slot-assembly-eviction-plan.md`).
fn assembly_slot_class(kind: EntityKind, is_struct: bool) -> SlotClass {
    if is_struct {
        // Any IL value type: `Struct`, `Enum`, `[<Struct>]` record/union.
        return SlotClass::Evicts;
    }
    match kind {
        // `isClassTy` (C# class, F# class) or `isStructTy` (belt-and-braces —
        // `is_struct` should already have caught these).
        EntityKind::Class | EntityKind::Struct | EntityKind::Enum => SlotClass::Evicts,
        // Not construction-capable — the value keeps the slot (interface: A2;
        // F# union/record: `isClassTy`-false, project M20k/M20l; F# module:
        // Amodule; measure: never an expression-position type).
        EntityKind::Interface
        | EntityKind::Union
        | EntityKind::Record
        | EntityKind::Module
        | EntityKind::Measure => SlotClass::Keeps,
        // Undecidable — a delegate is feature-gated in FCS
        // (`DelegateTypeNameResolutionFix`), an abbreviation chases its target
        // (project M20n), and an F# exception's slot behaviour is unprobed:
        // defer the contest rather than risk a wrong target.
        EntityKind::Delegate | EntityKind::Abbreviation | EntityKind::Exception => {
            SlotClass::Unknown
        }
    }
}

/// The names that appear in **more than one** of the supplied groups — the
/// cross-group collision set for the fold writer, where each group is one
/// surface's names in a single lookup namespace. A name repeated *within* one
/// group is not a collision (fold order decides there); only a name two
/// different groups both supply is the reference-order contest that must defer.
fn collisions<'n, G, N>(groups: G) -> HashSet<String>
where
    G: Iterator<Item = N>,
    N: Iterator<Item = &'n str>,
{
    let mut first_supplier: HashMap<&str, usize> = HashMap::new();
    let mut collided: HashSet<String> = HashSet::new();
    for (i, names) in groups.enumerate() {
        for name in names {
            match first_supplier.get(name) {
                Some(&j) if j != i => {
                    collided.insert(name.to_string());
                }
                Some(_) => {}
                None => {
                    first_supplier.insert(name, i);
                }
            }
        }
    }
    collided
}

/// What an `[<AutoOpen>]` submodule fold contributes for one name, split by the
/// three FCS environments a cross-tier straddle must order independently — a
/// single conflated maximum mis-orders them (codex review of the straddle
/// slice), so [`Resolver::submodule_contributions_at`] records each separately
/// and a straddling direct-tier case is compared against it per environment.
#[derive(Default, Clone, Copy)]
struct SubmoduleFold {
    /// Latest file supplying the name in the **value** namespace — a value or an
    /// unshadowed case (what a bare *expression* reads).
    value_slot: Option<usize>,
    /// Latest file supplying the name in the **constructor** namespace — a case
    /// or exception (what a bare *pattern* reads); a plain value never enters it.
    case: Option<usize>,
    /// Whether some submodule supplies the name as a **constructible type**,
    /// which takes FCS's value slot but whose constructor sema cannot model — so
    /// its presence can only defer the value dimension, never resolve it.
    has_type: bool,
}

impl<'a> Resolver<'a> {
    /// Push one source-ordered *opened* value entry per distinct bare name of
    /// `handle` into the current frame — the names an `open type T` (or the
    /// auto-open fold over a module) brings into unqualified scope (`open type
    /// System.Math` makes `Sqrt` resolve to `System.Math.Sqrt`). A uniquely-selectable
    /// static records its [`Resolution::Member`]; an overloaded / metadata-ambiguous
    /// name records [`Resolution::Deferred`] — the name is in scope and shadows by
    /// position, but choosing among overloads is the type checker's job, not name
    /// resolution.
    ///
    /// [`AssemblyEnv::open_static_entries`] supplies the pair, and with it the one
    /// exclusion this level owes FCS: **extension members never enter the
    /// unqualified environment** — not F#-native augmentations (bare `Force` out of
    /// FSharp.Core's auto-open `LazyExtensions`), not C#-style `[<Extension>]`
    /// statics (bare `Select` after `open type System.Linq.Enumerable`). Both are
    /// FS0039 to the real compiler; both used to resolve here.
    ///
    /// The entries are [`ScopeEntry::opened`], so [`lookup`](Self::lookup) drops
    /// them while an [`opaque_value_open`](Self::opaque_value_open) is in scope and
    /// [`resolve_file`] does not leak them across same-named top-level blocks.
    /// `certain` withholds the definite target while keeping the name in scope (it still
    /// shadows by position). Callers pass `false` when something they cannot see could
    /// outrank what they can: the namespace half of a cross-kind path does so when the
    /// **module** half at the same FQN is not fully enumerable, because a hidden module
    /// name can contest a namespace name and FCS folds the halves in reference order
    /// (review round 17). A merge names a definite target only when *every* half is fully
    /// enumerable.
    pub(super) fn open_type_statics(&mut self, handle: EntityHandle, open_pos: u32, certain: bool) {
        // Collect first — `open_static_entries` borrows `self.assemblies`, and the
        // push below borrows `self.scopes` mutably; the two must not overlap.
        let generation = self.open_generation;
        let entries: Vec<ScopeEntry> = self
            .assemblies
            .open_static_entries(handle)
            .into_iter()
            .map(|(name, idx)| {
                let res = match idx {
                    Some(idx) if certain => Resolution::Member {
                        parent: handle,
                        idx,
                    },
                    _ => Resolution::Deferred(DeferredReason::UnboundName),
                };
                ScopeEntry::opened(name.to_string(), res, generation, open_pos)
            })
            .collect();
        self.module_frame().entries.extend(entries);
    }

    /// Push the complete-or-opaque **fold surfaces** of the assembly module(s)
    /// at one opened path — one [`OpenFoldSurface`] per referenced assembly
    /// exposing the FQN, already enumerated by the caller (it needs their
    /// residue *before* deciding the generation barrier). This is the fold's
    /// entry writer (`docs/assembly-module-open-plan.md`, "the fold"): every
    /// name each surface lists goes into scope in FCS's fold order, into the
    /// namespace(s) it occupies — value entries, constructor-case entries
    /// ([`ScopeEntry::opened_case`]), pattern-only active-pattern tags.
    ///
    /// A name names its definite target ([`OpenFoldTarget::Member`] /
    /// [`OpenFoldTarget::Entity`]) unless:
    /// - the surface itself marked it opaque (a union case, a type name, an
    ///   overload set);
    /// - **more than one assembly** supplies the name — which one FCS binds
    ///   depends on reference order, which sema does not model;
    /// - the caller passed `demote` — the group carries name-unknown residue
    ///   (or an unfolded namespace half), so fold order within it is not
    ///   decidable and every one of its names must defer.
    ///
    /// A demoted name is pushed as [`Resolution::Deferred`]: in scope,
    /// shadowing by position, naming nothing (D5 — never a wrong target).
    /// `demote_cases` demotes the constructor-**case** entries alone (union
    /// cases, exception constructors, active-pattern tags): the group carries
    /// tycon-tier-confined residue ([`OpenFoldSurface::residue_below_vals`] —
    /// a case-nameless union), whose hidden names share the tycon tier with
    /// these entries but fold *before* the vals, which therefore stay
    /// definite (round 10).
    ///
    /// A surface's [`OpenFoldSurface::contestant_names`] (a namespace half's
    /// constructor-slot **type** names) count as belonging to that surface for the
    /// collision test but are never pushed as entries: a same-named *value* from
    /// another surface (the module half) then collides and defers — a bare
    /// value-vs-type contest FCS orders by reference and we do not — while the
    /// contestant's own surface keeps its later `[<AutoOpen>]` value.
    pub(super) fn open_assembly_module_fold(
        &mut self,
        surfaces: Vec<OpenFoldSurface>,
        open_pos: u32,
        demote: bool,
        demote_cases: bool,
    ) {
        let generation = self.open_generation;
        // A name supplied by two *different* surfaces is a reference-order contest:
        // demote it. Within ONE surface, fold order decides (latest push wins), so
        // a repeated name there is not a collision. A surface's contestant type
        // names participate as if they were its entries (so a module-half value of
        // that name collides), but are not themselves resolvable.
        //
        // The demotion is **not** split by lookup namespace. F# does keep the
        // expression value and pattern constructor namespaces separate — a plain
        // value never enters the pattern map, so in principle a value/exception
        // clash could defer only the expression and still name the exception in a
        // pattern. But a value that is a `[<Literal>]` (or a `decimal` literal,
        // which fsc emits as an init-only field with `DecimalConstantAttribute`,
        // NOT the CLI `Literal` flag — Q17) *is* a constant pattern, so it contests
        // the pattern namespace too; we cannot reliably tell those apart from a
        // plain value here. A per-space split therefore risks committing an
        // exception in pattern position where FCS binds a colliding literal (codex
        // round 8), so a collided constructor entry defers in **both** namespaces —
        // correctness over the narrow availability of a pattern-only survivor.
        let collided = collisions(surfaces.iter().map(|s| {
            s.entries
                .iter()
                .map(|e| e.name.as_str())
                .chain(s.contestant_names.iter().map(String::as_str))
        }));
        let mut entries: Vec<ScopeEntry> = Vec::new();
        for s in &surfaces {
            for e in &s.entries {
                let demoted = demote || (demote_cases && e.is_case) || collided.contains(&e.name);
                let res = if demoted {
                    Resolution::Deferred(DeferredReason::UnboundName)
                } else {
                    match e.target {
                        OpenFoldTarget::Member { parent, idx } => {
                            Resolution::Member { parent, idx }
                        }
                        OpenFoldTarget::Entity(h) => Resolution::Entity(h),
                        OpenFoldTarget::Opaque => Resolution::Deferred(DeferredReason::UnboundName),
                    }
                };
                let mut entry = match e.space {
                    // Active-pattern tags live in the constructor namespace only —
                    // an expression-position use never sees them.
                    OpenFoldSpace::Pattern => {
                        ScopeEntry::opened_pattern_only(e.name.clone(), res, generation, open_pos)
                    }
                    OpenFoldSpace::Value | OpenFoldSpace::Both => {
                        ScopeEntry::opened(e.name.clone(), res, generation, open_pos)
                    }
                };
                entry.opened_case = e.is_case;
                // A val that may be a CLI literal / decimal constant contests the
                // pattern namespace as a constant pattern (see
                // [`OpenFoldName::constant_pattern`]); demoted entries are
                // `Deferred` and defer in the scan regardless.
                entry.maybe_constant_pattern = e.constant_pattern;
                // An assembly active-pattern tag carries its demangled recognizer
                // shape (Stage 3b): its `Deferred` resolution has no identity to
                // key the shape on, so the applied-head split reads it from here.
                // But a **demoted** entry defers precisely because which
                // constructor occupies the name is unknown — a residue-bearing
                // open could hide a shadowing case, or two assemblies could
                // contribute a same-named union case / differently-shaped
                // recognizer that FCS binds by reference order — so its shape is
                // untrustworthy and must not drive the split (certain-implies-exact).
                entry.opened_ap_shape = if demoted { None } else { e.ap_shape };
                entries.push(entry);
            }
        }
        self.module_frame().entries.extend(entries);
    }

    /// Whether `mp` is **exactly** a declared in-project module path — a
    /// top-level [`module`](Self::module_paths) header, a nested module's full
    /// qualified path ([`Self::nested_module_exports`]), or either from an earlier
    /// Compile-order file. An *exact* match (not the "rooted at / under a module"
    /// prefix test of [`open_imports_project_values`](Self::open_imports_project_values)),
    /// because [`resolved_project_module`](Self::resolved_project_module) must
    /// resolve `open Shared` to the real module `Demo.Shared`, not to a spurious
    /// `Demo.N.Shared` that is merely "under" the current module `Demo.N`.
    /// Anonymous-root nested modules are deliberately excluded (their values carry
    /// no qualified export path, so they cannot be enumerated — the conservative
    /// `open_imports_project_values` fallback covers them).
    pub(super) fn is_project_module_path(&self, mp: &[String]) -> bool {
        self.module_paths.iter().any(|p| p == mp)
            || self.nested_module_exports.iter().any(|p| p == mp)
            || self.preceding.is_exact_project_module(mp)
            || self.preceding.is_exact_nested_module(mp)
    }

    /// Whether `np` is a declared project **namespace** path — same file
    /// ([`Self::namespace_paths`]) or an earlier Compile-order one
    /// ([`ProjectItems::is_namespace`]).
    pub(super) fn is_project_namespace_path(&self, np: &[String]) -> bool {
        self.namespace_paths.iter().any(|p| p == np) || self.preceding.is_namespace(np)
    }

    /// Everything an `open <path>` names — project modules **and** namespace
    /// readings (assembly and/or project) — as a single list ordered **highest
    /// priority first**. Precedence is the path's relativeness/nesting, *not*
    /// the kind: a relative module out-ranks a same-named root namespace and a
    /// relative namespace out-ranks a root module (FCS). One base yields at most
    /// one path; the path can be a module *and* an assembly-namespace reading at
    /// once (they merge), the module first — its values shadow the merged
    /// namespace's statics — but never a project module *and* a project
    /// namespace (FS0247).
    ///
    /// The one walk (previously two parallel walks whose disjoint application
    /// phases let every module out-rank every reading regardless of base):
    /// 0. a lexically in-scope **module alias** as the bare head (modules only —
    ///    an alias expands to a module path, under which no namespace lives);
    /// 1. **explicit opens**, latest first;
    /// 2. **enclosing nesting**, innermost first — modules may nest through the
    ///    full container; namespaces only through the enclosing-namespace prefix
    ///    (`namespace_depth`), never a module segment (FCS leaves `open Inner`
    ///    in `module Outer.Client` → `Outer.Client.Inner` undefined; a relative
    ///    `open` reaches only the current namespace's immediate child, with an
    ///    FS0893 partial-path warning, and the root — never an ancestor, FS0039);
    /// 3. the **root** path as written.
    ///
    /// `rooted` (a `global.`-qualified open) keeps only tier 3. Returning *all*
    /// readings (not a single canonical one) matches FCS's name environment: a
    /// later chained open keeps the relative reading higher **and** still
    /// reaches a root-only namespace. A project reading resolves no assembly
    /// path itself, but a name it *shadows* must be seen at its true priority
    /// ([`AssemblyPath::ProjectShadowed`] → defer) before any lower assembly
    /// reading can win.
    pub(super) fn open_interpretations(
        &self,
        written: &[String],
        rooted: bool,
    ) -> Vec<OpenInterpretation> {
        let mut out: Vec<OpenInterpretation> = Vec::new();
        let consider = |out: &mut Vec<OpenInterpretation>,
                        full: Vec<String>,
                        namespaces_reachable: bool| {
            if self.is_project_module_path(&full)
                && !out
                    .iter()
                    .any(|o| matches!(o, OpenInterpretation::Module(p) if *p == full))
            {
                out.push(OpenInterpretation::Module(full.clone()));
            }
            // An **assembly** module at this base. Considered *after* a project module
            // of the same path — but **not suppressed by it**: FCS merges the two, so
            // `open Demo.ModuleOpen.Plain` with a project module of that FQN imports the
            // project's values *and* the referenced assembly's, the project's winning a
            // collision (both fsi-verified — the apply loop pushes this one first, so
            // latest-wins gives the project module the collision; review round 6).
            //
            // The tiered walk is what reaches it at all: without it the open would only
            // ever see the path *as written*, so `namespace A; open M` and
            // `open A; open M` would never find `A.M`, and a colliding root `M` could
            // win instead (review, Slice A round 1).
            if self.opened_assembly_module(&full).is_some()
                && !out
                    .iter()
                    .any(|o| matches!(o, OpenInterpretation::AssemblyModule(p) if *p == full))
            {
                out.push(OpenInterpretation::AssemblyModule(full.clone()));
            }
            if namespaces_reachable
                && (self.assemblies.has_namespace(&full) || self.is_project_namespace_path(&full))
                && !out
                    .iter()
                    .any(|o| matches!(o, OpenInterpretation::Reading(p) if *p == full))
            {
                out.push(OpenInterpretation::Reading(full));
            }
        };
        for (full, namespaces_reachable) in self.open_tier_candidates(written, rooted) {
            consider(&mut out, full, namespaces_reachable);
        }
        out
    }

    /// Every **candidate full path** an `open <written>` could name, tier by
    /// tier — the single tier enumeration behind
    /// [`open_interpretations`](Self::open_interpretations) *and* the
    /// dropped-path hazard check in `decls.rs`. One enumeration, two
    /// consumers, so the two cannot drift apart ("two traversals over one
    /// space must span the same space" — review round 16; a dropped TypeDef
    /// can BE the module a candidate names, round 23). Each candidate is
    /// paired with whether namespaces are reachable at its tier (see the
    /// tier notes on `open_interpretations`).
    pub(super) fn open_tier_candidates(
        &self,
        written: &[String],
        rooted: bool,
    ) -> Vec<(Vec<String>, bool)> {
        let mut out: Vec<(Vec<String>, bool)> = Vec::new();
        if !rooted {
            // Tier 0 — a module alias as the bare head (`open Alias[.rest]` →
            // `Target[.rest]`); the alias is a module, never a namespace.
            if let Some((head, rest)) = written.split_first()
                && let Some(target) = self.lexical_alias_target(head)
            {
                let mut full = target.to_vec();
                full.extend_from_slice(rest);
                out.push((full, false));
            }
            // Tier 1 — explicit opens (namespace + chained module), latest first.
            for prefix in self.open_shortening_prefixes.iter().rev() {
                let mut full = prefix.clone();
                full.extend_from_slice(written);
                out.push((full, true));
            }
            // Tier 2 — implicit relative resolution, innermost first; namespaces
            // are reachable only at the enclosing-namespace base.
            for k in (self.namespace_depth.max(1)..=self.container_path.len()).rev() {
                let mut full = self.container_path[..k].to_vec();
                full.extend_from_slice(written);
                out.push((full, k == self.namespace_depth));
            }
        }
        // Tier 3 — the as-written root, kept *in addition* to any relative match.
        out.push((written.to_vec(), true));
        out
    }

    /// The **referenced-assembly** namespace a possibly-relative `open path`
    /// names, F#-style: the innermost enclosing-namespace-prefixed form that is an
    /// assembly namespace (a relative open binds to the *nearest* enclosing
    /// namespace's child), else the as-written root path. A `global`-rooted open
    /// (`rooted`) is absolute and returned unchanged.
    ///
    /// The **current enclosing namespace** — `container_path[..namespace_depth]` —
    /// the single namespace prefix a relative path is resolved through (F#'s
    /// FS0039: a relative `open`/reference reaches the *current* namespace's
    /// child and the root, **never an ancestor** `A.B` from inside `A.B.C`, and
    /// never a module segment past the namespace). Empty inside a top-level/nested
    /// module (`namespace_depth == 0`). This is the one place the namespace-
    /// relative window lives, matching [`Self::open_interpretations`]'s tier-2
    /// `k == namespace_depth` candidate, so the assembly-side canonicalisation and
    /// type resolution can't drift from it (the bug an ad-hoc per-site loop kept
    /// re-introducing).
    pub(super) fn enclosing_namespace(&self) -> &[String] {
        &self.container_path[..self.namespace_depth.min(self.container_path.len())]
    }

    /// Whether opening module `mp` may bring **value-space names we cannot
    /// enumerate** — a module alias, or union cases / exception constructors /
    /// active patterns ([`Self::modules_with_hidden_values`], same file or an
    /// earlier Compile-order one). Such an open conservatively shadows earlier
    /// opens (see the `open` arm's generation bump).
    pub(super) fn module_has_hidden_values(&self, mp: &[String]) -> bool {
        self.modules_with_hidden_values.contains(mp)
            || self.preceding.modules_with_hidden_values.contains(mp)
    }

    /// The qualified paths of `[<AutoOpen>]` modules directly under `container`
    /// (see [`super::model::is_directly_in`]) — the **distinct paths**, from
    /// earlier files' non-`private` ones ([`ProjectItems::auto_open_modules_directly_in`],
    /// privacy-filtered at the export boundary) plus this file's own
    /// **accessible** ones. Path-level and *file-blind*: it answers "is there an
    /// auto-open submodule of this name here?", which is what the
    /// conservative consumers ([`Self::namespace_fold_has_hidden_values`],
    /// [`Self::project_namespace_contestant_names`]) need — both over-defer, so an
    /// over-reach (a submodule declared only under a *plain* parent fragment) is
    /// sound. The **fold itself** does not route through here: it uses the
    /// per-fragment, file-ordered [`Self::auto_open_fragments_reachable`]
    /// (Stage 5), which is exact about which `(module, file)` fragments are
    /// auto-opened and in what order.
    ///
    /// A same-file `private` submodule is filtered by accessibility, not
    /// blanket-included (codex review round 4, fcs-dump-verified: `namespace N`
    /// with `[<AutoOpen>] module private A`, then an UNRELATED `namespace Other`
    /// in the SAME file doing `open N`, does not see `A`'s contents — FCS
    /// reports the name unbound). `private` restricts a module to its own
    /// enclosing container and that container's descendants, so it is visible
    /// only when [`Resolver::container_path`] — the *open statement's own*
    /// lexical position — starts with `container` (the candidate's direct
    /// parent). `preceding`'s half needs no such check: it is already
    /// privacy-filtered at the file/export boundary.
    pub(super) fn project_auto_open_submodules_in(&self, container: &[String]) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = self.preceding.auto_open_modules_directly_in(container);
        out.extend(
            self.auto_open_module_paths
                .iter()
                .filter(|(p, private)| {
                    super::model::is_directly_in(p, container)
                        && (!private || self.container_path.starts_with(container))
                })
                .map(|(p, _)| p.clone()),
        );
        out
    }

    /// The `[<AutoOpen>]` **fragments** declared *directly* in `container`, as
    /// `(path, file)` pairs — the same-file half (this file, at
    /// [`ProjectItems::num_files`], privacy-filtered against the site exactly as
    /// [`Self::project_auto_open_submodules_in`]) plus the already-filtered
    /// earlier-file half ([`ProjectItems::auto_open_fragments_directly_in`]). A
    /// module with fragments in several files appears once per fragment — the
    /// per-fragment provenance the file-ordered fold reads (Stage 5).
    fn auto_open_fragments_directly_in(&self, container: &[String]) -> Vec<(Vec<String>, usize)> {
        let mut out = self.preceding.auto_open_fragments_directly_in(container);
        let current = self.preceding.num_files();
        out.extend(
            self.auto_open_module_paths
                .iter()
                .filter(|(p, private)| {
                    super::model::is_directly_in(p, container)
                        && (!private || self.container_path.starts_with(container))
                })
                .map(|(p, _)| (p.clone(), current)),
        );
        out
    }

    /// Every `[<AutoOpen>]` fragment reachable by opening `namespace`, as
    /// `(module_path, file)` pairs in **Compile-order file order** (Stage 5). This
    /// is the fold list `open <namespace>` walks: each fragment contributes its
    /// *own-file* members (folded at `file`), and a name contested across fragments
    /// resolves by latest file — so the natural push order (this list) already
    /// commits the winner.
    ///
    /// Nesting is **same-file parent-gated**: a nested `[<AutoOpen>] module Q`
    /// lives lexically inside one `module P` block, in that block's file `f`, and
    /// is auto-opened only when *that* block is itself an auto-open fragment — so a
    /// child `(Q, f)` is reached only through a parent fragment `(P, f)` at the
    /// **same** file. A plain `module P` augmentation carrying an `[<AutoOpen>]`
    /// child therefore folds nothing (the child's parent fragment is absent). Only
    /// the top level (directly in the namespace, which spans files) admits
    /// fragments at any file. The final sort is stable, so within a file the
    /// recursion's parent-before-child order (a child folds after its parent)
    /// survives.
    fn auto_open_fragments_reachable(&self, namespace: &[String]) -> Vec<(Vec<String>, usize)> {
        fn collect(
            resolver: &Resolver<'_>,
            path: &[String],
            file: usize,
            out: &mut Vec<(Vec<String>, usize)>,
        ) {
            out.push((path.to_vec(), file));
            // Children of *this* block: fragments directly in `path` at the SAME
            // file `file` (a nested module lives in its parent block's file).
            for (child, cf) in resolver.auto_open_fragments_directly_in(path) {
                if cf == file {
                    collect(resolver, &child, file, out);
                }
            }
        }
        let mut out: Vec<(Vec<String>, usize)> = Vec::new();
        for (path, file) in self.auto_open_fragments_directly_in(namespace) {
            collect(self, &path, file, &mut out);
        }
        out.sort_by_key(|(_, file)| *file);
        out
    }

    /// Whether opening project namespace/module `container` may bring
    /// value-space names we cannot enumerate — [`Self::module_has_hidden_values`]
    /// of its own direct cases, **or** (recursively) of any `[<AutoOpen>]`
    /// submodule it folds in ([`Self::project_auto_open_submodules_in`]). A
    /// namespace's own tycon tier is a plain project export scan and is never
    /// hidden by itself, but a nested `[<AutoOpen>]` MODULE can be (an active
    /// pattern's cases are never cross-file exported at all — see
    /// [`Self::module_has_hidden_values`]'s doc). [`Self::open_project_namespace_values`]
    /// calls this on each child **before** recursing into it (not once, upfront,
    /// for the whole tree — codex review of §7's machinery slice): the barrier
    /// must land between a container's own already-pushed entries and its
    /// hidden child, so the child's unenumerable name can shadow them; bumping
    /// before the container's own push instead would stamp those entries with
    /// the new generation too, and they would never go stale.
    pub(super) fn namespace_fold_has_hidden_values(&self, namespace: &[String]) -> bool {
        self.module_has_hidden_values(namespace)
            || self
                .project_auto_open_submodules_in(namespace)
                .iter()
                .any(|sub| self.namespace_fold_has_hidden_values(sub))
    }

    /// The names of **constructible project types** (`SlotClass::Evicts` /
    /// `Unknown` — a class, struct, enum, or undecidable-kind declaration;
    /// never a plain union/record/interface, `SlotClass::Keeps`) declared
    /// directly under `namespace`, same-file and earlier-file
    /// ([`super::model::ProjectItems::direct_type_contestants`]), plus
    /// (recursively) those of its `[<AutoOpen>]` submodules
    /// ([`Self::project_auto_open_submodules_in`]) — the project-side mirror
    /// of `AssemblyEnv::open_namespace_fold_surfaces`'s `contestant_names`.
    ///
    /// Codex review of §7's machinery slice: a project namespace's own
    /// constructible type takes FCS's unqualified constructor slot exactly
    /// like an assembly namespace's does, so it can EVICT a same-named
    /// *value* from a DIFFERENT surface — a colocated assembly module sharing
    /// the open's FQN. [`open_assembly_module_fold`](Self::open_assembly_module_fold)'s
    /// `collisions()` already demotes a name two different `OpenFoldSurface`s
    /// both supply; the caller pushes these names as a contestant-only surface
    /// (`entries` empty) into that SAME `surfaces` list so a colliding
    /// assembly value defers instead of wrongly staying definite. Not pushed
    /// as resolvable entries themselves: sema does not model project type
    /// members/constructors, so a bare use of the contested name still
    /// defers — sound (never a wrong target), just unavailable, exactly the
    /// concession `docs/assembly-module-open-plan.md` §8 already makes for
    /// the analogous assembly case.
    pub(super) fn project_namespace_contestant_names(&self, namespace: &[String]) -> Vec<String> {
        let mut out = self.direct_project_type_contestants(namespace);
        for sub in self.project_auto_open_submodules_in(namespace) {
            out.extend(self.project_namespace_contestant_names(&sub));
        }
        out
    }

    /// The names of constructible project types (`SlotClass::Evicts` /
    /// `Unknown`) declared **directly** at `container` — same-file and
    /// earlier-file, non-recursive. The one-container piece
    /// [`Self::project_namespace_contestant_names`] flattens across a whole
    /// recursive fold (for the ASSEMBLY-facing contest, where only "is this
    /// name contested by *some* project source" matters, not which one), and
    /// [`Self::open_project_namespace_values`] instead calls this **per
    /// source, in fold order** (codex review round 2): a later-folded
    /// source's own type must evict an EARLIER source's value at exactly that
    /// fold position — flattening first and demoting second, as the
    /// assembly-facing surface does, cannot express "later evicts earlier"
    /// at all, only "some project source contests".
    fn direct_project_type_contestants(&self, container: &[String]) -> Vec<String> {
        let mut out = self.preceding.direct_type_contestants(container);
        out.extend(
            self.type_path_exports
                .iter()
                .filter(|(p, _, slot)| {
                    p.len() == container.len() + 1
                        && p.starts_with(container)
                        && *slot != SlotClass::Keeps
                })
                .map(|(p, _, _)| p.last().expect("non-empty qualified path").clone()),
        );
        out
    }

    /// The winning **direct-tier** contribution [`Self::open_module_values`]
    /// pushes for each name declared directly under `path` (a project
    /// namespace): its project-global [`ItemId`] and declaring Compile-order file
    /// ([`ProjectItems::file_of`]). The value index wins the id where a name is
    /// both a value and a case (expression-latest); the constructor index fills
    /// only names the value index missed. Same-file contributions fold last (the
    /// current file's index, [`ProjectItems::num_files`]), overriding earlier
    /// files. Accessibility (own-/inherited-`private`) is filtered exactly as the
    /// open-fold does, so an inaccessible name never enters the straddle contest.
    fn direct_tier_ids_at(&self, path: &[String]) -> HashMap<String, (ItemId, usize)> {
        let site = self.container_path.clone();
        let mut out: HashMap<String, (ItemId, usize)> = HashMap::new();
        for (name, id) in self.preceding.direct_value_children(path, &site) {
            out.insert(name, (id, self.preceding.file_of(id)));
        }
        for (name, id) in self.preceding.direct_constructor_children(path, &site) {
            out.entry(name)
                .or_insert_with(|| (id, self.preceding.file_of(id)));
        }
        let current = self.preceding.num_files();
        for item in &self.items {
            if let Some(q) = &item.qualified
                && q.len() == path.len() + 1
                && q.starts_with(path)
                && super::model::accessible_from(item.access_root_len, q, &site)
            {
                out.insert(
                    q.last().expect("non-empty qualified path").clone(),
                    (item.id, current),
                );
            }
        }
        out
    }

    /// What any `[<AutoOpen>]` submodule of `path` (recursively over
    /// [`Self::project_auto_open_submodules_in`]) contributes for each name, in
    /// the three dimensions a cross-tier straddle must keep separate (see
    /// [`SubmoduleFold`]) — FCS folds each into its own environment, so a single
    /// per-name maximum conflates them and mis-orders (codex review of the
    /// straddle slice). Compared against [`Self::direct_tier_ids_at`] in
    /// [`Self::open_project_namespace_values`].
    ///
    /// **Per-fragment exact** (Stage 5): the contribution list is
    /// [`Self::auto_open_fragments_reachable`] — every `(submodule, file)`
    /// fragment reachable by opening `path`, with same-file parent-gated nesting —
    /// and each fragment contributes the members it declares *in its own file*, at
    /// that file. This drops the Stage-4 approximations at the root: a plain
    /// `module` augmentation is not a fragment (contributes nothing); an auto-open
    /// fragment's members fold at their true file, never overstated to a later
    /// augmentation's; and a name a module supplies from several files takes the
    /// **latest** such fragment (the file-`max`). With the fold exact, the caller
    /// lets a genuinely later submodule win instead of conservatively deferring.
    fn submodule_contributions_at(&self, path: &[String]) -> HashMap<String, SubmoduleFold> {
        let site = self.container_path.clone();
        let mut out: HashMap<String, SubmoduleFold> = HashMap::new();
        let current = self.preceding.num_files();
        for (sub, file) in self.auto_open_fragments_reachable(path) {
            // The names this fragment declares in *its* file (ids unused here —
            // only the fold position matters): earlier files from the per-file
            // cross-file queries, the current file from `self.items`.
            let mut value_names: Vec<String>;
            let mut case_names: Vec<String>;
            if file < current {
                value_names = Vec::new();
                case_names = self
                    .preceding
                    .fragment_constructor_children(&sub, file, &site)
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect();
                for (name, id) in self.preceding.fragment_value_children(&sub, file, &site) {
                    // A maybe-literal value is a constant-pattern contestant: it
                    // ALSO feeds the `case` dimension (below), so a direct case
                    // does not win the constructor slot merely because the case
                    // index lists no submodule case — FCS folds the submodule's
                    // vals after the direct tycon tier, and a literal there beats
                    // the direct case in bare pattern position (codex round 1).
                    if self.preceding.is_attributed_item(id) {
                        case_names.push(name.clone());
                    }
                    value_names.push(name);
                }
            } else {
                value_names = Vec::new();
                case_names = Vec::new();
                for item in &self.items {
                    if let Some(q) = &item.qualified
                        && q.len() == sub.len() + 1
                        && q.starts_with(sub.as_slice())
                        && super::model::accessible_from(item.access_root_len, q, &site)
                    {
                        let name = q.last().expect("non-empty qualified path").clone();
                        // A case — or a maybe-literal value, a constant-pattern
                        // contestant in the constructor namespace (see the
                        // cross-file branch above).
                        if item.is_case() || item.attributed {
                            case_names.push(name.clone());
                        }
                        value_names.push(name);
                    }
                }
            }
            // Value-namespace children (a value or an unshadowed case), at this
            // fragment's file.
            for name in value_names {
                let e = out.entry(name).or_default();
                e.value_slot = Some(e.value_slot.map_or(file, |v| v.max(file)));
            }
            // Constructor-namespace children (union / exception / active-pattern
            // cases — plus maybe-literal values, constant-pattern contestants,
            // added above) feed the `case` dimension only. A value-live
            // union/exception case is ALSO a value, but it is already in
            // `value_names` (it is a value), so feeding `value_slot` here would be
            // redundant — and it MUST NOT, because a **pattern-only**
            // active-pattern case (Stage 3a) is a case but *not* a value
            // (`fragment_value_children` excludes it), so it would wrongly claim
            // the value slot.
            for name in case_names {
                let e = out.entry(name).or_default();
                e.case = Some(e.case.map_or(file, |c| c.max(file)));
            }
            // A constructible type takes FCS's unqualified value slot, but sema
            // models no project type constructor — so it can only ever DEFER the
            // value dimension, never let the direct case win it. (File-blind — an
            // over-defer is sound.)
            for name in self.direct_project_type_contestants(&sub) {
                out.entry(name).or_default().has_type = true;
            }
        }
        out
    }

    /// The canonical target of a same-file module abbreviation named `name` that
    /// is **lexically in scope** here, or `None`. A module abbreviation is a
    /// *bare-head lexical name*, not a member accessible by qualified path:
    /// `module N = (module X = Target)` makes `X` visible (unqualified) inside `N`
    /// and its nested modules, but `N.X` is **invisible** anywhere (FCS reports
    /// FS0039 — even from a child of `N`). So this matches only against the head of
    /// a reference.
    ///
    /// Lexical shadowing is respected: the **innermost** module-like declaration
    /// of `name` in scope wins, whatever its kind. We find that declaration via
    /// [`module_like_names`](Self::module_like_names) (nested modules *and*
    /// abbreviations) — the longest enclosing container that declares `name` — and
    /// follow it **only if it is a resolvable alias**. If the nearest declaration
    /// is instead a real nested module or an unresolvable abbreviation, `None` is
    /// returned so the ordinary module-path tiers resolve that inner module (or
    /// stay conservative) rather than the shadowed outer alias.
    ///
    /// Alias targets are flattened at definition (the RHS is resolved through
    /// [`resolved_project_module`](Self::resolved_project_module), which itself
    /// follows aliases), so the returned target is never itself an alias — no
    /// recursion or cycle risk. Aliases declared in an *earlier* Compile-order file
    /// are not followed (a rare case; their reference stays conservative).
    pub(super) fn lexical_alias_target(&self, name: &str) -> Option<&[String]> {
        // The innermost enclosing container that declares `name` as module-like
        // (longest prefix of `container_path`) shadows any outer one.
        let scope = (0..=self.container_path.len())
            .rev()
            .map(|k| &self.container_path[..k])
            .find(|scope| {
                self.module_like_names
                    .get(*scope)
                    .is_some_and(|names| names.contains(name))
            })?;
        // Follow it only if that nearest declaration is a resolvable alias.
        let mut alias_path = scope.to_vec();
        alias_path.push(name.to_string());
        self.module_aliases.get(&alias_path).map(Vec::as_slice)
    }

    /// Record the **current container** as a module that brings value-space names
    /// we cannot enumerate (a union case / exception constructor / active pattern,
    /// or — at the caller's discretion — a module alias). See
    /// [`Self::modules_with_hidden_values`].
    pub(super) fn note_hidden_value_module(&mut self, path: Vec<String>) {
        self.modules_with_hidden_values.insert(path);
    }

    /// If `written` (a plain `open` clause's path) names an **in-project
    /// module** — directly, or shortened by an earlier open / the enclosing
    /// namespace — return that module's fully-qualified path, using the exact
    /// project-module predicate [`is_project_module_path`](Self::is_project_module_path)
    /// at each tier. `None` when no tier names a project module — the open is then
    /// a conservative project-module fallback, an assembly type/module, or a
    /// namespace prefix instead.
    ///
    /// Precedence mirrors F#'s "latest open wins". The shortening tiers are tried
    /// most-recent-first so a later open shadows an earlier one when both could
    /// shorten the name (`open A; open B; open Shared` chooses `B.Shared`):
    /// 1. **explicit opens** — every prefix in [`Self::open_shortening_prefixes`]
    ///    (namespace opens *and* chained module opens, one source-ordered list),
    ///    latest first; this is what makes a later namespace open out-rank an
    ///    earlier module open, and `open Shared; open Sub` chain `Sub` to
    ///    `Shared.Sub`;
    /// 2. **enclosing namespace/module nesting**, innermost first;
    /// 3. **root** — the path as written (fully-qualified).
    ///
    /// `rooted` (a `global.`-qualified open) **bypasses the shortening tiers 1–2**
    /// and considers only tier 3: `open global.Root` names the root `Root`, never
    /// an enclosing `N.Root` or a prior open's `Root`.
    ///
    /// A **module abbreviation** is followed when it is the *bare head* of the
    /// reference (tier 0): `open Alias` → `Target` and `open Alias.Sub` →
    /// `Target.Sub`. A qualified path *through* an alias's container (`open
    /// N.Alias`) does not see the alias (FCS FS0039), so it falls to the ordinary
    /// tiers and stays conservative.
    pub(super) fn resolved_project_module(
        &self,
        written: &[String],
        rooted: bool,
    ) -> Option<Vec<String>> {
        let (head, rest) = written.split_first()?;
        if !rooted {
            // Tier 0 — a lexically in-scope module abbreviation as the bare head:
            // rewrite `Alias[.rest]` to `Target[.rest]` (the alias is not a
            // qualified member, so only the head is followed).
            if let Some(target) = self.lexical_alias_target(head) {
                let mut full = target.to_vec();
                full.extend_from_slice(rest);
                return self.is_project_module_path(&full).then_some(full);
            }
            // Tier 1 — explicit opens (namespace + chained module), one
            // source-ordered list, latest open first so the most recent shadowing
            // prefix wins across open kinds.
            for prefix in self.open_shortening_prefixes.iter().rev() {
                let mut full = prefix.clone();
                full.extend_from_slice(written);
                if self.is_project_module_path(&full) {
                    return Some(full);
                }
            }
            // Tier 2 — enclosing namespace/module nesting, innermost first.
            for k in (1..=self.container_path.len()).rev() {
                let mut full = self.container_path[..k].to_vec();
                full.extend_from_slice(written);
                if self.is_project_module_path(&full) {
                    return Some(full);
                }
            }
        }
        // Tier 3 — the path as written (root / fully-qualified).
        if self.is_project_module_path(written) {
            return Some(written.to_vec());
        }
        None
    }

    /// The handle of the **ordinary value** (not a case constructor) exported at
    /// exactly `path` — this file's (the source-latest) or an earlier Compile-order
    /// one. The same-file/cross-file counterpart of
    /// [`ProjectItems::ordinary_value_at`], used to detect a value that shadows a
    /// type-qualified case for the qualifier.
    pub(super) fn ordinary_value_at(&self, path: &[String]) -> Option<ItemId> {
        self.items
            .iter()
            .rev()
            .find(|it| {
                !it.is_case()
                    && it.qualified.as_deref() == Some(path)
                    // A non-`rec` binder being defined is not yet in scope in its own
                    // RHS, so it must not shadow its own qualified self-reference.
                    && !self.pending_items.contains(&it.id)
            })
            .map(|it| it.id)
            .or_else(|| self.preceding.ordinary_value_at(path))
    }

    /// Whether an **ordinary value** is exported at any proper prefix of the
    /// type-qualified case `full` (`[container.., Type, Case]`), in **either** this
    /// file or an earlier one. When so, F# may bind that value and read the rest as
    /// member access on it, so we **defer** rather than resolve the case — sound
    /// (never a wrong target). A same-named union *case constructor* does not count
    /// ([`ordinary_value_at`](Self::ordinary_value_at) skips case ids), so `type
    /// Color = Color | Red; Color.Red` still resolves.
    ///
    /// This is deliberately **order-insensitive** — and that is not a conservative
    /// approximation but the **FCS-faithful behaviour** (Gap C of
    /// `docs/type-qualified-case-prefix-plan.md`, closed by probe). A value and a
    /// type can share a qualified path only when declared in the **same module
    /// block**: the cross-block shapes are illegal (a same-named module in two
    /// files or two blocks is FS0248; a module and a namespace at one path is
    /// FS0247 — an earlier "cross-file augmentation resolves to the case" pin came
    /// from FS0248-illegal source). And in the same-block shape **the value
    /// commits, in every variant**: union *and* enum (the 2-segment "an enum case
    /// beats an *earlier* value" rule does not carry to the qualified form), either
    /// declaration order, expression position (member access on the value — the
    /// use only compiles when the member exists) and pattern position (FS1127).
    /// The same rule as the same-file classifier's dottable-value segment
    /// (`is_dottable_value` → defer). An [`ItemId`]/Compile-order "latest-wins"
    /// comparison here is **wrong**, not just unnecessary — an earlier attempt
    /// produced wrong resolutions; do not reintroduce one.
    ///
    /// A non-`rec` binding's *own* qualified self-reference (`let Color =
    /// Lib.Container.Color.Red`) does **not** count its own binder: that binder is
    /// not in scope in its own RHS, and [`ordinary_value_at`](Self::ordinary_value_at)
    /// skips the [`pending_items`](Self::pending_items) the eager
    /// [`prepare_binding`](Self::prepare_binding) push would otherwise expose, so the
    /// self-reference reaches the earlier case (Gap B of
    /// `docs/type-qualified-case-prefix-plan.md`).
    pub(super) fn value_shadows_case(&self, full: &[String]) -> bool {
        (1..full.len()).any(|k| self.ordinary_value_at(&full[..k]).is_some())
    }

    /// Resolve a cross-file **type-qualified case** reference `written` (the whole
    /// dotted path `[container.., Type, Case]` as written, e.g. `["Color", "Red"]`
    /// under `open Lib`, or `["Lib", "Color", "Red"]` fully-qualified) to the case's
    /// project-global handle in an **earlier** file, or `None`.
    ///
    /// Name-shortens the *whole* path through the same tiers as
    /// [`resolved_project_module`](Self::resolved_project_module) (explicit opens
    /// latest-first, enclosing namespace/module nesting innermost-first, then the
    /// root as written), checking the cross-file
    /// [`ProjectItems::type_qualified_cases`] index at each — so `Color.Red` resolves
    /// relative to an `open Lib` or the enclosing `namespace Lib`, and the
    /// fully-qualified form resolves at the root. Self-validating: only an exact
    /// earlier-file `[…, Type, Case]` path hits, so an unintended shortening simply
    /// misses. The 2-segment same-file `Type.Case` is handled by
    /// [`type_case_path`](Self::type_case_path) before this; a same-file
    /// *module-qualified* `Pal.Color.Red` by
    /// [`classify_same_file_module_qualified_case`](Self::classify_same_file_module_qualified_case)
    /// (Gap A); this consults only `preceding`.
    ///
    /// A candidate whose resolved path is shadowed by an **ordinary value** is
    /// rejected ([`value_shadows_case`](Self::value_shadows_case), checking both this
    /// file and earlier ones): F# may bind the value first and read the rest as
    /// member access on it, so we defer. A union **case constructor** of the same
    /// name (`type Color = Color | Red`) is not a value and does not shadow.
    ///
    /// **Sound completeness gap** (Gap C of
    /// `docs/type-qualified-case-prefix-plan.md`): a value sharing the qualified path
    /// defers the case *regardless of Compile order* (F#'s value-vs-type precedence
    /// at the same path is same-file / cross-file dependent and not order-decidable
    /// without declaration-block provenance, which sema does not model). FCS resolves
    /// some of these to the case; we say nothing (never a wrong target).
    pub(super) fn cross_file_type_case(&self, written: &[String], rooted: bool) -> Option<ItemId> {
        // An `open type T` (modelled or not) sets `unmodelled_open_active`: T's
        // unmodelled nested types could supply the head segment, shadowing the
        // project type for the qualifier (`open Lib; open type Demo.Thing; Inner.Red`
        // binds `Demo.Thing.Inner.Red`, not the project `Lib.Inner.Red`). An unrooted
        // candidate could be redirected by that open, so defer — exactly as the
        // qualified-value branch does (`rooted || !unmodelled_open_active`).
        if !rooted && self.unmodelled_open_active {
            return None;
        }
        let hit = |full: &[String]| -> Option<ItemId> {
            self.preceding
                .type_qualified_case(full, &self.container_path)
                .filter(|_| !self.value_shadows_case(full))
        };
        if !rooted {
            // Tier 0 — a lexically in-scope **module alias** as the bare head:
            // `module P = Lib.Pal; P.Color.Red` is `Lib.Pal.Color.Red`. The alias is
            // *definitive* for the head (it shadows a same-named root — FCS), so
            // resolve only through the target and do not fall through to the tiers
            // below; otherwise a colliding root `module P` would wrongly bind.
            if let Some((head, rest)) = written.split_first()
                && let Some(target) = self.lexical_alias_target(head)
            {
                let mut full = target.to_vec();
                full.extend_from_slice(rest);
                return hit(&full);
            }
            // Tier 1 — explicit opens, latest first.
            for prefix in self.open_shortening_prefixes.iter().rev() {
                let mut full = prefix.clone();
                full.extend_from_slice(written);
                if let Some(id) = hit(&full) {
                    return Some(id);
                }
            }
            // Tier 2 — enclosing namespace/module nesting, innermost first.
            for k in (1..=self.container_path.len()).rev() {
                let mut full = self.container_path[..k].to_vec();
                full.extend_from_slice(written);
                if let Some(id) = hit(&full) {
                    return Some(id);
                }
            }
        }
        // Tier 3 — the path as written (root / fully-qualified).
        hit(written)
    }

    /// Bring the **direct** exported values of project module `module_path` into
    /// the current frame as source-ordered [`ScopeEntry::opened`] entries — the
    /// bare names an `open M` makes resolvable (substep 3). Same-file values come
    /// from [`Self::items`] (a value whose qualified export path is
    /// `[module_path…, name]`, exactly one segment beyond), earlier-file values
    /// from [`ProjectItems::direct_value_children`]; a value nested deeper (in a
    /// submodule) has a longer path and is excluded. A name found in *this* file
    /// wins over a same-named cross-file one (this file augments the earlier
    /// module); both are [`Resolution::Item`]. The entries are
    /// [`ScopeEntry::opened`], so [`lookup`](Self::lookup) gives correct
    /// latest-wins shadowing against locals and [`resolve_file`] does not leak them
    /// across same-named top-level blocks. The module may also hold submodules /
    /// types we do not model, so the caller sets
    /// [`opaque_dotted_open`](Self::opaque_dotted_open) to keep dotted heads
    /// through it conservative. Returns the number of values enumerated — zero
    /// means an empty / submodule-only module or a module *alias* (whose target we
    /// cannot see), which the caller treats fully conservatively.
    ///
    /// `fragment_file` restricts the fold to the members `module_path` declares in
    /// **one** Compile-order file — set by
    /// [`open_project_namespace_values`](Self::open_project_namespace_values) when
    /// folding a single `[<AutoOpen>]` fragment reached implicitly by opening the
    /// enclosing namespace, so a plain augmentation (a different file) or a
    /// later-file redefinition contributes nothing (Stage 5). A plain, explicit
    /// `open M` passes `None` (every member of every fragment folds).
    pub(super) fn open_module_values(
        &mut self,
        module_path: &[String],
        open_pos: u32,
        fragment_file: Option<usize>,
    ) -> usize {
        // Collect first — the scans borrow `self.items` / `self.preceding`
        // immutably, while the push below borrows `self.scopes` mutably.
        let generation = self.open_generation;
        // The reference site (this `open`'s enclosing container) decides whether a
        // `private` value of `module_path` is accessible — F# hides it from an
        // `open` outside the container's subtree (oracle-pinned; on `main` such a
        // value wrongly resolved cross-file).
        let site = self.container_path.clone();
        // Stage 5 per-fragment gate: folding a single `[<AutoOpen>]` fragment
        // brings in only the members `module_path` declares in that fragment's
        // file. Same-file members share the current file (one check); cross-file
        // members are gated per id by their own `file_of`.
        let current_file = self.preceding.num_files();
        let same_file_folds = fragment_file.is_none_or(|f| f == current_file);
        // A module whose `open` brings value-space names we cannot enumerate (an
        // active pattern, an alias, …) is **hidden**: its own exported cases cannot
        // be *trusted in pattern position*, because a hidden constructor of the same
        // name could shadow them (FCS: `open M; match x with Red` picks `M`'s active
        // pattern `(|Red|_|)` over its union case `Red`). They still resolve as
        // *values* in expression position; only their pattern use is suppressed
        // ([`Self::pattern_suppressed_case_ids`], consulted by
        // [`case_reference`](Self::case_reference)).
        let hidden = self.module_has_hidden_values(module_path);
        let mut suppressed: Vec<ItemId> = Vec::new();
        // Two independent namespace projections over the same source order (see the
        // plan): the *value* projection (`seen_values`, for `lookup`) and the
        // *constructor* projection (`seen_ctors` — same-file **case** names — for
        // `case_reference`). `pushed_normal` tracks ids already pushed as ordinary
        // entries so an unshadowed case (which serves both namespaces) is not also
        // given a redundant `pattern_only` entry.
        let mut seen_values: HashSet<String> = HashSet::new();
        let mut seen_ctors: HashSet<String> = HashSet::new();
        let mut pushed_normal: HashSet<ItemId> = HashSet::new();
        let mut entries: Vec<ScopeEntry> = Vec::new();
        // The opened module's own **maybe-literal** value names (this file's
        // members, gated exactly like the same-file pass below). FCS folds a
        // module's vals *after* its tycons (exceptions → tycons → vals), so a
        // maybe-literal value constant-shadows the module's own same-named case
        // in bare pattern position REGARDLESS of source order — every case push
        // below checks this set plus the cross-file history
        // ([`ProjectItems::module_value_may_be_constant_pattern`]) and defers
        // the case via [`Self::pattern_suppressed_case_ids`]. An inaccessible
        // value is filtered exactly as FCS filters the opened environment, so
        // the case then stays committed. (For a single-fragment fold the
        // cross-file history spans every file, over-approximating the
        // fragment's own members — a wider defer, never a commit.)
        let mut constant_names: HashSet<String> = HashSet::new();
        for item in &self.items {
            if item.attributed
                && let Some(q) = &item.qualified
                && q.len() == module_path.len() + 1
                && q.starts_with(module_path)
                && same_file_folds
                && super::model::accessible_from(item.access_root_len, q, &site)
            {
                constant_names.insert(q.last().expect("non-empty qualified path").clone());
            }
        }
        // Same-file pass: push every same-file `[M, …]` child (value **or** case) as
        // an ordinary entry — it serves both namespaces (a union case is a value
        // too). Record value names in `seen_values`, same-file case names in
        // `seen_ctors`. No same-file dedup: a name exported twice at a path (a case
        // and a later same-named `let`) keeps both, so latest-wins `lookup` picks
        // the later one in expression position (mirroring the in-file frame).
        for item in &self.items {
            if let Some(q) = &item.qualified
                && q.len() == module_path.len() + 1
                && q.starts_with(module_path)
                && same_file_folds
                && super::model::accessible_from(item.access_root_len, q, &site)
            {
                let name = q.last().expect("non-empty qualified path").clone();
                seen_values.insert(name.clone());
                if item.is_case() {
                    seen_ctors.insert(name.clone());
                    if hidden
                        || constant_names.contains(&name)
                        || self
                            .preceding
                            .module_value_may_be_constant_pattern(q, &site)
                    {
                        suppressed.push(item.id);
                    }
                }
                pushed_normal.insert(item.id);
                let mut entry =
                    ScopeEntry::opened(name, Resolution::Item(item.id), generation, open_pos);
                entry.maybe_constant_pattern = item.attributed;
                entries.push(entry);
            }
        }
        // The cross-file member sets. A plain `open M` (`fragment_file == None`)
        // takes the latest **accessible** export per path across every earlier
        // file ([`direct_value_children`] — an inaccessible `private` value
        // omitted, a public export under a later inaccessible `private` recovered).
        // A single `[<AutoOpen>]` fragment at earlier-file `f` takes the members
        // *declared in `f`* ([`fragment_value_children`]) — NOT the collapsed
        // latest, so a later plain augmentation neither hides this fragment's own
        // member nor substitutes its own. A current-file fragment
        // (`f == current_file`) has no earlier-file members, so both sets are empty
        // (its members come from the same-file pass above).
        let (value_children, ctor_children) = match fragment_file {
            None => (
                self.preceding.direct_value_children(module_path, &site),
                self.preceding
                    .direct_constructor_children(module_path, &site),
            ),
            Some(f) if f < current_file => (
                self.preceding
                    .fragment_value_children(module_path, f, &site),
                self.preceding
                    .fragment_constructor_children(module_path, f, &site),
            ),
            Some(_) => (Vec::new(), Vec::new()),
        };
        // Cross-file *value* pass (for expression position), dedup against
        // `seen_values`.
        for (name, id) in value_children {
            if seen_values.insert(name.clone()) {
                if self.preceding.is_case_item(id) {
                    let mut q = module_path.to_vec();
                    q.push(name.clone());
                    if hidden
                        || constant_names.contains(&name)
                        || self
                            .preceding
                            .module_value_may_be_constant_pattern(&q, &site)
                    {
                        suppressed.push(id);
                    }
                }
                pushed_normal.insert(id);
                let mut entry =
                    ScopeEntry::opened(name, Resolution::Item(id), generation, open_pos);
                entry.maybe_constant_pattern = self.preceding.is_attributed_item(id);
                entries.push(entry);
            }
        }
        // Cross-file *constructor* pass (for pattern position) — **independent** of
        // `seen_values`, so a same-file `let` does not block a cross-file case
        // (High-2). Push a `pattern_only` entry for each cross-file case not already
        // pushed as an ordinary entry (an unshadowed case is its own value entry)
        // and not shadowed by a same-file case (`seen_ctors`). A hidden module's
        // pattern entries are suppressed too (High-1: an unenumerable active pattern
        // could shadow them).
        for (name, id) in ctor_children {
            if !seen_ctors.contains(&name) && pushed_normal.insert(id) {
                let mut q = module_path.to_vec();
                q.push(name.clone());
                if hidden
                    || constant_names.contains(&name)
                    || self
                        .preceding
                        .module_value_may_be_constant_pattern(&q, &site)
                {
                    suppressed.push(id);
                }
                entries.push(ScopeEntry::opened_pattern_only(
                    name,
                    Resolution::Item(id),
                    generation,
                    open_pos,
                ));
            }
        }
        let count = entries.len();
        self.module_frame().entries.extend(entries);
        self.pattern_suppressed_case_ids.extend(suppressed);
        count
    }

    /// Bring a project **namespace**'s direct cases/exceptions into scope —
    /// [`Self::open_module_values`] on `namespace` itself, since F# forbids
    /// values at namespace scope, its direct exports (if any) are exactly the
    /// tycon tier — then recurse into its direct `[<AutoOpen>]` submodules
    /// ([`Self::project_auto_open_submodules_in`], Compile-order: earlier
    /// files first, this file's own last), each folded the same way and in
    /// turn recursed into. This is the project-side mirror of
    /// `AssemblyEnv::open_namespace_fold_surfaces`'s tycon-tier-then-auto-open
    /// recursion (`docs/assembly-module-open-plan.md`, §7's "machinery"
    /// slice): every push shares the namespace open's own `open_pos`, so the
    /// whole recursive surface behaves as one open and a name two levels
    /// supply is ordered purely by push position — deepest/latest wins,
    /// matching FCS folding a submodule's contents after its parent's.
    ///
    /// The barrier for a hidden child is raised **here, per child, right
    /// before recursing into it** — not once upfront by the caller for the
    /// whole tree (codex review of §7's machinery slice: bumping upfront would
    /// stamp `namespace`'s own just-pushed entries with the bumped generation
    /// too, so a LATER hidden grandchild's unenumerable name could never make
    /// them go stale, and a case `case_reference` should defer would instead
    /// commit). Checking [`Self::namespace_fold_has_hidden_values`] on each
    /// child individually — not the umbrella `namespace` — means the bump
    /// lands exactly between "everything folded so far" and "this hidden
    /// child", covering `namespace`'s own entries and any earlier sibling but
    /// never a later one.
    ///
    /// **Not** used for a plain project *module* open (`has_project_module` in
    /// `decls.rs`'s `ModuleDecl::Open` arm still calls
    /// [`Self::open_module_values`] directly) — recursing there is a separate,
    /// unscoped gap this slice does not touch (`docs/assembly-module-open-plan.md`
    /// §7 only prices the namespace flavor).
    ///
    /// Before recursing into each `[<AutoOpen>]` submodule, this pushes a
    /// `Deferred` override for that submodule's own constructible type names
    /// (codex review round 2): a later-folded submodule's type takes FCS's
    /// unqualified constructor slot and evicts an EARLIER sibling's
    /// same-named value — `Clash () = 1` in one auto-open sibling, folded
    /// before `type Clash()` in the next, binds the type after `open N`,
    /// fcs-dump-verified. Doing this right **before recursing into that
    /// submodule** (not, say, once upfront for the whole tree) is what keeps
    /// a genuine same-container tie sound the other way: F#'s tycon tier
    /// folds before that SAME container's own vals, so the submodule's own
    /// value, pushed by the recursive call straight after, still wins by
    /// position — only an EARLIER sibling's already-pushed value is what
    /// this override can reach. Sema does not model project type
    /// constructors, so the override never claims the type as a target —
    /// just declines, same concession as
    /// [`Self::project_namespace_contestant_names`].
    ///
    /// **Not** applied to `namespace` itself at the top of this function**:**
    /// a namespace's own constructible type evicting an unrelated LOCAL value
    /// is already the pre-existing `head_value_slot`/`SlotClass` eviction
    /// machinery's job (`resolve_type_members.rs`'s `an_open_supplied_type_evicts_at_the_opens_position`
    /// and friends) — pushing a second, blunter override for the SAME name
    /// there doesn't just duplicate it, it interferes (codex's fix, applied
    /// unconditionally at every recursion level including the top, regressed
    /// those tests). This override exists only for the type/value contest
    /// this slice's recursion newly makes reachable — BETWEEN two project
    /// sources inside one recursive namespace fold — which has no other
    /// mechanism watching it.
    ///
    /// **A cross-tier name straddling `namespace`'s own direct tier and one of
    /// its auto-open submodules is resolved by per-name Compile-order provenance**
    /// (oracle-pinned): the latest declaring file wins, with a file's auto-open
    /// submodules folding after its own direct tier. `open_module_values(namespace,
    /// ..)` below pushes the whole direct tier first and the submodule loop after,
    /// so a submodule always wins by push position — correct EXCEPT when the
    /// direct tier's file is later, when the direct winner is re-pushed at the END
    /// to out-position the submodule pushes. [`Self::direct_tier_ids_at`] /
    /// [`Self::submodule_contributions_at`] carry each contribution's file
    /// ([`ProjectItems::file_of`]); the decision is per FCS environment (value /
    /// constructor / type-eviction, folded independently).
    ///
    /// With [`Self::submodule_contributions_at`] now **per-fragment exact** (Stage
    /// 5 — auto-open-only members at their true files), the fold both directions:
    /// when the direct tier out-files every submodule contribution it wins; when a
    /// submodule genuinely out-files the direct tier its natural push already
    /// commits it (**S1**, an earlier direct case losing to a later auto-open
    /// submodule value — previously a conservative defer for lack of surface
    /// provenance). A **same-file** straddle is not deferred either: within one
    /// file FCS folds the direct tier before its auto-open fragments,
    /// block-order-independently (fcs-dump probes A/B/D/E/F/G), so the tie breaks to
    /// the submodule exactly as a strictly-later submodule file does — the value
    /// comparison is `>=` and the direct-winner comparisons strict `>`. A hidden
    /// fold (an unenumerable value producer — `extern`, an active pattern) and a
    /// value slot a constructible type may own still **defer** (sound). Gated on
    /// [`Self::is_project_namespace_path`] — a plain project MODULE is one
    /// declaration site, so it has no multi-file fragment to interleave and the
    /// check is skipped for every recursive (submodule) call, where `namespace` is
    /// always a module.
    pub(super) fn open_project_namespace_values(
        &mut self,
        namespace: &[String],
        open_pos: u32,
    ) -> usize {
        // Cross-tier straddle: a name declared BOTH at this namespace's own
        // direct tier and by one of its `[<AutoOpen>]` submodules is folded by
        // FCS per file in Compile order, and *within* each file the direct tier
        // folds before that file's auto-open submodules (block-order-independently)
        // — so the **latest file** wins, and a same-file tie goes to the submodule.
        // `open_module_values` below pushes the whole direct tier first and the
        // submodule loop after it, so a submodule always wins by push position —
        // correct EXCEPT when the direct tier's file is strictly later than every
        // colliding submodule's, when FCS binds the direct case. Decide via
        // per-name file provenance
        // ([`Self::direct_tier_ids_at`] / [`Self::submodule_contributions_at`]),
        // separately for the value namespace, the constructor namespace, and the
        // type-eviction slot (FCS folds each independently — one conflated maximum
        // mis-orders them). A direct winner is re-pushed at the END to out-position
        // the submodule pushes; a submodule winner needs no action.
        //
        // Gated on [`Self::is_project_namespace_path`] — a plain project MODULE is
        // one file / one declaration, no multi-file fragment to interleave. The
        // same predicate distinguishes the top-level (namespace) call from the
        // recursive (submodule) ones, which is exactly when the direct fold below
        // must restrict to `[<AutoOpen>]` fragments: a namespace's own tycon tier
        // is always brought in, a submodule's members only through auto-open
        // fragments.
        let is_namespace = self.is_project_namespace_path(namespace);
        let direct_tier = if is_namespace {
            self.direct_tier_ids_at(namespace)
        } else {
            HashMap::new()
        };
        // Straddle outcomes, pushed after the fold so they out-position the
        // submodule pushes: `value_winners` (direct case wins the value slot
        // outright — one ordinary entry serves both namespaces); `ctor_winners`
        // (direct case wins only the constructor namespace — a pattern-only
        // entry); `value_deferrals` (a constructible type may own the value slot,
        // which sema cannot model — defer the expression, the pattern still
        // decided by the constructor outcome); `deferred_straddles` (a hidden fold
        // value, or a same-file straddle we cannot order by file index — defer
        // both).
        let mut value_winners: Vec<(String, ItemId)> = Vec::new();
        let mut ctor_winners: Vec<(String, ItemId)> = Vec::new();
        let mut value_deferrals: Vec<String> = Vec::new();
        let mut deferred_straddles: Vec<String> = Vec::new();
        if !direct_tier.is_empty() {
            let contributions = self.submodule_contributions_at(namespace);
            let fold_hidden = self.namespace_fold_has_hidden_values(namespace);
            // A constructible type directly under the namespace takes FCS's value
            // slot exactly as a submodule's does — and is equally file-blind here.
            let direct_types: HashSet<String> = self
                .direct_project_type_contestants(namespace)
                .into_iter()
                .collect();
            for (name, (id, direct_file)) in direct_tier {
                let Some(sub) = contributions.get(&name) else {
                    continue; // not a straddle — no submodule supplies this name
                };
                if fold_hidden {
                    // A hidden submodule (active pattern, alias, `[<AutoOpen>]`
                    // type) may supply the straddle name through a channel we can
                    // neither enumerate nor order, and its generation barrier
                    // (raised in the loop below) stales the direct push — so DEFER.
                    deferred_straddles.push(name);
                    continue;
                }
                // The submodule contributions carry each member's *true* fold
                // position — its own auto-open fragment's file
                // ([`Self::submodule_contributions_at`] is per-fragment exact) — so
                // both namespaces are ordered by a straight file comparison against
                // the direct tier. Within a *single* file FCS folds the direct tier
                // BEFORE its auto-open fragments, block-order-independently
                // (fcs-dump probes A/B/D/E/F/G) — so a same-file tie (`==`) breaks
                // to the submodule exactly as a strictly-later submodule file does.
                // This is why the value comparison is `>=` (submodule wins ties) and
                // the direct-winner comparisons are strict `>` (direct wins only by
                // a strictly later file).
                let type_present = sub.has_type || direct_types.contains(&name);

                // The two namespaces are decided INDEPENDENTLY (FCS folds each on
                // its own). The direct case wins a namespace only by strictly
                // out-filing every submodule contribution to it — a same-file tie
                // goes to the submodule, which folds after the direct tier within a
                // file. `value_slot` and `case` can now DIFFER: a value-live
                // union/exception case feeds both, but a **pattern-only**
                // active-pattern case (Stage 3a) feeds `case` only (it is not a
                // value), so the direct case can win the value slot while the
                // submodule's active pattern wins the constructor slot.
                let direct_wins_value =
                    !type_present && sub.value_slot.is_none_or(|v| direct_file > v);
                let direct_wins_ctor = sub.case.is_none_or(|c| direct_file > c);
                if direct_wins_value && direct_wins_ctor {
                    // One ordinary re-push serves both namespaces (the direct value
                    // is also a case, and it out-files the submodule in both).
                    value_winners.push((name, id));
                    continue;
                }
                // Constructor namespace: the direct case wins by a pattern-only
                // re-push (so it does not clobber the value slot the submodule /
                // direct value owns); else the submodule case wins by its natural
                // (later) push. Types never contest the constructor namespace.
                if direct_wins_ctor {
                    ctor_winners.push((name.clone(), id));
                }
                // Value namespace.
                if !direct_wins_value {
                    // A constructible type may own the slot unmodelled → defer;
                    // otherwise a submodule value wins by its natural push (no
                    // action).
                    let submodule_wins_value =
                        !type_present && sub.value_slot.is_some_and(|v| v >= direct_file);
                    if !submodule_wins_value {
                        value_deferrals.push(name.clone());
                    }
                } else if !direct_wins_ctor && sub.value_slot.is_some() {
                    // The direct case wins the value slot but LOSES the constructor
                    // slot to the submodule (a pattern-only active pattern). When the
                    // submodule has no value (`value_slot` is None — the common case,
                    // e.g. an `[<AutoOpen>]` active pattern over a direct exception),
                    // the direct value already wins by its earlier direct-tier push,
                    // and re-pushing it would clobber the submodule's constructor
                    // entry — so do nothing. But when the submodule ALSO has a real
                    // (earlier) value, that value's later frame push would out-position
                    // the direct value, which should win — and a value-only re-push is
                    // not expressible, so DEFER the value (a sound over-defer of an
                    // exotic collision; the constructor namespace still resolves).
                    value_deferrals.push(name.clone());
                }
            }
        }

        // The namespace's own direct tier folds first (all of it — a namespace is
        // not itself auto-open-fragmented). Then every reachable `[<AutoOpen>]`
        // fragment folds **in Compile-order file order**
        // ([`Self::auto_open_fragments_reachable`]), each contributing only its
        // own-file members. A name contested across fragments therefore has its
        // latest-file contribution pushed last, so it wins by push position —
        // matching FCS's latest-file rule without any per-path re-push. (The old
        // per-module-path recursion folded all of a module's members at the
        // module's list position, mis-ordering multi-file/nested fragments.)
        let mut count = self.open_module_values(namespace, open_pos, None);
        for (frag_path, frag_file) in self.auto_open_fragments_reachable(namespace) {
            // A constructible type in this fragment takes FCS's unqualified slot,
            // evicting an EARLIER-folded sibling's same-named value; push a
            // `Deferred` override for its type names before folding it (sema models
            // no project type constructor, so it only declines — never a target).
            let evicting_types = self.direct_project_type_contestants(&frag_path);
            if !evicting_types.is_empty() {
                let generation = self.open_generation;
                let entries: Vec<ScopeEntry> = evicting_types
                    .into_iter()
                    .map(|name| {
                        ScopeEntry::opened(
                            name,
                            Resolution::Deferred(DeferredReason::UnboundName),
                            generation,
                            open_pos,
                        )
                    })
                    .collect();
                self.module_frame().entries.extend(entries);
            }
            // A fragment bringing value-space names we cannot enumerate (an active
            // pattern, an alias, an `extern`) bumps the generation before it folds,
            // so its unenumerable name shadows (stales) everything folded earlier.
            if self.module_has_hidden_values(&frag_path) {
                self.open_generation += 1;
            }
            count += self.open_module_values(&frag_path, open_pos, Some(frag_file));
        }
        // The straddle winners, re-pushed last so they out-position the submodule
        // pushes. `value_winners` / `ctor_winners` are non-empty only when
        // `fold_hidden` was false, so no barrier was raised in the loop and this
        // carries the same generation as the direct push — never stale.
        let generation = self.open_generation;
        for (name, id) in value_winners {
            // Ordinary entry: a case serves the value AND constructor namespaces.
            // A maybe-literal winner keeps its constant-pattern flag; a case
            // winner's constant-shadow suppression (if any) already fired
            // id-keyed when its fragment folded, and survives this re-push.
            let mut entry = ScopeEntry::opened(name, Resolution::Item(id), generation, open_pos);
            entry.maybe_constant_pattern = self.item_is_attributed(id);
            self.module_frame().entries.push(entry);
            count += 1;
        }
        for (name, id) in ctor_winners {
            // Pattern-only: wins the constructor namespace without disturbing the
            // value namespace (a later submodule value / type owns that slot).
            self.module_frame()
                .entries
                .push(ScopeEntry::opened_pattern_only(
                    name,
                    Resolution::Item(id),
                    generation,
                    open_pos,
                ));
            count += 1;
        }
        for name in value_deferrals {
            // Value-namespace-only deferral: an ordinary `Deferred` (NOT
            // `opened_case`, NOT `pattern_only`) shadows the expression slot so a
            // use defers, while `case_reference` reads it as a non-case and scans
            // past it to the constructor outcome — so a pattern the type does not
            // contest still resolves.
            self.module_frame().entries.push(ScopeEntry::opened(
                name,
                Resolution::Deferred(DeferredReason::UnboundName),
                generation,
                open_pos,
            ));
            count += 1;
        }
        for name in deferred_straddles {
            // Defer both namespaces. Marked a case (`case_classification` reads a
            // bare `Deferred` as a definitely-non-case value, so an unmarked
            // override would let `case_reference` scan straight past it to an
            // earlier submodule's real case); `opened_case = true` short-circuits
            // it — sound even between two plain values, since Deferred is never
            // wrong.
            let mut entry = ScopeEntry::opened(
                name,
                Resolution::Deferred(DeferredReason::UnboundName),
                generation,
                open_pos,
            );
            entry.opened_case = true;
            self.module_frame().entries.push(entry);
            count += 1;
        }
        count
    }

    /// The [`ItemId`] of the value `value` exported **directly** by project module
    /// `module_path` (its qualified export path is exactly `[module_path…,
    /// value]`), or `None`. Searches this file's [`Self::items`] first, then
    /// earlier Compile-order files ([`ProjectItems::lookup_qualified_path`]) — the
    /// value an already-name-shortened `Mod.value` resolves to. Only `let` values
    /// are recorded with qualified paths, so a `value` that is actually a nested
    /// module / type yields `None` (the path then defers, never a wrong member).
    pub(super) fn qualified_value_in(&self, module_path: &[String], value: &str) -> Option<ItemId> {
        let mut full = module_path.to_vec();
        full.push(value.to_string());
        // `last`, not `find`: a same-file name can be exported more than once at
        // the same path — a union case and a later `let` of the same name (`type T
        // = Red`; `let Red = 0`). F# resolves the *latest* such binding in
        // expression position, and `self.items` is source-ordered, so take the last
        // match before falling back to an earlier file's export.
        self.items
            .iter()
            .rfind(|i| {
                i.qualified.as_deref() == Some(full.as_slice())
                    // Accessibility-gate the same-file qualified value exactly as the
                    // cross-file `lookup_qualified_path` does: a `let private` value
                    // is invisible to a *sibling* module (FCS FS1094). `rfind` takes
                    // the source-latest *accessible* match, so a public value shadowed
                    // by a later inaccessible `private` one is still recovered.
                    && super::model::accessible_from(
                        i.access_root_len,
                        &full,
                        &self.container_path,
                    )
            })
            .map(|i| i.id)
            .or_else(|| {
                self.preceding
                    .lookup_qualified_path(&full, &self.container_path)
            })
    }

    /// Whether a same-file companion value at the exact path `full` is **provably
    /// inaccessible** from the reference site: at least one same-file item is keyed to
    /// `full` and *every* such item is inaccessible. Only then may the companion
    /// branch treat the value as transparent and continue the candidate walk.
    ///
    /// The key word is *provably*. A binding inside a nested module of a **headerless**
    /// file carries no `qualified` path (`ExportedItem::qualified == None`), so its
    /// accessibility cannot be decided here at all — there is no item keyed to `full`,
    /// this returns `false`, and the caller keeps the sound `Miss` delegation rather
    /// than stepping the walk over a possibly-accessible value onto a farther
    /// candidate. This mirrors [`qualified_value_in`](Self::qualified_value_in), which
    /// likewise binds a same-file value only through `qualified` and otherwise falls to
    /// the cross-file path.
    fn companion_value_provably_inaccessible(&self, full: &[String]) -> bool {
        let mut keyed = self
            .items
            .iter()
            .filter(|i| i.qualified.as_deref() == Some(full))
            .peekable();
        keyed.peek().is_some()
            && keyed.all(|i| {
                !super::model::accessible_from(i.access_root_len, full, &self.container_path)
            })
    }

    /// If a *constructor case* named `name` is in scope — a union case, an
    /// exception constructor, or an active-pattern case (an entry pointing at its
    /// recognizer) — return its resolution: a constructor-shaped pattern head
    /// naming it is a case *reference*, not a binder. In **pattern** position (the
    /// only caller) a case is resolved through F#'s constructor namespace, which
    /// **plain** values do not enter, so a same-named unattributed value does
    /// **not** shadow the case; a *maybe-literal* (attributed / assembly
    /// constant-pattern) value DOES contest it and defers the reference (see
    /// [`ScopeEntry::maybe_constant_pattern`]) — unlike an *expression* use, where
    /// [`resolve_name_use`](Self::resolve_name_use)'s [`lookup`](Self::lookup)
    /// lets the latest binding (value or case) win. So this scans the frames for
    /// the latest *case* entry, skipping value / parameter entries rather than
    /// stopping at the first name match the way `lookup` does. `None` when no case
    /// is in scope (including a require-qualified case — never added — or one in a
    /// sibling namespace), so a caller keeps the decline-and-drop behaviour for a
    /// genuine maybe-var head.
    pub(super) fn case_reference(&self, name: &str) -> Option<Resolution> {
        // A bare head: a maybe-literal value met before the case defers it
        // (`constant_shadow_defers` — FCS's `ePatItems` holds literal values
        // too, latest-wins).
        self.case_reference_entry(name, true)
            .map(|entry| entry.resolution)
    }

    /// Like [`case_reference`](Self::case_reference) but also reports the
    /// recognizer [`ActivePatternShape`] carried on the matched scope entry —
    /// non-`None` only for an opened **assembly** active-pattern tag, whose
    /// `Deferred` resolution has no identity for
    /// [`resolution_active_pattern_shape`](Self::resolution_active_pattern_shape)
    /// to key on (Stage 3b). The applied-head split combines the two: this
    /// entry-carried shape for an assembly case, the resolution-keyed one for a
    /// same-file / cross-file `Item` / `Local` case.
    pub(super) fn applied_active_pattern_case(
        &self,
        name: &str,
    ) -> Option<(Resolution, Option<ActivePatternShape>)> {
        // An *applied* head is exempt from the constant-pattern contest: a
        // literal pattern takes no arguments (FS3191), so on a clean program an
        // applied head is never the literal — the case reading is the only
        // legal one, and committing it stays exact.
        self.case_reference_entry(name, false)
            .map(|entry| (entry.resolution, entry.opened_ap_shape))
    }

    /// The scope entry [`case_reference`](Self::case_reference) resolves a
    /// pattern-position `name` to — the latest in-scope **case** entry (values do
    /// not shadow a case in the constructor namespace), or `None` when none is in
    /// scope or a hidden/stale/opaque open forces a deferral. Split out so both
    /// `case_reference` and [`applied_active_pattern_case`](Self::applied_active_pattern_case)
    /// see the *same* entry — no duplicate scan to drift.
    fn case_reference_entry(
        &self,
        name: &str,
        constant_shadow_defers: bool,
    ) -> Option<&ScopeEntry> {
        let name = id_text(name);
        for frame in self.scopes.iter().rev() {
            for entry in frame.entries.iter().rev() {
                if entry.name != name {
                    continue;
                }
                // An opened entry that an unmodelled open could shadow with a *case*
                // (F#: the latest open wins) cannot be trusted in pattern position —
                // defer rather than return a possibly-shadowed case (a wrong
                // go-to-def). This mirrors [`lookup`](Self::lookup)'s expression-side
                // conservatism: skip an opened entry while an
                // [`opaque_value_open`](Self::opaque_value_open) (a plain `open
                // <assembly module>` / opaque `open type`, whose unenumerable names
                // may include a shadowing constructor) is in scope, or when it is
                // stale (a later hidden open bumped the generation).
                if entry.from_open && self.opaque_value_open {
                    return None;
                }
                if entry.generation != self.open_generation {
                    // Stale — a later residue-bearing open may shadow it, a
                    // project case included (codex round 22): defer.
                    return None;
                }
                // An opened cross-file case from a *hidden* module is not trustworthy
                // in pattern position — a hidden constructor of the same name could
                // shadow it ([`Self::pattern_suppressed_case_ids`]) — so defer. This
                // also covers the value-shadowed case's `pattern_only` entry from a
                // hidden module (it is added there too).
                if let Resolution::Item(id) = entry.resolution
                    && self.pattern_suppressed_case_ids.contains(&id)
                {
                    return None;
                }
                // An opened *assembly* case — a folded union case, exception
                // constructor, or active-pattern tag ([`ScopeEntry::opened_case`]).
                // It has no def to classify; the fold already knows it is a case.
                // Its resolution may be `Deferred` (an opaque case): still a case
                // *reference* — the name provably occupies the constructor
                // namespace — just one with no committed target.
                if entry.opened_case {
                    return Some(entry);
                }
                match self.case_classification(entry.resolution) {
                    Some(true) => return Some(entry),
                    // A **maybe-literal** value met before the case: a literal is
                    // a *constant pattern*, which DOES contest the constructor
                    // namespace (FCS's `ePatItems` holds cases and literal values,
                    // latest-wins), so an earlier case may be constant-shadowed —
                    // defer. Bare heads only: an applied literal pattern is
                    // FS3191-illegal, so the applied path keeps the case.
                    Some(false) if constant_shadow_defers && entry.maybe_constant_pattern => {
                        return None;
                    }
                    // A **plain** value does not shadow a case in the constructor
                    // namespace (and an unattributed `let` provably cannot be a
                    // literal): keep scanning for an earlier case.
                    Some(false) => continue,
                    // Unclassifiable (cross-file `Item`): could be a shadowing case.
                    None => return None,
                }
            }
        }
        None
    }

    /// Entry-aware [`Self::case_classification`] of the **latest in-scope
    /// binding** of `head`: `Some(true)` = definitely a constructor case,
    /// `Some(false)` = definitely an ordinary value, `None` = nothing in scope
    /// or unclassifiable. The wrapper exists because an opened *assembly* case
    /// ([`ScopeEntry::opened_case`]) is a case whatever its [`Resolution`]
    /// says — an opaque one carries `Deferred`, which classification of the
    /// resolution alone would misread as a definite value, letting a folded
    /// union case take a `Color.Red` qualifier as if it were a dottable value.
    pub(super) fn head_case_classification(&self, head: &str) -> Option<bool> {
        let (opened_case, res) = {
            let entry = self.lookup_entry(head)?;
            (entry.opened_case, entry.resolution)
        };
        if opened_case {
            return Some(true);
        }
        self.case_classification(res)
    }

    /// Whether `res` names a constructor **case** — a union case, exception
    /// constructor, or active-pattern case — by its [`DefKind`]: `Some(true)` for a
    /// case, `Some(false)` for a definite non-case (an ordinary value / opened
    /// static member), `None` when it cannot be classified here. A
    /// [`Resolution::Local`], a [`Resolution::Item`] (same-file via the def arena,
    /// cross-file via [`ProjectItems::is_case_item`]), and an opened static
    /// ([`Resolution::Member`] / overloaded-static [`Resolution::Deferred`]) are all
    /// classifiable. `None` only for an out-of-range / unmapped handle.
    pub(super) fn case_classification(&self, res: Resolution) -> Option<bool> {
        let def = match res {
            Resolution::Local(id) => id,
            Resolution::Item(id) => match id.index().checked_sub(self.item_base as usize) {
                // Same-file: classify via this file's def arena.
                Some(local) => self.items.get(local)?.def,
                // Cross-file: its def lives in an earlier file's arena, so consult
                // the exported case-id set instead.
                None => return Some(self.preceding.is_case_item(id)),
            },
            // An opened type static (`Member`, or `Deferred` for an overloaded one)
            // and any other in-scope resolution are definitely *not* constructor
            // cases — they are values, so member access applies (`Some(false)`).
            Resolution::Member { .. }
            | Resolution::Deferred(_)
            | Resolution::Entity(_)
            | Resolution::Unresolved => return Some(false),
        };
        Some(matches!(
            self.defs[def.index()].kind,
            DefKind::UnionCase | DefKind::ExceptionCase | DefKind::ActivePattern
        ))
    }

    /// Whether the module-level value behind `id` is **attributed** — a
    /// maybe-literal constant-pattern contestant
    /// ([`ExportedItem::attributed`](super::model::ExportedItem)): same-file via
    /// this file's export arena, cross-file via
    /// [`ProjectItems::is_attributed_item`].
    fn item_is_attributed(&self, id: ItemId) -> bool {
        match id.index().checked_sub(self.item_base as usize) {
            Some(local) => self.items.get(local).is_some_and(|it| it.attributed),
            None => self.preceding.is_attributed_item(id),
        }
    }

    /// Resolve a dotted path (`Shared.foo`). When the head is **not** bound in
    /// local scope and the whole path names a module-qualified export of an
    /// earlier file, record the value [`Resolution::Item`] at the *whole-path*
    /// range — FCS reports the value use spanning the entire `LongIdentExpr` —
    /// and leave the leading qualifier segment `Deferred` (we do not model
    /// modules as resolvable defs yet). Otherwise fall back to the intra-file
    /// shape: the leading segment is an ordinary name use and the tail is
    /// `Deferred` qualified/member access.
    ///
    /// The local-scope check comes first: if the head is a parameter/local/item
    /// (`m` in `m.x` where `m` is a parameter) — or an opened-type static value
    /// (`open type T` importing a value `m`) — this is member access on a value,
    /// *not* a module path, so cross-file / assembly resolution is left out and
    /// the intra-file path taken; resolving it to a cross-file item / assembly
    /// path that happens to share the head name would be a wrong go-to-definition.
    pub(super) fn resolve_long_ident(&mut self, segments: &[SyntaxToken]) {
        let Some((first, rest)) = segments.split_first() else {
            return;
        };

        // `base.Member` — the `base` keyword (parsed as an `IDENT_TOK` with the
        // *unquoted* text `base`, matching FCS's `Ident("base")`) is the reserved
        // base-class receiver, not a value binder. It must never resolve to an
        // in-file name, even one a back-ticked `` ``base`` `` binder defines (which
        // is a *different* identifier — its token text carries the backticks). The
        // whole path is then a member access we don't model, so defer it. Gated on
        // the raw token text (`base`, no backticks) so the quoted form is
        // unaffected.
        if first.text() == "base" {
            return;
        }

        // A head whose latest in-scope entry a generation barrier STALED gates
        // every qualified channel below, exactly like `opaque_dotted_open` but
        // scoped to this head ([`Self::head_entry_staled`], codex round 10):
        // the barrier is coarser than what its open actually contests, so FCS
        // may still bind the staled entry — a local `X` after `let X = 5` —
        // and a case / member / assembly target reached past it would be a
        // wrong go-to-definition. The path falls through to the intra-file
        // fallback, whose head lookup skips the stale entry and defers.
        let head_staled = self.head_entry_staled(id_text(first.text()));

        // A qualified in-file enum case `Color.Red`. The qualifier `Color`
        // resolves **latest-wins across the value and type namespaces** (FCS): the
        // enum case is the answer when the enum type is the latest `Color` — no
        // in-scope value `Color`, or that value is *earlier* in source than the
        // enum type. A later in-scope value instead shadows it (then `Color.Red`
        // is member access on the value — the intra-file fallback below). The enum
        // type itself shadows a same-named cross-file *value* export / assembly
        // path (it is the more-local, later definition), so this is tried before
        // those — but NOT a cross-file module-namespace owner of the head: FCS
        // binds `module Color / let Red = 99` from an earlier file over the
        // same-file type (probes M13/M14 of `docs/project-type-member-plan.md`,
        // the r13 module-namespace rule cross-file), so a contested head stands
        // down and the qualified-value path below resolves it.
        // (`type_case_path` already declines when a nested module shadows the head,
        // or for a multi-segment / non-enum path.)
        // Skipped while an opaque/unmodelled open is in scope: a plain `open
        // <module>` or `open type T` could supply the head `Color` as an unmodelled
        // submodule/nested-type/value that out-ranks a same-file type for the
        // qualifier — FCS resolves `open M (module Color); type Color = Red | Blue;
        // Color.Red` through `M.Color.Red`, not the same-file case. We cannot prove
        // the open lacks such a `Color`, so defer (sound; an availability gap
        // matching the cross-file branch below).
        if let [case_seg] = rest
            && !self.opaque_value_open
            && !self.opaque_dotted_open
            && !self.unmodelled_open_active
            && !head_staled
            && !self.head_contested_by_project_module(first.text())
            && let Some((type_id, case_res)) = self.type_case_path(first.text(), case_seg.text())
        {
            // The value-vs-type qualifier rule differs by case kind (FCS, expression
            // position): an *enum* case wins the qualifier unless a value of that
            // name is *later* than the type (`let Color = 0; type Color = Red = 0;
            // Color.Red` → the case; reverse order → member access). A *union* case
            // (RQA or not) loses to **any** in-scope value — F# reads `Color.Red` as
            // member access on the value whenever one exists (`let Color = 0; type
            // Color = Red | Blue; Color.Red` is FS0039, not the case). So defer
            // whenever a value would take the qualifier.
            //
            // Only an **ordinary** value counts — a same-named union *case*
            // constructor (`type Color = Color | Red`) is not a dottable value, so it
            // does not shadow the qualifier (FCS resolves `Color.Red` to the case).
            // The definite-non-case test is `case_classification == Some(false)` (the
            // same `head_is_definite_value` test the cross-file block below uses): it
            // catches an **opened** assembly / auto-open value too, which has no
            // in-file `Def` range — so testing the classification, not
            // `value_def_range`, is what makes `open Demo.Auto (value Tag); type Tag =
            // Case | Other; Tag.Case` defer to the opened value (FCS), not the case.
            let head = id_text(first.text());
            let head_is_value = self.head_case_classification(head) == Some(false);
            let value_takes_qualifier = match self.case_resolution_kind(case_res) {
                // An enum case loses the qualifier only to a *later* value; the
                // position needs an in-file range (an opened value has none, and is
                // never later than the same-file enum we model, so it does not take).
                Some(DefKind::EnumCase) => {
                    head_is_value
                        && self
                            .value_def_range(head)
                            .is_some_and(|vr| vr.start() > self.defs[type_id.index()].range.start())
                }
                // A union case loses the qualifier to *any* definite non-case value.
                _ => head_is_value,
            };
            if !value_takes_qualifier {
                self.record_type_qualifier(first, case_seg, type_id, case_res);
                return;
            }
        }

        // A qualified in-file **static member** `Color.Red` (probes M1/M2d of
        // `docs/project-type-member-plan.md`). Same opaque-open gating as the
        // case branch above. The qualifier follows the *enum* rule — latest-wins
        // across the type and value namespaces (probes M2c/M2d): a value of the
        // head name *later* in source than the type takes the qualifier (member
        // access on the value — fall through to the intra-file shape, which
        // resolves the head to that value); the type otherwise keeps it and the
        // member emits. An in-scope value with **no in-file range** (an opened /
        // auto-open value) is an unprobed contest — defer the whole path rather
        // than pick a side.
        if let [member_seg] = rest
            && !self.opaque_value_open
            && !self.opaque_dotted_open
            && !self.unmodelled_open_active
            && !head_staled
            && !self.head_contested_by_project_module(first.text())
            && let Some((type_id, member_def)) =
                self.type_member_path(first.text(), member_seg.text(), first.text_range().start())
        {
            let head = id_text(first.text());
            let head_is_value = self.head_case_classification(head) == Some(false);
            if head_is_value {
                match self.value_def_range(head) {
                    // A later value takes the qualifier: member access on the
                    // value — the intra-file fallback below resolves the head to
                    // it (M2c). An earlier value lost it to the type (M2d).
                    Some(vr) if vr.start() > self.defs[type_id.index()].range.start() => {}
                    Some(_) => {
                        self.record_type_qualifier(
                            first,
                            member_seg,
                            type_id,
                            Resolution::Local(member_def),
                        );
                        return;
                    }
                    // An opened value has no in-file position to compare — the
                    // opened-value-vs-lexical-type qualifier contest is unprobed.
                    None => {
                        self.record(
                            first.text_range(),
                            Resolution::Deferred(DeferredReason::QualifiedAccess),
                        );
                        return;
                    }
                }
            } else {
                self.record_type_qualifier(
                    first,
                    member_seg,
                    type_id,
                    Resolution::Local(member_def),
                );
                return;
            }
        }

        // When the head is a **definite value** in scope — a local/parameter/item
        // binding, or an opened `let`/static value — this is member access on a
        // value (`m.x`), not a module/namespace path: leave cross-file and assembly
        // resolution out and take the intra-file path (the fallback resolves the
        // head where it can, the tail defers). But a head that is a *constructor
        // case* (a nullary case has no dottable members) or an *unclassifiable*
        // opened name does **not** block a dotted *module* path: `open Lib; Red.foo`
        // where `Lib` exposes both a case `Red` and a `module Red` is `Lib.Red.foo`
        // (FCS), so we let the self-validating module resolution below try it — it
        // falls through to member access when the head is not actually a module.
        // The qualified resolution is also skipped while an `opaque_value_open` is
        // in scope (it could provide the head as a value we cannot enumerate) or an
        // `opaque_dotted_open` is (an opened project module could provide the head
        // as one of its unmodelled submodules/types): resolving the path then risks
        // a wrong target.
        let head = id_text(first.text());

        // A raw `global` head is F#'s namespace-root marker, never a value: it
        // must not count as a "definite value" even when a same-file `` ``global`` ``
        // binder is in scope (that binder's `idText` normalises to the same text
        // `global`). Excluding it here routes the path through the rooted
        // qualified resolution below — which strips the `global` segment — instead
        // of the intra-file member-access fallback, so the marker never resolves
        // to the colliding binding. The keyword's token text is the unquoted
        // `global` (a `` ``global`` `` *use* carries backticks, a distinct
        // identifier), mirroring the `base` guard above.
        let head_is_definite_value =
            first.text() != "global" && self.head_case_classification(head) == Some(false);
        // …but a definite value takes that total priority only while it still
        // HOLDS FCS's unqualified-name slot: a `type Color` entering the slot
        // later EVICTS it (latest-wins across the value and type namespaces —
        // the M2c/M2d qualifier rule, applied here to the gate; probes
        // M20a–M20i), and the reference becomes a module/type-qualified path
        // again. An evicted head takes the qualified block below (modules are
        // searched first — a cross-file `module Color` wins); whatever the
        // block cannot resolve then *defers* at the fallback, never re-binding
        // the evicted value. Only a compound path is gated — a bare name never
        // reaches the qualified block, and its (pre-existing) eviction
        // blindness in `resolve_name_use` is a separate boundary.
        let head_slot = if head_is_definite_value && !rest.is_empty() {
            self.head_value_slot(head)
        } else {
            HeadSlot::Held
        };
        if (!head_is_definite_value || matches!(head_slot, HeadSlot::Evicted))
            && !self.opaque_value_open
            && !self.opaque_dotted_open
            && !head_staled
        {
            // A **same-file module-qualified** type-qualified case (`Pal.Color.Red`
            // where `Pal.Color` is a same-file type — Gap A). Tried **first**, before
            // the cross-file qualified-value / exact-export branches: the head
            // resolves through the *lexical* container chain, so a same-file nested
            // module shadows an earlier file's same-named root module (FCS resolves
            // `Pal.Color.Red` to this file's case even when an earlier file exports a
            // value at the written path). Gated on `!unmodelled_open_active` like the
            // 2-segment same-file branch (an `open type` could supply the head).
            if !self.unmodelled_open_active
                && self.record_same_file_module_qualified_case(segments, false)
            {
                return;
            }

            // A qualified value reference `Mod.value` (`A.B.value`, `Alias.value`):
            // name-shorten the module *prefix* the same way an `open` path is
            // resolved (relative to the enclosing namespace / opens, following
            // module abbreviations) and look the value up under it — same-file or an
            // earlier Compile-order file. FCS reports the value use spanning the
            // whole dotted path; the module qualifier segments are left `Deferred`
            // (no module-as-def model).
            //
            // Both guards apply. `!opaque_value_open`: an opaque open could shadow
            // the head with a value, making `head.rest` member access, not a module
            // path. `!opaque_dotted_open`: a plain `open <project module>` could
            // supply the head from content we do not model (a submodule, or a type
            // from an assembly the module's namespace merges with), so a head
            // resolved relative to the *enclosing namespace* could be the wrong
            // target — we cannot prove otherwise, so we conservatively defer while
            // any project-module open is in scope (correctness over availability).
            // Stage-1 signature screen (project side): when some
            // precedence-ordered reading of this path may be
            // signature-exposed, FCS binds the signature, so none of the
            // project lookups below may bind a lower-priority candidate —
            // they are skipped and the reference defers (the assembly tier
            // repeats the veto internally). See
            // [`Self::sig_screens_reading_of`].
            let written_path: Vec<String> = segments
                .iter()
                .map(|t| id_text(t.text()).to_string())
                .collect();
            let sig_screened = self.sig_screens_reading_of(&written_path);

            if let Some((value_seg, prefix_segs)) = segments.split_last() {
                let mut prefix: Vec<String> = prefix_segs
                    .iter()
                    .map(|t| id_text(t.text()).to_string())
                    .collect();
                let rooted = prefix_segs.first().is_some_and(|t| t.text() == "global");
                if rooted {
                    prefix.remove(0);
                }
                if !sig_screened
                    && !prefix.is_empty()
                    // An `open type T` / `open <assembly type>` whose nested types we
                    // do not model could supply the head (`open type T; M.value` where
                    // `T` has a nested type `M`), shadowing the project-module
                    // shortening — the same reason the assembly path defers under
                    // `unmodelled_open_active`. So an *unrooted* prefix defers while
                    // one is in scope; a `global.`-rooted prefix is absolute (no open
                    // can redirect it), so it is unaffected.
                    && (rooted || !self.unmodelled_open_active)
                    && let Some(module_path) = self.resolved_project_module(&prefix, rooted)
                    // A module's own simple name is not in scope within itself or a
                    // nested module (FCS FS0039 for `M.x` inside `M`, `Outer.v`
                    // inside `Outer.Inner`, *and* `Outer.Inner.y` inside `Outer` —
                    // using the own name as the head to reach a descendant): the
                    // name becomes referenceable only from a *sibling* (or below).
                    // The constraint is on the **head** segment, which
                    // `resolved_project_module` lands at `module_path[..head_len]`
                    // (the resolved prefix minus the tail segments written after the
                    // head). Reject when that head landing is the current container
                    // or an ancestor of it. A sibling/descendant head is not such a
                    // prefix, so `Inner.y` (head `Inner`) and `Sib.x` (head a
                    // sibling) still resolve. This also closes the non-`rec` timing
                    // the eager `self.items` push would otherwise expose: qualifying
                    // a current-group binding needs the current module's own name as
                    // the head, now rejected.
                    && let head_len = module_path.len() - prefix.len() + 1
                    && !self.container_path.starts_with(&module_path[..head_len])
                    && let Some(id) =
                        self.qualified_value_in(&module_path, id_text(value_seg.text()))
                {
                    let whole =
                        TextRange::new(first.text_range().start(), value_seg.text_range().end());
                    self.record(whole, Resolution::Item(id));
                    for seg in prefix_segs {
                        self.record(
                            seg.text_range(),
                            Resolution::Deferred(DeferredReason::QualifiedAccess),
                        );
                    }
                    return;
                }
            }

            // The exact written path may instead name an **earlier file's**
            // qualified export directly (`Demo.Target.foo`). The head may be the
            // *enclosing namespace's* own name, which is referenceable cross-file
            // (an earlier file established the namespace) even though the relative
            // resolution above rejects a self/ancestor head — FCS's project oracle
            // resolves it. No name-shortening, so no head-guard; the relative
            // resolution above already handles the common cross-file shapes, so this
            // only adds the enclosing-namespace-qualified case. A *same-file* such
            // reference still misses here (`preceding` lacks it) and so defers,
            // matching FCS (FS0039 — same-file, the namespace's own name is not yet
            // in scope). (The root-qualified form `global.Demo.Target.foo` does not
            // parse in expression position yet — see the parser's
            // `global_headed_app_source_does_not_panic` — so it never reaches here;
            // it can be handled when the parser supports `global` as an expr atom.)
            if let Some(last) = rest.last()
                && !sig_screened
                && let Some(id) = self
                    .preceding
                    .lookup_qualified_path(&written_path, &self.container_path)
            {
                let whole = TextRange::new(first.text_range().start(), last.text_range().end());
                self.record(whole, Resolution::Item(id));
                for seg in segments.iter().take(segments.len() - 1) {
                    self.record(
                        seg.text_range(),
                        Resolution::Deferred(DeferredReason::QualifiedAccess),
                    );
                }
                return;
            }

            // The TYPE-SIDE fallbacks below are barred for an EVICTED head
            // (probes M20t/M20u, codex round 5). Everything above resolved
            // through the module namespace, which FCS's compound search runs
            // FIRST — no type candidate can shadow a module binding. These
            // two resolve through the TYCON table instead, where the evicting
            // type itself is a nearer candidate FCS tries before them: when
            // it owns the residual FCS binds ITS member/case (`open LibNs`
            // supplying `type Color` with `static member Red` beats an
            // earlier file's root `type Color = Red | …` — M20t), and only
            // when it lacks the residual does FCS backtrack to them (M20u).
            // Sema cannot prove which way that goes (an opened type's members
            // are not modeled through opens), so the evicted head defers at
            // the fallback instead — never a wrong target, at worst a lost
            // M20u-shaped agreement.
            let value_evicted = matches!(head_slot, HeadSlot::Evicted);

            // …or a cross-file **type-qualified case** `Type.Case` (`Lib.Color.Red`,
            // or `Color.Red` shortened by `open Lib` / the enclosing namespace): the
            // qualifier names an earlier file's union/enum type whose case this is.
            // Resolved through the project type-qualified-case index, mirroring the
            // qualified-value resolution above. The whole dotted span resolves to the
            // case; the qualifier segments defer (we do not export project *types* as
            // defs, so the head has no cross-file target — a sound nav gap, never a
            // wrong resolution). A leading `global.` does not parse in expression
            // position yet, so `rooted` is always false here (matching the
            // qualified-export block above).
            if !rest.is_empty()
                && !value_evicted
                && !sig_screened
                && let Some(id) = self.cross_file_type_case(&written_path, false)
            {
                let whole = TextRange::new(
                    first.text_range().start(),
                    segments.last().expect("non-empty path").text_range().end(),
                );
                self.record(whole, Resolution::Item(id));
                for seg in segments.iter().take(segments.len() - 1) {
                    self.record(
                        seg.text_range(),
                        Resolution::Deferred(DeferredReason::QualifiedAccess),
                    );
                }
                return;
            }

            // …or a referenced-assembly path, resolved through the shared F#
            // precedence tiers (opens → current enclosing namespace → root), with
            // the value/member leaf generator (`assembly_path_records` records a
            // trailing public static as a `Member`). This is the same walker the
            // type path uses, so the two positions stay in lock-step.
            //
            // Under an `unmodelled_open_active` open (an opened assembly module /
            // `open type` whose nested types we do not model), the *relative*
            // readings (through an open or the enclosing namespace) are unsafe —
            // the open's unmodelled contents could shadow them. The **root /
            // as-written** reading is safe only when it is the absolute winner:
            // every higher-precedence reading — the shared
            // [`Self::assembly_prefixes_by_priority`] walk, so a tier added there
            // is seen here too — must be absent or read the path identically. If
            // one resolves it to a *different* target, or a **project entity
            // captures it** (`ProjectShadowed` — it would win in F# exactly like a
            // differing assembly reading), the root binding is unsafe, so we defer
            // rather than bind the wrong root — `namespace Demo; open type
            // Demo.Calc; Sub.Calc.Zero()` is `Demo.Sub.Calc.Zero` via the
            // enclosing namespace (a project `module Sub` there shadows it the
            // same way), and `module M; open Demo; open type Demo.Calc;
            // Sub.Calc.Zero()` is `Demo.Sub.Calc.Zero` via the explicit open —
            // neither the root `Sub.Calc.Zero`. (The root prefix walks too and
            // self-compares equal — a no-op.)
            let resolved = if value_evicted {
                // The assembly tiers are type-side too — the evicting type
                // shadows them the same way (see the M20t/M20u note above).
                None
            } else if self.unmodelled_open_active {
                match self.assembly_path_records(&[], segments) {
                    AssemblyPath::Resolved {
                        payload: root_recs, ..
                    } => {
                        let higher_reading_differs =
                            self.assembly_prefixes_by_priority().any(|prefix| {
                                match self.assembly_path_records(prefix, segments) {
                                    AssemblyPath::Resolved { payload, .. } => payload != root_recs,
                                    // A higher abbreviation-defer / self-module
                                    // reading is uncertain, so the root binding is
                                    // unsafe.
                                    AssemblyPath::ProjectShadowed
                                    | AssemblyPath::SelfModuleShadowed
                                    | AssemblyPath::AbbreviationOpaque => true,
                                    AssemblyPath::NoMatch => false,
                                }
                            });
                        if higher_reading_differs {
                            None // a higher-precedence reading would win, but is unsafe → defer
                        } else {
                            Some(root_recs)
                        }
                    }
                    // A self-module-shadowed root defers here too: an unmodelled
                    // open in scope could supply the current module's own name
                    // (which FCS does not bind as a self-qualifier), so — unlike the
                    // opens-modelled `else` arm — we cannot safely resolve it.
                    AssemblyPath::ProjectShadowed
                    | AssemblyPath::SelfModuleShadowed
                    | AssemblyPath::AbbreviationOpaque
                    | AssemblyPath::NoMatch => None,
                }
            } else {
                // Value/member path: a project-bound head (nested module, local,
                // value prefix) captures the reference, so the as-written shadow
                // vetoes the opens tier too. Both defers (a project shadow, a
                // clean no-match) fall through identically here — the tail of
                // this function records the deferral.
                match self.resolve_assembly_path_tiered(
                    |prefix| self.assembly_path_records(prefix, segments),
                    true,
                    |_| ShadowVeto::None,
                ) {
                    TieredResolution::Resolved(recs) => Some(recs),
                    TieredResolution::ShadowDeferred | TieredResolution::NoMatch => None,
                }
            };
            if let Some(recs) = resolved {
                self.apply(recs);
                return;
            }
        }

        // Intra-file fallback: head is a name use, tail deferred. But a *dotted*
        // path whose head is a constructor **case** is not member access — a
        // nullary case has no dottable members, so `Red.foo` (where `Red` is an
        // opened case) is a qualifier into a same-named module / type that we could
        // not resolve here (e.g. `open M` where `M` has both `type T = Red` and
        // `module Red`, reached under an `opaque_dotted_open` that skips the
        // module-path resolution above). FCS resolves it through the module
        // (`M.Red.foo`); we cannot, so defer the head rather than record a wrong
        // go-to-def onto the case. (A definite *value* head stays member access; an
        // unclassifiable cross-file head stays a value reference — its case-ness is
        // not reachable, the cross-file-case follow-up.)
        let head_is_case = !rest.is_empty() && self.head_case_classification(head) == Some(true);
        // A raw `global` head reaching the fallback (its rooted path resolved no
        // project/assembly target) is the namespace-root *marker*, not a name use:
        // defer it rather than `resolve_name_use`, which would bind it to a
        // colliding `` ``global`` `` value in scope (they share the normalised text
        // `global`). Mirrors the `base` early-return; here we defer because the
        // qualifier tail may still be meaningful (an availability gap, never a
        // wrong go-to-def).
        // A definite-value head that no longer holds (or provably may not
        // hold) FCS's unqualified slot — evicted by a later type, or in an
        // unorderable contest with one (`head_value_slot`) — likewise defers:
        // FCS reads the path as module/type-qualified, so recording member
        // access on the value would be a wrong target (probes M20a/M20e).
        if head_is_case || first.text() == "global" || !matches!(head_slot, HeadSlot::Held) {
            self.record(
                first.text_range(),
                Resolution::Deferred(DeferredReason::QualifiedAccess),
            );
        } else {
            self.resolve_name_use(first);
        }
        for seg in rest {
            self.record(
                seg.text_range(),
                Resolution::Deferred(DeferredReason::QualifiedAccess),
            );
        }
    }

    /// Whether `names` is a path F# resolves *within the project* — searched
    /// before referenced assemblies, so it must not fall through to a colliding
    /// assembly type/member (and an `open type` of it must not model the
    /// assembly's statics). Declines a path that is:
    ///  - rooted at the *current* module — its own values live in local scope,
    ///    not the cross-file index, so we cannot tell a member it provides from
    ///    one it does not, and defer either way (sound);
    ///  - rooted at a *nested* module (same-file local name, or an earlier file's
    ///    exported qualified path) whose members we do not yet model;
    ///  - member access on a project *value* (`Demo.Calc.x` where `Demo.Calc` is
    ///    a `let`) — F# binds the value, so the path is never the assembly;
    ///  - a bare reference that *is* a declared project module path exactly.
    ///
    /// A project module that is only a *proper* prefix is deliberately NOT
    /// declined: it merges with the assembly namespace, so F# falls through to
    /// the assembly when the module does not provide the tail (FCS-verified). The
    /// fall-through is sound only while the project value index is complete — true
    /// today (a module holds only `let` values).
    pub(super) fn path_is_project_shadowed(&self, names: &[String]) -> bool {
        // A project *value* prefix (`Demo.Calc.x` where `Demo.Calc` is a `let`)
        // shadows only in the *value/expression* namespace; the type-namespace
        // part is shared with [`Self::path_is_project_type_shadowed`]. A bare ref
        // that *is* a declared top-level project module exactly shadows only here
        // too: a **module is not a type**, so it does not occupy the type name —
        // `(x: Calc)` with a top-level `module Calc` and `open Demo` is the
        // assembly type `Demo.Calc`, never the module (FCS-verified), so it must
        // not gate the type path.
        self.path_is_project_type_shadowed(names)
            || self.preceding.is_exact_project_module(names)
            || self.preceding.is_project_value_prefixed(names)
    }

    /// Whether `names` is rooted at the **current module's own path** — the head
    /// (and any leading segments) is the module the resolver is presently walking,
    /// written in full (`module_path`-qualified, e.g. `Demo.Calc` inside a
    /// headerless-file `module Demo`). Kept for the full-path shadow in
    /// [`Self::path_is_project_type_shadowed`]; the *namespace-relative* self
    /// reference (`List.fold` inside `namespace N` / `module List`) is
    /// [`Self::path_rooted_at_self_or_ancestor_module`].
    pub(super) fn rooted_at_current_module(&self, names: &[String]) -> bool {
        self.module_path.as_ref().is_some_and(|mp| {
            !mp.is_empty() && mp.len() <= names.len() && names.starts_with(mp.as_slice())
        })
    }

    /// If `names` is an enclosing-module **self-qualifier**, the full
    /// namespace-qualified path of the member it names; else `None`.
    ///
    /// FCS does not bind a module's own name from within its body — `M.x` inside
    /// `M`, `Outer.v` inside `Outer.Inner`, and `Outer.Inner.y` inside `Outer` are
    /// all FS0039 — so the head, when it is the **simple name** of the current
    /// module or an enclosing one, resolves nowhere *in the project* and the path
    /// falls through to whatever an `open` / implicit `[<AutoOpen>]` supplies
    /// (`Microsoft.FSharp.Collections.List` for bare `List`). The enclosing
    /// modules are the segments of the module chain
    /// `container_path[namespace_depth..]`, so the head is a self-qualifier iff it
    /// equals one of them — a *suffix* test, which (unlike a prefix of the chain)
    /// also catches a module nested under another module (`List.fold` inside
    /// `module N` / `module List`, where the chain is `[N, List]`).
    ///
    /// The returned path re-roots the reference at that module's **full** path
    /// (`container_path[..=j]`, the nearest enclosing module of the name) so the
    /// cross-file index — keyed by full paths — can be probed in the right frame
    /// regardless of nesting depth ([`Self::self_module_shadow_only`]).
    fn self_qualified_member_path(&self, names: &[String]) -> Option<Vec<String>> {
        let head = names.first()?;
        let depth = self.namespace_depth.min(self.container_path.len());
        let j = (depth..self.container_path.len())
            .rev()
            .find(|&j| &self.container_path[j] == head)?;
        let mut full = self.container_path[..=j].to_vec();
        full.extend_from_slice(&names[1..]);
        Some(full)
    }

    /// Whether a **same-name non-self module/type** the head binds ahead of self
    /// is in scope. FCS resolves a self-qualified head to the *nearest non-self*
    /// entity of that name, searching outward, so the relaxation must decline when
    /// one exists:
    /// - a **child** module/type in the *current* container — `module List` (or
    ///   `type List`) inside `module List` captures `List.rev`
    ///   (`N.List.List.rev`), never FSharp.Core; and
    /// - a same-name **module** in an *enclosing* container — an outward
    ///   `module List` a self reference falls out to (`module Root` with both
    ///   `module List` and `module Outer.List`, where `List.rev` inside the inner
    ///   one binds `Root.List.rev`).
    ///
    /// A module's own name lives in its *parent's* container, never its own, so
    /// the current-container check finds only genuine descendants, and the
    /// enclosing check stops **below the parent** (`< len-1`) so it never matches
    /// self. The enclosing arm is deliberately restricted to **modules**, not
    /// types: a same-name enclosing *type* is the `type List` / `module List`
    /// companion pattern, which FCS resolves per member (`List.length` there is
    /// FSharp.Core's), so blanket-deferring it would regress a real pattern —
    /// whereas a same-name enclosing *module* only arises in the pathological
    /// nested-`module List` shape, where the (sound) over-deferral is never paid by
    /// real code. Cross-file same-name modules/types need no separate check here:
    /// they are keyed by full path in `preceding` and caught by
    /// [`ProjectItems::binds_along_path`](super::model::ProjectItems::binds_along_path)
    /// at the reconstructed path.
    fn same_name_entity_shadows_head(&self, head: &str) -> bool {
        let in_current = self
            .module_like_names
            .get(&self.container_path)
            .is_some_and(|names| names.contains(head))
            || self
                .type_defs
                .get(&self.container_path)
                .is_some_and(|types| types.contains_key(head));
        // A same-name MODULE declared in an enclosing container — a genuine
        // *cousin*, same-file (`module_like_names`) or an earlier file
        // (`preceding` module headers / nested paths). A level `k` whose own chain
        // segment *is* the head (`container_path[k] == head`) is skipped: the
        // module it finds is the self/ancestor named `head` (at
        // `container_path[..=k]`, part of the current chain), not a cousin — an
        // ancestor self-qualifier (`List.rev` inside `module List` / `module
        // Helpers`) must still fall through to FSharp.Core.
        let depth = self.namespace_depth.min(self.container_path.len());
        let enclosing_module = (depth..self.container_path.len().saturating_sub(1))
            .filter(|&k| self.container_path[k] != head)
            .any(|k| {
                let prefix = &self.container_path[..k];
                self.module_like_names
                    .get(prefix)
                    .is_some_and(|names| names.contains(head))
                    || {
                        let mut full = prefix.to_vec();
                        full.push(head.to_string());
                        self.preceding.is_exact_project_module(&full)
                            || self.preceding.is_exact_nested_module(&full)
                    }
            });
        in_current || enclosing_module
    }

    /// Whether `names` is project-shadowed **only** because it is an
    /// enclosing-module self-qualifier ([`Self::self_qualified_member_path`], or a
    /// fully-qualified [`Self::rooted_at_current_module`] reference) with **no
    /// reachable project binding** at the path — the one project shadow an `open`
    /// can still redirect (see
    /// [`AssemblyPath::SelfModuleShadowed`](super::state::AssemblyPath::SelfModuleShadowed)).
    ///
    /// A module can be **split across files**: FCS merges `module N.List` over a
    /// namespace's files, and an *earlier* fragment's member is reachable through
    /// the module name — `List.fold2` inside a later `N.List` fragment binds the
    /// project's `N.List.fold2`, not FSharp.Core's `List.fold2` (fcs-dump: a
    /// project member resolves to the project, a name only FSharp.Core defines
    /// falls through to it — the merge is *per member*). So a self-qualifier is
    /// redirectable only when the project binds nothing the head could reach:
    /// - the **merged module** does not supply the tail
    ///   ([`ProjectItems::binds_along_path`](super::model::ProjectItems::binds_along_path)
    ///   at the reconstructed self path, or the raw fully-qualified spelling) —
    ///   covering cross-file values, modules, and types (`Operators.Checked` binds
    ///   a project `type Checked` over FSharp.Core's nested `Checked` module); and
    /// - no **same-name child** module/type captures the head
    ///   ([`Self::same_name_entity_shadows_head`] same-file, or a cross-file child
    ///   under `container_path ++ names`).
    ///
    /// Otherwise it stays a plain
    /// [`ProjectShadowed`](super::state::AssemblyPath::ProjectShadowed) — a
    /// conservative deferral, never a wrong FSharp.Core commit.
    pub(super) fn self_module_shadow_only(&self, names: &[String]) -> bool {
        // In a `module rec` / `namespace rec`, FCS *does* put the module's own name
        // in scope, so a self-qualified `List.rev` binds the project's own
        // `N.List.rev` (fcs-dump), not FSharp.Core. The whole "self is FS0039"
        // premise is void here, so never relax — keep the conservative deferral.
        if self.recursive_module_active {
            return false;
        }
        let reconstructed = self.self_qualified_member_path(names);
        if reconstructed.is_none() && !self.rooted_at_current_module(names) {
            return false;
        }
        // The merged current module (cross-file) supplies the tail, at the
        // reconstructed self path or the raw already-qualified spelling.
        if reconstructed
            .as_deref()
            .is_some_and(|p| self.preceding.binds_along_path(p))
            || self.preceding.binds_along_path(names)
        {
            return false;
        }
        // A same-name child module/type binds the head ahead of self — same-file in
        // the current container, or a cross-file child under `container_path ++ names`.
        if let Some(head) = names.first() {
            let child_path: Vec<String> =
                self.container_path.iter().chain(names).cloned().collect();
            if self.same_name_entity_shadows_head(head)
                || self.preceding.binds_along_path(&child_path)
            {
                return false;
            }
        }
        true
    }

/// Stage-1 signature screen, **project side**
    /// (`docs/fsi-signature-restriction-plan.md`): whether some
    /// precedence-ordered reading of the written path — each open /
    /// enclosing-namespace prefix of [`Self::assembly_prefixes_by_priority`],
    /// the root included — lands on a path a paired signature may expose. A
    /// screened reading outranks or equals every candidate the qualified
    /// project lookups could commit, and FCS binds the *signature* there
    /// (probe: root `module M; let x` + signatured `module A.M` exposing
    /// `x`: inside `namespace A`, `M.x` binds the `.fsi`, not the root
    /// module), so every lower-priority binding — a project `Item` as much
    /// as an assembly member — must be withheld. The assembly tier repeats
    /// the veto internally ([`ProjectItems::sig_screened_path`] via
    /// [`Self::path_is_project_type_shadowed`]); this is the check the
    /// *project*-side commit sites run before binding.
    pub(super) fn sig_screens_reading_of(&self, written: &[String]) -> bool {
        if !self.preceding.has_sig_screens() {
            return false;
        }
        self.preceding.sig_screened_path(written)
            || self.assembly_prefixes_by_priority().any(|prefix| {
                let full: Vec<String> = prefix
                    .iter()
                    .cloned()
                    .chain(written.iter().cloned())
                    .collect();
                self.preceding.sig_screened_path(&full)
            })
    }

    /// The *type-namespace* subset of [`Self::path_is_project_shadowed`]: whether
    /// `names` is a path F# resolves to a project **type** (a `type`/nested
    /// module rooting a type, not a *value* nor a bare top-level *module*) ahead
    /// of the referenced assemblies. Used to resolve an `open type` target, which
    /// lives purely in the type namespace — a project *value* of the same name
    /// does **not** shadow it (F# `open type Demo.Calc` opens the assembly type
    /// even when an earlier `module Demo` has a `let Calc`), and neither does a
    /// bare top-level *module* of the same name (a module is not a type).
    pub(super) fn path_is_project_type_shadowed(&self, names: &[String]) -> bool {
        self.rooted_at_current_module(names)
            || self
                .nested_module_locals
                .iter()
                .any(|p| names.starts_with(p.as_slice()))
            // …and same-file *qualified* paths of nested modules / types /
            // exceptions (`Demo.Calc` for a `type Calc` under `namespace Demo`,
            // referenced from a sibling `module M`): `nested_module_locals` holds
            // only the *relative* form, so a namespace-qualified reference needs
            // the exported (qualified) form too.
            || self
                .nested_module_exports
                .iter()
                .any(|p| names.starts_with(p.as_slice()))
            // NB: a *top-level* project module exactly equal to `names`
            // ([`ProjectItems::is_exact_project_module`]) is **not** a type
            // shadow — a module is not a type, so it never shadows a same-named
            // assembly type in type position (FCS); it lives in the value-only
            // [`Self::path_is_project_shadowed`]. A *nested* module still defers
            // here (its qualified path may root a project type we model later).
            || self.preceding.is_rooted_at_nested_module(names)
            // Stage-1 signature screen (`docs/fsi-signature-restriction-plan.md`):
            // a path under a signatured module root whose residual the
            // signature *may* expose must not commit to a merged assembly
            // member in ANY namespace — FCS binds the `.fsi` (probe:
            // sig-exposed `Shared.shown` with a colliding `RefLib` → the
            // `.fsi`), and Stage 1 has no signature identity to commit, so
            // it defers. A residual absent from the signature text falls
            // through to the assembly exactly as FCS does.
            || self.preceding.sig_screened_path(names)
    }

    /// Record a qualified in-file enum-case path `Color.Red` (`type_seg`,
    /// `case_seg`): the head → the enum *type* def and the whole span → the case
    /// def, mirroring FCS. Shared by expression ([`resolve_long_ident`](Self::resolve_long_ident))
    /// and pattern ([`resolve_pat_types`](Self::resolve_pat_types)) position, both
    /// of which FCS resolves identically. Returns `true` (and records both) iff it
    /// matched; otherwise records nothing (the path defers, never a wrong member).
    pub(super) fn record_type_case_path(
        &mut self,
        type_seg: &SyntaxToken,
        case_seg: &SyntaxToken,
    ) -> bool {
        let Some((type_id, case_res)) = self.type_case_path(type_seg.text(), case_seg.text())
        else {
            return false;
        };
        self.record_type_qualifier(type_seg, case_seg, type_id, case_res);
        true
    }

    /// Classify a **same-file module-qualified** type-qualified case `Pal.Color.Red`
    /// (exactly three segments) against the complete per-container declared-name view
    /// [`Self::container_decls`]. This is Gap A of
    /// `docs/type-qualified-case-prefix-plan.md`, closed for the clean case — sound by
    /// construction (decide on certainty, defer on any contention).
    ///
    /// 1. **Head** `Pal` → a **candidate loop** over the same-file containers that
    ///    declare it in a namespace that can own a dotted head
    ///    ([`DeclKinds::stops_dotted_head`]: the module namespace — module / alias —
    ///    plus, in expression position, a `let` value; a type / union-case ctor /
    ///    active pattern / exception ctor never hides a farther module, so those
    ///    containers are *skipped*, FCS-probed both positions). The walk spans the
    ///    current namespace and enclosing modules within it (`k >= namespace_depth`,
    ///    innermost first; plus the **root** only in a *headerless* file) — **no
    ///    opens tier** (F# prefers the lexically-enclosing module over an
    ///    `open`-supplied one). A non-clean stop ([`DeclKinds::is_clean_module_head`],
    ///    position-aware: a co-declared value disqualifies only in expression
    ///    position) → [`Miss`](SameFileQualified::Miss) (the head is committed /
    ///    redirected — a cross-file branch may try). A clean candidate that is the
    ///    current container or an ancestor (FS0039), or whose *residual* resolves
    ///    nothing ([`classify_module_qualified_segment`](Self::classify_module_qualified_segment)
    ///    → `None`), **continues to the next candidate outward** — FCS binds the
    ///    first same-named module whose residual resolves, so stopping at the
    ///    nearest one navigated cross-file while FCS bound an outer same-file
    ///    module.
    /// 2. **Segment** `Color` in each candidate module → see
    ///    [`classify_module_qualified_segment`](Self::classify_module_qualified_segment).
    ///
    /// **`DeferStop` vs `Miss` matters for soundness:** when the head binds same-file,
    /// the reference is same-file-rooted, so the caller must **not** fall through to
    /// the cross-file branches — a same-file `Pal` shadows an earlier file's same-named
    /// module, so navigating to `file0.Pal.Color.Red` would be a wrong target. `Miss`
    /// is returned only when nothing same-file *binds* the head (so F# searches
    /// outward / cross-file), or the segment is a submodule a qualified-value branch
    /// resolves same-file.
    ///
    /// Restricted to exactly three segments: the 2-segment `Color.Red` is owned by
    /// [`type_case_path`](Self::type_case_path), and a 4+-segment (multi-module-head)
    /// path defers (a later stage).
    fn classify_same_file_module_qualified_case(
        &self,
        segments: &[SyntaxToken],
        in_pattern: bool,
    ) -> SameFileQualified {
        let [head_seg, type_seg, case_seg] = segments else {
            return SameFileQualified::Miss;
        };
        let head = id_text(head_seg.text());
        let ty = id_text(type_seg.text());
        let case = id_text(case_seg.text());
        let use_pos = head_seg.text_range().start();

        // Resolve the head to the innermost same-file container whose declaration of
        // it *stops the dotted-head walk* ([`DeclKinds::stops_dotted_head`]): the
        // module namespace (module / alias) plus — in expression position — a `let`
        // value, which commits member access. Other namespaces (type, union-case
        // ctor, active pattern, exception ctor) never own or hide a dotted head
        // (FCS-probed: an outer same-file `module Pal` wins past any of them, in
        // both positions), so containers declaring `Pal` only as those are
        // *skipped*, not stopped at — stopping there returned `Miss` and let the
        // cross-file branches navigate to an earlier file's export while FCS binds
        // the outer same-file module (a wrong target). Search the **current
        // namespace and enclosing modules within it** (`k >= namespace_depth`),
        // innermost first. *Ancestor* namespaces (`1 <= k < namespace_depth`) are
        // **not** searched for a bare head (FCS FS0039 — `Pal` in `namespace N.Sub`
        // does not see `N.Pal`), the same relative rule as
        // [`open_interpretations`](Self::open_interpretations).
        //
        // The **root** (`k == 0`) is always searched, last: a headerless file's
        // implicit anonymous root module lexically encloses the reference (and its
        // sibling modules), and a `namespace global` root module is likewise a
        // same-file candidate — FCS resolves `namespace global; module Pal … ;
        // namespace Client; Pal.Color.Red` to that root `Pal` when nothing
        // outranks it (probe G1), and a colliding root module in another file is
        // FS0248, so the root candidate can never mask a legal cross-file target.
        // The "a root module ranks below `open`s" behaviour observed earlier
        // (`namespace global; module Pal … open A; Pal.Color.Red` binds `A.Pal`)
        // is NOT a special root tier — it is the ordinary positional latest-wins
        // contest below: in those probes the `open` sat later in source than the
        // root module declaration. With the open *earlier* than the module, the
        // module wins even in `namespace global` (probe G3), exactly like any
        // other candidate.
        // The walk is a **candidate loop**, not a first-stop search: FCS tries each
        // same-named module candidate innermost→outward and binds the first whose
        // *residual* (`Color.Red`) resolves — a nearer `module Pal` lacking `Color`
        // does not end the search (FCS-probed, both positions, r14). So a candidate
        // whose segment classification finds nothing (`None` below) *continues* to
        // the next candidate; only when every same-file candidate declines may the
        // caller fall through to cross-file resolution.
        let relative = (self.namespace_depth.max(1)..=self.container_path.len()).rev();
        let root_tier: &[usize] = &[0];
        for k in relative.chain(root_tier.iter().copied()) {
            let parent = &self.container_path[..k];
            let Some(&kinds) = self.container_decls.get(parent).and_then(|d| d.get(head)) else {
                continue; // nothing named `Pal` here — next container outward
            };
            if !kinds.stops_dotted_head(in_pattern) {
                continue; // a namespace that cannot own or hide a dotted head — skip
            }
            if !kinds.is_clean_module_head(in_pattern) {
                // The walk stopped on something that redirects the head away from a
                // same-file module: a module **alias** (its target may be cross-file
                // — the alias-aware cross-file path follows it; FCS-pinned, an inner
                // alias shadows an outer real module, so do NOT continue outward), a
                // `let` value in expression position (member access — FCS binds the
                // value and errors on `.Color` rather than trying a farther module),
                // or an exception-ctor-contended module (FS0037-illegal source;
                // decline). So `Miss`: let the cross-file / qualified-value branches
                // resolve exactly as before (no regression; a wrong target there, if
                // any, is pre-existing).
                return SameFileQualified::Miss;
            }
            let mut module_path = parent.to_vec();
            module_path.push(head.to_string());

            // FS0039: the module's own name is not in scope as a head within itself
            // or a nested module — this candidate simply does not exist in the
            // environment, and F# tries the *next* same-named candidate outward
            // (FCS-probed: inside `Top.Pal`, `Pal.Color.Red` binds an outer
            // same-file `Client.Pal`), so continue, don't abandon the search.
            if self.container_path.starts_with(&module_path) {
                continue;
            }

            // The head environment is ONE source-position-ordered latest-wins list
            // over lexical module declarations and `open`s (FCS-probed, r16): an
            // in-scope open declared *later* than this candidate outranks it —
            // even from a nearer scope while the candidate sits in an enclosing
            // namespace. An open declared *earlier* loses to it (pinned by
            // `module_qualified_case_prefers_enclosing_over_an_open`), so the
            // comparison is positional, not scope-shaped. But an outranking open
            // commits only when its target could own the *residual* — FCS
            // backtracks past an `open A` whose `A.Pal` has nothing named `Color`
            // to the lexical candidate (probe BK1, codex r17) — so each later
            // open is judged by [`Self::open_contests_candidate`], latest first:
            // the first non-transparent one decides, and a residual-less open is
            // skipped. (Opaque/unmodelled opens never reach here — the callers
            // gate on their flags — and the implicit auto-opens precede every
            // same-file declaration, so only the recorded explicit namespace
            // opens can contest.)
            let contest = self
                .explicit_open_prefixes
                .iter()
                .rev() // latest open first — the winner under latest-wins
                .filter(|(open_pos, _)| kinds.module_pos.is_none_or(|p| *open_pos > p))
                .find_map(|(_, prefix)| {
                    self.open_contests_candidate(prefix, head, ty, case, in_pattern, use_pos)
                });
            if let Some(outcome) = contest {
                return outcome;
            }

            match self.classify_module_qualified_segment(module_path, ty, case, in_pattern, use_pos)
            {
                Some(outcome) => return outcome,
                // This candidate does not own the residual — F# searches outward
                // to the next same-named module (same-file first, then cross-file).
                None => continue,
            }
        }
        // Every lexical candidate declined — but opens are candidates too, and one
        // positioned *earlier* than the last lexical candidate has not been
        // consulted yet (the per-candidate contest only looks at later opens). FCS
        // backtracks to it (probe OP2: `open A; module Pal = type Other; …
        // Pal.Color.Red` binds `A.Pal.Color.Red`), so run the remaining opens,
        // latest first. Re-checking an already-transparent later open is a no-op
        // (any non-transparent one would have returned above), so scanning all of
        // them is equivalent to scanning the remainder.
        if let Some(outcome) = self
            .explicit_open_prefixes
            .iter()
            .rev()
            .find_map(|(_, prefix)| {
                self.open_contests_candidate(prefix, head, ty, case, in_pattern, use_pos)
            })
        {
            return outcome;
        }
        SameFileQualified::Miss // no same-file candidate owns it — cross-file may try
    }

    /// How an in-scope `open <prefix>` acts as a **candidate** for the head of
    /// `Pal.Color.Red` (see the candidate loop of
    /// [`classify_same_file_module_qualified_case`](Self::classify_same_file_module_qualified_case):
    /// opens later than a lexical candidate are consulted before it, and all
    /// remaining opens after the lexical candidates decline — the positional
    /// latest-wins environment, FCS-probed r16/OP2). Returns:
    ///
    /// - `None` — transparent: the prefix supplies no `head` at all, or its
    ///   target **provably** does not own the residual (a *same-file* module
    ///   target is judged by the complete [`Resolver::container_decls`] view; a
    ///   *cross-file* project target by the complete cross-file value / module /
    ///   namespace / type indexes, probes CF2/CF5; an *assembly* target by the
    ///   complete assembly env) — FCS backtracks past it (probe BK1) and the
    ///   search continues.
    /// - A **same-file module** target is a full candidate: its residual is
    ///   classified by the same complete-information machinery as a lexical
    ///   candidate ([`classify_module_qualified_segment`](Self::classify_module_qualified_segment)),
    ///   so the open's case **emits** (FCS-probed OP1/OP2/OP3/OSpat/VP1pat/OO1,
    ///   both positions) — unless the head name is contended where the target is
    ///   declared (not a clean module head there) or the target is the current
    ///   container or an ancestor (the FS0039 own-name shape at one remove,
    ///   unprobed), which defer.
    /// - `Some(Miss)` — the target is a cross-file project entity and the exact
    ///   type-qualified case `prefix ++ [head, ty, case]` **positively exists**:
    ///   the open-aware [`cross_file_type_case`](Self::cross_file_type_case)
    ///   branch resolves precisely that (its opens tier is latest-first too), so
    ///   fall through to it.
    /// - `Some(DeferStop)` — the target may own the residual in a way sema
    ///   cannot resolve or rule out. For a cross-file project target that is:
    ///   a *hidden* module (an earlier file's abbreviation, an active-pattern
    ///   module — names unenumerable), the current container or an ancestor
    ///   (FS0039 at one remove), same-file declarations of `ty` under a shared
    ///   namespace (the multi-file merge is unclassified), a value / submodule /
    ///   deeper namespace at the segment (probe CF11: FCS resolves a cross-file
    ///   submodule's own member), a type at the segment in expression
    ///   position (unmodeled members may commit — the Bexpr sacrifice) or with
    ///   unenumerable cases (an abbreviation commits through its target, probe
    ///   CF8), or any other project-introduced name the conflated shadow index
    ///   holds at the segment that no arm positively classified (an `extern`
    ///   prototype, an earlier file's module abbreviation — the conservative
    ///   catch-all). Likewise an assembly target with an entity/namespace at
    ///   `ty`.
    ///   Neither the cross-file lower tiers nor a farther candidate may win
    ///   then — defer, and stop the search.
    fn open_contests_candidate(
        &self,
        prefix: &[String],
        head: &str,
        ty: &str,
        case: &str,
        in_pattern: bool,
        use_pos: TextSize,
    ) -> Option<SameFileQualified> {
        let mut full = prefix.to_vec();
        full.push(head.to_string());
        // A same-file module target is a FULL candidate: sema holds the complete
        // view of a same-file module, so its residual is classified with exactly
        // the machinery a lexical candidate gets — `Emit` included (FCS-pinned,
        // probes OP1/OP2/OP3/OSpat/VP1pat/OO1: the open's same-file
        // `A.Pal.Color.Red` binds, in both positions). `None` (the target does
        // not own the residual) keeps the open transparent, so the search
        // backtracks past it exactly as before (probe BK1).
        if self.module_paths.iter().any(|p| p == &full)
            || self.nested_module_exports.iter().any(|p| p == &full)
        {
            // Contention at the opened scope (a dottable value / alias /
            // FS0037-illegal exception sharing the head name where the open's
            // target is declared): the target is not a clean module head there,
            // so decline to emit through it — defer, never fall through (the
            // open still outranks whatever ranks below).
            if let Some(&kinds) = self.container_decls.get(prefix).and_then(|d| d.get(head))
                && !kinds.is_clean_module_head(in_pattern)
            {
                return Some(SameFileQualified::DeferStop);
            }
            // The open supplying the *current* container (or an ancestor) as the
            // head is the FS0039 own-name shape at one remove — unprobed; defer
            // rather than emit or fall through.
            if self.container_path.starts_with(&full) {
                return Some(SameFileQualified::DeferStop);
            }
            return self.classify_module_qualified_segment(full, ty, case, in_pattern, use_pos);
        }
        // A **same-file-only namespace** target also earns the complete
        // treatment: a namespace spans files, but when it exists in no earlier
        // file and no referenced assembly, the same-file view of its contents is
        // total, so its residual classifies exactly like a module's (FCS-probed
        // NS1: `namespace A.Pal; type Color = Red | Blue` + `open A` binds
        // `A.Pal.Color.Red`). The same clean-head / self-ancestor guards apply.
        if self.namespace_paths.iter().any(|p| p == &full)
            && !self.preceding.is_namespace(&full)
            && !self.assemblies.has_namespace(&full)
        {
            if let Some(&kinds) = self.container_decls.get(prefix).and_then(|d| d.get(head))
                && !kinds.is_clean_module_head(in_pattern)
                && kinds.stops_dotted_head(in_pattern)
            {
                return Some(SameFileQualified::DeferStop);
            }
            if self.container_path.starts_with(&full) {
                return Some(SameFileQualified::DeferStop);
            }
            return self.classify_module_qualified_segment(full, ty, case, in_pattern, use_pos);
        }
        // A cross-file project module, or a project namespace (namespaces span
        // files, so even a same-file one is not a complete view of the residual
        // when the namespace also exists elsewhere). The residual decides
        // (FCS-probed CF2/CF3/CF5/CF8/CF11, refining r17's blanket DeferStop):
        // the cross-file value / constructor / module / namespace / type indexes
        // are complete for real-root files, so what the target owns at `ty` is
        // decidable. A target owning nothing there falls THROUGH to the assembly
        // arm below (a project module/namespace merges with a same-named
        // assembly namespace, which may still own the residual) and, when that
        // too is silent, out to the trailing `None`: transparent — FCS
        // backtracks past the open to the next candidate (probes CF2expr /
        // CF2pat / CF2b / CF5).
        if self.preceding.is_exact_project_module(&full)
            || self.preceding.is_exact_nested_module(&full)
            || self.is_project_namespace_path(&full)
        {
            let mut case_path = full.clone();
            case_path.push(ty.to_string());
            case_path.push(case.to_string());
            // The exact type-qualified case positively exists AND is accessible from
            // here: the open-aware cross-file branch resolves precisely that. An
            // inaccessible `private`-type case is not delegated (it would defer
            // there anyway) — it falls to the conservative handling below.
            if self
                .preceding
                .type_qualified_case(&case_path, &self.container_path)
                .is_some()
            {
                return Some(SameFileQualified::Miss);
            }
            // A hidden target — an earlier file's module abbreviation (its own
            // path is marked hidden; FCS treats an abbreviation as file-private
            // and backtracks, probe CF10, but sema does not model that) or a
            // module declaring value-space names we cannot enumerate (an active
            // pattern, …): "owns nothing named `ty`" is unprovable.
            if self.module_has_hidden_values(&full) {
                return Some(SameFileQualified::DeferStop);
            }
            // The open supplying the *current* container (or an ancestor) as the
            // head is the FS0039 own-name shape at one remove — unprobed for
            // cross-file targets too; defer rather than emit or fall through
            // (mirrors the same-file arms above).
            if self.container_path.starts_with(&full) {
                return Some(SameFileQualified::DeferStop);
            }
            // THIS file's own declarations under the target (a namespace block
            // shared with an earlier file): any declaration of `ty` there is
            // contention, conservatively deferred rather than classified — the
            // merged multi-file view is not modeled. Probe CF9 (a same-file
            // `type Color` without the member) shows FCS can still backtrack
            // past it, so this only over-defers, never mis-targets.
            if self
                .container_decls
                .get(&full)
                .is_some_and(|d| d.contains_key(ty))
            {
                return Some(SameFileQualified::DeferStop);
            }
            let mut ty_path = full.clone();
            ty_path.push(ty.to_string());
            // A value / constructor at the segment commits member access (the
            // same-file `is_dottable_value` rule; Gap C's value-wins probes).
            if self
                .items
                .iter()
                .any(|i| i.qualified.as_deref() == Some(ty_path.as_slice()))
                || self
                    .preceding
                    .lookup_qualified_path(&ty_path, &self.container_path)
                    .is_some()
            {
                return Some(SameFileQualified::DeferStop);
            }
            // A companion submodule / deeper namespace at the segment can own
            // the case itself — FCS resolves `Pal.Color.Red` through the open to
            // a cross-file submodule's own `let Red` (probe CF11) — and its
            // members are not classified here: defer, never fall through. The
            // *real*-module index, not the conflated name-shadow set: a
            // cross-file `type Color`'s shadow must not defer here (the type
            // rule below owns it). Same-file entities under the target are
            // namespace-shared shapes the `container_decls` arm above already
            // deferred, except a same-file `module <full…, ty>` *header*, which
            // never enters `container_decls` — hence the `module_paths` check.
            if self.module_paths.iter().any(|p| p == &ty_path)
                || self.preceding.is_exact_project_module(&ty_path)
                || self.preceding.is_real_nested_module(&ty_path)
                || self.is_project_namespace_path(&ty_path)
            {
                return Some(SameFileQualified::DeferStop);
            }
            // A cross-file type at the segment (the type index). With its case
            // set fully indexed and the positive hit above missed, the type owns
            // no such case — in pattern position it then commits nothing (a
            // static member is not a pattern; FCS backtracks, probe CF3pat), so
            // fall through. In expression position unmodeled members may commit
            // (the Bexpr sacrifice: FCS backtracked in probe CF3expr, but sema
            // cannot prove member absence), and an abbreviation's cases live on
            // its unchased target (FCS commits through it, probes CF8/CF8pat),
            // so both defer.
            match self.preceding.exported_type_at(&ty_path) {
                Some(cases_enumerable) if in_pattern && cases_enumerable => {}
                Some(_) => return Some(SameFileQualified::DeferStop),
                // Anything ELSE the conflated name-shadow set holds at the
                // segment (an `extern` prototype, an earlier file's module
                // abbreviation, a future contributor of
                // `record_project_name_shadow`) is a project-introduced name
                // none of the arms above positively classified: the target is
                // not provably transparent, so defer — falling through would
                // hand the residual to a lower-ranked candidate FCS may never
                // reach (e.g. an extern `Color` commits `A.Pal.Color` as the
                // value; only illegal residuals were observed, but the
                // catch-all keeps every unclassified shadow conservative).
                None if self.preceding.is_exact_nested_module(&ty_path) => {
                    return Some(SameFileQualified::DeferStop);
                }
                None => {}
            }
            // Nothing project-side owns `ty` — fall through to the assembly arm.
        }
        // An assembly type (module / static class) or namespace: the env is
        // complete for referenced assemblies, so absence at `ty` is provable.
        if self.opened_assembly_type(&full).is_some() || self.assemblies.has_namespace(&full) {
            let mut ty_path = full.clone();
            ty_path.push(ty.to_string());
            let owns = self.opened_assembly_type(&ty_path).is_some()
                || self.assemblies.has_namespace(&ty_path);
            return owns.then_some(SameFileQualified::DeferStop);
        }
        None
    }

    /// Classify the residual `Color.Red` of a module-qualified `Pal.Color.Red`
    /// **within one resolved same-file module candidate** (a step of
    /// [`classify_same_file_module_qualified_case`](Self::classify_same_file_module_qualified_case)'s
    /// candidate loop). `None` means the candidate does not own the residual at all
    /// — FCS then tries the next same-named module outward, so the caller continues
    /// the walk. `Some(outcome)` ends the search:
    ///
    /// - A dottable value / ctor at the segment — or, in *pattern* position, an
    ///   active-pattern case (it joins the constructor namespace) — is same-file
    ///   member access / contention → [`DeferStop`](SameFileQualified::DeferStop).
    /// - A type carrying the modeled case → [`Emit`](SameFileQualified::Emit)
    ///   (FCS: the type's case wins even over a companion `module Color` value).
    /// - In **expression** position, a type *without* the modeled case →
    ///   `DeferStop`: FCS consults the type's members **before** a companion
    ///   module's contents and before searching outward (probed: `type Color()`
    ///   with `static member Red` wins over both `module Color = let Red` and an
    ///   earlier file's export), and sema cannot prove a type has no member `Red`
    ///   (an augmentation can add one later in the file) — so it deliberately
    ///   over-defers the member-less shape (where FCS resolves the companion or
    ///   searches outward): a sound availability sacrifice.
    /// - In **pattern** position a memberless-or-not type never commits (a static
    ///   member is not a pattern — FCS-probed), so a type without the case falls
    ///   to the companion checks and then to `None` (outward).
    /// - A companion **submodule** `Pal.Color` declaring `Red`
    ///   ([`Self::container_decls`] under the extended path — complete, so absence
    ///   is provable): same-file. An expression-position companion `let Red` is
    ///   exactly what the qualified-value branch resolves →
    ///   [`Miss`](SameFileQualified::Miss) (delegate to it, stop the walk); any
    ///   other companion `Red` (a union case a two-level head would own, a
    ///   pattern-bindable literal, …) is same-file but unmodeled → `DeferStop`.
    fn classify_module_qualified_segment(
        &self,
        module_path: Vec<String>,
        ty: &str,
        case: &str,
        in_pattern: bool,
        use_pos: TextSize,
    ) -> Option<SameFileQualified> {
        let seg = self
            .container_decls
            .get(&module_path)
            .and_then(|d| d.get(ty))
            .copied()
            .unwrap_or_default();
        if seg.is_dottable_value() || (in_pattern && seg.active_pattern) {
            return Some(SameFileQualified::DeferStop);
        }
        // Is the same-file type `ty` inaccessible from the reference site? A
        // `private` type referenced from a *sibling* module (FCS FS1092) contributes
        // NEITHER its case NOR its static member. This is a **guard on those two
        // branches only**, not a `return`: the type simply drops out of the candidate
        // set, while everything else about this candidate is unchanged. In particular
        // a same-named **companion module** stays accessible and must still be
        // classified below (FCS binds an inner companion `let Red` even when the
        // sibling type is private — `Top.Nest.A.FooModule.Red`), and if nothing here
        // owns the residual the walk continues outward (a farther same-file module, an
        // earlier `open`, then a cross-file `A.Foo` — all accessibility-gated, #1000).
        // Returning early (`None`/`Miss`/`DeferStop`) each skips a different one of
        // those downstream branches and misbinds (codex); guarding subtracts exactly
        // the two type-owned emissions and nothing else.
        let type_inaccessible = self
            .type_defs
            .get(&module_path)
            .is_some_and(|m| m.contains_key(ty))
            && {
                let mut type_path = module_path.clone();
                type_path.push(ty.to_string());
                let access_root = self
                    .type_access_roots
                    .get(&module_path)
                    .and_then(|m| m.get(ty))
                    .copied()
                    .flatten();
                !super::model::accessible_from(access_root, &type_path, &self.container_path)
            };
        if !type_inaccessible {
            if let Some(&case_res) = self
                .type_cases
                .get(&module_path)
                .and_then(|m| m.get(ty))
                .and_then(|c| c.get(case))
            {
                let Some(&type_id) = self.type_defs.get(&module_path).and_then(|m| m.get(ty))
                else {
                    // Defensive: `type_cases` and `type_defs` are populated together,
                    // so a case without its type should not occur — decline same-file
                    // (`Miss`) rather than emit a case with no type target.
                    return Some(SameFileQualified::Miss);
                };
                return Some(SameFileQualified::Emit { type_id, case_res });
            }
            if !in_pattern && seg.ty {
                // A modeled emit-eligible **static member** at the segment resolves
                // (FCS-pinned, probes M1/M2a/M2d/M4b of
                // `docs/project-type-member-plan.md`; a member and a case can never
                // share a name — FS0023, probe M2b — so this never contends with the
                // case emit above). Anything else the type owns — or might own —
                // keeps the unconditional defer: unmodeled members may commit (the
                // Bexpr rule; an *instance* member commits and errors rather than
                // backtracking, probe M9).
                if let Some(member_def) =
                    self.emittable_type_member(&module_path, ty, case, use_pos)
                    && let Some(&type_id) = self.type_defs.get(&module_path).and_then(|m| m.get(ty))
                {
                    return Some(SameFileQualified::Emit {
                        type_id,
                        case_res: Resolution::Local(member_def),
                    });
                }
                return Some(SameFileQualified::DeferStop);
            }
        }
        let mut companion_path = module_path;
        companion_path.push(ty.to_string());
        match self
            .container_decls
            .get(&companion_path)
            .and_then(|d| d.get(case))
        {
            Some(k) if !in_pattern && k.value => {
                // An expression-position companion `let` value is what the
                // qualified-value branch resolves — delegate via `Miss`. But a
                // **provably inaccessible** companion value (`let private Red`, or one
                // under a `module private`) is transparent (`None`, continue the
                // walk): `Miss` there would end the same-file walk and let
                // `qualified_value_in`'s cross-file exact-path commit an earlier file's
                // `A.Foo.Red` export before a farther same-file `A.Foo.Red` is tried
                // (codex) — the same rule as an inaccessible type case/member. When
                // accessibility is *unprovable* (a headerless file's nested binding has
                // no `qualified` path), keep `Miss`: stepping the walk over a
                // possibly-accessible value would wrongly emit a farther candidate.
                let mut full = companion_path.clone();
                full.push(case.to_string());
                if self.companion_value_provably_inaccessible(&full) {
                    None
                } else {
                    Some(SameFileQualified::Miss)
                }
            }
            Some(_) => Some(SameFileQualified::DeferStop),
            None => None,
        }
    }

    /// Resolve a same-file module-qualified case for expression / pattern position,
    /// per [`classify_same_file_module_qualified_case`](Self::classify_same_file_module_qualified_case).
    /// On [`Emit`](SameFileQualified::Emit) record the whole `Pal.Color.Red` span →
    /// the case, the type segment → its def, and the leading module segment →
    /// `Deferred` (no module-as-def). Returns `true` when the path is **same-file
    /// rooted** — either recorded (`Emit`) or deferred on contention (`DeferStop`) —
    /// so the caller must STOP and not fall through to the cross-file branches.
    /// `false` ([`Miss`](SameFileQualified::Miss)) lets the caller continue.
    /// `in_pattern` distinguishes pattern position (where an active-pattern segment
    /// contends) from expression position.
    fn record_same_file_module_qualified_case(
        &mut self,
        segments: &[SyntaxToken],
        in_pattern: bool,
    ) -> bool {
        match self.classify_same_file_module_qualified_case(segments, in_pattern) {
            SameFileQualified::Emit { type_id, case_res } => {
                let [head_seg, type_seg, case_seg] = segments else {
                    return false;
                };
                let whole =
                    TextRange::new(head_seg.text_range().start(), case_seg.text_range().end());
                self.record(whole, case_res);
                self.record(type_seg.text_range(), Resolution::Local(type_id));
                self.record(
                    head_seg.text_range(),
                    Resolution::Deferred(DeferredReason::QualifiedAccess),
                );
                true
            }
            SameFileQualified::DeferStop => true,
            SameFileQualified::Miss => false,
        }
    }

    /// Resolve a qualified case **pattern** head (`Color.Red`, `Lib.Color.Red`) the
    /// way FCS does — identically to the expression form. A 2-segment head that
    /// names an in-file type resolves same-file ([`Self::record_type_case_path`]:
    /// head → the type def, whole → the case); otherwise the whole written path is
    /// looked up in the cross-file type-qualified-case index
    /// ([`Self::cross_file_type_case`]), recording the whole span → the case and the
    /// qualifier segments → `Deferred` (no cross-file type def to point the head at).
    ///
    /// The whole resolution — same-file and cross-file — is gated like the
    /// expression path ([`resolve_long_ident`](Self::resolve_long_ident)): an opaque
    /// or `open type` open could supply the head `Color` as an unmodelled
    /// module/type/value that out-ranks the type for the qualifier, so defer while
    /// one is in scope.
    pub(super) fn record_qualified_case_pattern(&mut self, segs: &[SyntaxToken]) {
        if self.opaque_value_open || self.opaque_dotted_open || self.unmodelled_open_active {
            return;
        }
        // A `global.`-rooted head (now parseable — see `pat.rs`) is the
        // namespace-root marker (FCS's `MangledGlobalName`), not a real segment.
        // Rooted *pattern* resolution isn't implemented yet, so DEFER rather than
        // let the `id_text`-normalising same/cross-file helpers below mis-resolve
        // it — e.g. to a stray escaped ``global`` module's case. The check is on
        // the *raw* keyword text (`global` is a keyword, so an ordinary module
        // can't be named it without backticks; an escaped ``global`` head reads
        // as ``global`` here and is unaffected). Follow-up: real rooted lookup,
        // threading rooting through these helpers as the module path does in
        // `decls.rs`.
        if segs.first().is_some_and(|t| t.text() == "global") {
            return;
        }
        if let [type_seg, case_seg] = segs
            && self.record_type_case_path(type_seg, case_seg)
        {
            return;
        }
        // A same-file module-qualified case `Pal.Color.Red` (Gap A), tried before the
        // cross-file index — a same-file type shadows an earlier file's same-named one.
        // Pattern position (`in_pattern = true`): an active-pattern segment contends.
        if self.record_same_file_module_qualified_case(segs, true) {
            return;
        }
        if segs.len() >= 2 {
            // A leading `global` was deferred above, so `segs` here is an ordinary
            // (unrooted) qualified path.
            let written: Vec<String> = segs.iter().map(|t| id_text(t.text()).to_string()).collect();
            // Stage-1 signature screen (project side), the pattern-position
            // twin of the expression gate: a possibly-signature-exposed
            // reading outranks the cross-file case candidate, so defer.
            if self.sig_screens_reading_of(&written) {
                return;
            }
            if let Some(id) = self.cross_file_type_case(&written, false) {
                let (first, last) = (
                    segs.first().expect("non-empty"),
                    segs.last().expect("non-empty"),
                );
                let whole = TextRange::new(first.text_range().start(), last.text_range().end());
                self.record(whole, Resolution::Item(id));
                for seg in &segs[..segs.len() - 1] {
                    self.record(
                        seg.text_range(),
                        Resolution::Deferred(DeferredReason::QualifiedAccess),
                    );
                }
            }
        }
    }

    /// Record a `Type.Case` qualifier hit: the head segment → the *type* def and
    /// the whole `Type.Case` span → the case (its canonical [`Resolution`]), as FCS
    /// reports them.
    pub(super) fn record_type_qualifier(
        &mut self,
        type_seg: &SyntaxToken,
        case_seg: &SyntaxToken,
        type_id: DefId,
        case_res: Resolution,
    ) {
        self.record(type_seg.text_range(), Resolution::Local(type_id));
        let whole = TextRange::new(type_seg.text_range().start(), case_seg.text_range().end());
        self.record(whole, case_res);
    }

    /// The source range of the latest in-scope **value** named `name` (a value /
    /// parameter / local / pattern binder), or `None` if none is in scope. Used
    /// to break a value-vs-type qualifier collision by source order (latest
    /// wins): see the enum-case branch of [`resolve_long_ident`](Self::resolve_long_ident).
    pub(super) fn value_def_range(&self, name: &str) -> Option<TextRange> {
        self.value_resolution_def_range(self.lookup(name)?)
    }

    /// The in-file definition range behind a value-frame resolution — the
    /// range half of [`value_def_range`](Self::value_def_range), for callers
    /// that already hold the resolution.
    fn value_resolution_def_range(&self, res: Resolution) -> Option<TextRange> {
        match res {
            Resolution::Local(id) => Some(self.defs[id.index()].range),
            Resolution::Item(id) => {
                let local = id.index().checked_sub(self.item_base as usize)?;
                Some(self.defs[self.items.get(local)?.def.index()].range)
            }
            // `lookup` only ever yields in-file `Local` / `Item` value-frame
            // entries; other kinds have no in-file value range to compare.
            Resolution::Entity(_)
            | Resolution::Member { .. }
            | Resolution::Deferred(_)
            | Resolution::Unresolved => None,
        }
    }

    /// The [`DefKind`] a *same-file* case resolution names — a `Local` def, or a
    /// same-file `Item`'s def. Used to apply the union-vs-enum value-collision rule
    /// for a `Type.Case` qualifier (a same-file case is `UnionCase` / `EnumCase`).
    pub(super) fn case_resolution_kind(&self, res: Resolution) -> Option<DefKind> {
        let def_id = match res {
            Resolution::Local(id) => id,
            Resolution::Item(id) => {
                let local = id.index().checked_sub(self.item_base as usize)?;
                self.items.get(local)?.def
            }
            _ => return None,
        };
        Some(self.defs[def_id.index()].kind)
    }

    /// For a qualified `Type.Case` path, the `(type_def, case_resolution)` when
    /// `type_name` is an in-file union/enum type visible from the current container
    /// with a case `case_name`. `None` otherwise — the head is not an in-file type,
    /// or it has no such case (then the path defers, never resolving to a wrong
    /// member). The case resolution is the case's canonical identity (a
    /// [`Resolution::Item`] in a real-root file, else [`Resolution::Local`]).
    ///
    /// Walks the container path outward like [`lookup_type_def`](Self::lookup_type_def):
    /// the **innermost** match of that name fixes the result. A *module-like* name
    /// (nested module / abbreviation, [`Self::module_like_names`]) at a level
    /// shadows any enclosing type for member access, so the path defers there
    /// (its members are unmodelled). Otherwise the innermost in-file *type* fixes
    /// it, and only *its* enum cases — in the same container — are reachable: a
    /// nested module sees an enclosing namespace's enum, but an inner non-enum
    /// `Color` (or a module `Color`) shadowing an enclosing enum `Color` makes
    /// `Color.Red` defer. Cross-file `A.Color.Red` is a later slice.
    pub(super) fn type_case_path(
        &self,
        type_name: &str,
        case_name: &str,
    ) -> Option<(DefId, Resolution)> {
        let tname = id_text(type_name);
        let cname = id_text(case_name);
        for k in (0..=self.container_path.len()).rev() {
            let prefix = &self.container_path[..k];
            // A module-like `Color` at this level shadows any enclosing type for
            // member access — its members are unmodelled, so the path defers.
            if self
                .module_like_names
                .get(prefix)
                .is_some_and(|names| names.contains(tname))
            {
                return None;
            }
            let Some(&type_id) = self.type_defs.get(prefix).and_then(|m| m.get(tname)) else {
                continue; // no `Color` of any kind here — try the enclosing container
            };
            // Innermost type of this name: it shadows enclosing ones, so resolve
            // only when it is the type whose case this is (else the path defers).
            let case_res = self
                .type_cases
                .get(prefix)
                .and_then(|m| m.get(tname))
                .and_then(|c| c.get(cname))
                .copied();
            return case_res.map(|cres| (type_id, cres));
        }
        None
    }

    /// Order the definite value at `head` (already `id_text`-normalised)
    /// against every in-scope **type** of that name, deciding [`HeadSlot`]:
    ///
    /// - same-file written types along the container chain
    ///   ([`Self::type_defs`] — every entry there was declared before this
    ///   use, the resolver walks decls in source order), at their declaration
    ///   position;
    /// - types supplied by an explicit namespace `open` — a project type
    ///   exported at `prefix + [head]` by an earlier file, or by an earlier
    ///   same-file namespace block — at the **open's** position: FCS enters
    ///   them in the slot where the open is written (probes M20h/M20i).
    ///
    /// Only a type whose name actually enters the slot counts
    /// ([`SlotClass`], codex round 1): a class/struct/enum evicts, a plain
    /// union/record/interface never does (probes M20k/M20l/M20o), and a
    /// statically-undecidable repr (abbreviation, delegate, unspecified-kind
    /// object model) makes the contest [`HeadSlot::Unordered`]. The latest
    /// eviction-relevant position is compared against the value's
    /// ([`Self::value_def_range`]); a value with no in-file position (an
    /// opened module's value) cannot be ordered either, so any
    /// possibly-evicting type alongside one is likewise `Unordered` —
    /// mirroring the member branch's positionless defer. Deliberately **not**
    /// consulted (each keeps today's behaviour, an availability/mis-record
    /// boundary of the same family as the M19 residual-blindness): assembly
    /// types under an explicit namespace open, and anything an
    /// opaque/unmodelled open might supply — the qualified block is barred
    /// while one is in scope, so a proven eviction could not resolve anything
    /// anyway.
    fn head_value_slot(&self, head: &str) -> HeadSlot {
        // The latest in-scope type position per class: `Keeps` types are
        // ignored outright (they never occupy the slot).
        let mut evictor_pos: Option<u32> = None;
        let mut unknown_pos: Option<u32> = None;
        let mut note = |pos: u32, class: SlotClass| match class {
            SlotClass::Evicts => evictor_pos = Some(evictor_pos.map_or(pos, |p| p.max(pos))),
            SlotClass::Unknown => unknown_pos = Some(unknown_pos.map_or(pos, |p| p.max(pos))),
            SlotClass::Keeps => {}
        };
        for k in 0..=self.container_path.len() {
            let prefix = &self.container_path[..k];
            if let Some(&id) = self.type_defs.get(prefix).and_then(|m| m.get(head)) {
                let class = self
                    .type_slot_classes
                    .get(prefix)
                    .and_then(|m| m.get(head))
                    .copied()
                    .unwrap_or(SlotClass::Unknown);
                note(u32::from(self.defs[id.index()].range.start()), class);
            }
        }
        // Namespace opens AND project module opens both import *project* types
        // into the slot (a module open's `M.Color` class evicts exactly like a
        // namespace-supplied one — probe M20v, codex round 8), so both prefix
        // lists feed the project checks below.
        let open_prefixes = self
            .explicit_open_prefixes
            .iter()
            .chain(&self.module_open_prefixes);
        for (pos, prefix) in open_prefixes {
            let mut path = prefix.clone();
            path.push(head.to_string());
            // The LAST same-path export wins — a redefinition (illegal, but
            // live mid-edit: FS0037 within a file, FS0248 across files)
            // shadows every earlier definition, and every other consumer of a
            // redefined type is last-wins (`define_type`,
            // `ProjectItems::extend_with`) — so a stale earlier class must
            // not decide the slot: within the file take the latest entry
            // (codex round 2), and a same-file export shadows an earlier
            // file's at the same path entirely (codex round 6).
            if let Some((_, _, class)) = self.type_path_exports.iter().rfind(|(p, _, _)| p == &path)
            {
                note(*pos, *class);
            } else if let Some(class) = self.preceding.exported_type_slot_class(&path) {
                note(*pos, class);
            } else if self
                .type_defs
                .get(prefix.as_slice())
                .is_some_and(|m| m.contains_key(head))
            {
                // A same-file type filed under the opened module's container
                // path but absent from the exports — an ANONYMOUS-ROOT
                // module's type (`export_type_path` skips those files). Its
                // slot class is in `type_slot_classes`, but the export-side
                // `private` downgrade (probe M20r) never ran, so treat the
                // contest as undecidable rather than trust it.
                note(*pos, SlotClass::Unknown);
            }
        }
        // …and a referenced-assembly type at an opened path. Assembly namespace
        // types occupy FCS's slot through every *reading*
        // ([`Self::assembly_open_prefixes`]): `open System`'s class `Math`
        // (A1–A6), a project-namespace open, and a *direct* `open Demo` merging
        // a project module with the assembly namespace `Demo` (codex round 2) —
        // but NOT a module alias, which produces no reading (codex round 1).
        // Consulted **in addition** to the project checks above: a merged path
        // can hold both, and a class there evicts even if a co-named project
        // type keeps (under-eviction is the only unsound direction). An evicted
        // head then defers — the assembly branch of the qualified block is
        // barred for it (the M20t/M20u rule), converting a wrong-target into a
        // sound defer.
        for (pos, prefix) in &self.assembly_open_prefixes {
            for (kind, is_struct) in self.assemblies.public_types_named(prefix, head) {
                note(*pos, assembly_slot_class(kind, is_struct));
            }
        }
        // The value's slot position. An OPENED value sits at its `open`'s
        // offset — FCS enters it in the slot where the open is written, so an
        // `open M` after the type re-takes the slot for `M.Color` and one
        // before it loses (probes M20p/M20q, codex round 3) — while a source
        // binding sits at its definition's.
        let (value_pos, value_from_open) = match self.lookup_entry(head) {
            Some(entry) if entry.from_open => (Some(entry.open_pos), true),
            Some(entry) => (
                self.value_resolution_def_range(entry.resolution)
                    .map(|r| u32::from(r.start())),
                false,
            ),
            None => (None, false),
        };
        match value_pos {
            Some(value_pos) => {
                // An EQUAL position means one `open` supplied both the value
                // and the type (a multi-reading open: a root module's value +
                // a relative namespace's type; written bindings can never
                // tie — distinct tokens never share an offset). FCS breaks
                // the tie by reading priority (the higher reading's TYPE
                // wins — probe M20x, codex round 9), which the slot ordering
                // does not model — undecidable, defer.
                let same_open_tie = |p: u32| value_from_open && p == value_pos;
                if evictor_pos.is_some_and(|p| p > value_pos) {
                    HeadSlot::Evicted
                } else if unknown_pos.is_some_and(|p| p > value_pos)
                    || evictor_pos.is_some_and(same_open_tie)
                    || unknown_pos.is_some_and(same_open_tie)
                {
                    HeadSlot::Unordered
                } else {
                    HeadSlot::Held
                }
            }
            // A value with no derivable position: any possibly-evicting type
            // in scope makes the contest undecidable.
            None if evictor_pos.is_some() || unknown_pos.is_some() => HeadSlot::Unordered,
            None => HeadSlot::Held,
        }
    }

    /// Whether a **project module** owns `head` at any completion the bare
    /// head can reach — the relative tiers of the same-file candidate walk
    /// (`k >= namespace_depth`, plus the root) **and** every explicit
    /// `open <prefix>` in scope. FCS binds a project `module Color`'s value
    /// for `Color.Red` over a same-file type's member or case — whether the
    /// module arrives cross-file at a written completion (probes M13/M14 of
    /// `docs/project-type-member-plan.md`) or through an `open`, even one
    /// *earlier* in source than the type (probes M15/M16, codex round 2) —
    /// the r13 "the module namespace owns dotted heads" rule, position-blind.
    /// A contested head makes the type-qualifier emits stand down: the
    /// qualified-value / opens machinery then resolves the module's value,
    /// and other shapes defer through the shadow indexes exactly as before
    /// the emits existed. The conflated name-shadow set is consulted
    /// deliberately — any project-introduced name at a reachable completion
    /// counts as contested; over-standing-down only costs availability,
    /// never a wrong target. (Assembly modules are NOT consulted: the
    /// pre-existing pin has the same-file type shadowing an assembly path.)
    fn head_contested_by_project_module(&self, head: &str) -> bool {
        let head = id_text(head);
        let relative = (self.namespace_depth.max(1)..=self.container_path.len()).rev();
        let root_tier: &[usize] = &[0];
        let written = relative
            .chain(root_tier.iter().copied())
            .map(|k| self.container_path[..k].to_vec());
        let opened = self
            .explicit_open_prefixes
            .iter()
            .map(|(_, prefix)| prefix.clone());
        for mut path in written.chain(opened) {
            path.push(head.to_string());
            // The current container (or an ancestor) does not contest: a
            // module's own name is not in scope as a head within itself — the
            // FS0039 own-name rule the candidate walk applies (`module Color;
            // type Color = Red = 0 …; Color.Red` binds the enum case, probe
            // M17, codex round 3).
            if self.container_path.starts_with(&path) {
                continue;
            }
            // ONLY real modules contest — same-file headers +
            // `real_nested_module_exports`, cross-file module headers + real
            // nested modules. Everything else a completion could hold is
            // deliberately absent, because standing down is not a defer here
            // (the fall-through can EMIT through the cross-file machinery), so
            // over-inclusion produces wrong targets or lost resolutions:
            // - NAMESPACES cannot own a 2-segment *expression* residual at all
            //   (values never live directly under one) — FCS backtracks to the
            //   lexical type (probe M18, codex round 4).
            // - The conflated name-shadow set holds cross-file TYPES (via
            //   `namespace global` even at root completions) — a cross-file
            //   type never outranks the lexical type (the CF12/CF13 principle;
            //   probe M19, codex round 5: vetoing here handed the residual to
            //   an earlier file's same-written-path case, a wrong target),
            //   cross-file module ALIASES (file-private to FCS, probe CF10 —
            //   they must not contest), and `extern`s (a value cannot own the
            //   residual: member access on it commits and errors, probe M9's
            //   principle — no legal contest exists).
            if self.module_paths.iter().any(|p| p == &path)
                || self.real_nested_module_exports.iter().any(|p| p == &path)
                || self.preceding.is_exact_project_module(&path)
                || self.preceding.is_real_nested_module(&path)
            {
                return true;
            }
        }
        false
    }

    /// The emit target for member `name` of the type `ty` declared in
    /// `container`, if the member index can answer **and** the answer is
    /// emit-eligible at `use_pos`: not name-suppressed by an unfiled
    /// augmentation, not a type with suppressed emission (`inherit`,
    /// unextractable member names), a known emittable static, and already
    /// visible (an augmentation's members do not exist before it — probe M4a).
    /// `None` means "do not emit" — the caller keeps its conservative defer;
    /// it never means "provably absent" (that is D2, a later stage).
    fn emittable_type_member(
        &self,
        container: &[String],
        ty: &str,
        name: &str,
        use_pos: TextSize,
    ) -> Option<DefId> {
        if self.unindexed_augmented_names.contains(ty) {
            return None;
        }
        let set = self.type_members.get(container)?.get(ty)?;
        if set.emit_suppressed {
            return None;
        }
        let entry = set.entries.get(name)?;
        if use_pos < entry.visible_from {
            return None;
        }
        entry.emit
    }

    /// The 2-segment static-member analogue of [`type_case_path`](Self::type_case_path):
    /// resolve `Color.Red` where `Color` is an in-file type and `Red` an
    /// emit-eligible static member, walking the container chain innermost-first
    /// with the same module-like shadowing rule. Returns the type's def and the
    /// member's def. The innermost type of the name owns the segment: when it
    /// lacks an emittable member the path declines (defers downstream) rather
    /// than trying an enclosing type.
    fn type_member_path(
        &self,
        type_name: &str,
        member_name: &str,
        use_pos: TextSize,
    ) -> Option<(DefId, DefId)> {
        let tname = id_text(type_name);
        let mname = id_text(member_name);
        for k in (0..=self.container_path.len()).rev() {
            let prefix = &self.container_path[..k];
            if self
                .module_like_names
                .get(prefix)
                .is_some_and(|names| names.contains(tname))
            {
                return None;
            }
            let Some(&type_id) = self.type_defs.get(prefix).and_then(|m| m.get(tname)) else {
                continue;
            };
            return self
                .emittable_type_member(prefix, tname, mname, use_pos)
                .map(|member_def| (type_id, member_def));
        }
        None
    }

    /// The resolution of `name` in **value/expression** position: search frames
    /// innermost-first, and within a frame take the *latest* matching binding
    /// (position-ordered shadowing).
    ///
    /// Active-pattern *cases* are skipped: they share the value frame so the
    /// position-ordered scoping and [`case_reference`](Self::case_reference)
    /// (pattern position) come for free, but — unlike union / exception
    /// constructors — a case name is **not** a value in expression position
    /// (`let v = Even` is FCS `FS0039`, even though `match x with Even` resolves).
    /// So an expression use skips the case entry and falls through to the latest
    /// *value* of that name (or `None` → `Deferred`); the recognizer's own body
    /// constructing a case (`… then Even`) is then a sound coverage gap, never a
    /// wrong resolution. (Union / exception cases are *not* skipped — they are
    /// genuine value constructors: `let c = Red` resolves.) The recognizer-body
    /// decline for a *bare* case-name expression is handled separately, in
    /// [`resolve_name_use`](Self::resolve_name_use) via
    /// [`ap_body_case_names`](Self::ap_body_case_names), so it never touches this
    /// shared lookup (and hence never a qualified head).
    ///
    /// **Opened** entries ([`ScopeEntry::from_open`]) are skipped while an
    /// [`opaque_value_open`](Self::opaque_value_open) is in scope: that open could
    /// shadow the opened name with a value we cannot enumerate, so the modelled
    /// opened resolution might be wrong — declining (and falling through to an
    /// earlier non-opened binding, or `Deferred`) is sound. An opened entry is
    /// also skipped once its [`generation`](ScopeEntry::generation) is *stale* —
    /// a later `open M` with unmodelled value-namespace members (union cases /
    /// exception constructors / active patterns we cannot enumerate) bumped
    /// [`open_generation`](Self::open_generation), conservatively shadowing every
    /// earlier opened name (F#: the latest open wins). With no opaque open in
    /// scope and a current generation, opened statics / module values compete by
    /// source order like any other entry, so the latest in scope wins.
    pub(super) fn lookup(&self, name: &str) -> Option<Resolution> {
        self.lookup_entry(name).map(|entry| entry.resolution)
    }

    /// The frame walk behind [`lookup`](Self::lookup), returning the winning
    /// [`ScopeEntry`] itself — [`head_value_slot`](Self::head_value_slot)
    /// needs its [`from_open`](ScopeEntry::from_open) /
    /// [`open_pos`](ScopeEntry::open_pos) to order the value at the position
    /// FCS's slot uses (the `open`'s, for an opened value — probes
    /// M20p/M20q). One walk so the two can never disagree on the skipping
    /// rules.
    ///
    /// A generation-**stale** entry is not returned (codex round 22): a later
    /// open with names we cannot list shadows every earlier entry — opened AND
    /// lexical — because FCS's environment is latest-wins across bindings and
    /// opens alike, so `let Hit = 1; open M` binds M's hidden `Hit` when it has
    /// one. A binding after the bump carries the new generation and wins.
    /// Filtering the latest match is equivalent to skipping and scanning on:
    /// generations only grow along push order and the walk meets entries
    /// newest-first, so everything past a stale match is at least as stale.
    fn lookup_entry(&self, name: &str) -> Option<&ScopeEntry> {
        self.latest_entry(name)
            .filter(|e| e.generation == self.open_generation)
    }

    /// The latest in-scope value entry for `name` **regardless of generation
    /// staleness** — [`Self::lookup_entry`] minus its staleness filter, and the
    /// walk both share (so they can never disagree on the other skipping
    /// rules). Exists so [`Self::head_entry_staled`] can distinguish "no entry
    /// at all" from "an entry the barrier staled".
    fn latest_entry(&self, name: &str) -> Option<&ScopeEntry> {
        for frame in self.scopes.iter().rev() {
            for entry in frame.entries.iter().rev() {
                if entry.name != name {
                    continue;
                }
                if entry.pattern_only {
                    continue; // constructor namespace only — not a value here
                }
                if entry.from_open && self.opaque_value_open {
                    // An opaque open could shadow this opened name.
                    continue;
                }
                if let Resolution::Local(id) = entry.resolution
                    && self.defs[id.index()].kind == DefKind::ActivePattern
                {
                    continue; // pattern-only: not a value in expression position
                }
                return Some(entry);
            }
        }
        None
    }

    /// Whether the latest in-scope entry for `head` was **staled by a
    /// generation barrier** — it exists, but [`lookup`](Self::lookup) skips it
    /// because a later open bumped the generation. A *dotted* path through such
    /// a head must DEFER rather than resolve through the qualified channels
    /// (codex round 10): the barrier is coarse — it stales every earlier entry,
    /// not just the names its open actually contests — so FCS may well still
    /// bind the staled entry (an unrelated local; the cross-kind type barrier
    /// never evicts those), making any target the qualified block reaches past
    /// it a wrong go-to-definition. A head with **no** entry at all is
    /// unaffected: the barrier staled nothing the qualified channels could
    /// mistake, so an enumerated group's own names (its namespace half's types
    /// included) keep resolving. Hidden-name groups (residue) still need the
    /// blanket [`opaque_dotted_open`](Self::opaque_dotted_open) — a name we
    /// cannot list could be a head with no entry to go stale.
    pub(super) fn head_entry_staled(&self, head: &str) -> bool {
        self.latest_entry(head)
            .is_some_and(|e| e.generation != self.open_generation)
    }
}

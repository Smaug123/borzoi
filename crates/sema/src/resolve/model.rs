//! Public output model of name resolution. These are the inert result types
//! [`resolve_file`](super::resolve_file) and
//! [`resolve_project`](super::resolve_project) return — [`ResolvedFile`],
//! [`ResolvedProject`], the per-name [`Resolution`], and the cross-file
//! [`ProjectItems`] index threaded through the Compile-order fold — together
//! with the [`ExportedItem`] handles ([`ItemId`]) they index. They carry no
//! resolver state; the walk that produces them lives in the parent
//! [`resolve`](super) module.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rowan::{TextRange, TextSize};

use crate::assembly_env::{AssemblyEnv, EntityHandle, MemberIndex};
use crate::def::{Def, DefId, SemanticClass};
use crate::diagnostics::SemaDiagnostic;

use super::state::ActivePatternShape;

/// One export recorded at a qualified path, in the Compile order
/// [`ProjectItems::extend_with`] appends them (file order, then source order
/// within a file) — so the *last* [`ExportRecord`] in a path's history is the
/// source-latest export there.
///
/// Keeping the whole history (rather than a latest-wins id) is what lets a
/// query pick the latest export a reference site can *access*: a public export
/// shadowed at the same path by a later inaccessible `private` redeclaration is
/// still present as an earlier record, so [`ProjectItems::latest_accessible_value`]
/// finds it. `access_root_len` / `is_case` carry the two facts the accessibility
/// and namespace predicates need without a second lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExportRecord {
    /// The exported binding's project-global handle.
    id: ItemId,
    /// This export's accessibility, as a prefix length of its own qualified path
    /// (see [`ExportedItem::access_root_len`]): `None` = public; `Some(k)` =
    /// accessible only from a site within the `k`-segment prefix of the path.
    access_root_len: Option<usize>,
    /// Whether the export is a **constructor case** (union case / `exception`
    /// constructor / active-pattern case), i.e. live in the constructor (pattern)
    /// namespace. Union/exception cases are *also* values; an active-pattern case
    /// is not (`pattern_only`).
    is_case: bool,
    /// Whether the export is **pattern-namespace-only** — an active-pattern case
    /// (Stage 3a, `docs/export-decl-model-plan.md`). Such a case rides this history
    /// so it inherits the constructor namespace's Compile-order provenance and
    /// accessibility recovery (a plain union case does the same), but a bare use in
    /// *expression* position is FS0039, so every value-namespace query excludes it
    /// ([`Self::latest_accessible_value`] et al.). `false` for a value or a
    /// value-live union/exception case.
    pattern_only: bool,
}

/// One module root a signature file constrains: the module's qualified path
/// and whether the *signature* marks it `[<AutoOpen>]` (conclusion 6 of the
/// probe sweep: the signature's attribute is authoritative).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SigRoot {
    pub(super) path: Vec<String>,
    pub(super) auto_open: bool,
}

/// What a `.fsi` signature file contributes to the Compile-order fold in
/// Stage 1 of `docs/fsi-signature-restriction-plan.md`: not exports (Stage 2),
/// but a **screen** — the roots it constrains plus a deliberately
/// over-approximated set of every name it could expose (each non-trivia
/// token's `idText`, plus its ident-shaped pieces). The screen's one job is to
/// keep the fold from committing a *referenced-assembly* member at a path the
/// signature may expose: FCS binds the `.fsi` there (probe: sig-exposed
/// `Shared.shown` with a colliding `RefLib.dll` → the `.fsi`), and Stage 1
/// has no signature identity to commit, so the honest verdict is `Deferred`.
/// A name absent from the whole signature text provably cannot be exposed, so
/// it falls through to the merged assembly exactly as FCS does (probe:
/// hidden `Shared.bar` → the assembly). Over-approximation errs toward
/// deferral — availability, never a wrong commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SigScreen {
    /// The module paths the signature constrains: top-level `module M.N`
    /// headers, and modules declared directly under a `namespace` fragment.
    pub(super) roots: Vec<SigRoot>,
    /// Every name the signature could possibly expose (over-approximate).
    pub(super) names: HashSet<String>,
    /// Paths of `[<AutoOpen>]` modules the signature declares directly under a
    /// `namespace` fragment. Folded (at the paired implementation's slot) into
    /// [`ProjectItems::auto_open_module_paths`] so a later file's `open` of
    /// the namespace sees the auto-open even when only the signature carries
    /// the attribute (conclusion 6).
    pub(super) auto_open_nested: Vec<Vec<String>>,
}

/// Exported project items visible to a file from *earlier* in Compile order.
///
/// F# resolution is order-sensitive across files: a file may reference
/// definitions from itself and earlier files only. The Compile-order fold
/// ([`resolve_project`](super::resolve_project)) threads this accumulator through each file —
/// [`resolve_file`](super::resolve_file) consults it for cross-file references and the fold grows it
/// with each file's exports afterwards (`docs/type-checker-plan.md` D2).
///
/// Only *module-qualified* exports are indexed for cross-file lookup: a
/// bare-name cross-file reference is illegal in F# without an `open` (each
/// header-less file is its own implicit module), and `open` resolution is a
/// later slice. So a later file resolves an earlier file's `let` only through a
/// qualified path (`Shared.foo`), keyed here by that dotted name.
#[derive(Debug, Default, Clone)]
pub struct ProjectItems {
    /// Module-qualified export *path* (`["Shared", "foo"]`) → its **full export
    /// history** in Compile order (see [`ExportRecord`]). Keyed by the segment
    /// vector, not a dotted string: a quoted identifier may itself contain a `.`
    /// (`` `B.x` `` is one segment), so joining would conflate `A.``B.x``` with the
    /// genuine path `A.B.x`.
    ///
    /// Every export at a path is kept, not just the source-latest — this replaces
    /// the old latest-wins `by_qualified_path` (value namespace) and `constructors`
    /// (case namespace) maps *and* the `private_value_ids` / `public_value_paths`
    /// collapse-defer stopgaps. A single per-path history, queried by
    /// [`Self::latest_accessible_value`] / [`Self::latest_accessible_case`],
    /// subsumes all four: because no export is lost to a same-path shadow, a public
    /// export sitting under a later *inaccessible* `private` redeclaration is still
    /// selectable (the collapse the latest-wins maps could only defer), and the
    /// value/constructor namespaces read the same history through different
    /// predicates.
    pub(super) value_exports: HashMap<Vec<String>, Vec<ExportRecord>>,
    /// Declared project *module header* paths (`["Demo"]`, `["Demo", "Calc"]`).
    ///
    /// A project module header and a same-named referenced-assembly *namespace*
    /// **merge** in F# (FCS-verified): a path *under* a project module resolves
    /// in-project only when the module actually provides that member; otherwise
    /// F# falls through to the merged assembly namespace. So a module header
    /// blocks an assembly resolution only when the reference *is* the module
    /// path exactly ([`Self::is_exact_project_module`]) — a proper-prefix match
    /// (`module Demo.Calc` does not export `Answer`, ref `Demo.Calc.Answer`)
    /// falls through to the assembly, because the module does not provide the
    /// tail but the merged namespace might.
    ///
    /// Exported *values* are tracked separately in [`Self::value_exports`]:
    /// a value prefix is genuine member access on a project value, which always
    /// defers ([`Self::is_project_value_prefixed`]) — F# binds the value first,
    /// and member access then needs its type (Phase 3). Pure *namespace*
    /// prefixes (the segments above a module — `Demo` in `module Demo.Other`)
    /// are not recorded at all: namespaces merge wholesale.
    pub(super) module_headers: HashSet<Vec<String>>,
    /// Qualified paths of *nested* `module X = …` declarations
    /// (`["Demo", "Calc"]`), accumulated across earlier files. Unlike
    /// [`Self::module_headers`], these defer on a **proper prefix** too
    /// ([`Self::is_rooted_at_nested_module`]): sema does not model a nested
    /// module's members yet (parser 8.4 lands only the syntax), so a reference
    /// rooted at one (`Demo.Calc.Answer`) cannot be proven to resolve in-project
    /// — but it must not fall through to a *colliding* referenced-assembly
    /// member either (the `assembly_path_records` soundness tripwire). Deferring
    /// is sound (over-defers an unrelated assembly path that happens to share
    /// the prefix — availability, never a wrong resolution).
    pub(super) nested_module_paths: HashSet<Vec<String>>,
    /// Every earlier-file project **type**'s simple name, from each file's
    /// syntactic whole-file pre-scan
    /// ([`ResolvedFile::own_type_simple_names`]). The attribute resolution's
    /// cross-file guard (EX-3 §2(d)): a `[<Foo>]` candidate must defer when a
    /// preceding file could declare `type FooAttribute = …` — the tiered walk
    /// does not index bare cross-file project types, so without this set a
    /// cross-file alias would be invisible and the walk would wrongly commit
    /// an assembly type (or "nothing"). Syntactic rather than export-derived
    /// so a headerless file's types count too; over-approximate by design (a
    /// spurious name match only defers).
    pub(super) project_type_simple_names: HashSet<String>,
    /// Qualified paths of earlier-file **real** nested `module X = …` definitions
    /// — the module-only subset of [`Self::nested_module_paths`], which conflates
    /// every project-introduced name (types, exceptions, module abbreviations,
    /// `extern`s) for the assembly-shadow tripwire. Lets a later file ask "is
    /// there a genuine module at this path?" — a companion submodule can own a
    /// dotted residual itself (FCS resolves `Pal.Color.Red` to a cross-file
    /// submodule's own member, probe CF11), where a type's shadow must not
    /// answer (a type at an open-supplied head is transparent, probes
    /// CF12/CF13).
    pub(super) real_nested_modules: HashSet<Vec<String>>,
    /// Qualified paths of declared project **namespaces** (`["Outer", "Inner"]`
    /// for `namespace Outer.Inner`), accumulated across files. Lets
    /// [`Resolver::open_interpretations`](super::state::Resolver::open_interpretations) resolve an `open <namespace>`
    /// *relative* to the enclosing namespace — so `open Inner` inside
    /// `namespace Outer` enumerates the cases declared directly under
    /// `Outer.Inner`. (Modules merge with the assembly path; namespaces did not,
    /// until this index.)
    pub(super) namespace_paths: HashSet<Vec<String>>,
    /// Qualified paths of earlier-file modules whose `open` may bring value-space
    /// names we cannot enumerate — aliases, or modules declaring union cases /
    /// exception constructors / active patterns (see
    /// [`Resolver::modules_with_hidden_values`](super::state::Resolver::modules_with_hidden_values)). A later file's `open M` of one
    /// must bump the open generation so it shadows earlier opens.
    pub(super) modules_with_hidden_values: HashSet<Vec<String>>,
    /// Each earlier-file **non-`private`** `[<AutoOpen>]` module *fragment*,
    /// paired with the Compile-order file that declared it, in Compile order —
    /// `private` ones are excluded, since F# does not bring a `private` module
    /// into scope for another file's `open` of its namespace. Opening the
    /// containing namespace also opens a listed module; because sema does not
    /// enumerate their nested types, a bare type name under that namespace may
    /// be shadowed.
    ///
    /// The **file** is the per-fragment auto-open provenance the namespace-fold
    /// reads (Stage 5, `docs/sema-accessibility-collapse-foundation.md`): a
    /// module `A` may have `[<AutoOpen>]` fragments in several files and *plain*
    /// (un-attributed) augmentations in others. Only a member declared in an
    /// `[<AutoOpen>]`-attributed fragment is auto-opened, and it folds at *that
    /// fragment's* file, so [`Self::is_auto_open_fragment`] answers "is `(A,
    /// file)` an auto-open fragment?" per member. The same path can therefore
    /// appear more than once here, once per declaring file.
    ///
    /// A `Vec`, not a `HashSet` (codex review of §7's machinery slice): when
    /// two PRECEDING files each declare a same-named-clashing auto-open
    /// submodule of one namespace, which one's value wins the later `open`'s
    /// fold is decided by Compile order (later file wins), and a hash set's
    /// iteration order does not preserve insertion order — it would make the
    /// winner nondeterministic. [`Self::extend_with`] appends in the same
    /// Compile-order loop as every other per-file accumulator.
    pub(super) auto_open_module_paths: Vec<(Vec<String>, usize)>,
    /// The project-global [`ItemId`]s of earlier files' **constructor cases**
    /// (exported non-qualified union / exception constructors —
    /// [`ExportedItem::is_case`]). Lets a later file classify an opened cross-file
    /// [`Resolution::Item`] as a case ([`Resolver::case_classification`](super::state::Resolver::case_classification)) — for
    /// pattern-position resolution and case/module collision soundness.
    pub(super) case_item_ids: HashSet<ItemId>,
    /// The project-global [`ItemId`]s of earlier files' **attributed**
    /// module-level `let` values ([`ExportedItem::attributed`]) — maybe-literal
    /// constant-pattern contestants. A `[<Literal>]` value enters FCS's pattern
    /// namespace (`ePatItems`) and beats a case, and attribute *identity* is
    /// unverifiable from source, so attribute presence is the sound
    /// over-approximation. Read per id
    /// ([`Self::is_attributed_item`]) to flag opened value entries, and per
    /// module child path ([`Self::module_value_may_be_constant_pattern`]) to
    /// suppress a case an opened module's own maybe-literal would out-fold
    /// (FCS folds a module's vals *after* its tycons).
    pub(super) attributed_value_ids: HashSet<ItemId>,
    /// The recognizer [`ActivePatternShape`] of each earlier-file module-level
    /// **active-pattern case**, keyed by its project-global [`ItemId`] (Stage 3a,
    /// `docs/export-decl-model-plan.md`). AP cases ride [`Self::value_exports`] as
    /// **pattern-only** [`ExportRecord`]s — so they inherit the constructor
    /// namespace's Compile-order provenance and accessibility recovery exactly as a
    /// union case does — and this side map carries the shape a use site needs to
    /// split a parameterized use (`DivBy divisor`) as a same-file one does. Read by
    /// [`Self::active_pattern_shape_of`].
    pub(super) active_pattern_shapes: HashMap<ItemId, ActivePatternShape>,
    /// Each union/enum **case**'s *type-qualified* export path (`["Lib", "Color",
    /// "Red"]` = container + type + case) → its project-global handle. Parallel to
    /// [`Self::value_exports`] (the *value*-namespace history), but keyed by the
    /// path that goes *through the type* — the only way an enum or
    /// `[<RequireQualifiedAccess>]` union case is reachable, and an additional path
    /// for an ordinary union case. Lets [`Resolver::resolve_long_ident`](super::state::Resolver::resolve_long_ident) resolve a
    /// cross-file `Lib.Color.Red` / `open Lib; Color.Red` to the case. A latest-wins
    /// map: a later file shadows an earlier same path.
    pub(super) type_qualified_cases: HashMap<Vec<String>, (ItemId, Option<usize>)>,
    /// Every earlier-file **type definition**'s qualified path (`["A", "Pal",
    /// "Color"]` = container + type name) → whether its **case set is fully
    /// indexed** in [`Self::type_qualified_cases`] (`false` for an abbreviation —
    /// its cases live on a target sema does not chase cross-file — or a bodyless
    /// repr). The cross-file **type index**: unlike [`Self::type_qualified_cases`]
    /// it holds *every* exported type, case-carrying or not, so a later file can
    /// decide whether a cross-file open target's segment names a type at all —
    /// and, when the flag is `true` and the case index is silent, prove the type
    /// owns no such case (see
    /// [`Resolver::open_contests_candidate`](super::state::Resolver::open_contests_candidate)).
    /// A later file shadows an earlier same path (latest wins — a duplicate type
    /// path is FS0037-illegal anyway). Each entry also carries the type's
    /// [`SlotClass`], so a later
    /// file can decide whether `open`ing this type's container EVICTS a
    /// same-named local value from FCS's unqualified slot (probes M20h–M20o).
    pub(super) type_paths: HashMap<Vec<String>, (bool, SlotClass)>,
    /// The **signature screens** of earlier `.fsi` files, in Compile order
    /// (`docs/fsi-signature-restriction-plan.md` Stage 1). Each records the
    /// module roots a signature constrains and an over-approximation of every
    /// name the signature could expose. A dropped (signature-hidden) member
    /// falls through to a merged referenced-assembly member (FCS-probed:
    /// `Shared.bar` → the assembly), but a member the signature *may* expose
    /// must not — FCS binds the `.fsi` even when the assembly also provides
    /// the name (probe: `Shared.shown` with a colliding `RefLib` → the
    /// `.fsi`), and Stage 1 has no signature identity to commit, so such a
    /// path defers ([`Self::sig_screened_path`]). Pushed at the signature's
    /// own Compile slot, which over-defers *intervening* files (FCS resolves
    /// those to the assembly — probe; deferral is the sound direction).
    pub(super) sig_screens: Vec<Arc<SigScreen>>,
    /// Count of items interned across all earlier files. The next file's items
    /// receive project-global [`ItemId`]s starting here, so handles are unique
    /// across the whole project and a single-file caller (`default()`, count 0)
    /// still numbers its items from zero.
    pub(super) count: u32,
    /// Per-file [`ItemId`] base, in Compile order: `item_file_bases[i]` is
    /// [`Self::count`] *before* file `i`'s exports were folded, so file `i` owns
    /// the half-open id range `[item_file_bases[i], item_file_bases[i + 1])` (the
    /// last file running to [`Self::count`]). The per-name Compile-order
    /// **provenance** the cross-file namespace-straddle fold reads
    /// ([`Self::file_of`]): a name declared at both a namespace's direct tier and
    /// an `[<AutoOpen>]` submodule tier resolves by "latest Compile-order file
    /// wins", which needs each contributing export's declaring file.
    pub(super) item_file_bases: Vec<u32>,
}

impl ProjectItems {
    /// The project-global id the next interned item will receive — the base
    /// from which [`resolve_file`](super::resolve_file) numbers the file it is about to resolve.
    pub(super) fn next_base(&self) -> u32 {
        self.count
    }

    /// The Compile-order index of the file that declared `id` — the file whose
    /// half-open id range contains it (see [`Self::item_file_bases`]).
    /// `partition_point` finds the first base *strictly after* `id`, so
    /// `saturating_sub(1)` is `id`'s owning file. Correct for any id an *earlier*
    /// file exported; an id from the file currently being resolved (not yet
    /// folded) is attributed [`Self::num_files`] by the fold's same-file paths.
    pub(super) fn file_of(&self, id: ItemId) -> usize {
        let idx = u32::try_from(id.index()).expect("item index fits in u32");
        self.item_file_bases
            .partition_point(|&base| base <= idx)
            .saturating_sub(1)
    }

    /// The number of earlier files folded so far — also the Compile-order index
    /// the file *currently being resolved* occupies (it folds after all earlier
    /// files), so the straddle fold stamps a same-file contribution with this
    /// index, the latest of all.
    pub(super) fn num_files(&self) -> usize {
        self.item_file_bases.len()
    }

    /// Resolve a fully-qualified export path to the handle a reference at `site`
    /// binds, if an earlier file exported one accessible from there. `path` is the
    /// `idText`-normalised segment list of a dotted reference (`["Shared", "foo"]`).
    ///
    /// The **latest accessible** binding in the path's export history wins: FCS
    /// resolves `M.x` to the newest `x`, but skips a `private` redeclaration
    /// inaccessible from `site` back to an earlier accessible one (fcs-dump: a public
    /// `let x` shadowed by a later `let private x` still binds the public value from
    /// outside — FS1094 only when *no* accessible binding remains). So this is
    /// [`Self::latest_accessible_value`] — the collapse model's recovery, which the
    /// complete per-path history makes possible.
    pub(super) fn lookup_qualified_path(&self, path: &[String], site: &[String]) -> Option<ItemId> {
        self.latest_accessible_value(path, site)
    }

    /// The handle of the union/enum **case** at type-qualified `path` (`["Lib",
    /// "Color", "Red"]` = container + type + case), if an earlier file exported it
    /// **and** the declaring type is accessible from the reference site `site`. The
    /// cross-file counterpart of [`Resolver::type_case_path`](super::state::Resolver::type_case_path) — resolves a
    /// `Lib.Color.Red` / `open Lib; Color.Red` reference to the case.
    ///
    /// The accessibility gate carries the case's declaring-type access-root (`None`
    /// public; `Some(k)` = the `private` container prefix length): an inaccessible
    /// `type private` case is not imported by an `open` (FCS FS0039), so it must not
    /// resolve cross-file — a wrong target the ungated index produced.
    pub(super) fn type_qualified_case(&self, path: &[String], site: &[String]) -> Option<ItemId> {
        self.type_qualified_cases
            .get(path)
            .filter(|(_, access_root_len)| accessible_from(*access_root_len, path, site))
            .map(|(id, _)| *id)
    }

    /// Whether an earlier file exports a **type** at exactly `path`, and if so
    /// whether its case set is fully indexed (see [`Self::type_paths`]).
    pub(super) fn exported_type_at(&self, path: &[String]) -> Option<bool> {
        self.type_paths.get(path).map(|(cases, _)| *cases)
    }

    /// The [`SlotClass`] of the earlier-file type exported at exactly `path`,
    /// if any — whether its name enters FCS's unqualified slot when brought
    /// into scope by an `open` (probes M20h–M20o).
    pub(super) fn exported_type_slot_class(&self, path: &[String]) -> Option<SlotClass> {
        self.type_paths.get(path).map(|(_, slot)| *slot)
    }

    /// The names of earlier-file types declared **directly** under `container`
    /// whose [`SlotClass`] is not [`SlotClass::Keeps`] — a class/struct/enum
    /// (`Evicts`) or an abbreviation/delegate/undecidable-kind (`Unknown`), the
    /// same construction-capable set [`super::lookup::type_name_is_value_slot_contestant`]'s
    /// referenced-assembly mirror tests. `Keeps` types (unions, records,
    /// interfaces) never enter FCS's unqualified constructor slot, so they are
    /// never contestants. Feeds
    /// [`Resolver::project_namespace_contestant_names`](super::state::Resolver) —
    /// codex review of §7's machinery slice: a project namespace's own
    /// constructible type can evict a same-named value from a DIFFERENT
    /// surface (an assembly module sharing the open's FQN) exactly as an
    /// assembly namespace's constructible types already do
    /// ([`crate::assembly_env::AssemblyEnv::open_namespace_fold_surfaces`]),
    /// so it must join the fold's `contestant_names`, not be left uncounted.
    pub(super) fn direct_type_contestants(&self, container: &[String]) -> Vec<String> {
        self.type_paths
            .iter()
            .filter(|(p, (_, slot))| {
                p.len() == container.len() + 1
                    && p.starts_with(container)
                    && *slot != SlotClass::Keeps
            })
            .map(|(p, _)| p.last().expect("non-empty qualified path").clone())
            .collect()
    }

    /// Whether a prefix of `names` is an exported project **value** path —
    /// i.e. the reference is member access on a project value (`Demo.Calc.x`
    /// where `Demo.Calc` is a `let`). F# binds the value first (it shadows a
    /// same-named assembly type), so the assembly index must not be consulted;
    /// declining (Deferred) avoids a wrong assembly hit on a name collision.
    ///
    /// All prefixes are checked (`1..=names.len()`), though an *exact* value
    /// path is already caught by the cross-file `Item` branch before the
    /// assembly lookup runs — only proper-prefix value matches reach here.
    pub(super) fn is_project_value_prefixed(&self, names: &[String]) -> bool {
        // A prefix is a project value only if it holds a *value* export — a
        // pattern-only active-pattern-case path (Stage 3a) is not a dottable value,
        // so it does not make `names` value-prefixed.
        (1..=names.len()).any(|k| {
            self.value_exports
                .get(&names[..k])
                .is_some_and(|h| h.iter().any(|r| !r.pattern_only))
        })
    }

    /// The handle of the exported **ordinary value** (a `let`/static value, *not* a
    /// union/exception **case constructor**) at exactly `path`, if any. Used by the
    /// type-qualified-case lookup to detect a value that shadows the qualifier (F#
    /// reads `Color.Red` as member access on a value `Color`, but a *case*
    /// constructor `Color` — `type Color = Color | Red` — is not a dottable value and
    /// does not shadow). The source-latest export at the path (the last
    /// [`ExportRecord`]) counts: a later `let` shadowing an earlier case makes the
    /// path an ordinary value (correct — the value wins), while a path whose latest
    /// export is itself a case is *not* an ordinary value (returns `None`).
    pub(super) fn ordinary_value_at(&self, path: &[String]) -> Option<ItemId> {
        self.value_exports
            .get(path)
            // A pattern-only active-pattern case (Stage 3a) is not a value-namespace
            // export, so it must be transparent here — skip it and read the latest
            // ACTUAL value-space export (else a trailing AP case masks an underlying
            // ordinary value, making a `Container.Color.Red` qualifier resolve as a
            // case where FCS binds the value).
            .and_then(|h| h.iter().rev().find(|r| !r.pattern_only))
            .filter(|r| !r.is_case)
            .map(|r| r.id)
    }

    /// Whether `names` *is* exactly a declared project module path. Such a bare
    /// reference names the module itself (which we do not model as a def, and
    /// which shadows a same-named assembly type), so it defers. A path *under* a
    /// module — a proper-prefix match — is deliberately **not** caught here:
    /// the module and the assembly namespace merge, and F# falls through to the
    /// assembly when the module does not provide the tail (see
    /// [`Self::module_headers`]).
    pub(super) fn is_exact_project_module(&self, names: &[String]) -> bool {
        self.module_headers.contains(names)
    }

    /// Whether `names` is rooted at (equal to, or a path under) a declared
    /// *nested* project module from an earlier file. Such a reference defers
    /// rather than falling through to a colliding assembly member — see
    /// [`Self::nested_module_paths`].
    pub(super) fn is_rooted_at_nested_module(&self, names: &[String]) -> bool {
        self.nested_module_paths
            .iter()
            .any(|p| names.starts_with(p.as_slice()))
    }

    /// Whether `names` is *exactly* a declared *nested* project module from an
    /// earlier file (not merely a path under one). The cross-file half of the
    /// enumerable-module predicate ([`Resolver::is_project_module_path`](super::state::Resolver::is_project_module_path)).
    pub(super) fn is_exact_nested_module(&self, names: &[String]) -> bool {
        self.nested_module_paths.contains(names)
    }

    /// Whether an earlier file binds **any resolvable entity along `path`** — a
    /// project value, module, type, or type-qualified case that FCS would resolve
    /// the reference into. The complete "does the project own this path" test the
    /// self-qualifier relaxation reads: a self reference through a module the
    /// project also supplies must NOT commit a referenced-assembly reading
    /// ([`Resolver::self_module_shadow_only`](super::state::Resolver::self_module_shadow_only)).
    ///
    /// The per-member merge (`docs`/[`Self::module_headers`]) fixes the prefix
    /// semantics: an **exact** match of any kind owns the path, and a value/type at
    /// a *proper* prefix is genuine member/static access on it — but a *module* at
    /// a proper prefix does **not** own the path (the module merges with the
    /// assembly namespace, so `List.rev` beside a project `N.List.fold2` still
    /// reaches FSharp.Core). Ungated by accessibility on purpose: this only ever
    /// *withholds* the relaxation (a conservative deferral, never a wrong commit),
    /// so an inaccessible earlier binding is safe to honour.
    pub(super) fn binds_along_path(&self, path: &[String]) -> bool {
        let value_at = |p: &[String]| {
            self.value_exports
                .get(p)
                .is_some_and(|h| h.iter().any(|r| !r.pattern_only))
        };
        let exact = value_at(path)
            || self.module_headers.contains(path)
            || self.nested_module_paths.contains(path)
            || self.type_paths.contains_key(path)
            || self.type_qualified_cases.contains_key(path);
        exact
            || (1..path.len())
                .any(|k| value_at(&path[..k]) || self.type_paths.contains_key(&path[..k]))
    }

    /// Whether an earlier file defines a **real** nested `module X = …` at exactly
    /// `names` (see [`Self::real_nested_modules`] — unlike
    /// [`Self::is_exact_nested_module`]'s shadow set, a type/exception/alias/
    /// `extern` name does not answer).
    pub(super) fn is_real_nested_module(&self, names: &[String]) -> bool {
        self.real_nested_modules.contains(names)
    }

    /// The latest export at `path` the reference site `site` can **access**,
    /// selected from the path's export history by `keep` (which restricts to the
    /// value namespace — everything — or the constructor namespace — cases only).
    ///
    /// Walks the history newest-first and returns the first record that both
    /// satisfies `keep` and is *accessible* from `site`: a public export
    /// (`access_root_len` `None`) always is; a restricted one only when `site`
    /// lies within its access-root — the `k`-segment prefix of `path`, its
    /// declaring container or a `private` ancestor's container (F#'s inherited
    /// `private` rule, oracle-pinned; see [`ExportedItem::access_root_len`]).
    /// This is the one query the collapse model exposes; because the history is
    /// complete, a public export under a later inaccessible `private`
    /// redeclaration is still found.
    fn latest_accessible(
        &self,
        path: &[String],
        site: &[String],
        keep: impl Fn(&ExportRecord) -> bool,
    ) -> Option<ItemId> {
        let history = self.value_exports.get(path)?;
        history
            .iter()
            .rev()
            .find(|r| keep(r) && accessible_from(r.access_root_len, path, site))
            .map(|r| r.id)
    }

    /// The latest export at `path` accessible from `site` that was declared in
    /// Compile-order `file` and satisfies `keep` — the per-**fragment** analogue
    /// of [`Self::latest_accessible`] (Stage 5). Unlike the file-blind query, a
    /// later plain augmentation in another file neither shadows nor supplies a
    /// fragment's own member: the auto-open fold folds each `[<AutoOpen>]`
    /// fragment's members at *its* file, so it must read the export declared
    /// *there*, not the collapsed latest.
    fn latest_accessible_in_file(
        &self,
        path: &[String],
        site: &[String],
        file: usize,
        keep: impl Fn(&ExportRecord) -> bool,
    ) -> Option<ItemId> {
        let history = self.value_exports.get(path)?;
        history
            .iter()
            .rev()
            .find(|r| {
                keep(r)
                    && self.file_of(r.id) == file
                    && accessible_from(r.access_root_len, path, site)
            })
            .map(|r| r.id)
    }

    /// The `(name, id)` of every **value** exported directly under `module_path`
    /// in Compile-order `file`, accessible from `site` — the members a single
    /// `[<AutoOpen>]` fragment at `file` contributes to the fold (Stage 5). Like
    /// [`Self::direct_value_children`] but pinned to one file, so a plain
    /// augmentation elsewhere neither adds a spurious member nor hides this
    /// fragment's own.
    pub(super) fn fragment_value_children(
        &self,
        module_path: &[String],
        file: usize,
        site: &[String],
    ) -> Vec<(String, ItemId)> {
        self.value_exports
            .keys()
            .filter(|q| q.len() == module_path.len() + 1 && q.starts_with(module_path))
            .filter_map(|q| {
                // Value-namespace: exclude pattern-only active-pattern cases (Stage
                // 3a) — a value-live union/exception case still counts.
                self.latest_accessible_in_file(q, site, file, |r| !r.pattern_only)
                    .map(|id| (q.last().expect("non-empty qualified path").clone(), id))
            })
            .collect()
    }

    /// The `(name, id)` of every **constructor case** exported directly under
    /// `module_path` in Compile-order `file`, accessible from `site` — the
    /// constructor-namespace twin of [`Self::fragment_value_children`].
    pub(super) fn fragment_constructor_children(
        &self,
        module_path: &[String],
        file: usize,
        site: &[String],
    ) -> Vec<(String, ItemId)> {
        self.value_exports
            .keys()
            .filter(|q| q.len() == module_path.len() + 1 && q.starts_with(module_path))
            .filter_map(|q| {
                self.latest_accessible_in_file(q, site, file, |r| r.is_case)
                    .map(|id| (q.last().expect("non-empty qualified path").clone(), id))
            })
            .collect()
    }

    /// The latest export at `path` accessible from `site`, counting **values and
    /// cases** (a case is a value in expression position). The value-namespace
    /// query — feeds [`Self::direct_value_children`] and, once built, the
    /// straddle's value slot.
    pub(super) fn latest_accessible_value(
        &self,
        path: &[String],
        site: &[String],
    ) -> Option<ItemId> {
        // A value or a value-live union/exception case — never a pattern-only
        // active-pattern case (FS0039 in expression position; Stage 3a).
        self.latest_accessible(path, site, |r| !r.pattern_only)
    }

    /// The latest **case** export at `path` accessible from `site` — restricted
    /// to constructor cases, so a case shadowed at the same path by a later
    /// ordinary `let` value is still found (the constructor namespace, live in
    /// pattern position). Replaces the old case-only `constructors` map.
    pub(super) fn latest_accessible_case(
        &self,
        path: &[String],
        site: &[String],
    ) -> Option<ItemId> {
        self.latest_accessible(path, site, |r| r.is_case)
    }

    /// The `(name, id)` of every earlier-file **value** exported *directly* under
    /// `module_path` that is accessible from `site` — its qualified path is exactly
    /// `[module_path…, name]`, one segment beyond it. The values an `open
    /// <module_path>` brings into unqualified scope from preceding Compile-order
    /// files (substep 3); a value nested deeper (in a submodule) has a longer path
    /// and is excluded. Each name resolves to its [`Self::latest_accessible_value`],
    /// so a name whose only exports are inaccessible `private` ones is omitted (it
    /// stays invisible to the `open`), and a public export shadowed by a later
    /// inaccessible `private` is recovered.
    pub(super) fn direct_value_children(
        &self,
        module_path: &[String],
        site: &[String],
    ) -> Vec<(String, ItemId)> {
        self.value_exports
            .keys()
            .filter(|q| q.len() == module_path.len() + 1 && q.starts_with(module_path))
            .filter_map(|q| {
                self.latest_accessible_value(q, site)
                    .map(|id| (q.last().expect("non-empty qualified path").clone(), id))
            })
            .collect()
    }

    /// Whether an assembly reading of the qualified path `names` is withheld
    /// by a **signature screen** (see [`Self::sig_screens`]): some signatured
    /// module root is a proper prefix of `names` and a segment past the root
    /// appears in that signature's name set — so the signature *may* expose
    /// the path, FCS would bind the `.fsi`, and Stage 1 (which commits no
    /// signature identity) must defer rather than commit the merged assembly
    /// member. A residual whose every segment is absent from the signature
    /// text provably cannot be signature-exposed and falls through.
    pub(super) fn sig_screened_path(&self, names: &[String]) -> bool {
        self.sig_screens.iter().any(|screen| {
            screen.roots.iter().any(|root| {
                names.len() > root.path.len()
                    && names.starts_with(&root.path)
                    && names[root.path.len()..]
                        .iter()
                        .any(|seg| screen.names.contains(seg))
            })
        })
    }

    /// The open-fold counterpart of [`Self::sig_screened_path`]: whether the
    /// bare name `name`, folded into scope by opening the assembly surface at
    /// `opened`, is screened. Two reaches:
    /// - `opened` at or under a signatured root: the entry's qualified path is
    ///   `opened + [name]`, so the ordinary path screen applies;
    /// - a signatured root strictly *under* `opened` (the surface of an
    ///   `open <namespace>` can fold an `[<AutoOpen>]` submodule's members,
    ///   whose provenance the entry no longer carries): screen on the name
    ///   alone — coarser, deferral-only.
    pub(super) fn sig_screened_open_name(&self, opened: &[String], name: &str) -> bool {
        self.sig_screens.iter().any(|screen| {
            screen.roots.iter().any(|root| {
                if opened.starts_with(&root.path) {
                    opened[root.path.len()..]
                        .iter()
                        .any(|seg| screen.names.contains(seg))
                        || screen.names.contains(name)
                } else {
                    root.path.len() > opened.len()
                        && root.path.starts_with(opened)
                        && screen.names.contains(name)
                }
            })
        })
    }

    /// Fold one resolved file's exports into the accumulator: index its
    /// module-qualified value paths (for cross-file lookup *and* the project
    /// value-shadow check) and record its declared module headers (from the
    /// header, so a value-less module counts). The namespace segments above a
    /// module are *not* recorded; they merge with assemblies. Also advances the
    /// item count. Called by [`resolve_project`](super::resolve_project) after each file.
    ///
    /// **Lockstep invariant:** the set of [`ResolvedFile`] fields read here *is*
    /// a file's contribution to the Compile-order threaded state.
    /// [`ResolvedFile::same_export_contribution`] compares exactly this set to
    /// decide whether a recomputed file leaves the accumulator unchanged (so the
    /// incremental fold may reuse the suffix). A field added here that is *not*
    /// added there would let the incremental fold reuse a stale suffix; the
    /// `incremental ≡ batch` differential (`resolve_incremental_diff.rs`) is the
    /// machine check on the coupling.
    ///
    /// Every index is derived from the file's source-ordered
    /// [`ExportDecl`](super::model::ExportDecl) list
    /// ([`FileExportIndices::from_decls`]); the derivation reproduces the legacy
    /// per-feature export fields exactly (`docs/export-decl-model-plan.md` Stage 2).
    ///
    /// `paired_screen` is the screen of the signature this file is the paired
    /// implementation of, if any (`docs/fsi-signature-restriction-plan.md`
    /// Stage 1): the derivation then **drops the file's value/case identity
    /// exports** — the signature restricts them, and Stage 1 emits no
    /// signature identity to replace them — while keeping every defer-only
    /// shadow and marker ([`FileExportIndices::from_decls_screened`]).
    pub(super) fn extend_with(&mut self, file: &ResolvedFile, paired_screen: Option<&SigScreen>) {
        // Record this file's id base BEFORE interning its items, so [`Self::file_of`]
        // maps any exported id back to its Compile-order file (the straddle fold's
        // per-name provenance).
        self.item_file_bases.push(self.count);
        // This file's Compile-order index — the fragment provenance the auto-open
        // fold reads (the base was just pushed, so `len - 1` is this file).
        let file_idx = self.item_file_bases.len() - 1;

        // A signature file's own contribution is its screen alone (Stage 1:
        // it exports nothing; its decl list is empty, so the derivation below
        // folds nothing else from it).
        if let Some(screen) = &file.sig_screen {
            self.sig_screens.push(Arc::clone(screen));
        }

        let idx = match paired_screen {
            None => FileExportIndices::from_decls(file),
            Some(screen) => FileExportIndices::from_decls_screened(file, screen),
        };
        for module in idx.module_headers {
            self.module_headers.insert(module);
        }
        for nested in idx.nested_module_paths {
            self.nested_module_paths.insert(nested);
        }
        for nested in idx.real_nested_modules {
            self.real_nested_modules.insert(nested);
        }
        for (path, entry) in idx.type_qualified_cases {
            self.type_qualified_cases.insert(path, entry);
        }
        for (path, info) in idx.type_paths {
            self.type_paths.insert(path, info);
        }
        // The attribute resolution's cross-file project-type guard
        // ([`Resolver::project_type_named`](super::state::Resolver)): a bare
        // attribute name a preceding file's type could satisfy must defer,
        // because bare cross-file project types are not otherwise indexed for
        // the tiered walk. Fed from the file's *syntactic* whole-file
        // pre-scan, not its qualified exports — a headerless (anonymous
        // module) file exports no type paths, yet F# still exposes its types
        // to later files through the implicit filename module (codex on this
        // stage).
        self.project_type_simple_names
            .extend(file.own_type_simple_names.iter().cloned());
        for ns in idx.namespace_paths {
            self.namespace_paths.insert(ns);
        }
        for hidden in idx.modules_with_hidden_values {
            self.modules_with_hidden_values.insert(hidden);
        }
        for auto_open in idx.auto_open_module_paths {
            self.auto_open_module_paths.push((auto_open, file_idx));
        }
        for (path, record) in idx.value_exports {
            // Append to the path's export history in Compile order — every export
            // is kept, so a query can pick the latest *accessible* one (the value /
            // constructor namespaces read this same history through different
            // predicates; no export is lost to a shadow).
            self.value_exports.entry(path).or_default().push(record);
        }
        for id in idx.case_item_ids {
            self.case_item_ids.insert(id);
        }
        for id in idx.attributed_value_ids {
            self.attributed_value_ids.insert(id);
        }
        for (id, shape) in idx.active_pattern_shapes {
            self.active_pattern_shapes.insert(id, shape);
        }
        self.count +=
            u32::try_from(file.exports.items.len()).expect("more than u32::MAX items in one file");
    }

    /// Whether `id` is an earlier file's exported **constructor case** (see
    /// [`Self::case_item_ids`]).
    pub(super) fn is_case_item(&self, id: ItemId) -> bool {
        self.case_item_ids.contains(&id)
    }

    /// Whether `id` is an earlier file's **attributed** module-level value
    /// (see [`Self::attributed_value_ids`]) — a maybe-literal, which contests
    /// the pattern namespace as a constant pattern.
    pub(super) fn is_attributed_item(&self, id: ItemId) -> bool {
        self.attributed_value_ids.contains(&id)
    }

    /// Whether `path` (a module child, container + name) exports an
    /// **attributed value accessible from `site`** — a maybe-literal constant
    /// pattern. FCS folds an opened module's vals *after* its tycons
    /// (`AddModuleOrNamespaceContentsToNameEnv`: exceptions → tycons → vals),
    /// so such a value beats the module's own same-named case in bare pattern
    /// position **regardless of their source order** — the case must defer
    /// ([`Resolver::pattern_suppressed_case_ids`](super::state::Resolver)).
    /// An inaccessible value is filtered exactly as FCS filters it from the
    /// opened environment, leaving the case committed.
    pub(super) fn module_value_may_be_constant_pattern(
        &self,
        path: &[String],
        site: &[String],
    ) -> bool {
        self.value_exports.get(path).is_some_and(|history| {
            history.iter().any(|rec| {
                self.attributed_value_ids.contains(&rec.id)
                    && accessible_from(rec.access_root_len, path, site)
            })
        })
    }

    /// The `(name, id)` of every earlier-file **constructor case** exported
    /// directly under `module_path` that is accessible from `site` — the
    /// constructor pass of [`Resolver::open_module_values`](super::state::Resolver::open_module_values).
    /// Mirrors [`Self::direct_value_children`], but each name resolves to its
    /// [`Self::latest_accessible_case`] so a case shadowed at its path by a later
    /// ordinary `let` value is still enumerated (the constructor namespace, live
    /// in pattern position).
    pub(super) fn direct_constructor_children(
        &self,
        module_path: &[String],
        site: &[String],
    ) -> Vec<(String, ItemId)> {
        self.value_exports
            .keys()
            .filter(|q| q.len() == module_path.len() + 1 && q.starts_with(module_path))
            .filter_map(|q| {
                self.latest_accessible_case(q, site)
                    .map(|id| (q.last().expect("non-empty qualified path").clone(), id))
            })
            .collect()
    }

    /// The recognizer [`ActivePatternShape`] of the earlier-file **active-pattern
    /// case** with handle `id`, if any (Stage 3a). A use whose applied head resolved
    /// to a cross-file AP case (`Resolution::Item`) looks the shape up here to split
    /// its arguments; a non-AP `Item` (an ordinary value, a union/exception case) is
    /// absent. See [`Self::active_pattern_shapes`].
    pub(super) fn active_pattern_shape_of(&self, id: ItemId) -> Option<ActivePatternShape> {
        self.active_pattern_shapes.get(&id).copied()
    }

    /// Whether `path` is a declared earlier-file project **namespace** (see
    /// [`Self::namespace_paths`]).
    pub(super) fn is_namespace(&self, path: &[String]) -> bool {
        self.namespace_paths.contains(path)
    }

    /// Whether an earlier file exports a direct `[<AutoOpen>]` module under
    /// `namespace`.
    pub(super) fn has_auto_open_module_in_namespace(&self, namespace: &[String]) -> bool {
        !self.auto_open_modules_directly_in(namespace).is_empty()
    }

    /// The qualified paths of earlier-file **non-`private`** `[<AutoOpen>]`
    /// modules directly under `container` (see [`is_directly_in`]) — the
    /// cross-file half of [`Resolver::project_auto_open_submodules_in`]
    /// (`resolve/lookup.rs`), which also collects the same-file half and
    /// recurses to fold a project namespace's auto-open descendants like the
    /// assembly namespace half's `[<AutoOpen>]` recursion
    /// (`AssemblyEnv::open_namespace_fold_surfaces`).
    pub(super) fn auto_open_modules_directly_in(&self, container: &[String]) -> Vec<Vec<String>> {
        self.auto_open_module_paths
            .iter()
            .filter(|(p, _)| is_directly_in(p, container))
            .map(|(p, _)| p.clone())
            .collect()
    }

    /// The earlier-file non-`private` `[<AutoOpen>]` **fragments** directly in
    /// `container`, as `(path, file)` pairs (Stage 5). Unlike
    /// [`Self::auto_open_modules_directly_in`] (paths only), it keeps the
    /// declaring file — a module may appear more than once, once per fragment —
    /// which the file-ordered fold and its same-file parent-nesting rule need.
    pub(super) fn auto_open_fragments_directly_in(
        &self,
        container: &[String],
    ) -> Vec<(Vec<String>, usize)> {
        self.auto_open_module_paths
            .iter()
            .filter(|(p, _)| is_directly_in(p, container))
            .cloned()
            .collect()
    }
}

/// A `type_qualified_cases` entry's payload: the case's handle and its
/// accessibility (`access_root_len`, #1000), keyed by the type-qualified path.
type QualifiedCaseExport = (ItemId, Option<usize>);

/// The per-file cross-file index contributions [`ProjectItems::extend_with`]
/// folds, derived from one file's [`ExportDecl`] list ([`Self::from_decls`]).
/// Each field mirrors one cross-file index; the derivation reproduces its
/// ordering and conservatism exactly (`docs/export-decl-model-plan.md` Stage 2's
/// derivation table).
///
/// The order-sensitive fields (`value_exports` history, the latest-wins
/// `type_qualified_cases` / `type_paths` insertion orders, the Compile-order
/// `auto_open_module_paths`) preserve decl order; the rest fold into a
/// `HashSet`/`HashMap`, so their order is irrelevant.
///
/// `PartialEq` is the contribution currency of
/// [`ResolvedFile::same_export_contribution`]: comparing the *derived indices*
/// (rather than re-listing the source fields that feed them) makes that
/// comparison drift-proof — a new fold input is included automatically, and
/// provenance the fold never reads (a decl's `pos`) is excluded automatically,
/// because neither can enter the comparison except through [`Self::from_decls`].
/// The order-insensitive fields are compared as ordered `Vec`s, which is
/// conservative (a set-preserving reorder compares unequal) but sound, and for
/// the incremental fold's use — one file recomputed against its own prior
/// resolution — decl order is the file's own source order, unchanged by a
/// body-only edit.
#[derive(Default, PartialEq, Eq)]
struct FileExportIndices {
    value_exports: Vec<(Vec<String>, ExportRecord)>,
    case_item_ids: Vec<ItemId>,
    attributed_value_ids: Vec<ItemId>,
    active_pattern_shapes: Vec<(ItemId, ActivePatternShape)>,
    module_headers: Vec<Vec<String>>,
    nested_module_paths: Vec<Vec<String>>,
    real_nested_modules: Vec<Vec<String>>,
    namespace_paths: Vec<Vec<String>>,
    modules_with_hidden_values: Vec<Vec<String>>,
    type_qualified_cases: Vec<(Vec<String>, QualifiedCaseExport)>,
    type_paths: Vec<(Vec<String>, (bool, SlotClass))>,
    auto_open_module_paths: Vec<Vec<String>>,
}

/// Record `path`'s **container** (`path` minus its last segment) as a
/// hidden-value module — the derivation of the `note_hidden_value_module`
/// markers whose legacy path is `self.container_path`.
fn push_container_hidden(fi: &mut FileExportIndices, path: &[String]) {
    if let Some((_, container)) = path.split_last() {
        fi.modules_with_hidden_values.push(container.to_vec());
    }
}

impl FileExportIndices {
    /// Derive the file's cross-file index contributions from its source-ordered
    /// [`ExportDecl`] list.
    fn from_decls(file: &ResolvedFile) -> Self {
        Self::derive(file, None)
    }

    /// Like [`Self::from_decls`], but for the **paired implementation of a
    /// signature file** (`docs/fsi-signature-restriction-plan.md` Stage 1).
    /// The signature restricts the file's cross-file surface, and Stage 1
    /// emits no signature identity to replace it, so the derivation:
    ///
    /// - **drops every value/case identity export** (`Item`,
    ///   `ActivePatternCase`) — a hidden member then falls through to a
    ///   merged referenced-assembly member exactly as FCS does (probe:
    ///   `Shared.bar` → the assembly), while a possibly-exposed one is
    ///   deferred by the screen ([`ProjectItems::sig_screened_path`]); each
    ///   dropped export marks its container hidden, so opens stay
    ///   conservative;
    /// - **keeps every defer-only shadow** (module headers, nested-module /
    ///   type / abbreviation / extern paths, hidden-value markers) — those
    ///   only ever withhold a commit;
    /// - **demotes type payloads to unprovable** (`(false,
    ///   SlotClass::Unknown)`): a signature can hide a representation
    ///   (opacity), so neither a case-completeness proof nor a slot-class
    ///   eviction verdict derived from the implementation may survive;
    /// - **honours the signature's `[<AutoOpen>]`** (conclusion 6): a header
    ///   root the signature attributes stays auto-open even when the
    ///   implementation header is bare, and signature-declared auto-open
    ///   nested modules join `auto_open_module_paths` (marked hidden, so the
    ///   fold's generation barrier fires for them).
    fn from_decls_screened(file: &ResolvedFile, screen: &SigScreen) -> Self {
        Self::derive(file, Some(screen))
    }

    fn derive(file: &ResolvedFile, screen: Option<&SigScreen>) -> Self {
        let mut fi = Self::default();
        for decl in &file.export_decls {
            let anon = decl.anonymous_root;
            match &decl.kind {
                ExportDeclKind::Item {
                    item,
                    type_qualified,
                } => match item {
                    Some(_) if screen.is_some() => {
                        // Signature-restricted (Stage 1): the value/case
                        // identity is dropped, and the container is marked
                        // hidden — it holds names the boundary no longer
                        // enumerates.
                        push_container_hidden(&mut fi, &decl.path);
                    }
                    Some(item_idx) => {
                        let it = &file.exports.items[*item_idx];
                        if let Some(path) = it.qualified_path() {
                            fi.value_exports.push((
                                path.to_vec(),
                                ExportRecord {
                                    id: it.id,
                                    access_root_len: it.access_root_len,
                                    is_case: it.is_case(),
                                    // A value-namespace item (`let` value or a
                                    // value-live union/exception case) is never
                                    // pattern-only — that is the AP-case branch below.
                                    pattern_only: false,
                                },
                            ));
                        }
                        if it.is_case() {
                            fi.case_item_ids.push(it.id);
                        }
                        if it.attributed {
                            fi.attributed_value_ids.push(it.id);
                        }
                        if let Some(tq) = type_qualified {
                            // The type-qualified case index is gated on the case's
                            // accessibility (#1000); its `access_root_len` equals the
                            // item's own (both are `export_access_root_len(type_is_private)`
                            // at every producer), so read it from the referenced item.
                            fi.type_qualified_cases
                                .push((tq.clone(), (it.id, it.access_root_len)));
                        }
                    }
                    None => {
                        // Anonymous-root non-RQA union / exception case: no
                        // `ExportedItem`, only the container's hidden-value marker
                        // (plan pitfall 1). Such a decl is produced solely by
                        // `export_case` under an anonymous root.
                        debug_assert!(
                            anon,
                            "an Item decl with no ExportedItem must be anonymous-root"
                        );
                        push_container_hidden(&mut fi, &decl.path);
                    }
                },
                ExportDeclKind::Type { info, auto_open } => {
                    if !anon {
                        fi.nested_module_paths.push(decl.path.clone());
                        if let Some((cases_enumerable, slot)) = info {
                            // Signature-restricted: the signature may declare
                            // the type opaquely (or not at all), so neither
                            // the implementation's case-completeness proof
                            // nor its slot class may survive — both could
                            // turn a defer into a wrong commit. The path
                            // itself stays (a defer-only shadow).
                            let payload = if screen.is_some() {
                                (false, SlotClass::Unknown)
                            } else {
                                (*cases_enumerable, *slot)
                            };
                            fi.type_paths.push((decl.path.clone(), payload));
                        }
                    }
                    if *auto_open {
                        // A `[<AutoOpen>]` type marks its container hidden (fires
                        // under an anonymous root too — the legacy call is unguarded).
                        push_container_hidden(&mut fi, &decl.path);
                    }
                }
                ExportDeclKind::Module {
                    header,
                    auto_open,
                    private,
                } => {
                    if !anon {
                        if *header {
                            fi.module_headers.push(decl.path.clone());
                        } else {
                            fi.real_nested_modules.push(decl.path.clone());
                            fi.nested_module_paths.push(decl.path.clone());
                        }
                        // The signature's `[<AutoOpen>]` on a constrained
                        // root is authoritative (conclusion 6): the module is
                        // auto-open even when the implementation header
                        // carries no attribute.
                        let sig_auto_open = screen.is_some_and(|s| {
                            s.roots.iter().any(|r| r.auto_open && r.path == decl.path)
                        });
                        if (*auto_open || sig_auto_open) && !*private {
                            fi.auto_open_module_paths.push(decl.path.clone());
                        }
                    }
                }
                ExportDeclKind::ModuleAbbrev => {
                    if !anon {
                        fi.nested_module_paths.push(decl.path.clone());
                        fi.modules_with_hidden_values.push(decl.path.clone());
                    }
                }
                ExportDeclKind::ExceptionTycon => {
                    if !anon {
                        fi.nested_module_paths.push(decl.path.clone());
                    }
                }
                ExportDeclKind::Extern { name } => {
                    if !anon && !name.is_empty() {
                        let mut shadow = decl.path.clone();
                        shadow.extend(name.iter().cloned());
                        fi.nested_module_paths.push(shadow);
                    }
                    // The hidden marker is recorded unconditionally (the legacy
                    // `note_hidden_value_module` call is unguarded), on the
                    // container — which is `decl.path` itself for an `Extern`.
                    fi.modules_with_hidden_values.push(decl.path.clone());
                }
                ExportDeclKind::Namespace => {
                    fi.namespace_paths.push(decl.path.clone());
                }
                ExportDeclKind::ActivePatternCase { item, shape } => match item {
                    Some(_) if screen.is_some() => {
                        // Signature-restricted (Stage 1): as for `Item` — the
                        // case identity is dropped, the container marked
                        // hidden.
                        push_container_hidden(&mut fi, &decl.path);
                    }
                    Some(item_idx) => {
                        // Stage 3a: the AP case rides `value_exports` as a
                        // **pattern-only** case record (`is_case = true`,
                        // `pattern_only = true`), so it inherits the constructor
                        // namespace's Compile-order provenance and accessibility
                        // recovery exactly as a union case does — the value-namespace
                        // queries exclude it via `pattern_only`, so a bare
                        // expression-position use stays FS0039. It also enters
                        // `case_item_ids` (a cross-file `Item` classifies as a case)
                        // and the `active_pattern_shapes` side map (the recognizer
                        // shape for the use-site split). Its container is **not**
                        // marked hidden — the case is now enumerable, the narrowed AP
                        // hidden trigger; a container hidden for another reason keeps
                        // that reason's own marker.
                        let it = &file.exports.items[*item_idx];
                        fi.value_exports.push((
                            decl.path.clone(),
                            ExportRecord {
                                id: it.id,
                                access_root_len: it.access_root_len,
                                is_case: true,
                                pattern_only: true,
                            },
                        ));
                        fi.case_item_ids.push(it.id);
                        fi.active_pattern_shapes.push((it.id, *shape));
                    }
                    None => {
                        // Anonymous root: no cross-file handle, so keep today's
                        // conservative hidden-value marker for the container.
                        push_container_hidden(&mut fi, &decl.path);
                    }
                },
            }
        }
        if let Some(screen) = screen {
            // Every constrained root is hidden-valued: the signature may
            // expose members Stage 1 does not enumerate, so opens of the
            // root must shadow conservatively (the generation bump) even
            // when the implementation's own decls produced no marker.
            for root in &screen.roots {
                fi.modules_with_hidden_values.push(root.path.clone());
            }
            // Signature-declared `[<AutoOpen>]` nested modules (conclusion 6)
            // — auto-open even when the implementation carries no attribute,
            // and hidden so the namespace fold's barrier fires for them.
            for path in &screen.auto_open_nested {
                fi.auto_open_module_paths.push(path.clone());
                fi.modules_with_hidden_values.push(path.clone());
            }
        }
        fi
    }
}

/// Whether `path` is a **direct child** of `namespace` (exactly one segment
/// deeper). The one definition of "an auto-open module directly in this
/// namespace" — the exact-namespace-only rule (never ancestors: F# `open N`
/// imports only `N`'s direct members) that both the same-file walk and the
/// cross-file [`ProjectItems`] check share, so they cannot drift apart.
pub(super) fn is_directly_in(path: &[String], namespace: &[String]) -> bool {
    path.len() == namespace.len() + 1 && path.starts_with(namespace)
}

/// Whether a reference at `site` (its enclosing container path) can **access**
/// an export at `path` with access-root `access_root_len` (see
/// [`ExportedItem::access_root_len`]): a public export (`None`) always; a
/// restricted one (`Some(k)`) only when `site` lies within the export's
/// `k`-segment access-root prefix — F#'s `private` visibility, inherited from
/// the declaring value/type/module and oracle-pinned. `k` is always a valid
/// prefix length of `path` (the access-root is a proper prefix — a container).
pub(super) fn accessible_from(
    access_root_len: Option<usize>,
    path: &[String],
    site: &[String],
) -> bool {
    match access_root_len {
        None => true,
        Some(k) => site.starts_with(&path[..k]),
    }
}

/// A **project-global** handle for a top-level binding (a value/function a
/// module exports). A [`Resolution::Item`] points here, whether the binding is
/// in this file or an earlier Compile-order file.
///
/// Ids are assigned in Compile order: each file's items occupy a contiguous
/// range starting at the running [`ProjectItems`] count, so a handle is unique
/// across the whole project. Reach the underlying [`Def`] through
/// [`ResolvedProject::item_def`] (cross-file aware) or, for a file's own items,
/// [`ResolvedFile::resolved_def`] — never by indexing, so the contiguous-range
/// scheme stays an implementation detail. A single-file caller numbers from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemId(u32);

impl ItemId {
    pub(super) fn new(index: usize) -> Self {
        ItemId(u32::try_from(index).expect("more than u32::MAX items in one project"))
    }

    pub(super) fn index(self) -> usize {
        self.0 as usize
    }
}

/// Whether a type definition's name enters FCS's unqualified-name slot
/// (`eUnqualifiedItems`). `AddPartsOfTyconRefToNameEnv` adds a tycon there
/// only when it "may have construction": class types, struct types (enums and
/// `[<Struct>]` unions/records included — `isStructTy`), and feature-gated
/// delegates. Unions, records, and interfaces never enter. Decides whether a
/// later type EVICTS a same-named definite value from the slot — probes
/// M20a–M20o, `docs/project-type-member-plan.md` §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlotClass {
    /// Provably enters the slot: an enum, a `[<Struct>]`-attributed
    /// union/record (probe M20m), or an object model that is class- or
    /// struct-kinded (explicitly, or via a primary constructor).
    Evicts,
    /// Provably never enters: a plain union (M20k), record (M20l), or an
    /// explicit interface (M20o).
    Keeps,
    /// Statically undecidable: an abbreviation (FCS chases its target —
    /// `type C = int` evicts, a union target keeps; probe M20n), a delegate
    /// (langversion-gated), an unspecified-kind object model (F# infers
    /// class vs interface from the members), or a bodyless/inline-IL repr.
    /// A contest against one defers.
    Unknown,
}

/// One declaration a file contributes to the cross-file boundary, in source
/// order (`docs/export-decl-model-plan.md` Stage 2). A single per-file
/// `Vec<ExportDecl>` is the currency [`ProjectItems::extend_with`] folds; every
/// cross-file index derives from it, replacing the earlier per-feature parallel
/// export fields.
///
/// **`path` is per-kind** (see [`ExportDeclKind`] for what it means for each):
/// for most kinds it is the declaration's own qualified path (its container
/// segments followed by its own name); for [`ExportDeclKind::Extern`] it is the
/// *container* (the hidden path), with the function name carried in the kind;
/// and for [`ExportDeclKind::Namespace`] it is the namespace prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExportDecl {
    /// Qualified path — per-kind meaning (see the type docs and [`ExportDeclKind`]).
    pub(super) path: Vec<String>,
    /// Source position of the declaring occurrence (start). Provenance for
    /// positional contests and the future same-file convergence
    /// (`docs/export-decl-model-plan.md` Stage 4). No Stage-2 derivation reads it.
    pub(super) pos: TextSize,
    /// Whether the declaration sits under an anonymous top-level module
    /// ([`Resolver::anonymous_root`](super::state::Resolver)). Such declarations are
    /// not cross-file-addressable (the export writers are guarded `!anonymous_root`),
    /// but some facts still cross the boundary — a hidden-value marker for an
    /// anonymous-root union/exception case. Recording every decl with the flag
    /// keeps the derivations faithful without losing that information (plan pitfall 1).
    pub(super) anonymous_root: bool,
    pub(super) kind: ExportDeclKind,
}

/// The typed payload of an [`ExportDecl`]. Each variant corresponds to a
/// declaration nature and drives a documented set of derivations in
/// [`ProjectItems::extend_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExportDeclKind {
    /// A value-namespace item — a `let` value, a non-RQA union case, or an
    /// `exception` constructor. `path` = container + name.
    ///
    /// `item` indexes [`ResolvedFile::exports`]`.items` — the stored
    /// [`ExportedItem`] this decl mirrors, from which `value_exports` /
    /// `case_item_ids` derive. It is `None` for an **anonymous-root** union/enum
    /// exception case, where [`export_case`](super::Resolver::export_case) creates no
    /// `ExportedItem`; such a decl exists only to carry the hidden-value marker for
    /// its container (plan pitfall 1).
    Item {
        item: Option<usize>,
        /// The case's *type-qualified* export path (`[container.., Type, Case]`),
        /// threaded by [`export_type_qualified_case`](super::Resolver::export_type_qualified_case)
        /// for a non-RQA union case, an RQA union case, or an enum case; `None` for
        /// an ordinary value or an `exception` constructor. Feeds `type_qualified_cases`.
        type_qualified: Option<Vec<String>>,
    },
    /// A `type` name. `path` = container + name segment(s).
    ///
    /// `info` is `Some((cases_enumerable, slot))` for a genuine (single-ident,
    /// non-augmentation) type definition — which feeds `type_paths`, keyed by
    /// `path`; it is `None` for an augmentation (`type A.B with …`) or a dotted
    /// abbreviation head, which records only the conflated nested-module shadow.
    /// `auto_open` marks a `[<AutoOpen>]` type, whose container is hidden.
    Type {
        info: Option<(bool, SlotClass)>,
        auto_open: bool,
    },
    /// A module. `path` = the module's full path. `header` distinguishes a
    /// top-level `module`/`namespace`-rooted header (feeds `module_headers`) from a
    /// nested `module X = …` (feeds `real_nested_modules`). `auto_open` / `private`
    /// carry the `[<AutoOpen>]` and `module private` bits so
    /// `auto_open_module_paths` derives (non-private auto-open modules only).
    Module {
        header: bool,
        auto_open: bool,
        private: bool,
    },
    /// A module abbreviation `module P = Target`. `path` = container + P — both
    /// its nested-module shadow path and its hidden-value path.
    ModuleAbbrev,
    /// An `exception E` constructor's tycon-side presence. `path` = container + E.
    /// The value-namespace constructor is a separate [`Self::Item`] record.
    ExceptionTycon,
    /// An `extern` declaration. `path` = the *container* (its hidden-value path,
    /// recorded unconditionally). `name` = the function-name segments (empty for a
    /// nameless recovery node); `path` + `name` is the nested-module shadow path,
    /// recorded only when `name` is non-empty (matching the legacy
    /// [`record_project_name_shadow`](super::Resolver::record_project_name_shadow) guard).
    Extern { name: Vec<String> },
    /// A `namespace` header ancestor prefix. `path` = the prefix.
    Namespace,
    /// A module-level active-pattern case (Stage 3a). `path` = container + case
    /// name. AP cases are **pattern-namespace-only** — a bare use in expression
    /// position is FS0039 — so, unlike a value-namespace case, they never enter
    /// [`value_exports`](ProjectItems::value_exports); they enter the dedicated
    /// [`active_pattern_case_exports`](ProjectItems::active_pattern_case_exports)
    /// index (carrying the recognizer `shape`, so a cross-file *parameterized* use
    /// splits its arguments exactly as a same-file one does) and
    /// [`case_item_ids`](ProjectItems::case_item_ids).
    ///
    /// `item` indexes [`ResolvedFile::exports`]`.items` — the AP case's own
    /// [`ExportedItem`] (with `qualified: None`, so value-namespace queries never
    /// see it; its `def` is the per-case recognizer-span use, so a cross-file
    /// `Resolution::Item` points go-to-def at the recognizer). It is `None` under
    /// an **anonymous root** (no cross-file handle), where the decl falls back to
    /// today's hidden-value marker for its container.
    ActivePatternCase {
        item: Option<usize>,
        shape: ActivePatternShape,
    },
}

/// Why a use was left unresolved-but-not-an-error. Every variant is honest
/// "say nothing" territory (D5): the LSP shows no go-to-definition and no
/// diagnostic. More reasons (member access needing inference, etc.) arrive
/// with their producers in later phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeferredReason {
    /// A single-segment name not bound by any in-file scope. It may resolve to
    /// an opened namespace or a referenced assembly once those environments
    /// exist (Phase 2); until then we decline to guess.
    UnboundName,
    /// The qualified / member tail of a dotted path (`a.B`, `M.N.x`): resolving
    /// the segments after the first needs the receiver's type (Phase 3) or the
    /// module / assembly environment (Phase 2).
    QualifiedAccess,
    /// A **type-position** name we decline to resolve because a shadow is
    /// *possible* but we cannot pin it: an opaque / unmodelled `open` could
    /// supply a type of this name, the path is project-shadowed, or several
    /// distinct opens match ambiguously. Recorded so a consumer can distinguish
    /// "maybe shadowed — defer" from a name that genuinely resolves to nothing
    /// (left unrecorded), which means no shadow is possible. Inference reads this
    /// to type a primitive-alias annotation soundly (only at the unrecorded
    /// signal); see `docs/sema-phase3-impl-plan.md` (R1).
    ShadowableType,
}

/// How a name use resolves. A closed, inspectable value (not a callback), per
/// "data descriptions over behavioural abstractions".
///
/// [`Resolution::Unresolved`] is the single error-eligible variant (D5),
/// reserved for Phase 4 diagnostics — never produced here, but the property
/// tests assert we never do, which is why it earns its place now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resolution {
    /// A binding in this file's scope tree that is *not* a top-level export — a
    /// function/lambda parameter or a `match`-clause local.
    Local(DefId),
    /// A top-level module binding (this file, or an earlier Compile-order one).
    Item(ItemId),
    /// A type/module in a referenced assembly (the type a qualified path roots
    /// at, e.g. `Console` in `System.Console.WriteLine`).
    Entity(EntityHandle),
    /// A member of a referenced-assembly type (`WriteLine` in
    /// `System.Console.WriteLine`).
    Member {
        parent: EntityHandle,
        idx: MemberIndex,
    },
    /// In scope-shape but not resolvable yet; never an error (D5).
    Deferred(DeferredReason),
    /// Genuinely absent from every scope and import we model. The only
    /// error-eligible variant; not produced until Phase 4.
    Unresolved,
}

/// The ways an `open` declaration **perturbs later name resolution** — the "why
/// a later name deferred" fact the [`ResolutionTrace`] exposes. Named for its
/// main constituents, the opacity flags, but it also carries the generation
/// barrier (see [`staled_earlier`](Self::staled_earlier)).
///
/// Each of the first three is a walk-state boolean the use-site resolvers
/// consult (see the field docs on the private `Resolver`); the fourth is the
/// `open_generation` bump. An open that triggers none is fully modelled — it
/// perturbs nothing. One that triggers any defers a category of later names
/// while it is in scope:
///
/// - [`opaque_value`](Self::opaque_value) — bare-name lookup skips every
///   *opened* entry (the open could shadow a modelled name with an
///   unenumerable value);
/// - [`opaque_dotted`](Self::opaque_dotted) — dotted-path *heads* defer (the
///   open's submodules / nested types are unmodelled, so a head through it
///   could be project- or assembly-rooted);
/// - [`unmodelled`](Self::unmodelled) — *qualified* paths defer (an `open
///   type`, or a plain `open` of an assembly module / class, whose nested
///   types we cannot enumerate);
/// - [`staled_earlier`](Self::staled_earlier) — the open raised the
///   generation barrier, staling every earlier opened name *and local binding*,
///   so a later dotted head through a staled entry defers even when none of the
///   three flags is set;
/// - [`imported_deferred`](Self::imported_deferred) — the open imported a name
///   that is *itself* `Deferred` (a cross-assembly duplicate, say), so a use of
///   that name defers with the open as its source — even though the open
///   modeled its import fully (no flag, no barrier).
/// - [`added_reading`](Self::added_reading) — the open contributed a namespace
///   **reading** / shortening prefix, a new qualified-path precedence entry.
///   Unlike the five above this is not a deferral mechanism in itself — a
///   reading usually *resolves* a name — but it re-orders qualified-path
///   precedence: a later dotted head can root at *this* reading in preference to
///   a lower open's, and if this reading owns the path with an *uncertain*
///   member (`open Low; open High; M.Mangled`, where `High.M.Mangled` is
///   undecidable) the head defers where deleting this open would let the lower
///   reading resolve. It fires far more broadly than the deferral flags (nearly
///   every meaningful namespace/module open adds a reading), so it marks the open
///   a *candidate* whose precedence a reader must correlate against the token —
///   never a proven cause. This is the reason `perturbs_resolution` means
///   "candidate", not "culprit".
///
/// **Attribution is by transition.** The three flags are monotone within a
/// top-level block (set true, never cleared until the block ends), so this
/// records the ones this open *flipped false→true* — a later open that would
/// independently set an already-set flag records `false` for it (the category
/// was already poisoned), so unblocking a name can need more than one deletion.
/// The generation is *not* monotone-saturating (it bumps on every barrier), so
/// `staled_earlier` fires for each open that raises one.
///
/// **Scope of the claim.** These are the *per-open* deferral mechanisms the
/// trace models — each is a property of the open itself. It does *not* enumerate
/// every deferral cause, and in particular cannot model the **per-token** ones,
/// which depend on the *use site*, not on any one open: an attribute whose
/// in-file type precedes a later open (every open advances the open frontier —
/// `latest_open_pos`), a member/qualified tail pending inference, or
/// pattern-position case suppression (`pattern_suppressed_case_ids`). So an open
/// with every field `false` is not *proof* it perturbs nothing — only that it
/// triggered none of the modeled per-open mechanisms; a caller must not label it
/// harmless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OpenOpacity {
    /// Set `opaque_value_open` — bare-name resolution skips opened entries.
    pub opaque_value: bool,
    /// Set `opaque_dotted_open` — dotted-path heads defer.
    pub opaque_dotted: bool,
    /// Set `unmodelled_open_active` — qualified paths defer.
    pub unmodelled: bool,
    /// Raised the `open_generation` barrier — stales earlier opened names and
    /// local bindings, so a later dotted head through one defers. Fires even
    /// when the three flags stay `false` (a childless assembly module that also
    /// names a namespace with a constructible type is one such open).
    pub staled_earlier: bool,
    /// Imported a name whose resolution is itself `Deferred` — a scope entry the
    /// open pushed that resolves to nothing definite (a cross-assembly duplicate
    /// resolved ambiguously). The open is the *source* of that deferred name,
    /// though it set no flag and raised no barrier.
    pub imported_deferred: bool,
    /// Contributed a namespace **reading** / shortening prefix — a new
    /// qualified-path precedence entry (`self.imports` and/or
    /// `self.open_shortening_prefixes` grew). Not a deferral mechanism itself:
    /// a reading usually *resolves* names. But it re-orders qualified-path
    /// precedence, so a later dotted head can root at this reading over a lower
    /// open's, deferring when this reading owns the path with an uncertain
    /// member. Fires for nearly every meaningful namespace/module open — a
    /// *candidate* marker, not a proven cause.
    pub added_reading: bool,
}

impl OpenOpacity {
    /// Whether this open perturbs later resolution through any modeled per-open
    /// mechanism — an opacity flag, the generation barrier, importing a deferred
    /// name, or adding a namespace-reading precedence entry. The candidate-culprit
    /// predicate; `false` means "triggered none of the modeled mechanisms", not
    /// "provably harmless" (see the type docs' scope note). Because
    /// `added_reading` fires for nearly every meaningful namespace/module open,
    /// `true` is decisively a *candidate*, not a culprit.
    pub fn perturbs_resolution(self) -> bool {
        self.opaque_value
            || self.opaque_dotted
            || self.unmodelled
            || self.staled_earlier
            || self.imported_deferred
            || self.added_reading
    }
}

/// One `open` declaration's contribution to a file's resolution-perturbing
/// state — the unit of the resolution-explain [`ResolutionTrace`]. Purely
/// diagnostic: it carries no resolution the walk consumes, only enough to point
/// a human at an `open` that could defer a name and say how it perturbs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTrace {
    /// Source range of the whole `open …` declaration.
    pub range: TextRange,
    /// The opened path, `idText`-normalised (`["TypeEquality"]`; the type's
    /// path for an `open type`). Empty for `open global` or an unparsed target.
    pub path: Vec<String>,
    /// Whether this is an `open type …` (vs a plain `open <path>`).
    pub is_type: bool,
    /// How this open perturbs later resolution (see [`OpenOpacity`]).
    pub opacity: OpenOpacity,
}

/// The **resolution-explain trace** for one file: every `open` declaration in
/// source order, each with how it perturbs later resolution ([`OpenTrace`]).
///
/// It answers "why did this name defer?" — the investigation `open
/// TypeEquality` poisoning a bare `List.replicate` motivated. Correlate a
/// token's [`Resolution`](ResolvedFile::resolution_at) (a
/// `Deferred(QualifiedAccess)`, say) with the perturbing opens
/// ([`OpenOpacity::perturbs_resolution`]); which one — if any — actually gated
/// this token is left to the reader, since the trace carries neither the head/
/// tail distinction nor per-token block scope.
///
/// Always computed (opens per file are few); read through
/// [`ResolvedFile::resolution_trace`]. Deterministic from source — two parses
/// of the same text trace identically — so it does not perturb the
/// `incremental ≡ batch` fold differential.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolutionTrace {
    /// Every `open` in the file, in source order (all top-level blocks and
    /// nested modules; correlate to a token by comparing [`OpenTrace::range`]).
    pub opens: Vec<OpenTrace>,
}

/// The **constructor-case kind** of an [`ExportedItem`] — which flavour of
/// constructor case it is (all of them also live in the constructor/pattern
/// namespace). `None` on an item's `case` field is an ordinary `let` value.
/// Each variant is set by exactly one producer call site, which already knows
/// the kind (`docs/export-decl-model-plan.md` Stage 1):
///
/// - [`Union`](Self::Union) — a union case; `require_qualified` is `true` for a
///   `[<RequireQualifiedAccess>]` union (reachable only as `Type.Case`), `false`
///   for a plain union case (also bare-/`Mod.Case`-reachable);
/// - [`Enum`](Self::Enum) — an `enum` case (always require-qualified);
/// - [`Exception`](Self::Exception) — an `exception E of …` constructor.
///
/// Stage 1 stores the kind but does not yet read it (the boolean
/// `ExportedItem::is_case` still drives every cross-file index); the
/// kind-sensitive commits it unlocks land in Stage 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseKind {
    /// A union case. `require_qualified` mirrors the union's
    /// `[<RequireQualifiedAccess>]` attribute — `true` when the case is reachable
    /// only as `Type.Case`, `false` for a plain (also bare-reachable) union case.
    Union { require_qualified: bool },
    /// An `enum` case (`type Color = Red = 0 | …`), always require-qualified.
    Enum,
    /// An `exception E of …` constructor.
    Exception,
}

/// A top-level binding a file contributes to files later in Compile order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedItem {
    /// The exported name, with backticks stripped (`idText` semantics).
    pub(super) name: String,
    /// The module-qualified *path* (`["Shared", "foo"]`) a later file must use
    /// to reference this binding, or `None` when the binding sits in an
    /// anonymous (header-less) module and so has no qualified cross-file path we
    /// model. Only `Some` paths enter [`ProjectItems`]'s cross-file index.
    pub(super) qualified: Option<Vec<String>>,
    /// This binding's project-global handle.
    pub(super) id: ItemId,
    /// The defining binder in the resolved file's arena.
    pub(super) def: DefId,
    /// The **constructor-case kind** of this export ([`CaseKind`]), or `None` for
    /// an ordinary `let` value. `Some(_)` marks the export as also live in the
    /// constructor (pattern) namespace — a union case, an `enum` case, or an
    /// `exception` constructor. The boolean projection [`Self::is_case`] is
    /// carried into [`ProjectItems::case_item_ids`] so a *later* file can tell
    /// whether an opened cross-file [`Resolution::Item`] is a case — needed in
    /// pattern position ([`Resolver::case_reference`](super::state::Resolver::case_reference)) and to keep a
    /// case/module name collision sound
    /// ([`Resolver::case_classification`](super::state::Resolver::case_classification)). The kind itself
    /// (RQA union vs enum vs exception) is the payload the cross-file
    /// constructor-namespace and active-pattern features consume
    /// (`docs/export-decl-model-plan.md`).
    pub(super) case: Option<CaseKind>,
    /// This export's **accessibility**, as a prefix length of [`Self::qualified`]:
    /// `None` = public (accessible everywhere); `Some(k)` = accessible only from a
    /// reference site within the `k`-segment prefix of the qualified path — the
    /// export's *access-root*. The access-root is F#'s `private` visibility scope,
    /// oracle-pinned and **inherited**:
    ///
    /// - own `let private X` at `[…C, X]` → `Some(C.len())` (its container `C`);
    /// - a value in `module private M` (M at `[…P, M]`) → `Some(P.len())` (M's
    ///   *parent* `P` — a `private` module is visible in its enclosing scope);
    /// - a union/`exception` case of a `private` type in `M` → `Some(M.len())`;
    /// - a public export → `None`.
    ///
    /// Stacked `private` boundaries take the **deepest** (longest prefix). Because
    /// the access-root is always a proper prefix (a container) of the path, a
    /// length suffices, and [`Resolver::open_module_values`](super::state::Resolver::open_module_values)
    /// / the collapse recovery filter through [`accessible_from`]. `internal` is
    /// intra-project-visible (one assembly), so only `private` narrows this.
    pub(super) access_root_len: Option<usize>,
    /// `true` when the defining module-level `let` binding carried **any
    /// attribute list** (on the binding or its enclosing `let` decl) — a
    /// maybe-`[<Literal>]`. A literal value is an FCS *constant pattern*, which
    /// contests the pattern namespace against constructor cases (latest-wins in
    /// `ePatItems`), and attribute identity cannot be verified from source (a
    /// shadowing `LiteralAttribute` alias is undetectable), so presence is the
    /// sound over-approximation: an attributed value makes a colliding bare case
    /// use **defer**, while an unattributed value provably cannot be a literal
    /// and never contests. Always `false` for cases and active-pattern case
    /// handles.
    pub(super) attributed: bool,
}

impl ExportedItem {
    /// The exported name (`idText`-normalised: surrounding double-backticks
    /// removed).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The module-qualified cross-file path, if any (see the field docs).
    pub fn qualified_path(&self) -> Option<&[String]> {
        self.qualified.as_deref()
    }

    /// This binding's project-global [`ItemId`].
    pub fn id(&self) -> ItemId {
        self.id
    }

    /// The defining binder, an index into [`ResolvedFile`]'s arena.
    pub fn def(&self) -> DefId {
        self.def
    }

    /// Whether this export is a **constructor case** (a union / `enum` /
    /// `exception` constructor — see [`Self::case`]), i.e. also live in the
    /// constructor (pattern) namespace. The boolean the cross-file indices key
    /// on; a lossless projection of [`Self::case_kind`].
    pub(super) fn is_case(&self) -> bool {
        self.case.is_some()
    }

    /// The [`CaseKind`] this export carries, or `None` for an ordinary value
    /// (see the `case` field). Exposes the stored kind so it can be observed;
    /// there is no runtime consumer in Stage 1 of `docs/export-decl-model-plan.md`
    /// (the kind-sensitive commits land in Stage 3).
    pub fn case_kind(&self) -> Option<CaseKind> {
        self.case
    }
}

/// The top-level bindings a file exports, in source order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExportedItems {
    pub(super) items: Vec<ExportedItem>,
}

impl ExportedItems {
    /// The exported items, in source order.
    pub fn iter(&self) -> impl Iterator<Item = &ExportedItem> + '_ {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// The result of resolving a single file: the definition arena, a map from each
/// name-use range to its [`Resolution`], and the items the file exports.
///
/// [`PartialEq`] is by *value* — two `ResolvedFile`s are equal when their whole
/// modelled content matches (the resolutions map, binder arena, exports, every
/// threaded field, and diagnostics), independent of the source-tree instance
/// they were resolved from. This is what lets the incremental fold be
/// differentially tested against the cold fold (`incremental ≡ batch`): two
/// independent parses of the same text resolve to *equal* files even though
/// their rowan nodes differ by identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFile {
    /// All binders introduced in the file, indexed by [`DefId`].
    pub(super) defs: Vec<Def>,
    /// Every resolved occurrence (uses *and* the binders' own self-references),
    /// keyed by the occurrence's source range.
    pub(super) resolutions: HashMap<TextRange, Resolution>,
    /// The *type* each written attribute resolved to (EX-3 §2(d),
    /// `docs/extension-scope-enumeration-plan.md`), keyed by the written
    /// attribute name's range (the full dotted path, matching FCS's
    /// `rangeOfLid`). Kept apart from [`Self::resolutions`] — the attribute
    /// name is a *type* use resolved through FCS's suffix-first candidate
    /// order, a different query than the ranges the main map's differentials
    /// pin. [`Resolution::Entity`] / [`Resolution::Local`] are commitments
    /// (certain-implies-exact against the `attrs` oracle);
    /// [`Resolution::Deferred`] makes no claim; an attribute neither candidate
    /// matches is *absent* (FCS errors and sinks nothing there).
    pub(super) attribute_resolutions: HashMap<TextRange, Resolution>,
    /// The file's syntactic whole-file type-simple-name pre-scan (see
    /// [`Resolver::own_type_simple_names`](super::state::Resolver)), carried
    /// into [`ProjectItems::project_type_simple_names`] by the Compile-order
    /// fold — header-independent, so a headerless file's types guard later
    /// files' attribute candidates too.
    pub(super) own_type_simple_names: HashSet<String>,
    /// The abbreviation-declared subset of [`Self::own_type_simple_names`] —
    /// a committed [`Resolution::Local`] attribute resolution of such a name
    /// may alias `ExtensionAttribute`, so
    /// [`Self::attributes_may_declare_extension`] treats it as a possible
    /// marker.
    pub(super) own_abbrev_type_simple_names: HashSet<String>,
    /// `true` when some attribute has no resolvable name shape (nameless
    /// `[<>]`, an ident-less path) — unkeyable, so the gate keeps the
    /// presence defer.
    pub(super) attribute_shape_unknowable: bool,
    /// The instance / static member names this file's `type … with`
    /// augmentations declare, and whether any member's name was not walkable
    /// (EX-3 §2(a); see
    /// [`Resolver::collect_augmentation_extension_names`](super::state::Resolver)).
    pub(super) augmentation_instance_names: HashSet<String>,
    pub(super) augmentation_static_names: HashSet<String>,
    pub(super) augmentation_names_unknowable: bool,
    /// The augmentation member names **preceding Compile-order files**
    /// declare, accumulated by the fold (EX-3 §2(b)) — empty for a
    /// single-file caller. The cross-file sibling of the own-file sets above;
    /// preceding files' *un-walkable* members fold into
    /// [`Self::preceding_declares_extension_source`] instead (wholesale).
    pub(super) preceding_augmentation_instance_names: HashSet<String>,
    pub(super) preceding_augmentation_static_names: HashSet<String>,
    pub(super) exports: ExportedItems,
    /// The project-global [`ItemId`] of this file's *first* export — the base
    /// of the contiguous range its items occupy. Zero for a single-file caller;
    /// set by the Compile-order fold otherwise. Lets [`Self::resolved_def`] map
    /// an own-item handle back to a local export index.
    pub(super) item_base: u32,
    /// The file's declared project **namespace** paths (`["Outer", "Inner"]`),
    /// from its `namespace` header(s), each with its ancestor prefixes. Retained
    /// as a file-level fact because the OV-6 extension gate reads it through the
    /// public [`Self::namespace_paths`] accessor (`crates/sema/src/infer.rs`); the
    /// *cross-file* namespace index now derives from the `Namespace`
    /// [`ExportDecl`]s (see [`ProjectItems::extend_with`]).
    pub(super) namespace_paths: Vec<Vec<String>>,
    /// Whether a **preceding** Compile-order file declares a project **extension
    /// source** the fold could not name-key — an augmentation member with an
    /// un-walkable name (EX-3 §2(b)) or an attribute that may declare a C#-style
    /// `[<Extension>]` (EX-3 §2(d)) — that a later same-namespace file sees with
    /// no explicit `open` (the OV-6 gate's *cross-file* extension signal; the
    /// walkable augmentation names thread as the `preceding_augmentation_*` sets
    /// instead, and an auto-open module *as such* contributes nothing — AO-1: its
    /// extension-capable contents are exactly those two signals). Set by
    /// [`resolve_project`](super::resolve_project) as the forward accumulation
    /// over Compile order (F# is order-sensitive — a later file's source is not
    /// in scope earlier); the single-file [`resolve_file`](super::resolve_file)
    /// leaves it `false` (no preceding files). The gate defers wholesale when
    /// true.
    pub(super) preceding_declares_extension_source: bool,
    /// EX-2 (`docs/extension-scope-enumeration-plan.md`): the **assembly**
    /// namespace paths this file's explicit `open <namespace>` clauses bring into
    /// scope. The overload engine's extension-absence gate asks the same
    /// `extension_named_in_scope` query about these as about the file's declared
    /// namespace chain — an extension in an opened assembly namespace is in scope
    /// exactly as one in an enclosing namespace is. See
    /// [`Resolver::open_extension_namespaces`](super::state::Resolver).
    pub(super) open_extension_namespaces: Vec<Vec<String>>,
    /// EX-2: some `open` in this file brings an extension surface whose names the
    /// resolver cannot enumerate (a project open — EX-3 — an assembly module /
    /// `open type`, or an opaque / vetoed / dropped-path open). The gate defers
    /// every method-call commit in the file when set. See
    /// [`Resolver::open_extension_unknowable`](super::state::Resolver).
    pub(super) open_extension_unknowable: bool,
    /// The [`ActivePatternShape`] of each same-file active-pattern recognizer,
    /// keyed by each of its per-case *use def ids* (the `Resolution::Local`
    /// identity a case use resolves to). Read through
    /// [`Self::active_pattern_shape`]; see
    /// [`Resolver::active_pattern_shape`](super::state::Resolver::active_pattern_shape).
    pub(super) active_pattern_shape: HashMap<DefId, ActivePatternShape>,
    /// Always-sound semantic diagnostics found while walking the file (today
    /// only `use rec`; see [`SemaDiagnostic`]). Source-ordered.
    pub(super) diagnostics: Vec<SemaDiagnostic>,
    /// The **resolution-explain trace**: every `open` in the file with the
    /// opaque-open flags it set ([`ResolutionTrace`]). Purely diagnostic — no
    /// cross-file fold or use-site resolver reads it — so it is excluded from
    /// [`Self::same_export_contribution`]; deterministic from source, so it does
    /// not perturb the `incremental ≡ batch` value-equality differential. Read
    /// through [`Self::resolution_trace`].
    pub(super) resolution_trace: ResolutionTrace,
    /// The file's cross-file declarations, in source order — the single currency
    /// [`ProjectItems::extend_with`] folds (`docs/export-decl-model-plan.md`
    /// Stage 2). Every cross-file index derives from this list.
    pub(super) export_decls: Vec<ExportDecl>,
    /// `Some` iff this is a `.fsi` **signature file**
    /// (`docs/fsi-signature-restriction-plan.md` Stage 1): its screen — the
    /// only thing a signature contributes to the fold today. A signature's
    /// other fields are all empty/inert (it owns no `ItemId` range, exports
    /// nothing, and records no resolutions until Stage 2). `None` for every
    /// implementation file.
    pub(super) sig_screen: Option<Arc<SigScreen>>,
}

/// Build the end-offset index a token classifier queries, from a set of
/// resolved occurrences and a per-resolution classify function. The shared core
/// of [`ResolvedFile::token_classifier`] and [`ResolvedProject::token_classifier`]:
/// index each *committed* occurrence by its **end** offset, keeping the widest
/// (smallest-start) occurrence ending there, so a qualified tail token maps to
/// the whole dotted path it closes while a qualifier head keeps its own segment.
/// Built once (O(occurrences)). The two callers differ only in `classify` — a
/// single file classifies from its own arena; a project also follows a cross-file
/// `Item` to its declaring file.
///
/// Returning the *owned* index — rather than a closure over it — is what keeps
/// the public classifiers detached: `classify` borrows the resolved file, but the
/// returned `HashMap` does not, so [`end_index_classifier`] can wrap it into an
/// `impl Fn(..) + use<>` that outlives the file it was built from.
fn build_end_index(
    occurrences: &HashMap<TextRange, Resolution>,
    classify: impl Fn(Resolution) -> Option<SemanticClass>,
) -> HashMap<TextSize, (TextSize, SemanticClass)> {
    let mut by_end: HashMap<TextSize, (TextSize, SemanticClass)> = HashMap::new();
    for (occ, res) in occurrences {
        if let Some(class) = classify(*res) {
            let entry = by_end.entry(occ.end()).or_insert((occ.start(), class));
            if occ.start() < entry.0 {
                *entry = (occ.start(), class);
            }
        }
    }
    by_end
}

/// Wrap an owned end-offset index (from [`build_end_index`]) into a token
/// classifier: each query is O(1). The `+ use<>` bound keeps the opaque return
/// type precisely captured — it owns `by_end` and borrows nothing, so callers may
/// retain the classifier after the resolved file it was built from is dropped, or
/// require a `'static` classifier.
fn end_index_classifier(
    by_end: HashMap<TextSize, (TextSize, SemanticClass)>,
) -> impl Fn(TextRange) -> Option<SemanticClass> + use<> {
    move |token: TextRange| {
        by_end
            .get(&token.end())
            // The occurrence ending here must contain the token — always true for
            // a segment of a qualified path; guarded defensively.
            .and_then(|&(start, class)| (start <= token.start()).then_some(class))
    }
}

impl ResolvedFile {
    /// Whether `self` and `other` fold into the Compile-order [`ProjectItems`]
    /// accumulator *identically* — i.e. [`ProjectItems::extend_with`] would grow
    /// the accumulator the same way from either. Compares exactly the three things
    /// `extend_with` reads from a file and nothing else: the resolution map,
    /// binder arena, diagnostics, and file-local extension-scope flags do not
    /// feed the cross-file fold, so two files that differ only in those still
    /// contribute the same downstream state.
    ///
    /// The incremental fold ([`resolve_project_incremental`](super::resolve_project_incremental))
    /// calls this on a *recomputed* file to decide whether its export
    /// contribution changed; if not, later files whose parse tree is unchanged
    /// can still be reused. **Precondition:** both were resolved with the same
    /// entering item base — guaranteed by the caller, which compares only while
    /// the prefix is still in sync — so the [`ItemId`]s inside the exports are
    /// numbered from the same origin and hence directly comparable.
    ///
    /// **Drift-proof by construction.** Rather than re-list the source fields the
    /// fold reads (which invites the two ways that list can silently fall out of
    /// step with the fold: comparing a field the fold *doesn't* read — e.g. a
    /// decl's `pos` provenance — spuriously invalidates the suffix; omitting one
    /// the fold *does* read reuses a stale suffix, which is unsound), it compares
    /// the fold's own input:
    ///
    /// - the item **count** (`extend_with` advances [`ProjectItems`]'s id base by
    ///   `exports.items.len()`, so a differing length renumbers every later file's
    ///   [`ItemId`]s);
    /// - the **derived indices** [`FileExportIndices::from_decls`] builds from
    ///   `export_decls` and the `exports.items` those decls point at — the exact
    ///   value `extend_with` folds, so equality of the two is definitionally
    ///   "folds identically" (`pos` cannot enter, because `from_decls` never reads
    ///   it; a body edit that only shifts positions leaves the indices identical);
    /// - `own_type_simple_names`, folded into the cross-file attribute-guard set
    ///   (`project_type_simple_names`) and *not* derivable from the decls (a
    ///   headerless file's types export nothing yet still count).
    ///
    /// A field newly read by the fold is picked up here automatically as long as
    /// it flows through one of those three — no second edit site to forget. The
    /// `body_edit_preserves_full_suffix_reuse` property (`resolve_incremental_diff.rs`)
    /// is the generative guard on the spurious-invalidation direction, which the
    /// `incremental ≡ batch` differential (blind to over-comparison — it stays
    /// green when reuse is merely *missed*) cannot see.
    ///
    /// The extension-source signal has a *second* half, the file's own
    /// augmentation/attribute syntax, that is not a [`ResolvedFile`] field; the
    /// incremental fold checks it separately (it needs the source tree, which
    /// this method does not take).
    pub(super) fn same_export_contribution(&self, other: &ResolvedFile) -> bool {
        self.exports.items.len() == other.exports.items.len()
            && FileExportIndices::from_decls(self) == FileExportIndices::from_decls(other)
            && self.own_type_simple_names == other.own_type_simple_names
            // A signature file's whole contribution is its screen
            // (`extend_with` pushes it into `ProjectItems::sig_screens`), so
            // a `.fsi` edit that changes the screen must invalidate the
            // suffix. The screen also parameterises the *paired
            // implementation's* derivation (`from_decls_screened`) — the
            // incremental fold covers that side by requiring the pairing
            // partner index to match while the prefix (the signature's own
            // contribution included) is in sync.
            && self.sig_screen == other.sig_screen
    }

    /// The inert [`ResolvedFile`] a `.fsi` **signature file** occupies a
    /// Compile slot with (`docs/fsi-signature-restriction-plan.md` Stage 1):
    /// no binders, no resolutions, no exports — it owns no `ItemId` range
    /// (`item_base` is the running count, its range empty) — only its screen.
    /// Stage 2 replaces this with a real signature surface.
    pub(super) fn inert_signature(item_base: u32, screen: Arc<SigScreen>) -> ResolvedFile {
        ResolvedFile {
            defs: Vec::new(),
            resolutions: HashMap::new(),
            attribute_resolutions: HashMap::new(),
            own_type_simple_names: HashSet::new(),
            own_abbrev_type_simple_names: HashSet::new(),
            attribute_shape_unknowable: false,
            augmentation_instance_names: HashSet::new(),
            augmentation_static_names: HashSet::new(),
            augmentation_names_unknowable: false,
            preceding_augmentation_instance_names: HashSet::new(),
            preceding_augmentation_static_names: HashSet::new(),
            exports: ExportedItems::default(),
            item_base,
            namespace_paths: Vec::new(),
            preceding_declares_extension_source: false,
            open_extension_namespaces: Vec::new(),
            open_extension_unknowable: false,
            active_pattern_shape: HashMap::new(),
            diagnostics: Vec::new(),
            export_decls: Vec::new(),
            sig_screen: Some(screen),
        }
    }

    /// The resolution recorded at `range`, if any occurrence was resolved
    /// there. A binder's own defining range resolves to itself, so this answers
    /// go-to-definition for both references and definitions.
    pub fn resolution_at(&self, range: TextRange) -> Option<Resolution> {
        self.resolutions.get(&range).copied()
    }

    /// The file's resolution-explain trace — every `open` with how it perturbs
    /// later resolution (see [`ResolutionTrace`]). Pair it with
    /// [`Self::resolution_at`] to investigate *why* a name deferred: a
    /// `Deferred(QualifiedAccess)` head and an [`OpenTrace`] whose
    /// [`opacity`](OpenTrace::opacity)
    /// [`perturbs_resolution()`](OpenOpacity::perturbs_resolution) are the
    /// candidate correlation (which open — if any — gated it is for the caller
    /// to judge; see [`ResolutionTrace`]).
    pub fn resolution_trace(&self) -> &ResolutionTrace {
        &self.resolution_trace
    }

    /// The type the attribute written at `range` resolved to, if the resolver
    /// made any claim there (see [`Self::attribute_resolutions`]).
    pub fn attribute_resolution_at(&self, range: TextRange) -> Option<Resolution> {
        self.attribute_resolutions.get(&range).copied()
    }

    /// The full written-attribute-range→[`Resolution`] map (EX-3 §2(d)).
    pub fn attribute_resolutions(&self) -> &HashMap<TextRange, Resolution> {
        &self.attribute_resolutions
    }

    /// The instance-member names this file's augmentations declare (EX-3 §2(a)).
    pub fn augmentation_instance_names(&self) -> &HashSet<String> {
        &self.augmentation_instance_names
    }

    /// The static-member sibling of [`Self::augmentation_instance_names`].
    pub fn augmentation_static_names(&self) -> &HashSet<String> {
        &self.augmentation_static_names
    }

    /// Whether some augmentation member's name was not walkable — the gate
    /// keeps the wholesale defer (EX-3 §2(a)).
    pub fn augmentation_names_unknowable(&self) -> bool {
        self.augmentation_names_unknowable
    }

    /// The instance-member names preceding Compile-order files' augmentations
    /// declare (EX-3 §2(b); empty for a single-file caller).
    pub fn preceding_augmentation_instance_names(&self) -> &HashSet<String> {
        &self.preceding_augmentation_instance_names
    }

    /// The static-member sibling of
    /// [`Self::preceding_augmentation_instance_names`].
    pub fn preceding_augmentation_static_names(&self) -> &HashSet<String> {
        &self.preceding_augmentation_static_names
    }

    /// Whether a same-file `type … with` augmentation could contribute an
    /// extension member **named `name`** to a call of the given shape —
    /// EX-3 §2(a)'s name-keyed refinement of the old "any augmentation ⇒
    /// defer" trigger. `true` also when some augmentation member's name was
    /// not walkable (the unknowable bit — wholesale, like every other
    /// unknowable surface).
    pub fn augmentation_declares_extension_named(&self, name: &str, is_static: bool) -> bool {
        self.augmentation_names_unknowable
            || if is_static {
                self.augmentation_static_names.contains(name)
            } else {
                self.augmentation_instance_names.contains(name)
            }
    }

    /// Whether some attribute in this file **may declare an extension** — the
    /// EX-3 §2(d) stage-5 refinement of the old "any attribute ⇒ defer"
    /// trigger, derived from the per-attribute resolutions the stage-3/4
    /// differentials validate:
    ///
    /// - an attribute with an unkeyable *shape* (nameless `[<>]`) — defer;
    /// - a [`Resolution::Deferred`] verdict — the resolver could not pin the
    ///   type, so it could be (an alias of) `ExtensionAttribute` — defer;
    /// - a committed [`Resolution::Entity`] — an extension marker exactly
    ///   when it **is** `System.Runtime.CompilerServices.ExtensionAttribute`
    ///   ([`AssemblyEnv::is_extension_attribute`]); any other concrete type
    ///   provably is not;
    /// - a committed [`Resolution::Local`] — provably not the marker when the
    ///   in-file declaration is a concrete type (its own tycon, never the
    ///   SRCS one); possibly the marker when it is an **abbreviation** (the
    ///   resolver does not chase in-file targets) — defer on those;
    /// - an attribute with **no record** — both candidates missed everywhere
    ///   with no shadow possible (the differential's decline-by-absence
    ///   agreement: FCS errors and binds nothing) — contributes nothing.
    pub fn attributes_may_declare_extension(&self, env: &AssemblyEnv) -> bool {
        self.attribute_shape_unknowable
            || self.attribute_resolutions.values().any(|res| match res {
                Resolution::Deferred(_) => true,
                Resolution::Entity(h) => env.is_extension_attribute(*h),
                Resolution::Local(id) => {
                    let name = &self.defs[id.index()].name;
                    self.own_abbrev_type_simple_names
                        .contains(super::id_text(name))
                }
                // No other variant is ever recorded for an attribute; if one
                // appears, defer rather than trust it.
                _ => true,
            })
    }

    /// The full range→[`Resolution`] map.
    pub fn resolutions(&self) -> &HashMap<TextRange, Resolution> {
        &self.resolutions
    }

    /// The binder a [`DefId`] names.
    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.index()]
    }

    /// The items this file exports.
    pub fn exports(&self) -> &ExportedItems {
        &self.exports
    }

    /// The always-sound semantic diagnostics found in this file (today only
    /// `use rec`; see [`SemaDiagnostic`]), in source order.
    pub fn diagnostics(&self) -> &[SemaDiagnostic] {
        &self.diagnostics
    }

    /// Whether a preceding Compile-order file declares a project extension source
    /// the fold could not name-key (an un-walkable augmentation member name or an
    /// attribute that may declare a `[<Extension>]`) — the OV-6 gate's *cross-file*
    /// extension signal (see the field
    /// [`Self::preceding_declares_extension_source`]). The LSP inference layer
    /// reads it.
    pub fn preceding_declares_extension_source(&self) -> bool {
        self.preceding_declares_extension_source
    }

    /// The **assembly** namespace paths this file's explicit `open <namespace>`
    /// clauses bring into scope (EX-2 — see the field
    /// [`Self::open_extension_namespaces`]). The overload engine's
    /// extension-absence gate folds these into its in-scope namespace set.
    pub fn open_extension_namespaces(&self) -> &[Vec<String>] {
        &self.open_extension_namespaces
    }

    /// Whether some `open` in this file brings an extension surface whose names the
    /// resolver cannot enumerate (EX-2 — see the field
    /// [`Self::open_extension_unknowable`]). The gate defers wholesale when true.
    pub fn open_extension_unknowable(&self) -> bool {
        self.open_extension_unknowable
    }

    /// The file's declared project **namespace** paths, each with its ancestor
    /// prefixes (`namespace A.B` ⇒ `["A"]`, `["A", "B"]`). The OV-6 extension gate
    /// folds these: F# treats a file's enclosing namespace as an extension-method
    /// scope with no explicit `open`, so a referenced assembly's extension in that
    /// namespace is in scope here.
    pub fn namespace_paths(&self) -> &[Vec<String>] {
        &self.namespace_paths
    }

    /// The project-global base of this file's items — the [`ItemId`] of its
    /// first export. See [`Self::item_base`].
    pub fn item_base(&self) -> u32 {
        self.item_base
    }

    /// The in-file [`Def`] a resolution points at, if it points at a local or
    /// an item **of this file**. A cross-file [`Resolution::Item`] (handle
    /// outside this file's range), `Deferred`, and `Unresolved` return `None`;
    /// reach a cross-file item through [`ResolvedProject::item_def`].
    pub fn resolved_def(&self, res: Resolution) -> Option<&Def> {
        match res {
            Resolution::Local(id) => Some(self.def(id)),
            Resolution::Item(id) => {
                let local = id.index().checked_sub(self.item_base as usize)?;
                let item = self.exports.items.get(local)?;
                Some(self.def(item.def))
            }
            // `Entity` / `Member` resolve into referenced assemblies, not this
            // file's def arena; reach them through the [`AssemblyEnv`](crate::AssemblyEnv) the
            // resolution was produced against.
            Resolution::Entity(_)
            | Resolution::Member { .. }
            | Resolution::Deferred(_)
            | Resolution::Unresolved => None,
        }
    }

    /// The [`ActivePatternShape`] of the same-file active-pattern recognizer a
    /// resolution names, if `res` is a case *use* of one. A module-level case use
    /// resolves to [`Resolution::Item`] (Stage 3a — one identity shared with
    /// cross-file uses), an anonymous-root / local one to [`Resolution::Local`];
    /// both key the stored shape by the case's use-def, so an `Item` is mapped to
    /// that def through this file's exports. `None` for any other resolution — a
    /// non-active-pattern binder, a **cross-file** item, a referenced-assembly
    /// entity, `Deferred`, `Unresolved`.
    pub fn active_pattern_shape(&self, res: Resolution) -> Option<ActivePatternShape> {
        match res {
            Resolution::Local(id) => self.active_pattern_shape.get(&id).copied(),
            // A same-file `Item` (a module-level AP case handle): map it to its
            // exported use-def, then the shape. A cross-file `Item` (out of this
            // file's range) is `None` — same-file-only, like `resolved_def`.
            Resolution::Item(id) => {
                let local = id.index().checked_sub(self.item_base as usize)?;
                let item = self.exports.items.get(local)?;
                self.active_pattern_shape.get(&item.def).copied()
            }
            _ => None,
        }
    }

    /// The [`SemanticClass`] of the name occurrence at `range`, or `None` when
    /// we decline to classify it.
    ///
    /// We commit only where name resolution reaches a binder **of this file** (a
    /// [`Resolution::Local`] or a same-file [`Resolution::Item`]); a cross-file
    /// item, a referenced-assembly [`Resolution::Entity`] / [`Resolution::Member`],
    /// a [`Resolution::Deferred`], a [`Resolution::Unresolved`], and any range
    /// that resolved to nothing all decline. This is the same
    /// say-nothing-when-unsure contract [`Resolution`] itself keeps: a decline
    /// makes no claim, so the classification differential can hold every `Some`
    /// against FCS without a decline ever counting against us. Cross-file and
    /// cross-assembly commitments arrive with
    /// [`ResolvedProject`]-level classification in a later stage.
    pub fn classification_at(&self, range: TextRange) -> Option<SemanticClass> {
        self.class_of(self.resolution_at(range)?)
    }

    /// The [`SemanticClass`] a resolution commits to, or `None` where we decline.
    /// [`Self::resolved_def`] is itself the commit gate: it yields a `Def` only
    /// for a [`Resolution::Local`] or a same-file [`Resolution::Item`], and
    /// `None` for a cross-file item, an `Entity` / `Member`, `Deferred`, and
    /// `Unresolved` — exactly the cases we decline.
    fn class_of(&self, res: Resolution) -> Option<SemanticClass> {
        Some(self.resolved_def(res)?.kind.semantic_class())
    }

    /// A token-oriented classifier: given an **identifier token** range, the
    /// [`SemanticClass`] of the (possibly qualified) name occurrence it belongs
    /// to. This is the entry point a semantic-token highlighter wants, and it
    /// differs from [`Self::classification_at`] in one way that matters: a
    /// *qualified* reference (`Color.Red`, `C.P`) records its resolution under
    /// the **whole dotted span** — which ends at the tail segment — while the
    /// lexer hands one token per segment. So the tail token (`Red`, `P`) has no
    /// exact key of its own (enum cases are require-qualified and member
    /// definitions are not self-recorded), and an exact lookup would decline it.
    ///
    /// The classifier keys each committed occurrence by its **end** offset,
    /// taking the widest (smallest-start) occurrence ending there — the full
    /// path a tail token closes. A qualifier head (`Color` in `Color.Red`) keeps
    /// its own occurrence, which ends at that segment. The index is built once
    /// (O(n) in the occurrence count); each query is O(1), so classifying a whole
    /// file's tokens stays linear rather than rescanning the map per token. Only
    /// occurrences we classify — an in-file binder, the same commit gate as
    /// [`Self::classification_at`] — enter it; a cross-file / referenced-assembly
    /// tail still declines, exactly as an exact lookup would.
    pub fn token_classifier(&self) -> impl Fn(TextRange) -> Option<SemanticClass> + use<> {
        end_index_classifier(build_end_index(self.resolutions(), |res| {
            self.class_of(res)
        }))
    }

    /// The [`DefId`] a resolution points at, if it names a local or an item
    /// **of this file** — the [`Self::resolved_def`] companion that returns the
    /// identity rather than the [`Def`]. Used by inference to key a binder's
    /// inference variable by a stable handle so a value *use* can be unified
    /// with its binding. Same in-file restriction: a cross-file
    /// [`Resolution::Item`], `Entity`, `Member`, `Deferred`, and `Unresolved`
    /// return `None`.
    pub fn resolved_def_id(&self, res: Resolution) -> Option<DefId> {
        match res {
            Resolution::Local(id) => Some(id),
            Resolution::Item(id) => {
                let local = id.index().checked_sub(self.item_base as usize)?;
                let item = self.exports.items.get(local)?;
                Some(item.def)
            }
            Resolution::Entity(_)
            | Resolution::Member { .. }
            | Resolution::Deferred(_)
            | Resolution::Unresolved => None,
        }
    }
}

/// The result of resolving a whole project: the per-file [`ResolvedFile`]s in
/// Compile order. Routes a project-global [`Resolution::Item`] to its declaring
/// file, which a single [`ResolvedFile`] cannot do for cross-file handles.
///
/// Each file is held behind an [`Arc`] so the incremental fold
/// ([`resolve_project_incremental`](super::resolve_project_incremental)) can
/// reuse an unchanged file's whole result with a refcount bump instead of a deep
/// clone of its resolution map, binder arena, and exports — the reuse must be
/// O(1) per file, or a keystroke would still pay O(project) to *copy* what it
/// avoided re-resolving. Equality is still by value ([`Arc`] delegates to the
/// pointee), so the `incremental ≡ batch` differential compares content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProject {
    pub(super) files: Vec<Arc<ResolvedFile>>,
}

impl ResolvedProject {
    /// The resolved files, in Compile order. Each is [`Arc`]-shared so a reused
    /// file (an incremental fold's unchanged file) is the *same* allocation as in
    /// the previous fold — [`Arc::ptr_eq`] against a prior result detects reuse.
    pub fn files(&self) -> &[Arc<ResolvedFile>] {
        &self.files
    }

    /// The resolved file at Compile-order index `idx`.
    pub fn file(&self, idx: usize) -> &ResolvedFile {
        &self.files[idx]
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The declaring file's Compile-order index and the [`Def`] an item handle
    /// names. Each file owns a contiguous [`ItemId`] range, so the owner is the
    /// file whose range contains the handle. Returns `None` for a non-`Item`
    /// resolution or an out-of-range handle.
    pub fn item_def(&self, res: Resolution) -> Option<(usize, &Def)> {
        let Resolution::Item(id) = res else {
            return None;
        };
        self.files.iter().enumerate().find_map(|(idx, f)| {
            let base = f.item_base() as usize;
            (id.index() >= base && id.index() < base + f.exports.items.len())
                .then(|| f.resolved_def(res).map(|def| (idx, def)))
                .flatten()
        })
    }

    /// project and its referenced assemblies**:
    ///
    /// - a *cross-file* reference — a name this file uses that an earlier
    ///   Compile-order file defines (a [`Resolution::Item`] whose binder lives
    ///   elsewhere) — is classified via its declaring file's binder;
    /// - a *referenced-assembly* reference — the type a qualified path roots at
    ///   ([`Resolution::Entity`]) or a member of it ([`Resolution::Member`]) —
    ///   is classified against `env` ([`AssemblyEnv::entity_class`] /
    ///   [`AssemblyEnv::member_class`]).
    ///
    /// `env` must be the [`AssemblyEnv`] the project was resolved against, so the
    /// handles its resolutions carry index into it.
    pub fn token_classifier(
        &self,
        file_idx: usize,
        env: &AssemblyEnv,
    ) -> impl Fn(TextRange) -> Option<SemanticClass> + use<> {
        let file = self.file(file_idx);
        end_index_classifier(build_end_index(file.resolutions(), |res| {
            // A `Local` or same-file `Item` classifies from this file's arena; a
            // cross-file `Item` follows to its declaring file's binder (`item_def`
            // handles same-file items too, so that arm only fires for `Local`s and
            // genuinely cross-file items); an `Entity` / `Member` classifies
            // against the referenced-assembly env.
            file.class_of(res)
                .or_else(|| self.item_def(res).map(|(_, def)| def.kind.semantic_class()))
                .or_else(|| match res {
                    Resolution::Entity(handle) => env.entity_class(handle),
                    Resolution::Member { parent, idx } => env.member_class(parent, idx),
                    _ => None,
                })
        }))
    }
}

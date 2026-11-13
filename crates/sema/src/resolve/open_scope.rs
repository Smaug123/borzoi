//! The walk's **open scope** ([`OpenScope`]): every piece of resolver state
//! that an `open`, a `module rec` header or a nested module's header sets, and
//! that therefore lives exactly as long as the module that wrote it.
//!
//! A top-level block starts from [`OpenScope::block_start`]. A nested module
//! clones the scope on entry and, on exit, hands the clone and the body's final
//! scope to [`OpenScope::leave`], which names each field that does not simply
//! come back. Every other field returns to its value on entry by construction,
//! so a field added here is restored without anyone remembering to.
//!
//! The container path and the block's local-path shadow sets
//! ([`Resolver::nested_module_locals`], [`Resolver::augmentation_head_locals`])
//! share this lifetime but stay outside: each is restored through its own
//! mark ([`ContainerPath::mark`](super::container::ContainerPath::mark),
//! [`PathSet::mark`](super::path_set::PathSet::mark)), which costs nothing,
//! where a clone of either would copy every path the file has entered.

use std::rc::Rc;

use crate::hash::{HashMap, HashSet};
use crate::names::{NameTable, PathId};

use super::model::{ItemId, OpenOpacity};
use super::state::{AutoOpenTypeShadow, DottedOpacity, OpenGroup, ShorteningPrefix};
#[cfg(doc)]
use super::state::{DeclKinds, OpenInterpretation, Resolver, ScopeEntry};

/// The open-scoped walk state; see the [module docs](self).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpenScope {
    /// Opened namespace reading-groups in scope, **one [`OpenGroup`] per
    /// `open`**, in source order — F#'s implicit auto-opens first, then the
    /// file's explicit `open` decls (an `open` is in scope for the decls after
    /// it).
    ///
    /// Name resolution walks these through [`Resolver::open_reading_prefixes`]
    /// **latest-open-first** and, within an open, in the group's priority order:
    /// the latest `open` with a *complete* reading of the name wins (F# is
    /// latest-open-wins, not ambiguity — `open Demo; open Demo.Sub; Calc` is
    /// `Demo.Sub.Calc`, reversed it is `Demo.Calc`). A short path that does not
    /// resolve directly is retried under each reading (`open System` makes
    /// `Console.WriteLine` resolve as `System.Console.WriteLine`).
    pub(super) imports: Vec<OpenGroup>,
    /// Every prefix that can **shorten a later module open**, in source order
    /// (latest last): the implicit auto-opens, each explicit `open <namespace>`,
    /// and each resolved `open <project module>`. The single source-ordered
    /// sequence lets [`resolved_project_module`](Resolver::resolved_project_module)
    /// honour F#'s "latest open wins" *across open kinds* — `open A.Container;
    /// open B; open Shared` resolves `Shared` as `B.Shared` (the later namespace
    /// open) even though a module open (`A.Container`) preceded it, and `open
    /// Shared; open Sub` chains `Sub` to `Shared.Sub`. Kept separate from
    /// [`Self::imports`] (which holds only *namespace* prefixes, for
    /// assembly/qualified-path resolution) so adding module prefixes here does not
    /// pollute that. Block-scoped (reset per top-level block, saved/restored
    /// across nested modules).
    pub(super) open_shortening_prefixes: Vec<ShorteningPrefix>,
    /// Opened **assembly module** paths whose bare-name surface is *not provably
    /// complete* ([`AssemblyEnv::open_fold_surface`](crate::AssemblyEnv::open_fold_surface)'s residue — projection dropped
    /// a nested type, the pickle is unknowable, a member is undecodable, …).
    ///
    /// A later `open Sub` shortens through such a prefix (`Parent.Sub`), and the module
    /// it names may be exactly the one projection *dropped* — invisible to us, but bound
    /// by FCS at a **higher** priority than any root `Sub`. Resolving the root one's
    /// values would then be a wrong target, so an open that finds nothing under an
    /// incomplete prefix goes opaque instead (review round 9). Same block scoping as
    /// [`Self::open_shortening_prefixes`].
    pub(super) incomplete_open_prefixes: Vec<Vec<String>>,
    /// Every in-scope **explicit namespace `open`** (project or assembly — the
    /// kinds that do *not* set an opaque/unmodelled flag and so reach the same-file
    /// module-qualified classifier), as `(source position, canonical opened
    /// prefix)`. FCS-probed (r16): the dotted-head environment is one
    /// source-position-ordered latest-wins list over lexical module declarations
    /// and opens — an open declared *after* a same-file `module Pal` outranks it
    /// for the head of `Pal.Color.Red` (residual backtracking included), one
    /// declared *before* it loses. The classifier compares each lexical candidate
    /// ([`DeclKinds::module_pos`]) against these: a later open whose prefix could
    /// supply a module/namespace/assembly entity named like the head *contests*
    /// the candidate, and the classifier declines (the open-aware cross-file
    /// branches own resolution then). Implicit auto-opens precede every same-file
    /// declaration and so can never contest — they are not recorded. Block-scoped
    /// (reset per top-level block, saved/restored across nested modules) like
    /// [`Self::imports`].
    pub(super) explicit_open_prefixes: Vec<OpenedPrefix>,
    /// Every in-scope **project module `open`** (the [`OpenInterpretation::Module`]
    /// arm), as `(source position, canonical module path)`. A module open
    /// imports the module's TYPES into FCS's unqualified slot too (probe
    /// M20v, codex round 8 — the opened `M.Color` class evicts an earlier
    /// value exactly like a namespace-open-supplied type), so
    /// [`head_value_slot`](Resolver::head_value_slot) consults these
    /// alongside [`Self::explicit_open_prefixes`]. Same block-scoped
    /// lifecycle (reset per top-level block, saved/restored across nested
    /// modules).
    pub(super) module_open_prefixes: Vec<OpenedPrefix>,
    /// Every open through which an **assembly namespace's types are reachable**,
    /// as `(source position, canonical namespace path)` — every
    /// [`OpenInterpretation::Reading`] the open produced, captured *before* the
    /// `project_readings_only` filter that keeps a direct project-module /
    /// assembly-namespace merge out of [`Self::explicit_open_prefixes`]. This is
    /// exactly where a referenced-assembly type occupies FCS's unqualified slot
    /// and can evict a same-named local value: a plain `open System` (A1), a
    /// project-namespace open, **and** a *direct* `open Demo` where `Demo` is a
    /// project module merged with the assembly namespace `Demo` (codex round 2 —
    /// FCS binds `Demo.Calc` there). A module *alias* (`open Alias`) produces no
    /// reading — `open_interpretations` marks its namespaces unreachable — so it
    /// is correctly absent (codex round 1). Consulted by
    /// [`head_value_slot`](Resolver::head_value_slot) for the assembly-eviction
    /// check only (`docs/head-slot-assembly-eviction-plan.md`); the *project*
    /// slot checks keep using `explicit`/`module` prefixes. Same block-scoped
    /// lifecycle as [`Self::explicit_open_prefixes`].
    pub(super) assembly_open_prefixes: Vec<(u32, Vec<String>)>,
    /// Monotonic generation, bumped each time an `open M` of a project module with
    /// **unmodelled value-namespace members** is processed. Each opened
    /// [`ScopeEntry`] is tagged with the generation at its creation
    /// ([`ScopeEntry::generation`]); [`lookup`](Resolver::lookup) treats an opened
    /// entry whose generation is older than this as *shadowed* by that later open
    /// — we cannot enumerate the new open's union cases / exception constructors /
    /// active patterns, so any of them might shadow an earlier opened name, and
    /// conservatively dropping every earlier opened name (F#: the latest open
    /// wins) is sound. A *fully-modelled* open (only `let`s) does not bump it, so
    /// earlier opens stay resolvable. Block-scoped (reset per top-level block,
    /// saved/restored across nested modules) like `imports`.
    pub(super) open_generation: usize,
    /// The start offset of the latest `open` (of any kind) in scope in this
    /// top-level block: an open is scoped to its module, so leaving a nested
    /// module restores it. The positional contest of the attribute and type
    /// resolutions: F# is latest-wins across bindings and opens alike, so an
    /// in-file type commit whose definition *precedes* an open must defer —
    /// the open could supply the name at higher priority (`type
    /// ObsoleteAttribute … ; open System; [<Obsolete>]` binds
    /// `System.ObsoleteAttribute` in FCS). Reset per top-level block with the
    /// rest of the open state.
    pub(super) latest_open_pos: u32,
    /// Project-global [`ItemId`]s of opened cross-file cases that must **not** be
    /// trusted in pattern position — exported cases of a *hidden* module (one whose
    /// `open` also brings unenumerable constructors, e.g. an active pattern, that
    /// could shadow them; see [`open_module_values`](Resolver::open_module_values)).
    /// They still resolve as values in expression position; only
    /// [`case_reference`](Resolver::case_reference) consults this to defer the pattern
    /// use. Block-scoped (reset per top-level block, saved/restored across nested
    /// modules) like [`Self::open_generation`].
    pub(super) pattern_suppressed_case_ids: HashSet<ItemId>,
    /// What the opens in scope leave unknowable ([`OpenOpacityState`]).
    pub(super) opacity: OpenOpacityState,
    /// Type names a **same-file** `[<AutoOpen>]` module has opened into the
    /// current scope. F# opens an auto-open nested module into the remainder
    /// of its CONTAINER's scope — including a *module* container or the
    /// anonymous root, which the namespace-keyed cross-file signal
    /// ([`Resolver::project_auto_open_module_in_namespace`](super::Resolver))
    /// can never match (a module container path is not a walked prefix). The
    /// file's own types are fully modelled, so this signal is name-KEYED:
    /// populated at each auto-open nested module's exit with its direct type
    /// names, kept (not restored) so it covers the container's remaining
    /// scope, and propagated outward when the auto-open module's *own*
    /// container is itself auto-open — the FCS-recursive chain. A bare
    /// type-position use of one of these names defers as shadowable; the
    /// upgrade to resolving the actual binder is a later slice. The value
    /// carries the IMPORT position (the auto-open module declaration's
    /// start — F#'s in-scope introductions contest positionally, so a
    /// same-container `type` declared AFTER the import outranks it and must
    /// keep resolving exactly) and the name's **minimum visible depth**: a
    /// name contributed through a `module private` auto-open module is
    /// visible no shallower than that module's container, so exit-time
    /// propagation drops it once the walk leaves that container
    /// (`type private` members never enter the set at all). The depth is
    /// consulted only at exits — an entry live in a scope's set is visible
    /// to everything at or below that scope.
    pub(super) auto_open_type_shadow_names: HashMap<String, AutoOpenTypeShadow>,
    /// While [`Self::recursive_module_active`]: every nested-module name
    /// declared anywhere in the `rec` block (pre-scanned on entry, all
    /// nesting depths — a superset of what is in scope at any one point,
    /// which can only over-defer, never mis-bind). A multi-segment
    /// type-position path whose head is one of these may descend into a
    /// module the source-ordered walk has not reached yet — the
    /// forward-declared counterpart of
    /// [`type_path_descends_into_nested_module`](super::Resolver), which only
    /// sees modules already walked. Empty outside `rec` blocks.
    pub(super) rec_module_names: Rc<HashSet<String>>,
    /// The **access-floor** of the current walk position: the access-root
    /// length imposed by the deepest enclosing `module private …`, or `None` if
    /// no `private` module encloses here. A `module private M` (M at `[…P, M]`)
    /// makes its contents accessible only from within its *parent* `P`, so it
    /// contributes floor `Some(P.len())`; stacked private modules take the
    /// deepest (largest). A value/case declared here inherits this floor unless
    /// its own (or its type's) `private` modifier narrows it further — see
    /// [`Resolver::export_access_root_len`], which combines the two into the export's
    /// [`ExportedItem::access_root_len`](super::model::ExportedItem). This is what
    /// makes the collapse recovery sound *and* correct: a private-module value is
    /// filtered from an outside `open` (never committed), while a sibling within
    /// the enclosing namespace still resolves it (oracle-pinned D2/D6). Set at the
    /// file root and saved/restored as [`nested_module`](Resolver::nested_module)
    /// descends. An absolute segment count (a prefix of every descendant
    /// `container_path`), so it stays valid as the walk goes deeper.
    pub(super) access_floor: Option<usize>,
    /// Whether the container being walked is a module whose path qualifies
    /// its exports: a `module` header, or a nested module under a real root.
    /// That path is then [`Resolver::container_path`], read through
    /// [`module_path`](Resolver::module_path); a flag rather than a copy, since a
    /// copy per nested module is quadratic in the nesting depth.
    pub(super) in_exporting_module: bool,
    /// `true` while walking a `module rec` / `namespace rec` block. Later type
    /// declarations are in scope for earlier type annotations there, so a bare
    /// single-segment type name must defer unless already resolved in-file.
    pub(super) recursive_module_active: bool,
}

/// An `open`'s prefix in [`OpenScope::explicit_open_prefixes`] or
/// [`OpenScope::module_open_prefixes`]: where the `open` is written, and the
/// prefix, with its id in the env's name table so that a probe for a name
/// under it is a child lookup rather than a path built and hashed.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpenedPrefix {
    pub(super) pos: u32,
    pub(super) id: PathId,
    pub(super) path: Vec<String>,
}

impl OpenedPrefix {
    /// The prefix `path` of an `open` written at `pos`, interned in `names`.
    pub(super) fn new(names: &NameTable, pos: u32, path: Vec<String>) -> Self {
        OpenedPrefix {
            pos,
            id: names.intern_path(&path),
            path,
        }
    }
}

impl OpenScope {
    /// The scope at the start of a top-level block: only the implicit
    /// auto-opens are in scope (`imports` and `shortening`), no explicit open
    /// has raised any opacity, and the block's header supplies the rest.
    /// Imports are scoped to a *single* top-level block: an `open` in one
    /// `namespace`/`module` block does not carry to the next (FCS-verified,
    /// even for two same-named `namespace N` blocks).
    pub(super) fn block_start(
        imports: Vec<OpenGroup>,
        shortening: Vec<ShorteningPrefix>,
        header: BlockHeader,
    ) -> OpenScope {
        OpenScope {
            imports,
            open_shortening_prefixes: shortening,
            incomplete_open_prefixes: Vec::new(),
            explicit_open_prefixes: Vec::new(),
            module_open_prefixes: Vec::new(),
            assembly_open_prefixes: Vec::new(),
            open_generation: 0,
            latest_open_pos: 0,
            pattern_suppressed_case_ids: HashSet::default(),
            opacity: OpenOpacityState::default(),
            auto_open_type_shadow_names: header.auto_open_type_shadow_names,
            rec_module_names: Rc::new(header.rec_module_names),
            access_floor: header.access_floor,
            in_exporting_module: header.in_exporting_module,
            recursive_module_active: header.recursive,
        }
    }

    /// The scope after a nested module whose body ended in `inner`, `self`
    /// being the scope at the module's entry. `inner` is left with nothing
    /// worth reading.
    ///
    /// Every field comes back to its value at entry except
    /// [`Self::auto_open_type_shadow_names`]: an `[<AutoOpen>]` module opens
    /// into the rest of its container's scope, so what its body grew is merged
    /// outward ([`ModuleExit::AutoOpen`]). The field list is checked in debug
    /// builds: every other field must equal its value at entry.
    pub(super) fn leave(mut self, inner: &mut OpenScope, exit: ModuleExit) -> OpenScope {
        #[cfg(debug_assertions)]
        let entry = self.clone();
        let auto_open_type_shadow_names = match exit {
            ModuleExit::Plain => std::mem::take(&mut self.auto_open_type_shadow_names),
            ModuleExit::AutoOpen(auto_open) => auto_open.merge(
                std::mem::take(&mut self.auto_open_type_shadow_names),
                std::mem::take(&mut inner.auto_open_type_shadow_names),
            ),
        };
        let left = OpenScope {
            auto_open_type_shadow_names,
            ..self
        };
        #[cfg(debug_assertions)]
        {
            let mut unnamed = left.clone();
            unnamed
                .auto_open_type_shadow_names
                .clone_from(&entry.auto_open_type_shadow_names);
            assert_eq!(
                unnamed, entry,
                "a field `OpenScope::leave` does not name came back changed"
            );
        }
        left
    }
}

/// What a top-level block's header decides about its [`OpenScope`].
pub(super) struct BlockHeader {
    /// The same-file `[<AutoOpen>]` type shadows earlier blocks of the same
    /// namespace left behind, each entering at this block's start.
    pub(super) auto_open_type_shadow_names: HashMap<String, AutoOpenTypeShadow>,
    /// A `rec` block's nested-module names, pre-scanned; empty otherwise.
    pub(super) rec_module_names: HashSet<String>,
    /// A `module private Foo` root's access floor (its parent's length).
    pub(super) access_floor: Option<usize>,
    /// Whether the header is a `module`, whose path qualifies its exports.
    pub(super) in_exporting_module: bool,
    /// Whether the block is `rec`.
    pub(super) recursive: bool,
}

/// How a nested module's scope folds into its container's when it ends
/// ([`OpenScope::leave`]).
pub(super) enum ModuleExit {
    /// A plain module: what it opened stays inside it.
    Plain,
    /// An `[<AutoOpen>]` module, which opens into the rest of its container.
    AutoOpen(AutoOpenExit),
}

/// An `[<AutoOpen>]` nested module's exit ([`ModuleExit::AutoOpen`]).
pub(super) struct AutoOpenExit {
    /// The container's depth (the module's parent's path length).
    pub(super) parent_depth: usize,
    /// Whether the module is `module private`.
    pub(super) private: bool,
    /// Where the module is declared: its own types enter the container here.
    pub(super) import_pos: u32,
    /// The module's own direct public type names.
    pub(super) own_types: Vec<String>,
}

impl AutoOpenExit {
    /// The container's auto-open type shadows (`outer`) once the module whose
    /// body ended with `grown` opens into it: the names grown inside the body
    /// (an auto-open descendant's names flow outward through exactly this
    /// branch — the FCS-recursive chain) plus this module's own direct
    /// **public** type names (`type private T` is visible within the module
    /// only — codex round 2). Accessibility is depth-bounded, not boolean: a
    /// name contributed through a `module private` auto-open module is visible
    /// no shallower than that module's container, so each entry carries its
    /// minimum visible depth, filtered here on every hop outward.
    fn merge(
        self,
        mut outer: HashMap<String, AutoOpenTypeShadow>,
        grown: HashMap<String, AutoOpenTypeShadow>,
    ) -> HashMap<String, AutoOpenTypeShadow> {
        for (name, entry) in grown {
            // Entries the parent scope already had pass through untouched;
            // this subtree's contributions are depth-filtered (invisible
            // at the parent → dropped) and — through a private module —
            // pinned to the parent's depth.
            let inherited = outer.get(&name) == Some(&entry);
            if inherited {
                continue;
            }
            if entry.min_depth > self.parent_depth {
                continue;
            }
            let min_depth = if self.private {
                entry.min_depth.max(self.parent_depth)
            } else {
                entry.min_depth
            };
            merge_auto_open_shadow(
                &mut outer,
                name,
                AutoOpenTypeShadow {
                    import_pos: entry.import_pos,
                    min_depth,
                },
            );
        }
        for name in self.own_types {
            merge_auto_open_shadow(
                &mut outer,
                name,
                AutoOpenTypeShadow {
                    import_pos: self.import_pos,
                    min_depth: if self.private { self.parent_depth } else { 0 },
                },
            );
        }
        outer
    }
}

/// Merge one auto-open type-shadow contribution into the scope's set.
/// Same-name contributions merge **monotonically soundly** (codex round 3: a
/// later `module private` sibling's depth-pinned entry must not evict an
/// earlier public one): the minimum visible depth keeps the name shadowing
/// wherever ANY contribution is visible, and the maximum import position
/// makes the positional contest against in-file types as conservative as the
/// latest introduction — either can only widen the defer, never mis-bind.
fn merge_auto_open_shadow(
    map: &mut HashMap<String, AutoOpenTypeShadow>,
    name: String,
    new: AutoOpenTypeShadow,
) {
    map.entry(name)
        .and_modify(|e| {
            e.min_depth = e.min_depth.min(new.min_depth);
            e.import_pos = e.import_pos.max(new.import_pos);
        })
        .or_insert(new);
}

/// What the opens in scope leave unknowable, one part per kind of name
/// resolution they can blind. Each part is only ever **raised** within a
/// scope (an open is in scope for every declaration after it, which the walk
/// visits in source order); leaving the scope is the only way down, and that
/// is [`OpenScope::leave`]'s doing, not this type's.
///
/// The three parts are independent: no part implies another. In particular
/// `value` does **not** imply `dotted`, because the
/// [`Resolver::open_imports_project_values`] fallback raises `value` alone, so
/// every read site that must defer under either asks
/// [`Self::bare_or_dotted_heads_unknowable`].
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct OpenOpacityState {
    /// `true` once an open that *could* bring an unqualified **value** we do not
    /// model is in scope: a plain `open <assembly module/class>` (its values /
    /// statics are unmodelled here), or an `open type T` whose target we could not
    /// resolve to an assembly handle or that is project-shadowed. Such an open
    /// might provide — or shadow — a bare name with a value we cannot enumerate,
    /// so while one is in scope [`lookup`](Resolver::lookup) skips every *opened*
    /// ([`ScopeEntry::from_open`]) entry — resolving a modelled opened name could
    /// otherwise pick a target the opaque open shadows (correctness over
    /// availability). An unknown bare value head could likewise be a
    /// dotted-path head the walk cannot resolve, so the dotted-path gate in
    /// [`resolve_long_ident`](Resolver::resolve_long_ident) asks
    /// [`Self::bare_or_dotted_heads_unknowable`], which reads this part as well
    /// as [`Self::dotted`]: this part does not raise that one. A plain `open
    /// <namespace>` brings only *types*, no unqualified
    /// values, so it does **not** set this.
    value: bool,
    /// Which opens in scope could supply a *dotted-path head* we do not model
    /// (see [`DottedOpacity`]). While [`DottedOpacity::any`] holds, the
    /// dotted-path gate in [`resolve_long_ident`](Resolver::resolve_long_ident)
    /// leaves such a head unresolved rather than risk routing it to a
    /// colliding cross-file / referenced-assembly path (correctness over
    /// availability). Unlike `opaque_value_open` it does **not** make
    /// [`lookup`](Resolver::lookup) skip opened entries — an opened module's bare
    /// values are modelled and must resolve. Save/restored across nested
    /// modules.
    dotted: DottedOpacity,
    /// `true` once an open that brings *type members* into unqualified scope is
    /// in scope — `open type T`, or `open <module>`/`open <type>` whose path is
    /// itself an assembly type. Its **static members** are modelled (one *opened*
    /// [`ScopeEntry`] per static name was pushed into the frame by
    /// [`Resolver::open_type_statics`]), but its **nested types** are not — so a
    /// *qualified* path could still come from it, and while one is in scope
    /// `resolve_long_ident` cannot soundly resolve a qualified path via the
    /// *other* (namespace) opens and defers instead. Also set for an `open type`
    /// whose target we could not resolve to a handle (an in-project type, an
    /// exotic type form): nothing is modelled then, so every open-based resolution
    /// stays conservative. Monotonic: an open is in scope for the decls after it,
    /// which we visit in source order.
    unmodelled: bool,
}

impl OpenOpacityState {
    /// Raise [`Self::value`].
    pub(super) fn raise_value(&mut self) {
        self.value = true;
    }

    /// Raise [`Self::dotted`] beyond enumerable project modules.
    pub(super) fn raise_dotted(&mut self) {
        self.dotted.set_opaque();
    }

    /// Add an enumerable project module to [`Self::dotted`].
    pub(super) fn add_dotted_project_module(&mut self, module: Vec<String>) {
        self.dotted.add_project_module(module);
    }

    /// Raise [`Self::unmodelled`].
    pub(super) fn raise_unmodelled(&mut self) {
        self.unmodelled = true;
    }

    /// Whether bare-name lookup must skip every *opened* scope entry: an open
    /// in scope may shadow it with a value the walk cannot enumerate.
    pub(super) fn skips_opened_values(&self) -> bool {
        self.value
    }

    /// Whether an open in scope may supply a *qualified* path's head from
    /// members the walk does not model (`open type`, an opened assembly type).
    pub(super) fn qualified_paths_unmodelled(&self) -> bool {
        self.unmodelled
    }

    /// The opens in scope that could supply a dotted-path head.
    pub(super) fn dotted(&self) -> &DottedOpacity {
        &self.dotted
    }

    /// Whether a bare value or a dotted-path head may come from an open the
    /// walk does not model.
    pub(super) fn bare_or_dotted_heads_unknowable(&self) -> bool {
        self.value || self.dotted.any()
    }

    /// Whether any open in scope leaves anything unknowable.
    pub(super) fn any(&self) -> bool {
        self.value || self.dotted.any() || self.unmodelled
    }

    /// Whether any open in scope leaves something unknowable beyond the
    /// enumerable project modules [`DottedOpacity::project_modules`] lists.
    pub(super) fn any_beyond_enumerable_project_modules(&self) -> bool {
        self.value || self.dotted.beyond_project_modules() || self.unmodelled
    }

    /// The parts raised, each as one bit, for a trace to compare across an
    /// open ([`OpacityLevels::raised_since`]).
    pub(super) fn levels(&self) -> OpacityLevels {
        OpacityLevels {
            value: self.value,
            dotted: self.dotted.any(),
            unmodelled: self.unmodelled,
        }
    }
}

/// [`OpenOpacityState`] with each part reduced to whether it is raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OpacityLevels {
    value: bool,
    dotted: bool,
    unmodelled: bool,
}

impl OpacityLevels {
    /// The opacity parts raised between `before` and `self`, as the
    /// resolution-explain trace's per-open record. Every other field of the
    /// returned [`OpenOpacity`] is `false`, for the caller to fill.
    ///
    /// Each part only rises within a scope, so `now && !before` is exactly
    /// what was raised in between.
    pub(super) fn raised_since(self, before: OpacityLevels) -> OpenOpacity {
        OpenOpacity {
            opaque_value: self.value && !before.value,
            opaque_dotted: self.dotted && !before.dotted,
            unmodelled: self.unmodelled && !before.unmodelled,
            ..OpenOpacity::default()
        }
    }
}

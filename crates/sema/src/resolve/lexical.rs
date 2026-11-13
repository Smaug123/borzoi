//! Where the walk is ([`Lexical`]): the value and type-parameter scope
//! stacks, the container the walk is in and the top-level block it belongs
//! to, and the frames of the containers it is not in, parked until a block
//! of theirs takes them again. Pushed, popped and replaced as the walk
//! enters and leaves declarations, unlike the [`Indexes`](super::indexes::Indexes),
//! which keep what they record for the rest of the file.

use crate::hash::{HashMap, HashSet};

use super::container::ContainerPath;
use super::path_set::PathSet;
use super::state::{Frame, RecordedFold};
use crate::def::DefId;
use crate::names::NameTable;

/// Where the walk is; see the [module docs](self).
pub(super) struct Lexical<'a> {
    pub(super) scopes: Vec<Frame>,
    /// The **type-parameter** scope: a stack of frames, one per generic
    /// definition currently open (a `type` header, a generic `let`/function, a
    /// generic `member`). Each frame maps a typar's `idText` name (the bare `T`
    /// of `'T`/`^T`) to its [`DefKind::TypeParam`](crate::DefKind::TypeParam) binder. A `'T` *use* in a type
    /// position ([`resolve_type`](crate::resolve::Resolver::resolve_type)'s `Type::Var` arm) or a
    /// `'T.Member` expression looks the name up here, innermost frame first, so a
    /// member's own `<'T>` shadows an enclosing type's.
    ///
    /// Separate from both the value [`scopes`](Self::scopes) stack (typars are in
    /// F#'s disjoint *type* namespace, so a value lookup must never find one) and
    /// the container-keyed [`type_defs`](crate::resolve::indexes::Indexes::type_defs) (typars are
    /// definition-scoped and ephemeral, not visible container-wide). Empty
    /// whenever no generic definition is open.
    pub(super) typar_scopes: Vec<Vec<(String, DefId)>>,
    /// The **value scope of each top-level container** — a `namespace`/`module`
    /// header's path (`[]` for an anonymous module) — keyed like
    /// [`Indexes::type_defs`](crate::resolve::indexes::Indexes::type_defs). F# merges same-named `namespace N` blocks and isolates
    /// distinct ones; modelling that needs *per-container* value frames rather
    /// than one shared base frame, so [`resolve_file`](super::resolve_file) activates a container's
    /// frame from here (pushing it on [`Self::scopes`]) for the span of each
    /// top-level block and stores the accumulated, position-ordered frame back
    /// after. A later same-named block re-takes it; a distinct namespace gets a
    /// fresh one; a nested module pushes its own frame on top (seeing the
    /// enclosing container but not leaking into it).
    ///
    /// Union *cases* are value-namespace entries, so they live in these frames
    /// (added at their source position), not a side index — which is why
    /// [`lookup`](crate::resolve::Resolver::lookup) gives correct source-order shadowing between a
    /// value and a same-named case, and why a nested module sees its enclosing
    /// namespace's cases. (`[<RequireQualifiedAccess>]` cases are simply not added;
    /// cross-file cases stay deferred, as for `type_defs`.)
    pub(super) top_level: HashMap<Vec<String>, Frame>,
    /// Dotted path of the enclosing *container* — the `module`/`namespace`
    /// header's `longId` (`["Demo"]` for `namespace Demo` as well as for
    /// `module Demo`; empty for an anonymous module or `namespace global`).
    /// Distinct from [`Resolver::module_path`](crate::resolve::Resolver::module_path), which is `None` for a namespace
    /// because a namespace binds no *values*; nested modules, however, *are*
    /// qualified under a namespace, so this is the prefix used to export a
    /// nested module's cross-file shadow path.
    pub(super) container_path: ContainerPath<'a>,
    /// How many leading segments of [`Self::container_path`] are the enclosing
    /// **namespace** (the rest are nested-module names). For `namespace
    /// Outer.Inner` it is 2; for a top-level `module Outer.Client` it is 1 (the
    /// dotted prefix `Outer`, the module name `Client` excluded); it is unchanged
    /// as the walk descends into nested modules. Implicit *relative namespace*
    /// resolution ([`open_interpretations`](crate::resolve::Resolver::open_interpretations))
    /// probes only these enclosing-namespace prefixes — a module segment is **not**
    /// a namespace container, so `open Inner` inside `module Outer.Client` must not
    /// reach `Outer.Client.Inner` (FCS leaves it undefined).
    pub(super) namespace_depth: usize,
    /// How many entries of [`Output::namespace_paths`](crate::resolve::output::Output::namespace_paths) earlier top-level blocks
    /// recorded: the namespaces FCS has in its environment at a use in the
    /// current block, which adds a namespace only once the block declaring it
    /// closes.
    pub(super) block_namespace_start: usize,
    /// The length of the current top-level block's header path (`namespace
    /// A.B` or `module A.M`; zero for an anonymous module or `namespace
    /// global`): the prefix of [`Self::container_path`] under which this
    /// block's own declarations land in [`Indexes::container_decls`](crate::resolve::indexes::Indexes::container_decls).
    pub(super) block_container_len: usize,
    /// Local names (`SynComponentInfo.longId`) of nested `module X = …` decls
    /// (parser 8.4), for shadowing **same-file** references written *relative* to
    /// the enclosing module (`Calc.Answer`). Sema does not yet model
    /// nested-module *scopes* (a separate slice), so it cannot enumerate the
    /// members a nested module provides — but a reference rooted at one must
    /// **not** fall through to a colliding referenced-assembly member (the
    /// `assembly_path_records` soundness tripwire). We therefore defer any such
    /// reference. This over-defers (an unrelated assembly `Calc.*` reference also
    /// defers — an availability cost, never a wrong resolution) and is the
    /// conservative stand-in until nested-module resolution lands.
    ///
    /// Holds the **active top-level block's** names only: a *relative* reference
    /// can reach a nested module of the current container, but not one under a
    /// *distinct* sibling block (`namespace A`'s `Sub` must not veto `Sub.Calc`
    /// in `namespace B` — FCS resolves the opened assembly there), while a
    /// same-named block still sees it (`namespace A` again: `Sub.Calc.Zero` is
    /// the project value, so the shadow must survive — FCS). So
    /// [`resolve_file`](super::resolve_file) activates the set per block from
    /// [`Self::top_level_nested_locals`], exactly as [`Self::top_level`] does
    /// for value frames. (Qualified cross-block references are covered by
    /// [`Indexes::nested_module_exports`](crate::resolve::indexes::Indexes::nested_module_exports), which stays file-wide.)
    pub(super) nested_module_locals: PathSet,
    /// The per-container store behind [`Self::nested_module_locals`], keyed and
    /// managed like [`Self::top_level`]: each top-level block re-takes its
    /// container's accumulated set (fresh for a first/distinct container) and
    /// stores it back after.
    pub(super) top_level_nested_locals: HashMap<Vec<String>, PathSet>,
    /// The paths a same-file **augmentation head** (`type Demo.Calc with …`)
    /// names — as written ([`Self::augmentation_head_locals`]) and
    /// `container_path`-qualified ([`Indexes::augmentation_head_exports`](crate::resolve::indexes::Indexes::augmentation_head_exports)),
    /// mirroring the two forms [`Resolver::record_project_name_shadow`](crate::resolve::Resolver::record_project_name_shadow) records
    /// and following the same block lifecycle
    /// ([`Self::top_level_augmentation_locals`]).
    ///
    /// Held apart from the nested-module shadow because an augmentation head
    /// occupies **only the value namespace**. It names an *existing* type, so
    /// it introduces no project type and must not gate a type path
    /// ([`Resolver::project_type_shadow_cause`](crate::resolve::Resolver::project_type_shadow_cause) does not read these) — that is
    /// the whole of "a file that augments `T` loses every later `(v : T)`". But
    /// its members really do join the augmented type's method groups, and an
    /// extension overload is selectable there (fsi: `type Demo.Calc with static
    /// member Zero (x: int) = x + 1000` makes `Demo.Calc.Zero 5` bind the
    /// *extension*, while the same-named `static member Answer = 99` loses
    /// outright to the intrinsic), so a **value** path rooted at one must still
    /// defer: [`Resolver::project_shadow_cause`](crate::resolve::Resolver::project_shadow_cause) reads them.
    ///
    /// Nothing else does. Every other consumer of the nested-module sets asks
    /// "is there a project *module* here?", which an augmentation head never
    /// answers.
    pub(super) augmentation_head_locals: PathSet,
    /// The per-container store behind [`Self::augmentation_head_locals`],
    /// managed like [`Self::top_level_nested_locals`].
    pub(super) top_level_augmentation_locals: HashMap<Vec<String>, PathSet>,
    /// The project half of this block's implicit open of its own enclosing
    /// namespace ([`Resolver::open_own_enclosing_namespace`](crate::resolve::Resolver::open_own_enclosing_namespace)): the namespace's
    /// fragments from earlier files and earlier blocks of this file, as the
    /// fold wrote them at the block's start. An explicit `open` of the same
    /// namespace in the block writes them again at its own position
    /// ([`Resolver::replay_own_namespace_fold`](crate::resolve::Resolver::replay_own_namespace_fold)): FCS's `open` re-adds the
    /// fragments the environment has registered, and the current block's own
    /// fragment is registered only once the block ends. Block-scoped: set at
    /// each block's start.
    pub(super) own_namespace_fold: RecordedFold,
    /// The in-file types the current top-level block declares. Reset per
    /// block: a namespace reopened in a later block, `namespace global`
    /// included, makes an augmentation there an extension to FCS.
    pub(super) block_type_defs: HashSet<DefId>,
    /// The index in [`Self::scopes`] of each module or namespace frame on the
    /// stack, innermost last: every frame above the innermost holds
    /// expression-local bindings.
    pub(super) module_frames: Vec<usize>,
    /// `true` while the file's top-level module is *anonymous* (the implicit
    /// filename module — `ModuleOrNamespaceKind::Anon`, no `namespace`/`module`
    /// header). A nested module under it is reachable cross-file only via the
    /// filename-qualified path (`<FileName>.Calc.x`), which sema does not model,
    /// so [`Resolver::nested_module`](crate::resolve::Resolver::nested_module) suppresses its cross-file export. This is
    /// distinct from "empty container path": `namespace global` also has an empty
    /// path but is a *real* (global) namespace, so its nested modules *are*
    /// bare-cross-file referenceable (`Calc.x`) and must be exported. Set per
    /// top-level module in [`resolve_file`](super::resolve_file).
    pub(super) anonymous_root: bool,
}

impl<'a> Lexical<'a> {
    /// The position before the walk enters the file's first block.
    pub(super) fn new(names: &'a NameTable) -> Self {
        Lexical {
            scopes: Vec::new(),
            typar_scopes: Vec::new(),
            top_level: HashMap::default(),
            container_path: ContainerPath::new(names),
            namespace_depth: 0,
            block_namespace_start: 0,
            block_container_len: 0,
            nested_module_locals: PathSet::default(),
            top_level_nested_locals: HashMap::default(),
            augmentation_head_locals: PathSet::default(),
            top_level_augmentation_locals: HashMap::default(),
            own_namespace_fold: RecordedFold::default(),
            block_type_defs: HashSet::default(),
            module_frames: Vec::new(),
            anonymous_root: false,
        }
    }

    /// The frame top-level bindings of the *current* module are added to. At
    /// decl-processing time the scope stack is exactly the chain of enclosing
    /// module frames (every transient frame — lambda params, match clauses,
    /// let-RHS params — is pushed and popped within a single expression
    /// resolution), so the innermost is the current module's frame: the base
    /// frame for a top-level module, or the frame [`Resolver::nested_module`](super::Resolver::nested_module) pushed
    /// for a `module M = …` body. Adding to the innermost (not the base) keeps a
    /// nested module's bindings out of the enclosing scope.
    pub(super) fn module_frame(&mut self) -> &mut Frame {
        self.scopes
            .last_mut()
            .expect("module frame pushed in resolve_file")
    }

    /// Write [`Self::own_namespace_fold`] into the module frame again, as an
    /// `open` at `open_pos` whose first entries take `generation`; the number
    /// of generation barriers the fold raises.
    pub(super) fn replay_own_namespace_fold(&mut self, generation: usize, open_pos: u32) -> usize {
        let frame = self
            .scopes
            .last_mut()
            .expect("module frame pushed in resolve_file");
        frame.extend(
            self.own_namespace_fold
                .entries
                .iter()
                .map(|(barriers_before, entry)| {
                    let mut entry = entry.clone();
                    entry.generation = generation + barriers_before;
                    entry.open_pos = open_pos;
                    entry
                }),
        );
        self.own_namespace_fold.barriers
    }
}

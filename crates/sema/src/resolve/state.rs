//! Resolver state: the mutable walk context ([`Resolver`]) and its supporting
//! scope types ([`ScopeEntry`], [`Frame`], [`AssemblyPath`], [`OpenInterpretation`]),
//! plus the implicit auto-open seed ([`implicit_open_namespaces`]). The
//! behaviour — the `impl Resolver` methods that drive the walk — lives in the
//! parent [`resolve`](super) module; the fields here are `pub(super)` so those
//! impl blocks (anywhere in the `resolve` module subtree) can reach them.

use std::collections::{HashMap, HashSet};

use rowan::TextRange;

use crate::assembly_env::{AssemblyEnv, EntityHandle};
use crate::def::{Def, DefId};
use crate::diagnostics::SemaDiagnostic;

use super::model::{
    ExportDecl, ExportedItem, ItemId, OpenTrace, ProjectItems, Resolution, SlotClass,
};

/// A binding visible in a scope frame. `name` is `idText`-normalised; later
/// entries in a frame shadow earlier ones (position-ordered shadowing, D4).
#[derive(Debug, Clone)]
pub(super) struct ScopeEntry {
    pub(super) name: String,
    pub(super) resolution: Resolution,
    /// `true` if an `open` brought this name into scope — an opened type's static
    /// member (`open type T`), or (a later slice) an opened module's value
    /// (`open M`) — rather than a source binding (`let`, a union/exception case, a
    /// parameter). Two consequences, both keeping opens sound:
    /// - while an [`opaque_value_open`](Resolver::opaque_value_open) is in scope,
    ///   [`lookup`](Resolver::lookup) skips it — an open whose contents we cannot
    ///   enumerate might shadow it with a value we do not model, so resolving the
    ///   modelled opened name would risk a wrong target (correctness over
    ///   availability);
    /// - it is dropped when a top-level container frame is stored back
    ///   ([`resolve_file`](super::resolve_file)), so an `open` in one block does not leak into a later
    ///   same-named block (F#: opens are scoped to one block; values merge).
    pub(super) from_open: bool,
    /// The [`open_generation`](Resolver::open_generation) when this entry was
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
    /// value-shadowed cross-file case brought in by [`open_module_values`] so a
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
    /// their [`DefKind`] (an opened assembly case has no def to classify).
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
}

impl ScopeEntry {
    /// A name introduced by a *source binding* — a `let` value/parameter, a
    /// union/exception case, an active-pattern case, a `match`/lambda local —
    /// stamped with the current [`open_generation`](Resolver::open_generation)
    /// so a later residue-bearing `open` can shadow it (see
    /// [`Self::generation`]).
    pub(super) fn binding(name: String, resolution: Resolution, generation: usize) -> Self {
        ScopeEntry {
            name,
            resolution,
            from_open: false,
            generation,
            pattern_only: false,
            open_pos: 0,
            opened_case: false,
            opened_ap_shape: None,
            maybe_constant_pattern: false,
        }
    }

    /// A name brought into scope by an `open` (see [`Self::from_open`]), tagged
    /// with the current [`open_generation`](Resolver::open_generation) so a later
    /// unmodelled open can shadow it (see [`Self::generation`]), and with the
    /// open's source offset (see [`Self::open_pos`]).
    pub(super) fn opened(
        name: String,
        resolution: Resolution,
        generation: usize,
        open_pos: u32,
    ) -> Self {
        ScopeEntry {
            name,
            resolution,
            from_open: true,
            generation,
            pattern_only: false,
            open_pos,
            opened_case: false,
            opened_ap_shape: None,
            maybe_constant_pattern: false,
        }
    }

    /// A constructor-namespace-only opened entry — a value-shadowed cross-file
    /// case (see [`Self::pattern_only`]).
    pub(super) fn opened_pattern_only(
        name: String,
        resolution: Resolution,
        generation: usize,
        open_pos: u32,
    ) -> Self {
        ScopeEntry {
            name,
            resolution,
            from_open: true,
            generation,
            pattern_only: true,
            open_pos,
            opened_case: false,
            opened_ap_shape: None,
            maybe_constant_pattern: false,
        }
    }
}

/// One lexical scope: a flat, source-ordered list of its own bindings. The
/// parent-linked scope *tree* of D4 is realised implicitly as the live stack of
/// frames during the walk — at any use, the frame stack is exactly that use's
/// root-to-node path through the tree.
#[derive(Debug, Default)]
pub(super) struct Frame {
    pub(super) entries: Vec<ScopeEntry>,
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
    /// A project value (member access on it), an *exact* project module path,
    /// or the current module shadows the path; F# resolves it in-project, so
    /// the assembly index must not be consulted. A project module that is only a
    /// *proper* prefix does **not** land here — it merges with the assembly
    /// namespace and falls through (see [`Resolver::assembly_path_records`]).
    ProjectShadowed,
    /// This reading lands on a **generic type-abbreviation child** of a module
    /// (missed by the arity-0 `nested` walk): the name binds in the module, but
    /// the abbreviation target is unmodelled and FCS's ownership is
    /// target-sensitive (a record/union target falls through, a class target
    /// keeps the module), so this reading can neither resolve nor confidently
    /// disown the path — it **defers**.
    ///
    /// Unlike [`Self::ProjectShadowed`] it is **tier-local**: it does *not* trip
    /// the preemptive as-written-root veto in
    /// [`Resolver::resolve_assembly_path_tiered`], because it is a lower-priority
    /// *assembly* reading, not a lexical project-bound head — a higher-priority
    /// `open` that resolves the whole path must still win over it (codex review
    /// 4). Reached in priority order, it defers like `ProjectShadowed`.
    AbbreviationOpaque,
    /// Not an assembly path at all.
    NoMatch,
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

/// The per-prefix shadow verdict a caller of
/// [`Resolver::resolve_assembly_path_tiered`](super::Resolver) supplies for
/// each tier the walk visits — a named strength instead of two positionally
/// swappable boolean closures (wrong wiring is now a type error, and the
/// disabled case reads as what it is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShadowVeto {
    /// Nothing at this prefix can shadow the name.
    None,
    /// A coarse, name-blind risk (a project auto-open module whose nested
    /// types sema does not enumerate; a namespace whose assembly's
    /// abbreviations are unknowable): consulted only once the tier's own
    /// lookup is a genuine no-match — pre-emptive checking would needlessly
    /// defer every other real type under the same reading.
    OnNoMatch,
    /// An exact, name-keyed shadow (an in-scope assembly `[<AutoOpen>]`
    /// module with an accessible member of exactly this name): vetoes even a
    /// same-tier real match — FCS-probed, an auto-open module's contents
    /// outrank the same namespace's own direct members.
    Preemptive,
}

/// One [`Resolver::auto_open_type_shadow_names`] entry — see the field docs.
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
    Resolved(R),
    /// Some reading at winning priority is project-shadowed: a project entity
    /// owns the name there and may satisfy the whole path invisibly (sema does
    /// not model project types / nested-module members), so no assembly reading
    /// — complete or partial — may be applied. Defer.
    ShadowDeferred,
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
#[derive(Clone, Debug)]
pub(super) struct OpenGroup {
    /// The readings, highest-priority first.
    pub(super) readings: Vec<Vec<String>>,
}

/// One interpretation of an `open <path>` clause at one base of the open's
/// walk ([`Resolver::open_interpretations`]), in the **one** proximity-ranked
/// list: the relativeness/nesting of the *path* sets precedence, not the
/// module-vs-reading category (FCS: a relative module out-ranks a same-named
/// root namespace, and a relative namespace out-ranks a root module). A base
/// can yield both kinds at once — a project module *merges* with a same-path
/// referenced-assembly namespace — but never a project module *and* a project
/// namespace (FS0247).
#[derive(Debug)]
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
/// the per-name value of [`Resolver::container_decls`], the complete declared-name
/// view that closes Gap A of `docs/type-qualified-case-prefix-plan.md`.
///
/// Only names reachable *bare* or as `Container.Name` are recorded (the
/// value/type/module namespaces): `let` values, exception constructors,
/// **non**-`[<RequireQualifiedAccess>]` union cases, types, nested modules /
/// abbreviations, and active-pattern cases. Require-qualified union cases and enum
/// cases are *not* — they are reachable only as `Type.Case`, so they never collide
/// with a module-qualifier segment.
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
    /// A **real** nested module (`module M = …`). *Not* an abbreviation — those set
    /// [`Self::alias`] instead, since an alias's target may be cross-file.
    pub(super) module: bool,
    /// Source position (range start) of the latest `module M = …` declaration
    /// setting [`Self::module`]. The head environment is **one source-ordered
    /// latest-wins list over lexical module declarations and `open`s** (FCS-probed:
    /// an `open` declared *after* a lexical `module Pal` outranks it for the head
    /// of `Pal.Color.Red`, one declared *before* it loses), so a candidate is
    /// compared positionally against [`Resolver::explicit_open_prefixes`].
    pub(super) module_pos: Option<u32>,
    /// A module **abbreviation** (`module P = Target`). The target may be cross-file,
    /// so a `P.Color.Red` head must be left to the alias-aware cross-file path rather
    /// than treated as a same-file module — but a nearer alias still **shadows** an
    /// outer real module of the same name, so it must stop the head walk.
    pub(super) alias: bool,
    /// An active-pattern case.
    pub(super) active_pattern: bool,
}

/// The modeled members of one in-file type — the per-type value of
/// [`Resolver::type_members`]. See that field for the population rules.
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
    pub(super) visible_from: rowan::TextSize,
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
        self.module && !self.alias && !self.exception_ctor && (in_pattern || !self.value)
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

pub(super) struct Resolver<'a> {
    pub(super) defs: Vec<Def>,
    pub(super) items: Vec<ExportedItem>,
    pub(super) resolutions: HashMap<TextRange, Resolution>,
    pub(super) scopes: Vec<Frame>,
    /// The **type-parameter** scope: a stack of frames, one per generic
    /// definition currently open (a `type` header, a generic `let`/function, a
    /// generic `member`). Each frame maps a typar's `idText` name (the bare `T`
    /// of `'T`/`^T`) to its [`DefKind::TypeParam`] binder. A `'T` *use* in a type
    /// position ([`resolve_type`](Self::resolve_type)'s `Type::Var` arm) or a
    /// `'T.Member` expression looks the name up here, innermost frame first, so a
    /// member's own `<'T>` shadows an enclosing type's.
    ///
    /// Separate from both the value [`scopes`](Self::scopes) stack (typars are in
    /// F#'s disjoint *type* namespace, so a value lookup must never find one) and
    /// the container-keyed [`type_defs`](Self::type_defs) (typars are
    /// definition-scoped and ephemeral, not visible container-wide). Empty
    /// whenever no generic definition is open.
    pub(super) typar_scopes: Vec<Vec<(String, DefId)>>,
    /// In-file `type` definitions, keyed by their enclosing container path
    /// (`Self::container_path`) then by `idText` name → defining binder. F#
    /// keeps types in a *separate* namespace from values, so this is queried
    /// only from type-syntactic positions ([`Self::resolve_type`]) — never the
    /// value [`Self::lookup`] — and so a type `T` and a value `T` never collide.
    ///
    /// **Container-scoped, not module-block-scoped**: a use sees the types
    /// declared under its *own* container, across however many source blocks
    /// that container is split into. F# treats two `namespace N` blocks as one
    /// namespace, so a later block's `type B = A` resolves `A` to the earlier
    /// block's `type A`; two *distinct* containers (`namespace A` vs
    /// `namespace B`) stay isolated because their keys differ. Within a
    /// container, a later type may reference an earlier one, and a whole
    /// `type … and …` group is interned before any of its RHSs is resolved, so
    /// the group is mutually recursive. Cross-file type resolution (`A.T` from
    /// another file) is a later slice — those references stay deferred via the
    /// existing [`Self::record_project_name_shadow`].
    pub(super) type_defs: HashMap<Vec<String>, HashMap<String, DefId>>,
    /// Each in-file type's [`SlotClass`] — keyed exactly like
    /// [`Self::type_defs`] and populated by the same
    /// [`define_type`](Self::define_type) call (last-wins on redefinition).
    /// Consumed by [`head_value_slot`](Self::head_value_slot): whether the
    /// type's name enters FCS's unqualified slot and so can EVICT a
    /// same-named definite value (probes M20a–M20o).
    pub(super) type_slot_classes: HashMap<Vec<String>, HashMap<String, SlotClass>>,
    /// Each in-file type's **access-root** — keyed like [`Self::type_defs`] and
    /// populated by the same [`define_type`](Self::define_type) call. `None` =
    /// public; `Some(k)` = accessible only from a site within the `k`-segment
    /// prefix of the type's qualified path (its `private` container plus any
    /// enclosing `private` module). A same-file module-qualified `A.Foo.Red`
    /// resolution reads it so a `private` type inaccessible from the reference site
    /// (e.g. a sibling module) does not resolve its case/member (FCS FS1092).
    pub(super) type_access_roots: HashMap<Vec<String>, HashMap<String, Option<usize>>>,
    /// The **complete declared-name view of each container** — keyed like
    /// [`Self::type_defs`], mapping each declared name to the namespaces it occupies
    /// ([`DeclKinds`]). Populated from the same declaration walk that fills
    /// `type_defs` / `type_cases` / `module_like_names` / the value frames, one mark
    /// per declaration ([`mark_decl`](Self::mark_decl)). Unlike those partial indices
    /// (and unlike `by_qualified_path`, which omits anonymous-root nested members),
    /// this is exhaustive for the modeled subset, so the same-file module-qualified
    /// case resolver
    /// ([`classify_same_file_module_qualified_case`](Self::classify_same_file_module_qualified_case))
    /// can decide a head/segment with *certainty* — emit only when unambiguous, defer
    /// on any contention (Gap A of `docs/type-qualified-case-prefix-plan.md`).
    pub(super) container_decls: HashMap<Vec<String>, HashMap<String, DeclKinds>>,
    /// In-file **type-qualified cases**, keyed by enclosing container path then by
    /// *type* name then by case name → the case's defining binder. Holds every
    /// `enum` case **and** every union case (`[<RequireQualifiedAccess>]` or not),
    /// so a qualified `Color.Red` ([`resolve_long_ident`](Self::resolve_long_ident))
    /// resolves the head `Color` to the [`Self::type_defs`] entry and the whole span
    /// to the case here — for unions and enums uniformly. Enum and RQA-union cases
    /// are reachable *only* this way (never bare `Red` — FCS reports `FS0039`); a
    /// non-RQA union case is *also* a value-namespace member (it lives in
    /// [`Self::top_level`] too, for bare / `Mod.Case` access), and this index adds
    /// the type-qualified path on top. Container-scoped like `type_defs` (looked up
    /// under the same container as the type). Cross-file `Lib.Color.Red` is resolved
    /// through the separate [`ProjectItems`] type-qualified-case index.
    ///
    /// Stores each case's **[`Resolution`]** (not a bare [`DefId`]), so `Color.Red`
    /// resolves to the *same* resolution as the case's declaration (one identity for
    /// find-references / rename). In a real-root file every case is exported and so
    /// is a [`Resolution::Item`] — a non-RQA union case reuses its value-namespace
    /// handle, an enum / RQA-union case its dedicated type-qualified handle; only an
    /// anonymous-root case (no cross-file handle) is a [`Resolution::Local`].
    pub(super) type_cases: HashMap<Vec<String>, HashMap<String, HashMap<String, Resolution>>>,
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
    /// not an enclosing `enum Color`'s case. [`type_case_path`](Self::type_case_path)
    /// consults this while walking outward so it defers rather than wrongly
    /// reaching the enclosing enum. (Type-position resolution
    /// [`lookup_type_def`](Self::lookup_type_def) does *not* consult it — a module
    /// does not shadow a type in the type namespace.)
    pub(super) module_like_names: HashMap<Vec<String>, HashSet<String>>,
    /// The **value scope of each top-level container** — a `namespace`/`module`
    /// header's path (`[]` for an anonymous module) — keyed like
    /// [`Self::type_defs`]. F# merges same-named `namespace N` blocks and isolates
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
    /// [`lookup`](Self::lookup) gives correct source-order shadowing between a
    /// value and a same-named case, and why a nested module sees its enclosing
    /// namespace's cases. (`[<RequireQualifiedAccess>]` cases are simply not added;
    /// cross-file cases stay deferred, as for `type_defs`.)
    pub(super) top_level: HashMap<Vec<String>, Frame>,
    /// Exports of earlier Compile-order files, for cross-file qualified lookup.
    pub(super) preceding: &'a ProjectItems,
    /// Name index over referenced assemblies, for fully-qualified type/member
    /// path resolution.
    pub(super) assemblies: &'a AssemblyEnv,
    /// Project-global id of this file's first item (the `preceding` count).
    pub(super) item_base: u32,
    /// Dotted path of the module currently being walked (`["Shared"]`), used to
    /// qualify exports. `None` inside an anonymous module.
    pub(super) module_path: Option<Vec<String>>,
    /// Dotted path of the enclosing *container* — the `module`/`namespace`
    /// header's `longId` (`["Demo"]` for `namespace Demo` as well as for
    /// `module Demo`; empty for an anonymous module or `namespace global`).
    /// Distinct from [`Self::module_path`], which is `None` for a namespace
    /// because a namespace binds no *values*; nested modules, however, *are*
    /// qualified under a namespace, so this is the prefix used to export a
    /// nested module's cross-file shadow path.
    pub(super) container_path: Vec<String>,
    /// How many leading segments of [`Self::container_path`] are the enclosing
    /// **namespace** (the rest are nested-module names). For `namespace
    /// Outer.Inner` it is 2; for a top-level `module Outer.Client` it is 1 (the
    /// dotted prefix `Outer`, the module name `Client` excluded); it is unchanged
    /// as the walk descends into nested modules. Implicit *relative namespace*
    /// resolution ([`open_interpretations`](Self::open_interpretations))
    /// probes only these enclosing-namespace prefixes — a module segment is **not**
    /// a namespace container, so `open Inner` inside `module Outer.Client` must not
    /// reach `Outer.Client.Inner` (FCS leaves it undefined).
    pub(super) namespace_depth: usize,
    /// The **access-floor** of the current walk position: the access-root
    /// length imposed by the deepest enclosing `module private …`, or `None` if
    /// no `private` module encloses here. A `module private M` (M at `[…P, M]`)
    /// makes its contents accessible only from within its *parent* `P`, so it
    /// contributes floor `Some(P.len())`; stacked private modules take the
    /// deepest (largest). A value/case declared here inherits this floor unless
    /// its own (or its type's) `private` modifier narrows it further — see
    /// [`Self::export_access_root_len`], which combines the two into the export's
    /// [`ExportedItem::access_root_len`](super::model::ExportedItem). This is what
    /// makes the collapse recovery sound *and* correct: a private-module value is
    /// filtered from an outside `open` (never committed), while a sibling within
    /// the enclosing namespace still resolves it (oracle-pinned D2/D6). Set at the
    /// file root and saved/restored as [`nested_module`](Self::nested_module)
    /// descends. An absolute segment count (a prefix of every descendant
    /// `container_path`), so it stays valid as the walk goes deeper.
    pub(super) access_floor: Option<usize>,
    /// Every declared named-module path in the file, accumulated as the walk
    /// enters each module (see [`ResolvedFile::module_paths`](super::model::ResolvedFile::module_paths)).
    pub(super) module_paths: Vec<Vec<String>>,
    /// Every declared project namespace path in the file (see
    /// [`ResolvedFile::namespace_paths`](super::model::ResolvedFile::namespace_paths)).
    pub(super) namespace_paths: Vec<Vec<String>>,
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
    /// [`Self::nested_module_exports`], which stays file-wide.)
    pub(super) nested_module_locals: Vec<Vec<String>>,
    /// The per-container store behind [`Self::nested_module_locals`], keyed and
    /// managed like [`Self::top_level`]: each top-level block re-takes its
    /// container's accumulated set (fresh for a first/distinct container) and
    /// stores it back after.
    pub(super) top_level_nested_locals: HashMap<Vec<String>, Vec<Vec<String>>>,
    /// The same nested modules' **qualified** paths (`module_path` prefix + the
    /// local name, `["Demo", "Calc"]`), exported via [`ResolvedFile`](super::model::ResolvedFile) into the
    /// cross-file shadow index so a *later* file's reference (`Demo.Calc.Answer`)
    /// defers too — see [`ProjectItems::nested_module_paths`].
    pub(super) nested_module_exports: Vec<Vec<String>>,
    /// Qualified paths of the file's **real** nested `module X = …` definitions —
    /// the module-only subset of [`Self::nested_module_exports`], which (via
    /// [`record_project_name_shadow`](Self::record_project_name_shadow)) conflates
    /// every project-introduced name: types, exceptions, module abbreviations,
    /// and `extern`s too. Folded into [`ProjectItems::real_nested_modules`] so a
    /// later file can ask "is there a genuine module at this path?" without a
    /// type's shadow answering yes — the companion-submodule arm of
    /// [`open_contests_candidate`](Self::open_contests_candidate) (a cross-file
    /// *type* at an open-supplied head is transparent, probes CF12/CF13; a
    /// *module* owning the residual is not, probe CF11). Guarded by
    /// [`Self::anonymous_root`] like the other cross-file exports.
    pub(super) real_nested_module_exports: Vec<Vec<String>>,
    /// Every **type definition**'s qualified export path (`["A", "Pal", "Color"]`
    /// = container + type name) paired with whether its **case set is fully
    /// indexed** in the type-qualified case exports (the `type_qualified` paths on
    /// the case `Item` [`ExportDecl`](super::model::ExportDecl)s) — `true` for a genuine
    /// non-abbreviation repr (a union/enum's cases are all exported; a
    /// record/object-model/delegate owns none), `false` for an abbreviation
    /// (whose cases live on its target, which sema does not chase cross-file) or
    /// a bodyless repr. Exported via [`ResolvedFile`](super::model::ResolvedFile)
    /// into [`ProjectItems::type_paths`] — the cross-file type index, which lets a
    /// later file decide whether an open target's segment names a type at all
    /// and, when the flag is `true`, prove the type owns no given case
    /// ([`Resolver::open_contests_candidate`](Self::open_contests_candidate)).
    /// Source-ordered ([`define_type`](Self::define_type)'s last-wins carries
    /// over); empty for an anonymous-root file (no cross-file path). Each
    /// entry also carries the type's [`SlotClass`], consumed by
    /// [`head_value_slot`](Self::head_value_slot) to decide eviction.
    pub(super) type_path_exports: Vec<(Vec<String>, bool, SlotClass)>,
    /// Opened namespace reading-groups in scope, **one [`OpenGroup`] per
    /// `open`**, in source order — F#'s implicit auto-opens first, then the
    /// file's explicit `open` decls (an `open` is in scope for the decls after
    /// it).
    ///
    /// Name resolution walks these through [`Self::open_reading_prefixes`]
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
    /// sequence lets [`resolved_project_module`](Self::resolved_project_module)
    /// honour F#'s "latest open wins" *across open kinds* — `open A.Container;
    /// open B; open Shared` resolves `Shared` as `B.Shared` (the later namespace
    /// open) even though a module open (`A.Container`) preceded it, and `open
    /// Shared; open Sub` chains `Sub` to `Shared.Sub`. Kept separate from
    /// [`Self::imports`] (which holds only *namespace* prefixes, for
    /// assembly/qualified-path resolution) so adding module prefixes here does not
    /// pollute that. Block-scoped (reset per top-level block, saved/restored
    /// across nested modules).
    pub(super) open_shortening_prefixes: Vec<Vec<String>>,
    /// Opened **assembly module** paths whose bare-name surface is *not provably
    /// complete* ([`AssemblyEnv::module_open_is_fully_enumerable`] — projection dropped
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
    pub(super) explicit_open_prefixes: Vec<(u32, Vec<String>)>,
    /// Every in-scope **project module `open`** (the [`OpenInterpretation::Module`]
    /// arm), as `(source position, canonical module path)`. A module open
    /// imports the module's TYPES into FCS's unqualified slot too (probe
    /// M20v, codex round 8 — the opened `M.Color` class evicts an earlier
    /// value exactly like a namespace-open-supplied type), so
    /// [`head_value_slot`](Resolver::head_value_slot) consults these
    /// alongside [`Self::explicit_open_prefixes`]. Same block-scoped
    /// lifecycle (reset per top-level block, saved/restored across nested
    /// modules).
    pub(super) module_open_prefixes: Vec<(u32, Vec<String>)>,
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
    /// ([`ScopeEntry::generation`]); [`lookup`](Self::lookup) treats an opened
    /// entry whose generation is older than this as *shadowed* by that later open
    /// — we cannot enumerate the new open's union cases / exception constructors /
    /// active patterns, so any of them might shadow an earlier opened name, and
    /// conservatively dropping every earlier opened name (F#: the latest open
    /// wins) is sound. A *fully-modelled* open (only `let`s) does not bump it, so
    /// earlier opens stay resolvable. Block-scoped (reset per top-level block,
    /// saved/restored across nested modules) like `imports`.
    pub(super) open_generation: usize,
    /// Project-global [`ItemId`]s of opened cross-file cases that must **not** be
    /// trusted in pattern position — exported cases of a *hidden* module (one whose
    /// `open` also brings unenumerable constructors, e.g. an active pattern, that
    /// could shadow them; see [`open_module_values`](Self::open_module_values)).
    /// They still resolve as values in expression position; only
    /// [`case_reference`](Self::case_reference) consults this to defer the pattern
    /// use. Block-scoped (reset per top-level block, saved/restored across nested
    /// modules) like [`Self::open_generation`].
    pub(super) pattern_suppressed_case_ids: HashSet<ItemId>,
    /// Qualified paths of in-project modules whose `open` may bring **value-space
    /// names we cannot enumerate**: a module declaring union cases / exception
    /// constructors / active patterns (we enumerate only `let` values), or a
    /// module *alias* whose target we could **not** resolve to an in-project
    /// module (an alias of an assembly module — a resolvable alias is canonicalised
    /// instead, see [`Self::module_aliases`]). Opening one bumps
    /// [`Self::open_generation`] so it shadows earlier opens. A *file-level*
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
    /// from the non-`private` `Module` [`ExportDecl`](super::model::ExportDecl)s)
    /// filters the `private` ones out, since F# does not bring
    /// a `private` module into scope for another file's `open` of its
    /// namespace.
    pub(super) auto_open_module_paths: Vec<(Vec<String>, bool)>,
    /// EX-2 (`docs/extension-scope-enumeration-plan.md`): the **assembly**
    /// namespace paths an explicit `open <namespace>` brings into scope, unioned
    /// across every `open` in the file. The overload engine's extension-absence
    /// gate ([`crate::infer`]'s `ExtensionScope`) reads these — an extension
    /// declared in an opened assembly namespace is in scope exactly as one in the
    /// file's declared namespace chain is, so the gate asks the *same*
    /// [`AssemblyEnv::extension_named_in_scope`](crate::AssemblyEnv) query about
    /// them. Only `open`s that resolve *entirely* to assembly-namespace readings
    /// with no residual uncertainty land here; anything else sets
    /// [`Self::open_extension_unknowable`] instead.
    ///
    /// **File-global, never block-scoped.** The gate is file-global (a single
    /// `ExtensionScope` per file), so an `open` in any nested module is folded in
    /// here for the whole file — an over-approximation that can only *add*
    /// deferrals (an `open` leaking out of its module makes the gate ask about a
    /// namespace it need not), never a wrong commit. Deliberately absent from the
    /// nested-module save/restore in `decls.rs` for that reason.
    pub(super) open_extension_namespaces: Vec<Vec<String>>,
    /// EX-2: some explicit `open` in the file brings an extension surface whose
    /// names cannot be enumerated here — a project module/namespace (EX-3), an
    /// assembly module or `open type`, or an opaque / vetoed / dropped-path open.
    /// The gate defers *every* method-call commit in the file when set, exactly as
    /// the pre-EX-2 presence gate did for *any* `open`. File-global like
    /// [`Self::open_extension_namespaces`], and monotone (set, never cleared), so
    /// the nested-module save/restore leaves it alone.
    pub(super) open_extension_unknowable: bool,
    /// The *type* each written attribute resolved to, keyed by the written
    /// name's range — built by `Resolver::resolve_attribute_lists` and carried
    /// into [`ResolvedFile::attribute_resolutions`](super::model::ResolvedFile).
    pub(super) attribute_resolutions: HashMap<TextRange, Resolution>,
    /// Every project **type**'s simple name declared *anywhere in this file*,
    /// pre-scanned before the walk ([`resolve_file`](super::resolve_file)) —
    /// order-independent by design. The attribute resolution's own-file guard
    /// (`Resolver::project_type_named`): a candidate a project type could
    /// satisfy must defer, because the tiered walk indexes neither
    /// later-in-file types (not yet walked) nor abbreviation targets.
    /// Over-approximate (an augmentation target counts too) — sound, since a
    /// spurious match only defers.
    pub(super) own_type_simple_names: HashSet<String>,
    /// The subset of [`Self::own_type_simple_names`] with a **generic**
    /// declaration anywhere in the file. FCS's attribute lookup is arity-0
    /// (`DefiniteEmpty`), so it skips a generic local type where the
    /// arity-agnostic `lookup_type_def` would hand it back — an in-file
    /// attribute commit for such a name must defer (codex round 7).
    pub(super) own_generic_type_simple_names: HashSet<String>,
    /// The subset of [`Self::own_type_simple_names`] declared by an
    /// `exception` anywhere in the file. Exceptions never enter `type_defs`,
    /// so an in-file *type* hit for such a name may be reaching past a closer
    /// exception FCS would bind — the in-file attribute commit defers
    /// (codex on stage 4).
    pub(super) own_exception_simple_names: HashSet<String>,
    /// The subset of [`Self::own_type_simple_names`] declared as an
    /// **abbreviation** anywhere in the file — a committed
    /// [`Resolution::Local`] attribute resolution of such a name may alias
    /// `ExtensionAttribute` (the resolver does not chase in-file abbreviation
    /// targets), so the gate's derivation treats it as a possible extension
    /// marker (EX-3 §2(d) stage 5).
    pub(super) own_abbrev_type_simple_names: HashSet<String>,
    /// The type and exception simple names declared **directly inside any
    /// `[<AutoOpen>]` module of this file** — pre-scanned file-globally like
    /// [`Self::own_type_simple_names`], so position- and block-blind. An
    /// in-file attribute hit for such a name must defer (AO-2): the
    /// auto-open import contests it positionally in FCS, and the walk's
    /// block-scoped [`Self::auto_open_type_shadow_names`] guard cannot see a
    /// straddle — a block-1 direct type, a block-2 auto-open redeclaration,
    /// and a block-3 attribute bind the *auto-open's* type in FCS while
    /// `lookup_type_def` retains block 1's (codex on AO-2). Position-blind
    /// is over-approximate — an in-file def declared after the import would
    /// win and could commit — which only defers (sound).
    pub(super) own_auto_open_type_names: HashSet<String>,
    /// `true` when some attribute in the file has no resolvable *name shape*
    /// — a nameless `[<>]` or an ident-less path — so the gate cannot key it
    /// and must keep the presence defer (EX-3 §2(d) stage 5).
    pub(super) attribute_shape_unknowable: bool,
    /// The **instance** member names this file's `type … with` augmentations
    /// declare (EX-3 §2(a)): an augmentation member joins its own name's
    /// call group, so the gate defers exactly those names instead of every
    /// call. Collected from *every* augmentation shape (intrinsic or optional,
    /// single-ident or dotted head — member names need no head resolution).
    pub(super) augmentation_instance_names: HashSet<String>,
    /// The **static** sibling of [`Self::augmentation_instance_names`]. A
    /// member whose staticness is not walkable lands in both sets.
    pub(super) augmentation_static_names: HashSet<String>,
    /// `true` when some augmentation member's *name* is not walkable (an
    /// operator head, an `inherit`) — the gate keeps the wholesale defer.
    pub(super) augmentation_names_unknowable: bool,
    /// The start offset of the latest `open` (of any kind) seen so far in this
    /// top-level block — monotone (nested-module exits do not restore it; a
    /// stale high-water mark only over-defers). The attribute resolution's
    /// positional contest: F# is latest-wins across bindings and opens alike,
    /// so an in-file attribute-type commit whose definition *precedes* an open
    /// must defer — the open could supply the candidate at higher priority
    /// (`type ObsoleteAttribute … ; open System; [<Obsolete>]` binds
    /// `System.ObsoleteAttribute` in FCS). Reset per top-level block with the
    /// rest of the open state.
    pub(super) latest_open_pos: u32,
    /// `true` while walking a `module rec` / `namespace rec` block. Later type
    /// declarations are in scope for earlier type annotations there, so a bare
    /// single-segment type name must defer unless already resolved in-file.
    pub(super) recursive_module_active: bool,
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
    pub(super) rec_module_names: HashSet<String>,
    /// In-project **module abbreviations**: the alias's absolute path
    /// (`[scope…, Alias]`) → the *canonical* fully-qualified path of the module it
    /// abbreviates. The RHS is resolved with the same precedence as an `open` path
    /// ([`resolved_project_module`](Self::resolved_project_module)), and chains are
    /// flattened at definition (resolving `module A2 = A1` follows `A1`), so each
    /// value is already a non-alias module path. An abbreviation is a *bare-head
    /// lexical name*: [`lexical_alias_target`](Self::lexical_alias_target) matches
    /// the alias *name* against a reference head when the alias's scope encloses
    /// the use, and `resolved_project_module` rewrites `Alias[.rest]` →
    /// `Target[.rest]` — so `open Alias`/`open Alias.Sub` resolve to the target,
    /// while a qualified `open N.Alias` does not (the alias is not a member).
    /// Only resolvable (in-project) targets are recorded — an unresolvable alias
    /// stays in [`Self::modules_with_hidden_values`] (conservative). A *file-level*
    /// accumulator keyed by absolute path; not yet merged across files, so an alias
    /// declared in an earlier file stays conservative.
    pub(super) module_aliases: HashMap<Vec<String>, Vec<String>>,
    /// `true` once an open that brings *type members* into unqualified scope is
    /// in scope — `open type T`, or `open <module>`/`open <type>` whose path is
    /// itself an assembly type. Its **static members** are modelled (one *opened*
    /// [`ScopeEntry`] per static name was pushed into the frame by
    /// [`Self::open_type_statics`]), but its **nested types** are not — so a
    /// *qualified* path could still come from it, and while one is in scope
    /// `resolve_long_ident` cannot soundly resolve a qualified path via the
    /// *other* (namespace) opens and defers instead. Also set for an `open type`
    /// whose target we could not resolve to a handle (an in-project type, an
    /// exotic type form): nothing is modelled then, so every open-based resolution
    /// stays conservative. Monotonic: an open is in scope for the decls after it,
    /// which we visit in source order.
    pub(super) unmodelled_open_active: bool,
    /// `true` once an open that *could* bring an unqualified **value** we do not
    /// model is in scope: a plain `open <assembly module/class>` (its values /
    /// statics are unmodelled here), or an `open type T` whose target we could not
    /// resolve to an assembly handle or that is project-shadowed. Such an open
    /// might provide — or shadow — a bare name with a value we cannot enumerate,
    /// so while one is in scope [`lookup`](Self::lookup) skips every *opened*
    /// ([`ScopeEntry::from_open`]) entry — resolving a modelled opened name could
    /// otherwise pick a target the opaque open shadows (correctness over
    /// availability). It also implies [`Self::opaque_dotted_open`] (an unknown
    /// bare value head could likewise be a dotted-path head we cannot resolve), so
    /// the dotted-path gate in [`resolve_long_ident`](Self::resolve_long_ident)
    /// checks both. A plain `open <namespace>` brings only *types*, no unqualified
    /// values, so it does **not** set this. Save/restored across nested modules.
    pub(super) opaque_value_open: bool,
    /// `true` once an open whose **submodules / nested types we do not model**
    /// could supply a *dotted-path head* is in scope. Set by every
    /// [`Self::opaque_value_open`] open (implied) **and** by an *enumerable*
    /// `open M` of a project module: `M`'s direct values are modelled (pushed as
    /// [`ScopeEntry::opened`] entries, so a *bare* name resolves), but `M` may
    /// also contain submodules/types we do not enumerate, so a dotted head
    /// `Sub.bar` through it could be project-rooted. While one is in scope the
    /// dotted-path gate in [`resolve_long_ident`](Self::resolve_long_ident) leaves
    /// such a head unresolved rather than risk routing it to a colliding
    /// cross-file / referenced-assembly path (correctness over availability).
    /// Unlike `opaque_value_open` it does **not** make [`lookup`](Self::lookup)
    /// skip opened entries — the module's bare values are modelled and must
    /// resolve. Save/restored across nested modules.
    pub(super) opaque_dotted_open: bool,
    /// `true` while the file's top-level module is *anonymous* (the implicit
    /// filename module — `ModuleOrNamespaceKind::Anon`, no `namespace`/`module`
    /// header). A nested module under it is reachable cross-file only via the
    /// filename-qualified path (`<FileName>.Calc.x`), which sema does not model,
    /// so [`Self::nested_module`] suppresses its cross-file export. This is
    /// distinct from "empty container path": `namespace global` also has an empty
    /// path but is a *real* (global) namespace, so its nested modules *are*
    /// bare-cross-file referenceable (`Calc.x`) and must be exported. Set per
    /// top-level module in [`resolve_file`](super::resolve_file).
    pub(super) anonymous_root: bool,
    /// Project-global [`ItemId`]s of the value binders of the **current non-`rec`
    /// `let … and …` group**, while that group's RHSs are being resolved — i.e.
    /// items that are interned (eagerly pushed to [`Self::items`] by
    /// [`prepare_binding`](Self::prepare_binding)) but **not yet in scope**, because
    /// a non-`rec` binding is not visible in its own (or its group's) RHS. Empty
    /// outside such a window and for `rec` groups (whose binders *are* in scope).
    ///
    /// [`ordinary_value_at`](Self::ordinary_value_at) skips these so the
    /// value-vs-type-qualifier shadow check
    /// ([`value_shadows_case`](Self::value_shadows_case)) does not let a binding
    /// being defined shadow its own qualified self-reference: `let Color =
    /// Lib.Container.Color.Red` resolves to the earlier file's case, not member
    /// access on the not-yet-in-scope `Color` (FCS-verified). Set/restored around
    /// [`resolve_rhss`](Self::resolve_rhss) in [`module_let`](Self::module_let).
    pub(super) pending_items: HashSet<ItemId>,
    /// The case names (`idText`-normalised) of every active-pattern recognizer
    /// whose own RHS is *currently being resolved* — accumulated by
    /// [`resolve_rhss`](Self::resolve_rhss) / [`resolve_local_let_rhss`](Self::resolve_local_let_rhss)
    /// around each binding's body and restored after. A **bare** expression use of
    /// one of these names in the recognizer's own body ([`resolve_name_use`](Self::resolve_name_use))
    /// is ambiguous between constructing the result case (FCS `ActivePatternCase`)
    /// and a fresh uppercase *pattern* rebinding (`match n with A -> A`, where FCS
    /// binds `A` as a fresh local) — a distinction a resolution-only pass cannot
    /// draw, since it drops uppercase provisional binders — so the use *declines*
    /// rather than commit an outer same-named value (the original AP-body-shadow
    /// bug) or the case. Only **bare** single-ident expression uses are declined:
    /// a *qualified* head (`A.X`, where `A` names a type sharing the case name) is
    /// resolved by [`resolve_long_ident`](Self::resolve_long_ident), which never
    /// consults this set, so it resolves normally. Empty outside a recognizer body.
    pub(super) ap_body_case_names: HashSet<String>,
    /// The [`ActivePatternShape`] of each same-file active-pattern recognizer,
    /// keyed by each of its per-case *use def ids* (the `Resolution::Local`
    /// identity a case use resolves to). Written once per recognizer by
    /// [`define_active_pattern`](Resolver::define_active_pattern); read back
    /// through [`ResolvedFile::active_pattern_shape`](super::model::ResolvedFile::active_pattern_shape).
    /// A never-cleared file-lifetime map (the `Resolver` is per-file, def ids
    /// are unique within a file). Read back through
    /// [`ResolvedFile::active_pattern_shape`](super::model::ResolvedFile::active_pattern_shape)
    /// and consumed by the applied-head split in
    /// [`resolve_pat_types`](Resolver::resolve_pat_types) (Stage 2 of
    /// `docs/parameterized-active-pattern-args-plan.md`).
    pub(super) active_pattern_shape: HashMap<DefId, ActivePatternShape>,
    /// The ident-token ranges of active-pattern *parameter* arguments the
    /// shape-keyed split has resolved as expressions (Stage 2 of
    /// `docs/parameterized-active-pattern-args-plan.md`): in `match n with DivBy
    /// divisor -> …`, `divisor` is a parameter (an outer-value reference), not a
    /// binder, so the resolution-independent [`binders`](crate::binders) walk's
    /// fabricated binder for it must be suppressed. The three binder-interning
    /// loops ([`pattern_locals`](Resolver::pattern_locals),
    /// [`prepare_binding`](Resolver::prepare_binding),
    /// [`resolve_local_let`](Resolver::resolve_local_let)) skip any def whose
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
    /// `true` while resolving a **binding-head** pattern — a `let` head or a
    /// lambda's curried parameters — where a shape-keyed active-pattern split may
    /// run ([`resolve_pattern_arg_as_expr`](Resolver::resolve_pattern_arg_as_expr)).
    /// In such a position an earlier curried parameter is **not yet in scope** when
    /// a later parameter pattern's active-pattern argument is resolved (FCS scopes
    /// curried parameters left to right — `let f d (DivBy d)` scopes the first `d`
    /// into the second parameter's `DivBy d`), so resolving the argument against the
    /// enclosing scope could commit to the *wrong* target (a same-named module value
    /// the earlier parameter should shadow). We therefore **decline** the argument's
    /// expression resolution here (its fabricated binder is still excluded — a sound
    /// coverage gap, never a wrong commit); a *match*-clause / general pattern
    /// position (the flag is `false`) resolves it normally against the enclosing
    /// scope, which is exactly FCS's rule there. Set/restored around the let-head
    /// ([`resolve_let_head_pat_types`](Resolver::resolve_let_head_pat_types)) and the
    /// lambda-parameter walk ([`pattern_locals`](Resolver::pattern_locals) with
    /// [`BinderRole::Param`](crate::binders::BinderRole)).
    pub(super) decline_binding_head_param_exprs: bool,
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
    /// The file's cross-file declarations, in source order — the single currency
    /// [`ProjectItems::extend_with`](super::model::ProjectItems::extend_with) folds
    /// (`docs/export-decl-model-plan.md` Stage 2). Every cross-file index derives
    /// from this list; a decl is appended at the same program point each legacy
    /// export writer fires, so the derivations reproduce today's ordering.
    pub(super) export_decls: Vec<ExportDecl>,
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

/// The implicit auto-opens as [`Resolver::imports`] entries — each is its own
/// single-reading `open` group (no relative/root split; they are fully qualified),
/// in source order *before* any explicit `open`, so an explicit open shadows them
/// under the latest-open-wins walk.
pub(super) fn implicit_open_groups(assemblies: &AssemblyEnv) -> Vec<OpenGroup> {
    implicit_open_namespaces(assemblies)
        .into_iter()
        .map(|ns| OpenGroup { readings: vec![ns] })
        .collect()
}

//! A name-indexed, flattened view over the entities of referenced assemblies —
//! the environment a name use resolves *into* when it names a type or member
//! from a `.dll` rather than an in-project binding (`docs/type-checker-plan.md`
//! D3, Phase 2.2).
//!
//! [`AssemblyEnv`] flattens each referenced assembly's [`Entity`] tree (top-
//! level *and* nested types) into an interned arena addressed by
//! [`EntityHandle`], so a member of a nested type can still be named by a stable
//! handle. It builds a `(namespace, name) → EntityHandle` index over the
//! top-level types and exposes descent helpers ([`AssemblyEnv::nested`],
//! [`AssemblyEnv::member`]) for the segments below the first.
//!
//! This is the **flattened-interned-index** resolution of the design doc's open
//! "`EntityHandle` identity" question, for this slice. It is pure: the caller
//! (the LSP shell) parses the resolved `.dll` paths into [`EcmaView`]s and hands
//! them in. Until the qualified-path / import slices consume it, it is unused by
//! the resolver — built and tested ahead of its consumer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use borzoi_assembly::{
    AbbreviationTarget, Access, AssemblyIdentity, Augmentation, EcmaView, Entity, EntityKind,
    ImportError, Member, MethodLike, TypeRef,
};

use crate::def::SemanticClass;
use crate::resolve::ActivePatternShape;

/// Fuel bound for chasing a chain of type-abbreviation markers
/// (`type A = B; type B = C; …`) in [`AssemblyEnv::resolve_abbreviation_target`].
/// Real alias chains are short; the bound only stops a pathological or crafted
/// cycle from looping (each hop is a distinct marker, so a legitimate chain
/// terminates well within it).
const ABBREV_CHASE_FUEL: u32 = 32;

/// Index of a source assembly in [`AssemblyEnv::assemblies`]. Identifies which
/// referenced DLL an interned entity came from, so a resolved member can name
/// the file to read its portable PDB from (go-to-definition).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AssemblyId(u32);

/// Which loaded DLL an interned entity belongs to, for same-assembly
/// comparison. Prefers per-DLL provenance ([`AssemblyId`]) — the true
/// discriminator, distinct even for two byte-identical-manifest DLLs (issue
/// #150) — and falls back to the manifest [`AssemblyIdentity`] only for the
/// synthetic single-group [`AssemblyEnv::from_entities`], which tags no
/// provenance. Keying is **homogeneous within an env**: every entity is built
/// by the same path, so all carry provenance or none do, and two keys therefore
/// compare iff they name the same loaded DLL.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AssemblyKey<'a> {
    Provenance(AssemblyId),
    Identity(&'a AssemblyIdentity),
}

/// A stable handle for one interned entity (a type/module) in an
/// [`AssemblyEnv`], top-level or nested. A newtype over an arena index, per "no
/// primitive obsession": a handle is meaningless against a different env.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityHandle(u32);

impl EntityHandle {
    fn new(index: usize) -> Self {
        EntityHandle(u32::try_from(index).expect("more than u32::MAX entities in one project"))
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// An index into an entity's `members` list. Pairs with the owning
/// [`EntityHandle`] to name a member (the design doc's `Member { parent, idx }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MemberIndex(u32);

impl MemberIndex {
    pub(crate) fn new(index: usize) -> Self {
        MemberIndex(u32::try_from(index).expect("more than u32::MAX members on one entity"))
    }

    /// The position of the named member within its entity's `members` list.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One interned entity: its projected [`Entity`] (with `nested_types` moved out
/// during interning — navigate nesting via `children`/[`AssemblyEnv::nested`],
/// not `entity.nested_types`, which is left empty) and the handles of its
/// nested types.
#[derive(Debug, Clone)]
struct EntityNode {
    entity: Entity,
    children: Vec<EntityHandle>,
    /// The assembly this entity (and its nested subtree) came from, when the env
    /// was built per-assembly ([`AssemblyEnv::from_assemblies`],
    /// [`AssemblyEnv::from_views`]); `None` for the synthetic single-group
    /// [`AssemblyEnv::from_entities`]. The per-DLL discriminator: two loaded
    /// DLLs get distinct ids even when their manifest `AssemblyIdentity`s are
    /// byte-identical.
    assembly: Option<AssemblyId>,
    /// Whether this entity's assembly has
    /// [`AbbreviationVisibility::Unknowable`] F# signature data — its pickle
    /// failed to decode, or it embeds foreign CCUs. Inherited by nested entities
    /// from their root. The OV-0.5 extension-member index reads it: an
    /// unknowable assembly may declare instance extension members in modules the
    /// pickle never described, so [`Entity::extension_member_names`] cannot be
    /// trusted complete and [`AssemblyEnv::module_extension_members`] reports
    /// [`ExtensionMembers::Unknowable`]. Derived (in the assembly projector) as
    /// `is_fsharp_assembly && !authoritative` — gated on F#-detection, unlike
    /// [`Self::signature_non_authoritative`].
    extensions_unknowable: bool,
    /// Whether this entity's assembly's host F# signature was **not
    /// authoritative** — its pickle is absent/undecodable, or it embeds foreign
    /// CCUs (an `fsc --standalone` build). Inherited by nested entities from their
    /// root. Semantic-token classification reads it via
    /// [`AssemblyEnv::fsharp_signature_unreliable`]: fsc's IL-level
    /// `CompilationMappingAttribute` / module-value markers survive but are then
    /// heuristic, and FCS imports the assembly through IL (a module reads as a
    /// plain type, its `let`s as ordinary members), so
    /// [`AssemblyEnv::entity_class`] / [`AssemblyEnv::member_class`] decline the
    /// module-specific classes rather than mis-colour them. This is plain
    /// `!authoritative` — *not* gated on F#-detection (a `--standalone` image can
    /// lose its assembly-level `FSharpInterfaceDataVersionAttribute`, so the
    /// [`Self::extensions_unknowable`] composite would miss it). Carried per
    /// **entity** (not per [`AssemblyId`]) so it is preserved on every build path,
    /// including [`AssemblyEnv::from_entities`], which tags no ids.
    signature_non_authoritative: bool,
    /// The **top-level** namespace this entity's subtree was declared in. A *nested*
    /// ECMA TypeDef carries no namespace of its own (`Entity::namespace` is empty), so
    /// a nested module cannot ask "did my namespace drop a type?" without it — and a
    /// dropped nested descendant is recorded under the enclosing *top-level* namespace
    /// (review, Slice A). Inherited down the subtree exactly as
    /// [`Self::extensions_unknowable`] is.
    owning_namespace: Vec<String>,
}

/// The outcome of a **type/module-qualified** lookup (`Path.Member`) — the *whole*
/// answer for the qualified `Channel`, selection and path-ownership together (see
/// [`AssemblyEnv::static_lookup`]).
///
/// Three states, because "we cannot name a target" and "the name is not there" must
/// not be confused: an occupied name still *owns the path*, so a lower-priority
/// reading must not re-root it and resolve the same path somewhere else (review
/// round 3), while a genuinely absent one *must* let it (review round 4). Those two
/// review findings were the same defect twice — two predicates disagreeing about
/// whether a member exists — which is why ownership is a state of this enum rather
/// than a second predicate that could drift from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaticLookup {
    /// **No member of that name is reachable here at all** — not on the entity, not
    /// through its base chain, counting only members FCS's qualified lookup can see
    /// (so an F#-native augmentation, which it provably cannot, does not keep the name
    /// occupied). The tail is genuinely absent, and *only* this lets a lower-priority
    /// reading own the path instead.
    Absent,
    /// Exactly one uniquely-selectable public static: this is it.
    Resolved(MemberIndex),
    /// The name **is** occupied, but we decline to name a target — defer, and keep the
    /// path owned. Either it is not *selectable* through a qualified path (an
    /// instance-only member, an inherited static, an unknowable base chain — FCS finds
    /// the name and errors rather than re-rooting the path; for a *module* receiver
    /// only its own contents count, never the compiled class's base chain — see
    /// `module_qualified_occupied`), or it is selectable but not *uniquely* (an
    /// overload set, a metadata ambiguity), or we cannot decide it at all (an
    /// undecidable augmentation — an `Augmentation` of `Possible` certainty).
    Uncertain,
}

/// The complete-or-opaque bare-name surface an `open <assembly module>` folds
/// into scope — [`AssemblyEnv::open_fold_surface`], the fold's per-container
/// unit (`docs/assembly-module-open-plan.md`, "the fold").
///
/// `entries` lists **every** bare name the open imports, in FCS's fold order
/// (`AddModuleOrNamespaceContentsToNameEnv`: exceptions → tycon tier → vals →
/// `[<AutoOpen>]` submodules, recursively). A consumer pushes them in that
/// order into a latest-wins scope, and FCS's precedence — a val beating its own
/// module's union case, an auto-open child's value beating the parent's val —
/// falls out of the order instead of being re-derived per pairing.
///
/// `residue` is the *name-unknown* remainder: `true` when the open imports
/// names this surface cannot list at all (an unknowable pickle, an undecodable
/// member, a union whose case names the pickle did not supply). A residue-free
/// surface is **complete**: no name FCS folds is missing from `entries`, so
/// nothing outside them needs shadowing. With residue, the consumer must both
/// shadow every earlier open (the generation barrier) and demote this open's
/// own entries to opaque — an unlisted name can outrank them and fold order
/// within the container is no longer decidable.
#[derive(Debug, Clone, Default)]
pub struct OpenFoldSurface {
    /// Every enumerated bare name, in FCS fold order.
    pub entries: Vec<OpenFoldName>,
    /// Whether names exist that `entries` cannot list (see type doc).
    pub residue: bool,
    /// A *weaker* residue: unlistable names exist, but all are confined to the
    /// top container's own **tycon tier** (a case-nameless union child), which
    /// FCS folds *before* the container's vals. They can shadow an earlier
    /// open's names and this container's own tycon-tier entries (so the
    /// consumer still bumps the barrier and demotes the case entries), but
    /// they can never outrank the container's **vals**, which stay definite —
    /// the round-10 `HiddenBelowVals` rule, now per fold position. The same
    /// loss in an `[<AutoOpen>]` *descendant* escalates to [`Self::residue`]
    /// instead: a child's tycon tier folds *after* the parent's vals, so its
    /// hidden cases can contest them.
    pub residue_below_vals: bool,
    /// Names this surface contributes as **value-slot contestants** but not as
    /// resolvable entries — a namespace half's constructible **type** names (see
    /// [`AssemblyEnv::open_namespace_fold_surfaces`]). They take FCS's unqualified
    /// constructor slot, so a same-named *value* from **another** surface (the
    /// module half, or a different assembly) is a reference-order contest that
    /// must defer; but a value from **this** surface (an `[<AutoOpen>]` module in
    /// the same namespace, which FCS folds after the tycon tier) still wins, so a
    /// contestant only ever demotes across surfaces, never within one. Not pushed
    /// as an entry: the type takes its slot via the eviction/type-index channel,
    /// which also serves qualified `Type.Member`, and a value entry here would
    /// clobber it.
    pub contestant_names: Vec<String>,
}

/// One bare name of an [`OpenFoldSurface`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFoldName {
    /// The F# source name a use site writes.
    pub name: String,
    /// The definite target, or [`OpenFoldTarget::Opaque`] when the name is in
    /// scope (it shadows by position) but names nothing we can commit to.
    pub target: OpenFoldTarget,
    /// Which lookup namespace(s) the name occupies.
    pub space: OpenFoldSpace,
    /// Whether the name is a constructor **case** — a union case, an exception
    /// constructor, or an active-pattern tag. Pattern-position lookup
    /// (`case_reference`) accepts these where it skips plain values.
    pub is_case: bool,
    /// The recognizer shape when this entry is an **active-pattern tag** (a
    /// `space: Pattern`, `is_case: true`, `target: Opaque` entry demangled from a
    /// `|A|B|` val name); `None` for every other entry. Carried to the scope
    /// entry so an applied assembly active-pattern head splits its arguments by
    /// shape (`docs/export-decl-model-plan.md` Stage 3b) — the tag's `Deferred`
    /// resolution has no identity to key the shape on.
    pub ap_shape: Option<ActivePatternShape>,
    /// Whether a **val** entry may be an FCS *constant pattern* — a CLI
    /// `Literal`-flagged field, a `System.Decimal` field (a C# `const decimal`
    /// carries `DecimalConstantAttribute` with NO `Literal` flag — Q17), or an
    /// undecidable target ([`AssemblyEnv`]'s `value_may_be_constant_pattern`).
    /// A literal enters FCS's pattern namespace (`ePatItems`, latest-wins), so
    /// such a val met before a case in the bare pattern-position scan defers
    /// the case rather than being skipped as a plain value. `false` for every
    /// case / type-shadow entry.
    pub constant_pattern: bool,
}

/// The target of an [`OpenFoldName`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenFoldTarget {
    /// A uniquely-selectable member (a module val).
    Member {
        parent: EntityHandle,
        idx: MemberIndex,
    },
    /// A whole entity — an exception's constructor *is* its type.
    Entity(EntityHandle),
    /// In scope, shadowing, but naming nothing (the consumer defers): union
    /// cases and active-pattern tags (no member exists to point at), nested
    /// type names (FCS's unqualified constructor slot — modelled only as a
    /// contestant), an auto-open *type*'s statics, an overload set, a
    /// cross-assembly collision.
    Opaque,
}

/// Which lookup namespace an [`OpenFoldName`] occupies. F# separates the
/// expression **value** space from the pattern **constructor** space; union
/// cases and exception constructors live in both, active-pattern tags in the
/// constructor space only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenFoldSpace {
    /// Expression position only (vals, type names, auto-open statics).
    Value,
    /// Pattern position only (active-pattern tags).
    Pattern,
    /// Both (union cases, exception constructors — a case is a value too).
    Both,
}

/// The tags **and** [`ActivePatternShape`] of an active-pattern banana name, for
/// a recognizer folded into scope from a referenced assembly (Stage 3b of
/// `docs/export-decl-model-plan.md`): `|Even|Odd|` → total multi-case
/// `[Even, Odd]`; `|Positive|_|` → partial single-case `[Positive]` (the trailing
/// `_` is the partial marker, not a tag). `None` when the name starts with `|`
/// but is not a well-formed banana — the caller must treat the val as
/// name-unknown residue rather than guess, and attaches no shape.
///
/// Follows FCS's `ActivePatternInfoOfValName` (`PrettyNaming.fs`): the recognizer
/// is **total** unless the *last* `|`-segment is exactly `_`; every remaining
/// segment is a real case name. The IL method name *is* the mangled logical
/// name FCS itself demangles, so `total` / `single_case` / the case list are
/// exactly what FCS computes.
///
/// [`ActivePatternShape::arity`] is always `None`. The metadata parameter count
/// over-counts F#'s type-derived `paramCount` under argument tupling (the
/// flattened IL signature of an F# assembly cannot distinguish a curried group
/// from a tupled one — its `arg_group_count` is `None` for the same reason), and
/// an *over*-estimated arity is a wrong commit (a use at `k = paramCount + 1`
/// would treat the result binder as a parameter). So arity stays unknown, and
/// only a **total single-case** recognizer — whose split is `frontAndBack`, arity
/// -free — changes behaviour; a partial one keeps today's fabricate-a-binder.
///
/// Used only by the fold ([`AssemblyEnv::fold_container_into`], for `open
/// <module>` / `open <namespace>`); the caller has already established the
/// container is an authoritative F# module.
fn active_pattern_banana(name: &str) -> Option<(Vec<&str>, ActivePatternShape)> {
    let inner = name.strip_prefix('|')?.strip_suffix('|')?;
    let parts: Vec<&str> = inner.split('|').collect();
    // FCS: partial iff the LAST `|`-segment is the wildcard marker `_`.
    let (total, tags): (bool, Vec<&str>) = match parts.split_last() {
        Some((last, front)) if *last == "_" => (false, front.to_vec()),
        _ => (true, parts),
    };
    // Malformed only if a *case* segment is empty (`||`, `|A||B|`) — then attach
    // no shape. Two forms FCS accepts must NOT be reported malformed, which would
    // make the caller mark the whole open surface as residue and demote unrelated
    // members:
    // - a **zero-tag** recognizer (the quoted `` `|_|` ``, a partial with no case
    //   names — `ActivePatternInfoOfValName` returns empty tags; codex 5b);
    // - a **nonterminal `_`** segment (`|_|A|`): FCS treats only the *last* `_` as
    //   the partial marker, so a `_` before it stays a real tag (codex 6b), which
    //   `single_case` must count.
    if tags.iter().any(|t| t.is_empty()) {
        return None;
    }
    let shape = ActivePatternShape {
        total,
        single_case: tags.len() == 1,
        arity: None,
    };
    Some((tags, shape))
}

/// Whether a type of `kind` (with IL value-type-ness `is_struct`) occupies FCS's
/// unqualified **constructor slot**, and so contests a same-named value brought
/// into scope by the same `open`. The mirror of the resolver's
/// `assembly_slot_class` (`resolve/lookup.rs`) — `Evicts`/`Unknown` map to
/// `true`, `Keeps` to `false` — kept in sync with it: `mayHaveConstruction =
/// isClassTy || isStructTy || isDelegateTy` (`AddPartsOfTyconRefToNameEnv`), plus
/// the undecidable kinds (delegate / abbreviation) which defer the contest rather
/// than risk a wrong target. `Exception` is `Unknown` there but folded as an
/// (opaque) entry in the fold's pass 1, so the caller excludes it separately.
fn type_name_is_value_slot_contestant(kind: EntityKind, is_struct: bool) -> bool {
    if is_struct {
        // Any IL value type: `Struct`, `Enum`, `[<Struct>]` record/union.
        return true;
    }
    match kind {
        // `Evicts`: a construction-capable reference type.
        EntityKind::Class | EntityKind::Struct | EntityKind::Enum => true,
        // `Unknown`: undecidable, so defer the contest.
        EntityKind::Delegate | EntityKind::Abbreviation | EntityKind::Exception => true,
        // `Keeps`: takes no unqualified constructor slot.
        EntityKind::Interface
        | EntityKind::Union
        | EntityKind::Record
        | EntityKind::Module
        | EntityKind::Measure => false,
    }
}

/// The **channel** a name is reached through. One of the two dimensions of the
/// extension-visibility rule (the other is [`ExtensionKind`]); together they decide
/// [`Presence`], and [`presence`] is the *only* place that decision is written down.
///
/// The two channels genuinely disagree — a C#-style `[<Extension>]` static is out of
/// the unqualified environment but *is* reachable as `Enumerable.Select(xs, f)` — so
/// the rule cannot be keyed on the member alone. Every consumer of the extension facts
/// names its channel, which is what stops a new consumer from inventing a fourth
/// opinion: adding a channel here is an exhaustive-match error in `presence`, and the
/// FCS matrix (`tests/all/extension_visibility_matrix.rs`) has a column per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channel {
    /// A **bare** name, after an `open` / `open type` / the auto-open fold
    /// ([`AssemblyEnv::open_static_entries`]).
    Bare,
    /// A **qualified** path — `Type.Member`, `Module.value`
    /// ([`AssemblyEnv::static_lookup`]).
    Qualified,
}

/// How sure we are of an [`ExtensionKind`]. `Possible` is not a hedge: it is the
/// honest answer when the metadata cannot decide, and it resolves to a *deferral*
/// rather than to a guess in either direction — both directions being wrong
/// resolutions (hide a value FCS resolves, or surface a member FCS hides).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Certainty {
    Certain,
    Possible,
}

/// What sort of **extension member** a member is — the fact FCS's name resolution
/// keys on, derived once by [`AssemblyEnv::extension_kind`], which is the only reader
/// of the underlying metadata (`Augmentation`, `is_extension_method`,
/// `is_extension_container`, `arg_group_count`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtensionKind {
    /// Not an extension member: an ordinary `let` / member / static, present in every
    /// channel. A *curried* `[<Extension>] static member M x y` lands here too — FCS's
    /// C#-style predicate requires exactly one argument group, so a curried one stays
    /// in unqualified scope (fsi-verified).
    Ordinary,
    /// An **F#-native augmentation** (`type T with member M …`), which fsc compiles to
    /// a static of the enclosing module/type. Reachable only through the dot on a
    /// *value*, so it is in neither channel (fsi: both bare `Force` and
    /// `LazyExtensions.Force l` are FS0039).
    ///
    /// [`Certainty::Possible`] when only the IL dot-name mangling says so, on an image
    /// with no usable pickle — a dotted `[<CompiledName>]` on an ordinary `let` is
    /// indistinguishable from an augmentation's mangling.
    Augmentation(Certainty),
    /// A **C#-style `[<Extension>]` member** — FCS's
    /// `IsMethInfoPlainCSharpStyleExtensionMember`: a non-generic `[<Extension>]`
    /// container, the method itself `[<Extension>]`, exactly one argument group with
    /// ≥ 1 argument. Out of the unqualified environment, but *qualified*-reachable
    /// (fsi: `System.Linq.Enumerable.Select(xs, f)` compiles).
    ///
    /// [`Certainty::Possible`] for an F# assembly, whose flattened IL signature cannot
    /// say whether the source was curried (kept in scope) or tupled (hidden).
    CSharpStyle(Certainty),
}

/// Whether a member is *there* for name resolution, on one [`Channel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presence {
    /// It resolves through this channel.
    Present,
    /// FCS provably does not reach it here — so a lower-priority reading may own the
    /// name instead, and reporting it present would be a wrong target.
    Absent,
    /// We cannot tell which of the two it is, and both mistakes are wrong resolutions.
    /// The name is **occupied** (it shadows by position / owns the path) but names no
    /// target: a deferral.
    Uncertain,
}

/// The whole extension-visibility rule, in one table: what an [`ExtensionKind`] does
/// to name resolution on a given [`Channel`]. Every cell is fsi-verified against the
/// real compiler and pinned by `tests/all/extension_visibility_matrix.rs`.
///
/// The asymmetric row is [`ExtensionKind::CSharpStyle`], and it is why the rule needs
/// the channel dimension at all: FCS filters those out of the unqualified environment
/// (`ChooseMethInfosForNameEnv` — bare `Select` after `open type System.Linq.Enumerable`
/// is FS0039) while its *qualified* member lookup reaches them normally.
fn presence(kind: ExtensionKind, channel: Channel) -> Presence {
    match (kind, channel) {
        (ExtensionKind::Ordinary, _) => Presence::Present,
        // An augmentation is reachable through neither channel — only through the dot
        // on a value — so `Certain` is absent everywhere, and the undecidable case
        // defers everywhere.
        (ExtensionKind::Augmentation(Certainty::Certain), _) => Presence::Absent,
        (ExtensionKind::Augmentation(Certainty::Possible), _) => Presence::Uncertain,
        (ExtensionKind::CSharpStyle(Certainty::Certain), Channel::Bare) => Presence::Absent,
        (ExtensionKind::CSharpStyle(Certainty::Possible), Channel::Bare) => Presence::Uncertain,
        (ExtensionKind::CSharpStyle(_), Channel::Qualified) => Presence::Present,
    }
}

/// A flattened, name-indexed view over referenced assemblies' entities. See the
/// module docs.
#[derive(Debug, Default, Clone)]
pub struct AssemblyEnv {
    nodes: Vec<EntityNode>,
    /// `(namespace, name, generic arity) → handle` for **top-level** types
    /// only; nested types (whose namespace is empty) are reached by descent
    /// from their encloser. Keyed by the segment-structured namespace rather
    /// than a dotted string so a quoted identifier containing a `.` cannot
    /// collide with a multi-segment namespace. Arity is part of the key
    /// because CLR metadata distinguishes types that share a namespace and
    /// simple name but differ in generic arity (`` Func`1 `` vs `` Func`2 ``);
    /// `Entity.name` strips the `` `n `` suffix into `generic_parameters`, so
    /// without arity those distinct types would collapse onto one handle.
    ///
    /// Keyed by the name F# *source* uses: the IL name for an ordinary entity, or
    /// the stripped source name for a module-suffix module (`ListModule` is keyed
    /// as `List`, never as `ListModule` — F# source never writes the compiled
    /// name). A suffixed module's source-name key never displaces a same-named
    /// type (see [`AssemblyEnv::from_entities`]); for every non-suffixed entity
    /// the IL name *is* the source name.
    by_type: HashMap<TypeKey, EntityHandle>,
    /// `namespace` → handles of its top-level `[<AutoOpen>]` modules. F# opens
    /// these into unqualified scope whenever their enclosing namespace is open
    /// (FSharp.Core's `Operators` / `ExtraTopLevelOperators`, which carry
    /// `printfn` / `id` / operators). The resolver folds them into its
    /// open-type set so a bare `printfn` resolves to the module's static
    /// member — see `Resolver::open_auto_open_modules_in`.
    auto_open_modules: HashMap<Vec<String>, Vec<EntityHandle>>,
    /// Namespaces where an assembly whose **abbreviations are unknowable**
    /// ([`AbbreviationVisibility::Unknowable`]: its signature pickle failed to
    /// decode, or it embeds foreign CCU pickles we never decode) is known —
    /// via an observed ECMA TypeDef *directly* in that namespace — to declare
    /// something, so it may also declare a metadata-invisible type
    /// abbreviation there that shadows a primitive alias. Exact namespaces
    /// only, never ancestors: F# `open N` imports only `N`'s direct members,
    /// so evidence of a real declaration in a *descendant* namespace says
    /// nothing about whether `N` itself has an abbreviation, and marking `N`
    /// on that basis would defer opens with no abbreviation anywhere in
    /// scope.
    ///
    /// This is the coarse, name-blind *fallback* channel. The normal channel
    /// is precise: a decodable pickle's abbreviations arrive as synthesised
    /// [`EntityKind::Abbreviation`] marker entities in the entity tree itself
    /// (see `apply_abbreviation_markers` in `borzoi-assembly`), indexed
    /// and matched by name like any other type. A namespace whose *only*
    /// public content is bare abbreviations emits no TypeDef and so stays
    /// invisible to this fallback — acceptable for the rare
    /// undecodable-pickle case it now serves.
    unknowable_abbreviation_namespaces: HashSet<Vec<String>>,
    /// Source DLL path per [`AssemblyId`] — one entry per loaded assembly, in
    /// load order. `Some` when the build path knows the DLL's path
    /// ([`AssemblyEnv::from_assemblies`]); `None` for a path-less view
    /// ([`AssemblyEnv::from_views`]), which still gets its own id so provenance
    /// distinguishes same-named views. An entity's [`EntityNode::assembly`]
    /// indexes this.
    assemblies: Vec<Option<PathBuf>>,
    /// Manifest identity per [`AssemblyId`], parallel to [`Self::assemblies`] —
    /// one entry per *loaded* DLL, **including one whose types were all dropped**
    /// (a rootless assembly contributes no [`Self::top_level_types`], so counting
    /// DLL names off the interned entities would miss it — issue #150 / codex P2).
    /// `Some` when the build path knows the identity: [`AssemblyEnv::from_views`]
    /// reads `EcmaView::identity` for every view; [`AssemblyEnv::from_assemblies`]
    /// takes it from the first surviving root, so a *rootless* assembly there is
    /// `None` (its identity is genuinely unavailable — a documented residual on
    /// that constructor's input shape). Empty for the synthetic single-group
    /// [`AssemblyEnv::from_entities`], which registers no per-DLL identities;
    /// [`Self::unique_assembly_key_for_name`] falls back to the entities there.
    assembly_identities: Vec<Option<AssemblyIdentity>>,
    /// Whether the set of loaded-DLL identities is **incomplete** — some DLL the
    /// env cannot name is present. A referenced CCU is pickled only by simple
    /// name, so an unnameable DLL could *be* that name: its presence makes
    /// referenced-CCU uniqueness undecidable, and [`Self::unique_assembly_key_for_name`]
    /// declines wholesale (correctness over availability). Set when a `None`
    /// identity is registered (a rootless projection the shorter constructors
    /// could not name) or by [`Self::mark_referenced_assemblies_incomplete`] (the
    /// LSP host, when the projector **skipped** a DLL FCS can still load — issue
    /// #150 / codex P2). The runtime env supplies every identity and skips no
    /// loadable DLL, so this stays `false` there.
    assembly_identities_incomplete: bool,
    /// Every **top-level** type's handle, in interning order — unlike
    /// [`Self::by_type`] this keeps *all* handles at a colliding
    /// `(namespace, name, arity)` (two referenced assemblies can expose the
    /// same FQN; `by_type` is first-wins). [`Self::public_types_named`] scans
    /// it so the head-slot eviction check sees a constructible class even when
    /// a non-constructible collision was indexed first.
    top_level_types: Vec<EntityHandle>,
    /// [`Self::top_level_types`] grouped by their **exact** namespace — the
    /// per-namespace index the extension gate's hot per-call queries
    /// ([`Self::namespace_has_extension_named`] and its `_in_assembly` sibling) scan
    /// instead of the whole `top_level_types` list. Without it those queries were
    /// O(all referenced types) *per namespace per call*, which on a full SDK
    /// reference closure dominated `infer_file` (review, GPT-5.6). Same handles as
    /// `top_level_types`, so it keeps all collisions at a shared FQN too.
    types_by_namespace: HashMap<Vec<String>, Vec<EntityHandle>>,
    /// **Namespace** paths the referenced assemblies' assembly-level
    /// `[<assembly: AutoOpen("…")>]` attributes implicitly open — what drives
    /// the resolver's implicit opens (plan
    /// `docs/fsharp-core-autoopen-resolution-plan.md` S3, the analogue of
    /// FCS's `AddCcuToTcEnv`). Ordered as FCS applies them (assembly order,
    /// then manifest-attribute order within an assembly, with `Microsoft`
    /// prepended for FSharp.Core itself — FCS's fslib special case), deduped
    /// keeping the **last** occurrence's position (a re-open re-establishes
    /// latest-open precedence), and already filtered by
    /// [`Self::record_assembly_auto_opens`]: module/type-shaped and
    /// nonexistent paths never land here.
    implicit_open_namespace_paths: Vec<Vec<String>>,
    /// Handles of the **module/type-shaped** assembly-level auto-opens that
    /// [`Self::record_assembly_auto_opens`] drops from
    /// [`Self::implicit_open_namespace_paths`] (they are opened *like a module*,
    /// not a namespace — FSharp.Core's `IntrinsicOperators` /
    /// `TaskBuilderExtensions*`). Their **operators** are unmodelled and their
    /// nested *values* are deliberately not made bare-resolvable, but FCS still
    /// brings their **extension members** into method-call scope — so the OV-6
    /// extension-absence gate ([`Self::extension_named_in_scope`]) treats their
    /// presence as an extension surface and defers, lest it falsely prove an
    /// intrinsic overload's name absent.
    auto_open_module_handles: Vec<EntityHandle>,
    /// The assembly-level auto-opens [`Self::record_assembly_auto_opens`]
    /// **dropped** as *contested* — a namespace declared by more than one
    /// referenced assembly (narrowing 1), which sema's path-based (assembly-blind)
    /// open machinery cannot apply contributor-scoped, so it applies none of it.
    ///
    /// Each entry is `(contributing assembly's provenance id, namespace path)` —
    /// the id, not a name, because two loaded DLLs can share a simple name or
    /// even a whole manifest identity (issue #150).
    /// The *name-resolution* consequence is unchanged (the open is not applied at
    /// all — the pre-S3 status quo, deferrals only). The **extension gate** reads
    /// them here, and does so contributor-scoped: FCS opens the *contributing
    /// CCU's* namespace entity, so only **that assembly's** extension members in
    /// that namespace enter scope — a sibling's same-named namespace stays closed
    /// (fsi-verified, see the auto-open plan). Recording the entries rather than a
    /// bare "something was dropped" bit is what lets the gate ask about the called
    /// *name* instead of deferring wholesale, and this is not a corner: FSharp.Core
    /// auto-opens `Microsoft`, which the BCL also declares
    /// (`Microsoft.Win32.SafeHandles`), so **every** real project has a contested
    /// auto-open, and a wholesale defer here defers every overloaded call in the
    /// codebase.
    contested_auto_opens: Vec<(AssemblyId, Vec<String>)>,
    /// Whether the referenced-assembly **extension surface is not fully known** —
    /// a *global* projection failure that could hide an extension in any namespace:
    /// an assembly-level `[<AutoOpen>]` list that could not be read (its implicit
    /// opens are unknown), or an entirely **skipped assembly** projection. The OV-6
    /// gate then defers wholesale. Set by
    /// [`Self::mark_extension_surface_unknowable`] (the LSP host, which observes
    /// these failures); false by default (a clean projection is trusted).
    extension_surface_unknowable: bool,
    /// Namespaces in which a referenced assembly **dropped an undecodable type**
    /// (possibly a C#-style `[<Extension>]` class the entity tree no longer shows).
    /// The OV-6 gate treats these as possibly-extension-bearing per
    /// [`Self::extension_named_in_scope`] — *namespace-scoped* uncertainty, so a file
    /// whose in-scope namespaces had no drop still commits (unlike the global
    /// [`Self::extension_surface_unknowable`]). Populated by
    /// [`Self::mark_namespace_dropped_type`] (the LSP host).
    namespaces_with_dropped_types: HashSet<Vec<String>>,
}

/// The top-level type index key: namespace segments, simple name, and generic
/// arity (the number of type parameters the type declares).
type TypeKey = (Vec<String>, String, usize);

/// One referenced assembly as
/// [`AssemblyEnv::from_assemblies_with_projection_knowability`] takes it: the source
/// DLL path, its projected roots, its [`AbbreviationVisibility`], its
/// [`fsharp_extension_index_unknowable`](borzoi_assembly::AssemblyProjectionSkips::fsharp_extension_index_unknowable) bit, its
/// [`fsharp_signature_non_authoritative`](borzoi_assembly::AssemblyProjectionSkips::fsharp_signature_non_authoritative) bit, its
/// assembly-level `[<assembly: AutoOpen("…")>]` paths (manifest order), and its
/// manifest identity — supplied explicitly (the caller's `EcmaView` knows it)
/// so a **rootless** projection still registers a DLL name in
/// [`AssemblyEnv::assembly_identities`]; `None` falls back to the first root
/// (issue #150 / codex P2).
type AssemblyProjectionInput = (
    PathBuf,
    Vec<Entity>,
    AbbreviationVisibility,
    bool,
    bool,
    Vec<String>,
    Option<AssemblyIdentity>,
);

/// Whether one referenced assembly's F# type abbreviations are fully
/// represented in its projected entity tree.
///
/// The projection synthesises a name-only [`EntityKind::Abbreviation`] marker
/// for every public metadata-invisible abbreviation it can decode from the
/// assembly's signature pickle, so the common case is [`Self::Modelled`]:
/// abbreviation shadows are ordinary name-keyed lookups. [`Self::Unknowable`]
/// is the fallback for an assembly whose abbreviations could not be decoded
/// ([`AssemblyProjectionSkips::fsharp_abbreviations_unknowable`][skips]): type-position
/// lookup must then treat every namespace the assembly declares into as
/// possibly shadowed (coarse and name-blind, but sound).
///
/// [skips]: borzoi_assembly::AssemblyProjectionSkips::fsharp_abbreviations_unknowable
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbbreviationVisibility {
    /// The entity tree (including synthesised abbreviation markers) is
    /// abbreviation-complete for this assembly.
    Modelled,
    /// The assembly may export abbreviations nothing in the entity tree
    /// witnesses — its signature pickle failed to decode, or it embeds
    /// foreign CCU pickles that are never decoded.
    Unknowable,
}

/// A module's F#-native **instance extension members**, as the overload
/// resolution extension-absence gate (`docs/overload-resolution-plan.md`
/// §4.1(4)) needs them — the answer to "does this opened module declare an
/// instance extension member named `M`?".
///
/// Built by stage OV-0.5 from the pickle's `IsExtensionMember ∧ IsInstance` bit
/// ([`Entity::extension_member_names`]), the *no-false-negative* signal that
/// replaces the per-method [`is_extension_method`] flag OV-0 found unreliable.
///
/// [`is_extension_method`]: borzoi_assembly::MethodLike::is_extension_method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionMembers<'a> {
    /// The module's instance extension member source names are fully known —
    /// the gate checks membership of the queried name against this slice
    /// (empty ⇒ the module declares none). Exhaustive because the owning
    /// assembly's F# signature data is [`AbbreviationVisibility::Modelled`].
    Known(&'a [String]),
    /// The owning assembly's F# signature data is
    /// [`AbbreviationVisibility::Unknowable`] (its pickle failed to decode, or it
    /// embeds foreign CCUs), so its extension members cannot be enumerated — the
    /// gate must **defer** rather than trust a possibly-incomplete name set.
    Unknowable,
}

/// The outcome of walking an entity's base-type chain
/// ([`AssemblyEnv::base_chain`]) — how completely the inherited member set is
/// known, which decides whether an inheritance-aware lookup can resolve or must
/// defer.
enum BaseChain {
    /// Fully resolved to a root (`System.Object`, or a type with no base): the
    /// handles hold the *complete* inherited member set, nearest first.
    Complete(Vec<EntityHandle>),
    /// Resolved down to — but not including — `System.Object`, which is absent
    /// from this env. Every base *except* the universal `Object` is present, so a
    /// data-member lookup (Object declares none) is complete, but a method call
    /// naming an `Object` method must defer — its inherited overload is invisible.
    ObjectCapped(Vec<EntityHandle>),
    /// A base could not be resolved (generic, nested, absent, or a metadata cycle)
    /// and it is not the universal `Object`: the inherited group is unknowable, so
    /// every lookup must defer.
    Incomplete,
}

/// The outcome of walking an **interface receiver's** member sources
/// ([`AssemblyEnv::interface_member_chain`]) — the interface-DAG analogue of
/// [`BaseChain`]. FCS gives a receiver whose static type is an interface
/// `System.Object`'s members *plus* all transitively inherited interfaces
/// (`TypeHierarchy.fs:256–260`); a class/struct receiver does **not** walk its
/// interfaces (`followInterfaces=false`), which is why this is a separate walk
/// used only for interface receivers. See `docs/interface-walk-plan.md`.
#[cfg_attr(test, derive(Debug))]
enum InterfaceChain {
    /// All transitively inherited interfaces resolved **and** `System.Object`
    /// present: `levels` = the receiver interface, its inherited interfaces, then
    /// `System.Object` last (a value's dynamic type is a class, so `Object`'s
    /// members are farther than any interface's).
    Complete(Vec<EntityHandle>),
    /// All inherited interfaces resolved, but `System.Object` is absent from the
    /// env (a single-assembly view). `levels` excludes `Object`. A data-member
    /// lookup is still complete (`Object` declares no data members); a method call
    /// naming an `Object` method must defer.
    ObjectCapped(Vec<EntityHandle>),
    /// A transitively inherited interface is unresolvable (generic — the common
    /// `ICollection<T> : IEnumerable<T>` — nested, absent, or wrong-assembly), so
    /// the member surface is unknowable and every lookup must defer.
    Incomplete,
}

impl AssemblyEnv {
    /// Build the env from already-enumerated top-level entities (the pure core).
    /// On a `(namespace, name)` collision across assemblies the first-enumerated
    /// wins — a deterministic, refine-later choice (real FCS resolution has
    /// reference-order rules; this slice does not model multi-assembly shadowing).
    pub fn from_entities(roots: Vec<Entity>) -> Self {
        let mut env = AssemblyEnv::default();
        env.index_roots(
            roots
                .into_iter()
                // Synthetic entities are authoritative: no non-authoritative flag.
                .map(|r| (None, AbbreviationVisibility::Modelled, false, false, r)),
        );
        env
    }

    /// Build the env from referenced assemblies, **tagging each assembly's
    /// entities with its source DLL path** so a resolved member can name the
    /// file it came from — go-to-definition reads that DLL's portable PDB for the
    /// source location. Same name-index rules as [`Self::from_entities`]; on a
    /// cross-assembly `(namespace, name, arity)` collision the assembly listed
    /// first wins (deterministic, refine-later).
    pub fn from_assemblies(assemblies: Vec<(PathBuf, Vec<Entity>)>) -> Self {
        Self::from_assemblies_with_abbreviation_visibility(
            assemblies
                .into_iter()
                .map(|(path, roots)| (path, roots, AbbreviationVisibility::Modelled, Vec::new()))
                .collect(),
        )
    }

    /// Build the env from referenced assemblies, with each assembly's
    /// [`AbbreviationVisibility`] and its assembly-level
    /// `[<assembly: AutoOpen("…")>]` path list
    /// (`EcmaView::assembly_auto_opens`, dotted paths in manifest order). An
    /// [`AbbreviationVisibility::Unknowable`] assembly's namespaces are
    /// recorded so type-position lookup can treat bare names under them as
    /// possibly shadowed by an abbreviation the entity tree does not witness;
    /// the AutoOpen paths feed [`Self::implicit_open_namespace_paths`].
    ///
    /// Assumes each assembly's F#-native extension index is **known** (its pickle
    /// decoded). The runtime path — where a pickle can be absent or undecodable —
    /// uses [`Self::from_assemblies_with_projection_knowability`], which also
    /// carries the per-assembly
    /// [`fsharp_extension_index_unknowable`](borzoi_assembly::AssemblyProjectionSkips::fsharp_extension_index_unknowable) bit.
    pub fn from_assemblies_with_abbreviation_visibility(
        assemblies: Vec<(PathBuf, Vec<Entity>, AbbreviationVisibility, Vec<String>)>,
    ) -> Self {
        Self::from_assemblies_with_projection_knowability(
            assemblies
                .into_iter()
                .map(|(path, roots, visibility, auto_opens)| {
                    // `false`/`false`: this constructor assumes an authoritative
                    // pickle (extension index known, F# signature authoritative).
                    // `None` identity: derived from the first root (this
                    // constructor's callers do not carry a rootless DLL's identity).
                    (path, roots, visibility, false, false, auto_opens, None)
                })
                .collect(),
        )
    }

    /// [`Self::from_assemblies_with_abbreviation_visibility`] plus, per assembly, its
    /// [`fsharp_extension_index_unknowable`](borzoi_assembly::AssemblyProjectionSkips::fsharp_extension_index_unknowable) bit (the `bool`)
    /// — folded into each entity's extension-knowability so a broken FSharp.Core
    /// pickle (abbreviation-exempt but extension-blind) still defers the name-keyed
    /// gate. The runtime env-build ([`crate`]'s LSP host) uses this; tests that don't
    /// exercise a broken pickle use the shorter constructor above.
    pub fn from_assemblies_with_projection_knowability(
        assemblies: Vec<AssemblyProjectionInput>,
    ) -> Self {
        let mut env = AssemblyEnv::default();
        let mut tagged: Vec<(
            Option<AssemblyId>,
            AbbreviationVisibility,
            bool,
            bool,
            Entity,
        )> = Vec::new();
        let mut auto_opens: Vec<(AssemblyId, String, Vec<String>)> = Vec::new();
        for (
            path,
            roots,
            visibility,
            extension_index_unknowable,
            signature_non_authoritative,
            raw_auto_opens,
            manifest_identity,
        ) in assemblies
        {
            let id = AssemblyId(
                u32::try_from(env.assemblies.len()).expect("more than u32::MAX assemblies"),
            );
            env.assemblies.push(Some(path));
            // Register the DLL's identity so a referenced-CCU name is counted per
            // loaded DLL. Prefer the caller-supplied `manifest_identity` (known to
            // its `EcmaView` even when the projection is rootless); fall back to the
            // first surviving root for callers that do not supply it. An
            // *unnameable* such assembly (rootless, no supplied identity) makes the
            // identity set incomplete — it could be any name — so uniqueness must
            // decline (see [`Self::assembly_identities_incomplete`]).
            let identity = manifest_identity.or_else(|| roots.first().map(|r| r.assembly.clone()));
            if identity.is_none() {
                env.assembly_identities_incomplete = true;
            }
            env.assembly_identities.push(identity);
            // The assembly's manifest identity name, for the FSharp.Core special
            // case — every root carries it. The deref itself keys on `id`.
            if let Some(first) = roots.first() {
                auto_opens.push((id, first.assembly.name.clone(), raw_auto_opens));
            } else if !raw_auto_opens.is_empty() {
                // An assembly whose types were **all** dropped still declares
                // `[<AutoOpen>]` targets — with no surviving roots we cannot resolve
                // them or even name the identity, and their extensions (if any) are
                // invisible, so the extension surface is unknowable (review, GPT-5.6).
                env.extension_surface_unknowable = true;
            }
            tagged.extend(roots.into_iter().map(move |r| {
                (
                    Some(id),
                    visibility,
                    extension_index_unknowable,
                    signature_non_authoritative,
                    r,
                )
            }));
        }
        env.index_roots(tagged);
        env.record_assembly_auto_opens(auto_opens);
        env
    }

    /// The name-indexing fold shared by [`Self::from_entities`] and
    /// [`Self::from_assemblies`]: intern each root (tagged with its assembly, if
    /// any) and key it for lookup.
    fn index_roots(
        &mut self,
        roots: impl IntoIterator<
            Item = (
                Option<AssemblyId>,
                AbbreviationVisibility,
                bool,
                bool,
                Entity,
            ),
        >,
    ) {
        // Index each entity by the name F# *source* uses to name it: a module-
        // suffix module (`ListModule`) is referenced as `List`, *never* by its
        // compiled name, so the IL name is not a source-lookup key. The suffix
        // exists because a same-named type clashed (`List` the type vs `List` the
        // module), and that type wins the bare name in type position — so index
        // entities that carry no source name (everything except suffixed modules)
        // first, then the suffixed modules under their source name with
        // `or_insert`, which only fills a `(namespace, name, arity)` slot the
        // first pass left free.
        //
        // Limitation: when the clashing type is *non-generic* (same arity 0 as the
        // module) it takes the shared slot and the module ends up with no key — a
        // value-position `Tagged.member` call then defers (sound — never a wrong
        // target). F# disambiguates type vs value position; a single-handle index
        // cannot, so that is a position-aware-index slice for later. The common
        // FSharp.Core companions (`List`/`Option`/`Map`/…) are generic, so their
        // arity-1+ type and arity-0 module never collide and both resolve.
        let mut source_named_type_keys: Vec<(TypeKey, EntityHandle)> = Vec::new();
        let mut source_named_module_keys: Vec<(TypeKey, EntityHandle)> = Vec::new();
        for (
            assembly,
            abbreviation_visibility,
            extension_index_unknowable,
            signature_non_authoritative,
            root,
        ) in roots
        {
            let namespace = root.namespace.clone();
            if abbreviation_visibility == AbbreviationVisibility::Unknowable
                && !self.unknowable_abbreviation_namespaces.contains(&namespace)
            {
                self.unknowable_abbreviation_namespaces
                    .insert(namespace.clone());
            }
            let arity = root.generic_parameters.len();
            // A *public* `[<AutoOpen>]` module is opened wherever its namespace
            // is — index it so the resolver can fold its members into unqualified
            // scope. An internal/private auto-open module is not accessible
            // cross-assembly, so it must not contribute (the open-type machinery
            // checks member accessibility, not the parent entity's).
            let is_auto_open_module = root.is_auto_open
                && root.kind == EntityKind::Module
                && root.access == Access::Public;
            // Source-named entities defer to the later passes so a same-named
            // plainly-named type wins the slot; within them, source-named
            // TYPES (a `[<CompiledName>]`-renamed type, an abbreviation
            // marker for a renamed abbreviation) key before source-named
            // MODULES (suffixed companions) — F# gives a type the bare name
            // and the companion module its suffix, regardless of which order
            // the entities appear in (codex round 5: a renamed abbreviation's
            // marker, appended after the ECMA roots, must still outrank its
            // suffixed `module` companion).
            let is_module = root.kind == EntityKind::Module;
            let (key, deferred) = match &root.source_name {
                Some(src) => ((namespace.clone(), src.clone(), arity), true),
                None => ((namespace.clone(), root.name.clone(), arity), false),
            };
            // Extensions are unknowable either because the whole F# signature is
            // ([`AbbreviationVisibility::Unknowable`]) or because the extension-member
            // overlay specifically could not be built (a decoded-but-absent pickle for
            // an assembly the abbreviation flag exempts — FSharp.Core; review, GPT-5.6).
            let extensions_unknowable = abbreviation_visibility
                == AbbreviationVisibility::Unknowable
                || extension_index_unknowable;
            let handle = self.intern(
                root,
                assembly,
                extensions_unknowable,
                signature_non_authoritative,
            );
            self.top_level_types.push(handle);
            self.types_by_namespace
                .entry(namespace.clone())
                .or_default()
                .push(handle);
            if !deferred {
                self.by_type.entry(key).or_insert(handle);
            } else if is_module {
                source_named_module_keys.push((key, handle));
            } else {
                source_named_type_keys.push((key, handle));
            }
            if is_auto_open_module {
                // FCS auto-opens RECURSIVELY (NameResolution.fs's
                // `AddModuleOrNamespaceRefsToNameEnv` is documented "Recursive
                // because of 'AutoOpen'"): a nested public `[<AutoOpen>]`
                // module of an auto-open module is opened by the same
                // namespace open. Register the whole transitive closure in
                // FCS's **depth-first pre-order** — FCS recurses into each
                // nested auto-open module before its next sibling, and
                // later-added contents win, so a later SIBLING outranks an
                // earlier sibling's descendant (codex on this change);
                // latest-wins lookup reproduces that when the statics are
                // pushed in the same order.
                let mut closure = Vec::new();
                let mut stack = vec![handle];
                while let Some(h) = stack.pop() {
                    closure.push(h);
                    // Reversed so the first qualifying child is processed
                    // (and pushed) before its later siblings.
                    for &child in self.children(h).iter().rev() {
                        let e = self.entity(child);
                        if e.is_auto_open
                            && e.kind == EntityKind::Module
                            && e.access == Access::Public
                        {
                            stack.push(child);
                        }
                    }
                }
                self.auto_open_modules
                    .entry(namespace)
                    .or_default()
                    .extend(closure);
            }
        }
        for (key, handle) in source_named_type_keys
            .into_iter()
            .chain(source_named_module_keys)
        {
            self.by_type.entry(key).or_insert(handle);
        }
    }

    /// Build the env from referenced-assembly views, enumerating each one's
    /// type definitions and reading each one's assembly-level AutoOpen list.
    /// Propagates the first [`ImportError`] from any view. Each view gets its
    /// own `AssemblyId` (with no source path — [`Self::assembly_path`] stays
    /// `None`) so the AutoOpen deref can tell same-named views apart.
    pub fn from_views<V: EcmaView>(views: &[V]) -> Result<Self, ImportError> {
        let mut env = AssemblyEnv::default();
        let mut tagged = Vec::new();
        let mut auto_opens = Vec::new();
        let mut dropped_namespaces = Vec::new();
        for view in views {
            let (roots, skips) = view.enumerate_type_defs_with_skips()?;
            let visibility = if skips.fsharp_abbreviations_unknowable {
                AbbreviationVisibility::Unknowable
            } else {
                AbbreviationVisibility::Modelled
            };
            // An F# assembly whose extension-member overlay could not be built has an
            // unread (not empty) extension index — the name-keyed gate must treat its
            // extensions as unknowable, even when abbreviations are exempt (FSharp.Core).
            let extension_index_unknowable = skips.fsharp_extension_index_unknowable;
            let signature_non_authoritative = skips.fsharp_signature_non_authoritative;
            // A **dropped** type may be a C#-style `[<Extension>]` class the entity
            // tree no longer shows, so its namespace is possibly extension-bearing
            // for the OV-6 gate (mirrors the LSP's separate construction path).
            dropped_namespaces.extend(skips.dropped_types.iter().map(|d| d.enclosing_namespace()));
            let id = AssemblyId(
                u32::try_from(env.assemblies.len()).expect("more than u32::MAX assemblies"),
            );
            env.assemblies.push(None);
            // Every view is a distinct loaded DLL with a known identity — register
            // it (even a rootless one) so a referenced-CCU name is counted per DLL.
            env.assembly_identities.push(Some(view.identity().clone()));
            tagged.extend(roots.into_iter().map(move |r| {
                (
                    Some(id),
                    visibility,
                    extension_index_unknowable,
                    signature_non_authoritative,
                    r,
                )
            }));
            auto_opens.push((
                id,
                view.identity().name.clone(),
                view.assembly_auto_opens()?,
            ));
        }
        env.index_roots(tagged);
        env.record_assembly_auto_opens(auto_opens);
        for namespace in dropped_namespaces {
            env.mark_namespace_dropped_type(namespace);
        }
        Ok(env)
    }

    /// Fold the referenced assemblies' assembly-level
    /// `[<assembly: AutoOpen("…")>]` lists into
    /// [`Self::implicit_open_namespace_paths`]. `per_assembly` is
    /// `(provenance id, manifest identity name, dotted paths in manifest
    /// order)`, in env assembly order — the order FCS applies the opens in
    /// (`CreateInitialTcEnv` folds `AddCcuToTcEnv` over the CCUs). The deref
    /// keys on the **provenance id**, never the name: two loaded DLLs can share
    /// a simple name, or a byte-identical manifest identity, and FCS resolves
    /// each path within the contributing CCU itself (issue #150). The name
    /// serves only the FSharp.Core special case.
    ///
    /// Mirrors FCS with two deliberate narrowings:
    /// - **FSharp.Core prepend** — FCS opens `Microsoft` for FSharp.Core even
    ///   though no manifest attribute says so (`AddCcuToTcEnv`'s fslib special
    ///   case, "Microsoft is opened by default in FSharp.Core"); reproduced
    ///   here keyed on the manifest identity name.
    /// - **Deref is per-assembly** — FCS resolves each path within the
    ///   *contributing* CCU and warns-and-skips when absent there
    ///   (`ApplyAssemblyLevelAutoOpenAttributeToTcEnv`); a stale attribute
    ///   must not open a namespace only some *other* referenced assembly
    ///   declares (codex P2 — that would make the other assembly's auto-open
    ///   modules bare-resolvable where FCS errors).
    /// - **Contested namespaces are dropped** (narrowing 1): FCS opens the
    ///   contributing CCU's namespace *entity* — a sibling assembly's
    ///   same-named namespace stays closed (fsi-verified twice: a stand-in
    ///   assembly's `[<AutoOpen>]` module under `Microsoft.FSharp.Core` does
    ///   not bare-resolve next to real FSharp.Core, and `Extensions.Logging.…`
    ///   is FS0039 even with `Microsoft.Extensions.Logging.Abstractions`
    ///   referenced — codex P2, round 3). The resolver's open machinery is
    ///   path-based (an open applies to every assembly declaring the path),
    ///   so a recorded entry is faithful exactly when **no sibling declares
    ///   the namespace** — then env-wide and contributor-scoped are
    ///   indistinguishable, for bare values, qualified shortening, and the
    ///   auto-open nested-type shadow alike. A contested entry is dropped
    ///   *entirely*: identical to the pre-S3 status quo for that assembly
    ///   (deferrals where FCS resolves, never a new wrong resolution).
    ///   Assembly-scoped namespace opens are the follow-up slice that lifts
    ///   this.
    /// - **Module/type-shaped paths are dropped** (narrowing 2): FCS opens
    ///   a module path like a module (that is how `IntrinsicOperators` /
    ///   `TaskBuilderExtensions.*Priority` work), but those modules' surface
    ///   is operators (not resolved until the A4/S4 demangle slice) and
    ///   *extension members*, which F# never makes bare-resolvable — and the
    ///   resolver's open-type machinery cannot yet tell an extension static
    ///   from a plain one (a generic F#-native extension member carries no
    ///   flag in the model). Opening them would make bare `Bind`/`Source`
    ///   wrongly resolve (a D5 soundness violation); skipping them only costs
    ///   deferrals. Revisit with A4/S4.
    fn record_assembly_auto_opens(&mut self, per_assembly: Vec<(AssemblyId, String, Vec<String>)>) {
        for (contributor, identity_name, raw) in per_assembly {
            let fslib_prepend = if identity_name == "FSharp.Core" {
                Some("Microsoft".to_string())
            } else {
                None
            };
            for path in fslib_prepend.into_iter().chain(raw) {
                // FCS `splitNamespace`: the attribute argument is a plain
                // dotted path (no quoted-identifier escaping at this level).
                let segments: Vec<String> = path.split('.').map(str::to_string).collect();
                if segments.iter().any(String::is_empty) {
                    continue;
                }
                if let Some(handle) = self.assembly_entity_at_path(contributor, &segments) {
                    // Module/type-shaped: dropped from the namespace opens (its
                    // bare values are not made resolvable), but retained here so the
                    // OV-6 extension-absence gate can fold its extension members.
                    self.auto_open_module_handles.push(handle);
                    continue;
                }
                if !self.namespace_declared_only_by(contributor, &segments) {
                    // A *contested* namespace (some assembly declares it, but not
                    // only the contributor) is dropped from the opens. Whether it can
                    // be recorded contributor-scoped turns on the **contributor**, not
                    // the env: FCS opens the *contributing CCU's* namespace entity, so
                    // only content that assembly visibly declares can enter scope.
                    if self.contributor_declares_namespace(contributor, &segments) {
                        // The contributor visibly declares the namespace (and so does a
                        // sibling — else `namespace_declared_only_by` would be true).
                        // Record it with its contributor so the extension gate can ask,
                        // per called name, whether *that assembly's* content there
                        // declares an extension of it (EX-1); a sibling's content never
                        // enters.
                        self.contested_auto_opens
                            .push((contributor, segments.clone()));
                    } else {
                        // The contributor declares **no visible content** at the target,
                        // yet the attribute names it. Either it is a stale/nonexistent
                        // path (FCS warns-and-skips), or the contributor's target was a
                        // module/type **dropped** during projection — whose extensions
                        // are then invisible to the gate, and whose drop marker sits in
                        // the *enclosing* namespace, so a contributor-scoped query at the
                        // exact path would prove every name absent. The two are
                        // indistinguishable (a sibling declaring the same namespace is
                        // irrelevant — FCS never imports it), so the extension surface is
                        // unknowable and the OV-6 gate must defer wholesale (review,
                        // GPT-5.6).
                        self.extension_surface_unknowable = true;
                    }
                    continue;
                }
                // Keep the LAST occurrence's position: FCS applies every
                // open in sequence, so a duplicate (repeated attribute, or
                // two assemblies auto-opening the same namespace)
                // re-establishes that namespace's latest-open precedence
                // (fsi-verified: `AutoOpen("A"); AutoOpen("B"); AutoOpen("A")`
                // binds an ambiguous bare name to A, not B — codex P2,
                // round 2).
                if let Some(existing) = self
                    .implicit_open_namespace_paths
                    .iter()
                    .position(|p| p == &segments)
                {
                    self.implicit_open_namespace_paths.remove(existing);
                }
                self.implicit_open_namespace_paths.push(segments);
            }
        }
    }

    /// Whether the assembly with provenance `contributor` names an entity
    /// at the fully-qualified dotted `path` — used to classify that assembly's
    /// AutoOpen path as module/type-shaped (some split of the path names one
    /// of *its* top-level types, possibly descending into nested types)
    /// versus namespace-shaped. Tries every namespace/type split because the
    /// path alone does not say where the namespace stops
    /// (`Microsoft.FSharp.Core.LanguagePrimitives.IntrinsicOperators` is
    /// namespace × type × nested-type). Arity 0 throughout: an AutoOpen path
    /// names a module, and modules are non-generic. Scans
    /// [`Self::top_level_types`] rather than [`Self::by_type`] because the
    /// deref must be **per-assembly** (see
    /// [`Self::record_assembly_auto_opens`]) and the first-wins `by_type`
    /// slot may hold a same-FQN entity from a different assembly. Candidates
    /// are filtered by [`Self::assembly_provenance`], never by name: a sibling
    /// DLL sharing the contributor's simple name — or its whole manifest
    /// identity — must not have *its* module folded (issue #150). Matched by
    /// the name F# source writes (`source_name` when present — a suffixed
    /// module's AutoOpen path says `List`, never `ListModule`).
    fn assembly_entity_at_path(
        &self,
        contributor: AssemblyId,
        path: &[String],
    ) -> Option<EntityHandle> {
        for split in 0..path.len() {
            let candidates = self.top_level_types.iter().copied().filter(|&h| {
                let e = self.entity(h);
                self.assembly_provenance(h) == Some(contributor)
                    && e.namespace.as_slice() == &path[..split]
                    && e.source_name.as_deref().unwrap_or(&e.name) == path[split]
                    && e.generic_parameters.is_empty()
            });
            for top in candidates {
                let mut handle = top;
                let mut ok = true;
                for segment in &path[split + 1..] {
                    // Nested descent stays within `top`'s assembly by
                    // construction (children were interned from its subtree).
                    match self.nested(handle, segment, 0) {
                        Some(child) => handle = child,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    return Some(handle);
                }
            }
        }
        None
    }

    /// Resolve an abbreviation target's dotted path to a top-level-then-nested
    /// entity, selecting the owning DLL by `assembly_matches` (over an entity
    /// handle, so a caller can key on per-DLL provenance). Three things set it
    /// apart from the source-domain [`Self::assembly_entity_at_path`] (which the
    /// AutoOpen deref uses), and each is why abbreviation targets need their own
    /// resolver:
    ///
    /// - **DLL by handle predicate, not simple name.** A proven same-CCU target is
    ///   pinned to the marker's own DLL by [`AssemblyKey`]; a manifest identity
    ///   (let alone a simple name) can collide with a byte-identical
    ///   duplicate-reference sibling (issue #150).
    /// - **Logical-name matching at every segment.** The pickle path is in the
    ///   *logical* name domain, so each segment is matched against the IL `name`
    ///   (a `[<CompilationRepresentation(ModuleSuffix)>]` module contributes its
    ///   suffixed `FooModule`) **or** the source name (a `[<CompiledName>]` type
    ///   contributes its source name) — top-level split *and* nested descent alike.
    ///   [`Self::nested`] is deliberately source-domain (it never matches a
    ///   suffixed module's compiled name, which F# source never writes), so it
    ///   cannot serve this path.
    /// - **Type-over-module at the terminal.** The final segment names the target
    ///   *type*, so it must not bind a ModuleSuffix companion module that shares
    ///   the type's source name (see `terminal_matches` below).
    /// - **Unique-or-decline.** Returns a target only when exactly one entity
    ///   matches; two distinct matches decline (see `found` below).
    fn abbreviation_target_at_path(
        &self,
        path: &[String],
        assembly_matches: impl Fn(EntityHandle) -> bool,
    ) -> Option<EntityHandle> {
        let last = path.len().checked_sub(1)?; // an empty path names nothing
        // A nullary entity whose logical name — its IL `name` or its source name —
        // is `seg`. Every entity on the target path must also be cross-assembly
        // **public**: F# permits a public abbreviation of an internal/private type
        // in the declaring library (FS0044), but a *consumer* accessing a member
        // through the alias is rejected (FS0491), so an inaccessible entity on the
        // path declines the target (codex review 5). Used for the *container*
        // segments a multi-segment path descends through.
        let seg_matches = |e: &Entity, seg: &str| {
            e.generic_parameters.is_empty()
                && (e.name == seg || e.source_name.as_deref() == Some(seg))
        };
        // The **terminal** segment names the target *type*, so it must not bind a
        // `[<CompilationRepresentation(ModuleSuffix)>]` companion module that
        // shares the type's source name: `type Outer = Mid` names the type `Mid`,
        // and FCS gives type-over-module precedence, so the chase reaches `Mid`'s
        // abbreviation marker (→ its own target), never the companion module's
        // members (which FCS rejects on the expansion, FS0039). Intermediate
        // container segments may legitimately be modules, so only the terminal is
        // constrained (codex review).
        let terminal_matches =
            |e: &Entity, seg: &str| seg_matches(e, seg) && e.kind != EntityKind::Module;
        // Resolve to a target only when it is **uniquely** determined. Two
        // distinct entities matching the same (assembly-scope, path) — a
        // namespace/type split that is ambiguous, or two indistinguishable
        // same-path candidates in an env whose `assembly_matches` cannot tell
        // them apart (`from_entities`, keyed by manifest identity, is the only
        // path where that is possible) — cannot be disambiguated, so decline
        // rather than return the first (correctness over availability, codex P2).
        let mut found: Option<EntityHandle> = None;
        for split in 0..path.len() {
            let candidates = self.top_level_types.iter().copied().filter(|&h| {
                let e = self.entity(h);
                self.is_public(h)
                    && assembly_matches(h)
                    && e.namespace.as_slice() == &path[..split]
                    && if split == last {
                        terminal_matches(e, &path[split])
                    } else {
                        seg_matches(e, &path[split])
                    }
            });
            for top in candidates {
                let mut handle = top;
                let mut ok = true;
                for (offset, segment) in path[split + 1..].iter().enumerate() {
                    // Descend by the logical name too (children were interned from
                    // `top`'s subtree, so this stays within its assembly). The last
                    // descended segment is the terminal (type-only, no companion
                    // module); the rest are containers. A non-public entity anywhere
                    // on the path declines.
                    let is_terminal = split + 1 + offset == last;
                    match self.children(handle).iter().copied().find(|&c| {
                        self.is_public(c) && {
                            let e = self.entity(c);
                            if is_terminal {
                                terminal_matches(e, segment)
                            } else {
                                seg_matches(e, segment)
                            }
                        }
                    }) {
                        Some(child) => handle = child,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    match found {
                        None => found = Some(handle),
                        // A second, distinct match: ambiguous — decline.
                        Some(prev) if prev != handle => return None,
                        Some(_) => {}
                    }
                }
            }
        }
        found
    }

    /// Whether the assembly with provenance `contributor` declares a
    /// **public** top-level type in `namespace` (or a nested namespace below
    /// it) — the per-assembly counterpart of [`Self::has_namespace`] — while
    /// **no other assembly** in the env does. Both halves serve the AutoOpen
    /// deref (see [`Self::record_assembly_auto_opens`]): the contributor half
    /// is FCS's own per-CCU deref; the no-sibling half is what makes the
    /// resolver's path-based (assembly-blind) open machinery equivalent to
    /// FCS's contributor-scoped open for this namespace. Sibling-ness is
    /// provenance, not name: a same-named (even identically-manifested)
    /// sibling DLL is still a sibling, and counting its content as the
    /// contributor's would record a contested open env-wide (issue #150).
    /// Public-only on both sides: internal types are not cross-assembly
    /// reachable, so a sibling with only internal declarations cannot be
    /// distinguished through any lookup the resolver performs.
    fn namespace_declared_only_by(&self, contributor: AssemblyId, namespace: &[String]) -> bool {
        let mut contributor_declares = false;
        for &h in &self.top_level_types {
            let e = self.entity(h);
            if !e.namespace.starts_with(namespace) || !self.is_public(h) {
                continue;
            }
            if self.assembly_provenance(h) == Some(contributor) {
                contributor_declares = true;
            } else {
                return false;
            }
        }
        contributor_declares
    }

    /// Whether the assembly with provenance `contributor` declares a **public**
    /// top-level type in `namespace` (or a nested namespace below it) — the
    /// contributor half of [`Self::namespace_declared_only_by`], asked on its own
    /// when a *sibling* is known to declare the namespace too. This decides whether a
    /// contested auto-open can be recorded contributor-scoped: FCS opens the
    /// *contributing CCU's* namespace entity, so a target the contributor does not
    /// visibly declare (stale, or **dropped** during projection) is projection-unknown
    /// — never made visible by a sibling's same-named namespace (see
    /// [`Self::record_assembly_auto_opens`]). Public-only for the same reason as
    /// [`Self::namespace_declared_only_by`]: an internal declaration is not
    /// cross-assembly reachable.
    fn contributor_declares_namespace(
        &self,
        contributor: AssemblyId,
        namespace: &[String],
    ) -> bool {
        self.top_level_types.iter().any(|&h| {
            self.assembly_provenance(h) == Some(contributor)
                && self.is_public(h)
                && self.entity(h).namespace.starts_with(namespace)
        })
    }

    /// The namespace paths the referenced assemblies implicitly open via
    /// assembly-level `[<assembly: AutoOpen("…")>]`, in application order —
    /// see `record_assembly_auto_opens` (private, above) for what is (and is
    /// not) included. This is the *manifest-derived* set; the resolver and the
    /// extension gate both open the wider
    /// [`Self::effective_implicit_open_namespace_paths`], which appends the
    /// hardcoded FSharp.Core fallback.
    pub fn implicit_open_namespace_paths(&self) -> &[Vec<String>] {
        &self.implicit_open_namespace_paths
    }

    /// FCS's hardcoded FSharp.Core implicit-open fallback — the namespaces the
    /// compiler opens in every file even when the referenced FSharp.Core omits the
    /// assembly-level `[<AutoOpen>]` attributes (an old build, or a stand-in).
    /// Appended to the manifest-derived set by
    /// [`Self::effective_implicit_open_namespace_paths`]; a real FSharp.Core's
    /// manifest set is already a superset, so it is deduped away there.
    const FSHARP_CORE_IMPLICIT_OPEN_FALLBACK: [&str; 3] = [
        "Microsoft.FSharp.Core",
        "Microsoft.FSharp.Collections",
        "Microsoft.FSharp.Control",
    ];

    /// The **effective** implicit-open namespace set: the manifest-derived
    /// [`Self::implicit_open_namespace_paths`] followed by the
    /// `FSHARP_CORE_IMPLICIT_OPEN_FALLBACK` (deduped, in FCS's order). This
    /// is the single source of truth for *what the resolver actually opens*, so both
    /// the resolver's implicit-open seed (`resolve::state::implicit_open_namespaces`)
    /// and the extension gate (`extension_named_in_scope`) read it — the namespaces
    /// the gate proves a name absent from are then exactly the ones the resolver
    /// opens. Without the fallback, an old/stand-in FSharp.Core (no manifest
    /// auto-opens) still has these three opened, and an `[<AutoOpen>]` extension in
    /// one of them would be missed (review, GPT-5.6).
    ///
    /// Computed on the fly (the fallback is three namespaces, appended to the
    /// manifest set) so **every** construction path — including the derived
    /// [`Default`] empty env — reports the same set with no cache to keep in sync.
    /// The per-call cost is negligible now that the hot per-namespace queries hit
    /// the `types_by_namespace` index rather than scanning all types.
    pub fn effective_implicit_open_namespace_paths(&self) -> Vec<Vec<String>> {
        let mut out = self.implicit_open_namespace_paths.clone();
        for ns in Self::FSHARP_CORE_IMPLICIT_OPEN_FALLBACK {
            let segments: Vec<String> = ns.split('.').map(str::to_string).collect();
            if !out.contains(&segments) {
                out.push(segments);
            }
        }
        out
    }

    /// Intern `entity` and its nested-type subtree depth-first, returning the
    /// entity's handle. `nested_types` is moved out of the stored entity so it
    /// is not duplicated; navigation goes through the returned child handles.
    fn intern(
        &mut self,
        mut entity: Entity,
        assembly: Option<AssemblyId>,
        extensions_unknowable: bool,
        signature_non_authoritative: bool,
    ) -> EntityHandle {
        let nested = std::mem::take(&mut entity.nested_types);
        let owning_namespace = entity.namespace.clone();
        let handle = EntityHandle::new(self.nodes.len());
        self.nodes.push(EntityNode {
            entity,
            children: Vec::new(),
            assembly,
            extensions_unknowable,
            signature_non_authoritative,
            owning_namespace: owning_namespace.clone(),
        });
        // Nested types belong to the same assembly as their encloser — and share its
        // extension-index (un)knowability, its F#-signature authority, and its top-level
        // namespace (a nested TypeDef declares none of its own).
        let children: Vec<EntityHandle> = nested
            .into_iter()
            .map(|n| {
                let child = self.intern(
                    n,
                    assembly,
                    extensions_unknowable,
                    signature_non_authoritative,
                );
                self.nodes[child.index()].owning_namespace = owning_namespace.clone();
                self.propagate_owning_namespace(child, &owning_namespace);
                child
            })
            .collect();
        self.nodes[handle.index()].children = children;
        handle
    }

    /// Stamp `namespace` on every node of `handle`'s subtree — a nested TypeDef has no
    /// namespace of its own, and consumers that must ask a namespace-keyed question of
    /// it (dropped types) need the top-level encloser's.
    fn propagate_owning_namespace(&mut self, handle: EntityHandle, namespace: &[String]) {
        let children = self.nodes[handle.index()].children.clone();
        for child in children {
            self.nodes[child.index()].owning_namespace = namespace.to_vec();
            self.propagate_owning_namespace(child, namespace);
        }
    }

    /// The per-DLL provenance of the entity `handle` — which loaded assembly it
    /// was interned from ([`Self::from_assemblies`] / [`Self::from_views`]);
    /// `None` for an env built without per-assembly grouping
    /// ([`Self::from_entities`]). The true per-DLL discriminator: unlike a
    /// simple name or even a whole manifest `AssemblyIdentity`, two loaded DLLs
    /// never share it (issue #150). A nested type reports its enclosing
    /// assembly's.
    fn assembly_provenance(&self, handle: EntityHandle) -> Option<AssemblyId> {
        self.nodes[handle.index()].assembly
    }

    /// The [`AssemblyKey`] of `handle` — which loaded DLL it belongs to, for
    /// same-assembly comparison. Per-DLL provenance when the env was built
    /// per-assembly ([`Self::from_assemblies`] / [`Self::from_views`]); the
    /// manifest identity as a fallback for [`Self::from_entities`]. See
    /// [`AssemblyKey`] for why provenance is the strict discriminator.
    fn assembly_key(&self, handle: EntityHandle) -> AssemblyKey<'_> {
        match self.assembly_provenance(handle) {
            Some(id) => AssemblyKey::Provenance(id),
            None => AssemblyKey::Identity(&self.entity(handle).assembly),
        }
    }

    /// The source DLL path the entity `handle` came from — `Some` only when the
    /// env was built with source paths ([`Self::from_assemblies`]); `None` for one
    /// built via [`Self::from_entities`] / [`Self::from_views`]. A nested type
    /// reports its enclosing assembly. Go-to-definition reads this DLL's portable
    /// PDB for the member's source location.
    pub fn assembly_path(&self, handle: EntityHandle) -> Option<&Path> {
        self.assembly_provenance(handle)
            .and_then(|id| self.assemblies.get(id.0 as usize))
            .and_then(|p| p.as_deref())
    }

    /// The total number of interned entities (top-level + nested).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Resolve a top-level type by its namespace, simple name, and generic
    /// arity (the number of type arguments; `0` for a non-generic type). `None`
    /// if no referenced assembly declares that type at that arity — never a
    /// guess (D5). Arity is required because `` List`1 `` and a hypothetical
    /// non-generic `List` are distinct types under one `(namespace, name)`.
    pub fn lookup_type(
        &self,
        namespace: &[String],
        name: &str,
        arity: usize,
    ) -> Option<EntityHandle> {
        self.by_type
            .get(&(namespace.to_vec(), name.to_string(), arity))
            .copied()
    }

    /// Whether `namespace` is a namespace any referenced assembly declares with a
    /// **cross-assembly-accessible** (public) type — a public top-level type lives
    /// in it (`["Demo", "Sub"]`) **or** in a nested namespace below it (`["Demo"]`,
    /// a parent of `Demo.Sub`). Empty `namespace` is the global namespace, always
    /// present (so callers should special-case it rather than ask).
    ///
    /// The **public** filter matches the accessibility the rest of resolution
    /// enforces ([`Self::lookup_type`] + [`Self::is_public`]): a namespace
    /// reachable only through `internal`/`private` types is not one F# can open or
    /// resolve through from another assembly, so it must not drive
    /// canonicalisation — else a relative `open Sub` would be rewritten to a
    /// `Demo.Sub` that holds nothing accessible, shadowing a public root `Sub`.
    /// Used to canonicalise a possibly-relative `open` against the enclosing
    /// namespaces: a relative `open Sub` in `namespace Demo` names `Demo.Sub`
    /// exactly when this returns `true` for it. O(types); opens are rare, so no
    /// dedicated namespace index yet.
    ///
    /// Scans the **full top-level set**, not the first-wins `by_type` index — the same
    /// reason [`Self::public_types_named`] does (review round 15). `by_type`
    /// keeps one handle per `(namespace, name, arity)`, so an *inaccessible* type that
    /// happened to be enumerated first hides a **public** same-keyed type from another
    /// assembly. Asking `by_type` would then answer "no public namespace here" for a
    /// namespace F# can plainly open — and the module-open cut reads this to decide
    /// whether a path is a cross-kind merge, so a false `false` there commits a
    /// *definite target* for a name the namespace half may contest.
    pub fn has_namespace(&self, namespace: &[String]) -> bool {
        self.top_level_types.iter().any(|&handle| {
            self.is_public(handle) && self.entity(handle).namespace.starts_with(namespace)
        })
    }

    /// The `(kind, is_struct)` of every **public** top-level type named `name`
    /// directly in `namespace`, across all generic arities. The head-slot
    /// eviction check (`docs/head-slot-assembly-eviction-plan.md`) consults
    /// this: a class/struct/enum brought into scope by `open <namespace>`
    /// enters FCS's unqualified-name slot and evicts a same-named local value.
    /// Arity-agnostic because a bare head with no type args matches any arity
    /// in FCS (`LookupTypeNameInEnvNoArity`) — a generic-only `Color<'T>`
    /// evicts a bare `Color` too (probe Ageneric). Only *public* types are
    /// cross-assembly reachable and so can occupy the slot. O(types); opens
    /// are rare, mirroring [`Self::has_namespace`].
    pub fn public_types_named(&self, namespace: &[String], name: &str) -> Vec<(EntityKind, bool)> {
        let mut out = Vec::new();
        // Scan the full top-level set, not `by_type` — a colliding
        // `(namespace, name, arity)` keeps only its first handle there, so a
        // constructible class indexed *after* a non-constructible collision
        // would be invisible (codex round 3). Match the **source** name F#
        // writes (a suffixed module is keyed as `List`, not `ListModule`),
        // consistent with `by_type`.
        for &handle in &self.top_level_types {
            let e = self.entity(handle);
            let src = e.source_name.as_deref().unwrap_or(e.name.as_str());
            if e.namespace.as_slice() == namespace && src == name && self.is_public(handle) {
                out.push((e.kind, e.is_struct));
            }
        }
        out
    }

    /// The public nested **module** named `name` (by its F# *source* name — a suffixed
    /// companion module compiles to `TaggedModule` but is written `Tagged`).
    ///
    /// [`Self::nested`] deliberately prefers the *type* when a type and its companion
    /// module share a name (`type Tagged` + `module Tagged`), which is right for a
    /// type-position lookup and wrong for an `open`: `open Demo.Outer.Tagged` imports
    /// the **module**'s values (fsi-verified against the fixture), so a module-path walk
    /// needs this (review, Slice A round 6).
    pub fn nested_module(&self, handle: EntityHandle, name: &str) -> Option<EntityHandle> {
        self.children(handle).iter().copied().find(|&child| {
            let e = self.entity(child);
            e.kind == EntityKind::Module
                && e.source_name.as_deref().unwrap_or(e.name.as_str()) == name
                && self.is_public(child)
        })
    }

    /// **Every** public top-level entity at `(namespace, name)` — not just the first.
    ///
    /// Two referenced assemblies may expose the same FQN, and [`Self::lookup_type`]'s
    /// index is first-wins. FCS **merges** them: with both assemblies referenced,
    /// `open Dup.M` imports the unique values of each, and a colliding name binds the
    /// *later-referenced* one (fsi-verified with two probe libraries). A consumer that
    /// took only the first handle would silently lose the other's values and could bind
    /// a collision to the wrong assembly — a wrong target (review, Slice A round 5).
    /// Scans the full top-level set, exactly as [`Self::public_types_named`] does and
    /// for the same reason.
    pub fn public_entities_named(&self, namespace: &[String], name: &str) -> Vec<EntityHandle> {
        // The exact-namespace bucket is the same population as a
        // `top_level_types` scan filtered to `namespace` — indexed, because
        // callers run this per path split (the attribute uncertainty scan per
        // attribute × prefix × split; `opened_assembly_modules` per split).
        self.types_in_namespace(namespace)
            .iter()
            .copied()
            .filter(|&handle| self.entity_source_name(handle) == name && self.is_public(handle))
            .collect()
    }

    /// [`Self::public_entities_named`] without the accessibility filter: every
    /// top-level entity at `(namespace, name)`, public or not. The attribute
    /// uncertainty scan's occupancy check — FCS resolves an *inaccessible*
    /// suffixed candidate and reports accessibility errors rather than falling
    /// through to the written one, so an internal occupant must defer the
    /// candidate, not read as a clean miss (codex round 4 on EX-3 §2(d)).
    pub(crate) fn entities_named(&self, namespace: &[String], name: &str) -> Vec<EntityHandle> {
        self.types_in_namespace(namespace)
            .iter()
            .copied()
            .filter(|&handle| self.entity_source_name(handle) == name)
            .collect()
    }

    /// Whether a **retained manifest auto-open surface** could supply an entity
    /// named `name` into bare scope — the surfaces
    /// [`Self::record_assembly_auto_opens`] keeps *out* of
    /// [`Self::effective_implicit_open_namespace_paths`] (so no
    /// namespace-prefix walk ever searches them):
    ///
    /// - a **contested** auto-open, applied by FCS contributor-scoped: only the
    ///   contributing assembly's content in that namespace enters scope — its
    ///   types, and its auto-open modules' trees (a dropped type in the
    ///   namespace answers `true` for every name: we cannot see what it was);
    /// - a **module/type-shaped** target ([`Self::auto_open_module_handles`]):
    ///   its tree's entities are bare-visible, and a dropped type in its owning
    ///   namespace could be a hidden one.
    ///
    /// Deferral-only for the attribute resolution (EX-3 §2(d)): a `true` here
    /// never resolves anything, it only withholds a commitment that a modeled
    /// lower tier would otherwise win against FCS's higher-priority retained
    /// surface (doom-loop round 6, refound by codex on stage 3).
    ///
    /// When the surface is **globally unknowable** — an assembly's `AutoOpen`
    /// list could not be read, or a whole projection was skipped
    /// ([`Self::mark_extension_surface_unknowable`]) — an *unseen* auto-open
    /// could supply any name at higher priority, so every candidate is
    /// unrulable (codex round 5).
    pub(crate) fn retained_auto_open_could_supply_entity_named(&self, name: &str) -> bool {
        if self.extension_surface_unknowable {
            return true;
        }
        // Per-surface uncertainty: a dropped type could be anything, and an
        // assembly whose signature pickle is undecodable
        // ([`AbbreviationVisibility::Unknowable`]) hides its module-scoped
        // type aliases from the tree walked below — FCS still imports them
        // through the auto-open (codex on stage 4).
        if self.contested_auto_opens.iter().any(|(contributor, ns)| {
            self.namespace_has_dropped_type(ns)
                || self.unknowable_abbreviations_in_namespace(ns)
                || self.types_in_namespace(ns).iter().any(|&h| {
                    self.assembly_provenance(h) == Some(*contributor)
                        && (self.entity_source_name(h) == name
                            || self.entity_tree_has_entity_named(h, name))
                })
        }) {
            return true;
        }
        self.auto_open_module_handles.iter().any(|&h| {
            let owning = &self.nodes[h.index()].owning_namespace;
            self.namespace_has_dropped_type(owning)
                || self.unknowable_abbreviations_in_namespace(owning)
                || self.entity_tree_has_entity_named(h, name)
        })
    }

    /// Whether `handle` **is** `System.Runtime.CompilerServices.ExtensionAttribute`
    /// — matched by metadata identity (namespace + name), in *any* referenced
    /// assembly's copy, exactly as FCS recognises the marker. The gate's
    /// question for a committed attribute resolution (EX-3 §2(d) stage 5): an
    /// attribute resolving to this type makes its carrier an extension
    /// declaration; one resolving to any other concrete type provably does not.
    pub fn is_extension_attribute(&self, handle: EntityHandle) -> bool {
        let e = self.entity(handle);
        e.name == "ExtensionAttribute"
            && e.namespace.len() == 3
            && e.namespace[0] == "System"
            && e.namespace[1] == "Runtime"
            && e.namespace[2] == "CompilerServices"
    }

    /// The source (else IL) simple name of `handle`.
    fn entity_source_name(&self, handle: EntityHandle) -> &str {
        let e = self.entity(handle);
        e.source_name.as_deref().unwrap_or(e.name.as_str())
    }

    /// Whether any entity in the tree **under** `handle` (children at any
    /// depth; not `handle` itself) is named `name`. Deliberately deeper than
    /// what FCS makes bare-visible (a type nested in a non-auto-open child is
    /// not) — an over-approximation that only defers.
    fn entity_tree_has_entity_named(&self, handle: EntityHandle, name: &str) -> bool {
        self.children(handle).iter().any(|&child| {
            self.entity_source_name(child) == name || self.entity_tree_has_entity_named(child, name)
        })
    }

    /// The entity a handle names. Its `nested_types` is empty (moved out during
    /// interning); use [`Self::children`] / [`Self::nested`] for nesting.
    pub fn entity(&self, handle: EntityHandle) -> &Entity {
        &self.nodes[handle.index()].entity
    }

    /// The [`SemanticClass`] of a referenced-assembly entity, or `None` where we
    /// decline: an F# `module` reads as [`SemanticClass::Module`] and every plain
    /// type kind (class, struct, interface, enum, union, record, delegate,
    /// abbreviation, measure) as a [`SemanticClass::Type`] — the head a qualified
    /// path (`System.Console`, `Demo.Calc`) roots at.
    ///
    /// An **exception** entity is declined. F# resolves *both* a constructor use
    /// (`raise (Boom …)`) and a type-position use (`:? Boom`) of a referenced
    /// exception to the same `Resolution::Entity`, which carries no occurrence
    /// role; a same-file exception constructor is an `enumMember` while the type
    /// is a `type`, so committing either here would mis-colour the other. Declining
    /// (under-colour, never mis-colour) is the honest answer until the resolution
    /// distinguishes the two.
    /// Whether `handle`'s source assembly has a **non-authoritative** F#
    /// signature — its pickle is absent/undecodable, or it embeds foreign CCU
    /// pickles (an `fsc --standalone` build). In that state fsc's IL-level F#
    /// markers (`CompilationMappingAttribute` for module kind, the static-property
    /// shape for a module value) survive but the pickle overlay never ran, so they
    /// are *heuristic, not authoritative* — and FCS imports such an assembly
    /// through IL, where a module reads as a plain type and its `let`s as ordinary
    /// members (verified against real `--standalone` output). A classifier must
    /// therefore not trust `EntityKind::Module` (or the module-member split) here.
    ///
    /// The entity's [`EntityNode::signature_non_authoritative`] bit — carried on
    /// every build path (including [`Self::from_views`], which tags no
    /// [`AssemblyId`]), so a non-authoritative assembly's module classifications
    /// decline regardless of how the env was built.
    fn fsharp_signature_unreliable(&self, handle: EntityHandle) -> bool {
        self.nodes[handle.index()].signature_non_authoritative
    }

    pub fn entity_class(&self, handle: EntityHandle) -> Option<SemanticClass> {
        match self.entity(handle).kind {
            // A module's kind is trustworthy only from an authoritative F# pickle;
            // a non-authoritative assembly's `Module` is an IL heuristic FCS does
            // not share (it imports the type through IL), so decline rather than
            // mis-colour it a namespace. See [`Self::fsharp_signature_unreliable`].
            EntityKind::Module if self.fsharp_signature_unreliable(handle) => None,
            EntityKind::Module => Some(SemanticClass::Module),
            EntityKind::Class
            | EntityKind::Struct
            | EntityKind::Interface
            | EntityKind::Enum
            | EntityKind::Delegate
            | EntityKind::Union
            | EntityKind::Record
            | EntityKind::Abbreviation
            | EntityKind::Measure => Some(SemanticClass::Type),
            EntityKind::Exception => None,
        }
    }

    /// The [`SemanticClass`] of a referenced-assembly member — read with its
    /// **parent** entity's kind, which changes the answer:
    ///
    /// - a member of an F# `module` is a `let`, not a C#-style method: a module
    ///   *value* (`let x = …`) is a [`SemanticClass::Value`], while a module
    ///   *function* (`let f x = …`) is a [`SemanticClass::Function`]. The value
    ///   case is either the rebranded 0-parameter property tagged
    ///   [`MethodLike::module_value`] or a generic value (`typeof<'T>`) fsc emits
    ///   as a generic method, flagged [`MethodLike::is_module_value_binding`] —
    ///   both are values; only a member with ≥1 argument group is a function. A
    ///   method of any *other* entity (a class, struct, …) is a genuine
    ///   [`SemanticClass::Method`].
    /// - a field of an `enum` is one of its cases → [`SemanticClass::EnumCase`];
    ///   a field of a `module` is a `[<Literal>] let` (fsc emits the literal as a
    ///   static field, which the pickle merge claims) → [`SemanticClass::Value`],
    ///   matching FCS's `IsValue`; a field of anything else is data →
    ///   [`SemanticClass::Property`] (the standard token legend has no distinct field).
    /// - a property is a [`SemanticClass::Property`]; an event a
    ///   [`SemanticClass::Event`].
    ///
    /// Returns `None` (declines) for a member of a `module` whose assembly has a
    /// non-authoritative F# signature (`fsharp_signature_unreliable`): its
    /// module-member split is an IL heuristic FCS does not share — FCS imports such
    /// an assembly through IL, where the `let`s are ordinary members — so a
    /// committed `Value`/`Function` would mis-colour them. Only the module case is
    /// gated; a class/struct member is projected the same way FCS imports it.
    pub fn member_class(&self, handle: EntityHandle, idx: MemberIndex) -> Option<SemanticClass> {
        let parent_kind = self.entity(handle).kind;
        if parent_kind == EntityKind::Module && self.fsharp_signature_unreliable(handle) {
            return None;
        }
        Some(match self.member_at(handle, idx) {
            Member::Method(m) => match parent_kind {
                // An F# module `let` is a value when it takes zero argument groups —
                // whether fsc emitted it as a rebranded property (`module_value`) or,
                // for a generic value like `typeof<'T>`, a generic method
                // (`is_module_value_binding`, which the property flag can't cover). A
                // method with ≥1 argument group is a genuine module function.
                EntityKind::Module if m.module_value.is_some() || m.is_module_value_binding => {
                    SemanticClass::Value
                }
                EntityKind::Module => SemanticClass::Function,
                _ => SemanticClass::Method,
            },
            Member::Field(_) => match parent_kind {
                EntityKind::Enum => SemanticClass::EnumCase,
                // A field on an F# `module` is a `[<Literal>] let` — fsc emits it as a
                // static literal field (not an accessor method), and the F# pickle merge
                // claims it into the module's member list. FCS classifies a use of it as
                // a module *value* (`IsValue`), the same as any other module `let`, so
                // it must not fall through to `Property` like a class's data field.
                EntityKind::Module => SemanticClass::Value,
                _ => SemanticClass::Property,
            },
            Member::Property(_) => SemanticClass::Property,
            Member::Event(_) => SemanticClass::Event,
        })
    }

    /// The F# source full name of `handle`: namespace segments, enclosing
    /// entity names for nested types, and the entity's source name when metadata
    /// records one (`List`, not `ListModule`).
    pub fn entity_full_name(&self, handle: EntityHandle) -> String {
        let mut segments = Vec::new();
        if self.push_entity_full_name(handle, &mut segments) {
            return segments.join(".");
        }

        let entity = self.entity(handle);
        segments.extend(entity.namespace.iter().cloned());
        segments.push(
            entity
                .source_name
                .as_deref()
                .unwrap_or(&entity.name)
                .to_string(),
        );
        segments.join(".")
    }

    fn push_entity_full_name(&self, target: EntityHandle, segments: &mut Vec<String>) -> bool {
        for &root in &self.top_level_types {
            if self.push_entity_full_name_from(root, target, true, segments) {
                return true;
            }
        }
        false
    }

    fn push_entity_full_name_from(
        &self,
        current: EntityHandle,
        target: EntityHandle,
        include_namespace: bool,
        segments: &mut Vec<String>,
    ) -> bool {
        let before = segments.len();
        let entity = self.entity(current);
        if include_namespace {
            segments.extend(entity.namespace.iter().cloned());
        }
        segments.push(
            entity
                .source_name
                .as_deref()
                .unwrap_or(&entity.name)
                .to_string(),
        );
        if current == target {
            return true;
        }
        for &child in self.children(current) {
            if self.push_entity_full_name_from(child, target, false, segments) {
                return true;
            }
        }
        segments.truncate(before);
        false
    }

    /// Whether the entity is `public` — the accessibility a *cross-assembly*
    /// reference requires (internal/private types are not reachable from
    /// another assembly via a qualified path).
    pub fn is_public(&self, handle: EntityHandle) -> bool {
        self.entity(handle).access == Access::Public
    }

    /// Whether the entity is an F# **module** (compiled to a class carrying
    /// `CompilationMappingAttribute(SourceConstructFlags.Module)`). A plain
    /// `open` imports an F# module's contents but not a plain class's statics, so
    /// the distinction matters for classifying `open <path>`.
    pub fn is_module(&self, handle: EntityHandle) -> bool {
        self.entity(handle).kind == EntityKind::Module
    }

    /// Whether the entity is `[<RequireQualifiedAccess>]`. Opening such a module is
    /// an **error** in FCS (FS0892 — "This declaration opens the module …, which is
    /// marked as 'RequireQualifiedAccess'") and imports nothing, so an `open` of it
    /// contributes no bare names (fsi-verified; `docs/assembly-module-open-plan.md`
    /// Q5).
    pub fn is_require_qualified_access(&self, handle: EntityHandle) -> bool {
        self.entity(handle).is_require_qualified_access
    }

    /// Whether an `open` of `handle` could hide a **nested module** from us — so a later
    /// `open Sub` shortening through it may name a module we cannot see, which FCS binds
    /// at a *higher* priority than any root `Sub` (review rounds 9/10).
    ///
    /// Only two signals can hide a whole nested module: a **dropped type** in the owning
    /// namespace (the drop may *be* that module), and an **unknowable pickle**. A hidden
    /// union case or auto-open child cannot conjure a nested module path.
    pub fn module_may_hide_nested_modules(&self, handle: EntityHandle) -> bool {
        matches!(
            self.module_extension_members(handle),
            ExtensionMembers::Unknowable
        ) || self.namespace_has_dropped_type(&self.nodes[handle.index()].owning_namespace)
    }

    /// The complete-or-opaque bare-name surface an `open <handle>` folds into
    /// scope — see [`OpenFoldSurface`]. Entries come out in FCS's fold order
    /// (`AddModuleOrNamespaceContentsToNameEnv`):
    ///
    /// 1. **exception constructors** (both spaces, target = the entity);
    /// 2. the **tycon tier**, declaration-ordered per child: the type name as a
    ///    value-space contestant (FCS's unqualified constructor slot — opaque:
    ///    we model the *shadow*, not the construction), a non-RQA union's
    ///    accessible **cases** (both spaces, opaque; an absent case list —
    ///    `union_case_names` of `None`, no pickle described the union — is
    ///    residue, while a knowably-empty list, a private representation,
    ///    contributes nothing), and a non-generic `[<AutoOpen>]` **type**
    ///    (residue — FCS adds its statics at the tycon tier, *below* the
    ///    module's own vals, and the projection cannot list them; round 14);
    /// 3. **vals** ([`Self::open_static_entries`] — the Slice-D extension rules
    ///    ride along), each a definite [`OpenFoldTarget::Member`] when uniquely
    ///    selectable; an **active pattern**'s val also emits its tags into the
    ///    constructor space (a malformed banana name ⇒ residue);
    /// 4. **`[<AutoOpen>]` submodules**, declaration-ordered, recursively —
    ///    *after* the vals, so a child's value beats the parent's same-named
    ///    val, exactly as FCS folds them.
    ///
    /// Residue (see [`OpenFoldSurface::residue`]) is set for the name-unknown
    /// remainder only: an unknowable pickle (`ExtensionMembers::Unknowable` —
    /// abbreviations and active patterns are then invisible wholesale), an
    /// undecodable member ([`Entity::skipped_members`] — its *source* name may
    /// differ from the recorded IL name), or a case-nameless union. Dropped
    /// types are deliberately **not** consulted here: they are a property of a
    /// *path* (a same-FQN merge partner we cannot see), not of this handle's
    /// contents, and the caller owns that check
    /// (`any_split_of_a_module_path_has_a_dropped_type`).
    pub fn open_fold_surface(&self, handle: EntityHandle) -> OpenFoldSurface {
        let mut out = OpenFoldSurface::default();
        self.fold_container_into(handle, &mut out, true);
        self.demote_pattern_shadowed_exceptions(&mut out);
        out
    }

    /// §8 cell 8b's MODULE-half flavor (found by codex review of the
    /// module-open matrix): an exception's committed [`OpenFoldTarget::Entity`]
    /// is sound in *pattern* position only while no LATER same-surface value
    /// may shadow it as a **constant pattern**. A `[<Literal>]` folds at the
    /// vals / auto-open tier — after the exception tier — and FCS then binds
    /// the bare pattern to the literal, so `case_reference`'s
    /// constructor-namespace scan returning the exception would be a wrong
    /// target. Such an exception folds *opaque* instead: still in scope, still
    /// a case shape, naming nothing (the pattern defers; the expression
    /// position is untouched — bare-name lookup already binds the later value).
    ///
    /// A **provably plain** later value leaves the exception committed: plain
    /// values do not enter the pattern namespace, and the exception still wins
    /// the constructor lookup (FCS-verified by the matrix's `exn-shadow-mod`
    /// cells). "Provably plain" is a property or method (a literal is always a
    /// field), or a field that is neither CLI-`Literal`-flagged nor of type
    /// `System.Decimal` — a C# `const decimal` is a literal carried by
    /// `DecimalConstantAttribute` with NO `Literal` flag (Q17), so a decimal
    /// field's literal-ness is unknowable and demotes conservatively.
    fn demote_pattern_shadowed_exceptions(&self, out: &mut OpenFoldSurface) {
        for i in 0..out.entries.len() {
            let e = &out.entries[i];
            if !(e.is_case && matches!(e.target, OpenFoldTarget::Entity(_))) {
                continue;
            }
            let shadowed = out.entries[i + 1..].iter().any(|later| {
                later.name == out.entries[i].name
                    && !later.is_case
                    && later.space != OpenFoldSpace::Pattern
                    && self.value_may_be_constant_pattern(&later.target)
            });
            if shadowed {
                out.entries[i].target = OpenFoldTarget::Opaque;
            }
        }
    }

    /// Whether a folded value entry may be a compile-time constant — and so a
    /// **constant pattern** that shadows a case in pattern position. See
    /// [`Self::demote_pattern_shadowed_exceptions`] for the classification.
    fn value_may_be_constant_pattern(&self, target: &OpenFoldTarget) -> bool {
        match target {
            OpenFoldTarget::Member { parent, idx } => match self.member_at(*parent, *idx) {
                Member::Field(f) => {
                    f.is_literal
                        || matches!(
                            &f.ty,
                            TypeRef::Named { namespace, name, .. }
                                if namespace.len() == 1
                                    && namespace[0] == "System"
                                    && name == "Decimal"
                        )
                }
                Member::Property(_) | Member::Method(_) | Member::Event(_) => false,
            },
            // No member to inspect (an opaque overload set, an entity): assume
            // the worst — an opaque case merely defers, never a wrong target.
            OpenFoldTarget::Entity(_) | OpenFoldTarget::Opaque => true,
        }
    }

    /// The complete-or-opaque bare-name surfaces an `open <namespace>` folds into
    /// scope from the **referenced assemblies** — the namespace half's own tycon
    /// tier (`docs/assembly-module-open-plan.md`, "the namespace half joins the
    /// fold"), the mirror of [`Self::open_fold_surface`] for a namespace rather
    /// than a module. FCS's `AddModuleOrNamespaceContentsToNameEnv` adds a
    /// namespace's direct **exception constructors** and **union cases** (the
    /// tycon tier), the **constructible type names** as unqualified value-slot
    /// contestants, and then, recursively, its `[<AutoOpen>]` submodules.
    /// Consumed as extra surfaces of an open's fold group, so a name a surface
    /// supplies collides per-name with the module half (a cross-kind FQN is a
    /// merge FCS orders by reference and we do not — the contest defers) and its
    /// name-unknown residue bumps the group's generation barrier.
    ///
    /// A namespace surface commits **no definite [`OpenFoldTarget::Entity`]
    /// targets**: its exception constructors fold *opaque* (§8 of the plan,
    /// option A). FCS can re-order the bare name after the fold — a later
    /// open's constructible type evicts it from the unqualified constructor
    /// slot, a same-surface `[<Literal>]` beats it as a constant pattern — and
    /// sema's bare-name lookup models neither, so committing the exception
    /// risks a wrong target where opaque merely defers.
    ///
    /// **One surface per contributing assembly** (like the module half's
    /// `opened_assembly_modules`):
    /// FCS folds different assemblies' namespace contributions in reference order,
    /// which we do not model, so a name two assemblies both supply must collide
    /// *across* surfaces and defer — lumping them into one surface would let the
    /// writer treat a cross-assembly duplicate as declaration-ordered and commit
    /// whichever it appended last.
    ///
    /// This does **not** push the direct types' *type-name* entries: a
    /// namespace-level type in FCS's unqualified constructor slot is the head-slot
    /// eviction machinery's channel (`assembly_slot_class`) for `Type.Member` and
    /// value-eviction, and pushing it as a value entry here would clobber that
    /// live qualified-access channel. Instead a constructible type's name goes in
    /// the surface's [`OpenFoldSurface::contestant_names`], which demotes a
    /// same-named value from *another* surface (the module half) without touching
    /// its own assembly's later `[<AutoOpen>]` value. A namespace has no vals of
    /// its own (F# forbids values at namespace scope); its `[<AutoOpen>]` modules
    /// are the roots being opened, so they fold with `top = true` — a case-nameless
    /// union / auto-open type in one of them is residue *below that module's own
    /// vals* (round 10), not full residue that would demote them.
    ///
    /// An assembly whose F# **signature is unknowable** may hide erased type
    /// abbreviations from the namespace entirely (they live only in the pickle),
    /// so a residue-only surface is appended: an unseen constructible abbreviation
    /// could contest the module half, which must therefore defer.
    pub fn open_namespace_fold_surfaces(&self, namespace: &[String]) -> Vec<OpenFoldSurface> {
        // The public top-level entities declared *directly* in the namespace. The
        // direct `[<AutoOpen>]` modules are a subset of this; we fold them
        // recursively below (`fold_container_into` descends into their nested
        // auto-opens). We deliberately do NOT iterate
        // `auto_open_modules_in_namespace`, which is the transitive *closure* — the
        // recursion would then fold every nested module a second time.
        let direct: Vec<EntityHandle> = self
            .top_level_types
            .iter()
            .copied()
            .filter(|&h| self.is_public(h) && self.entity(h).namespace.as_slice() == namespace)
            .collect();
        // The distinct contributing assemblies, in first-seen order.
        let mut assemblies: Vec<AssemblyIdentity> = Vec::new();
        for &h in &direct {
            let a = &self.entity(h).assembly;
            if !assemblies.iter().any(|x| x == a) {
                assemblies.push(a.clone());
            }
        }
        let mut surfaces: Vec<OpenFoldSurface> = assemblies
            .iter()
            .map(|asm| {
                let mut out = OpenFoldSurface::default();
                let dir: Vec<EntityHandle> = direct
                    .iter()
                    .copied()
                    .filter(|&h| &self.entity(h).assembly == asm)
                    .collect();
                self.fold_tycon_tier(&dir, &mut out, true, false);
                // Constructible type names → value-slot contestants (exceptions
                // excluded: pass 1 already folded them as entries, which contest
                // through the ordinary cross-surface collision).
                out.contestant_names = dir
                    .iter()
                    .filter_map(|&h| {
                        let e = self.entity(h);
                        (e.kind != EntityKind::Exception
                            && type_name_is_value_slot_contestant(e.kind, e.is_struct))
                        .then(|| e.source_name.as_deref().unwrap_or(&e.name).to_string())
                    })
                    .collect();
                // The direct `[<AutoOpen>]` modules, recursive, after the tycon
                // tier (FCS: `AddModuleOrNamespaceRefsToNameEnv` runs last).
                // `top = true`: each is a directly-opened root, so its own tycon
                // residue stays below its own vals; the recursion folds its nested
                // auto-opens with `top = false`.
                for &child in dir.iter().filter(|&&h| {
                    let e = self.entity(h);
                    e.kind == EntityKind::Module && e.is_auto_open
                }) {
                    self.fold_container_into(child, &mut out, true);
                }
                // A namespace surface commits NO definite `Entity` targets — its
                // exception constructors (direct, and in recursed `[<AutoOpen>]`
                // modules) fold **opaque** (§8 of the plan, option A). FCS may
                // re-order the bare name after the fold: a LATER open's
                // constructible type takes the unqualified constructor slot and
                // evicts the exception (8a), and in pattern position a
                // same-surface `[<Literal>]` — undetectable in general, a
                // `decimal` literal carries no CLI `Literal` flag (Q17) — beats
                // it as a constant pattern (8b). Sema's bare-name lookup models
                // neither eviction, so a definite target here can be a wrong
                // target; opaque (in scope, shadowing by position, naming
                // nothing) is sound in both spaces. Recovering the definite
                // exception means modelling FCS's bare-name slot eviction —
                // §8's option B, its own slice.
                for e in &mut out.entries {
                    if matches!(e.target, OpenFoldTarget::Entity(_)) {
                        e.target = OpenFoldTarget::Opaque;
                    }
                }
                out
            })
            .collect();
        if self.unknowable_abbreviations_in_namespace(namespace) {
            surfaces.push(OpenFoldSurface {
                residue: true,
                ..OpenFoldSurface::default()
            });
        }
        surfaces
    }

    /// The **tycon tier** of a set of sibling children, in FCS fold order:
    /// exception constructors first (both spaces, a definite [`OpenFoldTarget::Entity`]
    /// target — which the namespace fold then demotes to opaque, see
    /// [`Self::open_namespace_fold_surfaces`]), then, in declaration order per child,
    /// optionally the type name as
    /// a value-space constructor-slot contestant (`push_type_names`), a
    /// non-`[<RequireQualifiedAccess>]` union's accessible cases, and an
    /// `[<AutoOpen>]` type's opaque statics (residue). Shared by the module fold
    /// ([`Self::fold_container_into`], `true` — Q2, a nested type is bare-nameable
    /// through the opened module) and the namespace fold
    /// ([`Self::open_namespace_fold_surfaces`], `false` — a namespace type takes
    /// FCS's slot via the eviction channel, not a fold entry). `top` distinguishes
    /// the opened container's own tier (a case-nameless union is
    /// [`OpenFoldSurface::residue_below_vals`], below its vals — round 10) from a
    /// recursed `[<AutoOpen>]` child's (whose tier folds *after* the parent's
    /// vals, so it escalates to full residue).
    fn fold_tycon_tier(
        &self,
        children: &[EntityHandle],
        out: &mut OpenFoldSurface,
        top: bool,
        push_type_names: bool,
    ) {
        let child_name = |c: &Entity| c.source_name.as_deref().unwrap_or(&c.name).to_string();

        // 1. Exception constructors (FCS adds all exceptions before any tycon).
        for &child in children {
            if !self.is_public(child) {
                continue;
            }
            let c = self.entity(child);
            if c.kind == EntityKind::Exception {
                out.entries.push(OpenFoldName {
                    name: child_name(c),
                    target: OpenFoldTarget::Entity(child),
                    space: OpenFoldSpace::Both,
                    is_case: true,
                    ap_shape: None,
                    constant_pattern: false,
                });
            }
        }

        // 2. The tycon tier, in declaration order.
        for &child in children {
            if !self.is_public(child) {
                continue;
            }
            let c = self.entity(child);
            // A type name occupies FCS's unqualified constructor slot. The module
            // fold pushes it for every kind (over-inclusive — a spurious deferral
            // is never a wrong target); the namespace fold pushes none (a namespace
            // type takes the slot via the eviction channel, not a fold entry).
            if push_type_names && c.kind != EntityKind::Module && c.kind != EntityKind::Exception {
                // Opaque: we model the *shadow* (the slot the type name takes),
                // not the construction. An EXCEPTION is excluded above — pass 1
                // already pushed its name with a definite `Entity` target, and
                // both of FCS's readings of the bare name (`Item.ExnCase`, and the
                // exception class's constructor slot) name that same entity; a
                // later opaque entry here would mask the definite one (round 20).
                out.entries.push(OpenFoldName {
                    name: child_name(c),
                    target: OpenFoldTarget::Opaque,
                    space: OpenFoldSpace::Value,
                    is_case: false,
                    ap_shape: None,
                    constant_pattern: false,
                });
            }
            if c.kind == EntityKind::Union && !c.is_require_qualified_access {
                match &c.union_case_names {
                    // The pickle did not describe this union (foreign CCU, no
                    // pickle): its case names are unknowable. The hidden names
                    // are tycon-tier — below the top container's vals (round
                    // 10), but above a parent's when recursed.
                    None => {
                        if top {
                            out.residue_below_vals = true;
                        } else {
                            out.residue = true;
                        }
                    }
                    // The complete accessible-case list — possibly empty (a
                    // private representation contributes nothing to a
                    // cross-assembly open, and is NOT residue).
                    Some(cases) => {
                        for case in cases {
                            out.entries.push(OpenFoldName {
                                name: case.clone(),
                                target: OpenFoldTarget::Opaque,
                                space: OpenFoldSpace::Both,
                                is_case: true,
                                ap_shape: None,
                                constant_pattern: false,
                            });
                        }
                    }
                }
            }
            if c.is_auto_open && c.kind != EntityKind::Module && c.generic_parameters.is_empty() {
                // An `[<AutoOpen>]` *type*: FCS adds its static content at the
                // tycon tier when `CanAutoOpenTyconRef` admits it (review round
                // 14). We cannot enumerate that content: the projection
                // deliberately drops an F# data kind's non-field properties (a
                // record's `static member Tag` has no `Member` at all), so
                // `open_static_entries` under-lists it. Name-unknown residue —
                // tycon-tier at the top container (below its vals,
                // fsi-verified), full when recursed.
                //
                // A GENERIC auto-open type is excluded: `CanAutoOpenTyconRef`
                // demands an empty typar list, so FCS opens nothing from it —
                // it hides nothing and must not be residue (codex round 20).
                // FCS also demands the type be F#-declared (`not IsILTycon`),
                // which we do not test: a C#-declared `[<AutoOpen>]` type is
                // treated as residue FCS would ignore — availability only,
                // never a wrong target.
                if top {
                    out.residue_below_vals = true;
                } else {
                    out.residue = true;
                }
            }
        }
    }

    /// One container's contribution to [`Self::open_fold_surface`] — split out
    /// so the `[<AutoOpen>]` submodule recursion appends into the same list.
    /// `top` marks the opened container itself: a tycon-tier-confined loss
    /// there is [`OpenFoldSurface::residue_below_vals`] (FCS folds it before
    /// the vals), while the same loss in a recursed `[<AutoOpen>]` child folds
    /// *after* the parent's vals and so escalates to the full residue.
    fn fold_container_into(&self, handle: EntityHandle, out: &mut OpenFoldSurface, top: bool) {
        if matches!(
            self.module_extension_members(handle),
            ExtensionMembers::Unknowable
        ) {
            out.residue = true;
        }
        let entity = self.entity(handle);
        if !entity.skipped_members.is_empty() {
            out.residue = true;
        }

        // 1-2. Exceptions and the tycon tier over this module's own children. A
        // module fold pushes ALL nested type names (Q2 — a nested type is
        // bare-nameable through the opened module).
        self.fold_tycon_tier(self.children(handle), out, top, true);

        // 3. Vals — and each active pattern's tags into the constructor space.
        let vals: Vec<(String, Option<MemberIndex>)> = self
            .open_static_entries(handle)
            .into_iter()
            .map(|(name, idx)| (name.to_string(), idx))
            .collect();
        // A banana-named val is an active-pattern recognizer **only in an
        // authoritative F# module**. On an assembly whose F# signature is
        // unreliable (`fsc --standalone`, an undecoded pickle) `EntityKind::Module`
        // is an IL heuristic FCS does not share — it imports the entity through IL,
        // where a banana-named `let` is an ordinary method group, never a
        // recognizer (Stage 3b; the same reason [`Self::entity_class`] declines the
        // kind). So demangle only when the container's signature is authoritative;
        // otherwise the `|Foo|` is just a value entry.
        let authoritative_module = !self.fsharp_signature_unreliable(handle);
        for (name, idx) in vals {
            if authoritative_module && name.starts_with('|') {
                match active_pattern_banana(&name) {
                    Some((tags, shape)) => {
                        // The tags carry the recognizer shape. Whether that shape
                        // may actually drive a use-site split is decided at the
                        // split site, where the full scope is known: a same-named
                        // `[<Literal>]` / constant value (here, a later `open`, a
                        // local `let`, or an auto-open child) is a CONSTANT PATTERN
                        // FCS's latest-wins puts in charge of the name, which
                        // `case_reference` skips as an ordinary value — so the split
                        // declines when any same-named value is in scope (codex
                        // rounds 4c/5a). A zero-tag recognizer contributes no entry.
                        for tag in tags {
                            out.entries.push(OpenFoldName {
                                name: tag.to_string(),
                                target: OpenFoldTarget::Opaque,
                                space: OpenFoldSpace::Pattern,
                                is_case: true,
                                ap_shape: Some(shape),
                                constant_pattern: false,
                            });
                        }
                    }
                    None => out.residue = true,
                }
            }
            let target = match idx {
                Some(idx) => OpenFoldTarget::Member {
                    parent: handle,
                    idx,
                },
                None => OpenFoldTarget::Opaque,
            };
            let constant_pattern = self.value_may_be_constant_pattern(&target);
            out.entries.push(OpenFoldName {
                name,
                target,
                space: OpenFoldSpace::Value,
                is_case: false,
                ap_shape: None,
                constant_pattern,
            });
        }

        // 4. `[<AutoOpen>]` submodules, declaration-ordered, recursive — after
        // the vals (FCS: `AddModuleOrNamespaceRefsToNameEnv` runs last, so a
        // child's value shadows the parent's).
        let auto_open_children: Vec<EntityHandle> = self
            .children(handle)
            .iter()
            .copied()
            .filter(|&child| {
                self.is_public(child)
                    && self.entity(child).kind == EntityKind::Module
                    && self.entity(child).is_auto_open
            })
            .collect();
        for child in auto_open_children {
            self.fold_container_into(child, out, false);
        }
    }

    /// The module's F#-native **instance extension members** — the overload
    /// resolution extension-absence gate's input (see [`ExtensionMembers`]).
    ///
    /// Returns [`ExtensionMembers::Unknowable`] when `handle`'s assembly has
    /// unknowable F# signature data (its pickle failed to decode, or it embeds
    /// foreign CCUs — the [`AbbreviationVisibility::Unknowable`] signal, tracked
    /// per interned entity): its extension members cannot be soundly enumerated,
    /// so the gate must defer. Otherwise returns
    /// [`ExtensionMembers::Known`] with the module's
    /// [`Entity::extension_member_names`] — an empty slice for a module (or any
    /// non-module entity) that declares none. The names are the F# *source* names
    /// a use site writes (`recv.M`), so the gate compares the queried member name
    /// directly.
    pub fn module_extension_members(&self, handle: EntityHandle) -> ExtensionMembers<'_> {
        if self.nodes[handle.index()].extensions_unknowable {
            return ExtensionMembers::Unknowable;
        }
        ExtensionMembers::Known(&self.entity(handle).extension_member_names)
    }

    /// Mark the env's referenced-assembly **extension surface unknowable** — a
    /// projection failure (an unreadable `[<AutoOpen>]` list, a dropped type that
    /// may be a C#-style `[<Extension>]` class, or an entirely skipped assembly)
    /// could hide an extension the gate cannot otherwise see. Forces the OV-6
    /// extension-absence gate to defer wholesale. Called by the host (the LSP)
    /// after building the env, since the projection happens outside this crate.
    pub fn mark_extension_surface_unknowable(&mut self) {
        self.extension_surface_unknowable = true;
    }

    /// Mark the env's loaded-DLL **identity set incomplete** — the host skipped a
    /// DLL it could not project at all, so its manifest name is unregistered.
    /// That name could collide with a referenced CCU, so the abbreviation-target
    /// resolver can no longer prove any referenced-CCU name unique and declines
    /// every abbreviation target into a referenced CCU (correctness over
    /// availability). Called by the host (the LSP) after building the env, since
    /// which DLLs were skipped is known there.
    pub fn mark_referenced_assemblies_incomplete(&mut self) {
        self.assembly_identities_incomplete = true;
    }

    /// Record that a referenced assembly **dropped an undecodable type** in
    /// `namespace` (it may be a C#-style `[<Extension>]` class the entity tree no
    /// longer shows). The OV-6 gate treats `namespace` as possibly-extension-bearing
    /// — namespace-scoped, unlike [`Self::mark_extension_surface_unknowable`], so a
    /// file whose in-scope namespaces had no drop still commits. Called by the host
    /// (the LSP), which observes the drop. `namespace` is the dropped type's
    /// enclosing namespace (empty for a root-namespace type).
    pub fn mark_namespace_dropped_type(&mut self, namespace: Vec<String>) {
        self.namespaces_with_dropped_types.insert(namespace);
    }

    /// Whether a referenced assembly **dropped an undecodable type** in `namespace`
    /// (see [`Self::mark_namespace_dropped_type`]) — so a same-FQN sibling of some
    /// type there may have been dropped, making a lone surviving entry's identity
    /// unreliable. The applicability matcher's named-argument affirmation reads this.
    pub(crate) fn namespace_has_dropped_type(&self, namespace: &[String]) -> bool {
        self.namespaces_with_dropped_types.contains(namespace)
    }

    /// Whether a dropped type sits in **any namespace this module path could be split
    /// at** — the question [`Self::namespace_has_dropped_type`] asks, over the whole space
    /// the *lookup* walks rather than the one namespace the visible encoding happens to
    /// occupy (review round 16).
    ///
    /// `opened_assembly_modules` merges **every** split of an FQN, because one assembly
    /// may expose `A.B.C` as a top-level `C` in namespace `A.B` while another nests it as
    /// root module `A` → `B` → `C` (review round 7). The uncertainty check did not follow:
    /// it looked only at the *visible* handle's own namespace. So a visible root-module
    /// encoding (owning namespace `[]`) was certified `Complete` while another assembly
    /// had a **dropped** top-level `C` in `A.B` — a dropped type that could itself be
    /// another same-FQN module, whose members FCS would merge and order by reference. A
    /// definite target, decided against a half we could not see.
    ///
    /// Two traversals over one space must span the same space. So ask every split
    /// `path[..k]` for `k` in `0..=len` — the `len` module splits, **plus the full path**,
    /// which is the namespace half of a cross-kind pair. Bounded by the path's length, and
    /// conservative by construction: a dropped type anywhere along it means we cannot
    /// prove what this open imports.
    pub(crate) fn any_split_of_a_module_path_has_a_dropped_type(&self, path: &[String]) -> bool {
        (0..=path.len()).any(|k| self.namespace_has_dropped_type(&path[..k]))
    }

    /// **EX-1 — the name-keyed extension gate.** Whether any referenced-assembly
    /// extension source in scope could contribute an extension member **named
    /// `name`** to a call whose receiver is a value (`is_static = false`) or a
    /// type-qualified path (`is_static = true`).
    ///
    /// This is the *name-keyed* replacement for the OV-6 gate's pair of presence
    /// checks (the assembly-auto-open and per-namespace presence tests this
    /// replaces), and the coverage refinement OV-9
    /// measured the need for: an in-scope extension joins **its own name's** group
    /// flat and competes there, and can affect no call of any *other* name (probed
    /// 2026-07-12 — see `docs/extension-scope-enumeration-plan.md` §1). So a
    /// present-but-differently-named extension surface is no reason to defer, and
    /// deferring on one is what zeroed overload coverage for every project that
    /// references FSharp.Core (whose implicit auto-opens are always a surface).
    ///
    /// **The source set is unchanged** — deliberately. It is the same three
    /// sources the presence gate already enumerated (complete by construction
    /// after eight OV-6 review rounds), each refined from a boolean to a name
    /// test; nothing new is *added* to the scope, so this can only shrink the
    /// deferred set, never license a commit a complete name enumeration would not:
    ///
    /// 1. the global unknowables — a projection failure
    ///    ([`Self::mark_extension_surface_unknowable`]) or a contested auto-open —
    ///    which stay a **wholesale** defer, since an extension of *any* name may be
    ///    hiding behind them;
    /// 2. the auto-open surfaces (implicit `[<assembly: AutoOpen>]` namespaces, and
    ///    module/type-shaped auto-opens);
    /// 3. the file's own in-scope namespace chain (`in_scope_namespaces`: the
    ///    always-in-scope root `[]` plus each declared namespace and its ancestors),
    ///    where a referenced extension is in scope with no `open` at all.
    ///
    /// An **explicit `open`** is *not* consulted here: the resolver does not yet
    /// export what each `open` resolved to, so [`crate::infer`]'s gate still defers
    /// on any `open` (stage EX-2).
    ///
    /// For an auto-opened *namespace* this asks whether the namespace's whole entity
    /// tree contributes the name — a superset of what opening a namespace actually
    /// brings into scope (F#-native extension members need their *module* opened),
    /// so the answer over-approximates the extension set and can only add deferrals.
    pub(crate) fn extension_named_in_scope(
        &self,
        in_scope_namespaces: &[Vec<String>],
        name: &str,
        is_static: bool,
    ) -> bool {
        // (1) Global unknowable — a projection failure could hide an extension of
        // *any* name, so it stays a wholesale defer.
        if self.extension_surface_unknowable {
            return true;
        }
        // (2) The auto-open surfaces. The implicit-open set is the resolver's
        // *effective* one (manifest auto-opens + the hardcoded FSharp.Core fallback),
        // so the namespaces we prove the name absent from are exactly those the
        // resolver opens — an old/stand-in FSharp.Core still opens
        // `Microsoft.FSharp.{Core,Collections,Control}`, and an `[<AutoOpen>]`
        // extension there must be accounted for (review, GPT-5.6).
        if self
            .effective_implicit_open_namespace_paths()
            .iter()
            .any(|ns| self.namespace_has_extension_named(ns, name, is_static))
        {
            return true;
        }
        // A *contested* auto-open is applied by FCS **contributor-scoped** (it opens
        // the contributing CCU's namespace entity, so a sibling assembly's same-named
        // namespace stays closed), so only that assembly's extensions in it can enter
        // scope. Every real project has one of these — FSharp.Core auto-opens
        // `Microsoft`, which the BCL also declares — so answering by name here, rather
        // than deferring wholesale, is what makes the whole refinement bite.
        if self.contested_auto_opens.iter().any(|(contributor, ns)| {
            self.namespace_has_extension_named_in_assembly(ns, name, is_static, *contributor)
        }) {
            return true;
        }
        if self.auto_open_module_handles.iter().any(|&h| {
            // A **dropped** TypeDef beneath the (surviving) auto-opened module is
            // absent from the tree walked below, yet FCS imports it through the
            // auto-open — so consult the drop marker first. A dropped nested type is
            // recorded under its **top-level namespace** (nested types share it), which
            // is exactly the handle's owning namespace; any drop there could be the
            // module's own hidden extension container, of any name (mirrors
            // [`Self::module_may_hide_nested_modules`]).
            self.namespace_has_dropped_type(&self.nodes[h.index()].owning_namespace)
                || self.entity_tree_has_extension_named(h, name, is_static)
        }) {
            return true;
        }
        // (3) The file's in-scope namespace chain.
        in_scope_namespaces
            .iter()
            .any(|ns| self.namespace_has_extension_named(ns, name, is_static))
    }

    /// [`Self::namespace_has_extension_named`], restricted to the content of **one
    /// assembly** (by provenance — a same-named sibling DLL's content is still a
    /// sibling's) — the contributor of a *contested* auto-open, whose namespace
    /// entity is the only one FCS opens (a sibling's same-named namespace stays
    /// closed). A dropped type in the namespace still answers `true` for every name:
    /// we cannot see whose it was, let alone what it declared.
    fn namespace_has_extension_named_in_assembly(
        &self,
        namespace: &[String],
        name: &str,
        is_static: bool,
        contributor: AssemblyId,
    ) -> bool {
        self.namespaces_with_dropped_types.contains(namespace)
            || self.types_in_namespace(namespace).iter().any(|&h| {
                self.assembly_provenance(h) == Some(contributor)
                    && self.entity_tree_has_extension_named(h, name, is_static)
            })
            || self
                .auto_open_modules_in_namespace(namespace)
                .iter()
                .any(|&h| {
                    self.assembly_provenance(h) == Some(contributor)
                        && self.entity_tree_has_extension_named(h, name, is_static)
                })
    }

    /// The top-level type handles whose **exact** namespace is `namespace` — the
    /// [`Self::types_by_namespace`] index lookup, empty if none. Replaces a linear
    /// [`Self::top_level_types`] scan in the hot extension-gate queries.
    fn types_in_namespace(&self, namespace: &[String]) -> &[EntityHandle] {
        self.types_by_namespace
            .get(namespace)
            .map_or(&[], Vec::as_slice)
    }

    /// The per-namespace half of [`Self::extension_named_in_scope`]: whether any
    /// referenced assembly declares an extension member **named `name`** in
    /// `namespace`'s entity tree (or in one of its auto-open modules). A namespace
    /// with a **dropped type** ([`Self::mark_namespace_dropped_type`]) answers
    /// `true` for every name — the dropped type may be an `[<Extension>]` class
    /// declaring anything, and we cannot see it.
    fn namespace_has_extension_named(
        &self,
        namespace: &[String],
        name: &str,
        is_static: bool,
    ) -> bool {
        self.namespaces_with_dropped_types.contains(namespace)
            || self
                .types_in_namespace(namespace)
                .iter()
                .any(|&h| self.entity_tree_has_extension_named(h, name, is_static))
            || self
                .auto_open_modules_in_namespace(namespace)
                .iter()
                .any(|&h| self.entity_tree_has_extension_named(h, name, is_static))
    }

    /// The per-entity-tree half of [`Self::extension_named_in_scope`]: whether
    /// `handle`'s tree carries an extension member named `name` of the kind the
    /// call needs.
    ///
    /// The two F#-native channels are the OV-0.5 name indexes, selected by the call
    /// shape — because FCS selects the same way: a **value receiver**'s group takes
    /// only *instance* extensions (`MethInfo.IsInstance`, overload-resolution-plan
    /// §6.1(a)) and a **type-qualified static** call's group only *static* ones
    /// (probed 2026-07-12: an opened `type System.String with static member Compare`
    /// joins `System.String.Compare 1`). Reading the wrong list would be exactly the
    /// unsoundness EX-0 exists to prevent.
    ///
    /// A C#-style `[<Extension>]` method counts only for an **instance** call: FCS
    /// hard-wires such a method's `IsInstance` to true (`infos.fs:574–582`), so it is
    /// reachable as `recv.M(…)` and never as `T.M(…)`.
    ///
    /// Two shapes answer `true` for **every** name, since they hide names we cannot
    /// read: an [`ExtensionMembers::Unknowable`] index (the owning assembly's F#
    /// signature data failed to decode), and any **skipped member** (undecodable, so
    /// possibly an `[<Extension>]` of any name).
    fn entity_tree_has_extension_named(
        &self,
        handle: EntityHandle,
        name: &str,
        is_static: bool,
    ) -> bool {
        if matches!(
            self.module_extension_members(handle),
            ExtensionMembers::Unknowable
        ) {
            return true;
        }
        let entity = self.entity(handle);
        let fsharp_names = if is_static {
            &entity.static_extension_member_names
        } else {
            &entity.extension_member_names
        };
        if fsharp_names.iter().any(|n| n == name) {
            return true;
        }
        if !entity.skipped_members.is_empty() {
            return true;
        }
        if !is_static
            && entity.members.iter().any(
                |m| matches!(m, Member::Method(mm) if mm.is_extension_method && member_name(m) == name),
            )
        {
            return true;
        }
        self.children(handle)
            .iter()
            .any(|&c| self.entity_tree_has_extension_named(c, name, is_static))
    }

    /// The handles of `handle`'s nested types, in declaration order.
    pub fn children(&self, handle: EntityHandle) -> &[EntityHandle] {
        &self.nodes[handle.index()].children
    }

    /// Descend to a nested type by simple name and generic arity (`Outer` →
    /// `Inner`). Arity disambiguates same-named nested types as it does for
    /// top-level ones (see [`Self::lookup_type`]). `None` if no nested type of
    /// `handle` matches.
    pub fn nested(&self, handle: EntityHandle, name: &str, arity: usize) -> Option<EntityHandle> {
        let children = self.children(handle);
        let matches =
            |e: &Entity, candidate: &str| candidate == name && e.generic_parameters.len() == arity;
        // Match by the name F# source uses, mirroring [`Self::by_type`]'s
        // tiers: an ordinary child by its IL name first (a real type keeps
        // the bare name), then source-named TYPES (a `[<CompiledName>]`-
        // renamed type or a renamed-abbreviation marker), then source-named
        // MODULES (suffixed companions) — the same type-over-module slot rule
        // as the top-level index, independent of child storage order (codex
        // round 6: a nested marker appended after its suffixed `module`
        // companion must still win the bare name). A suffixed module's
        // compiled name (`TaggedModule`) is never matched — F# source never
        // writes it.
        children
            .iter()
            .copied()
            .find(|c| {
                let e = self.entity(*c);
                e.source_name.is_none() && matches(e, &e.name)
            })
            .or_else(|| {
                children.iter().copied().find(|c| {
                    let e = self.entity(*c);
                    e.kind != EntityKind::Module
                        && e.source_name.as_deref().is_some_and(|src| matches(e, src))
                })
            })
            .or_else(|| {
                children.iter().copied().find(|c| {
                    let e = self.entity(*c);
                    e.kind == EntityKind::Module
                        && e.source_name.as_deref().is_some_and(|src| matches(e, src))
                })
            })
    }

    /// The handles of the top-level `[<AutoOpen>]` modules declared directly in
    /// `namespace` — empty if none. The resolver opens these into unqualified
    /// scope whenever the namespace is opened (implicitly or via `open`), so a
    /// bare `printfn` resolves to `ExtraTopLevelOperators`'s static member.
    /// The registered set includes the transitive closure of nested public
    /// `[<AutoOpen>]` modules (FCS opens them recursively); assembly-level
    /// `[<assembly: AutoOpen("…")>]` attributes drive
    /// [`Self::implicit_open_namespace_paths`] instead — they decide *which*
    /// namespaces are implicitly opened, and this index then supplies those
    /// namespaces' auto-open modules.
    pub fn auto_open_modules_in_namespace(&self, namespace: &[String]) -> &[EntityHandle] {
        self.auto_open_modules
            .get(namespace)
            .map_or(&[], Vec::as_slice)
    }

    /// Whether an `[<AutoOpen>]` module directly in `namespace` declares an
    /// **accessible** nested type/module matching `name` (arity-agnostic —
    /// F#'s in-scope type lookup for a bare annotation does not key on arity,
    /// mirroring [`Self::nested`]'s two-tier IL-name/source-name match without
    /// the arity filter). Only `public` children count: FSharp.Core's
    /// auto-open modules (`Operators`, `ExtraTopLevelOperators`, …) carry many
    /// `private` compiler-generated closure classes that can never be named
    /// from source — counting them turned this into an "any children" check
    /// that shadowed every bare annotation in every file, since those two
    /// modules are always in scope via the implicit `Microsoft.FSharp.Core`
    /// open (found by review; see `docs/completed/r2-annotation-typing-plan.md`).
    pub fn auto_open_module_shadows_type_named(&self, handle: EntityHandle, name: &str) -> bool {
        self.children(handle).iter().any(|child| {
            self.is_public(*child)
                && match &self.entity(*child).source_name {
                    Some(src) => src == name,
                    None => self.entity(*child).name == name,
                }
        })
    }

    /// The [`Self::auto_open_module_shadows_type_named`] check over every
    /// `[<AutoOpen>]` module directly in `namespace`.
    pub fn auto_open_modules_in_namespace_shadow_type_named(
        &self,
        namespace: &[String],
        name: &str,
    ) -> bool {
        self.auto_open_modules_in_namespace(namespace)
            .iter()
            .any(|handle| self.auto_open_module_shadows_type_named(*handle, name))
    }

    /// Whether an assembly whose abbreviations are
    /// [unknowable](AbbreviationVisibility::Unknowable) could hold a
    /// metadata-invisible type abbreviation **directly in** `namespace` — one
    /// that shadows a primitive alias, or (for a cross-kind `open`) contests a
    /// same-named value, despite nothing in the entity tree (not even a
    /// synthesised marker) witnessing it.
    ///
    /// A recorded namespace is the *exact* namespace of a surviving entity, but a
    /// pickle-only abbreviation is erased from IL, so it may sit **directly in an
    /// ancestor** of a namespace the assembly visibly declares into — an assembly
    /// with a public type only in `Foo.Sub` can still hide a `type X = …` in
    /// `Foo` itself. So `namespace` matches when it is a recorded namespace **or a
    /// prefix of one** (`has_namespace` recognises the ancestor by the same
    /// `starts_with`, and a false "no" there would commit a definite target for a
    /// name the hidden abbreviation may bind — codex round 6).
    pub fn unknowable_abbreviations_in_namespace(&self, namespace: &[String]) -> bool {
        self.unknowable_abbreviation_namespaces
            .iter()
            .any(|ns| ns.starts_with(namespace))
    }

    /// Whether `handle` is a metadata-invisible type-abbreviation **marker**
    /// (see `apply_abbreviation_markers` in `borzoi-assembly`): the name is real.
    /// A lookup landing on one resolves *through* its target when
    /// [`Self::resolve_abbreviation_target`] yields one, and otherwise defers.
    pub fn is_abbreviation(&self, handle: EntityHandle) -> bool {
        self.entity(handle).kind == EntityKind::Abbreviation
    }

    /// Resolve an abbreviation marker to the entity its target *names*, so a
    /// consumer can resolve *through* the alias (`type S = System.String` lets
    /// `S.Format` bind the member on `System.String`) instead of deferring.
    ///
    /// Returns `Some(handle)` only for a **nullary `Named`** target that resolves
    /// to a loaded entity — chasing a chained alias (`type A = B` where `B` is
    /// itself a marker) with a fuel bound against a pathological cycle. `None` for
    /// every shape that does not name a single entity whose members a path can
    /// walk (a type parameter, a function, a tuple, a *generic instantiation*), a
    /// target whose CCU is not loaded, or one that does not resolve — the consumer
    /// then keeps deferring, so this can only turn a defer into a *correct*
    /// resolution, never a wrong one.
    pub fn resolve_abbreviation_target(&self, marker: EntityHandle) -> Option<EntityHandle> {
        self.resolve_abbreviation_target_fueled(marker, ABBREV_CHASE_FUEL)
    }

    fn resolve_abbreviation_target_fueled(
        &self,
        marker: EntityHandle,
        fuel: u32,
    ) -> Option<EntityHandle> {
        let fuel = fuel.checked_sub(1)?;
        match self.entity(marker).abbreviation_target.as_ref()? {
            AbbreviationTarget::Named { ccu, path, args } if args.is_empty() => {
                let target = match ccu {
                    // Proven same-CCU (the pickle used a `Local` tcref): resolve
                    // within the marker's OWN DLL, matched by per-DLL provenance —
                    // a manifest identity (let alone a simple name) can collide with
                    // a byte-identical duplicate-reference sibling and resolve a
                    // wrong tree (issue #150).
                    None => {
                        let marker_key = self.assembly_key(marker);
                        self.abbreviation_target_at_path(path, |h| {
                            self.assembly_key(h) == marker_key
                        })?
                    }
                    // A referenced CCU, known only by simple name. If two *distinct*
                    // loaded DLLs share that name we cannot tell which the pickle
                    // meant, so decline rather than guess (correctness over
                    // availability); the consumer keeps deferring. Distinctness is
                    // by DLL, so byte-identical siblings count as two, not one.
                    Some(name) => {
                        let key = self.unique_assembly_key_for_name(name)?;
                        self.abbreviation_target_at_path(path, |h| self.assembly_key(h) == key)?
                    }
                };
                // Chase a chained alias; a non-marker target is the terminus.
                if self.is_abbreviation(target) {
                    self.resolve_abbreviation_target_fueled(target, fuel)
                } else {
                    Some(target)
                }
            }
            // `Var` / function / tuple / generic-instantiation targets do not name
            // a single member-bearing entity — keep deferring.
            _ => None,
        }
    }

    /// The [`AssemblyKey`] of the sole loaded DLL whose manifest simple name is
    /// `name`, or `None` if **no** loaded DLL has that name or **two or more
    /// distinct** DLLs do. A referenced CCU is pickled only by its simple name;
    /// an absent or ambiguous name cannot be pinned to one DLL, so the target
    /// declines (see [`Self::resolve_abbreviation_target`]). Two byte-identical
    /// duplicate-reference DLLs count as two (issue #150) — a bare
    /// manifest-identity comparison would collapse them and let a target guess
    /// into the wrong tree.
    ///
    /// Counts off the per-DLL [`Self::assembly_identities`] registry so a
    /// same-named DLL whose types were **all dropped** (no
    /// [`Self::top_level_types`]) still makes the name ambiguous (codex P2). For
    /// the synthetic single-group [`Self::from_entities`] (no registry) it falls
    /// back to the interned entities, keyed by manifest identity.
    fn unique_assembly_key_for_name(&self, name: &str) -> Option<AssemblyKey<'_>> {
        // An **incomplete** identity set — an unnameable rootless projection, or a
        // DLL the projector skipped entirely — could itself be `name`, so a
        // matching sibling might not be the sole DLL of that name: uniqueness is
        // undecidable, decline (codex P2). The runtime env supplies every identity
        // and skips no loadable DLL, so this never fires there.
        if self.assembly_identities_incomplete {
            return None;
        }
        if !self.assembly_identities.is_empty() {
            let mut found: Option<AssemblyId> = None;
            for (idx, ident) in self.assembly_identities.iter().enumerate() {
                if ident.as_ref().is_some_and(|a| a.name == name) {
                    let id = AssemblyId(u32::try_from(idx).expect("more than u32::MAX assemblies"));
                    match found {
                        None => found = Some(id),
                        Some(prev) if prev != id => return None,
                        Some(_) => {}
                    }
                }
            }
            return found.map(AssemblyKey::Provenance);
        }
        let mut found: Option<AssemblyKey<'_>> = None;
        for &h in &self.top_level_types {
            if self.entity(h).assembly.name == name {
                let key = self.assembly_key(h);
                match found {
                    None => found = Some(key),
                    Some(prev) if prev != key => return None,
                    Some(_) => {}
                }
            }
        }
        found
    }

    /// Whether `handle` has a **public type-abbreviation child** named `name`, at
    /// **any** arity — the generic-arity gap the arity-0 [`Self::nested`] walk
    /// misses. A qualified path landing on such a child must defer: the
    /// abbreviation's target is unmodelled, and FCS *chases* it before deciding
    /// ownership — a record/union target falls through to a lower reading, a
    /// class target keeps this module (probed) — so we can commit neither
    /// direction and decline instead (D5, correctness over availability). Mirrors
    /// the [`Self::is_abbreviation`] defer the arity-0 `nested` branch of
    /// `assembly_path_records` already applies; this closes it for a *generic*
    /// abbreviation, which `nested(.., 0)` skips on arity.
    ///
    /// `assembly_path_records` consults this only in the `Uncertain` arm of its
    /// `static_lookup` — *after* a resolvable module value would have won (a
    /// module legally declaring both `let X` and `type X<'a>` resolves to the
    /// val, codex review 4) — and returns its `AbbreviationOpaque` reading so the
    /// defer is tier-local rather than a preemptive lexical shadow.
    pub fn has_public_abbreviation_child(&self, handle: EntityHandle, name: &str) -> bool {
        self.children(handle).iter().copied().any(|c| {
            self.is_public(c) && self.is_abbreviation(c) && {
                let e = self.entity(c);
                e.source_name.as_deref().unwrap_or(&e.name) == name
            }
        })
    }

    /// Find a member of `handle` by its display name, returning its
    /// [`MemberIndex`]. The first member with that name wins (overloads share a
    /// name; disambiguating them needs signatures, a later concern). `None` if
    /// the entity has no member with that name.
    pub fn member(&self, handle: EntityHandle, name: &str) -> Option<MemberIndex> {
        self.member_where(handle, name, |_| true)
    }

    /// Classify `m` (a member of `handle`) as an extension member — the **one** reader
    /// of the underlying metadata in this crate. What that classification then *does*
    /// to a lookup is [`presence`]'s business, per [`Channel`]; keeping the two apart
    /// is what stops a consumer from re-deriving the facts and drifting from the rule.
    ///
    /// The two facts, and why each has an undecidable case:
    ///
    /// - **F#-native augmentation** ([`Augmentation`], per member — not per *name*:
    ///   F# permits a `let M` beside an augmentation `M`, and FCS resolves `M` to the
    ///   `let`). `Certain` when the pickled val says so; `Possible` when only the IL
    ///   dot-name mangling does, on an image with no usable pickle — a dotted
    ///   `[<CompiledName>]` on an ordinary `let` looks identical.
    /// - **C#-style extension** — FCS's `IsMethInfoPlainCSharpStyleExtensionMember`:
    ///   the enclosing type carries `[<Extension>]`
    ///   ([`Entity::is_extension_container`](borzoi_assembly::Entity::is_extension_container),
    ///   `isEnclExtTy`) **and is non-generic**
    ///   (`IsTyconRefUsedForCSharpStyleExtensionMembers`'s `isNil (tcref.Typars m)` — a
    ///   generic container is not a C#-style extension container at all, so `open type
    ///   G<int>` then bare `GenExt` compiles, fsi-verified), the method carries it, and
    ///   it has **exactly one argument group** with ≥ 1 argument. A *curried*
    ///   `[<Extension>] static member M x y` stays in scope (fsi-verified), so the
    ///   one-group requirement is load-bearing: with
    ///   [`arg_group_count`](borzoi_assembly::MethodLike::arg_group_count)
    ///   `Some(1)` it is one; with `Some(_)` it is [`ExtensionKind::Ordinary`]; with
    ///   `None` — an F# assembly, whose flattened IL signature cannot distinguish
    ///   curried from tupled — we cannot tell, so [`Certainty::Possible`].
    ///
    /// The C#-style fact never applies to a **module**: FCS adds a module's contents
    /// through its *vals* (`AddModuleOrNamespaceContentsToNameEnv`), where the C#-style
    /// predicate never runs — so an `[<Extension>]`-attributed module-level `let`,
    /// which fsc marks with the CLR attribute on both the `let` and the module class,
    /// stays bare-resolvable (fsi-verified). Only the augmentation fact hides a module
    /// member.
    fn extension_kind(&self, handle: EntityHandle, m: &Member) -> ExtensionKind {
        let Member::Method(mm) = m else {
            return ExtensionKind::Ordinary;
        };
        match mm.augmentation {
            Augmentation::Certain => return ExtensionKind::Augmentation(Certainty::Certain),
            Augmentation::Possible => return ExtensionKind::Augmentation(Certainty::Possible),
            Augmentation::No => {}
        }
        let entity = self.entity(handle);
        let csharp_style = entity.kind != EntityKind::Module
            && entity.is_extension_container
            && entity.generic_parameters.is_empty()
            && mm.is_extension_method
            && !mm.signature.parameters.is_empty();
        if !csharp_style {
            return ExtensionKind::Ordinary;
        }
        match mm.arg_group_count {
            Some(1) => ExtensionKind::CSharpStyle(Certainty::Certain),
            // Two or more groups: curried, so FCS's predicate does not match it and it
            // behaves like any ordinary static.
            Some(_) => ExtensionKind::Ordinary,
            None => ExtensionKind::CSharpStyle(Certainty::Possible),
        }
    }

    /// Whether `m` (a member of `handle`) is *there*, on `channel` — the composition of
    /// [`Self::extension_kind`] with [`presence`], and the only way any lookup in this
    /// crate consults the extension facts.
    fn presence_of(&self, handle: EntityHandle, m: &Member, channel: Channel) -> Presence {
        presence(self.extension_kind(handle, m), channel)
    }

    /// Whether `m` is an F#-native augmentation the *qualified* lookups must skip —
    /// The **whole** answer for a type/module-qualified path (`Type.Member`,
    /// `Module.value`): both *which member the path selects* and *whether the path is
    /// occupied at all* — the two questions a tiered qualified walk must ask, answered
    /// by one traversal over one candidate set so they cannot disagree.
    ///
    /// That single-traversal shape is the point. They used to be two predicates —
    /// this one, and an inheritance-aware ownership probe — and review rounds 3 and 4
    /// were both the same defect: the two disagreeing about whether a member exists,
    /// so a path FCS resolves elsewhere was silently swallowed (round 4: a hidden
    /// augmentation the selection dropped but the probe still counted) or a path FCS
    /// resolves *here* was re-rooted onto a lower tier (round 3). With one candidate
    /// set, [`StaticLookup::Absent`] *means* "no candidate anywhere", and the
    /// fall-through rule reads off it directly.
    ///
    /// A **candidate** is a public member of the name that FCS's qualified lookup can
    /// see — i.e. one the `presence` table does not call absent on the qualified `Channel`. So an F#-native augmentation is *not* a candidate (fsi:
    /// `LazyExtensions.Force l` is FS0039 — the path is genuinely not there, and a
    /// lower-priority `open` may own it), while a C#-style `[<Extension>]` static *is*
    /// (fsi: `System.Linq.Enumerable.Select(xs, f)` compiles).
    ///
    /// The three outcomes:
    ///
    /// - exactly one candidate at `handle`'s own level, **static** and decidably
    ///   present ⇒ [`StaticLookup::Resolved`];
    /// - one or more own-level static candidates that do not meet that bar — an
    ///   overload set, a metadata ambiguity, or an undecidable augmentation (whose
    ///   name is *occupied*, we simply cannot say by what) ⇒ [`StaticLookup::Uncertain`];
    /// - no own-level static candidate, but the name is still reachable by FCS's
    ///   qualified lookup (`qualified_path_occupied` — for a *type*, the
    ///   inheritance-aware, kind-agnostic member lookup; for a *module*, the
    ///   in-module search over its own contents, which never sees the compiled
    ///   class's base chain) ⇒ [`StaticLookup::Uncertain`] as well: FCS finds it
    ///   and errors (or resolves) rather than re-rooting the path, so falling
    ///   through would hand the path to a reading FCS never consults;
    /// - nothing anywhere ⇒ [`StaticLookup::Absent`], and only then may a lower tier
    ///   own the path.
    pub fn static_lookup(&self, handle: EntityHandle, name: &str) -> StaticLookup {
        let mut candidates = self
            .entity(handle)
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| member_name(m) == name && member_is_static(m) && member_is_public(m))
            .map(|(i, m)| (i, self.presence_of(handle, m, Channel::Qualified)))
            .filter(|(_, p)| *p != Presence::Absent);
        match (candidates.next(), candidates.next()) {
            (Some((idx, Presence::Present)), None) => StaticLookup::Resolved(MemberIndex::new(idx)),
            // Occupied, but not uniquely selectable (a second candidate) or not
            // decidable at all (`Presence::Uncertain`).
            (Some(_), _) => StaticLookup::Uncertain,
            // No *static* of the name here — but FCS's member lookup may still reach
            // one, in which case the path is occupied and must not fall through.
            (None, _) if self.qualified_path_occupied(handle, name) => StaticLookup::Uncertain,
            (None, _) => StaticLookup::Absent,
        }
    }

    /// The uniquely-selectable public static named `name`, or `None` when the lookup
    /// is [`StaticLookup::Absent`] or [`StaticLookup::Uncertain`]. Callers that must
    /// distinguish those two — a qualified-path walk deciding whether a lower tier
    /// may own the path — want [`Self::static_lookup`] instead.
    pub fn static_member(&self, handle: EntityHandle, name: &str) -> Option<MemberIndex> {
        match self.static_lookup(handle, name) {
            StaticLookup::Resolved(idx) => Some(idx),
            StaticLookup::Absent | StaticLookup::Uncertain => None,
        }
    }

    /// The bare names an `open type T` / an `open` of a module — including the
    /// implicit auto-open fold — brings into unqualified scope, each paired with
    /// its unique [`MemberIndex`] or `None` when the name is not uniquely
    /// selectable (an overload set, or a metadata ambiguity: the caller records a
    /// [`Resolution::Deferred`](crate::Resolution::Deferred) — the name is in scope
    /// and shadows by position, but choosing among the candidates is the type
    /// checker's job). Distinct names, in member-list order, first occurrence kept.
    ///
    /// Public statics, minus **every extension member** — FCS admits none of them
    /// to the unqualified environment, and both exclusions below are fsi-verified
    /// FS0039 against the real compiler:
    ///
    /// - **F#-native** augmentations, instance and static: bare `Force`/`Create` out
    ///   of FSharp.Core's auto-open `LazyExtensions`. FCS filters them with
    ///   `AddValRefsToItems`'s `not vref.IsMember`. Unlike the C#-style case these
    ///   are not reachable qualified either, so [`Self::static_member`] drops them.
    /// - **C#-style** `[<Extension>]` statics: bare `Select` after
    ///   `open type System.Linq.Enumerable`. FCS filters them with
    ///   `ChooseMethInfosForNameEnv`.
    ///
    /// See `extension_kind` for the exact predicates and the `presence` table for the
    /// rule — including the two shapes we cannot decide, which enter scope as
    /// deferrals rather than as a guess in either direction.
    pub fn open_static_entries(&self, handle: EntityHandle) -> Vec<(&str, Option<MemberIndex>)> {
        let entity = self.entity(handle);
        let openable: Vec<(usize, &Member, Presence)> = entity
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| member_is_static(m) && member_is_public(m))
            .map(|(i, m)| (i, m, self.presence_of(handle, m, Channel::Bare)))
            .filter(|(_, _, p)| *p != Presence::Absent)
            .collect();
        let mut seen = HashSet::new();
        openable
            .iter()
            .map(|(_, m, _)| member_name(m))
            .filter(|name| seen.insert(*name))
            .map(|name| {
                let mut matching = openable.iter().filter(|(_, m, _)| member_name(m) == name);
                let unique = match (matching.next(), matching.next()) {
                    // Uniquely selectable — unless we cannot even tell whether FCS
                    // would admit it, in which case the name is in scope (it shadows
                    // by position) but names no target.
                    (Some((idx, _, Presence::Present)), None) => Some(MemberIndex::new(*idx)),
                    _ => None,
                };
                (name, unique)
            })
            .collect()
    }

    /// Whether FCS's **type-qualified member lookup** could find a member named
    /// `name` on `handle` — the path-**ownership** half of [`Self::static_lookup`],
    /// consulted only when no own-level static of the name is selectable (stage OV-7,
    /// review round 2). Private, and reachable only through `static_lookup`: that is
    /// what makes the two incapable of disagreeing about whether a member exists,
    /// which is exactly what review rounds 3 and 4 caught them doing.
    ///
    /// A **module** receiver — one whose `EntityKind::Module` is *authoritative*
    /// (its F# signature decoded; see [`Self::fsharp_signature_unreliable`]) — is
    /// answered by [`Self::module_qualified_occupied`] instead, because FCS
    /// resolves a module-qualified name through
    /// `ResolveExprLongIdentInModuleOrNamespace`, whose search domain is the
    /// module's own contents, never the compiled class's inheritance chain. A
    /// module whose signature is **non-authoritative** (a pickle-less or
    /// `--standalone` image) is only an IL heuristic — FCS imports it as a plain
    /// type — so it takes the type rule below, exactly as
    /// [`Self::entity_class`](AssemblyEnv) declines to classify it as a module.
    /// Everything below is the **type**-receiver rule
    /// (`ResolveLongIdentInTyconRefs`).
    ///
    /// FCS's type-member lookup is inheritance-aware and kind-agnostic (probed
    /// 2026-07-10: an *inherited static* resolves through the derived name, and an
    /// *instance-only* member of the name makes FCS error rather than re-root the
    /// path at a lower-priority reading), so this walks the base chain — the
    /// receiver's own level included — for **any public member** of the name that
    /// FCS can see ([`Presence`] on [`Channel::Qualified`], so a certainly-hidden
    /// augmentation does not count: fsi, `CoreExts.ExtStatic "x"` is FS0039, and
    /// with `High.M`'s only `X` an augmentation, `open Low; open High; M.X` is
    /// `Low.M.X`).
    ///
    /// A chain whose membership cannot be enumerated counts as *possible* (own and
    /// defer, never fall through to a reading FCS would not reach): an **interface**
    /// root (FCS's lookup includes transitively inherited interfaces and `Object`'s
    /// members, which `base_chain` cannot see — probed, review round 3: an inherited
    /// interface member owns), an unresolvable base ([`BaseChain::Incomplete`]), an
    /// Object-capped chain queried for one of `Object`'s own public members, or an
    /// undecodable (skipped) member of the name at any level.
    ///
    /// **Public-only is deliberate** (probed, review round 3): a *non-public*
    /// member of the name, and `Object`'s protected `Finalize` /
    /// `MemberwiseClone`, do **not** confer ownership — FCS filters
    /// accessibility at enumeration and re-roots the path at the lower-priority
    /// reading in both shapes, so counting them would defer (and un-resolve)
    /// paths FCS resolves.
    fn qualified_path_occupied(&self, handle: EntityHandle, name: &str) -> bool {
        // A genuine (authoritative) module takes the in-module search domain; a
        // non-authoritative one is really a plain type to FCS, so it falls
        // through to the base-chain rule below (mirroring `entity_class`).
        if self.entity(handle).kind == EntityKind::Module
            && !self.fsharp_signature_unreliable(handle)
        {
            return self.module_qualified_occupied(handle, name);
        }
        // An interface-rooted reading owns conservatively: its member surface
        // (inherited interfaces + `Object`) is not enumerable through
        // `base_chain`, and FCS owns such a reading even for a base-interface
        // member. The cost — an interface-rooted path with a genuinely absent
        // member no longer falls through — is a deferral on a vanishingly rare
        // name collision.
        if self.entity(handle).kind == EntityKind::Interface {
            return true;
        }
        let chain = match self.base_chain(handle) {
            BaseChain::Complete(c) => c,
            BaseChain::ObjectCapped(c) => {
                if is_object_method_name(name) {
                    return true;
                }
                c
            }
            BaseChain::Incomplete => return true,
        };
        chain.into_iter().any(|level| {
            self.has_skipped_member(level, name)
                || self.entity(level).members.iter().any(|m| {
                    member_name(m) == name
                        && member_is_public(m)
                        && self.presence_of(level, m, Channel::Qualified) != Presence::Absent
                })
        })
    }

    /// Whether FCS's **module-qualified lookup** could find `name` in module
    /// `handle` — the module half of [`Self::qualified_path_occupied`], consulted
    /// only when no own-level static of the name is selectable.
    ///
    /// FCS resolves `Module.name` through `ResolveExprLongIdentInModuleOrNamespace`
    /// (NameResolution.fs), whose search domain is the module's own contents —
    /// vals (`AllValsByLogicalName`), exception constructors, union cases
    /// (`TryFindTypeWithUnionCase`), nested types, submodules — and **never** the
    /// compiled class's base chain: `Object`'s members (`Equals`, `ToString`,
    /// `GetHashCode`, …) are unreachable through a module qualifier. On no match it
    /// razes `UndefinedName`, and `AtMostOneResultQuery` then lets the *type*
    /// search re-root the path (`moduleSearch +++ tyconSearch`,
    /// `ResolveExprLongIdentPrim`) — the exact opposite of the type-receiver rule:
    /// `open System; open Microsoft.FSharp.Core; String.Equals ("a", "b")` is
    /// `System.String.Equals`, not the FSharp.Core `String` module, because the
    /// module's failed member lookup does not own the path (the
    /// `resolve_string_qualifier_repro` divergence — the base-chain rule made
    /// `Equals` "occupied" via `Object`, so the later-open module reading wrongly
    /// owned the path and the `open System` tier was never consulted).
    ///
    /// Each clause maps to one FCS in-module search; vals are already covered by
    /// [`Self::static_lookup`]'s own-level candidate scan (a module's vals compile
    /// to own-level statics, matched by *source* name exactly as
    /// `AllValsByLogicalName` is keyed by logical name — so an IL
    /// `[<CompiledName>]` spelling does not occupy). The remainder:
    ///
    /// - an **undecodable member** of the name ([`Self::has_skipped_member`] —
    ///   same IL-name-keyed precision as the type-receiver rule, and the same
    ///   vanishingly-rare renamed-skip miss);
    /// - an **unknowable pickle** ([`ExtensionMembers::Unknowable`]): the vals'
    ///   source names cannot be enumerated at all (a rename is invisible), so any
    ///   name may be occupied — own and defer, mirroring the open fold's residue;
    /// - a public **child type** of the name that FCS resolves in the module —
    ///   [`Self::child_type_keeps_module_qualifier`]. The arity-0 children FCS
    ///   *would* resolve here (a non-generic nested type, a submodule, an
    ///   exception) were already consumed by the path walk's arity-0
    ///   [`Self::nested`] step, so the only child that reaches this clause is a
    ///   **generic** one — and FCS keeps the module for *every* generic type kind
    ///   except a **record** or **union**, whose bare name is not an expression
    ///   and which FCS re-roots to a lower-priority reading (probed exhaustively:
    ///   class, struct, interface, and delegate children keep the module even
    ///   with no accessible constructor, while `Collide.GenRec` / `Collide.GenUni`
    ///   fall through to a lower same-named type's static). A kind-blind check
    ///   would occupy the record/union and wrongly retain the module qualifier
    ///   (codex review). A generic **abbreviation** is *not* decided here — its
    ///   target is unmodelled and FCS's answer is target-sensitive, so
    ///   `assembly_path_records` defers the whole path via
    ///   [`Self::has_public_abbreviation_child`] before this runs (codex review
    ///   3). The non-authoritative-signature gate on
    ///   [`Self::qualified_path_occupied`] keeps this kind test on trustworthy
    ///   pickle data — a `--standalone` record misread as a class never reaches
    ///   here;
    /// - a public child **union**'s accessible **case** of the name —
    ///   `[<RequireQualifiedAccess>]` unions included (their cases resolve at the
    ///   lowest in-module priority, but still *in the module* — the final
    ///   `tyconSearch +++ moduleSearch +++ unionSearch` arm); a union whose case
    ///   names the pickle did not supply (`union_case_names` of `None`) may hide
    ///   any name, so it occupies conservatively. **Bounded residual** (codex
    ///   review, unmodelled): FCS's `TryFindTypeWithUnionCase` stops at the
    ///   *first* child union declaring the case and only then checks
    ///   representation accessibility, so two child unions sharing a case name
    ///   where the first has a private representation would make FCS fall through
    ///   — but a private representation contributes no names to `union_case_names`
    ///   at all, so we cannot see the first union declared it, and this `any`
    ///   accepts the later union. The scenario (two module-level unions with a
    ///   shared case name, the first private, colliding with a same-named type's
    ///   static) is vanishingly rare and unmodellable from the accessible-case
    ///   list alone.
    ///
    /// **Public-only is deliberate**, as in the type rule: FCS filters
    /// accessibility during the module search (`IsValAccessible` /
    /// `IsTyconReprAccessible` / `AccessibleEntityRef`) and re-roots past an
    /// inaccessible match. A *dropped* (undecodable) child type is NOT consulted:
    /// like the open fold, drops are a property of a path's other split partners,
    /// and counting the namespace-coarse signal here would occupy every name in
    /// every module of an affected namespace.
    fn module_qualified_occupied(&self, handle: EntityHandle, name: &str) -> bool {
        if self.has_skipped_member(handle, name) {
            return true;
        }
        if matches!(
            self.module_extension_members(handle),
            ExtensionMembers::Unknowable
        ) {
            return true;
        }
        self.children(handle).iter().copied().any(|child| {
            if !self.is_public(child) {
                return false;
            }
            let c = self.entity(child);
            if c.source_name.as_deref().unwrap_or(&c.name) == name
                && self.child_type_keeps_module_qualifier(c)
            {
                return true;
            }
            c.kind == EntityKind::Union
                && match &c.union_case_names {
                    None => true,
                    Some(cases) => cases.iter().any(|case| case == name),
                }
        })
    }

    /// Whether FCS keeps the **module qualifier** on `Module.Name` when a public
    /// child type of the module is named `Name` — the child-type arm of
    /// [`Self::module_qualified_occupied`]. FCS's in-module type lookup
    /// (`LookupTypeNameInEntityMaybeHaveArity`) resolves the bare name to the
    /// child for *every* type kind **except a record or union**, whose bare name
    /// is not an expression: those two alone FCS re-roots to a lower-priority
    /// reading (probed exhaustively — a class with only a private constructor, a
    /// struct with only the implicit default constructor, a non-constructible
    /// interface, and a delegate all keep the module; a record and a union do
    /// not). So the rule is purely kind-based, *not* constructibility.
    ///
    /// An **abbreviation** child returns `true` here (it is not a record/union),
    /// making the module-qualified name [`StaticLookup::Uncertain`] — but
    /// `assembly_path_records` then intercepts it (via
    /// [`Self::has_public_abbreviation_child`], in the `Uncertain` arm, after a
    /// resolvable val would have won) and defers the whole path tier-locally
    /// (`AbbreviationOpaque`): FCS's ownership is target-sensitive and the target
    /// is unmodelled, so neither committing the module nor falling through is
    /// safe.
    ///
    /// Only a **generic** child reaches this test — the arity-0 [`Self::nested`]
    /// step consumes non-generic children first — so `Enum` / `Measure` /
    /// `Exception` / `Module` (none of which can be generic) are unreachable
    /// here; they map to "keeps" (the safe side: keeping the qualifier defers the
    /// leaf, never a wrong target, whereas falling through could re-root a path
    /// FCS owns). The gate keeps this on authoritative pickle data, so the kind
    /// is trustworthy.
    fn child_type_keeps_module_qualifier(&self, entity: &Entity) -> bool {
        !matches!(entity.kind, EntityKind::Record | EntityKind::Union)
    }

    /// The type of the **single unambiguous public instance field / non-indexer
    /// property** named `name` on `handle` — the one member kind a member-access
    /// (`recv.Name`) discharges in Stage 3.3a. `None` (defer) whenever the lookup
    /// is not that exact shape:
    ///
    /// - **no** public instance member named `name` anywhere in the receiver's base
    ///   chain (the member is absent, static, or non-public);
    /// - **more than one** public instance member named `name` (a property + a
    ///   method group, two data members — an ambiguity F# resolves by member-kind
    ///   precedence we do not model, so we decline per correctness-over-
    ///   availability rather than guess);
    /// - the unique member is a **method**, an **event**, an **indexer** property
    ///   (non-empty index parameters), or a **write-only** property (no getter) —
    ///   none is a *readable* plain data member this stage types (`recv.Name` is a
    ///   read, so a setter-only property cannot be read).
    ///
    /// Returns the member's signature [`TypeRef`] (borrowed), which the caller
    /// (Stage 3.3a's `HasMember` wake) bridges into a [`Ty`](crate::Ty); that
    /// bridge may itself decline (a generic member type), in which case the access
    /// still defers. A method group of the same name is deliberately caught by the
    /// count check, so `s.ToString` (an overloaded method) defers here, not in the
    /// bridge.
    pub fn instance_data_member_ty(&self, handle: EntityHandle, name: &str) -> Option<&TypeRef> {
        self.instance_data_member(handle, name).map(|(_, _, ty)| ty)
    }

    /// The **declaring [`EntityHandle`], [`MemberIndex`], and type** of the single
    /// unambiguous public instance field / non-indexer property named `name`
    /// reachable from `handle` — exactly the member [`Self::instance_data_member_ty`]
    /// selects, but also returning *which* member it is, on *which* (possibly base)
    /// entity, so a consumer can name it (`Resolution::Member { parent, idx }` — the
    /// LSP member-resolution enrichment in Stage 3.3b renders/navigates it). The
    /// selection rules (single unambiguous public instance readable non-indexer data
    /// member) are identical; see [`Self::instance_data_member_ty`].
    ///
    /// **Inheritance (Stage 3.x-inh).** The lookup walks `handle`'s base-type chain
    /// nearest first (`base_chain`), so an *inherited* field / property
    /// resolves (returned under its **declaring** base's handle). A derived member of
    /// the name *shadows* any inherited one, so the first level that declares `name`
    /// wins — later (base) levels are never consulted for it, matching C#/F# name
    /// hiding. The **receiver's own** level is checked *before* the base chain: a
    /// member it declares hides inherited ones, so it resolves even when the chain
    /// cannot be completed (a class deriving from a closed generic base, `List<int>`).
    /// `System.Object` declares no data members, so an Object-capped chain (its only
    /// unresolved link an absent `Object`) is *complete* here; an inherited member on a
    /// truly incomplete chain (a generic / absent non-`Object` base) defers — but only
    /// when the name is *not* found on the receiver itself.
    pub fn instance_data_member(
        &self,
        handle: EntityHandle,
        name: &str,
    ) -> Option<(EntityHandle, MemberIndex, &TypeRef)> {
        // The receiver's own level first — a member it declares hides any inherited
        // one, so this needs no base chain and works even when the chain is incomplete.
        // (Sound for an interface receiver too: it is a subtype of every interface it
        // inherits, so an own member soundly hides an inherited one.)
        if let Some(result) = self.data_member_at_level(handle, name) {
            return result;
        }
        // An **interface receiver** walks the interface DAG (+ `System.Object`)
        // instead of the base chain, applying the exactly-one-declaring-level rule
        // (`docs/interface-walk-plan.md`): v1 does not hide across sibling interfaces,
        // so a name declared on ≥ 2 inherited levels is an ambiguity we defer.
        // `System.Object` declares no data members, so an Object-capped chain is
        // still complete for a data lookup.
        if self.entity(handle).kind == EntityKind::Interface {
            let chain = match self.interface_member_chain(handle) {
                InterfaceChain::Complete(c) | InterfaceChain::ObjectCapped(c) => c,
                InterfaceChain::Incomplete => return None,
            };
            let declaring: Vec<EntityHandle> = chain
                .iter()
                .skip(1) // `chain[0]` is the receiver, already checked above.
                .copied()
                .filter(|&level| self.data_member_at_level(level, name).is_some())
                .collect();
            return match declaring.as_slice() {
                [only] => self.data_member_at_level(*only, name).flatten(),
                _ => None, // zero or ≥ 2 inherited interfaces declare the name.
            };
        }
        // Not declared on the receiver — walk the base chain for an inherited member.
        let chain = match self.base_chain(handle) {
            BaseChain::Complete(c) | BaseChain::ObjectCapped(c) => c,
            BaseChain::Incomplete => return None,
        };
        // `chain[0]` is the receiver, already checked above; walk its bases.
        for level in chain.into_iter().skip(1) {
            if let Some(result) = self.data_member_at_level(level, name) {
                return result;
            }
        }
        None
    }

    /// Resolve `name` as a data member **declared at this one level** (no base walk).
    /// The outer `Option` says whether the level *declares* the name at all: `None`
    /// means it does not (the caller continues to a base); `Some(inner)` means it does
    /// — and, because a declared member *hides* inherited ones, `inner` is the final
    /// answer: `Some(..)` a resolved readable instance data member, or `None` a defer
    /// (an ambiguous name, a **static** — unreachable through a value receiver — or a
    /// method / event / indexer / write-only or non-public-getter property, each of
    /// which hides the inherited data member).
    #[allow(clippy::type_complexity)]
    fn data_member_at_level(
        &self,
        level: EntityHandle,
        name: &str,
    ) -> Option<Option<(EntityHandle, MemberIndex, &TypeRef)>> {
        // An **undecodable** member of the name at this level (dropped into
        // `skipped_members`) could be the real declaration — it may hide the inherited
        // member — but the reader couldn't decode it. Treat it as "declared here, but
        // defer" so the walk neither resolves it nor falls through to a hidden base.
        if self.has_skipped_member(level, name) {
            return Some(None);
        }
        // Statics are included in this declaring check (a public static of the name
        // hides the inherited instance member); non-public members are excluded
        // (cross-assembly, an inaccessible derived member is removed before hiding).
        let mut matching = self
            .entity(level)
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| member_name(m) == name && member_is_public(m));
        let (idx, first) = matching.next()?; // `None` → not declared at this level
        if matching.next().is_some() {
            return Some(None); // ambiguous at this level — defer
        }
        if member_is_static(first) {
            return Some(None);
        }
        let ty = match first {
            Member::Field(f) => &f.ty,
            // A **readable** non-indexer property only. An indexer carries index
            // parameters; a read (`recv.Name`) goes through the *getter*, so gate on
            // the **getter's own** accessibility being public — not the property-level
            // `access`, which a public setter can inflate above a `private get`
            // (`{ public set; private get; }` reports `access == Public` yet is
            // unreadable cross-assembly). A write-only property (`getter_access ==
            // None`) and a non-public getter both defer.
            Member::Property(p)
                if p.parameters.is_empty() && p.getter_access == Some(Access::Public) =>
            {
                &p.ty
            }
            // A method, event, indexer, write-only property, or non-public-getter
            // property of the name is not a readable public data member — and, declared
            // at this level, it *shadows* any inherited data member, so it defers.
            Member::Method(_) | Member::Property(_) | Member::Event(_) => return Some(None),
        };
        Some(Some((level, MemberIndex::new(idx), ty)))
    }

    /// Whether `level` records an **undecodable** member of `name` in
    /// [`Entity::skipped_members`] — a member the reader dropped because it could not
    /// decode its signature. Such a member could hide or overload the name, so a walk
    /// that sees only the decoded `members` (where it looks absent) must defer rather
    /// than resolve a base member. Matched on the IL name (a skipped member carries no
    /// decoded source name); an undecodable member that *also* carries a
    /// `[<CompiledName>]` rename is the vanishingly rare miss.
    fn has_skipped_member(&self, level: EntityHandle, name: &str) -> bool {
        self.entity(level)
            .skipped_members
            .iter()
            .any(|skipped| skipped.name == name)
    }

    /// The **[`MemberIndex`], return type, and parameter count** of the single
    /// **non-overloaded, non-generic public instance method** named `name` on
    /// `handle` — the target of a Stage-3.3d method call `recv.Method(args)`, whose
    /// *type* is the method's return type (independent of how the arguments *type*,
    /// but only when the argument list is well-formed — see the parameter count).
    /// The selection mirrors [`Self::instance_data_member`]'s ambiguity rule —
    /// exactly **one** public instance member of that name (so an overloaded method
    /// group, or a data-member/method name clash, defers) — and additionally
    /// requires that the one member is a **non-constructor, non-generic**
    /// [`Member::Method`]:
    ///
    /// - **Overloaded** (≥ 2 public instance members of the name) ⇒ `None` (F#
    ///   would need overload resolution — the B3 hard pile, deferred).
    /// - **Generic** (`generic_parameters` non-empty) ⇒ `None`: instantiation is a
    ///   later slice, and the return type would generally mention a method typar
    ///   the bridge cannot render anyway.
    /// - A **constructor** (`is_constructor`) ⇒ `None` (`.ctor` is not a member
    ///   expression on a value receiver).
    /// - A **field / property / event** of the name (or a **static** / non-public
    ///   method) ⇒ `None` (a data member is [`Self::instance_data_member`]'s job; a
    ///   static needs a type-qualified path; a non-public method is unreferenceable
    ///   cross-assembly).
    ///
    /// The returned [`TypeRef`] is the method's **return type**, which the caller
    /// bridges to a [`Ty`](crate::ty::Ty). A **`void`** return is returned as-is (the caller — the
    /// `HasMember` wake — records the method's identity but defers the `unit` type
    /// it cannot yet model); every other undecodable/unmodelled return likewise
    /// defers at the *bridge*, not here, so the member's identity is still recorded.
    ///
    /// **Inheritance (Stage 3.x-inh).** The method group is collected across
    /// `handle`'s whole base chain (`base_chain`), not just its own members, so an
    /// *inherited* single method resolves (`s.GetType()` ⇒ the `System.Object.GetType`
    /// return, returned under `Object`'s handle). Name hiding is by nearest declaring
    /// level: the level that first declares the name owns it, and must offer a public
    /// *instance method* — a same-name **static** or non-method there hides the
    /// inherited instance member but is unreachable through a value receiver, so the
    /// call defers. The group is collected from the owning level down (statics ignored)
    /// and **deduplicated by partial signature (OV-3)**: an override / covariant-return
    /// override / `new` re-declaration of the same name+params on a nearer level hides
    /// the inherited copy, so it collapses to the nearest member and an *overridden
    /// single method now resolves* (relaxing 3.x-inh's "no cross-level dedup"). The
    /// signature keys resolve each level's `assembly: None` references against that
    /// level's declaring assembly (`type_sig_key`) — the cross-assembly identity that
    /// makes the comparison sound. After dedup, ≥ 2 *distinct* members (a genuine
    /// overload set) is the B3 overload-resolution hard pile and defers. When the chain
    /// is *Object-capped* —
    /// `System.Object` absent from the env (a single-assembly view) — a call naming an
    /// `Object` method (`Equals`/`GetHashCode`/`GetType`/`ToString`) defers, since the
    /// inherited-but-invisible `Object` overload would make the group incomplete; every
    /// other name is unaffected. A generic / absent / wrong-assembly non-`Object` base
    /// defers the whole lookup.
    ///
    /// The trailing `usize` is the method's **parameter count**. The return type is
    /// the call's type only when the call is *well-formed*: an ill-arity call
    /// (`s.ToLowerInvariant(1)`, `s.Insert()`) is not typed by FCS as the method
    /// return but falls back to `obj`, so the caller gates on the supplied argument
    /// count matching this parameter count exactly (a conservative check — it also
    /// defers calls that omit `[<Optional>]` / `params` arguments, whose modelling is
    /// a later slice, but never emits a wrong type).
    pub fn instance_method(
        &self,
        handle: EntityHandle,
        name: &str,
    ) -> Option<(EntityHandle, MemberIndex, &TypeRef, usize)> {
        // Exactly one distinct member resolves; >= 2 (a genuine overload set —
        // distinct partial signatures) is the B3 overload-resolution hard pile
        // and defers *for this single-candidate entry point* (the OV-6 engine
        // consumes the multi-candidate group through
        // [`Self::instance_method_group`] directly). Every member is a public
        // instance method here, so only a constructor or a generic method still
        // declines.
        let group = self.instance_method_group(handle, name)?;
        let &(level, idx, m) = match group.as_slice() {
            [single] => single,
            _ => return None,
        };
        // A generic method or a constructor of that name still declines.
        if m.is_constructor || !m.generic_parameters.is_empty() {
            return None;
        }
        Some((
            level,
            idx,
            &m.signature.return_type,
            m.signature.parameters.len(),
        ))
    }

    /// The **complete, deduplicated public-instance method group** named `name`
    /// on a value receiver of `handle` — the OV-6 overload engine's candidate
    /// set (`docs/overload-resolution-plan.md` §4.1(1)(2)(3)(5)) — or `None`
    /// when the group is not provably complete, in which case the call must
    /// defer:
    ///
    /// - the base chain is `BaseChain::Incomplete` (a generic / absent /
    ///   wrong-assembly base makes the inherited group unknowable), or it is
    ///   `BaseChain::ObjectCapped` and `name` is an `Object` method (the
    ///   invisible `Object` overload would make the group incomplete) — both
    ///   private to the crate, hence the un-linked names;
    /// - an **undecodable** member of the name sits anywhere in the chain
    ///   (`Self::has_skipped_member`);
    /// - the nearest level declaring the name offers no usable public *instance
    ///   method* of it (only statics / a non-method — a member-kind clash we do
    ///   not model).
    ///
    /// Otherwise the returned group is non-empty and every element is a public
    /// instance method, deduplicated by partial signature (OV-3) so an override
    /// / covariant-return override / `new` re-declaration collapses to the
    /// nearest level. It may still contain a constructor or generic method (the
    /// engine's own [`AssemblyEnv::may_apply`] / [`AssemblyEnv::must_apply`]
    /// handle those), and — the whole point — may be a genuine overload set of
    /// **≥ 2** distinct members. See [`Self::instance_method`] for the
    /// single-candidate typing wrapper and the semantics prose the two share.
    ///
    /// Public because the group *is* the overload engine's input, and the OV-9
    /// coverage report (`crates/sema/tests/all/overload_corpus_diff.rs`) classifies a
    /// deferral by re-running the same primitives the engine does — the group,
    /// then [`Self::may_apply`] / [`Self::must_apply`] — rather than by
    /// re-deriving them.
    pub fn instance_method_group(
        &self,
        handle: EntityHandle,
        name: &str,
    ) -> Option<Vec<(EntityHandle, MemberIndex, &MethodLike)>> {
        self.method_group(handle, name, false)
    }

    /// The **static** sibling of [`Self::instance_method_group`] (stage OV-7): the
    /// complete, deduplicated public-**static** method group named `name` on a
    /// type-qualified receiver `handle` (`Type.Method(args)`). Statics share the
    /// instance machinery end-to-end — FCS resolves a qualified static call through
    /// the same hierarchy walk (probed 2026-07-10: `D.M 3` with `M` static on base
    /// `B` resolves to `B.M`, and an inherited static *competes in betterness*, so
    /// a derived-only scan would wrongly commit) — differing only in which members
    /// participate: a type-qualified path reaches only statics, so the group filter
    /// and the owning-level usability check flip to `is_static`.
    ///
    /// Public for the same reason as [`Self::instance_method_group`].
    pub fn static_method_group(
        &self,
        handle: EntityHandle,
        name: &str,
    ) -> Option<Vec<(EntityHandle, MemberIndex, &MethodLike)>> {
        self.method_group(handle, name, true)
    }

    /// The shared walk behind [`Self::instance_method_group`] /
    /// [`Self::static_method_group`]: `want_static` selects which membership kind
    /// participates (a type-qualified path reaches only statics; a value receiver
    /// only instance members). Everything else — the chain-completeness gates, the
    /// nearest-declaring-level ownership rule, and the partial-signature dedup — is
    /// identical, mirroring FCS's one `ResolveLongIdentInTypePrim` walk.
    fn method_group(
        &self,
        handle: EntityHandle,
        name: &str,
        want_static: bool,
    ) -> Option<Vec<(EntityHandle, MemberIndex, &MethodLike)>> {
        // An **interface receiver** builds its group from `System.Object`'s members
        // *plus* all transitively inherited interfaces (§2.1) — a distinct walk
        // ([`Self::interface_member_chain`]) with a distinct hiding rule, so it goes
        // through its own path. **Static** interface calls stay deferred: static
        // abstract/virtual interface member lookup has different rules again (a
        // non-goal — `docs/interface-walk-plan.md`).
        if self.entity(handle).kind == EntityKind::Interface {
            return if want_static {
                None
            } else {
                self.interface_method_group(handle, name)
            };
        }
        let chain = match self.base_chain(handle) {
            BaseChain::Complete(c) => c,
            BaseChain::ObjectCapped(c) => {
                // `System.Object` is absent from this env, so its universal instance
                // methods are invisible. A call naming one competes with the
                // inherited-but-unseen `Object` member, so its group is incomplete —
                // defer. Any other name is unaffected.
                if is_object_method_name(name) {
                    return None;
                }
                c
            }
            BaseChain::Incomplete => return None,
        };
        // An **undecodable** member of the name anywhere in the chain (dropped into
        // `skipped_members`) could hide or overload the group, but the reader couldn't
        // decode it — so defer rather than resolve past it.
        if chain
            .iter()
            .any(|&level| self.has_skipped_member(level, name))
        {
            return None;
        }
        // Name hiding is by name, per *declaring level*: the nearest level that
        // declares the name (any public member) owns it and hides every inherited
        // member of the name below. The owning level must offer a usable public
        // method of the wanted kind; if its public members of the name are only
        // of the **other** membership kind or a **non-method** (field / property /
        // event), they hide the inherited members but cannot be reached this way —
        // a static is unreachable through a value receiver, an instance member
        // unreachable through a type-qualified path (FCS leaves such a call `obj`)
        // — so the call defers. A wrong-kind member that merely *coexists* with a
        // right-kind method of the name at the owning level (an overload set —
        // e.g. `Object.Equals(object)` instance + static `Equals(object, object)`)
        // does **not** hide: it is ignored for this call, not a blocker.
        let Some(start) = chain.iter().position(|&level| {
            self.entity(level)
                .members
                .iter()
                .any(|m| member_name(m) == name && member_is_public(m))
        }) else {
            return None; // the name is declared nowhere in the chain
        };
        // The owning level's public right-kind members of the name must be **all
        // methods** and at least one: a same-level non-method (a field / property /
        // event sharing the name — illegal in C# but representable in metadata) is a
        // member-kind clash F# resolves by precedence we do not model, so it defers,
        // as does an owner whose members of the name are only wrong-kind /
        // non-method (which hide the inherited member but are unreachable through
        // this call shape). Wrong-kind members are excluded here — one that merely
        // coexists with a right-kind method (an overload set) is ignored, not a
        // clash.
        let owning: Vec<&Member> = self
            .entity(chain[start])
            .members
            .iter()
            .filter(|m| {
                member_name(m) == name && member_is_public(m) && member_is_static(m) == want_static
            })
            .collect();
        if owning.is_empty() || owning.iter().any(|m| !matches!(m, Member::Method(_))) {
            return None;
        }
        // Collect the public method group of the wanted kind named `name` from the
        // owning level down through its bases (wrong-kind members and non-methods
        // are ignored — they do not participate in this call shape, and a deeper
        // non-method is hidden by the owning level's method), **nearest level
        // first**.
        let mut group: Vec<(EntityHandle, MemberIndex, &MethodLike)> = Vec::new();
        for &level in &chain[start..] {
            for (idx, m) in self.entity(level).members.iter().enumerate() {
                if let Member::Method(mm) = m
                    && member_is_public(m)
                    && member_is_static(m) == want_static
                    // Match on the *source* name (`member_name` honours a
                    // `[<CompiledName>]` / `CompilationSourceName` rename), the same
                    // comparison the owning-level checks above use — not the raw IL
                    // `mm.name`, which would drop a renamed member the caller found
                    // by its F# source identifier.
                    && member_name(m) == name
                {
                    group.push((level, MemberIndex::new(idx), mm));
                }
            }
        }
        // **Inherited-overload hiding / override dedup (OV-3).** F# hides an
        // inherited method that a *nearer* level re-declares with the *same partial
        // signature* (`MethInfosEquivByNameAndPartialSig`, §2.1) — a plain override,
        // a covariant-return override, or a `new` re-declaration all collapse to the
        // nearest level's member, which is the one a value receiver of this type
        // calls. So drop a member whose partial key was already claimed by a
        // **strictly nearer** level. Keying resolves each level's `assembly: None`
        // references against that level's declaring assembly ([`type_sig_key`]),
        // which is exactly the cross-assembly identity the 3.x-inh code declined to
        // decide — now sound.
        //
        // Hiding is **only across levels**: two members with the same partial key on
        // the *same* declaring level (raw IL can carry MethodDefs differing only by
        // return type) are an ambiguous / unsupported group, kept distinct so the
        // group stays ≥ 2 and defers — never collapsed into one wrong candidate. A
        // member whose signature cannot be keyed ([`method_partial_key`] returns
        // `None`) gets a unique synthetic key, so it never collapses either.
        let mut claimed: HashMap<String, EntityHandle> = HashMap::new();
        let mut deduped: Vec<(EntityHandle, MemberIndex, &MethodLike)> = Vec::new();
        for (i, &(level, idx, mm)) in group.iter().enumerate() {
            let key = match method_partial_key(mm, &self.entity(level).assembly.name) {
                None => format!("?unkeyable:{i}"),
                Some(k) => k,
            };
            // Hidden only if a *different* (necessarily nearer, since we walk
            // nearest-first) level already claimed this partial signature.
            if let Some(&claimed_level) = claimed.get(&key)
                && claimed_level != level
            {
                continue;
            }
            claimed.entry(key).or_insert(level);
            deduped.push((level, idx, mm));
        }
        Some(deduped)
    }

    /// The public-instance method group named `name` on an **interface receiver**
    /// (`want_static == false` only; statics are declined by the caller). FCS's
    /// group is `System.Object`'s members plus all transitively inherited
    /// interfaces (§2.1); [`Self::interface_member_chain`] enumerates those levels.
    ///
    /// **v1 rule — no cross-DAG hiding.** Unlike a base-class chain, an interface
    /// DAG has *sibling* levels not in any subtype relation, so nearer-hides-farther
    /// is unsound. The name must therefore be declared on **exactly one** level of
    /// the closure (own or a single inherited interface, or `System.Object`); ≥ 2
    /// declaring levels is an ambiguity we defer (a re-declaration FCS would hide
    /// down to one is a sound-but-incomplete miss, recovered by the optional IW-3).
    /// The group is that one level's method(s) of the name — an overload set on a
    /// single interface is a legitimate group fed to the OV engine — with no
    /// cross-level collection or dedup, since only one level declares the name.
    ///
    /// Declines (defers) when: the closure is [`InterfaceChain::Incomplete`]; it is
    /// [`InterfaceChain::ObjectCapped`] and `name` is an `Object` method (the
    /// invisible `Object` overload would make the group incomplete); an undecodable
    /// member of the name sits anywhere in the closure; the name is declared on ≠ 1
    /// level; or the single declaring level's public members of the name are not all
    /// instance methods (a member-kind clash, or a wrong-kind member that hides the
    /// method — the same owning-level rule the base-class path uses).
    fn interface_method_group(
        &self,
        handle: EntityHandle,
        name: &str,
    ) -> Option<Vec<(EntityHandle, MemberIndex, &MethodLike)>> {
        let chain = match self.interface_member_chain(handle) {
            InterfaceChain::Complete(c) => c,
            InterfaceChain::ObjectCapped(c) => {
                // `System.Object` absent: its universal instance methods are
                // invisible, so a call naming one has an incomplete group — defer.
                if is_object_method_name(name) {
                    return None;
                }
                c
            }
            InterfaceChain::Incomplete => return None,
        };
        // An undecodable member of the name anywhere in the closure could hide or
        // overload the group — defer rather than resolve past it.
        if chain
            .iter()
            .any(|&level| self.has_skipped_member(level, name))
        {
            return None;
        }
        // The name must be declared (any public member) on exactly one level.
        let declaring: Vec<EntityHandle> = chain
            .iter()
            .copied()
            .filter(|&level| {
                self.entity(level)
                    .members
                    .iter()
                    .any(|m| member_name(m) == name && member_is_public(m))
            })
            .collect();
        let [owner] = declaring.as_slice() else {
            return None;
        };
        let owner = *owner;
        // The single declaring level must offer usable public *instance methods* of
        // the name: at least one, and no wrong-kind member of the name (a field /
        // property / event, which — declared here — hides the inherited method but
        // cannot be reached as a call). This is the base-class path's owning-level
        // rule, applied to the one interface level that owns the name.
        let owning: Vec<&Member> = self
            .entity(owner)
            .members
            .iter()
            .filter(|m| member_name(m) == name && member_is_public(m) && !member_is_static(m))
            .collect();
        if owning.is_empty() || owning.iter().any(|m| !matches!(m, Member::Method(_))) {
            return None;
        }
        // The group is that level's instance method(s) of the name — no cross-level
        // dedup (only one level declares it). An overload set on the single interface
        // stays ≥ 2 and is handed to the OV engine unchanged.
        let group: Vec<(EntityHandle, MemberIndex, &MethodLike)> = self
            .entity(owner)
            .members
            .iter()
            .enumerate()
            .filter_map(|(idx, m)| match m {
                Member::Method(mm)
                    if member_is_public(m) && !member_is_static(m) && member_name(m) == name =>
                {
                    Some((owner, MemberIndex::new(idx), mm))
                }
                _ => None,
            })
            .collect();
        Some(group)
    }

    /// Walk `handle`'s base-type chain, nearest first, resolving each base to its
    /// interned handle. See [`BaseChain`] for the three outcomes. Only *non-generic,
    /// top-level, present* bases extend the chain ([`Self::resolve_base`]); the
    /// universal `System.Object`, commonly absent from a single-assembly env, caps it
    /// ([`BaseChain::ObjectCapped`]); any other unresolvable / generic base makes the
    /// inherited group unknowable ([`BaseChain::Incomplete`]). Bounded against
    /// corrupt self-referential metadata: a base already in the chain ends the walk
    /// as `Incomplete`.
    fn base_chain(&self, handle: EntityHandle) -> BaseChain {
        let mut chain = vec![handle];
        let mut current = handle;
        loop {
            let entity = self.entity(current);
            let base = match &entity.base_type {
                None => return BaseChain::Complete(chain),
                Some(base) => base,
            };
            match self.resolve_base(base, &entity.assembly.name) {
                Some(next) if !chain.contains(&next) => {
                    chain.push(next);
                    current = next;
                }
                // A resolvable base already in the chain is a metadata cycle — stop.
                Some(_) => return BaseChain::Incomplete,
                // Unresolvable: the universal `System.Object` caps the chain; any
                // other absent / generic / nested / wrong-assembly base sinks it.
                None if is_system_object(base) => return BaseChain::ObjectCapped(chain),
                None => return BaseChain::Incomplete,
            }
        }
    }

    /// The member-source levels an **interface receiver** sees, deduplicated by
    /// handle: the receiver interface, its transitively inherited interfaces (a
    /// breadth-first walk over each level's `interfaces`), and `System.Object`
    /// appended last. See [`InterfaceChain`] for the completeness outcomes and
    /// `docs/interface-walk-plan.md` for why this is interface-receiver-only.
    ///
    /// Any inherited interface `resolve_base` cannot resolve — a generic
    /// (`IEnumerable<T>`), nested, absent, or wrong-assembly one — sinks the whole
    /// walk to [`InterfaceChain::Incomplete`]: an invisible inherited member could
    /// hide or ambiguate the lookup, so the surface is unknowable. Bounded against
    /// metadata cycles by the `seen` set.
    ///
    /// **v1 does not hide across the interface DAG.** Two *sibling* interfaces are
    /// not in a subtype relation, so the base chain's nearer-hides-farther rule is
    /// unsound here; the returned level order is therefore informational, and the
    /// consumers ([`Self::instance_data_member`] / [`Self::method_group`]) apply an
    /// *exactly-one-declaring-level* rule instead of cross-level dedup.
    fn interface_member_chain(&self, handle: EntityHandle) -> InterfaceChain {
        debug_assert_eq!(self.entity(handle).kind, EntityKind::Interface);
        let mut levels: Vec<EntityHandle> = vec![handle];
        let mut seen: HashSet<EntityHandle> = HashSet::from([handle]);
        let mut i = 0;
        while i < levels.len() {
            let level = levels[i];
            i += 1;
            let entity = self.entity(level);
            let declaring = &entity.assembly.name;
            for iface in &entity.interfaces {
                match self.resolve_base(iface, declaring) {
                    Some(next) => {
                        if seen.insert(next) {
                            levels.push(next);
                        }
                    }
                    None => return InterfaceChain::Incomplete,
                }
            }
        }
        // `System.Object` is the universal member source an interface receiver also
        // sees. Look it up by name — but only trust the first-wins `by_type` slot
        // when it really holds the universal root, **not** a same-FQN impostor a
        // user assembly happens to define (and enumerate) first. The
        // assembly-identity guard that protects [`Self::base_chain`] (via
        // [`Self::resolve_base`] on the base edge) is unavailable here: an interface
        // has no base edge to Object. The genuine root is the unique **base-less
        // class** — any impostor `System.Object` extends the real one (so carries a
        // `base_type`), and an interface / struct / enum / delegate / module of the
        // name is the wrong `kind`. When the slot is absent or unprovable, cap
        // ([`InterfaceChain::ObjectCapped`]): `Object`'s members defer, but the
        // interface's own and inherited members still resolve.
        match self.lookup_type(&[String::from("System")], "Object", 0) {
            Some(obj)
                if self.entity(obj).kind == EntityKind::Class
                    && self.entity(obj).base_type.is_none() =>
            {
                levels.push(obj);
                InterfaceChain::Complete(levels)
            }
            _ => InterfaceChain::ObjectCapped(levels),
        }
    }

    /// Resolve a base-type [`TypeRef`] to an interned handle, but only for the
    /// **non-generic, top-level** named types the chain walk can complete soundly. A
    /// generic base (its members may mention type parameters we cannot yet
    /// substitute), a nested base, a primitive, or an absent one yields `None`.
    ///
    /// Honours **assembly identity**: `by_type` keeps only the first-enumerated
    /// definition per `(namespace, name, arity)`, so when two referenced assemblies
    /// define the same full type name — or the base's assembly is absent while a
    /// same-named type from another is present — that slot may be the *wrong* type.
    /// The base `TypeRef` names its assembly (or is `assembly: None`, meaning the
    /// *declaring* type's own assembly `declaring_assembly`); resolution declines
    /// unless the candidate's assembly **name** matches. Name — not the full
    /// `(name, version, public_key_token)` identity — because a base is compiled
    /// against a possibly-different *version* than the one loaded, and the compiler
    /// binds by name with version redirection; matching the full identity would
    /// wrongly defer that ordinary case.
    pub(crate) fn resolve_base(
        &self,
        base: &TypeRef,
        declaring_assembly: &str,
    ) -> Option<EntityHandle> {
        match base {
            TypeRef::Named {
                assembly,
                namespace,
                name,
                type_args,
                segment_arities,
            } if type_args.is_empty()
                && segment_arities.iter().all(|&a| a == 0)
                && !name.contains('/') =>
            {
                let candidate = self
                    .by_type
                    .get(&(namespace.clone(), name.clone(), 0))
                    .copied()?;
                let expected = assembly
                    .as_ref()
                    .map_or(declaring_assembly, |a| a.name.as_str());
                (self.entity(candidate).assembly.name == expected).then_some(candidate)
            }
            _ => None,
        }
    }

    /// The transitive **strict** supertypes of `start` — every resolvable base
    /// class and (transitively) implemented interface, excluding `start` itself.
    ///
    /// The OV-5 applicability matcher's `must_apply` subtype affirmation consults
    /// this: an argument type `A` *affirms* a parameter type `P` when `P` is a
    /// supertype of `A` (a boxing `int :> obj`, or an interface `A` implements).
    /// A *positive* membership is sound even from a **partial** walk — the
    /// resolved bases/interfaces are genuine supertypes regardless of what could
    /// not be resolved — so, unlike [`Self::base_chain`], this needs no
    /// completeness signal: it collects whatever [`Self::resolve_base`] resolves
    /// (non-generic, top-level, present base classes and interfaces) and stops at
    /// the rest. Missing a supertype only weakens `must_apply` into a deferral,
    /// never a wrong affirmation. Bounded against metadata cycles by the visited
    /// set.
    pub(crate) fn super_types(&self, start: EntityHandle) -> HashSet<EntityHandle> {
        let mut out = HashSet::new();
        let mut stack = vec![start];
        while let Some(handle) = stack.pop() {
            if !out.insert(handle) {
                continue;
            }
            let entity = self.entity(handle);
            let declaring = &entity.assembly.name;
            if let Some(base) = &entity.base_type
                && let Some(next) = self.resolve_base(base, declaring)
            {
                stack.push(next);
            }
            for iface in &entity.interfaces {
                if let Some(next) = self.resolve_base(iface, declaring) {
                    stack.push(next);
                }
            }
        }
        out.remove(&start);
        out
    }

    /// The **distinct display names of `handle`'s public instance members** —
    /// fields, non-indexer properties, **and** methods — the candidate set
    /// dot-completion offers on a value receiver of this type (Stage 3.3b). A
    /// completion list is a set of genuinely-callable candidates, not a type
    /// assertion, so methods belong even though [`Self::instance_data_member`]
    /// (which *types* an access) admits only data members. Static members are
    /// excluded (they need a type-qualified path, not a value receiver); an
    /// overloaded name appears once (deduplicated, first occurrence kept).
    /// Exact-entity only — unlike the *typing* path ([`Self::instance_method`] /
    /// [`Self::instance_data_member`], which walk the base chain), completion does
    /// not yet offer inherited members; a follow-up slice can reuse
    /// `base_chain` here.
    ///
    /// Accessibility: a member is included only when public; a property gates on
    /// its **getter** being public (a `private get`-only or write-only property is
    /// not readably completable from another assembly), reusing the same
    /// getter-accessibility rule as [`Self::instance_data_member`].
    pub fn public_instance_member_names(&self, handle: EntityHandle) -> Vec<&str> {
        let mut seen = HashSet::new();
        self.entity(handle)
            .members
            .iter()
            .filter(|m| !member_is_static(m) && member_is_public(m) && member_is_readable(m))
            .map(member_name)
            .filter(|name| seen.insert(*name))
            .collect()
    }

    fn member_where(
        &self,
        handle: EntityHandle,
        name: &str,
        pred: impl Fn(&Member) -> bool,
    ) -> Option<MemberIndex> {
        self.entity(handle)
            .members
            .iter()
            .position(|m| member_name(m) == name && pred(m))
            .map(MemberIndex::new)
    }

    /// The member an [`EntityHandle`] + [`MemberIndex`] names.
    pub fn member_at(&self, handle: EntityHandle, idx: MemberIndex) -> &Member {
        &self.entity(handle).members[idx.index()]
    }

    /// The **F# source name** the member at `(handle, idx)` is referenced by —
    /// the counterpart to [`Self::member`], which looks a member up *by* that
    /// name. For a method this is its `[<CompiledName>]`/`CompilationSourceName`
    /// when the IL name was rewritten (`printfn`, not the compiled
    /// `PrintFormatLine`); for fields, properties, and events the IL name *is*
    /// the source name. The LSP renders this in hovers.
    pub fn member_display_name(&self, handle: EntityHandle, idx: MemberIndex) -> &str {
        member_name(self.member_at(handle, idx))
    }
}

/// The **F# source name** a member is referenced by, across the four kinds.
/// For a method this is its `CompilationSourceName` (`printfn`) when the IL
/// renamed it (`PrintFormatLine`), else the IL name; the other kinds have no
/// such attribute, so their IL name *is* the source name. Matching on the
/// source name is correct, not merely additive: once F# sets `[<CompiledName>]`
/// the compiled name is not a legal source identifier.
fn member_name(member: &Member) -> &str {
    match member {
        Member::Method(m) => m.source_name.as_deref().unwrap_or(&m.name),
        Member::Field(f) => &f.name,
        Member::Property(p) => &p.name,
        Member::Event(e) => &e.name,
    }
}

/// Whether a member is `static` (accessible through `Type.Member` without a
/// receiver), across the four member kinds.
fn member_is_static(member: &Member) -> bool {
    match member {
        Member::Method(m) => m.is_static,
        Member::Field(f) => f.is_static,
        Member::Property(p) => p.is_static,
        Member::Event(e) => e.is_static,
    }
}

/// Whether a member is `public` — required for a cross-assembly reference.
fn member_is_public(member: &Member) -> bool {
    let access = match member {
        Member::Method(m) => m.access,
        Member::Field(f) => f.access,
        Member::Property(p) => p.access,
        Member::Event(e) => e.access,
    };
    access == Access::Public
}

/// Whether a base-type [`TypeRef`] names `System.Object` — the universal root at
/// which every inheritance chain terminates. Recognised structurally so the chain
/// walk ([`AssemblyEnv::base_chain`]) can treat it as a terminator even when
/// `Object` is absent from a single-assembly env ([`BaseChain::ObjectCapped`]).
fn is_system_object(base: &TypeRef) -> bool {
    matches!(
        base,
        TypeRef::Named { namespace, name, type_args, segment_arities, .. }
            if name == "Object"
                && namespace.len() == 1
                && namespace[0] == "System"
                // Only the genuine *non-generic* `System.Object` is the universal root.
                // A (hand-written / corrupt) generic `System.Object<T>` is an unknown
                // base like any other — it must not cap the chain as complete.
                && type_args.is_empty()
                && segment_arities.iter().all(|&a| a == 0)
    )
}

/// Whether `name` is one of `System.Object`'s **public** methods — the instance
/// `Equals` / `GetHashCode` / `GetType` / `ToString`, or the static
/// `Equals(object, object)` / `ReferenceEquals` — which every type inherits.
/// (`Finalize` / `MemberwiseClone` are protected and never participate in a
/// cross-assembly call.) A call of one of these names always competes with the
/// inherited `Object` member — through a value receiver for the instance ones,
/// through a type-qualified static path (stage OV-7) for the static ones — so it
/// is only typeable when `Object` itself is in the env — see
/// [`BaseChain::ObjectCapped`]. One kind-agnostic set is deliberate: a blanket
/// defer of the wrong kind's name only widens deferral, never a wrong commit.
fn is_object_method_name(name: &str) -> bool {
    matches!(
        name,
        "Equals" | "GetHashCode" | "GetType" | "ToString" | "ReferenceEquals"
    )
}

/// Whether a member is **completable by name** on a value receiver — a
/// (non-constructor) method, a field, or a **non-indexer** property whose
/// *getter* is public (Stage 3.3b dot-completion). Excluded: a **constructor**
/// (`.ctor` — not a member expression on a value receiver); a write-only or
/// `private get`-only property (it cannot be read through `recv.Name`, so it is
/// not a usable candidate); an **indexer** (a parameterised property is accessed
/// as `recv.[i]`, not `recv.Item`, so its name would mislead); and an event
/// (`recv.Name` is not a plain member expression). The getter / indexer rules
/// mirror [`AssemblyEnv::instance_data_member`]'s.
fn member_is_readable(member: &Member) -> bool {
    match member {
        Member::Method(m) => !m.is_constructor,
        Member::Field(_) => true,
        Member::Property(p) => p.parameters.is_empty() && p.getter_access == Some(Access::Public),
        Member::Event(_) => false,
    }
}

/// A structural signature key for one [`TypeRef`], with a `Named`'s
/// `assembly: None` resolved to `declaring` — the declaring-assembly name of the
/// type whose member carries this type. An `assembly: None` reference is
/// relative to *its own* declaring assembly, so two `TypeRef`s from different
/// base-chain levels (potentially different assemblies) only compare soundly
/// after this resolution — the OV-3 cross-assembly identity fix that made the
/// 3.x-inh "no cross-level dedup" caution unnecessary.
///
/// Returns `None` for a shape it cannot canonicalise confidently; the caller
/// then treats the two signatures as *not provably equal* and declines to
/// collapse them (the conservative, never-wrong direction). Nullability is
/// ignored (erased for overload resolution). A **byref** referent (`int&`) is
/// kept **distinct** from its by-value form (`int`) — `M(ref int)` and `M(int)`
/// are different signatures FCS keeps apart; only the byref *kind* (ref / out /
/// in) is collapsed, at the parameter level in [`method_partial_key`], matching
/// `MethInfosEquivByNameAndSig`. Nested generic **segment arities** participate
/// (`Outer<int>.Inner` vs `Outer.Inner<int>` differ) since `name` has the
/// backtick-arity suffixes stripped and `type_args` is flat. A **bounded** array
/// (`sizes`/`lower_bounds`) keys distinctly from a plain vector of the same
/// element+rank.
///
/// Every struct-variant arm destructures **all** fields (no `..`), so adding a
/// field to [`TypeRef`] is a compile error here rather than a silently-dropped
/// distinguisher that would conflate two signatures — the recurring bug class
/// this closes ("have the machine enforce the invariant").
fn type_sig_key(ty: &TypeRef, declaring: &str) -> Option<String> {
    match ty {
        TypeRef::Primitive(p) => Some(format!("p:{p:?}")),
        TypeRef::Var { index, is_method } => {
            Some(format!("v:{}:{index}", if *is_method { 'm' } else { 't' }))
        }
        TypeRef::Named {
            assembly,
            namespace,
            name,
            type_args,
            segment_arities,
        } => {
            let asm = assembly.as_ref().map_or(declaring, |a| a.name.as_str());
            let mut s = format!(
                "n:{asm}:{}:{name}#{segment_arities:?}(",
                namespace.join(".")
            );
            for ta in type_args {
                s.push_str(&type_sig_key(&ta.ty, declaring)?);
                s.push(',');
            }
            s.push(')');
            Some(s)
        }
        TypeRef::Array {
            element,
            rank,
            sizes,
            lower_bounds,
        } => Some(format!(
            "a{rank}s{sizes:?}l{lower_bounds:?}:{}",
            type_sig_key(&element.ty, declaring)?
        )),
        // A byref referent is a distinct type (`int&` ≠ `int`); mark it so. The
        // read-only bit (`modreq(InAttribute)` — `in int&` vs `ref int&`) is part
        // of the signature the CLI matches on, so it keys distinctly too.
        TypeRef::ByRef { inner, readonly } => Some(format!(
            "&{}{}",
            if *readonly { "ro:" } else { "" },
            type_sig_key(inner, declaring)?
        )),
        TypeRef::Ptr(Some(inner)) => Some(format!("*{}", type_sig_key(inner, declaring)?)),
        TypeRef::Ptr(None) => Some("*void".to_string()),
    }
}

/// The **partial signature key** of a method — generic arity + parameter types
/// (return type ignored), the currency of F#'s inherited-overload *hiding*
/// (`MethInfosEquivByNameAndPartialSig`, §2.1 of the overload plan). Two methods
/// of the *same name* on different base-chain levels with an equal partial key
/// are the same group member: a plain override, a covariant-return override, or
/// a `new` re-declaration — FCS hides the inherited (deeper) one, keeping the
/// nearest. Keyed relative to `declaring` so cross-assembly levels compare.
///
/// `None` if any parameter type is unkeyable (see [`type_sig_key`]); the caller
/// then does not collapse, so an unrepresentable signature never causes a wrong
/// (over-eager) resolution.
fn method_partial_key(m: &MethodLike, declaring: &str) -> Option<String> {
    let mut s = format!("g{}(", m.generic_parameters.len());
    for p in &m.signature.parameters {
        // The projector carries byref/out-ness on the `Parameter` flag, leaving
        // `p.ty` as the *referent*, so a byref parameter (`int&`) must be keyed
        // distinctly from its by-value form (`int`) here — else `M(ref int)` and
        // `M(int)` would wrongly collapse. The byref *kind* (ref / out / in) is
        // collapsed to one `&` marker (matching FCS's `MethInfosEquivByNameAndSig`);
        // over-distinguishing would only defer more, never mis-resolve.
        if p.is_byref {
            s.push('&');
        }
        s.push_str(&type_sig_key(&p.ty, declaring)?);
        s.push(',');
    }
    s.push(')');
    Some(s)
}
#[cfg(test)]
mod presence_table_tests {
    use super::{Certainty, Channel, ExtensionKind, Presence, presence};

    /// The whole extension-visibility rule, spelled out cell by cell — a snapshot of
    /// [`presence`], so changing the rule is a deliberate edit here rather than a
    /// side-effect of touching one lookup.
    ///
    /// This does not *justify* the cells: the FCS matrix
    /// (`tests/all/extension_visibility_matrix.rs`) does, by diffing every one of them
    /// against the real compiler through every channel. What this pins is that the
    /// table remains the single place the rule lives, and that the two channels stay
    /// genuinely different — the `CSharpStyle` row, absent bare and present qualified,
    /// is the reason [`Channel`] exists at all.
    #[test]
    fn the_rule_is_this_table_and_nothing_else() {
        use Certainty::{Certain, Possible};
        use Channel::{Bare, Qualified};
        use ExtensionKind::{Augmentation, CSharpStyle, Ordinary};
        use Presence::{Absent, Present, Uncertain};

        // (kind, bare, qualified) — every inhabitant of `ExtensionKind`.
        let table = [
            (Ordinary, Present, Present),
            // Reachable only through the dot on a value: in neither channel.
            (Augmentation(Certain), Absent, Absent),
            (Augmentation(Possible), Uncertain, Uncertain),
            // The asymmetric row: out of the unqualified environment, but
            // `Enumerable.Select(xs, f)` compiles.
            (CSharpStyle(Certain), Absent, Present),
            (CSharpStyle(Possible), Uncertain, Present),
        ];

        for (kind, bare, qualified) in table {
            assert_eq!(presence(kind, Bare), bare, "bare presence of {kind:?}");
            assert_eq!(
                presence(kind, Qualified),
                qualified,
                "qualified presence of {kind:?}"
            );
        }
    }
}

#[cfg(test)]
mod active_pattern_banana_tests {
    use super::active_pattern_banana;
    use crate::resolve::ActivePatternShape;
    use proptest::prelude::*;

    fn shape(total: bool, single_case: bool) -> ActivePatternShape {
        ActivePatternShape {
            total,
            single_case,
            arity: None,
        }
    }

    /// Every derivation the fold relies on, spelled out — the demangle table that
    /// `docs/export-decl-model-plan.md` Stage 3b pins. Arity is always `None`
    /// (the flattened IL parameter count over-counts under tupling).
    #[test]
    fn the_derivation_is_this_table() {
        // (mangled IL name, tags, shape)
        let ok: &[(&str, &[&str], ActivePatternShape)] = &[
            ("|Even|Odd|", &["Even", "Odd"], shape(true, false)),
            ("|Scale|", &["Scale"], shape(true, true)),
            ("|DivBy|_|", &["DivBy"], shape(false, true)),
            ("|Nonempty|_|", &["Nonempty"], shape(false, true)),
            ("|A|B|C|", &["A", "B", "C"], shape(true, false)),
            ("|A|B|C|_|", &["A", "B", "C"], shape(false, false)),
            // A zero-tag partial recognizer (the quoted `` `|_|` ``): well-formed,
            // contributes no case tags, must NOT poison the surface (codex 5b).
            ("|_|", &[], shape(false, false)),
            // A nonterminal `_` is a real tag (only the LAST `_` is the partial
            // marker); the surface must not be poisoned (codex 6b).
            ("|_|A|", &["_", "A"], shape(true, false)),
            ("|A|_|B|", &["A", "_", "B"], shape(true, false)),
        ];
        for (name, tags, sh) in ok {
            let expected: Vec<&str> = tags.to_vec();
            assert_eq!(
                active_pattern_banana(name),
                Some((expected, *sh)),
                "banana {name:?}"
            );
        }

        // Malformed → no shape (residue, today's behaviour).
        for bad in [
            "",          // empty
            "|",         // one delimiter
            "||",        // empty inner
            "|A||B|",    // empty middle segment
            "Even|Odd|", // no leading delimiter
            "|Even|Odd", // no trailing delimiter
            "Even",      // not a banana at all
        ] {
            assert_eq!(active_pattern_banana(bad), None, "malformed {bad:?}");
        }
    }

    /// A generator over well-formed banana names: a mangle → demangle round-trip
    /// recovers the tags, totality and single-case, with arity always `None`.
    fn tags_and_totality() -> impl Strategy<Value = (Vec<String>, bool)> {
        (
            prop::collection::vec("[A-Za-z][A-Za-z0-9]{0,4}", 1..4),
            any::<bool>(),
        )
    }

    proptest! {
        #[test]
        fn mangle_roundtrips_through_demangle((tags, total) in tags_and_totality()) {
            // Mangle exactly as fsc does: `|A|B|` (total) / `|A|B|_|` (partial).
            let joined = tags.join("|");
            let mangled = if total {
                format!("|{joined}|")
            } else {
                format!("|{joined}|_|")
            };
            let single_case = tags.len() == 1;
            let recovered = active_pattern_banana(&mangled);
            prop_assert_eq!(
                recovered.as_ref().map(|(t, _)| t.clone()),
                Some(tags.iter().map(String::as_str).collect::<Vec<_>>())
            );
            prop_assert_eq!(recovered.map(|(_, s)| s), Some(shape(total, single_case)));
        }

        /// A name with any empty `|`-segment is malformed and attaches no shape —
        /// so an unlistable assembly recognizer defers rather than mis-splitting.
        #[test]
        fn empty_segment_is_never_a_shape(prefix in "[A-Za-z]{1,3}", suffix in "[A-Za-z]{1,3}") {
            let mangled = format!("|{prefix}||{suffix}|");
            prop_assert_eq!(active_pattern_banana(&mangled), None);
        }
    }
}

#[cfg(test)]
mod from_views_tests {
    use super::{AssemblyEnv, InterfaceChain};
    use borzoi_assembly::{
        AbbreviationTarget, Access, AssemblyIdentity, AssemblyProjectionSkips, EcmaView, Entity,
        EntityKind, FSharpResource, ImportError, Member, Nullability, Primitive, Property,
        SkippedProjectionItem, TypeRef, Version,
    };

    fn ident(name: &str) -> AssemblyIdentity {
        AssemblyIdentity {
            name: name.to_string(),
            version: Version {
                major: 0,
                minor: 0,
                build: 0,
                revision: 0,
            },
            public_key_token: None,
        }
    }

    /// A minimal [`EcmaView`] that projects **no** types but reports one **dropped**
    /// type in a chosen namespace — to check `from_views` propagates the drop.
    struct FakeView {
        identity: AssemblyIdentity,
        dropped_fqn: String,
    }

    impl EcmaView for FakeView {
        fn identity(&self) -> &AssemblyIdentity {
            &self.identity
        }
        fn assembly_refs(&self) -> Vec<AssemblyIdentity> {
            vec![]
        }
        fn enumerate_type_defs_with_skips(
            &self,
        ) -> Result<(Vec<Entity>, AssemblyProjectionSkips), ImportError> {
            Ok((
                vec![],
                AssemblyProjectionSkips {
                    dropped_types: vec![SkippedProjectionItem {
                        name: self.dropped_fqn.clone(),
                        reason: "undecodable".to_string(),
                    }],
                    skipped_fsharp_overlays: vec![],
                    fsharp_abbreviations_unknowable: false,
                    fsharp_extension_index_unknowable: false,
                    fsharp_signature_non_authoritative: false,
                },
            ))
        }
        fn assembly_auto_opens(&self) -> Result<Vec<String>, ImportError> {
            Ok(vec![])
        }
        fn fsharp_resources(&self) -> Result<Vec<FSharpResource>, ImportError> {
            Ok(vec![])
        }
    }

    /// A configurable [`EcmaView`] used to reproduce the two ways an
    /// assembly-level auto-open can point at a type the projection **dropped** —
    /// so the surviving entity tree the extension gate walks no longer shows it,
    /// while FCS (which reads the whole assembly) still imports it. Both are
    /// [`AssemblyEnv::extension_named_in_scope`] soundness holes: the gate must
    /// defer, not prove the name absent from a tree missing a branch.
    struct ConfigView {
        identity: AssemblyIdentity,
        roots: Vec<Entity>,
        auto_opens: Vec<String>,
        /// Fully-qualified names of types the projection dropped (`A.M/Inner` for a
        /// nested type — [`SkippedProjectionItem::enclosing_namespace`] strips the
        /// `/Inner` tail, so the drop is recorded under the top-level namespace).
        dropped_fqns: Vec<String>,
        /// The assembly's [`fsharp_extension_index_unknowable`](borzoi_assembly::AssemblyProjectionSkips::fsharp_extension_index_unknowable)
        /// — its F#-native extension overlay could not be built (absent/undecodable
        /// pickle), so its extension index is unread, not empty.
        extension_index_unknowable: bool,
        /// The assembly's [`fsharp_signature_non_authoritative`](borzoi_assembly::AssemblyProjectionSkips::fsharp_signature_non_authoritative)
        /// — its host F# pickle was not authoritative, so its module-kind markers
        /// are IL heuristics the classifier must decline.
        signature_non_authoritative: bool,
    }

    impl ConfigView {
        /// A view for assembly `name` with the given roots and auto-opens, no
        /// projection degradations — the common case the drop/overlay tests perturb.
        fn new(name: &str, roots: Vec<Entity>, auto_opens: &[&str]) -> Self {
            ConfigView {
                identity: ident(name),
                roots,
                auto_opens: auto_opens.iter().map(|s| (*s).to_string()).collect(),
                dropped_fqns: vec![],
                extension_index_unknowable: false,
                signature_non_authoritative: false,
            }
        }
    }

    impl EcmaView for ConfigView {
        fn identity(&self) -> &AssemblyIdentity {
            &self.identity
        }
        fn assembly_refs(&self) -> Vec<AssemblyIdentity> {
            vec![]
        }
        fn enumerate_type_defs_with_skips(
            &self,
        ) -> Result<(Vec<Entity>, AssemblyProjectionSkips), ImportError> {
            Ok((
                self.roots.clone(),
                AssemblyProjectionSkips {
                    dropped_types: self
                        .dropped_fqns
                        .iter()
                        .map(|fqn| SkippedProjectionItem {
                            name: fqn.clone(),
                            reason: "undecodable".to_string(),
                        })
                        .collect(),
                    skipped_fsharp_overlays: vec![],
                    fsharp_abbreviations_unknowable: false,
                    fsharp_extension_index_unknowable: self.extension_index_unknowable,
                    fsharp_signature_non_authoritative: self.signature_non_authoritative,
                },
            ))
        }
        fn assembly_auto_opens(&self) -> Result<Vec<String>, ImportError> {
            Ok(self.auto_opens.clone())
        }
        fn fsharp_resources(&self) -> Result<Vec<FSharpResource>, ImportError> {
            Ok(vec![])
        }
    }

    /// A public, non-generic, extension-member-free module `name` in `namespace`,
    /// owned by assembly `assembly` — the surviving half of an auto-open target.
    fn module_entity(assembly: &str, namespace: &[&str], name: &str) -> Entity {
        Entity {
            assembly: ident(assembly),
            namespace: namespace.iter().map(|s| (*s).to_string()).collect(),
            name: name.to_string(),
            kind: EntityKind::Module,
            access: Access::Public,
            is_sealed: false,
            generic_parameters: vec![],
            base_type: None,
            interfaces: vec![],
            members: vec![],
            skipped_members: vec![],
            method_def_tokens: vec![],
            nested_types: vec![],
            is_readonly: false,
            is_byref_like: false,
            is_struct: false,
            is_auto_open: false,
            is_require_qualified_access: false,
            is_no_equality: false,
            is_no_comparison: false,
            is_structural_equality: false,
            is_structural_comparison: false,
            is_allow_null_literal: false,
            obsolete: None,
            experimental: None,
            default_member: None,
            compiler_feature_required: vec![],
            source_name: None,
            extension_member_names: vec![],
            union_case_names: None,
            static_extension_member_names: Vec::new(),
            is_extension_container: false,
            custom_attrs: vec![],
            abbreviation_target: None,
        }
    }

    // ===== Interface-walk primitive (IW) — synthetic-entity unit tests =====
    //
    // These cover the completeness / soundness cases the real-BCL differential in
    // `crates/sema/tests/all/infer_member_access_diff.rs` cannot easily reach: an
    // interface diamond, a generic inherited interface (→ `Incomplete`), an
    // Object-capped env, and the sibling-ambiguity defer that is the whole reason
    // the interface DAG uses an exactly-one-declaring-level rule instead of the base
    // chain's cross-level hiding. See `docs/interface-walk-plan.md`.

    /// A non-generic interface [`TypeRef`] (an `interfaces` / base entry).
    fn iface_ref(namespace: &[&str], name: &str) -> TypeRef {
        TypeRef::Named {
            assembly: None,
            namespace: namespace.iter().map(|s| (*s).to_string()).collect(),
            name: name.to_string(),
            type_args: vec![],
            segment_arities: vec![0],
        }
    }

    /// An interface entity carrying its extended `interfaces` and `members`.
    fn iface_entity(
        assembly: &str,
        namespace: &[&str],
        name: &str,
        interfaces: Vec<TypeRef>,
        members: Vec<Member>,
    ) -> Entity {
        Entity {
            kind: EntityKind::Interface,
            interfaces,
            members,
            ..module_entity(assembly, namespace, name)
        }
    }

    /// A `System.Object` class entity — the universal member source.
    fn object_entity(assembly: &str) -> Entity {
        Entity {
            kind: EntityKind::Class,
            ..module_entity(assembly, &["System"], "Object")
        }
    }

    /// A public readable non-indexer instance property `name : System.Int32`.
    fn int_prop(name: &str) -> Member {
        Member::Property(Property {
            name: name.to_string(),
            access: Access::Public,
            ty: TypeRef::Primitive(Primitive::I4),
            parameters: vec![],
            is_static: false,
            has_getter: true,
            has_setter: false,
            getter_access: Some(Access::Public),
            is_required: false,
            compiler_feature_required: vec![],
            nullability: Nullability::Oblivious,
            custom_attrs: vec![],
            implements: Vec::new(),
            unclassified_impls: Vec::new(),
        })
    }

    fn handle_of(env: &AssemblyEnv, namespace: &[&str], name: &str) -> super::EntityHandle {
        let ns: Vec<String> = namespace.iter().map(|s| (*s).to_string()).collect();
        env.lookup_type(&ns, name, 0)
            .unwrap_or_else(|| panic!("no type {namespace:?}.{name}"))
    }

    /// A public *static* readable property `name : System.Int32`.
    fn static_int_prop(name: &str) -> Member {
        match int_prop(name) {
            Member::Property(p) => Member::Property(Property {
                is_static: true,
                ..p
            }),
            other => other,
        }
    }

    /// An abbreviation marker `namespace.name` whose decoded target is `target`.
    fn abbrev_marker(
        assembly: &str,
        namespace: &[&str],
        name: &str,
        target: Option<AbbreviationTarget>,
    ) -> Entity {
        Entity {
            kind: EntityKind::Abbreviation,
            abbreviation_target: target,
            ..module_entity(assembly, namespace, name)
        }
    }

    fn named_target(ccu: Option<&str>, path: &[&str]) -> AbbreviationTarget {
        AbbreviationTarget::Named {
            ccu: ccu.map(str::to_string),
            path: path.iter().map(|s| (*s).to_string()).collect(),
            args: Vec::new(),
        }
    }

    #[test]
    fn resolve_abbreviation_target_follows_a_nullary_named_target() {
        // `type S = System.String` (marker in `Lib`, target in `mscorlib`) resolves
        // through to the `String` entity, on which the member tail then walks.
        let string = Entity {
            kind: EntityKind::Class,
            members: vec![static_int_prop("Format")],
            ..module_entity("mscorlib", &["System"], "String")
        };
        let marker = abbrev_marker(
            "Lib",
            &["Lib"],
            "S",
            Some(named_target(Some("mscorlib"), &["System", "String"])),
        );
        let env = AssemblyEnv::from_entities(vec![string, marker]);

        let marker_h = handle_of(&env, &["Lib"], "S");
        let string_h = handle_of(&env, &["System"], "String");
        assert_eq!(env.resolve_abbreviation_target(marker_h), Some(string_h));
        // The static member is reachable on the resolved target.
        assert!(matches!(
            env.static_lookup(string_h, "Format"),
            super::StaticLookup::Resolved(_)
        ));
    }

    #[test]
    fn resolve_abbreviation_target_chases_a_chained_alias() {
        // `type A = B` and `type B = C` (a concrete class) — resolving `A` chases
        // through `B` to `C`.
        let c = Entity {
            kind: EntityKind::Class,
            ..module_entity("Lib", &["Lib"], "C")
        };
        let b = abbrev_marker(
            "Lib",
            &["Lib"],
            "B",
            Some(named_target(None, &["Lib", "C"])),
        );
        let a = abbrev_marker(
            "Lib",
            &["Lib"],
            "A",
            Some(named_target(None, &["Lib", "B"])),
        );
        let env = AssemblyEnv::from_entities(vec![c, b, a]);

        let a_h = handle_of(&env, &["Lib"], "A");
        let c_h = handle_of(&env, &["Lib"], "C");
        assert_eq!(env.resolve_abbreviation_target(a_h), Some(c_h));
    }

    #[test]
    fn resolve_abbreviation_target_declines_unresolvable_targets() {
        // A typar target, a target whose CCU is not loaded, and a marker with no
        // decoded target all decline — the consumer keeps deferring.
        let typar_marker =
            abbrev_marker("Lib", &["Lib"], "SelfVar", Some(AbbreviationTarget::Var(0)));
        let unloaded_marker = abbrev_marker(
            "Lib",
            &["Lib"],
            "Missing",
            Some(named_target(Some("NotLoaded"), &["Some", "Type"])),
        );
        let no_target_marker = abbrev_marker("Lib", &["Lib"], "Bare", None);
        let env = AssemblyEnv::from_entities(vec![typar_marker, unloaded_marker, no_target_marker]);

        for name in ["SelfVar", "Missing", "Bare"] {
            let h = handle_of(&env, &["Lib"], name);
            assert_eq!(
                env.resolve_abbreviation_target(h),
                None,
                "{name} must decline (defer)",
            );
        }
    }

    #[test]
    fn resolve_abbreviation_target_respects_exact_identity_across_same_named_assemblies() {
        // Two loaded assemblies share the simple name `Same` (an extern-alias
        // shape) but differ in version, each declaring `N.Widget`.
        fn ident_v(name: &str, major: u16) -> AssemblyIdentity {
            AssemblyIdentity {
                name: name.to_string(),
                version: Version {
                    major,
                    minor: 0,
                    build: 0,
                    revision: 0,
                },
                public_key_token: None,
            }
        }
        fn widget(assembly: AssemblyIdentity) -> Entity {
            Entity {
                kind: EntityKind::Class,
                assembly,
                ..module_entity("_", &["N"], "Widget")
            }
        }
        let same_v1 = ident_v("Same", 1);
        let same_v2 = ident_v("Same", 2);

        // A `None`-ccu (proven same-CCU) marker in `Same` v1 → must pin v1's
        // `Widget`, never v2's same-named sibling.
        let local_marker = Entity {
            kind: EntityKind::Abbreviation,
            assembly: same_v1.clone(),
            abbreviation_target: Some(named_target(None, &["N", "Widget"])),
            ..module_entity("_", &["Lib"], "LocalAlias")
        };
        // A `Some("Same")`-ccu marker → the name is ambiguous, so decline.
        let ref_marker = Entity {
            kind: EntityKind::Abbreviation,
            assembly: ident("Lib"),
            abbreviation_target: Some(named_target(Some("Same"), &["N", "Widget"])),
            ..module_entity("_", &["Lib"], "RefAlias")
        };
        let env = AssemblyEnv::from_entities(vec![
            widget(same_v1.clone()),
            widget(same_v2),
            local_marker,
            ref_marker,
        ]);

        let local = env
            .resolve_abbreviation_target(handle_of(&env, &["Lib"], "LocalAlias"))
            .expect("same-CCU target resolves");
        assert_eq!(
            env.entity(local).assembly,
            same_v1,
            "a same-CCU target must pin the marker's exact assembly, not a same-named sibling",
        );
        assert_eq!(
            env.resolve_abbreviation_target(handle_of(&env, &["Lib"], "RefAlias")),
            None,
            "an ambiguous referenced CCU name must decline",
        );
    }

    #[test]
    fn resolve_abbreviation_target_matches_module_suffix_segments_by_logical_name() {
        // The pickle path is in the logical-name domain, so a
        // `[<CompilationRepresentation(ModuleSuffix)>]` module contributes its
        // suffixed IL name (`InnerModule`), while its projected source name is
        // `Inner`. This must match at BOTH the top-level split and — the case
        // `nested` deliberately does not serve — a *nested* descent, or a target
        // beneath such a module is never found and the alias wrongly keeps
        // deferring. Here `InnerModule` is nested under a plain `Outer`.
        let widget = Entity {
            kind: EntityKind::Class,
            ..module_entity("Probe", &[], "Widget")
        };
        let inner = Entity {
            kind: EntityKind::Module,
            source_name: Some("Inner".to_string()),
            nested_types: vec![widget],
            ..module_entity("Probe", &[], "InnerModule")
        };
        let outer = Entity {
            kind: EntityKind::Module,
            nested_types: vec![inner],
            ..module_entity("Probe", &["Probe"], "Outer")
        };
        let marker = abbrev_marker(
            "Probe",
            &["Lib"],
            "WAlias",
            Some(named_target(
                None,
                &["Probe", "Outer", "InnerModule", "Widget"],
            )),
        );
        let env = AssemblyEnv::from_entities(vec![outer, marker]);

        let resolved = env
            .resolve_abbreviation_target(handle_of(&env, &["Lib"], "WAlias"))
            .expect("a target nested under a module-suffix module must resolve by logical name");
        assert_eq!(env.entity(resolved).name, "Widget");
    }

    #[test]
    fn resolve_abbreviation_target_declines_an_inaccessible_target() {
        // A public abbreviation of an *internal* type: F# permits the library but
        // rejects member access through the alias in a consumer, so resolve-through
        // must decline rather than commit one of the internal type's members.
        let secret = Entity {
            kind: EntityKind::Class,
            access: Access::Internal,
            ..module_entity("Lib", &["N"], "Secret")
        };
        let marker = abbrev_marker(
            "Lib",
            &["Lib"],
            "Alias",
            Some(named_target(None, &["N", "Secret"])),
        );
        let env = AssemblyEnv::from_entities(vec![secret, marker]);

        assert_eq!(
            env.resolve_abbreviation_target(handle_of(&env, &["Lib"], "Alias")),
            None,
            "a target that is not cross-assembly public must decline",
        );
    }

    #[test]
    fn resolve_abbreviation_target_pins_by_provenance_across_byte_identical_siblings() {
        // Two loaded DLLs share a *byte-identical* manifest identity (a duplicate
        // reference / extern-alias shape), each declaring `N.Widget` with a
        // distinguishing static. Only per-DLL provenance tells them apart — a
        // manifest-identity comparison collapses them (issue #150). The
        // `None`-ccu (`Local`) marker lives in the *contributor* view, so its
        // target must pin *that* view's `Widget`, never the sibling's; and a
        // `Some("Lib")` marker, seeing two distinct DLLs for the name, declines.
        fn widget_with(member: &str) -> Entity {
            Entity {
                kind: EntityKind::Class,
                members: vec![static_int_prop(member)],
                ..module_entity("Lib", &["N"], "Widget")
            }
        }
        // The sibling is interned FIRST, so a provenance-blind scan returns its
        // `Widget` — the shape that makes this bite.
        let sibling = ConfigView::new("Lib", vec![widget_with("FromSibling")], &[]);
        let contributor = ConfigView::new(
            "Lib",
            vec![
                widget_with("FromContributor"),
                abbrev_marker(
                    "Lib",
                    &["Lib"],
                    "LocalAlias",
                    Some(named_target(None, &["N", "Widget"])),
                ),
                abbrev_marker(
                    "Lib",
                    &["Lib"],
                    "RefAlias",
                    Some(named_target(Some("Lib"), &["N", "Widget"])),
                ),
            ],
            &[],
        );
        let env = AssemblyEnv::from_views(&[sibling, contributor]).expect("build env");

        let local = env
            .resolve_abbreviation_target(handle_of(&env, &["Lib"], "LocalAlias"))
            .expect("a same-CCU target resolves");
        assert!(
            matches!(
                env.static_lookup(local, "FromContributor"),
                super::StaticLookup::Resolved(_)
            ),
            "a Local target must pin the marker's OWN DLL's `Widget`",
        );
        assert!(
            matches!(
                env.static_lookup(local, "FromSibling"),
                super::StaticLookup::Absent
            ),
            "never the byte-identical sibling DLL's `Widget`",
        );
        assert_eq!(
            env.resolve_abbreviation_target(handle_of(&env, &["Lib"], "RefAlias")),
            None,
            "a referenced CCU name shared by two distinct DLLs is ambiguous — decline",
        );
    }

    #[test]
    fn resolve_abbreviation_target_prefers_the_type_over_a_module_suffix_companion() {
        // `type Mid = Target` with a `[<CompilationRepresentation(ModuleSuffix)>]`
        // companion `module Mid` (projected as `MidModule`, source name `Mid`),
        // and `type Outer = Mid`. The module TypeDef is interned before the
        // synthesized `Mid` marker, and both match the logical segment `Mid` — but
        // `type Outer = Mid` names the *type* `Mid`, so the chase must reach the
        // marker (→ `Target`), never the companion module (whose members FCS
        // rejects on the expansion, FS0039).
        let target = Entity {
            kind: EntityKind::Class,
            ..module_entity("Lib", &["N"], "Target")
        };
        // Interned BEFORE the marker, matching `Mid` only by its source name.
        let mid_module = Entity {
            kind: EntityKind::Module,
            source_name: Some("Mid".to_string()),
            ..module_entity("Lib", &["N"], "MidModule")
        };
        let mid_marker = abbrev_marker(
            "Lib",
            &["N"],
            "Mid",
            Some(named_target(None, &["N", "Target"])),
        );
        let outer_marker = abbrev_marker(
            "Lib",
            &["N"],
            "Outer",
            Some(named_target(None, &["N", "Mid"])),
        );
        let env = AssemblyEnv::from_entities(vec![target, mid_module, mid_marker, outer_marker]);

        let target_h = handle_of(&env, &["N"], "Target");
        assert_eq!(
            env.resolve_abbreviation_target(handle_of(&env, &["N"], "Outer")),
            Some(target_h),
            "the chase must prefer the type `Mid` over its module-suffix companion",
        );
    }

    #[test]
    fn resolve_abbreviation_target_declines_a_name_shared_with_a_rootless_sibling() {
        // Two loaded DLLs are named `Lib`: the contributor declares `N.Widget`
        // and a marker whose target CCU is `Lib`; the sibling declares NO
        // surviving types (all dropped) yet still loads. Counting only *rooted*
        // DLLs would see `Lib` as unique and resolve into the contributor — but
        // the pickle's `Some("Lib")` cannot choose between two loaded DLLs of
        // that name, so it must decline (issue #150 / codex P2). The per-DLL
        // identity registry counts the rootless sibling.
        let contributor = ConfigView::new(
            "Lib",
            vec![
                Entity {
                    kind: EntityKind::Class,
                    ..module_entity("Lib", &["N"], "Widget")
                },
                abbrev_marker(
                    "Lib",
                    &["Lib"],
                    "RefAlias",
                    Some(named_target(Some("Lib"), &["N", "Widget"])),
                ),
            ],
            &[],
        );
        let rootless = ConfigView::new("Lib", vec![], &[]);
        let env = AssemblyEnv::from_views(&[contributor, rootless]).expect("build env");

        assert_eq!(
            env.resolve_abbreviation_target(handle_of(&env, &["Lib"], "RefAlias")),
            None,
            "a CCU name shared with a rootless sibling DLL is still ambiguous — decline",
        );
    }

    #[test]
    fn resolve_abbreviation_target_declines_a_duplicate_local_without_provenance() {
        // `from_entities` carries no per-DLL provenance, so two byte-identical
        // DLLs' roots merge to the same `AssemblyKey::Identity`. A `Local`
        // (`None`-ccu) marker then matches BOTH same-path `Widget`s; unable to
        // tell which DLL the pickle meant, the resolver must decline rather than
        // return the first (codex P2). `top_level_types` keeps both colliding
        // handles, so the ambiguity is visible.
        fn widget() -> Entity {
            Entity {
                kind: EntityKind::Class,
                ..module_entity("Lib", &["N"], "Widget")
            }
        }
        let marker = abbrev_marker(
            "Lib",
            &["Lib"],
            "LocalAlias",
            Some(named_target(None, &["N", "Widget"])),
        );
        let env = AssemblyEnv::from_entities(vec![widget(), widget(), marker]);

        assert_eq!(
            env.resolve_abbreviation_target(handle_of(&env, &["Lib"], "LocalAlias")),
            None,
            "two indistinguishable same-path candidates must decline, not pick the first",
        );
    }

    #[test]
    fn resolve_abbreviation_target_counts_a_rootless_sibling_via_the_runtime_constructor() {
        // The LSP's runtime constructor (`from_assemblies_with_projection_knowability`):
        // a referenced DLL whose projection is ROOTLESS (all types dropped) still
        // carries its manifest identity, supplied explicitly because there is no
        // surviving root to derive it from. Two DLLs named `Lib` — the contributor
        // with `N.Widget` and a `Some("Lib")` marker, plus a rootless sibling —
        // must make the CCU name ambiguous and decline (issue #150 / codex P2). A
        // constructor that ignored the supplied identity would count only the
        // contributor and resolve into it.
        let widget = Entity {
            kind: EntityKind::Class,
            ..module_entity("Lib", &["N"], "Widget")
        };
        let marker = abbrev_marker(
            "Lib",
            &["Lib"],
            "RefAlias",
            Some(named_target(Some("Lib"), &["N", "Widget"])),
        );
        let env = AssemblyEnv::from_assemblies_with_projection_knowability(vec![
            (
                std::path::PathBuf::from("Contributor.dll"),
                vec![widget, marker],
                super::AbbreviationVisibility::Modelled,
                false,
                false,
                Vec::new(),
                Some(ident("Lib")),
            ),
            (
                std::path::PathBuf::from("Rootless.dll"),
                vec![],
                super::AbbreviationVisibility::Modelled,
                false,
                false,
                Vec::new(),
                Some(ident("Lib")),
            ),
        ]);

        assert_eq!(
            env.resolve_abbreviation_target(handle_of(&env, &["Lib"], "RefAlias")),
            None,
            "a rootless same-named sibling still makes the CCU name ambiguous — decline",
        );
    }

    #[test]
    fn resolve_abbreviation_target_declines_when_a_rootless_sibling_identity_is_unknown() {
        // The shorter constructors cannot name a rootless projection, leaving a
        // `None` registry entry. That unknown identity could itself be `Lib`, so a
        // `Some("Lib")` target's uniqueness is undecidable — decline, rather than
        // silently skip the unknown and treat the rooted sibling as the sole DLL of
        // that name (codex P2).
        let widget = Entity {
            kind: EntityKind::Class,
            ..module_entity("Lib", &["N"], "Widget")
        };
        let marker = abbrev_marker(
            "Lib",
            &["Lib"],
            "RefAlias",
            Some(named_target(Some("Lib"), &["N", "Widget"])),
        );
        let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
            (
                std::path::PathBuf::from("Rooted.dll"),
                vec![widget, marker],
                super::AbbreviationVisibility::Modelled,
                Vec::new(),
            ),
            (
                std::path::PathBuf::from("Rootless.dll"),
                vec![],
                super::AbbreviationVisibility::Modelled,
                Vec::new(),
            ),
        ]);

        assert_eq!(
            env.resolve_abbreviation_target(handle_of(&env, &["Lib"], "RefAlias")),
            None,
            "an unknown (rootless, unnamed) identity makes the CCU name undecidable — decline",
        );
    }

    #[test]
    fn resolve_abbreviation_target_declines_when_a_dll_was_skipped() {
        // A DLL the projector skipped entirely leaves no registry entry, so its
        // manifest name is unknown to the env. Once the host marks the env
        // incomplete, a `Some("Lib")` target must decline — the skipped DLL could
        // itself be `Lib`, so the sole *registered* `Lib` is no longer provably the
        // one the pickle meant (codex P2).
        let widget = Entity {
            kind: EntityKind::Class,
            ..module_entity("Lib", &["N"], "Widget")
        };
        let marker = abbrev_marker(
            "Lib",
            &["Lib"],
            "RefAlias",
            Some(named_target(Some("Lib"), &["N", "Widget"])),
        );
        let contributor = ConfigView::new("Lib", vec![widget, marker], &[]);
        let mut env = AssemblyEnv::from_views(&[contributor]).expect("build env");

        // Before any skip, the sole registered `Lib` resolves.
        assert!(
            env.resolve_abbreviation_target(handle_of(&env, &["Lib"], "RefAlias"))
                .is_some(),
            "the sole loaded `Lib` resolves before the env is marked incomplete",
        );
        // A skipped DLL makes the identity set incomplete — decline.
        env.mark_referenced_assemblies_incomplete();
        assert_eq!(
            env.resolve_abbreviation_target(handle_of(&env, &["Lib"], "RefAlias")),
            None,
            "a skipped DLL makes the CCU name undecidable — decline",
        );
    }

    /// A diamond `IDerived : IA, IB` with `IA, IB : IBase`, plus `System.Object`:
    /// the chain is `Complete`, deduplicates `IBase` to one occurrence, and appends
    /// `Object` last.
    #[test]
    fn interface_member_chain_dedups_diamond_and_appends_object() {
        let env = AssemblyEnv::from_entities(vec![
            iface_entity("Lib", &["N"], "IBase", vec![], vec![]),
            iface_entity(
                "Lib",
                &["N"],
                "IA",
                vec![iface_ref(&["N"], "IBase")],
                vec![],
            ),
            iface_entity(
                "Lib",
                &["N"],
                "IB",
                vec![iface_ref(&["N"], "IBase")],
                vec![],
            ),
            iface_entity(
                "Lib",
                &["N"],
                "IDerived",
                vec![iface_ref(&["N"], "IA"), iface_ref(&["N"], "IB")],
                vec![],
            ),
            object_entity("Lib"),
        ]);
        let derived = handle_of(&env, &["N"], "IDerived");
        let base = handle_of(&env, &["N"], "IBase");
        let object = handle_of(&env, &["System"], "Object");
        match env.interface_member_chain(derived) {
            InterfaceChain::Complete(levels) => {
                assert_eq!(
                    levels.iter().filter(|&&h| h == base).count(),
                    1,
                    "IBase, reachable through both IA and IB, appears once"
                );
                assert_eq!(levels.first().copied(), Some(derived), "receiver is first");
                assert_eq!(
                    levels.last().copied(),
                    Some(object),
                    "System.Object is appended last"
                );
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    /// A **generic** inherited interface cannot be resolved (`Ty` has no generic
    /// args), so the whole surface is `Incomplete` — an invisible inherited member
    /// could hide or ambiguate a lookup.
    #[test]
    fn interface_member_chain_generic_inherited_is_incomplete() {
        let generic_base = TypeRef::Named {
            assembly: None,
            namespace: vec!["N".to_string()],
            name: "IGen`1".to_string(),
            type_args: vec![borzoi_assembly::NullableType::oblivious(
                TypeRef::Primitive(Primitive::I4),
            )],
            segment_arities: vec![1],
        };
        let env = AssemblyEnv::from_entities(vec![
            iface_entity("Lib", &["N"], "IHas", vec![generic_base], vec![]),
            object_entity("Lib"),
        ]);
        let has = handle_of(&env, &["N"], "IHas");
        assert!(matches!(
            env.interface_member_chain(has),
            InterfaceChain::Incomplete
        ));
    }

    /// Without `System.Object` in the env (a single-assembly view) the chain is
    /// `ObjectCapped`, and an `Object`-method call through the interface defers
    /// while an own method still resolves.
    #[test]
    fn interface_object_capped_defers_object_method_only() {
        let env =
            AssemblyEnv::from_entities(vec![iface_entity("Lib", &["N"], "IThing", vec![], vec![])]);
        let thing = handle_of(&env, &["N"], "IThing");
        assert!(matches!(
            env.interface_member_chain(thing),
            InterfaceChain::ObjectCapped(_)
        ));
        // A call naming an Object method (GetHashCode) defers under the cap.
        assert!(
            env.instance_method(thing, "GetHashCode").is_none(),
            "an Object method is invisible without Object in the env"
        );
    }

    /// The soundness keystone: a name declared on two **sibling** interfaces (not in
    /// a subtype relation) is ambiguous — v1 does not hide one by the other, so the
    /// data-member lookup defers. The single-inherited control resolves, proving the
    /// defer is caused by the ambiguity, not the walk.
    #[test]
    fn interface_sibling_ambiguity_defers_but_single_inherited_resolves() {
        // ISib : IA, IB — both declare a property `P`.
        let ambiguous = AssemblyEnv::from_entities(vec![
            iface_entity("Lib", &["N"], "IA", vec![], vec![int_prop("P")]),
            iface_entity("Lib", &["N"], "IB", vec![], vec![int_prop("P")]),
            iface_entity(
                "Lib",
                &["N"],
                "ISib",
                vec![iface_ref(&["N"], "IA"), iface_ref(&["N"], "IB")],
                vec![],
            ),
            object_entity("Lib"),
        ]);
        let sib = handle_of(&ambiguous, &["N"], "ISib");
        assert!(
            ambiguous.instance_data_member(sib, "P").is_none(),
            "`P` on two sibling interfaces is ambiguous — defer, never pick one"
        );

        // Control: `ISingle : IA` — `P` on exactly one inherited interface resolves.
        let single = AssemblyEnv::from_entities(vec![
            iface_entity("Lib", &["N"], "IA", vec![], vec![int_prop("P")]),
            iface_entity(
                "Lib",
                &["N"],
                "ISingle",
                vec![iface_ref(&["N"], "IA")],
                vec![],
            ),
            object_entity("Lib"),
        ]);
        let single_h = handle_of(&single, &["N"], "ISingle");
        let ia = handle_of(&single, &["N"], "IA");
        let resolved = single.instance_data_member(single_h, "P");
        assert!(
            matches!(resolved, Some((owner, _, _)) if owner == ia),
            "`P` on the one inherited interface resolves to IA, got {resolved:?}"
        );
    }

    /// A same-FQN `System.Object` **impostor** a user assembly defines (and
    /// first-wins into `by_type`) must not be trusted as the universal root: it
    /// extends the real `Object`, so it carries a `base_type`, whereas the genuine
    /// root is base-less. Without the rootness guard the interface chain would
    /// append it and publish its arbitrary members for *every* interface receiver —
    /// resolutions FCS (which uses the CLR root) never exposes. The guard caps
    /// instead. (`codex` review, IW P2.)
    #[test]
    fn interface_chain_rejects_impostor_system_object() {
        let impostor = Entity {
            kind: EntityKind::Class,
            base_type: Some(iface_ref(&["System"], "ValueType")), // any base ⇒ not the root
            members: vec![int_prop("Pwned")],
            ..module_entity("Evil", &["System"], "Object")
        };
        let env = AssemblyEnv::from_entities(vec![
            impostor,
            iface_entity("Lib", &["N"], "IThing", vec![], vec![]),
        ]);
        let thing = handle_of(&env, &["N"], "IThing");
        assert!(
            matches!(
                env.interface_member_chain(thing),
                InterfaceChain::ObjectCapped(_)
            ),
            "a base-bearing System.Object impostor is not the root — cap instead"
        );
        assert!(
            env.instance_data_member(thing, "Pwned").is_none(),
            "the impostor's member must never be published for an interface receiver"
        );
    }

    /// **Review (P2), round: dropped descendants under a module-shaped auto-open.**
    /// `[<assembly: AutoOpen("A.M")>]` names a **surviving** module `A.M`, but a
    /// nested TypeDef beneath it was dropped during projection (recorded under the
    /// owning top-level namespace `A`). The auto-open enters `auto_open_module_handles`
    /// and the gate walks `M`'s surviving tree — which no longer shows the dropped
    /// container. FCS still imports it, so an extension of *any* name may be hiding;
    /// the gate must defer rather than prove the name absent.
    #[test]
    fn module_shaped_auto_open_defers_when_a_descendant_type_was_dropped() {
        let mut view = ConfigView::new("Lib", vec![module_entity("Lib", &["A"], "M")], &["A.M"]);
        view.dropped_fqns = vec!["A.M/Inner".to_string()];
        let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build env");
        assert!(
            env.extension_named_in_scope(&[], "Foo", false),
            "a dropped descendant of the auto-opened module hides a possible extension \
             of any name, so the gate must defer"
        );

        // Control: the identical env WITHOUT the drop proves the module tree really
        // is otherwise extension-free — the defer above is caused by the drop, not by
        // the auto-open surface itself.
        let clean = ConfigView::new("Lib", vec![module_entity("Lib", &["A"], "M")], &["A.M"]);
        let clean_env = AssemblyEnv::from_views(std::slice::from_ref(&clean)).expect("build env");
        assert!(
            !clean_env.extension_named_in_scope(&[], "Foo", false),
            "with no drop, the auto-opened module declares no extension named Foo, so commit"
        );
    }

    /// The semantic-token classifier honours signature authority on the
    /// **`from_views`** path too (codex review): a non-authoritative view's module
    /// kind is an IL heuristic FCS does not share (it imports the assembly through
    /// IL, a module reads as a plain type), so `entity_class` must decline it —
    /// even though `from_views` tags no `AssemblyId`. The authority bit rides on
    /// the entity node, so it survives every build path.
    #[test]
    fn from_views_declines_module_kind_for_a_non_authoritative_assembly() {
        let class_of = |view: ConfigView| {
            let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build env");
            let h = env.lookup_type(&["A".to_string()], "M", 0).expect("A.M");
            env.entity_class(h)
        };
        // Authoritative (control): the module classifies as a module.
        assert_eq!(
            class_of(ConfigView::new(
                "Lib",
                vec![module_entity("Lib", &["A"], "M")],
                &[]
            )),
            Some(super::SemanticClass::Module),
            "an authoritative module classifies as a module"
        );
        // Non-authoritative: decline (under-colour, never mis-colour).
        let mut view = ConfigView::new("Lib", vec![module_entity("Lib", &["A"], "M")], &[]);
        view.signature_non_authoritative = true;
        assert_eq!(
            class_of(view),
            None,
            "a non-authoritative assembly's module kind is declined on the from_views path"
        );
    }

    /// **Review (P2), round: sibling-only contested targets.** `[<assembly:
    /// AutoOpen("A.M")>]` names a module `A.M` the contributor `C` **dropped** during
    /// projection (marker under `A`, the enclosing namespace, not `A.M`), while a
    /// *sibling* assembly `S` happens to declare a namespace at `A.M`. `has_namespace`
    /// then succeeds solely because of `S`, so the target must not be recorded as a
    /// plain contested source: the contributor-scoped query finds no visible `C`
    /// content at `A.M` and the drop marker sits one level up, so it would prove
    /// every name absent. The contributor's target is dropped-or-absent — projection
    /// unknown — so the gate must defer wholesale.
    #[test]
    fn sibling_only_contested_target_defers_wholesale() {
        let mut contributor = ConfigView::new("C", vec![], &["A.M"]);
        contributor.dropped_fqns = vec!["A.M".to_string()];
        let sibling = ConfigView::new("S", vec![module_entity("S", &["A", "M"], "X")], &[]);
        let env = AssemblyEnv::from_views(&[contributor, sibling]).expect("build env");
        assert!(
            env.extension_named_in_scope(&[], "Foo", false),
            "the contributor's auto-open target was dropped; a sibling's same-named \
             namespace does not make it visible, so the gate must defer"
        );

        // Control: a *genuine* contested target — the contributor visibly declares
        // content at `A.M` too — stays name-keyed (EX-1's whole point), so an
        // unrelated name still commits.
        let contributor_visible =
            ConfigView::new("C", vec![module_entity("C", &["A", "M"], "Y")], &["A.M"]);
        let sibling2 = ConfigView::new("S", vec![module_entity("S", &["A", "M"], "X")], &[]);
        let contested_env =
            AssemblyEnv::from_views(&[contributor_visible, sibling2]).expect("build env");
        assert!(
            !contested_env.extension_named_in_scope(&[], "Foo", false),
            "a genuinely contested target with no drop declares no extension named Foo, so commit"
        );
    }

    /// **Review (codex P2): an assembly whose extension index is unread must stay
    /// unknowable.** When an F# assembly's signature pickle is absent or fails to
    /// decode, `apply_extension_member_index` never runs and every module's
    /// extension-name index is empty *because unread*. The name-keyed gate must not
    /// read that empty list as proof of absence:
    /// [`AssemblyProjectionSkips::fsharp_extension_index_unknowable`] folds into the
    /// per-entity extension-knowability, so the assembly's extension queries answer
    /// [`ExtensionMembers::Unknowable`] and the gate defers for every name.
    ///
    /// The bit's motivating *producer* is FSharp.Core (abbreviation-exempt yet
    /// extension-blind on a broken pickle — tested at the assembly layer). Its
    /// *consumption* here is generic: any assembly carrying the bit makes its own
    /// modules' extension queries unknowable, so this uses a neutral identity to
    /// isolate that mechanism from FSharp.Core's synthetic `Microsoft` auto-open.
    #[test]
    fn unread_extension_index_defers_the_gate() {
        // A module in an in-scope namespace, whose (unread) extension index is empty.
        let mut view = ConfigView::new("Lib", vec![module_entity("Lib", &["N"], "M")], &[]);
        view.extension_index_unknowable = true;
        let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build env");
        let in_scope = vec![vec!["N".to_string()]];
        assert!(
            env.extension_named_in_scope(&in_scope, "Foo", false),
            "an unread extension index is not proof of absence — the gate must defer"
        );
        assert!(
            env.extension_named_in_scope(&in_scope, "Bar", true),
            "…for a static call's name too"
        );

        // Control: the identical env whose pickle *did* decode (index known empty)
        // proves the name absent and commits.
        let clean = ConfigView::new("Lib", vec![module_entity("Lib", &["N"], "M")], &[]);
        let clean_env = AssemblyEnv::from_views(std::slice::from_ref(&clean)).expect("build env");
        assert!(
            !clean_env.extension_named_in_scope(&in_scope, "Foo", false),
            "a known-empty extension index declares no extension named Foo, so commit"
        );
    }

    /// **Review (codex P2): the gate must scan the resolver's *effective* implicit
    /// opens.** The resolver opens the manifest-derived implicit namespaces **plus** a
    /// hardcoded FSharp.Core fallback (`Microsoft.FSharp.{Core,Collections,Control}`) —
    /// which keeps an old/stand-in FSharp.Core that omits the assembly-level AutoOpen
    /// attributes working. An `[<AutoOpen>]` extension in a *fallback* namespace is
    /// therefore in scope even with an empty manifest set, so the gate must query the
    /// same effective set (`effective_implicit_open_namespace_paths`) or it would prove
    /// the name absent and commit an intrinsic overload FCS would not choose.
    #[test]
    fn gate_scans_the_hardcoded_implicit_open_fallback_namespaces() {
        // A module in a FALLBACK namespace, with an instance extension named `Foo`,
        // and NO manifest auto-opens (so `implicit_open_namespace_paths` is empty).
        let mut m = module_entity("Lib", &["Microsoft", "FSharp", "Control"], "M");
        m.extension_member_names = vec!["Foo".to_string()];
        let view = ConfigView::new("Lib", vec![m], &[]);
        let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build env");
        assert!(
            env.implicit_open_namespace_paths().is_empty(),
            "no manifest auto-opens: the manifest-derived set is empty, so only the \
             hardcoded fallback opens Microsoft.FSharp.Control"
        );
        assert!(
            env.extension_named_in_scope(&[], "Foo", false),
            "the resolver opens Microsoft.FSharp.Control via the fallback, so its \
             `[<AutoOpen>]` extension is in scope — the gate must defer a call of its name"
        );
        // Still name-keyed: a different name in the same fallback namespace commits.
        assert!(
            !env.extension_named_in_scope(&[], "OtherName", false),
            "the fallback namespace declares no extension named OtherName, so commit"
        );
    }

    /// **Review (codex P2): the empty `default()` env must open the fallback too.**
    /// The resolver seeds its implicit opens from
    /// `effective_implicit_open_namespace_paths`, so the documented empty env
    /// `AssemblyEnv::default()` must report the same hardcoded FSharp.Core fallback
    /// (`Microsoft.FSharp.{Core,Collections,Control}`) as `from_entities(vec![])` —
    /// else a project declaring one of those namespaces loses its historical implicit
    /// open. (Computing the set on the fly, rather than caching it per constructor,
    /// is what keeps `default()` in step.)
    #[test]
    fn default_env_reports_the_fsharp_core_implicit_open_fallback() {
        let expected: Vec<Vec<String>> = [
            "Microsoft.FSharp.Core",
            "Microsoft.FSharp.Collections",
            "Microsoft.FSharp.Control",
        ]
        .iter()
        .map(|ns| ns.split('.').map(str::to_string).collect())
        .collect();
        assert_eq!(
            AssemblyEnv::default().effective_implicit_open_namespace_paths(),
            expected,
            "the empty `default()` env still opens the hardcoded FSharp.Core fallback"
        );
        assert_eq!(
            AssemblyEnv::default().effective_implicit_open_namespace_paths(),
            AssemblyEnv::from_entities(vec![]).effective_implicit_open_namespace_paths(),
            "`default()` and `from_entities(vec![])` must open the same implicit set"
        );
    }

    #[test]
    fn from_views_propagates_dropped_type_namespaces() {
        // OV-6 review (GPT-5.6): a dropped type may be a C#-style `[<Extension>]`
        // class, so `from_views` must record its namespace as
        // possibly-extension-bearing (the LSP path already does; the public
        // constructor now does too). A `Demo.Ext` drop marks `Demo`; an unrelated
        // namespace is untouched.
        let view = FakeView {
            identity: AssemblyIdentity {
                name: "Test".to_string(),
                version: Version {
                    major: 0,
                    minor: 0,
                    build: 0,
                    revision: 0,
                },
                public_key_token: None,
            },
            dropped_fqn: "Demo.Ext".to_string(),
        };
        let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build env");
        // Name-keyed (EX-1), but a **dropped** type answers `true` for every name:
        // it may be an `[<Extension>]` class declaring anything, and we cannot read it.
        let demo = vec![vec!["Demo".to_string()]];
        let other = vec![vec!["Other".to_string()]];
        assert!(
            env.extension_named_in_scope(&demo, "Substring", false),
            "the dropped type's namespace is possibly-extension-bearing, for any name"
        );
        assert!(
            env.extension_named_in_scope(&demo, "AnyOtherName", true),
            "…including a static call's name — the dropped type is unreadable, so unknowable"
        );
        assert!(
            !env.extension_named_in_scope(&other, "Substring", false),
            "an unrelated namespace is unaffected"
        );
    }

    // ===== AutoOpen deref provenance (issue #150) =====
    //
    // Two loaded DLLs can share a simple name — or a byte-identical manifest
    // `AssemblyIdentity` — while being distinct assemblies (extern-alias /
    // duplicate-reference setups). The AutoOpen deref must select the
    // *contributing DLL's* entity by `AssemblyId` provenance, never by name:
    // folding a same-named sibling's module both imports a surface FCS keeps
    // closed AND misses the contributor's own extension members (the gate then
    // proves a name absent that FCS has in scope — an unsound commit).

    /// A module `namespace.name` owned by manifest identity `assembly`, whose
    /// instance-extension index is exactly `extensions`.
    fn extension_module(
        assembly: &str,
        namespace: &[&str],
        name: &str,
        extensions: &[&str],
    ) -> Entity {
        let mut m = module_entity(assembly, namespace, name);
        m.extension_member_names = extensions.iter().map(|s| (*s).to_string()).collect();
        m
    }

    /// **Module-shaped deref, byte-identical manifest identities.** The sibling
    /// is listed (and interned) first, so a simple-name match finds *its* `Ns.M`
    /// before the contributor's. The fold must land on the contributor's module:
    /// its extension enters the gate's surface, the sibling's does not.
    #[test]
    fn module_shaped_auto_open_selects_the_contributing_dll_among_identical_identities() {
        let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
            (
                std::path::PathBuf::from("/refs/sibling/Lib.dll"),
                vec![extension_module("Lib", &["Ns"], "M", &["FromSibling"])],
                super::AbbreviationVisibility::Modelled,
                vec![],
            ),
            (
                std::path::PathBuf::from("/refs/contributor/Lib.dll"),
                vec![extension_module("Lib", &["Ns"], "M", &["FromContributor"])],
                super::AbbreviationVisibility::Modelled,
                vec!["Ns.M".to_string()],
            ),
        ]);
        assert!(
            env.extension_named_in_scope(&[], "FromContributor", false),
            "the contributing DLL's module is the auto-open target: its extension is in scope"
        );
        assert!(
            !env.extension_named_in_scope(&[], "FromSibling", false),
            "the same-named sibling's module is a different DLL's: FCS never opens it"
        );
    }

    /// The same-simple-name (but distinct-version) variant of
    /// [`module_shaped_auto_open_selects_the_contributing_dll_among_identical_identities`]:
    /// even a full `AssemblyIdentity` comparison could tell these apart, but the
    /// discriminator must be per-DLL provenance, not any name.
    #[test]
    fn module_shaped_auto_open_selects_the_contributing_dll_among_same_named_siblings() {
        let mut contributor_m = extension_module("Lib", &["Ns"], "M", &["FromContributor"]);
        contributor_m.assembly.version.major = 2;
        let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
            (
                std::path::PathBuf::from("/refs/sibling/Lib.dll"),
                vec![extension_module("Lib", &["Ns"], "M", &["FromSibling"])],
                super::AbbreviationVisibility::Modelled,
                vec![],
            ),
            (
                std::path::PathBuf::from("/refs/contributor/Lib.dll"),
                vec![contributor_m],
                super::AbbreviationVisibility::Modelled,
                vec!["Ns.M".to_string()],
            ),
        ]);
        assert!(
            env.extension_named_in_scope(&[], "FromContributor", false),
            "the contributing DLL's module is the auto-open target: its extension is in scope"
        );
        assert!(
            !env.extension_named_in_scope(&[], "FromSibling", false),
            "the same-named sibling's module is a different DLL's: FCS never opens it"
        );
    }

    /// **Namespace-shaped deref, same-named siblings.** A simple-name guard
    /// counts the sibling's `Ns` content as the contributor's own, so
    /// `namespace_declared_only_by` wrongly reports sole ownership and the open
    /// is recorded env-wide — making the *sibling's* namespace content
    /// bare-resolvable where FCS keeps it closed. Provenance must classify it
    /// as contested: dropped from the env-wide opens, contributor-scoped in the
    /// extension gate.
    #[test]
    fn namespace_auto_open_contested_by_a_same_named_sibling_is_contributor_scoped() {
        let env = AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![
            (
                std::path::PathBuf::from("/refs/sibling/Lib.dll"),
                vec![extension_module(
                    "Lib",
                    &["Ns"],
                    "SiblingMod",
                    &["FromSibling"],
                )],
                super::AbbreviationVisibility::Modelled,
                vec![],
            ),
            (
                std::path::PathBuf::from("/refs/contributor/Lib.dll"),
                vec![extension_module(
                    "Lib",
                    &["Ns"],
                    "ContribMod",
                    &["FromContributor"],
                )],
                super::AbbreviationVisibility::Modelled,
                vec!["Ns".to_string()],
            ),
        ]);
        let ns: Vec<String> = vec!["Ns".to_string()];
        assert!(
            !env.implicit_open_namespace_paths().contains(&ns),
            "a same-named sibling DLL also declares Ns: the open is contested, not env-wide"
        );
        assert!(
            env.extension_named_in_scope(&[], "FromContributor", false),
            "the contested open is contributor-scoped: the contributor's extension is in scope"
        );
        assert!(
            !env.extension_named_in_scope(&[], "FromSibling", false),
            "the sibling's same-named namespace stays closed: its extension never enters scope"
        );
    }

    /// The `from_views` build path must distinguish same-named views too — each
    /// view is a distinct loaded DLL, so it gets its own provenance even though
    /// no source path is known for it.
    #[test]
    fn from_views_module_shaped_auto_open_distinguishes_same_named_views() {
        let sibling = ConfigView::new(
            "Lib",
            vec![extension_module("Lib", &["Ns"], "M", &["FromSibling"])],
            &[],
        );
        let contributor = ConfigView::new(
            "Lib",
            vec![extension_module("Lib", &["Ns"], "M", &["FromContributor"])],
            &["Ns.M"],
        );
        let env = AssemblyEnv::from_views(&[sibling, contributor]).expect("build env");
        assert!(
            env.extension_named_in_scope(&[], "FromContributor", false),
            "the contributing view's module is the auto-open target: its extension is in scope"
        );
        assert!(
            !env.extension_named_in_scope(&[], "FromSibling", false),
            "the same-named sibling view's module is a different DLL's: FCS never opens it"
        );
    }
}

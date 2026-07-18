//! The definition model: a name introduced by F# source, and what sort of
//! binder it is.

use borzoi_cst::syntax::SyntaxToken;
use rowan::TextRange;

/// A name introduced by F# source — a `let`-bound value or function, a
/// parameter, and (in later stages) modules and local pattern binders.
///
/// Stage A produces `Def`s only from pattern binder extraction
/// ([`crate::binders`]). The scope tree that interns them and assigns stable
/// identifiers lands in Stage C, so a `Def` carries no id yet.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Def {
    /// The bound identifier's source text, verbatim from the token. Backticks
    /// (`` `quoted name` ``) are retained as written; `idText`-style
    /// normalisation that collapses `` `x` `` and `x` to one name is deferred
    /// to the resolution stage that actually needs name *identity* (Stage C),
    /// where it can be checked against FCS.
    pub name: String,
    /// Source range of the defining identifier token, as a byte range into
    /// the file (rowan's [`TextRange`]).
    pub range: TextRange,
    pub kind: DefKind,
    /// `true` for a *provisional* maybe-var binder: a nullary single-segment
    /// `LongIdent` head reached below a `let` head or in a parameter / match
    /// pattern (FCS's `mkSynPatMaybeVar`). The parser routes lowercase idents
    /// to `Named` and upper-case ones to `LongIdent`, so these are exactly the
    /// constructor-shaped names (`None`, `Empty`) that bind *only if* they do
    /// not resolve to a nullary constructor / literal. Binder extraction cannot
    /// decide that — it has no resolution environment — so it flags them and
    /// the resolution stage drops the ones that are really constructor
    /// references. Definite binders (`Named` leaves, the direct `let` head)
    /// are never provisional.
    pub provisional: bool,
}

/// What sort of binder a [`Def`] is.
///
/// `Module` (from the module/namespace walker) arrives with its producer in a
/// later stage, alongside value mutability (a binding-level property the
/// headPat alone does not carry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefKind {
    /// A value or function bound by a `let` head: `is_function` is `true` for
    /// the curried function form (`let f x = …`) and `false` for a plain
    /// value (`let x = …`).
    Value { is_function: bool },
    /// A parameter bound by a lambda or function-binding argument pattern.
    Parameter,
    /// A local bound by a refutable pattern that is not a parameter — the
    /// binders of a `match` clause pattern (`match e with x -> …`). Distinct
    /// from [`DefKind::Parameter`] because a future consumer (completion,
    /// hover) will want to label the two differently, mirroring FCS treating
    /// both as locals but a `match` binder not being a formal parameter.
    PatternLocal,
    /// A type introduced by a `type` definition — an abbreviation, record,
    /// union, enum, or class (`type T = …`). The bound name occupies F#'s
    /// *type* namespace, disjoint from the value namespace the other kinds
    /// live in, so a type [`Def`] is referenced only from type-syntactic
    /// positions (annotations, abbreviation right-hand sides, field types,
    /// cast/`new`/type-application targets), never from an expression-level
    /// value name use.
    Type,
    /// A union case introduced by `type T = A | B of …`. Interned when the type
    /// is walked, like [`DefKind::Type`], but it lives in the *value* namespace:
    /// a case is used as a constructor (`B 3`) and as a pattern head
    /// (`match x with B n -> …`), so it is added to the value scope and found by
    /// the ordinary value lookup. Resolving a constructor-shaped name (an
    /// upper-case single segment) to a case is what lets a nullary pattern head
    /// like `None` / `Red` be told apart from a fresh binder (the provisional
    /// maybe-var, [`Def::provisional`]).
    UnionCase,
    /// A constructor introduced by an `exception E of …` definition (or its
    /// abbreviation `exception Alias = E`). Like [`DefKind::UnionCase`] it lives
    /// in the *value* namespace — used as a constructor (`raise (E x)`, `E "x"`)
    /// and as a pattern head (`try … with E m -> …`) — so it is added to the
    /// value scope, found by the ordinary value lookup, and recognised in
    /// pattern position by the case-reference lookup, exactly as a union case is.
    /// An exception is never `[<RequireQualifiedAccess>]`, so the constructor is
    /// always in unqualified scope. (An `exception` also introduces an exception
    /// *type* in the disjoint type namespace; modelling that facet is a later
    /// slice — this binder is the value-namespace constructor.)
    ExceptionCase,
    /// The *recognizer* value of an active-pattern definition
    /// (`let (|Even|Odd|) … = …`) — the top-level function the cases dispatch to.
    /// Its [`range`](Def::range) is the `|Even|Odd|` name span (parens excluded),
    /// matching FCS, which resolves both the recognizer's own occurrence and
    /// every case *use* (`match x with Even`, or `then Even` constructing a case
    /// in the recognizer body) to this span. So each case name is a value-frame
    /// entry pointing at *this* def, and it is recognised in pattern position by
    /// the case-reference lookup, exactly as a union / exception case is.
    ActivePattern,
    /// A case *token* inside an active-pattern name (`Even`, `Odd` of
    /// `(|Even|Odd|)`). Interned only so the case's *defining occurrence* (the
    /// token in the `(|…|)` name) resolves to itself, as FCS reports it — a
    /// distinct symbol from the [`DefKind::ActivePattern`] recognizer. Case
    /// *uses* resolve to the recognizer, not here (the trailing `_` of a partial
    /// pattern is not a case and is never interned).
    ActivePatternCase,
    /// A case of an `enum` definition (`type Color = Red = 0 | Green = 1`). Enum
    /// cases are **require-qualified**: reachable only as `Color.Red`, never bare
    /// `Red` (FCS reports bare `Red` as `FS0039`), so — unlike a union case — an
    /// enum case is *not* added to the unqualified value frame. Its defining
    /// occurrence self-resolves, and a qualified use `Color.Red` resolves to it
    /// (via the enum-case index), with the head `Color` resolving to the enum
    /// [`DefKind::Type`].
    EnumCase,
    /// A **static member** of a type definition or same-file augmentation
    /// (`type Color() = static member Red = 1`, `member val`, a get/set
    /// property, or a single un-overloaded static method). Interned only for
    /// the emit-eligible subset of the type-member index (see
    /// `docs/project-type-member-plan.md`): a qualified `Color.Red` /
    /// `Pal.Color.Red` use in expression position resolves to it (FCS-pinned,
    /// probes M1/M2a/M2d/M4b), with the type segment resolving to the
    /// [`DefKind::Type`]. Its defining occurrence is *not* self-recorded (FCS
    /// reports synthetic symbols — a `member val`'s backing field — at the name
    /// token, so recording there would disagree with the oracle). Like an enum
    /// case it is reachable only through its type's name, never bare — it is
    /// *not* added to any value frame.
    Member,
    /// A **type parameter** declared by a `type` / `let` / `member` header
    /// (`type Foo<'T>`, `let f<'T>`, `member _.M<'T>`). Lives in F#'s *type*
    /// namespace, disjoint from values, and is **definition-scoped** — visible
    /// only inside the declaring definition, so it is held in a stack of typar
    /// frames rather than the value scope or the container-keyed type index. Its
    /// [`range`](Def::range) is the whole `'T` / `^T` occurrence *including the
    /// sigil* (the `TYPAR_DECL` node range), matching FCS, which reports the
    /// declaring occurrence and every `'T` use at the apostrophe-inclusive span.
    TypeParam,
}

/// What *sort of thing* a resolved name refers to, at the granularity a
/// consumer highlighting or labelling the name needs — the semantic-classification
/// currency (an LSP semantic-tokens pass maps each variant to a token type).
///
/// This is a **presentation-neutral semantic fact**, deliberately coarser than
/// [`DefKind`] in places (it does not distinguish a parameter from a `match`
/// local — both are locals a highlighter colours alike) and finer in others
/// (later stages classify referenced-assembly members into
/// property/method/field/event, distinctions [`DefKind::Member`] does not draw).
/// It is the say-something half of resolution's say-nothing-when-unsure
/// contract: it is only ever produced where name resolution has *committed* to a
/// binder, so — like [`Resolution`](crate::Resolution) — every value here is a
/// claim the FCS differential can hold us to.
///
/// The in-file ([`DefKind`]-derived) variants come from a file's own binders;
/// the referenced-assembly variants (`Module`, `Method`, `Property`, `Event`)
/// come from classifying a [`Resolution::Entity`](crate::Resolution) /
/// [`Resolution::Member`](crate::Resolution) against the
/// [`AssemblyEnv`](crate::AssemblyEnv) (see
/// [`AssemblyEnv::entity_class`](crate::AssemblyEnv::entity_class) /
/// [`AssemblyEnv::member_class`](crate::AssemblyEnv::member_class)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SemanticClass {
    /// A curried function value (`let f x = …`).
    Function,
    /// A non-function value binding (`let x = …`) — a module value or a local
    /// `let`. Function-ness here is *syntactic* (whether the binding has
    /// parameter patterns), so a value of function type bound without parameters
    /// (`let g = fun x -> x`) is a `Value`; the differential never asserts
    /// value-vs-function on this variant for exactly that reason.
    Value,
    /// A function / lambda formal parameter.
    Parameter,
    /// A `match`-clause (or other refutable-pattern) local. Kept distinct from
    /// [`Self::Parameter`] because [`DefKind`] does; a highlighter may collapse
    /// the two.
    PatternLocal,
    /// A type name (`type T = …` — abbreviation, record, union, enum, or class).
    /// Coarse: the record-vs-union-vs-class flavour is a later refinement.
    Type,
    /// A union case (`type T = A | B of …`), used as a constructor or a pattern
    /// head.
    UnionCase,
    /// An `exception` constructor (`exception E of …`).
    ExceptionCase,
    /// An active-pattern recognizer (`(|Even|Odd|)`) or one of its case tokens.
    ActivePattern,
    /// An `enum` case (`type Color = Red = 0`), reachable only qualified.
    EnumCase,
    /// A static member of an in-file type definition or augmentation, of a
    /// flavour [`DefKind::Member`] does not pin down (method vs property).
    Member,
    /// A type parameter (`'T` / `^T`) declared by a `type` / `let` / `member`
    /// header — see [`DefKind::TypeParam`].
    TypeParameter,
    /// A module or namespace-like entity in a referenced assembly (an F#
    /// `module`), the head a qualified assembly path roots at.
    Module,
    /// A method of a referenced-assembly type.
    Method,
    /// A property or field of a referenced-assembly type (the standard token
    /// legend has no distinct `field`, so both read as a property).
    Property,
    /// An event of a referenced-assembly type.
    Event,
}

impl DefKind {
    /// The presentation-neutral [`SemanticClass`] of a name bound by this kind.
    /// Total and pure: every [`DefKind`] maps to exactly one class.
    pub fn semantic_class(self) -> SemanticClass {
        match self {
            DefKind::Value { is_function: true } => SemanticClass::Function,
            DefKind::Value { is_function: false } => SemanticClass::Value,
            DefKind::Parameter => SemanticClass::Parameter,
            DefKind::PatternLocal => SemanticClass::PatternLocal,
            DefKind::Type => SemanticClass::Type,
            DefKind::UnionCase => SemanticClass::UnionCase,
            DefKind::ExceptionCase => SemanticClass::ExceptionCase,
            DefKind::ActivePattern | DefKind::ActivePatternCase => SemanticClass::ActivePattern,
            DefKind::EnumCase => SemanticClass::EnumCase,
            DefKind::Member => SemanticClass::Member,
            DefKind::TypeParam => SemanticClass::TypeParameter,
        }
    }
}

/// A stable identifier for a [`Def`] within a single resolved file — an index
/// into the file's definition arena (see [`crate::ResolvedFile`]). A newtype
/// rather than a bare index per "no primitive obsession": a `DefId` from one
/// file is meaningless against another's arena, and the type makes that
/// mismatch a compile error rather than a silently-wrong lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DefId(u32);

impl DefId {
    pub(crate) fn new(index: usize) -> Self {
        DefId(u32::try_from(index).expect("more than u32::MAX defs in one file"))
    }

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

impl Def {
    /// Build a definite (non-provisional) `Def` from an identifier token and
    /// its kind.
    pub(crate) fn from_token(token: &SyntaxToken, kind: DefKind) -> Self {
        Self {
            name: token.text().to_string(),
            range: token.text_range(),
            kind,
            provisional: false,
        }
    }

    /// Build a definite (non-provisional) `Def` from a name and an explicit
    /// range, for a binder whose span is not a single identifier token: a
    /// type parameter's `'T` occurrence (sigil + name, the `TYPAR_DECL` node) or
    /// an operator-named case (see [`Self::from_op_name`]).
    pub(crate) fn from_range(name: &str, range: TextRange, kind: DefKind) -> Self {
        Self {
            name: name.to_string(),
            range,
            kind,
            provisional: false,
        }
    }

    /// Build a `Def` for an operator-named union/enum case — its compiled
    /// `op_Nil` / `op_ColonColon` name and the source range of the operator
    /// tokens (see `UnionCase::operator_name`). Unlike [`Self::from_token`] there
    /// is no single identifier token, so the name and range are passed directly.
    pub(crate) fn from_op_name(name: &str, range: TextRange, kind: DefKind) -> Self {
        Self::from_range(name, range, kind)
    }

    /// Build a *provisional* maybe-var `Def` (see [`Def::provisional`]).
    pub(crate) fn provisional_from_token(token: &SyntaxToken, kind: DefKind) -> Self {
        Self {
            provisional: true,
            ..Self::from_token(token, kind)
        }
    }
}

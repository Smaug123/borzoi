//! Normalised AST data model — the shared shape both projectors target.
//!
//! [`from_cst`](super::from_cst) (our rowan AST) and
//! [`from_fcs`](super::from_fcs) (the `fcs-dump ast` JSON) both project into
//! these types; the differential diff is then a plain `assert_eq!`.

/// Top-level normalised parse — an implementation file (`.fs`) or a signature
/// file (`.fsi`, phase 10.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedRoot {
    Impl(NormalisedImplFile),
    Sig(NormalisedSigFile),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedImplFile {
    /// Parsed-file trivia warning directives (`#nowarn` / `#warnon`) in active
    /// code, in source order. FCS's parse tree exposes only kind + range here,
    /// not parsed warning numbers, so the normalised model carries kind only.
    pub warn_directives: Vec<NormalisedWarnDirectiveKind>,
    pub modules: Vec<NormalisedModule>,
}

/// A signature file (phase 10.11) — FCS's `ParsedSigFileInput.contents`. The
/// header/segment structure mirrors [`NormalisedImplFile`] exactly (FCS's
/// `SynModuleOrNamespaceSig` is field-for-field identical to
/// `SynModuleOrNamespace`); only the body declarations differ
/// ([`NormalisedSigDecl`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedSigFile {
    /// Parsed-file trivia warning directives (`#nowarn` / `#warnon`) in active
    /// code, in source order. See [`NormalisedImplFile::warn_directives`].
    pub warn_directives: Vec<NormalisedWarnDirectiveKind>,
    pub modules: Vec<NormalisedSigModule>,
}

/// One parsed-file warning directive from FCS's trivia payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalisedWarnDirectiveKind {
    Nowarn,
    Warnon,
}

/// An access modifier — FCS's `SynAccess` (`SyntaxTree.fsi`). `public` /
/// `internal` / `private`, at whichever declaration/binding/pattern site the
/// grammar's `opt_access` admits it. Modelled as an `Option<NormalisedAccess>`
/// at each site, mirroring FCS's `SynAccess option`: `None` is the absent
/// modifier, and an *explicit* `public` is `Some(Public)` — distinct from
/// `None`, exactly as FCS distinguishes them.
///
/// Our CST captures the modifier as a raw `ACCESS_TOK` token (whose text is the
/// keyword), so [`from_cst`](super::from_cst) reads the keyword and
/// [`from_fcs`](super::from_fcs) decodes the `SynAccess` DU; the differential
/// diff then makes access *placement* divergences (which token attaches to
/// which construct) visible, where previously both sides elided it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalisedAccess {
    Public,
    Internal,
    Private,
}

/// One `SynModuleOrNamespaceSig` (phase 10.11). Mirrors [`NormalisedModule`]
/// (shared `kind`/`is_rec`/`attributes`), with signature `decls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedSigModule {
    pub kind: NormalisedModuleKind,
    pub is_rec: bool,
    pub attributes: Vec<Vec<NormalisedAttribute>>,
    /// `SynModuleOrNamespaceSig.accessibility` (FCS field 6) — `module internal
    /// M` / `namespace` carries none. See [`NormalisedAccess`].
    pub access: Option<NormalisedAccess>,
    pub decls: Vec<NormalisedSigDecl>,
}

/// One `SynModuleSigDecl`. Phase 10.13a models [`Open`](NormalisedSigDecl::Open)
/// (`open` / `open type`); `module abbrev` / nested-module sigs (10.13b), `val`
/// signatures (10.12), type-signature reprs (10.14), and exception sigs (10.15)
/// add variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedSigDecl {
    /// `SynModuleSigDecl.Open(target, range)` — reuses the impl-side
    /// [`NormalisedOpenTarget`] (the `open`/`open type` distinction).
    Open { target: NormalisedOpenTarget },
    /// `SynModuleSigDecl.NestedModule(moduleInfo, isRecursive, moduleDecls, …)`
    /// (phase 10.13b) — a nested `module X = <sig block>`. `long_id` is the name
    /// (`SynComponentInfo.longId`); `is_rec` is `module rec` (always `false` in a
    /// valid `.fsi` — FCS rejects `module rec` there); `attributes` is the
    /// header's `SynComponentInfo.attributes` (phase 10.7d); `decls` is the
    /// recursively-projected signature body.
    NestedModule {
        long_id: Vec<String>,
        is_rec: bool,
        attributes: Vec<Vec<NormalisedAttribute>>,
        /// `SynComponentInfo.accessibility` (FCS field 6) — `module internal N`.
        /// See [`NormalisedAccess`].
        access: Option<NormalisedAccess>,
        decls: Vec<NormalisedSigDecl>,
    },
    /// `SynModuleSigDecl.ModuleAbbrev(ident, longId, range)` (phase 10.13b) — a
    /// module abbreviation `module X = Bar.Baz`. Same shape as the impl-side
    /// [`NormalisedDecl::ModuleAbbrev`].
    ModuleAbbrev { ident: String, long_id: Vec<String> },
    /// `SynModuleSigDecl.Val(valSig, range)` (phase 10.12a) — a `val x : int`
    /// specification. `name` is `SynValSig.ident`, `ty` the `synType`,
    /// `attributes` the `SynValSig.attributes` lists (`[<Literal>] val x : int`),
    /// `typars` the `explicitTypeParams` (`val f<'T> : …`, phase 10.12) with their
    /// inside-`<>` `constraints`, and `literal` the `= <expr>` value
    /// (`SynValSig.synExpr`, `[<Literal>] val x : int = 1`, phase 10.12), and
    /// `access` the modifier (`val internal x : int`, see [`NormalisedAccess`]);
    /// arity and inline/mutable flags are elided.
    Val {
        attributes: Vec<Vec<NormalisedAttribute>>,
        name: String,
        /// `SynValSig.accessibility` (FCS field 8, a `SynValSigAccess` — the
        /// *overall* access slot; per-getter/setter access on a `val … with
        /// get, set` remains elided). See [`NormalisedAccess`].
        access: Option<NormalisedAccess>,
        /// Explicit value type parameters — FCS's `SynValSig.explicitTypeParams`
        /// (`val f<'T> : …`, phase 10.12). Empty for a non-generic value.
        typars: Vec<NormalisedTypar>,
        /// The inside-`<>` `when` constraints on those typars
        /// (`val f<'T when 'T : comparison> : …`). The after-type `when` clause
        /// lives in `ty` as a [`NormalisedType::WithGlobalConstraints`].
        constraints: Vec<NormalisedTypeConstraint>,
        ty: NormalisedType,
        /// The `= <literal>` value — FCS's `SynValSig.synExpr` (`val x : int = 1`,
        /// phase 10.12), a full expression (usually a `Const`). `None` for a `val`
        /// without a literal value.
        literal: Option<Box<NormalisedExpr>>,
    },
    /// `SynModuleSigDecl.Types(types, range)` (phase 10.14, first slice) — a
    /// group of type-definition signatures (`SynTypeDefnSig list`). Reuses the
    /// impl-side [`NormalisedTypeDefn`]: a `SynTypeDefnSig`'s
    /// `SynTypeDefnSigRepr.Simple` wraps the same `SynTypeDefnSimpleRepr` as the
    /// impl `SynTypeDefnRepr.Simple`, so the abbreviation form projects through
    /// the shared definition shape (its `implicit_ctor` is always `None` — a
    /// `SynTypeDefnSig` has no implicit-constructor slot). Only the abbreviation
    /// repr is produced today; record / union / enum / object-model reprs and
    /// `and`-chains are later slices.
    Types(Vec<NormalisedTypeDefn>),
    /// `SynModuleSigDecl.Exception(exnSig, range)` (phase 10.15) — an exception
    /// signature `exception E [of …] [= path] [with member …]`. Reuses the
    /// impl-side [`NormalisedExnDefn`]: FCS's `SynExceptionSig(exnRepr,
    /// withKeyword, members, range)` shares the `SynExceptionDefnRepr` (and its
    /// field layout) with the impl `SynExceptionDefn`, so the `exconCore` forms
    /// project identically. The `members` slot carries the `with member …`
    /// augmentation's member *sigs* (projected through the shared
    /// [`NormalisedMember`], like the impl augmentation's member bodies).
    Exception(NormalisedExnDefn),
    /// `SynModuleSigDecl.HashDirective(ParsedHashDirective(ident, args, range),
    /// range)` — a `#`-directive in a signature file (`#I`, `#load`, …).
    HashDirective {
        ident: String,
        args: Vec<NormalisedHashDirectiveArg>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedModule {
    pub kind: NormalisedModuleKind,
    /// `SynModuleOrNamespace.isRecursive` — `module rec Foo` / `namespace rec
    /// A.B`. Always `false` for an [`Anon`](NormalisedModuleKind::Anon) module
    /// (FCS field 1 is `false` there).
    pub is_rec: bool,
    /// `SynModuleOrNamespace.attribs` (phase 10.7e, FCS field 5) — a whole-file
    /// `[<AutoOpen>] module Foo` header's attribute lists (one inner vec per
    /// `SynAttributeList`). Empty for an anonymous module and for a `namespace`
    /// (which cannot carry attributes).
    pub attributes: Vec<Vec<NormalisedAttribute>>,
    /// `SynModuleOrNamespace.accessibility` (FCS field 6) — a `module internal
    /// M` header. Always `None` for an [`Anon`](NormalisedModuleKind::Anon)
    /// module and for a `namespace` (neither carries an access modifier). See
    /// [`NormalisedAccess`].
    pub access: Option<NormalisedAccess>,
    pub decls: Vec<NormalisedDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedModuleKind {
    /// `SynModuleOrNamespaceKind.AnonModule` — the implicit module wrapping a
    /// script-style file body. Its `longId` is filename-derived (random under
    /// tempfiles), so it is **not** projected here.
    Anon,
    /// A header-introduced module/namespace (phase 8.2). `long_id` is the
    /// source-derived dotted name (`SynModuleOrNamespace.longId`, empty for
    /// `namespace global`); `kind` selects which header keyword introduced it.
    Named {
        long_id: Vec<String>,
        kind: NamedKind,
    },
}

/// Which `SynModuleOrNamespaceKind` a header-introduced module/namespace
/// carries — `module Foo` / `namespace Foo` / `namespace global`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKind {
    /// `module Foo` — `NamedModule`.
    Module,
    /// `namespace Foo.Bar` — `DeclaredNamespace`.
    Namespace,
    /// `namespace global` — `GlobalNamespace` (empty `long_id`).
    GlobalNamespace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedDecl {
    Expr(NormalisedExpr),
    /// `SynModuleDecl.Let(isRec, bindings, range, trivia)` — top-level
    /// `let x = e` (or `let rec …`). Phase 4.1 only produces `is_rec = false`
    /// with a single binding; the projector still carries the list so future
    /// `and`-chains slot in without a shape change.
    Let {
        is_rec: bool,
        bindings: Vec<NormalisedBinding>,
    },
    /// `SynModuleDecl.Open(target, range)` — an `open` declaration. The range
    /// is elided; the target carries the distinction between a
    /// module/namespace path and an opened type.
    Open {
        target: NormalisedOpenTarget,
    },
    /// `SynModuleDecl.NestedModule(moduleInfo, isRecursive, decls, …)` — a
    /// nested `module X = <block>` (phase 8.4). `long_id` is the name
    /// (`SynComponentInfo.longId`); `is_rec` is `module rec`; `decls` is the
    /// recursively-projected body. A nested module is always a *module* (FCS
    /// never produces a nested namespace), so there is no kind discriminant.
    /// `attributes` is the header's `SynComponentInfo.attributes` (phase 10.7d,
    /// FCS field 0 — one inner vec per `SynAttributeList`); `access` is the
    /// modifier (`module internal N =`, see [`NormalisedAccess`]); `isContinuing`
    /// / ranges are elided.
    NestedModule {
        long_id: Vec<String>,
        is_rec: bool,
        attributes: Vec<Vec<NormalisedAttribute>>,
        /// `SynComponentInfo.accessibility` (FCS field 6) — `module internal N =
        /// …`. See [`NormalisedAccess`].
        access: Option<NormalisedAccess>,
        decls: Vec<NormalisedDecl>,
    },
    /// `SynModuleDecl.ModuleAbbrev(ident, longId, range)` — a module
    /// abbreviation `module X = Bar.Baz` (phase 8.5). `ident` is the single-name
    /// LHS; `long_id` is the abbreviated module path (RHS). Range is elided.
    ModuleAbbrev {
        ident: String,
        long_id: Vec<String>,
    },
    /// `SynModuleDecl.Types(typeDefns, range)` — a group of one or more
    /// `and`-joined type definitions (phase 9). Phase 9.1 produces exactly one
    /// definition per group (only `and` aggregates, which is phase 9.2). The
    /// range is elided.
    Types(Vec<NormalisedTypeDefn>),
    /// `SynModuleDecl.Exception(exnDefn, range)` — an exception definition
    /// (phase 9.15a). The range is elided.
    Exception(NormalisedExnDefn),
    /// `SynModuleDecl.Attributes(attributes, range)` — a standalone attribute
    /// declaration, `[<assembly: …>]` (phase 10.7). One inner vec per
    /// `SynAttributeList`. The range is elided.
    Attributes(Vec<Vec<NormalisedAttribute>>),
    /// `SynModuleDecl.HashDirective(ParsedHashDirective(ident, args, range), range)`
    /// — a `#`-directive (`#I "/tmp"`, `#load "a.fs"`, …). `ident` is the
    /// directive name (the `I` / `load`); `args` the argument list.
    HashDirective {
        ident: String,
        args: Vec<NormalisedHashDirectiveArg>,
    },
}

/// One `ParsedHashDirectiveArgument` (`SyntaxTrivia`/`SyntaxTree`): a string
/// literal, an `int32`, or a source identifier (`__SOURCE_DIRECTORY__` &co.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedHashDirectiveArg {
    /// `ParsedHashDirectiveArgument.String(value, kind, range)`. The value is
    /// the raw UTF-16 code-unit payload, because directive string arguments use
    /// the same .NET `string` carrier as parser literals.
    String {
        value: Vec<u16>,
        kind: SynStringKind,
    },
    /// `ParsedHashDirectiveArgument.Int32(value, range)`.
    Int32(i32),
    /// `ParsedHashDirectiveArgument.Ident(ident, range)` — a plain identifier
    /// argument (`#nowarn FS`, `#time on`). The `idText` (backticks stripped).
    Ident(String),
    /// `ParsedHashDirectiveArgument.SourceIdentifier(ident, value, range)` — a
    /// source-location identifier such as `__SOURCE_DIRECTORY__`. `ident` is the
    /// source spelling. `value` is FCS's expanded value, canonicalised after the
    /// FCS-side projector validates it against the source file range.
    SourceIdentifier {
        ident: String,
        value: NormalisedSourceIdentifierValue,
    },
}

/// Canonicalised expansion of a source identifier. FCS carries concrete strings:
/// the physical source directory, the physical source file name, or the
/// physical 1-based line number. The path strings depend on the temp/corpus
/// checkout path, so the FCS projector validates them before collapsing to
/// stable variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedSourceIdentifierValue {
    SourceDirectory,
    SourceFile,
    Line(String),
}

/// `SynExceptionDefn(SynExceptionDefnRepr(attrs, caseName: SynUnionCase, longId,
/// xmlDoc, accessibility, range), withKeyword, members, range)` (phase 9.15a).
/// `case` is the reused [`NormalisedUnionCase`] (name + `of` fields); `abbrev`
/// is the `= SomeExn` target path (`longId`), `None` for a non-abbreviation;
/// `members` is the `with member …` augmentation (phase 9.15b; always empty in
/// 9.15a). The `access` is projected (see [`NormalisedAccess`]); `withKeyword`
/// / xmlDoc / ranges are elided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedExnDefn {
    /// `SynExceptionDefnRepr.attributes` (phase 10.7m) — the exception's
    /// attribute lists, one inner `Vec` per `SynAttributeList`. FCS concatenates
    /// the *leading* `[<A>] exception …` lists with any *after-keyword*
    /// `exception [<B>] …` lists (`$1 @ cas`, `pars.fsy:1347`), in source order;
    /// the reused `caseName` (`SynUnionCase`) always carries none. Empty for an
    /// unattributed definition.
    pub attributes: Vec<Vec<NormalisedAttribute>>,
    /// `SynExceptionDefnRepr.accessibility` (FCS field 4) — `exception private E
    /// of …`. See [`NormalisedAccess`].
    pub access: Option<NormalisedAccess>,
    pub case: NormalisedUnionCase,
    pub abbrev: Option<Vec<String>>,
    pub members: Vec<NormalisedMember>,
}

/// `SynTypeDefn(typeInfo, typeRepr, members, implicitConstructor, range,
/// trivia)`. Phase 9.1 projects the name (`SynComponentInfo.longId`) and the
/// repr; 9.3 adds the type parameters (`SynComponentInfo.typeParams`); 10.7a
/// adds the header attributes (`SynComponentInfo.attributes`). The header
/// `access` is projected (see [`NormalisedAccess`]); `preferPostfix` / xmlDoc /
/// ranges / trivia are elided, and members / the implicit constructor (empty
/// for an abbreviation) arrive with the object-model slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedTypeDefn {
    /// `SynComponentInfo.attributes` (phase 10.7a) — the type-header attribute
    /// lists, one inner `Vec` per `SynAttributeList`. Empty for an unattributed
    /// definition (and for every `and`-chained definition that carries no
    /// leading `[<…>]`).
    pub attributes: Vec<Vec<NormalisedAttribute>>,
    /// `SynComponentInfo.accessibility` (FCS field 6) — the type's *own*
    /// before-name access modifier (`type internal Foo`, `and private U`).
    /// Distinct from the constructor access ([`NormalisedMember::ImplicitCtor`])
    /// and from the after-name `type C private = …` slot, which FCS discards.
    /// See [`NormalisedAccess`].
    pub access: Option<NormalisedAccess>,
    pub long_id: Vec<String>,
    /// `SynComponentInfo.typeParams` flattened to the typar list (phase 9.3).
    /// The `SynTyparDecls` variant (`PostfixList`/`PrefixList`/`SinglePrefix`)
    /// and `preferPostfix` are elided — `type T<'a>` and `type 'a T` declare
    /// the same parameter. Empty for a non-generic definition.
    pub typars: Vec<NormalisedTypar>,
    /// The type-parameter constraints in source order (phase 9.3b) — the union
    /// of the inside-`<>` `when` clause (`SynTyparDecls.PostfixList` constraints)
    /// and the after-decls one (`SynComponentInfo.constraints`). Empty when the
    /// definition has no `when` clause.
    pub constraints: Vec<NormalisedTypeConstraint>,
    pub repr: NormalisedTypeRepr,
    /// `SynTypeDefn.members` (phase 9.13) — the *outer* member list: an
    /// augmentation's members (`type T with member …`) or trailing members on a
    /// simple repr (`type R = {…} with member …`). Distinct from a pure object
    /// model's members, which live in the
    /// [`ObjectModel`](NormalisedTypeRepr::ObjectModel) repr (slot 1). Empty for
    /// a non-augmented definition.
    pub members: Vec<NormalisedMember>,
    /// `SynTypeDefn.implicitConstructor` (phase 9.8a) — the primary constructor
    /// `type T(args) [as self]`, if any. The same `ImplicitCtor` is *also*
    /// prepended to the [`ObjectModel`](NormalisedTypeRepr::ObjectModel) repr's
    /// member list (FCS populates both slots), so the projectors mirror that
    /// dual placement. `None` for a definition with no primary constructor.
    pub implicit_ctor: Option<NormalisedMember>,
}

/// One `SynTypeConstraint` (`SyntaxTree.fsi:399`, phase 9.3b), reduced to the
/// subject typar (and, for the subtype form, the constraint type). Covers the
/// variants with a parser surface today; `default` is deferred. Shared in
/// spirit with phase 10.12 (`val` signatures), the other `SynTypeConstraint`
/// consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedTypeConstraint {
    /// `'a :> T` — `WhereTyparSubtypeOfType`.
    SubtypeOf {
        typar: NormalisedTypar,
        ty: NormalisedType,
    },
    /// `'a : struct` — `WhereTyparIsValueType`.
    IsValueType(NormalisedTypar),
    /// `'a : not struct` — `WhereTyparIsReferenceType`.
    IsReferenceType(NormalisedTypar),
    /// `'a : null` — `WhereTyparSupportsNull`.
    SupportsNull(NormalisedTypar),
    /// `'a : not null` — `WhereTyparNotSupportsNull`.
    NotSupportsNull(NormalisedTypar),
    /// `'a : comparison` — `WhereTyparIsComparable`.
    IsComparable(NormalisedTypar),
    /// `'a : equality` — `WhereTyparIsEquatable`.
    IsEquatable(NormalisedTypar),
    /// `'a : unmanaged` — `WhereTyparIsUnmanaged`.
    IsUnmanaged(NormalisedTypar),
    /// `^T : (static member M : sig)` — `WhereTyparSupportsMember` (SRTP). The
    /// `member` is the constrained member signature, projected through the
    /// shared [`NormalisedMember`] (the same `SynMemberSig.Member(SynValSig)`
    /// FCS records here and in a signature type body). `support` is the support
    /// *type* list (FCS field 0): a single typar for `^T : (…)` (a
    /// `SynType.Var`), or several for the parenthesised alternatives form
    /// `(^a or ^b) : (…)` / `(Witnesses or ^T) : (…)` (a
    /// `SynType.Paren(SynType.Or(…))`, flattened to its alternatives — the
    /// syntactic `Paren`/`Or` wrapper elided). Each alternative is a
    /// [`NormalisedType`] because FCS's `typeAlts` operands are
    /// `appTypeWithoutNull`: a typar ([`NormalisedType::Var`]) *or* a concrete
    /// type ([`NormalisedType::LongIdent`] / [`NormalisedType::App`]).
    SupportsMember {
        support: Vec<NormalisedType>,
        member: Box<NormalisedMember>,
    },
    /// `'a : enum<'b>` — `WhereTyparIsEnum(typar, typeArgs, range)`. `args` is the
    /// `< … >` type-argument list (one element, the underlying type).
    IsEnum {
        typar: NormalisedTypar,
        args: Vec<NormalisedType>,
    },
    /// `'a : delegate<args, ret>` — `WhereTyparIsDelegate(typar, typeArgs, range)`.
    /// `args` is the `< … >` list (two elements: the tupled argument type and the
    /// return type).
    IsDelegate {
        typar: NormalisedTypar,
        args: Vec<NormalisedType>,
    },
    /// `when IFoo<'T>` — `WhereSelfConstrained(ty, range)` (F# 7 IWSAM
    /// shorthand). Field 0 is the constraint type; there is **no** subject
    /// typar. The syntactic `SELF_CONSTRAINT` wrapper / FCS range is elided.
    SelfConstrained(NormalisedType),
}

/// `SynTyparDecl(attributes, SynTypar(ident, staticReq, _), …)` — the typar
/// name, whether it is the head-type (statically-resolved) form `^a`
/// (`TyparStaticReq.HeadType`) vs plain `'a` (`None`), the leading attribute
/// run (`type T<[<Measure>] 'a>`), and the intersection constraints
/// (`'t & #seq<int>`). Mirrors the [`NormalisedType::Var`] fields plus the
/// attributes and constraints.
///
/// `attributes` is `SynTyparDecl`'s field 0 — one inner `Vec` per
/// `SynAttributeList`, empty for an unattributed typar and for the bare
/// `SynTypar` uses (constraint subjects, trait-call support) where no
/// `SynTyparDecl` wrapper exists.
///
/// `intersection_constraints` is `SynTyparDecl`'s field 2
/// (`'t & #seq<int> & #IDisposable`, the `ConstraintIntersectionOnFlexibleTypes`
/// feature) — each a flexible `SynType` ([`NormalisedType::Hash`]), in source
/// order. Empty for a plain typar and for the bare `SynTypar` uses (no
/// `SynTyparDecl` wrapper). The `AmpersandRanges` trivia is elided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedTypar {
    pub name: String,
    pub head_type: bool,
    pub attributes: Vec<Vec<NormalisedAttribute>>,
    pub intersection_constraints: Vec<NormalisedType>,
}

/// `SynTypeDefnRepr` / `SynTypeDefnSimpleRepr` — the right-hand side of one type
/// definition. Phase 9.1 modelled the [`Abbrev`](NormalisedTypeRepr::Abbrev)
/// form (`SynTypeDefnSimpleRepr.TypeAbbrev`); 9.4 adds
/// [`Record`](NormalisedTypeRepr::Record) (`SynTypeDefnSimpleRepr.Record`,
/// carrying its repr-level `access`). Union, enum, and object-model reprs land
/// in later phase-9 slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedTypeRepr {
    /// `SynTypeDefnSimpleRepr.None` — a **bodyless** type definition (no `=`):
    /// `[<Measure>] type m`, `type Foo`, the `recover`-path `type C(x)`. FCS's
    /// `tyconDefn` bare-`typeNameInfo` / `recover` alternatives. The range is
    /// elided, so this variant carries no payload.
    None,
    Abbrev(NormalisedType),
    /// `SynTypeDefnSimpleRepr.Record(accessibility, recordFields, range)` (phase
    /// 9.4). `access` is the repr-level modifier `type R = private { … }`
    /// (FCS field 0); `fields` the record fields. See [`NormalisedAccess`].
    Record {
        access: Option<NormalisedAccess>,
        fields: Vec<NormalisedField>,
    },
    /// `SynTypeDefnSimpleRepr.Union(accessibility, unionCases, range)` (phase
    /// 9.5). `access` is the repr-level modifier `type U = private | A`
    /// (FCS field 0); `cases` the union cases. See [`NormalisedAccess`].
    Union {
        access: Option<NormalisedAccess>,
        cases: Vec<NormalisedUnionCase>,
    },
    /// `SynTypeDefnSimpleRepr.Enum` (phase 9.6) — the enum's cases.
    Enum(Vec<NormalisedEnumCase>),
    /// `SynTypeDefnRepr.ObjectModel(kind, members, _)` (phase 9.7) — a
    /// `member`-bearing class-like body. `kind` is the `SynTypeDefnKind`
    /// (`Unspecified` for a bare `type T = member …`; the explicit
    /// `class`/`struct`/`interface` markers and `Augmentation` are later
    /// slices). `members` is the repr's member list (slot 1; the outer
    /// `SynTypeDefn.members` augmentation slot and the `implicitConstructor`
    /// slot arrive with 9.13/9.8 respectively).
    ObjectModel {
        kind: NormalisedTypeDefnKind,
        members: Vec<NormalisedMember>,
    },
    /// A delegate body — `type T = delegate of int -> int`. FCS lowers this to
    /// `ObjectModel(SynTypeDefnKind.Delegate(ty, arity), [AbstractSlot
    /// "Invoke"], _)`; both the `arity` (`SynValInfo`) and the synthetic
    /// `Invoke` slot are derived from the same signature type, so we keep only
    /// the signature `ty` here (the CST side mirrors it from the
    /// `DELEGATE_REPR`'s `<type>` child).
    Delegate(NormalisedType),
}

/// `SynTypeDefnKind` (`SyntaxTree.fsi:1356`) — the object-model "shape". Phase
/// 9.7 only reaches `Unspecified` (a bare `type T = member …`); `Class` /
/// `Struct` / `Interface` (9.12) and `Augmentation` (9.13) extend this. The
/// abbreviation/record/union/enum kinds are modelled by the distinct
/// [`NormalisedTypeRepr`] variants, so they never appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalisedTypeDefnKind {
    Unspecified,
    /// `SynTypeDefnKind.Augmentation` — a `type T with member …` augmentation
    /// (phase 9.13a). The repr's own member list is empty; the members live in
    /// the outer [`NormalisedTypeDefn::members`] slot.
    Augmentation,
    /// `SynTypeDefnKind.Class` — an explicit `type T = class … end` (phase 9.12).
    Class,
    /// `SynTypeDefnKind.Struct` — an explicit `type T = struct … end` (phase 9.12).
    Struct,
    /// `SynTypeDefnKind.Interface` — an explicit `type T = interface … end`
    /// (phase 9.12).
    Interface,
}

/// One `SynMemberDefn` (`SyntaxTree.fsi:1656`) inside an object model. Phase
/// 9.7 models the [`Member`](NormalisedMember::Member) form — a member binding,
/// reusing [`NormalisedBinding`] (its `SynLeadingKeyword.Member` distinguishes
/// it from a `let`; the `SynValData`/`SynMemberFlags` — always an instance
/// member at 9.7 — are elided). The implicit constructor, `val` fields,
/// `inherit`/`interface`, abstract slots, auto-properties, and get/set members
/// are later phase-9 slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedMember {
    Member(NormalisedBinding),
    /// `SynMemberDefn.ImplicitCtor` (phase 9.8a) — the implicit primary
    /// constructor. `args` is the constructor argument pattern (FCS 43.x's
    /// `ctorArgs: SynPat`: `Const(Unit)` for `()`, `Paren(…)` otherwise),
    /// reusing the shared [`NormalisedPat`] projector; `self_id` is the
    /// optional `as <self>` identifier; `attributes` is `ImplicitCtor.attributes`
    /// (phase 10.7j, FCS field 1 — `type T [<A>] ()`); `access` is the ctor
    /// modifier (`type C private (x)`, see [`NormalisedAccess`]). xmlDoc is elided.
    ImplicitCtor {
        args: NormalisedPat,
        self_id: Option<String>,
        attributes: Vec<Vec<NormalisedAttribute>>,
        /// `SynMemberDefn.ImplicitCtor.accessibility` (FCS field 0) — `type C
        /// private (x)`. See [`NormalisedAccess`].
        access: Option<NormalisedAccess>,
    },
    /// `SynMemberDefn.LetBindings` (phase 9.8b/9.8c) — class-local `let`/`let
    /// rec` bindings. `is_rec` is `isRecursive` (field 2); `bindings` reuses
    /// [`NormalisedBinding`] (each carries its `SynLeadingKeyword`
    /// `Let`/`LetRec`/`And`). The `isStatic` field (field 1) is elided — for a
    /// `static let` (9.8c) the static-ness instead rides on the head binding's
    /// leading keyword (`StaticLet`/`StaticLetRec`, which
    /// `mkClassMemberLocalBindings` rewrites in place of `Let`/`LetRec`). The
    /// `do`-binding form is a later slice.
    LetBindings {
        is_rec: bool,
        bindings: Vec<NormalisedBinding>,
    },
    /// `SynMemberDefn.ValField` (phase 9.9b) — an explicit `val` field, reusing
    /// the shared [`NormalisedField`] (`name`/`ty`/`is_mutable`/`is_static`).
    ValField(NormalisedField),
    /// `SynMemberDefn.Inherit` (phase 9.11a) — a base-class clause with no
    /// constructor arguments (`inherit Base`). `base_type` is FCS's `baseType:
    /// SynType option` — `Some` for the normal form, `None` only on the
    /// `inherit`-with-no-type error-recovery production. The `asIdent` (`as
    /// base`) and trivia are elided.
    Inherit {
        base_type: Option<NormalisedType>,
    },
    /// `SynMemberDefn.ImplicitInherit` (phase 9.11a) — a base-class clause with
    /// constructor arguments (`inherit Base(args)`). `base_type` is FCS's
    /// `inheritType: SynType`; `args` is `inheritArgs: SynExpr` (`Const(Unit)`
    /// for `()`, `Paren(…)` otherwise), reusing the shared [`NormalisedExpr`]
    /// projector. The `inheritAlias` (`as base`) and trivia are elided.
    ImplicitInherit {
        base_type: NormalisedType,
        args: NormalisedExpr,
    },
    /// `SynMemberDefn.Interface` (phase 9.11b) — an interface implementation
    /// `interface I [with member …]`. `interface_type` is FCS's `interfaceType`
    /// (reusing [`NormalisedType`]); `members` is the `members: SynMemberDefns
    /// option` — `None` for a bare `interface I`, `Some(list)` for a `with`
    /// block (possibly empty). `withKeyword` and trivia are elided.
    Interface {
        interface_type: NormalisedType,
        members: Option<Vec<NormalisedMember>>,
    },
    /// `SynMemberDefn.GetSetMember` (phase 9.14) — a property with explicit
    /// `get`/`set` accessors, `member this.P with get() = … [and set …]`. `name`
    /// is the property path (FCS duplicates it into each accessor binding's
    /// `headPat.longDotId`; we project it once); `get`/`set` are the present
    /// accessors. Accessibility rides on each [`NormalisedAccessor::access`] — a
    /// per-accessor `and private set`, or a member-level `member private this.P`
    /// which FCS folds onto every present accessor. Attributes, member flags and
    /// trivia are elided.
    GetSetMember {
        name: Vec<String>,
        get: Option<NormalisedAccessor>,
        set: Option<NormalisedAccessor>,
    },
    /// `SynMemberDefn.AutoProperty` (phase 9.9c) — an auto-implemented property
    /// `[static] member val [access] X [: T] = <expr> [with get[, set]]`. `name`
    /// is the property identifier (field 2); `is_static` is `isStatic` (field 1);
    /// `ty` is the optional type annotation (`typeOpt`, field 3); `prop_kind`
    /// (field 4, driven by the `with get[, set]` clause) is the
    /// getter/setter shape; `expr` is the initialiser RHS (field 9);
    /// `attributes` is `AutoProperty.attributes` (phase 10.7h, FCS field 0);
    /// `access` is the *overall* modifier (`member val private X`, the
    /// `SynValSigAccess` field-0 slot — a per-accessor `with private get` is
    /// elided). Member flags and xmlDoc are elided.
    AutoProperty {
        name: String,
        is_static: bool,
        ty: Option<NormalisedType>,
        prop_kind: NormalisedPropKind,
        expr: NormalisedExpr,
        attributes: Vec<Vec<NormalisedAttribute>>,
        /// `SynMemberDefn.AutoProperty.accessibility` (FCS field 8, a
        /// `SynValSigAccess` — the *overall* access slot). `member val private X
        /// = …`. Per-getter/setter access on a `with get, set` clause remains
        /// elided. See [`NormalisedAccess`].
        access: Option<NormalisedAccess>,
    },
    /// `SynMemberDefn.AbstractSlot` (phase 9.10c) — an abstract member slot
    /// `abstract [member] Name : <type>`, a `SynValSig` with no `= <expr>` body —
    /// **and** the shared shape for a signature-file member sig
    /// (`SynMemberSig.Member`, phase 10.14). `name` is the slot ident; `ty` the
    /// signature type (`SynValSig.synType`, reusing [`NormalisedType`]);
    /// `leading_keyword` is `Abstract`/`AbstractMember`/`Member`/`StaticMember`/…;
    /// `attributes` is `SynValSig.attributes` (phase 10.7g, FCS field 0).
    /// `literal` is the optional `= <literal>` value (`SynValSig.synExpr`, field 9;
    /// phase 10.12 member-literal) — a full [`NormalisedExpr`], `None` when absent.
    /// Only ever `Some` on a **signature-file** member sig (`member a : int = 10`,
    /// `abstract`/`static member` carriers); an *impl*-side `SynMemberDefn.Abstract‑
    /// Slot` is bodyless (FCS rejects `abstract M : int = 1` — FS0010), so its
    /// projection is always `None`. The `SynValInfo` arity, `SynMemberFlags`,
    /// explicit typar decls, and inline/mutable flags are elided.
    AbstractSlot {
        name: String,
        ty: NormalisedType,
        leading_keyword: NormalisedLeadingKeyword,
        attributes: Vec<Vec<NormalisedAttribute>>,
        literal: Option<Box<NormalisedExpr>>,
        /// `SynValSig.accessibility` (FCS field 8, a `SynValSigAccess` — the
        /// *overall* access slot). `Some` only on a legal **signature-file**
        /// member sig (`member internal M : int`); always `None` on an *impl*
        /// abstract slot, where an access modifier is illegal and FCS discards
        /// it. See [`NormalisedAccess`].
        access: Option<NormalisedAccess>,
    },
}

/// One accessor of a [`NormalisedMember::GetSetMember`] (phase 9.14) — FCS's
/// per-accessor `SynBinding`, reduced to its argument patterns (`get()` →
/// `[Paren(Const Unit)]`, `set v` → `[Named v]`) and its body expression. The
/// accessor's `get`/`set` `extraId`, the shared property path, `SynValData`,
/// flags and trivia are elided (the path lives on
/// [`NormalisedMember::GetSetMember::name`]; the get-vs-set distinction is the
/// slot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedAccessor {
    /// The accessor binding's `SynBinding.attributes` (phase 10.7f). A get/set
    /// property's leading `[<…>]` is duplicated by FCS onto *both* accessor
    /// bindings, so each present accessor carries the same lists. One inner vec
    /// per `SynAttributeList`.
    pub attributes: Vec<Vec<NormalisedAttribute>>,
    /// The accessor binding's head-pattern accessibility (FCS's `SynBinding`
    /// `headPat.SynPat.LongIdent.accessibility`, field 4) — `member this.P with
    /// get() = … and private set v = …`. See [`NormalisedAccess`].
    pub access: Option<NormalisedAccess>,
    pub args: Vec<NormalisedPat>,
    pub body: NormalisedExpr,
}

/// `SynMemberKind` restricted to the cases an auto-property's `propKind`
/// (phase 9.9c) can take: a plain `member val X = e` (`Member`), `with get`
/// (`PropertyGet`), `with set` (`PropertySet` — grammar-accepted, though later
/// rejected semantically), or `with get, set` (`PropertyGetSet`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedPropKind {
    Member,
    PropertyGet,
    PropertySet,
    PropertyGetSet,
}

/// One `SynEnumCase` (phase 9.6): the case `ident` and its `value` expression
/// (FCS's `valueExpr`, a `SynExpr` such as `Const 0` — *not* a `SynConst`).
/// `attributes` is `SynEnumCase.attributes` (phase 10.7, field 0); xmlDoc elided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedEnumCase {
    pub attributes: Vec<Vec<NormalisedAttribute>>,
    pub ident: String,
    pub value: NormalisedExpr,
}

/// One `SynUnionCase` (phase 9.5): the case `ident` and its `caseType`
/// ([`NormalisedUnionCaseKind`]). `attributes` is `SynUnionCase.attributes`
/// (phase 10.7, field 0); accessibility / xmlDoc elided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedUnionCase {
    pub attributes: Vec<Vec<NormalisedAttribute>>,
    pub ident: String,
    pub kind: NormalisedUnionCaseKind,
}

/// `SynUnionCaseKind` (`SyntaxTree.fsi:1331`) — a union case's representation.
/// The ordinary [`Fields`](NormalisedUnionCaseKind::Fields) form (`of T1 * …`,
/// each a [`NormalisedField`] — anonymous or `name : T`; union-case fields are
/// never `mutable`) and the FSharp.Core-only
/// [`FullType`](NormalisedUnionCaseKind::FullType) signature form (`Name :
/// topType`, `| Some : Value:'T -> 'T option`). The `FullType`'s `fullTypeInfo`
/// (`SynValInfo`, derived from the signature type's labels) is elided — the
/// `fullType` `SynType` already carries the labelled parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedUnionCaseKind {
    Fields(Vec<NormalisedField>),
    FullType(NormalisedType),
}

/// A `SynField` as a record field (phase 9.4). `name` is `SynField.idOpt`
/// (always `Some` for a record field), `ty` the `fieldType`, `is_mutable` the
/// `isMutable` flag. `attributes` is `SynField.attributes` (phase 10.7, field 0)
/// — populated for record fields (10.7b) and `val` fields (10.7i); union-case
/// `of` fields leave it empty (that attribute carrier is a later slice). The
/// `access` (`val` fields only, see [`NormalisedAccess`]) is projected;
/// `isStatic` (record: always `false`) / xmlDoc / ranges / trivia are elided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedField {
    pub attributes: Vec<Vec<NormalisedAttribute>>,
    /// `SynField.accessibility` (FCS field 6) — a `val` field's access
    /// (`val mutable internal x : int`, phase 9.9b). Always `None` for a record
    /// (9.4) or union-case (9.5) field. See [`NormalisedAccess`].
    pub access: Option<NormalisedAccess>,
    pub name: Option<String>,
    pub ty: NormalisedType,
    pub is_mutable: bool,
    /// `SynField.isStatic` — `true` only for a `static val` field (phase 9.9b);
    /// always `false` for a record (9.4) or union-case (9.5) field.
    pub is_static: bool,
}

/// `SynOpenDeclTarget` — `open Foo.Bar` (`ModuleOrNamespace`, a dotted path)
/// vs `open type T` (`Type`, an opened type). Ranges (and the open target's
/// own `SynLongIdent` dot/comma ranges) are elided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedOpenTarget {
    ModuleOrNamespace(Vec<String>),
    Type(NormalisedType),
}

/// `SynBinding` — the normaliser models a non-typed, public binding whose LHS
/// is a value-form [`NormalisedPat::Named`] or a function-form
/// [`NormalisedPat::LongIdent`] and whose RHS is any [`NormalisedExpr`].
/// Return-type, xmlDoc, value-info and trivia are elided like ranges
/// (see `docs/parser-plan.md` D4). `attributes` carries the binding's
/// `SynAttributes` (phase 10.5) as a list-of-lists, faithful to FCS's
/// `SynAttributeList list`; empty for an unattributed binding.
/// `SynBinding.Trivia.LeadingKeyword` (`SynLeadingKeyword`,
/// `SyntaxTrivia.fsi:304`), restricted to the variants reachable from a
/// `let`-binding head. `Let`/`LetRec`/`And`/`Use`/`UseRec` cover the
/// module-level and expression-level `let`/`use` forms; `LetBang`/`UseBang`/
/// `AndBang` cover the computation-expression bang binders (phase 10.4b);
/// [`Member`](NormalisedLeadingKeyword::Member) covers an object-model instance
/// member (phase 9.7); [`StaticMember`](NormalisedLeadingKeyword::StaticMember)
/// a `static member` (phase 9.9a);
/// [`StaticLet`](NormalisedLeadingKeyword::StaticLet) /
/// [`StaticLetRec`](NormalisedLeadingKeyword::StaticLetRec) the head binding of
/// a `static let` / `static let rec` (phase 9.8c). The remaining member/static
/// leading
/// keywords — `Override`, `Default`, `Static`, `AbstractMember`, `Val`, `New` —
/// are **declared but not yet projected**: reserved as enum variants (so
/// concurrent phase-9/10 branches don't collide on this enum), but no
/// `fcs_leading_keyword` arm maps them and nothing constructs them yet, so the
/// FCS-side projector still `panic!`s on an out-of-scope leading keyword rather
/// than projecting wrong. Each slice adds its arm when it ground-truths the FCS
/// discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalisedLeadingKeyword {
    Let,
    LetRec,
    And,
    Use,
    UseRec,
    LetBang,
    UseBang,
    AndBang,
    /// `SynLeadingKeyword.Member` — an object-model instance member
    /// (`member this.M = …`, phase 9.7).
    Member,
    /// `SynLeadingKeyword.StaticMember` — a `static member M = …` (phase 9.9a).
    StaticMember,
    /// `SynLeadingKeyword.StaticLet` — the head binding of a `static let` (phase
    /// 9.8c); `mkClassMemberLocalBindings` rewrites `Let` → `StaticLet`.
    StaticLet,
    /// `SynLeadingKeyword.StaticLetRec` — the head binding of a `static let rec`
    /// (phase 9.8c); `mkClassMemberLocalBindings` rewrites `LetRec` →
    /// `StaticLetRec`.
    StaticLetRec,
    /// `SynLeadingKeyword.Do` — a class-body `do <expr>` binding (phase 9.8d),
    /// FCS's `SynBinding` of kind `Do` inside a `SynMemberDefn.LetBindings`. Its
    /// head pattern is a synthetic `SynPat.Const(Unit)`; the `do` body lives in
    /// `SynBinding.expr`.
    Do,
    /// `SynLeadingKeyword.StaticDo` — a class-body `static do <expr>` binding
    /// (phase 9.8d), the static counterpart of [`Do`](NormalisedLeadingKeyword::Do)
    /// (`SynMemberDefn.LetBindings([Do …], isStatic = true)`).
    StaticDo,
    /// `SynLeadingKeyword.Synthetic` — a binding with no source keyword of its
    /// own. The object-expression value-binding form `{ new T() with X = e }`
    /// (FCS's `objExprBindings`) builds its head binding with this (the shared
    /// `with` is not a per-binding keyword); `and`-chained bindings still get
    /// [`And`](NormalisedLeadingKeyword::And).
    Synthetic,

    // Reserved for later phase-9/10 member slices: declared so concurrent
    // branches don't collide on this enum; not yet projected (no
    // `fcs_leading_keyword` arm, no construction site). See `docs/parser-plan.md`.
    /// `override this.M() = …` (phase 9.10).
    Override,
    /// `default this.M() = …` (phase 9.10).
    Default,
    /// a `static` non-member binding, e.g. a static `let`/`do` (phase 9.9).
    Static,
    /// `abstract M : …` — a bare abstract member slot (phase 9.10c).
    Abstract,
    /// `abstract member M : …` — an abstract member slot (phase 9.10c).
    AbstractMember,
    /// `static abstract M : …` — a static-abstract interface member sig
    /// (phase 10.14, slice 3a).
    StaticAbstract,
    /// `static abstract member M : …` — a static-abstract interface member sig
    /// (phase 10.14, slice 3a).
    StaticAbstractMember,
    /// a `val` field (phase 9.9b) or a value signature (phase 10.12).
    Val,
    /// an explicit constructor `new(args) = …` (phase 9.10).
    New,
    /// `SynLeadingKeyword.Extern` — an `extern` DllImport prototype (FCS's
    /// `cPrototype`, `pars.fsy:3186`), lowered to a `SynModuleDecl.Let` whose
    /// binding has this leading keyword, a `LongIdent(name, Pats[Tuple[…]])` head
    /// pattern of C-typed arguments, and a synthetic `failwith "…"` RHS.
    Extern,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedBinding {
    /// `SynBinding.Trivia.LeadingKeyword` — which keyword introduced this
    /// binding. Distinguishes `let`/`use`/`and` and their bang forms, and
    /// (via `LetRec`/`UseRec`) records the `rec` of the group's head binding,
    /// mirroring FCS (which carries both this and `SynModuleDecl.Let.isRec`).
    pub leading_keyword: NormalisedLeadingKeyword,
    pub is_mutable: bool,
    pub is_inline: bool,
    pub attributes: Vec<Vec<NormalisedAttribute>>,
    /// The binding's accessibility — FCS stores it on the head pattern
    /// (`SynPat.Named.accessibility` field 2 for a value binding, or
    /// `SynPat.LongIdent.accessibility` field 4 for a function / member / ctor
    /// head), *not* on `SynBinding` itself. Our CST captures it as an
    /// `ACCESS_TOK` child of the `BINDING` node. Covers `let private x`,
    /// `let internal f a`, `member private this.M`, `private new(…)`. See
    /// [`NormalisedAccess`].
    pub access: Option<NormalisedAccess>,
    pub pat: NormalisedPat,
    pub expr: NormalisedExpr,
}

/// `SynAttribute { TypeName; ArgExpr; Target; AppliesToGetterAndSetter; Range }`.
/// `type_name` is the `TypeName` `SynLongIdent` segments; `arg` is the
/// `ArgExpr` (a bare `[<Foo>]` carries FCS's synthetic `mkSynUnit`, i.e.
/// `Const(Unit)`); `target` is the optional `Target` ident's `idText`
/// (`[<assembly: …>]` etc.). `AppliesToGetterAndSetter` and `Range` are elided.
/// Phase 10.5b populates `arg` from the parsed argument expression (bare
/// attributes still yield `Const(Unit)`); phase 10.5c populates `target` from
/// the `attributeTarget` word (`None` for an untargeted attribute).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedAttribute {
    pub type_name: Vec<String>,
    pub target: Option<String>,
    pub arg: NormalisedExpr,
}

/// FCS's `SynArgPats` — the argument payload of a [`NormalisedPat::LongIdent`].
/// Either the curried list of atomic argument patterns (`Some x`, `f a b`) or
/// the named-field group of an applied union-case pattern (`Case (field = pat;
/// …)`). The two are mutually exclusive in FCS's grammar
/// (`atomicPatsOrNamePatPairs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedArgPats {
    /// `SynArgPats.Pats(pats)` — the curried argument patterns, each itself a
    /// `NormalisedPat`. Empty for a nullary head (`None`, `Foo.Bar`).
    Pats(Vec<NormalisedPat>),
    /// `SynArgPats.NamePatPairs(pats, range, trivia)` — the named-field group.
    /// Each entry is `(field_name, value_pat)`; `field_name` is the single
    /// `idText` (FCS wraps it in a one-segment `SynLongIdent`), in source order.
    NamePatPairs(Vec<(String, NormalisedPat)>),
}

/// A `SynPat` — currently the [`Named`] form (`let x = …`), the
/// [`LongIdent`] form (`let f x y = …`), the [`Wildcard`] form (`_`,
/// either as a value-form head or a function-form arg), the three
/// atomic variants added in phase 6.1 ([`Paren`], [`Const`], [`Null`]),
/// and the [`Typed`] form added in phase 6.2 (`pat : T`, always
/// reached through `parenPattern COLON typeWithTypeConstraints` so the
/// `Typed` node lives one level beneath a `Paren`). Tuple / or /
/// list-cons / record patterns arrive later.
///
/// [`Named`]: NormalisedPat::Named
/// [`LongIdent`]: NormalisedPat::LongIdent
/// [`Wildcard`]: NormalisedPat::Wildcard
/// [`Paren`]: NormalisedPat::Paren
/// [`Const`]: NormalisedPat::Const
/// [`Null`]: NormalisedPat::Null
/// [`Typed`]: NormalisedPat::Typed
/// [`Tuple`]: NormalisedPat::Tuple
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedPat {
    /// `SynPat.Named(SynIdent(idText, _), isThisVal=false, accessibility, _)`.
    /// The single string is the FCS `idText` (backticks stripped). A binding
    /// head's `accessibility` (`let private x`) is *not* projected here — it is
    /// read at the binding level ([`NormalisedBinding::access`]), so the pattern
    /// stays access-agnostic in every context it appears.
    Named(String),
    /// `SynPat.LongIdent(longDotId, extraId=None, typars, args, accessibility,
    /// range)` — function-form binding head / applied union-case pattern. Any
    /// binding-head `accessibility` (`let private f a`, `private new(…)`) is
    /// projected at the binding level ([`NormalisedBinding::access`]), not here.
    /// `head` is the path segments of `longDotId`
    /// (single-segment for a bare value/function head, multi-segment for a
    /// dotted union-case path). `typars` is FCS's `typars: SynValTyparDecls
    /// option` (field 2) flattened to the typar list — non-empty for an
    /// explicit-generic head (`let f<'a> …`, `let h<'a> = …`), empty otherwise
    /// (including FCS's synthetic `noInferredTypars` ctor-head marker, which
    /// carries no real decls). `args` is FCS's `SynArgPats` — either the
    /// curried `Pats` list or the named-field `NamePatPairs` group (see
    /// [`NormalisedArgPats`]).
    LongIdent {
        head: Vec<String>,
        typars: Vec<NormalisedTypar>,
        args: NormalisedArgPats,
    },
    /// `SynPat.Wild(range)` — the wildcard `_`. Carries no payload; the
    /// range is elided like every other normalised node.
    Wildcard,
    /// `SynPat.Paren(pat, range)` — a parenthesised pattern. FCS keeps
    /// these in the AST so the projector preserves the wrapping on both
    /// sides; a tree-shape divergence (one side folds, the other
    /// doesn't) would otherwise show up as a phantom diff.
    Paren(Box<NormalisedPat>),
    /// `SynPat.Const(constant, range)` — a literal-headed pattern.
    /// Reuses the expression-side `NormalisedConst` since both project
    /// to the same FCS `SynConst`.
    Const(NormalisedConst),
    /// `SynPat.Null(range)` — the `null` pattern. Carries no payload.
    Null,
    /// `SynPat.Typed(pat, targetType, range)` — `pat : T`. Always
    /// appears wrapped in a [`Paren`] because FCS only reaches the
    /// `Typed` constructor through `parenPattern COLON
    /// typeWithTypeConstraints` (`pars.fsy:3929`).
    ///
    /// [`Paren`]: NormalisedPat::Paren
    Typed {
        pat: Box<NormalisedPat>,
        ty: NormalisedType,
    },
    /// `SynPat.Tuple(isStruct, elementPats, commaRanges, range)` — a
    /// comma-separated tuple pattern. Two-or-more elements (FCS's grammar
    /// reaches this via `applPats (COMMA applPat)+` for the ref-tuple form and
    /// `STRUCT LPAREN tupleParenPatternElements rparen` for the struct form).
    /// `is_struct` records FCS's `isStruct` field — `true` for `struct (x, y)`
    /// (which produces the tuple *directly*, no `Paren` wrapper), `false` for a
    /// plain `x, y` / `(x, y)`. Mirrors [`NormalisedExpr::Tuple`].
    Tuple {
        is_struct: bool,
        elements: Vec<NormalisedPat>,
    },
    /// `SynPat.As(lhsPat, rhsPat, range)` — an `as`-pattern `p1 as p2`.
    /// FCS reaches this via `headBindingPattern AS constrPattern`
    /// (`pars.fsy:3570`) and `parenPattern AS constrPattern`
    /// (`pars.fsy:3902`); `%right AS` makes `as` the lowest pattern
    /// precedence, so the left operand absorbs any tuple to its left and
    /// chains left-nested. The range is elided like every other node.
    As {
        lhs: Box<NormalisedPat>,
        rhs: Box<NormalisedPat>,
    },
    /// `SynPat.ArrayOrList(isArray, elementPats, range)` — a list `[ … ]`
    /// (`is_array=false`) or array `[| … |]` (`is_array=true`) pattern.
    /// `elements` is FCS's `elementPats`; each is itself a full pattern, so
    /// `[a, b]` is a one-element list whose element is a `Tuple` (`;`, not
    /// `,`, separates list elements). Empty is valid (`[]` / `[||]`).
    ArrayOrList {
        is_array: bool,
        elements: Vec<NormalisedPat>,
    },
    /// `SynPat.Record(fieldPats: NamePatPairField list, range)` — a record
    /// pattern `{ X = p; … }`. Each field is `(name, pat)`: `name` is the
    /// field's `SynLongIdent` segments (FCS's `path`, so `{ M.X = p }` has a
    /// multi-segment name), `pat` is the field-value `parenPattern`. Fields
    /// are kept in source order; the `=`/`;` ranges and `NamePatPairField`'s
    /// own trivia/range slots are elided.
    Record {
        fields: Vec<(Vec<String>, NormalisedPat)>,
    },
    /// `SynPat.IsInst(pat: SynType, range)` — the dynamic type-test pattern
    /// `:? T`. FCS reaches this via `constrPattern: COLON_QMARK
    /// atomTypeOrAnonRecdType` (`pars.fsy:3729`); the `pat` field is a
    /// `SynType` (despite the name), so `ty` reuses the type-side projector.
    /// The classic downcast `:? T as x` is `As(IsInst(T), Named x)`.
    IsInst { ty: NormalisedType },
    /// `SynPat.ListCons(lhsPat, rhsPat, range, trivia)` — the cons pattern
    /// `h :: t`. FCS reaches this via `parenPattern COLON_COLON parenPattern`
    /// (`pars.fsy:3944`), `%right COLON_COLON` (`:361`) — the tightest infix
    /// pattern operator, right-associative, so `a :: b :: c` is
    /// `ListCons(a, ListCons(b, c))`. The `ColonColonRange` trivia is elided.
    ListCons {
        lhs: Box<NormalisedPat>,
        rhs: Box<NormalisedPat>,
    },
    /// `SynPat.Ands(pats, range)` — the conjunction pattern `a & b & c`. FCS
    /// reaches this via `conjPatternElements`/`conjParenPatternElements`
    /// (`pars.fsy:3649`/`:4000`), `%left AMP` (`:355`). The list is flat
    /// (n-ary): `a & b & c` is `Ands[a, b, c]`. Tighter than `,`/`:`/`as`,
    /// looser than `::`.
    Ands { pats: Vec<NormalisedPat> },
    /// `SynPat.Or(lhsPat, rhsPat, range, trivia)` — the or-pattern `A | B`. FCS
    /// reaches this via `headBindingPattern barCanBeRightBeforeNull
    /// headBindingPattern` / the `parenPattern` form (`pars.fsy:3584`/`:3916`),
    /// `%left BAR` (`:266`) — the loosest infix but `as`, left-associative, so
    /// `A | B | C` is `Or(Or(A,B), C)`. The `BarRange` trivia is elided.
    Or {
        lhs: Box<NormalisedPat>,
        rhs: Box<NormalisedPat>,
    },
    /// `SynPat.Attrib(pat, attributes, range)` — an attributed pattern
    /// `[<Foo>] p`. FCS reaches this only via `attributes parenPattern`
    /// (`pars.fsy:3940`), so it appears inside parens / list / array elements,
    /// prefixing the inner `pat`. `attributes` mirrors the let-binding carrier's
    /// `Vec<Vec<NormalisedAttribute>>` shape (one inner `Vec` per
    /// `SynAttributeList`, with adjacent lists grouped). The attrib prefix binds
    /// tighter than `,`/`as`/`|` and looser than `:`/`&`/`::`, so the inner pat
    /// absorbs the latter (e.g. `([<A>] h :: t)` is `Attrib(ListCons …)`).
    Attrib {
        pat: Box<NormalisedPat>,
        attributes: Vec<Vec<NormalisedAttribute>>,
    },
    /// `SynPat.OptionalVal(ident: Ident, range)` — the optional-argument pattern
    /// `?ident`. FCS reaches this via `atomicPattern: QMARK ident`
    /// (`pars.fsy:3802`); the single string is the FCS `idText` (backticks
    /// stripped), exactly as [`Named`] carries it. Optional arguments are only
    /// *semantically* valid on type members, but that check is post-parse, so
    /// the pattern appears in the `ParsedInput` wherever an atomic pattern can.
    ///
    /// [`Named`]: NormalisedPat::Named
    OptionalVal(String),
    /// `SynPat.QuoteExpr(expr: SynExpr, range)` (`SyntaxTree.fsi:1161`) — a code
    /// quotation `<@ … @>` in pattern position, the parameter of a
    /// parameterised active pattern (`SpecificCall <@ f @> (args)`). The inner
    /// `expr` is a full `SynExpr.Quote`, so it projects to a
    /// [`NormalisedExpr::Quote`].
    QuoteExpr(Box<NormalisedExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedExpr {
    Const(NormalisedConst),
    /// `SynExpr.Null(range)` — the `null` literal expression. FCS keeps
    /// this distinct from [`Const`](NormalisedExpr::Const) (`null` is not
    /// a `SynConst`), so it has no payload; the range is elided.
    Null,
    /// `SynExpr.Ident` — FCS's optimised representation of a one-segment
    /// `SynLongIdent` (`SyntaxTree.fsi:805`). The string is the FCS
    /// `Ident.idText`: backticks stripped, anything else verbatim.
    Ident(String),
    /// `SynExpr.Typar(SynTypar, range)` — the F# 7 typar expression `'T`, a
    /// type parameter used as an expression (the head of a statically-resolved
    /// `'T.Member` call). The `String` is the typar name (FCS's `Ident.idText`,
    /// backticks stripped). The `SynTypar`'s static-requirement is always `None`
    /// (FCS's `QUOTE ident` production hard-codes it) and the range is elided,
    /// so only the name is carried.
    Typar(String),
    /// `SynExpr.LongIdent` — a dotted-path expression. Each `String` is
    /// the `Ident.idText` for a segment (backticks stripped). The diff
    /// harness ignores `dotRanges` and `IdentTrivia` (range-elision and
    /// trivia-elision live in this projector by design). Source-written
    /// expressions only reach this representation with two-or-more
    /// segments (FCS uses [`Ident`] for one), but the `fun`-parameter
    /// lowering also synthesises a *single*-segment `LongIdent` as the
    /// scrutinee for a nullary union-case arg (`fun X -> …` ⇒
    /// `match X with X -> …`), so a one-element `Vec` is valid here.
    ///
    /// [`Ident`]: NormalisedExpr::Ident
    LongIdent(Vec<String>),
    /// `SynExpr.Paren` — a parenthesised expression `( e )`. The boxed
    /// inner expression is the wrapped value; FCS's `leftParenRange`,
    /// `rightParenRange`, and the outer `range` are elided by the
    /// projector (the diff is shape-only).
    Paren(Box<NormalisedExpr>),
    /// `SynExpr.Tuple` — a comma-separated tuple. The boolean records
    /// whether it's the `struct (..)` form (FCS's `isStruct` field). The
    /// `Vec` is the tuple's elements in source order (FCS's `exprs`);
    /// `commaRanges` and the outer `range` are elided.
    Tuple {
        is_struct: bool,
        elements: Vec<NormalisedExpr>,
    },
    /// `SynExpr.App` — function application `f x`. Mirrors FCS's
    /// `App(ExprAtomicFlag, isInfix, funcExpr, argExpr, range)`:
    /// `is_atomic` is `true` for adjacent `f(x)` (FCS's `ExprAtomicFlag.Atomic`,
    /// serialised as `0`) and `false` for whitespace-separated `f x`
    /// (`NonAtomic` / `1`). `is_infix` is set when an operator was
    /// flipped from `(+) x y` to `App(App(+, x), y)`-with-isInfix=true; we
    /// project it through but Phase 3.3 only produces `false`. The boxed
    /// pair are `funcExpr` and `argExpr`. `range` is elided.
    App {
        is_atomic: bool,
        is_infix: bool,
        func: Box<NormalisedExpr>,
        arg: Box<NormalisedExpr>,
    },
    /// `SynExpr.DotGet` — postfix member access `expr.Member` (phase
    /// 10.16a). Mirrors FCS's `DotGet of expr * rangeOfDot * longDotId:
    /// SynLongIdent * range`. `expr` is the boxed LHS; `long_dot_id` is the
    /// member path's segment texts (backticks stripped), matching how
    /// [`NormalisedExpr::LongIdent`] projects a `SynLongIdent`. Only produced
    /// for a non-ident LHS — an identifier chain `a.b.c` is
    /// [`NormalisedExpr::LongIdent`]. Both ranges and the trivia list are
    /// elided.
    DotGet {
        expr: Box<NormalisedExpr>,
        long_dot_id: Vec<String>,
    },
    /// `SynExpr.Dynamic(funcExpr, qmarkRange, argExpr, range)` — the dynamic
    /// lookup `a?b`. `lhs` is the boxed `funcExpr`; `arg` is the boxed `argExpr`
    /// — an [`Ident`](NormalisedExpr::Ident) for the `a?b` member-name form, or a
    /// [`Paren`](NormalisedExpr::Paren) for the `a?(e)` form. The qmark range and
    /// outer range are elided.
    Dynamic {
        lhs: Box<NormalisedExpr>,
        arg: Box<NormalisedExpr>,
    },
    /// `SynExpr.DotLambda` — the accessor-function shorthand `_.member`
    /// (`LanguageFeature.AccessorFunctionShorthand`). Mirrors FCS's
    /// `DotLambda of expr: SynExpr * range * trivia: SynExprDotLambdaTrivia`.
    /// `expr` is the boxed body (the `atomicExpr` after `_.`); the synthesised
    /// lambda parameter is post-parse, so it is absent on both sides. The range
    /// and trivia are elided.
    DotLambda {
        expr: Box<NormalisedExpr>,
    },
    /// `SynExpr.DotIndexedGet` — a dotted indexer read `expr.[index]` (phase
    /// 10.16a). Mirrors FCS's `DotIndexedGet of objectExpr * indexArgs:
    /// SynExpr * dotRange * range`. `object` is the indexed expression;
    /// `index` is the (single) index expression — a [`NormalisedExpr::Tuple`]
    /// for the multi-arg `arr.[i, j]`. Both ranges are elided.
    DotIndexedGet {
        object: Box<NormalisedExpr>,
        index: Box<NormalisedExpr>,
    },
    /// `SynExpr.IndexRange` — a range / slice `lower..upper` (phase 10.22).
    /// Mirrors FCS's `IndexRange of expr1: SynExpr option * opm * expr2:
    /// SynExpr option * range1 * range2 * range`. Either bound may be `None`
    /// (`2..` is `lower = Some`, `upper = None`; `..3` the reverse); all
    /// ranges are elided.
    IndexRange {
        lower: Option<Box<NormalisedExpr>>,
        upper: Option<Box<NormalisedExpr>>,
    },
    /// `SynExpr.IndexFromEnd` — a from-end index/slice bound `^expr` inside an
    /// indexer (`arr.[^1]`, `arr.[^3..]`, phase 10.22b). Mirrors FCS's
    /// `IndexFromEnd of expr: SynExpr * range`; the range is elided.
    IndexFromEnd {
        expr: Box<NormalisedExpr>,
    },
    /// `SynExpr.AddressOf` — the `&e` / `&&e` prefix forms. Mirrors FCS's
    /// `AddressOf of isByref: bool * expr: SynExpr * opRange: range *
    /// range: range`. `is_byref` is `true` for managed byref (`&`),
    /// `false` for the unmanaged-nativeptr form (`&&`). Both ranges are
    /// elided.
    AddressOf {
        is_byref: bool,
        expr: Box<NormalisedExpr>,
    },
    /// `SynExpr.New` — an object-construction expression `new T(args)`.
    /// Mirrors FCS's `New of isProtected: bool * targetType: SynType *
    /// expr: SynExpr * range: range`. The expression form always carries
    /// `is_protected = false` (the `true` case is `inherit`-style base
    /// construction, which has no expression surface); the field is kept so
    /// the diff would catch any surprise. The range is elided.
    New {
        is_protected: bool,
        ty: NormalisedType,
        arg: Box<NormalisedExpr>,
    },
    /// `SynExpr.ObjExpr` — an object expression `{ new T(args) with member … }`.
    /// Mirrors FCS's `ObjExpr of objType: SynType * argOptions: (SynExpr *
    /// Ident option) option * withKeyword * bindings: SynBinding list *
    /// members: SynMemberDefns * extraImpls: SynInterfaceImpl list * … `.
    ///
    /// Projects: `ty` (`objType`), the optional constructor argument `arg` (the
    /// expression half of `argOptions` — `None` for the bare `new T with …`
    /// form, `Some` for `new T(args) with …`), the value `bindings` (FCS's
    /// `bindings`, the `with X = e [and …]` form — each a [`NormalisedBinding`]
    /// with `SynLeadingKeyword.Synthetic` on the head and `And` on subsequent
    /// `and`-chained ones), the `with member …` `members`, and `extra_impls`
    /// (FCS's `extraImpls`, the trailing `interface I with member …` clauses —
    /// each a [`NormalisedMember::Interface`]). `bindings` and `members` are
    /// mutually exclusive in a single `with` block (FCS's `objExprBindings` is
    /// one or the other). The `Ident option` base name and the
    /// `withKeyword`/`newExprRange`/`range` are elided.
    ObjExpr {
        ty: NormalisedType,
        arg: Option<Box<NormalisedExpr>>,
        bindings: Vec<NormalisedBinding>,
        members: Vec<NormalisedMember>,
        extra_impls: Vec<NormalisedMember>,
    },
    /// `SynExpr.InferredUpcast` — the `upcast e` prefix coercion. Mirrors
    /// FCS's `InferredUpcast of expr: SynExpr * range`. Unlike the `:>` infix
    /// coercion, the inferred form carries no target type (it is supplied by
    /// inference), so only the wrapped expr is projected; the range is elided.
    InferredUpcast {
        expr: Box<NormalisedExpr>,
    },
    /// `SynExpr.InferredDowncast` — the `downcast e` prefix coercion. Mirrors
    /// FCS's `InferredDowncast of expr: SynExpr * range`; the typeless sibling
    /// of the `:?>` infix downcast. The range is elided.
    InferredDowncast {
        expr: Box<NormalisedExpr>,
    },
    /// `SynExpr.Lazy` — the `lazy e` delayed-computation prefix. Mirrors FCS's
    /// `Lazy of expr: SynExpr * range`; only the delayed expr is projected, the
    /// range elided.
    Lazy {
        expr: Box<NormalisedExpr>,
    },
    /// `SynExpr.Assert` — the `assert e` runtime-assertion prefix. Mirrors FCS's
    /// `Assert of expr: SynExpr * range`; only the asserted expr is projected,
    /// the range elided.
    Assert {
        expr: Box<NormalisedExpr>,
    },
    /// `SynExpr.Fixed` — the `fixed e` pinning prefix. Mirrors FCS's
    /// `Fixed of expr: SynExpr * range`; only the pinned expr is projected, the
    /// range elided. (The pinned expr is a full `declExpr`, so it can be any
    /// `NormalisedExpr` — tuple, infix App, cast, control-flow, …)
    Fixed {
        expr: Box<NormalisedExpr>,
    },
    /// `SynExpr.TypeApp` — expression-level generic type application `f<int>`
    /// (phase 10.20). Mirrors FCS's `TypeApp of expr: SynExpr *
    /// lessRange: range * typeArgs: SynType list * commaRanges: range list *
    /// greaterRange: range option * typeArgsRange: range * range`. `expr` is
    /// the boxed type-applied head; `type_args` is the `< … >` block. The four
    /// ranges, the comma list, and the `greaterRange` option are elided.
    TypeApp {
        expr: Box<NormalisedExpr>,
        type_args: Vec<NormalisedType>,
    },
    /// `SynExpr.Typed of expr * targetType * range` — `(e : T)` type
    /// annotation surface from phase 7.1. The range is elided like every
    /// other normalised node.
    Typed {
        expr: Box<NormalisedExpr>,
        ty: NormalisedType,
    },
    /// `SynExpr.TypeTest of expr * targetType * range` — the `e :? T` dynamic
    /// type-test operator. Same `(expr, targetType)` payload as [`Self::Typed`];
    /// the range is elided.
    TypeTest {
        expr: Box<NormalisedExpr>,
        ty: NormalisedType,
    },
    /// `SynExpr.Upcast of expr * targetType * range` — the `e :> T` upcast
    /// operator. The range is elided.
    Upcast {
        expr: Box<NormalisedExpr>,
        ty: NormalisedType,
    },
    /// `SynExpr.Downcast of expr * targetType * range` — the `e :?> T` dynamic
    /// downcast operator. The range is elided.
    Downcast {
        expr: Box<NormalisedExpr>,
        ty: NormalisedType,
    },
    /// `SynExpr.IfThenElse` — `if c then e1 [else e2]`. Mirrors FCS's
    /// `IfThenElse of ifExpr * thenExpr * elseExpr option *
    ///                 spIfToThen * isFromErrorRecovery * range * trivia`
    /// (`SyntaxTree.fsi:790`). `else_branch` is `None` for the no-else
    /// form (phase 5.2) and `Some _` for the three-part shape (phase
    /// 5.1); the trivia/range/debug-point/error-recovery slots are
    /// elided.
    IfThenElse {
        condition: Box<NormalisedExpr>,
        then_branch: Box<NormalisedExpr>,
        else_branch: Option<Box<NormalisedExpr>>,
    },
    /// `SynExpr.Sequential` — flattened to an n-ary list to match how
    /// FCS's right-leaning `Sequential(_, _, e1, Sequential(_, _, e2,
    /// e3, …))` projects. The debug-point flag, `isTrueSeq`, range,
    /// and trivia are elided.
    Sequential(Vec<NormalisedExpr>),
    /// `SynExpr.InterpolatedString of contents *
    ///                                 synStringKind: SynStringKind *
    ///                                 range` (`SyntaxTree.fsi:970`).
    /// `parts` mirrors `SynInterpolatedStringPart list` in source order:
    /// `String` parts carry the decoded literal text, `FillExpr` parts
    /// carry the parsed inner expression plus its `Ident option` format
    /// qualifier (`{x:N2}` → `Some("N2")`). Only ranges are elided.
    InterpolatedString {
        parts: Vec<NormalisedInterpPart>,
        kind: SynStringKind,
    },
    /// `SynExpr.Lambda of fromMethod * inLambdaSeq * args: SynSimplePats *
    ///                    body: SynExpr * parsedData * range: range *
    ///                    trivia` (`SyntaxTree.fsi:825`). FCS encodes
    /// the curried form as nested `Lambda`s with one
    /// `SynSimplePats.SimplePats([p], _)` per arrow level plus a
    /// `parsedData = Some(args, body)` cache on the outermost node
    /// that lists the parsed argument patterns flat alongside the real
    /// body. We project the flat shape (the `parsedData` view): `args`
    /// is the parameter-pattern list in source order, `body` is the
    /// real body expression (after walking past the curried
    /// scaffolding). `fromMethod` is always `false` for `fun`-lambdas
    /// (it's `true` only for compiler-generated method-shape rewrites),
    /// `inLambdaSeq` is an internal computation-expression flag,
    /// trivia/ranges are elided.
    Lambda {
        args: Vec<NormalisedPat>,
        body: Box<NormalisedExpr>,
    },
    /// `SynExpr.Match of spBind * expr: SynExpr * clauses: SynMatchClause
    ///                   list * range * trivia` (`SyntaxTree.fsi:847`).
    /// We don't parse a surface `match` expression yet; this variant
    /// exists because FCS *synthesises* `Match` scaffolding when it lowers
    /// a non-simple `fun` parameter (a const, `null`, ctor-app, …) into a
    /// compiler-generated `_argN` simple pat plus
    /// `match _argN with <pat> -> body` (`SyntaxTreeOps.fs:357-369`). The
    /// debug-point, range, and trivia slots are elided.
    Match {
        scrutinee: Box<NormalisedExpr>,
        clauses: Vec<NormalisedMatchClause>,
    },
    /// `SynExpr.MatchBang of matchDebugPoint: DebugPointAtBinding *
    ///                       expr: SynExpr * clauses: SynMatchClause list *
    ///                       range * trivia` (`SyntaxTree.fsi:916`) — the
    /// computation-expression `match! e with …` binder. Field-for-field
    /// identical to [`NormalisedExpr::Match`], but kept a *distinct* variant
    /// so a `match!` never normalises equal to a `match` (FCS reports them as
    /// different `SynExpr` cases). The debug-point, range, and trivia slots
    /// are elided.
    MatchBang {
        scrutinee: Box<NormalisedExpr>,
        clauses: Vec<NormalisedMatchClause>,
    },
    /// `SynExpr.While of whileDebugPoint: DebugPointAtWhile * whileExpr: SynExpr
    ///                  * doExpr: SynExpr * range` (`SyntaxTree.fsi:656`) — a
    /// `while cond do body` loop. `cond` is the `whileExpr`, `body` the
    /// `doExpr`; the debug-point and range slots are elided.
    While {
        cond: Box<NormalisedExpr>,
        body: Box<NormalisedExpr>,
    },
    /// `SynExpr.WhileBang of whileDebugPoint: DebugPointAtWhile *
    ///                      whileExpr: SynExpr * doExpr: SynExpr * range`
    /// (`SyntaxTree.fsi:928`) — the computation-expression `while! cond do body`
    /// binder. Field-for-field identical to [`NormalisedExpr::While`], but kept
    /// a *distinct* variant so a `while!` never normalises equal to a `while`.
    WhileBang {
        cond: Box<NormalisedExpr>,
        body: Box<NormalisedExpr>,
    },
    /// `SynExpr.ForEach of forDebugPoint: DebugPointAtFor * inDebugPoint:
    ///                    DebugPointAtInOrTo * seqExprOnly: SeqExprOnly *
    ///                    isFromSource: bool * pat: SynPat * enumExpr: SynExpr *
    ///                    bodyExpr: SynExpr * range` (`SyntaxTree.fsi:671`) — a
    /// `for pat in enumExpr do body` loop. `pat` is the binder, `enum_expr` the
    /// enumerable collection, `body` the loop body; the two debug points,
    /// `seqExprOnly`, `isFromSource`, and range are elided.
    ForEach {
        pat: NormalisedPat,
        enum_expr: Box<NormalisedExpr>,
        body: Box<NormalisedExpr>,
    },
    /// `SynExpr.For of forDebugPoint: DebugPointAtFor * toDebugPoint:
    ///                DebugPointAtInOrTo * ident: Ident * equalsRange: range
    ///                option * identBody: SynExpr * direction: bool * toBody:
    ///                SynExpr * doBody: SynExpr * range` (`SyntaxTree.fsi:659`) —
    /// a `for ident = from to/downto to do body` range loop. `ident` is the loop
    /// variable (`idText`), `from`/`to` the start/end bounds, `ascending` the
    /// direction (`to` = `true`, `downto` = `false`), `body` the loop body. The
    /// two debug points, `equalsRange`, and range are elided.
    For {
        ident: String,
        from: Box<NormalisedExpr>,
        ascending: bool,
        to: Box<NormalisedExpr>,
        body: Box<NormalisedExpr>,
    },
    /// `SynExpr.TryWith of tryExpr: SynExpr * withCases: SynMatchClause list *
    ///                    range * tryDebugPoint: DebugPointAtTry *
    ///                    withDebugPoint: DebugPointAtWith * trivia`
    /// (`SyntaxTree.fsi:759`) — a `try body with <clauses>` exception handler.
    /// `body` is the protected `tryExpr`; `clauses` are the `withCases`,
    /// reusing [`NormalisedMatchClause`] verbatim (FCS's `withClauses` is the
    /// same `patternClauses` non-terminal as `match … with`). The two debug
    /// points, range, and trivia are elided.
    TryWith {
        body: Box<NormalisedExpr>,
        clauses: Vec<NormalisedMatchClause>,
    },
    /// `SynExpr.TryFinally of tryExpr: SynExpr * finallyExpr: SynExpr * range *
    ///                       tryDebugPoint: DebugPointAtTry * finallyDebugPoint:
    ///                       DebugPointAtFinally * trivia` (`SyntaxTree.fsi:768`)
    /// — a `try body finally cleanup` expression. `body` is the protected
    /// `tryExpr`, `finally` the `finallyExpr` cleanup; the two debug points,
    /// range, and trivia (`TryKeyword` / `FinallyKeyword`) are elided. Kept a
    /// *distinct* variant from [`NormalisedExpr::TryWith`] so a `try … finally`
    /// never normalises equal to a `try … with` (FCS reports them as different
    /// `SynExpr` cases).
    TryFinally {
        body: Box<NormalisedExpr>,
        finally: Box<NormalisedExpr>,
    },
    /// `SynExpr.Quote of operator: SynExpr * isRaw: bool *
    ///                  quotedExpr: SynExpr * isFromQueryExpression: bool *
    ///                  range` (`SyntaxTree.fsi:603`). `is_raw` distinguishes
    /// the untyped `<@@ … @@>` form (`true`) from the typed `<@ … @>`
    /// (`false`); `inner` is the quoted expression. The synthetic
    /// `operator` ident (`op_Quotation`/`op_QuotationRaw`) and the
    /// always-`false` `isFromQueryExpression` carry no syntactic
    /// information and are elided.
    Quote {
        is_raw: bool,
        inner: Box<NormalisedExpr>,
    },
    /// `SynExpr.ComputationExpr of hasSeqBuilder: bool * expr: SynExpr *
    ///                            range` (`SyntaxTree.fsi:702`) — the body
    /// of a computation-expression brace `{ … }`. `hasSeqBuilder` is always
    /// `false` at parse and is elided; the boxed expr is the brace body.
    /// The builder application `seq { … }` projects as
    /// `App { func: Ident "seq", arg: ComputationExpr(…) }`.
    ComputationExpr(Box<NormalisedExpr>),
    /// `SynExpr.Record(baseInfo, copyInfo, recordFields, range)`
    /// (`SyntaxTree.fsi:634`) — a record expression. `copy` is the
    /// copy-and-update source (`{ src with … }`), `None` for a plain field
    /// list. `inherit_info` is FCS's `baseInfo` (`{ inherit Base(args); … }`) —
    /// the base type and the constructor-args expression (FCS synthesises
    /// `Const(Unit)` for a bare `inherit Base` / `inherit Base()`); `None` for a
    /// non-inherit record. The per-field equals/separator/range trivia is elided.
    Record {
        inherit_info: Option<(NormalisedType, Box<NormalisedExpr>)>,
        copy: Option<Box<NormalisedExpr>>,
        fields: Vec<NormalisedRecordField>,
    },
    /// `SynExpr.AnonRecd(isStruct, copyInfo, recordFields, range, trivia)`
    /// (`SyntaxTree.fsi:620`) — an anonymous-record expression `{| F = e; … |}`.
    /// `is_struct` is FCS's `isStruct` (`struct {| … |}`; always `false` in
    /// this slice — the struct form is deferred). `copy` is the copy-and-update
    /// source (`{| src with … |}`), `None` for a plain field list. `fields`
    /// reuses [`NormalisedRecordField`] (name segments + value); FCS's
    /// per-field `equalsRange` and the trivia/range are elided. FCS's anon-recd
    /// field value is mandatory, so each `value` is `Some`.
    AnonRecd {
        is_struct: bool,
        copy: Option<Box<NormalisedExpr>>,
        fields: Vec<NormalisedRecordField>,
    },
    /// `SynExpr.ArrayOrList(isArray, exprs: SynExpr list, range)`
    /// (`SyntaxTree.fsi:628`) — an *empty* list `[]` (`is_array=false`) or
    /// array `[||]` (`is_array=true`). The parser productions only ever emit
    /// this variant for the empty body (a non-empty `[ … ]` is
    /// [`ArrayOrListComputed`](NormalisedExpr::ArrayOrListComputed)), so
    /// `elements` is empty in practice; it still decodes FCS's `exprs` list
    /// faithfully.
    ArrayOrList {
        is_array: bool,
        elements: Vec<NormalisedExpr>,
    },
    /// `SynExpr.ArrayOrListComputed(isArray, expr: SynExpr, range)`
    /// (`SyntaxTree.fsi:682`) — a non-empty list `[ e ]` / `[ e1; e2; … ]`
    /// (`is_array=false`) or array `[| … |]` (`is_array=true`). `inner` is
    /// FCS's single `sequentialExpr` body: a [`Sequential`] for two-or-more
    /// `;`/offside-separated elements, the bare element otherwise. The `;`
    /// separates elements; a `,` makes a one-element list of a tuple
    /// (`[a, b]` ⇒ `inner = Tuple[a, b]`).
    ///
    /// [`Sequential`]: NormalisedExpr::Sequential
    ArrayOrListComputed {
        is_array: bool,
        inner: Box<NormalisedExpr>,
    },
    /// `SynExpr.YieldOrReturn(flags, expr, …)` (`SyntaxTree.fsi:899`) and
    /// `SynExpr.YieldOrReturnFrom(flags, expr, …)` (`:904`). `from`
    /// distinguishes the `!` variant; `flags` is FCS's `(bool, bool)` pair
    /// (`yield`/`yield!` ⇒ `(true, false)`, `return`/`return!` ⇒
    /// `(false, true)`). Trivia/range are elided.
    YieldOrReturn {
        flags: (bool, bool),
        from: bool,
        inner: Box<NormalisedExpr>,
    },
    /// `SynExpr.DoBang of expr: SynExpr * range * trivia`
    /// (`SyntaxTree.fsi:925`) — a `do! e` in a computation expression. The
    /// boxed expr is the bound expression; range/trivia are elided.
    DoBang(Box<NormalisedExpr>),
    /// `SynExpr.JoinIn of lhsExpr: SynExpr * lhsRange: range * rhsExpr: SynExpr
    /// * range` (`SyntaxTree.fsi:883`) — the query computation-expression join
    /// operator `lhs in rhs` (`join x in xs on (a = b)` parses as
    /// `JoinIn(App(join, x), App(App(xs, on), Paren(a = b)))`). `lhs`/`rhs` are
    /// the two operands; the `lhsRange` and overall range are elided.
    JoinIn {
        lhs: Box<NormalisedExpr>,
        rhs: Box<NormalisedExpr>,
    },
    /// `SynExpr.Do of expr: SynExpr * range` (`SyntaxTree.fsi:884`) — a `do e`
    /// statement (module-level via `SynModuleDecl.Expr`, or a sequence/CE-body
    /// element). The boxed expr is the bound expression; range is elided.
    Do(Box<NormalisedExpr>),
    /// `SynExpr.LetOrUse of SynLetOrUse` (`SyntaxTree.fsi:913`,
    /// `SynLetOrUse` at `:564`) — an expression-level `let`/`use` binding
    /// with a body. Two source forms project here: the computation-expression
    /// bang binders `let!`/`use!`/`and!` (phase 10.4b — never recursive, each
    /// binding's [`NormalisedBinding::leading_keyword`] one of
    /// `LetBang`/`UseBang`/`AndBang`), and the plain offside block `let`/`use`
    /// (`parser-expr-block-let` — `is_rec` follows `let rec`, keywords
    /// `Let`/`LetRec`/`Use`/`UseRec`/`And`). `and`/`and!` followers are extra
    /// `bindings` entries (FCS keeps them in one `SynLetOrUse`). `body` is
    /// `SynLetOrUse.Body`. `IsFromSource` and ranges/trivia are elided.
    LetOrUse {
        is_rec: bool,
        bindings: Vec<NormalisedBinding>,
        body: Box<NormalisedExpr>,
    },
    /// `SynExpr.MatchLambda of isExnMatch: bool * keywordRange: range *
    ///                         matchClauses: SynMatchClause list *
    ///                         matchDebugPoint * range` (`SyntaxTree.fsi`).
    /// The `function pat -> e | …` sugar. FCS keeps this as a *distinct*
    /// parsed node rather than desugaring it to `fun _argN -> match _argN
    /// with …` (that synthesis is a later, typecheck-time step), so we
    /// mirror it: there is no scrutinee, only the clause list. The
    /// `isExnMatch`, `keywordRange`, debug-point, and range slots are
    /// elided.
    MatchLambda {
        clauses: Vec<NormalisedMatchClause>,
    },
    /// `SynExpr.LongIdentSet of longDotId: SynLongIdent * expr: SynExpr *
    /// range` (`SyntaxTree.fsi:819`) — `x <- e` / `a.b.c <- e`, the
    /// `LongOrSingleIdent` arm of `mkSynAssign` (`SyntaxTreeOps.fs:523`).
    /// `long_dot_id` is the target path's `Ident.idText` segments (backticks
    /// stripped, like [`NormalisedExpr::LongIdent`]); `value` is the RHS. The
    /// `SynLongIdent` dot-ranges/trivia and the outer range are elided.
    LongIdentSet {
        long_dot_id: Vec<String>,
        value: Box<NormalisedExpr>,
    },
    /// `SynExpr.Set of targetExpr: SynExpr * rhsExpr: SynExpr * range`
    /// (`SyntaxTree.fsi:832`) — the `mkSynAssign` fallback (`SyntaxTreeOps.fs:531`)
    /// for any `<-` whose LHS is neither an identifier path nor a recognised
    /// indexed/property target (`(x) <- e`, `f x <- e`). `target` is the LHS
    /// expression, `value` the RHS; the range is elided.
    Set {
        target: Box<NormalisedExpr>,
        value: Box<NormalisedExpr>,
    },
    /// `SynExpr.NamedIndexedPropertySet of longDotId: SynLongIdent *
    /// expr1: SynExpr * expr2: SynExpr * range` (`SyntaxTree.fsi:847`) —
    /// `Type.Items(e1) <- e2`, the `mkSynAssign` arm for an application whose
    /// function is a `LongIdent` (`SyntaxTreeOps.fs:529`). `long_dot_id` is the
    /// function path's segments, `expr1` the index argument, `expr2` the
    /// assigned value. A *single-ident* function is `SynExpr.Ident`, not
    /// `LongIdent`, and so produces [`NormalisedExpr::Set`] instead. The range
    /// is elided.
    NamedIndexedPropertySet {
        long_dot_id: Vec<String>,
        expr1: Box<NormalisedExpr>,
        expr2: Box<NormalisedExpr>,
    },
    /// `SynExpr.DotIndexedSet of objectExpr: SynExpr * indexArgs: SynExpr *
    /// valueExpr: SynExpr * leftOfSetRange * dotRange * range`
    /// (`SyntaxTree.fsi:838`) — `arr.[i] <- v`, the `mkSynAssign` arm for a
    /// `DotIndexedGet` LHS (`SyntaxTreeOps.fs:526`). Reachable since phase
    /// 10.16a added `arr.[i]` parsing. `object` is the indexed expression,
    /// `index` the index args (a [`NormalisedExpr::Tuple`] for `arr.[i, j]`),
    /// `value` the RHS. All ranges are elided.
    DotIndexedSet {
        object: Box<NormalisedExpr>,
        index: Box<NormalisedExpr>,
        value: Box<NormalisedExpr>,
    },
    /// `SynExpr.DotSet of targetExpr: SynExpr * longDotId: SynLongIdent *
    /// rhsExpr: SynExpr * range` (`SyntaxTree.fsi:828`) — `expr.Member <- v`,
    /// the `mkSynAssign` arm for a `DotGet` LHS (`SyntaxTreeOps.fs:525`).
    /// Reachable since phase 10.16a added postfix `expr.Member` parsing on a
    /// non-ident head (a pure ident chain `a.b <- v` is `LongIdentSet`).
    /// `expr` is the LHS object, `long_dot_id` the member path's segments,
    /// `value` the RHS. The range is elided.
    DotSet {
        expr: Box<NormalisedExpr>,
        long_dot_id: Vec<String>,
        value: Box<NormalisedExpr>,
    },
    /// `SynExpr.DotNamedIndexedPropertySet of targetExpr: SynExpr *
    /// longDotId: SynLongIdent * argExpr: SynExpr * rhsExpr: SynExpr * range`
    /// (`SyntaxTree.fsi:850`) — `expr.Member(i) <- v`, the `mkSynAssign` arm
    /// for an application whose function is a `DotGet` (`SyntaxTreeOps.fs:530`).
    /// Reachable since phase 10.16a: `(obj).P(i) <- v` builds
    /// `App(DotGet(Paren obj, ["P"]), i)`. A `LongIdent`-function application
    /// (`obj.P(i) <- v`, ident receiver) is `NamedIndexedPropertySet` instead.
    /// `target` is the receiver object, `long_dot_id` the member path's
    /// segments, `expr1` the index argument, `expr2` the assigned value.
    DotNamedIndexedPropertySet {
        target: Box<NormalisedExpr>,
        long_dot_id: Vec<String>,
        expr1: Box<NormalisedExpr>,
        expr2: Box<NormalisedExpr>,
    },
    /// `SynExpr.TraitCall(supportTys: SynType, traitSig: SynMemberSig,
    /// argExpr: SynExpr, range)` (`SyntaxTree.fsi`) — an SRTP trait call
    /// `( ^a : (static member M : ^a -> int) x )`. `support` is the support
    /// *type* list (FCS field 0): one head typar for `^a` (a `SynType.Var`), or
    /// several for the parenthesised alternatives `((^a or ^b) : …)` (a
    /// `SynType.Paren(SynType.Or(…))`, flattened to its alternatives — the same
    /// `fcs_support_types` projection the SRTP member *constraint*
    /// [`NormalisedTypeConstraint::SupportsMember`] uses). Unlike the constraint
    /// form, FCS rejects a *concrete* alternative in a trait-call expression, so
    /// in practice every alternative is a typar ([`NormalisedType::Var`]) — but
    /// the field is `NormalisedType` so the two share one projection. `member`
    /// reuses the shared [`NormalisedMember`] projection (the same
    /// `classMemberSpfn` payload). `arg` is the argument expression. The range is
    /// elided; FCS wraps the whole node in `SynExpr.Paren`, projected as
    /// [`NormalisedExpr::Paren`].
    TraitCall {
        support: Vec<NormalisedType>,
        member: Box<NormalisedMember>,
        arg: Box<NormalisedExpr>,
    },
    /// `SynExpr.LibraryOnlyStaticOptimization(constraints, expr, optimizedExpr,
    /// range)` — FSharp.Core's static-optimization binding RHS
    /// (`mainExpr when 'T : ty = branch …`). FCS folds the surface clauses *right*
    /// into a nest of these (`SyntaxTreeOps.mkSynBindingRhs`), and this mirrors
    /// that nesting: `constraints` is the (`and`-chained) condition list of the
    /// *outermost* clause **in source order** (FCS's grammar stores them reversed;
    /// both projectors normalise to source order), `expr` its branch, and
    /// `optimized_expr` the rest — the
    /// next nested `StaticOptimization`, bottoming out at the fallthrough main
    /// expression. The range is elided. The CST projector
    /// ([`super::from_cst`]) builds the same nesting by folding its flat
    /// `STATIC_OPTIMIZATION_EXPR` clauses in reverse.
    StaticOptimization {
        constraints: Vec<NormalisedStaticOptConstraint>,
        expr: Box<NormalisedExpr>,
        optimized_expr: Box<NormalisedExpr>,
    },
    /// `SynExpr.LibraryOnlyUnionCaseFieldGet(expr, longId, fieldNum, range)` —
    /// FSharp.Core's cons-cell field read `expr.( :: ).<int>`. `expr` is the
    /// object; `field_num` the field index. The `longId` is always the cons
    /// operator (`["op_ColonColon"]`, hardcoded by the grammar), so it is elided.
    LibraryOnlyUnionCaseFieldGet {
        expr: Box<NormalisedExpr>,
        field_num: i32,
    },
    /// `SynExpr.LibraryOnlyUnionCaseFieldSet(expr, longId, fieldNum, rhsExpr,
    /// range)` — the set form `expr.( :: ).<int> <- rhs`. FCS's `mkSynAssign`
    /// collapses an assignment over a `LibraryOnlyUnionCaseFieldGet` into this;
    /// the CST projector ([`super::from_cst::normalise_assign`]) reproduces that.
    /// The `longId` is elided as above.
    LibraryOnlyUnionCaseFieldSet {
        expr: Box<NormalisedExpr>,
        field_num: i32,
        value: Box<NormalisedExpr>,
    },
    /// An error-recovery placeholder for a sub-expression that failed to
    /// parse — FCS's `SynExpr.ArbitraryAfterError(debugStr, range)`, the marker
    /// it substitutes for a missing/unparseable expression while recovering
    /// (e.g. the RHS of `let x =` with nothing after the `=`). Our parser
    /// represents the same hole as an absent `Expr` child under a recovered
    /// node (a zero-width `ERROR`), which the projector maps here too. The
    /// `debugStr` and range are elided, so this is a shape-only "the expression
    /// here didn't parse" marker — the diff currency for Phase 11 recovery.
    Error,
}

/// One `SynStaticOptimizationConstraint` (`SyntaxTree.fsi:1048`) — a single
/// condition of a [`NormalisedExpr::StaticOptimization`] clause. The subject
/// typar reuses [`NormalisedTypar`]; ranges are elided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedStaticOptConstraint {
    /// `WhenTyparTyconEqualsTycon(typar, rhsType, _)` — `'T : ty`.
    WhenTyparTyconEqualsTycon {
        typar: NormalisedTypar,
        rhs_type: NormalisedType,
    },
    /// `WhenTyparIsStruct(typar, _)` — the bare `'T struct`.
    WhenTyparIsStruct { typar: NormalisedTypar },
}

/// One `SynMatchClause of pat: SynPat * whenExpr: SynExpr option *
/// resultExpr: SynExpr * range * spTarget: DebugPointAtTarget *
/// trivia: SynMatchClauseTrivia` (`SyntaxTree.fsi:1063`). The
/// `fun`-parameter lowering only ever emits the no-guard form, so `when`
/// is always `None` for the synthetic clauses we currently produce, but
/// the field is modelled so a real `match`/`when` projection can reuse
/// this shape later. Range, debug-point, and trivia slots are elided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedMatchClause {
    pub pat: NormalisedPat,
    pub when: Option<Box<NormalisedExpr>>,
    pub result: Box<NormalisedExpr>,
}

/// One `SynExprRecordField(fieldName, equalsRange, expr, range,
/// blockSeparator)` of a [`NormalisedExpr::Record`] (`SyntaxTree.fsi:991`).
/// `name` is the field's `SynLongIdent` segments; `value` is the bound
/// expression (`None` only for an error-recovery field with no `= e`). The
/// equals/separator/range trivia and the `RecordFieldName` trailing-dot bool
/// are elided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedRecordField {
    pub name: Vec<String>,
    pub value: Option<Box<NormalisedExpr>>,
}

/// One member of an interpolated string body. Mirrors FCS's
/// `SynInterpolatedStringPart` (`SyntaxTree.fsi:1000`) one-for-one, only
/// eliding ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedInterpPart {
    /// `SynInterpolatedStringPart.String of value: string * range: range`
    /// — a literal-text part. The range is elided; the value is the raw UTF-16
    /// code-unit payload.
    String(Vec<u16>),
    /// `SynInterpolatedStringPart.FillExpr of fillExpr: SynExpr *
    ///                                        qualifiers: Ident option`
    /// — a fill expression. `expr` is the spliced expression; `qualifier` is
    /// the trailing `: ident` format specifier (FCS grammar `declExpr COLON
    /// ident`, e.g. `{x:N2}` → `Some("N2")`), or `None` for a bare `{x}`. The
    /// qualifier's range is elided; its `idText` is kept.
    FillExpr {
        expr: NormalisedExpr,
        qualifier: Option<String>,
    },
}

/// `SynType` — phases 7.1–7.10 model the atomic shapes, type variables,
/// function arrows, tuple types, prefix/postfix type-applications,
/// array suffixes, hash constraints, anon-record types, and dotted-path
/// applications on non-`path` roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedType {
    /// `SynType.LongIdent(SynLongIdent)` — `int`, `A.B.C`. The
    /// `Vec<String>` is the path segments (backticks stripped), mirroring
    /// the LongIdent projection used for `SynExpr.LongIdent`.
    LongIdent(Vec<String>),
    /// `SynType.Anon(range)` — the wildcard type `_`. The range is
    /// elided, leaving a payload-less variant.
    Anon,
    /// `SynType.Paren(innerType, range)` — `( T )`. The boxed value is
    /// the wrapped inner type; the range is elided.
    Paren(Box<NormalisedType>),
    /// `SynType.Var(SynTypar, range)` — a type variable. `name` is the
    /// `SynTypar.ident.idText` (backticks stripped if any) and
    /// `head_type` is `true` iff `SynTypar.staticReq = TyparStaticReq.HeadType`
    /// (`^T`); `false` is the plain `'a` form.
    Var { name: String, head_type: bool },
    /// `SynType.Fun(argType, returnType, range, trivia)` — a function
    /// arrow `T -> U`. The trivia carries the arrow range, which we
    /// elide. Right-associative on the FCS side (`tupleType RARROW typ`
    /// in `pars.fsy`), so `int -> int -> int` projects as
    /// `Fun(int, Fun(int, int))`.
    Fun {
        arg: Box<NormalisedType>,
        ret: Box<NormalisedType>,
    },
    /// `SynType.Tuple(isStruct, path, range)` — a tuple type
    /// `T * U * V`. The path mirrors FCS's `SynTupleTypeSegment` list
    /// (`SyntaxTree.fsi:459`) one-for-one and is *flat*, not nested
    /// pairs. Phase 7.4 only produces `is_struct = false` and
    /// `Type`/`Star` segments; `Slash` and `struct (T * U)` arrive
    /// later.
    Tuple {
        is_struct: bool,
        path: Vec<NormalisedTupleSegment>,
    },
    /// `SynType.App(typeName, lessRange, typeArgs, commaRanges,
    /// greaterRange, isPostfix, range)` — a type-constructor
    /// application (`SyntaxTree.fsi:472`). Ranges and the comma list
    /// are elided; the structural payload is `type_name`, `type_args`,
    /// and `is_postfix`. Postfix `int list` projects as
    /// `App { type_name: list, type_args: [int], is_postfix: true }`;
    /// left-associative on the FCS side (`appType appTypeConPower` in
    /// `pars.fsy:6378`), so `int list option` projects as
    /// `App(option, [App(list, [int])])`.
    App {
        type_name: Box<NormalisedType>,
        type_args: Vec<NormalisedType>,
        is_postfix: bool,
    },
    /// `SynType.Array(rank, elementType, range)` — an array-type
    /// suffix (`SyntaxTree.fsi:475`). `rank` is the dimensionality (`1`
    /// for `int[]`, `2` for `int[,]`, …); `element_type` is the boxed
    /// element shape. The range is elided. Left-associative on the
    /// FCS side (`appTypeWithoutNull arrayTypeSuffix`), so the jagged
    /// array `int[][]` projects as
    /// `Array { rank: 1, element_type: Array { rank: 1,
    /// element_type: int } }`.
    Array {
        rank: usize,
        element_type: Box<NormalisedType>,
    },
    /// `SynType.HashConstraint(innerType, range)` — a flexible-type
    /// constraint (`SyntaxTree.fsi:518`); the `#T` surface syntax
    /// (`pars.fsy:2609-2611`) means "any subtype of `T`". Atomic-level
    /// on the FCS side (`atomType: hashConstraint | …`,
    /// `pars.fsy:6534`), so the hash binds tighter than postfix-app:
    /// `#int list` projects as
    /// `App(list, [Hash(int)], is_postfix: true)`, not
    /// `Hash(App(list, [int]))`. The range is elided.
    Hash { inner: Box<NormalisedType> },
    /// `SynType.AnonRecd(isStruct, fields, range)` — an anonymous
    /// record type (`SyntaxTree.fsi:500`); `{| F : int; G : string |}`
    /// for `is_struct = false` and `struct {| F : int |}` for
    /// `is_struct = true`. `fields` mirrors FCS's
    /// `(Ident * SynType) list` one-for-one, in source order, with
    /// each `Ident.idText` (backticks stripped) carried as `String`.
    /// Sits at FCS's `atomTypeOrAnonRecdType` layer (one above strict
    /// `atomType`), so `{| F : int |} list` projects as
    /// `App(list, [AnonRecd { is_struct: false, fields: [(F, int)] }],
    /// is_postfix: true)`. The range is elided.
    AnonRecd {
        is_struct: bool,
        fields: Vec<(String, NormalisedType)>,
    },
    /// `SynType.LongIdentApp(typeName, longDotId, lessRange, typeArgs,
    /// commaRanges, greaterRange, range)` — a dotted-path application
    /// whose root is itself a non-`path` atomic type
    /// (`pars.fsy:6600-6605`, `atomType DOT path
    /// [typeArgsNoHpaDeprecated]`). `root` is the LHS atomic type,
    /// `path` is the post-dot ident sequence (backticks stripped), and
    /// `type_args` is the optional `<…>` block in source order (empty
    /// for the bare `root.path` shape). Ranges, the less / greater
    /// ranges, and the comma list are elided. Left-associative on the
    /// FCS side, so chains nest: `(int).Foo<string>.Bar` projects as
    /// `LongIdentApp { root: LongIdentApp { root: Paren(int), path:
    /// [Foo], type_args: [string] }, path: [Bar], type_args: [] }`.
    LongIdentApp {
        root: Box<NormalisedType>,
        path: Vec<String>,
        type_args: Vec<NormalisedType>,
    },
    /// `SynType.WithNull(innerType, ambivalent, range, trivia)` — a
    /// nullable reference type `T | null` (`SyntaxTree.fsi:536`),
    /// parsed via `appTypeCanBeNullable` (`pars.fsy:6357-6359`).
    /// `inner` is the boxed `appTypeWithoutNull` before the `|`. The
    /// `ambivalent` flag is always `false` at parse time (so it carries
    /// no syntactic information), and the bar / overall ranges are
    /// elided — consistent with how ranges and parse-invariant flags
    /// are dropped elsewhere. Sits between `tupleType` and
    /// `appTypeWithoutNull`, so the postfix `int list | null` projects
    /// as `WithNull(App(list, [int], is_postfix: true))` and the tuple
    /// `string | null * int` as
    /// `Tuple([WithNull(string), int])`.
    WithNull { inner: Box<NormalisedType> },
    /// `SynType.WithGlobalConstraints(typeName, constraints, range)` — a type
    /// carrying a trailing `when` constraint clause (`'T when 'T : struct`),
    /// from FCS's `typeWithTypeConstraints` grammar. `base` is the boxed base
    /// type; `constraints` reuses the existing [`NormalisedTypeConstraint`]
    /// projection (the same `when` payload a type-definition header carries).
    /// The range is elided.
    WithGlobalConstraints {
        base: Box<NormalisedType>,
        constraints: Vec<NormalisedTypeConstraint>,
    },
    /// `SynType.Intersection(typar option, types, range, trivia)` — a
    /// flexible-type constraint intersection `#A & #B` / `'T & #A`
    /// (`SyntaxTree.fsi:557`, phase 10.10), reachable from the typed-paren
    /// surface (`(x : #A & #B)`). `typar` is `Some` for the `typar AMP …` head
    /// form (`'T & …`) and `None` for the `hashConstraint AMP …` form (`#A & …`,
    /// where the leading `#A` is the first `types` element). A non-`#` operand
    /// is an FCS "non-flexible type" error but still parses into `types`. Ranges
    /// and the `AmpersandRanges` trivia are elided.
    Intersection {
        typar: Option<NormalisedTypar>,
        types: Vec<NormalisedType>,
    },
    /// `SynType.MeasurePower(baseMeasure, exponent, range)` — a
    /// unit-of-measure power `m^2` (`SyntaxTree.fsi:521`, phase 10.8),
    /// reached as a type-argument actual (`(x : float<m^2>)`). `base` is
    /// the boxed base measure (a `LongIdent` unit, or a `Var` typar); the
    /// `exponent` is the [`NormalisedRationalConst`] power. The range is
    /// elided. A measure *product* (`kg m`) is just the existing postfix
    /// [`App`](NormalisedType::App), and the `/` measure division is a 10.9
    /// `Tuple`+`Slash`, so neither appears here.
    MeasurePower {
        base: Box<NormalisedType>,
        exponent: NormalisedRationalConst,
    },
    /// `SynType.StaticConstant(SynConst, range)` — a literal type-provider
    /// static argument (`SyntaxTree.fsi:525`, phase 10.9), e.g. the `42` in
    /// `(x : Foo<42>)`. Reuses the expression-side [`NormalisedConst`] since
    /// the held `SynConst` is identical to a `CONST_EXPR` literal. The range
    /// is elided.
    StaticConstant(NormalisedConst),
    /// `SynType.StaticConstantExpr(SynExpr, range)` — a `const`-expression
    /// static argument (`SyntaxTree.fsi:531`, phase 10.9), e.g. `const E` in
    /// `(x : Foo<const E>)`. The boxed inner is the atomic expression; the
    /// range is elided.
    StaticConstantExpr(Box<NormalisedExpr>),
    /// `SynType.StaticConstantNamed(ident: SynType, value: SynType, range)` —
    /// a named static argument (`SyntaxTree.fsi:534`, phase 10.9), e.g.
    /// `N=42` in `(x : Foo<N=42>)`. Both sides are full types (`value` is a
    /// `StaticConstant 42` when written as a literal, a `LongIdent int` when
    /// written as a type). The range is elided.
    StaticConstantNamed {
        ident: Box<NormalisedType>,
        value: Box<NormalisedType>,
    },
    /// `SynType.StaticConstantNull(range)` — a `null` static argument
    /// (`SyntaxTree.fsi:528`, phase 10.9), e.g. `(x : Foo<null>)`. The range
    /// is elided, leaving a payload-less variant.
    StaticConstantNull,
    /// `SynType.SignatureParameter(attributes, isOptional, id, usedType, range)`
    /// (`SyntaxTree.fsi`, phase 10.12b) — a labelled function-type parameter in a
    /// value / member / delegate signature (`x: int`, `?x: int`). `is_optional`
    /// is the leading `?`, `id` the parameter name (always `Some` for the
    /// named/optional forms parsed today; an unnamed-but-attributed param —
    /// deferred — would be `None`), and `used_type` the boxed value type after the
    /// `:`. `attributes` (deferred — always empty today) mirrors FCS's field 0.
    /// The range and the `SynArgInfo` companion are elided.
    SignatureParameter {
        attributes: Vec<Vec<NormalisedAttribute>>,
        is_optional: bool,
        id: Option<String>,
        used_type: Box<NormalisedType>,
    },
}

/// `SynRationalConst` (`SyntaxTree.fsi:221-235`, phase 10.8) — the exponent
/// of a [`NormalisedType::MeasurePower`]. One-for-one with FCS's cases; all
/// ranges are elided. `Integer`/`Rational` carry plain `i32` values
/// (FCS rejects out-of-`int32` magnitudes); `Negate` arises both from a `^-`
/// operator and from a standalone `-`, and `Paren` from a parenthesised
/// exponent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedRationalConst {
    Integer(i32),
    Rational { num: i32, denom: i32 },
    Negate(Box<NormalisedRationalConst>),
    Paren(Box<NormalisedRationalConst>),
}

/// One segment in a [`NormalisedType::Tuple`] path. One-for-one with
/// FCS's `SynTupleTypeSegment` cases (`SyntaxTree.fsi:459`); ranges on
/// the `Star`/`Slash` variants are elided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedTupleSegment {
    Type(NormalisedType),
    Star,
    Slash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedConst {
    Int32(i32),
    UInt32(u32),
    Int64(i64),
    UInt64(u64),
    SByte(i8),
    Byte(u8),
    Int16(i16),
    UInt16(u16),
    IntPtr(i64),
    UIntPtr(u64),
    /// `SynConst.Double` — compared by bit pattern via `f64::to_bits` so
    /// NaN-payload and signed-zero differences would show up as a diff
    /// failure (which is the correct behaviour — we want to know if FCS
    /// and we disagree). System.Text.Json round-trips finite normal
    /// doubles exactly via "shortest round-trippable" serialisation
    /// (since .NET Core 3.0), so `.as_f64()` is lossless on both sides.
    Double(u64),
    /// `SynConst.Single` — compared by `f32::to_bits`. System.Text.Json
    /// emits Singles using the shortest round-trippable text (G9), which
    /// means reading back as f64 and narrowing via `as f32` recovers the
    /// original bit pattern.
    Single(u32),
    /// `SynConst.Char` — raw UTF-16 code unit. FCS's `char` is a .NET
    /// `System.Char`, so recovery can carry lone surrogates that are not Unicode
    /// scalar values. Compare the code unit directly instead of routing through
    /// Rust `char` / JSON string conversion, which would replace lone surrogates
    /// with U+FFFD and hide differences.
    Char(u16),
    /// `SynConst.String` — decoded text plus the lexer-classified form
    /// (regular / verbatim / triple-quoted). FCS strings are .NET UTF-16
    /// strings, and recovery can carry lone surrogate code units. Compare the
    /// raw units directly instead of routing through Rust `String` / JSON string
    /// conversion, which would replace lone surrogates with U+FFFD and hide
    /// differences.
    String {
        value: Vec<u16>,
        kind: SynStringKind,
    },
    /// `SynConst.Bytes` — decoded byte content plus the FCS-classified
    /// form (regular / verbatim). FCS's `SynByteStringKind` has no
    /// triple-quote variant: `"""abc"""B` reports `Regular` even though
    /// its source form is triple-quoted (see `lex.fsl:135-136`). The
    /// our-side projector picks the decoder by `SyntaxKind` and then
    /// stamps the FCS-equivalent `kind` here.
    Bytes {
        value: Vec<u8>,
        kind: SynByteStringKind,
    },
    /// `SynConst.Decimal` — canonical text form of the value. The plan's
    /// "preserve trailing-zero scale" requirement makes value-equality
    /// (`1.0m == 1.00m`) unsuitable, and no Rust `Decimal` type with
    /// scale-preserving equality is in the dep tree. Both projectors
    /// canonicalise to the text that .NET's `decimal.ToString(InvariantCulture)`
    /// produces (`fcs-dump`'s `DecimalConverter` does the FCS side; our
    /// side runs [`canonicalise_decimal_source`]).
    Decimal(String),
    /// `SynConst.UserNum(value, suffix)` — numeric-literal-suffix form
    /// (`123I`, `42N`, `1_000G`). FCS's lex (`lex.fsl`:511-513) splits
    /// the token at the last character and strips `_` from the value;
    /// the suffix is the trailing alpha char.
    UserNum {
        value: String,
        suffix: String,
    },
    Bool(bool),
    Unit,
    /// `SynConst.SourceIdentifier(constant, value, range)` — the three magic
    /// identifiers `__SOURCE_DIRECTORY__` / `__SOURCE_FILE__` / `__LINE__`
    /// (`pars.fsy:3475-3477`, fed by the lexer's `KEYWORD_STRING`). FCS carries
    /// both the source spelling (`constant`) and the *expanded* value (`value`):
    /// the physical source directory, the physical source file name, or the
    /// physical 1-based line number. Path-valued expansions depend on the
    /// temporary source path used by the oracle, so the FCS projector validates
    /// them against the source range before canonicalising them.
    SourceIdentifier {
        constant: String,
        value: NormalisedSourceIdentifierValue,
    },
    /// `SynConst.Measure(constant, constantRange, synMeasure, trivia)` — a
    /// unit-of-measure annotated numeric literal (`1.0<m>`). `constant` is the
    /// underlying numeric `SynConst`; `measure` is the annotation. The constant
    /// range and the `SynMeasureConstantTrivia` (the `<`/`>` ranges) are elided.
    Measure {
        constant: Box<NormalisedConst>,
        measure: NormalisedMeasure,
    },
}

/// `SynMeasure` (`SyntaxTree.fsi:188-216`) — the annotation carried by a
/// [`NormalisedConst::Measure`]. One-for-one with FCS's cases; all ranges (and
/// the `*`/`/`/`^` operator ranges) are elided. FCS wraps every
/// `measureTypeExpr` in a [`Seq`](NormalisedMeasure::Seq), so even a single
/// named measure `<m>` is `Seq[Named ["m"]]`; the anonymous `<_>`
/// ([`Anon`](NormalisedMeasure::Anon)) is the sole un-wrapped form, reached
/// through the dedicated `measureTypeArg: LESS UNDERSCORE GREATER` arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalisedMeasure {
    /// `SynMeasure.Named(longId, _)` — a named unit (`m`, `SI.metre`). The
    /// segments of the `LongIdent`.
    Named(Vec<String>),
    /// `SynMeasure.Var(typar, _)` — a measure variable (`'u`).
    Var(NormalisedTypar),
    /// `SynMeasure.One(_)` — the dimensionless `1` (`<1>`).
    One,
    /// `SynMeasure.Anon(_)` — the inferred measure `<_>`.
    Anon,
    /// `SynMeasure.Seq(measures, _)` — a juxtaposition `<m s>`.
    Seq(Vec<NormalisedMeasure>),
    /// `SynMeasure.Product(m1, _, m2, _)` — `<m * s>`.
    Product(Box<NormalisedMeasure>, Box<NormalisedMeasure>),
    /// `SynMeasure.Divide(m1, _, m2, _)` — `<m / s>`, or the no-numerator
    /// reciprocal `</s>` where `m1` is `None`.
    Divide(Option<Box<NormalisedMeasure>>, Box<NormalisedMeasure>),
    /// `SynMeasure.Power(m, _, exponent, _)` — `<m ^ 2>`.
    Power(Box<NormalisedMeasure>, NormalisedRationalConst),
    /// `SynMeasure.Paren(m, _)` — `<(m s)>`.
    Paren(Box<NormalisedMeasure>),
}

/// Mirror of FCS's `SynStringKind` (`SyntaxTree.fsi:95`). Phase 2 emits
/// only `Regular`; the other variants land in later commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SynStringKind {
    Regular,
    Verbatim,
    TripleQuote,
}

/// Mirror of FCS's `SynByteStringKind` (`SyntaxTree.fs:132-135`). Only
/// two cases exist: `Regular` covers both `"..."B` and `"""..."""B`
/// source forms (FCS doesn't track triple-quoted byte strings
/// separately); `Verbatim` is `@"..."B`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SynByteStringKind {
    Regular,
    Verbatim,
}

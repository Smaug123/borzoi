//! Concrete-syntax tree built on top of [`rowan`].
//!
//! This is the "green/red" representation chosen in `docs/parser-plan.md` D3.
//! The parser builds an untyped [`SyntaxNode`] tree using [`rowan::GreenNodeBuilder`];
//! the *typed* facade in this module ([`ImplFile`], [`ModuleOrNamespace`], …)
//! wraps those nodes with strongly-typed accessors so consumers can navigate
//! the tree without matching on raw [`SyntaxKind`]s.

mod kinds;
pub mod projection;

// The borzoi-astgen-generated facade (plan PR D). The types category is generated and
// re-exported below as the real facade; bespoke `*Type` accessors stay
// hand-written in this module. See `generated/mod.rs`.
mod generated;

pub use generated::union_decls::*;
pub use generated::union_exprs::*;
pub use generated::union_pats::*;
pub use generated::union_types::*;
// The frozen per-version facades (plan PR E): `borzoi_cst::syntax::v8` /
// `::v9`. The names re-exported above (no `vN::` prefix) are the *union* surface
// — the maximal, every-version superset; pin a `vN` for a frozen, exhaustive
// view (`docs/ast-versioning-plan.md` D4).
pub use generated::{v8, v9};
pub use kinds::{KindInterval, SyntaxKind, kind_in_surface, kind_interval};

/// Rowan's [`rowan::Language`] is the per-language plumbing that ties our
/// [`SyntaxKind`] enum to rowan's `u16`-tagged green nodes. Zero-sized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FSharpLang {}

impl rowan::Language for FSharpLang {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        SyntaxKind::from_raw(raw.0).expect("rowan SyntaxKind out of range")
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxNode = rowan::SyntaxNode<FSharpLang>;
pub type SyntaxToken = rowan::SyntaxToken<FSharpLang>;
pub type SyntaxElement = rowan::SyntaxElement<FSharpLang>;

/// Common interface for the typed-AST facade. One implementor per node-kind
/// (see [`ImplFile`], [`ModuleOrNamespace`], …). Mirrors rust-analyzer's
/// `AstNode`.
pub trait AstNode: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}

/// Helper used by every typed-AST accessor: the first non-trivia child node
/// of the given kind.
fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    parent.children().find_map(N::cast)
}

/// Helper: iterate every non-trivia child node of the given kind.
fn children<'a, N: AstNode + 'a>(parent: &'a SyntaxNode) -> impl Iterator<Item = N> + 'a {
    parent.children().filter_map(N::cast)
}

/// Helper: the first child *token* of the given kind, skipping trivia. Used
/// by typed-AST accessors that name a specific keyword/punctuation token
/// (e.g. `LPAREN_TOK` inside a unit literal); the wildcard "first non-trivia
/// token" case is `ConstExpr::literal` which does not go via this helper.
#[allow(dead_code)] // wired up per accessor as kinds land
fn token(parent: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == kind)
}

/// `true` for an `ELIF_TOK`-headed nested `IF_THEN_ELSE_EXPR` — an `elif`
/// branch, which occupies an enclosing `if`'s else slot. Distinct from a nested
/// `if` headed by `IF_TOK` (`if a then if p then q`), which is itself the
/// then-branch. Used by [`IfThenElseExpr`]'s keyword-relative branch resolution
/// so a missing then-branch before a bare `elif` (`if a then elif b then c`)
/// still attributes the `elif` to the else slot rather than to `then`.
fn is_elif_node(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::IF_THEN_ELSE_EXPR && token(node, SyntaxKind::ELIF_TOK).is_some()
}

/// Helper: `` ``foo bar`` `` → `foo bar`; `foo` → `foo`. Identifier tokens keep
/// their backticks in the green tree (lossless), but FCS stores the unquoted
/// `Ident.idText`, so accessors that classify an identifier *by content* must
/// de-quote first.
fn dequote_ident(text: &str) -> &str {
    text.strip_prefix("``")
        .and_then(|t| t.strip_suffix("``"))
        .unwrap_or(text)
}

// ---- typed AST nodes -------------------------------------------------------
//
// The pattern below is the rust-analyzer playbook: every typed node is a
// thin newtype around a `SyntaxNode`, and accessors walk the children. We
// write these out by hand initially (per plan D6); revisit `ungrammar`
// codegen once the facade is large enough to feel the maintenance.

macro_rules! ast_node {
    ($name:ident, $kind:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(SyntaxNode);

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == SyntaxKind::$kind
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                if Self::can_cast(node.kind()) {
                    Some(Self(node))
                } else {
                    None
                }
            }
            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

ast_node!(ImplFile, IMPL_FILE);
ast_node!(SigFile, SIG_FILE);
ast_node!(ModuleOrNamespace, MODULE_OR_NAMESPACE);
// The `ModuleDecl` / `SigDecl` / `TypeDefnRepr` / `MemberDefn` / `Measure` /
// `RationalConst` dispatch enums and their member newtypes are GENERATED
// (`generated::union_decls`, re-exported above); their accessors stay
// hand-written below (plan PR D4). Standalone non-variant nodes interspersed in
// this block (`TypeDefn`, `Typar*`, `RecordFieldDecl`, `*Case`, `ImplicitCtor`,
// `GetSetAccessor`, `ValSig`, `Binding`, …) stay hand-written.
ast_node!(TypeDefn, TYPE_DEFN);
ast_node!(TyparDecls, TYPAR_DECLS);
ast_node!(TyparDecl, TYPAR_DECL);
ast_node!(TyparConstraints, TYPAR_CONSTRAINTS);
ast_node!(TyparConstraint, TYPAR_CONSTRAINT);
ast_node!(RecordFieldDecl, RECORD_FIELD_DECL);
ast_node!(UnionCase, UNION_CASE);
ast_node!(UnionCaseField, UNION_CASE_FIELD);
ast_node!(EnumCase, ENUM_CASE);
ast_node!(ImplicitCtor, IMPLICIT_CTOR);
ast_node!(GetSetAccessor, GET_SET_ACCESSOR);
ast_node!(ValSig, VAL_SIG);
ast_node!(Binding, BINDING);
ast_node!(BindingReturnInfo, BINDING_RETURN_INFO);
// The `Pat` dispatch enum and its member newtypes (`*Pat`) are GENERATED
// (`generated::union_pats`, re-exported above); their accessors stay
// hand-written below (plan PR D2). `ActivePatName` / `NamePatPairs` /
// `NamePatPair` / `RecordPatField` are not `Pat` variants, so they stay
// hand-written here.
ast_node!(ActivePatName, ACTIVE_PAT_NAME);
ast_node!(NamePatPairs, NAME_PAT_PAIRS);
ast_node!(NamePatPair, NAME_PAT_PAIR);
ast_node!(RecordPatField, RECORD_PAT_FIELD);
// The `Expr` dispatch enum and its member newtypes (`*Expr`, incl. the
// multi-kind `AppExpr` / `YieldExpr`) are GENERATED (`generated::union_exprs`,
// re-exported above); their accessors stay hand-written below (plan PR D3).
// Nodes interspersed here that are NOT `Expr` variants — `LongIdent`,
// `AttributeList`, `Attribute`, `RecordField`, `MatchClause` — stay hand-written.
ast_node!(LongIdent, LONG_IDENT);
ast_node!(AttributeList, ATTRIBUTE_LIST);
ast_node!(Attribute, ATTRIBUTE);
// The `Type` dispatch enum and its member newtypes (`*Type`) are GENERATED
// (`generated::union_types`, re-exported above); their accessors stay
// hand-written in this module (the `impl *Type { … }` blocks below). See plan
// PR D1. `AnonRecdTypeField` is not a `Type` variant, so it stays hand-written.
ast_node!(AnonRecdTypeField, ANON_RECD_TYPE_FIELD);
ast_node!(RecordField, RECORD_FIELD);
ast_node!(MatchClause, MATCH_CLAUSE);
// The two child nodes of a `STATIC_OPTIMIZATION_EXPR`
// ([`StaticOptimizationExpr`], a generated `Expr` member). Not dispatch-enum
// members, so they stay hand-written here, like [`MatchClause`].
ast_node!(StaticOptWhenClause, STATIC_OPT_WHEN_CLAUSE);
ast_node!(StaticOptCondition, STATIC_OPT_CONDITION);

// `YieldExpr` (`YIELD_OR_RETURN_EXPR | YIELD_OR_RETURN_FROM_EXPR`) and `AppExpr`
// (`APP_EXPR | INFIX_APP_EXPR`) are the two multi-kind `Expr` nodes; they are now
// GENERATED (`generated::union_exprs`) with explicit kind sets, like every other
// `*Expr` node. Their bespoke accessors (`is_from`/`is_yield`, `is_infix`) stay
// hand-written below.

impl ImplFile {
    pub fn modules(&self) -> impl Iterator<Item = ModuleOrNamespace> + '_ {
        children(&self.0)
    }
}

impl SigFile {
    /// The file's `SynModuleOrNamespaceSig`s (phase 10.11) — FCS's
    /// `ParsedSigFileInput.contents`. Reuses the [`ModuleOrNamespace`] facade
    /// (the parser emits the same `MODULE_OR_NAMESPACE` node for sig headers,
    /// whose `kind`/`long_id`/`is_rec`/`attributes` read identically).
    pub fn modules(&self) -> impl Iterator<Item = ModuleOrNamespace> + '_ {
        children(&self.0)
    }
}

/// `SynModuleOrNamespaceKind` — the four shapes a top-level
/// [`ModuleOrNamespace`] can take. [`Anon`](ModuleOrNamespaceKind::Anon) is
/// the implicit module wrapping a script-style body (no header keyword);
/// the other three carry a source-derived `longId` and are introduced by a
/// `module`/`namespace` header (phase 8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModuleOrNamespaceKind {
    /// No header keyword — `SynModuleOrNamespaceKind.AnonModule`.
    Anon,
    /// `module Foo` — `SynModuleOrNamespaceKind.NamedModule`.
    NamedModule,
    /// `namespace Foo.Bar` — `SynModuleOrNamespaceKind.DeclaredNamespace`.
    DeclaredNamespace,
    /// `namespace global` — `SynModuleOrNamespaceKind.GlobalNamespace`
    /// (empty `longId`).
    GlobalNamespace,
}

impl ModuleOrNamespace {
    pub fn decls(&self) -> impl Iterator<Item = ModuleDecl> + '_ {
        children(&self.0)
    }

    /// The signature-file declarations (phase 10.13a) — FCS's
    /// `SynModuleOrNamespaceSig.decls`. Used when this node is a sig-file
    /// segment (under a [`SigFile`] root); casts the decl children to [`SigDecl`]
    /// rather than the impl-side [`ModuleDecl`].
    pub fn sig_decls(&self) -> impl Iterator<Item = SigDecl> + '_ {
        children(&self.0)
    }

    /// The leading header keyword token, if any: [`SyntaxKind::MODULE_TOK`]
    /// (a named module) or [`SyntaxKind::NAMESPACE_TOK`] (a namespace). An
    /// anonymous module has neither.
    fn header_keyword(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| matches!(t.kind(), SyntaxKind::MODULE_TOK | SyntaxKind::NAMESPACE_TOK))
    }

    /// `SynModuleOrNamespaceKind` — derived from the header tokens. No header
    /// keyword ⇒ [`Anon`](ModuleOrNamespaceKind::Anon); a `module` keyword ⇒
    /// [`NamedModule`](ModuleOrNamespaceKind::NamedModule); a `namespace`
    /// keyword ⇒ [`GlobalNamespace`](ModuleOrNamespaceKind::GlobalNamespace)
    /// when a [`SyntaxKind::GLOBAL_TOK`] is present, else
    /// [`DeclaredNamespace`](ModuleOrNamespaceKind::DeclaredNamespace).
    pub fn kind(&self) -> ModuleOrNamespaceKind {
        match self.header_keyword().map(|t| t.kind()) {
            None => ModuleOrNamespaceKind::Anon,
            Some(SyntaxKind::MODULE_TOK) => ModuleOrNamespaceKind::NamedModule,
            Some(SyntaxKind::NAMESPACE_TOK) => {
                if self
                    .0
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .any(|t| t.kind() == SyntaxKind::GLOBAL_TOK)
                {
                    ModuleOrNamespaceKind::GlobalNamespace
                } else {
                    ModuleOrNamespaceKind::DeclaredNamespace
                }
            }
            Some(_) => unreachable!("header_keyword only returns MODULE_TOK / NAMESPACE_TOK"),
        }
    }

    /// `true` for the implicit anonymous module wrapping a script-style body
    /// — i.e. no `module`/`namespace` header keyword child.
    pub fn is_anon(&self) -> bool {
        self.kind() == ModuleOrNamespaceKind::Anon
    }

    /// `SynModuleOrNamespace.isRecursive` — `true` iff a `rec` keyword
    /// (`module rec Foo` / `namespace rec A.B`) sits in the header. Encoded
    /// as the presence of a [`SyntaxKind::REC_TOK`] child token.
    pub fn is_rec(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::REC_TOK)
    }

    /// The header's dotted name — FCS's `SynModuleOrNamespace.longId`. The
    /// header's [`SyntaxKind::LONG_IDENT`] is the only direct `LONG_IDENT`
    /// child (body decls nest their own paths inside `*_DECL` nodes).
    /// `None` for an anonymous module and for `namespace global` (whose
    /// target is a [`SyntaxKind::GLOBAL_TOK`], not a path).
    pub fn long_id(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// The header's attribute lists — FCS's `SynModuleOrNamespace.attribs`, e.g.
    /// `[<AutoOpen>]` on a whole-file `module Foo` header. Covers both the
    /// *leading* `[<A>] module Foo` form (phase 10.7e) and the *after-keyword*
    /// `module [<A>] Foo` form (phase 10.7k) — FCS appends them into one list, so
    /// these are the [`SyntaxKind::ATTRIBUTE_LIST`] children *before the name*
    /// (`take_while` up to the header [`SyntaxKind::LONG_IDENT`]). A body decl that
    /// fails attribute recovery leaves a bare `ATTRIBUTE_LIST` as a *later* child
    /// (after the name), which is correctly excluded. Only for a
    /// [`NamedModule`](ModuleOrNamespaceKind::NamedModule): an anonymous module (no
    /// header) and a `namespace` (which cannot carry attributes) both yield none.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        let named_module = self.kind() == ModuleOrNamespaceKind::NamedModule;
        self.0
            .children_with_tokens()
            .take_while(|el| el.kind() != SyntaxKind::LONG_IDENT)
            .filter_map(|el| el.into_node())
            .filter_map(AttributeList::cast)
            .take(if named_module { usize::MAX } else { 0 })
    }
}

impl ValDecl {
    /// The value signature — FCS's `SynModuleSigDecl.Val.valSig` (`SynValSig`).
    /// The sole [`VAL_SIG`](SyntaxKind::VAL_SIG) child holding the name and
    /// `: <type>`. `None` only on a malformed (parser-bailed) decl.
    pub fn val_sig(&self) -> Option<ValSig> {
        child(&self.0)
    }

    /// The `val` signature's attribute lists — FCS's `SynValSig.attributes`,
    /// e.g. `[<Literal>] val x : int`. The leading `ATTRIBUTE_LIST` children of
    /// the `VAL_DECL` (the signature type nests inside the `VAL_SIG` child, so
    /// no attribute list leaks in from there — cf. [`AbstractSlot::attributes`]).
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }
}

impl TypeDefnsDecl {
    /// The type definitions in this `SynModuleDecl.Types` group. Phase 9.1
    /// yields exactly one; an `and`-chain (phase 9.2) yields several.
    pub fn defns(&self) -> impl Iterator<Item = TypeDefn> + '_ {
        children(&self.0)
    }
}

impl AttributesDecl {
    /// The attribute lists — FCS's `SynModuleDecl.Attributes.attributes` (phase
    /// 10.7), a standalone `[<assembly: …>]` not attached to a carrier. The
    /// [`AttributeList`] children, in source order.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }
}

impl TypeDefn {
    /// The type-header attribute lists — FCS's `SynComponentInfo.attributes`
    /// (phase 10.7a). Leading [`AttributeList`] children of the `TYPE_DEFN`,
    /// before the `TYPE_TOK`/`AND_TOK` keyword. Only the first definition of a
    /// `type … and …` group carries them (the leading `[<…>]` attaches to the
    /// first `SynComponentInfo`); `and`-chained definitions yield none here.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The type's name — FCS's `SynComponentInfo.longId`. The only direct
    /// [`SyntaxKind::LONG_IDENT`] child (the abbreviation RHS's own paths nest
    /// inside the repr node). `None` only on a malformed header with no name.
    pub fn long_id(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// The definition's type parameters — FCS's `SynComponentInfo.typeParams`
    /// (`SynTyparDecls option`, phase 9.3). `None` for a non-generic definition.
    /// The `PostfixList`/`PrefixList`/`SinglePrefix` variant is not represented
    /// (the typar list is the same); see [`TyparDecls::typars`].
    pub fn typar_decls(&self) -> Option<TyparDecls> {
        child(&self.0)
    }

    /// The definition's right-hand side — FCS's `SynTypeDefnRepr`. `None` for a
    /// **bodyless** type (no `=`, FCS's `SynTypeDefnSimpleRepr.None` —
    /// `[<Measure>] type m`, `type Foo`) and on a malformed body; otherwise the
    /// repr node (an abbreviation, record, union, enum, or object model).
    pub fn repr(&self) -> Option<TypeDefnRepr> {
        child(&self.0)
    }

    /// The implicit primary constructor — FCS's `SynTypeDefn.implicitConstructor`
    /// (phase 9.8a, `type T(args) [as self]`). `None` for a definition without
    /// a primary constructor. The constructor also appears (prepended) in the
    /// object-model repr's member list on FCS's side; the normaliser mirrors
    /// that dual placement.
    pub fn implicit_ctor(&self) -> Option<ImplicitCtor> {
        child(&self.0)
    }

    /// The outer member list — FCS's `SynTypeDefn.members` (phase 9.13). These
    /// are an *augmentation*'s members (`type T with member …`) or trailing
    /// members on a simple repr (`type R = {…} with member …`), distinct from a
    /// pure object model's members (which live inside the
    /// [`ObjectModelRepr`](TypeDefnRepr::ObjectModel) repr, slot 1). The direct
    /// [`SyntaxKind::MEMBER_DEFN`]/[`SyntaxKind::MEMBER_LET_BINDINGS`] children
    /// of the `TYPE_DEFN` (a pure object model nests its members inside the repr
    /// node, so they are not picked up here); empty for a non-augmented
    /// definition.
    pub fn members(&self) -> impl Iterator<Item = MemberDefn> + '_ {
        children(&self.0)
    }

    /// The type-parameter constraints, in source order (phase 9.3b) — the union
    /// of FCS's `SynTyparDecls.PostfixList` constraints (the inside-`<>` `when`
    /// clause, nested in [`TyparDecls`]) and `SynComponentInfo.constraints` (the
    /// after-decls `when` clause, a direct [`TyparConstraints`] child). The
    /// inside-`<>` clause precedes the after-decls one, matching FCS's
    /// concatenation order. Empty when the definition has no header `when`
    /// clause.
    ///
    /// Collected from those two *header* positions only — **not** every
    /// [`TyparConstraint`] descendant: a member's `when`-constrained return type
    /// (`member _.M (x: 'T) : 'T when 'T : struct`) nests a [`TyparConstraints`]
    /// inside the member body (under a [`SyntaxKind::CONSTRAINED_TYPE`]), and
    /// that constraint belongs to the *return type*, not the type's header.
    pub fn constraints(&self) -> impl Iterator<Item = TyparConstraint> + '_ {
        // inside-`<>` clause (nested in the header's `TyparDecls`), then the
        // after-decls clause (a direct `TyparConstraints` child of the
        // `TYPE_DEFN`). Both are owned newtypes, so collect into a `Vec` to
        // sidestep borrowing the temporary `TyparConstraints`.
        let inside = self
            .typar_decls()
            .and_then(|d| d.constraint_clause())
            .map(|c| c.constraints().collect::<Vec<_>>())
            .unwrap_or_default();
        let after = child::<TyparConstraints>(&self.0)
            .map(|c| c.constraints().collect::<Vec<_>>())
            .unwrap_or_default();
        inside.into_iter().chain(after)
    }
}

impl TyparDecls {
    /// The individual type-parameter declarations, in source order — FCS's
    /// `SynTyparDecls.TyparDecls`.
    pub fn typars(&self) -> impl Iterator<Item = TyparDecl> + '_ {
        children(&self.0)
    }

    /// The inside-`<>` `when` constraint clause, if present (phase 9.3b) — FCS's
    /// `SynTyparDecls.PostfixList` constraints.
    pub fn constraint_clause(&self) -> Option<TyparConstraints> {
        child(&self.0)
    }
}

/// The kind of a [`TyparConstraint`] (phase 9.3b) — the supported
/// `SynTypeConstraint` variants, read from the constraint's operator/keyword
/// tokens. The deferred variants (`default`, `enum`/`delegate`, member, and the
/// self-constrained bare type) have no parser surface, so they are absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TyparConstraintKind {
    /// `'a :> T` — `WhereTyparSubtypeOfType` (carries [`TyparConstraint::ty`]).
    SubtypeOf,
    /// `'a : struct` — `WhereTyparIsValueType`.
    ValueType,
    /// `'a : not struct` — `WhereTyparIsReferenceType`.
    ReferenceType,
    /// `'a : null` — `WhereTyparSupportsNull`.
    SupportsNull,
    /// `'a : not null` — `WhereTyparNotSupportsNull`.
    NotSupportsNull,
    /// `'a : comparison` — `WhereTyparIsComparable`.
    Comparable,
    /// `'a : equality` — `WhereTyparIsEquatable`.
    Equatable,
    /// `'a : unmanaged` — `WhereTyparIsUnmanaged`.
    Unmanaged,
    /// `'a : enum<'b>` — `WhereTyparIsEnum` (carries [`TyparConstraint::type_args`]).
    Enum,
    /// `'a : delegate<args, ret>` — `WhereTyparIsDelegate` (carries
    /// [`TyparConstraint::type_args`]).
    Delegate,
}

impl TyparConstraints {
    /// The individual constraints, in source order — FCS's `typeConstraints`
    /// (`and`-separated).
    pub fn constraints(&self) -> impl Iterator<Item = TyparConstraint> + '_ {
        children(&self.0)
    }
}

impl TyparConstraint {
    /// The subject type variable — the `'a` in `'a : comparison` (FCS's
    /// `typar`; reuses the [`TyparDecl`] node shape). `None` for the
    /// parenthesised SRTP-alternatives form `(^a or ^b) : (member …)` /
    /// `(Witnesses or ^T) : (member …)`, whose `or`-separated operands are
    /// general types read via [`Self::support_types`], not a subject typar.
    pub fn typar(&self) -> Option<TyparDecl> {
        child(&self.0)
    }

    /// The `or`-separated support types of the parenthesised SRTP-alternatives
    /// member constraint `(^a or ^b) : (member …)` / `(Witnesses or ^T) :
    /// (member …)` — FCS's `typeAlts` operands (`appTypeWithoutNull`), each a
    /// [`VarType`] typar or a concrete [`Type`]. The direct `Type` children of
    /// the constraint, in source order; empty for the single-typar form `^T :
    /// (member …)` (whose support is the subject [`Self::typar`]) and for every
    /// non-member kind. The `'a :> T` subtype target is *also* a direct `Type`
    /// child, so it is excluded here (by the same `COLON_GREATER_TOK` check
    /// [`Self::ty`] keys off) — the two accessors partition the constraint's
    /// `Type` children by kind and never both return one.
    pub fn support_types(&self) -> impl Iterator<Item = Type> + '_ {
        // A `:>` subtype constraint's direct `Type` child is the target, read via
        // `ty`; it is not an SRTP support alternative.
        let is_subtype = token(&self.0, SyntaxKind::COLON_GREATER_TOK).is_some();
        children(&self.0).filter(move |_| !is_subtype)
    }

    /// The constraint type — present only for the subtype form `'a :> T`
    /// (`WhereTyparSubtypeOfType`); `None` for every other kind. Keyed on the
    /// `COLON_GREATER_TOK` so a parenthesised SRTP support `(Foo or ^T) :
    /// (member …)`, whose operand `Type`s are also direct children, does not
    /// surface its first alternative here.
    pub fn ty(&self) -> Option<Type> {
        if token(&self.0, SyntaxKind::COLON_GREATER_TOK).is_some() {
            child(&self.0)
        } else {
            None
        }
    }

    /// The type-argument list — present only for the `enum<…>` / `delegate<…>`
    /// forms (`WhereTyparIsEnum` / `WhereTyparIsDelegate`), whose `< … >` args
    /// live in the [`SyntaxKind::CONSTRAINT_TYPE_ARGS`] wrapper child. Empty for
    /// every other kind, so the subtype form's direct constraint type (read via
    /// [`Self::ty`]) is never returned here.
    pub fn type_args(&self) -> impl Iterator<Item = Type> {
        let args: Vec<Type> = self
            .0
            .children()
            .find(|n| n.kind() == SyntaxKind::CONSTRAINT_TYPE_ARGS)
            .map(|wrapper| children(&wrapper).collect())
            .unwrap_or_default();
        args.into_iter()
    }

    /// The constrained member signature — present only for an SRTP member
    /// constraint `^T : (static member M : sig)` (`WhereTyparSupportsMember`),
    /// the `MEMBER_SIG` child. `None` for every other kind. The constraint's
    /// own `^T` subject is a [`TyparDecl`], not a [`MemberSig`], so it is never
    /// returned here.
    pub fn member_sig(&self) -> Option<MemberSig> {
        child(&self.0)
    }

    /// The self-constrained type — present only for the F# 7 bare
    /// self-constraint `when IFoo<'T>` (`WhereSelfConstrained`), whose type is
    /// wrapped in a [`SyntaxKind::SELF_CONSTRAINT`] child. `None` for every
    /// other kind. The wrapper keeps this type from being read as the subtype
    /// form's direct constraint type (`'a :> T`, [`Self::ty`]); a self-constraint
    /// has no subject typar, so [`Self::typar`] returns `None` for it.
    pub fn self_constraint(&self) -> Option<Type> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::SELF_CONSTRAINT)
            .and_then(|n| child::<Type>(&n))
    }

    /// Which constraint this is, read from the operator/keyword tokens. `None`
    /// on a malformed/unsupported constraint (see [`TyparConstraintKind`]).
    pub fn kind(&self) -> Option<TyparConstraintKind> {
        if token(&self.0, SyntaxKind::COLON_GREATER_TOK).is_some() {
            return Some(TyparConstraintKind::SubtypeOf);
        }
        // A *direct* `IDENT_TOK` reading `not` (de-quoted, so `` ``not`` `` also
        // matches). The subject typar's own ident is nested in the [`TyparDecl`]
        // child, so it is not seen here.
        let has_not = self
            .0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::IDENT_TOK && dequote_ident(t.text()) == "not");
        if token(&self.0, SyntaxKind::STRUCT_TOK).is_some() {
            return Some(if has_not {
                TyparConstraintKind::ReferenceType
            } else {
                TyparConstraintKind::ValueType
            });
        }
        if token(&self.0, SyntaxKind::NULL_TOK).is_some() {
            return Some(if has_not {
                TyparConstraintKind::NotSupportsNull
            } else {
                TyparConstraintKind::SupportsNull
            });
        }
        // `'a : delegate<…>` — the `delegate` keyword is its own `DELEGATE_TOK`,
        // not an `IDENT_TOK`, so it must be checked before the ident lookup below
        // (which `?`-returns `None` when no direct ident is present).
        if token(&self.0, SyntaxKind::DELEGATE_TOK).is_some() {
            return Some(TyparConstraintKind::Delegate);
        }
        let ident = self
            .0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)?;
        match dequote_ident(ident.text()) {
            "comparison" => Some(TyparConstraintKind::Comparable),
            "equality" => Some(TyparConstraintKind::Equatable),
            "unmanaged" => Some(TyparConstraintKind::Unmanaged),
            // `'a : enum<…>` — a bare `enum` ident (not a keyword) followed by a
            // type-argument list (read via [`Self::type_args`]).
            "enum" => Some(TyparConstraintKind::Enum),
            _ => None,
        }
    }
}

impl TyparDecl {
    /// The typar's leading attribute lists — FCS's `SynTyparDecl.attributes`
    /// (field 0), `type T<[<Measure>] 'a>`. Leading [`AttributeList`] children;
    /// empty for an unattributed typar.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The type-variable identifier — the `a` in `'a` / `^a`
    /// (`SynTypar.ident`).
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// `true` for the head-type form `^a` (`TyparStaticReq.HeadType`), `false`
    /// for the plain `'a` form (`TyparStaticReq.None`). Read from the sigil
    /// token kind ([`SyntaxKind::HAT_TOK`] vs [`SyntaxKind::QUOTE_TOK`]).
    pub fn is_head_type(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| matches!(t.kind(), SyntaxKind::QUOTE_TOK | SyntaxKind::HAT_TOK))
            .map(|t| t.kind() == SyntaxKind::HAT_TOK)
            .unwrap_or(false)
    }

    /// The `& <flexible-type>` intersection constraints — FCS's
    /// `SynTyparDecl.intersectionConstraints` (`'t & #seq<int> & #IDisposable`,
    /// each a `hashConstraint` flexible type). The [`Type`] children of the
    /// declaration, in source order; empty for a plain typar. (A typar decl has
    /// no other [`Type`] children — its attributes nest inside `ATTRIBUTE_LIST`
    /// and its name is a token — so every [`Type`] child is a constraint.)
    pub fn intersection_constraints(&self) -> impl Iterator<Item = Type> + '_ {
        children(&self.0)
    }
}

impl TypeAbbrev {
    /// The abbreviated type — FCS's `SynTypeDefnSimpleRepr.TypeAbbrev.rhsType`.
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl DelegateRepr {
    /// The delegate's signature type — the `topType` after `of`. This is FCS's
    /// `SynTypeDefnKind.Delegate.ty` (and the type of the synthetic `Invoke`
    /// slot). `None` only on a malformed body with no parseable type.
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl RecordRepr {
    /// The record's fields in source order — FCS's
    /// `SynTypeDefnSimpleRepr.Record.recordFields`.
    pub fn fields(&self) -> impl Iterator<Item = RecordFieldDecl> + '_ {
        children(&self.0)
    }
}

impl RecordFieldDecl {
    /// The field's attribute lists — FCS's `SynField.attributes` (phase 10.7),
    /// `type R = { [<A>] X : int }`. Leading [`AttributeList`] children; empty for
    /// an unattributed field.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The field name — FCS's `SynField.idOpt` (always `Some` for a record
    /// field; `None` only on a malformed field with no name).
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// `true` iff the field is `mutable` — FCS's `SynField.isMutable`. Encoded
    /// as the presence of a [`SyntaxKind::MUTABLE_TOK`] child.
    pub fn is_mutable(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::MUTABLE_TOK)
    }

    /// The field's type — FCS's `SynField.fieldType` (the full `parse_type`).
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl UnionRepr {
    /// The union's cases in source order — FCS's
    /// `SynTypeDefnSimpleRepr.Union.unionCases`.
    pub fn cases(&self) -> impl Iterator<Item = UnionCase> + '_ {
        children(&self.0)
    }
}

impl UnionCase {
    /// The case's attribute lists — FCS's `SynUnionCase.attributes` (phase 10.7),
    /// `type T = | [<A>] X`. Leading [`AttributeList`] children; empty for an
    /// unattributed case.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The case name — FCS's `SynUnionCase.ident` (`SynIdent`). The first direct
    /// [`SyntaxKind::IDENT_TOK`] child (a named field's ident is nested inside a
    /// [`SyntaxKind::UNION_CASE_FIELD`], so it is not a direct child here).
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// The case's fields in source order — FCS's `SynUnionCaseKind.Fields`
    /// (`of T1 * x:T2 * …`). Empty for a nullary case (and for the
    /// [`Self::full_type`] signature form).
    pub fn fields(&self) -> impl Iterator<Item = UnionCaseField> + '_ {
        children(&self.0)
    }

    /// The case's `FullType` signature — FCS's `SynUnionCaseKind.FullType`
    /// (`Name : topType`, FSharp.Core's `| Some : Value:'T -> 'T option`). The
    /// sole direct [`Type`] child, present only for the signature form (`None`
    /// for the ordinary `Fields` form). A named *field*'s type nests inside a
    /// [`SyntaxKind::UNION_CASE_FIELD`], so it never collides here.
    pub fn full_type(&self) -> Option<Type> {
        child(&self.0)
    }

    /// The compiled operator name and source range of an operator-named case —
    /// `op_Nil` for `([])` and `op_ColonColon` for `( :: )` (FSharp.Core's `list`
    /// constructors, FCS's `CompileOpName`). `None` for an ordinary ident-named
    /// case (use [`Self::ident`]).
    pub fn operator_name(&self) -> Option<(&'static str, rowan::TextRange)> {
        operator_case_name(&self.0)
    }
}

/// The compiled operator name (`op_Nil` / `op_ColonColon`) and source range for
/// an operator-named union or enum case — `([])` (an `LBRACK_TOK`+`RBRACK_TOK`
/// name) or `( :: )` (a `COLON_COLON_TOK` name), the FSharp.Core `list`
/// constructors that are valid as both. `None` for an ordinary ident-named case.
/// The single production source of these derived names (FCS records `op_Nil` /
/// `op_ColonColon` as the `SynIdent.idText`); consumed by name resolution and
/// the differential normaliser alike.
fn operator_case_name(node: &SyntaxNode) -> Option<(&'static str, rowan::TextRange)> {
    let mut lbrack = None;
    let mut rbrack = None;
    for tok in node.children_with_tokens().filter_map(|el| el.into_token()) {
        match tok.kind() {
            SyntaxKind::COLON_COLON_TOK => return Some(("op_ColonColon", tok.text_range())),
            SyntaxKind::LBRACK_TOK => lbrack = Some(tok.text_range()),
            SyntaxKind::RBRACK_TOK => rbrack = Some(tok.text_range()),
            _ => {}
        }
    }
    match (lbrack, rbrack) {
        (Some(l), Some(r)) => Some(("op_Nil", l.cover(r))),
        _ => None,
    }
}

impl UnionCaseField {
    /// The field's optional name — FCS's `SynField.idOpt` (`Some` for a named
    /// field `x : T`, `None` for an anonymous field `T`). The first direct
    /// [`SyntaxKind::IDENT_TOK`] child; the field type's own idents are nested
    /// inside the type node, so they are not picked up here.
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// The field's type — FCS's `SynField.fieldType` (parsed at the
    /// tuple-segment level, so the case's `*` separates fields).
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl EnumRepr {
    /// The enum's cases in source order — FCS's
    /// `SynTypeDefnSimpleRepr.Enum.cases`.
    pub fn cases(&self) -> impl Iterator<Item = EnumCase> + '_ {
        children(&self.0)
    }
}

impl EnumCase {
    /// The case's attribute lists — FCS's `SynEnumCase.attributes` (phase 10.7),
    /// `type E = | [<A>] A = 0`. Leading [`AttributeList`] children; empty for an
    /// unattributed case.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The case name — FCS's `SynEnumCase.ident` (`SynIdent`). The first direct
    /// [`SyntaxKind::IDENT_TOK`] child (the value expression's own idents are
    /// nested inside the value node).
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// The case value — FCS's `SynEnumCase.valueExpr` (a `SynExpr`, e.g.
    /// `Const 0`), the single [`Expr`] child after the `=`.
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The compiled operator name and source range of an operator-named enum case
    /// — `op_Nil` for `([])` and `op_ColonColon` for `( :: )` (FCS's bar-led
    /// `unionCaseName EQUALS atomicExpr`, e.g. `| ([]) = 0`). `None` for an
    /// ordinary ident-named case.
    pub fn operator_name(&self) -> Option<(&'static str, rowan::TextRange)> {
        operator_case_name(&self.0)
    }
}

impl ObjectModelRepr {
    /// The object model's members in source order — FCS's
    /// `SynTypeDefnRepr.ObjectModel.members` (phase 9.7 yields one or more
    /// [`MemberDefn::Member`]). Empty for an *augmentation* repr (its members
    /// live in the outer [`TypeDefn::members`] slot; see [`Self::is_augmentation`]).
    pub fn members(&self) -> impl Iterator<Item = MemberDefn> + '_ {
        children(&self.0)
    }

    /// `true` iff this is an *augmentation* repr — FCS's
    /// `SynTypeDefnKind.Augmentation` (`type T with member …`, phase 9.13a).
    /// Encoded as the presence of a [`SyntaxKind::WITH_TOK`] child (the `with`
    /// that stands in for the `=`); the members are then in the outer
    /// [`TypeDefn::members`] slot and this repr node carries none. A pure object
    /// model (`type C = member …`, kind `Unspecified`) has no `with`.
    pub fn is_augmentation(&self) -> bool {
        token(&self.0, SyntaxKind::WITH_TOK).is_some()
    }

    /// `true` iff written with an explicit `class … end` kind marker — FCS's
    /// `SynTypeDefnKind.Class` (phase 9.12). Encoded as a [`SyntaxKind::CLASS_TOK`]
    /// direct token child.
    pub fn is_class(&self) -> bool {
        token(&self.0, SyntaxKind::CLASS_TOK).is_some()
    }

    /// `true` iff written with an explicit `struct … end` kind marker — FCS's
    /// `SynTypeDefnKind.Struct` (phase 9.12, a [`SyntaxKind::STRUCT_TOK`] child).
    pub fn is_struct(&self) -> bool {
        token(&self.0, SyntaxKind::STRUCT_TOK).is_some()
    }

    /// `true` iff written with an explicit `interface … end` kind marker — FCS's
    /// `SynTypeDefnKind.Interface` (phase 9.12). Encoded as a direct
    /// [`SyntaxKind::INTERFACE_TOK`] *token* child; an `interface` *member*
    /// (9.11b) nests its `INTERFACE_TOK` inside an [`InterfaceImpl`] node, so this
    /// direct-token check does not confuse the two.
    pub fn is_interface(&self) -> bool {
        token(&self.0, SyntaxKind::INTERFACE_TOK).is_some()
    }
}

/// Which keyword introduced a [`MemberMethod`] — drives the binding's
/// `SynLeadingKeyword` (and the elided `SynMemberFlags`). `member` (9.7),
/// `static member` (9.9a), `override` / `default` (9.10a), or `new` (9.10b, an
/// explicit constructor whose head is the `new` keyword).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberLeading {
    Member,
    StaticMember,
    Override,
    Default,
    New,
}

impl MemberMethod {
    /// The member's `SynBinding` — its head pattern (`this.M`, a dotted
    /// [`LongIdentPat`]) and RHS expression. `None` only on a malformed
    /// (parser-bailed) member.
    pub fn binding(&self) -> Option<Binding> {
        child(&self.0)
    }

    /// The member's attribute lists — FCS's `SynBinding.attributes` (phase
    /// 10.7f), e.g. `[<A>] member this.M = …`. The leading `ATTRIBUTE_LIST`
    /// children of the `MEMBER_DEFN` (the binding's head/RHS nest their own nodes,
    /// so no attribute list leaks in from there).
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// `true` iff this is a `static member` (phase 9.9a) — encoded as a leading
    /// [`SyntaxKind::STATIC_TOK`] child. Drives the binding's `SynLeadingKeyword`
    /// (`StaticMember` vs `Member`) and `SynMemberFlags.IsInstance`.
    pub fn is_static(&self) -> bool {
        self.leading_keyword() == MemberLeading::StaticMember
    }

    /// Which keyword leads this member — its `SynLeadingKeyword`. Read off the
    /// first direct keyword token: `static member` is a leading
    /// [`SyntaxKind::STATIC_TOK`] (9.9a); `override`/`default` their own tokens
    /// (9.10a); otherwise a plain `member` (9.7). An explicit constructor
    /// `new(…)` (9.10b) has no leading keyword token — its head *is* the `new`
    /// keyword (the binding head's sole [`SyntaxKind::NEW_TOK`] segment) — so it
    /// is detected from the head. A malformed member with no keyword defaults to
    /// [`MemberLeading::Member`].
    pub fn leading_keyword(&self) -> MemberLeading {
        let direct = self
            .0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find_map(|t| match t.kind() {
                SyntaxKind::STATIC_TOK => Some(MemberLeading::StaticMember),
                SyntaxKind::OVERRIDE_TOK => Some(MemberLeading::Override),
                SyntaxKind::DEFAULT_TOK => Some(MemberLeading::Default),
                SyntaxKind::MEMBER_TOK => Some(MemberLeading::Member),
                _ => None,
            });
        if let Some(k) = direct {
            return k;
        }
        if self.head_is_new() {
            return MemberLeading::New;
        }
        MemberLeading::Member
    }

    /// `true` iff this member is an explicit constructor (phase 9.10b) — its
    /// binding head is the `new` keyword (a [`SyntaxKind::NEW_TOK`] segment in
    /// the head [`LongIdent`]). Scans only the head `LONG_IDENT` (path-segment
    /// tokens), never the body, so a `new` expression in the RHS can't trip it.
    fn head_is_new(&self) -> bool {
        self.binding()
            .and_then(|b| b.pat())
            .and_then(|p| match p {
                Pat::LongIdent(l) => l.head(),
                _ => None,
            })
            .and_then(|head| head.idents().next())
            .is_some_and(|t| t.kind() == SyntaxKind::NEW_TOK)
    }
}

impl AbstractSlot {
    /// The slot's value signature — FCS's `SynMemberDefn.AbstractSlot.slotSig`
    /// (`SynValSig`), the `[VAL_SIG]` child holding the name and `: <type>`.
    /// `None` only on a malformed (parser-bailed) slot.
    pub fn val_sig(&self) -> Option<ValSig> {
        child(&self.0)
    }

    /// The slot's attribute lists — FCS's `SynValSig.attributes` (phase 10.7g),
    /// e.g. `[<A>] abstract member M : int`. The leading `ATTRIBUTE_LIST` children
    /// of the `ABSTRACT_SLOT` (the signature type nests inside the `VAL_SIG`
    /// child, so no attribute list leaks in from there).
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// `true` iff written `abstract member …` (a [`SyntaxKind::MEMBER_TOK`] after
    /// `abstract`) rather than the bare `abstract …` — FCS's `AbstractMember`
    /// vs `Abstract` leading keyword (`SynValSig.trivia.LeadingKeyword`).
    pub fn is_abstract_member(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::MEMBER_TOK)
    }

    /// `true` iff written `static abstract …` (a leading [`SyntaxKind::STATIC_TOK`]
    /// before `abstract`) — the F# 7 IWSAM static-abstract interface slot. Selects
    /// the `StaticAbstract`/`StaticAbstractMember` leading keyword (paired with
    /// [`Self::is_abstract_member`]) over `Abstract`/`AbstractMember`.
    pub fn is_static(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::STATIC_TOK)
    }
}

impl MemberSig {
    /// The value signature — FCS's `SynMemberSig.Member.memberSig` (`SynValSig`):
    /// the sole [`VAL_SIG`](SyntaxKind::VAL_SIG) child holding the name and
    /// `: <type>`. `None` only on a malformed (parser-bailed) member sig.
    pub fn val_sig(&self) -> Option<ValSig> {
        child(&self.0)
    }

    /// The member-sig's attribute lists — FCS's `SynValSig.attributes`. The
    /// leading [`AttributeList`] children (the signature type's attributes nest
    /// inside the `VAL_SIG`).
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The leading keyword that introduces the member sig — derived from the
    /// [`STATIC_TOK`](SyntaxKind::STATIC_TOK) / [`ABSTRACT_TOK`](SyntaxKind::ABSTRACT_TOK)
    /// / [`MEMBER_TOK`](SyntaxKind::MEMBER_TOK) marker tokens, matching FCS's
    /// `SynValSig.trivia.LeadingKeyword`: `static member` → `StaticMember`,
    /// `abstract member` → `AbstractMember`, `abstract` → `Abstract`, plain
    /// `member` → `Member`. (`new`-constructor sigs are a later slice.)
    pub fn leading_keyword(&self) -> MemberSigLeading {
        let mut has_static = false;
        let mut has_abstract = false;
        let mut has_member = false;
        for tok in self
            .0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
        {
            match tok.kind() {
                SyntaxKind::STATIC_TOK => has_static = true,
                SyntaxKind::ABSTRACT_TOK => has_abstract = true,
                SyntaxKind::MEMBER_TOK => has_member = true,
                // A `new`-ctor sig (`new : … -> T`) — the `new` keyword is the
                // leading marker and the (synthetic) name; FCS's `New` leading
                // keyword on a `SynMemberSig.Member` with `CtorMemberFlags`.
                SyntaxKind::NEW_TOK => return MemberSigLeading::New,
                // `override M : …` / `default M : …` — a standalone leading
                // keyword (never combined with `static`/`abstract`/`member`);
                // FCS's `SynLeadingKeyword.Override` / `.Default`. Both share the
                // `IsOverrideOrExplicitImpl` member flags, so the keyword is the
                // only distinguisher.
                SyntaxKind::OVERRIDE_TOK => return MemberSigLeading::Override,
                SyntaxKind::DEFAULT_TOK => return MemberSigLeading::Default,
                _ => {}
            }
        }
        match (has_static, has_abstract, has_member) {
            (true, true, true) => MemberSigLeading::StaticAbstractMember,
            (true, true, false) => MemberSigLeading::StaticAbstract,
            (false, true, true) => MemberSigLeading::AbstractMember,
            (false, true, false) => MemberSigLeading::Abstract,
            (true, false, true) => MemberSigLeading::StaticMember,
            // `static` with no `member` — FCS's `SynLeadingKeyword.Static` (a
            // distinct keyword from `StaticMember`), e.g. `static Zero : ^T` in
            // an SRTP member constraint.
            (true, false, false) => MemberSigLeading::Static,
            _ => MemberSigLeading::Member,
        }
    }
}

/// Which leading keyword introduces a [`MemberSig`] — FCS's
/// `SynValSig.trivia.LeadingKeyword` for a `SynMemberSig.Member`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberSigLeading {
    /// `member M : T`.
    Member,
    /// `static M : T` — a `static`-only member sig (no `member`), FCS's
    /// `SynLeadingKeyword.Static`. Valid in an SRTP member constraint
    /// (`^T : (static Zero : ^T)`).
    Static,
    /// `static member M : T`.
    StaticMember,
    /// `abstract M : T`.
    Abstract,
    /// `abstract member M : T`.
    AbstractMember,
    /// `static abstract M : T`.
    StaticAbstract,
    /// `static abstract member M : T`.
    StaticAbstractMember,
    /// `override M : T` — an override member sig, FCS's `SynLeadingKeyword.Override`.
    Override,
    /// `default M : T` — a default member sig, FCS's `SynLeadingKeyword.Default`.
    Default,
    /// `new : … -> T` — an explicit constructor sig (name "new").
    New,
}

impl ValSig {
    /// The signature's identifier — FCS's `SynValSig.ident` (`abstract M : …` →
    /// `M`). The first [`SyntaxKind::IDENT_TOK`] child (the signature type's
    /// idents nest inside the type node). For an operator-named value
    /// (`val (+) : …`) this is the bare operator token between the parens (the
    /// `IDENT_TOK` of the `[LPAREN_TOK, IDENT_TOK, RPAREN_TOK]` run), matching
    /// FCS's `OriginalNotationWithParen` spelling. `None` on a malformed slot
    /// **or** an active-pattern name (`val (|Foo|_|) : …`), whose idents live
    /// inside the [`ActivePatName`] node — read those via [`Self::active_pat_name`].
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// The active-pattern name — `(|Foo|_|)`, `(|Foo|Bar|)` — when this value is
    /// named by one (FCS's `valSpfn` `opName` active-pattern productions).
    /// Mutually exclusive with [`Self::ident`]: an active-pattern name carries
    /// an [`ActivePatName`] child instead of a bare `IDENT_TOK`. FCS folds the
    /// whole name into the single `idText` of `SynValSig.ident`; reconstruct
    /// that text from [`ActivePatName::case_tokens`].
    pub fn active_pat_name(&self) -> Option<ActivePatName> {
        child(&self.0)
    }

    /// The signature type — FCS's `SynValSig.synType` (`abstract M : int -> int`
    /// → `Fun(int, int)`). The `VAL_SIG`'s [`Type`] child. `None` only on a
    /// malformed slot (a missing `: <type>`).
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }

    /// The explicit value type parameters — FCS's `SynValSig.explicitTypeParams`
    /// (`val f<'T> : …`, phase 10.12). The postfix [`TyparDecls`] child (the same
    /// node a `type T<'a>` header carries), or `None` for a non-generic value.
    pub fn typar_decls(&self) -> Option<TyparDecls> {
        child(&self.0)
    }

    /// The inside-`<>` `when` constraint clause on the explicit typars
    /// (`val f<'T when 'T : comparison> : …`), in source order — FCS's
    /// `SynTyparDecls.PostfixList` constraints, nested in [`Self::typar_decls`].
    /// (The *after-type* `when` clause — `val f : 'T -> 'T when …` — lives in the
    /// signature type as a [`Type::Constrained`], not here.) Empty when the value
    /// has no inside-`<>` constraints.
    pub fn constraints(&self) -> impl Iterator<Item = TyparConstraint> + '_ {
        self.typar_decls()
            .and_then(|d| d.constraint_clause())
            .into_iter()
            .flat_map(|c| c.constraints().collect::<Vec<_>>())
    }

    /// The `= <literal>` value, if present — FCS's `SynValSig.synExpr` (a
    /// `[<Literal>]` value's right-hand side, `val x : int = 1`, phase 10.12). The
    /// sole [`Expr`] child (a full `SynExpr`; the signature type is the [`Type`]
    /// child, a different node kind, so the two never clash). `None` for a `val`
    /// without a literal value.
    pub fn literal_value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl MemberLetBindings {
    /// The class-local bindings in source order — FCS's
    /// `SynMemberDefn.LetBindings.bindings` (one for `let x = …`, several for an
    /// `and`-chain). Same `BINDING` children as a [`LetDecl`].
    pub fn bindings(&self) -> impl Iterator<Item = Binding> + '_ {
        children(&self.0)
    }

    /// The binding group's attribute lists — FCS's `SynBinding.attributes` on the
    /// head binding (phase 10.7l, `opt_attributes opt_access classDefnBindings`,
    /// `pars.fsy:2004`), e.g. `[<VolatileField>] let mutable x = 0`. The leading
    /// `ATTRIBUTE_LIST` children of the `MEMBER_LET_BINDINGS` (the bindings nest
    /// their own pattern/RHS nodes, so no attribute list leaks in from there). A
    /// consumer projects these onto the *first* binding, exactly like the
    /// module-level [`LetDecl`] (FCS homes the run on the group's first binding).
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// `SynMemberDefn.LetBindings.isRecursive` — `true` iff a `rec` keyword
    /// follows the `let` (encoded as a [`SyntaxKind::REC_TOK`] child).
    pub fn is_rec(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::REC_TOK)
    }

    /// `true` iff introduced by `use` rather than `let` (the head `LET_TOK`'s
    /// text is `use`) — drives the binding's `SynLeadingKeyword`.
    pub fn is_use(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::LET_TOK)
            .is_some_and(|t| t.text() == "use")
    }

    /// `true` iff a `static let`/`static let rec` — FCS's
    /// `SynMemberDefn.LetBindings.isStatic` (phase 9.8c), encoded as a leading
    /// [`SyntaxKind::STATIC_TOK`] child (before `LET_TOK`).
    pub fn is_static(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::STATIC_TOK)
    }
}

impl MemberDo {
    /// The `do`-bound expression — FCS's `SynBinding.expr` for the `Do` binding
    /// (the body run at construction time). Held inside the reused
    /// [`SyntaxKind::DO_EXPR`] child, whose offside scaffolding is zero-width
    /// `ERROR` tokens, so this digs through to the `DO_EXPR`'s body expression.
    pub fn expr(&self) -> Option<Expr> {
        child::<DoExpr>(&self.0).and_then(|d| d.inner())
    }

    /// `true` iff a `static do` — FCS's `SynMemberDefn.LetBindings.isStatic`
    /// (phase 9.8d), encoded as a leading [`SyntaxKind::STATIC_TOK`] child
    /// (before the `DO_EXPR`).
    pub fn is_static(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::STATIC_TOK)
    }
}

impl ValField {
    /// The field's attribute lists — FCS's `SynField.attributes` (phase 10.7i,
    /// field 0), e.g. `[<DefaultValue>] val mutable x : int`. The leading
    /// `ATTRIBUTE_LIST` children of the `VAL_FIELD` (the field type nests inside
    /// its own [`Type`] node, so no attribute list leaks in from there).
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The field name — FCS's `SynField.idOpt` (always `Some` for a `val`
    /// field; `None` only on a malformed field). The first [`SyntaxKind::IDENT_TOK`]
    /// child (the field type's idents nest inside the type node).
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// `true` iff `val mutable …` — FCS's `SynField.isMutable`
    /// ([`SyntaxKind::MUTABLE_TOK`] child).
    pub fn is_mutable(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::MUTABLE_TOK)
    }

    /// `true` iff `static val …` — FCS's `SynField.isStatic`
    /// ([`SyntaxKind::STATIC_TOK`] child).
    pub fn is_static(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::STATIC_TOK)
    }

    /// The field's type — FCS's `SynField.fieldType` (the full `parse_type`).
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl InheritMember {
    /// The base class type — FCS's `SynMemberDefn.Inherit.baseType` /
    /// `ImplicitInherit.inheritType` (the `inherit Base` / `inherit Base()`
    /// `atomType`). The sole direct [`Type`] child (the constructor-args
    /// expression's own types nest inside the [`Expr`] child). `None` only on a
    /// malformed `inherit` with no base type (FCS's `Inherit(None, …)` recovery).
    pub fn base_type(&self) -> Option<Type> {
        child(&self.0)
    }

    /// The constructor arguments — FCS's `SynMemberDefn.ImplicitInherit.
    /// inheritArgs` (`inherit Base()` → `Const Unit`, `inherit Base(a, b)` →
    /// `Paren(Tuple)`). The sole direct [`Expr`] child. `None` for the
    /// argument-less `inherit Base` form (FCS's `SynMemberDefn.Inherit`).
    pub fn args(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// `true` iff this is the `ImplicitInherit` form (constructor args present,
    /// `inherit Base()`), `false` for the bare `Inherit` form (`inherit Base`) —
    /// the discriminant FCS encodes as the `ImplicitInherit` vs `Inherit` case.
    pub fn is_implicit(&self) -> bool {
        self.args().is_some()
    }
}

impl InterfaceImpl {
    /// The implemented interface — FCS's `SynMemberDefn.Interface.interfaceType`
    /// (`interface I` / `interface I<int>` / `interface Foo.IBar`, an
    /// `appTypeWithoutNull`). The sole direct [`Type`] child (the `with`-block
    /// members' own types nest inside their member nodes). `None` only on a
    /// malformed `interface` with no type.
    pub fn interface_type(&self) -> Option<Type> {
        child(&self.0)
    }

    /// `true` iff the interface carries a `with member …` block — a
    /// [`SyntaxKind::WITH_TOK`] child. This is FCS's `members: SynMemberDefns
    /// option` discriminant: `with` → `Some` (the [`Self::members`] list, possibly
    /// empty); no `with` → `None` (the bare `interface I`).
    pub fn has_with(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::WITH_TOK)
    }

    /// The interface's own member implementations — FCS's
    /// `SynMemberDefn.Interface.members` (the `with member … = …` block). The
    /// [`MemberDefn`] children of the `INTERFACE_IMPL` node; empty for a bare
    /// `interface I` (where [`Self::has_with`] is `false`, i.e. FCS's `None`).
    pub fn members(&self) -> impl Iterator<Item = MemberDefn> + '_ {
        children(&self.0)
    }
}

impl GetSetMember {
    /// The property's attribute lists — FCS's `SynBinding.attributes` (phase
    /// 10.7f), e.g. `[<A>] member this.P with get … and set …`. The leading
    /// `ATTRIBUTE_LIST` children of the `GET_SET_MEMBER`; FCS duplicates the
    /// attribute onto *both* accessor bindings, so a consumer projects this list
    /// onto each present accessor.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The property path — FCS's per-accessor `SynBinding.headPat` `longDotId`
    /// (e.g. `this.P`, shared by both accessors; we store it once on the member
    /// head). The sole [`LongIdentPat`]'s [`LongIdent`]. `None` only on a
    /// malformed head. For a dotted operator / active-pattern property name
    /// (`member x.(+) with …`, `member x.(|Foo|_|) with …`) read the whole head
    /// via [`Self::head_pat`] instead — an active-pattern segment is a sibling
    /// `ACTIVE_PAT_NAME`, not part of this `LONG_IDENT`.
    pub fn name(&self) -> Option<LongIdent> {
        child::<LongIdentPat>(&self.0).and_then(|p| child(p.syntax()))
    }

    /// The property's head pattern — the sole [`LongIdentPat`]. Carries the full
    /// name (a dotted operator's `( op )` tokens inside the `LONG_IDENT`, or an
    /// active-pattern's sibling `ACTIVE_PAT_NAME`), so a consumer wanting every
    /// name segment should read this rather than [`Self::name`].
    pub fn head_pat(&self) -> Option<LongIdentPat> {
        child(&self.0)
    }

    /// `true` iff `static member P with get … ` — FCS's `SynMemberFlags.IsInstance
    /// = false` on both accessor bindings, encoded as a leading
    /// [`SyntaxKind::STATIC_TOK`] child (the same marker [`MemberMethod`] and
    /// [`AutoProperty`] carry).
    pub fn is_static(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::STATIC_TOK)
    }

    /// The getter accessor — the [`GetSetAccessor`] child whose keyword is
    /// `get` (FCS's `getBinding`, `MemberKind = PropertyGet`). `None` for a
    /// set-only property.
    pub fn getter(&self) -> Option<GetSetAccessor> {
        children::<GetSetAccessor>(&self.0).find(|a| a.is_get())
    }

    /// The setter accessor — the [`GetSetAccessor`] child whose keyword is
    /// `set` (FCS's `setBinding`). `None` for a get-only property.
    pub fn setter(&self) -> Option<GetSetAccessor> {
        children::<GetSetAccessor>(&self.0).find(|a| !a.is_get())
    }
}

impl GetSetAccessor {
    /// `true` iff this is the `get` accessor (a [`SyntaxKind::GET_TOK`] child),
    /// `false` for the `set` accessor (a [`SyntaxKind::SET_TOK`]).
    pub fn is_get(&self) -> bool {
        token(&self.0, SyntaxKind::GET_TOK).is_some()
    }

    /// The accessor's *own* attribute lists — FCS's `with [<A>] get() …`
    /// (`pars.fsy`'s `opt_attributes` before the accessor keyword). The leading
    /// `ATTRIBUTE_LIST` children of the `GET_SET_ACCESSOR`. Distinct from the
    /// property-level `[<A>] member this.P with …`, which FCS duplicates onto both
    /// accessor bindings ahead of these (a consumer concatenates the two, the
    /// property-level lists first — matching FCS's `SynBinding.attributes` order).
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The accessor's argument patterns — FCS's `SynBinding.headPat` args
    /// (`get()` → `[Paren(Const Unit)]`, `set v` → `[Named v]`, `get(i)` →
    /// `[Paren(Named i)]`). The [`Pat`] children, in source order.
    pub fn args(&self) -> impl Iterator<Item = Pat> + '_ {
        children(&self.0)
    }

    /// The accessor body — FCS's `SynBinding` rhs expression (`get() = <expr>`).
    /// The sole [`Expr`] child. `None` only on a malformed accessor.
    pub fn body(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The accessor's return-type annotation, if any (`get() : T = …`) — the
    /// type inside the [`SyntaxKind::BINDING_RETURN_INFO`] child. FCS models
    /// each accessor as a `SynBinding`, so this is that binding's `returnInfo`,
    /// and (as elsewhere) FCS wraps the body in `SynExpr.Typed(body, T)`;
    /// [`Self::body`] returns the unwrapped body regardless.
    pub fn return_type(&self) -> Option<Type> {
        child::<BindingReturnInfo>(&self.0).and_then(|ri| ri.ty())
    }
}

/// `SynMemberKind` for an auto-property's `propKind` (phase 9.9c), derived from
/// the `with get[, set]` clause: a plain `member val X = e`
/// ([`Member`](AutoPropertyKind::Member)), `with get`
/// ([`PropertyGet`](AutoPropertyKind::PropertyGet)), `with set`
/// ([`PropertySet`](AutoPropertyKind::PropertySet) — grammar-accepted, rejected
/// only later by the checker), or `with get, set`
/// ([`PropertyGetSet`](AutoPropertyKind::PropertyGetSet)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoPropertyKind {
    Member,
    PropertyGet,
    PropertySet,
    PropertyGetSet,
}

impl AutoProperty {
    /// The property's attribute lists — FCS's
    /// `SynMemberDefn.AutoProperty.attributes` (phase 10.7h, field 0), e.g.
    /// `[<A>] member val X = 0`. The leading `ATTRIBUTE_LIST` children of the
    /// `AUTO_PROPERTY` (the type annotation and RHS nest inside their own nodes,
    /// so no attribute list leaks in from there).
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The property name — FCS's `SynMemberDefn.AutoProperty.ident`. The first
    /// [`SyntaxKind::IDENT_TOK`] child (the type annotation's and RHS's idents
    /// nest inside their nodes).
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// `true` iff `static member val …` — FCS's
    /// `SynMemberDefn.AutoProperty.isStatic` ([`SyntaxKind::STATIC_TOK`] child).
    pub fn is_static(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::STATIC_TOK)
    }

    /// The optional type annotation — FCS's `AutoProperty.typeOpt`
    /// (`member val X : T = …`). The direct child [`Type`] node, present only
    /// after a [`SyntaxKind::COLON_TOK`].
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }

    /// The initialiser expression — FCS's `AutoProperty.synExpr` (the `= <expr>`
    /// RHS).
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The getter/setter shape — FCS's `AutoProperty.propKind`, derived from the
    /// `with get[, set]` clause: a [`SyntaxKind::SET_TOK`] child means
    /// `with get, set`; a [`SyntaxKind::GET_TOK`] child alone means `with get`;
    /// neither means a plain `member val X = e`.
    pub fn prop_kind(&self) -> AutoPropertyKind {
        let mut get = false;
        let mut set = false;
        for t in self
            .0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
        {
            match t.kind() {
                SyntaxKind::GET_TOK => get = true,
                SyntaxKind::SET_TOK => set = true,
                _ => {}
            }
        }
        match (get, set) {
            (true, true) => AutoPropertyKind::PropertyGetSet,
            (true, false) => AutoPropertyKind::PropertyGet,
            (false, true) => AutoPropertyKind::PropertySet,
            (false, false) => AutoPropertyKind::Member,
        }
    }
}

impl ImplicitCtor {
    /// The constructor's attribute lists — FCS's
    /// `SynMemberDefn.ImplicitCtor.attributes` (phase 10.7j, field 1), e.g.
    /// `type T [<A>] ()`. The leading `ATTRIBUTE_LIST` children of the
    /// `IMPLICIT_CTOR` (the args pattern nests inside its own [`Pat`] node, so no
    /// attribute list leaks in from there).
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The constructor argument pattern — FCS's `SynMemberDefn.ImplicitCtor`
    /// `ctorArgs` (a `SynPat`): a [`Pat::Const`] (unit) for the empty `()`, or
    /// a [`Pat::Paren`] for `(x: int, y)` etc. `None` only on a malformed ctor.
    pub fn args(&self) -> Option<Pat> {
        child(&self.0)
    }

    /// The `as <self-id>` self-identifier — FCS's
    /// `SynMemberDefn.ImplicitCtor.selfIdentifier`. The [`SyntaxKind::IDENT_TOK`]
    /// following the [`SyntaxKind::AS_TOK`]; `None` when there is no `as` clause.
    pub fn self_id(&self) -> Option<SyntaxToken> {
        // The only direct `IDENT_TOK` child (the args pattern's idents nest
        // inside the pat node), present only after an `as`.
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }
}

impl ExprDecl {
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl OpenDecl {
    /// `true` for the `open type T` form — `SynOpenDeclTarget.Type`. Encoded
    /// as the presence of a [`SyntaxKind::TYPE_TOK`] child (the swallowed
    /// `type` keyword the parser recovered from the raw stream). When `false`
    /// the target is a module/namespace path (`SynOpenDeclTarget.ModuleOrNamespace`).
    pub fn is_type(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::TYPE_TOK)
    }

    /// The dotted path of a module/namespace `open` — FCS's
    /// `SynOpenDeclTarget.ModuleOrNamespace.longId`. `None` for the
    /// `open type T` form (use [`OpenDecl::ty`] there).
    pub fn long_ident(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// The opened type of an `open type T` — FCS's
    /// `SynOpenDeclTarget.Type.typeName`. `None` for the module/namespace
    /// form (use [`OpenDecl::long_ident`] there).
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl NestedModuleDecl {
    /// The module header's attribute lists — FCS's nested
    /// `SynComponentInfo.attributes`, e.g. `[<AutoOpen>]` on a `module M = …` head.
    /// Covers both the *leading* `[<A>] module M` form (phase 10.7d) and the
    /// *after-keyword* `module [<A>] M` form (phase 10.7k) — FCS appends them into
    /// one list, so these are the [`SyntaxKind::ATTRIBUTE_LIST`] children *before
    /// the name* (`take_while` up to the header [`SyntaxKind::LONG_IDENT`]). A body
    /// decl that fails attribute recovery (e.g. `[<A>] open System`, a deferred
    /// carrier) leaves a bare `ATTRIBUTE_LIST` as a *later* child (after the name),
    /// which is correctly excluded. (Well-formed body attributes nest inside their
    /// own `*_DECL` node and never appear here.)
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        self.0
            .children_with_tokens()
            .take_while(|el| el.kind() != SyntaxKind::LONG_IDENT)
            .filter_map(|el| el.into_node())
            .filter_map(AttributeList::cast)
    }

    /// The nested module's dotted name — FCS's
    /// `SynModuleDecl.NestedModule.moduleInfo` (`SynComponentInfo.longId`).
    /// The header's [`SyntaxKind::LONG_IDENT`] is the only *direct*
    /// `LONG_IDENT` child (body decls nest their own paths inside `*_DECL`
    /// nodes). `None` only on a malformed header with no name.
    pub fn long_id(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// `SynModuleDecl.NestedModule.isRecursive` — `true` iff a `rec` keyword
    /// (`module rec X = …`) sits in the header. Encoded as the presence of a
    /// direct [`SyntaxKind::REC_TOK`] child token.
    pub fn is_rec(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::REC_TOK)
    }

    /// The nested module's body — FCS's `SynModuleDecl.NestedModule.decls`.
    /// The decls are direct `*_DECL` children (the body is *not* wrapped in a
    /// [`SyntaxKind::MODULE_OR_NAMESPACE`]).
    pub fn decls(&self) -> impl Iterator<Item = ModuleDecl> + '_ {
        children(&self.0)
    }

    /// The nested module's body in a *signature* file — FCS's
    /// `SynModuleSigDecl.NestedModule.moduleDecls` (phase 10.13b). The same direct
    /// `*_DECL` children as [`Self::decls`], cast to [`SigDecl`] instead of the
    /// impl-side [`ModuleDecl`].
    pub fn sig_decls(&self) -> impl Iterator<Item = SigDecl> + '_ {
        children(&self.0)
    }
}

impl ModuleAbbrevDecl {
    /// The abbreviation's LHS — FCS's `SynModuleDecl.ModuleAbbrev.ident`, a
    /// *single* name. Encoded as the *first* direct [`SyntaxKind::LONG_IDENT`]
    /// child (the header name; the parser rejects a dotted LHS as an `ERROR`
    /// node, so a `MODULE_ABBREV_DECL`'s LHS is always one segment).
    pub fn ident(&self) -> Option<LongIdent> {
        children::<LongIdent>(&self.0).next()
    }

    /// The abbreviation's RHS — FCS's `SynModuleDecl.ModuleAbbrev.longId`, the
    /// (possibly dotted) module path being abbreviated. The *second* direct
    /// [`SyntaxKind::LONG_IDENT`] child (the body, parsed as a bare path).
    pub fn long_id(&self) -> Option<LongIdent> {
        children::<LongIdent>(&self.0).nth(1)
    }
}

impl ExceptionDefnDecl {
    /// The exception's attribute lists — FCS's `SynExceptionDefnRepr.attributes`
    /// (phase 10.7m). The leading `[<A>] exception …` lists and any after-keyword
    /// `exception [<B>] …` lists are both direct [`AttributeList`] children of the
    /// `EXCEPTION_DEFN` (before the case's `UNION_CASE` node), so they appear here
    /// in source order — matching FCS's `$1 @ cas` concatenation. The reused
    /// `caseName` (`SynUnionCase`) carries its own attributes (always empty for an
    /// exception) inside the nested `UNION_CASE`, so it does not leak in here.
    /// Empty for an unattributed exception.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The exception's case data — FCS's `SynExceptionDefnRepr.caseName`, a
    /// reused [`UnionCase`] (`SynUnionCase`) carrying the name and any `of`
    /// fields (`exception E of int`). The single direct
    /// [`SyntaxKind::UNION_CASE`] child. `None` only on a malformed definition
    /// with no case name.
    pub fn union_case(&self) -> Option<UnionCase> {
        child(&self.0)
    }

    /// The abbreviation target — FCS's `SynExceptionDefnRepr.longId`, the
    /// (possibly dotted) path of `exception E = SomeExn`. The direct
    /// [`SyntaxKind::LONG_IDENT`] child after the `=`. `None` for a
    /// non-abbreviation exception.
    pub fn abbrev_path(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// The augmentation members — FCS's `SynExceptionDefn.members`, the
    /// `exception E with member …` block (phase 9.15b). Empty for a plain
    /// exception (phase 9.15a never produces members).
    pub fn members(&self) -> impl Iterator<Item = MemberDefn> + '_ {
        children(&self.0)
    }
}

impl LetDecl {
    /// `SynModuleDecl.Let.isRec` — `true` iff the parser saw a `rec` keyword
    /// after `let`. Encoded as the presence of a [`SyntaxKind::REC_TOK`]
    /// child token (set at parse time; trivia and other token kinds are
    /// filtered out so only the actual `rec` matches).
    pub fn is_rec(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::REC_TOK)
    }

    /// `true` iff this declaration was introduced by `use` rather than `let`
    /// (the head keyword's `LET_TOK` text is `use`). Drives the binding's
    /// `SynLeadingKeyword` (`Use`/`UseRec` vs `Let`/`LetRec`). Both `let` and
    /// `use` reach the parser through the same `Virtual::Let` and are emitted
    /// as `LET_TOK`, so the text — not the kind — carries the distinction.
    pub fn is_use(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::LET_TOK)
            .is_some_and(|t| t.text() == "use")
    }

    /// The bindings introduced by this `let`. A single `let x = e` yields
    /// one binding; `let x = e and y = e'` yields two; etc.
    pub fn bindings(&self) -> impl Iterator<Item = Binding> + '_ {
        children(&self.0)
    }

    /// The `[< … >]` attribute groups attached to this `let` (phase 10.5).
    /// FCS models attributes on each `SynBinding`, attaching a leading
    /// `opt_attributes` to the *first* binding of the group; our green tree
    /// keeps them as leading `ATTRIBUTE_LIST` children of the `LET_DECL`
    /// (before `LET_TOK`, matching source order), and the normaliser projects
    /// them onto the first binding's `attributes`. Empty for an unattributed
    /// `let`.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }
}

impl ExternDecl {
    /// The leading `[< … >]` attribute groups (FCS's `SynBinding.attributes` —
    /// the `[<DllImport(…)>]`). Only the *direct* `ATTRIBUTE_LIST` children; the
    /// return type's own attributes are nested inside the `EXTERN_RET` child and
    /// are elided by the normaliser.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The return type (`EXTERN_RET`, FCS's `cRetType`).
    pub fn return_info(&self) -> Option<ExternRet> {
        child(&self.0)
    }

    /// The prototype's name (FCS's `ident`, a single-segment `SynLongIdent`).
    pub fn name(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// The arguments (FCS's `externArgs`), in source order.
    pub fn args(&self) -> impl Iterator<Item = ExternArg> + '_ {
        children(&self.0)
    }
}

/// The base of an extern C type before any C-style suffixes (`&`, `*`, `[]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExternCTypeBase {
    /// The C `void` keyword.
    Void(SyntaxToken),
    /// A path-like C type (`int`, `System.IntPtr`, `byref`, ...).
    Path(LongIdent),
}

/// A C-style suffix on an extern C type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExternCTypeSuffix {
    /// The managed-byref suffix (`T&`).
    Byref(SyntaxToken),
    /// The native-pointer suffix (`T*` / `void*`).
    Pointer(SyntaxToken),
    /// The array suffix (`T[]`), represented by the opening `[` token.
    Array(SyntaxToken),
}

fn extern_c_type_base(node: &SyntaxNode) -> Option<ExternCTypeBase> {
    node.children_with_tokens().find_map(|el| match el {
        rowan::NodeOrToken::Node(n) => LongIdent::cast(n).map(ExternCTypeBase::Path),
        rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::VOID_TOK => {
            Some(ExternCTypeBase::Void(t))
        }
        _ => None,
    })
}

fn extern_c_type_suffixes(node: &SyntaxNode) -> impl Iterator<Item = ExternCTypeSuffix> + '_ {
    node.children_with_tokens()
        .filter_map(|el| match el.into_token()? {
            t if t.kind() == SyntaxKind::AMP_TOK => Some(ExternCTypeSuffix::Byref(t)),
            t if t.kind() == SyntaxKind::STAR_TOK => Some(ExternCTypeSuffix::Pointer(t)),
            t if t.kind() == SyntaxKind::LBRACK_TOK => Some(ExternCTypeSuffix::Array(t)),
            _ => None,
        })
}

impl ExternRet {
    /// `true` iff the return type is the bare C `void` keyword, with no C-style
    /// suffixes. FCS maps this form to `unit`; `void*` is a native pointer type
    /// and returns `false`.
    pub fn is_void(&self) -> bool {
        matches!(self.c_type_base(), Some(ExternCTypeBase::Void(_)))
            && self.c_type_suffixes().next().is_none()
    }

    /// The return type's base path (`None` for `void`-based C types such as
    /// bare `void` and `void*`). Use [`Self::c_type_base`] and
    /// [`Self::c_type_suffixes`] to distinguish those forms.
    pub fn ty(&self) -> Option<LongIdent> {
        match self.c_type_base() {
            Some(ExternCTypeBase::Path(path)) => Some(path),
            Some(ExternCTypeBase::Void(_)) | None => None,
        }
    }

    /// The return type's base (`void` or a path), before C-style suffixes.
    pub fn c_type_base(&self) -> Option<ExternCTypeBase> {
        extern_c_type_base(&self.0)
    }

    /// The C-style suffixes (`&`, `*`, `[]`) attached to the return type, in
    /// source order.
    pub fn c_type_suffixes(&self) -> impl Iterator<Item = ExternCTypeSuffix> + '_ {
        extern_c_type_suffixes(&self.0)
    }
}

impl ExternArg {
    /// The argument's `[< … >]` attribute groups (FCS's `externArg`'s
    /// `opt_attributes`).
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// The argument's base C type path (FCS's `cType`), before C-style suffixes.
    /// Returns `None` for `void`-based C types such as `void*`; use
    /// [`Self::c_type_base`] and [`Self::c_type_suffixes`] to distinguish them.
    pub fn ty(&self) -> Option<LongIdent> {
        match self.c_type_base() {
            Some(ExternCTypeBase::Path(path)) => Some(path),
            Some(ExternCTypeBase::Void(_)) | None => None,
        }
    }

    /// The argument type's base (`void` or a path), before C-style suffixes.
    pub fn c_type_base(&self) -> Option<ExternCTypeBase> {
        extern_c_type_base(&self.0)
    }

    /// The C-style suffixes (`&`, `*`, `[]`) attached to the argument type, in
    /// source order.
    pub fn c_type_suffixes(&self) -> impl Iterator<Item = ExternCTypeSuffix> + '_ {
        extern_c_type_suffixes(&self.0)
    }

    /// The optional argument name (`None` → an unnamed `SynPat.Wild`) — the
    /// trailing `IDENT_TOK` after the type path.
    pub fn name(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }
}

impl AttributeList {
    /// The attributes inside this `[< … >]` group — one or more, `;`-separated
    /// (phase 10.5a; FCS's `attributeListElements`).
    pub fn attributes(&self) -> impl Iterator<Item = Attribute> + '_ {
        children(&self.0)
    }
}

impl Attribute {
    /// `SynAttribute.TypeName` — the attribute's `path` as a [`LongIdent`].
    pub fn type_name(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// `SynAttribute.ArgExpr` — the optional atomic argument expression
    /// (`[<Foo(1, 2)>]`, phase 10.5b). The `Expr` child after the `LONG_IDENT`
    /// path (which is not `Expr`-castable, so `child` returns the arg). `None`
    /// for a bare attribute, whose FCS `ArgExpr` is the synthetic `mkSynUnit`.
    pub fn arg(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// `SynAttribute.Target` — the optional `attributeTarget` word
    /// (`[<assembly: Foo>]` → `assembly`, phase 10.5c). The `IDENT_TOK` inside
    /// the leading [`SyntaxKind::ATTRIBUTE_TARGET`] child; `None` for an
    /// untargeted attribute.
    pub fn target(&self) -> Option<SyntaxToken> {
        self.0
            .children()
            .find(|n| n.kind() == SyntaxKind::ATTRIBUTE_TARGET)
            .and_then(|n| {
                n.children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
            })
    }
}

impl Binding {
    /// The binding's leading attribute lists — FCS's `SynBinding.attributes`
    /// (field 4) in the `let [<Literal>] x = …` form, where the run sits between
    /// the `let`/`and` keyword and the pattern. Leading [`AttributeList`] children
    /// of the `BINDING`; empty for an unattributed binding (and for the pre-`let`
    /// form `[<A>] let x`, whose attributes are leading children of the enclosing
    /// `LET_DECL` instead). A pattern's own attributes (`ATTRIB_PAT`) are nested
    /// inside the pattern child, not direct children, so they are not included.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// `SynBinding.isMutable` — `true` iff the parser saw a `mutable`
    /// keyword on this binding. Encoded as the presence of a
    /// [`SyntaxKind::MUTABLE_TOK`] child token.
    pub fn is_mutable(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::MUTABLE_TOK)
    }

    /// `SynBinding.isInline` — `true` iff the parser saw an `inline`
    /// keyword on this binding. Encoded as the presence of an
    /// [`SyntaxKind::INLINE_TOK`] child token.
    pub fn is_inline(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::INLINE_TOK)
    }

    /// `SynBinding.headPat` — the LHS pattern. [`NamedPat`] for the
    /// value form `let x = e`, [`WildcardPat`] for `let _ = e`, or
    /// [`LongIdentPat`] for the function form `let f x y = e`.
    pub fn pat(&self) -> Option<Pat> {
        child(&self.0)
    }

    /// `SynBinding.expr` — the RHS expression. Returns `None` only on a
    /// malformed (parser-bailed) tree; well-formed input always has one.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The return-type annotation, if the head had one (`let x : T = …`) —
    /// the type inside the [`SyntaxKind::BINDING_RETURN_INFO`] child. For a
    /// regular binding, FCS also wraps the RHS in `SynExpr.Typed(rhs, T)`;
    /// computation-expression bang binders (`let!`/`use!`/`and!`) keep the
    /// annotation only in `SynBinding.returnInfo`. In both cases [`Self::expr`]
    /// returns the unwrapped RHS, since the type lives in a sibling node, not on
    /// the expression.
    pub fn return_type(&self) -> Option<Type> {
        child::<BindingReturnInfo>(&self.0).and_then(|ri| ri.ty())
    }
}

impl BindingReturnInfo {
    /// The annotated type — FCS's `SynBindingReturnInfo.typeName`.
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl NamedPat {
    /// The single [`SyntaxKind::IDENT_TOK`] child carrying the LHS identifier.
    /// `None` for the active-pattern form (`let (|Foo|Bar|) = …`), whose name
    /// is an [`ActivePatName`] child instead — read it via
    /// [`Self::active_pat_name`].
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// The active-pattern name when this is a *nullary* active-pattern binding
    /// (`let (|Foo|Bar|) = …`, a `match`-clause head). FCS's maybe-var collapse
    /// turns a nullary active-pattern occurrence into `SynPat.Named` (its
    /// `idText` leads with `|`, so it is var-like); the constituent tokens are
    /// kept under this [`ActivePatName`]. Mutually exclusive with
    /// [`Self::ident`].
    pub fn active_pat_name(&self) -> Option<ActivePatName> {
        child(&self.0)
    }
}

impl OptionalValPat {
    /// The [`SyntaxKind::IDENT_TOK`] child carrying the optional argument's name
    /// — FCS's `SynPat.OptionalVal.ident`. Both plain (`?x`) and backtick-quoted
    /// (`` ?`a b` ``) idents land here; strip the backticks for the `idText`
    /// equivalent. Absent only on the recovery shape (`?` with no following
    /// ident).
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }
}

impl LongIdentPat {
    /// The leading [`LongIdent`] child — FCS's
    /// `SynPat.LongIdent.longDotId`. Single-segment for a bare value/function
    /// head (`let f …`, `Some x`), multi-segment for a dotted union-case path
    /// (`Foo.Bar`, `A.B.C`); read the segments via [`LongIdent::idents`].
    pub fn head(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// The active-pattern name head — `(|Foo|Bar|)`, `(|Foo|_|)` — when this
    /// `LONG_IDENT_PAT` is an active-pattern definition / use. Mutually
    /// exclusive with [`Self::head`]: an active-pattern head carries an
    /// [`ActivePatName`] child instead of a plain [`LongIdent`]. FCS folds the
    /// whole name into the single `idText` of a one-segment `SynLongIdent`;
    /// reconstruct that text from [`ActivePatName::case_tokens`].
    pub fn active_pat_name(&self) -> Option<ActivePatName> {
        child(&self.0)
    }

    /// The explicit value-typar declarations on the head — FCS's
    /// `SynPat.LongIdent.typars` (`SynValTyparDecls option`, field 2). `Some`
    /// for a generic binding head (`let f<'a> …`, `let h<'a> = …`); `None` for
    /// a non-generic head. The `TYPAR_DECLS` node sits between the head
    /// [`LongIdent`] and the argument patterns; read the typars via
    /// [`TyparDecls::typars`]. (The synthetic `noInferredTypars` FCS attaches to
    /// ctor heads has no parser surface here, so it never appears.)
    pub fn typar_decls(&self) -> Option<TyparDecls> {
        child(&self.0)
    }

    /// The curried argument patterns in source order — FCS's
    /// `SynArgPats.Pats` payload. Each arg is a [`Pat::Named`] or
    /// [`Pat::Wildcard`] at this slice; tuple / typed args arrive later.
    ///
    /// Empty for the named-field form (`Case (field = pat)`), whose arguments
    /// live under [`Self::name_pat_pairs`] instead — the two are mutually
    /// exclusive, mirroring FCS's `SynArgPats.Pats` vs `.NamePatPairs`.
    pub fn args(&self) -> impl Iterator<Item = Pat> + '_ {
        children(&self.0)
    }

    /// The named-field argument group — FCS's `SynArgPats.NamePatPairs` — when
    /// this is the `Case (field = pat; …)` form. `None` for the ordinary
    /// curried-args ([`Self::args`]) form.
    pub fn name_pat_pairs(&self) -> Option<NamePatPairs> {
        child(&self.0)
    }
}

impl ActivePatName {
    /// The case-name tokens of the active-pattern name, in source order — the
    /// `Foo`, `Bar` of `(|Foo|Bar|)` (each an [`SyntaxKind::IDENT_TOK`]) and
    /// the trailing `_` of a partial `(|Foo|_|)` (an
    /// [`SyntaxKind::UNDERSCORE_TOK`]). The surrounding `(` / `)` and the `|`
    /// separators are skipped, so FCS's single `idText` is rebuilt as `"|"` +
    /// the case texts joined by `"|"` + `"|"`.
    pub fn case_tokens(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| matches!(t.kind(), SyntaxKind::IDENT_TOK | SyntaxKind::UNDERSCORE_TOK))
    }

    /// The source range of the active-pattern *name* itself — the `|Foo|Bar|`
    /// span from the first `|` to the last `|`, with the surrounding `(` / `)`
    /// **excluded**. This is the range FCS reports for the active-pattern value:
    /// it folds `(|Foo|Bar|)` into a one-segment `SynLongIdent` whose single
    /// `idText` is `"|Foo|Bar|"`, ranged over exactly the bars-and-names span.
    /// `None` for a malformed name with no `|` (recovery).
    pub fn name_range(&self) -> Option<rowan::TextRange> {
        let mut bars = self
            .0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == SyntaxKind::BAR_TOK);
        let first = bars.next()?;
        let last = bars.last().unwrap_or_else(|| first.clone());
        Some(rowan::TextRange::new(
            first.text_range().start(),
            last.text_range().end(),
        ))
    }
}

impl NamePatPairs {
    /// The field pairs in source order — FCS's `SynArgPats.NamePatPairs.pats`
    /// (a `NamePatPairField list`). Each is a [`NamePatPair`]; `SEMI_TOK` /
    /// layout separators and the surrounding `(` / `)` are interleaved as
    /// non-`NamePatPair` children.
    pub fn pairs(&self) -> impl Iterator<Item = NamePatPair> + '_ {
        children(&self.0)
    }
}

impl NamePatPair {
    /// The field name — FCS's `NamePatPairField` long-id, always a single
    /// `ident` here (the grammar is `ident EQUALS parenPattern`, not a `path`).
    /// The leading [`SyntaxKind::IDENT_TOK`] child. `None` only on recovery
    /// from a missing field name.
    pub fn name(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// The field's value pattern — FCS's `NamePatPairField` `pat`. The sole
    /// [`Pat`] child after the `=`. `None` on recovery from a missing value.
    pub fn pat(&self) -> Option<Pat> {
        child(&self.0)
    }
}

impl WildcardPat {
    /// The single [`SyntaxKind::UNDERSCORE_TOK`] child carrying the
    /// wildcard's `_`. Exposed mostly for completeness (and so callers
    /// that want the source range can read the token's span); the
    /// pattern's semantics are fully determined by its kind.
    pub fn underscore(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::UNDERSCORE_TOK)
    }
}

impl ParenPat {
    /// The inner pattern wrapped by the parens. `SynPat.Paren`'s field 0
    /// in FCS terms. Returns `None` only when the parser bailed
    /// mid-production — well-formed input always has an inner pattern.
    pub fn inner(&self) -> Option<Pat> {
        child(&self.0)
    }
}

impl ConstPat {
    /// The first non-trivia child token under this `CONST_PAT`. Mirrors
    /// [`ConstExpr::literal`]: for single-token literals the returned
    /// token *is* the literal and its kind selects the `SynConst`
    /// variant; for the unit form `()` it returns the
    /// [`SyntaxKind::LPAREN_TOK`] and callers dispatch on the kind alone.
    pub fn literal(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| !t.kind().is_trivia())
    }
}

impl NullPat {
    /// The single [`SyntaxKind::NULL_TOK`] child carrying the `null`
    /// keyword. Exposed for source-range recovery; the pattern's
    /// semantics are fully determined by its kind.
    pub fn keyword(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::NULL_TOK)
    }
}

impl TypedPat {
    /// The annotated pattern — FCS's `SynPat.Typed.pat` field. The
    /// `TYPED_PAT` node stores its children as `[<inner-pat>, COLON_TOK,
    /// <type>]`, so the first `Pat` child is the annotated pattern.
    /// Mirrors [`TypedExpr::expr`] on the pattern side.
    pub fn pat(&self) -> Option<Pat> {
        child(&self.0)
    }

    /// The type annotation — FCS's `SynPat.Typed.targetType` field.
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl TuplePat {
    /// The tuple's element patterns in source order — FCS's
    /// `SynPat.Tuple.elementPats` payload. Two-or-more `Pat` children,
    /// with `COMMA_TOK` separators interleaved between them in the
    /// green tree. Mirrors [`TupleExpr::elements`] on the pattern side.
    pub fn elements(&self) -> impl Iterator<Item = Pat> + '_ {
        children(&self.0)
    }

    /// `true` for the `struct (p1, p2)` form — FCS's `SynPat.Tuple.isStruct`.
    /// Recovered from a leading `STRUCT_TOK` child (a regular comma tuple `a, b`
    /// has none). The struct-tuple's parens are children of this node, so it has
    /// no `Paren` wrapper — mirrors [`TupleExpr::is_struct`] on the pattern side.
    pub fn is_struct(&self) -> bool {
        token(&self.0, SyntaxKind::STRUCT_TOK).is_some()
    }
}

impl AsPat {
    /// The left operand — FCS's `SynPat.As.lhsPat` field. The `AS_PAT`
    /// node stores its children as `[<lhs-pat>, AS_TOK, <rhs-pat>]`, so
    /// the first `Pat` child is the left operand.
    pub fn lhs(&self) -> Option<Pat> {
        children(&self.0).next()
    }

    /// The right operand — FCS's `SynPat.As.rhsPat` field. The second
    /// `Pat` child (a `constrPattern`-level pattern).
    pub fn rhs(&self) -> Option<Pat> {
        children(&self.0).nth(1)
    }

    /// The `as` keyword token between the two operands.
    pub fn as_token(&self) -> Option<SyntaxToken> {
        token(&self.0, SyntaxKind::AS_TOK)
    }
}

impl ArrayOrListPat {
    /// `true` for the array form `[| … |]`, `false` for the list form
    /// `[ … ]` — FCS's `SynPat.ArrayOrList.isArray` field. Recovered from
    /// the opener: an [`SyntaxKind::LBRACK_BAR_TOK`] child means array.
    pub fn is_array(&self) -> bool {
        token(&self.0, SyntaxKind::LBRACK_BAR_TOK).is_some()
    }

    /// The element patterns in source order — FCS's
    /// `SynPat.ArrayOrList.elementPats` payload. Each is a full
    /// `parenPattern` (so an element may itself be a `TUPLE_PAT`, `AS_PAT`,
    /// etc.), with `SEMI_TOK` separators interleaved in the green tree.
    pub fn elements(&self) -> impl Iterator<Item = Pat> + '_ {
        children(&self.0)
    }
}

impl RecordPat {
    /// The field patterns in source order — FCS's
    /// `SynPat.Record.fieldPats` (a `NamePatPairField list`). Each is a
    /// [`RecordPatField`]; `SEMI_TOK`/layout separators and the surrounding
    /// `{` / `}` are interleaved as non-`RecordPatField` children.
    pub fn fields(&self) -> impl Iterator<Item = RecordPatField> + '_ {
        children(&self.0)
    }
}

impl RecordPatField {
    /// The field name — FCS's `NamePatPairField` long-id. The leading
    /// [`LongIdent`] child (a `path`, so `{ M.X = p }` is qualified). `None`
    /// only on recovery from a missing field name.
    pub fn name(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// The field's value pattern — FCS's `NamePatPairField` `pat`. The sole
    /// [`Pat`] child after the `=`; the field-name [`LongIdent`] is not
    /// `Pat`-castable, so it is skipped. `None` on recovery from a missing
    /// value.
    pub fn pat(&self) -> Option<Pat> {
        child(&self.0)
    }
}

impl IsInstPat {
    /// The tested type — FCS's `SynPat.IsInst.pat` field (a `SynType`, despite
    /// the field name). The sole [`Type`] child after the `:?` token. `None`
    /// only on recovery from a `:?` with no following type.
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl QuotePat {
    /// The quoted expression — FCS's `SynPat.QuoteExpr.expr`, a full
    /// `SynExpr.Quote`. `QUOTE_PAT` wraps exactly one [`SyntaxKind::QUOTE_EXPR`]
    /// child (the shared quotation parser's node), so this returns it as an
    /// [`Expr::Quote`]. `None` only on recovery from an unclosed quotation that
    /// produced no inner node.
    pub fn inner(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl ListConsPat {
    /// The head pattern — FCS's `SynPat.ListCons.lhsPat`. The `LIST_CONS_PAT`
    /// node stores its children as `[<lhs-pat>, COLON_COLON_TOK, <rhs-pat>]`,
    /// so the first `Pat` child is the head. Mirrors [`AsPat::lhs`].
    pub fn lhs(&self) -> Option<Pat> {
        children(&self.0).next()
    }

    /// The tail pattern — FCS's `SynPat.ListCons.rhsPat`. The second `Pat`
    /// child. Right-associative, so `a :: b :: c` nests another
    /// `LIST_CONS_PAT` here.
    pub fn rhs(&self) -> Option<Pat> {
        children(&self.0).nth(1)
    }

    /// The `::` operator token between the two operands.
    pub fn cons_token(&self) -> Option<SyntaxToken> {
        token(&self.0, SyntaxKind::COLON_COLON_TOK)
    }
}

impl AndsPat {
    /// The conjunction's operand patterns in source order — FCS's
    /// `SynPat.Ands.pats` payload. Two-or-more `Pat` children, with `AMP_TOK`
    /// separators interleaved between them in the green tree. Mirrors
    /// [`TuplePat::elements`]; the `Ands` list is flat (`a & b & c` is one
    /// `Ands` of three, not nested).
    pub fn operands(&self) -> impl Iterator<Item = Pat> + '_ {
        children(&self.0)
    }
}

impl OrPat {
    /// The left operand — FCS's `SynPat.Or.lhsPat`. The `OR_PAT` node stores
    /// its children as `[<lhs-pat>, BAR_TOK, <rhs-pat>]`, so the first `Pat`
    /// child is the left operand. Left-associative, so `A | B | C` nests
    /// another `OR_PAT` here. Mirrors [`AsPat::lhs`].
    pub fn lhs(&self) -> Option<Pat> {
        children(&self.0).next()
    }

    /// The right operand — FCS's `SynPat.Or.rhsPat`. The second `Pat` child.
    pub fn rhs(&self) -> Option<Pat> {
        children(&self.0).nth(1)
    }

    /// The `|` operator token between the two operands.
    pub fn bar_token(&self) -> Option<SyntaxToken> {
        token(&self.0, SyntaxKind::BAR_TOK)
    }
}

impl AttribPat {
    /// The attributed pattern — FCS's `SynPat.Attrib.pat` field. The
    /// `ATTRIB_PAT` node stores its children as `[ATTRIBUTE_LIST+, <inner-pat>]`;
    /// the [`AttributeList`]s are not `Pat`-castable, so the first `Pat` child
    /// is the inner pattern. Mirrors [`TypedPat::pat`] on the attribute side.
    pub fn pat(&self) -> Option<Pat> {
        child(&self.0)
    }

    /// The attribute list(s) prefixing the pattern — FCS's
    /// `SynPat.Attrib.attributes` (`SynAttributes`, a `SynAttributeList list`).
    /// One or more adjacent `[< … >]` groups; reuses the phase-10.5
    /// [`AttributeList`] facade.
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }
}

// `Pat` (the `SynPat` dispatch enum) and its member newtypes are GENERATED:
// see `generated::union_pats`, re-exported above. Their accessors are the
// hand-written `impl *Pat { ... }` blocks below (plan PR D2).

// `Expr` (the `SynExpr` dispatch enum) and its member newtypes are GENERATED:
// see `generated::union_exprs`, re-exported above. Their accessors are the
// hand-written `impl *Expr { ... }` blocks below (plan PR D3).

// `Type` (the `SynType` dispatch enum) and its member newtypes are GENERATED:
// see `generated::union_types`, re-exported above. Their accessors are the
// hand-written `impl *Type { ... }` blocks below (plan PR D1).

impl MeasureLitExpr {
    /// The underlying numeric constant — the sole [`ConstExpr`] child (the
    /// `1.0` in `1.0<m>`), carrying FCS's `SynConst.Measure.constant`.
    pub fn const_expr(&self) -> Option<ConstExpr> {
        child(&self.0)
    }

    /// The measure annotation — the sole [`Measure`] child (the `<m>`).
    pub fn measure(&self) -> Option<Measure> {
        child(&self.0)
    }
}

impl MeasureSeq {
    /// The juxtaposed measure factors, in source order (`m`, `s` in `<m s>`).
    pub fn measures(&self) -> impl Iterator<Item = Measure> + '_ {
        children(&self.0)
    }
}

impl MeasureNamed {
    /// The path of the named measure — the sole [`LongIdent`] child.
    pub fn path(&self) -> Option<LongIdent> {
        child(&self.0)
    }
}

impl MeasureProduct {
    /// The left factor — the first [`Measure`] child.
    pub fn lhs(&self) -> Option<Measure> {
        children(&self.0).next()
    }

    /// The right factor — the second [`Measure`] child.
    pub fn rhs(&self) -> Option<Measure> {
        children(&self.0).nth(1)
    }
}

impl MeasureDivide {
    /// The numerator — the [`Measure`] child *before* the `SLASH_TOK`, or
    /// `None` for the reciprocal `</s>` (where the `/` leads).
    pub fn numerator(&self) -> Option<Measure> {
        let mut seen_slash = false;
        for el in self.0.children_with_tokens() {
            match el {
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::SLASH_TOK => {
                    seen_slash = true;
                }
                rowan::NodeOrToken::Node(n) if Measure::can_cast(n.kind()) => {
                    return if seen_slash { None } else { Measure::cast(n) };
                }
                _ => {}
            }
        }
        None
    }

    /// The denominator — the [`Measure`] child *after* the `SLASH_TOK`.
    pub fn denominator(&self) -> Option<Measure> {
        let mut seen_slash = false;
        for el in self.0.children_with_tokens() {
            match el {
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::SLASH_TOK => {
                    seen_slash = true;
                }
                rowan::NodeOrToken::Node(n) if seen_slash && Measure::can_cast(n.kind()) => {
                    return Measure::cast(n);
                }
                _ => {}
            }
        }
        None
    }
}

impl MeasurePower {
    /// The base measure — the sole [`Measure`] child.
    pub fn base(&self) -> Option<Measure> {
        child(&self.0)
    }

    /// `true` iff the operator is the `^-` spelling — FCS wraps the exponent in
    /// `SynRationalConst.Negate` in that case. Read from the
    /// [`SyntaxKind::MEASURE_POWER_OP_TOK`] child's text (mirrors
    /// [`MeasurePowerType::is_negated`]).
    pub fn is_negated(&self) -> bool {
        token(&self.0, SyntaxKind::MEASURE_POWER_OP_TOK).is_some_and(|t| t.text() == "^-")
    }

    /// The exponent — the sole [`RationalConst`] child.
    pub fn exponent(&self) -> Option<RationalConst> {
        child(&self.0)
    }
}

impl MeasureVar {
    /// The measure variable's name — the [`SyntaxKind::IDENT_TOK`] child (the
    /// `u` of `'u` / `^u`), backticks-as-lexed.
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, SyntaxKind::IDENT_TOK)
    }

    /// `true` for the head-type form `^u` (`TyparStaticReq.HeadType`), `false`
    /// for the quote form `'u` (`TyparStaticReq.None`) — read from the sigil
    /// token kind ([`SyntaxKind::HAT_TOK`] vs [`SyntaxKind::QUOTE_TOK`]).
    pub fn is_head_type(&self) -> bool {
        token(&self.0, SyntaxKind::HAT_TOK).is_some()
    }
}

impl MeasureParen {
    /// The parenthesised measure — the sole [`Measure`] child.
    pub fn inner(&self) -> Option<Measure> {
        child(&self.0)
    }
}

impl ConstExpr {
    /// The first non-trivia child token under this `CONST_EXPR`. For
    /// single-token literals (`INT32_LIT`, `BOOL_LIT`, …) this *is* the
    /// literal token and its kind tells callers which variant they got.
    /// For the multi-token unit literal `()` = `SynConst.Unit` it returns
    /// the [`SyntaxKind::LPAREN_TOK`]; callers dispatch on that kind and
    /// don't read the token's text.
    pub fn literal(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| !t.kind().is_trivia())
    }
}

impl NullExpr {
    /// The single [`SyntaxKind::NULL_TOK`] child carrying the `null`
    /// keyword. Exposed for source-range recovery; the expression's
    /// semantics are fully determined by its kind (FCS's `SynExpr.Null`).
    pub fn keyword(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::NULL_TOK)
    }
}

impl IdentExpr {
    /// The single [`SyntaxKind::IDENT_TOK`] child carrying the
    /// identifier's source text. Backticked idents keep their backticks
    /// in the token text; consumers that want FCS's `Ident.idText`
    /// semantics need to strip them.
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }
}

impl TyparExpr {
    /// The typar name — the `T` in the F# 7 typar expression `'T`
    /// (`SynExpr.Typar`). The single [`SyntaxKind::IDENT_TOK`] child after the
    /// `QUOTE_TOK` sigil. Backticked idents keep their backticks; consumers
    /// wanting FCS's `Ident.idText` strip them. The static-requirement is
    /// always `None` (only the quote sigil reaches this node), so there is no
    /// head-type accessor.
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }
}

impl LongIdentExpr {
    /// The inner [`LongIdent`] child holding the path's idents and dots.
    /// `SynExpr.LongIdent`'s wrapping is purely structural; the body lives
    /// on the inner `SynLongIdent`.
    pub fn long_ident(&self) -> Option<LongIdent> {
        child(&self.0)
    }
}

impl ParenExpr {
    /// The inner expression wrapped by the parens. `SynExpr.Paren`'s field
    /// 0 in FCS terms. Returns `None` only when the parser bailed mid-
    /// production — well-formed input always has an inner expression here.
    pub fn inner(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl TupleExpr {
    /// The tuple's elements in source order. Equivalent to FCS's
    /// `SynExpr.Tuple.exprs` (the `SynExpr list` field). Commas (and
    /// trivia) sit between successive elements in the green tree but
    /// the typed accessor filters them out by only yielding `Expr`
    /// children.
    pub fn elements(&self) -> impl Iterator<Item = Expr> + '_ {
        children(&self.0)
    }

    /// `true` for the `struct (e1, e2)` form — FCS's `SynExpr.Tuple.isStruct`.
    /// Recovered from a leading `STRUCT_TOK` child (a regular `(1, 2)` tuple has
    /// none). The struct-tuple's parens are children of this node, so it has no
    /// `Paren` wrapper.
    pub fn is_struct(&self) -> bool {
        token(&self.0, SyntaxKind::STRUCT_TOK).is_some()
    }
}

impl SequentialExpr {
    /// The sequenced statements in source order. The green tree is
    /// n-ary (a flat list of `Expr` children); this filters out the
    /// `Virtual::BlockSep`-derived zero-width ERRORs and any other
    /// non-expression children to match how FCS's right-leaning
    /// `Sequential(_, _, e1, Sequential(_, _, e2, e3, …), …)` flattens
    /// into a list when projected.
    pub fn statements(&self) -> impl Iterator<Item = Expr> + '_ {
        children(&self.0)
    }
}

/// One member of an [`InterpStringExpr`]'s body, in source order. Mirrors
/// FCS's `SynInterpolatedStringPart`: a [`Fragment`] is a literal text
/// stretch with the surrounding `$"` / `{` / `}` / `"` delimiters
/// (`SynInterpolatedStringPart.String`), a [`Fill`] is a parsed
/// `SynExpr` whose value is spliced in at runtime, plus its optional
/// `: ident` format qualifier (`SynInterpolatedStringPart.FillExpr`).
///
/// [`Fragment`]: InterpStringPart::Fragment
/// [`Fill`]: InterpStringPart::Fill
#[derive(Debug, Clone)]
pub enum InterpStringPart {
    Fragment(SyntaxToken),
    /// A `{ expr }` fill. `expr` is the spliced expression; `qualifier` is the
    /// trailing `: ident` format specifier (`{x:N2}` → `Some("N2")` token),
    /// or `None` for a bare `{x}`. Mirrors `FillExpr of fillExpr * qualifiers:
    /// Ident option`.
    Fill {
        expr: Expr,
        qualifier: Option<SyntaxToken>,
    },
}

impl InterpStringExpr {
    /// The interp-string's body in source order — alternating
    /// `Fragment` (literal text + delimiters) and `Fill` (parsed
    /// expression). For the bare form `$"hello"` this yields a single
    /// `Fragment`; for `$"x={ e }"` it yields `Fragment, Fill, Fragment`.
    ///
    /// The `: ident` format qualifier of a fill (`{x:N2}`) is tokenised at
    /// the `INTERP_STRING_EXPR` level (a `COLON_TOK` then `IDENT_TOK` sibling
    /// following the fill's `Expr` node), so a top-level `IDENT_TOK` is
    /// attached to the [`Fill`] it trails. The `COLON_TOK` is not modelled.
    ///
    /// [`Fill`]: InterpStringPart::Fill
    pub fn parts(&self) -> Vec<InterpStringPart> {
        let mut parts: Vec<InterpStringPart> = Vec::new();
        for el in self.0.children_with_tokens() {
            match el {
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::INTERP_STRING_FRAGMENT => {
                    parts.push(InterpStringPart::Fragment(t));
                }
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::IDENT_TOK => {
                    // The only top-level `IDENT_TOK` in an interp-string is a
                    // fill's format qualifier; attach it to the fill it trails.
                    if let Some(InterpStringPart::Fill {
                        qualifier: slot @ None,
                        ..
                    }) = parts.last_mut()
                    {
                        *slot = Some(t);
                    }
                }
                rowan::NodeOrToken::Node(n) => {
                    if let Some(expr) = Expr::cast(n) {
                        parts.push(InterpStringPart::Fill {
                            expr,
                            qualifier: None,
                        });
                    }
                }
                _ => {}
            }
        }
        parts
    }
}

impl AppExpr {
    /// `true` when this node is the inner `App(NonAtomic, isInfix=true,
    /// op, lhs)` produced by FCS's `mkSynInfix` lowering — i.e. its
    /// [`SyntaxKind`] is [`SyntaxKind::INFIX_APP_EXPR`].
    pub fn is_infix(&self) -> bool {
        self.0.kind() == SyntaxKind::INFIX_APP_EXPR
    }

    /// FCS's `ExprAtomicFlag.Atomic` — `true` for an adjacent application
    /// `f(x)` / bracket indexer `arr[i]` (no whitespace between function and
    /// argument), `false` for the whitespace-separated `f (x)`. Recovered from
    /// the presence of either the
    /// [`SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK`] (paren-app) or
    /// [`SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK`] (bracket-indexer) marker the
    /// parser stamps between the function and argument of an atomic application.
    /// Always `false` for an [`SyntaxKind::INFIX_APP_EXPR`] (FCS lowers infix ops
    /// with `ExprAtomicFlag.NonAtomic`), which never carries a marker.
    pub fn is_atomic(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| {
                matches!(
                    t.kind(),
                    SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK
                        | SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK
                )
            })
    }

    /// `true` when this is an adjacent **bracket indexer** `arr[i]` — carrying
    /// the [`SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK`] marker — rather than a
    /// function call. F# lowers `arr[i]` to a `GetSlice`/`Item` **member lookup**,
    /// not a `SynExpr.App` function application, so a consumer that models
    /// applications (e.g. the sema type inferrer) must treat this as a distinct,
    /// unmodelled construct even though the parser stores it under an
    /// [`SyntaxKind::APP_EXPR`]. The whitespace-separated `f [i]` (application of a
    /// list literal) carries **no** marker and so is *not* a bracket indexer.
    pub fn is_bracket_indexer(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::HIGH_PRECEDENCE_BRACK_APP_TOK)
    }

    /// The function position — FCS's `SynExpr.App.funcExpr`. For a
    /// regular [`SyntaxKind::APP_EXPR`] this is the first `Expr` child
    /// in source order; for an [`SyntaxKind::INFIX_APP_EXPR`] the green
    /// children are stored as `[lhs, op]` (source order), but FCS
    /// records the operator as `funcExpr` — so we return the second
    /// child instead.
    pub fn func(&self) -> Option<Expr> {
        if self.is_infix() {
            children(&self.0).nth(1)
        } else {
            children(&self.0).next()
        }
    }

    /// The argument position — FCS's `SynExpr.App.argExpr`. For a
    /// regular [`SyntaxKind::APP_EXPR`] this is the second `Expr`
    /// child; for an [`SyntaxKind::INFIX_APP_EXPR`] the LHS sits *first*
    /// in source order but FCS records it as `argExpr`, so we return
    /// the first child instead.
    pub fn arg(&self) -> Option<Expr> {
        if self.is_infix() {
            children(&self.0).next()
        } else {
            children(&self.0).nth(1)
        }
    }
}

impl AssignExpr {
    /// The mutation target — the LHS of `<-`. The first `Expr` child in
    /// source order (the `LARROW_TOK` separates it from the value). FCS's
    /// `mkSynAssign` (`SyntaxTreeOps.fs:518`) projects *this* expression's
    /// shape onto the concrete `SynExpr.*Set` variant; the diff normaliser
    /// replays that dispatch.
    pub fn target(&self) -> Option<Expr> {
        children(&self.0).next()
    }

    /// The assigned value — the RHS of `<-` (FCS's `declExprBlock`). The
    /// second `Expr` child in source order. `None` only on a malformed
    /// (parser-bailed) RHS.
    pub fn value(&self) -> Option<Expr> {
        children(&self.0).nth(1)
    }
}

impl DotGetExpr {
    /// The LHS expression being accessed — FCS's `SynExpr.DotGet.expr`.
    /// The first (and only) `Expr` child; the [`LongIdent`] sibling holds
    /// the member path, so it is skipped by the `Expr` cast filter.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The member path — FCS's `SynExpr.DotGet.longDotId`. The [`LongIdent`]
    /// child carries the leading `DOT_TOK` and the member `IDENT_TOK`s;
    /// [`LongIdent::idents`] projects the segment texts (`["Bar"; "Baz"]`
    /// for `(f x).Bar.Baz`).
    pub fn long_ident(&self) -> Option<LongIdent> {
        child(&self.0)
    }
}

impl DynamicExpr {
    /// The LHS expression — FCS's `SynExpr.Dynamic.funcExpr`. The first `Expr`
    /// child in source order (before the `QMARK_TOK`).
    pub fn lhs(&self) -> Option<Expr> {
        children(&self.0).next()
    }

    /// The dynamic argument — FCS's `SynExpr.Dynamic.argExpr`. The second `Expr`
    /// child (after the `QMARK_TOK`): an [`IdentExpr`] for the `a?b` member-name
    /// form, or a [`ParenExpr`] for the `a?(e)` form.
    pub fn arg(&self) -> Option<Expr> {
        children(&self.0).nth(1)
    }
}

impl DotLambdaExpr {
    /// The body of the accessor-function shorthand — FCS's
    /// `SynExpr.DotLambda.expr`, the `atomicExpr` parsed after `_.`. The single
    /// `Expr` child (the `UNDERSCORE_TOK` / `DOT_TOK` siblings are tokens, so
    /// the cast filter skips them). `None` only if the parser bailed before the
    /// body. For `_.Foo.Bar` this is the folded `LongIdentExpr ["Foo"; "Bar"]`.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl DotIndexedGetExpr {
    /// The object being indexed — FCS's `SynExpr.DotIndexedGet.objectExpr`.
    /// The first `Expr` child in source order.
    pub fn object(&self) -> Option<Expr> {
        children(&self.0).next()
    }

    /// The index argument(s) — FCS's `SynExpr.DotIndexedGet.indexArgs`. The
    /// second `Expr` child (a [`TupleExpr`] for the multi-arg `arr.[i, j]`).
    pub fn index(&self) -> Option<Expr> {
        children(&self.0).nth(1)
    }
}

impl LibraryOnlyFieldGetExpr {
    /// The object whose cons-cell field is read — FCS's
    /// `SynExpr.LibraryOnlyUnionCaseFieldGet.expr` (`expr.( :: ).<int>`). The sole
    /// `Expr` child (the `( :: )` name and field number are tokens). `None` only
    /// on malformed input.
    pub fn object(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The field number — FCS's `fieldNum`, a signed `int` — the `INT32_LIT`
    /// after `.( :: ).`. (The union-case name is always the cons operator
    /// `op_ColonColon`, fixed by the grammar, so it has no accessor.) Decodes any
    /// `INT32` spelling: decimal, `0x`/`0o`/`0b` radix, and the int32 `l` suffix.
    /// The 32-bit pattern is reinterpreted as `i32` exactly as FCS does, so a
    /// high-bit literal is negative (`0xFFFFFFFF` → `-1`). `None` only on
    /// malformed input or a literal wider than 32 bits (which FCS rejects too).
    pub fn field_num(&self) -> Option<i32> {
        let tok = token(&self.0, SyntaxKind::INT32_LIT)?;
        let owned = tok.text().replace('_', "");
        // Drop the int32 `l`/`L` suffix if present (never a hex digit, so this is
        // unambiguous), then decode the decimal / `0x`/`0o`/`0b` body.
        let text = owned.strip_suffix(['l', 'L']).unwrap_or(&owned);
        let (radix, digits) = match text.get(..2) {
            Some("0x" | "0X") => (16, &text[2..]),
            Some("0o" | "0O") => (8, &text[2..]),
            Some("0b" | "0B") => (2, &text[2..]),
            _ => (10, text),
        };
        u32::from_str_radix(digits, radix).ok().map(|n| n as i32)
    }
}

impl IndexRangeExpr {
    /// The lower bound — FCS's `SynExpr.IndexRange.expr1`. The `Expr` child
    /// appearing *before* the `..` token, or `None` for an open-lower range
    /// (`..upper`). Found by scanning children in source order and keeping the
    /// last `Expr` seen before the [`SyntaxKind::DOT_DOT_TOK`]. A from-end bound
    /// (`^1`, an [`SyntaxKind::INDEX_FROM_END_EXPR`]) is an ordinary `Expr` child,
    /// returned here / by [`Self::upper`] like any other bound.
    pub fn lower(&self) -> Option<Expr> {
        let mut found = None;
        for el in self.0.children_with_tokens() {
            match el {
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::DOT_DOT_TOK => break,
                rowan::NodeOrToken::Node(n) => {
                    if let Some(e) = Expr::cast(n) {
                        found = Some(e);
                    }
                }
                _ => {}
            }
        }
        found
    }

    /// The upper bound — FCS's `SynExpr.IndexRange.expr2`. The first `Expr`
    /// child *after* the `..` token, or `None` for an open-upper range
    /// (`lower..`).
    pub fn upper(&self) -> Option<Expr> {
        let mut seen_dot_dot = false;
        for el in self.0.children_with_tokens() {
            match el {
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::DOT_DOT_TOK => {
                    seen_dot_dot = true;
                }
                rowan::NodeOrToken::Node(n) if seen_dot_dot => {
                    if let Some(e) = Expr::cast(n) {
                        return Some(e);
                    }
                }
                _ => {}
            }
        }
        None
    }
}

impl IndexFromEndExpr {
    /// The from-end bound expression — FCS's `SynExpr.IndexFromEnd.expr` (the `1`
    /// in `arr.[^1]`). The sole [`Expr`] child; the leading `^`
    /// ([`SyntaxKind::HAT_TOK`]) is a sibling token.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl LongIdent {
    /// The path's identifier tokens in source order. Skips `DOT_TOK`s and
    /// trivia; equivalent to projecting FCS's `SynLongIdent.LongIdent` field
    /// (the `Ident list`). Backticks are preserved in the token text. A `new`
    /// constructor head (phase 9.10b) carries the `new` keyword as its sole
    /// segment (a [`SyntaxKind::NEW_TOK`], text `"new"`), mirroring FCS's
    /// `SynLongIdent(["new"])`, so it is yielded here too.
    pub fn idents(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| matches!(t.kind(), SyntaxKind::IDENT_TOK | SyntaxKind::NEW_TOK))
    }

    /// The active-pattern-name segments embedded in this path — `(|Foo|_|)` used
    /// as an `opName` (FCS's `identExpr: opName`, where `opName` includes the
    /// active-pattern productions). In expression position a path segment can be
    /// an `IDENT_TOK` ([`Self::idents`]), an operator-value's mangled `IDENT_TOK`
    /// (also yielded by [`Self::idents`]), **or** an [`ActivePatName`] node — and
    /// the last cannot be projected as a single token (FCS folds the whole name
    /// into one `SynLongIdent` ident, `"|Foo|_|"`), so it is surfaced here as a
    /// node. Empty for an ordinary ident / operator-value path; non-empty for a
    /// bare `(|Foo|_|)`, a folded `(|Foo|_|).Bar`, or a qualified
    /// `Foo.(|Bar|_|)`. Consumers that resolve a path by its ident tokens
    /// ([`Self::idents`]) must consult this too, lest they silently drop the
    /// active-pattern segment (and mis-read the remaining tokens as the path).
    pub fn active_pat_names(&self) -> impl Iterator<Item = ActivePatName> + '_ {
        children(&self.0)
    }
}

impl IfThenElseExpr {
    /// `SynExpr.IfThenElse.ifExpr` — the guard sub-expression sitting
    /// between `if` and `then`. Resolved *keyword-relatively* as the first
    /// `Expr` appearing before the `THEN_TOK`. Returns `None` only on a
    /// malformed (parser-bailed) tree; well-formed input always has one.
    ///
    /// Keyword-relative rather than positional so error-recovery holes are
    /// attributed to the right branch: a missing `then`/`else` branch leaves a
    /// zero-width `ERROR` (not an `Expr`), and a missing *middle* branch
    /// (`if a then else c`) would otherwise let positional access mistake the
    /// `else` expression for the `then` branch.
    pub fn condition(&self) -> Option<Expr> {
        self.0
            .children_with_tokens()
            .take_while(|el| el.kind() != SyntaxKind::THEN_TOK)
            .filter_map(|el| el.into_node())
            .find_map(Expr::cast)
    }

    /// `SynExpr.IfThenElse.thenExpr` — the `then`-branch expression: the first
    /// `Expr` after `THEN_TOK`, stopping at the else slot (an `ELSE_TOK`, or a
    /// bare `elif` — an `is_elif_node` nested `IF_THEN_ELSE`). The `elif` stop
    /// matters under recovery: with the then-branch missing, the `elif` node is
    /// the *first* `Expr` after `then` (`if a then elif b then c`), so without
    /// it the `elif` would be mistaken for the then-branch. `None` for the
    /// `if c then` recovery hole.
    pub fn then_branch(&self) -> Option<Expr> {
        self.0
            .children_with_tokens()
            .skip_while(|el| el.kind() != SyntaxKind::THEN_TOK)
            .skip(1)
            .take_while(|el| {
                el.kind() != SyntaxKind::ELSE_TOK && !el.as_node().is_some_and(is_elif_node)
            })
            .filter_map(|el| el.into_node())
            .find_map(Expr::cast)
    }

    /// `SynExpr.IfThenElse.elseExpr` — the `else`-branch expression. With an
    /// `ELSE_TOK`, the first `Expr` after it (`None` if the else expression is
    /// a recovery hole, e.g. `if a then b else`). Without one, the bare `elif`
    /// branch — an `is_elif_node` nested `IF_THEN_ELSE` — or `None` for the
    /// plain no-`else` form. Use [`Self::has_else`] to tell "no else" from
    /// "else present, expression missing".
    pub fn else_branch(&self) -> Option<Expr> {
        if token(&self.0, SyntaxKind::ELSE_TOK).is_some() {
            self.0
                .children_with_tokens()
                .skip_while(|el| el.kind() != SyntaxKind::ELSE_TOK)
                .skip(1)
                .filter_map(|el| el.into_node())
                .find_map(Expr::cast)
        } else {
            self.0.children().find(is_elif_node).and_then(Expr::cast)
        }
    }

    /// Whether an `else` (or `elif`) branch is present at all — an `ELSE_TOK`
    /// keyword, or a bare `elif` (`is_elif_node`). Distinguishes the
    /// no-`else` form (`else_branch` absent and this `false`) from an `else`
    /// keyword whose expression failed to parse (this `true`, `else_branch`
    /// `None`), which FCS recovers as `SynExpr.ArbitraryAfterError`.
    pub fn has_else(&self) -> bool {
        token(&self.0, SyntaxKind::ELSE_TOK).is_some()
            || self.0.children().any(|n| is_elif_node(&n))
    }
}

impl FunExpr {
    /// The parameter patterns in source order — FCS's `SynExpr.Lambda`
    /// uses a curried encoding (`Lambda(_, _, [p1], Lambda(_, _, [p2],
    /// body))`) plus a `parsedData = Some(args, body)` cache on the
    /// outermost node; our green tree keeps them flat under one
    /// `FUN_EXPR`, so this is the moral equivalent of `parsedData`'s
    /// first component. Every `Pat` child up to but not including the
    /// body is a parameter — the `RARROW_TOK` separates the two
    /// groups in source order.
    pub fn args(&self) -> impl Iterator<Item = Pat> + '_ {
        children(&self.0)
    }

    /// The lambda body — `SynExpr.Lambda.body` (equivalently
    /// `parsedData`'s second component). Resolved as the `Expr` child
    /// after the `RARROW_TOK`; well-formed input always has one.
    /// Returns `None` only on a malformed (parser-bailed) tree.
    pub fn body(&self) -> Option<Expr> {
        children(&self.0).next()
    }
}

impl MatchExpr {
    /// The scrutinee — FCS's `SynExpr.Match.expr`. It is the sole direct
    /// `Expr` child of `MATCH_EXPR` (clause results are nested inside
    /// [`MatchClause`] nodes, which are not `Expr`-castable). Returns
    /// `None` only on a malformed (parser-bailed) tree.
    pub fn scrutinee(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The clause arms in source order — FCS's `SynExpr.Match.clauses`.
    /// Phase 5.M.1 always emits exactly one.
    pub fn clauses(&self) -> impl Iterator<Item = MatchClause> + '_ {
        children(&self.0)
    }
}

impl MatchBangExpr {
    /// The scrutinee — FCS's `SynExpr.MatchBang.expr`. The sole direct `Expr`
    /// child of `MATCH_BANG_EXPR` (clause results are nested inside
    /// [`MatchClause`] nodes, which are not `Expr`-castable). Same shape as
    /// [`MatchExpr::scrutinee`]. Returns `None` only on a malformed
    /// (parser-bailed) tree.
    pub fn scrutinee(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The clause arms in source order — FCS's `SynExpr.MatchBang.clauses`.
    /// Reuses [`MatchClause`] verbatim (same shape as `match`).
    pub fn clauses(&self) -> impl Iterator<Item = MatchClause> + '_ {
        children(&self.0)
    }
}

impl TryExpr {
    /// The protected body — FCS's `SynExpr.TryWith.tryExpr` /
    /// `SynExpr.TryFinally.tryExpr`. The *leading* direct `Expr` child of
    /// `TRY_EXPR`. In the `with` form it is the sole direct `Expr` child (the
    /// handler results are nested inside [`MatchClause`] nodes, not
    /// `Expr`-castable); in the `finally` form the finally body is a second
    /// direct `Expr` child after it (see [`Self::finally_expr`]). Returns `None`
    /// only on a malformed (parser-bailed) tree.
    pub fn try_expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The handler arms in source order — FCS's `SynExpr.TryWith.withCases`.
    /// Reuses [`MatchClause`] verbatim (the `with`-clause list is FCS's
    /// `patternClauses`, the same non-terminal as `match … with`). Empty for the
    /// `try … finally …` form.
    pub fn with_clauses(&self) -> impl Iterator<Item = MatchClause> + '_ {
        children(&self.0)
    }

    /// `true` iff this is the `try … finally …` form (`SynExpr.TryFinally`),
    /// recovered from the presence of a [`SyntaxKind::FINALLY_TOK`] child —
    /// versus the `try … with …` form (`SynExpr.TryWith`, marked by
    /// [`SyntaxKind::WITH_TOK`] + a [`MatchClause`] list).
    pub fn is_try_finally(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::FINALLY_TOK)
    }

    /// The finally body — FCS's `SynExpr.TryFinally.finallyExpr`. The *trailing*
    /// `Expr` child (after [`SyntaxKind::FINALLY_TOK`]), resolved as the second
    /// `Expr` child via `nth(1)`: the `try … finally …` form has exactly two
    /// direct `Expr` children (body then finally body), while the `try … with …`
    /// form has only one — so this is `None` for the `with` form. Mirrors
    /// [`WhileExpr::body`]'s positional disambiguation.
    pub fn finally_expr(&self) -> Option<Expr> {
        children(&self.0).nth(1)
    }
}

impl WhileExpr {
    /// The loop condition — FCS's `SynExpr.While.whileExpr`. The *leading*
    /// `Expr` child (before `DO_TOK`). Returns `None` only on a malformed
    /// (parser-bailed) tree.
    pub fn cond(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The loop body — FCS's `SynExpr.While.doExpr`, the `Expr` child after
    /// `DO_TOK`. Resolved as the *trailing* `Expr` child (`last()`): with a
    /// present condition and body the node has two `Expr` children, so `last()`
    /// is the body; on a malformed tree missing the body it coincides with the
    /// condition. Mirrors [`MatchClause::result`]'s positional disambiguation.
    pub fn body(&self) -> Option<Expr> {
        children(&self.0).last()
    }
}

impl WhileBangExpr {
    /// The loop condition — FCS's `SynExpr.WhileBang.whileExpr`. The *leading*
    /// `Expr` child (before `DO_TOK`). Same shape as [`WhileExpr::cond`].
    pub fn cond(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The loop body — FCS's `SynExpr.WhileBang.doExpr`, the *trailing* `Expr`
    /// child after `DO_TOK`. Same positional disambiguation as
    /// [`WhileExpr::body`].
    pub fn body(&self) -> Option<Expr> {
        children(&self.0).last()
    }
}

impl ForEachExpr {
    /// The binder pattern — FCS's `SynExpr.ForEach.pat`. The sole `Pat` child
    /// (before `IN_TOK`). Returns `None` only on a malformed (parser-bailed)
    /// tree.
    pub fn pat(&self) -> Option<Pat> {
        child(&self.0)
    }

    /// The enumerable collection — FCS's `SynExpr.ForEach.enumExpr`. The
    /// *leading* `Expr` child (between `IN_TOK` and `DO_TOK`), resolved as the
    /// first `Expr` child. Same positional disambiguation as
    /// [`WhileExpr::cond`].
    pub fn enum_expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The loop body — FCS's `SynExpr.ForEach.bodyExpr`, the `Expr` child after
    /// `DO_TOK`. Resolved as the *trailing* `Expr` child (`last()`): with a
    /// present collection and body the node has two `Expr` children, so
    /// `last()` is the body. Mirrors [`WhileExpr::body`].
    pub fn body(&self) -> Option<Expr> {
        children(&self.0).last()
    }
}

impl ForExpr {
    /// The loop variable — FCS's `SynExpr.For.ident`. The `IDENT_TOK` child.
    pub fn ident(&self) -> Option<SyntaxToken> {
        token(&self.0, SyntaxKind::IDENT_TOK)
    }

    /// The loop direction — FCS's `SynExpr.For.direction` (`true` = ascending
    /// `to`, `false` = descending `downto`). Recovered from which keyword token
    /// the node carries; defaults to `true` on a malformed tree missing both.
    pub fn is_ascending(&self) -> bool {
        token(&self.0, SyntaxKind::DOWNTO_TOK).is_none()
    }

    /// The start bound — FCS's `SynExpr.For.identBody`. The first of the three
    /// `Expr` children (start, end, body), in source order.
    pub fn from_expr(&self) -> Option<Expr> {
        children(&self.0).next()
    }

    /// The end bound — FCS's `SynExpr.For.toBody`. The second `Expr` child.
    pub fn to_expr(&self) -> Option<Expr> {
        children(&self.0).nth(1)
    }

    /// The loop body — FCS's `SynExpr.For.doBody`, the `Expr` child after
    /// `DO_TOK`. The third (and last) `Expr` child.
    pub fn body(&self) -> Option<Expr> {
        children(&self.0).nth(2)
    }
}

impl MatchClause {
    /// The clause pattern — FCS's `SynMatchClause.pat`. The first (and
    /// only) `Pat` child, before the `WHEN_TOK` / `RARROW_TOK`.
    pub fn pat(&self) -> Option<Pat> {
        child(&self.0)
    }

    /// The optional `when` guard — FCS's `SynMatchClause.whenExpr`. Present
    /// only when the clause carries a [`SyntaxKind::WHEN_TOK`]; it is then
    /// the *leading* `Expr` child (between the `when` and the `->`). Without
    /// a guard there is exactly one `Expr` child (the result), so we must
    /// gate on the token to avoid mistaking the result for a guard.
    pub fn guard(&self) -> Option<Expr> {
        token(&self.0, SyntaxKind::WHEN_TOK)?;
        children(&self.0).next()
    }

    /// The clause result — FCS's `SynMatchClause.resultExpr`, the first `Expr`
    /// child *after* the `RARROW_TOK`. Resolved keyword-relatively (not as the
    /// trailing child) so a missing result under recovery is reported as `None`
    /// even when a guard is present: `A when cond ->` has a single `Expr` child
    /// (the guard `cond`), and a positional `last()` would wrongly return it as
    /// the result instead of the empty hole. `None` for the `A ->` /
    /// `A when c ->` recovery holes.
    pub fn result(&self) -> Option<Expr> {
        self.0
            .children_with_tokens()
            .skip_while(|el| el.kind() != SyntaxKind::RARROW_TOK)
            .skip(1)
            .filter_map(|el| el.into_node())
            .find_map(Expr::cast)
    }
}

impl MatchLambdaExpr {
    /// The clause arms in source order — FCS's
    /// `SynExpr.MatchLambda.matchClauses`. Reuses [`MatchClause`] verbatim
    /// (same `pat`/`guard`/`result` shape as `match`); there is no
    /// scrutinee, so the clauses are the only structural children.
    pub fn clauses(&self) -> impl Iterator<Item = MatchClause> + '_ {
        children(&self.0)
    }
}

impl AddressOfExpr {
    /// `true` for the managed-byref `&e` form, `false` for the unmanaged
    /// nativeptr `&&e` form. Mirrors FCS's `SynExpr.AddressOf.isByref`
    /// field. Read from the op-token's kind ([`SyntaxKind::AMP_TOK`] vs
    /// [`SyntaxKind::AMP_AMP_TOK`]) — the parser stamps the matching
    /// token, so the typed-AST flag is just a kind lookup.
    pub fn is_byref(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| matches!(t.kind(), SyntaxKind::AMP_TOK | SyntaxKind::AMP_AMP_TOK))
            .map(|t| t.kind() == SyntaxKind::AMP_TOK)
            .expect("ADDRESS_OF_EXPR must contain an AMP_TOK or AMP_AMP_TOK")
    }

    /// The expression to which the address-of prefix is applied. FCS's
    /// `SynExpr.AddressOf.expr` field. Returns `None` only on a malformed
    /// (parser-bailed) tree; well-formed input always has an inner expr.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl NewExpr {
    /// The constructed type — FCS's `SynExpr.New.targetType` field. The first
    /// (and only) [`Type`] child, before the argument expression. `None` only on
    /// the error-recovery path where `new` was not followed by a type.
    pub fn target_type(&self) -> Option<Type> {
        child(&self.0)
    }

    /// The constructor argument — FCS's `SynExpr.New.expr` field (`()` →
    /// `Const Unit`, `(a, b)` → `Paren(Tuple)`, …). The sole [`Expr`] child; the
    /// target [`Type`] is not `Expr`-castable, so `child` returns the argument.
    /// `None` only on the missing-argument recovery path.
    pub fn arg(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl ObjExpr {
    /// The leading [`NewExpr`] child carrying the object type and the optional
    /// constructor argument. The object expression reuses the `NEW_EXPR` carrier
    /// (`{ new T(args) … }`) so the type/args parsing is shared with the
    /// object-*construction* `new T(args)`; `obj_type`/`arg` reach through it.
    fn base_call(&self) -> Option<NewExpr> {
        child(&self.0)
    }

    /// The implemented object type — FCS's `SynExpr.ObjExpr.objType`. Read from
    /// the leading [`NewExpr`] carrier ([`NewExpr::target_type`]). `None` only on
    /// the error-recovery path where `new` was not followed by a type.
    pub fn obj_type(&self) -> Option<Type> {
        self.base_call()?.target_type()
    }

    /// The constructor argument — the expression half of FCS's
    /// `SynExpr.ObjExpr.argOptions`. `None` for the bare `new T with …` /
    /// `new T` form (no parens, FCS `argOptions = None`); `Some` for
    /// `new T(args) with …` (`()` → `Const Unit`, `(a, b)` → `Paren(Tuple)`).
    pub fn arg(&self) -> Option<Expr> {
        self.base_call()?.arg()
    }

    /// The members of the `with member …` block — FCS's
    /// `SynExpr.ObjExpr.members`. The
    /// [`SyntaxKind::MEMBER_DEFN`]/[`SyntaxKind::GET_SET_MEMBER`] children of the
    /// `OBJ_EXPR` node (the same offside member block as a `type T with member …`
    /// augmentation); empty for the bare `new T` form. **Excludes** the
    /// [`SyntaxKind::INTERFACE_IMPL`] children (FCS keeps those in the separate
    /// `extraImpls` slot, [`Self::extra_impls`]) — [`MemberDefn`] casts an
    /// `INTERFACE_IMPL` as its [`MemberDefn::Interface`] variant, so the member
    /// list is filtered to exclude it.
    pub fn members(&self) -> impl Iterator<Item = MemberDefn> + '_ {
        children::<MemberDefn>(&self.0).filter(|m| !matches!(m, MemberDefn::Interface(_)))
    }

    /// The extra interface implementations — FCS's `SynExpr.ObjExpr.extraImpls`
    /// (`SynInterfaceImpl list`). The [`SyntaxKind::INTERFACE_IMPL`] children of
    /// the `OBJ_EXPR` node, each an `interface I with member …` clause trailing
    /// the (optional) `with member …` block (`{ new Base() with member …
    /// interface I with member … }`). The same `INTERFACE_IMPL` node the
    /// type-definition interface member (9.11b) produces; yielded as the
    /// [`MemberDefn::Interface`] variant so the normaliser reuses the shared
    /// member projection.
    pub fn extra_impls(&self) -> impl Iterator<Item = MemberDefn> + '_ {
        children::<MemberDefn>(&self.0).filter(|m| matches!(m, MemberDefn::Interface(_)))
    }

    /// The value bindings — FCS's `SynExpr.ObjExpr.bindings` (`SynBinding list`).
    /// The [`SyntaxKind::BINDING`] children of the `OBJ_EXPR` node, each an
    /// `X = e` clause of the value-binding form `{ new T() with X = e [and …] }`
    /// (FCS's `objExprBindings: OWITH localBindings OEND`, distinct from the
    /// `with member …` form whose children are [`SyntaxKind::MEMBER_DEFN`]). The
    /// same `BINDING` node a `let` produces, so the normaliser reuses the shared
    /// binding projection; the head binding's leading keyword is supplied as
    /// `Synthetic` and `and`-chained ones as `And` (there is no per-binding
    /// keyword token — the `with` is shared).
    pub fn bindings(&self) -> impl Iterator<Item = Binding> + '_ {
        children::<Binding>(&self.0)
    }
}

impl InferredUpcastExpr {
    /// The coerced expression — FCS's `SynExpr.InferredUpcast.expr` field.
    /// The sole [`Expr`] child of the `INFERRED_UPCAST_EXPR` node (after the
    /// `UPCAST_TOK`). `None` only on the missing-operand recovery path.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl InferredDowncastExpr {
    /// The coerced expression — FCS's `SynExpr.InferredDowncast.expr` field.
    /// The sole [`Expr`] child of the `INFERRED_DOWNCAST_EXPR` node (after the
    /// `DOWNCAST_TOK`). `None` only on the missing-operand recovery path.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl LazyExpr {
    /// The delayed expression — FCS's `SynExpr.Lazy.expr` field. The sole
    /// [`Expr`] child of the `LAZY_EXPR` node (after the `LAZY_TOK`). `None`
    /// only on the missing-operand recovery path.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl AssertExpr {
    /// The asserted expression — FCS's `SynExpr.Assert.expr` field. The sole
    /// [`Expr`] child of the `ASSERT_EXPR` node (after the `ASSERT_TOK`).
    /// `None` only on the missing-operand recovery path.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl FixedExpr {
    /// The pinned expression — FCS's `SynExpr.Fixed.expr` field. The sole
    /// [`Expr`] child of the `FIXED_EXPR` node (after the `FIXED_TOK`). Unlike
    /// `lazy`/`assert`, this operand is a *full* `declExpr` (it folds in infix,
    /// tuple `,`, `:=`, `<-`, `:>`, control-flow), so the child can be any
    /// [`Expr`] variant. `None` only on the missing-operand recovery path.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl TypeAppExpr {
    /// The type-applied head — FCS's `SynExpr.TypeApp.expr` field. The sole
    /// [`Expr`] child (an `Ident` for `f<int>`, a `LongIdent` for
    /// `Seq.empty<int>`), captured before the `<` opener. Returns `None` only
    /// on the error-recovery path where no head was parsed.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The type-argument list — FCS's `SynExpr.TypeApp.typeArgs` field, in
    /// source order. The [`Type`] children between the `<` / `>` punctuation
    /// (`LESS_TOK`, `COMMA_TOK`, `GREATER_TOK` are skipped by `children::<Type>`).
    /// The head is an [`Expr`], not a [`Type`], so it is naturally excluded.
    /// Empty only for the spaced empty form `f< >` (FCS's `LESS GREATER` arm).
    pub fn type_args(&self) -> Vec<Type> {
        children::<Type>(&self.0).collect()
    }
}

impl TypedExpr {
    /// The wrapped expression — FCS's `SynExpr.Typed.expr` field. The
    /// `TYPED_EXPR` node stores its children as `[<inner-expr>, COLON_TOK,
    /// <type>]`, so the first `Expr` child is the annotated value.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The type annotation — FCS's `SynExpr.Typed.targetType` field.
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl TypeTestExpr {
    /// The tested expression — FCS's `SynExpr.TypeTest.expr` field. The
    /// `TYPE_TEST_EXPR` node stores its children as `[<inner-expr>,
    /// COLON_QMARK_TOK, <type>]`, so the first `Expr` child is the value.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The target type — FCS's `SynExpr.TypeTest.targetType` field. The sole
    /// [`Type`] child (`None` only on the missing-type recovery path).
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl UpcastExpr {
    /// The upcast expression — FCS's `SynExpr.Upcast.expr` field. The
    /// `UPCAST_EXPR` node stores its children as `[<inner-expr>,
    /// COLON_GREATER_TOK, <type>]`, so the first `Expr` child is the value.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The target type — FCS's `SynExpr.Upcast.targetType` field. The sole
    /// [`Type`] child (`None` only on the missing-type recovery path).
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl DowncastExpr {
    /// The downcast expression — FCS's `SynExpr.Downcast.expr` field. The
    /// `DOWNCAST_EXPR` node stores its children as `[<inner-expr>,
    /// COLON_QMARK_GREATER_TOK, <type>]`, so the first `Expr` child is the value.
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The target type — FCS's `SynExpr.Downcast.targetType` field. The sole
    /// [`Type`] child (`None` only on the missing-type recovery path).
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl ConsExpr {
    /// The head expression — the `lhs` of `a :: b`. The `CONS_EXPR` node stores
    /// its children as `[<lhs-expr>, COLON_COLON_TOK, <rhs-expr>]`, so the first
    /// `Expr` child is the head. Mirrors [`ListConsPat::lhs`].
    pub fn lhs(&self) -> Option<Expr> {
        children(&self.0).next()
    }

    /// The tail expression — the `rhs` of `a :: b`. The second `Expr` child.
    /// Right-associative, so `a :: b :: c` nests another `CONS_EXPR` here.
    pub fn rhs(&self) -> Option<Expr> {
        children(&self.0).nth(1)
    }

    /// The `::` operator token between the two operands.
    pub fn cons_token(&self) -> Option<SyntaxToken> {
        token(&self.0, SyntaxKind::COLON_COLON_TOK)
    }
}

impl JoinInExpr {
    /// The left operand — FCS's `SynExpr.JoinIn.lhsExpr` (the `join x` of
    /// `join x in xs`). The `JOIN_IN_EXPR` node stores its children as
    /// `[<lhs-expr>, IN_TOK, <rhs-expr>]`, so the first `Expr` child is the
    /// left side. Mirrors [`ConsExpr::lhs`].
    pub fn lhs(&self) -> Option<Expr> {
        children(&self.0).next()
    }

    /// The right operand — FCS's `SynExpr.JoinIn.rhsExpr` (the `xs on (a = b)`
    /// of `join x in xs on (a = b)`). The second `Expr` child.
    pub fn rhs(&self) -> Option<Expr> {
        children(&self.0).nth(1)
    }

    /// The `in` (`JOIN_IN`) operator token between the two operands.
    pub fn in_token(&self) -> Option<SyntaxToken> {
        token(&self.0, SyntaxKind::IN_TOK)
    }
}

impl QuoteExpr {
    /// FCS's `SynExpr.Quote.isRaw` — `true` for the untyped/raw form
    /// `<@@ … @@>`, `false` for the typed `<@ … @>`. Recovered from the
    /// opening [`SyntaxKind::LQUOTE_TOK`]'s source text (`<@@` ⇒ raw); the
    /// closer's raw-ness is not consulted because FCS, on a mismatch,
    /// keeps the opener's flag (see [`SyntaxKind::RQUOTE_TOK`]).
    pub fn is_raw(&self) -> bool {
        token(&self.0, SyntaxKind::LQUOTE_TOK).is_some_and(|t| t.text() == "<@@")
    }

    /// The quoted expression — FCS's `SynExpr.Quote.quotedExpr` field, the
    /// expression between the delimiters.
    pub fn inner(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl InlineIlExpr {
    /// The inline-IL instruction string — FCS's `ilCode` (parsed downstream by
    /// `ParseAssemblyCodeInstructions`, not modelled as a `SynExpr`). A bare
    /// string-literal token child, so it is the sole direct token of a
    /// string-literal kind; string *arguments* sit nested inside a `CONST_EXPR`
    /// and are not surfaced here.
    pub fn instruction(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| {
                matches!(
                    t.kind(),
                    SyntaxKind::STRING_LIT
                        | SyntaxKind::VERBATIM_STRING_LIT
                        | SyntaxKind::TRIPLE_STRING_LIT
                )
            })
    }

    /// The curried value arguments — FCS's `SynExpr.LibraryOnlyILAssembly.args`.
    /// The IL string is a bare token (not an `Expr`) and the type operands are
    /// `Type` children, so the `Expr` children are exactly the arguments.
    pub fn args(&self) -> impl Iterator<Item = Expr> + '_ {
        children(&self.0)
    }

    /// The instruction's type operands in source order — the `type (T)` type
    /// argument (FCS's `typeArgs`) followed by the `: retTy` return type (FCS's
    /// `retTy`). Both are `Type` children; a caller needing to tell them apart
    /// can split on the [`SyntaxKind::COLON_TOK`] that precedes the return type.
    pub fn types(&self) -> impl Iterator<Item = Type> + '_ {
        children(&self.0)
    }
}

impl TraitCallExpr {
    /// The support type — FCS's `SynExpr.TraitCall.supportTys`. The head-type
    /// typar `^a`, a [`VarType`] child (the only direct `Type` child; the member
    /// signature's own types are nested inside the [`MemberSig`]). For the
    /// parenthesised alternatives form `((^a or int) : …)` this is the first
    /// alternative (always a typar); use [`Self::support_types`] for the whole
    /// list. `None` only on malformed input.
    pub fn support_type(&self) -> Option<Type> {
        child(&self.0)
    }

    /// The support alternatives — one [`VarType`] for a single `^a` support;
    /// for the parenthesised alternatives `((^a or int) : …)` the whole
    /// `or`-separated list, whose first operand is a typar and whose later ones
    /// are arbitrary types (FCS's `typarAlts` is `typar (OR appType)*`). The
    /// member signature's own types nest inside the [`MemberSig`], so they are
    /// never direct `Type` children and are not returned here.
    pub fn support_types(&self) -> impl Iterator<Item = Type> + '_ {
        children(&self.0)
    }

    /// The trait member signature — FCS's `SynExpr.TraitCall.traitSig`. The
    /// `MEMBER_SIG` child (the `classMemberSpfn` payload shared with the SRTP
    /// member *constraint*). `None` only on malformed input.
    pub fn member_sig(&self) -> Option<MemberSig> {
        child(&self.0)
    }

    /// The argument expression — FCS's `SynExpr.TraitCall.argExpr`. The sole
    /// direct `Expr` child (the support type is a [`Type`] and the member
    /// signature a [`MemberSig`], neither castable to [`Expr`]). `None` only on
    /// malformed input.
    pub fn arg(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl ComputationExpr {
    /// The body expression between the braces — FCS's
    /// `SynExpr.ComputationExpr.expr` field. (`hasSeqBuilder` carries no
    /// syntactic information at parse and has no accessor.)
    pub fn inner(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl StaticOptimizationExpr {
    /// The fallthrough main expression — FCS's `typedSequentialExpr` before the
    /// first `when` clause (the innermost `optimizedExpr` in FCS's nested
    /// `LibraryOnlyStaticOptimization` fold). The sole *direct* `Expr` child; each
    /// clause's branch expression nests inside its [`StaticOptWhenClause`], so it
    /// is not a direct child here. `None` only on malformed input.
    pub fn main_expr(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// The `when <conditions> = <branch>` clauses in source order — FCS's
    /// `opt_staticOptimizations`.
    pub fn clauses(&self) -> impl Iterator<Item = StaticOptWhenClause> + '_ {
        children(&self.0)
    }
}

impl StaticOptWhenClause {
    /// The `and`-chained conditions — the clause's
    /// `SynStaticOptimizationConstraint` list.
    pub fn conditions(&self) -> impl Iterator<Item = StaticOptCondition> + '_ {
        children(&self.0)
    }

    /// The branch expression after the clause's `=` — FCS's
    /// `typedSequentialExprBlock`. The sole *direct* `Expr` child (the conditions
    /// are [`StaticOptCondition`]s, not `Expr`-castable). `None` only on
    /// malformed input.
    pub fn branch(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl StaticOptCondition {
    /// The subject typar — FCS's `SynStaticOptimizationConstraint` `typar` (`'T`).
    pub fn typar(&self) -> Option<TyparDecl> {
        child(&self.0)
    }

    /// `true` for the bare `'T struct` form (`WhenTyparIsStruct`) — marked by a
    /// `struct` keyword in place of `: <type>`.
    pub fn is_struct(&self) -> bool {
        token(&self.0, SyntaxKind::STRUCT_TOK).is_some()
    }

    /// The constrained-to type — FCS's `WhenTyparTyconEqualsTycon.rhsType`
    /// (`'T : ty`). `None` for the [`Self::is_struct`] form (no type), or on
    /// malformed input.
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl RecordExpr {
    /// The `inherit Base(args)` clause of an inheriting record expression
    /// (`{ inherit Base(args); F = e }`) — FCS's `SynExpr.Record.baseInfo`.
    /// Reuses the [`InheritMember`] node ([`InheritMember::base_type`] /
    /// [`InheritMember::args`]); `None` for an ordinary record.
    pub fn inherit(&self) -> Option<InheritMember> {
        child(&self.0)
    }

    /// The copy-and-update source — FCS's `SynExpr.Record.copyInfo` expression
    /// (`{ src with … }`). Present only in the copy-update form; `None` for a
    /// plain field-list record. It is the sole *direct* `Expr` child
    /// (`RECORD_FIELD` is not `Expr`-castable, so field values don't collide).
    pub fn copy_source(&self) -> Option<Expr> {
        token(&self.0, SyntaxKind::WITH_TOK)?;
        child(&self.0)
    }

    /// The field bindings in source order — FCS's `SynExpr.Record.recordFields`.
    pub fn fields(&self) -> impl Iterator<Item = RecordField> + '_ {
        children(&self.0)
    }
}

impl AnonRecdExpr {
    /// The copy-and-update source — FCS's `SynExpr.AnonRecd.copyInfo`
    /// expression (`{| src with … |}`). Present only in the copy-update form;
    /// `None` for a plain field-list anon-record. The sole *direct* `Expr`
    /// child (`RECORD_FIELD` is not `Expr`-castable). Mirrors
    /// [`RecordExpr::copy_source`].
    pub fn copy_source(&self) -> Option<Expr> {
        token(&self.0, SyntaxKind::WITH_TOK)?;
        child(&self.0)
    }

    /// The field bindings in source order — FCS's
    /// `SynExpr.AnonRecd.recordFields`. Reuses [`RecordField`] (the same
    /// `RECORD_FIELD` green node the regular record uses).
    pub fn fields(&self) -> impl Iterator<Item = RecordField> + '_ {
        children(&self.0)
    }

    /// `true` for the `struct {| … |}` form — FCS's `SynExpr.AnonRecd.isStruct`.
    /// Recovered from a leading `STRUCT_TOK` child.
    pub fn is_struct(&self) -> bool {
        token(&self.0, SyntaxKind::STRUCT_TOK).is_some()
    }
}

impl ArrayOrListExpr {
    /// `true` for the array form `[| … |]` — FCS's `isArray` field (shared by
    /// `SynExpr.ArrayOrList` and `SynExpr.ArrayOrListComputed`). Recovered from
    /// the opener: a `LBRACK_BAR_TOK` (`[|`) ⇒ array, a `LBRACK_TOK` (`[`) ⇒
    /// list.
    pub fn is_array(&self) -> bool {
        token(&self.0, SyntaxKind::LBRACK_BAR_TOK).is_some()
    }

    /// The body expression between the delimiters — FCS's
    /// `SynExpr.ArrayOrListComputed.expr` (the single `sequentialExpr`). `None`
    /// for an empty `[]` / `[||]` (FCS's `SynExpr.ArrayOrList(_, [], _)`), which
    /// has no `Expr` child. A two-or-more-element body is a single
    /// [`SequentialExpr`] child; the brackets are tokens, so the body is the
    /// sole `Expr`-castable child either way.
    pub fn inner(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl RecordField {
    /// The field name — FCS's `SynExprRecordField.fieldName` (a
    /// `RecordFieldName = SynLongIdent * bool`). The [`LongIdent`] child before
    /// `EQUALS_TOK`.
    pub fn field_name(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// The field value — FCS's `SynExprRecordField.expr`, the `Expr` child after
    /// `EQUALS_TOK`. (The field-name [`LongIdent`] is not `Expr`-castable, so
    /// `child` returns the value.)
    pub fn value(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl YieldExpr {
    /// `true` for the `!` ("from") form — FCS's `SynExpr.YieldOrReturnFrom`
    /// (`yield!` / `return!`) vs `SynExpr.YieldOrReturn` (`yield` /
    /// `return`). Recovered from the node kind.
    pub fn is_from(&self) -> bool {
        self.0.kind() == SyntaxKind::YIELD_OR_RETURN_FROM_EXPR
    }

    /// `true` for `yield` / `yield!`, `false` for `return` / `return!`.
    /// This is `flags.0` of FCS's `YieldOrReturn(flags, …)`; recovered from
    /// the keyword token text (`yield`/`yield!` start with `yield`).
    ///
    /// The `for … -> e` comprehension body (a `YIELD_OR_RETURN_EXPR` carrying a
    /// `RARROW_TOK` instead of a keyword) is FCS's `YieldOrReturn((true, false),
    /// …)` — always an implicit `yield` — so the arrow reads as `is_yield`.
    pub fn is_yield(&self) -> bool {
        if token(&self.0, SyntaxKind::RARROW_TOK).is_some() {
            return true;
        }
        token(&self.0, SyntaxKind::YIELD_TOK)
            .or_else(|| token(&self.0, SyntaxKind::YIELD_BANG_TOK))
            .is_some_and(|t| t.text().starts_with("yield"))
    }

    /// The yielded/returned expression — FCS's `expr` field.
    pub fn inner(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl DoBangExpr {
    /// The `do!`-bound expression — FCS's `SynExpr.DoBang.expr` field. The
    /// offside-block scaffolding around it is held as zero-width `ERROR`
    /// tokens, so the first `Expr` child is the body.
    pub fn inner(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl DoExpr {
    /// The `do`-bound expression — FCS's `SynExpr.Do.expr` field. The
    /// offside-block scaffolding around it is held as zero-width `ERROR`
    /// tokens, so the first `Expr` child is the body.
    pub fn inner(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl LetOrUseExpr {
    /// The bindings of this `let!`/`use!`(+`and!`) group — FCS's
    /// `SynLetOrUse.Bindings`. Each is a `BINDING` child; the head is the
    /// `let!`/`use!` binding and any followers are `and!` bindings. The
    /// per-binding leading keyword lives in the preceding `BINDER_TOK`/
    /// `AND_BANG_TOK` token, not in the `BINDING` node.
    pub fn bindings(&self) -> impl Iterator<Item = Binding> + '_ {
        children(&self.0)
    }

    /// The body expression — FCS's `SynLetOrUse.Body`. The binding RHS blocks
    /// are nested inside the `BINDING` children, so the sole *direct* `Expr`
    /// child of this node is the body.
    pub fn body(&self) -> Option<Expr> {
        child(&self.0)
    }

    /// `true` iff the head binder is `use`/`use!` rather than `let`/`let!`. The
    /// head keyword is a `BINDER_TOK` (`let!`/`use!`, the bang form) or a
    /// `LET_TOK` (`let`/`use`, the plain expression-level form); in both cases
    /// the keyword *text* — not the kind — carries the distinction (`use`/`use!`
    /// vs `let`/`let!`), mirroring [`LetDecl::is_use`]. Drives the head
    /// binding's `SynLeadingKeyword` (`Use`/`UseBang` vs `Let`/`LetBang`);
    /// `and`/`and!` followers are always `And`/`AndBang`.
    pub fn is_use(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| matches!(t.kind(), SyntaxKind::BINDER_TOK | SyntaxKind::LET_TOK))
            .is_some_and(|t| t.text().starts_with("use"))
    }

    /// `true` iff this is a `let rec …` group (a [`SyntaxKind::REC_TOK`] child).
    /// Only the plain expression-level form (head `LET_TOK`) can be recursive;
    /// the bang form (`let!`/`use!`) never is, so this is always `false` there.
    /// Drives `SynLetOrUse.IsRecursive`.
    pub fn is_rec(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::REC_TOK)
    }

    /// `true` iff this is the computation-expression bang form (`let!`/`use!`,
    /// head [`SyntaxKind::BINDER_TOK`]) rather than the plain expression-level
    /// `let`/`use` (head [`SyntaxKind::LET_TOK`]). Both share this node; the head
    /// token discriminates. Drives `SynLetOrUse.IsBang` and selects the
    /// per-binding `SynLeadingKeyword` family (`*Bang` vs plain).
    pub fn is_bang(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::BINDER_TOK)
    }

    /// The head keyword token — the [`SyntaxKind::LET_TOK`] (`let`/`use`, plain
    /// form) or [`SyntaxKind::BINDER_TOK`] (`let!`/`use!`, bang form) that
    /// [`Self::is_use`] / [`Self::is_bang`] read. Exposed so a diagnostic can be
    /// anchored at the keyword rather than the whole expression (FCS anchors
    /// `FS0821` at the entire `SynExpr.LetOrUse` range, but the keyword is the
    /// more useful position for a reader).
    pub fn keyword(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| matches!(t.kind(), SyntaxKind::BINDER_TOK | SyntaxKind::LET_TOK))
    }
}

impl LongIdentType {
    /// The inner [`LongIdent`] child holding the dotted-path body. FCS's
    /// `SynType.LongIdent` wraps a `SynLongIdent` directly; we keep the
    /// `LONG_IDENT` node so the same path-projection helpers work for
    /// types as for expressions.
    pub fn long_ident(&self) -> Option<LongIdent> {
        child(&self.0)
    }
}

impl AnonType {
    /// The sole [`SyntaxKind::UNDERSCORE_TOK`] child, exposed so callers
    /// that want the source range can read its span. The type's semantics
    /// are fully determined by its kind.
    pub fn underscore(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::UNDERSCORE_TOK)
    }
}

impl ParenType {
    /// The inner type wrapped by the parens. FCS's `SynType.Paren.innerType`.
    /// Returns `None` only on a malformed (parser-bailed) tree.
    pub fn inner(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl FunType {
    /// The argument type — FCS's `SynType.Fun.argType` field. The
    /// `FUN_TYPE` node stores its children as `[<arg>, RARROW_TOK,
    /// <ret>]`, so the first `Type` child is the argument.
    pub fn arg(&self) -> Option<Type> {
        children(&self.0).next()
    }

    /// The return type — FCS's `SynType.Fun.returnType` field. The
    /// second `Type` child in source order.
    pub fn ret(&self) -> Option<Type> {
        children(&self.0).nth(1)
    }
}

/// One element of a [`TupleType`]'s flat segment path. Mirrors FCS's
/// `SynTupleTypeSegment` (`SyntaxTree.fsi:459`) one-for-one: a type
/// child or a separator. Phase 7.4 models `Type` and `Star`; phase 10.9
/// adds `Slash` (the `/` unit-of-measure tuple form, `float<1/s>`). The
/// projection is flat — `int * string * bool` produces five segments
/// `[Type; Star; Type; Star; Type]`, not nested pairs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TupleSegment {
    Type(Type),
    Star(SyntaxToken),
    Slash(SyntaxToken),
}

impl TupleType {
    /// `true` iff this is a *struct* tuple type — FCS's `SynType.Tuple.isStruct`,
    /// the leading `struct` keyword of `struct (T1 * T2)`. Read off the
    /// [`STRUCT_TOK`](SyntaxKind::STRUCT_TOK) the struct form carries (a plain
    /// `T1 * T2` tuple has none).
    pub fn is_struct(&self) -> bool {
        token(&self.0, SyntaxKind::STRUCT_TOK).is_some()
    }

    /// Walk the `TUPLE_TYPE` node's children-and-tokens in source order,
    /// collecting each `Type` node and `STAR_TOK` / `SLASH_TOK` separator
    /// token as a flat segment list. Trivia tokens between segments are
    /// skipped; their presence in the green tree is preserved separately via
    /// `SyntaxNode::text_range`.
    pub fn segments(&self) -> Vec<TupleSegment> {
        self.0
            .children_with_tokens()
            .filter_map(|el| match el {
                rowan::NodeOrToken::Node(n) => Type::cast(n).map(TupleSegment::Type),
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::STAR_TOK => {
                    Some(TupleSegment::Star(t))
                }
                rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::SLASH_TOK => {
                    Some(TupleSegment::Slash(t))
                }
                rowan::NodeOrToken::Token(_) => None,
            })
            .collect()
    }
}

impl AppType {
    /// `true` for the postfix surface form (`int list`,
    /// `'a option`); `false` for the prefix form (`Foo<int>`,
    /// `Dict<string, int>`). Mirrors FCS's `SynType.App.isPostfix`
    /// field. Discriminates on the presence of a [`SyntaxKind::LESS_TOK`]
    /// child: the parser emits that token only for the prefix form
    /// (between the head and the first arg).
    pub fn is_postfix(&self) -> bool {
        !self
            .0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::LESS_TOK)
    }

    /// The type-constructor head — FCS's `SynType.App.typeName` field.
    /// For postfix `int list` this is the *last* `Type` child (`list`);
    /// for prefix `Foo<int>` it is the *first* `Type` child (`Foo`).
    /// Returns `None` only when the parser bailed mid-production.
    pub fn type_name(&self) -> Option<Type> {
        let types: Vec<_> = children::<Type>(&self.0).collect();
        if self.is_postfix() {
            types.into_iter().next_back()
        } else {
            types.into_iter().next()
        }
    }

    /// The type-argument list — FCS's `SynType.App.typeArgs` field, in
    /// source order. Postfix `int list` has exactly one arg (`int`,
    /// the first and only `Type` child up to but excluding the head);
    /// prefix `Foo<int, string>` has the children after the head in
    /// source order (the `LESS_TOK`, `COMMA_TOK`, and `GREATER_TOK`
    /// punctuation tokens are skipped by `children::<Type>`).
    pub fn type_args(&self) -> Vec<Type> {
        let types: Vec<_> = children::<Type>(&self.0).collect();
        if self.is_postfix() {
            let n = types.len();
            if n <= 1 {
                Vec::new()
            } else {
                types.into_iter().take(n - 1).collect()
            }
        } else {
            types.into_iter().skip(1).collect()
        }
    }
}

impl LongIdentAppType {
    /// The root atomic type the dotted path is applied to — FCS's
    /// `SynType.LongIdentApp.typeName` field. The *first* [`Type`] child:
    /// a `ParenType` (`(int list).Foo`), `AppType` (`Foo<int>.Bar`),
    /// or another `LongIdentAppType` from the enclosing iteration of
    /// the parser's left-associative chain (`(int).Foo<string>.Bar`).
    /// Returns `None` only when the parser bailed mid-production.
    pub fn root(&self) -> Option<Type> {
        children::<Type>(&self.0).next()
    }

    /// The dotted path being applied — FCS's
    /// `SynType.LongIdentApp.longDotId` field. The sole [`LongIdent`]
    /// child, capturing the post-dot path segments (`Foo` for
    /// `(int).Foo`, `Foo.Bar` for `(int).Foo.Bar`). Returns `None`
    /// only when the parser bailed before consuming any ident after
    /// the dot.
    pub fn path(&self) -> Option<LongIdent> {
        child(&self.0)
    }

    /// The type-argument list — FCS's `SynType.LongIdentApp.typeArgs`
    /// field, in source order. The [`Type`] children after the
    /// root, captured between the `<` / `>` punctuation tokens. Empty
    /// for the bare `root.path` shape (no angle brackets in source).
    pub fn type_args(&self) -> Vec<Type> {
        children::<Type>(&self.0).skip(1).collect()
    }
}

impl StaticConstType {
    /// The held literal token — FCS's `SynType.StaticConstant.constant`,
    /// recovered exactly as a [`ConstExpr::literal`]: the first non-trivia
    /// child token. Its [`SyntaxKind`] (`INT32_LIT`, `STRING_LIT`,
    /// `BOOL_LIT`, …) tells callers which `SynConst` variant it carries.
    pub fn literal(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| !t.kind().is_trivia())
    }
}

impl StaticConstExprType {
    /// The held atomic expression — FCS's `SynType.StaticConstantExpr.expr`.
    /// The sole [`Expr`] child after the `const` keyword. Returns `None` only
    /// when the parser bailed (no expression followed `const`).
    pub fn expr(&self) -> Option<Expr> {
        child(&self.0)
    }
}

impl StaticConstNamedType {
    /// The name side — FCS's `SynType.StaticConstantNamed.ident` (itself a
    /// `SynType`, e.g. `LongIdent N`). The *first* [`Type`] child, before the
    /// `=`. Returns `None` only when the parser bailed mid-production.
    pub fn ident(&self) -> Option<Type> {
        children::<Type>(&self.0).next()
    }

    /// The value side — FCS's `SynType.StaticConstantNamed.value` (a
    /// `SynType`, e.g. `StaticConstant 42` or `LongIdent int`). The *second*
    /// [`Type`] child, after the `=`. Returns `None` only when the parser
    /// bailed before the value.
    pub fn value(&self) -> Option<Type> {
        children::<Type>(&self.0).nth(1)
    }
}

impl ArrayType {
    /// The element type — FCS's `SynType.Array.elementType` field. The
    /// sole [`Type`] child of this node, captured from `parse_app_type`'s
    /// shared checkpoint before the suffix wrap. Returns `None` only when
    /// the parser bailed mid-production.
    pub fn element_type(&self) -> Option<Type> {
        child(&self.0)
    }

    /// The array rank — FCS's `SynType.Array.rank` field. Recovered as
    /// `1 + n` where `n` is the count of [`SyntaxKind::COMMA_TOK`]
    /// children between the brackets. FCS rejects rank > 32 (lex.fsl
    /// dispatches only ranks 1–32 of `arrayTypeSuffix`) but this
    /// facade returns whatever the input gives; a diagnostic for
    /// rank > 32 belongs to a later validation pass.
    pub fn rank(&self) -> usize {
        1 + self
            .0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == SyntaxKind::COMMA_TOK)
            .count()
    }
}

impl HashConstraintType {
    /// The constrained inner type — FCS's `SynType.HashConstraint.innerType`
    /// field. The sole [`Type`] child, produced either by the recursive
    /// `parse_atomic_type` call in `parse_atomic_type`'s `Hash` branch (`#T`)
    /// or by the RHS of the `_ :> T` app-type shorthand. Returns `None` only
    /// when the parser bailed mid-production.
    pub fn inner(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl WithNullType {
    /// The inner (nullable) type — FCS's `SynType.WithNull.innerType`
    /// field. The sole [`Type`] child, the `appTypeWithoutNull` parsed
    /// before the `|`. Returns `None` only when the parser bailed
    /// mid-production.
    pub fn inner(&self) -> Option<Type> {
        child(&self.0)
    }

    /// The `|` token — the source of FCS's
    /// `SynTypeWithNullTrivia.BarRange`. Exposed for full-fidelity
    /// callers; `None` only on a malformed tree.
    pub fn bar_token(&self) -> Option<SyntaxToken> {
        token(&self.0, SyntaxKind::BAR_TOK)
    }
}

impl SignatureParameterType {
    /// The parameter's attribute lists — FCS's
    /// `SynType.SignatureParameter.attributes` (`[<InlineIfLambda>] k : …`).
    /// Leading [`AttributeList`] children; empty for an unattributed parameter.
    /// (The attribute idents nest inside the [`AttributeList`] nodes, so they do
    /// not disturb [`Self::name`]'s direct-child `IDENT_TOK` lookup.)
    pub fn attributes(&self) -> impl Iterator<Item = AttributeList> + '_ {
        children(&self.0)
    }

    /// `true` iff the parameter is optional — FCS's
    /// `SynType.SignatureParameter.isOptional`, the leading `?`
    /// ([`QMARK_TOK`](SyntaxKind::QMARK_TOK)) of `?x: int`.
    pub fn is_optional(&self) -> bool {
        token(&self.0, SyntaxKind::QMARK_TOK).is_some()
    }

    /// The parameter name — FCS's `SynType.SignatureParameter.id` (always `Some`
    /// for the named/optional forms this carrier models). The sole
    /// [`IDENT_TOK`](SyntaxKind::IDENT_TOK) before the `:`.
    pub fn name(&self) -> Option<SyntaxToken> {
        token(&self.0, SyntaxKind::IDENT_TOK)
    }

    /// The parameter's value type — FCS's `SynType.SignatureParameter.usedType`,
    /// the `appTypeCanBeNullable` after the `:`. The sole [`Type`] child. `None`
    /// only on a malformed (parser-bailed) tree.
    pub fn value_type(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl ConstrainedType {
    /// The base type the constraints apply to — FCS's
    /// `SynType.WithGlobalConstraints.typeName`. The sole [`Type`] child parsed
    /// before the `when`. `None` only on a malformed (parser-bailed) tree.
    pub fn base(&self) -> Option<Type> {
        child(&self.0)
    }

    /// The trailing `when` constraint group — FCS's
    /// `SynType.WithGlobalConstraints.constraints`, carried in the same
    /// [`TyparConstraints`] node a type-definition header uses. Read the
    /// individual constraints via [`TyparConstraints::constraints`]. `None` for
    /// the `:>` shorthand form ([`Self::subtype`]).
    pub fn constraints(&self) -> Option<TyparConstraints> {
        child(&self.0)
    }

    /// The constraint type of the `'a :> T` subtype shorthand — the second
    /// [`Type`] child, parsed after the `:>`. `Some` only for the shorthand form
    /// (`member` / parameter `'a :> T`); `None` for the explicit `when` form
    /// (whose constraints live under [`Self::constraints`]). FCS folds the
    /// shorthand to `WithGlobalConstraints(base, [WhereTyparSubtypeOfType(base's
    /// typar, T)])`, so the constraint subject is the base typar (see
    /// [`Self::base`]).
    pub fn subtype(&self) -> Option<Type> {
        children::<Type>(&self.0).nth(1)
    }
}

impl IntersectionType {
    /// The head typar — `Some` for FCS's `Intersection(Some typar, …)` (the
    /// `typar AMP …` form, `'T & …`), `None` for `Intersection(None, …)` (the
    /// `hashConstraint AMP …` form, `#A & …`, where the leading `#A` is instead
    /// the first [`Self::types`] element). Recovered from whether the first
    /// `Type` child is a [`VarType`]: the parser opens this node only when the
    /// head was a *bare* typar or a hash constraint, so the leading child is
    /// unambiguously one or the other.
    pub fn typar(&self) -> Option<VarType> {
        match children::<Type>(&self.0).next() {
            Some(Type::Var(v)) => Some(v),
            _ => None,
        }
    }

    /// The intersection operand types — FCS's `Intersection(_, types, …)`.
    /// Every `Type` child except a leading head typar (which lives in
    /// [`Self::typar`]); for the hash-head form the leading `#A` is included
    /// here, matching FCS's `types` list.
    pub fn types(&self) -> impl Iterator<Item = Type> + '_ {
        let skip = usize::from(matches!(
            children::<Type>(&self.0).next(),
            Some(Type::Var(_))
        ));
        children::<Type>(&self.0).skip(skip)
    }
}

impl MeasurePowerType {
    /// The base measure — FCS's `SynType.MeasurePower.baseMeasure` field.
    /// The sole [`Type`] child (`m` in `m^2`, or a typar `'a`/`^a`). Returns
    /// `None` only when the parser bailed mid-production.
    pub fn base(&self) -> Option<Type> {
        child(&self.0)
    }

    /// `true` iff the operator is the `^-` spelling — FCS wraps the exponent
    /// in `SynRationalConst.Negate` in that case. Read from the
    /// [`SyntaxKind::MEASURE_POWER_OP_TOK`] child's text; `false` for the
    /// plain `^` and for a malformed tree with no operator token.
    pub fn is_negated(&self) -> bool {
        token(&self.0, SyntaxKind::MEASURE_POWER_OP_TOK).is_some_and(|t| t.text() == "^-")
    }

    /// The exponent — FCS's `SynType.MeasurePower.exponent` field. The sole
    /// [`RationalConst`] child. Returns `None` only when the parser bailed
    /// before consuming the exponent.
    pub fn exponent(&self) -> Option<RationalConst> {
        child(&self.0)
    }
}

impl RationalConstInteger {
    /// The integer-literal token — the sole [`SyntaxKind::INT32_LIT`] child.
    /// Its text may carry a [`sign_fold`](crate::parser)-merged `-` (e.g.
    /// `-1` in `m^(-1)`). Returns `None` only on a malformed tree.
    pub fn value_token(&self) -> Option<SyntaxToken> {
        token(&self.0, SyntaxKind::INT32_LIT)
    }
}

impl RationalConstRational {
    /// The numerator — the *first* [`SyntaxKind::INT32_LIT`] child (the `1`
    /// in `1/2`).
    pub fn numerator(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::INT32_LIT)
    }

    /// The denominator — the *second* [`SyntaxKind::INT32_LIT`] child (the
    /// `2` in `1/2`).
    pub fn denominator(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == SyntaxKind::INT32_LIT)
            .nth(1)
    }
}

impl RationalConstNegate {
    /// The negated inner rational constant — the sole [`RationalConst`]
    /// child (the `2` in `m^(- 2)`). Returns `None` only on a malformed tree.
    pub fn inner(&self) -> Option<RationalConst> {
        child(&self.0)
    }
}

impl RationalConstParen {
    /// The parenthesised inner rational constant — the sole [`RationalConst`]
    /// child (the `1/2` in `m^(1/2)`). Returns `None` only on a malformed
    /// tree.
    pub fn inner(&self) -> Option<RationalConst> {
        child(&self.0)
    }
}

impl AnonRecdType {
    /// `true` for the struct-anon-recd surface
    /// `struct {| F : int |}` — FCS's `SynType.AnonRecd.isStruct = true`
    /// (`SyntaxTree.fsi:500`). Read from the presence of the leading
    /// [`SyntaxKind::STRUCT_TOK`] child, which the parser only emits in
    /// the struct branch.
    pub fn is_struct(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::STRUCT_TOK)
    }

    /// The field declarations in source order — FCS's
    /// `SynType.AnonRecd.fields` (a `(Ident * SynType) list`). Each
    /// child is an [`AnonRecdTypeField`]; the iterator skips the
    /// surrounding `{|` / `|}` / `;` tokens because they are token
    /// children of the outer node, not [`AstNode`]s.
    pub fn fields(&self) -> impl Iterator<Item = AnonRecdTypeField> + '_ {
        children(&self.0)
    }
}

impl AnonRecdTypeField {
    /// The field-name token — the leading [`SyntaxKind::IDENT_TOK`]
    /// child. Returns `None` only when the parser bailed before
    /// consuming the ident (recovery from a missing field name).
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// The field type — the sole [`Type`] child, produced by the
    /// `parse_type` call inside the parser's
    /// `parse_anon_recd_type_field`. `None` when the parser bailed
    /// mid-production (e.g. missing `:`).
    pub fn ty(&self) -> Option<Type> {
        child(&self.0)
    }
}

impl VarType {
    /// The type-variable identifier, e.g. the `a` in `'a` or the `T` in
    /// `^T`. Mirrors `SynTypar.ident`.
    pub fn ident(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
    }

    /// `true` for the head-typar form `^T` (`TyparStaticReq.HeadType`),
    /// `false` for the plain `'a` form (`TyparStaticReq.None`). Read from
    /// the sigil token kind ([`SyntaxKind::HAT_TOK`] vs
    /// [`SyntaxKind::QUOTE_TOK`]); the parser stamps the matching token so
    /// the typed flag is a pure kind lookup.
    pub fn is_head_type(&self) -> bool {
        self.0
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| matches!(t.kind(), SyntaxKind::QUOTE_TOK | SyntaxKind::HAT_TOK))
            .map(|t| t.kind() == SyntaxKind::HAT_TOK)
            .expect("VAR_TYPE must contain a QUOTE_TOK or HAT_TOK")
    }
}

//! `borzoi-astgen` — the typed-AST facade generator for `borzoi-cst`
//! (`docs/ast-versioning-plan.md` D4, D5, D6).
//!
//! The facade in `crates/cst/src/syntax/` is a large, mostly-mechanical layer:
//! newtype-over-`SyntaxNode` structs, `AstNode` impls, and closed dispatch enums
//! (`Type`, `Expr`, `Pat`, …). The *versioning* work (frozen per-version facades
//! projected from the union, `docs/ast-versioning-plan.md` D2/D4) multiplies that
//! boilerplate across versions, which is exactly the case for codegen.
//!
//! This generator is a **pure-Rust descriptor table** — a
//! [`Node`]/[`Enum`]/[`ValueEnum`] model grouped into [`Category`] units, from
//! which we emit the facade boilerplate. (An ungrammar-based plan was
//! rejected: ungrammar is unmaintained, and our curated short enum-variant
//! names — `Type::Hash` ← `HashConstraintType` — plus multi-kind nodes —
//! `AppExpr` ← `APP_EXPR | INFIX_APP_EXPR` — would need sidecars regardless.)
//! The tables declare **every** type of the union: each node newtype, dispatch
//! enum and value enum, with the value enums' docs.
//!
//! The accessors (positional / token-based / custom-enum-returning) are **not**
//! generated — their bodies stay hand-written in `crates/cst/src/syntax/union.rs`
//! (rust-analyzer's `nodes.rs` + `node_ext.rs` split). astgen reads their
//! signatures back with `syn` ([`accessors`]) and classifies each return type
//! into a closed set of shapes ([`shape`]), failing on one outside it. Those
//! signatures and the tables make up the union's lock, `crates/cst/facades/
//! union.lock` ([`lock`]): the surface one fact per line.
//!
//! From the union's lock and the feature table, [`facade`] derives a lock per
//! language version and generates that version's facade from it: a module
//! owning every node type and enum of the version's surface, whose accessors
//! re-cast the union's.
//!
//! Output is checked in (one file per [`Category`], the facades, under
//! `crates/cst/src/syntax/generated/`, and the locks) and kept honest by a
//! staleness test (`tests/up_to_date.rs`): regenerate with
//! `cargo run -p borzoi-astgen --example generate`. (The generator is an *example*, not
//! a binary, so it does not make root `cargo run` ambiguous with the LSP.)
//!
//! Which language version introduced which kind comes from the feature table,
//! `crates/cst/language-features.tsv`, which [`features`] parses and turns into
//! cst's `LanguageFeature` module as well, so the facades and the parser's
//! gates read one source.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use borzoi_spawn::BoundedCommand;

pub mod accessors;
pub mod facade;
pub mod features;
pub mod lock;
pub mod shape;

use lock::{Line, Lock, Payload};
use shape::{Elem, Names};

/// A typed-AST node: a newtype over `SyntaxNode`.
pub struct Node {
    /// The newtype's Rust name, e.g. `"LongIdentType"`.
    pub name: &'static str,
    /// The `SyntaxKind`s this node's `can_cast` accepts. When empty the single
    /// kind is derived from `name` by [`screaming_snake`]
    /// (`LongIdentType` → `LONG_IDENT_TYPE`); a non-empty list is for the rare
    /// multi-kind node (e.g. `AppExpr` ← `APP_EXPR | INFIX_APP_EXPR`).
    pub kinds: &'static [&'static str],
}

/// A dispatch enum: a closed sum over [`Node`]s, the precise/frozen layer
/// (`docs/ast-versioning-plan.md` D7) consumers exhaustively match.
pub struct Enum {
    /// The enum's Rust name, e.g. `"Type"`.
    pub name: &'static str,
    /// `(variant, node)` pairs in declaration order, e.g.
    /// `("Hash", "HashConstraintType")` — the variant name is curated, not
    /// derived from the node name.
    pub variants: &'static [(&'static str, &'static str)],
}

/// A value enum: a plain Rust enum an accessor computes (rather than a sum
/// over nodes), e.g. `TyparDeclsForm` or `TupleSegment`.
pub struct ValueEnum {
    /// The enum's Rust name.
    pub name: &'static str,
    /// Its doc comment, one entry per line.
    pub doc: &'static [&'static str],
    /// The traits it derives, in `#[derive]` order.
    pub derives: &'static [&'static str],
    /// Its variants, in declaration order.
    pub variants: &'static [ValueVariant],
}

/// One variant of a [`ValueEnum`].
pub struct ValueVariant {
    /// The variant's name.
    pub name: &'static str,
    /// Its doc comment, one entry per line (empty for none).
    pub doc: &'static [&'static str],
    /// What it carries.
    pub fields: Fields,
}

/// What a [`ValueVariant`] carries. Field types are Rust source, each of which
/// must be in astgen's shape set ([`shape::classify`]).
pub enum Fields {
    /// Nothing.
    Unit,
    /// Positional fields.
    Tuple(&'static [&'static str]),
    /// Named fields, `(name, type)`.
    Named(&'static [(&'static str, &'static str)]),
}

/// One generated file: a group of [`Node`]s, the [`Enum`]s over them and the
/// [`ValueEnum`]s beside them.
pub struct Category {
    /// Repo-root-relative output path.
    pub path: &'static str,
    /// One-line description for the generated file's module-doc header.
    pub summary: &'static str,
    /// Member newtypes (every node any of `enums` references must be here).
    pub nodes: &'static [Node],
    /// Dispatch enums over `nodes`.
    pub enums: &'static [Enum],
    /// Value enums: plain Rust enums the hand-written accessors compute
    /// (`TyparDeclsForm`, `TupleSegment`, …), declared here with their docs.
    pub value_enums: &'static [ValueEnum],
}

/// Every category, in swap order (types → patterns → …). Each entry is one
/// checked-in generated file.
pub const CATEGORIES: &[Category] = &[
    Category {
        path: "crates/cst/src/syntax/generated/union_types.rs",
        summary: "The **types** category: the `Type` dispatch enum and its member newtypes.",
        nodes: TYPE_NODES,
        enums: TYPE_ENUMS,
        value_enums: &[],
    },
    Category {
        path: "crates/cst/src/syntax/generated/union_pats.rs",
        summary: "The **patterns** category: the `Pat` dispatch enum and its member newtypes.",
        nodes: PAT_NODES,
        enums: PAT_ENUMS,
        value_enums: &[],
    },
    Category {
        path: "crates/cst/src/syntax/generated/union_exprs.rs",
        summary: "The **expressions** category: the `Expr` dispatch enum and its member newtypes.",
        nodes: EXPR_NODES,
        enums: EXPR_ENUMS,
        value_enums: &[],
    },
    Category {
        path: "crates/cst/src/syntax/generated/union_decls.rs",
        summary: "The **declarations / misc** category: the `ModuleDecl`, `SigDecl`, \
                  `TypeDefnRepr`, `MemberDefn`, `Measure`, and `RationalConst` dispatch \
                  enums and their member newtypes (`ModuleDecl`/`SigDecl` share several).",
        nodes: DECL_NODES,
        enums: DECL_ENUMS,
        value_enums: &[],
    },
    Category {
        path: "crates/cst/src/syntax/generated/union_standalone.rs",
        summary: "The **standalone** nodes, which no category's dispatch enum holds \
                  (`ImplFile`, `Binding`, `LongIdent`, …), the `ParsedInput` enum over \
                  the two file roots, and the value enums the accessors return.",
        nodes: STANDALONE_NODES,
        enums: STANDALONE_ENUMS,
        value_enums: VALUE_ENUMS,
    },
];

/// The *types* category nodes. Order matches the `Type` enum's variants for
/// readable diffs.
pub const TYPE_NODES: &[Node] = &[
    Node {
        name: "LongIdentType",
        kinds: &[],
    },
    Node {
        name: "AnonType",
        kinds: &[],
    },
    Node {
        name: "ParenType",
        kinds: &[],
    },
    Node {
        name: "VarType",
        kinds: &[],
    },
    Node {
        name: "FunType",
        kinds: &[],
    },
    Node {
        name: "TupleType",
        kinds: &[],
    },
    Node {
        name: "AppType",
        kinds: &[],
    },
    Node {
        name: "LongIdentAppType",
        kinds: &[],
    },
    Node {
        name: "ArrayType",
        kinds: &[],
    },
    Node {
        name: "HashConstraintType",
        kinds: &[],
    },
    Node {
        name: "AnonRecdType",
        kinds: &[],
    },
    Node {
        name: "WithNullType",
        kinds: &[],
    },
    Node {
        name: "ConstrainedType",
        kinds: &[],
    },
    Node {
        name: "IntersectionType",
        kinds: &[],
    },
    Node {
        name: "MeasurePowerType",
        kinds: &[],
    },
    Node {
        name: "StaticConstType",
        kinds: &[],
    },
    Node {
        name: "StaticConstExprType",
        kinds: &[],
    },
    Node {
        name: "StaticConstNamedType",
        kinds: &[],
    },
    Node {
        name: "StaticConstNullType",
        kinds: &[],
    },
    Node {
        name: "SignatureParameterType",
        kinds: &[],
    },
];

/// The `Type` dispatch enum. Variant names mirror the hand-written
/// `crate::syntax::Type` so the generated facade is a drop-in.
pub const TYPE_ENUMS: &[Enum] = &[Enum {
    name: "Type",
    variants: &[
        ("LongIdent", "LongIdentType"),
        ("Anon", "AnonType"),
        ("Paren", "ParenType"),
        ("Var", "VarType"),
        ("Fun", "FunType"),
        ("Tuple", "TupleType"),
        ("App", "AppType"),
        ("LongIdentApp", "LongIdentAppType"),
        ("Array", "ArrayType"),
        ("Hash", "HashConstraintType"),
        ("AnonRecd", "AnonRecdType"),
        ("WithNull", "WithNullType"),
        ("Constrained", "ConstrainedType"),
        ("Intersection", "IntersectionType"),
        ("MeasurePower", "MeasurePowerType"),
        ("StaticConst", "StaticConstType"),
        ("StaticConstExpr", "StaticConstExprType"),
        ("StaticConstNamed", "StaticConstNamedType"),
        ("StaticConstNull", "StaticConstNullType"),
        ("SignatureParameter", "SignatureParameterType"),
    ],
}];

/// The *patterns* category nodes. Order matches the `Pat` enum's variants.
pub const PAT_NODES: &[Node] = &[
    Node {
        name: "NamedPat",
        kinds: &[],
    },
    Node {
        name: "LongIdentPat",
        kinds: &[],
    },
    Node {
        name: "WildcardPat",
        kinds: &[],
    },
    Node {
        name: "ParenPat",
        kinds: &[],
    },
    Node {
        name: "ConstPat",
        kinds: &[],
    },
    Node {
        name: "MeasureLitPat",
        kinds: &[],
    },
    Node {
        name: "NullPat",
        kinds: &[],
    },
    Node {
        name: "TypedPat",
        kinds: &[],
    },
    Node {
        name: "TuplePat",
        kinds: &[],
    },
    Node {
        name: "AsPat",
        kinds: &[],
    },
    Node {
        name: "ArrayOrListPat",
        kinds: &[],
    },
    Node {
        name: "RecordPat",
        kinds: &[],
    },
    Node {
        name: "IsInstPat",
        kinds: &[],
    },
    Node {
        name: "ListConsPat",
        kinds: &[],
    },
    Node {
        name: "AndsPat",
        kinds: &[],
    },
    Node {
        name: "OrPat",
        kinds: &[],
    },
    Node {
        name: "AttribPat",
        kinds: &[],
    },
    Node {
        name: "OptionalValPat",
        kinds: &[],
    },
    Node {
        name: "QuotePat",
        kinds: &[],
    },
];

/// The `Pat` dispatch enum. Variant names mirror the hand-written
/// `crate::syntax::Pat`.
pub const PAT_ENUMS: &[Enum] = &[Enum {
    name: "Pat",
    variants: &[
        ("Named", "NamedPat"),
        ("LongIdent", "LongIdentPat"),
        ("Wildcard", "WildcardPat"),
        ("Paren", "ParenPat"),
        ("Const", "ConstPat"),
        ("MeasureLit", "MeasureLitPat"),
        ("Null", "NullPat"),
        ("Typed", "TypedPat"),
        ("Tuple", "TuplePat"),
        ("As", "AsPat"),
        ("ArrayOrList", "ArrayOrListPat"),
        ("Record", "RecordPat"),
        ("IsInst", "IsInstPat"),
        ("ListCons", "ListConsPat"),
        ("Ands", "AndsPat"),
        ("Or", "OrPat"),
        ("Attrib", "AttribPat"),
        ("OptionalVal", "OptionalValPat"),
        ("Quote", "QuotePat"),
    ],
}];

/// The *expressions* category nodes. Order matches the `Expr` enum's variants.
/// `AppExpr` and `YieldExpr` are the two multi-kind nodes (each `can_cast`s two
/// `SyntaxKind`s), so they carry explicit `kinds`.
pub const EXPR_NODES: &[Node] = &[
    Node {
        name: "ConstExpr",
        kinds: &[],
    },
    Node {
        name: "MeasureLitExpr",
        kinds: &[],
    },
    Node {
        name: "NullExpr",
        kinds: &[],
    },
    Node {
        name: "IdentExpr",
        kinds: &[],
    },
    Node {
        name: "LongIdentExpr",
        kinds: &[],
    },
    Node {
        name: "TyparExpr",
        kinds: &[],
    },
    Node {
        name: "ParenExpr",
        kinds: &[],
    },
    Node {
        name: "TupleExpr",
        kinds: &[],
    },
    Node {
        name: "AppExpr",
        kinds: &["APP_EXPR", "INFIX_APP_EXPR"],
    },
    Node {
        name: "DotGetExpr",
        kinds: &[],
    },
    Node {
        name: "DynamicExpr",
        kinds: &[],
    },
    Node {
        name: "DotLambdaExpr",
        kinds: &[],
    },
    Node {
        name: "DotIndexedGetExpr",
        kinds: &[],
    },
    Node {
        name: "TypeAppExpr",
        kinds: &[],
    },
    Node {
        name: "IndexRangeExpr",
        kinds: &[],
    },
    Node {
        name: "IndexFromEndExpr",
        kinds: &[],
    },
    Node {
        name: "AddressOfExpr",
        kinds: &[],
    },
    Node {
        name: "NewExpr",
        kinds: &[],
    },
    Node {
        name: "ObjExpr",
        kinds: &[],
    },
    Node {
        name: "InferredUpcastExpr",
        kinds: &[],
    },
    Node {
        name: "InferredDowncastExpr",
        kinds: &[],
    },
    Node {
        name: "LazyExpr",
        kinds: &[],
    },
    Node {
        name: "AssertExpr",
        kinds: &[],
    },
    Node {
        name: "FixedExpr",
        kinds: &[],
    },
    Node {
        name: "AssignExpr",
        kinds: &[],
    },
    Node {
        name: "TypedExpr",
        kinds: &[],
    },
    Node {
        name: "TypeTestExpr",
        kinds: &[],
    },
    Node {
        name: "UpcastExpr",
        kinds: &[],
    },
    Node {
        name: "DowncastExpr",
        kinds: &[],
    },
    Node {
        name: "ConsExpr",
        kinds: &[],
    },
    Node {
        name: "JoinInExpr",
        kinds: &[],
    },
    Node {
        name: "IfThenElseExpr",
        kinds: &[],
    },
    Node {
        name: "SequentialExpr",
        kinds: &[],
    },
    Node {
        name: "InterpStringExpr",
        kinds: &[],
    },
    Node {
        name: "FunExpr",
        kinds: &[],
    },
    Node {
        name: "QuoteExpr",
        kinds: &[],
    },
    Node {
        name: "InlineIlExpr",
        kinds: &[],
    },
    Node {
        name: "TraitCallExpr",
        kinds: &[],
    },
    Node {
        name: "StaticOptimizationExpr",
        kinds: &[],
    },
    Node {
        name: "LibraryOnlyFieldGetExpr",
        kinds: &[],
    },
    Node {
        name: "DotMissingExpr",
        kinds: &[],
    },
    Node {
        name: "ComputationExpr",
        kinds: &[],
    },
    Node {
        name: "RecordExpr",
        kinds: &[],
    },
    Node {
        name: "AnonRecdExpr",
        kinds: &[],
    },
    Node {
        name: "ArrayOrListExpr",
        kinds: &[],
    },
    Node {
        name: "YieldExpr",
        kinds: &["YIELD_OR_RETURN_EXPR", "YIELD_OR_RETURN_FROM_EXPR"],
    },
    Node {
        name: "DoBangExpr",
        kinds: &[],
    },
    Node {
        name: "DoExpr",
        kinds: &[],
    },
    Node {
        name: "LetOrUseExpr",
        kinds: &[],
    },
    Node {
        name: "MatchExpr",
        kinds: &[],
    },
    Node {
        name: "MatchLambdaExpr",
        kinds: &[],
    },
    Node {
        name: "MatchBangExpr",
        kinds: &[],
    },
    Node {
        name: "WhileExpr",
        kinds: &[],
    },
    Node {
        name: "WhileBangExpr",
        kinds: &[],
    },
    Node {
        name: "ForEachExpr",
        kinds: &[],
    },
    Node {
        name: "ForExpr",
        kinds: &[],
    },
    Node {
        name: "TryExpr",
        kinds: &[],
    },
];

/// The `Expr` dispatch enum. Variant names mirror the hand-written
/// `crate::syntax::Expr`; `App` and `Yield` each cover two kinds (see
/// [`EXPR_NODES`]).
pub const EXPR_ENUMS: &[Enum] = &[Enum {
    name: "Expr",
    variants: &[
        ("Const", "ConstExpr"),
        ("MeasureLit", "MeasureLitExpr"),
        ("Null", "NullExpr"),
        ("Ident", "IdentExpr"),
        ("LongIdent", "LongIdentExpr"),
        ("Typar", "TyparExpr"),
        ("Paren", "ParenExpr"),
        ("Tuple", "TupleExpr"),
        ("App", "AppExpr"),
        ("DotGet", "DotGetExpr"),
        ("Dynamic", "DynamicExpr"),
        ("DotLambda", "DotLambdaExpr"),
        ("DotIndexedGet", "DotIndexedGetExpr"),
        ("TypeApp", "TypeAppExpr"),
        ("IndexRange", "IndexRangeExpr"),
        ("IndexFromEnd", "IndexFromEndExpr"),
        ("AddressOf", "AddressOfExpr"),
        ("New", "NewExpr"),
        ("ObjExpr", "ObjExpr"),
        ("InferredUpcast", "InferredUpcastExpr"),
        ("InferredDowncast", "InferredDowncastExpr"),
        ("Lazy", "LazyExpr"),
        ("Assert", "AssertExpr"),
        ("Fixed", "FixedExpr"),
        ("Assign", "AssignExpr"),
        ("Typed", "TypedExpr"),
        ("TypeTest", "TypeTestExpr"),
        ("Upcast", "UpcastExpr"),
        ("Downcast", "DowncastExpr"),
        ("Cons", "ConsExpr"),
        ("JoinIn", "JoinInExpr"),
        ("IfThenElse", "IfThenElseExpr"),
        ("Sequential", "SequentialExpr"),
        ("InterpString", "InterpStringExpr"),
        ("Fun", "FunExpr"),
        ("Quote", "QuoteExpr"),
        ("InlineIl", "InlineIlExpr"),
        ("TraitCall", "TraitCallExpr"),
        ("StaticOptimization", "StaticOptimizationExpr"),
        ("LibraryOnlyFieldGet", "LibraryOnlyFieldGetExpr"),
        ("DotMissing", "DotMissingExpr"),
        ("Computation", "ComputationExpr"),
        ("Record", "RecordExpr"),
        ("AnonRecd", "AnonRecdExpr"),
        ("ArrayOrList", "ArrayOrListExpr"),
        ("Yield", "YieldExpr"),
        ("DoBang", "DoBangExpr"),
        ("Do", "DoExpr"),
        ("LetOrUse", "LetOrUseExpr"),
        ("Match", "MatchExpr"),
        ("MatchLambda", "MatchLambdaExpr"),
        ("MatchBang", "MatchBangExpr"),
        ("While", "WhileExpr"),
        ("WhileBang", "WhileBangExpr"),
        ("ForEach", "ForEachExpr"),
        ("For", "ForExpr"),
        ("Try", "TryExpr"),
    ],
}];

/// The *declarations / misc* category nodes — the member newtypes of the six
/// dispatch enums in [`DECL_ENUMS`]. `ModuleDecl` and `SigDecl` share five of
/// them (`OpenDecl`, `NestedModuleDecl`, `ModuleAbbrevDecl`, `TypeDefnsDecl`,
/// `ExceptionDefnDecl`); each appears once here. A few kinds are not derivable
/// from the node name and carry explicit `kinds` (`TypeDefnsDecl` → `TYPE_DEFNS`,
/// `ExceptionDefnDecl` → `EXCEPTION_DEFN`, `MemberMethod` → `MEMBER_DEFN`).
pub const DECL_NODES: &[Node] = &[
    // ModuleDecl / SigDecl members.
    Node {
        name: "ExprDecl",
        kinds: &[],
    },
    Node {
        name: "LetDecl",
        kinds: &[],
    },
    Node {
        name: "OpenDecl",
        kinds: &[],
    },
    Node {
        name: "NestedModuleDecl",
        kinds: &[],
    },
    Node {
        name: "ModuleAbbrevDecl",
        kinds: &[],
    },
    Node {
        name: "TypeDefnsDecl",
        kinds: &["TYPE_DEFNS"],
    },
    Node {
        name: "ExceptionDefnDecl",
        kinds: &["EXCEPTION_DEFN"],
    },
    Node {
        name: "ExternDecl",
        kinds: &["EXTERN_DECL"],
    },
    Node {
        name: "ExternArg",
        kinds: &["EXTERN_ARG"],
    },
    Node {
        name: "ExternRet",
        kinds: &["EXTERN_RET"],
    },
    Node {
        name: "AttributesDecl",
        kinds: &[],
    },
    Node {
        name: "HashDirectiveDecl",
        kinds: &["HASH_DIRECTIVE_DECL"],
    },
    Node {
        name: "ValDecl",
        kinds: &[],
    },
    // TypeDefnRepr members.
    Node {
        name: "TypeAbbrev",
        kinds: &[],
    },
    Node {
        name: "RecordRepr",
        kinds: &[],
    },
    Node {
        name: "UnionRepr",
        kinds: &[],
    },
    Node {
        name: "EnumRepr",
        kinds: &[],
    },
    Node {
        name: "ObjectModelRepr",
        kinds: &[],
    },
    Node {
        name: "DelegateRepr",
        kinds: &[],
    },
    Node {
        name: "InlineIlRepr",
        kinds: &[],
    },
    // MemberDefn members.
    Node {
        name: "MemberMethod",
        kinds: &["MEMBER_DEFN"],
    },
    Node {
        name: "MemberLetBindings",
        kinds: &[],
    },
    Node {
        name: "MemberDo",
        kinds: &[],
    },
    Node {
        name: "ValField",
        kinds: &[],
    },
    Node {
        name: "InheritMember",
        kinds: &[],
    },
    Node {
        name: "InterfaceImpl",
        kinds: &[],
    },
    Node {
        name: "GetSetMember",
        kinds: &[],
    },
    Node {
        name: "AutoProperty",
        kinds: &[],
    },
    Node {
        name: "AbstractSlot",
        kinds: &[],
    },
    Node {
        name: "MemberSig",
        kinds: &[],
    },
    // Measure members.
    Node {
        name: "MeasureSeq",
        kinds: &[],
    },
    Node {
        name: "MeasureNamed",
        kinds: &[],
    },
    Node {
        name: "MeasureProduct",
        kinds: &[],
    },
    Node {
        name: "MeasureDivide",
        kinds: &[],
    },
    Node {
        name: "MeasurePower",
        kinds: &[],
    },
    Node {
        name: "MeasureOne",
        kinds: &[],
    },
    Node {
        name: "MeasureAnon",
        kinds: &[],
    },
    Node {
        name: "MeasureVar",
        kinds: &[],
    },
    Node {
        name: "MeasureParen",
        kinds: &[],
    },
    // RationalConst members.
    Node {
        name: "RationalConstInteger",
        kinds: &[],
    },
    Node {
        name: "RationalConstRational",
        kinds: &[],
    },
    Node {
        name: "RationalConstNegate",
        kinds: &[],
    },
    Node {
        name: "RationalConstParen",
        kinds: &[],
    },
];

/// The six declaration / misc dispatch enums. Variant names mirror the
/// hand-written `crate::syntax` enums; `ModuleDecl` and `SigDecl` reuse shared
/// member nodes from [`DECL_NODES`].
pub const DECL_ENUMS: &[Enum] = &[
    Enum {
        name: "ModuleDecl",
        variants: &[
            ("Expr", "ExprDecl"),
            ("Let", "LetDecl"),
            ("Open", "OpenDecl"),
            ("NestedModule", "NestedModuleDecl"),
            ("ModuleAbbrev", "ModuleAbbrevDecl"),
            ("Types", "TypeDefnsDecl"),
            ("Exception", "ExceptionDefnDecl"),
            ("Extern", "ExternDecl"),
            ("Attributes", "AttributesDecl"),
            ("HashDirective", "HashDirectiveDecl"),
        ],
    },
    Enum {
        name: "SigDecl",
        variants: &[
            ("Open", "OpenDecl"),
            ("NestedModule", "NestedModuleDecl"),
            ("ModuleAbbrev", "ModuleAbbrevDecl"),
            ("Val", "ValDecl"),
            ("Types", "TypeDefnsDecl"),
            ("Exception", "ExceptionDefnDecl"),
            ("HashDirective", "HashDirectiveDecl"),
        ],
    },
    Enum {
        name: "TypeDefnRepr",
        variants: &[
            ("Abbrev", "TypeAbbrev"),
            ("Record", "RecordRepr"),
            ("Union", "UnionRepr"),
            ("Enum", "EnumRepr"),
            ("ObjectModel", "ObjectModelRepr"),
            ("Delegate", "DelegateRepr"),
            ("InlineIl", "InlineIlRepr"),
        ],
    },
    Enum {
        name: "MemberDefn",
        variants: &[
            ("Member", "MemberMethod"),
            ("LetBindings", "MemberLetBindings"),
            ("Do", "MemberDo"),
            ("ValField", "ValField"),
            ("Inherit", "InheritMember"),
            ("Interface", "InterfaceImpl"),
            ("GetSetMember", "GetSetMember"),
            ("AutoProperty", "AutoProperty"),
            ("AbstractSlot", "AbstractSlot"),
            ("MemberSig", "MemberSig"),
        ],
    },
    Enum {
        name: "Measure",
        variants: &[
            ("Seq", "MeasureSeq"),
            ("Named", "MeasureNamed"),
            ("Product", "MeasureProduct"),
            ("Divide", "MeasureDivide"),
            ("Power", "MeasurePower"),
            ("One", "MeasureOne"),
            ("Anon", "MeasureAnon"),
            ("Var", "MeasureVar"),
            ("Paren", "MeasureParen"),
        ],
    },
    Enum {
        name: "RationalConst",
        variants: &[
            ("Integer", "RationalConstInteger"),
            ("Rational", "RationalConstRational"),
            ("Negate", "RationalConstNegate"),
            ("Paren", "RationalConstParen"),
        ],
    },
];

/// The *standalone* nodes: newtypes no dispatch enum holds, which accessors
/// return directly. Grouped by the part of the tree they sit in.
pub const STANDALONE_NODES: &[Node] = &[
    // Files and their modules.
    Node {
        name: "ImplFile",
        kinds: &[],
    },
    Node {
        name: "SigFile",
        kinds: &[],
    },
    Node {
        name: "ModuleOrNamespace",
        kinds: &[],
    },
    // Type definitions and their parts.
    Node {
        name: "TypeDefn",
        kinds: &[],
    },
    Node {
        name: "TyparDecls",
        kinds: &[],
    },
    Node {
        name: "TyparDecl",
        kinds: &[],
    },
    Node {
        name: "TyparConstraints",
        kinds: &[],
    },
    Node {
        name: "TyparConstraint",
        kinds: &[],
    },
    Node {
        name: "RecordFieldDecl",
        kinds: &[],
    },
    Node {
        name: "UnionCase",
        kinds: &[],
    },
    Node {
        name: "UnionCaseField",
        kinds: &[],
    },
    Node {
        name: "EnumCase",
        kinds: &[],
    },
    Node {
        name: "ImplicitCtor",
        kinds: &[],
    },
    Node {
        name: "GetSetAccessor",
        kinds: &[],
    },
    Node {
        name: "ValSig",
        kinds: &[],
    },
    Node {
        name: "Binding",
        kinds: &[],
    },
    Node {
        name: "BindingReturnInfo",
        kinds: &[],
    },
    // Parts of patterns.
    Node {
        name: "ActivePatName",
        kinds: &[],
    },
    Node {
        name: "NamePatPairs",
        kinds: &[],
    },
    Node {
        name: "NamePatPair",
        kinds: &[],
    },
    Node {
        name: "RecordPatField",
        kinds: &[],
    },
    // Parts of expressions, types and attributes.
    Node {
        name: "LongIdent",
        kinds: &[],
    },
    Node {
        name: "AttributeList",
        kinds: &[],
    },
    Node {
        name: "Attribute",
        kinds: &[],
    },
    Node {
        name: "AnonRecdTypeField",
        kinds: &[],
    },
    Node {
        name: "RecordField",
        kinds: &[],
    },
    Node {
        name: "MatchClause",
        kinds: &[],
    },
    // The two child nodes of a `STATIC_OPTIMIZATION_EXPR`.
    Node {
        name: "StaticOptWhenClause",
        kinds: &[],
    },
    Node {
        name: "StaticOptCondition",
        kinds: &[],
    },
];

/// The standalone category's one dispatch enum: a parse's root, FCS's
/// `ParsedInput`. A facade's is what `Parse::view` returns.
pub const STANDALONE_ENUMS: &[Enum] = &[Enum {
    name: "ParsedInput",
    variants: &[("ImplFile", "ImplFile"), ("SigFile", "SigFile")],
}];

/// Derives of a fieldless value enum that is also hashed.
const COPY_HASH: &[&str] = &["Debug", "Clone", "Copy", "PartialEq", "Eq", "Hash"];
/// Derives of a fieldless value enum.
const COPY: &[&str] = &["Debug", "Clone", "Copy", "PartialEq", "Eq"];
/// Derives of a value enum whose variants carry tree handles.
const HANDLES: &[&str] = &["Debug", "Clone", "PartialEq", "Eq", "Hash"];

/// A variant that carries nothing and has no doc.
const fn unit(name: &'static str) -> ValueVariant {
    ValueVariant {
        name,
        doc: &[],
        fields: Fields::Unit,
    }
}

/// The value enums the hand-written accessors return, in the order their
/// accessors appear in `crates/cst/src/syntax/union.rs`.
pub const VALUE_ENUMS: &[ValueEnum] = &[
    ValueEnum {
        name: "ModuleOrNamespaceKind",
        doc: &[
            "`SynModuleOrNamespaceKind` — the four shapes a top-level",
            "[`ModuleOrNamespace`] can take. [`Anon`](ModuleOrNamespaceKind::Anon) is",
            "the implicit module wrapping a script-style body (no header keyword);",
            "the other three carry a source-derived `longId` and are introduced by a",
            "`module`/`namespace` header (phase 8.2).",
        ],
        derives: COPY_HASH,
        variants: &[
            ValueVariant {
                name: "Anon",
                doc: &["No header keyword — `SynModuleOrNamespaceKind.AnonModule`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "NamedModule",
                doc: &["`module Foo` — `SynModuleOrNamespaceKind.NamedModule`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "DeclaredNamespace",
                doc: &["`namespace Foo.Bar` — `SynModuleOrNamespaceKind.DeclaredNamespace`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "GlobalNamespace",
                doc: &[
                    "`namespace global` — `SynModuleOrNamespaceKind.GlobalNamespace`",
                    "(empty `longId`).",
                ],
                fields: Fields::Unit,
            },
        ],
    },
    ValueEnum {
        name: "TyparDeclsForm",
        doc: &[
            "The source shape of a [`TyparDecls`] — FCS's `SynTyparDecls` case. The",
            "three declare the same parameters; they differ in where the list sits and",
            "what it may carry (only the postfix list holds a `when` clause).",
        ],
        derives: COPY,
        variants: &[
            ValueVariant {
                name: "Postfix",
                doc: &["`T<'a, 'b>` — `SynTyparDecls.PostfixList`, after the name."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "PrefixList",
                doc: &[
                    "`('a, 'b) T` — `SynTyparDecls.PrefixList`, parenthesised before the",
                    "name (one or more parameters).",
                ],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "SinglePrefix",
                doc: &[
                    "`'a T` — `SynTyparDecls.SinglePrefix`, one bare parameter before the",
                    "name.",
                ],
                fields: Fields::Unit,
            },
        ],
    },
    ValueEnum {
        name: "TyparConstraintKind",
        doc: &[
            "The kind of a [`TyparConstraint`] (phase 9.3b) — the supported",
            "`SynTypeConstraint` variants, read from the constraint's operator/keyword",
            "tokens. The deferred variants (`default`, `enum`/`delegate`, member, and the",
            "self-constrained bare type) have no parser surface, so they are absent.",
        ],
        derives: COPY,
        variants: &[
            ValueVariant {
                name: "SubtypeOf",
                doc: &["`'a :> T` — `WhereTyparSubtypeOfType` (carries [`TyparConstraint::ty`])."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "ValueType",
                doc: &["`'a : struct` — `WhereTyparIsValueType`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "ReferenceType",
                doc: &["`'a : not struct` — `WhereTyparIsReferenceType`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "SupportsNull",
                doc: &["`'a : null` — `WhereTyparSupportsNull`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "NotSupportsNull",
                doc: &["`'a : not null` — `WhereTyparNotSupportsNull`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "Comparable",
                doc: &["`'a : comparison` — `WhereTyparIsComparable`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "Equatable",
                doc: &["`'a : equality` — `WhereTyparIsEquatable`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "Unmanaged",
                doc: &["`'a : unmanaged` — `WhereTyparIsUnmanaged`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "Enum",
                doc: &[
                    "`'a : enum<'b>` — `WhereTyparIsEnum` (carries [`TyparConstraint::type_args`]).",
                ],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "Delegate",
                doc: &[
                    "`'a : delegate<args, ret>` — `WhereTyparIsDelegate` (carries",
                    "[`TyparConstraint::type_args`]).",
                ],
                fields: Fields::Unit,
            },
        ],
    },
    ValueEnum {
        name: "MemberLeading",
        doc: &[
            "Which keyword introduced a [`MemberMethod`](crate::syntax::MemberMethod) —",
            "drives the binding's `SynLeadingKeyword` (and the elided `SynMemberFlags`).",
            "`member` (9.7), `static member` (9.9a), `override` / `default` (9.10a), or",
            "`new` (9.10b, an explicit constructor whose head is the `new` keyword).",
        ],
        derives: COPY,
        variants: &[
            unit("Member"),
            unit("StaticMember"),
            unit("Override"),
            unit("Default"),
            unit("New"),
        ],
    },
    ValueEnum {
        name: "MemberSigLeading",
        doc: &[
            "Which leading keyword introduces a [`MemberSig`](crate::syntax::MemberSig) —",
            "FCS's `SynValSig.trivia.LeadingKeyword` for a `SynMemberSig.Member`.",
        ],
        derives: COPY,
        variants: &[
            ValueVariant {
                name: "Member",
                doc: &["`member M : T`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "Static",
                doc: &[
                    "`static M : T` — a `static`-only member sig (no `member`), FCS's",
                    "`SynLeadingKeyword.Static`. Valid in an SRTP member constraint",
                    "(`^T : (static Zero : ^T)`).",
                ],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "StaticMember",
                doc: &["`static member M : T`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "Abstract",
                doc: &["`abstract M : T`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "AbstractMember",
                doc: &["`abstract member M : T`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "StaticAbstract",
                doc: &["`static abstract M : T`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "StaticAbstractMember",
                doc: &["`static abstract member M : T`."],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "Override",
                doc: &[
                    "`override M : T` — an override member sig, FCS's `SynLeadingKeyword.Override`.",
                ],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "Default",
                doc: &[
                    "`default M : T` — a default member sig, FCS's `SynLeadingKeyword.Default`.",
                ],
                fields: Fields::Unit,
            },
            ValueVariant {
                name: "New",
                doc: &["`new : … -> T` — an explicit constructor sig (name \"new\")."],
                fields: Fields::Unit,
            },
        ],
    },
    ValueEnum {
        name: "AutoPropertyKind",
        doc: &[
            "`SynMemberKind` for an auto-property's `propKind` (phase 9.9c), derived from",
            "the `with get[, set]` clause: a plain `member val X = e`",
            "([`Member`](AutoPropertyKind::Member)), `with get`",
            "([`PropertyGet`](AutoPropertyKind::PropertyGet)), `with set`",
            "([`PropertySet`](AutoPropertyKind::PropertySet) — grammar-accepted, rejected",
            "only later by the checker), or `with get, set`",
            "([`PropertyGetSet`](AutoPropertyKind::PropertyGetSet)).",
        ],
        derives: COPY_HASH,
        variants: &[
            unit("Member"),
            unit("PropertyGet"),
            unit("PropertySet"),
            unit("PropertyGetSet"),
        ],
    },
    ValueEnum {
        name: "ExternCTypeBase",
        doc: &["The base of an extern C type before any C-style suffixes (`&`, `*`, `[]`)."],
        derives: HANDLES,
        variants: &[
            ValueVariant {
                name: "Void",
                doc: &["The C `void` keyword."],
                fields: Fields::Tuple(&["SyntaxToken"]),
            },
            ValueVariant {
                name: "Path",
                doc: &["A path-like C type (`int`, `System.IntPtr`, `byref`, ...)."],
                fields: Fields::Tuple(&["LongIdent"]),
            },
        ],
    },
    ValueEnum {
        name: "ExternCTypeSuffix",
        doc: &["A C-style suffix on an extern C type."],
        derives: HANDLES,
        variants: &[
            ValueVariant {
                name: "Byref",
                doc: &["The managed-byref suffix (`T&`)."],
                fields: Fields::Tuple(&["SyntaxToken"]),
            },
            ValueVariant {
                name: "Pointer",
                doc: &["The native-pointer suffix (`T*` / `void*`)."],
                fields: Fields::Tuple(&["SyntaxToken"]),
            },
            ValueVariant {
                name: "Array",
                doc: &["The array suffix (`T[]`), represented by the opening `[` token."],
                fields: Fields::Tuple(&["SyntaxToken"]),
            },
        ],
    },
    ValueEnum {
        name: "InterpStringPart",
        doc: &[
            "One member of an [`InterpStringExpr`](crate::syntax::InterpStringExpr)'s",
            "body, in source order. Mirrors FCS's `SynInterpolatedStringPart`: a",
            "[`Fragment`] is a literal text stretch with the surrounding `$\"` / `{` /",
            "`}` / `\"` delimiters (`SynInterpolatedStringPart.String`), a [`Fill`] is a",
            "parsed `SynExpr` whose value is spliced in at runtime, plus its optional",
            "`: ident` format qualifier (`SynInterpolatedStringPart.FillExpr`).",
            "",
            "[`Fragment`]: InterpStringPart::Fragment",
            "[`Fill`]: InterpStringPart::Fill",
        ],
        derives: &["Debug", "Clone"],
        variants: &[
            ValueVariant {
                name: "Fragment",
                doc: &[],
                fields: Fields::Tuple(&["SyntaxToken"]),
            },
            ValueVariant {
                name: "Fill",
                doc: &[
                    "A `{ expr }` fill. `expr` is the spliced expression; `qualifier` is the",
                    "trailing `: ident` format specifier (`{x:N2}` → `Some(\"N2\")` token),",
                    "or `None` for a bare `{x}`. Mirrors `FillExpr of fillExpr * qualifiers:",
                    "Ident option`. The token is lossless, so a backtick-quoted qualifier",
                    "keeps the backticks that FCS's `idText` drops.",
                ],
                fields: Fields::Named(&[("expr", "Expr"), ("qualifier", "Option<SyntaxToken>")]),
            },
        ],
    },
    ValueEnum {
        name: "TupleSegment",
        doc: &[
            "One element of a [`TupleType`](crate::syntax::TupleType)'s flat segment",
            "path. Mirrors FCS's `SynTupleTypeSegment` (`SyntaxTree.fsi:459`)",
            "one-for-one: a type child or a separator. Phase 7.4 models `Type` and",
            "`Star`; phase 10.9 adds `Slash` (the `/` unit-of-measure tuple form,",
            "`float<1/s>`). The projection is flat — `int * string * bool` produces five",
            "segments `[Type; Star; Type; Star; Type]`, not nested pairs.",
        ],
        derives: HANDLES,
        variants: &[
            ValueVariant {
                name: "Type",
                doc: &[],
                fields: Fields::Tuple(&["Type"]),
            },
            ValueVariant {
                name: "Star",
                doc: &[],
                fields: Fields::Tuple(&["SyntaxToken"]),
            },
            ValueVariant {
                name: "Slash",
                doc: &[],
                fields: Fields::Tuple(&["SyntaxToken"]),
            },
        ],
    },
];

/// `CamelCase` → `SCREAMING_SNAKE_CASE`: an underscore before every uppercase
/// letter except the first, then upper-case throughout. Faithful for the facade
/// node names (no acronyms / consecutive capitals); `LongIdentType` →
/// `LONG_IDENT_TYPE`. A wrong derivation surfaces as a missing `SyntaxKind`
/// variant at compile time, so the table self-checks.
pub fn screaming_snake(camel: &str) -> String {
    let mut out = String::new();
    for (i, ch) in camel.char_indices() {
        if i > 0 && ch.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}

/// The `SyntaxKind` names a node's `can_cast` accepts — its explicit `kinds`, or
/// the single derived kind.
pub(crate) fn node_kinds(node: &Node) -> Vec<String> {
    if node.kinds.is_empty() {
        vec![screaming_snake(node.name)]
    } else {
        node.kinds.iter().map(|k| (*k).to_string()).collect()
    }
}

fn find_node<'a>(nodes: &'a [Node], name: &str) -> &'a Node {
    nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("enum references unknown node {name:?}"))
}

/// Emit a node newtype + its `AstNode` impl. The tuple field is `pub(crate)` so
/// the hand-written accessors (in `crate::syntax`) can reach `self.0`.
fn gen_node(node: &Node, out: &mut String) {
    let kinds = node_kinds(node);
    let can_cast = kinds
        .iter()
        .map(|k| format!("SyntaxKind::{k}"))
        .collect::<Vec<_>>()
        .join(" | ");
    out.push_str(&format!(
        "#[derive(Debug, Clone, PartialEq, Eq, Hash)]\n\
         pub struct {name}(pub(crate) SyntaxNode);\n\n\
         impl AstNode for {name} {{\n\
         fn can_cast(kind: SyntaxKind) -> bool {{ matches!(kind, {can_cast}) }}\n\
         fn cast(node: SyntaxNode) -> Option<Self> {{\n\
         if Self::can_cast(node.kind()) {{ Some(Self(node)) }} else {{ None }}\n\
         }}\n\
         fn syntax(&self) -> &SyntaxNode {{ &self.0 }}\n\
         }}\n\n",
        name = node.name,
    ));
}

/// Emit a dispatch enum + its `AstNode` impl (`can_cast`/`cast`/`syntax`).
fn gen_enum(e: &Enum, nodes: &[Node], out: &mut String) {
    gen_enum_impl(e.name, e.variants, nodes, out);
}

/// Emit a dispatch enum `name` over the given `variants` (`(variant, node)`
/// pairs), reading each node's kinds from the tables.
fn gen_enum_impl(name: &str, variants: &[(&str, &str)], nodes: &[Node], out: &mut String) {
    let variants: Vec<(&str, &str, Vec<String>)> = variants
        .iter()
        .map(|(variant, node)| (*variant, *node, node_kinds(find_node(nodes, node))))
        .collect();
    gen_enum_with_kinds(name, &variants, out);
}

/// Emit a dispatch enum `name` whose variants are `(variant, node, the kinds
/// the node casts from)`, with its `AstNode` impl. The `cast` arm for any kind
/// not in `variants` falls through to `None`, which is how a facade's
/// narrower enum reads a node outside its surface.
pub(crate) fn gen_enum_with_kinds(
    name: &str,
    variants: &[(&str, &str, Vec<String>)],
    out: &mut String,
) {
    let mut variant_decls = String::new();
    let mut all_kinds: Vec<String> = Vec::new();
    let mut cast_arms = String::new();
    let mut syntax_arms = String::new();
    for (variant, node_name, kinds) in variants {
        variant_decls.push_str(&format!("    {variant}({node_name}),\n"));
        syntax_arms.push_str(&format!(
            "            {name}::{variant}(n) => n.syntax(),\n"
        ));
        for kind in kinds.iter().cloned() {
            cast_arms.push_str(&format!(
                "            SyntaxKind::{kind} => {node_name}::cast(node).map({name}::{variant}),\n",
            ));
            all_kinds.push(kind);
        }
    }
    let can_cast = all_kinds
        .iter()
        .map(|k| format!("SyntaxKind::{k}"))
        .collect::<Vec<_>>()
        .join(" | ");
    out.push_str(&format!(
        "#[derive(Debug, Clone, PartialEq, Eq, Hash)]\n\
         pub enum {name} {{\n{variant_decls}}}\n\n\
         impl AstNode for {name} {{\n\
         fn can_cast(kind: SyntaxKind) -> bool {{ matches!(kind, {can_cast}) }}\n\
         fn cast(node: SyntaxNode) -> Option<Self> {{\n\
         match node.kind() {{\n{cast_arms}            _ => None,\n}}\n\
         }}\n\
         fn syntax(&self) -> &SyntaxNode {{\n\
         match self {{\n{syntax_arms}}}\n\
         }}\n\
         }}\n",
    ));
}

/// Push `doc` as `///` lines indented by `indent`.
fn push_doc(doc: &[&str], indent: &str, out: &mut String) {
    for line in doc {
        if line.is_empty() {
            out.push_str(&format!("{indent}///\n"));
        } else {
            out.push_str(&format!("{indent}/// {line}\n"));
        }
    }
}

/// Every field type of a value enum's variants, as written in the table.
fn field_types(v: &ValueEnum) -> impl Iterator<Item = &'static str> + '_ {
    v.variants.iter().flat_map(|variant| match variant.fields {
        Fields::Unit => Vec::new(),
        Fields::Tuple(types) => types.to_vec(),
        Fields::Named(fields) => fields.iter().map(|(_, ty)| *ty).collect(),
    })
}

/// Emit a value enum with its docs.
fn gen_value_enum(v: &ValueEnum, out: &mut String) {
    push_doc(v.doc, "", out);
    out.push_str(&format!(
        "#[derive({})]\npub enum {} {{\n",
        v.derives.join(", "),
        v.name
    ));
    for variant in v.variants {
        push_doc(variant.doc, "    ", out);
        match variant.fields {
            Fields::Unit => out.push_str(&format!("    {},\n", variant.name)),
            Fields::Tuple(types) => {
                out.push_str(&format!("    {}({}),\n", variant.name, types.join(", ")));
            }
            Fields::Named(fields) => {
                let fields: Vec<String> = fields
                    .iter()
                    .map(|(name, ty)| format!("{name}: {ty}"))
                    .collect();
                out.push_str(&format!(
                    "    {} {{ {} }},\n",
                    variant.name,
                    fields.join(", ")
                ));
            }
        }
    }
    out.push_str("}\n\n");
}

/// The category declaring the node, dispatch enum or value enum `name`.
fn home_of(name: &str) -> &'static Category {
    CATEGORIES
        .iter()
        .find(|c| {
            c.nodes.iter().any(|n| n.name == name)
                || c.enums.iter().any(|e| e.name == name)
                || c.value_enums.iter().any(|v| v.name == name)
        })
        .unwrap_or_else(|| panic!("`{name}` is in no category"))
}

/// The `use` lines a category's file needs: the substrate, and the types its
/// value enums carry that other categories declare.
fn category_imports(category: &Category) -> String {
    let names = table_names();
    let mut substrate = vec!["AstNode", "SyntaxKind", "SyntaxNode"];
    let mut foreign: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for v in category.value_enums {
        for ty in field_types(v) {
            let shape = shape::classify_str(ty, &names)
                .unwrap_or_else(|e| panic!("a field of value enum `{}`: {e}", v.name));
            for elem in shape.elem().leaves() {
                match elem {
                    Elem::Token if !substrate.contains(&"SyntaxToken") => {
                        substrate.push("SyntaxToken");
                    }
                    Elem::Node(n) | Elem::Enum(n) | Elem::Value(n) => {
                        let home = home_of(n);
                        if home.path != category.path {
                            foreign
                                .entry(module_of(home.path))
                                .or_default()
                                .insert(n.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut out = format!("use crate::syntax::{{{}}};\n", substrate.join(", "));
    for (module, names) in foreign {
        let names: Vec<String> = names.into_iter().collect();
        out.push_str(&format!("use super::{module}::{{{}}};\n", names.join(", ")));
    }
    out.push('\n');
    out
}

/// Generate one [`Category`]'s union facade file, `rustfmt`-stable.
pub fn generate(category: &Category) -> String {
    let mut out = format!(
        "//! GENERATED by `tools/astgen` — do not edit by hand.\n\
         //! Regenerate with `cargo run -p borzoi-astgen --example generate`; kept honest\n\
         //! by borzoi-astgen's `tests/up_to_date.rs` staleness test.\n\
         //!\n\
         //! {summary}\n\
         //!\n\
         //! Boilerplate only — bespoke accessors stay hand-written in\n\
         //! `crate::syntax`, re-exported from there as the real facade (plan PR D).\n\n\
         {imports}",
        summary = category.summary,
        imports = category_imports(category),
    );
    for node in category.nodes {
        gen_node(node, &mut out);
    }
    for e in category.enums {
        gen_enum(e, category.nodes, &mut out);
    }
    for v in category.value_enums {
        gen_value_enum(v, &mut out);
    }
    reformat(&out)
}

/// The type names astgen's tables declare, by what each one is.
pub fn table_names() -> Names {
    let mut names = Names::default();
    for category in CATEGORIES {
        names
            .nodes
            .extend(category.nodes.iter().map(|n| n.name.to_string()));
        names
            .enums
            .extend(category.enums.iter().map(|e| e.name.to_string()));
        names
            .values
            .extend(category.value_enums.iter().map(|v| v.name.to_string()));
    }
    names
}

/// The repo-relative path of the union's lock.
pub const UNION_LOCK_PATH: &str = "crates/cst/facades/union.lock";

/// The union's surface: every node, dispatch enum and value enum in astgen's
/// tables, and every accessor in `crates/cst/src/syntax/union.rs`. Panics,
/// listing them, on any reason the accessors cannot be read
/// ([`accessors::read_accessors`]) or the result is incoherent
/// ([`lock::Lock::check`]).
pub fn union_lock() -> Lock {
    lock_from(accessors::UNION_SOURCE)
}

/// [`union_lock`] over the given text of `union.rs`.
pub fn lock_from(union_source: &str) -> Lock {
    let names = table_names();
    let mut lines = BTreeSet::new();
    for category in CATEGORIES {
        for node in category.nodes {
            for kind in node_kinds(node) {
                lines.insert(Line::Node {
                    node: node.name.to_string(),
                    kind,
                });
            }
        }
        for e in category.enums {
            for (variant, node) in e.variants {
                lines.insert(Line::Enum {
                    name: e.name.to_string(),
                    variant: variant.to_string(),
                    node: node.to_string(),
                });
            }
        }
        for v in category.value_enums {
            for derive in v.derives {
                lines.insert(Line::Derive {
                    name: v.name.to_string(),
                    trait_: derive.to_string(),
                });
            }
            for variant in v.variants {
                let field = |ty: &str| {
                    shape::classify_str(ty, &names).unwrap_or_else(|e| {
                        panic!("a field of `{}::{}`: {e}", v.name, variant.name)
                    })
                };
                let payload = match variant.fields {
                    Fields::Unit => Payload::Unit,
                    Fields::Tuple(types) => {
                        Payload::Tuple(types.iter().map(|t| field(t)).collect())
                    }
                    Fields::Named(fields) => Payload::Named(
                        fields
                            .iter()
                            .map(|(name, ty)| (name.to_string(), field(ty)))
                            .collect(),
                    ),
                };
                lines.insert(Line::Value {
                    name: v.name.to_string(),
                    variant: variant.name.to_string(),
                    payload,
                });
            }
        }
    }
    let read = accessors::read_accessors(union_source, &names).unwrap_or_else(|errors| {
        panic!(
            "{} cannot be read for the lock:\n  {}",
            accessors::UNION_PATH,
            errors.join("\n  ")
        )
    });
    for accessor in read {
        lines.insert(Line::Accessor {
            owner: accessor.owner,
            name: accessor.name,
            shape: accessor.shape,
            delegate: None,
        });
    }
    let lock = Lock { lines };
    let errors = lock.check();
    assert!(
        errors.is_empty(),
        "the union's surface is incoherent:\n  {}",
        errors.join("\n  ")
    );
    lock
}

/// The generated-module name for a category path, e.g. `union_types`.
fn module_of(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".rs"))
        .expect("category path is `…/<module>.rs`")
}

/// Every generated file as `(repo-relative path, contents)` — the union
/// categories, the feature module, the union's lock, and every per-version
/// facade's lock and code ([`facade::outputs`]). The single source the
/// generator binary writes and the staleness test checks.
pub fn all_outputs() -> Vec<(String, String)> {
    let rows = features::feature_table();
    let mut out: Vec<(String, String)> = CATEGORIES
        .iter()
        .map(|c| (c.path.to_string(), generate(c)))
        .collect();
    out.push((
        features::OUTPUT_PATH.to_string(),
        features::generate_features(&rows),
    ));
    let union = union_lock();
    out.push((UNION_LOCK_PATH.to_string(), lock::render("union", &union)));
    out.extend(facade::outputs(&rows, &union));
    out
}

/// Pipe `src` through `rustfmt` so the generator's output is byte-stable under
/// the repo's (default) formatting — i.e. `cargo fmt` never touches the checked-in
/// file. Requires `rustfmt` on `PATH` (it is, via the toolchain).
pub fn reformat(src: &str) -> String {
    // `BoundedCommand` owns the plumbing: the launch takes the workspace's single
    // spawn lock (concurrent test threads reformat in parallel, and on macOS
    // concurrent spawns leak raw pipe descriptors into each other's children), and
    // the source goes in on a writer thread while both output pipes are drained on
    // theirs. Writing a whole generated file to stdin by hand — as this did — is
    // the pipe deadlock waiting for a big enough input: rustfmt's output fills its
    // stdout buffer, it stops reading, and both sides block.
    let mut cmd = Command::new("rustfmt");
    cmd.args(["--edition", "2024", "--emit", "stdout"]);
    let output = BoundedCommand::new(cmd)
        .stdin_bytes(src.as_bytes().to_vec())
        .run_ok("rustfmt");
    String::from_utf8(output.stdout).expect("rustfmt output is utf-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screaming_snake_matches_node_kinds() {
        assert_eq!(screaming_snake("LongIdentType"), "LONG_IDENT_TYPE");
        assert_eq!(
            screaming_snake("HashConstraintType"),
            "HASH_CONSTRAINT_TYPE"
        );
        assert_eq!(screaming_snake("OptionalValPat"), "OPTIONAL_VAL_PAT");
        assert_eq!(screaming_snake("WithNullType"), "WITH_NULL_TYPE");
    }

    #[test]
    fn every_enum_variant_has_a_node() {
        for category in CATEGORIES {
            for e in category.enums {
                for (_, node_name) in e.variants {
                    let _ = find_node(category.nodes, node_name);
                }
            }
        }
    }
}

#[cfg(test)]
mod lock_tests {
    use std::collections::BTreeMap;

    use syn::visit::Visit;

    use super::*;

    /// Counts every `pub fn` anywhere in a file, however deeply nested.
    #[derive(Default)]
    struct PubFns(usize);

    impl<'ast> Visit<'ast> for PubFns {
        fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
            self.0 += usize::from(matches!(f.vis, syn::Visibility::Public(_)));
            syn::visit::visit_item_fn(self, f);
        }

        fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
            self.0 += usize::from(matches!(f.vis, syn::Visibility::Public(_)));
            syn::visit::visit_impl_item_fn(self, f);
        }
    }

    /// No public fn in `union.rs` escapes the lock: the accessors it lists
    /// number exactly the file's `pub fn`s, counted both by a walk of the whole
    /// syntax tree and by a line scan (which also sees inside macro bodies,
    /// where the walk cannot).
    #[test]
    fn every_pub_fn_in_union_is_a_locked_accessor() {
        let locked = union_lock()
            .lines
            .iter()
            .filter(|l| matches!(l, Line::Accessor { .. }))
            .count();
        let mut walk = PubFns::default();
        walk.visit_file(&syn::parse_file(accessors::UNION_SOURCE).unwrap());
        let scanned = accessors::UNION_SOURCE
            .lines()
            .filter(|l| l.trim_start().starts_with("pub fn "))
            .count();
        assert!(locked > 300, "only {locked} accessors were read");
        assert_eq!(walk.0, locked, "pub fns in the syntax tree vs the lock");
        assert_eq!(scanned, locked, "`pub fn` lines vs the lock");
    }

    /// Every kind a node casts from is a `SyntaxKind` variant, and no kind is
    /// cast by two nodes (so a kind's typed node, where it has one, is
    /// unique).
    #[test]
    fn every_kind_is_a_syntax_kind_cast_by_one_node() {
        let kinds_rs = include_str!("../../../crates/cst/src/syntax/kinds.rs");
        let file = syn::parse_file(kinds_rs).unwrap();
        let variants: BTreeSet<String> = file
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Enum(e) if e.ident == "SyntaxKind" => {
                    Some(e.variants.iter().map(|v| v.ident.to_string()).collect())
                }
                _ => None,
            })
            .expect("kinds.rs declares `SyntaxKind`");
        let mut owner: BTreeMap<String, String> = BTreeMap::new();
        for line in &union_lock().lines {
            if let Line::Node { node, kind } = line {
                assert!(
                    variants.contains(kind),
                    "`{node}` casts from `{kind}`, no SyntaxKind"
                );
                if let Some(other) = owner.insert(kind.clone(), node.clone()) {
                    panic!("`{kind}` is cast by both `{other}` and `{node}`");
                }
            }
        }
    }

    /// Each type name is declared once across every category's tables.
    #[test]
    fn every_type_name_is_declared_once() {
        let mut seen = BTreeSet::new();
        for category in CATEGORIES {
            let names = category
                .nodes
                .iter()
                .map(|n| n.name)
                .chain(category.enums.iter().map(|e| e.name))
                .chain(category.value_enums.iter().map(|v| v.name));
            for name in names {
                assert!(seen.insert(name), "`{name}` is declared twice");
            }
        }
    }

    /// The union's lock reads back as the same surface, and the checked-in
    /// lock parses (the staleness test holds it equal to a fresh render).
    #[test]
    fn the_union_lock_round_trips() {
        let lock = union_lock();
        let text = lock::render("union", &lock);
        assert_eq!(lock::parse(&text).unwrap(), lock);
        let on_disk = include_str!("../../../crates/cst/facades/union.lock");
        let parsed = lock::parse(on_disk).unwrap_or_else(|e| panic!("{UNION_LOCK_PATH}: {e}"));
        assert!(parsed.check().is_empty(), "{:#?}", parsed.check());
    }

    /// An accessor whose return type is outside the shape set fails the lock,
    /// naming it.
    #[test]
    fn an_unknown_shape_fails_the_lock_naming_the_accessor() {
        let source = format!(
            "{}\nimpl ParenExpr {{ pub fn label(&self) -> String {{ todo!() }} }}\n",
            accessors::UNION_SOURCE
        );
        let panic = std::panic::catch_unwind(|| lock_from(&source)).expect_err("the lock built");
        let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("ParenExpr::label"), "{message}");
    }
}

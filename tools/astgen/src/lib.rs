//! `borzoi-astgen` — the typed-AST facade generator for `borzoi-cst`
//! (`docs/ast-versioning-plan.md` Stage 4).
//!
//! The facade in `crates/cst/src/syntax/` is a large, mostly-mechanical layer:
//! newtype-over-`SyntaxNode` structs, `AstNode` impls, and closed dispatch enums
//! (`Type`, `Expr`, `Pat`, …). The *versioning* work (frozen per-version facades
//! projected from the union, `docs/ast-versioning-plan.md` D2/D4) multiplies that
//! boilerplate across versions, which is exactly the case for codegen (D5).
//!
//! This generator is a **pure-Rust descriptor table** — a [`Node`]/[`Enum`] model
//! grouped into [`Category`] units, from which we emit the facade boilerplate. It
//! takes no external dependency (an earlier ungrammar-based plan was dropped:
//! ungrammar is unmaintained, and our curated short enum-variant names —
//! `Type::Hash` ← `HashConstraintType` — plus multi-kind nodes — `AppExpr` ←
//! `APP_EXPR | INFIX_APP_EXPR` — would need sidecars regardless). The descriptor
//! expresses all of that directly.
//!
//! Bespoke accessors (positional / token-based / custom-enum-returning, ~half of
//! the facade) are **not** generated — they stay hand-written alongside the
//! generated code (rust-analyzer's `nodes.rs` + `node_ext.rs` split). The
//! generator owns only the boilerplate + the per-version cascade to come.
//!
//! Output is checked in (one file per [`Category`], under
//! `crates/cst/src/syntax/generated/`) and kept honest by a staleness test
//! (`tests/up_to_date.rs`): regenerate with
//! `cargo run -p borzoi-astgen --example generate`. (The generator is an *example*, not
//! a binary, so it does not make root `cargo run` ambiguous with the LSP.)

use std::process::Command;

use borzoi_spawn::BoundedCommand;

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

/// One generated file: a group of [`Node`]s and the [`Enum`]s over them, swapped
/// in as a unit (plan PR D, by category).
pub struct Category {
    /// Repo-root-relative output path.
    pub path: &'static str,
    /// One-line description for the generated file's module-doc header.
    pub summary: &'static str,
    /// Member newtypes (every node any of `enums` references must be here).
    pub nodes: &'static [Node],
    /// Dispatch enums over `nodes`.
    pub enums: &'static [Enum],
}

/// Every category, in swap order (types → patterns → …). Each entry is one
/// checked-in generated file.
pub const CATEGORIES: &[Category] = &[
    Category {
        path: "crates/cst/src/syntax/generated/union_types.rs",
        summary: "The **types** category: the `Type` dispatch enum and its member newtypes.",
        nodes: TYPE_NODES,
        enums: TYPE_ENUMS,
    },
    Category {
        path: "crates/cst/src/syntax/generated/union_pats.rs",
        summary: "The **patterns** category: the `Pat` dispatch enum and its member newtypes.",
        nodes: PAT_NODES,
        enums: PAT_ENUMS,
    },
    Category {
        path: "crates/cst/src/syntax/generated/union_exprs.rs",
        summary: "The **expressions** category: the `Expr` dispatch enum and its member newtypes.",
        nodes: EXPR_NODES,
        enums: EXPR_ENUMS,
    },
    Category {
        path: "crates/cst/src/syntax/generated/union_decls.rs",
        summary: "The **declarations / misc** category: the `ModuleDecl`, `SigDecl`, \
                  `TypeDefnRepr`, `MemberDefn`, `Measure`, and `RationalConst` dispatch \
                  enums and their member newtypes (`ModuleDecl`/`SigDecl` share several).",
        nodes: DECL_NODES,
        enums: DECL_ENUMS,
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
fn node_kinds(node: &Node) -> Vec<String> {
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
/// pairs). Factored out so per-version facades can pass a *subset* of an enum's
/// variants — e.g. `v8::Type` with the F# 9.0 `WithNull` variant removed. The
/// `cast` arm for any kind not in `variants` falls through to `None`, which is
/// exactly the projection's out-of-surface behaviour.
fn gen_enum_impl(name: &str, variants: &[(&str, &str)], nodes: &[Node], out: &mut String) {
    let mut variant_decls = String::new();
    let mut all_kinds: Vec<String> = Vec::new();
    let mut cast_arms = String::new();
    let mut syntax_arms = String::new();
    for (variant, node_name) in variants {
        variant_decls.push_str(&format!("    {variant}({node_name}),\n"));
        syntax_arms.push_str(&format!(
            "            {name}::{variant}(n) => n.syntax(),\n"
        ));
        for kind in node_kinds(find_node(nodes, node_name)) {
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
         use crate::syntax::{{AstNode, SyntaxKind, SyntaxNode}};\n\n",
        summary = category.summary,
    );
    for node in category.nodes {
        gen_node(node, &mut out);
    }
    for e in category.enums {
        gen_enum(e, category.nodes, &mut out);
    }
    reformat(&out)
}

/// A frozen, published per-version typed surface (`docs/ast-versioning-plan.md`
/// D4). Each is a projection of the union: nodes/variants introduced after the
/// surface's version are dropped, the rest re-exported. `v9` equals the union
/// today (nothing is introduced after 9.0); `v8` drops the F# 9.0 nullness node.
pub struct Surface {
    /// Repo-root-relative output path.
    pub path: &'static str,
    /// The F# language version this surface freezes, as a decimal.
    pub lang: f64,
    /// One-line description for the generated file's module-doc header.
    pub summary: &'static str,
}

/// The published surfaces, ascending. (`v8`/`v9` are the two with/without the
/// one post-floor typed delta; 4.6–7.0 resolve to the nearest, D3.)
pub const SURFACES: &[Surface] = &[
    Surface {
        path: "crates/cst/src/syntax/generated/v8.rs",
        lang: 8.0,
        summary: "The frozen **F# 8.0** typed surface — the union minus post-8.0 \
                  nodes. Today that is exactly the 9.0 nullness type, so only `Type` \
                  is a distinct (exhaustive, `WithNull`-free) enum; the rest re-exports \
                  the union.",
    },
    Surface {
        path: "crates/cst/src/syntax/generated/v9.rs",
        lang: 9.0,
        summary: "The frozen **F# 9.0** typed surface. Identical to the union today \
                  (nothing is introduced after 9.0), so it re-exports the union \
                  wholesale; it becomes a distinct projection only when a post-9.0 \
                  typed node is modelled.",
    },
];

/// The F# language version a kind was introduced at — mirrors cst's interval
/// table (`kinds.rs`). Drift is caught by cst's `v8` projection properties
/// (`ast_projection.rs`): a wrong row changes which variants the generated `v8`
/// excludes, breaking `v8_refines_v9_by_exactly_nullness`. `None` ⇒ present
/// since before the floor.
fn kind_introduced(kind: &str) -> Option<f64> {
    match kind {
        "WITH_NULL_TYPE" => Some(9.0),
        _ => None,
    }
}

/// Whether a node is in `lang`'s surface — i.e. all of its kinds are legal at
/// `lang`.
fn node_in_surface(node: &Node, lang: f64) -> bool {
    node_kinds(node)
        .iter()
        .all(|k| kind_introduced(k).is_none_or(|intro| lang >= intro))
}

/// The generated-module name for a category path, e.g. `union_types`.
fn module_of(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".rs"))
        .expect("category path is `…/<module>.rs`")
}

/// Generate one [`Surface`]'s frozen facade file: a projection of the union that
/// re-exports the in-surface nodes/enums and emits a distinct enum for any whose
/// variant set the surface narrows.
pub fn generate_facade(surface: &Surface) -> String {
    let lang = surface.lang;
    let header = format!(
        "//! GENERATED by `tools/astgen` — do not edit by hand.\n\
         //! Regenerate with `cargo run -p borzoi-astgen --example generate`; kept honest\n\
         //! by borzoi-astgen's `tests/up_to_date.rs` staleness test.\n\
         //!\n\
         //! {summary}\n\
         //!\n\
         //! A frozen, exhaustive surface (`docs/ast-versioning-plan.md` D4): matching\n\
         //! a distinct enum here is total over that version's syntax, and a node\n\
         //! introduced later casts to `None`. Bespoke accessors live on the union\n\
         //! nodes (`crate::syntax`); a `Type` here is re-cast on demand.\n\n",
        summary = surface.summary,
    );
    // Body first, so we can tell whether any distinct enum was emitted (and thus
    // whether the `AstNode`/`SyntaxKind`/`SyntaxNode` import is needed — a
    // pure-re-export surface like `v9` uses none of them).
    let mut out = String::new();
    let mut emitted_enum = false;
    for category in CATEGORIES {
        let module = module_of(category.path);
        let nodes_in: Vec<&str> = category
            .nodes
            .iter()
            .filter(|n| node_in_surface(n, lang))
            .map(|n| n.name)
            .collect();
        let all_nodes_in = nodes_in.len() == category.nodes.len();

        let mut unchanged_enums: Vec<&str> = Vec::new();
        let mut narrowed_enums: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
        for e in category.enums {
            let in_variants: Vec<(&str, &str)> = e
                .variants
                .iter()
                .filter(|(_, node)| node_in_surface(find_node(category.nodes, node), lang))
                .map(|(v, node)| (*v, *node))
                .collect();
            if in_variants.len() == e.variants.len() {
                unchanged_enums.push(e.name);
            } else {
                narrowed_enums.push((e.name, in_variants));
            }
        }

        if all_nodes_in && narrowed_enums.is_empty() {
            // Whole category unchanged at this version — re-export it wholesale.
            out.push_str(&format!("pub use super::{module}::*;\n\n"));
            continue;
        }
        let mut reexports: Vec<&str> = nodes_in;
        reexports.extend(unchanged_enums);
        if !reexports.is_empty() {
            out.push_str(&format!(
                "pub use super::{module}::{{{}}};\n\n",
                reexports.join(", "),
            ));
        }
        for (name, in_variants) in &narrowed_enums {
            gen_enum_impl(name, in_variants, category.nodes, &mut out);
            out.push('\n');
            emitted_enum = true;
        }
    }
    let imports = if emitted_enum {
        "use crate::syntax::{AstNode, SyntaxKind, SyntaxNode};\n\n"
    } else {
        ""
    };
    reformat(&format!("{header}{imports}{out}"))
}

/// Every generated file as `(repo-relative path, contents)` — the union
/// categories plus the per-version surface facades. The single source the
/// generator binary writes and the staleness test checks.
pub fn all_outputs() -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> =
        CATEGORIES.iter().map(|c| (c.path, generate(c))).collect();
    out.extend(SURFACES.iter().map(|s| (s.path, generate_facade(s))));
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

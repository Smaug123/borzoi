//! GENERATED typed-AST modules (`tools/astgen`), re-exported from
//! [`crate::syntax`].
//!
//! astgen's tables declare every typed node of the union: the dispatch-enum
//! categories [`union_types`] (`Type`), [`union_pats`] (`Pat`),
//! [`union_exprs`] (`Expr`) and [`union_decls`] (`ModuleDecl`, `SigDecl`,
//! `TypeDefnRepr`, `MemberDefn`, `Measure`, `RationalConst`), each with its
//! member newtypes, and [`union_standalone`], the newtypes no category's
//! dispatch enum holds (`ImplFile`, `Binding`, `LongIdent`, …), the
//! `ParsedInput` root enum, and the value enums the accessors return. The
//! accessors stay hand-written in `crate::syntax`'s `union` module.
//!
//! `facades` holds the per-version facades, each generated from its lock
//! (`crates/cst/facades/<module>.lock`) and the union's.

pub mod union_decls;
pub mod union_exprs;
pub mod union_pats;
pub mod union_standalone;
pub mod union_types;

pub mod facades;

// `LanguageFeature` and its data, from `crates/cst/language-features.tsv`;
// re-exported from `crate::language_version` and (`kind_feature`) from
// `crate::syntax`.
pub mod features;

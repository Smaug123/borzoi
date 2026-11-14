//! GENERATED typed-AST facade modules (`tools/astgen`), re-exported from
//! [`crate::syntax`] as the real facade.
//!
//! astgen's tables declare every typed node of the union: the dispatch-enum
//! categories [`union_types`] (`Type`), [`union_pats`] (`Pat`),
//! [`union_exprs`] (`Expr`) and [`union_decls`] (`ModuleDecl`, `SigDecl`,
//! `TypeDefnRepr`, `MemberDefn`, `Measure`, `RationalConst`), each with its
//! member newtypes, and [`union_standalone`], the newtypes no dispatch enum
//! holds (`ImplFile`, `Binding`, `LongIdent`, …) and the value enums the
//! accessors return. The accessors stay hand-written in `crate::syntax`'s
//! `union` module. The generated boilerplate's fidelity is guarded by the
//! whole existing suite — the FCS differential normaliser, the parser tests,
//! and the nullness projection properties all run against it.

pub mod union_decls;
pub mod union_exprs;
pub mod union_pats;
pub mod union_standalone;
pub mod union_types;

// The frozen, published per-version facades (plan PR E / D4): projections of the
// union that drop post-version nodes/variants. `v9` == the union today; `v8`
// drops the F# 9.0 nullness `Type::WithNull`. Re-exported from `crate::syntax`.
#[cfg(feature = "unstable-union")]
pub mod v8;
#[cfg(feature = "unstable-union")]
pub mod v9;

// `LanguageFeature` and its data, from `crates/cst/language-features.tsv`;
// re-exported from `crate::language_version` and (`kind_feature`) from
// `crate::syntax`.
pub mod features;

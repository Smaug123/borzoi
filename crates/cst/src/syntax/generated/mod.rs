//! GENERATED typed-AST facade modules (`tools/astgen`), re-exported from
//! [`crate::syntax`] as the real facade.
//!
//! **Status (plan PR D).** All dispatch-enum categories are now generated and
//! wired in: [`union_types`] (`Type`, PR D1), [`union_pats`] (`Pat`, PR D2),
//! [`union_exprs`] (`Expr`, PR D3), and [`union_decls`] (`ModuleDecl`, `SigDecl`,
//! `TypeDefnRepr`, `MemberDefn`, `Measure`, `RationalConst`, PR D4) — the
//! hand-written enums and their member newtypes are gone; the bespoke accessors
//! stay hand-written in `crate::syntax`. (Standalone non-enum newtypes — e.g.
//! `ImplFile`, `Binding`, `LongIdent` — remain hand-written `ast_node!`s.) The
//! generated boilerplate's fidelity is guarded by the whole existing suite — the
//! FCS differential normaliser, the parser tests, and the nullness projection
//! properties all run against the generated facade now.

pub mod union_decls;
pub mod union_exprs;
pub mod union_pats;
pub mod union_types;

// The frozen, published per-version facades (plan PR E / D4): projections of the
// union that drop post-version nodes/variants. `v9` == the union today; `v8`
// drops the F# 9.0 nullness `Type::WithNull`. Re-exported from `crate::syntax`.
pub mod v8;
pub mod v9;

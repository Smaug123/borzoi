//! The **module-open matrix**: every child shape a PURE assembly-module `open`
//! can carry, crossed with the contest scenarios, diffed against FCS — the
//! module-open half of the §7 grid (`docs/assembly-module-open-plan.md`), the
//! seam the original twenty review rounds hand-searched. (Mechanics live in
//! [`crate::common::fold_matrix`]; this module owns the grid.)
//!
//! ## The grid
//!
//! Each child shape lives in its own module `Demo.MOpen.<Shape>` in the abbrev
//! fixture, and nothing else declares those FQNs, so — unlike the cross-kind
//! `Demo.NsFold` grid — every open here exercises the *module* fold surface
//! alone: no namespace half, no cross-kind generation barrier, no
//! reference-order contest between halves.
//!
//! Shapes: values + a `[<Literal>]`, a module-level exception (which COMMITS,
//! unlike the §8-demoted namespace-level one), plain / RQA / struct unions, an
//! active pattern, an `[<AutoOpen>]` submodule (folded recursively), an
//! `[<AutoOpen>]` type (pickle-only residue that demotes its group), a nested
//! class with a static, a plain submodule, and a `DupA`/`DupB` pair whose
//! colliding value pins position-ordered cross-open shadowing. Channels: bare
//! expression, pattern position, and dotted heads through the opened module's
//! children (Slice B's nested-type / submodule channels), plus project-`let`
//! contests on both sides of the open.

use crate::common::fold_matrix::{Cell, Position, run_matrix};

const CELLS: &[Cell] = &[
    // ---- values and a literal: the fold's bread and butter ----
    Cell {
        decls: &[],
        label: "vals / module value, expression",
        body: &["open Demo.MOpen.Vals"],
        probe: "mVal",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "vals / literal, expression",
        body: &["open Demo.MOpen.Vals"],
        probe: "MLit",
        position: Position::Expr,
    },
    Cell {
        // A `[<Literal>]` is a CONSTANT pattern; a plain value in pattern
        // position would be a fresh binder instead.
        decls: &[],
        label: "vals / literal, bare pattern",
        body: &["open Demo.MOpen.Vals"],
        probe: "MLit",
        position: Position::PatternBare,
    },
    // ---- project-`let` contests around the open (position-ordered) ----
    Cell {
        decls: &[],
        label: "vals / project binding shadows the earlier open, expression",
        body: &["open Demo.MOpen.Vals", "let mVal = 5"],
        probe: "mVal",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "vals / later open shadows the project binding, expression",
        body: &["let mVal = 5", "open Demo.MOpen.Vals"],
        probe: "mVal",
        position: Position::Expr,
    },
    // ---- module-level exception: commits (the §8 demotion is namespace-only) ----
    Cell {
        decls: &[],
        label: "exn-mod / module value, expression",
        body: &["open Demo.MOpen.ExnMod"],
        probe: "mExnVal",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "exn-mod / exception, expression",
        body: &["open Demo.MOpen.ExnMod"],
        probe: "MExn",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "exn-mod / exception, pattern",
        body: &["open Demo.MOpen.ExnMod"],
        probe: "MExn",
        position: Position::PatternCtor,
    },
    // ---- exception + same-named [<AutoOpen>] literal in ONE module (§8 cell
    // 8b's module-half flavor, codex review): the literal folds after the
    // exception and wins the bare name in both positions — in a pattern it is
    // a constant pattern, so returning the exception would be a wrong target ----
    Cell {
        decls: &[],
        label: "exn-lit-mod / literal-vs-exception, expression",
        body: &["open Demo.MOpen.ExnLitMod"],
        probe: "MExnLit",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "exn-lit-mod / literal-vs-exception, bare pattern",
        body: &["open Demo.MOpen.ExnLitMod"],
        probe: "MExnLit",
        position: Position::PatternBare,
    },
    // ---- the same shadow with a PLAIN value: what does the pattern bind? ----
    Cell {
        decls: &[],
        label: "exn-shadow-mod / plain value shadows the exception, expression",
        body: &["open Demo.MOpen.ExnShadowMod"],
        probe: "MExnShadow",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "exn-shadow-mod / plain value shadows the exception, pattern",
        body: &["open Demo.MOpen.ExnShadowMod"],
        probe: "MExnShadow",
        position: Position::PatternCtor,
    },
    // ---- plain union: cases fold opaque (Q1) ----
    Cell {
        decls: &[],
        label: "union-mod / unique case, expression",
        body: &["open Demo.MOpen.UnionMod"],
        probe: "MCaseB",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "union-mod / unique case, pattern",
        body: &["open Demo.MOpen.UnionMod"],
        probe: "MCaseB",
        position: Position::PatternCtor,
    },
    Cell {
        decls: &[],
        label: "union-mod-dotted / type-qualified case, expression",
        body: &["open Demo.MOpen.UnionMod"],
        probe: "MUnion.MCaseB",
        position: Position::Expr,
    },
    // ---- RQA union: cases NOT imported bare; the dotted channel is the only one ----
    Cell {
        decls: &[],
        label: "rqa-mod / case not imported, expression",
        body: &["open Demo.MOpen.RqaMod"],
        probe: "MRqaA",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "rqa-mod-dotted / required-qualified case, expression",
        body: &["open Demo.MOpen.RqaMod"],
        probe: "MRqa.MRqaA",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "rqa-mod-dotted / required-qualified case, bare pattern",
        body: &["open Demo.MOpen.RqaMod"],
        probe: "MRqa.MRqaA",
        position: Position::PatternBare,
    },
    // ---- struct union: cases import bare (non-RQA) ----
    Cell {
        decls: &[],
        label: "struct-mod / unique case, expression",
        body: &["open Demo.MOpen.StructMod"],
        probe: "MOn",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "struct-mod / unique case, bare pattern",
        body: &["open Demo.MOpen.StructMod"],
        probe: "MOn",
        position: Position::PatternBare,
    },
    // ---- active pattern: tags fold opaque ----
    Cell {
        decls: &[],
        label: "actpat / tag, bare pattern",
        body: &["open Demo.MOpen.ActPat"],
        probe: "MEven",
        position: Position::PatternBare,
    },
    // ---- [<AutoOpen>] submodule: folded recursively ----
    Cell {
        decls: &[],
        label: "auto-sub / module value, expression",
        body: &["open Demo.MOpen.AutoSub"],
        probe: "mAutoOuter",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "auto-sub / auto-open inner value, expression",
        body: &["open Demo.MOpen.AutoSub"],
        probe: "mAutoInner",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "auto-sub-dotted / submodule-qualified value, expression",
        body: &["open Demo.MOpen.AutoSub"],
        probe: "Inner.mAutoInner",
        position: Position::Expr,
    },
    // ---- [<AutoOpen>] type: its statics are unenumerable, but they fold at
    // the tycon tier BEFORE the same surface's vals, so the module's own value
    // still commits soundly (contrast the namespace matrix's AutoType shape,
    // where the value sat on another surface and reference order demoted it) ----
    Cell {
        decls: &[],
        label: "auto-type-mod / module value beside the auto-open type, expression",
        body: &["open Demo.MOpen.AutoTypeMod"],
        probe: "mPoisoned",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "auto-type-mod / auto-opened static, expression",
        body: &["open Demo.MOpen.AutoTypeMod"],
        probe: "MAutoStatic",
        position: Position::Expr,
    },
    // ---- nested class: the bare constructor slot, and Slice B's dotted head ----
    Cell {
        decls: &[],
        label: "class-mod / module value, expression",
        body: &["open Demo.MOpen.ClassMod"],
        probe: "mClassVal",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "class-mod / nested type constructor, bare expression",
        body: &["open Demo.MOpen.ClassMod"],
        probe: "MClass",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "class-mod-dotted / static under the nested-type head, expression",
        body: &["open Demo.MOpen.ClassMod"],
        probe: "MClass.MStat",
        position: Position::Expr,
    },
    // ---- plain submodule: contents NOT imported; Slice B's dotted head ----
    Cell {
        decls: &[],
        label: "sub-mod / submodule content not imported, expression",
        body: &["open Demo.MOpen.SubMod"],
        probe: "subVal",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "sub-mod-dotted / submodule-qualified value, expression",
        body: &["open Demo.MOpen.SubMod"],
        probe: "Sub.subVal",
        position: Position::Expr,
    },
    // ---- two module opens: position-ordered shadowing, no reference-order haze ----
    Cell {
        decls: &[],
        label: "dup / later open wins the colliding value, expression",
        body: &["open Demo.MOpen.DupA", "open Demo.MOpen.DupB"],
        probe: "dupVal",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "dup / value unique to the earlier open, expression",
        body: &["open Demo.MOpen.DupA", "open Demo.MOpen.DupB"],
        probe: "onlyA",
        position: Position::Expr,
    },
];

/// Cells where FCS resolves the probe but we do not — each must remain
/// *exactly* "we name nothing while FCS resolves" (see the harness ratchet).
const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "vals / literal, bare pattern",
        "an opened `[<Literal>]` is not modelled as a constant pattern — the bare name \
         reads as a fresh binder instead (literal-ness is undetectable in general, Q17); \
         same machinery as §8's option B",
    ),
    (
        "exn-lit-mod / literal-vs-exception, bare pattern",
        "§8 cell 8b, module-half flavor: FCS binds the later-folded literal (a constant \
         pattern) over the exception; the pattern-shadowed exception folds opaque \
         (`demote_pattern_shadowed_exceptions`), so the pattern defers rather than \
         committing either",
    ),
    (
        "union-mod / unique case, expression",
        "an assembly union case folds opaque (Q1): in scope, naming no target",
    ),
    (
        "union-mod / unique case, pattern",
        "an assembly union case folds opaque (Q1): in scope, naming no target",
    ),
    (
        "union-mod-dotted / type-qualified case, expression",
        "an assembly union case folds opaque (Q1) and is not a static member of its type \
         in the entity model, so the type-qualified reading defers the same way",
    ),
    (
        "rqa-mod-dotted / required-qualified case, expression",
        "an assembly union case folds opaque (Q1) even when RQA makes the qualified \
         channel the ONLY one — the sharpest availability edge of the opaque-case fold",
    ),
    (
        "rqa-mod-dotted / required-qualified case, bare pattern",
        "an assembly union case folds opaque (Q1) even when RQA makes the qualified \
         channel the ONLY one — the sharpest availability edge of the opaque-case fold",
    ),
    (
        "struct-mod / unique case, expression",
        "an assembly union case folds opaque (Q1)",
    ),
    (
        "struct-mod / unique case, bare pattern",
        "an assembly union case folds opaque (Q1)",
    ),
    (
        "actpat / tag, bare pattern",
        "an active-pattern tag folds opaque: in pattern scope, naming no target",
    ),
    (
        "auto-sub-dotted / submodule-qualified value, expression",
        "a submodule as a dotted head (`open M` then `Sub.f`) defers — Slice B of the plan",
    ),
    (
        "auto-type-mod / auto-opened static, expression",
        "an [<AutoOpen>] type's statics are pickle-only — not enumerable",
    ),
    (
        "class-mod / nested type constructor, bare expression",
        "a bare nested-type constructor is the constructor slot, not the value fold \
         (mirror of the namespace matrix's `class / unique type` gap)",
    ),
    (
        "class-mod-dotted / static under the nested-type head, expression",
        "a type nested in the opened module as a dotted head defers — Slice B's \
         nested-type channel",
    ),
    (
        "sub-mod-dotted / submodule-qualified value, expression",
        "a submodule as a dotted head (`open M` then `Sub.f`) defers — Slice B of the plan",
    ),
];

#[test]
fn module_open_matches_fcs_on_every_cell() {
    run_matrix(CELLS, KNOWN_GAPS, &[], "module_open_matrix");
}

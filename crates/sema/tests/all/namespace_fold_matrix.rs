//! The **namespace-fold matrix**: every child shape a cross-kind open's *namespace
//! half* can carry, crossed with the module half and the contest scenarios, diffed
//! against FCS. (The mechanics — snippet construction, the batched FCS invocation,
//! the bijection and its ratchet — are the shared harness in
//! [`crate::common::fold_matrix`]; this module owns the grid.)
//!
//! ## Why a matrix
//!
//! Folding the assembly namespace half of a cross-kind `open` (an FQN that is both an
//! assembly module and an assembly namespace, Q9) was a hand-driven search: five
//! `codex review` rounds each surfaced one *cell* of an enumerable product —
//! value-vs-type contests, cross-assembly duplicates, the value/pattern namespace
//! split, unknowable abbreviations, namespace-type eviction of an earlier open. This
//! harness makes the product explicit and lets the machine find the next cell instead
//! of a reviewer (`docs/assembly-module-open-plan.md` §7).
//!
//! ## The grid
//!
//! Each child shape lives in its own cross-kind namespace `Demo.NsFold.<Shape>`: the
//! **namespace half** (`tests/fixtures/fsharp_abbrev_env/Fixture.fs`) carries the
//! shape; the **module half** (`tests/fixtures/autoopen_env/Fixture.fs`, `module
//! Demo.NsFold.<Shape>`) carries a unique `mh…` value plus, where a comment says so, a
//! value that deliberately collides with a namespace-half name. Isolating each shape
//! keeps a residue-bearing one (an `[<AutoOpen>]` type, a case-nameless union) from
//! poisoning a sibling's cell.
//!
//! Shapes: exception, exception + same-named `[<Literal>]` (§8 cell 8b), plain union,
//! `[<RequireQualifiedAccess>]` union, struct union, plain class, `[<AutoOpen>]` type,
//! `[<AutoOpen>]` module (with a literal and an active pattern), type abbreviation,
//! a same-surface tier clash (`TierClash`: a tycon-tier type vs a same-named
//! auto-open value), and a cross-open duplicate-type pair (`EvictA`/`EvictB`).
//! Channels: bare expression, pattern position for the constructor shapes
//! (constructor-shaped with an argument, plus the bare shape a constant pattern
//! takes), and **dotted heads** — the probe is a whole `Head.Member` path, so the
//! qualified channel and its contests (a project `let` capturing the head, a
//! generation-barrier-staled head — codex round 10 — and the cross-open
//! whole-path-first / latest-open-wins precedence) are cells too.
//!
//! ## The property
//!
//! Exactly the extension matrix's bijection, made absence-a-value on both sides: FCS
//! resolves the probe to `X` (into one of our two fixtures) ⟺ we resolve it to `X`;
//! FCS resolves nothing ⟺ we resolve nothing. The soundness direction — **we commit
//! `X` ⟹ FCS agrees** — is the no-wrong-target invariant; the availability direction —
//! **FCS resolves ⟹ we resolve or it is a listed [`KNOWN_GAPS`] deferral** — ratchets
//! the conservative losses this slice makes on purpose (an opaque union case, an
//! active-pattern tag, a reference-order collision, a residue-poisoned group). A gap
//! that starts naming a target, or that FCS stops resolving, fails: the ratchet only
//! tightens.

use crate::common::fold_matrix::{Cell, Position, run_matrix};

const CELLS: &[Cell] = &[
    // ---- exception: value + pattern scope, folded opaque (§8 option A) ----
    Cell {
        decls: &[],
        label: "exn / module-half unique value",
        body: &["open Demo.NsFold.Exn"],
        probe: "mhExn",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "exn / unique exception, expression",
        body: &["open Demo.NsFold.Exn"],
        probe: "NsExnSolo",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "exn / unique exception, pattern",
        body: &["open Demo.NsFold.Exn"],
        probe: "NsExnSolo",
        position: Position::PatternCtor,
    },
    Cell {
        decls: &[],
        label: "exn / colliding value-vs-exception, expression",
        body: &["open Demo.NsFold.Exn"],
        probe: "NsExn",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "exn / colliding value-vs-exception, pattern",
        body: &["open Demo.NsFold.Exn"],
        probe: "NsExn",
        position: Position::PatternCtor,
    },
    // ---- exception + same-named [<Literal>] in ONE surface (§8 cell 8b): FCS folds
    // the exception at the tycon tier and the auto-open module's literal after it,
    // so the literal wins the bare name in both positions ----
    Cell {
        decls: &[],
        label: "exn-lit / module-half unique value",
        body: &["open Demo.NsFold.ExnLit"],
        probe: "mhExnLit",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "exn-lit / literal-vs-exception, expression",
        body: &["open Demo.NsFold.ExnLit"],
        probe: "NsExnLit",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "exn-lit / literal-vs-exception, bare pattern",
        body: &["open Demo.NsFold.ExnLit"],
        probe: "NsExnLit",
        position: Position::PatternBare,
    },
    // ---- plain union: cases into value + pattern scope (we fold them opaque) ----
    Cell {
        decls: &[],
        label: "union / module-half unique value",
        body: &["open Demo.NsFold.Union"],
        probe: "mhUnion",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "union / unique case, expression",
        body: &["open Demo.NsFold.Union"],
        probe: "UCaseB",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "union / unique case, pattern",
        body: &["open Demo.NsFold.Union"],
        probe: "UCaseB",
        position: Position::PatternCtor,
    },
    Cell {
        decls: &[],
        label: "union / colliding case-vs-value, expression",
        body: &["open Demo.NsFold.Union"],
        probe: "UCaseA",
        position: Position::Expr,
    },
    // ---- RQA union: cases NOT imported bare (Q6) ----
    Cell {
        decls: &[],
        label: "rqa / module-half unique value",
        body: &["open Demo.NsFold.RqaUnion"],
        probe: "mhRqa",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "rqa / case not imported, expression",
        body: &["open Demo.NsFold.RqaUnion"],
        probe: "RqaA",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "rqa / case not imported, pattern",
        body: &["open Demo.NsFold.RqaUnion"],
        probe: "RqaA",
        position: Position::PatternCtor,
    },
    // ---- struct union: cases import bare (non-RQA) ----
    Cell {
        decls: &[],
        label: "struct-union / module-half unique value",
        body: &["open Demo.NsFold.StructUnion"],
        probe: "mhStruct",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "struct-union / unique case, expression",
        body: &["open Demo.NsFold.StructUnion"],
        probe: "StructOn",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "struct-union / unique case, pattern",
        body: &["open Demo.NsFold.StructUnion"],
        probe: "StructOn",
        position: Position::PatternCtor,
    },
    // ---- plain class: a constructor-slot type ----
    Cell {
        decls: &[],
        label: "class / module-half unique value",
        body: &["open Demo.NsFold.ClassType"],
        probe: "mhClass",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "class / unique type, expression",
        body: &["open Demo.NsFold.ClassType"],
        probe: "NsClassSolo",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "class / colliding value-vs-type, expression",
        body: &["open Demo.NsFold.ClassType"],
        probe: "NsClass",
        position: Position::Expr,
    },
    // ---- [<AutoOpen>] type: statics we cannot enumerate (residue poisons the group) ----
    Cell {
        decls: &[],
        label: "auto-type / module-half value (residue-poisoned)",
        body: &["open Demo.NsFold.AutoType"],
        probe: "mhAutoType",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "auto-type / auto-opened static",
        body: &["open Demo.NsFold.AutoType"],
        probe: "AutoStatic",
        position: Position::Expr,
    },
    // ---- [<AutoOpen>] module: enumerable; folded recursively ----
    Cell {
        decls: &[],
        label: "auto-module / module-half unique value",
        body: &["open Demo.NsFold.AutoModule"],
        probe: "mhAutoModule",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "auto-module / unique auto-open value",
        body: &["open Demo.NsFold.AutoModule"],
        probe: "nsAutoSolo",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "auto-module / colliding auto-open value",
        body: &["open Demo.NsFold.AutoModule"],
        probe: "nsAutoVal",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "auto-module / literal",
        body: &["open Demo.NsFold.AutoModule"],
        probe: "NsLiteral",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "auto-module / active-pattern tag, pattern",
        body: &["open Demo.NsFold.AutoModule"],
        probe: "NsEven",
        position: Position::PatternCtor,
    },
    // ---- type abbreviation (pickle-only) ----
    Cell {
        decls: &[],
        label: "abbrev / module-half unique value",
        body: &["open Demo.NsFold.Abbrev"],
        probe: "mhAbbrev",
        position: Position::Expr,
    },
    // ==== dotted-head cells: the probe is a whole `Head.Member` path, so the
    // contest sits behind a QUALIFIED head (plan §7's dotted-head column).
    // Both sides record the member at the whole-path span, so the currency is
    // unchanged. ====
    //
    // ---- the live qualified channel: an uncontested namespace-type head ----
    Cell {
        decls: &[],
        label: "class-dotted / unique type static, expression",
        body: &["open Demo.NsFold.ClassType"],
        probe: "NsClassSolo.SoloStat",
        position: Position::Expr,
    },
    Cell {
        // The bare `NsClass` value-vs-type contest (codex P1-A) moved behind a
        // dotted head: whichever half wins the head decides whether `.Stat`
        // resolves, so committing the static would gamble on reference order.
        decls: &[],
        label: "class-dotted / collided value-vs-type head, expression",
        body: &["open Demo.NsFold.ClassType"],
        probe: "NsClass.Stat",
        position: Position::Expr,
    },
    // ---- type-qualified union cases (the RQA ones REQUIRE this channel) ----
    Cell {
        decls: &[],
        label: "union-dotted / type-qualified case, expression",
        body: &["open Demo.NsFold.Union"],
        probe: "UnionShape.UCaseB",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "union-dotted / type-qualified case, bare pattern",
        body: &["open Demo.NsFold.Union"],
        probe: "UnionShape.UCaseB",
        position: Position::PatternBare,
    },
    Cell {
        decls: &[],
        label: "rqa-dotted / required-qualified case, expression",
        body: &["open Demo.NsFold.RqaUnion"],
        probe: "RqaShape.RqaA",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "rqa-dotted / required-qualified case, bare pattern",
        body: &["open Demo.NsFold.RqaUnion"],
        probe: "RqaShape.RqaA",
        position: Position::PatternBare,
    },
    // ---- module-qualified value through the namespace half's child module ----
    Cell {
        decls: &[],
        label: "auto-module-dotted / module-qualified value, expression",
        body: &["open Demo.NsFold.AutoModule"],
        probe: "NsAutoModule.nsAutoSolo",
        position: Position::Expr,
    },
    // ---- project-binding contests: a `let` between the opens captures the
    // head (F# is value-first for a long ident's head), so the type's static
    // must NOT resolve. The staled variants are codex round 10 as cells: a
    // later cross-kind open's generation barrier stales the binding, and the
    // head must DEFER, not fall through to the assembly reading. ----
    Cell {
        decls: &[],
        label: "class-dotted / project value captures the head, expression",
        body: &["open Demo.NsFold.ClassType", "let NsClassSolo = 5"],
        probe: "NsClassSolo.SoloStat",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "class-dotted / staled project head defers (round 10), expression",
        body: &[
            "open Demo.NsFold.ClassType",
            "let NsClassSolo = 5",
            "open Demo.NsFold.Union",
        ],
        probe: "NsClassSolo.SoloStat",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "class / staled project binding, bare expression",
        body: &[
            "open Demo.NsFold.ClassType",
            "let NsClassSolo = 5",
            "open Demo.NsFold.Union",
        ],
        probe: "NsClassSolo",
        position: Position::Expr,
    },
    Cell {
        // The availability half of round 10: a head with NO entry to stale
        // keeps resolving through the earlier open across a later cross-kind
        // bump (this is the cell a blanket dotted veto would break).
        decls: &[],
        label: "class-dotted / type head live across a later cross-kind open, expression",
        body: &["open Demo.NsFold.ClassType", "open Demo.NsFold.Union"],
        probe: "NsClassSolo.SoloStat",
        position: Position::Expr,
    },
    // ---- second-tier clash in ONE surface: tycon tier folds `type NsTier`,
    // the auto-open vals fold `let NsTier` after it — the value wins the bare
    // slot and captures the dotted head ----
    Cell {
        decls: &[],
        label: "tier / module-half unique value",
        body: &["open Demo.NsFold.TierClash"],
        probe: "mhTier",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "tier / same-surface value-vs-type, bare expression",
        body: &["open Demo.NsFold.TierClash"],
        probe: "NsTier",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "tier-dotted / static under the value-captured head, expression",
        body: &["open Demo.NsFold.TierClash"],
        probe: "NsTier.TierStat",
        position: Position::Expr,
    },
    // ---- cross-open dotted-head contest: EvictA and EvictB both carry a
    // class `NsDup`. F# prefers the reading that resolves the WHOLE path
    // (`FromA` through the earlier open), latest-open-wins on a tie
    // (`DupStat` is on both) — the tiered-walk pins. ----
    Cell {
        decls: &[],
        label: "evict / earlier module-half value under a later cross-kind bump",
        body: &["open Demo.NsFold.EvictA", "open Demo.NsFold.EvictB"],
        probe: "mhEvictA",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "evict-dotted / member unique to the earlier type, expression",
        body: &["open Demo.NsFold.EvictA", "open Demo.NsFold.EvictB"],
        probe: "NsDup.FromA",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "evict-dotted / member unique to the later type, expression",
        body: &["open Demo.NsFold.EvictA", "open Demo.NsFold.EvictB"],
        probe: "NsDup.FromB",
        position: Position::Expr,
    },
    Cell {
        decls: &[],
        label: "evict-dotted / member on both types, expression",
        body: &["open Demo.NsFold.EvictA", "open Demo.NsFold.EvictB"],
        probe: "NsDup.DupStat",
        position: Position::Expr,
    },
];

/// Cells where FCS resolves the probe but we deliberately defer — the conservative
/// losses this slice makes on purpose. Each must remain *exactly* "we defer while FCS
/// resolves"; naming a target, or FCS falling silent, fails the ratchet.
const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "exn / unique exception, expression",
        "a namespace-half exception folds opaque (§8 option A): a later open's constructible \
         type would evict it from the constructor slot, which bare-name lookup does not model",
    ),
    (
        "exn / unique exception, pattern",
        "a namespace-half exception folds opaque (§8 option A): a same-surface literal would \
         beat it as a constant pattern (8b), and literal-ness is undetectable in general (Q17)",
    ),
    (
        "exn / colliding value-vs-exception, expression",
        "module value vs exception constructor — a value-space contest FCS orders by reference",
    ),
    (
        "exn / colliding value-vs-exception, pattern",
        "a collided constructor defers in BOTH namespaces — a colliding literal is a pattern too (Q17), so the exception cannot safely win the pattern",
    ),
    (
        "exn-lit / literal-vs-exception, bare pattern",
        "§8 cell 8b: FCS binds the later-folded literal (a constant pattern) over the \
         exception; the exception folds opaque, so the pattern defers rather than \
         committing either",
    ),
    (
        "union / unique case, expression",
        "an assembly union case folds opaque (Q1): in scope, naming no target",
    ),
    (
        "union / unique case, pattern",
        "an assembly union case folds opaque (Q1): in scope, naming no target",
    ),
    (
        "union / colliding case-vs-value, expression",
        "case vs module value — a reference-order contest",
    ),
    (
        "struct-union / unique case, expression",
        "an assembly union case folds opaque (Q1)",
    ),
    (
        "struct-union / unique case, pattern",
        "an assembly union case folds opaque (Q1)",
    ),
    (
        "class / unique type, expression",
        "a bare namespace-type constructor is the eviction/type channel, not the value fold",
    ),
    (
        "class / colliding value-vs-type, expression",
        "value vs constructor-slot type — a reference-order contest (codex P1-A)",
    ),
    (
        "auto-type / module-half value (residue-poisoned)",
        "an [<AutoOpen>] type's unenumerable statics are residue that demotes the group",
    ),
    (
        "auto-type / auto-opened static",
        "an [<AutoOpen>] type's statics are pickle-only — not enumerable",
    ),
    (
        "auto-module / colliding auto-open value",
        "auto-open value vs module value — a reference-order contest",
    ),
    (
        "auto-module / active-pattern tag, pattern",
        "an active-pattern tag folds opaque: in pattern scope, naming no target",
    ),
    (
        "class-dotted / collided value-vs-type head, expression",
        "the bare value-vs-type contest (codex P1-A) behind a dotted head: whichever half \
         folds later captures the head (here FCS's later-folded type resolves the static), \
         and that order is reference-order — `head_value_slot` is Unordered, so we defer",
    ),
    (
        "union-dotted / type-qualified case, expression",
        "an assembly union case folds opaque (Q1) and is not a static member of its type \
         in the entity model, so the type-qualified reading defers the same way",
    ),
    (
        "union-dotted / type-qualified case, bare pattern",
        "an assembly union case folds opaque (Q1) and is not a static member of its type \
         in the entity model, so the type-qualified reading defers the same way",
    ),
    (
        "rqa-dotted / required-qualified case, expression",
        "an assembly union case folds opaque (Q1) even when RQA makes the qualified \
         channel the ONLY one — the sharpest availability edge of the opaque-case fold",
    ),
    (
        "rqa-dotted / required-qualified case, bare pattern",
        "an assembly union case folds opaque (Q1) even when RQA makes the qualified \
         channel the ONLY one — the sharpest availability edge of the opaque-case fold",
    ),
    (
        "evict / earlier module-half value under a later cross-kind bump",
        "the round-4 generation barrier is coarser than FCS: a later cross-kind open's \
         namespace type stales EVERY earlier opened entry, including an unrelated \
         module-half value the later open does not even collide with",
    ),
];

#[test]
fn namespace_fold_matches_fcs_on_every_cell() {
    run_matrix(CELLS, KNOWN_GAPS, &[], "ns_fold_matrix");
}

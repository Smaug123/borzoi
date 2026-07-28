//! The **implicit-namespace matrix**: the same fold contests the explicit-`open`
//! grids enumerate, reached through FCS's *implicit* open of a block's own
//! enclosing namespace instead of through an `open` the source writes.
//! (Mechanics live in [`crate::common::fold_matrix`]; this module owns the grid.)
//!
//! ## Why a matrix
//!
//! `ImplicitlyOpenOwnNamespace` (`CheckDeclarations.fs`) opens the enclosing
//! namespace path once per top-level block, before any declaration, by
//! resolving it with `ResolveLongIdentAsModuleOrNamespace` — which yields
//! **every** modref at the FQN — and folding them all. So it is the *same*
//! fold an explicit `open` of that path performs, over the same halves: this
//! project's own fragment, each referenced assembly's namespace, and a same-FQN
//! top-level module in any of them.
//!
//! The first implementation of that channel folded only the assembly namespace
//! half and committed its names, which was wrong in exactly two ways — an
//! assembly auto-open value beat a colliding project union case, and a
//! namespace half's value beat a same-FQN module half it should have deferred
//! to. Both are cells of a product the explicit grids already enumerate; they
//! were reachable only because those grids' probe is always a header-less
//! top-level `let`, so no cell of theirs has a namespace container at all.
//! This grid supplies the missing dimension, over the same fixture shapes.
//!
//! ## The grid
//!
//! Every cell writes **no `open`**: the container header is the whole
//! mechanism. Three families, one per kind of half the implicit fold merges:
//!
//! - `Demo.NsFold.<Shape>` — cross-kind: the **namespace half** lives in the
//!   abbrev fixture, a same-FQN **module half** in the autoopen fixture. The
//!   `mh…` probes are the module half's own values, which an implicit fold that
//!   collected only namespace surfaces would not see at all.
//! - `Demo.PjFold.<Shape>` / `Demo.PjMix.NsOnly` — the project half: an earlier
//!   Compile-order file declares the shape in the same namespace the probe file
//!   declares, against an assembly half at the same FQN.
//! - `Demo.Auto` and `Demo.ModuleOpen.Merged` — the plain auto-open channel and
//!   the module/namespace merge, plus the two negative controls that bound the
//!   channel: a *deeper* namespace does not reach its ancestor's auto-opens
//!   (the full path is opened, never a prefix), and a local binding of the name
//!   wins (the implicit open sits below every declaration).
//!
//! ## The property
//!
//! The shared bijection: FCS resolves the probe to `X` (into a fixture, or into
//! one of the cell's decl files) ⟺ we resolve it to `X`; FCS resolves nothing
//! ⟺ we resolve nothing. [`KNOWN_GAPS`] ratchets the deferrals this channel
//! makes on purpose — each must remain *exactly* "we defer while FCS resolves".

use crate::common::fold_matrix::{Cell, Container, Position, run_matrix};

const PJ_EXN: &str = "namespace Demo.PjFold.Exn\nexception PjExn of int\n";
const PJ_UNION: &str =
    "namespace Demo.PjFold.Union\ntype PjUnion =\n    | PjCaseA\n    | PjCaseB\n";
const PJ_AUTOMOD: &str = "namespace Demo.PjFold.AutoMod\n\n[<AutoOpen>]\nmodule PjAuto =\n    let pjAutoVal () = 7\n    let pjAutoSolo () = 8\n";
const PJ_CLASS: &str =
    "namespace Demo.PjFold.ClassShape\ntype PjClass() =\n    static member PjStat = 9\n";
const PJ_MIX_MOD: &str = "module Demo.PjMix.NsOnly\n\nlet pjModVal () = 10\n";

const CELLS: &[Cell] = &[
    // ================= cross-kind: module half + namespace half =================
    // The `mh…` probes are the assembly MODULE half's own values. An implicit
    // fold that collected `open_namespace_fold_surfaces` alone never sees them.
    Cell {
        container: Container::Namespace("Demo.NsFold.Exn"),
        decls: &[],
        label: "exn / module-half unique value",
        body: &[],
        after: &[],
        probe: "mhExn",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.Exn"),
        decls: &[],
        label: "exn / unique exception, expression",
        body: &[],
        after: &[],
        probe: "NsExnSolo",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.Exn"),
        decls: &[],
        label: "exn / unique exception, pattern",
        body: &[],
        after: &[],
        probe: "NsExnSolo",
        position: Position::PatternCtor,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.Exn"),
        decls: &[],
        label: "exn / colliding value-vs-exception, expression",
        body: &[],
        after: &[],
        probe: "NsExn",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.Union"),
        decls: &[],
        label: "union / module-half unique value",
        body: &[],
        after: &[],
        probe: "mhUnion",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.Union"),
        decls: &[],
        label: "union / unique case, expression",
        body: &[],
        after: &[],
        probe: "UCaseB",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.Union"),
        decls: &[],
        label: "union / unique case, pattern",
        body: &[],
        after: &[],
        probe: "UCaseB",
        position: Position::PatternCtor,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.Union"),
        decls: &[],
        label: "union / colliding case-vs-value, expression",
        body: &[],
        after: &[],
        probe: "UCaseA",
        position: Position::Expr,
    },
    // A `module A.B.M` header encloses in `A.B` — the name above its own — so
    // the same merge is reached from a module-headed file.
    Cell {
        container: Container::Module("Demo.NsFold.Union.ProbeModValue"),
        decls: &[],
        label: "module-header / module-half unique value",
        body: &[],
        after: &[],
        probe: "mhUnion",
        position: Position::Expr,
    },
    Cell {
        container: Container::Module("Demo.NsFold.Union.ProbeModCase"),
        decls: &[],
        label: "module-header / unique case, expression",
        body: &[],
        after: &[],
        probe: "UCaseB",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.RqaUnion"),
        decls: &[],
        label: "rqa / module-half unique value",
        body: &[],
        after: &[],
        probe: "mhRqa",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.RqaUnion"),
        decls: &[],
        label: "rqa / case not imported, expression",
        body: &[],
        after: &[],
        probe: "RqaA",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.ClassType"),
        decls: &[],
        label: "class / module-half unique value",
        body: &[],
        after: &[],
        probe: "mhClass",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.ClassType"),
        decls: &[],
        label: "class / unique type, expression",
        body: &[],
        after: &[],
        probe: "NsClassSolo",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.ClassType"),
        decls: &[],
        label: "class / colliding value-vs-type, expression",
        body: &[],
        after: &[],
        probe: "NsClass",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.ClassType"),
        decls: &[],
        label: "class-dotted / unique type static, expression",
        body: &[],
        after: &[],
        probe: "NsClassSolo.SoloStat",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.AutoType"),
        decls: &[],
        label: "auto-type / module-half value (residue-poisoned)",
        body: &[],
        after: &[],
        probe: "mhAutoType",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.AutoType"),
        decls: &[],
        label: "auto-type / auto-opened static",
        body: &[],
        after: &[],
        probe: "AutoStatic",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.AutoModule"),
        decls: &[],
        label: "auto-module / module-half unique value",
        body: &[],
        after: &[],
        probe: "mhAutoModule",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.AutoModule"),
        decls: &[],
        label: "auto-module / unique auto-open value",
        body: &[],
        after: &[],
        probe: "nsAutoSolo",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.AutoModule"),
        decls: &[],
        label: "auto-module / colliding auto-open value",
        body: &[],
        after: &[],
        probe: "nsAutoVal",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.AutoModule"),
        decls: &[],
        label: "auto-module / active-pattern tag, pattern",
        body: &[],
        after: &[],
        probe: "NsEven",
        position: Position::PatternCtor,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.TierClash"),
        decls: &[],
        label: "tier / module-half unique value",
        body: &[],
        after: &[],
        probe: "mhTier",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.NsFold.TierClash"),
        decls: &[],
        label: "tier / same-surface value-vs-type, bare expression",
        body: &[],
        after: &[],
        probe: "NsTier",
        position: Position::Expr,
    },
    // ===================== the project half folds last (Q14) =====================
    Cell {
        container: Container::Namespace("Demo.PjFold.Exn"),
        decls: &[PJ_EXN],
        label: "pj-exn / assembly module-half value, expression",
        body: &[],
        after: &[],
        probe: "mhPjExn",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.PjFold.Exn"),
        decls: &[PJ_EXN],
        label: "pj-exn / project exception, expression",
        body: &[],
        after: &[],
        probe: "PjExn",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.PjFold.Exn"),
        decls: &[PJ_EXN],
        label: "pj-exn / project exception, pattern",
        body: &[],
        after: &[],
        probe: "PjExn",
        position: Position::PatternCtor,
    },
    Cell {
        container: Container::Namespace("Demo.PjFold.Union"),
        decls: &[PJ_UNION],
        label: "pj-union / assembly module-half value, expression",
        body: &[],
        after: &[],
        probe: "mhPjUnion",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.PjFold.Union"),
        decls: &[PJ_UNION],
        label: "pj-union / project case, expression",
        body: &[],
        after: &[],
        probe: "PjCaseB",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.PjFold.Union"),
        decls: &[PJ_UNION],
        label: "pj-union / project case, pattern",
        body: &[],
        after: &[],
        probe: "PjCaseB",
        position: Position::PatternCtor,
    },
    Cell {
        container: Container::Namespace("Demo.PjFold.AutoMod"),
        decls: &[PJ_AUTOMOD],
        label: "pj-auto / assembly module-half value, expression",
        body: &[],
        after: &[],
        probe: "mhPjAuto",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.PjFold.AutoMod"),
        decls: &[PJ_AUTOMOD],
        label: "pj-auto / project auto-open value, expression",
        body: &[],
        after: &[],
        probe: "pjAutoSolo",
        position: Position::Expr,
    },
    Cell {
        // The project half applies LAST, so FCS binds the project auto-open
        // value over the colliding assembly module-half value.
        container: Container::Namespace("Demo.PjFold.AutoMod"),
        decls: &[PJ_AUTOMOD],
        label: "pj-auto / colliding value, expression",
        body: &[],
        after: &[],
        probe: "pjAutoVal",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.PjFold.ClassShape"),
        decls: &[PJ_CLASS],
        label: "pj-class / assembly module-half value, expression",
        body: &[],
        after: &[],
        probe: "mhPjClass",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.PjFold.ClassShape"),
        decls: &[PJ_CLASS],
        label: "pj-class-dotted / static under the project type head, expression",
        body: &[],
        after: &[],
        probe: "PjClass.PjStat",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.PjMix.NsOnly"),
        decls: &[PJ_MIX_MOD],
        label: "pj-mix / project module value, expression",
        body: &[],
        after: &[],
        probe: "pjModVal",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.PjMix.NsOnly"),
        decls: &[PJ_MIX_MOD],
        label: "pj-mix / assembly namespace exception, expression",
        body: &[],
        after: &[],
        probe: "PjNsExn",
        position: Position::Expr,
    },
    // ============ the plain auto-open channel, and what bounds it ============
    Cell {
        container: Container::Namespace("Demo.Auto"),
        decls: &[],
        label: "auto / assembly auto-open module value",
        body: &[],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        container: Container::Module("Demo.Auto.ProbeModAuto"),
        decls: &[],
        label: "auto / reached from a module header one level down",
        body: &[],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // The FULL path is opened, never a prefix: from inside `Demo.Auto.Deeper`
        // the auto-opens of `Demo.Auto` are out of scope (FCS: FS0039).
        container: Container::Namespace("Demo.Auto.Deeper"),
        decls: &[],
        label: "auto / a deeper namespace does not reach its ancestor's auto-opens",
        body: &[],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // The implicit open sits below every declaration, so the file's own
        // binding wins — "nothing" in the matrix currency on both sides.
        container: Container::Namespace("Demo.Auto"),
        decls: &[],
        label: "auto / a local binding of the same name wins",
        body: &["let extraValue () = 999"],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // Both halves supply this name and FCS orders them by reference.
        container: Container::Namespace("Demo.ModuleOpen.Merged"),
        decls: &[],
        label: "merged / name supplied by both halves",
        body: &[],
        after: &[],
        probe: "fromModuleHalf",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.ModuleOpen.Merged"),
        decls: &[],
        label: "merged / name unique to the namespace half",
        body: &[],
        after: &[],
        probe: "onlyInNamespaceHalf",
        position: Position::Expr,
    },
    // ====== what a LATER declaration in the same block does to the fold ======
    // The implicit open sits at position 0, below everything the block declares
    // — including declarations the fold itself cannot see when it runs. A local
    // target is "nothing" in the matrix currency on both sides, so these cells
    // read: FCS binds something local (None) ⟺ we do not commit the assembly's
    // (None). A wrong commit shows up as `ours: Some(<fixture member>)`.
    Cell {
        container: Container::Namespace("Demo.Auto"),
        decls: &[],
        label: "later-decl / a later local binding out-ranks the implicit member",
        body: &["let extraValue () = 1"],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    // Shadowings the resolver does not model generally — a later constructible
    // type taking the value slot, and a later `[<AutoOpen>]` *type*'s statics
    // folding above an earlier open — reproduce through an explicit `open` too
    // and have their own tasks. Their cells belong here because this channel
    // meets them far more often, and it must DECLINE rather than commit a
    // target FCS shadows: `screen_block_local_shadows` makes both `None` on our
    // side, which is the same value FCS's local target renders as in the matrix
    // currency, so they are ordinary passing cells and not gaps.
    //
    // A same-block `[<AutoOpen>]` **module** is no longer one of them: it folds
    // its own surface at its declaration position
    // (`Resolver::fold_own_auto_open_module`), so the cell below passes by real
    // position-ordered shadowing rather than by a screen. That is the whole
    // point of the fold-back — see `crate::auto_open_foldback_matrix`.
    Cell {
        container: Container::Namespace("Demo.Auto"),
        decls: &[],
        label: "later-decl / a later constructible type takes the implicit member's slot",
        body: &["type Tag() =", "    member _.Marker = 1"],
        after: &[],
        probe: "Tag",
        position: Position::Expr,
    },
    Cell {
        // An `[<AutoOpen>]` **type**'s statics fold into the enclosing scope
        // too, and are reachable by no name-level pre-scan — the closed
        // container flag is what covers them.
        container: Container::Namespace("Demo.Auto"),
        decls: &[],
        label: "later-decl / a later [<AutoOpen>] type's static out-ranks the implicit member",
        body: &[
            "[<AutoOpen>]",
            "type LocalStatics =",
            "    static member extraValue () = 1",
        ],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // A plain union **keeps** the value slot (`SlotClass::Keeps`), so it
        // shadows nothing and the assembly value must still resolve
        // (fcs-dump-verified: bare `Tag` is `Demo.Auto.Extra.Tag`). Screening on
        // every type name rather than the slot classification cost this cell.
        container: Container::Namespace("Demo.Auto"),
        decls: &[],
        label: "later-decl / a slot-keeping union does not shadow the implicit member",
        body: &["type Tag =", "    | A", "    | B"],
        after: &[],
        probe: "Tag",
        position: Position::Expr,
    },
    Cell {
        // FCS's `CanAutoOpenTyconRef` ends `tcref.Typars(m) |> List.isEmpty`, so
        // a GENERIC `[<AutoOpen>]` type auto-opens nothing at all and shadows
        // nothing (fcs-dump-verified: `extraValue` still binds the assembly's).
        container: Container::Namespace("Demo.Auto"),
        decls: &[],
        label: "later-decl / a generic [<AutoOpen>] type auto-opens nothing",
        body: &[
            "[<AutoOpen>]",
            "type Holder<'a> =",
            "    static member Hold = 1",
        ],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // The **project** half needs the same screen: an earlier file's
        // auto-open module value is folded at position 0 too, so a later local
        // type takes its slot exactly as it takes an assembly member's.
        container: Container::Namespace("Demo.PjFold.AutoMod"),
        decls: &[PJ_AUTOMOD],
        label: "later-decl / a later type shadows the project half's auto-open value",
        body: &["type pjAutoSolo() =", "    member _.Marker = 1"],
        after: &[],
        probe: "pjAutoSolo",
        position: Position::Expr,
    },
    Cell {
        // A union case inside a same-block `[<AutoOpen>]` module is a
        // value-space binder no `Pat` walk reaches, so the screen's pre-scan
        // must collect it separately.
        container: Container::Namespace("Demo.Auto"),
        decls: &[],
        label: "later-decl / a later [<AutoOpen>] module's union case out-ranks the implicit member",
        body: &[
            "[<AutoOpen>]",
            "module LocalCases =",
            "    type LocalU =",
            "        | Tag",
        ],
        after: &[],
        probe: "Tag",
        position: Position::Expr,
    },
    Cell {
        container: Container::Namespace("Demo.Auto"),
        decls: &[],
        label: "later-decl / a later [<AutoOpen>] module's value out-ranks the implicit member",
        body: &[
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    let extraValue () = 1",
        ],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
];

/// Cells where we deliberately defer while FCS resolves — the ratchet only
/// tightens: a listed cell that starts naming a target, or that FCS stops
/// resolving, fails.
///
/// Every entry here is a gap the **explicit**-`open` grids already carry under
/// the same label and the same reason (`namespace_fold_matrix::KNOWN_GAPS`,
/// `project_half_matrix::KNOWN_GAPS`). That correspondence is the grid's real
/// claim: the implicit channel folds the path exactly as an `open` of it would,
/// so it neither commits more (a wrong target) nor defers more (an avoidable
/// gap). A gap appearing here and NOT there — or vice versa — means the two
/// channels have drifted apart, which is the failure this grid exists to catch.
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
        "module-header / unique case, expression",
        "an assembly union case folds opaque (Q1) — the `module A.B.M` header reaches the \
         same fold as the `namespace A.B` one, so it inherits the same gap",
    ),
    (
        "class / unique type, expression",
        "the matrix env includes the autoopen fixture, whose deliberately unresolvable \
         `[<assembly: AutoOpen(\"SemaAutoOpen.NoSuchPath\")>]` makes the env-wide auto-open \
         surface unknowable — an unseen auto-open could supply a *value* of any name, so the \
         bare-constructor fallback withholds every commitment in this closure \
         (`assembly_bare_value_surface_could_supply`, arm 0)",
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
        "pj-class-dotted / static under the project type head, expression",
        "a PROJECT type's static member is not modelled — sema resolves members of \
         referenced-assembly types only, so the dotted head defers",
    ),
    (
        "pj-mix / assembly namespace exception, expression",
        "a namespace-half exception folds opaque (§8 option A) — the same gap as this grid's \
         `exn / unique exception` cells, here under a project module half",
    ),
    (
        "merged / name supplied by both halves",
        "the module half (autoopen fixture) and the namespace half (abbrev fixture) both \
         supply it; FCS folds them in reference order, which sema does not model, so the \
         collision defers rather than binding either — the regression that shipped when \
         this channel collected only namespace surfaces and committed one",
    ),
];

#[test]
fn implicit_namespace_matches_fcs_on_every_cell() {
    run_matrix(CELLS, KNOWN_GAPS, &[], "implicit_ns_matrix");
}

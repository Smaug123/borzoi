//! The **auto-open fold-back matrix**: what a file's own `[<AutoOpen>]` nested
//! module contributes to the rest of its enclosing block.
//! (Mechanics live in [`crate::common::fold_matrix`]; this module owns the grid.)
//!
//! ## The claim under test
//!
//! A nested `[<AutoOpen>] module M` is exactly an `open M` written at M's
//! closing position — the same fold, over the same halves, as any other open.
//! Everything in the grid follows from that one sentence, and each family
//! bounds it in a direction fcs-dump probes settled first:
//!
//! - **Position is source position.** Which of a same-named `open` and an
//!   `[<AutoOpen>]` module wins is decided purely by which comes later
//!   (`OrderAfter`/`OrderBefore` probes), and a use *preceding* the module does
//!   not see it at all (FCS reports no use — the name is unbound).
//! - **…except in a `module rec` block**, where every declaration is in scope
//!   for every other: there a use before the module *does* bind it, and it beats
//!   even a later `open` of a colliding module. That ordering is not a position
//!   rule at all, so the fold is declined wholesale in a rec block rather than
//!   modelled wrong.
//! - **The surface is recursive.** An `[<AutoOpen>]` module inside an
//!   `[<AutoOpen>]` module reaches the grandparent, and a *plain* submodule of
//!   an auto-open module lends its short name as a dotted head.
//! - **An anonymous root folds nothing**, because a header-less file's nested
//!   module has no modelled qualified path — so the fold declines there too,
//!   rather than silently enumerating zero names and letting an earlier open's
//!   same-named value stand.
//!
//! ## The property
//!
//! The shared bijection: FCS resolves the probe to `X` (into a fixture) ⟺ we
//! resolve it to `X`; FCS resolves nothing ⟺ we resolve nothing. A target in
//! the probe file itself — which is what "the file's own auto-open module won"
//! looks like — is *nothing* on both sides, so a cell whose expected answer is
//! `None` fails exactly when one side reaches into the assembly and the other
//! does not. [`KNOWN_GAPS`] ratchets the deferrals this channel makes on
//! purpose.

use crate::common::fold_matrix::{Cell, Container, Position, run_matrix};

/// A Compile-order-preceding project file whose namespace supplies a union case
/// `PjMarker`, so an `open` of it can be contested by the probe file's own
/// auto-open module in the **constructor** namespace.
const PJ_CASE: &str = "namespace Demo.FbCase\ntype PjHolder =\n    | PjMarker\n";

const CELLS: &[Cell] = &[
    // ===================== the fold happens at all =====================
    Cell {
        // The repro: `Demo.Auto`'s auto-open `Extra.extraValue` is in scope
        // from the explicit open, and the block's own `[<AutoOpen>]` module,
        // declared later, must take the name back.
        container: Container::Module("Demo.FoldBack.ExplicitOpen"),
        decls: &[],
        label: "after / explicit open, then the block's own auto-open module",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    let extraValue () = 1",
        ],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // The same contest through the *implicit* enclosing-namespace fold
        // rather than a written `open` — the channel that made this defect
        // reachable without any `open` in the source at all.
        container: Container::Namespace("Demo.Auto"),
        decls: &[],
        label: "after / implicit namespace fold, then the block's own auto-open module",
        body: &[
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    let extraValue () = 1",
        ],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // The shape that dominates real code: `namespace N` plus one
        // `[<AutoOpen>]` module holding the file. A use INSIDE that module
        // cannot be shadowed by the module's own fold-back — FCS folds M's
        // surface into the ENCLOSING scope, while inside M the same names are
        // in scope by ordinary position — so the namespace's own fold must
        // still reach the probe.
        container: Container::NamespaceAutoOpen("Demo.Auto"),
        decls: &[],
        label: "inside / a use inside the auto-open module still sees the namespace fold",
        body: &[],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // Inside the module, an `extern` prototype's name is a value binder we
        // do not intern, so a use of it must decline rather than fall through to
        // the assembly value the enclosing namespace supplies. The blanket
        // screen used to hide this by declining the whole fold for the file
        // (codex round 2).
        //
        // The probe name is `extraShortenTarget`, not `extraValue`: this cell's
        // probe module is itself `[<AutoOpen>]` under `namespace Demo.Auto`, so
        // whatever it declares joins that namespace's project fragment for
        // **every other cell in the batch** — naming it `extraValue` silently
        // turned four other cells' FCS side from the assembly value to this
        // file's.
        container: Container::NamespaceAutoOpen("Demo.Auto"),
        decls: &[],
        label: "inside / an extern prototype shadows the namespace fold's value",
        body: &["extern int extraShortenTarget()"],
        after: &[],
        probe: "extraShortenTarget",
        position: Position::Expr,
    },
    // ===================== position bounds the fold =====================
    Cell {
        // A use preceding the module does not see it: FCS reports the
        // assembly's value, not the block's own.
        container: Container::Module("Demo.FoldBack.BeforeDecl"),
        decls: &[],
        label: "before / a use preceding the auto-open module does not see it",
        body: &["open Demo.Auto"],
        after: &[
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    let extraValue () = 1",
        ],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // …except in a `module rec` block, where FCS binds the block's own
        // auto-open module from a use that precedes it (fcs-dump-probed), and
        // where the auto-open module beats even a LATER `open` of a colliding
        // one. Neither is a position rule, so the fold declines here.
        container: Container::ModuleRec("Demo.Auto.RecFoldBack"),
        decls: &[],
        label: "rec / a use preceding the auto-open module in a rec block binds it",
        body: &[],
        after: &[
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    let extraValue () = 1",
        ],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // Negative control: a *plain* nested module contributes nothing, so the
        // assembly's value stands.
        container: Container::Module("Demo.FoldBack.PlainNested"),
        decls: &[],
        label: "plain / a nested module without [<AutoOpen>] folds nothing",
        body: &[
            "open Demo.Auto",
            "module LocalPlain =",
            "    let extraValue () = 1",
        ],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    // ===================== the surface is recursive =====================
    Cell {
        container: Container::Module("Demo.FoldBack.NestedAuto"),
        decls: &[],
        label: "nested / an auto-open module inside an auto-open module reaches the grandparent",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module OuterAuto =",
            "    [<AutoOpen>]",
            "    module InnerAuto =",
            "        let extraValue () = 1",
        ],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // A PLAIN submodule of an auto-open module lends its short name: FCS
        // binds `OuterAuto.ChainedInner.chainedValue` through the bare head
        // `ChainedInner`. The assembly supplies the very same dotted path
        // (`Demo.Auto.Extra.ChainedInner.chainedValue`), so a fall-through
        // names a target and this cell discriminates.
        container: Container::Module("Demo.FoldBack.NestedPlain"),
        decls: &[],
        label: "nested-plain / a plain submodule of the auto-open module lends its short name",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module OuterAuto =",
            "    module ChainedInner =",
            "        let chainedValue () = 1",
        ],
        after: &[],
        probe: "ChainedInner.chainedValue",
        position: Position::Expr,
    },
    // ===================== what the fold brings =====================
    Cell {
        // A constructible type in the auto-open module takes FCS's unqualified
        // value slot from the assembly's same-named `let Tag = 99`.
        container: Container::Module("Demo.FoldBack.TypeEvicts"),
        decls: &[],
        label: "evict / a type in the auto-open module takes the value slot",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    type Tag() =",
            "        member _.Marker = 1",
        ],
        after: &[],
        probe: "Tag",
        position: Position::Expr,
    },
    Cell {
        container: Container::Module("Demo.FoldBack.CaseExpr"),
        decls: &[],
        label: "case / a union case in the auto-open module wins the value slot",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    type Holder =",
            "        | Tag",
        ],
        after: &[],
        probe: "Tag",
        position: Position::Expr,
    },
    Cell {
        container: Container::Module("Demo.FoldBack.CasePattern"),
        decls: &[],
        label: "case / the same case in pattern position",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    type Holder =",
            "        | Tag",
        ],
        after: &[],
        probe: "Tag",
        position: Position::PatternBare,
    },
    Cell {
        container: Container::Module("Demo.FoldBack.Literal"),
        decls: &[],
        label: "literal / a [<Literal>] in the auto-open module wins the value slot",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    [<Literal>]",
            "    let Tag = 99",
        ],
        after: &[],
        probe: "Tag",
        position: Position::Expr,
    },
    Cell {
        // `private` restricts the module to its enclosing container, which is
        // exactly where this probe sits — so the fold still happens
        // (fcs-dump-probed).
        container: Container::Module("Demo.FoldBack.PrivateAuto"),
        decls: &[],
        label: "private / a private auto-open module folds within its own container",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module private LocalAuto =",
            "    let extraValue () = 1",
        ],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // An active-pattern case is a value-space name `open_module_values`
        // cannot enumerate — but it lives in the pattern namespace alone, so it
        // must not shadow an *unrelated* earlier-opened value. The blunt
        // generation barrier would; declining the case by name does not.
        container: Container::Module("Demo.FoldBack.HiddenValues"),
        decls: &[],
        label: "hidden / an active pattern in the auto-open module shadows no earlier value",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    let (|Marker|_|) (x: int) = if x = 0 then Some () else None",
        ],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
    Cell {
        // …not even the value it is *named after*: FCS does not admit an
        // active-pattern case as a value at all (`let v = Even` is FS0039), so
        // the assembly's `Demo.Auto.Extra.Tag` still wins expression position.
        container: Container::Module("Demo.FoldBack.HiddenNameClash"),
        decls: &[],
        label: "hidden / an active-pattern case does not take the value slot it is named for",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    let (|Tag|_|) (x: int) = if x = 0 then Some () else None",
        ],
        after: &[],
        probe: "Tag",
        position: Position::Expr,
    },
    Cell {
        // …and in *pattern* position it does contest, against a case an earlier
        // open supplied. The fold cannot name the local recognizer's target, so
        // the reference must DEFER — scanning past it to the earlier project
        // case would be a wrong go-to-def.
        container: Container::Module("Demo.FoldBack.HiddenCaseClash"),
        decls: &[PJ_CASE],
        label: "hidden / an active-pattern case declines an earlier open's case in pattern position",
        body: &[
            "open Demo.FbCase",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    let (|PjMarker|_|) (x: int) = if x = 0 then Some () else None",
        ],
        after: &[],
        probe: "PjMarker",
        position: Position::PatternBare,
    },
    Cell {
        // A `private` recognizer is invisible outside its own module, so it
        // contests nothing in the enclosing scope and the case an earlier open
        // supplied still wins. The decline must therefore be filtered by
        // accessibility, not driven by the name alone (codex round 3).
        container: Container::Module("Demo.FoldBack.PrivateAp"),
        decls: &[PJ_CASE],
        label: "private / a private active-pattern case does not decline an earlier open's case",
        body: &[
            "open Demo.FbCase",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    let private (|PjMarker|_|) (x: int) = if x = 0 then Some () else None",
        ],
        after: &[],
        probe: "PjMarker",
        position: Position::PatternBare,
    },
    Cell {
        // A `private` prototype is visible within its own module only, so it
        // takes no slot in the enclosing scope either — the name-keyed decline
        // must be filtered by accessibility exactly as the recognizer's is
        // (codex round 4).
        container: Container::Module("Demo.FoldBack.PrivateExtern"),
        decls: &[],
        label: "private / a private extern prototype takes no enclosing slot",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    extern int private Tag()",
        ],
        after: &[],
        probe: "Tag",
        position: Position::Expr,
    },
    Cell {
        // A `private` type is visible within its own module only, so it takes no
        // slot in the enclosing scope and the assembly's value stands.
        container: Container::Module("Demo.FoldBack.PrivateType"),
        decls: &[],
        label: "private / a private type in the auto-open module takes no enclosing slot",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    type private Tag() =",
            "        member _.Marker = 1",
        ],
        after: &[],
        probe: "Tag",
        position: Position::Expr,
    },
    // ===================== the anonymous root =====================
    Cell {
        // A header-less file's nested module has no modelled qualified path, so
        // its members cannot be enumerated. Folding zero names would leave the
        // assembly's `extraValue` standing where FCS binds the block's own, so
        // the fold declines instead.
        container: Container::Anon,
        decls: &[],
        label: "anon / an anonymous root's auto-open module cannot be enumerated",
        body: &[
            "open Demo.Auto",
            "[<AutoOpen>]",
            "module LocalAuto =",
            "    let extraValue () = 1",
        ],
        after: &[],
        probe: "extraValue",
        position: Position::Expr,
    },
];

/// Cells where we deliberately defer while FCS resolves — each must remain
/// *exactly* that.
const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "hidden / an active pattern in the auto-open module shadows no earlier value",
        "the `[<AutoOpen>]` marker is unprovable in this env — the autoopen fixture carries an unknowable auto-open surface, which makes every attribute candidate unrulable — so the fold declines rather than committing either side of it",
    ),
    (
        "hidden / an active-pattern case does not take the value slot it is named for",
        "the `[<AutoOpen>]` marker is unprovable in this env — the autoopen fixture carries an unknowable auto-open surface, which makes every attribute candidate unrulable — so the fold declines rather than committing either side of it",
    ),
    (
        "private / a private active-pattern case does not decline an earlier open's case",
        "the `[<AutoOpen>]` marker is unprovable in this env — the autoopen fixture carries an unknowable auto-open surface, which makes every attribute candidate unrulable — so the fold declines rather than committing either side of it",
    ),
    (
        "private / a private extern prototype takes no enclosing slot",
        "the `[<AutoOpen>]` marker is unprovable in this env — the autoopen fixture carries an unknowable auto-open surface, which makes every attribute candidate unrulable — so the fold declines rather than committing either side of it",
    ),
    (
        "private / a private type in the auto-open module takes no enclosing slot",
        "the `[<AutoOpen>]` marker is unprovable in this env — the autoopen fixture carries an unknowable auto-open surface, which makes every attribute candidate unrulable — so the fold declines rather than committing either side of it",
    ),
];

#[test]
fn auto_open_foldback_matches_fcs_on_every_cell() {
    run_matrix(CELLS, KNOWN_GAPS, &[], "auto_open_foldback_matrix");
}

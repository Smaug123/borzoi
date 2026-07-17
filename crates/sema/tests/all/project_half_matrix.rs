//! The **project-half matrix**: the placement column §7 lists last — a
//! cross-kind `open` where one half of the FQN is **project code** in an
//! earlier Compile-order file. (Mechanics live in
//! [`crate::common::fold_matrix`]; this module owns the grid.)
//!
//! ## The grid
//!
//! Two flavors, one per direction of the project/assembly split:
//!
//! - `Demo.PjFold.<Shape>` — the ASSEMBLY module half lives in the autoopen
//!   fixture; a PROJECT decl file declares `namespace Demo.PjFold.<Shape>`
//!   carrying the shape. This is the `is_project_namespace_path` arm of the
//!   `cross_kind` demote: the project namespace half is not yet a fold
//!   surface, so the blanket demote holds the conservative line — these
//!   cells pin exactly what that blanket costs (and what it protects), and
//!   the machinery slice that folds the project half flips them.
//! - `Demo.PjMix.NsOnly` — the ASSEMBLY namespace half lives in the abbrev
//!   fixture (no assembly module half anywhere); a PROJECT decl file declares
//!   `module Demo.PjMix.NsOnly`. The project module half already joins the
//!   fold as the last-applied group (Q14: the project half beats every
//!   assembly half), so these cells pin the live behavior.
//!
//! Each cell's decl files are its own (our side resolves the cell as a
//! two-file project); on the FCS side every file joins one batched project,
//! isolated by FQN. The currency covers targets in the fixtures AND in the
//! cell's decl files, so "the project half wins" is a positive value on both
//! sides, not a blind `None`.

use crate::common::fold_matrix::{Cell, Position, run_matrix};

const PJ_EXN: &str = "namespace Demo.PjFold.Exn\nexception PjExn of int\n";
const PJ_UNION: &str =
    "namespace Demo.PjFold.Union\ntype PjUnion =\n    | PjCaseA\n    | PjCaseB\n";
const PJ_AUTOMOD: &str = "namespace Demo.PjFold.AutoMod\n\n[<AutoOpen>]\nmodule PjAuto =\n    let pjAutoVal () = 7\n    let pjAutoSolo () = 8\n";
const PJ_CLASS: &str =
    "namespace Demo.PjFold.ClassShape\ntype PjClass() =\n    static member PjStat = 9\n";
const PJ_MIX_MOD: &str = "module Demo.PjMix.NsOnly\n\nlet pjModVal () = 10\n";

const CELLS: &[Cell] = &[
    // ---- project namespace half carries an exception ----
    Cell {
        decls: &[PJ_EXN],
        label: "pj-exn / assembly module-half value, expression",
        body: &["open Demo.PjFold.Exn"],
        probe: "mhPjExn",
        position: Position::Expr,
    },
    Cell {
        decls: &[PJ_EXN],
        label: "pj-exn / project exception, expression",
        body: &["open Demo.PjFold.Exn"],
        probe: "PjExn",
        position: Position::Expr,
    },
    Cell {
        decls: &[PJ_EXN],
        label: "pj-exn / project exception, pattern",
        body: &["open Demo.PjFold.Exn"],
        probe: "PjExn",
        position: Position::PatternCtor,
    },
    // ---- project namespace half carries a union ----
    Cell {
        decls: &[PJ_UNION],
        label: "pj-union / assembly module-half value, expression",
        body: &["open Demo.PjFold.Union"],
        probe: "mhPjUnion",
        position: Position::Expr,
    },
    Cell {
        decls: &[PJ_UNION],
        label: "pj-union / project case, expression",
        body: &["open Demo.PjFold.Union"],
        probe: "PjCaseB",
        position: Position::Expr,
    },
    Cell {
        decls: &[PJ_UNION],
        label: "pj-union / project case, pattern",
        body: &["open Demo.PjFold.Union"],
        probe: "PjCaseB",
        position: Position::PatternCtor,
    },
    // ---- project namespace half carries an [<AutoOpen>] module; one value
    // collides with the assembly module half ----
    Cell {
        decls: &[PJ_AUTOMOD],
        label: "pj-auto / assembly module-half value, expression",
        body: &["open Demo.PjFold.AutoMod"],
        probe: "mhPjAuto",
        position: Position::Expr,
    },
    Cell {
        decls: &[PJ_AUTOMOD],
        label: "pj-auto / project auto-open value, expression",
        body: &["open Demo.PjFold.AutoMod"],
        probe: "pjAutoSolo",
        position: Position::Expr,
    },
    Cell {
        // The project half applies LAST (Q14), so FCS binds the project
        // auto-open value over the assembly module-half value.
        decls: &[PJ_AUTOMOD],
        label: "pj-auto / colliding value, expression",
        body: &["open Demo.PjFold.AutoMod"],
        probe: "pjAutoVal",
        position: Position::Expr,
    },
    // ---- project namespace half carries a class ----
    Cell {
        decls: &[PJ_CLASS],
        label: "pj-class / assembly module-half value, expression",
        body: &["open Demo.PjFold.ClassShape"],
        probe: "mhPjClass",
        position: Position::Expr,
    },
    Cell {
        decls: &[PJ_CLASS],
        label: "pj-class-dotted / static under the project type head, expression",
        body: &["open Demo.PjFold.ClassShape"],
        probe: "PjClass.PjStat",
        position: Position::Expr,
    },
    // ---- the reverse flavor: PROJECT module half + assembly namespace half ----
    Cell {
        decls: &[PJ_MIX_MOD],
        label: "pj-mix / project module value, expression",
        body: &["open Demo.PjMix.NsOnly"],
        probe: "pjModVal",
        position: Position::Expr,
    },
    Cell {
        decls: &[PJ_MIX_MOD],
        label: "pj-mix / assembly namespace exception, expression",
        body: &["open Demo.PjMix.NsOnly"],
        probe: "PjNsExn",
        position: Position::Expr,
    },
    Cell {
        decls: &[PJ_MIX_MOD],
        label: "pj-mix-dotted / assembly namespace type static, expression",
        body: &["open Demo.PjMix.NsOnly"],
        probe: "PjNsClass.PjNsStat",
        position: Position::Expr,
    },
];

/// Cells where FCS resolves the probe but we do not — each must remain
/// *exactly* "we name nothing while FCS resolves" (see the harness ratchet).
/// The `is_project_namespace_path` blanket demote's six cells (assembly
/// module-half values under pj-exn/pj-union/pj-auto/pj-class, plus the two
/// pj-auto project-side cells) flipped once the project namespace half was
/// folded as a recursive `open_project_namespace_values` (§7's "machinery"
/// slice) and the `cross_kind` arm was deleted — they are no longer gaps.
const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "pj-class-dotted / static under the project type head, expression",
        "a PROJECT type's static member is not modelled — sema resolves members of \
         referenced-assembly types only, so the dotted head defers",
    ),
    (
        "pj-mix / assembly namespace exception, expression",
        "a namespace-half exception folds opaque (§8 option A) — the same gap as the \
         namespace matrix's `exn / unique exception` cells, here under a project module half",
    ),
    (
        "pj-mix-dotted / assembly namespace type static, expression",
        "the qualified channel through the assembly namespace half defers when the open \
         also matches a PROJECT module (contrast the namespace matrix's `class-dotted / \
         unique type static`, which resolves when both halves are assemblies)",
    ),
];

#[test]
fn project_half_matches_fcs_on_every_cell() {
    // The colliding cell is the grid's Q14-precedence witness: its gap entry
    // alone would pass whichever side FCS picked, so pin the FCS side to the
    // PROJECT declaration site (decl 0's `pjAutoVal` binder) — the assembly
    // module-half member rendering (`Demo.PjFold.AutoMod.pjAutoVal`) fails it.
    let pj_auto_val = PJ_AUTOMOD.find("pjAutoVal").expect("decl text");
    let fcs_pins = [(
        "pj-auto / colliding value, expression",
        format!("pj:0:{}..{}", pj_auto_val, pj_auto_val + "pjAutoVal".len()),
    )];
    run_matrix(CELLS, KNOWN_GAPS, &fcs_pins, "project_half_matrix");
}

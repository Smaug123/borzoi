//! The names and value-namespace paths a file publishes for later files
//! (`ExportedItem::name`, `ExportedItem::qualified_path`), pinned for every
//! way the walk makes an export: `let` values (quoted, operator-named,
//! private, in a nested module, in a headerless file), union, enum and
//! `exception` cases, active-pattern cases, and the exports a signature
//! file materialises at its implementation's slot, contested names among
//! them (`docs/sema-interning-plan.md`, Stage 6c).
//!
//! A name is published in its logical domain: backticks stripped, an
//! operator compiled. A value path is `None` where a later file has no
//! module-qualified way to reach the export.

use borzoi_sema::{AssemblyEnv, CaseKind, ResolvedFile, resolve_project_files};

use crate::resolve_signatures::project;

/// One export as a later file sees it: its name, its value-namespace path,
/// its case kind, and whether its binder lives in the paired signature.
type Published = (String, Option<Vec<String>>, Option<CaseKind>, bool);

fn published(rf: &ResolvedFile, env: &AssemblyEnv) -> Vec<Published> {
    rf.exports()
        .iter()
        .map(|e| {
            (
                e.name(env).to_string(),
                e.qualified_path(env),
                e.case_kind(),
                e.def().is_none(),
            )
        })
        .collect()
}

fn path(segments: &[&str]) -> Option<Vec<String>> {
    Some(segments.iter().map(|s| s.to_string()).collect())
}

fn row(
    name: &str,
    qualified: Option<Vec<String>>,
    case: Option<CaseKind>,
    from_signature: bool,
) -> Published {
    (name.to_string(), qualified, case, from_signature)
}

const IMPL: &str = "module Lib.A

let ``my value`` = 1
let (+.) a b = a + b
let private hidden = 2
type U =
    | Plain
    | ``Quoted Case``
[<RequireQualifiedAccess>]
type R = X | Y
type Color = Red = 0 | Green = 1
exception Boom of int
let (|Even|Odd|) n = if n % 2 = 0 then Even else Odd
module Inner =
    let deep = 3
    let ``deep quoted`` = 4
";

const SIG: &str = "module Lib.S

val ``quoted val`` : int
val (+!) : int -> int -> int
val plain : int
type V =
    | P
    | ``Q q``
type T = | Dup
val Dup : int
module Nested =
    val inner : int
";

const SIG_IMPL: &str = "module Lib.S

let ``quoted val`` = 1
let (+!) a b = a + b
let plain = 2
type V =
    | P
    | ``Q q``
type T = | Dup
let Dup = 3
module Nested =
    let inner = 4
";

const HEADERLESS: &str = "let anon = 1
let ``anon quoted`` = 2
";

/// The fixture's Compile order: an implementation, a signature pair, and a
/// headerless file.
pub(crate) const FIXTURE: &[(&str, &str)] = &[
    ("A.fs", IMPL),
    ("S.fsi", SIG),
    ("S.fs", SIG_IMPL),
    ("Anon.fs", HEADERLESS),
];

#[test]
fn every_export_publishes_its_logical_name_and_value_path() {
    let env = AssemblyEnv::default();
    let files = project(FIXTURE);
    let resolved = resolve_project_files(&files, &env);
    let union = Some(CaseKind::Union {
        require_qualified: false,
    });
    let rqa = Some(CaseKind::Union {
        require_qualified: true,
    });
    assert_eq!(
        published(resolved.file(0), &env),
        vec![
            row("my value", path(&["Lib", "A", "my value"]), None, false),
            row("op_PlusDot", path(&["Lib", "A", "op_PlusDot"]), None, false),
            row("hidden", path(&["Lib", "A", "hidden"]), None, false),
            row("Plain", path(&["Lib", "A", "Plain"]), union, false),
            row(
                "Quoted Case",
                path(&["Lib", "A", "Quoted Case"]),
                union,
                false
            ),
            row("X", None, rqa, false),
            row("Y", None, rqa, false),
            row("Red", None, Some(CaseKind::Enum), false),
            row("Green", None, Some(CaseKind::Enum), false),
            row(
                "Boom",
                path(&["Lib", "A", "Boom"]),
                Some(CaseKind::Exception),
                false
            ),
            row("Even", None, None, false),
            row("Odd", None, None, false),
            row("deep", path(&["Lib", "A", "Inner", "deep"]), None, false),
            row(
                "deep quoted",
                path(&["Lib", "A", "Inner", "deep quoted"]),
                None,
                false
            ),
        ],
    );
    assert_eq!(published(resolved.file(1), &env), vec![]);
    assert_eq!(
        published(resolved.file(2), &env),
        vec![
            // The implementation's own exports, which the signature screens.
            row("quoted val", path(&["Lib", "S", "quoted val"]), None, false),
            row(
                "op_PlusBang",
                path(&["Lib", "S", "op_PlusBang"]),
                None,
                false
            ),
            row("plain", path(&["Lib", "S", "plain"]), None, false),
            row("P", path(&["Lib", "S", "P"]), union, false),
            row("Q q", path(&["Lib", "S", "Q q"]), union, false),
            row("Dup", path(&["Lib", "S", "Dup"]), union, false),
            row("Dup", path(&["Lib", "S", "Dup"]), None, false),
            row("inner", path(&["Lib", "S", "Nested", "inner"]), None, false),
            // The signature's, materialised here.
            row("quoted val", path(&["Lib", "S", "quoted val"]), None, true),
            row(
                "op_PlusBang",
                path(&["Lib", "S", "op_PlusBang"]),
                None,
                true
            ),
            row("plain", path(&["Lib", "S", "plain"]), None, true),
            row("P", path(&["Lib", "S", "P"]), union, true),
            row("Q q", path(&["Lib", "S", "Q q"]), union, true),
            // Two declarations put `Dup` in value scope: no bare identity.
            row("Dup", None, union, true),
            row("Dup", None, None, true),
            // A nested module's `val` is outside the modelled signature
            // surface, so it materialises no export.
        ],
    );
    assert_eq!(
        published(resolved.file(3), &env),
        vec![
            row("anon", None, None, false),
            row("anon quoted", None, None, false),
        ],
    );
}

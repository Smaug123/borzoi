//! Infix operators over ground operands, against FCS.
//!
//! Two properties, graded against FCS's typed tree:
//!
//! 1. **Over FSharp.Core's operators, the rule's type.** With both operands
//!    ground, arithmetic is typed as the operands and a comparison is `bool`.
//! 2. **A redefinition anywhere withholds the rule.** An operator can be
//!    redefined by many routes — in the file under its spelling or compiled
//!    name, in a local, in a `module rec` after its use, in an opened,
//!    auto-opened or alias-auto-opened module, in an earlier file, in another
//!    assembly's module, through an assembly-level auto-open — and FCS then
//!    types a use by that definition. Every route here redefines the operator
//!    with a *different* result type, so a use typed by FSharp.Core's rule
//!    shows up as a wrong type; and each case also pins that FCS really does
//!    take the redefinition, or it would prove nothing.

use crate::common::{
    full_bcl_env, invoke_fcs_dump, parse_fcs_binder_types_with_errors, parse_fcs_types_with_errors,
    temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, SyntaxRecovery, infer_file, resolve_file};

/// What one file committed: expression types, binder types, and — of the
/// expression types — how many sit at a whole infix application.
#[derive(Debug, Default, Clone, Copy)]
struct Committed {
    exprs: usize,
    binders: usize,
    operator_nodes: usize,
}

/// Resolve, infer and grade `src` against both typed-tree oracles: every
/// committed expression and binder type must be FCS's, at the same range.
fn check_types(src: &str) -> Committed {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}\n{src}", parsed.errors);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = full_bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env);

    let path = temp_fs_file("infer_operators_types", src);
    let types_json = invoke_fcs_dump("types", &path);
    let binders_json = invoke_fcs_dump("binder-types", &path);
    let _ = std::fs::remove_file(&path);
    let (fcs_types, _) = parse_fcs_types_with_errors(&types_json, src);
    let (fcs_binders, _) = parse_fcs_binder_types_with_errors(&binders_json, src);

    let infix_ranges: std::collections::HashSet<(usize, usize)> = file
        .syntax()
        .descendants()
        .filter_map(borzoi_cst::syntax::AppExpr::cast)
        .filter(|app| {
            app.func().is_some_and(|f| {
                f.syntax().kind() == borzoi_cst::syntax::SyntaxKind::INFIX_APP_EXPR
            })
        })
        .map(|app| {
            let r = app.syntax().text_range();
            let text = &src[u32::from(r.start()) as usize..u32::from(r.end()) as usize];
            let lead = text.len() - text.trim_start().len();
            let trail = text.len() - text.trim_end().len();
            (
                u32::from(r.start()) as usize + lead,
                u32::from(r.end()) as usize - trail,
            )
        })
        .collect();

    let mut c = Committed::default();
    for (range, ty) in inferred.types() {
        let key = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        let theirs = fcs_types.get(&key).unwrap_or_else(|| {
            panic!(
                "we inferred `{}` at `{}` but FCS has no node there\n{src}",
                ty.render(),
                &src[key.0..key.1]
            )
        });
        assert_eq!(&ty.render(), theirs, "at `{}`\n{src}", &src[key.0..key.1]);
        c.exprs += 1;
        if infix_ranges.contains(&key) {
            c.operator_nodes += 1;
        }
    }
    for (def_id, ty) in inferred.def_types() {
        let def = resolved.def(*def_id);
        let key = (
            u32::from(def.range.start()) as usize,
            u32::from(def.range.end()) as usize,
        );
        let theirs = fcs_binders.get(&key).unwrap_or_else(|| {
            panic!(
                "we inferred `{}` for `{}` but FCS has no binder\n{src}",
                ty.render(),
                def.name
            )
        });
        assert_eq!(&ty.render(), theirs, "binder `{}`\n{src}", def.name);
        c.binders += 1;
    }
    c
}

/// Arithmetic over one numeric type (and `+` over strings and chars) is typed
/// as the operands; a comparison over a comparable type is `bool`; and a
/// function whose operands are ground stays complete, so it publishes.
#[test]
fn operators_over_ground_operands_type_like_fcs() {
    let c = check_types(
        "module M\n\
         let a = 1 + 2 * 3 - 4 / 5 % 6\n\
         let b = \"x\" + \"y\"\n\
         let c = 'a' + 'b'\n\
         let d = 1.5 * 2.0\n\
         let e = (1, \"s\") = (2, \"t\")\n\
         let f (s: string) = s + \"!\"\n\
         let g (x: int) = x >= 0\n",
    );
    // `a`'s five operators, `b`, `c`, `d`, `e`, `f`'s and `g`'s bodies.
    assert_eq!(c.operator_nodes, 11, "{c:?}");
    // `a`…`e` and the functions `f : string -> string`, `g : int -> bool`.
    assert_eq!(c.binders, 7, "{c:?}");
}

/// An operand that is not ground when the operator is walked is one FCS may
/// fix through the operator (`x + 1` makes `x` an `int`), a constraint we do
/// not model: the operator commits nothing, and the binding is incomplete.
#[test]
fn an_open_operand_commits_nothing() {
    let c = check_types("module M\nlet h x = x + 1\nlet k y = y = y\n");
    assert_eq!((c.operator_nodes, c.binders), (0, 0), "{c:?}");
}

/// Mismatched operands are an FCS error, recovered as it likes (`1 = "s"` puts
/// `int` at `"s"`): nothing inside such an application is committed.
#[test]
fn mismatched_operands_commit_nothing() {
    let c = check_types("module M\nlet m = 1 = \"s\"\nlet n = 1.0 < 2\n");
    assert_eq!(c.exprs, 0, "{c:?}");
}

/// An operator another assembly redefines, reached through an `open`: FCS types
/// `1 + 2` by that definition (`string`). The resolver finds the fixture's
/// member, and only the FSharp.Core identity check keeps inference from typing
/// it by FSharp.Core's rule (`int`).
#[test]
fn another_assemblys_operator_is_not_typed_by_fsharp_cores_rule() {
    let src = "module M\nopen OpsFixture.StringOps\nlet k = 1 + 2\n";
    let parsed = parse(src);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = crate::common::operators_fixture_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env);

    let path = temp_fs_file("infer_operators_fixture", src);
    let dll = crate::common::ensure_operators_fixture_built();
    let types_json = crate::common::invoke_fcs_dump_with_refs("types", &path, &[dll]);
    let _ = std::fs::remove_file(&path);
    let (fcs_types, _) = parse_fcs_types_with_errors(&types_json, src);
    let i = src.find("1 + 2").expect("the application");
    assert_eq!(
        fcs_types.get(&(i, i + 5)).map(String::as_str),
        Some("System.String"),
        "FCS types the fixture's operator by its own definition"
    );
    let ours = inferred
        .types()
        .iter()
        .find(|(r, _)| (u32::from(r.start()) as usize, u32::from(r.end()) as usize) == (i, i + 5));
    assert!(
        ours.is_none_or(|(_, ty)| ty.render() == "System.String"),
        "we typed the fixture's operator by FSharp.Core's rule: {ours:?}"
    );
}

/// An operator application in callee position (`(1 + 2) 3`) is applied as a
/// function, which FCS rejects, keeping no node inside it. Its operands are
/// walked in synth mode, so the callee path must discard what they record —
/// nodes and local binders alike.
#[test]
fn an_operator_application_used_as_a_callee_records_nothing_inside() {
    let c = check_types("module M\nlet a = (1 + 2) 3\nlet b = ((let y = 1 in y) + 2) 3\n");
    assert_eq!(c.exprs, 0, "{c:?}");
}

/// Each route by which `+` (or `<`) can be redefined in a single file, with a
/// `string` result, and the application to check. FCS types the application
/// `string` on every route (asserted), so FSharp.Core's rule (`int`, `bool`)
/// would be a wrong type.
const REDEFINITION_ROUTES: [(&str, &str); 7] = [
    // In the file, under the spelling.
    (
        "module M\nlet (+) (a: int) (b: int) = \"s\"\nlet k = 1 + 2\n",
        "1 + 2",
    ),
    // In the file, under the compiled name.
    (
        "module M\nlet op_Addition (a: int) (b: int) = \"s\"\nlet k = 1 + 2\n",
        "1 + 2",
    ),
    // In a local.
    (
        "module M\nlet k = (let (+) (a: int) (b: int) = \"s\" in 1 + 2)\n",
        "1 + 2",
    ),
    // In a `module rec`, after the use.
    (
        "module rec M\nlet k = 1 + 2\nlet (+) (a: int) (b: int) = \"s\"\n",
        "1 + 2",
    ),
    // In an opened module.
    (
        "module M\nmodule Ops =\n    let (+) (a: int) (b: int) = \"s\"\nopen Ops\nlet k = 1 + 2\n",
        "1 + 2",
    ),
    // In an auto-opened module.
    (
        "module M\n[<AutoOpen>]\nmodule Ops =\n    let (<) (a: int) (b: int) = \"s\"\nlet k = 1 < 2\n",
        "1 < 2",
    ),
    // In a module auto-opened through an aliased attribute (FCS warns, FS3561).
    (
        "module M\ntype AO = Microsoft.FSharp.Core.AutoOpenAttribute\n[<AO>]\nmodule Ops =\n    \
         let (+) (a: int) (b: int) = \"s\"\nlet k = 1 + 2\n",
        "1 + 2",
    ),
];

#[test]
fn every_single_file_redefinition_route_withholds_the_rule() {
    for (src, app) in REDEFINITION_ROUTES {
        let path = temp_fs_file("infer_operators_route", src);
        let json = invoke_fcs_dump("types", &path);
        let _ = std::fs::remove_file(&path);
        let (fcs, _) = parse_fcs_types_with_errors(&json, src);
        let i = src.find(app).expect("the application");
        assert_eq!(
            fcs.get(&(i, i + app.len())).map(String::as_str),
            Some("System.String"),
            "the route must really redefine the operator, or it proves nothing\n{src}"
        );
        // Every commit is graded against FCS, so FSharp.Core's rule at `app`
        // fails here.
        check_types(src);
    }
}

/// An operator redefined in an **earlier file**'s auto-open module, reached
/// through an `open` of its enclosing module: FCS binds the use to it, so the
/// application must not take FSharp.Core's rule (`bool`) — the redefinition
/// returns a `string`.
#[test]
fn an_operator_redefined_in_an_earlier_file_withholds_the_rule() {
    let a = "module A\n[<AutoOpen>]\nmodule Ops =\n    let (<) (x: int) (y: int) = \"s\"\n";
    let b = "module B\nopen A\nlet c = 1 < 2\n";
    let written: Vec<(std::path::PathBuf, String)> = [("ops_a", a), ("ops_b", b)]
        .iter()
        .map(|(label, src)| (temp_fs_file(label, src), (*src).to_string()))
        .collect();
    let paths: Vec<&std::path::Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = crate::common::invoke_fcs_dump_project(&paths);
    let fcs = crate::common::parse_fcs_uses_project(&json, &written);
    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }
    let i = b.rfind('<').expect("the use");
    let fcs_b = fcs
        .iter()
        .find(|f| f.path.file_name() == written[1].0.file_name())
        .expect("FCS uses for B");
    let fcs_use = fcs_b
        .uses
        .iter()
        .find(|u| (u.start, u.end) == (i, i + 1))
        .expect("FCS use at `<`");
    assert_eq!(fcs_use.full_name.as_deref(), Some("A.Ops.(<)"));

    let asts: Vec<ImplFile> = [a, b]
        .iter()
        .map(|src| ImplFile::cast(parse(src).root).expect("impl file"))
        .collect();
    let env = full_bcl_env();
    let proj = borzoi_sema::resolve_project(&asts, env);
    let inferred = infer_file(&asts[1], proj.file(1), env);
    let app = b.find("1 < 2").expect("the application");
    let ours = inferred.types().iter().find(|(r, _)| {
        (u32::from(r.start()) as usize, u32::from(r.end()) as usize) == (app, app + 5)
    });
    assert!(
        ours.is_none(),
        "typed a redefined operator by FSharp.Core's rule: {ours:?}"
    );
}

/// An operator another assembly redefines through an **assembly-level
/// auto-open** (`[<assembly: AutoOpen("OpsFixture.ManifestOps")>]`): with no
/// `open`, FCS types `3 - 1` by that definition (`string`).
#[test]
fn an_assembly_level_auto_open_redefinition_withholds_the_rule() {
    let src = "module M\nlet k = 3 - 1\n";
    let parsed = parse(src);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = crate::common::operators_fixture_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(&file, &resolved, env);

    let path = temp_fs_file("infer_operators_manifest", src);
    let dll = crate::common::ensure_operators_fixture_built();
    let types_json = crate::common::invoke_fcs_dump_with_refs("types", &path, &[dll]);
    let _ = std::fs::remove_file(&path);
    let (fcs_types, _) = parse_fcs_types_with_errors(&types_json, src);
    let i = src.find("3 - 1").expect("the application");
    assert_eq!(
        fcs_types.get(&(i, i + 5)).map(String::as_str),
        Some("System.String"),
        "FCS types the auto-opened operator by its own definition"
    );
    let ours = inferred
        .types()
        .iter()
        .find(|(r, _)| (u32::from(r.start()) as usize, u32::from(r.end()) as usize) == (i, i + 5));
    assert!(
        ours.is_none(),
        "typed a redefined operator by FSharp.Core's rule: {ours:?}"
    );
}

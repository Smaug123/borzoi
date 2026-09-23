//! Infix operators over ground operands, against FCS.
//!
//! Two layers, each graded on its own:
//!
//! 1. **Which operator a token is.** An operator can be redefined — in the
//!    file, under its spelling (`let (+) a b = …`) or its compiled name
//!    (`let op_Addition a b = …`), or in an opened or auto-opened module — and
//!    FCS then uses that definition. [`borzoi_sema::ResolvedFile::operator_target_at`]
//!    records what the token resolves to; inference trusts it only when it is
//!    FSharp.Core's own member. The differential grades that claim against the
//!    symbol FCS reports at the token, over files that shadow and files that do
//!    not.
//! 2. **What the application's type is.** With the operator proven FSharp.Core's
//!    and both operands ground, the result follows from FSharp.Core's typing
//!    rules; the expression and binder types are graded against FCS's typed
//!    tree like every other inference differential.

use crate::common::{
    full_bcl_env, invoke_fcs_dump, parse_fcs_binder_types_with_errors, parse_fcs_types_with_errors,
    parse_fcs_uses, temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, infer_file, resolve_file,
};

/// The operators the table models, and the FSharp.Core symbol FCS reports for
/// each when it is FSharp.Core's.
const OPERATORS: [(&str, &str); 10] = [
    ("+", "Microsoft.FSharp.Core.Operators.(+)"),
    ("-", "Microsoft.FSharp.Core.Operators.(-)"),
    ("*", "Microsoft.FSharp.Core.Operators.(*)"),
    ("/", "Microsoft.FSharp.Core.Operators.(/)"),
    ("=", "Microsoft.FSharp.Core.Operators.(=)"),
    ("<>", "Microsoft.FSharp.Core.Operators.(<>)"),
    ("<", "Microsoft.FSharp.Core.Operators.(<)"),
    (">", "Microsoft.FSharp.Core.Operators.(>)"),
    ("<=", "Microsoft.FSharp.Core.Operators.(<=)"),
    (">=", "Microsoft.FSharp.Core.Operators.(>=)"),
];

/// The F# source full name of the assembly member `res` names, or `None` for
/// anything else.
fn member_full_name(env: &AssemblyEnv, res: Resolution) -> Option<String> {
    let Resolution::Member { parent, idx } = res else {
        return None;
    };
    Some(format!(
        "{}.{}",
        env.entity_full_name(parent),
        env.member_display_name(parent, idx)
    ))
}

/// The compiled name FSharp.Core gives an operator's spelling.
fn compiled(spelling: &str) -> &'static str {
    match spelling {
        "+" => "op_Addition",
        "-" => "op_Subtraction",
        "*" => "op_Multiply",
        "/" => "op_Division",
        "=" => "op_Equality",
        "<>" => "op_Inequality",
        "<" => "op_LessThan",
        ">" => "op_GreaterThan",
        "<=" => "op_LessThanOrEqual",
        ">=" => "op_GreaterThanOrEqual",
        other => panic!("no compiled name for {other}"),
    }
}

/// Every operator claim we make in `src` agrees with FCS: where we say a token
/// is FSharp.Core's operator, FCS's symbol at that token is that operator.
/// Returns each operator token's start offset and whether we claimed it.
fn check_operator_targets(src: &str) -> Vec<(usize, bool)> {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}\n{src}", parsed.errors);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = full_bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);

    let path = temp_fs_file("infer_operators", src);
    let json = invoke_fcs_dump("uses", &path);
    let _ = std::fs::remove_file(&path);
    let uses = parse_fcs_uses(&json, src);

    let mut outcomes = Vec::new();
    for (range, target) in resolved.operator_targets() {
        let key = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        let spelling = &src[key.0..key.1];
        let Some(ours) = member_full_name(env, *target) else {
            outcomes.push((key.0, false));
            continue;
        };
        let expected_core = OPERATORS
            .iter()
            .find(|(s, _)| *s == spelling)
            .map(|(_, fcs)| *fcs)
            .expect("an operator in the table");
        // Our claim names the FSharp.Core member by compiled name; FCS by display.
        assert_eq!(
            ours,
            expected_core.replace(&format!("({spelling})"), compiled(spelling)),
            "operator `{spelling}` at {key:?} resolved to an unexpected member\n{src}"
        );
        let fcs = uses
            .iter()
            .find(|u| (u.start, u.end) == key)
            .unwrap_or_else(|| panic!("FCS reports no use at `{spelling}` {key:?}\n{src}"));
        assert_eq!(
            fcs.full_name.as_deref(),
            Some(expected_core),
            "we say `{spelling}` at {key:?} is FSharp.Core's; FCS disagrees\n{src}"
        );
        outcomes.push((key.0, true));
    }
    outcomes.sort_unstable();
    outcomes
}

/// With nothing redefined, every operator is FSharp.Core's.
#[test]
fn core_operators_resolve_to_fsharp_core() {
    let src = "module M\n\
               let a = 1 + 2 - 3 * 4 / 5\n\
               let b = (1 = 2, 1 <> 2, 1 < 2, 1 > 2, 1 <= 2, 1 >= 2)\n";
    let outcomes = check_operator_targets(src);
    assert_eq!(outcomes.len(), 10, "{outcomes:?}");
    assert!(outcomes.iter().all(|(_, claimed)| *claimed), "{outcomes:?}");
}

/// Every way of redefining an operator withholds the FSharp.Core claim: in the
/// file under its spelling or its compiled name, in a local, and in an opened
/// or auto-opened module.
#[test]
fn a_redefined_operator_is_not_fsharp_cores() {
    for (defs, body) in [
        ("let (+) (a: int) (b: int) = a - b\n", "let a = 1 + 2\n"),
        (
            "let op_Addition (a: int) (b: int) = a - b\n",
            "let a = 1 + 2\n",
        ),
        ("", "let a = (let (+) x y = x - y in 1 + 2)\n"),
        (
            "module Ops =\n    let (=) (a: int) (b: int) = false\nopen Ops\n",
            "let a = 1 = 2\n",
        ),
        (
            "[<AutoOpen>]\nmodule Ops =\n    let (<) (a: int) (b: int) = false\n",
            "let a = 1 < 2\n",
        ),
    ] {
        let src = format!("module M\n{defs}{body}");
        let outcomes = check_operator_targets(&src);
        // The redefined operator's use is the last operator token in the file.
        let (_, claimed) = outcomes.last().expect("an operator token");
        assert!(!claimed, "a redefined operator must not be claimed\n{src}");
    }
}

/// A preceding file's `[<AutoOpen>]` module that redefines an operator: FCS
/// binds the use in the next file to it, so we must not claim FSharp.Core's.
#[test]
fn an_operator_redefined_in_a_preceding_auto_open_module_is_not_fsharp_cores() {
    let a = "module A\n[<AutoOpen>]\nmodule Ops =\n    let (<) (x: int) (y: int) = false\n";
    let b = "module B\nopen A\nlet c = 1 < 2\n";
    let written: Vec<(std::path::PathBuf, String)> = [("ops_a", a), ("ops_b", b)]
        .iter()
        .map(|(label, src)| (temp_fs_file(label, src), (*src).to_string()))
        .collect();
    let paths: Vec<&std::path::Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = crate::common::invoke_fcs_dump_project(&paths);
    let fcs = crate::common::parse_fcs_uses_project(&json, &written);
    let asts: Vec<ImplFile> = [a, b]
        .iter()
        .map(|src| ImplFile::cast(parse(src).root).expect("impl file"))
        .collect();
    let env = full_bcl_env();
    let proj = borzoi_sema::resolve_project(&asts, env);
    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }
    let i = b.rfind('<').expect("the use");
    let range = rowan::TextRange::new((i as u32).into(), ((i + 1) as u32).into());
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
    let ours = proj.file(1).operator_target_at(range);
    assert!(
        ours.and_then(|res| member_full_name(env, res)).is_none(),
        "we claimed an assembly operator where FCS binds A.Ops.(<): {ours:?}"
    );
}

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

/// A redefined operator is typed by its definition, not FSharp.Core's rule.
#[test]
fn a_redefined_operator_is_not_typed_by_fsharp_cores_rule() {
    let c = check_types("module M\nlet (+) (a: string) (b: string) = 1\nlet k = \"a\" + \"b\"\n");
    assert_eq!(c.operator_nodes, 0, "{c:?}");
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

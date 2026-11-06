//! Union-case values from a referenced assembly — `None`, `Some x`,
//! `Ok v`, `Choice2Of3 s`, and a case as a first-class function — graded
//! against FCS.
//!
//! A case's type is read off the static maker fsc compiles it to
//! (`union_case_scheme`): its fields and the union they build, over the
//! union's type parameters. The grid crosses case expressions with the
//! positions a value sits in (a value binding, a function body, a checked
//! branch, an annotated return, a tuple, an argument), and asserts
//! certain-implies-exact on every node and binder we type, plus a must-commit
//! obligation on each cell whose case and context are modelled and FCS checks
//! cleanly — so a decline that creeps in fails as surely as a wrong type.

use std::path::Path;

use crate::common::{
    GradedDumps, full_bcl_env, module_value_fixture_env, module_value_fixture_refs, temp_fs_file,
    try_invoke_fcs_dump, try_invoke_fcs_dump_with_refs,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, SyntaxRecovery, infer_file, resolve_file};

const HEADER: &str = "module M\n";

/// `(id, expression, modelled)`: a case expression, and whether this stage
/// types it. `{x}` is the cell's parameter, an `int`.
const CASES: &[(&str, &str, bool)] = &[
    ("none", "None", true),
    ("someLit", "Some 1", true),
    ("someStr", "Some \"s\"", true),
    ("someParam", "Some {x}", true),
    ("someSome", "Some (Some 1)", true),
    ("someNone", "Some None", true),
    ("someParen", "(Some 2)", true),
    ("vnone", "ValueNone", true),
    ("vsome", "ValueSome 1", true),
    ("ok", "Ok 1", true),
    ("error", "Error \"e\"", true),
    ("okSome", "Ok (Some 1)", true),
    ("choice1", "Choice1Of2 1", true),
    ("choice2of3", "Choice2Of3 \"s\"", true),
    ("someFn", "Some", true),
    ("okFn", "Ok", true),
    ("qualified", "Option.Some 1", false),
    ("fullyQualified", "Microsoft.FSharp.Core.Option.None", false),
    ("someTuple", "Some (1, \"a\")", true),
    ("someUnit", "Some ()", true),
    ("someLambda", "Some (fun y -> y)", false),
    ("someList", "Some [1]", false),
    ("cons", "1 :: []", false),
];

/// `(id, template, modelled)`: where the expression `{e}` sits. `{i}` is the
/// cell's index; a template that names `{x}` declares it an `int` parameter.
const CONTEXTS: &[(&str, &str, bool)] = &[
    ("value", "let v{i} = {e}\n", true),
    ("unitFn", "let f{i} () = {e}\n", true),
    ("param", "let f{i} ({x}: int) = {e}\n", true),
    ("generic", "let f{i} {x} = {e}\n", true),
    ("tuple", "let f{i} ({x}: int) = ({e}, {e})\n", true),
    ("ignore", "let f{i} ({x}: int) = ignore ({e})\n", true),
    ("id", "let f{i} ({x}: int) = id ({e})\n", true),
    (
        "branch",
        "let f{i} ({x}: int) (b: bool) = if b then {e} else {e}\n",
        true,
    ),
    // A local bound to an application is open until the argument check is
    // solved (`OpenLocal`), whatever the callee: not this stage's to lift.
    (
        "local",
        "let f{i} ({x}: int) =\n    let r = {e}\n    r\n",
        false,
    ),
    (
        "annotated",
        "let f{i} ({x}: int) : int option = {e}\n",
        true,
    ),
];

/// The cases whose type keeps a variable of the union's (`None` is
/// `'a option`). FCS generalises a value binding of one (a case applied to
/// generalisable arguments is generalisable), and unifies two `if` branches
/// of one; inference generalises no value binding and relates an `else` to
/// its `then` only by a check, so neither context is obligated for them.
const GENERIC_RESULT: &[&str] = &[
    "none",
    "someNone",
    "vnone",
    "ok",
    "error",
    "okSome",
    "choice1",
    "choice2of3",
    "someFn",
    "okFn",
];

#[derive(Default, Debug)]
struct Tally {
    cells: usize,
    committed: usize,
    exprs: usize,
    binders: usize,
    missed: Vec<String>,
    uncheckable: Vec<String>,
}

/// One cell: its program line, binder name, obligation and label.
type Cell = (u32, String, bool, String);

fn check(
    source: &str,
    cells: &[Cell],
    tally: &mut Tally,
    env: &borzoi_sema::AssemblyEnv,
    refs: &[&Path],
) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {:?}\n{source}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(
        &file,
        &resolved,
        env,
        &borzoi_sema::PrecedingTypes::default(),
    );
    let path = temp_fs_file("infer_union_case", source);
    let (types_json, binders_json) = if refs.is_empty() {
        (
            try_invoke_fcs_dump("types", &path),
            try_invoke_fcs_dump("binder-types", &path),
        )
    } else {
        (
            try_invoke_fcs_dump_with_refs("types", &path, refs),
            try_invoke_fcs_dump_with_refs("binder-types", &path, refs),
        )
    };
    let _ = std::fs::remove_file(&path);
    let (Ok(types_json), Ok(binders_json)) = (types_json, binders_json) else {
        tally.uncheckable.extend(cells.iter().map(|c| c.3.clone()));
        return;
    };
    let dumps = GradedDumps::read(&types_json, &binders_json, source, Some(&file))
        .expect("every declaration is placeable");
    for (range, ty) in inferred.types() {
        let key = (
            u32::from(range.start()) as usize,
            u32::from(range.end()) as usize,
        );
        if dumps.is_ungraded(key) {
            continue;
        }
        let ours = ty.render();
        let theirs = dumps.types.get(&key).unwrap_or_else(|| {
            panic!(
                "we inferred `{ours}` at {key:?} (`{}`) but FCS reports no node there\n{source}",
                &source[key.0..key.1]
            )
        });
        assert_eq!(
            &ours,
            theirs,
            "expression type mismatch at {key:?} (`{}`)\n{source}",
            &source[key.0..key.1]
        );
        tally.exprs += 1;
    }
    let line_of = |offset: usize| source[..offset].matches('\n').count() as u32 + 1;
    let mut ours_by_name = std::collections::HashMap::new();
    for (def_id, ty) in inferred.def_types() {
        let def = resolved.def(*def_id);
        let key = (
            u32::from(def.range.start()) as usize,
            u32::from(def.range.end()) as usize,
        );
        if dumps.is_ungraded(key) {
            continue;
        }
        let ours = ty.render();
        let theirs = dumps.binders.get(&key).unwrap_or_else(|| {
            panic!(
                "we inferred `{ours}` for binder `{}` at {key:?} but FCS reports no binder \
                 there\n{source}",
                def.name
            )
        });
        assert_eq!(
            &ours,
            theirs,
            "binder type mismatch for `{}` at {key:?} (line {})\n{source}",
            def.name,
            line_of(key.0)
        );
        assert!(
            !dumps.hides_constraint(key, &ours),
            "binder `{}` typed `{ours}`, which FCS constrains\n{source}",
            def.name
        );
        tally.binders += 1;
        ours_by_name.insert((def.name.to_string(), line_of(key.0)), ours);
    }
    for (line, name, obligated, label) in cells {
        tally.cells += 1;
        let erroring = dumps.errors.iter().any(|e| e.line == *line);
        match ours_by_name.get(&(name.clone(), *line)) {
            Some(_) => tally.committed += 1,
            None if *obligated && !erroring => {
                let fcs = dumps.binders.iter().find_map(|(&(s, e), ty)| {
                    (line_of(s) == *line && &source[s..e] == name).then(|| ty.clone())
                });
                tally
                    .missed
                    .push(format!("{label}: FCS `{}`", fcs.unwrap_or_default()));
            }
            None => {}
        }
    }
}

/// The program for one context: every case in it, one declaration each.
fn program(
    header: &str,
    cases: &[(&str, &str, bool)],
    context: &(&str, &str, bool),
) -> (String, Vec<Cell>) {
    let mut src = header.to_string();
    let mut cells = Vec::new();
    for (i, (cid, expr, modelled)) in cases.iter().enumerate() {
        // The annotated return types every case at `int option`; only the
        // cases of that type belong there.
        if context.0 == "annotated"
            && !matches!(*cid, "none" | "someLit" | "someParam" | "someParen")
        {
            continue;
        }
        let line = src.matches('\n').count() as u32 + 1;
        let decl = context
            .1
            .replace("{e}", expr)
            .replace("{x}", "x")
            .replace("{i}", &i.to_string());
        let name = if context.1.starts_with("let v") {
            format!("v{i}")
        } else {
            format!("f{i}")
        };
        cells.push((
            line,
            name,
            *modelled
                && context.2
                && !(matches!(context.0, "value" | "branch") && GENERIC_RESULT.contains(cid)),
            format!("{}/{cid}", context.0),
        ));
        src.push_str(&decl);
    }
    (src, cells)
}

#[test]
fn every_case_in_every_context_agrees_with_fcs() {
    let mut tally = Tally::default();
    for context in CONTEXTS {
        let (src, cells) = program(HEADER, CASES, context);
        check(&src, &cells, &mut tally, full_bcl_env(), &[]);
    }
    eprintln!("{tally:?}");
    assert!(tally.uncheckable.is_empty(), "{:?}", tally.uncheckable);
    assert!(
        tally.missed.is_empty(),
        "{} obligated cells declined:\n{}",
        tally.missed.len(),
        tally.missed.join("\n")
    );
    assert!(tally.committed >= 100, "{tally:?}");
}

/// Cases of the fixture library's unions (`module_value_corpus`): those the
/// maker reads exactly — a sealed, an unsealed and a generic field, several
/// fields (applied to a syntactic tuple, to a tuple-typed value, and
/// unapplied) — and one per guard of `union_case_scheme`: a measure direct
/// and behind an abbreviation, an error-level obsolete case, a constrained
/// type parameter. Each guard's rows are what a deletion of it breaks.
const FIXTURE_CASES: &[(&str, &str, bool)] = &[
    ("ustr", "UStr \"s\"", true),
    ("ustrFn", "UStr", true),
    ("ugen", "UGen 1", true),
    ("ugenFn", "UGen", true),
    ("unone", "UNone", true),
    ("unew", "UNew 1", true),
    ("uobj", "UObj (box 1)", true),
    ("uobjFn", "UObj", true),
    ("upair", "UPair (1, \"a\")", true),
    ("upairFn", "UPair", true),
    ("upairTupleValue", "UPair (id (1, \"a\"))", true),
    ("uobjStr", "UObj \"s\"", true),
    ("uobjParam", "UObj {x}", true),
    ("umeasure", "UMeasure", false),
    ("udist", "UDist", false),
    ("uold", "UOld 1", false),
    ("uoldFn", "UOld", false),
    ("ucon", "UCon", false),
    ("uconLit", "UCon 1", false),
];

#[test]
fn fixture_union_cases_agree_with_fcs_and_decline_what_the_maker_cannot_vouch_for() {
    let header = "module M\nopen FSharpModuleValues\n";
    let refs = module_value_fixture_refs();
    let mut tally = Tally::default();
    for context in CONTEXTS
        .iter()
        .filter(|c| matches!(c.0, "unitFn" | "param" | "generic"))
    {
        let (src, mut cells) = program(header, FIXTURE_CASES, context);
        // An open parameter passed where an unsealed type is expected stays
        // open in FCS (`'a :> obj`, trivially met); an application's argument
        // check cannot relate the two whatever the callee, so it declines.
        for cell in &mut cells {
            if cell.3 == "generic/uobjParam" {
                cell.2 = false;
            }
        }
        check(&src, &cells, &mut tally, module_value_fixture_env(), &refs);
    }
    eprintln!("{tally:?}");
    assert!(tally.uncheckable.is_empty(), "{:?}", tally.uncheckable);
    assert!(
        tally.missed.is_empty(),
        "{} obligated cells declined:\n{}",
        tally.missed.len(),
        tally.missed.join("\n")
    );
    assert!(tally.committed >= 15, "{tally:?}");
}

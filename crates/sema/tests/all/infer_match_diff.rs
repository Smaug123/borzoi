//! `match` over a referenced assembly's union cases, constants, tuples and
//! names, graded against FCS.
//!
//! FCS checks a `match` in source order (`TcMatchClauses`): the scrutinee,
//! then each clause's pattern against the scrutinee's type, its guard as a
//! `bool`, and its result. The grid crosses scrutinee shapes with clause sets
//! — well-typed ones, and ill-typed ones whose rejected clause FCS drops —
//! and positions (a body, a checked argument, an annotated return), and holds
//! every node and binder we type to FCS's, with a must-commit obligation on
//! each modelled cell FCS checks cleanly.

use crate::common::{
    GradedDumps, full_bcl_env, module_value_fixture_env, temp_fs_file, try_invoke_fcs_dump,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, SyntaxRecovery, infer_file, resolve_file};

/// `ga` is an earlier binding whose type stays open (`'a option`).
const HEADER: &str = "module M\nlet ga = None\n";

/// `(id, parameters, scrutinee, clauses, modelled)`. `modelled` says every
/// pattern, guard and result is in the modelled subset, and the match has
/// more than one clause (FCS elaborates a one-clause match to other nodes, so
/// inference declines it); whether FCS accepts the cell is read from FCS.
const MATCHES: &[(&str, &str, &str, &str, bool)] = &[
    (
        "someNone",
        "(o: int option)",
        "o",
        "| Some v -> v | None -> 0",
        true,
    ),
    (
        "noneSome",
        "(o: int option)",
        "o",
        "| None -> 0 | Some v -> v",
        true,
    ),
    (
        "someWild",
        "(o: int option)",
        "o",
        "| Some _ -> 1 | None -> 0",
        true,
    ),
    (
        "someParen",
        "(o: int option)",
        "o",
        "| Some (v) -> v | None -> 0",
        true,
    ),
    ("wildOnly", "(o: int option)", "o", "| _ -> \"s\"", false),
    ("nameOnly", "(o: int option)", "o", "| x -> x", false),
    (
        "rewrap",
        "(o: int option)",
        "o",
        "| Some v -> Some v | None -> None",
        true,
    ),
    (
        "guardParam",
        "(o: int option) (b: bool)",
        "o",
        "| Some v when b -> v | _ -> 0",
        true,
    ),
    (
        "guardCmp",
        "(o: int option)",
        "o",
        "| Some v when v > 0 -> v | _ -> 0",
        false,
    ),
    (
        "nested",
        "(o: int option option)",
        "o",
        "| Some (Some v) -> v | _ -> 0",
        true,
    ),
    (
        "nestedNone",
        "(o: int option option)",
        "o",
        "| Some None -> 1 | Some (Some _) -> 2 | None -> 3",
        true,
    ),
    (
        "okError",
        "(r: Result<int, string>)",
        "r",
        "| Ok v -> v | Error _ -> 0",
        true,
    ),
    (
        "errorFirst",
        "(r: Result<int, string>)",
        "r",
        "| Error e -> e | Ok _ -> \"ok\"",
        true,
    ),
    (
        "choice",
        "(c: Choice<int, string>)",
        "c",
        "| Choice1Of2 n -> n | Choice2Of2 _ -> 0",
        true,
    ),
    (
        "vopt",
        "(o: int voption)",
        "o",
        "| ValueSome v -> v | ValueNone -> 0",
        true,
    ),
    (
        "ints",
        "(n: int)",
        "n",
        "| 1 -> \"a\" | 2 -> \"b\" | _ -> \"c\"",
        true,
    ),
    ("intName", "(n: int)", "n", "| 0 -> n | k -> k", true),
    ("strs", "(s: string)", "s", "| \"a\" -> 1 | _ -> 2", true),
    (
        "chars",
        "(c: char)",
        "c",
        "| 'a' -> true | _ -> false",
        true,
    ),
    ("bools", "(b: bool)", "b", "| true -> 1 | false -> 0", true),
    (
        "tuple",
        "(t: int * string)",
        "t",
        "| (1, s) -> s | (_, s) -> s",
        true,
    ),
    (
        "tupleConst",
        "(t: int * string)",
        "t",
        "| (k, \"a\") -> k | _ -> 0",
        true,
    ),
    // A later arm is checked against the first arm's open type, a relation
    // inference drops (as for an `if`'s branches), so the result stays open.
    ("openOpt", "o", "o", "| Some v -> v | None -> 0", false),
    ("openInt", "n", "n", "| 1 -> 2 | k -> k", true),
    ("openPair", "p", "p", "| (a, b) -> a", false),
    // An application as scrutinee leaves the payload open: not yet traced.
    (
        "scrutApp",
        "(n: int)",
        "Some n",
        "| Some v -> v | None -> 0",
        false,
    ),
    (
        "scrutNone",
        "(n: int)",
        "None",
        "| Some v -> v + n | None -> n",
        false,
    ),
    (
        "noBar",
        "(o: int option)",
        "o",
        "Some v -> v | None -> 0",
        true,
    ),
    (
        "parenScrut",
        "(n: int)",
        "(Some n)",
        "Some w1 -> w1 | None -> 2",
        true,
    ),
    (
        "noneScrut",
        "(n: int)",
        "None",
        "None -> n | Some w -> w",
        true,
    ),
    // A guard fixes an unannotated parameter to `bool`, and an unmodelled
    // guard (`g v`) constrains what it calls.
    (
        "guardOpenParam",
        "(o: int option) b",
        "o",
        "| Some v when b -> v | _ -> 0",
        true,
    ),
    (
        "guardCall",
        "(o: int option) g",
        "o",
        "| Some v when g v -> v | _ -> 0",
        false,
    ),
    // A pattern of structure over an earlier binding's open variable would
    // retype that binding; a name bound twice is FS0038.
    ("earlierOpen", "()", "ga", "| Some 1 -> 2 | _ -> 3", false),
    (
        "dupBinder",
        "(t: int * string)",
        "t",
        "| (x, x) -> x | _ -> \"\"",
        false,
    ),
    // A function-typed payload is loosened at its use, as FCS loosens a
    // value of a known type; an annotated pattern FCS relates by equality,
    // and rejects on a mismatch.
    (
        "fnPayload",
        "(o: (obj -> int) option) (h: obj -> int)",
        "o",
        "| Some g -> g | None -> h",
        false,
    ),
    (
        "fnPayloadApplied",
        "(o: (obj -> int) option)",
        "o",
        "| Some g -> g 1 | None -> 0",
        true,
    ),
    (
        "annotatedPat",
        "(s: string)",
        "s",
        "| (x: obj) -> x | _ -> null",
        false,
    ),
    (
        "annotatedPatSame",
        "(s: string)",
        "s",
        "| (x: string) -> x | _ -> \"\"",
        false,
    ),
    (
        "resultOpt",
        "(o: int option)",
        "o",
        "| Some v -> Ok v | None -> Error \"none\"",
        false,
    ),
    (
        "matchInArm",
        "(o: int option) (p: int option)",
        "o",
        "| Some v -> (match p with Some w -> w | None -> v) | None -> 0",
        true,
    ),
    (
        "activeLike",
        "(o: int option)",
        "o",
        "| Some v -> v | none -> 0",
        true,
    ),
    // Ill-typed: FCS rejects one clause and keeps the rest.
    (
        "armMismatch",
        "(o: int option)",
        "o",
        "| Some v -> v | None -> \"s\"",
        true,
    ),
    (
        "patMismatch",
        "(o: int option)",
        "o",
        "| Some v -> v | Ok w -> w",
        true,
    ),
    (
        "constMismatch",
        "(n: int)",
        "n",
        "| 1 -> 1 | \"a\" -> 2 | _ -> 3",
        true,
    ),
    (
        "payloadMismatch",
        "(o: int option)",
        "o",
        "| Some \"s\" -> 1 | _ -> 2",
        true,
    ),
    (
        "arityMismatch",
        "(o: int option)",
        "o",
        "| Some -> 1 | _ -> 2",
        false,
    ),
    (
        "openFixedThenWrong",
        "o",
        "o",
        "| Some 1 -> 1 | Some \"s\" -> 2 | _ -> 3",
        true,
    ),
    (
        "guardNotBool",
        "(o: int option)",
        "o",
        "| Some v when v -> v | _ -> 0",
        true,
    ),
    // FCS's decision compiler drops a clause no value can reach, keeping no
    // node or binder of it, so a match with one declines. A guarded clause
    // covers nothing, so what follows it is reachable.
    (
        "wildFirst",
        "(o: int option)",
        "o",
        "| _ -> 1 | Some v -> v",
        false,
    ),
    ("nameFirst", "(n: int)", "n", "| k -> k | 1 -> 2", false),
    (
        "repeatCase",
        "(o: int option)",
        "o",
        "| Some v -> v | Some w -> w | None -> 0",
        false,
    ),
    (
        "repeatConst",
        "(n: int)",
        "n",
        "| 1 -> 1 | 1 -> 2 | _ -> 3",
        false,
    ),
    (
        "hexRepeat",
        "(n: int)",
        "n",
        "| 1 -> 1 | 0x1 -> 2 | _ -> 3",
        false,
    ),
    (
        "repeatStr",
        "(s: string)",
        "s",
        "| \"a\" -> 1 | \"a\" -> 2 | _ -> 3",
        false,
    ),
    (
        "coveredWild",
        "(o: int option)",
        "o",
        "| Some _ -> 1 | None -> 0 | _ -> 2",
        false,
    ),
    (
        "boolsWild",
        "(b: bool)",
        "b",
        "| true -> 1 | false -> 0 | _ -> 2",
        false,
    ),
    ("unitWild", "(u: unit)", "u", "| () -> 1 | _ -> 2", false),
    (
        "tupleCovered",
        "(t: int * string)",
        "t",
        "| (_, s) -> s | (1, s) -> s",
        false,
    ),
    (
        "nestedCovered",
        "(o: int option option)",
        "o",
        "| Some _ -> 1 | Some (Some v) -> v | None -> 0",
        false,
    ),
    // A pattern inference does not read (a list) after a catch-all: what
    // reaches it is still asked, as of a pattern matching everything.
    (
        "wildThenList",
        "(l: int list)",
        "l",
        "| _ -> 1 | [] -> 2",
        false,
    ),
    (
        "guardedRepeat",
        "(o: int option) (b: bool)",
        "o",
        "| Some v when b -> v | Some w -> w | None -> 0",
        true,
    ),
    (
        "guardedWildFirst",
        "(o: int option) (b: bool)",
        "o",
        "| _ when b -> 1 | Some v -> v | None -> 0",
        true,
    ),
    (
        "distinctConsts",
        "(n: int)",
        "n",
        "| 1 -> 1 | 2 -> 2 | 3 -> 3 | _ -> 4",
        true,
    ),
    (
        "nestedDistinct",
        "(o: int option option)",
        "o",
        "| Some None -> 1 | Some _ -> 2 | None -> 3",
        true,
    ),
];

/// `(id, template, modelled)`: where the match `{m}` sits.
const CONTEXTS: &[(&str, &str, bool)] = &[
    ("body", "let f{i} {params} = match {s} with {c}\n", true),
    ("paren", "let f{i} {params} = (match {s} with {c})\n", true),
    (
        "ignore",
        "let f{i} {params} = ignore (match {s} with {c})\n",
        true,
    ),
    // An argument checked against `id`'s open domain: the arms' relation to
    // it is dropped, as for an `if` there, so the result stays open.
    ("id", "let f{i} {params} = id (match {s} with {c})\n", false),
    (
        "tuple",
        "let f{i} {params} = ((match {s} with {c}), 1)\n",
        true,
    ),
];

#[derive(Default, Debug)]
struct Tally {
    cells: usize,
    committed: usize,
    committed_erroring: usize,
    exprs: usize,
    binders: usize,
    missed: Vec<String>,
}

/// One cell: its line, binder name, obligation and label.
type Cell = (u32, String, bool, String);

fn check(source: &str, cells: &[Cell], tally: &mut Tally) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {:?}\n{source}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = full_bcl_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(
        &file,
        &resolved,
        env,
        &borzoi_sema::PrecedingTypes::default(),
    );
    let path = temp_fs_file("infer_match", source);
    let types_json = try_invoke_fcs_dump("types", &path).expect("FCS checks the file");
    let binders_json = try_invoke_fcs_dump("binder-types", &path).expect("FCS checks the file");
    let _ = std::fs::remove_file(&path);
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
            Some(_) => {
                tally.committed += 1;
                tally.committed_erroring += usize::from(erroring);
            }
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

fn program(context: &(&str, &str, bool)) -> (String, Vec<Cell>) {
    let mut src = HEADER.to_string();
    let mut cells = Vec::new();
    for (i, (id, params, scrutinee, clauses, modelled)) in MATCHES.iter().enumerate() {
        let line = src.matches('\n').count() as u32 + 1;
        src.push_str(
            &context
                .1
                .replace("{i}", &i.to_string())
                .replace("{params}", params)
                .replace("{s}", scrutinee)
                .replace("{c}", clauses),
        );
        cells.push((
            line,
            format!("f{i}"),
            *modelled && context.2,
            format!("{}/{id}", context.0),
        ));
    }
    (src, cells)
}

#[test]
fn every_match_in_every_context_agrees_with_fcs() {
    let mut tally = Tally::default();
    for context in CONTEXTS {
        let (src, cells) = program(context);
        check(&src, &cells, &mut tally);
    }
    eprintln!("{tally:?}");
    assert!(
        tally.missed.is_empty(),
        "{} obligated cells declined:\n{}",
        tally.missed.len(),
        tally.missed.join("\n")
    );
    assert!(tally.committed >= 100, "{tally:?}");
    assert!(
        tally.committed_erroring >= 5,
        "the ill-typed rows commit nothing: {tally:?}"
    );
}

/// A pattern of structure over a scrutinee typed through a local FCS
/// generalises (`let g1 = idf in match g1 ho with Some 1 -> …`) must not
/// ground the local: FCS keeps `idf : 'a -> 'a` at its right-hand side.
#[test]
fn a_pattern_does_not_ground_a_generalisable_local_through_the_scrutinee() {
    for body in [
        "(let g1 = idf in (match (if b then (g1 ho) else None) with Some 1 -> 1 | _ -> 2))",
        "(let g1 = idf in (match g1 ho with Some 1 -> 1 | _ -> 2))",
        "(let g1 = idf in (match g1 ho with Some v -> v | None -> 2))",
        "(let g1 = idf in (match (if g1 b then ho else None) with Some 1 -> 1 | _ -> 2))",
    ] {
        let src = format!("module M\nlet idf x = x\nlet ho = Some 5\nlet t (b: bool) = {body}\n");
        check(&src, &[], &mut Tally::default());
    }
}

/// A `let` pattern reads no union case and no constant, so nothing relates
/// such a pattern to an opaque local (`let g = idf in let (1) = g 1 in 0`
/// keeps `idf : 'a -> 'a` in FCS).
#[test]
fn let_patterns_over_cases_and_constants_commit_only_what_fcs_kept() {
    for body in [
        "let g = idf in let (1) = g 1 in 0",
        "let g = idf in let (Some 1) = g (Some 1) in 0",
        "let (Some v) = ho in v",
    ] {
        let src = format!("module M\nlet idf x = x\nlet ho = Some 5\nlet t () = {body}\n");
        check(&src, &[], &mut Tally::default());
    }
}

/// FCS checks in source order: an earlier call fixes the scrutinee, and a
/// pattern contradicting it is the one rejected (`fs : string -> string`,
/// `(fs x, match x with 1 -> x | y -> y)` keeps `x : string`).
///
/// FCS's dumps cannot grade these — they decline to convert a declaration it
/// error-recovered — so FCS's answer is pinned here instead: in every one,
/// `fs x` fixes `x : string` before any pattern is checked, so whatever we
/// publish for `x`, a pattern's names, or `f`'s domain must say `string`.
#[test]
fn a_pattern_does_not_override_an_earlier_argument_check() {
    for body in [
        "(fs x, match x with 1 -> x | y -> y)",
        "(fs x, match x with \"a\" -> x | y -> y)",
        "(fs x, match x with Some v -> 1 | None -> 2)",
        "(fs x, match x with (a, b) -> 1 | _ -> 2)",
        "(fs x, match Some x with Some 1 -> 1 | _ -> 2)",
        "(fs x, match Some x with Some v -> v | None -> \"n\")",
        // Earlier relations conflicting among themselves: FCS keeps the first.
        "((if true then \"\" else x), fi x, (match x with 1 -> x | _ -> x))",
    ] {
        let src =
            format!("module M\nlet fs (s: string) = s\nlet fi (i: int) = i\nlet f x = {body}\n");
        let parsed = parse(&src);
        let recovery = SyntaxRecovery::of(&parsed);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let env = full_bcl_env();
        let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
        let inferred = infer_file(
            &file,
            &resolved,
            env,
            &borzoi_sema::PrecedingTypes::default(),
        );
        for (def, ty) in inferred.def_types() {
            let name = resolved.def(*def).name.to_string();
            let ty = ty.render();
            match name.as_str() {
                "x" | "y" | "v" => assert_eq!(ty, "System.String", "`{name}` in {body}"),
                "f" => assert!(ty.starts_with("System.String ->"), "`f : {ty}` in {body}"),
                _ => {}
            }
        }
    }
}

/// An earlier clause's unmodelled or dropped relation fixes a payload in FCS
/// before a later pattern contradicts it: `if x` makes `x : bool`, an arm `1`
/// checked against the first arm makes it `int`. FCS's dumps cannot grade
/// these error-recovered declarations, so FCS's answer is pinned: a later
/// pattern's `string` must not reach `x`.
#[test]
fn a_later_pattern_does_not_override_an_earlier_clause() {
    for (body, x) in [
        (
            "match o with Some x -> (if x then 1 else 2) | Some \"\" -> 2 | None -> 3",
            "System.Boolean",
        ),
        (
            "match o with Some x -> x | Some y -> 1 | Some \"\" -> 2 | None -> 3",
            "System.Int32",
        ),
    ] {
        let src = format!("module M\nlet f o = {body}\n");
        let parsed = parse(&src);
        let recovery = SyntaxRecovery::of(&parsed);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let env = full_bcl_env();
        let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
        let inferred = infer_file(
            &file,
            &resolved,
            env,
            &borzoi_sema::PrecedingTypes::default(),
        );
        for (def, ty) in inferred.def_types() {
            if resolved.def(*def).name.as_str() == "x" {
                assert_eq!(ty.render(), x, "`x` in {body}");
            }
        }
    }
}

/// A member access before the match on a receiver the pattern makes known:
/// FCS looked the member up before the pattern, on a receiver it did not yet
/// know (FS0072), and keeps `o : 'a`. Pinned, since FCS's dumps cannot grade
/// the error-recovered declaration: neither `o` nor the access may read as
/// `string`'s.
#[test]
fn a_pattern_does_not_wake_an_earlier_member_lookup() {
    for body in [
        "(o.Length, match o with \"a\" -> 1 | _ -> 2)",
        "(o.Length, match o with Some v -> 1 | None -> 2)",
    ] {
        let src = format!("module M\nlet f o = {body}\n");
        let parsed = parse(&src);
        let recovery = SyntaxRecovery::of(&parsed);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let env = full_bcl_env();
        let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
        let inferred = infer_file(
            &file,
            &resolved,
            env,
            &borzoi_sema::PrecedingTypes::default(),
        );
        assert!(
            inferred.member_resolutions().is_empty(),
            "a member resolved in {body}"
        );
        for (def, ty) in inferred.def_types() {
            let name = resolved.def(*def).name.to_string();
            assert!(
                !matches!(name.as_str(), "o" | "f"),
                "`{name} : {}` published in {body}",
                ty.render()
            );
        }
    }
}

/// A pattern that does not fit fails whole in inference, but FCS types the
/// part that fits before rejecting the rest: `(x, 1)` against
/// `int * string` keeps `x : int`, which a later use in the clause must not
/// retype. Pinned, as FCS's dumps cannot grade the error-recovered
/// declaration.
#[test]
fn a_mismatched_pattern_does_not_leave_its_names_to_the_clause() {
    for body in [
        "match t with (x, 1) -> (match x with \"\" -> 0 | _ -> 1) | _ -> 2",
        "match t with (x, 1) -> String.length x | _ -> 2",
    ] {
        let src = format!("module M\nlet f (t: int * string) = {body}\n");
        let parsed = parse(&src);
        let recovery = SyntaxRecovery::of(&parsed);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let env = full_bcl_env();
        let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
        let inferred = infer_file(
            &file,
            &resolved,
            env,
            &borzoi_sema::PrecedingTypes::default(),
        );
        for (def, ty) in inferred.def_types() {
            if resolved.def(*def).name.as_str() == "x" {
                assert_eq!(ty.render(), "System.Int32", "`x` in {body}");
            }
        }
    }
}

/// The same for a union field's pattern, whose equality fails apart from the
/// union's: `UPair (x, 1)` against `UPair of int * string` keeps `x : int`.
#[test]
fn a_mismatched_field_pattern_does_not_leave_its_names_to_the_clause() {
    let src = "module M\nopen FSharpModuleValues\n\
               let f (p: UPair) = match p with UPair (x, 1) -> (match x with \"\" -> 1 | _ -> 2) | _ -> 3\n";
    let parsed = parse(src);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = module_value_fixture_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let inferred = infer_file(
        &file,
        &resolved,
        env,
        &borzoi_sema::PrecedingTypes::default(),
    );
    for (def, ty) in inferred.def_types() {
        if resolved.def(*def).name.as_str() == "x" {
            assert_eq!(ty.render(), "System.Int32", "`x`");
        }
    }
}

/// A condition fixes an unannotated parameter's function-type slot to `bool`
/// before a later pattern relates the parameter itself: FCS has one variable
/// for both, so the pattern is the one it rejects (`if x then (match x with
/// 1 -> …)` keeps `x : bool`, `f : bool -> bool`). FCS's dumps cannot grade
/// the error-recovered declaration, so FCS's answers are pinned (read from its
/// tooltips): whatever we publish for a binder must be FCS's, and every node
/// at `x` must say `bool`.
#[test]
fn a_pattern_does_not_override_a_condition_on_its_parameter() {
    const B: &str = "System.Boolean";
    let rows: &[(&str, &[(&str, &str)])] = &[
        (
            "let f x = if x then (match x with 1 -> x | _ -> x) else false",
            &[("x", B), ("f", "System.Boolean -> System.Boolean")],
        ),
        (
            "let f x = if x then (match x with Some v -> 1 | None -> 2) else 3",
            &[
                ("x", B),
                ("v", "System.Object"),
                ("f", "System.Boolean -> System.Int32"),
            ],
        ),
        (
            "let f x = if x then (match x with \"a\" -> 1 | _ -> 2) else 3",
            &[("x", B), ("f", "System.Boolean -> System.Int32")],
        ),
        (
            "let f (o: int option) x = \
             match o with Some 1 when x -> 1 | _ -> (match x with 1 -> 2 | _ -> 3)",
            &[
                ("x", B),
                ("o", "Microsoft.FSharp.Core.FSharpOption<System.Int32>"),
                (
                    "f",
                    "Microsoft.FSharp.Core.FSharpOption<System.Int32> -> System.Boolean -> System.Int32",
                ),
            ],
        ),
        (
            "let f (x, y) = if x then (match x with 1 -> y | _ -> y) else y",
            &[("x", B), ("y", "'a"), ("f", "System.Boolean * 'a -> 'a")],
        ),
        (
            "let f x = if x then (match Some x with Some 1 -> 1 | _ -> 2) else 3",
            &[("x", B), ("f", "System.Boolean -> System.Int32")],
        ),
    ];
    for (src, fcs) in rows {
        let src = format!("module M\n{src}\n");
        let parsed = parse(&src);
        let recovery = SyntaxRecovery::of(&parsed);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let env = full_bcl_env();
        let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
        let inferred = infer_file(
            &file,
            &resolved,
            env,
            &borzoi_sema::PrecedingTypes::default(),
        );
        for (def, ty) in inferred.def_types() {
            let name = resolved.def(*def).name.to_string();
            let ty = ty.render();
            let expected = fcs
                .iter()
                .find_map(|&(n, t)| (n == name).then_some(t))
                .unwrap_or_else(|| panic!("`{name} : {ty}` has no pinned answer in {src}"));
            assert_eq!(ty, expected, "`{name}` in {src}");
        }
        for (range, ty) in inferred.types() {
            let text = &src[usize::from(range.start())..usize::from(range.end())];
            if text == "x" {
                assert_eq!(ty.render(), B, "`x` node in {src}");
            }
        }
    }
}

/// SplitMix64: a deterministic stream, so a failing seed reproduces exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
}

/// The scrutinee types the clause generator builds patterns at.
#[derive(Debug, Clone, Copy)]
enum PatTy {
    Int,
    Str,
    Bool,
    Unit,
    Opt(&'static PatTy),
    Res,
    Pair,
    /// The file's own union ([`OWN_UNIONS`]'s `PU`).
    Own,
    /// The file's own generic union, at `int`.
    OwnGeneric,
    /// The file's own `[<RequireQualifiedAccess>]` union.
    OwnQualified,
}

/// The unions the generated programs declare, so clause sets cover and
/// repeat the project's own cases as they do FSharp.Core's.
const OWN_UNIONS: &str = "type PU = PA | PB of int | PC of string * bool\n\
                          type PG<'a> = PGa | PGb of 'a | PGc of 'a * 'a\n\
                          [<RequireQualifiedAccess>]\n\
                          type PQ = Q1 | Q2 of int\n";

impl PatTy {
    fn spelling(self) -> String {
        match self {
            PatTy::Int => "int".into(),
            PatTy::Str => "string".into(),
            PatTy::Bool => "bool".into(),
            PatTy::Unit => "unit".into(),
            PatTy::Opt(t) => format!("({}) option", t.spelling()),
            PatTy::Res => "Result<int, string>".into(),
            PatTy::Pair => "(int * string)".into(),
            PatTy::Own => "PU".into(),
            PatTy::OwnGeneric => "PG<int>".into(),
            PatTy::OwnQualified => "PQ".into(),
        }
    }
}

const SCRUTINEE_TYS: &[PatTy] = &[
    PatTy::Int,
    PatTy::Str,
    PatTy::Bool,
    PatTy::Unit,
    PatTy::Opt(&PatTy::Int),
    PatTy::Opt(&PatTy::Opt(&PatTy::Int)),
    PatTy::Opt(&PatTy::Bool),
    PatTy::Res,
    PatTy::Pair,
    PatTy::Own,
    PatTy::OwnGeneric,
    PatTy::OwnQualified,
];

/// A well-typed pattern at `t`, from a small alphabet that repeats itself
/// often (so clauses cover and repeat one another), its names fresh.
fn gen_pat(rng: &mut Rng, t: PatTy, names: &mut usize) -> String {
    let mut fresh = || {
        *names += 1;
        format!("v{names}")
    };
    if rng.chance(15) {
        return if rng.chance(50) { "_".into() } else { fresh() };
    }
    match t {
        PatTy::Int => ["0", "1", "2", "0x1"][rng.below(4)].into(),
        PatTy::Str => ["\"a\"", "\"b\""][rng.below(2)].into(),
        PatTy::Bool => ["true", "false"][rng.below(2)].into(),
        PatTy::Unit => "()".into(),
        PatTy::Opt(inner) => {
            if rng.chance(35) {
                "None".into()
            } else {
                format!("Some ({})", gen_pat(rng, *inner, names))
            }
        }
        PatTy::Res => {
            if rng.chance(50) {
                format!("Ok ({})", gen_pat(rng, PatTy::Int, names))
            } else {
                format!("Error ({})", gen_pat(rng, PatTy::Str, names))
            }
        }
        PatTy::Pair => format!(
            "({}, {})",
            gen_pat(rng, PatTy::Int, names),
            gen_pat(rng, PatTy::Str, names)
        ),
        PatTy::Own => {
            let q = if rng.chance(30) { "PU." } else { "" };
            match rng.below(3) {
                0 => format!("{q}PA"),
                1 => format!("{q}PB ({})", gen_pat(rng, PatTy::Int, names)),
                _ if rng.chance(30) => format!("{q}PC _"),
                _ => format!(
                    "{q}PC ({}, {})",
                    gen_pat(rng, PatTy::Str, names),
                    gen_pat(rng, PatTy::Bool, names)
                ),
            }
        }
        PatTy::OwnGeneric => match rng.below(3) {
            0 => "PGa".into(),
            1 => format!("PGb ({})", gen_pat(rng, PatTy::Int, names)),
            _ => format!(
                "PGc ({}, {})",
                gen_pat(rng, PatTy::Int, names),
                gen_pat(rng, PatTy::Int, names)
            ),
        },
        PatTy::OwnQualified => {
            if rng.chance(40) {
                "PQ.Q1".into()
            } else {
                format!("PQ.Q2 ({})", gen_pat(rng, PatTy::Int, names))
            }
        }
    }
}

/// Generated clause sets — irrefutable clauses before others, repeated and
/// covered cases and constants, guarded clauses — over every scrutinee type,
/// graded against FCS. Its decision compiler keeps no node or binder of a
/// clause no value reaches, so `check` fails on any node we type in one: the
/// reachability judgement may decline a reachable clause, never commit an
/// unreachable one.
#[test]
fn generated_clause_sets_agree_with_fcs() {
    let programs: usize = std::env::var("BORZOI_MATCH_CLAUSE_PROGRAMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let mut tally = Tally::default();
    let mut irrefutable_first = 0;
    for seed in 0..programs as u64 {
        let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) ^ 0x6a09);
        let mut src = format!("module M\n{OWN_UNIONS}");
        let mut cells = Vec::new();
        for i in 0..60 {
            let t = SCRUTINEE_TYS[rng.below(SCRUTINEE_TYS.len())];
            let mut names = 0;
            let clauses: Vec<String> = (0..2 + rng.below(4))
                .map(|k| {
                    let pat = gen_pat(&mut rng, t, &mut names);
                    let guard = if rng.chance(20) { " when b" } else { "" };
                    format!("| {pat}{guard} -> {k}")
                })
                .collect();
            if clauses[..clauses.len() - 1]
                .iter()
                .any(|c| c.starts_with("| _ ->") || c.starts_with("| v"))
            {
                irrefutable_first += 1;
            }
            let line = src.matches('\n').count() as u32 + 1;
            src.push_str(&format!(
                "let f{i} (s: {}) (b: bool) = match s with {}\n",
                t.spelling(),
                clauses.join(" ")
            ));
            cells.push((line, format!("f{i}"), false, format!("seed {seed}/f{i}")));
        }
        check(&src, &cells, &mut tally);
    }
    eprintln!("{tally:?} irrefutable_first={irrefutable_first}");
    assert!(
        irrefutable_first >= programs * 5,
        "too few clause sets put a catch-all before another clause: {irrefutable_first}"
    );
    // A program commits about 15 of its 60 functions (the rest decline:
    // unreachable clauses, unmodelled patterns), so a floor at half that
    // rate holds at any volume while still failing a modelled arm gone dead.
    assert!(
        tally.committed >= programs * 8,
        "the generated matches commit too little to be evidence: {tally:?}"
    );
}

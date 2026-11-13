//! Differential test for type variables written in a module-level function
//! binding's annotations (`let f (x: 'a) (g: 'a -> 'b) : 'b = …`), against both
//! FCS typed-tree oracles: `types` (every expression node) and `binder-types`
//! (every binder).
//!
//! # What FCS does
//!
//! A type variable no `<…>` declares is *implicitly scoped*: the first mention
//! creates it (`TcTypeOrMeasureParameter`, `TyparRigidity.WarnIfNotRigid`), and
//! every later mention of the same name in the same module-level declaration
//! is the same variable — the unscoped typar environment starts empty per
//! module element and is threaded through the declaration. It is an ordinary
//! inference variable: the body may fix it (FS0064, a warning, and the binder
//! then has the fixed type), and the declaration generalises whatever is left.
//!
//! # The grid
//!
//! Every head (the parameters, each with an annotation from a closed list of
//! shapes, some of which inference declines) by every return annotation by
//! every body the head's names allow, one binding per line so that FCS's
//! errors attribute to cells. The bodies include well-typed ones FCS
//! generalises, ones that fix a variable (FS0064), and ill-typed ones whose
//! relations conflict in source order. The same grid runs again under
//! `let f<'a, 'b> …`, where the variables are rigid: a body that fixes or
//! merges them is an error, after which FCS may report no binder of the
//! declaration, and a variable left undeclared is defaulted to `obj`. The
//! declaration forms ([`FORMS`]) add `inline`, an explicit `<'a>`, a sibling
//! binding reusing a name and a `^a`; declarations with a constraint or an
//! attribute, and value bindings, must decline.
//!
//! # What is compared
//!
//! Certain-implies-exact on every committed expression and binder type, with
//! the strict reading on every file (most are ill-typed somewhere): a commit
//! needs an FCS record at the same range, and must equal it. A binder whose
//! type FCS constrains must not be published without the constraint
//! ([`GradedDumps::hides_constraint`]).
//!
//! # Vacuity
//!
//! A per-cell obligation ([`must_commit`]): where the head, the return
//! annotation and the body are in the modelled subset and FCS reports no error
//! on the cell's line, the function's own binder must be committed. A cell
//! outside the subset ([`must_decline`]) must commit nothing generic. Plus
//! floors on generic commits and on commits where FCS fixed a variable
//! (FS0064, read off the `types` dump's warnings per line).

use crate::common::{GradedDumps, full_bcl_env, temp_fs_file, try_invoke_fcs_dump};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, SyntaxRecovery, infer_file, resolve_file};

/// The helpers every program sees, above the cells.
const HEADER: &str = "module M\n\
type Box<'t> = { V: 't }\n\
let monoInt (i: int) = i\n\
let monoStr (s: string) = s\n\
let pick (a: 'z) (b: 'z) = a\n";

/// A head: an id, the parameter text, the names it binds, and whether its
/// annotations are in the modelled subset.
struct Head {
    id: &'static str,
    params: &'static str,
    names: &'static [&'static str],
    modelled: bool,
}

const HEADS: &[Head] = &[
    Head {
        id: "a",
        params: "(x: 'a)",
        names: &["x"],
        modelled: true,
    },
    Head {
        id: "aa",
        params: "(x: 'a) (y: 'a)",
        names: &["x", "y"],
        modelled: true,
    },
    Head {
        id: "ab",
        params: "(x: 'a) (y: 'b)",
        names: &["x", "y"],
        modelled: true,
    },
    Head {
        id: "tup",
        params: "(x: 'a, y: 'b)",
        names: &["x", "y"],
        modelled: true,
    },
    Head {
        id: "tup_aa",
        params: "(x: 'a, y: 'a)",
        names: &["x", "y"],
        modelled: true,
    },
    Head {
        id: "list",
        params: "(x: 'a list) (y: 'a)",
        names: &["x", "y"],
        modelled: true,
    },
    Head {
        id: "opt",
        params: "(x: 'a option) (y: 'a)",
        names: &["x", "y"],
        modelled: true,
    },
    Head {
        id: "fun",
        params: "(g: 'a -> 'b) (x: 'a)",
        names: &["g", "x"],
        modelled: true,
    },
    Head {
        id: "pair",
        params: "(x: 'a * 'b)",
        names: &["x"],
        modelled: true,
    },
    Head {
        id: "arr",
        params: "(x: 'a[]) (y: 'a)",
        names: &["x", "y"],
        modelled: true,
    },
    Head {
        id: "box",
        params: "(x: Box<'a>) (y: 'a)",
        names: &["x", "y"],
        modelled: true,
    },
    Head {
        id: "upper",
        params: "(x: 'T) (y: 'U)",
        names: &["x", "y"],
        modelled: true,
    },
    // The annotation bridge declines this head whatever its argument
    // (`List<int>` too): outside the subset, but graded.
    Head {
        id: "glist",
        params: "(x: System.Collections.Generic.List<'a>) (y: 'a)",
        names: &["x", "y"],
        modelled: false,
    },
    Head {
        id: "mixed",
        params: "(x: 'a) y",
        names: &["x", "y"],
        modelled: true,
    },
    // One variable spelled two ways: FCS reads `'``a``` as `'a`.
    Head {
        id: "esc",
        params: "(x: 'a) (y: '``a``)",
        names: &["x", "y"],
        modelled: true,
    },
    Head {
        id: "esc_b",
        params: "(x: '``a``) (y: 'b)",
        names: &["x", "y"],
        modelled: true,
    },
    Head {
        id: "int_n",
        params: "(x: 'a) (n: int)",
        names: &["x", "n"],
        modelled: true,
    },
    Head {
        id: "unit",
        params: "()",
        names: &[],
        modelled: true,
    },
    // Declined shapes: a measure variable, a constrained head, a constraint
    // written on the variable, a wildcard.
    Head {
        id: "measure",
        params: "(x: float<'u>) (y: float<'u>)",
        names: &["x", "y"],
        modelled: false,
    },
    Head {
        id: "map",
        params: "(x: Map<'a, int>) (y: 'a)",
        names: &["x", "y"],
        modelled: false,
    },
    Head {
        id: "when",
        params: "(x: 'a when 'a : equality) (y: 'a)",
        names: &["x", "y"],
        modelled: false,
    },
    Head {
        id: "anon",
        params: "(x: _) (y: 'a)",
        names: &["x", "y"],
        modelled: false,
    },
    Head {
        id: "flex",
        params: "(x: #seq<'a>) (y: 'a)",
        names: &["x", "y"],
        modelled: false,
    },
    Head {
        id: "underscore",
        params: "(x: '_a) (y: '_a)",
        names: &["x", "y"],
        modelled: false,
    },
    Head {
        id: "underscore_esc",
        params: "(x: '``_a``) (y: '``_a``)",
        names: &["x", "y"],
        modelled: false,
    },
];

/// A return annotation: an id, its text, and whether it is modelled.
const RETS: &[(&str, &str, bool)] = &[
    ("none", "", true),
    ("ra", " : 'a", true),
    ("rb", " : 'b", true),
    ("rlist", " : 'a list", true),
    ("rint", " : int", true),
    ("rc", " : 'c", true),
    // A coercion, not an equality: inference drops the relation.
    ("robj", " : obj", false),
    ("rpair", " : 'a * 'b", true),
    ("resc", " : '``a``", true),
];

/// A body: an id, its text, the names it needs, and whether it is in the
/// subset whose clean cells must commit the function ([`must_commit`]).
///
/// Outside it, beside the ill-typed bodies: an `if`, whose `else` is checked
/// against the `then` branch, a relation inference drops; and (by
/// [`TUPLE_BODIES`]) a tuple under a return annotation, whose elements are
/// checked against the annotation's, which inference drops too.
const BODIES: &[(&str, &str, &[&str], bool)] = &[
    ("x", "x", &["x"], true),
    ("y", "y", &["y"], true),
    ("xy", "(x, y)", &["x", "y"], true),
    ("yx", "(y, x)", &["x", "y"], true),
    ("if", "if true then x else y", &["x", "y"], false),
    ("monoInt", "monoInt x", &["x"], true),
    ("monoStrY", "monoStr y", &["y"], true),
    ("monoBoth", "(monoInt x, monoStr y)", &["x", "y"], true),
    ("monoConflict", "(monoInt x, monoStr x)", &["x"], false),
    ("monoConflictRev", "(monoStr x, monoInt x)", &["x"], false),
    ("monoNested", "monoStr (monoInt x)", &["x"], false),
    ("id", "id x", &["x"], true),
    ("gx", "g x", &["g", "x"], true),
    ("gMono", "g (monoInt x)", &["g", "x"], false),
    ("gThenMono", "(g x, monoStr x)", &["g", "x"], false),
    ("box", "box x", &["x"], false),
    ("ignore", "ignore x", &["x"], false),
    ("length", "x.Length", &["x"], false),
    ("eq", "x = x", &["x"], false),
    ("compare", "compare x y", &["x", "y"], false),
    ("fst", "fst x", &["x"], false),
    ("field", "x.V", &["x"], false),
    ("fieldMono", "monoInt x.V", &["x"], false),
    ("cond", "if x then 1 else 2", &["x"], false),
    (
        "monoThenCond",
        "(monoInt x, if x then 1 else 2)",
        &["x"],
        false,
    ),
    (
        "condThenMono",
        "(if x then 1 else 2, monoInt x)",
        &["x"],
        false,
    ),
    // A condition on one parameter, a fixing use of another sharing its
    // variable, in both orders.
    (
        "monoYThenCondX",
        "(monoInt y, if x then 1 else 2)",
        &["x", "y"],
        false,
    ),
    (
        "condXThenMonoY",
        "(if x then 1 else 2, monoInt y)",
        &["x", "y"],
        false,
    ),
    ("condX", "if x then y else y", &["x", "y"], false),
    ("local", "let z = x in z", &["x"], false),
    ("localAnnSame", "let (z: 'a) = x in z", &["x"], false),
    ("localAnnFix", "let (z: 'a) = 1 in monoStr x", &["x"], false),
    ("localAnnNew", "let (z: 'q) = monoInt x in x", &["x"], false),
    ("lambdaAnn", "fun (z: 'a) -> x", &["x"], false),
    ("defaultof", "Unchecked.defaultof<'a>", &[], false),
    ("n", "n", &["n"], true),
    ("xn", "(x, n)", &["x", "n"], true),
    ("list", "[x]", &["x"], false),
    ("some", "Some x", &["x"], false),
    ("stmt", "ignore x; y", &["x", "y"], false),
    ("lit", "1", &[], true),
    ("monoN", "monoInt n", &["n"], true),
    // Two written variables FCS unifies with each other (FS0064).
    ("pick", "pick x y", &["x", "y"], true),
    // The body fixes the head's `'a` through a mention of its own: a `let`
    // annotation, a lambda parameter's, a typed expression. FCS types the
    // head's parameter (and the function) concretely, with FS0064.
    ("localAnnGround", "let (z: 'a) = 1 in x", &["x"], false),
    (
        "lambdaAnnGround",
        "((fun (z: 'a) -> z) 1, x)",
        &["x"],
        false,
    ),
    ("typedGround", "((1 : 'a), x)", &["x"], false),
    // A dropped relation (the `else` branch) fixing `'a` beside a parameter
    // with a ground annotation: under `<'a>` FCS rejects the declaration
    // whole, the annotated parameter included.
    ("ifLit", "if true then x else 1", &["x"], false),
    // A statically resolved operator, which the walk does not model, fixing
    // `'a`: under a declaration FCS rejects it whole.
    ("plus", "x + 1", &["x"], false),
    // A body mention of the head's `'a`, spelled escaped.
    ("localAnnEsc", "let (z: '``a``) = x in z", &["x"], false),
];

/// The bodies mentioning a variable by a name the heads use: the same
/// variable as the head's, so the head's annotations decline whole.
const SAME_NAME_BODIES: &[&str] = &[
    "localAnnSame",
    "localAnnFix",
    "lambdaAnn",
    "defaultof",
    "localAnnGround",
    "lambdaAnnGround",
    "typedGround",
    "localAnnEsc",
];

/// The declaration forms beside the plain `let fN … = body` the main grid
/// uses, each with a small head × body grid of its own.
const FORMS: &[(&str, &str)] = &[
    // `inline`: a plain variable is still a plain variable.
    ("inline", "let inline f{i} {params}{ret} = {body}\n"),
    // An explicit `<'a>` makes the variables rigid (FS0064 becomes an error).
    ("explicit", "let f{i}<'a> {params}{ret} = {body}\n"),
    ("explicit_ab", "let f{i}<'a, 'b> {params}{ret} = {body}\n"),
    // A declaration spelling its variable escaped.
    ("explicit_esc", "let f{i}<'``a``> {params}{ret} = {body}\n"),
    // Two declarations: each `'a` is its own.
    (
        "sibling",
        "let h{i} (w: 'a) = monoInt w\nlet f{i} {params}{ret} = {body}\n",
    ),
    // A `^a` beside the plain one.
    ("hat", "let inline f{i} (q: ^a) {params}{ret} = {body}\n"),
];

/// One cell's outcome.
#[derive(Debug, Default, Clone)]
struct Tally {
    cells: usize,
    /// Cells whose function binder we committed.
    committed: usize,
    /// Committed cells FCS reports an error on.
    committed_erroring: usize,
    /// Committed cells where FCS fixed a written variable (the binder is
    /// ground though the head names a variable): FS0064.
    committed_fixed: usize,
    /// Committed cells whose binder is generic.
    committed_generic: usize,
    exprs: usize,
    binders: usize,
    /// `(cell, FCS's binder)` for every obligation we missed.
    missed: Vec<String>,
    /// Cells FCS warns FS0064 on, and of them those we committed.
    fs0064: usize,
    committed_fs0064: usize,
    /// A generic commit on a cell whose head or body is to decline.
    undeclined: Vec<String>,
    /// Commits (ground, so agreeing with FCS) on a cell whose body mentions a
    /// head's name.
    same_name_committed: usize,
    /// Cells FCS cannot check even alone.
    uncheckable: Vec<String>,
}

/// The one-cell program holding `cell` of `source`: the header and the cell's
/// own declaration lines, renumbered.
fn alone(
    source: &str,
    cell: &(u32, String, bool, String),
) -> (String, Vec<(u32, String, bool, String)>) {
    let lines: Vec<&str> = source.lines().collect();
    let header_lines = HEADER.lines().count();
    let mut src = HEADER.to_string();
    // A declaration is one line, or two for the sibling form, whose function
    // is the second.
    let fline = cell.0 as usize;
    let start = if fline >= 2 && lines[fline - 2].starts_with(&format!("let h{}", &cell.1[1..])) {
        fline - 1
    } else {
        fline
    };
    for l in &lines[start - 1..fline] {
        src.push_str(l);
        src.push('\n');
    }
    let new_line = (header_lines + 1 + fline - start) as u32;
    (
        src,
        vec![(new_line, cell.1.clone(), cell.2, cell.3.clone())],
    )
}

/// The bodies built as a tuple, which are in the obligation subset only
/// without a return annotation ([`BODIES`]).
const TUPLE_BODIES: &[&str] = &["xy", "yx", "monoBoth", "xn"];

/// The cells whose function binder must be committed: modelled head, return
/// annotation and body, and FCS clean on the cell's line.
fn must_commit(
    head: &Head,
    ret: &(&str, &str, bool),
    body: &str,
    body_modelled: bool,
    clean: bool,
) -> bool {
    head.modelled
        && ret.2
        && body_modelled
        && !(TUPLE_BODIES.contains(&body) && !ret.1.is_empty())
        && clean
}

/// Check one program: every commit agrees with FCS, and each cell `(line,
/// name, obligated)` is tallied.
fn check(source: &str, cells: &[(u32, String, bool, String)], tally: &mut Tally) {
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
    let path = temp_fs_file("infer_annotation_typar", source);
    let types_json = try_invoke_fcs_dump("types", &path);
    let binders_json = try_invoke_fcs_dump("binder-types", &path);
    let _ = std::fs::remove_file(&path);
    let (Ok(types_json), Ok(binders_json)) = (types_json, binders_json) else {
        // FCS aborts the whole file on some ill-typed cells (an infinite type
        // through a lambda), so check each cell alone; a cell FCS cannot
        // check alone grades nothing, and is counted.
        if cells.len() == 1 {
            tally.uncheckable.push(cells[0].3.clone());
            return;
        }
        for cell in cells {
            let (src, one) = alone(source, cell);
            check(&src, &one, tally);
        }
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
        let ours = ty.render(&env);
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
        let ours = ty.render(&env);
        let theirs = dumps.binders.get(&key).unwrap_or_else(|| {
            panic!(
                "we inferred `{ours}` for binder `{}` at {key:?} but FCS reports no binder \
                 there\n{source}",
                def.spelling(env)
            )
        });
        assert_eq!(
            &ours,
            theirs,
            "binder type mismatch for `{}` at {key:?} (line {})\n{source}",
            def.spelling(env),
            line_of(key.0)
        );
        assert!(
            !dumps.hides_constraint(key, &ours),
            "binder `{}` typed `{ours}`, which FCS constrains\n{source}",
            def.spelling(env)
        );
        tally.binders += 1;
        ours_by_name.insert((def.spelling(env).into_owned(), line_of(key.0)), ours);
    }
    let warned: Vec<(u32, u32)> = serde_json::from_str::<serde_json::Value>(&types_json)
        .expect("types JSON")["Warnings"]
        .as_array()
        .expect("the `types` payload carries `Warnings`")
        .iter()
        .map(|w| {
            (
                w["Line"].as_u64().unwrap() as u32,
                w["Code"].as_u64().unwrap() as u32,
            )
        })
        .collect();
    for (line, name, obligated, label) in cells {
        tally.cells += 1;
        let erroring = dumps.errors.iter().any(|e| e.line == *line);
        let less_generic = warned.contains(&(*line, 64));
        tally.fs0064 += usize::from(less_generic);
        let fcs = fcs_binder(&dumps, source, *line, name);
        match ours_by_name.get(&(name.clone(), *line)) {
            Some(ours) => {
                tally.committed += 1;
                tally.committed_erroring += usize::from(erroring);
                tally.committed_fs0064 += usize::from(less_generic);
                if ours.contains('\'') && must_decline(label) {
                    tally.undeclined.push(format!("{label}: `{ours}`"));
                }
                if SAME_NAME_BODIES
                    .iter()
                    .any(|b| label.ends_with(&format!("/{b}")))
                {
                    tally.same_name_committed += 1;
                }
                if ours.contains('\'') {
                    tally.committed_generic += 1;
                } else if cell_names_variable(label) {
                    tally.committed_fixed += 1;
                }
            }
            None if *obligated && !erroring => {
                tally
                    .missed
                    .push(format!("{label}: FCS `{}`", fcs.unwrap_or_default()));
            }
            None => {}
        }
    }
}

/// Whether a cell (`head/ret/body`) must not commit a generic function: its
/// head is outside the modelled subset, or its body mentions a head's name.
/// A ground commit there comes from rules that read no declined annotation.
fn must_decline(label: &str) -> bool {
    let mut parts = label.split('/');
    let head = parts.next().unwrap_or("");
    let body = parts.nth(1).unwrap_or("");
    HEADS.iter().any(|h| h.id == head && !h.modelled) || SAME_NAME_BODIES.contains(&body)
}

/// Whether a cell's label (`head/ret/body`) names a head that writes a variable.
fn cell_names_variable(label: &str) -> bool {
    let head = label.split('/').next().unwrap_or("");
    HEADS
        .iter()
        .find(|h| h.id == head)
        .is_some_and(|h| h.params.contains('\''))
}

/// FCS's binder type for `name` declared on `line`.
fn fcs_binder(dumps: &GradedDumps, source: &str, line: u32, name: &str) -> Option<String> {
    dumps.binders.iter().find_map(|(&(s, e), ty)| {
        (source[..s].matches('\n').count() as u32 + 1 == line && &source[s..e] == name)
            .then(|| ty.clone())
    })
}

/// The program for one `(head, ret)` pair under `form`, and its cells.
fn program(
    head: &Head,
    ret: &(&str, &str, bool),
    form: Option<&str>,
    obligate: bool,
    bodies: &[(&str, &str, &[&str], bool)],
) -> (String, Vec<(u32, String, bool, String)>) {
    let mut src = HEADER.to_string();
    let mut cells = Vec::new();
    for (i, (bid, body, needs, body_modelled)) in bodies.iter().enumerate() {
        if !needs.iter().all(|n| head.names.contains(n)) {
            continue;
        }
        // FCS aborts the whole file (no implementation file) on an annotated
        // lambda under any return annotation.
        if *bid == "lambdaAnn" && !ret.1.is_empty() {
            continue;
        }
        let line = src.matches('\n').count() as u32 + 1;
        let template = form.unwrap_or("let f{i} {params}{ret} = {body}\n");
        let decl = template
            .replace("{i}", &i.to_string())
            .replace("{params}", head.params)
            .replace("{ret}", ret.1)
            .replace("{body}", body);
        // The function sits on the template's own line: the second for the
        // sibling form, the first otherwise.
        let fline = if template.starts_with("let h{i}") {
            line + 1
        } else {
            line
        };
        let obligated = obligate && must_commit(head, ret, bid, *body_modelled, true);
        cells.push((
            fline,
            format!("f{i}"),
            obligated,
            format!("{}/{}/{bid}", head.id, ret.0),
        ));
        src.push_str(&decl);
    }
    (src, cells)
}

/// The main grid: every head by every return annotation by every body.
#[test]
fn every_head_return_and_body_agrees_with_fcs() {
    let mut tally = Tally::default();
    for head in HEADS {
        for ret in RETS {
            let (src, cells) = program(head, ret, None, true, BODIES);
            check(&src, &cells, &mut tally);
        }
    }
    eprintln!("typar grid: {tally:?}");
    assert!(
        tally.missed.is_empty(),
        "{} obligated cell(s) not committed:\n{}",
        tally.missed.len(),
        tally.missed.join("\n")
    );
    assert!(
        tally.undeclined.is_empty(),
        "generic commits where the annotations must decline:\n{}",
        tally.undeclined.join("\n")
    );
    assert!(tally.committed_generic >= 1, "no generic commit: {tally:?}");
    assert!(tally.committed_fixed >= 1, "no FS0064 commit: {tally:?}");
    assert!(
        tally.committed_fs0064 >= 1,
        "no commit FCS warns FS0064 on: {tally:?}"
    );
}

/// The main grid with the variables declared (`let f<'a, 'b> …`), which
/// makes them rigid: FCS rejects a body that fixes one or merges the two, so
/// those cells must commit nothing generic, while a body that leaves them
/// apart generalises exactly as with implicit variables.
#[test]
fn every_head_return_and_body_agrees_with_fcs_under_declared_variables() {
    let mut tally = Tally::default();
    for head in HEADS {
        for ret in RETS {
            // A variable the declaration leaves out — `'T`, `'U`, `'c`, or an
            // unannotated parameter's — FCS defaults to `obj`, which is not
            // modelled: such a function stays silent.
            let defaults_to_obj = matches!(head.id, "upper" | "mixed") || ret.0 == "rc";
            let (src, cells) = program(
                head,
                ret,
                Some("let f{i}<'a, 'b> {params}{ret} = {body}\n"),
                !defaults_to_obj,
                BODIES,
            );
            check(&src, &cells, &mut tally);
        }
    }
    eprintln!("declared typar grid: {tally:?}");
    assert!(
        tally.missed.is_empty(),
        "{} obligated cell(s) not committed:\n{}",
        tally.missed.len(),
        tally.missed.join("\n")
    );
    assert!(
        tally.undeclined.is_empty(),
        "generic commits where the annotations must decline:\n{}",
        tally.undeclined.join("\n")
    );
    assert!(tally.committed_generic >= 1, "no generic commit: {tally:?}");
}

/// Declarations whose variables are not read: a constraint on a declared
/// variable, an attribute on one, one declared twice. Nothing generic is
/// committed, and with the `x + 1` body (an unmodelled relation fixing a
/// declared variable, which FCS rejects whole) nothing at all is: the ground
/// `n : int` beside it is withheld with the rest.
#[test]
fn constrained_and_attributed_declarations_decline() {
    let forms = [
        "let f{i}<'a when 'a : equality> {params}{ret} = {body}\n",
        "let f{i}<'a, 'b when 'b : comparison> {params}{ret} = {body}\n",
        "let f{i}<[<Measure>] 'u, 'a, 'b> {params}{ret} = {body}\n",
        "let f{i}<'a, 'a> {params}{ret} = {body}\n",
    ];
    let heads: Vec<&Head> = HEADS
        .iter()
        .filter(|h| matches!(h.id, "a" | "aa" | "ab" | "fun" | "int_n"))
        .collect();
    let mut tally = Tally::default();
    for form in forms {
        let before = tally.committed_generic;
        for head in &heads {
            let (src, cells) = program(head, &RETS[0], Some(form), false, BODIES);
            check(&src, &cells, &mut tally);
        }
        assert_eq!(tally.committed_generic, before, "{form}: {tally:?}");
    }
    eprintln!("declined declarations: {tally:?}");
}

/// The declaration forms, over the heads with plain variables and the bodies
/// of the obligation subset plus the order-sensitive ones.
#[test]
fn every_declaration_form_agrees_with_fcs() {
    let heads: Vec<&Head> = HEADS
        .iter()
        .filter(|h| matches!(h.id, "a" | "aa" | "ab" | "fun" | "mixed" | "int_n"))
        .collect();
    let bodies: Vec<(&str, &str, &[&str], bool)> = BODIES
        .iter()
        .copied()
        .filter(|(id, ..)| {
            matches!(
                *id,
                "x" | "xy" | "if" | "monoInt" | "monoConflict" | "gx" | "gMono" | "n" | "lit"
            )
        })
        .collect();
    let mut tally = Tally::default();
    for (id, form) in FORMS {
        let mut this = Tally::default();
        for head in &heads {
            for ret in RETS
                .iter()
                .filter(|r| matches!(r.0, "none" | "ra" | "rint"))
            {
                let (src, cells) = program(head, ret, Some(form), false, &bodies);
                check(&src, &cells, &mut this);
            }
        }
        eprintln!("typar form {id}: {this:?}");
        // A head mentioning a `^a` reads none of its variables.
        if *id == "hat" {
            assert_eq!(this.committed_generic, 0, "{id}: {this:?}");
        } else {
            assert!(
                this.committed_generic > 0,
                "{id} commits nothing generic: {this:?}"
            );
        }
        tally.cells += this.cells;
        tally.committed += this.committed;
        tally.committed_generic += this.committed_generic;
        tally.undeclined.extend(this.undeclined);
    }
    eprintln!("typar forms: {tally:?}");
    assert!(
        tally.undeclined.is_empty(),
        "generic commits where the annotations must decline:\n{}",
        tally.undeclined.join("\n")
    );
}

/// The oracle's value on the cells a name-keyed renaming got wrong, pinned:
/// FCS names a generalised inference variable `'a`, `'b`, … whatever the
/// annotations wrote, and the canonical rendering must still tell distinct
/// variables apart.
#[test]
fn the_oracle_renders_distinct_variables_with_one_name_apart() {
    let src = format!(
        "{HEADER}let f (x: 'a) y = 1\nlet g (x: 'a) (y: 'b) = fst x\nlet h (x: 'T) (y: 'a) = (y, x)\n"
    );
    let path = temp_fs_file("infer_annotation_typar_oracle", &src);
    let binders_json = try_invoke_fcs_dump("binder-types", &path).expect("FCS checks it");
    let types_json = try_invoke_fcs_dump("types", &path).expect("FCS checks it");
    let _ = std::fs::remove_file(&path);
    let dumps = GradedDumps::read(&types_json, &binders_json, &src, None).expect("placeable");
    let line = HEADER.lines().count() as u32;
    let binder = |l: u32, n: &str| fcs_binder(&dumps, &src, l, n).unwrap_or_default();
    assert_eq!(binder(line + 1, "f"), "'a -> 'b -> System.Int32");
    assert_eq!(binder(line + 2, "g"), "'a * 'b -> 'c -> 'a");
    assert_eq!(binder(line + 3, "h"), "'a -> 'b -> 'b * 'a");
}

/// Value bindings annotated with a type variable decline: the value
/// restriction decides what such a binding generalises, which nothing here
/// models. Whatever is committed must agree, and nothing generic is.
#[test]
fn value_bindings_with_a_type_variable_decline() {
    let src = format!(
        "{HEADER}let v1 : 'a list = []\nlet v2 : 'a -> 'a = id\nlet (v3: 'a) = Unchecked.defaultof<_>\n\
         let v4 : 'a option = None\nlet v5 : 'a = 1\nlet v6 : 'a * int = (1, 2)\nlet (v7: 'a, v8: 'b) = (1, \"s\")\n"
    );
    let mut tally = Tally::default();
    check(&src, &[], &mut tally);
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
        let name = resolved.def(*def).spelling(env);
        assert!(
            !(name.starts_with('v') && ty.render(&env).contains('\'')),
            "`{name}` committed generic `{}`",
            ty.render(&env)
        );
    }
}

/// A function-typed parameter's domains, by how FCS treats a use of it:
/// `(id, type, sealed)`. FCS loosens a use of a function-typed value
/// (`MakeApplicableExprWithFlex`): each parameter of a type that is not sealed
/// (`obj`, an interface, an unsealed class) becomes a fresh subtype of it, so
/// `let f (g: obj -> 'a) = g` is `(obj -> 'a) -> 'b -> 'a`. A project type is
/// read as unsealed here whatever it is, which only declines.
const DOMAINS: &[(&str, &str, bool)] = &[
    ("obj", "obj", false),
    ("class", "C", false),
    ("iface", "System.IComparable", false),
    ("objTuple", "obj * int", false),
    ("record", "Box<int>", false),
    ("string", "string", true),
    ("typar", "'c", true),
    ("fun", "(int -> int)", true),
];

/// Every function-typed parameter shape over every domain, used returned,
/// applied, partially applied, piped, passed and wrapped: certain-implies-exact
/// on each, and a sealed domain's clean cells must commit.
#[test]
fn function_typed_parameters_loosen_as_fcs_does() {
    let heads = [
        ("one", "(g: {D} -> 'a) (x: {D})"),
        ("two", "(g: {D} -> 'a -> 'b) (x: {D})"),
    ];
    let bodies: &[(&str, &str)] = &[
        ("ret", "g"),
        ("app", "g x"),
        ("pipe", "x |> g"),
        ("passed", "pick g g"),
        ("wrapped", "id g"),
        ("pair", "(g, x)"),
    ];
    let mut tally = Tally::default();
    for &(did, dty, sealed) in DOMAINS {
        for (hid, head) in heads {
            let params = head.replace("{D}", dty);
            for ret in ["", " : {D} -> 'a"] {
                // The returned function's own annotation only fits `one`.
                if !ret.is_empty() && hid != "one" {
                    continue;
                }
                let ret = ret.replace("{D}", dty);
                let mut src = format!("{HEADER}type C() = class end\n");
                let mut cells = Vec::new();
                for (i, (bid, body)) in bodies.iter().enumerate() {
                    if !ret.is_empty() && *bid != "ret" && *bid != "wrapped" {
                        continue;
                    }
                    let line = src.matches('\n').count() as u32 + 1;
                    src.push_str(&format!("let f{i} {params}{ret} = {body}\n"));
                    cells.push((
                        line,
                        format!("f{i}"),
                        sealed,
                        format!(
                            "{did}/{hid}{}/{bid}",
                            if ret.is_empty() { "" } else { "+ret" }
                        ),
                    ));
                }
                check(&src, &cells, &mut tally);
            }
        }
    }
    // A hole in the annotation is not read at all.
    let src = format!("{HEADER}let f0 (g: obj -> _) = g\nlet f1 (g: obj -> _) (x: obj) = g x\n");
    let line = HEADER.lines().count() as u32;
    check(
        &src,
        &[
            (line + 1, "f0".into(), false, "hole/one/ret".into()),
            (line + 2, "f1".into(), false, "hole/one/app".into()),
        ],
        &mut tally,
    );
    eprintln!("function-typed parameters: {tally:?}");
    assert!(
        tally.missed.is_empty(),
        "{} obligated cell(s) not committed:\n{}",
        tally.missed.len(),
        tally.missed.join("\n")
    );
    assert!(tally.committed_generic >= 1, "no generic commit: {tally:?}");
}

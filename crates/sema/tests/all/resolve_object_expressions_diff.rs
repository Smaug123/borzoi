//! Names inside **object-expression members** against FCS.
//!
//! `{ new T(args) with member … interface I with member … }` closes over the
//! scope it is written in: each member sees the enclosing function's locals
//! and parameters, under its own self-identifier and parameters, which shadow
//! them (a parameter shadows the self-identifier too). The scope generator
//! (`common::scope_gen`, swept by `resolve_scope_gen_diff`) plants uses inside
//! generated object expressions; the cases here are the shapes it does not
//! build — generic members, property accessors, nested object expressions,
//! one inside a class member, a member parameter spelled like its own
//! self-identifier — and the forms FCS rejects or that the resolver declines,
//! which must still not commit a wrong answer.
//!
//! Each case is graded on two properties:
//!
//! * **certain implies exact**: every in-file binder we commit anywhere in the
//!   file is the one FCS names at that range, and we commit nothing in-file
//!   where FCS names something outside it;
//! * **must commit**: each use marked `«…»` is committed to FCS's binder, in a
//!   case FCS accepts — so the first property cannot hold by declining.

use std::path::PathBuf;

use crate::common::{
    CensusDecl, FileCensus, census_resolve_uses, invoke_fcs_dump_census, parse_census_jsonl,
    temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, ProjectItems, Resolution, ResolvedFile, SyntaxRecovery, resolve_file,
};
use rowan::TextRange;

const PRELUDE: &str = "\
type I =
    abstract M : int -> int
type J =
    abstract N<'U> : 'U -> 'U
    abstract P : int with get, set
";

/// `(case, FCS accepts it)`.
const CASES: &[(&str, bool)] = &[
    // WoofWare.Expect `Dot.fs`: curried members, locals inside the body.
    (
        "\
type IProcess =
    abstract Create : string -> string -> int
    abstract Start : int -> bool
let process' =
    { new IProcess with
        member _.Create exe args =
            let psi = exe + args
            «psi».Length + «exe».Length
        member _.Start p = «p» > 0 }
",
        true,
    ),
    // The self-identifier shadows a parameter of the enclosing function.
    (
        "\
let f (x: int) =
    { new I with
        member x.M y = «x».M «y» }
",
        true,
    ),
    // Rejected by FCS: a member parameter spelled like its own
    // self-identifier. FCS still binds the use, to the parameter.
    (
        "\
let f (x: int) =
    { new I with
        member x.M x = x }
",
        false,
    ),
    // A discarded self-identifier: the enclosing names stay visible.
    (
        "\
let f (x: int) =
    let z = x
    { new I with
        member _.M y = «x» + «y» + «z» }
",
        true,
    ),
    // Generic members, an enclosing generic function's type parameter, and a
    // property's accessors sharing one self-identifier.
    (
        "\
let f<«'T»> (t: «'T») (seed: int) =
    { new J with
        member this.N<«'U»> (u: «'U») = ignore «t»; «u»
        member this.P
            with get () = «seed» + «this».P
            and set v = ignore («v» + «seed») }
",
        true,
    ),
    // An `interface … with` section beside the `with` block, each member
    // seeing the enclosing scope.
    (
        "\
let f (x: int) =
    { new System.Object() with
        member _.ToString() = string «x»
      interface I with
        member s.M y = «s».GetHashCode() + «y» + «x» }
",
        true,
    ),
    // Rejected by FCS's parser: an `interface` section laid out under a base
    // call with no `with` block.
    (
        "\
let f (x: int) =
    { new System.Object()
      interface I with
        member _.M y = y + x }
",
        false,
    ),
    // A nested object expression sees the outer member's self-identifier and
    // parameter, and its own shadow them.
    (
        "\
let f (x: int) =
    { new I with
        member outer.M y =
            let inner =
                { new I with
                    member _.M z = «outer».M «z» + «y» + «x» }
            let y = 1
            inner.M «y» }
",
        true,
    ),
    // Inside a class member: the class's fields, the type member's
    // self-identifier and parameter.
    (
        "\
type C(p: int) =
    let fld = p
    member this.Make (q: int) : I =
        { new I with
            member _.M r = «fld» + «q» + «r» + «this».Make(0).M 0 }
",
        true,
    ),
    // At module level: module values, and a lambda closing over a member
    // parameter.
    (
        "\
let k = 3
let o =
    { new I with
        member _.M y = (fun z -> «z» + «y») «k» }
",
        true,
    ),
    // `override` on a class base, calling `base`.
    (
        "\
let f (x: int) =
    { new System.Object() with
        override this.ToString() = base.ToString() + string «x» + string («this».GetHashCode()) }
",
        true,
    ),
    // A bare name spelled like the member being defined is never the member.
    (
        "\
type K =
    abstract go : int -> int
let f (go: int) =
    { new K with
        member _.go y = «go» + «y» }
",
        true,
    ),
    // The constructor argument resolves in the enclosing scope.
    (
        "\
[<AbstractClass>]
type B(n: int) =
    abstract M : int -> int
let f (x: int) =
    { new B(«x») with
        member _.M y = «y» + «x» }
",
        true,
    ),
    // Rejected by FCS: a static member in an object expression.
    (
        "\
let f (x: int) =
    { new System.Object() with
        static member S = x
        override _.ToString() = string x }
",
        false,
    ),
    // A base alias, which the resolver declines.
    (
        "\
let f (x: int) =
    { new System.Object() as b with
        override _.ToString() = b.ToString() + string x }
",
        false,
    ),
    // Recovered input: a member whose body is missing.
    (
        "\
let f (x: int) =
    let y = x
    { new I with
        member _.M x =
      interface J with
        member _.N u = u
        member _.P with get () = y and set v = () }
",
        false,
    ),
];

struct Program {
    src: String,
    must_commit: Vec<TextRange>,
}

fn render(module: usize, marked: &str) -> Program {
    let mut src = format!("module M{module}\n{PRELUDE}");
    let mut must_commit = Vec::new();
    let mut open: Option<usize> = None;
    for c in marked.chars() {
        match c {
            '«' => {
                assert!(open.is_none(), "nested markers in {marked:?}");
                open = Some(src.len());
            }
            '»' => {
                let start = open.take().expect("balanced markers");
                must_commit.push(range_of(start, src.len()));
            }
            c => src.push(c),
        }
    }
    assert!(open.is_none(), "unclosed marker in {marked:?}");
    Program { src, must_commit }
}

fn range_of(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(
        &file,
        &ProjectItems::default(),
        &AssemblyEnv::default(),
        &recovery,
    )
}

/// Every commitment of `rf` that FCS does not make: an in-file binder FCS
/// does not name, or an assembly target where FCS names an in-file one. FCS's silence at
/// a range is evidence only when it accepted the program (`clean`): a check
/// that errored sinks no uses for whatever it abandoned, which implicates
/// nobody.
fn wrong_commitments(src: &str, rf: &ResolvedFile, file: &FileCensus, clean: bool) -> Vec<String> {
    let uses = census_resolve_uses(file, src);
    let mut wrong = Vec::new();
    for (range, res) in rf.resolutions() {
        let range = *range;
        let ours = match res {
            Resolution::Local(_) | Resolution::Item(_) => rf.resolved_def(*res).map(|d| d.range),
            Resolution::Unresolved => {
                wrong.push(format!("{:?} at {range:?}: Unresolved", &src[range]));
                continue;
            }
            // An assembly target: wrong wherever FCS names an in-file binder
            // there. FCS spans a dotted access from its head, so match on the
            // shared end.
            Resolution::Entity(_) | Resolution::Member { .. } => {
                let in_file: Vec<_> = uses
                    .iter()
                    .filter(|u| {
                        u.end == usize::from(range.end())
                            && u.start <= usize::from(range.start())
                            && matches!(u.decl, CensusDecl::InFile(..))
                    })
                    .collect();
                if !in_file.is_empty() {
                    wrong.push(format!(
                        "{:?} at {range:?}: we gave {res:?}, FCS {:?}",
                        &src[range],
                        in_file.iter().map(|u| u.decl).collect::<Vec<_>>()
                    ));
                }
                continue;
            }
            Resolution::Deferred(_) => continue,
        };
        let at: Vec<_> = uses
            .iter()
            .filter(|u| range_of(u.start, u.end) == range)
            .collect();
        // Our own defining occurrence, which FCS need not report.
        if at.is_empty() && (ours == Some(range) || !clean) {
            continue;
        }
        let agrees = !at.is_empty()
            && at.iter().all(|u| match u.decl {
                CensusDecl::InFile(s, e) => ours == Some(range_of(s, e)),
                CensusDecl::OtherFile => false,
                CensusDecl::Absent => true,
            });
        if !agrees {
            wrong.push(format!(
                "{:?} at {range:?}: we bound {ours:?}, FCS {:?}",
                &src[range],
                at.iter().map(|u| u.decl).collect::<Vec<_>>()
            ));
        }
    }
    wrong
}

#[test]
fn object_expression_members_agree_with_fcs() {
    let programs: Vec<Program> = CASES
        .iter()
        .enumerate()
        .map(|(i, (c, _))| render(i, c))
        .collect();
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|p| temp_fs_file("object_expr", &p.src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), programs.len(), "one census line per program");

    let mut failures = Vec::new();
    for ((program, file), (_, accepted)) in programs.iter().zip(&census).zip(CASES) {
        let src = &program.src;
        let clean = file.ok && !file.has_check_errors;
        if clean != *accepted {
            failures.push(format!(
                "FCS {} this case, unlike the table says:\n{src}",
                if clean { "accepts" } else { "rejects" }
            ));
        }
        let rf = resolve(src);
        for w in wrong_commitments(src, &rf, file, clean) {
            failures.push(format!("{w}\n{src}"));
        }
        if !clean {
            continue;
        }
        let uses = census_resolve_uses(file, src);
        for &range in &program.must_commit {
            let fcs = uses
                .iter()
                .find(|u| range_of(u.start, u.end) == range)
                .map(|u| u.decl);
            let ours = rf
                .resolution_at(range)
                .and_then(|r| rf.resolved_def(r))
                .map(|d| d.range);
            match fcs {
                Some(CensusDecl::InFile(s, e)) if ours == Some(range_of(s, e)) => {}
                _ => failures.push(format!(
                    "{:?} at {range:?} must commit to FCS's {fcs:?}, we gave {ours:?}\n{src}",
                    &src[range]
                )),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The receiver's names, spelled every way a use can reach them, against an
/// enclosing value, enum, union or module of the same name: `` ``base`` `` (the base-class
/// receiver, which FCS binds even back-ticked), `` ``_`` `` (the discarded
/// self-identifier, which FCS binds under `member _.M`), and an ordinary name
/// the self-identifier may or may not take. Crossed with where the member
/// sits — an object expression over a class or an interface, a property
/// accessor, a type's own member — every commitment must be FCS's, and an
/// enclosing value no receiver name hides must be committed.
#[test]
fn receiver_names_never_bind_an_enclosing_value_fcs_hides() {
    const OUTER: [&str; 3] = ["``base``", "``_``", "x"];
    // What the enclosing name is: a value, or something a dotted use
    // `X.GetHashCode` could qualify through — an enum or union type whose
    // case is `GetHashCode`, a module with a `GetHashCode`.
    const ENCLOSING: [&str; 4] = [
        "let {x} = obj ()",
        "type {x} = GetHashCode = 1",
        "type {x} = | GetHashCode",
        "module {x} =\n    let GetHashCode () = 1",
    ];
    // The receiver as written before the member name, including none at
    // all — recovered input, where FCS synthesizes a discarded receiver.
    const SELF: [&str; 4] = ["_.", "s.", "x.", ""];
    let sites: [fn(&str, &str) -> String; 6] = [
        |me, u| {
            format!(
                "    {{ new System.Object() with\n        override {me}ToString() = string ({u}.GetHashCode()) }}\n"
            )
        },
        |me, u| format!("    {{ new IGet with\n        member {me}Get() = {u}.GetHashCode() }}\n"),
        |me, u| {
            format!(
                "    {{ new IProp with\n        member {me}P with get () = {u}.GetHashCode() }}\n"
            )
        },
        // A class's, a record's and a union's own member, rendered below.
        |_, _| String::new(),
        |_, _| String::new(),
        |_, _| String::new(),
    ];
    let mut programs = Vec::new();
    let mut must = Vec::new();
    // Rows whose member writes a receiver: the ones that can type-check.
    let mut with_receiver = Vec::new();
    for outer in OUTER {
        for (e, enclosing) in ENCLOSING.iter().enumerate() {
            for me in SELF {
                for (i, site) in sites.iter().enumerate() {
                    let mut src = String::from(
                        "module M\ntype IGet =\n    abstract Get : unit -> int\ntype IProp =\n    abstract P : int\n",
                    );
                    src.push_str(&enclosing.replace("{x}", outer));
                    src.push('\n');
                    let body = if i >= 3 {
                        // A type's own member, after the enclosing value: a
                        // class's, which has a base receiver, then a record's
                        // and a union's, which have none.
                        let name = format!("C{}{}{e}{i}", me.len(), outer.len());
                        let repr = match i {
                            3 => "()",
                            4 => " = { F: int }",
                            _ => " = | Case",
                        };
                        src.push_str(&format!(
                            "type {name}{repr} with\n    member {me}M() = {outer}.GetHashCode()\n"
                        ));
                        None
                    } else {
                        Some(site(me, outer))
                    };
                    if let Some(body) = body {
                        src.push_str("let run () =\n");
                        src.push_str(&body);
                    }
                    // The use: the last spelling of `outer` in the file.
                    let at = src.rfind(outer).unwrap();
                    let range = range_of(at, at + outer.len());
                    // An ordinary name no receiver takes is the enclosing value,
                    // and so is `` ``base`` `` where no base receiver exists.
                    let base_free = outer == "``base``" && i >= 4;
                    if (outer == "x" || base_free) && me != "x." && e == 0 {
                        must.push((programs.len(), range));
                    }
                    with_receiver.push(!me.is_empty());
                    programs.push(src);
                }
            }
        }
    }
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|src| temp_fs_file("object_receiver", src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), programs.len(), "one census line per program");
    let mut failures = Vec::new();
    let mut clean_rows = 0usize;
    for (i, (src, file)) in programs.iter().zip(&census).enumerate() {
        let clean = file.ok && !file.has_check_errors;
        clean_rows += usize::from(clean && with_receiver[i]);
        let rf = resolve(src);
        for w in wrong_commitments(src, &rf, file, clean) {
            failures.push(format!("{w}\n{src}"));
        }
        for (_, range) in must.iter().filter(|(row, _)| *row == i) {
            let committed = rf
                .resolution_at(*range)
                .is_some_and(|r| matches!(r, Resolution::Local(_) | Resolution::Item(_)));
            if clean && !committed {
                failures.push(format!(
                    "the enclosing value at {range:?} was declined\n{src}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let receiver_rows = with_receiver.iter().filter(|r| **r).count();
    assert!(
        clean_rows >= receiver_rows / 2,
        "only {clean_rows} of {receiver_rows} rows with a receiver type-check: \
         the table no longer exercises what it claims to"
    );
}

/// An upper-case parameter that names no union case is a variable binder
/// (FCS warns, FS0049), so in the body it is that binder, never whatever the
/// enclosing scope gives its name: a value, a type, an opened type's member.
/// Crossed with every place a pattern binds — an object-expression member, a
/// type member, a lambda, a `match` clause, a local or module function, a
/// module-level pattern binding — and graded with FSharp.Core and the BCL
/// referenced, so an opened member is really in scope.
#[test]
fn an_upper_case_parameter_hides_an_enclosing_value() {
    // `(prelude, name)`: what the enclosing scope gives the name.
    let enclosings = [
        ("let X = 123\n", "X"),
        ("type X = GetHashCode = 1\n", "X"),
        ("open type System.DateTime\n", "Now"),
    ];
    let bodies = [
        "let o = { new I with member _.M N = N.GetHashCode() }\n",
        "type C() =\n    member _.M (N: int) = N.GetHashCode()\n",
        "let l = fun (N: int) -> N.GetHashCode()\n",
        "let m (n: int) =\n    match n with\n    | N -> N.GetHashCode()\n",
        "let f () =\n    let g (N: int) = N.GetHashCode()\n    g 1\n",
        "let h (N: int) = N.GetHashCode()\n",
        "module Inner =\n    let (a, N) = (1, 2)\n    let b = N.GetHashCode()\n",
    ];
    let mut programs: Vec<String> = Vec::new();
    for (enclosing, name) in enclosings {
        for body in bodies {
            programs.push(format!(
                "module M\ntype I =\n    abstract M : int -> int\n{enclosing}{}",
                body.replace('N', name)
            ));
        }
    }
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|src| temp_fs_file("upper_param", src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    let mut failures = Vec::new();
    for (src, file) in programs.iter().zip(&census) {
        let clean = file.ok && !file.has_check_errors;
        if !clean {
            failures.push(format!("FCS rejects the case:\n{src}"));
        }
        let parsed = parse(src);
        let recovery = SyntaxRecovery::of(&parsed);
        let impl_file = ImplFile::cast(parsed.root).expect("impl file");
        let rf = resolve_file(
            &impl_file,
            &ProjectItems::default(),
            crate::common::full_bcl_env(),
            &recovery,
        );
        for w in wrong_commitments(src, &rf, file, clean) {
            failures.push(format!("{w}\n{src}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A parameter that is not a plain name (`_`, a tuple) gets a synthesized
/// binding FCS names `_arg1`, `_arg2`, …, and a use of that spelling in the
/// body is that binding, not an enclosing value of the name. Crossed with
/// every place a parameter binds.
#[test]
fn a_synthesized_argument_name_hides_an_enclosing_value() {
    const PRELUDE: &str =
        "module M\ntype I =\n    abstract M : int -> int\nlet _arg1 = 123\nlet _arg2 = 456\n";
    let bodies = [
        "let o = { new I with member _.M _ = _arg1 }\n",
        "type C() =\n    member _.M (_: int) = _arg1\n",
        "let l = fun (_: int) -> _arg1\n",
        "let f (_: int) = _arg1\n",
        "let g () =\n    let h (_: int) (_: int) = _arg2\n    h 1 2\n",
        "let t ((a: int), (b: int)) = _arg1\n",
    ];
    let programs: Vec<String> = bodies.iter().map(|b| format!("{PRELUDE}{b}")).collect();
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|src| temp_fs_file("synth_arg", src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    let mut failures = Vec::new();
    for (src, file) in programs.iter().zip(&census) {
        let clean = file.ok && !file.has_check_errors;
        let rf = resolve(src);
        for w in wrong_commitments(src, &rf, file, clean) {
            failures.push(format!("{w}\n{src}"));
        }
        if !clean {
            failures.push(format!("FCS rejects the case:\n{src}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

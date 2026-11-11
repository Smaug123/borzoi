//! Differential test (`parser::parse` vs FCS at every language version): the
//! language features FCS gates *while parsing*, with
//! `CheckLanguageFeatureAndRecover` (FS3350) or a lexer check, and then parses
//! anyway. Each fixture is parsed at every version on both sides, and the
//! **whole** diagnostic set — span, severity and message — must agree, as must
//! the normalised tree.
//!
//! The gated features, with the version FCS introduces each at
//! (`LanguageFeatures.fs`) and where it checks it:
//!
//! - `DotlessFloat32Literal` (5.0): `1f` is FS1153 below it (`lex.fsl`).
//! - `AttributesToRightOfModuleKeyword` (6.0): `module [<A>] M`
//!   (`pars.fsy` `moduleIntro`).
//! - `InterfacesWithAbstractStaticMembers` (7.0): `static abstract`. From 7.0
//!   on FCS draws the FS3535 "advanced feature" *warning* there instead, which
//!   we do not report (see [`FS3535`]).
//! - `ConstraintIntersectionOnFlexibleTypes` (8.0): `'T & #I` in type position
//!   and in a type-parameter declaration.
//! - `WhileBang` (8.0): `while!`.
//! - `AllowTypedLetUseAndBang` (10.0): `let! x: T`, `use!`/`and!` alike, and
//!   `return e: T` / `yield e: T` and their `!` forms.
//!
//! The gate is diagnostic-only: FCS recovers and builds the tree it builds at
//! the newest version, and so do we, so the tree is also checked to be the same
//! at every version. The one exception is the value of a dotless `float32`
//! literal, for which FCS's lexer substitutes `0.0f` below 5.0
//! ([`INVALID_FLOAT`]).
//!
//! Two parse-time gates are *not* modelled, and [`unmodelled_gates_are_pinned`]
//! holds them to that: the accessor shorthand `_.M` (`AccessorFunctionShorthand`,
//! 8.0) and the integer `for _ = …` loop (`WildCardInForLoop`, 4.7). Modelling
//! either would make every file using it version-dependent under
//! `diagnostics_are_version_invariant` (see `docs/fcs-divergences.md`,
//! "Language-version pinning").

use std::collections::{BTreeSet, HashSet};
use std::io::Write;

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::{FileKind, Parse, ParseOptions, parse_with_options};
use tempfile::NamedTempFile;

use crate::common::{fcs_all_diagnostics, fcs_ast_batch_at, normalised_ast};

/// FCS's FS3535 warning on a `static abstract` member from F# 7.0 on. We do not
/// report it: interfaces with static abstract members are routinely declared
/// under `#nowarn "3535"`, which FCS honours and we do not consume (the
/// "`#nowarn` / `#warnon` numbers parsed but not consumed" entry in
/// `docs/fcs-divergences.md`), so reporting it would flag files the build does
/// not. It is the only FCS diagnostic the comparison leaves out.
const FS3535: &str = "Declaring \"interfaces with static abstract methods\" is an advanced \
     feature. See https://aka.ms/fsharp-iwsams for guidance. You can disable this warning by \
     using '#nowarn \"3535\"' or '--nowarn:3535'.";

/// FCS's FS1153, the lexer's error on a dotless `float32` literal below F# 5.0.
/// FCS's lexer then substitutes `0.0f` for the literal's value, where our tree
/// keeps the text written; a fixture's tree is still compared with FCS at every
/// version that does not report this, and is checked to be the same at all.
const INVALID_FLOAT: &str = "Invalid floating point number";

/// One version per band of every gate below, ascending, with the
/// `<LangVersion>` token FCS reads.
const VERSIONS: &[(LanguageVersion, &str)] = &[
    (LanguageVersion::V4_6, "4.6"),
    (LanguageVersion::V4_7, "4.7"),
    (LanguageVersion::V5_0, "5.0"),
    (LanguageVersion::V6_0, "6.0"),
    (LanguageVersion::V7_0, "7.0"),
    (LanguageVersion::V8_0, "8.0"),
    (LanguageVersion::V9_0, "9.0"),
    (LanguageVersion::V10_0, "10.0"),
    (LanguageVersion::Preview, "preview"),
];

/// Whether FCS gates something in the fixture: a gated fixture must draw an
/// FCS diagnostic at the floor, a control none at any version. Checked against
/// FCS, so a fixture cannot silently stop testing what it is listed for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    Gated,
    Control,
}

use Expect::{Control, Gated};

const IMPL: &[(&str, Expect)] = &[
    // `while!`: the keyword.
    (
        "module M\nlet f () = async {\n    while! async { return false } do\n        ()\n    return 1 }\n",
        Gated,
    ),
    // Constraint intersection: the whole intersection type…
    (
        "module M\nlet f (x: #seq<int> & #System.IDisposable) = ()\n",
        Gated,
    ),
    (
        "module M\nlet f<'T when 'T :> seq<int>> (x: 'T & #System.IDisposable) = ()\n",
        Gated,
    ),
    (
        "module M\nlet f (x: 'T & #seq<int> & #System.IDisposable) = ()\n",
        Gated,
    ),
    // …and, in a type-parameter declaration, the `&` and its constraints.
    (
        "module M\nlet f<'a & #System.IDisposable> (x: 'a) = ()\n",
        Gated,
    ),
    (
        "module M\nlet f<'a & #System.IDisposable & #seq<int>, 'b> (x: 'a) (y: 'b) = ()\n",
        Gated,
    ),
    // `static abstract`: FS3350 below 7.0, the FS3535 warning from it.
    (
        "module M\ntype I =\n    static abstract M : int -> int\n",
        Gated,
    ),
    (
        "module M\ntype I =\n    static abstract member M : int -> int\n",
        Gated,
    ),
    (
        "module M\ntype I =\n    static abstract M : int -> int\n    static abstract member N : int\n",
        Gated,
    ),
    (
        "module M\ntype I<'T when 'T :> I<'T>> =\n    static abstract member Zero : 'T\n    abstract member Plain : int\n",
        Gated,
    ),
    // An unannotated `abstract` is not gated.
    ("module M\ntype I =\n    abstract M : int -> int\n", Control),
    // Typed `let!` / `use!` / `and!`: the annotation's type.
    (
        "module M\nlet f () = async {\n    let! x: int = async { return 1 }\n    return x }\n",
        Gated,
    ),
    (
        "module M\nlet f () = async {\n    use! x: System.IDisposable = async { return null }\n    return 1 }\n",
        Gated,
    ),
    (
        "module M\nlet f () = async {\n    let! x: int = async { return 1 }\n    and! y: int = async { return 1 }\n    return x }\n",
        Gated,
    ),
    (
        "module M\nlet f () = async {\n    let! x = async { return 1 }\n    and! y: int list = async { return [] }\n    return x }\n",
        Gated,
    ),
    // Typed `return` / `return!` / `yield` / `yield!`: the expression and its
    // annotation.
    ("module M\nlet f () = async {\n    return 1: int }\n", Gated),
    (
        "module M\nlet f () = async {\n    return! async { return 1 }: Async<int> }\n",
        Gated,
    ),
    ("module M\nlet s = seq { yield 1: int }\n", Gated),
    ("module M\nlet s = seq { yield! [ 1 ]: int list }\n", Gated),
    ("module M\nlet s = [ yield 1: int ]\n", Gated),
    // Parenthesised annotations, and a plain typed `let`, are not gated.
    (
        "module M\nlet f () = async {\n    let! (x: int) = async { return 1 }\n    return (x: int) }\n",
        Control,
    ),
    ("module M\nlet f () =\n    let x: int = 1 in x\n", Control),
    // A comprehension's `->` yields through another production, not gated.
    (
        "module M\nlet a = [ for i in 0 .. 1 -> i : int ]\nlet b = seq { for i in 0 .. 1 -> (i : int) }\n",
        Control,
    ),
    (
        "module M\nlet f () = async {\n    let x: int = 1\n    return x }\n",
        Control,
    ),
    // Attributes to the right of `module`: FCS reports at the (empty)
    // `opt_rec`, which takes the range of the token after it.
    ("module [<AutoOpen>] Foo\nlet x = 1\n", Gated),
    ("module [<AutoOpen>] rec Foo\nlet x = 1\n", Gated),
    ("module [<AutoOpen>] private Foo.Bar\nlet x = 1\n", Gated),
    ("module [<AutoOpen>] private rec Foo\nlet x = 1\n", Gated),
    ("module [<A>]\n  [<B>] Foo\nlet x = 1\n", Gated),
    (
        "module M\nmodule [<AutoOpen>] Inner =\n    let x = 1\n",
        Gated,
    ),
    (
        "module M\n[<AutoOpen>]\nmodule [<RequireQualifiedAccess>] Inner =\n    let x = 1\n",
        Gated,
    ),
    (
        "namespace N\nmodule [<AutoOpen>]  Inner =\n    let x = 1\n",
        Gated,
    ),
    (
        "namespace N\nmodule [<A>] internal rec Inner =\n    let x = 1\n",
        Gated,
    ),
    ("[<AutoOpen>]\nmodule M\nlet x = 1\n", Control),
    (
        "module M\n[<AutoOpen>]\nmodule Inner =\n    let x = 1\n",
        Control,
    ),
    // A dotless `float32` literal; the dotted, exponent and hex forms are not
    // gated.
    (
        "module M\nlet f () = 1.0f\nlet g () = 1e3f\nlet h () = 0x1f\nlet i = 1_0f\n",
        Gated,
    ),
    (
        "module M\nlet j = -2f\nlet k = 3F\nlet l = (+4f)\nlet m = 1__0f\n",
        Gated,
    ),
    ("module M\nlet m = 0x3f800000lf\nlet n = 1.5F\n", Control),
    (
        "module M\nlet f x =\n    match x with\n    | 1f -> 0\n    | _ -> 1\n",
        Gated,
    ),
    // Several gates in one file.
    (
        "module [<AutoOpen>] M\ntype I =\n    static abstract M : int\nlet f () = async {\n    let! x: int = async { return 1 }\n    while! async { return false } do\n        ()\n    return x }\nlet g (x: #seq<int> & #System.IDisposable) = 2f\n",
        Gated,
    ),
    // Gated constructs in dead `#if` code are never lexed.
    (
        "module M\n#if NEVER\nlet x = 1f\nlet f () = async { while! async { return false } do () }\n#endif\nlet y = 1\n",
        Control,
    ),
];

const SIG: &[(&str, Expect)] = &[
    (
        "module M\ntype I =\n    static abstract M : int -> int\n",
        Gated,
    ),
    (
        "module M\ntype I =\n    static abstract member M : int -> int\n",
        Gated,
    ),
    ("module [<AutoOpen>] M\nval x : int\n", Gated),
    (
        "module M\nmodule [<AutoOpen>] Inner =\n    val x : int\n",
        Gated,
    ),
    (
        "module M\nval f<'a & #System.IDisposable> : 'a -> unit\n",
        Gated,
    ),
    (
        "module M\nval f : #seq<int> & #System.IDisposable -> unit\n",
        Gated,
    ),
    ("module M\nval x : float32\n", Control),
];

fn parse_at(source: &str, kind: FileKind, lang: LanguageVersion) -> Parse {
    let symbols = HashSet::new();
    parse_with_options(
        source,
        ParseOptions {
            file_kind: kind,
            symbols: &symbols,
            lang,
        },
    )
}

fn source_file(source: &str, kind: FileKind) -> NamedTempFile {
    let suffix = match kind {
        FileKind::Impl => ".fs",
        FileKind::Sig => ".fsi",
    };
    let mut tmp = NamedTempFile::with_suffix(suffix).expect("create tempfile");
    tmp.write_all(source.as_bytes()).expect("write source");
    tmp
}

type Diagnostic = (usize, usize, String, &'static str);

fn our_diagnostics(parse: &Parse) -> BTreeSet<Diagnostic> {
    let errors = parse
        .errors
        .iter()
        .map(|e| (e.span.start, e.span.end, e.message.clone(), "Error"));
    let warnings = parse
        .warnings
        .iter()
        .map(|e| (e.span.start, e.span.end, e.message.clone(), "Warning"));
    errors.chain(warnings).collect()
}

fn fcs_diagnostics(json: &str, source: &str) -> BTreeSet<Diagnostic> {
    fcs_all_diagnostics(json, source)
        .into_iter()
        .map(|(span, message, severity)| {
            let severity = match severity.as_str() {
                "Error" => "Error",
                "Warning" => "Warning",
                other => panic!("unexpected FCS severity {other:?}"),
            };
            (span.start, span.end, message, severity)
        })
        .filter(|(_, _, message, _)| message != FS3535)
        .collect()
}

/// Every way `source` diverges from FCS across [`VERSIONS`], one line each, so
/// a run reports the whole grid rather than its first cell.
fn check(source: &str, kind: FileKind, expect: Expect) -> Vec<String> {
    let mut failures = Vec::new();
    let tmp = source_file(source, kind);
    let preview = parse_at(source, kind, LanguageVersion::Preview);
    for &(lang, lang_text) in VERSIONS {
        let json = fcs_ast_batch_at(tmp.path(), lang_text);
        let fcs_diags = fcs_diagnostics(&json, source);
        if lang == LanguageVersion::MIN && fcs_diags.is_empty() == (expect == Gated) {
            failures.push(format!(
                "{source:?} is listed as {expect:?}, but FCS at {lang_text} reports {fcs_diags:?}"
            ));
        }
        if expect == Control && !fcs_diags.is_empty() {
            failures.push(format!(
                "control {source:?}: FCS reports {fcs_diags:?} at {lang_text}"
            ));
        }
        let ours = parse_at(source, kind, lang);
        let our_diags = our_diagnostics(&ours);
        if our_diags != fcs_diags {
            failures.push(format!(
                "diagnostics diverge from FCS at {lang_text} for {source:?}\n  ours: {our_diags:?}\n  fcs:  {fcs_diags:?}"
            ));
        }
        let fcs = normalised_ast::normalise_fcs_dump(&json);
        let rust = normalised_ast::normalise_parse(&ours);
        let fcs_substituted_a_value = fcs_diags.iter().any(|d| d.2 == INVALID_FLOAT);
        if rust != fcs && !fcs_substituted_a_value {
            failures.push(format!(
                "AST divergence at {lang_text} for {source:?}\n  rust: {rust:?}\n  fcs:  {fcs:?}"
            ));
        }
        if !ours.root.structurally_eq(&preview.root) {
            failures.push(format!(
                "the gates are diagnostic-only, but the {lang_text} tree of {source:?} is not the preview tree"
            ));
        }
    }
    failures
}

fn check_all(fixtures: &[(&str, Expect)], kind: FileKind) {
    let failures: Vec<String> = fixtures
        .iter()
        .flat_map(|&(source, expect)| check(source, kind, expect))
        .collect();
    assert!(
        failures.is_empty(),
        "{} divergence(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn impl_feature_gates_match_fcs_at_every_version() {
    check_all(IMPL, FileKind::Impl);
}

#[test]
fn sig_feature_gates_match_fcs_at_every_version() {
    check_all(SIG, FileKind::Sig);
}

/// Gated constructs in input both sides also reject and recover from — a
/// missing annotation, a malformed literal. Recovery diagnostics are not the
/// subject here, so only the modelled gates' diagnostics are compared, at
/// every version.
#[test]
fn feature_gates_on_recovered_input_match_fcs() {
    const RECOVERED: &[(&str, Expect)] = &[
        (
            "module Module\n\nlet _ =\n    async { \n        return new MyType() :\n    }\n",
            Gated,
        ),
        (
            "module Module\n\nseq {\n    yield! [ 1 .. 10 ] :\n}\n",
            Gated,
        ),
        (
            "module M\nlet f () = async {\n    let! x : = async { return 1 }\n    return x }\n",
            Gated,
        ),
        // Not an integer, so not a dotless `float32`: an invalid numeric
        // literal at every version.
        ("module M\nlet x = 1_f\nlet y = 2__f\n", Control),
    ];
    let mut failures = Vec::new();
    for &(source, expect) in RECOVERED {
        let tmp = source_file(source, FileKind::Impl);
        let mut gated_somewhere = false;
        for &(lang, lang_text) in VERSIONS {
            let json = fcs_ast_batch_at(tmp.path(), lang_text);
            let fcs = modelled_gate_diagnostics(
                fcs_all_diagnostics(&json, source)
                    .into_iter()
                    .map(|(span, message, _)| (span, message)),
                source,
                lang,
            );
            gated_somewhere |= !fcs.is_empty();
            let ours = modelled_gate_diagnostics(
                parse_at(source, FileKind::Impl, lang)
                    .errors
                    .into_iter()
                    .map(|e| (e.span, e.message)),
                source,
                lang,
            );
            if ours != fcs {
                failures.push(format!(
                    "gate diagnostics diverge at {lang_text} for {source:?}\n  ours: {ours:?}\n  fcs:  {fcs:?}"
                ));
            }
        }
        if gated_somewhere != (expect == Gated) {
            failures.push(format!(
                "{source:?} is listed as {expect:?}, but FCS gates something: {gated_somewhere}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The parse-time gates deliberately left unmodelled, each with the version
/// FCS introduces it at and the error number it reports below that.
///
/// Modelling either would make every file using the construct
/// version-dependent under `diagnostics_are_version_invariant`, so a guessed
/// language version (an orphan buffer, or a project whose `LangVersion` the
/// evaluator cannot pin) would retain no diagnostics for it — and `_.M` lambdas
/// and `for _ = …` loops are common in real code. Until that trade-off is
/// decided, this test pins the leniency: FCS errors below the threshold and we
/// do not. It fails if either side changes.
#[test]
fn unmodelled_gates_are_pinned() {
    const UNMODELLED: &[(&str, LanguageVersion, &str, i64)] = &[
        (
            "module M\nlet f xs = xs |> List.map (_.Length) |> List.map _.ToString()\n",
            LanguageVersion::V8_0,
            "7.0",
            3350,
        ),
        (
            "module M\nlet f () =\n    for _ = 1 to 3 do\n        ()\n",
            LanguageVersion::V4_7,
            "4.6",
            525,
        ),
    ];
    for &(source, introduced, below, error_number) in UNMODELLED {
        let tmp = source_file(source, FileKind::Impl);
        let json = fcs_ast_batch_at(tmp.path(), below);
        assert!(
            !crate::common::fcs_diagnostic_spans(&json, source, error_number).is_empty(),
            "FCS at {below} must report FS{error_number:04} for {source:?}",
        );
        for &(lang, _) in VERSIONS {
            let ours = parse_at(source, FileKind::Impl, lang);
            assert!(
                ours.errors.is_empty() && ours.warnings.is_empty(),
                "{source:?} is lenient at {lang} (FCS gates it on {introduced}): {:?} {:?}",
                ours.errors,
                ours.warnings,
            );
        }
    }
}

/// The modelled gates' diagnostics at `lang` for one file: the FS3350s naming a
/// modelled feature, and the FS1153s on a `float32` literal (FCS reports
/// FS1153 for other bad literals too).
/// Nullness is left out: below 9.0 FCS does not parse `string | null` as a
/// nullness annotation at all (`docs/completed/ast-versioning-nullness-proof.md`).
fn modelled_gate_diagnostics(
    diags: impl IntoIterator<Item = (std::ops::Range<usize>, String)>,
    source: &str,
    lang: LanguageVersion,
) -> BTreeSet<(usize, usize, String)> {
    use borzoi_cst::language_version::LanguageFeature;
    let feature_messages: Vec<String> = [
        LanguageFeature::AttributesToRightOfModuleKeyword,
        LanguageFeature::InterfacesWithAbstractStaticMembers,
        LanguageFeature::ConstraintIntersectionOnFlexibleTypes,
        LanguageFeature::WhileBang,
        LanguageFeature::AllowTypedLetUseAndBang,
    ]
    .map(|f| f.not_available_message(lang))
    .to_vec();
    // Any decimal `float32`-suffixed literal: broader than the dotless form, so
    // an FS1153 the gate reports on a literal FCS does not is still compared.
    let float32_literal = |span: &std::ops::Range<usize>| {
        source
            .get(span.clone())
            .is_some_and(|text| text.ends_with(['f', 'F']) && !text.contains(['x', 'X']))
    };
    diags
        .into_iter()
        .filter(|(span, message)| {
            feature_messages.contains(message)
                || (message == INVALID_FLOAT && float32_literal(span))
        })
        .map(|(span, message)| (span.start, span.end, message))
        .collect()
}

/// Every corpus file's modelled gate diagnostics at F# 4.6, against FCS's.
/// The fixtures above are hand-picked; this is where a construct they miss
/// (a production that shares a gated node but not FCS's check, say) shows up.
/// Each file where either side reports one is pinned in
/// `tests/manifests/feature_gate_corpus.txt` as `agree` or `diverge`.
#[test]
#[ignore = "full-corpus differential parse at F# 4.6 (us + FCS); run with --ignored under nix develop"]
fn feature_gates_match_fcs_over_corpus() {
    use crate::common::corpus_manifest::{
        check_manifest, corpus_count_item, corpus_relative, regenerate_ignored, sort_by_corpus_key,
    };
    use crate::common::{collect_fsharp_corpus_files, corpus_root, par_map, read_corpus_source};
    use borzoi_oracle_harness::manifest::Manifest;

    let root = corpus_root();
    let mut files = collect_fsharp_corpus_files(&root)
        .unwrap_or_else(|err| panic!("walk F# corpus under {}: {err}", root.display()));
    assert!(!files.is_empty(), "no F# files under {root:?}");
    sort_by_corpus_key(&root, &mut files);
    // FCS's service parser defines these implicitly; see `parser_corpus_diff`.
    let symbols = |path: &std::path::Path| -> HashSet<String> {
        let script = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("fsx"));
        let names: [&str; 2] = if script {
            ["INTERACTIVE", "EDITING"]
        } else {
            ["COMPILED", "EDITING"]
        };
        names.iter().map(|s| s.to_string()).collect()
    };
    let verdicts = par_map(files.len(), |i| {
        let path = &files[i];
        let source = match read_corpus_source(path) {
            Ok(source) => source,
            Err(err) if err.is_non_utf8() => return None,
            Err(err) => panic!("{err}"),
        };
        let kind = if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("fsi"))
        {
            FileKind::Sig
        } else {
            FileKind::Impl
        };
        let symbols = symbols(path);
        let parse = parse_with_options(
            &source,
            ParseOptions {
                file_kind: kind,
                symbols: &symbols,
                lang: LanguageVersion::V4_6,
            },
        );
        let ours = modelled_gate_diagnostics(
            parse.errors.into_iter().map(|e| (e.span, e.message)),
            &source,
            LanguageVersion::V4_6,
        );
        let json = fcs_ast_batch_at(path, "4.6");
        let fcs = if json.contains("\"Diagnostics\":") {
            modelled_gate_diagnostics(
                fcs_all_diagnostics(&json, &source)
                    .into_iter()
                    .map(|(span, message, _)| (span, message)),
                &source,
                LanguageVersion::V4_6,
            )
        } else {
            return Some("fcs-error".to_string());
        };
        match (ours.is_empty() && fcs.is_empty(), ours == fcs) {
            (true, _) => None,
            (false, true) => Some("agree".to_string()),
            (false, false) => {
                eprintln!(
                    "{}: ours-only {:?}, fcs-only {:?}",
                    path.display(),
                    ours.difference(&fcs).collect::<Vec<_>>(),
                    fcs.difference(&ours).collect::<Vec<_>>(),
                );
                Some("diverge".to_string())
            }
        }
    });
    let agreeing = verdicts.iter().flatten().filter(|v| *v == "agree").count();
    assert!(agreeing > 0, "no corpus file exercises a modelled gate");
    let entries = std::iter::once(corpus_count_item(files.len())).chain(
        files
            .iter()
            .zip(verdicts)
            .filter_map(|(path, verdict)| Some((corpus_relative(&root, path), verdict?))),
    );
    let manifest = Manifest::from_keyed(entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check_manifest(
        "feature_gate_corpus",
        &manifest,
        &regenerate_ignored("parser_diff_feature_gates"),
    );
}

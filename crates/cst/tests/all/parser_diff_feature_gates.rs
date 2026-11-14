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
//! - `AccessorFunctionShorthand` (8.0): `_.M`, at its `_.`.
//! - `ParsedHashDirectiveArgumentNonQuotes` (9.0) and `ScopedNowarn` (10.0):
//!   a `#nowarn` / `#warnon` argument unquoted or `FS`-prefixed, a top-level
//!   `#warnon`, a directive in a nested module, an argument that is not a
//!   number (`WarnScopes.fs`).
//! - `AllowTypedLetUseAndBang` (10.0): `let! x: T`, `use!`/`and!` alike, and
//!   `return e: T` / `yield e: T` and their `!` forms.
//! - `BetterAnonymousRecordParsing` (10.0): the lex-filter's split of `>|}`
//!   and `@>|}`, reported at FCS's lexbuf lexeme.
//!
//! The gate is diagnostic-only: FCS recovers and builds the tree it builds at
//! the newest version, and so do we, so the tree is also checked to be the same
//! at every version. The one exception is the value of a dotless `float32`
//! literal, for which FCS's lexer substitutes `0.0f` below 5.0
//! ([`INVALID_FLOAT`]).
//!
//! The gates whose FCS effect is `Reshape` are graded apart, in
//! [`reshape_gates_match_fcs_at_every_version`]: below the feature's version
//! FCS raises a parse error and recovers to a different tree, where we report
//! the same error and keep the newer tree, so there the trees are graded by
//! the recovery relation (`common::recovery`).

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

/// FCS's FS0010 for a `_.` pattern below F# 4.7 (`parsUnexpectedSymbolDot`).
const UNEXPECTED_SYMBOL_DOT: &str =
    "Unexpected symbol '.' in member definition. Expected 'with', '=' or other token.";

/// FCS's FS0525 (`parsIntegerForLoopRequiresSimpleIdentifier`), which it
/// reports for `for _ = …` below F# 4.7.
const FOR_LOOP_REQUIRES_SIMPLE_IDENTIFIER: &str =
    "An integer for loop must use a simple identifier";

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
    (LanguageVersion::V11_0, "11.0"),
    (LanguageVersion::Preview, "preview"),
];

/// Whether FCS gates something in the fixture: a gated fixture must draw an
/// FCS diagnostic at some version (at the floor for every gate but the warn
/// directives' FS0203, which appears from 9.0), a control none at any version.
/// Checked against FCS, so a fixture cannot silently stop testing what it is
/// listed for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    Gated,
    Control,
}

use Expect::{Control, Gated};

const IMPL: &[(&str, Expect)] = &[
    // `_.M`: the `_.`.
    (
        "module M\nlet f xs = xs |> List.map (_.Length) |> List.map _.ToString()\n",
        Gated,
    ),
    // `>|}` closing a type application, and `@>|}` closing a quotation: FCS
    // reports at its lexbuf's lexeme, which is the `>|}` itself, the `@>|}`
    // when nothing peeked it, and otherwise the token handed over before it.
    ("module M\nlet x = {| A = id<int>|}\n", Gated),
    (
        "module M\nlet y = {| B = <@ 1 @>|}\nlet z = {| C = <@@ foo bar @@>|}\n",
        Gated,
    ),
    ("module M\nlet y = {| B = <@ (1) @>|}.B\n", Gated),
    // FCS folds the sign into the literal before handing it over.
    (
        "module M\nlet y = {| B = <@ -1 @>|}\nlet z = {| C = <@ f -2.0 @>|}\n",
        Gated,
    ),
    ("module M\nlet y = {| B = <@ 1 @> |}\n", Control),
    // `#nowarn` / `#warnon`: unquoted and `FS`-prefixed arguments, a
    // top-level `#warnon`, a directive in a nested module, arguments that are
    // not numbers, a repeated `#warnon`, and malformed directives.
    (
        "module M\n#nowarn 40\n#nowarn FS41 \"FS42\" \"43\" \"\"\"44\"\"\"\nlet x = 1\n",
        Gated,
    ),
    (
        "module M\n#warnon \"40\"\nmodule N =\n#nowarn \"41\"\n    let x = 1\nlet y = 2\n",
        Gated,
    ),
    (
        "module M\n#nowarn \"abc\" \"\" \"FS\" \"99999999999\" xyz\nlet x = 1\n",
        Gated,
    ),
    (
        "module M\n#nowarn \"40\"\n#warnon \"40\"\n#warnon \"40\"\n#warnon \"41\"\n#warnon \"41\"\nlet x = 1\n",
        Gated,
    ),
    (
        "module M\n#nowarn\n#nowarnx 1\n#nowarn;; \"43\"\n#nowarn 45;46\n#nowarn \"47\";;\nlet x = 1\n",
        Gated,
    ),
    (
        "module M\n#nowarn \"40\" \"\"\"41\"\"\" // c\n#nowarn \"40\"\nlet x = 1\n",
        Control,
    ),
    // A directive's range is counted in UTF-16 columns from its line's start.
    (
        "module M\nmodule N =\n\t#nowarn \"40\" // é\n    let x = 1\n",
        Gated,
    ),
    // `Int32.TryParse` admits a sign, surrounding white space and trailing NULs.
    (
        "module M\n#nowarn \"\t40\" \"41\t\" \"+42\" \"\x0B43\x0C\" \"44\0\" \"45\t\0\"\nlet x = 1\n",
        Control,
    ),
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

/// Our parse's diagnostics, less the gates of the features FCS checks only in
/// its type checker, which FCS's parse does not report (sema's
/// `infer_legality_pin_diff` holds them to its check).
fn our_diagnostics(parse: &Parse) -> BTreeSet<Diagnostic> {
    use borzoi_cst::language_version::FcsStage;
    use borzoi_cst::parser::DiagnosticClass;
    let errors = parse
        .errors
        .iter()
        .filter(|e| {
            !matches!(e.class, DiagnosticClass::Legality { feature }
                if feature.fcs_stages() == [FcsStage::Check])
        })
        .map(|e| (e.span.start, e.span.end, e.message.clone(), "Error"));
    let warnings = parse
        .warnings
        .iter()
        .map(|e| (e.span.start, e.span.end, e.message.clone(), "Warning"));
    errors.chain(warnings).collect()
}

/// FCS's parse diagnostics, less [`FS3535`] and its informational ones (the
/// FS3876 a repeated `#nowarn` draws from 10.0), which a parse of ours, having
/// errors and warnings only, does not report.
fn fcs_diagnostics(json: &str, source: &str) -> BTreeSet<Diagnostic> {
    fcs_all_diagnostics(json, source)
        .into_iter()
        .filter_map(|(span, message, severity)| {
            let severity = match severity.as_str() {
                "Error" => "Error",
                "Warning" => "Warning",
                "Info" => return None,
                other => panic!("unexpected FCS severity {other:?}"),
            };
            Some((span.start, span.end, message, severity))
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
    let mut fcs_gates = false;
    for &(lang, lang_text) in VERSIONS {
        let json = fcs_ast_batch_at(tmp.path(), lang_text);
        let fcs_diags = fcs_diagnostics(&json, source);
        fcs_gates |= !fcs_diags.is_empty();
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
    if expect == Gated && !fcs_gates {
        failures.push(format!(
            "{source:?} is listed as gated, but FCS reports nothing at any version"
        ));
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

/// The gates whose FCS effect is `Reshape`: `_.` in a pattern
/// (`SingleUnderscorePattern`, FS0010 at the `.`) and `for _ = …`
/// (`WildCardInForLoop`, FS0525 at the `_`), both 4.7. Below the feature's
/// version FCS raises a parse error and recovers to a tree of its own; we
/// report the same error and keep the newer tree.
///
/// At every version the diagnostics must agree exactly and our tree be the
/// preview tree. Where FCS's parse is clean the normalised trees must agree;
/// where it recovers, the trees are graded by the recovery relation
/// (`common::recovery`), which must not find them divergent and must compare
/// the declaration after the damage.
#[test]
fn reshape_gates_match_fcs_at_every_version() {
    use crate::common::recovery::{Relation, grade};
    const RESHAPE: &[&str] = &[
        "module M\nlet f () =\n    for _ = 1 to 3 do\n        ()\nlet g = 2\n",
        "module M\nlet f () =\n    for _ = 3 downto 1 do printfn \"x\"\nlet g = 2\n",
        "module M\ntype T() =\n    member _.M = 1\n    member this.N = 2\nlet g = 3\n",
        "module M\nlet f x =\n    match x with\n    | _.Case -> 1\n    | _ -> 2\nlet g = 3\n",
    ];
    let mut failures = Vec::new();
    for &source in RESHAPE {
        let tmp = source_file(source, FileKind::Impl);
        let preview = parse_at(source, FileKind::Impl, LanguageVersion::Preview);
        let mut recovered_somewhere = false;
        for &(lang, lang_text) in VERSIONS {
            let json = fcs_ast_batch_at(tmp.path(), lang_text);
            let fcs_diags = fcs_diagnostics(&json, source);
            let ours = parse_at(source, FileKind::Impl, lang);
            let our_diags = our_diagnostics(&ours);
            if our_diags != fcs_diags {
                failures.push(format!(
                    "diagnostics diverge from FCS at {lang_text} for {source:?}\n  ours: \
                     {our_diags:?}\n  fcs:  {fcs_diags:?}"
                ));
            }
            if !ours.root.structurally_eq(&preview.root) {
                failures.push(format!(
                    "the {lang_text} tree of {source:?} is not the preview tree"
                ));
            }
            if fcs_diags.is_empty() {
                let fcs = normalised_ast::normalise_fcs_dump(&json);
                let rust = normalised_ast::normalise_parse(&ours);
                if rust != fcs {
                    failures.push(format!(
                        "AST divergence at {lang_text} for {source:?}\n  rust: {rust:?}\n  \
                         fcs:  {fcs:?}"
                    ));
                }
            } else {
                recovered_somewhere = true;
                let verdict = grade(&ours, &HashSet::new(), &json, source);
                if verdict.relation == Relation::Divergent || verdict.compared == 0 {
                    failures.push(format!(
                        "recovered trees at {lang_text} for {source:?}: {} {}",
                        verdict.token(),
                        verdict.first_divergence.unwrap_or_default(),
                    ));
                }
            }
        }
        if !recovered_somewhere {
            failures.push(format!("FCS reports nothing for {source:?} at any version"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `ExtendedStringInterpolation` (8.0), a `Reshape` gate we follow rather than
/// keep the newer tree for: below 8.0 FCS's lexer reads a `$$"""` opener as a
/// `$"""` one, single braces delimiting a fill and `{{` a literal brace, and
/// reports FS1251 at the lexeme, which runs to the string's first fill or its
/// end. So does ours, and at every version the diagnostics and the normalised
/// trees must agree with FCS's.
#[test]
fn extended_interpolation_lexes_as_fcs_does_at_every_version() {
    const EXTENDED: &[&str] = &[
        "module M\nlet x = $$\"\"\"a{{1}}b\"\"\"\nlet g = 3\n",
        "module M\nlet y = $$\"\"\"a{x}b{{2}}c\"\"\"\nlet g = 3\n",
        "module M\nlet z = $$$\"\"\"{{{3}}} and {4}\"\"\"\nlet w = $\"\"\"p{5}q\"\"\"\n",
        "module M\nlet b = $$\"\"\"{{1}}\"\"\".Length\nlet c = $$\"\"\"%d{{7}}\"\"\"\n",
    ];
    let mut failures = Vec::new();
    for &source in EXTENDED {
        let tmp = source_file(source, FileKind::Impl);
        let mut gated = false;
        for &(lang, lang_text) in VERSIONS {
            let json = fcs_ast_batch_at(tmp.path(), lang_text);
            let fcs_diags = fcs_diagnostics(&json, source);
            gated |= !fcs_diags.is_empty();
            let ours = parse_at(source, FileKind::Impl, lang);
            let our_diags = our_diagnostics(&ours);
            if our_diags != fcs_diags {
                failures.push(format!(
                    "diagnostics diverge from FCS at {lang_text} for {source:?}\n  ours: \
                     {our_diags:?}\n  fcs:  {fcs_diags:?}"
                ));
            }
            let fcs = normalised_ast::normalise_fcs_dump(&json);
            let rust = normalised_ast::normalise_parse(&ours);
            if rust != fcs {
                failures.push(format!(
                    "AST divergence at {lang_text} for {source:?}\n  rust: {rust:?}\n  fcs:  \
                     {fcs:?}"
                ));
            }
            if !ours.shape_depends_on_language_version {
                failures.push(format!(
                    "{source:?} holds a `$$\"\"\"` string, so its shape depends on the version"
                ));
            }
        }
        if !gated {
            failures.push(format!("FCS reports nothing for {source:?} at any version"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Where FCS puts an indented `#nowarn`'s `DirectiveRange`, which its FS0236,
/// FS3875, FS3353 and top-level-`#warnon` FS3350 report at. The compiler the
/// feature table transcribes starts it after the spaces before the `#`
/// (`WarnScopes.fs`, `parseDirective`); the oracle's older FCS starts it at the
/// start of the line. We follow the compiler, so this pins the oracle's reading
/// with ours, and fails once the oracle moves (then delete it, and indent a
/// directive in `IMPL`).
#[test]
fn an_indented_directive_range_starts_at_its_hash() {
    let source = "module M\nmodule N =\n    #nowarn \"41\" // why\n    let x = 1\n";
    let line_start = source.find("    #nowarn").expect("the directive");
    let hash = source.find('#').expect("the directive");
    let end = source.find(" // why").expect("the comment") + " // why".len();
    let message = "Directives inside modules are ignored".to_string();
    let tmp = source_file(source, FileKind::Impl);
    let json = fcs_ast_batch_at(tmp.path(), "9.0");
    assert_eq!(
        fcs_diagnostics(&json, source),
        BTreeSet::from([(line_start, end, message.clone(), "Warning")]),
    );
    let ours = our_diagnostics(&parse_at(source, FileKind::Impl, LanguageVersion::V9_0));
    assert_eq!(ours, BTreeSet::from([(hash, end, message, "Warning")]));
}

/// The modelled gates' diagnostics at `lang` for one file: the FS3350s naming a
/// modelled feature, the FS1153s on a `float32` literal (FCS reports FS1153
/// for other bad literals too), the FS0010 of a `_.` pattern, the FS0525 of a
/// `for _ =` loop (FCS reports FS0525 for other loop patterns too), and the
/// FS0203 of a warn directive's argument.
/// Nullness is left out: below 9.0 FCS does not parse `string | null` as a
/// nullness annotation at all (`docs/completed/ast-versioning-nullness-proof.md`).
/// So are the warn-directive diagnostics reported at the directive's range
/// (FS0236, and `ScopedNowarn`'s FS3350), which the oracle starts elsewhere
/// for an indented directive ([`an_indented_directive_range_starts_at_its_hash`]).
fn modelled_gate_diagnostics(
    diags: impl IntoIterator<Item = (std::ops::Range<usize>, String)>,
    source: &str,
    lang: LanguageVersion,
) -> BTreeSet<(usize, usize, String)> {
    use borzoi_cst::language_version::LanguageFeature;
    let feature_messages: Vec<String> = [
        LanguageFeature::AttributesToRightOfModuleKeyword,
        LanguageFeature::InterfacesWithAbstractStaticMembers,
        LanguageFeature::AccessorFunctionShorthand,
        LanguageFeature::ConstraintIntersectionOnFlexibleTypes,
        LanguageFeature::WhileBang,
        LanguageFeature::ParsedHashDirectiveArgumentNonQuotes,
        LanguageFeature::AllowTypedLetUseAndBang,
        LanguageFeature::BetterAnonymousRecordParsing,
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
    let text_is =
        |span: &std::ops::Range<usize>, text: &str| source.get(span.clone()) == Some(text);
    diags
        .into_iter()
        .filter(|(span, message)| {
            feature_messages.contains(message)
                || (message == INVALID_FLOAT && float32_literal(span))
                || (message == UNEXPECTED_SYMBOL_DOT && text_is(span, "."))
                || (message == FOR_LOOP_REQUIRES_SIMPLE_IDENTIFIER && text_is(span, "_"))
                || message.starts_with("Invalid warning number '")
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

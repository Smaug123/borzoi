//! Differential test (`parser::parse` vs FCS): `#`-directives in a `.fs` file —
//! FCS's `hashDirective: HASH IDENT hashDirectiveArgs` (`pars.fsy:482`), i.e.
//! `SynModuleDecl.HashDirective(ParsedHashDirective(ident, args, _), _)`. The
//! normaliser compares the directive name and the argument list (string / int32
//! literals and source identifiers such as `__SOURCE_DIRECTORY__`), including
//! the canonicalised source-identifier expansion.

use crate::common::{assert_asts_match, assert_asts_match_fcs_rejects_ours_accepts};

/// A regular-string argument (`#I "/tmp"`).
#[test]
fn diff_ast_hash_regular_string_arg() {
    assert_asts_match("#I \"/tmp\"\n");
}

/// A regular-string argument with a lone surrogate escape. Hash directives use
/// `ParsedHashDirectiveArgument.String`, not `SynConst.String`, so this pins
/// that string carrier's raw UTF-16 payload too.
#[test]
fn diff_ast_hash_regular_string_arg_lone_surrogate() {
    assert_asts_match("#I \"\\uD800\"\n");
}

/// A verbatim-string argument (`#I @"C:\Temp"`).
#[test]
fn diff_ast_hash_verbatim_string_arg() {
    assert_asts_match("#I @\"C:\\Temp\"\n");
}

/// A source-identifier argument (`#I __SOURCE_DIRECTORY__`).
#[test]
fn diff_ast_hash_source_identifier_arg() {
    assert_asts_match("#I __SOURCE_DIRECTORY__\n");
}

/// A path-valued source-identifier argument for the physical source file.
#[test]
fn diff_ast_hash_source_file_arg() {
    assert_asts_match("#I __SOURCE_FILE__\n");
}

/// `__LINE__` as a source-identifier argument. Unlike the path-valued source
/// identifiers, this carries a concrete line number in the normalised model; put
/// it on line 2 so a spelling-only comparison would miss this.
#[test]
fn diff_ast_hash_line_source_identifier_arg() {
    assert_asts_match("// pad\n#I __LINE__\n");
}

/// A plain-identifier argument (`#nowarn FS`, `#time on`) — FCS's
/// `ParsedHashDirectiveArgument.Ident`, distinct from the magic source
/// identifiers. `#nowarn FS` is an invalid warning id, so FCS sets
/// `ParseHadErrors` while still emitting the directive AST.
#[test]
fn diff_ast_hash_plain_ident_args() {
    assert_asts_match_fcs_rejects_ours_accepts("#nowarn FS\n");
    assert_asts_match("#time on\n");
}

/// Integer arguments — decimal and hex (`#time 10`, `#time 0x10`); FCS's
/// `INT32` admits non-decimal integer tokens.
#[test]
fn diff_ast_hash_int_args() {
    assert_asts_match("#time 10\n");
    assert_asts_match("#time 0x10\n");
}

/// A backtick-quoted magic name is an ordinary `Ident` argument, not a
/// `SourceIdentifier` (only the bare keyword-string spelling is the latter).
#[test]
fn diff_ast_hash_quoted_magic_is_ident() {
    assert_asts_match("#I ``__SOURCE_DIRECTORY__``\n");
}

/// Two string arguments (`#load "a.fs" "b.fs"`).
#[test]
fn diff_ast_hash_two_string_args() {
    assert_asts_match("#load \"a.fs\" \"b.fs\"\n");
}

/// A `#`-directive followed by a sibling `let` — the directive must not swallow
/// the following declaration.
#[test]
fn diff_ast_hash_then_let() {
    assert_asts_match("#nowarn \"57\"\nlet x = 1\n");
}

/// A directive's arguments have a natural end, so FCS lets the next declaration
/// follow on the *same line* without a separator (`#I "/tmp" let x = 1`,
/// `#I "/tmp" #load "a.fs"`). Pins that we do not require a separator either.
#[test]
fn diff_ast_hash_then_decl_same_line() {
    assert_asts_match("#I \"/tmp\" let x = 1\n");
    assert_asts_match("#I \"/tmp\" #load \"a.fs\"\n");
}

/// `#line` reaches the parser as a real `HashDirective` (FCS lexer-consumes only
/// the light directive) — pin that it round-trips.
#[test]
fn diff_ast_hash_line() {
    assert_asts_match("#line 10\nlet x = 1\n");
}

/// The light-syntax directive `#light` is consumed by FCS's lexer and yields
/// **no** `SynModuleDecl.HashDirective`, unlike every other `#`-directive. Our
/// lexer surfaces it as `# light` (so it lands in the CST losslessly), but the
/// normaliser drops it — so a file that opens with `#light` projects to the same
/// decl list as FCS. Pins that (regression for the spurious-`HashDirective` bug).
#[test]
fn diff_ast_hash_light_is_not_a_directive() {
    assert_asts_match("#light\nlet x = 1\n");
    assert_asts_match("module M\n#light\nlet x = 1\n");
}

/// Only the *adjacent* `#light` spelling is lexer-consumed; a spaced `# light`
/// is an ordinary `#`-directive that FCS keeps as a `HashDirective`.
#[test]
fn diff_ast_hash_spaced_light_is_a_directive() {
    assert_asts_match("# light\nlet x = 1\n");
}

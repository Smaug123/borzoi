//! Differential test (`parser::parse` vs FCS): F#'s ML-compatibility *reserved
//! words* (`break`, `checked`, `component`, `constraint`, `continue`, `fori`,
//! `include`, `mixin`, `parallel`, `params`, `process`, `protected`, `pure`,
//! `sealed`, `trait`, `tailcall`, `virtual`) used as ordinary identifiers.
//!
//! FCS's lexer maps each of these to `RESERVED` in its keyword table, but
//! `KeywordOrIdentifierToken` immediately emits an FS0046 *warning* ("The
//! identifier '…' is reserved for future use by F#") and returns an `IDENT`
//! token — so the parser only ever sees an identifier and `ParseHadErrors`
//! stays `false` (`dotnet/fsharp/src/Compiler/SyntaxTree/LexHelpers.fs`).
//!
//! We mirror that: the words lex as `Token::Ident`, parse as identifiers
//! everywhere, and a lexeme-scan diagnostic pass records the FS0046 warning.
//! `assert_asts_match` only checks our *errors* list is empty, so a warning
//! doesn't perturb it — these are pure "we-reject / FCS-accepts" divergences
//! that resolve once the words stop erroring.

use crate::common::assert_asts_match;

/// `let break = 10` — the motivating corpus case
/// (`Conformance/LexicalAnalysis/IdentifiersAndKeywords/E_ReservedIdentKeywords.fs`).
/// A reserved word as a bound value pattern.
#[test]
fn diff_reserved_let_binding_break() {
    assert_asts_match("let break = 10\n");
}

/// Every reserved word in turn as a `let`-bound identifier.
#[test]
fn diff_reserved_all_words_as_let() {
    for word in [
        "break",
        "checked",
        "component",
        "constraint",
        "continue",
        "fori",
        "include",
        "mixin",
        "parallel",
        "params",
        "process",
        "protected",
        "pure",
        "sealed",
        "trait",
        "tailcall",
        "virtual",
    ] {
        assert_asts_match(&format!("let {word} = 10\n"));
    }
}

/// A reserved word in *expression* position (a bare reference).
#[test]
fn diff_reserved_expr_reference() {
    assert_asts_match("let x = checked\n");
}

/// A reserved word as a *function name* with a parameter.
#[test]
fn diff_reserved_function_name() {
    assert_asts_match("let sealed x = x + 1\n");
}

/// A reserved word as a *parameter* name.
#[test]
fn diff_reserved_parameter_name() {
    assert_asts_match("let f pure = pure + 1\n");
}

/// A reserved word as a record *field* label.
#[test]
fn diff_reserved_record_field() {
    assert_asts_match("type T = { mutable process : int }\n");
}

/// A reserved word as a *namespace* segment
/// (`Diagnostics/General/W_Keyword_tailcall01.fs`).
#[test]
fn diff_reserved_namespace_segment() {
    assert_asts_match("namespace tailcall\n\nmodule M =\n    let x = 1\n");
}

/// A reserved word as a *module* name.
#[test]
fn diff_reserved_module_name() {
    assert_asts_match("module virtual\n\nlet x = 1\n");
}

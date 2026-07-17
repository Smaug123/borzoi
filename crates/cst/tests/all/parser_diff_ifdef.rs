//! Differential test (`parser::parse` vs FCS): conditional compilation —
//! `#if`/`#else`/`#elif`/`#endif` branch selection (with and without defined
//! symbols) and `#nowarn`/`#line` trivia. Split out of the former monolithic
//! `parser_diff.rs`.

use crate::common::{
    assert_asts_match, assert_asts_match_allow_errors, assert_asts_match_with_defines,
};

// ---- conditional compilation (stage C3, docs/completed/parser-ifdef-plan.md) ---------
//
// `fcs-dump ast` parses with no `--define` flags, so every `#if <ident>` is
// false and the active branch is the `#else` / post-`#endif` code. Our parser
// uses an empty symbol set (`parse`), so it selects the same branch — the two
// agree by construction. The directive lines and dead branches are trivia, so
// the projected AST is exactly that of the active branch.

/// `#if FOO … #else … #endif` with `FOO` undefined selects the `#else` arm.
#[test]
fn diff_ast_ifdef_else_selected() {
    assert_asts_match("#if FOO\nlet x = 1\n#else\nlet y = 2\n#endif\n");
}

/// `#if FOO … #endif` (no `#else`): the dead then-branch contributes nothing;
/// only the code after `#endif` is active.
#[test]
fn diff_ast_ifdef_no_else_then_dead() {
    assert_asts_match("#if FOO\nlet x = 1\n#endif\nlet z = 3\n");
}

/// `#elif` chain, all conditions false → the `#else` arm is active.
#[test]
fn diff_ast_ifdef_elif_chain_falls_to_else() {
    assert_asts_match("#if FOO\nlet x = 1\n#elif BAR\nlet y = 2\n#else\nlet z = 3\n#endif\n");
}

/// Nested `#if` inside a dead branch is wholly inactive; the outer `#else`
/// is active.
#[test]
fn diff_ast_ifdef_nested_in_dead_branch() {
    assert_asts_match("#if FOO\n#if BAR\nlet x = 1\n#endif\n#else\nlet y = 2\n#endif\n");
}

/// A directive splitting an expression: the binding RHS comes from the active
/// `#else` arm (`2`), with the `#if`/dead-`1`/`#else` lines as trivia. Exercises
/// the offside layer across the inactive gap.
#[test]
fn diff_ast_ifdef_inside_let_rhs() {
    assert_asts_match("let x =\n#if FOO\n    1\n#else\n    2\n#endif\n");
}

/// `#nowarn` in active code is a trivia directive (FCS's `WARN_DIRECTIVE`
/// hidden token), not an AST node — the projected AST is just the `let`.
#[test]
fn diff_ast_nowarn_is_trivia() {
    assert_asts_match("#nowarn \"40\"\nlet x = 1\n");
}

/// `#warnon` is the symmetric warning-scope trivia directive.
#[test]
fn diff_ast_warnon_is_trivia() {
    assert_asts_match("#warnon \"40\"\nlet x = 1\n");
}

/// `#line` in active code is a trivia directive (FCS's `HASH_LINE`), not an
/// AST node.
#[test]
fn diff_ast_line_directive_is_trivia() {
    assert_asts_match("#line 1 \"orig.fs\"\nlet x = 1\n");
}

/// A malformed `#if` condition — `*COMPILED*` is not a valid preprocessor
/// expression — is an error on *both* sides (FCS emits FS3182/FS3184; we emit
/// a directive-condition parse error). FCS treats the bad condition as false,
/// so the dead `#if` arm contributes nothing and only `exit 1` is active; the
/// projected ASTs must still match. Mirrors FCS's
/// `Conformance/LexicalAnalysis/ConditionalCompilation/E_MustBeIdent01.fs`.
#[test]
fn diff_ast_ifdef_malformed_condition_is_error() {
    assert_asts_match_allow_errors("#if *COMPILED*\nexit 0\n#endif\nexit 1\n");
}

// ---- conditional compilation with defined symbols (stage C4) ---------------
//
// `fcs-dump ast <file> SYM…` defines each `SYM`, and our parser parses with
// the same `{SYM…}`, so `#if SYM` selects the *then* branch on both sides.
// The symmetric counterpart to the undefined-symbol fixtures above.

/// `#if FOO … #else … #endif` with `FOO` defined selects the *then* arm.
#[test]
fn diff_ast_ifdef_then_selected_when_defined() {
    assert_asts_match_with_defines("#if FOO\nlet x = 1\n#else\nlet y = 2\n#endif\n", &["FOO"]);
}

/// `#if FOO … #endif` (no `#else`) with `FOO` defined: the then-branch is
/// active, plus the code after `#endif`.
#[test]
fn diff_ast_ifdef_then_active_no_else() {
    assert_asts_match_with_defines("#if FOO\nlet x = 1\n#endif\nlet z = 3\n", &["FOO"]);
}

// No defined-symbol `#elif` fixture: FCS's `fcs-dump ast` parses with its
// default editing language version, which does not enable
// `LanguageFeature.PreprocessorElif` — it rejects `#elif` as an unknown
// directive and treats every `#elif` arm as inactive (verified: `#if FOO
// (false) / #elif BAR (BAR defined) / #endif` selects *neither* branch under
// FCS). Our parser implements modern-F# `#elif`, so a defined-symbol elif
// fixture would diverge on the dump config, not on parser correctness.
// Enabling the feature in the dump would mean threading a language version
// through every `ast` dump and re-validating all the non-directive fixtures —
// disproportionate for this optional stage. The shared falls-through-to-`#else`
// behaviour is already pinned by `diff_ast_ifdef_elif_chain_falls_to_else`.

/// Nested `#if`, both symbols defined → the innermost then-branch is active.
#[test]
fn diff_ast_ifdef_nested_both_defined() {
    assert_asts_match_with_defines(
        "#if FOO\n#if BAR\nlet x = 1\n#endif\n#endif\nlet z = 3\n",
        &["FOO", "BAR"],
    );
}

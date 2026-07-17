//! Differential test (`parser::parse` vs FCS): explicit value-typar declarations
//! on a *dotted* pattern long-identifier head — `A.B.Case<'T> y`.
//!
//! FCS's `constrPattern: atomicPatternLongIdent explicitValTyparDecls
//! atomicPatsOrNamePatPairs` (`pars.fsy:3693`) takes the typars after the *whole*
//! `pathOp`, so they attach to a qualified head exactly as they do to a bare one
//! (`Case<'T> y`, already supported). They land in `SynPat.LongIdent`'s
//! `typars: SynValTyparDecls option` slot, between the head path and the args.
//!
//! Our parser previously recognised the typar decls only after a *single*-ident
//! head (the promotion gate keyed on the first ident's end), so every dotted form
//! here errored at the `<`. The `global.`-rooted head shares the same machinery
//! and so shared the same gap.

use crate::common::assert_asts_match;

/// The motivating shape: a qualified case pattern carrying explicit typars and a
/// curried argument.
#[test]
fn diff_dotted_head_typars_with_arg() {
    assert_asts_match("match x with\n| A.B.Case<'T> y -> 1\n| _ -> 0\n");
}

/// Typars with *no* args: FCS's first `constrPattern` alternative
/// (`atomicPatternLongIdent explicitValTyparDecls`) — a `SynPat.LongIdent` with
/// `SynArgPats.Pats []`, not a `Named`.
#[test]
fn diff_dotted_head_typars_nullary() {
    assert_asts_match("match x with\n| A.B.Case<'T> -> 1\n| _ -> 0\n");
}

/// Several typars on a longer path.
#[test]
fn diff_dotted_head_multiple_typars() {
    assert_asts_match("match x with\n| A.B.C.Case<'T, 'U> y -> 1\n| _ -> 0\n");
}

/// Typars followed by the *named-field* argument group (`SynArgPats.NamePatPairs`)
/// rather than the curried list — the third `constrPattern` alternative.
#[test]
fn diff_dotted_head_typars_name_pat_pairs() {
    assert_asts_match("match x with\n| A.Case<'T>(f = y) -> 1\n| _ -> 0\n");
}

/// A dotted head with typars as a *paren* pattern element — the same
/// `constrPattern` reduction reached through `parenPattern`.
#[test]
fn diff_dotted_head_typars_in_paren() {
    assert_asts_match("let f (A.B.Case<'T> y) = 1\n");
}

/// A `global.`-rooted head with typars: the rooted path routes through the same
/// long-ident machinery, so it inherits the fix.
#[test]
fn diff_global_rooted_head_typars() {
    assert_asts_match("match x with\n| global.A.Case<'T> y -> 1\n| _ -> 0\n");
}

/// The single-ident head keeps working (regression guard for the branch-selection
/// gate, which must still force `LongIdent` over `Named` for a bare `Case<'T>`).
#[test]
fn diff_single_head_typars_nullary() {
    assert_asts_match("match x with\n| Case<'T> -> 1\n| _ -> 0\n");
}

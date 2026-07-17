//! Differential test (`parser::parse` vs FCS): a pattern long-identifier path
//! whose *final* segment is an `opName` — a parenthesised operator (`A.B.(+)`,
//! the spaced `A.B.( * )`, the glued `A.B.(*)`) or an active-pattern name
//! (`A.B.(|Foo|_|)`).
//!
//! FCS's pattern path is `pathOp: ident | opName | ident DOT pathOp`
//! (`pars.fsy:6930`), so every `atomicPatternLongIdent` — a `match`-clause head, a
//! `let` binding head, a curried atomic argument, a `global.`-rooted path — may
//! end in an `opName`, yielding a `SynPat.LongIdent` whose last `SynLongIdent`
//! segment is the operator's mangled `idText` (`op_Addition`) or the active
//! pattern's folded name (`|Foo|_|`). Only the *last* segment may be an `opName`;
//! the intermediate ones stay plain idents.
//!
//! Our parser's `sweep_long_ident_dot_continuation` accepted only ident segments
//! after a `.`, so every form here was a "trailing dot in long identifier path"
//! error. The *member* head (`member x.(+)`, `member A.B.(|Foo|Bar|)`) already
//! parsed this shape; this slice shares that machinery with the pattern heads.

use crate::common::assert_asts_match;

/// A qualified operator name applied in a `match` clause.
#[test]
fn diff_dotted_operator_name_in_clause() {
    assert_asts_match("match x with\n| A.B.(+) y -> 1\n| _ -> 0\n");
}

/// A qualified *active-pattern* name — the shape the last review round flagged.
#[test]
fn diff_dotted_active_pat_name_in_clause() {
    assert_asts_match("match x with\n| A.B.(|Foo|_|) y -> 1\n| _ -> 0\n");
}

/// A total active pattern, nullary (no args): FCS's bare `atomicPatternLongIdent`
/// reduction — still a `SynPat.LongIdent`, since the path is multi-segment.
#[test]
fn diff_dotted_active_pat_name_nullary() {
    assert_asts_match("match x with\n| A.(|Foo|Bar|) -> 1\n| _ -> 0\n");
}

/// The spaced multiply name (`( * )`) as the final segment.
#[test]
fn diff_dotted_spaced_star_name() {
    assert_asts_match("match x with\n| A.B.( * ) y -> 1\n| _ -> 0\n");
}

/// The glued `(*)` multiply token (the lexer fuses it, or it would open a block
/// comment) as the final segment.
#[test]
fn diff_dotted_glued_star_name() {
    assert_asts_match("match x with\n| A.B.(*) y -> 1\n| _ -> 0\n");
}

/// A three-segment path before the `opName` — the intermediate `. ident` segments
/// are swept as usual.
#[test]
fn diff_multi_segment_path_to_operator_name() {
    assert_asts_match("match x with\n| A.B.C.(+) y -> 1\n| _ -> 0\n");
}

/// A `global.`-rooted path ending in an active-pattern name — the rooted head and
/// the `opName` tail composing, each of which the other slice added.
#[test]
fn diff_global_rooted_path_to_active_pat_name() {
    assert_asts_match("match x with\n| global.A.(|Foo|_|) y -> 1\n| _ -> 0\n");
}

/// A `let` *binding* head whose path ends in an operator name (`let A.B.(+) x y =
/// …`) — the same `pathOp` reduction reached from `headBindingPattern`.
#[test]
fn diff_binding_head_dotted_operator_name() {
    assert_asts_match("let A.B.(+) x y = x\n");
}

/// A curried *atomic argument* whose path ends in an operator name — FCS's
/// `atomicPattern: atomicPatternLongIdent`, which is nullary: the `(+)` path must
/// not swallow the following arg.
#[test]
fn diff_atomic_arg_dotted_operator_name() {
    assert_asts_match("let f A.B.(+) y = 1\n");
}

/// The same, with typars after the path — the `opName` tail composing with the
/// `constrPattern` typar slot.
#[test]
fn diff_dotted_active_pat_name_with_typars() {
    assert_asts_match("match x with\n| A.(|Foo|_|)<'T> y -> 1\n| _ -> 0\n");
}

/// A qualified operator name as a paren-pattern element.
#[test]
fn diff_dotted_operator_name_in_paren() {
    assert_asts_match("let f (A.B.(+) y) = 1\n");
}

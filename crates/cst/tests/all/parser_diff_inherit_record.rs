//! Differential test (`parser::parse` vs FCS): inheriting record expressions
//! `{ inherit Base(args); F = e }` — FCS's `recdExpr` first alternative
//! (`INHERIT atomType opt_HIGH_PRECEDENCE_APP opt_atomicExprAfterType
//! recdExprBindings`, `pars.fsy:5680`), which yields a `SynExpr.Record` whose
//! `baseInfo` carries the base type and the constructor-args expression (FCS
//! synthesises `Const(Unit)` for a bare `inherit Base` / `inherit Base()`). The
//! normaliser models the `baseInfo` (base type + args); the per-field trivia is
//! elided, so these pin the base-construction shape and the field list.

use crate::common::assert_asts_match;

/// The canonical inheriting record: a base ctor call plus one field.
#[test]
fn diff_ast_inherit_record_arg_and_field() {
    assert_asts_match("let x = { inherit B(); X = 1 }\n");
}

/// A bare `inherit Base` with no parens — FCS still synthesises `Const(Unit)`
/// args.
#[test]
fn diff_ast_inherit_record_bare_base() {
    assert_asts_match("let x = { inherit B }\n");
}

/// `inherit Base()` with an explicit unit arg and no fields.
#[test]
fn diff_ast_inherit_record_unit_no_fields() {
    assert_asts_match("let x = { inherit B() }\n");
}

/// Constructor args (`inherit Base(1, 2)`) — a `Paren(Tuple)` base-args expr.
#[test]
fn diff_ast_inherit_record_tuple_args() {
    assert_asts_match("let x = { inherit B(1, 2); X = 1 }\n");
}

/// A dotted base type (`inherit System.Object()`).
#[test]
fn diff_ast_inherit_record_dotted_base() {
    assert_asts_match("let x = { inherit System.Object(); someField = \"abc\" }\n");
}

/// Two trailing fields after the inherit clause.
#[test]
fn diff_ast_inherit_record_two_fields() {
    assert_asts_match("let x = { inherit B(); X = 1; Y = 2 }\n");
}

/// The multi-line corpus shape (`SyntaxTree/Expression/InheritRecord - Field 1.fs`):
/// a multi-line base-ctor argument, then offside-separated fields.
#[test]
fn diff_ast_inherit_record_multiline() {
    assert_asts_match(
        "let x =\n  { inherit Exception(\"a \" + \"b\")\n    X = 42\n    Y = \"t\" }\n",
    );
}

/// An inheriting record as a constructor body (the `members_basics.fs` shape):
/// `new(s) = { inherit System.Object(); someField = "abc" }`.
#[test]
fn diff_ast_inherit_record_ctor_body() {
    assert_asts_match(
        "type C =\n  val f : string\n  new(s) = { inherit System.Object(); f = s }\n",
    );
}

/// FCS's `atomicExprAfterType` consumes only the argument *atom*, so a trailing
/// postfix on the base construction (`{ inherit B().M }`) is an FCS parse error.
/// We must reject it too rather than folding `.M` into the base args. Pins that it
/// errors and stays lossless.
#[test]
fn inherit_record_postfix_arg_rejects() {
    use borzoi_cst::parser::parse;
    let src = "let x = { inherit B().M; X = 1 }\n";
    let p = parse(src);
    assert!(
        !p.errors.is_empty(),
        "a postfix on the inherit base args must error (FCS does)"
    );
    assert_eq!(p.root.text().to_string(), src, "lossless");
}

//! Differential test (`parser::parse` vs FCS): unit-of-measure annotated
//! numeric literals in expression position — FCS's `rawConstant
//! HIGH_PRECEDENCE_TYAPP measureTypeArg` (`pars.fsy:3521`), projecting to
//! `SynConst.Measure(constant, range, SynMeasure, trivia)`.
//!
//! The measure grammar (`measureTypeExpr`, `pars.fsy:6693-6760`) is the closed
//! recursive grammar: `Named` (a `path`), `Var` (a typar), `Paren`, `One`
//! (`<1>`), `Anon` (`<_>`), `Power` (`^` rational exponent), juxtaposition
//! `Seq`, `Product` (`*`), and `Divide` (`/`, incl. the no-numerator reciprocal
//! `</s>`). Every `measureTypeExpr` is wrapped in a `Seq` by FCS, so even a
//! single named measure `<m>` is `Seq[Named ["m"]]`.

use crate::common::assert_asts_match;

/// The corpus case (`WoofWare.Myriad2`): a `float` literal with a single named
/// measure — `Const(Measure(Double, Seq[Named ["measure"]]))`.
#[test]
fn diff_ast_measure_float_named() {
    assert_asts_match("let x = 1.0<measure>\n");
}

/// An `int` literal carrying a measure — `Const(Measure(Int32, Seq[Named ["kg"]]))`.
#[test]
fn diff_ast_measure_int_named() {
    assert_asts_match("let x = 5<kg>\n");
}

/// A dotted measure name — FCS's `path`, a multi-segment `SynLongIdent`.
#[test]
fn diff_ast_measure_dotted_named() {
    assert_asts_match("let x = 1.0<SI.metre>\n");
}

/// The dimensionless `1` measure — `SynMeasure.One` (the only integer FCS
/// admits at the `measureTypePower` level).
#[test]
fn diff_ast_measure_one() {
    assert_asts_match("let x = 1.0<1>\n");
}

/// The anonymous measure `<_>` — `SynMeasure.Anon`, reached through the
/// dedicated `measureTypeArg: LESS UNDERSCORE GREATER` arm (not wrapped in a
/// `Seq`).
#[test]
fn diff_ast_measure_anon() {
    assert_asts_match("let x = 3.0<_>\n");
}

/// A measure variable `<'u>` — `SynMeasure.Var`.
#[test]
fn diff_ast_measure_var() {
    assert_asts_match("let x = 1.0<'u>\n");
}

/// Juxtaposition `<m s>` — `SynMeasure.Seq[Named, Named]`.
#[test]
fn diff_ast_measure_seq() {
    assert_asts_match("let x = 2.0<m s>\n");
}

/// Product `<m * s>` — `SynMeasure.Product`.
#[test]
fn diff_ast_measure_product() {
    assert_asts_match("let x = 2.0<m * s>\n");
}

/// Division `<m / s>` — `SynMeasure.Divide(Some _, _)`.
#[test]
fn diff_ast_measure_divide() {
    assert_asts_match("let x = 2.0<m / s>\n");
}

/// Left-associative division chain `<m / s / s>` —
/// `Divide(Divide(Seq[m], Seq[s]), Seq[s])`.
#[test]
fn diff_ast_measure_divide_chain() {
    assert_asts_match("let x = 2.0<m / s / s>\n");
}

/// The no-numerator reciprocal `</s>` — `SynMeasure.Divide(None, Seq[s])`.
#[test]
fn diff_ast_measure_reciprocal() {
    assert_asts_match("let x = 2.0</s>\n");
}

/// Power `<m ^ 2>` — `SynMeasure.Power(Named, SynRationalConst.Integer 2)`.
#[test]
fn diff_ast_measure_power() {
    assert_asts_match("let x = 9.8<m ^ 2>\n");
}

/// Adjacent power `<m^2>` — the `^` need not be spaced.
#[test]
fn diff_ast_measure_power_adjacent() {
    assert_asts_match("let x = 9.8<m^2>\n");
}

/// Negative power `<m^-2>` — `Power(Named, Negate(Integer 2))`.
#[test]
fn diff_ast_measure_power_negative() {
    assert_asts_match("let x = 9.8<m^-2>\n");
}

/// Rational power `<m^(1/2)>` — `Power(Named, Paren(Rational 1/2))`.
#[test]
fn diff_ast_measure_power_rational() {
    assert_asts_match("let x = 1.0<m^(1/2)>\n");
}

/// Parenthesised measure `<(m s)>` — `Seq[Paren(Seq[Named, Named])]`.
#[test]
fn diff_ast_measure_paren() {
    assert_asts_match("let x = 1.0<(m s)>\n");
}

/// The measure literal as an operand of an arithmetic expression — the shape
/// that broke `TestJsonSerde.fs` (`s / 1.0<measure>`).
#[test]
fn diff_ast_measure_in_arithmetic() {
    assert_asts_match("let y = z / 1.0<measure>\n");
}

/// The measure literal as a record-field value — the shape that broke
/// `PureGymDtos.fs` (`Latitude = 1.0<measure>`).
#[test]
fn diff_ast_measure_in_record_field() {
    assert_asts_match("let r = { Latitude = 1.0<measure> }\n");
}

/// The dimensionless `1` written as a hex literal — FCS decodes the `INT32`
/// value and admits `0x1` as `One` with no error.
#[test]
fn diff_ast_measure_one_hex() {
    assert_asts_match("let x = 1.0<0x1>\n");
}

/// The dimensionless `1` with an `l` Int32 suffix — `1l` decodes to `1`, so
/// FCS admits it as `One`.
#[test]
fn diff_ast_measure_one_suffixed() {
    assert_asts_match("let x = 1.0<1l>\n");
}

/// A head-type measure variable `<^u>` — FCS's `measureTypeAtom: typar` via the
/// `INFIX_AT_HAT_OP ident` form, `SynMeasure.Var(SynTypar(_, HeadType, _))`.
#[test]
fn diff_ast_measure_var_head_type() {
    assert_asts_match("let x = 1.0<^u>\n");
}

/// A reciprocal on the right of a product — `<m * /s>` →
/// `Product(Seq[m], Divide(None, Seq[s]))`. The `*`/`/` RHS is a full
/// `measureTypeExpr`, so it can lead with the no-numerator `/`.
#[test]
fn diff_ast_measure_product_reciprocal_rhs() {
    assert_asts_match("let x = 1.0<m * /s>\n");
}

/// A reciprocal on the right of a division — `<m / /s>` →
/// `Divide(Seq[m], Divide(None, Seq[s]))`.
#[test]
fn diff_ast_measure_divide_reciprocal_rhs() {
    assert_asts_match("let x = 1.0<m / /s>\n");
}

/// A measure literal followed by a member access — `1.0<m>.ToString()`. The
/// `>.` fuses in the raw stream but LexFilter splits the filtered close `>`, so
/// the named measure closes and the `.ToString()` chains onto the whole literal.
#[test]
fn diff_ast_measure_named_then_dot() {
    assert_asts_match("let z = 1.0<m>.ToString()\n");
}

/// An *anonymous* measure literal before the same fused-`>` tail —
/// `1.0<_>.ToString()`. The `<_>` close must be detected on the filtered
/// stream (the raw stream fuses `>.` into one `Op(">.")` token).
#[test]
fn diff_ast_measure_anon_then_dot() {
    assert_asts_match("let z = 1.0<_>.ToString()\n");
}

/// A `global`-rooted measure path — `1.0<global.SI.m>`. FCS spells the
/// `GLOBAL` head as the mangled `` `global` `` idText; the differential
/// normaliser must strip that to line up with our bare `global`.
#[test]
fn diff_ast_measure_global_path() {
    assert_asts_match("let x = 1.0<global.SI.m>\n");
}

/// A reciprocal whose denominator is itself a reciprocal — `</ /s>` →
/// `Divide(None, Divide(None, Seq[s]))`. FCS's `INFIX_STAR_DIV_MOD_OP
/// measureTypeExpr` makes the reciprocal body a full `measureTypeExpr`, so it
/// can lead with another `/`.
#[test]
fn diff_ast_measure_nested_reciprocal() {
    assert_asts_match("let x = 1.0</ /s>\n");
}

/// A nested reciprocal as a product's right operand — `<m * / /s>` →
/// `Product(Seq[m], Divide(None, Divide(None, Seq[s])))`.
#[test]
fn diff_ast_measure_product_nested_reciprocal() {
    assert_asts_match("let x = 1.0<m * / /s>\n");
}

/// A reciprocal followed by a product — `</s * m>` →
/// `Product(Divide(None, Seq[s]), Seq[m])`. Pins that the reciprocal operand
/// consumes only its own denominator (the `* m` binds at the outer product
/// level), so the nested-reciprocal recursion stays bounded to leading `/`s.
#[test]
fn diff_ast_measure_reciprocal_then_product() {
    assert_asts_match("let x = 1.0</s * m>\n");
}

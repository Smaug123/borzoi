//! Units of measure — `MeasurePower` (`m^2`) + `SynRationalConst`
//! (phase 10.8). Reached through the prefix-app `float<…>` type-argument
//! surface; the measure *product* (`kg m`) reuses the phase-7 postfix app.

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Helper: find the sole `MEASURE_POWER_TYPE` node in a parse.
fn measure_power_node(parse: &crate::parser::Parse) -> SyntaxNode {
    parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::MEASURE_POWER_TYPE)
        .expect("MEASURE_POWER_TYPE present")
}

/// Phase 10.8 — `m^2`: the minimal measure power. Pins
/// `MEASURE_POWER_TYPE > [LONG_IDENT_TYPE(m), MEASURE_POWER_OP_TOK("^"),
/// RATIONAL_CONST_INTEGER > [INT32_LIT("2")]]` and the facade accessors
/// (`base` is a `LongIdent`, the exponent an `Integer`, not negated).
#[test]
fn measure_power_int_green_shape() {
    use crate::syntax::{AstNode, MeasurePowerType, RationalConst, Type};
    let source = "(x : float<m^2>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let mp = MeasurePowerType::cast(measure_power_node(&parse)).expect("casts");
    match mp.base().expect("base present") {
        Type::LongIdent(_) => {}
        other => panic!("base must be LongIdent(m); got {other:?}"),
    }
    assert!(!mp.is_negated(), "`^` is not the negated spelling");
    match mp.exponent().expect("exponent present") {
        RationalConst::Integer(i) => {
            assert_eq!(i.value_token().expect("value token").text(), "2");
        }
        other => panic!("exponent must be Integer(2); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 10.8 — `m^-1`: the `^-` operator spelling. The operator token
/// carries the trailing minus (`MEASURE_POWER_OP_TOK` text `"^-"`), and the
/// `Negate` is *not* a green node — it is recovered from `is_negated()`. The
/// exponent node itself is a plain `Integer(1)`.
#[test]
fn measure_power_negate_operator() {
    use crate::syntax::{AstNode, MeasurePowerType, RationalConst};
    let source = "(x : float<m^-1>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let mp = MeasurePowerType::cast(measure_power_node(&parse)).expect("casts");
    assert!(mp.is_negated(), "`^-` is the negated spelling");
    let op = mp
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::MEASURE_POWER_OP_TOK)
        .expect("op token present");
    assert_eq!(op.text(), "^-");
    match mp.exponent().expect("exponent") {
        RationalConst::Integer(i) => {
            assert_eq!(i.value_token().expect("value").text(), "1");
        }
        other => panic!("exponent node must be Integer(1) (Negate is the operator); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 10.8 — `m^(1/2)`: a parenthesised rational exponent. Pins
/// `RATIONAL_CONST_PAREN > [LPAREN_TOK, RATIONAL_CONST_RATIONAL >
/// [INT32_LIT("1"), SLASH_TOK, INT32_LIT("2")], RPAREN_TOK]` and the
/// `numerator`/`denominator` accessors.
#[test]
fn measure_power_paren_rational_green_shape() {
    use crate::syntax::{AstNode, MeasurePowerType, RationalConst};
    let source = "(x : float<m^(1/2)>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let mp = MeasurePowerType::cast(measure_power_node(&parse)).expect("casts");
    let RationalConst::Paren(paren) = mp.exponent().expect("exponent") else {
        panic!(
            "exponent must be Paren; got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    let RationalConst::Rational(r) = paren.inner().expect("paren inner") else {
        panic!("paren inner must be Rational");
    };
    assert_eq!(r.numerator().expect("numerator").text(), "1");
    assert_eq!(r.denominator().expect("denominator").text(), "2");
    assert_lossless(source, &parse);
}

/// Phase 10.8 — `m^(- 2)`: a space-separated `-` inside the parens is a
/// real `MINUS` (not sign-folded), so the exponent is
/// `Paren(Negate(Integer 2))` — a `RATIONAL_CONST_NEGATE` node wrapping a
/// `RATIONAL_CONST_INTEGER`, distinct from the operator-driven negate.
#[test]
fn measure_power_paren_spaced_negate_green_shape() {
    use crate::syntax::{AstNode, MeasurePowerType, RationalConst};
    let source = "(x : float<m^(- 2)>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let mp = MeasurePowerType::cast(measure_power_node(&parse)).expect("casts");
    assert!(!mp.is_negated(), "operator is a bare `^`");
    let RationalConst::Paren(paren) = mp.exponent().expect("exponent") else {
        panic!("exponent must be Paren");
    };
    let RationalConst::Negate(neg) = paren.inner().expect("paren inner") else {
        panic!(
            "paren inner must be Negate; got tree:\n{}",
            debug_tree(&parse.root)
        );
    };
    match neg.inner().expect("negate inner") {
        RationalConst::Integer(i) => assert_eq!(i.value_token().expect("value").text(), "2"),
        other => panic!("negate inner must be Integer(2); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 10.8 — `'a^2`: the base of a measure power may be a typar
/// (`VAR_TYPE`), not only a `LongIdent`.
#[test]
fn measure_power_typar_base_green_shape() {
    use crate::syntax::{AstNode, MeasurePowerType, Type};
    let source = "(x : float<'a^2>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let mp = MeasurePowerType::cast(measure_power_node(&parse)).expect("casts");
    match mp.base().expect("base") {
        Type::Var(_) => {}
        other => panic!("base must be Var('a); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 10.8 — `(m)^2`: a parenthesised base. FCS's `powerType` base is an
/// `atomTypeOrAnonRecdType`, so the measure-power tail must be detected on the
/// head atom (not only inside `parse_app_type_con_power`, which sees only
/// path/typar heads). The `MEASURE_POWER_TYPE` base is the `PAREN_TYPE`.
#[test]
fn measure_power_paren_base_green_shape() {
    use crate::syntax::{AstNode, MeasurePowerType, Type};
    let source = "(x : float<(m)^2>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    let mp = MeasurePowerType::cast(measure_power_node(&parse)).expect("casts");
    match mp.base().expect("base") {
        Type::Paren(_) => {}
        other => panic!("base must be Paren(m); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// Phase 10.8 — `m^(1/0)`: a zero denominator. FCS reports a parse error
/// (`parsIllegalDenominatorForMeasureExponent`) but still builds the
/// `Rational(1, 0)` node; we mirror both — a non-empty error list *and* the
/// `RATIONAL_CONST_RATIONAL` shape.
#[test]
fn measure_power_zero_denominator_is_error() {
    use crate::syntax::{AstNode, MeasurePowerType, RationalConst};
    let source = "(x : float<m^(1/0)>)\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a zero denominator must record a parse error"
    );
    let mp = MeasurePowerType::cast(measure_power_node(&parse)).expect("casts");
    let RationalConst::Paren(paren) = mp.exponent().expect("exponent") else {
        panic!("exponent must be Paren");
    };
    let RationalConst::Rational(r) = paren.inner().expect("paren inner") else {
        panic!("paren inner must be Rational(1, 0)");
    };
    assert_eq!(r.numerator().expect("numerator").text(), "1");
    assert_eq!(r.denominator().expect("denominator").text(), "0");
    assert_lossless(source, &parse);
}

/// Phase 10.8 recovery — `(x : m^) -1`: a *missing* exponent before a
/// LexFilter-swallowed `)`. The exponent dispatch is raw-gated, so it must
/// **not** cross the `)` to grab the outer `-1`: the `MEASURE_POWER_TYPE`
/// captures only `m^` (no stolen `)` / `-1`), and a parse error is recorded.
/// (`assert_lossless` alone can't catch the theft — an ERROR-drained `)`
/// still round-trips — so the node text is the discriminating check.)
#[test]
fn measure_power_missing_exponent_does_not_steal_outer_token() {
    use crate::syntax::{AstNode, MeasurePowerType};
    let source = "let z = (x : m^) -1\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a missing exponent must record a parse error"
    );
    let mp = MeasurePowerType::cast(measure_power_node(&parse)).expect("casts");
    assert_eq!(
        mp.syntax().text().to_string(),
        "m^",
        "the measure power must capture only `m^`, not the swallowed `)` or outer `-1`; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 10.8 recovery — `(x : m^()) 1`: an *empty* exponent paren. The
/// numerator dispatch is raw-gated, so the swallowed `)` of `()` is consumed
/// as the paren closer and the outer `1` is **not** grabbed as the numerator:
/// the `MEASURE_POWER_TYPE` captures `m^()`, with a parse error recorded.
#[test]
fn measure_power_empty_exponent_paren_does_not_steal_outer_token() {
    use crate::syntax::{AstNode, MeasurePowerType};
    let source = "let z = (x : m^()) 1\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "an empty exponent paren must record a parse error"
    );
    let mp = MeasurePowerType::cast(measure_power_node(&parse)).expect("casts");
    assert_eq!(
        mp.syntax().text().to_string(),
        "m^()",
        "the measure power must capture only `m^()`, not the outer `1`; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 10.8 — `(x : m^u)`: a `^` followed by an *identifier* is a measure
/// power expecting an integer exponent, **not** an SRTP-typar postfix
/// application `App(^u, [m])`. FCS's LR parser grabs the `^` as the measure
/// operator (`INFIX_AT_HAT_OP`) and then reports "Expected integer"; we match
/// that verdict — `m^` becomes a `MEASURE_POWER_TYPE` with no exponent and a
/// recorded error (clean, lossless recovery — no token theft), rather than
/// silently accepting `m^u`. (Ground-truthed: `dotnet fcs-dump ast` reports
/// `ParseHadErrors` for `m^u` in both bare and `<…>` positions.)
#[test]
fn measure_power_caret_then_ident_is_error_not_typar_app() {
    let source = "(x : m^u)\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "`m^u` must record a parse error (FCS rejects it), not parse cleanly"
    );
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::MEASURE_POWER_TYPE),
        "the `^` is claimed as the measure operator; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 10.8 — `kg m^2`: a measure *product* over a power. The postfix
/// loop wraps the whole thing as `App(MeasurePower(m, 2), [kg], postfix)`,
/// so the `APP_TYPE` head is the `MeasurePowerType` and the (single) arg is
/// `LongIdent(kg)`.
#[test]
fn measure_product_over_power_green_shape() {
    use crate::syntax::{AppType, AstNode, Type};
    let source = "(x : float<kg m^2>)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "got errors: {:?}", parse.errors);
    // The outer App is `float<…>`; the *inner* App is the `kg m^2` product.
    let inner_app = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::APP_TYPE)
        .find(|n| {
            AppType::cast(n.clone())
                .and_then(|a| a.type_name())
                .is_some_and(|t| matches!(t, Type::MeasurePower(_)))
        })
        .expect("an APP_TYPE whose head is a MeasurePower");
    let app = AppType::cast(inner_app).expect("casts");
    assert!(app.is_postfix(), "product is the postfix app form");
    let args = app.type_args();
    assert_eq!(args.len(), 1, "product has one factor as the arg (kg)");
    match &args[0] {
        Type::LongIdent(_) => {}
        other => panic!("product arg must be LongIdent(kg); got {other:?}"),
    }
    assert_lossless(source, &parse);
}

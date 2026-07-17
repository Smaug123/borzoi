//! Type-variable / typar types (`'a`, head-typar `^a`) inside annotations.
//!
//! Extracted verbatim from the former flat `types.rs` (one submodule per
//! `parse_type` grammar form).

use super::super::super::*; // parser internals under test
use super::super::*; // shared tree-rendering test helpers (see `tests/mod.rs`)

/// Phase 7.2 — `(x : 'a)` produces a [`SyntaxKind::VAR_TYPE`] under
/// the [`SyntaxKind::TYPED_EXPR`], with the children being a
/// [`SyntaxKind::QUOTE_TOK`] sigil followed by the typar `IDENT_TOK`.
/// Pins the green-tree shape and that the [`crate::syntax::VarType`]
/// facade's `is_head_type()` reads `false` for the quoted form.
#[test]
fn typed_paren_expr_with_quote_typar_shape() {
    let source = "(x : 'a)\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "expected no errors for `(x : 'a)`, got: {:?}",
        parse.errors,
    );
    let var_type = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::VAR_TYPE)
        .expect("VAR_TYPE under the TYPED_EXPR");
    let sigil = var_type
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| !t.kind().is_trivia())
        .expect("VAR_TYPE has a non-trivia leading token");
    assert_eq!(
        sigil.kind(),
        SyntaxKind::QUOTE_TOK,
        "expected QUOTE_TOK sigil for `'a`; got {:?}",
        sigil.kind(),
    );
    assert_eq!(sigil.text(), "'", "QUOTE_TOK text");
    let ident = var_type
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .expect("VAR_TYPE has an IDENT_TOK child");
    assert_eq!(ident.text(), "a", "typar ident text");
    use crate::syntax::AstNode;
    let typed_var =
        crate::syntax::VarType::cast(var_type).expect("VAR_TYPE casts to the typed facade");
    assert!(
        !typed_var.is_head_type(),
        "`'a` must project to TyparStaticReq::None (is_head_type = false)",
    );
    assert_lossless(source, &parse);
}

/// Phase 7.2 — `(x : ^T)` produces a [`SyntaxKind::VAR_TYPE`] whose
/// sigil child is a [`SyntaxKind::HAT_TOK`], routing through the
/// `Token::Op("^")` arm of `parse_atomic_type`. Mirrors the FCS
/// `INFIX_AT_HAT_OP` rule that gates on op-text equal to `"^"` for
/// `TyparStaticReq.HeadType`.
#[test]
fn typed_paren_expr_with_head_typar_shape() {
    let source = "(x : ^T)\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "expected no errors for `(x : ^T)`, got: {:?}",
        parse.errors,
    );
    let var_type = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::VAR_TYPE)
        .expect("VAR_TYPE under the TYPED_EXPR");
    let sigil = var_type
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| !t.kind().is_trivia())
        .expect("VAR_TYPE has a non-trivia leading token");
    assert_eq!(
        sigil.kind(),
        SyntaxKind::HAT_TOK,
        "expected HAT_TOK sigil for `^T`; got {:?}",
        sigil.kind(),
    );
    assert_eq!(sigil.text(), "^", "HAT_TOK text");
    let ident = var_type
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .expect("VAR_TYPE has an IDENT_TOK child");
    assert_eq!(ident.text(), "T", "typar ident text");
    use crate::syntax::AstNode;
    let typed_var =
        crate::syntax::VarType::cast(var_type).expect("VAR_TYPE casts to the typed facade");
    assert!(
        typed_var.is_head_type(),
        "`^T` must project to TyparStaticReq::HeadType (is_head_type = true)",
    );
    assert_lossless(source, &parse);
}

/// Phase 7.2 — `(x : '_)`: a `'` followed by something that is not
/// an identifier (here `_`, which lexes as a separate token in the
/// filtered stream) records a parser error and does not crash. Pins
/// the recovery path in `parse_var_type`'s ident-lookahead miss arm.
#[test]
fn typed_paren_expr_with_quote_then_non_ident_records_error() {
    let source = "(x : '_)\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "expected a parser error for `'` not followed by an ident; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.2 — `(x : ') y\n`: the `'` sigil sits just before the
/// type-annotation's closing `)`, which LexFilter swallows. The
/// filtered stream therefore lands on the outer `y`, which is
/// outside the surrounding `(…)`. Without a raw-stream boundary
/// check (codex round-1 P2 against phase 7.2), `parse_var_type`
/// would bump that outer ident as the typar name and drag the real
/// `)` in as `ERROR`, corrupting the outer parse. The fix is the
/// same shape as in `parse_type`: gate the post-sigil ident
/// lookahead on the next non-trivia *raw* token being an ident.
#[test]
fn quote_typar_ident_lookahead_does_not_cross_swallowed_rparen() {
    let source = "(x : ') y\n";
    let parse = parse(source);
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for `(x : ')`");
    let outer_has_rparen = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        outer_has_rparen,
        "outer PAREN_EXPR must keep its closing `)`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    // The outer `y` must NOT have been absorbed as a typar name.
    let stole_y_as_ident = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::VAR_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "y")
            })
    });
    assert!(
        !stole_y_as_ident,
        "outer `y` must not be absorbed as the typar name; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !parse.errors.is_empty(),
        "expected a parser error for missing ident after `'`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// Phase 7.2 — `(x : ^) y\n`: head-typar mirror of the
/// `(x : ') y` case. Routes through the `Token::Op("^")` arm of
/// `parse_atomic_type` so it exercises the same boundary fix from
/// the `^`-side.
#[test]
fn head_typar_ident_lookahead_does_not_cross_swallowed_rparen() {
    let source = "(x : ^) y\n";
    let parse = parse(source);
    let paren = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PAREN_EXPR)
        .expect("PAREN_EXPR for `(x : ^)`");
    let outer_has_rparen = paren
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::RPAREN_TOK && t.text() == ")");
    assert!(
        outer_has_rparen,
        "outer PAREN_EXPR must keep its closing `)`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    let stole_y_as_ident = parse.root.descendants().any(|n| {
        n.kind() == SyntaxKind::VAR_TYPE
            && n.descendants_with_tokens().any(|el| {
                el.into_token()
                    .is_some_and(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == "y")
            })
    });
    assert!(
        !stole_y_as_ident,
        "outer `y` must not be absorbed as the typar name; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !parse.errors.is_empty(),
        "expected a parser error for missing ident after `^`; got tree:\n{}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

//! Computation-expression binders: `let!`/`use!`/`and!`.

use borzoi_cst::lexer::{Token, lex};
use borzoi_cst::lexfilter::{FilteredToken, Virtual, filter};

use crate::common::assert_filtered_streams_match;

/// `let!` inside a computation expression. FCS converts `BINDER` → `OBINDER`
/// (`FSharpTokenKind.OffsideBinder`) when it starts a `CtxtLetDecl(blockLet=true)`,
/// reusing the exact same `CtxtLetDecl` + RHS-`SeqBlock` + offside-pop machinery
/// as plain `let` (LexFilter.fs:2166-2170). So the filtered shape is identical
/// to plain `let` except `OLET` → `OBINDER`.
#[test]
fn diff_filtered_let_bang() {
    assert_filtered_streams_match("let f = async {\n    let! x = e\n    return x\n}\n");
}

/// `let! … in …` explicit-`in` form. Still `OBINDER` (it is in a block), but
/// the `in` keeps the binding's RHS block from emitting a trailing
/// `OffsideBlockSep` before `return`.
#[test]
fn diff_filtered_let_bang_explicit_in() {
    assert_filtered_streams_match("let f = async {\n    let! x = e in return x\n}\n");
}

/// `use!` lexes to the same `BINDER` token as `let!` (lex.fsl:363), so it also
/// surfaces as `OffsideBinder` — the `let!`-vs-`use!` distinction is recovered
/// by the parser from the raw stream, exactly as `let`-vs-`use` is.
#[test]
fn diff_filtered_use_bang() {
    assert_filtered_streams_match("let f = async {\n    use! x = e\n    return x\n}\n");
}

/// `and!` applicative binder. FCS converts `AND_BANG` → `OAND_BANG`, which maps
/// to `FSharpTokenKind.None` (ServiceLexing.fs has no `OAND_BANG` arm) and is
/// dropped from the public token stream — like `OBLOCKEND`. So the differential
/// oracle can only confirm the *surrounding* structure: each bang binder opens
/// its own `CtxtLetDecl`, so the `and!` binding is separated from the `let!`
/// binding by an `OffsideBlockSep` and closes with its own `OffsideDeclEnd`.
/// The `Virtual::AndBang` token itself is pinned by `and_bang_emits_virtual`
/// below, since the diff harness cannot see it.
#[test]
fn diff_filtered_and_bang() {
    assert_filtered_streams_match(
        "let f = async {\n    let! x = a\n    and! y = b\n    return x\n}\n",
    );
}

/// Pins the `and!` token, which the differential harness drops (FCS surfaces
/// `OAND_BANG` as `FSharpTokenKind.None`). The real filtered stream the parser
/// consumes still carries `Virtual::AndBang`, immediately preceded by the
/// `OffsideBlockSep` that separates it from the `let!` binding and immediately
/// followed by the raw `and!`-binding pattern ident.
#[test]
fn and_bang_emits_virtual() {
    let src = "async {\n    let! x = a\n    and! y = b\n    return x\n}\n";
    let toks: Vec<FilteredToken<'_>> = filter(src, lex(src))
        .map(|(t, _)| t.expect("lex ok"))
        .collect();

    let binders = toks
        .iter()
        .filter(|t| matches!(t, FilteredToken::Virtual(Virtual::Binder)))
        .count();
    let and_bangs = toks
        .iter()
        .filter(|t| matches!(t, FilteredToken::Virtual(Virtual::AndBang)))
        .count();
    assert_eq!(
        binders, 1,
        "expected one Virtual::Binder for `let!`: {toks:?}"
    );
    assert_eq!(
        and_bangs, 1,
        "expected one Virtual::AndBang for `and!`: {toks:?}"
    );

    let i = toks
        .iter()
        .position(|t| matches!(t, FilteredToken::Virtual(Virtual::AndBang)))
        .expect("AndBang present");
    assert!(
        matches!(toks[i - 1], FilteredToken::Virtual(Virtual::BlockSep)),
        "AndBang should follow the inter-binding BlockSep: {toks:?}"
    );
    assert!(
        matches!(toks[i + 1], FilteredToken::Raw(Token::Ident("y"))),
        "AndBang should be followed by its binding's pattern ident: {toks:?}"
    );
}

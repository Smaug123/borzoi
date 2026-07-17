//! `fun` lambdas, comprehensions, and lambdas inside bracket pairs.

use crate::common::assert_filtered_streams_match;

/// `fun` lambda inside a `let` RHS. Exercises CtxtFun (pushed by FUN,
/// emits OFUN), the RARROW rule (pushes SeqBlock(OneSided) so the arrow
/// body gets a block without a leading OBLOCKBEGIN), and the EOF cascade:
/// SeqBlock(OneSided) emits ORIGHT_BLOCK_END, then CtxtFun emits OEND,
/// then the outer LetDecl SeqBlock + CtxtLetDecl close normally.
/// (LexFilter.fs:2532, 2304, 2055)
#[test]
fn diff_filtered_fun_lambda() {
    assert_filtered_streams_match("let f = fun x -> x\n");
}

/// A parenthesized lambda: `(fun x -> x)`. CtxtParen is pushed at `(`; when
/// `)` arrives it acts as `TokenRExprParen` (LexFilter.fs:194), force-closing
/// the inner SeqBlock(OneSided) + CtxtFun before balancing CtxtParen. The
/// virtual tokens therefore carry `)` 's span rather than the EOF span.
#[test]
fn diff_filtered_fun_in_parens() {
    assert_filtered_streams_match("let f = (fun x -> x)\n");
}

/// `fun` with a function-type-annotated parameter. Forces the RARROW rule
/// to skip the `->` inside the type annotation `(g: int -> int)` and only
/// fire on the real lambda arrow. CtxtParen pushed at `(` raises
/// `paren_depth`, which is above the level recorded in `Context::Fun { depth
/// }`, so the RARROW gate correctly skips the annotation arrow.
#[test]
fn diff_filtered_fun_annotated_param() {
    assert_filtered_streams_match("let f = fun (g: int -> int) -> g 1\n");
}

/// Lambda inside a list literal `[ fun x -> x ]`. Exercises the `[`/`]`
/// CtxtParen pair: `]` acts as `TokenRExprParen`, force-closing the inner
/// SeqBlock(OneSided) + CtxtFun before balancing CtxtParen. Unlike RPAREN,
/// RBRACK is emitted (not swallowed) in the filtered stream.
/// (LexFilter.fs:193, 2282)
#[test]
fn diff_filtered_fun_in_list() {
    assert_filtered_streams_match("let f = [ fun x -> x ]\n");
}

/// List comprehension `[ for x in xs -> x ]`. Exercises the RARROW push gate's
/// CtxtFor arm (LexFilter.fs:2308): with `CtxtFor` at the head, `->` opens a
/// `CtxtSeqBlock(AddOneSidedBlockEnd)` for the yield expression. `]` then
/// arrives as `TokenRExprParen` and force-closes the inner `SeqBlock(OneSided)`
/// (emitting `OffsideRightBlockEnd`), `CtxtFor` (silent), and the inner
/// `SeqBlock(NoAddBlockEnd)` from `[`, before balancing `CtxtParen(Opener::Brack)`.
#[test]
fn diff_filtered_for_comprehension_in_list() {
    assert_filtered_streams_match("let f xs = [ for x in xs -> x ]\n");
}

/// Array comprehension `[| for x in xs -> x |]`. Same RARROW/CtxtFor mechanism
/// as the list form; the difference is the `CtxtParen(Opener::BrackBar)` opener
/// and the `|]` closer (emitted, not swallowed).
#[test]
fn diff_filtered_for_comprehension_in_array() {
    assert_filtered_streams_match("let f xs = [| for x in xs -> x |]\n");
}

/// Lambda inside an array literal `[| fun x -> x |]`. Exercises the `[|`/`|]`
/// CtxtParen pair. `|]` is emitted in the filtered stream (not swallowed).
/// (LexFilter.fs:193, 2282)
#[test]
fn diff_filtered_fun_in_array() {
    assert_filtered_streams_match("let f = [| fun x -> x |]\n");
}

/// Lambda inside a record expression `{ F = fun x -> x }`. Exercises the
/// `{`/`}` CtxtParen pair: `}` force-closes SeqBlock(OneSided) + CtxtFun,
/// then is swallowed by FCS's outer wrapper (like RPAREN). The OffsideEnd and
/// OffsideRightBlockEnd therefore carry `}`'s span.
/// (LexFilter.fs:193, 2282, outer wrapper:2831)
#[test]
fn diff_filtered_fun_in_record() {
    assert_filtered_streams_match("let r = { F = fun x -> x }\n");
}

/// Lambda inside a `begin … end` block. Exercises the `begin`/`end`
/// CtxtParen pair: `end` acts as `TokenRExprParen`, force-closing the inner
/// SeqBlock(OneSided) + CtxtFun before balancing CtxtParen. Unlike RPAREN/
/// RBRACE, `end` is emitted in the filtered stream (not swallowed).
/// Also exercises `isIfBlockContinuator(END)` — `end` aligned with `if`
/// must not pop CtxtIf. (LexFilter.fs:193, 202, 2282)
#[test]
fn diff_filtered_fun_in_begin_end() {
    assert_filtered_streams_match("let f = begin fun x -> x end\n");
}

/// Lambda inside a `class … end` block. CLASS is unconditional in FCS
/// (LexFilter.fs:2573) and pushes `CtxtParen + SeqBlock(AddBlockEnd)` —
/// unlike `begin`/`sig`, the inner block emits OBLOCKBEGIN/OBLOCKEND around
/// the body. `end` then balances `CtxtParen(Opener::Class)` per
/// `parenTokensBalance` (LexFilter.fs:415).
#[test]
fn diff_filtered_fun_in_class_end() {
    assert_filtered_streams_match("let f = class fun x -> x end\n");
}

/// Lambda inside a `sig … end` block. SIG is folded into the TokenLExprParen
/// arm in FCS (LexFilter.fs:2281) — same structure as `begin` but with a
/// `SIG` token at the head and a balance match keyed on `Opener::Sig`.
/// Inner SeqBlock is NoAddBlockEnd, so no OBLOCKBEGIN around the body.
#[test]
fn diff_filtered_fun_in_sig_end() {
    assert_filtered_streams_match("let f = sig fun x -> x end\n");
}

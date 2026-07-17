//! Self-pinned `Virtual::BlockEnd` placement.
//!
//! The lex-filter differential ([`crate::common::assert_filtered_streams_match`])
//! *drops* `Virtual::BlockEnd` on our side: FCS's outer LexFilter wrapper
//! (`LexFilter.fs`, `GetToken`) swallows every `OBLOCKEND` and re-inserts
//! `OBLOCKEND_COMING_SOON`/`_IS_HERE` tokens that carry no `FSharpTokenKind`
//! arm (→ `FSharpTokenKind.None`) and are filtered out of the public token
//! stream, so `tokens-filtered` never surfaces a block end. The differential is
//! therefore structurally blind to `BlockEnd` — yet the parser's correctness
//! turns on exactly where it lands:
//!
//! * the `and`-chain gate (`parse_type_defn`'s `closed_block`) admits a
//!   continuation only once the previous body's `BlockEnd` has arrived;
//! * `parse_member_block_items` drains each member body's close `BlockEnd`;
//! * the phase-9.13b bare-trailing-member valid/invalid split is decided by
//!   whether the body-close `BlockEnd` lands *after* the member (valid,
//!   in-block) or *before* it (FCS-invalid, `=`-line form).
//!
//! Those are otherwise pinned only *indirectly*, via the parser AST diffs. These
//! tests pin the placement *directly* against our own filter output — the
//! [`super::computation_expr::and_bang_emits_virtual`] idiom for a virtual the
//! differential cannot see — so a regression that moves `BlockEnd` fails here
//! loudly rather than (at best) as a downstream AST divergence.

use borzoi_cst::lexer::{Token, lex};
use borzoi_cst::lexfilter::{FilteredToken, Virtual, filter};

/// The filtered token stream for `src`, panicking on any lex error.
fn filtered(src: &str) -> Vec<FilteredToken<'_>> {
    filter(src, lex(src))
        .map(|(t, _)| t.expect("lex ok"))
        .collect()
}

fn is_block_end(t: &FilteredToken<'_>) -> bool {
    matches!(t, FilteredToken::Virtual(Virtual::BlockEnd))
}

fn first_raw(toks: &[FilteredToken<'_>], tok: Token<'_>) -> usize {
    toks.iter()
        .position(|t| matches!(t, FilteredToken::Raw(r) if *r == tok))
        .unwrap_or_else(|| panic!("expected a raw {tok:?} in the filtered stream: {toks:?}"))
}

/// The `and`-chain gate: a `type … = <repr>` body must close (emit `BlockEnd`)
/// before an offside `and`, or `parse_type_defn`'s `closed_block` check refuses
/// to splice the continuation (an *inline* `and` inside a still-open body is
/// FCS's "Unexpected keyword 'and'"). Pin that the type body's `BlockEnd` lands
/// immediately before the `and`, and is the only one before it.
#[test]
fn blockend_closes_type_body_before_and() {
    let toks = filtered("type T = { X : int }\nand U = { Y : int }\n");
    let and_idx = first_raw(&toks, Token::And);
    assert!(
        is_block_end(&toks[and_idx - 1]),
        "the type body `BlockEnd` must immediately precede `and`: {toks:?}"
    );
    let before_and = toks[..and_idx].iter().filter(|t| is_block_end(t)).count();
    assert_eq!(
        before_and, 1,
        "exactly one `BlockEnd` should close the first body before `and`: {toks:?}"
    );
}

/// Phase 9.13b bare trailing member, *valid* form (`type R =⏎ { … }⏎ member …`):
/// the member sits inside the still-open body block, reached across a
/// `BlockSep`, with the body-closing `BlockEnd` landing *after* the member. Pin
/// that no `BlockEnd` precedes the `member` keyword — the discriminator the
/// bare-members hook relies on to route the member into the outer slot.
#[test]
fn no_blockend_before_member_in_valid_bare_trailing() {
    let toks = filtered("type R =\n  { X : int }\n  member M = 1\n");
    let member_idx = first_raw(&toks, Token::Member);
    assert!(
        matches!(
            toks[member_idx - 1],
            FilteredToken::Virtual(Virtual::BlockSep)
        ),
        "a valid in-block bare member must be reached across a `BlockSep`: {toks:?}"
    );
    assert!(
        !toks[..member_idx].iter().any(is_block_end),
        "no body-close `BlockEnd` may precede a valid in-block member: {toks:?}"
    );
}

/// Phase 9.13b bare trailing member, *FCS-invalid* form (`type R = { … }⏎
/// member …`, the `=`-line record): the body block closes *before* the member,
/// so a `BlockEnd` lands immediately before the `member` keyword. Pin that split
/// — it is what stops the bare-members hook from wrongly accepting the member
/// (`classify_object_model_item`'s virtual guard sees the `BlockEnd` first).
#[test]
fn blockend_before_member_in_invalid_inline_trailing() {
    let toks = filtered("type R = { X : int }\n  member M = 1\n");
    let member_idx = first_raw(&toks, Token::Member);
    assert!(
        is_block_end(&toks[member_idx - 1]),
        "the invalid `=`-line trailing member must be preceded by the body-close \
         `BlockEnd`: {toks:?}"
    );
}

/// Member-block close drains: each member body closes with `BlockEnd`+`DeclEnd`,
/// consecutive members are separated by a `BlockSep`, and the type body's own
/// `BlockEnd` trails the last member's. Pin that shape so a regression in the
/// per-member close-drain sequence (`parse_member_block_items` consumes it) is
/// caught here rather than as a downstream member-routing divergence.
#[test]
fn blockend_drains_between_and_after_members() {
    let toks = filtered("type T() =\n  member x.M = 1\n  member x.N = 2\n");
    let members: Vec<usize> = toks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| matches!(t, FilteredToken::Raw(Token::Member)).then_some(i))
        .collect();
    assert_eq!(members.len(), 2, "two members expected: {toks:?}");

    // The first member body closes `BlockEnd` then `DeclEnd` before the
    // inter-member `BlockSep`.
    let between = &toks[members[0]..members[1]];
    let be = between
        .iter()
        .position(is_block_end)
        .expect("first member body `BlockEnd`");
    assert!(
        matches!(between[be + 1], FilteredToken::Virtual(Virtual::DeclEnd)),
        "a member body `BlockEnd` should be followed by its `DeclEnd`: {toks:?}"
    );
    assert!(
        between[be + 1..]
            .iter()
            .any(|t| matches!(t, FilteredToken::Virtual(Virtual::BlockSep))),
        "consecutive members should be separated by a `BlockSep`: {toks:?}"
    );

    // The stream ends with the last member body's `BlockEnd` then the type
    // body's `BlockEnd`.
    let n = toks.len();
    assert!(
        is_block_end(&toks[n - 1]) && is_block_end(&toks[n - 2]),
        "the stream should end with member-body then type-body `BlockEnd`: {toks:?}"
    );
}

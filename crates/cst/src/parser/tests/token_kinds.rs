//! The raw table's kind-level trivia classification against the parser's
//! classification of the rebuilt entry, which it stands in for on the
//! productions' hot paths (`Parser::drain_raw_up_to`,
//! `Parser::significant_raw_from`).

use super::*;

/// Every raw kind: each lexed kind and each kind that is not lexed.
fn every_raw_kind() -> impl Iterator<Item = RawKind> {
    TokenKind::ALL
        .iter()
        .map(|&k| RawKind::Lexed(k))
        .chain(RawKind::NOT_LEXED.iter().copied())
}

#[test]
fn kind_level_trivia_agrees_with_the_rebuilt_entry() {
    for kind in every_raw_kind() {
        // The classification never reads a token's text; a `$` keeps the
        // extended interpolated kinds' delimiter count well formed.
        match kind.rebuild("$") {
            Ok(tt) => {
                assert_eq!(kind.trivia_syntax_kind(), raw_trivia_kind(&tt), "{kind:?}");
                assert_eq!(kind.is_trivia(), raw_is_trivia(&tt), "{kind:?}");
                assert_eq!(kind.is_trivia(), raw_significant(tt).is_none(), "{kind:?}");
            }
            Err(_) => {
                assert_eq!(kind.trivia_syntax_kind(), None, "{kind:?}");
                assert!(!kind.is_trivia(), "{kind:?}");
            }
        }
    }
}

#[test]
fn token_kind_all_lists_every_kind_once() {
    let distinct: std::collections::BTreeSet<_> = TokenKind::ALL.iter().collect();
    assert_eq!(distinct.len(), TokenKind::ALL.len());
    // `TokenKind` is a fieldless `repr(u16)` enum declared in the order `ALL`
    // lists it, so the discriminants are exactly `0..ALL.len()`.
    for (i, &k) in TokenKind::ALL.iter().enumerate() {
        assert_eq!(k as usize, i, "{k:?}");
    }
}

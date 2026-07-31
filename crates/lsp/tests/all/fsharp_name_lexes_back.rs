//! The oracle for [`borzoi_assembly::format_fsharp_name`]: a rendered name must
//! **lex back as exactly that one identifier**.
//!
//! Four rounds of review found four different names the renderer emitted bare
//! that F# rejects — a quoted case name, a `mod`, a `fixed`, an `A²` — each
//! found by a human reading a hand-copied word table against the compiler's.
//! That is the wrong instrument. `borzoi-cst`'s lexer already decides what F#
//! reads as an identifier, and it is differentially tested against FCS, so the
//! question "may this name be written this way?" is a *machine* question:
//! tokenize the rendered spelling and see.
//!
//! This crate is where the two meet — it already depends on `borzoi-assembly`
//! and `borzoi-cst` — so the oracle costs no new dependency edge, and neither
//! crate's self-containment is touched.
//!
//! # What this oracle cannot see
//!
//! Our lexer's identifier rule is `[\p{L}\p{Nl}_][\p{L}\p{N}\p{Mn}\p{Mc}\p{Pc}\p{Cf}']*`,
//! whose *part* class is `\p{N}` — every number category. F# admits only
//! `DecimalDigitNumber` and `LetterNumber`, so F# rejects `A²`
//! (`OtherNumber`) where our lexer accepts it. Two implementations sharing an
//! approximation agree vacuously, so this oracle is blind on exactly the axis
//! review round 3 found. That is why `format_fsharp_name` confines its bare
//! spelling to ASCII instead of matching the categories.
//!
//! Our lexer also has no `__token_…` entries, so it reads the compiler's
//! synthetic offside words as ordinary identifiers.
//!
//! Both blind spots are *pinned*
//! ([`the_oracle_is_blind_to_the_unicode_category_difference`],
//! [`the_oracle_is_blind_to_the_synthetic_lexer_words`]) so a later reader
//! cannot mistake this test's silence for coverage, and so the parts of
//! `format_fsharp_name` that cover them are not mistaken for redundancy.

use borzoi_assembly::format_fsharp_name;
use borzoi_cst::lexer::{Token, lex};
use proptest::prelude::*;

/// Lex `rendered` and recover the single identifier it spells, if that is what
/// it is. `None` when it lexed as anything else — several tokens, a keyword, a
/// literal — which is the failure this oracle exists to catch.
fn lexes_as_one_identifier(rendered: &str) -> Option<String> {
    let tokens: Vec<_> = lex(rendered)
        .filter(|(token, _)| !matches!(token, Ok(Token::Whitespace) | Ok(Token::Newline)))
        .collect();
    let [(only, _)] = tokens.as_slice() else {
        return None;
    };
    let Ok(only) = only else {
        return None;
    };
    match only {
        Token::Ident(text) => Some((*text).to_string()),
        // A quoted identifier's token text carries its delimiters; the name is
        // what sits between them.
        Token::QuotedIdent(text) => Some(
            text.strip_prefix("``")
                .and_then(|t| t.strip_suffix("``"))
                .expect("the lexer's QuotedIdent rule requires both delimiters")
                .to_string(),
        ),
        _ => None,
    }
}

/// The property: for any name, the rendered spelling lexes as one identifier
/// naming *that* name. Under-quoting breaks it (the spelling lexes as a keyword,
/// a wildcard, or several tokens); so does over-quoting a name that already
/// contains the delimiters.
fn assert_round_trips(name: &str) {
    let rendered = format_fsharp_name(name);
    let lexed = lexes_as_one_identifier(&rendered).unwrap_or_else(|| {
        panic!("{name:?} rendered as {rendered:?}, which does not lex as one identifier")
    });
    assert_eq!(
        lexed, name,
        "{name:?} rendered as {rendered:?}, which lexes as the identifier {lexed:?}"
    );
}

/// The names the four review rounds turned up, plus the shapes around them.
/// Written out rather than left to the generator: a regression corpus should
/// name its regressions.
#[test]
fn the_names_review_found_all_round_trip() {
    for name in [
        // Round 1: an F# quoted identifier keeps its spaces in metadata.
        "Circle Case",
        // Round 4a: the lexer's word list, not the tooltip list.
        "mod",
        "fixed",
        "_",
        "type",
        "member",
        "__LINE__",
        // fsc's generated names.
        "Circle@DebugTypeProxy",
        "System-Collections-Generic-IDictionary<'Key, 'T>-get_Keys@60",
        // Ordinary names, which must not acquire noise.
        "WriteLine",
        "Choice1Of2",
        "_private",
        "x'",
    ] {
        assert_round_trips(name);
    }
}

/// Generated names over an alphabet built from what metadata actually contains:
/// identifier characters, the separators fsc generates, and the punctuation that
/// forces quoting. The generator is the point — a hand-written table is what
/// four review rounds were correcting.
fn metadata_name() -> impl Strategy<Value = String> {
    // No backtick: a name carrying one has no faithful F# spelling at all (see
    // `a_name_carrying_backticks_has_no_faithful_spelling`), so it is outside
    // what this property can assert rather than a bug it would find.
    let alphabet = prop::sample::select(vec![
        'a', 'B', 'z', '_', '0', '9', '\'', ' ', '@', '-', '.', '<', '>', '|', '+', '!',
    ]);
    prop::collection::vec(alphabet, 1..8).prop_map(|chars| chars.into_iter().collect())
}

/// Seeds are not persisted. A saved case is only useful if replaying it
/// reproduces the failure, and the strategy here is the thing under
/// development — the first failure it found (a bare `` ` ``) is now outside the
/// alphabet deliberately, so its seed would replay as an unrelated name while
/// still reading as a live regression. The corpus that *does* survive strategy
/// changes is [`the_names_review_found_all_round_trip`], written out by hand.
fn config() -> proptest::test_runner::Config {
    proptest::test_runner::Config {
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    }
}

proptest! {
    #![proptest_config(config())]

    /// For all generated metadata names, the rendered spelling lexes back as
    /// exactly that identifier.
    #[test]
    fn a_generated_name_round_trips(name in metadata_name()) {
        let rendered = format_fsharp_name(&name);
        let lexed = lexes_as_one_identifier(&rendered);
        prop_assert_eq!(
            lexed.as_deref(),
            Some(name.as_str()),
            "{:?} rendered as {:?}",
            name,
            rendered
        );
    }

    /// Every word the renderer's table calls unwritable really is: rendering it
    /// bare would not lex as an identifier of that name. This is the table's
    /// *lower* bound, checked against the lexer rather than against a copy of
    /// the compiler's list — the drift that review round 4a caught.
    #[test]
    fn a_bare_word_that_does_not_lex_is_quoted(word in "[a-z_]{1,12}") {
        if lexes_as_one_identifier(&word).as_deref() != Some(word.as_str()) {
            prop_assert!(
                format_fsharp_name(&word).starts_with("``"),
                "{:?} does not lex as an identifier, so it must be quoted",
                word
            );
        }
    }
}

/// The second blind spot: our lexer has no `__token_…` entries at all, so it
/// reads the compiler's synthetic offside/parser words as ordinary identifiers
/// and the round-trip property would pass on a bare one. `format_fsharp_name`
/// carries them in its table for that reason — the table is not redundant with
/// this oracle, it covers exactly what the oracle cannot see.
#[test]
fn the_oracle_is_blind_to_the_synthetic_lexer_words() {
    assert_eq!(
        lexes_as_one_identifier("__token_OBLOCKSEP").as_deref(),
        Some("__token_OBLOCKSEP"),
        "our lexer takes a synthetic token word as an identifier; if this ever \
         fails, it models them and the oracle now covers this axis"
    );
    assert_eq!(
        format_fsharp_name("__token_OBLOCKSEP"),
        "``__token_OBLOCKSEP``"
    );
}

/// The blindness, pinned. Our lexer accepts `A²` as an identifier because its
/// part class is `\p{N}`; F# rejects `OtherNumber`, so a bare `A²` is not a
/// legal F# identifier and the round-trip property above would nonetheless pass
/// on it. `format_fsharp_name` covers the gap by confining bare spellings to
/// ASCII — this test exists so that anyone tempted to widen that rule back to
/// the Unicode categories, on the grounds that the oracle is green, sees why the
/// oracle cannot be the authority here.
#[test]
fn the_oracle_is_blind_to_the_unicode_category_difference() {
    assert_eq!(
        lexes_as_one_identifier("A\u{b2}").as_deref(),
        Some("A\u{b2}"),
        "our lexer takes `A²` bare; if this ever fails, the lexer has been \
         tightened to F#'s categories and the oracle now covers this axis"
    );
    // The renderer does not rely on that: it quotes every non-ASCII name.
    assert_eq!(format_fsharp_name("A\u{b2}"), "``A\u{b2}``");
}

/// The one shape with no faithful spelling, kept out of the property above and
/// pinned here instead: a name whose own text carries the delimiters. Bare, it
/// lexes as the shorter name between them; quoted, they close early. The
/// renderer quotes it so the backticks are visible as part of the name, and this
/// records that the result deliberately does not round-trip.
#[test]
fn a_name_carrying_backticks_has_no_faithful_spelling() {
    let rendered = format_fsharp_name("``Odd``");
    assert_eq!(rendered, "````Odd````");
    assert_ne!(
        lexes_as_one_identifier(&rendered).as_deref(),
        Some("``Odd``"),
        "if this ever round-trips, F# has grown an escape and the renderer \
         should use it"
    );
}

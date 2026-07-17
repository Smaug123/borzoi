//! FS0046 "The identifier '…' is reserved for future use by F#".
//!
//! F#'s ML-compatibility reserved words (`break`, `sealed`, `tailcall`, … — see
//! [`crate::lexer::RESERVED_IDENTS`]) lex as ordinary identifiers, but FCS's
//! `KeywordOrIdentifierToken` emits a *warning* for each occurrence before
//! handing the parser an `IDENT` (`dotnet/fsharp/src/Compiler/SyntaxTree/LexHelpers.fs`).
//! So `ParseHadErrors` stays false; we surface the warning on `Parse::warnings`
//! (not `errors`) — see [`super::super::reserved_ident_diagnostics`].
//!
//! Cross-checked against FCS's *acceptance* by the differential suite in
//! `tests/all/parser_diff_reserved_idents.rs`; these are the local-only tests for
//! the warning payload and the negative cases the differential helper (which
//! ignores warnings) can't express.

use super::super::*;

/// The (message, span) of every reserved-identifier warning emitted for `source`.
fn reserved_warnings(source: &str) -> Vec<(String, std::ops::Range<usize>)> {
    parse(source)
        .warnings
        .into_iter()
        .filter(|w| w.message.contains("reserved for future use"))
        .map(|w| (w.message, w.span))
        .collect()
}

#[test]
fn reserved_let_binding_warns_at_the_word() {
    // `let break = 10` — the word is bytes 4..9.
    assert_eq!(
        reserved_warnings("let break = 10\n"),
        vec![(
            "The identifier 'break' is reserved for future use by F#".to_string(),
            4..9,
        )],
    );
    // A warning, never an error — `ParseHadErrors` (our `errors`) stays clean.
    assert!(parse("let break = 10\n").errors.is_empty());
}

#[test]
fn every_reserved_word_warns() {
    for word in crate::lexer::RESERVED_IDENTS {
        let source = format!("let {word} = 10\n");
        let warns = reserved_warnings(&source);
        assert_eq!(
            warns.len(),
            1,
            "expected one warning for {word:?}: {warns:?}"
        );
        assert!(
            warns[0].0.contains(&format!("'{word}'")),
            "warning {:?} should name {word:?}",
            warns[0].0,
        );
        assert!(parse(&source).errors.is_empty(), "{word:?} must not error",);
    }
}

#[test]
fn multiple_reserved_words_each_warn() {
    // Two reserved words → two warnings, one per occurrence, in source order.
    let warns = reserved_warnings("let f sealed pure = 1\n");
    assert_eq!(warns.len(), 2, "{warns:?}");
    assert_eq!(warns[0].1, 6..12); // `sealed`
    assert_eq!(warns[1].1, 13..17); // `pure`
}

#[test]
fn backtick_quoted_reserved_word_does_not_warn() {
    // ``break`` is an explicit quoted identifier (a `Token::QuotedIdent`), not a
    // bare reserved word — FCS issues no FS0046.
    assert!(reserved_warnings("let ``break`` = 10\n").is_empty());
}

#[test]
fn reserved_word_in_inactive_region_does_not_warn() {
    // Inside a false `#if`, the word never lexes to `Token::Ident`, so no warning
    // — matching FCS, which doesn't lex inactive code.
    let source = "#if UNDEFINED\nlet break = 10\n#endif\nlet x = 1\n";
    assert!(reserved_warnings(source).is_empty(), "{source:?}");
}

#[test]
fn reserved_word_as_substring_of_ident_does_not_warn() {
    // `breakpoint` / `sealedThing` are ordinary identifiers that merely *contain*
    // a reserved word — the scan matches whole lexemes only.
    assert!(reserved_warnings("let breakpoint = 1\n").is_empty());
    assert!(reserved_warnings("let sealedThing = 1\n").is_empty());
}

#[test]
fn reserved_word_immediately_followed_by_hash_does_not_warn() {
    // FCS lexes `break#` as one `ident '#'` lexeme and looks *that* up — `break#`
    // isn't reserved, so it emits only FS1141, no FS0046 (verified via fcs-dump).
    assert!(reserved_warnings("let y = break#\n").is_empty());
    // A space breaks the composite: `break #` lexes `break` (→ FS0046) then `#`.
    assert_eq!(reserved_warnings("let y = break #\n").len(), 1);
}

#[test]
fn reserved_word_followed_by_bang_warns_over_the_whole_lexeme() {
    // FCS's `ident '!'` rule looks up the *trimmed* `break` (so FS0046 fires
    // alongside FS1141) but keeps the full `break!` `LexemeRange`. Verified via
    // fcs-dump: `break!` → FS0046 over cols 8..14 (the `!` included).
    let warns = reserved_warnings("let y = break! x\n");
    assert_eq!(warns.len(), 1, "{warns:?}");
    assert_eq!(warns[0].1, 8..14); // `break!`, bang included

    // A space breaks the composite: the warning covers just `break`.
    let spaced = reserved_warnings("let y = break ! x\n");
    assert_eq!(spaced.len(), 1, "{spaced:?}");
    assert_eq!(spaced[0].1, 8..13); // `break`
}

#[test]
fn reserved_word_glued_to_multichar_bang_op_warns_over_one_bang() {
    // Logos lexes `break!!` / `break!=` as `Ident("break")` + `Op("!!")` /
    // `Op("!=")`, but FCS's `ident '!'` consumes exactly one `!`, so FS0046
    // spans `break!` (cols 8..14) — verified via fcs-dump for `!!`, `!=`, `!+`.
    for src in [
        "let y = break!! x\n",
        "let y = break!= x\n",
        "let y = break!+ x\n",
    ] {
        let warns = reserved_warnings(src);
        assert_eq!(warns.len(), 1, "{src:?}: {warns:?}");
        assert_eq!(warns[0].1, 8..14, "{src:?}"); // `break!`, one bang only
    }
}

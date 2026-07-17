//! MSBuild's **escaped value domain**.
//!
//! MSBuild stores every property, item spec and metadatum value *escaped*, and
//! unescapes it exactly once, at the point of use. That single rule is the
//! whole model; `docs/msbuild-escaped-value-plan.md` derives the consequences
//! (and the review findings that were, before this module, each modelled as a
//! rule of its own).
//!
//! Ported from dotnet/msbuild `src/Shared/EscapingUtilities.cs`:
//!
//! - [`escape`] ← `Escape` / `AppendEscapedString` (line 153/289). Rewrites each
//!   of the **nine** reserved characters to `%XX`, with **lowercase** hex
//!   (`HexDigitChar`, line 260: `x + (x < 10 ? '0' : 'a' - 10)`).
//! - [`unescape`] ← `UnescapeAll` (line 59). Decodes `%` + two hex digits to a
//!   single UTF-16 char (line 112: `(char)((digit1 << 4) + digit2)` — so `%e2`
//!   is U+00E2, *not* a UTF-8 byte), left to right, **never re-scanning decoded
//!   output**. A `%` with any other suffix stays literal.
//!
//! Three sources put text *into* the domain and one operation takes it out; see
//! [`Escaped`]. The pair satisfies `unescape(escape(s)) == s` for all `s` — the
//! law that makes the domain sound, property-tested below. Note the reverse does
//! **not** hold (`escape(unescape(s)) != s` in general), so no caller may
//! unescape and re-store.

/// `EscapingUtilities.cs:310` — `s_charsToEscape`. Nine characters, not one:
/// a `;` or `*` reaching an item spec unescaped splits a list or globs a
/// directory that MSBuild treats as literal text.
const CHARS_TO_ESCAPE: &[u8] = b"%*?@$();'";

/// Escape text that entered from *outside* the domain — a filesystem path, a
/// toolset seed, a property-function result. MSBuild does exactly this when it
/// seeds the reserved path properties (`Evaluator.cs:1186–1189`) and the
/// toolset paths (`Toolset.cs:802`).
pub fn escape(unescaped: &str) -> String {
    if !unescaped.bytes().any(|b| CHARS_TO_ESCAPE.contains(&b)) {
        return unescaped.to_string();
    }
    let mut out = String::with_capacity(unescaped.len() * 2);
    for ch in unescaped.chars() {
        // Every reserved character is ASCII, so a `char`-wise walk classifies
        // multi-byte characters correctly (their UTF-8 bytes are all >= 0x80).
        if ch.is_ascii() && CHARS_TO_ESCAPE.contains(&(ch as u8)) {
            out.push('%');
            out.push(hex_digit(ch as u8 / 0x10));
            out.push(hex_digit(ch as u8 & 0x0f));
        } else {
            out.push(ch);
        }
    }
    out
}

/// Lowercase, matching `EscapingUtilities.HexDigitChar` (line 260). The case is
/// observable — an escaped `;` is `%3b` — so it is pinned, not incidental.
fn hex_digit(x: u8) -> char {
    debug_assert!(x < 16);
    char::from(if x < 10 { b'0' + x } else { b'a' - 10 + x })
}

fn decode_hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// The point of use: take text *out* of the escaped domain, exactly once.
///
/// A faithful port of `UnescapeAll`. Two behaviours are load-bearing and easy
/// to get wrong by writing the "obvious" decoder instead:
///
/// - **Decoded output is never re-scanned.** `%2525` is `%25`, not `%`.
/// - **The scan for the next `%` resumes one past the previous `%`**, not past
///   the escape it consumed — so a failed decode leaves its `%` literal and the
///   *next* character can still start one: `%%41` is `%A`.
pub fn unescape(escaped: &str) -> String {
    let bytes = escaped.as_bytes();
    let Some(first) = bytes.iter().position(|b| *b == b'%') else {
        return escaped.to_string();
    };

    let mut out = String::with_capacity(escaped.len());
    // Byte offset up to which `out` is up to date with `escaped`.
    let mut copied = 0usize;
    let mut at = first;
    loop {
        if let (Some(hi), Some(lo)) = (
            bytes.get(at + 1).copied().and_then(decode_hex_digit),
            bytes.get(at + 2).copied().and_then(decode_hex_digit),
        ) {
            // The two consumed bytes are hex digits, never `%`, so the next `%`
            // the scan finds is at or after `at + 3` — the slice below cannot
            // run backwards.
            debug_assert!(copied <= at);
            out.push_str(&escaped[copied..at]);
            // A single UTF-16 char in MSBuild; the value is in 0..=0xFF, which
            // is a Unicode scalar, so there is no surrogate to worry about.
            out.push(char::from(hi * 0x10 + lo));
            copied = at + 3;
        }
        match bytes[at + 1..].iter().position(|b| *b == b'%') {
            Some(next) => at += 1 + next,
            None => break,
        }
    }
    out.push_str(&escaped[copied..]);
    out
}

/// Whether any `%XX` in `escaped` decodes to one of `targets`.
///
/// A downstream scanner that re-reads decoded text — the glob resolver splitting
/// on `;` and parsing `*`/`?`, the wildcard-import matcher — would take such a
/// character as *syntax*, when MSBuild classified it as data before decoding.
/// Callers that cannot express "literal, despite looking like syntax" decline
/// instead of guessing.
pub fn decodes_to_any(escaped: &str, targets: &[char]) -> bool {
    escaped.as_bytes().windows(3).any(|w| {
        w[0] == b'%'
            && w[1].is_ascii_hexdigit()
            && w[2].is_ascii_hexdigit()
            && unescape(std::str::from_utf8(w).expect("ASCII escape"))
                .chars()
                .next()
                .is_some_and(|c| targets.contains(&c))
    })
}

/// A value in MSBuild's escaped domain.
///
/// The type exists to make "which domain is this string in?" a question the
/// compiler answers rather than a reviewer. It deliberately has no `Display`,
/// no `Deref`, and no `AsRef<str>`: text leaves only through [`Escaped::unescape`]
/// (a point of use) or [`Escaped::as_escaped`] (a splice or a scan that MSBuild
/// itself performs on escaped text — `;` splitting, glob classification,
/// `@(…)`/`%(…)` detection).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Escaped(String);

impl Escaped {
    /// Project XML body/attribute text, or a caller-supplied global property.
    /// Both are **already** escaped-domain text: `<P>a%20b</P>` evaluates to
    /// `a b`, and `-p:P=a%20b` likewise. Taken verbatim.
    pub fn from_xml(text: impl Into<String>) -> Self {
        Escaped(text.into())
    }

    /// Text the evaluator computed from the world: a filesystem path, a
    /// toolset/SDK seed, a property-function result. Escaped on the way in,
    /// exactly as MSBuild escapes such values when it seeds them.
    pub fn from_computed(text: &str) -> Self {
        Escaped(escape(text))
    }

    /// Leave the domain. The only exit — and it consumes nothing, so a caller
    /// cannot accidentally unescape twice: the result is a `String`, which has
    /// no `unescape`.
    pub fn unescape(&self) -> String {
        unescape(&self.0)
    }

    /// The escaped text, for splicing into another escaped buffer and for the
    /// scans MSBuild performs on escaped text (`;` splits, glob and
    /// item/metadata-reference classification). Not for display, comparison
    /// against user-facing text, or the filesystem.
    pub fn as_escaped(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Append another escaped value — composition *within* the domain, which is
    /// what `$(A)$(B)` does.
    pub fn push(&mut self, other: &Escaped) {
        self.0.push_str(&other.0);
    }

    /// Append **project XML text** — the literal runs between `$(…)`
    /// references. XML text is already escaped-domain text (`<P>a%20b</P>`
    /// evaluates to `a b`), so it composes verbatim.
    pub fn push_xml(&mut self, text: &str) {
        self.0.push_str(text);
    }

    /// Split a `;`-delimited list the way MSBuild does: **on the semicolons of
    /// the escaped text**, so an escaped `%3b` is data and does not split.
    ///
    /// This is the rule for every list MSBuild builds out of a property — item
    /// specs, and the property-to-list conversion a task parameter performs.
    /// Oracle-pinned 2026-07-12: with `<D>A%3bB</D>`, `<X Include="$(D)"/>` is
    /// **one** item whose identity is `A;B`; likewise
    /// `TargetFrameworks=net8.0%3bnet9.0` is one (bogus) framework, not two.
    ///
    /// Decoding first and splitting after turns that one entry into two — which
    /// is exactly why the split lives on this type rather than being re-derived,
    /// correctly or otherwise, at each leaf. Fragments come back still escaped;
    /// each leaves the domain at its own point of use.
    pub fn split_list(&self) -> impl Iterator<Item = Escaped> + '_ {
        self.0
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Escaped(s.to_string()))
    }

    /// Trim surrounding whitespace **in the domain**, then decode.
    ///
    /// The order is the whole content of this method. An escaped `%20` is a
    /// literal space MSBuild keeps — a file really named `Custom.targets ` is
    /// named `Custom.targets%20` — so trimming *after* decoding eats a character
    /// that is data, and the caller then probes a different filename entirely.
    /// Only whitespace the author actually wrote as whitespace is padding.
    pub fn trimmed_unescaped(&self) -> String {
        unescape(self.0.trim())
    }

    /// Whether the escaped text carries a **live** glob metacharacter — one
    /// MSBuild would treat as syntax. An escaped `%2a` is a literal star and is
    /// *not* live, which is why this question must be asked before decoding:
    /// classify first, unescape second.
    pub fn has_live_wildcard(&self) -> bool {
        self.0.contains(['*', '?'])
    }

    /// Append text **raw**, bypassing escaping.
    ///
    /// MSBuild's own hole, and the only one: the `Char` a string indexer
    /// returns (`$(P[3])`) goes back into the buffer unescaped, so its `%` can
    /// still compose a `%XX` escape with whatever follows it in the body. With
    /// `<Pct>100%</Pct>`, `$(Pct.ToString())20b` is the literal `100%20b` (a
    /// function result, escaped) while `$(Pct[3])20b` is `" b"` (a `Char`, raw)
    /// — both pinned against `dotnet msbuild` 10.0.301. The name is ugly on
    /// purpose: every call site is a place the domain invariant is suspended.
    pub fn push_unescaped_raw(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// `EscapingUtilities.cs:310` — the nine, and the lowercase hex of
    /// `HexDigitChar` (line 260).
    #[test]
    fn escapes_exactly_the_nine_reserved_characters_in_lowercase_hex() {
        assert_eq!(escape("%"), "%25");
        assert_eq!(escape("*"), "%2a");
        assert_eq!(escape("?"), "%3f");
        assert_eq!(escape("@"), "%40");
        assert_eq!(escape("$"), "%24");
        assert_eq!(escape("("), "%28");
        assert_eq!(escape(")"), "%29");
        assert_eq!(escape(";"), "%3b");
        assert_eq!(escape("'"), "%27");
        // Everything else rides through, including characters a URL encoder
        // would touch (space, `#`, `&`, non-ASCII).
        assert_eq!(escape("a b#&é/\\"), "a b#&é/\\");
    }

    /// The character set is the whole point of the sixth finding: a project
    /// directory named `a;b` seeds `a%3bb`, so the `;` never splits an item
    /// list (oracle-pinned 2026-07-12: `dotnet msbuild -getItem:Compile` on a
    /// project in such a directory returns *one* item with a literal `;`).
    #[test]
    fn a_semicolon_in_computed_text_survives_as_one_value() {
        let dir = Escaped::from_computed("/repo/a;b");
        assert_eq!(dir.as_escaped(), "/repo/a%3bb");
        assert_eq!(dir.unescape(), "/repo/a;b");
        // …and it does not split when the escaped text is scanned for `;`.
        assert_eq!(dir.as_escaped().split(';').count(), 1);
    }

    #[test]
    fn unescape_decodes_percent_hex_pairs_in_either_case() {
        assert_eq!(unescape("a%20b"), "a b");
        assert_eq!(unescape("%3B"), ";");
        assert_eq!(unescape("%3b"), ";");
        assert_eq!(unescape("B%65ta"), "Beta");
    }

    /// `%XX` is one **UTF-16 char**, not a UTF-8 byte (`EscapingUtilities.cs:112`).
    /// This settles the compile-item plan's "multi-byte escapes" open question.
    #[test]
    fn unescape_decodes_a_pair_as_one_utf16_char_not_a_utf8_byte() {
        assert_eq!(unescape("%e2"), "â");
        assert_eq!(unescape("%E2"), "â");
        // A UTF-8 encoder would have produced the two bytes of `â` from `%c3%a2`;
        // MSBuild produces two *characters*.
        assert_eq!(unescape("%c3%a2"), "Ã¢");
    }

    /// Decoded output is never re-scanned (the `UnescapeAll` loop appends the
    /// decoded char and advances past it).
    #[test]
    fn unescape_does_not_rescan_decoded_output() {
        assert_eq!(unescape("%2525"), "%25");
        assert_eq!(unescape("%252520"), "%2520");
    }

    /// The next-`%` scan resumes at `indexOfPercent + 1`, so a `%` whose decode
    /// failed stays literal *and* the character after it can start an escape.
    #[test]
    fn a_failed_decode_leaves_its_percent_literal() {
        assert_eq!(unescape("%%41"), "%A");
        assert_eq!(unescape("%zz"), "%zz");
        assert_eq!(unescape("%2"), "%2");
        assert_eq!(unescape("%"), "%");
        assert_eq!(unescape("100%"), "100%");
    }

    /// The composed-escape case the walker differential caught: neither `100%`
    /// nor the body carries an escape, but the composition does.
    #[test]
    fn an_escape_composed_across_a_splice_is_decoded() {
        assert_eq!(unescape("100%100%"), "100\u{10}0%");
    }

    /// MSBuild splits a property into a list on the semicolons of the
    /// **escaped** text, so an escaped `%3b` is data. Oracle-pinned 2026-07-12:
    /// with `<D>A%3bB</D>`, `<X Include="$(D)"/>` is one item, `A;B`.
    #[test]
    fn a_list_splits_on_escaped_semicolons_only() {
        let list = Escaped::from_xml("A%3bB;C");
        let fragments: Vec<String> = list.split_list().map(|f| f.unescape()).collect();
        assert_eq!(fragments, vec!["A;B".to_string(), "C".to_string()]);

        // Decoding first would have produced three entries — the bug this
        // ordering exists to prevent.
        assert_eq!(list.unescape().split(';').count(), 3);
    }

    /// Glob metacharacters are classified before decoding, for the same reason:
    /// an escaped `%2a` is a literal star in a filename, not a wildcard.
    #[test]
    fn a_wildcard_is_classified_before_decoding() {
        assert!(Escaped::from_xml("a*.props").has_live_wildcard());
        assert!(!Escaped::from_xml("star%2afile.props").has_live_wildcard());
        assert_eq!(
            Escaped::from_xml("star%2afile.props").unescape(),
            "star*file.props"
        );
        assert!(decodes_to_any("star%2afile.props", &['*', '?']));
        assert!(!decodes_to_any("a*.props", &['*', '?']));
    }

    /// Padding is trimmed *before* decoding, because an escaped `%20` is a
    /// literal space in the value — a file named `Custom.targets ` is written
    /// `Custom.targets%20`, and trimming after decoding would probe a different
    /// filename.
    #[test]
    fn trimming_happens_in_the_domain() {
        let padded = Escaped::from_xml("  Custom.targets%20  ");
        assert_eq!(padded.trimmed_unescaped(), "Custom.targets ");
        // Decoding first would have eaten the escaped space along with the
        // authored padding.
        assert_eq!(padded.unescape().trim(), "Custom.targets");
    }

    proptest! {
        /// **The law.** Escaping and unescaping round-trips for every string —
        /// this is what makes the domain sound, and what lets a value be stored
        /// escaped and read back exactly once.
        #[test]
        fn unescape_of_escape_is_the_identity(s in ".*") {
            prop_assert_eq!(unescape(&escape(&s)), s);
        }

        /// Escaping leaves no reserved character live: every reserved byte in
        /// the output is the `%` of an escape it introduced.
        #[test]
        fn escape_leaves_no_live_reserved_character(s in ".*") {
            let escaped = escape(&s);
            let bytes = escaped.as_bytes();
            for (i, b) in bytes.iter().enumerate() {
                if CHARS_TO_ESCAPE.contains(b) {
                    prop_assert_eq!(*b, b'%', "live reserved char at {} in {:?}", i, escaped);
                    prop_assert!(
                        bytes.get(i + 1).copied().and_then(decode_hex_digit).is_some()
                            && bytes.get(i + 2).copied().and_then(decode_hex_digit).is_some(),
                        "bare `%` at {} in {:?}",
                        i,
                        escaped
                    );
                }
            }
        }

        /// Text with no `%` is already outside the reach of the decoder, so
        /// unescaping it is a no-op (the `IndexOf('%') == -1` fast path).
        #[test]
        fn unescape_is_the_identity_on_percent_free_text(s in "[^%]*") {
            prop_assert_eq!(unescape(&s), s);
        }

        /// `Escaped` never loses the two ways in: XML text is taken verbatim,
        /// computed text is escaped, and both read back correctly.
        #[test]
        fn the_two_entrances_agree_on_what_comes_out(s in ".*") {
            prop_assert_eq!(Escaped::from_computed(&s).unescape(), s.clone());
            let from_xml = Escaped::from_xml(s.clone());
            prop_assert_eq!(from_xml.as_escaped(), s);
        }
    }
}

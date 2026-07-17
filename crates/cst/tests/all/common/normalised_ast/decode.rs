//! Literal and interpolated-string text decoding shared by both projectors.
//!
//! Pure `&str -> value` helpers: integer / float / char / string /
//! byte-string decoders, decimal canonicalisation, base64, and the
//! interpolation brace-digraph collapsing. [`super::from_cst`] uses these to
//! decode token text; [`super::from_fcs`] reuses [`decode_base64`] for
//! `SynConst.Bytes`.

/// Delimiter style of a fragment chain. Determined from the leading
/// fragment's `$"` / `$"""` / `$@"` (≡ `@$"`) opener — every continuation
/// fragment in the chain inherits it, since FCS records one
/// `SynStringKind` per `SynExpr.InterpolatedString`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InterpFragmentStyle {
    Single,
    Triple,
    Verbatim,
    /// `$$"""…"""` (≥2 `$`); `n` = delimiter length (the leading `$` count). A
    /// fill opens on a `{`-run ≥ `n`, closes on a `}`-run of `n`; runs shorter
    /// than `n` are literal content. Maps to `SynStringKind::TripleQuote`.
    Extended {
        n: usize,
    },
}

/// Detect the style of a leading interp fragment from its source text.
/// `Begin` / `BeginEnd` fragments start with `$"` (single); `TripleBegin` /
/// `TripleBeginEnd` start with `$"""` (triple); `VerbatimBegin` /
/// `VerbatimBeginEnd` start with `$@"` or `@$"` (verbatim). The triple and
/// verbatim openers are disjoint, so order between their tests doesn't
/// matter; the single `$"` is the fallback.
pub(super) fn detect_interp_style(text: &str) -> InterpFragmentStyle {
    let bytes = text.as_bytes();
    // Extended (`$$"""…`, ≥2 `$` + `"""`) is disjoint from the single `$"""`
    // test below (which needs exactly one `$`). `n` = leading `$` count.
    let dollars = bytes.iter().take_while(|&&c| c == b'$').count();
    if dollars >= 2 && bytes[dollars..].starts_with(b"\"\"\"") {
        InterpFragmentStyle::Extended { n: dollars }
    } else if bytes.starts_with(b"$\"\"\"") {
        InterpFragmentStyle::Triple
    } else if bytes.starts_with(b"$@\"") || bytes.starts_with(b"@$\"") {
        InterpFragmentStyle::Verbatim
    } else {
        InterpFragmentStyle::Single
    }
}

/// Strip the interp-string-fragment delimiters from a fragment's source text
/// and apply the appropriate escape-decoding for `style`, returning the raw
/// UTF-16 code units FCS stores in its .NET `string`.
/// Fragment shapes:
///
///   Single-quoted (`SynStringKind::Regular`):
///   * `BeginEnd`: `$"…"`   — leading `$"`,  trailing `"`.
///   * `Begin`:    `$"…{`   — leading `$"`,  trailing `{`.
///   * `Part`:     `}…{`    — leading `}`,   trailing `{`.
///   * `End`:      `}…"`    — leading `}`,   trailing `"`.
///
///   Triple-quoted (`SynStringKind::TripleQuote`):
///   * `TripleBeginEnd`: `$"""…"""` — leading `$"""`, trailing `"""`.
///   * `TripleBegin`:    `$"""…{`   — leading `$"""`, trailing `{`.
///   * `Part`:           `}…{`      — leading `}`,    trailing `{`.
///   * `End`:            `}…"""`    — leading `}`,    trailing `"""`.
///
///   Verbatim (`SynStringKind::Verbatim`, `$@"…"` ≡ `@$"…"`):
///   * `VerbatimBeginEnd`: `$@"…"` — leading `$@"`/`@$"` (3), trailing `"`.
///   * `VerbatimBegin`:    `$@"…{` — leading `$@"`/`@$"` (3), trailing `{`.
///   * `Part`:             `}…{`   — leading `}`,             trailing `{`.
///   * `End`:              `}…"`   — leading `}`,             trailing `"`.
///
///   Extended (`SynStringKind::TripleQuote`, `$$"""…"""`, `n` = `$` count):
///   * `ExtendedBeginEnd`: `$$"""…"""` — leading `$`×n + `"""` (n+3), trail `"""`.
///   * `ExtendedBegin`:    `$$"""…{…{` — leading n+3, trailing fill-open `{`-run.
///   * `Part`:             `}…}…{…{`   — leading `n` `}`, trailing `{`-run.
///   * `End`:              `}…}…"""`   — leading `n` `}`, trailing `"""`.
///
///   A trailing `{`-run of length `r` opens a fill: `r-n` leading braces are
///   content unless `r ≥ 2n` (FS1248, no content braces). No digraph collapse,
///   no backslash escape; a content `}`-run ≥ `n` is dropped (FS1249). Each
///   content `%`-run is transformed (`r < n` → `2r`; `n ≤ r ≤ 2n-1` →
///   `2(r-n)+1`; `r ≥ 2n` → dropped, FS1250) — see
///   [`collapse_extended_interp_body`].
///
/// After delimiter-strip, `{{`/`}}` digraphs collapse to single `{`/`}`
/// (interp-specific) in all styles. Single-quoted bodies then run through
/// [`decode_string_literal`] wrapped in a synthetic `"…"` to honour
/// `singleQuoteString` escapes; triple-quoted bodies do NOT honour backslash
/// escapes (FCS `tripleQuoteString` in `lex.fsl:1540` has no `\\X` arm) —
/// `\n` in a triple-quoted body is the two literal characters `\` and `n`, so
/// we skip the regular string pass and return the brace-collapsed body's UTF-16
/// units.
/// Verbatim bodies likewise have no backslash escape but additionally collapse
/// the verbatim quote escape `""` → `"` (see
/// [`collapse_verbatim_interp_body`]).
pub(super) fn decode_interp_fragment(text: &str, style: InterpFragmentStyle) -> Vec<u16> {
    let body = interp_fragment_body(text, style);
    match style {
        InterpFragmentStyle::Single => {
            let collapsed = collapse_interp_brace_digraphs(body);
            let synthetic = format!("\"{collapsed}\"");
            decode_string_literal(&synthetic)
        }
        InterpFragmentStyle::Verbatim => {
            collapse_verbatim_interp_body(body).encode_utf16().collect()
        }
        InterpFragmentStyle::Triple => collapse_triple_interp_brace_digraphs(body)
            .encode_utf16()
            .collect(),
        InterpFragmentStyle::Extended { n } => collapse_extended_interp_body(body, n)
            .encode_utf16()
            .collect(),
    }
}

/// Strip an interp fragment's delimiters and return the raw inner body
/// (before brace-digraph collapse / escape decode). Shared by
/// [`decode_interp_fragment`] and the byte-string downgrade path, so both agree
/// on the delimiter spans.
fn interp_fragment_body(text: &str, style: InterpFragmentStyle) -> &str {
    let bytes = text.as_bytes();
    debug_assert!(!bytes.is_empty(), "interp fragment cannot be empty");
    let (start, end) = match style {
        InterpFragmentStyle::Single => {
            let start = if bytes[0] == b'$' { 2 } else { 1 };
            (start, text.len() - 1)
        }
        InterpFragmentStyle::Verbatim => {
            // Leading fragment opens with `$@"` / `@$"` (3 bytes);
            // continuation fragments (`Part`/`End`) open with `}` (1 byte).
            // The trailing delimiter is always a single `"` or `{`.
            let start = if bytes[0] == b'}' { 1 } else { 3 };
            (start, text.len() - 1)
        }
        InterpFragmentStyle::Triple => {
            let start = if bytes.starts_with(b"$\"\"\"") { 4 } else { 1 };
            let end = if bytes.ends_with(b"\"\"\"") {
                text.len() - 3
            } else {
                text.len() - 1
            };
            (start, end)
        }
        InterpFragmentStyle::Extended { n } => {
            // Leading delimiter: opener `$`-run + `"""` (n+3), else the
            // `n`-brace fill closer (capped at the actual `}`-run).
            let start = if bytes[0] == b'$' {
                n + 3
            } else {
                bytes.iter().take_while(|&&c| c == b'}').count().min(n)
            };
            // Trailing delimiter: closing `"""`, else a fill-opening `{`-run.
            // For a run in `[n, 2n-1]`, the leading `run-n` braces are content
            // (strip only `n`); a run ≥ 2n keeps no content braces (FS1248 —
            // strip the whole run).
            let end = if bytes.ends_with(b"\"\"\"") {
                text.len() - 3
            } else {
                let run = bytes.iter().rev().take_while(|&&c| c == b'{').count();
                let strip = if run >= 2 * n { run } else { n };
                text.len() - strip
            };
            (start, end)
        }
    };
    &text[start..end]
}

/// Extended (`$$"""…`) body decode: no backslash escape, no `{{`/`}}` digraph
/// collapse. Two extended-only run transforms apply (`lex.fsl:1668-1733`):
///
/// * A content `}`-run of length ≥ `n` is unmatched (FS1249) and FCS drops the
///   whole run from the decoded text; shorter `}`-runs are kept verbatim.
/// * Each maximal `%`-run of length `r` is transformed (`maxPercents = 2n-1`):
///   `r < n` → `2r` `%` (literal-percent doubling); `n ≤ r ≤ 2n-1` →
///   `2(r-n)+1` `%` (one format `%` + doubled surplus); `r ≥ 2n` → FS1250 and
///   the whole run is dropped. Runs are transformed independently.
///
/// All other bytes (including `{`-runs < `n`) are kept verbatim.
fn collapse_extended_interp_body(body: &str, n: usize) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut seg_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'}' {
            out.push_str(&body[seg_start..i]);
            let run = bytes[i..].iter().take_while(|&&c| c == b'}').count();
            if run < n {
                for _ in 0..run {
                    out.push('}');
                }
            }
            i += run;
            seg_start = i;
        } else if bytes[i] == b'%' {
            out.push_str(&body[seg_start..i]);
            let run = bytes[i..].iter().take_while(|&&c| c == b'%').count();
            let emit = if run >= 2 * n {
                0
            } else if run < n {
                2 * run
            } else {
                2 * (run - n) + 1
            };
            for _ in 0..emit {
                out.push('%');
            }
            i += run;
            seg_start = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&body[seg_start..]);
    out
}

/// Triple-quoted variant of [`collapse_interp_brace_digraphs`]: only
/// `{{` and `}}` collapse to literal braces. `\` is a content byte
/// (no backslash escapes in `tripleQuoteString`).
pub fn collapse_triple_interp_brace_digraphs(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' if bytes.get(i + 1) == Some(&b'{') => {
                out.push('{');
                i += 2;
            }
            b'}' if bytes.get(i + 1) == Some(&b'}') => {
                out.push('}');
                i += 2;
            }
            _ => {
                // Scan a content run. Start the search at `i + 1`, not
                // `i`: when `bytes[i]` is itself a stray `{` or `}` (the
                // lexer lets one through for parser recovery, see
                // `lex_interp_triple_opener` and `scan_cont` in
                // `crates/cst/src/lexer/`), a search from `i` finds
                // position 0 and `i` never advances — infinite loop.
                // Searching from `i + 1` always consumes byte `i` as
                // content, so progress is guaranteed.
                let next = bytes[i + 1..]
                    .iter()
                    .position(|&b| b == b'{' || b == b'}')
                    .map(|n| i + 1 + n)
                    .unwrap_or(bytes.len());
                out.push_str(
                    std::str::from_utf8(&bytes[i..next])
                        .expect("interp body is UTF-8 by lexer construction"),
                );
                i = next;
            }
        }
    }
    out
}

/// Verbatim variant of [`collapse_triple_interp_brace_digraphs`]: `{{` /
/// `}}` collapse to literal braces (the interp brace escape) and `""`
/// collapses to a single `"` (the verbatim quote escape). `\` is a content
/// byte — verbatim bodies have no backslash escapes (`lex.fsl`'s
/// `verbatimString`). Like the triple collapser, the content-run scan
/// starts at `i + 1` so a stray single `{` / `}` / `"` always makes
/// progress.
fn collapse_verbatim_interp_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' if bytes.get(i + 1) == Some(&b'{') => {
                out.push('{');
                i += 2;
            }
            b'}' if bytes.get(i + 1) == Some(&b'}') => {
                out.push('}');
                i += 2;
            }
            b'"' if bytes.get(i + 1) == Some(&b'"') => {
                out.push('"');
                i += 2;
            }
            _ => {
                let next = bytes[i + 1..]
                    .iter()
                    .position(|&b| b == b'{' || b == b'}' || b == b'"')
                    .map(|n| i + 1 + n)
                    .unwrap_or(bytes.len());
                out.push_str(
                    std::str::from_utf8(&bytes[i..next])
                        .expect("interp body is UTF-8 by lexer construction"),
                );
                i = next;
            }
        }
    }
    out
}

/// Collapse interp-string brace digraphs `{{` / `}}` to single `{` /
/// `}` before the rest of the body runs through the regular string
/// decoder. F# treats `{{` / `}}` as the escape forms for literal
/// `{` / `}` inside an interp body (`lex.fsl:1471`); plain
/// `singleQuoteString` doesn't know about them, so we strip them here
/// first.
pub fn collapse_interp_brace_digraphs(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' if bytes.get(i + 1) == Some(&b'{') => {
                out.push('{');
                i += 2;
            }
            b'}' if bytes.get(i + 1) == Some(&b'}') => {
                out.push('}');
                i += 2;
            }
            _ => {
                // Start after `i`: a stray `{`, `}`, or trailing `\`
                // should be copied as content and must still advance.
                let next = bytes[i + 1..]
                    .iter()
                    .position(|&b| b == b'{' || b == b'}' || b == b'\\')
                    .map(|n| i + 1 + n)
                    .unwrap_or(bytes.len());
                out.push_str(
                    std::str::from_utf8(&bytes[i..next])
                        .expect("interp body is UTF-8 by lexer construction"),
                );
                i = next;
            }
        }
    }
    out
}

/// `` ``foo bar`` `` → `foo bar`; `foo` → `foo`. FCS stores the unquoted
/// text in `Ident.idText` (no backticks survive the lexer's normalisation).
pub(super) fn strip_backticks(text: &str) -> &str {
    text.strip_prefix("``")
        .and_then(|t| t.strip_suffix("``"))
        .unwrap_or(text)
}

/// Collapse a compiler-generated `fun`-lowering argument name `_arg<N>` to a
/// canonical `_arg` (dropping the index), leaving every other identifier
/// unchanged.
///
/// When a `fun` parameter is a non-simple pattern (`fun (KeyValue (k, v)) ->
/// …`), FCS rewrites it to `fun _arg<N> -> match _arg<N> with …`, numbering the
/// fresh scrutinee with a parse-time `SynArgNameGenerator` counter. That counter
/// is reset per top-level `let`/`use` but **carried across `type` definitions
/// and all their members** (and other decls) — a stateful discipline that is
/// brittle to replicate in a stateless projector and depends on the entire
/// preceding file. The index carries no syntactic information: the name is a
/// fresh scrutinee, never user-referenced, so canonicalising it on **both**
/// projectors compares the lowering's *structure* (the synthesised `match` and
/// its clauses) without coupling the diff to FCS's counter bookkeeping. A user
/// identifier literally spelled `_arg<N>` is collapsed identically on both
/// sides, so the comparison stays sound (it can only ever make both sides agree,
/// never mask a structural difference).
pub(super) fn canonicalise_synth_arg(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("_arg")
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        return "_arg".to_string();
    }
    text.to_string()
}

/// Split an optional leading `+`/`-` (a folded sign — see the parser's
/// `sign_fold` pass) off a numeric literal token's text, returning
/// `(is_minus, magnitude)`. The parser merges an adjacent sign into the
/// literal token, so a folded `-2147483648` / `-1.5m` / `-1I` arrives as
/// one token; the magnitude is what the existing decoders parse.
pub(super) fn split_num_sign(text: &str) -> (bool, &str) {
    if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = text.strip_prefix('+') {
        (false, rest)
    } else {
        (false, text)
    }
}

/// Decode an integer literal's body — the part *before* any
/// trailing-alpha suffix and with the leading `0x`/`0o`/`0b` prefix
/// stripped — into a `u64` magnitude. The `_` digit separators are
/// dropped. Returns the parsed value alongside its radix so callers can
/// distinguish decimal `42` (radix 10) from hex `0x42` (radix 16).
///
/// Only valid where the *value* fits `u64` — which holds for every
/// typed-width literal that survives the parser's range check.
///
/// Expects an unsigned magnitude (no leading sign); callers handling a
/// folded `±literal` strip the sign with [`split_num_sign`] first and
/// negate the typed result.
pub(super) fn decode_int_body(text: &str) -> u64 {
    let (radix, body) =
        if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (16, rest)
        } else if let Some(rest) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
            (8, rest)
        } else if let Some(rest) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
            (2, rest)
        } else {
            (10, text)
        };
    // After stripping the `0x`/`0o`/`0b` prefix, the suffix is the
    // trailing run of characters that are neither digits-in-`radix` nor
    // the `_` separator. For radix 16 this means `A-F`/`a-f` count as
    // digits (so e.g. `0xCAFEL` finds the suffix at `L`, not `C`).
    let suffix_start = body
        .char_indices()
        .find(|(_, c)| !c.is_digit(radix) && *c != '_')
        .map(|(i, _)| i)
        .unwrap_or(body.len());
    let digits = &body[..suffix_start];
    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    u64::from_str_radix(&cleaned, radix).unwrap_or_else(|e| {
        panic!("body {cleaned:?} of {text:?} (radix {radix}) doesn't parse: {e:?}")
    })
}

/// Parse an integer literal's text into a typed integer `T` via `u64`.
/// The parser has already range-checked the magnitude, so the narrowing
/// `TryFrom<u64>` is infallible for any kind that funnels through here.
pub(super) fn parse_suffixed_int<T>(text: &str) -> T
where
    T: TryFrom<u64>,
    <T as TryFrom<u64>>::Error: std::fmt::Debug,
{
    let value = decode_int_body(text);
    T::try_from(value).unwrap_or_else(|e| panic!("body of {text:?} doesn't fit type: {e:?}"))
}

/// Decode an `IEEE64_LIT` token's text into a 64-bit double bit pattern.
/// Two source forms funnel into this kind:
///   * `Token::Float64` decimal/exponent text (`1.0`, `1e10`, `1.5e-3`,
///     `1_0.0`) — F#'s `float` (.NET `Double.Parse`) accepts digit
///     separators in the value, so FCS round-trips `1_0.0` to the
///     `10.0` bit pattern. Rust's `f64::from_str` does not accept `_`,
///     so strip them before parsing.
///   * `Token::XIEEE64` hex/oct/bin body with `LF` suffix
///     (`0x4024000000000000LF`) — FCS strips `LF`, removes underscores,
///     parses as int64, and bit-casts via `Int64BitsToDouble` (lex.fsl
///     :506-509). Equivalent: decode body as u64 via the same path as
///     [`decode_int_body`] minus the `LF`. A sign folded onto this form
///     applies after the bit-cast; model that by flipping the sign bit for `-`.
pub(super) fn decode_ieee64(text: &str) -> u64 {
    let (minus, body) = split_num_sign(text);
    if let Some(body) = body.strip_suffix("LF") {
        let bits = decode_int_body(body);
        if minus { bits ^ (1u64 << 63) } else { bits }
    } else {
        let cleaned: String = text.chars().filter(|c| *c != '_').collect();
        cleaned
            .parse::<f64>()
            .unwrap_or_else(|e| panic!("Float64 token {text:?} doesn't parse: {e:?}"))
            .to_bits()
    }
}

/// Decode an `IEEE32_LIT` token's text into a 32-bit float bit pattern.
/// Two source forms funnel into this kind:
///   * `Token::Float32` (decimal/exponent or dotless) with `f`/`F`
///     suffix — FCS's `evalFloat` (`lex.fsl`:212) strips the trailing
///     char then removes underscores before `float32(...)`. Match.
///   * `Token::XIEEE32` hex/oct/bin body with `lf` suffix — FCS parses
///     the body as int64 in `0..=0xFFFFFFFF` and bit-casts via `ToSingle`
///     (`lex.fsl`:498-504). Equivalent: decode body as u32, then
///     `f32::from_bits`. A sign folded onto this form applies after the
///     bit-cast; model that by flipping the sign bit for `-`.
pub(super) fn decode_ieee32(text: &str) -> u32 {
    let (minus, body) = split_num_sign(text);
    if let Some(body) = body.strip_suffix("lf") {
        let bits = decode_int_body(body);
        let bits = u32::try_from(bits)
            .unwrap_or_else(|_| panic!("XIEEE32 body {body:?} of {text:?} doesn't fit u32"));
        if minus { bits ^ (1u32 << 31) } else { bits }
    } else {
        let body = &text[..text.len() - 1];
        let cleaned: String = body.chars().filter(|c| *c != '_').collect();
        cleaned
            .parse::<f32>()
            .unwrap_or_else(|e| panic!("Float32 token {text:?} doesn't parse: {e:?}"))
            .to_bits()
    }
}

/// Strip the trailing `B` from a byte-char literal text, leaving the
/// `'…'`-wrapped char. Caller must have verified `text.ends_with('B')`.
pub(super) fn strip_byte_suffix(text: &str) -> &str {
    text.strip_suffix('B')
        .expect("strip_byte_suffix called on text without trailing `B`")
}

/// Decode a byte-string literal's body to its UTF-16 *code units* — the
/// granularity FCS's buffer stores before `stringBufferAsBytes` takes each
/// unit's low byte (see [`units_to_bytes`]). `text` is the full token text
/// including delimiters and the trailing `B`; `style` selects the single-
/// vs triple-quoted decoder.
///
/// Every path keeps raw UTF-16 units so a lone-surrogate escape records
/// its raw low byte (FCS stores `\uD800`'s as `0x00`): the plain single-quoted
/// case uses [`decode_string_literal_units`] and the interp case uses
/// [`decode_interp_fragment`]. Verbatim and triple bodies honour no `\u` escape
/// (backslash is literal), so re-encoding their decoded `String` is already
/// lossless.
///
/// The parser recovers a byte suffix on a *bare* interpolated string
/// (`$"abc"B`, `$"""…"""B`, `$@"abc"B` ≡ `@$"abc"B`) as a `BYTE_STRING_LIT` /
/// `TRIPLE_BYTE_STRING_LIT` / `VERBATIM_BYTE_STRING_LIT` whose token text
/// retains the leading `$` (or `@$`). FCS lexes such a string as an
/// interpolated string — collapsing the brace digraphs `{{`/`}}` to single
/// `{`/`}` (and, for verbatim, `""` → `"`) — *before* downgrading it to
/// `BYTEARRAY`, so the recovered bytes must come from the brace-collapsing
/// interp decoder, not the plain string decoder (`$"{{"B` is the single byte
/// `{`, not two braces). Plain (non-interp) byte strings (`"abc"B`, `@"abc"B`,
/// `"""abc"""B`) do not collapse braces, so they route through the ordinary
/// decoders. The `@$"…"B` spelling starts with `@` rather than `$`, so the
/// interp test also matches the `@$` prefix (distinct from the plain `@"`
/// verbatim byte string).
pub(super) fn decode_byte_string_body(text: &str, style: InterpFragmentStyle) -> Vec<u16> {
    let stripped = strip_byte_suffix(text);
    if stripped.starts_with('$') || stripped.starts_with("@$") {
        decode_interp_fragment(stripped, style)
    } else {
        match style {
            InterpFragmentStyle::Single => decode_string_literal_units(stripped),
            InterpFragmentStyle::Verbatim => decode_verbatim_string_literal(stripped)
                .encode_utf16()
                .collect(),
            InterpFragmentStyle::Triple => decode_triple_quote_string_literal(stripped)
                .encode_utf16()
                .collect(),
            // Extended (`$$"""…`) has no byte form — the closer in
            // `extendedInterpolatedString` (`lex.fsl:1641`) has no `B` arm, so
            // `$$"""x"""B` is `App(interp, ident "B")`, never a byte string.
            InterpFragmentStyle::Extended { .. } => {
                unreachable!("extended interp has no byte-string form")
            }
        }
    }
}

/// Decode a `Token::Char` literal text into its raw UTF-16 code unit. The text
/// retains the surrounding `'…'`; this function strips those and dispatches on
/// the interior shape.
///
/// Escape forms (lex.fsl:303-313, 519-575):
///   * `\\`, `\"`, `\'`, `\a` `\f` `\v` `\n` `\t` `\b` `\r` — common
///     escapes (FCS's `escape` helper).
///   * `\xHH` — two-digit hex (value `0..=255`).
///   * `\uHHHH` — four-digit hex (any BMP code unit, incl. lone surrogates).
///   * `\UHHHHHHHH` — eight-digit hex. FCS rejects non-BMP code points
///     for char (`lexThisUnicodeOnlyInStringLiterals`), so only BMP
///     values reach a clean parse here.
///   * `\NNN` decimal trigraph (value `0..=255`).
///
/// The decoded value mirrors FCS's recovery for char-specific edge cases:
///   * A `\uHHHH` (or `\U0000HHHH`) escape may name a *lone surrogate*
///     (U+D800..=U+DFFF) — a valid UTF-16 code unit but not a Unicode scalar.
///     Keep that raw unit instead of converting through Rust `char`.
///   * A `\U` escape *above* the BMP (`> U+FFFF`, whether a valid astral
///     scalar like `\U0001F600` or out of range like `\U00110000`) is
///     invalid in a char literal — FCS reports FS1159/FS1245 and recovers
///     with `CHAR (char 0)`, i.e. NUL — so we return `0` for it.
///
/// Plain form: the first UTF-16 code unit of the scalar between the quotes.
pub(super) fn decode_char_literal(text: &str) -> u16 {
    let inner = text
        .strip_prefix('\'')
        .and_then(|t| t.strip_suffix('\''))
        .unwrap_or_else(|| panic!("char literal text {text:?} not wrapped in single quotes"));
    let bytes = inner.as_bytes();
    if bytes.first() != Some(&b'\\') {
        let ch = inner
            .chars()
            .next()
            .unwrap_or_else(|| panic!("char literal {text:?} has empty body"));
        let mut units = [0u16; 2];
        return ch.encode_utf16(&mut units)[0];
    }
    let codepoint = match bytes[1] {
        b'\\' => u32::from(b'\\'),
        b'"' => u32::from(b'"'),
        b'\'' => u32::from(b'\''),
        b'a' => 0x07,
        b'f' => 0x0c,
        b'v' => 0x0b,
        b'n' => 0x0a,
        b't' => 0x09,
        b'b' => 0x08,
        b'r' => 0x0d,
        b'x' | b'u' | b'U' => u32::from_str_radix(&inner[2..], 16)
            .unwrap_or_else(|e| panic!("char {text:?} hex escape doesn't parse: {e:?}")),
        b'0'..=b'9' => inner[1..]
            .parse::<u32>()
            .unwrap_or_else(|e| panic!("char {text:?} trigraph doesn't parse: {e:?}")),
        other => panic!("char literal {text:?} has unknown escape byte {other:#x}"),
    };
    if codepoint <= 0xFFFF {
        // Fits a UTF-16 unit: keep it verbatim, including lone surrogates.
        codepoint as u16
    } else {
        // `\U` above the BMP is invalid in a char literal; FCS recovers
        // with CHAR 0 (NUL), so the decoded value is U+0000.
        0
    }
}

/// Decode the body of a regular `"..."` string literal into the raw UTF-16 code
/// units FCS stores for `SynConst.String`.
pub(super) fn decode_string_literal(text: &str) -> Vec<u16> {
    decode_string_literal_units(text)
}

/// Decode a regular `"..."` string body to its raw UTF-16 *code units* —
/// the shared core of [`decode_string_literal`] and the single-quoted
/// byte-string path (which takes each unit's low byte; see
/// [`decode_byte_string_body`]). The lexer token text retains the surrounding
/// double quotes; this helper strips them and walks the inner content applying
/// FCS's escape table (`lex.fsl`'s
/// `singleQuoteString` rules at 1255-1410):
///
/// * Single-letter escapes via the `escape` helper (`\a`/`\f`/`\v`/`\n`/
///   `\t`/`\b`/`\r`, plus the trivial `\\`/`\"`/`\'`).
/// * `\xHH` — hex byte appended as a Unicode code point (`addUnicodeChar
///   buf (int (hexGraphShort ...))`).
/// * `\uHHHH` — four-hex code *unit* (any BMP value, incl. lone surrogates).
/// * `\UHHHHHHHH` — eight-hex Unicode scalar (including astral planes,
///   unlike chars where FCS rejects non-BMP). An *out-of-range* value
///   (`> U+10FFFF`) is FS1245 in FCS, which then appends nothing — so we
///   skip it too (see [`push_code_point`]).
/// * `\NNN` decimal trigraph (`0..=255`, byte-valued).
/// * `\` + newline + leading whitespace — line continuation: the
///   backslash, the newline, and any subsequent space/tab characters are
///   removed.
/// * Literal newlines and other characters pass through unchanged.
///
/// A `\u`/`\U` escape may name a *lone surrogate* (U+D800..=U+DFFF) — a
/// valid UTF-16 code unit but not a Unicode scalar, so it cannot live in a
/// Rust `char`/`String`. Returning raw units keeps this observable in the
/// normalised AST instead of collapsing through U+FFFD.
fn decode_string_literal_units(text: &str) -> Vec<u16> {
    let inner = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or_else(|| panic!("string literal text {text:?} not wrapped in double quotes"));
    let mut out: Vec<u16> = Vec::with_capacity(inner.len());
    let mut bytes = inner.as_bytes();
    while !bytes.is_empty() {
        if bytes[0] != b'\\' {
            // UTF-8 boundary: find the next backslash (or end) and emit the slice as-is.
            let next = bytes
                .iter()
                .position(|&b| b == b'\\')
                .unwrap_or(bytes.len());
            out.extend(
                std::str::from_utf8(&bytes[..next])
                    .expect("string body is UTF-8 by lexer construction")
                    .encode_utf16(),
            );
            bytes = &bytes[next..];
            continue;
        }
        if bytes.len() < 2 {
            out.push(u16::from(b'\\'));
            bytes = &bytes[1..];
            continue;
        }
        match bytes[1] {
            b'\\' => {
                push_scalar(&mut out, '\\');
                bytes = &bytes[2..];
            }
            b'"' => {
                push_scalar(&mut out, '"');
                bytes = &bytes[2..];
            }
            b'\'' => {
                push_scalar(&mut out, '\'');
                bytes = &bytes[2..];
            }
            b'a' => {
                push_scalar(&mut out, '\u{07}');
                bytes = &bytes[2..];
            }
            b'f' => {
                push_scalar(&mut out, '\u{0c}');
                bytes = &bytes[2..];
            }
            b'v' => {
                push_scalar(&mut out, '\u{0b}');
                bytes = &bytes[2..];
            }
            b'n' => {
                push_scalar(&mut out, '\n');
                bytes = &bytes[2..];
            }
            b't' => {
                push_scalar(&mut out, '\t');
                bytes = &bytes[2..];
            }
            b'b' => {
                push_scalar(&mut out, '\u{08}');
                bytes = &bytes[2..];
            }
            b'r' => {
                push_scalar(&mut out, '\r');
                bytes = &bytes[2..];
            }
            b'x' => {
                // `\xHH` requires exactly two hex digits after `\x`. FCS's
                // `singleQuoteString` regex (`lex.fsl`:1255-1410) only
                // fires when the full body is present; bodies like `"\x"`
                // fall through to the literal pass-through arm. Our
                // single-string lexer blindly skips 2 bytes per `\`, so we
                // need to detect the incomplete case here.
                if let Some(v) = try_fixed_hex_escape(bytes, 2, 4) {
                    push_code_point(&mut out, v);
                    bytes = &bytes[4..];
                } else {
                    push_literal_backslash(&mut out, &mut bytes);
                }
            }
            b'u' => {
                if let Some(v) = try_fixed_hex_escape(bytes, 2, 6) {
                    push_code_point(&mut out, v);
                    bytes = &bytes[6..];
                } else {
                    push_literal_backslash(&mut out, &mut bytes);
                }
            }
            b'U' => {
                if let Some(v) = try_fixed_hex_escape(bytes, 2, 10) {
                    push_code_point(&mut out, v);
                    bytes = &bytes[10..];
                } else {
                    push_literal_backslash(&mut out, &mut bytes);
                }
            }
            b'0'..=b'9' => {
                // `\NNN` requires exactly three decimal digits with value
                // 0..=255. Anything else (too short, non-digit, > 255) is
                // a regex non-match in FCS and falls through to the
                // literal pass-through.
                if let Some(v) = try_trigraph(bytes) {
                    push_code_point(&mut out, v);
                    bytes = &bytes[4..];
                } else {
                    push_literal_backslash(&mut out, &mut bytes);
                }
            }
            b'\n' => {
                // Line continuation: drop the backslash + newline + any
                // subsequent run of `' '` / `'\t'` per `singleQuoteString`'s
                // `'\\' newline anywhite*` arm (`lex.fsl`:1256).
                bytes = &bytes[2..];
                while matches!(bytes.first(), Some(b' ' | b'\t')) {
                    bytes = &bytes[1..];
                }
            }
            b'\r' => {
                // `\r\n` line-continuation form: same treatment as `\n`.
                let after = if bytes.get(2) == Some(&b'\n') { 3 } else { 2 };
                bytes = &bytes[after..];
                while matches!(bytes.first(), Some(b' ' | b'\t')) {
                    bytes = &bytes[1..];
                }
            }
            _ => {
                push_literal_backslash(&mut out, &mut bytes);
            }
        }
    }
    out
}

/// Append a Unicode scalar to a UTF-16 code-unit buffer (one unit for a
/// BMP scalar, a surrogate pair for an astral one).
fn push_scalar(out: &mut Vec<u16>, ch: char) {
    let mut buf = [0u16; 2];
    out.extend_from_slice(ch.encode_utf16(&mut buf));
}

/// Append a numeric escape value (`\xHH` / `\uHHHH` / `\UHHHHHHHH` /
/// `\NNN`) as UTF-16 code unit(s). A value `<= 0xFFFF` is emitted as a
/// single unit *verbatim* — including a lone surrogate (U+D800..=U+DFFF),
/// which the normalised AST now keeps as-is for string consts and whose low
/// byte is taken as-is for byte consts. An astral scalar splits into its
/// surrogate pair.
///
/// An out-of-range value (`> U+10FFFF`, reachable only via a malformed
/// `\UHHHHHHHH`) appends *nothing*: FCS reports FS1245 and recovers by
/// dropping the escape entirely (`"x\U00110000y"` decodes to `"xy"`), so
/// emitting a replacement unit here would diverge from the recovered AST.
fn push_code_point(out: &mut Vec<u16>, v: u32) {
    if v <= 0xFFFF {
        out.push(v as u16);
    } else if let Some(ch) = char::from_u32(v) {
        push_scalar(out, ch);
    }
}

/// Push the backslash + the next Unicode scalar literally into `out` and
/// advance `bytes` past them. FCS's `singleQuoteString` (`lex.fsl`:1255-
/// 1410) only recognises the escape table; for any other byte after `\`,
/// both the backslash and the next character are appended to the buffer
/// (`"\q"` parses to value `\q`). Also used by the fixed-width arms when
/// their body is incomplete (`"\x"` → `\x`).
fn push_literal_backslash(out: &mut Vec<u16>, bytes: &mut &[u8]) {
    out.push(u16::from(b'\\'));
    if bytes.len() < 2 {
        *bytes = &bytes[1..];
        return;
    }
    let next = bytes[1];
    if next.is_ascii() {
        out.push(u16::from(next));
        *bytes = &bytes[2..];
    } else {
        let scalar = std::str::from_utf8(&bytes[1..])
            .ok()
            .and_then(|s| s.chars().next())
            .expect("non-UTF-8 byte after backslash in string body");
        push_scalar(out, scalar);
        *bytes = &bytes[1 + scalar.len_utf8()..];
    }
}

/// Try to decode a fixed-width hex escape body. `bytes` starts at the
/// `\`; `start` is the offset of the first hex digit (2 for `\xHH`,
/// `\uHHHH`, `\UHHHHHHHH`); `end` is the offset one past the last hex
/// digit (4/6/10 respectively). Returns `Some(value)` if every byte in
/// `[start, end)` is a hex digit; `None` if the body is too short or
/// contains non-hex bytes (FCS regex non-match → literal pass-through).
fn try_fixed_hex_escape(bytes: &[u8], start: usize, end: usize) -> Option<u32> {
    if bytes.len() < end {
        return None;
    }
    let body = &bytes[start..end];
    if !body.iter().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hex = std::str::from_utf8(body).expect("hex body is ASCII");
    u32::from_str_radix(hex, 16).ok()
}

/// Try to decode a `\NNN` decimal trigraph. `bytes` starts at the `\`.
/// Returns `Some(value)` if positions 1..=3 are decimal digits and the
/// combined value fits in `0..=255`; `None` otherwise (FCS regex
/// non-match → literal pass-through).
fn try_trigraph(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }
    let d1 = bytes[1];
    let d2 = bytes[2];
    let d3 = bytes[3];
    if !(d1.is_ascii_digit() && d2.is_ascii_digit() && d3.is_ascii_digit()) {
        return None;
    }
    let v = u32::from(d1 - b'0') * 100 + u32::from(d2 - b'0') * 10 + u32::from(d3 - b'0');
    if v > 255 { None } else { Some(v) }
}

/// Decode a verbatim `@"..."` string. The lexer keeps the leading `@"`
/// and the trailing `"`; this strips them and collapses the only
/// in-string escape, `""` → `"`. All other characters (including
/// newlines and backslashes) pass through verbatim — `lex.fsl`'s
/// `verbatimString` rules at 1433-1525 just append the raw lexemes to
/// the buffer.
pub(super) fn decode_verbatim_string_literal(text: &str) -> String {
    let inner = text
        .strip_prefix("@\"")
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or_else(|| {
            panic!("verbatim string literal text {text:?} not wrapped in `@\"...\"`")
        });
    let mut out = String::with_capacity(inner.len());
    let mut rest = inner;
    while let Some(idx) = rest.find('"') {
        out.push_str(&rest[..idx]);
        out.push('"');
        // The lexer guarantees `""` (the escape) is the only context
        // where a `"` byte appears inside the string body — a lone
        // closing `"` ends the token. So `rest[idx+1]` must also be `"`.
        rest = &rest[idx + 2..];
    }
    out.push_str(rest);
    out
}

/// Decode a triple-quoted `"""..."""` string. No escapes apply; the
/// content between the outer triples is the literal value. `lex.fsl`'s
/// `tripleQuoteString` at 1540 only recognises `"""` as the terminator
/// and appends every other lexeme verbatim.
pub(super) fn decode_triple_quote_string_literal(text: &str) -> String {
    text.strip_prefix("\"\"\"")
        .and_then(|t| t.strip_suffix("\"\"\""))
        .unwrap_or_else(|| panic!("triple string literal text {text:?} not wrapped in `\"\"\"`"))
        .to_string()
}

/// RFC 4648 base64 decoder — used to convert the JSON payload of
/// `SynConst.Bytes` (System.Text.Json's default `byte[]` encoding) back
/// to the raw bytes. Standard alphabet, `=` padding, no whitespace
/// tolerance (System.Text.Json never emits any).
///
/// A dedicated helper rather than pulling in `base64`/`data-encoding`
/// because it's the only consumer in the crate; the function is small
/// and the integration tests already roll a non-trivial amount of
/// FCS-equivalent decoding by hand.
pub(super) fn decode_base64(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    assert!(
        bytes.len().is_multiple_of(4),
        "base64 length must be a multiple of 4: {s:?}"
    );
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks_exact(4) {
        let v0 = b64_val(chunk[0]);
        let v1 = b64_val(chunk[1]);
        let v2 = b64_val(chunk[2]);
        let v3 = b64_val(chunk[3]);
        out.push(((v0 << 2) | (v1 >> 4)) as u8);
        if chunk[2] != b'=' {
            out.push((((v1 & 0x0f) << 4) | (v2 >> 2)) as u8);
        }
        if chunk[3] != b'=' {
            out.push((((v2 & 0x03) << 6) | v3) as u8);
        }
    }
    out
}

fn b64_val(b: u8) -> u32 {
    match b {
        b'A'..=b'Z' => u32::from(b - b'A'),
        b'a'..=b'z' => u32::from(b - b'a') + 26,
        b'0'..=b'9' => u32::from(b - b'0') + 52,
        b'+' => 62,
        b'/' => 63,
        b'=' => 0, // padding placeholder; caller masks contribution out
        other => panic!("invalid base64 byte {other:#x}"),
    }
}

/// Turn a byte-string's decoded UTF-16 code units into the byte array FCS
/// stores: `stringBufferAsBytes` (`LexHelpers.fs:122`) takes the low byte
/// of each unit.
///
/// FCS's buffer is fed by `addByteChar`/`addUnicodeChar` at the `char`
/// (UTF-16 code unit) granularity — so an astral codepoint contributes
/// *two* bytes (low bytes of its surrogate pair), whether it arrived as a
/// literal in source (`"😀"B`) or via a `\U0001F600` escape (FCS's escape
/// decoder emits the surrogate pair rather than the int32; see
/// `lex.fsl:1330-1332`). A lone surrogate likewise contributes its raw low
/// byte (`\uD800` → `0x00`), which is why [`decode_byte_string_body`]
/// returns raw units rather than a lossily-decoded `String`.
pub(super) fn units_to_bytes(units: &[u16]) -> Vec<u8> {
    units.iter().map(|&u| u as u8).collect()
}

/// Canonicalise a `Token::Decimal` source text (e.g. `1.0m`, `1e10M`,
/// `1.5_0e+2m`) to the form `System.Decimal.Parse(s, AllowExponent |
/// Number).ToString(InvariantCulture)` would produce. Trailing-zero scale
/// must survive intact — `1.0m` ≠ `1.00m` at this layer (they have
/// different `decimal` representations and System.Text.Json's
/// `DecimalConverter` distinguishes them).
///
/// Algorithm:
/// 1. Strip the trailing `m`/`M` and `_` separators.
/// 2. Split off an optional `[eE][+-]?digits` exponent.
/// 3. Split the mantissa on `.` into integer/fractional digit strings.
/// 4. Apply the exponent: `effectiveScale = fracLen - exp`. Negative
///    effective scale shifts digits left (appends zeros) so the
///    represented value stays an integer; positive scale > 28 triggers
///    banker's rounding (`MidpointRounding.ToEven`) down to a 28-place
///    scale to match `Decimal.Parse`.
/// 5. Strip leading zeros from the integer portion (keep at least one).
///
/// This intentionally does not detect mantissa overflow (>28 significant
/// digits after the rounding step) — `decimal.Parse` would throw there,
/// but the lexer's regex will let any digit run through and the diff
/// tests are the gating signal if we ever care.
pub(super) fn canonicalise_decimal_source(text: &str) -> String {
    let no_suffix = text.strip_suffix(['m', 'M']).unwrap_or_else(|| {
        panic!("Decimal token {text:?} must end with `m`/`M` (lexer invariant)")
    });
    let cleaned: String = no_suffix.chars().filter(|c| *c != '_').collect();

    let (mantissa, exp): (&str, i64) = match cleaned.find(['e', 'E']) {
        Some(idx) => {
            let exp = cleaned[idx + 1..]
                .parse::<i64>()
                .unwrap_or_else(|_| panic!("Decimal exponent in {text:?} doesn't parse as i64"));
            (&cleaned[..idx], exp)
        }
        None => (cleaned.as_str(), 0),
    };

    let (int_part, frac_part) = match mantissa.find('.') {
        Some(idx) => (&mantissa[..idx], &mantissa[idx + 1..]),
        None => (mantissa, ""),
    };

    let all_digits = format!("{int_part}{frac_part}");
    let frac_len = frac_part.len() as i64;
    let scale = frac_len - exp;

    let (digit_str, final_scale) = if scale <= 0 {
        let mut d = all_digits;
        for _ in 0..(-scale) {
            d.push('0');
        }
        (d, 0usize)
    } else if scale > 28 {
        // System.Decimal can't hold scale > 28; Decimal.Parse applies
        // banker's rounding (ToEven) to bring the scale back into range.
        let drop = (scale - 28) as usize;
        let keep_len = all_digits.len().saturating_sub(drop);
        let (kept, dropped) = all_digits.split_at(keep_len);
        let round_up = should_round_up(kept, dropped);
        let mut kept_string = kept.to_string();
        if round_up {
            increment_decimal_string(&mut kept_string);
        }
        // Re-pad so we still have at least one integer digit.
        let needed = 28usize;
        if kept_string.len() < needed + 1 {
            let pad = needed + 1 - kept_string.len();
            kept_string = format!("{}{}", "0".repeat(pad), kept_string);
        }
        (kept_string, needed)
    } else {
        let needed = scale as usize;
        if all_digits.len() < needed + 1 {
            let pad = needed + 1 - all_digits.len();
            (format!("{}{}", "0".repeat(pad), all_digits), needed)
        } else {
            (all_digits, needed)
        }
    };

    let int_len = digit_str.len() - final_scale;
    let int_str = &digit_str[..int_len];
    let frac_str = &digit_str[int_len..];

    let trimmed_int = {
        let t = int_str.trim_start_matches('0');
        if t.is_empty() { "0" } else { t }
    };

    if final_scale == 0 {
        trimmed_int.to_string()
    } else {
        format!("{trimmed_int}.{frac_str}")
    }
}

/// Banker's rounding decision (`MidpointRounding.ToEven`) used by
/// `Decimal.Parse`. `kept` is the digit string of the truncated
/// coefficient; `dropped` is the rejected tail. Returns `true` iff
/// the rounded value increments by one ULP.
fn should_round_up(kept: &str, dropped: &str) -> bool {
    let first = match dropped.chars().next() {
        Some(c) => c,
        None => return false,
    };
    match first {
        '0'..='4' => false,
        '6'..='9' => true,
        '5' => {
            if dropped[1..].chars().any(|c| c != '0') {
                true
            } else {
                // Exact midpoint — round to even (round up if the last
                // kept digit is odd).
                kept.chars()
                    .last()
                    .is_some_and(|c| c.to_digit(10).unwrap() % 2 == 1)
            }
        }
        _ => panic!("non-digit {first:?} in dropped tail {dropped:?}"),
    }
}

/// In-place `+1` on a non-negative decimal digit string, propagating
/// the carry left and prepending `1` if necessary.
fn increment_decimal_string(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    let mut carry = 1u8;
    for byte in bytes.iter_mut().rev() {
        if carry == 0 {
            break;
        }
        let d = *byte - b'0' + carry;
        if d == 10 {
            *byte = b'0';
            carry = 1;
        } else {
            *byte = b'0' + d;
            carry = 0;
        }
    }
    if carry == 1 {
        bytes.insert(0, b'1');
    }
    *s = String::from_utf8(bytes).expect("digit string stays ASCII");
}

//! The case-variant axis shared by the condition, property-expression and
//! `.fsproj` grammar differentials.
//!
//! MSBuild reads most of its lexemes case-insensitively — keywords, built-in
//! function names, property names, item types, metadata names, property-function
//! type and member names — and compares strings in a condition under
//! `OrdinalIgnoreCase`. A generator that only ever spells each of those one way
//! cannot tell a case-insensitive implementation from a case-sensitive one: both
//! agree with MSBuild on every input it builds. That is how a planted
//! case-sensitive `==` passed every generated sweep.
//!
//! Two tools, used together:
//!
//! - [`respell_case`] flips the ASCII letters of exactly the lexemes MSBuild
//!   folds, so a respelt input must mean the same thing as the original. The
//!   sweeps use it **metamorphically**: our verdict on the respelling must equal
//!   our verdict on the original, which is the only check that sees a
//!   case-sensitivity bug that *declines* rather than commits wrongly. The
//!   respelling's own claim — that MSBuild's answer does not move — is checked
//!   against the oracle per case, so a scanning mistake here fails loudly
//!   instead of widening what the metamorphic check believes is equivalent.
//! - [`CASE_FAMILIES`] and [`gen_case_comparison`] make operands that differ only
//!   in case actually meet, including the non-ASCII letters where
//!   `OrdinalIgnoreCase` (per-UTF-16-unit simple uppercase, not culture-aware,
//!   no full case folding) gives a specific answer a plausible reimplementation
//!   would get wrong. Those go to the oracle under certain-implies-exact; nothing
//!   about them is assumed.

use super::SplitMix64;

/// Which language a respelt text is written in. It decides which letters
/// MSBuild reads case-insensitively, and so which [`respell_case`] may flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseContext {
    /// A `Condition` string. Keywords (`and`, `or`), built-in function names
    /// (`HasTrailingSlash`, `Exists`), bare and quoted comparison operands
    /// (string equality is `OrdinalIgnoreCase`, the boolean vocabulary and `0x`
    /// hex are case-insensitive), and every identifier inside a `$(…)`.
    Condition,
    /// A property body. Only the identifiers inside a `$(…)` are
    /// case-insensitive: the literal text around them *is* the value.
    PropertyBody,
}

/// Flip the case of a random subset of the ASCII letters in every lexeme MSBuild
/// reads case-insensitively, and leave every other character as it was.
///
/// Inside a `$(…)` it touches the property name, a static call's type name
/// (`[MSBuild]`, `[System.IO.Path]`) and function name, and every member name in
/// the chain (`.Contains`, `.Length`). It never touches a property function's
/// *arguments*, except for the identifiers of a `$(…)` nested in one, because
/// `Contains('a')` and `Contains('A')` are different questions; nor an
/// `Exists(…)` argument, because filesystem case-sensitivity is the host's, not
/// MSBuild's; nor any non-ASCII letter, because whether MSBuild folds one is
/// what [`CASE_FAMILIES`] exists to ask the oracle.
pub fn respell_case(rng: &mut SplitMix64, text: &str, context: CaseContext) -> String {
    let mut respeller = Respeller {
        rng,
        src: text.chars().collect(),
        i: 0,
        out: String::with_capacity(text.len()),
    };
    match context {
        CaseContext::Condition => respeller.condition(),
        CaseContext::PropertyBody => respeller.body(),
    }
    respeller.out
}

/// Respell a bare name — a property's definition, an item type, an element of
/// the vocabulary — by flipping a random subset of its ASCII letters.
pub fn respell_name(rng: &mut SplitMix64, name: &str) -> String {
    name.chars().map(|c| flip_ascii(rng, c)).collect()
}

/// Respell every name in a property map, keeping the values.
pub fn respell_keys(rng: &mut SplitMix64, props: &[(String, String)]) -> Vec<(String, String)> {
    props
        .iter()
        .map(|(k, v)| (respell_name(rng, k), v.clone()))
        .collect()
}

fn flip_ascii(rng: &mut SplitMix64, c: char) -> char {
    if !c.is_ascii_alphabetic() || rng.below(2) == 0 {
        c
    } else if c.is_ascii_uppercase() {
        c.to_ascii_lowercase()
    } else {
        c.to_ascii_uppercase()
    }
}

struct Respeller<'a> {
    rng: &'a mut SplitMix64,
    src: Vec<char>,
    i: usize,
    out: String,
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl Respeller<'_> {
    fn peek(&self, offset: usize) -> Option<char> {
        self.src.get(self.i + offset).copied()
    }

    fn at_expression_open(&self) -> bool {
        self.peek(0) == Some('$') && self.peek(1) == Some('(')
    }

    fn copy(&mut self) {
        self.out.push(self.src[self.i]);
        self.i += 1;
    }

    fn copy_flipped(&mut self) {
        let c = flip_ascii(self.rng, self.src[self.i]);
        self.out.push(c);
        self.i += 1;
    }

    /// An identifier run, flipped.
    fn identifier(&mut self) {
        while self.peek(0).is_some_and(is_ident) {
            self.copy_flipped();
        }
    }

    fn whitespace(&mut self) {
        while self.peek(0).is_some_and(char::is_whitespace) {
            self.copy();
        }
    }

    fn condition(&mut self) {
        while let Some(c) = self.peek(0) {
            if self.at_expression_open() {
                self.expression();
            } else if c == '\'' {
                self.quoted(c, true);
            } else if c.is_ascii_alphabetic() || c == '_' {
                let end = (self.i..self.src.len())
                    .find(|&j| !is_ident(self.src[j]))
                    .unwrap_or(self.src.len());
                let word: String = self.src[self.i..end].iter().collect();
                self.identifier();
                if word.eq_ignore_ascii_case("exists") {
                    self.whitespace();
                    if self.peek(0) == Some('(') {
                        self.verbatim_parenthesised();
                    }
                }
            } else {
                self.copy();
            }
        }
    }

    fn body(&mut self) {
        while self.peek(0).is_some() {
            if self.at_expression_open() {
                self.expression();
            } else {
                self.copy();
            }
        }
    }

    /// A quoted run opened by `delim`. A condition operand's letters compare
    /// case-insensitively (`flip_text`); a property-function argument's do not.
    /// Either way a nested `$(…)` has its identifiers respelt.
    fn quoted(&mut self, delim: char, flip_text: bool) {
        self.copy();
        while let Some(c) = self.peek(0) {
            if c == delim {
                self.copy();
                return;
            }
            if self.at_expression_open() {
                self.expression();
            } else if flip_text {
                self.copy_flipped();
            } else {
                self.copy();
            }
        }
    }

    /// `$(` … `)`: a property reference or a static call, then a member chain.
    fn expression(&mut self) {
        self.copy(); // `$`
        self.copy(); // `(`
        self.whitespace();
        if self.peek(0) == Some('[') {
            self.copy();
            while self.peek(0).is_some_and(|c| c != ']') {
                self.copy_flipped();
            }
            if self.peek(0) == Some(']') {
                self.copy();
            }
            while self.peek(0) == Some(':') {
                self.copy();
            }
        }
        self.identifier();
        while let Some(c) = self.peek(0) {
            match c {
                ')' => {
                    self.copy();
                    return;
                }
                '.' => {
                    self.copy();
                    self.whitespace();
                    self.identifier();
                }
                '(' => self.arguments(),
                _ => self.copy(),
            }
        }
    }

    /// A property function's argument list, from `(` to its matching `)`.
    fn arguments(&mut self) {
        self.copy(); // `(`
        while let Some(c) = self.peek(0) {
            match c {
                ')' => {
                    self.copy();
                    return;
                }
                '\'' | '`' | '"' => self.quoted(c, false),
                '(' => self.arguments(),
                _ if self.at_expression_open() => self.expression(),
                _ => self.copy(),
            }
        }
    }

    /// `(` … `)` copied character for character, quote-aware.
    fn verbatim_parenthesised(&mut self) {
        let mut depth = 0usize;
        let mut quote: Option<char> = None;
        while let Some(c) = self.peek(0) {
            self.copy();
            match (quote, c) {
                (Some(q), _) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '\'' | '`' | '"') => quote = Some(c),
                (None, '(') => depth += 1,
                (None, ')') => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Families of operands that are equal, or nearly so, under *some* notion of
/// case folding. A comparison draws both operands from one family, so these
/// pairs actually meet. No member's verdict is assumed: every comparison goes to
/// the oracle, and our evaluator is free to decline any of them.
pub const CASE_FAMILIES: &[&[&str]] = &[
    // ASCII, where the answer is plain case-insensitivity.
    &["abc", "ABC", "aBc"],
    &["Debug", "debug", "DEBUG"],
    &["net8.0", "NET8.0", "Net8.0"],
    &["true", "TRUE", "True", "on", "ON", "Yes", "YES"],
    &["0x1f", "0X1F", "0x1F", "31"],
    // Latin-1: a one-to-one case pair.
    &["caf\u{e9}", "CAF\u{c9}", "Caf\u{e9}", "cafe"],
    // Turkish i: dotted capital İ (U+0130) and dotless ı (U+0131).
    &["i", "I", "\u{130}", "\u{131}"],
    // German sharp s: no single-character uppercase, so `ß` is not `SS`; capital
    // ẞ (U+1E9E) lowercases to it.
    &["stra\u{df}e", "STRASSE", "strasse", "STRA\u{1e9e}E"],
    // Greek sigma: medial σ and final ς both uppercase to Σ.
    &["\u{3c3}", "\u{3c2}", "\u{3a3}"],
    // Kelvin sign (U+212A) and Ångström sign (U+212B): Unicode case folding
    // takes them to ASCII `k` and to `å`, but each is its own uppercase.
    &["k", "K", "\u{212a}"],
    &["\u{e5}", "\u{c5}", "\u{212b}"],
    // Long s (U+017F): uppercases to ASCII `S`.
    &["s", "S", "\u{17f}"],
    // Micro sign (U+00B5) uppercases to Greek capital mu (U+039C).
    &["\u{b5}", "\u{3bc}", "\u{39c}"],
    // A titlecase digraph: Ǆ / ǅ / ǆ.
    &["\u{1c4}", "\u{1c5}", "\u{1c6}"],
    // Outside the BMP: Deseret 𐐀 / 𐐨, a surrogate pair in UTF-16.
    &["\u{10400}", "\u{10428}"],
];

/// `name` with its first letter that has a non-ASCII case relative replaced by
/// that relative: `s`/`S` by `ſ` (U+017F), `i`/`I` by `ı` (U+0131), `k`/`K` by
/// the Kelvin sign (U+212A).
pub fn non_ascii_lookalike(name: &str) -> Option<String> {
    let (at, c) = name
        .char_indices()
        .find(|(_, c)| matches!(c.to_ascii_lowercase(), 's' | 'i' | 'k'))?;
    let replacement = match c.to_ascii_lowercase() {
        's' => '\u{17f}',
        'i' => '\u{131}',
        _ => '\u{212a}',
    };
    let mut out = String::with_capacity(name.len() + 2);
    out.push_str(&name[..at]);
    out.push(replacement);
    out.push_str(&name[at + c.len_utf8()..]);
    Some(out)
}

/// A comparison whose two operands differ, if at all, only in case: both drawn
/// from one [`CASE_FAMILIES`] entry, or a defined property against a respelling
/// of its own value (the `'$(Configuration)' == 'debug'` shape).
pub fn gen_case_comparison(rng: &mut SplitMix64, props: &[(String, String)]) -> String {
    let op = if rng.below(4) == 0 { "!=" } else { "==" };
    if !props.is_empty() && rng.below(6) == 0 {
        // A reference spelt with a non-ASCII letter that *some* case mapping
        // takes to the defined name (`ſ` uppercases to `S`, `ı` to `I`, the
        // Kelvin sign folds to `k`). MSBuild's property names are ASCII, so
        // none of these may resolve to the defined property — and a lookup
        // that folded with Unicode tables would make one of them do so.
        let (name, value) = rng.pick(props).clone();
        if let Some(lookalike) = non_ascii_lookalike(&name) {
            return format!("'$({lookalike})' {op} '{value}'");
        }
    }
    if !props.is_empty() && rng.below(3) == 0 {
        let (name, value) = rng.pick(props).clone();
        let value = respell_name(rng, &value);
        return if rng.below(2) == 0 {
            format!("'$({name})' {op} '{value}'")
        } else {
            format!("'{value}' {op} '$({name})'")
        };
    }
    let family = *rng.pick(CASE_FAMILIES);
    let lhs = *rng.pick(family);
    let rhs = *rng.pick(family);
    // A bare word half the time where the operand can be one, so the
    // unquoted-operand path meets case too.
    let spell = |s: &str, rng: &mut SplitMix64| {
        if s.chars().all(|c| c.is_ascii_alphabetic()) && rng.below(2) == 0 {
            s.to_string()
        } else {
            format!("'{s}'")
        }
    };
    let lhs = spell(lhs, rng);
    let rhs = spell(rhs, rng);
    format!("{lhs} {op} {rhs}")
}

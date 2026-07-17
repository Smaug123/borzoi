//! Parser for `#if` / `#elif` directive expressions.
//!
//! Grammar, per FCS's `src/Compiler/pppars.fsy`:
//!
//! ```text
//! Expr  ::=  ID                       // identifier (no keywords; `true`/`false` are ordinary idents)
//!        |   '!' Expr                 // logical not
//!        |   Expr '&&' Expr           // left-associative
//!        |   Expr '||' Expr           // left-associative
//!        |   '(' Expr ')'
//! ```
//!
//! Precedence (tightest → loosest): `!`, `&&`, `||`.
//!
//! The token set, per FCS's `src/Compiler/pplex.fsl`: `ID`, `!`, `&&`, `||`,
//! `(`, `)`, EOF. Whitespace is space and tab only. A `//` starts a trailing
//! comment that terminates the token stream. A `(*` anywhere in the
//! expression is a hard error.
//!
//! The caller is expected to have stripped the `#if` / `#elif` prefix
//! already, matching FCS where that prefix is consumed by the preprocessor
//! lexer as the synthetic `PRELUDE` token.

use std::fmt;

/// A parsed `#if` / `#elif` expression.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Expr {
    Ident(String),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

impl Expr {
    pub fn ident(s: impl Into<String>) -> Self {
        Self::Ident(s.into())
    }

    pub fn and(a: Self, b: Self) -> Self {
        Self::And(Box::new(a), Box::new(b))
    }

    pub fn or(a: Self, b: Self) -> Self {
        Self::Or(Box::new(a), Box::new(b))
    }

    /// Evaluate the expression under a symbol lookup. Mirrors FCS's
    /// `LexerIfdefEval`: an identifier is true iff `lookup(name)` returns
    /// true. Case-sensitive (`HashSet<String>` semantics; F# define
    /// constants are case-sensitive).
    ///
    /// `lookup` is a closure rather than a `HashSet` so callers can plug
    /// in any source of truth (a set, a Vec, an env-var probe) without
    /// allocating. Typical use: `expr.eval(|n| symbols.contains(n))`.
    pub fn eval(&self, lookup: impl Fn(&str) -> bool) -> bool {
        // Indirection through a `&` shim lets the recursive helper take
        // `&impl Fn` without needing the bound to be `Copy`. The public
        // signature stays clean (no `&`-on-`Fn` at the call site).
        self.eval_inner(&lookup)
    }

    fn eval_inner(&self, lookup: &impl Fn(&str) -> bool) -> bool {
        match self {
            Self::Ident(name) => lookup(name),
            Self::Not(e) => !e.eval_inner(lookup),
            Self::And(a, b) => a.eval_inner(lookup) && b.eval_inner(lookup),
            Self::Or(a, b) => a.eval_inner(lookup) || b.eval_inner(lookup),
        }
    }

    /// Print the expression in a fully-parenthesised canonical form that
    /// always round-trips through [`parse_if_expr`]. Not intended for
    /// diagnostics — every binary op is wrapped in `(...)` and every `!`
    /// in `(!...)`, which is unambiguous but ugly. Pretty-printing for
    /// diagnostics is deferred until a consumer needs it.
    pub fn to_canonical_string(&self) -> String {
        let mut out = String::new();
        self.write_canonical(&mut out);
        out
    }

    fn write_canonical(&self, out: &mut String) {
        match self {
            Self::Ident(s) => out.push_str(s),
            Self::Not(e) => {
                out.push_str("(!");
                e.write_canonical(out);
                out.push(')');
            }
            Self::And(a, b) => {
                out.push('(');
                a.write_canonical(out);
                out.push_str(" && ");
                b.write_canonical(out);
                out.push(')');
            }
            Self::Or(a, b) => {
                out.push('(');
                a.write_canonical(out);
                out.push_str(" || ");
                b.write_canonical(out);
                out.push(')');
            }
        }
    }
}

/// An error from [`parse_if_expr`]. `at` is a byte offset into the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub at: usize,
    pub kind: ParseErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// Input ended where an identifier, `!`, or `(` was expected.
    UnexpectedEof,
    /// A token appeared where an operand was expected (e.g. `&&` at the
    /// start, or `)` without a matching `(`).
    UnexpectedToken,
    /// `(` was opened without a matching `)`.
    UnclosedParen,
    /// A valid expression was followed by more tokens (other than a `//`
    /// comment, which is silently consumed).
    UnexpectedTrailingTokens,
    /// A character that does not start any valid token.
    InvalidChar(char),
    /// A `(* ... *)` block comment appeared inside the expression. Matches
    /// FCS's `pplexExpectedSingleLineComment` error.
    BlockCommentNotAllowed,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ParseErrorKind::UnexpectedEof => {
                write!(f, "unexpected end of input at byte {}", self.at)
            }
            ParseErrorKind::UnexpectedToken => write!(f, "unexpected token at byte {}", self.at),
            ParseErrorKind::UnclosedParen => write!(f, "unclosed `(` at byte {}", self.at),
            ParseErrorKind::UnexpectedTrailingTokens => {
                write!(f, "unexpected trailing tokens at byte {}", self.at)
            }
            ParseErrorKind::InvalidChar(c) => {
                write!(f, "invalid character {c:?} at byte {}", self.at)
            }
            ParseErrorKind::BlockCommentNotAllowed => {
                write!(
                    f,
                    "`(* ... *)` block comment not allowed at byte {}",
                    self.at
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

impl std::ops::Not for Expr {
    type Output = Self;

    fn not(self) -> Self {
        Self::Not(Box::new(self))
    }
}

/// Parse a single `#if` / `#elif` expression body.
///
/// `input` is the expression text only — the `#if` / `#elif` prefix has
/// already been consumed by the caller.
pub fn parse_if_expr(input: &str) -> Result<Expr, ParseError> {
    let mut p = Parser::new(input);
    let expr = p.parse_or()?;
    p.expect_eof()?;
    Ok(expr)
}

// ============================================================================
// Tokenizer
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok<'a> {
    Ident(&'a str),
    Not,
    And,
    Or,
    LParen,
    RParen,
    Eof,
}

struct Tokenizer<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn peek_byte(&self, offset: usize) -> Option<u8> {
        self.input.as_bytes().get(self.pos + offset).copied()
    }

    fn next_token(&mut self) -> Result<(Tok<'a>, usize), ParseError> {
        // Skip ASCII space / tab. Newlines aren't expected (directives are
        // single-line; the caller strips them), but if one appears we treat
        // it as unrecognised input via the catch-all below.
        while matches!(self.peek_byte(0), Some(b' ' | b'\t')) {
            self.pos += 1;
        }

        let start = self.pos;
        let Some(b) = self.peek_byte(0) else {
            return Ok((Tok::Eof, start));
        };

        match b {
            b'!' => {
                self.pos += 1;
                Ok((Tok::Not, start))
            }
            b'&' => {
                if self.peek_byte(1) == Some(b'&') {
                    self.pos += 2;
                    Ok((Tok::And, start))
                } else {
                    Err(self.invalid_char_here())
                }
            }
            b'|' => {
                if self.peek_byte(1) == Some(b'|') {
                    self.pos += 2;
                    Ok((Tok::Or, start))
                } else {
                    Err(self.invalid_char_here())
                }
            }
            b'(' => {
                // FCS treats `(*` as a hard error
                // (pplexExpectedSingleLineComment). In its fslex rules
                // longest-match wins, so `(*` beats `(`.
                if self.peek_byte(1) == Some(b'*') {
                    return Err(ParseError {
                        at: start,
                        kind: ParseErrorKind::BlockCommentNotAllowed,
                    });
                }
                self.pos += 1;
                Ok((Tok::LParen, start))
            }
            b')' => {
                self.pos += 1;
                Ok((Tok::RParen, start))
            }
            b'/' => {
                if self.peek_byte(1) == Some(b'/') {
                    // Trailing line comment terminates the token stream.
                    self.pos = self.input.len();
                    Ok((Tok::Eof, start))
                } else {
                    Err(self.invalid_char_here())
                }
            }
            _ if is_ident_start(b) => {
                let mut end = self.pos + 1;
                while let Some(c) = self.input.as_bytes().get(end).copied()
                    && is_ident_continue(c)
                {
                    end += 1;
                }
                let ident = &self.input[self.pos..end];
                self.pos = end;
                Ok((Tok::Ident(ident), start))
            }
            _ => Err(self.invalid_char_here()),
        }
    }

    /// Form an `InvalidChar` error at the current position, reading the
    /// character (as a full UTF-8 codepoint) without advancing. Advancing
    /// past it isn't required: the parser stops on the first lex error.
    fn invalid_char_here(&self) -> ParseError {
        // `rest()` is guaranteed non-empty because the caller checked.
        let ch = self.rest().chars().next().expect("non-empty rest");
        ParseError {
            at: self.pos,
            kind: ParseErrorKind::InvalidChar(ch),
        }
    }
}

/// F# `#if` identifiers follow F# identifier rules (Unicode letter / digit /
/// `_` / `'`). Stage 1 restricts to ASCII; the corpus is ASCII-only and real
/// `#if` symbols are conventionally ASCII (`DEBUG`, `NETSTANDARD`, etc.). If
/// a later differential test against FCS diverges on a non-ASCII symbol,
/// extend here with `unicode-ident` or similar.
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'\''
}

// ============================================================================
// Parser
// ============================================================================

struct Parser<'a> {
    tok: Tokenizer<'a>,
    peeked: Option<(Tok<'a>, usize)>,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            tok: Tokenizer::new(input),
            peeked: None,
        }
    }

    fn peek(&mut self) -> Result<&(Tok<'a>, usize), ParseError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.tok.next_token()?);
        }
        Ok(self.peeked.as_ref().expect("just populated"))
    }

    fn bump(&mut self) -> Result<(Tok<'a>, usize), ParseError> {
        match self.peeked.take() {
            Some(t) => Ok(t),
            None => self.tok.next_token(),
        }
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_and()?;
        while matches!(self.peek()?.0, Tok::Or) {
            self.bump()?;
            let right = self.parse_and()?;
            left = Expr::or(left, right);
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_not()?;
        while matches!(self.peek()?.0, Tok::And) {
            self.bump()?;
            let right = self.parse_not()?;
            left = Expr::and(left, right);
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek()?.0, Tok::Not) {
            self.bump()?;
            let inner = self.parse_not()?;
            Ok(!inner)
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let (tok, pos) = self.bump()?;
        match tok {
            Tok::Ident(s) => Ok(Expr::Ident(s.to_string())),
            Tok::LParen => {
                let inner = self.parse_or()?;
                let (close, close_pos) = self.bump()?;
                match close {
                    Tok::RParen => Ok(inner),
                    Tok::Eof => Err(ParseError {
                        at: pos,
                        kind: ParseErrorKind::UnclosedParen,
                    }),
                    _ => Err(ParseError {
                        at: close_pos,
                        kind: ParseErrorKind::UnexpectedToken,
                    }),
                }
            }
            Tok::Eof => Err(ParseError {
                at: pos,
                kind: ParseErrorKind::UnexpectedEof,
            }),
            _ => Err(ParseError {
                at: pos,
                kind: ParseErrorKind::UnexpectedToken,
            }),
        }
    }

    fn expect_eof(&mut self) -> Result<(), ParseError> {
        let (tok, pos) = self.bump()?;
        match tok {
            Tok::Eof => Ok(()),
            _ => Err(ParseError {
                at: pos,
                kind: ParseErrorKind::UnexpectedTrailingTokens,
            }),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn id(s: &str) -> Expr {
        Expr::ident(s)
    }

    /// Evaluate against a fixed symbol set. Convenience helper used by
    /// example tests so call sites read as `eval_with(&e, &["DEFINED"])`.
    fn eval_with(e: &Expr, symbols: &[&str]) -> bool {
        let set: HashSet<&str> = symbols.iter().copied().collect();
        e.eval(|n| set.contains(n))
    }

    // ---- example tests: happy path ----

    #[test]
    fn parses_bare_identifier() {
        assert_eq!(parse_if_expr("FOO"), Ok(id("FOO")));
    }

    #[test]
    fn parses_underscore_and_apostrophe_identifiers() {
        assert_eq!(parse_if_expr("_FOO"), Ok(id("_FOO")));
        assert_eq!(parse_if_expr("foo'bar"), Ok(id("foo'bar")));
        assert_eq!(parse_if_expr("F123"), Ok(id("F123")));
    }

    #[test]
    fn treats_true_and_false_as_idents() {
        // Per ExtendedIfGrammar.fs e18/e19: `#if true` is `Ident("true")`,
        // not a boolean literal.
        assert_eq!(parse_if_expr("true"), Ok(id("true")));
        assert_eq!(parse_if_expr("false"), Ok(id("false")));
    }

    #[test]
    fn ignores_leading_and_trailing_whitespace() {
        assert_eq!(parse_if_expr("  FOO  "), Ok(id("FOO")));
        assert_eq!(parse_if_expr("\tFOO\t"), Ok(id("FOO")));
    }

    #[test]
    fn parses_unary_not() {
        assert_eq!(parse_if_expr("!FOO"), Ok(!id("FOO")));
        assert_eq!(parse_if_expr("! FOO"), Ok(!id("FOO")));
        assert_eq!(parse_if_expr("!!FOO"), Ok(!!id("FOO")));
        assert_eq!(parse_if_expr("!!!FOO"), Ok(!!!id("FOO")));
    }

    #[test]
    fn parses_and_and_or() {
        assert_eq!(parse_if_expr("A && B"), Ok(Expr::and(id("A"), id("B"))));
        assert_eq!(parse_if_expr("A || B"), Ok(Expr::or(id("A"), id("B"))));
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // A || B && C → Or(A, And(B, C))
        assert_eq!(
            parse_if_expr("A || B && C"),
            Ok(Expr::or(id("A"), Expr::and(id("B"), id("C"))))
        );
        // A && B || C → Or(And(A, B), C)
        assert_eq!(
            parse_if_expr("A && B || C"),
            Ok(Expr::or(Expr::and(id("A"), id("B")), id("C")))
        );
    }

    #[test]
    fn not_binds_tighter_than_and() {
        // !A && B → And(Not(A), B)
        assert_eq!(parse_if_expr("!A && B"), Ok(Expr::and(!id("A"), id("B"))));
    }

    #[test]
    fn and_and_or_are_left_associative() {
        // A && B && C → And(And(A, B), C)
        assert_eq!(
            parse_if_expr("A && B && C"),
            Ok(Expr::and(Expr::and(id("A"), id("B")), id("C")))
        );
        assert_eq!(
            parse_if_expr("A || B || C"),
            Ok(Expr::or(Expr::or(id("A"), id("B")), id("C")))
        );
    }

    #[test]
    fn parens_override_precedence() {
        // (A || B) && C → And(Or(A, B), C)
        assert_eq!(
            parse_if_expr("(A || B) && C"),
            Ok(Expr::and(Expr::or(id("A"), id("B")), id("C")))
        );
        // Redundant parens are transparent.
        assert_eq!(parse_if_expr("(FOO)"), Ok(id("FOO")));
        assert_eq!(parse_if_expr("((FOO))"), Ok(id("FOO")));
    }

    #[test]
    fn trailing_line_comment_terminates_expression() {
        assert_eq!(parse_if_expr("FOO // a comment"), Ok(id("FOO")));
        assert_eq!(
            parse_if_expr("A && B // tail"),
            Ok(Expr::and(id("A"), id("B")))
        );
    }

    // ---- example tests: error paths ----

    #[test]
    fn empty_input_errors() {
        let err = parse_if_expr("").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedEof);
        assert_eq!(err.at, 0);
    }

    #[test]
    fn whitespace_only_input_errors() {
        let err = parse_if_expr("   ").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedEof);
    }

    #[test]
    fn leading_operator_errors() {
        let err = parse_if_expr("&& FOO").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedToken);
        assert_eq!(err.at, 0);
    }

    #[test]
    fn dangling_not_errors() {
        let err = parse_if_expr("!").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedEof);
    }

    #[test]
    fn dangling_and_errors() {
        let err = parse_if_expr("FOO &&").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedEof);
    }

    #[test]
    fn unclosed_paren_errors() {
        let err = parse_if_expr("(FOO").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnclosedParen);
        assert_eq!(err.at, 0);
    }

    #[test]
    fn unmatched_close_paren_errors() {
        let err = parse_if_expr(")").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedToken);
        assert_eq!(err.at, 0);
    }

    #[test]
    fn trailing_tokens_after_complete_expression_errors() {
        let err = parse_if_expr("FOO BAR").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::UnexpectedTrailingTokens);
    }

    #[test]
    fn block_comment_inside_expression_errors() {
        let err = parse_if_expr("(* nope *)").unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::BlockCommentNotAllowed);
        assert_eq!(err.at, 0);
    }

    #[test]
    fn invalid_char_errors() {
        let err = parse_if_expr("@FOO").unwrap_err();
        assert!(matches!(err.kind, ParseErrorKind::InvalidChar('@')));
        assert_eq!(err.at, 0);
    }

    #[test]
    fn lone_ampersand_or_pipe_errors() {
        assert!(matches!(
            parse_if_expr("A & B").unwrap_err().kind,
            ParseErrorKind::InvalidChar('&')
        ));
        assert!(matches!(
            parse_if_expr("A | B").unwrap_err().kind,
            ParseErrorKind::InvalidChar('|')
        ));
    }

    // ---- canonical-form sanity ----

    #[test]
    fn canonical_form_is_fully_parenthesised() {
        let e = Expr::and(Expr::or(id("A"), id("B")), !id("C"));
        assert_eq!(e.to_canonical_string(), "((A || B) && (!C))");
    }

    // ---- eval: structural example tests ----

    #[test]
    fn eval_ident_is_lookup_membership() {
        assert!(eval_with(&id("X"), &["X"]));
        assert!(!eval_with(&id("X"), &[]));
        assert!(!eval_with(&id("X"), &["Y", "Z"]));
    }

    #[test]
    fn eval_is_case_sensitive() {
        // F# define constants are case-sensitive; `DEBUG` and `Debug` are
        // distinct symbols.
        assert!(!eval_with(&id("DEBUG"), &["debug"]));
        assert!(eval_with(&id("Debug"), &["Debug"]));
    }

    /// Each entry is `(name, expression source, expected eval under
    /// {"DEFINED"})`. Drawn from FCS's `ExtendedIfGrammar.fs` (e0..e23),
    /// where the active branch in the fixture pins the expected boolean:
    /// `#if E … #else other` — if the active body is `0` (not `failwith`)
    /// then `E` evaluates true under `{DEFINED}`; otherwise false.
    ///
    /// e18 / e19 confirm `true` / `false` are ordinary identifiers (per
    /// the FCS comment "true/false are seen as identifiers not values"),
    /// so both evaluate to false when neither is in the symbol set.
    const EXTENDED_GRAMMAR_CASES: &[(&str, &str, bool)] = &[
        ("e0", "DEFINED", true),
        ("e1", "UNDEFINED", false),
        ("e2", "DEFINED && UNDEFINED", false),
        ("e3", "UNDEFINED && DEFINED", false),
        ("e4", "DEFINED || UNDEFINED", true),
        ("e5", "UNDEFINED || DEFINED", true),
        ("e6", "!UNDEFINED", true),
        ("e7", "!DEFINED", false),
        ("e8", "!UNDEFINED || DEFINED", true),
        ("e9", "!DEFINED && DEFINED", false),
        ("e10", "DEFINED && DEFINED && UNDEFINED", false),
        ("e11", "UNDEFINED || UNDEFINED || DEFINED", true),
        ("e12", "DEFINED || DEFINED && UNDEFINED", true),
        ("e13", "UNDEFINED && DEFINED || DEFINED", true),
        ("e14", "(DEFINED)", true),
        ("e15", "(DEFINED || DEFINED) && UNDEFINED", false),
        ("e16", "UNDEFINED && (DEFINED || DEFINED)", false),
        ("e17", "DEFINED // A test comment", true),
        ("e18", "true", false),
        ("e19", "false", false),
        ("e20", "!!DEFINED", true),
        ("e21", "!!!DEFINED", false),
        ("e22", "!!UNDEFINED", false),
        ("e23", "!!!UNDEFINED", true),
    ];

    #[test]
    fn eval_matches_fcs_extended_if_grammar() {
        for &(label, source, expected) in EXTENDED_GRAMMAR_CASES {
            let parsed = parse_if_expr(source)
                .unwrap_or_else(|err| panic!("{label}: parse {source:?}: {err}"));
            let got = eval_with(&parsed, &["DEFINED"]);
            assert_eq!(
                got, expected,
                "{label}: eval({source:?}, {{DEFINED}}) = {got}, expected {expected}"
            );
        }
    }

    // ---- property tests ----

    /// Generates ASCII identifiers per the lexer's accepted shape.
    fn arb_ident() -> impl Strategy<Value = String> {
        "[A-Za-z_][A-Za-z0-9_']{0,15}"
    }

    fn arb_expr() -> impl Strategy<Value = Expr> {
        let leaf = arb_ident().prop_map(Expr::Ident);
        leaf.prop_recursive(
            5,  // depth
            32, // total nodes
            3,  // branching factor at each recursion
            |inner| {
                prop_oneof![
                    inner.clone().prop_map(|e| !e),
                    (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::and(a, b)),
                    (inner.clone(), inner).prop_map(|(a, b)| Expr::or(a, b)),
                ]
            },
        )
    }

    /// Eval-focused generator: identifiers are drawn from a 4-name universe
    /// (`A`, `B`, `C`, `D`) so that a random subset has a meaningful chance
    /// of containing each. The structural generator above uses random
    /// `[A-Za-z_][...]{0,15}` identifiers — fine for round-tripping, but
    /// almost every random ident would miss a random `HashSet<String>`,
    /// driving eval uniformly to false and starving the algebraic
    /// properties of useful evidence.
    fn arb_small_expr() -> impl Strategy<Value = Expr> {
        let leaf = prop_oneof![
            Just(Expr::ident("A")),
            Just(Expr::ident("B")),
            Just(Expr::ident("C")),
            Just(Expr::ident("D")),
        ];
        leaf.prop_recursive(5, 32, 3, |inner| {
            prop_oneof![
                inner.clone().prop_map(|e| !e),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::and(a, b)),
                (inner.clone(), inner).prop_map(|(a, b)| Expr::or(a, b)),
            ]
        })
    }

    /// Symbol set as a 4-bit mask over `{A, B, C, D}`. 16 possible sets,
    /// proptest will explore each.
    fn arb_symbols() -> impl Strategy<Value = HashSet<String>> {
        (0u8..16).prop_map(|mask| {
            let mut set = HashSet::new();
            for (bit, name) in ["A", "B", "C", "D"].iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    set.insert((*name).to_string());
                }
            }
            set
        })
    }

    fn ev(e: &Expr, s: &HashSet<String>) -> bool {
        e.eval(|n| s.contains(n))
    }

    proptest! {
        /// Round-trip: any `Expr` we can build re-parses from its canonical
        /// form back into itself. This is the central correctness property.
        #[test]
        fn canonical_string_round_trips(e in arb_expr()) {
            let s = e.to_canonical_string();
            let parsed = parse_if_expr(&s).expect("canonical form must parse");
            prop_assert_eq!(parsed, e);
        }

        /// Totality: the parser never panics on arbitrary short ASCII input.
        /// Stage 1 is restricted to ASCII; non-ASCII input is rejected as
        /// `InvalidChar`, so an ASCII corpus is sufficient.
        #[test]
        fn parser_is_total_on_ascii(s in "[\\x09\\x20-\\x7e]{0,40}") {
            let _ = parse_if_expr(&s);
        }

        /// Whitespace insensitivity for the canonical form: padding any
        /// canonical string with spaces and tabs (in places where they're
        /// allowed — i.e. anywhere outside an identifier) doesn't change
        /// the parse. We exercise this by injecting whitespace at the start
        /// and end, which are always permitted.
        #[test]
        fn surrounding_whitespace_is_ignored(e in arb_expr()) {
            let s = e.to_canonical_string();
            let padded = format!(" \t  {s}  \t ");
            prop_assert_eq!(parse_if_expr(&padded).unwrap(), e);
        }

        /// Trailing line comments are dropped before the parser sees them.
        #[test]
        fn trailing_line_comment_is_ignored(e in arb_expr()) {
            let s = e.to_canonical_string();
            let with_comment = format!("{s} // a trailing comment with && || ! ( )");
            prop_assert_eq!(parse_if_expr(&with_comment).unwrap(), e);
        }

        // ---- eval: algebraic laws ----
        //
        // These check that `eval` faithfully realises Boolean algebra over
        // the AST. Each law rewrites the expression into an equivalent
        // shape; if eval agrees on both shapes for every symbol set, the
        // implementation is preserving the algebra.

        /// Double negation: `!!e ≡ e`.
        #[test]
        fn eval_double_negation(e in arb_small_expr(), s in arb_symbols()) {
            prop_assert_eq!(ev(&!!e.clone(), &s), ev(&e, &s));
        }

        /// De Morgan: `!(a && b) ≡ !a || !b`.
        #[test]
        fn eval_de_morgan_and(a in arb_small_expr(), b in arb_small_expr(), s in arb_symbols()) {
            let lhs = !Expr::and(a.clone(), b.clone());
            let rhs = Expr::or(!a, !b);
            prop_assert_eq!(ev(&lhs, &s), ev(&rhs, &s));
        }

        /// De Morgan dual: `!(a || b) ≡ !a && !b`.
        #[test]
        fn eval_de_morgan_or(a in arb_small_expr(), b in arb_small_expr(), s in arb_symbols()) {
            let lhs = !Expr::or(a.clone(), b.clone());
            let rhs = Expr::and(!a, !b);
            prop_assert_eq!(ev(&lhs, &s), ev(&rhs, &s));
        }

        /// Commutativity of `&&`.
        #[test]
        fn eval_and_is_commutative(
            a in arb_small_expr(),
            b in arb_small_expr(),
            s in arb_symbols(),
        ) {
            prop_assert_eq!(
                ev(&Expr::and(a.clone(), b.clone()), &s),
                ev(&Expr::and(b, a), &s),
            );
        }

        /// Commutativity of `||`.
        #[test]
        fn eval_or_is_commutative(
            a in arb_small_expr(),
            b in arb_small_expr(),
            s in arb_symbols(),
        ) {
            prop_assert_eq!(
                ev(&Expr::or(a.clone(), b.clone()), &s),
                ev(&Expr::or(b, a), &s),
            );
        }

        /// Distributivity of `&&` over `||`: `a && (b || c) ≡ (a && b) || (a && c)`.
        #[test]
        fn eval_and_distributes_over_or(
            a in arb_small_expr(),
            b in arb_small_expr(),
            c in arb_small_expr(),
            s in arb_symbols(),
        ) {
            let lhs = Expr::and(a.clone(), Expr::or(b.clone(), c.clone()));
            let rhs = Expr::or(Expr::and(a.clone(), b), Expr::and(a, c));
            prop_assert_eq!(ev(&lhs, &s), ev(&rhs, &s));
        }

        /// Idempotence of `&&`: `a && a ≡ a`.
        #[test]
        fn eval_and_is_idempotent(e in arb_small_expr(), s in arb_symbols()) {
            prop_assert_eq!(ev(&Expr::and(e.clone(), e.clone()), &s), ev(&e, &s));
        }

        /// Idempotence of `||`: `a || a ≡ a`.
        #[test]
        fn eval_or_is_idempotent(e in arb_small_expr(), s in arb_symbols()) {
            prop_assert_eq!(ev(&Expr::or(e.clone(), e.clone()), &s), ev(&e, &s));
        }

        /// Eval depends only on which identifiers are in the symbol set,
        /// not on the names themselves: a uniform renaming of identifiers
        /// together with the symbol set preserves the result. This pins
        /// down that `eval` looks at nothing except `lookup(name)`.
        #[test]
        fn eval_is_name_agnostic(e in arb_small_expr(), s in arb_symbols()) {
            // Rename A↔W, B↔X, C↔Y, D↔Z in both the expression and the
            // symbol set; eval should be unchanged.
            fn rename(e: &Expr) -> Expr {
                match e {
                    Expr::Ident(n) => Expr::ident(match n.as_str() {
                        "A" => "W",
                        "B" => "X",
                        "C" => "Y",
                        "D" => "Z",
                        other => other,
                    }),
                    Expr::Not(inner) => !rename(inner),
                    Expr::And(a, b) => Expr::and(rename(a), rename(b)),
                    Expr::Or(a, b) => Expr::or(rename(a), rename(b)),
                }
            }
            let renamed_set: HashSet<String> = s
                .iter()
                .map(|n| match n.as_str() {
                    "A" => "W".to_string(),
                    "B" => "X".to_string(),
                    "C" => "Y".to_string(),
                    "D" => "Z".to_string(),
                    other => other.to_string(),
                })
                .collect();
            prop_assert_eq!(ev(&rename(&e), &renamed_set), ev(&e, &s));
        }
    }
}

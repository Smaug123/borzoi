//! Tokeniser for F# source. First slice of a port of `lex.fsl`.
//!
//! Scope:
//! - Keywords, identifiers (incl. ``backtick-quoted``), integer/float/decimal/bignum literals,
//!   strings (single, verbatim, triple) and char literals — both top-level and as
//!   embedded forms inside `(* ... *)` block comments, so closer detection isn't fooled
//!   by `*)` inside a string.
//! - Single- and (nestable) multi-line comments.
//! - The punctuation/operator table, explicit whitespace + newline tokens.
//!
//! Out of scope for now (callers should expect parser-level handling once added):
//! - Interpolated strings (`$"..."`, `$$"""..."""`)
//! - Preprocessor and `#line` directives — `#` lexes as `Hash`, and recognising
//!   `#if`/`#else`/`#endif`/`#nowarn`/`#line` is a higher-layer concern (the
//!   sequence `#` + ident or `#` + int reaches the parser intact).
//! - `<@ @>` / `<@@ @@>` code-quotation markers
//! - XML doc comments as a distinct token (`///` is currently emitted as `LineComment`)
//! - The full operator-classification taxonomy from `lex.fsl`
//!   (`INFIX_STAR_STAR_OP`, `INFIX_AT_HAT_OP`, etc.) — emitted as `Op(&str)` for now.
//!
//! The token stream is offside-aware-ready: whitespace and newlines are first-class tokens
//! so that a future `LexFilter` port can drive indentation handling without re-tokenising.

mod callbacks;
pub mod interp;
#[cfg(test)]
mod tests;

use callbacks::{
    lex_block_comment, lex_interp_extended_opener, lex_interp_opener, lex_interp_triple_opener,
    lex_interp_verbatim_opener, lex_single_string, lex_triple_string, lex_verbatim_string,
};
use logos::Logos;
use std::ops::Range;

pub type Span = Range<usize>;

/// F#'s ML-compatibility *reserved words*: identifiers the language keeps
/// reserved for possible future use. FCS's keyword table maps each to a
/// `RESERVED` token, but `KeywordOrIdentifierToken` immediately turns that back
/// into an ordinary `IDENT` after emitting an FS0046 *warning* ("The identifier
/// '…' is reserved for future use by F#"); the parser therefore only ever sees
/// an identifier and `ParseHadErrors` stays `false`
/// (`dotnet/fsharp/src/Compiler/SyntaxTree/LexHelpers.fs`).
///
/// We mirror that by lexing these as plain [`Token::Ident`]; the FS0046 warning
/// is recovered by a lexeme scan over the token stream (`reserved_ident_diagnostics`
/// in the parser layer). Kept in source-sorted order for readability — callers
/// use [`is_reserved_ident`] rather than assuming an order.
pub const RESERVED_IDENTS: &[&str] = &[
    "break",
    "checked",
    "component",
    "constraint",
    "continue",
    "fori",
    "include",
    "mixin",
    "parallel",
    "params",
    "process",
    "protected",
    "pure",
    "sealed",
    "tailcall",
    "trait",
    "virtual",
];

/// Is `word` one of F#'s ML-compatibility [`RESERVED_IDENTS`] — a bare
/// identifier that FCS accepts with an FS0046 warning?
pub fn is_reserved_ident(word: &str) -> bool {
    RESERVED_IDENTS.contains(&word)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum LexError {
    UnterminatedString,
    UnterminatedComment,
    #[default]
    Unknown,
}

/// Which fragment of an interpolated string a [`Token::InterpString`] represents.
///
/// FCS has four distinct tokens for these (`INTERP_STRING_BEGIN_END`,
/// `INTERP_STRING_BEGIN`, `INTERP_STRING_PART`, `INTERP_STRING_END`,
/// `pars.fsy:7055-7092`); the public `FSharpLexer` API collapses all four to
/// `FSharpTokenKind.String` (`ServiceLexing.fs:1573-1577`). We use a single
/// `Token::InterpString` variant carrying this discriminator because Logos
/// can't dispatch one `#[token("$\"")]` regex to multiple variants — but the
/// driver layer also synthesises `Part`/`End` after a fill closes, so the
/// 4-way enum is the natural shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterpKind {
    /// `$"hello"` — bare single-quoted interpolated string with no `{` fills.
    /// Lexer-emitted. FCS: `INTERP_STRING_BEGIN_END` with
    /// `SynStringKind.Regular`. Span covers `$"..."` inclusive of quotes.
    ///
    /// `is_byte` records a trailing `B` suffix (`$"hello"B`). FCS rejects
    /// interpolated byte strings (FS3377) and downgrades the token to a
    /// `BYTEARRAY`; we keep the byte fact here and let the parser recover
    /// to `SynConst.Bytes` with the diagnostic (detected in
    /// `callbacks::lex_interp_opener`).
    BeginEnd { is_byte: bool },
    /// `$"hello {` — opener of a single-quoted interpolated string with at
    /// least one fill. Lexer-emitted. Span includes the opening `$"` *and*
    /// the trailing `{`. FCS: `INTERP_STRING_BEGIN` with
    /// `SynStringKind.Regular`. No byte suffix is possible at an opener.
    Begin,
    /// `} world {` — closer of one fill, opener of the next, in a multi-fill
    /// chain. Driver-emitted only (Logos never sees this in isolation). Span
    /// includes the bracketing `}` *and* `{`. FCS: `INTERP_STRING_PART`.
    /// Style-agnostic: the same variant is synthesised for single- and
    /// triple-quoted parents; the driver carries the style on its frame
    /// stack rather than encoding it on every continuation token. A `Part`
    /// reopens a fill rather than closing the string, so it never carries a
    /// byte suffix.
    Part,
    /// `} world"` — closer of the final fill. Driver-emitted only. Span
    /// includes the bracketing `}` and the closing `"` (single-quoted) or
    /// `"""` (triple-quoted), plus a trailing `B` when `is_byte`. Same
    /// style-agnostic note as `Part`. FCS: `INTERP_STRING_END`. `is_byte`
    /// records a `}…"B` / `}…"""B` closer; FCS has no recovery for a byte
    /// suffix on a fill-bearing interp string, so the parser emits FS3377
    /// but keeps the ordinary interp shape.
    End { is_byte: bool },
    /// `$"""hello"""` — bare triple-quoted interpolated string with no
    /// `{` fills. Lexer-emitted. Distinct from `BeginEnd` so the parser
    /// can attach `SynStringKind.TripleQuote` and the fragment decoder
    /// can strip the 4-byte / 3-byte delimiters. `is_byte` records a
    /// trailing `B` (`$"""hello"""B`); see `BeginEnd`.
    TripleBeginEnd { is_byte: bool },
    /// `$"""hello {` — opener of a triple-quoted interp with at least one
    /// fill. Lexer-emitted. Span includes the opening `$"""` *and* the
    /// trailing `{`. Continuation tokens (`Part`/`End`) carry no style
    /// flag — the driver knows from the matching frame. No byte suffix at
    /// an opener.
    TripleBegin,
    /// `$@"hello"` / `@$"hello"` — bare verbatim interpolated string with
    /// no `{` fills. Lexer-emitted. FCS lexes the `$@"` / `@$"` opener
    /// (`lex.fsl:687`) to `LexerStringStyle.Verbatim`; the resulting
    /// `SynExpr.InterpolatedString` carries `SynStringKind.Verbatim`.
    /// Verbatim bodies use the `""`→`"` quote escape and have no backslash
    /// escapes. Span covers `$@"..."` inclusive of the opener and closer.
    ///
    /// `is_byte` records a trailing `B` (`$@"hello"B`); FCS rejects it
    /// (FS3377) and recovers `SynConst.Bytes(_, SynByteStringKind.Verbatim,
    /// _)` — see `BeginEnd`.
    VerbatimBeginEnd { is_byte: bool },
    /// `$@"hello {` / `@$"hello {` — opener of a verbatim interpolated
    /// string with at least one fill. Lexer-emitted. Span includes the
    /// opening `$@"` / `@$"` *and* the trailing `{`. Continuation tokens
    /// (`Part`/`End`) carry no style flag — the driver knows from the
    /// matching frame. No byte suffix at an opener. There is no
    /// triple-quoted verbatim interp (the `@` makes it single-`"`-delimited
    /// with the `""` escape).
    VerbatimBegin,
    /// `$$"""hello"""` (and `$$$"""…"""`, …) — bare extended (bracket-count)
    /// interpolated string with no fills. Lexer-emitted. FCS's
    /// `('$'+) '"' '"' '"'` rule (`lex.fsl:620`) fires for **N≥2** leading
    /// `$`; `n` records that count (the *interpolation delimiter length* —
    /// the number of `{` that open a fill and `}` that close it). The body
    /// is triple-like (closer `"""`, newlines content, no backslash escape)
    /// but with **no** `{{`/`}}` digraph: a `{`/`}` run shorter than `n` is
    /// literal content. The AST kind is `SynStringKind.TripleQuote`
    /// (`lex.fsl:1645`) — extended is not distinguished from `$"""…"""` in
    /// the tree. Unlike single/triple/verbatim interp, a trailing `B` is
    /// **not** a byte suffix (the `extendedInterpolatedString` closer at
    /// `lex.fsl:1641` has no byte arm), so there is no `is_byte` here — the
    /// `B` lexes as a separate identifier.
    ExtendedBeginEnd { n: usize },
    /// `$$"""hello{{` (and `$$$"""…{{{`, …) — opener of an extended interp
    /// string with at least one fill. Lexer-emitted. `n` is the delimiter
    /// length (count of leading `$`). Span includes the opening `$$"""` *and*
    /// the whole opening `{`-run (run ≥ `n`; the leading `run-n` braces are
    /// literal content, the last `n` open the fill — or, for a run ≥ `2n`,
    /// all are consumed and the parser emits FS1248). Continuation tokens
    /// (`Part`/`End`) carry no style flag — the driver knows `n` from the
    /// matching frame.
    ExtendedBegin { n: usize },
}

#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(error = LexError)]
pub enum Token<'a> {
    // ---- trivia -------------------------------------------------------------
    /// Spaces, tabs, and a leading UTF-8 BOM (which F# tolerates at file start).
    #[regex(r"[ \t\u{FEFF}]+")]
    Whitespace,

    #[regex(r"\r\n|\n|\r")]
    Newline,

    /// `// ...` and `/// ...` to end-of-line. We don't distinguish XML doc
    /// comments yet; both forms lex to the same token. Using a regex that
    /// covers the whole line (rather than a `//` literal + callback) ensures
    /// we beat the generic `Op` regex on lines that start with `//<` etc.
    ///
    /// `#! ...` shebang lines on `.fsx` scripts are also folded in here. See
    /// `lex.fsl` (`"#!" op_char*` → routes to `singleLineComment`). The F#
    /// compiler restricts shebangs to the first line of a file; we don't —
    /// out-of-position `#!` isn't valid F# code anyway, so accepting it as a
    /// comment is harmless and avoids a positional anchor Logos can't express.
    #[regex(r"//[^\n\r]*")]
    #[regex(r"#![^\n\r]*")]
    LineComment,

    #[token("(*", lex_block_comment)]
    BlockComment,

    // ---- keywords (ALWAYS) --------------------------------------------------
    #[token("and")]
    And,
    #[token("as")]
    As,
    #[token("assert")]
    Assert,
    #[token("base")]
    Base,
    #[token("begin")]
    Begin,
    #[token("class")]
    Class,
    #[token("do")]
    Do,
    #[token("done")]
    Done,
    #[token("downto")]
    DownTo,
    #[token("else")]
    Else,
    #[token("end")]
    End,
    #[token("exception")]
    Exception,
    #[token("false")]
    False,
    #[token("finally")]
    Finally,
    #[token("for")]
    For,
    #[token("fun")]
    Fun,
    #[token("function")]
    Function,
    #[token("if")]
    If,
    #[token("in")]
    In,
    #[token("inherit")]
    Inherit,
    #[token("lazy")]
    Lazy,
    #[token("let")]
    Let,
    #[token("match")]
    Match,
    #[token("mod")]
    Mod,
    #[token("module")]
    Module,
    #[token("mutable")]
    Mutable,
    #[token("new")]
    New,
    #[token("of")]
    Of,
    #[token("open")]
    Open,
    #[token("or")]
    Or,
    #[token("private")]
    Private,
    #[token("rec")]
    Rec,
    #[token("sig")]
    Sig,
    #[token("struct")]
    Struct,
    #[token("then")]
    Then,
    #[token("to")]
    To,
    #[token("true")]
    True,
    #[token("try")]
    Try,
    #[token("type")]
    Type,
    #[token("val")]
    Val,
    #[token("when")]
    When,
    #[token("while")]
    While,
    #[token("with")]
    With,
    #[token("_", priority = 3)]
    Underscore,

    // ---- keywords (F#-specific) --------------------------------------------
    #[token("abstract")]
    Abstract,
    #[token("const")]
    Const,
    #[token("default")]
    Default,
    #[token("delegate")]
    Delegate,
    #[token("downcast")]
    Downcast,
    #[token("elif")]
    Elif,
    #[token("extern")]
    Extern,
    #[token("fixed")]
    Fixed,
    #[token("global")]
    Global,
    #[token("inline")]
    Inline,
    #[token("interface")]
    Interface,
    #[token("internal")]
    Internal,
    #[token("member")]
    Member,
    #[token("namespace")]
    Namespace,
    #[token("null")]
    Null,
    #[token("override")]
    Override,
    #[token("public")]
    Public,
    #[token("return")]
    Return,
    #[token("static")]
    Static,
    #[token("upcast")]
    Upcast,
    #[token("use")]
    Use,
    #[token("void")]
    Void,
    #[token("yield")]
    Yield,

    // ---- bang forms --------------------------------------------------------
    #[token("do!")]
    DoBang,
    #[token("yield!")]
    YieldBang,
    #[token("return!")]
    ReturnBang,
    #[token("match!")]
    MatchBang,
    #[token("and!")]
    AndBang,
    #[token("let!")]
    LetBang,
    #[token("use!")]
    UseBang,
    #[token("while!")]
    WhileBang,

    // ---- keyword strings ---------------------------------------------------
    /// `__SOURCE_DIRECTORY__`, `__SOURCE_FILE__`, `__LINE__` — three magic
    /// identifiers that FCS surfaces as `KEYWORD_STRING` rather than `IDENT`
    /// (`LexHelpers.fs:434-436`). At parse time they expand to the literal
    /// source directory / file / line; FCS substitutes the value via
    /// `getSourceIdentifierValue` while lexing. We only emit the source
    /// spelling — substitution is the consumer's job, since byte spans alone
    /// don't carry the file path or 1-based line.
    ///
    /// The literals are length 20 / 16 / 8, all longer than the explicit
    /// `priority = 2` on `Ident`, so logos picks the keyword-string variant
    /// without further hinting.
    #[token("__SOURCE_DIRECTORY__")]
    #[token("__SOURCE_FILE__")]
    #[token("__LINE__")]
    KeywordString(&'a str),

    // ---- identifiers --------------------------------------------------------
    /// Unicode-aware identifier matching `ident_start_char ident_char*` from
    /// `lex.fsl`. Trailing `'` (prime) is permitted. We use `priority = 2` so
    /// keyword tokens (literal `#[token("...")]`) win on length ties.
    #[regex(r"[\p{L}\p{Nl}_][\p{L}\p{N}\p{Mn}\p{Mc}\p{Pc}\p{Cf}']*", priority = 2)]
    Ident(&'a str),

    /// `` ``ident with spaces`` ``
    #[regex(r"``(?:[^`\n\r\t]|`[^`\n\r\t])+``")]
    QuotedIdent(&'a str),

    // ---- numeric literals ---------------------------------------------------
    // Order: more specific (longer, suffixed) before less specific. Logos picks
    // the longest match, so explicit priorities mostly aren't required.
    //
    // Known divergence from `lex.fsl`: the strict `digit ((digit|sep)* digit)?`
    // shape would reject trailing-separator forms like `1_`, `0xFF_`, `1e_`.
    // We accept them as a single numeric token instead. Attempts to encode the
    // strict shape (`D+(?:_+D+)*` or `D([D_]*D)?`) silently miscompile in
    // Logos 0.14 when the Ident regex is present — the DFA merger lets `_`
    // tail-extend a digit-run, so `1_` lexes as `Int("1_")` regardless. The
    // theoretical fix is a callback that bumps back trailing `_`s, but Logos
    // doesn't support negative-bump; trailing-`_` literals don't appear in
    // real F# source (the corpus walks ~6000 files with zero such cases), so
    // we accept the looseness and let the parser layer reject if it cares.
    /// `0xCAFEBABE`, `0b1010`, `0o755`. Optional digit separators.
    #[regex(r"0[xX][0-9A-Fa-f][0-9A-Fa-f_]*")]
    #[regex(r"0[oO][0-7][0-7_]*")]
    #[regex(r"0[bB][01][01_]*")]
    XInt(&'a str),

    /// Suffix variants of the above (`0x..L`, `0x..un`, …). Split by base so
    /// the digit sets match `xinteger` in `lex.fsl` — a loose `[0-9A-Fa-f_]+`
    /// would happily accept malformed inputs like `0b102uy` or `0o9L`.
    #[regex(
        r"(0[xX][0-9A-Fa-f][0-9A-Fa-f_]*|0[oO][0-7][0-7_]*|0[bB][01][01_]*)(y|uy|s|us|l|u|ul|n|un|L|UL|uL)"
    )]
    XIntSuffixed(&'a str),

    /// Bit-pattern float forms. `xieee32 = xinteger 'l' 'f'` in `lex.fsl`, so
    /// the integer body can be any of the three bases — `0b...lf` and `0o...LF`
    /// are valid as well as `0x...`.
    #[regex(r"(0[xX][0-9A-Fa-f][0-9A-Fa-f_]*|0[oO][0-7][0-7_]*|0[bB][01][01_]*)lf")]
    XIEEE32(&'a str),
    #[regex(r"(0[xX][0-9A-Fa-f][0-9A-Fa-f_]*|0[oO][0-7][0-7_]*|0[bB][01][01_]*)LF")]
    XIEEE64(&'a str),

    /// Decimal integers with optional underscores and a required suffix.
    #[regex(r"[0-9][0-9_]*(y|uy|s|us|l|u|ul|n|un|L|UL|uL)")]
    IntSuffixed(&'a str),

    /// `123I`, `42N` — arbitrary-precision / numeric-literal suffixes.
    #[regex(r"[0-9][0-9_]*[INZQRG]")]
    BigNum(&'a str),

    /// `1.0m`, `1m`, `1e10m` — `System.Decimal`.
    #[regex(r"([0-9][0-9_]*\.[0-9_]*([eE][+\-]?[0-9_]+)?|[0-9][0-9_]*[eE][+\-]?[0-9_]+|[0-9][0-9_]*)[mM]")]
    Decimal(&'a str),

    /// `1.0f`, `1.0e5f` — IEEE32 (dotted/exponent form).
    #[regex(r"([0-9][0-9_]*\.[0-9_]*([eE][+\-]?[0-9_]+)?|[0-9][0-9_]*[eE][+\-]?[0-9_]+)[fF]")]
    /// Dotless f32 literal (`LanguageFeature.DotlessFloat32Literal`).
    #[regex(r"[0-9][0-9_]*[fF]")]
    Float32(&'a str),

    /// `1.0`, `1e10`, `1.5e-3` — IEEE64.
    #[regex(r"[0-9][0-9_]*\.[0-9_]*([eE][+\-]?[0-9_]+)?")]
    #[regex(r"[0-9][0-9_]*[eE][+\-]?[0-9_]+")]
    Float64(&'a str),

    /// `int '..'` — decimal int immediately followed by the range operator.
    /// Mirrors lex.fsl's `INT32_DOT_DOT` special case (line 403): without this,
    /// `1..10` lexes as `Float64("1.")`, `Dot`, `Int("10")` because the float
    /// regex would consume `1.` before `DotDot` gets a chance. Logos longest-
    /// match picks this 3+-char variant over the 2-char `1.` float.
    #[regex(r"[0-9][0-9_]*\.\.")]
    IntDotDot(&'a str),

    /// Bare decimal integer. Must lose to all the suffixed variants above on
    /// longer matches; if input is just `123`, only this regex fires.
    #[regex(r"[0-9][0-9_]*")]
    Int(&'a str),

    // ---- char & string literals --------------------------------------------
    /// `'a'`, `'\n'`, `'\\'`, `'''` (apostrophe), `'\000'` (trigraph),
    /// `'\x7F'`, `'ꯍ'`, `'\U0001F600'`, plus the byte forms `'a'B`.
    ///
    /// The unescaped body class mirrors FCS's `lex.fsl:305` `char` rule
    /// (`[^'\\''\n''\r''\t''\b']`): it excludes backslash, the layout chars
    /// (newline, CR, tab), and backspace (U+0008) — but *not* the apostrophe,
    /// so `'''` is the char literal for `'` (the middle `'` is an ordinary
    /// body char). An escaped apostrophe (`'\''`) matches the first alternative.
    #[regex(r#"'(\\[\\"'afvntbr]|[^\\\n\r\t\u{8}])'B?"#)]
    #[regex(r#"'\\[0-9][0-9][0-9]'B?"#)]
    #[regex(r#"'\\x[0-9A-Fa-f][0-9A-Fa-f]'B?"#)]
    #[regex(r#"'\\u[0-9A-Fa-f]{4}'B?"#)]
    #[regex(r#"'\\U[0-9A-Fa-f]{8}'B?"#)]
    Char(&'a str),

    /// `"""..."""`. The triple-quoted *interpolated* form `$"""..."""`
    /// is routed separately to [`Token::InterpString`] via
    /// `lex_interp_triple_opener` (Logos's longest-literal-wins
    /// resolves the ambiguity between `$"""` and `$"`).
    #[token(r#"""""#, lex_triple_string)]
    TripleString,

    /// `@"..."`. Only escape is `""`.
    #[token("@\"", lex_verbatim_string)]
    VerbatimString,

    /// `"..."`.
    #[token("\"", lex_single_string)]
    String,

    /// Interpolated-string fragment. The [`InterpKind`] payload
    /// distinguishes the FCS shapes recognised by the lexer + driver
    /// (`BeginEnd`, `Begin`, `Part`, `End`, `TripleBeginEnd`,
    /// `TripleBegin`, `VerbatimBeginEnd`, `VerbatimBegin`,
    /// `ExtendedBeginEnd`, `ExtendedBegin`); see the `InterpKind` doc. The
    /// Logos arms below emit openers only — single-quoted (`$"`) →
    /// `BeginEnd` or `Begin`; triple-quoted (`$"""`) → `TripleBeginEnd` or
    /// `TripleBegin`; verbatim (`$@"` / `@$"`) → `VerbatimBeginEnd` or
    /// `VerbatimBegin`; extended (`$$"""`, ≥2 `$`) → `ExtendedBeginEnd` or
    /// `ExtendedBegin`. `Part` and `End` are constructed by the `interp`
    /// state-machine wrapper
    /// around the raw Logos stream — they fire when a fill's matching `}`
    /// is observed in the parent expression stream, at which point the
    /// wrapper byte-walks the next string fragment without re-engaging
    /// Logos. The continuation variants are style-agnostic; the driver's
    /// frame stack carries the style so the byte-walker can pick the
    /// right closer and escape rules.
    ///
    /// Logos's longest-literal-wins rule routes `$"""` to
    /// `lex_interp_triple_opener` in preference to the `$"` arm, and the
    /// 3-char `$@"` / `@$"` verbatim literals beat the 1-char `$`
    /// (`Dollar`) and the `@`/`@$` `Op` regex run. Longer dollar runs
    /// (`$$"""…`, `$$$"""…`, …) take the bracket-count "extended
    /// interpolation" arm: the `\$\$+"""` regex (≥2 `$` then `"""`) beats
    /// both the 1-`$` `$"""` literal (it requires exactly one `$`) and the
    /// `Op("$$")` run (anchored on `"""`, so `$$x` / `$$ ` keep their
    /// `Op`/`Dollar` lexing) → `ExtendedBeginEnd` or `ExtendedBegin`.
    #[token("$\"\"\"", lex_interp_triple_opener)]
    #[token("$\"", lex_interp_opener)]
    #[token("$@\"", lex_interp_verbatim_opener)]
    #[token("@$\"", lex_interp_verbatim_opener)]
    #[regex("\\$\\$+\"\"\"", lex_interp_extended_opener)]
    InterpString(InterpKind),

    // ---- punctuation --------------------------------------------------------
    #[token("(*)")]
    LParenStarRParen,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBrack,
    #[token("]")]
    RBrack,
    #[token("[|")]
    LBrackBar,
    #[token("|]")]
    BarRBrack,
    #[token("[<")]
    LBrackLess,
    #[token(">]")]
    GreaterRBrack,
    /// `<@` — typed code-quotation opener. Wins over `Op` (priority 2 > 1).
    /// Shorter than `LQuoteRaw`, so logos maximal-munch selects `LQuoteRaw`
    /// when the next char is `@` (i.e. `<@@`).
    #[token("<@")]
    LQuote,
    /// `<@@` — untyped (raw) code-quotation opener.
    #[token("<@@")]
    LQuoteRaw,
    /// `@>` — typed code-quotation closer (emitted in the filtered stream).
    /// Loses to `Op` when followed immediately by another op character (e.g.
    /// `@>.`), so compound forms `@>.` / `@>|}` are separate tokens below.
    #[token("@>")]
    RQuote,
    /// `@@>` — untyped (raw) code-quotation closer.
    #[token("@@>")]
    RQuoteRaw,
    /// `@>.` — typed closer immediately followed by `.`. FCS's `lex.fsl` emits
    /// this as `RQUOTE_DOT`; `LexFilter` splits it back into `RQUOTE` + `DOT`.
    /// We match the full 3-char string so logos doesn't prefer `Op("@>.")`.
    #[token("@>.")]
    RQuoteDot,
    /// `@@>.` — untyped variant of `RQuoteDot`.
    #[token("@@>.")]
    RQuoteRawDot,
    /// `@>|}` — typed closer inside an anonymous-record expression
    /// (`{| F = <@ 1 @>|}`). FCS emits `RQUOTE_BAR_RBRACE`; we split it into
    /// `RQUOTE` + `BAR_RBRACE` in the lexfilter.
    #[token("@>|}")]
    RQuoteBarRBrace,
    /// `@@>|}` — untyped variant of `RQuoteBarRBrace`.
    #[token("@@>|}")]
    RQuoteRawBarRBrace,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("{|")]
    LBraceBar,
    #[token("|}")]
    BarRBrace,
    #[token(",")]
    Comma,
    #[token(";;")]
    SemiSemi,
    #[token(";")]
    Semi,
    #[token("..^")]
    DotDotHat,
    #[token("..")]
    DotDot,
    /// F# "funky operator names" — fixed identifier-like operator forms that
    /// can appear in declarations such as `let (.()) x = ...` or
    /// `abstract (.[]) : ...`. `lex.fsl` emits these as `FUNKY_OPERATOR_NAME`
    /// in one bite; without them, `.[]` lexes as `Dot LBrack RBrack`, which
    /// a parser couldn't match against an operator-name production.
    #[token(".[]")]
    #[token(".[]<-")]
    #[token(".()")]
    #[token(".()<-")]
    #[token(".[,]")]
    #[token(".[,]<-")]
    #[token(".[,,]")]
    #[token(".[,,]<-")]
    #[token(".[,,,]")]
    #[token(".[,,,]<-")]
    #[token(".[..]")]
    #[token(".[..,..]")]
    #[token(".[..,..,..]")]
    #[token(".[..,..,..,..]")]
    FunkyOpName(&'a str),
    #[token(".")]
    Dot,
    #[token("::")]
    ColonColon,
    #[token(":?>")]
    ColonQMarkGreater,
    #[token(":?")]
    ColonQMark,
    #[token(":>")]
    ColonGreater,
    #[token(":=")]
    ColonEquals,
    #[token(":")]
    Colon,
    #[token("->")]
    RArrow,
    #[token("<-")]
    LArrow,
    #[token("=")]
    Equals,
    #[token("&&")]
    AmpAmp,
    #[token("&")]
    Amp,
    #[token("||")]
    BarBar,
    #[token("|")]
    Bar,
    #[token("??")]
    QMarkQMark,
    #[token("?")]
    QMark,
    #[token("#")]
    Hash,
    #[token("$")]
    Dollar,
    #[token("~")]
    Tilde,

    /// Standalone `'`. Used in F# type parameter names like `'T`, `'a`. The
    /// `Char` regex consumes proper character literals (`'a'`) before this
    /// fires, so `'` only matches here when it doesn't start a char literal.
    #[token("'")]
    Quote,

    /// Bare `<`. The bool payload mirrors FCS's `LESS of bool`: `false` at lex
    /// time (the lexer doesn't know the context), promoted to `true` by the
    /// lexfilter's `peek_adjacent_typars` when the `<` opens a generic type
    /// application (`f<int>`, `list<string>`). The parser dispatches on the
    /// bool. Explicit `priority = 2` because a single-char `#[token]` would
    /// otherwise tie the `Op` regex's `priority = 1` and logos would refuse
    /// to choose. Multi-char compounds (`<=`, `<-`, `<@`, `[<`) win by length.
    #[token("<", |_| false, priority = 2)]
    Less(bool),

    /// Bare `>`. See [`Token::Less`] for the bool payload semantics. Same priority
    /// reasoning. Multi-char compounds (`>=`, `>>`, `>]`, `->`) win by length.
    #[token(">", |_| false, priority = 2)]
    Greater(bool),

    /// Any other run of operator characters. Loses to specific tokens above
    /// when the lengths tie, otherwise wins by being longer. Classification
    /// into the `INFIX_*` precedence buckets is the parser layer's job.
    ///
    /// Mirrors lex.fsl's general-operator grammar `ignored_op_char* SIG op_char*`
    /// (lines 970-986): an optional `ignored_op_char` (`. $ ?`), then one
    /// *significant* char (the SIG class — every op_char except `:`/`.`/`?`),
    /// then any op_chars (`:` included as a *trailing* char). It deliberately
    /// does NOT match a run starting with `:` (those form the fixed
    /// `Colon`/`ColonColon`/… tokens) nor a run of pure `ignored_op_char`s
    /// (those are `Dot`/`DotDot`/`QMark`/…), so `::!`, `:^`, `...`, `?.` split
    /// exactly as FCS splits them instead of over-munching into one `Op`.
    /// `$` is in both classes — in lex.fsl it is both an `ignored_op_char` and
    /// an `INFIX_COMPARE_OP` head.
    ///
    /// The leading `ignored_op_char` is bounded to *one* (lex.fsl allows `*`):
    /// an unbounded run lets logos greedily consume a pure-dot prefix (`...`)
    /// into a dead, non-accepting path and then fail to fall back to the
    /// shorter `DotDot` accept (it errors instead of splitting). Capping at one
    /// keeps the dead path ≤1 char — never longer than the `DotDot`/`QMarkQMark`
    /// (length-2) accept — so logos always falls back. Real F# operators carry
    /// at most one leading ignored char (`.+`, `.*`, `?+`); an operator with two
    /// or more (`..+`, `.?+`) instead splits (`DotDot` + `Op("+")`), diverging
    /// from FCS — but a full F# corpus sweep finds zero such cases,
    /// and the alternatives are strictly worse: unbounded `*` lex-errors on
    /// `...`, and a 2-char bound re-introduces errors on mixed runs like `.?`
    /// (dead path of 2 with no length-2 token to fall back to). One is the safe
    /// maximum, and these forms still lex (never error).
    #[regex(r"[.$?]?[!%&*+\-/<=>@^|~$][!$%&*+\-./<=>?@^|~:]*", priority = 1)]
    Op(&'a str),
}

/// Iterate `(Result<Token, LexError>, Span)` over `src`.
///
/// The stream is interp-aware: when the Logos lexer emits
/// `Token::InterpString(InterpKind::Begin)`, a small state-machine wrapper
/// tracks `{`/`}` nesting through the following fill, then takes over
/// byte-walking the next string fragment and emits `InterpString(Part)` or
/// `InterpString(End)` as appropriate. The wrapper is otherwise a transparent
/// pass-through over the raw Logos stream.
pub fn lex(src: &str) -> impl Iterator<Item = (Result<Token<'_>, LexError>, Span)> + '_ {
    interp::InterpDriver::new(src)
}

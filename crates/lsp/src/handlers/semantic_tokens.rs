//! `textDocument/semanticTokens/full` — syntax highlighting.
//!
//! Two layers feed one token stream. The **lexical** layer lexes the buffer
//! with the F# lexer and maps each token to a lexically-unambiguous category
//! (keyword, comment, string, number, operator) — the categories where
//! overriding the editor's own grammar is always an improvement. The
//! **semantic** layer classifies the one thing the lexer cannot tell apart,
//! *identifiers*: it asks name resolution
//! ([`classification_at`](borzoi_sema::ResolvedFile::classification_at)) what
//! each identifier occurrence refers to — a function, a type, a parameter, a
//! union case, … — and maps that [`SemanticClass`] to a token type. An
//! identifier resolution declines to classify (a cross-file / referenced-assembly
//! name we don't place here yet, or a name resolution can't settle) is left
//! uncoloured, falling through to the editor's grammar: under-colour, never
//! mis-colour.
//!
//! The semantic layer is best-effort and degrades cleanly. A file in an
//! evaluated project is classified against the whole project's resolution; an
//! orphan / unevaluated buffer falls back to single-file resolution (locals,
//! parameters, and same-file bindings); and if neither is available the stream
//! is the lexical layer alone.
//!
//! Three protocol details drive the shape of the code:
//!
//! - **Tokens may not cross a line break.** The delta encoding has no way to
//!   express it, so a multi-line token — a `(* … *)` block comment, a
//!   `"""…"""` / `@"…"` string, a multi-line interpolation fragment — is
//!   split into one highlight per line (`Cursor::walk`).
//! - **The wire format is delta-encoded** relative to the previous token, in
//!   UTF-16 code units, and the tokens must be sorted and non-overlapping
//!   (`encode`). Lexing yields tokens in source order and the per-line split
//!   preserves it, so the stream is already sorted by construction.
//! - **The legend is a contract.** Each token's `token_type` is an index into
//!   the `token_types` array we advertise at `initialize`; `Hl` is the single
//!   source of truth for both (see [`legend`]).
//!
//! Lexing is *not* preprocessor-aware here (unlike the diagnostic path): we
//! run the raw [`lex`] so tokens inside inactive `#if` branches are still
//! highlighted. An editor wants keywords coloured everywhere, dead branch or
//! not. One cosmetic consequence is that the `if` in a `#if` directive line
//! lexes as the `if` keyword and is coloured as such; harmless for eyeballing.

use borzoi_cst::lexer::{Token, lex};
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, SemanticClass, resolve_file};
use lsp_types::{
    SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend, SemanticTokensParams,
    SemanticTokensResult, Url,
};
use rowan::TextRange;

use crate::cst_panic_safe::parse_with_symbols;
use crate::paths::{lexically_normalize, paths_equal};
use crate::server::{State, path_extension};

/// The highlight categories this server emits. Declaration order **is** the
/// legend order: [`Hl::index`] is `self as u32`, which indexes into the
/// `token_types` array [`legend`] builds from [`Hl::ALL`] in the same order.
/// The `legend_indices_match_kinds` test pins the two together.
///
/// The first five are the *lexical* categories ([`classify`]); the rest are the
/// *semantic* (identifier) categories a [`SemanticClass`] maps to
/// ([`Hl::of_class`]). New variants are appended so existing wire indices never
/// shift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hl {
    // Lexical categories.
    Keyword,
    Comment,
    String,
    Number,
    Operator,
    // Semantic (identifier) categories.
    Function,
    Type,
    Variable,
    Parameter,
    EnumMember,
    Method,
    Property,
    Event,
    Namespace,
    TypeParameter,
}

impl Hl {
    /// Every kind, in legend order. `Hl::ALL[h.index()] == h`.
    const ALL: [Hl; 15] = [
        Hl::Keyword,
        Hl::Comment,
        Hl::String,
        Hl::Number,
        Hl::Operator,
        Hl::Function,
        Hl::Type,
        Hl::Variable,
        Hl::Parameter,
        Hl::EnumMember,
        Hl::Method,
        Hl::Property,
        Hl::Event,
        Hl::Namespace,
        Hl::TypeParameter,
    ];

    /// The standard LSP [`SemanticTokenType`] this kind advertises as.
    fn token_type(self) -> SemanticTokenType {
        match self {
            Hl::Keyword => SemanticTokenType::KEYWORD,
            Hl::Comment => SemanticTokenType::COMMENT,
            Hl::String => SemanticTokenType::STRING,
            Hl::Number => SemanticTokenType::NUMBER,
            Hl::Operator => SemanticTokenType::OPERATOR,
            Hl::Function => SemanticTokenType::FUNCTION,
            Hl::Type => SemanticTokenType::TYPE,
            Hl::Variable => SemanticTokenType::VARIABLE,
            Hl::Parameter => SemanticTokenType::PARAMETER,
            Hl::EnumMember => SemanticTokenType::ENUM_MEMBER,
            Hl::Method => SemanticTokenType::METHOD,
            Hl::Property => SemanticTokenType::PROPERTY,
            Hl::Event => SemanticTokenType::EVENT,
            Hl::Namespace => SemanticTokenType::NAMESPACE,
            Hl::TypeParameter => SemanticTokenType::TYPE_PARAMETER,
        }
    }

    /// Map a resolved [`SemanticClass`] to the token category we highlight it
    /// as. Coarser than the class in places the standard legend does not
    /// distinguish: a value, a parameter, and a `match` local are all
    /// `variable`/`parameter`; the several constructor kinds (union / enum /
    /// exception case) all read as `enumMember`; an active pattern as a
    /// `function`; an in-file member — whose method-vs-property flavour
    /// [`SemanticClass::Member`] does not carry — defaults to `method`; and a
    /// referenced-assembly field is folded into `property` (no `field` in the
    /// standard legend). An F# `module` reads as a `namespace`.
    fn of_class(class: SemanticClass) -> Hl {
        match class {
            SemanticClass::Function | SemanticClass::ActivePattern => Hl::Function,
            SemanticClass::Value | SemanticClass::PatternLocal => Hl::Variable,
            SemanticClass::Parameter => Hl::Parameter,
            SemanticClass::Type => Hl::Type,
            SemanticClass::UnionCase | SemanticClass::ExceptionCase | SemanticClass::EnumCase => {
                Hl::EnumMember
            }
            SemanticClass::Member | SemanticClass::Method => Hl::Method,
            SemanticClass::Property => Hl::Property,
            SemanticClass::Event => Hl::Event,
            SemanticClass::Module => Hl::Namespace,
            SemanticClass::TypeParameter => Hl::TypeParameter,
        }
    }

    /// The legend index, i.e. the `token_type` field on the wire.
    fn index(self) -> u32 {
        self as u32
    }
}

/// The legend advertised at `initialize` and referenced by every token's
/// `token_type`. No modifiers — this lexical pass has nothing to attach them
/// to. Shared with [`crate::server::server_capabilities`] so the advertised
/// legend and the emitted indices can't drift.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: Hl::ALL.iter().map(|h| h.token_type()).collect(),
        token_modifiers: Vec::new(),
    }
}

/// Map a lexer [`Token`] to its highlight category, or `None` for tokens that
/// carry no lexical highlight. Exhaustive on purpose: a new lexer token is a
/// compile error here until someone decides how it should be coloured.
fn classify(token: &Token<'_>) -> Option<Hl> {
    use Token::*;
    Some(match token {
        // Keywords: the lexer gives each its own variant (always-keywords,
        // F#-specific keywords, and the `let!`/`do!`/… bang forms), so
        // highlighting them is a pure match with no context required.
        And | As | Assert | Base | Begin | Class | Do | Done | DownTo | Else | End | Exception
        | False | Finally | For | Fun | Function | If | In | Inherit | Lazy | Let | Match | Mod
        | Module | Mutable | New | Of | Open | Or | Private | Rec | Sig | Struct | Then | To
        | True | Try | Type | Val | When | While | With | Abstract | Const | Default | Delegate
        | Downcast | Elif | Extern | Fixed | Global | Inline | Interface | Internal | Member
        | Namespace | Null | Override | Public | Return | Static | Upcast | Use | Void | Yield
        | DoBang | YieldBang | ReturnBang | MatchBang | AndBang | LetBang | UseBang | WhileBang => {
            Hl::Keyword
        }
        // `__SOURCE_FILE__` & co. are KEYWORD_STRING in FCS. (The legacy ML
        // reserved words — `break`, `sealed`, … — lex as ordinary `Token::Ident`
        // and fall to the no-lexical-highlight arm below, matching FCS, which
        // hands the parser an `IDENT` for them; see `crate::lexer::RESERVED_IDENTS`.)
        KeywordString(_) => Hl::Keyword,

        LineComment | BlockComment => Hl::Comment,

        // Char and every string flavour. Interpolation fragments are strings;
        // the `{ … }` fills between them lex as ordinary tokens and pick up
        // their own highlight.
        Char(_) | String | TripleString | VerbatimString | InterpString(_) => Hl::String,

        // `IntDotDot` (`1..`) is broadly a number, but its span also covers the
        // trailing `..` range operator; `highlights` intercepts it to colour
        // only the digits, so this arm is the (graceful) fallback.
        XInt(_) | XIntSuffixed(_) | XIEEE32(_) | XIEEE64(_) | IntSuffixed(_) | BigNum(_)
        | Decimal(_) | Float32(_) | Float64(_) | IntDotDot(_) | Int(_) => Hl::Number,

        // The generic operator-char run: `|>`, `>>`, `+`, custom operators.
        // The *fixed* operator/punctuation tokens (`->`, `=`, `<`, brackets, …)
        // are deliberately left to the editor's grammar (see module docs).
        Op(_) => Hl::Operator,

        // No lexical highlight: identifiers (need sema), the bare `<`/`>`/`_`,
        // every bracket / punctuation / quotation marker, and trivia.
        Whitespace | Newline | Ident(_) | QuotedIdent(_) | Underscore | LParenStarRParen
        | LParen | RParen | LBrack | RBrack | LBrackBar | BarRBrack | LBrackLess
        | GreaterRBrack | LQuote | LQuoteRaw | RQuote | RQuoteRaw | RQuoteDot | RQuoteRawDot
        | RQuoteBarRBrace | RQuoteRawBarRBrace | LBrace | RBrace | LBraceBar | BarRBrace
        | Comma | SemiSemi | Semi | DotDotHat | DotDot | FunkyOpName(_) | Dot | ColonColon
        | ColonQMarkGreater | ColonQMark | ColonGreater | ColonEquals | Colon | RArrow | LArrow
        | Equals | AmpAmp | Amp | BarBar | Bar | QMarkQMark | QMark | Hash | Dollar | Tilde
        | Quote | Less(_) | Greater(_) => {
            return None;
        }
    })
}

/// One highlighted span, on a single line, in absolute LSP coordinates
/// (0-based line; `start_char`/`length` in UTF-16 code units). The
/// intermediate the pure core produces before delta-encoding — easy to assert
/// on in tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AbsToken {
    line: u32,
    start_char: u32,
    length: u32,
    hl: Hl,
}

/// A running position over the source in LSP coordinates (0-based line; column
/// in UTF-16 code units). Threaded through the whole lexer stream so positions
/// are computed in one pass — calling `offset_to_position` per token would
/// rescan from the file start each time, making a full-document request
/// quadratic in the file size.
struct Cursor {
    line: u32,
    character: u32,
}

impl Cursor {
    /// Advance over one lexer token's text `slice`, and — when `hl` is `Some` —
    /// emit one [`AbsToken`] per line the slice covers. Line breaks are never
    /// part of an emitted span (an LSP token can't cross one), so a line the
    /// slice covers with no other content (a blank line inside a block comment)
    /// yields nothing; continuation lines start at column 0 because the token
    /// text, including any leading indentation, begins there. The `\r\n|\n|\r`
    /// rule matches [`crate::position::offset_to_position`], so the cursor stays
    /// in lockstep with the rest of the server's positions.
    fn walk(&mut self, slice: &str, hl: Option<Hl>, out: &mut Vec<AbsToken>) {
        let mut length: u32 = 0;
        let mut chars = slice.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' | '\n' => {
                    if length > 0
                        && let Some(hl) = hl
                    {
                        out.push(AbsToken {
                            line: self.line,
                            start_char: self.character,
                            length,
                            hl,
                        });
                    }
                    // `\r\n` is one break (mirrors `offset_to_position`).
                    if c == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    self.line += 1;
                    self.character = 0;
                    length = 0;
                }
                _ => length += c.len_utf16() as u32,
            }
        }
        if length > 0 {
            if let Some(hl) = hl {
                out.push(AbsToken {
                    line: self.line,
                    start_char: self.character,
                    length,
                    hl,
                });
            }
            self.character += length;
        }
    }
}

/// The byte span of a lexer token as a rowan [`TextRange`], so it can be looked
/// up against name resolution's occurrence-keyed map.
fn to_text_range(span: &std::ops::Range<usize>) -> TextRange {
    TextRange::new(
        u32::try_from(span.start).expect("offset fits u32").into(),
        u32::try_from(span.end).expect("offset fits u32").into(),
    )
}

/// Lex `text` and produce its highlight spans in source order. A single pass:
/// the [`Cursor`] tracks the current position as it walks every token (so there
/// is no per-token position re-scan), splitting multi-line tokens into one span
/// per line. Each **identifier** token defers to `classify_ident` (name
/// resolution); everything else is coloured lexically by [`classify`].
fn highlights_with(text: &str, classify_ident: impl Fn(TextRange) -> Option<Hl>) -> Vec<AbsToken> {
    let mut out = Vec::new();
    let mut cursor = Cursor {
        line: 0,
        character: 0,
    };
    for (tok, span) in lex(text) {
        let slice = &text[span.clone()];
        match &tok {
            // `1..10` lexes as a single `IntDotDot` whose span covers the
            // digits *and* the `..` range operator. Colour only the digits as a
            // number; leave the trailing `..` to the editor's grammar, exactly
            // as the standalone `DotDot` token is left uncoloured. The regex
            // guarantees the slice ends with two ASCII dots.
            Ok(Token::IntDotDot(_)) => {
                let dots = slice.len() - "..".len();
                cursor.walk(&slice[..dots], Some(Hl::Number), &mut out);
                cursor.walk(&slice[dots..], None, &mut out);
            }
            // Identifiers carry no *lexical* highlight (`classify` returns `None`
            // for them); their colour, if any, is the semantic layer's — the
            // name-resolution classification at this occurrence's range.
            Ok(Token::Ident(_) | Token::QuotedIdent(_)) => {
                let hl = classify_ident(to_text_range(&span));
                cursor.walk(slice, hl, &mut out);
            }
            // A token that fails to lex (e.g. an unterminated string) carries no
            // `Token` to classify; advance over it but leave it uncoloured (the
            // diagnostic squiggle already flags it).
            other => {
                let hl = match other {
                    Ok(token) => classify(token),
                    Err(_) => None,
                };
                cursor.walk(slice, hl, &mut out);
            }
        }
    }
    out
}

/// Lexical-only highlights: the semantic layer declines every identifier. The
/// pure core the lexical tests assert on and the last-resort fallback in
/// [`handle`].
fn highlights(text: &str) -> Vec<AbsToken> {
    highlights_with(text, |_| None)
}

/// Delta-encode the absolute spans into the LSP wire format. Each token is
/// expressed relative to its predecessor; on a new line `delta_start` is the
/// absolute column. Relies on `tokens` being sorted (it is, by construction in
/// [`highlights`]); the `debug_assert` documents and checks that invariant
/// rather than masking a bug by silently sorting.
fn encode(tokens: &[AbsToken]) -> Vec<SemanticToken> {
    let mut data = Vec::with_capacity(tokens.len());
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;
    for t in tokens {
        debug_assert!(
            t.line > prev_line || (t.line == prev_line && t.start_char >= prev_start),
            "semantic tokens must be emitted in ascending (line, column) order"
        );
        let delta_line = t.line - prev_line;
        let delta_start = if delta_line == 0 {
            t.start_char - prev_start
        } else {
            t.start_char
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: t.length,
            token_type: t.hl.index(),
            token_modifiers_bitset: 0,
        });
        prev_line = t.line;
        prev_start = t.start_char;
    }
    data
}

/// The lexical-only token stream for `text`, ready for the LSP wire. Pure:
/// lex → classify → per-line split → delta-encode. The semantic layer's
/// identifier colours are added by the semantic layer (`sema_tokens`).
pub fn semantic_tokens(text: &str) -> Vec<SemanticToken> {
    encode(&highlights(text))
}

/// The full token stream for `text`, with identifier colours drawn from
/// `classify` — an identifier-token `TextRange` → [`SemanticClass`] lookup. The
/// pure core of the semantic layer; the shell (`project_tokens` /
/// `single_file_tokens`) supplies the closure from name resolution's
/// [`ResolvedFile::token_classifier`](borzoi_sema::ResolvedFile::token_classifier),
/// which resolves qualified tails (`Color.Red`'s `.Red`) that a per-token exact
/// lookup would miss.
fn sema_tokens(
    text: &str,
    classify: impl Fn(TextRange) -> Option<SemanticClass>,
) -> Vec<SemanticToken> {
    encode(&highlights_with(text, |range| {
        classify(range).map(Hl::of_class)
    }))
}

/// Classify a file resolved as part of its evaluated project — the authoritative
/// path. `None` when the file isn't locatable in an evaluated project (an orphan
/// buffer, or a project we couldn't evaluate), so [`handle`] falls back.
fn project_tokens(state: &mut State, uri: &Url) -> Option<Vec<SemanticToken>> {
    let path = uri.to_file_path().ok()?;
    let project = state.workspace.owning_project(&path)?;
    let State {
        semantic,
        workspace,
        docs,
        ..
    } = state;
    // This file's Compile-order index. Computed *before* resolving so we can fold
    // only the prefix up to it: F# is order-sensitive, so file `idx`'s tokens can
    // never depend on a later Compile-order file. On the large projects profiled
    // this drops the (wasted) suffix fold from every keystroke.
    let parses = semantic
        .parses_for_project(&project, workspace, docs)?
        .clone();
    let idx = parses
        .paths
        .iter()
        .position(|p| paths_equal(&lexically_normalize(p), &lexically_normalize(&path)))?;
    // The env must be the *exact* one the fold resolved against — a re-fetched
    // env can shift the `Entity` / `Member` handles the resolution recorded (see
    // `resolved_prefix_and_env_for`), so take both from one paired call rather
    // than re-fetching the env separately.
    let (resolved, env) = semantic.resolved_prefix_and_env_for(&project, idx, workspace, docs)?;
    // Lex the *resolved* text (what the classification ranges index into), not a
    // separately-fetched buffer, so ranges and offsets can't disagree. The
    // project-level classifier resolves cross-file references (a name defined in
    // an earlier Compile-order file) and referenced-assembly types/members,
    // which the single-file one declines. `resolved` covers `idx` (a prefix fold
    // up to it), and any cross-file `Item` it references is declared at an earlier
    // index, so `token_classifier`'s `item_def` lookup stays in-bounds.
    let classify = resolved.token_classifier(idx, &env);
    Some(sema_tokens(&parses.texts[idx], classify))
}

/// Single-file fallback for an orphan / unevaluated-project buffer: resolve the
/// one buffer in isolation (no project items, no referenced assemblies), so
/// locals, parameters, and same-file bindings still get classified. Only
/// implementation files (`.fs` / `.fsx`) — a signature file is not an
/// [`ImplFile`] and falls through to the lexical layer.
fn single_file_tokens(state: &mut State, uri: &Url) -> Option<Vec<SemanticToken>> {
    if !matches!(path_extension(uri).as_deref(), Some("fs" | "fsx")) {
        return None;
    }
    let text = state.docs.get(uri)?.clone();
    let symbols = state.symbols_for_uri(uri);
    let lang = state.lang_version_for_uri(uri);
    let parse = parse_with_symbols(&text, &symbols, lang)?;
    let file = ImplFile::cast(parse.root)?;
    let resolved = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());
    let classify = resolved.token_classifier();
    Some(sema_tokens(&text, classify))
}

/// Run the `textDocument/semanticTokens/full` handler. Returns `None` — a
/// `null` result, which clients accept — for a buffer we don't have or a
/// non-F# file. Only `.fs` / `.fsi` / `.fsx` are lexed; gating mirrors the
/// diagnostic dispatch so we never hand the F# lexer, say, a `.fsproj`.
///
/// The identifier colours come from name resolution, preferring the file's
/// evaluated project (`project_tokens`) and falling back to single-file
/// resolution (`single_file_tokens`); if neither is available the stream is
/// the lexical layer alone ([`semantic_tokens`]).
pub fn handle(state: &mut State, params: SemanticTokensParams) -> Option<SemanticTokensResult> {
    let uri = params.text_document.uri;
    if !matches!(path_extension(&uri).as_deref(), Some("fs" | "fsi" | "fsx")) {
        return None;
    }
    // Only *open* buffers are highlighted. Without this guard `project_tokens`
    // would read a Compile item straight from disk (and resolve the whole
    // project) for a file that was never opened, or was closed — regressing the
    // `null` contract, which is keyed on the buffer's presence in `docs`.
    if !state.docs.contains_key(&uri) {
        return None;
    }
    let data = if let Some(data) = project_tokens(state, &uri) {
        data
    } else if let Some(data) = single_file_tokens(state, &uri) {
        data
    } else {
        semantic_tokens(state.docs.get(&uri)?)
    };
    Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, TextDocumentIdentifier, Url};
    use proptest::prelude::*;

    /// Decode the LSP wire format back to absolute `(line, start_char, length,
    /// token_type)` tuples, applying the same delta rules a client does.
    fn decode(data: &[SemanticToken]) -> Vec<(u32, u32, u32, u32)> {
        let mut line = 0u32;
        let mut start = 0u32;
        let mut out = Vec::with_capacity(data.len());
        for t in data {
            if t.delta_line == 0 {
                start += t.delta_start;
            } else {
                line += t.delta_line;
                start = t.delta_start;
            }
            out.push((line, start, t.length, t.token_type));
        }
        out
    }

    #[test]
    fn classifies_keyword_number_comment() {
        // `let`(kw) x = `1`(num) `// hi`(comment); `x` and `=` carry no colour.
        let toks = decode(&semantic_tokens("let x = 1 // hi\n"));
        assert_eq!(
            toks,
            vec![
                (0, 0, 3, Hl::Keyword.index()),
                (0, 8, 1, Hl::Number.index()),
                (0, 10, 5, Hl::Comment.index()),
            ]
        );
    }

    #[test]
    fn highlights_strings_and_operators() {
        // `x`(none) `|>`(op) `"hi"`(string).
        let toks = decode(&semantic_tokens("x |> \"hi\""));
        assert_eq!(
            toks,
            vec![
                (0, 2, 2, Hl::Operator.index()),
                (0, 5, 4, Hl::String.index()),
            ]
        );
    }

    #[test]
    fn block_comment_splits_per_line() {
        // A multi-line token becomes one highlight per line; the `\n`s
        // themselves are not covered.
        let c = Hl::Comment.index();
        let toks = decode(&semantic_tokens("(*\nfoo\n*)"));
        assert_eq!(toks, vec![(0, 0, 2, c), (1, 0, 3, c), (2, 0, 2, c)]);
    }

    #[test]
    fn triple_string_splits_per_line() {
        // The same per-line split for a multi-line string literal.
        let s = Hl::String.index();
        let toks = decode(&semantic_tokens("\"\"\"a\nbb\"\"\""));
        // line 0: `"""a` (4); line 1: `bb"""` (5).
        assert_eq!(toks, vec![(0, 0, 4, s), (1, 0, 5, s)]);
    }

    #[test]
    fn column_is_utf16_after_multibyte() {
        // `🦀` is 2 UTF-16 units, so the keyword after it starts at column 3
        // (crab = 2, space = 1), proving columns are UTF-16, not bytes/chars.
        let toks = decode(&semantic_tokens("🦀 let"));
        assert_eq!(toks, vec![(0, 3, 3, Hl::Keyword.index())]);
    }

    #[test]
    fn range_colours_digits_not_the_dots() {
        // `1..10` lexes the `1..` as one `IntDotDot` token; only the digits are
        // a number, so the `..` (cols 1-2) is left uncoloured and `10` is its
        // own number span.
        let n = Hl::Number.index();
        let toks = decode(&semantic_tokens("1..10"));
        assert_eq!(toks, vec![(0, 0, 1, n), (0, 3, 2, n)]);
    }

    #[test]
    fn legend_indices_match_kinds() {
        let legend = legend();
        assert_eq!(legend.token_types.len(), Hl::ALL.len());
        assert!(legend.token_modifiers.is_empty());
        for hl in Hl::ALL {
            assert_eq!(legend.token_types[hl.index() as usize], hl.token_type());
            assert_eq!(Hl::ALL[hl.index() as usize], hl);
        }
    }

    fn params_for(uri: Url) -> SemanticTokensParams {
        SemanticTokensParams {
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            text_document: TextDocumentIdentifier { uri },
        }
    }

    #[test]
    fn handle_returns_tokens_for_fs_buffer() {
        let mut state = State::new();
        let uri = Url::parse("file:///tmp/A.fs").unwrap();
        state.docs.insert(uri.clone(), "let x = 1\n".to_string());
        match handle(&mut state, params_for(uri)).expect("Some for an open .fs buffer") {
            SemanticTokensResult::Tokens(t) => assert!(!t.data.is_empty()),
            other => panic!("expected Tokens, got {other:?}"),
        }
    }

    #[test]
    fn handle_skips_non_fsharp_and_missing_buffers() {
        let mut state = State::new();
        let txt = Url::parse("file:///tmp/readme.txt").unwrap();
        state.docs.insert(txt.clone(), "let x = 1".to_string());
        assert!(
            handle(&mut state, params_for(txt)).is_none(),
            "a non-F# buffer must not be highlighted"
        );
        let missing = Url::parse("file:///tmp/missing.fs").unwrap();
        assert!(
            handle(&mut state, params_for(missing)).is_none(),
            "an unopened buffer has no text to highlight"
        );
    }

    /// The semantic layer merges with the lexical one: for a synthetic
    /// classifier that colours the identifier `x` (bytes 4..5 of `let x = 1`) as
    /// a value, the wire stream carries `let`(keyword), `x`(variable),
    /// `1`(number) — the lexical categories untouched and the identifier now
    /// coloured.
    #[test]
    fn sema_layer_colours_identifiers_and_keeps_lexical() {
        let text = "let x = 1\n";
        let classify = |range: TextRange| {
            (range == TextRange::new(4.into(), 5.into())).then_some(SemanticClass::Value)
        };
        let toks = decode(&sema_tokens(text, classify));
        assert_eq!(
            toks,
            vec![
                (0, 0, 3, Hl::Keyword.index()),
                (0, 4, 1, Hl::Variable.index()),
                (0, 8, 1, Hl::Number.index()),
            ]
        );
    }

    /// A declined identifier (the classifier says nothing) stays uncoloured,
    /// exactly as in the lexical-only stream — so the semantic layer can only
    /// ever *add* colour, never remove a lexical one.
    #[test]
    fn declined_identifiers_match_the_lexical_stream() {
        let text = "let x = f y\n";
        assert_eq!(sema_tokens(text, |_| None), semantic_tokens(text));
    }

    /// End-to-end through [`handle`] on an orphan `.fs` buffer (no project): the
    /// single-file fallback resolves it, so `f` is a function and its parameter
    /// `a` a parameter — the two identifier categories name resolution settles
    /// without a project.
    #[test]
    fn handle_classifies_identifiers_single_file() {
        let mut state = State::new();
        let uri = Url::parse("file:///tmp/Classified.fs").unwrap();
        let text = "let f a = a\n";
        state.docs.insert(uri.clone(), text.to_string());
        let SemanticTokensResult::Tokens(t) =
            handle(&mut state, params_for(uri)).expect("Some for an open .fs buffer")
        else {
            panic!("expected Tokens");
        };
        let toks = decode(&t.data);
        // `let`(kw, cols 0..3), `f`(function, col 4), `a`(parameter, col 6),
        // `a`(parameter, col 10). Two parameter uses of `a`; one function `f`.
        assert!(
            toks.contains(&(0, 4, 1, Hl::Function.index())),
            "`f` should be a function; got {toks:?}"
        );
        assert_eq!(
            toks.iter()
                .filter(|(_, _, _, ty)| *ty == Hl::Parameter.index())
                .count(),
            2,
            "both uses of `a` should be parameters; got {toks:?}"
        );
    }

    /// A qualified reference records its resolution at the whole dotted range,
    /// so the leaf segment has no exact key of its own; `token_classifier` maps
    /// it to the path's class. In `let c = Color.Red` the qualifier `Color` is a
    /// type and the leaf `Red` an enum member — the qualified-leaf case an exact
    /// per-token lookup missed entirely.
    #[test]
    fn qualified_leaf_and_head_are_both_classified() {
        let text = "type Color = Red = 0 | Green = 1\nlet c = Color.Red\n";
        let parse = borzoi_cst::parser::parse(text);
        let file = ImplFile::cast(parse.root).expect("impl file");
        let resolved = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());
        let toks = decode(&sema_tokens(text, resolved.token_classifier()));
        // Line 1: `Color` (cols 8..13) is the type; `Red` (cols 14..17) the case.
        assert!(
            toks.contains(&(1, 8, 5, Hl::Type.index())),
            "`Color` should be a type; got {toks:?}"
        );
        assert!(
            toks.contains(&(1, 14, 3, Hl::EnumMember.index())),
            "the qualified leaf `Red` should be an enum member; got {toks:?}"
        );
    }

    /// A generator of F#-ish source: a concatenation of real token snippets
    /// (keywords, comments, multi-line strings, numbers, operators, idents)
    /// interleaved with whitespace, line breaks, and multi-byte characters —
    /// enough to exercise classification, per-line splitting, and UTF-16
    /// column maths together.
    fn fsharp_sourcey() -> impl Strategy<Value = String> {
        let snippet = prop_oneof![
            Just("let "),
            Just("if "),
            Just("module "),
            Just("match "),
            Just("// line comment"),
            Just("(* block *)"),
            Just("(*\nmulti\nline\n*)"),
            Just("\"string\""),
            Just("\"\"\"triple\nstring\"\"\""),
            Just("@\"verbatim\nstr\""),
            Just("$\"interp {x} end\""),
            Just("123"),
            Just("0xFF"),
            Just("1.5"),
            Just("1.0m"),
            Just("42UL"),
            Just("1..10"),
            Just("|>"),
            Just("+"),
            Just(">>"),
            Just("="),
            Just("foo"),
            Just("x'"),
            Just(" "),
            Just("\n"),
            Just("\r\n"),
            Just("\t"),
            Just("À"),
            Just("🦀"),
        ]
        .prop_map(|s: &str| s.to_string());
        proptest::collection::vec(snippet, 0..30).prop_map(|v| v.concat())
    }

    proptest! {
        /// Oracle: the bytes the wire format highlights are exactly the bytes
        /// of the classifiable lexer tokens — line-break bytes aside. This
        /// cross-checks the per-line splitter, the delta encoder, and the
        /// UTF-16 ↔ byte position round-trip against a byte map built straight
        /// from the lexer, so a bug in any of them shows up as a mismatch.
        #[test]
        fn highlighted_bytes_match_classifiable_tokens(text in fsharp_sourcey()) {
            // Reference: byte → highlight class, straight from the lexer,
            // mirroring `highlights`' `IntDotDot` refinement (digits only).
            let mut expected: Vec<Option<Hl>> = vec![None; text.len()];
            for (tok, span) in lex(&text) {
                match &tok {
                    Ok(Token::IntDotDot(_)) => {
                        for slot in &mut expected[span.start..span.end - "..".len()] {
                            *slot = Some(Hl::Number);
                        }
                    }
                    Ok(token) => {
                        if let Some(hl) = classify(token) {
                            for b in span.clone() {
                                expected[b] = Some(hl);
                            }
                        }
                    }
                    Err(_) => {}
                }
            }
            // Got: decode the wire format and map each span back to bytes.
            let mut got: Vec<Option<Hl>> = vec![None; text.len()];
            for (line, start, len, ty) in decode(&semantic_tokens(&text)) {
                let b0 = crate::position::position_to_offset(
                    &text,
                    Position { line, character: start },
                );
                let b1 = crate::position::position_to_offset(
                    &text,
                    Position { line, character: start + len },
                );
                for slot in &mut got[b0..b1] {
                    *slot = Some(Hl::ALL[ty as usize]);
                }
            }
            // They agree everywhere except the line-break bytes a multi-line
            // token covers: `expected` has the token's class there, `got` is
            // `None` because LSP spans stop at the break.
            for b in 0..text.len() {
                let byte = text.as_bytes()[b];
                if byte == b'\r' || byte == b'\n' {
                    continue;
                }
                prop_assert_eq!(expected[b], got[b], "mismatch at byte {}", b);
            }
        }

        /// On arbitrary input the encoder never panics and the decoded stream
        /// is well-formed: every `token_type` is a legend index and the tokens
        /// are in non-decreasing (line, column) order (so no client-side delta
        /// underflows).
        #[test]
        fn wire_stream_is_well_formed(text in any::<String>()) {
            let mut prev = (0u32, 0u32);
            for (line, start, _len, ty) in decode(&semantic_tokens(&text)) {
                prop_assert!(ty < Hl::ALL.len() as u32);
                prop_assert!((line, start) >= prev, "tokens out of order at {:?}", (line, start));
                prev = (line, start);
            }
        }

        /// The semantic layer is strictly *additive* over the lexical one: it
        /// only ever colours identifier bytes (which the lexical layer leaves
        /// uncoloured), and it touches nothing else. Classifying **every**
        /// identifier as a function (a fixed class) is the maximal-overlay case;
        /// the stream must stay well-formed, every non-identifier byte must keep
        /// its lexical colour, and every identifier byte must now be a function.
        #[test]
        fn sema_layer_is_additive_over_identifiers(text in fsharp_sourcey()) {
            // Reference from the lexer: which bytes are identifier bytes, and the
            // lexical class of every other byte (mirroring `highlights`).
            let mut is_ident = vec![false; text.len()];
            let mut lexical: Vec<Option<Hl>> = vec![None; text.len()];
            for (tok, span) in lex(&text) {
                match &tok {
                    Ok(Token::IntDotDot(_)) => {
                        for slot in &mut lexical[span.start..span.end - "..".len()] {
                            *slot = Some(Hl::Number);
                        }
                    }
                    Ok(Token::Ident(_) | Token::QuotedIdent(_)) => {
                        for b in span.clone() {
                            is_ident[b] = true;
                        }
                    }
                    Ok(token) => {
                        if let Some(hl) = classify(token) {
                            for b in span.clone() {
                                lexical[b] = Some(hl);
                            }
                        }
                    }
                    Err(_) => {}
                }
            }

            // The maximal overlay: every identifier occurrence → a function.
            let sema = sema_tokens(&text, |_| Some(SemanticClass::Function));

            // Well-formed: legend indices, ascending non-overlapping order.
            let mut prev = (0u32, 0u32);
            for (line, start, _len, ty) in decode(&sema) {
                prop_assert!(ty < Hl::ALL.len() as u32);
                prop_assert!((line, start) >= prev, "out of order at {:?}", (line, start));
                prev = (line, start);
            }

            // Decode the sema stream back to a per-byte colour map.
            let mut got: Vec<Option<Hl>> = vec![None; text.len()];
            for (line, start, len, ty) in decode(&sema) {
                let b0 = crate::position::position_to_offset(
                    &text,
                    Position { line, character: start },
                );
                let b1 = crate::position::position_to_offset(
                    &text,
                    Position { line, character: start + len },
                );
                for slot in &mut got[b0..b1] {
                    *slot = Some(Hl::ALL[ty as usize]);
                }
            }

            for b in 0..text.len() {
                let byte = text.as_bytes()[b];
                if byte == b'\r' || byte == b'\n' {
                    continue; // line-break bytes are never inside an LSP span.
                }
                if is_ident[b] {
                    prop_assert_eq!(got[b], Some(Hl::Function), "ident byte {} not recoloured", b);
                } else {
                    prop_assert_eq!(got[b], lexical[b], "non-ident byte {} changed", b);
                }
            }
        }
    }
}

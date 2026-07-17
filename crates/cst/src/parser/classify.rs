//! Raw-token and character classification predicates shared across the
//! [`super::Parser`] productions: leading-token gates (`raw_starts_*`), trivia
//! mapping, operator classification, and the .NET-aligned identifier-case
//! helpers. Split out of `parser/mod.rs`; every entry is a pure function over
//! a token, `char`, or `&str` with no parser state of its own.

use super::numeric::{classify_suffixed_int, split_fold_sign};
use crate::directives::TriviaToken;
use crate::lexer::Token;
use crate::syntax::SyntaxKind;

/// `true` if `tok` (one filtered-stream Raw token) can stand as the leading
/// token of an *atomicExpr* (`pars.fsy:5211`). Atomic-level prefix
/// operators — FCS's `PREFIX_OP atomicExpr` rule (`pars.fsy:5258`) — are
/// included via [`is_prefix_op_text`], so `!x` / `~~~x` both pass even
/// though they expand to `App(prefix, atom)` rather than a single token.
/// Minus-level prefixes (`MINUS`, `PLUS_MINUS_OP`, `AMP`, …) are NOT in
/// this set; they sit one rule up at `minusExpr` and are captured by
/// [`raw_starts_minus_expr`].
///
/// Used as the leading-token check for *arg-position* (where `argExpr`
/// only accepts `atomicExpr` or the `ADJACENT_PREFIX_OP atomicExpr`
/// rewrite — never a bare minus-level prefix) and as the no-LParen-body
/// fallback in [`super::Parser::next_non_trivia_raw_after`] lookups.
pub(super) fn raw_starts_atomic_expr(tok: &Token<'_>) -> bool {
    match tok {
        Token::Int(_)
        | Token::XInt(_)
        | Token::IntSuffixed(_)
        | Token::XIntSuffixed(_)
        | Token::Float64(_)
        | Token::Float32(_)
        | Token::XIEEE64(_)
        | Token::XIEEE32(_)
        | Token::Char(_)
        | Token::String
        | Token::VerbatimString
        | Token::TripleString
        | Token::Decimal(_)
        | Token::BigNum(_)
        | Token::True
        | Token::False
        // `null` is `SynExpr.Null` — its own `atomicExpr` production
        // (`pars.fsy:5402`, reached via `atomicExprAfterType`), at the
        // same precedence level as `TRUE`/`FALSE`. Parsed by
        // `parse_null_expr` into a `NULL_EXPR` node (not a const).
        | Token::Null
        // `base.Member` — base-class member access (FCS's `BASE DOT
        // atomicExprQualification`, `pars.fsy:5276`). The `base` keyword heads a
        // long-ident path (FCS's `Ident("base")`); a `.` qualification is
        // mandatory but that is enforced in `parse_base_expr`, so the head token
        // alone admits it as an atom-start everywhere a name can stand.
        | Token::Base
        // `global.Path` — the `global` namespace root (FCS's `GLOBAL DOT …`).
        // Like `base`, FCS treats the `global` keyword as an identifier heading
        // a long-ident path (its `idText` is the single-backtick-quoted
        // `` `global` ``); unlike `base`, a `.` qualification is *optional* — a
        // bare `global` is a valid single-segment `SynExpr.LongIdent`. Either
        // way it heads an atom, so admit it wherever a name can stand.
        | Token::Global
        | Token::Ident(_)
        | Token::QuotedIdent(_)
        // The F# 7 typar expression `'T` — a `QUOTE` heading an atom
        // (`pars.fsy:5263 QUOTE ident` → `SynExpr.Typar`). The `Char` regex
        // consumes a real char literal (`'a'`) before `Token::Quote` fires, so a
        // `Quote` here is always the sigil of a typar-expr `'T` (or a bare `'`,
        // which `parse_atomic_expr_head` reports as a clean error). Stands
        // wherever an atom can — arg position (`f 'T.M`), tuple element, paren
        // body — so admitting the token here covers them all; the `.Member`
        // qualification then chains via the postfix tail.
        | Token::Quote
        | Token::KeywordString(_)
        // Code-quotation openers — `quoteExpr` is reachable from `atomicExpr`
        // (`pars.fsy:5258` lists `quoteExpr` among the atomic alternatives),
        // so `<@ … @>` stands wherever an atom can (arg position, paren body,
        // tuple element, …). The matching closer is consumed by
        // `parse_quote_expr`, not here.
        | Token::LQuote
        | Token::LQuoteRaw
        // Computation-expression brace `{ … }`. `braceExprBody` is reachable
        // from `atomicExpr`, so `seq { … }` parses as the application of
        // `seq` to the brace atom, and a bare `{ … }` is itself an atom.
        // (Only the computation-expression arm of `{ … }` exists; record /
        // object expressions are not yet parsed — see `COMPUTATION_EXPR`.)
        | Token::LBrace
        // Anonymous-record expression `{| F = e; … |}` (`SynExpr.AnonRecd`),
        // FCS's `braceBarExpr` atomic alternative. Parsed by
        // `parse_anon_recd_expr`.
        | Token::LBraceBar
        // `struct (…)` / `struct {| … |}` — struct tuple / anon-record
        // expressions (FCS's `STRUCT LPAREN tupleExpr rparen` `pars.fsy:5314`
        // and `STRUCT braceBarExprCore` `:5909`). These are `atomicExprAfterType`
        // alternatives, so — unlike `if`/`match` — `struct (…)` is a valid app
        // argument (`f struct (1, 2)`). Dispatched in `parse_atomic_expr_head`;
        // a `struct` not followed by `(` / `{|` is a clean error there.
        | Token::Struct
        // List `[ … ]` and array `[| … |]` literal expressions — FCS's
        // `listExpr` (`pars.fsy:5298`) / `arrayExpr` (`:5450`), both
        // `atomicExprAfterType` alternatives via `arrayExpr`. As atoms they
        // stand in arg / expr-start / tuple-element / paren-body positions
        // alike (`f [1]`, `[1] @ xs`), so including the openers here admits
        // them everywhere at once. Dispatched in `parse_atomic_expr_head`.
        | Token::LBrack
        | Token::LBrackBar
        // The glued `(*)` multiply operator-value: a single lexer token
        // (`pars.fsy:6806 opName: LPAREN_STAR_RPAREN`). It stands as an atom
        // wherever a name can — arg position (`List.reduce (*)`), expr-start,
        // tuple element — so admitting the token here covers them all at once.
        // Dispatched in `parse_atomic_expr_head`. The *spaced* `( * )` is a
        // different token sequence (`Op("*")`) and stays the wildcard.
        | Token::LParenStarRParen
        | Token::LParen
        // `begin … end` — the verbose-syntax parenthesis (`beginEndExpr`, an
        // `atomicExpr` alternative `pars.fsy:5417`). `begin e end` is
        // `SynExpr.Paren e` and `begin end` is `SynConst.Unit`, so the opener
        // stands wherever `(` can: expr-start, arg position (`f begin x end` is
        // `App(f, Paren x)`), tuple element, paren body. Dispatched in
        // `parse_atomic_expr_head`; the matching `end` is consumed there.
        | Token::Begin => true,
        Token::InterpString(
            crate::lexer::InterpKind::BeginEnd { .. }
            | crate::lexer::InterpKind::Begin
            | crate::lexer::InterpKind::TripleBeginEnd { .. }
            | crate::lexer::InterpKind::TripleBegin
            | crate::lexer::InterpKind::VerbatimBeginEnd { .. }
            | crate::lexer::InterpKind::VerbatimBegin
            | crate::lexer::InterpKind::ExtendedBeginEnd { .. }
            | crate::lexer::InterpKind::ExtendedBegin { .. },
        ) => true,
        Token::Op(s) => is_prefix_op_text(s),
        _ => false,
    }
}

/// `true` if `tok` can begin an attribute's argument expression — FCS's
/// `opt_atomicExprAfterType` (`pars.fsy:1542`/`5385`/`5655`).
/// `atomicExprAfterType` is the `atomicExpr` subset reachable *after* a path,
/// and notably **excludes** a bare leading ident (`[<Foo Bar>]` is the bare
/// attribute `Foo` with no arg, not an application — verified `ParseHadErrors`)
/// and the prefix-operator forms. So this is exactly [`raw_starts_atomic_expr`]
/// minus the `Ident` / `QuotedIdent` and prefix-`Op` starters: constants
/// (including the `__LINE__` / `__SOURCE_FILE__` source-identifier
/// `KeywordString`s, which reach `atomicExprAfterType` through `constant` and
/// are handled by [`super::Parser::parse_const_payload`]), `null` (the
/// `SynExpr.Null` atom, via [`super::Parser::parse_null_expr`]), `(` (unit /
/// parenExpr), `{` (braceExpr), the interpolated-string openers, the
/// `<@`/`<@@` quotation openers, and `{|` (the anon-record expression
/// `braceBarExpr` → [`super::Parser::parse_anon_recd_expr`]) — the forms our
/// [`super::Parser::parse_atomic_expr`] parses today. (FCS's `beginEndExpr` arm
/// isn't parsed as an expression anywhere yet, so an attribute arg in that form
/// is a documented gap rather than a regression here.)
///
/// `Token::Struct` is **excluded** even though it's an `atomicExpr` starter:
/// only the struct *anon-record* `struct {| … |}` (a `braceBarExpr`) is in
/// `atomicExprAfterType`, while the struct *tuple* `struct (…)` lives in the
/// wider `atomicExpr` (`pars.fsy:5314`). A per-token gate can't tell the two
/// apart, and FCS rejects an unparenthesised `struct (…)` here
/// (`[<A struct (1, 2)>]`, `inherit C struct (1, 2)`). Excluding `struct`
/// matches FCS for the tuple form and merely keeps the (vanishingly rare)
/// unparenthesised `struct {| … |}` attribute arg an unsupported clean error —
/// exactly its pre-10.18 behaviour, so no regression. The parenthesised forms
/// (`[<A(struct (1, 2))>]`) are unaffected: their head token is `(`.
///
/// The list opener `[` (`Token::LBrack`) is **excluded**, but the array opener
/// `[|` (`Token::LBrackBar`) is **kept**. This mirrors FCS's split: `arrayExpr`
/// (`[| … |]`, `pars.fsy:5450`) is one of `atomicExprAfterType`'s alternatives,
/// while `listExpr` (`[ … ]`, `:5298`) lives only in the wider `atomicExpr`. So
/// FCS accepts `[<Foo [|1|]>]` / `inherit B [|1|]` (array) but rejects
/// `[<Foo [1]>]` / `inherit B [1]` (list — FS0010 "Unexpected symbol '[' in
/// attribute list"). Both forms are now full `atomicExpr` expressions
/// ([`raw_starts_atomic_expr`] admits them, [`super::Parser::parse_array_or_list_expr`]
/// parses them); excluding only `LBrack` matches FCS exactly. The parenthesised
/// list `[<Foo([1])>]` is unaffected (head token `(`).
///
/// The glued `(*)` operator-value token ([`Token::LParenStarRParen`]) is
/// **excluded** too: it is the `opName` `( * )`, which `atomicExprAfterType`
/// omits (`[<A(*)>]` / `new C(*)` are FCS errors). The general `( op )` form
/// has head token `(` and is filtered out at the `(` arm of
/// [`super::Parser::peek_starts_aftertype_arg`].
pub(super) fn raw_starts_attribute_arg(tok: &Token<'_>) -> bool {
    raw_starts_atomic_expr(tok)
        && !matches!(
            tok,
            Token::Ident(_)
                | Token::QuotedIdent(_)
                // `base` (the `BASE DOT` member-access head) is **excluded** for
                // the same reason as a bare ident: FCS's `atomicExprAfterType`
                // does not include the `BASE DOT` production, so an
                // unparenthesised `base.M` after an attribute / `new` / `inherit`
                // (`[<A base.M>]`, `new T base.M`, `inherit B base.M`) is an FCS
                // error ("Unexpected keyword 'base'"). The parenthesised forms
                // (`new T(base.M)`) are unaffected — their head token is `(`.
                | Token::Base
                // `global` (the `GLOBAL DOT` namespace-root head) is **excluded**
                // for exactly the same reason as `base`: FCS's
                // `atomicExprAfterType` does not include the `GLOBAL DOT`
                // production, so an unparenthesised `global.M` after an attribute
                // / `new` / `inherit` (`[<A global.X>]`, `new T global.M`,
                // `inherit B global.M`) is an FCS error. The parenthesised forms
                // (`new T(global.M)`) are unaffected — their head token is `(`.
                | Token::Global
                // The F# 7 typar expression `'T` (FCS's `QUOTE ident` →
                // `SynExpr.Typar`) is **excluded** for the same reason as a bare
                // ident: it is an `atomicExpr` alternative that
                // `atomicExprAfterType` omits, so an unparenthesised typar-expr
                // after an attribute / `new` / `inherit` (`[<A 'T>]`, `new B 'T`,
                // `inherit B 'T`) is an FCS parse error. The parenthesised forms
                // (`[<A('T)>]`, `new B('T)`, `inherit B('T)`) are unaffected —
                // their head token is `(`.
                | Token::Quote
                | Token::Op(_)
                | Token::Struct
                | Token::LBrack
                | Token::LParenStarRParen
        )
}

/// `true` if `tok` is one of the token kinds that
/// [`super::Parser::parse_const_payload`] dispatches on without panicking.
///
/// This is the strict subset of [`raw_starts_atomic_expr`] that
/// excludes idents and prefix operators: all numeric/string/char
/// literals, the two bool keywords, the three source-identifier
/// keyword-strings, and `(` (the unit-literal opener).
/// `LParen` is shared with the paren-expression / paren-pattern shapes,
/// so callers in those positions must do their own LParen-vs-unit
/// disambiguation before delegating to `parse_const_payload`.
///
/// `KeywordString` (`__SOURCE_DIRECTORY__` / `__SOURCE_FILE__` /
/// `__LINE__`) belongs here because [`super::Parser::parse_const_payload`]
/// dispatches on it (→ `SynConst.SourceIdentifier`, `pars.fsy:3475-3477`).
/// The expression path admits it via [`raw_starts_atomic_expr`] (which names
/// it directly), but the *pattern* and *static-constant-type* paths reach
/// `parse_const_payload` only through this predicate — FCS accepts a source
/// identifier as a `SynPat.Const` and a `SynType.StaticConstant`, so omitting
/// it here made those positions wrongly reject it.
pub(super) fn raw_starts_const_payload(tok: &Token<'_>) -> bool {
    matches!(
        tok,
        Token::Int(_)
            | Token::XInt(_)
            | Token::IntSuffixed(_)
            | Token::XIntSuffixed(_)
            | Token::Float64(_)
            | Token::Float32(_)
            | Token::XIEEE64(_)
            | Token::XIEEE32(_)
            | Token::Char(_)
            | Token::String
            | Token::VerbatimString
            | Token::TripleString
            | Token::Decimal(_)
            | Token::BigNum(_)
            | Token::True
            | Token::False
            | Token::KeywordString(_)
            | Token::LParen,
    ) || byte_interp_lit_kind_for(tok).is_some()
}

/// The de-quoted text of an identifier token — a backticked `` ``foo bar`` ``
/// reduced to `foo bar`, a plain `foo` left as-is. FCS stores the unquoted text
/// in `Ident.idText`, so positions that classify an identifier *by content*
/// (e.g. the `comparison`/`not` keywords of a type-parameter constraint) must
/// match against this, not the raw lexeme. `None` for a non-identifier token.
pub(super) fn ident_token_text<'a>(tok: &Token<'a>) -> Option<&'a str> {
    match tok {
        Token::Ident(s) => Some(s),
        Token::QuotedIdent(s) => Some(
            s.strip_prefix("``")
                .and_then(|t| t.strip_suffix("``"))
                .unwrap_or(s),
        ),
        _ => None,
    }
}

/// `true` if `tok` is a numeric-literal token carrying a folded leading
/// `+`/`-` sign. Only [`super::sign_fold`] produces such a token — the lexer's
/// numeric regexes never match a sign — so a sign-prefixed literal text is an
/// unambiguous marker of a fold. The pattern-start gates consult this because
/// their *raw*-stream lookahead still sees the pre-fold `Op("-")`/`Op("+")`
/// (the fold rewrites only the filtered stream), and would otherwise reject a
/// folded constant in a continuation/nested pattern position (`Some -1`,
/// `1, -1`, `1 :: -1`, `let f -1 = …`).
pub(super) fn token_is_folded_signed_literal(tok: &Token<'_>) -> bool {
    matches!(
        tok,
        Token::Int(t)
            | Token::XInt(t)
            | Token::IntSuffixed(t)
            | Token::XIntSuffixed(t)
            | Token::Float64(t)
            | Token::Float32(t)
            | Token::XIEEE64(t)
            | Token::XIEEE32(t)
            | Token::Decimal(t)
            | Token::BigNum(t)
            if t.starts_with(['-', '+'])
    )
}

/// `true` if `tok` begins an *atomic* pattern: an ident, `_`, `null`, a
/// list/array opener (`[` / `[|`), the `struct` of a struct-tuple pattern
/// (`struct (p1, p2, …)`), or a const-literal opener (including `(`). The
/// raw-stream analogue of [`super::Parser::is_atomic_pat_start`], which
/// delegates here.
///
/// Used by [`super::Parser::try_emit_head_binding_pat_element`] to gate
/// function-form promotion and the curried-arg sweep against the *raw*
/// stream. A LexFilter-swallowed `)` is absent from the filtered stream but
/// surfaces as [`Token::RParen`] in the raw stream; querying the raw token
/// here makes the gate reject it (a `)` is not an atomic-pat start), so the
/// promotion/sweep stops at the enclosing paren instead of reaching past it
/// into the next curried argument.
///
/// `Token::Struct` is unconditionally an atomic-pat start: in *pattern*
/// position `struct` only ever introduces the struct-tuple pattern (FCS's
/// `STRUCT LPAREN tupleParenPatternElements rparen`, `pars.fsy:3853`). A bare
/// `struct` not followed by `(` is an FCS parse error too; the dispatcher
/// ([`super::Parser::try_emit_atomic_pat`]) surfaces a clean lossless error.
///
/// `Token::LParenStarRParen` (the glued `(*)` multiply operator-value) is an
/// atomic-pat start too — `(*)`/`( * )` are operator-value patterns (FCS's
/// `opName`), so a curried `(*)` argument (`let f (*) = …`, `let (+) (*) = …`)
/// must promote/sweep like any other atomic arg. The general `( op )` and
/// spaced `( * )` forms ride the `LParen` arm (a const-payload opener); only
/// the *glued* token needs naming here. ([`super::Parser::try_emit_atomic_pat`]
/// has the matching dispatch arm.)
///
/// A quotation opener (`<@` / `<@@`, [`Token::LQuote`] / [`Token::LQuoteRaw`])
/// is an atomic-pat start: FCS's `atomicPattern: quoteExpr` (`pars.fsy:3776`) →
/// `SynPat.QuoteExpr`. A quotation only reaches pattern position as the argument
/// of a parameterised active pattern (`SpecificCall <@ … @> (args)`), so it must
/// gate function-form promotion / the curried-arg sweep like any other atomic
/// arg; [`super::Parser::try_emit_atomic_pat`] has the matching dispatch arm.
pub(super) fn raw_starts_atomic_pat(tok: &Token<'_>) -> bool {
    matches!(
        tok,
        Token::Ident(_)
            | Token::QuotedIdent(_)
            | Token::Underscore
            // `global` — only ever a *rooted long-ident* pattern head
            // (`global.N.Case`, FCS's `atomicPatternLongIdent: GLOBAL DOT
            // pathOp`). A *bare* `global` is not a valid pattern (FCS FS0010),
            // so the dispatcher ([`super::Parser::try_emit_atomic_pat`]) admits
            // it only when a `. ident` tail follows and otherwise surfaces a
            // clean lossless error — mirroring the token-only shape of the
            // `Struct`/`QMark` arms above.
            | Token::Global
            | Token::Null
            | Token::LBrack
            | Token::LBrackBar
            | Token::LBrace
            | Token::Struct
            | Token::LQuote
            | Token::LQuoteRaw
            // `?ident` — the optional-argument pattern (FCS's `QMARK ident` →
            // `SynPat.OptionalVal`, `pars.fsy:3802`). `?` only leads a pattern,
            // never an operator-value, in pattern position, so it is an
            // unconditional atomic-pat start here; the dispatcher
            // ([`super::Parser::try_emit_atomic_pat`]) reports a clean error if
            // no ident follows, mirroring FCS's lack of a `QMARK`-only rule.
            | Token::QMark
            | Token::LParenStarRParen
    ) || raw_starts_const_payload(tok)
}

/// `true` if `tok` begins a `constrPattern`-level element — an atomic pattern
/// ([`raw_starts_atomic_pat`]) or the `:?` IsInst prefix. This is exactly what
/// [`super::Parser::emit_pat_atom`] can consume, so the precedence climber gates its
/// `::` rhs / `,` continuation-element emits on it against the *raw* stream:
/// a LexFilter-swallowed `)` surfaces as [`Token::RParen`] here (not a
/// pattern-element start), so a missing tail before `)` (e.g. `(h ::)`) bails
/// cleanly instead of consuming the post-`)` token off the filtered stream.
pub(super) fn raw_starts_pat_element(tok: &Token<'_>) -> bool {
    raw_starts_atomic_pat(tok) || matches!(tok, Token::ColonQMark)
}

/// FCS downgrades a byte suffix on a *bare* interpolated string
/// (`$"abc"B`, `$"""abc"""B`, `$@"abc"B`) to `BYTEARRAY` and recovers
/// `SynConst.Bytes(_, _, _)` with FS3377. Map such an opener to the
/// byte-string lit kind we recover to — single (`BYTE_STRING_LIT`),
/// triple (`TRIPLE_BYTE_STRING_LIT`), or verbatim
/// (`VERBATIM_BYTE_STRING_LIT`, projected as `SynByteStringKind.Verbatim`).
/// Fill-bearing and non-byte openers return `None`.
pub(super) fn byte_interp_lit_kind(kind: &crate::lexer::InterpKind) -> Option<SyntaxKind> {
    match kind {
        crate::lexer::InterpKind::BeginEnd { is_byte: true } => Some(SyntaxKind::BYTE_STRING_LIT),
        crate::lexer::InterpKind::TripleBeginEnd { is_byte: true } => {
            Some(SyntaxKind::TRIPLE_BYTE_STRING_LIT)
        }
        crate::lexer::InterpKind::VerbatimBeginEnd { is_byte: true } => {
            Some(SyntaxKind::VERBATIM_BYTE_STRING_LIT)
        }
        _ => None,
    }
}

/// As [`byte_interp_lit_kind`] but reading through a raw token, for use
/// in token-shape predicates like [`raw_starts_const_payload`].
fn byte_interp_lit_kind_for(tok: &Token<'_>) -> Option<SyntaxKind> {
    match tok {
        Token::InterpString(kind) => byte_interp_lit_kind(kind),
        _ => None,
    }
}

/// `true` if `tok` can lead a *minusExpr* (`pars.fsy:5141`) — every
/// atomic starter plus the minus-level prefix forms (`AMP`, `AMP_AMP`,
/// `MINUS`, `PLUS_MINUS_OP "+"/"+."/"-."`, `PERCENT_OP "%"/"%%"`). The
/// minus-level prefixes ONLY start a *fresh* expression here (top of
/// file, paren body, infix RHS, tuple element); they are NOT accepted in
/// arg position — `f - x` is infix application, not `f (-x)` — which is
/// why `raw_starts_atomic_expr` keeps them out.
pub(super) fn raw_starts_minus_expr(tok: &Token<'_>) -> bool {
    if raw_starts_atomic_expr(tok) {
        return true;
    }
    match tok {
        Token::Amp | Token::AmpAmp => true,
        // `IF` lives at `declExpr` level in pars.fsy (`pars.fsy:4324`).
        // It belongs in *expression-start* position (file top, paren body,
        // infix RHS, tuple element), where `parse_pratt_expr`'s `Token::If`
        // dispatch intercepts before reaching `parse_minus_expr`. It is
        // *not* in `raw_starts_atomic_expr`, so it correctly fails arg
        // position (`f if c then a else b` doesn't apply — FCS requires
        // parens around the if).
        Token::If => true,
        // `FUN` lives at `declExpr` level in pars.fsy (`pars.fsy:4318
        // FUN atomicPatterns RARROW typedSeqExprBlockR`). Same role as
        // `If`: legal in expression-start position. Crucial for the raw-
        // stream lookahead in [`Self::peek_is_expr_start`]'s LParen
        // arm — `(fun x -> x) y` needs the raw past `(` to register as
        // a valid expr starter, or the paren-expr dispatch never fires.
        // Like `If`, it's deliberately *not* in `raw_starts_atomic_expr`
        // so arg position (`f fun x -> x`) doesn't apply — FCS requires
        // parens around the lambda there too.
        Token::Fun => true,
        // `yield`/`return`/`yield!`/`return!` are `declExpr`-level keyword
        // prefixes (`pars.fsy:4488`/`:4510`), dispatched in
        // `parse_minus_expr`. Like `if`/`fun` they belong in expression-start
        // position (file top, CE body, paren body, …) but are deliberately
        // *not* in `raw_starts_atomic_expr`, so they don't apply as bare app
        // args (`f yield 1` doesn't parse as `f (yield 1)`). `do!` is absent —
        // it surfaces as a virtual and is handled in the binder slice.
        Token::Yield | Token::Return | Token::YieldBang | Token::ReturnBang => true,
        // `MATCH` lives at `declExpr` level in pars.fsy (`pars.fsy:4221
        // MATCH typedSequentialExpr withClauses`). Same role as `If`/`Fun`:
        // legal in expression-start position (file top, paren/let-RHS body,
        // infix RHS, tuple element) but *not* in `raw_starts_atomic_expr`,
        // so arg position (`f match x with …`) doesn't apply — FCS requires
        // parens around a `match` argument.
        Token::Match => true,
        // `MATCH_BANG` (`match!`) shares `match`'s grammar slot — a raw
        // `declExpr`-level keyword (`pars.fsy:4233`). Same role: legal in
        // expression-start position (incl. the raw-stream LParen lookahead in
        // `peek_is_expr_start` / `is_expr_start_at`, so `(match! x with …)`
        // registers), but *not* in `raw_starts_atomic_expr` (arg position
        // requires parens).
        Token::MatchBang => true,
        // `WHILE` is a `declExpr`-level loop (`pars.fsy:4367`). Same role as
        // `match`/`if`: legal in expression-start position (file top, CE body,
        // let/paren body, …) and reachable through the LParen raw-stream
        // lookahead, but *not* an atomic-arg starter.
        Token::While => true,
        // `WHILE_BANG` (`while!`) shares `while`'s grammar slot — a raw
        // `declExpr`-level loop binder.
        Token::WhileBang => true,
        // `FOR` is a `declExpr`-level loop (`pars.fsy:4372`). Same role as
        // `while`: legal in expression-start position (file top, CE body,
        // let/paren body, …) and reachable through the LParen raw-stream
        // lookahead, but *not* an atomic-arg starter.
        Token::For => true,
        // `TRY` lives at `declExpr` level in pars.fsy (`pars.fsy:4245
        // TRY typedSequentialExprBlockR withClauses`). Same role as
        // `match`/`while`/`for`: legal in expression-start position (file top,
        // CE body, let/paren body, infix RHS, tuple element) and reachable
        // through the LParen raw-stream lookahead (so `(try x with _ -> 0)`
        // registers), but *not* in `raw_starts_atomic_expr`, so arg position
        // (`f try x with …`) doesn't apply — FCS requires parens.
        Token::Try => true,
        // `FUNCTION` lives at `declExpr` level in pars.fsy (the MatchLambda
        // production). Same role as `Match`/`Fun`: legal in expression-start
        // position but *not* in `raw_starts_atomic_expr`. Crucial for the
        // raw-stream lookahead in the LParen arms of `peek_is_expr_start` /
        // `is_expr_start_at` — `(function A -> 1)` needs the raw past `(` to
        // register as a valid expr starter (LexFilter's `Virtual::Function`
        // relabel only surfaces in the filtered stream), or the paren-expr
        // dispatch never fires.
        Token::Function => true,
        // `NEW` opens an object-construction expression `new T(args)` at FCS's
        // `minusExpr` level (`pars.fsy:5173`), the same precedence layer as the
        // address-of / upcast prefixes. So, like `if`/`match`, it belongs in
        // expression-start position (file top, `let` RHS, paren body, tuple
        // element, infix RHS) — `parse_minus_expr`'s `Token::New` dispatch
        // intercepts it there. It is deliberately *not* in
        // `raw_starts_atomic_expr`: `new` is a `minusExpr`, not an `atomicExpr`,
        // so `f new T()` does not apply — FCS requires parens (`f (new T())`).
        Token::New => true,
        // `upcast` / `downcast` are `minusExpr`-level keyword prefixes
        // (`pars.fsy:5182`/`:5185`, → `SynExpr.InferredUpcast`/`InferredDowncast`),
        // the same precedence layer as `new` / the address-of prefixes. Like
        // `new`, they belong in expression-start position (file top, `let` RHS,
        // paren body, tuple element, infix RHS) where `parse_minus_expr`'s
        // `Token::Upcast`/`Token::Downcast` dispatch intercepts them — and they
        // are deliberately *not* in `raw_starts_atomic_expr`: a `minusExpr` is
        // not an `atomicExpr`, so `f upcast x` does not apply (FCS requires
        // `f (upcast x)`). **Must stay in lockstep with that dispatch**: admitting
        // the token here without the dispatch would fall through to
        // `parse_const_payload`'s `unreachable!` and panic.
        Token::Upcast | Token::Downcast => true,
        // `lazy` / `assert` are `declExpr`-level keyword prefixes
        // (`pars.fsy:4346`/`:4349`, → `SynExpr.Lazy`/`SynExpr.Assert`) sitting at
        // FCS's `expr_app` precedence. Like `if`/`new`/`upcast`, they belong in
        // expression-start position (file top, `let` RHS, paren body, tuple
        // element, infix RHS) where `parse_minus_expr`'s `Token::Lazy`/
        // `Token::Assert` dispatch intercepts them. Deliberately *not* in
        // `raw_starts_atomic_expr`: a `declExpr` is not an `atomicExpr`, so
        // `f lazy x` does not apply (FCS requires `f (lazy x)`). **Must stay in
        // lockstep with that dispatch** — admitting the token here without it
        // would fall through to `parse_const_payload`'s `unreachable!`.
        Token::Lazy | Token::Assert => true,
        // `fixed` is the `declExpr`-level pinning prefix (`pars.fsy:4624 FIXED
        // declExpr`, → `SynExpr.Fixed`). Like `lazy`/`if`/`new`, it belongs in
        // expression-start position (file top, `let`/`use` RHS, paren body,
        // tuple element, infix RHS) where `parse_minus_expr`'s `Token::Fixed`
        // dispatch intercepts it. Deliberately *not* in `raw_starts_atomic_expr`:
        // a `declExpr` is not an `atomicExpr`, so `f fixed x` does not apply (FCS
        // rejects it). **Must stay in lockstep with that dispatch** — admitting
        // the token here without it would fall through to `parse_const_payload`'s
        // `unreachable!`. (Unlike `lazy`/`assert`, the dispatched producer parses
        // the operand with the full `parse_expr`, not a tight Pratt frame — see
        // [`Parser::parse_fixed`] — because `FIXED declExpr` has no `%prec` and
        // its operand binds looser than every infix operator.)
        Token::Fixed => true,
        // Same eligible set as `Parser::op_is_minus_expr_prefix` —
        // `IsValidPrefixOperatorUse`'s named PLUS_MINUS_OP cases plus
        // bare `-`/PERCENT_OP `%`/`%%`. Keep these two predicates in
        // lockstep: `peek_is_expr_start` returning `true` here without
        // `op_is_minus_expr_prefix` accepting in `parse_minus_expr`
        // would loop on a token both "starts an expr" and "can't be
        // consumed", crashing the impl-file separator gate.
        Token::Op(text) => matches!(*text, "-" | "+" | "+." | "-." | "?+" | "?-" | "%" | "%%"),
        _ => false,
    }
}

/// `true` if `tok` — the first non-trivia *raw* token immediately after an
/// opening `(` — can begin the **body** of a parenthesised expression (or be
/// the `)` of a unit `()`). This is the single predicate every `(`-after
/// lookahead in expression position shares (`peek_is_expr_start`,
/// `is_expr_start_at`, `peek_starts_app_arg`, `peek_starts_atomic_expr`,
/// `peek_high_precedence_paren_app`, and `parse_atomic_expr`'s LParen
/// dispatch), so they cannot drift apart.
///
/// It is [`raw_starts_minus_expr`] (a paren body is a full expression) plus
/// six cases that predicate deliberately excludes: `RParen` (the unit literal
/// `()`); `Token::Let`/`Use` — a parenthesised block `let`/`use`
/// (`(let a = 1 in a)`) surfaces as `Raw(LParen), Virtual(Let)`, so the raw
/// token past the `(` is `Token::Let`/`Use`, which `raw_starts_minus_expr`
/// keeps out (a bare `let` is not an app-arg / atomic-expr starter; without the
/// `let`/`use` allowance the paren-expr dispatch never fires and the
/// `Virtual::Let` production is unreachable — including as a function argument
/// (`f (let x = 1 in x)`)); `Token::Do` — a parenthesised `do` statement
/// (`(do f)` / `f (do g)`) surfaces as `Raw(LParen), Virtual(Do)` for the same
/// reason (the `do` reaches the filtered stream only as `Virtual::Do`, which
/// `parse_minus_expr` dispatches to `parse_do_expr`), so the raw `Token::Do`
/// past the `(` must be admitted or the paren-expr dispatch never fires;
/// `Token::DotDot` — a parenthesised open-lower range
/// `(..3)` (FCS's `DOT_DOT declExpr`, a `declExpr` but not a `minusExpr`, so
/// `raw_starts_minus_expr` keeps it out); `Token::IntDotDot` — a
/// parenthesised *glued* numeric range `(1..3)` / `(1..)`. The lex-filter splits
/// `IntDotDot("1..")` into `INT32` + `DOT_DOT` in the *filtered* stream, but
/// this lookahead consults the **raw** stream (which still carries the fused
/// token, like every LexFilter split), so a raw `IntDotDot` past the `(` must be
/// admitted here or the common `(1..3)` / `f(1..3)` forms are rejected before
/// `parse_range_expr` ever sees the split; and `Token::Op("*")` — a
/// parenthesised whole-dimension wildcard `( *, 1)` / `f( * )` / `( * )` (FCS's
/// nullary `STAR` leaf — `raw_starts_minus_expr` deliberately keeps `*` out so
/// `f * x` stays infix multiply, but a paren *body* is a full `parse_expr` that
/// does accept the wildcard). The `(*` comment opener never produces a raw
/// `Op("*")` after the `(` (the lexer makes it a comment token), so admitting
/// `Op("*")` here is safe. **Note `( * )` is *not* the parenthesised multiply
/// operator at the parse layer:** FCS's *parser* produces
/// `Paren(IndexRange(None,None))` for it (the operator-value reinterpretation —
/// `let mul = ( * )` — is post-parse), so matching that (rather than deferring
/// like the genuine operator-value `( + )`) is correct against the oracle
/// (`diff_ast_wildcard_paren_only`). The paren body is a full `parse_expr`,
/// which handles the `..` forms via `parse_range_expr` and the `*` via the
/// `minusExpr`-level leaf, so admitting them here gives `(..3)` / `(1..3)` /
/// `( *, 1)` / `( * )` / `f (..3)` / `1 + (..3)` while the bare unparenthesised
/// `1 + ..3` stays a clean error.
pub(super) fn raw_after_lparen_starts_expr(tok: &Token<'_>) -> bool {
    matches!(
        tok,
        Token::RParen
            | Token::Let
            | Token::Use
            | Token::Do
            | Token::DotDot
            | Token::IntDotDot(_)
            // `(..^1)` — a parenthesised open-lower from-end slice. The lex-filter
            // splits `..^` (`DotDotHat`) into `..` + the `^` prefix in the
            // *filtered* stream, but this lookahead consults the **raw** stream
            // (which still carries the fused token), so a raw `DotDotHat` past the
            // `(` must be admitted here — exactly like the fused `IntDotDot` above
            // — or `(..^1)` is rejected before the split is parsed.
            | Token::DotDotHat
            | Token::Op("*")
            // `(#` opens FSharp.Core's inline-IL expression
            // `(# "instr" … #)` (FCS's `inlineAssemblyExpr`, a `parenExprBody`).
            // In expression position a `#` directly after `(` is unambiguously
            // inline IL — there is no other `( #` form — so admitting `Hash`
            // here lets every `(`-after lookahead treat `(# … #)` as the atom
            // it is (arg position, expr start, tuple element). The dispatch in
            // `parse_atomic_expr_head` routes it to `parse_inline_il_expr`
            // *before* the paren-expr arm.
            | Token::Hash
            // `( _.member …` — a parenthesised accessor-function shorthand
            // (`(_.Foo)`, `(_.A, _.B)`, `f (_.Foo)`). `Underscore` is not in
            // `raw_starts_minus_expr` (a bare `_` is not an expression), and this
            // single-token raw predicate can't see the trailing `.`, so it admits
            // `_` broadly; the dot-lambda-vs-error disambiguation happens at the
            // atomic-head dispatch (`Self::at_dot_lambda` → `parse_dot_lambda_expr`
            // vs. the const-expr error arm). A bare `( _ )` thus routes to the
            // paren body and surfaces a clean error there, matching FCS (which
            // reports "Expected '.'" on `(_)`).
            | Token::Underscore
            // `( ?opt …` — a parenthesised optional named argument
            // (`M(?opt = value)`, `M(?opt)`). Like `Underscore`, `?` is not in
            // `raw_starts_minus_expr` (a bare `?` is the postfix dynamic
            // operator), and this single-token check can't see the trailing
            // ident, so it admits `?` broadly; the optional-arg-vs-error
            // disambiguation happens at the atomic-head dispatch
            // (`Self::qmark_opens_optional_arg`). A bare `( ? )` routes to the
            // paren body and surfaces a clean error there.
            | Token::QMark
    ) || raw_starts_minus_expr(tok)
        // A parenthesised operator-value `( op )` (FCS's `opName`,
        // `pars.fsy:6793`). The token right after `(` being an operator name
        // means the `(` *can* open an expression even when that operator can't
        // otherwise lead one (the infix-only `(|>)`, `(=)`, `(<)`, …). This
        // single-token check admits `( op …` broadly; the actual operator-value
        // vs paren-body disambiguation (whether `)` immediately follows) lives
        // in [`super::Parser::at_paren_op_value`], so a non-immediate-`)` form
        // like `( |> x )` still falls through to `parse_paren_expr` and errors
        // cleanly. Already-admitted prefix-able ops (`+`, `-`, `&`, …) are a
        // harmless overlap.
        || is_paren_operator_name(tok)
        // A bare `|` immediately after `(` opens an active-pattern-name value
        // `(|Foo|_|)` / `(|Foo|Bar|)` (FCS's `identExpr: opName`, where `opName`
        // includes the active-pattern productions `pars.fsy:6812-6819`). Like
        // the operator-value disjunct above, the name is an *atomic value*, not
        // a paren *body* — the `(` opens an expression even though `|` could
        // never lead an inner expression. The full disambiguation (and the
        // swallowed `)` close) lives at the atomic-head dispatch
        // ([`super::Parser::at_active_pat_name`] →
        // [`super::Parser::parse_active_pat_name_expr`]). The operator-value
        // pipes (`(|>)` / `(||)`) glue into `Op` / `BarBar`, never a bare
        // `Bar`, so they stay on the operator-value path; the `aftertype`-arg
        // gate excludes this form (FCS rejects `new C(|Foo|_|)`).
        || matches!(tok, Token::Bar)
}

/// `true` if `tok` is an operator-name token that can sit inside `( … )` as a
/// parenthesised operator-value — FCS's `operatorName` non-terminal
/// (`pars.fsy:6826`), restricted to the single-token forms. Used both by the
/// `(`-after expr-start gate ([`raw_after_lparen_starts_expr`]) and by
/// [`super::Parser::at_paren_op_value`], which additionally checks the closing
/// `)`.
///
/// Notable boundaries against FCS's `operatorName`:
/// * **`Op("*")` is excluded.** A spaced `( * )` is the whole-dimension
///   wildcard `Paren(IndexRange(None, None))`, *not* the multiply
///   operator-value (FCS resolves the grammar conflict toward the paren-body —
///   see [`raw_after_lparen_starts_expr`]'s `Op("*")` arm). The *glued* `(*)`
///   multiply operator-value is the lexer's dedicated [`Token::LParenStarRParen`]
///   token, handled separately.
/// * **`DotDot` (`..`) is included** — `(..)` is `op_Range`, but the
///   immediate-`)` check keeps `(..3)` an open-ended `IndexRange`.
/// * `DotDotHat` (`..^`), single `Bar` (`|`), `QMarkQMark` (`??`) and
///   `ColonColon` (`::`) are **excluded** — FCS rejects `(..^)`, `(|)`, `(??)`
///   as operator-values, and `(::)` is the special cons form handled elsewhere.
/// * `FunkyOpName` — the clean subset (`.[]`, `.()`, `.()<-`) *is* admitted (see
///   [`is_clean_funky_operator_name`]); the deprecated / comma / slice spellings
///   (`.[]<-`, `.[,]`, `.[..]`, …) stay clean errors, matching FCS's
///   `deprecatedOperator` parse error. Active-pattern names (`(|A|B|)`) — also an
///   `opName` in FCS — are handled, but off the bare-`Bar`-after-`(` lookahead
///   ([`raw_after_lparen_starts_expr`] / [`super::Parser::at_active_pat_name`]),
///   *not* this single-token operator predicate.
/// * **`Or` (`(or)`) is included** — the ML-compat boolean operator name. Current
///   FCS has *removed* `OR` from `operatorName`, so this is a deliberate
///   divergence: `(or)` is valid, shipped FSharp.Core source (via SourceLink),
///   parses warn-only (FS0086) under FsAutoComplete, and an LSP serving real
///   source must read it. See the `Token::Or` arm below and
///   `docs/fcs-divergences.md`. (`(&)` needs no special arm — `Token::Amp` is
///   already admitted above; `and` was never an FCS operator name.)
pub(super) fn is_paren_operator_name(tok: &Token<'_>) -> bool {
    matches!(
        tok,
        Token::Equals
            | Token::Less(_)
            | Token::Greater(_)
            | Token::QMark
            | Token::Amp
            | Token::AmpAmp
            | Token::BarBar
            | Token::ColonEquals
            | Token::Dollar
            | Token::DotDot
            // `(or)` — the ML-compat boolean operator name. `or` lexes to the
            // [`Token::Or`] keyword (not a `Token::Op`), so it needs an explicit
            // arm; `(&)` (`Token::Amp`) is already covered above. FCS's grammar
            // carried `operatorName: OR { "or" }` historically and recent FCS has
            // dropped it, but it is valid, shipped FSharp.Core source (reachable
            // via SourceLink) and parses cleanly under FsAutoComplete — only a
            // *semantic* FS0086 "operator should not normally be redefined"
            // warning, never a parse error. We admit it into the permissive union
            // surface (the D7 "incomplete, never wrong" floor): an LSP serving
            // real source must read `let (or) e1 e2 = …` rather than error on it.
            // A deliberate, documented divergence from current FCS (see
            // `docs/fcs-divergences.md`); no differential coverage, since FCS now
            // errors. `and` is *not* an operator name (FCS never admitted it), so
            // only `Token::Or` joins here.
            | Token::Or
    ) || matches!(tok, Token::Op(s) if *s != "*")
        || matches!(tok, Token::FunkyOpName(s) if is_clean_funky_operator_name(s))
}

/// `true` for the fused index-operator names FCS's `operatorName:
/// FUNKY_OPERATOR_NAME` production (`pars.fsy:6890`) admits *without* a parse
/// error. FCS accepts the whole `FUNKY_OPERATOR_NAME` token there, but reports
/// `deprecatedOperator` (FS0035, severity Error — a parse-phase diagnostic) for
/// every spelling except `.[]`, `.()`, `.()<-`:
///
/// ```text
/// | FUNKY_OPERATOR_NAME
///    { if $1 <> ".[]" && $1 <> ".()" && $1 <> ".()<-" then
///          deprecatedOperator (lhs parseState)
///      $1 }
/// ```
///
/// So exactly those three are clean; the comma/slice forms (`.[,]`, `.[..]`, …)
/// and the deprecated `.[]<-` stay parse errors on both sides (`both_reject`).
/// Admitting only the clean set keeps the "we accept ⟹ FCS accepts" invariant —
/// accepting a deprecated form would be a we-accept/FCS-rejects divergence. The
/// [`Token::FunkyOpName`] text is bumped verbatim under `IDENT_TOK` by
/// [`super::Parser::consume_paren_op_value`]; the differential normaliser unwraps
/// FCS's mangled `op_ArrayLookup` / `op_ArrayAssign` / `op_DotLBrackRBrack` plus
/// `OriginalNotationWithParen` back to that same source spelling.
pub(super) fn is_clean_funky_operator_name(s: &str) -> bool {
    matches!(s, ".[]" | ".()" | ".()<-")
}

/// `true` if `tok` can lead an atomic type (FCS's `atomType`,
/// `pars.fsy:6552-…`): plain or backticked identifier for `LONG_IDENT_TYPE`,
/// `_` for `ANON_TYPE`, `(` for `PAREN_TYPE`, `'` for the plain-typar
/// `VAR_TYPE`, `^` (as a `Token::Op`) for the head-typar `VAR_TYPE`, `#` for a
/// hash constraint, and (phase 10.9) the type-provider static-constant heads:
/// a bare literal (`rawConstant` / `TRUE` / `FALSE` → `StaticConstant`), `null`
/// (`StaticConstantNull`), and `const` (`StaticConstantExpr`). Used to gate type
/// acceptance against the *raw* stream — a LexFilter-swallowed `)` between the
/// filtered cursor and the next filtered token must NOT be treated as if the
/// type body extends past it; gating on the raw stream stops the recovery from
/// consuming tokens outside the surrounding parens.
pub(super) fn raw_starts_atomic_type(tok: &Token<'_>) -> bool {
    match tok {
        Token::Ident(_)
        | Token::QuotedIdent(_)
        // `global.Path` — the global-namespace root as a type-path head (FCS's
        // `GLOBAL DOT …`). FCS treats the `global` keyword as an identifier
        // heading a `SynType.LongIdent`, so admit it as an atomic-type start
        // wherever a path head can stand.
        | Token::Global
        | Token::Underscore
        | Token::LParen
        | Token::Quote
        | Token::Hash
        // Static-constant heads (phase 10.9). `Null`/`Const` are explicit;
        // the literal forms reuse `raw_starts_const_payload` (which also
        // accepts `LParen`/`True`/`False` — `LParen` is the `PAREN_TYPE`
        // head matched above, `True`/`False` are the bool `StaticConstant`).
        | Token::Null
        | Token::Const => true,
        Token::Op(text) => *text == "^",
        other => raw_starts_const_payload(other),
    }
}

/// `true` iff `tok` unconditionally leads an anon-record type — FCS's
/// `anonRecdType: [STRUCT] LBRACE_BAR recdFieldDeclList bar_rbrace`
/// (`pars.fsy:2510-2522`), restricted to the *bare* `{|` head. The
/// optional `STRUCT` prefix is **not** included here: a bare `struct`
/// followed by anything other than `{|` is a different construct
/// (e.g. `struct (int * int)`, struct-typed declarations) and accepting
/// it at this layer would let the recovery gate dispatch into
/// [`super::Parser::parse_app_type`] which would then panic inside
/// [`super::Parser::parse_atomic_type`]'s unreachable arm. The two-token
/// `struct {|` case is handled by [`super::Parser::peek_starts_type_or_anon_recd`]
/// on the parser side.
///
/// Combined with [`raw_starts_atomic_type`] (plus the struct-lookahead
/// check on `&self`) this is FCS's `atomTypeOrAnonRecdType`
/// (`pars.fsy:6520`) — the layer [`super::Parser::parse_app_type`] sits on.
/// The two predicates are kept separate so the hash branch in
/// [`super::Parser::parse_atomic_type`] (which is FCS's `atomType`, strictly
/// *not* `atomTypeOrAnonRecdType`) can recurse with the strict gate
/// alone and reject `#{| F : int |}` at the recovery point the same
/// way FCS does.
pub(super) fn raw_starts_anon_recd_type(tok: &Token<'_>) -> bool {
    matches!(tok, Token::LBraceBar)
}

/// `true` if `tok` starts an FCS `appTypeConPower` postfix-app head
/// (`pars.fsy:6344-6355`), i.e. an `appTypeCon` → `path` or `typar`.
/// Strict subset of [`raw_starts_atomic_type`]: excludes `LParen`
/// (parenthesised types) and `Underscore` (anon `_`), neither of which
/// can serve as the head of a postfix application. The `^` op is
/// included for the head-typar `^T` form (mirroring
/// [`raw_starts_atomic_type`]).
///
/// Used by [`super::Parser::parse_app_type`] to decide whether to loop after
/// the running atomic — the raw-stream lookahead ensures a LexFilter-
/// swallowed `)` between the LHS and the next filtered token isn't
/// crossed (pinned by
/// `app_type_post_head_lookahead_does_not_cross_swallowed_rparen`).
pub(super) fn raw_starts_postfix_app_head(tok: &Token<'_>) -> bool {
    match tok {
        // `Token::Global` heads a `global.Path` type constructor, an
        // `appTypeCon` path like any ident (`int global.Foo` ⇒ `global.Foo<int>`).
        Token::Ident(_) | Token::QuotedIdent(_) | Token::Quote | Token::Global => true,
        Token::Op(text) => *text == "^",
        _ => false,
    }
}

/// `true` iff `tok` is an integer literal FCS admits as a unit-of-measure
/// exponent (phase 10.8) — one that classifies to
/// [`SyntaxKind::INT32_LIT`]: a decimal / hex / oct / bin literal, or a
/// lowercase-`l`-suffixed Int32. A `1L` (Int64) or `1uy` (byte) suffix is a
/// *different* terminal that FCS rejects in the exponent position (it reports
/// "Unexpected integer literal in type arguments"), so it is excluded here.
pub(super) fn token_is_int32_exponent(tok: &Token<'_>) -> bool {
    match tok {
        Token::Int(_) | Token::XInt(_) => true,
        Token::IntSuffixed(text) | Token::XIntSuffixed(text) => {
            matches!(classify_suffixed_int(text), Ok(SyntaxKind::INT32_LIT))
        }
        _ => false,
    }
}

/// `true` iff `tok` is an integer literal denoting zero, in any radix and
/// with or without a `sign_fold`-merged sign / lowercase-`l` Int32 suffix.
/// Drives FCS's "Denominator must not be 0 in unit-of-measure exponent" parse
/// error (`pars.fsy:3487`). A literal is zero iff every digit — after
/// stripping the sign, the `0x`/`0o`/`0b` radix prefix, the `l` suffix, and
/// `_` separators — is `'0'`.
pub(super) fn int32_exponent_is_zero(tok: &Token<'_>) -> bool {
    let text = match tok {
        Token::Int(t) | Token::XInt(t) | Token::IntSuffixed(t) | Token::XIntSuffixed(t) => *t,
        _ => return false,
    };
    let (_, body) = split_fold_sign(text);
    let body = body.strip_suffix('l').unwrap_or(body);
    let body = body
        .strip_prefix("0x")
        .or_else(|| body.strip_prefix("0X"))
        .or_else(|| body.strip_prefix("0o"))
        .or_else(|| body.strip_prefix("0O"))
        .or_else(|| body.strip_prefix("0b"))
        .or_else(|| body.strip_prefix("0B"))
        .unwrap_or(body);
    let digits: Vec<char> = body.chars().filter(|&c| c != '_').collect();
    !digits.is_empty() && digits.iter().all(|&c| c == '0')
}

/// `true` iff `tok` is an integer literal denoting `1`, in any radix and with
/// or without a lowercase-`l` Int32 suffix or `_` separators. Drives the
/// unit-of-measure `measureTypePower: INT32` arm (`pars.fsy:6732`), where FCS
/// compares the *decoded* `INT32` value against `1` (so `0x1`, `0o1`, `1l` are
/// all the dimensionless [`SyntaxKind::MEASURE_ONE`](crate::syntax::SyntaxKind::MEASURE_ONE)
/// with no error; only a different value is the "unexpected integer literal"
/// error). Decoding mirrors [`int32_exponent_is_zero`]'s prefix/suffix
/// stripping, then parses the body in its radix.
pub(super) fn int32_exponent_is_one(tok: &Token<'_>) -> bool {
    let text = match tok {
        Token::Int(t) | Token::XInt(t) | Token::IntSuffixed(t) | Token::XIntSuffixed(t) => *t,
        _ => return false,
    };
    let (_, body) = split_fold_sign(text);
    let body = body.strip_suffix('l').unwrap_or(body);
    let (radix, digits) =
        if let Some(b) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (16, b)
        } else if let Some(b) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            (8, b)
        } else if let Some(b) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (2, b)
        } else {
            (10, body)
        };
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    i64::from_str_radix(&cleaned, radix).is_ok_and(|v| v == 1)
}

/// Mirror of FCS's `String.isLeadingIdentifierCharacterUpperCase`
/// (`../fsharp/src/Compiler/Utilities/illib.fs:740`). Drives the
/// `SynPat.LongIdent` vs `SynPat.Named` classification at every
/// `atomicPatternLongIdent` site (`pars.fsy:3810`): uppercase head →
/// `SynPat.LongIdent` (constructor-like); lowercase head →
/// `SynPat.Named` (value binder).
///
/// FCS's check (paraphrased):
/// ```text
/// let isUpper = Char.IsUpper c
/// if isUpper = Char.IsLower c then Char.IsLetter c else isUpper
/// ```
/// where `Char.IsUpper` is `Lu`-only, `Char.IsLower` is `Ll`-only,
/// and `Char.IsLetter` is `Lu|Ll|Lt|Lm|Lo`. Equivalently the
/// classifier returns true iff `c ∈ {Lu, Lt, Lm, Lo}` — a letter,
/// but not lower-case.
///
/// Rust's `char::is_uppercase`, `char::is_lowercase`, and
/// `char::is_alphabetic` use Unicode's *derived* `Uppercase`,
/// `Lowercase`, and `Alphabetic` properties, which are broader than
/// the .NET BCL's strict general-category checks. We have to
/// subtract `Other_Uppercase`, `Other_Lowercase`, `Nl`, and
/// `Other_Alphabetic` to recover the .NET-aligned answers:
///
/// * `Other_Uppercase` (Unicode 16.0): Roman numerals `U+2160..216F`
///   (in `Nl`) and circled Latin caps `U+24B6..24CF` (in `So`). Both
///   blocks make `c.is_uppercase()` return `true` even though
///   `Char.IsUpper` returns `false`.
/// * `Other_Lowercase` (Unicode 16.0): a 22-range set across
///   ordinal indicators (`U+00AA`, `U+00BA` — `Lo`), modifier-letter
///   blocks (`U+02B0..02B8`, `U+1D2C..1D6A`, … — `Lm`), small
///   Roman numerals `U+2170..217F` (`Nl`), circled Latin small
///   letters `U+24D0..24E9` (`So`), and others. Without subtracting
///   these, an `Lm`/`Lo` head like `let ᵃ = 0` would mis-classify as
///   `Named` because Rust reports it lower-case.
/// * `Nl` (Letter_Number) is part of Rust's `Alphabetic` property
///   but not of .NET's `IsLetter`, so an uncased Nl head like
///   `let ᛮ = 0` would mis-classify as `LongIdent` without the
///   subtraction.
/// * `Other_Alphabetic` non-letters — chiefly `Mn` combining marks,
///   `Mc` spacing marks, and `So` circled-letter symbols — are also
///   part of Rust's `Alphabetic` property but not .NET `IsLetter`.
///   These are reachable through backtick-quoted idents such as
///   `` ``\u{0345}`` `` and must classify as `Named`.
///
/// For `Token::QuotedIdent` the leading/trailing `` `` `` are
/// stripped before classification — FCS's `Ident.idText` holds the
/// unescaped form, so `` ``Foo`` `` classifies as uppercase via the
/// `F`. Quoted idents whose stripped content begins with a digit or
/// symbol (e.g. `` ``2x`` ``) classify as `Named`, matching FCS's
/// `IsLetter`-false fallthrough.
///
/// Non-BMP heads (codepoint ≥ `0x10000`) always classify as `Named`.
/// FCS reads `s[0]` of the .NET (UTF-16) string, which is the lone
/// high surrogate — `Char.IsUpper`/`IsLower`/`IsLetter` all return
/// `false` for surrogates, so the binder lowers to `Named`. We
/// short-circuit before consulting Rust's predicates because Rust
/// decodes the full scalar value and would otherwise classify
/// e.g. Deseret capitals (`U+10400..1044F`) as `LongIdent`. The
/// guard also makes the `Other_Uppercase`, `Other_Lowercase`, `Nl`,
/// and `Other_Alphabetic` tables BMP-only, eliminating their plane-1
/// entries.
pub(super) fn ident_text_leads_uppercase(text: &str) -> bool {
    let stripped = text
        .strip_prefix("``")
        .and_then(|t| t.strip_suffix("``"))
        .unwrap_or(text);
    let Some(c) = stripped.chars().next() else {
        return false;
    };
    if (c as u32) > 0xFFFF {
        return false;
    }
    let is_upper = dotnet_is_upper(c);
    let is_lower = dotnet_is_lower(c);
    if is_upper == is_lower {
        c.is_alphabetic() && !is_letter_number(c) && !is_other_alphabetic_non_letter(c)
    } else {
        is_upper
    }
}

/// `true` iff `c` is in Unicode general category `Lu` — exact mirror
/// of .NET's `Char.IsUpper`. Subtracts `Other_Uppercase` from Rust's
/// `is_uppercase()` so that `Other_Uppercase`-only characters
/// (Roman numerals `U+2160..216F` in `Nl`; circled Latin caps
/// `U+24B6..24CF` in `So`) classify as `false`.
fn dotnet_is_upper(c: char) -> bool {
    c.is_uppercase() && !is_other_uppercase(c)
}

/// `true` iff `c` is in Unicode general category `Ll` — exact mirror
/// of .NET's `Char.IsLower`. Subtracts `Other_Lowercase` from Rust's
/// `is_lowercase()` so that `Other_Lowercase`-only characters
/// (modifier letters in `1D2C..1D6A`, ordinal indicators `00AA`/
/// `00BA`, small Roman numerals `2170..217F`, …) classify as
/// `false`.
fn dotnet_is_lower(c: char) -> bool {
    c.is_lowercase() && !is_other_lowercase(c)
}

/// `true` iff `c` is in Unicode general category `Nl`
/// (Letter_Number) per Unicode 16.0 — BMP entries only, since the
/// caller's non-BMP guard short-circuits plane-1 codepoints before
/// this is consulted.
///
/// `U+2183..U+2184` are not `Nl` (`Lu` and `Ll` respectively) so
/// the Roman-numeral range splits at that gap.
fn is_letter_number(c: char) -> bool {
    matches!(
        c as u32,
        0x16EE..=0x16F0
            | 0x2160..=0x2182
            | 0x2185..=0x2188
            | 0x3007
            | 0x3021..=0x3029
            | 0x3038..=0x303A
            | 0xA6E6..=0xA6EF
    )
}

/// `true` iff `c` has the Unicode `Other_Uppercase` derived property
/// per Unicode 16.0 (`PropList.txt`) — BMP entries only.
///
/// Rust's `is_uppercase()` is `Lu | Other_Uppercase`; subtracting
/// this recovers `Lu`-only. Non-BMP `Other_Uppercase` blocks
/// (`U+1F130..1F149` squared caps, `U+1F150..1F169` and
/// `U+1F170..1F189` negative circled/squared caps) are unreachable
/// because the caller short-circuits non-BMP scalars first.
fn is_other_uppercase(c: char) -> bool {
    matches!(c as u32, 0x2160..=0x216F | 0x24B6..=0x24CF)
}

/// `true` iff `c` has the Unicode `Other_Lowercase` derived property
/// per Unicode 16.0 (`PropList.txt`) — BMP entries only.
///
/// Rust's `is_lowercase()` is `Ll | Other_Lowercase`; subtracting
/// this recovers `Ll`-only. Non-BMP `Other_Lowercase` blocks
/// (modifier letters in `U+10780..107BA`, Cyrillic modifiers in
/// `U+1E030..1E06D`) are unreachable because the caller
/// short-circuits non-BMP scalars first.
///
/// `U+AB69` is a detached singleton between `AB5C..AB5F` and `Lo`
/// territory; it has to be listed explicitly because it's not
/// adjacent to the surrounding `AB5C..AB5F` block.
fn is_other_lowercase(c: char) -> bool {
    matches!(
        c as u32,
        0x00AA
            | 0x00BA
            | 0x02B0..=0x02B8
            | 0x02C0..=0x02C1
            | 0x02E0..=0x02E4
            | 0x0345
            | 0x037A
            | 0x10FC
            | 0x1D2C..=0x1D6A
            | 0x1D78
            | 0x1D9B..=0x1DBF
            | 0x2071
            | 0x207F
            | 0x2090..=0x209C
            | 0x2170..=0x217F
            | 0x24D0..=0x24E9
            | 0x2C7C..=0x2C7D
            | 0xA69C..=0xA69D
            | 0xA770
            | 0xA7F2..=0xA7F4
            | 0xA7F8..=0xA7F9
            | 0xAB5C..=0xAB5F
            | 0xAB69
    )
}

/// `true` iff `c` has the BMP portion of Unicode's `Other_Alphabetic`
/// derived property and is not a .NET `Char.IsLetter`. This is generated
/// from `regex-syntax`'s vendored Unicode 16.0 table; non-BMP ranges are
/// unreachable because [`ident_text_leads_uppercase`] short-circuits first.
fn is_other_alphabetic_non_letter(c: char) -> bool {
    matches!(
        c as u32,
        0x0345
            | 0x0363..=0x036F
            | 0x05B0..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5
            | 0x05C7
            | 0x0610..=0x061A
            | 0x064B..=0x0657
            | 0x0659..=0x065F
            | 0x0670
            | 0x06D6..=0x06DC
            | 0x06E1..=0x06E4
            | 0x06E7..=0x06E8
            | 0x06ED
            | 0x0711
            | 0x0730..=0x073F
            | 0x07A6..=0x07B0
            | 0x0816..=0x0817
            | 0x081B..=0x0823
            | 0x0825..=0x0827
            | 0x0829..=0x082C
            | 0x0897
            | 0x08D4..=0x08DF
            | 0x08E3..=0x08E9
            | 0x08F0..=0x0903
            | 0x093A..=0x093B
            | 0x093E..=0x094C
            | 0x094E..=0x094F
            | 0x0955..=0x0957
            | 0x0962..=0x0963
            | 0x0981..=0x0983
            | 0x09BE..=0x09C4
            | 0x09C7..=0x09C8
            | 0x09CB..=0x09CC
            | 0x09D7
            | 0x09E2..=0x09E3
            | 0x0A01..=0x0A03
            | 0x0A3E..=0x0A42
            | 0x0A47..=0x0A48
            | 0x0A4B..=0x0A4C
            | 0x0A51
            | 0x0A70..=0x0A71
            | 0x0A75
            | 0x0A81..=0x0A83
            | 0x0ABE..=0x0AC5
            | 0x0AC7..=0x0AC9
            | 0x0ACB..=0x0ACC
            | 0x0AE2..=0x0AE3
            | 0x0AFA..=0x0AFC
            | 0x0B01..=0x0B03
            | 0x0B3E..=0x0B44
            | 0x0B47..=0x0B48
            | 0x0B4B..=0x0B4C
            | 0x0B56..=0x0B57
            | 0x0B62..=0x0B63
            | 0x0B82
            | 0x0BBE..=0x0BC2
            | 0x0BC6..=0x0BC8
            | 0x0BCA..=0x0BCC
            | 0x0BD7
            | 0x0C00..=0x0C04
            | 0x0C3E..=0x0C44
            | 0x0C46..=0x0C48
            | 0x0C4A..=0x0C4C
            | 0x0C55..=0x0C56
            | 0x0C62..=0x0C63
            | 0x0C81..=0x0C83
            | 0x0CBE..=0x0CC4
            | 0x0CC6..=0x0CC8
            | 0x0CCA..=0x0CCC
            | 0x0CD5..=0x0CD6
            | 0x0CE2..=0x0CE3
            | 0x0CF3
            | 0x0D00..=0x0D03
            | 0x0D3E..=0x0D44
            | 0x0D46..=0x0D48
            | 0x0D4A..=0x0D4C
            | 0x0D57
            | 0x0D62..=0x0D63
            | 0x0D81..=0x0D83
            | 0x0DCF..=0x0DD4
            | 0x0DD6
            | 0x0DD8..=0x0DDF
            | 0x0DF2..=0x0DF3
            | 0x0E31
            | 0x0E34..=0x0E3A
            | 0x0E4D
            | 0x0EB1
            | 0x0EB4..=0x0EB9
            | 0x0EBB..=0x0EBC
            | 0x0ECD
            | 0x0F71..=0x0F83
            | 0x0F8D..=0x0F97
            | 0x0F99..=0x0FBC
            | 0x102B..=0x1036
            | 0x1038
            | 0x103B..=0x103E
            | 0x1056..=0x1059
            | 0x105E..=0x1060
            | 0x1062..=0x1064
            | 0x1067..=0x106D
            | 0x1071..=0x1074
            | 0x1082..=0x108D
            | 0x108F
            | 0x109A..=0x109D
            | 0x1712..=0x1713
            | 0x1732..=0x1733
            | 0x1752..=0x1753
            | 0x1772..=0x1773
            | 0x17B6..=0x17C8
            | 0x1885..=0x1886
            | 0x18A9
            | 0x1920..=0x192B
            | 0x1930..=0x1938
            | 0x1A17..=0x1A1B
            | 0x1A55..=0x1A5E
            | 0x1A61..=0x1A74
            | 0x1ABF..=0x1AC0
            | 0x1ACC..=0x1ACE
            | 0x1B00..=0x1B04
            | 0x1B35..=0x1B43
            | 0x1B80..=0x1B82
            | 0x1BA1..=0x1BA9
            | 0x1BAC..=0x1BAD
            | 0x1BE7..=0x1BF1
            | 0x1C24..=0x1C36
            | 0x1DD3..=0x1DF4
            | 0x24B6..=0x24E9
            | 0x2DE0..=0x2DFF
            | 0xA674..=0xA67B
            | 0xA69E..=0xA69F
            | 0xA802
            | 0xA80B
            | 0xA823..=0xA827
            | 0xA880..=0xA881
            | 0xA8B4..=0xA8C3
            | 0xA8C5
            | 0xA8FF
            | 0xA926..=0xA92A
            | 0xA947..=0xA952
            | 0xA980..=0xA983
            | 0xA9B4..=0xA9BF
            | 0xA9E5
            | 0xAA29..=0xAA36
            | 0xAA43
            | 0xAA4C..=0xAA4D
            | 0xAA7B..=0xAA7D
            | 0xAAB0
            | 0xAAB2..=0xAAB4
            | 0xAAB7..=0xAAB8
            | 0xAABE
            | 0xAAEB..=0xAAEF
            | 0xAAF5
            | 0xABE3..=0xABEA
            | 0xFB1E
    )
}

/// `true` if `text` is a `PREFIX_OP` per `lex.fsl:986`
/// (`ignored_op_char* '!' op_char*` or `ignored_op_char* '~' op_char*`).
/// The `[.$?]*` leading run is *ignored* for classification (same as
/// [`classify_op_text`]). `!=` is excluded: at equal match length
/// `INFIX_COMPARE_OP` (lex.fsl:978) wins by lex-rule source order — so
/// `!=` is the comparator, not a prefix.
///
/// fslex tie-breaks on source order at equal match length. Among rules
/// 970–986, only rule 978 (`= | != | < | $` head) can match a string
/// whose head-after-ignored is `!`/`~` *and* whose ignored prefix
/// contains a `$` (since `$` is itself a valid 978 head, consumed
/// non-greedily). When that happens, 978 wins → the token is INFIX,
/// not PREFIX. e.g. `$!` is INFIX_COMPARE_OP, not a prefix.
///
/// Concrete inclusions: `!`, `!+`, `!~`, `~`, `~~~`, `.!`, `?~`.
/// Concrete exclusions: `!=`, `==`, `<-`, `+`, `&`, `$!`, `$~~`,
/// `.$!`.
pub(super) fn is_prefix_op_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b'.' | b'$' | b'?') {
        i += 1;
    }
    let is_prefix_shape = match bytes.get(i) {
        Some(b'~') => true,
        Some(b'!') => bytes.get(i + 1) != Some(&b'='),
        _ => false,
    };
    if !is_prefix_shape {
        return false;
    }
    // If the ignored prefix contains a `$`, rule 978 (INFIX_COMPARE_OP)
    // also matches with the same length and wins by source order.
    !bytes[..i].contains(&b'$')
}

/// Classify an `Op(text)` raw lexer token into the Pratt binding-power
/// pair `(lbp, rbp)` for [`super::Parser::peek_infix_op`]. Mirrors FCS's
/// `lex.fsl` (lines 970–984) `ignored_op_char* <first> op_char*` regex
/// family: a leading run of `.`, `$`, `?` is *ignored* for classification
/// (so `.+` reads as `+`, `?>` reads as `>`, `$=` reads as `=`); the next
/// character then picks the precedence bucket. Returns `None` for the
/// few all-ignored shapes (`..`, `...`) which FCS treats as semantically
/// distinct tokens (range operator etc.) rather than `mkSynInfix`-style
/// binary applications — phase 3.4 doesn't model them.
pub(super) fn classify_op_text(s: &str) -> Option<(u16, u16)> {
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n == 0 {
        return None;
    }
    // Greedy `ignored_op_char* = [.$?]*` prefix — same as fslex.
    let mut greedy_end = 0;
    while greedy_end < n && matches!(bytes[greedy_end], b'.' | b'$' | b'?') {
        greedy_end += 1;
    }
    let head = bytes.get(greedy_end).copied();
    // fslex matches the longest rule; on equal length, the rule that
    // appears EARLIEST in lex.fsl wins. Since every rule below matches
    // the *entire* input (the trailing `op_char*` accepts the rest),
    // all matches have length `n`, so the tie-break is purely rule
    // order. Iterate rules in lex.fsl order and return the first that
    // applies.
    //
    // Lines 970–984 in `src/Compiler/lex.fsl` (the FCS submodule):
    // - 970: ignored* `**`   op_char*  → INFIX_STAR_STAR_OP   (right, 80)
    // - 972: ignored* `*/%`  op_char*  → INFIX_STAR_DIV_MOD_OP (left,  70)
    // - 974: ignored* `+-`   op_char*  → PLUS_MINUS_OP        (left,  60)
    // - 976: ignored* `@^`   op_char*  → INFIX_AT_HAT_OP      (right, 40)
    // - 978: ignored* (`= != < $`) op_char* → INFIX_COMPARE_OP (left, 30)
    // - 980: ignored* `>`    op_char*  → INFIX_COMPARE_OP     (left,  30)
    // - 982: ignored* `&`    op_char*  → INFIX_AMP_OP         (left,  30)
    // - 984: ignored* `|`    op_char*  → INFIX_BAR_OP         (left,  30)
    // - 986: ignored* `!~`   op_char*  → PREFIX_OP            (not infix)
    if head == Some(b'*') && bytes.get(greedy_end + 1) == Some(&b'*') {
        return Some((80, 80));
    }
    if matches!(head, Some(b'*' | b'/' | b'%')) {
        return Some((70, 71));
    }
    if matches!(head, Some(b'+' | b'-')) {
        return Some((60, 61));
    }
    if matches!(head, Some(b'@' | b'^')) {
        return Some((40, 40));
    }
    // INFIX_COMPARE_OP head set: `=`, `<`, `$`, or literal `!=`.
    if matches!(head, Some(b'=' | b'<' | b'$')) {
        return Some((30, 31));
    }
    if head == Some(b'!') && bytes.get(greedy_end + 1) == Some(&b'=') {
        return Some((30, 31));
    }
    // `$` as the operator head can also appear *within* the greedy
    // ignored prefix (since `$` is in both the ignored set AND the
    // line-978 head set). Lines 970–976 lose to line 978 here only if
    // their head pattern isn't otherwise matched at `greedy_end` — but
    // we've already returned for all those head bytes. So fall back to
    // line 978 by hunting for any `$` in the prefix.
    if bytes[..greedy_end].contains(&b'$') {
        return Some((30, 31));
    }
    // INFIX_COMPARE_OP via `>` (line 980), INFIX_AMP_OP via `&` (982),
    // INFIX_BAR_OP via `|` (984) — all share precedence band 30.
    if matches!(head, Some(b'>' | b'&' | b'|')) {
        return Some((30, 31));
    }
    // `!`-headed (other than `!=`) and `~`-headed are PREFIX_OP per
    // line 986 — *not* infix. Falls through to None.
    None
}

/// Map a raw lexer trivia variant to its [`SyntaxKind`]. Returns `None` for
/// non-trivia tokens.
pub(super) fn trivia_kind(tok: &Token<'_>) -> Option<SyntaxKind> {
    match tok {
        Token::Whitespace => Some(SyntaxKind::WHITESPACE),
        Token::Newline => Some(SyntaxKind::NEWLINE),
        Token::LineComment => Some(SyntaxKind::LINE_COMMENT),
        Token::BlockComment => Some(SyntaxKind::BLOCK_COMMENT),
        _ => None,
    }
}

/// `true` if a raw-stream entry is trivia for *lookahead* purposes: active
/// whitespace / comments ([`trivia_kind`]), or a directive / inactive-code
/// marker ([`TriviaToken::HashIf`] … [`TriviaToken::InactiveCode`]). Raw
/// scanners skip these the way they skip whitespace.
pub(super) fn raw_is_trivia(tt: &TriviaToken<'_>) -> bool {
    match tt {
        TriviaToken::Lexed(t) => trivia_kind(t).is_some(),
        _ => true,
    }
}

/// The significant (non-trivia) real token a raw entry carries, or `None`
/// if it is active trivia or a directive / inactive-code marker.
pub(super) fn raw_significant<'a, 'src>(tt: &'a TriviaToken<'src>) -> Option<&'a Token<'src>> {
    match tt {
        TriviaToken::Lexed(t) if trivia_kind(t).is_none() => Some(t),
        _ => None,
    }
}

/// The trivia [`SyntaxKind`] to emit for a raw entry that is trivia — active
/// whitespace / comment, or a directive / inactive-code marker — or `None`
/// for a significant token.
pub(super) fn raw_trivia_kind(tt: &TriviaToken<'_>) -> Option<SyntaxKind> {
    match tt {
        TriviaToken::Lexed(t) => trivia_kind(t),
        marker => marker.trivia_syntax_kind(),
    }
}

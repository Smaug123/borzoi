//! `SyntaxKind` — the flat tag attached to every green node and token.
//!
//! Variant names mirror FCS's `SynExpr`/`SynPat`/`SynModuleDecl`/… cases
//! (per `docs/parser-plan.md` D5) so the projector that diffs against
//! `tools/fcs-dump ast` is trivial. Only the kinds Phase 1 needs are listed;
//! grow per language-feature phase.
//!
//! This module also owns the **interval table** ([`kind_interval`] /
//! [`kind_in_surface`]) — the single source of truth for which language version
//! a kind is legal at, shared by the parser's version gate and the per-version
//! typed facades (`docs/ast-versioning-plan.md` D5).

use crate::language_version::LanguageVersion;

/// `u16` repr is required by rowan: it stores kinds as `u16` internally and
/// round-trips them through `Language::kind_from_raw` / `kind_to_raw`.
///
/// SCREAMING_SNAKE_CASE here is the rust-analyzer convention — the kinds map
/// onto grammar terminals/non-terminals in BNF-flavoured docs (`INT32_LIT`,
/// `EXPR_DECL`), and matching that naming makes the diff against
/// `pars.fsy`/`SyntaxTree.fsi` easier to eyeball. Suppress the default lint.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
// The untyped substrate is the honest, additive, lossy layer: you are never
// meant to exhaustively match raw kinds — match the ones you know and ignore
// the rest. Non-exhaustiveness is *correct* here, unlike the closed, frozen
// `ast::vN` dispatch enums (`docs/ast-versioning-plan.md` D7).
#[non_exhaustive]
pub enum SyntaxKind {
    // ---- terminals (tokens) -------------------------------------------------
    /// Integer literal — currently catches the `Int32` produced by the lexer.
    /// Other widths (`Int64`, `UInt8`, …) get distinct kinds when the parser
    /// learns to consume them.
    INT32_LIT,

    /// Signed 8-bit integer literal — `SynConst.SByte`. Lexer suffix `y`,
    /// e.g. `127y`.
    SBYTE_LIT,

    /// Unsigned 8-bit integer literal — `SynConst.Byte`. Lexer suffix `uy`,
    /// e.g. `255uy`. (Byte-char form `'a'B` will also funnel here once
    /// chars land.)
    BYTE_LIT,

    /// Signed 16-bit integer literal — `SynConst.Int16`. Lexer suffix `s`,
    /// e.g. `32767s`.
    INT16_LIT,

    /// Unsigned 16-bit integer literal — `SynConst.UInt16`. Lexer suffix
    /// `us`, e.g. `65535us`.
    UINT16_LIT,

    /// Unsigned 32-bit integer literal — `SynConst.UInt32`. Lexer suffixes
    /// `u`, `ul`, e.g. `42u`, `0xFFul`.
    UINT32_LIT,

    /// Signed 64-bit integer literal — `SynConst.Int64`. Lexer suffix `L`,
    /// e.g. `9223372036854775807L`.
    INT64_LIT,

    /// Unsigned 64-bit integer literal — `SynConst.UInt64`. Lexer suffixes
    /// `uL`, `UL`, e.g. `18446744073709551615UL`.
    UINT64_LIT,

    /// Signed native-width integer literal — `SynConst.IntPtr`. Lexer
    /// suffix `n`, e.g. `1n`.
    INTPTR_LIT,

    /// Unsigned native-width integer literal — `SynConst.UIntPtr`. Lexer
    /// suffix `un`, e.g. `1un`.
    UINTPTR_LIT,

    /// 64-bit IEEE-754 float literal — `SynConst.Double`. Covers both the
    /// decimal/exponent forms (`Token::Float64`, e.g. `1.0`, `1e10`) and
    /// the hex-bit-pattern form (`Token::XIEEE64`, e.g. `0x4024000000000000LF`).
    /// FCS emits the same `SynConst.Double` for both — the lexer is the
    /// only place the two forms diverge.
    IEEE64_LIT,

    /// 32-bit IEEE-754 float literal — `SynConst.Single`. Covers
    /// `Token::Float32` (decimal/exponent or dotless with `f`/`F` suffix,
    /// e.g. `1.0f`, `42f`, `1.5e-3F`) and `Token::XIEEE32` (hex
    /// bit-pattern with `lf` suffix, e.g. `0x40490fdblf`).
    IEEE32_LIT,

    /// Character literal — `SynConst.Char`. Lexer's `Token::Char` text
    /// retains the surrounding `'…'`; the normaliser decodes escapes
    /// (`\n`, `\xFF`, `\uHHHH`, `\UHHHHHHHH`, `\NNN` trigraph). Byte-char
    /// form (trailing `B`) routes to [`SyntaxKind::BYTE_LIT`] instead.
    CHAR_LIT,

    /// Regular string literal — `SynConst.String(text, SynStringKind.Regular,
    /// _)`. Lexer's `Token::String` text retains the surrounding double
    /// quotes; the normaliser decodes the inner content (single-letter
    /// escapes, `\NNN` trigraph, `\xHH`/`\uHHHH`/`\UHHHHHHHH` Unicode, and
    /// `\`-newline-whitespace line continuation). The byte-string form
    /// `"..."B` routes to [`SyntaxKind::BYTE_STRING_LIT`] instead.
    STRING_LIT,

    /// Verbatim string literal — `SynConst.String(text,
    /// SynStringKind.Verbatim, _)`. Lexer's `Token::VerbatimString`, source
    /// form `@"..."`. The only in-string escape is `""` (a literal quote);
    /// all other characters including newlines pass through verbatim. The
    /// byte-string form `@"..."B` routes to
    /// [`SyntaxKind::VERBATIM_BYTE_STRING_LIT`] instead.
    VERBATIM_STRING_LIT,

    /// Triple-quoted string literal — `SynConst.String(text,
    /// SynStringKind.TripleQuote, _)`. Lexer's `Token::TripleString`,
    /// source form `"""..."""`. No escapes at all: everything between the
    /// outer triples is literal (including single or double `"` runs). The
    /// byte-string form `"""..."""B` routes to
    /// [`SyntaxKind::TRIPLE_BYTE_STRING_LIT`] instead.
    TRIPLE_STRING_LIT,

    /// Regular byte-string literal — `SynConst.Bytes(bytes,
    /// SynByteStringKind.Regular, _)`. Lexer's `Token::String` with a
    /// trailing `B`; the parser dispatches on the suffix. Same escape
    /// table as [`SyntaxKind::STRING_LIT`], but each decoded character
    /// must fit in a byte (FCS errors via `FS1156` otherwise; surfacing
    /// the diagnostic is future-phase work).
    BYTE_STRING_LIT,

    /// Verbatim byte-string literal — `SynConst.Bytes(bytes,
    /// SynByteStringKind.Verbatim, _)`. Lexer's `Token::VerbatimString`
    /// with a trailing `B`, source form `@"..."B`. Same `""` escape as
    /// [`SyntaxKind::VERBATIM_STRING_LIT`]; characters must fit in a byte.
    VERBATIM_BYTE_STRING_LIT,

    /// Triple-quoted byte-string literal — `SynConst.Bytes(bytes,
    /// SynByteStringKind.Regular, _)`. FCS has no `TripleQuote` variant
    /// in `SynByteStringKind`; the kind field is `Regular`, but the
    /// source form `"""..."""B` has the no-escapes content rules of
    /// [`SyntaxKind::TRIPLE_STRING_LIT`], so a dedicated parser/normaliser
    /// token keeps the decoder selection local.
    TRIPLE_BYTE_STRING_LIT,

    /// `__SOURCE_DIRECTORY__` / `__SOURCE_FILE__` / `__LINE__` —
    /// `SynConst.SourceIdentifier(spelling, expanded, range)` in FCS
    /// (`pars.fsy:3475-3477`, fed by the `sourceIdentifier` rule that
    /// matches the lexer's `KEYWORD_STRING`). Lexer's
    /// `Token::KeywordString` text retains the literal spelling; the
    /// expanded value (current source dir / source file path / 1-based
    /// line) is computed by the consumer, since byte spans alone don't
    /// carry the file path or 1-based line.
    SOURCE_IDENTIFIER_LIT,

    /// Token kind for an `InterpString(BeginEnd)`/`Begin`/`Part`/`End`
    /// fragment of an interpolated string. Always appears as a child of
    /// [`SyntaxKind::INTERP_STRING_EXPR`]; never as a free literal.
    INTERP_STRING_FRAGMENT,

    /// `SynExpr.InterpolatedString` — the whole interpolated string,
    /// covering both the bare form (one `INTERP_STRING_FRAGMENT` child
    /// carrying a `BeginEnd` token) and the fill-bearing form
    /// (`INTERP_STRING_FRAGMENT` for `Begin`, then for each fill an
    /// inner expression followed by an `INTERP_STRING_FRAGMENT` for
    /// `Part`/`End`). Multi-fill is out of scope for the initial
    /// implementation, so the parser only produces 1- or 3-child shapes.
    INTERP_STRING_EXPR,

    /// Arbitrary-precision / numeric-literal-suffix literal —
    /// `SynConst.UserNum(value, suffix)`. Lexer's `Token::BigNum`, source
    /// forms `123I`, `42N`, `1_000G`. FCS's `pars.fsy` rule splits the
    /// token at the last character: the `value` is the digit run with
    /// underscores removed, the `suffix` is the trailing alpha char.
    USER_NUM_LIT,

    /// `System.Decimal` literal — `SynConst.Decimal`. Lexer's
    /// `Token::Decimal`, source forms `1.0m`, `1m`, `1e10m`, etc. The
    /// trailing `m`/`M` is part of the token text; the normaliser strips
    /// it (and the `_` digit separators) and canonicalises against
    /// `decimal.ToString(InvariantCulture)` to match FCS's serialised
    /// shape (trailing-zero scale preserved).
    DECIMAL_LIT,

    /// `true` / `false` keyword — `SynConst.Bool` in FCS. One kind for both
    /// values; the token's text distinguishes them.
    BOOL_LIT,

    /// `(` punctuator. Used as part of the unit literal
    /// `()` = `SynConst.Unit` and the [`SyntaxKind::PAREN_EXPR`] wrapper for
    /// `( e )`.
    LPAREN_TOK,

    /// `)` punctuator. Symmetric to [`SyntaxKind::LPAREN_TOK`].
    RPAREN_TOK,

    /// Identifier token — covers both plain `foo` and backticked
    /// `` ``foo bar`` ``. FCS strips backticks before storing in
    /// `Ident.idText`; our tree keeps the source text losslessly and the
    /// normaliser strips them at projection time.
    IDENT_TOK,

    /// `.` punctuator between segments of a dotted path (`Foo.Bar.Baz`).
    DOT_TOK,

    /// `..` range operator (FCS's `DOT_DOT`). Separates the bounds of a
    /// range / slice expression (`1..10`, `arr.[2..]`); the lexer fuses an
    /// adjacent `int..` into one `IntDotDot` token, which the lex-filter
    /// splits back so this token reaches the parser standalone. The sole
    /// child token of an [`SyntaxKind::INDEX_RANGE_EXPR`]. (The lexer's `..^`
    /// `DOT_DOT_HAT` is lex-filter-split into this `..` plus a `^` `Op` prefix,
    /// so no dedicated green-tree token is needed for the from-end slice.)
    DOT_DOT_TOK,

    /// `,` punctuator separating tuple elements (`1, 2, 3`).
    COMMA_TOK,

    /// `let` keyword (FCS's `Token::Let`). LexFilter rewrites the raw `let` to
    /// [`crate::lexfilter::Virtual::Let`] in offside contexts (almost all of
    /// them); the parser consumes the virtual and emits `LET_TOK` as a
    /// zero-width token carrying the same span as the rewritten real token
    /// (per LexFilter's `insert_token` convention).
    LET_TOK,

    /// `=` punctuator. Currently only consumed by [`SyntaxKind::BINDING`] for
    /// the `let x = e` form; future infix-`=` uses can reuse this same kind.
    EQUALS_TOK,

    /// `rec` keyword (FCS's `Token::Rec`). Sits between `LET_TOK` and the
    /// first [`SyntaxKind::BINDING`] when present, encoding the `isRec` flag
    /// on `SynModuleDecl.Let`. LexFilter passes this through as a raw token
    /// — it is not rewritten to a virtual — so the parser sees it in the
    /// filtered stream at its source position. Optional: a `let` without
    /// `rec` simply has no `REC_TOK` child.
    REC_TOK,

    /// `and` keyword (FCS's `Token::And`). Used to join successive
    /// [`SyntaxKind::BINDING`]s inside a single [`SyntaxKind::LET_DECL`]
    /// (mutually-recursive group, regardless of whether `rec` is present —
    /// FCS accepts `let f = 1 and g = 2` but warns FS0588). LexFilter's
    /// `is_let_continuator` (LexFilter.fs:336) explicitly keeps `CtxtLetDecl`
    /// open across `Token::And` via the `+1` offside guard so the raw flows
    /// through here. Future uses (`type X = … and Y = …`, mutually-recursive
    /// members) can reuse the same kind.
    AND_TOK,

    /// `inline` keyword (FCS's `Token::Inline`). Optional per-binding
    /// modifier in `opt_inline` of FCS's `localBinding` grammar
    /// (pars.fsy:3055 et seq.); when present, sits between
    /// [`SyntaxKind::REC_TOK`]/[`SyntaxKind::AND_TOK`] and the
    /// [`SyntaxKind::MUTABLE_TOK`]/`headBindingPattern`. FCS requires
    /// `inline` to precede `mutable` (`let mutable inline x = …` is FS0010);
    /// LexFilter passes this through as a raw token. The `isInline` flag on
    /// `SynBinding` projects from the presence of this child token within
    /// the [`SyntaxKind::BINDING`].
    INLINE_TOK,

    /// `mutable` keyword (FCS's `Token::Mutable`). Optional per-binding
    /// modifier in `opt_mutable` of FCS's `localBinding` grammar; sits after
    /// any [`SyntaxKind::INLINE_TOK`] and before the `headBindingPattern`.
    /// LexFilter passes this through as a raw token. The `isMutable` flag on
    /// `SynBinding` projects from the presence of this child token within
    /// the [`SyntaxKind::BINDING`].
    MUTABLE_TOK,

    /// `as` keyword (FCS's `Token::AS`). Separates the two operands of a
    /// [`SyntaxKind::AS_PAT`] (`<pat> as <pat>`). FCS's grammar reaches
    /// this through `headBindingPattern AS constrPattern` (`pars.fsy:3570`)
    /// and `parenPattern AS constrPattern` (`pars.fsy:3902`); LexFilter
    /// passes it through as a raw token.
    AS_TOK,

    /// `open` keyword (FCS's `Token::Open`). Sole opener of a
    /// [`SyntaxKind::OPEN_DECL`]. LexFilter does not push a context for
    /// `open` (it is an ordinary statement-leading token), so the parser sees
    /// it directly in the filtered stream and emits it as `OPEN_TOK`.
    OPEN_TOK,

    /// `type` keyword (FCS's `Token::Type`) in the `open type T` form. The
    /// raw `type` is *swallowed* by LexFilter — it pushes a transient
    /// `CtxtTypeDefns` and never surfaces in the filtered stream (mirroring
    /// `module`) — so [`OPEN_DECL`](SyntaxKind::OPEN_DECL) recovers it from
    /// the raw stream and emits it as `TYPE_TOK`, marking the open target as
    /// `SynOpenDeclTarget.Type` rather than `ModuleOrNamespace`.
    TYPE_TOK,

    /// `of` keyword (FCS's `Token::Of`) introducing a discriminated-union case's
    /// field list (`A of int * string`, phase 9.5). A direct child token of a
    /// [`SyntaxKind::UNION_CASE`].
    OF_TOK,

    /// `delegate` keyword (FCS's `Token::DELEGATE`) heading a delegate type
    /// definition body (`type T = delegate of int -> int`). A direct child
    /// token of a [`SyntaxKind::DELEGATE_REPR`].
    DELEGATE_TOK,

    /// `member` keyword (FCS's `Token::Member`) introducing an object-model
    /// member method (`member this.M = …`, phase 9.7). Unlike the swallowed
    /// `type`/`module` keywords, `member` flows through LexFilter as a real
    /// filtered token, so it is claimed directly. Its presence is FCS's
    /// `SynBinding.Trivia.LeadingKeyword.Member`. A direct child token of a
    /// [`SyntaxKind::MEMBER_DEFN`].
    MEMBER_TOK,

    /// `static` keyword (FCS's `Token::Static`) preceding a `member` in an
    /// object-model body (`static member M = …`, phase 9.9a). A real
    /// filtered token, claimed before [`SyntaxKind::MEMBER_TOK`]; its presence
    /// is FCS's `SynBinding.Trivia.LeadingKeyword.StaticMember` (and the member's
    /// `SynMemberFlags.IsInstance = false`). A direct child token of a
    /// [`SyntaxKind::MEMBER_DEFN`].
    STATIC_TOK,

    /// `module` keyword (FCS's `Token::Module`) introducing a file-level
    /// named module (`module Foo`). The raw `module` is *swallowed* by
    /// LexFilter — it pushes a transient `CtxtModuleHead` and never surfaces
    /// in the filtered stream (mirroring `type`) — so the phase-8.2 header
    /// parser recovers it from the raw stream and emits it as `MODULE_TOK`,
    /// the same way `parse_let_head_and_bindings` claims the raw `let`. Its presence as a
    /// direct child of [`SyntaxKind::MODULE_OR_NAMESPACE`] marks the module's
    /// `SynModuleOrNamespaceKind` as `NamedModule`. (The nested `module Foo =
    /// …` decl form — `SynModuleDecl.NestedModule` — arrives in phase 8.4.)
    MODULE_TOK,

    /// `namespace` keyword (FCS's `Token::Namespace`) introducing a file-level
    /// namespace (`namespace Foo.Bar`). Unlike `module`, LexFilter passes
    /// `namespace` through unchanged (it opens a `CtxtNamespaceHead` but emits
    /// the token), so the phase-8.2 header parser sees it in the filtered
    /// stream and emits it directly. Its presence as a direct child of
    /// [`SyntaxKind::MODULE_OR_NAMESPACE`] marks the kind as
    /// `DeclaredNamespace` (or `GlobalNamespace` when a [`SyntaxKind::GLOBAL_TOK`]
    /// follows).
    NAMESPACE_TOK,

    /// `global` keyword (FCS's `Token::Global`) as the sole target of a
    /// `namespace global` header. FCS's post-parse pass strips a leading
    /// `global` segment and, when it was the *only* segment, sets the kind to
    /// `GlobalNamespace` with an empty `longId`. We emit the bare `global`
    /// as `GLOBAL_TOK` (rather than folding it into the
    /// [`SyntaxKind::LONG_IDENT`] path) so the header projects to
    /// `GlobalNamespace` with no path. (A dotted `global.Foo` head still flows
    /// through the ordinary path parser — see the known limitation in the
    /// phase-8 sub-plan.)
    GLOBAL_TOK,

    /// Access modifier (`internal` / `private` / `public`, FCS's
    /// `Token::Internal` / `Token::Private` / `Token::Public`) in a module
    /// header (`module internal Foo`). FCS's `moduleIntro` accepts
    /// `opt_access` between the `module` keyword and the path
    /// (`pars.fsy:536`); accessibility is field 6 of `SynModuleOrNamespace`
    /// and is **elided** by the differential normaliser, so this token exists
    /// purely to keep the modifier out of `ERROR` and preserve the
    /// `text(tree) == source` invariant. (Namespaces take no access modifier.)
    ACCESS_TOK,

    /// `_` underscore (FCS's `Token::Underscore`). Sole child of a
    /// [`SyntaxKind::WILDCARD_PAT`] or [`SyntaxKind::ANON_TYPE`]; the
    /// underlying construct's range is recoverable from this token's span.
    UNDERSCORE_TOK,

    /// `:` punctuator (FCS's `Token::Colon`). Currently emitted by
    /// [`SyntaxKind::TYPED_EXPR`] between the wrapped expression and the
    /// type annotation. Future uses (member-binding return types, signature
    /// parameters, typed patterns) can reuse this same kind.
    COLON_TOK,

    /// `?` punctuator (FCS's `Token::QMark`) — the optional-argument sigil
    /// introducing a [`SyntaxKind::OPTIONAL_VAL_PAT`] (`?ident`). Sole
    /// keyword-like child before the named ident; the pattern's range starts
    /// here.
    QMARK_TOK,

    /// `:?` punctuator (FCS's `Token::ColonQMark`) — the dynamic type-test
    /// operator introducing a [`SyntaxKind::IS_INST_PAT`]. Sole keyword-like
    /// child before the tested type; the pattern's range starts here.
    COLON_QMARK_TOK,

    /// `::` punctuator (FCS's `Token::ColonColon`) — the cons operator between
    /// the operands of a [`SyntaxKind::LIST_CONS_PAT`].
    COLON_COLON_TOK,

    /// `:>` punctuator (FCS's `Token::ColonGreater`) — the subtype operator in a
    /// `WhereTyparSubtypeOfType` type-parameter constraint (`'a :> T`, phase
    /// 9.3b), between the constraint's typar and its type, and the upcast
    /// expression operator `e :> T` introducing a [`SyntaxKind::UPCAST_EXPR`].
    COLON_GREATER_TOK,

    /// `:?>` punctuator (FCS's `Token::ColonQMarkGreater`) — the dynamic
    /// downcast expression operator `e :?> T` introducing a
    /// [`SyntaxKind::DOWNCAST_EXPR`], between the cast expression and the target
    /// type.
    COLON_QMARK_GREATER_TOK,

    /// `'` punctuator (FCS's `Token::Quote`) introducing a "normal" type
    /// variable, e.g. `'a` in `(x : 'a)`. Produces a `SynTypar` with
    /// `TyparStaticReq.None` (`pars.fsy` `typar` rule, line 6760). Sole
    /// non-trivia child of a [`SyntaxKind::VAR_TYPE`] before the ident.
    QUOTE_TOK,

    /// `^` sigil introducing a head-typar (statically-resolved type
    /// parameter), e.g. `^T` in `(x : ^T)`. FCS recognises this as the
    /// `INFIX_AT_HAT_OP` token whose text equals `"^"` (`pars.fsy`
    /// `typar` rule, line 6764); our lexer emits it via the general
    /// `Token::Op("^")` operator regex, and the parser checks the text
    /// before bumping. Distinguishes `TyparStaticReq.HeadType` from the
    /// plain `'`-quoted form.
    HAT_TOK,

    /// `->` punctuator (FCS's `Token::RArrow`). Emitted by
    /// [`SyntaxKind::FUN_TYPE`] between the argument and return types.
    /// The token's span recovers FCS's `SynTypeFunTrivia.ArrowRange`.
    /// Future uses (`fun x -> e`, match-clause arrows) reuse the same
    /// kind.
    RARROW_TOK,

    /// `<-` punctuator (FCS's `Token::LARROW`). Emitted between the target
    /// and value of an [`SyntaxKind::ASSIGN_EXPR`] (`x <- e`). FCS has no
    /// `LARROW` AST node — the token only survives as the operator position
    /// of `mkSynAssign`'s output and is otherwise range data — so this kind
    /// exists purely to keep the green tree lossless.
    LARROW_TOK,

    /// `*` separator (FCS's `STAR` token, which fsyacc receives via the
    /// `INFIX_STAR_DIV_MOD_OP` family). Our lexer emits the general
    /// `Token::Op("*")`; the parser stamps this kind only when the op's
    /// text equals `"*"` (so `**`, `*+`, etc. stay as ERROR or future
    /// op kinds). Used as the segment separator inside a
    /// [`SyntaxKind::TUPLE_TYPE`] — one `STAR_TOK` between each
    /// neighbouring type child, matching FCS's
    /// `SynTupleTypeSegment.Star` range.
    STAR_TOK,

    /// `if` keyword (FCS's `Token::If`). LexFilter passes this through as a
    /// raw token (`LexFilter.fs:2506`) — `CtxtIf` is pushed but no virtual
    /// rewrite happens. The parser consumes it via the normal `bump_into`
    /// path inside [`SyntaxKind::IF_THEN_ELSE_EXPR`].
    IF_TOK,

    /// `then` keyword (FCS's `Token::Then`). LexFilter rewrites this to
    /// [`crate::lexfilter::Virtual::Then`] at the offside layer
    /// (`LexFilter.fs:2477`), pushing `CtxtThen` + a SeqBlock that emits
    /// `Virtual::BlockBegin` anchored at the body. The parser handles the
    /// virtual like [`SyntaxKind::LET_TOK`]: the raw `Token::Then` still
    /// sits at `raw_pos` with the same span as the virtual, so emit it
    /// directly as `THEN_TOK` and advance both cursors.
    THEN_TOK,

    /// `else` keyword (FCS's `Token::Else`). LexFilter rewrites this to
    /// [`crate::lexfilter::Virtual::Else`] (`LexFilter.fs:2483`) when not
    /// merged with a following `if` into a single `ELIF` token; the
    /// dispatch pushes `CtxtElse` + a SeqBlock with `Virtual::BlockBegin`
    /// for the body. Same emission pattern as [`SyntaxKind::THEN_TOK`].
    ELSE_TOK,

    /// `elif` keyword, or `else if` after LexFilter's same-line merge
    /// (FCS's `Token::Elif`, see `LexFilter.fs:2483`). LexFilter passes
    /// the token through as raw and pushes `CtxtIf`, exactly like
    /// `Token::If`. The parser emits this kind for the elif arm of an
    /// `if`/`elif` chain — a single token whose source span covers either
    /// the four-character `elif` keyword or the merged `else…if` run
    /// (including any whitespace between them). Lossless either way:
    /// the surrounding `IF_THEN_ELSE_EXPR` node carries the structural
    /// nesting that distinguishes the bare elif (`isElif = true` in
    /// FCS's trivia) from the desugared `else if` form.
    ELIF_TOK,

    /// `fun` keyword (FCS's `Token::Fun`). LexFilter rewrites this to
    /// [`crate::lexfilter::Virtual::Fun`] (`LexFilter.fs:2360`), pushing
    /// `CtxtFun` so the body's offside scope is anchored to the column
    /// of `fun`. The raw `Token::Fun` still sits at `raw_pos` with the
    /// same span as the virtual, so the parser emits this directly as
    /// `FUN_TOK` and advances both cursors — same pattern as
    /// [`SyntaxKind::LET_TOK`] / [`SyntaxKind::THEN_TOK`]. Sole opener
    /// of [`SyntaxKind::FUN_EXPR`].
    FUN_TOK,

    /// `match` keyword (FCS's `Token::Match`, `pars.fsy:4221`). Unlike
    /// `fun`/`let`, LexFilter does *not* rewrite it to a virtual — it
    /// passes through as raw and pushes `CtxtMatch`. The parser bumps it
    /// directly. Sole opener of [`SyntaxKind::MATCH_EXPR`].
    MATCH_TOK,

    /// `match!` keyword (FCS's `Token::MatchBang`, `pars.fsy:4233`). Like
    /// [`SyntaxKind::MATCH_TOK`], LexFilter does *not* rewrite it to a
    /// virtual — it passes through as raw and pushes
    /// `CtxtMatch`/`CtxtMatchClauses`, so the parser bumps it directly. Sole
    /// opener of [`SyntaxKind::MATCH_BANG_EXPR`].
    MATCH_BANG_TOK,

    /// `with` keyword at the head of a `match … with …`. LexFilter
    /// rewrites the raw `Token::With` to [`crate::lexfilter::Virtual::With`]
    /// (`OWITH`, `LexFilter.fs`), pushing `CtxtMatchClauses`; the raw still
    /// sits at `raw_pos` with the same span, so the parser emits this
    /// directly and advances both cursors — same pattern as
    /// [`SyntaxKind::FUN_TOK`]. Separates the scrutinee from the clause
    /// list inside [`SyntaxKind::MATCH_EXPR`].
    WITH_TOK,

    /// `when` keyword introducing a [`SyntaxKind::MATCH_CLAUSE`] guard
    /// (FCS's `Token::When`, `pars.fsy` `patternAndGuard`). Unlike
    /// `with`, LexFilter leaves it as a raw `Token::When` (it pushes a
    /// `CtxtWhen` context but does not relabel the token), so the parser
    /// bumps it directly. Followed by the guard expression, which the
    /// parser reads with `parse_expr` up to the clause's `->`.
    WHEN_TOK,

    /// `or` keyword (`Token::Or`) separating the typars of a parenthesised
    /// typar-alternatives SRTP member constraint subject —
    /// `(^a or ^b) : (static member …)` (FCS's `typeAlts`, `pars.fsy:2705`).
    OR_TOK,

    /// `function` keyword opening a [`SyntaxKind::MATCH_LAMBDA_EXPR`]
    /// (FCS's `SynExpr.MatchLambda`). LexFilter rewrites the raw
    /// `Token::Function` to [`crate::lexfilter::Virtual::Function`]
    /// (`OFUNCTION`), pushing `CtxtFunction` + `CtxtMatchClauses`; the raw
    /// still sits at `raw_pos` with the same span, so the parser emits this
    /// directly and advances both cursors — same pattern as
    /// [`SyntaxKind::FUN_TOK`] / [`SyntaxKind::WITH_TOK`]. Sole opener of
    /// [`SyntaxKind::MATCH_LAMBDA_EXPR`].
    FUNCTION_TOK,

    /// `null` keyword (FCS's `Token::Null`). Phase 6.1 emits this inside
    /// a [`SyntaxKind::NULL_PAT`] (`let null = …` or `let f null = …`).
    /// FCS uses `null` both as a pattern (`SynPat.Null`) and as an
    /// expression (`SynExpr.Null`); the expression surface reuses this
    /// same kind as the sole child of a [`SyntaxKind::NULL_EXPR`]. Phase
    /// 10.9 also reuses it as the sole child of a
    /// [`SyntaxKind::STATIC_CONST_NULL_TYPE`] (`(x : Foo<null>)`).
    NULL_TOK,

    /// `const` keyword (FCS's `Token::Const`). Phase 10.9 emits this as the
    /// leading child of a [`SyntaxKind::STATIC_CONST_EXPR_TYPE`] —
    /// `const E` in `(x : Foo<const E>)`, FCS's `CONST atomicExpr`
    /// (`pars.fsy:6583`).
    CONST_TOK,

    /// `<` opener of a prefix type application (`Foo<int>`). FCS's
    /// `LESS` token carries a bool payload set by LexFilter's
    /// `peek_adjacent_typars` to distinguish the type-argument bracket
    /// from the binary `<` comparison; we record only the kind here and
    /// recover the typar-bracket disambiguation from the surrounding
    /// [`SyntaxKind::APP_TYPE`] (or future type-parameter-list) context.
    /// Sole opener inside the prefix form of [`SyntaxKind::APP_TYPE`],
    /// matching FCS's `SynType.App(_, Some lessRange, _, _, _, false, _)`.
    LESS_TOK,

    /// `>` closer of a prefix type application (`Foo<int>`). Symmetric to
    /// [`SyntaxKind::LESS_TOK`]. LexFilter's `smash_typar_token` splits
    /// trailing `>>` / `>=` runs into separate tokens for nested generics
    /// like `List<List<int>>`, so the parser always sees a bare `>` here.
    /// Mirrors FCS's `greaterRange` field on `SynType.App`.
    GREATER_TOK,

    /// `[` opener. FCS's context-neutral `LBRACK` token (lex.fsl); this
    /// kind maps it wherever it appears. Two consumers so far:
    /// - the type grammar's `arrayTypeSuffix` production (`pars.fsy:6397-…`)
    ///   — the [`SyntaxKind::ARRAY_TYPE`] suffix. When emitted in the
    ///   IDENT-adjacent `name[]` form, LexFilter inserts a
    ///   [`crate::lexfilter::Virtual::HighPrecedenceBrackApp`] virtual
    ///   between the IDENT and the `[`; the parser absorbs that virtual as a
    ///   zero-width ERROR before bumping `LBRACK_TOK`, mirroring FCS's
    ///   `appTypeWithoutNull HIGH_PRECEDENCE_BRACK_APP arrayTypeSuffix` arm.
    /// - the list-pattern opener of an [`SyntaxKind::ARRAY_OR_LIST_PAT`]
    ///   (`pars.fsy:3786`). The list-*expression* `[ e; e ]` opener will
    ///   reuse this kind in a later phase.
    LBRACK_TOK,

    /// `]` closer. FCS's context-neutral `RBRACK` token (lex.fsl) —
    /// symmetric to [`SyntaxKind::LBRACK_TOK`]. Closes both the
    /// `arrayTypeSuffix` ([`SyntaxKind::ARRAY_TYPE`]) and the list form of
    /// [`SyntaxKind::ARRAY_OR_LIST_PAT`].
    RBRACK_TOK,

    /// `[|` opener of the array form of an [`SyntaxKind::ARRAY_OR_LIST_PAT`].
    /// FCS's `LBRACK_BAR` token (lex.fsl) — `atomicPattern: LBRACK_BAR
    /// listPatternElements BAR_RBRACK` (`pars.fsy:3789`). The array-literal
    /// *expression* opener will reuse this kind in a later phase.
    LBRACK_BAR_TOK,

    /// `|]` closer of the array form of an [`SyntaxKind::ARRAY_OR_LIST_PAT`].
    /// FCS's `BAR_RBRACK` token (lex.fsl) — symmetric to
    /// [`SyntaxKind::LBRACK_BAR_TOK`] in the same production.
    BAR_RBRACK_TOK,

    /// `#` opener of a [`SyntaxKind::HASH_CONSTRAINT_TYPE`]. FCS's `HASH`
    /// token (lex.fsl) — the type grammar's `hashConstraint` production
    /// (`pars.fsy:2609-2611`) is the sole consumer of this kind. Distinct
    /// from line-leading preprocessor `#if`/`#load`/… directives, which
    /// are folded out by [`crate::directives`] before the filtered stream
    /// reaches the parser; only inline `#` in a type position (e.g. the
    /// flexible-type constraint `(x : #int)`) arrives here.
    HASH_TOK,

    /// `{|` opener of an [`SyntaxKind::ANON_RECD_TYPE`] (and the parallel
    /// anon-record *expression* form, when that lands). FCS's
    /// `LBRACE_BAR` token (`lex.fsl`) — the type grammar consumes it in
    /// `braceBarFieldDeclListCore: LBRACE_BAR recdFieldDeclList bar_rbrace`
    /// (`pars.fsy:2516-2522`).
    LBRACE_BAR_TOK,

    /// `|}` closer of an [`SyntaxKind::ANON_RECD_TYPE`]. FCS's
    /// `BAR_RBRACE` token (`lex.fsl`) — symmetric to
    /// [`SyntaxKind::LBRACE_BAR_TOK`] inside the same production.
    BAR_RBRACE_TOK,

    /// A standalone `|` bar. FCS's `BAR` token (`lex.fsl`); LexFilter
    /// relabels it to `BAR_JUST_BEFORE_NULL` when the nullness feature
    /// is on and the next token is `NULL` (`LexFilter.fs:2613`). Phase
    /// 7.11 emits it inside a [`SyntaxKind::WITH_NULL_TYPE`] (the `|` of
    /// `string | null`); match-case / DU surfaces will reuse this same
    /// token kind when they land.
    BAR_TOK,

    /// `struct` keyword. FCS's `STRUCT` token (`lex.fsl`). Phase 7.9
    /// introduces it as the struct-anon-recd-type marker
    /// (`anonRecdType: STRUCT braceBarFieldDeclListCore`,
    /// `pars.fsy:2510-2513`). Later phases will reuse it for
    /// `struct (T * U)` tuple types (`atomType` STRUCT-LPAREN form,
    /// `pars.fsy:6549-…`) and for struct type definitions.
    STRUCT_TOK,

    /// `;` field separator. FCS's `SEMICOLON` token (`lex.fsl`) —
    /// `seps: SEMICOLON | …` (`pars.fsy:6981-6985`). Phase 7.9
    /// recognises it only between anon-recd-type fields; later phases
    /// add list/array/record-expression consumers, sequential
    /// expressions, etc.
    SEMI_TOK,

    /// `;;` top-level declaration separator. FCS's `SEMICOLON_SEMICOLON`
    /// token (`lex.fsl`) — `topSeparator: SEMICOLON_SEMICOLON`
    /// (`pars.fsy:6967-6969`). Accepted between or after module
    /// declarations; it produces no `SynModuleDecl` of its own, so the
    /// decl loop emits it as an inert separator token (a *leading* `;;`,
    /// before any decl, is instead an error — FCS rejects it).
    SEMISEMI_TOK,

    /// `yield` / `return` keyword token of a
    /// [`SyntaxKind::YIELD_OR_RETURN_EXPR`]. FCS lexes both to the same
    /// `YIELD` token carrying a bool (`yield` ⇒ `true`, `return` ⇒
    /// `false`); we keep the distinct source keywords as one kind and
    /// recover `isYield` from the token text (`yield` ⇒ true) in
    /// [`crate::syntax::YieldExpr::is_yield`].
    YIELD_TOK,

    /// `yield!` / `return!` keyword token of a
    /// [`SyntaxKind::YIELD_OR_RETURN_FROM_EXPR`]. Symmetric to
    /// [`SyntaxKind::YIELD_TOK`] for the `!` (from) forms.
    YIELD_BANG_TOK,

    /// `do!` keyword token of a [`SyntaxKind::DO_BANG_EXPR`]. LexFilter
    /// rewrites the raw `Token::DoBang` to [`crate::lexfilter::Virtual::DoBang`]
    /// (the raw still sits at `raw_pos` with the same span), so the parser
    /// emits this directly from the virtual, like
    /// [`SyntaxKind::THEN_TOK`]/[`SyntaxKind::LET_TOK`].
    DO_BANG_TOK,

    /// `while` keyword opening a [`SyntaxKind::WHILE_EXPR`] (FCS's
    /// `Token::While`, `pars.fsy:4367`). Like `match`, LexFilter does *not*
    /// rewrite it to a virtual — it passes through as raw (pushing `CtxtWhile`),
    /// so the parser bumps it directly.
    WHILE_TOK,

    /// `while!` keyword opening a [`SyntaxKind::WHILE_BANG_EXPR`] (FCS's
    /// `Token::WhileBang`). Like [`SyntaxKind::WHILE_TOK`], a plain raw token
    /// (not rewritten to a virtual), bumped directly.
    WHILE_BANG_TOK,

    /// `for` keyword opening a [`SyntaxKind::FOR_EACH_EXPR`] or
    /// [`SyntaxKind::FOR_EXPR`] (FCS's `Token::For`, `pars.fsy:4372`). Like
    /// `while`, LexFilter does *not* rewrite it to a virtual — it passes through
    /// as raw (pushing `CtxtFor`), so the parser bumps it directly.
    FOR_TOK,

    /// `to` keyword of an ascending `for i = a to b do …` range loop
    /// ([`SyntaxKind::FOR_EXPR`]; FCS's `Token::To`, `pars.fsy:5636`). A plain
    /// raw token, bumped directly; its presence (vs [`SyntaxKind::DOWNTO_TOK`])
    /// is FCS's `SynExpr.For.direction = true`.
    TO_TOK,

    /// `downto` keyword of a descending `for i = a downto b do …` range loop
    /// ([`SyntaxKind::FOR_EXPR`]; FCS's `Token::DownTo`, `pars.fsy:5638`). A
    /// plain raw token, bumped directly; its presence is FCS's
    /// `SynExpr.For.direction = false`.
    DOWNTO_TOK,

    /// `do` keyword separating the condition from the body in a
    /// [`SyntaxKind::WHILE_EXPR`]. LexFilter rewrites the raw `Token::Do` to
    /// [`crate::lexfilter::Virtual::Do`] (`ODO`) at the same span (the raw still
    /// sits at `raw_pos`), so the parser emits this directly from the virtual,
    /// like [`SyntaxKind::WITH_TOK`]/[`SyntaxKind::THEN_TOK`].
    DO_TOK,

    /// `done` — the explicit verbose-syntax terminator of a `do` block
    /// (`while … do … done`, `do! … done`; FCS's `Token::Done`). LexFilter
    /// relabels the raw `Token::Done` to the block-closing
    /// [`crate::lexfilter::Virtual::DeclEnd`] at the `done` span while keeping
    /// the raw token at `raw_pos`, so the parser claims it directly (the
    /// `WITH_TOK`/`DO_TOK` pattern). The `done` keyword is not stored in the
    /// FCS AST (`SynExpr.While`/`DoBang` carry no `done` trivia), so the
    /// normaliser elides it; claiming it just keeps `text(tree) == source` and
    /// avoids a leftover raw token. A *synthetic* offside block close (no
    /// `done`) is consumed as a zero-width `ERROR` instead, not this token.
    DONE_TOK,

    /// `try` keyword opening a [`SyntaxKind::TRY_EXPR`] (FCS's `Token::Try`,
    /// `pars.fsy:4245`). Like `match`/`while`/`for`, LexFilter does *not*
    /// rewrite it to a virtual — it passes through as raw and pushes `CtxtTry`
    /// (+ a one-sided SeqBlock for the body), so the parser bumps it directly.
    /// Sole opener of [`SyntaxKind::TRY_EXPR`].
    TRY_TOK,

    /// `finally` keyword separating the body from the cleanup expression in a
    /// `try … finally …` ([`SyntaxKind::TRY_EXPR`]; FCS's `Token::Finally`,
    /// `pars.fsy:4313`). A plain raw passthrough (LexFilter force-closes the
    /// try body's SeqBlock at the `finally` but does not relabel the token);
    /// the parser bumps it directly. Its presence (vs a
    /// [`SyntaxKind::WITH_TOK`] plus a clause list) is what discriminates
    /// `TryFinally` from `TryWith` (phase 10.20b).
    FINALLY_TOK,

    /// `let!` / `use!` keyword token heading a [`SyntaxKind::LET_OR_USE_EXPR`].
    /// LexFilter rewrites the raw `Token::LetBang`/`Token::UseBang` to
    /// [`crate::lexfilter::Virtual::Binder`] (the raw still sits at `raw_pos`
    /// with the same span), so the parser emits this directly from the virtual,
    /// like [`SyntaxKind::DO_BANG_TOK`]/[`SyntaxKind::LET_TOK`]. The `let!`-vs-
    /// `use!` distinction is recovered from this token's text.
    BINDER_TOK,

    /// `and!` keyword token introducing an additional applicative binding in a
    /// [`SyntaxKind::LET_OR_USE_EXPR`]. LexFilter rewrites the raw
    /// `Token::AndBang` to [`crate::lexfilter::Virtual::AndBang`] (raw still at
    /// `raw_pos`, same span); emitted directly from the virtual.
    AND_BANG_TOK,

    /// `in` keyword of an explicit-`in` binder (`let! p = e in body`). LexFilter
    /// consumes the raw `Token::In` in its IN arm (emitting the binder's
    /// `Virtual::DeclEnd` at the `in`'s span) and does *not* surface it in the
    /// filtered stream, so the parser claims the raw `in` directly from the raw
    /// stream — mirroring FCS's `SynLetOrUse.Trivia.InKeyword`. Elided by the
    /// normaliser.
    ///
    /// Also the `in` of a `for pat in enumExpr do …` binder
    /// ([`SyntaxKind::FOR_EACH_EXPR`]). There LexFilter's `in` arm is gated on
    /// `Context::LetDecl`, *not* `Context::For`, so the raw `Token::In` is left
    /// in the filtered stream and the parser bumps it directly (a plain raw
    /// passthrough, like [`SyntaxKind::WHILE_TOK`]).
    IN_TOK,

    /// `{` opener of a [`SyntaxKind::COMPUTATION_EXPR`] (FCS's `LBRACE`,
    /// `lex.fsl`). Distinct from the anon-record-type `{|`
    /// ([`SyntaxKind::LBRACE_BAR_TOK`]). The matching `}` is swallowed by
    /// LexFilter (it never reaches the filtered stream, like the `)` of a
    /// paren expression), so the parser recovers it from the raw stream via
    /// `bump_swallowed_closer`. Reused by [`SyntaxKind::RECORD_PAT`]
    /// (`atomicPattern: LBRACE recordPatternElementsAux rbrace`,
    /// `pars.fsy:3780`); record / object expressions will reuse it too.
    LBRACE_TOK,

    /// `}` closer of a [`SyntaxKind::COMPUTATION_EXPR`] or
    /// [`SyntaxKind::RECORD_PAT`] (FCS's `RBRACE`). Emitted from the raw
    /// stream because LexFilter swallows it from the filtered stream —
    /// symmetric to [`SyntaxKind::RPAREN_TOK`].
    RBRACE_TOK,

    /// Code-quotation opener — `<@` (FCS's `Token::LQuote`, typed) or
    /// `<@@` (`Token::LQuoteRaw`, raw/untyped). One kind for both forms;
    /// the `isRaw` flag on FCS's `SynExpr.Quote` is recovered from the
    /// token's source text (`<@@` ⇒ raw) by
    /// [`crate::syntax::QuoteExpr::is_raw`], the same text-based recovery
    /// the `BOOL_LIT` and infix-operator kinds use. Sole opener of a
    /// [`SyntaxKind::QUOTE_EXPR`].
    LQUOTE_TOK,

    /// Code-quotation closer — `@>` (FCS's `Token::RQuote`) or `@@>`
    /// (`Token::RQuoteRaw`). Symmetric to [`SyntaxKind::LQUOTE_TOK`].
    /// LexFilter splits the compound `@>.` / `@>|}` closers into a plain
    /// `RQuote` + `Dot` / `BarRBrace` before the parser sees them, so the
    /// parser always bumps a bare closer here. FCS reports
    /// `parsMismatchedQuote` when the closer's raw-ness differs from the
    /// opener's but still builds the `Quote` with the *opener*'s `isRaw`;
    /// we mirror that (push a parse error, keep the opener-derived flag).
    RQUOTE_TOK,

    /// Run of spaces and tabs between tokens.
    WHITESPACE,

    /// `\n`, `\r\n`, or `\r` line terminator.
    NEWLINE,

    /// `// …` to end of line. XML-doc `/// …` is folded in for now; split
    /// when parser-level doc-comment handling lands.
    LINE_COMMENT,

    /// `(* … *)` (nestable).
    BLOCK_COMMENT,

    /// `#line N` / `# N "file"` source-location directive — FCS's
    /// `HASH_LINE` token (`lex.fsl:757-811`, declared `pars.fsy:154`). A
    /// trivia kind: FCS emits it only under `skip=false` (full-trivia /
    /// editor mode) and no grammar rule consumes it. The preprocessor
    /// driver recognises the directive in [`crate::directives`]; its
    /// full-trivia mode will emit one payload-free token covering the whole
    /// directive line (the line number and filename are recovered
    /// separately via the directive layer's line-directive store, mirroring
    /// FCS, which keeps them on the lexbuf rather than the token). Distinct
    /// from the inline `#` flexible-type constraint token
    /// [`SyntaxKind::HASH_TOK`]. See
    /// `docs/completed/hashline-warndirective-trivia-plan.md`.
    HASH_LINE,

    /// `#nowarn …` / `#warnon …` warning-scope directive — FCS's
    /// `WARN_DIRECTIVE` token (`lex.fsl:1084-1089`, declared `pars.fsy:155`).
    /// A trivia kind on the same footing as [`SyntaxKind::HASH_LINE`]: FCS
    /// emits it only under `skip=false` and no grammar rule consumes it. The
    /// preprocessor driver recognises the directive in [`crate::directives`];
    /// its full-trivia mode will emit one token over the whole directive
    /// line. The parsed warning numbers live on the recognised
    /// [`crate::directives::Directive`], not on this token. See
    /// `docs/completed/hashline-warndirective-trivia-plan.md`.
    WARN_DIRECTIVE,

    /// `#if` conditional-compilation directive line — FCS's `HASH_IF` token
    /// (`lex.fsl:1010-1020`, declared `pars.fsy:155`). A trivia kind: FCS
    /// emits it only under `skip=false` and no grammar rule consumes it (the
    /// directive drives the preprocessor, never the AST). The full-trivia
    /// driver mode will emit one token over the directive line. See
    /// `docs/completed/parser-ifdef-plan.md`.
    HASH_IF,

    /// `#else` conditional-compilation directive line — FCS's `HASH_ELSE`
    /// token (`lex.fsl:1022-1033`, declared `pars.fsy:155`). Trivia, on the
    /// same footing as [`SyntaxKind::HASH_IF`].
    HASH_ELSE,

    /// `#elif` conditional-compilation directive line — FCS's `HASH_ELIF`
    /// token (`lex.fsl:1035-1051`, declared `pars.fsy:155`). Trivia, on the
    /// same footing as [`SyntaxKind::HASH_IF`].
    HASH_ELIF,

    /// `#endif` conditional-compilation directive line — FCS's `HASH_ENDIF`
    /// token (`lex.fsl:1053-1063`, declared `pars.fsy:155`). Trivia, on the
    /// same footing as [`SyntaxKind::HASH_IF`].
    HASH_ENDIF,

    /// A dead (`#if`-eliminated) source region — FCS's `INACTIVECODE` token
    /// (the `ifdefSkip` lexer state, `lex.fsl:1101-1223`, declared
    /// `pars.fsy:154`). Trivia: one token spans the whole skipped region
    /// (including any nested `#if`…`#endif` inside it, which are *not*
    /// re-lexed), so malformed bytes in a dead branch never reach the lexer.
    /// The full-trivia driver mode will emit it; the parser keeps it in the
    /// tree for `text(tree) == source` but elides it from the projected AST.
    /// See `docs/completed/parser-ifdef-plan.md`.
    INACTIVECODE,

    /// Unrecognised input — emitted by the parser when the lexer returns
    /// `Err(LexError)` or when a token is encountered out of context. Carries
    /// no value other than its span.
    ERROR,

    // ---- composites (nodes) -------------------------------------------------
    /// Root for an implementation file — `ParsedImplFileInput` in FCS.
    /// Holds zero or more [`SyntaxKind::MODULE_OR_NAMESPACE`] children plus trivia.
    IMPL_FILE,

    /// Root for a signature file — `ParsedSigFileInput`. Reserved; not yet
    /// produced.
    SIG_FILE,

    /// `SynModuleOrNamespace` — a top-level module or namespace, including
    /// the implicit anonymous module that wraps the body of a script-style
    /// file with no `module` / `namespace` header.
    MODULE_OR_NAMESPACE,

    /// `SynModuleDecl.Expr` — a top-level expression occurring as a
    /// declaration (only legal in scripts and at the implicit-module top
    /// level).
    EXPR_DECL,

    /// `SynModuleDecl.Open` — an `open` declaration. Shape:
    /// `OPEN_DECL > [OPEN_TOK, LONG_IDENT]` for the module/namespace target
    /// (`SynOpenDeclTarget.ModuleOrNamespace`), or
    /// `OPEN_DECL > [OPEN_TOK, TYPE_TOK, <type>]` for the type target
    /// (`SynOpenDeclTarget.Type`, the `open type T` form). The presence of a
    /// `TYPE_TOK` child distinguishes the two.
    OPEN_DECL,

    /// `SynModuleDecl.NestedModule` — a nested `module X = <block>`
    /// declaration (phase 8.4). Shape:
    /// `NESTED_MODULE_DECL > [MODULE_TOK, REC_TOK?, ACCESS_TOK?, LONG_IDENT,
    /// EQUALS_TOK, <body decls…>]`. The name (FCS's `SynComponentInfo.longId`)
    /// is the single direct [`SyntaxKind::LONG_IDENT`] child; the offside body
    /// decls are direct `*_DECL` children (FCS models them via
    /// `SynComponentInfo`, not a nested `SynModuleOrNamespace`, so this node is
    /// *not* a [`SyntaxKind::MODULE_OR_NAMESPACE`]). The opening/closing
    /// `OBLOCKBEGIN`/`OBLOCKEND` virtuals are kept as zero-width
    /// [`SyntaxKind::ERROR`] placeholders.
    NESTED_MODULE_DECL,

    /// `SynModuleDecl.ModuleAbbrev` — a module abbreviation `module X = LongId`
    /// (FCS's `namedModuleDefnBlock` resolves a body of one bare long-ident to
    /// an abbreviation, `pars.fsy:1427`). Phase 8.4 detects the shape and emits
    /// this **distinct** node (with a deferred-to-8.5 parse error) so it is not
    /// mistaken for a [`NESTED_MODULE_DECL`](SyntaxKind::NESTED_MODULE_DECL):
    /// it is deliberately *not* cast by `ModuleDecl` yet, so consumers don't see
    /// a bogus `NestedModule` projection. Phase 8.5 adds the facade/normaliser
    /// projection (`ModuleAbbrev { ident, long_id }`) over this node. Holds the
    /// same header tokens as a nested module plus the long-ident body (kept
    /// losslessly for 8.5 to reinterpret).
    MODULE_ABBREV_DECL,

    /// `SynModuleDecl.Types` — a group of one or more `and`-joined type
    /// definitions (phase 9.1). FCS aggregates an `and`-chain into one
    /// `Types` node and starts a fresh node at each new `type` keyword
    /// (`SyntaxTree.fsi:1768`), so this carrier holds one or more
    /// [`SyntaxKind::TYPE_DEFN`] children. Shape:
    /// `TYPE_DEFNS > [TYPE_DEFN]` (phase 9.1 emits exactly one; the `and`
    /// multiplicity is phase 9.2). The swallowed `type` keyword and the
    /// offside body's `OBLOCKBEGIN`/`OBLOCKEND` virtuals live inside the
    /// `TYPE_DEFN`.
    TYPE_DEFNS,

    /// `SynTypeDefn` — one type definition (`SyntaxTree.fsi:1642`). Shape for
    /// the abbreviation form (phase 9.1):
    /// `TYPE_DEFN > [TYPE_TOK, LONG_IDENT, EQUALS_TOK, <OBLOCKBEGIN ERROR>,
    /// TYPE_ABBREV, <OBLOCKEND ERROR>]`. The leading `type`/`and` keyword
    /// (`TYPE_TOK`, swallowed by LexFilter and recovered from the raw stream)
    /// precedes the name (`SynComponentInfo.longId`, a `LONG_IDENT`); the
    /// repr node carries the body. Type parameters, members, the implicit
    /// constructor, record/union/enum/object-model reprs, and the
    /// `and` leading keyword arrive in later phase-9 slices.
    TYPE_DEFN,

    /// `SynTypeDefnSimpleRepr.TypeAbbrev` — a type abbreviation's right-hand
    /// side (`type T = <typ>`, `SyntaxTree.fsi:1404`, `pars.fsy:2455`). Wraps
    /// the single `SynType` child (FCS's `rhsType`, parsed by the phase-7
    /// `parse_type` — the full `typ`, so `->`/`*`/postfix-app extend it). The
    /// `ParserDetail` (`Ok`/`ErrorRecovery`) is elided by the normaliser.
    TYPE_ABBREV,

    /// `SynTyparDecls` — a type definition's type-parameter declarations
    /// (`SyntaxTree.fsi:448`, phase 9.3). Holds one or more
    /// [`SyntaxKind::TYPAR_DECL`] children. Three source shapes, all projected
    /// to the same flat typar list (the `PostfixList`/`PrefixList`/`SinglePrefix`
    /// variant and `SynComponentInfo.preferPostfix` are elided): postfix
    /// `T<'a, 'b>` (`[<ERROR HPA>, LESS_TOK, TYPAR_DECL (COMMA_TOK TYPAR_DECL)*,
    /// GREATER_TOK]`, via the phase-7.6 `HighPrecedenceTyApp` machinery) and
    /// single-prefix `'a T` (`[TYPAR_DECL]`, the typar before the name).
    /// Parenthesised-prefix `('a, 'b) T` is deferred (its `)` is swallowed).
    TYPAR_DECLS,

    /// `SynTyparDecl` — one type-parameter declaration (`SyntaxTree.fsi:390`,
    /// phase 9.3). Shape `[(QUOTE_TOK | HAT_TOK), IDENT_TOK]`, wrapping FCS's
    /// `SynTypar(ident, staticReq, _)`: `QUOTE_TOK` for `'a`
    /// (`TyparStaticReq.None`), `HAT_TOK` for `^a` (`TyparStaticReq.HeadType`).
    /// Attributes and intersection constraints (`'T & IDisposable`) are later
    /// slices; for now a decl is just the typar.
    TYPAR_DECL,

    /// The `when …` type-parameter constraint clause (`opt_typeConstraints`,
    /// `pars.fsy:2615`, phase 9.3b). Shape `[WHEN_TOK, TYPAR_CONSTRAINT,
    /// (AND_TOK, TYPAR_CONSTRAINT)*]`. Appears in two source positions, both
    /// projected to the type definition's constraint list: *inside* the angle
    /// brackets (a child of [`SyntaxKind::TYPAR_DECLS`], before `GREATER_TOK` —
    /// FCS's `SynTyparDecls.PostfixList` constraints) and *after* the decls (a
    /// direct child of [`SyntaxKind::TYPE_DEFN`], before `EQUALS_TOK` — FCS's
    /// `SynComponentInfo.constraints`).
    TYPAR_CONSTRAINTS,

    /// One `SynTypeConstraint` (`SyntaxTree.fsi:399`, phase 9.3b). The subject
    /// typar is a [`SyntaxKind::TYPAR_DECL`] child; the kind is read from the
    /// operator/keyword tokens that follow it:
    /// * `[TYPAR_DECL, COLON_TOK, IDENT_TOK]` — `comparison` / `equality` /
    ///   `unmanaged` (`WhereTyparIs{Comparable,Equatable,Unmanaged}`);
    /// * `[TYPAR_DECL, COLON_TOK, STRUCT_TOK]` — `struct`
    ///   (`WhereTyparIsValueType`), or with a leading `IDENT_TOK("not")` →
    ///   `not struct` (`WhereTyparIsReferenceType`);
    /// * `[TYPAR_DECL, COLON_TOK, NULL_TOK]` — `null` (`WhereTyparSupportsNull`),
    ///   or `IDENT_TOK("not")` + `NULL_TOK` → `not null`
    ///   (`WhereTyparNotSupportsNull`);
    /// * `[TYPAR_DECL, COLON_GREATER_TOK, <type>]` — `:> T`
    ///   (`WhereTyparSubtypeOfType`);
    /// * `[TYPAR_DECL, COLON_TOK, IDENT_TOK("enum"), CONSTRAINT_TYPE_ARGS]` —
    ///   `enum<…>` (`WhereTyparIsEnum`), and
    ///   `[TYPAR_DECL, COLON_TOK, DELEGATE_TOK, CONSTRAINT_TYPE_ARGS]` —
    ///   `delegate<…>` (`WhereTyparIsDelegate`); the `< … >` args live in the
    ///   [`SyntaxKind::CONSTRAINT_TYPE_ARGS`] wrapper, read via
    ///   `TyparConstraint::type_args`;
    /// * `[TYPAR_DECL, COLON_TOK, LPAREN_TOK, MEMBER_SIG, RPAREN_TOK]` — an SRTP
    ///   `(member …)` member constraint (`WhereTyparSupportsMember`).
    ///
    /// Deferred (no parser surface yet): `default 'a : t` (library-only) and the
    /// self-constrained bare type.
    TYPAR_CONSTRAINT,

    /// The `< … >` type-argument list of an `enum<…>` / `delegate<…>` typar
    /// constraint — a dedicated wrapper so the args are *not* confused with the
    /// subtype form's direct constraint type ([`crate::syntax::TyparConstraint::ty`]). Shape
    /// `[ERROR (HPA), LESS_TOK, <type> (COMMA_TOK <type>)*, GREATER_TOK]`, the
    /// same `typeArgsNoHpaDeprecated` block a generic type application carries
    /// (built by `consume_type_args_no_hpa`); read via
    /// [`SyntaxKind::TYPAR_CONSTRAINT`]'s `type_args` accessor.
    CONSTRAINT_TYPE_ARGS,

    /// `SynTypeDefnSimpleRepr.Record` — a record type's right-hand side
    /// (`type T = { F : T1; mutable G : T2 }`, `SyntaxTree.fsi:1382`,
    /// `pars.fsy:2479`/`braceFieldDeclList`, phase 9.4). Shape
    /// `[LBRACE_TOK, RECORD_FIELD_DECL (SEMI_TOK | <BlockSep ERROR>)* …, RBRACE_TOK]`.
    /// The `{` is a real `LBrace`; the `}` is LexFilter-swallowed and recovered
    /// from the raw stream (like the CE `}` in 10.2). Record-level accessibility
    /// is elided.
    RECORD_REPR,

    /// `SynField` as a record field (`SyntaxTree.fsi:1490`, phase 9.4). Shape
    /// `[MUTABLE_TOK?, IDENT_TOK, COLON_TOK, <typ>]` — FCS's `idOpt` (the field
    /// name), `isMutable`, and `fieldType` (the full `parse_type`). Attributes /
    /// `isStatic` (always `false` for a record field) / accessibility / xmlDoc
    /// are elided.
    RECORD_FIELD_DECL,

    /// `SynTypeDefnSimpleRepr.Union` — a discriminated-union right-hand side
    /// (`type T = A | B of int`, `SyntaxTree.fsi:1376`, `pars.fsy:2461`/
    /// `unionTypeRepr`, phase 9.5). Shape `[ACCESS_TOK?, BAR_TOK?, UNION_CASE
    /// (BAR_TOK UNION_CASE)*]` — an optional repr-level access modifier, an
    /// optional leading `|`, then `Bar`-separated cases. Union-level
    /// accessibility is elided.
    UNION_REPR,

    /// `SynUnionCase` (`SyntaxTree.fsi:1431`, phase 9.5). Shape
    /// `[IDENT_TOK, (OF_TOK UNION_CASE_FIELD (STAR_TOK UNION_CASE_FIELD)*)?]` —
    /// the case name (`SynIdent`) and the optional `of` field list. Attributes /
    /// accessibility / xmlDoc are elided; operator case names (`(::)`) are a
    /// later slice.
    UNION_CASE,

    /// `SynField` as a discriminated-union case field (`SyntaxTree.fsi:1490`,
    /// phase 9.5). Shape `[(IDENT_TOK COLON_TOK)?, <typ>]` — an optional field
    /// name (`SynField.idOpt`, for `x : T`) then the field type, parsed at the
    /// tuple-segment level (`parse_app_type_can_be_nullable`) so the enclosing
    /// `*` separates fields. `isMutable` (always `false` here) / attributes /
    /// accessibility are elided.
    UNION_CASE_FIELD,

    /// `SynTypeDefnSimpleRepr.Enum` — an enum right-hand side (`type T = A = 0
    /// | B = 1`, `SyntaxTree.fsi:1379`, phase 9.6). Shares the union grammar:
    /// shape `[BAR_TOK?, ENUM_CASE (BAR_TOK ENUM_CASE)*]`. Whether a
    /// `unionTypeRepr` body is an `Enum` or a [`UNION_REPR`](SyntaxKind::UNION_REPR)
    /// is decided post-hoc — any `Name = value` case makes it an enum
    /// (`pars.fsy:2461`).
    ENUM_REPR,

    /// `SynEnumCase` (`SyntaxTree.fsi:1416`, phase 9.6). Shape
    /// `[IDENT_TOK, EQUALS_TOK, <value-expr>]` — the case name (`SynIdent`) and
    /// its value, which is a `SynExpr` (`atomicExpr`, e.g. `Const 0`), not a
    /// `SynConst`. Attributes / xmlDoc are elided.
    ENUM_CASE,

    /// `SynTypeDefnRepr.ObjectModel` — a class-like type body whose members are
    /// declared in a block (`type T =\n  member this.M = …`,
    /// `SyntaxTree.fsi:1629`, `pars.fsy:1812`/`classDefnBlockKindUnspecified`,
    /// phase 9.7). Shape `[MEMBER_DEFN …]` — one or more member nodes. The
    /// `SynTypeDefnKind` is `Unspecified` for a bare `type T = member …` (the
    /// explicit `class`/`struct`/`interface … end` kind markers and the
    /// implicit constructor are later phase-9 slices). The body-opening
    /// `OBLOCKBEGIN` and body-closing `OBLOCKEND` virtuals live in the enclosing
    /// [`SyntaxKind::TYPE_DEFN`] (consumed by `parse_type_defn_repr`), as for the
    /// simple reprs; each member's own `OBLOCKEND·ODECLEND·OBLOCKSEP` virtuals
    /// are zero-width `ERROR` placeholders inside this node.
    OBJECT_MODEL_REPR,

    /// A delegate type-definition body — `type T = delegate of int -> int`
    /// (`pars.fsy:1779`, the `DELEGATE OF topType` rule). FCS lowers this to
    /// `SynTypeDefnRepr.ObjectModel(SynTypeDefnKind.Delegate(ty, arity),
    /// [AbstractSlot "Invoke"], _)`, where `ty` is the signature type and the
    /// `arity`/synthetic `Invoke` slot are both derived from the same
    /// `topType`. We keep the surface shape instead: `[DELEGATE_TOK, OF_TOK,
    /// <type>]` — the `delegate`/`of` keywords and the signature `SynType`
    /// (parsed by the shared `parse_type`). The body-opening/closing
    /// `OBLOCKBEGIN`/`OBLOCKEND` virtuals live in the enclosing
    /// [`SyntaxKind::TYPE_DEFN`], as for the simple reprs.
    DELEGATE_REPR,

    /// `SynTypeDefnSimpleRepr.LibraryOnlyILAssembly` — FSharp.Core's inline-IL
    /// **type** definition body `( # "instr" # )` (`pars.fsy:2483`'s
    /// `LPAREN HASH string HASH rparen`), e.g. `type byref<'T> = (# "!0&" #)`.
    /// Shape `[LPAREN_TOK, HASH_TOK, <il-string-lit>, HASH_TOK, RPAREN_TOK]`:
    /// the type-definition production owns the surrounding `(`/`)` directly
    /// (unlike the expression-position [`SyntaxKind::INLINE_IL_EXPR`], which FCS
    /// wraps in a `Paren`), so they are children of this node, not a wrapper.
    /// The closing `)` is LexFilter-swallowed (like every paren closer) and
    /// recovered from the raw stream. The IL instruction string is a bare
    /// literal token (FCS parses it with `ParseAssemblyCodeType`, not as a
    /// `SynType`), so it is *not* wrapped in a type node. FSharp.Core-only; the
    /// grammar takes neither type arguments, value arguments, nor a return type
    /// (those belong to the expression form alone).
    INLINE_IL_REPR,

    /// `SynMemberDefn.Member` — an object-model member binding
    /// (`SyntaxTree.fsi:1663`, phase 9.7). Shape `[MEMBER_TOK, BINDING]`: the
    /// `member` leading keyword followed by a member `SynBinding`. The binding's
    /// `headPat` is a dotted [`SyntaxKind::LONG_IDENT_PAT`] (`this.M`, the
    /// self-identifier plus the member name), and its RHS reuses the shared
    /// `parse_let_equals_rhs`. The `SynValData`/`SynMemberFlags` (always an
    /// instance member here) are elided; the `MEMBER_TOK` is the binding's
    /// `SynLeadingKeyword.Member`.
    MEMBER_DEFN,

    /// `SynMemberDefn.ImplicitCtor` — a type's implicit primary constructor
    /// (`type T(x: int) [as self] = …`, `SyntaxTree.fsi:1673`,
    /// `pars.fsy:1647`'s `opt_simplePatterns`, phase 9.8a). Shape
    /// `[<args-pat>, AS_TOK?, IDENT_TOK?]` — the constructor argument pattern
    /// then the optional `as <self-id>`. The args are a regular `SynPat` (FCS
    /// 43.x unifies ctor args into `SynPat`, *not* the older `SynSimplePats`):
    /// a bare [`SyntaxKind::CONST_PAT`] (unit) for the empty `()`, else a
    /// [`SyntaxKind::PAREN_PAT`]. A direct child of [`SyntaxKind::TYPE_DEFN`]
    /// (parsed in the header); the normaliser projects it to *both*
    /// `SynTypeDefn.implicitConstructor` and the prepended head of the
    /// `ObjectModel` repr's member list, mirroring FCS's dual placement.
    IMPLICIT_CTOR,

    /// `SynMemberDefn.LetBindings` — class-local `let`/`let rec` bindings in an
    /// object-model body (`type T() =`⏎`  let x = …`, `SyntaxTree.fsi:1691`,
    /// `pars.fsy:3152`'s `classDefnBindings`, phase 9.8b). Same internal shape
    /// as a [`SyntaxKind::LET_DECL`] (`[LET_TOK, REC_TOK?, BINDING,
    /// (AND_TOK BINDING)*]`), parsed by the shared `parse_let_decl_at`; the
    /// distinct node kind is what marks it a *member* rather than a module-level
    /// `let`. `isStatic` (always `false` here) is elided; `isRecursive` is the
    /// `REC_TOK`. The `do`-binding form (`do …`, a `LetBindings` with a `Do`
    /// binding) is the separate [`SyntaxKind::MEMBER_DO`] node.
    MEMBER_LET_BINDINGS,

    /// A class-body `do <expr>` binding (`type T() =`⏎`  do printfn …`, phase
    /// 9.8d) — FCS's `SynMemberDefn.LetBindings([SynBinding(kind = Do, …)],
    /// isStatic, isRecursive = false, …)` (the `do`-binding `classDefnBindings`
    /// arm). Shape `[STATIC_TOK?, DO_EXPR]`: a leading `STATIC_TOK` for a
    /// `static do`, then the reused statement-level [`SyntaxKind::DO_EXPR`]
    /// holding the `do` keyword and offside-block body. The distinct node kind
    /// marks it a *member* (its body is the constructor's, not a `SynModuleDecl`).
    MEMBER_DO,

    /// `SynExpr.Const` — a constant-literal expression.
    CONST_EXPR,

    /// `SynExpr.Null` — the `null` literal expression. FCS keeps this
    /// distinct from [`SyntaxKind::CONST_EXPR`] (`null` is *not* a
    /// `SynConst`; it is its own `atomicExpr` production, `pars.fsy:5402`).
    /// Shape: `NULL_EXPR > [NULL_TOK]`.
    NULL_EXPR,

    /// `SynExpr.Ident` — a single-identifier expression. FCS describes
    /// this as the "optimized representation for SynExpr.LongIdent
    /// (false, \[id\], id.idRange)" (SyntaxTree.fsi:805).
    IDENT_EXPR,

    /// `SynExpr.LongIdent` — a dotted-path expression
    /// (`Foo.Bar.Baz`). Wraps a single [`SyntaxKind::LONG_IDENT`] child holding the
    /// `IDENT_TOK`s and `DOT_TOK`s in source order. Phase 2 only emits
    /// this when the path has at least two segments; single-segment
    /// paths take FCS's `SynExpr.Ident` optimised representation via
    /// [`SyntaxKind::IDENT_EXPR`].
    LONG_IDENT_EXPR,

    /// `SynExpr.Paren` — a parenthesised expression `( e )`. Distinct from
    /// the unit literal `()` (which lives under [`SyntaxKind::CONST_EXPR`] as
    /// `LPAREN_TOK` + `RPAREN_TOK` with only trivia between). FCS keeps
    /// `Paren` in the AST to distinguish `A.M((x, y))` from `A.M(x, y)`,
    /// among other tooling-relevant cases (`SyntaxTree.fsi:594-598`).
    /// Shape: `PAREN_EXPR > [LPAREN_TOK, <inner-expr>, RPAREN_TOK]` with
    /// trivia interleaved.
    PAREN_EXPR,

    /// `SynExpr.Tuple` — a comma-separated tuple `e1, e2, …` (always at
    /// least two elements; a single expression without a trailing comma
    /// is not a tuple). Shape: `TUPLE_EXPR > [<expr>, COMMA_TOK,
    /// <expr>, COMMA_TOK, …, <expr>]` with trivia interleaved. The
    /// struct-tuple form `struct (1, 2)` carries the same shape under
    /// a `STRUCT_TOK`-marked wrapper and is deferred to a later phase.
    TUPLE_EXPR,

    /// `SynExpr.App` — function application `f x`. Always binary: `f x y`
    /// nests as `App(App(f, x), y)` (left-associative). Shape: `APP_EXPR >
    /// [<func-expr>, <arg-expr>]` with trivia interleaved. FCS's
    /// `ExprAtomicFlag` field distinguishes `f(x)` (Atomic, adjacent) from
    /// `f x` (NonAtomic, whitespace-separated); Phase 3.3 only emits the
    /// NonAtomic form — the Atomic case requires the LexFilter to surface
    /// `HIGH_PRECEDENCE_PAREN_APP`, which is deferred.
    APP_EXPR,

    /// `SynExpr.App(_, isInfix = true, …)` — the *inner* App produced by
    /// `mkSynInfix` for `a OP b`, where OP is applied to its LHS. FCS
    /// encodes the binary form as `App(NonAtomic, false, App(NonAtomic,
    /// true, op, lhs), rhs)`. The inner `App` carries `isInfix = true`;
    /// the outer one (wrapping it + the RHS) is a regular [`SyntaxKind::APP_EXPR`].
    ///
    /// Shape: `INFIX_APP_EXPR > [<lhs-expr>, <op-expr>]` in *source* order
    /// (lossless invariant). The typed-AST accessors (`AppExpr::func` /
    /// `AppExpr::arg`) swap them to match FCS's `funcExpr = op,
    /// argExpr = lhs`. The op-expr is a single-segment
    /// [`SyntaxKind::LONG_IDENT_EXPR`] wrapping an [`SyntaxKind::IDENT_TOK`] carrying the
    /// operator's source text (`+`, `<=`, `::`, …); FCS stores the
    /// compile-name-mangled form (`op_Addition`) in `Ident.idText` and
    /// keeps the original via `IdentTrivia.OriginalNotation`.
    INFIX_APP_EXPR,

    /// Zero-width marker for FCS's `ExprAtomicFlag.Atomic` — the
    /// LexFilter `Virtual::HighPrecedenceParenApp` virtual that sits between
    /// an atomic expression and an immediately-adjacent `(` (`f(x)`, no
    /// whitespace). Emitted as a direct child of the [`SyntaxKind::APP_EXPR`]
    /// it introduces, between the function and the argument; carries no text
    /// (lossless: the bytes are the following `(`). Its mere *presence*
    /// records that the application is atomic (`f(x)`) rather than
    /// whitespace-separated (`f (x)`); [`crate::syntax::AppExpr::is_atomic`]
    /// reads it to recover FCS's `App(ExprAtomicFlag.Atomic, …)` flag.
    /// Previously consumed as a bare `ERROR`; the dedicated kind keeps the
    /// flag in the green tree instead of discarding it. Stamped at every site
    /// that builds an [`SyntaxKind::APP_EXPR`] from the marker — expression
    /// application (`parse_app_expr`) and enum-case values (`f(1)` in
    /// `type E = A = f(1)`). The sibling marker in binding heads `let f(x)`
    /// (a `SynValData` function head) and attribute args (`[<A(1)>]`, where
    /// `A` is the attribute *type* and `(1)` its arg — no `App` node) stays
    /// `ERROR`: those are not `SynExpr.App` nodes and carry no atomic flag.
    HIGH_PRECEDENCE_PAREN_APP_TOK,

    /// Zero-width marker for FCS's `ExprAtomicFlag.Atomic` on the *non-dotted*
    /// bracket indexer `arr[i]` — the LexFilter
    /// [`crate::lexfilter::Virtual::HighPrecedenceBrackApp`] virtual that sits
    /// between an *ident* and an immediately-adjacent `[` (`arr[i]`, no
    /// whitespace). The sibling of [`SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK`]
    /// for the `pars.fsy:5242` production `atomicExpr HIGH_PRECEDENCE_BRACK_APP
    /// atomicExpr`, which lowers to `SynExpr.App(ExprAtomicFlag.Atomic, false,
    /// head, ArrayOrListComputed[…])` — an atomic application of the head to a
    /// bracketed list literal, *not* a [`SyntaxKind::DOT_INDEXED_GET_EXPR`]
    /// (that is the *dotted* `arr.[i]`). Emitted as a direct child of the
    /// [`SyntaxKind::APP_EXPR`] it introduces, between the function and the
    /// list-literal argument; carries no text (lossless: the bytes are the
    /// following `[`). [`crate::syntax::AppExpr::is_atomic`] reads either this
    /// or the paren marker to recover the atomic flag. Only the ident-adjacent
    /// `[` carries it; a `[` after `)` / `]` (or whitespace-separated) is an
    /// ordinary `ExprAtomicFlag.NonAtomic` whitespace application with no marker.
    HIGH_PRECEDENCE_BRACK_APP_TOK,

    /// `SynExpr.DotGet` — postfix member access `expr.Member` (phase
    /// 10.16a). FCS: `DotGet of expr * rangeOfDot * longDotId: SynLongIdent
    /// * range` (`SyntaxTree.fsi:822`). Only produced when the LHS is *not*
    /// a plain ident / long-ident — a pure identifier chain `a.b.c` is
    /// `SynExpr.LongIdent` ([`SyntaxKind::LONG_IDENT_EXPR`]) instead, per
    /// `mkSynDot`'s append arms. Consecutive members fold into one
    /// `SynLongIdent` (`(f x).Bar.Baz`).
    ///
    /// Shape: `DOT_GET_EXPR > [<inner-expr>, LONG_IDENT]`, where the
    /// [`SyntaxKind::LONG_IDENT`] child holds the leading [`SyntaxKind::DOT_TOK`]
    /// and the member [`SyntaxKind::IDENT_TOK`]s (`.Bar.Baz` =
    /// `[DOT_TOK, IDENT_TOK, DOT_TOK, IDENT_TOK]`); `LongIdent::idents`
    /// projects the segment texts.
    DOT_GET_EXPR,

    /// `SynExpr.Dynamic` — the dynamic-lookup operator `a?b` (FCS's
    /// `atomicExpr QMARK dynamicArg`, `pars.fsy:5284`). `?` is a *postfix*
    /// operator at the precedence of `.` (`%left DOT QMARK`, `pars.fsy:377`),
    /// left-associative and with no adjacency requirement (`a?b` ≡ `a ? b`).
    /// FCS: `Dynamic of funcExpr: SynExpr * qmarkRange: range *
    /// argExpr: SynExpr * range`. The `argExpr` is either a single
    /// `SynExpr.Ident` (the `IDENT` `dynamicArg`, the dynamic member name) or a
    /// `SynExpr.Paren` (the `( typedSequentialExpr )` `dynamicArg`).
    ///
    /// Shape: `DYNAMIC_EXPR > [<lhs-expr>, QMARK_TOK, <arg-expr>]`. Sits in the
    /// postfix tail, so a following `.member` / adjacent application chains onto
    /// the whole node (`a?b.c` = `DotGet(Dynamic(a, b), [c])`); a chain `a?b?c`
    /// nests left (`Dynamic(Dynamic(a, b), c)`). The `<-` set form `a?b <- v`
    /// has no dedicated node — FCS lowers it to `Set(Dynamic(a, b), v)` via the
    /// generic assignment fallback, which our `<-` handling reproduces for free.
    DYNAMIC_EXPR,

    /// `SynExpr.DotLambda` — the accessor-function shorthand `_.Member`
    /// (FCS's `LanguageFeature.AccessorFunctionShorthand`, `pars.fsy:5212`
    /// `UNDERSCORE DOT atomicExpr`). `_.Foo` is sugar for `(fun x -> x.Foo)`;
    /// the synthesised lambda parameter is introduced at type-check time, so
    /// the parse tree carries only the *body* (the `atomicExpr` after `_.`).
    /// FCS: `DotLambda of expr: SynExpr * range * trivia: SynExprDotLambdaTrivia`
    /// (`SyntaxTree.fsi:826`); the trivia's `UnderscoreRange`/`DotRange` are
    /// elided here.
    ///
    /// Shape: `DOT_LAMBDA_EXPR > [UNDERSCORE_TOK, DOT_TOK, <body-expr>]`. The
    /// single `Expr` child is the body — exactly what [`SyntaxKind::DOT_GET_EXPR`]
    /// folding produces for the rest of the chain, so `_.Foo.Bar` is
    /// `DotLambda(LongIdent ["Foo"; "Bar"])` and `_.Item(3)` is
    /// `DotLambda(App(Atomic, Ident "Item", Paren …))`.
    DOT_LAMBDA_EXPR,

    /// `SynExpr.DotIndexedGet` — a dotted indexer read `expr.[index]`
    /// (phase 10.16a). FCS: `DotIndexedGet of objectExpr * indexArgs:
    /// SynExpr * dotRange * range` (`SyntaxTree.fsi:834`), built by
    /// `mkSynDotBrackGet`. `indexArgs` is a single expression (a
    /// [`SyntaxKind::TUPLE_EXPR`] for the multi-arg `arr.[i, j]`).
    ///
    /// Shape: `DOT_INDEXED_GET_EXPR > [<object-expr>, DOT_TOK, LBRACK_TOK,
    /// <index-expr>, RBRACK_TOK]`. The two `Expr` children are the object
    /// (first) and the index (second); the bracket closer `]` is a real
    /// `RBRACK_TOK` (the lex-filter does not swallow it, unlike `)`).
    DOT_INDEXED_GET_EXPR,

    /// `SynExpr.TypeApp` — expression-level generic type application
    /// `f<int>` (phase 10.20). FCS: `TypeApp of expr: SynExpr *
    /// lessRange: range * typeArgs: SynType list * commaRanges: range list *
    /// greaterRange: range option * typeArgsRange: range * range`
    /// (`SyntaxTree.fsi:749`), built by the `atomicExpr` production
    /// `atomicExpr HIGH_PRECEDENCE_TYAPP typeArgsActual` (`pars.fsy:5252`).
    /// A postfix continuation at the *atomic* level — like
    /// [`SyntaxKind::DOT_GET_EXPR`] and the high-precedence paren application —
    /// so it binds tighter than a whitespace function application
    /// (`f<int> x` = `App(TypeApp(f, [int]), x)`) and a `(` adjacent to the
    /// closing `>` opens an atomic application (`ResizeArray<_>()` =
    /// `App(Atomic, TypeApp(ResizeArray, [Anon]), Const Unit)`), the
    /// [`SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK`] marker between `>` and `(`
    /// being emitted by the LexFilter just as for any ident-adjacent `(`.
    ///
    /// Shape: `TYPE_APP_EXPR > [<head-expr>, LESS_TOK, <type-arg>(, COMMA_TOK,
    /// <type-arg>)*, GREATER_TOK]`. The `HIGH_PRECEDENCE_TYAPP` adjacency
    /// marker the LexFilter emits between the head and the `<` is consumed
    /// zero-width as an `ERROR` token (the same idiom the prefix
    /// [`SyntaxKind::APP_TYPE`] uses for FCS's elided `HIGH_PRECEDENCE_TYAPP`)
    /// and is not surfaced. The first [`crate::syntax::Expr`] child is the
    /// type-applied head; the [`crate::syntax::Type`] children between the
    /// `<` / `>` are the type arguments. The typed-AST accessors
    /// [`crate::syntax::TypeAppExpr::expr`] / [`crate::syntax::TypeAppExpr::type_args`]
    /// read them.
    TYPE_APP_EXPR,

    /// `SynExpr.IndexRange` — a range / slice expression `lower..upper`
    /// (phase 10.22). FCS: `IndexRange of expr1: SynExpr option * opm: range *
    /// expr2: SynExpr option * range1 * range2 * range` (`SyntaxTree.fsi:690`),
    /// the general `..` range used both as an indexer argument (`arr.[2..]`)
    /// and as a list / array / `for` range (`[1..10]`, `for i in 1..10`).
    /// `..` is a low-precedence operator binding just above `,` (FCS's
    /// `%left DOT_DOT`), so it sits between the tuple layer and the Pratt
    /// infix layer.
    ///
    /// Shape: `INDEX_RANGE_EXPR > [<lower>?, DOT_DOT_TOK, <upper>?]`. Either
    /// bound may be absent — `2..` is `[<lower>, DOT_DOT_TOK]`, `..3` is
    /// `[DOT_DOT_TOK, <upper>]`. The typed-AST accessors
    /// [`crate::syntax::IndexRangeExpr::lower`] /
    /// [`crate::syntax::IndexRangeExpr::upper`] recover the two `SynExpr
    /// option` bounds by their position relative to the `DOT_DOT_TOK`.
    ///
    /// The whole-dimension wildcard `*` (FCS's nullary `STAR` production,
    /// `arr.[*]` / `m.[*, 1..2]`, phase 10.22a) is the **both-bounds-absent**
    /// `IndexRange(None, None)`, carried by the variant shape
    /// `INDEX_RANGE_EXPR > [STAR_TOK]` — a `*` token and *no* `DOT_DOT_TOK`.
    /// `lower()` / `upper()` both return `None` for it (no `DOT_DOT_TOK`, no
    /// `Expr` child), matching FCS's `(None, _, None, …)`.
    INDEX_RANGE_EXPR,

    /// `SynExpr.IndexFromEnd` — a from-end index/slice bound `^expr` (`arr.[^1]`,
    /// `arr.[^3..]`, `arr.[..^1]`, phase 10.22b). A `minusExpr`-level prefix in
    /// FCS, so it appears wherever a `declExpr` does (also `let i = ^1`, `[ ^1 ]`).
    /// Shape: `INDEX_FROM_END_EXPR > [HAT_TOK, <expr>]` — the leading `^`
    /// (`Token::Op("^")`) then the bound; the typed-AST accessor
    /// [`crate::syntax::IndexFromEndExpr::expr`] returns the bound.
    INDEX_FROM_END_EXPR,

    /// `SynExpr.AddressOf` — the `&expr` / `&&expr` prefix forms. FCS
    /// keeps these as a distinct AST node rather than an App over a
    /// pseudo-operator (`SyntaxTree.fsi`'s `AddressOf of isByref * expr *
    /// opRange * range`). `&` produces `isByref = true` (managed byref),
    /// `&&` produces `isByref = false` (unmanaged nativeptr).
    ///
    /// Shape: `ADDRESS_OF_EXPR > [<op-token>, <inner-expr>]` where the
    /// op token is [`SyntaxKind::AMP_TOK`] or [`SyntaxKind::AMP_AMP_TOK`].
    /// The typed-AST accessor [`crate::syntax::AddressOfExpr::is_byref`]
    /// reads the op-token kind to recover FCS's `isByref` flag.
    ADDRESS_OF_EXPR,

    /// `SynExpr.New` — an object-construction expression `new T(args)`
    /// (FCS's `minusExpr` production `NEW atomType opt_HIGH_PRECEDENCE_APP
    /// atomicExprAfterType`, `pars.fsy:5173`). FCS keeps it as a distinct
    /// AST node, `New of isProtected: bool * targetType: SynType *
    /// expr: SynExpr * range` (`SyntaxTree.fsi:642`). The expression form
    /// always yields `isProtected = false` (the `true` case is reserved for
    /// `inherit`-style base construction, which has no expression surface),
    /// so this node carries no protected flag.
    ///
    /// Shape: `NEW_EXPR > [NEW_TOK, <target-type>, <arg-expr>]`. The
    /// `opt_HIGH_PRECEDENCE_APP` adjacency marker before the args (`new
    /// T(…)` has no space) is consumed zero-width as an `ERROR` token (the
    /// same idiom [`SyntaxKind::INHERIT_MEMBER`] / [`SyntaxKind::IMPLICIT_CTOR`]
    /// use for FCS's elided `opt_HIGH_PRECEDENCE_APP`) and is not surfaced.
    /// The argument is `atomicExprAfterType` (`()` → `Const Unit`, `(a, b)`
    /// → `Paren(Tuple)`, …), parsed head-only so a trailing `.Member`
    /// belongs to the enclosing expression, not the args (matching FCS,
    /// which requires `(new T()).Member`). The typed-AST accessors
    /// [`crate::syntax::NewExpr::target_type`] / [`crate::syntax::NewExpr::arg`]
    /// read the type and argument children.
    NEW_EXPR,

    /// `SynExpr.ObjExpr` — an *object expression* `{ new T(args) with member … }`
    /// (FCS's `objExpr`, `pars.fsy:5828`). Distinct from [`SyntaxKind::NEW_EXPR`]
    /// (the object-*construction* `new T(args)`): an object expression is a
    /// brace-delimited anonymous implementation of an interface or class, with
    /// member overrides and optional extra interface implementations. FCS keeps
    /// it as `ObjExpr of objType: SynType * argOptions: (SynExpr * Ident option)
    /// option * withKeyword: range option * bindings: SynBinding list *
    /// members: SynMemberDefns * extraImpls: SynInterfaceImpl list *
    /// newExprRange: range * range` (`SyntaxTree.fsi:645`).
    ///
    /// Shape (Stage A — the member form, no extra interfaces):
    /// `OBJ_EXPR > [LBRACE_TOK, NEW_EXPR, WITH_TOK?, <member-node>*, RBRACE_TOK]`.
    /// The leading [`SyntaxKind::NEW_EXPR`] child carries the object type
    /// (`objType`) and the optional constructor argument (`argOptions` — `None`
    /// when the `new T` form has no parens, `Some` for `new T(args)`); the
    /// typed-AST accessors [`crate::syntax::ObjExpr::obj_type`] /
    /// [`crate::syntax::ObjExpr::arg`] reach through it. Members (the
    /// `with member …` block) are the `MEMBER_DEFN`/`GET_SET_MEMBER` children,
    /// parsed by the same offside member-block machinery as a `type T with
    /// member …` augmentation. The closing `}` is LexFilter-swallowed and
    /// recovered from the raw stream like every other brace expression.
    OBJ_EXPR,

    /// `SynExpr.InferredUpcast` — the `upcast e` keyword-prefix coercion
    /// (FCS's `minusExpr` production `UPCAST minusExpr`, `pars.fsy:5182`).
    /// FCS keeps it as a distinct AST node, `InferredUpcast of expr: SynExpr *
    /// range` (`SyntaxTree.fsi:867`): unlike the `:>` infix coercion
    /// (`SynExpr.Upcast`, which carries a target type) the *inferred* form
    /// has no explicit type — it is supplied by inference.
    ///
    /// Shape: `INFERRED_UPCAST_EXPR > [UPCAST_TOK, <inner-expr>]`. The
    /// operand is a `minusExpr` (same precedence layer as the address-of
    /// prefix), parsed by [`crate::syntax::InferredUpcastExpr::expr`]'s
    /// producer at that level. Distinct from [`SyntaxKind::INFERRED_DOWNCAST_EXPR`]
    /// purely by the keyword token / FCS variant.
    INFERRED_UPCAST_EXPR,

    /// `SynExpr.InferredDowncast` — the `downcast e` keyword-prefix coercion
    /// (FCS's `minusExpr` production `DOWNCAST minusExpr`, `pars.fsy:5185`).
    /// `InferredDowncast of expr: SynExpr * range` (`SyntaxTree.fsi:870`);
    /// the inferred (typeless) sibling of the `:?>` infix `SynExpr.Downcast`.
    ///
    /// Shape: `INFERRED_DOWNCAST_EXPR > [DOWNCAST_TOK, <inner-expr>]`. See
    /// [`SyntaxKind::INFERRED_UPCAST_EXPR`].
    INFERRED_DOWNCAST_EXPR,

    /// `SynExpr.Lazy` — the `lazy e` delayed-computation prefix (FCS's
    /// production `LAZY declExpr %prec expr_lazy`, `pars.fsy:4346`,
    /// `Lazy of expr: SynExpr * range`, `SyntaxTree.fsi:873`).
    ///
    /// Shape: `LAZY_EXPR > [LAZY_TOK, <inner-expr>]`. Unlike the
    /// `minusExpr`-operand prefixes `upcast`/`downcast`/`&`, `lazy` sits at FCS's
    /// `expr_app` precedence (tighter than every infix operator) with a
    /// grammatical `declExpr` operand — precedence clips that operand to this
    /// codebase's `parse_minus_expr` level *plus* a leading open-lower range
    /// (`lazy ..3` = `Lazy(IndexRange(None, 3))`), parsed by
    /// [`crate::syntax::LazyExpr::expr`]'s producer. So `lazy a + b` =
    /// `(lazy a) + b` but `lazy f y` = `Lazy(App(f, y))` and `lazy if … ` =
    /// `Lazy(IfThenElse …)`. Sibling of [`SyntaxKind::ASSERT_EXPR`].
    LAZY_EXPR,

    /// `SynExpr.Assert` — the `assert e` runtime-assertion prefix (FCS's
    /// production `ASSERT declExpr %prec expr_assert`, `pars.fsy:4349`,
    /// `Assert of expr: SynExpr * range`, `SyntaxTree.fsi:876`).
    ///
    /// Shape: `ASSERT_EXPR > [ASSERT_TOK, <inner-expr>]`. Identical precedence
    /// and operand grammar to [`SyntaxKind::LAZY_EXPR`] — the two are dispatched
    /// through the same parameterised producer. (FCS additionally reports a
    /// dedicated "assert is not a first-class value" error for the operandless
    /// `assert`; we surface that as the shared missing-operand recovery error.)
    ASSERT_EXPR,

    /// `SynExpr.Fixed` — the `fixed e` pinning prefix (FCS's production
    /// `FIXED declExpr`, `pars.fsy:4624`, `Fixed of expr: SynExpr * range`,
    /// `SyntaxTree.fsi:966`).
    ///
    /// Shape: `FIXED_EXPR > [FIXED_TOK, <inner-expr>]`. Looks like
    /// [`SyntaxKind::LAZY_EXPR`] but binds the **opposite** way: `FIXED declExpr`
    /// carries *no* `%prec`, so the rule inherits its rightmost terminal's
    /// precedence (`FIXED`, which has none) and every shift/reduce conflict
    /// defaults to *shift* — the operand greedily absorbs the whole `declExpr`.
    /// So `fixed a + b` = `Fixed(a + b)`, `fixed a, b` = `Fixed(Tuple(a, b))`,
    /// `fixed a :> T` = `Fixed(Upcast(a, T))`, `fixed if … ` = `Fixed(IfThenElse
    /// …)`, all *folding into* the operand — where the tighter `lazy a + b` =
    /// `(lazy a) + b`. Only the `: T` type annotation, `;` sequencing, and `in`
    /// (all above `declExpr` in FCS's grammar) bind looser: `fixed a : T` =
    /// `Typed(Fixed a, T)`. The operand is parsed by
    /// [`crate::syntax::FixedExpr::expr`]'s producer with the full `parse_expr`
    /// (== FCS `declExpr`), not the tight Pratt frame `lazy`/`assert` use.
    ///
    /// `fixed` is only *semantically* valid as a `use x = fixed e` binding RHS,
    /// but that restriction is a typecheck error — FCS (and we) build the node
    /// anywhere a `declExpr` appears.
    FIXED_EXPR,

    /// `SynExpr.TypeTest(expr, targetType, range)` — the dynamic type-test
    /// operator `e :? T` (`declExpr COLON_QMARK typ`, `pars.fsy:4634`). FCS keeps
    /// it as a distinct AST node rather than an `App`. The left operand is a full
    /// `declExpr`; the right operand is a *type* (`typ`, arrow/tuple/generic
    /// inclusive). Shape: `TYPE_TEST_EXPR > [<inner-expr>, COLON_QMARK_TOK,
    /// <type>]` with trivia interleaved. The typed-AST accessors
    /// [`crate::syntax::TypeTestExpr::expr`] / [`crate::syntax::TypeTestExpr::ty`]
    /// read the expression and type children. Left-associative, sitting between
    /// `::` and `+`/`-` in the precedence table (`pars.fsy:363`). A `:?` with no
    /// following type is the `COLON_QMARK recover` arm: the node carries just the
    /// expression and operator, with an "expected type" error.
    TYPE_TEST_EXPR,

    /// `SynExpr.Upcast(expr, targetType, range)` — the upcast operator `e :> T`
    /// (`declExpr COLON_GREATER typ`, `pars.fsy:4642`). Like
    /// [`SyntaxKind::TYPE_TEST_EXPR`], a distinct node over a `declExpr` LHS and a
    /// `typ` RHS. Shape: `UPCAST_EXPR > [<inner-expr>, COLON_GREATER_TOK,
    /// <type>]`. The typed-AST accessors [`crate::syntax::UpcastExpr::expr`] /
    /// [`crate::syntax::UpcastExpr::ty`] read the children. Left-associative,
    /// sitting just below the compare bucket (`pars.fsy:358`). The
    /// `COLON_GREATER recover` arm (missing type) carries just expression +
    /// operator with an "expected type" error.
    UPCAST_EXPR,

    /// `SynExpr.Downcast(expr, targetType, range)` — the dynamic downcast
    /// operator `e :?> T` (`declExpr COLON_QMARK_GREATER typ`, `pars.fsy:4650`).
    /// Like [`SyntaxKind::UPCAST_EXPR`] but for `:?>` and projecting to
    /// `SynExpr.Downcast`. Shape: `DOWNCAST_EXPR > [<inner-expr>,
    /// COLON_QMARK_GREATER_TOK, <type>]`. The typed-AST accessors
    /// [`crate::syntax::DowncastExpr::expr`] / [`crate::syntax::DowncastExpr::ty`]
    /// read the children. Left-associative at the same precedence band as `:>`
    /// (`pars.fsy:358`). The `COLON_QMARK_GREATER recover` arm (missing type)
    /// carries just expression + operator with an "expected type" error.
    DOWNCAST_EXPR,

    /// The cons expression `a :: b` — FCS's `declExpr COLON_COLON declExpr`
    /// (`pars.fsy:4765`), `%right COLON_COLON` (`:361`). Unlike the `mkSynInfix`
    /// operators (`+`, `*`, …), FCS does *not* lower this to the two-tier
    /// `App(App(op, lhs), rhs)` shape: it builds a *single*
    /// `App(NonAtomic, isInfix = true, op_ColonColon, Tuple(false, [lhs; rhs]))`
    /// — the operator applied to a synthesised pair. We keep a dedicated node
    /// (mirroring the pattern-side [`SyntaxKind::LIST_CONS_PAT`]) for a lossless
    /// `text(tree) == source` tree, and the normaliser projects it to that
    /// App-of-Tuple shape so the diff against FCS lines up. Right-associative
    /// (`a :: b :: c` is `a :: (b :: c)`) and sits between `@`/`^` (looser,
    /// `:360`) and `:?` / `+`/`-` (tighter, `:363`/`:364`); the Pratt climber
    /// picks it up via [`crate::parser`]'s cons continuation, beside the
    /// type-relation operators.
    ///
    /// Shape: `CONS_EXPR > [<lhs-expr>, COLON_COLON_TOK, <rhs-expr>]`. The
    /// typed-AST accessors [`crate::syntax::ConsExpr::lhs`] /
    /// [`crate::syntax::ConsExpr::rhs`] read the two `Expr` children.
    CONS_EXPR,

    /// The query computation-expression join operator — `<lhs> in <rhs>`
    /// (`pars.fsy:4669 declExpr JOIN_IN declExpr` → `SynExpr.JoinIn(lhsExpr,
    /// lhsRange, rhsExpr, range)`). The `in` keyword is rewritten to the
    /// `JOIN_IN` token by LexFilter whenever it sits inside a `{ … }` brace
    /// computation-expression body ([`crate::lexfilter`]'s `detect_join_in_ctxt`,
    /// FCS `LexFilter.fs:747`), so `query { join x in xs on (a = b) }` parses
    /// the body as `JoinIn(App(join, x), App(App(xs, on), Paren(a = b)))`. The
    /// detection is contextual, not keyword-driven, so even `query { a in b }`
    /// is a `JoinIn`. Left-associative at the `||`/`or` precedence band
    /// (`%left OR BAR_BAR JOIN_IN`, `pars.fsy:352`); the Pratt climber picks it
    /// up via [`crate::parser`]'s join-in continuation, beside the cons and
    /// type-relation operators.
    ///
    /// Shape: `JOIN_IN_EXPR > [<lhs-expr>, IN_TOK, <rhs-expr>]`. The typed-AST
    /// accessors [`crate::syntax::JoinInExpr::lhs`] /
    /// [`crate::syntax::JoinInExpr::rhs`] read the two `Expr` children.
    JOIN_IN_EXPR,

    /// The `<-` mutation surface — `<lhs> <- <rhs>`
    /// (`pars.fsy:4661 minusExpr LARROW declExprBlock`). One CST kind for
    /// what FCS splits into six AST variants: at parse time the LHS is just
    /// a `minusExpr`, and FCS's `mkSynAssign` (`SyntaxTreeOps.fs:518`)
    /// *projects* the `(lhs, rhs)` pair onto `LongIdentSet` / `DotSet` /
    /// `Set` / `DotIndexedSet` / `NamedIndexedPropertySet` /
    /// `DotNamedIndexedPropertySet` purely from the LHS shape. We keep the
    /// lossless pre-dispatch form here and replay `mkSynAssign` in the
    /// normaliser. Shape: `ASSIGN_EXPR > [<target-expr>, LARROW_TOK,
    /// <value-expr>]`; the typed-AST accessors
    /// [`crate::syntax::AssignExpr::target`] / [`crate::syntax::AssignExpr::value`]
    /// read the two `Expr` children in source order. Right-associative
    /// (`%right LARROW`, `pars.fsy:343`) and the lowest-precedence operator,
    /// but its LHS binds only a `minusExpr`, so `a + b <- c` is
    /// `a + (b <- c)`.
    ASSIGN_EXPR,

    /// `&` punctuator — the address-of prefix and pattern-conjunction
    /// binder. NOT an infix operator in FCS's grammar (pars.fsy has no
    /// `declExpr AMP declExpr` rule); single `&` only appears as `AMP
    /// minusExpr` (line 5162) and the pattern conjunction (lines
    /// 3650/4000). Phase 3.5 emits this under [`SyntaxKind::ADDRESS_OF_EXPR`];
    /// phase 6.8 emits it as the operand separator of a
    /// [`SyntaxKind::ANDS_PAT`].
    AMP_TOK,

    /// `&&` punctuator — the unmanaged-pointer address-of prefix. FCS's
    /// `AMP_AMP` is *also* the boolean conjunction operator in infix
    /// position (pars.fsy:355 `%left AMP AMP_AMP`), but the prefix form
    /// produces `AddressOf(isByref=false, ...)` rather than an App. The
    /// parser disambiguates by precedence layer: the minusExpr/argExpr
    /// arm of the prefix-form parser takes it as address-of; the Pratt
    /// climber takes it as infix conjunction.
    AMP_AMP_TOK,

    /// `SynModuleDecl.Let(isRec, bindings, range, trivia)` — a top-level
    /// `let x = e` binding (or recursive group `let rec x = e and y = e'`).
    /// Phase 4.2 handles `opt_rec` and `and`-chains; patterns / type
    /// annotations / non-trivial LHS arrive in later slices. Shape:
    /// `LET_DECL > [LET_TOK, REC_TOK?, BINDING, (AND_TOK, BINDING)*]` with
    /// trivia interleaved. The `isRec` flag projects from the presence of
    /// `REC_TOK`; `and`-chains without `rec` are accepted (FCS warns
    /// FS0588 but parses as `isRec = false`).
    LET_DECL,

    /// `SynBinding` — one slot of a `let` / `member` / `do` binding group.
    /// Phase 4.3 adds the optional `INLINE_TOK?` / `MUTABLE_TOK?` modifiers
    /// (FCS's `opt_inline opt_mutable` in `localBinding`); the optional
    /// return-type annotation (`let x : T = …`) lands as a
    /// [`SyntaxKind::BINDING_RETURN_INFO`] child between the head pattern and
    /// `=`. Attributes and non-trivial LHS patterns arrive in later slices.
    /// Shape: `BINDING > [INLINE_TOK?, MUTABLE_TOK?, (NAMED_PAT |
    /// LONG_IDENT_PAT), BINDING_RETURN_INFO?, EQUALS_TOK, <rhs-expr>]` with
    /// trivia interleaved.
    BINDING,

    /// `SynBinding.returnInfo` (`SynBindingReturnInfo`, `SyntaxTree.fsi:1277`)
    /// — the `: T` return-type annotation on a binding head (`let x : T = …`,
    /// `let f a : T = …`). FCS reaches this via `bindingPattern
    /// opt_topReturnTypeWithTypeConstraints EQUALS …` (`pars.fsy:3327`); the
    /// colon binds the type to the *binding*, not the head pattern (which stays
    /// a bare `SynPat.Named`/`SynPat.LongIdent`). Shape:
    /// `BINDING_RETURN_INFO > [COLON_TOK, <type>]`. FCS additionally wraps the
    /// RHS expression in `SynExpr.Typed(rhs, T)` with the same type
    /// (`mkSynBindingRhs`, `SyntaxTreeOps.fs:747`); the normaliser reconstructs
    /// that wrapper from this node, so the differential projection matches
    /// without modelling `returnInfo` separately.
    BINDING_RETURN_INFO,

    /// `SynPat.Named(SynIdent, isThisVal, accessibility, range)` — the simple
    /// `let x = …` LHS pattern wrapping a single identifier. Phase 4.1 only
    /// produces the `isThisVal = false`, no-accessibility form. Shape:
    /// `NAMED_PAT > [IDENT_TOK]` with no surrounding trivia. Also serves as
    /// the per-arg pattern inside a [`SyntaxKind::LONG_IDENT_PAT`] when the
    /// binding has the function form `let f x y = …`.
    ///
    /// A *nullary* active-pattern occurrence (`let (|Foo|Bar|) = …`, a
    /// `match`-clause head) also lands here — FCS's maybe-var collapse turns it
    /// into `SynPat.Named` because its `idText` (`"|Foo|Bar|"`) leads with `|`,
    /// not an uppercase letter. The shape is then `NAMED_PAT >
    /// [ACTIVE_PAT_NAME]` (the single child is the active-pattern name node, not
    /// an `IDENT_TOK`).
    NAMED_PAT,

    /// `SynPat.LongIdent(longDotId, extraId, typars, args, accessibility,
    /// range)` — the function-form binding head, e.g. `let f x y = …`.
    /// Phase 4.4 emits this only when at least one curried arg follows the
    /// head ident; the value form `let x = e` still uses
    /// [`SyntaxKind::NAMED_PAT`] (matching FCS, which post-processes the
    /// zero-arg case to `SynPat.Named`). Args are either
    /// [`SyntaxKind::NAMED_PAT`] (`let f x = …`) or
    /// [`SyntaxKind::WILDCARD_PAT`] (`let f _ = …`); tuple / typed args
    /// arrive later.
    ///
    /// Shape: `LONG_IDENT_PAT > [LONG_IDENT, (NAMED_PAT | WILDCARD_PAT)+]`
    /// with trivia interleaved. The head `LONG_IDENT` carries the function
    /// name; the trailing arg patterns are the curried arguments, in
    /// source order. FCS's `SynArgPats.Pats` slot is implicit in the
    /// children list — there is no separate node for it because rowan's
    /// flat children already capture the same information.
    ///
    /// The named-field union-case form (`Case (field = pat; …)`, FCS's
    /// `SynArgPats.NamePatPairs`) instead carries a single
    /// [`SyntaxKind::NAME_PAT_PAIRS`] child in place of the flat arg list.
    LONG_IDENT_PAT,

    /// The parenthesised name of an active-pattern definition / use —
    /// `(|Foo|Bar|)`, `(|Foo|_|)`. FCS bakes the whole pipe-delimited case
    /// list into the single `idText` of a one-segment `SynLongIdent` (a total
    /// `(|Foo|Bar|)` → `"|Foo|Bar|"`, a partial `(|Foo|_|)` → `"|Foo|_|"`), so
    /// the active-pattern head is just a [`SyntaxKind::LONG_IDENT_PAT`] whose
    /// head segment is this node instead of a plain [`SyntaxKind::LONG_IDENT`].
    /// We keep the constituent tokens for losslessness and reconstruct FCS's
    /// `idText` from the case-name tokens (`|` + names joined by `|` + `|`).
    ///
    /// Shape: `ACTIVE_PAT_NAME > [LPAREN_TOK, BAR_TOK, (IDENT_TOK |
    /// UNDERSCORE_TOK) (BAR_TOK (IDENT_TOK | UNDERSCORE_TOK))* BAR_TOK,
    /// RPAREN_TOK]`, with trivia interleaved. The leading `|` and the `|`
    /// before the closing `)` are both present; the closing `)` is
    /// LexFilter-swallowed (recovered from the raw stream).
    ACTIVE_PAT_NAME,

    /// FCS's `SynArgPats.NamePatPairs(pats, range, trivia)` — the
    /// named-field argument group of a union-case / function-form pattern
    /// (`Case (field = pat; …)`). Sits as the sole argument child of a
    /// [`SyntaxKind::LONG_IDENT_PAT`], replacing the flat
    /// [`SyntaxKind::NAME_PAT_PAIR`]-free `SynArgPats.Pats` arg list. FCS's
    /// `atomicPatsOrNamePatPairs: LPAREN namePatPairs rparen`
    /// (`pars.fsy:3750`); the parens belong to this node (FCS's `ParenRange`
    /// trivia), and the field separator is `;` (`SEMICOLON`/`OBLOCKSEP`),
    /// **not** `,` (a `,` is an FCS parse error here). The closing `)` is
    /// LexFilter-swallowed.
    ///
    /// Shape: `NAME_PAT_PAIRS > [LPAREN_TOK, NAME_PAT_PAIR (SEMI_TOK
    /// NAME_PAT_PAIR)*, RPAREN_TOK]`.
    NAME_PAT_PAIRS,

    /// One `NamePatPairField(longId, eqRange, fieldRange, pat, sepRange)` of a
    /// [`SyntaxKind::NAME_PAT_PAIRS`] group — FCS's `namePatPair: ident EQUALS
    /// parenPattern` (`pars.fsy:3676`). Structurally the union-case sibling of
    /// [`SyntaxKind::RECORD_PAT_FIELD`] (both project to FCS's shared
    /// `NamePatPairField`), but the field name is a single `ident`, not a
    /// `path` — so `Case (M.X = p)` is an FCS parse error, unlike `{ M.X = p }`.
    ///
    /// Shape: `NAME_PAT_PAIR > [IDENT_TOK (field name), EQUALS_TOK, <value
    /// parenPattern>]`.
    NAME_PAT_PAIR,

    /// `SynPat.Wild(range)` — the wildcard pattern `_`. Stands in as a
    /// binding head (`let _ = e` — value form) or as a curried argument
    /// inside a [`SyntaxKind::LONG_IDENT_PAT`] (`let f _ = e`). Shape:
    /// `WILDCARD_PAT > [UNDERSCORE_TOK]`, a single token. FCS does *not*
    /// promote `_` to function-form even with trailing idents (`let _ x =
    /// e` is a parser error, not `LongIdent` with wildcard head); we
    /// mirror that by only branching to function-form on an ident head.
    WILDCARD_PAT,

    /// `SynPat.Paren(pat, range)` — a parenthesised pattern `( pat )`.
    /// FCS keeps this in the AST rather than folding it away
    /// (`SyntaxTree.fsi:1143`), to preserve user-written precedence
    /// information for tooling. Shape: `PAREN_PAT > [LPAREN_TOK, <inner-pat>,
    /// RPAREN_TOK]` with trivia interleaved. The unit-literal form `()`
    /// goes to [`SyntaxKind::CONST_PAT`] instead — `parse_atomic_pat`
    /// disambiguates by peeking past the opening paren.
    PAREN_PAT,

    /// `SynPat.Const(constant, range)` — a constant-literal pattern.
    /// Covers numeric/string/char/bool literals (the same set
    /// `parse_const_expr` accepts on the expression side) and the
    /// unit-literal form `()`. Shape:
    /// `CONST_PAT > [<literal-token>]` for the literal variants and
    /// `CONST_PAT > [LPAREN_TOK, RPAREN_TOK]` for unit — mirroring
    /// [`SyntaxKind::CONST_EXPR`]'s shape. Trivia inside `()` lands on
    /// the surrounding `CONST_PAT` between the two punctuator tokens.
    CONST_PAT,

    /// `SynPat.Null(range)` — the `null` pattern (`let null = …`,
    /// `let f null = …`). Shape: `NULL_PAT > [NULL_TOK]`. FCS rejects
    /// this as a value-form binding head at the typechecker level but
    /// parses it; we mirror the parse and the typecheck error is out of
    /// scope here.
    NULL_PAT,

    /// `SynPat.Typed(pat, targetType, range)` — a type-annotated pattern
    /// `pat : type`. FCS only reaches this through `parenPattern COLON
    /// typeWithTypeConstraints` (`pars.fsy:3929`), so the construct is
    /// only legal *inside* `parenPattern` — i.e. always wrapped by an
    /// outer `SynPat.Paren`. Phase 6.2 emits this only from inside
    /// [`SyntaxKind::PAREN_PAT`] (`let (x : int) = …`,
    /// `let f (x : int) = …`); bare-binding-head typed forms
    /// (`let x : int = …`) route through `SynBinding.returnInfo` and
    /// live on a later phase.
    ///
    /// Shape: `TYPED_PAT > [<inner-pat>, COLON_TOK, <type>]` with
    /// trivia interleaved. Mirrors [`SyntaxKind::TYPED_EXPR`] one-for-one
    /// on the pattern side.
    TYPED_PAT,

    /// `SynPat.Tuple(isStruct, elementPats, commaRanges, range)` — a
    /// tuple pattern `p1, p2, …, pn` (`SyntaxTree.fsi`, `pars.fsy`
    /// `headBindingPat`). FCS reaches this from `headBindingPat` (at
    /// the top of a let-binding head) and from `parenPattern` (inside
    /// `( p1, p2 )`); the parenthesised form is `SynPat.Paren(Tuple(…))`.
    /// Phase 6.3 emits the non-struct variant only — `struct (x, y)`
    /// tuple patterns are deferred.
    ///
    /// We keep the green-tree shape flat — one `COMMA_TOK` between
    /// each `pat` — paralleling [`SyntaxKind::TUPLE_TYPE`]'s flat
    /// `STAR_TOK` representation. FCS's `SynPat.Tuple` is itself a
    /// flat list, not nested pairs, so this projects one-for-one.
    /// Shape: `TUPLE_PAT > [<pat>, COMMA_TOK, <pat>, (COMMA_TOK, <pat>)*]`
    /// with trivia interleaved.
    TUPLE_PAT,

    /// `SynPat.As(lhsPat, rhsPat, range)` — an `as`-pattern `p1 as p2`.
    /// FCS reaches this through `headBindingPattern AS constrPattern`
    /// (`pars.fsy:3570`, top-level `let` heads) and `parenPattern AS
    /// constrPattern` (`pars.fsy:3902`, inside parens). `%right AS`
    /// (`pars.fsy:248`) makes `as` the *lowest* pattern precedence, so it
    /// binds the whole comma-list to its left (`x, y as z` ⇒
    /// `As(Tuple[x,y], z)`) and chains left-nested (`x as y as z` ⇒
    /// `As(As(x,y),z)`). The right operand is `constrPattern` (applPat
    /// level): atomic or function-form `Ctor args`, never a tuple, typed
    /// pat, or nested `as`.
    ///
    /// Shape: `AS_PAT > [<lhs-pat>, AS_TOK, <rhs-pat>]` with trivia
    /// interleaved.
    AS_PAT,

    /// `SynPat.ArrayOrList(isArray, elementPats, range)` — a list `[ … ]`
    /// or array `[| … |]` pattern (`SyntaxTree.fsi:1146`). FCS reaches it
    /// from `atomicPattern` (`pars.fsy:3786-3790`):
    /// - `LBRACK listPatternElements RBRACK` → `ArrayOrList(false, …)`;
    /// - `LBRACK_BAR listPatternElements BAR_RBRACK` → `ArrayOrList(true, …)`.
    ///   `listPatternElements` (`pars.fsy:4035-4043`) is zero-or-more
    ///   `parenPattern` separated by `;`/`OBLOCKSEP` (either order, trailing
    ///   sep tolerated). Empty is valid — `[]` / `[||]` are legal patterns.
    ///
    /// Atomic-level: each *element* is a full `parenPattern`, so inside an
    /// element `,` builds a tuple and per-element `:` / `as` apply (`[a, b]`
    /// is a one-element list whose element is a tuple — `;`, not `,`, is the
    /// list separator).
    ///
    /// Shapes (trivia interleaved):
    /// - list: `ARRAY_OR_LIST_PAT > [LBRACK_TOK, (<elem> (SEMI_TOK <elem>)*)? RBRACK_TOK]`
    /// - array: `ARRAY_OR_LIST_PAT > [LBRACK_BAR_TOK, …, BAR_RBRACK_TOK]`
    ///
    /// `isArray` is recovered from the delimiter token: a `LBRACK_BAR_TOK`
    /// opener ⇒ array.
    ARRAY_OR_LIST_PAT,

    /// `SynPat.Record(fieldPats: NamePatPairField list, range)` — a record
    /// pattern `{ X = p; Y = q }`. FCS's `atomicPattern: LBRACE
    /// recordPatternElementsAux rbrace` (`pars.fsy:3780`); each field is a
    /// `recordPatternElement: path EQUALS parenPattern` (`pars.fsy:4023`).
    /// Atomic-level (slots into `try_emit_atomic_pat`), so it works as a
    /// let-head, function-form curried arg, and `fun` lambda arg.
    ///
    /// Shape: `RECORD_PAT > [LBRACE_TOK, (RECORD_PAT_FIELD (SEMI_TOK
    /// RECORD_PAT_FIELD)*)?, RBRACE_TOK]`. The field separator is `;`
    /// (`SEMICOLON`/`OBLOCKSEP`), **not** `,` — a `,` builds a tuple inside
    /// one field's value (`{ X = a, b }` is one field whose value is
    /// `Tuple[a, b]`). The closing `}` is LexFilter-swallowed.
    RECORD_PAT,

    /// One `NamePatPairField(longId, eqRange, fieldRange, pat, sepRange)` of
    /// a [`SyntaxKind::RECORD_PAT`]. Shape: `RECORD_PAT_FIELD > [LONG_IDENT
    /// (field name `path`), EQUALS_TOK, <value parenPattern>]`. The field
    /// name is a `path` (`SynLongIdent`), so `{ M.X = p }` is qualified.
    RECORD_PAT_FIELD,

    /// `SynPat.IsInst(pat: SynType, range)` — the dynamic type-test pattern
    /// `:? T`. FCS's `constrPattern: COLON_QMARK atomTypeOrAnonRecdType`
    /// (`pars.fsy:3729`); it sits one level above the atomic patterns (it's a
    /// `constrPattern`, not an `atomicPattern`), so the parser emits it from
    /// the shared head-binding entry (`try_emit_head_binding_pat_element`),
    /// which reaches match clauses, `let` heads, and parenthesised
    /// (`fun`-arg) elements. The tested type is the `atomTypeOrAnonRecdType`
    /// level — an atomic type (incl. the `Foo<…>` prefix-app) or an anonymous
    /// record type — *not* the full type grammar (`->`/`*`/postfix-app
    /// terminate it).
    ///
    /// Shape: `IS_INST_PAT > [COLON_QMARK_TOK, <type>]`. A `:?` with no
    /// following type is a parse error (mirroring FCS's `COLON_QMARK recover`
    /// arm), leaving the node with no type child.
    IS_INST_PAT,

    /// `SynPat.ListCons(lhsPat, rhsPat, range, trivia)` — the cons pattern
    /// `h :: t`. FCS's `parenPattern COLON_COLON parenPattern`
    /// (`pars.fsy:3944`), `%right COLON_COLON` (`:361`). `::` is the *tightest*
    /// infix pattern operator and right-associative, so `a :: b :: c` is
    /// `ListCons(a, ListCons(b, c))` and `a :: b, c` is `Tuple[ListCons(a,b),
    /// c]`. Emitted by the precedence-climbing pattern tail (`wrap_pat_tail` /
    /// `climb_pat_tail` in the parser).
    ///
    /// Shape: `LIST_CONS_PAT > [<lhs-pat>, COLON_COLON_TOK, <rhs-pat>]`.
    LIST_CONS_PAT,

    /// `SynPat.Ands(pats, range)` — the conjunction pattern `a & b & c`. FCS's
    /// `conjPatternElements` / `conjParenPatternElements`
    /// (`pars.fsy:3649`/`:4000`), `%left AMP` (`:355`). N-ary and flat (a single
    /// `Ands` holds all operands, not nested pairs), tighter than `,`/`:`/`as`
    /// and looser than `::`, so `a & b :: c` is `Ands[a, ListCons(b,c)]` and
    /// `a & b, c` is `Tuple[Ands[a,b], c]`. Emitted by the precedence-climbing
    /// pattern tail (`wrap_pat_tail` / `climb_pat_tail` in the parser).
    ///
    /// Shape: `ANDS_PAT > [<pat>, (AMP_TOK <pat>)+]`.
    ANDS_PAT,

    /// `SynPat.Or(lhsPat, rhsPat, range, trivia)` — the or-pattern `A | B`.
    /// FCS's `headBindingPattern barCanBeRightBeforeNull headBindingPattern` /
    /// `parenPattern barCanBeRightBeforeNull parenPattern`
    /// (`pars.fsy:3584`/`:3916`), `%left BAR` (`:266`). The *loosest* infix
    /// pattern operator, left-associative, so `A | B | C` is `Or(Or(A,B), C)`
    /// and `A, B | C` is `Or(Tuple[A,B], C)`. Emitted by the precedence-climbing
    /// pattern tail (`wrap_pat_tail` / `climb_pat_tail` in the parser); a `|`
    /// *after* a `match` clause's `-> result` is the clause separator instead
    /// (owned by `parse_match_clauses`), distinguished purely by the `->`
    /// boundary.
    ///
    /// Shape: `OR_PAT > [<lhs-pat>, BAR_TOK, <rhs-pat>]`.
    OR_PAT,

    /// `SynPat.Attrib(pat, attributes, range)` — an attributed pattern
    /// `[<Foo>] p`. FCS's `attributes parenPattern` (`pars.fsy:3940`),
    /// `%prec paren_pat_attribs`. Reachable only at the `parenPattern` level
    /// (inside parens, list/array elements), not at a bare binding head; the
    /// attribute list(s) reuse the phase-10.5 [`SyntaxKind::ATTRIBUTE_LIST`]
    /// primitive. The attrib prefix binds tighter than `,`/`as`/`|` (which
    /// wrap it from outside) but looser than `:`/`&`/`::` (absorbed into the
    /// inner pattern) — verified against FCS: `([<A>] x : int)` is
    /// `Attrib(Typed …)` and `([<A>] h :: t)` is `Attrib(ListCons …)`, while
    /// `([<A>] x, y)` is `Tuple[Attrib x, y]`. `SimplePatOfPat` recurses
    /// through `Attrib` like `Typed` (`SyntaxTreeOps.fs:315`), so a `fun`
    /// arg's body-lowering decision is taken by the inner pattern.
    ///
    /// Shape: `ATTRIB_PAT > [ATTRIBUTE_LIST+, <inner-pat>]`.
    ATTRIB_PAT,

    /// `SynPat.OptionalVal(ident: Ident, range)` — the optional-argument
    /// pattern `?ident`. FCS's `atomicPattern: QMARK ident` (`pars.fsy:3802`);
    /// it sits at the atomic-pattern level (alongside [`SyntaxKind::NAMED_PAT`]
    /// / [`SyntaxKind::WILDCARD_PAT`]), so the parser emits it from
    /// [`SyntaxKind::OPTIONAL_VAL_PAT`]'s atomic dispatcher, reaching every
    /// curried-arg / parenthesised-element site. Optional arguments are only
    /// *semantically* valid on type members, but that restriction is enforced
    /// after parsing, so the `ParsedInput` carries the pattern unconditionally.
    /// The named ident strips its backticks in FCS's `Ident.idText`, matching
    /// the [`SyntaxKind::IDENT_TOK`] text projection.
    ///
    /// Shape: `OPTIONAL_VAL_PAT > [QMARK_TOK, IDENT_TOK]`. A `?` with no
    /// following ident is a parse error (FCS has no `QMARK`-only production),
    /// leaving the node with no ident child.
    OPTIONAL_VAL_PAT,

    /// `SynPat.QuoteExpr(expr: SynExpr, range)` (`SyntaxTree.fsi:1161`) — a code
    /// quotation `<@ … @>` in *pattern* position, FCS's `atomicPattern:
    /// quoteExpr` (`pars.fsy:3776`). The only way a quotation reaches a pattern
    /// is as the *parameter* of a parameterised active pattern (`match e with |
    /// SpecificCall <@ f @> (args) -> …`): the quote is passed to the
    /// active-pattern function and the following pattern matches its output.
    /// Emitted from the atomic-pattern dispatcher, so it reaches every
    /// curried-arg / clause-head / parenthesised-element site.
    ///
    /// Shape: `QUOTE_PAT > [QUOTE_EXPR]`. The inner `expr` is a full
    /// `SynExpr.Quote`, so the node simply wraps the [`SyntaxKind::QUOTE_EXPR`]
    /// the shared quotation parser emits; [`crate::syntax::QuotePat::inner`]
    /// reads it back as the quoted [`Expr`](crate::syntax::Expr).
    QUOTE_PAT,

    /// `SynExpr.IfThenElse` — an `if c then e1 else e2` expression. FCS's
    /// shape is `IfThenElse of ifExpr * thenExpr * elseExpr option *
    /// spIfToThen * isFromErrorRecovery * range * trivia`
    /// (`SyntaxTree.fsi:790`). Phase 5.1 only produces the basic
    /// three-part form: `if`, condition, `then`, then-branch, `else`,
    /// else-branch. `elif` chains, the no-else form, and the
    /// `IsElif`/`ElseKeyword option` trivia details arrive in later
    /// slices.
    ///
    /// Shape: `IF_THEN_ELSE_EXPR > [IF_TOK, <cond-expr>, THEN_TOK,
    /// <then-expr>, ELSE_TOK, <else-expr>]` with trivia (including the
    /// `Virtual::BlockBegin` / `Virtual::BlockEnd` LexFilter scaffolding,
    /// emitted as zero-width ERROR tokens) interleaved.
    IF_THEN_ELSE_EXPR,

    /// `SynExpr.Lambda` — a `fun`-introduced lambda
    /// (`SyntaxTree.fsi:651`). FCS's representation curries the
    /// arguments: `fun x y -> body` projects as nested `Lambda`s with
    /// `Lambda(_, _, simplePats, Lambda(_, _, simplePats, body))`,
    /// plus a `parsedData = Some(args, body)` cache on the outermost
    /// node that lists the original arg patterns flat alongside the
    /// real body. We keep the green tree *flat*: a single `FUN_EXPR`
    /// holds the leading `FUN_TOK`, then one or more parameter
    /// patterns, then `RARROW_TOK`, then the body expression — the
    /// projector reconstructs the curried `Lambda` chain when needed.
    /// Phase 5.2 covers single-arrow, no-`function`-sugar forms;
    /// `match`-introduced lambdas (`function | _ -> …`) are a separate
    /// kind.
    ///
    /// Shape: `FUN_EXPR > [FUN_TOK, <pat>, (<pat>)*, RARROW_TOK,
    /// <body-expr>]` with trivia (including the `Virtual::BlockBegin`
    /// / `Virtual::BlockEnd` LexFilter scaffolding around the body,
    /// emitted as zero-width ERROR tokens) interleaved.
    FUN_EXPR,

    /// `SynExpr.Match` — `match scrut with pat -> e | …`
    /// (`SyntaxTree.fsi:728`, `pars.fsy:4221`). The
    /// `matchDebugPoint`, `range`, and `trivia` (`MatchKeyword` /
    /// `WithKeyword`) slots are structural-only; the projector elides
    /// them. Phases 5.M.1–5.M.3 cover multiple `|`-separated clauses (with
    /// an optional leading `|`) and `when` guards; the `function`
    /// (MatchLambda) sugar is [`SyntaxKind::MATCH_LAMBDA_EXPR`].
    ///
    /// Shape: `MATCH_EXPR > [MATCH_TOK, <scrutinee-expr>, WITH_TOK,
    /// <MATCH_CLAUSE>+]` with trivia (including the trailing
    /// `Virtual::RightBlockEnd` / `Virtual::End` LexFilter scaffolding,
    /// emitted as zero-width ERROR tokens) interleaved. The scrutinee is
    /// the sole direct `Expr` child; clause results live nested inside
    /// their [`SyntaxKind::MATCH_CLAUSE`].
    MATCH_EXPR,

    /// `SynMatchClause` — one `pat [when guard] -> result` arm of a
    /// [`SyntaxKind::MATCH_EXPR`] (`SyntaxTree.fsi:1189`,
    /// `pars.fsy:4958 patternClauses`). The `range`, `debugPoint`, and
    /// `trivia` (`ArrowRange` / `BarRange`) slots are structural-only.
    ///
    /// Shape: `MATCH_CLAUSE > [BAR_TOK?, <pat>, (WHEN_TOK <guard-expr>)?,
    /// RARROW_TOK, <result-expr>]`. The leading `BAR_TOK` is the optional
    /// `|` separator (Phase 5.M.2); the `WHEN_TOK`+guard is the optional
    /// `when` guard (Phase 5.M.3). When a guard is present the clause has
    /// two `Expr` children — the guard precedes `RARROW_TOK`, the result
    /// follows it — so the facade disambiguates positionally.
    MATCH_CLAUSE,

    /// `SynExpr.MatchLambda` — `function pat -> e | …`
    /// (`SyntaxTree.fsi`, `pars.fsy`). The `function` sugar for an
    /// anonymous single-argument `match`; FCS keeps it as a *distinct*
    /// parsed node (the `fun _argN -> match _argN with …` synthesis
    /// happens later, in typechecking — not in `ParsedInput`), so we
    /// mirror it as its own kind rather than desugaring. The
    /// `isExnMatch`, `keywordRange`, `matchDebugPoint`, and `range` slots
    /// are structural-only; the projector elides them.
    ///
    /// Shape: `MATCH_LAMBDA_EXPR > [FUNCTION_TOK, <MATCH_CLAUSE>+]` with
    /// trivia (including the trailing `Virtual::RightBlockEnd` /
    /// `Virtual::End` LexFilter scaffolding, emitted as zero-width ERROR
    /// tokens) interleaved. There is no scrutinee; the clause list reuses
    /// [`SyntaxKind::MATCH_CLAUSE`] verbatim.
    MATCH_LAMBDA_EXPR,

    /// `SynExpr.MatchBang` — `match! e with …`, the computation-expression
    /// match binder (`SyntaxTree.fsi:916`, `pars.fsy:4233`). Field-for-field
    /// identical to [`SyntaxKind::MATCH_EXPR`] apart from the keyword
    /// ([`SyntaxKind::MATCH_BANG_TOK`]) and the case name; the
    /// `matchDebugPoint`, `range`, and `trivia` (`MatchBangKeyword` /
    /// `WithKeyword`) slots are structural-only and elided by the projector.
    /// FCS parses `match!` at any expression position, not only inside a CE.
    ///
    /// Shape: `MATCH_BANG_EXPR > [MATCH_BANG_TOK, <scrutinee-expr>, WITH_TOK,
    /// <MATCH_CLAUSE>+]` with trivia (including the trailing
    /// `Virtual::RightBlockEnd` / `Virtual::End` LexFilter scaffolding,
    /// emitted as zero-width ERROR tokens) interleaved. The scrutinee is the
    /// sole direct `Expr` child; clause results live nested inside their
    /// [`SyntaxKind::MATCH_CLAUSE`]. The clause list reuses
    /// [`SyntaxKind::MATCH_CLAUSE`] verbatim.
    MATCH_BANG_EXPR,

    /// `SynExpr.While` — `while cond do body` (`SyntaxTree.fsi:656`,
    /// `pars.fsy:4367`). The `whileDebugPoint` and `range` slots are
    /// structural-only and elided by the projector.
    ///
    /// Shape: `WHILE_EXPR > [WHILE_TOK, <cond-expr>, DO_TOK, ERROR(BlockBegin),
    /// <body-expr>, ERROR(BlockEnd), ERROR(DeclEnd)]` with trivia interleaved.
    /// The `do` body is a SeqBlock; its `BlockBegin`/`BlockEnd`/`DeclEnd`
    /// LexFilter scaffolding is consumed as zero-width/`ERROR` leaves, exactly
    /// as [`SyntaxKind::DO_BANG_EXPR`] does (via `parse_if_body`), and a
    /// multi-statement body wraps in a [`SyntaxKind::SEQUENTIAL_EXPR`]. The
    /// condition is the leading `Expr` child, the body the trailing one.
    WHILE_EXPR,

    /// `SynExpr.WhileBang` — `while! cond do body` (`SyntaxTree.fsi:928`), the
    /// computation-expression while binder. Identical fields/shape to
    /// [`SyntaxKind::WHILE_EXPR`] apart from the keyword
    /// ([`SyntaxKind::WHILE_BANG_TOK`]) and the case name; same `DO_TOK` /
    /// SeqBlock-body / optional `done`-terminator handling (the parser routes
    /// both through `parse_while_loop`). FCS parses `while!` at any expression
    /// position, not only inside a CE.
    WHILE_BANG_EXPR,

    /// `SynExpr.ForEach` — `for pat in enumExpr do body` (`SyntaxTree.fsi:671`,
    /// `pars.fsy:4372`). The `forDebugPoint`, `inDebugPoint`, `seqExprOnly`,
    /// `isFromSource`, and `range` slots are structural-only and elided by the
    /// projector.
    ///
    /// Two body forms share this node:
    ///
    /// * **`do` form** — `FOR_EACH_EXPR > [FOR_TOK, <pat>, IN_TOK, <enum-expr>,
    ///   DO_TOK, ERROR(BlockBegin), <body-expr>, ERROR(BlockEnd),
    ///   ERROR(DeclEnd)]`. The `do` body reuses the SeqBlock scaffolding of
    ///   [`SyntaxKind::WHILE_EXPR`] (consumed via `parse_if_body` +
    ///   `consume_block_decl_end`, incl. the optional `done` terminator).
    /// * **`->` comprehension form** (`for pat in e -> body`, `pars.fsy:4412`) —
    ///   `FOR_EACH_EXPR > [FOR_TOK, <pat>, IN_TOK, <enum-expr>,
    ///   YIELD_OR_RETURN_EXPR > [RARROW_TOK, <body-expr>], ERROR(RightBlockEnd)]`.
    ///   FCS desugars the arrow to `SynExpr.YieldOrReturn((true, false), body)`
    ///   (an implicit `yield`) and sets `seqExprOnly = true`; once `seqExprOnly`
    ///   is elided, the yield-wrapped body is what distinguishes the two forms.
    ///
    /// In both, the binder pattern is a `parenPattern`, the `in` a raw
    /// `Token::In` left in the filtered stream (see [`SyntaxKind::IN_TOK`]), the
    /// enumerable collection the leading `Expr` child, and the body the trailing
    /// one.
    FOR_EACH_EXPR,

    /// `SynExpr.For` — `for ident = identBody to/downto toBody do doBody`
    /// (`SyntaxTree.fsi:659`, `pars.fsy:4418`). The `forDebugPoint`,
    /// `toDebugPoint`, `equalsRange`, and `range` slots are structural-only and
    /// elided by the projector; `direction` is recovered from whether the loop
    /// carries [`SyntaxKind::TO_TOK`] (ascending) or [`SyntaxKind::DOWNTO_TOK`]
    /// (descending).
    ///
    /// Shape: `FOR_EXPR > [FOR_TOK, IDENT_TOK, EQUALS_TOK, <from-expr>,
    /// TO_TOK|DOWNTO_TOK, <to-expr>, DO_TOK, ERROR(BlockBegin), <body-expr>,
    /// ERROR(BlockEnd), ERROR(DeclEnd)]` with trivia interleaved. FCS's
    /// `forLoopRange` parses a `parenPattern` then extracts an `Ident` via
    /// `idOfPat`; for valid input that pattern is always a bare ident, so the
    /// loop variable is captured directly as `IDENT_TOK`. The three `Expr`
    /// children are, in order, the start bound (`identBody`), the end bound
    /// (`toBody`), and the body (`doBody`); the `do` body reuses
    /// [`SyntaxKind::FOR_EACH_EXPR`]'s SeqBlock scaffolding.
    FOR_EXPR,

    /// `SynExpr.TryWith` — `try body with <clauses>` (`SyntaxTree.fsi:759`,
    /// `pars.fsy:4245`) — and `SynExpr.TryFinally` — `try body finally cleanup`
    /// (`SyntaxTree.fsi:768`, `pars.fsy:4313`, phase 10.20b). One green node
    /// covers both forms; the trailing [`SyntaxKind::WITH_TOK`] +
    /// [`SyntaxKind::MATCH_CLAUSE`] list (TryWith) versus
    /// [`SyntaxKind::FINALLY_TOK`] + a finally-body `Expr` (TryFinally)
    /// discriminates them, mirroring how [`SyntaxKind::FOR_EXPR`] recovers
    /// `direction` from `TO_TOK`/`DOWNTO_TOK`. The `tryDebugPoint`,
    /// `withDebugPoint`/`finallyDebugPoint`, `range`, and trivia
    /// (`TryKeyword` / `TryToWithRange` / `WithKeyword` / `WithToEndRange` /
    /// `FinallyKeyword`) slots are structural-only and elided by the projector.
    ///
    /// Shape (TryWith): `TRY_EXPR > [TRY_TOK, <body-expr>, ERROR(RightBlockEnd),
    /// WITH_TOK, <MATCH_CLAUSE>+]` with trivia interleaved (the clause list
    /// carries the trailing `Virtual::RightBlockEnd` / `Virtual::End`
    /// scaffolding as zero-width ERROR tokens, exactly as
    /// [`SyntaxKind::MATCH_EXPR`]). The body is a one-sided SeqBlock parsed by
    /// `parse_seq_block_body` (so a multi-statement body wraps in a
    /// [`SyntaxKind::SEQUENTIAL_EXPR`]); the clause list reuses
    /// [`SyntaxKind::MATCH_CLAUSE`] verbatim. The body is the *leading* `Expr`
    /// child; clause results live nested inside their `MATCH_CLAUSE`.
    ///
    /// Shape (TryFinally): `TRY_EXPR > [TRY_TOK, <body-expr>,
    /// ERROR(RightBlockEnd), FINALLY_TOK, ERROR(BlockBegin), <finally-expr>,
    /// ERROR(BlockEnd?), ERROR(DeclEnd)]` with trivia interleaved. The finally
    /// body is a *regular* SeqBlock (the `while`/`for` `do`-body shape) parsed
    /// via `parse_block_body_after_keyword`. Both the try body and the finally
    /// body are direct `Expr` children — the body is the *leading* one, the
    /// finally body the *trailing* one (there is no `MATCH_CLAUSE`).
    TRY_EXPR,

    /// `SynExpr.Sequential` — a sequence of expressions evaluated in
    /// order, with the last one's value being the sequence's value
    /// (`SyntaxTree.fsi:704`). FCS shape:
    /// `Sequential of debugPoint * isTrueSeq * expr1 * expr2 * range *
    /// trivia`; the binary FCS shape is right-leaning (`Sequential(_, _,
    /// e1, Sequential(_, _, e2, e3, …), …)`), but we keep a flat
    /// n-ary green-tree shape because the offside `Virtual::BlockSep`
    /// scaffolding gives us exactly the boundaries between statements
    /// in one pass — re-nesting them as binary pairs would be lossy
    /// without adding trivia anchors. The projector reconstructs the
    /// binary FCS form when needed.
    ///
    /// Produced by every one-sided SeqBlock body via the shared
    /// `Parser::parse_seq_block_body` gatherer: `if`/`then`/`else`
    /// branches, `fun` and `match`-clause bodies, `let!`/`use!`
    /// computation-expression bodies, and the `let`/function-binding RHS.
    ///
    /// Shape: `SEQUENTIAL_EXPR > [<expr>, <sep>, <expr>, …]` — two-or-more
    /// `Expr` children separated by either a zero-width ERROR placeholder
    /// (an offside `Virtual::BlockSep`; raw newlines remain in trivia, owned
    /// by the prior expr) or a [`SyntaxKind::SEMI_TOK`] (an explicit `;`).
    /// The separators are tokens, so [`crate::syntax::SequentialExpr::statements`]
    /// filters them out when projecting to the FCS statement list.
    SEQUENTIAL_EXPR,

    /// `SynExpr.Quote(operator, isRaw, quotedExpr, isFromQueryExpression,
    /// range)` — a code quotation `<@ e @>` / `<@@ e @@>`
    /// (`SyntaxTree.fsi:603`, grammar `quoteExpr` `pars.fsy:5433`). Shape:
    /// `QUOTE_EXPR > [LQUOTE_TOK, <inner-expr>, RQUOTE_TOK]` with trivia
    /// interleaved. FCS's `operator` field is a synthetic `SynExpr.Ident`
    /// (`op_Quotation` / `op_QuotationRaw`) and `isFromQueryExpression` is
    /// always `false` at parse — both are parse-invariant noise elided by
    /// the normaliser; only `isRaw` (recovered from the `LQUOTE_TOK` text)
    /// and the inner expression carry syntactic information.
    QUOTE_EXPR,

    /// `SynExpr.LibraryOnlyILAssembly(ilCode, typeArgs, args, retTy, range)` —
    /// FSharp.Core's inline-IL expression body `# "instr" type (T) arg₀ … : retTy #`
    /// (`pars.fsy:5640 inlineAssemblyExpr`). Shape:
    /// `INLINE_IL_EXPR > [HASH_TOK, <il-string-lit>,
    /// (TYPE_TOK LPAREN_TOK <type> RPAREN_TOK)?, <arg-expr>*,
    /// (COLON_TOK (<type> | LPAREN_TOK RPAREN_TOK))?, HASH_TOK]`
    /// with trivia interleaved. FCS reaches inline IL only via
    /// `parenExpr: LPAREN parenExprBody rparen`, so this node is always the
    /// inner child of a [`SyntaxKind::PAREN_EXPR`] that owns the surrounding
    /// `(`/`)` — the shape is `Paren(LibraryOnlyILAssembly)`, mirroring
    /// `(e : T)` → `Paren(Typed)`. The IL instruction string is a bare literal
    /// token (FCS parses it with `ParseAssemblyCodeInstructions`, not as a
    /// `SynExpr`), so it is *not* wrapped in a `CONST_EXPR`. The `type (…)`
    /// keyword, both inner `)`s (the type-arg paren and the `: ()` unit return),
    /// and the outer closing `)` are LexFilter-swallowed and recovered from the
    /// raw stream; the value arguments are `argExpr`s, the type-arg and return
    /// types are `atomType`/`typ`.
    INLINE_IL_EXPR,

    /// `SynExpr.TraitCall(supportTys, traitSig, argExpr, range)` — a
    /// statically-resolved-type-parameter (SRTP) trait call
    /// `( ^a : (static member M : ^a -> int) x )` (`pars.fsy:5529`,
    /// `parenExprBody: typars COLON LPAREN classMemberSpfn rparen
    /// typedSequentialExpr`). Shape:
    /// `TRAIT_CALL_EXPR > [VAR_TYPE, COLON_TOK, LPAREN_TOK, MEMBER_SIG,
    /// RPAREN_TOK, <arg-expr>]` with trivia interleaved. Like inline IL, FCS
    /// reaches a trait call only via `parenExpr: LPAREN parenExprBody rparen`,
    /// so this node is always the inner child of a [`SyntaxKind::PAREN_EXPR`]
    /// that owns the surrounding `(`/`)` — the shape is `Paren(TraitCall)`. The
    /// support type is the head-type typar `^a` (a [`SyntaxKind::VAR_TYPE`]);
    /// FCS rejects the plain `'a` form here, so only the `^` sigil is parsed.
    /// The member signature reuses the shared [`SyntaxKind::MEMBER_SIG`]
    /// (`classMemberSpfn`, also the SRTP-constraint payload). Both the member
    /// signature's closing `)` and the outer paren's `)` are LexFilter-swallowed
    /// and recovered from the raw stream; the argument is a `typedSequentialExpr`.
    TRAIT_CALL_EXPR,

    /// `SynExpr.LibraryOnlyUnionCaseFieldGet(expr, longId, fieldNum, range)` —
    /// FSharp.Core's cons-cell field read `expr.( :: ).<int>` (`pars.fsy:5351`,
    /// the `LPAREN COLON_COLON rparen DOT INT32` dot-qualification). Shape:
    /// `LIBRARY_ONLY_FIELD_GET_EXPR > [<object-expr>, DOT_TOK, LPAREN_TOK,
    /// COLON_COLON_TOK, RPAREN_TOK, DOT_TOK, INT32_LIT]`. The union-case name is
    /// always the cons operator (`op_ColonColon`, hardcoded by the grammar); the
    /// field number is the `INT32_LIT`. The closing `)` is LexFilter-swallowed (a
    /// paren closer) and recovered. The *set* form `… <- rhs` is an ordinary
    /// [`SyntaxKind::ASSIGN_EXPR`] over this get (FCS's `mkSynAssign` collapses it
    /// to `LibraryOnlyUnionCaseFieldSet`; the normaliser reproduces that). FCS
    /// flags the construct library-only (a parse error outside fslib) but builds
    /// the node; we read it without erroring, to serve real FSharp.Core source.
    LIBRARY_ONLY_FIELD_GET_EXPR,

    /// `SynExpr.LibraryOnlyStaticOptimization(constraints, expr, optimizedExpr,
    /// range)` — FSharp.Core's static-optimization binding RHS,
    /// `mainExpr when 'T : ty = branch …` (`pars.fsy:3391`
    /// `typedExprWithStaticOptimizations`). Shape:
    /// `STATIC_OPTIMIZATION_EXPR > [<main-expr>, STATIC_OPT_WHEN_CLAUSE+]` with
    /// the inter-element offside `BlockSep` virtuals interleaved as zero-width
    /// `ERROR`s. The whole node is the binding's RHS expression; the `main-expr`
    /// is FCS's `typedSequentialExpr` fallthrough, each clause a
    /// `(constraints, branchExpr)` static optimization. FCS folds the clauses
    /// *right* into nested `LibraryOnlyStaticOptimization`
    /// (`SyntaxTreeOps.mkSynBindingRhs`): `m when C1 = e1 when C2 = e2` is
    /// `LOSO(C1, e1, LOSO(C2, e2, m))`; the normaliser reproduces that nesting.
    STATIC_OPTIMIZATION_EXPR,

    /// One `when <conditions> = <branch>` clause of a
    /// [`SyntaxKind::STATIC_OPTIMIZATION_EXPR`] (`pars.fsy:3402`
    /// `staticOptimization`). Shape `[WHEN_TOK, STATIC_OPT_CONDITION,
    /// (AND_TOK STATIC_OPT_CONDITION)*, EQUALS_TOK, <branch-expr>]`. The branch is
    /// a `typedSequentialExprBlock`; the conditions are the clause's
    /// `SynStaticOptimizationConstraint` list (`and`-chained).
    STATIC_OPT_WHEN_CLAUSE,

    /// One `SynStaticOptimizationConstraint` (`SyntaxTree.fsi:1048`,
    /// `pars.fsy:3413` `staticOptimizationCondition`). Two shapes:
    /// * `[TYPAR_DECL, COLON_TOK, <type>]` — `'T : ty`
    ///   (`WhenTyparTyconEqualsTycon(typar, rhsType)`);
    /// * `[TYPAR_DECL, STRUCT_TOK]` — the bare `'T struct`
    ///   (`WhenTyparIsStruct(typar)`).
    ///
    /// The subject typar reuses the [`SyntaxKind::TYPAR_DECL`] node (`'a`/`^a`).
    STATIC_OPT_CONDITION,

    /// `SynExpr.ComputationExpr(hasSeqBuilder, expr, range)` — the body of
    /// a computation-expression brace `{ … }` (`SyntaxTree.fsi:702`,
    /// grammar `computationExpr` `pars.fsy:5604`). Shape:
    /// `COMPUTATION_EXPR > [LBRACE_TOK, <inner-expr>, RBRACE_TOK]` with
    /// trivia interleaved. With a builder ident (`seq { … }`) the enclosing
    /// `App(Ident, ComputationExpr)` falls out of the normal application
    /// juxtaposition; a bare `{ … }` is a `COMPUTATION_EXPR` on its own.
    /// `hasSeqBuilder` is always `false` at parse and is elided by the
    /// normaliser.
    ///
    /// Brace overloading: FCS shares `{ … }` across record expressions,
    /// object expressions, and computation expressions
    /// (`braceExprBody`, `pars.fsy:5580`). The brace parser disambiguates a
    /// leading longident followed by `=`/`with` as a [`SyntaxKind::RECORD_EXPR`];
    /// object expressions (`{ new T … }`) are still deferred (they need member
    /// syntax), so every *other* `{ … }` parses as a `COMPUTATION_EXPR`.
    COMPUTATION_EXPR,

    /// `SynExpr.Record(baseInfo, copyInfo, recordFields, range)` — a record
    /// expression (`SyntaxTree.fsi:634`, grammar `recdExpr` `pars.fsy:5679`).
    /// Two forms are parsed: the field-list `{ F = e; … }` and the
    /// copy-and-update `{ src with F = e; … }` (`copyInfo = Some src`). The
    /// `baseInfo` (`inherit …` records) is always `None` here and the per-field
    /// `equalsRange`/`blockSeparator`/range trivia is elided by the normaliser.
    ///
    /// Shape: `RECORD_EXPR > [LBRACE_TOK, (<copy-src-expr> WITH_TOK)?,
    /// RECORD_FIELD (sep RECORD_FIELD)*, RBRACE_TOK]` with trivia interleaved
    /// (`sep` = `SEMI_TOK` / a zero-width `ERROR` for `Virtual::BlockSep`); the
    /// `}` is swallowed and recovered like a computation expression's, and a
    /// copy-update's trailing `Virtual::End` is consumed as a zero-width
    /// `ERROR`. The optional leading copy source is the sole direct `Expr`
    /// child of `RECORD_EXPR` (field values are nested inside `RECORD_FIELD`).
    RECORD_EXPR,

    /// `SynExpr.AnonRecd` — an anonymous-record *expression* `{| F = e; … |}`
    /// (`SyntaxTree.fsi:620`). FCS: `AnonRecd(isStruct, copyInfo,
    /// recordFields: (SynLongIdent * range option * SynExpr) list, range,
    /// trivia)`. Reuses the record field-list machinery (`RECORD_FIELD`,
    /// `consume_one_seps_group`) since FCS's `braceBarExprCore` is built over
    /// the same `recdExprCore` (`pars.fsy:5917`).
    ///
    /// Shape: `ANON_RECD_EXPR > [LBRACE_BAR_TOK, (<copy-src-expr> WITH_TOK)?,
    /// RECORD_FIELD (sep RECORD_FIELD)*, BAR_RBRACE_TOK]` with trivia
    /// interleaved (`sep` = `SEMI_TOK` / a zero-width `ERROR` for
    /// `Virtual::BlockSep`). Unlike `RECORD_EXPR`'s `}`, the `|}`
    /// ([`SyntaxKind::BAR_RBRACE_TOK`]) is a *real* filtered token (not
    /// swallowed), bumped directly. A copy-update's trailing `Virtual::End` is
    /// consumed zero-width. The `struct {| … |}` form (`isStruct = true`) is
    /// deferred. Like `RECORD_EXPR`, the optional copy source is the sole
    /// direct `Expr` child (field values nest inside `RECORD_FIELD`).
    ANON_RECD_EXPR,

    /// `SynExpr.ArrayOrList` / `SynExpr.ArrayOrListComputed` — a list `[ … ]`
    /// or array `[| … |]` *expression* (`SyntaxTree.fsi:628`/`:682`, grammar
    /// `listExpr` `pars.fsy:5298` / `arrayExpr` `:5450`). FCS uses two AST
    /// variants distinguished by emptiness, both carried by this one node:
    /// an empty `[]` / `[||]` is `ArrayOrList(isArray, [], range)`, while a
    /// non-empty `[ e ]` / `[ e1; e2; … ]` is `ArrayOrListComputed(isArray,
    /// body, range)` whose `body` is the single `sequentialExpr` (a
    /// `SEQUENTIAL_EXPR` for two-or-more `;`/offside-separated elements, a
    /// bare element otherwise). The element separator is `;`, **not** `,`:
    /// `[a, b]` is a one-element list of the tuple `(a, b)`, while `[a; b]`
    /// is a two-element list.
    ///
    /// Shape: `ARRAY_OR_LIST_EXPR > [LBRACK_TOK, <body-expr>?, RBRACK_TOK]`
    /// (list) or `… > [LBRACK_BAR_TOK, <body-expr>?, BAR_RBRACK_TOK]` (array).
    /// `isArray` is recovered from the opener token (a `LBRACK_BAR_TOK` ⇒
    /// array). The body reuses the offside/`;` sequence gatherer
    /// ([`crate::parser`]'s `parse_seq_block_body`), so a multi-element body is
    /// one `SEQUENTIAL_EXPR` child. The closers `]` / `|]` are real filtered
    /// tokens (not swallowed, unlike `)` / `}`), bumped directly.
    ARRAY_OR_LIST_EXPR,

    /// `SynExprRecordField(fieldName, equalsRange, expr, range, blockSeparator)`
    /// — one `F = e` binding of a [`SyntaxKind::RECORD_EXPR`]
    /// (`SyntaxTree.fsi:991`). Shape:
    /// `RECORD_FIELD > [LONG_IDENT, EQUALS_TOK, <value-expr>]`. The field name
    /// is FCS's `RecordFieldName` (`SynLongIdent * bool`); the trailing-dot bool
    /// and the equals/separator ranges are elided.
    RECORD_FIELD,

    /// `SynExpr.YieldOrReturn(flags, expr, range, trivia)` — a `yield e`
    /// or `return e` (`SyntaxTree.fsi:899`, grammar `pars.fsy:4488`).
    /// Shape: `YIELD_OR_RETURN_EXPR > [YIELD_TOK, <inner-expr>]`. The
    /// `flags` tuple is `(isYield, !isYield)`: `yield` ⇒ `(true, false)`,
    /// `return` ⇒ `(false, true)`. `isYield` is recovered from the
    /// `YIELD_TOK` text; the trivia/range slots are elided.
    ///
    /// Also the implicit-`yield` body of a `for pat in e -> body` comprehension
    /// (`arrowThenExprR`, `pars.fsy:5608`): FCS desugars `-> body` to
    /// `YieldOrReturn((true, false), body)`, so this node is emitted as
    /// `YIELD_OR_RETURN_EXPR > [RARROW_TOK, <inner-expr>]` — no `yield` keyword,
    /// and [`crate::syntax::YieldExpr::is_yield`] reads the `RARROW_TOK` as the
    /// always-`true` `isYield`.
    YIELD_OR_RETURN_EXPR,

    /// `SynExpr.YieldOrReturnFrom(flags, expr, range, trivia)` — a
    /// `yield! e` or `return! e` (`SyntaxTree.fsi:904`, grammar
    /// `pars.fsy:4510`). Shape:
    /// `YIELD_OR_RETURN_FROM_EXPR > [YIELD_BANG_TOK, <inner-expr>]`. Same
    /// `flags` convention as [`SyntaxKind::YIELD_OR_RETURN_EXPR`].
    YIELD_OR_RETURN_FROM_EXPR,

    /// `SynExpr.DoBang(expr, range, trivia)` — a `do! e` in a computation
    /// expression (`SyntaxTree.fsi:925`, grammar `pars.fsy:4613`). Shape:
    /// `DO_BANG_EXPR > [DO_BANG_TOK, ERROR(BlockBegin), <inner-expr>,
    /// ERROR(BlockEnd), ERROR(DeclEnd)]` — the offside-block scaffolding is
    /// consumed as zero-width `ERROR` placeholders (the same treatment as
    /// the `if`/`then` body), so only the `DO_BANG_TOK` and the body expr
    /// carry information. Trivia/range elided.
    DO_BANG_EXPR,

    /// `SynExpr.Do(expr, range)` — a `do e` statement (`SyntaxTree.fsi:884`).
    /// In #light syntax FCS reaches this through `declExpr`
    /// (`hardwhiteDoBinding`, `pars.fsy:4211`), so a module-level `do e` is
    /// `SynModuleDecl.Expr(SynExpr.Do(e, _), _)` — a `DO_EXPR` inside the
    /// ordinary `EXPR_DECL` path — and a `do e` inside a sequence/CE body is a
    /// `SynExpr.Do` element. Shape mirrors [`SyntaxKind::DO_BANG_EXPR`]:
    /// `DO_EXPR > [DO_TOK, ERROR(BlockBegin), <inner-expr>, ERROR(BlockEnd),
    /// ERROR(DeclEnd)]` — the SeqBlock scaffolding is consumed as zero-width
    /// `ERROR` placeholders, so only `DO_TOK` and the body expr carry
    /// information. The `DO_TOK` keyword is shared with the `while`/`for`
    /// `do`-body. Trivia/range elided.
    DO_EXPR,

    /// `SynExpr.LetOrUse(SynLetOrUse)` — an expression-level `let`/`use`
    /// binding with a body (`SyntaxTree.fsi:913`, `SynLetOrUse` at `:564`).
    /// Phase 10.4b reaches this only through the computation-expression bang
    /// binders `let!`/`use!`/`and!`; a plain `let … in` expression slice would
    /// share the node. Shape:
    /// `LET_OR_USE_EXPR > [BINDER_TOK, BINDING, (AND_BANG_TOK, BINDING)*,
    /// ERROR(BlockEnd/DeclEnd/BlockSep)…, <body-expr>]`. The binding RHS blocks
    /// and the inter-binding/inter-body offside scaffolding are consumed as
    /// zero-width `ERROR` (as for `do!`/`if`); the binding's leading keyword
    /// (`let!`/`use!`/`and!`) is recovered from the preceding `BINDER_TOK`/
    /// `AND_BANG_TOK`. The single trailing non-`BINDING` expression child is the
    /// `SynLetOrUse.Body`. `IsRecursive` is `false` for the bang forms.
    LET_OR_USE_EXPR,

    /// `SynLongIdent` — the dotted-path body, a flat sequence of
    /// [`SyntaxKind::IDENT_TOK`] and [`SyntaxKind::DOT_TOK`] children (`Foo.Bar` = `IDENT, DOT,
    /// IDENT`). FCS stores idents and dotRanges as parallel lists; we
    /// keep them interleaved because the green tree is already
    /// source-ordered and the projector reconstructs the idents list by
    /// filtering out the dots.
    LONG_IDENT,

    /// `SynExpr.Typed(expr, targetType, range)` — an expression with a
    /// type annotation, `e : T`. Phase 7.1 only emits this from inside
    /// [`SyntaxKind::PAREN_EXPR`] (`(e : T)`); free-standing typed
    /// expressions (e.g. as a top-level binding RHS) land when the binding
    /// or expression layer needs them. Shape:
    /// `TYPED_EXPR > [<inner-expr>, COLON_TOK, <type>]` with trivia
    /// interleaved.
    TYPED_EXPR,

    /// `SynType.LongIdent(longDotId)` — a long-identifier reference used
    /// as a type, e.g. `int`, `System.Int32`. Shape:
    /// `LONG_IDENT_TYPE > [LONG_IDENT]` carrying the dotted path. FCS
    /// stores the long-ident directly on the case; we wrap it in a
    /// dedicated node so the typed-AST facade can dispatch on kind.
    LONG_IDENT_TYPE,

    /// `SynType.Anon(range)` — the anonymous-placeholder type `_`. Used
    /// in positions that accept inference (typed expressions, future
    /// generic-arg slots). Shape: `ANON_TYPE > [UNDERSCORE_TOK]`, a single
    /// token.
    ANON_TYPE,

    /// `SynType.Paren(innerType, range)` — a parenthesised type `( T )`.
    /// FCS keeps `Paren` in the AST so the formatter and tooling can
    /// distinguish `int -> (int * int)` from `int -> int * int`. Shape:
    /// `PAREN_TYPE > [LPAREN_TOK, <inner-type>, RPAREN_TOK]` with trivia
    /// interleaved.
    PAREN_TYPE,

    /// `SynType.Var(SynTypar, range)` — a type variable. FCS's `SynTypar`
    /// is `SynTypar(ident, staticReq, isCompGen)`; the `staticReq` flag
    /// distinguishes the plain quoted form `'a` (`TyparStaticReq.None`)
    /// from the head-typar form `^T` (`TyparStaticReq.HeadType`). Shape:
    /// `VAR_TYPE > [(QUOTE_TOK | HAT_TOK), IDENT_TOK]`; the leading sigil's
    /// kind tells the facade which `staticReq` to project.
    VAR_TYPE,

    /// `SynType.Fun(argType, returnType, range, trivia)` — a function
    /// type `T -> U` (`pars.fsy:6215`, `SyntaxTree.fsi:506`). The
    /// grammar is right-recursive (`typ → tupleType RARROW typ`), so
    /// `int -> int -> int` nests as `Fun(int, Fun(int, int))`. Shape:
    /// `FUN_TYPE > [<arg-type>, RARROW_TOK, <return-type>]` with trivia
    /// interleaved; the trivia's `ArrowRange` is recoverable from the
    /// `RARROW_TOK`'s span.
    FUN_TYPE,

    /// `SynType.Tuple(isStruct, path, range)` — a tuple type
    /// `T * U * V` (`pars.fsy:6243`, `SyntaxTree.fsi:496`). The grammar
    /// makes the path *flat*: `int * string * bool` projects as a
    /// single `SynTupleTypeSegment` list
    /// `[Type int; Star; Type string; Star; Type bool]`, not nested
    /// pairs. Shape: `TUPLE_TYPE > [<ty>, STAR_TOK, <ty>, …]` with
    /// trivia interleaved. The grammar layer sits between `appType`
    /// (tighter) and `typ` (which adds the arrow), so a `* / ->` mix
    /// like `int * int -> int` nests as `Fun(Tuple(int, int), int)`.
    /// Phase 7.4 covers only `isStruct = false` and the `*` segment
    /// (no `struct (T * U)`, no `Slash` form).
    TUPLE_TYPE,

    /// `SynType.App(name, lessRange, args, commaRanges, greaterRange,
    /// isPostfix, range)` — a type-constructor application
    /// (`pars.fsy:6371` and `pars.fsy:6378`). One node-kind covers both
    /// surface forms; the surface is recoverable from the children:
    ///
    /// - Postfix `int list` (phase 7.5): no `LESS_TOK` child. Shape
    ///   `APP_TYPE > [<arg-type>, <head-type>]` in source order; the
    ///   head is restricted by FCS's `appTypeConPower → appTypeCon` to
    ///   `path` (`LONG_IDENT_TYPE`) or `typar` (`VAR_TYPE`).
    ///   `isPostfix = true`.
    /// - Prefix `Foo<int, string>` (phase 7.6, not yet implemented):
    ///   `LESS_TOK` child present. Shape
    ///   `APP_TYPE > [<head-type>, LESS_TOK, <arg-type>,
    ///   (COMMA_TOK, <arg-type>)*, GREATER_TOK]`. `isPostfix = false`.
    ///
    /// The grammar layer sits between `tupleType` (looser) and
    /// `atomType` (tighter), so app binds tighter than `*` and `->`:
    /// `int list * string list` projects as
    /// `Tuple(App(int,list), App(string,list))`, and
    /// `int -> int list` as `Fun(int, App(int, list))`. Left-recursive
    /// in FCS (`appTypeWithoutNull appTypeConPower`), so
    /// `int list option` nests as `App(option, App(list, int))`, not
    /// flat.
    APP_TYPE,

    /// `SynType.LongIdentApp(typeName, longDotId, lessRange, args,
    /// commaRanges, greaterRange, range)` — a dotted-path type
    /// application whose root is itself a non-`path` atomic type
    /// (`pars.fsy:6600-6605`, grammar `atomType DOT path
    /// [typeArgsNoHpaDeprecated]`). Distinct from
    /// [`SyntaxKind::APP_TYPE`] because the root spans more than a
    /// plain dotted ident: a paren-wrapped type (`(int list).Foo`)
    /// or another `App` / `LongIdentApp` (`Foo<int>.Bar<string>`,
    /// `(int).Foo<string>.Bar`). Plain dotted paths like
    /// `System.Collections.Generic.List<int>` project as
    /// `App(LongIdent[System.Collections.Generic.List], [int])` —
    /// a single `APP_TYPE` with a multi-segment head — so they
    /// never reach this kind.
    ///
    /// Note on grammar vs. LR tables: `pars.fsy:6600` also admits
    /// `'T.Foo` (bare typar LHS), but FCS's compiled LR tables
    /// reject that surface and recover as `App(Foo, [Var 'T],
    /// postfix=true)` with a "Unexpected identifier in definition"
    /// error. The parser mirrors FCS by only firing the dot-chain
    /// rule when the LHS is Paren / Anon / HPA-wrapped App; see
    /// `parse_atomic_type`'s `head_can_chain` gate.
    ///
    /// Shape `LONG_IDENT_APP_TYPE > [<root-type>, DOT_TOK,
    /// LONG_IDENT, (LESS_TOK <arg-type> (COMMA_TOK <arg-type>)*
    /// GREATER_TOK)?]`. The optional angle-bracket block matches
    /// the same surface as the prefix arm of [`SyntaxKind::APP_TYPE`]
    /// (HPA-gated `<…>`); when absent the node represents the bare
    /// `root.path` form.
    ///
    /// Left-associative chaining: `(int).Foo<string>.Bar` parses
    /// as `LongIdentApp(LongIdentApp(Paren int, [Foo], [string]),
    /// [Bar], [])`, matching FCS's left-recursive
    /// `atomType DOT path` rule.
    LONG_IDENT_APP_TYPE,

    /// `SynType.Array(rank, elementType, range)` — an array-type suffix
    /// (`pars.fsy:6371-6376`, projection `pars.fsy:6397-…`). Shape
    /// `ARRAY_TYPE > [<elementType>, (ERROR-HPBA)?, LBRACK_TOK,
    /// (COMMA_TOK)*, RBRACK_TOK]`, where the rank is recoverable as
    /// `1 + count(COMMA_TOK children)` (FCS rejects rank > 32; this
    /// kind records whatever the input gives — the diagnostic belongs
    /// to a later pass).
    ///
    /// Left-associative: chained suffixes like `int[][]` (a jagged
    /// array) nest as `Array(rank=1, Array(rank=1, int))`. Composes
    /// with both surface forms of [`SyntaxKind::APP_TYPE`] — the
    /// element-type slot accepts whatever `parse_app_type` produces, so
    /// `int list[]` parses as `Array(rank=1, App(list, [int], postfix))`.
    ///
    /// The optional pre-`LBRACK_TOK` HPBA placeholder marks the IDENT-
    /// adjacent `name[]` form (LexFilter's
    /// [`crate::lexfilter::Virtual::HighPrecedenceBrackApp`]); the
    /// non-adjacent `(int)[]` form omits it. Both arms project to the
    /// same `SynType.Array` on the FCS side
    /// (`pars.fsy:6371-6376`'s two grammar rules merge in the semantic
    /// action).
    ARRAY_TYPE,

    /// Flexible-type constraint — FCS's `SynType.HashConstraint(inner,
    /// range)`. The `#T` form comes from `pars.fsy:2609-2611`
    /// (`HASH atomType`) with shape
    /// `HASH_CONSTRAINT_TYPE > [HASH_TOK, <inner-atomic-type>]`; the inner
    /// child is whatever `parse_atomic_type` produces at the recursive
    /// call site (so the FCS-equivalent `atomType` recursion covers
    /// `##int`, `#'T`, `#(int -> int)`, and `#Foo<int>` — the prefix-app
    /// wrap sits inside `parse_atomic_type` rather than the layer above,
    /// matching FCS's `atomType → appTypeConPower` placement). The
    /// `_ :> T` app-type shorthand also projects to this node, preserving
    /// the source surface as `[UNDERSCORE_TOK, COLON_GREATER_TOK,
    /// <inner-type>]`.
    ///
    /// Postfix application and array suffixes sit at the
    /// [`SyntaxKind::APP_TYPE`] / [`SyntaxKind::ARRAY_TYPE`] layer above,
    /// so `#int list` projects as `App(list, [HashConstraint(int)],
    /// postfix)` — the hash binds tighter than the postfix application.
    HASH_CONSTRAINT_TYPE,

    /// `{| F : int; G : string |}` anonymous-record type — FCS's
    /// `SynType.AnonRecd(isStruct, fields, range)` from
    /// `pars.fsy:6520-6531` (`atomTypeOrAnonRecdType: anonRecdType`).
    /// Shape `ANON_RECD_TYPE > [STRUCT_TOK?, LBRACE_BAR_TOK,
    /// (ANON_RECD_TYPE_FIELD (SEMI_TOK ANON_RECD_TYPE_FIELD)*)?,
    /// BAR_RBRACE_TOK]`; the optional leading `STRUCT_TOK` distinguishes
    /// `struct {| F : int |}` from the reference variant.
    ///
    /// FCS layers this one above `atomType`
    /// (`atomTypeOrAnonRecdType: atomType | anonRecdType`); we
    /// dispatch from the same site as the hash branch in
    /// `parse_atomic_type` because the postfix-app / array suffix
    /// loops sit on a shared checkpoint and so still wrap it
    /// correctly — `{| F : int |} list` projects as
    /// `App(list, [AnonRecd], postfix)`.
    ANON_RECD_TYPE,

    /// `string | null` nullable reference type — FCS's
    /// `SynType.WithNull(innerType, ambivalent, range, trivia)` from
    /// `appTypeCanBeNullable: appTypeWithoutNull BAR_JUST_BEFORE_NULL
    /// NULL` (`pars.fsy:6357-6359`). Shape: `WITH_NULL_TYPE >
    /// [<inner-type>, BAR_TOK, NULL_TOK]`. The `ambivalent` flag is
    /// always `false` at parse time, so it carries no syntactic
    /// information and is not represented in the tree. FCS layers this
    /// between `tupleType` (above) and `appTypeWithoutNull` (the
    /// postfix array/app layer, below): `int list | null` parses as
    /// `WithNull(App(list, [int], postfix))`, and `string | null * int`
    /// as `Tuple([WithNull(string); int])`.
    WITH_NULL_TYPE,

    /// A type carrying a trailing `when` constraint clause — FCS's
    /// `SynType.WithGlobalConstraints(typeName, constraints, range)` from the
    /// `typeWithTypeConstraints: typ WHEN typeConstraints` grammar
    /// (`pars.fsy:6023`). Appears wherever `typeWithTypeConstraints` is reached:
    /// a binding's return-type annotation (`let f x : 'T when 'T : struct = …`),
    /// a typed pattern / expression, a `val`/member signature. Shape:
    /// `CONSTRAINED_TYPE > [<base type>, TYPAR_CONSTRAINTS]`, where the
    /// [`SyntaxKind::TYPAR_CONSTRAINTS`] child is the same `WHEN_TOK` +
    /// `and`-separated [`SyntaxKind::TYPAR_CONSTRAINT`] group a type-definition
    /// header uses.
    CONSTRAINED_TYPE,

    /// `'T & IDisposable` / `#A & #B` constraint intersection — FCS's
    /// `SynType.Intersection(typar option, types, range, trivia)` from
    /// `intersectionType` (`pars.fsy:6328-6335`, phase 10.10). Shape:
    /// `INTERSECTION_TYPE > [<head-type>, (AMP_TOK <type>)+]`, where the head
    /// is either a [`SyntaxKind::VAR_TYPE`] (the `typar AMP …` form — FCS's
    /// `Intersection(Some typar, …)`) or a [`SyntaxKind::HASH_CONSTRAINT_TYPE`]
    /// (the `hashConstraint AMP …` form — `Intersection(None, …)`, where the
    /// leading `#A` is instead the first `types` element). The facade recovers
    /// the typar-vs-hash split from the first `Type` child's kind.
    ///
    /// Sits at FCS's `appTypeWithoutNull` layer (parallel to the postfix-app /
    /// array / nullable arms), so it binds tighter than the tuple `*` and the
    /// arrow: `#A & #B -> int` projects as `Fun(Intersection([#A; #B]), int)`.
    /// Only a *bare* typar / hash head opens it — a prefix-applied typar
    /// (`'T<int>`) is not a head, matching FCS's parse error on `'T<int> & …`.
    INTERSECTION_TYPE,

    /// A single `ident COLON typ` field inside an
    /// [`SyntaxKind::ANON_RECD_TYPE`]. FCS's `fieldDecl: opt_mutable
    /// opt_access ident COLON typ` (`pars.fsy:2978-2980`) projects via
    /// `SynField`, but the anon-recd post-processor
    /// (`pars.fsy:6526-6529`) rejects attributes / mutable / access on
    /// AnonRecd fields with a parse error; phase 7.9 admits only the
    /// minimal `IDENT_TOK COLON_TOK <typ>` form.
    ANON_RECD_TYPE_FIELD,

    /// The `[<` attribute-list opener (lexer `Token::LBrackLess`). A leaf
    /// token child of [`SyntaxKind::ATTRIBUTE_LIST`].
    LBRACK_LESS_TOK,

    /// The `>]` attribute-list closer (lexer `Token::GreaterRBrack`). A leaf
    /// token child of [`SyntaxKind::ATTRIBUTE_LIST`].
    GREATER_RBRACK_TOK,

    /// A single custom attribute — FCS's `SynAttribute` record
    /// (`SyntaxTree.fsi:1209`), grammar `attribute: attributeTarget? path
    /// opt_HIGH_PRECEDENCE_APP opt_atomicExprAfterType` (`pars.fsy:1542`).
    /// Shape: `ATTRIBUTE > [ATTRIBUTE_TARGET?, LONG_IDENT, <arg-expr>?]` — the
    /// optional `attributeTarget` (phase 10.5c), the `path`, then the optional
    /// `atomicExprAfterType` argument (phase 10.5b). A bare attribute has no
    /// target or arg child; its `ArgExpr` is FCS's synthetic `mkSynUnit`,
    /// materialised only in the normaliser.
    ATTRIBUTE,

    /// The `attributeTarget` prefix of an [`SyntaxKind::ATTRIBUTE`] — FCS's
    /// `attributeTarget` (`pars.fsy:1565`), populating `SynAttribute.Target`.
    /// Shape: `ATTRIBUTE_TARGET > [IDENT_TOK, COLON_TOK]`. The target word is
    /// emitted as `IDENT_TOK` regardless of whether it lexed as an ident
    /// (`assembly:`/`field:`/…), the `type` keyword, or the `return` keyword —
    /// FCS's canonical `Target` idText is the source text in each supported case.
    /// The `module:` target is *not* supported: `[<module: …>]` is an FCS parse
    /// error (the `module` keyword drives LexFilter's module-head machinery even
    /// inside `[< … >]`), so `module` falls through to the path parser's error.
    ATTRIBUTE_TARGET,

    /// One `[< … >]` attribute group — FCS's `SynAttributeList` record
    /// (`SyntaxTree.fsi:1229`), grammar `attributeList` (`pars.fsy:1516`).
    /// Shape: `ATTRIBUTE_LIST > [LBRACK_LESS_TOK, ATTRIBUTE (<seps> ATTRIBUTE)*,
    /// GREATER_RBRACK_TOK]`. Phase 10.5a admits one or more attributes separated
    /// by FCS's `seps` group (`;`, an offside `OBLOCKSEP`, or the `OBLOCKSEP ;` /
    /// `; OBLOCKSEP` pairs) — the `;` is a `SEMI_TOK`, the `OBLOCKSEP` a
    /// zero-width `ERROR`. A trailing separator before `>]` is tolerated
    /// (`opt_seps`). One or more adjacent lists form the carrier's
    /// `SynAttributes` (a list-of-lists); they attach to a `let`-binding as
    /// leading children of its [`SyntaxKind::LET_DECL`], before `LET_TOK`.
    ATTRIBUTE_LIST,

    // ---- reserved for phases 9–10 (declared, not yet emitted) ---------------
    //
    // Pre-declared so concurrent feature branches don't collide on the
    // `SyntaxKind` enum tail — the one region every remaining phase-9/10
    // sub-phase would otherwise append to. Each variant is claimed by the
    // sub-phase named in its doc (`docs/parser-plan.md`). Until then nothing
    // constructs it: the derived traits keep it from tripping `dead_code` (as
    // for the long-reserved `SIG_FILE` above), and every `cast` in `mod.rs`
    // maps an unknown kind to `None`. A sub-phase implementing one wires up its
    // parser production + facade + normaliser; it may leave the variant here or
    // move it into the section above.

    // -- phase 9 Block B: object-model member keywords (9.9–9.14) --
    /// `abstract` keyword (lexer `Token::Abstract`) — `SynMemberDefn.AbstractSlot`
    /// leading keyword, e.g. `abstract member M : int -> int` (phase 9.10).
    /// Reserved; not yet emitted.
    ABSTRACT_TOK,

    /// `override` keyword (lexer `Token::Override`) — the `override this.M() = …`
    /// member leading keyword (phase 9.10). Reserved; not yet emitted.
    OVERRIDE_TOK,

    /// `default` keyword (lexer `Token::Default`) — the `default this.M() = …`
    /// member leading keyword (phase 9.10). Reserved; not yet emitted.
    DEFAULT_TOK,

    /// `inherit` keyword (lexer `Token::Inherit`) — opens a `SynMemberDefn.Inherit`
    /// / `ImplicitInherit` base-class clause, e.g. `inherit Base()` (phase 9.11a).
    INHERIT_TOK,

    /// `val` keyword (FCS's `Token::Val`) introducing an explicit field
    /// declaration (`val mutable x : int`, phase 9.9b) — a real filtered token, a
    /// direct child of a [`SyntaxKind::VAL_FIELD`] (FCS's
    /// `SynField.trivia.LeadingKeyword.Val`). Also opens a `SynValSig` value
    /// signature (phase 10.12, not yet emitted).
    VAL_TOK,

    /// `interface` keyword (lexer `Token::Interface`). Two uses: in member
    /// position (LexFilter `OINTERFACE_MEMBER`) it opens a
    /// `SynMemberDefn.Interface` implementation, e.g. `interface I with …`
    /// (phase 9.11b, nested in an `INTERFACE_IMPL` node); as a `type T =
    /// interface … end` kind marker it is a direct token child of an
    /// `OBJECT_MODEL_REPR`, setting `SynTypeDefnKind.Interface` (phase 9.12).
    INTERFACE_TOK,

    /// `class` keyword (lexer `Token::Class`) — the explicit
    /// `type T = class … end` kind marker, `SynTypeDefnKind.Class` (phase 9.12),
    /// a direct token child of an `OBJECT_MODEL_REPR`.
    CLASS_TOK,

    /// `begin` keyword (lexer `Token::Begin`) — the verbose-syntax block opener.
    /// As an expression it opens a `begin e end` group (`SynExpr.Paren`, the
    /// `beginEndExpr` production `pars.fsy:5419`), a direct token child of a
    /// [`SyntaxKind::PAREN_EXPR`] (or of the [`SyntaxKind::CONST_EXPR`] unit for
    /// the empty `begin end`); as a module body it opens
    /// `module X = begin … end` (`wrappedNamedModuleDefn`, `pars.fsy:1478`), a
    /// direct token child of the [`SyntaxKind::NESTED_MODULE_DECL`]. Paired with
    /// [`SyntaxKind::END_TOK`].
    BEGIN_TOK,

    /// `end` keyword (lexer `Token::End`) — closes an explicit
    /// `class`/`struct`/`interface … end` body (phase 9.12) or a
    /// `begin … end` block (the expression and module-body forms above).
    END_TOK,

    /// `new` keyword (lexer `Token::New`) — opens an explicit constructor member
    /// `new(args) = …` (phase 9.10b, a `NEW_TOK` head segment in the member's
    /// head `LONG_IDENT`) or an object-construction expression `new T(args)`
    /// ([`SyntaxKind::NEW_EXPR`]).
    NEW_TOK,

    /// `upcast` keyword (lexer `Token::Upcast`) — the prefix coercion operator
    /// opening an [`SyntaxKind::INFERRED_UPCAST_EXPR`].
    UPCAST_TOK,

    /// `downcast` keyword (lexer `Token::Downcast`) — the prefix coercion
    /// operator opening an [`SyntaxKind::INFERRED_DOWNCAST_EXPR`].
    DOWNCAST_TOK,

    /// `lazy` keyword (lexer `Token::Lazy`) — the delayed-computation prefix
    /// opening a [`SyntaxKind::LAZY_EXPR`].
    LAZY_TOK,

    /// `assert` keyword (lexer `Token::Assert`) — the runtime-assertion prefix
    /// opening an [`SyntaxKind::ASSERT_EXPR`].
    ASSERT_TOK,

    /// `fixed` keyword (lexer `Token::Fixed`) — the pinning prefix opening a
    /// [`SyntaxKind::FIXED_EXPR`].
    FIXED_TOK,

    // -- phase 9 Block B: object-model member node kinds (9.9–9.14) --
    /// `SynMemberDefn.ValField` — an explicit field declaration in an
    /// object-model body (`val mutable x : int` / `static val x : int`,
    /// `SyntaxTree.fsi:1712`, `pars.fsy:2168`'s `valDefnDecl`, phase 9.9b).
    /// Shape `[STATIC_TOK?, VAL_TOK, MUTABLE_TOK?, IDENT_TOK, COLON_TOK, <typ>]`,
    /// wrapping FCS's `SynField` (the same shape projected for record/union-case
    /// fields, here with `isStatic` significant). Unlike a member/let it has no
    /// `= <expr>` RHS, so no offside RHS block.
    VAL_FIELD,

    /// `SynMemberDefn.Inherit` / `ImplicitInherit` — a base-class inheritance
    /// member, e.g. `inherit Base()` (phase 9.11a). Shape `[INHERIT_TOK, <type>,
    /// <args-expr>?, (AS_TOK IDENT_TOK)?]`; the args-expr's presence
    /// discriminates `ImplicitInherit` (args) from `Inherit` (none).
    INHERIT_MEMBER,

    /// `SynMemberDefn.Interface` — an interface implementation member, e.g.
    /// `interface I with member … = …` (phase 9.11b). Shape `[INTERFACE_TOK,
    /// <type>, (WITH_TOK MEMBER_DEFN*)?]`; the `WITH_TOK`'s presence is FCS's
    /// `members: SynMemberDefns option` discriminant (`Some` vs `None`).
    INTERFACE_IMPL,

    /// `SynMemberDefn.AbstractSlot` — an abstract member slot wrapping a
    /// `SynValSig`, e.g. `abstract member M : int -> int` (phase 9.10).
    /// Reserved; not yet emitted.
    ABSTRACT_SLOT,

    /// `SynMemberSig.Member` — a member signature in a signature-file type body,
    /// e.g. `member M : int`, `abstract M : int -> int`, `static member Make :
    /// unit -> T` (phase 10.14, slice 3a). Shape `[STATIC_TOK?, ABSTRACT_TOK?,
    /// MEMBER_TOK?, VAL_SIG]`, where the `VAL_SIG` child carries the name and
    /// `: <type>` and the leading keyword tokens select the member kind
    /// (`member`/`abstract`/`abstract member`/`static member`). Distinct from
    /// the impl-side `ABSTRACT_SLOT` (which is always an `abstract` slot): in a
    /// signature file FCS represents *every* member — concrete or abstract — as
    /// one `SynMemberSig.Member` carrier.
    MEMBER_SIG,

    /// `SynMemberDefn.AutoProperty` — an auto-implemented property, e.g.
    /// `member val X = 0 with get, set` (phase 9.9c). Shape `[STATIC_TOK?,
    /// MEMBER_TOK, VAL_TOK, ACCESS_TOK?, IDENT_TOK, (COLON_TOK <typ>)?,
    /// EQUALS_TOK, <expr>, (WITH_TOK GET_TOK (COMMA_TOK SET_TOK)?)?]`.
    AUTO_PROPERTY,

    /// `get` accessor word in an auto-property's `with get[, set]` clause (phase
    /// 9.9c) — FCS lexes it as a contextual `Token::Ident("get")`; emitted as a
    /// distinct token so the property's `propKind` is recoverable from the tree.
    /// (Also the accessor marker of a `GET_SET_MEMBER`, phase 9.14.)
    GET_TOK,

    /// `set` accessor word in an auto-property's `with get, set` clause (phase
    /// 9.9c) — a contextual `Token::Ident("set")`, sibling of [`Self::GET_TOK`].
    SET_TOK,

    /// `SynMemberDefn.GetSetMember` — a property with explicit `get`/`set`
    /// accessors, e.g. `member this.P with get() = … and set v = …` (phase 9.14).
    /// Shape `[MEMBER_TOK, LONG_IDENT_PAT (the property head), WITH_TOK,
    /// GET_SET_ACCESSOR, (AND_TOK GET_SET_ACCESSOR)?]`.
    GET_SET_MEMBER,

    /// One accessor of a [`Self::GET_SET_MEMBER`] (phase 9.14) — a `get`/`set`
    /// clause with its own argument patterns and body. Shape `[GET_TOK|SET_TOK,
    /// <arg pats>, EQUALS_TOK, <body expr>]`; mirrors FCS's per-accessor
    /// `SynBinding` (its `extraId` get/set and the shared property path are
    /// reconstructed by the facade/normaliser).
    GET_SET_ACCESSOR,

    /// `SynValSig` — a value/member signature (`name : type` with arity/trivia).
    /// The payload of `ABSTRACT_SLOT` (phase 9.10) and of a signature-file
    /// `SynModuleSigDecl.Val` (phase 10.12); the two phases share this node and
    /// its normaliser projection. For a signature-file `val`, the shape is
    /// `VAL_SIG > [MUTABLE_TOK?, INLINE_TOK?, IDENT_TOK, COLON_TOK, <type>]`
    /// (the leading `val` keyword is a child of the enclosing
    /// [`VAL_DECL`](SyntaxKind::VAL_DECL)).
    VAL_SIG,

    /// `SynModuleSigDecl.Val` — a `val` specification in a signature file
    /// (`.fsi`, phase 10.12a), e.g. `val x : int`. Shape
    /// `VAL_DECL > [VAL_TOK, VAL_SIG]`: the leading `val` keyword followed by the
    /// [`VAL_SIG`](SyntaxKind::VAL_SIG) carrier (mirrors how
    /// [`ABSTRACT_SLOT`](SyntaxKind::ABSTRACT_SLOT)'s
    /// `abstract` keyword sits outside its `VAL_SIG` child). The impl-side
    /// counterpart is a `let` binding, so unlike `open`/nested-module decls this
    /// node is sig-only.
    VAL_DECL,

    // -- phase 9 Block C / phase 10.7: exceptions and the standalone attribute decl --
    /// `exception` keyword (lexer `Token::Exception`) — opens a
    /// `SynModuleDecl.Exception` definition (phase 9.15). Reserved; not yet
    /// emitted.
    EXCEPTION_TOK,

    /// `SynModuleDecl.Exception` / `SynExceptionDefn` — an exception definition,
    /// e.g. `exception E of int` (phase 9.15). Reserved; not yet emitted.
    EXCEPTION_DEFN,

    /// `SynModuleDecl.Attributes` — a standalone attribute declaration not
    /// attached to a following carrier, e.g. `[<assembly: Foo>]` (phase 10.7).
    /// Reserved; not yet emitted.
    ATTRIBUTES_DECL,

    // -- phase 10 Block C: long-tail `SynType` (10.8–10.9) --
    /// `SynType.MeasurePower` — a unit-of-measure power, e.g. the `m^2` in
    /// `(x : float<m^2>)` (phase 10.8). Shape `MEASURE_POWER_TYPE > [<base
    /// SynType>, MEASURE_POWER_OP_TOK, <rational-const node>]`.
    MEASURE_POWER_TYPE,

    /// The `^` / `^-` operator of a [`MEASURE_POWER_TYPE`](SyntaxKind::MEASURE_POWER_TYPE)
    /// (phase 10.8) — FCS's `INFIX_AT_HAT_OP` in the `appTypeConPower`
    /// production (`pars.fsy:6344`). Our lexer emits both as a single
    /// `Token::Op` (`"^"` or `"^-"`); the parser stamps this kind only when
    /// the op text is exactly one of those two. The normaliser reads the
    /// token text to decide whether the exponent is wrapped in
    /// `SynRationalConst.Negate` (the `"^-"` spelling).
    MEASURE_POWER_OP_TOK,

    /// `/` divisor operator inside a measure-power rational exponent, e.g.
    /// the `/` in `m^(1/2)` (phase 10.8). FCS's `INFIX_STAR_DIV_MOD_OP` with
    /// text `"/"` in the `rationalConstant` rule (`pars.fsy:3486`); our lexer
    /// emits `Token::Op("/")` and the parser stamps this kind only when the
    /// op text equals `"/"`. Separates the numerator and denominator inside a
    /// [`RATIONAL_CONST_RATIONAL`](SyntaxKind::RATIONAL_CONST_RATIONAL).
    SLASH_TOK,

    /// A standalone prefix `-` inside a rational exponent, e.g. the `-` in
    /// `m^(- 2)` (phase 10.8) — FCS's `MINUS` in the `rationalConstant` /
    /// `atomicRationalConstant` rules (`pars.fsy:3489`/`:3513`). Only arises
    /// when the `-` is *not* adjacent to its digit (an adjacent `-2` is
    /// folded into a single signed literal by `sign_fold`); the leading child
    /// of a [`RATIONAL_CONST_NEGATE`](SyntaxKind::RATIONAL_CONST_NEGATE).
    MINUS_TOK,

    /// `SynRationalConst.Integer` — a plain integer measure exponent, e.g.
    /// the `2` in `m^2` (phase 10.8). Shape `RATIONAL_CONST_INTEGER >
    /// [INT32_LIT]`; the literal text may carry a `sign_fold`-merged `-`.
    RATIONAL_CONST_INTEGER,

    /// `SynRationalConst.Rational` — a fractional measure exponent, e.g. the
    /// `1/2` in `m^(1/2)` (phase 10.8). Shape `RATIONAL_CONST_RATIONAL >
    /// [INT32_LIT, SLASH_TOK, INT32_LIT]` (numerator, `/`, denominator).
    /// Reachable only inside a
    /// [`RATIONAL_CONST_PAREN`](SyntaxKind::RATIONAL_CONST_PAREN).
    RATIONAL_CONST_RATIONAL,

    /// `SynRationalConst.Negate` — a negated rational exponent built from a
    /// standalone prefix `-` (phase 10.8). Shape `RATIONAL_CONST_NEGATE >
    /// [MINUS_TOK, <inner rational-const>]`. (A `^-` operator instead
    /// produces the `Negate` at projection time without this node; see
    /// [`MEASURE_POWER_OP_TOK`](SyntaxKind::MEASURE_POWER_OP_TOK).)
    RATIONAL_CONST_NEGATE,

    /// `SynRationalConst.Paren` — a parenthesised rational exponent, e.g.
    /// `(1/2)` / `(-1)` in `m^(1/2)` / `m^(-1)` (phase 10.8). Shape
    /// `RATIONAL_CONST_PAREN > [LPAREN_TOK, <inner rational-const>,
    /// RPAREN_TOK]`. The parens are also the only place a
    /// [`RATIONAL_CONST_RATIONAL`](SyntaxKind::RATIONAL_CONST_RATIONAL) can
    /// appear.
    RATIONAL_CONST_PAREN,

    /// `SynType.StaticConstant` — a literal type-provider static argument, e.g.
    /// the `42` in `(x : Foo<42>)` (phase 10.9). Shape `STATIC_CONST_TYPE >
    /// [<const literal token>]`, where the literal is whatever
    /// `parse_const_payload` emits (`INT32_LIT`, `STRING_LIT`, `BOOL_LIT`, …);
    /// the held `SynConst` is recovered from that token exactly as for a
    /// [`CONST_EXPR`](SyntaxKind::CONST_EXPR).
    STATIC_CONST_TYPE,

    /// `SynType.StaticConstantExpr` — a `const`-expression type-provider static
    /// argument, e.g. `(x : Foo<const E>)` (phase 10.9). Shape
    /// `STATIC_CONST_EXPR_TYPE > [CONST_TOK, <atomic expr>]` (FCS's
    /// `CONST atomicExpr`, `pars.fsy:6583`).
    STATIC_CONST_EXPR_TYPE,

    /// `SynType.StaticConstantNamed` — a named type-provider static argument,
    /// e.g. the `N=42` in `(x : Foo<N=42>)` (phase 10.9). Shape
    /// `STATIC_CONST_NAMED_TYPE > [<ident type>, EQUALS_TOK, <value type>]`
    /// (FCS's `typeArgActual: typ EQUALS typ`, `pars.fsy:6668`); both sides are
    /// full `SynType`s.
    STATIC_CONST_NAMED_TYPE,

    /// `SynType.StaticConstantNull` — a `null` type-provider static argument,
    /// e.g. `(x : Foo<null>)` (phase 10.9). Shape `STATIC_CONST_NULL_TYPE >
    /// [NULL_TOK]`.
    STATIC_CONST_NULL_TYPE,

    // -- phase 10 Block D: signature-only `SynType` carriers (10.12) --
    /// `SynType.SignatureParameter` — a named/optional function-type parameter
    /// in a value signature, e.g. the `x: int` in `val f : x: int -> int`
    /// (phase 10.12). Reserved; not yet emitted.
    SIGNATURE_PARAMETER_TYPE,

    /// `SynType.WithGlobalConstraints` — a signature type carrying trailing
    /// `when` constraints, e.g. `'T -> 'T when 'T : comparison` (phase 10.12).
    /// Reserved; not yet emitted.
    WITH_GLOBAL_CONSTRAINTS_TYPE,

    // -- unit-of-measure annotated literals (`SynConst.Measure`) --
    /// A measure-annotated numeric literal expression — FCS's `rawConstant
    /// HIGH_PRECEDENCE_TYAPP measureTypeArg` (`pars.fsy:3521`), projecting to
    /// `SynExpr.Const(SynConst.Measure(constant, range, synMeasure, trivia))`.
    /// Shape `MEASURE_LIT_EXPR > [CONST_EXPR, LESS_TOK, <measure>, GREATER_TOK]`
    /// (the `HIGH_PRECEDENCE_TYAPP` adjacency virtual is consumed zero-width as
    /// an `ERROR` before the `<`). The inner `CONST_EXPR` carries the underlying
    /// `SynConst`; the `<measure>` child is one of the `MEASURE_*` nodes below.
    MEASURE_LIT_EXPR,

    /// `SynMeasure.Seq` — a juxtaposition sequence of measure factors, e.g.
    /// `<kg m s>`. Shape `MEASURE_SEQ > [<measure-power>+]`. FCS wraps *every*
    /// `measureTypeExpr` in a `Seq`, so even a single named measure `<m>` is
    /// `Seq[Named ["m"]]`.
    MEASURE_SEQ,

    /// `SynMeasure.Named` — a named unit of measure (a `path`), e.g. `m` or
    /// `SI.metre`. Shape `MEASURE_NAMED > [LONG_IDENT]`.
    MEASURE_NAMED,

    /// `SynMeasure.Product` — `m * s`. Shape `MEASURE_PRODUCT > [<measure>,
    /// STAR_TOK, <measure>]`, left-associative.
    MEASURE_PRODUCT,

    /// `SynMeasure.Divide` — `m / s`, or the no-numerator reciprocal `/s`
    /// (`Divide(None, _)`). Shape `MEASURE_DIVIDE > [<measure>?, SLASH_TOK,
    /// <measure>]` — the leading measure is absent for the reciprocal form.
    MEASURE_DIVIDE,

    /// `SynMeasure.Power` — `m ^ 2`. Shape `MEASURE_POWER > [<measure>,
    /// MEASURE_POWER_OP_TOK, <rational-const>]`, reusing the `RATIONAL_CONST_*`
    /// exponent nodes (the same as [`MEASURE_POWER_TYPE`](SyntaxKind::MEASURE_POWER_TYPE)).
    MEASURE_POWER,

    /// `SynMeasure.One` — the dimensionless `1` measure (`<1>`). Shape
    /// `MEASURE_ONE > [INT32_LIT]`; FCS admits only the literal `1` here.
    MEASURE_ONE,

    /// `SynMeasure.Anon` — the anonymous (inferred) measure `<_>`. Shape
    /// `MEASURE_ANON > [UNDERSCORE_TOK]`. Reached through the dedicated
    /// `measureTypeArg: LESS UNDERSCORE GREATER` arm, so it is *not* wrapped in
    /// a `Seq`.
    MEASURE_ANON,

    /// `SynMeasure.Var` — a measure variable `<'u>`. Shape `MEASURE_VAR >
    /// [QUOTE_TOK, IDENT_TOK]`.
    MEASURE_VAR,

    /// `SynMeasure.Paren` — a parenthesised measure `<(m s)>`. Shape
    /// `MEASURE_PAREN > [LPAREN_TOK, <measure>, RPAREN_TOK]`.
    MEASURE_PAREN,

    // -- extern DllImport prototypes (FCS's `cPrototype`, `pars.fsy:3186`) --
    /// `extern` keyword (lexer `Token::Extern`) — introduces an `extern`
    /// DllImport prototype. Leads an [`SyntaxKind::EXTERN_DECL`].
    EXTERN_TOK,

    /// `void` keyword (lexer `Token::Void`) — the C `void` base of an `extern`
    /// prototype C type. Sits in an [`SyntaxKind::EXTERN_RET`] or
    /// [`SyntaxKind::EXTERN_ARG`]; bare return `void` has no suffix, while
    /// `void*` carries a following [`SyntaxKind::STAR_TOK`].
    VOID_TOK,

    /// An `extern` DllImport prototype (FCS's `cPrototype`), lowered by FCS to a
    /// `SynModuleDecl.Let([binding])`. Shape `EXTERN_DECL > [ATTRIBUTE_LIST*,
    /// EXTERN_TOK, EXTERN_RET, ACCESS_TOK?, LONG_IDENT, LPAREN_TOK, (EXTERN_ARG
    /// (COMMA_TOK EXTERN_ARG)*)?, RPAREN_TOK]`. The normaliser projects it to the
    /// same `Let` binding FCS produces (leading keyword `Extern`, a
    /// `LongIdent(name, Pats[Tuple[…]])` head pattern, a synthetic `failwith` RHS).
    EXTERN_DECL,

    /// The return type of an [`SyntaxKind::EXTERN_DECL`] (FCS's `cRetType`). Shape
    /// `EXTERN_RET > [ATTRIBUTE_LIST*, (VOID_TOK | <type>), (& | * | [])*]` — the
    /// optional leading attributes are elided by the normaliser.
    EXTERN_RET,

    /// A single argument of an [`SyntaxKind::EXTERN_DECL`] (FCS's `externArg`).
    /// Shape `EXTERN_ARG > [ATTRIBUTE_LIST*, (VOID_TOK | <type>), (& | * | [])*,
    /// IDENT_TOK?]`: the C type and an optional argument name (absent → an
    /// unnamed `SynPat.Wild`).
    EXTERN_ARG,

    /// A `#`-directive (`#I "/tmp"`, `#load "a.fs"`), FCS's
    /// `SynModuleDecl.HashDirective(ParsedHashDirective(ident, args, _), _)`. Shape
    /// `HASH_DIRECTIVE_DECL > [HASH_TOK, IDENT_TOK, (STRING_LIT | INT32_LIT |
    /// IDENT_TOK)*]`: the directive name (first `IDENT_TOK`) and the argument list
    /// (string / int literals and source identifiers such as
    /// `__SOURCE_DIRECTORY__`). Reuses the existing [`SyntaxKind::HASH_TOK`] for `#`.
    HASH_DIRECTIVE_DECL,

    /// The type of a bare self-constraint `when IFoo<'T>` / `when 'T` (F# 7
    /// IWSAM shorthand, FCS's `SynTypeConstraint.WhereSelfConstrained(ty,
    /// range)`), wrapping the constraint's single [`crate::syntax::Type`]
    /// child. Unlike every other [`SyntaxKind::TYPAR_CONSTRAINT`] form there is
    /// **no** subject typar — the constraint head is an ordinary type, which
    /// may itself be a bare typar (`when 'T`, `when ^T list`). FCS's production
    /// is `appTypeWithoutNull`, so the wrapped type sits at the application
    /// layer: postfix-app and array suffixes are included, but the looser
    /// tuple / arrow / nullable layers are not. The wrapper keeps this type
    /// from being conflated with the subtype form's direct constraint type
    /// (`'a :> T`), which [`crate::syntax::TyparConstraint::ty`] reads. Shape:
    /// `TYPAR_CONSTRAINT > [SELF_CONSTRAINT > [<type>]]`.
    SELF_CONSTRAINT,

    /// The F# 7 typar expression `'T` — a type parameter used as an
    /// *expression*, the head of a statically-resolved (SRTP) member call
    /// `'T.Member`. FCS's `QUOTE ident` atomic production (`pars.fsy:5263`) →
    /// `SynExpr.Typar(SynTypar(id, TyparStaticReq.None, false), range)`. Shape
    /// `[QUOTE_TOK, IDENT_TOK]` — the quote sigil then the typar name. Only the
    /// quote sigil reaches this; a `^`-sigil `^T.M` is FCS's `IndexFromEnd`
    /// (the `^` from-end index prefix), so no `HAT_TOK` variant exists. The
    /// trailing `.Member` / `(args)` chain onto this via the ordinary
    /// [`SyntaxKind::DOT_GET_EXPR`] / [`SyntaxKind::APP_EXPR`] postfix tail.
    TYPAR_EXPR,

    /// Sentinel last variant. Never appears in a tree; exists so
    /// [`SyntaxKind::from_raw`] can range-check.
    #[doc(hidden)]
    __LAST,
}

impl SyntaxKind {
    /// Round-trip a `u16` written by rowan back into a [`SyntaxKind`], or
    /// `None` if the value is outside the enum range.
    pub fn from_raw(raw: u16) -> Option<Self> {
        if raw < SyntaxKind::__LAST as u16 {
            // SAFETY: every value below `__LAST` is a valid `SyntaxKind`
            // discriminant by construction (no explicit values, no gaps).
            Some(unsafe { std::mem::transmute::<u16, SyntaxKind>(raw) })
        } else {
            None
        }
    }

    /// `true` for trivia tokens (whitespace, newlines, comments, the `#line`
    /// / `#nowarn` / `#warnon` directive trivia, the `#if` / `#else` /
    /// `#elif` / `#endif` conditional-compilation directives, and
    /// `#if`-eliminated [`INACTIVECODE`](SyntaxKind::INACTIVECODE) regions) —
    /// the tokens that LexFilter / the preprocessor drop before the parser
    /// sees them, but which a full-fidelity pass splices into the green tree.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            SyntaxKind::WHITESPACE
                | SyntaxKind::NEWLINE
                | SyntaxKind::LINE_COMMENT
                | SyntaxKind::BLOCK_COMMENT
                | SyntaxKind::HASH_LINE
                | SyntaxKind::WARN_DIRECTIVE
                | SyntaxKind::HASH_IF
                | SyntaxKind::HASH_ELSE
                | SyntaxKind::HASH_ELIF
                | SyntaxKind::HASH_ENDIF
                | SyntaxKind::INACTIVECODE
        )
    }
}

/// The language-version interval during which a [`SyntaxKind`] is legal F#
/// syntax: a node of that kind is in surface at `lang` iff
/// `introduced <= lang < removed`.
///
/// This is the seed of the plan's D5 interval table — the single source of
/// truth consulted by both the parser's version gate ([`kind_in_surface`]) and
/// the per-version typed facades (`docs/ast-versioning-plan.md` Stage 4). It is
/// seeded from FCS's `featureVersionMap`
/// (`../fsharp/src/Compiler/Facilities/LanguageFeatures.fs`): a kind's
/// `introduced` is the version of the `LanguageFeature` that first made that
/// syntax legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindInterval {
    /// The first language version at which the kind is legal. `None` ⇒ legal
    /// since before our floor (F# 4.6) — always in surface.
    pub introduced: Option<LanguageVersion>,
    /// The first language version at which the kind is *no longer* legal (it was
    /// removed). `None` ⇒ never removed. No modelled kind is removed today, but
    /// the field is carried from the start so the table format does not churn
    /// when a future version drops a construct (`docs/ast-versioning-plan.md`
    /// D2/D5).
    pub removed: Option<LanguageVersion>,
}

impl KindInterval {
    /// A kind present at every version we model — introduced before the floor,
    /// never removed. The interval of all but the rare post-floor kind.
    pub const ALWAYS: KindInterval = KindInterval {
        introduced: None,
        removed: None,
    };
}

/// The [`KindInterval`] for `kind` — the version range over which a node of that
/// kind is legal F# syntax.
///
/// **Scope today — exact for the committed surfaces.** The published facades are
/// `v8` (F# 8.0) and `v9` (= the union, F# 9.0+), so the only distinction the
/// table must draw is what separates 8.0 from 9.0: a single post-8.0 kind,
/// [`WITH_NULL_TYPE`](SyntaxKind::WITH_NULL_TYPE) (nullness, `string | null`,
/// `LanguageFeature.NullnessChecking`, F# 9.0). Every other modelled kind is in
/// surface at 8.0, so for the committed surfaces — and any pin `>= 8.0` — this
/// one row makes [`kind_in_surface`] exact.
///
/// **Known gap (D3 limitation, pins `< 8.0`).** Some modelled kinds *were*
/// introduced after the 4.6 floor but at or below 8.0 — e.g. `INTERP_STRING_EXPR`
/// (`StringInterpolation`, F# 5.0), `WHILE_BANG_EXPR` (`WhileBang`, 8.0),
/// `INTERSECTION_TYPE` (`ConstraintIntersectionOnFlexibleTypes`, 8.0),
/// `DOT_LAMBDA_EXPR` (`AccessorFunctionShorthand`, 8.0). They are deliberately
/// **not** given rows yet, so [`kind_in_surface`] (and the gate over it)
/// *under-report* them at a pin below their introduction (4.6-7.0). This is the
/// documented unmodelled-older-pin limitation (plan D3) and the "incomplete,
/// never wrong" floor (D7): we decline to *guess* gating we cannot yet verify —
/// version-aware differential testing against FCS is the tracked prerequisite
/// (`docs/completed/ast-versioning-nullness-proof.md`), since each row is a
/// per-feature monotonic-addition (Case A) call. Stage-4 codegen seeds these
/// systematically from FCS `featureVersionMap`.
pub fn kind_interval(kind: SyntaxKind) -> KindInterval {
    match kind {
        // F# 9.0 nullness — `LanguageFeature.NullnessChecking, languageVersion90`.
        SyntaxKind::WITH_NULL_TYPE => KindInterval {
            introduced: Some(LanguageVersion::V9_0),
            removed: None,
        },
        _ => KindInterval::ALWAYS,
    }
}

/// Whether a node of `kind` is in the typed surface at `lang` — i.e. legal at
/// that language version (`introduced <= lang < removed`). The authority the
/// parser's version gate and the facade projections share, so that "the tree
/// holds a node out of surface at `lang`" and "the `vN` projection is not total
/// here" are by construction the same fact (`docs/ast-versioning-plan.md` P2).
pub fn kind_in_surface(kind: SyntaxKind, lang: LanguageVersion) -> bool {
    let interval = kind_interval(kind);
    interval
        .introduced
        .is_none_or(|introduced| lang >= introduced)
        && interval.removed.is_none_or(|removed| lang < removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every discriminant below the `__LAST` sentinel must round-trip
    /// through [`SyntaxKind::from_raw`] back to the same `u16` — this is the
    /// invariant the `transmute` in `from_raw` relies on (no explicit
    /// discriminants, no gaps). Adding variants anywhere before `__LAST`
    /// keeps it holding; this test fails loudly if a future edit breaks it.
    #[test]
    fn from_raw_round_trips_every_kind() {
        for raw in 0..(SyntaxKind::__LAST as u16) {
            let kind = SyntaxKind::from_raw(raw)
                .unwrap_or_else(|| panic!("from_raw({raw}) returned None below __LAST"));
            assert_eq!(kind as u16, raw, "discriminant mismatch for {kind:?}");
        }
    }

    /// Values at or beyond the sentinel are rejected.
    #[test]
    fn from_raw_rejects_out_of_range() {
        assert_eq!(SyntaxKind::from_raw(SyntaxKind::__LAST as u16), None);
        assert_eq!(SyntaxKind::from_raw(u16::MAX), None);
    }

    /// The directive / inactive-code kinds added for the full-trivia driver
    /// mode are classified as trivia, alongside whitespace / comments.
    #[test]
    fn directive_trivia_kinds_are_trivia() {
        assert!(SyntaxKind::HASH_LINE.is_trivia());
        assert!(SyntaxKind::WARN_DIRECTIVE.is_trivia());
        assert!(SyntaxKind::HASH_IF.is_trivia());
        assert!(SyntaxKind::HASH_ELSE.is_trivia());
        assert!(SyntaxKind::HASH_ELIF.is_trivia());
        assert!(SyntaxKind::HASH_ENDIF.is_trivia());
        assert!(SyntaxKind::INACTIVECODE.is_trivia());
    }

    #[test]
    fn non_trivia_kind_is_not_trivia() {
        assert!(!SyntaxKind::INT32_LIT.is_trivia());
        assert!(!SyntaxKind::HASH_TOK.is_trivia());
    }

    /// The interval table agrees with FCS's `featureVersionMap`
    /// (`LanguageFeatures.fs`) for every kind we gate, and treats everything
    /// else as present since before the floor. Today the only gated kind is
    /// nullness (`LanguageFeature.NullnessChecking, languageVersion90`).
    #[test]
    fn interval_table_matches_fcs() {
        assert_eq!(
            kind_interval(SyntaxKind::WITH_NULL_TYPE),
            KindInterval {
                introduced: Some(LanguageVersion::V9_0),
                removed: None,
            },
        );
        // A sample of kinds across the token / type / expression layers all
        // predate the floor, so they are ALWAYS in surface.
        for kind in [
            SyntaxKind::INT32_LIT,
            SyntaxKind::LONG_IDENT_TYPE,
            SyntaxKind::IF_THEN_ELSE_EXPR,
            SyntaxKind::TUPLE_TYPE,
        ] {
            assert_eq!(kind_interval(kind), KindInterval::ALWAYS, "{kind:?}");
        }
    }

    /// `kind_in_surface` honours the interval bounds: nullness is out of surface
    /// below 9.0 and in at 9.0 and above; an always-present kind is in surface at
    /// every version including the floor.
    #[test]
    fn kind_in_surface_honours_intervals() {
        use LanguageVersion::*;
        for v in [V4_6, V7_0, V8_0] {
            assert!(
                !kind_in_surface(SyntaxKind::WITH_NULL_TYPE, v),
                "{v} should exclude nullness",
            );
        }
        for v in [V9_0, V10_0, V11_0, Preview] {
            assert!(
                kind_in_surface(SyntaxKind::WITH_NULL_TYPE, v),
                "{v} should include nullness",
            );
        }
        let mut all = LanguageVersion::NUMBERED.to_vec();
        all.push(Preview);
        for v in all {
            assert!(kind_in_surface(SyntaxKind::INT32_LIT, v), "{v}");
        }
    }

    /// Documents the D3/D7 known gap (see [`kind_interval`]): modelled kinds
    /// introduced after the floor but at or below 8.0 are deliberately *not* yet
    /// gated, so the committed `v8` (8.0) surface stays exact — they are all in
    /// surface at 8.0 — while pins below 8.0 under-report rather than risk
    /// unverified gating. This test locks that intent: giving one of these a row
    /// is a deliberate change that must update this assertion (and should arrive
    /// with the version-aware differential coverage that verifies it).
    #[test]
    fn modelled_post_floor_kinds_not_yet_gated() {
        for kind in [
            SyntaxKind::INTERP_STRING_EXPR,
            SyntaxKind::WHILE_BANG_EXPR,
            SyntaxKind::INTERSECTION_TYPE,
            SyntaxKind::DOT_LAMBDA_EXPR,
        ] {
            assert_eq!(kind_interval(kind), KindInterval::ALWAYS, "{kind:?}");
            // ...so they are in surface even at the floor — the under-report.
            assert!(kind_in_surface(kind, LanguageVersion::V4_6), "{kind:?}");
            // But all are in surface at 8.0, so the committed v8 surface is exact.
            assert!(kind_in_surface(kind, LanguageVersion::V8_0), "{kind:?}");
        }
    }
}

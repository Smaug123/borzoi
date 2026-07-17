//! Post-lex offside rewriter — Rust port of `LexFilter.fs`.
//!
//! Reference: `../fsharp/src/Compiler/SyntaxTree/LexFilter.fs`. The port mirrors
//! FCS's state (a private `Filter` struct with `offside_stack`/`delayed`) and
//! method names (`hw_token_fetch`, `push_ctxt`, `peek_initial`) so the
//! cross-reference is mechanical; the rule bodies use Rust pattern matches
//! rather than F#'s nested-`match` style.
//!
//! Scope of the port (built feature-by-feature, one differential test per
//! commit; see `tests/all/lexfilter_diff/`):
//! - Hard-white mode only (no `#light off` support — this LSP doesn't need it).
//! - Virtual tokens prefixed `Offside*` in FCS get a flat enum ([`Virtual`]).
//! - The outer `LexFilter` wrapper's `OBLOCKEND → OBLOCKEND_COMING_SOON…` /
//!   `RBRACE → RBRACE_COMING_SOON…` swallowing (LexFilter.fs:2828-2839) maps
//!   the would-be virtual tokens to `FSharpTokenKind.None`, so the public-facing
//!   stream omits them. We emit [`Virtual::BlockEnd`] from the LexFilter-impl
//!   level for fidelity; the diff harness drops it before comparing.

use crate::language_version::LanguageVersion;
use crate::lexer::{LexError, Span, Token};

mod balance;
mod continuators;
mod head_transitions;
mod interp_parens;
mod offside_pops;
mod predispatch;
mod pushes;
mod typars_close_op;

#[cfg(test)]
mod position_tests;
#[cfg(test)]
mod undentation_tests;

use typars_close_op::{
    TyparScanAction, classify_typar_scan_token, is_typar_application_trigger, typars_close_op_split,
};

/// 1-based line, 0-based byte column. F# offside compares both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Pos {
    line: u32,
    col: u32,
}

/// FCS's `PositionWithColumn` (LexFilter.fs:582): `undentation_limit`'s
/// result — the minimum column the new context must start at (`col`),
/// paired with the anchor position of the context that imposed the limit
/// (`pos`), which the FS0058 message embeds ("… context started at
/// position (2:5) …"). The two usually agree (`col` is `pos.col` or
/// `pos.col + 1`) but not always: the empty-stack base case pairs `-1`
/// with the *new* context's anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PositionWithColumn {
    pos: Pos,
    col: i32,
}

/// Virtual tokens the offside rewriter can synthesise. Names mirror FCS's
/// `FSharpTokenKind.Offside*` variants — same wire-format so the differential
/// harness can compare without translation.
///
/// Only the variants needed by the currently-ported rules are listed. Extend
/// as the port progresses; the harness panics loudly on an unmapped FCS name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Virtual {
    /// `OLET` — replaces the `let`/`use` that begins an offside binding.
    Let,
    /// `OBINDER` — replaces the `let!`/`use!` that begins an offside
    /// computation-expression binding. Same `CtxtLetDecl` machinery as
    /// [`Virtual::Let`]; only the emitted virtual differs. FCS lexes both
    /// `let!` and `use!` to one `BINDER` token (lex.fsl:363) and the
    /// `let!`-vs-`use!` distinction is recovered downstream from the raw
    /// keyword, exactly as `let`-vs-`use` is. (LexFilter.fs:2166-2170,
    /// ServiceLexing.fs:1417 → `FSharpTokenKind.OffsideBinder`.)
    Binder,
    /// `OAND_BANG` — replaces the `and!` that begins an offside applicative
    /// computation-expression binding. Same `CtxtLetDecl` machinery as
    /// [`Virtual::Binder`], but a *fresh* binding (its own `OBLOCKSEP` and
    /// `ODECLEND`), not a let-continuator like plain `and`. FCS surfaces
    /// `OAND_BANG` as `FSharpTokenKind.None` (ServiceLexing.fs has no
    /// `OAND_BANG` arm), so its public lexer — and therefore the diff harness
    /// — drops it; the real stream the parser consumes still carries it.
    /// (LexFilter.fs:2173-2177.)
    AndBang,
    /// `OBLOCKBEGIN` — opens the RHS of `let _ = …`, `then`/`else` branches, etc.
    BlockBegin,
    /// `OBLOCKEND` — paired with [`Virtual::BlockBegin`]. Emitted internally
    /// but dropped by FCS's outer wrapper, so the diff harness filters it.
    BlockEnd,
    /// `ODECLEND` — closes a top-level declaration.
    DeclEnd,
    /// `OBLOCKSEP` — separates statements aligned at the same offside column
    /// in a `CtxtSeqBlock`. (LexFilter.fs:1912)
    BlockSep,
    /// `ODO` — replaces `do` at the head of a for/while/seq-expression
    /// `do` clause. (LexFilter.fs:2324)
    Do,
    /// `ODO_BANG` — replaces `do!` at the head of a computation-expression
    /// `do!` clause. Same CtxtDo machinery as `do`; only the emitted virtual
    /// differs. (LexFilter.fs:2324, ServiceLexing.fs:1415)
    DoBang,
    /// `OTHEN` — replaces `then` in an `if … then …` construct.
    /// (LexFilter.fs:2477)
    Then,
    /// `OELSE` — replaces `else` in an `if … then … else …` construct.
    /// (LexFilter.fs:2500)
    Else,
    /// `OFUN` — replaces `fun` at the head of a lambda expression.
    /// (LexFilter.fs:2532)
    Fun,
    /// `OFUNCTION` — replaces `function` at the head of a `function | … -> …`
    /// expression. (LexFilter.fs:2475)
    Function,
    /// `OLAZY` — replaces `lazy` when its operand is offside (on a later line)
    /// or a control-flow keyword, in which case a `CtxtSeqBlock` is also pushed
    /// so the whole operand block is the argument. The same-line, non-control
    /// case keeps the raw `Token::Lazy`. (LexFilter.fs:2232-2237,
    /// ServiceLexing.fs → `FSharpTokenKind.OffsideLazy`.)
    Lazy,
    /// `OASSERT` — replaces `assert` under the same `isControlFlowOrNotSameLine`
    /// condition as [`Virtual::Lazy`]; only the emitted virtual differs.
    /// (LexFilter.fs:2232-2237.)
    Assert,
    /// `OEND` — closes a `CtxtFun`, `CtxtMatchClauses`, or `CtxtWithAsLet`
    /// scope. (LexFilter.fs:1525-1528)
    End,
    /// `OWITH` — replaces `with` at the head of a `match … with …` (or
    /// `try … with …`) construct. (LexFilter.fs:2355, ServiceLexing.fs:1412)
    With,
    /// `JOIN_IN` — replaces the `in` keyword when it sits in a query
    /// computation-expression context (`query { … join x in xs … }`), so the
    /// parser sees the join-in infix operator (`declExpr JOIN_IN declExpr`,
    /// `pars.fsy:4669` → `SynExpr.JoinIn`) rather than an ordinary `let … in`
    /// / `for … in` keyword. FCS detects the context purely from the offside
    /// stack — an `in` whose head is `CtxtVanilla` over a brace `CtxtParen`
    /// (skipping intervening seq-block/`do`/`for` contexts), `detectJoinInCtxt`
    /// (LexFilter.fs:747) — so the rewrite is tied to the enclosing `{ … }`,
    /// **not** to the `join`/`on` words (even `query { a in b }` is a
    /// `JoinIn`). Like [`Virtual::With`] this is a *backed-by-raw* relabel: the
    /// raw `Token::In` stays in the stream at the same span, so the parser
    /// emits an `IN_TOK` for it. Surfaces as `FSharpTokenKind.JoinIn`
    /// (ServiceLexing.fs:1508), so the lexfilter diff names it `"JoinIn"`.
    /// (LexFilter.fs:1674.)
    JoinIn,
    /// `OINTERFACE_MEMBER` — replaces the `interface` keyword at the head
    /// of a member-style interface implementation (`interface I with …`
    /// inside a type body or augmentation). Pushes `CtxtInterfaceHead`.
    /// (LexFilter.fs:2569-2570, ServiceLexing.fs:1402)
    InterfaceMember,
    /// `ORIGHT_BLOCK_END` — closes a `SeqBlock(AddOneSidedBlockEnd)`, e.g.
    /// the body of a `->` arrow. Unlike `OBLOCKEND`, this token is *not*
    /// swallowed by FCS's outer wrapper and reaches the parser.
    /// (LexFilter.fs:1539)
    RightBlockEnd,
    /// `HIGH_PRECEDENCE_TYAPP` — inserted by `peek_adjacent_typars` between
    /// an identifier (or `delegate`/numeric literal) and an immediately-
    /// following `<` that opens a generic type application (`f<int>`,
    /// `list<string>`). Tells the parser to bind the type-arg list at higher
    /// precedence than ordinary function application. (LexFilter.fs:2664)
    HighPrecedenceTyApp,
    /// `HIGH_PRECEDENCE_PAREN_APP` — inserted between the closing `>` of a
    /// generic type application and an adjacent `(`, so `f<int>(x)` parses
    /// as one call rather than `(f<int>)(x)`. Also inserted between an
    /// identifier (or other application head) and an adjacent `(` —
    /// `f(x)` — by `insert_high_precedence_app`. (LexFilter.fs:1113-1119,
    /// 2625-2637, 2655)
    HighPrecedenceParenApp,
    /// `HIGH_PRECEDENCE_BRACK_APP` — inserted between an identifier and an
    /// adjacent `[` so `f[i]` parses as one indexer call rather than `f`
    /// followed by a list literal. (LexFilter.fs:2625-2637, 2650)
    HighPrecedenceBrackApp,
}

/// One element of the filtered token stream.
#[derive(Debug, Clone, PartialEq)]
pub enum FilteredToken<'a> {
    Raw(Token<'a>),
    Virtual(Virtual),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddBlockEnd {
    Yes,
    No,
    /// FCS's `AddOneSidedBlockEnd`. Like `Yes` but omits the opening
    /// `OBLOCKBEGIN` and closes with `ORIGHT_BLOCK_END` instead of
    /// `OBLOCKEND`. Used for `->` arrow bodies. (LexFilter.fs:2315)
    OneSided,
}

/// Subset of FCS's `Context` DU. Extended as new rules need each variant.
#[derive(Debug, Clone)]
enum Context {
    SeqBlock {
        first: bool,
        pos: Pos,
        add_block_end: AddBlockEnd,
    },
    LetDecl {
        block_let: bool,
        /// Offside column of the `let` keyword; the LetDecl offside-pop rule
        /// closes the context when an incoming token sits at column ≤ this.
        pos: Pos,
    },
    /// `for x in xs do ...`. Pushed by `FOR`, balances `IN` so the in-token
    /// passes through without force-closing the surrounding LetDecl. `depth`
    /// is the `paren_depth` at push time, mirroring `Fun`, so the RARROW gate
    /// fires when depth returns to the push level rather than requiring zero.
    /// (LexFilter.fs:2516)
    For { pos: Pos, depth: u32 },
    /// `do ...` clause (also `do!`). Pushed alongside a
    /// `SeqBlock(AddBlockEnd)` for the body. Closes with `ODECLEND` at EOF
    /// or when something is offside. (LexFilter.fs:2324)
    Do { pos: Pos },
    /// `while … do …` loop header. Pushed by WHILE, balances nothing
    /// (unlike For which balances IN). `depth` is the `paren_depth` at push
    /// time so the RARROW gate fires correctly inside parenthesized
    /// comprehensions. Pops silently when offside. (LexFilter.fs:2521)
    While { pos: Pos, depth: u32 },
    /// `fun … -> …` lambda. Pushed by FUN, emits OFUN. Closes with OEND
    /// when offside. `depth` is the `paren_depth` at push time so the RARROW
    /// gate fires when paren depth returns to that level rather than
    /// requiring global depth zero. (LexFilter.fs:2532, 2055)
    Fun { pos: Pos, depth: u32 },
    /// `function | … -> …` expression. Pushed by FUNCTION (LexFilter.fs:
    /// 2469-2475) alongside an inner `CtxtMatchClauses` anchored at the
    /// lookahead token; the FUNCTION token itself becomes `OFUNCTION`.
    /// Unlike `CtxtFun`, this context pops *silently* on offside —
    /// `endTokenForACtxt` is `_ -> None` (LexFilter.fs:1545) and the offside
    /// arm (L2068) does `popCtxt + reprocess`. The inner CtxtMatchClauses
    /// (and the OneSided SeqBlock its `->` pushes) provide the OEND /
    /// ORIGHT_BLOCK_END virtuals that surface on close.
    Function { pos: Pos },
    /// `if …` scope. Pushed by IF/ELIF, balances ELSE/ELIF so the inner
    /// then-body SeqBlock + CtxtThen can be force-closed when ELSE arrives.
    /// (LexFilter.fs:2506)
    If { pos: Pos },
    /// `… then …` scope. Pushed alongside a `SeqBlock(AddBlockEnd)` for the
    /// then-body. Pops silently — its job is to keep CtxtIf reachable via
    /// `suffixExists` so ELSE can force-close the then-body.
    /// (LexFilter.fs:2477)
    Then { pos: Pos },
    /// `else …` scope. Pushed alongside a `SeqBlock(AddBlockEnd)` for the
    /// else-body. Pops silently at EOF or when offside.
    /// (LexFilter.fs:2500)
    Else { pos: Pos },
    /// `match …` scrutinee scope. Pushed by `MATCH`/`MATCH_BANG`; passthrough
    /// (no virtual emitted). Closed silently by its offside-pop (LexFilter.fs:
    /// 2031). `WITH` balances it (LexFilter.fs:1266) so the trailing `with`
    /// arm dispatches without force-closing CtxtMatch.
    Match { pos: Pos },
    /// `with | … | …` match-arm scope. Pushed by the `WITH+CtxtMatch` arm
    /// (LexFilter.fs:2347); anchored at the *lookahead* token's column (the
    /// first token after `with`, typically the leading `|`). `leading_bar`
    /// records whether that lookahead is `|`, which shifts the BAR/END offside
    /// guard so a leading `|` doesn't immediately pop the context.
    /// (LexFilter.fs:2099-2113)
    MatchClauses { leading_bar: bool, pos: Pos },
    /// `when` guard scope inside a match arm. Pushed by `WHEN+CtxtSeqBlock`
    /// (LexFilter.fs:2526); passthrough (no virtual emitted). Closed silently
    /// by its offside-pop (LexFilter.fs:2049). Listed in the RARROW push gate
    /// so `pat when guard -> body` opens a OneSided block at the arrow.
    When { pos: Pos },
    /// Ordinary-expression context pushed on top of a `CtxtSeqBlock` for any
    /// real token that doesn't otherwise match a dispatch arm.
    /// (LexFilter.fs:2617) Passthrough (no virtual emitted); closes silently
    /// by its offside-pop (LexFilter.fs:1868) on `tokenStartCol <= pos.col`.
    ///
    /// Critical for the RARROW push gate: when a match-arm body starts (e.g.
    /// the `n` in `| n -> n | _ -> 0`), this CtxtVanilla sits on top of the
    /// arm's `SeqBlock(AddOneSidedBlockEnd)` and prevents the *next* `->`
    /// from re-firing the RARROW gate (which only triggers on
    /// MatchClauses/When/Fun/For/While heads). Without it, both arms would
    /// open a OneSided SeqBlock and EOF would emit one too many
    /// ORIGHT_BLOCK_END.
    ///
    /// FCS's `CtxtVanilla` carries an `isLongIdentEquals` boolean
    /// (LexFilter.fs:45) used only by the EQUALS+CtxtVanilla+CtxtWithAsLet
    /// arm (LexFilter.fs:2254): when the ordinary token at the start of a
    /// record-update binding is recognised as the head of `IDENT (DOT IDENT)*
    /// EQUALS`, the subsequent `=` opens an inner SeqBlock so that the
    /// binding's RHS forms its own offside scope. Set by
    /// `is_long_ident_equals` on the Vanilla push (LexFilter.fs:2618).
    Vanilla {
        pos: Pos,
        is_long_ident_equals: bool,
    },
    /// Parenthesis scope pushed by any `TokenLExprParen` token so the
    /// matching `TokenRExprParen` can force-close inner SeqBlock/CtxtFun
    /// contexts before balancing. `opener` records which bracket opened this
    /// scope so `parenTokensBalance` can check the correct pair.
    /// (LexFilter.fs:2282, 408)
    Paren { pos: Pos, opener: Opener },
    /// `try …` scope. Pushed by `TRY` alongside an inner
    /// `SeqBlock(AddOneSidedBlockEnd)` for the try-body. Balanced by `WITH`
    /// and `FINALLY` (LexFilter.fs:1266, 1269) so the surrounding force-
    /// closure stops at this context rather than burning through to an outer
    /// CtxtMatch. Pops silently on offside (LexFilter.fs:2073);
    /// `isTryBlockContinuator` (LexFilter.fs:236) lets aligned
    /// WITH/FINALLY/reprocessed-virtuals keep the construct open until the
    /// balance-driven dispatch fires. (LexFilter.fs:2589)
    Try { pos: Pos },
    /// `with` as used inside a record-update / anonymous-record /
    /// object-expression body (`{ r with A = 1 }`, `{| r with … |}`,
    /// `{ new I with M() = 1 }`). Pushed by the brace-shape WITH dispatch
    /// (LexFilter.fs:2363). Anchored at either the `with` keyword (when the
    /// first arm sits on the same line) or the surrounding SeqBlock's column
    /// (when the body wraps onto a new line) — see L2381-2401 for the rule.
    /// Closes with OEND, either via offside-pop (LexFilter.fs:2019) or via
    /// force-closure when the `}` closer arrives. `endTokenForACtxt` returns
    /// `Some(OEND)` (LexFilter.fs:1527).
    ///
    /// FCS folds five other lim-context variants (CtxtException /
    /// CtxtTypeDefns / CtxtMemberHead / CtxtInterfaceHead / CtxtMemberBody)
    /// and the `CtxtWithAsAugment` companion (LexFilter.fs:2362, 2458) into
    /// the same dispatch arm.
    WithAsLet { pos: Pos },
    /// `namespace Foo.Bar` head. Pushed by the NAMESPACE arm and consumed
    /// while the dotted-ident continues. `prev` tracks whether the last
    /// head token was a keyword/dot or an IDENT so the transition rule
    /// (LexFilter.fs:1726) can decide whether to accept the next token as
    /// continuation or transition to `CtxtNamespaceBody` + SeqBlock.
    NamespaceHead { pos: Pos, prev: NamespacePrev },
    /// `namespace Foo.Bar` body — everything after the dotted ident, up to
    /// EOF or the next `namespace` declaration. Pushed by the
    /// CtxtNamespaceHead transition (LexFilter.fs:1743). Offside-pop at
    /// LexFilter.fs:1985 fires only on NAMESPACE (the only non-continuator
    /// token aside from EOF).
    NamespaceBody { pos: Pos },
    /// `module Foo` / `module rec Bar` head. Pushed by the MODULE arm —
    /// the MODULE token itself is swallowed (FCS uses `pool.Return` +
    /// `hwTokenFetch`, no emit; MODULE_COMING_SOON / MODULE_IS_HERE faux
    /// tokens map to `FSharpTokenKind.None` and are filtered by the
    /// public-API tokenizer).
    ///
    /// `prev` tracks the head-state machine (LexFilter.fs:1752); `attrs`
    /// tracks whether we're inside a `[< ... >]` block of module
    /// attributes; `nested` records whether this is a nested module (used
    /// by `end_token_for_a_ctxt` to emit OBLOCKSEP on force-closure of
    /// incomplete nested heads, LexFilter.fs:1542-1543).
    ModuleHead {
        pos: Pos,
        prev: ModuleHeadPrev,
        attrs: bool,
        nested: bool,
    },
    /// `module Foo = …` / whole-file module body. Pushed by the
    /// CtxtModuleHead transition (LexFilter.fs:1774, 1789).
    /// `whole_file=true` means the file was a single module declaration
    /// without `=` / `:` — the body extends to EOF (LexFilter.fs:1789).
    /// Offside-pop at LexFilter.fs:1979.
    ModuleBody { pos: Pos, whole_file: bool },
    /// `type T = …` definition scope. Pushed by the TYPE keyword arm
    /// (LexFilter.fs:2579-2587), which swallows TYPE itself (FCS
    /// rewrites it as TYPE_COMING_SOON / TYPE_IS_HERE via
    /// `insertComingSoonTokens` — both map to `FSharpTokenKind.None`
    /// and are filtered by the public-API tokenizer). `equals_end`
    /// records the end-position of the `=` token; FCS uses it later
    /// to disambiguate class/struct/interface body openers via the
    /// `replaceCtxtIgnoreIndent` at LexFilter.fs:2228, but we only
    /// store it for forwards-compatibility — no arm consumes it yet.
    /// Offside-pop at LexFilter.fs:1966; `isTypeContinuator` allows
    /// `AND` / `WITH` / `BAR` / `END` / `}` to align with the type
    /// keyword without closing the construct.
    TypeDefns {
        pos: Pos,
        /// Captured at parity with FCS's `equalsEndPos` slot but not yet
        /// consumed — class/struct/interface body dispatch (LexFilter.fs:
        /// 948, 2537) is the consumer and hasn't been ported.
        #[allow(dead_code)]
        equals_end: Option<Pos>,
    },
    /// `CtxtMemberHead` — the prelude of a member declaration spanning the
    /// keyword(s) (`val`, `static`, `abstract`, `member`, `override`,
    /// `default`, `new`, with optional access modifier) up to the `=` that
    /// transitions into `CtxtMemberBody`. Offside-pop is silent-then-reprocess
    /// (LexFilter.fs:2007-2011). FCS only stores the head's anchor position;
    /// the prelude grammar runs in the ordinary token stream.
    MemberHead { pos: Pos },
    /// `CtxtMemberBody` — the right-hand side of a member declaration after
    /// `=`. Offside-pop emits `ODECLEND` (LexFilter.fs:1995-2005). A
    /// subsequent member keyword (VAL/STATIC/MEMBER/OVERRIDE/DEFAULT/
    /// ABSTRACT) while a `MemberBody` is on the stack triggers a multi-pop
    /// cascade up to and including this context (LexFilter.fs:2179-2195).
    MemberBody { pos: Pos },
    /// `CtxtWithAsAugment` — `with` opening a type augmentation, interface
    /// implementation block, exception augmentation, or a property
    /// accessor block when the binding head is not `IDENT`/etc.
    /// (LexFilter.fs:40, 2458, 2465). Two closure paths:
    ///   * Offside-pop (LexFilter.fs:2025-2029): END or anything left of
    ///     the anchor → pop and emit `ODECLEND`.
    ///   * Dedicated END balance arm (LexFilter.fs:1717-1722): END at or
    ///     right of the anchor → pop, delay `ODUMMY END`, emit `OEND`.
    ///
    /// `end_token_for_a_ctxt` returns `ODECLEND` so the MEMBER pop-cascade
    /// (LexFilter.fs:2185) emits ODECLEND when it unwinds through this
    /// context. `isWithAugmentBlockContinuator` is END only (L383-392) and
    /// gates the offside guard so an aligned `end` does not trigger the
    /// offside path.
    WithAsAugment { pos: Pos },
    /// `CtxtException` — `exception` declaration head (LexFilter.fs:62).
    /// Pushed by the `EXCEPTION` keyword (LexFilter.fs:2135-2141), pops
    /// silently on offside or `;;` (LexFilter.fs:1990). `endTokenForACtxt`
    /// is the default `None` arm, so the pop never emits a virtual.
    /// Observable only via downstream dispatch: a `WITH` while Exception
    /// is head reaches the L2362 arm (instead of the L2462 catch-all),
    /// which for `IDENT`-lookahead emits `OWITH` and pushes
    /// `CtxtWithAsLet` anchored at Exception's column.
    Exception { pos: Pos },
    /// `CtxtInterfaceHead` — head of an interface implementation in a
    /// member-style position, i.e. `interface I with …` inside a class
    /// body or type augmentation. Pushed by the `INTERFACE` catch-all
    /// (LexFilter.fs:2567-2570), which also rewrites the keyword to
    /// `OINTERFACE_MEMBER`. The companion paren-form arm
    /// (`type I = interface … end`, LexFilter.fs:2536) pushes
    /// `CtxtParen(INTERFACE, …)` instead and is handled separately.
    ///
    /// Pops silently on offside or `;;` (LexFilter.fs:1960). The
    /// `isInterfaceContinuator` predicate (LexFilter.fs:266) treats
    /// `END` / reprocessed `OBLOCKEND` / `ORIGHT_BLOCK_END` / `ODECLEND`
    /// as continuators, so they can align at the InterfaceHead's column
    /// without forcing it closed (an explicit `end` clause closes via
    /// the inner WithAsAugment instead). `endTokenForACtxt` is the
    /// default `None` arm — the pop emits no virtual at the
    /// InterfaceHead's range.
    ///
    /// Downstream dispatch divergences (relative to the L2462 WITH
    /// catch-all): the L2362 host-context arm fires when InterfaceHead
    /// is head, opening either `CtxtWithAsLet` (binding-head lookahead)
    /// or `CtxtWithAsAugment` anchored at the InterfaceHead's column
    /// (other lookahead). A dedicated L2436 recovery arm covers the
    /// case where the lookahead column is at or left of InterfaceHead's
    /// column: emit raw `WITH` and push nothing, so the next token
    /// participates in the surrounding SeqBlock as a sibling.
    InterfaceHead { pos: Pos },
}

/// State of the `CtxtNamespaceHead` dotted-ident scanner. Mirrors which
/// FCS pattern the previous head token matched (LexFilter.fs:1726-1733).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamespacePrev {
    /// NAMESPACE keyword, DOT, REC, or GLOBAL — accepts REC / IDENT / GLOBAL.
    Keyword,
    /// IDENT — accepts DOT.
    Ident,
}

/// State of the `CtxtModuleHead` dotted-ident scanner. Mirrors the
/// `prevToken` slot of FCS's `CtxtModuleHead` (LexFilter.fs:53, 1752).
/// FCS stores the previous *token* and then matches against literal
/// tokens; we collapse equivalent transitions into the four distinct
/// accept-set states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModuleHeadPrev {
    /// MODULE keyword (initial state; persists across access modifiers
    /// PUBLIC/PRIVATE/INTERNAL — those pass through without changing prev).
    /// Accepts GLOBAL / REC / IDENT plus the access-modifier and `[<`
    /// attribute openers.
    Module,
    /// REC keyword or DOT — accepts REC / IDENT (replace).
    RecOrDot,
    /// IDENT — accepts DOT (replace).
    Ident,
    /// GLOBAL — dead end (only EQUALS/COLON or fall-through transitions).
    /// FCS stores `prevToken := GLOBAL` after the `MODULE, GLOBAL` arm
    /// (LexFilter.fs:1763) and no subsequent accept-pattern matches
    /// prev=GLOBAL.
    Global,
}

impl Context {
    /// Mirrors FCS's `Context.StartPos` (LexFilter.fs:62-77). Used by
    /// `undentation_limit` and any caller that needs the context's anchor.
    fn start_pos(&self) -> Pos {
        match self {
            Context::SeqBlock { pos, .. }
            | Context::LetDecl { pos, .. }
            | Context::For { pos, .. }
            | Context::Do { pos, .. }
            | Context::While { pos, .. }
            | Context::Fun { pos, .. }
            | Context::Function { pos, .. }
            | Context::If { pos, .. }
            | Context::Then { pos, .. }
            | Context::Else { pos, .. }
            | Context::Match { pos, .. }
            | Context::MatchClauses { pos, .. }
            | Context::When { pos, .. }
            | Context::Vanilla { pos, .. }
            | Context::Paren { pos, .. }
            | Context::Try { pos, .. }
            | Context::WithAsLet { pos, .. }
            | Context::NamespaceHead { pos, .. }
            | Context::NamespaceBody { pos, .. }
            | Context::ModuleHead { pos, .. }
            | Context::ModuleBody { pos, .. }
            | Context::TypeDefns { pos, .. }
            | Context::MemberHead { pos, .. }
            | Context::MemberBody { pos, .. }
            | Context::WithAsAugment { pos, .. }
            | Context::Exception { pos, .. }
            | Context::InterfaceHead { pos, .. } => *pos,
        }
    }
}

/// Which `TokenLExprParen` token opened a `Context::Paren`. Mirrors the
/// `parenTokensBalance` pairs in LexFilter.fs:408-426 for the subset we
/// currently track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opener {
    Paren,    // `(`    closes with `)`   (RPAREN — swallowed by outer wrapper)
    Brace,    // `{`    closes with `}`   (RBRACE — swallowed by outer wrapper)
    Brack,    // `[`    closes with `]`   (RBRACK — emitted)
    BrackBar, // `[|`   closes with `|]`  (BAR_RBRACK — emitted)
    BraceBar, // `{|`   closes with `|}`  (BAR_RBRACE — emitted)
    Begin,    // `begin` closes with `end` (END — emitted). Per parenTokensBalance
    // (LexFilter.fs:414-424) END also pairs with CLASS/SIG/STRUCT/INTERFACE,
    // all of which likewise push CtxtParen.
    Sig, // `sig`   closes with `end` (END — emitted). Unconditional in
    // FCS, folded into the same arm as TokenLExprParen. Inner SeqBlock is
    // NoAddBlockEnd. (LexFilter.fs:2281)
    Class, // `class` closes with `end` (END — emitted). Unconditional in
    // FCS. Inner SeqBlock is AddBlockEnd (unlike Begin/Sig).
    // (LexFilter.fs:2573)
    Struct, // `struct` closes with `end` (END — emitted). GUARDED in FCS
    // (LexFilter.fs:2291-2302) on `CtxtSeqBlock :: (CtxtModuleBody |
    // CtxtTypeDefns) :: _` so `<'a when 'a : struct>` (typar constraint)
    // does not push a CtxtParen. Inner SeqBlock is NoAddBlockEnd.
    Interface, // `interface` closes with `end` (END — emitted). GUARDED in
    // FCS (LexFilter.fs:2537-2564) on `CtxtSeqBlock :: CtxtTypeDefns(_,
    // Some equalsEndPos) :: _` with INTERFACE immediately following `=`
    // and a lookahead-constrained next token. Inner SeqBlock is
    // AddBlockEnd (unlike Struct/Begin/Sig). The catch-all L2568
    // (`type C with interface ... with`) push is handled by the
    // INTERFACE-keyword arm that emits `Virtual::InterfaceMember` and
    // pushes `Context::InterfaceHead`.
    Quote,    // `<@`   closes with `@>`  (RQUOTE — emitted)
    QuoteRaw, // `<@@`  closes with `@@>` (RQUOTE — emitted)
    /// `<` opening a generic type application, after `peek_adjacent_typars`
    /// rewrites the bare `Less(false)` to `Less(true)`. Closes with
    /// `Greater(true)` (emitted). FCS treats `LESS true` / `GREATER true`
    /// as `TokenLExprParen` / `TokenRExprParen` so the typar angle
    /// suppresses inner offside SeqBlock separators and force-closes any
    /// inner CtxtSeqBlock/CtxtFun on close (LexFilter.fs:188, 196).
    TyparAngle,
    /// `$"…{` / `$"""…{` (a [`crate::lexer::InterpKind::Begin`] or
    /// [`crate::lexer::InterpKind::TripleBegin`] fragment) or `}…{`
    /// (a [`crate::lexer::InterpKind::Part`] fragment): the opener of
    /// an interpolation fill. Closes with [`crate::lexer::InterpKind::End`]
    /// (`}…"` / `}…"""`) or another [`crate::lexer::InterpKind::Part`]
    /// for multi-fill chains. The single-/triple-quoted distinction is
    /// invisible to LexFilter — fill body offside semantics are the
    /// same — so we share one `InterpFill` opener for both. FCS keeps
    /// these as a separate `CtxtParen(INTERP_STRING_…)` case
    /// (LexFilter.fs:2281-2287, 1697-1714) and treats the inner fill
    /// as not-offside-limited (LexFilter.fs:997-998).
    InterpFill,
}

impl Opener {
    /// FCS's `TokenLExprParen` active pattern (LexFilter.fs:186-190):
    /// BEGIN, LPAREN, LBRACE, LBRACE_BAR, LBRACK, LBRACK_BAR, LQUOTE, LESS
    /// true. Used by `undentationLimit` arms L844-845 / L849-850 to make the
    /// opener transparent — `let x = (\n    body\n)` with `body` left of `(`
    /// is legal, the body's offside being gated by `let` rather than `(`.
    /// SIG and CLASS push CtxtParen too but are *not* TokenLExprParen.
    ///
    /// `InterpFill` is deliberately **excluded**, matching FCS's
    /// `TokenLExprParen` (which omits `INTERP_STRING_BEGIN_PART` /
    /// `INTERP_STRING_PART`). FCS makes an interpolation-fill paren
    /// transparent *only* in a non-strict `undentationLimit` walk — via the
    /// generic `CtxtParen _ :: rest when not strict` arm (LexFilter.fs:786) —
    /// and keeps it *opaque* under a strict walk, where it falls to the
    /// catch-all (`CtxtParen _ -> col`, LexFilter.fs:988) so the fill body is
    /// limited by the paren's own column (the byte just after the interpolation
    /// opener) rather than by the enclosing `let`/`module`. Our non-strict
    /// `!strict && matches Paren` arm already grants the same non-strict
    /// transparency, so `InterpFill` needs no help here — and including it made
    /// the strict `isCorrectIndent` walk wrongly recurse to the enclosing
    /// context, spuriously flagging every multi-line interpolation body as
    /// offside once the FS0058 emission stage (§A) started consulting it.
    fn is_token_l_expr_paren(self) -> bool {
        matches!(
            self,
            Opener::Paren
                | Opener::Brace
                | Opener::Brack
                | Opener::BrackBar
                | Opener::BraceBar
                | Opener::Begin
                | Opener::Quote
                | Opener::QuoteRaw
                | Opener::TyparAngle
        )
    }
}

#[derive(Debug, Clone)]
enum TokenContent<'a> {
    Real(Token<'a>),
    Err(LexError),
    /// Synthetic EOF — emitted once the underlying iterator is exhausted, then
    /// repeatedly until the offside stack drains.
    Eof,
    Virtual(Virtual),
    /// FCS's `ODUMMY token` (LexFilter.fs:2608). A pop-trigger: queued after
    /// an `IN`/`DONE`/`RPAREN`/`END` pop so any cascading context-close rules
    /// get a chance to fire, then silently discarded. `inner` carries the
    /// originating real token so the continuator predicates can recursively
    /// unwrap (`isSeqBlockElementContinuator (ODUMMY t) =
    /// isSeqBlockElementContinuator t`, FCS L382, and analogous arms in
    /// `isLetContinuator` / `isTypeContinuator` / `isTypeSeqBlockElement
    /// Continuator` / `isWithAugmentBlockContinuator`). The motivating case is
    /// the END+CtxtWithAsAugment balance arm (L1717-1722): without inner-token
    /// awareness the queued Dummy fires a spurious OBLOCKSEP on the
    /// surrounding SeqBlock instead of being suppressed as a continuator.
    ///
    /// `prev_end` is a snapshot of `last_real_end` at queue time. FCS's
    /// `TokenTup` carries its own `LastTokenPos` field, so an ODUMMY built
    /// via `pool.UseLocation(tokenTup, ODUMMY token)` inherits the
    /// originating token's `LastTokenPos` even after `tokenTup` itself is
    /// emitted (and lastTokenPos advances). When a Dummy triggers the
    /// `SeqBlock(NotFirst)` OBLOCKSEP rule
    /// (`insertTokenFromPrevPosToCurrentPos`, LexFilter.fs:1365-1373) the
    /// OBLOCKSEP span uses the Dummy's preserved prev-end, not the
    /// filter's current `last_real_end`. The motivating case is a
    /// `Greater(true)` closer: the Dummy queued at Greater's position
    /// fires OBLOCKSEP between Greater and the next token, and the
    /// OBLOCKSEP span must point at the whitespace BEFORE Greater (i.e.
    /// from the prior real token's end), not the empty span after it.
    Dummy {
        prev_end: usize,
        inner: Box<Token<'a>>,
    },
}

#[derive(Debug, Clone)]
struct TokenTup<'a> {
    token: TokenContent<'a>,
    span: Span,
    start: Pos,
    end: Pos,
}

/// Control-flow outcome of a single `hw_token_fetch` dispatch rule. A rule
/// either passes the still-unconsumed token to the next rule, restarts the
/// dispatch loop, or yields a token to emit.
enum Step<'a> {
    /// Rule did not apply; the untouched token flows to the next rule.
    Pass(TokenTup<'a>),
    /// Rule consumed or delayed the token; restart the dispatch loop.
    Restart,
    /// Rule produced a token to emit from `hw_token_fetch`.
    Emit(TokenTup<'a>),
}

/// Apply the offside-rule filter to a raw lexer stream. Trivia (whitespace,
/// newlines, comments) is consumed internally for position tracking and does
/// not appear in the output.
///
/// `source` is the original text the token spans index into; the filter scans
/// each token's bytes to keep its line/column cursor honest across multi-line
/// tokens (block comments, triple/verbatim/continuation strings).
/// One element of the filtered stream: the (possibly-[`Virtual`]) token — or a
/// lexer error passed through from the input — together with its byte span.
pub type FilteredItem<'a> = (Result<FilteredToken<'a>, LexError>, Span);

pub fn filter<'a, I>(source: &'a str, tokens: I) -> impl Iterator<Item = FilteredItem<'a>>
where
    I: Iterator<Item = (Result<Token<'a>, LexError>, Span)>,
{
    // The streaming entry point the lex-filter differential tests consume as a
    // pure token stream. It cannot surface the diagnostics accumulator (it moves
    // the `Filter` into an opaque `impl Iterator`), so it pins the default
    // language version — the same "latest" surface the offside arithmetic has
    // always assumed. A caller that needs the offside diagnostics uses
    // [`filter_collect`] instead.
    Filter::new(source, LanguageVersion::DEFAULT, tokens)
}

/// Run the offside filter to completion, returning the full token stream, the
/// offside/indentation diagnostics ([`OffsideDiagnostic`], all FS0058) it
/// accumulated, **and** whether the stream's shape depends on the language
/// version ([`FilterRun::shape_depends_on_language_version`]). The streaming
/// [`filter`] cannot return these (it hands the `Filter` out as an opaque
/// iterator), so the parser — which needs them — uses this. `lang` resolves
/// the severity/presence gates
/// ([`LanguageVersion::strict_indentation_is_error`],
/// [`LanguageVersion::reports_invalid_decls_in_type_definitions`]).
pub fn filter_collect<'a, I>(source: &'a str, lang: LanguageVersion, tokens: I) -> FilterRun<'a>
where
    I: Iterator<Item = (Result<Token<'a>, LexError>, Span)>,
{
    let mut f = Filter::new(source, lang, tokens);
    let tokens: Vec<FilteredItem<'a>> = f.by_ref().collect();
    FilterRun {
        tokens,
        diagnostics: f.diagnostics,
        shape_depends_on_language_version: f.shape_depends_on_language_version,
    }
}

/// A completed [`filter_collect`] run: the filtered stream plus the run's
/// side outputs.
pub struct FilterRun<'a> {
    /// The filtered token stream, in emission order.
    pub tokens: Vec<FilteredItem<'a>>,
    /// The offside / indentation diagnostics (FS0058 family) accumulated
    /// during the run.
    pub diagnostics: Vec<OffsideDiagnostic>,
    /// Whether this run reached a decision point whose outcome differs
    /// across language versions — today exactly the strict-indentation gate
    /// (F# 8, [`LanguageVersion::strict_indentation_is_error`]): a
    /// version-gated context push whose anchor is offside is *aborted* at
    /// F# 8+ but *kept* (with a warning) below, so everything after nests
    /// differently. `false` **proves** the filtered stream — and therefore
    /// the parse tree — is identical under every [`LanguageVersion`]: two
    /// hypothetical runs under different versions produce identical streams
    /// up to their first divergence point, that point is reached in the same
    /// state by both, and it is by construction a version-gated offside push
    /// — exactly where this flag is set. Consumers that don't know the
    /// project's real language version (an untrusted `<LangVersion>`
    /// provenance) can therefore trust a `false`-flagged parse outright.
    ///
    /// `true` is a sound **over**-approximation: the differing stack
    /// operations can reconverge to the identical stream (an EOF-anchored
    /// push's abort is largely absorbed by the EOF force-closure cascade —
    /// `module M =` at end of file, a common mid-edit state). A consumer for
    /// which a false positive is costly should verify by comparing a parse
    /// from the other side of the boundary (strictness is the only shape
    /// input, so one representative per side decides) before acting.
    pub shape_depends_on_language_version: bool,
}

/// Severity of a lex-filter offside/indentation diagnostic. FCS reports every
/// one of these as **FS0058** (they funnel through its `IndentationProblem`),
/// but with either warning or (recoverable) error severity depending on the
/// specific problem and the language version — see [`OffsideDiagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsideSeverity {
    Warning,
    Error,
}

/// An offside / indentation problem the lex-filter detected — FCS's FS0058
/// family (the general "token is offside of context started earlier", the
/// nested-declaration-in-type checks, `in`-misindentation, and pattern-match
/// `|`-misalignment). Carries the final human-facing `message`, the byte `span`
/// FCS would report the squiggle at, and the resolved `severity`. The parser
/// merges these into [`crate::parser::Parse`]'s `errors` / `warnings`.
///
/// Emission is added by later stages of `docs/offside-diagnostics-plan.md`; the
/// accumulator exists (and is always empty) as of the infrastructure stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OffsideDiagnostic {
    pub message: String,
    pub span: Span,
    pub severity: OffsideSeverity,
}

struct Filter<'a, I> {
    raw: I,
    /// Original source the token spans index into; scanned by `pull_raw` to
    /// count embedded newlines for the line/column cursor.
    source: &'a str,
    /// LIFO stack of tokens to consume before pulling from `raw`. FCS's
    /// `delayedStack`.
    delayed: Vec<TokenTup<'a>>,
    offside_stack: Vec<Context>,
    /// Parallel to `offside_stack`: `undentation_skip[i]` is the index of the
    /// nearest context at-or-below `i` that is *not* "pure-skip" — i.e. the one
    /// `undentation_limit`'s non-strict walk would stop at — or `u32::MAX` when
    /// every context at-or-below `i` is pure-skip. Maintained O(1) on push/pop
    /// ([`Self::push_undentation_skip`]); lets `undentation_limit` jump over deep
    /// transparent runs (nested delimiters / lambdas) in O(1) instead of
    /// re-walking them, keeping the offside computation linear rather than
    /// quadratic in nesting depth. See [`Self::is_pure_skip`].
    undentation_skip: Vec<u32>,
    initialized: bool,
    eof_pulled: bool,
    /// 1-based current line, updated as Newline tokens pass through.
    cur_line: u32,
    /// Byte offset of the current line's first byte.
    cur_line_start_byte: usize,
    /// `(line, start_byte)` for every line that has hosted a token start, in
    /// ascending line order. Line 1 is seeded at construction (past a
    /// file-start BOM, matching the column baseline below); [`Self::pull_raw`]
    /// appends as it crosses newlines. Lines interior to a multi-line token
    /// are absent — no token starts there, so no [`Context`] can anchor
    /// there. Consulted by [`Self::utf16_col`] to convert a [`Pos`]'s byte
    /// column into the UTF-16 column FCS embeds in the FS0058 message.
    line_starts: Vec<(u32, usize)>,
    /// Last byte we've observed in the raw stream — used as the EOF span.
    last_byte: usize,
    /// Depth of all `TokenLExprParen` openers seen so far minus their closers.
    /// Mirrors the depth of `CtxtParen` entries on the stack. Used to gate
    /// the RARROW rule: `Context::Fun { depth }` records the depth at push
    /// time so that `->` inside `(g: int -> int)` doesn't open a body block.
    paren_depth: u32,
    /// End byte offset of the last real (non-virtual, non-trivia) token
    /// returned from `Iterator::next`. Used by `insert_token_from_prev_to_current`
    /// to compute the synthetic start of OBLOCKSEP: FCS uses prevEnd+1.
    last_real_end: usize,
    /// Whether the last real token returned from `Iterator::next` was an
    /// [`is_atomic_expr_end`] token. FCS's `prevWasAtomicEnd`: a glued-left op
    /// after such a token is infix, so the offside `ADJACENT_PREFIX_OP` rule must
    /// not treat it as a term-starter.
    last_real_was_atomic_end: bool,
    /// FCS's `tokensThatNeedNoProcessingCount` (LexFilter.fs:696). Tokens
    /// pushed via `delay_token_no_processing` increment this; the next N
    /// `pop_next_token_tup` results bypass dispatch and are returned raw.
    /// Currently the sole user is the multi-member pop cascade
    /// (LexFilter.fs:2179-2195) — synthetic END tokens and the saved
    /// trigger keyword flow straight through without re-triggering offside
    /// rules. FCS additionally uses it for the COMING_SOON/IS_HERE
    /// generation around MODULE/TYPE/RBRACE/RPAREN/OBLOCKEND
    /// (LexFilter.fs:1577-1636, 2828-2839); we don't model those synthetic
    /// tokens because they all map to `FSharpTokenKind.None` and are
    /// filtered out of the FCS public-API stream we diff against.
    tokens_that_need_no_processing: usize,
    /// FCS's `strictIndentation` (LexFilter.fs:766): resolved from the language
    /// version at construction (F# 8+ / `--strict-indentation+`). It governs two
    /// things in lock-step, as in FCS:
    ///  * the severity of an offside FS0058 — *error* when set, *warning* below;
    ///  * whether an offside context push is **aborted** (set) or **kept** (unset),
    ///    which changes the resulting tree (a following construct nests vs. becomes
    ///    a sibling).
    ///
    /// See [`LanguageVersion::strict_indentation_is_error`].
    strict_indentation_is_error: bool,
    /// Whether the nested-declaration-in-type FS0058 problems are reported at
    /// all (F# 10+). See
    /// [`LanguageVersion::reports_invalid_decls_in_type_definitions`]. Consumed
    /// by the nested-construct (§B–F) emission stage.
    #[allow(dead_code)]
    reports_invalid_decls_in_type: bool,
    /// Offside / indentation diagnostics accumulated during filtering, in the
    /// order detected. Drained by [`filter_collect`]. Empty until the emission
    /// stages of `docs/offside-diagnostics-plan.md` land.
    diagnostics: Vec<OffsideDiagnostic>,
    /// Set when this run reaches a [`PushStrictness::VersionGated`] context
    /// push whose anchor is offside — the one place the filtered stream's
    /// shape depends on the language version. See
    /// [`FilterRun::shape_depends_on_language_version`] for the full
    /// soundness argument (`false` proves version-independence).
    shape_depends_on_language_version: bool,
}

/// How a context push treats an offside anchor — FCS's `strictIndentation`
/// argument to `tryPushCtxt`, split by *where the strictness comes from* so
/// the filter can record when the tree shape depends on the language version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushStrictness {
    /// FCS passes a hardcoded `false` (or this is the plain `pushCtxt` path):
    /// the push is kept at every language version — an offside anchor still
    /// emits its FS0058, but never aborts, so the shape is version-invariant.
    AlwaysLenient,
    /// FCS passes `strictIndentation` (the F# 8 gate,
    /// [`LanguageVersion::strict_indentation_is_error`]): an offside anchor
    /// aborts the push at F# 8+ and keeps it below, so *reaching this case
    /// with an offside anchor* is exactly what makes the resulting tree
    /// version-dependent — [`Filter::try_push_ctxt`] records it on
    /// [`Filter::shape_depends_on_language_version`].
    VersionGated,
}

/// Mirrors FCS's `isAdjacent` (LexFilter.fs:1059-1062): two tokens are
/// adjacent when there is no whitespace, comment, or newline between them.
/// We work in byte spans; trivia between them would have widened the gap.
fn is_adjacent(left: &TokenTup<'_>, right: &TokenTup<'_>) -> bool {
    left.span.end == right.span.start
}

/// Port of FCS's `isAtomicExprEndToken` (`LexFilter.fs:394`): the token kinds
/// after which a glued `-`/`+`/`&` is *infix* (subtraction/addition/AND) rather
/// than a term-starting prefix sign. Used both by the offside `ADJACENT_PREFIX_OP`
/// rule here (a glued-left atomic end keeps the op infix) and by the parser's
/// sign-fold pass (which folds `-1`/`+1` only when *not* glued to such an end).
/// `KEYWORD_STRING` and `INT32_DOT_DOT` are deliberately absent — FCS omits them.
pub(crate) fn is_atomic_expr_end(tok: &Token<'_>) -> bool {
    matches!(
        tok,
        Token::Ident(_)
            | Token::QuotedIdent(_)
            | Token::Int(_)
            | Token::XInt(_)
            | Token::IntSuffixed(_)
            | Token::XIntSuffixed(_)
            | Token::Float64(_)
            | Token::Float32(_)
            | Token::XIEEE64(_)
            | Token::XIEEE32(_)
            | Token::Decimal(_)
            | Token::BigNum(_)
            | Token::String
            | Token::VerbatimString
            | Token::TripleString
            | Token::Char(_)
            | Token::RParen
            | Token::RBrack
            | Token::RBrace
            | Token::BarRBrace
            | Token::BarRBrack
            | Token::End
            | Token::Null
            | Token::False
            | Token::True
            | Token::Underscore
    )
}

impl<'a, I: Iterator<Item = (Result<Token<'a>, LexError>, Span)>> Filter<'a, I> {
    fn new(source: &'a str, lang: LanguageVersion, raw: I) -> Self {
        // FCS strips a file-start UTF-8 BOM (`U+FEFF`, 3 bytes) so it never shifts
        // the offside column of line 1. We keep it as leading trivia (for
        // losslessness) but start line-1 columns *after* it — otherwise line 1 is
        // offside-shifted right by the BOM width, and a later same-column
        // top-level token is misread as a continuation of the first statement.
        // The `col` subtractions below `saturating_sub` so the BOM whitespace
        // token itself (which starts *before* this baseline) does not underflow;
        // its own column is trivia and never consulted. Only the file-start BOM
        // is special — a `U+FEFF` elsewhere is an ordinary zero-width space.
        let cur_line_start_byte = if source.starts_with('\u{FEFF}') {
            '\u{FEFF}'.len_utf8()
        } else {
            0
        };
        Self {
            raw,
            source,
            delayed: Vec::new(),
            offside_stack: Vec::new(),
            undentation_skip: Vec::new(),
            initialized: false,
            eof_pulled: false,
            cur_line: 1,
            cur_line_start_byte,
            line_starts: vec![(1, cur_line_start_byte)],
            last_byte: 0,
            paren_depth: 0,
            last_real_end: 0,
            last_real_was_atomic_end: false,
            tokens_that_need_no_processing: 0,
            strict_indentation_is_error: lang.strict_indentation_is_error(),
            reports_invalid_decls_in_type: lang.reports_invalid_decls_in_type_definitions(),
            diagnostics: Vec::new(),
            shape_depends_on_language_version: false,
        }
    }

    fn is_trivia_real(t: &Token<'_>) -> bool {
        matches!(
            t,
            Token::Whitespace | Token::Newline | Token::LineComment | Token::BlockComment
        )
    }

    /// Advance the line/column cursor to absolute byte offset `to`, scanning the
    /// intervening `source[last_byte..to]` for `\n`, and return the [`Pos`] there.
    ///
    /// This spans **both** a token's own bytes and any *gap* preceding it. The
    /// active stream fed to the filter has directive lines and `#if`-inactive
    /// regions removed (`parser::parse_inner` keeps only `TriviaToken::Lexed`),
    /// so consecutive token spans are **not** contiguous. Their newlines never
    /// arrive as trivia tokens, yet FCS — whose lexbuf scans the whole file,
    /// inactive regions included — counts them. Scanning from `last_byte`
    /// recovers exactly those skipped line breaks, so a context anchored after
    /// an inactive region reports its real source line (which the FS0058
    /// "started at position" message embeds) and the synthetic EOF lands at the
    /// true end of file rather than before a trailing inactive region.
    ///
    /// Line counting mirrors FCS's `incrLine` on its `newline = '\n' | '\r' '\n'`
    /// pattern (`lex.fsl:315`): a lone `\r` is **not** a break (absent from
    /// FCS's `newline`), and the next line starts at the byte just past the last
    /// `\n` (`Position.NextLine`, `prim-lexing.fs:225`). Only the *last* line a
    /// segment reaches gets a [`Self::line_starts`] entry — interior lines host
    /// no token start (a gap's are inactive; a multi-line token's are its own
    /// body), so no [`Context`] anchors there and [`Self::utf16_col`] never
    /// queries them.
    fn advance_cursor_to(&mut self, to: usize) -> Pos {
        debug_assert!(
            to >= self.last_byte,
            "raw token spans advance monotonically"
        );
        let seg = &self.source[self.last_byte..to];
        let newlines = seg.bytes().filter(|&b| b == b'\n').count() as u32;
        if newlines > 0 {
            self.cur_line_start_byte = self.last_byte + seg.rfind('\n').unwrap() + 1;
            self.cur_line += newlines;
            self.line_starts
                .push((self.cur_line, self.cur_line_start_byte));
        }
        self.last_byte = to;
        Pos {
            line: self.cur_line,
            // `saturating_sub`: a file-start BOM seeds `cur_line_start_byte` past
            // byte 0, so the BOM trivia token — and the EOF of a BOM-only /
            // all-trivia file — would otherwise underflow.
            col: to.saturating_sub(self.cur_line_start_byte) as u32,
        }
    }

    /// Pull a single token from the underlying lexer, updating line/column
    /// state via [`Self::advance_cursor_to`]. Trivia tokens are consumed for
    /// position tracking and skipped; the next non-trivia token (or synthetic
    /// EOF) is returned.
    fn pull_raw(&mut self) -> Option<TokenTup<'a>> {
        loop {
            match self.raw.next() {
                Some((res, span)) => {
                    // Cross any filtered gap before this token, then its own
                    // bytes — the two are disjoint and partition `last_byte ..
                    // span.end`, so newlines are counted exactly once.
                    let start = self.advance_cursor_to(span.start);
                    let end = self.advance_cursor_to(span.end);
                    if let Ok(ref t) = res
                        && Self::is_trivia_real(t)
                    {
                        continue;
                    }
                    let token = match res {
                        Ok(t) => TokenContent::Real(t),
                        Err(e) => TokenContent::Err(e),
                    };
                    return Some(TokenTup {
                        token,
                        span,
                        start,
                        end,
                    });
                }
                None => {
                    if self.eof_pulled {
                        return None;
                    }
                    self.eof_pulled = true;
                    // Anchor EOF at the true end of file, advancing across any
                    // trailing filtered gap (an inactive `#if` region or
                    // directive that runs to EOF). FCS's lexbuf reaches
                    // `source.len()`; stopping at the last active token would
                    // put an EOF-anchored offside squiggle before that region.
                    let pos = self.advance_cursor_to(self.source.len());
                    return Some(TokenTup {
                        token: TokenContent::Eof,
                        span: self.source.len()..self.source.len(),
                        start: pos,
                        end: pos,
                    });
                }
            }
        }
    }

    fn pop_next_token_tup(&mut self) -> Option<TokenTup<'a>> {
        self.delayed.pop().or_else(|| self.pull_raw())
    }

    fn peek_next_token_tup(&mut self) -> Option<TokenTup<'a>> {
        if let Some(tt) = self.delayed.last() {
            return Some(tt.clone());
        }
        let fresh = self.pull_raw()?;
        self.delayed.push(fresh.clone());
        Some(fresh)
    }

    fn delay_token(&mut self, tt: TokenTup<'a>) {
        self.delayed.push(tt);
    }

    /// FCS's `delayTokenNoProcessing` (LexFilter.fs:699). Push the token
    /// onto the delayed stack AND bump the no-processing counter so the
    /// matching `pop_next_token_tup` bypasses dispatch and returns the
    /// token raw. Used by the multi-member pop cascade
    /// (LexFilter.fs:2179-2195) for the saved trigger keyword and every
    /// synthetic END inserted on the way down — they're computed eagerly
    /// at unwind time and must not re-trigger offside rules when popped.
    fn delay_token_no_processing(&mut self, tt: TokenTup<'a>) {
        self.delayed.push(tt);
        self.tokens_that_need_no_processing += 1;
    }

    /// FCS's `isControlFlowOrNotSameLine` (LexFilter.fs:1319-1324). `tt`
    /// is the current dispatch token (typically EQUALS); returns `true` when
    /// the next real token is either on a different line *or* is one of the
    /// control-flow keywords whose presence implies the RHS opens a fresh
    /// block. Used by the EQUALS/LARROW arms inside record-update and let-
    /// binding contexts to decide between `AddBlockEnd` (push OBLOCKBEGIN +
    /// match with OBLOCKEND) and `NoAddBlockEnd` (silent inner SeqBlock).
    fn is_control_flow_or_not_same_line(&mut self, tt: &TokenTup<'a>) -> bool {
        // FCS's `EOF _ -> false` arm tests the *current* dispatch token, not
        // the lookahead — so a synthetic EOF lookahead does NOT short-circuit
        // (LexFilter.fs:1320-1324). For an EQUALS at end of line followed by
        // an EOF on the next line, this must still report "not same line" so
        // the recovery path opens AddBlockEnd. Our callers only invoke this
        // for real EQUALS/LARROW tokens, but pin the guard anyway.
        if matches!(tt.token, TokenContent::Eof) {
            return false;
        }
        let Some(next) = self.peek_next_token_tup() else {
            // Stream genuinely exhausted (no synthetic EOF either) — fall
            // back to "false" so we don't synthesise spurious virtuals.
            return false;
        };
        if next.start.line != tt.start.line {
            return true;
        }
        // FCS pattern: `TRY | MATCH | MATCH_BANG | IF | LET _ | FOR | WHILE
        // | WHILE_BANG`. `LET _` matches `LET(false)` (`let`) and `LET(true)`
        // (`use`) per LexHelpers.fs:336-362 — both reach the same parser path.
        // `let!` is the separate `LET_BANG` token and is *not* in the pattern,
        // so `Token::LetBang` / `Token::UseBang` must stay out.
        matches!(
            &next.token,
            TokenContent::Real(
                Token::Try
                    | Token::Match
                    | Token::MatchBang
                    | Token::If
                    | Token::Let
                    | Token::Use
                    | Token::For
                    | Token::While
                    | Token::WhileBang
            )
        )
    }

    /// FCS's `isLongIdentEquals` (LexFilter.fs:1327-1351). Pure lookahead:
    /// returns `true` iff `token` begins a longident (IDENT or GLOBAL) and the
    /// upcoming stream matches `(DOT IDENT)* EQUALS` after it. Every consumed
    /// token is re-queued via `delay_token` so the caller sees an unchanged
    /// input stream.
    ///
    /// FCS uses this to recognise record-update / object-init bindings whose
    /// LHS may be qualified (`{ r with M.A = 1 }`), so that the EQUALS
    /// dispatch arm at LexFilter.fs:2253-2263 can push an inner SeqBlock
    /// when the binding is in fact a long-ident assignment.
    fn is_long_ident_equals(&mut self, token: &Token<'_>) -> bool {
        // FCS's `IDENT _` pattern covers both regular and backtick-quoted
        // identifiers; our lexer splits them into `Token::Ident` and
        // `Token::QuotedIdent`, so both must be accepted here. Without
        // QuotedIdent, record updates with backtick field names
        // (`{ r with ``A`` = 1 }`) would not push the inner SeqBlock and
        // multi-line updates would miss OBLOCKSEPs between bindings.
        if !matches!(
            token,
            Token::Ident(_) | Token::QuotedIdent(_) | Token::Global
        ) {
            return false;
        }
        self.is_long_ident_equals_loop()
    }

    /// FCS's inner `loop` (LexFilter.fs:1331-1350). Pops one token, classifies
    /// it (EQUALS → true; DOT IDENT → recurse; anything else → false), then
    /// re-queues it with `delay_token`. The DOT branch pops a second token to
    /// check for IDENT and likewise re-queues. All paths restore the stream.
    fn is_long_ident_equals_loop(&mut self) -> bool {
        let Some(tt) = self.pop_next_token_tup() else {
            return false;
        };
        let res = match &tt.token {
            TokenContent::Eof => false,
            TokenContent::Real(Token::Equals) => true,
            TokenContent::Real(Token::Dot) => {
                let Some(after_dot) = self.pop_next_token_tup() else {
                    self.delay_token(tt);
                    return false;
                };
                let after_dot_res = match &after_dot.token {
                    TokenContent::Eof => false,
                    TokenContent::Real(Token::Ident(_) | Token::QuotedIdent(_)) => {
                        self.is_long_ident_equals_loop()
                    }
                    _ => false,
                };
                self.delay_token(after_dot);
                after_dot_res
            }
            _ => false,
        };
        self.delay_token(tt);
        res
    }

    /// FCS's `nextTokenIsAdjacentLParen` (LexFilter.fs:1070-1074). Used inside
    /// `peek_adjacent_typars` to detect `f<int>(x)` and inject
    /// `HighPrecedenceParenApp` between the closing `>` and the `(`.
    fn next_token_is_adjacent_lparen(&mut self, tt: &TokenTup<'a>) -> bool {
        let Some(next) = self.peek_next_token_tup() else {
            return false;
        };
        matches!(&next.token, TokenContent::Real(Token::LParen)) && is_adjacent(tt, &next)
    }

    /// FCS's `nextTokenIsAdjacentLBrack` (LexFilter.fs:1064-1068). Used by
    /// the IDENT-adjacent-LBRACK dispatch to detect `f[i]` and inject
    /// `HighPrecedenceBrackApp` between the IDENT and the `[`.
    fn next_token_is_adjacent_lbrack(&mut self, tt: &TokenTup<'a>) -> bool {
        let Some(next) = self.peek_next_token_tup() else {
            return false;
        };
        matches!(&next.token, TokenContent::Real(Token::LBrack)) && is_adjacent(tt, &next)
    }

    /// FCS's `peekAdjacentTypars` (LexFilter.fs:1080-1247). Lookahead pass
    /// that distinguishes generic type application from a comparison `<`.
    ///
    /// `head` is the token immediately preceding the candidate `<` (an
    /// IDENT, DELEGATE, or numeric literal in the dispatch site). The
    /// scan succeeds iff:
    /// 1. the next token is one of `<`, `</`, `<^`, `<@`, *and* it sits
    ///    directly against `head` (no whitespace/newline/comment between),
    /// 2. a paren-balance walk from that opener reaches `>` (possibly a
    ///    fused close-op) at depth zero before encountering any token
    ///    that isn't part of the type-application grammar.
    ///
    /// On success: every consumed token is re-emitted (LIFO via
    /// `delay_token`) with the candidate `<` rewritten to `Less(true)`,
    /// each closing `>` to `Greater(true)`, and fused open/close ops
    /// split into their constituent tokens. If the closing `>` is
    /// immediately followed by an adjacent `(`, a
    /// `Virtual::HighPrecedenceParenApp` is injected between them.
    ///
    /// On failure: every consumed token is re-emitted unmodified. FCS
    /// actually re-emits with `res = false` substituted into LESS/GREATER
    /// payloads, which is functionally identical since the lexer already
    /// gave us `Less(false)`/`Greater(false)`.
    ///
    /// The `indentation` flag enables an additional check that no scanned
    /// token sits left of `head`'s end column; FCS uses `false` at the
    /// top-level IDENT call site.
    fn peek_adjacent_typars(&mut self, indentation: bool, head: &TokenTup<'a>) -> bool {
        let Some(lookahead) = self.peek_next_token_tup() else {
            return false;
        };
        let is_opener = match &lookahead.token {
            TokenContent::Real(Token::Less(_)) => true,
            TokenContent::Real(Token::LQuote) => true,
            TokenContent::Real(Token::Op(s)) => *s == "</" || *s == "<^",
            _ => false,
        };
        if !is_opener {
            return false;
        }
        if !is_adjacent(head, &lookahead) {
            return false;
        }

        let head_end_line = head.end.line;
        let head_end_col = head.end.col;

        let mut stack: Vec<(TokenTup<'a>, bool)> = Vec::new();
        // The opener was already in `delayed` from the peek above; pop it.
        let opener = self
            .pop_next_token_tup()
            .expect("opener was just peeked into delayed");
        stack.push((opener, true));

        let mut n_paren: i32 = 1;
        let mut succeeded = false;
        let mut adj_lparen_after_close = false;

        while let Some(tok) = self.pop_next_token_tup() {
            // FCS indentation guard (line 1098):
            // `lookaheadTokenStartPos < tokenEndPos` — line takes precedence.
            let indent_fails = indentation
                && (tok.start.line < head_end_line
                    || (tok.start.line == head_end_line && tok.start.col < head_end_col));

            let idx = stack.len();
            stack.push((tok, true));

            if indent_fails {
                break;
            }

            // Dispatch. Borrow the just-pushed entry for further inspection;
            // we may also need its location for the adjacent-LParen probe.
            let outcome = {
                let token_ref = &stack[idx].0.token;
                classify_typar_scan_token(token_ref)
            };

            match outcome {
                TyparScanAction::Fail => break,
                TyparScanAction::Continue => {}
                TyparScanAction::OpenParen => {
                    n_paren += 1;
                }
                TyparScanAction::ClosePlain => {
                    n_paren -= 1;
                    if n_paren <= 0 {
                        // FCS: `RPAREN | RBRACK` at depth 0 fails the scan
                        // (no success path — those are not typar closers).
                        break;
                    }
                }
                TyparScanAction::CloseGreater => {
                    n_paren -= 1;
                    if n_paren > 0 {
                        // Bare GREATER, no after-op: smash stays true.
                    } else {
                        succeeded = true;
                        let head_tok = &stack[idx].0;
                        if self.next_token_is_adjacent_lparen(head_tok) {
                            adj_lparen_after_close = true;
                        }
                        break;
                    }
                }
                TyparScanAction::CloseGreaterWithAfter => {
                    // `GreaterRBrack` — close with an after-op already
                    // fused (the `]` is the after-op).
                    n_paren -= 1;
                    if n_paren > 0 {
                        // Nested + has after-op → don't smash.
                        stack[idx].1 = false;
                    } else {
                        succeeded = true;
                        // After-op present: don't probe for adjacent LParen
                        // (the `]` separates the close from any following `(`).
                        break;
                    }
                }
                TyparScanAction::CloseOpSplit {
                    greater_count,
                    has_tail,
                } => {
                    n_paren -= greater_count as i32;
                    if n_paren > 0 {
                        stack[idx].1 = !has_tail;
                        // FCS lexes `>|]` and `>|}` as the atomic
                        // `GREATER_BAR_RBRACK` / `GREATER_BAR_RBRACE`
                        // tokens (one paren decrement each). Our lexer
                        // splits them into `Op(">|") + RBrack` / `+ RBrace`.
                        // When the inner generic is *nested*, the bare
                        // `RBrack`/`RBrace` would otherwise be classified
                        // as `OtherToken` at depth 1 and abort the outer
                        // scan. Pre-consume the adjacent bracket/brace
                        // here so the outer scan sees the fused close as
                        // a single unit; the inner trigger's own scan
                        // (e.g., from the IDENT preceding the inner `<`)
                        // will re-pop and smash the pair into
                        // `Greater(true) + BarRBrack`/`BarRBrace`.
                        let tail_is_pipe = matches!(
                            &stack[idx].0.token,
                            TokenContent::Real(Token::Op(s))
                                if typars_close_op_split(s)
                                    .is_some_and(|sp| matches!(sp.tail, Some(Token::Op("|"))))
                        );
                        if tail_is_pipe {
                            let trailer_info = self.peek_next_token_tup().and_then(|t| {
                                if is_adjacent(&stack[idx].0, &t)
                                    && matches!(
                                        t.token,
                                        TokenContent::Real(Token::RBrack)
                                            | TokenContent::Real(Token::RBrace)
                                    )
                                {
                                    Some(())
                                } else {
                                    None
                                }
                            });
                            if trailer_info.is_some() {
                                let popped =
                                    self.pop_next_token_tup().expect("just peeked into delayed");
                                stack.push((popped, false));
                            }
                        }
                    } else {
                        succeeded = true;
                        if !has_tail {
                            let head_tok = &stack[idx].0;
                            if self.next_token_is_adjacent_lparen(head_tok) {
                                adj_lparen_after_close = true;
                            }
                        }
                        break;
                    }
                }
                TyparScanAction::OtherToken => {
                    // FCS line 1184: any non-whitelisted token is only
                    // permissible at nesting depth > 1 (i.e. strictly
                    // inside some inner paren), otherwise scan fails.
                    if n_paren <= 1 {
                        break;
                    }
                }
            }
        }

        // Synthesise HighPrecedenceParenApp at the LParen's location
        // (peek-pulled into `delayed` during the close check). We re-peek
        // idempotently to recover the LParen's span.
        if adj_lparen_after_close {
            let lparen = self
                .peek_next_token_tup()
                .expect("adj_lparen_after_close ⇒ LParen still in delayed");
            let hpp_app = TokenTup {
                token: TokenContent::Virtual(Virtual::HighPrecedenceParenApp),
                span: lparen.span.clone(),
                start: lparen.start,
                end: lparen.end,
            };
            stack.push((hpp_app, false));
        }

        let res = succeeded;

        // Replay: pop the stack in reverse-insertion order and delay each.
        // Result: `delayed` top-down is opener, ..., close, (HPP_APP, LParen).
        while let Some((tt, smash)) = stack.pop() {
            if smash {
                self.smash_typar_token(tt, res);
            } else {
                self.delay_token(tt);
            }
        }

        res
    }

    /// Token-rewrite step inside `peek_adjacent_typars`. Mirrors FCS's
    /// per-arm smashing logic (LexFilter.fs:1194-1240). `res` is the bool
    /// payload to embed into LESS/GREATER outputs — `true` on a successful
    /// typar parse, `false` on backtrack.
    fn smash_typar_token(&mut self, tt: TokenTup<'a>, res: bool) {
        match &tt.token {
            // Fused openers `</`, `<^`: split into LESS res + (`/` or `^`).
            TokenContent::Real(Token::Op(s)) if *s == "</" || *s == "<^" => {
                let tail_op: &'static str = if *s == "</" { "/" } else { "^" };
                self.delay_pair_split_at_1(&tt, Token::Less(res), Token::Op(tail_op));
            }
            // Typed-quotation opener `<@`: split into LESS res + `@`.
            TokenContent::Real(Token::LQuote) => {
                self.delay_pair_split_at_1(&tt, Token::Less(res), Token::Op("@"));
            }
            // Fused closer `>]`: split into GREATER res + `]`.
            TokenContent::Real(Token::GreaterRBrack) => {
                self.delay_pair_split_at_1(&tt, Token::Greater(res), Token::RBrack);
            }
            // Bare GREATER: re-emit with the bool replaced.
            TokenContent::Real(Token::Greater(_)) => {
                self.delay_token(TokenTup {
                    token: TokenContent::Real(Token::Greater(res)),
                    span: tt.span.clone(),
                    start: tt.start,
                    end: tt.end,
                });
            }
            // `Op(s)` with `s` starting with `>`: split the leading run of
            // `>`s into individual GREATER res tokens, followed by an
            // optional tail token (LIFO: tail delayed first).
            // FCS has a dedicated `INFIX_COMPARE_OP ">:"` arm (LexFilter.fs:
            // 1204-1207) that splits the fused operator into `GREATER res +
            // COLON` — even though `TyparsCloseOp` rejects `>:`. Mirror that
            // here so the post-smash stream matches FCS when a successful
            // scan included `>:` (necessarily at n_paren > 1 via OtherToken).
            TokenContent::Real(Token::Op(">:")) => {
                self.delay_pair_split_at_1(&tt, Token::Greater(res), Token::Colon);
            }
            TokenContent::Real(Token::Op(s)) if s.starts_with('>') => {
                let Some(split) = typars_close_op_split(s) else {
                    // This can be reached via the `OtherToken` scan path for
                    // other `>`-prefixed ops that `typars_close_op_split`
                    // rejects (none currently in F# beyond `>:`, which is
                    // handled above). Mirror FCS's catch-all `delayToken
                    // tokenTup` (LexFilter.fs:1241): re-emit unchanged.
                    self.delay_token(tt);
                    return;
                };
                let start_byte = tt.span.start;
                let start_col = tt.start.col;
                let line = tt.start.line;
                let tail_start_byte = start_byte + split.greater_count;
                let tail_start_col = start_col + split.greater_count as u32;

                // For `Op(">|")` followed by an adjacent `RBrack`/`RBrace`,
                // FCS emits `BarRightBracket` / `BarRightBrace` instead of
                // `Op("|")` + the bracket — our lexer would have produced
                // that compound directly if `>` hadn't fused with `|`. Pull
                // and fuse here so the post-replay stream matches FCS.
                // (Mirrors FCS's GREATER_BAR_RBRACK / GREATER_BAR_RBRACE
                // handling at LexFilter.fs:1228-1236.)
                let mut tail_to_emit: Option<TokenTup<'a>> = match split.tail {
                    None => None,
                    Some(tail_tok) => Some(TokenTup {
                        token: TokenContent::Real(tail_tok),
                        span: tail_start_byte..tt.span.end,
                        start: Pos {
                            col: tail_start_col,
                            line,
                        },
                        end: tt.end,
                    }),
                };
                if matches!(
                    tail_to_emit.as_ref().map(|t| &t.token),
                    Some(TokenContent::Real(Token::Op("|")))
                ) && let Some(next) = self.peek_next_token_tup()
                    && next.span.start == tt.span.end
                    && matches!(
                        next.token,
                        TokenContent::Real(Token::RBrack) | TokenContent::Real(Token::RBrace)
                    )
                {
                    let next = self
                        .pop_next_token_tup()
                        .expect("peek_next_token_tup returned Some");
                    let fused_tok = match next.token {
                        TokenContent::Real(Token::RBrack) => Token::BarRBrack,
                        TokenContent::Real(Token::RBrace) => Token::BarRBrace,
                        _ => unreachable!("matched in the peek guard above"),
                    };
                    tail_to_emit = Some(TokenTup {
                        token: TokenContent::Real(fused_tok),
                        span: tail_start_byte..next.span.end,
                        start: Pos {
                            col: tail_start_col,
                            line,
                        },
                        end: next.end,
                    });
                }
                if let Some(tail_tt) = tail_to_emit {
                    self.delay_token(tail_tt);
                }
                for i in (0..split.greater_count).rev() {
                    let byte_start = start_byte + i;
                    self.delay_token(TokenTup {
                        token: TokenContent::Real(Token::Greater(res)),
                        span: byte_start..byte_start + 1,
                        start: Pos {
                            col: start_col + i as u32,
                            line,
                        },
                        end: Pos {
                            col: start_col + (i + 1) as u32,
                            line,
                        },
                    });
                }
            }
            // FCS additionally smashes GREATER_BAR_RBRACE, GREATER_BAR_RBRACK,
            // and RQUOTE_BAR_RBRACE inside the scan. The two GREATER_BAR_*
            // cases arrive in our stream as `Op(">|")` + RBrack/RBrace; they
            // are fused inside the `Op(">…")` arm above. The quotation case
            // is handled by the eager pre-rule split at the top of
            // `hw_token_fetch`. So no extra arms are needed here.
            _ => {
                // Default: re-emit unchanged. Covers LESS (rewritten by the
                // outer caller, not here) and any whitelist token.
                self.delay_token(tt);
            }
        }
    }

    /// Helper: split a 2-byte token spanning [start, end) at offset +1,
    /// emitting `head_tok` for [start, end-1) and `tail_tok` for
    /// [start+1, end). LIFO: tail is delayed first so `head_tok` is the
    /// next pop. Both halves are real tokens.
    fn delay_pair_split_at_1(
        &mut self,
        tt: &TokenTup<'a>,
        head_tok: Token<'a>,
        tail_tok: Token<'a>,
    ) {
        let mid_byte = tt.span.start + 1;
        let tail = TokenTup {
            token: TokenContent::Real(tail_tok),
            span: mid_byte..tt.span.end,
            start: Pos {
                col: tt.start.col + 1,
                line: tt.start.line,
            },
            end: tt.end,
        };
        self.delay_token(tail);
        let head = TokenTup {
            token: TokenContent::Real(head_tok),
            span: tt.span.start..mid_byte,
            start: tt.start,
            end: Pos {
                col: tt.end.col - 1,
                line: tt.end.line,
            },
        };
        self.delay_token(head);
    }

    /// FCS's `pushCtxt` (LexFilter.fs:1022) — unconditional push, defined as
    /// `tryPushCtxt false false`. With `strict=false` the push always lands,
    /// but FCS still runs the `isCorrectIndent` check (LexFilter.fs:990-1013)
    /// and so still *emits* the FS0058 "offside of context" diagnostic for a
    /// context anchored left of the undentation limit — it just doesn't abort.
    /// `trigger` is the byte span of the dispatch token FCS anchors that
    /// diagnostic at (`tokenTup`), distinct from `ctxt`'s own anchor column.
    ///
    /// Passes `anchor_is_eof = false`: every `push_ctxt` caller anchors at the
    /// *dispatch* token, which is never EOF (the EOF force-closure cascade empties
    /// the stack before any push runs). The lookahead-anchored pushes that *can*
    /// see EOF go through [`Self::try_push_ctxt`] directly.
    fn push_ctxt(&mut self, trigger: Span, ctxt: Context) {
        self.try_push_ctxt(PushStrictness::AlwaysLenient, false, false, trigger, ctxt);
    }

    /// `true` for contexts that `undentation_limit`'s non-strict walk skips
    /// unconditionally, with no arm firing for *any* `new_ctxt`: Vanilla,
    /// SeqBlock, Fun, Then, and Paren (any opener — in non-strict the
    /// L1451 `SeqBlock | Paren` skip precedes the class/struct/interface arm).
    /// A maximal run of these can be jumped in O(1) via [`Self::undentation_skip`].
    /// Everything else either imposes a limit (catch-alls) or carries a shape /
    /// `new_ctxt`-sensitive arm (Else / WithAsAugment / Function / Match /
    /// MatchClauses / Do — the last via the FCS L779 do-in-type/module arm),
    /// so the walk must visit it. The property test
    /// `undentation_cache_matches_reference` guards this classification.
    fn is_pure_skip(ctxt: &Context) -> bool {
        matches!(
            ctxt,
            Context::Vanilla { .. }
                | Context::SeqBlock { .. }
                | Context::Fun { .. }
                | Context::Then { .. }
                | Context::Paren { .. }
        )
    }

    /// Append the [`Self::undentation_skip`] entry for the context just pushed
    /// onto `offside_stack`. Call immediately after the push. O(1).
    fn push_undentation_skip(&mut self) {
        let i = self.offside_stack.len() - 1;
        let entry = if Self::is_pure_skip(&self.offside_stack[i]) {
            // Skip this context: the stop is wherever the one below stops
            // (`u32::MAX` if the whole prefix is pure-skip).
            if i == 0 {
                u32::MAX
            } else {
                self.undentation_skip[i - 1]
            }
        } else {
            // The walk stops here.
            i as u32
        };
        self.undentation_skip.push(entry);
    }

    /// FCS's `tryPushCtxt` (LexFilter.fs:771). Computes
    /// `undentation_limit` against the current stack and refuses the push
    /// when `strict` is set and `ctxt`'s anchor column violates the limit.
    /// Returns whether the push happened.
    ///
    /// `ignore_indent` skips the limit check entirely — mirrors FCS's
    /// `ignoreIndent` arg (LexFilter.fs:991), used by `pushCtxtSeqBlockAt`
    /// for `CtxtVanilla` and interpolated-string pushes which FCS exempts
    /// at LexFilter.fs:993-998. Called with `ignore_indent=true` by the
    /// EQUALS-driven `CtxtTypeDefns` replacement (`equals_pushes`), which
    /// mirrors FCS's `replaceCtxtIgnoreIndent` (LexFilter.fs:1039, 2228).
    /// `anchor_is_eof` is `true` when `ctxt`'s anchor is the synthetic EOF token
    /// — FCS's `startPosOfTokenTup` reads EOF as `ColumnMinusOne`
    /// (LexFilter.fs:640, "processed as if on column -1 … forces the closure of
    /// all contexts"), so a context pushed at EOF is one column further left than
    /// its byte position for the offside check. Only the handful of lookahead-
    /// anchored pushes can see EOF (the dispatch-token pushes never do — the EOF
    /// force-closure cascade pops the stack before any push runs), so
    /// [`Self::push_ctxt`] hard-codes `false` and only those sites pass `true`.
    fn try_push_ctxt(
        &mut self,
        strictness: PushStrictness,
        ignore_indent: bool,
        anchor_is_eof: bool,
        trigger: Span,
        ctxt: Context,
    ) -> bool {
        let correct = self.is_correct_indent(ignore_indent, anchor_is_eof, &trigger, &ctxt);
        let strict = match strictness {
            PushStrictness::AlwaysLenient => false,
            PushStrictness::VersionGated => {
                if !correct {
                    // This push is kept below F# 8 and aborted at F# 8+, so
                    // everything downstream nests differently: the stream's
                    // shape now provably depends on the language version.
                    // Recorded here — the single point where two hypothetical
                    // version runs first diverge — so an unset flag proves
                    // version-invariance (see [`FilterRun`]).
                    self.shape_depends_on_language_version = true;
                }
                self.strict_indentation_is_error
            }
        };
        if strict && !correct {
            return false;
        }
        self.offside_stack.push(ctxt);
        self.push_undentation_skip();
        true
    }

    /// FCS's `isCorrectIndent` block inside `tryPushCtxt` (LexFilter.fs:990-1013).
    /// Returns whether `ctxt`'s anchor column respects the undentation limit
    /// computed from the current stack; on a violation it records the FS0058
    /// "offside of context started earlier" diagnostic anchored at `trigger`
    /// (the dispatch token's byte span), with severity taken from the
    /// strict-indentation gate (error at F# 8+, warning below). The check —
    /// and the emission — run independently of `strict`: FCS's non-strict
    /// `pushCtxt` still emits, it just never aborts. Only `strict` controls
    /// whether the caller then refuses the push.
    ///
    /// The exemptions mirror FCS exactly: `ignore_indent` (its `ignoreIndent`
    /// arg), `CtxtVanilla` (a SeqBlock is always already pushed at this
    /// position), and interpolation-fill parens (`CtxtParen(INTERP_STRING_…)`,
    /// unlimited so multi-line interpolation bodies aren't offside) all short
    /// out to "correct" with no check and no diagnostic.
    fn is_correct_indent(
        &mut self,
        ignore_indent: bool,
        anchor_is_eof: bool,
        trigger: &Span,
        ctxt: &Context,
    ) -> bool {
        if ignore_indent {
            return true;
        }
        match ctxt {
            Context::Vanilla { .. } => return true,
            Context::Paren {
                opener: Opener::InterpFill,
                ..
            } => return true,
            _ => {}
        }
        let limit =
            Self::undentation_limit(true, ctxt, &self.offside_stack, &self.undentation_skip);
        // FCS's `startPosOfTokenTup`: an EOF-anchored context's offside column is
        // its byte column minus one (`ColumnMinusOne`). `ctxt.start_pos()` keeps
        // the true byte position (for the message / virtual anchoring); the −1
        // lives only here, in the comparison. Non-EOF anchors subtract 0.
        let c2 = ctxt.start_pos().col as i32 - i32::from(anchor_is_eof);
        if c2 >= limit.col {
            return true;
        }
        let severity = if self.strict_indentation_is_error {
            OffsideSeverity::Error
        } else {
            OffsideSeverity::Warning
        };
        // Byte-identical to FCS's `lexfltTokenIsOffsideOfContextStartedEarlier`
        // (FSComp.txt) fed through `warningStringOfPosition` (ParseHelpers.fs:34):
        // 1-based line, 0-based column printed 1-based — in UTF-16 code units
        // (FCS's lexbuf column scale), hence `utf16_col` rather than the raw
        // byte column. The embedded position is the limiting context's anchor,
        // so users can see which earlier construct imposed the limit.
        self.diagnostics.push(OffsideDiagnostic {
            message: format!(
                "Unexpected syntax or possible incorrect indentation: this token is offside of \
                 context started at position ({}:{}). Try indenting this further.\nTo continue \
                 using non-conforming indentation, pass the '--strict-indentation-' flag to the \
                 compiler, or set the language version to F# 7.",
                limit.pos.line,
                self.utf16_col(limit.pos) + 1,
            ),
            span: trigger.clone(),
            severity,
        });
        false
    }

    /// The FCS-facing column of `pos`: UTF-16 code units from the line start
    /// (FCS's lexbuf column scale), converted from our byte column. The two
    /// diverge whenever a non-ASCII character precedes `pos` on its line
    /// (`é` is 2 UTF-8 bytes / 1 UTF-16 unit). `pos` must be a token-start
    /// position — contexts anchor only at those — so its line always has a
    /// [`Self::line_starts`] entry and its column ends on a char boundary.
    fn utf16_col(&self, pos: Pos) -> u32 {
        let idx = self
            .line_starts
            .binary_search_by_key(&pos.line, |&(line, _)| line)
            .expect("context Pos lines host a token start");
        let line_start = self.line_starts[idx].1;
        self.source[line_start..line_start + pos.col as usize]
            .chars()
            .map(|c| c.len_utf16() as u32)
            .sum()
    }

    /// FCS's `undentationLimit` (LexFilter.fs:772-988). Returns the
    /// minimum start column the new context must respect to be accepted,
    /// paired with the anchor of the context that imposed it (which the
    /// FS0058 message embeds). `col: -1` means "no limit" (FCS's
    /// empty-stack base case, whose `pos` — the new context's own anchor —
    /// is never read: every column is `>= -1`).
    ///
    /// `new_ctxt` is the context being pushed — FCS threads it through
    /// because a handful of arms (e.g. L866 `CtxtSeqBlock` body for an
    /// `else`) discriminate on what's being pushed in addition to the
    /// existing stack shape.
    ///
    /// Only the arms that reference currently-ported contexts are
    /// populated; arms for not-yet-ported contexts are added alongside
    /// the push site that introduces them.
    fn undentation_limit(
        mut strict: bool,
        new_ctxt: &Context,
        mut stack: &[Context],
        skip: &[u32],
    ) -> PositionWithColumn {
        // Iterative form of FCS's recursive `undentationLimit`, plus an O(1)
        // fast-path over deep transparent runs. The original is tail-recursive
        // down the offside stack; since each delimiter opener pushes ~2 contexts
        // (a Paren/Brace plus a SeqBlock), a deeply nested `((((…))))` made the
        // stepwise walk O(depth) per push → O(n²) over the file (and, before
        // #572, overflowed the call stack). Looping preserves the exact stepwise
        // behaviour; the `skip`-cache jump below collapses each maximal run of
        // pure-skip contexts into one step, making the walk O(1) amortised.
        //
        // The stepwise arms remain the behavioural spec: the test
        // `undentation_cache_matches_reference` checks this function against a
        // verbatim, jump-free `undentation_limit_reference` over random stacks.
        loop {
            let Some((head, rest)) = stack.split_last() else {
                // FCS L774: empty stack → no limit.
                return PositionWithColumn {
                    pos: new_ctxt.start_pos(),
                    col: -1,
                };
            };

            // Fast-path (keeps this linear — see [`Self::undentation_skip`]): in
            // non-strict mode a pure-skip context and the whole run beneath it
            // are skipped without any arm firing, so jump straight to the first
            // non-pure-skip context (or to "empty" → no limit) instead of
            // stepping one at a time. Strict mode only ever skips Vanilla (one at
            // a time, below), so the jump is gated on `!strict`.
            if !strict && Self::is_pure_skip(head) {
                match skip[stack.len() - 1] {
                    u32::MAX => {
                        // Whole remaining prefix is pure-skip — same result
                        // as walking off the empty stack.
                        return PositionWithColumn {
                            pos: new_ctxt.start_pos(),
                            col: -1,
                        };
                    }
                    target => {
                        stack = &stack[..=target as usize];
                        continue;
                    }
                }
            }

            // FCS L777: CtxtVanilla — skip (SeqBlock always follows).
            if let Context::Vanilla { .. } = head {
                stack = rest;
                continue;
            }

            // FCS L785-786: under non-strict recursion, CtxtSeqBlock / CtxtParen
            // are transparent. Strict mode (the initial call from try_push_ctxt)
            // keeps them as limit-imposing contexts so the catch-alls at L987-988
            // apply.
            if !strict && matches!(head, Context::SeqBlock { .. } | Context::Paren { .. }) {
                stack = rest;
                continue;
            }

            // FCS L844-845 / L849-850 (`relaxWhitespace2`): a `TokenLExprParen`
            // opener (or a SeqBlock immediately inside one) places no limit until
            // we hit a leading construct deeper in the stack — so `let x = (\n
            // body\n)` with body left of `(` is legal, the body's offside being
            // gated by `let` (col+1) rather than `(` (col). Without this, our
            // strict push of the inner SeqBlock fails for body columns ≤ paren
            // column and the fallback path kicks in with the wrong anchor.
            if let Context::Paren { opener, .. } = head
                && opener.is_token_l_expr_paren()
            {
                strict = false;
                stack = rest;
                continue;
            }
            if let Context::SeqBlock { .. } = head
                && let Some(Context::Paren { opener, .. }) = rest.last()
                && opener.is_token_l_expr_paren()
            {
                strict = false;
                stack = &rest[..rest.len() - 1];
                continue;
            }

            // FCS L948-949: `type C = (class | struct | interface) ... end` —
            // the inner SeqBlock body is limited by `type`'s column + 1, not by
            // the class/struct/interface paren's column. This lets the body
            // deindent below the keyword when the keyword sits to the right of
            // `=` on the same line (`type I = interface\n  abstract …`). Without
            // this arm the catch-all returns Paren.col and strict pushes for
            // bodies left of the keyword fail, sending OBLOCKBEGIN through the
            // recovery fallback anchored at the keyword instead of at the body
            // token. (The CLASS opener isn't yet a push site, but include it in
            // the pattern to match FCS exactly.)
            if let Context::Paren {
                opener: Opener::Class | Opener::Struct | Opener::Interface,
                ..
            } = head
                && let [
                    ..,
                    limit_ctxt @ Context::TypeDefns { .. },
                    Context::SeqBlock { .. },
                ] = rest
            {
                return PositionWithColumn {
                    pos: limit_ctxt.start_pos(),
                    col: (limit_ctxt.start_pos().col as i32) + 1,
                };
            }

            // FCS L866-869 "MAJOR PERMITTED UNDENTATION": when pushing a SeqBlock
            // whose surrounding context is `CtxtElse :: CtxtIf`, the body may
            // deindent down to the `if`'s column. Concretely allows
            //   if x then y else
            //   let x = 3 + 4
            //   x + x
            // — the `else`-body SeqBlock anchored at `let` (col 0) must be
            // accepted even though `else` sits to the right. Without this arm
            // the catch-all below uses CtxtIf.col+1, refusing the strict push
            // and routing through the `NotFirstInSeqBlock` fallback anchored at
            // `else`, which mis-tokenises the body. Discrimination on
            // `new_ctxt` matches FCS — it only fires when a SeqBlock is being
            // pushed.
            if let Context::SeqBlock { .. } = new_ctxt
                && let Context::Else { .. } = head
                && let Some(Context::If { pos: if_pos }) = rest.last()
            {
                return PositionWithColumn {
                    pos: *if_pos,
                    col: if_pos.col as i32,
                };
            }

            // FCS L779-780: a freshly-pushed `CtxtSeqBlock(FirstInSeqBlock, …)`
            // sitting just inside a `CtxtDo :: CtxtSeqBlock :: (CtxtTypeDefns |
            // CtxtModuleBody)` is limited by the `do`'s column + 1 — the body
            // of a `do` in a type or module body must indent *past* the `do`,
            // not merely align with it. Without this arm the generic L902
            // recursion below treats the Do as transparent and the body
            // SeqBlock at `do`'s own column goes unflagged (`type C =\n    do\n
            // \    printfn "x"` gets no FS0058 at `printfn`).
            if let Context::SeqBlock { first: true, .. } = new_ctxt
                && let Context::Do { pos: do_pos } = head
                && let [
                    ..,
                    Context::TypeDefns { .. } | Context::ModuleBody { .. },
                    Context::SeqBlock { .. },
                ] = rest
            {
                return PositionWithColumn {
                    pos: *do_pos,
                    col: (do_pos.col as i32) + 1,
                };
            }

            // FCS L782-783: a freshly-pushed `CtxtSeqBlock(FirstInSeqBlock, …)`
            // sitting *just inside* a `CtxtWithAsAugment :: CtxtTypeDefns` is
            // limited by the type-defns column + 1. The augment-body SeqBlock
            // must indent at least one space past the `type` keyword. Without
            // this the L902 recursion (WithAsAugment → strict=false) lets the
            // SeqBlock through, and the augment body anchored at col 0 would
            // refuse a same-column body below it. Discrimination is on
            // `new_ctxt = SeqBlock(first=true)` matching FCS.
            if let Context::SeqBlock { first: true, .. } = new_ctxt
                && let Context::WithAsAugment { .. } = head
                && let Some(limit_ctxt @ Context::TypeDefns { .. }) = rest.last()
            {
                return PositionWithColumn {
                    pos: limit_ctxt.start_pos(),
                    col: (limit_ctxt.start_pos().col as i32) + 1,
                };
            }

            // FCS L877-878: when pushing a `CtxtWithAsAugment`, the body's
            // SeqBlock may align precisely with the enclosing
            // `CtxtMemberHead | CtxtTypeDefns | CtxtException |
            // CtxtInterfaceHead` — limit = host.col, not col+1.
            if let Context::WithAsAugment { .. } = new_ctxt
                && matches!(
                    head,
                    Context::MemberHead { .. }
                        | Context::TypeDefns { .. }
                        | Context::Exception { .. }
                        | Context::InterfaceHead { .. }
                )
            {
                return PositionWithColumn {
                    pos: head.start_pos(),
                    col: head.start_pos().col as i32,
                };
            }

            // FCS L815-816: when pushing a CtxtMatchClauses on top of a
            // `CtxtFunction :: CtxtSeqBlock :: CtxtLetDecl :: _` stack, the
            // limit is the let's column. Lets the clauses of
            // `let f x = function\n| Case1 -> …\n| Case2 -> …` align with the
            // outer `let` (column 0) — the common dedented-clauses style. Must
            // run before the generic CtxtFunction no-limit recursion just
            // below (FCS puts this arm first): that recursion would skip
            // through Function → SeqBlock → LetDecl and end at the catch-all's
            // letdecl.col + 1, flagging column-0 clauses with a spurious
            // FS0058 (and refusing a strict push of them).
            //
            // FCS keys this on `newCtxt = CtxtMatchClauses` (not on the head),
            // so the arm fires exactly once — at the MatchClauses push from the
            // FUNCTION dispatch — and not on every subsequent push inside the
            // function (which would over-relax the body SeqBlock pushed by
            // `->` and accept body columns left of the pattern).
            if let Context::MatchClauses { .. } = new_ctxt
                && let Context::Function { .. } = head
                && let [
                    ..,
                    limit_ctxt @ Context::LetDecl { .. },
                    Context::SeqBlock { .. },
                ] = rest
            {
                return PositionWithColumn {
                    pos: limit_ctxt.start_pos(),
                    col: limit_ctxt.start_pos().col as i32,
                };
            }

            // FCS L831-832, L902-903, L920-921: CtxtFun, CtxtFunction, CtxtThen,
            // CtxtElse, CtxtDo, CtxtWithAsAugment "place no limit until we hit a
            // leading construct". Recurse with strict=false so the SeqBlock/Paren
            // skip arms become active.
            if matches!(
                head,
                Context::Fun { .. }
                    | Context::Function { .. }
                    | Context::Then { .. }
                    | Context::Else { .. }
                    | Context::Do { .. }
                    | Context::WithAsAugment { .. }
            ) {
                strict = false;
                stack = rest;
                continue;
            }

            // FCS L790-793: `'begin match' / '(match' limited by minimum of the
            // two`. When the head is `CtxtMatch :: CtxtSeqBlock :: CtxtParen(BEGIN
            // | LPAREN) :: _`, return min(match.col, paren.col). Without this arm
            // the catch-all returns match.col alone, refusing valid clauses
            // aligned with the opener — `(match x with\n| _ -> 0)` pushes
            // CtxtMatchClauses at the `|`'s col 0, but match sits at col 1.
            if let Context::Match { pos: match_pos } = head
                && let [
                    ..,
                    Context::Paren {
                        pos: paren_pos,
                        opener: Opener::Paren | Opener::Begin,
                    },
                    Context::SeqBlock { .. },
                ] = rest
            {
                // FCS pairs the position with whichever context's column wins
                // (ties to the match, per its `<=`).
                let pos = if match_pos.col <= paren_pos.col {
                    *match_pos
                } else {
                    *paren_pos
                };
                return PositionWithColumn {
                    pos,
                    col: pos.col as i32,
                };
            }

            // FCS L804-807: more specific than the L827 `MatchClauses ::
            // CtxtMatch` rule below — when the match is paren/begin-wrapped
            // (`(match …)` / `begin match …end`), the limit is
            // `min(MatchClauses.col, Paren.col)`. Without this, `(match x
            // with\n| _ ->\n0)` with body aligned at the opener's column is
            // refused (MatchClauses at col 0, but the general arm returns
            // Match.col = 1, refusing body col 0). Must run before the
            // generic L827 arm.
            if let Context::MatchClauses { pos: mc_pos, .. } = head
                && let [
                    ..,
                    Context::Paren {
                        pos: paren_pos,
                        opener: Opener::Paren | Opener::Begin,
                    },
                    Context::SeqBlock { .. },
                    Context::Match { .. },
                ] = rest
            {
                // Same winner-pairing as the CtxtMatch arm above (ties to the
                // clauses context, per FCS's `<=`).
                let pos = if mc_pos.col <= paren_pos.col {
                    *mc_pos
                } else {
                    *paren_pos
                };
                return PositionWithColumn {
                    pos,
                    col: pos.col as i32,
                };
            }

            // FCS L823-824 / L827-828: `MatchClauses :: CtxtTry` and
            // `MatchClauses :: CtxtMatch` (the latter gated on relaxWhitespace2,
            // which we treat as always-on per modern FCS). The arrow body or
            // similar push under a match/try-with clause is limited by the
            // enclosing `try`/`match` column, not by the `|`'s column on
            // CtxtMatchClauses. Without this, `match x with\n    | _ ->\n0`
            // refuses the body SeqBlock push at col 0 (MatchClauses at col 4
            // imposes 4) and the fallback to `NotFirstInSeqBlock` at the arrow
            // mis-tokenises the body. Both arms match any `new_ctxt`.
            if let Context::MatchClauses { .. } = head
                && let Some(limit_ctxt) = rest.last()
                && matches!(limit_ctxt, Context::Try { .. } | Context::Match { .. })
            {
                return PositionWithColumn {
                    pos: limit_ctxt.start_pos(),
                    col: limit_ctxt.start_pos().col as i32,
                };
            }

            // FCS L971-972: permitted inner-construct alignment — pushing a
            // `CtxtIf` / `CtxtElse` / `CtxtThen` onto a `CtxtIf` limits to the
            // if's column exactly (not col+1), so `if …⏎then …⏎elif …⏎else …`
            // all aligned at the if's column is legal. Without this the catch-all
            // below returns if.col+1 and the (non-strict) then/elif/else push is
            // flagged offside — a spurious FS0058. `then`/`elif`/`else` are all
            // pushed via non-strict `push_ctxt`, so this only affects the offside
            // *diagnostic* (never a push decision), which is why the token-stream
            // corpus never forced it before the FS0058 emission stage.
            if let Context::If { pos: if_pos } = head
                && matches!(
                    new_ctxt,
                    Context::If { .. } | Context::Else { .. } | Context::Then { .. }
                )
            {
                return PositionWithColumn {
                    pos: *if_pos,
                    col: if_pos.col as i32,
                };
            }

            // FCS L956: `static member P with get() = …` — a `CtxtWithAsLet`
            // (the accessor `with`, pushed on top of the member's
            // `CtxtMemberHead`) is limited by the *member*'s column + 1, not by
            // the `with`'s own column. So a property accessor body may deindent
            // left of `with` as long as it stays right of the member keyword.
            // Without this arm the catch-all below uses WithAsLet.col + 1 and
            // flags a valid accessor body with a spurious FS0058. (FCS gates
            // this on `RelaxWhitespace`, which we treat as always-on per modern
            // FCS.) Must precede the catch-all, which handles WithAsLet heads.
            if let Context::WithAsLet { .. } = head
                && let Some(limit_ctxt @ Context::MemberHead { .. }) = rest.last()
            {
                return PositionWithColumn {
                    pos: limit_ctxt.start_pos(),
                    col: (limit_ctxt.start_pos().col as i32) + 1,
                };
            }

            // Catch-alls — FCS L983-988. Last statement in the loop: `return` so an
            // exhausted dispatch exits rather than looping on the same stack.
            let pos = head.start_pos();
            let col = pos.col as i32;
            return match head {
                // "These contexts all require indentation by at least one space"
                // (L983). All of FCS's listed contexts are now ported:
                // CtxtInterfaceHead, CtxtNamespaceHead, CtxtModuleHead,
                // CtxtException, CtxtModuleBody(false), CtxtIf, CtxtWithAsLet,
                // CtxtLetDecl, CtxtMemberHead, CtxtMemberBody.
                Context::If { .. }
                | Context::LetDecl { .. }
                | Context::WithAsLet { .. }
                | Context::NamespaceHead { .. }
                | Context::ModuleHead { .. }
                | Context::ModuleBody {
                    whole_file: false, ..
                }
                | Context::MemberHead { .. }
                | Context::MemberBody { .. }
                | Context::Exception { .. }
                | Context::InterfaceHead { .. } => PositionWithColumn { pos, col: col + 1 },
                // "These contexts can have their contents exactly aligning"
                // (L987). Of the listed contexts (CtxtParen / CtxtFor / CtxtWhen
                // / CtxtWhile / CtxtTypeDefns / CtxtMatch / CtxtModuleBody(true)
                // / CtxtNamespaceBody / CtxtTry / CtxtMatchClauses / CtxtSeqBlock),
                // ported: Paren, For, When, While, Match, MatchClauses, SeqBlock,
                // Try, NamespaceBody, ModuleBody(whole_file=true), TypeDefns.
                Context::Paren { .. }
                | Context::For { .. }
                | Context::When { .. }
                | Context::While { .. }
                | Context::Match { .. }
                | Context::MatchClauses { .. }
                | Context::SeqBlock { .. }
                | Context::Try { .. }
                | Context::NamespaceBody { .. }
                | Context::ModuleBody {
                    whole_file: true, ..
                }
                | Context::TypeDefns { .. } => PositionWithColumn { pos, col },
                // Vanilla / Fun / Function / Then / Else / Do / WithAsAugment
                // were handled by the recursive arms above (FCS L777, L831,
                // L902, L833, L920); reaching them here would be a bug.
                Context::Vanilla { .. }
                | Context::Fun { .. }
                | Context::Function { .. }
                | Context::Then { .. }
                | Context::Else { .. }
                | Context::Do { .. }
                | Context::WithAsAugment { .. } => {
                    unreachable!("recursive arm should have handled {head:?}")
                }
            };
        }
    }

    fn pop_ctxt(&mut self) -> Option<Context> {
        let popped = self.offside_stack.pop();
        if popped.is_some() {
            self.undentation_skip.pop();
        }
        popped
    }

    /// FCS's `pushCtxtSeqBlockAt` (LexFilter.fs:2770). Strict push of a
    /// CtxtSeqBlock anchored at the lookahead token; on refusal — and when
    /// `use_fallback` is set — pushes a `NotFirstInSeqBlock` recovery context
    /// at `fallback`'s position instead. The OBLOCKBEGIN (if `add_block_end`
    /// is `Yes`) lands at the actual anchoring token: lookahead when the
    /// strict push succeeded, `fallback` when we fell back.
    ///
    /// Returns whether a context was pushed (used internally by the wrappers;
    /// callers usually don't care).
    fn push_ctxt_seq_block_at(
        &mut self,
        strictness: PushStrictness,
        use_fallback: bool,
        fallback: &TokenTup<'a>,
        add_block_end: AddBlockEnd,
    ) {
        let target = self.peek_next_token_tup();
        let pushed = if let Some(ref t) = target {
            let anchor_is_eof = matches!(t.token, TokenContent::Eof);
            self.try_push_ctxt(
                strictness,
                false,
                anchor_is_eof,
                t.span.clone(),
                Context::SeqBlock {
                    first: true,
                    pos: t.start,
                    add_block_end,
                },
            )
        } else {
            false
        };
        if !pushed && use_fallback {
            // FCS L2772-2775: recovery context at the trigger position with
            // `NotFirstInSeqBlock` (no first-element flip), so a subsequent
            // EOF / offside token doesn't try to anchor virtuals here.
            self.push_ctxt(
                fallback.span.clone(),
                Context::SeqBlock {
                    first: false,
                    pos: fallback.start,
                    add_block_end,
                },
            );
        }
        if (pushed || use_fallback)
            && let AddBlockEnd::Yes = add_block_end
        {
            // OneSided (RARROW bodies) has no opening OBLOCKBEGIN — only a
            // closing ORIGHT_BLOCK_END. (LexFilter.fs:2780-2786)
            let anchor = if pushed {
                target
                    .as_ref()
                    .expect("strict push only succeeds with a target")
            } else {
                fallback
            };
            let bb = TokenTup {
                token: TokenContent::Virtual(Virtual::BlockBegin),
                span: anchor.span.clone(),
                start: anchor.start,
                end: anchor.end,
            };
            self.delay_token(bb);
        }
    }

    /// FCS's `pushCtxtSeqBlock` (LexFilter.fs:2764-2765): pushes *some* SeqBlock
    /// (real or recovery). `fallback` is the current dispatch token, used to
    /// anchor the recovery context and OBLOCKBEGIN when the lookahead can't
    /// legitimately open a new block.
    ///
    /// The `strict` flag is FCS's `strictIndentation`, not a hardcoded `true`:
    /// below F# 8 (or under `--strict-indentation-`) an offside SeqBlock push is
    /// **kept** (with a warning) rather than aborted, so the following construct
    /// nests inside the pushed context instead of becoming a sibling. The
    /// severity gate ([`Self::is_correct_indent`]) and this push decision are the
    /// same boolean in FCS — the *tree* depends on the language version, not just
    /// the diagnostic. See [`LanguageVersion::strict_indentation_is_error`].
    fn push_ctxt_seq_block(&mut self, fallback: &TokenTup<'a>, add_block_end: AddBlockEnd) {
        self.push_ctxt_seq_block_at(PushStrictness::VersionGated, true, fallback, add_block_end);
    }

    /// FCS's `tryPushCtxtSeqBlock` (LexFilter.fs:2767-2768): like
    /// [`Self::push_ctxt_seq_block`] but with no fallback — used for DO bodies
    /// and the WITH→CtxtWithAsAugment arm where a refused push leaves the stack
    /// alone (no spurious recovery SeqBlock, no OBLOCKBEGIN). `fallback` is
    /// unused but kept for symmetry. Threads `strictIndentation` for the same
    /// reason as [`Self::push_ctxt_seq_block`].
    fn try_push_ctxt_seq_block(&mut self, fallback: &TokenTup<'a>, add_block_end: AddBlockEnd) {
        self.push_ctxt_seq_block_at(PushStrictness::VersionGated, false, fallback, add_block_end);
    }

    fn head(&self) -> Option<&Context> {
        self.offside_stack.last()
    }

    /// FCS's `peekInitial` (LexFilter.fs:722). Read the first non-trivia
    /// token, push a top-level `SeqBlock(first=true, NoAddBlockEnd)` at its
    /// position, then delay it back so the main loop reprocesses it.
    fn peek_initial(&mut self) {
        self.initialized = true;
        let Some(first) = self.pull_raw() else {
            return;
        };
        let pos = first.start;
        let span = first.span.clone();
        self.delay_token(first);
        self.push_ctxt(
            span,
            Context::SeqBlock {
                first: true,
                pos,
                add_block_end: AddBlockEnd::No,
            },
        );
    }

    /// FCS's `insertToken` (LexFilter.fs:1375). Push `tt` back to delayed,
    /// return a synthetic carrying `virt` at the trigger's full range —
    /// FCS uses `(startPosOfTokenTup tokenTup, tokenTup.LexbufState.EndPos)`
    /// as the inserted token's lexbuf state.
    fn insert_token(&mut self, virt: Virtual, tt: TokenTup<'a>) -> TokenTup<'a> {
        let synth = TokenTup {
            token: TokenContent::Virtual(virt),
            span: tt.span.clone(),
            start: tt.start,
            end: tt.end,
        };
        self.delay_token(tt);
        synth
    }

    /// FCS's `insertTokenFromPrevPosToCurrentPos` (LexFilter.fs:1365). For
    /// `OBLOCKSEP` and friends — the synthetic spans the gap between the
    /// previous real token's end (shifted one column) and the current token's
    /// start. When the prev token ends at EOL and the current token starts at
    /// the first non-whitespace of the next line, this is a non-empty span
    /// covering the leading indentation (e.g. `\n    x` → span = `[prevEnd+1..x.start)`).
    fn insert_token_from_prev_to_current(
        &mut self,
        virt: Virtual,
        tt: TokenTup<'a>,
    ) -> TokenTup<'a> {
        let pos = tt.start;
        // ODUMMY preserves the prev-end snapshot from when it was queued —
        // mirrors FCS's per-`TokenTup` `LastTokenPos` (LexFilter.fs:1368
        // `tokenTup.LastTokenPos`). The motivating case is a `Greater(true)`
        // closer: ODUMMY is queued *before* `Greater` is returned, so the
        // OBLOCKSEP span that fires off the ODUMMY references the prior
        // real token's end (e.g. `Bar`), not `Greater`'s end (which by then
        // has updated `self.last_real_end`).
        let prev_end = match &tt.token {
            TokenContent::Dummy { prev_end, .. } => *prev_end,
            _ => self.last_real_end,
        };
        let span_start = (prev_end + 1).min(tt.span.start);
        let span = span_start..tt.span.start;
        let synth = TokenTup {
            token: TokenContent::Virtual(virt),
            span,
            start: pos,
            end: pos,
        };
        self.delay_token(tt);
        synth
    }

    /// FCS's `hwTokenFetch` (LexFilter.fs:1642) — the rule-dispatch loop.
    /// Returns one positioned token per call; recursive cases (`reprocess`,
    /// `reprocessWithoutBlockRule`) are inlined as a `continue`.
    fn hw_token_fetch(&mut self, mut use_block_rule: bool) -> Option<TokenTup<'a>> {
        loop {
            let tt = self.pop_next_token_tup()?;

            // Each rule either passes the token through to the next rule
            // (`Step::Pass`), restarts the dispatch loop (`Step::Restart` =>
            // `continue`), or yields a token to emit (`Step::Emit` => `return`).
            // The ordering mirrors FCS `hwTokenFetch`'s dispatch precedence; each
            // rule body carries its own `LexFilter.fs` cross-reference.
            macro_rules! step {
                ($call:expr) => {
                    match $call {
                        Step::Pass(t) => t,
                        Step::Restart => continue,
                        Step::Emit(t) => return Some(t),
                    }
                };
            }

            let tt = step!(self.predispatch(tt));
            // IN → JOIN_IN must run *before* the offside pops in `block_offside`
            // (FCS LexFilter.fs:1674 precedes the Vanilla/SeqBlock pops at
            // L1868): a query-join `in` on its own line would otherwise have its
            // `Vanilla` popped / an `OBLOCKSEP` inserted before the rewrite is
            // reached. The `predispatch` force-closure runs first, but the join
            // `in` balances its head so it is not closed there.
            let tt = step!(self.join_in_rewrite(tt));
            let tt = step!(self.head_transitions(tt, &mut use_block_rule));
            let tt = step!(self.block_offside(tt, &mut use_block_rule));
            let tt = step!(self.in_done_balances(tt));
            let tt = step!(self.keyword_offside_pops(tt));
            let tt = step!(self.clause_offside_pops(tt));
            let tt = step!(self.end_balances_augment(tt));
            let tt = step!(self.decl_offside_pops(tt));
            let tt = step!(self.module_type_pushes(tt));
            let tt = step!(self.binding_member_pushes(tt));
            let tt = step!(self.equals_pushes(tt));
            let tt = step!(self.expr_keyword_pushes(tt));
            let tt = step!(self.with_dispatch(tt));
            let tt = step!(self.try_finally_when_pushes(tt));
            let tt = step!(self.struct_interface_pushes(tt));
            // `infix_rhs_pushes` must precede the `CtxtVanilla` catch-all inside
            // `interp_and_paren`: FCS dispatches the "r.h.s. of an infix token
            // begins a new block" arm (LexFilter.fs:2330) *before* the ordinary-
            // token Vanilla push (LexFilter.fs:2617). When an infix operator
            // sits alone at the start of a continuation line (`a⏎ ||⏎ b`), the
            // Vanilla catch-all would otherwise consume it as an ordinary token,
            // so `infix_rhs_pushes` never runs and the next-line operand wrongly
            // starts a new statement. The two act on disjoint token sets (an
            // infix op is never a paren / interp opener), so ordering
            // `infix_rhs_pushes` first is otherwise inert.
            let tt = step!(self.infix_rhs_pushes(tt));
            let tt = step!(self.interp_and_paren(tt));

            // Default: pass token through.
            return Some(tt);
        }
    }
}

impl<'a, I: Iterator<Item = (Result<Token<'a>, LexError>, Span)>> Iterator for Filter<'a, I> {
    type Item = (Result<FilteredToken<'a>, LexError>, Span);

    fn next(&mut self) -> Option<Self::Item> {
        if !self.initialized {
            self.peek_initial();
        }
        let tt = self.hw_token_fetch(true)?;
        let span = tt.span;
        match tt.token {
            TokenContent::Real(t) => {
                self.last_real_end = span.end;
                self.last_real_was_atomic_end = is_atomic_expr_end(&t);
                Some((Ok(FilteredToken::Raw(t)), span))
            }
            TokenContent::Virtual(v) => Some((Ok(FilteredToken::Virtual(v)), span)),
            TokenContent::Err(e) => Some((Err(e), span)),
            TokenContent::Eof => None,
            // hw_token_fetch's ODUMMY arm consumes dummies in-loop; one
            // reaching here means a rule above this loop bypassed it.
            TokenContent::Dummy { .. } => unreachable!("ODUMMY escaped hw_token_fetch"),
        }
    }
}

//! `type` definitions, members, and struct/class/interface bodies.

use crate::common::assert_filtered_streams_match;

/// Simplest type-alias `type T = int`. TYPE is swallowed (FCS rewrites
/// it as `TYPE_COMING_SOON` / `TYPE_IS_HERE` via `insertComingSoonTokens`
/// at LexFilter.fs:2583, both mapping to `FSharpTokenKind.None` and
/// filtered by the public-API tokenizer). EQUALS replaces the
/// `CtxtTypeDefns(_, None)` with `(_, Some equalsEnd)` and pushes a
/// `SeqBlock(AddBlockEnd)` for the RHS (LexFilter.fs:2226-2230).
#[test]
fn diff_filtered_type_alias_int() {
    assert_filtered_streams_match("type T = int\n");
}

/// Discriminated union with leading-bar arms on fresh lines. Exercises
/// the BAR-grace=2 column gate (LexFilter.fs:1813) — without it, the
/// inner SeqBlock anchored at `A` (col 4) would treat the `|` at col 2
/// as offside and prematurely emit OBLOCKEND.
#[test]
fn diff_filtered_type_du_leading_bars() {
    assert_filtered_streams_match("type T =\n    | A\n    | B\n");
}

/// Discriminated union without a leading bar. The first arm sits at
/// `type`'s column under `CtxtTypeDefns`; the next arm with `|` aligns
/// with `type` too. Exercises the grace=-1 special case
/// (LexFilter.fs:1823): when the next token's column matches the
/// CtxtTypeDefns position and it's NOT an `isTypeSeqBlockElementContinuator`
/// (i.e. not `|` and not a reprocessed virtual), the inner SeqBlock pops.
#[test]
fn diff_filtered_type_du_no_leading_bar() {
    assert_filtered_streams_match("type T =\n    A\n    | B\n");
}

/// Two type definitions joined by `and`. `and` aligned with `type`
/// must NOT close the prior `CtxtTypeDefns` — `isTypeContinuator`
/// (LexFilter.fs:288) bumps the offside-pop guard from
/// `tokenStartCol <= offsidePos.Column` to `+1 <=`, keeping the
/// declaration scope open. Companion to `diff_filtered_let_rec_and`
/// for the type-definition family.
#[test]
fn diff_filtered_type_and_type() {
    assert_filtered_streams_match("type T = int\nand U = string\n");
}

/// Type definition followed by an unrelated top-level binding. The
/// `let` aligned with `type` is past `isTypeContinuator`'s accept set,
/// so the CtxtTypeDefns offside-pop fires.
#[test]
fn diff_filtered_type_then_let() {
    assert_filtered_streams_match("type T = int\nlet x = 1\n");
}

/// Simplest possible class member. Exercises the `MEMBER → CtxtMemberHead`
/// push (LexFilter.fs:2203), the `EQUALS + CtxtMemberHead → CtxtMemberBody +
/// SeqBlock` transition (LexFilter.fs:2271-2275), and the offside-pop that
/// closes the body at EOF (LexFilter.fs:2002-2005, emits `ODECLEND`).
#[test]
fn diff_filtered_simple_member() {
    assert_filtered_streams_match("type T() =\n    member _.f x = x + 1\n");
}

/// `static member` — exercises the same MemberHead/MemberBody machinery as the
/// plain member case, but also confirms that two consecutive member keywords
/// (`static` then `member`) collapse into a single MemberHead push (the
/// already-MemberHead guard at LexFilter.fs:2203 suppresses the second push).
#[test]
fn diff_filtered_static_member() {
    assert_filtered_streams_match("type T =\n    static member g () = 42\n");
}

/// Two adjacent members in a single type. Drives the multi-member pop
/// cascade (LexFilter.fs:2179-2195): the second `member` keyword arrives
/// while the first member's CtxtMemberBody (and its inner SeqBlock) are
/// still on the stack; the cascade pops them down to MemberBody before
/// reprocessing the keyword.
#[test]
fn diff_filtered_two_members() {
    assert_filtered_streams_match(
        "type T() =\n    member _.f x = x + 1\n    member _.g y = y - 1\n",
    );
}

/// Constructor declaration. `NEW` with a lookahead `(` pushes CtxtMemberHead
/// (LexFilter.fs:2214-2218). The body is a unit-returning constructor body.
#[test]
fn diff_filtered_constructor_new() {
    assert_filtered_streams_match("type T =\n    new() = { }\n");
}

/// `static let` inside a type. LET arriving with CtxtMemberHead on top pops
/// the head and pushes CtxtLetDecl anchored at the member-head's column
/// (LexFilter.fs:2146-2151), emitting OLET.
#[test]
fn diff_filtered_static_let() {
    assert_filtered_streams_match("type T =\n    static let cache = 0\n");
}

/// Bare type augmentation. `type T with\n    member ...\n` — WITH arrives
/// with `CtxtTypeDefns :: ...` on the stack and routes through the L2362
/// dispatch arm's else-branch (L2424+): lookahead is `MEMBER` (not in the
/// IDENT/RBRACE/access-modifier set), so we push `CtxtWithAsAugment` + a
/// SeqBlock(AddBlockEnd) and emit the raw WITH (not OWITH). At EOF the
/// body block pops silently and `CtxtWithAsAugment`'s offside-pop emits
/// `ODECLEND` (LexFilter.fs:2025-2029) before `CtxtTypeDefns` closes.
#[test]
fn diff_filtered_type_augment_with_member() {
    assert_filtered_streams_match("type T with\n    member _.f x = x\n");
}

/// Multi-line property accessor. `member _.P\n    with get() = 1\n    and set v = ()\n`
/// — WITH arrives with `CtxtMemberHead :: ...` on the stack and again
/// routes through the L2362 dispatch else-branch: lookahead `get` is an
/// IDENT, so the IDENT branch fires (L2367-2403) pushing `CtxtWithAsLet`
/// at the surrounding type's column (lookahead column > `with`'s end col
/// → tokenStartPos branch at L2394). Each `and`-prefixed accessor reuses
/// the same WithAsLet context.
#[test]
fn diff_filtered_property_accessor_multiline() {
    assert_filtered_streams_match(
        "type T() =\n    member _.P\n        with get() = 1\n        and set v = ()\n",
    );
}

/// Single-line property accessor. `member _.P with get() = 1\n` — same
/// L2362 else-branch + IDENT-lookahead path as the multi-line case, but
/// `with` and the IDENT share the line so the limCtxt.StartPos branch
/// fires (L2401). No WithAsAugment is involved; this test pins the
/// boundary against the multi-line case.
#[test]
fn diff_filtered_property_accessor_singleline() {
    assert_filtered_streams_match("type T() =\n    member _.P with get() = 1\n");
}

/// `type T with\nend\n` — augmentation body closed immediately by END.
/// Pins the dedicated END+WithAsAugment balance arm (LexFilter.fs:1717-
/// 1722): END at the WithAsAugment anchor (col >= pos.col) pops the
/// context, delays an `ODUMMY END` so cascading rules can fire, and
/// emits `OEND`. END is BALANCED for WithAsAugment via
/// `tokenBalancesHeadContext` (L1262), so the force-closure path short-
/// circuits — only the dedicated balance arm reaches OEND. The queued
/// Dummy is suppressed by `is_seq_block_element_continuator` via the
/// recursive ODUMMY-unwrap (L380) since `END` is itself a SeqBlock
/// continuator.
#[test]
fn diff_filtered_type_augment_empty_body_end() {
    assert_filtered_streams_match("type T with\nend\n");
}

/// `type T = struct\n    val x: int\nend\n` — STRUCT body in a TypeDefns.
/// FCS pushes `CtxtParen(Opener::Struct) + SeqBlock(NoAddBlockEnd)` at
/// the STRUCT keyword (LexFilter.fs:2291-2302), guarded on
/// `CtxtSeqBlock :: (CtxtModuleBody | CtxtTypeDefns) :: _`. The END at
/// column 0 balances the STRUCT-Opener pair (`parenTokensBalance`
/// LexFilter.fs:417) and emits END (`OEND` only when the inner block
/// is AddBlockEnd; STRUCT uses NoAddBlockEnd here so END is raw).
#[test]
fn diff_filtered_struct_body_in_typedefn() {
    assert_filtered_streams_match("type T = struct\n    val x: int\nend\n");
}

/// `module M = struct\nend\n` — STRUCT body in a ModuleBody. Same FCS
/// L2291 push site, just the other arm of the host-context guard
/// (CtxtModuleBody instead of CtxtTypeDefns).
#[test]
fn diff_filtered_struct_body_in_module() {
    assert_filtered_streams_match("module M = struct\nend\n");
}

/// Multi-line struct: the `struct` token sits on its own line, indented
/// under `type T =`. The inner SeqBlock anchors at struct's column; the
/// `end` aligns at the same column. Pins two behaviours:
///   * the L2291 guard fires when STRUCT is the first real token on a
///     fresh line under TypeDefns;
///   * after END balances Paren(Struct) (L1698), FCS queues an
///     ODUMMY(END) (L1712) which fires the SeqBlock(NotFirst) OBLOCKSEP
///     rule (L1912) under the outer CtxtTypeDefns — END is *not* an
///     `isTypeSeqBlockElementContinuator` (L346), so OBLOCKSEP emits
///     between END and EOF.
#[test]
fn diff_filtered_struct_body_multiline() {
    assert_filtered_streams_match("type T =\n    struct\n        val x: int\n    end\n");
}

/// `type I = interface\n    abstract X: int\nend\n` — basic INTERFACE
/// body under a TypeDefns. FCS pushes `CtxtParen(Opener::Interface) +
/// SeqBlock(AddBlockEnd)` at the INTERFACE keyword (LexFilter.fs:2537-2564).
/// Guard: stack matches `CtxtSeqBlock :: CtxtTypeDefns(_, Some equalsEndPos)
/// :: _`, INTERFACE immediately follows `=` (LastTokenPos == equalsEndPos),
/// and the lookahead token is one of {ABSTRACT/MEMBER/STATIC/OVERRIDE/...}
/// at column >= limitPos.Column + 1 (where limitPos = typePos when
/// `allowDeindent`, else INTERFACE's start). Unlike STRUCT, INTERFACE's
/// inner SeqBlock is AddBlockEnd, so virtual block-end fires on close.
#[test]
fn diff_filtered_interface_body_in_typedefn() {
    assert_filtered_streams_match("type I = interface\n    abstract X: int\nend\n");
}

/// Multi-line interface: `interface` on its own line under `type I =`.
/// `allowDeindent = false` (interface.EndPos.Line != equalsEndPos.Line),
/// so limitPos = interface.StartPos for the lookahead check. Also
/// exercises the OBLOCKSEP-after-END path (same shape as the multiline
/// struct test) since END is not an `isTypeSeqBlockElementContinuator`.
#[test]
fn diff_filtered_interface_body_multiline() {
    assert_filtered_streams_match("type I =\n    interface\n        abstract X: int\n    end\n");
}

/// Empty interface body: `type I = interface\nend\n`. Exercises the
/// `END` branch of the L2537 lookahead — the next token is END at column
/// 0, which is allowed because `lookaheadTokenStartPos.Column >=
/// typePos.Column` (0 >= 0).
#[test]
fn diff_filtered_interface_body_empty() {
    assert_filtered_streams_match("type I = interface\nend\n");
}

/// Deindented body, `interface` on same line as `=`. `allowDeindent =
/// true` so limitPos = typePos (column 0), and a body member at column 2
/// satisfies the `>= limitPos.Column + 1` (`>= 1`) check — even though
/// it's deindented below `interface`'s own column (8).
#[test]
fn diff_filtered_interface_body_deindented() {
    assert_filtered_streams_match("type I = interface\n  abstract X: int\nend\n");
}

/// `type C = class\n    val x: int\nend\n` — basic CLASS body under a
/// TypeDefns. Unlike STRUCT/INTERFACE the FCS L2573 push site is
/// unconditional (`| CLASS, _ ->`); CLASS has no other meaning in F#, so
/// there's no guard. The inner SeqBlock is AddBlockEnd (matching
/// INTERFACE, not STRUCT). Relies on the L948 `undentationLimit` arm
/// (`Paren(CLASS|STRUCT|INTERFACE) :: SeqBlock :: TypeDefns`) added with
/// the INTERFACE slice: without it the body anchor falls back to the
/// CLASS keyword's column.
#[test]
fn diff_filtered_class_body_in_typedefn() {
    assert_filtered_streams_match("type C = class\n    val x: int\nend\n");
}

/// Multi-line class: `class` on its own line under `type C =`. Same
/// shape as the multiline STRUCT/INTERFACE tests; also exercises the
/// OBLOCKSEP-after-END path under CtxtTypeDefns (END is not an
/// `isTypeSeqBlockElementContinuator`).
#[test]
fn diff_filtered_class_body_multiline() {
    assert_filtered_streams_match("type C =\n    class\n        val x: int\n    end\n");
}

/// Empty class body: `type C = class\nend\n`. END is the lookahead
/// immediately after CLASS; the inner SeqBlock falls back to recovery
/// (NotFirstInSeqBlock) because END can't anchor a fresh SeqBlock, but
/// OBLOCKBEGIN still emits at CLASS's span in both rust and FCS.
#[test]
fn diff_filtered_class_body_empty() {
    assert_filtered_streams_match("type C = class\nend\n");
}

/// Deindented class body — `class` on the same line as `=`, body at
/// col 2 (below CLASS's col 9). Strict push succeeds because the L948
/// undentation arm gives limit = type.col + 1 = 1; 2 >= 1. Pins that the
/// L948 arm covers CLASS the same way it covers STRUCT/INTERFACE.
#[test]
fn diff_filtered_class_body_deindented() {
    assert_filtered_streams_match("type C = class\n  val x: int\nend\n");
}

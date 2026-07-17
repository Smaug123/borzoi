//! `CtxtInterfaceHead` (`OffsideInterfaceMember`) paths.

use crate::common::assert_filtered_streams_match;

// EOF / `;;` shapes for `let … = function` are *not* asserted by a diff
// test: FCS's force-closure path emits virtuals whose range uses
// `Position.ColumnMinusOne` of the EOF lookahead (LexFilter.fs:644), so
// every EOF-closed virtual gets a synthetic span like `[1:0..1:16)`
// that our `insert_token` (which copies the EOF token's actual span)
// does not yet reproduce. That divergence is general to every context
// that closes at EOF — fixing it belongs in its own slice rather than
// this CtxtFunction port. The unconditional MatchClauses push (no EOF
// guard) is correct in the code and required by FCS's semantics — see
// the FUNCTION push site in src/lexfilter/mod.rs.

/// `type C() =\n    inherit obj()\n    interface I with\n        member _.M() = 1\n`
/// — canonical CtxtInterfaceHead path. INTERFACE appears in the type
/// body but *not* as the first token after `=` (LastTokenPos points at
/// `)` from `obj()`), so the L2536 paren guard fails and the L2567
/// catch-all fires. FCS pushes `CtxtInterfaceHead(interface.col=4)` and
/// rewrites `INTERFACE` to `OINTERFACE_MEMBER` (→
/// `FSharpTokenKind.OffsideInterfaceMember`). The follow-up `with` then
/// reaches the L2362 dispatch via the InterfaceHead head, the lookahead
/// `member` (col 8) is not a binding-head class so it falls into the
/// `_ ->` arm at L2424, and since `member.col=8 > InterfaceHead.col=4`
/// the L2436 recovery does *not* fire, so FCS pushes
/// `CtxtWithAsAugment(InterfaceHead.col=4)` + a SeqBlock and returns
/// raw `With`. This is the genuine observable divergence — without
/// CtxtInterfaceHead, Rust emits `Interface` (not OffsideInterfaceMember).
#[test]
fn diff_filtered_interface_head_basic() {
    assert_filtered_streams_match(
        "type C() =\n    inherit obj()\n    interface I with\n        member _.M() = 1\n",
    );
}

/// `type C() =\n    inherit obj()\n    interface I with\n    member _.M() = 1\n`
/// — InterfaceHead WITH-recovery path. The lookahead after `with`
/// is `member` at col 4, equal to InterfaceHead's anchor col 4, so the
/// L2436 guard fires (`lookaheadTokenStartPos.Column <= limCtxt.StartCol
/// && CtxtInterfaceHead`) and FCS returns raw `WITH` without pushing
/// CtxtWithAsAugment or a SeqBlock. The `member` then participates in
/// the outer TypeDefns SeqBlock as a sibling, separated by an
/// `OffsideBlockSep`. Pins L2436 specifically — without it the `_ ->`
/// catch-all would push WithAsAugment + SeqBlock and emit an
/// `OffsideBlockBegin` before `member`.
#[test]
fn diff_filtered_interface_head_with_recovery() {
    assert_filtered_streams_match(
        "type C() =\n    inherit obj()\n    interface I with\n    member _.M() = 1\n",
    );
}

/// `type C() =\n    inherit obj()\n    interface I with\n        member _.M() = 1\n    end\n`
/// — `end` aligned with InterfaceHead. `isInterfaceContinuator END`
/// returns true (LexFilter.fs:266-275), so the L1960 offside guard
/// uses `tokenStartCol + 1 <= offsidePos.Column` (i.e. `5 <= 4`),
/// which is false: InterfaceHead stays open and the inner WithAsAugment
/// closes via END instead, emitting `OffsideEnd`.
#[test]
fn diff_filtered_interface_head_with_explicit_end() {
    assert_filtered_streams_match(
        "type C() =\n    inherit obj()\n    interface I with\n        member _.M() = 1\n    end\n",
    );
}

/// `type C with\n    interface I with\n        member _.M() = 1\n` —
/// interface inside a type-augmentation. The outer WITH pushes
/// `CtxtWithAsAugment` + a SeqBlock, then `interface` arrives. The L2536
/// paren-arm guard fails (no TypeDefns with `Some equalsEndPos` on the
/// stack) so the L2567 catch-all fires. Exercises CtxtInterfaceHead
/// living under WithAsAugment + SeqBlock rather than under
/// TypeDefns + SeqBlock.
#[test]
fn diff_filtered_interface_head_in_type_augment() {
    assert_filtered_streams_match("type C with\n    interface I with\n        member _.M() = 1\n");
}

/// `type C() =\n    inherit obj()\n    interface I with\n        member _.M() = 1\n    interface J with\n        member _.N() = 2\n`
/// — two interface implementations in the same type body. The second
/// `interface J` at col 4 closes the first InterfaceHead via offside
/// (`tokenStartCol=4 <= offsidePos.Column=4`); FCS reprocesses the
/// token, the inner WithAsAugment + MemberBody unwind via `OffsideDeclEnd`,
/// then an `OffsideBlockSep` aligns the second interface at col 4,
/// and the second CtxtInterfaceHead push happens. Pins the offside-pop
/// arm (LexFilter.fs:1960) specifically.
#[test]
fn diff_filtered_interface_head_two_in_one_type() {
    assert_filtered_streams_match(
        "type C() =\n    inherit obj()\n    interface I with\n        member _.M() = 1\n    interface J with\n        member _.N() = 2\n",
    );
}

/// `type C() =\n    inherit obj()\n    interface I\n` — bare interface
/// declaration with no `with` clause. Pins the silent-pop behavior:
/// CtxtInterfaceHead's `endTokenForACtxt` returns None (it falls into
/// the default `_ -> None` arm at L1545), so close-on-EOF emits no
/// virtual at the interface's range. Only the surrounding TypeDefns /
/// SeqBlock close at EOF.
#[test]
fn diff_filtered_interface_head_bare() {
    assert_filtered_streams_match("type C() =\n    inherit obj()\n    interface I\n");
}

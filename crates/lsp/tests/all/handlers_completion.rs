//! Tests for the `textDocument/completion` handler (Stage 3.3b: member `recv.`
//! completion).
//!
//! Drives `handle` directly against a "restored" temp project whose
//! `AssemblyEnv` carries a real `System.Runtime.dll` (so `System.String` and its
//! members resolve). Pins that member completion:
//! - offers the receiver type's **public instance** members (`Length`, methods)
//!   after `s.` and mid-`s.Le`, and for a literal receiver `"hi".Le`;
//! - excludes **static** members (`Empty`) — a value receiver, not a type path;
//! - offers **nothing** on an untyped / deferred receiver (D5: silence).

use crate::common::{runtime_project_state, runtime_project_state_files};
use borzoi::handlers::completion;
use borzoi::server::State;
use lsp_types::{
    CompletionParams, CompletionResponse, PartialResultParams, Position, TextDocumentIdentifier,
    TextDocumentPositionParams, Url, WorkDoneProgressParams,
};

fn params(uri: &Url, line: u32, character: u32) -> CompletionParams {
    CompletionParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: None,
    }
}

/// The completion labels at `(line, character)`, or `None` when the handler
/// declined (not a completable position, or nothing to offer).
fn labels(state: &mut State, uri: &Url, line: u32, character: u32) -> Option<Vec<String>> {
    match completion::handle(state, params(uri, line, character)) {
        Some(CompletionResponse::Array(items)) => {
            Some(items.into_iter().map(|i| i.label).collect())
        }
        Some(CompletionResponse::List(list)) => {
            Some(list.items.into_iter().map(|i| i.label).collect())
        }
        None => None,
    }
}

/// Member completion folds only the Compile **prefix** up to the cursor's file:
/// a completion in file 0 of a two-file project resolves the receiver against
/// file 0 alone and never folds the later file. Pins the resolution-slice wiring
/// (`resolved_prefix_for` with the file's Compile index, not `usize::MAX`) — a
/// regression to the whole-project fold would cache both files.
#[test]
fn member_completion_folds_only_the_prefix_up_to_the_cursor_file() {
    let src = "module M\nlet s = \"hi\"\nlet n = s.\n";
    let (mut state, uris) =
        runtime_project_state_files(&[("Lib.fs", src), ("Later.fs", "module Later\nlet z = 1\n")]);
    let proj = uris[0]
        .to_file_path()
        .unwrap()
        .parent()
        .unwrap()
        .join("P.fsproj");

    // Completion still works (soundness): `s.` offers `System.String` members.
    let labels = labels(&mut state, &uris[0], 2, 10).expect("member completions after `s.`");
    assert!(
        labels.iter().any(|l| l == "Length"),
        "expected `Length` among {labels:?}"
    );

    // ...but only file 0 was folded; the later file never was.
    assert_eq!(
        state.semantic.cached_resolved_len(&proj),
        Some(1),
        "completion on file 0 folds only file 0, not the later Compile file"
    );
}

#[test]
fn trailing_dot_offers_string_instance_members() {
    // `s.` — completion offers `System.String`'s public instance members,
    // including the `Length` property and instance methods.
    let src = "module M\nlet s = \"hi\"\nlet n = s.\n";
    let (mut state, uri) = runtime_project_state(src);
    // Cursor right after the dot: `let n = s.` — the `.` is column 9, cursor at 10.
    let labels = labels(&mut state, &uri, 2, 10).expect("member completions after `s.`");
    assert!(
        labels.iter().any(|l| l == "Length"),
        "expected `Length` among {labels:?}"
    );
    // Methods belong in the candidate set (a completion list is callable
    // candidates, not a type assertion).
    assert!(
        labels.iter().any(|l| l == "ToString"),
        "expected an instance method (`ToString`) among {labels:?}"
    );
    // A **static** member must not appear — it needs a type path, not a receiver.
    assert!(
        !labels.iter().any(|l| l == "Empty"),
        "static `Empty` must not appear among instance members: {labels:?}"
    );
    // A constructor is not a name-completable member on a value receiver.
    assert!(
        !labels.iter().any(|l| l == ".ctor"),
        "a constructor (`.ctor`) must not appear: {labels:?}"
    );
}

#[test]
fn partial_member_offers_instance_members() {
    // Mid-`s.Le` — the same instance-member set (completion is prefix-agnostic on
    // our side; the client filters by the typed prefix).
    let src = "module M\nlet s = \"hi\"\nlet n = s.Le\n";
    let (mut state, uri) = runtime_project_state(src);
    // Cursor inside `Le`: `let n = s.Le`, `Le` at columns 10..12, cursor at 11.
    let labels = labels(&mut state, &uri, 2, 11).expect("member completions mid-`s.Le`");
    assert!(
        labels.iter().any(|l| l == "Length"),
        "expected `Length` among {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l == "Empty"),
        "static `Empty` must not appear: {labels:?}"
    );
}

#[test]
fn literal_receiver_partial_member_offers_instance_members() {
    // `"hi".Le` — the `DotGet` shape; the receiver's inferred type is
    // `System.String`, so the same instance members are offered.
    let src = "module M\nlet n = \"hi\".Le\n";
    let (mut state, uri) = runtime_project_state(src);
    // `"hi".Le` — `Le` at columns 13..15 on line 1; cursor at 14.
    let labels =
        labels(&mut state, &uri, 1, 14).expect("member completions for a literal receiver");
    assert!(
        labels.iter().any(|l| l == "Length"),
        "expected `Length` among {labels:?}"
    );
}

#[test]
fn untyped_receiver_offers_nothing() {
    // An unannotated parameter `x` used as a receiver has an *open* type inference
    // never grounds, so `x.` offers nothing (D5: silence over noise — never a
    // guess against some default type).
    let src = "module M\nlet f x = x.\n";
    let (mut state, uri) = runtime_project_state(src);
    // `let f x = x.` — the `.` is column 11, cursor at 12.
    let result = labels(&mut state, &uri, 1, 12);
    assert_eq!(
        result, None,
        "an untyped receiver must offer no completions"
    );
}

#[test]
fn non_member_position_offers_nothing() {
    // A cursor not after a member dot (on a plain binder) is not a member-access
    // position — completion declines (member completion is the only kind so far).
    let src = "module M\nlet s = \"hi\"\nlet n = s\n";
    let (mut state, uri) = runtime_project_state(src);
    // On the binder `n` (line 2, column 4).
    assert_eq!(labels(&mut state, &uri, 2, 4), None);
    // On the receiver `s` (line 2, column 8) — a bare name, not `s.`.
    assert_eq!(labels(&mut state, &uri, 2, 8), None);
}

#[test]
fn whitespace_after_a_completed_access_offers_nothing() {
    // Codex round-3 finding: a cursor after trivia *following* a completed member
    // access is no longer editing the member — offering `s`'s members there would
    // be noise. Trivia may only be skipped to reach a trailing *dot* (`s. |`);
    // an ident behind the trivia (`s.Length |`) must decline.
    let src = "module M\nlet s = \"hi\"\nlet n = s.Length \n";
    let (mut state, uri) = runtime_project_state(src);
    // The space after `Length` on line 2 is column 16; cursor after it, at 17.
    assert_eq!(
        labels(&mut state, &uri, 2, 17),
        None,
        "whitespace after a completed access must not offer the receiver's members"
    );
    // The next line (blank via the trailing newline): same rule across lines.
    assert_eq!(
        labels(&mut state, &uri, 3, 0),
        None,
        "the line after a completed access must not offer completions"
    );
}

#[test]
fn space_after_a_trailing_dot_still_offers_members() {
    // The one trivia skip that stays: `s. |` (cursor a space after a trailing
    // dot) is still a member-access edit — the dot is the nearest code token.
    let src = "module M\nlet s = \"hi\"\nlet n = s. \n";
    let (mut state, uri) = runtime_project_state(src);
    // `let n = s. ` — the dot at column 9, the space at 10; cursor after it, 11.
    let labels = labels(&mut state, &uri, 2, 11).expect("member completions a space after `s.`");
    assert!(
        labels.iter().any(|l| l == "Length"),
        "expected `Length` among {labels:?}"
    );
}

#[test]
fn literal_trailing_dot_is_scoped_out() {
    // `"hi".` (literal receiver, trailing dot) is the one shape completion does
    // NOT cover: under error recovery the `DOT_GET_EXPR` collapses — the receiver
    // `"hi"` becomes a bare sibling `CONST_EXPR` and the `.` an orphan token — so
    // no member-access node survives to anchor the receiver. Scoped out (a
    // parser-side follow-up, recorded in the impl plan); it must offer nothing
    // rather than crash.
    let src = "module M\nlet n = \"hi\".\n";
    let (mut state, uri) = runtime_project_state(src);
    // The `.` is at column 12 on line 1; cursor right after it, at 13.
    let result = labels(&mut state, &uri, 1, 13);
    assert_eq!(
        result, None,
        "literal trailing-dot is scoped out (parser recovery collapses the DotGet)"
    );
}

#[test]
fn deeper_dot_declines_rather_than_offering_the_head_members() {
    // Soundness (D5): a completion after a *later* dot (`s.Length.`) has receiver
    // `s.Length` (an `int`), not the head `s` (a `string`). This handler does not
    // chain the receiver's type through the intervening member, so it must
    // **decline** rather than wrongly offer `System.String`'s members — a bogus
    // list would be worse than none.
    let src = "module M\nlet s = \"hi\"\nlet n = s.Length.\n";
    let (mut state, uri) = runtime_project_state(src);
    // The trailing dot after `Length` on line 2: `let n = s.Length.`, that `.` is
    // at column 16; cursor right after it, at 17.
    let result = labels(&mut state, &uri, 2, 17);
    assert_eq!(
        result, None,
        "a deeper-dot completion must not offer the head receiver's members"
    );
}

#[test]
fn parenthesized_literal_receiver_offers_instance_members() {
    // `("hi").Le` — codex round-2 finding: parens are transparent to inference
    // (the type is recorded on the inner expression, never the `Paren` node), so
    // the receiver classification must peel them rather than decline.
    let src = "module M\nlet n = (\"hi\").Le\n";
    let (mut state, uri) = runtime_project_state(src);
    // `let n = ("hi").Le` — `Le` at columns 15..17 on line 1; cursor at 16.
    let labels =
        labels(&mut state, &uri, 1, 16).expect("member completions for a parenthesized literal");
    assert!(
        labels.iter().any(|l| l == "Length"),
        "expected `Length` among {labels:?}"
    );
}

#[test]
fn parenthesized_binder_receiver_offers_instance_members() {
    // `(s).Le` — the peeled receiver is an ident, typed via its binder.
    let src = "module M\nlet s = \"hi\"\nlet n = (s).Le\n";
    let (mut state, uri) = runtime_project_state(src);
    // `let n = (s).Le` — `Le` at columns 12..14 on line 2; cursor at 13.
    let labels =
        labels(&mut state, &uri, 2, 13).expect("member completions for a parenthesized binder");
    assert!(
        labels.iter().any(|l| l == "Length"),
        "expected `Length` among {labels:?}"
    );
}

#[test]
fn deeper_dot_in_dot_get_chain_declines() {
    // The `DOT_GET_EXPR` analogue (codex finding): `"hi".Length.To` — the dot
    // before `To` is inside the same member-path `LONG_IDENT`, but the real
    // receiver is `"hi".Length` (`int`), not `"hi"` (`string`). Must decline, not
    // offer `System.String`'s members.
    let src = "module M\nlet n = \"hi\".Length.To\n";
    let (mut state, uri) = runtime_project_state(src);
    // `"hi".Length.To` on line 1: `To` at columns 20..22; cursor inside, at 21.
    let result = labels(&mut state, &uri, 1, 21);
    assert_eq!(
        result, None,
        "a deeper dot in a DotGet chain must not offer the inner receiver's members"
    );
}

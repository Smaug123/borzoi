//! End-to-end: hover on a project-local symbol — defined in this file, an
//! earlier Compile-order file, or a signature — ends with its `///`
//! documentation, rendered through the same renderer as a referenced
//! assembly's `.xml`. Which doc FCS attaches is pinned by the
//! `xml_doc_source_diff` differential; these tests pin the wiring: the right
//! declaration's doc reaches the hover, and nothing else does.

use borzoi::handlers::hover;
use lsp_types::{
    HoverContents, HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    WorkDoneProgressParams,
};

use crate::common::runtime_project_state_files;

/// The separator hover puts between a symbol's signature and its documentation.
const DOC_SEPARATOR: &str = "\n\n---\n\n";

/// The documentation part of the hover at the `nth` occurrence of `needle` in
/// file `file` (cursor one character into it), or `None` when the hover has
/// none. Panics when there is no hover at all.
fn doc_at(files: &[(&str, &str)], file: usize, needle: &str, nth: usize) -> Option<String> {
    let (mut state, uris) = runtime_project_state_files(files);
    let uri: Url = uris[file].clone();
    let text = files[file].1;
    let offset = text
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("occurrence {nth} of {needle:?}"))
        .0
        + 1;
    let line = text[..offset].matches('\n').count() as u32;
    let col = (offset - text[..offset].rfind('\n').map_or(0, |i| i + 1)) as u32;
    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position {
                line,
                character: col,
            },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let hover = hover::handle(&mut state, params).expect("a hover");
    let body = match hover.contents {
        HoverContents::Markup(m) => m.value,
        other => panic!("expected markup, got {other:?}"),
    };
    body.split_once(DOC_SEPARATOR)
        .map(|(_, doc)| doc.to_string())
}

#[test]
fn a_same_file_function_shows_its_doc_at_a_use_and_at_its_definition() {
    let src = "module M\n/// Adds one.\nlet inc x = x + 1\nlet y = inc 2\n";
    let files = [("Lib.fs", src)];
    assert_eq!(doc_at(&files, 0, "inc", 1).as_deref(), Some("Adds one."));
    assert_eq!(doc_at(&files, 0, "inc", 0).as_deref(), Some("Adds one."));
}

#[test]
fn an_earlier_files_value_shows_its_structured_doc() {
    let a = "module A\n/// <summary>Doubles.</summary>\n/// <param name=\"x\">The input.</param>\nlet double (x: int) = x * 2\n";
    let b = "module B\nlet four = A.double 2\n";
    let files = [("A.fs", a), ("B.fs", b)];
    let doc = doc_at(&files, 1, "double", 0).expect("a doc");
    assert!(doc.starts_with("Doubles."), "{doc}");
    assert!(doc.contains("**Parameters**"), "{doc}");
    assert!(doc.contains("`x` — The input."), "{doc}");
}

#[test]
fn types_and_union_cases_show_their_own_docs() {
    let src = "module M\n/// A shape.\ntype Shape =\n    /// Round.\n    | Circle of float\n    /// Not round.\n    | Square of float\nlet s = Square 1.0\nlet t : Shape = s\n";
    let files = [("Lib.fs", src)];
    assert_eq!(
        doc_at(&files, 0, "Square", 1).as_deref(),
        Some("Not round.")
    );
    assert_eq!(doc_at(&files, 0, "Circle", 0).as_deref(), Some("Round."));
    assert_eq!(doc_at(&files, 0, "Shape", 1).as_deref(), Some("A shape."));
}

#[test]
fn a_local_binding_shows_its_doc() {
    let src = "module M\nlet f () =\n    /// The local.\n    let z = 1\n    z\n";
    let files = [("Lib.fs", src)];
    assert_eq!(doc_at(&files, 0, "z", 1).as_deref(), Some("The local."));
}

#[test]
fn a_neighbours_doc_is_never_shown() {
    // The first block is given to the ordinary comment by FCS's delayed grab
    // point; only the second reaches `v`.
    let src = "module M\n/// Not mine.\n// ordinary\n/// Mine.\nlet v = 1\nlet w = v\n";
    let files = [("Lib.fs", src)];
    assert_eq!(doc_at(&files, 0, "v", 1).as_deref(), Some("Mine."));
    // A doc after the attribute is attached to nothing.
    let src = "module M\n[<System.Obsolete(\"o\")>]\n/// Lost.\nlet v = 1\nlet w = v\n";
    let files = [("Lib.fs", src)];
    assert_eq!(doc_at(&files, 0, "v", 1), None);
}

#[test]
fn a_use_outside_a_signed_file_shows_the_signature_doc() {
    let fsi = "module A\n/// From the signature.\nval v: int\n";
    let fs = "module A\n///\nlet v = 1\nlet w = v\n";
    let user = "module B\nlet x = A.v\n";
    let files = [("A.fsi", fsi), ("A.fs", fs), ("B.fs", user)];
    assert_eq!(
        doc_at(&files, 2, "v", 0).as_deref(),
        Some("From the signature.")
    );
    // Inside the implementation, FCS falls back from the blank doc to the
    // signature's, which hover does not locate: it shows none rather than the
    // implementation's.
    assert_eq!(doc_at(&files, 1, "v", 1), None);
}

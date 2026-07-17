//! Tests for the `textDocument/documentSymbol` handler.
//!
//! Goes through `handle` directly with a populated `State` rather than over
//! JSON-RPC — that harness lands with Stage 8 and is shared with the other
//! handlers. The state-level tests pin the algorithm; the JSON-RPC tests
//! later just confirm the wire-format round-trip.

use borzoi::handlers::document_symbol;
use borzoi::server::State;
use lsp_types::{
    ClientCapabilities, DocumentSymbol, DocumentSymbolClientCapabilities, DocumentSymbolParams,
    DocumentSymbolResponse, PartialResultParams, Position, Range, SymbolKind,
    TextDocumentClientCapabilities, TextDocumentIdentifier, Url, WorkDoneProgressParams,
};

fn params(uri: &Url) -> DocumentSymbolParams {
    DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

/// Modern client capabilities: advertises hierarchical documentSymbol
/// support, the response shape virtually every real LSP client uses today
/// (VS Code, neovim's lsp clients, Emacs lsp-mode). The spec default
/// without this is the flat `SymbolInformation[]` shape — exercised by
/// `falls_back_to_flat_when_capability_absent` below.
fn hierarchical_caps() -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            document_symbol: Some(DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn state_with_hierarchical() -> State {
    let mut state = State::default();
    state.set_client_capabilities(hierarchical_caps());
    state
}

/// Drive the handler over a single in-memory buffer (no project, no
/// `.fsproj` lookup — the URI deliberately lives outside any directory tree
/// the workspace might find a project in). The state advertises
/// hierarchical-support so we get the `Nested` response shape the rest of
/// these tests pin against.
fn run(text: &str) -> Vec<DocumentSymbol> {
    let mut state = state_with_hierarchical();
    let uri = Url::parse("inmemory:///Sample.fs").unwrap();
    state.docs.insert(uri.clone(), text.to_string());
    match document_symbol::handle(&mut state, params(&uri)) {
        Some(DocumentSymbolResponse::Nested(syms)) => syms,
        Some(DocumentSymbolResponse::Flat(_)) => panic!("expected nested response"),
        None => Vec::new(),
    }
}

#[test]
fn lists_a_single_let_value() {
    let syms = run("let x = 1\n");
    assert_eq!(syms.len(), 1, "{syms:#?}");
    assert_eq!(syms[0].name, "x");
    assert_eq!(syms[0].kind, SymbolKind::VARIABLE);
    // Range pins the `x` identifier at line 0, columns 4..5.
    assert_eq!(
        syms[0].selection_range,
        Range {
            start: Position {
                line: 0,
                character: 4
            },
            end: Position {
                line: 0,
                character: 5
            },
        }
    );
    assert_eq!(syms[0].selection_range, syms[0].range);
}

#[test]
fn function_binding_is_a_function_kind() {
    let syms = run("let f x = x\n");
    assert_eq!(syms.len(), 1, "{syms:#?}");
    assert_eq!(syms[0].name, "f");
    assert_eq!(syms[0].kind, SymbolKind::FUNCTION);
}

#[test]
fn multiple_top_level_bindings_appear_in_source_order() {
    let syms = run("let a = 1\nlet b = 2\nlet c = 3\n");
    let names: Vec<_> = syms.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);
}

#[test]
fn module_qualified_bindings_appear_under_their_module() {
    // The exports list is flat (Stage 1 doesn't model nested-module
    // hierarchy yet), but a named-module binding still shows up.
    let syms = run("module Shared\nlet foo = 1\n");
    let names: Vec<_> = syms.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["foo"]);
}

#[test]
fn parameters_are_not_top_level_symbols() {
    // The parameter `x` in `let f x = x` is a binder, but it isn't an export
    // — `filter_map` on `DefKind::Parameter` keeps it out of the outline.
    let syms = run("let f x = x\n");
    assert!(
        syms.iter().all(|s| s.name != "x"),
        "parameter `x` leaked into the outline: {syms:#?}"
    );
}

#[test]
fn missing_buffer_returns_none() {
    // No `didOpen` for this URI → no buffer → the handler returns `None`
    // (which the JSON-RPC layer serialises as null, never an error).
    let mut state = State::default();
    let uri = Url::parse("inmemory:///Missing.fs").unwrap();
    let resp = document_symbol::handle(&mut state, params(&uri));
    assert!(resp.is_none(), "{resp:?}");
}

#[test]
fn parse_error_still_returns_an_outline() {
    // The CST parser is panic-safe and produces a tree with ERROR tokens.
    // A file that doesn't parse cleanly still has a recognisable
    // `ImplFile`; whichever bindings the parser recovered show up.
    // Concretely: `let x = 1` is fine, then the trailing `let y =` is
    // incomplete; `x` should still appear in the outline.
    let syms = run("let x = 1\nlet y =\n");
    assert!(
        syms.iter().any(|s| s.name == "x"),
        "expected `x` even with trailing parse error: {syms:#?}"
    );
}

#[test]
fn empty_buffer_returns_an_empty_list() {
    // An empty file is a valid (empty) anonymous module: handler returns
    // `Some(Nested(vec![]))`, not `None`. Some clients prefer the empty
    // list to "null" — both are spec-legal but the empty list is more
    // informative.
    let syms = run("");
    assert!(syms.is_empty(), "{syms:#?}");
}

/// LSP spec: when the client never advertised
/// `hierarchicalDocumentSymbolSupport`, the response shape is the
/// deprecated `SymbolInformation[]`, not `DocumentSymbol[]`. Old or minimal
/// clients depend on this; a server that always returns `Nested` would have
/// its responses rejected.
#[test]
fn falls_back_to_flat_when_capability_absent() {
    let mut state = State::default(); // No `set_client_capabilities` call.
    let uri = Url::parse("inmemory:///Sample.fs").unwrap();
    state.docs.insert(uri.clone(), "let x = 1\n".to_string());

    let resp = document_symbol::handle(&mut state, params(&uri)).expect("a response");
    let flat = match resp {
        DocumentSymbolResponse::Flat(infos) => infos,
        DocumentSymbolResponse::Nested(_) => {
            panic!("client did not advertise hierarchical; expected Flat")
        }
    };
    assert_eq!(flat.len(), 1);
    assert_eq!(flat[0].name, "x");
    assert_eq!(flat[0].kind, SymbolKind::VARIABLE);
    assert_eq!(flat[0].location.uri, uri);
}

/// The parser is run panic-safely (`cst_panic_safe`); the handler must
/// survive arbitrary short input rather than unwind through the request
/// loop and terminate the server. Mirrors
/// `diagnostics::tests::parse_diagnostics_never_panics_and_in_bounds`.
mod panic_safe {
    use super::*;
    use proptest::prelude::*;

    fn run_or_none(text: &str) -> Option<Vec<DocumentSymbol>> {
        let mut state = state_with_hierarchical();
        let uri = Url::parse("inmemory:///Sample.fs").unwrap();
        state.docs.insert(uri.clone(), text.to_string());
        match document_symbol::handle(&mut state, params(&uri)) {
            Some(DocumentSymbolResponse::Nested(syms)) => Some(syms),
            Some(DocumentSymbolResponse::Flat(_)) => panic!("unexpected flat response"),
            None => None,
        }
    }

    proptest! {
        /// For arbitrary short input the handler must not panic. We don't
        /// assert anything about the symbols returned — only that the call
        /// completes.
        #[test]
        fn handler_never_panics(src in "(?s).{0,80}") {
            let _ = run_or_none(&src);
        }
    }
}

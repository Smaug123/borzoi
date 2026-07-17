//! `textDocument/documentSymbol` — the outline view in the editor sidebar.
//!
//! File-only: parses the buffer (with the active preprocessor symbols),
//! extracts its top-level exports via [`crate::handlers::file_export_symbols`],
//! and emits one [`DocumentSymbol`] per binding. Cross-file and cross-assembly
//! enrichment doesn't change the *outline* of this file — it changes what those
//! names *resolve to*, which lives in the other handlers.
//!
//! Nested-module / type-definition members are not yet exported by sema, so
//! they don't appear in the outline today. The [`DocumentSymbol`] hierarchy
//! is wire-compatible with adding them as `children` later — no client-side
//! change required.

use borzoi_cst::syntax::{AstNode, ImplFile};
use lsp_types::{
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, SymbolInformation, SymbolKind,
};

use crate::cst_panic_safe::parse_with_symbols;
use crate::handlers::{file_export_symbols, symbol_information};
use crate::server::State;

/// Run the documentSymbol handler. Returns `None` when there is nothing
/// useful to show (no buffer, or the buffer doesn't parse to a recognisable
/// implementation file) — never an error envelope, never a server crash on
/// a parser-side panic.
///
/// The response shape (`DocumentSymbol[]` vs the deprecated
/// `SymbolInformation[]`) is negotiated against the client's
/// `hierarchicalDocumentSymbolSupport` capability. The spec default — when
/// the client advertised nothing — is the flat shape; only a client that
/// explicitly opted in gets the hierarchical one.
pub fn handle(state: &mut State, params: DocumentSymbolParams) -> Option<DocumentSymbolResponse> {
    let uri = params.text_document.uri;
    let text = state.docs.get(&uri).cloned()?;
    let symbols = state.symbols_for_uri(&uri);
    let lang = state.lang_version_for_uri(&uri);
    let parse = parse_with_symbols(&text, &symbols, lang)?;
    let file = ImplFile::cast(parse.root)?;
    let exports = file_export_symbols(&text, &file);

    if state.supports_hierarchical_document_symbols() {
        let outline: Vec<DocumentSymbol> = exports
            .into_iter()
            .map(|(name, kind, range)| document_symbol(name, kind, range))
            .collect();
        Some(DocumentSymbolResponse::Nested(outline))
    } else {
        let flat: Vec<SymbolInformation> = exports
            .into_iter()
            .map(|(name, kind, range)| symbol_information(&uri, name, kind, range))
            .collect();
        Some(DocumentSymbolResponse::Flat(flat))
    }
}

/// Helper: build a leaf [`DocumentSymbol`] whose whole-range and
/// selection-range both equal `range`. Once nested modules export members,
/// the whole-range will widen to cover the body; until then the binder range
/// is the most accurate thing we have.
fn document_symbol(name: String, kind: SymbolKind, range: lsp_types::Range) -> DocumentSymbol {
    // `deprecated` is `#[deprecated]` on `DocumentSymbol` itself in `lsp-types`
    // 0.95, but the field still exists and serialises; `tags` is the
    // replacement (empty here). The `allow(deprecated)` keeps the struct
    // literal building under `-D warnings` until lsp-types removes the field.
    #[allow(deprecated)]
    DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
    }
}

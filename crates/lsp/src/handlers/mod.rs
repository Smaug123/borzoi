//! LSP request handlers — one module per request method, plus shared helpers
//! that map between sema's byte-offset world and LSP's
//! (line, UTF-16 column) world.

use std::collections::HashMap;
use std::path::Path;

use borzoi_cst::syntax::ImplFile;
use borzoi_sema::{
    AssemblyEnv, DefKind, InferredFile, ProjectItems, Resolution, ResolvedFile, SyntaxRecovery, Ty,
    resolve_file,
};
use lsp_types::{Location, Range, SymbolInformation, SymbolKind, Url};
use rowan::{TextRange, TextSize};

use crate::paths::{lexically_normalize, paths_equal};
use crate::position::offset_to_position;

pub mod completion;
pub mod definition;
pub mod definition_availability;
pub mod diagnostic;
pub mod document_symbol;
pub mod hover;
pub mod references;
pub mod semantic_tokens;
pub mod workspace_diagnostic;
pub mod workspace_symbol;

/// A rowan byte range as an LSP [`Range`] over `text`.
pub fn range_to_lsp(text: &str, range: TextRange) -> Range {
    Range {
        start: offset_to_position(text, usize::from(range.start())),
        end: offset_to_position(text, usize::from(range.end())),
    }
}

/// The top-level exported symbols of one parsed file as `(name, kind, range)`
/// triples, in source order. Shared by `textDocument/documentSymbol` and
/// `workspace/symbol` so both surface exactly the same set: every export sema
/// records whose [`DefKind`] maps to a listable [`SymbolKind`] (parameters and
/// pattern-locals are dropped via [`symbol_kind_for`]).
///
/// Resolution runs with an empty [`ProjectItems`] and a default [`AssemblyEnv`]
/// because a file's *outline* — the names it declares and where — does not
/// depend on what those names resolve to elsewhere. Cross-file / cross-assembly
/// enrichment changes resolution, not the outline.
pub fn file_export_symbols(
    text: &str,
    file: &ImplFile,
    recovery: &SyntaxRecovery,
) -> Vec<(String, SymbolKind, Range)> {
    let resolved = resolve_file(
        file,
        &ProjectItems::default(),
        &AssemblyEnv::default(),
        recovery,
    );
    resolved
        .exports()
        .iter()
        .filter_map(|export| {
            // A single-file resolve materialises no signature surface, so
            // every export's def is in this file's own arena.
            let def = resolved.def(export.def()?);
            // Skip active-pattern **case** handles: these are cross-file identity
            // exports (Stage 3a), each pointing at the shared recognizer span with
            // kind [`DefKind::ActivePattern`], so listing them would advertise
            // `(|Even|Odd|)` as two duplicate `FUNCTION` symbols at one range. The
            // recognizer's own outline symbol is a separate concern (it is not an
            // export); until it lands the AP contributes no outline entry.
            if def.kind == DefKind::ActivePattern {
                return None;
            }
            let kind = symbol_kind_for(def.kind)?;
            Some((def.name.clone(), kind, range_to_lsp(text, def.range)))
        })
        .collect()
}

/// Map a sema [`DefKind`] to an LSP [`SymbolKind`]. Returns `None` for kinds
/// that should not appear in a symbol list (parameters, match-clause locals).
/// Top-level exports today are always [`DefKind::Value`], but the match stays
/// exhaustive against sema's enum evolving.
pub fn symbol_kind_for(kind: DefKind) -> Option<SymbolKind> {
    match kind {
        DefKind::Value { is_function: true } => Some(SymbolKind::FUNCTION),
        DefKind::Value { is_function: false } => Some(SymbolKind::VARIABLE),
        // A `type` definition — abbreviation, record, union, enum, or class. We
        // do not yet distinguish those sub-kinds in `Def`, so `CLASS` is the
        // umbrella LSP kind (refine to `STRUCT`/`ENUM`/`INTERFACE` once the
        // repr is carried on the def).
        DefKind::Type => Some(SymbolKind::CLASS),
        // A union case is a constructor in the value namespace; `ENUM_MEMBER` is
        // the closest LSP symbol kind. Cases are not exported as items yet, so
        // this arm is reached only once cross-file case export lands.
        DefKind::UnionCase => Some(SymbolKind::ENUM_MEMBER),
        // An exception is a top-level type-like declaration (its constructor
        // lives in the value namespace); `CLASS` is the umbrella outline kind,
        // matching `Type`. Like union cases, exception constructors are not
        // exported as items yet, so this arm is reached only once cross-file
        // export lands.
        DefKind::ExceptionCase => Some(SymbolKind::CLASS),
        // An active-pattern recognizer is a top-level function value; its cases
        // are value-namespace tags (closest LSP kind `ENUM_MEMBER`, as for union
        // cases). Neither is exported as an item yet, so these arms are reached
        // only once cross-file active-pattern export lands.
        DefKind::ActivePattern => Some(SymbolKind::FUNCTION),
        DefKind::ActivePatternCase => Some(SymbolKind::ENUM_MEMBER),
        // An enum case is the value-namespace member of an enum type;
        // `ENUM_MEMBER` is the exact LSP kind. Not exported as an item yet, so
        // this arm is reached only once cross-file enum-case export lands.
        DefKind::EnumCase => Some(SymbolKind::ENUM_MEMBER),
        // A static member of a type definition (`static member Red = 1`);
        // `PROPERTY` is the closest LSP outline kind (the emit-eligible subset
        // is properties and single static methods). Not exported as an item, so
        // this arm is reached only once cross-file member export lands.
        DefKind::Member => Some(SymbolKind::PROPERTY),
        // A type parameter is a definition-scoped binder, never a document-outline
        // symbol — like parameters and match-clause locals.
        DefKind::Parameter | DefKind::PatternLocal | DefKind::TypeParam => None,
    }
}

/// Build a flat [`SymbolInformation`] at `range` in `uri`. Shared by the flat
/// `documentSymbol` response shape and `workspace/symbol` (which is always
/// flat). `container_name` is `None` until sema exports nested-module
/// hierarchy.
pub fn symbol_information(
    uri: &Url,
    name: String,
    kind: SymbolKind,
    range: Range,
) -> SymbolInformation {
    // `deprecated` is `#[deprecated]` on `SymbolInformation` in lsp-types 0.95,
    // but the field is still required to construct it; `tags` is the
    // replacement (empty here). The `allow(deprecated)` keeps the literal
    // building under `-D warnings` until lsp-types removes the field.
    #[allow(deprecated)]
    SymbolInformation {
        name,
        kind,
        tags: None,
        deprecated: None,
        location: Location {
            uri: uri.clone(),
            range,
        },
        container_name: None,
    }
}

/// The resolution recorded at `byte` in `file`, under the rule the v1 handlers
/// (definition / references / hover) share:
///
/// 1. **Containment is inclusive at both ends.** A cursor at the very end of
///    an identifier's range (a frequent click pattern) should still resolve.
/// 2. **The smallest containing range wins, deferral or not.** The smallest
///    range is the name actually under the cursor; anything larger is an
///    enclosing construct that names something else. A dotted path
///    `Shared.foo` records the *whole path* as `Item(foo)` and the qualifier
///    `Shared` as `Deferred(QualifiedAccess)`, and a cursor on `Shared` is on
///    the module, not on `foo`: FCS reports the module there. Passing over the
///    deferral to the enclosing answer would serve `foo`'s definition for the
///    module — a wrong target, where the deferral is an honest "nothing".
/// 3. **Ties go to the range the cursor is strictly inside.** At a boundary
///    between two adjacent equal-length ranges (`a+b`, cursor before `b`), the
///    range starting there beats the one ending there, so the answer does not
///    depend on hash-map iteration order. Remaining ties fall to the earlier
///    start, which makes the choice total.
///
/// Returns `None` when no recorded range contains `byte` at all.
pub fn smallest_resolution_at(file: &ResolvedFile, byte: usize) -> Option<Resolution> {
    smallest_resolution_with_range(file, byte).map(|(_, r)| r)
}

/// Same containment rule as [`smallest_resolution_at`], but also returns the
/// matching `TextRange`. Hover uses the range to scope its tooltip to the
/// symbol under the cursor; the other handlers don't need it.
pub fn smallest_resolution_with_range(
    file: &ResolvedFile,
    byte: usize,
) -> Option<(TextRange, Resolution)> {
    let byte = TextSize::try_from(byte).ok()?;
    // Attribute-type resolutions live in their own map (EX-3 §2(d) — they
    // answer a different query, FCS's suffix-first candidate walk, and are
    // differentially gated separately), but for navigation they are ordinary
    // resolutions: chain them in so go-to-definition / hover on `[<MyAttr>]`
    // reach the attribute's type. The two maps' ranges never collide — an
    // attribute name's range holds no other resolution.
    file.resolutions()
        .iter()
        .chain(file.attribute_resolutions().iter())
        .filter(|(range, _)| range.start() <= byte && byte <= range.end())
        .min_by_key(|(range, _)| (range.len(), byte == range.end(), range.start()))
        .map(|(r, res)| (*r, *res))
}

/// The resolution go-to-definition **serves** for a cursor at `byte`, with the
/// range it was recorded at: the resolver's answer
/// ([`smallest_resolution_with_range`]) unless it deferred or recorded nothing
/// there, in which case inference's member resolution
/// ([`smallest_member_resolution_with_range`]) stands in, and failing that the
/// resolver's deferral (or nothing) is the verdict.
///
/// The one definition of that precedence. Go-to-definition, the
/// "no definition available" explanation, and the whole-project differential
/// against FCS all read it, so what the user is shown and what the oracle grades
/// cannot drift apart. `inferred` is `None` where there is no inference to
/// consult (a signature buffer, or the single-file fallback).
pub fn served_resolution_with_range(
    file: &ResolvedFile,
    inferred: Option<&InferredFile>,
    byte: usize,
) -> Option<(TextRange, Resolution)> {
    match smallest_resolution_with_range(file, byte) {
        Some(served @ (_, res)) if !matches!(res, Resolution::Deferred(_)) => Some(served),
        deferred_or_none => inferred
            .and_then(|i| smallest_member_resolution_with_range(i, byte))
            .or(deferred_or_none),
    }
}

/// The smallest inferred-type entry whose range contains `byte`, if any — the
/// type-side analogue of [`smallest_resolution_with_range`]. Hover uses it to
/// surface an expression's inferred type (literals today) where no name
/// resolution applies; "smallest" picks the innermost expression at the cursor.
pub fn smallest_inferred_type_with_range(
    inferred: &InferredFile,
    byte: usize,
) -> Option<(TextRange, &Ty)> {
    let byte = TextSize::try_from(byte).ok()?;
    inferred
        .types()
        .iter()
        .filter(|(range, _)| range.start() <= byte && byte <= range.end())
        .min_by_key(|(range, _)| range.len())
        .map(|(range, ty)| (*range, ty))
}

/// The smallest inference-recorded **member resolution** whose range contains
/// `byte`, if any (Stage 3.3b). Where the resolver only records
/// [`Resolution::Deferred`]`(QualifiedAccess)` at a member-name (`recv.Name`)
/// range, inference may have resolved the member against the receiver's type; the
/// LSP layers this over the resolver's answer so hover / go-to-definition on the
/// member name behave like a resolver-resolved [`Resolution::Member`]. Returns
/// the member's own `TextRange` (for scoping the hover tooltip) and the
/// resolution.
pub fn smallest_member_resolution_with_range(
    inferred: &InferredFile,
    byte: usize,
) -> Option<(TextRange, Resolution)> {
    let byte = TextSize::try_from(byte).ok()?;
    inferred
        .member_resolutions()
        .iter()
        .filter(|(range, _)| range.start() <= byte && byte <= range.end())
        .min_by_key(|(range, _)| (range.len(), byte == range.end(), range.start()))
        .map(|(range, res)| (*range, *res))
}

/// The URI the LSP client almost certainly knows `target_path` by: prefer an
/// open document whose path is path-equal (case-insensitive on
/// Windows/macOS, lexically-normalised) to `target_path` over a freshly-
/// constructed `Url::from_file_path(target_path)`. Used by every handler
/// that surfaces a `Location` pointing at a project file the editor may
/// already have open under a different spelling.
///
/// Falls back to building a `file://` URI from `target_path` when no open
/// buffer matches. Returns `None` only when the path can't be turned into
/// any URI at all (a non-absolute path on platforms `Url::from_file_path`
/// rejects, or a path with no representation).
pub fn preferred_uri(target_path: &Path, docs: &HashMap<Url, String>) -> Option<Url> {
    let target = lexically_normalize(target_path);
    for url in docs.keys() {
        if let Ok(doc_path) = url.to_file_path()
            && paths_equal(&lexically_normalize(&doc_path), &target)
        {
            return Some(url.clone());
        }
    }
    Url::from_file_path(target_path).ok()
}

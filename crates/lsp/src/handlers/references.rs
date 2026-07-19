//! `textDocument/references` — every use of the cursor's symbol, project-wide.
//!
//! Resolves the cursor to a [`Resolution`] (using the same containment +
//! prefer-non-`Deferred` rules as goto-definition), then iterates every file
//! in the [`ResolvedProject`] and collects ranges whose recorded resolution
//! is exactly that one ([`Resolution`] is `Copy + PartialEq`). A range in the
//! label position of an application argument shaped `name = value` is omitted:
//! without the callee's type, that syntax could be either a named-argument
//! label or the left operand of an ordinary Boolean equality. For
//! [`Resolution::Local`] only the cursor's file can contain references —
//! locals are file-scoped by definition — so the iteration short-circuits.
//!
//! `include_declaration` excludes the binder's own self-range when `false`,
//! per LSP spec. The definition's range is `def.range` from
//! [`ResolvedFile::def`] (for `Local`) or [`ResolvedProject::item_def`]
//! (for `Item`); `Entity`/`Member` have no source-side declaration, so
//! `include_declaration` is a no-op there.
//!
//! A [`Resolution::Deferred`] / [`Resolution::Unresolved`] / no-resolution
//! cursor yields `Some(vec![])` (the spec says return the empty list, never
//! an error), so a stale "Find Usages" panel clears cleanly rather than
//! sticking with the previous answer.

use std::collections::{HashMap, HashSet};

use borzoi_cst::syntax::{AstNode, Expr, ImplFile, SyntaxNode};
use borzoi_sema::{
    AssemblyEnv, ProjectItems, Resolution, ResolvedFile, ResolvedProject, resolve_file,
};
use lsp_types::{Location, ReferenceParams, Url};
use rowan::TextRange;

use crate::cst_panic_safe::parse_with_symbols;
use crate::handlers::{preferred_uri, range_to_lsp, smallest_resolution_with_range};
use crate::paths::{lexically_normalize, paths_equal};
use crate::position::position_to_offset;
use crate::semantic::ProjectParses;
use crate::server::State;

/// Run the find-references handler. Always returns `Some` when the cursor
/// has a buffer; an empty vec means "no references found", which clears the
/// client's reference panel rather than leaving it stale.
pub fn handle(state: &mut State, params: ReferenceParams) -> Option<Vec<Location>> {
    let pos = params.text_document_position.position;
    let uri = params.text_document_position.text_document.uri.clone();
    let include_declaration = params.context.include_declaration;
    let text = state.docs.get(&uri).cloned()?;
    let byte = position_to_offset(&text, pos);

    if let Some(locs) = project_references(state, &uri, byte, include_declaration) {
        return Some(locs);
    }
    // Fallback: orphan / partial-project buffer — single-file references
    // only. `Local` is the only resolution kind that survives without a
    // project context anyway.
    Some(single_file_references(
        state,
        &uri,
        &text,
        byte,
        include_declaration,
    ))
}

/// Project-level lookup. Returns `None` when the URI isn't in any project,
/// the project failed to evaluate, the URI isn't a Compile item, or the
/// cursor has no recorded resolution — letting the handler fall through to
/// the single-file fallback. Returns `Some(vec![])` for a cursor whose
/// resolution is [`Resolution::Deferred`] / [`Resolution::Unresolved`] (we
/// know the project layout; we just have nothing meaningful to report).
fn project_references(
    state: &mut State,
    uri: &Url,
    byte: usize,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let path = uri.to_file_path().ok()?;
    let project = state.workspace.owning_project(&path)?;
    let State {
        semantic,
        workspace,
        docs,
        ..
    } = state;
    let resolved = semantic.resolved_project_for(&project, workspace, docs)?;
    let parses = semantic
        .parses_for_project(&project, workspace, docs)?
        .clone();
    let target_file_idx = parses
        .paths
        .iter()
        .position(|p| paths_equal(&lexically_normalize(p), &lexically_normalize(&path)))?;
    let (target_range, target_res) =
        smallest_resolution_with_range(resolved.file(target_file_idx), byte)?;
    if matches!(target_res, Resolution::Deferred(_) | Resolution::Unresolved) {
        return Some(Vec::new());
    }
    if ambiguous_argument_label_ranges(parses.files[target_file_idx].file.syntax())
        .contains(&target_range)
    {
        return Some(Vec::new());
    }
    let decl_anchor = declaration_anchor(&resolved, target_res, target_file_idx);
    Some(collect_locations(
        &parses,
        &resolved,
        uri,
        target_res,
        decl_anchor,
        include_declaration,
        docs,
    ))
}

/// The (file_idx, range) of the binder for `target_res`, if any. `Local` is
/// anchored in the cursor's file; `Item` is anchored in whatever file
/// declares the export. `Entity`/`Member` have no source anchor — so
/// `include_declaration=false` is a no-op there.
fn declaration_anchor(
    resolved: &ResolvedProject,
    target_res: Resolution,
    cursor_file_idx: usize,
) -> Option<(usize, TextRange)> {
    match target_res {
        Resolution::Local(id) => {
            let range = resolved.file(cursor_file_idx).def(id).range;
            Some((cursor_file_idx, range))
        }
        Resolution::Item(_) => {
            let (file_idx, def) = resolved.item_def(target_res)?;
            Some((file_idx, def.range))
        }
        Resolution::Entity(_) | Resolution::Member { .. } => None,
        Resolution::Deferred(_) | Resolution::Unresolved => None,
    }
}

/// Walk every file in the project, collecting ranges whose resolution
/// matches `target_res`. For a [`Resolution::Local`] only the cursor's file
/// can match (locals never escape their file), so the iteration is bounded
/// to one file in that case.
fn collect_locations(
    parses: &ProjectParses,
    resolved: &ResolvedProject,
    request_uri: &Url,
    target_res: Resolution,
    decl_anchor: Option<(usize, TextRange)>,
    include_declaration: bool,
    docs: &HashMap<Url, String>,
) -> Vec<Location> {
    let file_range = match target_res {
        Resolution::Local(_) => {
            // Only the cursor's file can have references to a local. Find
            // it via the same path-lookup we used in `project_references`;
            // we already know which one it is via `decl_anchor`'s file_idx,
            // so just use that.
            if let Some((idx, _)) = decl_anchor {
                idx..idx + 1
            } else {
                // Should be unreachable: a `Local` always has a decl_anchor
                // (its own binder). Defensive empty range.
                0..0
            }
        }
        _ => 0..resolved.len(),
    };

    // Build a `Location` for a `(file, range)`, or `None` when no URI can be
    // formed. For the cursor's own file, preserve the request URI (the client may
    // have it open under a casing that differs from `parses.paths`); for other
    // files, prefer an open buffer's URI if one matches lexically, else build from
    // the disk path.
    let make_loc = |file_idx: usize, range: TextRange| -> Option<Location> {
        let uri = if same_path(&parses.paths[file_idx], request_uri) {
            request_uri.clone()
        } else {
            preferred_uri(&parses.paths[file_idx], docs)?
        };
        Some(Location {
            uri,
            range: range_to_lsp(&parses.texts[file_idx], range),
        })
    };

    let mut out = Vec::new();
    let mut decl_emitted = false;
    for file_idx in file_range {
        let file = resolved.file(file_idx);
        let ambiguous_labels =
            ambiguous_argument_label_ranges(parses.files[file_idx].file.syntax());
        for (range, _res) in matching_in_file(file, target_res) {
            if ambiguous_labels.contains(&range) {
                continue;
            }
            if matches!(decl_anchor, Some((f, r)) if f == file_idx && r == range) {
                decl_emitted = true;
                if !include_declaration {
                    // Skip the binder's own self-range when the client opted out.
                    continue;
                }
            }
            if let Some(loc) = make_loc(file_idx, range) {
                out.push(loc);
            }
        }
    }
    // An active-pattern case's declaration span (the recognizer name) is recorded
    // as the *recognizer* (`Resolution::Local`), not the case's `Item`, so
    // `matching_in_file` never yields it (unlike an ordinary value or union case,
    // whose declaration self-resolves to the target). When the client wants the
    // declaration, add the anchor explicitly — deduped by `decl_emitted`.
    if include_declaration
        && !decl_emitted
        && let Some((f, r)) = decl_anchor
        && let Some(loc) = make_loc(f, r)
    {
        out.push(loc);
    }
    out
}

/// Whether a `parses.paths` entry is the same file as `request_uri` under
/// the platform's path-equality rule.
fn same_path(path: &std::path::Path, request_uri: &Url) -> bool {
    let Ok(req_path) = request_uri.to_file_path() else {
        return false;
    };
    paths_equal(&lexically_normalize(path), &lexically_normalize(&req_path))
}

/// Every `(range, res)` in `file.resolutions()` — chained with the
/// attribute-type resolutions, which live in their own map (EX-3 §2(d)) but
/// are ordinary occurrences for reference listing (`[<MyAttr>]` is a use of
/// the attribute type) — where `res == target`. Wraps the filter as a
/// function so the caller iterates with a stable shape.
fn matching_in_file(
    file: &ResolvedFile,
    target: Resolution,
) -> impl Iterator<Item = (TextRange, Resolution)> + '_ {
    file.resolutions()
        .iter()
        .chain(file.attribute_resolutions().iter())
        .filter(move |(_, r)| **r == target)
        .map(|(r, res)| (*r, *res))
}

/// Single-file fallback: parse the buffer in isolation. Resolves locals /
/// parameters / same-file top-level bindings — anything cross-file or
/// cross-assembly is out of reach without project context, so this is
/// strictly less powerful than `project_references` and never used when
/// the project path succeeded.
fn single_file_references(
    state: &mut State,
    uri: &Url,
    text: &str,
    byte: usize,
    include_declaration: bool,
) -> Vec<Location> {
    let symbols = state.symbols_for_uri(uri);
    let lang = state.lang_version_for_uri(uri);
    let Some(parse) = parse_with_symbols(text, &symbols, lang) else {
        return Vec::new();
    };
    let Some(file) = ImplFile::cast(parse.root) else {
        return Vec::new();
    };
    let resolved = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());
    let Some((target_range, target_res)) = smallest_resolution_with_range(&resolved, byte) else {
        return Vec::new();
    };
    if matches!(target_res, Resolution::Deferred(_) | Resolution::Unresolved) {
        return Vec::new();
    }
    let ambiguous_labels = ambiguous_argument_label_ranges(file.syntax());
    if ambiguous_labels.contains(&target_range) {
        return Vec::new();
    }
    // The declaration anchor in the single-file case: a `Local` has its
    // binder in this file; an `Item` defined in this same file is also
    // resolvable here (the binder lives in `resolved`'s own arena). Any
    // resolution kind that has no in-file def returns `None` from
    // `resolved_def`, so include_declaration is a no-op for it.
    let decl_range = resolved.resolved_def(target_res).map(|d| d.range);

    let mut out = Vec::new();
    // Attribute-type occurrences live in their own map (EX-3 §2(d)) but are
    // ordinary references of the target type — chain them in, exactly as the
    // project path's `matching_in_file` does.
    for (range, res) in resolved
        .resolutions()
        .iter()
        .chain(resolved.attribute_resolutions().iter())
    {
        if *res != target_res {
            continue;
        }
        if ambiguous_labels.contains(range) {
            continue;
        }
        if !include_declaration && decl_range == Some(*range) {
            continue;
        }
        out.push(Location {
            uri: uri.clone(),
            range: range_to_lsp(text, *range),
        });
    }
    out
}

/// Ranges which might be named-argument labels in an application.
///
/// `f(name = value)` and `f (name = value)` have the same expression shape
/// whether `f` is a method accepting a named argument or an ordinary function
/// accepting a Boolean equality. Find-references cannot prove which reading is
/// correct without resolving the callee's type, so it makes no claim for the
/// possible label range. The ordinary resolver remains untouched: definition,
/// hover, and non-reference consumers retain both equality operands.
fn ambiguous_argument_label_ranges(root: &SyntaxNode) -> HashSet<TextRange> {
    let mut ranges = HashSet::new();
    for expression in root.descendants().filter_map(Expr::cast) {
        let Expr::App(application) = expression else {
            continue;
        };
        if application.is_infix() {
            continue;
        }
        if let Some(argument) = application.arg() {
            collect_ambiguous_argument_labels(&argument, &mut ranges);
        }
    }
    ranges
}

/// Inspect only the direct elements of one application argument list. An
/// equality nested inside a record, lambda, or other expression is an ordinary
/// sub-expression rather than a possible label.
fn collect_ambiguous_argument_labels(argument: &Expr, ranges: &mut HashSet<TextRange>) {
    let Some(inner) = (match argument {
        Expr::Paren(paren) => paren.inner(),
        other => Some(other.clone()),
    }) else {
        return;
    };

    if let Expr::Tuple(tuple) = &inner
        && !tuple.is_struct()
    {
        for element in tuple.elements() {
            if let Some(range) = ambiguous_argument_label(&element) {
                ranges.insert(range);
            }
        }
    } else if let Some(range) = ambiguous_argument_label(&inner) {
        ranges.insert(range);
    }
}

/// The possible label range of a direct argument element shaped
/// `App[InfixApp[label, "="], value]`.
fn ambiguous_argument_label(element: &Expr) -> Option<TextRange> {
    let Expr::App(outer) = element else {
        return None;
    };
    if outer.is_infix() {
        return None;
    }
    let Expr::App(equals) = outer.func()? else {
        return None;
    };
    if !equals.is_infix()
        || !equals
            .func()
            .is_some_and(|operator| operator.syntax().text().to_string().trim() == "=")
    {
        return None;
    }
    equals.arg().map(|label| label.syntax().text_range())
}

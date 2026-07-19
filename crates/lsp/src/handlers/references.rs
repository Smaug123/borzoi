//! `textDocument/references` — every use of the cursor's symbol, project-wide.
//!
//! Resolves the cursor to a [`Resolution`] (using the same containment +
//! prefer-non-`Deferred` rules as goto-definition), then iterates every file
//! in the [`ResolvedProject`] and collects ranges whose recorded resolution
//! is exactly that one ([`Resolution`] is `Copy + PartialEq`). A range in the
//! label position of a call argument shaped `name = value` or `?name = value`
//! is omitted when `name` is a bare identifier. The former could also be an
//! ordinary Boolean equality; the latter is unambiguously a label, but its
//! parameter cannot be identified without the callee's type. For
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

use borzoi_cst::syntax::{AppExpr, AstNode, Expr, ImplFile, SyntaxKind, SyntaxNode};
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
    let span = tracing::info_span!(
        "find_references",
        include_declaration = params.context.include_declaration,
        mode = tracing::field::Empty,
        target_kind = tracing::field::Empty,
        project_files = tracing::field::Empty,
        files_scanned = tracing::field::Empty,
        resolutions_scanned = tracing::field::Empty,
        result_count = tracing::field::Empty,
    );
    let _guard = span.enter();
    let pos = params.text_document_position.position;
    let uri = params.text_document_position.text_document.uri.clone();
    let include_declaration = params.context.include_declaration;
    let Some(text) = state.docs.get(&uri).cloned() else {
        span.record("mode", "missing_buffer");
        span.record("target_kind", "none");
        span.record("project_files", 0);
        span.record("files_scanned", 0);
        span.record("resolutions_scanned", 0);
        span.record("result_count", 0);
        return None;
    };
    let byte = position_to_offset(&text, pos);

    let answer = match project_references(state, &uri, byte, include_declaration) {
        Some(answer) => answer,
        // Fallback: orphan / partial-project buffer — single-file references
        // only. `Local` is the only resolution kind that survives without a
        // project context anyway.
        None => single_file_references(state, &uri, &text, byte, include_declaration),
    };
    span.record("mode", answer.mode);
    span.record("target_kind", answer.target_kind);
    span.record("project_files", answer.project_files);
    span.record("files_scanned", answer.scan.files_scanned);
    span.record("resolutions_scanned", answer.scan.resolutions_scanned);
    span.record("result_count", answer.scan.locations.len());
    Some(answer.scan.locations)
}

struct ReferencesAnswer {
    mode: &'static str,
    target_kind: &'static str,
    project_files: usize,
    scan: ReferenceScan,
}

struct ReferenceScan {
    locations: Vec<Location>,
    files_scanned: usize,
    resolutions_scanned: usize,
}

impl ReferenceScan {
    fn empty() -> Self {
        Self {
            locations: Vec::new(),
            files_scanned: 0,
            resolutions_scanned: 0,
        }
    }
}

fn resolution_kind(resolution: Resolution) -> &'static str {
    match resolution {
        Resolution::Local(_) => "local",
        Resolution::Item(_) => "item",
        Resolution::Entity(_) => "entity",
        Resolution::Member { .. } => "member",
        Resolution::Deferred(_) => "deferred",
        Resolution::Unresolved => "unresolved",
    }
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
) -> Option<ReferencesAnswer> {
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
        return Some(ReferencesAnswer {
            mode: "project",
            target_kind: resolution_kind(target_res),
            project_files: resolved.len(),
            scan: ReferenceScan::empty(),
        });
    }
    if ambiguous_argument_label_ranges(parses.files[target_file_idx].file.syntax())
        .contains(&target_range)
    {
        return Some(ReferencesAnswer {
            mode: "project",
            target_kind: resolution_kind(target_res),
            project_files: resolved.len(),
            scan: ReferenceScan::empty(),
        });
    }
    let decl_anchor = declaration_anchor(&resolved, target_res, target_file_idx);
    let project_files = resolved.len();
    Some(ReferencesAnswer {
        mode: "project",
        target_kind: resolution_kind(target_res),
        project_files,
        scan: collect_locations(
            &parses,
            &resolved,
            uri,
            target_res,
            decl_anchor,
            include_declaration,
            docs,
        ),
    })
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
) -> ReferenceScan {
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

    let files_scanned = file_range.len();
    let span = tracing::info_span!(
        "find_references.scan",
        files_scanned,
        resolutions_scanned = tracing::field::Empty,
        result_count = tracing::field::Empty,
    );
    let _guard = span.enter();
    let mut out = Vec::new();
    let mut resolutions_scanned = 0;
    let mut decl_emitted = false;
    for file_idx in file_range {
        let file = resolved.file(file_idx);
        resolutions_scanned += file.resolutions().len() + file.attribute_resolutions().len();
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
    span.record("resolutions_scanned", resolutions_scanned);
    span.record("result_count", out.len());
    ReferenceScan {
        locations: out,
        files_scanned,
        resolutions_scanned,
    }
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
) -> ReferencesAnswer {
    let span = tracing::info_span!("find_references.single_file");
    let _guard = span.enter();
    let empty = |target_kind| ReferencesAnswer {
        mode: "single_file",
        target_kind,
        project_files: 1,
        scan: ReferenceScan::empty(),
    };
    let symbols = state.symbols_for_uri(uri);
    let lang = state.lang_version_for_uri(uri);
    let Some(parse) = parse_with_symbols(text, &symbols, lang) else {
        return empty("parse_error");
    };
    let Some(file) = ImplFile::cast(parse.root) else {
        return empty("unsupported_file");
    };
    let resolved = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());
    let Some((target_range, target_res)) = smallest_resolution_with_range(&resolved, byte) else {
        return empty("none");
    };
    if matches!(target_res, Resolution::Deferred(_) | Resolution::Unresolved) {
        return empty(resolution_kind(target_res));
    }
    let ambiguous_labels = ambiguous_argument_label_ranges(file.syntax());
    if ambiguous_labels.contains(&target_range) {
        return empty(resolution_kind(target_res));
    }
    // The declaration anchor in the single-file case: a `Local` has its
    // binder in this file; an `Item` defined in this same file is also
    // resolvable here (the binder lives in `resolved`'s own arena). Any
    // resolution kind that has no in-file def returns `None` from
    // `resolved_def`, so include_declaration is a no-op for it.
    let decl_range = resolved.resolved_def(target_res).map(|d| d.range);

    let files_scanned = 1;
    let resolutions_scanned = resolved.resolutions().len() + resolved.attribute_resolutions().len();
    let scan_span = tracing::info_span!(
        "find_references.scan",
        files_scanned,
        resolutions_scanned,
        result_count = tracing::field::Empty,
    );
    let _scan_guard = scan_span.enter();
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
    scan_span.record("result_count", out.len());
    ReferencesAnswer {
        mode: "single_file",
        target_kind: resolution_kind(target_res),
        project_files: 1,
        scan: ReferenceScan {
            locations: out,
            files_scanned,
            resolutions_scanned,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_trace::capture;
    use lsp_types::{
        PartialResultParams, Position, ReferenceContext, TextDocumentIdentifier,
        TextDocumentPositionParams, WorkDoneProgressParams,
    };
    use std::fs;
    use tempfile::TempDir;

    fn params(uri: Url, include_declaration: bool) -> ReferenceParams {
        ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 0,
                    character: 4,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: ReferenceContext {
                include_declaration,
            },
        }
    }

    #[test]
    fn telemetry_describes_the_single_file_scan_and_answer() {
        let uri = Url::parse("inmemory:///Sample.fs").unwrap();
        let mut state = State::default();
        state
            .docs
            .insert(uri.clone(), "let x = 1\nlet y = x + x\n".to_string());

        let (locations, trace) = capture(|| handle(&mut state, params(uri, true)));
        assert_eq!(locations.unwrap().len(), 3);

        let request = trace.only_span("find_references");
        assert_eq!(request.field("mode"), Some("single_file"));
        assert_eq!(request.field("target_kind"), Some("item"));
        assert_eq!(request.field("project_files"), Some("1"));
        assert_eq!(request.field("files_scanned"), Some("1"));
        assert_eq!(request.field("result_count"), Some("3"));

        let scan = trace.only_span("find_references.scan");
        assert_eq!(scan.field("files_scanned"), Some("1"));
        assert_eq!(scan.field("result_count"), Some("3"));
        assert!(
            scan.field("resolutions_scanned")
                .unwrap()
                .parse::<usize>()
                .unwrap()
                >= 3
        );
    }

    #[test]
    fn telemetry_identifies_a_fully_cached_project_lookup() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("P.fsproj");
        let a = tmp.path().join("A.fs");
        let b = tmp.path().join("B.fs");
        fs::write(
            &project,
            r#"<Project>
              <ItemGroup>
                <Compile Include="A.fs" />
                <Compile Include="B.fs" />
              </ItemGroup>
            </Project>"#,
        )
        .unwrap();
        let a_text = "module Shared\nlet foo = 1\n";
        let b_text = "module Other\nlet x = Shared.foo\nlet y = Shared.foo\n";
        fs::write(&a, a_text).unwrap();
        fs::write(&b, b_text).unwrap();
        let a_uri = Url::from_file_path(a).unwrap();
        let b_uri = Url::from_file_path(b).unwrap();
        let mut state = State::default();
        state.docs.insert(a_uri.clone(), a_text.to_string());
        state.docs.insert(b_uri, b_text.to_string());
        let reference_params = || {
            let mut params = params(a_uri.clone(), true);
            params.text_document_position.position = Position {
                line: 1,
                character: 4,
            };
            params
        };

        assert_eq!(handle(&mut state, reference_params()).unwrap().len(), 3);
        let (locations, trace) = capture(|| handle(&mut state, reference_params()));
        assert_eq!(locations.unwrap().len(), 3);

        let request = trace.only_span("find_references");
        assert_eq!(request.field("mode"), Some("project"));
        assert_eq!(request.field("target_kind"), Some("item"));
        assert_eq!(request.field("project_files"), Some("2"));
        assert_eq!(request.field("files_scanned"), Some("2"));
        assert_eq!(request.field("result_count"), Some("3"));
        let lookup = trace.only_span("semantic.project_lookup");
        assert_eq!(lookup.field("scope"), Some("full"));
        assert_eq!(lookup.field("cache_hit"), Some("true"));
        assert_eq!(lookup.field("requested_files"), Some("2"));
        assert_eq!(lookup.field("cached_files"), Some("2"));
        assert!(trace.spans_named("resolve_project").is_empty());
    }
}

/// Ranges which might be named-argument labels in a call.
///
/// `f(name = value)` and `f (name = value)` have the same argument expression
/// shape whether `f` is a method accepting a named argument or an ordinary
/// function accepting a Boolean equality. An optional `?name` is certainly a
/// label, but the callee's type is still needed to identify its parameter.
/// Explicit construction and object expressions carry the same argument shape
/// directly on their `New` / `ObjExpr` nodes. Find-references makes no claim for
/// these label ranges. The ordinary resolver remains untouched: definition,
/// hover, and non-reference consumers retain both equality operands.
fn ambiguous_argument_label_ranges(root: &SyntaxNode) -> HashSet<TextRange> {
    let mut ranges = HashSet::new();
    for expression in root.descendants().filter_map(Expr::cast) {
        let argument = match expression {
            Expr::App(application) => {
                if application.is_infix() {
                    continue;
                }
                if is_query_join_on_wrapper(&application) {
                    continue;
                }
                // FCS lowers every completed infix expression to a non-infix outer
                // `App` whose function is the inner infix `App`. Its argument is the
                // operator's RHS, not a call argument list.
                if matches!(application.func(), Some(Expr::App(inner)) if inner.is_infix()) {
                    continue;
                }
                application.arg()
            }
            Expr::New(construction) => construction.arg(),
            Expr::ObjExpr(object_expression) => object_expression.arg(),
            _ => continue,
        };
        if let Some(argument) = argument {
            collect_ambiguous_argument_labels(&argument, &mut ranges);
        }
    }
    ranges
}

/// Whether this is the synthetic `xs on (a = b)` application on a query
/// `JoinIn` RHS. Its left-nested shape is `App(App(xs, on), Paren(equality))`;
/// an ordinary call used directly as either operand does not carry the bare
/// `on` argument and remains eligible for label filtering.
fn is_query_join_on_wrapper(application: &AppExpr) -> bool {
    let Some(Expr::JoinIn(join)) = application.syntax().parent().and_then(Expr::cast) else {
        return false;
    };
    if !join
        .rhs()
        .is_some_and(|rhs| rhs.syntax() == application.syntax())
    {
        return false;
    }
    let Some(Expr::App(prefix)) = application.func() else {
        return false;
    };
    let Some(Expr::Ident(on)) = prefix.arg() else {
        return false;
    };
    on.ident().is_some_and(|token| token.text() == "on")
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
        || !equals.func().is_some_and(|operator| {
            let mut tokens = operator
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| !token.kind().is_trivia());
            matches!(
                tokens.next(),
                Some(token) if token.kind() == SyntaxKind::IDENT_TOK && token.text() == "="
            ) && tokens.next().is_none()
        })
    {
        return None;
    }
    let label = equals.arg()?;
    match label {
        Expr::Ident(_) => Some(label.syntax().text_range()),
        Expr::LongIdent(optional)
            if optional
                .syntax()
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| token.kind() == SyntaxKind::QMARK_TOK) =>
        {
            let path = optional.long_ident()?;
            let mut idents = path.idents();
            let ident = idents.next()?;
            idents.next().is_none().then(|| ident.text_range())
        }
        _ => None,
    }
}

//! A cursor on the **qualifier** of a dotted path, across every handler that
//! picks a resolution by cursor position.
//!
//! In `Alpha.Beta.v` the resolver records the leaf's answer (`Item(v)`) over the
//! whole path and a deferral over each qualifier segment. A cursor on `Alpha` or
//! `Beta` is on a module, which FCS reports as such; it is *not* on `v`. Picking
//! the enclosing whole-path answer there would send go-to-definition to `v`,
//! render `v`'s hover, and list `v`'s references, all for a cursor on a module.
//!
//! The whole-project differential (`borzoi-corpus-diff`) grades the served
//! answer against FCS at each oracle record. This group states the same thing
//! as absolute properties of each handler, over a generated set of path
//! shapes, at **every** cursor byte inside every qualifier token rather than one
//! probe per record — so a handler that layers its own selection on top of the
//! shared one is held to it too:
//!
//! - go-to-definition on a qualifier never lands on the leaf's binder;
//! - hover on a qualifier never describes the leaf;
//! - find-references on a qualifier returns no occurrence of the leaf;
//! - and, so none of that passes by answering nothing anywhere, go-to-definition
//!   on the leaf still lands on its binder.

use std::fs;
use std::path::Path;

use borzoi::handlers::definition::{self, DefinitionOutcome};
use borzoi::handlers::{hover, references};
use borzoi::position::{offset_to_position, position_to_offset};
use borzoi::server::State;
use lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams, Location,
    PartialResultParams, ReferenceContext, ReferenceParams, TextDocumentIdentifier,
    TextDocumentPositionParams, Url, WorkDoneProgressParams,
};
use tempfile::TempDir;

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn position_params(uri: &Url, src: &str, byte: usize) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: offset_to_position(src, byte),
    }
}

fn definition_at(state: &mut State, uri: &Url, src: &str, byte: usize) -> Option<Location> {
    let params = GotoDefinitionParams {
        text_document_position_params: position_params(uri, src, byte),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    match definition::handle(state, params) {
        DefinitionOutcome::Ready(Some(GotoDefinitionResponse::Scalar(loc))) => Some(loc),
        DefinitionOutcome::Ready(None) => None,
        other => panic!(
            "unexpected definition response shape: {:?}",
            matches_shape(&other)
        ),
    }
}

fn matches_shape(outcome: &DefinitionOutcome) -> &'static str {
    match outcome {
        DefinitionOutcome::Ready(Some(GotoDefinitionResponse::Scalar(_))) => "scalar",
        DefinitionOutcome::Ready(Some(_)) => "non-scalar",
        DefinitionOutcome::Ready(None) => "none",
        DefinitionOutcome::Deferred(_) => "deferred fetch",
    }
}

fn hover_at(state: &mut State, uri: &Url, src: &str, byte: usize) -> Option<String> {
    let params = HoverParams {
        text_document_position_params: position_params(uri, src, byte),
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    hover::handle(state, params).map(|h| match h.contents {
        HoverContents::Markup(m) => m.value,
        other => panic!("expected markup hover, got {other:?}"),
    })
}

fn references_at(state: &mut State, uri: &Url, src: &str, byte: usize) -> Vec<Location> {
    let params = ReferenceParams {
        text_document_position: position_params(uri, src, byte),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration: true,
        },
    };
    references::handle(state, params).unwrap_or_default()
}

/// The declarations every cell's innermost module holds.
const DECLS: &[&str] = &["let v = 1", "let f x = x + 1", "type Color = Red | Green"];

/// `(shape, binding written against the path P, the leaf's binder name)`.
const LEAVES: &[(&str, &str, &str)] = &[
    ("value", "let u = P.v", "v"),
    ("apply", "let u = P.f 1", "f"),
    ("case", "let u = P.Color.Red", "Red"),
    (
        "pattern",
        "let u c = match c with | P.Color.Red -> 1 | _ -> 0",
        "Red",
    ),
];

const MODULES: &[&str] = &["Alpha", "Beta", "Gamma"];

fn nested_modules(names: &[&str]) -> String {
    let mut out = String::new();
    for (level, name) in names.iter().enumerate() {
        out.push_str(&" ".repeat(4 * level));
        out.push_str(&format!("module {name} =\n"));
    }
    for decl in DECLS {
        out.push_str(&" ".repeat(4 * names.len()));
        out.push_str(decl);
        out.push('\n');
    }
    out
}

/// One generated project: `Lib.fs` then `Use.fs`, the use in the last line of
/// `Use.fs`.
struct Cell {
    shape: &'static str,
    label: String,
    lib: String,
    using: String,
    /// The leaf's binder name, declared once in whichever file holds it.
    leaf: &'static str,
}

/// Depth × root × placement × leaf shape, the axes the whole-project sweep
/// crosses, minus the type annotation (no project type is answered today, so it
/// has no leaf to send anyone to).
fn cells() -> Vec<Cell> {
    let mut out = Vec::new();
    for depth in 1..=MODULES.len() {
        let modules = &MODULES[..depth];
        for namespaced in [false, true] {
            for same_file in [false, true] {
                for (shape, template, leaf) in LEAVES {
                    let path = if namespaced && !same_file {
                        format!("Ns.{}", modules.join("."))
                    } else {
                        modules.join(".")
                    };
                    let binding = template.replace("P.", &format!("{path}."));
                    let (lib, using) = match (namespaced, same_file) {
                        (false, false) => (
                            format!("module {}\n\n{}", modules[0], nested_modules(&modules[1..])),
                            format!("module Use\n\n{binding}\n"),
                        ),
                        (true, false) => (
                            format!("namespace Ns\n\n{}", nested_modules(modules)),
                            format!("module Use\n\n{binding}\n"),
                        ),
                        (false, true) => (
                            "module Lib\n\nlet unrelated = 0\n".to_string(),
                            format!("module Use\n\n{}\n{binding}\n", nested_modules(modules)),
                        ),
                        (true, true) => (
                            "module Lib\n\nlet unrelated = 0\n".to_string(),
                            format!(
                                "namespace Ns\n\n{}\nmodule Use =\n    {binding}\n",
                                nested_modules(modules)
                            ),
                        ),
                    };
                    out.push(Cell {
                        shape,
                        label: format!(
                            "depth{depth}-{}-{}-{shape}",
                            if namespaced { "namespace" } else { "module" },
                            if same_file { "same-file" } else { "cross-file" }
                        ),
                        lib,
                        using,
                        leaf,
                    });
                }
            }
        }
    }
    out
}

/// The byte ranges of the qualifier tokens in the last line of `src`: each
/// identifier immediately followed by `.`.
fn qualifier_tokens(src: &str) -> Vec<(usize, usize)> {
    let line_start = src.trim_end().rfind('\n').map_or(0, |i| i + 1);
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = line_start;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            if bytes.get(i) == Some(&b'.') {
                out.push((start, i));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The byte range of the leaf: the identifier right after the last qualifier.
fn leaf_token(src: &str, qualifiers: &[(usize, usize)]) -> (usize, usize) {
    let start = qualifiers.last().expect("a path has a qualifier").1 + 1;
    let len = src[start..]
        .find(|c: char| !c.is_ascii_alphanumeric())
        .expect("the leaf is followed by something");
    (start, start + len)
}

/// Where `leaf` is declared: the identifier after `let ` / ` ` in whichever file
/// declares it, as `(uri, byte range)`.
fn leaf_binder(cell: &Cell, lib: &Url, using: &Url) -> (Url, (usize, usize)) {
    let decl = |src: &str| {
        [format!("let {} ", cell.leaf), format!("= {} ", cell.leaf)]
            .iter()
            .find_map(|needle| {
                src.find(needle.as_str())
                    .map(|i| i + needle.len() - cell.leaf.len() - 1)
            })
            .map(|start| (start, start + cell.leaf.len()))
    };
    if let Some(range) = decl(&cell.lib) {
        (lib.clone(), range)
    } else {
        (
            using.clone(),
            decl(&cell.using).expect("the leaf is declared in one of the two files"),
        )
    }
}

fn location_range(src: &str, loc: &Location) -> (usize, usize) {
    (
        position_to_offset(src, loc.range.start),
        position_to_offset(src, loc.range.end),
    )
}

#[test]
fn a_qualifier_cursor_is_never_served_the_leaf() {
    let cells = cells();
    let mut qualifier_cursors = 0usize;
    let mut navigated = std::collections::BTreeSet::new();
    let mut undefined = Vec::new();
    for cell in &cells {
        let tmp = TempDir::new().unwrap();
        let lib_path = tmp.path().join("Lib.fs");
        let use_path = tmp.path().join("Use.fs");
        write(
            &tmp.path().join("P.fsproj"),
            r#"<Project>
  <ItemGroup>
    <Compile Include="Lib.fs" />
    <Compile Include="Use.fs" />
  </ItemGroup>
</Project>"#,
        );
        write(&lib_path, &cell.lib);
        write(&use_path, &cell.using);
        let lib_uri = Url::from_file_path(&lib_path).unwrap();
        let use_uri = Url::from_file_path(&use_path).unwrap();
        let mut state = State::default();
        state.docs.insert(lib_uri.clone(), cell.lib.clone());
        state.docs.insert(use_uri.clone(), cell.using.clone());

        let (binder_uri, binder) = leaf_binder(cell, &lib_uri, &use_uri);
        let binder_src = if binder_uri == lib_uri {
            &cell.lib
        } else {
            &cell.using
        };
        let is_leaf_binder =
            |loc: &Location| loc.uri == binder_uri && location_range(binder_src, loc) == binder;

        let qualifiers = qualifier_tokens(&cell.using);
        assert!(!qualifiers.is_empty(), "cell {}: no qualifier", cell.label);

        // Availability: the leaf itself still navigates, or every assertion
        // below could pass by the handlers answering nothing at all. Not every
        // shape resolves in every placement today, so this is a floor per
        // shape (checked after the loop) rather than per cell; a leaf that does
        // navigate must go to its own binder.
        let (leaf_start, _) = leaf_token(&cell.using, &qualifiers);
        match definition_at(&mut state, &use_uri, &cell.using, leaf_start + 1) {
            Some(loc) => {
                assert!(
                    is_leaf_binder(&loc),
                    "cell {}: the leaf {:?} navigated somewhere other than its binder: \
                     {loc:?}",
                    cell.label,
                    cell.leaf
                );
                navigated.insert(cell.shape);
            }
            None => undefined.push(cell.label.as_str()),
        }

        for &(start, end) in &qualifiers {
            let token = &cell.using[start..end];
            for byte in start..end {
                qualifier_cursors += 1;
                let def = definition_at(&mut state, &use_uri, &cell.using, byte);
                assert!(
                    !def.as_ref().is_some_and(is_leaf_binder),
                    "cell {}: definition on qualifier {token:?} (byte {byte}) went to the \
                     leaf {:?}\n{}",
                    cell.label,
                    cell.leaf,
                    cell.using
                );
                if let Some(body) = hover_at(&mut state, &use_uri, &cell.using, byte) {
                    assert!(
                        !body.contains(&format!("`{} ", cell.leaf))
                            && !body.contains(&format!("`{}`", cell.leaf)),
                        "cell {}: hover on qualifier {token:?} (byte {byte}) described the \
                         leaf: {body}",
                        cell.label
                    );
                }
                for loc in references_at(&mut state, &use_uri, &cell.using, byte) {
                    let src = if loc.uri == lib_uri {
                        &cell.lib
                    } else {
                        &cell.using
                    };
                    let (s, e) = location_range(src, &loc);
                    assert_eq!(
                        &src[s..e],
                        token,
                        "cell {}: references on qualifier {token:?} (byte {byte}) returned \
                         an occurrence of something else",
                        cell.label
                    );
                }
            }
        }
    }
    for (shape, _, _) in LEAVES {
        assert!(
            navigated.contains(shape),
            "no {shape} cell navigated its leaf, so nothing shows the qualifier \
             assertions are not passing by answering nothing; navigated {navigated:?}"
        );
    }
    eprintln!(
        "qualifier cursors: {} cells, {qualifier_cursors} cursor positions; leaves that \
         defer today: {undefined:?}",
        cells.len()
    );
}

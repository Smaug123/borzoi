//! FCS differential for `textDocument/references`.
//!
//! The handler is deliberately incomplete: a [`borzoi_sema::Resolution::Deferred`]
//! occurrence is omitted. Its soundness promise is the other direction — every
//! location it *does* return names the cursor's symbol. This test asks FCS for
//! every symbol use in a small project, queries the handler at each source-side
//! declaration it can answer, and asserts:
//!
//! ```text
//! handler locations ⊆ FCS uses of the cursor symbol
//! ```
//!
//! The currency is `(display name, declaration file, declaration byte range)`.
//! A source declaration gives FCS and the handler a common stable identity
//! without comparing sema's private `DefId` / project-local `ItemId` handles.

use std::fs;
use std::path::{Path, PathBuf};

use borzoi::handlers::references;
use borzoi::position::{offset_to_position, position_to_offset};
use borzoi::server::State;
use lsp_types::{
    PartialResultParams, ReferenceContext, ReferenceParams, TextDocumentIdentifier,
    TextDocumentPositionParams, Url, WorkDoneProgressParams,
};
use tempfile::TempDir;

use crate::common::{
    DeclSite, FileUses, NormalisedProjectUse, invoke_fcs_dump_project_with_refs,
    parse_fcs_uses_project,
};

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SymbolKey {
    name: String,
    decl: DeclSite,
}

#[derive(Debug, Clone)]
struct Target {
    key: SymbolKey,
    cursor_file: PathBuf,
    cursor_start: usize,
}

#[derive(Debug, Default)]
struct Coverage {
    queried_targets: usize,
    answered_targets: usize,
    locations: usize,
    same_file_locations: usize,
    cross_file_locations: usize,
    defining_locations: usize,
    use_locations: usize,
}

fn params(uri: &Url, source: &str, byte: usize, include_declaration: bool) -> ReferenceParams {
    ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: offset_to_position(source, byte),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration,
        },
    }
}

fn source_for<'a>(sources: &'a [(PathBuf, String)], path: &Path) -> &'a str {
    sources
        .iter()
        .find(|(candidate, _)| candidate == path)
        .map(|(_, source)| source.as_str())
        .unwrap_or_else(|| panic!("no source text for {}", path.display()))
}

fn uses_for<'a>(fcs: &'a [FileUses], path: &Path) -> &'a FileUses {
    fcs.iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("FCS reported no uses for {}", path.display()))
}

/// Every distinct, ordinary source symbol FCS exposes through a defining
/// occurrence. Requiring the defining use and its declaration location to be
/// the same range excludes implicit/synthetic symbols whose source identity is
/// not a cursor position the LSP can query.
fn source_targets(fcs: &[FileUses]) -> Vec<Target> {
    let mut targets = Vec::new();
    for file in fcs {
        for symbol_use in &file.uses {
            let Some(decl) = &symbol_use.decl else {
                continue;
            };
            if !symbol_use.is_from_definition
                || symbol_use.start == symbol_use.end
                || decl.file != file.path
                || decl.start != symbol_use.start
                || decl.end != symbol_use.end
            {
                continue;
            }
            let key = SymbolKey {
                name: symbol_use.name.clone(),
                decl: decl.clone(),
            };
            if targets.iter().any(|target: &Target| target.key == key) {
                continue;
            }
            targets.push(Target {
                key,
                cursor_file: file.path.clone(),
                cursor_start: symbol_use.start,
            });
        }
    }
    targets
}

fn matching_fcs_use<'a>(
    fcs: &'a [FileUses],
    path: &Path,
    start: usize,
    end: usize,
    target: &SymbolKey,
) -> Option<&'a NormalisedProjectUse> {
    uses_for(fcs, path).uses.iter().find(|symbol_use| {
        symbol_use.start == start
            && symbol_use.end == end
            && symbol_use.name == target.name
            && symbol_use.decl.as_ref() == Some(&target.decl)
    })
}

fn check_answer(
    state: &mut State,
    sources: &[(PathBuf, String)],
    fcs: &[FileUses],
    target: &Target,
    include_declaration: bool,
    coverage: &mut Coverage,
) -> usize {
    let cursor_source = source_for(sources, &target.cursor_file);
    let cursor_uri = Url::from_file_path(&target.cursor_file).unwrap();
    let locations = references::handle(
        state,
        params(
            &cursor_uri,
            cursor_source,
            target.cursor_start,
            include_declaration,
        ),
    )
    .expect("the queried buffer is open");

    for location in &locations {
        let path = location
            .uri
            .to_file_path()
            .unwrap_or_else(|()| panic!("references returned a non-file URI: {}", location.uri));
        let source = source_for(sources, &path);
        let start = position_to_offset(source, location.range.start);
        let end = position_to_offset(source, location.range.end);
        let Some(fcs_use) = matching_fcs_use(fcs, &path, start, end, &target.key) else {
            let occupants: Vec<_> = uses_for(fcs, &path)
                .uses
                .iter()
                .filter(|symbol_use| symbol_use.start == start && symbol_use.end == end)
                .collect();
            panic!(
                "handler returned {}:{}..{} for {:?}, but FCS has no use of that symbol there; FCS occupants: {occupants:#?}",
                path.display(),
                start,
                end,
                target.key,
            );
        };
        if !include_declaration {
            assert!(
                !fcs_use.is_from_definition,
                "includeDeclaration=false returned FCS's defining occurrence for {:?}",
                target.key,
            );
        }

        coverage.locations += 1;
        if path == target.cursor_file {
            coverage.same_file_locations += 1;
        } else {
            coverage.cross_file_locations += 1;
        }
        if fcs_use.is_from_definition {
            coverage.defining_locations += 1;
        } else {
            coverage.use_locations += 1;
        }
    }
    locations.len()
}

#[test]
fn every_reported_reference_is_the_cursor_symbol_according_to_fcs() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("References.fsproj");
    let library = tmp.path().join("Library.fs");
    let client = tmp.path().join("Client.fs");
    let library_source = r#"module Library

let alpha = 1
let shadow = 10
let add x = x + alpha
let pair a b = a, b

type Color =
    | Red
    | Blue
"#;
    let client_source = r#"module Client

open Library

let shadow = 20
let alphaDirect = Library.alpha
let alphaOpened = alpha
let addResult = Library.add alpha
let pairResult = Library.pair shadow alpha

let local z =
    let shadow = z
    shadow + alpha

let classify c =
    match c with
    | Red -> alpha
    | Blue -> shadow
"#;
    write(
        &project,
        r#"<Project>
  <ItemGroup>
    <Compile Include="Library.fs" />
    <Compile Include="Client.fs" />
  </ItemGroup>
</Project>"#,
    );
    write(&library, library_source);
    write(&client, client_source);

    let sources = vec![
        (library.clone(), library_source.to_string()),
        (client.clone(), client_source.to_string()),
    ];
    let paths: Vec<&Path> = sources.iter().map(|(path, _)| path.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, &[], &[], None);
    let fcs = parse_fcs_uses_project(&json, &sources);
    let targets = source_targets(&fcs);

    let mut state = State::default();
    for (path, source) in &sources {
        state
            .docs
            .insert(Url::from_file_path(path).unwrap(), source.clone());
    }

    let mut coverage = Coverage {
        queried_targets: targets.len(),
        ..Coverage::default()
    };
    for target in &targets {
        let with_declaration =
            check_answer(&mut state, &sources, &fcs, target, true, &mut coverage);
        if with_declaration == 0 {
            continue;
        }
        coverage.answered_targets += 1;
        let without_declaration =
            check_answer(&mut state, &sources, &fcs, target, false, &mut coverage);
        assert!(
            without_declaration < with_declaration,
            "including the declaration added nothing for {:?}",
            target.key,
        );
    }

    // Distribution assertions are part of the property: an all-Deferred
    // resolver, a project scan accidentally restricted to one file, or a
    // declaration-only result must not make the subset check pass vacuously.
    assert!(coverage.queried_targets >= 12, "{coverage:#?}");
    assert!(coverage.answered_targets >= 10, "{coverage:#?}");
    assert!(coverage.locations >= 30, "{coverage:#?}");
    assert!(coverage.same_file_locations >= 20, "{coverage:#?}");
    assert!(coverage.cross_file_locations >= 6, "{coverage:#?}");
    assert!(coverage.defining_locations >= 10, "{coverage:#?}");
    assert!(coverage.use_locations >= 15, "{coverage:#?}");
}

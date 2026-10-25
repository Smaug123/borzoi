//! FCS differential for `textDocument/references`.
//!
//! The handler is deliberately incomplete: a [`borzoi_sema::Resolution::Deferred`]
//! occurrence is omitted. Its soundness promise is the other direction — every
//! location it *does* return names the cursor's symbol. This test asks FCS for
//! every symbol use in a small project, queries the handler at each source-side
//! declaration **and at each use** FCS reports, and asserts:
//!
//! ```text
//! handler locations ⊆ FCS uses of the cursor symbol          (soundness)
//! FCS uses − handler locations = EXPECTED_DECLINED           (completeness, pinned)
//! a use site's answer, when non-empty, = its definition's    (cursor independence)
//! ```
//!
//! The declined set is pinned exactly rather than bounded, so a reference the
//! handler stops returning fails the test as surely as a wrong one does.
//!
//! The currency is `(display name, declaration file, declaration byte range)`.
//! A source declaration gives FCS and the handler a common stable identity
//! without comparing sema's private `DefId` / project-local `ItemId` handles.

use std::collections::BTreeSet;
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
    DeclSite, FileUses, NormalisedProjectUse, OracleRefScope, invoke_fcs_dump_project_with_refs,
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
) -> BTreeSet<Span> {
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

    let mut answered = BTreeSet::new();
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
        assert!(
            answered.insert((path, start, end)),
            "references returned one location twice for {:?}",
            target.key
        );
    }
    answered
}

/// A location: file and byte range.
type Span = (PathBuf, usize, usize);

/// Every use FCS reports of `key`'s symbol, its defining occurrence included.
fn fcs_uses_of(fcs: &[FileUses], key: &SymbolKey) -> BTreeSet<Span> {
    fcs.iter()
        .flat_map(|file| {
            file.uses
                .iter()
                .filter(|u| u.name == key.name && u.decl.as_ref() == Some(&key.decl))
                .map(|u| (file.path.clone(), u.start, u.end))
        })
        .collect()
}

/// `span` as `<file name>:<line>:<col>` (both 1-based), for a pinned list.
fn render_span(sources: &[(PathBuf, String)], (path, start, _): &Span) -> String {
    let position = offset_to_position(source_for(sources, path), *start);
    format!(
        "{}:{}:{}",
        path.file_name().unwrap().to_string_lossy(),
        position.line + 1,
        position.character + 1
    )
}

/// The references FCS reports in the fixture below that the handler does not
/// return: `<file>:<line>:<col> <symbol> (from …)`, sorted. Each is either a
/// location missing from the definition site's answer or a use site whose own
/// query returned nothing.
///
/// Two shapes today, both deferrals rather than wrong answers: a module is
/// answered nowhere, and a value reached through a qualified path into another
/// file (`Library.alpha`) is answered neither from that use nor from the
/// value's definition — the resolver defers the path, so the handler omits it.
const EXPECTED_DECLINED: &[&str] = &[
    "Client.fs:1:8 Client (from its definition)",
    "Client.fs:3:6 Library (from its definition)",
    "Client.fs:3:6 Library (from this use)",
    "Client.fs:6:19 Library (from its definition)",
    "Client.fs:6:19 Library (from this use)",
    "Client.fs:6:19 alpha (from its definition)",
    "Client.fs:6:19 alpha (from this use)",
    "Client.fs:8:17 Library (from its definition)",
    "Client.fs:8:17 Library (from this use)",
    "Client.fs:8:17 add (from its definition)",
    "Client.fs:8:17 add (from this use)",
    "Client.fs:9:18 Library (from its definition)",
    "Client.fs:9:18 Library (from this use)",
    "Client.fs:9:18 pair (from its definition)",
    "Client.fs:9:18 pair (from this use)",
    "Library.fs:1:8 Library (from its definition)",
];

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
    // No references of our own: this fixture's sources use only FSharp.Core and
    // the BCL, which the oracle's own SDK supplies.
    let json =
        invoke_fcs_dump_project_with_refs(&paths, &[], OracleRefScope::SdkPlusExtra, &[], None);
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
    // What the handler declined that FCS reports — the completeness direction.
    // The handler omits a `Deferred` occurrence by design, so this is not
    // asserted empty; it is pinned below, both ways, so a newly dropped
    // reference fails exactly as a newly found one does.
    let mut declined: Vec<String> = Vec::new();
    // What a query from a *use* site returned, against the definition site's.
    let mut use_site_queries = 0usize;
    for target in &targets {
        let with_declaration =
            check_answer(&mut state, &sources, &fcs, target, true, &mut coverage);
        let oracle = fcs_uses_of(&fcs, &target.key);
        for span in oracle.difference(&with_declaration) {
            declined.push(format!(
                "{} {} (from its definition)",
                render_span(&sources, span),
                target.key.name
            ));
        }
        if !with_declaration.is_empty() {
            coverage.answered_targets += 1;
            let without_declaration =
                check_answer(&mut state, &sources, &fcs, target, false, &mut coverage);
            assert!(
                without_declaration.len() < with_declaration.len(),
                "including the declaration added nothing for {:?}",
                target.key,
            );
        }

        // The same question asked from every use FCS reports, with the cursor
        // on the use's last byte (FCS names a use by its final identifier, so
        // in `Library.alpha` the record's first byte is on the module). Every
        // answer is held to the oracle by `check_answer`; a non-empty one must
        // also be the definition site's answer, since both name one symbol.
        for (path, start, end) in &oracle {
            let Some(cursor) = uses_for(&fcs, path)
                .uses
                .iter()
                .find(|u| (u.start, u.end) == (*start, *end) && u.name == target.key.name)
            else {
                continue;
            };
            if cursor.is_from_definition || cursor.start == cursor.end {
                continue;
            }
            use_site_queries += 1;
            let from_use = check_answer(
                &mut state,
                &sources,
                &fcs,
                &Target {
                    key: target.key.clone(),
                    cursor_file: path.clone(),
                    cursor_start: end - 1,
                },
                true,
                // The floors below are about the definition-site queries.
                &mut Coverage::default(),
            );
            if from_use.is_empty() {
                declined.push(format!(
                    "{} {} (from this use)",
                    render_span(&sources, &(path.clone(), *start, *end)),
                    target.key.name
                ));
            } else {
                assert_eq!(
                    from_use,
                    with_declaration,
                    "a cursor on the use of {:?} at {} was answered differently from its definition",
                    target.key,
                    render_span(&sources, &(path.clone(), *start, *end)),
                );
            }
        }
    }
    declined.sort();
    assert_eq!(
        declined, EXPECTED_DECLINED,
        "the references FCS reports that the handler does not changed; a removed line \
         is a reference now found, an added one a reference now lost"
    );
    assert!(
        use_site_queries >= 20,
        "{use_site_queries} use-site queries"
    );

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

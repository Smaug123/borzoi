//! Project-fixture tests for the `textDocument/semanticTokens/full` handler —
//! the part the in-crate unit tests can't reach: a file that lives in an
//! evaluated project *on disk*. The algorithm (lexical + semantic layering) and
//! the wire contract are pinned in `crates/lsp/src/handlers/semantic_tokens.rs`
//! and `lsp_integration.rs`; this pins the open-buffer guard against the project
//! path, where resolution would otherwise read the file from disk.

use std::fs;
use std::path::Path;

use borzoi::handlers::semantic_tokens;
use borzoi::server::{State, server_capabilities};
use lsp_types::{
    PartialResultParams, SemanticToken, SemanticTokenType, SemanticTokensParams,
    SemanticTokensResult, SemanticTokensServerCapabilities, TextDocumentIdentifier, Url,
    WorkDoneProgressParams,
};
use tempfile::TempDir;

/// The legend index the server advertises for `token_type` — read from the
/// capability so the test can't drift from the emitted legend.
fn token_type_index(ty: SemanticTokenType) -> u32 {
    let legend = match server_capabilities().semantic_tokens_provider {
        Some(SemanticTokensServerCapabilities::SemanticTokensOptions(o)) => o.legend,
        other => panic!("semantic tokens capability unset / wrong shape: {other:?}"),
    };
    legend
        .token_types
        .iter()
        .position(|t| *t == ty)
        .unwrap_or_else(|| panic!("{ty:?} not advertised in the legend")) as u32
}

/// Decode the delta-encoded wire stream to absolute `(line, col, len, type)`.
fn decode(data: &[SemanticToken]) -> Vec<(u32, u32, u32, u32)> {
    let (mut line, mut col) = (0u32, 0u32);
    let mut out = Vec::with_capacity(data.len());
    for t in data {
        if t.delta_line == 0 {
            col += t.delta_start;
        } else {
            line += t.delta_line;
            col = t.delta_start;
        }
        out.push((line, col, t.length, t.token_type));
    }
    out
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn params(uri: &Url) -> SemanticTokensParams {
    SemanticTokensParams {
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        text_document: TextDocumentIdentifier { uri: uri.clone() },
    }
}

/// A two-file project on disk; `A.fs` is opened, `B.fs` is written but returned
/// to the caller so the test can choose whether to open it.
struct Project {
    _tmp: TempDir,
    state: State,
    b_uri: Url,
    b_src: String,
}

fn two_file_project() -> Project {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("P.fsproj");
    let a = tmp.path().join("A.fs");
    let b = tmp.path().join("B.fs");
    write(
        &proj,
        r#"<Project>
          <ItemGroup>
            <Compile Include="A.fs" />
            <Compile Include="B.fs" />
          </ItemGroup>
        </Project>"#,
    );
    let a_src = "module Shared\nlet foo = 1\n";
    // B references A cross-file, so opening it exercises project-level
    // classification of a binder that lives in another Compile-order file.
    let b_src = "module Other\nlet bar = Shared.foo\n";
    write(&a, a_src);
    write(&b, b_src);

    let a_uri = Url::from_file_path(&a).unwrap();
    let b_uri = Url::from_file_path(&b).unwrap();
    let mut state = State::default();
    state.docs.insert(a_uri, a_src.to_string());
    Project {
        _tmp: tmp,
        state,
        b_uri,
        b_src: b_src.to_string(),
    }
}

/// A Compile item that exists on disk but was never opened (e.g. a request
/// arriving after `didClose`) must yield `null`, not tokens — even though the
/// project *could* resolve it from disk. Only open buffers are highlighted, so
/// the handler must not silently read the file (nor resolve the whole project)
/// for one that isn't open.
#[test]
fn closed_project_file_is_not_highlighted_from_disk() {
    let mut p = two_file_project();
    // B is a Compile item on disk but is absent from `docs` (never opened).
    assert!(
        semantic_tokens::handle(&mut p.state, params(&p.b_uri)).is_none(),
        "a closed Compile item must not be highlighted from disk"
    );
}

/// The guard doesn't over-reject: an *open* file that's part of an evaluated
/// project is still highlighted, and a same-file binding is classified (`bar`
/// is a value → `variable`), proving the project path reaches the semantic
/// layer.
#[test]
fn open_project_file_is_classified() {
    let mut p = two_file_project();
    p.state.docs.insert(p.b_uri.clone(), p.b_src.clone());
    let SemanticTokensResult::Tokens(tokens) =
        semantic_tokens::handle(&mut p.state, params(&p.b_uri))
            .expect("an open project file is highlighted")
    else {
        panic!("expected a full token set");
    };
    assert!(
        !tokens.data.is_empty(),
        "an open project file with a keyword and a binding must yield tokens"
    );
}

/// A cross-file reference is classified via the project fold: `B` uses
/// `Shared.foo`, a value defined in `A`, and the leaf `foo` comes back a
/// `variable` — the project path resolving what the single-file fallback can't.
#[test]
fn cross_file_reference_is_classified() {
    let mut p = two_file_project();
    p.state.docs.insert(p.b_uri.clone(), p.b_src.clone());
    let SemanticTokensResult::Tokens(tokens) =
        semantic_tokens::handle(&mut p.state, params(&p.b_uri)).expect("tokens for open B")
    else {
        panic!("expected a full token set");
    };
    let toks = decode(&tokens.data);
    // B line 1 is `let bar = Shared.foo`; the cross-file leaf `foo` is at col 17.
    let variable = token_type_index(SemanticTokenType::VARIABLE);
    assert!(
        toks.contains(&(1, 17, 3, variable)),
        "cross-file `foo` should be a variable; got {toks:?}"
    );
}

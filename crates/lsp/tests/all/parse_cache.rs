//! Stage 1 of incremental resolution: the per-file parse cache
//! ([`SemanticState::file_parses`]). A single-file edit must re-parse only the
//! file that changed and reuse every other file's tree verbatim, and the cache
//! must never serve a stale tree.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use borzoi::cst_panic_safe::parse_with_symbols;
use borzoi::semantic::ProjectParses;
use borzoi::server::State;
use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::syntax::{AstNode, ImplFile};
use lsp_types::Url;
use rowan::GreenNode;
use tempfile::TempDir;

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// A two-file project (`A.fs`, `B.fs`) with explicit Compile order.
fn two_file_project(tmp: &TempDir, a_src: &str, b_src: &str) -> PathBuf {
    let proj = tmp.path().join("P.fsproj");
    write(
        &proj,
        r#"<Project>
          <ItemGroup>
            <Compile Include="A.fs" />
            <Compile Include="B.fs" />
          </ItemGroup>
        </Project>"#,
    );
    write(&tmp.path().join("A.fs"), a_src);
    write(&tmp.path().join("B.fs"), b_src);
    proj
}

/// The comparable content of a `ProjectParses`: per file, its path, its exact
/// text, and its *structural* tree (a `GreenNode`, whose equality is recursive
/// and allocation-identity-independent — so a reused tree and a freshly parsed
/// identical tree compare equal here, unlike `SyntaxNode`).
fn snapshot(p: &ProjectParses) -> Vec<(PathBuf, String, GreenNode)> {
    (0..p.len())
        .map(|i| {
            (
                p.paths[i].clone(),
                p.texts[i].to_string(),
                p.files[i].file.syntax().green().into_owned(),
            )
        })
        .collect()
}

/// Rowan `SyntaxNode` equality is *identity* (same tree instance), not
/// structural: two independent parses of identical text are distinct nodes but
/// share a structurally-equal `GreenNode`. This underpins the reuse assertions
/// below — `syntax() == syntax()` there means "the very same tree", i.e. reused
/// rather than re-parsed. Pin the semantics so a rowan change can't quietly turn
/// the reuse checks vacuous.
#[test]
fn syntax_node_equality_is_identity_not_structural() {
    let src = "let a = 1\n";
    let syms = HashSet::new();
    let f1 = ImplFile::cast(
        parse_with_symbols(src, &syms, LanguageVersion::DEFAULT)
            .unwrap()
            .root,
    )
    .unwrap();
    let f2 = ImplFile::cast(
        parse_with_symbols(src, &syms, LanguageVersion::DEFAULT)
            .unwrap()
            .root,
    )
    .unwrap();
    assert_ne!(
        f1.syntax(),
        f2.syntax(),
        "independent parses must be distinct nodes (identity equality)"
    );
    assert_eq!(
        f1.syntax().green(),
        f2.syntax().green(),
        "...but structurally equal green trees"
    );
}

/// Editing one file re-parses only that file: the untouched file's tree is the
/// *same* node instance on the next build (a cache hit), while the edited file
/// is a new tree reflecting the new buffer text.
#[test]
fn edit_reparses_only_the_changed_file() {
    let tmp = TempDir::new().unwrap();
    let proj = two_file_project(&tmp, "let a = 1\n", "let b = 1\n");
    let b_uri = Url::from_file_path(tmp.path().join("B.fs")).unwrap();

    let mut state = State::default();

    // First build: both files parsed from disk and cached per file.
    let (a0, b0) = {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut state;
        let p = semantic
            .parses_for_project(&proj, workspace, docs)
            .expect("initial parses");
        assert_eq!(p.len(), 2);
        (p.files[0].clone(), p.files[1].clone())
    };

    // Edit B through a buffer overlay, then the text-sync invalidation.
    state.docs.insert(b_uri.clone(), "let b = 2\n".to_string());
    state.invalidate_owning_project(&b_uri);

    // Second build: A reused (same node), B re-parsed to the new text.
    let State {
        semantic,
        workspace,
        docs,
        ..
    } = &mut state;
    let p = semantic
        .parses_for_project(&proj, workspace, docs)
        .expect("rebuilt parses");
    assert_eq!(p.len(), 2);
    assert_eq!(
        p.files[0].file.syntax(),
        a0.file.syntax(),
        "unchanged A must be reused, not re-parsed"
    );
    assert_ne!(
        p.files[1].file.syntax(),
        b0.file.syntax(),
        "edited B must be re-parsed to a fresh tree"
    );
    assert_eq!(
        &*p.texts[1], "let b = 2\n",
        "B reflects the new buffer text"
    );
}

/// The cache never serves a stale tree: after a sequence of edits, the
/// incrementally-maintained `ProjectParses` is byte-for-byte what a cold build
/// of the same buffer state produces. Differential against a fresh `State`.
#[test]
fn cache_matches_a_cold_build_after_edits() {
    let tmp = TempDir::new().unwrap();
    let proj = two_file_project(&tmp, "let a = 1\n", "let b = 1\n");
    let a_uri = Url::from_file_path(tmp.path().join("A.fs")).unwrap();
    let b_uri = Url::from_file_path(tmp.path().join("B.fs")).unwrap();

    // Warm state: build, then apply a few edits, re-building through the cache
    // after each — including an edit and a revert (which must land back on the
    // original tree) and an edit to the *other* file.
    let mut warm = State::default();
    {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut warm;
        semantic
            .parses_for_project(&proj, workspace, docs)
            .expect("warm initial");
    }
    let edits = [
        (&b_uri, "let b = 2\n"),
        (&b_uri, "let b = 1\n"), // revert B
        (&a_uri, "let a = 99\n"),
    ];
    for (uri, text) in edits {
        warm.docs.insert((*uri).clone(), text.to_string());
        warm.invalidate_owning_project(uri);
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut warm;
        semantic
            .parses_for_project(&proj, workspace, docs)
            .expect("warm rebuild");
    }

    // Cold state: the same final buffer overlays, built once with an empty cache.
    let mut cold = State::default();
    cold.docs.insert(a_uri, "let a = 99\n".to_string());
    cold.docs.insert(b_uri, "let b = 1\n".to_string());

    let warm_snap = {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut warm;
        snapshot(
            semantic
                .parses_for_project(&proj, workspace, docs)
                .expect("warm final"),
        )
    };
    let cold_snap = {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut cold;
        snapshot(
            semantic
                .parses_for_project(&proj, workspace, docs)
                .expect("cold final"),
        )
    };
    assert_eq!(
        warm_snap, cold_snap,
        "the cached rebuild must equal a cold build of the same buffer state"
    );
}

/// A straddling source file (`match x with` — the F# 8 strict-indentation
/// boundary) shared by two projects must be judged against *each* project's
/// `LangVersion` trust, which is not a function of the file's text. One project
/// pins no version (knowable default → **trusted** provenance → folds); the
/// other writes `LangVersion` under an unpinnable item-condition (**untrusted**
/// provenance at the same effective version → a cold build refuses the fold).
///
/// The per-file parse cache keys on the parse *inputs*; the version-boundary
/// gate has one further input — the project's provenance — so a cached accept
/// from the trusted project must not satisfy the untrusted project. Regression
/// for the stage-1 cache: without carrying the provenance, the untrusted build
/// cache-hits and skips its gate, folding a project a cold build refuses.
#[test]
fn shared_straddling_file_respects_per_project_langversion_trust() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("A.fs"), "match x with\n");
    let trusted = tmp.path().join("Trusted.fsproj");
    write(
        &trusted,
        r#"<Project><ItemGroup><Compile Include="A.fs" /></ItemGroup></Project>"#,
    );
    let untrusted = tmp.path().join("Untrusted.fsproj");
    write(
        &untrusted,
        r#"<Project>
          <PropertyGroup>
            <LangVersion Condition="'@(NoSuchItem)' == 'yes'">7.0</LangVersion>
          </PropertyGroup>
          <ItemGroup><Compile Include="A.fs" /></ItemGroup>
        </Project>"#,
    );

    // Preconditions, each against a fresh cold cache: the trusted project folds,
    // the untrusted one refuses. If either stops holding the regression below is
    // meaningless (e.g. the two projects no longer parse at the same version, so
    // the shared cache never hits).
    {
        let mut s = State::default();
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut s;
        assert!(
            semantic
                .parses_for_project(&trusted, workspace, docs)
                .is_some(),
            "precondition: trusted project folds cold"
        );
    }
    {
        let mut s = State::default();
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut s;
        assert!(
            semantic
                .parses_for_project(&untrusted, workspace, docs)
                .is_none(),
            "precondition: untrusted project refuses cold"
        );
    }

    // The regression: one shared per-file cache. Warm it via the trusted project
    // (folds, caching A's accepted parse), then the untrusted project MUST still
    // refuse — its version-boundary gate cannot be satisfied by the cache hit.
    let mut s = State::default();
    let State {
        semantic,
        workspace,
        docs,
        ..
    } = &mut s;
    assert!(
        semantic
            .parses_for_project(&trusted, workspace, docs)
            .is_some(),
        "trusted warms the shared cache"
    );
    assert!(
        semantic
            .parses_for_project(&untrusted, workspace, docs)
            .is_none(),
        "untrusted must refuse the shared straddling file even though the \
         trusted project cached an accepted parse of it"
    );
}

/// A source file linked into two projects with *different* parse inputs (here,
/// different `DefineConstants`) keeps a cache variant per project, so building
/// one does not evict the other's tree: after both projects are built, editing
/// an unrelated file in the first and rebuilding it must still reuse the shared
/// file's tree. Guards against the single-entry-per-path thrash (codex).
#[test]
fn linked_file_under_divergent_projects_does_not_thrash() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("Shared.fs"), "let s = 1\n");
    write(&tmp.path().join("OnlyA.fs"), "let a = 1\n");
    // Project A: DefineConstants=FOO, sees Shared.fs + OnlyA.fs.
    let proj_a = tmp.path().join("A.fsproj");
    write(
        &proj_a,
        r#"<Project>
          <PropertyGroup><DefineConstants>FOO</DefineConstants></PropertyGroup>
          <ItemGroup>
            <Compile Include="Shared.fs" />
            <Compile Include="OnlyA.fs" />
          </ItemGroup>
        </Project>"#,
    );
    // Project B: DefineConstants=BAR, sees Shared.fs — same text, different
    // symbols, so a distinct cache variant of Shared.fs.
    let proj_b = tmp.path().join("B.fsproj");
    write(
        &proj_b,
        r#"<Project>
          <PropertyGroup><DefineConstants>BAR</DefineConstants></PropertyGroup>
          <ItemGroup><Compile Include="Shared.fs" /></ItemGroup>
        </Project>"#,
    );

    let mut state = State::default();

    // Build A, capturing Shared.fs's tree.
    let shared0 = {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut state;
        let p = semantic
            .parses_for_project(&proj_a, workspace, docs)
            .expect("A parses");
        let i = p
            .paths
            .iter()
            .position(|path| path.ends_with("Shared.fs"))
            .unwrap();
        p.files[i].clone()
    };

    // Build B — under the single-entry design this would evict A's Shared.fs
    // variant (same path, different symbols).
    {
        let State {
            semantic,
            workspace,
            docs,
            ..
        } = &mut state;
        semantic
            .parses_for_project(&proj_b, workspace, docs)
            .expect("B parses");
    }

    // Edit OnlyA.fs and rebuild A. Shared.fs is untouched, so its tree must be
    // reused from A's own variant — not re-parsed because B overwrote it.
    let only_a = Url::from_file_path(tmp.path().join("OnlyA.fs")).unwrap();
    state.docs.insert(only_a.clone(), "let a = 2\n".to_string());
    state.invalidate_owning_project(&only_a);

    let State {
        semantic,
        workspace,
        docs,
        ..
    } = &mut state;
    let p = semantic
        .parses_for_project(&proj_a, workspace, docs)
        .expect("A rebuilt");
    let i = p
        .paths
        .iter()
        .position(|path| path.ends_with("Shared.fs"))
        .unwrap();
    assert_eq!(
        p.files[i].file.syntax(),
        shared0.file.syntax(),
        "the linked Shared.fs must be reused, not re-parsed after building the \
         sibling project (no single-entry thrash)"
    );
}

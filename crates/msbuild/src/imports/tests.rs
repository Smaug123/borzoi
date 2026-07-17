use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use proptest::prelude::*;
use proptest::test_runner::{Config as PtConfig, TestRunner};
use tempfile::TempDir;

use super::detect_implicit_imports;
use crate::{Diagnostic, DiagnosticKind, ImplicitImportKind};

/// Extract `(kind, path)` pairs from a result vector. Most assertions
/// only care about these two fields; the span is `0..0` and is
/// checked once below.
fn pairs(diags: &[Diagnostic]) -> Vec<(ImplicitImportKind, PathBuf)> {
    diags
        .iter()
        .map(|d| match &d.kind {
            DiagnosticKind::ImplicitImportPresent { path, kind } => (*kind, path.clone()),
            other => panic!("unexpected diagnostic kind {other:?}"),
        })
        .collect()
}

#[test]
fn empty_tree_yields_no_diagnostics() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("Demo.fsproj");
    let got = under(&pairs(&detect_implicit_imports(&proj)), tmp.path());
    assert!(got.is_empty(), "got: {got:?}");
}

#[test]
fn nearest_directory_build_props_wins() {
    let tmp = TempDir::new().unwrap();
    let outer = tmp.path().join("outer");
    let inner = outer.join("inner");
    fs::create_dir_all(&inner).unwrap();
    fs::write(outer.join("Directory.Build.props"), "<Project/>").unwrap();
    let nearest = inner.join("Directory.Build.props");
    fs::write(&nearest, "<Project/>").unwrap();

    let proj = inner.join("Demo.fsproj");
    let got = under(&pairs(&detect_implicit_imports(&proj)), tmp.path());
    assert_eq!(got, [(ImplicitImportKind::DirectoryBuildProps, nearest)]);
}

#[test]
fn all_three_kinds_at_same_depth() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("proj");
    fs::create_dir_all(&dir).unwrap();
    let props = dir.join("Directory.Build.props");
    let targets = dir.join("Directory.Build.targets");
    let packages = dir.join("Directory.Packages.props");
    fs::write(&props, "<Project/>").unwrap();
    fs::write(&targets, "<Project/>").unwrap();
    fs::write(&packages, "<Project/>").unwrap();

    let proj = dir.join("Demo.fsproj");
    let got = under(&pairs(&detect_implicit_imports(&proj)), tmp.path());
    // Order within a single ancestor must be (Props, Targets, Packages).
    assert_eq!(
        got,
        [
            (ImplicitImportKind::DirectoryBuildProps, props),
            (ImplicitImportKind::DirectoryBuildTargets, targets),
            (ImplicitImportKind::DirectoryPackagesProps, packages),
        ]
    );
}

#[test]
fn distinct_kinds_can_resolve_to_different_depths() {
    let tmp = TempDir::new().unwrap();
    let outer = tmp.path().join("outer");
    let inner = outer.join("inner");
    fs::create_dir_all(&inner).unwrap();
    let props = inner.join("Directory.Build.props");
    let targets = outer.join("Directory.Build.targets");
    fs::write(&props, "<Project/>").unwrap();
    fs::write(&targets, "<Project/>").unwrap();

    let proj = inner.join("Demo.fsproj");
    let got = under(&pairs(&detect_implicit_imports(&proj)), tmp.path());
    assert_eq!(
        got,
        [
            (ImplicitImportKind::DirectoryBuildProps, props),
            (ImplicitImportKind::DirectoryBuildTargets, targets),
        ]
    );
}

#[test]
fn diagnostic_span_is_zero_zero() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    fs::write(dir.join("Directory.Build.props"), "<Project/>").unwrap();
    let proj = dir.join("Demo.fsproj");
    let diags: Vec<Diagnostic> = detect_implicit_imports(&proj)
        .into_iter()
        .filter(|d| match &d.kind {
            DiagnosticKind::ImplicitImportPresent { path, .. } => path.starts_with(tmp.path()),
            _ => false,
        })
        .collect();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].span, 0..0);
}

#[test]
fn directory_named_like_well_known_file_is_ignored() {
    // If a *directory* shares the name with the well-known file, we
    // must not report it — only regular files count. MSBuild itself
    // looks for a file.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    fs::create_dir(dir.join("Directory.Build.props")).unwrap();
    let proj = dir.join("Demo.fsproj");
    let got = under(&pairs(&detect_implicit_imports(&proj)), tmp.path());
    assert!(got.is_empty(), "directory should be ignored, got: {got:?}");
}

#[test]
fn bare_filename_project_path_yields_no_diagnostics() {
    // A `project_path` with no parent directory — `Path::parent` on
    // `Path::new("Demo.fsproj")` returns `Some("")`. The empty
    // ancestor would, if joined with a well-known filename and
    // stat-ed, probe the *current working directory* — which would
    // be wrong: the caller didn't ask us to look there. The helper
    // bails out early on relative project_paths to avoid this.
    let got = detect_implicit_imports(Path::new("Demo.fsproj"));
    assert!(got.is_empty(), "got: {got:?}");
}

#[test]
fn relative_project_path_does_not_probe_cwd() {
    // Concrete reproducer for the cwd-probing hazard: place a
    // `Directory.Build.props` in a tempdir, change cwd to that
    // tempdir, then ask for diagnostics on a bare-filename project
    // path. If the helper naively followed the `Some("")` parent
    // returned by `Path::parent` and joined it onto each well-known
    // name, `is_file` would resolve those joined names relative to
    // cwd and (falsely) report the file we just placed there as an
    // implicit import.
    //
    // The mutex serialises every cwd-mutating test in this binary;
    // no other test in the library touches `current_dir`, so the
    // cwd change is contained.
    use std::sync::Mutex;
    static CWD_GUARD: Mutex<()> = Mutex::new(());
    let _lock = CWD_GUARD.lock().unwrap_or_else(|e| e.into_inner());

    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("Directory.Build.props"), "<Project/>").unwrap();
    let prev_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();

    let got = detect_implicit_imports(Path::new("Demo.fsproj"));
    let got_relative = detect_implicit_imports(Path::new("./Demo.fsproj"));
    let got_nested_relative = detect_implicit_imports(Path::new("sub/Demo.fsproj"));

    // Always restore cwd before any assertion so a panic in this
    // test doesn't poison the binary's subsequent tests.
    std::env::set_current_dir(&prev_cwd).unwrap();

    assert!(
        got.is_empty(),
        "bare filename must not probe cwd, got: {got:?}"
    );
    assert!(
        got_relative.is_empty(),
        "./-prefixed relative path must not probe cwd, got: {got_relative:?}"
    );
    assert!(
        got_nested_relative.is_empty(),
        "nested relative path must not probe cwd, got: {got_nested_relative:?}"
    );
}

#[test]
fn project_path_need_not_exist() {
    // The project file itself never has to exist — the helper only
    // looks at ancestor directories for the well-known names.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    fs::write(dir.join("Directory.Build.props"), "<Project/>").unwrap();
    let proj = dir.join("does-not-exist.fsproj");
    let got = under(&pairs(&detect_implicit_imports(&proj)), tmp.path());
    assert_eq!(
        got,
        [(
            ImplicitImportKind::DirectoryBuildProps,
            dir.join("Directory.Build.props")
        )]
    );
}

#[test]
fn parent_dir_components_in_project_path_do_not_probe_phantom_ancestors() {
    // `/repo/a/../b/Demo.fsproj` lives in `/repo/b`. The walk must
    // collapse `..` lexically so we never stat `/repo/a/Directory.*` —
    // that directory is *not* on the project's real ancestor chain,
    // and a file under it would otherwise be falsely reported.
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    // Trap: this file is the one we must *not* surface.
    fs::write(a.join("Directory.Build.props"), "<Project/>").unwrap();
    // The expected match — placed in `/tmp/...` (the real grandparent
    // of `/tmp/.../b/Demo.fsproj`).
    let expected = tmp.path().join("Directory.Build.targets");
    fs::write(&expected, "<Project/>").unwrap();

    let project_path = a.join("..").join("b").join("Demo.fsproj");
    let got = under(&pairs(&detect_implicit_imports(&project_path)), tmp.path());
    assert_eq!(
        got,
        [(ImplicitImportKind::DirectoryBuildTargets, expected)],
        "phantom-ancestor trap not avoided"
    );
}

#[test]
fn cur_dir_components_in_project_path_are_skipped() {
    // `/tmp/.../proj/./Demo.fsproj` is the same as `/tmp/.../proj/Demo.fsproj`.
    // The `.` segment must not introduce a phantom level or otherwise
    // perturb the walk.
    let tmp = TempDir::new().unwrap();
    let proj_dir = tmp.path().join("proj");
    fs::create_dir_all(&proj_dir).unwrap();
    let expected = proj_dir.join("Directory.Build.props");
    fs::write(&expected, "<Project/>").unwrap();

    let project_path = proj_dir.join(".").join("Demo.fsproj");
    let got = under(&pairs(&detect_implicit_imports(&project_path)), tmp.path());
    assert_eq!(got, [(ImplicitImportKind::DirectoryBuildProps, expected)]);
}

/// Filter out anything not under `tmp_root` — defensive against the
/// off-chance some real-system ancestor of the tempdir actually
/// contains a `Directory.Build.props` (or sibling). All paths
/// returned by the helper are unresolved, so a literal `starts_with`
/// check matches paths constructed by joining onto `tmp.path()`.
fn under(
    diags: &[(ImplicitImportKind, PathBuf)],
    tmp_root: &Path,
) -> Vec<(ImplicitImportKind, PathBuf)> {
    diags
        .iter()
        .filter(|(_, p)| p.starts_with(tmp_root))
        .cloned()
        .collect()
}

// ---- Property test ----

/// Per-kind list of depths (relative to project dir, 0 = project's
/// own directory) at which to place the well-known file.
type Placements = BTreeMap<ImplicitImportKind, Vec<usize>>;

fn arb_placements() -> impl Strategy<Value = (u8, Placements)> {
    // Chain depth: 0..=4 intermediate directories above the project.
    // Project dir + up to 4 ancestors = up to 5 candidate levels.
    //
    // Per-kind generator: explicit 50/50 split between "no placement"
    // and "at least one placement". A naive `vec(0..levels, 0..=levels)`
    // would empty out only at rate 1/(levels+1), making the "no
    // implicit imports" regime undersampled. Splitting up front gives
    // direct control over the empty rate.
    (0u8..=4u8).prop_flat_map(|depth| {
        let levels = (depth as usize) + 1;
        let one_kind = move |kind: ImplicitImportKind| {
            prop_oneof![
                Just((kind, Vec::<usize>::new())),
                proptest::collection::vec(0usize..levels, 1..=levels).prop_map(move |v| {
                    let mut s = v;
                    s.sort_unstable();
                    s.dedup();
                    (kind, s)
                }),
            ]
        };
        (
            Just(depth),
            (
                one_kind(ImplicitImportKind::DirectoryBuildProps),
                one_kind(ImplicitImportKind::DirectoryBuildTargets),
                one_kind(ImplicitImportKind::DirectoryPackagesProps),
            )
                .prop_map(|(a, b, c)| {
                    let mut m: Placements = BTreeMap::new();
                    m.insert(a.0, a.1);
                    m.insert(b.0, b.1);
                    m.insert(c.0, c.1);
                    m
                }),
        )
    })
}

fn filename_for(kind: ImplicitImportKind) -> &'static str {
    match kind {
        ImplicitImportKind::DirectoryBuildProps => "Directory.Build.props",
        ImplicitImportKind::DirectoryBuildTargets => "Directory.Build.targets",
        ImplicitImportKind::DirectoryPackagesProps => "Directory.Packages.props",
    }
}

fn kind_order(k: ImplicitImportKind) -> u8 {
    match k {
        ImplicitImportKind::DirectoryBuildProps => 0,
        ImplicitImportKind::DirectoryBuildTargets => 1,
        ImplicitImportKind::DirectoryPackagesProps => 2,
    }
}

/// Build a chain of nested directories under `root`. Returns paths
/// from outermost (index 0 = `root` itself) to innermost (last). The
/// chain length equals `depth + 1`.
fn build_chain(root: &Path, depth: u8) -> Vec<PathBuf> {
    let mut path = root.to_path_buf();
    let mut out = vec![path.clone()];
    for i in 0..depth {
        path = path.join(format!("d{i}"));
        out.push(path.clone());
    }
    fs::create_dir_all(out.last().unwrap()).unwrap();
    out
}

/// Compute the expected diagnostic list for a given placement, in the
/// order detect_implicit_imports must return it: nearest-first by
/// depth, then by kind declaration order within an equal depth.
fn expected_for(
    placements: &Placements,
    chain: &[PathBuf],
    levels: usize,
) -> Vec<(ImplicitImportKind, PathBuf)> {
    let dir_at_depth = |d: usize| chain[levels - 1 - d].clone();
    let mut by_kind: Vec<(ImplicitImportKind, usize, PathBuf)> = Vec::new();
    for (kind, depths) in placements {
        if let Some(&min_d) = depths.first() {
            by_kind.push((*kind, min_d, dir_at_depth(min_d).join(filename_for(*kind))));
        }
    }
    // Nearest first = smallest depth first.
    by_kind.sort_by(|a, b| a.1.cmp(&b.1).then(kind_order(a.0).cmp(&kind_order(b.0))));
    by_kind.into_iter().map(|(k, _, p)| (k, p)).collect()
}

/// detect_implicit_imports returns exactly the nearest placed
/// instance of each kind (or nothing for kinds with no placement),
/// regardless of how many copies of a kind exist at deeper
/// ancestors. Also verifies that the generator hits the regimes we
/// care about (no-files, all-three, multiple-copies-of-one-kind).
#[test]
fn property_nearest_of_each_kind_is_reported() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Counters live inside the proptest closure (which must be `Fn`,
    // so we use interior mutability via atomics). After the run we
    // read them out for distribution sanity.
    let saw_empty = AtomicUsize::new(0);
    let saw_all_three = AtomicUsize::new(0);
    let saw_nearest_win = AtomicUsize::new(0);

    let mut runner = TestRunner::new(PtConfig {
        cases: 512,
        ..PtConfig::default()
    });
    let result = runner.run(&arb_placements(), |(depth, placements)| {
        let tmp = TempDir::new().unwrap();
        let chain = build_chain(tmp.path(), depth);
        let project_dir = chain.last().unwrap();
        let project_path = project_dir.join("Demo.fsproj");
        let levels = chain.len();
        let dir_at_depth = |d: usize| chain[levels - 1 - d].clone();

        // Place files.
        for (kind, depths) in &placements {
            for &d in depths {
                let path = dir_at_depth(d).join(filename_for(*kind));
                fs::write(&path, "<Project/>").unwrap();
            }
        }

        let expected = expected_for(&placements, &chain, levels);
        let got = under(&pairs(&detect_implicit_imports(&project_path)), tmp.path());

        prop_assert_eq!(
            &got,
            &expected,
            "mismatch: project={:?} placements={:?}",
            project_path,
            placements
        );

        let total_placed: usize = placements.values().map(|v| v.len()).sum();
        if total_placed == 0 {
            saw_empty.fetch_add(1, Ordering::Relaxed);
        }
        if placements.values().all(|v| !v.is_empty()) {
            saw_all_three.fetch_add(1, Ordering::Relaxed);
        }
        if placements.values().any(|v| v.len() >= 2) {
            saw_nearest_win.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    });
    result.unwrap();

    let saw_empty = saw_empty.load(Ordering::Relaxed);
    let saw_all_three = saw_all_three.load(Ordering::Relaxed);
    let saw_nearest_win = saw_nearest_win.load(Ordering::Relaxed);

    // Distribution sanity. The per-kind generator is 50/50 between
    // "empty" and "non-empty list of depths". Therefore, across 512
    // iterations:
    //   - "empty regime" (all three kinds empty): rate 1/8, expected
    //     count 64, σ ≈ 7.5. Threshold 10 = 7.2σ below mean,
    //     false-positive P < 1e-12.
    //   - "all three non-empty": rate 1/8, same bound.
    //   - "nearest win" (some kind has ≥2 placements): given a kind
    //     is non-empty, P(len ≥ 2) ≥ 4/5 at the smallest levels=2
    //     (uniform 1..=2 ⇒ 50/50) and grows with levels; combined
    //     with P(any non-empty) = 7/8, the regime rate is comfortably
    //     >30%, threshold 10 is many σ below mean.
    assert!(
        saw_empty >= 10,
        "empty regime under-sampled: {saw_empty} (expected ~64 of 512)"
    );
    assert!(
        saw_all_three >= 10,
        "all-three regime under-sampled: {saw_all_three} (expected ~64 of 512)"
    );
    assert!(
        saw_nearest_win >= 10,
        "nearest-win regime under-sampled: {saw_nearest_win}"
    );
}

//! End-to-end: two independent `SemanticState`s (a cold start, then a simulated
//! warm restart) sharing one on-disk cache directory. The second build must read
//! the cache instead of re-parsing every DLL, so its env build is faster while
//! producing the same-sized env.
//!
//! `#[ignore]`d; drive with `MEASURE_FSPROJ=/abs/path/to/restored.fsproj
//! cargo test --release -p borzoi --test all assembly_cache_project_e2e:: --
//! --ignored --nocapture`.

// The enabled cache is Unix-only — it needs POSIX overwrite-on-`rename` and
// degrades to disabled off-Unix (see `AssemblyCache::enabled`) — so the warm
// build only reads the cache there. Gated so the "warm beats cold" assertion
// isn't run against a disabled cache on a non-POSIX host.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use borzoi::assembly_cache::AssemblyCache;
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::semantic::SemanticState;
use borzoi::workspace::Workspace;

fn build_env_len(project: &std::path::Path, cache_dir: &std::path::Path) -> (usize, f64) {
    let mut workspace = Workspace::with_env(SdkDiscoveryEnv::from_process_env());
    let dotnet_root = workspace.dotnet_root_for_project(project);
    let mut sema = SemanticState::new();
    sema.set_assembly_cache(AssemblyCache::at(cache_dir.to_path_buf()));
    // Prime the parses cache so the timing isolates the assembly-env build.
    let docs: HashMap<lsp_types::Url, String> = HashMap::new();
    let mut workspace2 = Workspace::with_env(SdkDiscoveryEnv::from_process_env());
    let _ = sema.parses_for_project(project, &mut workspace2, &docs);

    let target_framework = workspace2.served_tfm_for_project(project);
    let t = Instant::now();
    let env = sema.assembly_env_for_project(
        project,
        dotnet_root.as_deref(),
        &target_framework,
        &workspace2,
    );
    let ms = t.elapsed().as_secs_f64() * 1e3;
    (env.len(), ms)
}

#[test]
#[ignore = "needs a restored .fsproj via MEASURE_FSPROJ; run --ignored --nocapture"]
fn warm_restart_reads_cache_and_is_faster() {
    let Some(fsproj) = std::env::var_os("MEASURE_FSPROJ") else {
        eprintln!("MEASURE_FSPROJ unset; skipping.");
        return;
    };
    let project = PathBuf::from(fsproj);
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("entities");

    // Cold: empty cache → parses+projects every DLL, populating the cache.
    let (len_cold, cold_ms) = build_env_len(&project, &cache_dir);
    // Warm restart: fresh SemanticState, same cache dir → reads the cache.
    let (len_warm, warm_ms) = build_env_len(&project, &cache_dir);

    println!(
        "{}: env types cold={len_cold} warm={len_warm} | build cold {cold_ms:.1}ms → warm {warm_ms:.1}ms | {:.1}x",
        project.display(),
        cold_ms / warm_ms.max(0.001),
    );
    assert_eq!(len_cold, len_warm, "warm env differs in size from cold");
    assert!(len_cold > 0, "empty env — project not restored?");
    assert!(
        warm_ms < cold_ms,
        "warm build ({warm_ms:.1}ms) should beat cold ({cold_ms:.1}ms)"
    );
}

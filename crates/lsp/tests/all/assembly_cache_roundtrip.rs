//! End-to-end check of the on-disk projection cache against a **real, complex
//! assembly**: the projected `Entity` tree must survive a serialize→deserialize
//! round-trip byte-for-byte (so a warm hit is indistinguishable from a fresh
//! projection), and a warm read must be faster than the cold parse+project.
//!
//! `#[ignore]`d because it needs a restored FSharp.Core on disk; run with
//! `--ignored --nocapture`. Skips (does not fail) when no FSharp.Core is found.

// The enabled cache is Unix-only — it needs POSIX overwrite-on-`rename` and
// degrades to disabled off-Unix (see `AssemblyCache::enabled`) — so a warm hit
// (asserted below) only ever occurs there. Gated so the assertion isn't run
// against a disabled cache on a non-POSIX host.
#![cfg(unix)]

use std::path::PathBuf;
use std::time::Instant;

use borzoi::assembly_cache::AssemblyCache;
use borzoi_assembly::{Ecma335Assembly, EcmaView};

/// A real FSharp.Core under the NuGet global package cache, if any.
fn find_fsharp_core() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let root = PathBuf::from(home).join(".nuget/packages/fsharp.core");
    let mut best: Option<PathBuf> = None;
    for ver in std::fs::read_dir(&root).ok()?.flatten() {
        for tfm in ["netstandard2.1", "netstandard2.0"] {
            let dll = ver.path().join("lib").join(tfm).join("FSharp.Core.dll");
            if dll.is_file() {
                best = Some(dll); // last wins; any is fine
            }
        }
    }
    best
}

#[test]
#[ignore = "needs a restored FSharp.Core; run with --ignored --nocapture"]
fn projection_survives_cache_round_trip_and_warm_is_faster() {
    let Some(dll) = find_fsharp_core() else {
        eprintln!("no FSharp.Core under ~/.nuget/packages/fsharp.core; skipping");
        return;
    };

    // Cold: read + parse + project directly (what a cache miss recomputes).
    let compute = || {
        let bytes = std::fs::read(&dll).unwrap();
        Ecma335Assembly::parse(&bytes)
            .unwrap()
            .enumerate_type_defs()
            .unwrap()
    };
    let t = Instant::now();
    let fresh = compute();
    let cold_ms = t.elapsed().as_secs_f64() * 1e3;
    assert!(!fresh.is_empty(), "FSharp.Core projected to zero types");

    let tmp = tempfile::tempdir().unwrap();
    let cache = AssemblyCache::at(tmp.path().to_path_buf());

    // Miss, then populate.
    assert!(cache.get(&dll).is_none(), "fresh cache should miss");
    cache.put(&dll, &fresh);

    // Warm: the hit must equal a fresh projection exactly (serde round-trip), and
    // be faster than the cold recompute.
    let t = Instant::now();
    let hit = cache.get(&dll).expect("warm cache should hit");
    let warm_ms = t.elapsed().as_secs_f64() * 1e3;

    assert_eq!(hit, fresh, "cached projection differs from a fresh one");
    println!(
        "FSharp.Core: {} types | cold parse+project {cold_ms:.1}ms | warm cache read {warm_ms:.1}ms | {:.1}x",
        fresh.len(),
        cold_ms / warm_ms.max(0.001),
    );
    assert!(
        warm_ms < cold_ms,
        "warm read ({warm_ms:.1}ms) should beat cold compute ({cold_ms:.1}ms)"
    );
}

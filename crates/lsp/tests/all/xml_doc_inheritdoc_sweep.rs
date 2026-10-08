//! `<inheritdoc>` expansion against Roslyn's, over real shipped documentation.
//!
//! The reference set is the SDK's NETCore and ASP.NET Core targeting packs
//! (every DLL, each simple name once) — the same set for the env and for
//! Roslyn's compilation — and every inheritdoc-bearing entry of every ASP.NET
//! Core assembly is compared under certain-implies-exact. The census of
//! declines by cause is printed. `BORZOI_INHERITDOC_SWEEP_ROOT` (a NuGet
//! global-packages folder) adds, per package id, its highest version's newest
//! .NET `lib/`/`ref/` folder, referenced together with both packs.
//!
//! Gated: zero disagreements, and the ASP.NET expanded count pinned exactly
//! (both directions — the pack is pinned by the flake, the expansion
//! deterministic, so any movement is a change to explain).
//!
//! ```sh
//! nix develop -c cargo test -p borzoi --test all xml_doc_inheritdoc_sweep:: -- --ignored --nocapture
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use borzoi::assembly_cache::AssemblyCache;
use borzoi::semantic::build_env_from_dll_paths;
use borzoi::xml_doc::lookup::DocSources;

use crate::common::ensure_system_runtime_dll;
use crate::common::inheritdoc_diff::{Census, compare_assembly};

/// ASP.NET Core entries expanded exactly as Roslyn expands them, measured
/// 2026-10-08 over Microsoft.AspNetCore.App.Ref 10.0.9.
const ASPNET_EXPANDED: usize = 2104;

fn dlls_in(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "dll"))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Each simple name once, first directory winning.
fn reference_set(dirs: &[&Path]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        for dll in dlls_in(dir) {
            let name = dll.file_name().unwrap().to_string_lossy().to_lowercase();
            if seen.insert(name) {
                out.push(dll);
            }
        }
    }
    out
}

fn packs() -> (PathBuf, PathBuf) {
    let netcore = ensure_system_runtime_dll()
        .parent()
        .expect("ref dir")
        .to_path_buf();
    let version = netcore
        .parent()
        .and_then(Path::parent)
        .expect("pack version dir");
    let packs = version.parent().and_then(Path::parent).expect("packs dir");
    let aspnet = packs
        .join("Microsoft.AspNetCore.App.Ref")
        .join(version.file_name().unwrap())
        .join("ref")
        .join("net10.0");
    assert!(
        aspnet.is_dir(),
        "no ASP.NET Core pack at {}",
        aspnet.display()
    );
    (netcore, aspnet)
}

fn sweep(references: &[PathBuf], targets: &[PathBuf]) -> Census {
    let (env, _) = build_env_from_dll_paths(
        references.iter().map(PathBuf::as_path),
        &AssemblyCache::disabled(),
    );
    let env = Arc::new(env);
    let mut sources = DocSources::default();
    let mut census = Census::default();
    for dll in targets {
        census.add(&compare_assembly(&env, &mut sources, references, dll));
    }
    census
}

#[test]
#[ignore = "sweeps the ASP.NET Core targeting pack against Roslyn; run explicitly"]
fn aspnet_pack_expands_exactly_as_roslyn() {
    let (netcore, aspnet) = packs();
    let references = reference_set(&[&netcore, &aspnet]);
    let targets: Vec<PathBuf> = references
        .iter()
        .filter(|p| p.starts_with(&aspnet))
        .cloned()
        .collect();
    let census = sweep(&references, &targets);
    census.print("ASP.NET Core 10 targeting pack");
    census.assert_sound();

    if let Some(root) = std::env::var_os("BORZOI_INHERITDOC_SWEEP_ROOT") {
        let mut nuget = Census::default();
        for dir in package_dirs(Path::new(&root)) {
            let references = reference_set(&[&netcore, &aspnet, &dir]);
            let targets: Vec<PathBuf> = references
                .iter()
                .filter(|p| p.starts_with(&dir))
                .cloned()
                .collect();
            if targets.is_empty() {
                continue;
            }
            nuget.merge(sweep(&references, &targets));
        }
        nuget.print("NuGet cache (highest version per package)");
        nuget.assert_sound();
    }

    assert_eq!(
        census.agrees, ASPNET_EXPANDED,
        "the ASP.NET Core expanded count moved; if every change is intended, update ASPNET_EXPANDED"
    );
}

/// Per package id under `root`, its highest version's newest .NET folder
/// (`lib/` before `ref/`), when that folder documents an `<inheritdoc>`.
fn package_dirs(root: &Path) -> Vec<PathBuf> {
    const TFMS: &[&str] = &[
        "net10.0",
        "net9.0",
        "net8.0",
        "net7.0",
        "net6.0",
        "net5.0",
        "netcoreapp3.1",
        "netstandard2.1",
        "netstandard2.0",
    ];
    let children = |dir: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        v.sort();
        v
    };
    let version_key = |v: &Path| -> (bool, Vec<u64>) {
        let s = v.file_name().unwrap().to_string_lossy().to_string();
        let release = !s.contains('-');
        let nums = s
            .split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        (release, nums)
    };
    let mut out = Vec::new();
    for package in children(root) {
        let Some(version) = children(&package)
            .into_iter()
            .filter(|p| p.is_dir())
            .max_by_key(|v| version_key(v))
        else {
            continue;
        };
        let chosen = ["lib", "ref"].iter().find_map(|kind| {
            TFMS.iter()
                .map(|tfm| version.join(kind).join(tfm))
                .find(|d| d.is_dir())
        });
        let Some(dir) = chosen else { continue };
        let documents_inheritdoc = dlls_in(&dir).iter().any(|dll| {
            std::fs::read(dll.with_extension("xml"))
                .is_ok_and(|bytes| bytes.windows(11).any(|w| w == b"<inheritdoc"))
        });
        if documents_inheritdoc {
            out.push(dir);
        }
    }
    out
}

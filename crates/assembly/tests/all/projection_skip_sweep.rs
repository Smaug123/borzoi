//! The whole-DLL projection skip gate.
//!
//! The LSP's per-DLL degradation (`enumerate_dll_type_defs`) skips an assembly
//! entirely when the file cannot be read, the PE/metadata cannot be parsed, the
//! type-def walk errors, or the reader panics. A skipped DLL is the *worst*
//! grade of uncertainty the resolver has to model: with a whole assembly's
//! types missing, no reading into any other assembly is provably unshadowed,
//! because the missing DLL could declare a colliding type or an assembly-level
//! `[<AutoOpen>]`. Every finer degradation — a dropped type, an unreadable
//! `AutoOpen` list — is namespace- or feature-scoped by comparison.
//!
//! So this sweep gates the rate at **zero**: run the LSP's exact pipeline
//! (read → [`Ecma335Assembly::parse`] →
//! [`EcmaView::enumerate_type_defs_with_skips`], each panic-safe) over a real
//! DLL population and fail if any assembly a project could actually reference
//! is lost. It is the standing check behind treating a skipped DLL as a
//! should-never-happen rather than a routine degradation.
//!
//! `#[ignore]`d and env-driven: it needs a populated package cache, not a
//! fixture, so it is a local ratchet in the mould of `resolve_real_project_diff`
//! rather than a CI job.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use borzoi_assembly::{Ecma335Assembly, EcmaView};
use borzoi_oracle_harness::panic_silence::catch_unwind_silent;

/// What the LSP's per-DLL pipeline made of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// Projected: the resolver sees every type this DLL declares.
    Projected {
        /// Types the projector dropped individually. Namespace-scoped
        /// uncertainty, not an assembly loss — reported for contrast, not gated.
        dropped_types: usize,
        /// Whether the assembly-level `[<AutoOpen>]` list could not be read.
        /// Global uncertainty, but it does not cost the assembly's types.
        auto_opens_unreadable: bool,
    },
    /// Skipped whole, carrying the stage and the refusal's `Display` so a gate
    /// failure names the cause rather than only the path.
    Skipped(String),
}

/// Run the LSP's pipeline over one DLL.
fn probe_dll(path: &Path) -> Outcome {
    let Ok(bytes) = std::fs::read(path) else {
        return Outcome::Skipped("read: failed".to_owned());
    };
    let parsed = match catch_unwind_silent(|| Ecma335Assembly::parse(&bytes)) {
        Err(_) => return Outcome::Skipped("parse: panicked".to_owned()),
        Ok(Err(e)) => return Outcome::Skipped(format!("parse: {e}")),
        Ok(Ok(view)) => view,
    };
    match catch_unwind_silent(|| parsed.enumerate_type_defs_with_skips()) {
        Err(_) => Outcome::Skipped("enumerate: panicked".to_owned()),
        Ok(Err(e)) => Outcome::Skipped(format!("enumerate: {e}")),
        Ok(Ok((_, skips))) => Outcome::Projected {
            dropped_types: skips.dropped_types.len(),
            auto_opens_unreadable: !matches!(
                catch_unwind_silent(|| parsed.assembly_auto_opens()),
                Ok(Ok(_))
            ),
        },
    }
}

/// Whether NuGet's content model could hand this DLL to the compiler as a
/// **compile asset**. Compile assets are the `.dll`s sitting *directly* in
/// `ref/<tfm>/` or `lib/<tfm>/` — one component for the framework, then the
/// file. Everything else a package ships is invisible to the compiler:
/// `runtimes/…/native/`, `tools/`, `analyzers/`, `build/`, and — the case that
/// matters here — the per-architecture subdirectories (`lib/net8.0/x64/…`) that
/// carry *native* DLLs. Those are not ECMA-335 at all, so refusing them is
/// correct behaviour rather than a projector gap, and testing the position
/// structurally keeps them out without exempting an error message (which would
/// also exempt a *managed* DLL our PE reader wrongly called headerless).
fn is_compile_asset(path: &Path) -> bool {
    let mut up = path.ancestors().skip(1); // skip the file itself
    let Some(_tfm) = up.next() else {
        return false;
    };
    up.next()
        .and_then(Path::file_name)
        .is_some_and(|n| n == "lib" || n == "ref")
}

fn collect_dlls(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_dlls(&path, out),
            Ok(t) if t.is_file() && path.extension().and_then(|e| e.to_str()) == Some("dll") => {
                out.push(path);
            }
            _ => {}
        }
    }
}

/// Sweep `BORZOI_DLL_SWEEP_ROOT` (colon-separated roots) and **fail if any
/// compile-asset DLL is skipped whole**. Prints usage and returns green when
/// unset.
#[test]
#[ignore = "needs a populated local package cache; run explicitly"]
fn no_compile_asset_dll_is_skipped_whole() {
    let Some(roots) = std::env::var_os("BORZOI_DLL_SWEEP_ROOT") else {
        eprintln!(
            "set BORZOI_DLL_SWEEP_ROOT=/path/to/.nuget/packages[:/more/roots] to run this sweep"
        );
        return;
    };
    let mut dlls = Vec::new();
    for root in roots.to_string_lossy().split(':').filter(|s| !s.is_empty()) {
        collect_dlls(Path::new(root), &mut dlls);
    }
    dlls.sort();
    eprintln!("[skip-sweep] {} DLLs under {:?}", dlls.len(), roots);

    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunks: Vec<Vec<PathBuf>> = dlls
        .chunks(dlls.len().div_ceil(threads).max(1))
        .map(<[PathBuf]>::to_vec)
        .collect();
    let results: Vec<(PathBuf, bool, Outcome)> = std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|p| (p.clone(), is_compile_asset(p), probe_dll(p)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .flatten()
            .collect()
    });
    assert!(
        !results.is_empty(),
        "no DLLs found under {roots:?} — the gate would pass vacuously"
    );

    report(&results);

    // The gate. Grouped by reason so one reader gap reads as one line rather
    // than as its hundred affected package versions.
    let mut by_reason: BTreeMap<&str, Vec<&Path>> = BTreeMap::new();
    for (path, asset, outcome) in &results {
        if let (true, Outcome::Skipped(reason)) = (*asset, outcome) {
            by_reason.entry(reason).or_default().push(path);
        }
    }
    assert!(
        by_reason.is_empty(),
        "{} compile-asset DLL(s) were skipped whole — a project referencing one loses every \
         type in it. Reasons:\n{}",
        by_reason.values().map(Vec::len).sum::<usize>(),
        by_reason
            .iter()
            .map(|(reason, paths)| format!(
                "  {} × {reason}\n    e.g. {}",
                paths.len(),
                paths[0].display()
            ))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// The gate's own two moving parts, pinned hermetically: a sweep that can only
/// be run against a package cache is a sweep whose *failure* path never
/// executes, and a position filter that quietly said `false` everywhere would
/// make the zero above vacuous.
#[test]
fn the_gate_classifies_position_and_refusal() {
    // Compile assets: the DLL sits directly in the framework folder.
    for asset in [
        "/c/.nuget/packages/microsoft.build/17.11.4/ref/net8.0/Microsoft.Build.dll",
        "/c/.nuget/packages/fsharp.core/9.0.100/lib/netstandard2.1/FSharp.Core.dll",
    ] {
        assert!(
            is_compile_asset(Path::new(asset)),
            "{asset} is a compile asset"
        );
    }
    // Not compile assets. The arch subdirectories are the ones that matter:
    // they carry *native* DLLs, which the reader is right to refuse, and which
    // the gate must therefore not count.
    for other in [
        "/c/.nuget/packages/microsoft.testplatform.testhost/18.8.1/lib/net8.0/x64/msdia140.dll",
        "/c/.nuget/packages/some.pkg/1.0.0/runtimes/win-x64/native/libfoo.dll",
        "/c/.nuget/packages/nerdbank.gitversioning/3.5.119/build/MSBuildFull/lib/win32/x64/git2.dll",
        "/c/.nuget/packages/some.pkg/1.0.0/tools/net8.0/any/tool.dll",
        "/c/loose.dll",
    ] {
        assert!(
            !is_compile_asset(Path::new(other)),
            "{other} is not a compile asset"
        );
    }

    // And the refusal path: bytes that are not a PE at all must come back
    // `Skipped`, carrying a reason, rather than reading as a clean projection.
    let dir = std::env::temp_dir().join("borzoi-skip-gate-selftest");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let not_an_assembly = dir.join("NotAnAssembly.dll");
    std::fs::write(&not_an_assembly, b"MZ but nothing else").expect("write scratch dll");
    match probe_dll(&not_an_assembly) {
        Outcome::Skipped(reason) => assert!(!reason.is_empty(), "the refusal names a cause"),
        other => panic!("garbage bytes must be refused, got {other:?}"),
    }
    std::fs::remove_file(&not_an_assembly).expect("clean up");
}

/// Print the population and the *finer* degradations beside the gated zero.
/// Neither a dropped type nor an unreadable `AutoOpen` list costs an assembly,
/// so neither is gated — but a jump in either is worth seeing.
fn report(results: &[(PathBuf, bool, Outcome)]) {
    let count = |asset: bool, projected: bool| {
        results
            .iter()
            .filter(|(_, a, o)| *a == asset && matches!(o, Outcome::Projected { .. }) == projected)
            .count()
    };
    let dropped: usize = results
        .iter()
        .filter_map(|(_, _, o)| match o {
            Outcome::Projected { dropped_types, .. } => Some(*dropped_types),
            Outcome::Skipped(_) => None,
        })
        .sum();
    let unreadable_auto_opens = results
        .iter()
        .filter(|(_, _, o)| {
            matches!(
                o,
                Outcome::Projected {
                    auto_opens_unreadable: true,
                    ..
                }
            )
        })
        .count();
    eprintln!(
        "[skip-sweep] compile-asset: {} projected, {} skipped | other: {} projected, \
         {} skipped (expected: native DLLs) | dropped types: {dropped} \
         | unreadable AutoOpen lists: {unreadable_auto_opens}",
        count(true, true),
        count(true, false),
        count(false, true),
        count(false, false),
    );
}

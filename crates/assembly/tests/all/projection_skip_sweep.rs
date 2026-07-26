//! The whole-DLL projection skip gate.
//!
//! The LSP's per-DLL degradation (`enumerate_dll_type_defs`) skips an assembly
//! entirely when the file cannot be read, the PE/metadata cannot be parsed, the
//! type-def walk errors, or the reader panics. A skipped assembly is the *worst*
//! grade of uncertainty the resolver has to model: with a whole assembly's types
//! missing, no reading into any *other* assembly is provably unshadowed, because
//! the missing one could declare a colliding type or an assembly-level
//! `[<AutoOpen>]`. Every finer degradation — a dropped type, an unreadable
//! `AutoOpen` list — is namespace- or feature-scoped by comparison.
//!
//! So this sweep gates the rate: run the LSP's exact pipeline
//! (read → [`Ecma335Assembly::parse`] →
//! [`EcmaView::enumerate_type_defs_with_skips`], each panic-safe) over a real
//! population and fail on any whole-assembly loss whose cause is not on the
//! short, named [`EXEMPT`] list.
//!
//! **It classifies by cause, not by path.** The tempting filter — "only gate the
//! files NuGet could select as compile assets" — means reimplementing the
//! content model (`borzoi_nuget::assets`) here, and a hand-rolled approximation
//! is wrong in *both* directions: it admits `tools/…/lib/win32/Foo.dll` and
//! rejects the valid pre-TFM flat `lib/Foo.dll`. Since a misclassification in
//! the rejecting direction silently hides exactly the regression this exists to
//! catch, the sweep looks at every assembly it can find and justifies each loss
//! individually instead.
//!
//! `#[ignore]`d and env-driven: it needs a populated package cache, not a
//! fixture, so it is a local ratchet in the mould of `resolve_real_project_diff`
//! rather than a CI job.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use borzoi_assembly::{Ecma335Assembly, EcmaView};
use borzoi_oracle_harness::panic_silence::catch_unwind_silent;

/// Extensions NuGet's content model accepts as a compile assembly, matched
/// case-insensitively (`borzoi_nuget::assets`). `.exe` is not decorative:
/// `microsoft.build.runtime/…/ref/net472/MSBuild.exe` is a real compile asset.
const ASSEMBLY_EXTENSIONS: &[&str] = &["dll", "exe", "winmd"];

/// Whole-assembly losses this gate tolerates, each with the reason it is not a
/// projector gap. A cause reaches this list only by being *justified*, never by
/// being common — an entry is a standing hole in the gate, so keep it short and
/// keep the justification honest.
const EXEMPT: &[(&str, &str)] = &[
    // A PE with no CLI header is not a managed assembly at all — a native DLL
    // (`msdia140.dll`, `git2-*.dll`) or a native EXE. There is nothing for an
    // ECMA-335 reader to read, and fsc would refuse it identically, so the
    // refusal is correct behaviour rather than a gap.
    (
        "no CLI header",
        "not a managed assembly (native PE); fsc refuses it too",
    ),
    // The .NET Framework targeting packs ship two shapes the reader refuses
    // whole. Both are real gaps of the same kind #199 fixed — a per-item
    // degradation propagated as a whole-image error — but they are reachable
    // only from a net4x target, so whether to close them waits on whether
    // borzoi supports net4x at all. Tracked separately; exempt, not forgotten.
    (
        "assembly has no Assembly manifest record",
        "netmodule in the net4x targeting pack; pending the net4x-support decision",
    ),
    (
        "manifest resource is not embedded in this file",
        "net4x targeting-pack mscorlib; pending the net4x-support decision",
    ),
];

/// A population smaller than this is not a package cache — it is a mistyped
/// root, or one whose subtrees are unreadable. The gate says nothing useful
/// about such a population, so it fails rather than passing vacuously.
const MIN_PROJECTED: usize = 1_000;

/// What the LSP's per-DLL pipeline made of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// Projected: the resolver sees every type this assembly declares.
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

impl Outcome {
    /// The exemption justifying this loss, if any.
    fn exemption(&self) -> Option<&'static str> {
        let Outcome::Skipped(reason) = self else {
            return None;
        };
        EXEMPT
            .iter()
            .find(|(needle, _)| reason.contains(needle))
            .map(|(_, why)| *why)
    }
}

/// Run the LSP's pipeline over one assembly file.
fn probe_assembly(path: &Path) -> Outcome {
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

fn is_assembly_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            ASSEMBLY_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// Collect every assembly file under `root`. A directory that cannot be read is
/// pushed to `unreadable` rather than silently treated as empty: an unreadable
/// subtree is *unexamined*, and a gate that cannot tell "nothing wrong here"
/// from "did not look here" is not a gate.
fn collect_assemblies(
    root: &Path,
    out: &mut Vec<PathBuf>,
    unreadable: &mut Vec<(PathBuf, String)>,
) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            unreadable.push((root.to_path_buf(), e.to_string()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                unreadable.push((root.to_path_buf(), e.to_string()));
                continue;
            }
        };
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_assemblies(&path, out, unreadable),
            Ok(t) if t.is_file() && is_assembly_file(&path) => out.push(path),
            Ok(_) => {}
            Err(e) => unreadable.push((path, e.to_string())),
        }
    }
}

/// Sweep `BORZOI_DLL_SWEEP_ROOT` (colon-separated roots) and **fail on any
/// whole-assembly loss without a named exemption**. Prints usage and returns
/// green when unset.
#[test]
#[ignore = "needs a populated local package cache; run explicitly"]
fn no_assembly_is_skipped_whole_without_a_named_reason() {
    let Some(roots) = std::env::var_os("BORZOI_DLL_SWEEP_ROOT") else {
        eprintln!(
            "set BORZOI_DLL_SWEEP_ROOT=/path/to/.nuget/packages[:/more/roots] to run this sweep"
        );
        return;
    };
    let mut files = Vec::new();
    let mut unreadable = Vec::new();
    for root in roots.to_string_lossy().split(':').filter(|s| !s.is_empty()) {
        collect_assemblies(Path::new(root), &mut files, &mut unreadable);
    }
    files.sort();
    assert!(
        unreadable.is_empty(),
        "{} subtree(s) under {roots:?} could not be enumerated, so the sweep did not examine \
         them and cannot claim anything about their assemblies:\n{}",
        unreadable.len(),
        unreadable
            .iter()
            .map(|(p, e)| format!("  {}: {e}", p.display()))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    eprintln!("[skip-sweep] {} assemblies under {roots:?}", files.len());

    let results = probe_all(&files);
    let projected = results
        .iter()
        .filter(|(_, o)| matches!(o, Outcome::Projected { .. }))
        .count();
    assert!(
        projected >= MIN_PROJECTED,
        "only {projected} assemblies projected under {roots:?} (need {MIN_PROJECTED}) — that is \
         not a package cache, and a gate over it would pass without examining anything",
    );
    report(&results, projected);

    // The gate. Grouped by reason so one reader gap reads as one line rather
    // than as its hundred affected package versions — which is how #199's
    // single root cause presented, as 77 separately skipped files.
    let mut by_reason: BTreeMap<&str, Vec<&Path>> = BTreeMap::new();
    for (path, outcome) in &results {
        if let (Outcome::Skipped(reason), None) = (outcome, outcome.exemption()) {
            by_reason.entry(reason).or_default().push(path);
        }
    }
    assert!(
        by_reason.is_empty(),
        "{} assembly file(s) were skipped whole for a cause with no named exemption — a project \
         referencing one loses every type in it. Either fix the reader, or add the cause to \
         `EXEMPT` with a justification:\n{}",
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

/// Probe every file, fanned out across the available cores.
fn probe_all(files: &[PathBuf]) -> Vec<(PathBuf, Outcome)> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunks: Vec<Vec<PathBuf>> = files
        .chunks(files.len().div_ceil(threads).max(1))
        .map(<[PathBuf]>::to_vec)
        .collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|p| (p.clone(), probe_assembly(p)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .flatten()
            .collect()
    })
}

/// Print the population, the exempted losses by justification, and the *finer*
/// degradations. Neither a dropped type nor an unreadable `AutoOpen` list costs
/// an assembly, so neither is gated — but a jump in either is worth seeing next
/// to the gated zero.
fn report(results: &[(PathBuf, Outcome)], projected: usize) {
    let dropped: usize = results
        .iter()
        .filter_map(|(_, o)| match o {
            Outcome::Projected { dropped_types, .. } => Some(*dropped_types),
            Outcome::Skipped(_) => None,
        })
        .sum();
    let unreadable_auto_opens = results
        .iter()
        .filter(|(_, o)| {
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
        "[skip-sweep] projected: {projected} | dropped types: {dropped} \
         | unreadable AutoOpen lists: {unreadable_auto_opens}"
    );
    let mut exempted: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, outcome) in results {
        if let Some(why) = outcome.exemption() {
            *exempted.entry(why).or_default() += 1;
        }
    }
    for (why, count) in &exempted {
        eprintln!("[skip-sweep] exempt: {count} × {why}");
    }
}

/// The gate's own moving parts, pinned hermetically: a sweep that can only run
/// against a package cache is a sweep whose *failure* path never executes.
#[test]
fn the_gate_recognises_a_refusal_and_its_exemptions() {
    // Bytes that are not a PE at all must come back `Skipped`, carrying a
    // reason, rather than reading as a clean projection. A unique directory per
    // run: sibling worktrees run this suite concurrently, and a shared path
    // would race on the write and the cleanup.
    let dir = tempfile::tempdir().expect("scratch dir");
    let not_an_assembly = dir.path().join("NotAnAssembly.dll");
    std::fs::write(&not_an_assembly, b"MZ but nothing else").expect("write scratch file");
    let outcome = probe_assembly(&not_an_assembly);
    let Outcome::Skipped(reason) = &outcome else {
        panic!("garbage bytes must be refused, got {outcome:?}");
    };
    // Garbage that is not even a PE is refused before the CLI-header check, so
    // it is *not* exempt — the exemption is narrow, as intended.
    assert!(!reason.is_empty(), "the refusal names a cause");
    assert_eq!(
        outcome.exemption(),
        None,
        "an unrecognised refusal must reach the gate, not be waved through"
    );

    // Every extension NuGet accepts is swept, case-insensitively; a `.pdb`
    // beside them is not.
    for name in ["A.dll", "B.EXE", "C.WinMD"] {
        assert!(is_assembly_file(Path::new(name)), "{name} is an assembly");
    }
    for name in ["A.pdb", "B.xml", "C"] {
        assert!(
            !is_assembly_file(Path::new(name)),
            "{name} is not an assembly"
        );
    }

    // An unreadable root is recorded rather than read as an empty subtree.
    let mut files = Vec::new();
    let mut unreadable = Vec::new();
    collect_assemblies(&dir.path().join("no-such-dir"), &mut files, &mut unreadable);
    assert!(files.is_empty());
    assert_eq!(unreadable.len(), 1, "the missing subtree is reported");
}

/// Each exemption must be a *narrow* substring of a real refusal, not a phrase
/// broad enough to wave through causes it was never justified for.
#[test]
fn every_exemption_is_justified_and_narrow() {
    for (needle, why) in EXEMPT {
        assert!(!needle.is_empty() && !why.is_empty(), "{needle:?}: {why:?}");
        // A one- or two-word needle ("assembly", "not embedded") would match
        // refusals nobody vetted. The real ones are full clauses.
        assert!(
            needle.split_whitespace().count() >= 3,
            "exemption {needle:?} is too broad to have been justified",
        );
    }
}

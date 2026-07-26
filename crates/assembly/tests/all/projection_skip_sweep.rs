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
//! **The population is not filtered by path.** The tempting filter — "only gate
//! the files NuGet could select as compile assets" — means reimplementing the
//! content model (`borzoi_nuget::assets`) here, and a hand-rolled approximation
//! is wrong in *both* directions: it admits `tools/…/lib/win32/Foo.dll` and
//! rejects the valid pre-TFM flat `lib/Foo.dll`. Since a misclassification in
//! the rejecting direction silently hides exactly the regression this exists to
//! catch, the sweep looks at every assembly it can find and justifies each loss
//! individually instead. Paths do appear in the [`Exemption`] scopes — but see
//! there for why that position is the safe one.
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

/// One tolerated whole-assembly loss.
///
/// A refusal message is the reader's *output*, so matching on it alone excuses
/// every future input that happens to produce the same words — which is how a
/// genuinely new loss slips through a gate like this. So an exemption whose
/// justification is a claim about *which files* must also say which files, and
/// only excuses losses there.
///
/// Note the direction. Narrowing an *exemption* can only turn a pass into a
/// failure, so a wrong path predicate here is loud; narrowing the *population*
/// (the compile-asset filter this sweep used to have) turns a failure into a
/// pass, so a wrong predicate there is silent. That asymmetry is why path
/// matching is right in this position and wrong in that one.
struct Exemption {
    /// Substring of the refusal this excuses.
    reason: &'static str,
    /// Lowercased path substring the excused files must sit under. `None` only
    /// when the justification is a property of the *bytes*, independent of
    /// which package shipped them.
    scope: Option<&'static str>,
    why: &'static str,
}

/// Whole-assembly losses this gate tolerates. A cause reaches this list only by
/// being *justified*, never by being common — an entry is a standing hole in
/// the gate, so keep it short and keep the justification honest.
const EXEMPT: &[Exemption] = &[
    // A PE with no CLI header is not a managed assembly at all — a native DLL
    // (`msdia140.dll`, `git2-*.dll`) or a native EXE. Unscoped, and this is the
    // one entry where that is right: the justification is a property of the
    // bytes, which any package may ship, and the refusal *is* that property.
    // Cross-checking it would mean a second PE reader, which is not worth
    // building to re-derive "the COM descriptor directory is empty".
    Exemption {
        reason: "no CLI header",
        scope: None,
        why: "not a managed assembly (native PE); fsc refuses it too",
    },
    // The .NET Framework targeting packs ship two shapes the reader refuses
    // whole. Both are real gaps of the same kind #199 fixed — a per-item
    // degradation propagated as a whole-image error — but they are reachable
    // only from a net4x target, so whether to close them waits on whether
    // borzoi supports net4x at all. Tracked separately; exempt, not forgotten.
    //
    // Scoped to the targeting packs, because that is what the justification
    // claims. A *non*-net4x assembly refused for either cause — say one with a
    // linked `ManifestResource` — is a new loss and must fail the gate.
    Exemption {
        reason: "assembly has no Assembly manifest record",
        scope: Some("microsoft.netframework.referenceassemblies"),
        why: "netmodule in the net4x targeting pack; pending the net4x-support decision",
    },
    Exemption {
        reason: "manifest resource is not embedded in this file",
        scope: Some("microsoft.netframework.referenceassemblies"),
        why: "net4x targeting-pack mscorlib; pending the net4x-support decision",
    },
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
    /// The exemption justifying this loss of the assembly at `path`, if any.
    /// Both halves must hold: the refusal must be one the entry names, *and*
    /// the file must sit where the entry's justification says it does.
    fn exemption(&self, path: &Path) -> Option<&'static str> {
        let Outcome::Skipped(reason) = self else {
            return None;
        };
        let lowered = path.to_string_lossy().to_lowercase();
        EXEMPT
            .iter()
            .find(|e| {
                reason.contains(e.reason) && e.scope.is_none_or(|scope| lowered.contains(scope))
            })
            .map(|e| e.why)
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

/// Sweep `BORZOI_DLL_SWEEP_ROOT` (a `PATH`-style list, so `:`-separated on
/// Unix and `;`-separated on Windows — a bare `:` split would cut a Windows
/// root at its drive colon) and **fail on any whole-assembly loss without a
/// named exemption**. Prints usage and returns green when unset.
#[test]
#[ignore = "needs a populated local package cache; run explicitly"]
fn no_assembly_is_skipped_whole_without_a_named_reason() {
    let Some(roots) = std::env::var_os("BORZOI_DLL_SWEEP_ROOT") else {
        eprintln!(
            "set BORZOI_DLL_SWEEP_ROOT to a {} list of package-cache roots \
             (e.g. ~/.nuget/packages) to run this sweep",
            if cfg!(windows) {
                "`;`-separated"
            } else {
                "`:`-separated"
            },
        );
        return;
    };
    let mut files = Vec::new();
    let mut unreadable = Vec::new();
    for root in std::env::split_paths(&roots).filter(|p| p.as_os_str() != "") {
        collect_assemblies(&root, &mut files, &mut unreadable);
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
        if let (Outcome::Skipped(reason), None) = (outcome, outcome.exemption(path)) {
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
    for (path, outcome) in results {
        if let Some(why) = outcome.exemption(path) {
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
        outcome.exemption(&not_an_assembly),
        None,
        "an unrecognised refusal must reach the gate, not be waved through"
    );

    // A **scoped** exemption excuses the loss only where its justification says
    // it does. The same refusal from anywhere else is a new loss: the reason
    // string is the reader's output, and matching on it alone would wave
    // through, say, a non-net4x assembly carrying a linked `ManifestResource`.
    let scoped = EXEMPT
        .iter()
        .find(|e| e.scope.is_some())
        .expect("a scoped exemption to exercise");
    let skipped = Outcome::Skipped(format!("parse: {}", scoped.reason));
    let inside = PathBuf::from(format!("/c/packages/{}/1.0.3/x.dll", scoped.scope.unwrap()));
    assert_eq!(
        skipped.exemption(&inside),
        Some(scoped.why),
        "the loss its justification covers is excused"
    );
    assert_eq!(
        skipped.exemption(Path::new(
            "/c/packages/unrelated.package/1.0.0/lib/net8.0/x.dll"
        )),
        None,
        "the same refusal outside the justified scope must reach the gate"
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
    for e in EXEMPT {
        assert!(
            !e.reason.is_empty() && !e.why.is_empty(),
            "{:?}: {:?}",
            e.reason,
            e.why
        );
        // A one- or two-word needle ("assembly", "not embedded") would match
        // refusals nobody vetted. The real ones are full clauses.
        assert!(
            e.reason.split_whitespace().count() >= 3,
            "exemption {:?} is too broad to have been justified",
            e.reason,
        );
        // A scope is matched against a lowercased path, so an entry with any
        // uppercase in it silently never fires — and an exemption that never
        // fires is not the hole it was written to be; the loss it was meant to
        // cover would fail the gate instead. Loud is fine, silent is not.
        if let Some(scope) = e.scope {
            assert_eq!(
                scope,
                scope.to_lowercase(),
                "scope {scope:?} must be lowercase to match"
            );
        }
    }
}

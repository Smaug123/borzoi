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
//! individually instead. An [`Exemption`] does name the *file* it excuses — but
//! see there for why that position is the safe one.
//!
//! `#[ignore]`d and env-driven: it needs a populated package cache, not a
//! fixture, so it is a local ratchet in the mould of `resolve_real_project_diff`
//! rather than a CI job.

use std::collections::{BTreeMap, BTreeSet};
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
/// failure, so a wrong predicate here is loud; narrowing the *population* (the
/// compile-asset filter this sweep used to have) turns a failure into a pass,
/// so a wrong predicate there is silent. That asymmetry is why matching on a
/// path is right in this position and wrong in that one.
struct Exemption {
    /// The step that must have refused the file.
    stage: Stage,
    /// The refusal's `Display`, matched in **full**. Not a substring: an error
    /// can carry input-controlled text (a resource name echoed back), and a
    /// substring match would let a package choose which exemption its failure
    /// lands in.
    error: &'static str,
    /// Assembly file names this excuses, spelled out (see [`is_named`]).
    /// **Empty** only when the justification is a property of the *bytes*,
    /// independent of which file carries them.
    ///
    /// The file's name rather than its package, because a name is the one part
    /// of a path that no cache layout, sweep root, or symlink can move: a
    /// directory-based scope has to decide which component is the package id
    /// (root-relative? any component?) and has to survive aliasing, and every
    /// answer to those has been a hole.
    files: &'static [&'static str],
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
        stage: Stage::Parse,
        error: "unsupported ECMA-335 layout: assembly reader: no CLI header",
        files: &[],
        why: "not a managed assembly (native PE); fsc refuses it too",
    },
    // The .NET Framework targeting packs ship two shapes the reader refuses
    // whole. Both are real gaps of the same kind #199 fixed — a per-item
    // degradation propagated as a whole-image error — but they are reachable
    // only from a net4x target, so whether to close them waits on whether
    // borzoi supports net4x at all. Tracked separately; exempt, not forgotten.
    //
    // Scoped to the two files, because that is what the justification claims.
    // Some *other* assembly refused for either cause — say one with a linked
    // `ManifestResource` — is a new loss and must fail the gate.
    Exemption {
        stage: Stage::Parse,
        error: "unsupported ECMA-335 layout: assembly has no Assembly manifest record",
        files: &["System.EnterpriseServices.Wrapper.dll"],
        why: "netmodule in the net4x targeting pack; pending the net4x-support decision",
    },
    Exemption {
        stage: Stage::Parse,
        error: "unsupported ECMA-335 layout: assembly reader: manifest resource is not embedded \
                in this file",
        files: &["mscorlib.dll"],
        why: "net4x targeting-pack mscorlib; pending the net4x-support decision",
    },
];

/// A population smaller than this is not a package cache — it is a mistyped
/// root, or one whose subtrees are unreadable. The gate says nothing useful
/// about such a population, so it fails rather than passing vacuously.
const MIN_PROJECTED: usize = 1_000;

/// Which step of the pipeline refused the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Read,
    Parse,
    Enumerate,
}

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
    /// Skipped whole. `error` is the refusal's `Display` *alone* — not a
    /// message the sweep composed around it — because [`Outcome::exemption`]
    /// compares it for equality. A `format!("{stage}: {e}")` blob matched by
    /// substring lets input-controlled text inside the error (a resource name
    /// the reader echoes back, say) select an exemption written for something
    /// else.
    Skipped { stage: Stage, error: String },
}

impl Outcome {
    /// The exemption justifying this loss of the assembly at `path`, if any.
    /// All three halves must hold: same stage, *exactly* the same error, and a
    /// path where the entry's justification says it applies.
    fn exemption(&self, path: &Path) -> Option<&'static str> {
        let Outcome::Skipped { stage, error } = self else {
            return None;
        };
        EXEMPT
            .iter()
            .find(|e| {
                e.stage == *stage
                    && e.error == error
                    && (e.files.is_empty() || e.files.iter().any(|f| is_named(path, f)))
            })
            .map(|e| e.why)
    }

    /// How a gate failure renders this loss.
    fn describe(&self) -> String {
        match self {
            Outcome::Projected { .. } => "projected".to_owned(),
            Outcome::Skipped { stage, error } => format!("{stage:?}: {error}"),
        }
    }
}

/// Whether `path` names an assembly file called `file` (case-insensitively).
///
/// The file's own name, not a directory anywhere above it. A directory-based
/// scope has to answer "which of these components is the package id?", which
/// depends on the sweep root, and it has to survive symlinks, which rewrite the
/// components above the file but never the file's own name.
fn is_named(path: &Path, file: &str) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(file))
}

/// Run the LSP's pipeline over one assembly file.
fn probe_assembly(path: &Path) -> Outcome {
    let skipped = |stage, error: String| Outcome::Skipped { stage, error };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => return skipped(Stage::Read, e.to_string()),
    };
    let parsed = match catch_unwind_silent(|| Ecma335Assembly::parse(&bytes)) {
        Err(_) => return skipped(Stage::Parse, "reader panicked".to_owned()),
        Ok(Err(e)) => return skipped(Stage::Parse, e.to_string()),
        Ok(Ok(view)) => view,
    };
    match catch_unwind_silent(|| parsed.enumerate_type_defs_with_skips()) {
        Err(_) => skipped(Stage::Enumerate, "reader panicked".to_owned()),
        Ok(Err(e)) => skipped(Stage::Enumerate, e.to_string()),
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

/// What a traversal found: the assemblies to probe, and every place it could
/// *not* look. The second list is the point — a gate that cannot tell "nothing
/// wrong here" from "did not look here" is not a gate, so anything unexamined
/// is carried out and fails the sweep rather than being absorbed as an empty
/// subtree.
#[derive(Default)]
struct Found {
    /// Keyed by canonical path, so an assembly reached twice — through
    /// overlapping roots, or through a symlink — is probed and counted once;
    /// duplicates would otherwise inflate the population against
    /// `MIN_PROJECTED`. The value is **every** logical path it was reached by.
    ///
    /// All of them, not the first: an [`Exemption`] is matched against the
    /// logical path, so keeping one alias would let traversal order decide
    /// whether a loss is excused. The bytes are probed once and the verdict
    /// must hold for every name the cache exposes them under.
    assemblies: BTreeMap<PathBuf, Vec<PathBuf>>,
    unexamined: Vec<(PathBuf, String)>,
}

/// Collect every assembly file under `root`.
///
/// Classification goes through [`std::fs::metadata`], which **follows**
/// symlinks: `DirEntry::file_type` reports the link itself, so a cache built
/// from linked package versions or shared storage would have every linked entry
/// silently match neither the file nor the directory arm. Cycles are bounded by
/// `visited`, which holds canonical directory paths.
fn collect_assemblies(root: &Path, found: &mut Found, visited: &mut BTreeSet<PathBuf>) {
    match root.canonicalize() {
        Ok(canonical) => {
            if !visited.insert(canonical) {
                return;
            }
        }
        Err(e) => {
            found.unexamined.push((root.to_path_buf(), e.to_string()));
            return;
        }
    }
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            found.unexamined.push((root.to_path_buf(), e.to_string()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                found.unexamined.push((root.to_path_buf(), e.to_string()));
                continue;
            }
        };
        let path = entry.path();
        // A broken symlink resolves to nothing; that is unexamined, not absent.
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                found.unexamined.push((path, e.to_string()));
                continue;
            }
        };
        if meta.is_dir() {
            collect_assemblies(&path, found, visited);
        } else if meta.is_file() && is_assembly_file(&path) {
            match path.canonicalize() {
                Ok(canonical) => {
                    found.assemblies.entry(canonical).or_default().push(path);
                }
                Err(e) => found.unexamined.push((path, e.to_string())),
            }
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
    let mut found = Found::default();
    let mut visited = BTreeSet::new();
    for root in std::env::split_paths(&roots).filter(|p| p.as_os_str() != "") {
        collect_assemblies(&root, &mut found, &mut visited);
    }
    assert!(
        found.unexamined.is_empty(),
        "{} path(s) under {roots:?} could not be examined, so the sweep cannot claim anything \
         about the assemblies there:\n{}",
        found.unexamined.len(),
        found
            .unexamined
            .iter()
            .map(|(p, e)| format!("  {}: {e}", p.display()))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    // One entry per distinct assembly, carrying every name the cache exposes it
    // under. The bytes are probed once (through the first alias — they are the
    // same bytes) and the verdict must hold for all of them.
    let aliases: Vec<Vec<PathBuf>> = found.assemblies.into_values().collect();
    let files: Vec<PathBuf> = aliases.iter().map(|names| names[0].clone()).collect();
    eprintln!("[skip-sweep] {} assemblies under {roots:?}", files.len());

    let results = probe_all(&files);
    // Conservation: every assembly discovered has exactly one outcome. Without
    // this, a lost worker chunk is indistinguishable from a clean sweep of
    // fewer files, and the gate below would pass over assemblies it never saw.
    assert_eq!(
        results.len(),
        files.len(),
        "{} assemblies were discovered but {} outcomes came back — some were never probed",
        files.len(),
        results.len(),
    );
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

    // The gate. Grouped by cause so one reader gap reads as one line rather
    // than as its hundred affected package versions — which is how #199's
    // single root cause presented, as 77 separately skipped files.
    // Judged against *every* alias, and excused only if every one is excused:
    // an exemption keyed on the file's name must not depend on which of an
    // assembly's names the traversal happened to reach first.
    let mut by_cause: BTreeMap<String, Vec<&Path>> = BTreeMap::new();
    for (names, (_, outcome)) in aliases.iter().zip(&results) {
        if !matches!(outcome, Outcome::Skipped { .. }) {
            continue;
        }
        for name in names {
            if outcome.exemption(name).is_none() {
                by_cause
                    .entry(outcome.describe())
                    .or_default()
                    .push(name.as_path());
            }
        }
    }
    assert!(
        by_cause.is_empty(),
        "{} assembly file(s) were skipped whole for a cause with no named exemption — a project \
         referencing one loses every type in it. Either fix the reader, or add the cause to \
         `EXEMPT` with a justification:\n{}",
        by_cause.values().map(Vec::len).sum::<usize>(),
        by_cause
            .iter()
            .map(|(cause, paths)| format!(
                "  {} × {cause}\n    e.g. {}",
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
        // A worker that panics outside the reader's own three catches would
        // otherwise take its whole chunk's files with it, silently. Resume the
        // panic instead: the caller's conservation check would catch the
        // shortfall anyway, but the panic says which worker died.
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
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
            Outcome::Skipped { .. } => None,
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
    let Outcome::Skipped { error, .. } = &outcome else {
        panic!("garbage bytes must be refused, got {outcome:?}");
    };
    // Garbage that is not even a PE is refused before the CLI-header check, so
    // it is *not* exempt — the exemption is narrow, as intended.
    assert!(!error.is_empty(), "the refusal names a cause");
    assert_eq!(
        outcome.exemption(&not_an_assembly),
        None,
        "an unrecognised refusal must reach the gate, not be waved through"
    );

    // A **scoped** exemption excuses the loss only for the file its
    // justification names. The same refusal from any other assembly is a new
    // loss: the error is the reader's output, and matching on it alone would
    // wave through, say, an arbitrary assembly carrying a linked
    // `ManifestResource`.
    let scoped = EXEMPT
        .iter()
        .find(|e| !e.files.is_empty())
        .expect("a scoped exemption to exercise");
    let skipped = Outcome::Skipped {
        stage: scoped.stage,
        error: scoped.error.to_owned(),
    };
    let scoped_file = scoped.files[0];
    assert_eq!(
        skipped.exemption(Path::new(&format!("/c/packages/p/1.0.3/{scoped_file}"))),
        Some(scoped.why),
        "the loss its justification covers is excused"
    );
    // Anywhere at all, since the name is the whole scope — a cache root, a
    // symlinked package, a layout nobody anticipated. That invariance is why
    // the name is the scope.
    assert_eq!(
        skipped.exemption(Path::new(&format!("/elsewhere/{scoped_file}"))),
        Some(scoped.why),
        "the same file under a different layout is the same file"
    );
    for near_miss in [
        "Other.dll".to_owned(),
        format!("prefix-{scoped_file}"),
        format!("{scoped_file}.bak"),
    ] {
        assert_eq!(
            skipped.exemption(Path::new(&format!("/c/packages/p/1.0.0/{near_miss}"))),
            None,
            "{near_miss} is not the file the exemption names"
        );
    }

    // An exemption is matched in FULL, so an error that merely *contains* one
    // is not excused. Otherwise a package could pick its exemption by naming a
    // resource after it — the reader echoes such names back into the message.
    let unscoped = EXEMPT
        .iter()
        .find(|e| e.files.is_empty())
        .expect("an unscoped exemption to exercise");
    assert_eq!(
        Outcome::Skipped {
            stage: unscoped.stage,
            error: format!(
                "unknown FSharp* resource name: FSharpSignature-{}",
                unscoped.error
            ),
        }
        .exemption(Path::new("/c/packages/some.pkg/1.0.0/lib/net8.0/x.dll")),
        None,
        "an error that merely embeds an exempt one must reach the gate",
    );
    // …and the stage is part of the match, so the same words from a different
    // step are a different failure.
    let elsewhere = match unscoped.stage {
        Stage::Parse => Stage::Enumerate,
        Stage::Read | Stage::Enumerate => Stage::Parse,
    };
    assert_eq!(
        Outcome::Skipped {
            stage: elsewhere,
            error: unscoped.error.to_owned(),
        }
        .exemption(Path::new("/c/packages/some.pkg/1.0.0/lib/net8.0/x.dll")),
        None,
        "the same words from another stage are a different failure",
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

    // A root that cannot be examined is recorded, not read as an empty subtree.
    let mut found = Found::default();
    collect_assemblies(
        &dir.path().join("no-such-dir"),
        &mut found,
        &mut BTreeSet::new(),
    );
    assert!(found.assemblies.is_empty());
    assert_eq!(found.unexamined.len(), 1, "the missing subtree is reported");
}

/// Traversal conserves: an assembly reachable twice is probed once, and one
/// reachable only through a symlink is still reached. Both are ways a real
/// package cache can make the population differ from what the gate assumes.
#[test]
fn traversal_follows_links_and_counts_each_assembly_once() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let pkg = dir.path().join("pkg/lib/net8.0");
    std::fs::create_dir_all(&pkg).expect("package dir");
    std::fs::write(pkg.join("Real.dll"), b"not a PE").expect("write");

    // Overlapping roots: the same tree twice, and a descendant of it. Every
    // duplicate would otherwise count towards `MIN_PROJECTED`, so a small
    // population could defeat the small-population guard by repetition.
    let mut found = Found::default();
    let mut visited = BTreeSet::new();
    for root in [dir.path(), dir.path(), pkg.as_path()] {
        collect_assemblies(root, &mut found, &mut visited);
    }
    assert!(found.unexamined.is_empty(), "{:?}", found.unexamined);
    assert_eq!(
        found.assemblies.len(),
        1,
        "one assembly reached three ways is one assembly: {:?}",
        found.assemblies
    );

    #[cfg(unix)]
    {
        // A symlinked package directory is followed. `DirEntry::file_type`
        // reports the *link*, so classifying on it would match neither the file
        // nor the directory arm and drop the subtree without a word.
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&pkg, &linked).expect("symlink");
        let mut found = Found::default();
        collect_assemblies(&linked, &mut found, &mut BTreeSet::new());
        assert!(found.unexamined.is_empty(), "{:?}", found.unexamined);
        assert_eq!(
            found.assemblies.len(),
            1,
            "the assembly behind the link is found: {:?}",
            found.assemblies
        );
        // …and it is remembered by the path it was *found under*, not the link
        // target: the exemption is matched against the logical name, which
        // canonicalisation can rewrite.
        let (canonical, logical) = found.assemblies.iter().next().expect("the one assembly");
        assert!(
            logical.iter().all(|p| p.starts_with(&linked)),
            "the logical paths go through the link: {logical:?}"
        );
        assert!(
            !canonical.starts_with(&linked),
            "the dedup key is the resolved target: {}",
            canonical.display()
        );

        // Two links to the same file keep **both** names. Dropping one would
        // let traversal order decide whether a name-scoped exemption fires.
        let second = dir.path().join("second-alias.dll");
        std::os::unix::fs::symlink(pkg.join("Real.dll"), &second).expect("symlink");
        let mut found = Found::default();
        let mut visited = BTreeSet::new();
        collect_assemblies(dir.path(), &mut found, &mut visited);
        let (_, names) = found
            .assemblies
            .iter()
            .find(|(_, names)| names.len() > 1)
            .expect("the aliased assembly keeps every name it was reached by");
        assert!(
            names.iter().any(|n| n == &second),
            "the second alias survives deduplication: {names:?}"
        );
        std::fs::remove_file(&second).expect("clean up");

        // A broken link is unexamined, not absent.
        let broken = dir.path().join("pkg/lib/net8.0/Dangling.dll");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &broken).expect("symlink");
        let mut found = Found::default();
        collect_assemblies(&pkg, &mut found, &mut BTreeSet::new());
        assert_eq!(
            found.unexamined.len(),
            1,
            "the dangling link is reported: {:?}",
            found.unexamined
        );
    }
}

/// Each exemption must be able to fire, and must name a whole refusal rather
/// than a phrase broad enough to wave through causes nobody vetted.
#[test]
fn every_exemption_is_justified_and_can_fire() {
    for e in EXEMPT {
        assert!(
            !e.error.is_empty() && !e.why.is_empty(),
            "{:?}: {:?}",
            e.error,
            e.why
        );
        // A one- or two-word error ("assembly", "not embedded") is not a whole
        // refusal, so it was probably meant as a substring — which this no
        // longer is.
        assert!(
            e.error.split_whitespace().count() >= 3,
            "exemption {:?} does not look like a whole refusal",
            e.error,
        );
        // A scope is one path component, so an id written with a separator in
        // it could never equal one and the exemption would sit inert — and an
        // exemption that never fires is not the hole it was written to be; the
        // loss it was meant to cover would fail the gate instead.
        for file in e.files {
            assert_eq!(
                Path::new(file).components().count(),
                1,
                "{file:?} must be a bare file name, not a path"
            );
            assert!(
                is_assembly_file(Path::new(file)),
                "{file:?} must be an assembly the sweep would actually probe"
            );
        }
        // Round-trip: the entry must actually excuse the loss it describes. An
        // `error` that does not match the reader's wording verbatim — a typo, a
        // message reworded upstream — would otherwise sit here doing nothing
        // until the gate failed on the loss it was supposed to cover.
        let path = PathBuf::from(format!(
            "/c/packages/p/1.0.0/lib/net8.0/{}",
            e.files.first().unwrap_or(&"any.dll")
        ));
        assert_eq!(
            Outcome::Skipped {
                stage: e.stage,
                error: e.error.to_owned(),
            }
            .exemption(&path),
            Some(e.why),
            "exemption {:?} does not excuse its own loss",
            e.error,
        );
    }
}

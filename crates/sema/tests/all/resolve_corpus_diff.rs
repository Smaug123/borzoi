//! Differential name-resolution sweep: our resolver vs FCS over the whole
//! corpus — the resolution analogue of `cst`'s `parser_corpus_diff.rs`.
//!
//! [`resolve_diff.rs`](crate) asserts the *strict* Stage C property (every
//! in-file use must resolve to the right binder) over a curated, fully-modeled
//! corpus. This sweep runs over real, partly-unmodeled F#: for every symbol use
//! FCS resolves whose declaration is in the same file and which is **lexical**
//! (bucket B1 — no type inference), we compare our resolution and bucket the
//! outcome into four classes:
//!
//! * **match** — our `resolution_at(range)` is a `Local`/`Item` pointing at a
//!   binder whose range *equals* FCS's declaration range. The headline coverage.
//! * **divergence** — we return `Unresolved`, resolve into an assembly
//!   `Entity`/`Member`, or point at a binder whose *name* differs from the use,
//!   all where FCS found an in-file binder. These are unambiguous soundness
//!   faults (D5: never `Unresolved`/out-of-file where resolvable), gated to zero
//!   by an assertion of their own; sites printed.
//! * **alt-binder** — we point at a *same-named* in-file binder at a *different*
//!   range than FCS. In a file FCS's isolated check reports errors for, this is
//!   dominated by isolation-bias recovery: checked alone, a pattern like
//!   `SynPat.Paren(p, _)` on an unresolved sibling type makes FCS *not* bind the
//!   inner `p` (so a body use falls back to an enclosing same-named binder),
//!   while our purely-lexical resolver binds it. In a file FCS checks *cleanly*
//!   there is no recovery to blame, so an alt-binder there is a wrong answer;
//!   each entry says which kind of file it is in (`fcs-clean` /
//!   `fcs-check-errors`). (Strict shadowing correctness is covered FCS-free by
//!   `resolve_scoping.rs` and exactly by `resolve_diff.rs`.)
//! * **gap** — we honestly return `Deferred`, or recorded nothing at that range
//!   (a construct we don't model yet, or a long-ident whose occurrence range we
//!   key differently). The categorised worklist behind them — what these gaps
//!   are, by construct — is the sibling report generator `resolve_divergence.rs`
//!   (its `gap_b1.txt`).
//!
//! # Uses FCS resolves outside the file
//!
//! Every other use FCS resolves — in **any** bucket — names a symbol declared
//! outside the file: FSharp.Core, the BCL. The sweep resolves under **exactly
//! the reference set FCS checked the file against** (each census line reports
//! it), so those are graded as the in-file ones are, by what we committed to:
//!
//! * an **assembly-target** use is one whose symbol FCS identifies as declared
//!   in one of those referenced assemblies. Committing to an assembly
//!   `Entity`/`Member`/`UnionCase` that FCS's identity names
//!   ([`assembly_answer_agrees`], the comparator the whole-project gate uses
//!   too) is a **match**; declining is an **assembly-gap**; any other answer —
//!   a different assembly declaration, an in-file binder (a local that leaked
//!   out of its scope and captured `string`), `Unresolved` — is a
//!   **divergence**, gated by the same assertion;
//! * an **external** use is one FCS declares in another file but names by no
//!   identity in a referenced assembly, or a namespace, which no one assembly
//!   declares (FCS names whichever it met first) and no answer of ours names.
//!   There is nothing to compare a committed answer with, so the only correct
//!   outcome is to decline, and any commitment is a divergence.
//!
//! Three kinds of use are not graded, and are counted on the summary line
//! instead:
//!
//! * one whose symbol FCS gave **no** declaration location and no identity in
//!   a referenced assembly (unsolved trait calls, and a default struct
//!   constructor, which can be in-file and is named by the file's own
//!   assembly). That is no evidence about where the target lives;
//! * one at a range where FCS *also* records an in-file symbol, as a use or a
//!   definition: a type name and its constructor, or the
//!   `OptionalArgumentAttribute` FCS synthesises at a member whose parameter is
//!   `?optional`. The in-file record is the one to grade there;
//! * one at a range where FCS records two **different** out-of-file answers,
//!   which no single answer of ours could agree with. A constructor record
//!   that ends with a record of its type's name (`StringBuilder()`, and the
//!   `.ctor` FCS records one character into `[<Obsolete>]`) answers the same
//!   site at a level the resolver does not model, and steps aside rather than
//!   making the site ambiguous.
//!
//! The other direction — FCS resolves in-file, we commit to an assembly
//! `Entity`/`Member` — is graded in the in-file slice (a divergence).
//!
//! # The manifest
//!
//! The corpus is pinned by the flake and both sides are deterministic, so every
//! outcome is a fixed fact, and the sweep checks it **exactly** against
//! `tests/manifests/resolve_corpus_diff.txt` rather than through count bounds.
//! Every line is `<key><TAB><value>` ([`Manifest::from_keyed`]), so independent
//! changes merge key by key (`tools/manifest-merge.sh`):
//!
//! - `<file>` → whether it was compared or skipped, and why; for a compared
//!   file, whether FCS's check errored, and three counts of the oracle's
//!   records: `b1=` its in-file B1 uses, `assembly=` its assembly-target uses
//!   and `external=` its graded external uses.
//! - `<file>:<line>:<col> "<text>"` → every in-file B1 use that is not a match
//!   (gap, alt-binder, divergence), every assembly-target use that is not a
//!   match (assembly-gap, divergence), and every external use we did not
//!   decline (a divergence).
//! - `<file>:<line>:<col> "<name>" binder` → every binder FCS declares that the
//!   walk neither interns nor declines, with its class and construct
//!   (`borzoi_sema::test_support::binder_completeness`): binder completeness's
//!   worklist. A use FCS binds to one of them that we commit is a divergence
//!   (`… behind-binder`), whatever bucket the use is in.
//!
//! No line counts our own verdicts: such a count changes with every item it
//! sums, so any two changes to one file's uses would conflict on it. The counts
//! are of what FCS reported, which moves only when the corpus or the oracle
//! does. So a file's in-file matches are its `b1` less its listed in-file uses,
//! its assembly matches its `assembly` less its listed assembly-target ones,
//! and its declined external uses its `external` less its listed external
//! ones, all left unlisted. The sweep checks, file by file, that every counted
//! record came to exactly one verdict, so the subtraction holds.
//!
//! Any difference fails with a line diff. A movement in either direction is
//! signal: a lost match is a regression, a gained one an improvement, and both
//! are acknowledged by regenerating the manifest and committing the diff:
//!
//! ```text
//! BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-sema --test all resolve_corpus_diff:: -- --ignored
//! ```
//!
//! Regeneration cannot bless a divergence: that assertion runs first.
//!
//! Of the uses FCS resolves in-file, only the **B1 lexical** slice is checked:
//! B2/B3 uses (`x.Length`, overloaded members) need inference we do not do, so
//! they are skipped — not divergences. (Out-of-file uses are graded in every
//! bucket: whatever machinery FCS needed, a committed answer must be its
//! answer, and declining makes no claim.)
//! FCS type-checks each file *in isolation* (`uses-census-batch`). Our resolver
//! runs single-file with empty `ProjectItems`, under an `AssemblyEnv` read from
//! the reference set the census reports. Only the resolver is graded: an answer
//! inference layers over a deferral (a member found once the receiver's type
//! is known) is the whole-project gate's to grade.
//!
//! `#[ignore]`d like the parser sweep: it type-checks a corpus sample (slow).
//! Run under `nix develop` (which sets `BORZOI_CORPUS`):
//!
//! ```text
//! cargo test -p borzoi-sema --test all resolve_corpus_diff:: -- --ignored --nocapture
//! ```
//!
//! Tune the sample with `BORZOI_RESOLVE_DIFF_STRIDE` (default 13 — every 13th
//! `.fs` file) and `BORZOI_RESOLVE_DIFF_LIMIT`. The manifest describes the
//! default sample, so a run with either set checks only the divergence gate.

use borzoi_oracle_harness::manifest::Manifest;
use borzoi_oracle_harness::panic_silence::silence_panics_here;

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use crate::common::corpus_manifest::{
    Positions, check_manifest, corpus_relative, regenerate_ignored,
};
use crate::common::{
    Bucket, CensusDecl, FileCensus, ResolveDiffUse, census_oracle_binders, census_oracle_uses,
    census_resolve_uses, env_usize_or, invoke_fcs_dump_census, parse_census_jsonl,
};
use borzoi_assembly::{Ecma335Assembly, EcmaView};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::TextRange;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::test_support::binder_completeness::{
    commits_behind_unmodelled_binders, unmodelled_binders,
};
use borzoi_sema::test_support::{
    AssemblyDecl, assembly_answer_agrees, assembly_resolution_decl, canonical_assembly,
};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, resolve_file};
use std::collections::{BTreeSet, HashMap};

/// The sample the checked-in manifest describes: every `DEFAULT_STRIDE`th file.
const DEFAULT_STRIDE: usize = 13;

/// How many sites of each kind to print for investigation.
const SAMPLE: usize = 40;

/// One disagreement between our resolution and FCS, for the printed sample.
struct Site {
    path: PathBuf,
    range: TextRange,
    text: String,
    /// FCS's declaration for this use: the in-file binder's range we *should*
    /// point at, the assembly declaration it names, or that it lies outside
    /// the file.
    expected: String,
    /// What we said (FCS resolved an in-file binder here).
    ours: String,
    /// Whether FCS's isolated check of the file reported an error.
    fcs_check_errors: bool,
}

#[derive(Default)]
struct Tally {
    /// Files FCS type-checked Ok and we parsed cleanly (the comparable set).
    files_compared: usize,
    matches: usize,
    /// Unambiguous faults (gated to zero): `Unresolved`, assembly entity, or a
    /// wrong-*named* binder where FCS found an in-file binder; any in-file
    /// answer where FCS's target is outside the file.
    divergences: Vec<Site>,
    /// Same-named in-file binder at a different range.
    alt_binders: Vec<Site>,
    /// In-file B1 uses we left `Deferred` or recorded nothing at — modeling
    /// gaps, not bugs.
    gaps: usize,
    /// Uses FCS identified as declared in a referenced assembly, every bucket,
    /// each graded against our answer by identity.
    assembly_graded: usize,
    /// Assembly-target uses our answer named exactly.
    assembly_matches: usize,
    /// Assembly-target uses we declined.
    assembly_gaps: usize,
    /// Uses FCS declared in another file but named by no identity in a
    /// referenced assembly, every bucket, each checked to be one we did not
    /// commit an answer for.
    external_graded: usize,
    /// Out-of-file uses at a range where FCS also records an in-file symbol,
    /// left to the in-file slice.
    external_shared_range: usize,
    /// Out-of-file ranges where FCS records two different answers.
    external_ambiguous: usize,
    /// Uses whose symbol FCS gave no declaration location and no identity in a
    /// referenced assembly: no claim to grade.
    external_absent: usize,
    /// FCS reported the file as not Ok (its check aborted or threw): skipped.
    fcs_not_ok: usize,
    /// Our parse produced errors, so its resolution isn't meaningful to diff.
    our_errors: usize,
    /// Our parse or resolve panicked (a construct not modeled yet).
    our_skipped: usize,
    unreadable: usize,
    /// One `(key, value)` per sampled file and per non-match use (see the
    /// module docs).
    manifest: Vec<(String, String)>,
    /// Binders FCS declares that the walk neither interns nor declines
    /// (`borzoi_sema::test_support::binder_completeness`), each also a manifest
    /// line.
    unmodelled_binders: usize,
    /// Compared files whose verdicts do not account for every oracle record
    /// the file's line counts, so its unlisted matches cannot be derived.
    unaccounted: Vec<String>,
}

#[test]
#[ignore = "full-corpus differential resolution (us + FCS type-check); run with --ignored under nix develop"]
fn resolution_matches_fcs_over_corpus() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!(
            "BORZOI_CORPUS unset; skipping resolution sweep. Run under \
             `nix develop`, or point it at an F# checkout."
        );
        return;
    };
    let root = PathBuf::from(root);
    let stride = env_usize_or("BORZOI_RESOLVE_DIFF_STRIDE", DEFAULT_STRIDE).max(1);
    let limit = env_usize_or("BORZOI_RESOLVE_DIFF_LIMIT", usize::MAX);

    let mut all_files = Vec::new();
    collect_fs(&root, &mut all_files);
    crate::common::corpus_manifest::sort_by_corpus_key(&root, &mut all_files);
    let sample: Vec<PathBuf> = all_files
        .iter()
        .step_by(stride)
        .take(limit)
        .cloned()
        .collect();
    assert!(!sample.is_empty(), "no .fs files under {root:?}");
    eprintln!(
        "resolve-diff: {} of {} .fs files (stride {stride}); type-checking each in isolation…",
        sample.len(),
        all_files.len()
    );

    let census: Vec<FileCensus> = parse_census_jsonl(&invoke_fcs_dump_census(&sample));
    assert_eq!(
        census.len(),
        sample.len(),
        "one census line per sampled file"
    );

    let references = OracleReferences::of(&census);
    eprintln!(
        "resolve-diff: resolving under the {} assemblies the census checked against",
        references.paths.len()
    );

    let mut tally = Tally::default();
    {
        // Silence the per-panic backtraces from our parser/resolver on unmodeled
        // constructs (we count outcomes ourselves) — per-thread, so a concurrent
        // test's genuine panic still prints (see `panic_silence`).
        //
        // Scoped to the loop, and *not* held across the assertions below: those
        // are the point of the test, and a failing one must keep its payload and
        // backtrace.
        let _silence = silence_panics_here();

        for file in &census {
            compare_file(&root, file, &references, &mut tally);
        }
    }

    let b1_seen = tally.matches + tally.gaps;
    eprintln!(
        "resolve-diff: {} files compared | {} match | {} diverge | {} alt-binder \
         ({} in files FCS checked cleanly) | {} gaps | in-file B1 coverage {}‰ | \
         {} fcs-not-ok | {} our-errors | {} our-skipped | {} unreadable | \
         {} unmodelled binders",
        tally.files_compared,
        tally.matches,
        tally.divergences.len(),
        tally.alt_binders.len(),
        tally
            .alt_binders
            .iter()
            .filter(|s| !s.fcs_check_errors)
            .count(),
        tally.gaps,
        (tally.matches * 1000).checked_div(b1_seen).unwrap_or(0),
        tally.fcs_not_ok,
        tally.our_errors,
        tally.our_skipped,
        tally.unreadable,
        tally.unmodelled_binders,
    );
    eprintln!(
        "resolve-diff: {} assembly-target uses graded ({} match | {} gaps) | {} \
         external uses graded | {} left to a same-range in-file record | {} \
         ambiguous ranges | {} with no FCS declaration",
        tally.assembly_graded,
        tally.assembly_matches,
        tally.assembly_gaps,
        tally.external_graded,
        tally.external_shared_range,
        tally.external_ambiguous,
        tally.external_absent,
    );

    print_sites("divergences (gated faults)", &tally.divergences);
    print_sites(
        "alt-binders (same name, different range)",
        &tally.alt_binders,
    );

    assert!(
        tally.unaccounted.is_empty(),
        "the sweep's verdicts do not account for the oracle records it counts, \
         so the manifest's unlisted matches would be wrong:\n{}",
        tally.unaccounted.join("\n")
    );

    // The soundness gate: wrong answers, which no manifest regeneration may
    // bless.
    assert!(
        tally.divergences.is_empty(),
        "{} uses are unambiguous faults: `Unresolved`, an assembly entity, or a \
         differently-named binder where FCS found an in-file binder; any answer \
         but the declaration FCS names where its target is in a referenced \
         assembly; or any answer where FCS's target is outside the file and \
         named by nothing we can compare. A resolver bug or soundness violation \
         regressed in.",
        tally.divergences.len(),
    );

    if stride != DEFAULT_STRIDE || limit != usize::MAX {
        eprintln!(
            "resolve-diff: NOT comparing the manifest — it describes the default \
             sample (stride {DEFAULT_STRIDE}, no limit), and this run sampled \
             stride {stride}, limit {limit}."
        );
        return;
    }
    let manifest =
        Manifest::from_keyed(tally.manifest).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check_manifest(
        "resolve_corpus_diff",
        &manifest,
        &regenerate_ignored("resolve_corpus_diff"),
    );
}

/// The reference set the census checked every file against, and the env our
/// resolver reads from exactly that set — so neither side can see a declaration
/// the other cannot.
struct OracleReferences {
    paths: Vec<String>,
    env: AssemblyEnv,
    /// The simple names of the assemblies in [`Self::env`], up to
    /// [`canonical_assembly`]: what decides whether FCS's identity for a use
    /// names a referenced assembly, rather than the checked file's own.
    names: HashSet<String>,
}

impl OracleReferences {
    /// Read the reference set off the census, asserting every file that was
    /// checked reports the same one: the env is built once, so a file checked
    /// against a different set would be graded against the wrong one.
    fn of(census: &[FileCensus]) -> Self {
        let sets: BTreeSet<&Vec<String>> = census
            .iter()
            .filter(|f| f.ok)
            .map(|f| {
                f.references.as_ref().unwrap_or_else(|| {
                    panic!(
                        "census line for {} reports no reference set; is fcs-dump stale?",
                        f.path
                    )
                })
            })
            .collect();
        assert_eq!(
            sets.len(),
            1,
            "every checked file must report the same reference set, got {} distinct ones",
            sets.len()
        );
        let paths = (*sets.first().unwrap()).clone();
        assert!(
            !paths.is_empty(),
            "the census checked files against no references"
        );
        let bytes: Vec<Vec<u8>> = paths
            .iter()
            .map(|p| std::fs::read(p).unwrap_or_else(|e| panic!("read reference {p}: {e}")))
            .collect();
        let views: Vec<Ecma335Assembly> = bytes
            .iter()
            .zip(&paths)
            .map(|(b, p)| {
                Ecma335Assembly::parse(b).unwrap_or_else(|e| panic!("parse reference {p}: {e:?}"))
            })
            .collect();
        let names = views
            .iter()
            .map(|v| canonical_assembly(&v.identity().name).to_string())
            .collect();
        let env = AssemblyEnv::from_views(&views)
            .unwrap_or_else(|e| panic!("build the census reference env: {e:?}"));
        Self { paths, env, names }
    }

    /// Whether `decl` names a declaration in one of the referenced assemblies.
    fn declares(&self, decl: &AssemblyDecl) -> bool {
        self.names.contains(canonical_assembly(&decl.assembly))
    }
}

/// What FCS answers at one out-of-file range, for grading.
#[derive(Clone, PartialEq, Eq)]
enum OutOfFile {
    /// A declaration in a referenced assembly, by identity.
    Assembly(AssemblyDecl),
    /// Declared in another file, named by no identity we can compare.
    External,
}

/// Print up to [`SAMPLE`] sites of one kind for triage.
fn print_sites(label: &str, sites: &[Site]) {
    if sites.is_empty() {
        return;
    }
    eprintln!(
        "\nresolution {label} ({}, showing up to {SAMPLE}):",
        sites.len()
    );
    for s in sites.iter().take(SAMPLE) {
        eprintln!(
            "  {}:{:?} {:?} -> FCS decl {}, we gave {}{}",
            s.path.display(),
            s.range,
            s.text,
            s.expected,
            s.ours,
            if s.fcs_check_errors {
                " [FCS check errored]"
            } else {
                ""
            },
        );
    }
}

/// Compare one census file's in-file B1 uses and out-of-file uses against our
/// resolution, folding the outcome into `tally`.
fn compare_file(root: &Path, file: &FileCensus, references: &OracleReferences, tally: &mut Tally) {
    let path = PathBuf::from(&file.path);
    let rel = corpus_relative(root, &path);
    if !file.ok {
        tally.fcs_not_ok += 1;
        tally.manifest.push((rel, "fcs-not-ok".to_string()));
        return;
    }
    let Ok(source) = std::fs::read_to_string(&path) else {
        tally.unreadable += 1;
        tally.manifest.push((rel, "unreadable".to_string()));
        return;
    };

    // Our parser/resolver panics on a few unmodeled constructs; catch so one
    // file can't abort the sweep. Parse and resolve together — both can panic.
    let resolved = catch_unwind(AssertUnwindSafe(|| {
        let parsed = parse(&source);
        if !parsed.errors.is_empty() {
            return None; // signal "our errors" without panicking
        }
        let recovery = SyntaxRecovery::of(&parsed);
        let root = parsed.root.clone();
        let impl_file = ImplFile::cast(parsed.root)?;
        Some((
            root,
            resolve_file(
                &impl_file,
                &ProjectItems::default(),
                &references.env,
                &recovery,
            ),
        ))
    }));
    let (root_node, rf) = match resolved {
        Ok(Some(resolved)) => resolved,
        Ok(None) => {
            tally.our_errors += 1;
            tally.manifest.push((rel, "our-parse-errors".to_string()));
            return;
        }
        Err(_) => {
            tally.our_skipped += 1;
            tally.manifest.push((rel, "our-panic".to_string()));
            return;
        }
    };

    tally.files_compared += 1;
    let positions = Positions::new(&source);
    let check = if file.has_check_errors {
        "fcs-check-errors"
    } else {
        "fcs-clean"
    };
    // Per kind of record, the verdicts given: listed, or (a match, a declined
    // external use) left to the counts.
    let (mut in_file_listed, mut matches) = (0usize, 0usize);
    let (mut assembly_listed, mut assembly_matched) = (0usize, 0usize);
    let (mut external_listed, mut external_declined) = (0usize, 0usize);

    // Binder completeness: every binder FCS declares that the walk does not
    // know about is listed, and a use FCS binds to one must not commit.
    let unmodelled = unmodelled_binders(&rf, &root_node, &census_oracle_binders(file, &source));
    for b in &unmodelled {
        tally.unmodelled_binders += 1;
        tally.manifest.push((
            format!(
                "{rel}:{} {:?} binder",
                positions.at(usize::from(b.range.start())),
                b.name
            ),
            format!("unmodelled-binder {:?} {}", b.class, b.site),
        ));
    }
    for c in commits_behind_unmodelled_binders(&rf, &unmodelled, &census_oracle_uses(file, &source))
    {
        let text = source
            .get(usize::from(c.use_range.start())..usize::from(c.use_range.end()))
            .unwrap_or("");
        tally.manifest.push((
            format!(
                "{rel}:{} {text:?} behind-binder",
                positions.at(usize::from(c.use_range.start()))
            ),
            "divergence commit-behind-unmodelled-binder".to_string(),
        ));
        tally.divergences.push(Site {
            path: path.clone(),
            range: c.use_range,
            text: text.to_string(),
            expected: format!("the unmodelled binder at {:?}", c.binder.range),
            ours: format!("{:?}", c.committed),
            fcs_check_errors: file.has_check_errors,
        });
    }

    let all_uses = census_resolve_uses(file, &source);
    // The ranges at which FCS records an in-file symbol, as a use *or* a
    // definition. FCS can record several symbols at one range: a type name and
    // its constructor, an attribute's type and its constructor, and — at a
    // type name whose primary constructor takes a `?optional` parameter — the
    // `OptionalArgumentAttribute` it synthesises, together with its namespace
    // path. Where one of them is in-file, an in-file answer is the in-file
    // slice's to grade (or, at a definition, nobody's), not the external one's.
    let in_file_ranges: HashSet<(usize, usize)> = all_uses
        .iter()
        .filter(|u| matches!(u.decl, CensusDecl::InFile(..)))
        .map(|u| (u.start, u.end))
        .collect();
    let uses: Vec<ResolveDiffUse> = all_uses
        .into_iter()
        // A definition is not a name to resolve; the implicit anonymous-module
        // symbol is reported at a zero-width range.
        .filter(|u| !u.is_from_definition && u.start != u.end)
        .collect();
    let out_of_file = out_of_file_answers(&uses, &in_file_ranges, references, tally);
    // The oracle's records this file's line counts, read off the census alone.
    let b1 = uses
        .iter()
        .filter(|u| matches!(u.decl, CensusDecl::InFile(..)) && u.bucket == Some(Bucket::B1))
        .count();
    let assembly = out_of_file
        .values()
        .filter(|a| matches!(a, OutOfFile::Assembly(_)))
        .count();
    let external = out_of_file.len() - assembly;

    // Out-of-file answers, one per range, in source order so the printed
    // sample reads top to bottom.
    let mut out_of_file: Vec<((usize, usize), OutOfFile)> = out_of_file.into_iter().collect();
    out_of_file.sort_by_key(|(range, _)| *range);
    for ((start, end), answer) in out_of_file {
        let use_range = TextRange::new(
            u32::try_from(start).unwrap().into(),
            u32::try_from(end).unwrap().into(),
        );
        let text = source.get(start..end).unwrap_or("");
        let key = format!("{rel}:{} {text:?}", positions.at(start));
        let at = |r: TextRange| positions.at(usize::from(r.start()));
        let res = rf.resolution_at(use_range);
        let ours = |res: Resolution| match res {
            Resolution::Local(_) | Resolution::Item(_) => match rf.resolved_def(res) {
                Some(def) => format!("binder={:?} at {}", def.name, at(def.range)),
                None => "no-in-file-def".to_string(),
            },
            Resolution::Unresolved => "unresolved".to_string(),
            // Single-file: no earlier file to name.
            Resolution::EarlierFileType { .. } => "earlier-file".to_string(),
            Resolution::Entity(_) | Resolution::Member { .. } | Resolution::UnionCase { .. } => {
                let decl = assembly_resolution_decl(&references.env, res);
                format!("assembly {} {}", decl.assembly, decl.full_name)
            }
            Resolution::Deferred(_) => unreachable!("a decline is not an answer"),
        };
        let mut diverge = |expected: String, ours: String, verdict: &str| {
            tally
                .manifest
                .push((key.clone(), format!("divergence {verdict} {ours}")));
            tally.divergences.push(Site {
                path: path.clone(),
                range: use_range,
                text: text.to_string(),
                expected,
                ours,
                fcs_check_errors: file.has_check_errors,
            });
        };
        match answer {
            // FCS named a declaration in a referenced assembly, which our env
            // holds too: a committed answer must be that declaration. Every
            // bucket is graded — whatever machinery FCS needed, a commitment
            // claims the target.
            OutOfFile::Assembly(expected) => {
                tally.assembly_graded += 1;
                match res {
                    None | Some(Resolution::Deferred(_)) => {
                        tally.assembly_gaps += 1;
                        assembly_listed += 1;
                        tally.manifest.push((key, "assembly-gap".to_string()));
                    }
                    Some(res) if assembly_answer_agrees(&references.env, res, &expected) => {
                        tally.assembly_matches += 1;
                        assembly_matched += 1;
                    }
                    Some(res) => {
                        assembly_listed += 1;
                        diverge(
                            format!("assembly {} {}", expected.assembly, expected.full_name),
                            ours(res),
                            "assembly-target",
                        );
                    }
                }
            }
            // FCS declared the target in another file and named it by nothing
            // we can compare, so declining is the only correct outcome.
            OutOfFile::External => {
                tally.external_graded += 1;
                match res {
                    None | Some(Resolution::Deferred(_)) => external_declined += 1,
                    Some(res) => {
                        external_listed += 1;
                        diverge("outside the file".to_string(), ours(res), "external-target");
                    }
                }
            }
        }
    }

    for u in uses {
        let use_range = TextRange::new(
            u32::try_from(u.start).unwrap().into(),
            u32::try_from(u.end).unwrap().into(),
        );
        let text = source.get(u.start..u.end).unwrap_or("");
        let key = format!("{rel}:{} {text:?}", positions.at(u.start));
        let at = |r: TextRange| positions.at(usize::from(r.start()));

        // Out-of-file uses were graded above, one answer per range.
        let CensusDecl::InFile(ds, de) = u.decl else {
            continue;
        };
        // Of in-file declarations, only the lexical (B1) bucket is reproducible
        // without inference.
        if u.bucket != Some(Bucket::B1) {
            continue;
        }
        let expected = TextRange::new(
            u32::try_from(ds).unwrap().into(),
            u32::try_from(de).unwrap().into(),
        );
        let site = |ours: String| Site {
            path: path.clone(),
            range: use_range,
            text: text.to_string(),
            expected: format!("{expected:?}"),
            ours,
            fcs_check_errors: file.has_check_errors,
        };

        let verdict = match rf.resolution_at(use_range) {
            // We recorded nothing here, or honestly deferred: a modeling gap,
            // not a disagreement (e.g. named-module headers we don't intern, or
            // a long-ident occurrence we key by a different range).
            None | Some(Resolution::Deferred(_)) => {
                tally.gaps += 1;
                Some("gap".to_string())
            }
            Some(res @ (Resolution::Local(_) | Resolution::Item(_))) => {
                match rf.resolved_def(res) {
                    // Exact match — the headline coverage, counted per file.
                    Some(def) if def.range == expected => {
                        tally.matches += 1;
                        None
                    }
                    // Same-named in-file binder, different range.
                    Some(def) if def.name == text => {
                        tally
                            .alt_binders
                            .push(site(format!("binder {:?} at {:?}", def.name, def.range)));
                        Some(format!(
                            "alt-binder fcs={} ours={} {check}",
                            at(expected),
                            at(def.range)
                        ))
                    }
                    // A *differently-named* binder — we resolved to the wrong
                    // symbol entirely.
                    Some(def) => {
                        tally
                            .divergences
                            .push(site(format!("binder {:?} at {:?}", def.name, def.range)));
                        Some(format!(
                            "divergence binder={:?} at {}",
                            def.name,
                            at(def.range)
                        ))
                    }
                    None => {
                        tally
                            .divergences
                            .push(site(format!("{res:?} (no in-file def)")));
                        Some("divergence no-in-file-def".to_string())
                    }
                }
            }
            // FCS resolved an in-file binder, but we point into an assembly or
            // claim the name is unresolved — an unambiguous soundness fault.
            Some(other) => {
                tally.divergences.push(site(format!("{other:?}")));
                Some("divergence out-of-file".to_string())
            }
        };
        match verdict {
            Some(v) => {
                in_file_listed += 1;
                tally.manifest.push((key, v));
            }
            None => matches += 1,
        }
    }
    if in_file_listed + matches != b1
        || assembly_listed + assembly_matched != assembly
        || external_listed + external_declined != external
    {
        tally.unaccounted.push(format!(
            "{rel}: b1={b1} but {in_file_listed} listed + {matches} matches; \
             assembly={assembly} but {assembly_listed} listed + {assembly_matched} matches; \
             external={external} but {external_listed} listed + {external_declined} declined"
        ));
    }
    tally.manifest.push((
        rel,
        format!("compared {check} b1={b1} assembly={assembly} external={external}"),
    ));
}

/// FCS's one answer at each out-of-file range of `uses` (non-definition,
/// non-zero-width records), counting into `tally` the records set aside: those
/// at a range where FCS also records an in-file symbol, those naming no target
/// we can read, and ranges with two different answers (see the module docs).
fn out_of_file_answers(
    uses: &[ResolveDiffUse],
    in_file_ranges: &HashSet<(usize, usize)>,
    references: &OracleReferences,
    tally: &mut Tally,
) -> HashMap<(usize, usize), OutOfFile> {
    let answer = |u: &ResolveDiffUse| -> Option<OutOfFile> {
        match (&u.decl, &u.assembly) {
            (CensusDecl::InFile(..), _) => None,
            // No one assembly declares a namespace (FCS names whichever it met
            // first), and no answer of ours names one, so it must decline.
            (_, Some(decl)) if decl.kind.as_deref() == Some("namespace") => {
                Some(OutOfFile::External)
            }
            (_, Some(decl)) if references.declares(decl) => Some(OutOfFile::Assembly(decl.clone())),
            (CensusDecl::OtherFile, _) => Some(OutOfFile::External),
            (CensusDecl::Absent, _) => None,
        }
    };
    // Ranges of every record that names its site by the site's own name rather
    // than as a constructor, in-file or out.
    let named_at: Vec<(usize, usize)> = uses
        .iter()
        .filter(|u| !u.is_constructor)
        .filter(|u| matches!(u.decl, CensusDecl::InFile(..)) || answer(u).is_some())
        .map(|u| (u.start, u.end))
        .collect();
    // A constructor record ending with a named record that encloses it is the
    // same written name, which that record grades.
    let shadowed = |u: &ResolveDiffUse| {
        u.is_constructor
            && named_at
                .iter()
                .any(|&(start, end)| end == u.end && start <= u.start)
    };

    let mut answers: HashMap<(usize, usize), OutOfFile> = HashMap::new();
    let mut ambiguous: HashSet<(usize, usize)> = HashSet::new();
    for u in uses {
        if matches!(u.decl, CensusDecl::InFile(..)) {
            continue;
        }
        if in_file_ranges.contains(&(u.start, u.end)) {
            tally.external_shared_range += 1;
            continue;
        }
        let Some(answer) = answer(u) else {
            tally.external_absent += 1;
            continue;
        };
        if shadowed(u) {
            continue;
        }
        match answers.get(&(u.start, u.end)) {
            None => {
                answers.insert((u.start, u.end), answer);
            }
            // The same declaration said twice adjudicates fine.
            Some(existing) if *existing == answer => {}
            Some(_) => {
                ambiguous.insert((u.start, u.end));
            }
        }
    }
    tally.external_ambiguous += ambiguous.len();
    answers.retain(|range, _| !ambiguous.contains(range));
    answers
}

/// Recursively collect `.fs` implementation files (not `.fsi`), skipping
/// build/VCS output and symlinks. Mirrors `uses_census.rs`'s collector.
fn collect_fs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some(".git" | "target" | "artifacts" | "bin" | "obj")
            ) {
                continue;
            }
            collect_fs(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("fs") {
            out.push(path);
        }
    }
}

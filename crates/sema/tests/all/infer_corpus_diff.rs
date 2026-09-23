//! Inference soundness sweep over the corpus: every expression type and binder
//! type [`borzoi_sema::infer_file`] commits, checked against FCS's typed tree —
//! the inference counterpart of `resolve_corpus_diff.rs`.
//!
//! The per-construct differentials (`infer_*_diff.rs`) grade the shapes someone
//! thought to write down, and the generative ones the shapes a generator can
//! build. Neither sees the shapes real code is made of, which is where a rule
//! that is sound on every curated case meets a combination nobody listed. This
//! sweep runs inference on a sample of real F# and grades every commit.
//!
//! # What is compared
//!
//! Certain-implies-exact, per committed range. Each commit falls in one bucket:
//!
//! * **agree** — FCS has a node (or binder) at that range with the same
//!   canonical type. Floored by [`MIN_AGREEMENTS`]: the sweep must keep
//!   measuring something.
//! * **divergence** — FCS reported no error anywhere in the top-level `let`
//!   the commit sits in, and either reports a different type there or no node
//!   at all. A declaration FCS checked without error is its answer, so any
//!   disagreement is ours. Ceilinged by [`MAX_DIVERGENCES`]; sites printed.
//! * **error-recovered** — FCS reported an error somewhere in that
//!   declaration, and disagrees or has no node. The file is checked *alone* (as
//!   the other corpus sweeps do), so an `open` of a sibling module fails and FCS
//!   recovers the declaration as it likes — including dropping a RHS on lines
//!   that carry no diagnostic of their own, which is why the unit is the
//!   declaration and not the line. Reported, not gated: a commit there is only
//!   as wrong as FCS's recovery is right.
//!
//! Both sides parse the same program: FCS's script check defines `INTERACTIVE`
//! and `EDITING` ([`FCS_SCRIPT_SYMBOLS`], pinned by
//! [`fcs_script_check_defines_exactly_these_symbols`]), and we parse with the
//! same set.
//!
//! # Coverage, by FCS node kind
//!
//! Alongside the gate, the sweep prints how many of FCS's nodes on clean lines
//! we commit, per FCS node kind (`application`, `call:instance`, `let`, …). It
//! is a measurement, not a gate: the uncovered column is the worklist for the
//! next inference slice, read in FCS's own vocabulary rather than guessed.
//!
//! # Completeness, by blocking construct
//!
//! One unmodelled construct anywhere in a binding marks the whole binding
//! incomplete, which switches off its argument checks and its generalisation —
//! so most of what inference could say about real code is withheld by the
//! *completeness* gate, not by any one rule. The sweep prints, from
//! [`borzoi_sema::InferredFile::incompleteness`], how many walked bindings are
//! complete and, per [`borzoi_sema::Incomplete`] reason, how many bindings have
//! it as their **only observed** reason and how many have it at all. The first
//! ranks what modelling that construct could unlock — a heuristic, not a bound:
//! the walk does not descend into what it does not model, so a reason beneath
//! an unmodelled construct goes unseen, and a failure's effect elsewhere (a
//! local aliasing an open local) can surface as a reason of its own. A
//! measurement, not a gate.
//!
//! Deferring is never graded. A file our parser rejects is skipped (as in
//! `resolve_corpus_diff`), and so is one FCS's batch handler could not check —
//! almost always FCS throwing "error recovery at …" while materialising the typed
//! tree of a file that does not check in isolation. Those are printed.
//!
//! # The reference set
//!
//! FCS checks each file as a script with the SDK's references; the sweep's env
//! is FSharp.Core plus **every** assembly in the reference pack, so a name FCS
//! resolves into, say, `System.Collections` resolves for us too rather than
//! falling through to a different reading. What remains different is that FCS
//! reads the *implementation* assemblies and we read the *reference* ones; the
//! `assembly-surface-check` skill records where that bites (which methods are
//! single-candidate).
//!
//! `#[ignore]`d: it type-checks a corpus sample. Run under `nix develop` (which
//! sets `BORZOI_CORPUS`):
//!
//! ```text
//! cargo test -p borzoi-sema --test all infer_corpus_diff:: -- --ignored --nocapture
//! ```
//!
//! Tune the sample with `BORZOI_INFER_DIFF_STRIDE` (default 13, the stride the
//! other corpus sweeps use) and `BORZOI_INFER_DIFF_LIMIT`. The ratchets below
//! are tied to the default stride.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use borzoi_cst::parser::parse_with_symbols;
use borzoi_cst::syntax::{AstNode, ImplFile, LetDecl};
use borzoi_oracle_harness::panic_silence::silence_panics_here;
use borzoi_sema::{AssemblyEnv, ProjectItems, SyntaxRecovery, infer_file, resolve_file};

use crate::common::{
    FcsCheckError, LineIndex, env_usize_or, parse_fcs_binder_types_with_errors,
    parse_fcs_types_with_errors, try_invoke_fcs_dump,
};
use serde::Deserialize;

/// Ceiling on commits that disagree with FCS on a line it checked cleanly. Zero
/// since the sweep was first run (2026-09-22), and it stays there: a
/// divergence here is a wrong hover on real code.
const MAX_DIVERGENCES: usize = 0;

/// Floor on commits FCS confirms (expression and binder types together). Only
/// goes up — bump it after a slice lands. One-sided, so it fails if the sweep
/// stops measuring, not if inference commits more.
///
/// 1701 measured 2026-09-23 (325 files compared, stride 13), with CE-1's
/// expression-level `let`s and sequences and structural tuple and wildcard
/// patterns typed; the floor sits a little under to absorb a file moving in or
/// out of FCS's checkable set.
const MIN_AGREEMENTS: usize = 1650;

/// The conditional-compilation symbols FCS's single-file script check defines,
/// which our parse must match or the two sides check different programs.
const FCS_SCRIPT_SYMBOLS: [&str; 2] = ["INTERACTIVE", "EDITING"];

/// How many sites of each kind to print.
const SAMPLE: usize = 40;

/// A committed type that disagreed with FCS.
#[derive(Debug)]
struct Site {
    path: PathBuf,
    kind: &'static str,
    line: u32,
    text: String,
    ours: String,
    fcs: Option<String>,
}

#[derive(Debug, Default)]
struct Tally {
    files_compared: usize,
    our_parse_errors: usize,
    /// Files skipped for a line directive (see [`has_line_directive`]).
    line_directives: usize,
    our_panics: usize,
    fcs_failed: Vec<(PathBuf, String)>,
    agree_exprs: usize,
    agree_binders: usize,
    divergences: Vec<Site>,
    error_lines: Vec<Site>,
    /// Per FCS node kind: `(nodes on clean lines, of which we committed)`.
    coverage: BTreeMap<String, (usize, usize)>,
    /// Bindings inference walked, and how many of them were complete.
    bindings: usize,
    complete_bindings: usize,
    /// Per incompleteness reason: bindings where it is the only observed
    /// reason (a heuristic ranking of what modelling it could unlock).
    sole_blocker: BTreeMap<String, usize>,
    /// Per incompleteness reason: bindings where it is among the reasons.
    any_blocker: BTreeMap<String, usize>,
}

/// The `types` payload's node kinds, which [`parse_fcs_types_with_errors`]
/// drops: the coverage table's axis.
#[derive(Deserialize)]
struct KindDump {
    #[serde(rename = "Exprs")]
    exprs: Vec<KindExpr>,
}

#[derive(Deserialize)]
struct KindExpr {
    #[serde(rename = "Range")]
    range: KindRange,
    #[serde(rename = "Kind")]
    kind: String,
}

#[derive(Deserialize)]
struct KindRange {
    #[serde(rename = "Start")]
    start: KindPos,
    #[serde(rename = "End")]
    end: KindPos,
}

#[derive(Deserialize)]
struct KindPos {
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Col")]
    col: u32,
}

/// FSharp.Core plus every assembly in the reference pack FCS's script check
/// draws on.
fn ref_pack_env() -> &'static AssemblyEnv {
    use std::sync::OnceLock;
    static ENV: OnceLock<AssemblyEnv> = OnceLock::new();
    ENV.get_or_init(|| {
        use borzoi_assembly::Ecma335Assembly;
        let sysrt = crate::common::ensure_system_runtime_dll();
        let pack = sysrt.parent().expect("ref pack dir").to_path_buf();
        let mut bytes: Vec<Vec<u8>> =
            vec![std::fs::read(crate::common::ensure_fsharp_core_dll()).expect("FSharp.Core")];
        let mut dlls: Vec<PathBuf> = std::fs::read_dir(&pack)
            .expect("read ref pack")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("dll"))
            .collect();
        dlls.sort();
        for dll in &dlls {
            bytes.push(std::fs::read(dll).expect("read ref pack dll"));
        }
        let views: Vec<_> = bytes
            .iter()
            .zip(
                std::iter::once(Path::new("FSharp.Core.dll"))
                    .chain(dlls.iter().map(|p| p.as_path())),
            )
            .map(|(b, p)| {
                Ecma335Assembly::parse(b).unwrap_or_else(|e| panic!("parse {}: {e:?}", p.display()))
            })
            .collect();
        AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
    })
}

/// The 1-based line of each byte offset's start, by binary search over line
/// starts.
struct Lines(Vec<usize>);

impl Lines {
    fn new(src: &str) -> Lines {
        let mut starts = vec![0];
        starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
        Lines(starts)
    }
    fn line_of(&self, offset: usize) -> u32 {
        (self.0.partition_point(|&s| s <= offset)) as u32
    }
}

/// Whether FCS reported an error on any line `[start, end)` spans.
fn touches_error(lines: &Lines, errors: &[FcsCheckError], start: usize, end: usize) -> bool {
    let (l0, l1) = (
        lines.line_of(start),
        lines.line_of(end.saturating_sub(1).max(start)),
    );
    errors.iter().any(|e| (l0..=l1).contains(&e.line))
}

/// Whether `source` carries a line directive (`#line 100 "f.fs"`, or `# 100`).
/// FCS reports ranges and diagnostics after it in the directive's *virtual*
/// coordinates — another line, often another file name, whose nodes the oracle
/// drops — so the file cannot be compared position by position, and is skipped.
fn has_line_directive(source: &str) -> bool {
    source.lines().any(|line| {
        let Some(rest) = line.trim_start().strip_prefix('#') else {
            return false;
        };
        let rest = rest.trim_start();
        rest.starts_with("line") || rest.starts_with(|c: char| c.is_ascii_digit())
    })
}

/// The byte range of the outermost `let` declaration enclosing `[start, end)`,
/// or the range itself outside any: the unit FCS's error recovery works in.
fn enclosing_decl(file: &ImplFile, start: usize, end: usize) -> (usize, usize) {
    let range = rowan::TextRange::new((start as u32).into(), (end as u32).into());
    let outermost = file
        .syntax()
        .covering_element(range)
        .ancestors()
        .filter_map(LetDecl::cast)
        .last();
    match outermost {
        Some(decl) => {
            let r = decl.syntax().text_range();
            (u32::from(r.start()) as usize, u32::from(r.end()) as usize)
        }
        None => (start, end),
    }
}

/// Compare one corpus file, folding its outcome into `tally`.
fn compare_file(path: &Path, tally: &Mutex<Tally>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    // FCS reads the file with a leading byte-order mark stripped, so its ranges
    // are offsets into the BOM-less text; ours must be too.
    let source = raw.strip_prefix('\u{feff}').unwrap_or(&raw).to_string();
    if has_line_directive(&source) {
        tally.lock().unwrap().line_directives += 1;
        return;
    }
    let symbols: std::collections::HashSet<String> =
        FCS_SCRIPT_SYMBOLS.iter().map(|s| s.to_string()).collect();
    let parsed = parse_with_symbols(&source, &symbols);
    if !parsed.errors.is_empty() {
        tally.lock().unwrap().our_parse_errors += 1;
        return;
    }
    let env = ref_pack_env();
    let file_for_decls = ImplFile::cast(parsed.root.clone());
    // Silence the expected panics of our own resolve/infer (counted below) —
    // and only those: a panic anywhere else in the worker must keep its payload.
    let silence = silence_panics_here();
    let ours = catch_unwind(AssertUnwindSafe(|| {
        let recovery = SyntaxRecovery::of(&parsed);
        let file = ImplFile::cast(parsed.root.clone())?;
        let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
        let inferred = infer_file(&file, &resolved, env);
        let exprs: Vec<((usize, usize), String)> = inferred
            .types()
            .iter()
            .map(|(r, t)| {
                (
                    (u32::from(r.start()) as usize, u32::from(r.end()) as usize),
                    t.render(),
                )
            })
            .collect();
        let binders: Vec<((usize, usize), String)> = inferred
            .def_types()
            .iter()
            .map(|(d, t)| {
                let r = resolved.def(*d).range;
                (
                    (u32::from(r.start()) as usize, u32::from(r.end()) as usize),
                    t.render(),
                )
            })
            .collect();
        Some((exprs, binders, inferred.incompleteness().to_vec()))
    }));
    drop(silence);
    let (exprs, binders, incompleteness) = match ours {
        Ok(Some(x)) => x,
        Ok(None) => return,
        Err(_) => {
            tally.lock().unwrap().our_panics += 1;
            return;
        }
    };
    {
        let mut t = tally.lock().unwrap();
        for reasons in &incompleteness {
            t.bindings += 1;
            let kinds: std::collections::BTreeSet<String> =
                reasons.iter().map(|r| format!("{r:?}")).collect();
            match kinds.len() {
                0 => t.complete_bindings += 1,
                1 => {
                    let only = kinds.iter().next().expect("one kind").clone();
                    *t.sole_blocker.entry(only).or_default() += 1;
                }
                _ => {}
            }
            for k in kinds {
                *t.any_blocker.entry(k).or_default() += 1;
            }
        }
    }
    let (types_json, binders_json) = match (
        try_invoke_fcs_dump("types", path),
        try_invoke_fcs_dump("binder-types", path),
    ) {
        (Ok(t), Ok(b)) => (t, b),
        (Err(e), _) | (_, Err(e)) => {
            tally
                .lock()
                .unwrap()
                .fcs_failed
                .push((path.to_path_buf(), e));
            return;
        }
    };
    let (fcs_types, errors) = parse_fcs_types_with_errors(&types_json, &source);
    let (fcs_binders, _) = parse_fcs_binder_types_with_errors(&binders_json, &source);
    let lines = Lines::new(&source);
    let kinds: KindDump = serde_json::from_str(&types_json).expect("fcs-dump types JSON shape");
    let idx = LineIndex::new(&source);
    let ours_at: std::collections::HashSet<(usize, usize)> =
        exprs.iter().map(|(r, _)| *r).collect();

    let mut t = tally.lock().unwrap();
    t.files_compared += 1;
    for node in &kinds.exprs {
        let start = idx.offset(node.range.start.line, node.range.start.col);
        let end = idx.offset(node.range.end.line, node.range.end.col);
        if touches_error(&lines, &errors, start, end) {
            continue;
        }
        let entry = t.coverage.entry(node.kind.clone()).or_default();
        entry.0 += 1;
        if ours_at.contains(&(start, end)) {
            entry.1 += 1;
        }
    }
    let grade = |kind: &'static str,
                 commits: &[((usize, usize), String)],
                 fcs: &crate::common::FcsTypeMap,
                 t: &mut Tally| {
        for ((s, e), ours) in commits {
            let theirs = fcs.get(&(*s, *e));
            if theirs == Some(ours) {
                if kind == "expr" {
                    t.agree_exprs += 1;
                } else {
                    t.agree_binders += 1;
                }
                continue;
            }
            let site = Site {
                path: path.to_path_buf(),
                kind,
                line: lines.line_of(*s),
                text: source[*s..*e].chars().take(60).collect(),
                ours: ours.clone(),
                fcs: theirs.cloned(),
            };
            let (d0, d1) = file_for_decls
                .as_ref()
                .map_or((*s, *e), |f| enclosing_decl(f, *s, *e));
            if touches_error(&lines, &errors, d0, d1) {
                t.error_lines.push(site);
            } else {
                t.divergences.push(site);
            }
        }
    };
    grade("expr", &exprs, &fcs_types, &mut t);
    grade("binder", &binders, &fcs_binders, &mut t);
}

fn print_sites(label: &str, sites: &[Site]) {
    if sites.is_empty() {
        return;
    }
    eprintln!(
        "\ninference {label} ({}, showing up to {SAMPLE}):",
        sites.len()
    );
    for s in sites.iter().take(SAMPLE) {
        eprintln!(
            "  {}:{} [{}] {:?}: ours `{}`, FCS {}",
            s.path.display(),
            s.line,
            s.kind,
            s.text,
            s.ours,
            s.fcs
                .as_deref()
                .map_or("<no node>".to_string(), |f| format!("`{f}`")),
        );
    }
}

#[test]
#[ignore = "corpus inference sweep (us + FCS type-check per file); run with --ignored under nix develop"]
fn inferred_types_match_fcs_over_corpus() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!("BORZOI_CORPUS unset; skipping the inference sweep.");
        return;
    };
    let root = PathBuf::from(root);
    let stride = env_usize_or("BORZOI_INFER_DIFF_STRIDE", 13).max(1);
    let limit = env_usize_or("BORZOI_INFER_DIFF_LIMIT", usize::MAX);
    let mut all = Vec::new();
    collect_fs(&root, &mut all);
    all.sort();
    let sample: Vec<PathBuf> = all.iter().step_by(stride).take(limit).cloned().collect();
    assert!(!sample.is_empty(), "no .fs files under {root:?}");
    eprintln!(
        "infer-diff: {} of {} .fs files (stride {stride}); type-checking each in isolation…",
        sample.len(),
        all.len()
    );
    // Build the env once, before the workers race for it.
    let _ = ref_pack_env();

    let tally = Mutex::new(Tally::default());
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = sample.get(i) else { break };
                    // A failure outside our own (caught) resolve/infer — the
                    // parser, the oracle — is a failure of the sweep. Its message
                    // has already printed; name the file it was on.
                    if catch_unwind(AssertUnwindSafe(|| compare_file(path, &tally))).is_err() {
                        panic!(
                            "infer-diff: comparing {} failed (see above)",
                            path.display()
                        );
                    }
                }
            });
        }
    });
    let mut t = tally.into_inner().unwrap();
    t.divergences
        .sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    t.error_lines
        .sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));

    eprintln!(
        "infer-diff: {} files compared | {} expr + {} binder agree | {} diverge | {} in \
         error-recovered declarations | {} our-parse-errors | {} line-directive files | {} \
         our-panics | {} fcs-failed",
        t.files_compared,
        t.agree_exprs,
        t.agree_binders,
        t.divergences.len(),
        t.error_lines.len(),
        t.our_parse_errors,
        t.line_directives,
        t.our_panics,
        t.fcs_failed.len(),
    );
    for (p, e) in &t.fcs_failed {
        eprintln!(
            "  fcs-failed: {}: {}",
            p.display(),
            e.chars().take(160).collect::<String>()
        );
    }
    let mut rows: Vec<(&String, &(usize, usize))> = t.coverage.iter().collect();
    rows.sort_by_key(|(_, (n, c))| std::cmp::Reverse(n - c));
    eprintln!("\ninference coverage of FCS nodes on clean lines, by kind (most uncovered first):");
    for (kind, (n, c)) in rows.iter().take(30) {
        eprintln!(
            "  {kind:<28} {c:>6} / {n:<6} ({:>5.1}%)",
            100.0 * *c as f64 / *n as f64
        );
    }
    eprintln!(
        "\ninference completeness: {} of {} walked bindings complete ({:.1}%)",
        t.complete_bindings,
        t.bindings,
        100.0 * t.complete_bindings as f64 / t.bindings.max(1) as f64
    );
    let mut sole: Vec<(&String, &usize)> = t.sole_blocker.iter().collect();
    sole.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    eprintln!("  only observed reason (a heuristic ranking, not a bound) / present in:");
    for (reason, n) in sole.iter().take(25) {
        eprintln!("    {reason:<40} {n:>6} / {:<6}", t.any_blocker[*reason]);
    }
    let mut any: Vec<(&String, &usize)> = t.any_blocker.iter().collect();
    any.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    eprintln!("  most widespread (present in):");
    for (reason, n) in any.iter().take(15) {
        eprintln!("    {reason:<40} {n:>6}");
    }
    print_sites("divergences (gated)", &t.divergences);
    print_sites("error-recovered disagreements (reported)", &t.error_lines);

    assert_eq!(t.our_panics, 0, "resolve/infer panicked on a corpus file");
    #[allow(clippy::absurd_extreme_comparisons)]
    {
        assert!(
            t.divergences.len() <= MAX_DIVERGENCES,
            "{} committed types disagree with FCS on cleanly-checked lines (ceiling \
             MAX_DIVERGENCES = {MAX_DIVERGENCES})",
            t.divergences.len()
        );
    }
    assert!(
        t.agree_exprs + t.agree_binders >= MIN_AGREEMENTS,
        "only {} commits agree with FCS (floor MIN_AGREEMENTS = {MIN_AGREEMENTS})",
        t.agree_exprs + t.agree_binders
    );
}

/// Recursively collect `.fs` implementation files, skipping build/VCS output
/// and symlinks. Mirrors `resolve_corpus_diff`'s collector.
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

/// The symbols FCS's single-file script check defines, read off FCS itself: one
/// `#if SYMBOL` / `#else` pair per candidate, and the branch FCS took shows in
/// the binder's type. [`FCS_SCRIPT_SYMBOLS`] must be exactly the ones it took,
/// or the sweep parses a different program from the one FCS checks.
#[test]
fn fcs_script_check_defines_exactly_these_symbols() {
    let candidates = [
        "INTERACTIVE",
        "EDITING",
        "COMPILED",
        "DEBUG",
        "RELEASE",
        "NETCOREAPP",
        "NET",
        "TRACE",
    ];
    let mut src = String::from("module M\n");
    for sym in candidates {
        src.push_str(&format!(
            "#if {sym}\nlet v_{sym} = 1\n#else\nlet v_{sym} = \"no\"\n#endif\n"
        ));
    }
    let path = crate::common::temp_fs_file("infer_corpus_symbols", &src);
    let json = crate::common::invoke_fcs_dump("binder-types", &path);
    let _ = std::fs::remove_file(&path);
    let binders = crate::common::parse_fcs_binder_types(&json, &src);
    let mut defined: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|sym| {
            let name = format!("v_{sym}");
            let start = src.find(&format!("let {name} = 1")).expect("binder") + 4;
            binders
                .get(&(start, start + name.len()))
                .map(String::as_str)
                == Some("System.Int32")
        })
        .collect();
    defined.sort_unstable();
    let mut expected = FCS_SCRIPT_SYMBOLS.to_vec();
    expected.sort_unstable();
    assert_eq!(defined, expected);
}

/// A file with a UTF-8 byte-order mark compares exactly like its BOM-less
/// twin: FCS strips the mark before its ranges are taken, so the sweep must
/// too, or every commit on the first line lands off by the mark's bytes. The
/// binding is on line 1 on purpose — later lines' offsets never see the mark.
#[test]
fn a_bom_prefixed_file_compares_like_its_bomless_twin() {
    let path = crate::common::temp_fs_file("infer_corpus_bom", "\u{feff}module M = let x = 1\n");
    let tally = Mutex::new(Tally::default());
    compare_file(&path, &tally);
    let _ = std::fs::remove_file(&path);
    let t = tally.into_inner().unwrap();
    assert!(t.divergences.is_empty(), "{:?}", t.divergences);
    assert_eq!(t.agree_exprs + t.agree_binders, 2, "`x` and `1`: {t:?}");
}

/// A file with a line directive is skipped rather than compared: FCS reports
/// what follows the directive in its virtual coordinates, so an error there
/// would miss the declaration it belongs to and a node would carry another
/// file's name. Here the directive sits before an erroring declaration, which
/// would otherwise read as a gated divergence.
#[test]
fn a_line_directive_file_is_skipped_not_compared() {
    let src = "module M\n#line 100 \"generated.fs\"\nlet (|A|B|) (x: int) (y: int) = if x > y then A else B\nlet s = \"BAD DOG!\"\n";
    let path = crate::common::temp_fs_file("infer_corpus_line", src);
    let tally = Mutex::new(Tally::default());
    compare_file(&path, &tally);
    let _ = std::fs::remove_file(&path);
    let t = tally.into_inner().unwrap();
    assert_eq!((t.line_directives, t.files_compared), (1, 0), "{t:?}");
    assert!(has_line_directive("# 7 \"x.fs\"\n"));
    assert!(!has_line_directive("#if DEBUG\n#endif\n"));
}

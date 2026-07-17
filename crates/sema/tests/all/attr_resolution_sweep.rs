//! EX-3 §2(d) stage 4 — the generative attribute-resolution differential and
//! the real-corpus sweep (`docs/extension-scope-enumeration-plan.md`, "§2(d)
//! revisited").
//!
//! Stage 3's seven review rounds each hand-found one more FCS-semantics
//! refinement (candidate fallthrough, positional contests, arity). This module
//! is the systematic replacement: **exhaustively enumerate** the in-file
//! semantic space — declaration kind × declaration name × genericity ×
//! position relative to an `open` × written attribute form — and diff every
//! cell against FCS through the resident `attrs-batch` oracle. The space where
//! FCS *semantics* (rather than assembly-metadata shapes) live is small enough
//! that enumeration beats sampling; the metadata shapes (dropped types,
//! collisions, retained auto-opens…) keep their synthetic-env fixtures in
//! `attr_resolution_diff`, which FCS cannot be fed without real DLLs.
//!
//! The property is stage 3's: **certain-implies-exact** — our commit names
//! FCS's resolution, or we decline; plus an aggregate commit floor so the
//! matrix cannot silently decay into wholesale deferral.
//!
//! The `#[ignore]`d corpus sweep runs the same comparison over a real F#
//! source tree (`BORZOI_CORPUS`, each file checked in isolation): every
//! attribute in real code becomes a test point for free.

use std::path::{Path, PathBuf};

use crate::attr_resolution_diff::{check_attrs_agree, fsharp_core_env};
use crate::common::{
    env_usize_or, invoke_fcs_dump_attrs_batch, parse_fcs_attrs_batch, temp_fs_file,
};

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, resolve_file};

/// One generated cell of the matrix.
struct Case {
    label: String,
    src: String,
}

/// A declaration template: `None` for the no-declaration row, else a
/// constructor from the declared name to the declaration text.
type DeclTemplate = Option<fn(&str) -> String>;

/// The exhaustive in-file semantic matrix. Dimensions:
///
/// - **declaration kind**: none; an attribute class; a *generic* attribute
///   class; an abbreviation of `LiteralAttribute`; an `exception` — each a
///   different occupant of F#'s type namespace;
/// - **declaration name**: `Literal` (contests the written form) or
///   `LiteralAttribute` (contests the suffixed candidate — and FSharp.Core's
///   own type);
/// - **declaration position**: before or after the attribute (F# forbids
///   forward type references outside `rec`, so "after" exercises the
///   decline/error paths);
/// - **`open Microsoft.FSharp.Core` position**: absent, at the top, or
///   between the declaration and the attribute (the latest-wins contest);
/// - **written attribute form**: `Literal` or `LiteralAttribute`.
fn matrix() -> Vec<Case> {
    let decl_kinds: [(&str, DeclTemplate); 9] = [
        ("none", None),
        (
            "class",
            Some(|n| format!("type {n}() =\n    inherit System.Attribute()\n")),
        ),
        (
            "generic",
            Some(|n| format!("type {n}<'T>() =\n    inherit System.Attribute()\n")),
        ),
        (
            "abbrev",
            Some(|n| format!("type {n} = Microsoft.FSharp.Core.LiteralAttribute\n")),
        ),
        ("exception", Some(|n| format!("exception {n} of string\n"))),
        // The same four occupants inside an `[<AutoOpen>]` module (AO-2):
        // the declaration reaches the attribute *through the auto-open*, so
        // these cells exercise the name-keyed guards that replaced the
        // presence defer — supplying (decl before), the positional contest
        // (open between / decl after), and decline-on-unresolvable (decl
        // after, no open).
        (
            "autoopen-class",
            Some(|n| {
                format!(
                    "[<AutoOpen>]\nmodule Helpers =\n    type {n}() =\n        inherit System.Attribute()\n"
                )
            }),
        ),
        (
            "autoopen-generic",
            Some(|n| {
                format!(
                    "[<AutoOpen>]\nmodule Helpers =\n    type {n}<'T>() =\n        inherit System.Attribute()\n"
                )
            }),
        ),
        (
            "autoopen-abbrev",
            Some(|n| {
                format!(
                    "[<AutoOpen>]\nmodule Helpers =\n    type {n} = Microsoft.FSharp.Core.LiteralAttribute\n"
                )
            }),
        ),
        (
            "autoopen-exception",
            Some(|n| format!("[<AutoOpen>]\nmodule Helpers =\n    exception {n} of string\n")),
        ),
    ];
    let names = ["Literal", "LiteralAttribute"];
    let writtens = ["Literal", "LiteralAttribute"];
    let open_line = "open Microsoft.FSharp.Core\n";

    let mut cases = Vec::new();
    for (kind, template) in &decl_kinds {
        let name_choices: &[&str] = if template.is_none() { &[""] } else { &names };
        for name in name_choices {
            let decl = template.map(|t| t(name));
            // (decl_pos, open_slot): decl before the attribute admits an open
            // at the top or between; decl after admits top only (the "between"
            // slot no longer exists); no decl admits top only.
            let arrangements: &[(&str, bool, Option<&str>)] = match &decl {
                None => &[("noopen", true, None), ("opentop", true, Some("top"))],
                Some(_) => &[
                    ("noopen", true, None),
                    ("opentop", true, Some("top")),
                    ("openbetween", true, Some("between")),
                    ("declafter-noopen", false, None),
                    ("declafter-opentop", false, Some("top")),
                ],
            };
            for (arr_label, decl_before, open_slot) in arrangements {
                for written in &writtens {
                    let mut src = String::from("module Test\n\n");
                    if *open_slot == Some("top") {
                        src.push_str(open_line);
                        src.push('\n');
                    }
                    if *decl_before && let Some(d) = &decl {
                        src.push_str(d);
                        src.push('\n');
                    }
                    if *open_slot == Some("between") {
                        src.push_str(open_line);
                        src.push('\n');
                    }
                    src.push_str(&format!("[<{written}>]\nlet x = 5\n"));
                    if !*decl_before && let Some(d) = &decl {
                        src.push('\n');
                        src.push_str(d);
                    }
                    cases.push(Case {
                        label: format!("{kind}-{name}-{arr_label}-{written}"),
                        src,
                    });
                }
            }
        }
    }
    cases
}

/// The exhaustive matrix, diffed cell-by-cell against FCS through one
/// resident `attrs-batch` child. Certain-implies-exact per cell (the reverse
/// direction only where FCS's check is clean — an erroring check can
/// under-report its sink without implicating us), plus an aggregate commit
/// floor.
#[test]
fn generative_matrix_agrees_with_fcs() {
    let env = fsharp_core_env();
    let cases = matrix();
    eprintln!("attr sweep: {} generated cells", cases.len());

    let files: Vec<(PathBuf, &Case)> = cases
        .iter()
        .map(|c| (temp_fs_file(&format!("attr_gen_{}", c.label), &c.src), c))
        .collect();
    let paths: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
    let jsonl = invoke_fcs_dump_attrs_batch(&paths);

    let by_path: std::collections::HashMap<String, &Case> = files
        .iter()
        .map(|(p, c)| (p.display().to_string(), *c))
        .collect();
    let entries = parse_fcs_attrs_batch(&jsonl, |path| by_path[path].src.clone());
    assert_eq!(entries.len(), cases.len(), "one oracle line per cell");

    let mut commits = 0usize;
    let mut declines = 0usize;
    for entry in &entries {
        let case = by_path[&entry.path];
        assert!(
            entry.ok,
            "oracle could not check {}: {}",
            case.label, entry.error
        );
        // Our side: the generated grammar must parse cleanly — a cell our
        // parser rejects is a hole in the sweep, not a decline.
        let p = parse(&case.src);
        assert!(
            p.errors.is_empty(),
            "cell {} does not parse: {:?}\n{}",
            case.label,
            p.errors,
            case.src
        );
        let file = ImplFile::cast(p.root).expect("impl file");
        let rf = resolve_file(&file, &ProjectItems::default(), &env);

        let clean = entry.oracle.errors.is_empty();
        let cell_commits = check_attrs_agree(&case.src, &env, &rf, &entry.oracle, clean);
        commits += cell_commits;
        declines += entry.oracle.attrs.len() - cell_commits;
        let _ = std::fs::remove_file(&entry.path);
    }

    eprintln!(
        "attr sweep: {} cells, {commits} commits, {declines} declines (of FCS-resolved attributes)",
        cases.len()
    );
    // The aggregate floor: measured 172 with the auto-open dimension (AO-2 —
    // 164 cells; each `[<AutoOpen>]` wrapper is itself a committing
    // attribute), floored a little under to tolerate small FCS drift while
    // still catching any decay toward wholesale deferral. Ratchet upward as
    // coverage grows.
    assert!(
        commits >= 160,
        "matrix commit floor: {commits} < 160 — the resolver decayed into wholesale deferral"
    );
}

fn collect_fs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_fs(&path, out);
        } else if path.extension().is_some_and(|e| e == "fs") {
            out.push(path);
        }
    }
}

/// The real-corpus sweep: every attribute FCS resolves in a real F# source
/// tree, diffed against our resolver (empty cross-file context, real
/// FSharp.Core env — the same conservative envelope the isolation census
/// uses). Certain-implies-exact; reports commit/decline rates.
#[test]
#[ignore = "corpus sweep: needs BORZOI_CORPUS + builds fcs-dump; run with --ignored under nix develop"]
fn corpus_attributes_agree_with_fcs() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!("BORZOI_CORPUS unset; skipping attribute corpus sweep.");
        return;
    };
    let env = fsharp_core_env();
    // Default sized to the oracle child's one-hour budget: an isolation
    // check of a compiler-sized corpus file runs ~5s, so ~250 files fits
    // with margin (stride 7 = 745 files hit the deadline at ~700).
    let stride = env_usize_or("BORZOI_ATTR_SWEEP_STRIDE", 19).max(1);
    let limit = env_usize_or("BORZOI_ATTR_SWEEP_LIMIT", usize::MAX);

    let mut all_files = Vec::new();
    collect_fs(&PathBuf::from(root), &mut all_files);
    all_files.sort();
    let sample: Vec<PathBuf> = all_files
        .iter()
        .step_by(stride)
        .take(limit)
        .cloned()
        .collect();
    assert!(!sample.is_empty(), "no .fs files in the corpus");
    eprintln!(
        "attr corpus sweep: {} of {} files (stride {stride})",
        sample.len(),
        all_files.len()
    );

    let jsonl = invoke_fcs_dump_attrs_batch(&sample);
    let entries = parse_fcs_attrs_batch(&jsonl, |path| {
        std::fs::read_to_string(path).unwrap_or_default()
    });

    let mut commits = 0usize;
    let mut fcs_attrs = 0usize;
    let mut skipped = 0usize;
    for entry in &entries {
        if !entry.ok {
            skipped += 1;
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&entry.path) else {
            skipped += 1;
            continue;
        };
        // A corpus file our parser cannot yet accept is out of this sweep's
        // scope (the parser corpus gate owns that number) — skip, don't fail.
        let p = parse(&src);
        if !p.errors.is_empty() {
            skipped += 1;
            continue;
        }
        let Some(file) = ImplFile::cast(p.root) else {
            skipped += 1;
            continue;
        };
        let rf = resolve_file(&file, &ProjectItems::default(), &env);
        fcs_attrs += entry.oracle.attrs.len();
        // Reverse direction off: a corpus file checked in isolation errors
        // freely (missing project siblings), and those errors can suppress
        // sink records without implicating our commits.
        commits += check_attrs_agree(&src, &env, &rf, &entry.oracle, false);
    }

    eprintln!(
        "attr corpus sweep: {} files ({skipped} skipped), {fcs_attrs} FCS-resolved attributes, \
         {commits} exact commits, {} declines",
        entries.len(),
        fcs_attrs - commits
    );
    let floor = env_usize_or("BORZOI_ATTR_SWEEP_COMMIT_FLOOR", 1);
    assert!(commits >= floor, "corpus commit floor: {commits} < {floor}");
}

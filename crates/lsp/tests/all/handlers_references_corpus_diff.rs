//! Corpus differential for `textDocument/references`.
//!
//! FCS type-checks a sample of real `.fs` files independently and reports each
//! occurrence together with its declaration range. We open the same text under
//! a virtual `.fs` URI, query the real LSP handler at a spread of source
//! declarations, and compare each returned location with FCS. For a clean FCS
//! check, the hard soundness property is:
//!
//! ```text
//! handler locations ⊆ FCS uses of (display name, in-file declaration range)
//! ```
//!
//! Virtual URIs are intentional: a corpus file may sit beside an `.fsproj`, but
//! the FCS census checks it in isolation. Letting project discovery succeed on
//! our side would compare two different semantic environments. The focused
//! `handlers_references_diff` test covers project-wide and cross-file results.
//!
//! Deferral is an allowed result, so soundness alone could pass vacuously. The
//! answered-target ratio and exact-location count are completeness ratchets,
//! while source-target, file, definition, and ordinary-use floors make the
//! sampled population explicit. At most [`TARGETS_PER_FILE`] declarations are
//! chosen at evenly spaced source positions per file to keep the LSP-boundary
//! sweep bounded without concentrating every query at the top of large files.
//! An isolation check with diagnostics can omit otherwise lexical symbol uses;
//! a returned range absent from such an FCS result is tracked as an oracle
//! omission, while a clean FCS result with no matching use is a divergence. A
//! corroboration-ratio floor prevents oracle omissions from swallowing the
//! useful population.
//!
//! This is ignored because FCS type-checks hundreds of files. Run under
//! `nix develop`, which sets `BORZOI_CORPUS`:
//!
//! ```text
//! cargo test -p borzoi --test all handlers_references_corpus_diff:: -- --ignored --nocapture
//! ```
//!
//! `BORZOI_REFERENCES_DIFF_STRIDE` (default 13) and
//! `BORZOI_REFERENCES_DIFF_LIMIT` tune the sample. The ratchets below are tied
//! to the default stride and the pinned corpus.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use borzoi::handlers::references;
use borzoi::position::{offset_to_position, position_to_offset};
use borzoi::server::State;
use borzoi_oracle_harness::panic_silence::silence_panics_here;
use lsp_types::{
    PartialResultParams, ReferenceContext, ReferenceParams, TextDocumentIdentifier,
    TextDocumentPositionParams, Url, WorkDoneProgressParams,
};

use crate::common::{FileCensus, LineIndex, invoke_fcs_dump_census, parse_fcs_census_jsonl};

const DEFAULT_STRIDE: usize = 13;
const TARGETS_PER_FILE: usize = 4;
const SAMPLE: usize = 40;

/// Population and completeness ratchets for the default sample. Measured
/// 2026-07-19 against the pinned F# corpus (`c3c01c99`): 401 files sampled,
/// 373 with source targets, 17,580 distinct source targets, 1,228 selected,
/// 826 answered (672‰), and 330 files with an answer. Floors leave modest
/// headroom for FCS isolation-check variation; raise them when coverage grows.
const MIN_FILES_WITH_TARGETS: usize = 360;
const MIN_FILES_ANSWERED: usize = 320;
const MIN_SOURCE_TARGETS: usize = 17_000;
const MIN_SELECTED_TARGETS: usize = 1_200;
const MIN_ANSWERED_PERMILLE: usize = 650;

/// Returned-location and oracle-quality ratchets from the same run: 1,540
/// locations corroborated by FCS (826 definitions and 714 ordinary uses), with
/// 136 ranges unobserved in FCS-erroring files. The corroboration denominator
/// includes alternate binders and divergences as well as omissions.
const MIN_EXACT_LOCATIONS: usize = 1_500;
const MIN_DEFINITION_LOCATIONS: usize = 800;
const MIN_USE_LOCATIONS: usize = 680;
const MIN_CORROBORATED_PERMILLE: usize = 900;

/// FCS must never contradict us with a differently named symbol or omit a
/// returned range from a clean check. The one measured same-name alternate
/// binder is FCS isolation recovery around an optional argument, the same
/// benign class documented by sema's resolution corpus differential.
const MAX_DIVERGENCES: usize = 0;
const MAX_ALT_BINDERS: usize = 5;
const MAX_HANDLER_PANICS: usize = 0;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SymbolKey {
    name: String,
    decl: (usize, usize),
}

#[derive(Debug, Clone)]
struct OracleUse {
    key: Option<SymbolKey>,
    name: String,
    start: usize,
    end: usize,
    is_from_definition: bool,
}

#[derive(Debug, Clone)]
struct Target {
    key: SymbolKey,
    cursor: usize,
}

#[derive(Debug)]
struct Site {
    path: PathBuf,
    target: SymbolKey,
    result: (usize, usize),
    occupants: Vec<(String, Option<(usize, usize)>)>,
}

#[derive(Default)]
struct Tally {
    files_seen: usize,
    fcs_not_ok: usize,
    fcs_with_check_errors: usize,
    unreadable: usize,
    files_without_targets: usize,
    files_with_targets: usize,
    files_answered: usize,
    source_targets: usize,
    selected_targets: usize,
    answered_targets: usize,
    declined_targets: usize,
    handler_panics: usize,
    exact_locations: usize,
    definition_locations: usize,
    use_locations: usize,
    divergences: Vec<Site>,
    alt_binders: Vec<Site>,
    oracle_omissions: Vec<Site>,
}

fn env_usize_or(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn params(uri: &Url, source: &str, byte: usize) -> ReferenceParams {
    ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: offset_to_position(source, byte),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration: true,
        },
    }
}

fn normalise_uses(file: &FileCensus, source: &str) -> Vec<OracleUse> {
    let index = LineIndex::new(source);
    file.uses
        .iter()
        .map(|symbol_use| {
            let (start, end) = symbol_use.use_range_bytes(&index);
            let decl = symbol_use.decl_range_bytes(&index);
            OracleUse {
                key: decl.map(|decl| SymbolKey {
                    name: symbol_use.name.clone(),
                    decl,
                }),
                name: symbol_use.name.clone(),
                start,
                end,
                is_from_definition: symbol_use.is_from_definition,
            }
        })
        .collect()
}

/// Ordinary source definitions whose defining occurrence and declaration range
/// coincide. This excludes implicit/synthetic declarations and gives both
/// implementations a cursor-addressable symbol identity.
fn source_targets(uses: &[OracleUse]) -> Vec<Target> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for symbol_use in uses {
        let Some(key) = &symbol_use.key else {
            continue;
        };
        if !symbol_use.is_from_definition
            || symbol_use.start == symbol_use.end
            || key.decl != (symbol_use.start, symbol_use.end)
            || uses
                .iter()
                .filter(|candidate| {
                    candidate.is_from_definition
                        && candidate.start == symbol_use.start
                        && candidate.end == symbol_use.end
                })
                .filter_map(|candidate| candidate.key.as_ref())
                .any(|candidate| candidate != key)
            || !seen.insert(key.clone())
        {
            continue;
        }
        targets.push(Target {
            key: key.clone(),
            cursor: symbol_use.start,
        });
    }
    targets.sort_by_key(|target| target.cursor);
    targets
}

/// Choose all small populations and an even source-order spread from large
/// ones. Every generated case is a valid FCS source declaration; no rejected
/// random candidates can skew the distribution.
fn selected_targets(targets: &[Target]) -> Vec<&Target> {
    if targets.len() <= TARGETS_PER_FILE {
        return targets.iter().collect();
    }
    (0..TARGETS_PER_FILE)
        .map(|i| &targets[i * (targets.len() - 1) / (TARGETS_PER_FILE - 1)])
        .collect()
}

fn compare_file(file: &FileCensus, ordinal: usize, tally: &mut Tally) {
    tally.files_seen += 1;
    if !file.ok {
        tally.fcs_not_ok += 1;
        return;
    }
    let path = PathBuf::from(&file.path);
    if file.has_check_errors {
        tally.fcs_with_check_errors += 1;
    }
    let Ok(source) = std::fs::read_to_string(&path) else {
        tally.unreadable += 1;
        return;
    };
    let uses = normalise_uses(file, &source);
    let targets = source_targets(&uses);
    tally.source_targets += targets.len();
    if targets.is_empty() {
        tally.files_without_targets += 1;
        return;
    }
    tally.files_with_targets += 1;

    let uri = Url::parse(&format!(
        "inmemory:///borzoi-references-corpus/{ordinal}.fs"
    ))
    .unwrap();
    let mut state = State::default();
    state.docs.insert(uri.clone(), source.clone());
    let mut file_answered = false;

    for target in selected_targets(&targets) {
        tally.selected_targets += 1;
        let result = catch_unwind(AssertUnwindSafe(|| {
            references::handle(&mut state, params(&uri, &source, target.cursor))
        }));
        let locations = match result {
            Ok(Some(locations)) => locations,
            Ok(None) => {
                tally.declined_targets += 1;
                continue;
            }
            Err(_) => {
                tally.handler_panics += 1;
                break;
            }
        };
        if locations.is_empty() {
            tally.declined_targets += 1;
            continue;
        }
        tally.answered_targets += 1;
        file_answered = true;

        for location in locations {
            if location.uri != uri {
                tally.divergences.push(Site {
                    path: path.clone(),
                    target: target.key.clone(),
                    result: (0, 0),
                    occupants: vec![(format!("unexpected URI {}", location.uri), None)],
                });
                continue;
            }
            let start = position_to_offset(&source, location.range.start);
            let end = position_to_offset(&source, location.range.end);
            let exact = uses.iter().find(|symbol_use| {
                symbol_use.start == start
                    && symbol_use.end == end
                    && symbol_use.key.as_ref() == Some(&target.key)
            });
            if let Some(exact) = exact {
                tally.exact_locations += 1;
                if exact.is_from_definition {
                    tally.definition_locations += 1;
                } else {
                    tally.use_locations += 1;
                }
                continue;
            }

            let occupants: Vec<_> = uses
                .iter()
                .filter(|symbol_use| symbol_use.start == start && symbol_use.end == end)
                .map(|symbol_use| {
                    (
                        symbol_use.name.clone(),
                        symbol_use.key.as_ref().map(|key| key.decl),
                    )
                })
                .collect();
            let site = Site {
                path: path.clone(),
                target: target.key.clone(),
                result: (start, end),
                occupants,
            };
            if site.occupants.is_empty() && file.has_check_errors {
                tally.oracle_omissions.push(site);
            } else if site
                .occupants
                .iter()
                .any(|(name, _)| name == &target.key.name)
            {
                tally.alt_binders.push(site);
            } else {
                tally.divergences.push(site);
            }
        }
    }

    if file_answered {
        tally.files_answered += 1;
    }
}

fn print_sites(label: &str, sites: &[Site]) {
    if sites.is_empty() {
        return;
    }
    eprintln!(
        "\nreferences {label} ({}, showing up to {SAMPLE}):",
        sites.len()
    );
    for site in sites.iter().take(SAMPLE) {
        eprintln!(
            "  {} target {:?} -> {:?}; FCS occupants {:?}",
            site.path.display(),
            site.target,
            site.result,
            site.occupants,
        );
    }
}

#[test]
#[ignore = "full-corpus find-references differential (FCS type-check); run with --ignored under nix develop"]
fn every_reported_corpus_reference_is_the_cursor_symbol_according_to_fcs() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!(
            "BORZOI_CORPUS unset; skipping references sweep. Run under `nix develop`, or point it at an F# checkout."
        );
        return;
    };
    let root = PathBuf::from(root);
    let stride = env_usize_or("BORZOI_REFERENCES_DIFF_STRIDE", DEFAULT_STRIDE).max(1);
    let limit = env_usize_or("BORZOI_REFERENCES_DIFF_LIMIT", usize::MAX);
    let mut all_files = Vec::new();
    collect_fs(&root, &mut all_files);
    all_files.sort();
    let sample: Vec<_> = all_files
        .iter()
        .step_by(stride)
        .take(limit)
        .cloned()
        .collect();
    assert!(!sample.is_empty(), "no .fs files under {root:?}");
    eprintln!(
        "references-diff: {} of {} .fs files (stride {stride}); type-checking each in isolation…",
        sample.len(),
        all_files.len(),
    );

    let census = parse_fcs_census_jsonl(&invoke_fcs_dump_census(&sample));
    let mut tally = Tally::default();
    {
        let _silence = silence_panics_here();
        for (ordinal, file) in census.iter().enumerate() {
            compare_file(file, ordinal, &mut tally);
        }
    }

    let answered_permille = tally.answered_targets * 1000 / tally.selected_targets.max(1);
    let returned_locations = tally.exact_locations
        + tally.divergences.len()
        + tally.alt_binders.len()
        + tally.oracle_omissions.len();
    let corroborated_permille = tally.exact_locations * 1000 / returned_locations.max(1);
    eprintln!(
        "references-diff: {} files seen | {} with targets | {} answered files | {} source targets | \
         {} selected | {} answered | {} declined | {} handler panics | {} exact locations \
         ({} definitions, {} uses) | {} divergences | {} alt-binders | {} oracle omissions | \
         {} FCS-not-ok | {} FCS-with-errors | {} unreadable | {} without targets | coverage {}.{}% | \
         corroborated {}.{}%",
        tally.files_seen,
        tally.files_with_targets,
        tally.files_answered,
        tally.source_targets,
        tally.selected_targets,
        tally.answered_targets,
        tally.declined_targets,
        tally.handler_panics,
        tally.exact_locations,
        tally.definition_locations,
        tally.use_locations,
        tally.divergences.len(),
        tally.alt_binders.len(),
        tally.oracle_omissions.len(),
        tally.fcs_not_ok,
        tally.fcs_with_check_errors,
        tally.unreadable,
        tally.files_without_targets,
        answered_permille / 10,
        answered_permille % 10,
        corroborated_permille / 10,
        corroborated_permille % 10,
    );
    print_sites("divergences", &tally.divergences);
    print_sites("same-name alternate binders", &tally.alt_binders);
    print_sites(
        "unobserved ranges in FCS-erroring files",
        &tally.oracle_omissions,
    );

    assert!(
        tally.files_with_targets >= MIN_FILES_WITH_TARGETS,
        "only {} sampled files exposed source targets (floor {MIN_FILES_WITH_TARGETS})",
        tally.files_with_targets,
    );
    assert!(
        tally.files_answered >= MIN_FILES_ANSWERED,
        "only {} sampled files received an answer (floor {MIN_FILES_ANSWERED})",
        tally.files_answered,
    );
    assert!(
        tally.source_targets >= MIN_SOURCE_TARGETS,
        "only {} distinct FCS source targets observed (floor {MIN_SOURCE_TARGETS})",
        tally.source_targets,
    );
    assert!(
        tally.selected_targets >= MIN_SELECTED_TARGETS,
        "only {} valid declaration queries generated (floor {MIN_SELECTED_TARGETS})",
        tally.selected_targets,
    );
    assert!(
        answered_permille >= MIN_ANSWERED_PERMILLE,
        "find-references answered only {}.{}% of selected source targets (floor {}.{}%)",
        answered_permille / 10,
        answered_permille % 10,
        MIN_ANSWERED_PERMILLE / 10,
        MIN_ANSWERED_PERMILLE % 10,
    );
    assert!(
        tally.exact_locations >= MIN_EXACT_LOCATIONS,
        "only {} returned locations were corroborated by FCS (floor {MIN_EXACT_LOCATIONS})",
        tally.exact_locations,
    );
    assert!(
        tally.definition_locations >= MIN_DEFINITION_LOCATIONS,
        "only {} corroborated definition locations (floor {MIN_DEFINITION_LOCATIONS})",
        tally.definition_locations,
    );
    assert!(
        tally.use_locations >= MIN_USE_LOCATIONS,
        "only {} corroborated ordinary-use locations (floor {MIN_USE_LOCATIONS})",
        tally.use_locations,
    );
    assert!(
        corroborated_permille >= MIN_CORROBORATED_PERMILLE,
        "FCS corroborated only {}.{}% of returned locations (floor {}.{}%); isolation-check omissions are masking too much of the differential",
        corroborated_permille / 10,
        corroborated_permille % 10,
        MIN_CORROBORATED_PERMILLE / 10,
        MIN_CORROBORATED_PERMILLE % 10,
    );
    assert_eq!(
        tally.divergences.len(),
        MAX_DIVERGENCES,
        "FCS contradicts the returned symbol at these sites; inspect the printed divergences",
    );
    assert!(
        tally.alt_binders.len() <= MAX_ALT_BINDERS,
        "{} same-name alternate binders exceed the ceiling {MAX_ALT_BINDERS}",
        tally.alt_binders.len(),
    );
    assert_eq!(
        tally.handler_panics, MAX_HANDLER_PANICS,
        "the handler panicked on {} selected declaration queries",
        tally.handler_panics,
    );
}

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
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "target" | "artifacts" | "bin" | "obj")
            ) {
                continue;
            }
            collect_fs(&path, out);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("fs") {
            out.push(path);
        }
    }
}

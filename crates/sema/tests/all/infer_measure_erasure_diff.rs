//! The **unit-of-measure erasure** differential: member typing against FCS over
//! an F#-authored assembly whose members carry units of measure
//! ([`common::measure_corpus`](crate::common::measure_corpus)).
//!
//! IL erases units of measure, so a member's IL signature says `float64` for a
//! `float<m>` and names a measure-parameterised `Vec<'u>` as the bare `Vec`. The
//! member wake types an access off that signature, so for every measure-bearing
//! member it would publish the *erased* type — a well-formed answer for a
//! different type, served by hover and fed to every later inference step.
//!
//! # The properties
//!
//! 1. **Soundness.** Every type we publish where FCS has a node is FCS's type,
//!    and every site where we publish a type is one FCS accepted with a node at
//!    that range. A site FCS rejects carries neither a type nor a member
//!    resolution of ours.
//! 2. **The fixture says what it claims.** Each cell's claim is checked against
//!    the oracle rather than trusted: a measure-bearing shape is one FCS types
//!    differently from its IL erasure, a measure-free one is one FCS types
//!    exactly as its erasure, and an accepted site is one FCS checks without
//!    error. Without this, a shape FCS normalised to its erasure (so a
//!    measure-blind answer would be *right*) would sit in the measure-bearing
//!    column and be graded as a decline we owe nothing for.
//! 3. **Every carrier is reached.** The measure-free controls of each carrier
//!    commit their erased type. Properties 1 and 2 are both satisfied by an
//!    engine that answers nothing, so without this a carrier the wake stopped
//!    reaching would leave every measure cell "passing" while testing nothing.

use std::collections::{HashMap, HashSet};

use crate::common::measure_corpus::{CARRIERS, Corpus, Expect, Site, corpus};
use crate::common::{
    FcsTypeMap, ensure_fsharp_core_dll, ensure_measure_fixture_built, ensure_system_runtime_dll,
    invoke_fcs_dump_with_refs, parse_fcs_types_with_errors, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, InferredFile, ProjectItems, SyntaxRecovery, infer_file, resolve_file,
};

struct Run {
    corpus: Corpus,
    inferred: InferredFile,
    /// The lines on which we recorded a member resolution.
    member_lines: HashSet<usize>,
    fcs_types: FcsTypeMap,
    error_lines: HashMap<usize, Vec<String>>,
}

fn line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        src.bytes()
            .enumerate()
            .filter(|&(_, b)| b == b'\n')
            .map(|(i, _)| i + 1),
    );
    starts
}

fn line_at(starts: &[usize], at: usize) -> usize {
    match starts.binary_search(&at) {
        Ok(i) => i + 1,
        Err(i) => i,
    }
}

fn run() -> Run {
    let corpus = corpus();
    let dll = ensure_measure_fixture_built();
    let system_runtime = ensure_system_runtime_dll();
    // The oracle checks the probe as a script against the framework and
    // FSharp.Core, so our env carries the same, plus `netstandard`, through which
    // FSharp.Core's pickle names its BCL abbreviation targets.
    let netstandard = system_runtime
        .parent()
        .expect("System.Runtime.dll sits in a ref dir")
        .join("netstandard.dll");
    let bcl_bytes = std::fs::read(&system_runtime).expect("read System.Runtime.dll");
    let netstandard_bytes = std::fs::read(&netstandard).expect("read netstandard.dll");
    let core_bytes = std::fs::read(ensure_fsharp_core_dll()).expect("read FSharp.Core.dll");
    let fixture_bytes = std::fs::read(dll).expect("read the measure fixture dll");
    let env = AssemblyEnv::from_views(&[
        Ecma335Assembly::parse(&bcl_bytes).expect("parse System.Runtime.dll"),
        Ecma335Assembly::parse(&netstandard_bytes).expect("parse netstandard.dll"),
        Ecma335Assembly::parse(&core_bytes).expect("parse FSharp.Core.dll"),
        Ecma335Assembly::parse(&fixture_bytes).expect("parse the measure fixture dll"),
    ])
    .expect("build AssemblyEnv");

    let parsed = parse(&corpus.probe);
    assert!(
        parsed.errors.is_empty(),
        "the generated probe must parse cleanly: {:?}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
    let inferred = infer_file(&file, &resolved, &env);

    let starts = line_starts(&corpus.probe);
    let member_lines = inferred
        .member_resolutions()
        .keys()
        .map(|range| line_at(&starts, usize::from(range.start())))
        .collect();

    let path = temp_fs_file("measure_erasure", &corpus.probe);
    let types_json = invoke_fcs_dump_with_refs("types", &path, &[dll]);
    let _ = std::fs::remove_file(&path);
    let (fcs_types, errors) = parse_fcs_types_with_errors(&types_json, &corpus.probe);
    let mut error_lines: HashMap<usize, Vec<String>> = HashMap::new();
    for e in errors {
        error_lines
            .entry(e.line as usize)
            .or_default()
            .push(format!("FS{:04}: {}", e.code, e.message));
    }

    Run {
        corpus,
        inferred,
        member_lines,
        fcs_types,
        error_lines,
    }
}

impl Run {
    /// The type we published at `range`, if any.
    fn our_type(&self, range: (usize, usize)) -> Option<String> {
        self.inferred
            .types()
            .iter()
            .find(|(r, _)| (usize::from(r.start()), usize::from(r.end())) == range)
            .map(|(_, ty)| ty.render())
    }
}

/// Property 1: every type we publish is FCS's, and a rejected site carries
/// nothing of ours.
#[test]
fn every_published_type_is_the_one_fcs_infers() {
    let run = run();
    let mut wrong: Vec<String> = Vec::new();
    for (range, ty) in run.inferred.types() {
        let key = (usize::from(range.start()), usize::from(range.end()));
        if let Some(fcs) = run.fcs_types.get(&key)
            && *fcs != ty.render()
        {
            let text = &run.corpus.probe[key.0..key.1];
            wrong.push(format!(
                "  `{text}` {key:?}: we typed `{}`, FCS says `{fcs}`",
                ty.render()
            ));
        }
    }
    for site in &run.corpus.sites {
        let ours = run.our_type(site.access);
        match site.expect {
            Expect::Accepts { .. } => {
                if let Some(ours) = ours
                    && !run.fcs_types.contains_key(&site.access)
                {
                    wrong.push(format!(
                        "  {}: we typed the access `{ours}` but FCS has no node there",
                        site.label
                    ));
                }
            }
            Expect::Rejects => {
                if let Some(ours) = ours {
                    wrong.push(format!(
                        "  {}: FCS rejects the line, we typed the access `{ours}`",
                        site.label
                    ));
                }
                if run.member_lines.contains(&site.line) {
                    wrong.push(format!(
                        "  {}: FCS rejects the line, we recorded a member resolution on it",
                        site.label
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} answer(s) FCS does not confirm:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// Whether FCS's rendering of a type mentions a unit of measure — the fixture's
/// own units, a measure variable, or the dimensionless `1` FCS defaults an
/// unconstrained measure variable to.
fn mentions_a_measure(fcs: &str) -> bool {
    fcs.contains("FSharpMeasure.m")
        || fcs.contains("<measure:")
        || fcs.contains("Microsoft.FSharp.Core.CompilerServices.MeasureOne")
}

/// Property 2: each cell's claim about FCS holds.
#[test]
fn every_cell_is_what_it_claims_to_be() {
    let run = run();
    let mut bad: Vec<String> = Vec::new();
    for site in &run.corpus.sites {
        let errors = run.error_lines.get(&site.line);
        match site.expect {
            Expect::Rejects => {
                if errors.is_none() {
                    bad.push(format!(
                        "  {}: claimed rejected, FCS checks `{}` cleanly",
                        site.label,
                        site.text.trim_end()
                    ));
                }
            }
            Expect::Accepts { measure_bearing } => {
                if let Some(errors) = errors {
                    bad.push(format!(
                        "  {}: claimed accepted, FCS reports {errors:?}",
                        site.label
                    ));
                    continue;
                }
                let Some(fcs) = run.fcs_types.get(&site.access) else {
                    bad.push(format!(
                        "  {}: FCS has no node at the access `{}`",
                        site.label,
                        &run.corpus.probe[site.access.0..site.access.1]
                    ));
                    continue;
                };
                // A shape × carrier cell is graded against its own erasure; an
                // extra cell against the presence of a measure in FCS's type.
                let bearing = match &site.erased {
                    Some(erased) => fcs != erased,
                    None => mentions_a_measure(fcs),
                };
                if bearing != measure_bearing {
                    bad.push(format!(
                        "  {}: claimed measure_bearing = {measure_bearing}, FCS types the \
                         access `{fcs}` (IL erasure {:?})",
                        site.label, site.erased
                    ));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} cell(s) are not what the corpus says they are:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// Property 3: every carrier inference reaches commits its measure-free
/// controls.
#[test]
fn every_carrier_commits_its_measure_free_controls() {
    let run = run();
    let mut missing: Vec<String> = Vec::new();
    for site in run
        .corpus
        .sites
        .iter()
        .filter(|s| s.is_control && s.inferred)
    {
        let erased = site
            .erased
            .as_deref()
            .expect("a control is a shape × carrier cell");
        match run.our_type(site.access) {
            Some(ours) if ours == erased => {}
            other => missing.push(format!(
                "  {}: expected the control to commit `{erased}`, got {other:?}",
                site.label
            )),
        }
    }
    assert!(
        missing.is_empty(),
        "{} control(s) did not commit, so the measure cells beside them on the same \
         carrier test nothing:\n{}",
        missing.len(),
        missing.join("\n")
    );
    // Every carrier has at least one control, or its column is untested.
    for carrier in CARRIERS.iter().filter(|c| c.inferred) {
        assert!(
            run.corpus
                .sites
                .iter()
                .any(|s: &Site| s.is_control && s.label.starts_with(carrier.label)),
            "carrier {} has no measure-free control",
            carrier.label
        );
    }
}

/// [`borzoi_sema::fsharp_core_intrinsic_tycons`] is exactly the set of FSharp.Core types its
/// shipped pickle declares with an IL-assembly representation and no
/// abbreviation — the ones no env entity answers for. A name added to the list
/// that is not one would clear a head without proof; one missing would decline
/// every member type applying it.
#[test]
fn fsharp_core_intrinsic_tycons_are_exactly_its_asm_reprs() {
    use borzoi_assembly::fsharp_pickle::model::PickledTyconRepr;
    use borzoi_assembly::{EcmaView, ResourceKind, unpickle_signature};
    let bytes = std::fs::read(ensure_fsharp_core_dll()).expect("read FSharp.Core.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core.dll");
    let resources = view.fsharp_resources().expect("FSharp.Core's F# resources");
    let primary = resources
        .iter()
        .find(|r| {
            matches!(
                r.kind,
                ResourceKind::SignatureData
                    | ResourceKind::SignatureCompressedData
                    | ResourceKind::SignatureDataFSharpCore
            )
        })
        .expect("FSharp.Core carries a signature pickle");
    let b = resources
        .iter()
        .find(|r| {
            matches!(
                r.kind,
                ResourceKind::SignatureDataB | ResourceKind::SignatureCompressedDataB
            )
        })
        .map(|r| r.payload.as_slice());
    let ccu = unpickle_signature(&primary.payload, b).expect("unpickle FSharp.Core");
    let mut found: Vec<(Vec<String>, String)> = Vec::new();
    let mut stack: Vec<(u32, Vec<String>)> = vec![(ccu.root_entity, Vec::new())];
    while let Some((stamp, path)) = stack.pop() {
        let entity = &ccu.tables.tycons[stamp as usize];
        if matches!(entity.repr, PickledTyconRepr::AsmRepr(_)) && entity.type_abbrev.is_none() {
            found.push((path.clone(), entity.logical_name.clone()));
        }
        let mut child_path = path.clone();
        if stamp != ccu.root_entity {
            child_path.push(entity.logical_name.clone());
        }
        for &child in &entity.module_type.entities {
            stack.push((child, child_path.clone()));
        }
    }
    found.sort();
    let core = ["Microsoft", "FSharp", "Core"].map(String::from).to_vec();
    let names: std::collections::BTreeSet<&str> = found
        .iter()
        .inspect(|(ns, name)| {
            assert_eq!(
                ns, &core,
                "FSharp.Core declares the intrinsic {name} outside Microsoft.FSharp.Core"
            )
        })
        .map(|(_, name)| name.as_str())
        .collect();
    let listed: std::collections::BTreeSet<&str> = borzoi_sema::fsharp_core_intrinsic_tycons()
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(names, listed);
}

/// The per-cell table — a measurement, not a gate.
#[test]
#[ignore = "report generator"]
fn verdict_report() {
    let run = run();
    for site in &run.corpus.sites {
        let ours = run.our_type(site.access).unwrap_or_else(|| "—".to_string());
        let fcs = run
            .fcs_types
            .get(&site.access)
            .cloned()
            .unwrap_or_else(|| "—".to_string());
        let errs = run
            .error_lines
            .get(&site.line)
            .map(|e| format!(" ERR {e:?}"))
            .unwrap_or_default();
        println!("{:<70} ours={:<24} fcs={}{errs}", site.label, ours, fcs);
    }
}

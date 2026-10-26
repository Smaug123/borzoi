//! The **unit-of-measure** differential for reading an F# module function's IL
//! signature as its F# type (`function_ty::module_function_scheme`), over a
//! generated fixture assembly ([`common::measure_fn_corpus`](crate::common::measure_fn_corpus)).
//!
//! IL erases units of measure, and a measure can hide behind any named type the
//! signature pickle mentions — an abbreviation in the function's own assembly or
//! another one, possibly several levels deep — so the reading is sound only if
//! it proves every such name measure-free before it trusts the IL.
//!
//! The reading is graded **directly**, through the same entry point the
//! FSharp.Core signature differential uses
//! (`every_read_fsharp_core_signature_is_the_type_fcs_gives_it`): inference
//! reaches it today only for FSharp.Core's own operators, so an application of a
//! fixture function in a probe would decline before the reading is consulted,
//! and test nothing.
//!
//! # The properties
//!
//! 1. **Soundness** (certain-implies-exact). Every function the reading
//!    admits has exactly the type FCS gives it bound to a value, and an
//!    application of it to all its arguments has exactly the type FCS gives
//!    that application.
//! 2. **The fixture says what it claims.** FCS checks the probe cleanly, and
//!    each row's value type mentions a unit of measure iff the row says it is
//!    measure-bearing. Without this a shape FCS normalised to its erasure would
//!    sit in the measure column and be graded as a decline we owe nothing for.
//! 3. **Non-vacuity.** Every measure-free control commits, in every position.
//!    Properties 1 and 2 are both satisfied by a reading that admits nothing.

use std::collections::HashMap;

use crate::common::measure_fn_corpus::{Corpus, MODULE, NS, POSITIONS, corpus};
use crate::common::{
    FcsTypeMap, ensure_fsharp_core_dll, ensure_measure_fn_fixture_built, ensure_system_runtime_dll,
    invoke_fcs_dump_with_refs, parse_fcs_binder_types_with_errors, temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_sema::AssemblyEnv;

struct Run {
    corpus: Corpus,
    /// Each function's value type as the reading gives it, by name; absent when
    /// the reading declines.
    value_types: HashMap<String, String>,
    /// Each row's application type as the reading gives it, by name.
    applied_types: HashMap<String, String>,
    fcs: FcsTypeMap,
    errors: Vec<String>,
}

fn run() -> Run {
    let corpus = corpus();
    let (fixture, base) = ensure_measure_fn_fixture_built();
    let system_runtime = ensure_system_runtime_dll();
    // The oracle checks the probe as a script against the framework and
    // FSharp.Core, so our env carries the same, plus `netstandard`, through which
    // FSharp.Core's pickle names its BCL abbreviation targets.
    let netstandard = system_runtime
        .parent()
        .expect("System.Runtime.dll sits in a ref dir")
        .join("netstandard.dll");
    let read = |p: &std::path::Path| {
        std::fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
    };
    let bytes = [
        read(&system_runtime),
        read(&netstandard),
        read(&ensure_fsharp_core_dll()),
        read(fixture),
        read(base),
    ];
    let views: Vec<_> = bytes
        .iter()
        .map(|b| Ecma335Assembly::parse(b).expect("parse a reference"))
        .collect();
    let env = AssemblyEnv::from_views(&views).expect("build AssemblyEnv");
    let module = env
        .lookup_type(&[NS.to_owned()], MODULE, 0)
        .unwrap_or_else(|| panic!("the fixture declares {NS}.{MODULE}"));

    let value_types: HashMap<String, String> =
        borzoi_sema::test_support::module_function_types(&env, module)
            .into_iter()
            .collect();
    let applied_types = corpus
        .rows
        .iter()
        .filter(|r| r.applied_binder.is_some())
        .filter_map(|r| {
            borzoi_sema::test_support::module_function_application_type(
                &env,
                module,
                &r.name,
                r.applied_arity,
            )
            .map(|ty| (r.name.clone(), ty))
        })
        .collect();

    let path = temp_fs_file("module_function_measures", &corpus.probe);
    let json = invoke_fcs_dump_with_refs("binder-types", &path, &[fixture, base]);
    let _ = std::fs::remove_file(&path);
    let (fcs, errors) = parse_fcs_binder_types_with_errors(&json, &corpus.probe);
    let errors = errors
        .into_iter()
        .map(|e| format!("line {}: FS{:04}: {}", e.line, e.code, e.message))
        .collect();
    Run {
        corpus,
        value_types,
        applied_types,
        fcs,
        errors,
    }
}

/// Property 1: what the reading admits is what FCS infers.
#[test]
fn every_admitted_module_function_has_the_type_fcs_gives_it() {
    let run = run();
    let mut wrong = Vec::new();
    for row in &run.corpus.rows {
        if let Some(ours) = run.value_types.get(&row.name) {
            match run.fcs.get(&row.value_binder) {
                Some(fcs) if fcs == ours => {}
                fcs => wrong.push(format!(
                    "  {} ({}) as a value: we read `{ours}`, FCS says {fcs:?}",
                    row.label, row.name
                )),
            }
        }
        if let (Some(ours), Some(binder)) = (run.applied_types.get(&row.name), row.applied_binder) {
            match run.fcs.get(&binder) {
                Some(fcs) if fcs == ours => {}
                fcs => wrong.push(format!(
                    "  {} ({}) applied: we read `{ours}`, FCS says {fcs:?}",
                    row.label, row.name
                )),
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} reading(s) FCS contradicts:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// Whether FCS's rendering of a type mentions a unit of measure: the fixture's
/// own units or a measure expression FCS builds from them.
fn mentions_a_measure(fcs: &str) -> bool {
    fcs.contains("FSharpMeasureFn.m")
        || fcs.contains("FSharpMeasureFn.s")
        || fcs.contains("FSharpMeasureFnBase.bm")
        || fcs.contains("Microsoft.FSharp.Core.CompilerServices.Measure")
        || fcs.contains("<measure:")
}

/// Property 2: FCS checks the probe cleanly, and each row's claim holds.
#[test]
fn every_row_is_what_it_claims_to_be() {
    let run = run();
    assert!(
        run.errors.is_empty(),
        "FCS rejects the generated probe:\n{}\n{}",
        run.errors.join("\n"),
        run.corpus.probe
    );
    let mut bad = Vec::new();
    for row in &run.corpus.rows {
        let Some(fcs) = run.fcs.get(&row.value_binder) else {
            bad.push(format!(
                "  {}: FCS has no binder `v` for {}",
                row.label, row.name
            ));
            continue;
        };
        if mentions_a_measure(fcs) != row.measure_bearing {
            bad.push(format!(
                "  {}: claimed measure_bearing = {}, FCS types {} as `{fcs}`",
                row.label, row.measure_bearing, row.name
            ));
        }
        if let Some(binder) = row.applied_binder
            && !run.fcs.contains_key(&binder)
        {
            bad.push(format!(
                "  {}: FCS has no binder `a` for {}",
                row.label, row.name
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} row(s) are not what the corpus says they are:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// Property 3: every measure-free control commits, as a value and applied.
#[test]
fn every_measure_free_control_commits() {
    let run = run();
    let mut missing = Vec::new();
    for row in run.corpus.rows.iter().filter(|r| r.is_control) {
        if !run.value_types.contains_key(&row.name) {
            missing.push(format!("  {} ({}) as a value", row.label, row.name));
        }
        if row.applied_binder.is_some() && !run.applied_types.contains_key(&row.name) {
            missing.push(format!("  {} ({}) applied", row.label, row.name));
        }
    }
    assert!(
        missing.is_empty(),
        "{} control(s) did not commit, so the measure rows beside them test nothing:\n{}",
        missing.len(),
        missing.join("\n")
    );
    for position in POSITIONS {
        assert!(
            run.corpus
                .rows
                .iter()
                .any(|r| r.is_control && r.label.starts_with(position.label)),
            "position {} has no measure-free control",
            position.label
        );
    }
}

/// The per-row table — a measurement, not a gate.
#[test]
#[ignore = "report generator"]
fn verdict_report() {
    let run = run();
    for row in &run.corpus.rows {
        let ours = run.value_types.get(&row.name).map_or("—", String::as_str);
        let fcs = run.fcs.get(&row.value_binder).map_or("—", String::as_str);
        println!(
            "{:<5} {:<60} ours={ours:<40} fcs={fcs}",
            row.name, row.label
        );
    }
}

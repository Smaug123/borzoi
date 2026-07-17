//! ECMA-335 II.7.1.1, checked as a metamorphic property against real
//! assemblies rather than hand-written expectations.
//!
//! See `crate::modifier_metamorphic` for the statement of the two properties
//! and why they are shaped this way. This file is only the corpus:
//!
//! * the C# and F# `MiniLib` fixtures — cheap, always built, and the place a
//!   regression will first show up in a normal `cargo test`;
//! * the SDK's `FSharp.Core` — the assembly the LSP reads on every project;
//! * the whole `Microsoft.NETCore.App.Ref` pack — ~170 assemblies, every
//!   signature shape a real .NET build can put in front of the projector.
//!
//! The pack sweep is the exhaustive one: it drives an unrecognised modifier in
//! front of *every node* of *every signature* in the BCL, which is precisely the
//! search a reviewer would otherwise have to do by hand, one guard at a time.

use borzoi_assembly::modifier_metamorphic::{
    HostileShape, ProbeOutcome, modopt_saturation_is_inert, modopt_saturation_is_inert_on,
    unknown_modreq_on_constraints_refuses, unknown_modreq_on_members_refuses,
    unknown_modreq_saturation_refuses,
};

use crate::common::{ensure_minilib_built, ensure_minilib_fs_built, ensure_sdk_fsharp_core};

/// Panic with the findings, if any. `label` names the assembly.
fn expect_clean(label: &str, outcome: ProbeOutcome) {
    assert!(
        outcome.baseline_members > 0,
        "{label}: probe is vacuous — the baseline projection kept no members"
    );
    assert!(
        outcome.findings.is_empty(),
        "{label}: {} metamorphic finding(s) over {} types / {} members:\n{}",
        outcome.findings.len(),
        outcome.baseline_types,
        outcome.baseline_members,
        outcome
            .findings
            .iter()
            .take(20)
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn probe_both(label: &str, path: &std::path::Path) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    expect_clean(
        &format!("{label} (P1: modopt inert)"),
        modopt_saturation_is_inert(&bytes).unwrap_or_else(|e| panic!("{label}: P1 probe: {e}")),
    );
    expect_clean(
        &format!("{label} (P2: unknown modreq refused)"),
        unknown_modreq_saturation_refuses(&bytes)
            .unwrap_or_else(|e| panic!("{label}: P2 probe: {e}")),
    );
}

/// The F# assemblies refuse *wholesale* under P2 — the decoration reaches the
/// `FSharpInterfaceDataVersionAttribute` constructor, and an assembly whose
/// F#-ness marker will not decode is refused rather than mis-classified. Pin
/// that, so the day it silently starts enumerating instead (which would mean a
/// `modreq` was ignored somewhere upstream) this test says so.
fn expect_wholesale_refusal(label: &str, path: &std::path::Path) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let outcome = unknown_modreq_saturation_refuses(&bytes)
        .unwrap_or_else(|e| panic!("{label}: P2 probe: {e}"));
    assert!(
        outcome.refused_wholesale,
        "{label}: expected the forged `modreq` on the F#-ness marker to refuse \
         the assembly outright"
    );
    expect_clean(&format!("{label} (P2: unknown modreq refused)"), outcome);
}

#[test]
fn minilib_csharp_obeys_the_modifier_rule() {
    probe_both("MiniLib (C#)", ensure_minilib_built());
}

#[test]
fn minilib_fsharp_obeys_the_modifier_rule() {
    let path = ensure_minilib_fs_built();
    expect_clean(
        "MiniLib (F#) (P1: modopt inert)",
        modopt_saturation_is_inert(&std::fs::read(path).unwrap()).expect("P1 probe"),
    );
    expect_wholesale_refusal("MiniLib (F#)", path);
}

#[test]
fn fsharp_core_obeys_the_modifier_rule() {
    let (dll, _) = ensure_sdk_fsharp_core();
    expect_clean(
        "FSharp.Core (P1: modopt inert)",
        modopt_saturation_is_inert(&std::fs::read(&dll).unwrap()).expect("P1 probe"),
    );
    expect_wholesale_refusal("FSharp.Core", &dll);
}

/// P2, aimed at the *member* signatures, with the enclosing type's own signatures
/// left undecorated.
///
/// This is where the survivor check has its teeth. Under whole-image saturation a
/// decorated `extends` refuses nearly every concrete type before a single member
/// is projected, so "no member survived" would hold even if every member-position
/// modifier check were broken. Leave the type header alone and each member path
/// has to refuse on its own.
#[test]
fn an_unknown_modreq_on_a_member_refuses_that_member() {
    let fsharp_core = ensure_sdk_fsharp_core().0;
    let corpus: [(&str, &std::path::Path); 3] = [
        ("MiniLib (C#)", ensure_minilib_built()),
        ("MiniLib (F#)", ensure_minilib_fs_built()),
        ("FSharp.Core", &fsharp_core),
    ];
    for (label, path) in corpus {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let outcome = unknown_modreq_on_members_refuses(&bytes)
            .unwrap_or_else(|e| panic!("{label}: probe: {e}"));
        // An F#-kinded image still refuses wholesale here: its F#-ness marker's
        // constructor is a *method*, so decorating member signatures reaches it.
        if outcome.refused_wholesale {
            continue;
        }
        expect_clean(&format!("{label} (P2: modreq on a member)"), outcome);
    }
}

/// P2, aimed at the one path that recognises a `modreq` *positionally*: the
/// `unmanaged` marker on a generic-parameter constraint, which the projector
/// consumes rather than surfaces. Decorating only the constraints (so nothing
/// else drops the type first) pins that consuming the marker does not take an
/// unrecognised `modreq` down with it.
///
/// The saturating probe cannot see this — under it the owning type drops because
/// its *base type* was decorated too, whatever the constraint path did.
#[test]
fn an_unknown_modreq_on_a_constraint_refuses_its_declarer() {
    let fsharp_core = ensure_sdk_fsharp_core().0;
    let corpus: [(&str, &std::path::Path); 3] = [
        ("MiniLib (C#)", ensure_minilib_built()),
        ("MiniLib (F#)", ensure_minilib_fs_built()),
        ("FSharp.Core", &fsharp_core),
    ];
    for (label, path) in corpus {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        expect_clean(
            &format!("{label} (P2: modreq on a constraint)"),
            unknown_modreq_on_constraints_refuses(&bytes)
                .unwrap_or_else(|e| panic!("{label}: probe: {e}")),
        );
    }
}

/// P1 over the *defensive* guards: mutate each assembly into a shape the
/// projector is supposed to refuse (a byref event/field/property type — none of
/// which any compiler emits, which is exactly why they were never covered), and
/// assert the refusal is stable under an ignorable modifier.
///
/// A guard that inspects the head of a signature without peeling silently stops
/// firing when a `modopt` sits in front of the byref, and the member it should
/// have refused is projected instead. That is the bug class this whole file
/// exists for; here it is, reachable.
#[test]
fn hostile_shapes_are_refused_stably_under_a_modopt() {
    let fsharp_core = ensure_sdk_fsharp_core().0;
    let corpus: [(&str, &std::path::Path); 3] = [
        ("MiniLib (C#)", ensure_minilib_built()),
        ("MiniLib (F#)", ensure_minilib_fs_built()),
        ("FSharp.Core", &fsharp_core),
    ];
    for (label, path) in corpus {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        for shape in HostileShape::ALL {
            if shape == HostileShape::None {
                continue; // the undisturbed image is the test above.
            }
            let outcome = modopt_saturation_is_inert_on(&bytes, shape)
                .unwrap_or_else(|e| panic!("{label} {shape:?}: probe: {e}"));
            expect_clean(
                &format!("{label} under {shape:?} (P1: modopt inert)"),
                outcome,
            );
        }
    }
}

/// The *absolute* companion to the invariance properties, and the one thing they
/// structurally cannot do.
///
/// P1 says the two projections agree. Two projections that both wrongly *accept*
/// a shape agree perfectly — so invariance cannot see a guard that is simply
/// **gone**. (It was: while porting the encoding, the byref synthetic-field guard
/// was deleted outright in a botched edit, and every metamorphic assertion still
/// passed. GPT-5.6 caught it by reading. This test catches it by running.)
///
/// So pin the guards themselves: build the shape they exist to refuse, and assert
/// the projector *refuses it*. The shapes come from the same model-mutation seam
/// as the probes, which is what makes them reachable at all — no metadata emitter
/// can forge an F#-kinded image with a byref logical field.
///
/// Note what is *not* here. A byref **field** is perfectly legal (a `ref` field in
/// a `ref struct`), and so is a byref-returning C# property — those shapes are
/// still worth running through P1 (they exercise the byref paths under a modifier)
/// but they carry no refusal expectation, and asserting one would be wrong.
#[test]
fn the_defensive_guards_actually_refuse() {
    let fsharp_core = ensure_sdk_fsharp_core().0;
    let cases: [(&str, &std::path::Path, HostileShape); 3] = [
        // No delegate type can be a byref: `project_event` refuses it.
        (
            "MiniLib (C#)",
            ensure_minilib_built(),
            HostileShape::ByRefEventTypes,
        ),
        // An F# record/exception logical field is a *property* carrying the field
        // flag, and F# cannot declare a byref one: `property_as_synthetic_field`
        // refuses it rather than synthesise a byref field.
        (
            "MiniLib (F#)",
            ensure_minilib_fs_built(),
            HostileShape::ByRefPropertyTypes,
        ),
        (
            "FSharp.Core",
            &fsharp_core,
            HostileShape::ByRefPropertyTypes,
        ),
    ];
    for (label, path, shape) in cases {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let outcome = modopt_saturation_is_inert_on(&bytes, shape)
            .unwrap_or_else(|e| panic!("{label} {shape:?}: probe: {e}"));
        assert!(
            outcome.baseline_refusals > 0,
            "{label}: the projector accepted {shape:?}, a shape it is supposed to \
             refuse — {} types / {} members projected and not one refusal recorded",
            outcome.baseline_types,
            outcome.baseline_members,
        );
    }
}

/// The exhaustive sweep: every reference assembly in the pack, both
/// properties. This is the one that would have caught the four
/// head-inspecting guards without a reviewer having to think of them.
#[test]
fn ref_pack_obeys_the_modifier_rule() {
    let dir = crate::common::sdk_ref_pack_dir();
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read ref pack dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dll"))
        .collect();
    paths.sort();
    assert!(paths.len() > 100, "ref pack looks empty: {dir:?}");

    let mut probed = 0usize;
    let mut findings: Vec<String> = Vec::new();
    for path in &paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        // A reference assembly the *baseline* cannot parse is out of scope
        // here — `bcl_ref_pack_sweep` is the test that owns that budget.
        let Ok(p1) = modopt_saturation_is_inert(&bytes) else {
            continue;
        };
        let Ok(p2) = unknown_modreq_saturation_refuses(&bytes) else {
            continue;
        };
        let Ok(p2c) = unknown_modreq_on_constraints_refuses(&bytes) else {
            continue;
        };
        let Ok(p2m) = unknown_modreq_on_members_refuses(&bytes) else {
            continue;
        };
        if p1.baseline_members == 0 {
            continue;
        }
        probed += 1;
        // A reference assembly is C#-kinded, so P2 must enumerate and drop
        // member by member rather than refusing the assembly: this is where the
        // survivor check has its teeth.
        if p2.refused_wholesale {
            findings.push(format!(
                "{name} [P2] refused the whole assembly; expected per-member drops"
            ));
        }
        findings.extend(p1.findings.iter().map(|f| format!("{name} [P1] {f}")));
        findings.extend(p2.findings.iter().map(|f| format!("{name} [P2] {f}")));
        findings.extend(
            p2c.findings
                .iter()
                .map(|f| format!("{name} [P2-constraints] {f}")),
        );
        // The member paths, unmasked by the type header. A reference assembly is
        // C#-kinded, so it must enumerate and drop member by member.
        if p2m.refused_wholesale {
            findings.push(format!(
                "{name} [P2-members] refused the whole assembly; expected per-member drops"
            ));
        }
        findings.extend(
            p2m.findings
                .iter()
                .map(|f| format!("{name} [P2-members] {f}")),
        );
    }

    assert!(probed > 100, "only {probed} pack assemblies had members");
    assert!(
        findings.is_empty(),
        "{} metamorphic finding(s) across {probed} reference assemblies \
         (first 30 shown):\n{}",
        findings.len(),
        findings
            .iter()
            .take(30)
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

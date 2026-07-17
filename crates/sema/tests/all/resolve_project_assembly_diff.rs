//! Differential test: our *cross-file* resolution **into a referenced
//! assembly** against FCS, via the project-aware `uses-project` oracle with the
//! fixture assembly referenced (`BORZOI_FCS_EXTRA_REFS`).
//!
//! This is the oracle the single-file `resolve_assembly_diff.rs` (one file +
//! assembly) and the ref-less `resolve_project_diff.rs` (many files, no
//! assembly) each cover only half of. Its absence is why the project-prefix
//! over-defers went unnoticed: F# *merges* a project module header with a
//! same-named assembly namespace, so a path under a project module whose tail
//! the module does not provide falls through to the assembly — which only a
//! project-**and**-assembly oracle can observe.
//!
//! The property (design-doc D7): for every use FCS resolves *into the fixture
//! assembly* (`Assembly == SemaAssemblyEnvFixture`), our resolution is an
//! `Entity`/`Member` with the same `(assembly, full name)` **or** honestly
//! `Deferred`/unrecorded — never `Unresolved`, a `Local`/`Item`, or a wrong
//! entity. A per-project count of expected assembly resolutions keeps it from
//! passing vacuously.

use crate::common::{
    ensure_assembly_fixture_built, invoke_fcs_dump_project_with_refs, parse_fcs_uses_project,
    temp_fs_file,
};
use borzoi_assembly::{Ecma335Assembly, Member};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project};
use rowan::TextRange;

/// The fixture's assembly simple name (its `<AssemblyName>`).
const FIXTURE_ASM: &str = "SemaAssemblyEnvFixture";

fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

fn member_name(m: &Member) -> &str {
    match m {
        Member::Method(x) => &x.name,
        Member::Field(x) => &x.name,
        Member::Property(x) => &x.name,
        Member::Event(x) => &x.name,
    }
}

/// The `(assembly, full name)` our resolution names, for comparison with FCS.
fn our_assembly_full(env: &AssemblyEnv, res: Resolution) -> (String, String) {
    fn full(ns: &[String], name: &str) -> String {
        if ns.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", ns.join("."), name)
        }
    }
    match res {
        Resolution::Entity(h) => {
            let e = env.entity(h);
            (e.assembly.name.clone(), full(&e.namespace, &e.name))
        }
        Resolution::Member { parent, idx } => {
            let e = env.entity(parent);
            (
                e.assembly.name.clone(),
                format!(
                    "{}.{}",
                    full(&e.namespace, &e.name),
                    member_name(env.member_at(parent, idx))
                ),
            )
        }
        _ => unreachable!("only Entity/Member reach here"),
    }
}

/// One project: `(label, source)` per file in Compile order, and the number of
/// uses the project should resolve *into the fixture assembly*.
struct Project {
    files: Vec<(&'static str, &'static str)>,
    expected_assembly: usize,
}

fn assert_matches_fcs(project: &Project) {
    let fixture = ensure_assembly_fixture_built();

    // Materialise the files (FCS reads them from disk).
    let written: Vec<(std::path::PathBuf, String)> = project
        .files
        .iter()
        .map(|(label, src)| (temp_fs_file(label, src), (*src).to_string()))
        .collect();
    let paths: Vec<&std::path::Path> = written.iter().map(|(p, _)| p.as_path()).collect();

    let json = invoke_fcs_dump_project_with_refs(&paths, &[fixture]);
    let fcs_files = parse_fcs_uses_project(&json, &written);

    // Our resolution over the same Compile-ordered sources, against the fixture.
    let bytes = std::fs::read(fixture).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");
    let asts: Vec<ImplFile> = written.iter().map(|(_, src)| impl_file(src)).collect();
    let proj = resolve_project(&asts, &env);

    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }

    let mut agreed = 0usize;
    for (i, (path, _src)) in written.iter().enumerate() {
        let fu = fcs_files
            .iter()
            .find(|f| f.path.file_name() == path.file_name())
            .unwrap_or_else(|| panic!("FCS reported no uses for {path:?}"));
        let rf = proj.file(i);

        for u in &fu.uses {
            if u.start == u.end {
                continue;
            }
            // Only uses FCS resolves *into the fixture assembly* are in scope.
            if u.assembly.as_deref() != Some(FIXTURE_ASM) {
                continue;
            }
            match rf.resolution_at(span(u.start, u.end)) {
                // Honest "say nothing" — e.g. a namespace qualifier we don't model.
                None | Some(Resolution::Deferred(_)) => {}
                Some(res @ (Resolution::Entity(_) | Resolution::Member { .. })) => {
                    let (asm, full) = our_assembly_full(&env, res);
                    assert_eq!(
                        Some(asm.as_str()),
                        u.assembly.as_deref(),
                        "use {:?} in {path:?}: assembly mismatch",
                        u.name
                    );
                    assert_eq!(
                        Some(full.as_str()),
                        u.full_name.as_deref(),
                        "use {:?} in {path:?}: full-name mismatch",
                        u.name
                    );
                    agreed += 1;
                }
                other => panic!(
                    "use {:?} at {}..{} in {path:?} resolves into the fixture assembly, \
                     but we gave {other:?}",
                    u.name, u.start, u.end
                ),
            }
        }
    }

    assert_eq!(
        agreed, project.expected_assembly,
        "assembly resolutions agreed for {:?}",
        project.files
    );
}

#[test]
fn cross_file_assembly_resolution_agrees_with_fcs() {
    let corpus = [
        // A project module header (`module Demo`) and the assembly namespace
        // `Demo` *merge*: a later file's direct path under `Demo` whose tail the
        // module does not provide falls through to the assembly. `Demo.Sub.Calc`
        // (Entity) + `.Zero` (Member) = 2.
        Project {
            files: vec![
                ("pa_modDemo_1", "module Demo\nlet placeholder = 1\n"),
                (
                    "pa_modDemo_2",
                    "module Other\nlet x = Demo.Sub.Calc.Zero()\n",
                ),
            ],
            expected_assembly: 2,
        },
        // Same, via an `open`: `open Demo.Sub` shortens the path; `Calc.Zero`
        // resolves as `Demo.Sub.Calc.Zero` despite `module Demo` in the project.
        Project {
            files: vec![
                ("pa_openDemoSub_1", "module Demo\nlet placeholder = 1\n"),
                ("pa_openDemoSub_2", "open Demo.Sub\nlet x = Calc.Zero()\n"),
            ],
            expected_assembly: 2,
        },
        // …and via the root-qualified `open global.Demo.Sub` (the form the
        // over-defer note named — `global` is irrelevant; F# merges either way).
        Project {
            files: vec![
                ("pa_globalOpen_1", "module Demo\nlet placeholder = 1\n"),
                (
                    "pa_globalOpen_2",
                    "open global.Demo.Sub\nlet x = Calc.Zero()\n",
                ),
            ],
            expected_assembly: 2,
        },
        // A project module *named exactly* like the assembly type's enclosing
        // type (`module Demo.Calc`) that does NOT export the referenced member:
        // `Demo.Calc.Answer` falls through to the assembly type `Demo.Calc`
        // (Entity) + member `Answer` (Member) = 2.
        Project {
            files: vec![
                ("pa_modDemoCalc_1", "module Demo.Calc\nlet foo = 1\n"),
                (
                    "pa_modDemoCalc_2",
                    "module Other\nlet x = Demo.Calc.Answer\n",
                ),
            ],
            expected_assembly: 2,
        },
        // A *value-less* project module (only an `open`) still does not shadow:
        // the merged assembly namespace provides `Demo.Calc.Answer`.
        Project {
            files: vec![
                ("pa_valueless_1", "module Demo.Calc\nopen System\n"),
                ("pa_valueless_2", "module Other\nlet x = Demo.Calc.Answer\n"),
            ],
            expected_assembly: 2,
        },
        // A project module named like the *written head* (`module Calc`) that
        // lacks the member: `open Demo` makes `Calc.Answer` resolve to the
        // assembly `Demo.Calc.Answer`, not the project module `Calc`.
        Project {
            files: vec![
                ("pa_headModule_1", "module Calc\nlet placeholder = 1\n"),
                ("pa_headModule_2", "open Demo\nlet x = Calc.Answer\n"),
            ],
            expected_assembly: 2,
        },
        // A project *value* prefix is NOT a merge: `Demo.Calc` is a project value
        // (an int), `.Answer` is member access on it — FCS resolves `Demo.Calc`
        // to the value (out of this assembly oracle's scope), so 0 assembly hits.
        // We must not wrongly resolve it to the assembly type.
        Project {
            files: vec![
                ("pa_value_1", "module Demo\nlet Calc = 1\n"),
                ("pa_value_2", "module Other\nlet x = Demo.Calc.Answer\n"),
            ],
            expected_assembly: 0,
        },
        // An `open type` target is a TYPE, and a **module is not a type**: an
        // earlier project `module Demo.Calc` (even one with a colliding `let Zero`)
        // does NOT shadow the assembly type `Demo.Calc` as the `open type` target.
        // FCS opens the assembly type's statics, so the later bare `Zero()` is the
        // assembly member `Demo.Calc.Zero` (FIXTURE_ASM), never the project value
        // of the same full name — exactly the [P2] the stage-2 codex review
        // *expected* to break, here pinned to FCS's actual behaviour. (The `Zero`
        // use counts; the `Demo` namespace qualifier and the `open type Demo.Calc`
        // clause we defer, which the differential allows.)
        Project {
            files: vec![
                ("pa_opentype_1", "module Demo.Calc\nlet Zero = 0\n"),
                (
                    "pa_opentype_2",
                    "module Other\nopen type Demo.Calc\nlet x = Zero()\n",
                ),
            ],
            expected_assembly: 1,
        },
        // Stage 3 namespace merge: an earlier file's project `namespace Sub` and
        // the referenced assembly's relative `Demo.Sub` *both* open under
        // `namespace Demo; open Sub` — so a path living only in the assembly
        // (`Calc.Zero`, where `Demo.Sub.Calc` has `Zero`) resolves to
        // `Demo.Sub.Calc.Zero`. The project namespace does not suppress the
        // assembly one (FCS). `Calc` (Entity `Demo.Sub.Calc`) + `Zero` (Member) =
        // 2; the `Demo`/`Sub` namespace qualifiers we defer.
        Project {
            files: vec![
                ("pa_nsmerge_1", "namespace Sub\n\ntype Marker = int\n"),
                (
                    "pa_nsmerge_2",
                    "namespace Demo\n\nmodule M =\n    open Sub\n    let y = Calc.Zero()\n",
                ),
            ],
            expected_assembly: 2,
        },
        // The merge opens BOTH the relative `Demo.Sub` and the **root** `Sub`
        // (project `namespace Sub` merged with the root assembly `Sub`), the
        // relative winning collisions. `Calc` is the relative `Demo.Sub.Calc` and
        // `Deep` the relative `Demo.Sub.Deep`; `RootOnly` — only in the root `Sub`
        // — falls to the lower-priority root reading `Sub.RootOnly`. Three
        // type-position entities; the namespace qualifiers defer.
        Project {
            files: vec![
                ("pa_rootonly_1", "namespace Sub\n\ntype Marker = int\n"),
                (
                    "pa_rootonly_2",
                    "namespace Demo\n\nmodule M =\n    open Sub\n    let f (x : RootOnly) = x\n    \
                     let g (y : Calc) = y\n    let h (z : Deep) = z\n",
                ),
            ],
            expected_assembly: 3,
        },
        // An assembly type EVICTS a same-named local value from the head slot
        // (`docs/head-slot-assembly-eviction-plan.md`): `let Calc = {| Answer =
        // 3 |}` then `open Demo` (after the value) puts the class `Demo.Calc`
        // in FCS's unqualified slot, so `Calc.Answer` binds `Demo.Calc.Answer`
        // — FCS resolves the head *and* the member into the fixture, not the
        // local anon-record. Sema cannot resolve the evicted head (the M20t/M20u
        // assembly bar), so it DEFERS — the whole point is that it must never
        // record the local `Calc` value where FCS binds the assembly type
        // (`expected_assembly: 0` — the panic guard is the real assertion; a
        // Stage-2 refinement would resolve `Demo.Calc.Answer` and lift this).
        Project {
            files: vec![(
                "pa_evict",
                "module M\nlet Calc = {| Answer = 3 |}\nopen Demo\nlet x = Calc.Answer\n",
            )],
            expected_assembly: 0,
        },
    ];

    for project in &corpus {
        assert_matches_fcs(project);
    }
}

/// Cross-file **type-position completeness**: the `needle` type-name segment in
/// project file `file_idx` must resolve to assembly entity `expected_full`. Like
/// [`resolve_assembly_diff::assert_type_use_complete`] but project-scoped (so a
/// *preceding* file can declare a same-named project module). FCS is consulted as
/// an oracle (by containment, since FCS spans the rightmost type over the whole
/// long-ident while we record the `Entity` at its segment), and our resolution
/// must match it — a deferral here *fails* the test.
fn assert_project_type_complete(
    files: &[(&'static str, &'static str)],
    file_idx: usize,
    needle: &str,
    expected_full: &str,
) {
    let fixture = ensure_assembly_fixture_built();

    let written: Vec<(std::path::PathBuf, String)> = files
        .iter()
        .map(|(label, src)| (temp_fs_file(label, src), (*src).to_string()))
        .collect();
    let paths: Vec<&std::path::Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, &[fixture]);
    let fcs_files = parse_fcs_uses_project(&json, &written);

    let bytes = std::fs::read(fixture).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");
    let asts: Vec<ImplFile> = written.iter().map(|(_, src)| impl_file(src)).collect();
    let proj = resolve_project(&asts, &env);

    let src = files[file_idx].1;
    let start = src
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {src:?}"));
    let end = start + needle.len();

    // FCS oracle: a fixture use covering our segment with this full name (else
    // the test is not measuring a real resolution).
    let target = &written[file_idx].0;
    let fu = fcs_files
        .iter()
        .find(|f| f.path.file_name() == target.file_name())
        .unwrap_or_else(|| panic!("FCS reported no uses for {target:?}"));
    fu.uses
        .iter()
        .filter(|u| u.start <= start && end <= u.end)
        .filter(|u| u.assembly.as_deref() == Some(FIXTURE_ASM))
        .find(|u| {
            u.full_name
                .as_deref()
                .map(|f| f.split('`').next().unwrap_or(f))
                == Some(expected_full)
        })
        .unwrap_or_else(|| {
            panic!("oracle: FCS does not resolve {needle:?} → {expected_full} in {src:?}")
        });

    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }

    match proj.file(file_idx).resolution_at(span(start, end)) {
        Some(res @ Resolution::Entity(_)) => {
            let (asm, full) = our_assembly_full(&env, res);
            assert_eq!(asm, FIXTURE_ASM, "{needle:?}: assembly");
            assert_eq!(full, expected_full, "{needle:?}: full name");
        }
        other => {
            panic!("incomplete: FCS resolves {needle:?} → {expected_full}, but we gave {other:?}")
        }
    }
}

#[test]
fn project_module_does_not_shadow_an_assembly_type_in_type_position() {
    // A module is not a type, so a project module never shadows a same-named (or
    // open-reachable) assembly TYPE in TYPE position — FCS resolves both cases
    // below to the assembly type `Demo.Calc`. (Regression: the stage-2 as-written
    // gate deferred these via `is_exact_project_module`, which belongs only in the
    // value/expression shadow predicate.)

    // A bare top-level `module Calc` + `open Demo`: `(x: Calc)` is the assembly
    // type `Demo.Calc`, reached through the open — not the project module.
    assert_project_type_complete(
        &[
            ("ptm_bare_1", "module Calc\nlet placeholder = 1\n"),
            ("ptm_bare_2", "open Demo\nlet f (x: Calc) = x\n"),
        ],
        1,
        "Calc",
        "Demo.Calc",
    );

    // Even a project module with the *exact same full name* (`module Demo.Calc`)
    // does not shadow the assembly type: `(x: Demo.Calc)` is the assembly type.
    assert_project_type_complete(
        &[
            ("ptm_fq_1", "module Demo.Calc\nlet placeholder = 1\n"),
            ("ptm_fq_2", "module Other\nlet f (x: Demo.Calc) = x\n"),
        ],
        1,
        "Calc",
        "Demo.Calc",
    );
}

/// codex R6 regression (soundness): opening a *project* namespace must not let a
/// same-named root **assembly** type win a type-position path the project shadows.
///
/// A project namespace `Ns` has a nested `module Sub` declaring `RootOnly`; the
/// referenced fixture assembly has a ROOT `Sub.RootOnly`. Under `open Ns`, `Sub`
/// shortens to the project `Ns.Sub`, so `(x: Sub.RootOnly)` is the PROJECT
/// `Ns.Sub.RootOnly` (FCS-verified) — never the assembly type. The per-open-readings
/// rework had dropped project-namespace opens from `imports`, so the walker fell past
/// the (assembly-empty) opens to the root tier and bound the assembly `Sub.RootOnly`.
/// Recording the project namespace in `imports` makes the assembly path
/// project-shadowed → deferred (sound silence, never a wrong target).
#[test]
fn project_namespace_open_does_not_leak_a_root_assembly_type() {
    let fixture = ensure_assembly_fixture_built();
    let bytes = std::fs::read(fixture).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");

    let files = [
        "namespace Ns\n\nmodule Sub =\n    type RootOnly = { X: int }\n",
        "namespace Other\n\nmodule M =\n    open Ns\n    let f (x: Sub.RootOnly) = x\n",
    ];
    let asts: Vec<ImplFile> = files.iter().map(|s| impl_file(s)).collect();
    let proj = resolve_project(&asts, &env);

    let src = files[1];
    let s = src
        .rfind("RootOnly")
        .expect("`RootOnly` use in the consumer file");
    let res = proj.file(1).resolution_at(span(s, s + "RootOnly".len()));
    assert!(
        !matches!(
            res,
            Some(Resolution::Entity(_)) | Some(Resolution::Member { .. })
        ),
        "`Sub.RootOnly` under `open Ns` (project `Ns.Sub.RootOnly` shadows the root \
         assembly `Sub.RootOnly`) must NOT bind the assembly type; got {res:?}",
    );
}

/// The project-MODULE sibling of the test above (PR #667 review round 2): an
/// `open Calc` whose raw path is a project module still carries a project
/// *namespace* reading through the prior `open P` (`P.Calc`), and FCS binds
/// `Sub.Thing` to the PROJECT `P.Calc.Sub.Thing` under it — so we must never
/// bind the colliding root assembly `Sub.Thing`. Today the module interpretation
/// sets `opaque_dotted_open`, which defers every dotted head in scope, so the
/// walker is never consulted; this pins that a root assembly binding stays
/// impossible if that blanket is ever made more precise (the reading group a
/// `raw_project_module` open feeds to `imports` is what would keep it sound).
#[test]
fn project_module_open_does_not_leak_a_root_assembly_type() {
    let fixture = ensure_assembly_fixture_built();
    let bytes = std::fs::read(fixture).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");

    let files = [
        "module Calc\n\nlet marker = 1\n",
        "namespace P.Calc\n\nmodule Sub =\n    type Thing = { v : int }\n",
        "module M\nopen P\nopen Calc\nlet f (x : Sub.Thing) = x\n",
    ];
    let asts: Vec<ImplFile> = files.iter().map(|s| impl_file(s)).collect();
    let proj = resolve_project(&asts, &env);

    let src = files[2];
    let s = src
        .rfind("Thing")
        .expect("`Thing` use in the consumer file");
    let res = proj.file(2).resolution_at(span(s, s + "Thing".len()));
    assert!(
        !matches!(
            res,
            Some(Resolution::Entity(_)) | Some(Resolution::Member { .. })
        ),
        "`Sub.Thing` under `open P; open Calc` (project `P.Calc.Sub.Thing` shadows the \
         root assembly `Sub.Thing`) must NOT bind the assembly type; got {res:?}",
    );
}

/// Broad **soundness** sweep for project/assembly namespace *merge* scenarios. For
/// every use FCS reports, if WE bind a fixture-assembly `Entity`/`Member` then FCS
/// must name the SAME `(assembly, full name)`. One-directional by design: we may
/// honestly defer (`None`/`Deferred`) or resolve a *project* entity (`Local`/`Item`)
/// where FCS resolves something — but we must NEVER produce a *wrong* assembly
/// target, whether a different assembly entity (wrong namespace/reading) or an
/// assembly type where FCS resolves a shadowing PROJECT one. Flushes the merge-corner
/// class codex surfaced one case per round (the namespace-merge rework: R6, R7).
fn assert_merge_sound(label: &str, files: &[(&'static str, &'static str)]) {
    let fixture = ensure_assembly_fixture_built();
    let written: Vec<(std::path::PathBuf, String)> = files
        .iter()
        .map(|(l, s)| (temp_fs_file(l, s), (*s).to_string()))
        .collect();
    let paths: Vec<&std::path::Path> = written.iter().map(|(p, _)| p.as_path()).collect();
    let json = invoke_fcs_dump_project_with_refs(&paths, &[fixture]);
    let fcs_files = parse_fcs_uses_project(&json, &written);

    let bytes = std::fs::read(fixture).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");
    let asts: Vec<ImplFile> = written.iter().map(|(_, s)| impl_file(s)).collect();
    let proj = resolve_project(&asts, &env);
    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }

    for (i, (path, _)) in written.iter().enumerate() {
        let fu = fcs_files
            .iter()
            .find(|f| f.path.file_name() == path.file_name())
            .unwrap_or_else(|| panic!("[{label}] no FCS uses for {path:?}"));
        let rf = proj.file(i);
        for u in &fu.uses {
            if u.start == u.end {
                continue;
            }
            if let Some(res @ (Resolution::Entity(_) | Resolution::Member { .. })) =
                rf.resolution_at(span(u.start, u.end))
            {
                let (asm, full) = our_assembly_full(&env, res);
                assert_eq!(
                    (Some(asm.as_str()), Some(full.as_str())),
                    (u.assembly.as_deref(), u.full_name.as_deref()),
                    "[{label}] SOUNDNESS: use {:?} at {}..{} in {path:?} — we bound a \
                     fixture-assembly target FCS disagrees with (wrong reading, or FCS \
                     resolves a shadowing project entity)",
                    u.name,
                    u.start,
                    u.end,
                );
            }
        }
    }
}

#[test]
fn namespace_merge_resolution_is_sound_against_fcs() {
    // Pure-assembly opens (no project shadow): the merge readings must land on the
    // right assembly namespace, latest-open-wins, relative-before-root.
    assert_merge_sound(
        "relative-open-collision",
        &[(
            "s",
            "namespace Demo\n\nmodule M =\n    open Sub\n    let f (x: Calc) = x\n",
        )],
    );
    assert_merge_sound(
        "relative-open-root-only",
        &[(
            "s",
            "namespace Demo\n\nmodule M =\n    open Sub\n    let f (x: RootOnly) = x\n",
        )],
    );
    assert_merge_sound(
        "relative-open-assembly-only",
        &[(
            "s",
            "namespace Demo\n\nmodule M =\n    open Sub\n    let f (x: Deep) = x\n",
        )],
    );
    assert_merge_sound(
        "latest-open-wins",
        &[(
            "s",
            "module M\nopen Demo\nopen Demo.Sub\nlet f (x: Calc) = x\n",
        )],
    );
    assert_merge_sound(
        "value-through-relative-open",
        &[(
            "s",
            "namespace Demo\n\nmodule M =\n    open Sub\n    let x = Calc.Zero()\n",
        )],
    );
    assert_merge_sound(
        "chained-open-both-readings",
        &[(
            "s",
            "namespace Demo\n\nmodule M =\n    open Sub\n    open Extra\n    \
             let f (x: RelThing) = x\n    let g (x: ExtraThing) = x\n    let h (x: Shared) = x\n",
        )],
    );

    // Project ⨝ assembly merges: a project entity that shadows must never let a
    // same-named assembly target leak.
    assert_merge_sound(
        "project-ns-shadows-root-assembly (R6)",
        &[
            (
                "Lib",
                "namespace Ns\n\nmodule Sub =\n    type RootOnly = { X: int }\n",
            ),
            (
                "Use",
                "namespace Other\n\nmodule M =\n    open Ns\n    let f (x: Sub.RootOnly) = x\n",
            ),
        ],
    );
    assert_merge_sound(
        "project-module-completes-partial-assembly (R7-A)",
        &[
            (
                "Lib",
                "namespace Ns\n\nmodule Calc =\n    type Inner = { X: int }\n",
            ),
            (
                "Use",
                "module M\nopen Ns\nopen Demo.Sub\nlet f (x: Calc.Inner) = x\n",
            ),
        ],
    );
    assert_merge_sound(
        "project-namespace-merge-chained-collision (R7-B)",
        &[
            ("Lib", "namespace Sub\n\ntype Placeholder = { P: int }\n"),
            (
                "Use",
                "namespace Demo\n\nmodule M =\n    open Sub\n    open Extra\n    let f (x: Shared) = x\n",
            ),
        ],
    );
    assert_merge_sound(
        "project-relative-namespace-shadows-assembly",
        &[
            ("Lib", "namespace Demo.Sub\n\ntype Deep = { D: int }\n"),
            (
                "Use",
                "namespace Demo\n\nmodule M =\n    open Sub\n    let f (x: Deep) = x\n",
            ),
        ],
    );
    assert_merge_sound(
        "project-module-does-not-shadow-assembly-type",
        &[
            ("Lib", "module Calc\n\nlet placeholder = 1\n"),
            ("Use", "module Other\nlet f (x: Demo.Calc) = x\n"),
        ],
    );

    // Rooted `open global.Sub` is absolute — it names the ROOT `Sub`, never the
    // relative `Demo.Sub`, so `Calc` is `Sub.Calc` (no merge canonicalisation).
    assert_merge_sound(
        "rooted-open-is-not-canonicalised",
        &[(
            "s",
            "namespace Demo\n\nmodule M =\n    open global.Sub\n    let f (x: Calc) = x\n",
        )],
    );
    // `open type` through a relative-open merge: `open Sub` (→ `Demo.Sub`) then
    // `open type Calc` is `Demo.Sub.Calc`, so bare `Zero` is its static.
    assert_merge_sound(
        "open-type-through-relative-merge",
        &[(
            "s",
            "namespace Demo\n\nmodule M =\n    open Sub\n    open type Calc\n    let x = Zero()\n",
        )],
    );
    // Value position where a project value shadows through a merged namespace: the
    // project `Ns.Sub.thing` (opened via `open Ns`) must not surface an assembly
    // target, and the assembly-only path must stay sound.
    assert_merge_sound(
        "project-value-through-merged-namespace",
        &[
            (
                "Lib",
                "namespace Sub\n\nmodule Deep =\n    let thing () = 1\n",
            ),
            (
                "Use",
                "namespace Demo\n\nmodule M =\n    open Sub\n    let x = Deep.thing ()\n",
            ),
        ],
    );
    // Two project namespaces opened in sequence, each merging with the assembly:
    // chained-open ordering must stay sound across several opens.
    assert_merge_sound(
        "two-merged-namespace-opens",
        &[
            ("Lib", "namespace Sub\n\ntype P = { A: int }\n"),
            (
                "Use",
                "namespace Demo\n\nmodule M =\n    open Demo\n    open Sub\n    \
                 let f (x: Calc) = x\n    let g (x: RootOnly) = x\n",
            ),
        ],
    );
    // A project type in the *relative* merged namespace shadowing an assembly type
    // reached through the root reading of the same open.
    assert_merge_sound(
        "project-relative-shadows-assembly-root-reading",
        &[
            ("Lib", "namespace Demo.Sub\n\ntype RootOnly = { R: int }\n"),
            (
                "Use",
                "namespace Demo\n\nmodule M =\n    open Sub\n    let f (x: RootOnly) = x\n",
            ),
        ],
    );

    // A project entity at a LOWER tier that completes the path outranks a
    // higher-tier assembly reading that resolves only a prefix: the enclosing-
    // namespace project module provides `Inner`, which the open's partial
    // `Demo.Sub.Calc` cannot — FCS binds `Demo.Calc.Inner` (the project), so a
    // held tier-1 partial must never be applied over a lower-tier project
    // shadow. (The tier-2 sibling of the R7-A open-vs-open case.)
    assert_merge_sound(
        "enclosing-ns-project-completion-outranks-open-partial",
        &[
            (
                "Lib",
                "namespace Demo\n\nmodule Calc =\n    type Inner = { X: int }\n",
            ),
            (
                "Use",
                "namespace Demo\n\nmodule M =\n    open Demo.Sub\n    let f (x: Calc.Inner) = x\n",
            ),
        ],
    );
    // The value-position sibling: the enclosing-namespace project module chain
    // `Demo.Calc.Deep.thing` completes what the open's partial `Demo.Sub.Calc`
    // cannot; FCS binds the project value.
    assert_merge_sound(
        "enclosing-ns-project-completion-outranks-open-partial-value",
        &[
            (
                "Lib",
                "namespace Demo\n\nmodule Calc =\n    module Deep =\n        let thing = 1\n",
            ),
            (
                "Use",
                "namespace Demo\n\nmodule M =\n    open Demo.Sub\n    let x = Calc.Deep.thing\n",
            ),
        ],
    );
    // A *project-only* relative namespace must keep its relative-before-root
    // rank inside one open's readings: `open Sub` in `namespace Other` opens the
    // project `Other.Sub` (relative) ABOVE the assembly root `Sub`, so a
    // colliding `Calc` is the project `Other.Sub.Calc` (FCS), never the assembly
    // `Sub.Calc`. Appending project readings after the assembly ones would let
    // the complete root reading win before the relative shadow is consulted.
    assert_merge_sound(
        "project-only-relative-reading-outranks-assembly-root",
        &[
            ("Lib", "namespace Other.Sub\n\ntype Calc = { C: int }\n"),
            (
                "Use",
                "namespace Other\n\nmodule M =\n    open Sub\n    let f (x: Calc) = x\n",
            ),
        ],
    );
    // An *unmodelled* open (`open type`) must not un-shadow a project value: the
    // enclosing-namespace reading of `Sub.Calc.Zero` is rooted at the PROJECT
    // nested module `Demo.Sub` — a project shadow at a higher tier — so the root
    // assembly `Sub.Calc.Zero` must not bind. FCS: the project
    // `Demo.Sub.Calc.Zero` value. (PR #667 review: the unmodelled-open guard
    // counted only a *resolving* higher reading as "differs", so a
    // project-shadowed one fell through to the root binding.)
    assert_merge_sound(
        "unmodelled-open-does-not-unshadow-a-project-value",
        &[
            (
                "Lib",
                "namespace Demo\n\nmodule Sub =\n    module Calc =\n        let Zero () = 0\n",
            ),
            (
                "Use",
                "namespace Demo\n\nmodule M =\n    open type Demo.Calc\n    let z = Sub.Calc.Zero()\n",
            ),
        ],
    );
    // One `open` can be BOTH an assembly type at its raw path (`Demo.Calc`, an
    // unmodelled open) AND a project namespace through a prior open's shortening
    // prefix (`open P` makes `Demo.Calc` also read as `P.Demo.Calc`). The
    // project reading out-ranks the root assembly reading, so `Sub.Calc.Zero` is
    // the PROJECT `P.Demo.Calc.Sub.Calc.Zero` (FCS) — never the root assembly
    // member. (PR #667 review: the assembly-type branch skipped the reading
    // group, so the unmodelled-open guard never saw the project reading.)
    assert_merge_sound(
        "assembly-type-open-with-a-project-namespace-reading",
        &[
            (
                "Lib",
                "namespace P.Demo.Calc.Sub\n\nmodule Calc =\n    let Zero = 1\n",
            ),
            (
                "Use",
                "module M\nopen P\nopen Demo.Calc\nlet y = Sub.Calc.Zero\n",
            ),
        ],
    );
    // The project-MODULE sibling of the previous case: an `open` whose raw path
    // is a project module (`Calc`) can still carry a project *namespace* reading
    // through a prior open (`open P` makes `Calc` also read as `P.Calc`). That
    // reading out-ranks the root, so `Sub.Thing` is the PROJECT
    // `P.Calc.Sub.Thing` (FCS) — never the root assembly `Sub.Thing`. (PR #667
    // review round 2: the project-module branch dropped the whole reading group
    // from `imports`, so the tier walker never saw the project shadow.)
    assert_merge_sound(
        "project-module-open-with-a-project-namespace-reading",
        &[
            ("Lib1", "module Calc\n\nlet marker = 1\n"),
            (
                "Lib2",
                "namespace P.Calc\n\nmodule Sub =\n    type Thing = { v : int }\n",
            ),
            (
                "Use",
                "module M\nopen P\nopen Calc\nlet f (x : Sub.Thing) = x\n",
            ),
        ],
    );
    // Same-named `namespace A` blocks MERGE: the second block still sees the
    // first block's nested `module Sub`, so `Sub.Calc.Zero` is the PROJECT
    // `A.Sub.Calc.Zero` (FCS) — the colliding root assembly `Sub.Calc.Zero`
    // must not bind. Guards the per-block scoping of the nested-module shadow:
    // the shadow set must be re-taken across same-named blocks, not cleared.
    assert_merge_sound(
        "same-named-block-nested-module-still-shadows-root",
        &[(
            "Blocks",
            "namespace A\n\nmodule Sub =\n    module Calc =\n        let Zero () = 9\n\n\
             namespace A\n\nmodule M =\n    let z = Sub.Calc.Zero()\n",
        )],
    );
}

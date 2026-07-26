//! Generative **qualified union-case pattern** differential vs FCS, over the
//! F# abbrev fixture assembly.
//!
//! `match x with Target.Carrier _ -> …` resolves its head in F#'s *constructor*
//! namespace, which is neither the value namespace nor quite the type one: some
//! same-named entities are transparent to it (the lookup reads past them to the
//! union), others occupy the head, and which is which is not guessable from the
//! entity's kind alone. Getting it right took five rounds of hand-written
//! review, each turning up one more shape — the "curated tests find one corner
//! at a time" trap `resolve_self_qualifier_gen_diff.rs` closes for the
//! self-qualifier and
//! `resolve_qualified_path_access_gen_diff.rs` for the member path. This harness
//! closes it here by *enumerating* the shapes instead of naming them one by one.
//!
//! Dimensions swept (their product, one probe block per combination):
//! - **union** — `Cases.Union.Target` or `Cases.GenericUnion.Target<'T>` (a case
//!   pattern writes no type arguments, so an arity-0 lookup would miss the
//!   generic one);
//! - **shadow** — the other in-scope `Target`: none, a `module`, a class, an
//!   abbreviation *of the union*, an `[<AutoOpen>]`-module type, or a union
//!   **lacking** the probed case;
//! - **order** — whether the shadow's `open` comes after the union's (so the
//!   shadow sits at a *higher* precedence tier) or before it;
//! - **case** — `Carrier` (field-carrying: compiles to a nested IL type) or
//!   `Nullary` (a singleton with none);
//! - **project union** — whether an earlier Compile-order file declares a
//!   `Target` union in the probe's own namespace, which is what makes the
//!   project and assembly readings contend.
//!
//! **The head is the probe.** FCS reports the head `Target`'s binding, and the
//! assertion is **certain-implies-exact**: where FCS is certain we agree exactly
//! *or decline*. Declining is the fail-safe (an availability loss, not a
//! soundness bug); a different target fails. The whole `Target.Case` span is
//! checked for **provenance** only (assembly vs project) — a nested case entity
//! carries no namespace of its own to compare a full name against.
//!
//! A floor on the number of verdicts actually compared keeps the sweep from
//! passing vacuously if the generated sources stop type-checking.

use std::path::{Path, PathBuf};

use crate::common::{
    NormalisedProjectUse, ensure_abbrev_fixture_built, ensure_case_pattern_autoopen_fixture_built,
    ensure_fsharp_core_dll, invoke_fcs_dump_project_with_refs, parse_fcs_uses_project,
    temp_fs_file,
};
use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project};
use rowan::TextRange;

/// Which fixture union the probe is written against.
#[derive(Clone, Copy, Debug)]
enum Union {
    /// `Cases.Union.Target` — `Carrier of int` / `Nullary`.
    Plain,
    /// `Cases.GenericUnion.Target<'T>` — the arity-agnostic lookup.
    Generic,
}

impl Union {
    fn namespace(self) -> &'static str {
        match self {
            Union::Plain => "Cases.Union",
            Union::Generic => "Cases.GenericUnion",
        }
    }

    /// The annotation giving the matched value the union's type, so the
    /// generated source type-checks and FCS reports a real binding.
    fn annotation(self) -> &'static str {
        match self {
            Union::Plain => "Cases.Union.Target",
            Union::Generic => "Cases.GenericUnion.Target<int>",
        }
    }
}

/// The other in-scope entity called `Target`, if any — one fixture namespace
/// each, so a probe picks a shadow by choosing which second namespace to open.
#[derive(Clone, Copy, Debug)]
enum Shadow {
    None,
    /// `module Target` — not a type at all.
    Module,
    /// `type Target()` — a class, with no member of the probed case's name.
    Class,
    /// `type Target = Cases.Union.Target` — an abbreviation of the very union
    /// being probed, so FCS chases it to the same cases.
    Abbreviation,
    /// `[<AutoOpen>] module Auto = type Target = …` — a type reachable only
    /// through the namespace's auto-open, which out-ranks its direct members.
    AutoOpenType,
    /// A union of the name that does **not** declare the probed case.
    CaselessUnion,
}

impl Shadow {
    fn namespace(self) -> Option<&'static str> {
        match self {
            Shadow::None => None,
            Shadow::Module => Some("Cases.ModuleShadow"),
            Shadow::Class => Some("Cases.TypeShadow"),
            Shadow::Abbreviation => Some("Cases.AbbrevShadow"),
            Shadow::AutoOpenType => Some("Cases.AutoOpenShadow"),
            Shadow::CaselessUnion => Some("Cases.CaselessUnion"),
        }
    }
}

/// One generated probe: its label (which reads as its dimensions), the
/// namespace its block declares, and where its head / whole-span ranges landed.
struct Probe {
    label: String,
    namespace: String,
    head: TextRange,
    whole: TextRange,
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// Build the probe file: one `namespace`-delimited block per combination, so
/// every probe rides in a single FCS request. An `open`'s scope is its block, so
/// the blocks do not leak into each other.
fn build_probe_file() -> (String, Vec<Probe>) {
    let mut src = String::new();
    let mut probes = Vec::new();

    for union in [Union::Plain, Union::Generic] {
        for shadow in [
            Shadow::None,
            Shadow::Module,
            Shadow::Class,
            Shadow::Abbreviation,
            Shadow::AutoOpenType,
            Shadow::CaselessUnion,
        ] {
            // With no shadow the two orders are the same source; emit one.
            let orders: &[bool] = match shadow {
                Shadow::None => &[false],
                _ => &[false, true],
            };
            for &shadow_first in orders {
                for case in ["Carrier", "Nullary"] {
                    // The *later* `open` wins, so a shadow opened first sits at
                    // the lower tier.
                    let order = if shadow_first { "ShadowLo" } else { "ShadowHi" };
                    let label = format!("{union:?}_{shadow:?}_{order}_{case}");
                    let namespace = format!("Probe.{label}");

                    src.push_str(&format!("namespace {namespace}\n"));
                    let opens: [Option<&str>; 2] = match (shadow.namespace(), shadow_first) {
                        (Some(s), true) => [Some(s), Some(union.namespace())],
                        (Some(s), false) => [Some(union.namespace()), Some(s)],
                        (None, _) => [Some(union.namespace()), None],
                    };
                    for o in opens.into_iter().flatten() {
                        src.push_str(&format!("open {o}\n"));
                    }
                    src.push_str(&format!("module M_{label} =\n"));
                    src.push_str(&format!(
                        "    let probe (x: {}) =\n        match x with\n        | ",
                        union.annotation()
                    ));
                    let head = src.len();
                    src.push_str("Target");
                    let head_end = src.len();
                    src.push('.');
                    src.push_str(case);
                    let whole_end = src.len();
                    src.push_str(if case == "Carrier" {
                        " _ -> 1\n"
                    } else {
                        " -> 1\n"
                    });
                    src.push_str("        | _ -> 0\n\n");

                    probes.push(Probe {
                        label,
                        namespace,
                        head: span(head, head_end),
                        whole: span(head, whole_end),
                    });
                }
            }
        }
    }
    (src, probes)
}

/// An earlier Compile-order file declaring a **project** `Target` union in every
/// probe's own namespace, so the project reading contends with the assembly one
/// at the enclosing-namespace tier.
fn project_union_file(probes: &[Probe]) -> String {
    let mut out = String::new();
    for p in probes {
        out.push_str(&format!(
            "namespace {}\n\ntype Target =\n    | Carrier of int\n    | Nullary\n    | Other\n\n",
            p.namespace
        ));
    }
    out
}

/// The `(assembly, full name)` our resolution names, for the entity kinds the
/// head probe can produce. `None` for a project-side resolution.
fn our_assembly_full(env: &AssemblyEnv, res: Resolution) -> Option<(String, String)> {
    let Resolution::Entity(h) = res else {
        return None;
    };
    let e = env.entity(h);
    let full = if e.namespace.is_empty() {
        e.name.clone()
    } else {
        format!("{}.{}", e.namespace.join("."), e.name)
    };
    Some((e.assembly.name.clone(), full))
}

/// FCS's verdict at one range, reduced to what the sweep holds us to.
enum Verdict<'a> {
    /// FCS bound a referenced-assembly symbol: `(assembly, full name)`.
    Assembly(&'a str, &'a str),
    /// FCS bound a symbol declared in one of the project files.
    InProject,
    /// FCS reported nothing at this exact range — a type error swallowed the
    /// use, or the name did not bind. No claim, so nothing to check.
    Silent,
}

fn fcs_verdict<'a>(uses: &'a [NormalisedProjectUse], range: TextRange) -> Verdict<'a> {
    let (s, e) = (usize::from(range.start()), usize::from(range.end()));
    let Some(u) = uses
        .iter()
        .find(|u| u.start == s && u.end == e && !u.is_from_definition)
    else {
        return Verdict::Silent;
    };
    if u.decl.is_some() {
        return Verdict::InProject;
    }
    match (u.assembly.as_deref(), u.full_name.as_deref()) {
        (Some(asm), Some(full)) => Verdict::Assembly(asm, full),
        _ => Verdict::Silent,
    }
}

/// Resolve one generated project both ways — FCS with `refs` referenced, and
/// ours over an [`AssemblyEnv`] built from the same DLLs — and assert
/// certain-implies-exact at every probe in the last (probe) file. Returns how
/// many verdicts were actually compared.
fn run_probes(
    label: &str,
    earlier: Option<String>,
    probe_src: String,
    probes: &[Probe],
    fcs_refs: &[&Path],
    env_dlls: &[&Path],
) -> usize {
    // FCS reads the sources from disk, in Compile order.
    let mut written: Vec<(PathBuf, String)> = Vec::new();
    if let Some(src) = earlier {
        written.push((temp_fs_file("casepat_gen_proj", &src), src));
    }
    written.push((temp_fs_file("casepat_gen_probe", &probe_src), probe_src));
    let paths: Vec<&Path> = written.iter().map(|(p, _)| p.as_path()).collect();

    let json = invoke_fcs_dump_project_with_refs(&paths, fcs_refs);
    let fcs_files = parse_fcs_uses_project(&json, &written);

    // Our side, over the same Compile-ordered sources and the same DLLs.
    let views: Vec<Ecma335Assembly> = env_dlls
        .iter()
        .map(|dll| {
            let bytes = std::fs::read(dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
            Ecma335Assembly::parse(&bytes).unwrap_or_else(|e| panic!("parse {dll:?}: {e:?}"))
        })
        .collect();
    let env = AssemblyEnv::from_views(&views).expect("build AssemblyEnv");
    let asts: Vec<ImplFile> = written
        .iter()
        .map(|(_, src)| {
            let p = parse(src);
            assert!(p.errors.is_empty(), "parse errors in generated source");
            ImplFile::cast(p.root).expect("impl file")
        })
        .collect();
    let proj = resolve_project(&asts, &env);

    for (p, _) in &written {
        let _ = std::fs::remove_file(p);
    }

    let probe_idx = written.len() - 1;
    let probe_path = &written[probe_idx].0;
    let fu = fcs_files
        .iter()
        .find(|f| f.path.file_name() == probe_path.file_name())
        .unwrap_or_else(|| panic!("FCS reported no uses for {probe_path:?}"));
    let rf = proj.file(probe_idx);

    let mut compared = 0usize;
    for probe in probes {
        let ctx = format!("{} [{label}]", probe.label);

        // --- The head: exact when FCS names an assembly symbol.
        match fcs_verdict(&fu.uses, probe.head) {
            Verdict::Silent => {}
            Verdict::Assembly(asm, full) => {
                compared += 1;
                match rf.resolution_at(probe.head) {
                    None | Some(Resolution::Deferred(_)) => {}
                    Some(res) => {
                        let (our_asm, our_full) =
                            our_assembly_full(&env, res).unwrap_or_else(|| {
                                panic!(
                                    "{ctx}: FCS bound the head to {asm}!{full}, we gave the \
                                     project resolution {res:?}"
                                )
                            });
                        assert_eq!(
                            (our_asm.as_str(), our_full.as_str()),
                            (asm, full),
                            "{ctx}: head target mismatch"
                        );
                    }
                }
            }
            Verdict::InProject => {
                compared += 1;
                if let Some(Resolution::Entity(h)) = rf.resolution_at(probe.head) {
                    panic!(
                        "{ctx}: FCS bound the head in-project, we gave the assembly entity {:?}",
                        env.entity(h).name
                    );
                }
            }
        }

        // --- The whole `Target.Case` span: provenance only.
        match fcs_verdict(&fu.uses, probe.whole) {
            Verdict::Silent => {}
            Verdict::Assembly(asm, full) => {
                compared += 1;
                match rf.resolution_at(probe.whole) {
                    None | Some(Resolution::Deferred(_)) => {}
                    Some(Resolution::Entity(h)) => assert_eq!(
                        env.entity(h).assembly.name.as_str(),
                        asm,
                        "{ctx}: whole-span assembly mismatch (FCS: {full})"
                    ),
                    Some(res) => {
                        panic!("{ctx}: FCS bound the case to {asm}!{full}, we gave {res:?}")
                    }
                }
            }
            Verdict::InProject => {
                compared += 1;
                if let Some(Resolution::Entity(h)) = rf.resolution_at(probe.whole) {
                    panic!(
                        "{ctx}: FCS bound the case in-project, we gave the assembly entity {:?}",
                        env.entity(h).name
                    );
                }
            }
        }
    }
    compared
}

/// The main sweep: every generated combination, with and without a contending
/// project union.
fn sweep(with_project_union: bool) -> usize {
    let (probe_src, probes) = build_probe_file();
    let earlier = with_project_union.then(|| project_union_file(&probes));
    let label = if with_project_union {
        "+project union"
    } else {
        "assembly only"
    };
    let fixture = ensure_abbrev_fixture_built();
    run_probes(label, earlier, probe_src, &probes, &[fixture], &[fixture])
}

/// The **retained manifest auto-open** arm (codex review): a bare-scope reading
/// no namespace-prefix walk can see, because `AssemblyEnv` keeps a
/// module/type-shaped assembly-level `[<AutoOpen>]` out of the implicit open
/// namespaces. Three probes, each pitting it against a different lower tier, and
/// the oracle — not a hand-written expectation — decides which wins:
///
/// - **bare** (`Probe.RetainedBare`, nothing else in scope): FCS binds the
///   auto-open's `Target`. The control — without it the other two prove nothing,
///   and it is what caught the first version of the fixture, where `[<AutoOpen>]`
///   sat on the module instead of the assembly and imported nothing at all;
/// - **root** (`Probe.RetainedRoot`, against a `namespace global` union of the
///   name): FCS binds the **auto-open**. This is the review's case, and the
///   resolver must not commit the root union it *can* see;
/// - **enclosing namespace** (the block sits *in* `Cases.Union`): FCS binds the
///   **enclosing namespace**, not the auto-open. The guard is name-keyed and
///   declines here too — a miss, not a wrong target.
fn sweep_retained_auto_open() -> usize {
    let mut src = String::new();
    let mut probes = Vec::new();
    // A control: no enclosing-namespace competitor at all, so the head can only
    // come from the manifest auto-open. If FCS does not bind it here, the
    // fixture is not producing the retained surface this arm is about.
    for case in ["Carrier", "Nullary"] {
        src.push_str("namespace Probe.RetainedBare\n");
        src.push_str(&format!("module MBare{case} =\n"));
        src.push_str(
            "    let probe (x: Cases.Retained.Auto.Target) =\n        match x with\n        | ",
        );
        let head = src.len();
        src.push_str("Target");
        let head_end = src.len();
        src.push('.');
        src.push_str(case);
        let whole_end = src.len();
        src.push_str(if case == "Carrier" {
            " _ -> 1\n"
        } else {
            " -> 1\n"
        });
        src.push_str("        | _ -> 0\n\n");
        probes.push(Probe {
            label: format!("RetainedBare_{case}"),
            namespace: "Probe.RetainedBare".to_string(),
            head: span(head, head_end),
            whole: span(head, whole_end),
        });
    }
    // The root tier — the lowest reading there is, and so the one a bare-scope
    // manifest auto-open can plausibly out-rank.
    for case in ["Carrier", "Nullary"] {
        src.push_str("namespace Probe.RetainedRoot\n");
        src.push_str(&format!("module MRoot{case} =\n"));
        src.push_str("    let probe (x: obj) =\n        match x with\n        | ");
        let head = src.len();
        src.push_str("Target");
        let head_end = src.len();
        src.push('.');
        src.push_str(case);
        let whole_end = src.len();
        src.push_str(if case == "Carrier" {
            " _ -> 1\n"
        } else {
            " -> 1\n"
        });
        src.push_str("        | _ -> 0\n\n");
        probes.push(Probe {
            label: format!("RetainedRoot_{case}"),
            namespace: "Probe.RetainedRoot".to_string(),
            head: span(head, head_end),
            whole: span(head, whole_end),
        });
    }
    for case in ["Carrier", "Nullary"] {
        src.push_str("namespace Cases.Union\n");
        src.push_str(&format!("module MRetained{case} =\n"));
        src.push_str("    let probe (x: Cases.Union.Target) =\n        match x with\n        | ");
        let head = src.len();
        src.push_str("Target");
        let head_end = src.len();
        src.push('.');
        src.push_str(case);
        let whole_end = src.len();
        src.push_str(if case == "Carrier" {
            " _ -> 1\n"
        } else {
            " -> 1\n"
        });
        src.push_str("        | _ -> 0\n\n");
        probes.push(Probe {
            label: format!("Retained_{case}"),
            namespace: "Cases.Union".to_string(),
            head: span(head, head_end),
            whole: span(head, whole_end),
        });
    }
    let refs = [
        ensure_abbrev_fixture_built(),
        ensure_case_pattern_autoopen_fixture_built(),
    ];
    let compared = run_probes(
        "retained auto-open",
        None,
        src.clone(),
        &probes,
        &refs,
        &refs,
    );

    // Availability for the **enclosing-namespace** probes specifically. The
    // manifest surface out-ranks the root tier (so `RetainedRoot_*` must keep
    // declining — nothing models that surface) but *not* the enclosing
    // namespace, so these must actually bind. The guard used to defer them
    // wholesale; the soundness property above tolerates that, which is why it
    // needs saying out loud.
    let views: Vec<Ecma335Assembly> = refs
        .iter()
        .map(|dll| {
            Ecma335Assembly::parse(&std::fs::read(dll).expect("read fixture"))
                .expect("parse fixture")
        })
        .collect();
    let env = AssemblyEnv::from_views(&views).expect("env");
    let proj = resolve_project(
        &[ImplFile::cast(parse(&src).root).expect("impl file")],
        &env,
    );
    // The *exact* entity, not merely `is_some`: a `Deferred` is a resolution
    // too, so an availability check that accepts one cannot fail when the
    // guard goes back to deferring — the regression it exists to catch.
    let target = env
        .lookup_type(&["Cases".into(), "Union".into()], "Target", 0)
        .expect("Cases.Union.Target in the fixture env");
    for probe in probes.iter().filter(|p| p.namespace == "Cases.Union") {
        assert_eq!(
            proj.file(0).resolution_at(probe.head),
            Some(Resolution::Entity(target)),
            "{}: the enclosing namespace out-ranks the manifest auto-open surface, \
             so the head must bind Cases.Union.Target",
            probe.label,
        );
    }
    compared
}

/// The **implicit-open** arm. F# auto-opens `Microsoft.FSharp.Core` into every
/// file, so a project union whose name FSharp.Core also declares — `Option`,
/// `Result`, `Choice` — has an assembly entity of that name permanently in
/// scope. An implicit open sits *below* the enclosing namespace, so the project
/// union still wins, and a contention check that cannot tell an implicit open
/// from an explicit one would veto every such case (codex review).
fn sweep_implicit_open_contention() -> usize {
    let earlier =
        "namespace Probe.Implicit\n\ntype Option =\n    | Some of int\n    | Nothing\n".to_string();
    let mut src = String::new();
    let mut probes = Vec::new();
    for case in ["Some", "Nothing"] {
        src.push_str("namespace Probe.Implicit\n");
        src.push_str(&format!("module MImplicit{case} =\n"));
        src.push_str("    let f (x: Probe.Implicit.Option) =\n        match x with\n        | ");
        let head = src.len();
        src.push_str("Option");
        let head_end = src.len();
        src.push('.');
        src.push_str(case);
        let whole_end = src.len();
        src.push_str(if case == "Some" {
            " n -> n\n"
        } else {
            " -> 0\n"
        });
        src.push_str("        | _ -> 0\n\n");
        probes.push(Probe {
            label: format!("Implicit_{case}"),
            namespace: "Probe.Implicit".to_string(),
            head: span(head, head_end),
            whole: span(head, whole_end),
        });
    }
    // FCS already references FSharp.Core; our env needs it loaded explicitly to
    // see the same implicit opens.
    let core = ensure_fsharp_core_dll();
    let compared = run_probes(
        "implicit open",
        Some(earlier.clone()),
        src.clone(),
        &probes,
        &[],
        &[core.as_path()],
    );
    // …and, uniquely among the arms, assert **availability**. Certain-implies-exact
    // tolerates a decline, so it cannot see a resolution we used to make and
    // stopped making — which is exactly what over-vetoing on an implicit open
    // looks like. This shape must actually bind.
    let env = AssemblyEnv::from_views(&[Ecma335Assembly::parse(
        &std::fs::read(&core).expect("read FSharp.Core"),
    )
    .expect("parse FSharp.Core")])
    .expect("FSharp.Core env");
    let asts: Vec<ImplFile> = [earlier.as_str(), src.as_str()]
        .iter()
        .map(|s| ImplFile::cast(parse(s).root).expect("impl file"))
        .collect();
    let proj = resolve_project(&asts, &env);
    for probe in &probes {
        assert!(
            matches!(
                proj.file(1).resolution_at(probe.whole),
                Some(Resolution::Item(_))
            ),
            "{}: a project union case must still bind with FSharp.Core's same-named \
             entity only implicitly opened; got {:?}",
            probe.label,
            proj.file(1).resolution_at(probe.whole)
        );
    }
    compared
}

/// The sweep proper, with a floor so it cannot pass vacuously. The floor is
/// deliberately well below the generated probe count — a shadow that occupies
/// the head makes the pattern ill-typed, and FCS reports nothing for some of
/// those — but far above zero, so a generation change that silently stops
/// producing checkable sources fails here.
#[test]
fn case_pattern_resolution_agrees_with_fcs() {
    let without = sweep(false);
    let with = sweep(true);
    let retained = sweep_retained_auto_open();
    let implicit = sweep_implicit_open_contention();
    assert!(
        without >= 40,
        "assembly-only sweep compared only {without} verdicts — generation is not producing \
         checkable sources"
    );
    assert!(
        with >= 40,
        "project-contended sweep compared only {with} verdicts"
    );
    assert!(
        implicit >= 2,
        "implicit-open sweep compared only {implicit} verdicts"
    );
    assert!(
        retained >= 6,
        "retained-auto-open sweep compared only {retained} verdicts — all three tiers \
         (bare, root, enclosing namespace) must produce a head verdict"
    );
}

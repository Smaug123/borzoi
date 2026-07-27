//! Whole-project name-resolution differential vs FCS over a **real, restored
//! F# project** — including lookups into imported assemblies (FSharp.Core, the
//! BCL, and NuGet package references).
//!
//! Where `sema`'s `resolve_corpus_diff.rs` type-checks each corpus file *in
//! isolation* (no siblings, empty `AssemblyEnv`), this drives the **real LSP
//! runtime chain** end-to-end over one project:
//!
//! 1. Compile order + `#if` defines via the workspace's `.fsproj` evaluation
//!    ([`SemanticState::parses_for_project`]).
//! 2. The project's composed reference set and the [`AssemblyEnv`] built from
//!    it, out of one cache entry
//!    ([`SemanticState::env_reference_dlls_for_project`] +
//!    [`SemanticState::assembly_env_for_project`]): the assets file's package
//!    and framework DLLs, each F# `<ProjectReference>`'s built output DLL, and
//!    the C# sidecar's metadata DLLs.
//! 3. `resolve_project` folds cross-file + assembly resolution over the lot.
//!
//! FCS is the oracle: `fcs-dump uses-project` type-checks the same Compile-
//! ordered files as one project (so cross-file *and* referenced-assembly names
//! resolve), reading **exactly** that reference set and nothing else. Each
//! FCS use is bucketed:
//!
//! * **match** — in-project: our `Local`/`Item` points at a binder in the same
//!   file + range FCS declares; assembly: our `Entity`/`Member` has the same
//!   `(assembly simple name, full name)`.
//! * **divergence** — `Unresolved`, wrong-*named* binder, or in-project-vs-
//!   assembly disagreement where FCS resolved concretely. Gated.
//! * **alt-binder** — same-named binder at a different range/file. Unlike the
//!   isolation corpus sweep (where recovery leaves some as noise), a *fully*
//!   checked project should agree on the exact binder, so this is gated to zero
//!   too — a same-name mismatch here is a real wrong-shadow go-to-def.
//! * **gap** — `Deferred`/unrecorded (a construct we don't model, or a B2/B3
//!   member whose receiver type we don't infer). Expected; counted, not gated.
//!
//! ## Supported project scope
//!
//! The oracle reads the composed reference set and parses under the caller's
//! `#if` symbols. It is a faithful differential for projects that are:
//!
//! * **signature-light** — a `.fsi`-bearing project folds (Stage 1 of
//!   `docs/fsi-signature-restriction-plan.md`), but a signatured module's
//!   members resolve to `Deferred`/the merged assembly until Stage 2 exports
//!   the signature surface, so heavy `.fsi` use shows up as gaps, not
//!   divergences;
//! * **restored, and with their F# `<ProjectReference>`s built** — the composed
//!   set names each referenced project's *output* DLL, so an unbuilt one is
//!   absent from both sides and its uses cannot be checked at all. That is not
//!   silent: FCS then errors, and the oracle-error gate below fails.
//!
//! A non-default `<LangVersion>` *is* supported: the project's resolved
//! `LanguageVersion` is threaded to the oracle as `--langversion:<canonical>`
//! (see `invoke_fcs_dump_project_with_refs`), so both sides take the same
//! version-gated syntax branches. Our side already parses each file at
//! `lang_version_for_project`. The one residual boundary is a pin the oracle's
//! own SDK can't honour (e.g. `11.0`, gated on a preview SDK): FCS then rejects
//! the `--langversion` flag, which surfaces as a *loud* oracle failure rather
//! than a silent divergence.
//!
//! The oracle reads our set *exclusively* because a differential is only
//! evidence when both sides read the same assemblies. An oracle that also
//! resolved its own SDK's framework would answer names our `AssemblyEnv` cannot
//! resolve at all; our side defers, and **a deferral is not a divergence**, so
//! the gate below would pass while the two sides read different worlds. What
//! this costs is that an incomplete reference set stops the project
//! type-checking rather than degrading quietly — hence the oracle-error gate.
//!
//! Validated against `WoofWare.{WeakHashTable, LiangHyphenation, Expect}`
//! (hundreds of in-project + FSharp.Core/BCL/NuGet-package resolutions each, zero
//! divergences).
//!
//! `#[ignore]`d and driven by `BORZOI_PROJECT_FSPROJ` (an absolute path to
//! a **restored** `.fsproj` — its `obj/project.assets.json` must exist). Skips
//! with guidance when unset. Run under `nix develop`:
//!
//! ```text
//! BORZOI_PROJECT_FSPROJ=/path/to/Foo/Foo.fsproj \
//!   cargo test -p borzoi --test all resolve_real_project_diff:: -- --ignored --nocapture
//! ```

use borzoi_oracle_harness::panic_silence::silence_panics_here;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::common::{
    FcsError, OracleRefScope, invoke_fcs_dump_project_with_refs, parse_fcs_uses_project,
};
use borzoi::sdk_discovery::SdkDiscoveryEnv;
use borzoi::semantic::SemanticState;
use borzoi::workspace::Workspace;
use borzoi_cst::language_version::LanguageVersion;
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project_files};
use rowan::TextRange;

/// How many sites of each kind to print for triage. A run on a large project
/// can gate on more sites than fit a readable report, and triage needs the
/// *whole* list to tell "one known class" from "one known class plus three of
/// something else" — so `BORZOI_PROJECT_SAMPLE` raises it.
fn sample_limit() -> usize {
    std::env::var("BORZOI_PROJECT_SAMPLE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40)
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// One disagreement between our resolution and FCS, for the printed sample.
struct Site {
    file: PathBuf,
    range: TextRange,
    text: String,
    /// What FCS resolved this use to.
    fcs: String,
    /// What we resolved it to.
    ours: String,
}

#[derive(Default)]
struct Tally {
    in_proj_match: usize,
    /// Subset of `in_proj_match` whose declaration is in a *different* file than
    /// the use — proof the harness actually exercised cross-file resolution.
    cross_file_match: usize,
    asm_match: usize,
    divergences: Vec<Site>,
    alt_binders: Vec<Site>,
    gaps: usize,
    /// Errors the oracle itself hit. An erroring file yields no divergences and
    /// never will — FCS reports no target for a use it could not check, so the
    /// use is skipped entirely. Counting these is what tells "we agree with the
    /// oracle" from "the oracle had nothing to say".
    oracle_errors: usize,
    /// A sample of the above, for triage.
    oracle_error_sites: Vec<(PathBuf, FcsError)>,
}

/// What our resolution names an assembly symbol: the declaring assembly's simple
/// name, and the symbol's full name rendered **both** fully-qualified
/// (`Microsoft.FSharp.Collections.Array.copy`) and with the entity's namespace
/// dropped (`Array.copy`). FCS's `FullName` sometimes omits the namespace on a
/// bare module/type reference, so accepting *either* form — and nothing else —
/// tolerates exactly that quirk without accepting an unrelated same-suffix
/// target. The member component uses [`AssemblyEnv::member_display_name`] so
/// `[<CompiledName>]` members are named as F# source (`ignore`, not IL `Ignore`).
struct OurAsm {
    assembly: String,
    qualified: String,
    /// Same but without the entity's leading namespace.
    unqualified: String,
}

fn our_assembly_full(env: &AssemblyEnv, res: Resolution) -> OurAsm {
    // A module compiled with a name-clash suffix (`ArrayModule`) carries its F#
    // source name (`Array`); FCS renders modules by that source name.
    fn entity_name(e: &borzoi_assembly::Entity) -> &str {
        e.source_name.as_deref().unwrap_or(&e.name)
    }
    let qualify = |ns: &[String], tail: &str| {
        if ns.is_empty() {
            tail.to_string()
        } else {
            format!("{}.{}", ns.join("."), tail)
        }
    };
    match res {
        Resolution::Entity(h) => {
            let e = env.entity(h);
            let name = entity_name(e).to_string();
            OurAsm {
                assembly: e.assembly.name.clone(),
                qualified: qualify(&e.namespace, &name),
                unqualified: name,
            }
        }
        Resolution::Member { parent, idx } => {
            let e = env.entity(parent);
            let tail = format!(
                "{}.{}",
                entity_name(e),
                env.member_display_name(parent, idx)
            );
            OurAsm {
                assembly: e.assembly.name.clone(),
                qualified: qualify(&e.namespace, &tail),
                unqualified: tail,
            }
        }
        _ => unreachable!("only Entity/Member reach here"),
    }
}

/// Whether one of our renderings equals FCS's full name, modulo the
/// double-backtick quoting FCS applies to identifiers that need it
/// (`Operators.``not```). Only the delimiter pairs are removed, never a lone
/// backtick: a quoted identifier may contain one (`lex.fsl` closes the quote on
/// a *doubled* backtick only), so ``a`b`` and ``ab`` name different symbols.
fn full_matches(ours: &OurAsm, fcs: &str) -> bool {
    let unquote = |s: &str| s.replace("``", "");
    let f = unquote(fcs);
    unquote(&ours.qualified) == f || unquote(&ours.unqualified) == f
}

/// Our resolution's full name with a **nested** entity's enclosing chain
/// restored: `Entity::namespace` is empty for one, so [`our_assembly_full`]'s
/// cheap rendering drops everything above the leaf
/// (`Checked.int64` for `Microsoft.FSharp.Core.Operators.Checked.int64`).
/// [`AssemblyEnv::entity_full_name`] reconstructs it, at the cost of a search
/// from the top-level roots — hence its use only after the cheap form missed,
/// not for every one of the tens of thousands of assembly uses.
fn our_assembly_full_nested(env: &AssemblyEnv, res: Resolution) -> Option<String> {
    match res {
        Resolution::Entity(h) => Some(env.entity_full_name(h)),
        Resolution::Member { parent, idx } => Some(format!(
            "{}.{}",
            env.entity_full_name(parent),
            env.member_display_name(parent, idx)
        )),
        _ => None,
    }
}

/// Whether our name is a *nested-type rendering gap*: a nested entity whose
/// enclosing chain [`our_assembly_full_nested`] could not reconstruct either.
/// When our namespace-less name is a proper whole-segment tail of FCS's fully-
/// qualified name, that's this gap — skip it rather than report a false
/// divergence. (A genuinely wrong resolution — a name that is *not* such a tail,
/// or in another assembly — still diverges.)
fn nested_rendering_gap(ours: &OurAsm, fcs: &str) -> bool {
    let f = fcs.replace("``", "");
    let u = ours.unqualified.replace("``", "");
    let fs: Vec<&str> = f.split('.').collect();
    let us: Vec<&str> = u.split('.').collect();
    !us.is_empty() && us.len() < fs.len() && fs.ends_with(us.as_slice())
}

#[test]
#[ignore = "whole-project resolution vs FCS over a restored project; set BORZOI_PROJECT_FSPROJ, run --ignored under nix develop"]
fn project_resolution_matches_fcs() {
    let Some(fsproj) = std::env::var_os("BORZOI_PROJECT_FSPROJ") else {
        eprintln!(
            "BORZOI_PROJECT_FSPROJ unset; skipping. Point it at a *restored* \
             .fsproj (its obj/project.assets.json must exist) and rerun with \
             --ignored under `nix develop`."
        );
        return;
    };
    let project = PathBuf::from(fsproj);
    let project_dir = project.parent().expect("fsproj has a parent dir");

    // 1. Compile order + defines, via the real workspace evaluation.
    let mut workspace = Workspace::with_env(SdkDiscoveryEnv::from_process_env());
    let mut sema = SemanticState::new();
    let docs: HashMap<lsp_types::Url, String> = HashMap::new();
    let dotnet_root = workspace.dotnet_root_for_project(&project);

    let parses = sema
        .parses_for_project(&project, &mut workspace, &docs)
        .unwrap_or_else(|| {
            panic!(
                "the LSP declined to resolve {project:?}. Common causes: the \
                 evaluated Compile order is untrustworthy (`items_uncertain`), \
                 or a Compile file tripped a CST parser limit. Pick a \
                 cleanly-evaluated project."
            )
        })
        .clone();
    assert!(!parses.files.is_empty(), "no Compile files for {project:?}");

    // The project's resolved `<LangVersion>`, threaded to the FCS oracle below so
    // both sides take the same version-gated syntax branches (our side already
    // parsed each file at this version, via `parses_for_project`). `None` when the
    // project uses the SDK default — FCS's own default agrees with
    // `LanguageVersion::DEFAULT`, so no `--langversion` flag is threaded. (The
    // analogous framework-side boundary — a non-default `<FrameworkReference>`
    // such as AspNetCore / WindowsDesktop — remains out of scope; it is documented
    // in the module header, as it can't be detected as cheaply.)
    let lang_version = {
        let v = workspace.lang_version_for_project(&project);
        (v != LanguageVersion::DEFAULT).then(|| v.to_string())
    };

    // The `#if` symbols our side parsed each file under (implicit `COMPILED` /
    // `EDITING` + the project's `$(DefineConstants)`). Threaded through to the FCS
    // oracle below so both sides take the same conditional-compilation branches —
    // otherwise a project with defines would diverge on `#if`.
    let symbols = workspace.symbols_for_project(&project);

    // `parse_fcs_uses_project` matches FCS's reported files to ours by basename;
    // duplicate basenames (e.g. two `AssemblyInfo.fs` in different dirs) would
    // cross-match and yield bogus (mis)matches. Refuse such projects.
    {
        let mut names: Vec<_> = parses.paths.iter().filter_map(|p| p.file_name()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            total,
            "project {project:?} has Compile files with duplicate basenames; the \
             basename-based FCS file matching would be ambiguous"
        );
    }

    // 2. The project's reference set and the `AssemblyEnv` built from it — the
    //    LSP's own composition, out of one cache entry so the two cannot be two
    //    resolutions that disagree: the assets file's package and framework
    //    DLLs, each F# `<ProjectReference>`'s built output DLL, and the C#
    //    sidecar's metadata DLLs. Composing the set here from the assets file
    //    alone (as this did) omits every `<ProjectReference>` on *both* sides,
    //    so a use of a referenced project's types is `Unresolved` for us and
    //    FS0039 for the oracle — landing in the ungated gaps bucket rather than
    //    being adjudicated.
    let assets = project_dir.join("obj").join("project.assets.json");
    assert!(
        assets.is_file(),
        "no project.assets.json at {assets:?} — restore the project first \
         (`dotnet restore {project:?}`)"
    );
    let target_framework = workspace.served_tfm_for_project(&project);
    // The per-DLL panic backtraces a real reference closure provokes are
    // silenced so a skipped DLL doesn't spam the report.
    let _silence = silence_panics_here();
    let (ref_dlls, retryable) = sema.env_reference_dlls_for_project(
        &project,
        dotnet_root.as_deref(),
        &target_framework,
        &workspace,
    );
    // A transient C# sidecar failure leaves the set uncached, so the env below
    // may be built from a *different* resolution. Comparing across two reference
    // sets is not evidence either way; fail rather than report a number.
    assert!(
        !retryable,
        "the reference set for {project:?} was not stable (a transient C# sidecar \
         failure left it uncached) — rerun"
    );
    let env = sema.assembly_env_for_project(
        &project,
        dotnet_root.as_deref(),
        &target_framework,
        &workspace,
    );
    drop(_silence);
    assert!(
        !ref_dlls.is_empty(),
        "composed no references for {project:?} — the oracle below needs a \
         non-empty set to be exclusive about"
    );

    // 3. Our whole-project resolution — the signature-aware fold, exactly as
    //    the LSP runs it (a `.fsi`-bearing project folds since Stage 1 of
    //    `docs/fsi-signature-restriction-plan.md`).
    let proj = resolve_project_files(&parses.files, &env);

    // FCS oracle over the same Compile order, reading **exactly** the set above
    // and nothing else. Letting it also resolve the running SDK's framework
    // would leave it able to bind names in assemblies we never read: our side
    // defers, FCS answers, and a deferral is not a divergence — so the run
    // scores plausible coverage while the two sides read different worlds.
    // Nothing is filtered out either: FSharp.Core and the framework DLLs are
    // part of the set our env was built from, and dropping either recreates the
    // same asymmetry from the other end.
    let fcs_refs: Vec<&Path> = ref_dlls.iter().map(PathBuf::as_path).collect();
    let path_refs: Vec<&Path> = parses.paths.iter().map(PathBuf::as_path).collect();
    let define_refs: Vec<&str> = symbols.iter().map(String::as_str).collect();
    let json = invoke_fcs_dump_project_with_refs(
        &path_refs,
        &fcs_refs,
        OracleRefScope::Exclusive,
        &define_refs,
        lang_version.as_deref(),
    );

    let sources: Vec<(PathBuf, String)> = parses
        .paths
        .iter()
        .zip(parses.texts.iter())
        .map(|(p, t)| (p.clone(), t.to_string()))
        .collect();
    let fcs_files = parse_fcs_uses_project(&json, &sources);

    let mut tally = Tally::default();
    for (i, path) in parses.paths.iter().enumerate() {
        // `uses-project` emits one `Files` entry per input path, so every Compile
        // file must appear. A miss would silently skip all of that file's uses
        // (hiding any divergence in it while other files keep the counters
        // non-vacuous), so fail loudly instead of continuing.
        let fu = fcs_files
            .iter()
            .find(|f| f.path.file_name() == path.file_name())
            .unwrap_or_else(|| panic!("FCS reported no uses entry for Compile file {path:?}"));
        let rf = proj.file(i);
        let src = &parses.texts[i];

        tally.oracle_errors += fu.errors.len();
        if let Some(first) = fu.errors.first()
            && tally.oracle_error_sites.len() < sample_limit()
        {
            tally.oracle_error_sites.push((path.clone(), first.clone()));
        }

        for u in &fu.uses {
            if u.is_from_definition || u.start == u.end {
                continue;
            }
            let use_range = span(u.start, u.end);
            let text = src.get(u.start..u.end).unwrap_or("");
            let site = |fcs: String, ours: String| Site {
                file: path.clone(),
                range: use_range,
                text: text.to_string(),
                fcs,
                ours,
            };

            let res = rf.resolution_at(use_range);

            if let Some(decl) = &u.decl {
                // FCS resolved this use to an in-project declaration.
                match res {
                    None | Some(Resolution::Deferred(_)) => tally.gaps += 1,
                    Some(r @ (Resolution::Local(_) | Resolution::Item(_))) => {
                        let (def_idx, def) = match r {
                            Resolution::Item(_) => proj.item_def(r).expect("item def for Item"),
                            Resolution::Local(_) => (i, rf.resolved_def(r).expect("local def")),
                            _ => unreachable!(),
                        };
                        let def_path = &parses.paths[def_idx];
                        let same_file = def_path.file_name() == decl.file.file_name();
                        if same_file && def.range == span(decl.start, decl.end) {
                            tally.in_proj_match += 1;
                            if def_idx != i {
                                tally.cross_file_match += 1;
                            }
                        } else if def.name == text {
                            tally.alt_binders.push(site(
                                format!("{:?}:{}..{}", decl.file.file_name(), decl.start, decl.end),
                                format!("{:?}:{:?}", def_path.file_name(), def.range),
                            ));
                        } else {
                            tally.divergences.push(site(
                                format!(
                                    "in-proj decl {:?}:{}..{}",
                                    decl.file.file_name(),
                                    decl.start,
                                    decl.end
                                ),
                                format!(
                                    "binder {:?} at {:?}:{:?}",
                                    def.name,
                                    def_path.file_name(),
                                    def.range
                                ),
                            ));
                        }
                    }
                    Some(other) => tally.divergences.push(site(
                        format!(
                            "in-proj decl {:?}:{}..{}",
                            decl.file.file_name(),
                            decl.start,
                            decl.end
                        ),
                        format!("{other:?}"),
                    )),
                }
            } else if let (Some(asm), Some(full)) = (&u.assembly, &u.full_name) {
                // FCS resolved this use into a referenced assembly.
                match res {
                    None | Some(Resolution::Deferred(_)) => tally.gaps += 1,
                    Some(r @ (Resolution::Entity(_) | Resolution::Member { .. })) => {
                        let ours = our_assembly_full(&env, r);
                        if &ours.assembly == asm && full_matches(&ours, full) {
                            tally.asm_match += 1;
                        } else if &ours.assembly == asm
                            && our_assembly_full_nested(&env, r)
                                .is_some_and(|n| n.replace("``", "") == full.replace("``", ""))
                        {
                            // A nested entity named in full — an adjudicated
                            // match, not a rendering gap.
                            tally.asm_match += 1;
                        } else if &ours.assembly == asm && nested_rendering_gap(&ours, full) {
                            tally.gaps += 1;
                        } else {
                            tally.divergences.push(site(
                                format!("{asm}!{full}"),
                                format!("{}!{}", ours.assembly, ours.qualified),
                            ));
                        }
                    }
                    Some(other) => tally
                        .divergences
                        .push(site(format!("asm {asm}!{full}"), format!("{other:?}"))),
                }
            }
            // else: out-of-scope (operator / symbol FCS gives no assembly or
            // full name for) — skipped, like the corpus sweep.
        }
    }

    report(&project, &tally);

    // Non-vacuous: the harness must actually exercise *cross-file* resolution
    // (a decl in another file — not just same-file locals) and imported-assembly
    // lookups. A single-file project, or one with only local references, does not
    // prove the cross-file path and is rejected here.
    assert!(
        tally.cross_file_match > 0,
        "no cross-file resolutions matched ({} same-file/local only) — pick a \
         multi-file project so the cross-file path is exercised",
        tally.in_proj_match,
    );
    assert!(
        tally.asm_match > 0,
        "no assembly resolutions matched — not exercising imported-assembly lookups"
    );
    // The oracle must actually have checked the project. With an exclusive
    // reference set, a reference we failed to compose makes FCS report FS0039 on
    // every use of it — which is *not* a divergence, because an unchecked use
    // carries no target to disagree with, so it silently leaves the comparison
    // instead of failing it. Gate on it, or the harness's own gap is the one
    // thing it cannot see.
    assert_eq!(
        tally.oracle_errors, 0,
        "the FCS oracle reported {} errors checking {project:?} (see the printed \
         sample) — the reference set we handed it is incomplete, so the counts \
         above compare against a project FCS could not check",
        tally.oracle_errors,
    );
    // The soundness gate: over a *fully* type-checked real project (no
    // isolation-bias recovery), every use FCS resolves concretely must be one we
    // agree with or honestly defer — never a wrong binder, wrong assembly symbol,
    // or `Unresolved` (D5). Alt-binders (our binder is same-named but at a
    // different range) are *also* gated to zero: unlike the isolation corpus
    // sweep — where recovery leaves some as expected noise — a fully-checked
    // project should agree on the exact binder, so a same-name mismatch here
    // would be a real wrong-shadow / wrong-file go-to-def and must not pass
    // silently. If a legitimate case ever surfaces, convert this to a ceiling
    // with the site documented.
    assert!(
        tally.divergences.is_empty() && tally.alt_binders.is_empty(),
        "{} divergences + {} alt-binders vs FCS (see the printed sites) for {project:?}",
        tally.divergences.len(),
        tally.alt_binders.len(),
    );
}

fn report(project: &Path, t: &Tally) {
    eprintln!(
        "\nresolve-real-project {}: {} in-proj match ({} cross-file) | {} asm match | \
         {} diverge | {} alt-binder | {} gaps | {} oracle errors",
        project.display(),
        t.in_proj_match,
        t.cross_file_match,
        t.asm_match,
        t.divergences.len(),
        t.alt_binders.len(),
        t.gaps,
        t.oracle_errors,
    );
    print_sites("divergences", &t.divergences);
    print_sites("alt-binders", &t.alt_binders);
    if !t.oracle_error_sites.is_empty() {
        eprintln!(
            "\noracle errors ({}, first per file, showing up to {}):",
            t.oracle_errors,
            sample_limit(),
        );
        for (path, err) in &t.oracle_error_sites {
            eprintln!(
                "  {:?}: FS{:04} {}",
                path.file_name().unwrap_or_default(),
                err.error_number,
                err.message,
            );
        }
    }
}

fn print_sites(label: &str, sites: &[Site]) {
    if sites.is_empty() {
        return;
    }
    eprintln!(
        "\n{label} ({}, showing up to {}):",
        sites.len(),
        sample_limit()
    );
    for s in sites.iter().take(sample_limit()) {
        eprintln!(
            "  {:?}:{:?} {:?} -> FCS {}, we gave {}",
            s.file.file_name().unwrap_or_default(),
            s.range,
            s.text,
            s.fcs,
            s.ours,
        );
    }
}

use std::collections::HashMap;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use borzoi::position::position_to_offset;
use borzoi::semantic::ProjectParses;
use borzoi_assembly::{
    Access, AssemblyIdentity, Entity, EntityKind, Field, Member, Nullability, Primitive, TypeRef,
    Version,
};
use borzoi_corpus_diff::{
    CorpusSummary, DeclSite, FileUses, LoadLimits, LoadOptions, LoadSkip, LoadedProject,
    ProjectAssetsStatus, ProjectUse, SkippedUses, check_project_corpus_run, compare_project_uses,
    corpus_runner_config_from_env, explain_token, invoke_fcs_uses_project, load_lsp_project,
    load_lsp_project_with_limits, load_lsp_project_with_options, parse_project_uses,
    project_candidates_from_env, project_corpus_run_options_from_env,
    render_project_corpus_run_report, run_project_corpus_diff_with_options, write_json_report_line,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project};
use lsp_types::Position;
use tempfile::TempDir;

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    fs::write(path, text).expect("write fixture file");
}

fn tiny_project() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("Tiny.fsproj");
    write(
        &project,
        r#"<Project>
  <PropertyGroup>
    <DefineConstants>LOCAL_TEST</DefineConstants>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="B.fs" />
  </ItemGroup>
</Project>
"#,
    );
    write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
    write(&tmp.path().join("B.fs"), "module B\nlet y = A.x\n");
    (tmp, project)
}

fn arcade_gated_project() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("ArcadeGated.fsproj");
    write(
        &project,
        r#"<Project>
  <PropertyGroup>
    <DefineConstants>BASE</DefineConstants>
    <DefineConstants Condition="'$(DISABLE_ARCADE)' == 'true'">$(DefineConstants);NO_ARCADE</DefineConstants>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>
"#,
    );
    write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
    (tmp, project)
}

fn text_range(src: &str, needle: &str) -> (usize, usize) {
    let start = src.find(needle).expect("needle appears in source");
    (start, start + needle.len())
}

fn nth_text_range(src: &str, needle: &str, n: usize) -> (usize, usize) {
    let (start, _) = src
        .match_indices(needle)
        .nth(n)
        .expect("needle occurrence appears in source");
    (start, start + needle.len())
}

fn synthetic_loaded_project(src: &str, env: AssemblyEnv) -> LoadedProject {
    let path = PathBuf::from("/tmp/corpus-diff-synthetic/B.fs");
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let files = vec![file];
    let env = Arc::new(env);
    let resolved = Arc::new(resolve_project(&files, env.as_ref()));
    LoadedProject {
        project: PathBuf::from("/tmp/corpus-diff-synthetic/Synthetic.fsproj"),
        parses: ProjectParses {
            files,
            paths: vec![path],
            texts: vec![Arc::<str>::from(src)],
        },
        resolved,
        assembly_env: env,
        project_assets: ProjectAssetsStatus::NotChecked,
        fcs_extra_refs: Vec::new(),
        define_constants: Vec::new(),
        lang_version: None,
    }
}

fn synthetic_assembly_env() -> AssemblyEnv {
    let identity = AssemblyIdentity {
        name: "Synthetic.Assembly".to_string(),
        version: Version {
            major: 1,
            minor: 0,
            build: 0,
            revision: 0,
        },
        public_key_token: None,
    };
    let value = Member::Field(Field {
        name: "Value".to_string(),
        access: Access::Public,
        ty: TypeRef::Primitive(Primitive::I4),
        is_static: true,
        is_init_only: false,
        is_volatile: false,
        is_literal: false,
        is_required: false,
        compiler_feature_required: Vec::new(),
        nullability: Nullability::Oblivious,
        custom_attrs: Vec::new(),
    });
    AssemblyEnv::from_entities(vec![Entity {
        extension_member_names: Vec::new(),
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        assembly: identity,
        namespace: vec!["Demo".to_string()],
        name: "Widget".to_string(),
        kind: EntityKind::Class,
        access: Access::Public,
        generic_parameters: Vec::new(),
        base_type: None,
        interfaces: Vec::new(),
        members: vec![value],
        skipped_members: Vec::new(),
        method_def_tokens: Vec::new(),
        is_sealed: false,
        nested_types: Vec::new(),
        is_readonly: false,
        is_byref_like: false,
        is_struct: false,
        is_auto_open: false,
        is_require_qualified_access: false,
        is_no_equality: false,
        is_no_comparison: false,
        is_structural_equality: false,
        is_structural_comparison: false,
        is_allow_null_literal: false,
        obsolete: None,
        experimental: None,
        default_member: None,
        compiler_feature_required: Vec::new(),
        source_name: None,
        custom_attrs: Vec::new(),
        abbreviation_target: None,
    }])
}

#[test]
fn lsp_loader_loads_plain_compile_order_project() {
    let (_tmp, project) = tiny_project();
    let loaded = load_lsp_project(&project).expect("project should load");
    assert_eq!(loaded.parses.paths.len(), 2);
    assert!(loaded.parses.paths[0].ends_with("A.fs"));
    assert!(loaded.parses.paths[1].ends_with("B.fs"));
    assert_eq!(loaded.define_constants, vec!["LOCAL_TEST"]);
    match &loaded.project_assets {
        ProjectAssetsStatus::Missing { path } => {
            assert!(path.ends_with("obj/project.assets.json"));
        }
        other => panic!("expected missing assets for unrestored fixture, got {other:?}"),
    }
}

#[test]
fn lsp_loader_refuses_signature_projects() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("Sig.fsproj");
    write(
        &project,
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fsi" />
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>
"#,
    );
    write(&tmp.path().join("A.fsi"), "module A\nval x : int\n");
    write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");
    match load_lsp_project(&project) {
        Err(LoadSkip::SignatureFilesUnsupported { path }) => {
            assert!(path.ends_with("A.fsi"));
        }
        other => panic!("expected signature-file skip, got {other:?}"),
    }
}

#[test]
fn lsp_loader_refuses_projects_over_max_files_before_semantic_load() {
    let (_tmp, project) = tiny_project();
    match load_lsp_project_with_limits(
        &project,
        LoadLimits {
            max_files: NonZeroUsize::new(1),
        },
    ) {
        Err(LoadSkip::TooManyFiles { files, max_files }) => {
            assert_eq!(files, 2);
            assert_eq!(max_files, NonZeroUsize::new(1).expect("non-zero"));
        }
        other => panic!("expected too-large skip, got {other:?}"),
    }
}

#[test]
fn lsp_loader_applies_explicit_msbuild_properties() {
    let (_tmp, project) = arcade_gated_project();

    let loaded = load_lsp_project_with_options(
        &project,
        &LoadOptions {
            limits: LoadLimits::default(),
            build_properties: HashMap::from([("DISABLE_ARCADE".to_string(), "true".to_string())]),
        },
    )
    .expect("project should load");

    assert!(loaded.define_constants.iter().any(|d| d == "BASE"));
    assert!(
        loaded.define_constants.iter().any(|d| d == "NO_ARCADE"),
        "DISABLE_ARCADE=true did not reach project evaluation: {:?}",
        loaded.define_constants
    );
}

#[test]
fn lsp_loader_reports_import_failure_for_uncertain_compile_items() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("Import.fsproj");
    write(
        &project,
        r#"<Project>
  <Import Project="Missing.props" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>
"#,
    );
    write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");

    match load_lsp_project(&project) {
        Err(LoadSkip::ItemsUncertain { details }) => {
            let details = details.to_string();
            assert!(details.contains("failed to follow import"), "{details}");
            assert!(details.contains("Missing.props"), "{details}");
        }
        other => panic!("expected detailed items-uncertain skip, got {other:?}"),
    }
}

#[test]
fn lsp_loader_reports_condition_details_for_uncertain_compile_items() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("Condition.fsproj");
    write(
        &project,
        r#"<Project>
  <ItemGroup Condition="'$(TargetFramework)' == 'net8.0'">
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>
"#,
    );
    write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");

    match load_lsp_project(&project) {
        Err(LoadSkip::ItemsUncertain { details }) => {
            let details = details.to_string();
            assert!(details.contains("compile conditions"), "{details}");
            assert!(details.contains("TargetFramework"), "{details}");
            assert!(details.contains("unresolved property"), "{details}");
        }
        other => panic!("expected detailed items-uncertain skip, got {other:?}"),
    }
}

#[test]
fn lsp_loader_reports_causal_details_for_uncertain_compile_items() {
    // Vehicles: `TargetFramework` is carved out of exact undefined reads
    // (never provably unset), so the import path stays unresolved; the
    // `VisualStudioVersion` read is a toolset name that still diagnoses
    // but must not mask the causal import detail.
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("Import.fsproj");
    write(
        &project,
        r#"<Project>
  <PropertyGroup>
    <Noise>$(VisualStudioVersion)</Noise>
  </PropertyGroup>
  <Import Project="$(TargetFramework)/Shared.props" />
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>
"#,
    );
    write(&tmp.path().join("A.fs"), "module A\nlet x = 1\n");

    match load_lsp_project(&project) {
        Err(LoadSkip::ItemsUncertain { details }) => {
            let details = details.to_string();
            assert!(details.contains("causes:"), "{details}");
            assert!(details.contains("dropped <Import Project="), "{details}");
            assert!(details.contains("TargetFramework"), "{details}");
            assert!(
                !details.contains("VisualStudioVersion"),
                "unrelated broad diagnostics should not mask causal details: {details}"
            );
        }
        other => panic!("expected causal items-uncertain skip, got {other:?}"),
    }
}

#[test]
fn comparison_reports_skipped_oracle_categories() {
    let src = "module B\nlet _ = 1\n";
    let loaded = synthetic_loaded_project(src, AssemblyEnv::default());
    let file = loaded.parses.paths[0].clone();
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file.clone(),
            diagnostics: Vec::new(),
            uses: vec![
                ProjectUse {
                    name: "x".to_string(),
                    start: 0,
                    end: 1,
                    is_from_definition: true,
                    decl: Some(DeclSite {
                        file: file.clone(),
                        start: 0,
                        end: 1,
                    }),
                    assembly: None,
                    full_name: None,
                },
                ProjectUse {
                    name: "x".to_string(),
                    start: 4,
                    end: 4,
                    is_from_definition: false,
                    decl: Some(DeclSite {
                        file: file.clone(),
                        start: 0,
                        end: 1,
                    }),
                    assembly: None,
                    full_name: None,
                },
                ProjectUse {
                    name: "printfn".to_string(),
                    start: 0,
                    end: 7,
                    is_from_definition: false,
                    decl: None,
                    assembly: Some("FSharp.Core".to_string()),
                    full_name: None,
                },
                ProjectUse {
                    name: "intrinsic".to_string(),
                    start: 0,
                    end: 9,
                    is_from_definition: false,
                    decl: None,
                    assembly: None,
                    full_name: None,
                },
            ],
        }],
    );
    assert_eq!(comparison.files_compared, 1);
    assert_eq!(comparison.uses_reported, 4);
    assert_eq!(comparison.uses_considered, 0);
    assert_eq!(comparison.assembly_uses_considered, 0);
    assert_eq!(comparison.assembly_matches, 0);
    assert_eq!(comparison.assembly_deferrals, 0);
    assert_eq!(
        comparison.skipped_uses,
        SkippedUses {
            definitions: 1,
            zero_width: 1,
            non_project_declarations: 1,
            no_oracle_declaration: 1,
        }
    );
    assert_eq!(comparison.divergences, Vec::new());
    assert_eq!(comparison.assembly_divergences, Vec::new());
    assert_eq!(comparison.reverse_divergences, Vec::new());
}

#[test]
fn comparison_matches_assembly_oracle_declarations() {
    let src = "module B\nlet _ = Demo.Widget.Value\n";
    let loaded = synthetic_loaded_project(src, synthetic_assembly_env());
    let file = loaded.parses.paths[0].clone();
    let (start, end) = text_range(src, "Demo.Widget.Value");
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file,
            diagnostics: Vec::new(),
            uses: vec![ProjectUse {
                name: "Value".to_string(),
                start,
                end,
                is_from_definition: false,
                decl: None,
                assembly: Some("Synthetic.Assembly".to_string()),
                full_name: Some("Demo.Widget.Value".to_string()),
            }],
        }],
    );

    assert_eq!(comparison.uses_considered, 0);
    assert_eq!(comparison.assembly_uses_considered, 1);
    assert_eq!(comparison.assembly_matches, 1);
    assert_eq!(comparison.assembly_deferrals, 0);
    assert_eq!(comparison.divergences, Vec::new());
    assert_eq!(comparison.assembly_divergences, Vec::new());
    assert_eq!(comparison.reverse_divergences, Vec::new());
}

#[test]
fn comparison_reports_wrong_assembly_resolution() {
    let src = "module B\nlet _ = Demo.Widget.Value\n";
    let loaded = synthetic_loaded_project(src, synthetic_assembly_env());
    let file = loaded.parses.paths[0].clone();
    let (start, end) = text_range(src, "Demo.Widget.Value");
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file,
            diagnostics: Vec::new(),
            uses: vec![ProjectUse {
                name: "Value".to_string(),
                start,
                end,
                is_from_definition: false,
                decl: None,
                assembly: Some("Synthetic.Assembly".to_string()),
                full_name: Some("Demo.Widget.Other".to_string()),
            }],
        }],
    );

    assert_eq!(comparison.assembly_uses_considered, 1);
    assert_eq!(comparison.assembly_matches, 0);
    assert_eq!(comparison.assembly_deferrals, 0);
    assert_eq!(comparison.assembly_divergences.len(), 1);
    assert_eq!(
        comparison.assembly_divergences[0].actual,
        "assembly Synthetic.Assembly full_name Demo.Widget.Value"
    );
}

#[test]
fn comparison_reports_reverse_only_project_resolution() {
    let src = "module B\nlet x = 1\nlet y = x\n";
    let loaded = synthetic_loaded_project(src, AssemblyEnv::default());
    let file = loaded.parses.paths[0].clone();
    let (module_start, module_end) = text_range(src, "B");
    let (x_def_start, x_def_end) = nth_text_range(src, "x", 0);
    let (y_def_start, y_def_end) = text_range(src, "y");
    let (x_use_start, x_use_end) = nth_text_range(src, "x", 1);
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file.clone(),
            diagnostics: Vec::new(),
            uses: vec![
                ProjectUse {
                    name: "B".to_string(),
                    start: module_start,
                    end: module_end,
                    is_from_definition: true,
                    decl: Some(DeclSite {
                        file: file.clone(),
                        start: module_start,
                        end: module_end,
                    }),
                    assembly: None,
                    full_name: None,
                },
                ProjectUse {
                    name: "x".to_string(),
                    start: x_def_start,
                    end: x_def_end,
                    is_from_definition: true,
                    decl: Some(DeclSite {
                        file: file.clone(),
                        start: x_def_start,
                        end: x_def_end,
                    }),
                    assembly: None,
                    full_name: None,
                },
                ProjectUse {
                    name: "y".to_string(),
                    start: y_def_start,
                    end: y_def_end,
                    is_from_definition: true,
                    decl: Some(DeclSite {
                        file: file.clone(),
                        start: y_def_start,
                        end: y_def_end,
                    }),
                    assembly: None,
                    full_name: None,
                },
            ],
        }],
    );

    assert_eq!(comparison.divergences, Vec::new());
    assert_eq!(comparison.assembly_divergences, Vec::new());
    assert_eq!(comparison.reverse_divergences.len(), 1);
    assert_eq!(
        comparison.reverse_divergences[0].range,
        (x_use_start, x_use_end)
    );
    assert_eq!(
        comparison.reverse_divergences[0].covering_oracles,
        Vec::<String>::new()
    );
}

#[test]
#[ignore = "builds/runs FCS; use --ignored for oracle smoke"]
fn tiny_project_matches_fcs() {
    let (_tmp, project) = tiny_project();
    let loaded = load_lsp_project(&project).expect("project should load");
    let json = invoke_fcs_uses_project(&loaded).expect("fcs-dump uses-project");
    let sources: Vec<_> = loaded
        .parses
        .paths
        .iter()
        .cloned()
        .zip(loaded.parses.texts.iter().cloned())
        .collect();
    let fcs = parse_project_uses(&json, &sources).expect("parse FCS uses");
    let comparison = compare_project_uses(&loaded, &fcs);
    assert_eq!(comparison.fcs_error_files, Vec::<PathBuf>::new());
    assert_eq!(comparison.divergences, Vec::new());
    assert_eq!(comparison.assembly_divergences, Vec::new());
    assert_eq!(comparison.reverse_divergences, Vec::new());
    assert!(
        comparison.uses_considered > 0,
        "fixture should exercise at least one project-declared use"
    );
    assert!(
        comparison.matches > 0,
        "fixture should produce at least one exact match"
    );
}

#[test]
#[ignore = "project corpus sweep; set BORZOI_PROJECT_CORPUS or BORZOI_PROJECT_LIST"]
fn project_corpus_resolution_diff() {
    let config = corpus_runner_config_from_env().expect("project corpus runner ratchets are valid");
    let projects = project_candidates_from_env().expect("project corpus runner settings are valid");
    let options = project_corpus_run_options_from_env().expect("project corpus options are valid");
    let run = run_project_corpus_diff_with_options(projects, options);
    eprint!("{}", render_project_corpus_run_report(&run));
    write_json_report_if_requested(&run.summary);

    check_project_corpus_run(&run, config).unwrap_or_else(|err| {
        panic!("{err}\n{}", run.summary.render_text_report());
    });
}

fn write_json_report_if_requested(summary: &CorpusSummary) {
    let Some(path) = std::env::var_os("BORZOI_PROJECT_REPORT_JSONL") else {
        return;
    };
    write_json_report_line(&PathBuf::from(path), summary)
        .expect("write BORZOI_PROJECT_REPORT_JSONL");
}

/// The resolution-explain tool end to end, over a synthetic project: an
/// `open type` whose target is unmodelled poisons dotted heads, so a bare
/// `Foo.Bar.baz` after it defers. [`explain_token`] must surface the token's
/// `Deferred` resolution AND the file's opaque `open` — the facts the
/// `open TypeEquality` / bare `List.replicate` investigation this tool
/// generalises turns on. A plain namespace `open` that brings nothing in stays
/// `clean`, so the two are distinguishable in the dump.
#[test]
fn explain_token_reports_the_opaque_open_of_a_deferred_head() {
    let src = "module M\nopen System\nopen type Opaque\nlet v = Foo.Bar.baz\n";
    let loaded = synthetic_loaded_project(src, AssemblyEnv::default());
    let (head, _) = text_range(src, "Foo.Bar.baz");
    let exp = explain_token(&loaded, 0, head);

    assert!(
        matches!(exp.resolution, Some(Resolution::Deferred(_))),
        "the dotted head must defer; got {:?}",
        exp.resolution
    );
    assert_eq!(exp.opens.len(), 2, "two opens in the file");

    let system = &exp.opens[0];
    assert_eq!(system.path, vec!["System".to_string()]);
    assert!(!system.is_type);
    assert!(
        !system.opacity.perturbs_resolution(),
        "a bring-nothing namespace open stays clean"
    );

    let opaque = &exp.opens[1];
    assert!(opaque.is_type);
    assert_eq!(opaque.path, vec!["Opaque".to_string()]);
    assert!(opaque.opacity.perturbs_resolution());

    // The fact the tool commits to: which opens perturb resolution (candidates).
    // It does NOT claim scope relevance — see the member-tail test below.
    let perturbing = exp.perturbing_opens();
    assert_eq!(perturbing.len(), 1);
    assert_eq!(perturbing[0].path, vec!["Opaque".to_string()]);

    let report = exp.render();
    assert!(
        report.contains("open type Opaque") && report.contains("PERTURBS"),
        "the rendered dump must name the perturbing open:\n{report}"
    );
    assert!(
        report.contains("HEAD") && report.contains("TAIL"),
        "the note must caveat head vs member tail:\n{report}"
    );
    // A no-per-open-effect open (`open System`) must NOT be labelled `clean` —
    // an all-false open can still take part in a per-token deferral, so the tool
    // never claims harmlessness (codex review round 4).
    assert!(
        report.contains("no modeled per-open effect"),
        "a no-effect open must read honestly, never `clean`:\n{report}"
    );
    assert!(
        !report.contains("— clean"),
        "the render must not claim an open is `clean`:\n{report}"
    );
}

/// Regression for the two over-claims `codex review` caught, which share a root
/// — the tool must not present an opaque `open` as the *cause* of a deferral it
/// cannot substantiate: (1) a member/qualified TAIL (`value.Member`, a resolved
/// local receiver) is `Deferred(QualifiedAccess)` pending inference regardless
/// of any open; and (2) an open's lexical scope is its block, not an offset
/// prefix, so an earlier open by offset may be out of scope entirely. The fix
/// removes the scope/causal verdict: the tool reports every opaque open as a
/// *candidate fact* (with ranges) and a caveated note, never a per-token
/// verdict. Here the member tail defers, the opaque open is still listed as a
/// candidate, and the note carries the head/tail + block-scope caveats.
#[test]
fn explain_token_does_not_blame_an_open_for_a_member_tail_defer() {
    let src = "module M\nopen type Opaque\nlet f value = value.Member\n";
    let loaded = synthetic_loaded_project(src, AssemblyEnv::default());
    let (tail, _) = text_range(src, "Member");
    let exp = explain_token(&loaded, 0, tail);

    // The member tail defers pending inference — not because of the open.
    assert!(
        matches!(exp.resolution, Some(Resolution::Deferred(_))),
        "the member tail defers; got {:?}",
        exp.resolution
    );

    // The perturbing open is still surfaced as a candidate fact (it IS opaque),
    // but the tool makes no per-token scope or causal claim about it.
    assert_eq!(
        exp.perturbing_opens().len(),
        1,
        "the perturbing open is listed as a candidate fact"
    );

    let report = exp.render();
    assert!(
        report.contains("TAIL") && report.contains("regardless"),
        "the note must caveat that a member tail defers regardless of any open:\n{report}"
    );
    assert!(
        report.contains("block"),
        "the note must caveat that an open's scope is its block, not an offset prefix:\n{report}"
    );
}

/// Regression for `codex review` round 5, P2: the deferred-token note must fire
/// even when the file has NO `open` declarations. A bare member tail
/// (`value.Member`) in an open-less file still defers pending inference, and the
/// report must explain that — an early `opens.is_empty()` return used to skip the
/// note entirely, contradicting the "fires for any deferred token" contract.
#[test]
fn explain_token_notes_a_deferred_tail_even_with_no_opens() {
    let src = "module M\nlet f value = value.Member\n";
    let loaded = synthetic_loaded_project(src, AssemblyEnv::default());
    let (tail, _) = text_range(src, "Member");
    let exp = explain_token(&loaded, 0, tail);

    assert!(
        matches!(exp.resolution, Some(Resolution::Deferred(_))),
        "the member tail defers; got {:?}",
        exp.resolution
    );
    assert!(exp.opens.is_empty(), "the file has no opens");

    let report = exp.render();
    assert!(
        report.contains("opens: (none)"),
        "the dump must record that there are no opens:\n{report}"
    );
    // The note still fires and carries the per-token caveats.
    assert!(
        report.contains("Deferred") && report.contains("TAIL") && report.contains("regardless"),
        "the deferred-token note must fire even with no opens:\n{report}"
    );
}

/// Ad-hoc "why did this token defer?" CLI, as an env-driven ignored test in the
/// mould of [`project_corpus_resolution_diff`]. Point it at a real project and a
/// token: it loads the project through the same path the LSP uses, resolves the
/// token, and dumps the resolution plus every `open`'s opacity to stderr — the
/// mechanical replacement for hand-tracing a "No definition available" hover.
///
/// `BORZOI_EXPLAIN_LINE` / `BORZOI_EXPLAIN_COL` are **1-based** (editor parity;
/// LSP is 0-based internally). `BORZOI_EXPLAIN_FILE` matches by path suffix or
/// substring, so a bare filename suffices.
#[test]
#[ignore = "explain one token; set BORZOI_EXPLAIN_PROJECT/FILE/LINE/COL"]
fn explain_token_at_position() {
    let Some(project) = std::env::var_os("BORZOI_EXPLAIN_PROJECT") else {
        eprintln!(
            "set BORZOI_EXPLAIN_PROJECT (a .fsproj), BORZOI_EXPLAIN_FILE (a .fs path or suffix), \
             and BORZOI_EXPLAIN_LINE / BORZOI_EXPLAIN_COL (1-based)"
        );
        return;
    };
    // Root the project path: borzoi's MSBuild evaluator rejects a non-rooted
    // `.fsproj` (a `ParseError`, surfaced as `ProjectEvaluationFailed`), so a
    // relative `../Foo/Foo.fsproj` would fail to load. Canonicalize to an
    // absolute path first.
    let project = std::fs::canonicalize(PathBuf::from(&project))
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", PathBuf::from(&project).display()));
    let file_arg = std::env::var("BORZOI_EXPLAIN_FILE").expect("set BORZOI_EXPLAIN_FILE");
    let line: u32 = std::env::var("BORZOI_EXPLAIN_LINE")
        .expect("set BORZOI_EXPLAIN_LINE")
        .parse()
        .expect("BORZOI_EXPLAIN_LINE is a 1-based line number");
    let col: u32 = std::env::var("BORZOI_EXPLAIN_COL")
        .expect("set BORZOI_EXPLAIN_COL")
        .parse()
        .expect("BORZOI_EXPLAIN_COL is a 1-based column");

    let loaded = load_lsp_project(&project)
        .unwrap_or_else(|skip| panic!("load {}: {skip:?}", project.display()));

    let file_idx = select_explain_file(&loaded.parses.paths, &file_arg);

    let text = &loaded.parses.texts[file_idx];
    let pos = Position {
        line: line.saturating_sub(1),
        character: col.saturating_sub(1),
    };
    let byte = position_to_offset(text, pos);
    let exp = explain_token(&loaded, file_idx, byte);
    eprintln!(
        "{} @ {line}:{col} (byte {byte})\n{}",
        loaded.parses.paths[file_idx].display(),
        exp.render()
    );
}

/// Select the file index for the explain CLI. Prefer a path **suffix** match
/// (whole trailing components — `Path::ends_with`, so `Foo.fs` does not match
/// `MyFoo.fs`), fall back to a substring match only if no suffix matched, and
/// require the choice to be **unique** at each stage — a duplicate basename or
/// an ambiguous substring panics with the candidates rather than silently
/// inspecting the wrong file (codex review round 3).
fn select_explain_file(paths: &[PathBuf], file_arg: &str) -> usize {
    let matching = |pred: &dyn Fn(&PathBuf) -> bool| -> Vec<usize> {
        paths
            .iter()
            .enumerate()
            .filter(|(_, p)| pred(p))
            .map(|(i, _)| i)
            .collect()
    };
    let names = |idxs: &[usize]| -> Vec<&PathBuf> { idxs.iter().map(|&i| &paths[i]).collect() };

    let suffix = matching(&|p| p.ends_with(file_arg));
    match suffix.as_slice() {
        [i] => return *i,
        [] => {}
        many => panic!(
            "BORZOI_EXPLAIN_FILE={file_arg:?} matches {} files by path suffix: {:?}",
            many.len(),
            names(many)
        ),
    }

    let substr = matching(&|p| p.to_string_lossy().contains(file_arg));
    match substr.as_slice() {
        [i] => *i,
        [] => panic!("no file matching {file_arg:?}; files: {paths:?}"),
        many => panic!(
            "BORZOI_EXPLAIN_FILE={file_arg:?} matches {} files by substring: {:?}; \
             pass a more specific path suffix",
            many.len(),
            names(many)
        ),
    }
}

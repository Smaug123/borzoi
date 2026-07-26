use std::collections::HashMap;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use borzoi::position::position_to_offset;
use borzoi::semantic::{ProjectParses, SemanticState};
use borzoi::workspace::Workspace;
use borzoi_assembly::{
    Access, AssemblyIdentity, Entity, EntityKind, Field, Member, Nullability, Primitive, TypeRef,
    Version,
};
use borzoi_corpus_diff::{
    CorpusSummary, DeclSite, FcsDiagnostic, FcsErrorFile, FcsPos, FcsRange, FileUses, LoadLimits,
    LoadOptions, LoadSkip, LoadedProject, ProjectAssetsStatus, ProjectUse, SkippedUses, UseDecl,
    check_project_corpus_run, compare_project_uses, corpus_runner_config_from_env, explain_token,
    fcs_error_skip_reason, invoke_fcs_uses_project, load_lsp_project, load_lsp_project_with_limits,
    load_lsp_project_with_options, parse_project_uses, project_candidates_from_env,
    project_corpus_run_options_from_env, render_project_corpus_run_report,
    run_project_corpus_diff_with_options, write_json_report_line,
};
use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_sema::{
    AssemblyEnv, ProjectFile, Resolution, SourceFile, qualified_names, resolve_project_files,
};
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
    let srcs = vec![SourceFile::Impl(file)];
    let paths = vec![path];
    let qnofs = qualified_names(&srcs, &paths);
    let files: Vec<ProjectFile> = srcs
        .into_iter()
        .zip(qnofs)
        .map(|(file, qnof)| ProjectFile::new(file, qnof))
        .collect();
    let env = Arc::new(env);
    let resolved = Arc::new(resolve_project_files(&files, env.as_ref()));
    LoadedProject {
        project: PathBuf::from("/tmp/corpus-diff-synthetic/Synthetic.fsproj"),
        parses: ProjectParses {
            files,
            paths,
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

/// A synthetic project of `(relative path, source)` Compile items, in Compile
/// order, resolved exactly as the runner resolves a real one. `.fsi` items
/// parse under the signature grammar, so a signatured pair folds here the same
/// way it does in the LSP.
fn synthetic_multi_file_project(items: &[(&str, &str)]) -> LoadedProject {
    let root = PathBuf::from("/tmp/corpus-diff-synthetic-multi");
    let paths: Vec<PathBuf> = items.iter().map(|(rel, _)| root.join(rel)).collect();
    let srcs: Vec<SourceFile> = items
        .iter()
        .map(|(rel, src)| {
            if rel.ends_with(".fsi") {
                let parsed = parse_sig(src);
                assert!(
                    parsed.errors.is_empty(),
                    "parse errors in {rel}: {:?}",
                    parsed.errors
                );
                SourceFile::Sig(SigFile::cast(parsed.root).expect("signature file"))
            } else {
                let parsed = parse(src);
                assert!(
                    parsed.errors.is_empty(),
                    "parse errors in {rel}: {:?}",
                    parsed.errors
                );
                SourceFile::Impl(ImplFile::cast(parsed.root).expect("impl file"))
            }
        })
        .collect();
    let qnofs = qualified_names(&srcs, &paths);
    let files: Vec<ProjectFile> = srcs
        .into_iter()
        .zip(qnofs)
        .map(|(file, qnof)| ProjectFile::new(file, qnof))
        .collect();
    let env = Arc::new(AssemblyEnv::default());
    let resolved = Arc::new(resolve_project_files(&files, env.as_ref()));
    LoadedProject {
        project: root.join("Synthetic.fsproj"),
        parses: ProjectParses {
            files,
            paths,
            texts: items
                .iter()
                .map(|(_, src)| Arc::<str>::from(*src))
                .collect(),
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
        definition_range: None,
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

/// A `.fsi` Compile item is loaded like any other: sema folds a signature
/// file into an inert slot carrying its screen and exported surface
/// (`resolve_project`), and Stages 2–3 of
/// `docs/fsi-signature-restriction-plan.md` export that surface with `.fsi`
/// identity — which is precisely what this runner exists to check against FCS.
#[test]
fn lsp_loader_loads_signature_projects() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("Sig.fsproj");
    write(
        &project,
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fsi" />
    <Compile Include="A.fs" />
    <Compile Include="B.fs" />
  </ItemGroup>
</Project>
"#,
    );
    write(&tmp.path().join("A.fsi"), "module A\n\nval x: int\n");
    write(&tmp.path().join("A.fs"), "module A\n\nlet x = 1\n");
    write(&tmp.path().join("B.fs"), "module B\n\nlet y = A.x\n");
    let loaded = load_lsp_project(&project).expect("a signatured project should load");
    assert_eq!(
        loaded.parses.paths.len(),
        3,
        "the .fsi keeps its Compile slot"
    );
    assert!(loaded.parses.paths[0].ends_with("A.fsi"));
    assert!(loaded.parses.paths[1].ends_with("A.fs"));
}

/// The claim the refusal was hiding: when a signature exposes a `val`, a
/// cross-file use of it resolves to the **`.fsi`** ident, so it matches an FCS
/// oracle that declares it there (conclusion 4: provenance = impl, def = sig).
/// A signature file's own slot records no resolutions, so FCS's uses inside the
/// `.fsi` can only ever be deferrals — never divergences.
#[test]
fn a_sig_exposed_val_matches_an_oracle_declaring_it_in_the_fsi() {
    let sig = "module A\n\nval x: int\n";
    let imp = "module A\n\nlet x = 1\n";
    let use_src = "module B\n\nlet y = A.x\n";
    let loaded = synthetic_multi_file_project(&[("A.fsi", sig), ("A.fs", imp), ("B.fs", use_src)]);
    let sig_path = loaded.parses.paths[0].clone();
    let use_path = loaded.parses.paths[2].clone();
    let (decl_start, decl_end) = text_range(sig, "x");
    let (use_start, use_end) = text_range(use_src, "A.x");

    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: use_path,
            diagnostics: Vec::new(),
            uses: vec![ProjectUse {
                name: "x".to_string(),
                start: use_start,
                end: use_end,
                is_from_definition: false,
                decl: UseDecl::InProject(DeclSite {
                    file: sig_path,
                    start: decl_start,
                    end: decl_end,
                }),
                assembly: None,
                full_name: None,
                declaring_entity_arity: None,
            }],
        }],
    );
    assert_eq!(comparison.divergences, Vec::new());
    assert_eq!(
        (comparison.uses_considered, comparison.matches),
        (1, 1),
        "the sig-exposed val must match the `.fsi` declaration, not defer"
    );
}

/// A project whose sources use a referenced **project**'s types can only be
/// type-checked by an oracle that was handed that project's output assembly.
/// Our own side gets it — the LSP's env fold locates each F#
/// `<ProjectReference>`'s built DLL — so the oracle must get the *same* set, or
/// every use of a referenced type is an FS0039 for FCS and the whole project is
/// discarded as "N files had FCS error diagnostics" (which is how
/// `WoofWare.PawPrint`'s main library, 113 files behind one project reference,
/// stayed unmeasured).
///
/// The invariant is equality, not "contains the ref DLL": the oracle and the env
/// must resolve against one reference set. The loader takes the refs from the
/// env's own cache entry (`env_reference_dlls_for_project`), so this compares
/// that stored list against an *independently* re-resolved one
/// ([`SemanticState::reference_dlls_for_project`]) — which also pins that the two
/// accessors agree where nothing has degraded. The containment assertion is there
/// so the equality cannot pass vacuously on two empty lists.
#[test]
fn the_oracle_reference_set_is_the_set_the_env_is_built_from() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lib = tmp.path().join("Lib").join("Lib.fsproj");
    write(
        &lib,
        "<Project>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  \
         </PropertyGroup>\n</Project>\n",
    );
    // The producer's *built* output — the only thing that makes the reference
    // resolvable for either side. Content is irrelevant here: this test observes
    // the composed path list, not a parsed assembly.
    write(
        &tmp.path()
            .join("Lib")
            .join("bin")
            .join("Debug")
            .join("net8.0")
            .join("Lib.dll"),
        "",
    );
    let app = tmp.path().join("App").join("App.fsproj");
    write(
        &app,
        r#"<Project>
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
  <ItemGroup>
    <ProjectReference Include="..\Lib\Lib.fsproj" />
  </ItemGroup>
</Project>
"#,
    );
    write(
        &tmp.path().join("App").join("A.fs"),
        "module A\nlet x = 1\n",
    );
    // A restored project: the env fold declines outright without an assets file,
    // so the reference set would be empty for reasons unrelated to the claim.
    write(
        &tmp.path()
            .join("App")
            .join("obj")
            .join("project.assets.json"),
        &serde_json::to_string_pretty(&serde_json::json!({
            "version": 3,
            "targets": {
                "net8.0": { "Lib/1.0.0": { "type": "project", "framework": "net8.0" } }
            },
            "libraries": {
                "Lib/1.0.0": {
                    "type": "project",
                    "msbuildProject": "../../Lib/Lib.fsproj",
                    "path": "../../Lib/Lib.fsproj"
                }
            },
            "packageFolders": { tmp.path().join("packages").to_str().expect("utf-8 tmp"): {} },
            "project": { "frameworks": { "net8.0": {} } },
        }))
        .expect("assets json"),
    );

    let loaded = load_lsp_project(&app).expect("project should load");
    assert!(
        loaded
            .fcs_extra_refs
            .iter()
            .any(|p| p.ends_with("Lib/bin/Debug/net8.0/Lib.dll")),
        "the oracle must see the project reference's built output; got {:?} (assets {:?})",
        loaded.fcs_extra_refs,
        loaded.project_assets
    );

    let mut workspace = Workspace::new();
    let mut semantic = SemanticState::new();
    let dotnet_root = workspace.dotnet_root_for_project(&app);
    let tfm = workspace.served_tfm_for_project(&app);
    let env_refs =
        semantic.reference_dlls_for_project(&app, dotnet_root.as_deref(), &tfm, &workspace);
    assert_eq!(
        loaded.fcs_extra_refs, env_refs,
        "the oracle's reference set must be the env's, not a second composition"
    );
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

/// An or-pattern binds one name **once**: `| A _n | B _n -> _n` has a single
/// `_n`, declared at the first alternative, and the second alternative's
/// spelling is a *use* of it. FCS agrees — and for an underscore-prefixed name
/// it reports nothing at all at the later alternatives, so those occurrences
/// are ranges the oracle is silent about. Silence is not evidence: they must be
/// counted, not reported as resolutions FCS contradicts — while the body use,
/// which the oracle *does* report, must match.
#[test]
fn an_unoracled_or_pattern_alias_is_not_a_reverse_divergence() {
    let src = "module B\n\ntype T =\n    | A of int\n    | B of int\n\nlet f (t: T) =\n    match t with\n    | A _n\n    | B _n -> _n\n";
    let loaded = synthetic_loaded_project(src, AssemblyEnv::default());
    let file = loaded.parses.paths[0].clone();
    // FCS's view: the binder is declared at the *first* alternative, and the
    // body use points there. Nothing at all is reported for the second
    // alternative's `_n`.
    let first = src.find("| A _n").expect("first alternative") + 6 - 2;
    let body = src.rfind("_n").expect("body use");
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file.clone(),
            diagnostics: Vec::new(),
            uses: vec![
                ProjectUse {
                    name: "_n".to_string(),
                    start: first,
                    end: first + 2,
                    is_from_definition: true,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: first,
                        end: first + 2,
                    }),
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
                },
                ProjectUse {
                    name: "_n".to_string(),
                    start: body,
                    end: body + 2,
                    is_from_definition: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: first,
                        end: first + 2,
                    }),
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
                },
            ],
        }],
    );
    // The body use is the one occurrence the oracle *does* report, and it names
    // the first alternative's binder — so it is graded, and it matches.
    assert_eq!(
        comparison.divergences,
        Vec::new(),
        "the body use points at the alternative FCS declares"
    );
    assert_eq!(comparison.matches, 1, "the body use is compared and agrees");

    // The fixture's oracle deliberately lists only the binder's two FCS uses,
    // so other names in the file have no covering oracle either; this asserts
    // about the second alternative's `_n` specifically.
    let second = src.find("| B _n").expect("second alternative") + 4;
    assert!(
        !comparison
            .reverse_divergences
            .iter()
            .any(|d| d.range == (second, second + 2)),
        "the second alternative's spelling has no covering oracle, so it is not \
         a divergence: {:?}",
        comparison.reverse_divergences
    );
    assert_eq!(
        comparison.unoracled_or_pattern_aliases, 1,
        "the silently-skipped alias must still be counted"
    );
    assert!(
        comparison.unoracled_definitions > 0,
        "the file's other binders are unoracled definitions, counted separately"
    );
}

/// A *merely enclosing* oracle use is not the oracle speaking about our range.
///
/// For a non-simple lambda parameter FCS synthesises an `_arg1` symbol spanning
/// the **whole** pattern, so every occurrence inside it is enclosed by an
/// unrelated use. That must not defeat the binding-position exemption: the
/// second alternative's `_n` is still a range FCS says nothing about, and
/// scoring it as a contradiction would report a correct answer as a divergence.
#[test]
fn an_enclosing_synthetic_use_does_not_defeat_the_alias_exemption() {
    let src =
        "module B\n\ntype T =\n    | A of int\n    | B of int\n\nlet f = fun (A _n | B _n) -> _n\n";
    let loaded = synthetic_loaded_project(src, AssemblyEnv::default());
    let file = loaded.parses.paths[0].clone();
    let pattern_start = src.find("A _n").expect("pattern start");
    let pattern_end = src.find(") ->").expect("pattern end");
    let first = src.find("A _n").expect("first alternative") + 2;
    let body = src.rfind("_n").expect("body use");
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file.clone(),
            diagnostics: Vec::new(),
            uses: vec![
                // FCS's synthetic parameter, spanning the whole pattern.
                ProjectUse {
                    name: "_arg1".to_string(),
                    start: pattern_start,
                    end: pattern_end,
                    is_from_definition: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: pattern_start,
                        end: pattern_end,
                    }),
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
                },
                ProjectUse {
                    name: "_n".to_string(),
                    start: first,
                    end: first + 2,
                    is_from_definition: true,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: first,
                        end: first + 2,
                    }),
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
                },
                ProjectUse {
                    name: "_n".to_string(),
                    start: body,
                    end: body + 2,
                    is_from_definition: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: first,
                        end: first + 2,
                    }),
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
                },
            ],
        }],
    );

    let second = src.find("| B _n").expect("second alternative") + 4;
    assert!(
        !comparison
            .reverse_divergences
            .iter()
            .any(|d| d.range == (second, second + 2)),
        "an enclosing synthetic use is not speech about this range: {:?}",
        comparison.reverse_divergences
    );
    assert_eq!(comparison.unoracled_or_pattern_aliases, 1);
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
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: 0,
                        end: 1,
                    }),
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
                },
                ProjectUse {
                    name: "x".to_string(),
                    start: 4,
                    end: 4,
                    is_from_definition: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: 0,
                        end: 1,
                    }),
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
                },
                ProjectUse {
                    name: "printfn".to_string(),
                    start: 0,
                    end: 7,
                    is_from_definition: false,
                    decl: UseDecl::Unlocated,
                    assembly: Some("FSharp.Core".to_string()),
                    full_name: None,
                    declaring_entity_arity: None,
                },
                ProjectUse {
                    name: "intrinsic".to_string(),
                    start: 0,
                    end: 9,
                    is_from_definition: false,
                    decl: UseDecl::Unlocated,
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
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
            out_of_project_declarations: 0,
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
                decl: UseDecl::Unlocated,
                assembly: Some("Synthetic.Assembly".to_string()),
                full_name: Some("Demo.Widget.Value".to_string()),
                declaring_entity_arity: None,
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
                decl: UseDecl::Unlocated,
                assembly: Some("Synthetic.Assembly".to_string()),
                full_name: Some("Demo.Widget.Other".to_string()),
                declaring_entity_arity: None,
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
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: module_start,
                        end: module_end,
                    }),
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
                },
                ProjectUse {
                    name: "x".to_string(),
                    start: x_def_start,
                    end: x_def_end,
                    is_from_definition: true,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: x_def_start,
                        end: x_def_end,
                    }),
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
                },
                ProjectUse {
                    name: "y".to_string(),
                    start: y_def_start,
                    end: y_def_end,
                    is_from_definition: true,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: y_def_start,
                        end: y_def_end,
                    }),
                    assembly: None,
                    full_name: None,
                    declaring_entity_arity: None,
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

/// An oracle-side failure must say what the oracle *said*. A bare count ("88
/// files had FCS error diagnostics") names neither a cause nor a next step —
/// diagnosing the one that hid `WoofWare.PawPrint`'s main library took a
/// bespoke probe, when the very first error ("The type 'DumpedAssembly' is not
/// defined") named the missing project reference outright.
#[test]
fn an_oracle_error_skip_quotes_the_diagnostics() {
    let reason = fcs_error_skip_reason(&[
        FcsErrorFile {
            path: PathBuf::from("/proj/Corelib.fs"),
            errors: vec![
                fcs_error(39, "The type 'DumpedAssembly' is not defined.", 10, 19),
                fcs_error(39, "The type 'TypeInfo' is not defined.", 13, 10),
            ],
        },
        FcsErrorFile {
            path: PathBuf::from("/proj/CliType.fs"),
            errors: vec![fcs_error(
                72,
                "Lookup on object of indeterminate type",
                4,
                2,
            )],
        },
    ]);
    assert!(
        reason.starts_with("2 files had FCS error diagnostics (3 errors): "),
        "the counts lead: {reason}"
    );
    assert!(
        reason.contains("Corelib.fs:10:19 FS0039 The type 'DumpedAssembly' is not defined."),
        "the first error is quoted with its site: {reason}"
    );
    assert!(
        reason.contains("CliType.fs:4:2 FS0072"),
        "a later file's error is quoted too, so one noisy file cannot crowd out \
         the rest: {reason}"
    );
}

/// Only the leading diagnostics are quoted — an 8473-error project must not
/// paste 8473 messages into a one-line skip reason — and the tail is *counted*
/// rather than dropped silently.
#[test]
fn an_oracle_error_skip_bounds_what_it_quotes() {
    let files: Vec<FcsErrorFile> = (0..20)
        .map(|i| FcsErrorFile {
            path: PathBuf::from(format!("/proj/F{i}.fs")),
            errors: vec![fcs_error(39, &format!("undefined thing {i}"), 1, 1)],
        })
        .collect();
    let reason = fcs_error_skip_reason(&files);
    assert!(
        reason.starts_with("20 files had FCS error diagnostics (20 errors): "),
        "{reason}"
    );
    let quoted = reason.matches(" FS0039 ").count();
    assert!(
        (1..=5).contains(&quoted),
        "expected a bounded quote, got {quoted} in {reason}"
    );
    assert!(
        reason.ends_with(&format!("(+{} more)", 20 - quoted)),
        "the unquoted tail must be counted: {reason}"
    );
}

fn fcs_error(number: i32, message: &str, line: u32, col: u32) -> FcsDiagnostic {
    FcsDiagnostic {
        severity: "Error".to_string(),
        message: message.to_string(),
        error_number: number,
        range: FcsRange {
            file: String::new(),
            start: FcsPos { line, col },
            end: FcsPos { line, col },
        },
    }
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
    assert_eq!(comparison.fcs_error_files, Vec::<FcsErrorFile>::new());
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

/// A plain namespace `open` that only adds a **reading** / shortening prefix
/// (`open Demo`, resolving to the assembly namespace) sets no deferral flag and
/// raises no barrier, yet it re-orders qualified-path precedence and so can defer
/// a later dotted head. The trace must flag it (`added_reading`) and the render
/// must name the signal — otherwise the explain tool omits the very open that
/// could be the cause (codex review P2: the `open Low; open High; M.Mangled`
/// precedence deferral).
#[test]
fn explain_token_flags_a_reading_adding_namespace_open() {
    let src = "module M\nopen Demo\nlet v = Widget.Value\n";
    let loaded = synthetic_loaded_project(src, synthetic_assembly_env());
    let (tok, _) = text_range(src, "Widget.Value");
    let exp = explain_token(&loaded, 0, tok);

    let demo = exp
        .opens
        .iter()
        .find(|o| o.path == vec!["Demo".to_string()])
        .expect("open Demo is traced");
    assert!(
        demo.opacity.added_reading,
        "open Demo added the reading `Demo`"
    );
    assert!(
        demo.opacity.perturbs_resolution(),
        "so it reads as a per-open perturbation candidate"
    );
    // Reading-precedence is the only signal for this clean namespace open.
    assert!(!demo.opacity.opaque_value);
    assert!(!demo.opacity.opaque_dotted);
    assert!(!demo.opacity.unmodelled);
    assert!(!demo.opacity.staled_earlier);
    assert!(!demo.opacity.imported_deferred);

    let report = exp.render();
    assert!(
        report.contains("open Demo") && report.contains("added_reading"),
        "the render must name the reading signal:\n{report}"
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

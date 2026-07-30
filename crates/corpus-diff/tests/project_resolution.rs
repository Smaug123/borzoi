use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use borzoi::position::position_to_offset;
use borzoi::semantic::{ProjectParses, SemanticState};
use borzoi::workspace::Workspace;
use borzoi_assembly::{
    Access, AssemblyIdentity, Entity, EntityKind, Field, Member, Nullability, Primitive, TypeRef,
    Version,
};
use borzoi_corpus_diff::{
    Comparison, CorpusSummary, DeclSite, FcsDiagnostic, FcsErrorFile, FcsPos, FcsRange, FileUses,
    LoadLimits, LoadOptions, LoadSkip, LoadedProject, ProjectAssetsStatus, ProjectUse, SkippedUses,
    UseDecl, check_project_corpus_run, compare_project_uses, corpus_runner_config_from_env,
    explain_token, fcs_dump_command, fcs_error_skip_reason, invoke_fcs_uses_project,
    load_lsp_project, load_lsp_project_with_limits, load_lsp_project_with_options,
    parse_project_uses, project_candidates_from_env, project_corpus_run_options_from_env,
    render_project_corpus_run_report, run_project_corpus_diff_with_options, write_json_report_line,
};
use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_oracle_harness::BatchChild;
use borzoi_sema::{
    AssemblyEnv, ProjectFile, Resolution, SourceFile, qualified_names, resolve_project_files,
};
use lsp_types::Position;
use tempfile::TempDir;

/// One request type-checks a whole (single-file) generated project, so it gets
/// a project-scale budget rather than the snippet-sized driver default.
const PROJECT_ORACLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

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

/// An attribute whose type is an in-file **abbreviation** of an in-file
/// attribute class, which is the shape where FCS's general symbol-use stream is
/// at its most crowded: the attribute's range can carry an entity use *and* a
/// constructor use, and the two name different declarations.
fn alias_attribute_project() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("AliasAttr.fsproj");
    write(
        &project,
        r#"<Project>
  <ItemGroup>
    <Compile Include="A.fs" />
  </ItemGroup>
</Project>
"#,
    );
    // Both halves in one file: `[<Base>]` names its attribute class directly and
    // must be graded, `[<Alias>]` goes through an abbreviation and must not be.
    write(
        &tmp.path().join("A.fs"),
        "module A\n\ntype BaseAttribute() =\n    inherit System.Attribute()\n\ntype Alias = BaseAttribute\n\n[<Base>]\nlet direct = 1\n\n[<Alias>]\nlet aliased = 2\n",
    );
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
                is_compiler_generated: false,
                decl: UseDecl::InProject(DeclSite {
                    file: sig_path,
                    start: decl_start,
                    end: decl_end,
                }),
                assembly: None,
                full_name: None,
                generic_arity: None,
                is_constructor: false,
                declaring: None,
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

/// FCS reports a destructuring parameter's synthetic value over the *pattern's*
/// span — the same span as the union case the pattern names — so a comparison
/// keyed on spans has two oracle answers for one site and no way to prefer the
/// one the author wrote. `WoofWare.Incremental`'s
/// `let inline toUnit FakeUnit.FakeUnit = ()` is the measured case: the oracle
/// emits both `_arg1` and `FakeUnit` at `FakeUnit.FakeUnit`, and picking `_arg1`
/// made a correct case resolution a divergence.
///
/// The compiler-generated ones are skipped, and counted so the skip is visible
/// rather than silent.
#[test]
fn a_compiler_generated_value_is_skipped_rather_than_compared() {
    // The helper fixes the path; the oracle's file must be the same one.
    let path = PathBuf::from("/tmp/corpus-diff-synthetic/B.fs");
    let src = "module A\nlet x = 1\n";
    let compared = ProjectUse {
        name: "x".to_string(),
        start: 13,
        end: 14,
        is_from_definition: false,
        is_compiler_generated: false,
        decl: UseDecl::Unlocated,
        assembly: None,
        full_name: None,
        generic_arity: None,
        is_constructor: false,
        declaring: None,
    };
    let generated = ProjectUse {
        name: "_arg1".to_string(),
        is_compiler_generated: true,
        ..compared.clone()
    };
    let loaded = synthetic_loaded_project(src, AssemblyEnv::from_entities(Vec::new()));
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: path.clone(),
            diagnostics: Vec::new(),
            uses: vec![compared, generated],
        }],
    );
    assert_eq!(comparison.skipped_uses.compiler_generated, 1);
    assert_eq!(
        comparison.skipped_uses.no_oracle_declaration, 1,
        "the author-written use is still adjudicated"
    );
}

/// A real managed assembly to stand in for a fixture's built output.
///
/// The reference set the oracle is handed is the one our env was *built* from,
/// so a fixture that writes an empty file where a DLL should be is testing
/// nothing: the file is dropped before either side sees it.
fn system_runtime_dll() -> PathBuf {
    if let Some(explicit) = std::env::var_os("BORZOI_SYSTEM_RUNTIME_DLL") {
        return PathBuf::from(explicit);
    }
    let dotnet_root = std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .expect("DOTNET_ROOT unset (run under `nix develop`, or set BORZOI_SYSTEM_RUNTIME_DLL)");
    let packs = dotnet_root.join("packs").join("Microsoft.NETCore.App.Ref");
    fs::read_dir(&packs)
        .unwrap_or_else(|e| panic!("read ref packs dir {}: {e}", packs.display()))
        .filter_map(|e| e.ok())
        .flat_map(|entry| {
            let refs = entry.path().join("ref");
            fs::read_dir(&refs)
                .into_iter()
                .flatten()
                .filter_map(|tfm| tfm.ok())
                .map(|tfm| tfm.path().join("System.Runtime.dll"))
                .collect::<Vec<_>>()
        })
        .find(|dll| dll.is_file())
        .unwrap_or_else(|| panic!("no System.Runtime.dll under {}", packs.display()))
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
    // resolvable for either side. A **real** assembly, because the set the
    // oracle is handed is the set our env was built from: a file we cannot
    // project is not in it (see
    // `the_oracle_set_excludes_a_reference_we_could_not_read`).
    let lib_dll = tmp
        .path()
        .join("Lib")
        .join("bin")
        .join("Debug")
        .join("net8.0")
        .join("Lib.dll");
    std::fs::create_dir_all(lib_dll.parent().expect("Lib.dll has a parent")).expect("mkdir");
    std::fs::copy(system_runtime_dll(), &lib_dll).expect("copy a real assembly into place");
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

/// A reference we could not **read** is not in the set either side resolves
/// against, so it must not be handed to the oracle.
///
/// The asymmetry is not cosmetic. Our env drops a superseded duplicate (a legacy
/// `system.runtime` contract assembly beside the framework pack's) and anything
/// it cannot project; handing FCS the *unfiltered* list gave it two corelibs,
/// and type-checking `WoofWare.PawPrint`'s main library then never terminated
/// (100%+ CPU, RSS past 21 GB in five minutes) — measured, and the reason this
/// list is the env's rather than the composition's.
#[test]
fn the_oracle_set_excludes_a_reference_we_could_not_read() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lib = tmp.path().join("Lib").join("Lib.fsproj");
    write(
        &lib,
        "<Project>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  \
         </PropertyGroup>\n</Project>\n",
    );
    // Present where the built output belongs, and not an assembly.
    write(
        &tmp.path()
            .join("Lib")
            .join("bin")
            .join("Debug")
            .join("net8.0")
            .join("Lib.dll"),
        "MZ but nothing else",
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
        !loaded
            .fcs_extra_refs
            .iter()
            .any(|p| p.ends_with("Lib/bin/Debug/net8.0/Lib.dll")),
        "an unreadable DLL is in neither side's reference set; got {:?}",
        loaded.fcs_extra_refs
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
                    is_compiler_generated: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: first,
                        end: first + 2,
                    }),
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
                },
                ProjectUse {
                    name: "_n".to_string(),
                    start: body,
                    end: body + 2,
                    is_from_definition: false,
                    is_compiler_generated: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: first,
                        end: first + 2,
                    }),
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
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
                    is_compiler_generated: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: pattern_start,
                        end: pattern_end,
                    }),
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
                },
                ProjectUse {
                    name: "_n".to_string(),
                    start: first,
                    end: first + 2,
                    is_from_definition: true,
                    is_compiler_generated: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: first,
                        end: first + 2,
                    }),
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
                },
                ProjectUse {
                    name: "_n".to_string(),
                    start: body,
                    end: body + 2,
                    is_from_definition: false,
                    is_compiler_generated: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: first,
                        end: first + 2,
                    }),
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
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
                    is_compiler_generated: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: 0,
                        end: 1,
                    }),
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
                },
                ProjectUse {
                    name: "x".to_string(),
                    start: 4,
                    end: 4,
                    is_from_definition: false,
                    is_compiler_generated: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: 0,
                        end: 1,
                    }),
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
                },
                ProjectUse {
                    name: "printfn".to_string(),
                    start: 0,
                    end: 7,
                    is_from_definition: false,
                    is_compiler_generated: false,
                    decl: UseDecl::Unlocated,
                    assembly: Some("FSharp.Core".to_string()),
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
                },
                ProjectUse {
                    name: "intrinsic".to_string(),
                    start: 0,
                    end: 9,
                    is_from_definition: false,
                    is_compiler_generated: false,
                    decl: UseDecl::Unlocated,
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
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
            compiler_generated: 0,
            non_project_declarations: 1,
            out_of_project_declarations: 0,
            no_oracle_declaration: 1,
            ambiguous_oracle_range: 0,
            shadowed_constructor_use: 0,
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
                is_compiler_generated: false,
                decl: UseDecl::Unlocated,
                assembly: Some("Synthetic.Assembly".to_string()),
                full_name: Some("Demo.Widget.Value".to_string()),
                generic_arity: None,
                is_constructor: false,
                declaring: None,
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

/// An attribute type is answered out of a **second** commit map, and the
/// differential has to read it.
///
/// Name resolution keeps attribute types apart from ordinary occurrences (they
/// answer FCS's suffix-first candidate walk), but the LSP serves both — hover
/// and go-to-definition on `[<Mark>]` reach the attribute's type. A comparison
/// that asked only `resolution_at` would see silence here and bank a
/// *deferral*, which claims nothing; the answer would go undiffed however wrong
/// it was, with every headline number and the divergence gate unmoved. So this
/// pins the join rather than the count: revert
/// `committed_resolution_at` to `resolution_at` and `matches` falls to 0 while
/// `deferrals` rises to 1.
#[test]
fn comparison_diffs_an_attribute_type_the_main_resolution_map_never_sees() {
    let src = "\
module B

type MarkAttribute () =
    inherit System.Attribute ()

[<Mark>]
let value = 1
";
    let loaded = synthetic_loaded_project(src, AssemblyEnv::default());
    let file = loaded.parses.paths[0].clone();
    // Occurrence 0 is the type's own declaration; occurrence 1 is the written
    // attribute name, which is where FCS reports the use.
    let (use_start, use_end) = nth_text_range(src, "Mark", 1);
    let (decl_start, decl_end) = text_range(src, "MarkAttribute");
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file.clone(),
            diagnostics: Vec::new(),
            uses: vec![ProjectUse {
                // FCS names the suffixed type, as it resolves it.
                name: "MarkAttribute".to_string(),
                start: use_start,
                end: use_end,
                is_from_definition: false,
                is_compiler_generated: false,
                decl: UseDecl::InProject(DeclSite {
                    file,
                    start: decl_start,
                    end: decl_end,
                }),
                assembly: None,
                full_name: None,
                generic_arity: None,
                is_constructor: false,
                declaring: None,
            }],
        }],
    );

    assert_eq!(comparison.uses_considered, 1);
    assert_eq!(comparison.matches, 1);
    assert_eq!(comparison.deferrals, 0);
    assert_eq!(comparison.attribute_commits_compared, 1);
    assert_eq!(comparison.divergences, Vec::new());
    assert_eq!(comparison.reverse_divergences, Vec::new());
}

/// A constructor record does not cost the site its comparison.
///
/// Wherever a written name both names something and calls it — `inherit
/// Base(1)`, `Foo()`, `[<Alias>]` — FCS reports the name *and* the constructor
/// at one range, and for a type with more than one constructor the two carry
/// different declarations. Sema answers the written name and models no separate
/// resolution for the constructor, so the name's record is the one that grades
/// the site and the constructor's steps aside. Treating the pair as two rival
/// answers instead would retire the comparison, quietly shrinking coverage on
/// ordinary code that has nothing to do with attributes.
#[test]
fn a_constructor_record_steps_aside_for_the_name_the_author_wrote() {
    let src = "module B\nlet x = 1\nlet y = x\n";
    let loaded = synthetic_loaded_project(src, AssemblyEnv::default());
    let file = loaded.parses.paths[0].clone();
    let (x_def_start, x_def_end) = nth_text_range(src, "x", 0);
    let (y_def_start, y_def_end) = text_range(src, "y");
    let (use_start, use_end) = nth_text_range(src, "x", 1);
    let named = ProjectUse {
        name: "x".to_string(),
        start: use_start,
        end: use_end,
        is_from_definition: false,
        is_compiler_generated: false,
        decl: UseDecl::InProject(DeclSite {
            file: file.clone(),
            start: x_def_start,
            end: x_def_end,
        }),
        assembly: None,
        full_name: None,
        generic_arity: None,
        is_constructor: false,
        declaring: None,
    };
    // Same range, a *different* declaration, and flagged as the constructor —
    // the shape a multi-constructor type produces.
    let constructor = ProjectUse {
        is_constructor: true,
        decl: UseDecl::InProject(DeclSite {
            file: file.clone(),
            start: y_def_start,
            end: y_def_end,
        }),
        ..named.clone()
    };
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file,
            diagnostics: Vec::new(),
            uses: vec![named, constructor],
        }],
    );

    assert_eq!(comparison.skipped_uses.shadowed_constructor_use, 1);
    assert_eq!(comparison.skipped_uses.ambiguous_oracle_range, 0);
    assert_eq!(comparison.uses_considered, 1);
    assert_eq!(comparison.matches, 1);
    assert_eq!(comparison.divergences, Vec::new());
    assert_eq!(comparison.reverse_divergences, Vec::new());
}

/// A record the comparator cannot grade does not get a vote on whether its
/// range is ambiguous.
///
/// The ambiguity skip exists because two *answers* at one range cannot
/// adjudicate a single verdict. A record with neither an in-project declaration
/// nor a complete assembly identity is not a second answer — it is a record the
/// forward pass sets aside as unadjudicable on its own account. Letting it vote
/// would silently retire a comparison that is perfectly well determined, and
/// nothing would fail: coverage would just quietly drain into the skip bucket.
#[test]
fn an_ungradable_oracle_record_does_not_make_its_range_ambiguous() {
    let src = "module B\nlet x = 1\nlet y = x\n";
    let loaded = synthetic_loaded_project(src, AssemblyEnv::default());
    let file = loaded.parses.paths[0].clone();
    let (x_def_start, x_def_end) = nth_text_range(src, "x", 0);
    let (use_start, use_end) = nth_text_range(src, "x", 1);
    let gradable = ProjectUse {
        name: "x".to_string(),
        start: use_start,
        end: use_end,
        is_from_definition: false,
        is_compiler_generated: false,
        decl: UseDecl::InProject(DeclSite {
            file: file.clone(),
            start: x_def_start,
            end: x_def_end,
        }),
        assembly: None,
        full_name: None,
        generic_arity: None,
        is_constructor: false,
        declaring: None,
    };
    // Same range, but no declaration and no assembly identity: the forward pass
    // counts this one as `no_oracle_declaration`.
    let ungradable = ProjectUse {
        name: "x".to_string(),
        decl: UseDecl::Unlocated,
        ..gradable.clone()
    };
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file,
            diagnostics: Vec::new(),
            uses: vec![gradable, ungradable],
        }],
    );

    assert_eq!(comparison.skipped_uses.ambiguous_oracle_range, 0);
    assert_eq!(comparison.skipped_uses.no_oracle_declaration, 1);
    assert_eq!(comparison.uses_considered, 1);
    assert_eq!(comparison.matches, 1);
    assert_eq!(comparison.divergences, Vec::new());
}

/// A source whose member access only **inference** can answer, and the BCL env
/// it needs: the resolver defers at `Length` (its receiver is a value, not a
/// path it can walk) and the `HasMember` wake resolves it against
/// `System.String`.
fn member_access_source() -> &'static str {
    "module B\nlet s = \"hi\"\nlet n = s.Length\n"
}

fn bcl_loaded_project(src: &str) -> LoadedProject {
    let bytes = fs::read(system_runtime_dll()).expect("read System.Runtime.dll");
    let bcl = borzoi_assembly::Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    synthetic_loaded_project(
        src,
        AssemblyEnv::from_views(&[bcl]).expect("build AssemblyEnv"),
    )
}

/// The oracle record FCS reports for the `Length` access, spanning `span`.
fn string_length_use(span: (usize, usize)) -> ProjectUse {
    ProjectUse {
        name: "Length".to_string(),
        start: span.0,
        end: span.1,
        is_from_definition: false,
        is_compiler_generated: false,
        decl: UseDecl::Unlocated,
        assembly: Some("System.Runtime".to_string()),
        full_name: Some("System.String.Length".to_string()),
        generic_arity: None,
        is_constructor: false,
        declaring: None,
    }
}

/// The answer inference commits at a member name is put to the oracle.
///
/// `x.Length` is a site the *resolver* only ever defers on, and inference then
/// answers — a go-to-definition target the LSP serves (`handlers/definition.rs`
/// layers the member table over the resolver's deferral). Read through the
/// resolver alone the site counts as a deferral, which claims nothing, so a
/// wrong member answer could never fail this differential however long it stood.
#[test]
fn a_member_answer_inference_supplies_is_put_to_the_oracle() {
    let src = member_access_source();
    let loaded = bcl_loaded_project(src);
    let file = loaded.parses.paths[0].clone();
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file,
            diagnostics: Vec::new(),
            uses: vec![string_length_use(text_range(src, "Length"))],
        }],
    );

    assert_eq!(comparison.assembly_uses_considered, 1);
    assert_eq!(comparison.member_commits_compared, 1);
    assert_eq!(comparison.assembly_matches, 1);
    assert_eq!(comparison.assembly_deferrals, 0);
    assert_eq!(comparison.assembly_divergences, Vec::new());
    // The one oracle record here is about `Length`, so only that range's answer
    // is confirmed; `let s` is reported unconfirmed for want of a record, which
    // is this fixture's doing and not the member surface's.
    let length = text_range(src, "Length");
    assert!(
        !comparison
            .reverse_divergences
            .iter()
            .any(|d| d.range == length),
        "the member answer is confirmed by the record covering it: {:?}",
        comparison.reverse_divergences
    );
}

/// The two sides key the same answer at different spans, and the comparison is
/// on the span they share the *end* of.
///
/// Inference keys the member **name** token so hover can scope its tooltip to
/// it; FCS reports one use spanning the whole access and names it by the final
/// segment's symbol. Comparing whole ranges instead compares nothing at all —
/// silently, since a missing answer reads as a deferral.
#[test]
fn a_member_answer_is_graded_against_the_oracle_span_it_ends() {
    let src = member_access_source();
    let loaded = bcl_loaded_project(src);
    let file = loaded.parses.paths[0].clone();
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file,
            diagnostics: Vec::new(),
            uses: vec![string_length_use(text_range(src, "s.Length"))],
        }],
    );

    assert_eq!(comparison.member_commits_compared, 1);
    assert_eq!(comparison.assembly_matches, 1);
    assert_eq!(comparison.assembly_deferrals, 0);
}

/// The member surface is *graded*, not merely counted: a member answer the
/// oracle contradicts is a divergence like any other.
#[test]
fn a_wrong_member_answer_is_a_divergence() {
    let src = member_access_source();
    let loaded = bcl_loaded_project(src);
    let file = loaded.parses.paths[0].clone();
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file,
            diagnostics: Vec::new(),
            uses: vec![ProjectUse {
                full_name: Some("System.String.Chars".to_string()),
                ..string_length_use(text_range(src, "Length"))
            }],
        }],
    );

    assert_eq!(comparison.member_commits_compared, 1);
    assert_eq!(comparison.assembly_matches, 0);
    assert_eq!(comparison.assembly_divergences.len(), 1);
    assert_eq!(
        comparison.assembly_divergences[0].actual,
        "assembly System.Runtime full_name System.String.Length"
    );
}

/// The reverse direction reads the member table too — the direction that
/// catches an answer the oracle never licensed at all, rather than one it
/// contradicts.
#[test]
fn a_member_answer_the_oracle_is_silent_about_is_a_reverse_divergence() {
    let src = member_access_source();
    let loaded = bcl_loaded_project(src);
    let file = loaded.parses.paths[0].clone();
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file.clone(),
            diagnostics: Vec::new(),
            uses: Vec::new(),
        }],
    );

    let (start, end) = text_range(src, "Length");
    assert!(
        comparison
            .reverse_divergences
            .iter()
            .any(|d| d.file == file && d.range == (start, end)),
        "the member commit must be reported unconfirmed: {:?}",
        comparison.reverse_divergences
    );
}

/// One served answer is reported once.
///
/// At a static call the resolver answers across the whole path *and* inference
/// records the member token inside it. The LSP reaches the resolver's answer
/// first (it takes the smallest resolution *containing* the cursor), so the
/// inner entry is never served — and grading it as well would compare one answer
/// twice and, with the oracle silent, report it as two separate soundness
/// failures at two ranges.
#[test]
fn a_member_entry_the_resolver_answers_over_is_not_reported_twice() {
    let src = "module B\nlet b = System.Object.ReferenceEquals (\"a\", \"b\")\n";
    let loaded = bcl_loaded_project(src);
    let file = loaded.parses.paths[0].clone();
    let path = text_range(src, "System.Object.ReferenceEquals");
    let comparison = compare_project_uses(
        &loaded,
        &[FileUses {
            path: file.clone(),
            diagnostics: Vec::new(),
            uses: Vec::new(),
        }],
    );

    let member: Vec<_> = comparison
        .reverse_divergences
        .iter()
        .filter(|d| d.range.1 == path.1)
        .map(|d| d.range)
        .collect();
    assert_eq!(
        member,
        vec![path],
        "the whole-path answer is the served one, and the only one reported"
    );
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
                is_compiler_generated: false,
                decl: UseDecl::Unlocated,
                assembly: Some("Synthetic.Assembly".to_string()),
                full_name: Some("Demo.Widget.Other".to_string()),
                generic_arity: None,
                is_constructor: false,
                declaring: None,
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
                    is_compiler_generated: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: module_start,
                        end: module_end,
                    }),
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
                },
                ProjectUse {
                    name: "x".to_string(),
                    start: x_def_start,
                    end: x_def_end,
                    is_from_definition: true,
                    is_compiler_generated: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: x_def_start,
                        end: x_def_end,
                    }),
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
                },
                ProjectUse {
                    name: "y".to_string(),
                    start: y_def_start,
                    end: y_def_end,
                    is_from_definition: true,
                    is_compiler_generated: false,
                    decl: UseDecl::InProject(DeclSite {
                        file: file.clone(),
                        start: y_def_start,
                        end: y_def_end,
                    }),
                    assembly: None,
                    full_name: None,
                    generic_arity: None,
                    is_constructor: false,
                    declaring: None,
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

/// The attribute commit surface, end to end against the **general** symbol-use
/// stream the corpus runner actually uses — not the attribute-specific oracle,
/// which reports one record per attribute and so cannot see this question.
///
/// `uses-project` may report more than one symbol at an attribute's range (an
/// entity use and a constructor use), and for an abbreviation those name
/// different declarations. Since a single range gets a single answer from us,
/// the crowded range is where reading the attribute map could turn a correct
/// answer into a divergence and fail the zero-divergence gate for a project
/// that is entirely valid.
#[test]
#[ignore = "builds/runs FCS; use --ignored for oracle smoke"]
fn alias_attribute_project_matches_fcs() {
    let (_tmp, project) = alias_attribute_project();
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
        comparison.attribute_commits_compared > 0,
        "the directly-named attribute class must be put to the oracle"
    );
    assert!(
        comparison.skipped_uses.shadowed_constructor_use > 0,
        "the constructor record must step aside for the record naming what the \
         author wrote, rather than grading a type answer it never spoke about"
    );
    assert_eq!(
        comparison.skipped_uses.ambiguous_oracle_range, 0,
        "with the constructor shadowed there is one answer per range, so \
         nothing here is unadjudicable"
    );
}

/// What span the **real** oracle reports a member access at — the fact the
/// comparison's alignment rests on, and the one thing the unit tests above
/// cannot establish, since they write the spans themselves.
///
/// Inference keys the member *name* token so hover can scope its tooltip to it.
/// FCS reports one use spanning the whole access (`s.Length`) and names it by
/// the final segment's symbol. Keying the comparison on whole ranges therefore
/// compares nothing at all — silently, because a missing answer reads as a
/// deferral — which is what this pins against.
///
/// The two sides here resolve `System.String` through different facades: our env
/// is the ref pack's `System.Runtime`, while FCS's default reference set
/// surfaces it through `netstandard`. So identities are deliberately not
/// asserted — a real project hands the oracle the very reference set its own env
/// was built from ([`the_oracle_reference_set_is_the_set_the_env_is_built_from`]),
/// and the corpus runner is where identities get graded.
#[test]
#[ignore = "builds/runs FCS; use --ignored for oracle smoke"]
fn fcs_reports_a_member_access_over_a_span_our_key_ends() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("B.fs");
    let src = member_access_source();
    write(&path, src);
    let mut loaded = bcl_loaded_project(src);
    loaded.project = tmp.path().join("Synthetic.fsproj");
    loaded.parses.paths = vec![path.clone()];

    let json = invoke_fcs_uses_project(&loaded).expect("fcs-dump uses-project");
    let sources = vec![(path.clone(), loaded.parses.texts[0].clone())];
    let fcs = parse_project_uses(&json, &sources).expect("parse FCS uses");

    let member = text_range(src, "Length");
    let reported: Vec<(usize, usize)> = fcs
        .iter()
        .flat_map(|f| f.uses.iter())
        .filter(|u| u.name == "Length")
        .map(|u| (u.start, u.end))
        .collect();
    assert_eq!(
        reported,
        vec![text_range(src, "s.Length")],
        "FCS reports the member over the whole access, not the name alone"
    );
    assert!(
        reported[0].0 < member.0 && reported[0].1 == member.1,
        "the spans share their end and nothing else: oracle {reported:?}, ours {member:?}"
    );

    let comparison = compare_project_uses(&loaded, &fcs);
    assert_eq!(comparison.fcs_error_files, Vec::<FcsErrorFile>::new());
    assert_eq!(
        comparison.member_commits_compared, 1,
        "the answer inference commits at that span's tail must be put to the oracle"
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

/// Byte range of the name written inside the cell's single `[<...>]`.
fn attribute_name_range(src: &str) -> (usize, usize) {
    let open = src.find("[<").expect("cell has an attribute");
    let start = open + 2;
    let end = src[start..].find(">]").expect("attribute is closed") + start;
    (start, end)
}

/// One generated cell of the attribute sweep: a whole single-file project.
struct AttrCell {
    /// The declaration kind, carried rather than parsed back out of `label` —
    /// `multi-ctor` contains the separator, so a label split silently yields
    /// `multi` and a coverage floor keyed on it can never be satisfied.
    kind: &'static str,
    label: String,
    src: String,
}

/// A declaration template: the declared name to the declaration text.
type AttrDeclTemplate = fn(&str) -> String;

/// The generated attribute-shape matrix, adjudicated against the **general**
/// symbol-use stream.
///
/// Every declaration is in-file and every attribute type is project-declared, so
/// a cell is graded on declaration ranges and needs no reference set. That is
/// deliberate: threading an exclusive reference set would make each cell's
/// verdict depend on which DLLs the ref pack happens to hold, and the question
/// here is not which assembly an attribute came from — it is *how many answers
/// the oracle reports at the written name's range*, which is a property of the
/// shape alone.
///
/// The axes are the ones that make a range crowded, since that is the shape the
/// attribute-specific oracle cannot express:
///
/// - **declaration kind** — a plain class, a *generic* class, a class with two
///   constructors, an abbreviation of a class, and an `exception` (which
///   declares a type in the same namespace without being one an attribute may
///   name). An abbreviation and a multi-constructor class are the two shapes
///   where the entity record and the constructor record at one range carry
///   *different* declarations.
/// - **declared name** vs **written form** — `Mark` against `MarkAttribute`,
///   crossed, which is F#'s suffix-first candidate walk. Both spellings are
///   declared in-file rather than contested against `FSharp.Core`, so the
///   contest is exercised without a reference set deciding it.
fn attribute_matrix() -> Vec<AttrCell> {
    let decl_kinds: [(&str, AttrDeclTemplate); 5] = [
        ("class", |n| {
            format!("type {n}() =\n    inherit System.Attribute()\n")
        }),
        ("generic", |n| {
            format!("type {n}<'T>() =\n    inherit System.Attribute()\n")
        }),
        // Two constructors: the constructor record at the attribute's range
        // cannot name a unique declaration, so the entity record must be the
        // one that grades the site.
        ("multi-ctor", |n| {
            format!("type {n}(x: int) =\n    inherit System.Attribute()\n    new () = {n}(0)\n")
        }),
        // The #226 shape: the written name is an abbreviation, so the entity
        // record names the abbreviation and the constructor record names the
        // target — two records, two declarations, one range.
        ("abbrev", |n| {
            format!("type {n}Base() =\n    inherit System.Attribute()\n\ntype {n} = {n}Base\n")
        }),
        ("exception", |n| format!("exception {n} of string\n")),
    ];
    let names = ["Mark", "MarkAttribute"];
    let writtens = ["Mark", "MarkAttribute"];

    let mut cells = Vec::new();
    // The **contested** shape, which the crossed axes above cannot express: a
    // cell declares `Mark` or `MarkAttribute`, never both, so only one
    // candidate can resolve and a resolver that tried the written name before
    // the suffixed one would answer every other cell identically. Here both are
    // declared at distinct ranges, so F#'s suffix-first walk is decidable from
    // the answer alone — `[<Mark>]` must bind `MarkAttribute`, and binding
    // `Mark` is exactly the wrong-order resolver. `[<MarkAttribute>]` binds it
    // too, by the second candidate, so the pair also pins that the first
    // candidate's *failure* falls through rather than ending the walk.
    for written in &writtens {
        let mut src = String::from("module Test\n\n");
        src.push_str("type Mark() =\n    inherit System.Attribute()\n\n");
        src.push_str("type MarkAttribute() =\n    inherit System.Attribute()\n\n");
        src.push_str(&format!("[<{written}>]\nlet x = 5\n"));
        cells.push(AttrCell {
            kind: "contested",
            label: format!("contested-written{written}"),
            src,
        });
    }
    for (kind, template) in &decl_kinds {
        for name in &names {
            for written in &writtens {
                let mut src = String::from("module Test\n\n");
                src.push_str(&template(name));
                src.push('\n');
                src.push_str(&format!("[<{written}>]\nlet x = 5\n"));
                cells.push(AttrCell {
                    kind,
                    label: format!("{kind}-decl{name}-written{written}"),
                    src,
                });
            }
        }
    }
    cells
}

/// The resident `uses-project-batch` child the generated sweep drives.
///
/// One child for the whole matrix. `invoke_fcs_uses_project` spawns a one-shot
/// per call and, without `BORZOI_FCS_DUMP`, builds `tools/fcs-dump` first — so
/// a cell-per-invocation loop pays a `dotnet build` and a process start twenty
/// times over, which is most of its wall clock and grows with every shape
/// added. The batch op exists for exactly this loop.
///
/// The request is the same project description the one-shot composes from
/// `LoadedProject`: these cells carry no references, no `#if` symbols and no
/// `<LangVersion>` pin, so `refs`/`defines`/`langversion` are empty and
/// `exclusiveRefs` is false — which is what the one-shot does when
/// `fcs_extra_refs` is empty (it *clears* `BORZOI_FCS_EXCLUSIVE_REFS` rather
/// than leaving an inherited one set). A cell that ever needs a reference set
/// must send it here too, or the oracle stops reading what our env was built
/// from.
///
/// Serialised behind a mutex because a resident oracle matches requests to
/// responses positionally, so it cannot serve concurrent callers.
///
/// One child across cells is safe for the isolation this sweep rests on — FCS
/// caches by file path and each cell writes its `A.fs` under its own temporary
/// directory — and that was checked rather than argued: the batched matrix
/// reports the same clean/erroring split, the same graded counts and the same
/// erroring-cell list as one one-shot invocation per cell did, in an eighth of
/// the wall clock. The single-project tests above stay on the one-shot driver,
/// which is the invocation the corpus runner itself makes.
fn uses_project_batch(paths: &[PathBuf]) -> String {
    static CHILD: OnceLock<Mutex<BatchChild>> = OnceLock::new();
    let child = CHILD.get_or_init(|| {
        Mutex::new(BatchChild::with_factory(
            Box::new(|| {
                fcs_dump_command("uses-project-batch").expect("build fcs-dump for the batch oracle")
            }),
            "fcs-dump uses-project-batch",
            PROJECT_ORACLE_TIMEOUT,
            2,
        ))
    });
    let request = serde_json::json!({
        "paths": paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "refs": Vec::<String>::new(),
        "exclusiveRefs": false,
    });
    let line = child
        .lock()
        .expect("batch oracle mutex")
        .request(&request.to_string());
    assert!(
        !line.contains("\"BatchError\""),
        "fcs-dump uses-project-batch refused the request: {line}"
    );
    line
}

/// What one cell produced: the comparison the corpus runner itself would make,
/// and the same comparison forced to grade a file whose check errored.
struct AttrCellOutcome {
    /// Exactly what the runner would do with this project. Empty of everything
    /// but `fcs_error_files` when the check errored, since
    /// `compare_project_uses` skips such a file before it looks at a single use.
    runner: Comparison,
    /// The comparison with the file's error diagnostics cleared, so the records
    /// FCS *did* emit are graded even for a cell that does not compile. The
    /// **real** comparator, not a second copy of its rules — only its input is
    /// relaxed. Identical to `runner` for a clean cell.
    forward: Comparison,
    /// `forward` narrowed to the written attribute range alone.
    ///
    /// The floor on an erroring cell has to be counted here, not on `forward`:
    /// that one considers every in-project use in the file — the generic
    /// parameter, the secondary constructor's call, the abbreviation's
    /// right-hand side — so it stays positive on declaration-body uses even if
    /// FCS stops emitting anything at all at the attribute range, which is the
    /// one range the sweep is about. Same defect as a whole-file
    /// `shadowed_constructor_use` floor, one range further out.
    ///
    /// Narrowing the comparator's *input* again rather than its rules: the
    /// oracle's records at that range are the whole input, so `uses_considered`
    /// is by construction the attribute range's own count.
    forward_at_attr: Comparison,
    fcs: Vec<FileUses>,
}

/// Run one generated cell as its own single-file project.
///
/// One project per cell, not one project of many files: a cell may declare its
/// attribute inside a module that later cells would see, and an
/// `[<AutoOpen>]`-shaped declaration leaks to every later file in the same
/// Compile order — so sharing a project would let one cell decide another's
/// verdict.
fn run_attribute_cell(cell: &AttrCell) -> AttrCellOutcome {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("AttrCell.fsproj");
    write(
        &project,
        "<Project>\n  <ItemGroup>\n    <Compile Include=\"A.fs\" />\n  </ItemGroup>\n</Project>\n",
    );
    write(&tmp.path().join("A.fs"), &cell.src);
    let loaded = load_lsp_project(&project)
        .unwrap_or_else(|e| panic!("cell {} should load: {e:?}", cell.label));
    let json = uses_project_batch(&loaded.parses.paths);
    let sources: Vec<_> = loaded
        .parses
        .paths
        .iter()
        .cloned()
        .zip(loaded.parses.texts.iter().cloned())
        .collect();
    let fcs = parse_project_uses(&json, &sources)
        .unwrap_or_else(|e| panic!("cell {}: parse FCS uses: {e:?}", cell.label));
    let runner = compare_project_uses(&loaded, &fcs);
    let mut relaxed = fcs.clone();
    for file in &mut relaxed {
        file.diagnostics.clear();
    }
    let forward = compare_project_uses(&loaded, &relaxed);
    let attr = attribute_name_range(&cell.src);
    let mut only_attr = relaxed.clone();
    for file in &mut only_attr {
        file.uses.retain(|u| (u.start, u.end) == attr);
    }
    let forward_at_attr = compare_project_uses(&loaded, &only_attr);
    AttrCellOutcome {
        runner,
        forward,
        forward_at_attr,
        fcs,
    }
}

/// The oracle records at `range`, split by whether they are constructor records
/// — the crowding this sweep is about, read at the written attribute name
/// itself rather than anywhere in the file.
fn records_at(fcs: &[FileUses], range: (usize, usize)) -> (usize, usize) {
    let at = fcs
        .iter()
        .flat_map(|f| f.uses.iter())
        .filter(|u| (u.start, u.end) == range);
    let mut constructors = 0;
    let mut named = 0;
    for u in at {
        if u.is_constructor {
            constructors += 1;
        } else {
            named += 1;
        }
    }
    (named, constructors)
}

/// The sweep: every generated attribute shape graded through the same
/// comparison the corpus runner uses.
///
/// `attr_resolution_sweep` in `borzoi-sema` already enumerates a larger
/// *semantic* matrix, but it rides on `fcs-dump attrs`, which emits one record
/// per attribute — it filters the sink to entity uses and groups by range, so
/// the constructor record is dropped before the harness ever sees it. That is a
/// sound projection for the question that oracle answers, and it makes the
/// oracle structurally unable to disagree with itself about how many answers a
/// site has. The corpus runner grades attributes through `uses-project`, where
/// both records survive and `oracle_shape` decides between them, so a shape
/// where the two streams part company can pass the sema sweep and diverge on a
/// real project — which is exactly what happened to the abbreviation-aliased
/// attribute, found by review rather than by any test.
///
/// This sweep is therefore not a second copy of that one. It asks the narrower
/// question the other cannot: for each shape, does the crowded range still
/// resolve to one adjudicable answer, and is it ours?
///
/// What it adds over [`alias_attribute_project_matches_fcs`] is **breadth, not
/// power against the known case** — measured, not assumed. Folding the
/// constructor-shadowing rule out of `oracle_shape` fails that fixture too, so
/// this does not catch the #226 defect any better; it catches the *next* shape
/// in that class. The mutation lands here on `multi-ctor`, a shape neither the
/// fixture (an abbreviation) nor the sema matrix (no multi-constructor axis)
/// contains, which is the whole argument for generating the population rather
/// than hand-picking a representative of it.
///
/// Certain-implies-exact per cell — our commit names FCS's resolution or we
/// decline.
///
/// The assertions are **per cell and at the written attribute range**, not
/// file-aggregate floors, because an aggregate floor here is satisfied by
/// accident: every class-based cell also contains `inherit System.Attribute()`,
/// which is itself a range carrying a type record and a constructor record, so
/// a whole-file `shadowed_constructor_use > 0` stays true even if no `[<...>]`
/// site shadows anything at all. Measured, not supposed — every clean cell
/// reports exactly two records at its attribute range, one of them a
/// constructor, and the file total is two per cell, so precisely half of it is
/// the inheritance.
///
/// **A cell whose check errors is graded, not skipped.** Only three of the four
/// name-against-written combinations resolve — F# tries `XAttribute` then `X`
/// and never strips a suffix, so a written `MarkAttribute` against a declared
/// `Mark` names neither — and an `exception` is not an `Attribute` subclass at
/// all. About half the matrix therefore type-checks with errors, currently
/// including *every* generic cell.
///
/// `compare_project_uses` drops such a file whole, before it looks at a single
/// use, so running it alone would leave those cells asserting nothing while
/// appearing to assert three things. They are graded by relaxing its *input*
/// instead — the diagnostics are cleared and the real comparator runs — and
/// only in the **forward** direction: an erroring check under-reports its sink,
/// so a record it never emitted is no evidence against a resolution of ours,
/// but a record it did emit still has to be what we say. That asymmetry is why
/// the reverse direction is asserted for clean cells only.
///
/// This does *not* catch a resolver that strips the `Attribute` suffix: FCS
/// emits nothing at the range it would bind, and absence under an erroring
/// check implicates nobody. Which cells error is read off FCS rather than
/// listed here — a hand-kept legality table would be one more thing to get
/// wrong, and it would go stale the moment a language version changed which
/// shapes compile.
///
/// **No coverage floor is an aggregate.** A count summed over cells and checked
/// after the loop is held up by whichever cells still work, so a whole axis can
/// go dark behind it — the same way a whole-file count is held up by a
/// declaration body. Both are the one mistake, at different widths, so the
/// floors here are stated at the only two widths that cannot hide a subset:
///
/// - **per cell**: if the oracle speaks at a cell's attribute range, that cell's
///   comparison must have graded it. Whether the oracle speaks is read from the
///   oracle, per cell, so the assertion is skipped only where there is provably
///   nothing to assert.
/// - **per declaration kind**: every kind the matrix generates must be graded
///   *somewhere*, in whichever population it lands in. That survives a language
///   version moving a kind between the two — which is a legitimate change, and
///   would make a floor keyed to one population fail for no reason — while
///   still failing loudly if a kind stops being graded at all. The list comes
///   from the generated cells, so it cannot fall behind the matrix.
#[test]
#[ignore = "builds/runs FCS; use --ignored for oracle smoke"]
fn generated_attribute_shapes_agree_with_the_project_use_stream() {
    let cells = attribute_matrix();
    eprintln!("attribute uses-project sweep: {} cells", cells.len());

    let mut attribute_commits = 0usize;
    let mut crowded_cells = 0usize;
    // Kinds graded as a compiling shape, and kinds graded only through the
    // relaxed comparison. Kept apart so the report says which population a kind
    // is covered by, and unioned for the floor so a kind moving between them is
    // not a failure.
    let mut clean_kinds: BTreeSet<&str> = BTreeSet::new();
    let mut erroring_kinds: BTreeSet<&str> = BTreeSet::new();
    let all_kinds: BTreeSet<&str> = cells.iter().map(|c| c.kind).collect();
    let mut errored: Vec<&str> = Vec::new();

    for cell in &cells {
        let outcome = run_attribute_cell(cell);
        // One path for every cell, with each assertion guarded by its own
        // precondition rather than by which half of an if/else it sits in.
        // The split version had every rule written twice — once for cells that
        // compile and once for the rest — and a rule added to one side and not
        // the other is silent, which is how three separate coverage holes got
        // in. Facts first, then assertions that name what they need.
        let clean = outcome.runner.fcs_error_files.is_empty();
        let attr = attribute_name_range(&cell.src);
        let (named, constructors) = records_at(&outcome.fcs, attr);
        let oracle_speaks_here = named + constructors > 0;
        let graded_at_attr = outcome.forward_at_attr.uses_considered > 0;

        if !clean {
            errored.push(&cell.label);
        }

        // Our commits must name what FCS reported, whether or not the rest of
        // the file type-checked.
        assert_eq!(
            outcome.forward.divergences,
            Vec::new(),
            "cell {}: project-declaration divergence",
            cell.label
        );
        assert_eq!(
            outcome.forward.assembly_divergences,
            Vec::new(),
            "cell {}: assembly-identity divergence",
            cell.label
        );

        // If the oracle spoke at this range, this cell's own comparison has to
        // have graded it — a total over cells would let the shapes that still
        // work vouch for the ones that stopped.
        if oracle_speaks_here {
            assert!(
                graded_at_attr,
                "cell {}: the oracle reports {} record(s) at the attribute \
                 range but the comparison graded none of them, so this cell's \
                 assertions examined nothing",
                cell.label,
                named + constructors
            );
        }

        if clean {
            assert_eq!(
                outcome.runner.reverse_divergences,
                Vec::new(),
                "cell {}: reverse divergence",
                cell.label
            );
            // A range the comparator cannot adjudicate is decided on the
            // oracle's answers alone, so it cannot produce a *wrong* pass — but
            // it silently drops exactly the per-shape comparison this sweep
            // exists to make, and a sweep that loses its subject while staying
            // green is the failure mode the whole file is written against.
            assert_eq!(
                outcome.runner.skipped_uses.ambiguous_oracle_range, 0,
                "cell {}: the attribute range stopped being adjudicable, so \
                 this shape is no longer compared at all",
                cell.label
            );
            assert_eq!(
                named, 1,
                "cell {}: the written attribute name must carry exactly one \
                 non-constructor record for the site to have an answer",
                cell.label
            );
            assert!(
                constructors >= 1,
                "cell {}: no constructor record at the attribute range, so \
                 this cell no longer exercises the crowding the sweep is \
                 about — the shape or the oracle changed under it",
                cell.label
            );
            // Per clean cell, not summed. A shape whose attribute answer
            // regresses to a deferral produces no *divergence* — the comparator
            // records a deferral, and `graded_at_attr` stays true because FCS
            // reported a use — so a total is held up by the shapes that still
            // resolve. Removing the resolver's fallback to the suffixed
            // candidate takes the total from eleven to four and every
            // divergence assertion stays green; this is what notices.
            assert!(
                outcome.runner.attribute_commits_compared >= 1,
                "cell {}: the attribute answer stopped being committed, so this \
                 shape is deferred rather than resolved and nothing here \
                 compares it",
                cell.label
            );
            crowded_cells += 1;
            attribute_commits += outcome.runner.attribute_commits_compared;
        }

        // Coverage is claimed only where the range was actually graded, in
        // either population: `records_at` reads the raw stream, so a record the
        // comparator's own eligibility rules skip is visible here while being
        // compared nowhere.
        if graded_at_attr {
            if clean {
                clean_kinds.insert(cell.kind);
            } else {
                erroring_kinds.insert(cell.kind);
            }
        }
    }

    // Every declaration kind the matrix generates has to be graded somewhere.
    // Not per population: a language version that starts or stops compiling a
    // kind moves it between the two legitimately, and a floor keyed to one
    // would fail for a change that lost no coverage at all. What it will not
    // survive is a kind that stops being graded in *either*.
    for kind in &all_kinds {
        assert!(
            clean_kinds.contains(kind) || erroring_kinds.contains(kind),
            "no {kind} cell was graded in either population, so the sweep no \
             longer covers that shape; graded clean {clean_kinds:?}, graded \
             through the relaxed comparison {erroring_kinds:?}, and these cells \
             errored: {errored:?}"
        );
    }
    // The three shapes whose attribute ranges are crowded *differently* — a
    // single constructor, an overload set, and an abbreviation whose two
    // records name different declarations — have to be graded as compiling
    // shapes specifically. That is the subject of the sweep, and the relaxed
    // comparison is a weaker check than the one they exist to get.
    for kind in ["class", "multi-ctor", "abbrev"] {
        assert!(
            clean_kinds.contains(kind),
            "no {kind} cell was graded as a clean check, so the crowded-range \
             comparison it exists for is no longer made; graded clean \
             {clean_kinds:?} and these cells errored: {errored:?}"
        );
    }

    eprintln!(
        "attribute uses-project sweep: {} cells checked clean ({crowded_cells} with a \
         crowded attribute range), {} graded through the relaxed comparison, \
         {attribute_commits} attribute commits compared; kinds graded clean \
         {clean_kinds:?}, kinds graded only relaxed {erroring_kinds:?}",
        cells.len() - errored.len(),
        errored.len(),
    );
    eprintln!("attribute uses-project sweep: cells whose check errored {errored:?}");
}

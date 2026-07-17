//! ProjectReference closure-walk tests.
//!
//! Split out of the former single-file `csharp_sidecar.rs`.

use super::support::*;

/// Phase 5 closure walk: a `buildMetadata` request on a project with a
/// `<ProjectReference>` returns the referenced project's emitted metadata
/// DLL in `transitive_project_refs`. The static `proj-ref` fixture has Top
/// referencing Leaf; we assert one transitive entry pointing at Leaf's
/// csproj and a readable LeafType in the published leaf DLL.
#[test]
fn build_metadata_proj_ref_surfaces_leaf_in_transitive_refs() {
    let dotnet = find_dotnet();

    let fixture = workspace_root().join("tools/csharp-sidecar/test-fixtures/proj-ref/top");
    let leaf_csproj =
        workspace_root().join("tools/csharp-sidecar/test-fixtures/proj-ref/leaf/Leaf.csproj");
    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let mut handle = build_and_start(&dotnet);
    let top_csproj = fixture.join("Top.csproj");
    let result = handle
        .build_metadata(
            &top_csproj,
            "Debug",
            "net10.0",
            &project_tfms_for(&top_csproj),
        )
        .expect("buildMetadata returns a result for Top with <ProjectReference>");

    assert_eq!(
        result.transitive_project_refs.len(),
        1,
        "expected exactly one transitive entry (Leaf), got {:?}",
        result.transitive_project_refs,
    );
    let entry = &result.transitive_project_refs[0];
    let entry_canon = entry
        .csproj_path
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", entry.csproj_path.display()));
    let leaf_canon = leaf_csproj
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", leaf_csproj.display()));
    assert_eq!(
        entry_canon, leaf_canon,
        "transitive entry should point at Leaf.csproj",
    );
    assert!(
        entry.metadata_dll_path.exists(),
        "leaf metadata DLL should exist at {}",
        entry.metadata_dll_path.display(),
    );
    // The leaf's DLL lives in the same cache root as the top's; verify so a
    // future refactor that accidentally publishes leaves elsewhere fails
    // loudly here rather than silently growing a parallel tree.
    let expected_root = workspace_root().join("obj/borzoi/csharp-sidecar");
    assert!(
        entry.metadata_dll_path.starts_with(&expected_root),
        "leaf DLL should be inside the workspace cache root: {}",
        entry.metadata_dll_path.display(),
    );

    let bytes = std::fs::read(&entry.metadata_dll_path).expect("read leaf DLL");
    let view = Ecma335Assembly::parse(&bytes).expect("parse leaf DLL");
    let entities = view
        .enumerate_type_defs()
        .expect("enumerate leaf type defs");
    let fqns: Vec<String> = entities
        .iter()
        .map(|e| {
            if e.namespace.is_empty() {
                e.name.clone()
            } else {
                format!("{}.{}", e.namespace.join("."), e.name)
            }
        })
        .collect();
    assert!(
        fqns.iter().any(|f| f == "LeafFixture.LeafType"),
        "expected LeafFixture.LeafType among emitted leaf types, got {fqns:?}",
    );

    handle.shutdown().expect("shutdown clean");
}

/// D12 differential: the sidecar's metadata-only emit of the *top* project
/// (which has a `<ProjectReference>` to a leaf) must agree with `dotnet
/// build -p:ProduceReferenceAssembly=true` on the TopFixture surface. The
/// closure walk runs internally for Top to be emittable — this test pins
/// the top-side correctness.
#[test]
#[ignore = "pre-existing limitation: the published ref-assembly's \
            return-type signatures carry custom modifiers we don't yet \
            decode (UnsupportedSignature). Fails on main with --ignored \
            too; this branch only lifts the .NET-SDK ignore gate."]
fn build_metadata_matches_dotnet_ref_assembly_proj_ref_top() {
    let dotnet = find_dotnet();
    diff_sidecar_against_ref_assembly(
        &dotnet,
        "proj-ref/top",
        "Top.csproj",
        "TopFixture",
        "TopFixture",
    );
}

/// D12 differential complement: the leaf DLL the sidecar published as part
/// of the closure walk must agree with the dedicated `dotnet build` of the
/// leaf project. Together with the top-side differential this pins both
/// halves of the closure-walk emit.
///
/// We discover the leaf DLL through the top's `transitive_project_refs`
/// (rather than building the leaf csproj directly through the sidecar) so
/// the test stays honest to the closure-walk code path.
#[test]
fn build_metadata_matches_dotnet_ref_assembly_proj_ref_leaf() {
    let dotnet = find_dotnet();

    let top_dir = workspace_root().join("tools/csharp-sidecar/test-fixtures/proj-ref/top");
    let leaf_dir = workspace_root().join("tools/csharp-sidecar/test-fixtures/proj-ref/leaf");
    let _fixture_lock = lock_fixture(&dotnet, &top_dir);

    let mut handle = build_and_start(&dotnet);
    let top_csproj = top_dir.join("Top.csproj");
    let result = handle
        .build_metadata(
            &top_csproj,
            "Debug",
            "net10.0",
            &project_tfms_for(&top_csproj),
        )
        .expect("sidecar buildMetadata returns a metadata DLL for top");
    handle.shutdown().expect("shutdown clean");

    assert_eq!(
        result.transitive_project_refs.len(),
        1,
        "expected exactly one transitive entry (Leaf), got {:?}",
        result.transitive_project_refs,
    );
    let sidecar_leaf_dll = &result.transitive_project_refs[0].metadata_dll_path;

    // dotnet build of the leaf csproj puts the ref assembly at the
    // standard MSBuild location. We don't reuse the top's build because
    // that emits TopFixture.dll and references LeafFixture.dll from
    // wherever ProjectReference resolution lands it; cleanest is a
    // dedicated leaf build.
    let dotnet_leaf_ref = build_ref_assembly(&dotnet, &leaf_dir, "LeafFixture", "Debug", "net10.0");
    assert!(
        dotnet_leaf_ref.exists(),
        "expected dotnet leaf ref assembly at {}",
        dotnet_leaf_ref.display(),
    );

    let sidecar_norm = read_and_normalise(sidecar_leaf_dll, "LeafFixture", "LeafFixture");
    let dotnet_norm = read_and_normalise(&dotnet_leaf_ref, "LeafFixture", "LeafFixture");

    assert_eq!(
        sidecar_norm, dotnet_norm,
        "sidecar's transitive leaf emit and `dotnet build -p:ProduceReferenceAssembly=true` \
         on the leaf disagree on the LeafFixture surface.\n  \
         sidecar: {sidecar_norm:#?}\n  dotnet:  {dotnet_norm:#?}",
    );
}

/// `buildMetadata` now performs the closure walk on the caller's behalf, so
/// successful-emit diagnostics on transitive projects (warnings, infos)
/// must flow back through the top response — otherwise the caller's only
/// signal that the leaf has a warning is a re-emit, which the cache makes
/// unlikely to happen again. A `#warning` in the leaf surfaces as CS1030
/// at parse time and rides Roslyn's `EmitResult.Diagnostics`; assert it
/// appears in the top's `diagnostics` array with the leaf's file path.
#[test]
fn build_metadata_proj_ref_aggregates_transitive_warnings() {
    let dotnet = find_dotnet();

    let fixture_root = workspace_root().join("target/csharp-sidecar-projref-warn-fixture");
    let _ = std::fs::remove_dir_all(&fixture_root);
    let nonce = unique_nonce();
    let (top_csproj, leaf_cs) = write_project_reference_fixture(&fixture_root, nonce, "warn-agg");
    let _fixture_lock = lock_fixture(&dotnet, top_csproj.parent().expect("top dir"));

    // Replace the default leaf content with a `#warning` directive; the
    // resulting CS1030 is a warning Roslyn surfaces during emit even with
    // `tolerateErrors: true`. Include the nonce in a comment so this test
    // body always hashes to a unique cache key: cache hits return empty
    // CompilerDiagnostics (the policy: diagnostics belong to the call that
    // re-derived them), which would mask the aggregation we want to test.
    // We need the re-emit path on every invocation.
    std::fs::write(
        &leaf_cs,
        format!(
            "// warn-agg nonce {nonce}\n#warning leaf-test-warning\nnamespace LeafFixture;\n\npublic sealed class LeafType {{ }}\n"
        ),
    )
    .expect("rewrite leaf cs with #warning");

    let mut handle = build_and_start(&dotnet);
    let result = handle
        .build_metadata(
            &top_csproj,
            "Debug",
            "net10.0",
            &project_tfms_for(&top_csproj),
        )
        .expect("closure walk should emit successfully despite #warning");

    let leaf_canon = leaf_cs
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", leaf_cs.display()));

    let leaf_warning = result.diagnostics.iter().find(|d| {
        d.id == "CS1030"
            && d.file_path
                .as_deref()
                .and_then(|p| std::fs::canonicalize(p).ok())
                .as_ref()
                == Some(&leaf_canon)
    });
    assert!(
        leaf_warning.is_some(),
        "expected CS1030 from leaf to appear in top response.diagnostics; got {:?}",
        result.diagnostics,
    );

    handle.shutdown().expect("shutdown clean");
}

/// D7 failure-mode: when a leaf project's emit fails (here, CS0101 from
/// duplicate type declarations that even `tolerateErrors: true` can't get
/// around), the sidecar's closure walk surfaces `BuildFailed` for the top
/// call rather than silently dropping the leaf and emitting top against a
/// stale DLL. The diagnostics carry the leaf's CS0101 because the top can't
/// build without it.
#[test]
fn build_metadata_proj_ref_leaf_emit_failure_returns_build_failed() {
    let dotnet = find_dotnet();

    let fixture_root = workspace_root().join("target/csharp-sidecar-projref-fail-fixture");
    let _ = std::fs::remove_dir_all(&fixture_root);
    let (top_csproj, leaf_cs) =
        write_project_reference_fixture(&fixture_root, unique_nonce(), "leaf-fail");
    let _fixture_lock = lock_fixture(&dotnet, top_csproj.parent().expect("top dir"));

    // Break the leaf with two declarations of the same type name. Roslyn
    // refuses to emit two metadata entries for the same fully-qualified
    // type name even under tolerateErrors=true (a fault `metadata-only`
    // skips body analysis cannot paper over).
    std::fs::write(
        &leaf_cs,
        "namespace LeafFixture;\n\npublic sealed class LeafType { }\npublic sealed class LeafType { }\n",
    )
    .expect("rewrite leaf cs with CS0101");

    let mut handle = build_and_start(&dotnet);
    let err = handle
        .build_metadata(
            &top_csproj,
            "Debug",
            "net10.0",
            &project_tfms_for(&top_csproj),
        )
        .expect_err("closure walk should surface the leaf's emit failure");

    match &err {
        SidecarError::Sidecar {
            kind: SidecarErrorKind::BuildFailed { diagnostics, .. },
            ..
        } => {
            assert!(
                diagnostics.iter().any(|d| d.id == "CS0101"),
                "expected CS0101 in surfaced diagnostics, got {diagnostics:?}",
            );
        }
        other => panic!("expected BuildFailed surfaced from leaf, got {other:?}"),
    }

    handle.shutdown().expect("shutdown clean");
}

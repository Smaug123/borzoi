//! Multi-target-framework resolution tests.
//!
//! Split out of the former single-file `csharp_sidecar.rs`.

use super::support::*;

/// Phase 4 of `docs/completed/multi-tfm-resolution-plan.md` — the load-bearing
/// integration test for the multi-TFM picker: the multi-tfm fixture has a
/// top targeting `net10.0` consuming a leaf with `<TargetFrameworks>
/// netstandard2.0;net6.0</TargetFrameworks>`. NuGet's nearest-compatible
/// algorithm resolves `net6.0` for the leaf (the closer match), and the
/// closure walker exposes that as the per-project TFM in
/// `project_tfms_for`. The sidecar's `BuildMetadata` must then emit the
/// leaf under `net6.0` (not `netstandard2.0` and not `net10.0`) and the top
/// under `net10.0` with the leaf substituted in as a
/// `PortableExecutableReference`.
///
/// The test pins three things in one call:
///   * The Rust-side closure walker returns `net6.0` for the leaf.
///   * The sidecar's transitive emit completes (a regression in the picker
///     would surface here as `MissingProjectTfm` or a load failure).
///   * The leaf's emitted DLL contains the `MultiTfmLeaf.LeafBeacon` type —
///     i.e. the per-project workspace successfully loaded the right TFM and
///     produced a real metadata DLL.
#[test]
fn build_metadata_multi_tfm_picker_emits_leaf_under_net6() {
    let dotnet = find_dotnet();

    let fixture = workspace_root().join("tools/csharp-sidecar/test-fixtures/multi-tfm/top");
    let leaf_csproj =
        workspace_root().join("tools/csharp-sidecar/test-fixtures/multi-tfm/leaf/Leaf.csproj");
    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let top_csproj = fixture.join("Top.csproj");
    let tfms = project_tfms_for(&top_csproj);
    // Find the leaf's resolved TFM by canonical path; the top csproj points
    // at `../leaf/Leaf.csproj` and the closure walker preserves that join
    // shape, so we canonicalise both sides for the comparison.
    let leaf_canon = leaf_csproj
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", leaf_csproj.display()));
    let leaf_tfm = tfms
        .iter()
        .find(|(p, _)| p.canonicalize().map(|c| c == leaf_canon).unwrap_or(false))
        .map(|(_, t)| t.as_str())
        .unwrap_or_else(|| {
            panic!("project_tfms_for did not return an entry for the leaf; got {tfms:?}")
        });
    assert_eq!(
        leaf_tfm, "net6.0",
        "NuGet's nearest-compatible algorithm should resolve net6.0 for the leaf \
         (consumer is net10.0, producer offers netstandard2.0;net6.0); got {leaf_tfm:?}",
    );

    let mut handle = build_and_start(&dotnet);
    let result = handle
        .build_metadata(&top_csproj, "Debug", "net10.0", &tfms)
        .expect("buildMetadata returns a result for the multi-tfm top");

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
    assert_eq!(
        entry_canon, leaf_canon,
        "transitive entry should point at the multi-tfm Leaf.csproj",
    );
    assert!(
        entry.metadata_dll_path.exists(),
        "leaf metadata DLL should exist at {}",
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
        fqns.iter().any(|f| f == "MultiTfmLeaf.LeafBeacon"),
        "expected MultiTfmLeaf.LeafBeacon among emitted leaf types, got {fqns:?}",
    );

    // Sanity: the leaf was emitted under net6.0, so the DLL's referenced
    // System.Runtime/mscorlib should be the net6.0 surface. We don't need a
    // full version match here (the assembly normaliser handles that for the
    // differential below); the type-existence check above is the load-
    // bearing assertion for this test.
    handle.shutdown().expect("shutdown clean");
}

/// Multi-TFM differential: the sidecar's metadata-only emit of the leaf
/// must agree with `dotnet build -p:ProduceReferenceAssembly=true
/// -p:TargetFramework=net6.0` on the LeafBeacon surface. This is the
/// closure-walk equivalent of `build_metadata_matches_dotnet_ref_assembly_
/// proj_ref_leaf`, except the producer csproj is multi-TFM and the picked
/// TFM is `net6.0` rather than the consumer's `net10.0`. A bug in either
/// half (sidecar picking the wrong TFM, or `dotnet build` not getting the
/// override) would surface as a normalised-assembly diff.
#[test]
fn build_metadata_matches_dotnet_ref_assembly_multi_tfm_leaf() {
    let dotnet = find_dotnet();

    let top_dir = workspace_root().join("tools/csharp-sidecar/test-fixtures/multi-tfm/top");
    let leaf_dir = workspace_root().join("tools/csharp-sidecar/test-fixtures/multi-tfm/leaf");
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
        .expect("sidecar buildMetadata returns a metadata DLL for the multi-tfm top");
    handle.shutdown().expect("shutdown clean");

    assert_eq!(
        result.transitive_project_refs.len(),
        1,
        "expected exactly one transitive entry (Leaf), got {:?}",
        result.transitive_project_refs,
    );
    let sidecar_leaf_dll = &result.transitive_project_refs[0].metadata_dll_path;

    let dotnet_leaf_ref = build_ref_assembly(&dotnet, &leaf_dir, "MultiTfmLeaf", "Debug", "net6.0");
    assert!(
        dotnet_leaf_ref.exists(),
        "expected dotnet leaf ref assembly at {}",
        dotnet_leaf_ref.display(),
    );

    let sidecar_norm = read_and_normalise(sidecar_leaf_dll, "MultiTfmLeaf", "MultiTfmLeaf");
    let dotnet_norm = read_and_normalise(&dotnet_leaf_ref, "MultiTfmLeaf", "MultiTfmLeaf");

    assert_eq!(
        sidecar_norm, dotnet_norm,
        "sidecar's multi-tfm leaf emit and `dotnet build \
         -p:ProduceReferenceAssembly=true -p:TargetFramework=net6.0` on the leaf \
         disagree on the MultiTfmLeaf surface.\n  \
         sidecar: {sidecar_norm:#?}\n  dotnet:  {dotnet_norm:#?}",
    );
}

/// Multi-TFM with a TFM-conditional inner `<ProjectReference>` — the
/// shape that pins the per-workspace edge discovery in `EmitClosure`.
///
/// The fixture's leaf declares
/// `<TargetFrameworks>netstandard2.0;net6.0</TargetFrameworks>` and has a
/// `<ProjectReference Include="..\polyfill\Polyfill.csproj"
/// Condition="'$(TargetFramework)' == 'net6.0'" />`. NuGet's
/// nearest-compatible algorithm picks `net6.0` for the leaf when consumed
/// from a `net10.0` top, so the inner ref fires; the closure walker's
/// project.assets.json read consequently includes the polyfill leaf in
/// `projectTfms`.
///
/// The sidecar's earlier shared-workspace implementation read direct
/// `<ProjectReference>` edges from the *top* workspace's view of the leaf
/// (which evaluates the leaf under the top's `net10.0`, falling back to
/// the first listed TFM `netstandard2.0` — under which the conditional ref
/// does NOT fire). It would consequently topo-sort `[leaf, top]`, then
/// crash inside `EmitOneInWorkspace` when emitting the leaf in its own
/// per-project workspace (which DOES see polyfill) because polyfill was
/// never emitted in a prior iteration. The Phase 4 rewrite reads edges
/// from each project's own per-project workspace, so the topo order
/// correctly includes `[polyfill, leaf, top]`.
///
/// The test pins:
///   * The Rust closure walker discovers all three projects with TFMs
///     `{top: net10.0, leaf: net6.0, polyfill: net6.0}`.
///   * `buildMetadata` returns two transitive entries (leaf, polyfill).
///   * Both emitted DLLs exist and contain their respective beacon types.
#[test]
fn build_metadata_multi_tfm_cond_inner_ref_discovered_under_net6() {
    let dotnet = find_dotnet();

    let top_dir = workspace_root().join("tools/csharp-sidecar/test-fixtures/multi-tfm-cond/top");
    let leaf_csproj =
        workspace_root().join("tools/csharp-sidecar/test-fixtures/multi-tfm-cond/leaf/Leaf.csproj");
    let polyfill_csproj = workspace_root()
        .join("tools/csharp-sidecar/test-fixtures/multi-tfm-cond/polyfill/Polyfill.csproj");
    let _fixture_lock = lock_fixture(&dotnet, &top_dir);

    let top_csproj = top_dir.join("Top.csproj");
    let tfms = project_tfms_for(&top_csproj);

    // The Rust closure walker should surface all three projects. We
    // canonicalise both sides because the closure walker preserves the
    // raw join shape (`top_dir/../leaf/Leaf.csproj`) while
    // std::fs::canonicalize emits the realpath form.
    let leaf_canon = leaf_csproj
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", leaf_csproj.display()));
    let polyfill_canon = polyfill_csproj
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", polyfill_csproj.display()));

    let leaf_tfm = tfms
        .iter()
        .find(|(p, _)| p.canonicalize().map(|c| c == leaf_canon).unwrap_or(false))
        .map(|(_, t)| t.as_str())
        .unwrap_or_else(|| panic!("project_tfms_for did not include the leaf; got {tfms:?}"));
    assert_eq!(
        leaf_tfm, "net6.0",
        "leaf should resolve to net6.0 (nearest-compatible to net10.0 consumer)",
    );
    let polyfill_tfm = tfms
        .iter()
        .find(|(p, _)| {
            p.canonicalize()
                .map(|c| c == polyfill_canon)
                .unwrap_or(false)
        })
        .map(|(_, t)| t.as_str())
        .unwrap_or_else(|| {
            panic!(
                "project_tfms_for did not include the polyfill leaf — the closure walker \
                 missed the TFM-conditional inner ref. Got {tfms:?}",
            )
        });
    assert_eq!(
        polyfill_tfm, "net6.0",
        "polyfill is single-TFM net6.0; should resolve as such",
    );

    let mut handle = build_and_start(&dotnet);
    let result = handle
        .build_metadata(&top_csproj, "Debug", "net10.0", &tfms)
        .expect("buildMetadata returns a result for the multi-tfm-cond top");
    handle.shutdown().expect("shutdown clean");

    // Both leaves should appear in transitive_project_refs. The order is
    // sorted by csproj path inside the sidecar, so we look them up by
    // canonical path.
    assert_eq!(
        result.transitive_project_refs.len(),
        2,
        "expected exactly two transitive entries (leaf + polyfill), got {:?}",
        result.transitive_project_refs,
    );
    let entry_for = |canon: &Path| -> &TransitiveProjectRef {
        result
            .transitive_project_refs
            .iter()
            .find(|r| {
                r.csproj_path
                    .canonicalize()
                    .map(|c| c == canon)
                    .unwrap_or(false)
            })
            .unwrap_or_else(|| {
                panic!(
                    "no transitive entry for {}; got {:?}",
                    canon.display(),
                    result.transitive_project_refs,
                )
            })
    };
    let leaf_entry = entry_for(&leaf_canon);
    let polyfill_entry = entry_for(&polyfill_canon);
    assert!(
        leaf_entry.metadata_dll_path.exists(),
        "leaf metadata DLL should exist at {}",
        leaf_entry.metadata_dll_path.display(),
    );
    assert!(
        polyfill_entry.metadata_dll_path.exists(),
        "polyfill metadata DLL should exist at {}",
        polyfill_entry.metadata_dll_path.display(),
    );

    // Type-existence checks pin that the per-project workspaces actually
    // emitted real metadata (vs. e.g. an empty DLL).
    let assert_has_type = |dll: &Path, fqn: &str| {
        let bytes = std::fs::read(dll).expect("read DLL");
        let view = Ecma335Assembly::parse(&bytes).expect("parse DLL");
        let entities = view.enumerate_type_defs().expect("enumerate type defs");
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
            fqns.iter().any(|f| f == fqn),
            "expected {fqn} in {}, got {fqns:?}",
            dll.display(),
        );
    };
    assert_has_type(&leaf_entry.metadata_dll_path, "MultiTfmCondLeaf.LeafBeacon");
    assert_has_type(
        &polyfill_entry.metadata_dll_path,
        "MultiTfmCondPolyfill.PolyfillBeacon",
    );
}

/// Phase 5 of `docs/completed/multi-tfm-resolution-plan.md`: cross-language closure walk.
///
/// The fixture wraps the same TFM-conditional shape from
/// `multi-tfm-cond` in an F# top `.fsproj` that `<ProjectReference>`s a
/// multi-TFM C# leaf (`netstandard2.0;net6.0`) whose own conditional inner
/// ref to a `net6.0` polyfill fires only under `net6.0`. The F# top targets
/// `net10.0`, so NuGet's nearest-compatible algorithm selects `net6.0` for
/// the leaf and the polyfill is pulled in.
///
/// The closure walker (`resolve_transitive_project_tfms`) is language-
/// agnostic: it reads `project.assets.json`, which NuGet writes for fsproj
/// and csproj alike. This test pins that an `.fsproj` root surfaces the
/// same C# subtree map a sibling `.csproj` root would, so the LSP can drop
/// the fsproj entry (MSBuildWorkspace cannot load `.fsproj`) and hand the
/// rest to the sidecar without language-specific branching at the
/// closure-discovery layer.
#[test]
fn build_metadata_multi_tfm_cond_fsharp_top_dispatches_csharp_subtree() {
    let dotnet = find_dotnet();

    let fixture_root =
        workspace_root().join("tools/csharp-sidecar/test-fixtures/multi-tfm-cond-fsharp");
    let fsharp_top_dir = fixture_root.join("fsharp-top");
    let fsproj = fsharp_top_dir.join("Top.fsproj");
    let leaf_csproj = fixture_root.join("leaf/Leaf.csproj");
    let polyfill_csproj = fixture_root.join("polyfill/Polyfill.csproj");
    let _fixture_lock = lock_fixture(&dotnet, &fsharp_top_dir);

    // The closure walker should accept the fsproj root and surface every
    // producer that NuGet recorded — including the C# leaves transitively
    // reached through the F# top's project graph.
    let tfms = project_tfms_for(&fsproj);
    assert_eq!(
        tfms.len(),
        3,
        "expected three closure entries (fsproj + leaf + polyfill), got {tfms:?}",
    );

    let leaf_canon = leaf_csproj
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", leaf_csproj.display()));
    let polyfill_canon = polyfill_csproj
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", polyfill_csproj.display()));
    let fsproj_canon = fsproj
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", fsproj.display()));

    let tfm_for = |canon: &Path| -> &str {
        tfms.iter()
            .find(|(p, _)| p.canonicalize().map(|c| c == *canon).unwrap_or(false))
            .map(|(_, t)| t.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "project_tfms_for did not include {}; got {tfms:?}",
                    canon.display(),
                )
            })
    };
    assert_eq!(
        tfm_for(&fsproj_canon),
        "net10.0",
        "F# top should resolve to its declared net10.0",
    );
    assert_eq!(
        tfm_for(&leaf_canon),
        "net6.0",
        "leaf should resolve to net6.0 (nearest-compatible to net10.0 consumer)",
    );
    assert_eq!(
        tfm_for(&polyfill_canon),
        "net6.0",
        "polyfill is single-TFM net6.0; should resolve as such",
    );

    // The fsproj cannot be loaded by MSBuildWorkspace, so the LSP filters
    // the closure to its C#-rooted subtree before dispatching to the
    // sidecar. The sidecar's `MissingProjectTfm` invariant (D5) requires
    // the root csproj to appear in the map, so we keep the leaf and
    // polyfill entries.
    let csharp_only_tfms: BTreeMap<PathBuf, String> = tfms
        .iter()
        .filter(|(p, _)| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("csproj"))
        })
        .map(|(p, t)| (p.clone(), t.clone()))
        .collect();
    assert_eq!(
        csharp_only_tfms.len(),
        2,
        "filtering to csproj should leave leaf + polyfill, got {csharp_only_tfms:?}",
    );

    let mut handle = build_and_start(&dotnet);
    let result = handle
        .build_metadata(&leaf_csproj, "Debug", "net6.0", &csharp_only_tfms)
        .expect("buildMetadata on the C# leaf reachable through the F# top");
    handle.shutdown().expect("shutdown clean");

    // Polyfill is the leaf's only ProjectReference under net6.0, so the
    // transitive list has exactly one entry.
    assert_eq!(
        result.transitive_project_refs.len(),
        1,
        "expected one transitive entry (polyfill), got {:?}",
        result.transitive_project_refs,
    );
    let polyfill_entry = result
        .transitive_project_refs
        .iter()
        .find(|r| {
            r.csproj_path
                .canonicalize()
                .map(|c| c == polyfill_canon)
                .unwrap_or(false)
        })
        .unwrap_or_else(|| {
            panic!(
                "no transitive entry for polyfill; got {:?}",
                result.transitive_project_refs,
            )
        });
    assert!(
        result.metadata_dll_path.exists(),
        "leaf metadata DLL should exist at {}",
        result.metadata_dll_path.display(),
    );
    assert!(
        polyfill_entry.metadata_dll_path.exists(),
        "polyfill metadata DLL should exist at {}",
        polyfill_entry.metadata_dll_path.display(),
    );

    let assert_has_type = |dll: &Path, fqn: &str| {
        let bytes = std::fs::read(dll).expect("read DLL");
        let view = Ecma335Assembly::parse(&bytes).expect("parse DLL");
        let entities = view.enumerate_type_defs().expect("enumerate type defs");
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
            fqns.iter().any(|f| f == fqn),
            "expected {fqn} in {}, got {fqns:?}",
            dll.display(),
        );
    };
    assert_has_type(
        &result.metadata_dll_path,
        "MultiTfmCondFsharpLeaf.LeafBeacon",
    );
    assert_has_type(
        &polyfill_entry.metadata_dll_path,
        "MultiTfmCondFsharpPolyfill.PolyfillBeacon",
    );
}

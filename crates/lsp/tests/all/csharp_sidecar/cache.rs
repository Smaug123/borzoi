//! Content-addressed cache-behaviour tests.
//!
//! Split out of the former single-file `csharp_sidecar.rs`.

use super::support::*;

/// D6 idempotency: a second `buildMetadata` for the same inputs hits the
/// content-addressed cache. Same content hash, same DLL path, `from_cache`
/// flips from `false` to `true`. We use a fresh temp fixture so the test is
/// independent of prior `cargo test` runs that may have warmed the cache.
#[test]
fn build_metadata_second_call_with_same_inputs_hits_cache() {
    let dotnet = find_dotnet();

    let fixture = workspace_root().join("target/csharp-sidecar-idempotency-fixture");
    let _ = std::fs::remove_dir_all(&fixture);
    std::fs::create_dir_all(&fixture).expect("create temp fixture root");

    std::fs::write(
        fixture.join("Idem.csproj"),
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>IdemFixture</RootNamespace>\n",
            "    <AssemblyName>IdemFixture</AssemblyName>\n",
            "  </PropertyGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write csproj");
    // Embed a per-invocation nonce so the cache hash is unique to this
    // `cargo test` run. The fixture root is wiped above, but the published
    // DLL lives at `<workspace>/obj/borzoi/csharp-sidecar/...`
    // and persists across runs. Without the nonce, the second `cargo test`
    // would find a cache hit on the *first* call and fail the
    // `!first.from_cache` assertion.
    let nonce = unique_nonce();
    std::fs::write(
        fixture.join("Idem.cs"),
        format!("// nonce {nonce}\nnamespace IdemFixture;\n\npublic sealed class Idem {{ }}\n"),
    )
    .expect("write cs");

    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let mut handle = build_and_start(&dotnet);
    let csproj = fixture.join("Idem.csproj");
    let project_tfms = project_tfms_for(&csproj);
    let first = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms)
        .expect("first emit");
    let second = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms)
        .expect("second emit");

    assert_eq!(
        first.content_hash, second.content_hash,
        "identical inputs must yield identical content hashes"
    );
    assert_eq!(
        first.metadata_dll_path, second.metadata_dll_path,
        "identical inputs must publish to the same path"
    );
    assert!(
        !first.from_cache,
        "first call on a fresh fixture cannot be from_cache"
    );
    assert!(
        second.from_cache,
        "second call with same inputs must be served from the cache"
    );

    handle.shutdown().expect("shutdown clean");
}

/// D6 input-sensitivity: mutating any single byte of any input must change
/// the content hash, and the next call must therefore miss the cache and
/// land a new DLL at a different path. Counterpart to the idempotency
/// test — together they pin "key is a function of the inputs."
#[test]
fn build_metadata_source_mutation_changes_content_hash() {
    let dotnet = find_dotnet();

    let fixture = workspace_root().join("target/csharp-sidecar-mutation-fixture");
    let _ = std::fs::remove_dir_all(&fixture);
    std::fs::create_dir_all(&fixture).expect("create temp fixture root");

    std::fs::write(
        fixture.join("Mut.csproj"),
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>MutFixture</RootNamespace>\n",
            "    <AssemblyName>MutFixture</AssemblyName>\n",
            "  </PropertyGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write csproj");
    // See the idempotency test for why we need a per-invocation nonce.
    let nonce = unique_nonce();
    let cs_path = fixture.join("Mut.cs");
    std::fs::write(
        &cs_path,
        format!("// nonce {nonce}\nnamespace MutFixture;\n\npublic sealed class Mut {{ }}\n"),
    )
    .expect("write cs");

    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let mut handle = build_and_start(&dotnet);
    let csproj = fixture.join("Mut.csproj");
    let project_tfms = project_tfms_for(&csproj);
    let first = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms)
        .expect("first emit");

    // Add a public member — alters the source bytes (and the emitted
    // metadata, but the key is what we're testing here). Keep the same
    // nonce so the only delta is the new member.
    std::fs::write(
        &cs_path,
        format!(
            "// nonce {nonce}\nnamespace MutFixture;\n\npublic sealed class Mut {{ public int X; }}\n"
        ),
    )
    .expect("write mutated cs");

    let second = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms)
        .expect("second emit on mutated source");

    assert_ne!(
        first.content_hash, second.content_hash,
        "a single-byte change to any source must yield a different content hash"
    );
    assert_ne!(
        first.metadata_dll_path, second.metadata_dll_path,
        "different content hashes must land at different cache paths"
    );
    assert!(
        !second.from_cache,
        "mutated source cannot be served from a cache populated by the prior bytes"
    );
    assert!(
        second.metadata_dll_path.exists(),
        "second emit should publish a new DLL at {}",
        second.metadata_dll_path.display()
    );

    handle.shutdown().expect("shutdown clean");
}

/// Phase 4's cache key is rooted in Roslyn's internal `GetDeterministicKey`,
/// which already cascades through `CompilationReference` MVIDs (made stable
/// by `WithDeterministic(true)` on every compilation we drive). So a project
/// with a `<ProjectReference>` whose inputs do not change must report
/// `from_cache=true` on the second call, exactly like a leaf project does.
/// The complementary [`build_metadata_with_project_reference_leaf_mutation_invalidates_top`]
/// test pins the other half of the property: that the cascade actually
/// invalidates when the referenced project changes.
#[test]
fn build_metadata_with_project_reference_serves_cache_when_unchanged() {
    let dotnet = find_dotnet();

    let fixture_root = workspace_root().join("target/csharp-sidecar-projref-cached-fixture");
    let _ = std::fs::remove_dir_all(&fixture_root);
    let (top_csproj, _leaf_cs) =
        write_project_reference_fixture(&fixture_root, unique_nonce(), "cached");
    let _fixture_lock = lock_fixture(&dotnet, top_csproj.parent().expect("top dir"));

    let mut handle = build_and_start(&dotnet);
    let project_tfms = project_tfms_for(&top_csproj);
    let first = handle
        .build_metadata(&top_csproj, "Debug", "net10.0", &project_tfms)
        .expect("first emit");
    let second = handle
        .build_metadata(&top_csproj, "Debug", "net10.0", &project_tfms)
        .expect("second emit");

    assert!(
        !first.from_cache,
        "first call on a fresh fixture cannot be from_cache"
    );
    assert!(
        second.from_cache,
        "second call with unchanged inputs and a <ProjectReference> must serve from cache; \
         the GetDeterministicKey cascade through CompilationReference MVIDs makes the leaf's \
         identity part of Top's key without re-emitting Top",
    );
    assert_eq!(
        first.content_hash, second.content_hash,
        "identical inputs must yield identical content hashes even with project refs",
    );
    assert_eq!(
        first.metadata_dll_path, second.metadata_dll_path,
        "identical inputs must publish to the same path",
    );
    // The transitive entries must be byte-stable across calls: same closure,
    // same leaf csproj, same content-addressed dll path.
    assert_eq!(
        first.transitive_project_refs, second.transitive_project_refs,
        "transitive_project_refs must be stable across identical-input calls",
    );
    assert_eq!(
        first.transitive_project_refs.len(),
        1,
        "Top has one direct <ProjectReference>; closure must surface it",
    );

    handle.shutdown().expect("shutdown clean");
}

/// Cascade-invalidation: mutating a `<ProjectReference>`'s source must change
/// the depending project's content hash. Roslyn's deterministic-key output
/// includes referenced compilations' MVIDs; with `WithDeterministic(true)`
/// the MVID is a content hash, so a leaf-source change propagates into
/// Top's cache key without our code walking the reference closure. This is
/// the load-bearing test for the cascade.
#[test]
fn build_metadata_with_project_reference_leaf_mutation_invalidates_top() {
    let dotnet = find_dotnet();

    let fixture_root = workspace_root().join("target/csharp-sidecar-projref-cascade-fixture");
    let _ = std::fs::remove_dir_all(&fixture_root);
    let (top_csproj, leaf_cs) =
        write_project_reference_fixture(&fixture_root, unique_nonce(), "cascade");
    let _fixture_lock = lock_fixture(&dotnet, top_csproj.parent().expect("top dir"));

    let mut handle = build_and_start(&dotnet);
    let project_tfms = project_tfms_for(&top_csproj);
    let first = handle
        .build_metadata(&top_csproj, "Debug", "net10.0", &project_tfms)
        .expect("first emit on unchanged sources");

    // Mutate Leaf's public surface. Top.cs is untouched.
    std::fs::write(
        &leaf_cs,
        "namespace LeafFixture;\n\npublic sealed class LeafType { public int Added; }\n",
    )
    .expect("rewrite leaf cs");

    let second = handle
        .build_metadata(&top_csproj, "Debug", "net10.0", &project_tfms)
        .expect("second emit after mutating leaf");

    assert_ne!(
        first.content_hash, second.content_hash,
        "mutating a <ProjectReference>'s source must change the depending project's cache key — \
         the deterministic-key cascade through CompilationReference is the whole point",
    );
    assert_ne!(
        first.metadata_dll_path, second.metadata_dll_path,
        "different content hashes must land at different cache paths",
    );
    assert!(
        !second.from_cache,
        "post-mutation emit cannot be from_cache: nothing at the new key was on disk",
    );
    assert!(
        first.metadata_dll_path.exists(),
        "first emit's DLL must remain on disk (it is still correct for its inputs): {}",
        first.metadata_dll_path.display(),
    );
    assert!(
        second.metadata_dll_path.exists(),
        "second emit should publish a fresh DLL at {}",
        second.metadata_dll_path.display(),
    );
    // The leaf surfaced in transitive_project_refs must itself have moved
    // to a different content-hashed path: its bytes changed, so its cache
    // key must too.
    assert_eq!(
        first.transitive_project_refs.len(),
        1,
        "Top has one ProjectReference"
    );
    assert_eq!(
        second.transitive_project_refs.len(),
        1,
        "Top has one ProjectReference"
    );
    assert_ne!(
        first.transitive_project_refs[0].metadata_dll_path,
        second.transitive_project_refs[0].metadata_dll_path,
        "leaf source mutation must move its emitted DLL to a new content-addressed path",
    );

    handle.shutdown().expect("shutdown clean");
}

/// `<AssemblyName>` is a project-controlled string; it must not be able
/// to influence the cache filename in any way that escapes
/// `obj/borzoi/csharp-sidecar/`. Under phase 4 the filename is the
/// lowercase-hex SHA-256 of the build's inputs — no project-controlled string
/// reaches the path layer. This regression test exists to catch any future
/// change that reintroduces project-controlled input on the path side. The
/// `escape-attempt` fixture sets `<AssemblyName>../escape-attempt`; the
/// emitted path must still be strictly inside the cache root and its
/// filename component must have no `/`, `\`, or `..` segments.
#[test]
fn build_metadata_sanitises_hostile_assembly_name() {
    let dotnet = find_dotnet();
    let fixture = workspace_root().join("tools/csharp-sidecar/test-fixtures/escape-attempt");
    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let mut handle = build_and_start(&dotnet);
    let csproj = fixture.join("Escape.csproj");
    let result = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms_for(&csproj))
        .expect("metadata-only emit should succeed even with a hostile AssemblyName");

    let expected_root = workspace_root().join("obj/borzoi/csharp-sidecar");
    let canonical_root = expected_root
        .canonicalize()
        .expect("cache root should exist after the emit");
    let canonical_dll = result
        .metadata_dll_path
        .canonicalize()
        .expect("emitted DLL should exist");
    assert!(
        canonical_dll.starts_with(&canonical_root),
        "emit escaped the cache root: {} not under {}",
        canonical_dll.display(),
        canonical_root.display(),
    );
    // No directory component should have leaked into the filename.
    let file_name = result
        .metadata_dll_path
        .file_name()
        .and_then(|n| n.to_str())
        .expect("emitted DLL has a filename");
    assert!(
        !file_name.contains('/') && !file_name.contains('\\') && !file_name.contains(".."),
        "emit filename `{file_name}` still contains a path-like fragment",
    );

    handle.shutdown().expect("shutdown clean");
}

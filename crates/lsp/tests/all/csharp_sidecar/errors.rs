//! Error and failure-mode tests.
//!
//! Split out of the former single-file `csharp_sidecar.rs`.

use super::support::*;

#[test]
fn build_metadata_missing_csproj_reports_not_found() {
    let dotnet = find_dotnet();
    let mut handle = build_and_start(&dotnet);
    let csproj = PathBuf::from("/definitely/does/not/exist/Foo.csproj");
    let err = handle
        .build_metadata(&csproj, "Debug", "net10.0", &BTreeMap::new())
        .expect_err("missing csproj should error");

    match err {
        SidecarError::Sidecar {
            kind: SidecarErrorKind::CsprojNotFound { csproj_path },
            ..
        } => assert_eq!(csproj_path, csproj.to_string_lossy()),
        other => panic!("expected CsprojNotFound, got {other:?}"),
    }

    handle.shutdown().expect("shutdown clean");
}

/// The sidecar's `EmitOptions` set `tolerateErrors: true` so a C# project
/// with method-body errors but an intact public surface still produces a
/// metadata DLL. The F# binder only consumes the public surface; without
/// this flag, a stray body-level typo elsewhere in a user's C# project
/// would silently strip the metadata DLL we hand the LSP. Verifies the
/// contract end-to-end: emit succeeds, the DLL parses, exposes the public
/// type, **and** the response carries the body-level CS0103 in its
/// `diagnostics` array.
///
/// The diagnostic surfacing is load-bearing for D8: `tolerateErrors`
/// silences body errors inside `EmitResult.Diagnostics`, so a naive
/// implementation would lose them entirely. Phase 7 fixes this by
/// unioning the emit diagnostics with `Compilation.GetDiagnostics()`,
/// which runs the full compile pass without the emit-side filter.
///
/// Uses a temp fixture under `target/` with a per-invocation nonce so
/// the cache always misses. Diagnostics on a D6 cache hit are an empty
/// array by design (the diagnostics belong to the call that re-derived
/// them), so a cached body-error fixture would silently pass the
/// diagnostic assertions even if the union were broken.
#[test]
fn build_metadata_tolerates_body_level_errors() {
    let dotnet = find_dotnet();

    let fixture = workspace_root().join("target/csharp-sidecar-body-error-fixture");
    let _ = std::fs::remove_dir_all(&fixture);
    std::fs::create_dir_all(&fixture).expect("create temp fixture root");

    std::fs::write(
        fixture.join("BodyError.csproj"),
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>BodyErrorFixture</RootNamespace>\n",
            "    <AssemblyName>BodyErrorFixture</AssemblyName>\n",
            "    <Nullable>enable</Nullable>\n",
            "  </PropertyGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write csproj");

    // Body-level CS0103 (undefined `missingValue`) with a per-invocation nonce
    // so this test's cache entry is unique per `cargo test` run. The published
    // DLL lives under `<workspace>/obj/borzoi/csharp-sidecar/...` and
    // persists across runs; without the nonce, a second invocation would hit
    // the cache, get an empty diagnostics array, and the CS0103 assertion
    // would fire even though the production code is correct.
    let nonce = unique_nonce();
    let cs_path = fixture.join("BodyError.cs");
    std::fs::write(
        &cs_path,
        format!(
            "// nonce {nonce}\nnamespace BodyErrorFixture;\n\npublic sealed class BodyError\n{{\n    public int Compute() => missingValue;\n}}\n",
        ),
    )
    .expect("write cs with body-level error");

    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let mut handle = build_and_start(&dotnet);
    let csproj = fixture.join("BodyError.csproj");
    let result = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms_for(&csproj))
        .expect("metadata-only emit should succeed despite body-level errors");

    assert!(
        !result.from_cache,
        "first emit on a nonce-mutated fixture must miss the cache so diagnostics surface",
    );
    assert!(
        result.metadata_dll_path.exists(),
        "expected metadata DLL at {}",
        result.metadata_dll_path.display(),
    );

    let bytes = std::fs::read(&result.metadata_dll_path).expect("read DLL");
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
        fqns.iter().any(|f| f == "BodyErrorFixture.BodyError"),
        "expected BodyErrorFixture.BodyError despite body-level error, got {fqns:?}",
    );

    // D8: the body-level CS0103 must surface in `diagnostics`. Find the
    // single CS0103 (there should be exactly one; if Roslyn ever starts
    // double-reporting, the assertion below would catch it).
    let cs0103: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.id == "CS0103")
        .collect();
    assert_eq!(
        cs0103.len(),
        1,
        "expected exactly one CS0103 diagnostic, got {} from diagnostics={:?}",
        cs0103.len(),
        result.diagnostics,
    );
    let diag = cs0103[0];
    assert_eq!(diag.severity, "Error", "CS0103 must be Error severity");
    assert!(
        diag.message.contains("missingValue"),
        "CS0103 message should mention the undefined identifier, got {:?}",
        diag.message,
    );
    let file_path = diag
        .file_path
        .as_ref()
        .expect("CS0103 must carry a source file path");
    assert!(
        file_path.ends_with("BodyError.cs"),
        "expected CS0103 file_path to point at BodyError.cs, got {file_path:?}",
    );
    let range = diag.range.expect("CS0103 must carry a source range");
    // The .cs file's `Compute()` body is on line 5 (0-based: 4) of the
    // fixture we just wrote: `    public int Compute() => missingValue;`.
    // We don't pin the exact line/column because the file we write here may
    // grow a header; we do require start==end line (CS0103 is for a single
    // identifier) and a non-empty character span.
    assert_eq!(
        range.start.line, range.end.line,
        "CS0103 should be on a single line, got {range:?}",
    );
    assert!(
        range.start.character < range.end.character,
        "CS0103 range should span at least one character, got {range:?}",
    );

    handle.shutdown().expect("shutdown clean");
}

/// D8 mandates "never fall back to a stale cached DLL." Phase 4's
/// content-addressed cache makes that property structural: the cache key
/// is a hash of every input, so a source mutation derives a new key, and
/// the previous DLL (at the old key) is never returned as the answer for
/// the new state. If the new emit fails, the sidecar returns `BuildFailed`
/// with no `metadataDllPath` in the payload — the previous DLL stays on
/// disk (correctly, since it is still the right answer for the previous
/// inputs were they ever to recur), but is no longer reachable through
/// any successful response for the current state.
///
/// The test writes a temp fixture under `target/` so it can mutate the
/// source between sidecar calls without polluting the repo. First emit
/// is on valid source (expect Built, DLL exists). We then overwrite the
/// .cs with duplicate type declarations (CS0101), which Roslyn refuses
/// to emit even with `tolerateErrors: true`. Second emit must return
/// `BuildFailed`.
#[test]
fn failed_re_emit_returns_build_failed_without_dll_path() {
    let dotnet = find_dotnet();
    // Live under target/ so cargo's gitignore covers any leftover, and
    // each run starts from a clean slate.
    let fixture = workspace_root().join("target/csharp-sidecar-stale-test-fixture");
    let _ = std::fs::remove_dir_all(&fixture);
    std::fs::create_dir_all(&fixture).expect("create temp fixture root");

    std::fs::write(
        fixture.join("Stale.csproj"),
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>StaleFixture</RootNamespace>\n",
            "    <AssemblyName>StaleFixture</AssemblyName>\n",
            "    <Nullable>enable</Nullable>\n",
            "  </PropertyGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write csproj");

    let cs_path = fixture.join("Stale.cs");
    std::fs::write(
        &cs_path,
        "namespace StaleFixture;\n\npublic sealed class Stale { }\n",
    )
    .expect("write valid cs");

    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let mut handle = build_and_start(&dotnet);
    let csproj = fixture.join("Stale.csproj");

    // First emit on the valid source: must succeed and produce a DLL.
    let first = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms_for(&csproj))
        .expect("first emit on valid source should succeed");
    assert!(
        first.metadata_dll_path.exists(),
        "first emit should produce a DLL at {}",
        first.metadata_dll_path.display(),
    );

    // Break the source with a CS0101 (duplicate definition). Even with
    // `tolerateErrors: true`, Roslyn refuses to emit two metadata
    // entries for the same fully-qualified type name.
    std::fs::write(
        &cs_path,
        concat!(
            "namespace StaleFixture;\n\n",
            "public sealed class Stale { }\n",
            "public sealed class Stale { }\n",
        ),
    )
    .expect("write broken cs");

    // Reuse the closure map computed from the valid csproj; we corrupted
    // the source file, not the assets manifest, so the closure is still
    // accurate from the sidecar's perspective.
    let err = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms_for(&csproj))
        .expect_err("second emit on broken source should fail");
    let diagnostics = match &err {
        SidecarError::Sidecar {
            kind: SidecarErrorKind::BuildFailed { diagnostics, .. },
            ..
        } => diagnostics.clone(),
        other => panic!("expected BuildFailed, got {other:?}"),
    };

    // D8: the BuildFailed payload must carry the compiler diagnostics so the
    // F# LSP can render "this C# project failed to bind because…". Roslyn
    // reports a duplicate type as CS0101 ("The namespace 'StaleFixture'
    // already contains a definition for 'Stale'"). We assert id, severity,
    // and that the source location points at the second `Stale` declaration
    // we just wrote.
    let cs0101: Vec<_> = diagnostics.iter().filter(|d| d.id == "CS0101").collect();
    assert!(
        !cs0101.is_empty(),
        "BuildFailed must carry the CS0101 diagnostic, got diagnostics={diagnostics:?}",
    );
    for diag in &cs0101 {
        assert_eq!(
            diag.severity, "Error",
            "CS0101 must be Error severity, got {:?}",
            diag.severity,
        );
        let file_path = diag
            .file_path
            .as_ref()
            .expect("CS0101 must carry a source file path");
        assert!(
            file_path.ends_with("Stale.cs"),
            "CS0101 file_path should point at Stale.cs, got {file_path:?}",
        );
        diag.range.expect("CS0101 must carry a source range");
    }

    // Sanity check: the `SidecarError::Sidecar` variant has no DLL-path slot,
    // which is the D8 wire-level guarantee. The previously-published DLL at
    // the *first*'s key intentionally stays on disk — it is still the right
    // answer for those source bytes — and a caller is expected to ignore it
    // because the second call's BuildFailed response carries no path.
    assert!(
        first.metadata_dll_path.exists(),
        "first DLL should still be on disk after a different-key failure; \
         content-addressing means the failed second key never collided with \
         the first key's path",
    );

    handle.shutdown().expect("shutdown clean");
}

/// Sister regression to [`failed_re_emit_returns_build_failed_without_dll_path`]:
/// `LoadFailed` (upstream of emit) must also surface without a `metadataDllPath`.
/// Emit once on a valid csproj, then corrupt the csproj XML so
/// `OpenProjectAsync` throws and the sidecar returns `LoadFailed`. As with the
/// emit-failure case under content-addressing, the earlier DLL stays on disk
/// (it is still correct for its inputs), but the second call's typed error
/// surfaces no path, so a well-behaved caller does not "fall back" to stale
/// metadata.
#[test]
fn failed_re_load_returns_load_failed_without_dll_path() {
    let dotnet = find_dotnet();
    let fixture = workspace_root().join("target/csharp-sidecar-load-fail-fixture");
    let _ = std::fs::remove_dir_all(&fixture);
    std::fs::create_dir_all(&fixture).expect("create temp fixture root");

    let csproj = fixture.join("LoadFail.csproj");
    std::fs::write(
        &csproj,
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>net10.0</TargetFramework>\n",
            "    <RootNamespace>LoadFailFixture</RootNamespace>\n",
            "    <AssemblyName>LoadFailFixture</AssemblyName>\n",
            "  </PropertyGroup>\n",
            "</Project>\n",
        ),
    )
    .expect("write csproj");

    std::fs::write(
        fixture.join("LoadFail.cs"),
        "namespace LoadFailFixture;\n\npublic sealed class LoadFail { }\n",
    )
    .expect("write valid cs");

    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let mut handle = build_and_start(&dotnet);

    // Resolve the closure-wide TFM map *before* we corrupt the csproj on
    // disk: the resolver reads only `obj/project.assets.json`, but a
    // future change might also peek at the csproj XML, and we want both
    // calls to send the same closure to the sidecar so the LoadFailed
    // surface is what's being tested.
    let project_tfms = project_tfms_for(&csproj);
    let first = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms)
        .expect("first emit on valid csproj should succeed");
    assert!(
        first.metadata_dll_path.exists(),
        "first emit should produce a DLL at {}",
        first.metadata_dll_path.display(),
    );

    // Corrupt the csproj XML so MSBuild's `OpenProjectAsync` throws.
    std::fs::write(&csproj, "<Project this is not valid xml").expect("corrupt csproj");

    let err = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms)
        .expect_err("second emit on malformed csproj should fail");
    match &err {
        SidecarError::Sidecar {
            kind: SidecarErrorKind::LoadFailed { .. },
            ..
        } => {}
        other => panic!("expected LoadFailed, got {other:?}"),
    }

    // As with the emit-failure case: the first DLL is still correct for its
    // inputs and stays on disk; the LoadFailed wire payload carries no path.
    assert!(
        first.metadata_dll_path.exists(),
        "first DLL should still exist after a LoadFailed on different csproj bytes"
    );

    handle.shutdown().expect("shutdown clean");
}

/// D5 hard-error wire trip: a `buildMetadata` request whose `projectTfms`
/// map is missing the top csproj surfaces `SidecarErrorKind::MissingProjectTfm`,
/// not a silent fallback. The xUnit suite pins the C# side of this policy;
/// this test pins the JSON-RPC wire shape so the Rust client correctly
/// decodes the error variant.
#[test]
fn build_metadata_missing_project_tfm_for_top_returns_hard_error() {
    let dotnet = find_dotnet();

    let fixture = workspace_root().join("tools/csharp-sidecar/test-fixtures/empty");
    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let mut handle = build_and_start(&dotnet);
    let csproj = fixture.join("Empty.csproj");
    let err = handle
        .build_metadata(
            &csproj,
            "Debug",
            "net10.0",
            // Deliberately empty — the top is not registered.
            &BTreeMap::new(),
        )
        .expect_err("missing top in projectTfms must hard-error");

    match &err {
        SidecarError::Sidecar {
            kind: SidecarErrorKind::MissingProjectTfm { csproj_path },
            ..
        } => {
            // The canonical csproj path the sidecar reports must match the
            // input modulo canonicalisation (so the LSP can attach a useful
            // diagnostic). Compare via std::fs::canonicalize on both sides
            // to defend against tmpfs symlink resolution differences.
            let expected = csproj
                .canonicalize()
                .unwrap_or_else(|e| panic!("canonicalize {}: {e}", csproj.display()));
            let reported = std::path::PathBuf::from(csproj_path);
            let reported_canon = reported
                .canonicalize()
                .unwrap_or_else(|e| panic!("canonicalize reported {}: {e}", reported.display()));
            assert_eq!(reported_canon, expected);
        }
        other => panic!("expected MissingProjectTfm, got {other:?}"),
    }

    handle.shutdown().expect("shutdown clean");
}

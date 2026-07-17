//! Differential checks against `dotnet` reference assemblies.
//!
//! Split out of the former single-file `csharp_sidecar.rs`.

use super::support::*;

/// D12 differential: the sidecar's metadata-only emit and `dotnet build
/// -p:ProduceReferenceAssembly=true` must agree on the user-visible type
/// surface of the `empty` fixture. The `empty` fixture has a single public
/// class with only public members, so the metadata-only output (which
/// retains private/internal members for IVT, per D5) and the strict ref
/// assembly (which strips them) project to the same set of phase-2 type
/// skeletons: namespace, name, kind, access, base, interfaces, nesting.
///
/// Phase 2's [`Ecma335Assembly`] leaves `members` empty, so this diff
/// pinpoints type-level disagreement only. Phase 3 of the assembly reader
/// will tighten this when members come online.
#[test]
fn build_metadata_matches_dotnet_ref_assembly_empty() {
    let dotnet = find_dotnet();
    diff_sidecar_against_ref_assembly(
        &dotnet,
        "empty",
        "Empty.csproj",
        "EmptyFixture",
        "EmptyFixture",
    );
}

/// Same as the `empty` differential, but for the `pkg-ref` fixture: a
/// public class that mentions a Newtonsoft.Json type in its public
/// surface. Exercises the same property in the presence of a
/// `PackageReference`-resolved compile-time DLL.
#[test]
fn build_metadata_matches_dotnet_ref_assembly_pkg_ref() {
    let dotnet = find_dotnet();
    diff_sidecar_against_ref_assembly(
        &dotnet,
        "pkg-ref",
        "PkgRef.csproj",
        "PkgRefFixture",
        "PkgRefFixture",
    );
}

/// `<Nullable>enable</Nullable>` differential: the `nullable` fixture's
/// `Holder` class mixes nullable-annotated members (`string?`) with
/// non-nullable ones. Roslyn's metadata-only emit and
/// `dotnet build -p:ProduceReferenceAssembly=true` must agree on which
/// members survive, on their (erased) signatures, AND on the
/// per-position nullability that the assembly reader projects (4m.1 for
/// typars, 4m.2 for parameters/fields/properties/return positions). The
/// normaliser appends `!` / `?` suffixes for `NotAnnotated` / `Annotated`
/// positions, so a regression where the SDK's nullable rewriter assigned
/// a different byte in one of the two emit modes would surface here as a
/// diff. The fixture deliberately sticks to scalar `string?` shapes —
/// composite forms (`string[]?`, `List<string?>`) would trigger the
/// byte[]-form refusal the assembly reader keeps in place until phase
/// 4m.3 lands.
#[test]
fn build_metadata_matches_dotnet_ref_assembly_nullable() {
    let dotnet = find_dotnet();
    diff_sidecar_against_ref_assembly(
        &dotnet,
        "nullable",
        "Nullable.csproj",
        "NullableFixture",
        "NullableFixture",
    );
}

/// `[InternalsVisibleTo]` is the only thing standing between an F# IVT
/// consumer and ghost-binding to a stripped reference assembly. The
/// sidecar pins `EmitOptions.IncludePrivateMembers = true` (D5) so the
/// internal type survives the metadata-only emit; this test asserts that
/// directly by loading the sidecar's DLL and looking for the internal
/// type. This is *not* a differential test against
/// `ProduceReferenceAssembly=true` because that mode strips internals by
/// construction, so the two pipelines necessarily disagree on the IVT
/// fixture — that disagreement is exactly the property the sidecar is
/// supposed to defend against (gospel P5: wrong metadata is worse than
/// no metadata; here, "wrong" means "missing internals an IVT consumer
/// needs").
///
/// Test shape:
///   1. emit via sidecar
///   2. load DLL through [`Ecma335Assembly`]
///   3. assert `IvtFixture.InternalGreeter` is present with
///      [`Access::Internal`]
///   4. assert `IvtFixture.PublicGreeter` is present with
///      [`Access::Public`] — guards against a regression where the
///      sidecar mistakenly classifies every type as `Public`, which
///      would trivially pass the first assertion.
#[test]
fn build_metadata_ivt_internal_survives_metadata_only_emit() {
    let dotnet = find_dotnet();

    let fixture = workspace_root().join("tools/csharp-sidecar/test-fixtures/ivt");
    let _fixture_lock = lock_fixture(&dotnet, &fixture);

    let mut handle = build_and_start(&dotnet);
    let csproj = fixture.join("Ivt.csproj");
    let result = handle
        .build_metadata(&csproj, "Debug", "net10.0", &project_tfms_for(&csproj))
        .expect("sidecar buildMetadata succeeds on the IVT fixture");
    handle.shutdown().expect("shutdown clean");

    let bytes = std::fs::read(&result.metadata_dll_path).expect("read sidecar DLL");
    let view = Ecma335Assembly::parse(&bytes).expect("parse sidecar DLL");
    let entities = view.enumerate_type_defs().expect("enumerate type defs");

    let find = |fqn: &str| -> &Entity {
        entities
            .iter()
            .find(|e| {
                let entity_fqn = if e.namespace.is_empty() {
                    e.name.clone()
                } else {
                    format!("{}.{}", e.namespace.join("."), e.name)
                };
                entity_fqn == fqn
            })
            .unwrap_or_else(|| {
                let all: Vec<String> = entities
                    .iter()
                    .map(|e| {
                        if e.namespace.is_empty() {
                            e.name.clone()
                        } else {
                            format!("{}.{}", e.namespace.join("."), e.name)
                        }
                    })
                    .collect();
                panic!("expected {fqn} in sidecar DLL; got: {all:?}")
            })
    };

    let internal_greeter = find("IvtFixture.InternalGreeter");
    assert_eq!(
        internal_greeter.access,
        Access::Internal,
        "InternalGreeter should be Access::Internal in the sidecar DLL; \
         got {:?}",
        internal_greeter.access,
    );

    let public_greeter = find("IvtFixture.PublicGreeter");
    assert_eq!(
        public_greeter.access,
        Access::Public,
        "PublicGreeter should be Access::Public in the sidecar DLL; \
         got {:?}",
        public_greeter.access,
    );
}

/// Source-generator differential. The `source-gen` fixture declares a
/// `[GeneratedRegex]` partial method so the in-box regex source generator
/// completes the declaration with a real body and an internal nested
/// helper class. The SG ships with the .NET shared framework (no
/// `<PackageReference>` needed), and Roslyn runs it during
/// `Project.GetCompilationAsync()` so the SG-produced compilation units
/// land in `Compilation.SyntaxTrees` before `Emit`.
///
/// What this test pins: the sidecar's metadata-only emit and
/// `dotnet build -p:ProduceReferenceAssembly=true` see the *same*
/// generated public surface, *including* the SG-emitted helper class.
/// A regression where the sidecar's compilation pipeline bypasses
/// generator finalisation (so the helper class vanishes on one side
/// only) is exactly what would fall out of the diff — gospel P4,
/// "leverage compute": the equality check itself is the oracle, no
/// by-hand enumeration of what the SG should produce.
///
/// We pick `GeneratedRegex` over `System.Text.Json`'s richer SG
/// deliberately: the JSON SG synthesises generic methods
/// (`TryGetTypeInfoForRuntimeCustomConverter<T>`) the phase-3a assembly
/// reader doesn't yet project, which would fail the test for an
/// unrelated reason. Once method generics land in the reader we can
/// grow this fixture.
#[test]
fn build_metadata_matches_dotnet_ref_assembly_source_gen() {
    let dotnet = find_dotnet();
    diff_sidecar_against_ref_assembly(
        &dotnet,
        "source-gen",
        "SourceGen.csproj",
        "SourceGenFixture",
        "SourceGenFixture",
    );
}

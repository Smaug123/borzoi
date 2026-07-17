//! Differential test: compare `src/fsproj`'s extracted Compile lists
//! against `dotnet msbuild -getItem:…` for the same fsproj.
//!
//! MSBuild is the reference implementation. Most fixture tests compare the
//! raw static `Compile` / `CompileBefore` / `CompileAfter` item lists; the
//! focused `CompileOrder` test invokes the F# SDK's
//! `FSharpSourceCodeCompileOrder` target and compares the final effective
//! `Compile` order.
//!
//! ## How the test invokes MSBuild
//!
//! `dotnet msbuild -getItem:Compile,CompileBefore,CompileAfter <fsproj>`
//! prints a JSON document with one array per item type; `dotnet msbuild
//! -target:FSharpSourceCodeCompileOrder -getItem:Compile <fsproj>` prints the
//! final F# source order. We deliberately run `dotnet` from this crate's
//! `CARGO_MANIFEST_DIR` (the repo root) rather than from the corpus root.
//! `global.json` discovery walks upward from the *current working directory*,
//! and the corpus's `global.json` pins an SDK version that may not be installed
//! — running from the repo root means MSBuild picks up whichever SDK the
//! devShell provides.
//!
//! We also pass `-p:DISABLE_ARCADE=true`. The F# repo's
//! `Directory.Build.props` chain imports `Microsoft.DotNet.Arcade.Sdk`
//! by default, and that SDK is pinned in the same `global.json` we're
//! skipping. Without disabling Arcade, MSBuild's SDK resolver can hang
//! or fail on a clean host. The Arcade machinery is irrelevant to a
//! `-getItem:` evaluation, so turning it off here is purely defensive.
//!
//! MSBuild treats every inherited environment variable as an initial
//! property, so a developer running with e.g. `BUILDING_USING_DOTNET=1`
//! exported would silently get a different set of `Compile` items than
//! a clean CI run (the FSharp.Core project gates its prelude routing
//! on that property). Our parser only sees the explicit `extras` list,
//! so the two sides would be evaluating different configurations.
//! [`run_msbuild`] strips the environment down to what `dotnet` itself
//! needs to find its runtime (`PATH`, `HOME`, `TMPDIR`, `DOTNET_*`,
//! `NUGET_*`), making the oracle hermetic with respect to the shell.
//!
//! ## Path comparison
//!
//! Our parser produces `project_dir.join(include)` lexically, so a
//! `<Compile Include="..\Compiler\Utilities\sformat.fsi" />` resolves to
//! `<FSharp.Core>/../Compiler/Utilities/sformat.fsi` with the `..` intact.
//! MSBuild's `FullPath` already lexically resolves the `..` segments and
//! returns `<Compiler>/Utilities/sformat.fsi`. We canonicalise both with
//! [`std::fs::canonicalize`] for the comparison — most fsproj we exercise
//! have the referenced files on disk under the corpus root, so canonicalisation
//! succeeds on both sides and yields identical paths for semantically
//! equivalent includes.
//!
//! ## Canonicalize-failure excuse
//!
//! Some `<Compile>` items reference generated sources via property
//! chains we don't fully evaluate. `FSharp.Compiler.Service.fsproj`
//! computes
//! `<FsYaccOutputFolder>$(IntermediateOutputPath)$(TargetFramework)\</FsYaccOutputFolder>`
//! where neither right-hand-side property is set in the project file
//! (both come from MSBuild's `Microsoft.Common.props` SDK targets or
//! the multi-targeting outer/inner build dance). Our parser substitutes
//! them with the empty string and records
//! [`DiagnosticKind::UndefinedProperty`] at the property-definition's
//! span, then `<Compile Include="$(FsYaccOutputFolder)ilpars.fsi" />`
//! resolves to a path that doesn't exist on disk. MSBuild, having
//! actually expanded the SDK properties, fills in the real
//! `artifacts/obj/…/<framework>/ilpars.fsi` — which also doesn't exist
//! pre-build, since the fslex/fsyacc targets haven't run.
//!
//! Rather than carve out a per-item allowlist, this test treats
//! [`std::fs::canonicalize`] failure on **both** sides as an excuse —
//! *but only if* the parser already self-reported at least one
//! substitution-related diagnostic
//! ([`UndefinedProperty`](DiagnosticKind::UndefinedProperty) or
//! [`UnsupportedPropertyExpression`](DiagnosticKind::UnsupportedPropertyExpression)).
//! A one-sided canonicalize failure is still a hard failure: it means
//! one of the two implementations resolved to a path the other didn't,
//! which is the kind of disagreement we're trying to catch (e.g. a
//! regression that left Windows backslashes in normal Compile includes
//! on Unix). Requiring two-sided failure keeps the oracle strict for
//! every item except the handful where neither implementation can
//! produce a real path, which today is just FCS's
//! `$(FsYaccOutputFolder)…` / `$(FsLexOutputFolder)…` generated sources
//! (12 of 381 Compile items).
//!
//! ## `Link` separator normalisation
//!
//! Our parser preserves `<Link>` metadata verbatim from the project
//! source, where authors usually write Windows-style backslashes
//! (`Driver\AssemblyResolveHandler.fs`). MSBuild's `-getItem:` JSON
//! output normalises these to forward slashes. Since `<Link>` is a
//! logical *path* — the choice of separator carries no semantic
//! information — we normalise both sides to forward slashes for the
//! comparison.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use borzoi_msbuild::{
    Diagnostic, DiagnosticKind, ItemKind, ResolvedItem, parse_fsproj_with_imports,
};
use borzoi_oracle_harness::BoundedCommand;
use serde::Deserialize;
use tempfile::TempDir;

#[test]
fn assembly_check() {
    run_diff("buildtools/AssemblyCheck/AssemblyCheck.fsproj", &[]);
}

#[test]
fn fslex() {
    run_diff("buildtools/fslex/fslex.fsproj", &[]);
}

#[test]
fn fsharp_core_proto() {
    run_diff(
        "src/FSharp.Core/FSharp.Core.fsproj",
        &[("Configuration", "Proto")],
    );
}

#[test]
fn fsharp_core_release() {
    run_diff(
        "src/FSharp.Core/FSharp.Core.fsproj",
        &[("Configuration", "Release")],
    );
}

#[test]
fn fsharp_compiler_service_release() {
    run_diff(
        "src/Compiler/FSharp.Compiler.Service.fsproj",
        &[("Configuration", "Release")],
    );
}

#[test]
fn compile_order_metadata_matches_fsharp_source_code_compile_order() {
    let tmp = TempDir::new().unwrap();
    let fsproj = tmp.path().join("Order.fsproj");
    let source = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Ord.fs" />
    <Compile Include="First.fs" CompileOrder="CompileFirst" />
    <CompileBefore Include="ExplicitBefore.fs" />
    <Compile Include="Before.fs" CompileOrder="CompileBefore" />
    <Compile Include="After.fs">
      <CompileOrder>CompileAfter</CompileOrder>
    </Compile>
    <CompileAfter Include="ExplicitAfter.fs" />
    <Compile Include="Last.fs" CompileOrder="CompileLast" />
  </ItemGroup>
</Project>"#;
    std::fs::write(&fsproj, source).unwrap();
    for file in [
        "Ord.fs",
        "First.fs",
        "ExplicitBefore.fs",
        "Before.fs",
        "After.fs",
        "ExplicitAfter.fs",
        "Last.fs",
    ] {
        std::fs::write(tmp.path().join(file), "module M\n").unwrap();
    }

    let project = parse_fsproj_with_imports(
        source,
        &fsproj,
        &HashMap::new(),
        &common::oracle_environment(),
        None,
        None,
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", fsproj.display()));
    let ours = item_file_names(project.items.iter());
    let theirs = run_msbuild_fsharp_compile_order(&fsproj);
    let expected = vec![
        "First.fs",
        "ExplicitBefore.fs",
        "Before.fs",
        "Ord.fs",
        "After.fs",
        "ExplicitAfter.fs",
        "Last.fs",
    ];
    assert_eq!(theirs, expected, "MSBuild oracle changed");
    assert_eq!(ours, theirs);
}

/// The sixth finding of `docs/msbuild-escaped-value-plan.md`, guarded where it
/// actually bites.
///
/// MSBuild escapes **nine** characters when it seeds a reserved path property
/// (`EscapingUtilities.cs:310`, applied at `Evaluator.cs:1186`), so a `;` in the
/// project's own directory reaches an item spec as `%3b` and cannot split the
/// list: this project yields **one** Compile item whose identity carries a
/// literal semicolon. Carrying provenance for `%` alone — as the
/// `literal_percents` side-channel did — split it into two, a certain and
/// undiagnosed wrong answer.
///
/// The property-table differential cannot catch this: `-getProperty` unescapes
/// at the read, so both models agree on the *value*. The escape is only
/// observable where the value is **scanned**, which is here.
#[test]
fn a_reserved_character_in_the_project_directory_does_not_split_items() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("semi;colon");
    std::fs::create_dir_all(&dir).unwrap();
    let fsproj = dir.join("Demo.fsproj");
    let source = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="$(MSBuildProjectDirectory)/x.fs" />
  </ItemGroup>
</Project>"#;
    std::fs::write(&fsproj, source).unwrap();
    std::fs::write(dir.join("x.fs"), "module M\n").unwrap();

    let theirs = run_msbuild(&fsproj, &[]);
    let theirs: Vec<&str> = theirs
        .items_for(ItemKind::Compile)
        .iter()
        .map(|i| i.full_path.as_str())
        .collect();
    assert_eq!(
        theirs.len(),
        1,
        "MSBuild oracle changed: the seed's `;` must not split the item list"
    );
    assert!(theirs[0].contains("semi;colon/x.fs"), "{theirs:?}");

    let project = parse_fsproj_with_imports(
        source,
        &fsproj,
        &HashMap::new(),
        &common::oracle_environment(),
        None,
        None,
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", fsproj.display()));
    let ours: Vec<String> = project
        .items
        .iter()
        .map(|item| item.include.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        ours.len(),
        1,
        "we must not split on the seed's `;`: {ours:?}"
    );
    assert!(ours[0].ends_with("semi;colon/x.fs"), "{ours:?}");
    // …and it must not have got there by degrading the expansion. (The parse
    // does raise the unresolved-`Sdk` diagnostic every fixture here raises — no
    // SDK resolver is wired into this call — so the assertion is scoped to the
    // class that would mean the escape machinery had withdrawn the claim.)
    assert!(
        !project.diagnostics.iter().any(|d| matches!(
            &d.kind,
            DiagnosticKind::UnsupportedPropertyExpression { .. }
        )),
        "the value must be modelled, not declined: {:?}",
        project.diagnostics
    );
}

#[test]
fn compile_update_compile_order_matches_fsharp_source_code_compile_order() {
    let tmp = TempDir::new().unwrap();
    let fsproj = tmp.path().join("UpdateOrder.fsproj");
    let source = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="A.fs" />
    <Compile Include="B.fs" />
    <Compile Update="B.fs" CompileOrder="CompileFirst" />
  </ItemGroup>
</Project>"#;
    std::fs::write(&fsproj, source).unwrap();
    for file in ["A.fs", "B.fs"] {
        std::fs::write(tmp.path().join(file), "module M\n").unwrap();
    }

    let project = parse_fsproj_with_imports(
        source,
        &fsproj,
        &HashMap::new(),
        &common::oracle_environment(),
        None,
        None,
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", fsproj.display()));
    let ours = item_file_names(project.items.iter());
    let theirs = run_msbuild_fsharp_compile_order(&fsproj);
    let expected = vec!["B.fs", "A.fs"];
    assert_eq!(theirs, expected, "MSBuild oracle changed");
    assert_eq!(ours, theirs);
}

/// The fsproj-3.3c chosen-TFM oracle (`docs/fsproj-tfm-selection-plan.md`,
/// stage 3.3c-1): evaluating with `TargetFramework` seeded as a global — what
/// the LSP's second pass does for a `<TargetFrameworks>` (plural) project —
/// must match `dotnet msbuild -p:TargetFramework=<first>` exactly, Compile
/// items and DefineConstants, gated branches included.
#[test]
fn multi_tfm_seeded_target_framework_matches_msbuild() {
    let tmp = TempDir::new().unwrap();
    let fsproj = tmp.path().join("Multi.fsproj");
    let source = r#"<Project>
  <PropertyGroup>
    <TargetFrameworks>net8.0;net10.0</TargetFrameworks>
  </PropertyGroup>
  <PropertyGroup Condition="'$(TargetFramework)' == 'net8.0'">
    <DefineConstants>EIGHT</DefineConstants>
  </PropertyGroup>
  <PropertyGroup Condition="'$(TargetFramework)' == 'net10.0'">
    <DefineConstants>TEN</DefineConstants>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Shared.fs" />
  </ItemGroup>
  <ItemGroup Condition="'$(TargetFramework)' == 'net8.0'">
    <Compile Include="Eight.fs" />
  </ItemGroup>
  <ItemGroup Condition="'$(TargetFramework)' == 'net10.0'">
    <Compile Include="Ten.fs" />
  </ItemGroup>
</Project>"#;
    std::fs::write(&fsproj, source).unwrap();
    for file in ["Shared.fs", "Eight.fs", "Ten.fs"] {
        std::fs::write(tmp.path().join(file), "module M\n").unwrap();
    }

    let extras = [("TargetFramework", "net8.0")];
    let mut props: HashMap<String, String> = HashMap::new();
    for (k, v) in extras {
        props.insert(k.into(), v.into());
    }
    let project = parse_fsproj_with_imports(
        source,
        &fsproj,
        &props,
        &common::oracle_environment(),
        None,
        None,
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", fsproj.display()));
    let msbuild = run_msbuild(&fsproj, &extras);

    for kind in [
        ItemKind::CompileBefore,
        ItemKind::Compile,
        ItemKind::CompileAfter,
    ] {
        let ours: Vec<&ResolvedItem> = project.items.iter().filter(|i| i.kind == kind).collect();
        compare_kind(
            "Multi.fsproj (TargetFramework=net8.0)",
            kind,
            &ours,
            msbuild.items_for(kind),
            &project.diagnostics,
        );
    }
    compare_define_constants(
        "Multi.fsproj (TargetFramework=net8.0)",
        &project.define_constants,
        &msbuild.properties.define_constants,
        &project.diagnostics,
    );

    // Belt and braces: the *gated* branch really fired on both sides (the
    // comparisons above would also pass if both sides dropped it).
    assert_eq!(project.define_constants, vec!["EIGHT".to_string()]);
    assert_eq!(
        item_file_names(project.items.iter()),
        vec!["Shared.fs".to_string(), "Eight.fs".to_string()]
    );
}

fn run_diff(rel_fsproj: &str, extras: &[(&str, &str)]) {
    let corpus = common::corpus_root();
    let joined = corpus.join(rel_fsproj);
    assert!(joined.is_file(), "missing fixture {}", joined.display());
    // `parse_fsproj` rejects non-rooted paths (see
    // `ParseError::RelativeProjectPath`), and `BORZOI_CORPUS` is
    // documented to accept relative paths — so absolutise via the
    // filesystem before handing it over. `canonicalize` also resolves
    // symlinks, which is fine here: MSBuild's `FullPath` does the same,
    // so the two sides stay comparable.
    let fsproj = std::fs::canonicalize(&joined)
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", joined.display()));

    let source = std::fs::read_to_string(&fsproj)
        .unwrap_or_else(|e| panic!("read {}: {e}", fsproj.display()));
    let mut props: HashMap<String, String> = HashMap::new();
    for (k, v) in extras {
        props.insert((*k).into(), (*v).into());
    }
    // `run_msbuild` always passes `-p:DISABLE_ARCADE=true` (see the
    // big comment there for why); pass it on the parser side too so
    // both sides evaluate the F# repo's `Directory.Build.props`
    // chain on the same branch. Without this the parser would
    // import Arcade conditionals MSBuild doesn't, and Compile lists
    // would diverge for reasons unrelated to the parser itself.
    props.insert("DISABLE_ARCADE".into(), "true".into());
    // Phase 7a: use the with-imports walker so `Directory.Build.props` /
    // `Directory.Build.targets` and explicit `<Import>` elements are
    // actually followed. Without this, fixtures sitting under the F#
    // tree (which has `Directory.Build.*` chains) would silently lose
    // any property the chain defines — `Configuration` defaults,
    // `OutputPath` derivations, etc. — and our list would diverge from
    // MSBuild even when the parser itself is correct.
    let project = parse_fsproj_with_imports(
        &source,
        &fsproj,
        &props,
        &common::oracle_environment(),
        None,
        None,
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", fsproj.display()));

    let msbuild = run_msbuild(&fsproj, extras);

    for kind in [
        ItemKind::CompileBefore,
        ItemKind::Compile,
        ItemKind::CompileAfter,
    ] {
        let ours: Vec<&ResolvedItem> = project.items.iter().filter(|i| i.kind == kind).collect();
        let theirs = msbuild.items_for(kind);
        compare_kind(rel_fsproj, kind, &ours, theirs, &project.diagnostics);
    }

    compare_define_constants(
        rel_fsproj,
        &project.define_constants,
        &msbuild.properties.define_constants,
        &project.diagnostics,
    );
}

fn item_file_names<'a>(items: impl IntoIterator<Item = &'a ResolvedItem>) -> Vec<String> {
    items
        .into_iter()
        .map(|item| file_name(&item.include))
        .collect()
}

fn msbuild_file_names(items: &[MsbuildItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| file_name(Path::new(&item.full_path)))
        .collect()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| panic!("path has no UTF-8 filename: {}", path.display()))
        .to_string()
}

/// Budget for one `dotnet msbuild` evaluation. A cold one restores packages and
/// walks the whole SDK import chain, which is legitimately minutes, so the bound
/// is far above the harness's per-request default: it is there to stop an
/// evaluation that has *stalled* — blocked on a NuGet lock held by a concurrent
/// run in a sibling worktree, say — from hanging the suite forever, not to police
/// a slow one.
const MSBUILD_TIMEOUT: Duration = Duration::from_secs(1800);

fn run_msbuild_fsharp_compile_order(fsproj: &Path) -> Vec<String> {
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    scrub_msbuild_env(&mut cmd);
    cmd.args([
        "msbuild",
        "-nologo",
        "-target:FSharpSourceCodeCompileOrder",
        "-getItem:Compile",
    ]);
    cmd.arg(fsproj);
    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!("dotnet msbuild for {}", fsproj.display()));
    let stdout = String::from_utf8(out.stdout).expect("msbuild stdout is UTF-8");
    let output: MsbuildOutput = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "could not parse msbuild JSON for {}: {e}\n--- stdout ---\n{stdout}",
            fsproj.display()
        )
    });
    msbuild_file_names(&output.items.compile)
}

/// Diff our `define_constants` against MSBuild's evaluated
/// `$(DefineConstants)`. We split MSBuild's raw string the same way the
/// parser does (`;`, trim, drop empties) so the comparison is list-shaped
/// — order and duplicates included.
///
/// If the parser already self-reported a substitution diagnostic (e.g.
/// `UndefinedProperty`) the value of `DefineConstants` may legitimately
/// diverge from MSBuild's because the property body referenced something
/// our evaluator couldn't resolve. In that case we only assert that
/// *ours* is a (multiset) subset of MSBuild's — we may be missing
/// segments that depended on the unresolved property, but we shouldn't
/// be inventing ones.
fn compare_define_constants(
    fixture: &str,
    ours: &[String],
    theirs_raw: &str,
    diagnostics: &[Diagnostic],
) {
    let theirs: Vec<String> = theirs_raw
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();

    if ours == theirs.as_slice() {
        return;
    }

    if has_substitution_diagnostic(diagnostics) {
        // Multiset subset: every occurrence of `o` in `ours` must be
        // matched by a distinct occurrence in `theirs`. Build a mutable
        // copy of `theirs` and strike out matches.
        let mut remaining = theirs.clone();
        let mut extras: Vec<String> = Vec::new();
        for o in ours {
            if let Some(pos) = remaining.iter().position(|t| t == o) {
                remaining.swap_remove(pos);
            } else {
                extras.push(o.clone());
            }
        }
        if extras.is_empty() {
            eprintln!(
                "{fixture}: DefineConstants — ours is a multiset subset of MSBuild's \
                 ({} vs {}); divergence excused by substitution diagnostic",
                ours.len(),
                theirs.len(),
            );
            return;
        }
        panic!(
            "{fixture}: DefineConstants — ours contains segments MSBuild didn't \
             ({extras:?}); ours={ours:?}, theirs={theirs:?}"
        );
    }

    panic!(
        "{fixture}: DefineConstants diverges from MSBuild\n  ours   = {ours:?}\n  theirs = {theirs:?}"
    );
}

fn run_msbuild(fsproj: &Path, extras: &[(&str, &str)]) -> MsbuildOutput {
    let mut cmd = Command::new("dotnet");
    // Run from the repo root so `global.json` discovery doesn't pick up the
    // corpus's `global.json` and reject the host SDK. See module docs.
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    // MSBuild treats every environment variable as an *initial property*,
    // so any inherited variable named the same as something the project
    // file gates on (e.g. `BUILDING_USING_DOTNET`, `FSHARPCORE_USE_PACKAGE`,
    // `OfficialBuildId`, `Configuration`, …) can flip which items the
    // project evaluates to — leaking the developer's shell state into the
    // oracle. Our parser only consumes the explicit `extras` list, so the
    // two sides would compare different configurations.
    //
    // Clear the environment and re-introduce only what `dotnet` itself
    // needs to locate its runtime: PATH (to find dotnet's helper binaries
    // and any process spawned by tasks), HOME (for `~/.nuget/packages`
    // and `~/.dotnet/`), and the optional `DOTNET_*` / `NUGET_*` overrides
    // when the caller has set them.
    scrub_msbuild_env(&mut cmd);
    cmd.args([
        "msbuild",
        "-nologo",
        "-getItem:Compile,CompileBefore,CompileAfter",
        // Phase 7: also retrieve the evaluated `$(DefineConstants)` so
        // the differential test in `compare_define_constants` can diff
        // ours against MSBuild's. Combining `-getItem:` and `-getProperty:`
        // changes the output JSON shape from `{ "Items": {…} }` to
        // `{ "Properties": {…}, "Items": {…} }`; `MsbuildOutput` carries
        // a `#[serde(default)]` `Properties` field so the deserialiser
        // stays happy either way.
        "-getProperty:DefineConstants",
        // The corpus's `FSharpBuild.Directory.Build.props` imports
        // `Microsoft.DotNet.Arcade.Sdk` unless `DISABLE_ARCADE=true`.
        // That SDK is pinned in the corpus's `global.json` which we're
        // deliberately skipping, so on a clean dev/CI host MSBuild has
        // no way to resolve it and the SDK resolver hangs or errors out
        // before producing the JSON we want to parse. The Arcade SDK
        // provides target/task plumbing that's irrelevant to a pure
        // `-getItem:` evaluation — disabling it lets MSBuild walk just
        // the project file + its own imports.
        "-p:DISABLE_ARCADE=true",
    ]);
    for (k, v) in extras {
        cmd.arg(format!("-p:{k}={v}"));
    }
    cmd.arg(fsproj);
    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!("dotnet msbuild for {}", fsproj.display()));
    let stdout = String::from_utf8(out.stdout).expect("msbuild stdout is UTF-8");
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "could not parse msbuild JSON for {}: {e}\n--- stdout ---\n{stdout}",
            fsproj.display()
        )
    })
}

fn scrub_msbuild_env(cmd: &mut Command) {
    cmd.env_clear();
    for var in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(var) {
            cmd.env(var, value);
        }
    }
    for (key, value) in std::env::vars() {
        if key.starts_with("DOTNET_") || key.starts_with("NUGET_") {
            cmd.env(key, value);
        }
    }
}

fn compare_kind(
    fixture: &str,
    kind: ItemKind,
    ours: &[&ResolvedItem],
    theirs: &[MsbuildItem],
    diagnostics: &[Diagnostic],
) {
    let same_len = ours.len() == theirs.len();
    let mut mismatches: Vec<String> = Vec::new();
    let mut excused = 0usize;
    let substitution_partial = has_substitution_diagnostic(diagnostics);

    let limit = ours.len().max(theirs.len());
    for i in 0..limit {
        match (ours.get(i), theirs.get(i)) {
            (Some(o), Some(t)) => {
                let our_canon = std::fs::canonicalize(&o.include);
                let their_canon = std::fs::canonicalize(Path::new(&t.full_path));
                let our_link = normalize_link(o.link.as_deref().unwrap_or(""));
                let their_link = normalize_link(t.link.as_str());
                match (our_canon, their_canon) {
                    (Ok(oc), Ok(tc)) => {
                        if oc != tc || our_link != their_link {
                            mismatches.push(format!(
                                "  [{i}] ours={} link={:?}\n      theirs={} link={:?}",
                                oc.display(),
                                our_link,
                                tc.display(),
                                their_link,
                            ));
                        }
                    }
                    (Err(_), Err(_)) if substitution_partial => {
                        // Neither path exists on disk, and the parser
                        // already self-reported incomplete property
                        // resolution — accept the position match,
                        // don't verify contents.
                        excused += 1;
                    }
                    (oc, tc) => {
                        // A one-sided canonicalize failure means our
                        // resolved path doesn't exist where MSBuild's
                        // does (or vice versa), which is a real
                        // disagreement regardless of any project-wide
                        // substitution diagnostic. Two-sided failure
                        // with no substitution diagnostic is also a
                        // real bug — we're producing a path that
                        // doesn't exist for an item where MSBuild
                        // didn't either.
                        mismatches.push(format!(
                            "  [{i}] canonicalize disagreement\n\
                                   ours={} canonicalize={:?} link={:?}\n\
                                   theirs={} canonicalize={:?} link={:?}",
                            o.include.display(),
                            oc.as_ref().err().map(|e| e.to_string()),
                            our_link,
                            t.full_path,
                            tc.as_ref().err().map(|e| e.to_string()),
                            their_link,
                        ));
                    }
                }
            }
            (Some(o), None) => mismatches.push(format!(
                "  [{i}] ours-only: {} link={:?}",
                o.include.display(),
                o.link.as_deref().unwrap_or(""),
            )),
            (None, Some(t)) => mismatches.push(format!(
                "  [{i}] msbuild-only: {} link={:?}",
                t.full_path, t.link,
            )),
            (None, None) => unreachable!(),
        }
    }

    if !mismatches.is_empty() || !same_len {
        let mut msg = format!(
            "{fixture}: {kind:?} item lists diverge from MSBuild \
             (ours: {}, msbuild: {})\n",
            ours.len(),
            theirs.len()
        );
        for line in &mismatches {
            msg.push_str(line);
            msg.push('\n');
        }
        if !diagnostics.is_empty() {
            msg.push_str("\nour parser emitted these diagnostics:\n");
            for d in diagnostics {
                msg.push_str(&format!("  {:?}\n", d.kind));
            }
        }
        panic!("{msg}");
    }
    if excused > 0 {
        eprintln!(
            "{fixture}: {kind:?} — {excused} item(s) excused by \
             canonicalize-failure + substitution diagnostic; position \
             verified, contents not"
        );
    }
}

/// Normalise a `<Link>` value's path separator for comparison: MSBuild
/// emits forward slashes regardless of how the project authored them,
/// our parser preserves the source verbatim. The Link is semantically
/// a path, so this difference is cosmetic.
fn normalize_link(link: &str) -> String {
    link.replace('\\', "/")
}

fn has_substitution_diagnostic(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| {
        matches!(
            d.kind,
            DiagnosticKind::UndefinedProperty { .. }
                | DiagnosticKind::UnsupportedPropertyExpression { .. }
        )
    })
}

#[derive(Deserialize)]
struct MsbuildOutput {
    #[serde(rename = "Items")]
    items: MsbuildItems,
    #[serde(default, rename = "Properties")]
    properties: MsbuildProperties,
}

#[derive(Deserialize, Default)]
struct MsbuildItems {
    #[serde(default, rename = "Compile")]
    compile: Vec<MsbuildItem>,
    #[serde(default, rename = "CompileBefore")]
    compile_before: Vec<MsbuildItem>,
    #[serde(default, rename = "CompileAfter")]
    compile_after: Vec<MsbuildItem>,
}

#[derive(Deserialize, Default)]
struct MsbuildProperties {
    /// MSBuild emits the raw evaluated string verbatim (e.g.
    /// `"RELEASE;TRACE"`); we split it the same way our parser splits
    /// `define_constants()` so the comparison is list-shaped.
    #[serde(default, rename = "DefineConstants")]
    define_constants: String,
}

#[derive(Deserialize)]
struct MsbuildItem {
    #[serde(rename = "FullPath")]
    full_path: String,
    /// Empty string when the item carried no `<Link>` metadata (MSBuild
    /// emits the key with an empty value rather than omitting it).
    #[serde(default, rename = "Link")]
    link: String,
}

impl MsbuildOutput {
    fn items_for(&self, kind: ItemKind) -> &[MsbuildItem] {
        match kind {
            ItemKind::Compile => &self.items.compile,
            ItemKind::CompileBefore => &self.items.compile_before,
            ItemKind::CompileAfter => &self.items.compile_after,
            // The caller only iterates the Compile trio. ProjectReference
            // doesn't have an oracle slot in `MsbuildOutput`, so reaching
            // this arm would be a programming error.
            ItemKind::ProjectReference => {
                unreachable!("items_for called with ProjectReference — oracle has no slot for it")
            }
        }
    }
}

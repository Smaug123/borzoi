//! The server must reach the *same* certainty on a real `net10.0` SDK project
//! that the msbuild crate's `sdk_style_*` differential fixtures do — but those
//! fixtures seed `MSBuildUserExtensionsPath` from a one-off probe of the real
//! MSBuild, whereas the server has to *derive* it (no subprocess at startup).
//! This pins both halves of that substitution:
//!
//! 1. [`derived_user_extensions_path_matches_msbuild`] — the value the server
//!    derives equals what `dotnet msbuild -getProperty:MSBuildUserExtensionsPath`
//!    reports, under the same process environment.
//! 2. [`plain_net10_reaches_certainty_through_the_server_environment`] — a plain
//!    `<Project Sdk="Microsoft.NET.Sdk">` evaluated through the production
//!    environment (`SdkDiscoveryEnv::from_process_env`) yields a *certain*
//!    PackageReference set. Without the derived seed the SDK-chain walk turns
//!    opaque at `Microsoft.Common.props`'s user-extension import gate and the
//!    set degrades to uncertain — which is exactly what would make the in-house
//!    NuGet resolver decline at step one on every real project.
//!
//! Requires the .NET SDK on PATH — the Nix devShell provides it.
//!
//! Unix-only: on Windows the derivation intentionally declines (`.NET`'s
//! `LocalApplicationData` known-folder API is not `%LOCALAPPDATA%`, see
//! [`borzoi::fsproj_diagnostics`]), so there is no seeded value or
//! certainty to assert. The module is declared unconditionally — the
//! module-tree guard requires that — and gated here.
#![cfg(unix)]

use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use borzoi::fsproj_diagnostics::msbuild_user_extensions_path;
use borzoi::glob_resolver;
use borzoi::sdk_discovery::{SdkDiscovery, SdkDiscoveryEnv};
use borzoi_msbuild::{GlobResolver, SdkResolver, parse_fsproj_with_imports};
use borzoi_spawn::BoundedCommand;

const PLAIN_NET10: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
</Project>
"#;

#[test]
fn derived_user_extensions_path_matches_msbuild() {
    // Derive from the same environment `from_process_env` uses in production
    // (real `$HOME`, real folder-derivation vars).
    let env = SdkDiscoveryEnv::from_process_env();
    let derived =
        msbuild_user_extensions_path(env.home_dir.as_deref(), |name| std::env::var_os(name))
            .expect("derivation must produce a value under `nix develop`");

    // Ground truth: ask the real MSBuild, inheriting the same process
    // environment the derivation read (no scrub) so the two are comparable.
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stub = dir.path().join("Probe.proj");
    std::fs::write(&stub, "<Project/>").expect("write probe project");
    let mut cmd = Command::new("dotnet");
    cmd.args([
        "msbuild",
        "-nologo",
        "-getProperty:MSBuildUserExtensionsPath",
    ]);
    cmd.arg(&stub);
    // `msbuild_user_extensions_path` computes the platform *default*; a real
    // `MSBuildUserExtensionsPath` override anywhere in the process env would
    // make MSBuild report that instead. Strip every case-variant (MSBuild reads
    // the property case-insensitively; env var names are case-sensitive on Unix)
    // so the probe computes the same default the derivation does.
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .eq_ignore_ascii_case("MSBuildUserExtensionsPath")
        {
            cmd.env_remove(&name);
        }
    }
    let out = BoundedCommand::new(cmd)
        .timeout(Duration::from_secs(120))
        .run_ok("dotnet msbuild -getProperty:MSBuildUserExtensionsPath");
    let expected = String::from_utf8(out.stdout)
        .expect("probe output is UTF-8")
        .trim()
        .to_string();

    assert_eq!(
        derived, expected,
        "the server's derived MSBuildUserExtensionsPath must equal MSBuild's own \
         stored value"
    );
}

#[test]
fn plain_net10_reaches_certainty_through_the_server_environment() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("borzoi-uep-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    let fsproj = root.join("App.fsproj");
    std::fs::write(&fsproj, PLAIN_NET10).unwrap();

    let mut env = SdkDiscoveryEnv::from_process_env();
    // The seed the fix installs must be present in the production environment
    // (any case — a real differently-cased override counts); if this key is
    // absent the certainty assertion below would be vacuous.
    assert!(
        env.build_environment
            .keys()
            .any(|key| key.eq_ignore_ascii_case("MSBuildUserExtensionsPath")),
        "from_process_env must carry a MSBuildUserExtensionsPath value"
    );
    // Isolate from the developer's real global MSBuild extensions: point the
    // user-extensions path at an empty directory (escaped into the value domain,
    // exactly as the seed does), so the `ImportBefore`/`ImportAfter` globs find
    // nothing and certainty depends only on the mechanism under test, not on
    // whatever the running profile happens to have installed.
    let extensions = tempfile::TempDir::new().expect("extensions tempdir");
    env.build_environment
        .retain(|key, _| !key.eq_ignore_ascii_case("MSBuildUserExtensionsPath"));
    env.build_environment.insert(
        "MSBuildUserExtensionsPath".to_string(),
        borzoi_msbuild::escape(&extensions.path().to_string_lossy()),
    );

    let disc = SdkDiscovery::for_project(&fsproj, &env).expect("SDK discovery");
    let resolver: &SdkResolver<'_> = &|name| disc.resolve(name);
    let glob: &GlobResolver<'_> = &glob_resolver::resolve;
    let parsed = parse_fsproj_with_imports(
        PLAIN_NET10,
        &fsproj,
        &HashMap::new(),
        &env.build_environment,
        Some(resolver),
        Some(glob),
    )
    .expect("parse");

    assert!(
        !parsed.package_references_uncertain,
        "a plain net10.0 SDK project must yield a certain PackageReference set \
         through the server's derived environment; causes: {:#?}",
        parsed.package_reference_uncertainties
    );
    assert!(
        parsed
            .package_references
            .iter()
            .any(|p| p.id.eq_ignore_ascii_case("FSharp.Core")),
        "expected the implicit FSharp.Core PackageReference; got {:?}",
        parsed
            .package_references
            .iter()
            .map(|p| &p.id)
            .collect::<Vec<_>>()
    );

    std::fs::remove_dir_all(&root).ok();
}

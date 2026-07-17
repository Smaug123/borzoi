#!/usr/bin/env bash
#
# Regenerate nix/deps.json: the pinned NuGet dependency set that the flake's
# offline package store is built from.
#
# Run it as the flake app, from the repository root:
#
#     nix run .#fetch-deps            # writes nix/deps.json
#     nix run .#fetch-deps -- path    # writes `path` instead
#
# The app provides a pinned dotnet SDK (must match the flake's `dotnet-sdk`)
# plus `nuget-to-json` on PATH. The script restores every .NET project that
# `cargo test` drives — the C# sidecar and its xUnit tests, fcs-dump, the
# assembly-crate fixtures, and the csharp-sidecar test fixtures — into a
# throwaway global-packages folder fed only by nuget.org, then hands that
# folder to `nuget-to-json`, which emits one `{pname, version, hash}` record
# per restored .nupkg.
#
# Restores are framework-dependent (no `--runtime`): the repo never does a
# RID-specific `dotnet publish`, so the closure carries no per-RID runtime
# packs and the resulting deps.json is portable across CI (linux) and dev
# (darwin). Implicit reference/runtime packs and the implicit FSharp.Core
# come from the SDK's own `packs/`, not NuGet, so they never enter the set.
set -euo pipefail

if [[ ! -f flake.nix || ! -d crates ]]; then
  echo "error: run this from the repository root (flake.nix not found)" >&2
  exit 1
fi

repo_root=$PWD
out=${1:-nix/deps.json}

work=$(mktemp -d -t borzoi-fetch-deps.XXXXXX)
trap 'chmod -R +w "$work" 2>/dev/null || true; rm -rf "$work"' EXIT

# Isolate from the caller's NuGet state so the resolved versions depend only
# on the pinned SDK and nuget.org, not on whatever the dev machine has cached.
# The dev shell exports RestoreSources/NUGET_PACKAGES to pin restores at the
# *existing* offline store; inheriting them here (the usual case, since this is
# run as `nix run .#fetch-deps` from the shell) would make regeneration resolve
# from the old feed — so a new or bumped package would silently restore its
# stale version or fail NU1101 instead of being fetched fresh from nuget.org.
unset RestoreSources NUGET_PACKAGES
export HOME="$work/home"
mkdir -p "$HOME"
export DOTNET_CLI_TELEMETRY_OPTOUT=1
export DOTNET_NOLOGO=1
export DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1

config="$work/nuget.config"
packages="$work/packages"
cat >"$config" <<'EOF'
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" protocolVersion="3" />
  </packageSources>
</configuration>
EOF

# Every project cargo test shells out to dotnet for (see AGENTS.md "Core" scope:
# the fcs-dump corpus differential sweep is #[ignore]'d and excluded).
project_list="$work/projects.txt"
{
  echo tools/csharp-sidecar/csharp-sidecar.csproj
  echo tools/csharp-sidecar.tests/csharp-sidecar.tests.csproj
  echo tools/fcs-dump/fcs-dump.fsproj
  echo tools/nuget-oracle/nuget-oracle.fsproj
  echo tools/msbuild-condition-oracle/msbuild-condition-oracle.fsproj
  find crates/assembly/tests/fixtures/assembly -maxdepth 2 \
    \( -name '*.csproj' -o -name '*.fsproj' \) | sort
  find tools/csharp-sidecar/test-fixtures \
    \( -name '*.csproj' -o -name '*.fsproj' \) | sort
} >"$project_list"

while IFS= read -r proj; do
  echo "restoring $proj" >&2
  dotnet restore "$repo_root/$proj" \
    --packages "$packages" \
    --configfile "$config"
done <"$project_list"

# The bundled-end-to-end test (crates/lsp/tests/csharp_sidecar_bundled_e2e.rs)
# writes a fresh net10.0 F# exe + C# library into a temp dir and restores them
# there. Capture that closure too so the temp-dir restore is offline-clean,
# even though today it resolves entirely from the SDK packs and adds nothing.
syn="$work/syn"
mkdir -p "$syn/csharp" "$syn/fsharp"
cat >"$syn/csharp/Lib.csproj" <<'EOF'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
</Project>
EOF
printf 'namespace Lib; public class C { public static int X => 1; }\n' >"$syn/csharp/Lib.cs"
cat >"$syn/fsharp/App.fsproj" <<'EOF'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="App.fs" />
    <ProjectReference Include="..\csharp\Lib.csproj" />
  </ItemGroup>
</Project>
EOF
printf 'module App\n[<EntryPoint>]\nlet main _ = 0\n' >"$syn/fsharp/App.fs"
echo "restoring synthetic bundled-e2e pair" >&2
dotnet restore "$syn/fsharp/App.fsproj" --packages "$packages" --configfile "$config"

echo "writing $out" >&2
nuget-to-json "$packages" >"$repo_root/$out"
echo "wrote $repo_root/$out" >&2

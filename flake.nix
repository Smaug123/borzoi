{
  inputs = {
    # The full (not `-small`) channel: Hydra builds the whole package set
    # before it advances, so big darwin dependencies (e.g. codex's
    # livekit-libwebrtc) come from cache.nixos.org instead of being built —
    # and possibly failing — locally. `-small` advances faster but left us
    # compiling libwebrtc from source on 2026-07-09 (and its link step
    # crashes cctools ld on aarch64-darwin).
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The F# compiler source the differential/MSBuild-diff tests walk
    # (`BORZOI_CORPUS`). The single source of truth for the corpus
    # revision; `flake = false` so Nix fetches just the source tree
    # (content-addressed, cache-friendly) rather than evaluating it as a
    # flake. `nix develop` exports its store path as `BORZOI_CORPUS`
    # (see the shellHook), so the tests need no on-disk checkout of the F#
    # compiler — bump the rev here to move the corpus.
    #
    # The rev tracks the compiler shipped in the devshell's default SDK
    # (`dotnet-sdk_10`, currently 10.0.301). To rederive it for a new SDK:
    # dotnet/sdk tag `v<sdk-version>` → `eng/Version.Details.xml`'s
    # `Microsoft.FSharp.Compiler` entry names a dotnet/dotnet (VMR) sha →
    # that VMR rev's `src/source-manifest.json` maps it to the dotnet/fsharp
    # commit. The corpus's own `global.json` then dictates which extra SDK
    # feature band the devshell must combine in (see `dotnet-sdk` below).
    fsharp-src = {
      url = "github:dotnet/fsharp/c3c01c991d17643700d343cee5c5a1e20c06ce03";
      flake = false;
    };
  };

  outputs = { nixpkgs, flake-utils, crane, rust-overlay, fsharp-src, ... }:
    let
      inherit (nixpkgs) lib;

      mkPkgs = system: import nixpkgs {
        inherit system;
        config.allowUnfree = true;
        overlays = [ (import rust-overlay) ];
      };

      mkRustToolchain = pkgs:
        pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "clippy" "rustfmt" ];
        };

      # Cargo's own fetcher (used inside crane's fixed-output deps build)
      # already targets `static.crates.io`, sidestepping the curl-UA 403 that
      # nixpkgs's legacy `importCargoLock` hits on the `crates.io/api/v1`
      # endpoint.
      mkRustSource = pkgs: craneLib:
        let
          sourceRoot = toString ./.;
          repoFilter = path: type:
            let
              rel = lib.removePrefix "${sourceRoot}/" (toString path);
            in
            rel == "Cargo.lock"
            || rel == "Cargo.toml"
            || rel == "crates"
            || lib.hasPrefix "crates/" rel
            # `tools/astgen` is a workspace member (the AST-facade generator), so
            # the workspace manifest references it; the filtered source must carry
            # it or `cargo metadata` fails on a missing member. `rel == "tools"`
            # only lets the walk descend — the rest of `tools/` (fcs-dump,
            # csharp-sidecar, …) is still pruned by falling through to `false`.
            || rel == "tools"
            || rel == "tools/astgen"
            || lib.hasPrefix "tools/astgen/" rel;
        in
        pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (craneLib.filterCargoSources path type) && (repoFilter path type);
        };

      # `doCheck = false`: the crate build does not run `cargo test`. The
      # dotnet-driven tests instead run offline in CI via
      # `nix develop --command cargo test` (see the `rust` job in
      # `.github/workflows/ci.yml`), where the devShell's shellHook points
      # NuGet at the offline closure (`nugetDeps`) and `BORZOI_CORPUS`
      # at the `fsharp-src` input — so they already hit the shared Nix cache
      # for packages without touching nuget.org.
      #
      # Running them inside the nix *build* sandbox too (`doCheck = true`)
      # was considered and deliberately not done: it buys only sandbox
      # hermeticity over the `nix develop` path, at a large cost. The whole
      # .NET surface would have to re-enter `mkRustSource`
      # (`craneLib.filterCargoSources` keeps only `.rs`/`.toml`, stripping
      # every `.cs`/`.fs`/`*proj`; all of `tools/` except `tools/astgen` is
      # excluded outright) — ~30
      # fixture/sidecar projects — plus a writable HOME and an in-sandbox
      # rebuild of the NUGET symlink farm, and every `nix build` would then
      # rerun the full suite. Not worth it while `nix develop` already
      # runs them offline.
      #
      # `extraArgs` lets callers layer on, e.g., `cargoExtraArgs = "--locked
      # --features otel"` for an OpenTelemetry-enabled build (see `packages.otel`
      # below) without disturbing the lean default. `extraArgs` is shared with the
      # dependency-only build; `finalArgs` applies to the *package* build only —
      # for install-time steps (e.g. `postInstall` wrapping) that have no meaning
      # in the artifact-only pass and would fail there (no binary to wrap yet).
      mkBorzoi = pkgs: craneLib: extraArgs: finalArgs:
        let
          src = mkRustSource pkgs craneLib;
          commonArgs = {
            inherit src;
            pname = "borzoi";
            version = "0.1.0";
            strictDeps = true;
            doCheck = false;
            # The Rust build sandbox has no .NET SDK (the sidecar is a separate
            # `buildDotnetModule` wired in via the wrapper below), so tell the
            # crate's `build.rs` not to attempt — and warn about failing — the
            # in-tree `OUT_DIR` sidecar build. This is an attribute of the
            # *package* derivation only; `inputsFrom = [ borzoi ]` in
            # the devShell does not propagate it, so `nix develop` still builds
            # and tests the bundled sidecar the ordinary way.
            BORZOI_SIDECAR_SKIP_INTREE_BUILD = "1";
          } // extraArgs;
          cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
            pname = "borzoi-deps";
          });
        in
        craneLib.buildPackage (commonArgs // finalArgs // {
          inherit cargoArtifacts;
        });
    in
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = mkPkgs system;
        rustToolchain = mkRustToolchain pkgs;
        craneLib = (crane.mkLib pkgs).overrideToolchain (_: rustToolchain);

        # Offline cargo registry: every crates.io dependency in `Cargo.lock`
        # vendored into the store, with a `config.toml` that rewrites
        # `[source.crates-io]` to point at it. The devShell's shellHook wires
        # this into `CARGO_HOME` so `nix develop --command cargo …` resolves
        # crates from the Nix closure — the cargo analogue of the `nugetDeps`
        # closure below. Without it the CI lint/test jobs run cargo online on
        # the ephemeral, network-restricted guests: on a cold cache cargo's
        # first command refreshes the crates.io index itself (its own fetcher,
        # outside Nix), and crates.io isn't reachable from the guest, so the
        # fetch dies with a TLS reset — a cold-cache-dependent flake.
        #
        # This is the *same* derivation crane already builds for the package
        # (`buildDepsOnly` vendors the identical `mkRustSource` src), so the
        # per-crate `.crate` fixed-output derivations are content-addressed and
        # served from the shared binary cache rather than re-fetched; it only
        # rebuilds when `Cargo.lock` changes.
        cargoVendorDir = craneLib.vendorCargoDeps {
          src = mkRustSource pkgs craneLib;
        };

        # The default binary enables `sourcelink-fetch`: go-to-definition into a
        # referenced assembly whose PDB lacks embedded source falls back to
        # fetching it from the SourceLink URL (see `crates/lsp/Cargo.toml`). The
        # explicit `--locked` restores crane's default, which overriding
        # `cargoExtraArgs` would otherwise drop.
        borzoi = mkBorzoi pkgs craneLib {
          cargoExtraArgs = "--locked --features sourcelink-fetch";
        } (sidecarWrap csharp-sidecar);
        # OpenTelemetry-enabled binary for profiling (`nix build .#otel`). The
        # HTTP/OTLP exporter compiles without a TLS backend, so no extra system
        # inputs are needed. See `crates/lsp/src/telemetry.rs`.
        borzoi-otel = mkBorzoi pkgs craneLib {
          cargoExtraArgs = "--locked --features otel";
        } (sidecarWrap csharp-sidecar);

        # The .NET SDK every dotnet-driven test shells out to. Pinned once
        # here and reused for the devShell, the fetch-deps app, and any future
        # build-sandbox use. The default SDK must match what `nix/deps.json`
        # was generated against — Roslyn 5.3.0 (referenced by the C# sidecar)
        # ships with this SDK — while the 10.0.1xx SDK is present so the
        # pinned F# compiler corpus can satisfy its `global.json` (the
        # corpus rev matching SDK 10.0.301's compiler pins 10.0.105, which
        # `latestPatch` roll-forward resolves within the 1xx band).
        dotnet-sdk = pkgs.dotnetCorePackages.combinePackages [
          pkgs.dotnet-sdk_10
          pkgs.dotnetCorePackages.sdk_10_0_1xx
        ];

        # Offline NuGet store: a content-addressed closure of every package
        # `cargo test` restores (the C# sidecar, fcs-dump, the xUnit tests,
        # the assembly fixtures, and the bundled-e2e synthetic pair). Built
        # from the pinned `nix/deps.json`; regenerate that with
        # `nix run .#fetch-deps`. Because each .nupkg lands in its own store
        # path, a shared binary cache serves the whole set to ephemeral CI
        # builders instead of anyone hitting nuget.org.
        nugetDeps = pkgs.mkNugetDeps {
          name = "borzoi";
          sourceFile = ./nix/deps.json;
        };

        # The C# sidecar (Roslyn metadata-emit service for C# `<ProjectReference>`
        # resolution) as a framework-dependent .NET publish. Built offline from
        # the same `nix/deps.json` closure the devShell restores against — the
        # file is a superset (it also covers fcs-dump / fixtures / tests), and
        # extra entries are harmless: `buildDotnetModule` only fails on a package
        # the restore needs but the closure lacks.
        #
        # Framework-dependent (not self-contained): the LSP spawns it as
        # `dotnet <dll>` using the runtime it already discovered, so the DLL only
        # needs its `net10.0` runtimeconfig — we never invoke buildDotnetModule's
        # own `$out/bin` wrapper. This is the packaging half of plan-doc D13; the
        # runtime discovery half (the `BORZOI_SIDECAR_DLL` override wired up
        # by `sidecarWrap` below) already shipped.
        csharp-sidecar = pkgs.buildDotnetModule {
          pname = "csharp-sidecar";
          version = "0.1.0";
          # Just the sidecar project: drop its dev build outputs and the fixture
          # tree (each fixture is its own project; none is referenced by the
          # csproj, which already `<Compile Remove>`s `test-fixtures/**`).
          src = lib.cleanSourceWith {
            src = ./tools/csharp-sidecar;
            filter = path: type:
              let rel = lib.removePrefix "${toString ./tools/csharp-sidecar}/" (toString path);
              in !(rel == "bin" || lib.hasPrefix "bin/" rel
                 || rel == "obj" || lib.hasPrefix "obj/" rel
                 || rel == "test-fixtures" || lib.hasPrefix "test-fixtures/" rel);
          };
          projectFile = "csharp-sidecar.csproj";
          nugetDeps = ./nix/deps.json;
          # A *single* SDK, not the combined `dotnet-sdk`: buildDotnetModule
          # symlinks the SDK's bundled runtime packs into its offline nuget
          # fallback, and two SDKs contribute the same pack
          # (`microsoft.dotnet.ilcompiler`, …) twice, colliding in
          # `configureNuget`. `dotnet-sdk_10` is the one `nix/deps.json` was
          # generated against and that ships the Roslyn (5.3.0) the sidecar pins,
          # so it's both sufficient and correct. Run under the matching runtime.
          dotnet-sdk = pkgs.dotnet-sdk_10;
          dotnet-runtime = pkgs.dotnetCorePackages.runtime_10_0;
          executables = [ "csharp-sidecar" ];
        };

        # Wrap the LSP binary so it finds the sidecar via the D13 override env
        # var (see `crates/lsp/src/csharp_sidecar/process.rs::resolve_sidecar_dll`).
        # Pointing at the separate sidecar derivation by store path avoids copying
        # its whole publish tree into the Rust output. `buildDotnetModule`
        # publishes the app under `$out/lib/<pname>/`.
        sidecarWrap = sidecar: {
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postInstall = ''
            wrapProgram $out/bin/borzoi \
              --set BORZOI_SIDECAR_DLL ${sidecar}/lib/csharp-sidecar/csharp-sidecar.dll
          '';
        };

        # Regenerates nix/deps.json. Run as `nix run .#fetch-deps` from the
        # repo root; it restores every project against nuget.org in an
        # isolated home and re-emits the {pname,version,hash} set.
        fetch-deps = pkgs.writeShellApplication {
          name = "fetch-deps";
          runtimeInputs = [
            dotnet-sdk
            pkgs.nuget-to-json
            pkgs.bash
            pkgs.coreutils
            pkgs.findutils
          ];
          text = ''exec bash "''${PWD}/nix/fetch-deps.sh" "$@"'';
        };
      in
      {
        packages.default = borzoi;
        packages.otel = borzoi-otel;
        packages.nugetDeps = nugetDeps;
        # Exposed for isolated verification (`nix build .#csharp-sidecar`) — a
        # faster, narrower check than the full wrapped LSP when iterating on the
        # sidecar's .NET build.
        packages.csharp-sidecar = csharp-sidecar;

        apps.fetch-deps = {
          type = "app";
          program = "${fetch-deps}/bin/fetch-deps";
        };

        devShells.default = craneLib.devShell {
          inputsFrom = [ borzoi ];

          packages = [
            # For tools/fcs-dump: a small F# binary that prints FCS ASTs so
            # the Rust parser can be differentially tested against the F#
            # compiler's own parser. Pinned to .NET 10 (current stable LTS);
            # the F# compiler source we test against uses a prerelease SDK
            # we don't want to take a dependency on.
            dotnet-sdk
          ];

          # Make every `dotnet` invocation under `cargo test` resolve packages
          # from the offline store instead of nuget.org.
          #
          #   - RestoreSources is read by MSBuild as a global property; setting
          #     it overrides whatever sources the user's ~/.nuget config lists,
          #     pinning restore to the local feed (plus the SDK's own
          #     library-packs, which MSBuild always adds). A package missing
          #     from both then fails loudly (NU1101) rather than silently
          #     fetching from the network, so an incomplete deps.json is caught
          #     instead of masked. `unset RestoreSources` if you deliberately
          #     need nuget.org; regenerate the closure with `nix run .#fetch-deps`.
          #
          #   - NUGET_PACKAGES points at a *writable* symlink farm, not the
          #     read-only store directly. F# projects pull an implicit
          #     FSharp.Core that ships in the SDK's library-packs (a different
          #     patch from the one our explicit deps pin); NuGet insists on
          #     installing it into the global-packages folder, which fails if
          #     that folder is read-only. The farm symlinks each store package
          #     in (no copy) while leaving the id-level dirs writable, so NuGet
          #     can drop those few SDK-sourced packages alongside. It's an
          #     inherited env var, so it also governs the bundled-e2e restore
          #     in a temp dir outside the repo. Keyed by the store path, so it
          #     rebuilds when the closure changes; built atomically so an
          #     interrupted shell never leaves a partial farm.
          shellHook = ''
            # Point cargo at the offline vendored registry (see `cargoVendorDir`
            # above), the cargo counterpart of the NuGet env below. A dedicated
            # CARGO_HOME keeps the generated config out of the developer's
            # ~/.cargo while staying writable for cargo's own bookkeeping; it's
            # keyed by the vendor-dir store path, so it's recreated whenever
            # `Cargo.lock` changes. CARGO_NET_OFFLINE is belt-and-suspenders:
            # with the vendor dir wired up nothing should need the network, so
            # any gap — e.g. a dependency added to a Cargo.toml without
            # re-vendoring — fails loudly with an offline error instead of
            # silently flaking on a blocked crates.io fetch. `unset
            # CARGO_NET_OFFLINE` (and the CARGO_HOME override) if you
            # deliberately need crates.io, e.g. `cargo add`; the vendor closure
            # refreshes on the next `nix develop` once `Cargo.lock` changes.
            export CARGO_NET_OFFLINE=true
            cargoHome="''${XDG_CACHE_HOME:-$HOME/.cache}/borzoi/cargo/${baseNameOf cargoVendorDir}"
            # crane's `vendorCargoDeps` emits its generated source-replacement
            # config at the *root* of the vendor dir ($out/config.toml), not
            # under .cargo/ (the latter is only how crane *captures* a project's
            # own config on the way in). Its `directory =` keys are absolute
            # store paths, so copying just this one file into a standalone
            # CARGO_HOME fully wires source replacement — this mirrors crane's
            # own configureCargoVendoredDepsHook, which appends the same file to
            # $CARGO_HOME/config.toml during its builds.
            if [ ! -f "$cargoHome/config.toml" ]; then
              mkdir -p "$cargoHome"
              cp ${cargoVendorDir}/config.toml "$cargoHome/config.toml"
            fi
            export CARGO_HOME=$cargoHome

            export DOTNET_ROOT=${dotnet-sdk}/share/dotnet
            export DOTNET_CLI_TELEMETRY_OPTOUT=1
            export DOTNET_NOLOGO=1
            export DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1
            export DOTNET_NUGET_SIGNATURE_VERIFICATION=false
            export RestoreSources=${nugetDeps}/share/nuget/source

            # Point the differential/MSBuild-diff tests at the pinned F#
            # source tree (the `fsharp-src` flake input), so the corpus
            # revision is content-addressed and the tests need no on-disk
            # checkout of the F# compiler.
            export BORZOI_CORPUS=${fsharp-src}

            nugetStore=${nugetDeps}/share/nuget/packages
            nugetFarm="''${XDG_CACHE_HOME:-$HOME/.cache}/borzoi/${baseNameOf nugetDeps}"
            if [ ! -e "$nugetFarm" ]; then
              mkdir -p "$(dirname "$nugetFarm")"
              tmp=$(mktemp -d "''${nugetFarm}.XXXXXX")
              for idp in "$nugetStore"/*; do
                id=''${idp##*/}
                mkdir -p "$tmp/$id"
                for vp in "$idp"/*; do
                  ln -sfn "$vp" "$tmp/$id/''${vp##*/}"
                done
              done
              mv "$tmp" "$nugetFarm" 2>/dev/null || rm -rf "$tmp"
            fi
            export NUGET_PACKAGES=$nugetFarm

            # Warm .NET first-time-use *serially*, once, before anything fans
            # out. The first `dotnet` command in a fresh home runs NuGet's
            # one-time migration (NuGet.Common.Migrations.MigrationRunner),
            # which takes a process-global named mutex "NuGet-Migrations". On
            # Unix coreclr backs that mutex with files under a hardcoded
            # /tmp/.dotnet/shm/session<N> path (it ignores TMPDIR). When `cargo
            # test` then fans out parallel `dotnet build`s on a cold machine,
            # they race that directory's creation and one dies with
            #   IOException: 'NuGet-Migrations' … mkdir(".../shm/session<N>") …
            #   errno == EEXIST    (dotnet/runtime#91987).
            # DOTNET_SKIP_FIRST_TIME_EXPERIENCE above does NOT suppress this
            # migration on .NET 10 (verified: the sentinel is still written), so
            # we force it to happen here under a single serial invocation. Once
            # the migration sentinel exists every later `dotnet` skips the
            # configurer entirely and never touches the shm path, so the race
            # window is closed. Gated on the sentinel so warm shells pay nothing;
            # `|| true` keeps a hiccup from breaking `nix develop`.
            nugetMigration="''${XDG_DATA_HOME:-$HOME/.local/share}/NuGet/Migrations/1"
            if [ ! -f "$nugetMigration" ]; then
              dotnet nuget locals all --list >/dev/null 2>&1 || true
            fi
          '';
        };
      }
    );
}

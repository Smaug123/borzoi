//! Shared helpers for the LSP crate's integration tests.
//!
//! The crate has one test binary (`tests/all/main.rs`); this is a `mod`-declared
//! submodule of it, so every case group (`tests/all/fcs_bridge.rs`,
//! `tests/all/csharp_sidecar/`, …) reaches it as `crate::common`. (A
//! `tests/foo.rs` *outside* `all/` would be compiled as a second test binary,
//! which is the trap `all_case_groups_are_declared` guards.)
//!
//! The fcs-dump runner here is duplicated in the CST and assembly crates'
//! `tests/all/common/mod.rs`. Each cluster of integration tests needs it, and
//! a shared dev-only harness crate adds more moving parts than it removes;
//! the surface is ~50 lines and rarely changes.

#![allow(dead_code)] // each importer uses a different subset.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use borzoi_oracle_harness::{BatchChild, default_timeout};
use borzoi_spawn::BoundedCommand;
use serde::Deserialize;

/// Budget for the `dotnet build` of `tools/fcs-dump`.
///
/// A cold build restores packages and compiles FCS, which is legitimately
/// minutes, so the bound sits far above the driver's per-child default: it is
/// there to stop a build that has *stalled* — blocked on a NuGet lock held by a
/// concurrent run in a sibling worktree, say — from hanging the suite forever,
/// not to police a slow one.
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// Budget for one whole-project `uses-project` type-check. See [`BUILD_TIMEOUT`]:
/// same reasoning, bigger job.
const PROJECT_TIMEOUT: Duration = Duration::from_secs(3600);

// ============================================================================
// Workspace pathing
// ============================================================================

/// The workspace root, two `..` jumps above the LSP crate's `CARGO_MANIFEST_DIR`.
/// `tools/fcs-dump` lives there.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

// ============================================================================
// fcs-dump invocation
// ============================================================================

/// Run `fcs-dump <subcommand> <source>` and return its stdout as a UTF-8 string.
///
/// Honours `BORZOI_FCS_DUMP` (path to a pre-built self-contained binary)
/// when set; otherwise builds `tools/fcs-dump` **once** per test binary and
/// execs the resulting assembly on every call.
///
/// The build-once strategy avoids the race that `dotnet run` causes: `dotnet
/// run` does an MSBuild incremental-build check on every invocation, mutating
/// `obj/Release/net10.0/*.cache`. When N parallel test threads all call `dotnet
/// run` concurrently they race on those shared files, producing non-deterministic
/// build failures. By building once under a `OnceLock` and then exec-ing the
/// already-built binary directly, the hot path has no shared mutable state.
pub fn invoke_fcs_dump(subcommand: &str, source: &Path) -> String {
    let cmd = if let Some(bin) = std::env::var_os("BORZOI_FCS_DUMP") {
        let mut c = Command::new(bin);
        c.arg(subcommand).arg(source);
        c
    } else {
        let bin = ensure_fcs_dump_built();
        let mut c = Command::new("dotnet");
        c.arg(bin).arg(subcommand).arg(source);
        c
    };

    let out = BoundedCommand::new(cmd).run_ok(format_args!("fcs-dump {subcommand}"));
    String::from_utf8(out.stdout).expect("fcs-dump stdout is UTF-8")
}

/// Build `tools/fcs-dump` once (thread-safe) and return the path to the
/// produced `.dll`. All subsequent callers get the cached path; only the
/// first caller pays the `dotnet build` cost (typically a fast up-to-date
/// check on a warm cache).
fn ensure_fcs_dump_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let project = project_dir();
            let mut cmd = Command::new("dotnet");
            cmd.args(["build", "-c", "Release", "--nologo"])
                .arg(&project);
            BoundedCommand::new(cmd)
                .timeout(BUILD_TIMEOUT)
                .run_ok("dotnet build fcs-dump");
            project
                .join("bin")
                .join("Release")
                .join("net10.0")
                .join("fcs-dump.dll")
        })
        .as_path()
}

pub fn project_dir() -> PathBuf {
    workspace_root().join("tools").join("fcs-dump")
}

/// Path to a real `FSharp.Core.dll` that is always present in the checkout: the
/// `tools/fcs-dump` build drops the FSharp.Compiler.Service dependency's
/// `FSharp.Core.dll` alongside `fcs-dump.dll`, so we reuse the build-once helper
/// and return the sibling. Mirrors the assembly/sema crates' helper.
pub fn ensure_fsharp_core_dll() -> PathBuf {
    ensure_fcs_dump_built()
        .parent()
        .expect("fcs-dump.dll has a parent dir")
        .join("FSharp.Core.dll")
}

/// A real BCL `System.Runtime.dll` reference assembly — the one carrying
/// `System.String`'s public API (its `Length` property, …). Located in the SDK's
/// `Microsoft.NETCore.App.Ref` pack under `$DOTNET_ROOT`, honour
/// `BORZOI_SYSTEM_RUNTIME_DLL` for CI. Used by the project-based member-access
/// hover test to seed a stubbed framework pack with a real assembly.
///
/// **Pack selection**: a `DOTNET_ROOT` may hold several major ref packs (e.g.
/// `10.0.8` *and* `9.0.10`), and a version-string sort mis-picks — `"9.0.10"`
/// sorts *after* `"10.0.0"` lexicographically. Instead, pick any pack that
/// actually contains `ref/net10.0/System.Runtime.dll`.
pub fn ensure_system_runtime_dll() -> PathBuf {
    if let Some(explicit) = std::env::var_os("BORZOI_SYSTEM_RUNTIME_DLL") {
        return PathBuf::from(explicit);
    }
    let dotnet_root = std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .expect("DOTNET_ROOT unset (run under `nix develop`, or set BORZOI_SYSTEM_RUNTIME_DLL)");
    let packs = dotnet_root.join("packs").join("Microsoft.NETCore.App.Ref");
    std::fs::read_dir(&packs)
        .unwrap_or_else(|e| panic!("read ref packs dir {}: {e}", packs.display()))
        .filter_map(|e| e.ok())
        .map(|e| {
            e.path()
                .join("ref")
                .join("net10.0")
                .join("System.Runtime.dll")
        })
        .find(|p| p.exists())
        .unwrap_or_else(|| {
            panic!(
                "no ref/net10.0/System.Runtime.dll under any pack in {}",
                packs.display()
            )
        })
}

/// A minimal `project.assets.json` naming the `Microsoft.NETCore.App` framework
/// reference for `net10.0` — enough for `build_assembly_env` to enumerate a
/// stubbed framework pack's DLLs. A (dummy) `packageFolders` entry is required —
/// the resolver rejects an assets file with none.
pub fn minimal_assets_json(package_folder: &Path) -> String {
    serde_json::json!({
        "version": 3,
        "targets": { "net10.0": {} },
        "libraries": {},
        "packageFolders": { package_folder.to_str().unwrap(): {} },
        "project": {
            "frameworks": {
                "net10.0": { "frameworkReferences": { "Microsoft.NETCore.App": {} } }
            }
        }
    })
    .to_string()
}

/// A "restored" temp project whose `AssemblyEnv` carries a real
/// `System.Runtime.dll` (so `System.String` and its members resolve), compiling
/// `src` as its single file. Returns the wired-up [`State`](borzoi::server::State)
/// and the file `Url`. The [`TempDir`](tempfile::TempDir) is leaked so the paths
/// outlive the returned `State` (test-only). Shared by the Stage 3.3b member-name
/// hover / go-to-definition / completion integration tests.
pub fn runtime_project_state(src: &str) -> (borzoi::server::State, lsp_types::Url) {
    use borzoi::sdk_discovery::SdkDiscoveryEnv;
    use borzoi::server::State;
    use borzoi::workspace::Workspace;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    let tmp = Box::leak(Box::new(tempfile::TempDir::new().unwrap()));
    let root = tmp.path();
    let dotnet_root = root.join("dotnet");
    let pack = dotnet_root
        .join("packs")
        .join("Microsoft.NETCore.App.Ref")
        .join("10.0.0")
        .join("ref")
        .join("net10.0");
    std::fs::create_dir_all(&pack).unwrap();
    let real_runtime = ensure_system_runtime_dll();
    std::fs::copy(&real_runtime, pack.join("System.Runtime.dll"))
        .unwrap_or_else(|e| panic!("copy System.Runtime.dll: {e}"));
    let pkgs = root.join("pkgs");
    std::fs::create_dir_all(&pkgs).unwrap();
    let proj = root.join("P.fsproj");
    let src_path = root.join("Lib.fs");
    write(
        &proj,
        r#"<Project>
          <ItemGroup><Compile Include="Lib.fs" /></ItemGroup>
        </Project>"#,
    );
    write(&src_path, src);
    write(
        &root.join("obj").join("project.assets.json"),
        &minimal_assets_json(&pkgs),
    );
    let env = SdkDiscoveryEnv {
        dotnet_root: Some(dotnet_root),
        ..SdkDiscoveryEnv::default()
    };
    let mut state = State::default();
    state.workspace = Workspace::with_env(env);
    let uri = lsp_types::Url::from_file_path(&src_path).unwrap();
    state.docs.insert(uri.clone(), src.to_string());
    (state, uri)
}

// ============================================================================
// fcs-dump `uses-project` — the cross-file + assembly resolution oracle
//
// Ported from `crates/sema/tests/all/common/mod.rs` (the same duplication the
// module header notes for `invoke_fcs_dump`): the LSP crate needs the
// project-mode oracle for its own real-project differential, and a shared
// dev-only harness crate is more moving parts than the copied ~120 lines.
// ============================================================================

/// A configured `fcs-dump` [`Command`] for `subcommand`, honouring
/// `BORZOI_FCS_DUMP` (a pre-built binary) or the build-once `.dll` run via
/// `dotnet`. The caller wires stdin/stdout/env as needed.
fn fcs_dump_command(subcommand: &str) -> Command {
    if let Some(bin) = std::env::var_os("BORZOI_FCS_DUMP") {
        let mut c = Command::new(bin);
        c.arg(subcommand);
        c
    } else {
        let bin = ensure_fcs_dump_built();
        let mut c = Command::new("dotnet");
        c.arg(bin).arg(subcommand);
        c
    }
}

/// Run `fcs-dump uses-project` over `paths` (Compile order), with `refs` (extra
/// `-r:` assembly DLLs — the project's resolved NuGet/framework closure) exposed
/// via `BORZOI_FCS_EXTRA_REFS`, `defines` (the `#if` symbols the caller
/// parsed under) via `BORZOI_FCS_DEFINES`, and `lang_version` (the project's
/// resolved `<LangVersion>`, canonical spelling) via `BORZOI_FCS_LANGVERSION`,
/// and return its JSON stdout. Type-checks the files as one project so cross-file
/// *and* referenced-assembly names resolve, under the caller's conditional-
/// compilation symbols and pinned language version.
///
/// `lang_version` should be `None` when the project uses the SDK-default version
/// (FCS's default already agrees with our `LanguageVersion::DEFAULT`, so no
/// `--langversion` flag is threaded) and `Some(canonical)` for a non-default pin.
pub fn invoke_fcs_dump_project_with_refs(
    paths: &[&Path],
    refs: &[&Path],
    defines: &[&str],
    lang_version: Option<&str>,
) -> String {
    let mut cmd = fcs_dump_command("uses-project");
    if !refs.is_empty() {
        let joined = refs
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(";");
        cmd.env("BORZOI_FCS_EXTRA_REFS", joined);
    }
    if !defines.is_empty() {
        cmd.env("BORZOI_FCS_DEFINES", defines.join(";"));
    }
    if let Some(lang) = lang_version {
        cmd.env("BORZOI_FCS_LANGVERSION", lang);
    }
    // The Compile order goes in on stdin; `BoundedCommand` streams it from its
    // own thread while draining both output pipes, so a project large enough to
    // fill a pipe buffer can't deadlock the round-trip (writing stdin
    // synchronously with the output pipes undrained, as this used to, is exactly
    // that bug — invisible at a handful of paths, a hang at a real one).
    //
    // One invocation type-checks every file in the Compile order, so it gets a
    // project-scale budget rather than the per-snippet default: a bound tight
    // enough to kill a healthy large project would be worse than no bound at all.
    let out = BoundedCommand::new(cmd)
        .stdin_lines(paths.iter().map(|p| p.display().to_string()))
        .timeout(PROJECT_TIMEOUT)
        .run_ok("fcs-dump uses-project");
    String::from_utf8(out.stdout).expect("fcs-dump stdout is UTF-8")
}

/// Lookup byte offset for an FCS `(line, col)` position. FCS uses 1-based lines
/// and 0-based columns counting **UTF-16 code units**, so `col` is walked a char
/// at a time accumulating UTF-16 units. (Ported verbatim from the sema harness.)
pub struct LineIndex<'a> {
    source: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut starts = vec![0, 0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { source, starts }
    }

    pub fn offset(&self, line: u32, col: u32) -> usize {
        let line = line as usize;
        let col = col as usize;
        if line >= self.starts.len() {
            return self.source.len();
        }
        let base = self.starts[line];
        let line_end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.len());
        let mut units = 0usize;
        let mut byte_pos = base;
        for ch in self.source[base..line_end].chars() {
            if units >= col {
                break;
            }
            let next_units = units + ch.len_utf16();
            if next_units > col {
                break;
            }
            units = next_units;
            byte_pos += ch.len_utf8();
        }
        byte_pos.min(self.source.len())
    }
}

#[derive(Deserialize)]
struct RawUse {
    #[serde(rename = "SymbolName")]
    symbol_name: String,
    #[serde(rename = "Range")]
    range: FcsRange,
    #[serde(rename = "IsFromDefinition")]
    is_from_definition: bool,
    #[serde(rename = "DeclRange")]
    decl_range: Option<FcsRange>,
    #[serde(rename = "Assembly", default)]
    assembly: Option<String>,
    #[serde(rename = "FullName", default)]
    full_name: Option<String>,
}

#[derive(Deserialize)]
struct FcsRange {
    #[serde(rename = "File")]
    file: String,
    #[serde(rename = "Start")]
    start: FcsPos,
    #[serde(rename = "End")]
    end: FcsPos,
}

#[derive(Deserialize)]
struct FcsPos {
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Col")]
    col: u32,
}

/// Where a use's symbol is declared, normalised to a project file and a byte
/// range into *that* file (which may differ from the use's own file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclSite {
    pub file: PathBuf,
    pub start: usize,
    pub end: usize,
}

/// One symbol use FCS reported for a project file, normalised to byte offsets,
/// with its cross-file declaration site and (for referenced-assembly targets)
/// the declaring assembly's simple name + the symbol's full name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedProjectUse {
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub is_from_definition: bool,
    pub decl: Option<DeclSite>,
    pub assembly: Option<String>,
    pub full_name: Option<String>,
}

/// All symbol uses FCS reported for one project file.
#[derive(Debug, Clone)]
pub struct FileUses {
    pub path: PathBuf,
    pub uses: Vec<NormalisedProjectUse>,
}

#[derive(Deserialize)]
struct ProjectUsesDump {
    #[serde(rename = "Files")]
    files: Vec<RawFileUses>,
}

#[derive(Deserialize)]
struct RawFileUses {
    #[serde(rename = "Path")]
    path: String,
    #[serde(rename = "Uses")]
    uses: Vec<RawUse>,
}

/// Parse `uses-project` JSON into per-file, byte-offset-normalised uses with
/// cross-file declaration sites. `sources` is `(path, text)` for every file fed
/// to the harness; files are matched to FCS's reported paths by **file name**
/// (robust against `Path.GetFullPath` not resolving symlinks — e.g. the macOS
/// `/Users` firmlink or a Nix store path). Callers must give files distinct
/// names.
pub fn parse_fcs_uses_project(json: &str, sources: &[(PathBuf, String)]) -> Vec<FileUses> {
    let dump: ProjectUsesDump =
        serde_json::from_str(json).expect("fcs-dump uses-project JSON shape");

    let by_name: HashMap<&OsStr, (&Path, &str)> = sources
        .iter()
        .map(|(p, src)| {
            (
                p.file_name().expect("source path has a file name"),
                (p.as_path(), src.as_str()),
            )
        })
        .collect();
    let lookup = |fcs_path: &str| -> Option<(&Path, &str)> {
        by_name.get(Path::new(fcs_path).file_name()?).copied()
    };

    dump.files
        .into_iter()
        .map(|f| {
            // FCS emits one `Files` entry per input path, so every reported file
            // must match a supplied source. A miss means a normalisation/path bug
            // that would silently drop that file's uses — fail loudly instead.
            let (path, src) = lookup(&f.path)
                .unwrap_or_else(|| panic!("fcs-dump reported uses for unknown file {:?}", f.path));
            let idx = LineIndex::new(src);
            let uses = f
                .uses
                .into_iter()
                .map(|u| {
                    let decl = u.decl_range.and_then(|d| {
                        lookup(&d.file).map(|(dpath, dsrc)| {
                            let didx = LineIndex::new(dsrc);
                            DeclSite {
                                file: dpath.to_path_buf(),
                                start: didx.offset(d.start.line, d.start.col),
                                end: didx.offset(d.end.line, d.end.col),
                            }
                        })
                    });
                    NormalisedProjectUse {
                        name: u.symbol_name,
                        start: idx.offset(u.range.start.line, u.range.start.col),
                        end: idx.offset(u.range.end.line, u.range.end.col),
                        is_from_definition: u.is_from_definition,
                        decl,
                        assembly: u.assembly,
                        full_name: u.full_name,
                    }
                })
                .collect();
            FileUses {
                path: path.to_path_buf(),
                uses,
            }
        })
        .collect()
}

// ============================================================
// SDK-resolution differential oracle — Stage 1 harness + fixtures.
//
// See docs/completed/sdk-resolution-oracle-plan.md. This drives the *existing*
// `tools/msbuild-condition-oracle` (unchanged) through its `project` op:
// a synthetic SDK's `Sdk.props` sets `_ResolvedSdkProps` to
// `$(MSBuildThisFileFullPath)`, so evaluating `<Project Sdk="…">` and reading
// that property back yields the `Sdk.props` path MSBuild resolved the SDK to —
// or `None` when MSBuild rejects the project. No new F# op is needed for
// single-root SDKs (verified: the in-process `project` op resolves a synthetic
// NuGet-pinned SDK offline).
//
// The condition-oracle builder below mirrors the one in
// `crates/msbuild/tests/common/mod.rs` (same binary, same content-fingerprint
// marker, so the build artifact is shared). It is duplicated for the same
// reason the fcs-dump runner is duplicated across crate `common` modules:
// each test cluster needs it, and the surface is small and stable.
// ============================================================

/// Build `tools/msbuild-condition-oracle` (unless
/// `BORZOI_MSBUILD_CONDITION_ORACLE` points at a prebuilt binary) and
/// return the apphost path. Mirrors `ensure_oracle_built` in the msbuild
/// crate's test common: a marker file whose *contents* fingerprint the tool's
/// sources, so a branch switch never leaves a stale oracle, while `cargo test`'s
/// serial binaries skip the `dotnet build` after the first.
fn ensure_condition_oracle_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            if let Some(bin) = std::env::var_os("BORZOI_MSBUILD_CONDITION_ORACLE") {
                return PathBuf::from(bin);
            }
            let project = workspace_root()
                .join("tools")
                .join("msbuild-condition-oracle");
            let bin = project.join("bin");
            let apphost = bin
                .join("Release")
                .join("net10.0")
                .join("msbuild-condition-oracle");
            let marker = bin.join(".msbuild-condition-oracle-built");
            let want = format!("{:016x}", condition_oracle_source_fingerprint(&project));

            let fresh = apphost.exists()
                && std::fs::read_to_string(&marker)
                    .map(|recorded| recorded.trim() == want)
                    .unwrap_or(false);
            if !fresh {
                let mut cmd = Command::new("dotnet");
                cmd.args(["build", "-c", "Release", "--nologo"])
                    .arg(&project);
                BoundedCommand::new(cmd)
                    .timeout(BUILD_TIMEOUT)
                    .run_ok("dotnet build msbuild-condition-oracle");
                assert!(
                    apphost.exists(),
                    "dotnet build msbuild-condition-oracle produced no apphost at {apphost:?}"
                );
                let tmp = bin.join(format!(
                    ".msbuild-condition-oracle-built.tmp-{}",
                    std::process::id()
                ));
                if std::fs::write(&tmp, &want).is_ok() && std::fs::rename(&tmp, &marker).is_err() {
                    let _ = std::fs::remove_file(&tmp);
                }
            }
            apphost
        })
        .as_path()
}

/// Hash the oracle's sources plus `flake.lock` (which pins the SDK, hence the
/// MSBuild `MSBuildLocator` loads). File names are hashed before contents so a
/// rename cannot alias two contents to one hash.
fn condition_oracle_source_fingerprint(project: &Path) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut sources: Vec<PathBuf> = std::fs::read_dir(project)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("fs" | "fsproj")
            )
        })
        .collect();
    sources.sort();
    sources.push(workspace_root().join("flake.lock"));

    let mut h = DefaultHasher::new();
    for p in &sources {
        p.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .hash(&mut h);
        std::fs::read(p).unwrap_or_default().hash(&mut h);
    }
    h.finish()
}

/// A resident condition-oracle child, driven for SDK-resolution readback. The
/// spawn factory scrubs the environment to the runtime essentials and then
/// **forces** `NUGET_PACKAGES` to the caller's global-packages folder: the
/// `nix develop` devshell pins `NUGET_PACKAGES` at its vendored registry, and a
/// synthetic-SDK fixture only resolves when the resolver looks in *our* folder.
/// A real override on the spawned `Command` wins for the child (the devshell's
/// pin only bites at the `nix develop` boundary).
pub struct SdkOracle {
    child: BatchChild,
}

impl SdkOracle {
    pub fn spawn(nuget_packages: &Path) -> SdkOracle {
        let bin = ensure_condition_oracle_built().to_path_buf();
        let gpf = nuget_packages.to_path_buf();
        let factory = move || {
            let mut cmd = Command::new(&bin);
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
            // Wins over the inherited (devshell-pinned) NUGET_PACKAGES.
            cmd.env("NUGET_PACKAGES", &gpf);
            cmd
        };
        SdkOracle {
            child: BatchChild::with_factory(
                Box::new(factory),
                "msbuild-condition-oracle (sdk resolution)",
                default_timeout(),
                2,
            ),
        }
    }

    /// Resolve `sdk_ref` (e.g. `"Foo/1.2.3"`) as the SDK of a stub project
    /// written at `project_path`, returning the `Sdk.props` path MSBuild
    /// resolved it to (via the `_ResolvedSdkProps` marker), or `None` when
    /// MSBuild rejects the project. An offline `nuget.config` must already sit
    /// beside `project_path` (see [`write_offline_nuget_config`]); the oracle
    /// writes the `.fsproj` itself.
    pub fn resolve(&mut self, sdk_ref: &str, project_path: &Path) -> Option<PathBuf> {
        let xml = format!(r#"<Project Sdk="{sdk_ref}"><Target Name="B" /></Project>"#);
        let req = serde_json::json!({
            "op": "project",
            "xml": xml,
            "names": ["_ResolvedSdkProps"],
            "path": project_path.to_string_lossy().into_owned(),
        });
        let line = serde_json::to_string(&req).expect("serialise request");
        let response = self.child.request(&line);
        let value: serde_json::Value =
            serde_json::from_str(&response).expect("oracle response is JSON");
        if let Some(err) = value.get("error") {
            panic!("oracle errored on {line}: {err}");
        }
        if value["ok"].as_bool() != Some(true) {
            return None;
        }
        let resolved = value["values"]["_ResolvedSdkProps"]
            .as_str()
            .expect("project op returns the requested property");
        // A name MSBuild never defines reads back as "" through this op; a real
        // resolution always sets the marker to a non-empty path.
        (!resolved.is_empty()).then(|| PathBuf::from(resolved))
    }
}

/// The folder spelling NuGet uses for a package version: lower-cased, the
/// numeric release padded to at least three components (`1.2` → `1.2.0`), a
/// trailing zero fourth component dropped, and any prerelease tag preserved.
/// Enough for the version shapes these tests use; not a full SemVer normaliser.
fn nuget_normalised_version(version: &str) -> String {
    let lower = version.to_ascii_lowercase();
    let (release, prerelease) = match lower.split_once('-') {
        Some((r, p)) => (r, Some(p)),
        None => (lower.as_str(), None),
    };
    let mut parts: Vec<u64> = release.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    while parts.len() < 3 {
        parts.push(0);
    }
    if parts.len() == 4 && parts[3] == 0 {
        parts.pop();
    }
    let mut out = parts
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".");
    if let Some(p) = prerelease {
        out.push('-');
        out.push_str(p);
    }
    out
}

/// The casing of a NuGet SDK package's inner SDK directory. NuGet's own
/// packages use capitalised `Sdk/`, but real third-party SDKs distributed as
/// NuGet packages (Arcade) ship a lowercase `sdk/`, and `collect_from_nuget`
/// probes both. On a case-sensitive filesystem these are distinct directories,
/// so materialising both spellings is what gives the differential teeth over
/// the lowercase probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkDirCasing {
    Upper,
    Lower,
}

impl SdkDirCasing {
    fn dir_name(self) -> &'static str {
        match self {
            SdkDirCasing::Upper => "Sdk",
            SdkDirCasing::Lower => "sdk",
        }
    }
}

/// Materialise a synthetic NuGet-distributed Project SDK in the global-packages
/// folder `gpf`, in the verified offline layout
/// `{gpf}/{id}/{version}/{Sdk|sdk}/Sdk.{props,targets}` plus the `.nuspec` and
/// `.nupkg.metadata` NuGet expects, where `{id}` and `{version}` are the
/// **NuGet-normalised** forms NuGet actually uses for the folder and `casing`
/// selects the inner directory spelling. `Sdk.props` sets the
/// `_ResolvedSdkProps` marker to its own full path. Returns that `Sdk.props`
/// path — the value both the oracle and `SdkDiscovery::resolve` must agree on.
///
/// Normalising the version matters: a caller passing the documented `x.y`
/// spelling (e.g. `1.2`) must land in `.../1.2.0/`, since that is where NuGet
/// (and MSBuild's resolver) looks; writing `.../1.2/` would resolve on our
/// directory scanner but not in real MSBuild, a false differential.
pub fn write_nuget_sdk(gpf: &Path, name: &str, version: &str, casing: SdkDirCasing) -> PathBuf {
    let pkg = gpf
        .join(name.to_ascii_lowercase())
        .join(nuget_normalised_version(version));
    let sdk = pkg.join(casing.dir_name());
    std::fs::create_dir_all(&sdk).expect("create synthetic SDK dir");
    let props = sdk.join("Sdk.props");
    std::fs::write(
        &props,
        "<Project><PropertyGroup>\
         <_ResolvedSdkProps>$(MSBuildThisFileFullPath)</_ResolvedSdkProps>\
         </PropertyGroup></Project>",
    )
    .expect("write Sdk.props");
    std::fs::write(sdk.join("Sdk.targets"), "<Project/>").expect("write Sdk.targets");
    std::fs::write(
        pkg.join(format!("{}.nuspec", name.to_ascii_lowercase())),
        format!(
            "<?xml version=\"1.0\"?><package><metadata>\
             <id>{name}</id><version>{version}</version>\
             <description>d</description><authors>a</authors>\
             </metadata></package>"
        ),
    )
    .expect("write nuspec");
    std::fs::write(
        pkg.join(".nupkg.metadata"),
        "{\"version\":2,\"contentHash\":\"deadbeef\",\"source\":null}",
    )
    .expect("write .nupkg.metadata");
    props
}

/// Write an offline `NuGet.Config` (all package sources cleared) into `dir`, so
/// SDK resolution stays local to the synthetic global-packages folder and never
/// reaches the network. NuGet discovers it by walking up from the project dir.
///
/// The filename must be `NuGet.Config`: NuGet's default settings search is
/// case-sensitive, so on a case-sensitive filesystem a lowercase `nuget.config`
/// is ignored and resolution falls back to ambient sources (i.e. the network).
pub fn write_offline_nuget_config(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create project dir");
    std::fs::write(
        dir.join("NuGet.Config"),
        "<?xml version=\"1.0\"?><configuration>\
         <packageSources><clear/></packageSources></configuration>",
    )
    .expect("write NuGet.Config");
}

/// Write an empty (`{}`) `global.json` into `dir` to establish a hermeticity
/// boundary for SDK resolution. Both real MSBuild and [`SdkDiscovery`] walk
/// upward from the project looking for the nearest `global.json`; without this,
/// an unrelated ancestor file (e.g. one above a nested `TMPDIR`) is consumed —
/// a malformed one breaks resolution, and settings like `sdk.paths` silently
/// change it. An empty object pins nothing, so it stops the walk without
/// otherwise influencing the result.
pub fn write_boundary_global_json(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create project dir");
    std::fs::write(dir.join("global.json"), "{}\n").expect("write global.json");
}

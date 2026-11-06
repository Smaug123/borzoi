//! The driver for `tools/nuget-oracle`: build it once (marker-gated), then
//! drive it as a long-lived JSONL request/response child.
//!
//! The process plumbing is [`BatchChild`]; this module only knows the JSON
//! protocol, how to build the tool, and which SDK's NuGet it runs. It lives here
//! rather than in `borzoi-nuget`'s tests because more than one crate grades
//! against it: `borzoi-nuget`'s differentials, and the LSP's restore-graph
//! oracle, which feeds it the dependency-graph specs MSBuild generates.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use crate::{BatchChild, BoundedCommand};

/// Budget for the one `dotnet build` this driver runs.
///
/// A cold build restores packages and runs a compiler, which is legitimately
/// minutes, so the bound sits far above the harness's per-request default: it is
/// there to stop a build that has *stalled* — blocked on a NuGet lock held by a
/// concurrent run in a sibling worktree, say — from hanging the suite forever,
/// not to police a slow one.
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// The workspace root, two `..` jumps above this crate's manifest dir;
/// `tools/nuget-oracle` lives there.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

fn project_dir() -> PathBuf {
    workspace_root().join("tools").join("nuget-oracle")
}

/// Build `tools/nuget-oracle` (unless `BORZOI_NUGET_ORACLE` points at a
/// prebuilt binary) and return the apphost path. Same content-bearing-marker
/// scheme as `ensure_fcs_dump_built`, and for the same reasons: one marker
/// file whose *contents* are the source fingerprint of the apphost on disk,
/// so branch-switching can never leave a stale oracle answering for the
/// wrong sources, while a run whose sources are unchanged skips the ~2 s
/// `dotnet build` that an earlier run already paid.
fn ensure_oracle_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            if let Some(bin) = std::env::var_os("BORZOI_NUGET_ORACLE") {
                return PathBuf::from(bin);
            }
            let project = project_dir();
            let bin = project.join("bin");
            let apphost = bin.join("Release").join("net10.0").join("nuget-oracle");
            let marker = bin.join(".nuget-oracle-built");
            let want = format!("{:016x}", oracle_source_fingerprint(&project));

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
                    .run_ok("dotnet build nuget-oracle");
                assert!(
                    apphost.exists(),
                    "dotnet build nuget-oracle produced no apphost at {apphost:?}"
                );
                write_marker_atomically(&marker, &want);
            }
            apphost
        })
        .as_path()
}

/// Hash the inputs whose change should force a rebuild: the tool's sources
/// and the flake lock (which pins the SDK and the offline package set).
fn oracle_source_fingerprint(project: &Path) -> u64 {
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
        // File name first so a rename can't alias two contents to one hash.
        p.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .hash(&mut h);
        std::fs::read(p).unwrap_or_default().hash(&mut h);
    }
    h.finish()
}

/// Write `contents` to `marker` atomically (temp + rename), best-effort.
fn write_marker_atomically(marker: &Path, contents: &str) {
    let Some(dir) = marker.parent() else {
        return;
    };
    let tmp = dir.join(format!(".nuget-oracle-built.tmp-{}", std::process::id()));
    if std::fs::write(&tmp, contents).is_ok() && std::fs::rename(&tmp, marker).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The directory of the .NET SDK that `dotnet` resolves from the workspace
/// root. This is the SDK whose NuGet [`Oracle::spawn`] runs, so its
/// `NuGet.Commands` is the one every such oracle verdict was established
/// against.
pub fn sdk_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| sdk_dir_for(Path::new("dotnet"), &workspace_root()))
        .as_path()
}

/// The directory of the .NET SDK that the muxer `dotnet` selects when run in
/// `directory`: `dotnet --version` names it (honouring any `global.json` above
/// `directory`) and `dotnet --list-sdks` says where it lives.
pub fn sdk_dir_for(dotnet: &Path, directory: &Path) -> PathBuf {
    let run = |arg: &str| {
        let mut cmd = Command::new(dotnet);
        cmd.current_dir(directory).arg(arg);
        let out = BoundedCommand::new(cmd).run_ok(format_args!("dotnet {arg}"));
        String::from_utf8(out.stdout).expect("dotnet output is UTF-8")
    };
    let version = run("--version");
    let version = version.trim();
    let listing = run("--list-sdks");
    // Each line is `<version> [<sdk-root>]`, e.g. `10.0.401 [/…/sdk]`.
    let roots: Vec<&str> = listing
        .lines()
        .filter_map(|line| line.strip_prefix(version)?.strip_prefix(" ["))
        .filter_map(|rest| rest.strip_suffix(']'))
        .collect();
    match roots.as_slice() {
        [root] => Path::new(root).join(version),
        _ => panic!(
            "expected exactly one SDK {version} in `dotnet --list-sdks`, found {}:\n{listing}",
            roots.len()
        ),
    }
}

/// A long-lived `nuget-oracle` child driven in lock-step: write one JSON
/// request line, read exactly the one JSON response line it produces.
///
/// [`BatchChild`] owns the process, so a wedged or crashed oracle is killed,
/// respawned, and the request retried rather than blocking forever. Respawning is
/// sound because the protocol is stateless: every request carries everything it
/// is answered from, so a fresh child answers identically.
pub struct Oracle {
    child: BatchChild,
}

impl Oracle {
    /// Spawn the oracle on the NuGet of [`sdk_dir`].
    pub fn spawn() -> Oracle {
        Oracle::spawn_with_environment(&[])
    }

    /// [`Self::spawn`], with each `(name, value)` set in the oracle's
    /// environment before NuGet loads.
    pub fn spawn_with_environment(environment: &[(&str, &str)]) -> Oracle {
        Oracle::spawn_on(sdk_dir(), environment)
    }

    /// Spawn the oracle on the NuGet of the SDK in `sdk` (an
    /// `<dotnet-root>/sdk/<version>` directory), with each `(name, value)` set
    /// in its environment before NuGet loads.
    pub fn spawn_on(sdk: &Path, environment: &[(&str, &str)]) -> Oracle {
        let sdk = sdk.to_str().expect("SDK directory is UTF-8");
        let assignments: Vec<String> = environment
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        let mut args = vec!["--nuget-from", sdk];
        for assignment in &assignments {
            args.extend(["--env", assignment.as_str()]);
        }
        Oracle {
            child: BatchChild::spawn(ensure_oracle_built(), &args),
        }
    }

    /// One request/response round-trip. Panics loudly on an `{"error": ..}`
    /// response — that means the harness or oracle is broken, never a legitimate
    /// differential result. A dead or wedged child is handled beneath us, by
    /// [`BatchChild::request`].
    pub fn request(&mut self, req: &serde_json::Value) -> serde_json::Value {
        let line = serde_json::to_string(req).expect("serialise request");
        let response = self.child.request(&line);

        let value: serde_json::Value =
            serde_json::from_str(&response).expect("nuget-oracle response is JSON");
        if let Some(err) = value.get("error") {
            panic!("nuget-oracle errored on {line}: {err}");
        }
        value
    }
}

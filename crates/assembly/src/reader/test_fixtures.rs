//! Shared fixture builder for the reader's unit tests.
//!
//! Both [`super::tests`] (stage 1) and [`super::typedefs_tests`] (stage 4) need
//! the compiled `MiniLib*` fixtures, and the projector's tests additionally need
//! `LiteralConsts`. Building them from a single `OnceLock` keeps the
//! `dotnet build`s serialized through one initializer — two separate builders
//! racing on the same `obj/`/`bin/` outputs is the failure mode this avoids.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use borzoi_spawn::BoundedCommand;

/// The fixture projects, in a fixed order (callers index into [`project_dlls`]
/// by position, so append rather than insert).
///
/// `LiteralConsts` is here for the projector's unit tests alone — the reader's
/// stage-1/stage-4 tests index only the `MiniLib*` prefix. It carries the
/// `[<CompiledName>]`-renamed `[<Literal>]` whose IL-heuristic projection is
/// pinned in `ecma335_assembly`'s tests, and is deliberately never diffed whole
/// against fcs-dump, so the skip it provokes stays local to that test.
const FIXTURES: [&str; 4] = ["MiniLib", "MiniLibFs", "MiniLibFsExt", "LiteralConsts"];

/// Budget for one fixture `dotnet build`.
///
/// A cold build restores packages and runs a compiler, which is legitimately
/// minutes, so the bound sits far above the driver's per-child default: it is
/// there to stop a build that has *stalled* — blocked on a NuGet lock held by a
/// concurrent run in a sibling worktree, say — from hanging the suite forever,
/// not to police a slow one.
pub(crate) const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

struct Built {
    /// The project `.dll`s, in `FIXTURES` order.
    project: Vec<PathBuf>,
    /// Every managed `.dll` in the fixtures' Release output dirs — the project
    /// outputs plus copied package references (e.g. `FSharp.Core.dll`),
    /// deduplicated by file name so a DLL copied into several outputs is read
    /// once.
    all: Vec<PathBuf>,
}

fn built() -> &'static Built {
    static BUILT: OnceLock<Built> = OnceLock::new();
    BUILT.get_or_init(|| {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assembly");
        let mut project = Vec::with_capacity(FIXTURES.len());
        let mut all: BTreeMap<String, PathBuf> = BTreeMap::new();
        for name in FIXTURES {
            let dir = base.join(name);
            let mut cmd = Command::new("dotnet");
            cmd.args(["build", "-c", "Release", "--nologo"]).arg(&dir);
            BoundedCommand::new(cmd)
                .timeout(BUILD_TIMEOUT)
                .run_ok(format_args!("dotnet build {name} fixture"));
            let out = dir.join("bin/Release/net10.0");
            project.push(out.join(format!("{name}.dll")));
            for entry in std::fs::read_dir(&out).expect("read fixture output dir") {
                let path = entry.expect("dir entry").path();
                if path.extension().and_then(|e| e.to_str()) == Some("dll") {
                    let file = path
                        .file_name()
                        .and_then(|f| f.to_str())
                        .expect("dll file name")
                        .to_string();
                    all.entry(file).or_insert(path);
                }
            }
        }
        Built {
            project,
            all: all.into_values().collect(),
        }
    })
}

/// The project `.dll`s, in `FIXTURES` order.
pub(super) fn project_dlls() -> Vec<PathBuf> {
    built().project.clone()
}

/// Every managed `.dll` produced by the fixtures (project outputs + copied
/// package references), deduplicated by file name. `pub(crate)` so the
/// projector's differential tests (outside the `reader` module) share the
/// corpus.
pub(crate) fn all_dlls() -> Vec<PathBuf> {
    built().all.clone()
}

//! Shared helpers for the msbuild crate's integration tests.
//!
//! Cargo treats subdirectories under `tests/` as modules rather than test
//! binaries, so the integration-test crates that need this (currently
//! `fsproj_implicit_imports.rs`, `fsproj_msbuild_diff.rs`) pull it in
//! with `mod common;`. (A top-level `tests/foo.rs` *would* be compiled
//! as a standalone test binary, which is the trap to avoid.)
//!
//! The `corpus_root` helper is duplicated in the CST and assembly
//! crates' `tests/common/mod.rs`. That surface is stable, tiny, and pure, so the
//! duplication is harmless — unlike the process plumbing, which made the same
//! argument for itself and was wrong (see the [`Oracle`] docs).
//!
//! The [`Oracle`] harness below drives `tools/msbuild-condition-oracle` (the
//! real MSBuild condition evaluator, in-process) for the condition
//! differential tests. Its *process plumbing* is the shared [`BatchChild`]; this
//! module only knows the JSON protocol and how to build the tool. The round-trip
//! used to be a hand-rolled, unbounded `read_line` — copied from the
//! `tools/nuget-oracle` runner, which had copied it from the CST harness, which
//! had meanwhile fixed it — so a wedged oracle hung the suite indefinitely. That
//! is what the shared crate exists to stop.

#![allow(dead_code)] // each importer uses a different subset.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use borzoi_oracle_harness::{BatchChild, BoundedCommand, default_timeout};

use borzoi_msbuild::test_support::{Outcome, PropertyMap, evaluate, substitute};

/// Budget for the one `dotnet build` this harness runs (the condition oracle).
///
/// A cold build restores packages and runs a compiler, which is legitimately
/// minutes, so the bound sits far above the harness's per-request default: it is
/// there to stop a build that has *stalled* — blocked on a NuGet lock held by a
/// concurrent run in a sibling worktree, say — from hanging the suite forever,
/// not to police a slow one.
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// Root of the F# corpus the differential tests walk, taken from the
/// `BORZOI_CORPUS` environment variable.
///
/// `nix develop` sets this to the pinned `fsharp-src` flake input — a
/// content-addressed checkout of the F# compiler in the Nix store (see
/// `flake.nix`). There is no on-disk fallback; run the corpus tests under
/// `nix develop`, or point `BORZOI_CORPUS` at a local F# checkout.
///
/// Panics if the variable is unset or does not resolve to a directory.
pub fn corpus_root() -> PathBuf {
    let root = match std::env::var_os("BORZOI_CORPUS") {
        Some(p) => PathBuf::from(p),
        None => panic!(
            "BORZOI_CORPUS is not set. Run the corpus tests under \
             `nix develop` (which points it at the pinned `fsharp-src` flake \
             input), or set it to a local F# compiler checkout."
        ),
    };
    assert!(
        root.is_dir(),
        "F# corpus root {root:?} (from BORZOI_CORPUS) is not a directory."
    );
    root
}

// ============================================================================
// tools/msbuild-condition-oracle harness
// ============================================================================

/// The workspace root, two `..` jumps above this crate's manifest dir;
/// `tools/msbuild-condition-oracle` lives there.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

fn oracle_project_dir() -> PathBuf {
    workspace_root()
        .join("tools")
        .join("msbuild-condition-oracle")
}

/// Build `tools/msbuild-condition-oracle` (unless
/// `BORZOI_MSBUILD_CONDITION_ORACLE` points at a prebuilt binary) and
/// return the apphost path. Same content-bearing-marker scheme as the
/// fcs-dump / nuget-oracle harnesses, and for the same reasons: a marker file
/// whose *contents* fingerprint the tool's sources, so branch-switching can
/// never leave a stale oracle answering for the wrong sources, while
/// `cargo test`'s serial test binaries skip the `dotnet build` after the first
/// has run it.
fn ensure_oracle_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            if let Some(bin) = std::env::var_os("BORZOI_MSBUILD_CONDITION_ORACLE") {
                return PathBuf::from(bin);
            }
            let project = oracle_project_dir();
            let bin = project.join("bin");
            let apphost = bin
                .join("Release")
                .join("net10.0")
                .join("msbuild-condition-oracle");
            let marker = bin.join(".msbuild-condition-oracle-built");
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
                    .run_ok("dotnet build msbuild-condition-oracle");
                assert!(
                    apphost.exists(),
                    "dotnet build msbuild-condition-oracle produced no apphost at {apphost:?}"
                );
                write_marker_atomically(&marker, &want);
            }
            apphost
        })
        .as_path()
}

/// Hash the inputs whose change should force a rebuild: the tool's sources
/// and the flake lock (which pins the SDK and thus the MSBuild the oracle
/// loads via MSBuildLocator).
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
    let tmp = dir.join(format!(
        ".msbuild-condition-oracle-built.tmp-{}",
        std::process::id()
    ));
    if std::fs::write(&tmp, contents).is_ok() && std::fs::rename(&tmp, marker).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Strip the oracle child's environment down to what the dotnet host needs,
/// making it hermetic with respect to the shell.
///
/// MSBuild folds *every* inherited environment variable in as an initial
/// property, so an ambient `Configuration=…`, `Version=…`, or even
/// `Undefined=…` would make the oracle see a value for a name our Rust side —
/// which only knows the property map we sent — treats as undefined or
/// differently-valued, producing a spurious differential failure unrelated to
/// the condition grammar. A blacklist of known generator names is fragile (any
/// name a future generator adds, or one like `Undefined` we already reference,
/// silently reopens the hole), so clear everything and re-add only the runtime
/// essentials — none of which a generated condition ever references. Mirrors
/// `scrub_msbuild_env` in `fsproj_msbuild_diff.rs`, and for the same reason.
fn scrub_oracle_env(cmd: &mut Command) {
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

/// The environment snapshot the oracle children run under — the same filter
/// [`scrub_oracle_env`] (and the per-test `run_*` spawners) apply to the
/// `dotnet msbuild` child, as the map `parse_fsproj_with_imports` takes for
/// its `environment` parameter. Handing our evaluator the *identical*
/// snapshot keeps certain-implies-exact meaningful: MSBuild folds every
/// child-visible env var in as an initial property, so both sides must see
/// the same set.
pub fn oracle_environment() -> HashMap<String, String> {
    let mut env = HashMap::new();
    for var in ["PATH", "HOME", "TMPDIR"] {
        if let Ok(value) = std::env::var(var) {
            env.insert(var.to_string(), value);
        }
    }
    for (key, value) in std::env::vars() {
        if key.starts_with("DOTNET_") || key.starts_with("NUGET_") {
            env.insert(key, value);
        }
    }
    // Not a real environment variable, but the name is env-honoured
    // (probed: an environment `MSBuildUserExtensionsPath` displaces the
    // computed one), so the snapshot is a faithful way to hand our
    // evaluator the same value the oracle child computes natively.
    env.insert(
        "MSBuildUserExtensionsPath".to_string(),
        msbuild_user_extensions_path().to_string(),
    );
    env
}

/// The host's `MSBuildUserExtensionsPath`, asked of the real MSBuild once
/// (`-getProperty` on an empty stub project under the scrubbed oracle
/// environment) and cached for the test binary's lifetime. .NET computes
/// it from the OS user profile — probed: redirecting `$HOME` does *not*
/// move it — so the only exact source is MSBuild itself.
fn msbuild_user_extensions_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = tempfile::TempDir::new().expect("tempdir for MSBuildUserExtensionsPath probe");
        let stub = dir.path().join("Probe.proj");
        std::fs::write(&stub, "<Project/>").expect("write probe project");
        let mut cmd = Command::new("dotnet");
        cmd.args([
            "msbuild",
            "-nologo",
            "-getProperty:MSBuildUserExtensionsPath",
        ]);
        cmd.arg(&stub);
        scrub_oracle_env(&mut cmd);
        // Route through the serialised, bounded spawner like every other child
        // in the harness — a raw `Command::output()` bypasses the spawn lock
        // (macOS pipe-descriptor leak; see `borzoi-spawn`).
        let out = BoundedCommand::new(cmd)
            .timeout(default_timeout())
            .run_ok("dotnet msbuild for MSBuildUserExtensionsPath probe");
        let value = String::from_utf8(out.stdout)
            .expect("probe output is UTF-8")
            .trim()
            .to_string();
        assert!(
            !value.is_empty(),
            "MSBuildUserExtensionsPath probe returned empty"
        );
        value
    })
}

/// A long-lived `msbuild-condition-oracle` child driven in lock-step: write
/// one JSON request line, read exactly the one JSON response line it produces.
///
/// [`BatchChild`] owns the process: a wedged or crashed oracle is killed,
/// respawned, and the request retried, rather than blocking forever. Respawning
/// is sound because the protocol is stateless — every request carries the full
/// property set it wants evaluated (and the condition or value to evaluate), so
/// a fresh child answers identically.
pub struct Oracle {
    child: BatchChild,
}

impl Oracle {
    pub fn spawn() -> Oracle {
        let bin = ensure_oracle_built().to_path_buf();
        // A factory rather than a bare program+args: the child's environment must
        // be scrubbed (see `scrub_oracle_env`), and every respawn must scrub it
        // the same way or the fresh child would see a different property set.
        let factory = move || {
            let mut cmd = Command::new(&bin);
            scrub_oracle_env(&mut cmd);
            cmd
        };
        Oracle {
            child: BatchChild::with_factory(
                Box::new(factory),
                "msbuild-condition-oracle",
                default_timeout(),
                2,
            ),
        }
    }

    /// One request/response round-trip. Panics loudly on an `{"error": ..}`
    /// response — that means the harness or oracle is broken, never a legitimate
    /// differential result (an illegal condition is the first-class
    /// `{"ok":false}` answer, not an error). A dead or wedged child is handled
    /// beneath us, by [`BatchChild::request`].
    fn request(&mut self, req: &serde_json::Value) -> serde_json::Value {
        let line = serde_json::to_string(req).expect("serialise request");
        let response = self.child.request(&line);

        let value: serde_json::Value =
            serde_json::from_str(&response).expect("oracle response is JSON");
        if let Some(err) = value.get("error") {
            panic!("oracle errored on {line}: {err}");
        }
        value
    }

    /// Ask the real MSBuild evaluator for the condition's truth against
    /// `props`. `Some(bool)` is the value MSBuild computed; `None` means
    /// MSBuild rejects the condition as illegal (`InvalidProjectFileException`).
    pub fn eval(&mut self, condition: &str, props: &[(String, String)]) -> Option<bool> {
        let properties: serde_json::Map<String, serde_json::Value> = props
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        let resp = self.request(&serde_json::json!({
            "op": "eval",
            "condition": condition,
            "properties": properties,
        }));
        if resp["ok"].as_bool() == Some(true) {
            Some(
                resp["value"]
                    .as_bool()
                    .expect("oracle ok response carries a boolean value"),
            )
        } else {
            None
        }
    }

    /// Ask the real MSBuild evaluator to expand `value` as a property body
    /// against `props`. `Some(string)` is the evaluated text MSBuild produced
    /// (`Some("")` when it reduces to empty); `None` means MSBuild threw
    /// evaluating it (a property function that fails — an unparseable version,
    /// an out-of-range indexer, an unknown member).
    pub fn expand(&mut self, value: &str, props: &[(String, String)]) -> Option<String> {
        let properties: serde_json::Map<String, serde_json::Value> = props
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        let resp = self.request(&serde_json::json!({
            "op": "expand",
            "value": value,
            "properties": properties,
        }));
        if resp["ok"].as_bool() == Some(true) {
            Some(
                resp["value"]
                    .as_str()
                    .expect("oracle ok response carries a string value")
                    .to_string(),
            )
        } else {
            None
        }
    }

    /// Evaluate a *whole project document* — `xml` verbatim, the same bytes our
    /// parser is given — and read back `names`. `Some(map)` is what MSBuild
    /// computed; `None` means MSBuild rejects the project.
    ///
    /// This is the only op that can see MSBuild's **XML layer** (insignificant
    /// whitespace, entity decoding, CDATA, comment-split text): [`Self::expand`]
    /// deliberately hands MSBuild a property *body* anchored between sentinels,
    /// so everything that layer does before expansion is invisible to it.
    ///
    /// A name MSBuild never defines reads back as `""`, indistinguishable here
    /// from a defined-empty property. That costs nothing: callers only assert on
    /// names *they* committed a value for.
    /// `path`, when given, is where the document is written and loaded from, so
    /// MSBuild's reserved path derivatives are computed from the same path the
    /// caller hands its own parser. Without it the project is evaluated in
    /// memory and those properties are meaningless to compare.
    /// The `project` op's item-side twin: evaluate `xml` *as the file at `path`*
    /// and read back the `FullPath` of every `item_type` item, in evaluation
    /// order — what `dotnet msbuild -getItem:` reports, but through the resident
    /// oracle, so a generative sweep pays .NET startup once per test binary
    /// rather than once per case.
    ///
    /// `None` when MSBuild rejects the project.
    pub fn items(&mut self, xml: &str, path: &Path, item_type: &str) -> Option<Vec<String>> {
        let resp = self.request(&serde_json::json!({
            "op": "items",
            "xml": xml,
            "path": path.to_string_lossy().into_owned(),
            "itemType": item_type,
        }));
        if resp["ok"].as_bool() != Some(true) {
            return None;
        }
        Some(
            resp["items"]
                .as_array()
                .expect("oracle ok response carries an items array")
                .iter()
                .map(|v| v.as_str().expect("each item is a string").to_string())
                .collect(),
        )
    }

    /// The `items` op enriched for dependency items, where `FullPath` is
    /// meaningless: each evaluated item of `item_type` comes back as its
    /// identity (`EvaluatedInclude` — what `Update`/`Remove` match against) and
    /// the requested `metadata` names' evaluated values (an unset metadatum
    /// reads back as `""`), in evaluation order. `None` when MSBuild rejects the
    /// project. This is what lets a generative sweep diff a `<PackageReference>`
    /// `Include`+`Update` collapse — identity matching and per-key metadata
    /// merge — against the real evaluator, resident.
    pub fn items_meta(
        &mut self,
        xml: &str,
        path: &Path,
        item_type: &str,
        metadata: &[&str],
    ) -> Option<Vec<(String, HashMap<String, String>)>> {
        let resp = self.request(&serde_json::json!({
            "op": "itemsMeta",
            "xml": xml,
            "path": path.to_string_lossy().into_owned(),
            "itemType": item_type,
            "metadata": metadata,
        }));
        if resp["ok"].as_bool() != Some(true) {
            return None;
        }
        Some(
            resp["items"]
                .as_array()
                .expect("oracle ok response carries an items array")
                .iter()
                .map(|item| {
                    let identity = item["identity"]
                        .as_str()
                        .expect("each item carries a string identity")
                        .to_string();
                    let values = item["metadata"]
                        .as_object()
                        .expect("each item carries a metadata object")
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                v.as_str().expect("metadata value is a string").to_string(),
                            )
                        })
                        .collect();
                    (identity, values)
                })
                .collect(),
        )
    }

    pub fn project(
        &mut self,
        xml: &str,
        names: &[String],
        path: Option<&Path>,
    ) -> Option<HashMap<String, String>> {
        let resp = self.request(&serde_json::json!({
            "op": "project",
            "xml": xml,
            "names": names,
            "path": path.map(|p| p.to_string_lossy().into_owned()),
        }));
        if resp["ok"].as_bool() != Some(true) {
            return None;
        }
        let values = resp["values"]
            .as_object()
            .expect("oracle ok response carries a values object");
        Some(
            values
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str()
                            .expect("oracle property value is a string")
                            .to_string(),
                    )
                })
                .collect(),
        )
    }
}

/// Which branch of the certain-implies-exact contract a case exercised — used
/// by the sweeps to assert coverage (that the corpus reaches both committed
/// booleans and isn't silently degrading to all-`Unsupported`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    True,
    False,
    Unsupported,
}

/// The differential contract, asserted for one `(condition, props)` case:
///
/// - We commit to `True`/`False` ⟹ MSBuild agrees with that *exact* boolean
///   (never the opposite, never "illegal"). A violation is a soundness bug:
///   we'd be gating an item on a truth value the real build disagrees with.
/// - We return `Unsupported` ⟹ no claim. MSBuild may say true, false, or
///   reject it; our fail-safe deliberately over-approximates the unmodelled.
///
/// Returns which branch fired so callers can tally coverage.
pub fn check_certain_implies_exact(
    oracle: &mut Oracle,
    condition: &str,
    props: &[(String, String)],
) -> Verdict {
    let mut map = PropertyMap::new();
    for (k, v) in props {
        map.insert(k.clone(), v.clone());
    }
    match evaluate(condition, &map).outcome {
        Outcome::Unsupported => Verdict::Unsupported,
        ours @ (Outcome::True | Outcome::False) => {
            let ours_bool = ours == Outcome::True;
            match oracle.eval(condition, props) {
                Some(theirs) => {
                    assert_eq!(
                        ours_bool, theirs,
                        "certain-implies-exact violated: we say {ours:?} for \
                         condition {condition:?} with props {props:?}, but MSBuild says {theirs}"
                    );
                }
                None => panic!(
                    "certain-implies-exact violated: we say {ours:?} for condition \
                     {condition:?} with props {props:?}, but MSBuild rejects it as illegal"
                ),
            }
            if ours_bool {
                Verdict::True
            } else {
                Verdict::False
            }
        }
    }
}

/// Which branch of the expansion certain-implies-exact contract a case
/// exercised — the property-value analogue of [`Verdict`]. `Exact` means our
/// `substitute` reported zero issues (so it committed to the expanded string);
/// `Partial` means it raised at least one [`Issue`](borzoi_msbuild::test_support::Issue)
/// (undefined reference or unsupported expression), withdrawing any claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpandVerdict {
    Exact,
    Partial,
}

/// The expansion differential contract, asserted for one `(value, props)` case:
///
/// - Our `substitute` reports **zero issues** ⟹ MSBuild expands `value` to the
///   **byte-identical** string (and does not error). A violation is a
///   soundness bug: a property value we'd feed downstream as evaluated fact
///   that the real build computes differently (or rejects).
/// - Our `substitute` reports **any issue** (`Undefined`/`Unsupported`) ⟹ no
///   claim. MSBuild may produce any string or throw; the issue already marks
///   the result partial, the fail-safe superset over both.
///
/// Scope: `value` must not use `@(…)`/`%(…)` (a different, item-typed language
/// our `substitute` passes through untouched *without* an issue — see the
/// plan's D1) nor `%XX` escapes in *plain text outside `$(…)`* (MSBuild
/// unescapes those in the evaluated value while `substitute` passes them
/// through without an issue — the known gap tracked in
/// `docs/compile-item-fidelity-plan.md`); the generators and corner lists
/// honour this. `%XX` *inside* a `$(…)` expression is in scope: the
/// expression evaluator declines it, landing the case in `Partial`.
///
/// To isolate the `$(…)`-expansion layer `substitute` models, the value is
/// anchored between inert `|` sentinels before handing it to MSBuild, and the
/// sentinels are stripped back off its answer. MSBuild extracts a property
/// element's body at the XML layer *before* expansion, and a non-empty
/// all-XML-whitespace body collapses to `""` there (verified against `dotnet
/// msbuild`); the sentinels give every body non-whitespace content, so that
/// pre-expansion layer is always the identity and only expansion is compared.
/// `|` is inert in MSBuild property bodies (not a substitution/item/escape
/// metacharacter), so `|value|` expands to `|` + expand(value) + `|`.
///
/// Returns which branch fired so callers can tally coverage.
pub fn check_expand_certain_implies_exact(
    oracle: &mut Oracle,
    value: &str,
    props: &[(String, String)],
) -> ExpandVerdict {
    let mut map = PropertyMap::new();
    for (k, v) in props {
        map.insert(k.clone(), v.clone());
    }
    let (ours, issues) = substitute(value, &map);
    if !issues.is_empty() {
        return ExpandVerdict::Partial;
    }
    match oracle.expand(&format!("|{value}|"), props) {
        Some(theirs) => {
            let theirs = theirs
                .strip_prefix('|')
                .and_then(|s| s.strip_suffix('|'))
                .expect("sentinels survive MSBuild expansion verbatim");
            assert_eq!(
                ours, theirs,
                "expand certain-implies-exact violated: we expand {value:?} with \
                 props {props:?} to {ours:?}, but MSBuild produces {theirs:?}"
            );
        }
        None => panic!(
            "expand certain-implies-exact violated: we commit to expansion {ours:?} \
             for {value:?} with props {props:?}, but MSBuild errors evaluating it"
        ),
    }
    ExpandVerdict::Exact
}

// ============================================================================
// Deterministic condition-string generation (shared by the sweep test)
// ============================================================================

/// The controlled property-name namespace the generators may *define*. Kept
/// small and disjoint from MSBuild reserved names. Conditions also reference
/// names deliberately *outside* this set (e.g. `$(Undefined)`) to exercise the
/// undefined-expands-to-empty path; [`scrub_oracle_env`] clears the whole child
/// environment, so any such name is unset on the MSBuild side too — matching
/// our Rust side regardless of the ambient shell.
pub const CONTROLLED_PROPERTY_NAMES: &[&str] = &[
    "Configuration",
    "Platform",
    "TargetFramework",
    "Version",
    "Foo",
    "Bar",
    "P0",
    "P1",
];

/// SplitMix64: tiny, deterministic mixing for reproducible input generation.
/// Fixed seeds keep every differential run identical, so a failure reproduces
/// exactly; the *random* exploration lives in the proptest file.
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

/// Scalar operands biased towards MSBuild's fiddly numeric/version/boolean
/// corners: decimals, signed/dot-led doubles, hex, version-shaped dotted
/// numbers, exponent-looking strings (NOT parsed as doubles), the boolean
/// vocabulary, quoted strings (some empty, some holding `$(…)`), and bare
/// property references.
fn gen_operand(rng: &mut SplitMix64) -> String {
    const BARE_NUMERIC: &[&str] = &[
        "0",
        "1",
        "42",
        "-1",
        "+2",
        "3.14",
        "+2.5",
        "-0.5",
        ".5",
        "1.",
        "0x10",
        "0xFF",
        "0xg",
        "2147483647",
        "2147483648",
        "6",
        "6.0",
        "6.0.0.0",
        "1.2",
        "1.2.3",
        "1.2.3.4",
        "10.0",
        "01",
        "1e2",
        "100",
    ];
    const BARE_BOOLISH: &[&str] = &[
        "true", "false", "True", "FALSE", "on", "off", "yes", "no", "On", "Off",
    ];
    const QUOTED_INNER: &[&str] = &[
        "",
        "abc",
        "On",
        "yes",
        "no",
        "6.0",
        "6.0.0.0",
        " 2.5 ",
        "net8.0",
        "1e2",
        "100",
        "$(P0)",
        "$(Foo)",
        "$(Undefined)",
        "x$(P1)y",
        "0x10",
        // Escape-bearing quoted operands (stage E2 of
        // `docs/msbuild-escaped-value-plan.md`): MSBuild unescapes `%XX` at the
        // operand leaf, so `'%74rue'` compares as `true` and `'a%20b'` as `a b`.
        // These used to degrade the whole condition; they are modelled now, and
        // this dimension is what holds that line. A bare `%` stays literal, and
        // an escape composed across a splice boundary is in scope too.
        "%74rue",
        "a%20b",
        "100%",
        "a%zz",
        "%25",
        "%$(P1)",
        "$(P0)%20x",
    ];
    const PROP_REFS: &[&str] = &["$(P0)", "$(P1)", "$(Foo)", "$(Bar)", "$(Undefined)"];

    match rng.below(10) {
        0..=3 => (*rng.pick(BARE_NUMERIC)).to_string(),
        4..=5 => (*rng.pick(BARE_BOOLISH)).to_string(),
        6..=8 => format!("'{}'", rng.pick(QUOTED_INNER)),
        _ => (*rng.pick(PROP_REFS)).to_string(),
    }
}

/// Numeric/version-shaped operands for *relational* comparisons — the operands
/// on which `<`/`<=`/`>`/`>=` are actually defined. Draws include a few
/// non-numeric shapes so the "relational on a non-number is illegal" boundary
/// (where our `Unsupported` must line up with MSBuild's error) gets exercised.
fn gen_relational_operand(rng: &mut SplitMix64) -> String {
    const OPERANDS: &[&str] = &[
        "0",
        "1",
        "42",
        "-1",
        "+2",
        "3.14",
        "+2.5",
        "-0.5",
        "0x10",
        "0xFF",
        "6",
        "6.0",
        "6.0.0.0",
        "1.2",
        "1.2.3",
        "1.2.3.4",
        "10.0",
        "01",
        "2147483647",
        "'6.0'",
        "' 2.5 '",
        "$(P0)",
        "$(Version)",
        "$(Undefined)",
        "abc",
        "0xg",
    ];
    (*rng.pick(OPERANDS)).to_string()
}

/// A comparison `lhs OP rhs` with random surrounding whitespace.
///
/// Biased towards `==`/`!=` (which MSBuild always reduces to a boolean via its
/// string fallback, so they reliably commit on both sides and can't produce a
/// soundness panic) drawing from the full [`gen_operand`] corner set; the
/// relational quarter draws numeric/version operands so it too usually
/// commits, while still probing the illegal-relational boundary.
fn gen_comparison(rng: &mut SplitMix64) -> String {
    let pad = |rng: &mut SplitMix64| -> &'static str {
        match rng.below(4) {
            0 => "",
            1 => "  ",
            _ => " ",
        }
    };
    let (op, lhs, rhs) = if rng.below(4) == 0 {
        const REL: &[&str] = &["<", "<=", ">", ">="];
        (
            *rng.pick(REL),
            gen_relational_operand(rng),
            gen_relational_operand(rng),
        )
    } else {
        let op = if rng.below(2) == 0 { "==" } else { "!=" };
        (op, gen_operand(rng), gen_operand(rng))
    };
    format!("{lhs}{}{op}{}{rhs}", pad(rng), pad(rng))
}

/// A boolean-level atom: a comparison, a bare boolean literal (the
/// `Condition="true"` / `Condition="on"` truthiness idiom, which MSBuild
/// accepts), or a parenthesised subexpression. Bare *non-boolean* operands are
/// deliberately excluded here: `Condition="42"` is illegal in MSBuild, so as a
/// standalone atom they only ever produce `Unsupported` and add no signal.
/// `HasTrailingSlash('…')` over the same operand alphabet.
///
/// This built-in is the reason the dimension exists: it expands its argument
/// into an *item list*, so it splits on `;` and trims each entry — and both of
/// those must happen **before** the escape decode, because an escaped `%3b` is
/// data rather than a separator and an escaped `%20` is data rather than
/// padding. `HasTrailingSlash('foo/%20')` is *false* in MSBuild (the decoded
/// value is `foo/ `, ending in a space), where decode-then-trim commits *true*.
/// A reviewer caught that; generating it is how it stays caught.
fn gen_has_trailing_slash(rng: &mut SplitMix64) -> String {
    const ARGS: &[&str] = &[
        "bin/Debug/",
        "bin/Debug",
        "foo%2f",
        "foo/%20",
        "  foo/  ",
        "a%3bb",
        "a/;",
        ";/",
        "",
        "$(P0)",
        "$(P0)/",
        "$(P1)%2f",
    ];
    format!("HasTrailingSlash('{}')", rng.pick(ARGS))
}

fn gen_bool_atom(rng: &mut SplitMix64, depth: u32) -> String {
    const BOOL_LITERALS: &[&str] = &[
        "true", "false", "True", "FALSE", "on", "off", "yes", "no", "'true'", "'false'", "'on'",
    ];
    match rng.below(12) {
        0..=6 => gen_comparison(rng),
        7..=8 => (*rng.pick(BOOL_LITERALS)).to_string(),
        9..=10 => gen_has_trailing_slash(rng),
        _ if depth == 0 => gen_comparison(rng),
        _ => format!("({})", gen_bool_expr(rng, depth - 1)),
    }
}

/// A full condition expression: atoms joined by `And`/`Or`, with optional `!`
/// negation, bounded by `depth`.
pub fn gen_bool_expr(rng: &mut SplitMix64, depth: u32) -> String {
    let mut expr = if rng.below(4) == 0 {
        format!("!{}", gen_bool_atom(rng, depth))
    } else {
        gen_bool_atom(rng, depth)
    };
    let conjuncts = rng.below(3); // 0, 1, or 2 more terms.
    for _ in 0..conjuncts {
        let op = if rng.below(2) == 0 { "And" } else { "Or" };
        let rhs = if rng.below(4) == 0 {
            format!("!{}", gen_bool_atom(rng, depth))
        } else {
            gen_bool_atom(rng, depth)
        };
        expr = format!("{expr} {op} {rhs}");
    }
    expr
}

/// A random property map over a subset of [`CONTROLLED_PROPERTY_NAMES`], with
/// values drawn from a small pool that overlaps the operand corners (so
/// `'$(Configuration)' == 'Debug'` etc. can actually hit).
pub fn gen_props(rng: &mut SplitMix64) -> Vec<(String, String)> {
    const VALUES: &[&str] = &[
        "Debug", "Release", "net8.0", "6.0", "6.0.0.0", "1", "0", "true", "false", "on", "", "x",
        "100",
        // Escape-bearing property values, so an escape can reach an operand by
        // splice as well as by literal — including a trailing bare `%`, which
        // can compose an escape with whatever the condition text puts after it.
        "a%20b", "%74rue", "100%", "20",
    ];
    let mut props = Vec::new();
    for name in CONTROLLED_PROPERTY_NAMES {
        // Roughly half the names get defined per case; the rest stay
        // undefined so the "undefined reference expands to empty" path is
        // exercised on both sides.
        if rng.below(2) == 0 {
            props.push(((*name).to_string(), (*rng.pick(VALUES)).to_string()));
        }
    }
    props
}

// ============================================================================
// Deterministic property-value (`$(…)` expansion) generation
// ============================================================================

/// A property map for the expansion sweep: values skew towards the
/// version/TFM/word shapes the *currently supported* property functions
/// (`GetTargetFramework*`, `Contains`/`StartsWith`/`EndsWith`, `TrimStart`)
/// actually consume, so a generated call on a defined receiver commits often
/// enough to keep the `Exact` coverage floor healthy.
pub fn gen_expand_props(rng: &mut SplitMix64) -> Vec<(String, String)> {
    const VALUES: &[&str] = &[
        "net8.0",
        "net472",
        "netstandard2.0",
        "8.0.0",
        "1.2.3",
        "10.1.300",
        "v8.0",
        "Debug",
        "abc",
        "",
        "x/y/z",
        // Leading/trailing whitespace so empty-`TrimStart` and whitespace-
        // sensitive members diverge visibly if we ever mishandle them.
        "  abc",
        "abc  ",
        // Adversarial values: quote characters of every delimiter kind
        // (inert in values, but they flow into method receivers and
        // spliced arguments) and non-ASCII including a non-BMP scalar
        // (UTF-16-unit semantics: `Length`/indexers must decline, not
        // commit a char count).
        "a`b",
        "a'b",
        "a\"b",
        "caf\u{e9}",
        "o\u{17f}x",
        "\u{1d11e}x",
        // `%XX` escapes, and a bare `%`, in property values — now that the
        // escaped-value domain models them (E1–E3, `docs/msbuild-escaped-value-plan.md`).
        // A bare `$(P)` splice unescapes at the leaf; a splice into a function
        // receiver/argument unescapes once at the call; a trailing bare `%`
        // composes with a hex-leading following segment (`$(P)abc` with `P=100%`
        // → `%ab` decoded). All of these used to be excluded as the "plain-text
        // gap"; the sweep now holds them to certain-implies-exact.
        "a%20b",
        "a%2fb",
        "100%",
        "%2520",
        "%74",
        // Escapes that decode to characters whose downstream handling E3 does
        // *not* model — a backslash (unix path fixup), a C0 control (culture
        // comparison), NUL (`GetFullPath` throws). These must land the reducing
        // expressions in `Partial` (a decline), never a wrong commit; the sweep
        // is what proves that class stays closed, rather than a reviewer finding
        // one consumer at a time. See `docs/msbuild-escaped-value-plan.md` (E3).
        "a%5cb",
        "%5cabc",
        "%01a",
        "a%00b",
        "%7f",
    ];
    let mut props = Vec::new();
    for name in CONTROLLED_PROPERTY_NAMES {
        if rng.below(2) == 0 {
            props.push(((*name).to_string(), (*rng.pick(VALUES)).to_string()));
        }
    }
    props
}

/// One segment of a generated property value: literal text, a property
/// reference (defined-or-undefined), or a call to one of *today's* supported
/// property functions. Excludes `@(…)`/`%(…)` (item/metadata references, a
/// different subsystem); `%XX` escapes are now **in** the space — the
/// escaped-value domain (E1–E3) models them at both the plain-splice layer and
/// inside expression arguments, so the sweep holds them to
/// certain-implies-exact.
fn gen_expand_segment(rng: &mut SplitMix64) -> String {
    // Literal chunks, including `%XX` escapes and a bare `%`: both sides store
    // escaped and unescape at the point of use, so a body's `%20` is a space and
    // a trailing `%` composes with a following hex-leading segment.
    const LITERALS: &[&str] = &[
        "", "a", "abc", "/", "x/y", "-", ".", " ", "TRACE", "1.0", "net", "lib/", "%20", "%2f",
        "a%2fb", "100%",
        // Decode-to-special-char escapes (backslash / control / NUL): a reducing
        // expression over these must decline, never wrong-commit (E3).
        "a%5cb", "%01a", "a%00b", "%7f",
    ];
    // Needles include escape-bearing and non-ASCII spellings: inside a
    // quoted expression argument these must decline (never commit a
    // comparison against raw text MSBuild would unescape).
    const NEEDLES: &[&str] = &[
        "",
        "8",
        "net",
        ".",
        "-",
        "v",
        "abc",
        "0",
        "%20",
        "a%zb",
        "caf\u{e9}",
        // Decode-to-special-char needles: culture-sensitive comparison and the
        // path fixup diverge on these, so the call must decline (E3).
        "%01",
        "a%5cb",
        "%00",
    ];
    const TFMS: &[&str] = &[
        "net8.0",
        "netstandard2.0",
        "net472",
        "netcoreapp3.1",
        "$(TargetFramework)",
    ];
    // Platform names for `IsOSPlatform`: real ones in assorted casings,
    // unknown names, the empty string (MSBuild errors), an escaped spelling,
    // and the non-ASCII `oſx` whose *invariant* uppercase is `OSX`.
    const PLATFORMS: &[&str] = &[
        "osx",
        "OSX",
        "macos",
        "linux",
        "windows",
        "Windows",
        "freebsd",
        "darwin",
        "banana",
        "",
        "%6fSX",
        "o\u{17f}x",
    ];
    // Paths for `IsPathRooted` / `EnsureTrailingSlash`: rooted/unrooted,
    // backslash-led (MSBuild-level rooting), whitespace-led (defeats
    // rooting), escaped, and bare-`%` spellings.
    const PATHS: &[&str] = &[
        "/a/b", "a/b", "", "\\a", " /a", "a/b/", "%2fabc", "a%20b", "a%zb", "100%",
    ];
    let delim = *rng.pick(&['\'', '`', '"']);
    match rng.below(12) {
        0..=5 => (*rng.pick(LITERALS)).to_string(),
        6..=7 => format!("$({})", rng.pick(CONTROLLED_PROPERTY_NAMES)),
        8 => {
            let name = *rng.pick(CONTROLLED_PROPERTY_NAMES);
            let method = *rng.pick(&["Contains", "StartsWith", "EndsWith"]);
            format!(
                "$({}.{}({delim}{}{delim}))",
                name,
                method,
                rng.pick(NEEDLES)
            )
        }
        9 => {
            let func = *rng.pick(&["GetTargetFrameworkIdentifier", "GetTargetFrameworkVersion"]);
            format!("$([MSBuild]::{func}({delim}{}{delim}))", rng.pick(TFMS))
        }
        10 => format!(
            "$([MSBuild]::IsOSPlatform({delim}{}{delim}))",
            rng.pick(PLATFORMS)
        ),
        _ => {
            let (ty, func) = *rng.pick(&[
                ("System.IO.Path", "IsPathRooted"),
                ("MSBuild", "EnsureTrailingSlash"),
            ]);
            format!("$([{ty}]::{func}({delim}{}{delim}))", rng.pick(PATHS))
        }
    }
}

/// A generated property value: 1–4 concatenated [`gen_expand_segment`]s, the
/// shape a real `<Prop>…</Prop>` body takes (literal text interleaved with
/// `$(…)` expansions).
pub fn gen_expand_value(rng: &mut SplitMix64) -> String {
    let count = 1 + rng.below(4);
    (0..count).map(|_| gen_expand_segment(rng)).collect()
}

/// A structural, parser-stressing `$(…)` expression: static calls, instance
/// chains, indexers, and nested-quote arguments (the shapes the general
/// parser's scanner and dispatch must handle). Member/function pools mix
/// supported and unsupported names, so many results are `Partial` today (the
/// Stage-3 evaluators for `Split`/indexers/`Version`/… aren't in yet) — the
/// point is that the parser finds the right extent and *never over-commits*.
/// The same generator turns those `Partial`s `Exact` once Stage 3's dispatch
/// lands, still under certain-implies-exact.
pub fn gen_grammar_value(rng: &mut SplitMix64) -> String {
    format!("$({})", gen_grammar_inner(rng, 2))
}

fn gen_grammar_inner(rng: &mut SplitMix64, depth: u32) -> String {
    let mut s = gen_grammar_root(rng, depth);
    for _ in 0..rng.below(3) {
        s.push_str(&gen_grammar_link(rng, depth));
    }
    s
}

fn gen_grammar_root(rng: &mut SplitMix64, depth: u32) -> String {
    // Property receiver (defined-or-undefined), or a static call.
    if rng.below(3) == 0 {
        return (*rng.pick(CONTROLLED_PROPERTY_NAMES)).to_string();
    }
    const TYPES: &[&str] = &[
        "MSBuild",
        "System.Version",
        "System.IO.Path",
        "System.String",
    ];
    const STATICS: &[&str] = &[
        "Parse",
        "Combine",
        "GetTargetFrameworkVersion",
        "GetTargetFrameworkIdentifier",
        "EnsureTrailingSlash",
        "IsRunningFromVisualStudio",
        "IsOSPlatform",
        "IsPathRooted",
        "AreFeaturesEnabled",
        "NormalizePath",
        "Nope",
    ];
    format!(
        "[{}]::{}({})",
        rng.pick(TYPES),
        rng.pick(STATICS),
        gen_grammar_args(rng, depth)
    )
}

fn gen_grammar_link(rng: &mut SplitMix64, depth: u32) -> String {
    const PARENLESS: &[&str] = &["Major", "Minor", "Build", "Length", "Bogus"];
    const METHODS: &[&str] = &[
        "Contains",
        "StartsWith",
        "TrimStart",
        "Split",
        "Substring",
        "ToString",
        "Nope",
    ];
    match rng.below(5) {
        0 => format!("[{}]", rng.below(4)),
        1 => format!(".{}", rng.pick(PARENLESS)),
        _ => format!(".{}({})", rng.pick(METHODS), gen_grammar_args(rng, depth)),
    }
}

fn gen_grammar_args(rng: &mut SplitMix64, depth: u32) -> String {
    if depth == 0 {
        return String::new();
    }
    // Occasionally a whitespace-only arg list (`Func( )`) — distinct from
    // `Func()` (empty), the boundary MSBuild is picky about (a zero-arg
    // intrinsic rejects `( )`).
    if rng.below(8) == 0 {
        return " ".to_string();
    }
    (0..rng.below(3))
        .map(|_| gen_grammar_arg(rng, depth - 1))
        .collect::<Vec<_>>()
        .join(",")
}

fn gen_grammar_arg(rng: &mut SplitMix64, depth: u32) -> String {
    match rng.below(5) {
        0 => rng.below(10).to_string(),
        1 => format!("$({})", gen_grammar_inner(rng, depth)),
        // Quoted literal, whose body may itself be a nested `$(…)` carrying its
        // own quotes (`'$(X.Split('-')[0])'`) — the scanner's hard case. All
        // three MSBuild delimiters are generated; a literal that happens to
        // contain the chosen delimiter is a *deliberately malformed* shape
        // (both sides must reject or agree — we must never over-commit).
        _ => {
            let delim = *rng.pick(&['\'', '`', '"']);
            format!("{delim}{}{delim}", gen_grammar_arg_literal(rng, depth))
        }
    }
}

fn gen_grammar_arg_literal(rng: &mut SplitMix64, depth: u32) -> String {
    // Version/path/separator-shaped literals so the Stage-3 evaluators
    // (`Version::Parse`, `EnsureTrailingSlash`, `Split` char-sets) actually
    // reduce for a healthy fraction of generated calls, rather than only
    // exercising the declining direction. The adversarial tail exists to
    // *hunt* for input-language semantics we don't model: `%XX` escapes
    // (MSBuild unescapes before functions run — the evaluator must decline,
    // never commit raw text), a bare `%` that stays literal, quote characters
    // of every delimiter kind, platform names (real, wrong-cased, and the
    // non-ASCII `oſx` whose invariant uppercase hits `OSX`), and non-ASCII
    // text generally. Never bake the evaluator's own accepted alphabet in
    // here — the sweep can only find what the generator can spell.
    const LITS: &[&str] = &[
        "",
        "-",
        ".",
        "x",
        "8",
        "net",
        "v",
        "-_",
        "1.2",
        "10.1.300",
        "1.2.3.4",
        "10",
        "/a/b",
        "/a/b/",
        "a\\b",
        "%20",
        "a%20b",
        "%2fa",
        "a%zb",
        "100%",
        "osx",
        "OSX",
        "Windows",
        "o\u{17f}x",
        "caf\u{e9}",
        "a'b",
        "a`b",
        "a\"b",
        "17.10",
        "999.999",
    ];
    if depth > 0 && rng.below(2) == 0 {
        format!("$({})", gen_grammar_inner(rng, depth))
    } else {
        (*rng.pick(LITS)).to_string()
    }
}

//! Driver for `tools/inheritdoc-oracle`: Roslyn's IDE `<inheritdoc>`
//! expansion, its XPath selection, and a C# compiler for purpose-built
//! fixtures, as a pool of resident JSONL children ([`OraclePool`]). Built once
//! per source fingerprint (the content-bearing-marker scheme the other oracle
//! harnesses use).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LockResult, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use borzoi_oracle_harness::BatchChild;
use borzoi_spawn::BoundedCommand;

/// Budget for the oracle's `dotnet build` (a cold restore plus a compile);
/// it stops a stalled build, not a slow one.
const BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// Budget for one request: building a compilation over a whole targeting
/// pack and expanding hundreds of entries is seconds, not minutes.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root parent")
        .to_path_buf()
}

fn ensure_built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            if let Some(bin) = std::env::var_os("BORZOI_INHERITDOC_ORACLE") {
                return PathBuf::from(bin);
            }
            let project = workspace_root().join("tools").join("inheritdoc-oracle");
            let bin = project.join("bin");
            let apphost = bin
                .join("Release")
                .join("net10.0")
                .join("inheritdoc-oracle");
            let marker = bin.join(".inheritdoc-oracle-built");
            let want = format!("{:016x}", fingerprint(&project));
            let fresh = apphost.exists()
                && std::fs::read_to_string(&marker).is_ok_and(|r| r.trim() == want);
            if !fresh {
                let mut cmd = Command::new("dotnet");
                cmd.args(["build", "-c", "Release", "--nologo"])
                    .arg(&project);
                BoundedCommand::new(cmd)
                    .timeout(BUILD_TIMEOUT)
                    .run_ok("dotnet build inheritdoc-oracle");
                assert!(apphost.exists(), "no apphost at {apphost:?}");
                let tmp = bin.join(format!(
                    ".inheritdoc-oracle-built.tmp-{}",
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

/// The oracle's sources plus `flake.lock` (which pins the SDK and the
/// offline package set), names hashed before contents.
fn fingerprint(project: &Path) -> u64 {
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
                Some("cs" | "csproj")
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

/// Roslyn's answer for one documentation ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expanded {
    /// The symbol's expanded documentation (`FullXmlFragment`), and the
    /// candidate rule of each `<inheritdoc>` of its own entry.
    Ok {
        xml: String,
        rules: Vec<(String, Option<String>)>,
    },
    /// No symbol of that ID in the assembly.
    NoSymbol,
    /// Several.
    Ambiguous,
}

/// One resident oracle child.
pub struct Oracle {
    child: BatchChild,
}

/// The resident oracle children: [`OraclePool::lock`] hands each thread the
/// same child every time.
///
/// A child serves one request at a time (its requests and responses are matched
/// positionally), and every `<inheritdoc>` case is a compilation and an
/// expansion on it, so one child for the whole binary serialises every case
/// group on it. A pool lets them run side by side.
///
/// **A thread keeps its child.** A child caches each DLL it has read until a
/// request of *its own* writes that path (`Program.Rewritten` in the oracle),
/// so a path written through one child and then read through another could be
/// read stale. libtest runs each test on a thread of its own, so pinning the
/// thread keeps a test's write-then-read sequences on one child; what tests
/// share is written once (behind a `OnceLock`) before anyone reads it.
///
/// A thread claims the child with the fewest live claimants, and gives the
/// claim back when it exits. Whether a child is *locked* says nothing about
/// whether it is taken: a thread holds its child's lock only for a request,
/// and keeps the child between requests.
pub struct OraclePool {
    slots: Vec<Mutex<Oracle>>,
    /// Per child, how many live threads have claimed it.
    claims: Mutex<Vec<usize>>,
}

/// A thread's claim on one child of a pool, given back when the thread exits.
struct Claim {
    pool: &'static OraclePool,
    slot: usize,
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut claims = self
            .pool
            .claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        claims[self.slot] -= 1;
    }
}

thread_local! {
    static CLAIM: RefCell<Option<Claim>> = const { RefCell::new(None) };
}

/// How many children the pool may hold: one per core, at most four by default
/// (each holds a compilation over a whole reference pack), overridden by
/// `BORZOI_INHERITDOC_CHILDREN`.
fn pool_size() -> usize {
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::env::var("BORZOI_INHERITDOC_CHILDREN")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| cores.min(4))
}

/// The pool, one per test binary. Its children spawn on their first request.
pub fn oracle() -> &'static OraclePool {
    static POOL: OnceLock<OraclePool> = OnceLock::new();
    POOL.get_or_init(|| {
        let bin = ensure_built().to_path_buf();
        OraclePool::new(pool_size(), move || Command::new(&bin))
    })
}

impl OraclePool {
    /// `n` children, each spawned by `make` on its first request.
    fn new(n: usize, make: impl Fn() -> Command + Clone + Send + 'static) -> Self {
        OraclePool {
            slots: (0..n)
                .map(|_| {
                    let make = make.clone();
                    Mutex::new(Oracle {
                        child: BatchChild::with_factory(
                            Box::new(make),
                            "inheritdoc-oracle",
                            REQUEST_TIMEOUT,
                            2,
                        ),
                    })
                })
                .collect(),
            claims: Mutex::new(vec![0; n]),
        }
    }

    /// Claim the child with the fewest live claimants (the lowest of equals).
    fn claim(&'static self) -> Claim {
        let mut claims = self
            .claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = (0..claims.len())
            .min_by_key(|&i| claims[i])
            .expect("a pool has a child");
        claims[slot] += 1;
        Claim { pool: self, slot }
    }

    /// This thread's child, locked: the one it claimed on its first call.
    pub fn lock(&'static self) -> LockResult<MutexGuard<'static, Oracle>> {
        let slot = CLAIM.with(|cell| {
            let mut claim = cell.borrow_mut();
            match &*claim {
                Some(c) if std::ptr::eq(c.pool, self) => c.slot,
                _ => {
                    let c = self.claim();
                    let slot = c.slot;
                    *claim = Some(c);
                    slot
                }
            }
        });
        self.slots[slot].lock()
    }
}

impl Oracle {
    fn request(&mut self, req: &serde_json::Value) -> serde_json::Value {
        let line = serde_json::to_string(req).expect("serialise request");
        let response = self.child.request(&line);
        let value: serde_json::Value =
            serde_json::from_str(&response).expect("inheritdoc-oracle response is JSON");
        if let Some(err) = value.get("error") {
            panic!(
                "inheritdoc-oracle errored on {}: {err}",
                &line[..line.len().min(400)]
            );
        }
        value
    }

    /// Compile `source` against `references` into `out_dir/<name>.dll` with
    /// its documentation file beside it; the compiler's errors otherwise.
    pub fn compile(
        &mut self,
        source: &str,
        name: &str,
        out_dir: &Path,
        references: &[PathBuf],
    ) -> Result<PathBuf, Vec<String>> {
        let plain: Vec<(PathBuf, Option<&str>)> =
            references.iter().map(|r| (r.clone(), None)).collect();
        self.compile_aliased(source, name, out_dir, &plain)
    }

    /// [`Self::compile`], with some references reached through an
    /// `extern alias`.
    pub fn compile_aliased(
        &mut self,
        source: &str,
        name: &str,
        out_dir: &Path,
        references: &[(PathBuf, Option<&str>)],
    ) -> Result<PathBuf, Vec<String>> {
        self.compile_request(source, name, out_dir, references, None)
    }

    /// [`Self::compile`], public-signed with `key_from`'s public key: a
    /// strong-named assembly whose references carry that key's token.
    pub fn compile_signed(
        &mut self,
        source: &str,
        name: &str,
        out_dir: &Path,
        references: &[PathBuf],
        key_from: &Path,
    ) -> Result<PathBuf, Vec<String>> {
        let plain: Vec<(PathBuf, Option<&str>)> =
            references.iter().map(|r| (r.clone(), None)).collect();
        self.compile_request(source, name, out_dir, &plain, Some(key_from))
    }

    fn compile_request(
        &mut self,
        source: &str,
        name: &str,
        out_dir: &Path,
        references: &[(PathBuf, Option<&str>)],
        key_from: Option<&Path>,
    ) -> Result<PathBuf, Vec<String>> {
        let references: Vec<serde_json::Value> = references
            .iter()
            .map(|(path, alias)| match alias {
                Some(alias) => serde_json::json!({ "path": path, "alias": alias }),
                None => serde_json::json!(path),
            })
            .collect();
        let mut request = serde_json::json!({
            "op": "compile",
            "source": source,
            "assemblyName": name,
            "outDir": out_dir,
            "references": references,
        });
        if let Some(key_from) = key_from {
            request["publicKeyFrom"] = key_from.to_string_lossy().into_owned().into();
        }
        let r = self.request(&request);
        if r["ok"].as_bool() == Some(true) {
            Ok(out_dir.join(format!("{name}.dll")))
        } else {
            Err(r["diagnostics"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| d.as_str().unwrap_or("").to_string())
                .collect())
        }
    }

    /// Roslyn's expansion of each of `ids` in the assembly read from
    /// `assembly`, over a compilation referencing exactly `references`.
    pub fn expand(
        &mut self,
        references: &[PathBuf],
        assembly: &Path,
        ids: &[String],
    ) -> Vec<Expanded> {
        let r = self.request(&serde_json::json!({
            "op": "expand",
            "references": references,
            "assembly": assembly,
            "ids": ids,
        }));
        r["results"]
            .as_array()
            .expect("results")
            .iter()
            .map(|res| match res["status"].as_str() {
                Some("ok") => Expanded::Ok {
                    xml: res["xml"].as_str().expect("xml").to_string(),
                    rules: res["rules"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|r| {
                            (
                                r["rule"].as_str().unwrap_or("").to_string(),
                                r["candidate"].as_str().map(str::to_string),
                            )
                        })
                        .collect(),
                },
                Some("no-symbol") => Expanded::NoSymbol,
                Some("ambiguous") => Expanded::Ambiguous,
                other => panic!("unexpected status {other:?}"),
            })
            .collect()
    }

    /// The nodes Roslyn's XPath selects from `xml`, wrapped in one
    /// `<selection>` element; `None` where Roslyn's selection fails.
    /// Exchange, in the DLL at `assembly`, the names of its two methods
    /// named `a` and `b` (their `MethodDef` rows' `Name` columns).
    pub fn swap_method_names(&mut self, assembly: &Path, a: &str, b: &str) {
        self.request(&serde_json::json!({
            "op": "swap-method-names",
            "assembly": assembly,
            "a": a,
            "b": b,
        }));
    }

    /// Rewrite the `AssemblyRef` row of `assembly` named `name` in place: its
    /// version, its public-key token's bytes, or its token cleared.
    pub fn patch_assembly_ref(&mut self, assembly: &Path, name: &str, patch: &AssemblyRefPatch) {
        let mut request = serde_json::json!({
            "op": "patch-assembly-ref",
            "assembly": assembly,
            "name": name,
        });
        if let Some(version) = patch.version {
            request["version"] = version.into();
        }
        if let Some(token) = patch.token {
            request["token"] = token.into();
        }
        if patch.drop_token {
            request["dropToken"] = true.into();
        }
        self.request(&request);
    }

    /// Write an assembly from a description (the oracle's `emit` op, see
    /// `tools/inheritdoc-oracle/Emit.cs`); `request` carries everything
    /// but the op.
    pub fn emit(&mut self, mut request: serde_json::Value) {
        request["op"] = serde_json::json!("emit");
        self.request(&request);
    }

    pub fn xpath(&mut self, xml: &str, path: &str) -> Option<String> {
        let r = self.request(&serde_json::json!({ "op": "xpath", "xml": xml, "path": path }));
        (r["ok"].as_bool() == Some(true))
            .then(|| r["selection"].as_str().expect("selection").to_string())
    }
}

/// A rewrite of one `AssemblyRef` row ([`Oracle::patch_assembly_ref`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct AssemblyRefPatch {
    /// The version to write, `a.b.c.d`.
    pub version: Option<&'static str>,
    /// The token to write over the row's own, as 16 hex digits.
    pub token: Option<&'static str>,
    /// Whether to clear the token to the empty blob.
    pub drop_token: bool,
}

/// Threads that ask the pool at the same time get different children, even
/// when each has finished its first request before the next asks: a thread
/// keeps its child between requests, so a child that is merely unlocked at
/// that moment may still be another live thread's.
#[test]
fn concurrent_threads_are_spread_over_every_child() {
    // A pool of its own, whose children are never spawned: nothing here sends
    // a request.
    let pool: &'static OraclePool = Box::leak(Box::new(OraclePool::new(4, || {
        Command::new("borzoi-test-never-spawned")
    })));
    let n = pool.slots.len();
    let barrier = std::sync::Barrier::new(n);
    let children: std::collections::BTreeSet<usize> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..n)
            .map(|_| {
                scope.spawn(|| {
                    // Each thread takes its child and lets go before the next
                    // one asks, as a test between two requests does.
                    let child = {
                        let guard = pool.lock().expect("an unpoisoned child");
                        std::ptr::from_ref::<Oracle>(&guard) as usize
                    };
                    barrier.wait();
                    child
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a pool thread"))
            .collect()
    });
    assert_eq!(
        children.len(),
        n,
        "{n} live threads share {} of {n} children",
        children.len()
    );
}

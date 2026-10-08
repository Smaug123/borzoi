//! Driver for `tools/inheritdoc-oracle`: Roslyn's IDE `<inheritdoc>`
//! expansion, its XPath selection, and a C# compiler for purpose-built
//! fixtures, as one resident JSONL child. Built once per source fingerprint
//! (the content-bearing-marker scheme the other oracle harnesses use).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
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

/// The resident oracle child.
pub struct Oracle {
    child: BatchChild,
}

/// One oracle per test binary, behind a mutex (its requests and responses are
/// matched positionally).
pub fn oracle() -> &'static Mutex<Oracle> {
    static ORACLE: OnceLock<Mutex<Oracle>> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let bin = ensure_built().to_path_buf();
        let factory = move || Command::new(&bin);
        Mutex::new(Oracle {
            child: BatchChild::with_factory(
                Box::new(factory),
                "inheritdoc-oracle",
                REQUEST_TIMEOUT,
                2,
            ),
        })
    })
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
        let references: Vec<serde_json::Value> = references
            .iter()
            .map(|(path, alias)| match alias {
                Some(alias) => serde_json::json!({ "path": path, "alias": alias }),
                None => serde_json::json!(path),
            })
            .collect();
        let r = self.request(&serde_json::json!({
            "op": "compile",
            "source": source,
            "assemblyName": name,
            "outDir": out_dir,
            "references": references,
        }));
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

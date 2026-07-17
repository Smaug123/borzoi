//! Differential test: the *environment* input model against `dotnet msbuild`.
//!
//! MSBuild folds the environment in as initial properties, but not verbatim:
//! it filters names (reserved, non-XML-name), overwrites some after promotion
//! (toolset-computed), treats values as *escaped-domain* text (so a `%XX` in
//! one is an escape, unescaped at the point of use), and resolves case in a way
//! that is host-dependent. Each of those is a chance to seed our property
//! table with a value the real build never has — and a seeded property is
//! *committed*, so a wrong seed is a wrong answer rather than a decline.
//!
//! Every environment-model bug found by review so far has been a name or value
//! class nobody thought to write a unit test for (`MSBuildThisFileFullPath`
//! spoofing, `1FOO=bar`, `FOO=%54rue`, a colliding `OS`/`os` pair). Enumerating
//! those classes by hand is exactly the losing game, so this test states the
//! contract once and hands the enumeration to the oracle.
//!
//! ## The contract: certain-implies-exact
//!
//! For each probed name, the walker either **commits** a value or **declines**
//! (an `UndefinedProperty` / `UnsupportedPropertyExpression` diagnostic naming
//! it). Declining is always allowed — consumers degrade. Committing is a claim,
//! and the claim must be MSBuild's value, byte for byte.
//!
//! A test that only asserted the implication could pass by declining
//! everything, so `commits_the_ordinary_cases` separately pins the names that
//! must stay *certain*: the point of C.2a is that ordinary environment
//! variables become readable, and a regression to blanket-declining would be a
//! silent loss of exactness.
//!
//! ## How the two sides are kept honest
//!
//! MSBuild sees the child's whole environment, so both sides must see the same
//! set: the child runs under `scrub` (PATH/HOME/TMPDIR + `DOTNET_*`/`NUGET_*`)
//! plus the case's extra variables, and our evaluator gets
//! `common::oracle_environment()` — the same scrub — plus the same extras.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use borzoi_msbuild::{DiagnosticKind, parse_fsproj};
use borzoi_oracle_harness::BoundedCommand;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

const MSBUILD_TIMEOUT: Duration = Duration::from_secs(600);

/// The names each case probes. Chosen to span the classes MSBuild treats
/// specially, *including* the ones we expect to decline — a declined name is
/// still worth probing, because the failure mode we are guarding against is
/// committing where MSBuild disagrees.
const PROBES: &[&str] = &[
    // Ordinary, promotable.
    "FOO",
    "MY_VAR",
    // Non-XML-name starts: Unix exports them, MSBuild refuses to promote them.
    "1FOO",
    // Reserved: MSBuild filters these out of the environment entirely.
    "MSBuildProjectName",
    "MSBuildThisFileFullPath",
    "MSBuildThisFileName",
    "MSBuildProjectDirectoryNoRoot",
    "MSBuildInteractive",
    "MSBuildLastTaskResult",
    "MSBuildProjectDefaultTargets",
    // Toolset-computed: MSBuild overwrites these *after* promotion.
    "MSBuildToolsPath",
    "MSBuildRuntimeType",
    // Deliberately promotable despite the MSBuild-ish name.
    "VisualStudioVersion",
    // Promoted, but whether the promoted value then *survives* depends on the
    // toolset: MSBuild ≤ 17 (SDK 8, 9) overwrites it with the toolset's own
    // directory, MSBuild 18 (SDK 10) leaves it standing. These cases evaluate
    // through `parse_fsproj`, where no SDK resolves and so no toolset is
    // known — the walker declines, and the oracle (whichever SDK the devshell
    // pins) cannot pull it into a commit. `nuget_props_chain.rs` pins both
    // toolset branches against a canonical fake SDK.
    "MSBuildExtensionsPath",
    // The pseudo-environment property, and the ChangeWaves threshold.
    "OS",
    "MSBuildDisableFeaturesFromVersion",
    // Rewritten by the `dotnet` host itself before MSBuild loads, so the
    // inherited value is not what the evaluation sees.
    "DOTNET_HOST_PATH",
];

/// One differential case: a set of extra environment variables.
struct Case {
    name: &'static str,
    env: &'static [(&'static str, &'static str)],
}

const CASES: &[Case] = &[
    Case {
        name: "empty",
        env: &[],
    },
    Case {
        name: "ordinary values",
        env: &[("FOO", "bar"), ("MY_VAR", "some value")],
    },
    Case {
        name: "empty value",
        env: &[("FOO", "")],
    },
    // MSBuild stores env values in its escaped domain and unescapes them at the
    // point of use, so `%54` becomes `T`. Since E1 the walker models that
    // domain, so these must now *commit* MSBuild's unescaped value rather than
    // decline.
    Case {
        name: "%XX escape in value",
        env: &[("FOO", "%54rue"), ("MY_VAR", "a%20b")],
    },
    // Not a valid XML element name: MSBuild never promotes it.
    Case {
        name: "leading-digit name",
        env: &[("1FOO", "bar")],
    },
    // Reserved names: a spoof must not displace the real (or absent) value.
    Case {
        name: "reserved-name spoofs",
        env: &[
            ("MSBuildProjectName", "Spoofed"),
            ("MSBuildThisFileFullPath", "/spoof/x.props"),
            ("MSBuildThisFileName", "spoof"),
            ("MSBuildProjectDirectoryNoRoot", "spoof"),
            ("MSBuildInteractive", "true"),
            ("MSBuildLastTaskResult", "false"),
            ("MSBuildProjectDefaultTargets", "Spoofed"),
        ],
    },
    // Toolset-computed names: MSBuild overwrites them after promotion.
    Case {
        name: "toolset-name spoofs",
        env: &[
            ("MSBuildToolsPath", "/spoof/tools"),
            ("MSBuildRuntimeType", "Spoofed"),
        ],
    },
    // These MSBuild deliberately leaves overridable from the environment.
    Case {
        name: "promotable msbuild-ish names",
        env: &[
            ("MSBuildExtensionsPath", "/spoof/ext"),
            ("VisualStudioVersion", "17.0"),
        ],
    },
    // The faked non-Windows `OS` default must lose to a genuine variable.
    Case {
        name: "OS override",
        env: &[("OS", "Windows_NT")],
    },
    // …and a case-colliding pair makes MSBuild's pick unspecified, so the
    // seeded default must not survive either.
    Case {
        name: "OS case collision",
        env: &[("OS", "Windows_NT"), ("os", "lowercase")],
    },
    Case {
        name: "ordinary case collision",
        env: &[("FOO", "upper"), ("foo", "lower")],
    },
    // The `dotnet` host overwrites this before MSBuild evaluates, so an
    // inherited value must not be promoted.
    Case {
        name: "dotnet-host-rewritten name",
        env: &[("DOTNET_HOST_PATH", "/spoof/dotnet")],
    },
    // ChangeWaves: unset (sentinel), set (rotation-clamped), and a
    // wrong-case spelling that Unix ignores.
    Case {
        name: "changewaves set",
        env: &[("MSBUILDDISABLEFEATURESFROMVERSION", "17.4")],
    },
    Case {
        name: "changewaves lowercase spelling",
        env: &[("msbuilddisablefeaturesfromversion", "17.4")],
    },
];

/// The probe project: read each name into `R_<i>`.
fn project_source() -> String {
    let mut s = String::from("<Project>\n  <PropertyGroup>\n");
    for (i, name) in PROBES.iter().enumerate() {
        s.push_str(&format!("    <R_{i}>$({name})</R_{i}>\n"));
    }
    s.push_str("  </PropertyGroup>\n  <Target Name=\"B\" />\n</Project>\n");
    s
}

fn scrub(cmd: &mut Command) {
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

/// MSBuild's value for each probe, under `extra`.
fn run_msbuild(proj: &Path, extra: &[(&str, &str)]) -> HashMap<String, String> {
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    scrub(&mut cmd);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.args(["msbuild", "-nologo"]);
    for i in 0..PROBES.len() {
        cmd.arg(format!("-getProperty:R_{i}"));
    }
    cmd.arg(proj);

    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok("dotnet msbuild -getProperty");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // `-getProperty:` with one name prints the bare value; with several it
    // prints a JSON document. We always pass several.
    let doc: serde_json_lite::Value = serde_json_lite::parse(&stdout)
        .unwrap_or_else(|e| panic!("parse -getProperty JSON ({e}):\n{stdout}"));
    doc.properties()
}

/// What the walker said about one probe.
struct Ours {
    properties: HashMap<String, String>,
    /// Names read as undefined — matched *exactly*: a substring match would let
    /// a declined `FOOBAR` silently suppress the comparison for `FOO`, which is
    /// how a real divergence would slip past this test.
    undefined: Vec<String>,
    /// Expression texts we could not evaluate; the probed name is matched as a
    /// substring here, because the expression embeds it.
    unsupported: Vec<String>,
}

impl Ours {
    fn declined(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.undefined.contains(&lower) || self.unsupported.iter().any(|e| e.contains(&lower))
    }
}

fn run_ours(source: &str, proj: &Path, environment: &HashMap<String, String>) -> Ours {
    let p = parse_fsproj(source, proj, &HashMap::new(), environment).expect("well-formed XML");

    let mut undefined = Vec::new();
    let mut unsupported = Vec::new();
    for d in &p.diagnostics {
        match &d.kind {
            DiagnosticKind::UndefinedProperty { name } => undefined.push(name.to_ascii_lowercase()),
            DiagnosticKind::UnsupportedPropertyExpression { expression } => {
                unsupported.push(expression.to_ascii_lowercase())
            }
            _ => {}
        }
    }
    Ours {
        properties: p.properties.clone(),
        undefined,
        unsupported,
    }
}

/// Check one environment snapshot against MSBuild, returning the reads that
/// diverged and how many we committed.
///
/// This *is* the contract: for every probed name the walker either declines
/// (fine — consumers degrade) or commits, and a commit must be MSBuild's value.
fn check_snapshot(source: &str, proj: &Path, extra: &[(String, String)]) -> (Vec<String>, usize) {
    let borrowed: Vec<(&str, &str)> = extra
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let theirs = run_msbuild(proj, &borrowed);

    let mut environment = common::oracle_environment();
    for (k, v) in extra {
        environment.insert(k.clone(), v.clone());
    }
    let ours = run_ours(source, proj, &environment);

    let mut divergences = Vec::new();
    let mut committed = 0usize;
    for (i, name) in PROBES.iter().enumerate() {
        let key = format!("R_{i}");
        let theirs_v = theirs.get(&key).map(String::as_str).unwrap_or("");
        let ours_v = ours.properties.get(&key).map(String::as_str).unwrap_or("");

        // Declining is always allowed; only a commit is a claim.
        if ours.declined(name) {
            continue;
        }
        committed += 1;
        if ours_v != theirs_v {
            divergences.push(format!(
                "$({name}) under {extra:?}: ours = {ours_v:?}, msbuild = {theirs_v:?}"
            ));
        }
    }
    (divergences, committed)
}

/// The core contract: wherever we commit, we agree with MSBuild.
#[test]
fn certain_environment_reads_are_exact() {
    let tmp = tempdir();
    let proj = tmp.join("Demo.fsproj");
    let source = project_source();
    std::fs::write(&proj, &source).unwrap();

    let mut divergences: Vec<String> = Vec::new();
    let mut committed_total = 0usize;

    for case in CASES {
        let extra: Vec<(String, String)> = case
            .env
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let (mut d, committed) = check_snapshot(&source, &proj, &extra);
        for line in &mut d {
            *line = format!("[{}] {line}", case.name);
        }
        divergences.extend(d);
        committed_total += committed;
    }

    assert!(
        divergences.is_empty(),
        "committed environment reads diverge from MSBuild:\n  {}",
        divergences.join("\n  ")
    );
    // Guard against the test passing by declining everything.
    assert!(
        committed_total > 0,
        "no read committed — certain-implies-exact held vacuously"
    );
}

/// The exactness ratchet: names that must stay *certain* under a given snapshot.
///
/// `certain_environment_reads_are_exact` permits declining — so a regression
/// that made the walker conservative everywhere would still pass it. These pin
/// the reads that must keep *committing*, and commit MSBuild's value.
fn assert_commits_exactly(extra: &[(&str, &str)], must_commit: &[&str]) {
    let tmp = tempdir();
    let proj = tmp.join("Demo.fsproj");
    let source = project_source();
    std::fs::write(&proj, &source).unwrap();

    let mut environment = common::oracle_environment();
    for (k, v) in extra {
        environment.insert((*k).to_string(), (*v).to_string());
    }
    let ours = run_ours(&source, &proj, &environment);
    let theirs = run_msbuild(&proj, extra);

    for (i, name) in PROBES.iter().enumerate() {
        if !must_commit.contains(name) {
            continue;
        }
        let key = format!("R_{i}");
        assert!(
            !ours.declined(name),
            "$({name}) must stay certain under {extra:?}, but the walker declined it"
        );
        assert_eq!(
            ours.properties.get(&key).map(String::as_str).unwrap_or(""),
            theirs.get(&key).map(String::as_str).unwrap_or(""),
            "$({name}) must stay certain and exact under {extra:?}"
        );
    }
}

/// C.2a exists so ordinary environment variables become readable.
#[test]
fn commits_the_ordinary_cases() {
    // `OS` is only ours to commit where MSBuild synthesises it — non-Windows
    // hosts. On Windows it is an ordinary environment variable, and this case
    // does not set one, so under the scrubbed snapshot both sides have no `OS`
    // at all and there is nothing to require. (`commits_escaped_environment_values`
    // does set it, so it requires it on every host.)
    let mut must_commit = vec!["FOO", "MY_VAR", "MSBuildDisableFeaturesFromVersion"];
    if !cfg!(windows) {
        must_commit.push("OS");
    }
    assert_commits_exactly(&[("FOO", "bar"), ("MY_VAR", "some value")], &must_commit);
}

/// An environment value is escaped-domain text, and since E1 the walker models
/// that domain — so `%XX` in a value must *commit* MSBuild's unescaped value
/// rather than degrade. Declining here would satisfy certain-implies-exact
/// while silently losing the exactness E1 bought, which is precisely what this
/// pins.
#[test]
fn commits_escaped_environment_values() {
    assert_commits_exactly(
        &[
            ("FOO", "%54rue"),
            ("MY_VAR", "a%20b"),
            ("OS", "%57indows_NT"),
        ],
        &["FOO", "MY_VAR", "OS"],
    );
}

/// The names a generated snapshot may bind. Case-variant spellings (`os`,
/// `foo`, the lowercase ChangeWaves name) are in the alphabet so that
/// *collisions* arise by construction rather than by someone remembering to
/// write one down.
const GEN_NAMES: &[&str] = &[
    "FOO",
    "foo",
    "MY_VAR",
    "OS",
    "os",
    "1FOO",
    "MSBuildProjectName",
    "MSBuildThisFileFullPath",
    "MSBuildToolsPath",
    "MSBuildExtensionsPath",
    "VisualStudioVersion",
    "MSBUILDDISABLEFEATURESFROMVERSION",
    "msbuilddisablefeaturesfromversion",
    "DOTNET_HOST_PATH",
];

/// The values a generated snapshot may bind them to. The `%XX` forms are the
/// point: crossed with `OS` they produce `OS=%57indows_NT`, the escaped
/// override of a seeded default — a combination the hand-written `CASES` list
/// did not contain, and which review had to find by hand.
const GEN_VALUES: &[&str] = &[
    "bar",
    "",
    "Unix",
    "Windows_NT",
    "17.4",
    "999.999",
    "%54rue",
    "%57indows_NT",
    "a%20b",
    "/spoof/dotnet",
];

/// Snapshots: each name independently absent, or bound to one of the values.
fn env_snapshot() -> impl Strategy<Value = Vec<(String, String)>> {
    proptest::collection::vec(proptest::option::of(0..GEN_VALUES.len()), GEN_NAMES.len()).prop_map(
        |choices| {
            choices
                .iter()
                .enumerate()
                .filter_map(|(i, choice)| {
                    choice.map(|v| (GEN_NAMES[i].to_string(), GEN_VALUES[v].to_string()))
                })
                .collect()
        },
    )
}

proptest! {
    // Each case spawns one `dotnet msbuild`, and every spawn in this workspace
    // serialises on the process-global spawn lock (`borzoi-spawn`). So the
    // case count is not free: it lengthens the *whole* suite, and the
    // FCS-backed oracles run against deadlines with little headroom
    // (`overload_corpus_diff` takes ~98s of a 120s budget on an idle machine).
    // 12 is the deliberate cap — the alphabets are small and hand-picked, so
    // the cross product is dense and a dozen draws still reach the
    // combinations a human would not enumerate (the escaped-`OS` bug fell out
    // of a draw this size). Raising it buys reach at the cost of wedging
    // someone else's oracle; raise it locally when hunting, not in the gate.
    #![proptest_config(ProptestConfig { cases: 12, failure_persistence: None, ..ProptestConfig::default() })]

    /// The same contract as `certain_environment_reads_are_exact`, but over
    /// *generated* snapshots — so a dangerous (name, value) pairing does not
    /// have to be foreseen to be tested.
    #[test]
    fn generated_environment_snapshots_are_certain_implies_exact(extra in env_snapshot()) {
        let tmp = tempdir();
        let proj = tmp.join("Demo.fsproj");
        let source = project_source();
        std::fs::write(&proj, &source).unwrap();

        let (divergences, _) = check_snapshot(&source, &proj, &extra);
        prop_assert!(
            divergences.is_empty(),
            "committed environment reads diverge from MSBuild:\n  {}",
            divergences.join("\n  ")
        );
    }
}

fn tempdir() -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!(
        "borzoi-env-diff-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// A pinhole JSON reader for `-getProperty:`'s `{"Properties":{...}}` document.
/// The crate has no JSON dependency and this is the only shape we need.
mod serde_json_lite {
    use std::collections::HashMap;

    pub struct Value(HashMap<String, String>);

    impl Value {
        pub fn properties(&self) -> HashMap<String, String> {
            self.0.clone()
        }
    }

    /// Parse `{ "Properties": { "K": "V", … } }`. Values are JSON strings with
    /// standard escapes; MSBuild emits no nested objects for `-getProperty:`.
    pub fn parse(s: &str) -> Result<Value, String> {
        let start = s.find('{').ok_or("no object")?;
        let body = &s[start..];
        let props_at = body.find("\"Properties\"").ok_or("no Properties key")?;
        let mut rest = body[props_at..]
            .find('{')
            .map(|i| &body[props_at + i + 1..])
            .ok_or("no Properties object")?;

        let mut out = HashMap::new();
        while let Some(k0) = rest.find('"') {
            // A `}` before the next quote ends the object.
            if let Some(close) = rest.find('}')
                && close < k0
            {
                break;
            }
            let (key, after) = read_string(&rest[k0..])?;
            let colon = after.find(':').ok_or("no colon")?;
            let after = &after[colon + 1..];
            let v0 = after.find('"').ok_or("no value string")?;
            let (value, after) = read_string(&after[v0..])?;
            out.insert(key, value);
            rest = after;
        }
        Ok(Value(out))
    }

    /// `s` starts at the opening quote; returns the unescaped body and the rest.
    fn read_string(s: &str) -> Result<(String, &str), String> {
        let b = s.as_bytes();
        debug_assert_eq!(b[0], b'"');
        let mut out = String::new();
        let mut i = 1;
        while i < b.len() {
            match b[i] {
                b'"' => return Ok((out, &s[i + 1..])),
                b'\\' => {
                    i += 1;
                    let c = *b.get(i).ok_or("trailing escape")?;
                    out.push(match c {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        other => other as char,
                    });
                    i += 1;
                }
                _ => {
                    let ch = s[i..].chars().next().ok_or("bad utf8")?;
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
        Err("unterminated string".to_string())
    }
}

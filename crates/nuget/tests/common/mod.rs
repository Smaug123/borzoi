//! Harness for `tools/nuget-oracle`: build it once (marker-gated), then
//! drive it as a long-lived JSONL request/response child.
//!
//! The process plumbing is the shared [`BatchChild`]; this module only knows the
//! JSON protocol and how to build the tool. It was previously a hand-rolled copy
//! of the fcs-dump runner with the timeout/respawn machinery deliberately left
//! out, on the reasoning that `NuGet.Versioning` calls are pure and synchronous
//! so a hang would be a harness bug. True as far as it goes — but "a hang would
//! be a bug" is not a reason to hang rather than say so, and there is no cost to
//! sharing a bounded driver with the oracles that provably do wedge.

#![allow(dead_code)] // each importer uses a different subset.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use borzoi_oracle_harness::{BatchChild, BoundedCommand};

/// Budget for the one `dotnet build` this harness runs (the nuget oracle).
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
/// wrong sources, while `cargo test`'s serial test binaries skip the ~2 s
/// `dotnet build` after the first one has run it.
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

/// A long-lived `nuget-oracle` child driven in lock-step: write one JSON
/// request line, read exactly the one JSON response line it produces.
///
/// [`BatchChild`] owns the process, so a wedged or crashed oracle is killed,
/// respawned, and the request retried rather than blocking forever. Respawning is
/// sound because the protocol is stateless — every request carries the version /
/// range / framework strings it wants parsed, so a fresh child answers
/// identically.
pub struct Oracle {
    child: BatchChild,
}

impl Oracle {
    pub fn spawn() -> Oracle {
        Oracle {
            child: BatchChild::spawn(ensure_oracle_built(), &[]),
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

// ============================================================================
// Deterministic input generation (shared by the differential tests)
// ============================================================================

/// SplitMix64: tiny, deterministic, good-enough mixing for input generation.
/// Fixed seeds keep every differential run identical, so a failure
/// reproduces exactly; the *random* exploration lives in the proptest files.
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

/// Structured-ish version strings biased towards NuGet's fiddly corners.
pub fn gen_version_string(rng: &mut SplitMix64) -> String {
    const NUMS: &[&str] = &[
        "0",
        "1",
        "7",
        "10",
        "01",
        "00",
        "123456",
        "2147483647",
        "2147483648",
        "4294967295",
        "18446744073709551616",
    ];
    const LABELS: &[&str] = &[
        "alpha",
        "BETA",
        "rc",
        "0",
        "1",
        "11",
        "011",
        "00",
        "a1",
        "1a",
        "-",
        "a-b",
        "x",
        "2147483647",
        "2147483648",
        // int.TryParse accepts a leading '-', so these are *numeric* labels.
        "-1",
        "-0",
        "-01",
        "-2147483648",
        "-2147483649",
    ];
    const NOISE: &[char] = &[
        ' ', '\t', '+', '-', '.', '*', 'v', '_', '~', 'α', '٣', '\u{00a0}', ',',
    ];

    let mode = rng.below(100);
    let mut s = String::new();
    if mode < 80 {
        // Structured: 1..=5 numeric parts, optional labels, optional metadata.
        let nparts = 1 + rng.below(5);
        for i in 0..nparts {
            if i > 0 {
                s.push('.');
            }
            s.push_str(rng.pick(NUMS));
        }
        if rng.below(2) == 0 {
            s.push('-');
            let nlabels = 1 + rng.below(3);
            for i in 0..nlabels {
                if i > 0 {
                    s.push('.');
                }
                s.push_str(rng.pick(LABELS));
            }
        }
        if rng.below(3) == 0 {
            s.push('+');
            let nmeta = 1 + rng.below(2);
            for i in 0..nmeta {
                if i > 0 {
                    s.push('.');
                }
                s.push_str(rng.pick(LABELS));
            }
        }
        // A quarter of structured inputs get one random char spliced in.
        if mode < 20 {
            let chars: Vec<char> = s.chars().collect();
            let at = rng.below(chars.len() + 1);
            let mut mutated: String = chars[..at].iter().collect();
            mutated.push(*rng.pick(NOISE));
            mutated.extend(&chars[at..]);
            s = mutated;
        }
    } else {
        // Fully adversarial soup.
        const ALPHABET: &[char] = &[
            '0', '1', '9', '.', '-', '+', 'a', 'Z', ' ', '*', 'v', 'α', '٣', '\t',
        ];
        let len = rng.below(13);
        for _ in 0..len {
            s.push(*rng.pick(ALPHABET));
        }
    }
    s
}

/// Range-shaped strings biased towards bracket/float corners.
pub fn gen_range_string(rng: &mut SplitMix64) -> String {
    fn gen_floatish(rng: &mut SplitMix64) -> String {
        const BASES: &[&str] = &[
            "*", "1.*", "0.*", "1.2.*", "1.2.3.*", "10.0.*", "01.*",
            // Dots-zero trailing stars: the shapes whose bracket behaviour a
            // fresh-seed soak caught the first fixed corpus mispinning.
            "1*", "0*", "12*", "1.9*",
        ];
        const RELEASES: &[&str] = &["", "-*", "-beta*", "-beta.*", "-BETA*", "-rc.1*", "-0*"];
        let base = *rng.pick(BASES);
        let release = *rng.pick(RELEASES);
        format!("{base}{release}")
    }

    match rng.below(100) {
        // Bare version (already exercised heavily in version_diff, but here
        // it goes through the *range* parser).
        0..=19 => gen_version_string(rng),
        // Bare float.
        20..=34 => gen_floatish(rng),
        // Bracketed forms.
        35..=84 => {
            let open = if rng.below(2) == 0 { '[' } else { '(' };
            let close = if rng.below(2) == 0 { ']' } else { ')' };
            let pad = |rng: &mut SplitMix64| -> &'static str {
                match rng.below(4) {
                    0 => " ",
                    1 => "  ",
                    _ => "",
                }
            };
            let bound = |rng: &mut SplitMix64| -> String {
                match rng.below(10) {
                    0..=5 => gen_version_string(rng),
                    6..=7 => gen_floatish(rng),
                    _ => String::new(),
                }
            };
            match rng.below(10) {
                // Single-element interval.
                0..=2 => {
                    let b = bound(rng);
                    format!("{open}{}{b}{}{close}", pad(rng), pad(rng))
                }
                // Three parts (invalid).
                3 => {
                    let (a, b, c) = (bound(rng), bound(rng), bound(rng));
                    format!("{open}{a},{b},{c}{close}")
                }
                // Two parts.
                _ => {
                    let (a, b) = (bound(rng), bound(rng));
                    format!(
                        "{open}{}{a}{},{}{b}{}{close}",
                        pad(rng),
                        pad(rng),
                        pad(rng),
                        pad(rng)
                    )
                }
            }
        }
        // Adversarial soup.
        _ => {
            const ALPHABET: &[char] = &[
                '0', '1', '.', ',', '-', '+', '*', '[', ']', '(', ')', 'a', ' ',
            ];
            let len = rng.below(14);
            let mut s = String::new();
            for _ in 0..len {
                s.push(*rng.pick(ALPHABET));
            }
            s
        }
    }
}

/// The TFM zoo: every framework family NuGet's mappings know, in short,
/// long, platformed, portable, and historical forms — plus adversarial
/// spellings.
pub const FRAMEWORK_ZOO: &[&str] = &[
    // Modern .NET (no platform).
    "net5.0",
    "net6.0",
    "net7.0",
    "net8.0",
    "net9.0",
    "net10.0",
    "net11.0",
    // The infamous digit split: "net10" is .NETFramework 1.0, "net10.0" is
    // .NETCoreApp 10.0.
    "net10",
    "net11",
    "net472.0",
    // Platformed modern TFMs.
    "net6.0-windows",
    "net8.0-windows",
    "net8.0-windows10.0.19041",
    "net5.0-windows10.0.19041.0",
    "net6.0-android",
    "net6.0-android31.0",
    "net6.0-ios",
    "net6.0-ios15.0",
    "net6.0-maccatalyst",
    "net6.0-macos",
    "net6.0-tvos",
    "net8.0-browser",
    "net8.0-tizen",
    "net8.0-Windows",
    "NET8.0-WINDOWS",
    "net8.0-windows-",
    "net8.0-",
    // .NET Framework.
    "net11",
    "net20",
    "net35",
    "net40",
    "net403",
    "net45",
    "net451",
    "net452",
    "net46",
    "net461",
    "net462",
    "net47",
    "net471",
    "net472",
    "net48",
    "net481",
    "net4.5",
    "net4.6.1",
    "net",
    // netstandard / netcoreapp.
    "netstandard",
    "netstandard1.0",
    "netstandard1.1",
    "netstandard1.2",
    "netstandard1.3",
    "netstandard1.4",
    "netstandard1.5",
    "netstandard1.6",
    "netstandard2.0",
    "netstandard2.1",
    "netstandard2.2",
    "netcoreapp1.0",
    "netcoreapp1.1",
    "netcoreapp2.0",
    "netcoreapp2.1",
    "netcoreapp2.2",
    "netcoreapp3.0",
    "netcoreapp3.1",
    "netcoreapp5.0",
    // .NETCore (Windows-8 Store framework) — codex-review gap.
    "netcore",
    "netcore45",
    "netcore451",
    "netcore50",
    "netcoreapp",
    // UWP / Windows / phone / Silverlight.
    "uap",
    "uap10.0",
    "uap10.0.14393",
    // Sub-10 UAP short forms (uap10 is v1.0, uap80 is v8.0) all floor
    // to UAP 10.0 for compat — codex-review gap.
    "uap8",
    "uap10",
    "uap80",
    "win",
    "win8",
    "win81",
    "win10",
    "winrt",
    // WinRT version ceiling: <=4.5 maps to Windows 8.0, above rejects.
    "winrt45",
    "winrt46",
    "winrt8",
    "wp",
    "wp7",
    "wp71",
    "wp8",
    "wp81",
    "wpa81",
    "sl3",
    "sl4",
    "sl5",
    // Xamarin / Mono.
    "monoandroid",
    "monoandroid90",
    "monotouch",
    "monomac",
    "xamarinios",
    "xamarin.ios",
    "xamarin.ios10",
    "xamarinmac",
    "xamarin.mac20",
    "xamarintvos",
    "xamarinwatchos",
    // Legacy dnx/asp/dotnet.
    "dnx451",
    "dnx452",
    "dnxcore50",
    "aspnet50",
    "aspnetcore50",
    "dotnet",
    "dotnet5.1",
    "dotnet5.4",
    "dotnet5.5",
    "dotnet5.6",
    "dotnet5.5",
    "dotnet6.0",
    // Others.
    "native",
    "tizen40",
    "tizen60",
    "netmf",
    "netnano1.0",
    "any",
    "agnostic",
    "unsupported",
    // Portable profiles.
    "portable-net45+win8",
    "portable-net45+win8+wpa81",
    "portable-net40+sl5+win8+wp8",
    "portable-win8+net45",
    "portable-net451+win81",
    "portable-net45+sl5+win8",
    "portable-profile259",
    "portable-Profile7",
    "portable-",
    "portable",
    "net45+win8",
    // Long (FrameworkName) forms.
    ".NETFramework,Version=v4.7.2",
    ".NETFramework,Version=v4.0,Profile=Client",
    ".NETFramework,Version=v4.0,Profile=Full",
    ".NETCoreApp,Version=v8.0",
    ".NETCoreApp,Version=v3.1",
    ".NETStandard,Version=v2.0",
    ".NETStandard,Version=v2.1",
    // Single-component long-form versions — codex-review gap.
    ".NETFramework,Version=v4",
    ".NETCoreApp,Version=v8",
    ".NETPortable,Version=v4.5,Profile=Profile259",
    ".NETPortable,Version=v0.0,Profile=Profile259",
    ".NETPlatform,Version=v5.4",
    "Silverlight,Version=v5.0",
    "WindowsPhone,Version=v8.0",
    "UAP,Version=v10.0",
    ".NETFramework, Version=v4.5",
    ".netframework,version=v4.5",
    ".NETFramework,Version=4.5",
    ".NETFramework,Version=v4.5,Profile=",
    ".NETCoreApp,Version=v8.0,Platform=windows",
    // Case and separator adversaria.
    "NET45",
    "Net45",
    "NETSTANDARD2.0",
    "NetStandard2.0",
    "netStandard2.0 ",
    " net8.0",
    "net8.0 ",
    "net 8.0",
    "net8,0",
    "net8_0",
    "netstandard-2.0",
    "net-8.0",
    "net8.0+windows",
    "v4.5",
    "8.0",
    "45",
    "netstandard2.0.3",
    "net5.0.1",
    "net999999999999.0",
    "net2147483648",
    "netstandard99.99",
    "",
    " ",
    "xyz123",
    "lib",
    "ref",
    "_._",
];

/// Mutate zoo entries: case flips, separator swaps, digit tweaks, splices.
pub fn gen_framework_string(rng: &mut SplitMix64) -> String {
    let base = *rng.pick(FRAMEWORK_ZOO);
    match rng.below(10) {
        0..=3 => {
            // Random ASCII-case flip across the string.
            base.chars()
                .map(|c| {
                    if c.is_ascii_alphabetic() && rng.below(3) == 0 {
                        if c.is_ascii_lowercase() {
                            c.to_ascii_uppercase()
                        } else {
                            c.to_ascii_lowercase()
                        }
                    } else {
                        c
                    }
                })
                .collect()
        }
        4..=6 => {
            // Splice one character.
            const NOISE: &[char] = &['.', '-', '+', '0', '1', '9', ' ', 'x', ','];
            let chars: Vec<char> = base.chars().collect();
            let at = rng.below(chars.len() + 1);
            let mut s: String = chars[..at].iter().collect();
            s.push(*rng.pick(NOISE));
            s.extend(&chars[at..]);
            s
        }
        7..=8 => {
            // Digit tweak.
            base.chars()
                .map(|c| {
                    if c.is_ascii_digit() && rng.below(3) == 0 {
                        char::from_digit(rng.below(10) as u32, 10).unwrap()
                    } else {
                        c
                    }
                })
                .collect()
        }
        _ => format!("{}{}", base, rng.pick(&["0", ".0", "-", "+x", "1"])),
    }
}

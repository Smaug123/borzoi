//! Generative differential: `<PackageReference>` `Include`/`Update` collapse
//! over an **identity-matching** alphabet, against the real MSBuild evaluator
//! (the oracle's `itemsMeta` op — each item's `EvaluatedInclude` plus its
//! captured metadata, resident, so the sweep pays .NET startup once per binary).
//!
//! ## Why this exists
//!
//! An `Update` modifies the *prior* `Include`s that share its identity, and
//! MSBuild matches those identities as `GetFullPath`-normalized paths under
//! Unicode `OrdinalIgnoreCase`. Our capture compares raw identities with
//! `eq_ignore_ascii_case`. That gap is invisible to the type system, and three
//! review rounds found three spellings of it one at a time — a non-ASCII case
//! variant (`Update="ångström"` ↔ `Include="Ångström"`), a path spelling
//! (`Update="./A"` ↔ `"A"`), and a decoded trailing space (`Update="A%20"` ↔
//! `"A"`, Windows-trimmed). Each would be a **wrong commit**: an `Update` proved
//! falsely inert, or mis-dropped in the merge, publishing stale metadata while
//! reporting `package_references_uncertain == false`.
//!
//! A reviewer finding those one per round is not a strategy. The evaluator now
//! guards them with a positive allow-list (only a plain package-id-shaped token
//! takes the raw-compare shortcut; anything else declines), but nothing
//! *mechanically* proves that allow-list is complete. This harness does: it
//! generates `Include`/`Update` node sequences whose identities span package-id
//! shapes, ASCII and non-ASCII case variants, path spellings, escaped forms, and
//! within-spec / cross-node duplicates, then asserts **certain implies exact**
//! against the real evaluator. If the allow-list ever lets a divergent identity
//! commit, the captured set diverges from MSBuild's and the sweep fails.
//!
//! ## The asserted property
//!
//! Whenever our parse commits — `package_references_uncertain == false`, the
//! public flag dependency consumers gate on — our effective `PackageReference`
//! set (ordered identities plus the five captured metadata: `Version` /
//! `VersionOverride` / `Include`/`Exclude`/`PrivateAssets`) must equal
//! MSBuild's, exactly. A decline makes no claim (the fail-safe channel), so the
//! harness also asserts a floor
//! on how often we *do* commit — a decline-everything model would pass
//! vacuously, and the floor keeps the real matching semantics (prior-only
//! merge, ASCII case-insensitivity, `;`-split, inert-Update) genuinely exercised.
//!
//! A built-in robustness: every *platform-divergent* identity (`./A`, `A%20`,
//! non-ASCII case) is in our decline set, so the harness never asserts on a case
//! whose MSBuild answer is platform-specific — it cannot go flaky between this
//! Unix oracle and a Windows LSP. What it *does* assert on is the plain-id space
//! where the raw compare is faithful on every platform.
//!
//! Live globs and `@(…)` item-derived `Update` targets are out of the generated
//! space (their own concerns). Inputs are a fixed-seed sweep, so a failure
//! reproduces exactly.

mod common;

use borzoi_msbuild::{PackageReference, parse_fsproj_with_imports};
use common::{Oracle, SplitMix64};
use std::collections::HashMap;

/// The metadata a captured [`PackageReference`] records — the columns the
/// differential compares. Kept in lock-step with the struct fields.
const CAPTURED_METADATA: &[&str] = &[
    "Version",
    "VersionOverride",
    "IncludeAssets",
    "ExcludeAssets",
    "PrivateAssets",
];

/// Identities whose raw `eq_ignore_ascii_case` compare *is* MSBuild's normalized
/// `OrdinalIgnoreCase` — plain package-id shapes, including ASCII case variants
/// of each other (`Alpha`/`alpha`, `Beta.Core`/`BETA.CORE`), so a generated
/// `Update` genuinely tests case-insensitive matching and merge.
const PLAIN_IDS: &[&str] = &[
    "Alpha",
    "alpha",
    "Beta.Core",
    "BETA.CORE",
    "Gamma-Net",
    "Delta_Pkg",
];

/// Identities MSBuild path-/case-normalizes onto a plain id but our raw compare
/// does not — the hazards the allow-list must decline. `./Alpha` /
/// `Sub/../Alpha` (path), `Alpha%20` (Windows-trimmed trailing space), `Alpha.`
/// (Windows-trimmed trailing dot), `Ångström` (non-ASCII case). When used as an
/// `Update` target against a matching `Include`, a wrong commit would diverge.
const HAZARD_IDS: &[&str] = &[
    "./Alpha",
    "Sub/../Alpha",
    "Alpha%20",
    "Alpha.",
    "\u{c5}ngstr\u{f6}m",
    "Alpha;alpha",
];

/// Metadata values kept plain ASCII (the escaped-value domain is
/// `fsproj_item_escape_generative_diff`'s job); here the identity is the star.
const VERSIONS: &[&str] = &["1.0.0", "2.0.0"];
const ASSET_VALUES: &[&str] = &["all", "none", "compile"];

/// The four asset/override metadata a generated `Ref` may additionally carry —
/// every captured column *except* `Version` (which `Ref::generate` handles
/// specially, since a versionless reference declines on its own account). Each
/// is drawn independently so a generated `Update` exercises the per-key merge
/// of each field, not just `Version`.
const EXTRA_METADATA: &[&str] = &[
    "VersionOverride",
    "IncludeAssets",
    "ExcludeAssets",
    "PrivateAssets",
];

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Include,
    Update,
}

struct Ref {
    op: Op,
    id: String,
    /// `(name, value)` metadata, emitted as attributes.
    metadata: Vec<(&'static str, String)>,
}

impl Ref {
    /// Generate one node of the given `op`. An `Include` always takes a plain id
    /// and a `Version` (a versionless reference declines on its own account — a
    /// separate, already-tested concern — and would only add noise to the
    /// matching signal). An `Update` draws a hazard id a third of the time when
    /// `allow_hazard` (exercising the decline), and otherwise a plain id; its
    /// `Version` is optional, since a metadata-only `Update` is the common SDK
    /// shape. Both may carry any of the four extra metadata columns, so the
    /// per-key merge of every captured field is exercised.
    fn generate(rng: &mut SplitMix64, op: Op, allow_hazard: bool) -> Ref {
        let id = match op {
            Op::Include => (*rng.pick(PLAIN_IDS)).to_string(),
            Op::Update if allow_hazard && rng.below(3) == 0 => (*rng.pick(HAZARD_IDS)).to_string(),
            Op::Update => (*rng.pick(PLAIN_IDS)).to_string(),
        };
        let mut metadata = Vec::new();
        if op == Op::Include || rng.below(2) == 0 {
            metadata.push(("Version", (*rng.pick(VERSIONS)).to_string()));
        }
        for name in EXTRA_METADATA {
            if rng.below(3) == 0 {
                let value = if *name == "VersionOverride" {
                    *rng.pick(VERSIONS)
                } else {
                    *rng.pick(ASSET_VALUES)
                };
                metadata.push((name, value.to_string()));
            }
        }
        Ref { op, id, metadata }
    }

    fn xml(&self) -> String {
        let attr = match self.op {
            Op::Include => "Include",
            Op::Update => "Update",
        };
        let mut s = format!("    <PackageReference {attr}=\"{}\"", self.id);
        for (name, value) in &self.metadata {
            s.push_str(&format!(" {name}=\"{value}\""));
        }
        s.push_str(" />\n");
        s
    }
}

/// Every fixture is an `Include` (node 0, so there is something to match) plus
/// one or two further nodes, *at least one of which is an `Update`*. Forcing a
/// meaningful `Update` into every fixture is what lets the sweep's commit floor
/// double as an `Update`-coverage floor — otherwise Include-only fixtures could
/// satisfy the floor while every `Update`-bearing fixture silently over-declined.
fn generate_refs(rng: &mut SplitMix64) -> Vec<Ref> {
    let mut refs = vec![Ref::generate(rng, Op::Include, false)];
    let tail = 1 + rng.below(2);
    for _ in 0..tail {
        let op = if rng.below(2) == 0 {
            Op::Include
        } else {
            Op::Update
        };
        refs.push(Ref::generate(rng, op, true));
    }
    if refs[1..].iter().all(|r| r.op == Op::Include) {
        *refs.last_mut().expect("at least two nodes") = Ref::generate(rng, Op::Update, true);
    }
    refs
}

fn project_xml(refs: &[Ref]) -> String {
    let mut s = String::from("<Project>\n  <ItemGroup>\n");
    for r in refs {
        s.push_str(&r.xml());
    }
    s.push_str("  </ItemGroup>\n</Project>\n");
    s
}

/// The comparison tuple for one effective reference: identity plus the captured
/// metadata columns, `None` for unset. MSBuild reports an unset metadatum as
/// `""`; our capture reports `None` — normalise both to `None`.
type Row = (String, Vec<Option<String>>);

fn ours_rows(refs: &[PackageReference]) -> Vec<Row> {
    refs.iter()
        .map(|r| {
            let cols = vec![
                r.version.clone(),
                r.version_override.clone(),
                r.include_assets.clone(),
                r.exclude_assets.clone(),
                r.private_assets.clone(),
            ];
            (r.id.clone(), cols)
        })
        .collect()
}

fn theirs_rows(items: &[(String, HashMap<String, String>)]) -> Vec<Row> {
    items
        .iter()
        .map(|(identity, metadata)| {
            let cols = CAPTURED_METADATA
                .iter()
                .map(|name| match metadata.get(*name) {
                    Some(v) if !v.is_empty() => Some(v.clone()),
                    _ => None,
                })
                .collect();
            (identity.clone(), cols)
        })
        .collect()
}

/// Returns `true` iff the parse committed to a certain package set
/// (`package_references_uncertain == false`) — the public flag dependency
/// consumers gate on — in which case the effective set is diffed against
/// MSBuild before returning. A `false` return therefore *is* a direct assertion
/// that the flag was set.
fn check(oracle: &mut Oracle, refs: &[Ref]) -> bool {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
    let proj = root.join("Demo.fsproj");
    let xml = project_xml(refs);
    std::fs::write(&proj, &xml).expect("write project");

    let parsed = parse_fsproj_with_imports(
        &xml,
        &proj,
        &HashMap::new(),
        &common::oracle_environment(),
        None,
        None,
    )
    .expect("well-formed XML parses");

    // The claim is *exactly* `package_references_uncertain == false` — the
    // public flag dependency consumers gate on. A diagnostic not tied to the
    // package set does not withdraw that claim, so it must NOT suppress the
    // comparison (else a `Decline` corner could pass on an unrelated diagnostic
    // while the set is still advertised certain, and a real regression of the
    // allow-list's `note_package_uncertain` would go unseen). Gate on the flag
    // alone; if it is clear the set is claimed exact and must match MSBuild.
    if parsed.package_references_uncertain {
        return false;
    }

    let ours = ours_rows(&parsed.package_references);
    let theirs = theirs_rows(
        &oracle
            .items_meta(&xml, &proj, "PackageReference", CAPTURED_METADATA)
            .expect("MSBuild evaluates these documents"),
    );

    assert_eq!(
        ours, theirs,
        "certain-implies-exact violated.\nproject:\n{xml}\nours:   {ours:#?}\ntheirs: {theirs:#?}"
    );
    true
}

/// The expected certain-implies-exact branch for a hand-picked corner.
#[derive(Clone, Copy)]
enum Expect {
    /// We must commit; `check`'s internal `assert_eq` then pins the exact set.
    Commit,
    /// We must decline — set `package_references_uncertain`. `check` gates on
    /// exactly that flag, so `!check` *is* a direct assertion of it, not a
    /// diff against MSBuild — a hazard identity's decline is a cross-platform
    /// contract (the allow-list rejects it), and on Unix some of these (`%20`,
    /// trailing `.`) do *not* even collapse in MSBuild, so a
    /// certain-implies-exact check would pass vacuously if the allow-list
    /// relaxed. Asserting the decline keeps the guard from disappearing on the
    /// platform the sweep runs on.
    Decline,
}

/// Hand-picked corners: one per finding across all review rounds, each with its
/// expected branch pinned. The committing corners exercise real matching
/// (merge, ASCII case-insensitivity, prior-only, inert-Update) and are diffed
/// against the real evaluator inside `check`; the declining corners pin that
/// every hazard identity — on the `Update` side *and* on a captured `Include`
/// (both arms of the production `has_hazard` scan) — is refused.
#[test]
fn hand_picked_corners() {
    let mut oracle = Oracle::spawn();
    let include = |id: &str, ver: &str| Ref {
        op: Op::Include,
        id: id.to_string(),
        metadata: vec![("Version", ver.to_string())],
    };
    let update = |id: &str, ver: &str| Ref {
        op: Op::Update,
        id: id.to_string(),
        metadata: vec![("Version", ver.to_string())],
    };
    let cases: Vec<(Vec<Ref>, Expect)> = vec![
        // Plain-id matching: the Update merges onto the prior Include.
        (
            vec![include("Alpha", "1.0.0"), update("Alpha", "2.0.0")],
            Expect::Commit,
        ),
        // ASCII case-insensitive matching: `alpha` updates `Alpha`.
        (
            vec![include("Alpha", "1.0.0"), update("alpha", "2.0.0")],
            Expect::Commit,
        ),
        // Per-key merge of the `VersionOverride` and `ExcludeAssets` columns
        // (both otherwise unexercised): the Update stamps them onto the matched
        // Include, `Version` untouched — every captured field must agree.
        (
            vec![
                Ref {
                    op: Op::Include,
                    id: "Alpha".to_string(),
                    metadata: vec![("Version", "1.0.0".to_string())],
                },
                Ref {
                    op: Op::Update,
                    id: "Alpha".to_string(),
                    metadata: vec![
                        ("VersionOverride", "9.9.9".to_string()),
                        ("ExcludeAssets", "compile".to_string()),
                    ],
                },
            ],
            Expect::Commit,
        ),
        // Prior-only: an Update *before* its Include applies to nothing.
        (
            vec![update("Alpha", "2.0.0"), include("Alpha", "1.0.0")],
            Expect::Commit,
        ),
        // Update matching no Include is inert (Include unchanged).
        (
            vec![include("Alpha", "1.0.0"), update("Beta.Core", "2.0.0")],
            Expect::Commit,
        ),
        // --- hazardous Update identity (the `sources` arm of `has_hazard`) ---
        // Path spelling: MSBuild matches `./Alpha`→`Alpha`.
        (
            vec![include("Alpha", "1.0.0"), update("./Alpha", "2.0.0")],
            Expect::Decline,
        ),
        // Trailing space, Windows-trimmed (does *not* collapse on Unix — the
        // case the old certain-implies-exact-only check missed).
        (
            vec![include("Alpha", "1.0.0"), update("Alpha%20", "2.0.0")],
            Expect::Decline,
        ),
        // Trailing dot, likewise Windows-trimmed only — the boundary the
        // `!id.ends_with('.')` guard covers, pinned explicitly for the same
        // reason as the trailing space above.
        (
            vec![include("Alpha", "1.0.0"), update("Alpha.", "2.0.0")],
            Expect::Decline,
        ),
        // Backslash separator: a literal filename char on Unix (no collapse)
        // but a path separator on Windows, where `.\Alpha` normalizes onto
        // `Alpha`. The allow-list rejects it today via the ASCII package-id
        // charset (`\` is not `[A-Za-z0-9._-]`), but that is Windows-only
        // behaviour the diff-on-Unix cannot pin, so it is asserted explicitly —
        // guarding, in particular, any future move back to a per-character
        // deny-list that forgets `\`.
        (
            vec![include("Alpha", "1.0.0"), update(".\\Alpha", "2.0.0")],
            Expect::Decline,
        ),
        // Non-ASCII case.
        (
            vec![
                include("\u{c5}ngstr\u{f6}m", "1.0.0"),
                update("\u{e5}ngstr\u{f6}m", "2.0.0"),
            ],
            Expect::Decline,
        ),
        // Same-spec duplicate is position-independent.
        (
            vec![update("Alpha;Alpha", "2.0.0"), include("Alpha", "1.0.0")],
            Expect::Decline,
        ),
        // --- hazardous captured Include (the `captured` arm of `has_hazard`) ---
        // A plain Update MSBuild path-normalizes onto a hazardous prior Include;
        // our raw matcher would treat the Update as inert, so deleting the
        // production scan over captured Includes would wrongly commit here.
        (
            vec![include("./Alpha", "1.0.0"), update("Alpha", "2.0.0")],
            Expect::Decline,
        ),
        (
            vec![include("Sub/../Alpha", "1.0.0"), update("Alpha", "2.0.0")],
            Expect::Decline,
        ),
    ];
    for (refs, expect) in &cases {
        let committed = check(&mut oracle, refs);
        match expect {
            Expect::Commit => assert!(
                committed,
                "expected a commit but the parse declined:\n{}",
                project_xml(refs)
            ),
            Expect::Decline => assert!(
                !committed,
                "expected a decline (hazardous identity must not take the \
                 raw-compare shortcut) but the parse committed:\n{}",
                project_xml(refs)
            ),
        }
    }
}

/// The sweep. Every case that commits is checked against MSBuild; the floor
/// keeps a decline-everything model from passing vacuously.
#[test]
fn generated_packageref_updates_are_certain_implies_exact() {
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0x9e37_79b9_7f4a_7c15);
    let mut committed = 0usize;
    const CASES: usize = 200;
    for _ in 0..CASES {
        let refs = generate_refs(&mut rng);
        if check(&mut oracle, &refs) {
            committed += 1;
        }
    }
    // Every fixture contains an `Update` (see `generate_refs`), so this is an
    // *Update-coverage* floor, not merely an overall one: it cannot be satisfied
    // by Include-only fixtures while the identity-matching path silently
    // over-declines. Declines come from the hazard-id Updates (the intended
    // fail-safe) and the occasional same-spec duplicate; a healthy fraction
    // still commits, since two-thirds of generated Updates carry plain ids. A
    // *fall* below the floor is a regression toward over-declining. (Observed at
    // this seed: 141/200.)
    eprintln!("packageref identity sweep: {committed}/{CASES} committed");
    assert!(
        committed * 2 >= CASES,
        "only {committed}/{CASES} Update-bearing fixtures committed — \
         certain-implies-exact is passing vacuously"
    );
}

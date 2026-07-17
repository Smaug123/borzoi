//! Generative differential: **certainty implies exactness** for the
//! package/framework dependency capture.
//!
//! `fsproj_packageref_diff` pins hand-written fixtures; this harness instead
//! *generates* small random projects plus a synthetic SDK, evaluates each with
//! our walker, and — whenever we claim `package_references_uncertain == false`
//! — diffs the captured `PackageReference` / `FrameworkReference` sets
//! field-for-field against `dotnet msbuild -getItem` resolving the same
//! synthetic SDK through `MSBuildSDKsPath`. Uncertain evaluations make no
//! claim, so they are skipped (a separate sanity test bounds how often that
//! may happen, because a property that always skips silently tests nothing).
//!
//! This is the mechanical enforcement of the certainty envelope: the flag's
//! soundness is maintained by many scattered "degrade here" sites, and this
//! property catches the class of bug where deleting or gating one of them
//! leaves us claiming certainty for an evaluation MSBuild disagrees with.
//!
//! Scope caveat: the oracle sees MSBuild's *evaluation-time* item view, so
//! restore-level semantics (CPM version application, `GlobalPackageReference`
//! conversion by `NuGet.targets`) are out of reach here. The generator still
//! emits CPM items — they must degrade the set to uncertain, and the sanity
//! test would notice if uncertainty became the norm — but a wrongly-*certain*
//! CPM capture whose evaluation items match MSBuild's would not be caught.
//! That end of the envelope belongs to the future restore-level oracle.
//!
//! The generated space deliberately stays inside constructs the unit and
//! fixture differentials already pin one-by-one (literal and `$(…)` items,
//! helper `@(…)` lists, attribute/child `Version`, defined/undefined-property
//! conditions, SDK/project property interleavings) — the value added here is
//! the *combinations*, especially across the SDK/project boundary.
//!
//! Each certain case spawns `dotnet msbuild`, so this runs under `nix develop`
//! (offline SDK) like the sibling differential tests.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod common;

use borzoi_msbuild::{
    ParsedProject, SdkPaths, SdkResolution, SdkResolveError, parse_fsproj_with_imports,
};
use borzoi_oracle_harness::BoundedCommand;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Fixture model. Everything renders from small index pools so shrinking is
// meaningful and the XML stays well-formed by construction.
// ---------------------------------------------------------------------------

const PROP_NAMES: [&str; 3] = ["VerA", "VerB", "PkgFlag"];
const IDS: [&str; 3] = ["Alpha", "Beta", "Gamma"];
const LITERALS: [&str; 3] = ["1.0", "2.0", "3.0-pre"];

/// A `$(…)`-expandable value position.
#[derive(Debug, Clone, Copy)]
enum Value {
    Lit(usize),
    Prop(usize),
}

impl Value {
    fn render(self) -> String {
        match self {
            Value::Lit(i) => LITERALS[i].to_string(),
            Value::Prop(i) => format!("$({})", PROP_NAMES[i]),
        }
    }
}

/// A `Condition` attribute. `Switch` is always defined (`true`, first SDK
/// group) so the `SwitchEq` variants evaluate cleanly; `NeverSet` is never
/// written anywhere, so both `Undef*` variants lean on an undefined property
/// and must degrade whatever package decision they gate.
#[derive(Debug, Clone, Copy)]
enum Cond {
    None,
    SwitchEq(bool),
    UndefEmpty,
    UndefEqTrue,
}

impl Cond {
    fn render(self) -> String {
        match self {
            Cond::None => String::new(),
            Cond::SwitchEq(want) => {
                format!(" Condition=\"'$(Switch)' == '{want}'\"")
            }
            Cond::UndefEmpty => " Condition=\"'$(NeverSet)' == ''\"".to_string(),
            Cond::UndefEqTrue => " Condition=\"'$(NeverSet)' == 'true'\"".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PropWrite {
    name: usize,
    value: Value,
    cond: Cond,
}

#[derive(Debug, Clone, Copy)]
enum Identity {
    Id(usize),
    TwoIds(usize, usize),
    Prop(usize),
    /// `@(HelperPkg)` — consumes whatever helper items precede it.
    Helper,
}

impl Identity {
    fn render(self) -> String {
        match self {
            Identity::Id(i) => IDS[i].to_string(),
            Identity::TwoIds(a, b) => format!("{};{}", IDS[a], IDS[b]),
            Identity::Prop(i) => format!("$({})", PROP_NAMES[i]),
            Identity::Helper => "@(HelperPkg)".to_string(),
        }
    }
}

/// The `Version` metadatum on a `PackageReference Update`: absent, an explicit
/// empty *clear* (`Version=""`), or a value.
#[derive(Debug, Clone, Copy)]
enum UpdateVersion {
    Absent,
    Clear,
    Val(Value),
}

#[derive(Debug, Clone, Copy)]
enum VersionSpec {
    /// No version at all — a versionless `PackageReference` is a deliberate
    /// conservative-uncertainty case, so this is generated rarely.
    Absent,
    Attr(Value),
    Child(Value),
}

#[derive(Debug, Clone, Copy)]
enum ItemDecl {
    Package {
        identity: Identity,
        version: VersionSpec,
        cond: Cond,
    },
    Framework {
        identity: Identity,
        cond: Cond,
    },
    /// `<HelperPkg …>` — feeds later `@(HelperPkg)` consumers.
    Helper {
        id: usize,
        version: Option<Value>,
        cond: Cond,
    },
    /// `<PackageReference Update="…" …>` — merges onto every prior matching
    /// `Include` (dropped if none), exercising the Include+Update collapse,
    /// including an explicit `Version=""` *clear*.
    PackageUpdate {
        identity: Identity,
        version: UpdateVersion,
        version_override: Option<Value>,
        private_assets: bool,
        cond: Cond,
    },
    /// CPM inputs: conservative uncertainty by design (rare; see module doc).
    CpmVersion {
        id: usize,
        version: Value,
    },
    CpmGlobal {
        id: usize,
        version: Value,
    },
}

impl ItemDecl {
    fn render(self, out: &mut String) {
        match self {
            ItemDecl::Package {
                identity,
                version,
                cond,
            } => {
                let id = identity.render();
                let cond = cond.render();
                match version {
                    VersionSpec::Absent => {
                        let _ = writeln!(out, r#"    <PackageReference Include="{id}"{cond} />"#);
                    }
                    VersionSpec::Attr(v) => {
                        let v = v.render();
                        let _ = writeln!(
                            out,
                            r#"    <PackageReference Include="{id}" Version="{v}"{cond} />"#
                        );
                    }
                    VersionSpec::Child(v) => {
                        let v = v.render();
                        let _ = writeln!(out, r#"    <PackageReference Include="{id}"{cond}>"#);
                        let _ = writeln!(out, "      <Version>{v}</Version>");
                        let _ = writeln!(out, "    </PackageReference>");
                    }
                }
            }
            ItemDecl::Framework { identity, cond } => {
                let id = identity.render();
                let cond = cond.render();
                let _ = writeln!(out, r#"    <FrameworkReference Include="{id}"{cond} />"#);
            }
            ItemDecl::Helper { id, version, cond } => {
                let id = IDS[id];
                let cond = cond.render();
                match version {
                    None => {
                        let _ = writeln!(out, r#"    <HelperPkg Include="{id}"{cond} />"#);
                    }
                    Some(v) => {
                        let v = v.render();
                        let _ = writeln!(
                            out,
                            r#"    <HelperPkg Include="{id}" Version="{v}"{cond} />"#
                        );
                    }
                }
            }
            ItemDecl::PackageUpdate {
                identity,
                version,
                version_override,
                private_assets,
                cond,
            } => {
                let id = identity.render();
                let mut attrs = String::new();
                match version {
                    UpdateVersion::Absent => {}
                    UpdateVersion::Clear => attrs.push_str(r#" Version="""#),
                    UpdateVersion::Val(v) => {
                        let _ = write!(attrs, r#" Version="{}""#, v.render());
                    }
                }
                if let Some(v) = version_override {
                    let _ = write!(attrs, r#" VersionOverride="{}""#, v.render());
                }
                if private_assets {
                    attrs.push_str(r#" PrivateAssets="all""#);
                }
                let _ = writeln!(
                    out,
                    r#"    <PackageReference Update="{id}"{attrs}{} />"#,
                    cond.render()
                );
            }
            ItemDecl::CpmVersion { id, version } => {
                let _ = writeln!(
                    out,
                    r#"    <PackageVersion Include="{}" Version="{}" />"#,
                    IDS[id],
                    version.render()
                );
            }
            ItemDecl::CpmGlobal { id, version } => {
                let _ = writeln!(
                    out,
                    r#"    <GlobalPackageReference Include="{}" Version="{}" />"#,
                    IDS[id],
                    version.render()
                );
            }
        }
    }
}

#[derive(Debug, Clone)]
enum Group {
    Props { cond: Cond, writes: Vec<PropWrite> },
    Items { cond: Cond, items: Vec<ItemDecl> },
}

impl Group {
    fn render(&self, out: &mut String) {
        match self {
            Group::Props { cond, writes } => {
                let _ = writeln!(out, "  <PropertyGroup{}>", cond.render());
                for write in writes {
                    let _ = writeln!(
                        out,
                        "    <{name}{cond}>{value}</{name}>",
                        name = PROP_NAMES[write.name],
                        cond = write.cond.render(),
                        value = write.value.render(),
                    );
                }
                let _ = writeln!(out, "  </PropertyGroup>");
            }
            Group::Items { cond, items } => {
                let _ = writeln!(out, "  <ItemGroup{}>", cond.render());
                for item in items {
                    item.render(out);
                }
                let _ = writeln!(out, "  </ItemGroup>");
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Fixture {
    sdk_groups: Vec<Group>,
    project_groups: Vec<Group>,
}

impl Fixture {
    fn sdk_props_xml(&self) -> String {
        // `Switch` is pinned first so `SwitchEq` conditions are always
        // deterministic in both walkers.
        let mut out = String::from(
            "<Project>\n  <PropertyGroup>\n    <Switch>true</Switch>\n  </PropertyGroup>\n",
        );
        for group in &self.sdk_groups {
            group.render(&mut out);
        }
        out.push_str("</Project>\n");
        out
    }

    fn project_xml(&self) -> String {
        let mut out = String::from("<Project Sdk=\"MySdk\">\n");
        for group in &self.project_groups {
            group.render(&mut out);
        }
        out.push_str("</Project>\n");
        out
    }
}

// ---------------------------------------------------------------------------
// Strategies. Weights keep genuinely-unknowable constructs in the pool (the
// boundary is the point) but rare enough that most cases stay certain and
// exercise the oracle — `most_generated_cases_are_certain` enforces that.
// ---------------------------------------------------------------------------

fn value_strategy() -> impl Strategy<Value = Value> {
    prop_oneof![
        3 => (0..LITERALS.len()).prop_map(Value::Lit),
        2 => (0..PROP_NAMES.len()).prop_map(Value::Prop),
    ]
}

fn cond_strategy() -> impl Strategy<Value = Cond> {
    // `SwitchEq` is weighted up deliberately: a cleanly-false gate on a
    // certain item is the sharpest probe this oracle has (an evaluator that
    // over-resolves it stays *certain* and diverges — the cardinal sin),
    // whereas the `Undef*` variants merely push a case into the skipped
    // uncertain bucket.
    prop_oneof![
        6 => Just(Cond::None),
        6 => proptest::bool::ANY.prop_map(Cond::SwitchEq),
        1 => Just(Cond::UndefEmpty),
        1 => Just(Cond::UndefEqTrue),
    ]
}

fn prop_write_strategy() -> impl Strategy<Value = PropWrite> {
    (0..PROP_NAMES.len(), value_strategy(), cond_strategy())
        .prop_map(|(name, value, cond)| PropWrite { name, value, cond })
}

fn identity_strategy() -> impl Strategy<Value = Identity> {
    prop_oneof![
        5 => (0..IDS.len()).prop_map(Identity::Id),
        2 => (0..IDS.len(), 0..IDS.len()).prop_map(|(a, b)| Identity::TwoIds(a, b)),
        1 => (0..PROP_NAMES.len()).prop_map(Identity::Prop),
        2 => Just(Identity::Helper),
    ]
}

fn update_version_strategy() -> impl Strategy<Value = UpdateVersion> {
    // `Clear` is weighted in deliberately: an `Update Version=""` that erases a
    // prior Include's version is the sharpest probe for the merge's clear
    // handling (getting it wrong keeps a stale version and stays *certain* —
    // the cardinal sin).
    prop_oneof![
        3 => Just(UpdateVersion::Absent),
        2 => Just(UpdateVersion::Clear),
        4 => value_strategy().prop_map(UpdateVersion::Val),
    ]
}

fn version_strategy() -> impl Strategy<Value = VersionSpec> {
    prop_oneof![
        1 => Just(VersionSpec::Absent),
        6 => value_strategy().prop_map(VersionSpec::Attr),
        3 => value_strategy().prop_map(VersionSpec::Child),
    ]
}

fn item_strategy() -> impl Strategy<Value = ItemDecl> {
    prop_oneof![
        6 => (identity_strategy(), version_strategy(), cond_strategy()).prop_map(
            |(identity, version, cond)| ItemDecl::Package {
                identity,
                version,
                cond,
            }
        ),
        2 => (identity_strategy(), cond_strategy())
            .prop_map(|(identity, cond)| ItemDecl::Framework { identity, cond }),
        3 => (
            0..IDS.len(),
            proptest::option::of(value_strategy()),
            cond_strategy()
        )
            .prop_map(|(id, version, cond)| ItemDecl::Helper { id, version, cond }),
        3 => (
            identity_strategy(),
            update_version_strategy(),
            proptest::option::of(value_strategy()),
            proptest::bool::ANY,
            cond_strategy(),
        )
            .prop_map(|(identity, version, version_override, private_assets, cond)| {
                ItemDecl::PackageUpdate {
                    identity,
                    version,
                    version_override,
                    private_assets,
                    cond,
                }
            }),
        1 => (0..IDS.len(), value_strategy())
            .prop_map(|(id, version)| ItemDecl::CpmVersion { id, version }),
        1 => (0..IDS.len(), value_strategy())
            .prop_map(|(id, version)| ItemDecl::CpmGlobal { id, version }),
    ]
}

fn group_strategy() -> impl Strategy<Value = Group> {
    prop_oneof![
        1 => (cond_strategy(), proptest::collection::vec(prop_write_strategy(), 1..3))
            .prop_map(|(cond, writes)| Group::Props { cond, writes }),
        1 => (cond_strategy(), proptest::collection::vec(item_strategy(), 1..4))
            .prop_map(|(cond, items)| Group::Items { cond, items }),
    ]
}

fn fixture_strategy() -> impl Strategy<Value = Fixture> {
    (
        proptest::collection::vec(group_strategy(), 0..3),
        proptest::collection::vec(group_strategy(), 1..4),
    )
        .prop_map(|(sdk_groups, project_groups)| Fixture {
            sdk_groups,
            project_groups,
        })
}

// ---------------------------------------------------------------------------
// Both-ways evaluation.
// ---------------------------------------------------------------------------

/// On-disk fixture: the tempdir keeps everything alive, `proj` and `sdks`
/// are the paths the oracle needs.
struct Laid {
    _dir: tempfile::TempDir,
    proj: PathBuf,
    sdks: PathBuf,
}

/// Lay the fixture out on disk in the canonical `MSBuildSDKsPath` shape
/// (`Sdks/MySdk/Sdk/Sdk.{props,targets}`) shared by both walkers, and parse
/// it with our evaluator.
fn write_and_parse(fixture: &Fixture) -> (Laid, ParsedProject) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    let sdks = root.join("Sdks");
    let sdk_dir = sdks.join("MySdk/Sdk");
    std::fs::create_dir_all(&sdk_dir).expect("mkdir sdk");
    let props = sdk_dir.join("Sdk.props");
    let targets = sdk_dir.join("Sdk.targets");
    std::fs::write(&props, fixture.sdk_props_xml()).expect("write Sdk.props");
    std::fs::write(&targets, "<Project/>").expect("write Sdk.targets");
    std::fs::create_dir_all(root.join("proj")).expect("mkdir proj");
    let proj = root.join("proj/Demo.fsproj");
    let project_xml = fixture.project_xml();
    std::fs::write(&proj, &project_xml).expect("write project");

    let resolver = move |name: &str| {
        if name == "MySdk" {
            Ok(SdkResolution::Single(SdkPaths {
                root: sdk_dir.clone(),
                props: props.clone(),
                targets: targets.clone(),
            }))
        } else {
            Err(SdkResolveError::NotFound)
        }
    };
    // The oracle child gets `MSBuildSDKsPath` as an environment variable
    // (see `run_get_item`), which MSBuild folds in as an initial property;
    // mirror the exact same snapshot on our side.
    let mut environment = common::oracle_environment();
    environment.insert(
        "MSBuildSDKsPath".to_string(),
        sdks.to_string_lossy().into_owned(),
    );
    let parsed = parse_fsproj_with_imports(
        &project_xml,
        &proj,
        &HashMap::new(),
        &environment,
        Some(&resolver),
        None,
    )
    .unwrap_or_else(|e| panic!("our parse: {e}\n{project_xml}"));
    (
        Laid {
            _dir: dir,
            proj,
            sdks,
        },
        parsed,
    )
}

#[derive(Deserialize)]
struct MsbuildItem {
    #[serde(rename = "Identity")]
    identity: String,
    #[serde(flatten)]
    metadata: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Items {
    #[serde(rename = "PackageReference", default)]
    package_reference: Vec<MsbuildItem>,
    #[serde(rename = "FrameworkReference", default)]
    framework_reference: Vec<MsbuildItem>,
}

#[derive(Deserialize)]
struct GetItemOutput {
    #[serde(rename = "Items")]
    items: Items,
}

/// Budget for one `dotnet msbuild` evaluation. A cold one restores packages and
/// walks the whole SDK import chain, which is legitimately minutes, so the bound
/// is far above the harness's per-request default: it is there to stop an
/// evaluation that has *stalled* — blocked on a NuGet lock held by a concurrent
/// run in a sibling worktree, say — from hanging the suite forever, not to police
/// a slow one.
const MSBUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// Mirror of `fsproj_packageref_diff::run_get_item`, plus the
/// `MSBuildSDKsPath` override that points MSBuild at the synthetic SDK. The
/// environment is cleared for the same reason as there: every inherited env
/// var is an MSBuild initial property.
fn run_get_item(proj: &Path, sdks_path: &Path) -> GetItemOutput {
    let mut cmd = Command::new("dotnet");
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
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
    cmd.env("MSBuildSDKsPath", sdks_path);
    cmd.args([
        "msbuild",
        "-nologo",
        "-getItem:PackageReference,FrameworkReference",
    ]);
    cmd.arg(proj);
    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!("dotnet msbuild for {}", proj.display()));
    let stdout = String::from_utf8(out.stdout).expect("msbuild stdout is UTF-8");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("parse -getItem JSON for {}: {e}\n{stdout}", proj.display()))
}

type PkgFields = (String, Vec<(&'static str, Option<String>)>);

fn msbuild_pkg_fields(item: &MsbuildItem) -> PkgFields {
    let g = |k: &str| item.metadata.get(k).filter(|v| !v.is_empty()).cloned();
    (
        item.identity.clone(),
        vec![
            ("Version", g("Version")),
            ("VersionOverride", g("VersionOverride")),
            ("IncludeAssets", g("IncludeAssets")),
            ("ExcludeAssets", g("ExcludeAssets")),
            ("PrivateAssets", g("PrivateAssets")),
        ],
    )
}

fn ours_pkg_fields(pr: &borzoi_msbuild::PackageReference) -> PkgFields {
    (
        pr.id.clone(),
        vec![
            ("Version", pr.version.clone()),
            ("VersionOverride", pr.version_override.clone()),
            ("IncludeAssets", pr.include_assets.clone()),
            ("ExcludeAssets", pr.exclude_assets.clone()),
            ("PrivateAssets", pr.private_assets.clone()),
        ],
    )
}

/// The property body: certain ⇒ both item sets match MSBuild exactly.
/// Returns whether the case was certain (i.e. the oracle actually ran).
fn check_certain_implies_exact(fixture: &Fixture) -> Result<bool, TestCaseError> {
    let (laid, parsed) = write_and_parse(fixture);
    if parsed.package_references_uncertain {
        return Ok(false);
    }
    let msbuild = run_get_item(&laid.proj, &laid.sdks);

    let ours_pkg: Vec<_> = parsed
        .package_references
        .iter()
        .map(ours_pkg_fields)
        .collect();
    let theirs_pkg: Vec<_> = msbuild
        .items
        .package_reference
        .iter()
        .map(msbuild_pkg_fields)
        .collect();
    prop_assert_eq!(
        &ours_pkg,
        &theirs_pkg,
        "claimed certain, but PackageReference capture diverges.\n--- Sdk.props ---\n{}\n--- project ---\n{}",
        fixture.sdk_props_xml(),
        fixture.project_xml()
    );

    let ours_fw: Vec<&str> = parsed
        .framework_references
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    let theirs_fw: Vec<&str> = msbuild
        .items
        .framework_reference
        .iter()
        .map(|i| i.identity.as_str())
        .collect();
    prop_assert_eq!(
        &ours_fw,
        &theirs_fw,
        "claimed certain, but FrameworkReference capture diverges.\n--- Sdk.props ---\n{}\n--- project ---\n{}",
        fixture.sdk_props_xml(),
        fixture.project_xml()
    );
    Ok(true)
}

/// Driven through a manual [`TestRunner`] (rather than the `proptest!` macro)
/// so the run can *prove* it exercised the oracle: uncertain cases skip the
/// comparison, and a run where every case happened to skip would otherwise be
/// indistinguishable from a passing one.
#[test]
fn certain_capture_matches_msbuild() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 64,
        source_file: Some(file!()),
        ..ProptestConfig::default()
    });
    let oracle_hits = std::cell::Cell::new(0u32);
    runner
        .run(&fixture_strategy(), |fixture| {
            if check_certain_implies_exact(&fixture)? {
                oracle_hits.set(oracle_hits.get() + 1);
            }
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    // With the generator's weights roughly half the cases are certain
    // (`most_generated_cases_are_certain` pins the stable lower bound); a run
    // with *zero* oracle comparisons means the harness itself is broken.
    assert!(
        oracle_hits.get() > 0,
        "no generated case reached the MSBuild oracle — the property tested nothing"
    );
}

/// The property above skips uncertain cases, so on its own it could rot into
/// testing (almost) nothing if the generator (or a regression) made
/// uncertainty the norm. Sample the strategy deterministically — our parser
/// only, no oracle — and require a healthy certain fraction.
#[test]
fn most_generated_cases_are_certain() {
    let mut runner = TestRunner::deterministic();
    let strategy = fixture_strategy();
    let total = 128;
    let mut certain = 0;
    for _ in 0..total {
        let fixture = strategy
            .new_tree(&mut runner)
            .expect("generate fixture")
            .current();
        let (_laid, parsed) = write_and_parse(&fixture);
        if !parsed.package_references_uncertain {
            certain += 1;
        }
    }
    assert!(
        certain * 4 >= total,
        "only {certain}/{total} generated cases were certain; the differential \
         property is barely exercising the oracle — rebalance the generator \
         weights or investigate an over-degrading evaluator"
    );
}

/// Deterministic end-to-end pin of the harness itself: a hand-built fixture
/// that is certain by construction, with a cleanly-false gate MSBuild must
/// also skip. If the plumbing rots (SDK layout, `MSBuildSDKsPath` env,
/// `-getItem` JSON shape), or the evaluator over-resolves a cleanly-false
/// item group (the exact mutation this canary was validated against), this
/// fails without depending on generator luck. `Ok(true)` = the oracle ran
/// and matched.
#[test]
fn known_certain_fixture_reaches_oracle_and_matches() {
    let fixture = Fixture {
        sdk_groups: vec![Group::Items {
            cond: Cond::None,
            items: vec![ItemDecl::Framework {
                identity: Identity::Id(2),
                cond: Cond::None,
            }],
        }],
        project_groups: vec![
            Group::Items {
                cond: Cond::SwitchEq(false),
                items: vec![ItemDecl::Package {
                    identity: Identity::Id(0),
                    version: VersionSpec::Attr(Value::Lit(0)),
                    cond: Cond::None,
                }],
            },
            Group::Items {
                cond: Cond::None,
                items: vec![ItemDecl::Package {
                    identity: Identity::Id(1),
                    version: VersionSpec::Child(Value::Lit(1)),
                    cond: Cond::None,
                }],
            },
        ],
    };
    assert!(
        check_certain_implies_exact(&fixture).expect("known-certain fixture must match MSBuild"),
        "known-certain fixture was reported uncertain — the oracle never ran"
    );
}

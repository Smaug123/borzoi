//! The B₁ differential of `docs/nuget-dgspec-boundary-plan.md` (stage 4):
//! [`resolve_dgspec`] against NuGet's restore over the same spec (B₀, the
//! oracle's `restoreDgSpec` op), run by the SDK's own NuGet.
//!
//! The contract is certain-implies-exact:
//!
//!   `resolve_dgspec` commits  ⟹  B₀ succeeds, and each framework's target in
//!                                 its assets file lists exactly the packages
//!                                 B₁ resolved for that framework.
//!
//! A decline makes no claim. B₀ restores straight from the packages folder the
//! resolver reads, with no package anywhere else, as `restore.rs`'s offline
//! restore does: the specs name that folder as `packagesPath` and only empty
//! or missing folders as sources.
//!
//! Every spec here is handed to B₁ with the identity of the NuGet that graded
//! it, so B₁ commits only because that identity is on the verified list, and
//! [`the_verified_list_is_the_graded_nuget`] holds the list to exactly the
//! identity this suite grades.
//!
//! The resolver's own graph rules are graded by `resolver_diff.rs` over far
//! larger graphs. What this file grades is the plumbing from the spec to the
//! resolver: which fields are modelled, which are inert, and which decline.
//! So the generator's universes are small and its specs vary every field the
//! resolver claims is inert, and the sweep requires each such variation to be
//! graded on a committed spec at least once.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use borzoi_nuget::dgspec::{
    DependencyGraphSpec, DgSpecResolveDecline, NuGetIdentity, ResolvedFramework, UnmodelledField,
    parse_dgspec, resolve_dgspec, verified_nuget_identities,
};
use borzoi_nuget::{
    DirectPackageRequirement, NuGetFramework, NuGetVersion, PackageId, PackageIdentity,
    PackagePaths, VersionRange, resolve_offline,
};
use borzoi_oracle_harness::manifest::{Manifest, check};
use serde_json::{Map, Value, json};

use crate::common::{Oracle, SplitMix64};

// ============================================================================
// The universe: packages in a packages folder
// ============================================================================

#[derive(Debug, Clone)]
struct Group {
    /// `None` renders a group with no `targetFramework` (NuGet's `Any`).
    tfm: Option<&'static str>,
    deps: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct Pkg {
    id: String,
    version: String,
    groups: Vec<Group>,
    /// Package-relative asset paths, written as empty files.
    files: Vec<String>,
    /// The `<metadata>` element's `minClientVersion`.
    min_client_version: Option<&'static str>,
    /// Each `<packageType>`'s attributes, as written.
    package_types: Vec<&'static str>,
    /// More `<metadata>` children, as written.
    metadata: Vec<&'static str>,
    /// The `<version>` element's text, when it is not [`Self::version`].
    version_text: Option<&'static str>,
    /// The `.nupkg.metadata` commit marker, when it is not a valid one.
    marker: Option<&'static str>,
    /// The whole nuspec, in place of the one [`Pkg::nuspec`] renders.
    raw_nuspec: Option<&'static str>,
}

impl Pkg {
    fn new(id: &str, version: &str, groups: Vec<Group>) -> Pkg {
        Pkg {
            id: id.to_owned(),
            version: version.to_owned(),
            groups,
            files: Vec::new(),
            min_client_version: None,
            package_types: Vec::new(),
            metadata: Vec::new(),
            version_text: None,
            marker: None,
            raw_nuspec: None,
        }
    }

    fn with_files(mut self, files: &[&str]) -> Pkg {
        self.files = files.iter().map(|file| (*file).to_owned()).collect();
        self
    }

    fn nuspec(&self) -> String {
        let groups: String = self
            .groups
            .iter()
            .map(|group| {
                let tfm = group
                    .tfm
                    .map(|tfm| format!(r#" targetFramework="{tfm}""#))
                    .unwrap_or_default();
                let deps: String = group
                    .deps
                    .iter()
                    .map(|(id, range)| format!(r#"<dependency id="{id}" version="{range}" />"#))
                    .collect();
                format!("<group{tfm}>{deps}</group>")
            })
            .collect();
        let min_client_version = self
            .min_client_version
            .map(|version| format!(r#" minClientVersion="{version}""#))
            .unwrap_or_default();
        let package_types = if self.package_types.is_empty() {
            String::new()
        } else {
            let types: String = self
                .package_types
                .iter()
                .map(|attributes| format!("<packageType {attributes} />"))
                .collect();
            format!("<packageTypes>{types}</packageTypes>")
        };
        format!(
            r#"<?xml version="1.0"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata{min_client_version}>
    <id>{}</id>
    <version>{}</version>
    <authors>a</authors>
    <description>d</description>
    {package_types}
    {}
    <dependencies>{groups}</dependencies>
  </metadata>
</package>
"#,
            self.id,
            self.version_text.unwrap_or(&self.version),
            self.metadata.concat(),
        )
    }
}

/// Write `packages` into a packages folder at `root` as NuGet commits them:
/// the nuspec, and the `.nupkg.metadata` marker NuGet reads to know the package
/// is installed (it refuses one without a `contentHash`).
fn install(root: &Path, packages: &[Pkg]) {
    for pkg in packages {
        let identity = PackageIdentity::new(
            PackageId::parse(&pkg.id).expect("a package id"),
            NuGetVersion::parse(&pkg.version).expect("a version"),
        );
        let paths = PackagePaths::new(root, &identity);
        fs::create_dir_all(&paths.package_dir).expect("package dir");
        let nuspec = pkg.raw_nuspec.map_or_else(|| pkg.nuspec(), str::to_owned);
        fs::write(&paths.nuspec_path, nuspec).expect("nuspec");
        for file in &pkg.files {
            let path = paths.package_dir.join(file);
            fs::create_dir_all(path.parent().expect("an asset is in a folder")).expect("folder");
            fs::write(&path, b"").expect("asset");
        }
        fs::write(
            &paths.metadata_path,
            pkg.marker
                .unwrap_or(r#"{"version":2,"contentHash":"AAAA","source":null}"#),
        )
        .expect("commit marker");
    }
}

// ============================================================================
// The spec
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// An empty folder, as `restore.rs` passes.
    Empty,
    /// A folder that does not exist, as an SDK's `library-packs` source can be.
    Missing,
}

#[derive(Debug, Clone)]
struct Dep {
    id: String,
    range: String,
    include: Option<&'static str>,
    suppress_parent: Option<&'static str>,
    auto_referenced: bool,
    /// The dependency's own `noWarn`.
    no_warn: Vec<&'static str>,
}

impl Dep {
    fn new(id: &str, range: &str) -> Dep {
        Dep {
            id: id.to_owned(),
            range: range.to_owned(),
            include: None,
            suppress_parent: None,
            auto_referenced: false,
            no_warn: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct Framework {
    alias: &'static str,
    deps: Vec<Dep>,
    /// The SDK's asset-target fallback list, `net461` to `net481`.
    asset_target_fallback: bool,
    framework_reference: bool,
    /// `downloadDependencies`: `(id, range)`.
    downloads: Vec<(&'static str, &'static str)>,
}

impl Framework {
    fn new(alias: &'static str, deps: Vec<Dep>) -> Framework {
        Framework {
            alias,
            deps,
            asset_target_fallback: true,
            framework_reference: false,
            downloads: Vec::new(),
        }
    }
}

/// A spec, in the vocabulary the generator varies. [`Shape::render`] writes it
/// as the SDK would.
#[derive(Debug, Clone)]
struct Shape {
    project_name: String,
    frameworks: Vec<Framework>,
    sources: Vec<Source>,
    all_warnings_as_errors: bool,
    warn_as_error: Vec<&'static str>,
    warn_not_as_error: Vec<&'static str>,
    no_warn: Vec<&'static str>,
    sdk_analysis_level: Option<&'static str>,
    /// `enableAudit`: `None` leaves `restoreAuditProperties` out.
    enable_audit: Option<&'static str>,
    /// `restorePackagesWithLockFile`.
    lock_file: Option<&'static str>,
    /// The contents of one config file the spec names, `{empty}` standing
    /// for the empty source folder; `None` names none.
    config: Option<&'static str>,
}

impl Shape {
    fn new(frameworks: Vec<Framework>) -> Shape {
        Shape {
            project_name: "App".to_owned(),
            frameworks,
            sources: vec![Source::Empty],
            all_warnings_as_errors: false,
            warn_as_error: Vec::new(),
            warn_not_as_error: Vec::new(),
            no_warn: Vec::new(),
            sdk_analysis_level: Some("10.0.400"),
            enable_audit: Some("false"),
            lock_file: Some("false"),
            config: None,
        }
    }

    /// The spec, naming folders under `dir` (`gp` is the packages folder).
    fn render(&self, dir: &Path) -> String {
        let path = |name: &str| dir.join(name).to_str().expect("UTF-8").to_owned();
        let project = path("App.fsproj");

        let mut sources = Map::new();
        for source in &self.sources {
            let folder = match source {
                Source::Empty => {
                    fs::create_dir_all(dir.join("empty")).expect("empty source");
                    path("empty")
                }
                Source::Missing => path("missing"),
            };
            sources.insert(folder, json!({}));
        }
        let config_file_paths: Vec<String> = match self.config {
            Some(config) => {
                fs::create_dir_all(dir.join("empty")).expect("empty source");
                fs::write(
                    dir.join("NuGet.Config"),
                    config.replace("{empty}", &path("empty")),
                )
                .expect("config");
                vec![path("NuGet.Config")]
            }
            None => Vec::new(),
        };

        let aliases: Vec<&str> = self.frameworks.iter().map(|f| f.alias).collect();
        let mut restore = json!({
            "projectUniqueName": project,
            "projectName": self.project_name,
            "projectPath": project,
            "packagesPath": path("gp"),
            "outputPath": format!("{}/", path("obj")),
            "projectStyle": "PackageReference",
            "crossTargeting": aliases.len() > 1,
            "configFilePaths": config_file_paths,
            "originalTargetFrameworks": aliases,
            "sources": sources,
            "frameworks": aliases
                .iter()
                .map(|alias| {
                    (
                        alias.to_string(),
                        json!({"targetAlias": alias, "projectReferences": {}}),
                    )
                })
                .collect::<Map<_, _>>(),
            "warningProperties": {
                "allWarningsAsErrors": self.all_warnings_as_errors,
                "warnAsError": self.warn_as_error,
                "warnNotAsError": self.warn_not_as_error,
                "noWarn": self.no_warn,
            },
            "restoreLockProperties": {"nuGetLockFilePath": path("obj/packages.lock.json")},
        });
        if let Some(value) = self.lock_file {
            restore["restoreLockProperties"]["restorePackagesWithLockFile"] = json!(value);
        }
        if let Some(value) = self.enable_audit {
            restore["restoreAuditProperties"] = json!({"enableAudit": value});
        }
        if let Some(level) = self.sdk_analysis_level {
            restore["SdkAnalysisLevel"] = json!(level);
        }

        let frameworks: Map<String, Value> = self
            .frameworks
            .iter()
            .map(|framework| {
                let dependencies: Map<String, Value> = framework
                    .deps
                    .iter()
                    .map(|dep| {
                        let mut value = json!({"target": "Package", "version": dep.range});
                        if let Some(include) = dep.include {
                            value["include"] = json!(include);
                        }
                        if let Some(suppress) = dep.suppress_parent {
                            value["suppressParent"] = json!(suppress);
                        }
                        if dep.auto_referenced {
                            value["autoReferenced"] = json!(true);
                        }
                        if !dep.no_warn.is_empty() {
                            value["noWarn"] = json!(dep.no_warn);
                        }
                        (dep.id.clone(), value)
                    })
                    .collect();
                let mut value = json!({
                    "targetAlias": framework.alias,
                    "dependencies": dependencies,
                    "warn": true,
                });
                if framework.asset_target_fallback {
                    value["imports"] = json!([
                        "net461", "net462", "net47", "net471", "net472", "net48", "net481"
                    ]);
                    value["assetTargetFallback"] = json!(true);
                }
                if !framework.downloads.is_empty() {
                    value["downloadDependencies"] = framework
                        .downloads
                        .iter()
                        .map(|(id, range)| json!({"name": id, "version": range}))
                        .collect();
                }
                if framework.framework_reference {
                    value["frameworkReferences"] =
                        json!({"Microsoft.NETCore.App": {"privateAssets": "all"}});
                }
                (framework.alias.to_owned(), value)
            })
            .collect();

        let spec = json!({
            "format": 1,
            "restore": {project.clone(): {}},
            "projects": {project.clone(): {
                "version": "1.0.0",
                "restore": restore,
                "frameworks": frameworks,
            }},
        });
        serde_json::to_string_pretty(&spec).expect("serialise")
    }
}

// ============================================================================
// The comparison
// ============================================================================

/// `(lower-cased id, normalised version)`, the currency both sides are
/// compared in.
type PackageSet = BTreeSet<(String, String)>;

fn closure_set(framework: &ResolvedFramework) -> PackageSet {
    framework
        .closure
        .packages
        .iter()
        .map(|package| {
            (
                package.identity.id.as_str().to_ascii_lowercase(),
                package.identity.version.to_normalized_string(),
            )
        })
        .collect()
}

/// The key both sides' closures are compared under: the framework's short
/// folder name.
///
/// Not the target's name in the assets file, which depends on the file's
/// version: version 4 names a target by its alias, version 3 by NuGet's
/// `NuGetFramework.ToString()` (the short folder name from `net5.0` on, the
/// long name, `.NETStandard,Version=v2.0`, before it). Which version restore
/// writes moves with the spec's `SdkAnalysisLevel`; the closure does not.
fn framework_key(framework: &NuGetFramework) -> String {
    framework
        .short_folder_name()
        .expect("a project framework has a short folder name")
}

/// What B₀ did with one spec.
#[derive(Debug)]
struct Restored {
    identity: NuGetIdentity,
    success: bool,
    /// The assets file's `version`.
    version: u64,
    /// Each target's package libraries, by [`framework_key`].
    targets: BTreeMap<String, PackageSet>,
    /// Every log code, sorted.
    codes: Vec<String>,
}

fn restore(oracle: &mut Oracle, text: &str) -> Restored {
    let response = oracle.request(&json!({"op": "restoreDgSpec", "text": text}));
    let identity = NuGetIdentity::new(
        response["nugetCommands"]["informationalVersion"]
            .as_str()
            .expect("the oracle names its NuGet"),
    );
    if response["ok"] == false {
        return Restored {
            identity,
            success: false,
            version: 0,
            targets: BTreeMap::new(),
            codes: vec![format!(
                "threw {}",
                response["exceptionType"]
                    .as_str()
                    .expect("an exception type")
            )],
        };
    }
    let results = response["results"].as_array().expect("results");
    let [result] = results.as_slice() else {
        panic!("one project restores, not {}", results.len());
    };
    let assets: Value =
        serde_json::from_str(result["assets"].as_str().expect("assets")).expect("assets JSON");
    let targets = assets["targets"]
        .as_object()
        .expect("targets")
        .iter()
        .map(|(name, libraries)| {
            let packages = libraries
                .as_object()
                .expect("a target maps libraries")
                .iter()
                .filter(|(_, library)| library["type"] == "package")
                .map(|(key, _)| {
                    let (id, version) = key.split_once('/').expect("`id/version`");
                    (
                        id.to_ascii_lowercase(),
                        NuGetVersion::parse(version)
                            .expect("a version")
                            .to_normalized_string(),
                    )
                })
                .collect();
            let framework = NuGetFramework::parse(name)
                .unwrap_or_else(|e| panic!("target {name:?} names a framework: {e}"));
            (framework_key(&framework), packages)
        })
        .collect();
    let mut codes: Vec<String> = assets["logs"]
        .as_array()
        .map(|logs| {
            logs.iter()
                .map(|log| log["code"].as_str().unwrap_or("?").to_owned())
                .collect()
        })
        .unwrap_or_default();
    codes.sort();
    codes.dedup();
    Restored {
        identity,
        success: result["success"].as_bool().expect("success"),
        version: assets["version"].as_u64().expect("an assets version"),
        targets,
        codes,
    }
}

/// Both sides' verdicts on one spec.
struct Graded {
    ours: Result<Vec<ResolvedFramework>, DgSpecResolveDecline>,
    theirs: Restored,
}

/// Restore `packages` and `shape` with B₀, then resolve them with B₁ under
/// the NuGet B₀ reports, and check certain-implies-exact.
fn grade(oracle: &mut Oracle, packages: &[Pkg], shape: &Shape) -> Graded {
    let dir = tempfile::tempdir().expect("temp dir");
    install(&dir.path().join("gp"), packages);
    let text = shape.render(dir.path());
    let spec = parse_dgspec(&text)
        .unwrap_or_else(|decline| panic!("the generator left the reader's vocabulary: {decline}"));
    grade_spec(oracle, &text, &spec, &format!("{shape:#?}\n{packages:#?}"))
}

/// [`grade`] for a spec already rendered (into a directory that holds its
/// packages folder) and parsed. `description` goes into a failure message.
fn grade_spec(
    oracle: &mut Oracle,
    text: &str,
    spec: &DependencyGraphSpec,
    description: &str,
) -> Graded {
    let theirs = restore(oracle, text);
    let ours = resolve_dgspec(&theirs.identity, spec, &project_of(spec));
    if let Ok(frameworks) = &ours {
        let context = || format!("{description}\nB₀: {theirs:#?}");
        assert!(theirs.success, "B₁ committed, but B₀ fails:\n{}", context());
        assert_eq!(
            by_framework(frameworks),
            theirs.targets,
            "B₁'s closures are not B₀'s:\n{}",
            context()
        );
        // Each framework's prediction, where B₁ makes one, is exactly what
        // restore logs (B₀'s log codes are the project's, all frameworks
        // together).
        if let Some(predicted) = frameworks
            .iter()
            .map(|framework| framework.warnings.clone())
            .collect::<Option<Vec<_>>>()
        {
            let predicted: BTreeSet<String> = predicted
                .into_iter()
                .flatten()
                .map(|code| code.as_str().to_owned())
                .collect();
            let logged: BTreeSet<String> = theirs.codes.iter().cloned().collect();
            assert_eq!(
                predicted,
                logged,
                "B₁'s warnings are not B₀'s:\n{}",
                context()
            );
        }
    }
    Graded { ours, theirs }
}

/// The unique name of the spec's one project.
fn project_of(spec: &DependencyGraphSpec) -> String {
    let [project] = spec.projects.as_slice() else {
        panic!("one project");
    };
    project.restore.project_unique_name.clone()
}

fn by_framework(frameworks: &[ResolvedFramework]) -> BTreeMap<String, PackageSet> {
    frameworks
        .iter()
        .map(|framework| (framework_key(&framework.framework), closure_set(framework)))
        .collect()
}

fn committed(graded: &Graded) -> BTreeMap<String, PackageSet> {
    match &graded.ours {
        Ok(frameworks) => by_framework(frameworks),
        Err(decline) => panic!("B₁ declined: {decline}"),
    }
}

fn set(packages: &[(&str, &str)]) -> PackageSet {
    packages
        .iter()
        .map(|(id, version)| (id.to_ascii_lowercase(), (*version).to_owned()))
        .collect()
}

fn group(tfm: Option<&'static str>, deps: &[(&str, &str)]) -> Group {
    Group {
        tfm,
        deps: deps
            .iter()
            .map(|(id, range)| ((*id).to_owned(), (*range).to_owned()))
            .collect(),
    }
}

// ============================================================================
// Hand cases
// ============================================================================

/// A package with only a `net461` dependency group contributes that group's
/// dependencies to a `net8.0` project through the asset-target fallback list,
/// and none without it.
#[test]
fn the_asset_target_fallback_list_selects_dependency_groups() {
    let mut oracle = Oracle::spawn();
    let packages = [
        Pkg::new("A", "1.0.0", vec![group(Some("net461"), &[("B", "2.0.0")])]),
        Pkg::new("B", "2.0.0", vec![]),
    ];
    let with = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    let graded = grade(&mut oracle, &packages, &with);
    assert_eq!(
        committed(&graded)["net8.0"],
        set(&[("A", "1.0.0"), ("B", "2.0.0")])
    );

    let mut without = with.clone();
    without.frameworks[0].asset_target_fallback = false;
    let graded = grade(&mut oracle, &packages, &without);
    assert_eq!(committed(&graded)["net8.0"], set(&[("A", "1.0.0")]));
}

/// A group for the project's own framework wins even when it is empty: the
/// fallback list is consulted only when no group suits the framework.
#[test]
fn an_empty_group_for_the_framework_shadows_the_fallback() {
    let mut oracle = Oracle::spawn();
    let packages = [
        Pkg::new(
            "A",
            "1.0.0",
            vec![
                group(Some("net461"), &[("B", "2.0.0")]),
                group(Some("netstandard2.0"), &[]),
            ],
        ),
        Pkg::new("B", "2.0.0", vec![]),
    ];
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    let graded = grade(&mut oracle, &packages, &shape);
    assert_eq!(committed(&graded)["net8.0"], set(&[("A", "1.0.0")]));
}

/// A config file that only lists sources, like a user's default
/// `NuGet.Config`.
const SOURCES_CONFIG: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <!-- the user's feeds -->
  <packageSources>
    <clear />
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" protocolVersion="3" />
    <add key="empty" value="{empty}" />
  </packageSources>
  <disabledPackageSources><add key="nuget.org" value="true" /></disabledPackageSources>
</configuration>
"#;

/// Package source mapping that maps no package to any source.
const MAPPING_CONFIG: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources><clear /><add key="empty" value="{empty}" /></packageSources>
  <packageSourceMapping>
    <packageSource key="empty"><package pattern="NoSuchPackage.*" /></packageSource>
  </packageSourceMapping>
</configuration>
"#;

/// Restore loads every config file the spec names. One that only lists sources
/// commits. Package source mapping does not filter the packages folder, but
/// B₁ admits no mapping, since restore fails on a malformed one (an empty
/// pattern) as it does on a file that is not XML.
#[test]
fn config_files_are_held_to_a_closed_vocabulary() {
    let mut oracle = Oracle::spawn();
    let packages = [Pkg::new("A", "1.0.0", vec![])];
    let mut shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    shape.config = Some(SOURCES_CONFIG);
    let graded = grade(&mut oracle, &packages, &shape);
    assert_eq!(committed(&graded)["net8.0"], set(&[("A", "1.0.0")]));

    let empty_pattern = r#"<configuration>
  <packageSources><clear /><add key="empty" value="{empty}" /></packageSources>
  <packageSourceMapping><packageSource key="empty"><package pattern="" /></packageSource></packageSourceMapping>
</configuration>"#;
    for (config, restores) in [
        (MAPPING_CONFIG, true),
        (empty_pattern, false),
        ("<configuration>", false),
    ] {
        shape.config = Some(config);
        let graded = grade(&mut oracle, &packages, &shape);
        assert_eq!(graded.theirs.success, restores, "{config}");
        match graded.ours {
            Err(DgSpecResolveDecline::Unmodelled { fields }) => {
                assert_eq!(fields, BTreeSet::from([UnmodelledField::Config]))
            }
            other => panic!("{config}: B₁ must decline on the config, not {other:?}"),
        }
    }
}

/// A source folder that does not exist is never read when every package is in
/// the packages folder: an SDK's `library-packs` source can be one.
#[test]
fn a_missing_source_folder_is_never_read() {
    let mut oracle = Oracle::spawn();
    let packages = [Pkg::new("A", "1.0.0", vec![])];
    let mut shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    shape.sources = vec![Source::Missing, Source::Empty];
    let graded = grade(&mut oracle, &packages, &shape);
    assert_eq!(committed(&graded)["net8.0"], set(&[("A", "1.0.0")]));
}

/// Restore fails as a whole when one framework fails, so B₁ declines the whole
/// project even though the other framework resolves.
#[test]
fn one_failing_framework_fails_the_project() {
    let mut oracle = Oracle::spawn();
    let packages = [
        Pkg::new("A", "1.0.0", vec![group(None, &[("C", "[1.0.0]")])]),
        Pkg::new("B", "1.0.0", vec![group(None, &[("C", "[2.0.0]")])]),
        Pkg::new("C", "1.0.0", vec![]),
        Pkg::new("C", "2.0.0", vec![]),
    ];
    let shape = Shape::new(vec![
        Framework::new("net8.0", vec![Dep::new("A", "[1.0.0, )")]),
        Framework::new(
            "netstandard2.0",
            vec![Dep::new("A", "[1.0.0, )"), Dep::new("B", "[1.0.0, )")],
        ),
    ]);
    let graded = grade(&mut oracle, &packages, &shape);
    assert!(!graded.theirs.success, "B₀ fails on the conflict");
    match graded.ours {
        Err(DgSpecResolveDecline::Resolve { target_alias, .. }) => {
            assert_eq!(target_alias, "netstandard2.0")
        }
        other => panic!("B₁ must decline on netstandard2.0, not {other:?}"),
    }
}

/// Restore warns (`NU1608`) when a direct reference eclipses a dependency's
/// upper bound. B₁ predicts the warning, and declines where a policy promotes
/// it, which fails the restore whose closure the resolver computes; a policy
/// that demotes or suppresses it commits again.
#[test]
fn a_promoted_warning_fails_a_restore_the_resolver_would_commit() {
    let mut oracle = Oracle::spawn();
    let packages = [
        Pkg::new("A", "1.0.0", vec![group(None, &[("B", "[1.0.0, 2.0.0)")])]),
        Pkg::new("B", "1.0.0", vec![]),
        Pkg::new("B", "2.0.0", vec![]),
    ];
    let mut shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )"), Dep::new("B", "[2.0.0, )")],
    )]);
    let graded = grade(&mut oracle, &packages, &shape);
    assert_eq!(
        committed(&graded)["net8.0"],
        set(&[("A", "1.0.0"), ("B", "2.0.0")])
    );
    assert_eq!(graded.theirs.codes, ["NU1608"], "restore warns");

    let promotions: [fn(&mut Shape); 2] = [
        |shape| shape.warn_as_error = vec!["NU1608"],
        |shape| shape.all_warnings_as_errors = true,
    ];
    for promote in promotions {
        promote(&mut shape);
        let graded = grade(&mut oracle, &packages, &shape);
        assert!(!graded.theirs.success, "the promoted warning fails restore");
        match graded.ours {
            Err(DgSpecResolveDecline::PromotedWarning {
                code: Some(code), ..
            }) => assert_eq!(code.as_str(), "NU1608"),
            other => panic!("B₁ must decline on the promoted NU1608, not {other:?}"),
        }
        shape.warn_as_error.clear();
        shape.all_warnings_as_errors = false;
    }

    shape.all_warnings_as_errors = true;
    for (no_warn, not_as_error) in [(vec!["NU1608"], vec![]), (vec![], vec!["NU1608"])] {
        shape.no_warn = no_warn;
        shape.warn_not_as_error = not_as_error;
        let graded = grade(&mut oracle, &packages, &shape);
        assert!(graded.ours.is_ok(), "{:?}", graded.ours);
    }
}

/// A version below a dependency's lower bound, which a nearer edge can settle
/// on, logs nothing: `NU1608` is about the upper bound only.
#[test]
fn a_version_below_a_dependencys_lower_bound_logs_nothing() {
    let mut oracle = Oracle::spawn();
    let packages = [
        Pkg::new("A", "1.0.0", vec![group(None, &[("B", "(1.0.0, )")])]),
        Pkg::new("B", "1.0.0", vec![]),
        Pkg::new("B", "2.0.0", vec![]),
    ];
    let mut shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )"), Dep::new("B", "[1.0.0, )")],
    )]);
    shape.all_warnings_as_errors = true;
    let graded = grade(&mut oracle, &packages, &shape);
    assert_eq!(
        committed(&graded)["net8.0"],
        set(&[("A", "1.0.0"), ("B", "1.0.0")])
    );
    assert!(graded.theirs.codes.is_empty(), "{:?}", graded.theirs.codes);
}

/// A package whose only assets are for a framework the project cannot
/// consume fails restore's compatibility check (`NU1202`), so B₁ declines it.
/// Through the asset-target fallback list restore instead takes the assets
/// with a warning (`NU1701`); B₁ does not model which assets the list admits,
/// and declines that too. The same package with assets the framework compiles
/// against commits.
#[test]
fn a_package_without_assets_for_the_framework_declines() {
    let mut oracle = Oracle::spawn();
    let lib_net48 = |groups| Pkg::new("A", "1.0.0", groups).with_files(&["lib/net48/A.dll"]);
    let cases = [
        ("netstandard2.0", false, vec![], "NU1202"),
        ("net472", false, vec![], "NU1202"),
        ("net8.0", true, vec![], "NU1701"),
    ];
    for (alias, fallback, groups, code) in cases {
        let packages = [lib_net48(groups)];
        let mut framework = Framework::new(alias, vec![Dep::new("A", "[1.0.0, )")]);
        framework.asset_target_fallback = fallback;
        let graded = grade(&mut oracle, &packages, &Shape::new(vec![framework]));
        assert_eq!(graded.theirs.codes, [code], "{alias}");
        assert_eq!(graded.theirs.success, code == "NU1701", "{alias}");
        assert!(
            matches!(
                graded.ours,
                Err(DgSpecResolveDecline::CompatibilityUnproven { .. })
            ),
            "{alias}: {:?}",
            graded.ours
        );
    }
    let packages = [Pkg::new("A", "1.0.0", vec![]).with_files(&["lib/netstandard2.0/A.dll"])];
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    let graded = grade(&mut oracle, &packages, &shape);
    assert_eq!(committed(&graded)["net8.0"], set(&[("A", "1.0.0")]));
}

/// Restore validates a package by its nuspec metadata even from a warm cache:
/// it refuses a `DotnetTool` package in a `PackageReference` project, a
/// package type whose version is malformed, and a package whose minimum client
/// version is above its own. B₁ declines all three, and admits a `Dependency`
/// type and a minimum below the restoring NuGet.
#[test]
fn restore_validated_metadata_declines_outside_the_admitted_domain() {
    let mut oracle = Oracle::spawn();
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    let with = |min_client_version, package_types: &[&'static str]| {
        let mut package = Pkg::new("A", "1.0.0", vec![]).with_files(&["lib/net8.0/A.dll"]);
        package.min_client_version = min_client_version;
        package.package_types = package_types.to_vec();
        [package]
    };
    for (packages, refused) in [
        (with(None, &[r#"name="DotnetTool""#]), true),
        (with(None, &[r#"name="Dependency" version="1""#]), true),
        (with(Some("99.0.0"), &[]), true),
        (with(None, &[r#"name="Dependency""#]), false),
        (with(Some("2.12"), &[]), false),
    ] {
        let graded = grade(&mut oracle, &packages, &shape);
        assert_eq!(!graded.theirs.success, refused, "{packages:?}");
        if refused {
            assert!(
                matches!(
                    graded.ours,
                    Err(DgSpecResolveDecline::PackageMetadata { .. })
                ),
                "{:?}",
                graded.ours
            );
        } else {
            assert_eq!(committed(&graded)["net8.0"], set(&[("A", "1.0.0")]));
        }
    }
}

/// Each descriptive `<metadata>` element, malformed ones included, is one
/// restore never reads: alone on a package, it commits, and B₀ agrees. So do a
/// commit marker naming the source the package came from, and the one a
/// Nix-built cache writes, with no version and an empty hash. (The generator
/// writes all of these too, in combination; this grades each one for certain.)
#[test]
fn descriptive_metadata_is_inert() {
    let mut oracle = Oracle::spawn();
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    for metadata in DESCRIPTIVE_METADATA {
        let mut package = Pkg::new("A", "1.0.0", vec![]).with_files(&["lib/net8.0/A.dll"]);
        package.metadata = vec![metadata];
        let graded = grade(&mut oracle, &[package], &shape);
        assert!(graded.ours.is_ok(), "{metadata}: {:?}", graded.ours);
    }
    for marker in [
        r#"{"version":2,"contentHash":"AAAA","source":"https://api.nuget.org/v3/index.json"}"#,
        r#"{ "contentHash": "" }"#,
    ] {
        let mut package = Pkg::new("A", "1.0.0", vec![]).with_files(&["lib/net8.0/A.dll"]);
        package.marker = Some(marker);
        let graded = grade(&mut oracle, &[package], &shape);
        assert!(graded.ours.is_ok(), "{marker}: {:?}", graded.ours);
    }
}

/// Restore throws on a commit marker without a content hash or with a bad
/// value under a repeated key (which a last-wins reader would drop), and on
/// content-file
/// rules it rejects; B₁ declines both, the second because it admits no
/// `contentFiles` at all.
#[test]
fn malformed_markers_and_unmodelled_metadata_decline() {
    let mut oracle = Oracle::spawn();
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    let base = || Pkg::new("A", "1.0.0", vec![]).with_files(&["lib/net8.0/A.dll"]);
    let mut no_hash = base();
    no_hash.marker = Some("{}");
    let mut repeated = base();
    repeated.marker = Some(r#"{"version":"bad","version":2,"contentHash":"AAAA"}"#);
    let mut content = base().with_files(&["lib/net8.0/A.dll", "contentFiles/cs/net8.0/A.cs"]);
    content.metadata = vec![DECLINED_METADATA[0]];
    for package in [no_hash, repeated, content] {
        let graded = grade(&mut oracle, std::slice::from_ref(&package), &shape);
        assert!(!graded.theirs.success, "{package:?}: {:?}", graded.theirs);
        assert!(
            matches!(
                graded.ours,
                Err(DgSpecResolveDecline::PackageMetadata { .. })
            ),
            "{:?}",
            graded.ours
        );
    }
}

/// Restore throws on a nuspec without an `<id>` or a `<version>`, or without
/// `<metadata>` at all, or with every element prefixed and no default
/// namespace (it looks for the identity in the default namespace), or behind
/// another element named `metadata` (it reads the first by name); B₁ requires
/// the identity exactly once, in the one `<metadata>`, in the default
/// namespace.
#[test]
fn a_nuspec_must_state_its_identity() {
    let mut oracle = Oracle::spawn();
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    let nuspecs = [
        r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd"><metadata><version>1.0.0</version><authors>a</authors><description>d</description></metadata></package>"#,
        r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd"><metadata><id>A</id><authors>a</authors><description>d</description></metadata></package>"#,
        r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd"></package>"#,
        r#"<p:package xmlns:p="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd"><p:metadata><p:id>A</p:id><p:version>1.0.0</p:version><p:authors>a</p:authors><p:description>d</p:description></p:metadata></p:package>"#,
        r#"<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd"><metadata xmlns="urn:other" /><metadata><id>A</id><version>1.0.0</version><authors>a</authors><description>d</description></metadata></package>"#,
    ];
    for nuspec in nuspecs {
        let mut package = Pkg::new("A", "1.0.0", vec![]).with_files(&["lib/net8.0/A.dll"]);
        package.raw_nuspec = Some(nuspec);
        let graded = grade(&mut oracle, &[package], &shape);
        assert!(!graded.theirs.success, "{nuspec}: {:?}", graded.theirs);
        assert!(
            matches!(
                graded.ours,
                Err(DgSpecResolveDecline::PackageMetadata { .. })
            ),
            "{nuspec}: {:?}",
            graded.ours
        );
    }
}

/// A version folder that is a symlink is one restore follows, so its metadata
/// counts, and B₁ follows it too.
#[cfg(unix)]
#[test]
fn a_symlinked_version_folders_metadata_is_validated() {
    let mut oracle = Oracle::spawn();
    let dir = tempfile::tempdir().expect("temp dir");
    let mut package = Pkg::new("A", "1.0.0", vec![]).with_files(&["lib/net8.0/A.dll"]);
    package.min_client_version = Some("99.0.0");
    install(&dir.path().join("elsewhere"), &[package]);
    let id_folder = dir.path().join("gp").join("a");
    fs::create_dir_all(&id_folder).unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("elsewhere").join("a").join("1.0.0"),
        id_folder.join("1.0.0"),
    )
    .unwrap();
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    let text = shape.render(dir.path());
    let spec = parse_dgspec(&text).unwrap();
    let graded = grade_spec(&mut oracle, &text, &spec, "a symlinked version folder");
    assert!(!graded.theirs.success, "{:?}", graded.theirs);
    assert!(
        matches!(
            graded.ours,
            Err(DgSpecResolveDecline::PackageMetadata { .. })
        ),
        "{:?}",
        graded.ours
    );
}

/// A package's symlinked `lib` folder is followed, by restore and by B₁, so
/// its assets count: `net9.0` assets fail a `net8.0` project (`NU1202`) and B₁
/// declines; `net8.0` assets commit.
#[cfg(unix)]
#[test]
fn a_symlinked_asset_folder_is_followed() {
    let mut oracle = Oracle::spawn();
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    for (tfm, restores) in [("net9.0", false), ("net8.0", true)] {
        let dir = tempfile::tempdir().expect("temp dir");
        install(&dir.path().join("gp"), &[Pkg::new("A", "1.0.0", vec![])]);
        let assets = dir.path().join("assets");
        fs::create_dir_all(assets.join(tfm)).unwrap();
        fs::write(assets.join(tfm).join("A.dll"), b"").unwrap();
        std::os::unix::fs::symlink(&assets, dir.path().join("gp/a/1.0.0/lib")).unwrap();
        let text = shape.render(dir.path());
        let spec = parse_dgspec(&text).unwrap();
        let graded = grade_spec(&mut oracle, &text, &spec, tfm);
        assert_eq!(
            graded.theirs.success, restores,
            "{tfm}: {:?}",
            graded.theirs
        );
        assert_eq!(graded.ours.is_ok(), restores, "{tfm}: {:?}", graded.ours);
    }
}

/// Restore throws on an empty project unique name; B₁ declines it.
#[test]
fn an_empty_project_unique_name_declines() {
    let mut oracle = Oracle::spawn();
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    let dir = tempfile::tempdir().expect("temp dir");
    install(&dir.path().join("gp"), &[Pkg::new("A", "1.0.0", vec![])]);
    let mut spec: Value = serde_json::from_str(&shape.render(dir.path())).unwrap();
    let project = the_project(&mut spec).clone();
    let mut project = project;
    project["restore"]["projectUniqueName"] = json!("");
    spec["projects"] = json!({"": project});
    spec["restore"] = json!({"": {}});
    let text = spec.to_string();
    let Ok(parsed) = parse_dgspec(&text) else {
        return;
    };
    let graded = grade_spec(&mut oracle, &text, &parsed, "an empty unique name");
    assert!(!graded.theirs.success, "{:?}", graded.theirs);
    assert!(graded.ours.is_err(), "{:?}", graded.ours);
}

/// Restore refuses aliases that differ only by case, aliases outside the
/// characters the SDK writes (`NU1019`), and a path holding a NUL; and it
/// compares a nuspec's version with the folder's by NuGet identity, under
/// which `1.0.0--0` is not `1.0.0-0`. The reader or B₁ declines each. No single-string
/// substitution reaches the alias cases, since an alias must change
/// consistently in three places, so they are graded here.
#[test]
fn restore_rejects_what_the_sdk_never_writes() {
    let mut oracle = Oracle::spawn();
    let packages = [Pkg::new("A", "1.0.0", vec![])];
    let shape = Shape::new(vec![
        Framework::new("net8.0", vec![Dep::new("A", "[1.0.0, )")]),
        Framework::new("net9.0", vec![Dep::new("A", "[1.0.0, )")]),
    ]);
    let renamed = |from: &str, to: &str, text: &str| -> String {
        let mut spec: Value = serde_json::from_str(text).unwrap();
        let project = the_project(&mut spec);
        let mut framework = project["frameworks"]
            .as_object_mut()
            .unwrap()
            .remove(from)
            .unwrap();
        framework["targetAlias"] = json!(to);
        framework["framework"] = json!(from);
        project["frameworks"][to] = framework;
        project["restore"]["frameworks"]
            .as_object_mut()
            .unwrap()
            .remove(from);
        project["restore"]["frameworks"][to] = json!({"targetAlias": to, "projectReferences": {}});
        let original = project["restore"]["originalTargetFrameworks"]
            .as_array_mut()
            .unwrap();
        for alias in original.iter_mut() {
            if alias == from {
                *alias = json!(to);
            }
        }
        spec.to_string()
    };
    let nul_output = |text: &str| -> String {
        let mut spec: Value = serde_json::from_str(text).unwrap();
        let output = the_project(&mut spec)["restore"]["outputPath"]
            .as_str()
            .unwrap()
            .to_owned();
        the_project(&mut spec)["restore"]["outputPath"] = json!(format!("{output}\u{0}x/"));
        spec.to_string()
    };
    type Mutation<'a> = Box<dyn Fn(&str) -> String + 'a>;
    let mutations: Vec<(&str, Mutation)> = vec![
        (
            "aliases differing by case",
            Box::new(|text| renamed("net9.0", "NET8.0", text)),
        ),
        (
            "an alias with a separator",
            Box::new(|text| renamed("net9.0", "net9.0/x", text)),
        ),
        ("a NUL in the output path", Box::new(nul_output)),
    ];
    for (label, mutate) in &mutations {
        let dir = tempfile::tempdir().expect("temp dir");
        install(&dir.path().join("gp"), &packages);
        let text = mutate(&shape.render(dir.path()));
        // The reader may decline first (it refuses names equal ignoring case),
        // which is as sound as B₁ declining.
        let Ok(spec) = parse_dgspec(&text) else {
            continue;
        };
        let graded = grade_spec(&mut oracle, &text, &spec, label);
        assert!(!graded.theirs.success, "{label}: {:?}", graded.theirs);
        assert!(graded.ours.is_err(), "{label}: {:?}", graded.ours);
    }

    let mut package = Pkg::new("A", "1.0.0-0", vec![]);
    package.version_text = Some("1.0.0--0");
    let single = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0-0, )")],
    )]);
    let graded = grade(&mut oracle, &[package], &single);
    assert!(
        matches!(
            graded.ours,
            Err(DgSpecResolveDecline::PackageMetadata { .. })
        ),
        "{:?}",
        graded.ours
    );
}

/// Restore validates every version it reads, not only the winners: a losing
/// version's minimum client version fails the restore even though another
/// version of the package wins.
#[test]
fn a_losing_versions_metadata_is_validated_too() {
    let mut oracle = Oracle::spawn();
    let mut loser = Pkg::new("C", "1.0.0", vec![]);
    loser.min_client_version = Some("99.0");
    let packages = [
        Pkg::new("A", "1.0.0", vec![group(None, &[("C", "[1.0.0, )")])]),
        Pkg::new("B", "1.0.0", vec![group(None, &[("C", "[2.0.0, )")])]),
        loser,
        Pkg::new("C", "2.0.0", vec![]),
    ];
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )"), Dep::new("B", "[1.0.0, )")],
    )]);
    let graded = grade(&mut oracle, &packages, &shape);
    assert!(!graded.theirs.success, "{:?}", graded.theirs);
    match graded.ours {
        Err(DgSpecResolveDecline::PackageMetadata { package, .. }) => {
            assert_eq!(package.version.to_normalized_string(), "1.0.0")
        }
        other => panic!("B₁ must decline on C 1.0.0's metadata, not {other:?}"),
    }
}

/// A package with the project's own name declines.
#[test]
fn a_package_named_like_the_project_declines() {
    let mut oracle = Oracle::spawn();
    let packages = [Pkg::new("A", "1.0.0", vec![])];
    let mut shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    shape.project_name = "a".to_owned();
    let graded = grade(&mut oracle, &packages, &shape);
    assert!(
        matches!(
            graded.ours,
            Err(DgSpecResolveDecline::PackageNamedLikeProject { .. })
        ),
        "{:?}",
        graded.ours
    );
}

// ============================================================================
// The verified list
// ============================================================================

/// The verified list is exactly the identity of the NuGet this suite grades
/// B₁ against: an identity reaches the list only by being graded, and one
/// stops being graded only by leaving it.
#[test]
fn the_verified_list_is_the_graded_nuget() {
    let mut oracle = Oracle::spawn();
    let response = oracle.request(&json!({"op": "nuGetLogCodes"}));
    let graded = response["nugetCommands"]["informationalVersion"]
        .as_str()
        .expect("the oracle names its NuGet")
        .to_owned();
    let manifest = Manifest::from_entries([graded]).expect("an identity is a manifest entry");
    check(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src/dgspec/verified_nuget.txt"),
        &manifest,
        "BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-nuget --test all \
         dgspec_resolve_diff::the_verified_list_is_the_graded_nuget",
    );
    assert_eq!(
        verified_nuget_identities().collect::<Vec<_>>(),
        manifest.entries(),
        "the compiled-in list is the file"
    );
}

// ============================================================================
// Declines, without the oracle
// ============================================================================

/// One spec the resolver commits to: a `net8.0` project with one package, under
/// the configuration `restore.rs` restores with.
fn base_spec(dir: &Path) -> Value {
    install(&dir.join("gp"), &[Pkg::new("A", "1.0.0", vec![])]);
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    serde_json::from_str(&shape.render(dir)).expect("rendered JSON")
}

fn the_project(spec: &mut Value) -> &mut Value {
    let projects = spec["projects"].as_object_mut().expect("projects");
    let [(_, project)] = projects
        .iter_mut()
        .collect::<Vec<_>>()
        .try_into()
        .expect("one project");
    project
}

fn verified() -> NuGetIdentity {
    NuGetIdentity::new(
        verified_nuget_identities()
            .next()
            .expect("the verified list is not empty"),
    )
}

fn resolve_text(
    identity: &NuGetIdentity,
    spec: &Value,
) -> Result<Vec<ResolvedFramework>, DgSpecResolveDecline> {
    let spec = parse_dgspec(&spec.to_string()).unwrap_or_else(|decline| panic!("{decline}"));
    // The base spec's project, by name: a mutation may add another.
    let project = spec
        .projects
        .iter()
        .map(|project| project.restore.project_unique_name.clone())
        .find(|name| name.ends_with("App.fsproj"))
        .expect("the base spec's project is in the spec");
    resolve_dgspec(identity, &spec, &project)
}

/// Each field the resolver does not model declines, naming it, when it is the
/// only change to a spec the resolver commits to.
#[test]
fn every_unmodelled_field_declines_by_name() {
    type Mutation = fn(&mut Value, &Path);
    let mutations: &[(UnmodelledField, Mutation)] = &[
        (UnmodelledField::ProjectName, |spec, _| {
            the_project(spec)["restore"]
                .as_object_mut()
                .unwrap()
                .remove("projectName");
        }),
        (UnmodelledField::PackagesPath, |spec, _| {
            the_project(spec)["restore"]
                .as_object_mut()
                .unwrap()
                .remove("packagesPath");
        }),
        (UnmodelledField::Sources, |spec, _| {
            the_project(spec)["restore"]["sources"]["https://api.nuget.org/v3/index.json"] =
                json!({});
        }),
        (UnmodelledField::Sources, |spec, _| {
            the_project(spec)["restore"]["sources"]["relative/folder"] = json!({});
        }),
        (UnmodelledField::FallbackFolders, |spec, dir| {
            the_project(spec)["restore"]["fallbackFolders"] =
                json!([dir.join("fallback").to_str().unwrap()]);
        }),
        (UnmodelledField::LockFile, |spec, _| {
            the_project(spec)["restore"]["restoreLockProperties"]["restorePackagesWithLockFile"] =
                json!("true");
        }),
        (UnmodelledField::LockFile, |spec, _| {
            the_project(spec)["restore"]["restoreLockProperties"]["restoreLockedMode"] =
                json!(true);
        }),
        (UnmodelledField::LockFile, |spec, _| {
            the_project(spec)["restore"]["restoreLockProperties"]
                .as_object_mut()
                .unwrap()
                .remove("nuGetLockFilePath");
        }),
        (UnmodelledField::LockFile, |spec, _| {
            the_project(spec)["restore"]["restoreLockProperties"]["nuGetLockFilePath"] =
                json!("packages.lock.json");
        }),
        (UnmodelledField::LockFile, |_, dir| {
            let lock = dir.join("obj/packages.lock.json");
            fs::create_dir_all(lock.parent().unwrap()).unwrap();
            fs::write(&lock, "{}").unwrap();
        }),
        (UnmodelledField::Audit, |spec, _| {
            the_project(spec)["restore"]
                .as_object_mut()
                .unwrap()
                .remove("restoreAuditProperties");
        }),
        (UnmodelledField::Audit, |spec, _| {
            the_project(spec)["restore"]["restoreAuditProperties"]["enableAudit"] = json!("true");
        }),
        (UnmodelledField::NotMicrosoftNetSdk, |spec, _| {
            the_project(spec)["restore"]["UsingMicrosoftNETSdk"] = json!(false);
        }),
        (UnmodelledField::CentralPackageManagement, |spec, _| {
            the_project(spec)["restore"]["centralPackageVersionsManagementEnabled"] = json!(true);
        }),
        (UnmodelledField::CentralPackageManagement, |spec, _| {
            the_project(spec)["restore"]["CentralPackageTransitivePinningEnabled"] = json!(true);
        }),
        (UnmodelledField::CentralPackageManagement, |spec, _| {
            the_project(spec)["frameworks"]["net8.0"]["dependencies"]["A"]["versionCentrallyManaged"] =
                json!(true);
        }),
        (UnmodelledField::CentralPackageManagement, |spec, _| {
            the_project(spec)["frameworks"]["net8.0"]["dependencies"]["A"]["versionOverride"] =
                json!("[1.0.0, )");
        }),
        (UnmodelledField::CentralPackageManagement, |spec, _| {
            the_project(spec)["frameworks"]["net8.0"]["centralPackageVersions"] =
                json!({"A": "[1.0.0, )"});
        }),
        (UnmodelledField::ProjectReferences, |spec, dir| {
            let lib = dir.join("Lib.fsproj");
            let lib = lib.to_str().unwrap();
            the_project(spec)["restore"]["frameworks"]["net8.0"]["projectReferences"][lib] =
                json!({"projectPath": lib});
        }),
        (UnmodelledField::PackagesToPrune, |spec, _| {
            the_project(spec)["frameworks"]["net8.0"]["packagesToPrune"] =
                json!({"System.Text.Json": "(,10.0.32767]"});
        }),
        (UnmodelledField::PackageTargetFallback, |spec, _| {
            the_project(spec)["frameworks"]["net8.0"]["assetTargetFallback"] = json!(false);
        }),
        (UnmodelledField::DependencyTarget, |spec, _| {
            the_project(spec)["frameworks"]["net8.0"]["dependencies"]["A"]["target"] =
                json!("Project");
        }),
        (UnmodelledField::RequiredMetadata, |spec, _| {
            the_project(spec)["restore"]
                .as_object_mut()
                .unwrap()
                .remove("projectPath");
        }),
        (UnmodelledField::RequiredMetadata, |spec, _| {
            the_project(spec)["restore"]
                .as_object_mut()
                .unwrap()
                .remove("outputPath");
        }),
        (UnmodelledField::RequiredMetadata, |spec, _| {
            the_project(spec)["restore"]["originalTargetFrameworks"] = json!(["net8.0", "net9.0"]);
        }),
        (UnmodelledField::RequiredMetadata, |spec, _| {
            the_project(spec)["restore"]["originalTargetFrameworks"] = json!([]);
        }),
        (UnmodelledField::RequiredMetadata, |spec, _| {
            let project = the_project(spec);
            let framework = project["frameworks"]["net8.0"].clone();
            project["frameworks"]["net9.0"] = framework;
        }),
        (UnmodelledField::RequiredMetadata, |spec, _| {
            let project = the_project(spec);
            let mut framework = project["frameworks"]["net8.0"].clone();
            framework["targetAlias"] = json!("net9.0");
            framework["framework"] = json!("net8.0");
            project["frameworks"]["net9.0"] = framework;
            project["restore"]["frameworks"]["net9.0"] =
                json!({"targetAlias": "net9.0", "projectReferences": {}});
            project["restore"]["originalTargetFrameworks"] = json!(["net8.0", "net9.0"]);
        }),
        (UnmodelledField::Config, |spec, dir| {
            let config = dir.join("NuGet.Config");
            fs::write(
                &config,
                r#"<configuration><config><add key="globalPackagesFolder" value="x" /></config></configuration>"#,
            )
            .unwrap();
            the_project(spec)["restore"]["configFilePaths"] = json!([config.to_str().unwrap()]);
        }),
        (UnmodelledField::OtherProjects, |spec, dir| {
            let mut other = the_project(spec).clone();
            let path = dir.join("Other.fsproj");
            let path = path.to_str().unwrap();
            other["restore"]["projectUniqueName"] = json!(path);
            other["restore"]["projectPath"] = json!(path);
            other["restore"]
                .as_object_mut()
                .unwrap()
                .remove("projectName");
            spec["projects"][path] = other;
        }),
        (UnmodelledField::OtherProjects, |spec, dir| {
            spec["restore"][dir.join("Missing.fsproj").to_str().unwrap()] = json!({});
        }),
        (UnmodelledField::NoFrameworks, |spec, _| {
            let project = the_project(spec);
            project["frameworks"] = json!({});
            project["restore"]["frameworks"] = json!({});
            project["restore"]["originalTargetFrameworks"] = json!([]);
        }),
    ];

    let identity = verified();
    for (index, (field, mutate)) in mutations.iter().enumerate() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut spec = base_spec(dir.path());
        resolve_text(&identity, &spec).unwrap_or_else(|decline| {
            panic!("the base spec must commit before mutation {index}: {decline}")
        });
        mutate(&mut spec, dir.path());
        match resolve_text(&identity, &spec) {
            Err(DgSpecResolveDecline::Unmodelled { fields }) => assert_eq!(
                fields,
                BTreeSet::from([*field]),
                "mutation {index} must decline on {field:?} alone"
            ),
            other => panic!("mutation {index} must decline on {field:?}, not {other:?}"),
        }
    }
}

/// For any spec the generator makes, an identity not on the verified list
/// declines, whatever else the spec holds.
#[test]
fn an_unverified_nuget_always_declines() {
    let mut rng = SplitMix64(0x5eed_0b1e);
    let identities = [
        NuGetIdentity::new(""),
        NuGetIdentity::new("7.0.3+0123456789abcdef"),
        NuGetIdentity::new(format!("{} ", verified())),
        NuGetIdentity::new(verified().as_str().to_ascii_uppercase()),
    ];
    for identity in &identities {
        assert!(!identity.is_verified(), "{identity:?} is not on the list");
    }
    for _ in 0..200 {
        let generated = generate(&mut rng);
        let dir = tempfile::tempdir().expect("temp dir");
        install(&dir.path().join("gp"), &generated.packages);
        let text = generated.shape.render(dir.path());
        let spec: Value = serde_json::from_str(&text).unwrap();
        for identity in &identities {
            assert!(
                matches!(
                    resolve_text(identity, &spec),
                    Err(DgSpecResolveDecline::UnverifiedNuGet { .. })
                ),
                "{identity:?} must decline"
            );
        }
    }
}

// ============================================================================
// The generated sweep
// ============================================================================

struct Generated {
    packages: Vec<Pkg>,
    shape: Shape,
}

const IDS: &[&str] = &["Alpha", "Beta", "Gamma", "Delta"];
const VERSIONS: &[&str] = &["1.0.0", "2.0.0", "3.0.0"];
const GROUP_TFMS: &[Option<&str>] = &[
    None,
    Some("net461"),
    Some("net48"),
    Some("netstandard2.0"),
    Some("net8.0"),
];
const PROJECT_FRAMEWORKS: &[&str] = &["net8.0", "netstandard2.0", "net472", "net10.0"];
/// `<metadata>` children B₁ admits as never read by restore, malformed ones
/// among them: each must be graded on a committed spec.
const DESCRIPTIVE_METADATA: &[&str] = &[
    "<tags>a b</tags>",
    r#"<license type="expression">MIT</license>"#,
    r#"<license type="expression">(((</license>"#,
    r#"<repository type="git" url="https://example.invalid/r" />"#,
    "<serviceable>true</serviceable>",
    "<serviceable>bogus</serviceable>",
    "<requireLicenseAcceptance>maybe</requireLicenseAcceptance>",
    "<icon>missing.png</icon>",
    "<readme>missing.md</readme>",
    "<releaseNotes>notes</releaseNotes>",
    "<owners>o</owners><title>t</title><summary>s</summary><copyright>c</copyright>",
    "<language>xx-INVALID</language><projectUrl>not a url</projectUrl>",
    "<licenseUrl>not a url</licenseUrl><iconUrl>not a url</iconUrl>",
    "<developmentDependency>true</developmentDependency>",
    "<developmentDependency>bogus</developmentDependency>",
];
/// `<metadata>` children restore reads and B₁ does not model.
const DECLINED_METADATA: &[&str] = &[
    r#"<contentFiles><files include="**/*" buildAction="Bad" /></contentFiles>"#,
    r#"<frameworkAssemblies><frameworkAssembly assemblyName="System.Xml" targetFramework="net48" /></frameworkAssemblies>"#,
    "<somethingNew>1</somethingNew>",
];

fn gen_range(rng: &mut SplitMix64) -> String {
    let version = rng.pick(VERSIONS);
    match rng.below(5) {
        0 => format!("[{version}]"),
        // Bounded above, so a nearer edge to a higher version leaves it
        // outside its constraint (`NU1608`).
        1 => "[1.0.0, 2.0.0)".to_owned(),
        _ => format!("[{version}, )"),
    }
}

fn generate(rng: &mut SplitMix64) -> Generated {
    let mut packages = Vec::new();
    for id in IDS {
        for version in VERSIONS {
            if rng.below(10) == 0 {
                continue;
            }
            let groups = (0..rng.below(3))
                .map(|_| Group {
                    tfm: *rng.pick(GROUP_TFMS),
                    deps: (0..rng.below(3))
                        .map(|_| ((*rng.pick(IDS)).to_owned(), gen_range(rng)))
                        .filter(|(dep, _)| dep != id)
                        .collect(),
                })
                .collect();
            let files: &[&str] = match rng.below(8) {
                0 => &["lib/netstandard2.0/X.dll"],
                1 => &["lib/net48/X.dll"],
                2 => &["lib/net8.0/X.dll", "lib/net461/X.dll"],
                3 => &["ref/netstandard2.0/X.dll", "lib/net10.0/X.dll"],
                4 => &["lib/netstandard1.0/_._"],
                5 => rng.pick(&[
                    &["build/X.targets"][..],
                    &["build/net48/X.targets"],
                    &["build/MSBuildCore/X.dll", "analyzers/dotnet/cs/X.dll"],
                ]),
                _ => &[],
            };
            let mut package = Pkg::new(id, version, groups).with_files(files);
            if rng.below(3) == 0 {
                package.metadata = vec![*rng.pick(DESCRIPTIVE_METADATA)];
            }
            match rng.below(40) {
                10 => package.metadata.push(*rng.pick(DECLINED_METADATA)),
                11 => package.version_text = Some(*rng.pick(&["9.9.9"])),
                12 => {
                    package.marker = Some(*rng.pick(&[
                        r#"{}"#,
                        r#"{"version":2,"contentHash":null}"#,
                        r#"{"version":2,"contentHash":"AAAA","source":"https://api.nuget.org/v3/index.json"}"#,
                    ]))
                }
                0 => package.min_client_version = Some(*rng.pick(&["2.12", "5.0.0", "99.0"])),
                1 => {
                    package.package_types = vec![*rng.pick(&[
                        r#"name="Dependency""#,
                        r#"name="DotnetTool""#,
                        r#"name="Dependency" version="1""#,
                    ])]
                }
                _ => {}
            }
            packages.push(package);
        }
    }

    // A download target: version 1.0.0 is installed, 2.0.0 is not.
    let mut download = Pkg::new("Download", "1.0.0", vec![]).with_files(&["data/x.txt"]);
    if rng.below(3) == 0 {
        download.package_types = vec![r#"name="DotnetPlatform""#];
    }
    packages.push(download);
    let mut aliases: Vec<&'static str> = PROJECT_FRAMEWORKS.to_vec();
    let count = 1 + usize::from(rng.below(3) == 0);
    let frameworks = (0..count)
        .map(|_| {
            let alias = aliases.remove(rng.below(aliases.len()));
            let mut ids: Vec<&str> = IDS.to_vec();
            let deps = (0..1 + rng.below(3))
                .map(|_| {
                    let id = ids.remove(rng.below(ids.len()));
                    let mut dep = Dep::new(id, &format!("[{}, )", rng.pick(VERSIONS)));
                    if rng.below(4) == 0 {
                        dep.include =
                            Some(*rng.pick(&["Runtime, Compile", "None", "All", "Compile"]));
                    }
                    if rng.below(4) == 0 {
                        dep.suppress_parent = Some(*rng.pick(&["All", "None", "Compile"]));
                    }
                    dep.auto_referenced = rng.below(4) == 0;
                    if rng.below(8) == 0 {
                        dep.no_warn = vec!["NU1608"];
                    }
                    dep
                })
                .collect();
            let mut framework = Framework::new(alias, deps);
            framework.asset_target_fallback = rng.below(4) != 0;
            framework.framework_reference = alias != "net472" && rng.below(2) == 0;
            if rng.below(3) == 0 {
                framework.downloads = vec![*rng.pick(&[
                    ("Download", "[1.0.0, 1.0.0]"),
                    ("Download", "[1.0.0, 1.0.0]"),
                    ("Download", "[2.0.0, 2.0.0]"),
                    ("Download", "[1.0.0, )"),
                ])];
            }
            framework
        })
        .collect();

    let mut shape = Shape::new(frameworks);
    shape.sources = match rng.below(4) {
        0 => vec![],
        1 => vec![Source::Missing],
        2 => vec![Source::Missing, Source::Empty],
        _ => vec![Source::Empty],
    };
    match rng.below(8) {
        0 => shape.all_warnings_as_errors = true,
        1 => shape.warn_as_error = vec!["NU1608"],
        2 | 3 => shape.warn_as_error = vec!["NU1605"],
        _ => {}
    }
    if rng.below(3) == 0 {
        shape.no_warn = vec!["NU1608", "NU1701"];
    }
    if rng.below(4) == 0 {
        shape.warn_not_as_error = vec!["NU1608"];
    }
    shape.sdk_analysis_level =
        *rng.pick(&[None, Some("9.0.100"), Some("10.0.100"), Some("10.0.400")]);
    shape.enable_audit = *rng.pick(&[
        Some("false"),
        Some("false"),
        Some("false"),
        Some("FALSE"),
        Some("FALSE"),
        None,
    ]);
    shape.lock_file = *rng.pick(&[Some("false"), None]);
    shape.config = *rng.pick(&[None, None, Some(SOURCES_CONFIG), Some(MAPPING_CONFIG)]);
    if rng.below(10) == 0 {
        shape.project_name = (*rng.pick(IDS)).to_owned();
    }
    Generated { packages, shape }
}

/// The variations of a committed spec the sweep must have graded at least
/// once, so that each inert claim is tested and not merely asserted.
fn features(
    generated: &Generated,
    frameworks: &[ResolvedFramework],
    restored: &Restored,
) -> Vec<String> {
    let shape = &generated.shape;
    let mut features = vec![
        format!("assets-version={}", restored.version),
        format!("frameworks={}", shape.frameworks.len()),
        format!("sources={:?}", shape.sources),
        format!("warn-as-error={:?}", shape.warn_as_error),
        format!("all-warnings-as-errors={}", shape.all_warnings_as_errors),
        format!(
            "predicted-warnings={}",
            frameworks
                .iter()
                .any(|framework| framework.warnings.as_ref().is_some_and(|w| !w.is_empty()))
        ),
        format!("no-warn={}", !shape.no_warn.is_empty()),
        format!("sdk-analysis-level={:?}", shape.sdk_analysis_level),
        format!("enable-audit={:?}", shape.enable_audit),
        format!("lock-file={:?}", shape.lock_file),
        format!("config={}", shape.config.is_some()),
    ];
    for framework in &shape.frameworks {
        features.push(format!("framework={}", framework.alias));
        if framework.framework_reference {
            features.push("framework-reference".to_owned());
        }
        if !framework.downloads.is_empty() {
            features.push("download".to_owned());
        }
        for dep in &framework.deps {
            if dep.include.is_some() {
                features.push("include".to_owned());
            }
            if dep.suppress_parent.is_some() {
                features.push("suppress-parent".to_owned());
            }
            if dep.auto_referenced {
                features.push("auto-referenced".to_owned());
            }
            if !dep.no_warn.is_empty() {
                features.push("dependency-no-warn".to_owned());
            }
        }
    }
    let committed_ids: BTreeSet<String> = frameworks
        .iter()
        .flat_map(|framework| &framework.closure.packages)
        .map(|package| {
            format!(
                "{} {}",
                package.identity.id,
                package.identity.version.to_normalized_string()
            )
        })
        .collect();
    for pkg in &generated.packages {
        let identity = format!("{} {}", pkg.id, pkg.version);
        if committed_ids
            .iter()
            .any(|c| c.eq_ignore_ascii_case(&identity))
        {
            if !pkg.files.is_empty() {
                features.push("package-with-assets".to_owned());
            }
            if let Some(minimum) = pkg.min_client_version {
                features.push(format!("min-client-version={minimum}"));
            }
            for package_type in &pkg.package_types {
                features.push(format!("package-type={package_type}"));
            }
            for metadata in &pkg.metadata {
                features.push(format!("metadata={metadata}"));
            }
            if let Some(marker) = pkg.marker {
                features.push(format!("marker={marker}"));
            }
        }
    }
    // The fallback list mattered: without it, the resolver computes another
    // closure.
    let root = tempfile::tempdir().expect("temp dir");
    install(root.path(), &generated.packages);
    for (framework, resolved) in shape.frameworks.iter().zip(frameworks) {
        let direct: Vec<DirectPackageRequirement> = framework
            .deps
            .iter()
            .map(|dep| {
                DirectPackageRequirement::new(
                    PackageId::parse(&dep.id).unwrap(),
                    VersionRange::parse(&dep.range).unwrap(),
                )
            })
            .collect();
        let without = resolve_offline(
            root.path(),
            &NuGetFramework::parse(framework.alias).unwrap(),
            &direct,
        );
        let identities = |closure: &borzoi_nuget::ResolvedPackageClosure| {
            closure
                .packages
                .iter()
                .map(|package| package.identity.clone())
                .collect::<Vec<_>>()
        };
        if without.as_ref().ok().map(identities) != Some(identities(&resolved.closure)) {
            features.push("asset-target-fallback-mattered".to_owned());
        }
    }
    features
}

/// Every variation [`features`] can report on a spec the generator makes.
fn required_features() -> BTreeSet<String> {
    let mut required: BTreeSet<String> = [
        "assets-version=3",
        "assets-version=4",
        "frameworks=1",
        "frameworks=2",
        "sources=[]",
        "sources=[Missing]",
        "sources=[Missing, Empty]",
        "sources=[Empty]",
        "warn-as-error=[]",
        "warn-as-error=[\"NU1605\"]",
        "warn-as-error=[\"NU1608\"]",
        "all-warnings-as-errors=true",
        "predicted-warnings=true",
        "dependency-no-warn",
        "download",
        "no-warn=true",
        "no-warn=false",
        "sdk-analysis-level=None",
        "sdk-analysis-level=Some(\"9.0.100\")",
        "sdk-analysis-level=Some(\"10.0.100\")",
        "sdk-analysis-level=Some(\"10.0.400\")",
        "enable-audit=Some(\"false\")",
        "enable-audit=Some(\"FALSE\")",
        "lock-file=Some(\"false\")",
        "lock-file=None",
        "config=true",
        "config=false",
        "framework-reference",
        "include",
        "suppress-parent",
        "auto-referenced",
        "asset-target-fallback-mattered",
        "package-with-assets",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    required.extend(PROJECT_FRAMEWORKS.iter().map(|f| format!("framework={f}")));
    required
}

/// The class of a decline, for the census.
fn decline_class(decline: &DgSpecResolveDecline) -> String {
    match decline {
        DgSpecResolveDecline::UnverifiedNuGet { .. } => "unverified-nuget".to_owned(),
        DgSpecResolveDecline::ProjectNotRestored { .. } => "not-restored".to_owned(),
        DgSpecResolveDecline::Unmodelled { fields } => fields
            .iter()
            .map(|field| field.name())
            .collect::<Vec<_>>()
            .join("+"),
        DgSpecResolveDecline::Resolve { decline, .. } => {
            let debug = format!("{decline:?}");
            let variant = debug.split(['(', ' ', '{']).next().unwrap_or_default();
            format!("resolve:{variant}")
        }
        DgSpecResolveDecline::PackageNamedLikeProject { .. } => "named-like-project".to_owned(),
        DgSpecResolveDecline::PromotedWarning { code, .. } => match code {
            Some(code) => format!("promoted-warning:{code}"),
            None => "promoted-warning:unpredicted".to_owned(),
        },
        DgSpecResolveDecline::CompatibilityUnproven { .. } => "compatibility-unproven".to_owned(),
        DgSpecResolveDecline::PackageRead { .. } => "package-read".to_owned(),
        DgSpecResolveDecline::PackageMetadata { .. } => "package-metadata".to_owned(),
        DgSpecResolveDecline::DownloadUnavailable { .. } => "download-unavailable".to_owned(),
    }
}

/// Certain-implies-exact over generated specs, with every inert variation
/// graded on at least one committed spec.
#[test]
fn generated_specs_resolve_identically() {
    let cases: usize = std::env::var("BORZOI_DGSPEC_RESOLVE_CASES")
        .ok()
        .map(|cases| cases.parse().expect("a case count"))
        .unwrap_or(600);
    let mut oracle = Oracle::spawn();
    let mut rng = SplitMix64(0xd65_bec1);
    let mut seen = BTreeSet::new();
    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    for _ in 0..cases {
        let generated = generate(&mut rng);
        let graded = grade(&mut oracle, &generated.packages, &generated.shape);
        let restore = if graded.theirs.success {
            "restores"
        } else {
            "fails"
        };
        let ours = match &graded.ours {
            Ok(frameworks) => {
                seen.extend(features(&generated, frameworks, &graded.theirs));
                "commits".to_owned()
            }
            Err(decline) => format!("declines {}", decline_class(decline)),
        };
        *census.entry(format!("{ours}; B₀ {restore}")).or_default() += 1;
    }
    let report: String = census
        .iter()
        .map(|(class, count)| format!("  {count:4} {class}\n"))
        .collect();
    eprintln!("dgspec resolve census over {cases} specs:\n{report}");
    let required = required_features();
    let missing: Vec<&String> = required.difference(&seen).collect();
    assert!(
        missing.is_empty(),
        "no committed spec graded {missing:?}; the census:\n{report}"
    );
}

/// Every path into a JSON value, each as the sequence of keys and indices that
/// reaches it, parents before children.
fn json_paths(value: &Value) -> Vec<Vec<PathStep>> {
    let mut paths = Vec::new();
    let mut stack = vec![(Vec::new(), value)];
    while let Some((path, value)) = stack.pop() {
        match value {
            Value::Object(members) => {
                for (key, child) in members {
                    let mut child_path = path.clone();
                    child_path.push(PathStep::Key(key.clone()));
                    stack.push((child_path, child));
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    let mut child_path = path.clone();
                    child_path.push(PathStep::Index(index));
                    stack.push((child_path, child));
                }
            }
            _ => {}
        }
        paths.push(path);
    }
    paths
}

#[derive(Debug, Clone)]
enum PathStep {
    Key(String),
    Index(usize),
}

fn at_mut<'a>(value: &'a mut Value, path: &[PathStep]) -> &'a mut Value {
    path.iter().fold(value, |value, step| match step {
        PathStep::Key(key) => &mut value[key.as_str()],
        PathStep::Index(index) => &mut value[*index],
    })
}

/// Every single-step deletion from a committing spec: each key or element
/// removed, and each non-empty object, array or string emptied. A variant the
/// reader declines is skipped; every other one is graded against B₀.
///
/// This is how a field restore requires is found without knowing restore's
/// validation rules: deleting it must make B₁ decline or B₀ agree.
#[test]
fn every_single_deletion_from_a_committing_spec_is_graded() {
    let mut oracle = Oracle::spawn();
    let packages = [
        Pkg::new("A", "1.0.0", vec![group(None, &[("B", "[1.0.0, )")])])
            .with_files(&["lib/netstandard2.0/A.dll"]),
        Pkg::new("B", "1.0.0", vec![]),
    ];
    let mut multi = Framework::new("netstandard2.0", vec![Dep::new("A", "[1.0.0, )")]);
    multi.deps[0].include = Some("Runtime, Compile");
    let mut shape = Shape::new(vec![
        Framework::new("net8.0", vec![Dep::new("A", "[1.0.0, )")]),
        multi,
    ]);
    shape.frameworks[0].framework_reference = true;
    shape.no_warn = vec!["NU1701"];
    shape.warn_as_error = vec!["NU1605"];

    let dir = tempfile::tempdir().expect("temp dir");
    install(&dir.path().join("gp"), &packages);
    let base: Value = serde_json::from_str(&shape.render(dir.path())).expect("JSON");
    let base_spec = parse_dgspec(&base.to_string()).expect("the base spec parses");
    let graded = grade_spec(&mut oracle, &base.to_string(), &base_spec, "the base spec");
    assert!(
        graded.ours.is_ok(),
        "the base spec must commit: {:?}",
        graded.ours
    );

    let mut graded_variants = 0;
    let mut committed_variants = 0;
    for path in json_paths(&base) {
        let Some((last, parent)) = path.split_last() else {
            continue;
        };
        let mut variants = Vec::new();
        let mut deleted = base.clone();
        match (at_mut(&mut deleted, parent), last) {
            (Value::Object(members), PathStep::Key(key)) => {
                members.remove(key);
            }
            (Value::Array(items), PathStep::Index(index)) => {
                items.remove(*index);
            }
            _ => unreachable!("a path's parent is a container"),
        }
        variants.push(("delete", deleted));
        let mut emptied = base.clone();
        let target = at_mut(&mut emptied, &path);
        let empty = match target {
            Value::Object(members) if !members.is_empty() => Some(json!({})),
            Value::Array(items) if !items.is_empty() => Some(json!([])),
            Value::String(text) if !text.is_empty() => Some(json!("")),
            _ => None,
        };
        if let Some(empty) = empty {
            *target = empty;
            variants.push(("empty", emptied));
        }
        for (kind, variant) in variants {
            let text = variant.to_string();
            let Ok(spec) = parse_dgspec(&text) else {
                continue;
            };
            if spec.projects.len() != 1 {
                continue;
            }
            let graded = grade_spec(&mut oracle, &text, &spec, &format!("{kind} {path:?}"));
            graded_variants += 1;
            committed_variants += usize::from(graded.ours.is_ok());
        }
    }
    eprintln!("graded {graded_variants} deletions, {committed_variants} committed");
    assert!(
        graded_variants >= 40 && committed_variants >= 10,
        "graded {graded_variants} deletions, {committed_variants} committed"
    );
}

/// Every string of a committing spec replaced by each other string the spec
/// holds, and by adversarial transforms of them, one substitution at a time, graded against B₀ where the reader
/// accepts the result. Deletions cannot reach a cross-field consistency rule
/// (two frameworks sharing an alias, a reference naming another project);
/// substituting the spec's own values for one another can.
#[test]
fn every_substitution_of_a_committing_specs_strings_is_graded() {
    let mut oracle = Oracle::spawn();
    let packages = [
        Pkg::new("A", "1.0.0", vec![group(None, &[("B", "[1.0.0, )")])])
            .with_files(&["lib/netstandard2.0/A.dll"]),
        Pkg::new("B", "1.0.0", vec![]),
    ];
    let mut shape = Shape::new(vec![
        Framework::new("net8.0", vec![Dep::new("A", "[1.0.0, )")]),
        Framework::new("netstandard2.0", vec![Dep::new("B", "[1.0.0, )")]),
    ]);
    shape.config = Some(SOURCES_CONFIG);
    let dir = tempfile::tempdir().expect("temp dir");
    install(&dir.path().join("gp"), &packages);
    let base: Value = serde_json::from_str(&shape.render(dir.path())).expect("JSON");

    // The spec's own strings, and adversarial transforms of each: the case
    // flipped (NuGet compares names ignoring case), a path separator appended,
    // and a control character embedded.
    let mut pool = BTreeSet::new();
    let mut leaves = Vec::new();
    for path in json_paths(&base) {
        let mut copy = base.clone();
        if let Value::String(text) = at_mut(&mut copy, &path) {
            pool.insert(text.clone());
            pool.insert(text.to_ascii_uppercase());
            pool.insert(format!("{text}/x"));
            pool.insert(format!("{text}\u{0}"));
            leaves.push(path);
        }
    }
    let mut graded_variants = 0;
    let mut committed_variants = 0;
    for path in &leaves {
        for replacement in &pool {
            let mut variant = base.clone();
            let target = at_mut(&mut variant, path);
            if target.as_str() == Some(replacement) {
                continue;
            }
            *target = Value::String(replacement.clone());
            let text = variant.to_string();
            let Ok(spec) = parse_dgspec(&text) else {
                continue;
            };
            if spec.projects.len() != 1 {
                continue;
            }
            let graded = grade_spec(
                &mut oracle,
                &text,
                &spec,
                &format!("{path:?} := {replacement:?}"),
            );
            graded_variants += 1;
            committed_variants += usize::from(graded.ours.is_ok());
        }
    }
    eprintln!("graded {graded_variants} substitutions, {committed_variants} committed");
    assert!(
        graded_variants >= 100 && committed_variants >= 10,
        "graded {graded_variants} substitutions, {committed_variants} committed"
    );
}

/// What an XML perturbation does to one slot of a template.
const XML_PERTURBATIONS: &[&str] = &[
    "",
    " ",
    "<!--c-->",
    "<!--c-->{}",
    "{}<!--c-->",
    " {} ",
    "{}\n",
];

/// Render `template`, whose slots are `{0}`, `{1}`, …, with `values`.
fn fill(template: &str, values: &[String]) -> String {
    values
        .iter()
        .enumerate()
        .fold(template.to_owned(), |text, (index, value)| {
            text.replace(&format!("{{{index}}}"), value)
        })
}

/// Every attribute and text slot of a committing nuspec and config file
/// perturbed in turn (emptied, blanked, padded, or with a comment beside or in
/// place of the value), each graded against B₀. The vocabularies decide what a
/// value is; this is how the decisions about whitespace and comments within
/// them are checked against restore's.
#[test]
fn every_perturbation_of_nuspec_and_config_text_is_graded() {
    const NUSPEC: &str = r#"<?xml version="1.0"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata minClientVersion="{0}">
    <id>{1}</id>
    <version>{2}</version>
    <authors>{3}</authors>
    <description>{4}</description>
    <packageTypes><packageType name="{5}" /></packageTypes>
    <serviceable>{6}</serviceable>
    <dependencies><group targetFramework="{7}"><dependency id="{8}" version="{9}" /></group></dependencies>
  </metadata>
</package>"#;
    const NUSPEC_VALUES: &[&str] = &[
        "2.12",
        "A",
        "1.0.0",
        "a",
        "d",
        "Dependency",
        "true",
        "net8.0",
        "B",
        "[1.0.0, )",
    ];
    const CONFIG: &str = r#"<configuration>
  <packageSources><add key="{0}" value="{1}" protocolVersion="{2}" /></packageSources>
  <disabledPackageSources><add key="{3}" value="{4}" /></disabledPackageSources>
</configuration>"#;
    const CONFIG_VALUES: &[&str] = &[
        "nuget.org",
        "https://api.nuget.org/v3/index.json",
        "3",
        "nuget.org",
        "true",
    ];

    let perturbed = |values: &[&str]| -> Vec<(usize, Vec<String>)> {
        let mut variants = vec![(usize::MAX, values.iter().map(|v| v.to_string()).collect())];
        for slot in 0..values.len() {
            for perturbation in XML_PERTURBATIONS {
                let mut filled: Vec<String> = values.iter().map(|v| v.to_string()).collect();
                filled[slot] = perturbation.replace("{}", values[slot]);
                variants.push((slot, filled));
            }
        }
        variants
    };

    let mut oracle = Oracle::spawn();
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    let mut graded_variants = 0;
    let mut committed_variants = 0;
    let mut grade_one = |nuspec: String, config: Option<String>, label: String| {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut a = Pkg::new("A", "1.0.0", vec![]).with_files(&["lib/net8.0/A.dll"]);
        a.raw_nuspec = Some(Box::leak(nuspec.into_boxed_str()));
        install(&dir.path().join("gp"), &[a, Pkg::new("B", "1.0.0", vec![])]);
        let mut shape = shape.clone();
        shape.config = config.map(|config| &*Box::leak(config.into_boxed_str()));
        let text = shape.render(dir.path());
        let spec = parse_dgspec(&text).expect("the spec parses");
        let graded = grade_spec(&mut oracle, &text, &spec, &label);
        graded_variants += 1;
        committed_variants += usize::from(graded.ours.is_ok());
        graded
    };
    for (slot, values) in perturbed(NUSPEC_VALUES) {
        let graded = grade_one(
            fill(NUSPEC, &values),
            None,
            format!("nuspec slot {slot}: {values:?}"),
        );
        if slot == usize::MAX {
            assert!(
                graded.ours.is_ok(),
                "the base nuspec commits: {:?}",
                graded.ours
            );
        }
    }
    for (slot, values) in perturbed(CONFIG_VALUES) {
        let nuspec = fill(
            NUSPEC,
            &NUSPEC_VALUES
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>(),
        );
        let graded = grade_one(
            nuspec,
            Some(fill(CONFIG, &values)),
            format!("config slot {slot}: {values:?}"),
        );
        if slot == usize::MAX {
            assert!(
                graded.ours.is_ok(),
                "the base config commits: {:?}",
                graded.ours
            );
        }
    }
    eprintln!("graded {graded_variants} perturbations, {committed_variants} committed");
    assert!(committed_variants >= 10, "{committed_variants} committed");
}

/// The ways a nuspec can spell its namespace, each graded against B₀: none,
/// each schema version NuGet has published, an unknown one, a prefix on every
/// element with no default, and a default on `<package>` with `<metadata>`'s
/// children prefixed. Certain-implies-exact on each, and the published
/// default-namespace forms commit.
#[test]
fn nuspec_namespace_forms_are_graded() {
    const BODY: &str =
        "<id>A</id><version>1.0.0</version><authors>a</authors><description>d</description>";
    let schema = |year: &str| format!("http://schemas.microsoft.com/packaging/{year}/nuspec.xsd");
    let mut forms: Vec<(String, bool)> = vec![(
        format!("<package><metadata>{BODY}</metadata></package>"),
        true,
    )];
    for year in ["2010/07", "2011/08", "2012/06", "2013/01", "2013/05"] {
        forms.push((
            format!(
                r#"<package xmlns="{}"><metadata>{BODY}</metadata></package>"#,
                schema(year)
            ),
            true,
        ));
    }
    forms.push((
        format!(r#"<package xmlns="urn:not-nuget"><metadata>{BODY}</metadata></package>"#),
        false,
    ));
    let prefixed = BODY.replace('<', "<p:").replace("<p:/", "</p:");
    forms.push((
        format!(
            r#"<p:package xmlns:p="{}"><p:metadata>{prefixed}</p:metadata></p:package>"#,
            schema("2013/05")
        ),
        false,
    ));
    forms.push((
        format!(
            r#"<package xmlns="{0}" xmlns:p="{0}"><metadata>{prefixed}</metadata></package>"#,
            schema("2013/05")
        ),
        false,
    ));

    let mut oracle = Oracle::spawn();
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    for (nuspec, must_commit) in forms {
        let mut package = Pkg::new("A", "1.0.0", vec![]).with_files(&["lib/net8.0/A.dll"]);
        package.raw_nuspec = Some(Box::leak(nuspec.clone().into_boxed_str()));
        let graded = grade(&mut oracle, &[package], &shape);
        if must_commit {
            assert!(graded.ours.is_ok(), "{nuspec}: {:?}", graded.ours);
        }
    }
}

/// Every top-level asset folder, with and without a framework subfolder, and
/// with a framework folder NuGet cannot parse (it throws), alone in a package
/// of a `net8.0` project with the fallback list: graded against B₀, with B₁
/// committing exactly where the compatibility rule says.
#[test]
fn asset_folder_shapes_are_graded() {
    let mut oracle = Oracle::spawn();
    let shape = Shape::new(vec![Framework::new(
        "net8.0",
        vec![Dep::new("A", "[1.0.0, )")],
    )]);
    let shapes: &[(&str, bool)] = &[
        ("build/A.targets", true),
        ("build/net48/A.targets", false),
        ("build/MSBuildCore/A.dll", true),
        ("buildTransitive/net48/A.props", false),
        ("buildMultiTargeting/A.props", true),
        ("buildCrossTargeting/A.props", true),
        ("analyzers/dotnet/cs/A.dll", true),
        ("tools/net48/A.exe", true),
        ("content/A.txt", true),
        ("contentFiles/any/net48/A.cs", true),
        ("runtimes/win-x64/lib/net48/A.dll", true),
        ("runtimes/win-x64/native/A.dll", true),
        ("native/A.dll", true),
        ("embed/net48/A.dll", true),
        ("lib/net48/A.dll", false),
        ("ref/net48/A.dll", false),
        ("lib/A.txt", false),
        ("lib/net48/A.xml", false),
        ("lib/net8.0/A.dll", true),
        ("build/portable-net45-client/A.targets", false),
        ("contentFiles/any/portable-net45-client/A.cs", false),
    ];
    for (file, commits) in shapes {
        let package = Pkg::new("A", "1.0.0", vec![]).with_files(&[file]);
        let graded = grade(&mut oracle, &[package], &shape);
        assert_eq!(graded.ours.is_ok(), *commits, "{file}: {:?}", graded.ours);
    }
}

/// Restore only checks that a download is in the packages folder: a missing
/// one fails it (`NU1101`), and nothing else about a present one matters (its
/// package type, minimum client version, commit marker, metadata). B₁ commits
/// for any present exact download and declines a missing one, and one whose
/// range is not exact, which the SDK never writes.
#[test]
fn downloads_are_a_presence_check() {
    let mut oracle = Oracle::spawn();
    let base = || Pkg::new("D", "1.0.0", vec![]).with_files(&["data/x.txt"]);
    let mut platform = base();
    platform.package_types = vec![r#"name="DotnetPlatform""#];
    let mut client = base();
    client.min_client_version = Some("99.0");
    let mut marker = base();
    marker.marker = Some("{}");
    let mut metadata = base();
    metadata.metadata = vec!["<somethingNew>1</somethingNew>"];
    let cases: Vec<(&str, Vec<Pkg>, &str, bool, bool)> = vec![
        ("present", vec![base()], "[1.0.0, 1.0.0]", true, true),
        ("short exact", vec![base()], "[1.0.0]", true, true),
        (
            "a platform package",
            vec![platform],
            "[1.0.0, 1.0.0]",
            true,
            true,
        ),
        (
            "a high client minimum",
            vec![client],
            "[1.0.0, 1.0.0]",
            true,
            true,
        ),
        (
            "an empty marker",
            vec![marker],
            "[1.0.0, 1.0.0]",
            true,
            true,
        ),
        (
            "unknown metadata",
            vec![metadata],
            "[1.0.0, 1.0.0]",
            true,
            true,
        ),
        ("missing", vec![], "[1.0.0, 1.0.0]", false, false),
        ("an open range", vec![base()], "[1.0.0, )", true, false),
        (
            "a floating range",
            vec![base()],
            "[1.0.*, 1.0.0]",
            false,
            false,
        ),
    ];
    for (label, mut packages, range, restores, commits) in cases {
        packages.push(Pkg::new("A", "1.0.0", vec![]));
        let mut framework = Framework::new("net8.0", vec![Dep::new("A", "[1.0.0, )")]);
        framework.downloads = vec![("D", range)];
        let graded = grade(&mut oracle, &packages, &Shape::new(vec![framework]));
        assert_eq!(
            graded.theirs.success, restores,
            "{label}: {:?}",
            graded.theirs
        );
        assert_eq!(graded.ours.is_ok(), commits, "{label}: {:?}", graded.ours);
        if !commits {
            assert!(
                matches!(
                    graded.ours,
                    Err(DgSpecResolveDecline::DownloadUnavailable { .. })
                ),
                "{label}: {:?}",
                graded.ours
            );
        }
    }
}

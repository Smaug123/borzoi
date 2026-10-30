//! `borzoi_nuget::dgspec::parse_dgspec` against NuGet's own reader
//! (`DependencyGraphSpec.Load`, via the oracle's `normaliseDgSpec` op).
//!
//! The contract is certain-implies-exact: whenever we return a spec, NuGet
//! loads the same document, and its values projected to the canonical shape
//! equal ours projected the same way. A decline claims nothing. Both
//! projections are built field by field from the respective object models,
//! so a default we fill in differently, a spelling we read differently, or a
//! document NuGet refuses that we accept, each shows up as a violation.
//!
//! Non-vacuity comes from two places: the hand cases pin which documents must
//! *commit* (not merely agree), and the generator requires every mutation kind
//! to have produced at least one agreement.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use borzoi_nuget::NuGetFramework;
use borzoi_nuget::dgspec::{
    DependencyGraphSpec, DgSpecDecline, DgSpecDeclineReason, NuGetLogCode, PackageSpec,
    PathSegment, ProjectRestoreMetadata, TargetFrameworkInformation, parse_dgspec,
};
use serde_json::{Value, json};

use super::common::{
    Oracle, SplitMix64, gen_framework_string, gen_range_string, gen_version_string,
};

// ---------------------------------------------------------------------------
// The canonical projection of our model: the Rust twin of the oracle's
// `packageSpecCanonical` and friends. Properties NuGet's types have that our
// model does not are ones whose keys decline, so they are NuGet's defaults.
// ---------------------------------------------------------------------------

fn framework_canonical(f: &NuGetFramework) -> Value {
    json!({
        "framework": f.framework(),
        "version": f.version_string(),
        "platform": f.platform().unwrap_or_default(),
        "platformVersion": f.platform_version_string(),
        "profile": f.profile().unwrap_or_default(),
    })
}

fn codes(codes: impl IntoIterator<Item = NuGetLogCode>) -> Value {
    codes.into_iter().map(|c| c.as_str()).collect()
}

fn restore_canonical(m: &ProjectRestoreMetadata) -> Value {
    let w = &m.warning_properties;
    let l = &m.restore_lock_properties;
    json!({
        "projectStyle": "PackageReference",
        "projectPath": m.project_path,
        "projectJsonPath": null,
        "outputPath": m.output_path,
        "projectName": m.project_name,
        "projectUniqueName": m.project_unique_name,
        "sources": m.sources.iter().map(|s| json!({
            "name": s, "source": s, "isEnabled": true, "protocolVersion": 2,
        })).collect::<Vec<_>>(),
        "packagesPath": m.packages_path,
        "cacheFilePath": null,
        "fallbackFolders": m.fallback_folders,
        "configFilePaths": m.config_file_paths,
        "frameworks": m.frameworks.iter().map(|f| json!({
            "targetAlias": f.target_alias,
            "framework": framework_canonical(&f.framework),
            "projectReferences": f.project_references.iter().map(|r| json!({
                "projectUniqueName": r.project_unique_name,
                "projectPath": r.project_path,
                "includeAssets": r.include_assets.bits(),
                "excludeAssets": r.exclude_assets.bits(),
                "privateAssets": r.private_assets.bits(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "originalTargetFrameworks": m.original_target_frameworks,
        "crossTargeting": m.cross_targeting,
        "legacyPackagesDirectory": false,
        "files": 0,
        "validateRuntimeAssets": false,
        "skipContentFileWrite": m.skip_content_file_write,
        "warningProperties": {
            "allWarningsAsErrors": w.all_warnings_as_errors,
            "warningsAsErrors": codes(w.warnings_as_errors.iter().copied()),
            "noWarn": codes(w.no_warn.iter().copied()),
            "warningsNotAsErrors": codes(w.warnings_not_as_errors.iter().copied()),
        },
        "restoreLockProperties": {
            "restorePackagesWithLockFile": l.restore_packages_with_lock_file,
            "nuGetLockFilePath": l.nuget_lock_file_path,
            "restoreLockedMode": l.restore_locked_mode,
        },
        "centralPackageVersionsEnabled": m.central_package_versions_enabled,
        "centralPackageVersionOverrideDisabled": false,
        "centralPackageFloatingVersionsEnabled": false,
        "centralPackageTransitivePinningEnabled": m.central_package_transitive_pinning_enabled,
        "restoreAuditProperties": m.restore_audit_properties.as_ref().map(|a| json!({
            "enableAudit": a.enable_audit,
            "auditLevel": a.audit_level,
            "auditMode": a.audit_mode,
            "suppressedAdvisories": a.suppressed_advisories,
        })),
        "sdkAnalysisLevel": m.sdk_analysis_level.as_ref().map(|v| v.to_full_string()),
        "usingMicrosoftNETSdk": m.using_microsoft_net_sdk,
        "useLegacyDependencyResolver": false,
        "restoreDoNotWriteDependencyGraphSpec": false,
    })
}

fn target_framework_canonical(t: &TargetFrameworkInformation) -> Value {
    let id_ranges = |entries: &[borzoi_nuget::dgspec::PackageRangeEntry]| -> Value {
        entries
            .iter()
            .map(|e| {
                json!({
                    "key": e.id.as_str(),
                    "name": e.id.as_str(),
                    "range": e.range.to_normalized_string(),
                })
            })
            .collect()
    };
    json!({
        "targetAlias": t.target_alias,
        "framework": framework_canonical(&t.framework),
        "dependencies": t.dependencies.iter().map(|d| json!({
            "name": d.id.as_str(),
            "range": d.range.to_normalized_string(),
            "target": d.target.bits(),
            "include": d.include.bits(),
            "suppressParent": d.suppress_parent.bits(),
            "noWarn": codes(d.no_warn.iter().copied()),
            "autoReferenced": d.auto_referenced,
            "versionCentrallyManaged": d.version_centrally_managed,
            "referenceType": "Direct",
            "generatePathProperty": d.generate_path_property,
            "aliases": d.aliases,
            "versionOverride": d.version_override.as_ref().map(|r| r.to_normalized_string()),
        })).collect::<Vec<_>>(),
        "imports": t.imports.iter().map(framework_canonical).collect::<Vec<_>>(),
        "assetTargetFallback": t.asset_target_fallback,
        "warn": t.warn,
        "downloadDependencies": t.download_dependencies.iter().map(|d| json!({
            "name": d.id.as_str(),
            "range": d.range.to_normalized_string(),
        })).collect::<Vec<_>>(),
        "centralPackageVersions": id_ranges(&t.central_package_versions),
        "frameworkReferences": t.framework_references.iter().map(|f| json!({
            "name": f.name,
            "privateAssets": f.private_assets.bits(),
        })).collect::<Vec<_>>(),
        "packagesToPrune": id_ranges(&t.packages_to_prune),
        "runtimeIdentifierGraphPath": t.runtime_identifier_graph_path,
    })
}

fn package_spec_canonical(p: &PackageSpec) -> Value {
    json!({
        "name": p.restore.project_name,
        "filePath": p.restore.project_path,
        "version": p.version.to_full_string(),
        "isDefaultVersion": p.is_default_version,
        "runtimeGraphEntries": 0,
        "hideWarningsAndErrors": false,
        "restore": restore_canonical(&p.restore),
        "frameworks": p.frameworks.iter().map(target_framework_canonical).collect::<Vec<_>>(),
    })
}

fn canonical(spec: &DependencyGraphSpec) -> Value {
    json!({
        "restore": spec.restore,
        "projects": spec.projects.iter().map(package_spec_canonical).collect::<Vec<_>>(),
    })
}

/// Sort the arrays whose order carries no meaning, on either side, so the
/// comparison sees sets as sets. Arrays left alone (sources, fallback folders,
/// imports, framework lists, …) are compared in order.
fn normalise(spec: &mut Value) {
    fn sort(v: &mut Value) {
        if let Value::Array(items) = v {
            items.sort_by_cached_key(|item| item.to_string());
        }
    }
    fn each(v: &mut Value, f: impl Fn(&mut Value)) {
        if let Value::Array(items) = v {
            items.iter_mut().for_each(f);
        }
    }
    sort(&mut spec["restore"]);
    each(&mut spec["projects"], |p| {
        each(&mut p["frameworks"], |f| {
            each(&mut f["dependencies"], |d| sort(&mut d["noWarn"]));
            for key in [
                "dependencies",
                "downloadDependencies",
                "centralPackageVersions",
                "frameworkReferences",
                "packagesToPrune",
            ] {
                sort(&mut f[key]);
            }
        });
        let restore = &mut p["restore"];
        if restore.is_object() {
            each(&mut restore["frameworks"], |f| {
                sort(&mut f["projectReferences"])
            });
            let warnings = &mut restore["warningProperties"];
            if warnings.is_object() {
                for key in ["warningsAsErrors", "noWarn", "warningsNotAsErrors"] {
                    sort(&mut warnings[key]);
                }
            }
            let audit = &mut restore["restoreAuditProperties"];
            if audit.is_object() {
                sort(&mut audit["suppressedAdvisories"]);
            }
        }
    });
    sort(&mut spec["projects"]);
}

/// The first place two canonical values differ, for a readable failure.
fn first_difference(ours: &Value, theirs: &Value, path: &str) -> Option<String> {
    match (ours, theirs) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).collect();
            keys.into_iter().find_map(|k| {
                first_difference(
                    a.get(k).unwrap_or(&Value::Null),
                    b.get(k).unwrap_or(&Value::Null),
                    &format!("{path}.{k}"),
                )
            })
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .enumerate()
            .find_map(|(i, (x, y))| first_difference(x, y, &format!("{path}[{i}]"))),
        _ if ours == theirs => None,
        _ => Some(format!("{path}: ours {ours} / NuGet {theirs}")),
    }
}

// ---------------------------------------------------------------------------
// Grading one document.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    /// We committed, NuGet loaded, and the projections are equal.
    Agree,
    /// We declined a document NuGet loads.
    DeclinedLoadable,
    /// We declined a document NuGet refuses.
    DeclinedRefused,
}

fn grade(oracle: &mut Oracle, text: &str) -> Result<(Outcome, Option<DgSpecDecline>), String> {
    let ours = parse_dgspec(text);
    let response = oracle.request(&json!({"op": "normaliseDgSpec", "text": text}));
    let loaded = response["ok"]
        .as_bool()
        .expect("normaliseDgSpec reports ok");
    match ours {
        Ok(spec) if loaded => {
            let mut ours = canonical(&spec);
            let mut theirs = response["spec"].clone();
            normalise(&mut ours);
            normalise(&mut theirs);
            match first_difference(&ours, &theirs, "$") {
                None => Ok((Outcome::Agree, None)),
                Some(difference) => Err(format!("we read it differently: {difference}")),
            }
        }
        Ok(_) => Err(format!(
            "we read a document NuGet refuses ({}: {})",
            response["exceptionType"], response["message"]
        )),
        Err(decline) if loaded => Ok((Outcome::DeclinedLoadable, Some(decline))),
        Err(decline) => Ok((Outcome::DeclinedRefused, Some(decline))),
    }
}

fn assert_sound(oracle: &mut Oracle, label: &str, text: &str) -> (Outcome, Option<DgSpecDecline>) {
    match grade(oracle, text) {
        Ok(outcome) => outcome,
        Err(violation) => panic!("{label}: {violation}\ndocument: {text}"),
    }
}

// ---------------------------------------------------------------------------
// Documents.
// ---------------------------------------------------------------------------

/// The smallest spec that exercises a project, its restore metadata, a
/// framework and a dependency.
fn minimal() -> Value {
    json!({
        "format": 1,
        "restore": {"/r/P.fsproj": {}},
        "projects": {"/r/P.fsproj": {
            "version": "1.0.0",
            "restore": {
                "projectUniqueName": "/r/P.fsproj",
                "projectName": "P",
                "projectPath": "/r/P.fsproj",
                "outputPath": "/r/obj/",
                "projectStyle": "PackageReference",
            },
            "frameworks": {"net8.0": {
                "targetAlias": "net8.0",
                "dependencies": {"A": {"target": "Package", "version": "[1.0.0, )"}},
            }},
        }},
    })
}

/// The shape the .NET 10 SDK writes for a multi-targeting CPM project with a
/// project reference: every key the on-disk census of real dgspecs found
/// commonly, except `runtimes`.
fn realistic() -> Value {
    let lib = "/r/lib/lib.fsproj";
    let app = "/r/app/app.fsproj";
    let restore_meta = |unique: &str, name: &str, tfms: &[&str], refs: Value| {
        json!({
            "projectUniqueName": unique,
            "projectName": name,
            "projectPath": unique,
            "packagesPath": "/home/u/.nuget/packages/",
            "outputPath": format!("{}/obj/", unique.rsplit_once('/').unwrap().0),
            "projectStyle": "PackageReference",
            "crossTargeting": true,
            "centralPackageVersionsManagementEnabled": true,
            "CentralPackageTransitivePinningEnabled": true,
            "configFilePaths": ["/home/u/.nuget/NuGet/NuGet.Config"],
            "originalTargetFrameworks": tfms,
            "sources": {"/sdk/FSharp/library-packs": {}, "https://api.nuget.org/v3/index.json": {}},
            "fallbackFolders": ["/fallback"],
            "frameworks": refs,
            "warningProperties": {
                "allWarningsAsErrors": true,
                "warnAsError": ["NU1605"],
                "noWarn": ["NU1701"],
                "warnNotAsError": ["NU1901", "NU1902"],
            },
            "restoreAuditProperties": {
                "enableAudit": "true",
                "auditLevel": "low",
                "auditMode": "all",
                "suppressedAdvisories": {"https://github.com/advisories/GHSA-0000-0000-0000": null},
            },
            "restoreLockProperties": {"restorePackagesWithLockFile": "false"},
            "SdkAnalysisLevel": "10.0.400",
        })
    };
    let cpv = json!({"Newtonsoft.Json": "13.0.3", "System.Text.Json": "8.0.5"});
    let prune = json!({"Microsoft.CSharp": "(,4.7.32767]", "System.Text.Json": "(,10.0.32767]"});
    let imports = json!([
        "net461", "net462", "net47", "net471", "net472", "net48", "net481"
    ]);
    json!({
        "format": 1,
        "restore": {app: {}},
        "projects": {
            app: {
                "version": "1.0.0",
                "restore": restore_meta(app, "app", &["net10.0", "net8.0"], json!({
                    "net10.0": {"framework": "net10.0", "targetAlias": "net10.0", "projectReferences": {
                        lib: {"projectPath": lib, "privateAssets": "all", "excludeAssets": "compile"},
                    }},
                    "net8.0": {"framework": "net8.0", "targetAlias": "net8.0", "projectReferences": {
                        lib: {"projectPath": lib},
                    }},
                })),
                "frameworks": {
                    "net10.0": {
                        "framework": "net10.0", "targetAlias": "net10.0",
                        "dependencies": {
                            "System.Text.Json": {
                                "target": "Package", "version": "[8.0.5, )",
                                "generatePathProperty": true, "versionCentrallyManaged": true,
                            },
                            "FSharp.Core": {
                                "include": "Runtime, Compile, Build, Native, Analyzers, BuildTransitive",
                                "suppressParent": "All", "target": "Package", "version": "[10.1.402, )",
                                "autoReferenced": true, "noWarn": ["NU1510"],
                            },
                        },
                        "centralPackageVersions": cpv,
                        "imports": imports, "assetTargetFallback": true, "warn": true,
                        "downloadDependencies": [{"name": "Foo.Bar", "version": "[1.0.0, 1.0.0]"}],
                        "frameworkReferences": {"Microsoft.NETCore.App": {"privateAssets": "all"}},
                        "packagesToPrune": prune,
                        "runtimeIdentifierGraphPath": "/sdk/RuntimeIdentifierGraph.json",
                    },
                    "net8.0": {
                        "framework": "net8.0", "targetAlias": "net8.0",
                        "dependencies": {"System.Text.Json": {
                            "target": "Package", "version": "[8.0.5, )", "versionCentrallyManaged": true,
                            "versionOverride": "[8.0.4, )",
                        }},
                        "centralPackageVersions": cpv,
                        "imports": imports, "assetTargetFallback": true, "warn": true,
                        "downloadDependencies": [
                            {"name": "Microsoft.NETCore.App.Ref", "version": "[8.0.32, 8.0.32]"},
                        ],
                        "frameworkReferences": {"Microsoft.NETCore.App": {"privateAssets": "all"}},
                        "packagesToPrune": prune,
                    },
                },
            },
            lib: {
                "version": "1.0.0",
                "restore": restore_meta(lib, "lib", &["netstandard2.0"], json!({
                    "netstandard2.0": {"framework": "netstandard2.0", "targetAlias": "netstandard2.0", "projectReferences": {}},
                })),
                "frameworks": {"netstandard2.0": {
                    "framework": "netstandard2.0", "targetAlias": "netstandard2.0",
                    "dependencies": {"NETStandard.Library": {
                        "suppressParent": "All", "target": "Package", "version": "[2.0.3, )",
                        "autoReferenced": true,
                    }},
                    "imports": imports, "assetTargetFallback": true, "warn": true,
                }},
            },
        },
    })
}

/// A JSON tree that can hold what `serde_json::Value` cannot: repeated keys.
#[derive(Debug, Clone)]
enum Node {
    Leaf(Value),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}

impl Node {
    fn from_value(v: &Value) -> Node {
        match v {
            Value::Array(items) => Node::Array(items.iter().map(Node::from_value).collect()),
            Value::Object(members) => Node::Object(
                members
                    .iter()
                    .map(|(k, v)| (k.clone(), Node::from_value(v)))
                    .collect(),
            ),
            leaf => Node::Leaf(leaf.clone()),
        }
    }

    fn render(&self, out: &mut String) {
        match self {
            Node::Leaf(v) => out.push_str(&v.to_string()),
            Node::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.render(out);
                }
                out.push(']');
            }
            Node::Object(members) => {
                out.push('{');
                for (i, (k, v)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    v.render(out);
                }
                out.push('}');
            }
        }
    }

    fn text(&self) -> String {
        let mut out = String::new();
        self.render(&mut out);
        out
    }

    fn at_mut(&mut self, path: &[Step]) -> &mut Node {
        match path.split_first() {
            None => self,
            Some((step, rest)) => match (self, step) {
                (Node::Array(items), Step::Index(i)) => items[*i].at_mut(rest),
                (Node::Object(members), Step::Member(i)) => members[*i].1.at_mut(rest),
                _ => unreachable!("paths come from this tree"),
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Step {
    Index(usize),
    Member(usize),
}

/// Every node in `node` by its path, with the key it sits under (an array item
/// inherits its array's key), and whether it is an object.
fn sites(node: &Node) -> Vec<(Vec<Step>, String, bool)> {
    fn walk(
        node: &Node,
        path: &mut Vec<Step>,
        key: &str,
        out: &mut Vec<(Vec<Step>, String, bool)>,
    ) {
        out.push((
            path.clone(),
            key.to_owned(),
            matches!(node, Node::Object(_)),
        ));
        match node {
            Node::Leaf(_) => {}
            Node::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    path.push(Step::Index(i));
                    walk(item, path, key, out);
                    path.pop();
                }
            }
            Node::Object(members) => {
                for (i, (k, v)) in members.iter().enumerate() {
                    path.push(Step::Member(i));
                    walk(v, path, k, out);
                    path.pop();
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(node, &mut Vec::new(), "", &mut out);
    out
}

// ---------------------------------------------------------------------------
// Value pools, keyed by what a field holds.
// ---------------------------------------------------------------------------

const FLAG_SPELLINGS: &[&str] = &[
    "All",
    "all",
    "None",
    "none",
    "",
    "Compile",
    "compile, runtime",
    "Runtime, Compile, Build, Native, Analyzers, BuildTransitive",
    "ContentFiles",
    "contentfiles",
    "BUILDTRANSITIVE",
    "BuildTransitive",
    "buildtransitive",
    "Compile, BuildTransitive",
    "Build",
    "Compile,,Runtime",
    "Compile, ,Runtime",
    " Compile ",
    "\tCompile",
    "Compile,",
    ",Compile",
    "1",
    "Compile;Runtime",
    "Compile|Runtime",
    "Bogus",
    "Compile, Bogus",
    "None, Compile",
    "All, None",
    "Compile\u{a0}",
];

const TARGET_SPELLINGS: &[&str] = &[
    "Package",
    "package",
    "PACKAGE",
    " Package",
    "Package ",
    "Project",
    "project",
    "ExternalProject",
    "Assembly",
    "Reference",
    "WinMD",
    "All",
    "None",
    "",
    "Package,Project",
    "Package, Project",
    "1",
    "Bogus",
    "PackageProjectExternal",
];

const CODE_SPELLINGS: &[&str] = &[
    "NU1605",
    "NU1603",
    "NU1701",
    "NU1510",
    "NU1901",
    "nu1605",
    " NU1605",
    "NU1605 ",
    "1605",
    "NU9999",
    "CS0168",
    "NU1605,NU1603",
    "Undefined",
    "",
    "NU5500",
];

const FRAMEWORK_FLAG_SPELLINGS: &[&str] = &[
    "all",
    "All",
    "ALL",
    "none",
    "None",
    "",
    "compile",
    "all, none",
    " all",
];

fn shape_confusion(rng: &mut SplitMix64) -> Value {
    rng.pick(&[
        json!(null),
        json!(true),
        json!(false),
        json!(0),
        json!(1),
        json!(1.5),
        json!("true"),
        json!("false"),
        json!(""),
        json!(" "),
        json!([]),
        json!({}),
        json!(["x"]),
    ])
    .clone()
}

/// A replacement for the value under `key`, drawn mostly from spellings of
/// what that key holds and sometimes from values of the wrong shape.
fn replacement(rng: &mut SplitMix64, key: &str, parent: &str) -> Value {
    if rng.below(6) == 0 {
        return shape_confusion(rng);
    }
    let pick = |rng: &mut SplitMix64, pool: &[&str]| json!(rng.pick(pool));
    match key {
        "include" | "exclude" | "suppressParent" | "includeAssets" | "excludeAssets" => {
            pick(rng, FLAG_SPELLINGS)
        }
        "privateAssets" if parent == "frameworkReferences" => pick(rng, FRAMEWORK_FLAG_SPELLINGS),
        "privateAssets" => pick(rng, FLAG_SPELLINGS),
        "target" => pick(rng, TARGET_SPELLINGS),
        "warnAsError" | "noWarn" | "warnNotAsError" => pick(rng, CODE_SPELLINGS),
        "version" | "versionOverride" => match rng.below(3) {
            0 => json!(gen_version_string(rng)),
            _ => json!(gen_range_string(rng)),
        },
        "SdkAnalysisLevel" => json!(gen_version_string(rng)),
        "framework" | "imports" | "originalTargetFrameworks" => json!(gen_framework_string(rng)),
        "format" => rng
            .pick(&[json!(1), json!(2), json!(0), json!(1.0), json!("1")])
            .clone(),
        "projectStyle" => rng
            .pick(&[
                json!("PackageReference"),
                json!("packagereference"),
                json!("ProjectJson"),
                json!("Unknown"),
                json!(""),
            ])
            .clone(),
        _ if parent == "centralPackageVersions" || parent == "packagesToPrune" => {
            json!(gen_range_string(rng))
        }
        _ => match rng.below(4) {
            0 => json!(true),
            1 => json!(false),
            _ => json!(rng.pick(&["", "x", " /r/P.fsproj", "/r/P.fsproj", "C:\\r\\P.fsproj"])),
        },
    }
}

/// A respelling of a name-keyed member's key: a package id, a framework alias,
/// a project unique name.
fn respelt_key(rng: &mut SplitMix64, key: &str, parent: &str) -> String {
    if parent == "frameworks" {
        return gen_framework_string(rng);
    }
    match rng.below(7) {
        0 => key.to_ascii_lowercase(),
        1 => key.to_ascii_uppercase(),
        2 => format!(" {key}"),
        3 => format!("{key} "),
        4 => format!("{key}..x"),
        5 => String::new(),
        _ => format!("{key}\u{e9}"),
    }
}

// ---------------------------------------------------------------------------
// The generator.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mutation {
    Replace,
    Remove,
    RecaseKey,
    RespellKey,
    Duplicate,
    /// A sibling copy under a respelt key: two framework references, sources or
    /// aliases equal up to case or whitespace, which the reader may merge.
    DuplicateRespelt,
    InsertUnmodelled,
}

const MUTATIONS: &[Mutation] = &[
    Mutation::Replace,
    Mutation::Remove,
    Mutation::RecaseKey,
    Mutation::RespellKey,
    Mutation::Duplicate,
    Mutation::DuplicateRespelt,
    Mutation::InsertUnmodelled,
];

/// Keys whose objects are keyed by name rather than by a fixed vocabulary.
const NAME_KEYED: &[&str] = &[
    "restore",
    "projects",
    "frameworks",
    "dependencies",
    "centralPackageVersions",
    "packagesToPrune",
    "frameworkReferences",
    "projectReferences",
    "sources",
    "suppressedAdvisories",
];

fn mutate(rng: &mut SplitMix64, doc: &mut Node, mutation: Mutation) {
    let all = sites(doc);
    // The key of each site's parent, for name-keyed decisions.
    let parent_key = |path: &[Step]| -> String {
        all.iter()
            .find(|(p, _, _)| p.len() + 1 == path.len() && path.starts_with_steps(p))
            .map(|(_, k, _)| k.clone())
            .unwrap_or_default()
    };
    let members: Vec<_> = all
        .iter()
        .filter(|(p, _, _)| matches!(p.last(), Some(Step::Member(_))))
        .collect();
    match mutation {
        Mutation::Replace => {
            let leaves: Vec<_> = all
                .iter()
                .filter(|(p, _, _)| matches!(doc.clone().at_mut(p), Node::Leaf(_)) && !p.is_empty())
                .collect();
            let (path, key, _) = rng.pick(&leaves);
            let parent = parent_key(path);
            let value = replacement(rng, key, &parent);
            *doc.at_mut(path) = Node::Leaf(value);
        }
        Mutation::Remove
        | Mutation::RecaseKey
        | Mutation::RespellKey
        | Mutation::Duplicate
        | Mutation::DuplicateRespelt => {
            let (path, _, _) = rng.pick(&members);
            let parent = parent_key(path);
            let (parent_path, last) = path.split_at(path.len() - 1);
            let Step::Member(index) = last[0] else {
                unreachable!()
            };
            let Node::Object(obj) = doc.at_mut(parent_path) else {
                unreachable!()
            };
            match mutation {
                Mutation::Remove => {
                    obj.remove(index);
                }
                Mutation::RecaseKey => {
                    let key = &mut obj[index].0;
                    *key = match key.chars().next() {
                        Some(c) if c.is_ascii_lowercase() => {
                            c.to_ascii_uppercase().to_string() + &key[1..]
                        }
                        Some(c) => c.to_ascii_lowercase().to_string() + &key[c.len_utf8()..],
                        None => "x".to_owned(),
                    };
                }
                Mutation::RespellKey => {
                    let respelt = respelt_key(rng, &obj[index].0, &parent);
                    obj[index].0 = respelt;
                }
                Mutation::Duplicate => {
                    let copy = obj[index].clone();
                    obj.push(copy);
                }
                Mutation::DuplicateRespelt => {
                    let (key, value) = obj[index].clone();
                    obj.push((respelt_key(rng, &key, &parent), value));
                }
                _ => unreachable!(),
            }
        }
        Mutation::InsertUnmodelled => {
            let objects: Vec<_> = all.iter().filter(|(_, _, is_object)| *is_object).collect();
            let (path, key, _) = rng.pick(&objects);
            let Node::Object(obj) = doc.at_mut(path) else {
                unreachable!()
            };
            let name = if NAME_KEYED.contains(&key.as_str()) {
                "Extra.Package"
            } else {
                rng.pick(&[
                    "zzUnmodelled",
                    "cacheFilePath",
                    "restoreUseLegacyDependencyResolver",
                    "runtimes",
                    "files",
                ])
            };
            obj.push((name.to_owned(), Node::Leaf(shape_confusion(rng))));
        }
    }
}

trait StartsWithSteps {
    fn starts_with_steps(&self, prefix: &[Step]) -> bool;
}

impl StartsWithSteps for [Step] {
    fn starts_with_steps(&self, prefix: &[Step]) -> bool {
        self.len() >= prefix.len()
            && self.iter().zip(prefix).all(|(a, b)| match (a, b) {
                (Step::Index(x), Step::Index(y)) | (Step::Member(x), Step::Member(y)) => x == y,
                _ => false,
            })
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// Edit a copy of `doc` with `f`.
fn with(doc: &Value, f: impl FnOnce(&mut Value)) -> String {
    let mut doc = doc.clone();
    f(&mut doc);
    doc.to_string()
}

fn dep(doc: &mut Value) -> &mut Value {
    &mut doc["projects"]["/r/P.fsproj"]["frameworks"]["net8.0"]["dependencies"]["A"]
}

fn fw(doc: &mut Value) -> &mut Value {
    &mut doc["projects"]["/r/P.fsproj"]["frameworks"]["net8.0"]
}

fn meta(doc: &mut Value) -> &mut Value {
    &mut doc["projects"]["/r/P.fsproj"]["restore"]
}

/// Documents that must commit, each a reading rule NuGet's reader applies that
/// a naive parser gets wrong; and documents that must decline. Every one is
/// also held to certain-implies-exact.
#[test]
fn hand_cases() {
    let m = minimal();
    let commits: Vec<(&str, String)> = vec![
        ("minimal", m.to_string()),
        ("realistic", realistic().to_string()),
        ("bom", format!("\u{feff}{m}")),
        (
            "target absent is 47, not Package",
            with(&m, |d| {
                dep(d).as_object_mut().unwrap().remove("target");
            }),
        ),
        (
            "target ignores case",
            with(&m, |d| dep(d)["target"] = json!("package")),
        ),
        (
            "target Project",
            with(&m, |d| dep(d)["target"] = json!("Project")),
        ),
        (
            "include ignores case",
            with(&m, |d| dep(d)["include"] = json!("compile, runtime")),
        ),
        (
            "include empty is none",
            with(&m, |d| dep(d)["include"] = json!("")),
        ),
        (
            "exclude folds into include",
            with(&m, |d| {
                dep(d)["include"] = json!("Compile, Runtime");
                dep(d)["exclude"] = json!("Compile");
            }),
        ),
        (
            "suppressParent ignores case",
            with(&m, |d| dep(d)["suppressParent"] = json!("all")),
        ),
        (
            "framework field overrides alias",
            with(&m, |d| fw(d)["framework"] = json!("net6.0")),
        ),
        (
            "floating dependency range",
            with(&m, |d| dep(d)["version"] = json!("1.*")),
        ),
        (
            "project version absent is the default",
            with(&m, |d| {
                d["projects"]["/r/P.fsproj"]
                    .as_object_mut()
                    .unwrap()
                    .remove("version");
            }),
        ),
        (
            "project version with metadata",
            with(&m, |d| {
                d["projects"]["/r/P.fsproj"]["version"] = json!("1.0.0-beta+meta");
            }),
        ),
        (
            "restore names an absent project",
            with(&m, |d| d["restore"]["/r/Q.fsproj"] = json!({})),
        ),
        (
            "one name with a non-ASCII cased letter",
            with(&m, |d| {
                d["restore"] = json!({"/r/\u{e9}/P.fsproj": {}});
            }),
        ),
        (
            "bare central version is a minimum",
            with(&m, |d| {
                fw(d)["centralPackageVersions"] = json!({"A": "1.0.0", "B": "[2.0.0]"});
            }),
        ),
        (
            "framework reference flags",
            with(&m, |d| {
                fw(d)["frameworkReferences"] =
                    json!({"X": {"privateAssets": "none"}, "Y": {"privateAssets": "All"}, "Z": {}});
            }),
        ),
        (
            "project reference defaults",
            with(&m, |d| {
                meta(d)["frameworks"] = json!({"net8.0": {"targetAlias": "net8.0",
                "projectReferences": {"/q.fsproj": {"projectPath": "/q.fsproj"}}}});
            }),
        ),
        (
            "advisory as an empty object",
            with(&m, |d| {
                meta(d)["restoreAuditProperties"] = json!({"suppressedAdvisories": {"https://github.com/advisories/GHSA-0000-0000-0000": {}}});
            }),
        ),
        (
            "empty project reference flags are the defaults",
            with(&m, |d| {
                meta(d)["frameworks"] = json!({"net8.0": {"targetAlias": "net8.0",
                    "projectReferences": {"/q.fsproj": {"projectPath": "/q.fsproj",
                        "includeAssets": "", "excludeAssets": "", "privateAssets": ""}}}});
            }),
        ),
        (
            "audit without advisories",
            with(&m, |d| {
                meta(d)["restoreAuditProperties"] = json!({"enableAudit": "true"});
            }),
        ),
        (
            "warning lists",
            with(&m, |d| {
                meta(d)["warningProperties"] =
                    json!({"warnAsError": ["NU1605", "NU1603"], "noWarn": ["NU1701"]});
            }),
        ),
        (
            "BuildTransitive implies Build",
            with(&m, |d| dep(d)["suppressParent"] = json!("buildtransitive")),
        ),
        (
            "excluding BuildTransitive excludes Build",
            with(&m, |d| dep(d)["exclude"] = json!("BuildTransitive")),
        ),
        (
            "empty advisory set is no set",
            with(&m, |d| {
                meta(d)["restoreAuditProperties"] =
                    json!({"enableAudit": "true", "suppressedAdvisories": {}});
            }),
        ),
        (
            "targetAlias absent is empty",
            with(&m, |d| {
                fw(d).as_object_mut().unwrap().remove("targetAlias");
            }),
        ),
        (
            "no restore metadata frameworks",
            with(&m, |d| {
                meta(d).as_object_mut().unwrap().remove("outputPath");
                meta(d).as_object_mut().unwrap().remove("projectName");
            }),
        ),
    ];
    let declines: Vec<(&str, String)> = vec![
        (
            "comment",
            m.to_string()
                .replacen("{\"format\"", "{/* c */\"format\"", 1),
        ),
        (
            "trailing comma",
            format!("{},}}", &m.to_string()[..m.to_string().len() - 1]),
        ),
        ("unknown top-level key", with(&m, |d| d["bogus"] = json!(1))),
        (
            "unknown dependency key",
            with(&m, |d| dep(d)["bogus"] = json!(1)),
        ),
        (
            "misspelt flag is dropped by NuGet",
            with(&m, |d| dep(d)["include"] = json!("Compile, Bogus")),
        ),
        (
            "string where a boolean belongs",
            with(&m, |d| dep(d)["autoReferenced"] = json!("true")),
        ),
        (
            "lower-case warning code is dropped by NuGet",
            with(&m, |d| {
                meta(d)["warningProperties"] = json!({"warnAsError": ["nu1605"]});
            }),
        ),
        (
            "numeric warning code",
            with(&m, |d| {
                meta(d)["warningProperties"] = json!({"warnAsError": ["1605"]});
            }),
        ),
        (
            "dependency without a range",
            with(&m, |d| {
                dep(d).as_object_mut().unwrap().remove("version");
            }),
        ),
        (
            "key-cased Dependencies",
            with(&m, |d| {
                let deps = fw(d)
                    .as_object_mut()
                    .unwrap()
                    .remove("dependencies")
                    .unwrap();
                fw(d)["Dependencies"] = deps;
            }),
        ),
        ("format 2", with(&m, |d| d["format"] = json!(2))),
        (
            "project key is not its unique name",
            with(&m, |d| {
                let p = d["projects"]
                    .as_object_mut()
                    .unwrap()
                    .remove("/r/P.fsproj")
                    .unwrap();
                d["projects"]["/r/Other.fsproj"] = p;
            }),
        ),
        (
            "package ids equal ignoring case",
            with(&m, |d| {
                fw(d)["dependencies"]["a"] = json!({"target": "Package", "version": "[2.0.0, )"});
            }),
        ),
        (
            "empty import is skipped by NuGet",
            with(&m, |d| fw(d)["imports"] = json!(["net461", ""])),
        ),
        (
            "unsupported import is refused by NuGet",
            with(&m, |d| fw(d)["imports"] = json!(["banana"])),
        ),
        (
            "tab beside a flag name",
            with(&m, |d| dep(d)["suppressParent"] = json!("\tCompile")),
        ),
        (
            "agnostic import is refused by NuGet",
            with(&m, |d| fw(d)["imports"] = json!(["agnostic"])),
        ),
        (
            "unnamed framework reference is refused by NuGet",
            with(&m, |d| {
                fw(d)["frameworkReferences"] = json!({"": {"privateAssets": "all"}});
            }),
        ),
        (
            "unsupported alias",
            with(&m, |d| {
                let f = fw(d).take();
                d["projects"]["/r/P.fsproj"]["frameworks"] = json!({"banana": f});
            }),
        ),
        (
            "platform NuGet salvages and we do not",
            with(&m, |d| {
                let mut f = fw(d).take();
                f.as_object_mut().unwrap().remove("targetAlias");
                d["projects"]["/r/P.fsproj"]["frameworks"] = json!({"net8.0-1": f});
            }),
        ),
        (
            "malformed portable alias",
            with(&m, |d| {
                let f = fw(d).take();
                d["projects"]["/r/P.fsproj"]["frameworks"] = json!({"portable.-net45+win8": f});
            }),
        ),
        (
            "framework references equal ignoring case",
            with(&m, |d| {
                fw(d)["frameworkReferences"] = json!({
                    "Microsoft.NETCore.App": {"privateAssets": "all"},
                    "microsoft.netcore.app": {"privateAssets": "all"},
                });
            }),
        ),
        (
            "restore names equal ignoring case",
            with(&m, |d| d["restore"]["/R/P.FSPROJ"] = json!({})),
        ),
        (
            "two names, one with a non-ASCII cased letter",
            with(&m, |d| {
                d["restore"]["/r/Q\u{e9}.fsproj"] = json!({});
            }),
        ),
        (
            "runtimes",
            with(&m, |d| {
                d["projects"]["/r/P.fsproj"]["runtimes"] = json!({"linux-x64": {"#import": []}});
            }),
        ),
        (
            "repeated JSON key",
            m.to_string().replacen(
                "\"target\":\"Package\"",
                "\"target\":\"Project\",\"target\":\"Package\"",
                1,
            ),
        ),
    ];

    let mut oracle = Oracle::spawn();
    for (label, text) in &commits {
        let (outcome, decline) = assert_sound(&mut oracle, label, text);
        assert_eq!(
            outcome,
            Outcome::Agree,
            "{label} must commit, but declined: {}\ndocument: {text}",
            decline.map(|d| d.to_string()).unwrap_or_default()
        );
    }
    for (label, text) in &declines {
        let (outcome, _) = assert_sound(&mut oracle, label, text);
        assert_ne!(
            outcome,
            Outcome::Agree,
            "{label} must decline\ndocument: {text}"
        );
    }
}

/// Random mutations of the two templates, each held to certain-implies-exact.
/// Every mutation kind must have produced at least one agreement, or the
/// generator is only exercising declines. `BORZOI_DGSPEC_DIFF_CASES` and
/// `BORZOI_DGSPEC_DIFF_SEED` widen or move the run.
#[test]
fn mutated_specs_are_read_as_nuget_reads_them() {
    let cases: usize = std::env::var("BORZOI_DGSPEC_DIFF_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let templates = [Node::from_value(&minimal()), Node::from_value(&realistic())];
    let seed = std::env::var("BORZOI_DGSPEC_DIFF_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x0d65_bec0_0001);
    let mut rng = SplitMix64(seed);
    let mut oracle = Oracle::spawn();
    let mut census: BTreeMap<(Mutation, Outcome), usize> = BTreeMap::new();
    let mut violations: Vec<(String, String)> = Vec::new();

    for _ in 0..cases {
        let mut doc = rng.pick(&templates).clone();
        let first = *rng.pick(MUTATIONS);
        mutate(&mut rng, &mut doc, first);
        if rng.below(3) == 0 {
            let second = *rng.pick(MUTATIONS);
            mutate(&mut rng, &mut doc, second);
        }
        let text = doc.text();
        match grade(&mut oracle, &text) {
            Ok((outcome, _)) => *census.entry((first, outcome)).or_default() += 1,
            Err(violation) => violations.push((format!("{first:?}: {violation}"), text)),
        }
    }

    eprintln!("dgspec mutation census (first mutation, outcome): {census:?}");
    assert!(
        violations.is_empty(),
        "{} of {cases} mutated specs read differently from NuGet:\n{}",
        violations.len(),
        by_kind(&violations)
    );
    for mutation in MUTATIONS {
        assert!(
            census.contains_key(&(*mutation, Outcome::Agree)),
            "no {mutation:?} mutation produced an agreement: the generator is only exercising declines"
        );
    }
}

/// Violations grouped by their message with digits and quoted values elided,
/// each kind shown once with its count and one example document.
fn by_kind(violations: &[(String, String)]) -> String {
    let mut kinds: BTreeMap<String, (usize, &str, &str)> = BTreeMap::new();
    for (message, text) in violations {
        let kind: String = message
            .split(" / ")
            .next()
            .unwrap_or(message)
            .chars()
            .map(|c| if c.is_ascii_digit() { '#' } else { c })
            .collect();
        let entry = kinds.entry(kind).or_insert((0, message, text));
        entry.0 += 1;
    }
    kinds
        .values()
        .map(|(count, message, text)| format!("{count:5}x {message}\n        {text}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// An unmodelled key inserted into any object of a spec we read makes the parse
/// decline *at or beneath* the insertion, so no object's vocabulary is open.
/// Absolute (no oracle), and exhaustive over the templates' objects.
#[test]
fn an_inserted_key_declines_beneath_its_insertion() {
    for template in [minimal(), realistic()] {
        let tree = Node::from_value(&template);
        assert!(parse_dgspec(&tree.text()).is_ok(), "template must parse");
        for (path, _, is_object) in sites(&tree) {
            if !is_object {
                continue;
            }
            let mut doc = tree.clone();
            let Node::Object(obj) = doc.at_mut(&path) else {
                unreachable!()
            };
            obj.push(("zz".to_owned(), Node::Leaf(json!({"zzz": []}))));
            let text = doc.text();
            let decline = parse_dgspec(&text).expect_err("an inserted key must decline");

            let expected = json_path(&tree, &path);
            let mut prefix = expected.clone();
            prefix.push(PathSegment::Key("zz".to_owned()));
            assert!(
                decline.path.segments().starts_with(&prefix),
                "inserting at {expected:?} declined at {} instead: {decline}",
                decline.path
            );
        }
    }
}

/// The decline path of the node at `steps` in `tree`.
fn json_path(tree: &Node, steps: &[Step]) -> Vec<PathSegment> {
    let mut out = Vec::new();
    let mut node = tree;
    for step in steps {
        node = match (node, step) {
            (Node::Array(items), Step::Index(i)) => {
                out.push(PathSegment::Index(*i));
                &items[*i]
            }
            (Node::Object(members), Step::Member(i)) => {
                out.push(PathSegment::Key(members[*i].0.clone()));
                &members[*i].1
            }
            _ => unreachable!(),
        };
    }
    out
}

/// The warning-code table is exactly `NuGetLogCode`'s names (less `Undefined`)
/// in the NuGet the oracle runs: a name we accept that the reader does not know
/// is one it drops, and a name it knows that we lack is a needless decline.
#[test]
fn log_codes_are_nugets() {
    let mut oracle = Oracle::spawn();
    let response = oracle.request(&json!({"op": "nuGetLogCodes"}));
    let mut theirs: Vec<String> = response["names"]
        .as_array()
        .expect("names")
        .iter()
        .map(|n| n.as_str().expect("name").to_owned())
        .filter(|n| n != "Undefined")
        .collect();
    theirs.sort();
    let ours: Vec<String> = NuGetLogCode::all().map(|c| c.as_str().to_owned()).collect();
    assert_eq!(ours, theirs);
}

/// Every `*.nuget.dgspec.json` under `BORZOI_DGSPEC_SWEEP_ROOT` (the files
/// `dotnet restore` leaves in each project's `obj/`), held to
/// certain-implies-exact, with a census of what declined and where.
#[test]
#[ignore = "walks real restored projects; set BORZOI_DGSPEC_SWEEP_ROOT"]
fn real_dgspecs_sweep() {
    let root = PathBuf::from(
        std::env::var_os("BORZOI_DGSPEC_SWEEP_ROOT").expect("set BORZOI_DGSPEC_SWEEP_ROOT"),
    );
    let mut files = Vec::new();
    collect_dgspecs(&root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "no *.nuget.dgspec.json under {root:?}");

    let mut oracle = Oracle::spawn();
    let mut outcomes: BTreeMap<Outcome, usize> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut violations = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        match grade(&mut oracle, &text) {
            Ok((outcome, decline)) => {
                *outcomes.entry(outcome).or_default() += 1;
                if let Some(decline) = decline {
                    *reasons.entry(decline_shape(&decline)).or_default() += 1;
                }
            }
            Err(violation) => violations.push(format!("{}: {violation}", file.display())),
        }
    }
    eprintln!("{} dgspecs: {outcomes:?}", files.len());
    let mut by_count: Vec<_> = reasons.into_iter().collect();
    by_count.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (shape, count) in by_count.iter().take(40) {
        eprintln!("{count:7} {shape}");
    }
    assert!(
        violations.is_empty(),
        "{} dgspecs read differently from NuGet; first 5:\n{}",
        violations.len(),
        violations
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn collect_dgspecs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "node_modules" || n == ".git")
            {
                continue;
            }
            collect_dgspecs(&path, out);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".nuget.dgspec.json"))
            && path
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n == "obj")
        {
            out.push(path);
        }
    }
}

/// A decline with its name-keyed path segments (project names, package ids,
/// aliases) blanked, so the census groups by where in the schema it arose.
fn decline_shape(decline: &DgSpecDecline) -> String {
    let mut shape = String::from("$");
    let mut previous = "";
    for segment in decline.path.segments() {
        match segment {
            PathSegment::Key(key) => {
                if NAME_KEYED.contains(&previous) {
                    shape.push_str(".*");
                } else {
                    shape.push('.');
                    shape.push_str(key);
                }
                previous = key;
            }
            PathSegment::Index(_) => shape.push_str("[]"),
        }
    }
    let reason = match &decline.reason {
        DgSpecDeclineReason::UnrecognisedFlag(t)
        | DgSpecDeclineReason::UnrecognisedTarget(t)
        | DgSpecDeclineReason::UnrecognisedLogCode(t) => {
            format!("{:?}({t:?})", std::mem::discriminant(&decline.reason))
        }
        other => format!("{other:?}")
            .split(['(', ' ', '{'])
            .next()
            .unwrap_or("")
            .to_owned(),
    };
    format!("{reason} at {shape}")
}

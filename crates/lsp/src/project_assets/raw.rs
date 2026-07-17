use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Deserialised form of `obj/project.assets.json`.
///
/// Only the fields this crate consumes are modelled; everything else
/// (`sha512`, `files`, `runtime`, `resource`, `dependencies`,
/// `projectFileDependencyGroups`, `restore`, etc.) is dropped on read.
/// `BTreeMap` keys give us deterministic iteration order so resolver
/// output is reproducible.
///
/// `packageFolders` is deserialised into a `Vec<PathBuf>` rather than a
/// `BTreeMap` because order matters: NuGet writes the primary global
/// packages folder first, with fallback folders after, and consumers
/// must respect that order. A `BTreeMap` would sort the folders
/// lexicographically and root every package DLL under the wrong cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawAssets {
    pub version: u32,
    pub targets: BTreeMap<String, BTreeMap<String, RawTargetEntry>>,
    pub libraries: BTreeMap<String, RawLibrary>,
    #[serde(rename = "packageFolders", with = "package_folders_serde")]
    pub package_folders: Vec<PathBuf>,
    pub project: RawProject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawTargetEntry {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compile: Option<BTreeMap<String, serde_json::Value>>,
    /// For `type: "project"` only: the *producer's* TFM that NuGet
    /// selected for this consumer. Written in the long moniker form
    /// (e.g. `.NETStandard,Version=v2.0`); short-form conversion is
    /// applied in `enumerate` when populating `Reference::ProjectRef`.
    /// `None` on package and unknown library kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawLibrary {
    #[serde(rename = "type")]
    pub kind: String,
    /// For `type: "package"`: relative directory under one of the
    /// `packageFolders`, always lowercased.
    /// For `type: "project"`: relative path to the referenced `.fsproj`/
    /// `.csproj`. NuGet lowercases the directory portion of this path,
    /// which loses information on case-sensitive filesystems (Linux),
    /// so `msbuildProject` is preferred when present — see below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// For `type: "project"` only: case-preserving relative path to the
    /// referenced project file. NuGet writes this alongside `path` and
    /// it carries the on-disk casing as MSBuild saw it. Prefer this for
    /// project-reference resolution; `path` may differ only in case
    /// (e.g. `NuGet.Services.Github` vs `NuGet.Services.GitHub`) and
    /// canonicalise to a missing file on Linux.
    #[serde(
        rename = "msbuildProject",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub msbuild_project: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawProject {
    pub frameworks: BTreeMap<String, RawProjectFramework>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawProjectFramework {
    #[serde(rename = "frameworkReferences", default)]
    pub framework_references: BTreeMap<String, serde_json::Value>,
}

/// JSON-order-preserving serde for the `packageFolders` field.
///
/// Reads `{ "path1": {...}, "path2": {...} }` into a `Vec<PathBuf>`
/// preserving insertion order (serde_json visits object keys in
/// document order). Serialises back to the same shape with empty
/// objects as values, which is enough to round-trip.
mod package_folders_serde {
    use std::path::PathBuf;

    pub fn serialize<S>(folders: &[PathBuf], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(folders.len()))?;
        for f in folders {
            map.serialize_entry(f, &serde_json::Value::Object(Default::default()))?;
        }
        map.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<PathBuf>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{IgnoredAny, MapAccess, Visitor};
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Vec<PathBuf>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object mapping package-folder paths to metadata")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some((k, _v)) = map.next_entry::<PathBuf, IgnoredAny>()? {
                    out.push(k);
                }
                Ok(out)
            }
        }
        deserializer.deserialize_map(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn load_fixture(name: &str) -> RawAssets {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/project_assets")
            .join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
    }

    #[test]
    fn single_tfm_fixture_deserialises() {
        let assets = load_fixture("single_tfm.json");

        assert_eq!(assets.version, 3);

        let targets: Vec<&String> = assets.targets.keys().collect();
        assert_eq!(targets, vec![&"net10.0".to_string()]);

        let net10 = &assets.targets["net10.0"];
        assert!(net10.contains_key("FSharp.Compiler.Service/43.12.204"));
        assert!(net10.contains_key("FSharp.Core/10.1.204"));
        assert!(net10.contains_key("FSharp.SystemTextJson/1.4.36"));

        let fcs = &net10["FSharp.Compiler.Service/43.12.204"];
        assert_eq!(fcs.kind, "package");
        let compile = fcs.compile.as_ref().expect("FCS must list compile assets");
        assert!(compile.contains_key("lib/netstandard2.0/FSharp.Compiler.Service.dll"));
    }

    #[test]
    fn single_tfm_fixture_libraries_use_lowercase_path() {
        let assets = load_fixture("single_tfm.json");
        let fcs = &assets.libraries["FSharp.Compiler.Service/43.12.204"];
        assert_eq!(fcs.kind, "package");
        assert_eq!(
            fcs.path.as_deref(),
            Some("fsharp.compiler.service/43.12.204")
        );
    }

    #[test]
    fn single_tfm_fixture_lists_framework_reference() {
        let assets = load_fixture("single_tfm.json");
        let net10 = &assets.project.frameworks["net10.0"];
        assert!(
            net10
                .framework_references
                .contains_key("Microsoft.NETCore.App")
        );
    }
}

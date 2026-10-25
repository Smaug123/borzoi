//! Generative differential: the **SDK's own** `<Compile>` operations, against
//! the real SDK evaluated by real MSBuild (the oracle's `items` op).
//!
//! ## Why this exists
//!
//! `Microsoft.NET.Sdk.DefaultItems.props` carries the default glob
//! `<Compile Include="**/*$(DefaultLanguageSourceExtension)" Exclude="…"
//! Condition="'$(EnableDefaultCompileItems)' == 'true'" />`. An F# project
//! normally switches it off: `Microsoft.FSharp.NetSdk.props` defaults
//! `EnableDefaultCompileItems` to `false`. But that props file is imported only
//! when `$(FSharpPropsShim)` exists, and a project can also just set the
//! property to `true`. Either way the glob *runs*, and the files it adds sit in
//! front of the project's own `<Compile>` list.
//!
//! The evaluator used to tolerate every Compile operation in the SDK tree, on
//! the theory that SDK machinery never decides which hand-written sources
//! compile. That theory is false exactly when this glob runs: the corpus
//! differential found six real projects (`tests/EndToEndBuildTests/**` in the
//! F# repository, which point `FSharpPropsShim` at a build output that does not
//! exist) where we committed one `Compile` item and MSBuild had the glob's
//! files plus a duplicate.
//!
//! ## The asserted property
//!
//! Whenever our parse commits — the Compile capture is not marked uncertain —
//! our ordered `Compile` list must equal MSBuild's exactly. A decline makes no
//! claim. The generated axes are the ones that decide whether the SDK glob
//! runs (`EnableDefaultCompileItems`, `EnableDefaultItems`, a missing
//! `FSharpPropsShim`) crossed with the project's own items and both production
//! seams: no glob resolver, and the shipped [`glob_resolver::resolve`] the LSP
//! passes.
//!
//! Where the SDK glob is provably dead and the project's own items are
//! literal, both seams **must** commit: a per-case obligation, so a model that
//! declined everything cannot pass.
//!
//! The space is small enough to run exhaustively, so there is no seed.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use borzoi_msbuild::{
    GlobResolver, ItemKind, SdkResolver, glob_resolver, parse_fsproj_with_imports, resolve_sdk,
    workloads,
};
use common::Oracle;

/// Source files laid down beside every generated project. Every name is
/// lower-case ASCII, so the default glob's ordering is the same under every
/// plausible collation: this harness is about *whether* the SDK's items are
/// honoured, not how a glob sorts.
const FILES: &[&str] = &[
    "a.fs",
    "b.fs",
    "z.fs",
    "sub/c.fs",
    // The SDK's `DefaultItemExcludes` / `DefaultExcludesInProjectFolder`
    // remove these; a resolver that ignored the `Exclude` would list them.
    "bin/debug/d.fs",
    "obj/e.fs",
    ".hidden/f.fs",
];

/// `EnableDefaultCompileItems` as the project body writes it, if at all.
const ENABLE_COMPILE: &[Option<&str>] = &[None, Some("true"), Some("false")];

/// `EnableDefaultItems` as the project body writes it, if at all. It gates the
/// whole default-item `<ItemGroup>` the glob sits in.
const ENABLE_ITEMS: &[Option<&str>] = &[None, Some("false")];

/// The project's own Compile items.
const OWN_ITEMS: &[&str] = &[
    "",
    "    <Compile Include=\"a.fs\" />\n",
    "    <Compile Include=\"z.fs;a.fs\" />\n",
    "    <Compile Include=\"b.fs\" />\n    <Compile Remove=\"sub/c.fs\" />\n",
];

#[derive(Debug, Clone, Copy)]
struct Fixture {
    missing_shim: bool,
    enable_compile: Option<&'static str>,
    enable_items: Option<&'static str>,
    own_items: &'static str,
}

impl Fixture {
    fn all() -> Vec<Fixture> {
        let mut out = Vec::new();
        for missing_shim in [false, true] {
            for &enable_compile in ENABLE_COMPILE {
                for &enable_items in ENABLE_ITEMS {
                    for &own_items in OWN_ITEMS {
                        out.push(Fixture {
                            missing_shim,
                            enable_compile,
                            enable_items,
                            own_items,
                        });
                    }
                }
            }
        }
        out
    }

    fn project_xml(&self) -> String {
        let mut props = String::from("    <TargetFramework>net10.0</TargetFramework>\n");
        if let Some(v) = self.enable_compile {
            props.push_str(&format!(
                "    <EnableDefaultCompileItems>{v}</EnableDefaultCompileItems>\n"
            ));
        }
        if let Some(v) = self.enable_items {
            props.push_str(&format!(
                "    <EnableDefaultItems>{v}</EnableDefaultItems>\n"
            ));
        }
        format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n{props}  </PropertyGroup>\n  \
             <ItemGroup>\n{}  </ItemGroup>\n</Project>\n",
            self.own_items
        )
    }

    /// The `Directory.Build.props` that, like the F# repository's test
    /// projects, points `FSharpPropsShim` at a file that does not exist — so
    /// the F# props never default `EnableDefaultCompileItems` to `false`.
    fn directory_build_props(&self) -> Option<&'static str> {
        self.missing_shim.then_some(
            "<Project>\n  <PropertyGroup>\n    \
             <FSharpPropsShim>$(MSBuildThisFileDirectory)missing/Microsoft.FSharp.NetSdk.props</FSharpPropsShim>\n  \
             </PropertyGroup>\n</Project>\n",
        )
    }

    /// Whether the SDK's default glob runs, by the SDK's own rules:
    /// `EnableDefaultItems=false` switches off the group it sits in; otherwise
    /// an explicit `EnableDefaultCompileItems` wins, and an unset one defaults
    /// to F#'s `false` — unless the F# props were never imported, leaving the
    /// SDK's `true`.
    ///
    /// The harness checks this against MSBuild on every case, so the
    /// obligations derived from it cannot silently drift from the reference.
    fn sdk_glob_runs(&self) -> bool {
        if self.enable_items == Some("false") {
            return false;
        }
        match self.enable_compile {
            Some(v) => v == "true",
            None => self.missing_shim,
        }
    }

    /// The project's own `Compile` includes, in order — MSBuild's whole list
    /// when the SDK glob does not run.
    fn own_includes(&self) -> Vec<&'static str> {
        match self.own_items {
            "" => vec![],
            s if s.contains("\"z.fs;a.fs\"") => vec!["z.fs", "a.fs"],
            s if s.contains("\"a.fs\"") => vec!["a.fs"],
            _ => vec!["b.fs"],
        }
    }

    /// Cases where we must commit, on both seams: the SDK glob is cleanly
    /// dead, and the project's own items are all literal includes.
    fn must_commit(&self) -> bool {
        !self.sdk_glob_runs() && !self.own_items.contains("Remove")
    }
}

/// Which production seam a case ran through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seam {
    NoResolver,
    ShippedResolver,
}

#[derive(Debug, Default)]
struct Tally {
    committed: usize,
    declined: usize,
    /// Distinct decline causes and how often each fired.
    reasons: std::collections::BTreeMap<String, usize>,
    /// Committed cases in which the SDK glob ran and we reproduced it.
    committed_with_sdk_items: usize,
}

fn lay_down(root: &Path, fixture: &Fixture) -> (PathBuf, String) {
    for file in FILES {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "module M\n").unwrap();
    }
    if let Some(props) = fixture.directory_build_props() {
        std::fs::write(root.join("Directory.Build.props"), props).unwrap();
    }
    let xml = fixture.project_xml();
    let project = root.join("Demo.fsproj");
    std::fs::write(&project, &xml).unwrap();
    (project, xml)
}

/// Our ordered `Compile` list, or why we declined to commit one.
fn ours(project: &Path, xml: &str, seam: Seam) -> Result<Vec<PathBuf>, String> {
    let dotnet_root = common::dotnet_root_from_env();
    let (user_dotnet_root, overrides_present) = common::workload_env_from_process();
    let sdk = |name: &str| {
        resolve_sdk(
            &dotnet_root,
            None,
            name,
            None,
            None,
            &workloads::WorkloadEnvironment {
                user_dotnet_root: user_dotnet_root.as_deref(),
                overrides_present,
                // The fixture tempdir has no global.json above it.
                global_json_pins_workload_set: false,
            },
        )
    };
    let glob: &GlobResolver<'_> = &glob_resolver::resolve;
    let parsed = parse_fsproj_with_imports(
        xml,
        project,
        &HashMap::new(),
        &common::oracle_environment(),
        Some(&sdk as &SdkResolver<'_>),
        (seam == Seam::ShippedResolver).then_some(glob),
    )
    .expect("well-formed XML parses");
    if parsed.items_uncertain {
        return Err(decline_reason(&parsed));
    }
    Ok(parsed
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Compile)
        .map(|item| item.include.clone())
        .collect())
}

/// The first recorded cause of a decline, for the census: a reason-less decline
/// is indistinguishable from a regression that declines everything.
fn decline_reason(parsed: &borzoi_msbuild::ParsedProject) -> String {
    if let Some(cause) = parsed.compile_condition_uncertainties.first() {
        return format!("condition {:?}: {:?}", cause.condition, cause.reason);
    }
    if let Some(cause) = parsed.compile_item_uncertainties.first() {
        return format!("{:?}", cause.kind);
    }
    "items_uncertain with no recorded cause".to_string()
}

/// Lexical normalisation, so `dir/./a.fs` and `dir/a.fs` compare equal without
/// touching the filesystem (a path we commit need not exist).
fn lexical(path: &Path) -> String {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.to_string_lossy().replace('\\', "/")
}

#[test]
fn sdk_compile_operations_are_exact_or_declined() {
    let mut oracle = Oracle::spawn();
    let mut tallies: HashMap<&'static str, Tally> = HashMap::new();
    let mut failures = Vec::new();
    let mut glob_running_cases = 0usize;

    for fixture in Fixture::all() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (project, xml) = lay_down(&root, &fixture);
        let theirs: Vec<String> = oracle
            .items(&xml, &project, "Compile", &[])
            .expect("MSBuild evaluates these documents")
            .iter()
            .map(|p| lexical(Path::new(p)))
            .collect();
        // Pin the reference against the fixture's own model, so the
        // must-commit obligations below are calibrated against MSBuild rather
        // than against my reading of the SDK.
        if !fixture.sdk_glob_runs() {
            let expected: Vec<String> = fixture
                .own_includes()
                .iter()
                .map(|f| lexical(&root.join(f)))
                .collect();
            assert_eq!(
                theirs, expected,
                "fixture model is wrong for {fixture:?}: MSBuild disagrees about the SDK glob"
            );
        } else {
            glob_running_cases += 1;
        }

        for seam in [Seam::NoResolver, Seam::ShippedResolver] {
            let key = match seam {
                Seam::NoResolver => "no resolver",
                Seam::ShippedResolver => "shipped resolver",
            };
            let tally = tallies.entry(key).or_default();
            let ours = match ours(&project, &xml, seam) {
                Ok(ours) => ours,
                Err(reason) => {
                    if fixture.must_commit() {
                        failures.push(format!(
                            "{fixture:?} via {key}: declined a case it must commit ({reason})"
                        ));
                    }
                    tally.declined += 1;
                    *tally.reasons.entry(reason).or_default() += 1;
                    continue;
                }
            };
            let ours: Vec<String> = ours.iter().map(|p| lexical(p)).collect();
            if ours != theirs {
                failures.push(format!(
                    "{fixture:?} via {key}:\n  ours:   {ours:?}\n  theirs: {theirs:?}"
                ));
                continue;
            }
            tally.committed += 1;
            if fixture.sdk_glob_runs() {
                tally.committed_with_sdk_items += 1;
            }
        }
    }

    for (seam, tally) in &tallies {
        eprintln!("sdk default compile diff [{seam}]: {tally:#?}");
    }
    assert!(
        failures.is_empty(),
        "certain-implies-exact violated in {} case(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    // Non-vacuity of the generator: the cases this harness exists for — the
    // SDK glob running — must actually be generated. (Today both seams decline
    // every one of them: the glob's `Exclude` reads `$(OutputPath)`, which the
    // walk cannot pin, so the census above shows them as declines rather than
    // commits. Committing them is coverage work, not a soundness fix.)
    assert!(
        glob_running_cases > 0,
        "no generated case runs the SDK default glob"
    );
}

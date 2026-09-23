//! Calibration of the oracle's `defines` op against a **real build's compiler
//! arguments**: the op's answer must equal, in order, the `--define:` entries of
//! the `FscCommandLineArgs` of a restored, compiling build of the same project
//! under the same globals.
//!
//! ## Why the oracle needs an oracle
//!
//! Every other op of `msbuild-condition-oracle` stops at evaluation, and so is
//! exactly as trustworthy as MSBuild's evaluator. `defines` answers a question
//! about the *build*: it restores, runs a real `Build`'s targets through
//! `Compile` in-process with `SkipCompilerExecution`, and reads the `--define:`
//! tokens of the arguments the real `Fsc` task computed. Two things in that are claims rather than
//! facts: that such a build passes fsc the same symbols as a real one, and
//! that declining every other define-capable spelling leaves no symbol
//! unreported. This test checks both against a real build — restore, then the
//! whole `Build`, compiling (`ProvideCommandLineArgs` alone, so fsc runs). The op is always
//! asked first, so it meets each fixture unrestored, as written.
//!
//! ## Scope, pinned from both sides
//!
//! The fixtures span what decides fsc's symbols:
//!
//! - the SDK's define targets: the target framework family (`.NETCoreApp` at
//!   and below 5.0, `.NETStandard`), the configuration (including one needing
//!   the `-`/`.` → `_` rewrite), and the opt-out switches alone and combined;
//! - the `DefineConstants` string-to-items conversion: whitespace, empty
//!   fragments, repeated and case-distinct symbols (F# symbols are
//!   case-sensitive), an overwrite that drops the self-reference;
//! - `Fsc`'s other sources: `Nullable` (whose `enable` match is case-sensitive)
//!   and `OtherFlags`, each also supplied through an item reference, which only
//!   task-parameter binding expands;
//! - user targets appending before `CoreCompile` and before `BeforeBuild` (the
//!   latter outside `Compile`'s graph, inside `Build`'s);
//! - a project reference, which the op resolves without building;
//! - a package whose build props append a define, which only a restore brings
//!   in.
//!
//! Six fixtures must make the op **decline**: `OtherFlags` carrying `-d:X`,
//! `/d:X`, a response file, an embedded line break, or either define spelling
//! padded with whitespace (`Fsc` passes its arguments to fsc as a response
//! file, one per line, and fsc trims each line). fsc reads
//! each as a define, and the op does not parse fsc's option grammar, so an
//! answer would omit a symbol.
//!
//! One fixture must make the op **disagree**: a user target that appends only
//! when fsc is really run. That is the op's genuine boundary, and the fixture
//! proves the calibration can see it; if it ever agrees, the comparison has
//! stopped discriminating.
//!
//! `net472` and `net8.0` are absent because the devshell restores offline from a
//! pinned package set that carries neither targeting pack; `net6.0` stands in
//! for the pre-current `.NETCoreApp` shape.
//!
//! Established 2026-09-23 against SDK 10.0.301.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use borzoi_oracle_harness::BoundedCommand;
use common::{Oracle, scrub_oracle_env};

/// What the op must do on a fixture, relative to the real build's arguments.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    /// Exactly the `--define:` list, in order.
    Agree,
    /// Answer, but not with the `--define:` list: the fixture exercises build
    /// logic outside the op's scope.
    Diverge,
    /// Decline: the fixture passes a symbol in a spelling the op does not parse.
    Decline,
}

struct Fixture {
    name: &'static str,
    /// Project elements inside `<Project>`, before the `Compile` item group.
    body: String,
    /// Further files beside the project, as `(name, contents)`.
    files: &'static [(&'static str, &'static str)],
    /// Whether the fixture references [`DEFINE_PACKAGE`], packed into a local
    /// source beside it.
    package: bool,
    /// One global set per case.
    cases: Vec<Vec<(&'static str, &'static str)>>,
    expect: Expect,
}

/// A single-case `net10.0` fixture whose `<PropertyGroup>` also holds `props`,
/// followed by the further project elements `rest`.
fn net10(name: &'static str, props: &str, rest: &str, expect: Expect) -> Fixture {
    Fixture {
        name,
        body: format!(
            "<PropertyGroup><TargetFramework>net10.0</TargetFramework>{props}\
             </PropertyGroup>{rest}"
        ),
        files: &[],
        package: false,
        cases: vec![vec![]],
        expect,
    }
}

fn fixtures() -> Vec<Fixture> {
    use Expect::{Agree, Decline, Diverge};

    let mut multi_cases = Vec::new();
    for tfm in ["net10.0", "net6.0", "netstandard2.0"] {
        for configuration in ["Debug", "Release", "My-Config.1"] {
            multi_cases.push(vec![
                ("TargetFramework", tfm),
                ("Configuration", configuration),
            ]);
        }
    }
    let late_target = |condition: &str| {
        format!(
            "<Target Name=\"AddLate\" BeforeTargets=\"CoreCompile\"{condition}>\
             <PropertyGroup><DefineConstants>$(DefineConstants);LATE</DefineConstants>\
             </PropertyGroup></Target>"
        )
    };
    vec![
        Fixture {
            name: "multi-targeted, appending",
            body: "<PropertyGroup>\
                   <TargetFrameworks>net10.0;net6.0;netstandard2.0</TargetFrameworks>\
                   <DefineConstants>$(DefineConstants);MINE</DefineConstants>\
                   </PropertyGroup>"
                .to_string(),
            files: &[],
            package: false,
            cases: multi_cases,
            expect: Agree,
        },
        net10(
            "DisableImplicitFrameworkDefines",
            "<DisableImplicitFrameworkDefines>true</DisableImplicitFrameworkDefines>",
            "",
            Agree,
        ),
        net10(
            "DisableDiagnosticTracing",
            "<DisableDiagnosticTracing>true</DisableDiagnosticTracing>",
            "",
            Agree,
        ),
        net10(
            "DisableImplicitFrameworkDefines and DisableDiagnosticTracing",
            "<DisableImplicitFrameworkDefines>true</DisableImplicitFrameworkDefines>\
             <DisableDiagnosticTracing>true</DisableDiagnosticTracing>",
            "",
            Agree,
        ),
        net10(
            "DisableImplicitConfigurationDefines",
            "<DisableImplicitConfigurationDefines>true</DisableImplicitConfigurationDefines>",
            "",
            Agree,
        ),
        net10(
            "whitespace and empty fragments",
            "<DefineConstants>$(DefineConstants);  SPACED  ;;LAST </DefineConstants>",
            "",
            Agree,
        ),
        net10(
            "duplicate and case-distinct symbols",
            "<DefineConstants>$(DefineConstants);MINE;mine;MINE</DefineConstants>",
            "",
            Agree,
        ),
        net10(
            "overwrite without self-reference",
            "<DefineConstants>ONLY</DefineConstants>",
            "",
            Agree,
        ),
        net10("Nullable enable", "<Nullable>enable</Nullable>", "", Agree),
        net10("Nullable Enable", "<Nullable>Enable</Nullable>", "", Agree),
        net10(
            "Nullable through an item reference",
            "<Nullable>@(Mode)</Nullable>",
            "<ItemGroup><Mode Include=\"enable\" /></ItemGroup>",
            Agree,
        ),
        net10(
            "OtherFlags without a define",
            "<OtherFlags>--warnon:1182</OtherFlags>",
            "",
            Agree,
        ),
        net10(
            "OtherFlags with a canonical define",
            "<OtherFlags>--define:EXTRA</OtherFlags>",
            "",
            Agree,
        ),
        net10(
            "OtherFlags define through an item reference",
            "<OtherFlags>@(Flag)</OtherFlags>",
            "<ItemGroup><Flag Include=\"--define:EXTRA\" /></ItemGroup>",
            Agree,
        ),
        net10(
            "user target before CoreCompile",
            "",
            &late_target(""),
            Agree,
        ),
        // `BeforeBuild` precedes `CoreBuild`, so it is outside `Compile`'s
        // graph but inside `Build`'s.
        net10(
            "user target before BeforeBuild",
            "",
            "<Target Name=\"Early\" BeforeTargets=\"BeforeBuild\">\
             <PropertyGroup><DefineConstants>$(DefineConstants);EARLY</DefineConstants>\
             </PropertyGroup></Target>",
            Agree,
        ),
        Fixture {
            files: &[
                (
                    "B/B.fsproj",
                    "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup>\
                     <TargetFramework>net10.0</TargetFramework></PropertyGroup>\
                     <ItemGroup><Compile Include=\"B.fs\" /></ItemGroup></Project>",
                ),
                ("B/B.fs", "module B\n"),
            ],
            ..net10(
                "project reference",
                "",
                "<ItemGroup><ProjectReference Include=\"B/B.fsproj\" /></ItemGroup>",
                Agree,
            )
        },
        net10(
            "OtherFlags -d:",
            "<OtherFlags>-d:EXTRA</OtherFlags>",
            "",
            Decline,
        ),
        net10(
            "OtherFlags /d:",
            "<OtherFlags>/d:EXTRA</OtherFlags>",
            "",
            Decline,
        ),
        Fixture {
            files: &[("flags.rsp", "--define:EXTRA\n")],
            ..net10(
                "OtherFlags response file",
                "<OtherFlags>@$(MSBuildProjectDirectory)/flags.rsp</OtherFlags>",
                "",
                Decline,
            )
        },
        // fsc trims response-file lines, and `Fsc` passes its arguments
        // through one, so padding hides nothing from fsc — only from a
        // classifier that does not trim.
        net10(
            "OtherFlags padded -d:",
            "<OtherFlags>&quot; -d:EXTRA&quot;</OtherFlags>",
            "",
            Decline,
        ),
        // `Fsc` writes its arguments to a response file, one per line; a line
        // break inside one argument is two lines to fsc.
        net10(
            "OtherFlags with an embedded line break",
            "<OtherFlags>&quot;--warnon:1182&#10;--define:EXTRA&quot;</OtherFlags>",
            "",
            Decline,
        ),
        net10(
            "OtherFlags padded --define:",
            "<OtherFlags>&quot;--define:EXTRA &quot;</OtherFlags>",
            "",
            Decline,
        ),
        Fixture {
            package: true,
            ..net10(
                "package build props append a define",
                "",
                "<ItemGroup><PackageReference Include=\"BorzoiDefinePkg\" Version=\"1.0.0\" />\
                 </ItemGroup>",
                Agree,
            )
        },
        net10(
            "user target skipped when fsc is not run (outside the op's scope)",
            "",
            &late_target(" Condition=\"'$(SkipCompilerExecution)' != 'true'\""),
            Diverge,
        ),
    ]
}

/// The `FscCommandLineArgs` of a real build — restore, then the whole `Build`
/// with fsc actually running (`ProvideCommandLineArgs` alone asks the task to
/// report its arguments) — under `globals`.
fn real_fsc_arguments(project: &Path, globals: &[(String, String)]) -> Vec<String> {
    let out_file = project.with_file_name("fsc-args.json");
    let mut cmd = Command::new("dotnet");
    cmd.args([
        "msbuild",
        "-nologo",
        "-restore",
        "-t:Build",
        "-p:ProvideCommandLineArgs=true",
        "-getItem:FscCommandLineArgs",
    ]);
    for (name, value) in globals {
        cmd.arg(format!("-p:{name}={value}"));
    }
    cmd.arg(format!("-getResultOutputFile:{}", out_file.display()));
    cmd.arg(project);
    scrub_oracle_env(&mut cmd);
    BoundedCommand::new(cmd)
        .timeout(Duration::from_secs(600))
        .run_ok(format_args!(
            "real build of {} under {globals:?}",
            project.display()
        ));
    let json = std::fs::read_to_string(&out_file).expect("read -getResultOutputFile output");
    let value: serde_json::Value = serde_json::from_str(&json).expect("getItem output is JSON");
    value["Items"]["FscCommandLineArgs"]
        .as_array()
        .expect("FscCommandLineArgs items present")
        .iter()
        .map(|item| {
            item["Identity"]
                .as_str()
                .expect("item identity is a string")
                .to_string()
        })
        .collect()
}

/// A package whose `build/<id>.props` appends `FROM_PACKAGE` to
/// `DefineConstants` — a symbol that exists only once the project is restored,
/// because package build props are imported through the restore outputs.
const DEFINE_PACKAGE: &str = "BorzoiDefinePkg";

/// Pack [`DEFINE_PACKAGE`] into `dir/pkgs` and point a `nuget.config` in `dir`
/// at it (added to, not replacing, the ambient sources the rest of the restore
/// needs).
fn provide_define_package(dir: &Path) {
    let source = dir.join("pkgsrc");
    std::fs::create_dir_all(source.join("build")).expect("create package source dir");
    std::fs::write(
        source.join("build").join(format!("{DEFINE_PACKAGE}.props")),
        "<Project><PropertyGroup>\
         <DefineConstants>$(DefineConstants);FROM_PACKAGE</DefineConstants>\
         </PropertyGroup></Project>",
    )
    .expect("write package props");
    let package_project = source.join(format!("{DEFINE_PACKAGE}.csproj"));
    std::fs::write(
        &package_project,
        format!(
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup>\
             <TargetFramework>netstandard2.0</TargetFramework>\
             <PackageId>{DEFINE_PACKAGE}</PackageId><Version>1.0.0</Version>\
             <IncludeBuildOutput>false</IncludeBuildOutput>\
             <EnableDefaultCompileItems>false</EnableDefaultCompileItems>\
             <NoWarn>NU5128</NoWarn></PropertyGroup>\
             <ItemGroup><None Include=\"build/{DEFINE_PACKAGE}.props\" Pack=\"true\" \
             PackagePath=\"build/{DEFINE_PACKAGE}.props\" /></ItemGroup></Project>"
        ),
    )
    .expect("write package project");
    let mut cmd = Command::new("dotnet");
    cmd.args(["pack", "-nologo", "-o"]);
    cmd.arg(dir.join("pkgs"));
    cmd.arg(&package_project);
    scrub_oracle_env(&mut cmd);
    BoundedCommand::new(cmd)
        .timeout(Duration::from_secs(600))
        .run_ok("dotnet pack of the define-appending package");
    std::fs::write(
        dir.join("nuget.config"),
        "<configuration><packageSources><add key=\"local\" value=\"pkgs\" />\
         </packageSources></configuration>",
    )
    .expect("write nuget.config");
}

/// Whether the fsc argument `argument` carries the `EXTRA` symbol the decline
/// fixtures plant: directly, or in the response file an `@path` argument names.
fn hands_fsc_extra(argument: &str) -> bool {
    match argument.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path).is_ok_and(|text| text.contains("EXTRA")),
        None => argument.contains("EXTRA"),
    }
}

#[test]
fn defines_op_matches_a_real_builds_fsc_arguments() {
    let mut oracle = Oracle::spawn();
    let mut failures = Vec::new();
    let mut compared = 0;
    // Non-vacuity: at least one agreeing case must carry a symbol only a target
    // adds, and one a package adds, or the comparison never exercised the op's
    // reasons to exist.
    let mut saw_target_added_symbol = false;
    let mut saw_package_added_symbol = false;

    for fixture in fixtures() {
        let dir = tempfile::TempDir::new().expect("tempdir for fixture");
        let project = dir.path().join("P.fsproj");
        std::fs::write(
            &project,
            format!(
                "<Project Sdk=\"Microsoft.NET.Sdk\">{}\
                 <ItemGroup><Compile Include=\"A.fs\" /></ItemGroup></Project>",
                fixture.body
            ),
        )
        .expect("write fixture project");
        std::fs::write(dir.path().join("A.fs"), "module A\n").expect("write fixture source");
        for (name, contents) in fixture.files {
            let file = dir.path().join(name);
            std::fs::create_dir_all(file.parent().expect("fixture file has a parent"))
                .expect("create fixture file dir");
            std::fs::write(file, contents).expect("write fixture file");
        }
        if fixture.package {
            provide_define_package(dir.path());
        }

        for case in &fixture.cases {
            let globals: Vec<(String, String)> = case
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            // The op goes first, on the project as written — unrestored, for
            // the first case of each fixture — because restoring is part of
            // what it claims to do, exactly as a real build does.
            let op = oracle.defines(&project, None, &globals);
            let arguments = real_fsc_arguments(&project, &globals);
            let fsc: Vec<String> = arguments
                .iter()
                .filter_map(|a| a.strip_prefix("--define:").map(str::to_string))
                .collect();
            compared += 1;
            println!(
                "{} {globals:?}\n  fsc: {fsc:?}\n  op:  {op:?}",
                fixture.name
            );
            match (fixture.expect, op) {
                (Expect::Agree, Ok(op)) if op == fsc => {
                    saw_target_added_symbol |= op.iter().any(|d| d.ends_with("_OR_GREATER"));
                    saw_package_added_symbol |= op.iter().any(|d| d == "FROM_PACKAGE");
                }
                (Expect::Diverge, Ok(op)) if op != fsc => {}
                // A decline is only justified if fsc really was handed the
                // symbol some other way — in an argument, or in a response file
                // one names; otherwise the fixture tests nothing.
                (Expect::Decline, Err(_)) if arguments.iter().any(|a| hands_fsc_extra(a)) => {}
                (expect, op) => failures.push(format!(
                    "`{}` under {globals:?}: expected {expect:?}, got {op:?} against fsc \
                     {fsc:?} (all arguments: {arguments:?})",
                    fixture.name
                )),
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {compared} calibration cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let expected_cases: usize = fixtures().iter().map(|f| f.cases.len()).sum();
    assert_eq!(compared, expected_cases, "every calibration case must run");
    assert!(
        saw_target_added_symbol,
        "no agreeing case carried a target-added `_OR_GREATER` symbol"
    );
    assert!(
        saw_package_added_symbol,
        "no agreeing case carried the package-added `FROM_PACKAGE` symbol"
    );
}

//! Calibration of the oracle's `defines` op against the **real compiler
//! arguments**: the op's answer must equal, in order, the `--define:` entries of
//! the design-time `FscCommandLineArgs` for the same project under the same
//! globals.
//!
//! ## Why the oracle needs an oracle
//!
//! Every other op of `msbuild-condition-oracle` stops at evaluation, and so is
//! exactly as trustworthy as MSBuild's evaluator. `defines` runs *targets*, and
//! only some of them: `AddImplicitDefineConstants` and
//! `_DisableDiagnosticTracing`, the SDK's writers of `DefineConstants` between
//! evaluation and `CoreCompile`. That is a claim about which build logic
//! matters, and a claim is what needs checking. The check is the value fsc
//! receives: a design-time build (`DesignTimeBuild`, `ProvideCommandLineArgs`,
//! `SkipCompilerExecution` — the route IDE tooling uses to read fsc's command
//! line) runs the whole build up to and including `CoreCompile` without
//! compiling, and reports the arguments it would pass.
//!
//! That route needs a restored project, which is why it is the calibration and
//! not the op: a generative sweep cannot afford a restore per case.
//!
//! ## Scope, pinned from both sides
//!
//! The fixtures span the dimensions the SDK's define logic reads: the target
//! framework family (`.NETCoreApp` at and below 5.0, `.NETStandard`), the
//! configuration (including one needing the `-`/`.` → `_` rewrite), the three
//! opt-out switches, a user value with whitespace and empty fragments (`Fsc`
//! receives an item list, so the string-to-items conversion is under test too),
//! and a user write that discards the self-reference.
//!
//! The other sources `Fsc` draws symbols from are covered too: `Nullable`
//! (whose `enable` match is case-sensitive, so both spellings are fixtures) and
//! `OtherFlags`, which the op declines when it could carry a define and must
//! not decline when it cannot. Duplicate and case-distinct symbols are a
//! fixture because MSBuild de-duplicates target outputs case-insensitively
//! unless told not to, and F# symbols are case-sensitive.
//!
//! One fixture is expected to **disagree**: a user target that appends to
//! `DefineConstants` before `CoreCompile`. The op deliberately does not run
//! arbitrary user targets, and consumers of it must decline such projects. The
//! fixture proves the calibration can see that boundary; if it ever agrees, the
//! comparison has stopped discriminating.
//!
//! `net472` and `net8.0` are absent because the devshell restores offline from a
//! pinned package set that carries neither targeting pack; `net6.0` stands in
//! for the pre-current `.NETCoreApp` shape.
//!
//! Each case is a restore plus a design-time build, from the devshell's offline
//! package set; the whole calibration takes about 12 s, so it runs with the
//! crate's ordinary tests and an SDK pin change meets it automatically.
//!
//! Established 2026-09-23 against SDK 10.0.301.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use borzoi_oracle_harness::BoundedCommand;
use common::{Oracle, scrub_oracle_env};

/// What the op must do on a fixture, relative to the fsc arguments.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    /// Exactly the `--define:` list, in order.
    Agree,
    /// Not the `--define:` list: the fixture exercises build logic outside the
    /// op's scope.
    Diverge,
    /// The op declines, because the fixture passes symbols through a route it
    /// does not read (`OtherFlags`).
    Decline,
}

struct Fixture {
    name: &'static str,
    /// The `<PropertyGroup>` body and any further project elements.
    body: &'static str,
    /// One global set per case.
    cases: Vec<Vec<(&'static str, &'static str)>>,
    expect: Expect,
}

fn fixtures() -> Vec<Fixture> {
    let mut multi_cases = Vec::new();
    for tfm in ["net10.0", "net6.0", "netstandard2.0"] {
        for configuration in ["Debug", "Release", "My-Config.1"] {
            multi_cases.push(vec![
                ("TargetFramework", tfm),
                ("Configuration", configuration),
            ]);
        }
    }
    vec![
        Fixture {
            name: "multi-targeted, appending",
            body: "<PropertyGroup>\
                   <TargetFrameworks>net10.0;net6.0;netstandard2.0</TargetFrameworks>\
                   <DefineConstants>$(DefineConstants);MINE</DefineConstants>\
                   </PropertyGroup>",
            cases: multi_cases,
            expect: Expect::Agree,
        },
        Fixture {
            name: "DisableImplicitFrameworkDefines",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <DisableImplicitFrameworkDefines>true</DisableImplicitFrameworkDefines>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Agree,
        },
        Fixture {
            name: "DisableDiagnosticTracing",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <DisableDiagnosticTracing>true</DisableDiagnosticTracing>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Agree,
        },
        Fixture {
            // `AddImplicitDefineConstants` is skipped, and with it its
            // dependency on `_DisableDiagnosticTracing`; `CoreCompile`'s
            // `BeforeTargets` hook still runs the latter.
            name: "DisableImplicitFrameworkDefines and DisableDiagnosticTracing",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <DisableImplicitFrameworkDefines>true</DisableImplicitFrameworkDefines>\
                   <DisableDiagnosticTracing>true</DisableDiagnosticTracing>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Agree,
        },
        Fixture {
            name: "DisableImplicitConfigurationDefines",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <DisableImplicitConfigurationDefines>true</DisableImplicitConfigurationDefines>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Agree,
        },
        Fixture {
            name: "whitespace and empty fragments",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <DefineConstants>$(DefineConstants);  SPACED  ;;LAST </DefineConstants>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Agree,
        },
        Fixture {
            // Case-distinct and repeated: F# symbols are case-sensitive, and
            // `Fsc` passes every item.
            name: "duplicate and case-distinct symbols",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <DefineConstants>$(DefineConstants);MINE;mine;MINE</DefineConstants>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Agree,
        },
        Fixture {
            name: "Nullable enable",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <Nullable>enable</Nullable>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Agree,
        },
        Fixture {
            // `Fsc`'s setter matches `enable` case-sensitively.
            name: "Nullable Enable",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <Nullable>Enable</Nullable>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Agree,
        },
        Fixture {
            // `OtherFlags` without a define: the decline must not be blanket.
            name: "OtherFlags without a define",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <OtherFlags>--warnon:1182</OtherFlags>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Agree,
        },
        Fixture {
            name: "OtherFlags with a define",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <OtherFlags>--define:EXTRA</OtherFlags>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Decline,
        },
        Fixture {
            name: "overwrite without self-reference",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   <DefineConstants>ONLY</DefineConstants>\
                   </PropertyGroup>",
            cases: vec![vec![]],
            expect: Expect::Agree,
        },
        Fixture {
            name: "user target before CoreCompile (outside the op's scope)",
            body: "<PropertyGroup>\
                   <TargetFramework>net10.0</TargetFramework>\
                   </PropertyGroup>\
                   <Target Name=\"AddLate\" BeforeTargets=\"CoreCompile\">\
                   <PropertyGroup>\
                   <DefineConstants>$(DefineConstants);LATE</DefineConstants>\
                   </PropertyGroup>\
                   </Target>",
            cases: vec![vec![]],
            expect: Expect::Diverge,
        },
    ]
}

/// The `--define:` arguments fsc would be given: a restore plus a design-time
/// `Compile` (which runs `CoreCompile` with `SkipCompilerExecution`, so fsc's
/// command line is computed and reported without compiling), under `globals`.
fn fsc_defines(project: &Path, globals: &[(String, String)]) -> Vec<String> {
    let out_file = project.with_file_name("fsc-args.json");
    let mut cmd = Command::new("dotnet");
    cmd.args([
        "msbuild",
        "-nologo",
        "-restore",
        "-t:Compile",
        "-p:DesignTimeBuild=true",
        "-p:ProvideCommandLineArgs=true",
        "-p:SkipCompilerExecution=true",
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
            "design-time build of {} under {globals:?}",
            project.display()
        ));
    let json = std::fs::read_to_string(&out_file).expect("read -getResultOutputFile output");
    let value: serde_json::Value = serde_json::from_str(&json).expect("getItem output is JSON");
    value["Items"]["FscCommandLineArgs"]
        .as_array()
        .expect("FscCommandLineArgs items present")
        .iter()
        .filter_map(|item| {
            item["Identity"]
                .as_str()
                .expect("item identity is a string")
                .strip_prefix("--define:")
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn defines_op_matches_the_design_time_fsc_arguments() {
    let mut oracle = Oracle::spawn();
    let mut failures = Vec::new();
    let mut compared = 0;
    // Non-vacuity: at least one agreeing case must carry a symbol only a target
    // adds, or the comparison never exercised the op's reason to exist.
    let mut saw_target_added_symbol = false;

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

        for case in &fixture.cases {
            let globals: Vec<(String, String)> = case
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let fsc = fsc_defines(&project, &globals);
            let op = oracle.defines(&project, None, &globals);
            compared += 1;
            println!(
                "{} {globals:?}\n  fsc: {fsc:?}\n  op:  {op:?}",
                fixture.name
            );
            match (fixture.expect, op) {
                (Expect::Agree, Ok(op)) if op == fsc => {
                    saw_target_added_symbol |= op.iter().any(|d| d.ends_with("_OR_GREATER"));
                }
                (Expect::Diverge, Ok(op)) if op != fsc => {}
                (Expect::Decline, Err(_)) => {}
                (expect, op) => failures.push(format!(
                    "`{}` under {globals:?}: expected {expect:?}, got {op:?} against fsc {fsc:?}",
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
}

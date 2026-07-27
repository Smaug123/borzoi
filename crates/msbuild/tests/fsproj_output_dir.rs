//! The [`OutputDirVerdict`] the walker reports for a project's `OutDir`.
//!
//! `OutDir` is the property that names the directory the build actually writes
//! to. Probed against real MSBuild (dotnet 10, SDK project, `net10.0`) before
//! this was written, because the interaction with `OutputPath` and
//! `AppendTargetFrameworkToOutputPath` is not guessable:
//!
//! | declared | resulting `OutDir` |
//! |---|---|
//! | *(nothing)* | `bin\Debug/net10.0/` |
//! | `OutDir=artifacts/` | `artifacts/` |
//! | `OutputPath=artifacts/` | `artifacts/net10.0/` |
//! | `AppendTargetFrameworkToOutputPath=false` | `bin\Debug/` |
//! | `OutDir=artifacts/` + `AppendTfm=false` | `artifacts/` |
//!
//! So a declared `OutDir` is a **complete** answer — the TFM is not appended to
//! it, and `AppendTargetFrameworkToOutputPath` does not touch it.
//!
//! The cases that matter here are the ones where the *evaluated string alone*
//! would mislead a consumer, since that is the whole reason the verdict exists
//! rather than a `properties.get("OutDir")` read at the call site.
//!
//! These cases resolve **no SDK**, so every write they see is user-authored.
//! That is the axis they cannot test: under a resolved SDK the chain writes
//! `OutDir` itself, and only a user-authored write is a redirect. The
//! verdicts in that configuration are pinned by
//! `borzoi`'s `output_dir_under_real_sdk` group, which runs the LSP's own
//! evaluation path against the host's install.

use std::collections::HashMap;

use borzoi_msbuild::{OutputDirVerdict, parse_fsproj};
use tempfile::TempDir;

fn verdict(body: &str) -> OutputDirVerdict {
    verdict_with(body, &HashMap::new())
}

fn verdict_with(body: &str, extras: &HashMap<String, String>) -> OutputDirVerdict {
    let xml = format!("<Project><PropertyGroup>{body}</PropertyGroup></Project>");
    let tmp = TempDir::new().expect("temp dir");
    let path = tmp.path().join("P.fsproj");
    std::fs::write(&path, &xml).expect("write project");
    parse_fsproj(&xml, &path, extras, &HashMap::new())
        .expect("well-formed")
        .output_dir
}

/// No write at all is the common case, and it must stay cheap: the consumer
/// keeps its default-layout scan.
#[test]
fn no_write_is_the_default_layout() {
    assert_eq!(verdict(""), OutputDirVerdict::Default);
    assert_eq!(
        verdict("<TargetFramework>net10.0</TargetFramework>"),
        OutputDirVerdict::Default
    );
}

/// A project can move its output without ever naming `OutDir` — MSBuild
/// derives one from a user `OutputPath`, so `<OutputPath>elsewhere/</OutputPath>`
/// builds to `elsewhere/net10.0/`. Deriving that is a targets-file computation
/// this walker does not reproduce, so it declines rather than committing to
/// the base it can see.
///
/// Declining is all this buys: both non-committing arms send the consumer to
/// the same `bin` scan. What matters is that the *committing* arm never
/// carries `elsewhere/` without the framework segment.
#[test]
fn a_user_redirect_without_an_out_dir_declines() {
    assert_eq!(
        verdict("<OutputPath>elsewhere/</OutputPath>"),
        OutputDirVerdict::Unknown
    );
    assert_eq!(
        verdict("<AppendTargetFrameworkToOutputPath>false</AppendTargetFrameworkToOutputPath>"),
        OutputDirVerdict::Unknown
    );
    // A declared `OutDir` still wins outright: MSBuild appends neither the
    // framework nor `OutputPath` to it, so the redirect is irrelevant.
    assert_eq!(
        verdict("<OutDir>artifacts/</OutDir><OutputPath>elsewhere/</OutputPath>"),
        OutputDirVerdict::Declared {
            path: "artifacts/".to_owned(),
        }
    );
}

/// A plain declared directory is reported verbatim — not normalised, not
/// joined to anything. MSBuild writes there directly.
#[test]
fn a_plain_declared_directory_is_reported_verbatim() {
    assert_eq!(
        verdict("<OutDir>artifacts/</OutDir>"),
        OutputDirVerdict::Declared {
            path: "artifacts/".to_owned(),
        }
    );
    // A rooted value is equally verbatim; making it project-relative is the
    // consumer's job, and it must be able to tell the two apart.
    assert_eq!(
        verdict("<OutDir>/srv/out/</OutDir>"),
        OutputDirVerdict::Declared {
            path: "/srv/out/".to_owned(),
        }
    );
}

/// **The reason this is a verdict and not a property read.** A reference to a
/// property this walk never saw defined expands to *empty*, and — because the
/// environment model resolves an undefined name exactly — the result is not
/// marked untrusted either. `$(SolutionDir)artifacts/` therefore evaluates to
/// a clean-looking, wrong, project-relative `artifacts/`.
///
/// `$(SolutionDir)` is the realistic spelling: it is undefined outside a
/// solution build, which is exactly the situation an editor is in.
#[test]
fn a_value_leaning_on_an_undefined_property_declines() {
    assert_eq!(
        verdict("<OutDir>$(SolutionDir)artifacts/</OutDir>"),
        OutputDirVerdict::Unknown
    );

    // The contrast that proves the check is about *definedness*, not about
    // `$(...)` appearing: the same shape with the property defined commits,
    // and commits to the expanded value.
    assert_eq!(
        verdict("<Root>/srv</Root><OutDir>$(Root)/artifacts/</OutDir>"),
        OutputDirVerdict::Declared {
            path: "/srv/artifacts/".to_owned(),
        }
    );
}

/// A defined-but-empty property is genuinely empty, so it commits — the
/// walker's environment model is exact about it, unlike the undefined case
/// above. Pinned because the two produce *identical* evaluated strings, so
/// only the raw-body check can tell them apart.
#[test]
fn a_defined_but_empty_reference_still_commits() {
    assert_eq!(
        verdict("<Empty></Empty><OutDir>$(Empty)artifacts/</OutDir>"),
        OutputDirVerdict::Declared {
            path: "artifacts/".to_owned(),
        }
    );
}

/// **A configuration-dependent directory is never committed to.** This
/// evaluation runs under whichever configuration the environment supplied,
/// while the user may have built another, so the value names a directory that
/// need not be the one the build writes — and a stale assembly sitting in it
/// would be folded against current source.
///
/// Three ways in, because each is invisible to the checks that catch the
/// others: a direct reference in the body; a *gate*, whose value never
/// mentions the configuration at all; and a helper property, which neither
/// the gate check nor the body scan can see through.
#[test]
fn a_configuration_dependent_directory_declines() {
    let extras = HashMap::from([("Configuration".to_owned(), "Debug".to_owned())]);
    for body in [
        "<OutDir>artifacts/$(Configuration)/</OutDir>",
        "<OutDir Condition=\"'$(Configuration)' == 'Debug'\">fast/</OutDir>",
        "<Which>$(Configuration)</Which><OutDir>out/$(Which)/</OutDir>",
    ] {
        assert_eq!(
            verdict_with(body, &extras),
            OutputDirVerdict::Unknown,
            "{body} must not commit to a single configuration's directory"
        );
    }
}

/// Deciding on the evaluated value means a directory that merely *contains*
/// the configuration declines too. That is the deliberate direction to err:
/// the only tool for seeing through a helper property is the value itself, so
/// distinguishing `Debugging/` from a genuine dependence would cost the
/// helper case. A decline sends the consumer to the scan it would have run
/// anyway; a wrong commit sends it to a directory the build never writes.
#[test]
fn an_incidental_occurrence_declines_too() {
    let extras = HashMap::from([("Configuration".to_owned(), "Debug".to_owned())]);
    assert_eq!(
        verdict_with("<OutDir>Debugging/</OutDir>", &extras),
        OutputDirVerdict::Unknown
    );
}

/// A caller-supplied global is read-only for the whole walk, so no XML write
/// ever reaches the recorder for it — but MSBuild honours the global and
/// writes there. Reading that back as "nobody declared anything" would send a
/// consumer to scan `bin` for a project building somewhere else.
#[test]
fn a_global_out_dir_is_a_declaration() {
    let extras = HashMap::from([("OutDir".to_owned(), "from-global/".to_owned())]);
    assert_eq!(
        verdict_with("", &extras),
        OutputDirVerdict::Declared {
            path: "from-global/".to_owned(),
        }
    );
    // The project cannot rebind it, so its own write does not win.
    assert_eq!(
        verdict_with("<OutDir>ignored/</OutDir>", &extras),
        OutputDirVerdict::Declared {
            path: "from-global/".to_owned(),
        }
    );
}

/// A body this walker cannot model (CDATA, entity-encoded whitespace) is a
/// write whose result is unknown — not an absence of one. MSBuild accepts the
/// value and redirects, so reading it back as "never written" would claim the
/// standard layout for a project that left it.
#[test]
fn an_unmodellable_body_is_a_refusal_not_an_absence() {
    assert_eq!(
        verdict("<OutDir><![CDATA[artifacts/]]></OutDir>"),
        OutputDirVerdict::Unknown
    );
}

/// A later write wins, as everywhere else in the property pass — including
/// when the later one is the *worse* verdict.
#[test]
fn the_last_write_decides() {
    assert_eq!(
        verdict("<OutDir>first/</OutDir><OutDir>second/</OutDir>"),
        OutputDirVerdict::Declared {
            path: "second/".to_owned(),
        }
    );
    assert_eq!(
        verdict("<OutDir>fine/</OutDir><OutDir>$(SolutionDir)x/</OutDir>"),
        OutputDirVerdict::Unknown
    );
}

/// An empty write leaves the default layout in force — MSBuild's own gate is
/// `'$(OutDir)' == ''`. A whitespace-only *element body* is the same thing,
/// because the XML layer stores it as `""` on both sides (the property-table
/// differential pins that); it is not a whitespace-named directory.
#[test]
fn an_empty_write_falls_back_to_the_default_layout() {
    assert_eq!(verdict("<OutDir></OutDir>"), OutputDirVerdict::Default);
    assert_eq!(verdict("<OutDir>   </OutDir>"), OutputDirVerdict::Default);
}

/// Padding *does* survive when it comes from somewhere the XML layer never
/// touched — a global — and then the value names a directory whose real
/// spelling we would have to guess at, while MSBuild's `== ''` gate does not
/// fire to rescue it. The same trap [`ParsedProject::target_name`] declines on.
#[test]
fn a_whitespace_only_value_from_a_global_declines() {
    let extras = HashMap::from([("Pad".to_owned(), "  ".to_owned())]);
    assert_eq!(
        verdict_with("<OutDir>$(Pad)</OutDir>", &extras),
        OutputDirVerdict::Unknown
    );
}

/// The ordering hazard between the two checks above and the undefined-reference
/// check: `$(SolutionDir)` alone expands to nothing, so an implementation that
/// tested emptiness first would file a project that declares a custom layout
/// under "declares nothing". Both arms send the consumer to the same scan, so
/// nothing observable turns on it today — this pins the distinction while it
/// is still true, since the emptiness fallback is only sound for a value that
/// really is empty.
#[test]
fn an_undefined_reference_expanding_to_nothing_is_not_the_default_layout() {
    assert_eq!(
        verdict("<OutDir>$(SolutionDir)</OutDir>"),
        OutputDirVerdict::Unknown
    );
}

/// A write carrying an item reference is refused by the property pass, which
/// removes the binding. That must not read back as "never written": the real
/// build has *some* value there, and a later reader of this verdict should see
/// a refusal rather than an absence.
#[test]
fn a_refused_write_is_not_the_default_layout() {
    assert_eq!(
        verdict("<OutDir>@(Thing)/out/</OutDir>"),
        OutputDirVerdict::Unknown
    );
}

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

use borzoi_msbuild::{OutputDirVerdict, parse_fsproj, parse_fsproj_with_imports};
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

/// **The reason this is a verdict and not a property read.** Any `$(...)` in
/// the body declines, whatever it expands to, because the expansion is where
/// a build dimension gets in. `$(SolutionDir)artifacts/` is the motivating
/// shape — undefined outside a solution build, which is exactly the situation
/// an editor is in, so it expands to a clean-looking, wrong, project-relative
/// `artifacts/`.
///
/// The rule is deliberately blunt rather than a definedness check. A defined
/// reference is no safer: `$(Root)` may itself have been written under a
/// configuration gate, and `$(Platform)` varies with a dimension the editor
/// picks. Excluding expansion outright excludes all of them at once, instead
/// of enumerating the ways dependence can hide.
#[test]
fn any_expansion_in_the_body_declines() {
    for body in [
        "<OutDir>$(SolutionDir)artifacts/</OutDir>",
        "<Root>/srv</Root><OutDir>$(Root)/artifacts/</OutDir>",
        "<Empty></Empty><OutDir>$(Empty)artifacts/</OutDir>",
        "<OutDir>artifacts/$(Platform)/</OutDir>",
    ] {
        assert_eq!(
            verdict(body),
            OutputDirVerdict::Unknown,
            "{body} leans on an expansion and must not commit"
        );
    }
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

/// A literal that merely *contains* the configuration's name is still a
/// literal: nothing about it varies. Pinned because the previous approach —
/// inspecting the evaluated value for the configuration — declined this, and
/// the reason it no longer has to is that the body test already excludes
/// every way a value could have depended on one.
#[test]
fn a_literal_resembling_the_configuration_still_commits() {
    let extras = HashMap::from([("Configuration".to_owned(), "Debug".to_owned())]);
    assert_eq!(
        verdict_with("<OutDir>Debugging/</OutDir>", &extras),
        OutputDirVerdict::Declared {
            path: "Debugging/".to_owned(),
        }
    );
}

/// The declaration count is taken **syntactically**, from the document tree,
/// so an alternative the property walk never visits is still seen. Each of
/// these hides from a different part of that walk: a cleanly-skipped
/// `<PropertyGroup>` is not walked at all, and a `<Choose>` arm is control
/// flow whose unselected branch is likewise never entered. Both mean the
/// output directory is not one fixed place.
///
/// Found by review, one hiding place at a time, until the count stopped being
/// driven off the walk.
#[test]
fn an_alternative_the_walk_never_visits_still_declines() {
    let extras = HashMap::from([("Configuration".to_owned(), "Debug".to_owned())]);
    let cases = [
        // A cleanly-false group: its gate evaluates without complaint, so the
        // group is skipped outright.
        "<Project><PropertyGroup><OutDir>common/</OutDir></PropertyGroup>\
         <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\
         <OutDir>release/</OutDir></PropertyGroup></Project>",
        // `<Choose>`: the unselected arm is never entered.
        "<Project><Choose><When Condition=\"'$(Configuration)' == 'Debug'\">\
         <PropertyGroup><OutDir>debug/</OutDir></PropertyGroup></When>\
         <Otherwise><PropertyGroup><OutDir>ship/</OutDir></PropertyGroup></Otherwise>\
         </Choose></Project>",
    ];
    for xml in cases {
        let tmp = TempDir::new().expect("temp dir");
        let path = tmp.path().join("P.fsproj");
        std::fs::write(&path, xml).expect("write project");
        let verdict = parse_fsproj(xml, &path, &extras, &HashMap::new())
            .expect("well-formed")
            .output_dir;
        assert_eq!(
            verdict,
            OutputDirVerdict::Unknown,
            "an unvisited alternative must still decline: {xml}"
        );
    }
}

/// A single arm inside `<Choose>` is conditional even with no `Condition` of
/// its own — which arm runs *is* the question, so the control flow alone
/// disqualifies it.
#[test]
fn a_sole_choose_arm_is_still_conditional() {
    let xml = "<Project><Choose><When Condition=\"'$(Configuration)' == 'Debug'\">\
               <PropertyGroup><OutDir>debug/</OutDir></PropertyGroup></When></Choose></Project>";
    let tmp = TempDir::new().expect("temp dir");
    let path = tmp.path().join("P.fsproj");
    std::fs::write(&path, xml).expect("write project");
    let extras = HashMap::from([("Configuration".to_owned(), "Debug".to_owned())]);
    assert_eq!(
        parse_fsproj(xml, &path, &extras, &HashMap::new())
            .expect("well-formed")
            .output_dir,
        OutputDirVerdict::Unknown
    );
}

/// A document the project pulls in but this walk never scanned could hold the
/// `<OutDir>` the real build takes, so the counts say nothing and the verdict
/// declines. `parse_fsproj` follows no imports at all, which makes it the
/// sharpest form of the case: the local declaration looks sole and
/// unconditioned, and is not.
#[test]
fn an_unscanned_import_declines() {
    assert_eq!(
        verdict(
            "<OutDir>local/</OutDir></PropertyGroup><Import Project=\"other.props\" /><PropertyGroup>"
        ),
        OutputDirVerdict::Unknown
    );
}

/// A property-expanded `Project` selects *which file* arrives, so what that
/// file declares is as conditional as anything behind a `Condition` — there
/// is no `Condition` attribute here to notice.
#[test]
fn a_property_selected_import_makes_its_declarations_conditional() {
    let tmp = TempDir::new().expect("temp dir");
    std::fs::write(
        tmp.path().join("Debug.props"),
        "<Project><PropertyGroup><OutDir>debug/</OutDir></PropertyGroup></Project>",
    )
    .expect("write imported props");
    let xml = "<Project><PropertyGroup><Configuration>Debug</Configuration></PropertyGroup>\
               <Import Project=\"$(Configuration).props\" /></Project>";
    let path = tmp.path().join("P.fsproj");
    std::fs::write(&path, xml).expect("write project");
    let parsed =
        parse_fsproj_with_imports(xml, &path, &HashMap::new(), &HashMap::new(), None, None)
            .expect("well-formed");
    assert_eq!(parsed.output_dir, OutputDirVerdict::Unknown);
}

/// `TreatAsLocalProperty` hands a global back to the project, so the global
/// no longer pins the answer and the project's own writes decide — which
/// means the element counts apply after all.
#[test]
fn a_global_taken_back_locally_is_not_pinned() {
    let extras = HashMap::from([("OutDir".to_owned(), "from-global/".to_owned())]);
    let xml = "<Project TreatAsLocalProperty=\"OutDir\">\
               <PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\
               <OutDir>local/</OutDir></PropertyGroup></Project>";
    let tmp = TempDir::new().expect("temp dir");
    let path = tmp.path().join("P.fsproj");
    std::fs::write(&path, xml).expect("write project");
    assert_eq!(
        parse_fsproj(xml, &path, &extras, &HashMap::new())
            .expect("well-formed")
            .output_dir,
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

/// More than one `<OutDir>` element anywhere in the walk declines, even when
/// the winner looks unconditional. Which one wins is a property of the build
/// this evaluation happens to model: a sibling gated on another configuration
/// is skipped here and taken there, so the directory is not one fixed place.
/// Counting elements rather than surviving writes is what sees the skipped
/// one at all.
#[test]
fn more_than_one_out_dir_element_declines() {
    assert_eq!(
        verdict("<OutDir>first/</OutDir><OutDir>second/</OutDir>"),
        OutputDirVerdict::Unknown
    );
    assert_eq!(
        verdict(
            "<OutDir>common/</OutDir>\
             <OutDir Condition=\"'$(Configuration)' == 'Release'\">release/</OutDir>"
        ),
        OutputDirVerdict::Unknown,
        "the Release write is skipped under Debug, but it still exists"
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

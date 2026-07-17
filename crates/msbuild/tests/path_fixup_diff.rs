//! Differential: our [`path_fixup_worlds`] brackets MSBuild's unix path fixup.
//!
//! Stage P0 of `docs/msbuild-unix-path-fixup-plan.md`. MSBuild's
//! `MaybeAdjustFilePath` rewrites a backslash-bearing property value `\`→`/` iff
//! its first segment exists as a directory relative to the process's working
//! directory — cwd-dependent, so the value it commits is one of exactly two.
//!
//! This test proves the bracketing: for a value the fixup is eligible on,
//! `path_fixup_worlds` returns the rewrite, and **whatever MSBuild commits — run
//! from a directory where the first segment exists, and from one where it does
//! not — is one of `{value, rewrite}`**. For an ineligible value, MSBuild
//! commits the value verbatim from either directory.
//!
//! Unlike the resident-oracle differentials, this one must spawn `dotnet msbuild`
//! itself: the fixup's base directory is the *process cwd*, which a long-lived
//! oracle fixes once. So it is a small hand-corner set, each run twice.

mod common;

use std::path::Path;
use std::time::Duration;

use borzoi_msbuild::test_support::{PropertyMap, path_fixup_worlds, substitute};
use borzoi_oracle_harness::BoundedCommand;

const MSBUILD_TIMEOUT: Duration = Duration::from_secs(120);

/// Evaluate `<P>{body}</P>` with `dotnet msbuild` run **from** `cwd`, and return
/// P's committed value. The project lives in `cwd`, so the property-value fixup's
/// base directory (the process cwd) is `cwd`.
fn eval_property_in(cwd: &Path, body: &str) -> String {
    let proj = cwd.join("Fixup.proj");
    std::fs::write(
        &proj,
        format!("<Project><PropertyGroup><P>{body}</P></PropertyGroup></Project>\n"),
    )
    .expect("write project");

    let mut cmd = std::process::Command::new("dotnet");
    cmd.current_dir(cwd);
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
    cmd.args(["msbuild", "Fixup.proj", "-getProperty:P"]);
    let out = BoundedCommand::new(cmd)
        .timeout(MSBUILD_TIMEOUT)
        .run_ok(format_args!("dotnet msbuild in {}", cwd.display()));
    // `-getProperty` prints the bare value plus a trailing newline.
    String::from_utf8(out.stdout)
        .expect("utf8")
        .trim_end_matches(['\r', '\n'])
        .to_string()
}

/// The first `\`/`/`-delimited segment of `value` (after `\`→`/`), the directory
/// MSBuild's `LooksLikeUnixFilePath` probes. `None` if there isn't one.
fn first_segment(value: &str) -> Option<String> {
    let converted = value.replace('\\', "/");
    let seg = converted.split('/').next().unwrap_or("");
    (!seg.is_empty()).then(|| seg.to_string())
}

/// Run one case: assert MSBuild's committed value from a first-segment-present
/// cwd and a first-segment-absent cwd both lie in our bracket `{value, worlds}`.
#[track_caller]
fn check(body: &str) {
    let worlds = path_fixup_worlds(body);
    let bracket: Vec<&str> = match &worlds {
        Some(rewrite) => vec![body, rewrite],
        None => vec![body],
    };

    // A directory where the value's first segment exists as a subdirectory.
    let hit = tempfile::TempDir::new().unwrap();
    if let Some(seg) = first_segment(body) {
        // Skip a leading-empty segment (absolute `/…`, probed cwd-independently).
        if !seg.is_empty() {
            std::fs::create_dir_all(hit.path().join(&seg)).unwrap();
        }
    }
    // A directory where it does not.
    let miss = tempfile::TempDir::new().unwrap();

    for cwd in [hit.path(), miss.path()] {
        let theirs = eval_property_in(cwd, body);
        assert!(
            bracket.contains(&theirs.as_str()),
            "MSBuild committed {theirs:?} for body {body:?}, outside our bracket \
             {bracket:?} (worlds = {worlds:?})"
        );
    }
    // Not required for correctness, but a healthy harness should actually see the
    // fixup fire on an eligible value from the hit directory — otherwise the
    // bracketing is never exercised on its interesting side.
    if let Some(rewrite) = &worlds {
        let hit_value = eval_property_in(hit.path(), body);
        assert_eq!(
            &hit_value, rewrite,
            "the fixup should have fired for {body:?} in a directory containing its \
             first segment"
        );
    }
}

#[test]
fn worlds_brackets_msbuild_from_both_cwds() {
    // Eligible: a relative backslash path — the two worlds are raw and rewrite,
    // and MSBuild picks by whether the first segment exists.
    check("obj\\Debug\\");
    check("sub\\deep\\file.fs");
    check("a\\\\b\\c"); // collapsing slashes
    // Ineligible: no ambiguity, MSBuild commits the value from either cwd.
    // (`worlds` runs on the *post-expansion* value, so a `$(…)`-prefixed literal
    // — where MSBuild would still leave the reference — is covered by the unit
    // tests; here every body is already a plain value.)
    check("obj/Debug/");
    check("plain");
}

/// The path-fixup keystone (`docs/msbuild-unix-path-fixup-plan.md` P3):
/// whenever *our* evaluator commits a `[System.IO.Path]::Combine` /
/// `IsPathRooted` expression that carries a backslash, the value MSBuild
/// commits — run from a directory that contains the first segment **and** one
/// that does not — must be byte-identical. This is the two-cwd,
/// `dotnet msbuild`-authoritative form of the `property_expr_diff` corners: it
/// pins that these commits are genuinely cwd-independent (unlike a bare literal
/// value, whose fixup is cwd-probed and which we therefore never commit).
#[track_caller]
fn check_expr_cwd_independent(body: &str) {
    let (ours, issues) = substitute(body, &PropertyMap::new());
    if !issues.is_empty() {
        // We declined — no claim, nothing to bracket. (Certain-implies-exact.)
        return;
    }
    // `a` exists in one cwd, not the other — the only lever the fixup's probe
    // has — so agreement across both proves the commit does not depend on it.
    let with_a = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(with_a.path().join("a")).unwrap();
    let without_a = tempfile::TempDir::new().unwrap();
    for cwd in [with_a.path(), without_a.path()] {
        let theirs = eval_property_in(cwd, body);
        assert_eq!(
            theirs,
            ours,
            "we committed {ours:?} for {body:?}, but MSBuild (run from {}) committed {theirs:?}",
            cwd.display()
        );
    }
}

#[test]
#[cfg(not(windows))]
fn path_functions_commit_backslash_cwd_independently() {
    // `Combine` converts a *live* backslash unconditionally (result never
    // carries one), so these commit the slash form from either cwd.
    check_expr_cwd_independent("$([System.IO.Path]::Combine('a\\b','c'))");
    check_expr_cwd_independent("$([System.IO.Path]::Combine('/repo/proj','obj\\'))");
    check_expr_cwd_independent("$([System.IO.Path]::Combine('nope','x\\y'))");
    // An *escaped* backslash survives the fixup, so `combine_path` would diverge
    // — we decline it (the helper returns early on our issues).
    check_expr_cwd_independent("$([System.IO.Path]::Combine('a%5cb','c'))");
    // `IsPathRooted` commits every non-leading backslash (rootedness unchanged),
    // declines a leading one (live-vs-escaped split).
    check_expr_cwd_independent("$([System.IO.Path]::IsPathRooted('obj\\'))");
    check_expr_cwd_independent("$([System.IO.Path]::IsPathRooted('a\\b'))");
    check_expr_cwd_independent("$([System.IO.Path]::IsPathRooted('C:\\a'))");
    check_expr_cwd_independent("$([System.IO.Path]::IsPathRooted('\\a'))");
}

/// The one shape we must **not** commit: a `$(…)`-spliced value with a backslash
/// adjacent to another separator. MSBuild's splice fixup collapses the run
/// gated on the first segment existing, so the result is genuinely cwd-dependent
/// — proven here — and `combine_path` (no collapse) would commit a single wrong
/// value. So our evaluator declines it; this test pins both that MSBuild really
/// diverges by cwd and that we make no claim.
#[test]
#[cfg(not(windows))]
fn combine_declines_a_cwd_dependent_spliced_separator_run() {
    // Write `<P>` into the project so the splice happens there, then read the
    // Combine result from a given cwd.
    fn eval_combine_of_p(cwd: &Path, pval: &str) -> String {
        let proj = cwd.join("Run.proj");
        std::fs::write(
            &proj,
            format!(
                "<Project><PropertyGroup><P>{pval}</P>\
                 <C>$([System.IO.Path]::Combine('$(P)','c'))</C></PropertyGroup></Project>\n"
            ),
        )
        .unwrap();
        let mut cmd = std::process::Command::new("dotnet");
        cmd.current_dir(cwd);
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
        cmd.args(["msbuild", "Run.proj", "-getProperty:C"]);
        let out = BoundedCommand::new(cmd)
            .timeout(MSBUILD_TIMEOUT)
            .run_ok(format_args!("dotnet msbuild in {}", cwd.display()));
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim_end_matches(['\r', '\n'])
            .to_string()
    }

    // Two shapes: a run *adjacent* to the backslash (`a\/b`) and one *elsewhere*
    // in the value (`a//b\c`) — MSBuild's fixup collapses every run, so both are
    // cwd-dependent and both must decline.
    for pval in ["a\\/b", "a//b\\c"] {
        let body = "$([System.IO.Path]::Combine('$(P)','c'))";
        let mut props = PropertyMap::new();
        props.insert("P", pval);
        let (_ours, issues) = substitute(body, &props);
        assert!(
            !issues.is_empty(),
            "a cwd-dependent spliced separator run ({pval:?}) must decline, but we committed"
        );

        let with_a = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(with_a.path().join("a")).unwrap();
        let without_a = tempfile::TempDir::new().unwrap();
        assert_ne!(
            eval_combine_of_p(with_a.path(), pval),
            eval_combine_of_p(without_a.path(), pval),
            "expected the spliced separator run {pval:?} to be cwd-dependent in MSBuild"
        );
    }
}

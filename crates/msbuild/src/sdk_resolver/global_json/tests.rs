use std::cell::Cell;
use std::fs;

use proptest::prelude::*;
use proptest::test_runner::{Config as PtConfig, TestRunner};
use tempfile::TempDir;

use super::super::SdkVersion;
use super::super::version_spec::RollForward;
use super::*;

// ============================================================
// Unit tests — happy paths
// ============================================================

#[test]
fn empty_object_yields_no_settings() {
    let parsed = parse_global_json("{}").expect("empty object is valid JSON");
    assert_eq!(parsed.sdk, None);
    assert!(parsed.msbuild_sdks.is_empty());
}

#[test]
fn missing_sdk_key_yields_no_sdk_block_but_keeps_msbuild_sdks() {
    // Sibling keys like `msbuild-sdks` and `tools` are independent of
    // the `sdk` block: a file with only `msbuild-sdks` produces no
    // `sdk` settings but the SDK pin map still surfaces.
    let parsed = parse_global_json(r#"{"msbuild-sdks": {"Some.Sdk": "1.0.0"}}"#).unwrap();
    assert_eq!(parsed.sdk, None);
    assert_eq!(parsed.msbuild_sdks.len(), 1);
    assert_eq!(
        parsed.msbuild_sdks.get("Some.Sdk"),
        Some(&SdkVersion::parse("1.0.0").unwrap()),
    );
}

#[test]
fn empty_sdk_block_yields_settings_with_no_fields() {
    let parsed = parse_global_json(r#"{"sdk": {}}"#).unwrap().sdk.unwrap();
    assert_eq!(parsed, GlobalJsonSettings::default());
}

#[test]
fn parses_version_only() {
    let parsed = parse_global_json(r#"{"sdk": {"version": "9.0.100"}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.version, Some(SdkVersion::parse("9.0.100").unwrap()));
    assert_eq!(parsed.roll_forward, None);
    assert_eq!(parsed.allow_prerelease, None);
}

#[test]
fn parses_all_three_fields() {
    let text = r#"
        {
            "sdk": {
                "version": "8.0.401",
                "rollForward": "latestMinor",
                "allowPrerelease": false
            }
        }
    "#;
    let parsed = parse_global_json(text).unwrap().sdk.unwrap();
    assert_eq!(parsed.version, Some(SdkVersion::parse("8.0.401").unwrap()));
    assert_eq!(parsed.roll_forward, Some(RollForward::LatestMinor));
    assert_eq!(parsed.allow_prerelease, Some(false));
}

#[test]
fn roll_forward_is_case_insensitive() {
    // The .NET host treats `rollForward` values as case-insensitive
    // (lowercase, camelCase, and uppercase all accepted). Document
    // that explicitly.
    for variant in [
        ("disable", RollForward::Disable),
        ("Disable", RollForward::Disable),
        ("DISABLE", RollForward::Disable),
        ("patch", RollForward::Patch),
        ("Patch", RollForward::Patch),
        ("feature", RollForward::Feature),
        ("Feature", RollForward::Feature),
        ("minor", RollForward::Minor),
        ("major", RollForward::Major),
        ("latestPatch", RollForward::LatestPatch),
        ("latestpatch", RollForward::LatestPatch),
        ("latestFeature", RollForward::LatestFeature),
        ("latestMinor", RollForward::LatestMinor),
        ("latestMajor", RollForward::LatestMajor),
        ("LATESTMAJOR", RollForward::LatestMajor),
    ] {
        let text = format!(
            r#"{{"sdk": {{"version": "1.0.100", "rollForward": "{}"}}}}"#,
            variant.0
        );
        let parsed = parse_global_json(&text)
            .unwrap_or_else(|e| panic!("rejected {:?}: {e}", variant.0))
            .sdk
            .unwrap();
        assert_eq!(parsed.roll_forward, Some(variant.1), "{:?}", variant.0);
    }
}

#[test]
fn allow_prerelease_true_and_false() {
    let parsed = parse_global_json(r#"{"sdk": {"allowPrerelease": true}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.allow_prerelease, Some(true));
    let parsed = parse_global_json(r#"{"sdk": {"allowPrerelease": false}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.allow_prerelease, Some(false));
}

#[test]
fn null_optional_fields_treated_as_absent() {
    // .NET tolerates `"version": null` etc. — serializers that emit
    // nullable optional fields shouldn't poison the rest of the file.
    let text = r#"
        {
            "sdk": {
                "version": null,
                "rollForward": null,
                "allowPrerelease": null
            }
        }
    "#;
    let parsed = parse_global_json(text).unwrap().sdk.unwrap();
    assert_eq!(parsed, GlobalJsonSettings::default());
}

#[test]
fn null_version_with_other_fields_set_keeps_them() {
    // Only the null field is treated as absent; siblings still apply.
    let text = r#"{"sdk": {"version": null, "allowPrerelease": false}}"#;
    let parsed = parse_global_json(text).unwrap().sdk.unwrap();
    assert_eq!(parsed.version, None);
    assert_eq!(parsed.allow_prerelease, Some(false));
}

// ============================================================
// Unit tests — JSONC comments and BOM
// ============================================================

#[test]
fn ignores_line_comments() {
    let text = r#"
        // top-level comment
        {
            // inside object
            "sdk": {
                "version": "9.0.100" // inline after value
            }
        }
    "#;
    let parsed = parse_global_json(text).unwrap().sdk.unwrap();
    assert_eq!(parsed.version, Some(SdkVersion::parse("9.0.100").unwrap()));
}

#[test]
fn ignores_block_comments() {
    let text = r#"
        /* leading block */ {
            "sdk": /* between key and value */ {
                "version": /* nested */ "9.0.100"
            }
        }
    "#;
    let parsed = parse_global_json(text).unwrap().sdk.unwrap();
    assert_eq!(parsed.version, Some(SdkVersion::parse("9.0.100").unwrap()));
}

#[test]
fn comments_inside_string_literals_are_preserved() {
    // The stripper must NOT chew up `//` or `/*` that appear inside a
    // JSON string. A version with a `//`-bearing prerelease would be
    // invalid by `SdkVersion::parse`, but we can test the structural
    // behaviour using a string value where the slashes are legal.
    let text = r#"{"sdk": {}, "comment": "do not strip // me"}"#;
    let parsed = parse_global_json(text).unwrap();
    // The parser ignores `comment`, but the test is that we didn't
    // accidentally turn the inner `//` into a comment and corrupt the
    // surrounding structure.
    assert_eq!(parsed.sdk, Some(GlobalJsonSettings::default()));
    assert!(parsed.msbuild_sdks.is_empty());
}

#[test]
fn tolerates_utf8_bom() {
    let mut text = String::from("\u{FEFF}");
    text.push_str(r#"{"sdk": {"version": "9.0.100"}}"#);
    let parsed = parse_global_json(&text).unwrap().sdk.unwrap();
    assert_eq!(parsed.version, Some(SdkVersion::parse("9.0.100").unwrap()));
}

// ============================================================
// Unit tests — error paths
// ============================================================

#[test]
fn rejects_top_level_array() {
    let err = parse_global_json("[]").unwrap_err();
    assert!(matches!(
        err,
        GlobalJsonError::InvalidType {
            field: "global.json",
            expected: "object"
        }
    ));
}

#[test]
fn rejects_sdk_not_object() {
    let err = parse_global_json(r#"{"sdk": "9.0.100"}"#).unwrap_err();
    assert!(matches!(
        err,
        GlobalJsonError::InvalidType {
            field: "sdk",
            expected: "object"
        }
    ));
}

#[test]
fn rejects_sdk_version_not_string() {
    let err = parse_global_json(r#"{"sdk": {"version": 8}}"#).unwrap_err();
    assert!(matches!(
        err,
        GlobalJsonError::InvalidType {
            field: "sdk.version",
            expected: "string"
        }
    ));
}

#[test]
fn rejects_allow_prerelease_not_bool() {
    let err = parse_global_json(r#"{"sdk": {"allowPrerelease": "true"}}"#).unwrap_err();
    assert!(matches!(
        err,
        GlobalJsonError::InvalidType {
            field: "sdk.allowPrerelease",
            expected: "boolean"
        }
    ));
}

#[test]
fn rejects_invalid_version_string() {
    let err = parse_global_json(r#"{"sdk": {"version": "not-a-version"}}"#).unwrap_err();
    assert!(matches!(err, GlobalJsonError::InvalidVersion(_)));
}

#[test]
fn rejects_sdk_version_with_invalid_feature_band() {
    // .NET SDK feature bands start at x.y.100. These three shapes are
    // syntactically parseable but can't refer to any real SDK, so the
    // host rejects them at global.json parse time.
    for bad in ["8.0", "8.0.0", "8.0.99"] {
        let text = format!(r#"{{"sdk": {{"version": "{bad}"}}}}"#);
        let err = parse_global_json(&text).unwrap_err();
        assert!(
            matches!(&err, GlobalJsonError::InvalidVersion(s) if s == bad),
            "expected InvalidVersion({bad:?}), got {err:?}"
        );
    }
}

#[test]
fn rejects_sdk_version_with_extra_numeric_components() {
    // .NET's global.json consumer requires strictly three numeric
    // components. Four-component versions (with or without a prerelease
    // tail) are rejected, even though `SdkVersion::parse` would happily
    // strip the trailing `.0` and normalise to `8.0.100`.
    for bad in ["8.0.100.0", "8.0.100.5", "8.0.100.0-rc.1"] {
        let text = format!(r#"{{"sdk": {{"version": "{bad}"}}}}"#);
        let err = parse_global_json(&text).unwrap_err();
        assert!(
            matches!(&err, GlobalJsonError::InvalidVersion(s) if s == bad),
            "expected InvalidVersion({bad:?}), got {err:?}"
        );
    }
}

#[test]
fn rejects_sdk_version_with_too_few_components() {
    // Single-component (`9`) and two-component (`9.0`) versions are
    // also outside the strict feature-band schema. Note `9.0` was
    // already covered by the feature-band test; this verifies the
    // single-component shape is rejected too.
    for bad in ["9", "9-preview.1"] {
        let text = format!(r#"{{"sdk": {{"version": "{bad}"}}}}"#);
        let err = parse_global_json(&text).unwrap_err();
        assert!(
            matches!(&err, GlobalJsonError::InvalidVersion(s) if s == bad),
            "expected InvalidVersion({bad:?}), got {err:?}"
        );
    }
}

#[test]
fn accepts_sdk_version_at_feature_band_boundary() {
    // x.y.100 is the lowest legal feature band; verify it parses.
    let parsed = parse_global_json(r#"{"sdk": {"version": "8.0.100"}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.version, Some(SdkVersion::parse("8.0.100").unwrap()));
}

#[test]
fn rejects_roll_forward_without_version() {
    // Eight of nine rollForward policies are meaningless without a
    // version pin; .NET rejects this shape and so do we.
    for (label, rf) in [
        ("disable", RollForward::Disable),
        ("patch", RollForward::Patch),
        ("feature", RollForward::Feature),
        ("minor", RollForward::Minor),
        ("major", RollForward::Major),
        ("latestPatch", RollForward::LatestPatch),
        ("latestFeature", RollForward::LatestFeature),
        ("latestMinor", RollForward::LatestMinor),
    ] {
        let text = format!(r#"{{"sdk": {{"rollForward": "{label}"}}}}"#);
        let err = parse_global_json(&text).unwrap_err();
        assert!(
            matches!(err, GlobalJsonError::RollForwardRequiresVersion(got) if got == rf),
            "expected RollForwardRequiresVersion({rf:?}) for {label:?}"
        );
    }
}

#[test]
fn accepts_latest_major_without_version() {
    // latestMajor with no version is the documented way to say "pick
    // the freshest installed" — it's equivalent to the no-rollForward
    // default. Either reading parses cleanly.
    let parsed = parse_global_json(r#"{"sdk": {"rollForward": "latestMajor"}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.version, None);
    assert_eq!(parsed.roll_forward, Some(RollForward::LatestMajor));
}

#[test]
fn rejects_unknown_roll_forward() {
    let err = parse_global_json(r#"{"sdk": {"version": "1.0.100", "rollForward": "bogus"}}"#)
        .unwrap_err();
    assert!(matches!(err, GlobalJsonError::InvalidRollForward(s) if s == "bogus"));
}

#[test]
fn rejects_malformed_json() {
    // Unterminated object.
    let err = parse_global_json(r#"{"sdk": {"#).unwrap_err();
    assert!(matches!(err, GlobalJsonError::Syntax { .. }));
}

#[test]
fn rejects_unterminated_block_comment_when_required_token_missing() {
    // `/* …` runs to end of input; the stripper rejects it directly
    // rather than silently swallowing the rest.
    let err = parse_global_json("/* never closed").unwrap_err();
    assert!(matches!(err, GlobalJsonError::Syntax { .. }));
}

#[test]
fn rejects_unterminated_block_comment_after_complete_value() {
    // A complete JSON value followed by an unterminated block comment
    // must NOT be silently accepted — that would hide a configuration
    // typo. .NET's JSONC consumer rejects this too.
    let err = parse_global_json(r#"{"sdk": {}} /*"#).unwrap_err();
    assert!(
        matches!(&err, GlobalJsonError::Syntax { message, .. } if message.contains("block comment")),
        "expected unterminated block-comment syntax error, got {err:?}"
    );
}

#[test]
fn rejects_trailing_input_after_top_level_value() {
    let err = parse_global_json(r#"{"sdk":{}} extra junk"#).unwrap_err();
    assert!(matches!(err, GlobalJsonError::Syntax { .. }));
}

#[test]
fn unicode_escape_in_string_is_decoded() {
    // Not used by global.json's schema, but we want to make sure a
    // version field that happens to contain `\uXXXX` doesn't crash —
    // it'll either decode to a non-version string and `InvalidVersion`,
    // or (here) be rejected by SdkVersion::parse.
    let err = parse_global_json(r#"{"sdk": {"version": "9.0.0_bogus"}}"#).unwrap_err();
    // After unicode-escape decoding: "9.0.0_bogus" which `SdkVersion`
    // refuses because `_` is not a legal SemVer prerelease alphabet
    // (no leading `-` separator either).
    assert!(matches!(err, GlobalJsonError::InvalidVersion(s) if s == "9.0.0_bogus"));
}

// ============================================================
// Unit tests — `msbuild-sdks` map
// ============================================================

#[test]
fn parses_msbuild_sdks_single_entry() {
    let parsed =
        parse_global_json(r#"{"msbuild-sdks": {"Microsoft.Build.NoTargets": "3.7.134"}}"#).unwrap();
    assert_eq!(parsed.sdk, None);
    assert_eq!(parsed.msbuild_sdks.len(), 1);
    assert_eq!(
        parsed.msbuild_sdks.get("Microsoft.Build.NoTargets"),
        Some(&SdkVersion::parse("3.7.134").unwrap()),
    );
}

#[test]
fn parses_msbuild_sdks_multiple_entries() {
    // Two entries with mixed casing in the names; BTreeMap iteration
    // is ascending by key, so we can assert the deterministic order.
    let text = r#"{
        "msbuild-sdks": {
            "Microsoft.Build.NoTargets": "3.7.134",
            "Microsoft.Build.Traversal": "4.1.92"
        }
    }"#;
    let parsed = parse_global_json(text).unwrap();
    let keys: Vec<&str> = parsed.msbuild_sdks.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["Microsoft.Build.NoTargets", "Microsoft.Build.Traversal"],
    );
    assert_eq!(
        parsed.msbuild_sdks.get("Microsoft.Build.Traversal"),
        Some(&SdkVersion::parse("4.1.92").unwrap()),
    );
}

#[test]
fn parses_msbuild_sdks_with_prerelease_version() {
    // NuGet versions on Project SDK packages are looser than .NET host
    // SDK versions; prereleases like `11.0.0-beta.25569.5` are legitimate
    // and must parse.
    let parsed = parse_global_json(
        r#"{"msbuild-sdks": {"Microsoft.DotNet.Arcade.Sdk": "11.0.0-beta.25569.5"}}"#,
    )
    .unwrap();
    assert_eq!(
        parsed.msbuild_sdks.get("Microsoft.DotNet.Arcade.Sdk"),
        Some(&SdkVersion::parse("11.0.0-beta.25569.5").unwrap()),
    );
}

#[test]
fn parses_msbuild_sdks_alongside_sdk_block() {
    let text = r#"{
        "sdk": {"version": "9.0.100"},
        "msbuild-sdks": {"Some.Sdk": "1.2.3"}
    }"#;
    let parsed = parse_global_json(text).unwrap();
    assert_eq!(
        parsed.sdk.as_ref().and_then(|s| s.version.as_ref()),
        Some(&SdkVersion::parse("9.0.100").unwrap()),
    );
    assert_eq!(
        parsed.msbuild_sdks.get("Some.Sdk"),
        Some(&SdkVersion::parse("1.2.3").unwrap()),
    );
}

#[test]
fn null_msbuild_sdks_treated_as_empty() {
    // Mirrors `null`-tolerance for the `sdk` block: serialisers that
    // emit nullable fields shouldn't poison the parse.
    let parsed = parse_global_json(r#"{"msbuild-sdks": null}"#).unwrap();
    assert!(parsed.msbuild_sdks.is_empty());
}

#[test]
fn empty_msbuild_sdks_is_empty_map() {
    let parsed = parse_global_json(r#"{"msbuild-sdks": {}}"#).unwrap();
    assert!(parsed.msbuild_sdks.is_empty());
}

#[test]
fn rejects_msbuild_sdks_not_object() {
    let err = parse_global_json(r#"{"msbuild-sdks": "1.0.0"}"#).unwrap_err();
    assert!(matches!(
        err,
        GlobalJsonError::InvalidType {
            field: "msbuild-sdks",
            expected: "object"
        }
    ));
}

#[test]
fn rejects_msbuild_sdks_entry_not_string() {
    // Map values must be strings naming a version. Numbers, bools,
    // objects, and arrays are all rejected with the entry name attached
    // so the diagnostic can pinpoint the offender.
    let err = parse_global_json(r#"{"msbuild-sdks": {"Bad.Sdk": 12}}"#).unwrap_err();
    assert!(
        matches!(
            &err,
            GlobalJsonError::InvalidMsBuildSdksEntryType { name, expected: "string" }
                if name == "Bad.Sdk"
        ),
        "expected InvalidMsBuildSdksEntryType for Bad.Sdk, got {err:?}",
    );
}

#[test]
fn rejects_msbuild_sdks_unparseable_version() {
    let err = parse_global_json(r#"{"msbuild-sdks": {"Bad.Sdk": "not-a-version"}}"#).unwrap_err();
    assert!(
        matches!(
            &err,
            GlobalJsonError::InvalidMsBuildSdkVersion { name, value }
                if name == "Bad.Sdk" && value == "not-a-version"
        ),
        "expected InvalidMsBuildSdkVersion for Bad.Sdk, got {err:?}",
    );
}

#[test]
fn null_msbuild_sdks_entry_is_skipped() {
    // Tolerance for serialiser-emitted nulls inside the map — the
    // surrounding map still parses, just without that entry.
    let parsed =
        parse_global_json(r#"{"msbuild-sdks": {"Kept.Sdk": "1.0.0", "Skipped.Sdk": null}}"#)
            .unwrap();
    assert_eq!(parsed.msbuild_sdks.len(), 1);
    assert!(parsed.msbuild_sdks.contains_key("Kept.Sdk"));
    assert!(!parsed.msbuild_sdks.contains_key("Skipped.Sdk"));
}

#[test]
fn duplicate_msbuild_sdks_keys_keep_first() {
    // JSON duplicate-key semantics are technically undefined.
    // `parse_msbuild_sdks` keeps the first occurrence — document that
    // here so behaviour stays stable even on malformed input.
    let parsed =
        parse_global_json(r#"{"msbuild-sdks": {"Dup.Sdk": "1.0.0", "Dup.Sdk": "2.0.0"}}"#).unwrap();
    assert_eq!(
        parsed.msbuild_sdks.get("Dup.Sdk"),
        Some(&SdkVersion::parse("1.0.0").unwrap()),
    );
}

// ============================================================
// `into_spec` semantics
// ============================================================

#[test]
fn into_spec_with_version_defaults_roll_forward_to_patch() {
    // .NET defaults rollForward to `patch` (not `latestPatch`) when
    // version is set but rollForward is absent. Patch prefers the
    // exact pin if installed, only rolling forward when missing —
    // latestPatch would prefer 9.0.105 over an installed 9.0.100 pin.
    let settings = GlobalJsonSettings {
        version: Some(SdkVersion::parse("9.0.100").unwrap()),
        roll_forward: None,
        allow_prerelease: None,
        paths: None,
    };
    let spec = settings.into_spec(true);
    assert_eq!(spec.roll_forward(), RollForward::Patch);
    assert_eq!(spec.version(), Some(&SdkVersion::parse("9.0.100").unwrap()));
}

#[test]
fn into_spec_respects_explicit_roll_forward() {
    let settings = GlobalJsonSettings {
        version: Some(SdkVersion::parse("9.0.100").unwrap()),
        roll_forward: Some(RollForward::Major),
        allow_prerelease: None,
        paths: None,
    };
    let spec = settings.into_spec(true);
    assert_eq!(spec.roll_forward(), RollForward::Major);
}

#[test]
fn into_spec_no_version_returns_any_version() {
    let settings = GlobalJsonSettings {
        version: None,
        roll_forward: Some(RollForward::Patch), // ignored by .NET when no version
        allow_prerelease: Some(false),
        paths: None,
    };
    let spec = settings.into_spec(true);
    assert_eq!(spec.version(), None);
    assert_eq!(spec.roll_forward(), RollForward::LatestMajor);
    assert!(!spec.allow_prerelease());
}

#[test]
fn into_spec_host_default_used_when_setting_absent() {
    let settings = GlobalJsonSettings {
        version: Some(SdkVersion::parse("9.0.100").unwrap()),
        roll_forward: None,
        allow_prerelease: None,
        paths: None,
    };
    assert!(settings.clone().into_spec(true).allow_prerelease());
    assert!(!settings.into_spec(false).allow_prerelease());
}

#[test]
fn into_spec_explicit_allow_prerelease_overrides_host_default() {
    let settings = GlobalJsonSettings {
        version: Some(SdkVersion::parse("9.0.100").unwrap()),
        roll_forward: None,
        allow_prerelease: Some(false),
        paths: None,
    };
    assert!(!settings.clone().into_spec(true).allow_prerelease());
    let settings = GlobalJsonSettings {
        version: Some(SdkVersion::parse("9.0.100").unwrap()),
        roll_forward: None,
        allow_prerelease: Some(true),
        paths: None,
    };
    assert!(settings.into_spec(false).allow_prerelease());
}

#[test]
fn into_spec_prerelease_pin_forces_allow_prerelease() {
    // Mirrors the test in version_spec/tests.rs: a prerelease version
    // unconditionally turns on allow_prerelease regardless of either
    // the JSON setting or the host default, because the pin itself
    // wouldn't satisfy otherwise.
    let settings = GlobalJsonSettings {
        version: Some(SdkVersion::parse("9.0.100-preview.1").unwrap()),
        roll_forward: None,
        allow_prerelease: Some(false),
        paths: None,
    };
    assert!(settings.into_spec(false).allow_prerelease());
}

// ============================================================
// `find_global_json` — filesystem walk
// ============================================================

#[test]
fn find_global_json_in_start_dir() {
    let temp = TempDir::new().unwrap();
    let gj = temp.path().join("global.json");
    fs::write(&gj, "{}").unwrap();
    let found = find_global_json(temp.path()).unwrap();
    assert_eq!(found, gj);
}

#[test]
fn find_global_json_walks_upward() {
    let temp = TempDir::new().unwrap();
    let gj = temp.path().join("global.json");
    fs::write(&gj, "{}").unwrap();
    let nested = temp.path().join("a").join("b").join("c");
    fs::create_dir_all(&nested).unwrap();
    let found = find_global_json(&nested).unwrap();
    assert_eq!(found, gj);
}

#[test]
fn find_global_json_picks_closest_ancestor() {
    let temp = TempDir::new().unwrap();
    let outer = temp.path().join("global.json");
    fs::write(&outer, "{}").unwrap();
    let inner_dir = temp.path().join("project");
    fs::create_dir(&inner_dir).unwrap();
    let inner = inner_dir.join("global.json");
    fs::write(&inner, "{}").unwrap();
    let nested = inner_dir.join("src");
    fs::create_dir(&nested).unwrap();
    let found = find_global_json(&nested).unwrap();
    assert_eq!(found, inner, "closest ancestor wins, not outermost");
}

#[test]
fn find_global_json_returns_none_when_absent() {
    let temp = TempDir::new().unwrap();
    let nested = temp.path().join("a").join("b");
    fs::create_dir_all(&nested).unwrap();
    assert_eq!(find_global_json(&nested), None);
}

#[test]
fn find_global_json_ignores_directory_named_global_json() {
    // Defensive: a directory called `global.json` shouldn't satisfy.
    let temp = TempDir::new().unwrap();
    fs::create_dir(temp.path().join("global.json")).unwrap();
    assert_eq!(find_global_json(temp.path()), None);
}

// ============================================================
// Strip-comments helper — interaction with string literals
// ============================================================

#[test]
fn strip_comments_preserves_inside_strings() {
    let result = super::strip_jsonc_comments(r#"{"k": "// not a comment"}"#).unwrap();
    assert_eq!(result, r#"{"k": "// not a comment"}"#);
}

#[test]
fn strip_comments_preserves_inside_strings_with_escaped_quote() {
    // `"\"// still a string//"` — the embedded backslash-quote shouldn't
    // end the string.
    let result = super::strip_jsonc_comments(r#"{"k": "\"// inside\""}"#).unwrap();
    assert_eq!(result, r#"{"k": "\"// inside\""}"#);
}

#[test]
fn strip_comments_handles_block_then_line() {
    let result = super::strip_jsonc_comments("/* x */ //y\n{}").unwrap();
    // Block becomes a single space-equivalent; line comment is gone.
    // Exact spacing doesn't matter for the parser — just that the
    // structural content survives.
    assert!(result.contains("{}"));
    assert!(!result.contains("//"));
    assert!(!result.contains("/*"));
}

#[test]
fn strip_comments_inserts_delimiter_so_tokens_dont_fuse() {
    // Without the delimiter, `tr/*x*/ue` would collapse to `true` and
    // a malformed file would silently parse — the comment has to act
    // as token boundary, just like whitespace would.
    let result = super::strip_jsonc_comments("tr/*x*/ue").unwrap();
    assert!(
        !result.contains("true"),
        "block comment fused two halves into a valid token: {result:?}"
    );
}

#[test]
fn block_comment_does_not_fuse_tokens_in_full_parse() {
    // End-to-end sanity check: a block comment in the middle of an
    // identifier-like value must not let it parse cleanly.
    let err = parse_global_json(r#"{"sdk": {"allowPrerelease": tr/*x*/ue}}"#).unwrap_err();
    assert!(matches!(err, GlobalJsonError::Syntax { .. }));
}

// ============================================================
// Property tests
// ============================================================

/// Generator for legal SemVer-shape version strings the SDK parser
/// will accept. Mirrors the proptest strategy in sdk_resolver/tests.rs,
/// but constrained to valid SDK feature bands (third component ≥ 100)
/// because `parse_global_json` rejects everything below — see the
/// `rejects_sdk_version_with_invalid_feature_band` test.
fn version_strategy() -> impl Strategy<Value = String> {
    let three_part = (1u64..40, 0u64..20, 100u64..600);
    let pre = prop::option::of(prop::sample::select(vec![
        "preview.1",
        "preview.2",
        "rc.1",
        "rc.2",
    ]));
    (three_part, pre).prop_map(|((a, b, c), pre)| match pre {
        Some(suffix) => format!("{a}.{b}.{c}-{suffix}"),
        None => format!("{a}.{b}.{c}"),
    })
}

fn roll_forward_strategy() -> impl Strategy<Value = (&'static str, RollForward)> {
    prop::sample::select(vec![
        ("disable", RollForward::Disable),
        ("patch", RollForward::Patch),
        ("feature", RollForward::Feature),
        ("minor", RollForward::Minor),
        ("major", RollForward::Major),
        ("latestPatch", RollForward::LatestPatch),
        ("latestFeature", RollForward::LatestFeature),
        ("latestMinor", RollForward::LatestMinor),
        ("latestMajor", RollForward::LatestMajor),
    ])
}

/// Property: round-trip a constructed `global.json` through the
/// parser. Builds JSON by template (so we don't need a JSON writer),
/// then asserts each parsed field matches the input that produced it.
///
/// Distribution sanity: every shape (version-only, all three,
/// allow-prerelease combinations) must be exercised across the run.
#[test]
fn parse_roundtrips_constructed_global_json() {
    let mut runner = TestRunner::new(PtConfig {
        cases: 256,
        ..PtConfig::default()
    });
    let saw_full = Cell::new(0u32);
    let saw_version_only = Cell::new(0u32);
    let saw_with_pre = Cell::new(0u32);
    let saw_with_pre_false = Cell::new(0u32);

    let strat = (
        version_strategy(),
        prop::option::of(roll_forward_strategy()),
        prop::option::of(any::<bool>()),
    );
    runner
        .run(&strat, |(ver_s, rf, allow_pre)| {
            // Build the JSON.
            let mut fields = Vec::new();
            fields.push(format!(r#""version": "{ver_s}""#));
            if let Some((label, _)) = rf {
                fields.push(format!(r#""rollForward": "{label}""#));
            }
            if let Some(b) = allow_pre {
                fields.push(format!(r#""allowPrerelease": {b}"#));
            }
            let json = format!(r#"{{"sdk": {{{}}}}}"#, fields.join(", "));

            // Parse and check.
            let parsed = parse_global_json(&json).unwrap().sdk.unwrap();
            assert_eq!(
                parsed.version,
                Some(SdkVersion::parse(&ver_s).unwrap()),
                "version field"
            );
            assert_eq!(
                parsed.roll_forward,
                rf.map(|(_, rf)| rf),
                "rollForward field"
            );
            assert_eq!(parsed.allow_prerelease, allow_pre, "allowPrerelease field");

            // Distribution accounting.
            if rf.is_some() && allow_pre.is_some() {
                saw_full.set(saw_full.get() + 1);
            }
            if rf.is_none() && allow_pre.is_none() {
                saw_version_only.set(saw_version_only.get() + 1);
            }
            if allow_pre == Some(true) {
                saw_with_pre.set(saw_with_pre.get() + 1);
            }
            if allow_pre == Some(false) {
                saw_with_pre_false.set(saw_with_pre_false.get() + 1);
            }
            Ok(())
        })
        .unwrap();

    // With 256 cases over 12 equally-likely shapes (1 of {None,Some}^2
    // for roll_forward × 1 of {None,Some(true),Some(false)} for
    // allow_pre), each bucket should fire ~21 times. Demand at least
    // five of each — false-positive probability under independent
    // uniform draws is well under 1e-11.
    assert!(saw_full.get() >= 5, "full shape rare: {}", saw_full.get());
    assert!(
        saw_version_only.get() >= 5,
        "version-only rare: {}",
        saw_version_only.get()
    );
    assert!(
        saw_with_pre.get() >= 5,
        "allowPrerelease=true rare: {}",
        saw_with_pre.get()
    );
    assert!(
        saw_with_pre_false.get() >= 5,
        "allowPrerelease=false rare: {}",
        saw_with_pre_false.get()
    );
}

/// Property: injecting a comment between any pair of JSON tokens
/// doesn't change the parse result. Generators emit a randomised mix
/// of line and block comments and splice them after each
/// whitespace-eligible position.
#[test]
fn comment_injection_does_not_change_parse() {
    let mut runner = TestRunner::new(PtConfig {
        cases: 128,
        ..PtConfig::default()
    });
    let block_count = Cell::new(0u32);
    let line_count = Cell::new(0u32);

    // The baseline is deliberately padded with whitespace at every
    // legal token boundary so the per-case `kinds` vector actually
    // gets exercised. Without this, the inner `{"sdk":...}` form
    // exposes only two spaces and the `>=50` distribution thresholds
    // become a tight Binomial(128, 0.5) lower-tail assertion.
    runner
        .run(
            &(version_strategy(), prop::collection::vec(any::<bool>(), 10)),
            |(ver, kinds)| {
                let baseline = format!(r#" {{ "sdk" : {{ "version" : "{ver}" }} }} "#);
                let mut out = String::new();
                let mut idx = 0;
                for ch in baseline.chars() {
                    out.push(ch);
                    if ch == ' ' && idx < kinds.len() {
                        if kinds[idx] {
                            out.push_str("/* xx */");
                            block_count.set(block_count.get() + 1);
                        } else {
                            // Line comments need a newline to terminate.
                            out.push_str("//xx\n");
                            line_count.set(line_count.get() + 1);
                        }
                        idx += 1;
                    }
                }
                let parsed = parse_global_json(&out)
                    .expect("comment injection should keep parse valid")
                    .sdk
                    .unwrap();
                assert_eq!(parsed.version, Some(SdkVersion::parse(&ver).unwrap()));
                Ok(())
            },
        )
        .unwrap();

    // The padded baseline has 10 spaces, so each case injects all 10
    // comments and the run produces ~1280 total injections drawn
    // uniformly from {block, line} — expected ~640 of each. Demand at
    // least 50 of each as a hard generator-skew tripwire. P[<50] under
    // independent uniform draws is well below 1e-30.
    assert!(
        block_count.get() >= 50,
        "too few block comments: {}",
        block_count.get()
    );
    assert!(
        line_count.get() >= 50,
        "too few line comments: {}",
        line_count.get()
    );
}

// ============================================================
// Unit tests — sdk.paths (.NET 10)
// ============================================================

#[test]
fn paths_absent_yields_none() {
    let parsed = parse_global_json(r#"{"sdk": {"version": "9.0.100"}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.paths, None);
}

#[test]
fn paths_null_yields_none() {
    // Symmetry with the other optional fields: explicit `null` is
    // tolerated as "field absent" so a serialiser emitting nullable
    // optionals doesn't reject the file.
    let parsed = parse_global_json(r#"{"sdk": {"paths": null}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.paths, None);
}

#[test]
fn paths_empty_array_yields_some_empty() {
    // Empty array is a meaningful shape: the .NET host treats it as
    // an explicit opt-out from the host install ("no SDK roots
    // available"). The LSP consumer relies on `Some(empty)` vs `None`
    // to distinguish that from the unconfigured default.
    let parsed = parse_global_json(r#"{"sdk": {"paths": []}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.paths, Some(Vec::new()));
}

#[test]
fn paths_host_token_recognised() {
    let parsed = parse_global_json(r#"{"sdk": {"paths": ["$host$"]}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.paths, Some(vec![SdkPathEntry::Host]));
}

#[test]
fn paths_relative_entry_kept_verbatim() {
    let parsed = parse_global_json(r#"{"sdk": {"paths": ["artifacts/bin/dotnet"]}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(
        parsed.paths,
        Some(vec![SdkPathEntry::Relative(
            "artifacts/bin/dotnet".to_string()
        )])
    );
}

#[test]
fn paths_preserves_entry_order() {
    // The .NET host iterates entries in document order; first match
    // wins. Both downstream consumers (the resolver's first-match
    // logic and the diagnostic that lists consulted roots) rely on
    // the parser preserving that order.
    let parsed = parse_global_json(r#"{"sdk": {"paths": ["first", "$host$", "third"]}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(
        parsed.paths,
        Some(vec![
            SdkPathEntry::Relative("first".to_string()),
            SdkPathEntry::Host,
            SdkPathEntry::Relative("third".to_string()),
        ])
    );
}

#[test]
fn host_token_is_case_sensitive() {
    // Pin the case-sensitivity choice explicitly: `$Host$`, `$HOST$`,
    // and `$hOsT$` all fall through to `Relative` rather than being
    // smushed into `Host`. Matches what the .NET host does, and
    // matters because typing `$Host$` would silently turn into a path
    // lookup against a directory literally named `$Host$`.
    for v in ["$Host$", "$HOST$", "$hOsT$", "$host", "host$", "$hosts$"] {
        let text = format!(r#"{{"sdk": {{"paths": ["{v}"]}}}}"#);
        let parsed = parse_global_json(&text).unwrap().sdk.unwrap();
        assert_eq!(
            parsed.paths,
            Some(vec![SdkPathEntry::Relative(v.to_string())]),
            "case-sensitivity broken for {v:?}"
        );
    }
}

#[test]
fn paths_with_other_sdk_fields() {
    // `paths` sits next to the existing fields; populating it must
    // not disturb their parse.
    let text = r#"
        {
            "sdk": {
                "version": "8.0.401",
                "rollForward": "latestMinor",
                "allowPrerelease": false,
                "paths": ["$host$", "../local"]
            }
        }
    "#;
    let parsed = parse_global_json(text).unwrap().sdk.unwrap();
    assert_eq!(parsed.version, Some(SdkVersion::parse("8.0.401").unwrap()));
    assert_eq!(parsed.roll_forward, Some(RollForward::LatestMinor));
    assert_eq!(parsed.allow_prerelease, Some(false));
    assert_eq!(
        parsed.paths,
        Some(vec![
            SdkPathEntry::Host,
            SdkPathEntry::Relative("../local".to_string()),
        ])
    );
}

// ---- sdk.paths error paths --------------------------------------

#[test]
fn rejects_paths_not_array() {
    for bad in [r#""$host$""#, "42", "true", "{}"] {
        let text = format!(r#"{{"sdk": {{"paths": {bad}}}}}"#);
        let err = parse_global_json(&text).unwrap_err();
        assert!(
            matches!(
                &err,
                GlobalJsonError::InvalidType {
                    field: "sdk.paths",
                    expected: "array"
                }
            ),
            "expected InvalidType for {bad}, got {err:?}"
        );
    }
}

#[test]
fn non_string_entries_are_silently_skipped() {
    // The .NET host (fxr/sdk_info.cpp) emits a trace warning and
    // skips non-string entries rather than rejecting the file. We
    // match that leniency — rejecting the whole file would lose the
    // user's `sdk.version` pin for a workspace the host would still
    // resolve. Surrounding string entries are kept in order.
    let text = r#"{"sdk": {"paths": ["$host$", 42, ".dotnet"]}}"#;
    let parsed = parse_global_json(text).unwrap().sdk.unwrap();
    assert_eq!(
        parsed.paths,
        Some(vec![
            SdkPathEntry::Host,
            SdkPathEntry::Relative(".dotnet".into())
        ])
    );
}

#[test]
fn null_entries_are_silently_skipped() {
    // `null` is just another non-string and follows the same
    // skip-don't-reject rule. The remaining string entries survive.
    let parsed = parse_global_json(r#"{"sdk": {"paths": [null, "$host$"]}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.paths, Some(vec![SdkPathEntry::Host]));
}

#[test]
fn all_non_string_entries_yields_empty_paths() {
    // If every entry is non-string, the result is an empty Vec —
    // not None. The field was present and the array was syntactically
    // valid, so we still record "explicit paths block, zero entries"
    // rather than falling back to host-only.
    let parsed = parse_global_json(r#"{"sdk": {"paths": [42, true, null]}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(parsed.paths, Some(Vec::new()));
}

#[test]
fn empty_string_entry_kept_as_relative() {
    // The .NET host pushes the empty string as-is into its paths
    // list and joins against the global.json directory at resolve
    // time (yielding the directory itself). Odd, but we match it
    // bit-for-bit. Both `paths` entries become `Relative`, including
    // the empty one.
    let parsed = parse_global_json(r#"{"sdk": {"paths": ["", "$host$"]}}"#)
        .unwrap()
        .sdk
        .unwrap();
    assert_eq!(
        parsed.paths,
        Some(vec![
            SdkPathEntry::Relative(String::new()),
            SdkPathEntry::Host,
        ])
    );
}

// ============================================================
// Property tests — sdk.paths
// ============================================================

/// Generator for an entry of `sdk.paths` as the raw string that would
/// appear in JSON. Returned as `String` so the test can serialise
/// trivially; the PBT classifies it itself to compute the expected
/// `SdkPathEntry`. Excludes characters that would need JSON escaping
/// (`"`, `\`, control characters) so we can splice it between
/// straight quotes without an escaper.
fn paths_entry_string_strategy() -> impl Strategy<Value = String> {
    // Tokens drawn from the kinds of paths real `global.json` files
    // carry: relative paths, parent-walks, plain names, plus the
    // literal `$host$`. The PBT cares about three things:
    //   (a) `$host$` must round-trip to `Host`;
    //   (b) every other non-empty string must round-trip to
    //       `Relative(s)`;
    //   (c) order is preserved.
    // A small generator gives us all three with high hit-rates on
    // `$host$`, which the round-trip property would otherwise see
    // rarely.
    prop::sample::select(vec![
        "$host$".to_string(),
        "./local".to_string(),
        "../foo".to_string(),
        "artifacts/bin/dotnet".to_string(),
        "x".to_string(),
        "with spaces in it".to_string(),
        "$Host$".to_string(),
        "$host".to_string(),
        "/abs/path".to_string(),
        "C:/Windows/Style".to_string(),
        // The .NET host accepts empty strings, so we do too — they
        // round-trip as `Relative("")`.
        String::new(),
    ])
}

/// Property: serialise an arbitrary list of entry strings into a
/// `global.json` body, parse it, and check that the result equals the
/// per-entry classification (`$host$` → `Host`, anything else →
/// `Relative`). Also verifies order preservation across runs.
///
/// Distribution sanity: at least some runs must include `$host$` so
/// the `Host` branch is exercised, and at least some must mix `Host`
/// and `Relative` in the same array.
#[test]
fn paths_roundtrip_preserves_classification_and_order() {
    let mut runner = TestRunner::new(PtConfig {
        cases: 256,
        ..PtConfig::default()
    });
    let saw_host = Cell::new(0u32);
    let saw_relative = Cell::new(0u32);
    let saw_mixed = Cell::new(0u32);
    let saw_empty = Cell::new(0u32);

    let strat = prop::collection::vec(paths_entry_string_strategy(), 0..6);
    runner
        .run(&strat, |entries| {
            // Splice each string between straight quotes. The
            // strategy guarantees JSON-safe content.
            let body = entries
                .iter()
                .map(|s| format!(r#""{s}""#))
                .collect::<Vec<_>>()
                .join(", ");
            let json = format!(r#"{{"sdk": {{"paths": [{body}]}}}}"#);
            let parsed = parse_global_json(&json)
                .expect("constructed JSON should parse")
                .sdk
                .unwrap();

            // Expected classification: literal `$host$` → Host;
            // everything else → Relative(s.clone()).
            let expected: Vec<SdkPathEntry> = entries
                .iter()
                .map(|s| {
                    if s == "$host$" {
                        SdkPathEntry::Host
                    } else {
                        SdkPathEntry::Relative(s.clone())
                    }
                })
                .collect();
            assert_eq!(parsed.paths, Some(expected));

            // Distribution accounting.
            let mut has_host = false;
            let mut has_rel = false;
            for s in &entries {
                if s == "$host$" {
                    has_host = true;
                    saw_host.set(saw_host.get() + 1);
                } else {
                    has_rel = true;
                    saw_relative.set(saw_relative.get() + 1);
                }
            }
            if has_host && has_rel {
                saw_mixed.set(saw_mixed.get() + 1);
            }
            if entries.is_empty() {
                saw_empty.set(saw_empty.get() + 1);
            }
            Ok(())
        })
        .unwrap();

    // Each run draws 0..6 entries from a 10-element pool with 1 of 10
    // entries being `$host$`. Across 256 cases × ~3 entries/case ≈ 768
    // draws, ~77 of them `$host$` and ~691 `Relative`. The mixed
    // bucket requires both a `$host$` draw *and* a non-`$host$` draw
    // in the same case; with a mean array length of 3 the expected
    // count is well above 50. The empty bucket fires whenever the
    // length draws to 0 — probability 1/7 over 256 ≈ 37.
    assert!(saw_host.get() >= 20, "too few $host$: {}", saw_host.get());
    assert!(
        saw_relative.get() >= 50,
        "too few Relative: {}",
        saw_relative.get()
    );
    assert!(saw_mixed.get() >= 10, "too few mixed: {}", saw_mixed.get());
    assert!(saw_empty.get() >= 5, "too few empty: {}", saw_empty.get());
}

/// Property: the parser is total over an arbitrary sequence of
/// bytes intended to land in the `sdk.paths` value position. The
/// outcome must be one of (a) `Ok` with a path list, (b) a
/// well-typed `GlobalJsonError` — and *never* a panic, infinite
/// loop, or behaviour outside that envelope.
///
/// Generates arbitrary printable-ASCII payloads (escapes excluded
/// so the surrounding JSON stays well-formed when the strategy
/// emits a plain string); splices them after `"sdk": { "paths":`.
/// Most random payloads will fail with `GlobalJsonError::Syntax`;
/// what matters is that we never crash.
#[test]
fn paths_parser_is_total() {
    let mut runner = TestRunner::new(PtConfig {
        cases: 256,
        ..PtConfig::default()
    });
    let strat = "[ -~&&[^\\\\\"]]{0,40}";
    runner
        .run(&strat, |payload| {
            let json = format!(r#"{{"sdk": {{"paths": {payload}}}}}"#);
            let _ = parse_global_json(&json); // must not panic
            Ok(())
        })
        .unwrap();
}

// ============================================================
// Workload-set pin detection (`GlobalJson::pins_workload_set`)
// ============================================================

#[test]
fn workload_version_in_sdk_block_pins_workload_set() {
    let parsed = parse_global_json(r#"{"sdk": {"workloadVersion": "10.0.201"}}"#).unwrap();
    assert!(parsed.pins_workload_set);
    // The host-side sdk block ignores the key: no version pin arises.
    assert_eq!(parsed.sdk, Some(GlobalJsonSettings::default()));
}

#[test]
fn ordinary_sdk_block_does_not_pin_workload_set() {
    let parsed = parse_global_json(r#"{"sdk": {"version": "10.0.300"}}"#).unwrap();
    assert!(!parsed.pins_workload_set);
    let parsed = parse_global_json(r#"{"sdk": {}}"#).unwrap();
    assert!(!parsed.pins_workload_set);
    let parsed = parse_global_json("{}").unwrap();
    assert!(!parsed.pins_workload_set);
}

#[test]
fn workload_pin_detection_is_case_insensitive() {
    // The workload GlobalJsonReader matches both the `sdk` key and the
    // `workloadVersion` key ordinal-case-insensitively — unlike the
    // host's case-sensitive sdk block, which ignores `"SDK"` entirely.
    let parsed = parse_global_json(r#"{"SDK": {"WORKLOADVERSION": "10.0.201"}}"#).unwrap();
    assert!(parsed.pins_workload_set);
    assert_eq!(parsed.sdk, None);
}

#[test]
fn non_object_sdk_value_pins_workload_set() {
    // The workload reader throws JsonFormatException on a non-object
    // `sdk` value (including null), failing the real evaluation, so the
    // document is outside the workload resolution envelope even though
    // the host-side parser folds `"sdk": null` into "absent".
    let parsed = parse_global_json(r#"{"sdk": null}"#).unwrap();
    assert!(parsed.pins_workload_set);
    assert_eq!(parsed.sdk, None);
    let parsed = parse_global_json(r#"{"Sdk": 42}"#).unwrap();
    assert!(parsed.pins_workload_set);
}

#[test]
fn workloads_update_mode_string_does_not_pin() {
    // A string workloads-update-mode only toggles the set-vs-manifests
    // preference, which changes behaviour only when a workloadsets
    // directory exists — a layout the locator resolution degrades on
    // independently.
    let parsed =
        parse_global_json(r#"{"sdk": {"workloads-update-mode": "workload-set"}}"#).unwrap();
    assert!(!parsed.pins_workload_set);
    // A non-string value makes the workload reader throw: pin.
    let parsed = parse_global_json(r#"{"sdk": {"workloads-update-mode": true}}"#).unwrap();
    assert!(parsed.pins_workload_set);
}

#[test]
fn duplicate_sdk_keys_pin_if_any_occurrence_pins() {
    // The workload reader walks every top-level `sdk` occurrence.
    let parsed = parse_global_json(
        r#"{"sdk": {"version": "10.0.300"}, "sdk": {"workloadVersion": "10.0.201"}}"#,
    )
    .unwrap();
    assert!(parsed.pins_workload_set);
}

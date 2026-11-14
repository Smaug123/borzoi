//! Every `fcs-dump` oracle that checks at a caller's `<LangVersion>` checks at
//! that version, the ones FCS calls out of support included.
//!
//! FCS refuses a version below 8.0 (FS3880) unless
//! `SKIP_VERSION_SUPPORTED_CHECK` is set: a single-file check then reports
//! FS3880 and checks at FCS's default, and a project check fails in script
//! resolution. A differential pinned to such a version would be graded against
//! the default instead, or not at all. The fixture uses `_.Length`, which F# 8.0
//! introduced and FCS's checker gates (FS3350 below 8.0), so a check at 7.0
//! reports it and a check at the default does not.
//!
//! Each oracle runs in a fresh process, its first request pinned at 7.0, so no
//! earlier request's state can stand in for the pin's own.

use std::path::{Path, PathBuf};

use borzoi_cst::language_version::LanguageVersion;
use serde_json::Value;

use crate::common::{
    OracleRefScope, fcs_dump_batch_child, host_fs, invoke_fcs_dump_project_with_refs,
};

/// FCS's "feature is not available in this language version".
const FEATURE_UNAVAILABLE: i64 = 3350;
/// FCS's "language version is out of support".
const OUT_OF_SUPPORT: i64 = 3880;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("Lib.fs");
    std::fs::write(
        &file,
        "module M\nlet lengths (xs: string list) = xs |> List.map _.Length\n",
    )
    .expect("write fixture");
    (dir, file)
}

/// A response line, parsed, with a `BatchError` turned into a panic.
fn parse(oracle: &str, line: &str) -> Value {
    let v: Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("{oracle}: unparseable response ({e}): {line}"));
    if let Some(err) = v.get("BatchError") {
        panic!("{oracle} refused the request: {err}");
    }
    v
}

/// The error numbers of a `{ Files: [ { Diagnostics } ] }` response.
fn project_codes(v: &Value) -> Vec<i64> {
    v["Files"]
        .as_array()
        .expect("Files")
        .iter()
        .flat_map(|f| f["Diagnostics"].as_array().expect("Diagnostics"))
        .map(|d| d["ErrorNumber"].as_i64().expect("ErrorNumber"))
        .collect()
}

/// The error numbers of a `file-batch` `types` response.
fn file_codes(v: &Value) -> Vec<i64> {
    v["Errors"]
        .as_array()
        .expect("Errors")
        .iter()
        .map(|d| d["Code"].as_i64().expect("Code"))
        .collect()
}

fn project_request(file: &Path, lang: &str) -> String {
    serde_json::json!({
        "paths": [file.display().to_string()],
        "langversion": lang,
    })
    .to_string()
}

/// The check at 7.0 reports the 8.0 feature's gate and not FS3880, and the
/// check at 8.0 reports neither, so the fixture tells the versions apart.
fn assert_checked_at_the_pin(oracle: &str, at_7: &[i64], at_8: &[i64]) {
    assert!(
        !at_7.contains(&OUT_OF_SUPPORT) && at_7.contains(&FEATURE_UNAVAILABLE),
        "{oracle} pinned at 7.0 must check at 7.0, but reported {at_7:?}"
    );
    assert!(
        !at_8.contains(&FEATURE_UNAVAILABLE) && !at_8.contains(&OUT_OF_SUPPORT),
        "{oracle} pinned at 8.0 must accept `_.Length`, but reported {at_8:?}"
    );
}

#[test]
fn uses_project_batch_checks_at_an_out_of_support_pin() {
    let (_dir, file) = fixture();
    let mut child = fcs_dump_batch_child("uses-project-batch");
    let mut codes = |lang| {
        project_codes(&parse(
            "uses-project-batch",
            &child.request(&project_request(&file, lang)),
        ))
    };
    let at_7 = codes("7.0");
    let at_8 = codes("8.0");
    assert_checked_at_the_pin("uses-project-batch", &at_7, &at_8);
}

#[test]
fn xmldoc_batch_checks_at_an_out_of_support_pin() {
    let (_dir, file) = fixture();
    let mut child = fcs_dump_batch_child("xmldoc-batch");
    let mut codes = |lang| {
        project_codes(&parse(
            "xmldoc-batch",
            &child.request(&project_request(&file, lang)),
        ))
    };
    let at_7 = codes("7.0");
    let at_8 = codes("8.0");
    assert_checked_at_the_pin("xmldoc-batch", &at_7, &at_8);
}

#[test]
fn file_batch_checks_at_an_out_of_support_pin() {
    let (_dir, file) = fixture();
    let mut child = fcs_dump_batch_child("file-batch");
    let mut codes = |lang| {
        let request = serde_json::json!({
            "kind": "types",
            "path": file.display().to_string(),
            "langversion": lang,
        })
        .to_string();
        file_codes(&parse("file-batch", &child.request(&request)))
    };
    let at_7 = codes("7.0");
    let at_8 = codes("8.0");
    assert_checked_at_the_pin("file-batch", &at_7, &at_8);
}

#[test]
fn uses_project_checks_at_an_out_of_support_pin() {
    let (_dir, file) = fixture();
    let codes = |lang| {
        let out = invoke_fcs_dump_project_with_refs(
            &[file.as_path()],
            &[],
            OracleRefScope::SdkPlusExtra,
            &[],
            Some(lang),
        );
        project_codes(&parse("uses-project", out.trim()))
    };
    let at_7 = codes("7.0");
    let at_8 = codes("8.0");
    assert_checked_at_the_pin("uses-project", &at_7, &at_8);
}

/// The switch is built in one place, which sets `SKIP_VERSION_SUPPORTED_CHECK`;
/// an oracle that spelled its own would check below 8.0 at FCS's default.
#[test]
fn every_langversion_switch_comes_from_the_one_builder() {
    let program = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fcs-dump/Program.fs");
    let text = host_fs::read_to_string(&program)
        .unwrap_or_else(|e| panic!("read {}: {e}", program.display()));
    let builders: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("\"--langversion:\" +"))
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(
        builders.len(),
        1,
        "`--langversion:` switches are built at lines {builders:?} of {}; build every one \
         through `pinnedLangVersionArgs`",
        program.display()
    );
}

/// `LanguageVersion::SUPPORTED_MIN` is the floor of the oracle's FCS: checked
/// as an unmodified compiler checks (`file-batch`'s `supportCheck`), every
/// version below it, and none from it up, is refused as out of support.
///
/// cst's `language_features` holds the constant to the `fsharp-src` compiler's
/// source; this holds it to the FCS that runs, so an SDK or package bump that
/// moves the floor fails here.
#[test]
fn the_support_floor_is_fcs_own() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut child = fcs_dump_batch_child("file-batch");
    let mut refused = Vec::new();
    for (i, lang) in LanguageVersion::ALL.into_iter().enumerate() {
        // A file per version: the checker caches by file and options.
        let file = dir.path().join(format!("V{i}.fs"));
        std::fs::write(&file, "module M\nlet x = 1\n").expect("write fixture");
        let request = serde_json::json!({
            "kind": "types",
            "path": file.display().to_string(),
            "langversion": lang.to_string(),
            "supportCheck": true,
        })
        .to_string();
        let codes = file_codes(&parse("file-batch", &child.request(&request)));
        if codes.contains(&OUT_OF_SUPPORT) {
            refused.push(lang);
        }
    }
    let below: Vec<LanguageVersion> = LanguageVersion::ALL
        .into_iter()
        .filter(|&v| v < LanguageVersion::SUPPORTED_MIN)
        .collect();
    assert_eq!(
        refused,
        below,
        "FCS refuses (FS3880) exactly the versions on the left, and \
         `LanguageVersion::SUPPORTED_MIN` ({}) says the right: move the constant to FCS's floor \
         (the lowest version it checks without SKIP_VERSION_SUPPORTED_CHECK)",
        LanguageVersion::SUPPORTED_MIN,
    );
}

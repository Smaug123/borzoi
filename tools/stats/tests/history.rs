use std::fs;
use std::path::{Path, PathBuf};

use borzoi_stats::{RecordInput, build_site, record_observation};
use serde_json::{Value, json};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const CORPUS: &str = "c3c01c991d17643700d343cee5c5a1e20c06ce03";
const LOCK_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn record_is_path_stable_and_a_rerun_replaces_the_observation() {
    let temp = tempfile::tempdir().unwrap();
    let summary = write_summary(temp.path(), "parser-divergence", json!({ "mode": "all" }));
    let mut input = input(temp.path(), summary);

    let first = record_observation(&input).expect("record first observation");
    assert_eq!(
        first,
        temp.path()
            .join("history/observations/parser-divergence")
            .join("v1-c3c01c991d17-32becba0320d")
            .join(format!("{COMMIT}.json"))
    );
    let first_json: Value = serde_json::from_str(&fs::read_to_string(&first).unwrap()).unwrap();
    assert_eq!(first_json["observation_schema_version"], 1);
    assert_eq!(first_json["series"], "v1-c3c01c991d17-32becba0320d");
    assert_eq!(first_json["generator"]["statistics"]["matches"], 7);
    assert_eq!(
        first_json["workflow"]["url"],
        "https://github.com/Smaug123/borzoi/actions/runs/42"
    );

    input.run_attempt = 2;
    let second = record_observation(&input).expect("record rerun");
    assert_eq!(second, first);
    let second_json: Value = serde_json::from_str(&fs::read_to_string(&second).unwrap()).unwrap();
    assert_eq!(second_json["workflow"]["run_attempt"], 2);

    input.flake_lock_hash =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
    let different_toolchain = record_observation(&input).expect("record new toolchain series");
    assert_ne!(different_toolchain, first);
}

#[test]
fn record_rejects_unsafe_identity_and_unknown_generator_schema() {
    let temp = tempfile::tempdir().unwrap();
    let unsafe_summary = write_summary(temp.path(), "../parser", json!({}));
    let err = record_observation(&input(temp.path(), unsafe_summary))
        .unwrap_err()
        .to_string();
    assert!(err.contains("measurement"), "{err}");

    let path = temp.path().join("summary.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "schema_version": 2,
            "measurement": "parser-divergence",
            "configuration": {},
            "statistics": { "matches": 7 }
        }))
        .unwrap(),
    )
    .unwrap();
    let err = record_observation(&input(temp.path(), path))
        .unwrap_err()
        .to_string();
    assert!(err.contains("schema version 2"), "{err}");

    let summary = write_summary(temp.path(), "parser-divergence", json!({}));
    let mut malformed_time = input(temp.path(), summary);
    malformed_time.measured_at = "2026-07-19Té:00-".into();
    let err = record_observation(&malformed_time).unwrap_err().to_string();
    assert!(err.contains("ISO-8601"), "{err}");
}

#[test]
fn site_contains_every_valid_observation_and_rejects_misfiled_data() {
    let temp = tempfile::tempdir().unwrap();
    let first_summary = write_summary(temp.path(), "parser-divergence", json!({ "mode": "all" }));
    record_observation(&input(temp.path(), first_summary)).unwrap();

    let second_summary = write_summary(
        temp.path(),
        "resolution-divergence",
        json!({ "scope": "in-file", "stride": 13 }),
    );
    let mut second = input(temp.path(), second_summary);
    second.commit = "1123456789abcdef0123456789abcdef01234567".into();
    second.measured_at = "2026-07-20T11:00:00Z".into();
    record_observation(&second).unwrap();

    let output = temp.path().join("site");
    assert_eq!(
        build_site(&temp.path().join("history"), &output).unwrap(),
        2
    );
    let data: Value =
        serde_json::from_str(&fs::read_to_string(output.join("data.json")).unwrap()).unwrap();
    assert_eq!(data.as_array().unwrap().len(), 2);
    assert_eq!(data[0]["generator"]["measurement"], "parser-divergence");
    assert_eq!(data[1]["generator"]["measurement"], "resolution-divergence");
    let html = fs::read_to_string(output.join("index.html")).unwrap();
    assert!(html.contains("Borzoi measurements"));
    assert!(html.contains("data.json"));
    assert!(output.join(".nojekyll").is_file());

    let actual = record_observation(&input(
        temp.path(),
        write_summary(temp.path(), "typed-ast", json!({})),
    ))
    .unwrap();
    let wrong = temp
        .path()
        .join("history/observations/typed-ast/wrong/place.json");
    fs::create_dir_all(wrong.parent().unwrap()).unwrap();
    fs::rename(actual, wrong).unwrap();
    let err = build_site(&temp.path().join("history"), &output)
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match its contents"), "{err}");
}

fn input(root: &Path, summary: PathBuf) -> RecordInput {
    RecordInput {
        summary,
        history: root.join("history"),
        repository: "Smaug123/borzoi".into(),
        commit: COMMIT.into(),
        measured_at: "2026-07-19T10:00:00Z".into(),
        run_id: 42,
        run_attempt: 1,
        corpus_revision: CORPUS.into(),
        flake_lock_hash: LOCK_HASH.into(),
    }
}

fn write_summary(root: &Path, measurement: &str, configuration: Value) -> PathBuf {
    let path = root.join(format!("{measurement}-summary.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "measurement": measurement,
            "configuration": configuration,
            "statistics": { "matches": 7, "divergences": 1 }
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

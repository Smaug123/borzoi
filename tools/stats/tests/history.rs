use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
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
    assert_eq!(first_json["workflow"]["run_number"], 42);
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

    for measured_at in [
        "0000-01-01T00:00:00Z",
        "2026-00-01T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2025-02-29T00:00:00Z",
        "2026-01-01T24:00:00Z",
        "2026-01-01T00:60:00Z",
        "2026-01-01T00:00:60Z",
    ] {
        let summary = write_summary(temp.path(), "parser-divergence", json!({}));
        let mut invalid_time = input(temp.path(), summary);
        invalid_time.measured_at = measured_at.into();
        let err = record_observation(&invalid_time).unwrap_err().to_string();
        assert!(err.contains("ISO-8601"), "{measured_at}: {err}");
    }
    for measured_at in [
        "2000-02-29T00:00:00Z",
        "2024-02-29T23:59:59Z",
        "9999-12-31T23:59:59Z",
    ] {
        let summary = write_summary(temp.path(), "parser-divergence", json!({}));
        let mut valid_time = input(temp.path(), summary);
        valid_time.measured_at = measured_at.into();
        record_observation(&valid_time).expect("valid Gregorian UTC timestamp");
    }

    let array_summary = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "bins": [1, 2], "matches": 7 }),
    );
    let err = record_observation(&input(temp.path(), array_summary))
        .unwrap_err()
        .to_string();
    assert!(err.contains("arrays"), "{err}");

    let summary = write_summary(temp.path(), "parser-divergence", json!({}));
    let mut invalid_run_number = input(temp.path(), summary);
    invalid_run_number.run_number = 0;
    let err = record_observation(&invalid_run_number)
        .unwrap_err()
        .to_string();
    assert!(err.contains("run number"), "{err}");
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
    assert!(
        html.contains("unique([...items].reverse().map(item => item.series))"),
        "the first series option must be the most recently observed"
    );
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

#[test]
fn site_orders_observations_by_workflow_creation_not_completion_time() {
    let temp = tempfile::tempdir().unwrap();

    let legacy_summary = write_summary(temp.path(), "parser-divergence", json!({}));
    let mut legacy = input(temp.path(), legacy_summary);
    legacy.measured_at = "2026-07-22T10:00:00Z".into();
    let legacy_path = record_observation(&legacy).unwrap();
    let mut legacy_json: Value =
        serde_json::from_str(&fs::read_to_string(&legacy_path).unwrap()).unwrap();
    legacy_json["workflow"]
        .as_object_mut()
        .unwrap()
        .remove("run_number");
    fs::write(
        &legacy_path,
        serde_json::to_vec_pretty(&legacy_json).unwrap(),
    )
    .unwrap();

    let older_summary = write_summary(temp.path(), "parser-divergence", json!({}));
    let mut older = input(temp.path(), older_summary);
    older.commit = "1123456789abcdef0123456789abcdef01234567".into();
    older.measured_at = "2026-07-21T10:00:00Z".into();
    older.run_number = 43;
    record_observation(&older).unwrap();

    let newer_summary = write_summary(temp.path(), "parser-divergence", json!({}));
    let mut newer = input(temp.path(), newer_summary);
    newer.commit = "2123456789abcdef0123456789abcdef01234567".into();
    newer.measured_at = "2026-07-20T10:00:00Z".into();
    newer.run_number = 44;
    record_observation(&newer).unwrap();

    let output = temp.path().join("site");
    assert_eq!(
        build_site(&temp.path().join("history"), &output).unwrap(),
        3
    );
    let data: Value =
        serde_json::from_str(&fs::read_to_string(output.join("data.json")).unwrap()).unwrap();
    assert_eq!(data[0]["commit"], COMMIT);
    assert_eq!(data[1]["commit"], older.commit);
    assert_eq!(data[2]["commit"], newer.commit);

    let html = fs::read_to_string(output.join("index.html")).unwrap();
    assert!(
        !html.contains("Date.parse(point.item.measured_at)"),
        "chart coordinates must follow observation order"
    );
    assert!(html.contains("const x = index =>"));
}

#[cfg(unix)]
#[test]
fn record_rejects_symlinks_in_every_existing_output_component() {
    let series = "v1-c3c01c991d17-ee961db1637c";
    for component in [
        String::new(),
        "observations".into(),
        "observations/parser-divergence".into(),
        format!("observations/parser-divergence/{series}"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let history = temp.path().join("history");
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let link = if component.is_empty() {
            history.clone()
        } else {
            history.join(&component)
        };
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&outside, &link).unwrap();

        let summary = write_summary(temp.path(), "parser-divergence", json!({}));
        let err = record_observation(&input(temp.path(), summary))
            .unwrap_err()
            .to_string();
        assert!(err.contains("symlink"), "{component:?}: {err}");
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
    }

    let temp = tempfile::tempdir().unwrap();
    let history = temp.path().join("history");
    let observation = history
        .join("observations/parser-divergence")
        .join(series)
        .join(format!("{COMMIT}.json"));
    fs::create_dir_all(observation.parent().unwrap()).unwrap();
    let outside = temp.path().join("outside.json");
    fs::write(&outside, b"do not overwrite").unwrap();
    symlink(&outside, &observation).unwrap();

    let summary = write_summary(temp.path(), "parser-divergence", json!({}));
    let err = record_observation(&input(temp.path(), summary))
        .unwrap_err()
        .to_string();
    assert!(err.contains("symlink"), "{err}");
    assert_eq!(fs::read(&outside).unwrap(), b"do not overwrite");
}

#[cfg(unix)]
#[test]
fn site_rejects_a_symlinked_history_or_observations_root() {
    for link_history in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let history = temp.path().join("history");
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        if link_history {
            fs::create_dir(outside.join("observations")).unwrap();
            symlink(&outside, &history).unwrap();
        } else {
            fs::create_dir(&history).unwrap();
            symlink(&outside, history.join("observations")).unwrap();
        }

        let err = build_site(&history, &temp.path().join("site"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("symlink"),
            "link_history={link_history}: {err}"
        );
    }
}

fn input(root: &Path, summary: PathBuf) -> RecordInput {
    RecordInput {
        summary,
        history: root.join("history"),
        repository: "Smaug123/borzoi".into(),
        commit: COMMIT.into(),
        measured_at: "2026-07-19T10:00:00Z".into(),
        run_id: 42,
        run_number: 42,
        run_attempt: 1,
        corpus_revision: CORPUS.into(),
        flake_lock_hash: LOCK_HASH.into(),
    }
}

fn write_summary(root: &Path, measurement: &str, configuration: Value) -> PathBuf {
    write_summary_with_statistics(
        root,
        measurement,
        configuration,
        json!({ "matches": 7, "divergences": 1 }),
    )
}

fn write_summary_with_statistics(
    root: &Path,
    measurement: &str,
    configuration: Value,
    statistics: Value,
) -> PathBuf {
    let path = root.join(format!("{measurement}-summary.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "measurement": measurement,
            "configuration": configuration,
            "statistics": statistics
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

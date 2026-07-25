use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use borzoi_stats::{RecordInput, build_site, read_project_corpus, record_observation};
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

/// A second corpus files, plots and validates alongside the F# one, and its
/// source is carried through to the observation. The dashboard separates the
/// two by measurement and series, so nothing has to know the source exists.
#[test]
fn a_measurement_over_another_corpus_records_and_validates() {
    let temp = tempfile::tempdir().unwrap();
    let summary = write_summary(temp.path(), "project-corpus-diff", json!({ "stride": 1 }));
    let mut input = input(temp.path(), summary);
    input.corpus_source = "Smaug123/borzoi-project-corpus".into();
    input.corpus_revision = "1111111111111111111111111111111111111111".into();

    let recorded = record_observation(&input).expect("record project-corpus observation");
    let json: Value = serde_json::from_str(&fs::read_to_string(&recorded).unwrap()).unwrap();
    assert_eq!(json["corpus"]["source"], "Smaug123/borzoi-project-corpus");
    assert_eq!(
        json["corpus"]["revision"],
        "1111111111111111111111111111111111111111"
    );

    // The F# measurements still record, and `site` validates the mixed history.
    let fsharp = write_summary(temp.path(), "parser-divergence", json!({}));
    record_observation(&self::input(temp.path(), fsharp)).expect("record fsharp observation");
    let count = build_site(&temp.path().join("history"), &temp.path().join("site"))
        .expect("site over a mixed-corpus history");
    assert_eq!(count, 2);

    let mut unsafe_source = input.clone();
    unsafe_source.corpus_source = "../escape".into();
    let err = record_observation(&unsafe_source).unwrap_err().to_string();
    assert!(err.contains("corpus source"), "{err}");
}

#[test]
fn the_corpus_digest_covers_every_pin_but_not_their_order() {
    let temp = tempfile::tempdir().unwrap();
    let one = pin("Smaug123/A", &"a".repeat(40), "A/A.fsproj");
    let two = pin("Smaug123/B", &"b".repeat(40), "B/B.fsproj");

    let forwards = read_project_corpus(&write_corpus(
        temp.path(),
        json!({ "schema_version": 1, "projects": [one.clone(), two.clone()] }),
    ))
    .expect("valid corpus");
    let backwards = read_project_corpus(&write_corpus(
        temp.path(),
        json!({ "schema_version": 1, "projects": [two.clone(), one.clone()] }),
    ))
    .expect("valid corpus");
    let digest = forwards.revision();
    assert_eq!(digest.len(), 40, "{digest}");
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(digest, backwards.revision());

    // Every field is part of the identity: a bumped revision, a different
    // project in the same repository, and an added or dropped pin must each
    // start a new series rather than silently joining the old one.
    for changed in [
        json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"c".repeat(40), "A/A.fsproj"), two.clone()] }),
        json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"a".repeat(40), "A/Other.fsproj"), two.clone()] }),
        json!({ "schema_version": 1, "projects": [one.clone()] }),
        json!({ "schema_version": 1, "projects": [one.clone(), two.clone(), pin("Smaug123/C", &"c".repeat(40), "C/C.fsproj")] }),
    ] {
        let other = read_project_corpus(&write_corpus(temp.path(), changed)).expect("valid corpus");
        assert_ne!(digest, other.revision());
    }

    // The digest is what `record` files the observation under, so it has to be
    // accepted as a corpus revision.
    let summary = write_summary(temp.path(), "project-corpus-diff", json!({}));
    let mut input = input(temp.path(), summary);
    input.corpus_source = "Smaug123/borzoi-project-corpus".into();
    input.corpus_revision = digest;
    record_observation(&input).expect("the corpus digest is a valid corpus revision");
}

#[test]
fn the_corpus_rejects_pins_it_cannot_check_out_or_would_double_count() {
    let temp = tempfile::tempdir().unwrap();
    let good = pin("Smaug123/A", &"a".repeat(40), "A/A.fsproj");
    for (expected, corpus) in [
        (
            "schema version",
            json!({ "schema_version": 2, "projects": [good.clone()] }),
        ),
        (
            "at least one",
            json!({ "schema_version": 1, "projects": [] }),
        ),
        (
            "repository",
            json!({ "schema_version": 1, "projects": [pin("no-owner", &"a".repeat(40), "A/A.fsproj")] }),
        ),
        (
            "revision",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A", "main", "A/A.fsproj")] }),
        ),
        (
            "project",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"a".repeat(40), "../escape/A.fsproj")] }),
        ),
        (
            "project",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"a".repeat(40), "/abs/A.fsproj")] }),
        ),
        (
            "project",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"a".repeat(40), "A/A.csproj")] }),
        ),
        (
            "project",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"a".repeat(40), "A dir/A.fsproj")] }),
        ),
        (
            "two revisions",
            json!({ "schema_version": 1, "projects": [good.clone(), pin("Smaug123/A", &"b".repeat(40), "A/Other.fsproj")] }),
        ),
        (
            "more than once",
            json!({ "schema_version": 1, "projects": [good.clone(), good.clone()] }),
        ),
    ] {
        let path = write_corpus(temp.path(), corpus);
        let err = read_project_corpus(&path).unwrap_err().to_string();
        assert!(err.contains(expected), "expected {expected:?}, got {err}");
    }

    // One repository contributing *several* projects is legitimate and must be
    // accepted — it is the shape a multi-project repository takes. The
    // workflow's checkout loop reads one line per project and so must clone at
    // most once per repository; this is the case that makes that necessary,
    // and rejecting it here would hide the requirement rather than meet it.
    let two_from_one = write_corpus(
        temp.path(),
        json!({ "schema_version": 1, "projects": [
            good.clone(),
            pin("Smaug123/A", &"a".repeat(40), "Other/Other.fsproj"),
        ] }),
    );
    let corpus =
        read_project_corpus(&two_from_one).expect("one repository may pin several projects");
    assert_eq!(corpus.projects.len(), 2);
    // Exactly one checkout is needed, at exactly one revision.
    let revisions: std::collections::BTreeSet<&str> = corpus
        .projects
        .iter()
        .map(|pin| pin.revision.as_str())
        .collect();
    assert_eq!(revisions.len(), 1);
}

/// The corpus the workflow actually checks out. A typo here fails the
/// measurement job several minutes into a clone, so parse it in-process.
#[test]
fn the_checked_in_project_corpus_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../nix/project-corpus.json");
    let corpus = read_project_corpus(&path).expect("nix/project-corpus.json is a valid corpus");
    assert!(
        corpus.projects.len() >= 2,
        "a one-project corpus cannot exercise cross-project references"
    );
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
        corpus_source: borzoi_stats::FSHARP_CORPUS_SOURCE.into(),
        corpus_revision: CORPUS.into(),
        flake_lock_hash: LOCK_HASH.into(),
    }
}

fn write_corpus(root: &Path, value: Value) -> PathBuf {
    let path = root.join("project-corpus.json");
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    path
}

fn pin(repository: &str, revision: &str, project: &str) -> Value {
    json!({ "repository": repository, "revision": revision, "project": project })
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

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

    // Re-spelling a pin without changing what it points at must leave the
    // series intact. A repository name re-cased checks out identical code, so
    // a digest that moved would restart the dashboard's trend for nothing.
    let recased = read_project_corpus(&write_corpus(
        temp.path(),
        json!({ "schema_version": 1, "projects": [
            pin("smaug123/a", &"a".repeat(40), "A/A.fsproj"),
            two.clone(),
        ] }),
    ))
    .expect("valid corpus");
    assert_eq!(digest, recased.revision());

    // The project path is *not* case-folded: it names a file on a
    // case-sensitive filesystem, where these really are different files.
    let repathed = read_project_corpus(&write_corpus(
        temp.path(),
        json!({ "schema_version": 1, "projects": [
            pin("Smaug123/A", &"a".repeat(40), "A/a.fsproj"),
            two.clone(),
        ] }),
    ))
    .expect("valid corpus");
    assert_ne!(digest, repathed.revision());

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
        // A second spelling of one path would pass the literal duplicate check
        // below and be measured twice, doubling that project's contribution to
        // every count in the series.
        (
            "project",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"a".repeat(40), "A/./A.fsproj")] }),
        ),
        (
            "project",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"a".repeat(40), "./A/A.fsproj")] }),
        ),
        // The workflow joins these with the path-list separator and the runner
        // splits them again, so a separator inside a pin arrives as two
        // fragments that name nothing.
        (
            "project",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"a".repeat(40), "A:B/A.fsproj")] }),
        ),
        (
            "project",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"a".repeat(40), "A;B/A.fsproj")] }),
        ),
        // `owner/repo` and `owner/repo.git` clone one repository under two
        // names, so both pins survive and the project is measured twice.
        (
            ".git",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A.git", &"a".repeat(40), "A/A.fsproj")] }),
        ),
        // GitHub resolves repository names case-insensitively, so these are
        // one repository; a case-sensitive Linux filesystem would keep two
        // checkouts of it and each project would be counted twice. Distinct
        // projects, so this reaches the repository-spelling check rather than
        // the duplicate-project one below.
        (
            "two ways",
            json!({ "schema_version": 1, "projects": [good.clone(), pin("smaug123/a", &"a".repeat(40), "Other/Other.fsproj")] }),
        ),
        // The same alias with one project reaches the duplicate check instead;
        // either way it must not survive to be measured twice.
        (
            "more than once",
            json!({ "schema_version": 1, "projects": [good.clone(), pin("smaug123/a", &"a".repeat(40), "A/A.fsproj")] }),
        ),
        // Git resolves an uppercase object ID to the same commit, so this
        // pins identical code under a spelling that would hash differently.
        (
            "lowercase",
            json!({ "schema_version": 1, "projects": [pin("Smaug123/A", &"A".repeat(40), "A/A.fsproj")] }),
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

/// A metric that stops being emitted is indistinguishable, on the dashboard,
/// from a metric a run failed to measure: both leave the previous point reading
/// as "Latest". The generator's within-run exhaustiveness cannot see this — the
/// key is gone from the type, not merely absent from one run — so the recorder
/// compares each observation against the one it follows and makes the deliberate
/// case say so.
#[test]
fn a_metric_that_vanishes_must_be_declared_retired() {
    let temp = tempfile::tempdir().unwrap();
    let first = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "census": { "occupied": 3, "kept": 1 } }),
    );
    record_observation(&input(temp.path(), first)).expect("record the first observation");

    let dropped = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "census": { "kept": 1 } }),
    );
    let mut second = input(temp.path(), dropped);
    second.commit = "1123456789abcdef0123456789abcdef01234567".into();
    second.run_number = 43;
    let err = record_observation(&second).unwrap_err().to_string();
    assert!(err.contains("census.occupied"), "{err}");
    assert!(err.contains("retired_statistics"), "{err}");

    let declared = write_full_summary(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "census": { "kept": 1 } }),
        json!(["census.occupied"]),
    );
    let mut declared_input = second.clone();
    declared_input.summary = declared;
    record_observation(&declared_input).expect("a declared retirement records");

    // A declaration that is left in place stays valid once the metric is long
    // gone, which is what makes "leave it there" safe advice: the run that
    // publishes the transition can fail, and a later observation that dropped
    // the marker would then find a predecessor still carrying the metric.
    let still_declared = write_full_summary(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "census": { "kept": 1 } }),
        json!(["census.occupied"]),
    );
    let mut third = input(temp.path(), still_declared);
    third.commit = "2123456789abcdef0123456789abcdef01234567".into();
    third.run_number = 44;
    record_observation(&third).expect("a declaration outlives the transition it was written for");

    // And the transition observation going missing entirely is the case that
    // advice exists for: this observation's predecessor is the one from before
    // the retirement, so without the marker it is refused.
    let temp = tempfile::tempdir().unwrap();
    let before = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "census": { "occupied": 3, "kept": 1 } }),
    );
    record_observation(&input(temp.path(), before)).unwrap();
    let undeclared = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "census": { "kept": 1 } }),
    );
    let mut later = input(temp.path(), undeclared);
    later.commit = "3123456789abcdef0123456789abcdef01234567".into();
    later.run_number = 60;
    let err = record_observation(&later).unwrap_err().to_string();
    assert!(err.contains("census.occupied"), "{err}");
}

/// The whole point of a retirement marker rather than a series split: the
/// metrics that *are* still comparable must keep their history. A digest that
/// moved would discard every other metric's trend to explain one dead key.
#[test]
fn retiring_a_metric_does_not_start_a_new_series() {
    let temp = tempfile::tempdir().unwrap();
    let first = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({ "mode": "all" }),
        json!({ "matches": 7, "occupied": 3 }),
    );
    let first_path = record_observation(&input(temp.path(), first)).unwrap();

    let declared = write_full_summary(
        temp.path(),
        "parser-divergence",
        json!({ "mode": "all" }),
        json!({ "matches": 7 }),
        json!(["occupied"]),
    );
    let mut second = input(temp.path(), declared);
    second.commit = "1123456789abcdef0123456789abcdef01234567".into();
    second.run_number = 43;
    let second_path = record_observation(&second).unwrap();

    assert_eq!(
        first_path.parent().unwrap(),
        second_path.parent().unwrap(),
        "a retirement must leave the series it was measured in intact"
    );
}

/// A retirement is a claim that the key is gone. Emitting it anyway would make
/// the declaration a licence to drop the key at any later run without notice.
#[test]
fn a_retirement_must_be_well_formed_and_not_contradict_the_statistics() {
    let temp = tempfile::tempdir().unwrap();
    for (expected, retired, statistics) in [
        (
            "also emits",
            json!(["matches"]),
            json!({ "matches": 7, "divergences": 1 }),
        ),
        (
            "also emits",
            json!(["census.kept"]),
            json!({ "census": { "kept": 1 } }),
        ),
        ("metric path", json!([""]), json!({ "matches": 7 })),
        ("metric path", json!(["census."]), json!({ "matches": 7 })),
        ("metric path", json!([".census"]), json!({ "matches": 7 })),
        ("metric path", json!(["a..b"]), json!({ "matches": 7 })),
        (
            "more than once",
            json!(["gone", "gone"]),
            json!({ "matches": 7 }),
        ),
        // A statistics key has to stand as one segment of a metric path, or the
        // metric it names cannot be written in a retirement — and an
        // observation that records a metric nobody can retire wedges the
        // series the moment that metric is dropped.
        //
        // A dot is how a path spells nesting, so this names the same metric as
        // `{"census": {"kept": 1}}`; re-nesting it would be invisible to the
        // drop check while every reader sees a new key.
        ("name a metric", json!([]), json!({ "census.kept": 1 })),
        (
            "name a metric",
            json!([]),
            json!({ "census": { "by.cause": 1 } }),
        ),
        // An empty key produces the path `""` at the root and `census.` when
        // nested. Neither is a metric path, so neither can be retired.
        ("name a metric", json!([]), json!({ "": 1 })),
        ("name a metric", json!([]), json!({ "census": { "": 1 } })),
    ] {
        let summary = write_full_summary(
            temp.path(),
            "parser-divergence",
            json!({}),
            statistics,
            retired,
        );
        let err = record_observation(&input(temp.path(), summary))
            .unwrap_err()
            .to_string();
        assert!(err.contains(expected), "expected {expected:?}, got {err}");
    }
}

/// The dashboard plots one point per nested *number* and skips a `null` exactly
/// as it skips an absent key, so a field that starts serialising as `null` has
/// dropped its metric. The recorder reads the statistics the same way, which is
/// what makes this check the across-run half of "every key, every run".
#[test]
fn a_metric_that_becomes_null_has_dropped() {
    let temp = tempfile::tempdir().unwrap();
    let first = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "ratio_basis_points": 4200 }),
    );
    record_observation(&input(temp.path(), first)).unwrap();

    let nulled = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "ratio_basis_points": Value::Null }),
    );
    let mut second = input(temp.path(), nulled);
    second.commit = "1123456789abcdef0123456789abcdef01234567".into();
    second.run_number = 43;
    let err = record_observation(&second).unwrap_err().to_string();
    assert!(err.contains("ratio_basis_points"), "{err}");
}

/// Runs finish out of order — the doc's ordering rules exist because they do —
/// so the comparison is against the observation this one will *follow* on the
/// dashboard, never against whatever happens to be newest. Comparing against
/// the newest would refuse an older observation for lacking a metric that was
/// introduced after it, which is not a drop at all.
#[test]
fn the_drop_check_compares_against_the_observation_it_follows() {
    let temp = tempfile::tempdir().unwrap();
    let oldest = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7 }),
    );
    let mut first = input(temp.path(), oldest);
    first.run_number = 10;
    record_observation(&first).unwrap();

    let widened = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "introduced_later": 1 }),
    );
    let mut newest = input(temp.path(), widened);
    newest.commit = "1123456789abcdef0123456789abcdef01234567".into();
    newest.run_number = 30;
    record_observation(&newest).expect("a widened metric namespace records");

    // A slow run from between the two arrives last. It never carried
    // `introduced_later`, and its own predecessor did not either.
    let late = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7 }),
    );
    let mut middle = input(temp.path(), late);
    middle.commit = "2123456789abcdef0123456789abcdef01234567".into();
    middle.run_number = 20;
    record_observation(&middle).expect("a late-arriving older observation is not a drop");

    // A rerun replaces its own observation, so it must not be compared against
    // itself — nor against anything newer than itself.
    let rerun = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7 }),
    );
    let mut again = middle.clone();
    again.summary = rerun;
    again.run_attempt = 2;
    record_observation(&again).expect("a rerun compares against its own predecessor");
}

/// The check's residual, pinned deliberately rather than left to be
/// rediscovered as a bug.
///
/// Each observation is compared against the greatest *already recorded* one
/// below it, so a drop escapes when the observations carrying the metric are
/// recorded after the drop itself — and once the first post-drop observation
/// escapes, every later one does too, because each is then compared against an
/// already-gapped predecessor. Here `temporary` is carried by runs 10 and 15
/// and gone from 20 onwards, but the runs are recorded 25, 20, 10, 15: nothing
/// below 25 or 20 exists when they land, and 15's predecessor is 10, which
/// still carries it. The 15 → 20 pair is never anyone's predecessor pair.
///
/// It is tempting to close this by checking the *successor* too, and that is
/// the trap: the observation such a check would refuse is the innocent one. In
/// the mirror case — a metric added and removed within two commits, then the
/// adding run lands late — the late run emitted a strict superset of both its
/// neighbours, and the drop it would be refused for belongs to an observation
/// that is already published and immutable. There would be nothing anyone could
/// change to discharge the refusal, so that observation would be permanently
/// unpublishable and the run permanently red.
///
/// Accepting the gap costs nothing a reader can see, because the dashboard
/// decides retirement from presence, not from the declaration: a metric absent
/// from the newest observation is labelled retired whether or not anyone said
/// so. What escapes is the record of intent, and the chance to notice a
/// generator regression at the moment it happened — not the reading.
#[test]
fn a_drop_escapes_when_the_runs_carrying_it_are_recorded_after_it() {
    let temp = tempfile::tempdir().unwrap();
    let commits = [
        "0123456789abcdef0123456789abcdef01234567",
        "1123456789abcdef0123456789abcdef01234567",
        "2123456789abcdef0123456789abcdef01234567",
        "3123456789abcdef0123456789abcdef01234567",
    ];
    // (run number, commit, does it carry `temporary`?), in recording order.
    for (run_number, commit, carries) in [
        (25, commits[3], false),
        (20, commits[2], false),
        (10, commits[0], true),
        (15, commits[1], true),
    ] {
        let statistics = if carries {
            json!({ "matches": 7, "temporary": 1 })
        } else {
            json!({ "matches": 7 })
        };
        let summary =
            write_summary_with_statistics(temp.path(), "parser-divergence", json!({}), statistics);
        let mut observation = input(temp.path(), summary);
        observation.commit = commit.into();
        observation.run_number = run_number;
        record_observation(&observation)
            .unwrap_or_else(|error| panic!("run {run_number} records: {error}"));
    }

    let output = temp.path().join("site");
    build_site(&temp.path().join("history"), &output).expect("the history is well formed");
    let data: Value =
        serde_json::from_str(&fs::read_to_string(output.join("data.json")).unwrap()).unwrap();
    let observations = data.as_array().unwrap();
    assert_eq!(observations.len(), 4);
    // The dashboard's ordering restores the true one, so the unchecked pair is
    // adjacent in the rendered history …
    let order: Vec<&str> = observations
        .iter()
        .map(|item| item["commit"].as_str().unwrap())
        .collect();
    assert_eq!(order, commits);
    // … and the newest observation does not carry the metric, which is what
    // makes the dashboard label it retired without needing the declaration.
    assert!(
        observations
            .last()
            .unwrap()
            .pointer("/generator/statistics/temporary")
            .is_none()
    );
}

/// A rerun overwrites its own observation, so a metric the first attempt
/// measured and the second does not is deleted outright — the only point
/// carrying it is gone, and no predecessor comparison would ever notice.
///
/// It is also the sharpest determinism check there is. A rerun measures the
/// same commit with the same generator over the same corpus, so it is the one
/// place two observations of *identical* code meet. A key set that differs
/// between them is not a retirement — nothing changed to retire — but a metric
/// namespace that depends on something other than the code, which is exactly
/// the sparse-map breach the contract forbids.
#[test]
fn a_rerun_may_not_quietly_drop_what_its_earlier_attempt_measured() {
    let temp = tempfile::tempdir().unwrap();
    // A chronological predecessor that never carried the metric, so only the
    // observation being replaced can catch its loss.
    let earlier = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7 }),
    );
    let mut first = input(temp.path(), earlier);
    first.run_number = 10;
    record_observation(&first).unwrap();

    let attempt_one = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "seen_once": 1 }),
    );
    let mut rerun = input(temp.path(), attempt_one);
    rerun.commit = "1123456789abcdef0123456789abcdef01234567".into();
    rerun.run_number = 20;
    record_observation(&rerun).expect("the first attempt records");

    let attempt_two = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7 }),
    );
    let mut second_attempt = rerun.clone();
    second_attempt.summary = attempt_two;
    second_attempt.run_attempt = 2;
    let err = record_observation(&second_attempt).unwrap_err().to_string();
    assert!(err.contains("seen_once"), "{err}");
    assert!(err.contains("attempt"), "{err}");

    // A retirement cannot excuse it. Retiring is a claim that the *code* no
    // longer emits the metric, and the code did not change between attempts of
    // one run — so a declaration here would be false, and honouring it would
    // make the marker a way to delete a recorded point.
    let declared = write_full_summary(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7 }),
        json!(["seen_once"]),
    );
    let mut with_declaration = second_attempt.clone();
    with_declaration.summary = declared;
    let err = record_observation(&with_declaration)
        .unwrap_err()
        .to_string();
    assert!(err.contains("seen_once"), "{err}");

    // The other direction is the same fault. A metric appearing only on the
    // second attempt is a namespace that depends on something other than the
    // code just as surely as one that disappears.
    let widened = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "seen_once": 1, "appeared_late": 1 }),
    );
    let mut wider = second_attempt.clone();
    wider.summary = widened;
    let err = record_observation(&wider).unwrap_err().to_string();
    assert!(err.contains("appeared_late"), "{err}");

    // The refused rerun leaves the recorded attempt untouched.
    let stored: Value = serde_json::from_str(
        &fs::read_to_string(
            temp.path()
                .join("history/observations/parser-divergence")
                .join("v1-c3c01c991d17-ee961db1637c")
                .join(format!("{}.json", rerun.commit)),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(stored["generator"]["statistics"]["seen_once"], 1);

    // Re-recording identical statistics is the ordinary rerun, and must pass.
    let identical = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({}),
        json!({ "matches": 7, "seen_once": 1 }),
    );
    let mut faithful = second_attempt.clone();
    faithful.summary = identical;
    record_observation(&faithful).expect("a rerun that measures the same thing records");
}

/// What the check guarantees, established by enumeration rather than by
/// argument.
///
/// The residual above is a statement about *arrival orders*, and arrival order
/// is exactly what a replay of the real history cannot vary — it only ever
/// exercises the one order that happened. So every order of one small series is
/// run instead, and the boundary between caught and escaped is asserted
/// directly: whenever any observation carrying the metric is recorded before
/// the first one that drops it, the drop is refused. That is the guarantee a
/// live series relies on, since its whole prefix is published long before a new
/// commit's run records.
///
/// The escape count is pinned too. It is not a target — it is the size of the
/// accepted gap, and a change that moves it has changed what the recorder
/// claims, whichever direction it moved in.
#[test]
fn every_arrival_order_that_records_a_carrier_first_catches_the_drop() {
    const CARRIERS: [u64; 2] = [10, 15];
    let commits = [
        "0123456789abcdef0123456789abcdef01234567",
        "1123456789abcdef0123456789abcdef01234567",
        "2123456789abcdef0123456789abcdef01234567",
        "3123456789abcdef0123456789abcdef01234567",
    ];
    let runs = [10_u64, 15, 20, 25];

    let mut escaped = Vec::new();
    for order in permutations(&[0, 1, 2, 3]) {
        let temp = tempfile::tempdir().unwrap();
        let mut refused = false;
        for &index in &order {
            let carries = CARRIERS.contains(&runs[index]);
            let statistics = if carries {
                json!({ "matches": 7, "temporary": 1 })
            } else {
                json!({ "matches": 7 })
            };
            let summary = write_summary_with_statistics(
                temp.path(),
                "parser-divergence",
                json!({}),
                statistics,
            );
            let mut observation = input(temp.path(), summary);
            observation.commit = commits[index].into();
            observation.run_number = runs[index];
            if record_observation(&observation).is_err() {
                refused = true;
            }
        }
        // Did any observation carrying the metric reach the history before the
        // first one that lacks it?
        let first_dropper = order
            .iter()
            .position(|&index| !CARRIERS.contains(&runs[index]))
            .expect("the series contains a dropping observation");
        let carrier_first = order[..first_dropper]
            .iter()
            .any(|&index| CARRIERS.contains(&runs[index]));

        if carrier_first {
            assert!(
                refused,
                "a drop recorded after a carrier must be refused: {:?}",
                order.iter().map(|&index| runs[index]).collect::<Vec<_>>()
            );
        } else if !refused {
            escaped.push(order.iter().map(|&index| runs[index]).collect::<Vec<_>>());
        }
    }

    assert_eq!(
        escaped.len(),
        8,
        "the accepted gap changed size; escapes were {escaped:?}"
    );
    // Every escape starts with a dropping observation, which is the shape the
    // residual describes: nothing carrying the metric is on disk yet.
    assert!(
        escaped.iter().all(|order| !CARRIERS.contains(&order[0])),
        "{escaped:?}"
    );
}

fn permutations(items: &[usize]) -> Vec<Vec<usize>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut output = Vec::new();
    for (index, &item) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(index);
        for mut tail in permutations(&rest) {
            tail.insert(0, item);
            output.push(tail);
        }
    }
    output
}

/// A configuration change starts a new series, and a new series has no
/// predecessor to have dropped anything: the old points are not comparable, so
/// nothing about them constrains the shape of the new ones.
#[test]
fn a_new_series_carries_no_obligation_from_the_old_one() {
    let temp = tempfile::tempdir().unwrap();
    let first = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({ "mode": "all" }),
        json!({ "matches": 7, "occupied": 3 }),
    );
    record_observation(&input(temp.path(), first)).unwrap();

    let reconfigured = write_summary_with_statistics(
        temp.path(),
        "parser-divergence",
        json!({ "mode": "sampled" }),
        json!({ "matches": 7 }),
    );
    let mut second = input(temp.path(), reconfigured);
    second.commit = "1123456789abcdef0123456789abcdef01234567".into();
    second.run_number = 43;
    record_observation(&second).expect("a new series starts clean");
}

/// The rendering half. A retired metric still has to be *readable* as retired:
/// its last point is not the latest measurement, and a card that says "Latest"
/// over a value from ten commits ago is the same lie the recorder now refuses
/// to create. Liveness is decided by the newest observation of the selected
/// series, which is where the metric namespace currently in force lives.
#[test]
fn the_dashboard_reads_liveness_off_the_newest_observation() {
    let temp = tempfile::tempdir().unwrap();
    let summary = write_summary(temp.path(), "parser-divergence", json!({}));
    record_observation(&input(temp.path(), summary)).unwrap();
    let output = temp.path().join("site");
    build_site(&temp.path().join("history"), &output).unwrap();

    let html = fs::read_to_string(output.join("index.html")).unwrap();
    assert!(
        html.contains("items.at(-1).generator.statistics"),
        "liveness must be read off the newest observation of the series"
    );
    assert!(
        html.contains("retired"),
        "a retired metric must be labelled"
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
    write_full_summary(root, measurement, configuration, statistics, json!([]))
}

fn write_full_summary(
    root: &Path,
    measurement: &str,
    configuration: Value,
    statistics: Value,
    retired: Value,
) -> PathBuf {
    let path = root.join(format!("{measurement}-summary.json"));
    let mut summary = json!({
        "schema_version": 1,
        "measurement": measurement,
        "configuration": configuration,
        "statistics": statistics
    });
    if retired != json!([]) {
        summary
            .as_object_mut()
            .unwrap()
            .insert("retired_statistics".into(), retired);
    }
    fs::write(&path, serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
    path
}

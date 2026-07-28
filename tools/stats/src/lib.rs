//! Durable observations and the static dashboard for Borzoi's corpus measurements.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha1::{Digest, Sha1};

const OBSERVATION_SCHEMA_VERSION: u32 = 1;
const GENERATOR_SCHEMA_VERSION: u32 = 1;
const PROJECT_CORPUS_SCHEMA_VERSION: u32 = 1;
/// The pinned F# compiler source tree the per-file sweeps walk, identified by
/// its own commit. Also [`RecordInput`]'s default corpus source.
pub const FSHARP_CORPUS_SOURCE: &str = "dotnet/fsharp";
const INDEX_HTML: &str = include_str!("site/index.html");

#[derive(Debug, Clone)]
pub struct RecordInput {
    pub summary: PathBuf,
    pub history: PathBuf,
    pub repository: String,
    pub commit: String,
    pub measured_at: String,
    pub run_id: u64,
    pub run_number: u64,
    pub run_attempt: u32,
    /// Which corpus this measurement walked, as `OWNER/NAME`. A measurement
    /// walks one corpus, and the series key already separates measurements, so
    /// the source itself does not enter the series digest — the revision does.
    pub corpus_source: String,
    pub corpus_revision: String,
    pub flake_lock_hash: String,
}

#[derive(Debug)]
pub enum StatsError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    Invalid(String),
}

impl fmt::Display for StatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation} {}: {source}", path.display()),
            Self::Json { path, source } => write!(f, "parse {}: {source}", path.display()),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for StatsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Invalid(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratorSummary {
    schema_version: u32,
    measurement: String,
    configuration: Value,
    statistics: Value,
    /// The metric paths this generator deliberately stopped emitting, in the
    /// dotted spelling the dashboard names them by.
    ///
    /// This is how the contract says "the metric namespace evolved". Without
    /// it, a key that disappears because it was renamed and a key that
    /// disappears because a generator regressed are the same event to every
    /// reader: see [`metrics_that_vanished`].
    ///
    /// It is deliberately *not* part of [`series_key`]. Retiring one metric
    /// must not restart the trend of the metrics beside it, which are still
    /// measuring exactly what they measured before — that is the whole reason
    /// this exists rather than a schema bump.
    ///
    /// A declaration is needed exactly once, in the observation where the key
    /// first goes missing; by the next run the predecessor already lacks it, so
    /// leaving the entry in place is harmless and dropping it costs nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired_statistics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    observation_schema_version: u32,
    series: String,
    repository: String,
    commit: String,
    measured_at: String,
    workflow: Workflow,
    corpus: Corpus,
    flake_lock_hash: String,
    generator: GeneratorSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Workflow {
    run_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run_number: Option<u64>,
    run_attempt: u32,
    url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    source: String,
    revision: String,
}

/// Validate a generator summary, wrap it in reproducibility metadata, and write
/// it to the one path determined by its measurement, series, and commit.
pub fn record_observation(input: &RecordInput) -> Result<PathBuf, StatsError> {
    validate_record_input(input)?;
    let generator: GeneratorSummary = read_json(&input.summary)?;
    validate_generator(&generator)?;

    let series = series_key(&generator, &input.corpus_revision, &input.flake_lock_hash)?;
    let observation = Observation {
        observation_schema_version: OBSERVATION_SCHEMA_VERSION,
        series: series.clone(),
        repository: input.repository.clone(),
        commit: input.commit.clone(),
        measured_at: input.measured_at.clone(),
        workflow: Workflow {
            run_id: input.run_id,
            run_number: Some(input.run_number),
            run_attempt: input.run_attempt,
            url: format!(
                "https://github.com/{}/actions/runs/{}",
                input.repository, input.run_id
            ),
        },
        corpus: Corpus {
            source: input.corpus_source.clone(),
            revision: input.corpus_revision.clone(),
        },
        flake_lock_hash: input.flake_lock_hash.clone(),
        generator,
    };
    let path = observation_path(&input.history, &observation);
    reject_symlinked_components(&input.history, &path)?;
    let parent = path.parent().expect("observation path has a parent");
    if let Some(previous) = recorded_predecessor(parent, &observation)? {
        let vanished = metrics_that_vanished(&previous.generator, &observation.generator);
        if !vanished.is_empty() {
            return invalid(format!(
                "observation for {} drops {} that {} measured: {}. The dashboard would keep \
                 offering each of them with the older commit's value reading as \"Latest\", which \
                 is indistinguishable from a run that failed to measure them. If the generator \
                 meant to stop emitting them, list them in its `retired_statistics`",
                observation.commit,
                if vanished.len() == 1 {
                    "the metric"
                } else {
                    "the metrics"
                },
                previous.commit,
                vanished.join(", ")
            ));
        }
    }
    create_dir_all(parent)?;
    write_json(&path, &observation)?;
    Ok(path)
}

/// Validate the complete current-tree history and build a self-contained Pages
/// directory. The deployed site is disposable; `history` remains authoritative.
pub fn build_site(history: &Path, output: &Path) -> Result<usize, StatsError> {
    let root = history.join("observations");
    reject_symlinked_components(history, &root)?;
    let mut paths = Vec::new();
    collect_json_files(&root, &mut paths)?;
    paths.sort();

    let mut observations = Vec::with_capacity(paths.len());
    for path in paths {
        let observation: Observation = read_json(&path)?;
        validate_observation(&observation)?;
        let expected = observation_path(history, &observation);
        if path != expected {
            return Err(StatsError::Invalid(format!(
                "observation {} does not match its contents; expected {}",
                path.display(),
                expected.display()
            )));
        }
        observations.push(observation);
    }
    observations.sort_by(observation_order);

    create_dir_all(output)?;
    write_json(&output.join("data.json"), &observations)?;
    write_file(&output.join("index.html"), INDEX_HTML.as_bytes())?;
    write_file(&output.join(".nojekyll"), b"")?;
    Ok(observations.len())
}

/// One project of the pinned project corpus: a repository at an exact
/// revision, and the `.fsproj` within it to measure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedProject {
    /// `OWNER/REPO` on GitHub.
    pub repository: String,
    /// The exact commit to check out. A branch or tag would let the corpus
    /// drift under a series that claims to be comparable.
    pub revision: String,
    /// The `.fsproj` to measure, relative to the repository root.
    pub project: String,
}

/// The pinned corpus of real F# projects the project-resolution differential
/// measures. Read from `nix/project-corpus.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCorpus {
    pub schema_version: u32,
    pub projects: Vec<PinnedProject>,
}

impl ProjectCorpus {
    /// The corpus revision to record this corpus under: a digest over every
    /// pin, so a bumped revision, an added project, or a dropped one all start
    /// a new comparable series. Order in the file is not part of the identity —
    /// the runner's counts are aggregates — so the pins are digested sorted.
    ///
    /// 40 hex characters, matching the shape [`RecordInput::corpus_revision`]
    /// takes for a single-repository corpus.
    ///
    /// The digest is taken over each pin's *identity*, not its text, so that
    /// re-spelling a pin without changing what it points at leaves the series
    /// intact: re-casing a repository name checks out the same code, and a
    /// digest that moved would restart the dashboard's trend for no reason.
    /// Validation has already rejected the spellings that cannot be
    /// canonicalised this way — revisions must be lowercase — so this and the
    /// corpus validator agree on what "the same pin" means.
    pub fn revision(&self) -> String {
        let mut pins: Vec<(String, &str, &str)> = self
            .projects
            .iter()
            .map(|pin| {
                (
                    repository_identity(&pin.repository),
                    pin.revision.as_str(),
                    pin.project.as_str(),
                )
            })
            .collect();
        pins.sort();
        let mut hash = Sha1::new();
        hash.update(b"borzoi-project-corpus\0");
        hash.update(self.schema_version.to_string().as_bytes());
        for (repository, revision, project) in pins {
            for field in [repository.as_str(), revision, project] {
                hash.update(b"\0");
                hash.update(field.as_bytes());
            }
        }
        format!("{:x}", hash.finalize())
    }
}

/// Read and validate the pinned project corpus.
pub fn read_project_corpus(path: &Path) -> Result<ProjectCorpus, StatsError> {
    let corpus: ProjectCorpus = read_json(path)?;
    validate_project_corpus(&corpus)?;
    Ok(corpus)
}

fn validate_project_corpus(corpus: &ProjectCorpus) -> Result<(), StatsError> {
    if corpus.schema_version != PROJECT_CORPUS_SCHEMA_VERSION {
        return invalid(format!(
            "unsupported project corpus schema version {} (expected {PROJECT_CORPUS_SCHEMA_VERSION})",
            corpus.schema_version
        ));
    }
    if corpus.projects.is_empty() {
        return invalid("project corpus must pin at least one project");
    }
    let mut seen: std::collections::BTreeMap<String, (&String, &String)> =
        std::collections::BTreeMap::new();
    for pin in &corpus.projects {
        if !valid_repository(&pin.repository) {
            return invalid(format!(
                "project corpus repository must be OWNER/REPO with path-safe components, got {:?}",
                pin.repository
            ));
        }
        // `owner/repo` and `owner/repo.git` clone the same repository, but the
        // duplicate and revision checks below compare these strings literally,
        // so both spellings survive as separate pins, get separate checkout
        // directories, and measure one project twice — doubling every count it
        // contributes while the workflow's comparable-count assertion still
        // passes. GitHub repository names cannot end in `.git`, so refusing
        // costs nothing.
        if pin.repository.ends_with(".git") {
            return invalid(format!(
                "project corpus repository must not carry a `.git` suffix, which aliases the \
                 same repository under a second spelling, got {:?}",
                pin.repository
            ));
        }
        validate_hex("project corpus revision", &pin.revision, 40)?;
        // Git resolves an uppercase object ID to the same commit, so two
        // casings of one revision check out identical code — but
        // `ProjectCorpus::revision` hashes the text, so they would produce
        // different digests and split one corpus across two series. Requiring
        // lowercase is the *only* spelling rather than folding it, so the file
        // and the digest agree by construction.
        if pin.revision.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return invalid(format!(
                "project corpus revision must be lowercase hexadecimal, got {:?}",
                pin.revision
            ));
        }
        if !valid_relative_project_path(&pin.project) {
            return invalid(format!(
                "project corpus project must be a relative `.fsproj` path in its only spelling \
                 (no `.` or `..` components, no whitespace, no path-list separator), got {:?}",
                pin.project
            ));
        }
        // One checkout per repository, so two revisions of it cannot both be
        // measured; and one measurement per project, so nothing double-counts.
        //
        // Both comparisons are by *identity*, not by text, because the corpus
        // is keyed on things that alias. See `repository_identity`.
        let identity = repository_identity(&pin.repository);
        if let Some((existing_repository, existing_revision)) =
            seen.insert(identity.clone(), (&pin.repository, &pin.revision))
        {
            if existing_revision != &pin.revision {
                return invalid(format!(
                    "project corpus pins {} at two revisions ({existing_revision} and {})",
                    pin.repository, pin.revision
                ));
            }
            if existing_repository != &pin.repository {
                return invalid(format!(
                    "project corpus spells one repository two ways ({existing_repository} and \
                     {}); GitHub resolves both to the same repository, so it would be cloned \
                     and measured twice",
                    pin.repository
                ));
            }
        }
        if corpus
            .projects
            .iter()
            .filter(|other| {
                repository_identity(&other.repository) == identity && other.project == pin.project
            })
            .count()
            > 1
        {
            return invalid(format!(
                "project corpus pins {}/{} more than once",
                pin.repository, pin.project
            ));
        }
    }
    Ok(())
}

/// The identity two pins are the *same repository* under.
///
/// Every check in this module compares pins to catch a project being measured
/// twice, and a comparison is only as good as the identity it uses. GitHub
/// resolves owner and repository names case-insensitively, so
/// `Smaug123/WoofWare.Expect` and `smaug123/woofware.expect` are one
/// repository that a literal comparison sees as two — and the workflow would
/// clone both, into two directories that a case-sensitive Linux filesystem
/// keeps happily distinct, and count the project twice while the job's
/// comparable-count assertion still passed.
///
/// ASCII case folding is *complete* here rather than one more entry on a list
/// of forbidden spellings: [`valid_repo_component`] admits only ASCII
/// alphanumerics, `-`, `_` and `.`, so ASCII lowercasing is the whole of the
/// case equivalence and no further alias exists to discover.
///
/// The project path is deliberately **not** folded: it names a file on the
/// runner's case-sensitive filesystem, where `A/B.fsproj` and `A/b.fsproj`
/// really are different files.
fn repository_identity(repository: &str) -> String {
    repository.to_ascii_lowercase()
}

/// A repository-relative `.fsproj` path that cannot escape its checkout. The
/// workflow interpolates this into a shell path, so it must also be free of
/// whitespace and of the tab the plan output uses as its separator.
///
/// The path must additionally be in its *only* spelling, because duplicate
/// detection compares these strings literally: `A/B.fsproj` and `A/./B.fsproj`
/// name one file but survive as two pins, and the runner would then visit that
/// project twice and double every count it contributes — a corrupted series
/// reported as a healthy one. Traversal components are therefore rejected
/// rather than normalised: no pin has any reason to contain one, so refusing
/// is both simpler and louder than rewriting.
///
/// It must also survive the journey to the runner intact. The workflow joins
/// these paths with the platform path-list separator and
/// `BORZOI_PROJECT_LIST` splits them with [`std::env::split_paths`], so a path
/// containing `:` — legal on Linux — would arrive as two nonexistent
/// fragments. Both separators are refused, not just the host's, since the pin
/// file is read on whichever machine runs the measurement.
fn valid_relative_project_path(value: &str) -> bool {
    !value.is_empty()
        && value.ends_with(".fsproj")
        && !value.starts_with('/')
        && !value.contains('\\')
        && !value.contains("//")
        && !value.contains(':')
        && !value.contains(';')
        && !value.bytes().any(|byte| byte.is_ascii_whitespace())
        && !value.bytes().any(|byte| byte.is_ascii_control())
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn validate_record_input(input: &RecordInput) -> Result<(), StatsError> {
    validate_provenance(Provenance {
        repository: &input.repository,
        commit: &input.commit,
        measured_at: &input.measured_at,
        run_id: input.run_id,
        run_attempt: input.run_attempt,
        corpus_source: &input.corpus_source,
        corpus_revision: &input.corpus_revision,
        flake_lock_hash: &input.flake_lock_hash,
    })?;
    if input.run_number == 0 {
        return invalid("workflow run number must be non-zero");
    }
    Ok(())
}

struct Provenance<'a> {
    repository: &'a str,
    commit: &'a str,
    measured_at: &'a str,
    run_id: u64,
    run_attempt: u32,
    corpus_source: &'a str,
    corpus_revision: &'a str,
    flake_lock_hash: &'a str,
}

fn validate_provenance(provenance: Provenance<'_>) -> Result<(), StatsError> {
    if !valid_repository(provenance.repository) {
        return invalid(format!(
            "repository must be OWNER/REPO with path-safe components, got {:?}",
            provenance.repository
        ));
    }
    if !valid_repository(provenance.corpus_source) {
        return invalid(format!(
            "corpus source must be OWNER/NAME with path-safe components, got {:?}",
            provenance.corpus_source
        ));
    }
    validate_hex("commit", provenance.commit, 40)?;
    validate_hex("corpus revision", provenance.corpus_revision, 40)?;
    validate_hex("flake.lock hash", provenance.flake_lock_hash, 64)?;
    if provenance.run_id == 0 {
        return invalid("workflow run id must be non-zero");
    }
    if provenance.run_attempt == 0 {
        return invalid("workflow run attempt must be non-zero");
    }
    if !valid_timestamp(provenance.measured_at) {
        return invalid(format!(
            "measured-at must be an ISO-8601 UTC timestamp (YYYY-MM-DDTHH:MM:SSZ), got {:?}",
            provenance.measured_at
        ));
    }
    Ok(())
}

fn validate_generator(generator: &GeneratorSummary) -> Result<(), StatsError> {
    if generator.schema_version != GENERATOR_SCHEMA_VERSION {
        return invalid(format!(
            "unsupported generator schema version {} (expected {})",
            generator.schema_version, GENERATOR_SCHEMA_VERSION
        ));
    }
    if !valid_measurement(&generator.measurement) {
        return invalid(format!(
            "measurement must be a lowercase kebab-case path segment, got {:?}",
            generator.measurement
        ));
    }
    if !generator.configuration.is_object() {
        return invalid("generator configuration must be a JSON object");
    }
    if !generator.statistics.is_object() {
        return invalid("generator statistics must be a JSON object");
    }
    if contains_array(&generator.statistics) {
        return invalid("generator statistics must not contain arrays");
    }
    if !contains_number(&generator.statistics) {
        return invalid("generator statistics must contain at least one number");
    }
    if let Some(key) = key_that_cannot_name_a_metric(&generator.statistics) {
        return invalid(format!(
            "generator statistics key {key:?} cannot name a metric: a key is one segment of a \
             metric path, so it must be non-empty and free of the dot that spells nesting. An \
             empty key yields a path no retirement can be written for, and a dotted one names the \
             same metric as the nested spelling of it — either way the metric could never be \
             retired, and the series would wedge the moment it was dropped"
        ));
    }
    let emitted = metric_paths(&generator.statistics);
    debug_assert!(
        emitted.iter().all(|path| valid_metric_path(path)),
        "a statistics tree whose keys are all valid segments names only retirable metrics"
    );
    let mut declared = std::collections::BTreeSet::new();
    for retired in &generator.retired_statistics {
        if !valid_metric_path(retired) {
            return invalid(format!(
                "retired statistic must be a dotted metric path with non-empty segments, got {retired:?}"
            ));
        }
        if !declared.insert(retired.as_str()) {
            return invalid(format!(
                "retired statistic {retired:?} is listed more than once"
            ));
        }
        if emitted.contains(retired.as_str()) {
            return invalid(format!(
                "generator retires {retired:?} but also emits it; a retirement says the metric is \
                 gone, and one that is still measured would licence dropping it later unannounced"
            ));
        }
    }
    Ok(())
}

/// Every metric the dashboard can plot from these statistics, in the dotted
/// spelling it names them by.
///
/// This mirrors the site's `numbers()` walk, and the mirroring is the point:
/// the dashboard plots one metric per nested **number**, so a `null` is not a
/// metric here either. A field that starts serialising as `null` has therefore
/// dropped its metric exactly as surely as one deleted from the type, and
/// [`metrics_that_vanished`] sees both.
fn metric_paths(statistics: &Value) -> std::collections::BTreeSet<String> {
    fn walk(value: &Value, prefix: &str, output: &mut std::collections::BTreeSet<String>) {
        match value {
            Value::Number(_) => {
                output.insert(prefix.to_string());
            }
            Value::Object(fields) => {
                for (key, child) in fields {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    walk(child, &path, output);
                }
            }
            Value::Null | Value::Bool(_) | Value::Array(_) | Value::String(_) => {}
        }
    }
    let mut output = std::collections::BTreeSet::new();
    walk(statistics, "", &mut output);
    output
}

fn valid_metric_path(value: &str) -> bool {
    !value.is_empty() && value.split('.').all(|segment| !segment.is_empty())
}

/// A statistics key that cannot stand as one segment of a metric path, if
/// there is one.
///
/// Both refusals exist so that every metric an accepted observation names can
/// be *retired* — an observation recording a metric no declaration can spell
/// wedges its series permanently the moment that metric is dropped, since the
/// drop can then neither be published nor declared.
///
/// - A **dotted** key collides with nesting: `{"a.b": 1}` and `{"a": {"b": 1}}`
///   are one metric name for two shapes, so re-nesting a key would be invisible
///   to [`metrics_that_vanished`] — nothing vanished, by name — while every
///   reader of the old shape sees a different key.
/// - An **empty** key yields the path `""` at the root, or a trailing-dot path
///   when nested. Neither is a metric path [`valid_metric_path`] accepts.
fn key_that_cannot_name_a_metric(statistics: &Value) -> Option<&str> {
    match statistics {
        Value::Object(fields) => fields.iter().find_map(|(key, child)| {
            if key.is_empty() || key.contains('.') {
                Some(key.as_str())
            } else {
                key_that_cannot_name_a_metric(child)
            }
        }),
        _ => None,
    }
}

/// The metrics `previous` measured that `incoming` neither measures nor
/// declares retired.
///
/// This is the across-run half of the contract's "every key, every run, always
/// a number". The within-run half is the generator's own exhaustiveness — emit
/// zeros for a closed enumeration, never `collect()` over the observed variants
/// — and no recorder can check it, because a summary with one fewer key is
/// indistinguishable from a measurement that genuinely has fewer metrics. Two
/// consecutive observations of one series are a different matter: they measure
/// the same thing over the same corpus with the same toolchain, so a key
/// present in one and absent from the next is a change in what is measured, and
/// the generator is the only thing that knows whether it meant it.
fn metrics_that_vanished(previous: &GeneratorSummary, incoming: &GeneratorSummary) -> Vec<String> {
    let still_emitted = metric_paths(&incoming.statistics);
    let retired: std::collections::BTreeSet<&str> = incoming
        .retired_statistics
        .iter()
        .map(String::as_str)
        .collect();
    metric_paths(&previous.statistics)
        .into_iter()
        .filter(|metric| !still_emitted.contains(metric) && !retired.contains(metric.as_str()))
        .collect()
}

fn validate_observation(observation: &Observation) -> Result<(), StatsError> {
    if observation.observation_schema_version != OBSERVATION_SCHEMA_VERSION {
        return invalid(format!(
            "unsupported observation schema version {}",
            observation.observation_schema_version
        ));
    }
    validate_provenance(Provenance {
        repository: &observation.repository,
        commit: &observation.commit,
        measured_at: &observation.measured_at,
        run_id: observation.workflow.run_id,
        run_attempt: observation.workflow.run_attempt,
        corpus_source: &observation.corpus.source,
        corpus_revision: &observation.corpus.revision,
        flake_lock_hash: &observation.flake_lock_hash,
    })?;
    if observation.workflow.run_number == Some(0) {
        return invalid("workflow run number must be non-zero");
    }
    validate_generator(&observation.generator)?;
    let expected_url = format!(
        "https://github.com/{}/actions/runs/{}",
        observation.repository, observation.workflow.run_id
    );
    if observation.workflow.url != expected_url {
        return invalid(format!(
            "workflow URL {:?} does not match repository and run id",
            observation.workflow.url
        ));
    }
    let expected_series = series_key(
        &observation.generator,
        &observation.corpus.revision,
        &observation.flake_lock_hash,
    )?;
    if observation.series != expected_series {
        return invalid(format!(
            "series {:?} does not match generator configuration (expected {expected_series:?})",
            observation.series
        ));
    }
    Ok(())
}

/// The observation `incoming` will *follow* on the dashboard: the greatest one
/// already recorded in its series directory that orders strictly before it,
/// under the very ordering [`build_site`] renders by.
///
/// Predecessor rather than "the newest recorded" because runs finish out of
/// order, and the ordering is by workflow creation, not completion. A slow run
/// that lands after a later one has a smaller metric namespace *because the
/// later commit widened it*, and comparing it against the newest would refuse
/// it for a drop that never happened. Strictly-before also excludes the
/// observation's own file, so a rerun is not compared against itself.
///
/// Reading the whole directory is cheap — a series is tens of small files — and
/// the alternative, tracking a per-series head, would be a second source of
/// truth about an ordering `build_site` already derives from the files.
///
/// "Already recorded" bounds what the check can claim, and the bound is real
/// rather than theoretical: an arrival order that lands a drop before the runs
/// that carried the metric leaves that adjacent pair unexamined for good, since
/// every later observation then compares against the gapped one. See
/// `a_drop_escapes_when_the_runs_carrying_it_are_recorded_after_it`, which pins
/// the shape, and "Retiring a metric" in `docs/continuous-measurements.md` for
/// why validating the successor instead would refuse the innocent observation.
fn recorded_predecessor(
    series: &Path,
    incoming: &Observation,
) -> Result<Option<Observation>, StatsError> {
    if !series.is_dir() {
        return Ok(None);
    }
    let mut paths = Vec::new();
    collect_json_files(series, &mut paths)?;
    paths.sort();
    let mut best: Option<Observation> = None;
    for path in paths {
        let stored: Observation = read_json(&path)?;
        if observation_order(&stored, incoming) != std::cmp::Ordering::Less {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|previous| observation_order(previous, &stored) == std::cmp::Ordering::Less)
        {
            best = Some(stored);
        }
    }
    Ok(best)
}

fn observation_order(a: &Observation, b: &Observation) -> std::cmp::Ordering {
    match (a.workflow.run_number, b.workflow.run_number) {
        (Some(a), Some(b)) => a.cmp(&b),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, None) => a.measured_at.cmp(&b.measured_at),
    }
    .then(a.generator.measurement.cmp(&b.generator.measurement))
    .then(a.commit.cmp(&b.commit))
}

/// The identity of a comparable series. The corpus *source* is deliberately
/// absent: a measurement walks exactly one corpus, and the measurement name is
/// already digested, so two sources cannot meet inside one series. Adding it
/// would also renumber every series already published to `stats-data`, and
/// [`validate_observation`] recomputes this key over historical observations.
fn series_key(
    generator: &GeneratorSummary,
    corpus_revision: &str,
    flake_lock_hash: &str,
) -> Result<String, StatsError> {
    let configuration =
        serde_json::to_vec(&generator.configuration).map_err(|source| StatsError::Json {
            path: PathBuf::from("<generator configuration>"),
            source,
        })?;
    let mut hash = Sha1::new();
    hash.update(b"borzoi-stats-series\0");
    hash.update(generator.schema_version.to_string().as_bytes());
    hash.update(b"\0");
    hash.update(generator.measurement.as_bytes());
    hash.update(b"\0");
    hash.update(corpus_revision.as_bytes());
    hash.update(b"\0");
    hash.update(flake_lock_hash.as_bytes());
    hash.update(b"\0");
    hash.update(configuration);
    let digest = format!("{:x}", hash.finalize());
    Ok(format!(
        "v{}-{}-{}",
        generator.schema_version,
        &corpus_revision[..12],
        &digest[..12]
    ))
}

fn observation_path(history: &Path, observation: &Observation) -> PathBuf {
    history
        .join("observations")
        .join(&observation.generator.measurement)
        .join(&observation.series)
        .join(format!("{}.json", observation.commit))
}

fn valid_measurement(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(repo) = parts.next() else {
        return false;
    };
    parts.next().is_none() && valid_repo_component(owner) && valid_repo_component(repo)
}

/// A dot is legal inside a name (`WoofWare.PawPrint`), so an all-dots
/// component is the one shape to exclude: `.` and `..` traverse rather than
/// name, and these components reach both a URL and a directory path.
fn valid_repo_component(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().any(|byte| byte != b'.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_hex(name: &str, value: &str, length: usize) -> Result<(), StatsError> {
    if value.len() != length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return invalid(format!(
            "{name} must be exactly {length} hexadecimal characters, got {value:?}"
        ));
    }
    Ok(())
}

fn valid_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let expected = match index {
            4 | 7 => b'-',
            10 => b'T',
            13 | 16 => b':',
            19 => b'Z',
            _ => {
                if byte.is_ascii_digit() {
                    continue;
                }
                return false;
            }
        };
        if *byte != expected {
            return false;
        }
    }

    let year = decimal(&bytes[0..4]);
    let month = decimal(&bytes[5..7]);
    let day = decimal(&bytes[8..10]);
    let hour = decimal(&bytes[11..13]);
    let minute = decimal(&bytes[14..16]);
    let second = decimal(&bytes[17..19]);
    if year == 0 || !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let days_in_month = match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days_in_month).contains(&day)
}

fn decimal(digits: &[u8]) -> u32 {
    digits
        .iter()
        .fold(0, |value, digit| value * 10 + u32::from(digit - b'0'))
}

fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn contains_array(value: &Value) -> bool {
    match value {
        Value::Array(_) => true,
        Value::Object(values) => values.values().any(contains_array),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn contains_number(value: &Value) -> bool {
    match value {
        Value::Number(_) => true,
        Value::Object(values) => values.values().any(contains_number),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::String(_) => false,
    }
}

fn reject_symlinked_components(root: &Path, target: &Path) -> Result<(), StatsError> {
    let relative = target
        .strip_prefix(root)
        .expect("observation target is rooted under its history directory");
    let mut current = if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root.to_path_buf()
    };
    reject_symlink(&current)?;
    for component in relative.components() {
        current.push(component);
        reject_symlink(&current)?;
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<(), StatsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => invalid(format!(
            "observation history contains symlink {}",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StatsError::Io {
            operation: "inspect",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn collect_json_files(dir: &Path, output: &mut Vec<PathBuf>) -> Result<(), StatsError> {
    reject_symlink(dir)?;
    let entries = fs::read_dir(dir).map_err(|source| StatsError::Io {
        operation: "read directory",
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| StatsError::Io {
            operation: "read directory entry in",
            path: dir.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| StatsError::Io {
            operation: "inspect",
            path: entry.path(),
            source,
        })?;
        if file_type.is_symlink() {
            return invalid(format!(
                "observation history contains symlink {}",
                entry.path().display()
            ));
        }
        if file_type.is_dir() {
            collect_json_files(&entry.path(), output)?;
        } else if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, StatsError> {
    let bytes = fs::read(path).map_err(|source| StatsError::Io {
        operation: "read",
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| StatsError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StatsError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|source| StatsError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    bytes.push(b'\n');
    write_file(path, &bytes)
}

fn create_dir_all(path: &Path) -> Result<(), StatsError> {
    fs::create_dir_all(path).map_err(|source| StatsError::Io {
        operation: "create directory",
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), StatsError> {
    fs::write(path, bytes).map_err(|source| StatsError::Io {
        operation: "write",
        path: path.to_path_buf(),
        source,
    })
}

fn invalid<T>(message: impl Into<String>) -> Result<T, StatsError> {
    Err(StatsError::Invalid(message.into()))
}

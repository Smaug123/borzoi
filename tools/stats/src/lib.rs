//! Durable observations and the static dashboard for Borzoi's corpus measurements.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha1::{Digest, Sha1};

const OBSERVATION_SCHEMA_VERSION: u32 = 1;
const GENERATOR_SCHEMA_VERSION: u32 = 1;
const FSHARP_CORPUS_SOURCE: &str = "dotnet/fsharp";
const INDEX_HTML: &str = include_str!("site/index.html");

#[derive(Debug, Clone)]
pub struct RecordInput {
    pub summary: PathBuf,
    pub history: PathBuf,
    pub repository: String,
    pub commit: String,
    pub measured_at: String,
    pub run_id: u64,
    pub run_attempt: u32,
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
            run_attempt: input.run_attempt,
            url: format!(
                "https://github.com/{}/actions/runs/{}",
                input.repository, input.run_id
            ),
        },
        corpus: Corpus {
            source: FSHARP_CORPUS_SOURCE.to_string(),
            revision: input.corpus_revision.clone(),
        },
        flake_lock_hash: input.flake_lock_hash.clone(),
        generator,
    };
    let path = observation_path(&input.history, &observation);
    let parent = path.parent().expect("observation path has a parent");
    create_dir_all(parent)?;
    write_json(&path, &observation)?;
    Ok(path)
}

/// Validate the complete current-tree history and build a self-contained Pages
/// directory. The deployed site is disposable; `history` remains authoritative.
pub fn build_site(history: &Path, output: &Path) -> Result<usize, StatsError> {
    let root = history.join("observations");
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
    observations.sort_by(|a, b| {
        a.measured_at
            .cmp(&b.measured_at)
            .then(a.generator.measurement.cmp(&b.generator.measurement))
            .then(a.commit.cmp(&b.commit))
    });

    create_dir_all(output)?;
    write_json(&output.join("data.json"), &observations)?;
    write_file(&output.join("index.html"), INDEX_HTML.as_bytes())?;
    write_file(&output.join(".nojekyll"), b"")?;
    Ok(observations.len())
}

fn validate_record_input(input: &RecordInput) -> Result<(), StatsError> {
    if !valid_repository(&input.repository) {
        return invalid(format!(
            "repository must be OWNER/REPO with path-safe components, got {:?}",
            input.repository
        ));
    }
    validate_hex("commit", &input.commit, 40)?;
    validate_hex("corpus revision", &input.corpus_revision, 40)?;
    validate_hex("flake.lock hash", &input.flake_lock_hash, 64)?;
    if input.run_id == 0 {
        return invalid("workflow run id must be non-zero");
    }
    if input.run_attempt == 0 {
        return invalid("workflow run attempt must be non-zero");
    }
    if !valid_timestamp(&input.measured_at) {
        return invalid(format!(
            "measured-at must be an ISO-8601 UTC timestamp (YYYY-MM-DDTHH:MM:SSZ), got {:?}",
            input.measured_at
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
    Ok(())
}

fn validate_observation(observation: &Observation) -> Result<(), StatsError> {
    if observation.observation_schema_version != OBSERVATION_SCHEMA_VERSION {
        return invalid(format!(
            "unsupported observation schema version {}",
            observation.observation_schema_version
        ));
    }
    let input = RecordInput {
        summary: PathBuf::new(),
        history: PathBuf::new(),
        repository: observation.repository.clone(),
        commit: observation.commit.clone(),
        measured_at: observation.measured_at.clone(),
        run_id: observation.workflow.run_id,
        run_attempt: observation.workflow.run_attempt,
        corpus_revision: observation.corpus.revision.clone(),
        flake_lock_hash: observation.flake_lock_hash.clone(),
    };
    validate_record_input(&input)?;
    validate_generator(&observation.generator)?;
    if observation.corpus.source != FSHARP_CORPUS_SOURCE {
        return invalid(format!(
            "unsupported corpus source {:?}",
            observation.corpus.source
        ));
    }
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

fn valid_repo_component(value: &str) -> bool {
    !value.is_empty()
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

fn collect_json_files(dir: &Path, output: &mut Vec<PathBuf>) -> Result<(), StatsError> {
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

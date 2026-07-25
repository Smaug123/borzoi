use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

use borzoi_stats::{
    FSHARP_CORPUS_SOURCE, RecordInput, build_site, read_project_corpus, record_observation,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("borzoi-stats: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    let options = parse_options(args)?;
    match command.as_str() {
        "record" => {
            let input = RecordInput {
                summary: path(&options, "summary")?,
                history: path(&options, "history")?,
                repository: required(&options, "repository")?,
                commit: required(&options, "commit")?,
                measured_at: required(&options, "measured-at")?,
                run_id: number(&options, "run-id")?,
                run_number: number(&options, "run-number")?,
                run_attempt: number(&options, "run-attempt")?,
                corpus_source: options
                    .get("corpus-source")
                    .cloned()
                    .unwrap_or_else(|| FSHARP_CORPUS_SOURCE.to_string()),
                corpus_revision: required(&options, "corpus-revision")?,
                flake_lock_hash: required(&options, "flake-lock-hash")?,
            };
            reject_unknown(
                &options,
                &[
                    "summary",
                    "history",
                    "repository",
                    "commit",
                    "measured-at",
                    "run-id",
                    "run-number",
                    "run-attempt",
                    "corpus-source",
                    "corpus-revision",
                    "flake-lock-hash",
                ],
            )?;
            let path = record_observation(&input).map_err(|error| error.to_string())?;
            println!("{}", path.display());
        }
        "corpus" => {
            reject_unknown(&options, &["pins", "emit"])?;
            let corpus =
                read_project_corpus(&path(&options, "pins")?).map_err(|error| error.to_string())?;
            match required(&options, "emit")?.as_str() {
                // The digest the observation is filed under; see
                // `ProjectCorpus::revision`.
                "digest" => println!("{}", corpus.revision()),
                // One tab-separated pin per line, for the workflow's checkout
                // loop. Validation already rejected whitespace in every field,
                // so `read repository revision project` splits it exactly.
                "plan" => {
                    let mut pins = corpus.projects;
                    pins.sort();
                    for pin in pins {
                        println!("{}\t{}\t{}", pin.repository, pin.revision, pin.project);
                    }
                }
                other => return Err(format!("--emit must be digest or plan, got {other:?}")),
            }
        }
        "site" => {
            reject_unknown(&options, &["history", "output"])?;
            let history = path(&options, "history")?;
            let output = path(&options, "output")?;
            let count = build_site(&history, &output).map_err(|error| error.to_string())?;
            println!(
                "built dashboard with {count} observations at {}",
                output.display()
            );
        }
        _ => return Err(usage()),
    }
    Ok(())
}

fn parse_options(args: impl Iterator<Item = String>) -> Result<BTreeMap<String, String>, String> {
    let mut args = args;
    let mut options = BTreeMap::new();
    while let Some(flag) = args.next() {
        let Some(name) = flag.strip_prefix("--") else {
            return Err(format!("expected --option, got {flag:?}"));
        };
        if name.is_empty() {
            return Err("empty option name".into());
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for --{name}"))?;
        if options.insert(name.to_string(), value).is_some() {
            return Err(format!("duplicate option --{name}"));
        }
    }
    Ok(options)
}

fn required(options: &BTreeMap<String, String>, name: &str) -> Result<String, String> {
    options
        .get(name)
        .cloned()
        .ok_or_else(|| format!("missing required option --{name}"))
}

fn path(options: &BTreeMap<String, String>, name: &str) -> Result<PathBuf, String> {
    required(options, name).map(PathBuf::from)
}

fn number<T>(options: &BTreeMap<String, String>, name: &str) -> Result<T, String>
where
    T: std::str::FromStr,
{
    let value = required(options, name)?;
    value
        .parse()
        .map_err(|_| format!("--{name} must be an integer, got {value:?}"))
}

fn reject_unknown(options: &BTreeMap<String, String>, allowed: &[&str]) -> Result<(), String> {
    if let Some(name) = options
        .keys()
        .find(|name| !allowed.contains(&name.as_str()))
    {
        return Err(format!("unknown option --{name}"));
    }
    Ok(())
}

fn usage() -> String {
    "usage: borzoi-stats record --summary PATH --history PATH --repository OWNER/REPO \
     --commit SHA --measured-at TIMESTAMP --run-id ID --run-number N --run-attempt N \
     --corpus-revision SHA --flake-lock-hash SHA256 [--corpus-source OWNER/NAME]\n       \
     borzoi-stats corpus --pins PATH --emit digest|plan\n       \
     borzoi-stats site --history PATH --output PATH"
        .into()
}

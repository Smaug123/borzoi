use std::path::PathBuf;
use std::process::ExitCode;

use borzoi_corpus_diff::{
    check_project_corpus_run, corpus_runner_config_from_env, project_candidates_from_env,
    project_corpus_run_options_from_env, render_project_corpus_run_report,
    run_project_corpus_diff_with_options, write_json_report_line,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let projects = project_candidates_from_env().map_err(|err| err.to_string())?;
    let options = project_corpus_run_options_from_env().map_err(|err| err.to_string())?;
    let config = corpus_runner_config_from_env().map_err(|err| err.to_string())?;
    let run = run_project_corpus_diff_with_options(projects, options);
    eprint!("{}", render_project_corpus_run_report(&run));

    if let Some(path) = std::env::var_os("BORZOI_PROJECT_REPORT_JSONL") {
        write_json_report_line(&PathBuf::from(path), &run.summary)
            .map_err(|err| format!("failed to write BORZOI_PROJECT_REPORT_JSONL: {err}"))?;
    }

    check_project_corpus_run(&run, config).map_err(|err| err.to_string())
}

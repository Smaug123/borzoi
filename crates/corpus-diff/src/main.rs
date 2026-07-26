use std::path::PathBuf;
use std::process::ExitCode;

use borzoi_corpus_diff::{
    ProjectCandidateSettings, check_project_corpus_run, corpus_runner_config_from_env,
    project_candidates_from_settings, project_corpus_run_options_from_env,
    render_project_corpus_run_report, run_project_corpus_diff_with_options,
    write_generator_summary, write_json_report_line,
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
    // The settings, not just the candidates they select: the generator summary
    // records how the run chose its projects, which is part of a series'
    // identity and is no longer recoverable once selection has happened.
    let settings = ProjectCandidateSettings::from_env().map_err(|err| err.to_string())?;
    let projects = project_candidates_from_settings(settings.clone());
    let options = project_corpus_run_options_from_env().map_err(|err| err.to_string())?;
    let config = corpus_runner_config_from_env().map_err(|err| err.to_string())?;
    let run = run_project_corpus_diff_with_options(projects, options);
    eprint!("{}", render_project_corpus_run_report(&run));

    if let Some(path) = std::env::var_os("BORZOI_PROJECT_REPORT_JSONL") {
        write_json_report_line(&PathBuf::from(path), &run.summary)
            .map_err(|err| format!("failed to write BORZOI_PROJECT_REPORT_JSONL: {err}"))?;
    }

    if let Some(path) = std::env::var_os("BORZOI_PROJECT_SUMMARY_JSON") {
        write_generator_summary(&PathBuf::from(path), &run.summary, &settings)
            .map_err(|err| format!("failed to write BORZOI_PROJECT_SUMMARY_JSON: {err}"))?;
    }

    check_project_corpus_run(&run, config).map_err(|err| err.to_string())
}

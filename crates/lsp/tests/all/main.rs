//! The `borzoi` test binary.
//!
//! Every case group is a submodule here rather than its own `tests/*.rs`
//! target: Cargo compiles and links each integration-test file as a separate
//! crate, so one binary pays that once. Filter with
//! `cargo test -p borzoi --test all <module>::`.

mod common;

mod assembly_cache_project_e2e;
mod assembly_cache_roundtrip;
mod csharp_ref_assembly_env_e2e;
mod csharp_sidecar;
mod csharp_sidecar_bundled_e2e;
mod fcs_bridge;
mod glob_msbuild_diff;
mod goto_source_fsharp_core;
mod goto_source_sidecar;
mod handlers_completion;
mod handlers_definition;
mod handlers_document_symbol;
mod handlers_hover;
mod handlers_references;
mod handlers_references_corpus_diff;
mod handlers_references_diff;
mod handlers_semantic_tokens;
mod handlers_workspace_symbol;
mod ifdef_diagnostics_integration;
mod lsp_integration;
mod lsp_msbuild_user_extensions_e2e;
mod parse_cache;
mod parser_corpus_sweep;
mod project_assets_integration;
mod reference_set_msbuild_diff;
mod resolve_real_project_diff;
mod restore_hostile_config_matrix;
mod restore_on_demand_e2e;
mod restore_to_scratch_diff;
mod sdk_project_fold_e2e;
mod sdk_resolution_exactness_diff;
mod sdk_resolution_oracle;
mod sdk_resolution_override_classification;
mod watched_assembly_refresh_e2e;

/// Every case group under `tests/all/` must be `mod`-declared here, or it is
/// silently never compiled or run. See the module for why that is worth a test.
#[test]
fn all_case_groups_are_declared() {
    borzoi_oracle_harness::module_tree::assert_all_case_groups_declared(
        env!("CARGO_MANIFEST_DIR"),
        file!(),
    );
}

//! The `borzoi-sema` test binary.
//!
//! Every case group is a submodule here rather than its own `tests/*.rs`
//! target: Cargo compiles and links each integration-test file as a separate
//! crate, so one binary pays that once. Filter with
//! `cargo test -p borzoi-sema --test all <module>::`.

mod common;

mod assembly_env;
mod attr_resolution_diff;
mod attr_resolution_sweep;
mod classify_assembly_diff;
mod classify_diff;
mod extension_visibility_matrix;
mod infer_annotation_entity_diff;
mod infer_binder_types_diff;
mod infer_literals_diff;
mod infer_member_access_diff;
mod infer_static_call_diff;
mod module_open_matrix;
mod namespace_fold_matrix;
mod overload_corpus_diff;
mod overloads_oracle;
mod project_half_matrix;
mod resolve_active_patterns;
mod resolve_assembly;
mod resolve_assembly_diff;
mod resolve_autoopen;
mod resolve_corpus_diff;
mod resolve_cross_file_active_patterns;
mod resolve_cross_file_cases;
mod resolve_diff;
mod resolve_divergence;
mod resolve_enums;
mod resolve_exceptions;
mod resolve_export_case_kind;
mod resolve_fsharp_abbrev;
mod resolve_fsharp_core;
mod resolve_incremental_diff;
mod resolve_module_opens;
mod resolve_nested_modules;
mod resolve_project;
mod resolve_project_assembly_diff;
mod resolve_project_diff;
mod resolve_qualified_path_access_gen_diff;
mod resolve_qualified_values;
mod resolve_qualifier_precedence_diff;
mod resolve_scoping;
mod resolve_straddle_gen_diff;
mod resolve_string_qualifier_repro;
mod resolve_type_members;
mod resolve_type_qualified_cases;
mod resolve_types;
mod resolve_union_cases;
mod types_census;
mod use_rec;
mod uses_census;
mod uses_census_project;
mod uses_project_smoke;
mod uses_smoke;

/// Every case group under `tests/all/` must be `mod`-declared here, or it is
/// silently never compiled or run. See the module for why that is worth a test.
#[test]
fn all_case_groups_are_declared() {
    borzoi_oracle_harness::module_tree::assert_all_case_groups_declared(
        env!("CARGO_MANIFEST_DIR"),
        file!(),
    );
}

//! The `borzoi-cst` test binary.
//!
//! Every case group is a submodule here rather than its own `tests/*.rs`
//! target. Cargo compiles and links each integration-test file as a
//! separate crate, and each one that drives FCS spawns its own `fcs-dump`
//! child; folding them into one binary pays both costs once. Filter with
//! `cargo test -p borzoi-cst --test all <module>::` — e.g.
//! `… --test all parser_diff_match::`.

mod common;

mod ast_projection;
mod corpus;
mod corpus_walk;
mod fcs_divergence;
mod kind_name_totality;
mod langversion_gate;
mod lexer_diff;
mod lexfilter_corpus;
mod lexfilter_depth;
mod lexfilter_diff;
mod line_index;
mod nullness_gate;
mod oracle_pool;
mod parser_corpus;
mod parser_corpus_diff;
mod parser_depth_limit;
mod parser_diff_active_pattern_expr;
mod parser_diff_active_patterns;
mod parser_diff_anon_recd;
mod parser_diff_app_nesting;
mod parser_diff_assignment;
mod parser_diff_ast_ranges;
mod parser_diff_atomic_app;
mod parser_diff_base;
mod parser_diff_begin_end;
mod parser_diff_brack_index;
mod parser_diff_class_do;
mod parser_diff_coercion;
mod parser_diff_colon_equals;
mod parser_diff_compexpr;
mod parser_diff_cons;
mod parser_diff_control_flow;
mod parser_diff_dot_access;
mod parser_diff_dot_lambda;
mod parser_diff_dynamic;
mod parser_diff_extern;
mod parser_diff_fixed;
mod parser_diff_functions;
mod parser_diff_global;
mod parser_diff_global_pat;
mod parser_diff_hash_directive;
mod parser_diff_ifdef;
mod parser_diff_inherit_record;
mod parser_diff_lazy;
mod parser_diff_let_bindings;
mod parser_diff_lists;
mod parser_diff_literals;
mod parser_diff_match;
mod parser_diff_measure;
mod parser_diff_module_structure;
mod parser_diff_multiline_infix;
mod parser_diff_new_expr;
mod parser_diff_obj_expr;
mod parser_diff_offside;
mod parser_diff_operators;
mod parser_diff_pat_opname_path;
mod parser_diff_pat_typars;
mod parser_diff_quote_pat;
mod parser_diff_ranges;
mod parser_diff_reserved_idents;
mod parser_diff_sig_files;
mod parser_diff_srtp_support_matrix;
mod parser_diff_static_optimization;
mod parser_diff_strings;
mod parser_diff_struct_expr;
mod parser_diff_struct_pat;
mod parser_diff_tabs;
mod parser_diff_then;
mod parser_diff_trait_call;
mod parser_diff_try;
mod parser_diff_typar_expr;
mod parser_diff_typar_intersection;
mod parser_diff_type_app_expr;
mod parser_diff_type_relation;
mod parser_diff_types;
mod parser_diff_when_constraints;
mod parser_ifdef;
mod shape_sensitivity;

/// Every case group under `tests/all/` must be `mod`-declared here, or it is
/// silently never compiled or run. See the module for why that is worth a test.
#[test]
fn all_case_groups_are_declared() {
    borzoi_oracle_harness::module_tree::assert_all_case_groups_declared(
        env!("CARGO_MANIFEST_DIR"),
        file!(),
    );
}

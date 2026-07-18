//! The `borzoi-assembly` test binary.
//!
//! Every case group is a submodule here rather than its own `tests/*.rs`
//! target: Cargo compiles and links each integration-test file as a separate
//! crate, so one binary pays that once. Filter with
//! `cargo test -p borzoi-assembly --test all <module>::`.

mod common;

mod abbreviation_target_diff;
mod assembly_auto_opens;
mod assembly_diff;
mod bcl_ref_pack_sweep;
mod display_member;
mod display_type;
mod doc_id_diff;
mod doc_id_fsharp_core_diff;
mod explicit_interface;
mod fail_loud;
mod fsharp_pickle_diff;
mod fsharp_pickle_fail_loud;
mod fsharp_pickle_fsharp_core;
mod fsharp_pickle_module_member_index;
mod generative_source_diff;
mod methodimpl_classification;
mod modifier_metamorphic;
mod pdb_fsharp_core;
mod projector_custom_modifiers;
mod projector_default_member;
mod projector_events;
mod projector_extension_index;
mod projector_fsharp_core;
mod projector_generic_nullability;
mod projector_generics;
mod projector_malformed_metadata;
mod projector_markers;
mod projector_member_shapes;
mod projector_nullable;
mod projector_obsolete_experimental;
mod projector_open_surface;
mod projector_overload_flags;
mod projector_ref_nullability;
mod projector_required_members;
mod projector_source_names;
mod projector_type_shapes;
mod projector_typeref_shapes;
mod well_known_attributes_sync;

/// Every case group under `tests/all/` must be `mod`-declared here, or it is
/// silently never compiled or run. See the module for why that is worth a test.
#[test]
fn all_case_groups_are_declared() {
    borzoi_oracle_harness::module_tree::assert_all_case_groups_declared(
        env!("CARGO_MANIFEST_DIR"),
        file!(),
    );
}

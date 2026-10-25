//! The `borzoi-nuget` test binary.
//!
//! Every case group is a submodule here rather than its own `tests/*.rs`
//! target. Cargo compiles and links each integration-test file as a separate
//! crate, and each one that drives `tools/nuget-oracle` spawns its own oracle
//! child; folding them into one binary pays both costs once. It also means a
//! failing unit-style case no longer stops the differentials from running:
//! Cargo's fail-fast is per *binary*, and there is now only one. Filter with
//! `cargo test -p borzoi-nuget --test all <module>::` — e.g.
//! `… --test all resolver_diff::`. The `#[ignore]`d soaks run with
//! `… --test all soak:: -- --ignored` and
//! `… --test all resolver_diff::randomised_soundness_soak -- --ignored`.

mod common;

mod compile_assets;
mod compile_assets_diff;
mod compile_assets_properties;
mod compile_assets_restore;
mod framework_diff;
mod framework_exhaustive;
mod framework_properties;
mod nuspec_diff;
mod package_cache;
mod range_diff;
mod range_properties;
mod resolver;
mod resolver_complexity;
mod resolver_diff;
mod soak;
mod version_diff;
mod version_properties;

/// Every case group under `tests/all/` must be `mod`-declared here, or it is
/// silently never compiled or run. See the module for why that is worth a test.
#[test]
fn all_case_groups_are_declared() {
    borzoi_oracle_harness::module_tree::assert_all_case_groups_declared(
        env!("CARGO_MANIFEST_DIR"),
        file!(),
    );
}

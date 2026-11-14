//! Staleness gate: every checked-in generated file — the union's modules and
//! lock, each unpublished facade's lock, each facade's code, the facade
//! property table and the module declarations — must equal a fresh
//! generation. Runs under the normal workspace `cargo test`, so a stale one
//! fails CI. Fix with `cargo run -p borzoi-astgen --example generate`.
//!
//! A published facade's lock and consumer fixture are the generator's input
//! and are never regenerated, so the second test holds them instead: every
//! distinct published facade has its fixture, stamped at its lock's epoch and
//! covering its lock.

use std::path::Path;

use borzoi_astgen::{consumer, facade, lock};

#[test]
fn generated_facades_are_up_to_date() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (rel_path, generated) in borzoi_astgen::all_outputs() {
        let path = root.join(rel_path);
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            on_disk,
            generated,
            "{} is stale — run `cargo run -p borzoi-astgen --example generate` to regenerate",
            path.display(),
        );
    }
}

#[test]
fn every_published_facade_has_a_covering_consumer_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut expected = Vec::new();
    for version in facade::versions() {
        let module = version.module();
        if !facade::PUBLISHED.contains(&module.as_str()) {
            continue;
        }
        let text = facade::checked_in_lock(&version)
            .unwrap_or_else(|| panic!("the published `{module}` has no lock"));
        let locked = lock::parse(&text).unwrap();
        if locked.alias_of().is_some() {
            continue;
        }
        let path = root.join(consumer::fixture_path(&module));
        let fixture = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "the published `{module}` has no consumer fixture ({}: {e}); \
                 `cargo run -p borzoi-astgen --example generate` writes it when it \
                 publishes a facade",
                path.display()
            )
        });
        expected.push(path.clone());
        let epoch = lock::freeze_epoch(&text).unwrap();
        let errors = consumer::check_fixture(&module, &locked, epoch, &fixture);
        assert!(
            errors.is_empty(),
            "{} does not cover its facade's lock:\n  {}",
            path.display(),
            errors.join("\n  ")
        );
    }
    let mut present: Vec<_> = std::fs::read_dir(root.join(consumer::FIXTURES_DIR))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().is_some_and(|n| n != "mod.rs"))
        .collect();
    present.sort();
    expected.sort();
    assert_eq!(
        present, expected,
        "the consumer fixtures are exactly those of the distinct published facades"
    );
}

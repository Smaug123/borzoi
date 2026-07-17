//! Staleness gate: every checked-in generated facade must equal a fresh
//! generation. Runs under the normal workspace `cargo test`, so a stale
//! `crates/cst/src/syntax/generated/*.rs` fails CI. Fix with
//! `cargo run -p borzoi-astgen --example generate`.

use std::path::Path;

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

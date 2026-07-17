//! Correctness oracles for assembly identity, references, and resources.
//!
//! Two families:
//! - refusal: a `ManifestResource` with a non-`CurrentFile` implementation
//!   yields [`Error::UnsupportedResourceImplementation`] (fabricated fixture);
//! - fuzz: the three readers never panic on mutated or arbitrary input.

use super::Error;
use super::manifest::{read_assembly, read_assembly_refs, read_resources};
use super::metadata::MetadataFile;
use super::tables::Tables;
use super::tests::fixtures;
use borzoi_spawn::BoundedCommand;
use proptest::prelude::*;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// A `ManifestResource` whose `Implementation` names another file is refused —
/// the reader extracts bytes only for in-file resources and never fabricates
/// bytes it does not have.
#[test]
fn non_current_file_resource_is_refused() {
    let bytes = emit("linked_file_resource");
    let md = MetadataFile::read(&bytes).expect("emitted fixture parses");
    let tables = Tables::new(&md).expect("table layout");
    assert_eq!(
        read_resources(&tables, &md),
        Err(Error::UnsupportedResourceImplementation),
    );
}

/// A `#~` stream that declares more table rows than its byte region can hold
/// must be refused by [`Tables::new`], not accepted with an impossible layout.
/// Otherwise `read_assembly_refs`/`read_resources` would trust the inflated
/// `row_count` and `Vec::with_capacity` a multi-gigabyte allocation before any
/// per-row bounds check fires.
#[test]
fn rejects_row_count_exceeding_table_stream() {
    use super::tests::{metadata_root_offset, tilde_heap_sizes_offset};

    for p in fixtures() {
        let original = std::fs::read(&p).expect("fixture");
        MetadataFile::read(&original).expect("fixture parses");

        let md_offset = metadata_root_offset(&original);
        // `#~` header: HeapSizes(1) + Reserved(1) + Valid(8) + Sorted(8), then
        // the row-count array. Inflate the first present table's count.
        let rows_at = tilde_heap_sizes_offset(&original, md_offset) + 1 + 1 + 8 + 8;
        let mut corrupted = original.clone();
        corrupted[rows_at..rows_at + 4].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());

        // The container still parses (row counts aren't validated there), but the
        // layout is impossible: the declared rows overrun the `#~` table region.
        let md = MetadataFile::read(&corrupted).expect("container still parses");
        assert!(
            Tables::new(&md).is_err(),
            "row count overrunning the table stream must be refused in {}",
            p.display()
        );
    }
}

proptest! {
    /// Mutating a real assembly at arbitrary offsets never panics the three
    /// manifest readers (when the container still parses).
    #[test]
    fn readers_never_panic_on_mutated_fixtures(
        which in 0usize..3,
        muts in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..64),
    ) {
        let files = fixtures();
        let mut bytes = std::fs::read(&files[which]).expect("fixture");
        for (off, val) in muts {
            if !bytes.is_empty() {
                let i = off % bytes.len();
                bytes[i] = val;
            }
        }
        drive(&bytes);
    }

    /// Arbitrary bytes never panic the readers.
    #[test]
    fn readers_never_panic_on_arbitrary(
        bytes in proptest::collection::vec(any::<u8>(), 0..8192),
    ) {
        drive(&bytes);
    }
}

/// Run the full Stage 3 read path, swallowing every error: it must never panic.
fn drive(bytes: &[u8]) {
    if let Ok(md) = MetadataFile::read(bytes)
        && let Ok(tables) = Tables::new(&md)
    {
        let _ = read_assembly(&tables);
        let _ = read_assembly_refs(&tables);
        let _ = read_resources(&tables, &md);
    }
}

/// Build the `MetadataEmitter` tool once and run it for `shape`, returning the
/// raw PE bytes it writes to stdout. Mirrors the integration tests'
/// `common::emit_metadata_fixture`, which a lib-internal test cannot reach.
fn emit(shape: &str) -> Vec<u8> {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    let bin = BUILT.get_or_init(|| {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/assembly/MetadataEmitter");
        let mut build = Command::new("dotnet");
        build
            .args(["build", "-c", "Release", "--nologo"])
            .arg(&project);
        BoundedCommand::new(build)
            .timeout(super::test_fixtures::BUILD_TIMEOUT)
            .run_ok("dotnet build MetadataEmitter");
        project.join("bin/Release/net10.0/MetadataEmitter.dll")
    });
    let mut run = Command::new("dotnet");
    run.arg(bin).arg(shape);
    BoundedCommand::new(run)
        .run_ok(format_args!("MetadataEmitter {shape}"))
        .stdout
}

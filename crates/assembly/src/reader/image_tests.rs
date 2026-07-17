//! Stage 7.0 correctness oracles for [`parse`] and the owned [`Image`].
//!
//! - **Equivalence**: `parse` is just the assembly of parts already tested
//!   individually (the stage-3 manifest reads and the stage-4–6 type walk), so
//!   its fields must equal those piecewise reads over the corpus.
//! - **Fuzz**: `parse` on arbitrary, truncated, and mutated bytes never panics
//!   (it inherits the per-component fuzz oracles).

use super::image::parse;
use super::manifest::{read_assembly, read_assembly_refs, read_resources};
use super::metadata::MetadataFile;
use super::tables::Tables;
use super::test_fixtures::all_dlls;
use super::typedefs::read_types;
use proptest::prelude::*;

/// `parse` reproduces exactly what the independent piecewise reads produce.
#[test]
fn parse_matches_piecewise_reads() {
    for dll in all_dlls() {
        let label = dll.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&dll).expect("fixture");

        let image = parse(&bytes).unwrap_or_else(|e| panic!("parse failed for {label}: {e:?}"));

        let md = MetadataFile::read(&bytes).expect("container parse");
        let tables = Tables::new(&md).expect("table layout");
        let assembly = read_assembly(&tables).expect("read_assembly");
        let references = read_assembly_refs(&tables).expect("read_assembly_refs");
        let resources = read_resources(&tables, &md).expect("read_resources");
        let types = read_types(&md).expect("type walk");

        assert_eq!(image.assembly, assembly, "assembly in {label}");
        assert_eq!(image.references, references, "references in {label}");
        assert_eq!(image.resources, resources, "resources in {label}");
        assert_eq!(image.type_defs, types.type_defs, "type_defs in {label}");
        assert_eq!(image.top_level, types.top_level, "top_level in {label}");
        assert_eq!(image.type_refs, types.type_refs, "type_refs in {label}");
        assert_eq!(
            image.member_refs, types.member_refs,
            "member_refs in {label}"
        );
        assert_eq!(
            image.assembly_attributes, types.assembly_attributes,
            "assembly_attributes in {label}"
        );

        // The fixtures are real assemblies: each names an `Assembly` row and at
        // least the `<Module>` pseudo-type, so the read is never vacuous.
        assert!(image.assembly.is_some(), "no Assembly row in {label}");
        assert!(!image.type_defs.is_empty(), "no types in {label}");
    }
}

proptest! {
    /// Arbitrary bytes never panic `parse`.
    #[test]
    fn parse_never_panics_on_arbitrary(bytes in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let _ = parse(&bytes);
    }

    /// Mutating a real assembly at arbitrary offsets never panics `parse`.
    #[test]
    fn parse_never_panics_on_mutated(
        which in 0usize..8,
        muts in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..64),
    ) {
        let dlls = all_dlls();
        let mut bytes = std::fs::read(&dlls[which % dlls.len()]).expect("fixture");
        for (off, val) in muts {
            if !bytes.is_empty() {
                let i = off % bytes.len();
                bytes[i] = val;
            }
        }
        let _ = parse(&bytes);
    }
}

/// Every prefix of a real assembly drives `parse` without panic.
#[test]
fn parse_never_panics_on_truncated() {
    for dll in all_dlls() {
        let bytes = std::fs::read(&dll).expect("fixture");
        let step = (bytes.len() / 256).max(1);
        let mut len = 0;
        while len <= bytes.len() {
            let _ = parse(&bytes[..len]);
            len += step;
        }
    }
}

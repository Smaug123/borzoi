//! Slice 1 of offline go-to-definition: pull the **embedded** portable PDB out
//! of a managed DLL's PE debug directory and inflate it to a portable-PDB
//! metadata image.
//!
//! The shipped `FSharp.Core.dll` carries its PDB *embedded* (an
//! `IMAGE_DEBUG_TYPE_EMBEDDEDPORTABLEPDB` entry, magic `MPDB`), not as a
//! sidecar `.pdb` or in the nuget package — so this is what makes go-to-FSharp.Core
//! source possible with zero network and no separate symbol file. Later slices
//! parse the inflated image's tables (documents, sequence points, embedded
//! source); this one only pins that we recover a well-formed metadata image.
//!
//! Requires the .NET 10 SDK on PATH (to build `tools/fcs-dump` once, which drops
//! the `FSharp.Core.dll` this reads); the Nix devShell provides it.

use borzoi_assembly::pdb::{PdbError, PortablePdb, embedded_portable_pdb};

use crate::common::{ensure_fsharp_core_dll, ensure_minilib_built};

/// `BSJB`, the ECMA-335 metadata-root signature (II.24.2.1) — the inflated
/// embedded PDB is itself a metadata image and must begin with it.
const METADATA_SIGNATURE: &[u8] = b"BSJB";

#[test]
fn fsharp_core_embedded_portable_pdb_inflates() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));

    let image = embedded_portable_pdb(&bytes)
        .expect("FSharp.Core's debug directory must parse")
        .expect("FSharp.Core ships an embedded portable PDB");

    // The inflated blob is a portable-PDB *metadata image*: a BSJB-rooted
    // ECMA-335 metadata container (with PDB-specific streams/tables on top).
    assert_eq!(
        image.get(..4),
        Some(METADATA_SIGNATURE),
        "inflated embedded PDB must be a BSJB metadata image"
    );
    // FSharp.Core's PDB is ~half a megabyte; a few dozen bytes would mean we
    // truncated the deflate stream. Lower-bounded to stay version-robust.
    assert!(
        image.len() > 100_000,
        "expected a substantial PDB image; got {} bytes",
        image.len()
    );
}

#[test]
fn assembly_without_embedded_pdb_returns_none() {
    // MiniLib is a stock-`dotnet build` C# assembly: its debug info is a
    // *sidecar* portable `.pdb` (the SDK default `DebugType`), so the PE
    // carries no `EmbeddedPortablePdb` entry and we return `Ok(None)` rather
    // than erroring.
    let dll = ensure_minilib_built();
    let bytes = std::fs::read(dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    assert!(
        embedded_portable_pdb(&bytes)
            .expect("MiniLib's debug directory must parse")
            .is_none(),
        "MiniLib has no embedded PDB"
    );
}

#[test]
fn non_pe_input_is_an_error() {
    // Not a PE at all → a structural error, never a silent `None` (which is
    // reserved for a valid PE that simply has no embedded PDB).
    let err = embedded_portable_pdb(b"not a PE at all").unwrap_err();
    assert!(matches!(err, PdbError::NotPortableExecutable));
}

/// Parse the embedded portable PDB's metadata image and read its `Document`
/// table — the first table the later sequence-point / embedded-source slices
/// build on. The document names are recovered through the portable-PDB
/// path-compression codec.
#[test]
fn fsharp_core_documents_include_known_source_files() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes)
        .expect("debug directory parses")
        .expect("FSharp.Core ships an embedded portable PDB");

    let pdb = PortablePdb::read(&image).expect("parse the portable-PDB metadata image");
    let count = pdb.document_count();
    assert!(
        count > 10,
        "FSharp.Core compiles from many source files; got {count} documents"
    );

    let names: Vec<String> = (1..=count)
        .map(|rid| pdb.document_name(rid).expect("document name decodes"))
        .collect();

    // Two stable FSharp.Core source files. `ends_with` on the basename is
    // robust to the build's path style ('/' vs '\') and root.
    assert!(
        names.iter().any(|n| n.ends_with("printf.fs")),
        "expected printf.fs among the documents; first few: {:?}",
        &names[..names.len().min(8)]
    );
    assert!(
        names.iter().any(|n| n.ends_with("prim-types.fs")),
        "expected prim-types.fs among the documents"
    );
}

/// Decode every method's first non-hidden sequence point and check it resolves
/// to a real source location: a valid `Document` row (→ an `.fs` file) and a
/// 1-based line/column. This exercises the sequence-points delta codec across
/// the whole real assembly — the mapping go-to-definition ultimately reads.
#[test]
fn fsharp_core_method_sequence_points_resolve_to_source() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes)
        .expect("debug directory parses")
        .expect("FSharp.Core ships an embedded portable PDB");
    let pdb = PortablePdb::read(&image).expect("parse the portable-PDB metadata image");

    let methods = pdb.method_debug_info_count();
    assert!(methods > 100, "FSharp.Core has many methods; got {methods}");

    let doc_count = pdb.document_count();
    let mut with_points = 0u32;
    let mut saw_fs_source = false;
    for rid in 1..=methods {
        let Some(sp) = pdb
            .method_first_sequence_point(rid)
            .unwrap_or_else(|e| panic!("decode sequence points for method {rid}: {e}"))
        else {
            continue; // a method with no source mapping (compiler-generated, etc.)
        };
        with_points += 1;
        assert!(
            sp.document >= 1 && sp.document <= doc_count,
            "method {rid} point references document {} outside 1..={doc_count}",
            sp.document
        );
        assert!(
            sp.start_line >= 1 && sp.start_column >= 1,
            "method {rid} point has a 0 line/column ({}:{})",
            sp.start_line,
            sp.start_column
        );
        if pdb
            .document_name(sp.document)
            .expect("referenced document resolves")
            .ends_with(".fs")
        {
            saw_fs_source = true;
        }
    }

    assert!(
        with_points > 100,
        "most FSharp.Core methods carry sequence points; got {with_points}"
    );
    assert!(
        saw_fs_source,
        "method sequence points should reference `.fs` source documents"
    );
}

/// Pull documents' **embedded source** straight from the PDB. FSharp.Core
/// embeds source for only its *generated* files (e.g. `FSCore.fs`); its
/// hand-written sources (`printf.fs`) are SourceLink'd, not embedded — so this
/// pins both that the embedded-source path decodes real text *and* that the
/// hand-written sources are correctly reported as not-embedded.
#[test]
fn fsharp_core_embedded_source_decodes_generated_files() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes)
        .expect("debug directory parses")
        .expect("FSharp.Core ships an embedded portable PDB");
    let pdb = PortablePdb::read(&image).expect("parse the portable-PDB metadata image");

    let count = pdb.document_count();

    // `printf.fs` exists as a document (sequence points reference it) but its
    // source is SourceLink'd, not embedded.
    let printf_rid = (1..=count)
        .find(|&rid| {
            pdb.document_name(rid)
                .map(|n| n.ends_with("printf.fs"))
                .unwrap_or(false)
        })
        .expect("FSharp.Core has a printf.fs document");
    assert_eq!(
        pdb.document_embedded_source(printf_rid)
            .expect("embedded-source lookup succeeds"),
        None,
        "printf.fs is SourceLink'd, not embedded"
    );

    // The generated files *are* embedded: the largest embedded document yields
    // substantial multi-line F# source text, exercising the whole path
    // (CustomDebugInformation scan → kind GUID → value blob → format/inflate).
    let largest = (1..=count)
        .filter_map(|rid| pdb.document_embedded_source(rid).ok().flatten())
        .max_by_key(String::len)
        .expect("FSharp.Core embeds at least one document's source");
    assert!(
        largest.len() > 1000 && largest.contains('\n'),
        "embedded source should be substantial multi-line text; got {} bytes",
        largest.len()
    );
}

/// FSharp.Core's PDB carries a SourceLink record mapping its (SourceLink-only)
/// documents to GitHub raw URLs — the JSON the LSP slice parses to build the
/// fetch effect for `printf.fs` / `printfn`. This pins that we extract it.
#[test]
fn fsharp_core_sourcelink_json_maps_to_github() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes)
        .expect("debug directory parses")
        .expect("FSharp.Core ships an embedded portable PDB");
    let pdb = PortablePdb::read(&image).expect("parse the portable-PDB metadata image");

    let json = pdb
        .sourcelink_json()
        .expect("sourcelink lookup succeeds")
        .expect("FSharp.Core's PDB carries a SourceLink record");

    // The SourceLink document is a JSON `{ "documents": { "<prefix>*": "<url>*" } }`
    // mapping; FSharp.Core's points at the dotnet source on GitHub.
    assert!(
        json.contains("\"documents\""),
        "SourceLink JSON should have a documents map; got: {}",
        &json[..json.len().min(120)]
    );
    assert!(
        json.contains("raw.githubusercontent.com"),
        "FSharp.Core SourceLink should map to GitHub raw URLs"
    );
}

//! Go-to-definition into an assembly whose PDB is a **sidecar** `.pdb` next to
//! the DLL (the common NuGet shape — FsUnit and most packages ship a separate
//! `.pdb`, not an embedded one), rather than embedded in the DLL.
//!
//! There is no checked-in sidecar fixture, so these tests *synthesise* one from
//! the embedded-PDB `FSharp.Core.dll` the FCS-dump build drops: strip its
//! embedded-PDB debug entry to force the sidecar path, and write its (extracted)
//! portable-PDB image to disk as the sidecar the CodeView entry names. The id
//! match against the DLL's CodeView id — the safeguard that rejects a stale
//! `.pdb` — is exercised against real Roslyn-emitted data.

use borzoi::goto_source::{
    definition_source, definition_source_in_pdb, sidecar_pdb_matches, sidecar_pdb_name,
};
use borzoi::handlers::definition::pdb_image_for;
use borzoi_assembly::pdb::{PortablePdb, embedded_portable_pdb};

use crate::common::ensure_fsharp_core_dll;

/// Index of the Debug data directory in the PE optional header.
const DEBUG_DIRECTORY: usize = 6;
/// Size of one `IMAGE_DEBUG_DIRECTORY` entry.
const DEBUG_ENTRY_SIZE: usize = 28;
/// `IMAGE_DEBUG_TYPE_EMBEDDEDPORTABLEPDB`.
const EMBEDDED_PORTABLE_PDB: u32 = 17;

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}

/// Map a PE RVA to its file offset via the section headers.
fn rva_to_file(b: &[u8], sec_off: usize, nsec: usize, rva: u32) -> usize {
    for i in 0..nsec {
        let s = sec_off + i * 40;
        let vsize = u32_at(b, s + 8);
        let vaddr = u32_at(b, s + 12);
        let rawsize = u32_at(b, s + 16);
        let rawptr = u32_at(b, s + 20);
        if rva >= vaddr && rva < vaddr + vsize.max(rawsize) {
            return (rawptr + (rva - vaddr)) as usize;
        }
    }
    panic!("RVA {rva:#x} not in any section");
}

/// Return a copy of a managed PE with its embedded-portable-PDB debug entry
/// neutralised (its `Type` field zeroed), so `embedded_portable_pdb` no longer
/// finds one — modelling a build that ships only a sidecar `.pdb`. The CodeView
/// entry that points at the sidecar is left intact.
fn strip_embedded_pdb_entry(bytes: &[u8]) -> Vec<u8> {
    let mut b = bytes.to_vec();
    let pe = u32_at(&b, 0x3c) as usize;
    assert_eq!(&b[pe..pe + 4], b"PE\0\0");
    let coff = pe + 4;
    let nsec = u16_at(&b, coff + 2) as usize;
    let opt = coff + 20;
    let magic = u16_at(&b, opt);
    let (data_dirs, sec_off) = match magic {
        0x10b => (96usize, opt + 224),
        0x20b => (112usize, opt + 240),
        m => panic!("unexpected optional-header magic {m:#x}"),
    };
    let dd = opt + data_dirs + DEBUG_DIRECTORY * 8;
    let debug_rva = u32_at(&b, dd);
    let debug_size = u32_at(&b, dd + 4) as usize;
    let dir = rva_to_file(&b, sec_off, nsec, debug_rva);
    let mut zeroed = false;
    for i in 0..debug_size / DEBUG_ENTRY_SIZE {
        let e = dir + i * DEBUG_ENTRY_SIZE;
        if u32_at(&b, e + 12) == EMBEDDED_PORTABLE_PDB {
            b[e + 12..e + 16].copy_from_slice(&0u32.to_le_bytes());
            zeroed = true;
        }
    }
    assert!(
        zeroed,
        "FSharp.Core should carry an embedded-PDB debug entry"
    );
    b
}

#[test]
fn embedded_pdb_id_equals_the_dll_codeview_id() {
    // The matching scheme on real Roslyn data: the embedded PDB's `#Pdb` id
    // equals the GUID++stamp the DLL records in its CodeView entry. This is the
    // exact comparison `sidecar_pdb_matches` makes for a sidecar `.pdb`.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes)
        .expect("debug directory parses")
        .expect("FSharp.Core ships an embedded portable PDB");
    assert!(
        sidecar_pdb_matches(&bytes, &image),
        "the embedded image's id should match the DLL's CodeView id"
    );
    // A non-PDB blob (here the DLL itself) is not a match.
    assert!(!sidecar_pdb_matches(&bytes, &bytes));
}

#[test]
fn definition_source_reads_a_standalone_pdb_image() {
    // A sidecar `.pdb` file is exactly the portable-PDB image the embedded path
    // would inflate. Feeding the extracted image straight to the image-taking
    // core yields the same result as the embedded convenience wrapper.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let pdb = PortablePdb::read(&image).unwrap();
    let rid = (1..=pdb.method_debug_info_count())
        .find(|&rid| matches!(pdb.method_first_sequence_point(rid), Ok(Some(_))))
        .expect("some method has a sequence point");
    let token = 0x0600_0000 | rid;

    assert_eq!(
        definition_source_in_pdb(&image, token).unwrap(),
        definition_source(&bytes, token).unwrap(),
        "the standalone-image core agrees with the embedded wrapper"
    );
}

#[test]
fn pdb_image_for_prefers_the_embedded_pdb() {
    // When the DLL embeds its PDB, no sidecar is consulted: the result is the
    // embedded image, even with nothing on disk beside the DLL.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let embedded = embedded_portable_pdb(&bytes).unwrap().unwrap();
    assert_eq!(pdb_image_for(&dll, &bytes), Some(embedded));
}

#[test]
fn pdb_image_for_loads_a_matching_sidecar() {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let image = embedded_portable_pdb(&bytes).unwrap().unwrap();
    let stripped = strip_embedded_pdb_entry(&bytes);
    assert!(
        embedded_portable_pdb(&stripped).unwrap().is_none(),
        "stripping should remove the embedded entry"
    );

    // Lay the stripped DLL + the extracted image (as the named sidecar) in a
    // temp dir, exactly as a sidecar-PDB NuGet package would on disk.
    let dir = std::env::temp_dir().join(format!("borzoi-sidecar-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dll_path = dir.join("FSharp.Core.dll");
    let sidecar_name = sidecar_pdb_name(&stripped).expect("CodeView names a sidecar");
    std::fs::write(&dll_path, &stripped).unwrap();
    std::fs::write(dir.join(&sidecar_name), &image).unwrap();

    // Matching sidecar → its image; this is what feeds go-to-definition.
    assert_eq!(pdb_image_for(&dll_path, &stripped), Some(image.clone()));

    // A stale/foreign `.pdb` (id mismatch) is rejected: corrupt the sidecar's
    // `#Pdb` id and the loader declines rather than navigating to wrong source.
    let mut wrong = image.clone();
    let id_off = pdb_id_offset(&wrong);
    wrong[id_off] ^= 0xFF;
    std::fs::write(dir.join(&sidecar_name), &wrong).unwrap();
    assert_eq!(pdb_image_for(&dll_path, &stripped), None);

    // A missing sidecar is likewise nothing (not an error).
    std::fs::remove_file(dir.join(&sidecar_name)).unwrap();
    assert_eq!(pdb_image_for(&dll_path, &stripped), None);

    let _ = std::fs::remove_dir_all(&dir);
}

/// File offset of the 20-byte PDB id (head of the `#Pdb` stream) in a portable
/// PDB metadata image, by walking the `BSJB` stream headers.
fn pdb_id_offset(image: &[u8]) -> usize {
    assert_eq!(&image[..4], b"BSJB");
    let version_len = u32_at(image, 12) as usize;
    let mut p = 16 + version_len.div_ceil(4) * 4;
    p += 2; // Flags
    let streams = u16_at(image, p) as usize;
    p += 2;
    for _ in 0..streams {
        let off = u32_at(image, p) as usize;
        p += 8;
        let name_start = p;
        while image[p] != 0 {
            p += 1;
        }
        let name = &image[name_start..p];
        p += 1;
        p = p.div_ceil(4) * 4; // name padded to a 4-byte boundary
        if name == b"#Pdb" {
            return off; // id is the first 20 bytes of the stream
        }
    }
    panic!("no #Pdb stream");
}

//! Differential for documentation-comment IDs on **F#-compiled** assemblies,
//! against the keys the F# compiler itself wrote into each one's doc XML.
//!
//! For an F# assembly the shipped XML is ground truth by construction: fsc
//! computes every key with the functions FCS's `XmlDocSig` uses
//! (`XmlDocSigOfVal` and friends in `TypedTreeOps`), and the keys follow F#'s
//! own dialect rather than Roslyn's — record fields keyed `P:`, union cases
//! `T:`, SRTP witness parameters at the head of the parameter list, `[0:]` for a
//! two-dimensional array, settable properties keyed with the setter's argument.
//! The C# differential (`doc_id_diff.rs`) cannot see any of it.
//!
//! Each subject is graded the same way: **every shipped key must be among the
//! IDs [`walk_doc_ids`] generates**, and the keys that are not, together with
//! every ID two members both generate, are pinned in an exact per-subject
//! manifest under `tests/manifests/doc_id_fsharp/`. Movement in either
//! direction fails with a line diff; an intended movement is acknowledged by
//! regenerating:
//!
//! ```sh
//! BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-assembly --test all doc_id_fsharp_diff::
//! ```
//!
//! Alongside the manifest, one hard assertion that regeneration cannot bless:
//! fsc stores the key it computed for every documented val, record field and
//! union case in the assembly's **signature pickle** (`XmlDocSig`, pickled by
//! `p_ValData` / `p_recdfield_spec` / `p_unioncase_spec` after
//! `XmlDocWriter.ComputeXmlDocSigs` ran), so every shipped key that is not a
//! type's `T:` key must be one of the pickled strings. That is the premise the
//! generator's F# path stands on; if a compiler ever breaks it, this fails
//! rather than quietly measuring something else. Pickled strings the XML lacks
//! are pinned (`unwritten`), not asserted: fsc keys a method-impl'd member
//! without writing it.
//!
//! The subjects are host-independent: the purpose-built `DocIdsFs` fixture,
//! whose every declaration is documented so its manifest pins each key, and the
//! four F# libraries `tools/fcs-dump` references at versions its project pins.
//! [`nuget_cache_sweep`] widens the same grading to a pinned list of packages in
//! the NuGet cache, gated on an environment variable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use borzoi_assembly::doc_id::walk_doc_ids;
use borzoi_assembly::fsharp_pickle::model::{PickledExnRepr, PickledRecdField, PickledTyconRepr};
use borzoi_assembly::{Ecma335Assembly, EcmaView, ResourceKind, unpickle_signature};
use borzoi_oracle_harness::manifest::{self, Manifest};

use crate::common::{ensure_doc_ids_fs_built, fcs_dump_bin_dir};

const REGENERATE: &str = "BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-assembly \
     --test all doc_id_fsharp_diff:: -- --include-ignored";

/// How finely a subject's manifest pins the shipped keys.
#[derive(Clone, Copy)]
enum Pin {
    /// One line per shipped key, hit or miss — for the fixture, whose keys are
    /// the specification of what it exercises.
    EveryKey,
    /// The misses only, plus the hit count — for a real library, whose
    /// thousands of hits would bury the lines a reviewer needs to see.
    MissesOnly,
}

/// The `<member name="…">` keys of a doc XML. fsc writes each key verbatim
/// (`XmlDocWriter.WriteXmlDocFile` does not escape it), so a flat scan for the
/// attribute opener is exact.
fn shipped_keys(xml: &str) -> BTreeSet<String> {
    const OPEN: &str = "<member name=\"";
    let mut keys = BTreeSet::new();
    let mut rest = xml;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let end = after.find('"').expect("unterminated member name attribute");
        keys.insert(after[..end].to_string());
        rest = &after[end..];
    }
    keys
}

/// Every `XmlDocSig` the assembly's host signature pickle records — vals,
/// record/class fields, union cases and their fields, exception fields — or
/// why the pickle could not be read.
fn pickled_sigs(view: &Ecma335Assembly) -> Result<BTreeSet<String>, String> {
    fn fields(fs: &[PickledRecdField], out: &mut BTreeSet<String>) {
        out.extend(fs.iter().map(|f| f.xmldoc_sig.clone()));
    }
    let resources = view.fsharp_resources().map_err(|e| e.to_string())?;
    let primary = resources
        .iter()
        .find(|r| {
            matches!(
                r.kind,
                ResourceKind::SignatureData
                    | ResourceKind::SignatureCompressedData
                    | ResourceKind::SignatureDataFSharpCore
            )
        })
        .ok_or("no F# signature resource")?;
    let stream_b = resources
        .iter()
        .find(|r| {
            matches!(
                r.kind,
                ResourceKind::SignatureDataB | ResourceKind::SignatureCompressedDataB
            )
        })
        .map(|r| r.payload.as_slice());
    let ccu = unpickle_signature(&primary.payload, stream_b).map_err(|e| e.to_string())?;
    let mut sigs = BTreeSet::new();
    sigs.extend(ccu.tables.vals.iter().map(|v| v.xmldoc_sig.clone()));
    for e in &ccu.tables.tycons {
        let cases = match &e.repr {
            PickledTyconRepr::Record(fs) => {
                fields(fs, &mut sigs);
                None
            }
            PickledTyconRepr::FSharpObjectModel(o) => {
                fields(&o.rfields, &mut sigs);
                None
            }
            PickledTyconRepr::Union(cases) => Some(cases),
            PickledTyconRepr::UnionWithStaticFields { cases, objmodel } => {
                fields(&objmodel.rfields, &mut sigs);
                Some(cases)
            }
            _ => None,
        };
        for c in cases.into_iter().flatten() {
            sigs.insert(c.xmldoc_sig.clone());
            fields(&c.fields, &mut sigs);
        }
        if let PickledExnRepr::Fresh(fs) = &e.exn_repr {
            fields(fs, &mut sigs);
        }
    }
    // An undocumented item pickles an empty sig.
    sigs.remove("");
    Ok(sigs)
}

/// Grade one assembly against the doc XML beside it and return its manifest.
/// Panics on a broken premise (see the module docs) or a vacuous subject.
fn grade(dll: &Path, pin: Pin, min_keys: usize) -> Manifest {
    let xml_path = dll.with_extension("xml");
    let xml = std::fs::read_to_string(&xml_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", xml_path.display()));
    let shipped = shipped_keys(&xml);
    assert!(
        shipped.len() >= min_keys,
        "{}: expected at least {min_keys} shipped doc keys, got {} — the grading would be vacuous",
        xml_path.display(),
        shipped.len()
    );

    let bytes = std::fs::read(dll).unwrap_or_else(|e| panic!("read {}: {e}", dll.display()));
    let view =
        Ecma335Assembly::parse(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", dll.display()));
    let types = view
        .enumerate_type_defs()
        .unwrap_or_else(|e| panic!("enumerate {}: {e}", dll.display()));
    let mut generated: BTreeMap<String, usize> = BTreeMap::new();
    for entity in &types {
        walk_doc_ids(entity, None, &mut |id| {
            *generated.entry(id).or_default() += 1
        });
    }

    let mut entries = vec![format!("keys {}", shipped.len())];
    match pickled_sigs(&view) {
        Ok(sigs) => {
            entries.push("pickle decoded".to_string());
            let unpickled: Vec<&String> = shipped
                .iter()
                .filter(|k| !k.starts_with("T:") && !sigs.contains(*k))
                .collect();
            assert!(
                unpickled.is_empty(),
                "{}: shipped non-`T:` keys that the signature pickle does not record — the \
                 premise that fsc pickles every key it writes no longer holds:\n{unpickled:#?}",
                dll.display()
            );
            entries.extend(
                sigs.iter()
                    .filter(|s| !shipped.contains(*s))
                    .map(|s| format!("unwritten {s}")),
            );
        }
        Err(e) => entries.push(format!("pickle undecodable: {e}")),
    }
    let mut hits = 0usize;
    for key in &shipped {
        let hit = generated.contains_key(key);
        hits += usize::from(hit);
        match (pin, hit) {
            (_, false) => entries.push(format!("miss {key}")),
            (Pin::EveryKey, true) => entries.push(format!("hit {key}")),
            (Pin::MissesOnly, true) => {}
        }
    }
    if matches!(pin, Pin::MissesOnly) {
        entries.push(format!("hits {hits}"));
    }
    entries.extend(
        generated
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(id, n)| format!("dup {id} x{n}")),
    );
    Manifest::from_entries(entries).unwrap_or_else(|e| panic!("{}: {e}", dll.display()))
}

fn manifest_path(label: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("manifests")
        .join("doc_id_fsharp")
        .join(format!("{label}.txt"))
}

fn check(label: &str, dll: &Path, pin: Pin, min_keys: usize) {
    let actual = grade(dll, pin, min_keys);
    manifest::check(&manifest_path(label), &actual, REGENERATE);
}

/// The purpose-built fixture: one documented declaration per F# key shape.
#[test]
fn fixture() {
    check("DocIdsFs", ensure_doc_ids_fs_built(), Pin::EveryKey, 100);
}

#[test]
fn fsharp_core() {
    let dll = fcs_dump_bin_dir().join("FSharp.Core.dll");
    check("FSharp.Core", &dll, Pin::MissesOnly, 2000);
}

#[test]
fn fsharp_compiler_service() {
    let dll = fcs_dump_bin_dir().join("FSharp.Compiler.Service.dll");
    check("FSharp.Compiler.Service", &dll, Pin::MissesOnly, 5000);
}

#[test]
fn fsharp_dependency_manager_nuget() {
    let dll = fcs_dump_bin_dir().join("FSharp.DependencyManager.Nuget.dll");
    check("FSharp.DependencyManager.Nuget", &dll, Pin::MissesOnly, 20);
}

#[test]
fn fsharp_system_text_json() {
    let dll = fcs_dump_bin_dir().join("FSharp.SystemTextJson.dll");
    check("FSharp.SystemTextJson", &dll, Pin::MissesOnly, 50);
}

/// F#-compiled packages graded from the NuGet global-packages folder: one
/// FSharp.Core per release line (the oldest compilers whose pickles the
/// projection reads) and the F# libraries a measurement over the whole cache
/// found most key-dense. Paths are relative to the packages root.
const NUGET_SUBJECTS: &[&str] = &[
    "fsharp.core/4.7.2/lib/netstandard2.0/FSharp.Core.dll",
    "fsharp.core/5.0.0/lib/netstandard2.0/FSharp.Core.dll",
    "fsharp.core/6.0.0/lib/netstandard2.1/FSharp.Core.dll",
    "fsharp.core/7.0.400/lib/netstandard2.1/FSharp.Core.dll",
    "fsharp.core/8.0.100/lib/netstandard2.1/FSharp.Core.dll",
    "fsharp.core/9.0.100/lib/netstandard2.1/FSharp.Core.dll",
    "fsharp.core/10.0.100/lib/netstandard2.1/FSharp.Core.dll",
    "fsharp.core/10.1.401/lib/netstandard2.1/FSharp.Core.dll",
    "argu/6.1.1/lib/netstandard2.0/Argu.dll",
    "fantomas.fcs/7.0.3/lib/netstandard2.0/Fantomas.FCS.dll",
    "fscheck/3.4.0/lib/netstandard2.0/FsCheck.dll",
];

/// Set to the NuGet global-packages folder (`~/.nuget/packages`) to run
/// [`nuget_cache_sweep`].
const NUGET_ROOT_ENV: &str = "BORZOI_FSHARP_DOC_ID_NUGET_ROOT";

/// [`NUGET_SUBJECTS`], graded exactly like the always-on subjects, each against
/// its own manifest. Ignored and environment-gated because the packages live in
/// a host's NuGet cache; a subject absent from the cache fails the run, naming
/// it, rather than passing on less than it claims (restore it, e.g. with a
/// throwaway project referencing that package version).
///
/// ```sh
/// BORZOI_FSHARP_DOC_ID_NUGET_ROOT=~/.nuget/packages \
/// nix develop -c cargo test -p borzoi-assembly --test all doc_id_fsharp_diff::nuget_cache_sweep \
///   -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs BORZOI_FSHARP_DOC_ID_NUGET_ROOT pointing at a NuGet cache holding the pinned packages"]
fn nuget_cache_sweep() {
    let root = std::env::var_os(NUGET_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {NUGET_ROOT_ENV} to the NuGet global-packages folder"));
    let absent: Vec<&str> = NUGET_SUBJECTS
        .iter()
        .copied()
        .filter(|rel| !root.join(rel).is_file() || !root.join(rel).with_extension("xml").is_file())
        .collect();
    assert!(
        absent.is_empty(),
        "pinned subjects missing (dll or xml) under {}:\n{absent:#?}",
        root.display()
    );
    for rel in NUGET_SUBJECTS {
        let label = format!("nuget/{}", rel.trim_end_matches(".dll").replace('/', "_"));
        eprintln!("grading {rel}");
        check(&label, &root.join(rel), Pin::MissesOnly, 50);
    }
}

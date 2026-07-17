//! Differential test for documentation comment IDs (`doc_id` module) against a
//! real Roslyn-emitted `.xml`.
//!
//! The `DocIds` C# fixture documents every public member, so its build emits a
//! sidecar `DocIds.xml` whose `<member name="…">` keys *are* Roslyn's
//! documentation comment IDs. We enumerate the same assembly through our own
//! reader, generate an ID for every type/member with [`walk_doc_ids`], and
//! assert that **every** Roslyn key is one we produce. That is the property the
//! eventual hover lookup depends on: for any documented member, the ID we
//! compute keys into the file.
//!
//! The check is a subset (`roslyn ⊆ ours`), not equality: our reader also
//! surfaces members Roslyn never documents — implicit accessor methods, the
//! private auto-property backing field — which are harmless extras here.
//!
//! Requires the .NET 10 SDK on PATH (the Nix devShell provides it), like the
//! other `projector_*` fixtures.

use std::collections::BTreeSet;

use borzoi_assembly::doc_id::walk_doc_ids;
use borzoi_assembly::{Ecma335Assembly, EcmaView};

use crate::common::{ensure_doc_ids_built, ensure_fsharp_core_dll};

/// Every documentation comment ID our reader generates for `dll`, in
/// emission order — duplicates preserved, because injectivity over them is
/// itself under test (see [`duplicate_ids`]).
fn our_ids_with_duplicates(dll: &std::path::Path) -> Vec<String> {
    let bytes = std::fs::read(dll).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    let types = view.enumerate_type_defs().expect("enumerate fixture types");
    let mut ids = Vec::new();
    for entity in &types {
        walk_doc_ids(entity, None, &mut |id| {
            ids.push(id);
        });
    }
    ids
}

/// Every documentation comment ID our reader generates for the fixture
/// assembly (types and members, recursively through nested types).
fn our_ids(dll: &std::path::Path) -> BTreeSet<String> {
    our_ids_with_duplicates(dll).into_iter().collect()
}

/// The IDs emitted more than once for `dll`, with their multiplicities.
/// `floor` is the anti-vacuity bound: at least that many IDs must have been
/// emitted at all, so a mostly-skipped enumeration can't fake injectivity.
fn duplicate_ids(dll: &std::path::Path, floor: usize) -> Vec<(String, usize)> {
    let mut ids = our_ids_with_duplicates(dll);
    assert!(
        ids.len() >= floor,
        "expected at least {floor} generated IDs from {dll:?}, got {} — \
         the injectivity check would be vacuous",
        ids.len()
    );
    ids.sort();
    let mut dups = Vec::new();
    let mut i = 0;
    while i < ids.len() {
        let mut j = i + 1;
        while j < ids.len() && ids[j] == ids[i] {
            j += 1;
        }
        if j - i > 1 {
            dups.push((ids[i].clone(), j - i));
        }
        i = j;
    }
    dups
}

/// The `<member name="…">` keys Roslyn wrote into the sidecar `.xml`. Scans for
/// the unambiguous `<member name="` opener — the only place that exact prefix
/// occurs — rather than pulling in an XML parser for a flat key list.
fn roslyn_keys(xml: &str) -> BTreeSet<String> {
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

#[test]
fn every_roslyn_doc_id_is_generated() {
    let dll = ensure_doc_ids_built();
    let xml_path = dll.with_extension("xml");
    let xml = std::fs::read_to_string(&xml_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", xml_path.display()));

    let ours = our_ids(dll);
    let theirs = roslyn_keys(&xml);

    // Sanity: the fixture really did document a representative spread, so an
    // empty/looks-built-but-isn't xml can't make this pass vacuously.
    assert!(
        theirs.len() >= 25,
        "expected a rich DocIds.xml; got {} keys: {theirs:#?}",
        theirs.len()
    );

    let missing: Vec<&String> = theirs.difference(&ours).collect();
    assert!(
        missing.is_empty(),
        "Roslyn documentation comment IDs not reproduced by our generator:\n{missing:#?}\n\n\
         (our generated IDs were:\n{ours:#?})"
    );
}

/// Injectivity: no two distinct positions in the assembly may generate the
/// same documentation comment ID. The hover lookup keys the doc XML by these
/// IDs, so a collision means one member silently reads another's docs —
/// exactly the corruption-over-availability failure D5 forbids. (The
/// differential above collects into a `BTreeSet`, so it would swallow a
/// collision without this check.)
#[test]
fn doc_ids_are_injective_over_the_fixture() {
    let dups = duplicate_ids(ensure_doc_ids_built(), 25);
    assert!(
        dups.is_empty(),
        "distinct members generated the same documentation comment ID \
         (id, multiplicity): {dups:#?}"
    );
}

/// [`doc_ids_are_injective_over_the_fixture`], over the real shipped
/// FSharp.Core — the richest F#-shaped member surface available in every
/// lane, covering shapes the C# fixture cannot produce (modules, unions,
/// curried members, measure types).
#[test]
fn doc_ids_are_injective_over_fsharp_core() {
    let dll = ensure_fsharp_core_dll();
    let dups = duplicate_ids(&dll, 1000);
    assert!(
        dups.is_empty(),
        "distinct FSharp.Core members generated the same documentation \
         comment ID (id, multiplicity): {dups:#?}"
    );
}

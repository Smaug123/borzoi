//! What a resolved file **commits**, as one surface.
//!
//! Name resolution records into two range-keyed maps: ordinary occurrences, and
//! attribute types (kept apart because they answer FCS's suffix-first candidate
//! walk). Both are served — the LSP navigates an attribute name through the
//! second exactly as it navigates any other name — so anything asking "what does
//! this file claim here?" has to read both. A consumer that reads only
//! `resolution_at` sees a committed attribute answer as silence, and silence is
//! what a differential treats as *making no claim*: the answer goes uncompared,
//! and a wrong one is invisible.
//!
//! These are the FCS-free cases for that surface. The empirical half rides on
//! `borzoi-corpus-diff`, which reads `committed_resolution_at` and so diffs
//! real-world attribute uses against FCS.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, ResolvedFile, SyntaxRecovery, resolve_file};

fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {src:?}: {:?}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(
        &file,
        &ProjectItems::default(),
        &AssemblyEnv::default(),
        &recovery,
    )
}

/// A file declaring an attribute and using it, so the attribute map is
/// non-empty without needing a referenced assembly.
const ATTRIBUTED: &str = "\
module CommitSurface

type MarkAttribute () =
    inherit System.Attribute ()

[<Mark>]
type Widget =
    { Field : int }
";

#[test]
fn an_attribute_answer_is_absent_from_the_main_map() {
    let resolved = resolve(ATTRIBUTED);
    assert!(
        !resolved.attribute_resolutions().is_empty(),
        "the snippet is supposed to exercise the attribute map"
    );
    // The premise of the whole surface: asking the main map about an attribute
    // range is not a partial answer, it is no answer.
    for range in resolved.attribute_resolutions().keys() {
        assert_eq!(
            resolved.resolution_at(*range),
            None,
            "main map answers at attribute range {range:?}"
        );
    }
}

#[test]
fn the_commit_surface_answers_wherever_either_map_does() {
    let resolved = resolve(ATTRIBUTED);
    for (range, res) in resolved
        .resolutions()
        .iter()
        .chain(resolved.attribute_resolutions().iter())
    {
        assert_eq!(
            resolved.committed_resolution_at(*range),
            Some(*res),
            "commit surface loses the answer at {range:?}"
        );
    }
}

#[test]
fn the_two_commit_maps_answer_at_disjoint_ranges() {
    // The union is only unambiguous because the maps do not collide; the
    // resolver debug-asserts this on every file it finishes, and this pins the
    // property for a file that populates both.
    let resolved = resolve(ATTRIBUTED);
    for range in resolved.attribute_resolutions().keys() {
        assert!(
            !resolved.resolutions().contains_key(range),
            "both maps answer at {range:?}"
        );
    }
}

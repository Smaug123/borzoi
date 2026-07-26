//! What resolution may claim when the referenced-assembly projection is
//! **incomplete** — some DLL is present that the env could not read at all.
//!
//! Nothing read out of the assemblies is provably unshadowed in that state: the
//! missing DLL could declare a colliding type, could carry an assembly-level
//! `[<AutoOpen>]` bringing names into any namespace, and could *be* the CCU a
//! pickled reference names. So every assembly-rooted reading is demoted to
//! [`DeferredReason::IncompleteAssemblies`] — correctness over availability, the
//! same trade the abbreviation-target resolver already made for the same reason.
//!
//! The tests come in two shapes. The targeted ones pin *which* deferral a
//! specific use gets, so the LSP can explain it. The sweep pins the invariant
//! itself over every source here at once: under an incomplete env, no
//! `Entity`/`Member` survives anywhere — and, so the sweep cannot pass by
//! resolving nothing, each source must produce one under a *complete* env.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, DeferredReason, InferredFile, ProjectItems, Resolution, ResolvedFile, infer_file,
    resolve_file,
};
use rowan::TextRange;

use crate::common::full_bcl_env;

/// Sources that reach into the referenced assemblies in each of the three ways
/// a `Resolution` can be recorded: the resolver's own map (`Resolver::record`),
/// its attribute map, and inference's member map.
///
/// Each is checked to produce an assembly reading under a complete projection
/// before its incomplete half is believed, so a source that quietly stops
/// resolving takes the sweep down with it rather than passing vacuously.
const ASSEMBLY_READING_SOURCES: &[&str] = &[
    // A type-position path, recorded per segment.
    "module M\nlet f (x : System.String) = x\n",
    // A nested namespace, so more than one qualifier segment is walked.
    "module M\nlet f (x : System.Text.StringBuilder) = x\n",
    // A type reached through an `open` rather than spelled out.
    "module M\nopen System\nlet f (x : String) = x\n",
    // A static member: the whole dotted span records a `Member`, the type
    // segment an `Entity`.
    "module M\nlet x = System.String.Empty\n",
    // An enum case — the same shape through a different member kind.
    "module M\nlet e = System.StringComparison.Ordinal\n",
    // A bare attribute, which commits through the suffix candidate (a
    // *qualified* attribute path defers wholesale regardless — see
    // `attr_resolution_diff`).
    "module M\n[<Literal>]\nlet X = 5\n",
    "module M\nopen System\n[<Obsolete>]\nlet f () = ()\n",
    // A member reached only through inference: the receiver's type comes from
    // the literal, and `Length` is looked up on it.
    "module M\nlet n = \"hi\".Length\n",
];

/// The BCL env every test here reads through, and the same env marked
/// incomplete. Cloning before marking is what keeps the pair comparable: the
/// two differ in the flag and in nothing else.
fn complete_and_incomplete() -> (AssemblyEnv, AssemblyEnv) {
    let complete = full_bcl_env().clone();
    let mut incomplete = complete.clone();
    incomplete.mark_referenced_assemblies_incomplete();
    assert!(!complete.identities_incomplete());
    assert!(incomplete.identities_incomplete());
    (complete, incomplete)
}

fn resolve(src: &str, env: &AssemblyEnv) -> ResolvedFile {
    resolve_and_infer(src, env).0
}

fn resolve_and_infer(src: &str, env: &AssemblyEnv) -> (ResolvedFile, InferredFile) {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), env);
    let inferred = infer_file(&file, &resolved, env);
    (resolved, inferred)
}

/// Every resolution a file records, across all three surfaces.
fn all_resolutions(resolved: &ResolvedFile, inferred: &InferredFile) -> Vec<Resolution> {
    resolved
        .resolutions()
        .values()
        .chain(resolved.attribute_resolutions().values())
        .chain(inferred.member_resolutions().values())
        .copied()
        .collect()
}

fn is_assembly_reading(res: &Resolution) -> bool {
    matches!(res, Resolution::Entity(_) | Resolution::Member { .. })
}

fn at(hay: &str, needle: &str) -> TextRange {
    let start = hay
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {hay:?}"));
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + needle.len()).unwrap().into(),
    )
}

/// The invariant, over every source here at once: an incomplete projection
/// leaves no assembly-rooted reading standing, on any of the three recording
/// surfaces.
///
/// The complete-env half is not decoration. Without it the sweep would pass on
/// a resolver that resolved nothing at all — which is exactly the failure mode
/// a wholesale demote can hide.
#[test]
fn an_incomplete_projection_leaves_no_assembly_reading_standing() {
    let (complete, incomplete) = complete_and_incomplete();
    for src in ASSEMBLY_READING_SOURCES {
        let (r, i) = resolve_and_infer(src, &complete);
        assert!(
            all_resolutions(&r, &i).iter().any(is_assembly_reading),
            "the corpus entry must exercise an assembly reading under a complete \
             projection, else the incomplete half proves nothing: {src:?}"
        );

        let (r, i) = resolve_and_infer(src, &incomplete);
        let survivors: Vec<Resolution> = all_resolutions(&r, &i)
            .into_iter()
            .filter(is_assembly_reading)
            .collect();
        assert!(
            survivors.is_empty(),
            "assembly readings survived an incomplete projection in {src:?}: {survivors:?}"
        );
    }
}

/// A type-position name that reaches a referenced assembly must defer with the
/// reason that names the cause — not with a generic one, and not by vanishing.
/// A consumer distinguishes "no claim" from "no such name", and the LSP renders
/// the difference (see `handlers::definition_availability`).
#[test]
fn a_resolved_type_defers_with_the_incomplete_projection_reason() {
    let (complete, incomplete) = complete_and_incomplete();
    let src = "module M\nlet f (x : System.String) = x\n";
    let name = at(src, "String");

    let rf = resolve(src, &complete);
    assert!(
        matches!(rf.resolution_at(name), Some(Resolution::Entity(_))),
        "the complete env must resolve the type, else this pins nothing"
    );

    let rf = resolve(src, &incomplete);
    assert_eq!(
        rf.resolution_at(name),
        Some(Resolution::Deferred(DeferredReason::IncompleteAssemblies)),
    );
}

/// A static member records under the *whole* dotted span — a second resolution
/// kind in the same map, which a demote written only for `Entity` would leave
/// standing.
#[test]
fn a_resolved_static_member_defers_too() {
    let (complete, incomplete) = complete_and_incomplete();
    let src = "module M\nlet x = System.String.Empty\n";
    let path = at(src, "System.String.Empty");

    let rf = resolve(src, &complete);
    assert!(
        matches!(rf.resolution_at(path), Some(Resolution::Member { .. })),
        "the complete env must resolve the member, else this pins nothing"
    );

    let rf = resolve(src, &incomplete);
    assert_eq!(
        rf.resolution_at(path),
        Some(Resolution::Deferred(DeferredReason::IncompleteAssemblies)),
    );
}

/// An attribute resolution lives in its own map. Pinned separately because that
/// map also feeds `attributes_may_declare_extension`, whose `Deferred(_)` arm
/// makes the demote *increase* deferral there — the safe direction, and worth a
/// regression pin.
#[test]
fn an_attribute_naming_an_assembly_type_defers() {
    let (complete, incomplete) = complete_and_incomplete();
    let src = "module M\n[<Literal>]\nlet X = 5\n";
    let attr = at(src, "Literal");

    let rf = resolve(src, &complete);
    assert!(
        matches!(
            rf.attribute_resolution_at(attr),
            Some(Resolution::Entity(_))
        ),
        "the complete env must resolve the attribute, else this pins nothing"
    );

    let rf = resolve(src, &incomplete);
    assert_eq!(
        rf.attribute_resolution_at(attr),
        Some(Resolution::Deferred(DeferredReason::IncompleteAssemblies)),
    );
}

/// The inference surface: a member identified from the receiver's inferred type
/// is an assembly reading like any other, and defers with the rest.
#[test]
fn an_inferred_member_access_defers() {
    let (complete, incomplete) = complete_and_incomplete();
    let src = "module M\nlet n = \"hi\".Length\n";
    let member = at(src, "Length");

    let (_, inferred) = resolve_and_infer(src, &complete);
    assert!(
        matches!(
            inferred.member_resolution_at(member),
            Some(Resolution::Member { .. })
        ),
        "the complete env must identify the member, else this pins nothing"
    );

    let (_, inferred) = resolve_and_infer(src, &incomplete);
    assert_eq!(
        inferred.member_resolution_at(member),
        Some(Resolution::Deferred(DeferredReason::IncompleteAssemblies)),
    );
}

/// Inference still *types* what it typed. A missing DLL makes us unsure which
/// entity a name denotes; it does not make the program's structure unknown, and
/// the LSP navigates by resolution, not by type. Pinned so the seal is not
/// widened into the type surface by accident.
#[test]
fn inferred_types_survive_an_incomplete_projection() {
    let (complete, incomplete) = complete_and_incomplete();
    let src = "module M\nlet n = 1\n";

    let (_, under_complete) = resolve_and_infer(src, &complete);
    let (_, under_incomplete) = resolve_and_infer(src, &incomplete);
    assert!(
        !under_complete.types().is_empty(),
        "the complete env must type something, else this pins nothing"
    );
    assert_eq!(under_complete.types(), under_incomplete.types());
}

/// The demote is scoped to *assembly* readings: a name bound in the file's own
/// scope tree is unaffected by anything a missing DLL could declare, because
/// nothing an assembly brings into scope can shadow an in-file binder.
#[test]
fn in_file_bindings_are_untouched() {
    let (complete, incomplete) = complete_and_incomplete();
    let src = "module M\nlet x = 1\nlet y = x\n";
    let start = src.rfind('x').expect("use of x");
    let use_site = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + 1).unwrap().into(),
    );

    let under_complete = resolve(src, &complete).resolution_at(use_site);
    assert!(
        matches!(
            under_complete,
            Some(Resolution::Item(_) | Resolution::Local(_))
        ),
        "expected an in-file binder, got {under_complete:?}"
    );
    assert_eq!(
        resolve(src, &incomplete).resolution_at(use_site),
        under_complete,
        "an incomplete projection must not perturb an in-file binding"
    );
}

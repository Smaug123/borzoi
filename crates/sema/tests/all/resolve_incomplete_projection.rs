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
//! The contract, in one sentence: under an incomplete projection sema commits to
//! **no assembly-rooted resolution and no assembly-derived type**.
//!
//! The type half cannot be swept over the finished maps the way the resolution
//! half is — a `Ty::Named(["System", "Int32"])` carries no record of whether it
//! came from a member's return or from the literal `1`. So it is drawn at each
//! door an assembly-supplied type enters inference through: the two that read
//! the resolver's own map (an annotation head, a static call's rooting type) are
//! sealed by that map already, and `Gen::wake_member` declines its unify. Every
//! door leaves the variable open for the ground-only read-off to drop. The tests
//! at the end pin both sides of that line — what goes, what stays, and why the
//! LSP's single-file fallback cannot hand back what went.
//!
//! The tests come in two shapes. The targeted ones pin *which* deferral a
//! specific use gets, so the LSP can explain it. The two sweeps pin each half of
//! the contract over every source here at once — no `Entity`/`Member` survives,
//! and no type survives that an empty env could not have supplied — each with
//! the guard that keeps it from passing on a sema that says nothing at all.

use std::collections::BTreeMap;

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, DeferredReason, InferredFile, ProjectItems, Resolution, ResolvedFile, Ty,
    infer_file, resolve_file,
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
    // An instance *method* call, whose return type reaches inference by the same
    // wake but through the overload machinery rather than the data-member lookup.
    "module M\nlet u = \"hi\".ToUpperInvariant()\n",
    // A static call, whose receiver type is built from the resolver's rooting
    // `Entity` (`Gen::static_callee`) rather than from a literal.
    "module M\nlet b = System.String.IsNullOrEmpty \"hi\"\n",
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

/// The type half of the same invariant, stated so a *future* door cannot open
/// quietly: under an incomplete projection, inference must publish exactly what
/// it publishes with **no assemblies at all**.
///
/// An env holding nothing can supply nothing, so it is the oracle for "this type
/// did not come out of the assemblies" — a check that needs no per-type
/// provenance, which is precisely what `Ty` does not carry. Any new code path
/// that reads a type out of the env and unifies it in fails this the moment a
/// source here exercises it, rather than waiting for someone to notice.
///
/// Both guards matter. Without the *loses* assertion the sweep would pass on an
/// inference that published nothing anywhere; without the *keeps* one it would
/// pass on one that sealed types wholesale, which is the change that was tried
/// and reverted (`let n = 1` must still be `int`).
#[test]
fn an_incomplete_projection_publishes_only_what_no_assemblies_would() {
    let (complete, incomplete) = complete_and_incomplete();
    let empty = AssemblyEnv::default();
    let mut any_loses = false;
    let mut any_keeps = false;

    for src in ASSEMBLY_READING_SOURCES.iter().chain(ASSEMBLY_FREE_SOURCES) {
        let (rc, ic) = resolve_and_infer(src, &complete);
        let (ri, ii) = resolve_and_infer(src, &incomplete);
        let (re, ie) = resolve_and_infer(src, &empty);

        assert_eq!(
            published(&ri, &ii),
            published(&re, &ie),
            "an incomplete projection published a type an empty env could not \
             have supplied, so it came out of the assemblies: {src:?}"
        );

        let under_complete = published(&rc, &ic);
        any_loses |= under_complete != published(&ri, &ii);
        any_keeps |= !under_complete.is_empty() && under_complete == published(&ri, &ii);
    }

    assert!(
        any_loses,
        "no source lost a type to the seal — the sweep would pass vacuously"
    );
    assert!(
        any_keeps,
        "no source kept its types through the seal — the sweep would pass on a \
         wholesale demote"
    );
}

/// Every type this file published, in a form two runs over the same source can
/// be compared by: expression types keyed by range, binder types by binder name
/// (a `DefId` is an allocation index, not a stable identity across runs).
fn published(resolved: &ResolvedFile, inferred: &InferredFile) -> BTreeMap<String, String> {
    let exprs = inferred
        .types()
        .iter()
        .map(|(range, ty)| (format!("expr {range:?}"), ty.render()));
    let defs = inferred
        .def_types()
        .iter()
        .map(|(def, ty)| (format!("def {}", resolved.def(*def).name), ty.render()));
    exprs.chain(defs).collect()
}

/// Sources whose types owe the referenced assemblies nothing — literals and the
/// shapes built out of them. The seal must not cost these, so the sweep above
/// carries them alongside the assembly-reaching ones.
const ASSEMBLY_FREE_SOURCES: &[&str] = &[
    "module M\nlet n = 1\n",
    "module M\nlet s = \"hi\"\n",
    "module M\nlet p = (1, \"hi\")\n",
    "module M\nlet f c = if c then 1 else 2\n",
    "module M\nlet id x = x\n",
];

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

/// A type owing the assemblies nothing keeps its answer. `let n = 1` is `int`
/// because the literal says so; no DLL, read or unread, has a say in it — so
/// the seal must not cost it. The complement of the test below, and the reason
/// the seal is drawn at the *door* rather than over the type maps wholesale.
#[test]
fn a_type_owing_the_assemblies_nothing_keeps_its_answer() {
    let (complete, incomplete) = complete_and_incomplete();
    let src = "module M\nlet n = 1\n";
    let (_, under_complete) = resolve_and_infer(src, &complete);
    let (_, under_incomplete) = resolve_and_infer(src, &incomplete);
    assert!(
        !under_complete.def_types().is_empty(),
        "the complete env must type the binder, else this pins nothing"
    );
    assert_eq!(
        under_complete.def_types(),
        under_incomplete.def_types(),
        "binder types must not move"
    );
    assert_eq!(
        under_complete.types(),
        under_incomplete.types(),
        "expression types must not move"
    );
}

/// A type read **out of** an assembly member does not survive the seal, even
/// though nothing downstream can tell where it came from.
///
/// `let n = "hi".Length` publishes `int` under a complete projection. If the
/// unread DLL supplies a colliding `String` whose `Length` returns something
/// else, that `int` is wrong — so under an incomplete projection the binder
/// gets no type at all, and neither does the access expression. The receiver's
/// own literal type is untouched: `"hi"` is `System.String` because it is a
/// string literal, which no DLL can contradict.
#[test]
fn a_type_read_out_of_an_assembly_member_does_not_survive() {
    let (complete, incomplete) = complete_and_incomplete();
    let src = "module M\nlet n = \"hi\".Length\n";
    let binder = at(src, "n");
    let access = at(src, "\"hi\".Length");
    let literal = at(src, "\"hi\"");

    let def_of = |resolved: &ResolvedFile| {
        resolved
            .resolution_at(binder)
            .and_then(|res| resolved.resolved_def_id(res))
            .expect("the binder is recorded")
    };

    let (resolved, inferred) = resolve_and_infer(src, &complete);
    assert_eq!(
        inferred.def_type(def_of(&resolved)),
        Some(&Ty::named("System.Int32")),
        "the complete env must type the binder from the member's return, \
         else this pins nothing"
    );
    assert_eq!(inferred.type_at(access), Some(&Ty::named("System.Int32")));

    let (resolved, inferred) = resolve_and_infer(src, &incomplete);
    assert_eq!(
        inferred.def_type(def_of(&resolved)),
        None,
        "the binder's type came out of an assembly member, so it goes with the seal"
    );
    assert_eq!(
        inferred.type_at(access),
        None,
        "the access's type is the member's return type, so it goes too"
    );
    assert_eq!(
        inferred.type_at(literal),
        Some(&Ty::named("System.String")),
        "the receiver is typed by its own literal, which owes the assemblies nothing"
    );
}

/// The LSP's single-file hover fallback cannot republish what the seal above
/// dropped, which is what makes the guarantee hold end to end rather than only
/// inside sema.
///
/// `hover::handle` re-infers an orphan buffer against an *empty* env. That
/// fallback is why sealing inference *wholesale* was not viable — it would hand
/// `let n = 1` back its `int` anyway — but an assembly-derived type is exactly
/// what an empty env cannot produce: with no assemblies there is no entity to
/// look the member up on. Pinned here, in sema, because it is a property of
/// inference rather than of the handler.
#[test]
fn an_empty_env_cannot_republish_a_sealed_member_type() {
    let empty = AssemblyEnv::default();
    let src = "module M\nlet n = \"hi\".Length\n";
    let (resolved, inferred) = resolve_and_infer(src, &empty);
    let def = resolved
        .resolution_at(at(src, "n"))
        .and_then(|res| resolved.resolved_def_id(res))
        .expect("the binder is recorded");
    assert_eq!(inferred.def_type(def), None);
    assert_eq!(inferred.type_at(at(src, "\"hi\".Length")), None);
}

/// A type read *through* a sealed resolution goes with it, though — a
/// consequence of where that type came from, not a policy about types.
///
/// `annotation_ty` types an annotated binder by reading the resolution its
/// annotation recorded; sealed, there is no entity there to read, and the
/// binder's type is simply never derived. Pinned so the asymmetry with the test
/// above is deliberate and visible rather than folklore.
#[test]
fn a_type_read_through_a_sealed_resolution_goes_with_it() {
    let (complete, incomplete) = complete_and_incomplete();
    let src = "module M\nlet x : System.String = \"hi\"\n";
    let binder = at(src, "x");

    let (resolved, inferred) = resolve_and_infer(src, &complete);
    let def = resolved
        .resolution_at(binder)
        .and_then(|res| resolved.resolved_def_id(res))
        .expect("the annotated binder is recorded");
    assert!(
        inferred.def_type(def).is_some(),
        "the complete env must type the annotated binder, else this pins nothing"
    );

    let (resolved, inferred) = resolve_and_infer(src, &incomplete);
    let def = resolved
        .resolution_at(binder)
        .and_then(|res| resolved.resolved_def_id(res))
        .expect("the binder still binds — only its annotation was sealed");
    assert_eq!(inferred.def_type(def), None);
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

//! Module-open fold, Slice B: the **pattern surface** an `open` imports, as
//! projected from real fixture DLLs.
//!
//! FCS folds a module's complete contents on `open` — union cases (value and
//! pattern scope), exception constructors, active-pattern tags, nested type
//! names — where the owned model historically carried only members. These
//! tests pin the two projection channels that make those names enumerable:
//!
//! - `Entity::union_case_names`, lifted from the host signature pickle by
//!   `apply_union_case_names` (the ECMA image cannot recover case names:
//!   the `NewCase` constructors are `[CompilerGenerated]`, the nullary-case
//!   getters are dropped properties, and per-case carrier types exist only
//!   for one representation);
//! - active-pattern methods surviving with their verbatim banana name
//!   (`|Even|Odd|`), from which the sema layer derives the tags.

use std::path::Path;

use borzoi_assembly::{AbbreviationTarget, Ecma335Assembly, EcmaView, Entity, EntityKind, Member};

use crate::common::{
    ensure_fs_ext_index_built, ensure_minilib_fs_built, ensure_sig_hidden_union_built,
};

fn load(dll: &Path) -> Vec<Entity> {
    let bytes = std::fs::read(dll).expect("read fixture dll");
    Ecma335Assembly::parse(&bytes)
        .expect("parse fixture dll")
        .enumerate_type_defs()
        .expect("enumerate fixture types")
}

/// The entity named `name`, searched recursively.
fn entity_named<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
    fn find<'a>(entities: &'a [Entity], name: &str) -> Option<&'a Entity> {
        for e in entities {
            if e.name == name {
                return Some(e);
            }
            if let Some(found) = find(&e.nested_types, name) {
                return Some(found);
            }
        }
        None
    }
    find(entities, name).unwrap_or_else(|| panic!("entity {name:?} not found"))
}

#[test]
fn union_case_names_are_lifted_from_the_pickle_in_declaration_order() {
    let entities = load(ensure_minilib_fs_built());
    let choice = entity_named(&entities, "Choice");
    assert_eq!(choice.kind, EntityKind::Union);
    assert_eq!(
        choice.union_case_names.as_deref(),
        Some(&["Yes".to_string(), "No".to_string()][..])
    );
}

#[test]
fn an_rqa_union_still_carries_its_case_names() {
    // RQA gates what an `open` *imports* (FCS: `isILOrRequiredQualifiedAccess`
    // suppresses the cases from unqualified/pattern scope) — a consumer
    // decision. The projection stays complete: the names are the data the
    // consumer needs to make it.
    let entities = load(ensure_minilib_fs_built());
    let rqa = entity_named(&entities, "RqaUnion");
    assert!(rqa.is_require_qualified_access);
    assert_eq!(
        rqa.union_case_names.as_deref(),
        Some(&["A".to_string(), "B".to_string()][..])
    );
}

#[test]
fn an_exception_is_its_own_constructor_name() {
    // `exception MyError of string`: the importable constructor name is the
    // entity name itself — no per-case list exists or is needed.
    let entities = load(ensure_minilib_fs_built());
    let exn = entity_named(&entities, "MyError");
    assert_eq!(exn.kind, EntityKind::Exception);
    assert!(exn.union_case_names.is_none());
}

#[test]
fn a_union_with_static_members_keeps_its_cases() {
    // `UnionWithStaticFields` is the second case-bearing pickle
    // representation; a union that also declares a `static member` must not
    // lose its cases to the representation split.
    let entities = load(ensure_fs_ext_index_built());
    let tallied = entity_named(&entities, "Tallied");
    assert_eq!(tallied.kind, EntityKind::Union);
    assert_eq!(
        tallied.union_case_names.as_deref(),
        Some(&["Zero".to_string(), "Some'".to_string()][..])
    );
}

#[test]
fn a_plain_union_in_a_module_keeps_its_cases() {
    let entities = load(ensure_fs_ext_index_built());
    let verdict = entity_named(&entities, "Verdict");
    assert_eq!(verdict.kind, EntityKind::Union);
    assert_eq!(
        verdict.union_case_names.as_deref(),
        Some(&["Accepted".to_string(), "Rejected".to_string()][..])
    );
}

#[test]
fn a_private_representation_has_knowably_zero_accessible_cases() {
    // `type Concealed = private | Hidden of int`: the case is pickled with a
    // restricted `TAccess`, so a cross-assembly consumer can never name it.
    // The overlay must record the ACCESSIBLE list — here empty — and not the
    // private name (which would wrongly shadow a same-named earlier binding),
    // nor `None` (which reads as unknowable and forces residue).
    let entities = load(ensure_fs_ext_index_built());
    let concealed = entity_named(&entities, "Concealed");
    assert_eq!(concealed.kind, EntityKind::Union);
    assert_eq!(concealed.union_case_names.as_deref(), Some(&[][..]));
}

#[test]
fn a_signature_hidden_union_has_knowably_zero_accessible_cases() {
    // `type Teq<'a,'b>` is exposed opaquely by `Teq.fsi` while `Teq.fs` defines
    // it as a union: the F# compiler lowers the union repr to `TNoRepr` in the
    // signature pickle (`SignatureConformance`), so the case-name overlay's
    // union-repr walk never reaches it — yet the compiled class keeps
    // `CompilationMapping(SumType)`, so ECMA still classifies it a union. The
    // projector must seal it to the ACCESSIBLE list — empty, since the
    // representation is hidden — not `None` (which reads as unknowable and, via
    // the module-open fold, deferred every dotted head after `open`ing this
    // namespace: the `TypeEquality.Teq` regression). Distinct from
    // `Concealed` above, whose union repr IS pickled (with a private case) —
    // this one has no union repr in the signature at all.
    let entities = load(ensure_sig_hidden_union_built());
    let teq = entity_named(&entities, "Teq");
    assert_eq!(teq.kind, EntityKind::Union);
    assert_eq!(teq.union_case_names.as_deref(), Some(&[][..]));
}

#[test]
fn active_pattern_methods_keep_their_banana_names_verbatim() {
    // The IL method name IS the source form: `|Even|Odd|` / `|Positive|_|`.
    // Sema derives the tags by splitting; any mangling here would sever that.
    let entities = load(ensure_fs_ext_index_built());
    let module_ = entity_named(&entities, "PatternSurface");
    let method_names: Vec<&str> = module_
        .members
        .iter()
        .filter_map(|m| match m {
            Member::Method(mm) => Some(mm.source_name.as_deref().unwrap_or(&mm.name)),
            _ => None,
        })
        .collect();
    assert!(
        method_names.contains(&"|Even|Odd|"),
        "total active pattern must survive verbatim; got {method_names:?}"
    );
    assert!(
        method_names.contains(&"|Positive|_|"),
        "partial active pattern must survive verbatim; got {method_names:?}"
    );
}

#[test]
fn an_exception_abbreviation_gets_a_marker_child() {
    // `exception PatternAlias = PatternProblem` emits no ECMA TypeDef; only the
    // pickle knows the alias. A name-only marker (kind Exception) must appear
    // among the module's children so the open fold sees the constructor name
    // (codex round 22).
    let entities = load(ensure_fs_ext_index_built());
    let module_ = entity_named(&entities, "PatternSurface");
    let alias = module_
        .nested_types
        .iter()
        .find(|e| e.name == "PatternAlias")
        .expect("exception-abbreviation marker synthesized");
    assert_eq!(alias.kind, EntityKind::Exception);
    // The real exception is still its own (ECMA-backed) child.
    let real = entity_named(&entities, "PatternProblem");
    assert_eq!(real.kind, EntityKind::Exception);
}

#[test]
fn an_auto_open_abbreviation_marker_carries_the_attribute() {
    // `[<AutoOpen>] type TalliedAlias = Tallied`: the marker must carry
    // `is_auto_open`, or the fold reads the surface as complete while FCS
    // imports the target's statics (codex round 22).
    let entities = load(ensure_fs_ext_index_built());
    let module_ = entity_named(&entities, "PatternSurface");
    let alias = module_
        .nested_types
        .iter()
        .find(|e| e.name == "TalliedAlias")
        .expect("abbreviation marker synthesized");
    assert!(alias.is_auto_open, "the pickled [<AutoOpen>] must survive");
}

#[test]
fn a_same_assembly_abbreviation_target_decodes_its_nested_path_and_self_ccu() {
    // `[<AutoOpen>] type TalliedAlias = Tallied` (module `PatternSurface`): the
    // target is a *same-assembly* type, but fsc pickles even that as a *non-local*
    // ref whose ccu is `FsExtIndex` itself (a public signature is written to be
    // read from elsewhere). The decoder stores the ccu verbatim — a name alone
    // cannot be proven to mean the host rather than a same-named referenced
    // assembly, so disambiguation is the sema layer's job. The decoded target is
    // the type's full *nested* logical path (it lives in a module) with
    // `ccu = Some("FsExtIndex")`.
    let entities = load(ensure_fs_ext_index_built());
    let alias = entity_named(&entities, "TalliedAlias");
    assert_eq!(
        alias.abbreviation_target,
        Some(AbbreviationTarget::Named {
            ccu: Some("FsExtIndex".to_string()),
            path: vec![
                "FsExtIndex".to_string(),
                "PatternSurface".to_string(),
                "Tallied".to_string(),
            ],
            args: Vec::new(),
        }),
        "TalliedAlias must decode its same-assembly nested target with the verbatim self-ccu",
    );
}

#[test]
fn arity_overloaded_unions_keep_their_own_cases() {
    // `type Ambig = AmbigA` beside `[<RequireQualifiedAccess>] type Ambig<'T> =
    // AmbigB of 'T`: both CLR paths strip to `Ambig`, so a name-only overlay
    // match hands one union the other's cases (codex round 24). The final
    // segment must be keyed by (name, generic arity).
    let entities = load(ensure_fs_ext_index_built());
    let module_ = entity_named(&entities, "PatternSurface");
    let plain = module_
        .nested_types
        .iter()
        .find(|e| e.name == "Ambig" && e.generic_parameters.is_empty())
        .expect("non-generic Ambig");
    let generic = module_
        .nested_types
        .iter()
        .find(|e| e.name == "Ambig" && e.generic_parameters.len() == 1)
        .expect("arity-1 Ambig");
    assert_eq!(
        plain.union_case_names.as_deref(),
        Some(&["AmbigA".to_string()][..])
    );
    assert_eq!(
        generic.union_case_names.as_deref(),
        Some(&["AmbigB".to_string()][..])
    );
}

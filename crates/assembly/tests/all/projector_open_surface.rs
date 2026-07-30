//! Module-open fold, Slice B: the **pattern surface** an `open` imports, as
//! projected from real fixture DLLs.
//!
//! FCS folds a module's complete contents on `open` — union cases (value and
//! pattern scope), exception constructors, active-pattern tags, nested type
//! names — where the owned model historically carried only members. These
//! tests pin the two projection channels that make those names enumerable:
//!
//! - `Entity::union_cases`, lifted from the host signature pickle by
//!   `apply_union_cases` (the ECMA image cannot recover case names:
//!   the `NewCase` constructors are `[CompilerGenerated]`, the nullary-case
//!   getters are dropped properties, and per-case carrier types exist only
//!   for one representation);
//! - active-pattern methods surviving with their verbatim banana name
//!   (`|Even|Odd|`), from which the sema layer derives the tags.

use std::path::Path;

use borzoi_assembly::{
    AbbreviationTarget, Ecma335Assembly, EcmaView, Entity, EntityKind, Member, UnionCases,
};

use crate::common::{
    ensure_fs_ext_index_built, ensure_key_collision_built, ensure_minilib_fs_built,
    ensure_pre_visible_union_built, ensure_sig_hidden_union_built,
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
fn union_cases_are_lifted_from_the_pickle_in_declaration_order() {
    let entities = load(ensure_minilib_fs_built());
    let choice = entity_named(&entities, "Choice");
    assert_eq!(choice.kind, EntityKind::Union);
    assert_eq!(
        choice.union_cases,
        UnionCases::Known(vec!["Yes".to_string(), "No".to_string()])
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
        rqa.union_cases,
        UnionCases::Known(vec!["A".to_string(), "B".to_string()])
    );
}

#[test]
fn an_exception_is_its_own_constructor_name() {
    // `exception MyError of string`: the importable constructor name is the
    // entity name itself — no per-case list exists or is needed.
    let entities = load(ensure_minilib_fs_built());
    let exn = entity_named(&entities, "MyError");
    assert_eq!(exn.kind, EntityKind::Exception);
    assert_eq!(exn.union_cases, UnionCases::Unknowable);
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
        tallied.union_cases,
        UnionCases::Known(vec!["Zero".to_string(), "Some'".to_string()])
    );
}

#[test]
fn a_plain_union_in_a_module_keeps_its_cases() {
    let entities = load(ensure_fs_ext_index_built());
    let verdict = entity_named(&entities, "Verdict");
    assert_eq!(verdict.kind, EntityKind::Union);
    assert_eq!(
        verdict.union_cases,
        UnionCases::Known(vec!["Accepted".to_string(), "Rejected".to_string()])
    );
}

#[test]
fn a_private_representation_has_knowably_zero_accessible_cases() {
    // `type Concealed = private | Hidden of int`: the case is pickled with a
    // restricted `TAccess`, so a cross-assembly consumer can never name it.
    // The overlay must record the ACCESSIBLE list — here empty — and not the
    // private name (which would wrongly shadow a same-named earlier binding),
    // nor `Unknowable` (which forces residue).
    let entities = load(ensure_fs_ext_index_built());
    let concealed = entity_named(&entities, "Concealed");
    assert_eq!(concealed.kind, EntityKind::Union);
    assert_eq!(concealed.union_cases, UnionCases::Known(Vec::new()));
}

#[test]
fn a_signature_hidden_union_has_knowably_zero_accessible_cases() {
    // `type Teq<'a,'b>` is exposed opaquely by `Teq.fsi` while `Teq.fs` defines
    // it as a union: the F# compiler lowers the union repr to `TNoRepr` in the
    // signature pickle (`SignatureConformance`), so the case-name overlay's
    // union-repr walk never reaches it — yet the compiled class keeps
    // `CompilationMapping(SumType)`, so ECMA still classifies it a union. The
    // projector must seal it to the ACCESSIBLE list — empty, since the
    // representation is hidden — not `Unknowable` (which, via
    // the module-open fold, deferred every dotted head after `open`ing this
    // namespace: the `TypeEquality.Teq` regression). Distinct from
    // `Concealed` above, whose union repr IS pickled (with a private case) —
    // this one has no union repr in the signature at all.
    let entities = load(ensure_sig_hidden_union_built());
    let teq = entity_named(&entities, "Teq");
    assert_eq!(teq.kind, EntityKind::Union);
    assert_eq!(teq.union_cases, UnionCases::Known(Vec::new()));
}

/// Every property a union surfaces, in projection order.
fn union_property_names(entity: &Entity) -> Vec<&str> {
    entity
        .members
        .iter()
        .filter_map(|m| match m {
            Member::Property(p) => Some(p.name.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_union_surfaces_exactly_the_members_its_pickle_publishes() {
    // `Verdict = Accepted | Rejected` compiles to three properties — `IsAccepted`,
    // `IsRejected` and the `Tag` discriminant — carrying identical attributes
    // (`[CompilerGenerated]`, `[DebuggerNonUserCode]`, `[DebuggerBrowsable]`).
    // FCS surfaces the two testers and hides `Tag`, so nothing on the rows
    // themselves separates what to keep from what to drop. The host pickle's
    // published member list does, and it is what
    // `retain_published_union_properties` reads.
    let entities = load(ensure_fs_ext_index_built());
    let verdict = entity_named(&entities, "Verdict");
    assert_eq!(
        union_property_names(verdict),
        vec!["IsAccepted", "IsRejected"]
    );
}

#[test]
fn a_union_whose_testers_are_unpublished_surfaces_none() {
    // The soundness direction, and the one a metadata-only rule fails.
    // `PreVisibleUnion` is compiled under `<LangVersion>8.0</LangVersion>`: fsc
    // emits public `IsHeads` / `IsTails` property rows attributed exactly as a
    // current compiler's nameable testers are, but does not publish them, so
    // `coin.IsHeads` is `FS0039` for a consumer. Deriving the kept set from the
    // case names — which are present and correct here — surfaces both and hands
    // sema a member the F# compiler rejects.
    let entities = load(ensure_pre_visible_union_built());
    let coin = entity_named(&entities, "Coin");
    assert_eq!(coin.kind, EntityKind::Union);
    assert_eq!(
        coin.union_cases,
        UnionCases::Known(vec!["Heads".to_string(), "Tails".to_string()]),
        "precondition: the cases ARE known, so a case-derived rule would fire"
    );
    assert_eq!(union_property_names(coin), Vec::<&str>::new());
}

#[test]
fn a_union_naming_no_accessible_case_surfaces_no_tester() {
    // `Concealed = private | Hidden of int` seals to `Some([])` — knowably zero
    // ACCESSIBLE cases. fsc emits no `IsHidden` for a private representation, so
    // no tester is at stake here; `Tag` is what must not survive.
    //
    // A union the pickle never claimed at all (no pickle, an undecodable one, a
    // foreign CCU's union) keeps `UnionCases::Unknowable` and loses its
    // properties to `drop_unvouched_union_properties` instead. No fixture can
    // produce one: every F# fixture here has an authoritative host pickle.
    let entities = load(ensure_fs_ext_index_built());
    let concealed = entity_named(&entities, "Concealed");
    assert_eq!(concealed.union_cases, UnionCases::Known(Vec::new()));
    assert_eq!(union_property_names(concealed), Vec::<&str>::new());
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
        plain.union_cases,
        UnionCases::Known(vec!["AmbigA".to_string()])
    );
    assert_eq!(
        generic.union_cases,
        UnionCases::Known(vec!["AmbigB".to_string()])
    );
}

/// Every entity named `name`, in projection order, searched recursively.
fn entities_named<'a>(entities: &'a [Entity], name: &str) -> Vec<&'a Entity> {
    fn walk<'a>(entities: &'a [Entity], name: &str, out: &mut Vec<&'a Entity>) {
        for e in entities {
            if e.name == name {
                out.push(e);
            }
            walk(&e.nested_types, name, out);
        }
    }
    let mut out = Vec::new();
    walk(entities, name, &mut out);
    out
}

/// The `_unique_<case>` singleton backing fields fsc emits for a nullary case —
/// the only thing on a projected row that still says which *source* union it is
/// once a `[<CompiledName>]` has made two rows share a name and arity.
fn case_marker_fields(entity: &Entity) -> Vec<&str> {
    entity
        .members
        .iter()
        .filter_map(|m| match m {
            Member::Field(f) => f.name.strip_prefix("_unique_"),
            _ => None,
        })
        .collect()
}

/// The row a key-only match selects first must not be handed the *other*
/// union's cases.
///
/// [`arity_overloaded_unions_keep_their_own_cases`] fixed this defect at the
/// name-only level by adding generic arity to the key. `(name, arity)` is not
/// injective either: a `[<CompiledName>]` that fabricates a backtick-arity name
/// puts a second union at one key, and this is that shape. F# forbids two types
/// of one name and arity in a namespace, so nothing but a deliberate
/// `[<CompiledName>]` reaches it — but it compiles with no warning, and the
/// consequence is not a silence. `union_cases` is what
/// `AssemblyEnv::authoritative_union_case` answers "is this a case of that
/// union" from, and what `open_fold_surface` contributes as *bare* names, so a
/// misattached list is a wrong go-to-definition target with a source location.
#[test]
fn two_unions_at_one_projected_key_attach_no_cases() {
    let entities = load(ensure_key_collision_built());
    let rows = entities_named(&entities, "V");
    assert_eq!(
        rows.len(),
        2,
        "precondition: the fixture must actually collide — two rows named V, \
         from source `V` and from `W`'s [<CompiledName(\"V`0\")>]. Got {:?}",
        rows.iter().map(|e| e.name.as_str()).collect::<Vec<_>>()
    );
    for row in &rows {
        assert_eq!(row.kind, EntityKind::Union);
        assert!(row.generic_parameters.is_empty(), "both rows are arity 0");
    }

    // Identify each row by its own backing fields, since the names no longer can.
    let v = rows
        .iter()
        .find(|e| case_marker_fields(e) == ["X", "Y"])
        .expect("a row whose singleton fields are _unique_X / _unique_Y — source V");
    let w = rows
        .iter()
        .find(|e| case_marker_fields(e) == ["R", "S"])
        .expect("a row whose singleton fields are _unique_R / _unique_S — source W");

    // The pickle's case list fits either row as readily, so applying it to the
    // one the key happened to find first is a guess. `None` is the decline, and
    // every consumer already treats it as "unknowable" rather than "empty".
    assert_eq!(
        v.union_cases,
        UnionCases::Unknowable,
        "source V was handed a case list on an ambiguous key: {:?}",
        v.union_cases
    );
    assert_eq!(
        w.union_cases,
        UnionCases::Unknowable,
        "source W was handed a case list on an ambiguous key: {:?}",
        w.union_cases
    );
}

/// A **class** at a pickled union's key keeps the members it declares.
///
/// The property retention is destructive, so selecting a class here would strip
/// its own public members as "unvouched union candidates" — a member FCS
/// exposes, gone. `matches_union`'s `EntityKind::Union` requirement is what
/// stops it, and this is the only test of that.
#[test]
fn a_class_sharing_a_unions_projected_key_keeps_its_own_members() {
    let entities = load(ensure_key_collision_built());
    let rows = entities_named(&entities, "U");
    assert_eq!(
        rows.len(),
        2,
        "precondition: the fixture must actually collide — a class U and \
         `Other`'s [<CompiledName(\"U`0\")>] both project to the name U"
    );
    let class = rows
        .iter()
        .find(|e| e.kind == EntityKind::Class)
        .expect("the class row");
    let union = rows
        .iter()
        .find(|e| e.kind == EntityKind::Union)
        .expect("the union row");
    assert!(
        union_property_names(class).contains(&"P"),
        "the class lost the property it declares: {:?}",
        union_property_names(class)
    );
    assert_eq!(
        class.union_cases,
        UnionCases::Unknowable,
        "a class was handed a union's cases"
    );
    // The union at that key is unambiguous — one union row — so it keeps both
    // its cases and its published testers.
    assert_eq!(
        union.union_cases,
        UnionCases::Known(vec!["A".to_string(), "B".to_string()])
    );
    assert_eq!(union_property_names(union), vec!["IsA", "IsB"]);
}

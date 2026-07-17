//! End-to-end projection of the real, shipped `FSharp.Core.dll` through
//! `Ecma335Assembly::enumerate_type_defs`, and the pickle overlays it drives.
//!
//! Until the signature reader modelled bounded multi-dimensional arrays
//! (`ELEMENT_TYPE_ARRAY` with a shape) and pointers (`ELEMENT_TYPE_PTR`,
//! including `void*`), this walk died on FSharp.Core's member signatures with
//! `UnsupportedElement(0x14)` / `(0x0f)` / `UnexpectedVoid` before any entity
//! projected. With the reader complete, the whole tree projects and the pickle
//! overlays (source names, measure) run against the *real* target rather than
//! only the MiniLibFs fixture — these tests pin that they recover the right F#
//! facts from the genuine assembly (e.g. `printfn` ⇐ the IL `PrintFormatLine`).
//!
//! These are stable F# API facts (FSharp.Core's public surface), so the exact
//! source names are pinned; the version-sensitive counts use lower bounds.
//!
//! Requires the .NET 10 SDK on PATH (to build `tools/fcs-dump` once, which
//! drops the `FSharp.Core.dll` this reads); the Nix devShell provides it.

use borzoi_assembly::pdb::{PortablePdb, embedded_portable_pdb};
use borzoi_assembly::{
    Augmentation, Ecma335Assembly, EcmaView, Entity, EntityKind, Member, MethodLike, ParamDefault,
};

use crate::common::ensure_fsharp_core_dll;

fn load() -> Vec<Entity> {
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("Ecma335Assembly::parse FSharp.Core");
    view.enumerate_type_defs()
        .expect("FSharp.Core must project end-to-end (no unsupported signature element)")
}

/// The top-level entity at `namespace::name`.
fn entity<'a>(es: &'a [Entity], namespace: &[&str], name: &str) -> &'a Entity {
    es.iter()
        .find(|e| {
            e.name == name
                && e.namespace
                    .iter()
                    .map(String::as_str)
                    .eq(namespace.iter().copied())
        })
        .unwrap_or_else(|| panic!("entity {}.{name} not found", namespace.join(".")))
}

/// The method of `e` whose recovered F# `source_name` is `source`.
fn method_by_source<'a>(e: &'a Entity, source: &str) -> &'a MethodLike {
    e.members
        .iter()
        .find_map(|m| match m {
            Member::Method(m) if m.source_name.as_deref() == Some(source) => Some(m),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no member with source_name {source:?} on {}", e.name))
}

#[test]
fn fsharp_core_enumerates_end_to_end() {
    let entities = load();
    // FSharp.Core ships well over a hundred top-level types/modules; a couple of
    // dozen would mean the walk bailed early. (The measure / extension overlays
    // also run here, so a clean `Ok` additionally proves they did not hard-error
    // against the real assembly.)
    assert!(
        entities.len() > 100,
        "expected > 100 top-level entities; got {}",
        entities.len()
    );
}

#[test]
fn fsharp_core_methods_have_unknown_arg_group_count() {
    // OV-6.1: FSharp.Core is an F# assembly (it carries a host F# signature
    // pickle), so a curried `member x.M a b` is indistinguishable from a tupled
    // `member x.M(a, b)` in its flattened MethodDef signatures.
    // `enumerate_with_skips_impl` therefore blanks *every* method's
    // `arg_group_count` to `None`, and the overload engine treats an unknown
    // grouping as possibly curried. Assert the blanking reached the whole tree
    // (nested types included). See `docs/completed/ov-6.1-curry-detection-plan.md`.
    fn assert_all_none(entities: &[Entity]) {
        for e in entities {
            for m in &e.members {
                if let Member::Method(m) = m {
                    assert_eq!(
                        m.arg_group_count, None,
                        "F# method {}.{} should have arg_group_count == None",
                        e.name, m.name
                    );
                }
            }
            assert_all_none(&e.nested_types);
        }
    }
    assert_all_none(&load());
}

#[test]
fn fsharp_core_classifies_fsharp_optional_parameters() {
    // FSharp.Core uses F# `?optional` parameters (e.g.
    // `Async.RunSynchronously(?timeout, ?cancellationToken)`); the projector must
    // classify at least one as `FSharpOptional` from `[<OptionalArgument>]`,
    // distinct from a .NET `[Optional]`. A targeted scan (not a full-tree diff),
    // so version-robust and independent of the inner-position nullability walk.
    let entities = load();
    let optionals = entities
        .iter()
        .flat_map(|e| &e.members)
        .filter_map(|m| match m {
            Member::Method(method) => Some(method),
            _ => None,
        })
        .flat_map(|m| &m.signature.parameters)
        .filter(|p| p.default == ParamDefault::FSharpOptional)
        .count();
    assert!(
        optionals > 0,
        "FSharp.Core should expose F# optional (?x) parameters; found none"
    );
}

#[test]
fn fsharp_core_module_suffix_source_names() {
    let entities = load();
    // `ListModule` (the IL class for the `List` module, which shares its name
    // with the `list` type) carries `[CompilationRepresentation(ModuleSuffix)]`;
    // its F# source name is the IL name minus `"Module"`.
    let list = entity(
        &entities,
        &["Microsoft", "FSharp", "Collections"],
        "ListModule",
    );
    assert!(matches!(list.kind, EntityKind::Module));
    assert_eq!(list.source_name.as_deref(), Some("List"));

    // A non-suffixed module keeps its name (source_name stays `None`).
    let operators = entity(&entities, &["Microsoft", "FSharp", "Core"], "Operators");
    assert_eq!(operators.source_name, None);
}

#[test]
fn fsharp_core_printf_member_source_names() {
    // The headline payoff: the auto-opened printf surface resolves its F# source
    // names from the pickle on the *real* assembly. Each compiles to a renamed IL
    // method (`PrintFormatLine` etc.), and the overlay recovers the source name.
    let entities = load();
    let extra = entity(
        &entities,
        &["Microsoft", "FSharp", "Core"],
        "ExtraTopLevelOperators",
    );
    for source in ["printfn", "sprintf", "eprintfn", "fprintfn"] {
        let m = method_by_source(extra, source);
        // The owned `name` stays the IL/compiled name (what the Roslyn
        // differential compares); `source_name` is the additional F# name.
        assert_ne!(
            m.name, source,
            "{source} should be renamed at IL (its `name` is the compiled name)"
        );
        assert!(
            m.name.starts_with("PrintFormat"),
            "{source} compiles to a PrintFormat* IL method; got {:?}",
            m.name
        );
    }
}

#[test]
fn fsharp_core_auto_open_modules() {
    let entities = load();
    // The implicit-open surface: FSharp.Core marks these modules `[<AutoOpen>]`
    // so their members are in scope unqualified (this is what makes `printfn`
    // resolvable without an `open`).
    for name in ["Operators", "ExtraTopLevelOperators"] {
        let m = entity(&entities, &["Microsoft", "FSharp", "Core"], name);
        assert!(m.is_auto_open, "{name} must be [<AutoOpen>]");
    }
}

#[test]
fn fsharp_core_measure_types_recovered() {
    let entities = load();
    // The measure overlay upgrades `[<Measure>] type` entities from `Class` to
    // `Measure` using the pickle's `TyparKind::Measure`. FSharp.Core ships the
    // SI unit measures; a clean projection means the overlay matched them all
    // (a mismatch would have hard-errored in `enumerate_type_defs`).
    let measures = entities
        .iter()
        .filter(|e| matches!(e.kind, EntityKind::Measure))
        .count();
    assert!(
        measures > 0,
        "expected the measure overlay to recover [<Measure>] types from FSharp.Core"
    );
}

#[test]
fn printfn_metadata_token_indexes_its_pdb_source() {
    // The slice-6 payoff: a *projected* method's `metadata_token` is the real
    // `MethodDef` token, so it indexes the PDB's parallel `MethodDebugInformation`
    // table and lands on the method's source. This is the bridge go-to-definition
    // crosses from a resolved member to a source location.
    let dll = ensure_fsharp_core_dll();
    let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
    let view = Ecma335Assembly::parse(&bytes).expect("parse FSharp.Core");
    let entities = view.enumerate_type_defs().expect("enumerate FSharp.Core");

    let extra = entity(
        &entities,
        &["Microsoft", "FSharp", "Core"],
        "ExtraTopLevelOperators",
    );
    let printfn = method_by_source(extra, "printfn");

    // It's a `MethodDef` token: tag 0x06 in the high byte, nonzero rid below.
    assert_eq!(
        printfn.metadata_token >> 24,
        0x06,
        "metadata_token should be a MethodDef token; got {:#010x}",
        printfn.metadata_token
    );
    let rid = printfn.metadata_token & 0x00FF_FFFF;
    assert!(rid != 0, "MethodDef rid must be nonzero");

    // That rid indexes the same DLL's embedded PDB and lands on the file where
    // `printfn` is actually defined — `fslib-extra-pervasives.fs`, the
    // `ExtraTopLevelOperators` module (the `Printf` *module* internals live in
    // printf.fs). Hitting the real definition site proves the token is the
    // correct row, not merely a well-formed MethodDef token.
    let image = embedded_portable_pdb(&bytes)
        .expect("debug directory parses")
        .expect("FSharp.Core ships an embedded portable PDB");
    let pdb = PortablePdb::read(&image).expect("parse the portable-PDB metadata image");
    let sp = pdb
        .method_first_sequence_point(rid)
        .expect("sequence points decode")
        .expect("printfn's method carries sequence points");
    let document = pdb
        .document_name(sp.document)
        .expect("the referenced document resolves");
    assert!(
        document.ends_with("fslib-extra-pervasives.fs"),
        "printfn's metadata_token should map to its definition file \
         fslib-extra-pervasives.fs; got {document}"
    );
}

/// The method of `e` named exactly `name` (IL name).
fn methods_named<'a>(e: &'a Entity, name: &str) -> Vec<&'a MethodLike> {
    e.members
        .iter()
        .filter_map(|m| match m {
            Member::Method(m) if m.name == name => Some(m),
            _ => None,
        })
        .collect()
}

/// Slice C (pickle-driven member list): a *generic* F#-native extension member
/// now carries `is_extension_method` from the pickled val's
/// `IsExtensionMember ∧ IsInstance` — the §7 gap the per-method overlay could
/// not close because it skipped generic vals (it had no way to arity-match
/// them against IL).
///
/// Two real shapes, both previously unflagged:
/// - `TaskBuilderExtensions.MediumPriority`'s five `TaskBuilder.MergeSources`
///   overloads — generic *methods* extending a non-generic builder, and a
///   same-`(name, arity)` overload set to boot (the flag must survive the
///   collision because the val facts are unanimous).
/// - `LazyExtensions`'s `Force` — an extension on a *generic target* (the
///   target's typar is lifted onto the method; the IL name is plain `Force`
///   because FSharp.Core gives it an explicit `[<CompiledName>]`). Its
///   sibling `Create` is a *static* extension and must stay unflagged
///   (FCS's `IsInstanceMember` gate) — the instance/static split sema's
///   bare-static filter (autoopen plan ⚠) will consume.
#[test]
fn fsharp_core_generic_extension_members_are_flagged() {
    let entities = load();

    let medium = entity(
        &entities,
        &["Microsoft", "FSharp", "Control", "TaskBuilderExtensions"],
        "MediumPriority",
    );
    let merge_sources = methods_named(medium, "TaskBuilder.MergeSources");
    assert_eq!(
        merge_sources.len(),
        5,
        "MediumPriority projects five TaskBuilder.MergeSources overloads"
    );
    for m in &merge_sources {
        assert!(
            m.is_extension_method,
            "TaskBuilder.MergeSources (arity {}) must be extension-flagged from the pickle",
            m.signature.parameters.len()
        );
        assert!(
            !m.generic_parameters.is_empty(),
            "MergeSources is a generic method"
        );
    }

    let lazy_ext = entity(
        &entities,
        &["Microsoft", "FSharp", "Control"],
        "LazyExtensions",
    );
    let force = methods_named(lazy_ext, "Force");
    assert!(!force.is_empty(), "LazyExtensions projects Force");
    for m in &force {
        assert!(
            m.is_extension_method,
            "Force is an instance extension on a generic target"
        );
    }
    for name in ["Create", "CreateFromValue"] {
        let statics = methods_named(lazy_ext, name);
        assert!(!statics.is_empty(), "LazyExtensions projects {name}");
        for m in &statics {
            assert!(
                !m.is_extension_method,
                "{name} is a *static* extension: never surface-flagged (IsInstanceMember gate)"
            );
        }
    }
}

/// The two extension facts on real FSharp.Core's `LazyExtensions`, which augments
/// `System.Lazy<'T>` with the instance `Force` and the statics
/// `Create`/`CreateFromValue`:
///
/// - `is_extension_method` (the *surface*, instance-callable flag) holds for `Force`
///   only — FCS's `IsInstanceMember` gate, which the overload engine relies on: a
///   static augmentation is not instance-callable.
/// - `is_fsharp_extension_member` (the augmentation flag) holds for **all three** —
///   none is reachable bare or module-qualified (both FS0039), which is what sema's
///   name-resolution filter reads.
///
/// The instance name index keeps its `IsInstanceMember`-gated contents for the
/// overload absence gate.
#[test]
fn fsharp_core_augmentation_flags_split_surface_from_name_resolution() {
    let entities = load();
    let lazy_ext = entity(
        &entities,
        &["Microsoft", "FSharp", "Control"],
        "LazyExtensions",
    );
    for m in methods_named(lazy_ext, "Force") {
        assert!(m.is_extension_method, "Force is an instance augmentation");
        assert!(
            m.augmentation == Augmentation::Certain,
            "…and an F#-native augmentation"
        );
    }
    for name in ["Create", "CreateFromValue"] {
        for m in methods_named(lazy_ext, name) {
            assert!(
                !m.is_extension_method,
                "{name} is a *static* augmentation: never surface-flagged"
            );
            assert!(
                m.augmentation == Augmentation::Certain,
                "{name} is still an F#-native augmentation: hidden from bare and \
                 module-qualified lookups alike"
            );
        }
    }
    assert_eq!(
        lazy_ext.extension_member_names,
        vec!["Force".to_string()],
        "the instance name index (overload absence gate) stays IsInstanceMember-gated"
    );
    // EX-0: the **static** sibling index, on the same real assembly. The two lists
    // pin each other — `Lazy<'T>`'s extension surface is one instance member
    // (`Force`) and two statics (`Create`/`CreateFromValue`) — and the static list
    // is what a name-keyed extension gate must consult on a *type-qualified static*
    // call (`Lazy.Create …`), which the IsInstance-gated list above cannot answer.
    //
    // This is not hypothetical: FSharp.Core's `FSharpReflectionExtensions` declares
    // 17 static extensions (`GetRecordFields`, `GetUnionCases`, …) on `FSharpType` /
    // `FSharpValue`, names a real static call *does* write. A name-keyed gate reading
    // only the instance index would report "no extension named GetRecordFields" and
    // commit an intrinsic FCS may not have chosen.
    assert_eq!(
        lazy_ext.static_extension_member_names,
        vec!["Create".to_string(), "CreateFromValue".to_string()],
        "the static name index carries the `type Lazy<'T> with static member …` \
         extensions the instance index (rightly) excludes"
    );
}

/// Slice C: the module member *list* is val-driven on the authoritative path,
/// so IL-only artefacts can no longer appear — no `$W` witness twin survives
/// in `Operators` (they are not vals), and the members that do appear keep the
/// val-supplied facts the overlays used to patch in: `printfn`'s source name
/// and `async`'s value-binding shape.
#[test]
fn fsharp_core_module_member_list_is_val_driven() {
    let entities = load();

    let operators = entity(&entities, &["Microsoft", "FSharp", "Core"], "Operators");
    let witness_twins: Vec<&str> = operators
        .members
        .iter()
        .filter_map(|m| match m {
            Member::Method(m) if m.name.ends_with("$W") => Some(m.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        witness_twins.is_empty(),
        "witness twins must not surface as module members: {witness_twins:?}"
    );
    // A generic operator val still projects (the member list keeps generics).
    assert!(
        !methods_named(operators, "op_Addition").is_empty(),
        "op_Addition must stay in the member list"
    );

    let printf = entity(&entities, &["Microsoft", "FSharp", "Core"], "PrintfModule");
    let printfn = method_by_source(printf, "printfn");
    assert_eq!(printfn.name, "PrintFormatLine");

    let extra = entity(
        &entities,
        &["Microsoft", "FSharp", "Core"],
        "ExtraTopLevelOperators",
    );
    let async_val = method_by_source(extra, "async");
    assert_eq!(async_val.name, "DefaultAsyncBuilder");
    assert!(
        async_val.module_value.is_some(),
        "`async` is a value binding (rebranded static property)"
    );
}

/// Whole-assembly sweep of the pickle-driven member list on real FSharp.Core:
/// the claim pass must account for every module val and every projected
/// member — a skip recorded by the pass means either a val shape we
/// mis-classified (the review-caught `typeof`/`sizeof`/`defaultof` zero-group
/// *generic* vals, which compile to generic MethodDefs, not properties) or a
/// public IL member the pickle genuinely does not describe. Lambda-lifted
/// internal helpers (`concatArray@29`) are *expected* IL-only leftovers — the
/// signature pickle never describes them — and must be retained silently, not
/// recorded.
#[test]
fn fsharp_core_member_list_pass_leaves_no_skips() {
    let entities = load();

    fn collect_pass_skips<'a>(es: &'a [Entity], out: &mut Vec<(String, &'a str, &'a str)>) {
        for e in es {
            for s in &e.skipped_members {
                if s.reason.contains("pickle-driven member list")
                    || s.reason.contains("no matching projected IL member")
                {
                    out.push((e.name.clone(), s.name.as_str(), s.reason.as_str()));
                }
            }
            collect_pass_skips(&e.nested_types, out);
        }
    }
    let mut skips = Vec::new();
    collect_pass_skips(&entities, &mut skips);
    assert!(
        skips.is_empty(),
        "the member-list pass must account for every val/member on FSharp.Core; \
         got {} skips, first 20: {:#?}",
        skips.len(),
        &skips[..skips.len().min(20)]
    );

    // The zero-group *generic* vals the review caught: generic MethodDefs, not
    // properties. They must survive the cutover.
    let operators = entity(&entities, &["Microsoft", "FSharp", "Core"], "Operators");
    for name in ["TypeOf", "TypeDefOf", "SizeOf"] {
        assert!(
            !methods_named(operators, name).is_empty(),
            "Operators.{name} (zero-group generic val) must stay projected"
        );
    }
    // `Unchecked` is a module declared *inside* the `Operators` module, so it
    // projects as a nested TypeDef.
    let unchecked = operators
        .nested_types
        .iter()
        .find(|e| e.name == "Unchecked")
        .expect("Operators nests the Unchecked module");
    assert!(
        !methods_named(unchecked, "DefaultOf").is_empty(),
        "Unchecked.DefaultOf must stay projected"
    );

    // A lambda-lifted internal helper stays in the member list (pre-cutover
    // behaviour): it is IL-only by design, not uncertainty. `String.concat`'s
    // `concatArray@29` closure lives on `StringModule` as a non-public
    // method.
    let string_module = entity(&entities, &["Microsoft", "FSharp", "Core"], "StringModule");
    assert!(
        string_module.members.iter().any(|m| matches!(
            m,
            Member::Method(m) if m.name.contains('@')
        )),
        "StringModule retains its lambda-lifted internal helpers"
    );
}

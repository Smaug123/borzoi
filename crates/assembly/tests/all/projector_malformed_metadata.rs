//! Defensive "fails-loud" projector tests asserted against a real PE whose
//! metadata is fabricated by the in-box `System.Reflection.Metadata` emitter
//! (`tests/fixtures/assembly/MetadataEmitter`). Each shape is one no C#/F#
//! compiler emits — producing it needs an assembler that writes the raw
//! signature / table bytes — so the assertion is that parsing and enumerating
//! the assembly (`Ecma335Assembly::parse` then enumeration) *refuses* it rather
//! than silently mismodelling it.
//!
//! The refusal is now *localized*: per the reader plan's "bound uncertainty", a
//! single undecodable member or type is dropped and **recorded** (on
//! [`Entity::skipped_members`] or the dropped-type list of
//! `enumerate_type_defs_with_skips`) rather than aborting the whole enumeration.
//! So [`refusal_reasons`] gathers the refusal from wherever it surfaces — a
//! propagated error *or* a recorded drop — and the `expect_*` helpers assert it
//! appears there. What still propagates as a hard error is genuine structural
//! corruption (a cyclic nesting chain) or a parse-time refusal; those flow
//! through [`refusal_reasons`] all the same.
//!
//! These pin the projector's refusal against the *byte* path (parse + project):
//! they drive `Ecma335Assembly::parse` over a real PE, exercising the in-crate
//! ECMA-335 reader end to end rather than a fabricated in-memory model.
//!
//! Requires the .NET 10 SDK on PATH — the Nix devShell provides it.

use crate::common;

use borzoi_assembly::{Ecma335Assembly, EcmaView, Entity, Member, Primitive, TypeRef};

/// Every "refusal reason" a `shape` produces, each rendered as the `Display` of
/// the underlying `ImportError`: a parse-time error, a whole-image enumeration
/// error, a dropped whole-type ([`Ecma335Assembly::enumerate_type_defs_with_skips`]'s
/// `dropped_types` list), or a per-type dropped member
/// ([`Entity::skipped_members`]).
///
/// Per-member "bound uncertainty" (the reader plan) means a single undecodable
/// member or type no longer *sinks* the whole assembly: it is dropped and
/// **recorded** rather than propagated. So these tests assert the projector
/// refuses a bad shape *somewhere* — as an error, or as a recorded drop — not
/// that the refusal aborts the entire enumeration.
fn refusal_reasons(bytes: &[u8]) -> Vec<String> {
    let view = match Ecma335Assembly::parse(bytes) {
        Ok(v) => v,
        Err(e) => return vec![e.to_string()],
    };
    let (entities, skips) = match view.enumerate_type_defs_with_skips() {
        Ok(pair) => pair,
        Err(e) => return vec![e.to_string()],
    };
    let mut reasons: Vec<String> = skips.dropped_types.into_iter().map(|s| s.reason).collect();
    fn walk(e: &Entity, out: &mut Vec<String>) {
        out.extend(e.skipped_members.iter().map(|s| s.reason.clone()));
        for n in &e.nested_types {
            walk(n, out);
        }
    }
    for e in &entities {
        walk(e, &mut reasons);
    }
    reasons
}

/// Assert `shape` is refused by a reason whose `Display` begins with `prefix`
/// (the `ImportError` variant's rendering — `"unsupported signature element"`
/// for `UnsupportedSignature`, `"unsupported ECMA-335 layout"` for
/// `UnsupportedEcmaLayout`) and contains every string in `needles`. The reason
/// may come from a propagated error *or* a recorded drop — both are the same
/// fail-loud outcome, merely at different granularities.
fn expect_refusal(shape: &str, prefix: &str, needles: &[&str]) {
    let bytes = common::emit_metadata_fixture(shape);
    let reasons = refusal_reasons(&bytes);
    let matched = reasons
        .iter()
        .any(|r| r.contains(prefix) && needles.iter().all(|n| r.contains(n)));
    assert!(
        matched,
        "shape {shape:?}: expected a refusal ({prefix:?}) containing {needles:?}, \
         got reasons: {reasons:?}"
    );
}

/// Emit `shape`, parse + enumerate it, and assert an `UnsupportedSignature`
/// refusal (as an error or a recorded drop) whose reason contains every
/// `needle`.
fn expect_unsupported_signature(shape: &str, needles: &[&str]) {
    expect_refusal(shape, "unsupported signature element", needles);
}

/// As [`expect_unsupported_signature`], but for an `UnsupportedEcmaLayout`
/// refusal.
fn expect_unsupported_layout(shape: &str, needles: &[&str]) {
    expect_refusal(shape, "unsupported ECMA-335 layout", needles);
}

/// Assert `shape` projects with **no** refusal at all — no error, no dropped
/// member or type — for the shapes the reader now models end to end.
fn assert_projects_cleanly(shape: &str) {
    let bytes = common::emit_metadata_fixture(shape);
    let reasons = refusal_reasons(&bytes);
    assert!(
        reasons.is_empty(),
        "shape {shape:?}: expected a clean projection, got refusals: {reasons:?}"
    );
}

// ----- Signature / structural shapes (#240) ------------------------------

#[test]
fn varargs_method_fails_loud_calling_convention() {
    // A `Log()` method whose MethodDefSig carries the VARARG calling
    // convention byte (0x05).
    expect_unsupported_signature("vararg", &["varargs"]);
}

#[test]
fn parameter_custom_modifier_fails_loud() {
    // `void Take(modreq(IsConst) int32)` — an *unrecognised required* modifier on
    // a parameter. ECMA-335 II.7.1.1: a `modreq` must be *understood*, and this
    // reader understands exactly two (`InAttribute` over a byref, `IsVolatile` on
    // a field). `IsConst` is neither, so the member is refused — by name, so the
    // diagnostic says which construct is missing.
    expect_unsupported_signature(
        "param_custom_modifier",
        &[
            "unrecognised required custom modifier",
            "System.Runtime.CompilerServices.IsConst",
        ],
    );
}

#[test]
fn parameter_optional_modifier_projects() {
    // `void Take(modopt(IsConst) int32)` — the same unrecognised modifier, but
    // *optional*. II.7.1.1 says a tool may ignore a `modopt` it does not
    // understand, so it is dropped and the member projects with a plain `int32`
    // parameter. This is the whole difference between the two bytes, and the
    // reason the decoder keeps `required` rather than refusing at decode.
    assert_projects_cleanly("param_optional_modifier");

    let bytes = common::emit_metadata_fixture("param_optional_modifier");
    let view = Ecma335Assembly::parse(&bytes).expect("parse");
    let entities = view.enumerate_type_defs().expect("enumerate");
    let take = entities
        .iter()
        .flat_map(|e| &e.members)
        .find_map(|m| match m {
            Member::Method(mm) if mm.name == "Take" => Some(mm),
            _ => None,
        })
        .expect("the method survives the ignored modopt");
    let param = take.signature.parameters.first().expect("one parameter");
    assert_eq!(
        param.ty,
        TypeRef::Primitive(Primitive::I4),
        "the ignorable modifier is dropped, leaving the bare type"
    );
    assert!(!param.is_byref && !param.is_readonly_ref);
}

#[test]
fn parameter_volatile_modreq_fails_loud() {
    // `void Take(modreq(IsVolatile) int32)` — the `volatile` marker away from the
    // one position that gives it meaning (a field type). Recognising a modifier
    // is not licence to drop it wherever it turns up.
    expect_unsupported_signature("param_volatile_modreq", &["`volatile` modifier"]);
}

#[test]
fn parameter_in_modreq_without_byref_fails_loud() {
    // `void Take(modreq(InAttribute) int32)` — the read-only-ref marker with no
    // byref beneath it. It qualifies a *reference*; over a plain value it has
    // nothing to say, so refuse rather than fabricate a meaning.
    expect_unsupported_signature(
        "param_in_modreq_not_byref",
        &["read-only-ref modifier", "not a byref"],
    );
}

#[test]
fn return_type_custom_modifier_fails_loud() {
    // `modreq(IsConst) int32 Get()` — an unrecognised required modifier on the
    // return type.
    expect_unsupported_signature(
        "return_custom_modifier",
        &[
            "unrecognised required custom modifier",
            "System.Runtime.CompilerServices.IsConst",
        ],
    );
}

#[test]
fn typed_reference_parameter_without_core_library_fails_loud() {
    // `void Take(typedref)` — an ELEMENT_TYPE_TYPEDBYREF (0x16 = 22) parameter.
    // The decoder now models the element (→ the `System.TypedReference` value
    // type), and a *well-formed* image projects it cleanly, attributed to the
    // corlib it references (see the MiniLib `ByRefLikeIntrinsics` differential
    // pin). This hand-built image, however, carries no core-library reference
    // and is not itself corlib, so `System.TypedReference` cannot be attributed
    // to any assembly — the projector refuses rather than record a misleading
    // same-assembly `TypeDef`.
    expect_unsupported_signature(
        "typed_reference_param",
        &["System.TypedReference", "no core-library reference"],
    );
}

#[test]
fn bounded_array_projects() {
    // `IEnumerable<int[10..]>` — an `ELEMENT_TYPE_ARRAY` carrying an explicit
    // lower bound. The model now records the full `ArrayShape` (rank + sizes +
    // lower bounds), so the bound is carried faithfully rather than dropped or
    // refused, and enumeration succeeds.
    assert_projects_cleanly("bounded_array");
}

#[test]
fn rank_one_array_projects() {
    // `IEnumerable<int[*]>` — a single-dim, non-vector `ELEMENT_TYPE_ARRAY` with
    // an empty shape (no explicit bounds). The reader now decodes it to a
    // rank-1 `TypeRef::Array` (the model conflates it with `SZARRAY`'s `int[]`,
    // which is immaterial to name resolution), so enumeration succeeds.
    assert_projects_cleanly("rank_one_array");
}

#[test]
fn array_element_custom_modifier_fails_loud() {
    // `IEnumerable<modreq(IsConst) int[]>` — an unrecognised required modifier
    // riding on an array element changes ABI; reject rather than flatten.
    expect_unsupported_signature(
        "array_custom_modifier",
        &["unrecognised required custom modifier"],
    );
}

#[test]
fn compiler_controlled_method_is_dropped_and_recorded() {
    // A method whose MethodAttributes MemberAccess mask is PrivateScope (0x0),
    // i.e. `compilercontrolled` accessibility — no C#/F# compiler emits it, but
    // C++/CLI and hand-written IL do (module-scope helpers). The model has no
    // accessibility variant for it, so the *member* is dropped and recorded;
    // the enclosing type (and the rest of the assembly) must survive. This
    // used to abort the whole image at parse.
    expect_unsupported_layout("compiler_controlled_method", &["compilercontrolled"]);

    let bytes = common::emit_metadata_fixture("compiler_controlled_method");
    let view = Ecma335Assembly::parse(&bytes)
        .expect("a compilercontrolled member must not sink the whole parse");
    let (entities, skips) = view
        .enumerate_type_defs_with_skips()
        .expect("enumeration succeeds with the member dropped");
    assert!(
        skips.dropped_types.is_empty(),
        "no whole type drops for one compilercontrolled member: {:?}",
        skips.dropped_types
    );
    let host = entities
        .iter()
        .find(|e| e.name == "Host")
        .expect("the enclosing type survives");
    assert!(
        !host
            .members
            .iter()
            .any(|m| matches!(m, Member::Method(mm) if mm.name == "Hidden")),
        "the compilercontrolled method is not surfaced"
    );
    let skip = host
        .skipped_members
        .iter()
        .find(|s| s.name == "Hidden")
        .expect("the drop is recorded against the member's name");
    assert!(
        skip.reason.contains("compilercontrolled"),
        "the recorded reason names the cause, got: {}",
        skip.reason
    );
}

#[test]
fn external_module_typeref_scope_fails_loud() {
    // A base TypeRef scoped to a sibling ModuleRef (multi-module assembly) —
    // the `ResolutionScope::ExternalModule` arm no single-file compiler emits.
    // A `ModuleRef`-scoped `TypeRef` is a resolution scope the reader refuses
    // at parse (it can't name the owning assembly for one).
    expect_unsupported_layout(
        "external_module_typeref",
        &["unsupported TypeRef resolution scope"],
    );
}

// ----- Custom-attribute blob shapes --------------------------------------
//
// Malformed CA blobs the reader decodes per the ctor signature and our
// projector then refuses. NullableAttribute /
// NullableContextAttribute on a typar / method / parameter; CFR and
// DefaultMember on a type.

#[test]
fn nullable_attribute_invalid_byte_fails_loud() {
    // `[Nullable(3)]` on a typar — byte 3 isn't a documented nullable state.
    expect_unsupported_signature("nullable_invalid_byte", &["0/1/2"]);
}

#[test]
fn nullable_attribute_byte_array_on_typar_fails_loud() {
    // `[Nullable(new byte[]{1})]` on a typar — the composite (byte[]) overload
    // is only legal at non-typar positions.
    expect_unsupported_signature("nullable_byte_array_form", &["byte[]", "typar"]);
}

#[test]
fn duplicate_nullable_context_attribute_fails_loud() {
    // Two `[NullableContext]` rows on one method — at most one is legal.
    expect_unsupported_signature(
        "duplicate_nullable_context",
        &["multiple NullableContextAttribute"],
    );
}

#[test]
fn nullable_attribute_named_args_fails_loud() {
    // `[Nullable(1, Flag = true)]` on a `string` parameter — Roslyn never emits
    // named args on NullableAttribute.
    expect_unsupported_signature("nullable_named_args", &["named args"]);
}

#[test]
fn nullable_vector_extra_trailing_bytes_fails_loud() {
    // `[Nullable(byte[4])]` on a `List<string>` parameter — the pre-order walk
    // wants 2 bytes, 4 are supplied.
    expect_unsupported_signature(
        "nullable_vector_extra_bytes",
        &["length mismatch", "pre-order walk"],
    );
}

#[test]
fn nullable_vector_insufficient_bytes_fails_loud() {
    // `[Nullable(byte[2])]` on a `List<List<string>>` parameter — the walk
    // wants 3 bytes and runs out.
    expect_unsupported_signature("nullable_vector_insufficient_bytes", &["exhausted"]);
}

#[test]
fn nullable_vector_over_pointer_projects() {
    // `[Nullable(new byte[]{0,0})]` on an `int**` parameter — a *well-formed*
    // vector over a pointer position. Roslyn's pre-order flag walk visits each
    // pointer node (an oblivious `0`) then the pointee, so the two flags match
    // exactly; the reader must consume both and project cleanly. This is the
    // regression guard for the walk skipping the pointer node — which spuriously
    // refused (and, pre-#708, sank) the `T*` / `T*[]` accessors throughout
    // `System.Private.CoreLib`.
    assert_projects_cleanly("nullable_pointer_vector");
}

#[test]
fn compiler_feature_required_null_ctor_arg_fails_loud() {
    // `[CompilerFeatureRequired((string)null)]` — a null feature name.
    expect_unsupported_signature(
        "cfr_null_ctor_arg",
        &["CompilerFeatureRequiredAttribute", "null"],
    );
}

#[test]
fn compiler_feature_required_wrong_arity_fails_loud() {
    // `[CompilerFeatureRequired("RefStructs", "extra")]` — arity 2, not 1.
    expect_unsupported_signature(
        "cfr_wrong_arity",
        &["CompilerFeatureRequiredAttribute", "unexpected ctor args"],
    );
}

#[test]
fn compiler_feature_required_unexpected_named_arg_fails_loud() {
    // `[CompilerFeatureRequired("RefStructs", Bogus = true)]` — unknown named.
    expect_unsupported_signature(
        "cfr_unexpected_named_arg",
        &["CompilerFeatureRequiredAttribute", "unexpected named arg"],
    );
}

#[test]
fn compiler_feature_required_non_bool_is_optional_fails_loud() {
    // `[CompilerFeatureRequired("RefStructs", IsOptional = "true")]` — the
    // documented `IsOptional` property carrying a string, not a bool.
    expect_unsupported_signature(
        "cfr_non_bool_is_optional",
        &["CompilerFeatureRequiredAttribute", "IsOptional"],
    );
}

#[test]
fn default_member_named_args_fails_loud() {
    // `[DefaultMember("Item", Whatever = "nope")]` — named args on DefaultMember.
    expect_unsupported_signature(
        "default_member_named_args",
        &["DefaultMemberAttribute", "named args"],
    );
}

#[test]
fn default_member_null_ctor_arg_fails_loud() {
    // `[DefaultMember((string)null)]` — a null member name.
    expect_unsupported_signature(
        "default_member_null_ctor_arg",
        &["DefaultMemberAttribute", "null"],
    );
}

// ----- Property / event accessor shapes ----------------------------------
//
// Properties/events project through their accessors (linked via MethodSemantics);
// the projector validates each accessor signature and refuses exotic shapes.

#[test]
fn property_generic_accessor_fails_loud() {
    // `int P { get; }` whose `get_P` is a generic method — accessor type
    // parameters have no slot in the model.
    expect_unsupported_signature(
        "property_generic_accessor",
        &["getter accessor", "is generic", "`P`"],
    );
}

#[test]
fn method_init_marker_void_fails_loud() {
    // `void M()` carrying `modreq(IsExternalInit)` on its void return, on a plain
    // method rather than a property setter. `IsExternalInit` is accepted only on
    // a set accessor (the sole compiler-emitted case), so on any other method
    // the modified void is refused rather than silently flattened to plain
    // `void` — a required custom modifier the model can't represent.
    expect_unsupported_signature(
        "method_init_marker_void",
        &["custom modifier on a void return"],
    );
}

#[test]
fn method_modopt_void_projects() {
    // `modopt(IsConst) void M()` — an ignorable modifier before a `void` return.
    // The `init` marker's setter-only rule governs *required* modifiers; a
    // `modopt` is dropped (II.7.1.1), so this is an ordinary `void`-returning
    // method on any member kind, not just a setter. (The `fcs-dump` oracle's
    // accessor-return validator classifies the whole modifier chain for the same
    // reason — a `modopt` may sit anywhere in it, including around the `init`
    // marker.)
    assert_projects_cleanly("method_modopt_void");

    let bytes = common::emit_metadata_fixture("method_modopt_void");
    let view = Ecma335Assembly::parse(&bytes).expect("parse");
    let entities = view.enumerate_type_defs().expect("enumerate");
    let m = entities
        .iter()
        .flat_map(|e| &e.members)
        .find_map(|m| match m {
            Member::Method(mm) if mm.name == "M" => Some(mm),
            _ => None,
        })
        .expect("the method survives the ignored modopt");
    assert_eq!(
        m.signature.return_type,
        TypeRef::Primitive(Primitive::Void),
        "the ignorable modifier is dropped, leaving a plain `void` return"
    );
}

#[test]
fn property_init_only_setter_projects() {
    // `int P { init; }` — the setter's return is `modreq(IsExternalInit) void`,
    // the C# `init` encoding. The reader recognises that specific shape (a
    // `RetType::Void` whose modifier run holds
    // `System.Runtime.CompilerServices.IsExternalInit`) and projects the setter
    // as a plain void return, so the property enumerates with no refusal.
    assert_projects_cleanly("property_init_only_setter");
}

#[test]
fn event_other_accessor_is_dropped_and_recorded() {
    // An event with an extra `Other` (0x4) MethodSemantics accessor — the
    // open-ended slot the model can't carry (emitted by some F# pickle output
    // and a little interop). The *event* is dropped and recorded — surfacing
    // it while silently ignoring one of its accessors would misrepresent the
    // member — and its accessor methods stay hidden (accessors surface only
    // through their owner). The enclosing type survives. This used to abort
    // the whole image at parse.
    expect_unsupported_layout(
        "event_other_accessor",
        &["non-standard (Other) method-semantics accessor"],
    );

    let bytes = common::emit_metadata_fixture("event_other_accessor");
    let view =
        Ecma335Assembly::parse(&bytes).expect("an Other accessor must not sink the whole parse");
    let (entities, skips) = view
        .enumerate_type_defs_with_skips()
        .expect("enumeration succeeds with the event dropped");
    assert!(
        skips.dropped_types.is_empty(),
        "no whole type drops for one Other accessor: {:?}",
        skips.dropped_types
    );
    let host = entities
        .iter()
        .find(|e| e.name == "Host")
        .expect("the enclosing type survives");
    assert!(
        !host
            .members
            .iter()
            .any(|m| matches!(m, Member::Event(ev) if ev.name == "Tick")),
        "the event with the Other accessor is not surfaced"
    );
    for accessor in ["add_Tick", "remove_Tick", "other_Tick"] {
        assert!(
            !host
                .members
                .iter()
                .any(|m| matches!(m, Member::Method(mm) if mm.name == accessor)),
            "accessor `{accessor}` of the dropped event must not leak as a plain method"
        );
    }
    let skip = host
        .skipped_members
        .iter()
        .find(|s| s.name == "Tick")
        .expect("the drop is recorded against the event's name");
    assert!(
        skip.reason
            .contains("non-standard (Other) method-semantics accessor"),
        "the recorded reason names the cause, got: {}",
        skip.reason
    );
}

#[test]
fn event_disagreeing_static_accessors_fails_loud() {
    // An event whose add accessor is static but remove accessor is instance.
    expect_unsupported_signature("event_disagreeing_static", &["disagreeing on static-ness"]);
}

#[test]
fn event_modreq_accessor_fails_loud() {
    // An event whose add accessor return carries `modreq(IsConst)`.
    // The `modreq` sits on the accessor's `void` return, so it lands in that
    // position's modifier run; unlike an `init` setter's `modreq(IsExternalInit)`,
    // `IsConst` is not the accepted marker, so the projector refuses the modified
    // void return rather than fabricate a plain `void`.
    expect_unsupported_signature(
        "event_modreq_accessor",
        &["custom modifier on a void return"],
    );
}

#[test]
fn event_generic_accessor_fails_loud() {
    // An event whose add accessor is a generic method.
    expect_unsupported_signature(
        "event_generic_accessor",
        &["add accessor", "is generic", "`Tick`"],
    );
}

// ----- Generic-parameter decode shapes -----------------------------------

#[test]
fn generic_method_arity_mismatch_fails_loud() {
    // A MethodDefSig declaring 2 generic params but only one GenericParam row.
    expect_unsupported_signature(
        "generic_arity_mismatch",
        &["calling-convention arity 2", "length 1"],
    );
}

#[test]
fn unmanaged_attribute_without_struct_bit_fails_loud() {
    // `[IsUnmanagedAttribute]` on a typar lacking the value-type special
    // constraint — `unmanaged` is additive on `struct`, never standalone.
    expect_unsupported_signature(
        "unmanaged_attribute_without_struct",
        &["unmanaged", "value-type"],
    );
}

#[test]
fn generic_constraint_custom_modifier_fails_loud() {
    // A type constraint carrying `modreq(IsConst) class IComparable` — a custom
    // modifier on a constraint (the reader decodes a TypeSpec-with-leading-CMOD
    // into the constraint's modifiers, which the projector refuses).
    expect_unsupported_signature("constraint_modreq", &["custom modifier"]);
}

#[test]
fn generic_constraint_unmanaged_modreq_on_non_value_type_fails_loud() {
    // `modreq(UnmanagedType) class IComparable` — the unmanaged-modreq decode
    // only fires on the canonical `System.ValueType` shape; elsewhere it is
    // just an unsupported custom modifier.
    expect_unsupported_signature(
        "constraint_unmanaged_modreq_non_value_type",
        &["custom modifier"],
    );
}

#[test]
fn unmanaged_modreq_behind_a_modopt_is_still_consumed() {
    // `modopt(IsConst) modreq(UnmanagedType) valuetype System.ValueType` on a
    // `struct` typar. The `unmanaged` marker is a *required* modifier the
    // constraint path recognises specially, and an ignorable `modopt` alongside it
    // must not hide it (II.7.1.1): the marker is found in the position's run, the
    // constraint is consumed as `is_unmanaged`, and no stray `System.ValueType`
    // interface surfaces. This once regressed because modifiers were a *chain*
    // and the code looked only at its head; the run is a list now, so "is the
    // marker in it" is the only question there is to ask.
    assert_projects_cleanly("unmanaged_modreq_behind_modopt");

    let bytes = common::emit_metadata_fixture("unmanaged_modreq_behind_modopt");
    let view = Ecma335Assembly::parse(&bytes).expect("parse");
    let entities = view.enumerate_type_defs().expect("enumerate");
    let pick = entities
        .iter()
        .flat_map(|e| &e.members)
        .find_map(|m| match m {
            Member::Method(mm) if mm.name == "Pick" => Some(mm),
            _ => None,
        })
        .expect("the generic method survives");
    let typar = pick.generic_parameters.first().expect("one type parameter");
    assert!(typar.is_unmanaged, "the marker sets `unmanaged`");
    assert!(typar.value_type_constraint, "`unmanaged` refines `struct`");
    assert!(
        typar.type_constraints.is_empty(),
        "the redundant `System.ValueType` row is consumed, not surfaced: {:?}",
        typar.type_constraints
    );
}

#[test]
fn unmanaged_modreq_without_struct_bit_fails_loud() {
    // The canonical `modreq(UnmanagedType) valuetype System.ValueType` shape on
    // a typar WITHOUT the value-type special-constraint bit — inconsistent.
    expect_unsupported_signature(
        "unmanaged_modreq_without_struct",
        &["unmanaged", "value-type"],
    );
}

// ----- "Bound uncertainty": a refusal is localized, not fatal ------------
//
// The complement of the fail-loud shapes above: these pin that a refusal drops
// only the offending member/type and keeps the rest, per the reader plan. This
// is the fix for the whole-assembly-abort bug where a single undecodable member
// in (say) `System.Private.CoreLib` zeroed all ~2,500 of its sibling types.

#[test]
fn bad_member_keeps_good_sibling_and_records_the_drop() {
    // `Host` carries a good `void Good()` and a VARARG `void Bad()`. Only `Bad`
    // is refused; `Good` and the type itself must survive, and `Bad` must be
    // recorded (not silently swallowed).
    let bytes = common::emit_metadata_fixture("mixed_good_and_bad_member");
    let view = Ecma335Assembly::parse(&bytes).expect("parse mixed fixture");
    let (entities, skips) = view
        .enumerate_type_defs_with_skips()
        .expect("one unsupported member must not abort enumeration");

    assert!(
        skips.dropped_types.is_empty(),
        "no whole type should drop for a single bad member, got: {:?}",
        skips.dropped_types
    );
    let host = entities
        .iter()
        .find(|e| e.name == "Host")
        .expect("the `Host` type survives its bad member");

    let has_method = |name: &str| {
        host.members
            .iter()
            .any(|m| matches!(m, Member::Method(mm) if mm.name == name))
    };
    assert!(
        has_method("Good"),
        "the good sibling is kept: {:?}",
        host.members
    );
    assert!(!has_method("Bad"), "the varargs member is dropped");

    let bad = host
        .skipped_members
        .iter()
        .find(|s| s.name == "Bad")
        .expect("the dropped member is recorded on `skipped_members`");
    assert!(
        bad.reason.contains("varargs"),
        "the recorded reason names the cause, got: {}",
        bad.reason
    );
}

#[test]
fn one_bad_member_does_not_abort_the_trait_enumeration() {
    // The LSP consumes the *trait* method (`enumerate_type_defs`), which used to
    // return `Err` — and so drop the whole DLL — on the first unreadable member.
    // It must now return `Ok`, keeping the enclosing type.
    let bytes = common::emit_metadata_fixture("vararg");
    let view = Ecma335Assembly::parse(&bytes).expect("parse vararg fixture");
    let entities = view
        .enumerate_type_defs()
        .expect("a single unsupported member must not sink the assembly");
    assert!(
        entities.iter().any(|e| e.name == "Host"),
        "the enclosing type survives with its only (bad) member dropped"
    );
}

#[test]
fn trait_consumers_see_the_whole_type_skip_record() {
    // A trait-only consumer (the LSP path is generic over `EcmaView`) must be
    // able to learn that a whole type was dropped — previously only the
    // concrete `Ecma335Assembly` exposed the dropped-type list, and the trait
    // method silently discarded it. The skip record must flow through the *trait*
    // surface.
    fn skips_via_trait<V: EcmaView>(view: &V) -> Vec<borzoi_assembly::SkippedProjectionItem> {
        view.enumerate_type_defs_with_skips()
            .expect("enumeration succeeds with the type dropped")
            .1
            .dropped_types
    }
    let bytes = common::emit_metadata_fixture("default_member_null_ctor_arg");
    let view = Ecma335Assembly::parse(&bytes).expect("parse default-member fixture");
    let skips = skips_via_trait(&view);
    assert!(
        skips
            .iter()
            .any(|s| s.reason.contains("DefaultMemberAttribute")),
        "the whole-type drop is visible through the EcmaView trait, got: {skips:?}"
    );
}

#[test]
fn undecodable_type_shape_is_dropped_as_a_whole_type_not_fatal() {
    // `[DefaultMember((string)null)]` is a *type*-level attribute the projector
    // refuses — an entity-shape failure, not a member one. The whole type is
    // dropped (there is no surviving `Entity` to hang the record on), recorded
    // in the dropped-type list, and the enumeration still succeeds.
    let bytes = common::emit_metadata_fixture("default_member_null_ctor_arg");
    let view = Ecma335Assembly::parse(&bytes).expect("parse default-member fixture");
    let (entities, skips) = view
        .enumerate_type_defs_with_skips()
        .expect("an undecodable type shape must not abort enumeration");

    assert!(
        skips
            .dropped_types
            .iter()
            .any(|s| s.reason.contains("DefaultMemberAttribute")),
        "the dropped type is recorded with its reason, got: {:?}",
        skips.dropped_types
    );
    assert!(
        !entities.iter().any(|e| e.name == "Host"),
        "the undecodable type is not among the kept entities"
    );
}

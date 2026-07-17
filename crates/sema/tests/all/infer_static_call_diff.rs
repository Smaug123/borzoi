//! Stage OV-7 — **static method-call** overload resolution: differential and
//! behaviour tests for a fully-qualified static call `Type.Method(args)` whose
//! rooting type the resolver resolved to a referenced-assembly entity
//! (`Resolution::Entity`), typed as the chosen overload's **return** type by the
//! same engine the instance calls use (`docs/overload-resolution-plan.md` §4 /
//! stage OV-7): the provably-complete static method group over the receiver's
//! base chain (inherited statics participate — probed 2026-07-10: `D.M 3` with
//! `M` static on base `B` resolves to `B.M`, and an inherited static *competes
//! in betterness*, so a derived-only scan would be unsound), the
//! extension-absence gate, the curry gate, then FCS's single-candidate arity
//! shortcut or the `may_apply`/`must_apply` commit keystone.
//!
//! As in [`infer_member_access_diff`], the differential builds an [`AssemblyEnv`]
//! from a real BCL `System.Runtime.dll` and iterates **our** inferred types,
//! asserting the FCS `types` oracle agrees at each exact range (D5: we never
//! over-claim). The behaviour tests pin the OUT shapes each defer *silently*:
//! a betterness-requiring group (`System.Math.Abs 3` — several TDC-survivable
//! candidates, FCS picks by exactness), an Object-capped `Equals`, named
//! arguments, wrong arity, a `void` return (identity recorded, type deferred),
//! and a project-defined receiver.

use std::collections::HashMap;

use crate::common::{ensure_system_runtime_dll, invoke_fcs_dump, parse_fcs_types, temp_fs_file};
use borzoi_assembly::{Ecma335Assembly, EcmaView};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile, SyntaxKind};
use borzoi_sema::{AssemblyEnv, InferredFile, ProjectItems, Resolution, infer_file, resolve_file};
use rowan::TextRange;

/// An [`AssemblyEnv`] over the real BCL `System.Runtime.dll` — so `System.String`
/// / `System.Math` / `System.Char` / `System.GC` (and their static methods) are
/// present, with no FSharp.Core (whose implicit auto-opens would trip the
/// extension-absence gate).
fn bcl_env() -> AssemblyEnv {
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
}

/// Infer `src` against the BCL env, returning the inference output and the
/// binder-name → rendered-type map.
fn infer_bcl(src: &str) -> (InferredFile, HashMap<String, String>) {
    let env = bcl_env();
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    (inferred, def_types)
}

/// Infer `src` against the BCL env and, for every expression type *we* produced,
/// assert the FCS `types` oracle agrees at that exact range (D5 soundness).
/// Returns how many of our types FCS confirmed (so a test can pin coverage).
fn assert_sound(src: &str) -> usize {
    let env = bcl_env();
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);

    let path = temp_fs_file("static_call_diff", src);
    let json = invoke_fcs_dump("types", &path);
    let _ = std::fs::remove_file(&path);
    let fcs = parse_fcs_types(&json, src);

    let mut checked = 0usize;
    for (range, ty) in inferred.types() {
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        let fcs_ty = fcs.get(&(start, end)).unwrap_or_else(|| {
            panic!(
                "we typed {start}..{end} as {} but FCS has no node there\nsrc={src:?}\nfcs keys={:?}",
                ty.render(),
                fcs.keys().collect::<Vec<_>>()
            )
        });
        assert_eq!(
            &ty.render(),
            fcs_ty,
            "type mismatch at {start}..{end} in {src:?}"
        );
        checked += 1;
    }
    checked
}

/// The text range of the first `IDENT_TOK` spelling `name` in `src`.
fn ident_range(src: &str, name: &str) -> TextRange {
    let parsed = parse(src);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    file.syntax()
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::IDENT_TOK && t.text() == name)
        .unwrap_or_else(|| panic!("no IDENT_TOK {name:?} in {src:?}"))
        .text_range()
}

// ===== Differentials: commits =====

#[test]
fn parenthesised_static_callee_defers() {
    // GPT-5.6 review (OV-7 round 4) asked to peel a parenthesised static callee
    // like the instance path does; probing (2026-07-11) shows the parenthesised
    // static path is a **method-value** elaboration with *different* semantics,
    // so the decline is deliberate: single-candidate
    // `(String.IsNullOrEmpty) "x"` ⇒ `application : Boolean` (peeling would
    // happen to agree), but overloaded `(String.Compare)("a", "b")` ⇒
    // `call:function : obj` — the first-class method-group conversion FAILS
    // where the call form commits `Int32`, so peeling into the call engine
    // would publish `Int32` at a node FCS types `obj`. Both shapes defer.
    // (The instance paren forms were probed too — `(s.Substring)(1)` /
    // `(s.ToLowerInvariant)()` — and *match* their call-form types, so the
    // instance path's existing paren transparency stands validated.)
    for src in [
        "module M\nlet b = (System.String.IsNullOrEmpty) \"x\"\n",
        "module M\nlet c = (System.String.Compare)(\"a\", \"b\")\n",
    ] {
        let (inferred, def_types) = infer_bcl(src);
        assert!(
            def_types.is_empty(),
            "a parenthesised static callee defers: {src:?} gave {def_types:?}"
        );
        assert!(
            inferred.types().is_empty(),
            "nothing is published for a parenthesised static callee: {src:?}"
        );
        assert!(
            inferred.member_resolutions().is_empty(),
            "no member resolution is invented: {src:?}"
        );
    }
}

#[test]
fn single_candidate_static_call_matches_fcs() {
    // `System.String.IsNullOrEmpty "x"` — a single-candidate static (FCS's
    // arity-only shortcut) ⇒ `b : bool`; the whole-call node is `System.Boolean`
    // at the range FCS reports (the full application, path included).
    let src = "module M\nlet b = System.String.IsNullOrEmpty \"x\"\n";
    assert!(assert_sound(src) >= 1, "the whole-call node must emit");
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("b").map(String::as_str),
        Some("System.Boolean")
    );
}

#[test]
fn single_candidate_static_call_value_arg_matches_fcs() {
    // The 3.3d-era defer pin, flipped: a static call with a value argument.
    let src = "module M\nlet s = \"hi\"\nlet b = System.String.IsNullOrEmpty s\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("b").map(String::as_str),
        Some("System.Boolean")
    );
}

#[test]
fn arity_refuted_static_overload_matches_fcs() {
    // `System.String.Compare("a", "b")` — a genuine overload set whose losers are
    // all arity-refuted (every other `Compare` takes ≥ 3 parameters), leaving the
    // `(String, String)` candidate as the unique `may_apply` survivor, affirmed by
    // `must_apply` (typeEquiv both positions) ⇒ `c : int`.
    let src = "module M\nlet c = System.String.Compare(\"a\", \"b\")\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("c").map(String::as_str), Some("System.Int32"));
}

#[test]
fn arity_refuted_char_overload_matches_fcs() {
    // `System.Char.IsDigit 'c'` — the `IsDigit(String, Int32)` loser is
    // arity-refuted; the `IsDigit(Char)` winner affirms ⇒ `d : bool`.
    let src = "module M\nlet d = System.Char.IsDigit 'c'\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("d").map(String::as_str),
        Some("System.Boolean")
    );
}

#[test]
fn type_refuted_static_overload_matches_fcs() {
    // `System.Math.Abs(3.5)` — a `Double` argument type-refutes every other
    // numeric `Abs` overload (no widening / `op_Implicit` channel *from* `Double`
    // into any of them), so `Abs(Double)` is the unique survivor ⇒ `a : float`.
    // Contrast `math_abs_int_defers` below: an `Int32` argument leaves several
    // TDC-survivable candidates, which needs betterness (OV-8) and so defers.
    let src = "module M\nlet a = System.Math.Abs(3.5)\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("a").map(String::as_str),
        Some("System.Double")
    );
}

#[test]
fn unit_call_on_one_param_single_candidate_matches_fcs() {
    // GPT-5.6 review (OV-7 round 1): direct unit syntax is candidate-dependent in
    // FCS — against a **single-candidate one-parameter** method, `M()` is one
    // (ill-typed) unit argument and the single-candidate shortcut still
    // elaborates the call as the member: `System.String.IsNullOrEmpty()` ⇒
    // `Boolean` (probed, both spellings). The arity gate therefore accepts a
    // window containing 1 for a unit call.
    for src in [
        "module M\nlet a = System.String.IsNullOrEmpty()\n",
        "module M\nlet a = System.String.IsNullOrEmpty ()\n",
    ] {
        assert_sound(src);
        let (_, def_types) = infer_bcl(src);
        assert_eq!(
            def_types.get("a").map(String::as_str),
            Some("System.Boolean"),
            "unit call against the 1-param single candidate commits: {src:?}"
        );
    }
}

#[test]
fn unit_call_on_multi_candidate_group_defers() {
    // The multi-candidate counterpart keeps the zero-argument reading only:
    // FCS does not elaborate an ill-typed unit argument through overload
    // resolution (`System.String.Compare()` ⇒ `call:function : obj`, no member
    // call — probed), so the matcher path defers a unit call that fits no
    // zero-arity candidate.
    let src = "module M\nlet c = System.String.Compare()\n";
    let (inferred, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("c"),
        None,
        "a unit call against a no-nullary overload set defers"
    );
    assert!(
        inferred.member_resolutions().is_empty(),
        "no member is recorded for the deferred unit call"
    );
}

#[test]
fn static_call_result_in_tuple_matches_fcs() {
    // The static call's ground result flows into surrounding structure.
    let src = "module M\nlet t = (System.Char.IsDigit 'c', 1)\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("t").map(String::as_str),
        Some("System.Boolean * System.Int32")
    );
}

#[test]
fn static_call_beside_generalisable_param() {
    // A ground static call does not block an unrelated parameter's
    // generalisation: `let f x = (System.String.IsNullOrEmpty "y", x)` ⇒
    // `'a -> bool * 'a` (matching FCS — the call grounds, `x` stays free).
    let src = "module M\nlet f x = (System.String.IsNullOrEmpty \"y\", x)\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("f").map(String::as_str),
        Some("'a -> System.Boolean * 'a")
    );
}

#[test]
fn static_call_member_resolution_recorded() {
    // A committed static call records the chosen overload at the method-name
    // token (the 3.3b LSP enrichment surface). The resolver left the overloaded
    // `Compare` as `Deferred(QualifiedAccess)`; the wake's record lights it up.
    let src = "module M\nlet c = System.String.Compare(\"a\", \"b\")\n";
    let (inferred, _) = infer_bcl(src);
    let at = inferred.member_resolution_at(ident_range(src, "Compare"));
    assert!(
        matches!(at, Some(Resolution::Member { .. })),
        "a committed static overload records its member: {at:?}"
    );
}

// ===== Behaviour: defers (each sound, each silent) =====

#[test]
fn math_abs_int_defers() {
    // `System.Math.Abs 3` — an `Int32` argument leaves `Abs(Int32)` *and* the
    // TDC-reachable `Abs(Int64)` / `Abs(Double)` / `Abs(Decimal)` / `Abs(IntPtr)`
    // as `may_apply` survivors; FCS picks `Abs(Int32)` by exact-match betterness,
    // which v1 does not model — so the call defers, publishing nothing.
    let src = "module M\nlet c = System.Math.Abs 3\n";
    let (inferred, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("c"), None, "a betterness-needing call defers");
    assert!(
        !inferred
            .types()
            .values()
            .any(|t| t.render().contains("Int")),
        "no type leaks from a deferred static call"
    );
}

#[test]
fn object_capped_static_equals_defers() {
    // `System.String.Equals("a", "b")`: `String` declares a static
    // `Equals(String, String)`, but the inherited static
    // `Object.Equals(Object, Object)` also joins FCS's group — and `System.Object`
    // is only *forwarded* in `System.Runtime.dll` (an Object-capped chain), so the
    // group is incomplete and the call must defer.
    let src = "module M\nlet e = System.String.Equals(\"a\", \"b\")\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("e"),
        None,
        "an Object-capped static Equals defers"
    );
}

#[test]
fn named_argument_static_call_defers() {
    // A named argument is a non-positional shape — the call defers fully (no
    // type, no recorded member).
    let src = "module M\nlet c = System.String.Compare(strA = \"a\", strB = \"b\")\n";
    let (inferred, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("c"), None, "named arguments defer");
    assert!(
        inferred.member_resolutions().is_empty(),
        "no member is recorded for a named-argument call"
    );
}

#[test]
fn wrong_arity_static_call_defers() {
    // `System.String.IsNullOrEmpty("x", 1)` — a 2-element argument list against
    // the 1-parameter single candidate: outside the arity window, FCS does not
    // type the call as the return (it errors) — defer.
    let src = "module M\nlet b = System.String.IsNullOrEmpty(\"x\", 1)\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("b"), None, "an ill-arity static call defers");
}

#[test]
fn void_static_call_records_member_but_defers_type() {
    // `System.GC.Collect()` — arity 0 leaves the parameterless overload as the
    // unique survivor, but its return is `void` (F# `unit`, unmodelled): the wake
    // records the member's identity (hover / go-to-def) and skips the type.
    let src = "module M\nlet u = System.GC.Collect()\n";
    let (inferred, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("u"), None, "a void static call's type defers");
    assert!(
        matches!(
            inferred.member_resolution_at(ident_range(src, "Collect")),
            Some(Resolution::Member { .. })
        ),
        "the void call's member identity is still recorded"
    );
}

#[test]
fn project_defined_static_receiver_defers() {
    // An in-file type's static member is not an assembly entity — the rooting
    // segment resolves to a project type, not `Resolution::Entity`, so the static
    // path declines and the call defers.
    let src = "module M\ntype T() =\n    static member M(x: int) = x\nlet a = T.M 3\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("a"),
        None,
        "a project-defined static receiver defers"
    );
}

#[test]
fn static_data_member_access_still_defers() {
    // `System.String.Empty` (a static *field* access, not a call) stays out of
    // OV-7's scope: only method calls are typed; the data read defers as before.
    let src = "module M\nlet x = System.String.Empty\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("x"), None, "a static field access defers");
}

#[test]
fn static_call_argument_poison_blocks_generalisation() {
    // `let f x = (System.String.IsNullOrEmpty x, x)`: the call commits (arity
    // shortcut — the argument's *type* is not checked), but `x` flows into the
    // method argument, a subsumption we drop; FCS grounds `x : string`, so `f`
    // must not generalise to `'a -> bool * 'a` — the argument poison blocks it.
    let src = "module M\nlet f x = (System.String.IsNullOrEmpty x, x)\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("f"),
        None,
        "a static-call argument parameter use must block generalisation"
    );
}

// ===== Behaviour: synthetic-entity shapes the BCL cannot express =====

/// The building blocks for a synthetic static-call env: a `MethodLike` static
/// method and an `Entity`, both cloned off the real `System.String` template so
/// every unrelated field is well-formed.
mod synthetic {
    use super::*;
    use borzoi_assembly::{
        Access, Entity, EntityKind, Member, MethodLike, MethodSignature, Nullability, ParamDefault,
        Parameter, Primitive, TypeRef,
    };

    pub fn param(ty: TypeRef) -> Parameter {
        Parameter {
            name: None,
            ty,
            is_byref: false,
            is_out: false,
            is_readonly_ref: false,
            default: ParamDefault::None,
            is_param_array: false,
            nullability: Nullability::Oblivious,
        }
    }

    pub fn static_method(
        name: &str,
        params: Vec<Parameter>,
        ret: TypeRef,
        agc: Option<usize>,
        template: &MethodLike,
    ) -> Member {
        Member::Method(MethodLike {
            name: name.to_string(),
            access: Access::Public,
            signature: MethodSignature {
                parameters: params,
                return_type: ret,
                return_nullability: Nullability::Oblivious,
            },
            arg_group_count: agc,
            is_static: true,
            is_virtual: false,
            is_abstract: false,
            is_constructor: false,
            generic_parameters: vec![],
            source_name: None,
            ..template.clone()
        })
    }

    /// The env: real `System.Runtime` entities plus the given `Demo.*` classes.
    pub fn env_with(extra: Vec<Entity>) -> AssemblyEnv {
        let dll = ensure_system_runtime_dll();
        let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
        let asm = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
        let mut entities = asm
            .enumerate_type_defs()
            .expect("enumerate System.Runtime types");
        entities.extend(extra);
        AssemblyEnv::from_entities(entities)
    }

    /// A `Demo.<name>` entity of `kind` with `members`, deriving from
    /// `base_name` (another `Demo.*` type) when given.
    pub fn demo_entity(
        name: &str,
        kind: EntityKind,
        members: Vec<Member>,
        base_name: Option<&str>,
        template: &Entity,
    ) -> Entity {
        entity_in(
            vec!["Demo".to_string()],
            name,
            kind,
            members,
            base_name,
            template,
        )
    }

    /// An entity in an arbitrary `namespace` (empty = the global namespace),
    /// deriving from `base_name` (another type in the *same* namespace) when
    /// given.
    pub fn entity_in(
        namespace: Vec<String>,
        name: &str,
        kind: EntityKind,
        members: Vec<Member>,
        base_name: Option<&str>,
        template: &Entity,
    ) -> Entity {
        Entity {
            namespace: namespace.clone(),
            name: name.to_string(),
            kind,
            members,
            nested_types: vec![],
            base_type: base_name.map(|b| TypeRef::Named {
                assembly: None,
                namespace,
                name: b.to_string(),
                type_args: vec![],
                segment_arities: vec![0],
            }),
            interfaces: vec![],
            extension_member_names: vec![],
            union_case_names: None,
            static_extension_member_names: Vec::new(),
            // The `System.String` template may carry undecodable members the
            // reader dropped; a synthetic entity must not inherit them (they
            // poison the namespace in the extension-absence gate and read as
            // possibly-hiding members in the group walks).
            skipped_members: vec![],
            ..template.clone()
        }
    }

    /// The `System.String` entity (and its first method) as field templates.
    pub fn templates() -> (Entity, MethodLike) {
        let dll = ensure_system_runtime_dll();
        let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
        let asm = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
        let entities = asm
            .enumerate_type_defs()
            .expect("enumerate System.Runtime types");
        let entity = entities
            .iter()
            .find(|e| e.namespace == ["System"] && e.name == "String")
            .expect("String template")
            .clone();
        let method = entity
            .members
            .iter()
            .find_map(|m| match m {
                Member::Method(mm) if !mm.is_constructor => Some(mm.clone()),
                _ => None,
            })
            .expect("String method template");
        (entity, method)
    }

    pub fn i4() -> TypeRef {
        TypeRef::Primitive(Primitive::I4)
    }
    pub fn boolean() -> TypeRef {
        TypeRef::Primitive(Primitive::Bool)
    }
    pub fn string_ty() -> TypeRef {
        TypeRef::Primitive(Primitive::String)
    }
    pub fn object_ty() -> TypeRef {
        TypeRef::Primitive(Primitive::Object)
    }
}

/// Infer `src` against `env`, returning the inference output.
fn infer_with(env: &AssemblyEnv, src: &str) -> InferredFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), env);
    infer_file(&file, &resolved, env)
}

#[test]
fn inherited_static_commits_through_the_derived_name() {
    // The OV-7 group-walk payoff (probed 2026-07-10, `p2.fs`: FCS resolves
    // `D.M 3` to the base's static): `Demo.SBase` declares the only
    // `SInherited(int): bool`; the call through `Demo.SDer` walks the chain,
    // finds the single candidate, and commits — recording the member under the
    // *declaring* base entity.
    use borzoi_assembly::EntityKind;
    let (ent_t, m_t) = synthetic::templates();
    let sbase = synthetic::demo_entity(
        "SBase",
        EntityKind::Class,
        vec![synthetic::static_method(
            "SInherited",
            vec![synthetic::param(synthetic::i4())],
            synthetic::boolean(),
            Some(1),
            &m_t,
        )],
        None,
        &ent_t,
    );
    let sder = synthetic::demo_entity("SDer", EntityKind::Class, vec![], Some("SBase"), &ent_t);
    let env = synthetic::env_with(vec![sbase, sder]);

    let src = "module M\nlet a = Demo.SDer.SInherited(1)\n";
    let inferred = infer_with(&env, src);
    assert!(
        inferred
            .types()
            .values()
            .any(|t| t.render() == "System.Boolean"),
        "the inherited static commits its return type through the derived name"
    );
    assert!(
        matches!(
            inferred.member_resolution_at(ident_range(src, "SInherited")),
            Some(Resolution::Member { .. })
        ),
        "the member is recorded (under the declaring base)"
    );
}

#[test]
fn cross_level_static_overload_with_unrefutable_loser_defers() {
    // The P3 probe shape (FCS picks the *base's* more-specific static by
    // betterness): `Demo.SBase.SClash(string): bool`, `Demo.SDer.SClash(obj): int`.
    // Both levels join the group (distinct partial signatures — hiding does not
    // collapse them), and the `obj` candidate is un-refutable for a string
    // argument, so ≥ 2 `may_apply` survivors remain and the call defers — never
    // committing the derived (nearer) candidate FCS would *not* pick.
    use borzoi_assembly::EntityKind;
    let (ent_t, m_t) = synthetic::templates();
    let sbase = synthetic::demo_entity(
        "SBase",
        EntityKind::Class,
        vec![synthetic::static_method(
            "SClash",
            vec![synthetic::param(synthetic::string_ty())],
            synthetic::boolean(),
            Some(1),
            &m_t,
        )],
        None,
        &ent_t,
    );
    let sder = synthetic::demo_entity(
        "SDer",
        EntityKind::Class,
        vec![synthetic::static_method(
            "SClash",
            vec![synthetic::param(synthetic::object_ty())],
            synthetic::i4(),
            Some(1),
            &m_t,
        )],
        Some("SBase"),
        &ent_t,
    );
    let env = synthetic::env_with(vec![sbase, sder]);

    let src = "module M\nlet a = Demo.SDer.SClash(\"hi\")\n";
    let inferred = infer_with(&env, src);
    assert!(
        inferred.types().values().all(|t| {
            let r = t.render();
            r != "System.Int32" && r != "System.Boolean"
        }),
        "a cross-level static overload with an un-refutable loser defers"
    );
    assert_eq!(
        inferred.member_resolution_at(ident_range(src, "SClash")),
        None,
        "no member is recorded on a deferred overload"
    );
}

#[test]
fn possibly_curried_static_defers_and_single_group_commits() {
    // The OV-6.1 curry gate holds for statics: an F# `static member M a b`
    // compiles to a flattened two-parameter static indistinguishable from a
    // tupled `M(a, b)` (`arg_group_count: None`), so the call defers; the same
    // shape provably single-group (`Some(1)`, the C#/VB fact) commits.
    use borzoi_assembly::EntityKind;
    let run = |agc: Option<usize>| {
        let (ent_t, m_t) = synthetic::templates();
        let s = synthetic::demo_entity(
            "S",
            EntityKind::Class,
            vec![synthetic::static_method(
                "M",
                vec![
                    synthetic::param(synthetic::i4()),
                    synthetic::param(synthetic::i4()),
                ],
                synthetic::boolean(),
                agc,
                &m_t,
            )],
            None,
            &ent_t,
        );
        let env = synthetic::env_with(vec![s]);
        let inferred = infer_with(&env, "module M\nlet a = Demo.S.M(1, 2)\n");
        inferred
            .types()
            .values()
            .map(|t| t.render())
            .find(|r| r == "System.Boolean")
    };
    assert_eq!(
        run(Some(1)).as_deref(),
        Some("System.Boolean"),
        "a provably single-group two-parameter static commits"
    );
    assert_eq!(
        run(None),
        None,
        "an unknown-grouping two-parameter static may be curried, so it defers"
    );
}

#[test]
fn unit_call_on_one_param_instance_single_candidate_commits() {
    // The unit-call arity rule lives in the **shared** single-candidate arm, so
    // the instance side gains it with the statics (probed 2026-07-10: `c.M()`
    // on a single-candidate 1-param `M(string)` ⇒ `call:instance : Int32`).
    // Synthetic `Demo.C` with an instance `M(string): int`; the annotated
    // receiver grounds through the R2-d entity-backed annotation.
    use borzoi_assembly::{EntityKind, Member};
    let (ent_t, m_t) = synthetic::templates();
    let mut m = synthetic::static_method(
        "M",
        vec![synthetic::param(synthetic::string_ty())],
        synthetic::i4(),
        Some(1),
        &m_t,
    );
    if let Member::Method(mm) = &mut m {
        mm.is_static = false;
    }
    let c = synthetic::demo_entity("C", EntityKind::Class, vec![m], None, &ent_t);
    let env = synthetic::env_with(vec![c]);

    let src = "module M\nlet f (c: Demo.C) = c.M()\n";
    let inferred = infer_with(&env, src);
    assert!(
        inferred
            .types()
            .values()
            .any(|t| t.render() == "System.Int32"),
        "the instance unit call against the 1-param single candidate commits"
    );
}

#[test]
fn namespace_precedence_honours_inherited_statics() {
    // GPT-5.6 review (OV-7 round 2, P1): with `X.D : X.B`, `X.B.M(int): bool`
    // (an *inherited* static), and a global-root `D` declaring `D.M(int): int`
    // directly, `namespace X … D.M(1)` must resolve through the
    // **enclosing-namespace** reading `X.D` — FCS's type-qualified member lookup
    // is inheritance-aware, so `X.D` owns the path (probed 2026-07-10:
    // FCS ⇒ `X.B.M : Boolean`). The resolver's direct-statics-only ownership
    // test used to mark `X.D` partial and fall through to the root `D`,
    // and inference then committed the root's `Int32` — a wrong emission.
    // After the fix, inference roots at `X.D` and the group walk commits the
    // inherited `Boolean`.
    use borzoi_assembly::EntityKind;
    let (ent_t, m_t) = synthetic::templates();
    let xb = synthetic::entity_in(
        vec!["X".to_string()],
        "B",
        EntityKind::Class,
        vec![synthetic::static_method(
            "M",
            vec![synthetic::param(synthetic::i4())],
            synthetic::boolean(),
            Some(1),
            &m_t,
        )],
        None,
        &ent_t,
    );
    let xd = synthetic::entity_in(
        vec!["X".to_string()],
        "D",
        EntityKind::Class,
        vec![],
        Some("B"),
        &ent_t,
    );
    let root_d = synthetic::entity_in(
        vec![],
        "D",
        EntityKind::Class,
        vec![synthetic::static_method(
            "M",
            vec![synthetic::param(synthetic::i4())],
            synthetic::i4(),
            Some(1),
            &m_t,
        )],
        None,
        &ent_t,
    );
    let env = synthetic::env_with(vec![xb, xd, root_d]);

    let src = "namespace X\n\nmodule Z =\n    let a = D.M(1)\n";
    let inferred = infer_with(&env, src);
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() != "System.Int32"),
        "the lower-priority root D's Int32 must never be published"
    );
    assert!(
        inferred
            .types()
            .values()
            .any(|t| t.render() == "System.Boolean"),
        "the inherited static through the enclosing-namespace X.D commits Boolean"
    );
}

#[test]
fn namespace_precedence_honours_instance_only_names() {
    // The kind-agnostic half of the ownership rule (probed 2026-07-10): a
    // higher-priority type with an **instance-only** member of the name still
    // owns the path — FCS errors (`call:function : obj`) rather than re-rooting
    // at the lower-priority static — so inference must publish *nothing*, not
    // the root `D.M`'s `Int32`.
    use borzoi_assembly::{EntityKind, Member};
    let (ent_t, m_t) = synthetic::templates();
    let mut inst = synthetic::static_method(
        "M",
        vec![synthetic::param(synthetic::i4())],
        synthetic::boolean(),
        Some(1),
        &m_t,
    );
    if let Member::Method(mm) = &mut inst {
        mm.is_static = false;
    }
    let xd = synthetic::entity_in(
        vec!["X".to_string()],
        "D",
        EntityKind::Class,
        vec![inst],
        None,
        &ent_t,
    );
    let root_d = synthetic::entity_in(
        vec![],
        "D",
        EntityKind::Class,
        vec![synthetic::static_method(
            "M",
            vec![synthetic::param(synthetic::i4())],
            synthetic::i4(),
            Some(1),
            &m_t,
        )],
        None,
        &ent_t,
    );
    let env = synthetic::env_with(vec![xd, root_d]);

    let src = "namespace X\n\nmodule Z =\n    let a = D.M(1)\n";
    let inferred = infer_with(&env, src);
    assert!(
        inferred.types().is_empty(),
        "an instance-only owning name publishes nothing (FCS errors): {:?}",
        inferred
            .types()
            .values()
            .map(|t| t.render())
            .collect::<Vec<_>>()
    );
    assert!(
        inferred.member_resolutions().is_empty(),
        "no member resolution is invented either"
    );
}

#[test]
fn namespace_precedence_owns_interface_rooted_paths() {
    // GPT-5.6 review (OV-7 round 3): an **interface**-rooted higher-priority
    // reading owns the path even when the member lives only on a *base*
    // interface — FCS's lookup includes transitively inherited interfaces,
    // which `base_chain` cannot see (an interface has no `base_type`), so
    // membership is unenumerable and ownership must be conservative. Probed
    // 2026-07-10: with `X.D : X.IBase` (interfaces) carrying `IBase.M`, and a
    // global-root class `D` with a public static `M(int): int`, FCS owns the
    // `X.D` reading and errors (`call:function : obj`) — it never re-roots —
    // so inference must publish nothing, not the root's `Int32`.
    //
    // The refuted siblings from the same review round, pinned by the resolver
    // *continuing to fall through* (both probed — FCS re-roots and resolves
    // the root `D.M : Int32`): a **non-public** member of the name on the
    // higher-priority type, and an inherited **protected `Object`** member
    // (`MemberwiseClone`); accessibility is filtered at enumeration, so
    // neither confers ownership.
    use borzoi_assembly::{EntityKind, Member, TypeRef};
    let (ent_t, m_t) = synthetic::templates();
    let mut inst = synthetic::static_method(
        "M",
        vec![synthetic::param(synthetic::i4())],
        synthetic::boolean(),
        Some(1),
        &m_t,
    );
    if let Member::Method(mm) = &mut inst {
        mm.is_static = false;
        mm.is_virtual = true;
        mm.is_abstract = true;
    }
    let ibase = synthetic::entity_in(
        vec!["X".to_string()],
        "IBase",
        EntityKind::Interface,
        vec![inst],
        None,
        &ent_t,
    );
    let mut iface_d = synthetic::entity_in(
        vec!["X".to_string()],
        "D",
        EntityKind::Interface,
        vec![],
        None,
        &ent_t,
    );
    iface_d.interfaces = vec![TypeRef::Named {
        assembly: None,
        namespace: vec!["X".to_string()],
        name: "IBase".to_string(),
        type_args: vec![],
        segment_arities: vec![0],
    }];
    let root_d = synthetic::entity_in(
        vec![],
        "D",
        EntityKind::Class,
        vec![synthetic::static_method(
            "M",
            vec![synthetic::param(synthetic::i4())],
            synthetic::i4(),
            Some(1),
            &m_t,
        )],
        None,
        &ent_t,
    );
    let env = synthetic::env_with(vec![ibase, iface_d, root_d]);

    let src = "namespace X\n\nmodule Z =\n    let a = D.M(1)\n";
    let inferred = infer_with(&env, src);
    assert!(
        inferred.types().is_empty(),
        "an interface-rooted owning reading publishes nothing (FCS errors): {:?}",
        inferred
            .types()
            .values()
            .map(|t| t.render())
            .collect::<Vec<_>>()
    );
    assert!(
        inferred.member_resolutions().is_empty(),
        "no member resolution is invented either"
    );
}

#[test]
fn module_function_application_declines_the_static_path() {
    // A qualified *module function* call (`Demo.Mod.f 3`) roots at a module
    // entity, which the static path declines outright: a module function is an
    // F# value (curried, applied with value semantics), not a .NET method call —
    // without the gate this single-candidate 1-parameter shape would wrongly
    // ride the method engine. It defers instead.
    use borzoi_assembly::EntityKind;
    let (ent_t, m_t) = synthetic::templates();
    let module = synthetic::demo_entity(
        "Mod",
        EntityKind::Module,
        vec![synthetic::static_method(
            "f",
            vec![synthetic::param(synthetic::i4())],
            synthetic::i4(),
            None,
            &m_t,
        )],
        None,
        &ent_t,
    );
    let env = synthetic::env_with(vec![module]);

    let src = "module M\nlet a = Demo.Mod.f 3\n";
    let inferred = infer_with(&env, src);
    assert!(
        inferred
            .types()
            .values()
            .all(|t| t.render() != "System.Int32"),
        "a module-function application does not ride the static method engine"
    );
    assert_eq!(
        inferred.member_resolution_at(ident_range(src, "f")),
        None,
        "no member resolution is invented for a module function"
    );
}

#[test]
fn explicit_open_of_a_clean_namespace_commits_static_call() {
    // EX-2 (`docs/extension-scope-enumeration-plan.md`): an `open <namespace>` no
    // longer blanket-defers the overload gate — it makes only that namespace's
    // extension members in scope, keyed by name and call shape. `System` declares
    // no *static* extension named `IsNullOrEmpty`, so `System.String.IsNullOrEmpty
    // "x"` still commits, and FCS agrees. (The static-call gate does still defer a
    // name a static augmentation in an opened namespace declares — see the EX-2
    // differential in `infer_member_access_diff`.)
    let src = "module M\nopen System\nlet b = System.String.IsNullOrEmpty \"x\"\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("b").map(String::as_str),
        Some("System.Boolean"),
        "EX-2: opening a namespace with no `IsNullOrEmpty` extension still commits the static call"
    );
    assert_sound(src);
}

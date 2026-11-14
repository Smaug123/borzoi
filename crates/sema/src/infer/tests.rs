//! Behaviour tests for 3.2c-2b function-type emission and condition typing,
//! over our *own* output (the FCS differential lives in
//! `tests/all/infer_binder_types_diff.rs`). Each resolves and infers a snippet and
//! reads binder types back by name.

use crate::hash::HashMap;

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};

use super::Gen;
use crate::recovery::SyntaxRecovery;
use crate::resolve::ResolvedFile;
use crate::ty::{Ty, TyKind, TyTable};
use crate::{AssemblyEnv, ProjectItems, resolve_file};

/// A synthetic FSharp.Core-shaped [`AssemblyEnv`]: an abbreviation
/// **marker** for each primitive alias whose `Local` pickled target
/// chases to a BCL-shaped `System.*` entity in the same synthetic
/// assembly — the exact runtime mechanism (markers + the target chase),
/// hermetically, with no DLL on disk. The identity name `FSharp.Core`
/// plus the `Microsoft.FSharp.Core` auto-open make bare `int`/`bool`
/// annotation heads resolve the way a real project's do.
fn primitive_env() -> AssemblyEnv {
    use borzoi_assembly::{AbbreviationTarget, EntityKind};
    let pairs: &[(&str, &str)] = &[
        ("bool", "Boolean"),
        ("char", "Char"),
        ("sbyte", "SByte"),
        ("byte", "Byte"),
        ("int16", "Int16"),
        ("uint16", "UInt16"),
        ("int", "Int32"),
        ("int32", "Int32"),
        ("uint", "UInt32"),
        ("int64", "Int64"),
        ("uint64", "UInt64"),
        ("float32", "Single"),
        ("float", "Double"),
        ("nativeint", "IntPtr"),
        ("unativeint", "UIntPtr"),
        ("obj", "Object"),
        ("string", "String"),
        ("decimal", "Decimal"),
    ];
    let mut roots: Vec<borzoi_assembly::Entity> = Vec::new();
    let mut bcl_done = crate::hash::HashSet::default();
    for (alias, target) in pairs {
        let mut marker = synthetic_entity(
            &["Microsoft", "FSharp", "Core"],
            alias,
            EntityKind::Abbreviation,
        );
        marker.abbreviation_target = Some(AbbreviationTarget::Named {
            ccu: None,
            path: vec!["System".to_string(), (*target).to_string()],
            args: Vec::new(),
        });
        roots.push(marker);
        if bcl_done.insert(*target) {
            let kind = if *target == "Object" || *target == "String" {
                EntityKind::Class
            } else {
                EntityKind::Struct
            };
            roots.push(synthetic_entity(&["System"], target, kind));
        }
    }
    AssemblyEnv::from_assemblies_with_abbreviation_visibility(vec![(
        std::path::PathBuf::from("FSharp.Core.dll"),
        roots,
        crate::AbbreviationVisibility::Modelled,
        vec!["Microsoft.FSharp.Core".to_string()],
    )])
}

fn synthetic_entity(
    ns: &[&str],
    name: &str,
    kind: borzoi_assembly::EntityKind,
) -> borzoi_assembly::Entity {
    use borzoi_assembly::{Access, AssemblyIdentity, Entity, UnionCases, Version};
    Entity {
        assembly: AssemblyIdentity {
            name: "FSharp.Core".to_string(),
            version: Version {
                major: 0,
                minor: 0,
                build: 0,
                revision: 0,
            },
            public_key_token: None,
        },
        namespace: ns.iter().map(|s| (*s).to_string()).collect(),
        name: name.to_string(),
        kind,
        access: Access::Public,
        is_sealed: false,
        generic_parameters: vec![],
        base_type: None,
        interfaces: vec![],
        members: vec![],
        skipped_members: vec![],
        method_def_tokens: vec![],
        nested_types: vec![],
        is_readonly: false,
        is_byref_like: false,
        is_struct: false,
        is_auto_open: false,
        is_require_qualified_access: false,
        metadata_name: Some(name.to_string()),
        marker_compiled_name: None,
        is_no_equality: false,
        is_no_comparison: false,
        is_structural_equality: false,
        is_structural_comparison: false,
        is_allow_null_literal: false,
        is_no_pia_local: false,
        obsolete: None,
        compiler_message: None,
        experimental: None,
        default_member: None,
        compiler_feature_required: vec![],
        source_name: None,
        extension_member_names: vec![],
        union_cases: UnionCases::Unknowable,
        union_case_makers: Vec::new(),
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        custom_attrs: vec![],
        abbreviation_target: None,
        definition_range: None,
        member_measures: borzoi_assembly::MemberMeasures::NotFSharp,
    }
}

/// Infer `src` (single-file), returning each binder's rendered (canonical)
/// type keyed by name. Snippet names are unique, so a plain map suffices.
/// The env is [`primitive_env`] so annotated snippets (`let x : int = …`)
/// type through the marker chase like a real project's would.
fn def_types(src: &str) -> HashMap<String, String> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = primitive_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
    let inferred = super::infer_file(&file, &resolved, &env, &crate::PrecedingTypes::default());
    inferred
        .def_types()
        .iter()
        .map(|(id, ty)| {
            (
                resolved.def(*id).spelling(&env).into_owned(),
                ty.render(&env),
            )
        })
        .collect()
}

/// Infer `src`, returning the canonical renders of every *expression*-node
/// type (the `types()` map), for tests that a condition-derived `bool` never
/// leaks into an expression read-off.
fn expr_type_renders(src: &str) -> Vec<String> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = primitive_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
    let inferred = super::infer_file(&file, &resolved, &env, &crate::PrecedingTypes::default());
    inferred.types().values().map(|t| t.render(&env)).collect()
}

#[test]
fn monomorphic_function_types_via_condition() {
    // `c` is grounded to bool by the condition; the body returns int; so the
    // function is `bool -> int`, published on `f`, and `c` publishes its
    // slot in that type.
    let types = def_types("module M\nlet f c = if c then 1 else 2\n");
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("System.Boolean -> System.Int32")
    );
    assert_eq!(
        types.get("c").map(String::as_str),
        Some("System.Boolean"),
        "a parameter publishes its slot in the published function type"
    );
}

#[test]
fn curried_function_type_right_associates() {
    // Two parameters, both grounded to bool by conditions; nested body → int.
    let types = def_types("module M\nlet f a b = if a then (if b then 1 else 2) else 3\n");
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("System.Boolean -> System.Boolean -> System.Int32")
    );
}

#[test]
fn polymorphic_identity_generalises() {
    // `let f x = x` generalises to `'a -> 'a` (Stage 3.2c-2c): the binding is
    // walk-complete (a bare parameter use), `x`'s variable is created this
    // binding and unpoisoned, so it is quantified. The parameter `x` is still
    // never published standalone. (Flipped from the pre-generalisation
    // `polymorphic_function_defers`, which pinned that `let f x = x` deferred.)
    let types = def_types("module M\nlet f x = x\n");
    assert_eq!(types.get("f").map(String::as_str), Some("'a -> 'a"));
    assert_eq!(types.get("x"), None, "the parameter is never published");
}

#[test]
fn unused_parameter_generalises_to_ground_return() {
    // `let f x = 42` generalises to `'a -> int`: the unused parameter is
    // quantified, the return is the literal's ground `int`. (Flipped from the
    // pre-generalisation `unused_parameter_defers_function`.)
    let types = def_types("module M\nlet f x = 42\n");
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("'a -> System.Int32")
    );
    assert_eq!(types.get("x"), None);
}

#[test]
fn annotated_parameter_types_the_function() {
    // Stage R2-b: a table-annotated parameter `(c: bool)` grounds its slot
    // *and* binder from the annotation (a parameter annotation is exact in
    // F# — subsumption applies at call sites, not the binder), so the
    // function types `bool -> int`, and the parameter publishes its
    // annotation.
    let types = def_types("module M\nlet f (c: bool) = if c then 1 else 2\n");
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("System.Boolean -> System.Int32")
    );
    assert_eq!(
        types.get("c").map(String::as_str),
        Some("System.Boolean"),
        "an annotated parameter publishes its annotation"
    );
}

#[test]
fn non_table_annotated_parameter_still_defers() {
    // A parameter annotation outside the R2-a gate (an unknown name, a
    // generic application) keeps today's mark-incomplete catch-all: the
    // function defers, and the condition must not ground the parameter
    // (FCS would keep the annotation's type and report the condition
    // error).
    for src in [
        "module M\nlet f (c: MyBool) = if c then 1 else 2\n",
        "module M\nlet f (c: bool option) = if c then 1 else 2\n",
    ] {
        let types = def_types(src);
        assert_eq!(types.get("f"), None, "{src:?}");
        assert_eq!(types.get("c"), None, "{src:?}");
    }
}

#[test]
fn ill_typed_condition_keeps_the_parameter_annotation() {
    // `let f (c: int) = if c then 1 else 2` is ill-typed at the condition;
    // FCS keeps `c : int` (the annotation is exact) and `f : int -> int`.
    // The annotation's `Eq(slot, int)` lands first in generation order; the
    // condition's later `Eq(slot, bool)` fails and rolls back whole.
    let src = "module M\nlet f (c: int) = if c then 1 else 2\n";
    let types = def_types(src);
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("System.Int32 -> System.Int32")
    );
    assert!(
        !expr_type_renders(src).contains(&"System.Boolean".to_string()),
        "no condition-derived bool may survive the annotation: {:?}",
        expr_type_renders(src)
    );
}

#[test]
fn annotated_param_beats_a_conflicting_arg_check_wake() {
    // The 3.3c hazard shape, now with the annotation *modelled* (R2-b):
    // `let h (y: string) = (y, fi y)` with in-file `fi : int -> int64`
    // (itself R2-b-annotated). FCS keeps `y : string` (the annotation is
    // exact on the binder; the application is the error site) and
    // `h : string -> string * int64` (the result is fixed by `fi`'s own
    // shape regardless of the bad argument). Ours agrees: the annotation
    // grounds `y` before the wake, so the wake's `Eq(y, int)` fails and
    // rolls back; its poison hits only the already-ground result, so `h`
    // still emits — and crucially no `int` reaches `y`'s synth-mode tuple
    // use (`7L` is the only Int64; nothing emits Int32).
    let src = "module M\nlet fi (n: int) = 7L\nlet h (y: string) = (y, fi y)\n";
    let types = def_types(src);
    assert_eq!(
        types.get("fi").map(String::as_str),
        Some("System.Int32 -> System.Int64")
    );
    assert_eq!(
        types.get("h").map(String::as_str),
        Some("System.String -> System.String * System.Int64")
    );
    let exprs = expr_type_renders(src);
    assert!(
        exprs.contains(&"System.String".to_string()),
        "y's synth-mode tuple use emits the annotation type: {exprs:?}"
    );
    assert!(
        !exprs.contains(&"System.Int32".to_string()),
        "no wake-derived int may reach y's use: {exprs:?}"
    );
}

// ========================================================================
// Stage R2-c — function return-type annotations
// ========================================================================

#[test]
fn return_annotation_grounds_parameter_through_body() {
    // `let h x : int = x`: the sealed `int` annotation discharges the
    // body↔annotation relation as a genuine equality (the ArgCheck wake's
    // own no-subsumption judgment), grounding `x` through its body use.
    let types = def_types("module M\nlet h x : int = x\n");
    assert_eq!(
        types.get("h").map(String::as_str),
        Some("System.Int32 -> System.Int32")
    );
    assert_eq!(
        types.get("x").map(String::as_str),
        Some("System.Int32"),
        "the parameter publishes its slot"
    );
}

#[test]
fn subsumption_return_annotation_defers_the_open_parameter() {
    // `let f x : obj = x` is legal via subsumption; the dropped relation
    // must not ground `x` (FCS: `obj -> obj`). The undischarged check
    // poisons `x`, so `f` defers — silence, never `'a -> obj`.
    let types = def_types("module M\nlet f x : obj = x\n");
    assert_eq!(types.get("f"), None);
    // With a ground parameter side the annotated return still emits.
    let ground = def_types("module M\nlet f (b: bool) : obj = b\n");
    assert_eq!(
        ground.get("f").map(String::as_str),
        Some("System.Boolean -> System.Object")
    );
}

#[test]
fn return_annotation_body_emits_no_expression_nodes() {
    // The body is walked in check mode: `let g (x: int) : string = "s"`
    // records no node for the literal (its elaborated type is the
    // annotation's business), while the function still types.
    let src = "module M\nlet g (x: int) : string = \"s\"\n";
    let types = def_types(src);
    assert_eq!(
        types.get("g").map(String::as_str),
        Some("System.Int32 -> System.String")
    );
    assert!(
        expr_type_renders(src).is_empty(),
        "an annotated function body emits no nodes: {:?}",
        expr_type_renders(src)
    );
}

#[test]
fn non_table_return_annotation_keeps_the_skip() {
    // An annotation outside the gate (generic app / unknown name) keeps
    // the pre-R2-c whole-binding skip: nothing emitted anywhere.
    for src in [
        "module M\nlet f x : int option = None\n",
        "module M\nlet f x : MyInt = x\n",
    ] {
        let types = def_types(src);
        assert_eq!(types.get("f"), None, "{src:?}");
        assert!(expr_type_renders(src).is_empty(), "{src:?}");
    }
}

#[test]
fn ill_typed_body_keeps_the_return_annotation() {
    // `let f x : int = "s"` errors at the body; FCS keeps `f : 'a -> int`.
    // The failed discharge rolls back whole and poisons only its own
    // (ground) endpoints, so the unused parameter still quantifies.
    let types = def_types("module M\nlet f x : int = \"s\"\n");
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("'a -> System.Int32")
    );
}

#[test]
fn deferred_binder_used_as_condition_is_not_bool() {
    // Condition typing must not retype a binder through a condition use. On
    // the ill-typed (mid-edit) `let x : int = 1 \n if x then …`, FCS keeps
    // `x : int` and reports the condition mismatch; grounding `x` to `bool`
    // would be a wrong hover (D5). Since Stage R2-a, `x` is typed `int`
    // from its annotation — the condition's failed `Eq` rolls back and the
    // annotation type stands. (Flipped from the pre-R2-a pin that `x`
    // stayed untyped.)
    let types = def_types("module M\nlet x : int = 1\nlet r = if x then 0 else 0\n");
    assert_eq!(
        types.get("x").map(String::as_str),
        Some("System.Int32"),
        "an annotated value used as a condition keeps its annotation type"
    );
    // A `rec` binder is deferred as a group and not condition-typed.
    let rec_types = def_types("module M\nlet rec x = 1\nand r = if x then 0 else 0\n");
    assert_eq!(
        rec_types.get("x"),
        None,
        "a rec binder is not condition-typed"
    );
}

// ========================================================================
// Stage R2-a — annotated value binders (docs/completed/r2-annotation-typing-plan.md)
// ========================================================================

#[test]
fn annotated_value_binder_types_from_annotation() {
    // The annotation types the binder; the RHS is never walked, so the
    // literal node stays absent from the expression map.
    let src = "module M\nlet x : int64 = 42\n";
    let types = def_types(src);
    assert_eq!(types.get("x").map(String::as_str), Some("System.Int64"));
    assert!(
        expr_type_renders(src).is_empty(),
        "the annotated binding's RHS is not walked"
    );
}

#[test]
fn annotated_binder_type_flows_to_uses() {
    // The binder's ground type propagates to a use through the existing
    // def-var machinery: `let y = x` is `Int64` too.
    let types = def_types("module M\nlet x : int64 = 42L\nlet y = x\n");
    assert_eq!(types.get("x").map(String::as_str), Some("System.Int64"));
    assert_eq!(types.get("y").map(String::as_str), Some("System.Int64"));
}

#[test]
fn trivial_typed_pattern_head_types_from_annotation() {
    // `let (x : int64) = 42` — the `Paren > Typed > Named` head rides the
    // same gate; the RHS is likewise not walked.
    let src = "module M\nlet (x : int64) = 42\n";
    let types = def_types(src);
    assert_eq!(types.get("x").map(String::as_str), Some("System.Int64"));
    assert!(expr_type_renders(src).is_empty());
}

#[test]
fn structural_annotations_recurse_through_table_leaves() {
    let types = def_types(
        "module M\nlet a : int * string = (1, \"s\")\nlet f : int -> int = fun x -> x\nlet arr : int[] = [| 1; 2 |]\n",
    );
    assert_eq!(
        types.get("a").map(String::as_str),
        Some("System.Int32 * System.String")
    );
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("System.Int32 -> System.Int32")
    );
    assert_eq!(types.get("arr").map(String::as_str), Some("System.Int32[]"));
}

#[test]
fn annotation_defer_shapes_stay_silent() {
    // An in-file `type int64` shadows the primitive: the annotation names
    // the project type, as FCS does (`M.int64`, probed), never `System.Int64`.
    let types = def_types("module M\ntype int64 = A of int\nlet x : int64 = A 1\n");
    assert_eq!(types.get("x").map(String::as_str), Some("M.int64"));
    for (src, why) in [
        // Generic application (`int64 option`) is a non-bare head.
        ("module M\nlet x : int64 option = None\n", "generic app"),
        // `unit` is deliberately excluded from the v1 table.
        ("module M\nlet x : unit = ()\n", "unit excluded"),
        // A struct tuple is not `TyKind::Tuple`.
        (
            "module M\nlet x : struct (int * int) = struct (1, 2)\n",
            "struct tuple",
        ),
        // Mutable binders are out of scope in v1.
        ("module M\nlet mutable x : int64 = 42L\n", "mutable binder"),
        // A non-trivial annotated pattern (tuple head) stays with the
        // pattern catch-all.
        (
            "module M\nlet ((a, b) : int * int) = (1, 2)\n",
            "non-trivial typed pattern",
        ),
        // `module rec`: the R2-0 marker defers the earlier annotation.
        (
            "module rec M\nlet x : int64 = A\ntype int64 = A\n",
            "rec-module forward shadow",
        ),
    ] {
        let types = def_types(src);
        assert_eq!(types.get("x"), None, "{why}: {src:?}");
        assert_eq!(types.get("a"), None, "{why}: {src:?}");
        assert_eq!(types.get("b"), None, "{why}: {src:?}");
    }
    // A multi-segment head whose tail the resolver concretely resolves is
    // NOT a defer shape: `System.Int64` bridges through the R2-d entity
    // record (the real-BCL differential `infer_annotation_entity_diff`
    // pins the same behaviour against System.Runtime).
    let types = def_types("module M\nlet x : System.Int64 = 42L\n");
    assert_eq!(types.get("x").map(String::as_str), Some("System.Int64"));
}

#[test]
fn param_with_unmodelled_ascription_used_as_condition_is_not_bool() {
    // The parameter `c` carries an unmodelled ascription `(c : int)` in the
    // body, is *used as a value* (the first tuple element), *and* is used as a
    // condition. Condition typing grounds only `c`'s private function-type slot
    // (not its binder var), so:
    //   - the standalone parameter type is never published (`def_type`);
    //   - the value use `c` reads its binder var, which stays un-ground, so no
    //     `bool` leaks into the expression `types()` map either;
    //   - the ascription element leaves the tuple un-ground, so the function
    //     defers.
    // FCS keeps `c : int` and reports the condition error, so publishing `bool`
    // anywhere would be wrong (D5).
    let src = "module M\nlet f c = (c, (c : int), if c then 1 else 2)\n";
    let types = def_types(src);
    assert_eq!(types.get("f"), None, "the non-ground function defers");
    assert_eq!(types.get("c"), None, "the parameter is never published");
    assert!(
        !expr_type_renders(src).contains(&"System.Boolean".to_string()),
        "no condition-derived bool may leak into the expression map: {:?}",
        expr_type_renders(src)
    );
}

#[test]
fn plain_value_used_as_condition_keeps_its_rhs_type() {
    // A plain value used as a condition is not condition-groundable either, but
    // loses no coverage: it is already typed by its (bool) RHS. `b : bool`
    // comes from `let b = true`, not from the condition.
    let types = def_types("module M\nlet b = true\nlet r = if b then 0 else 0\n");
    assert_eq!(types.get("b").map(String::as_str), Some("System.Boolean"));
}

proptest::proptest! {
    /// For any two same-suffixed integer literals, `let f c = if c then <a>
    /// else <b>` is `bool -> <that primitive>`: the condition grounds the
    /// parameter to `bool` (inside the function type) and the then-branch's
    /// literal is the return, for every suffix/value — the curried function
    /// type is built consistently. The parameter `c` publishes `bool`.
    #[test]
    fn condition_grounded_function_is_bool_to_literal(
        // Bounded to the byte range so every suffix (incl. `uy`) is in range.
        a in 0u32..=255,
        b in 0u32..=255,
        sfx_idx in 0usize..3,
    ) {
        let (sfx, prim) = [("", "System.Int32"), ("L", "System.Int64"), ("uy", "System.Byte")][sfx_idx];
        let src = format!("module M\nlet f c = if c then {a}{sfx} else {b}{sfx}\n");
        let types = def_types(&src);
        let want = format!("System.Boolean -> {prim}");
        proptest::prop_assert_eq!(
            types.get("f").map(String::as_str),
            Some(want.as_str()),
            "src={:?}", src
        );
        proptest::prop_assert_eq!(types.get("c").map(String::as_str), Some("System.Boolean"));
    }
}

// ===== Stage 3.2c-2c — generalisation behaviour tests =====

#[test]
fn constant_function_generalises_over_unused_second_param() {
    // `let k a b = a` ⇒ `'a -> 'b -> 'a`: both parameters quantified, the
    // return the first parameter's variable — reused, so it keeps `'a`.
    let types = def_types("module M\nlet k a b = a\n");
    assert_eq!(types.get("k").map(String::as_str), Some("'a -> 'b -> 'a"));
}

#[test]
fn swap_tuple_function_generalises() {
    // `let f a b = (b, a)` ⇒ `'a -> 'b -> 'b * 'a`: the parameters number by
    // head order, the return tuple by first appearance (`b` then `a`).
    let types = def_types("module M\nlet f a b = (b, a)\n");
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("'a -> 'b -> 'b * 'a")
    );
}

#[test]
fn mixed_ground_and_param_generalises() {
    // `let f c x = ((if c then 1 else 2), x)` ⇒ `bool -> 'a -> int * 'a`: `c`
    // grounds to bool via the condition, the tuple's first element is the
    // ground `int` if-result, `x` is quantified.
    let types = def_types("module M\nlet f c x = ((if c then 1 else 2), x)\n");
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("System.Boolean -> 'a -> System.Int32 * 'a")
    );
}

#[test]
fn slot_binder_reunification_lets_a_param_use_ground() {
    // `let f x = if x then (1, x) else (2, x)` ⇒ `bool -> int * bool`: the
    // condition grounds `x`'s slot to bool, the slot=binder reunification (a
    // *complete* binding) flows that to `x`'s binder var, so the tuple's `x`
    // element is `bool` and the function is fully ground. This is the payoff
    // 2b had to defer.
    let types = def_types("module M\nlet f x = if x then (1, x) else (2, x)\n");
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("System.Boolean -> System.Int32 * System.Boolean")
    );
}

#[test]
fn incomplete_body_blocks_generalisation() {
    // `let f x = x + 1`: the `+` application is unmodelled, so the binding is
    // incomplete and must NOT generalise — even though `x`'s variable is open.
    // FCS grounds `x` to int through `+` (a constraint we drop), so emitting
    // `'a -> 'b` would be wrong (D5). We defer.
    let types = def_types("module M\nlet f x = x + 1\n");
    assert_eq!(
        types.get("f"),
        None,
        "an unmodelled body must block generalisation"
    );
}

#[test]
fn poisoned_param_via_dropped_else_relation_defers() {
    // `let f c x = if c then x else x`: the else-branch `x` is checked against
    // the then-branch `x` (a dropped check-mode relation), poisoning `x`. FCS
    // relates the two through that relation; we drop it, so `x` must not
    // generalise. The binding defers.
    let types = def_types("module M\nlet f c x = if c then x else x\n");
    assert_eq!(
        types.get("f"),
        None,
        "a poisoned parameter must block generalisation"
    );

    // `let f b x = if b then (x, 1) else (x, 2)`: the else-tuple is checked
    // against the then-tuple, poisoning `x` (through the tuple's component
    // relation). FCS relates `x` across the branches; we defer.
    let types2 = def_types("module M\nlet f b x = if b then (x, 1) else (x, 2)\n");
    assert_eq!(
        types2.get("f"),
        None,
        "a component-poisoned parameter must block generalisation"
    );
}

#[test]
fn compound_condition_marks_incomplete_and_defers() {
    // `let f x y = if x && y then 1 else 2`: the condition `x && y` is a
    // compound this stage does not model (it imposes FCS constraints on `x`,
    // `y` we drop), so the binding is incomplete and defers — even though the
    // return is ground.
    let types = def_types("module M\nlet f x y = if x && y then 1 else 2\n");
    assert_eq!(
        types.get("f"),
        None,
        "a compound condition marks the binding incomplete"
    );
}

#[test]
fn skipped_earlier_binder_reference_defers() {
    // A later function referencing an *earlier binder inference skipped* must
    // not quantify it — FCS knows its type through a constraint we did not
    // model, so a bogus scheme would be a D5 violation. Skips: an annotated
    // binder whose annotation is outside the R2-a table subset
    // (`let a : int option = None` — a generic application defers), a
    // `let rec` group, and a pattern binding with an unmodelled element.
    // Each leaves the reference open; the function must defer.
    for src in [
        // Annotated value whose annotation R2-a's gate defers (generic app).
        "module M\nlet a : int option = None\nlet h x = (x, a)\n",
        // `let rec` group skipped whole.
        "module M\nlet rec a = 1\nlet h x = (x, a)\n",
        // A pattern binding with a list element, which `pattern_ty`
        // rejects after `a` has its variable.
        "module M\nlet (a, [_]) = (1, [1])\nlet h x = (x, a)\n",
    ] {
        let types = def_types(src);
        assert_eq!(
            types.get("h"),
            None,
            "a reference to a skipped earlier binder must defer the function: {src:?}"
        );
    }
    // The flip side (Stage R2-a): a *table*-annotated earlier binder is
    // ground, so the same shape generalises over it — exactly like a
    // literal-typed environment reference.
    for src in [
        "module M\nlet a : int = 1\nlet h x = (x, a)\n",
        "module M\nlet (a, b) = (1, 2)\nlet h x = (x, a)\n",
    ] {
        let types = def_types(src);
        assert_eq!(
            types.get("h").map(String::as_str),
            Some("'a -> 'a * System.Int32"),
            "a ground environment reference must not block generalisation: {src:?}"
        );
    }
}

#[test]
fn stale_environment_var_defers_via_the_mark() {
    // `let a = f 0` (an unmodelled application) leaves `a` open (an environment
    // var created in an *earlier* binding). `let g x = (x, a)`: `a`'s variable
    // has index < `g`'s mark, so `g` cannot generalise over it — it defers
    // rather than quantifying an inherited-open variable. (`x` alone would
    // generalise; `a` is what blocks it.)
    let types = def_types("module M\nlet a = f 0\nlet g x = (x, a)\n");
    assert_eq!(
        types.get("g"),
        None,
        "an inherited-open environment var must defer the function"
    );
}

#[test]
fn value_binding_id_alias_stays_silent() {
    // `let g = id` (a value binding of a generalised function) is rejected by
    // F#'s value restriction (FS0030); we stay silent — the instantiation of
    // `id`'s scheme leaves `g` open, so nothing is published. `id` itself
    // generalises to `'a -> 'a`.
    let types = def_types("module M\nlet id x = x\nlet g = id\n");
    assert_eq!(types.get("id").map(String::as_str), Some("'a -> 'a"));
    assert_eq!(types.get("g"), None, "value restriction: g stays silent");
}

#[test]
fn wake_must_not_ground_an_earlier_open_binder() {
    // `let g = id` leaves `g` carrying id's *instantiation* variables,
    // created in g's own (earlier) binding. A later application `g 1` must
    // NOT discharge its ArgCheck against that environment variable: FCS
    // keeps `g : 'a -> 'a` (fcs-dump probe, 2026-07-08 — F# generalises
    // `let g = id`, contra the FS0030 shorthand in the sibling test's
    // comment), so grounding `g` to `int -> int` through the wake and
    // publishing it — as happened before the env guard — is a wrong type,
    // not a missed one. The wake's mark gate blocks it; `g`, the `id` use
    // node, and (in the unannotated shape) `n` all stay silent.
    for src in [
        "module M\nlet id x = x\nlet g = id\nlet n = g 1\n",
        "module M\nlet id x = x\nlet g = id\nlet n : int = g 1\n",
    ] {
        let types = def_types(src);
        assert_eq!(types.get("id").map(String::as_str), Some("'a -> 'a"));
        assert_eq!(types.get("g"), None, "g must stay silent: {src:?}");
        assert!(
            !expr_type_renders(src).contains(&"System.Int32 -> System.Int32".to_string()),
            "no monomorphised type may leak onto the `id` use node: {src:?}"
        );
    }
    // The annotated shape still types `n` from its annotation.
    let annotated = def_types("module M\nlet id x = x\nlet g = id\nlet n : int = g 1\n");
    assert_eq!(annotated.get("n").map(String::as_str), Some("System.Int32"));
}

#[test]
fn wake_must_not_ground_an_earlier_open_argument() {
    // The argument-side twin: `let a = h 0` leaves `a` open (unmodelled
    // application). `let n = fb a` (in-file `fb : bool -> int`) must not
    // ground `a := bool` through the wake — `a`'s type is fixed by its own
    // (unmodelled) binding, and FCS resolves it there, not here.
    let src = "module M\nlet fb b = if b then 1 else 2\nlet a = h 0\nlet n = fb a\n";
    let types = def_types(src);
    assert_eq!(types.get("a"), None, "the earlier open binder stays silent");
}

#[test]
fn nested_instantiation_generalises_in_turn() {
    // `let h x = (id, x)` (with `id : 'a -> 'a` in scope) ⇒
    // `'a -> ('b -> 'b) * 'a`: the use of `id` instantiates its scheme to a
    // *fresh* variable pair (distinct from `h`'s own `'a`), which `h`
    // generalises in turn — the instantiation vars are ≥ `h`'s mark.
    let types = def_types("module M\nlet id x = x\nlet h x = (id, x)\n");
    assert_eq!(
        types.get("h").map(String::as_str),
        Some("'a -> ('b -> 'b) * 'a")
    );
}

#[test]
fn ascription_regression_still_defers_unchanged() {
    // The 2b decoupling regression (an unmodelled ascription used as a
    // condition) must stay deferred — it is an *incomplete* binding shape, so
    // generalisation never fires. This pins that 3.2c-2c did not regress #701.
    let src = "module M\nlet f c = (c, (c : int), if c then 1 else 2)\n";
    let types = def_types(src);
    assert_eq!(types.get("f"), None, "the ascription binding still defers");
    assert_eq!(types.get("c"), None, "the parameter is never published");
    assert!(
        !expr_type_renders(src).contains(&"System.Boolean".to_string()),
        "no condition-derived bool may leak: {:?}",
        expr_type_renders(src)
    );
}

/// The completeness census records one entry per counted binding, empty for
/// a complete one, naming each unmodelled construct it met — the input the
/// corpus sweep's blocker report reads.
#[test]
fn incompleteness_names_what_each_binding_did_not_model() {
    use super::{Incomplete, OperatorGap};
    let src = "module M\nlet a = 1\nlet f x = x + 1\nlet g () = 2\n\
               let (x: int) = 1\nlet (p, q) = (1, 2)\nlet h y = let z = y + 1 in 0\n\
               let i y = let z = (y + 1, 2) in 0\nlet j (y: int) = (y + 1).ToString()\n\
               let k y = (y + 1; 0)\n";
    let parsed = parse(src);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = primitive_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
    let inferred = super::infer_file(&file, &resolved, &env, &crate::PrecedingTypes::default());
    assert_eq!(
        inferred.incompleteness(),
        [
            vec![],
            vec![Incomplete::Operator(OperatorGap::Unresolved)],
            vec![],
            // `let (x: int) = 1` and `let (p, q) = …`: walked, complete.
            vec![],
            vec![],
            // `h`: the local's RHS failed; that is its one reason.
            vec![Incomplete::Operator(OperatorGap::Unresolved)],
            // `i`, `j`, `k`: the local left open, the method callee and the
            // statement left open are consequences of the operator, which
            // resolves to nothing here and is recorded once and alone.
            vec![Incomplete::Operator(OperatorGap::Unresolved)],
            vec![Incomplete::Operator(OperatorGap::Unresolved)],
            vec![Incomplete::Operator(OperatorGap::Unresolved)],
        ]
    );
}

/// A binding the walk skips because its own annotation is outside the
/// modelled set is still a binding the census counts, under its own
/// reason: leaving it out would drop exactly the bindings that blocker
/// stops from the denominator. And a single-segment callee — an operator
/// or active-pattern name in parentheses — is a value application, not a
/// method call, so its reason is the callee's shape.
#[test]
fn census_counts_unreadable_binding_annotations_and_bare_callees() {
    use super::Incomplete;
    let src = "module M\nlet y : Foo = 1\nlet (z: Foo) = 1\nlet f x : Foo = x\n\
               let (a: Foo), b = (1, 2)\nlet (|Pos|) x = x\nlet r = (|Pos|) 1\n";
    let parsed = parse(src);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = primitive_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
    let inferred = super::infer_file(&file, &resolved, &env, &crate::PrecedingTypes::default());
    assert_eq!(
        inferred.incompleteness(),
        [
            vec![Incomplete::BindingAnnotation],
            vec![Incomplete::BindingAnnotation],
            vec![Incomplete::BindingAnnotation],
            vec![Incomplete::BindingAnnotation],
            vec![],
            vec![Incomplete::CalleeShape(super::SyntaxKind::LONG_IDENT_EXPR)],
        ]
    );
}

/// A member access on a value of a project type is left untyped, and the
/// census says so rather than counting the binding complete.
#[test]
fn a_member_of_a_project_type_is_a_census_reason() {
    use super::Incomplete;
    // A property: a record's fields are read, its other members are not.
    let src = "module M\ntype T =\n    { X : int }\n    member t.Y = 1\nlet f (x : T) = x\nlet g (x : T) = x.Y\n";
    let parsed = parse(src);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = primitive_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
    let inferred = super::infer_file(&file, &resolved, &env, &crate::PrecedingTypes::default());
    assert_eq!(
        inferred.incompleteness(),
        [vec![], vec![Incomplete::ProjectTypeMember]]
    );
}

#[test]
fn expression_level_let_in_does_not_nest_as_let_decl() {
    // A binding whose RHS contains an expression-level `let … in` must not
    // trip the sequential-solve `debug_assert` in `let_binding` — that let is a
    // `LET_OR_USE_EXPR`, not a `LET_DECL`, so it does not nest as a walked
    // binding. Inferring it (which runs the assert in test builds) must not
    // panic, and the local and the binding both type through it (CE-1).
    let types = def_types("module M\nlet g = let y = 1 in y\n");
    assert_eq!(types.get("g").map(String::as_str), Some("System.Int32"));
    assert_eq!(types.get("y").map(String::as_str), Some("System.Int32"));
}

// ===== Stage 3.2c-3 — function application behaviour tests =====

#[test]
fn ground_application_grounds_the_result() {
    // `let f c = if c then 1 else 2` is `bool -> int`; `let n = f true` applies
    // it, so `n : int`. The result `r` is fixed by `Eq(f, Fun(d, r))` against
    // `f`'s ground type — independent of how the argument coerces (that relation
    // is dropped/poisoned).
    let types = def_types("module M\nlet f c = if c then 1 else 2\nlet n = f true\n");
    assert_eq!(types.get("n").map(String::as_str), Some("System.Int32"));
}

#[test]
fn partial_application_grounds_a_function_result() {
    // `let add a b = …` is `bool -> bool -> int`; `let g = add true` leaves the
    // residual `bool -> int`, ground, so `g : bool -> int`.
    let types = def_types(
        "module M\nlet add a b = if a then (if b then 1 else 2) else 3\nlet g = add true\n",
    );
    assert_eq!(
        types.get("g").map(String::as_str),
        Some("System.Boolean -> System.Int32")
    );
}

#[test]
fn polymorphic_application_wakes_and_grounds_result() {
    // Closed by 3.3c: `let id x = x` (`'a -> 'a`), `let n = id 42` ⇒ `n : int`.
    // `id` instantiates to `'b -> 'b`, so `Eq(id_use, Fun(d, r))` gives
    // `d = r = 'b`. The literal argument `42 : int` is suspended as
    // `ArgCheck { arg: int, dom: d }`; on this walk-complete value binding the
    // wake fires (`d` is a scheme instantiation var of ours — a no-subsumption
    // domain), discharging `Eq(int, d)`, so `d = r = int` and `n : int`,
    // matching FCS. (Flipped from the 3.2c-3 `polymorphic_application_defers`.)
    let types = def_types("module M\nlet id x = x\nlet n = id 42\n");
    assert_eq!(types.get("id").map(String::as_str), Some("'a -> 'a"));
    assert_eq!(
        types.get("n").map(String::as_str),
        Some("System.Int32"),
        "the suspended arg wake grounds the polymorphic application result"
    );
}

#[test]
fn applied_polymorphic_argument_wakes_and_generalises() {
    // `let g y = id y` (with `id : 'a -> 'a`): the argument `y` is suspended as
    // an `ArgCheck { arg: y, dom: d }` against `id`'s domain `d`. On this
    // *walk-complete* binding the wake fires — `d` resolves to a scheme
    // **instantiation variable** of ours (an unbound root in
    // `scheme_inst_vars`, so a no-subsumption domain) — discharging `Eq(y, d)`.
    // `y = d = r = 'a`, so `g` generalises to `'a -> 'a`, matching FCS. This is
    // the headline 3.3c payoff (flipped from the 3.2c-3
    // `applied_polymorphic_argument_defers_via_poison` deferral).
    let types = def_types("module M\nlet id x = x\nlet g y = id y\n");
    assert_eq!(types.get("id").map(String::as_str), Some("'a -> 'a"));
    assert_eq!(
        types.get("g").map(String::as_str),
        Some("'a -> 'a"),
        "the suspended arg↔param wake grounds the parameter, so the function generalises"
    );
}

#[test]
fn ill_typed_application_stays_silent() {
    // `let n = 5` then `n 3`-shape: `n : int` is applied as a function. The
    // `Eq(n, Fun(d, r))` fails to unify (int is not a function shape), so
    // `unify_atomic` rolls it back whole — `n` stays `int`, `r` stays open,
    // nothing wrong is emitted. `n` keeps its RHS type; the application result
    // is silent.
    let types = def_types("module M\nlet n = 5\nlet m = n 3\n");
    assert_eq!(
        types.get("n").map(String::as_str),
        Some("System.Int32"),
        "the value binder keeps its ground RHS type; the failed Eq rolls back"
    );
    assert_eq!(
        types.get("m"),
        None,
        "an ill-typed application result stays silent (the shape Eq fails)"
    );
}

#[test]
fn ground_application_dead_end_lets_unrelated_param_generalise() {
    // A fully-modelled application that dead-ends into ground types does not
    // block generalisation of an unrelated parameter: `let f c = …` (ground
    // `bool -> int`), `let h x = (f true, x)` ⇒ `'a -> int * 'a`. The
    // application `f true` is ground `int` (poison bites only the dropped arg
    // relation — a *ground* var is unaffected), so `x` still quantifies.
    let types = def_types("module M\nlet f c = if c then 1 else 2\nlet h x = (f true, x)\n");
    assert_eq!(
        types.get("h").map(String::as_str),
        Some("'a -> System.Int32 * 'a")
    );
}

#[test]
fn an_unresolved_operator_leaves_the_function_deferred() {
    // An infix application is an application of its operator, and here (no
    // FSharp.Core) `+` resolves to nothing, so `let f x = x + 1` defers
    // rather than generalising over an operand nothing constrained.
    let types = def_types("module M\nlet f x = x + 1\n");
    assert_eq!(
        types.get("f"),
        None,
        "the operator is unresolved; the function defers"
    );
}

#[test]
fn bracket_indexer_stays_unmodelled() {
    // `f[true]` parses as an `APP_EXPR` (carrying the
    // `HIGH_PRECEDENCE_BRACK_APP_TOK` marker), but F# lowers a bracket indexer
    // to a `GetSlice`/`Item` member lookup, *not* a function application — a
    // construct this stage does not model. So `let n = f[true]` must NOT be
    // typed via the application path (which would wrongly emit `n : int` by
    // treating the indexer as `f` applied to `[true]`). On a ground function
    // whose result is itself a function, the naive model would even *disagree*
    // with FCS (`g[true]` where `g : bool -> 'd -> int` is `obj -> int` in FCS,
    // not our `'? -> int`). We defer.
    let types = def_types("module M\nlet f c = if c then 1 else 2\nlet n = f[true]\n");
    assert_eq!(
        types.get("n"),
        None,
        "a bracket indexer is not a function application; it must defer"
    );
}

#[test]
fn failed_shape_application_result_never_generalises() {
    // A function binding whose body is an application that *cannot* take a
    // function shape must NOT generalise a leaked, unrelated result var. `let n
    // = 5` gives `n : int`; `let h y = n 3` applies `n` (a non-function) — the
    // `Eq(n, Fun(d, r))` fails and rolls back, leaving `r` a fresh open var.
    // Without poisoning `r`, `h`'s type `'y -> r` would generalise to a bogus
    // `'a -> 'b`; FCS reports a type error, so we must stay silent (D5).
    let types = def_types("module M\nlet n = 5\nlet h y = n 3\n");
    assert_eq!(types.get("n").map(String::as_str), Some("System.Int32"));
    assert_eq!(
        types.get("h"),
        None,
        "a failed-shape application result must not leak into a bogus scheme"
    );
}

// ===== Stage 3.3c — the application wake rule (suspended arg↔param) =====

#[test]
fn chained_application_wakes_through_the_chain() {
    // `let c x = id (id x)` ⇒ `'a -> 'a`: the inner `id x` wakes (`x`'s var =
    // `id`'s domain, a scheme instantiation var), grounding the inner result to
    // that same class; the outer `id (…)` wakes likewise, so the whole chain is
    // one quantifiable class → `'a -> 'a`, matching FCS.
    let types = def_types("module M\nlet id x = x\nlet c x = id (id x)\n");
    assert_eq!(types.get("c").map(String::as_str), Some("'a -> 'a"));
}

#[test]
fn in_file_monomorphic_function_wakes() {
    // `let fb b = if b then 1 else 2` is `bool -> int` (condition-grounded);
    // `let h y = fb y`: the arg `y` is suspended against `fb`'s domain `d`,
    // which the shape `Eq` grounds to the sealed primitive `bool` (a
    // no-subsumption domain). The complete binding's wake discharges
    // `Eq(y, bool)`, so `h : bool -> int`, matching FCS.
    let types = def_types("module M\nlet fb b = if b then 1 else 2\nlet h y = fb y\n");
    assert_eq!(
        types.get("h").map(String::as_str),
        Some("System.Boolean -> System.Int32")
    );
}

#[test]
fn ground_value_argument_wakes() {
    // `let b = true` (`b : bool`), `let fb b2 = if b2 then 1 else 2`
    // (`bool -> int`), `let n = fb b`: the argument is the ground value `b`, and
    // the domain grounds to the sealed `bool`. The wake discharges `Eq(b, bool)`
    // (a no-op — both are already `bool`); `n : int` from the result. FCS agrees.
    let types =
        def_types("module M\nlet b = true\nlet fb b2 = if b2 then 1 else 2\nlet n = fb b\n");
    assert_eq!(types.get("n").map(String::as_str), Some("System.Int32"));
}

#[test]
fn instantiated_scheme_argument_flows_into_a_tuple() {
    // `let k y = (id y, y)` ⇒ `'a -> 'a * 'a`: `id y` wakes (`y = id`'s domain,
    // a scheme instantiation var), so `y`, the inner result, and the outer
    // element all collapse into one quantifiable class; the tuple's second `y`
    // element shares it. FCS: `'a -> 'a * 'a`.
    let types = def_types("module M\nlet id x = x\nlet k y = (id y, y)\n");
    assert_eq!(types.get("k").map(String::as_str), Some("'a -> 'a * 'a"));
}

#[test]
fn annotated_param_hazard_emits_nothing_wrong() {
    // THE SOUNDNESS GUARD. `let h (y: string) = (y, f y)` with `f : int -> int`
    // is ill-typed mid-edit code; FCS keeps `y : string` and `h : string ->
    // string * int` (reporting the argument mismatch). Pre-R2-b the annotated
    // parameter marked the binding incomplete and everything deferred; since
    // R2-b the annotation is *modelled*, and the guard's substance is
    // unchanged: `y` is grounded to `string` by its annotation **before**
    // the wake, so the wake's `Eq(string, int)` fails and rolls back — `y`
    // is never retyped to `int` through the annotation-vs-application
    // conflict, and its synth-mode tuple use reads off `string` (FCS
    // agrees). `h` still emits (`string -> string * int` — the result is
    // fixed by `f`'s own shape), and each parameter publishes its
    // annotation, which FCS keeps on the binder.
    let src = "module M\nlet f (x: int) = x\nlet h (y: string) = (y, f y)\n";
    let types = def_types(src);
    assert_eq!(
        types.get("f").map(String::as_str),
        Some("System.Int32 -> System.Int32")
    );
    assert_eq!(
        types.get("h").map(String::as_str),
        Some("System.String -> System.String * System.Int32")
    );
    assert_eq!(
        types.get("y").map(String::as_str),
        Some("System.String"),
        "the annotated param publishes its annotation"
    );
    assert_eq!(types.get("x").map(String::as_str), Some("System.Int32"));
    // The precise leak check: the tuple's `y` element must read off the
    // annotation's `string`, not the wake's `int`. (An `Int32` render does
    // legitimately appear elsewhere — `f`'s body use of `x` — so the
    // assertion is range-keyed, not a blanket render scan.)
    let parsed = parse(src);
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = primitive_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
    let inferred = super::infer_file(&file, &resolved, &env, &crate::PrecedingTypes::default());
    // Key on the exact one-byte `y` token range: the tuple node itself
    // starts at the same offset (its parens are a separate wrapper).
    let y_use_at = src.rfind("(y,").expect("tuple") + 1;
    let y_use = inferred
        .types()
        .iter()
        .find(|(r, _)| u32::from(r.start()) as usize == y_use_at && r.len() == 1.into())
        .map(|(_, ty)| ty.render(&env));
    assert_eq!(
        y_use.as_deref(),
        Some("System.String"),
        "the tuple's `y` use must read off the annotation type"
    );
}

#[test]
fn ill_typed_literal_against_sealed_domain_fails_discharge_silently() {
    // `let fb b = if b then 1 else 2` (`bool -> int`), `let m = fb 1`: the
    // argument `1 : int` is suspended against the domain, which grounds to the
    // sealed `bool`. The complete binding's wake attempts `Eq(int, bool)`, which
    // **fails** to unify — `unify_atomic` rolls it back, leaving no trace — so
    // the ArgCheck is undischarged and its `arg`/`dom` are poisoned. The result
    // `r` is still grounded to `int` by `Eq(fb, Fun(bool, int))`, so `m : int`
    // (FCS agrees); nothing wrong is published for the argument.
    let types = def_types("module M\nlet fb b = if b then 1 else 2\nlet m = fb 1\n");
    assert_eq!(
        types.get("m").map(String::as_str),
        Some("System.Int32"),
        "the result is grounded by the function shape, independent of the failed arg discharge"
    );
}

#[test]
fn incomplete_binding_blocks_an_otherwise_wakeable_application() {
    // `let g y = (id y, (y : int))`: the `(y : int)` ascription elsewhere in the
    // body marks the binding **incomplete**. So even though `id y` is otherwise
    // wakeable (`y = id`'s scheme-inst domain), the completeness gate forbids the
    // wake, and the binding defers. FCS grounds `g : int -> int * int` through
    // the ascription (a constraint we drop); we stay silent (D5). This pins that
    // the gate protects annotations/ascriptions.
    let types = def_types("module M\nlet id x = x\nlet g y = (id y, (y : int))\n");
    assert_eq!(
        types.get("g"),
        None,
        "an ascription-incomplete binding must not discharge its arg wake"
    );
}

#[test]
fn undischarged_argcheck_poison_blocks_generalisation() {
    // The generalisation-interaction guard: an application whose domain is NOT a
    // no-subsumption type leaves its `ArgCheck` undischarged, and the deferred
    // poison on `arg`/`dom`/`r` must block quantifying them. `let fo (o: obj) =
    // 1` is `obj -> int`; `let g y = fo y`: `obj` admits subsumption (excluded
    // from the no-subsumption set), so the wake never fires. FCS keeps
    // `g : 'a -> int` (it does *not* ground `y := obj`); we defer rather than
    // emit a wrong `obj -> int`. (`fo`'s annotated param also independently
    // defers the callee, so this is belt-and-braces — but it pins the poison
    // path.)
    let types = def_types("module M\nlet fo (o: obj) = 1\nlet g y = fo y\n");
    assert_eq!(
        types.get("g"),
        None,
        "an undischarged arg wake (non-no-subsumption domain) must defer the function"
    );
}

proptest::proptest! {
    /// For every literal kind `literal_ty` covers, `let id x = x` +
    /// `let n = id <lit>` grounds `n` to that literal's type: the polymorphic
    /// application wakes (the domain is `id`'s scheme instantiation var), and the
    /// discharged `Eq(arg, dom)` grounds the result. Covers every suffix so the
    /// no-subsumption sealed-primitive set (which mirrors `literal_ty`) is
    /// exercised end-to-end.
    #[test]
    fn identity_application_grounds_result_to_literal_type(idx in 0usize..16) {
        // (literal source, the primitive `literal_ty` assigns it).
        let cases = [
            ("42", "System.Int32"),
            ("42y", "System.SByte"),
            ("42uy", "System.Byte"),
            ("42s", "System.Int16"),
            ("42us", "System.UInt16"),
            ("42u", "System.UInt32"),
            ("42L", "System.Int64"),
            ("42UL", "System.UInt64"),
            ("42n", "System.IntPtr"),
            ("42un", "System.UIntPtr"),
            ("4.2", "System.Double"),
            ("4.2f", "System.Single"),
            ("4.2m", "System.Decimal"),
            ("\"hi\"", "System.String"),
            ("'c'", "System.Char"),
            ("true", "System.Boolean"),
        ];
        let (lit, prim) = cases[idx];
        let src = format!("module M\nlet id x = x\nlet n = id {lit}\n");
        let types = def_types(&src);
        proptest::prop_assert_eq!(
            types.get("n").map(String::as_str),
            Some(prim),
            "src={:?}", src
        );
    }
}

#[test]
fn sealed_primitive_set_matches_literal_ty() {
    // The no-subsumption sealed-primitive set (`is_sealed_primitive`) must be
    // exactly the scalar `TyKind::Named`s `literal_ty` produces — the same
    // primitives a literal has, each sealed, so a `<lit>` argument may discharge
    // against a domain grounded to that primitive. This pins the two lists in
    // lock-step (a byte-string literal is a `TyKind::Array`, so it is *not* a named
    // sealed primitive — correctly excluded).
    use borzoi_cst::syntax::{ConstExpr, Expr};
    let kinds = [
        "42",
        "42y",
        "42uy",
        "42s",
        "42us",
        "42u",
        "42L",
        "42UL",
        "42n",
        "42un",
        "4.2",
        "4.2f",
        "4.2m",
        "\"s\"",
        "'c'",
        "true",
        "()",
        "\"bytes\"B",
    ];
    for lit in kinds {
        let src = format!("module M\nlet v = {lit}\n");
        let parsed = parse(&src);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        // Find the `ConstExpr` in the RHS.
        let cst = file
            .syntax()
            .descendants_of::<Expr>()
            .next()
            .and_then(|e| match e {
                Expr::Const(c) => Some(c),
                _ => None,
            })
            .or_else(|| file.syntax().descendants_of::<ConstExpr>().next())
            .expect("a const literal");
        let names = crate::names::NameTable::default();
        let tys = TyTable::session();
        let ty = super::literal_ty(&names, &tys, &cst);
        match ty.map(|t| tys.kind(t)) {
            // Every named literal type must be a sealed primitive.
            Some(TyKind::Named { path, .. }) => assert!(
                super::is_sealed_primitive(&path.segments_in(&names)),
                "{lit}: literal_ty gave named {path:?} but is_sealed_primitive says no"
            ),
            // A byte string is an array — not a sealed *named* primitive.
            Some(TyKind::Array { .. }) => {}
            // Deferred literals (none in this list) have no type.
            _ => {}
        }
    }
}

// ===== Stage 3.2c-2c — instantiation property tests =====

/// A tiny pure reference for the expected canonical render of a
/// permutation-family function `let f p1 … pn = (pσ(1), …, pσ(k))`: the head
/// is `'a -> … -> 'z` over `n` parameters (numbered by head order), and the
/// tuple return names each selected parameter by its head index. This computes
/// the *string* FCS should agree on without an FCS round-trip (a couple of
/// exemplars in the differential pin the convention).
fn expected_permutation_render(n: usize, sigma: &[usize]) -> String {
    let head: Vec<String> = (0..n).map(|i| crate::ty::typar_name(i as u32)).collect();
    let ret: Vec<String> = sigma
        .iter()
        .map(|&j| crate::ty::typar_name(j as u32))
        .collect();
    format!("{} -> {}", head.join(" -> "), ret.join(" * "))
}

proptest::proptest! {
    /// A permutation-family function generalises to the canonical scheme a pure
    /// reference predicts — parameters number by head order, the return tuple
    /// by first appearance. Exercises arbitrary arities and selections
    /// (including repeats and reordering).
    #[test]
    fn permutation_family_generalises_canonically(
        // 2..=6 parameters; a return tuple of 2..=4 selections into them.
        n in 2usize..=6,
        picks in proptest::collection::vec(0usize..6, 2..=4),
    ) {
        // Clamp the selections into the parameter range.
        let sigma: Vec<usize> = picks.iter().map(|&p| p % n).collect();
        let params: Vec<String> = (0..n).map(|i| format!("p{i}")).collect();
        let ret: Vec<String> = sigma.iter().map(|&j| format!("p{j}")).collect();
        let src = format!(
            "module M\nlet f {} = ({})\n",
            params.join(" "),
            ret.join(", ")
        );
        let types = def_types(&src);
        let want = expected_permutation_render(n, &sigma);
        proptest::prop_assert_eq!(
            types.get("f").map(String::as_str),
            Some(want.as_str()),
            "src={:?}", src
        );
    }
}

/// Every binding construct with a pattern that names one binder more than
/// once — an or-pattern's alternatives share their binders, so the
/// resolver gives both occurrences one [`crate::DefId`] — infers without
/// panicking: the walk may meet a binder twice, and what it records of
/// one must not assume otherwise ([`super::BinderEnv`]).
#[test]
fn a_binder_named_twice_in_one_pattern_infers() {
    let contexts: &[&dyn Fn(&str) -> String] = &[
        &|p| format!("let {p} = 1\n"),
        &|p| format!("let {p}, z = 1, 2\n"),
        &|p| format!("let f {p} = 0\n"),
        &|p| format!("let f {p} y = y\n"),
        &|p| format!("let f {p} : int = 0\n"),
        &|p| format!("let g = fun {p} -> 0\n"),
        &|p| format!("let h () =\n    let {p} = 1\n    0\n"),
        &|p| format!("let k v =\n    match v with\n    | {p} -> 0\n"),
        &|p| format!("let m () =\n    for {p} in [1] do ()\n"),
    ];
    let patterns = [
        "(x | x)",
        "(x, x)",
        "x x",
        "(x: int | x: int)",
        "((x: int) | (x: int))",
        "(x as x)",
        "(Some x | Some x)",
        "[x; x]",
        "(x, (x | x))",
        "((x | x), x)",
    ];
    let env = primitive_env();
    for context in contexts {
        for p in patterns {
            let src = format!("module M\n\n{}", context(p));
            let parsed = parse(&src);
            let recovery = SyntaxRecovery::of(&parsed);
            let file = ImplFile::cast(parsed.root).expect("impl file");
            let resolved = resolve_file(&file, &ProjectItems::default(), &env, &recovery);
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::infer_file(&file, &resolved, &env, &crate::PrecedingTypes::default())
            }));
            assert!(run.is_ok(), "inference panicked on {src:?}");
        }
    }
}

/// An empty file and its resolution, so a [`Gen`] can be constructed for
/// unit tests of the substrate helpers ([`Solver::instantiate`] /
/// [`Gen::generalise`]) that do not need real name resolution.
fn empty_resolved() -> (ImplFile, ResolvedFile) {
    use borzoi_cst::parser::parse;

    let parsed = parse("module M\n");
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(
        &file,
        &ProjectItems::default(),
        &AssemblyEnv::default(),
        &recovery,
    );
    (file, resolved)
}

#[test]
fn instantiation_produces_fresh_distinct_vars() {
    // Instantiating a scheme replaces each distinct `Param` with a fresh var,
    // memoised so the same index maps to one var and distinct indices to
    // distinct vars; no `Param` survives.
    let (file, resolved) = empty_resolved();
    let env = AssemblyEnv::default();
    let tys = TyTable::session();
    let preceding = crate::PrecedingTypes::default();
    let facts = super::FileFacts::build(&file, &resolved, &env, &tys, &preceding, None);
    let mut cx = Gen::new(&facts, &tys);
    let p = |i| tys.param(i);
    let scheme = tys.fun(p(0), tys.tuple(vec![p(0), p(1)]));
    let inst = cx.solver.instantiate(&tys, scheme);
    // Structure preserved, no Param survives.
    assert!(
        !super::generalise::contains_param(&tys, inst),
        "no Param may survive: {inst:?}"
    );
    let var = |t: Ty| match tys.kind(t) {
        TyKind::Var(v) => *v,
        other => panic!("a fresh var, got {other:?}"),
    };
    let TyKind::Fun { arg, ret } = tys.kind(inst) else {
        panic!("inst should be a function");
    };
    let arg_v = var(*arg);
    let TyKind::Tuple(elems) = tys.kind(*ret) else {
        panic!("ret should be a tuple");
    };
    let (e0, e1) = (var(elems[0]), var(elems[1]));
    // Param(0) appears in arg and elem 0 → same var.
    assert_eq!(arg_v, e0, "same Param index → same fresh var");
    // Param(1) is distinct from Param(0).
    assert_ne!(e0, e1, "distinct Param indices → distinct vars");
}

#[test]
fn generalise_after_instantiate_round_trips() {
    // Instantiating a canonical scheme and re-generalising the fresh vars
    // recovers the same scheme (up to canonical numbering). We drive this
    // through the table so the fresh vars are real keys.
    let (file, resolved) = empty_resolved();
    let env = AssemblyEnv::default();
    let tys = TyTable::session();
    let preceding = crate::PrecedingTypes::default();
    let facts = super::FileFacts::build(&file, &resolved, &env, &tys, &preceding, None);
    let mut cx = Gen::new(&facts, &tys);
    let scope = cx.enter_scope();
    let p = |i| tys.param(i);
    let scheme = tys.fun(p(0), tys.fun(p(1), tys.tuple(vec![p(1), p(0)])));
    let inst = cx.solver.instantiate(&tys, scheme);
    let resolved_inst = cx.solver.table.resolve(inst);
    let regen = cx
        .generalise(&scope, resolved_inst)
        .expect("fresh vars generalise");
    assert_eq!(regen, scheme, "generalise ∘ instantiate is the identity");
}

/// `type`'s members, by the per-record scan of every type definition in
/// the file that [`super::IntrinsicMemberNames`] replaces: the reference
/// it is held to.
fn intrinsic_member_names_by_scan(
    defn: &borzoi_cst::syntax::TypeDefn,
    file: &ImplFile,
) -> Option<crate::hash::HashSet<String>> {
    use borzoi_cst::syntax::{MemberDefn, Pat, TypeDefn, TypeDefnRepr};
    let last_name = |d: &TypeDefn| {
        d.long_id()
            .and_then(|li| li.idents().last())
            .map(|t| super::ident_text(&t))
    };
    let name = last_name(defn)?;
    let mut out = crate::hash::HashSet::default();
    for d in file.syntax().descendants_of::<TypeDefn>() {
        if last_name(&d).as_deref() != Some(name.as_str()) {
            continue;
        }
        let mut members: Vec<MemberDefn> = d.members().collect();
        if let Some(TypeDefnRepr::ObjectModel(om)) = d.repr() {
            members.extend(om.members());
        }
        for m in members {
            match m {
                MemberDefn::Interface(_) => {}
                MemberDefn::Member(mm) => {
                    let Some(Pat::LongIdent(l)) = mm.binding().and_then(|b| b.pat()) else {
                        return None;
                    };
                    let tok = l.head().and_then(|h| h.idents().last())?;
                    out.insert(super::ident_text(&tok));
                }
                _ => return None,
            }
        }
    }
    Some(out)
}

/// The per-file table answers every type definition as the per-record
/// scan of the file does, over every sequence of up to three
/// declarations drawn from a vocabulary of the ways a type gains (or
/// hides) members: its own body, an augmentation of the same last name
/// at another path or in a nested module, an interface implementation,
/// and members whose name is not read off the head.
#[test]
fn intrinsic_member_table_agrees_with_the_per_record_scan() {
    use borzoi_cst::syntax::TypeDefn;
    const DECLS: &[&str] = &[
        "type R = { X: int }\n",
        "type R =\n    { X: int }\n    member this.Y = 1\n",
        "type R with\n    member this.A = 1\n",
        "type R with\n    static member B = 1\n",
        "type R() =\n    member val C = 1 with get, set\n",
        "type R with\n    interface System.IDisposable with\n        member this.Dispose() = ()\n",
        "type S = { Y: int }\n",
        "type M.R with\n    member this.D = 1\n",
        "module N =\n    type R with\n        member this.E = 1\n",
        "type R with\n    static member (+) (a: R, b: R) = a\n",
        "type R =\n    | P\n    | Q\n    member this.G = 1\n",
        "type R =\n    abstract H: int\n",
        "type S with\n    member this.A = 1\n",
    ];
    let mut sequences: Vec<Vec<&str>> = vec![vec![]];
    let mut all = Vec::new();
    for _ in 0..3 {
        sequences = sequences
            .iter()
            .flat_map(|seq| {
                DECLS.iter().map(move |d| {
                    let mut next = seq.clone();
                    next.push(*d);
                    next
                })
            })
            .collect();
        all.extend(sequences.iter().cloned());
    }
    let (mut declined, mut named) = (0usize, 0usize);
    for seq in all {
        let src = seq.concat();
        let file = ImplFile::cast(parse(&src).root).expect("an implementation file");
        let table = super::IntrinsicMemberNames::of(&file);
        for defn in file.syntax().descendants_of::<TypeDefn>() {
            let expected = intrinsic_member_names_by_scan(&defn, &file);
            match &expected {
                None => declined += 1,
                Some(names) if !names.is_empty() => named += 1,
                Some(_) => {}
            }
            assert_eq!(table.of_type(&defn), expected.as_ref(), "{src}");
        }
    }
    // Neither outcome is vacuous: the grid reaches both a declined type and
    // one with members.
    assert!(
        declined > 0 && named > 0,
        "{declined} declined, {named} named"
    );
}

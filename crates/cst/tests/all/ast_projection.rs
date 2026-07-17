//! Properties for the versioned typed-AST facades + gate (plan P1–P4), over the
//! `{v8, v9}` surfaces. See `docs/completed/ast-versioning-nullness-proof.md`.
//!
//! The union surface is `syntax::Type` (everything, incl. the F# 9.0 `WithNull`).
//! The **generated** projected surfaces are `syntax::v8::Type` (F# 8.0 — the
//! union minus `WithNull`) and `syntax::v9::Type` (F# 9.0 — re-exported = the
//! union today). The properties establish that each `vN` projection is exact,
//! total on its surface, round-trips, and that the gate (`syntax::projection`) is
//! the same fact as projection totality.

use borzoi_cst::language_version::LanguageVersion;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::projection::{
    first_out_of_surface, first_out_of_surface_type, type_kind_in_surface,
};
use borzoi_cst::syntax::{AstNode, SyntaxKind, SyntaxNode, Type as UnionType, v8, v9};
use proptest::prelude::*;

/// Every `Type`-kind node in `src`'s parse, in preorder.
fn type_nodes(src: &str) -> Vec<SyntaxNode> {
    parse(src)
        .root
        .descendants()
        .filter(|n| UnionType::can_cast(n.kind()))
        .collect()
}

fn count_with_null(src: &str) -> usize {
    type_nodes(src)
        .iter()
        .filter(|n| n.kind() == SyntaxKind::WITH_NULL_TYPE)
        .count()
}

/// Base type annotations that contain no `WithNull` of their own — one per
/// distinct surface shape. Appending ` | null` wraps each in exactly one
/// `WITH_NULL_TYPE` node (nullness binds below tuple/function arrows, above the
/// postfix app/array layer — see `kinds.rs`).
const BASES: &[&str] = &[
    "int",           // LongIdent
    "int list",      // App
    "int -> string", // Fun
    "int * string",  // Tuple
    "int[]",         // Array
    "'T",            // Var
    "(int)",         // Paren
    "#seq<int>",     // Hash
];

/// Build a program of one `let` binding per spec; `true` appends ` | null`.
fn program(specs: &[(usize, bool)]) -> String {
    let mut s = String::new();
    for (i, (base_idx, has_null)) in specs.iter().enumerate() {
        let base = BASES[base_idx % BASES.len()];
        let null = if *has_null { " | null" } else { "" };
        s.push_str(&format!("let v{i} : {base}{null} = failwith \"\"\n"));
    }
    s
}

// ---- example-based smoke tests (fast, explicit) ----------------------------

#[test]
fn with_null_is_excluded_from_v8_but_present_in_union() {
    let nodes = type_nodes("let x : string | null = failwith \"\"\n");
    let wn = nodes
        .iter()
        .find(|n| n.kind() == SyntaxKind::WITH_NULL_TYPE)
        .expect("fixture must parse a WITH_NULL_TYPE node");
    // Union sees it; the F# 8.0 projection does not.
    assert!(UnionType::cast(wn.clone()).is_some());
    assert!(v8::Type::cast(wn.clone()).is_none());
}

#[test]
fn cast_returns_none_for_non_type_nodes() {
    // `cast` is documented to return `None` (never panic) for any non-type node
    // — consumers probe arbitrary nodes with it. Regression for the second
    // review of this slice (a debug-only postcondition that mis-fired on
    // non-type kinds).
    let root = parse("let x = 1 + 2\nmodule M = begin end\n").root;
    for n in root
        .descendants()
        .filter(|n| !UnionType::can_cast(n.kind()))
    {
        assert!(
            v8::Type::cast(n.clone()).is_none(),
            "non-type node {:?} must cast to None",
            n.kind()
        );
    }
}

#[test]
fn surface_type_round_trips_through_v8() {
    for n in type_nodes("let f : int -> int list = failwith \"\"\n") {
        assert_ne!(n.kind(), SyntaxKind::WITH_NULL_TYPE);
        let v = v8::Type::cast(n.clone()).expect("8.0-surface type casts to v8");
        // Same underlying node (no coarsening), kind preserved.
        assert_eq!(v.syntax(), &n);
        assert_eq!(v.syntax().kind(), n.kind());
    }
}

#[test]
fn gate_finds_nullness_only_below_9() {
    let src = "let x : string | null = failwith \"\"\n";
    let root = parse(src).root;
    assert!(first_out_of_surface_type(&root, LanguageVersion::V8_0).is_some());
    // 9.0 introduced nullness; at 9.0 and above the tree is fully viewable.
    assert!(first_out_of_surface_type(&root, LanguageVersion::V9_0).is_none());
    assert!(first_out_of_surface_type(&root, LanguageVersion::DEFAULT).is_none());
    assert!(first_out_of_surface_type(&root, LanguageVersion::Preview).is_none());
}

#[test]
fn gate_is_silent_on_nullness_free_code() {
    let src = "let f : int -> int list = failwith \"\"\nlet g : #seq<int> = failwith \"\"\n";
    let root = parse(src).root;
    for lang in [
        LanguageVersion::V4_6,
        LanguageVersion::V8_0,
        LanguageVersion::Preview,
    ] {
        assert!(first_out_of_surface_type(&root, lang).is_none(), "{lang}");
    }
}

#[test]
fn each_base_plus_null_yields_exactly_one_with_null() {
    // Guards the generator's accounting: ` | null` wraps once, whatever the base.
    for base in BASES {
        let src = format!("let x : {base} | null = failwith \"\"\n");
        assert_eq!(count_with_null(&src), 1, "base = {base:?}");
    }
}

#[test]
fn general_gate_coincides_with_type_gate_today() {
    // The only gated kind today is a `Type` kind (nullness), so the general gate
    // (`first_out_of_surface`, over every modelled kind) and the `Type`-restricted
    // gate must find the same node at every version. This locks the
    // generalisation: the day a non-`Type` kind is gated, the type-only gate would
    // start missing it and this equality would break — exactly the regression we
    // want flagged.
    for src in [
        "let x : string | null = failwith \"\"\n",
        "let f : int -> int list = failwith \"\"\n",
        "module M = begin end\n",
    ] {
        let root = parse(src).root;
        for lang in [
            LanguageVersion::V4_6,
            LanguageVersion::V8_0,
            LanguageVersion::V9_0,
            LanguageVersion::Preview,
        ] {
            assert_eq!(
                first_out_of_surface(&root, lang),
                first_out_of_surface_type(&root, lang),
                "src={src:?} lang={lang}",
            );
        }
    }
}

// ---- the properties (generated programs) -----------------------------------

proptest! {
    /// P-exclude / P-union-total / P-roundtrip, universally over every `Type`
    /// node of a generated program: v8 casts iff the kind is in the 8.0 surface
    /// (i.e. not `WithNull`); the union always casts; and a surface node's v8
    /// view wraps the identical underlying node.
    #[test]
    fn projection_is_exact_and_total(specs in prop::collection::vec((0usize..BASES.len(), any::<bool>()), 0..8)) {
        let src = program(&specs);
        for n in type_nodes(&src) {
            let in_surface = type_kind_in_surface(n.kind(), LanguageVersion::V8_0);
            prop_assert_eq!(in_surface, n.kind() != SyntaxKind::WITH_NULL_TYPE);

            // Union is total over every Type node.
            prop_assert!(UnionType::cast(n.clone()).is_some());

            // v8 casts exactly the surface kinds, and round-trips them.
            match v8::Type::cast(n.clone()) {
                Some(v) => {
                    prop_assert!(in_surface);
                    prop_assert_eq!(v.syntax(), &n);
                    prop_assert_eq!(v.syntax().kind(), n.kind());
                }
                None => prop_assert!(!in_surface),
            }
        }
    }

    /// P-gate ≡ totality: the gate fires under 8.0 iff the program uses nullness,
    /// and never fires at 9.0 / preview. The count of out-of-surface nodes equals
    /// the count of `WithNull` nodes equals the number of ` | null` suffixes.
    #[test]
    fn gate_matches_nullness_use(specs in prop::collection::vec((0usize..BASES.len(), any::<bool>()), 0..8)) {
        let src = program(&specs);
        let root = parse(&src).root;
        let null_count = specs.iter().filter(|(_, b)| *b).count();

        prop_assert_eq!(count_with_null(&src), null_count);
        prop_assert_eq!(
            first_out_of_surface_type(&root, LanguageVersion::V8_0).is_some(),
            null_count > 0
        );
        prop_assert!(first_out_of_surface_type(&root, LanguageVersion::V9_0).is_none());
        prop_assert!(first_out_of_surface_type(&root, LanguageVersion::Preview).is_none());
    }

    /// P2 (v9 totality) + P4 (no-coarsen across surfaces): `v9` is total over every
    /// `Type` node (it equals the union — F# 9.0 introduces the last/only modelled
    /// typed delta, nullness), and `v8` *refines* `v9` by exactly the one 9.0 node:
    /// `v8` casts a node iff `v9` casts it AND it is not `WithNull`. Both `vN` casts
    /// round-trip to the identical underlying node (no coarsening).
    #[test]
    fn v8_refines_v9_by_exactly_nullness(specs in prop::collection::vec((0usize..BASES.len(), any::<bool>()), 0..8)) {
        let src = program(&specs);
        for n in type_nodes(&src) {
            // v9 == union: total over every Type node, same underlying node.
            let v9 = v9::Type::cast(n.clone());
            prop_assert!(v9.is_some(), "v9 must be total over Type nodes: {:?}", n.kind());
            let v9 = v9.unwrap();
            prop_assert_eq!(v9.syntax(), &n);

            // v8 = v9 minus the 9.0 WithNull node.
            let in_v8 = v8::Type::cast(n.clone());
            prop_assert_eq!(in_v8.is_some(), n.kind() != SyntaxKind::WITH_NULL_TYPE);
            if let Some(v) = in_v8 {
                prop_assert_eq!(v.syntax(), &n);
            }
        }
    }
}

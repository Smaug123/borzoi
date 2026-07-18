//! Stages 3.3a / 3.3d — suspended-member (`HasMember`) worklist: differential and
//! behaviour tests for member access `recv.Name` (3.3a — a field or non-indexer
//! instance property) and a method **call** `recv.Method(args)` (3.3d — a
//! single-candidate, non-generic instance method, typed as its return type), both
//! on a non-generic `Ty::Named` receiver resolved through an [`AssemblyEnv`].
//!
//! The differential builds an [`AssemblyEnv`] from a real BCL `System.Runtime.dll`
//! (which carries `System.String`'s `Length` property, `Chars` indexer, and
//! `Empty` static field, plus single-candidate methods like `ToLowerInvariant` /
//! `GetTypeCode`), so our side has the same members FCS references when it
//! type-checks the same script against the SDK's real BCL — the two then agree on
//! `s.Length : System.Int32` (and `s.ToLowerInvariant() : System.String`) at the
//! FCS-reported range. As in [`infer_literals_diff`], we iterate **our** inferred
//! types and assert FCS agrees at that exact range (the D5 soundness direction: we
//! never over-claim).
//!
//! The behaviour tests pin the OUT-of-scope shapes each defer *silently* (never an
//! error, never a guess): an unknown member, a method group, an indexer, a static
//! member, an open receiver, an **overloaded** or **void** method call — plus the
//! poison payoffs (an unwoken member access, or a method-argument parameter use,
//! must block generalisation).

use std::collections::HashMap;

use crate::common::{
    ensure_assembly_fixture_built, ensure_fsharp_core_dll, ensure_system_runtime_dll,
    invoke_fcs_dump, invoke_fcs_dump_with_refs, parse_fcs_types, temp_fs_file,
};
use borzoi_assembly::{Augmentation, Ecma335Assembly, EcmaView};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile, SyntaxKind};
use borzoi_sema::{
    AssemblyEnv, InferredFile, ProjectItems, Resolution, ResolvedFile, infer_file, resolve_file,
};
use rowan::TextRange;

/// An [`AssemblyEnv`] over the real BCL `System.Runtime.dll` — so `System.String`
/// (and its `Length` / `Chars` / `Empty` members) is present.
fn bcl_env() -> AssemblyEnv {
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
}

/// The BCL env **plus the real FSharp.Core** — i.e. what every real F# project's
/// env actually looks like, and the configuration that used to defer *every*
/// overloaded call: FSharp.Core carries assembly-level `[<AutoOpen>]`s, which the
/// OV-6 extension gate treated as a wholesale "an extension might exist" surface.
/// EX-1 makes that gate name-keyed, so the surface now defers only calls of the
/// names it actually declares.
fn bcl_and_fsharp_core_env() -> AssemblyEnv {
    let bcl = ensure_system_runtime_dll();
    // The `netstandard` facade beside it: FSharp.Core's pickle names its BCL
    // abbreviation targets through the `netstandard` CCU, so the
    // primitive-alias chase (`int` → `int32` → a forwarder → `System.Int32`)
    // needs the facade loaded — as in a real project's reference closure.
    let netstd = bcl.parent().expect("ref dir").join("netstandard.dll");
    let core = ensure_fsharp_core_dll();
    let bcl_bytes = std::fs::read(&bcl).expect("read System.Runtime.dll");
    let netstd_bytes = std::fs::read(&netstd).expect("read netstandard.dll");
    let core_bytes = std::fs::read(&core).expect("read FSharp.Core.dll");
    let views = [
        Ecma335Assembly::parse(&bcl_bytes).expect("parse System.Runtime.dll"),
        Ecma335Assembly::parse(&netstd_bytes).expect("parse netstandard.dll"),
        Ecma335Assembly::parse(&core_bytes).expect("parse FSharp.Core.dll"),
    ];
    AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
}

/// EX-1's headline: with **FSharp.Core in the env**, an overloaded BCL call whose
/// name no in-scope extension declares now **commits**, and FCS agrees.
///
/// This is the coverage OV-9 measured as structurally unreachable: FSharp.Core's
/// implicit auto-opens are always an extension *surface*, so the presence-based
/// gate deferred every overloaded call in every real project. But an in-scope
/// extension joins **its own name's** group and competes only there (probed —
/// `docs/extension-scope-enumeration-plan.md` §1), and FSharp.Core's auto-opened
/// extension names are exotic (`AsyncRead`, `GetReverseIndex`, … — plan §6.1(c)),
/// so a call to `Substring` / `StartsWith` / `IndexOf` is provably unaffected by
/// them and is safe to commit.
#[test]
fn fsharp_core_env_no_longer_defers_unrelated_overload_names() {
    for (src, binder, expected) in [
        (
            "module M\nlet s = \"hi\"\nlet a = s.Substring(1)\n",
            "a",
            "System.String",
        ),
        (
            "module M\nlet s = \"hi\"\nlet b = s.StartsWith \"h\"\n",
            "b",
            "System.Boolean",
        ),
        (
            "module M\nlet s = \"hi\"\nlet c = s.IndexOf('h')\n",
            "c",
            "System.Int32",
        ),
    ] {
        let env = bcl_and_fsharp_core_env();
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let resolved = resolve_file(&file, &ProjectItems::default(), &env);
        let inferred = infer_file(&file, &resolved, &env);
        let def_types: HashMap<String, String> = inferred
            .def_types()
            .iter()
            .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
            .collect();
        assert_eq!(
            def_types.get(binder).map(String::as_str),
            Some(expected),
            "with FSharp.Core in the env, `{binder}` must still commit (EX-1: the \
             auto-open surface declares no extension of this name): {src:?}"
        );

        // And FCS agrees at every range we published (D5) — FCS references
        // FSharp.Core when it checks the snippet, so this is apples to apples.
        let path = temp_fs_file("ex1_fsharp_core", src);
        let json = invoke_fcs_dump("types", &path);
        let _ = std::fs::remove_file(&path);
        let fcs = parse_fcs_types(&json, src);
        for (range, ty) in inferred.types() {
            let key = (usize::from(range.start()), usize::from(range.end()));
            let fcs_ty = fcs
                .get(&key)
                .unwrap_or_else(|| panic!("we typed {key:?} but FCS has no node there in {src:?}"));
            assert_eq!(&ty.render(), fcs_ty, "type mismatch at {key:?} in {src:?}");
        }
    }
}

/// The BCL env plus the C# assembly fixture, whose `ExtColl` namespace carries the
/// EX-2 landmine — a C#-style `[<Extension>]` on `string` named `Substring`. FCS is
/// given the same fixture as a reference, so the two sides see the same extension
/// surface once `open ExtColl` brings it in.
fn bcl_and_assembly_fixture_env() -> AssemblyEnv {
    let bcl = ensure_system_runtime_dll();
    let fixture = ensure_assembly_fixture_built();
    let bcl_bytes = std::fs::read(&bcl).expect("read System.Runtime.dll");
    let fixture_bytes = std::fs::read(fixture).expect("read SemaAssemblyEnvFixture.dll");
    let views = [
        Ecma335Assembly::parse(&bcl_bytes).expect("parse System.Runtime.dll"),
        Ecma335Assembly::parse(&fixture_bytes).expect("parse SemaAssemblyEnvFixture.dll"),
    ];
    AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
}

/// EX-2 (`docs/extension-scope-enumeration-plan.md`): an explicit `open
/// <namespace>` makes that assembly namespace's extension members in scope, so the
/// overload gate must now defer a call whose name one of them declares — **and no
/// longer defer every other call in the file**, which is the coverage the pre-EX-2
/// "any `open` ⇒ defer" gate lost.
///
/// The fixture's `ExtColl.StringExts` is a C#-style extension class adding
/// `Substring(this string, double)` — colliding with `String.Substring`'s intrinsic
/// overload set — and nothing named `IndexOf`. So:
///
/// - `s.IndexOf('h')` (no colliding extension) **commits**, with *and* without the
///   `open`: opening an extension-bearing namespace no longer poisons an unrelated
///   name.
/// - `s.Substring(1)` **commits without the `open`** (the extension is not in scope)
///   but **defers with it** — the gate's decision now turns on the name, and the
///   `open` changes it.
/// - `s.Substring(1.5)` with the `open` is the landmine: FCS resolves it to the
///   *extension* (the `double` overload, the intrinsics being inapplicable) →
///   `System.Int64`, while our single-candidate arity shortcut would otherwise name
///   the inapplicable intrinsic `Substring(int)` → `System.String`. The gate must
///   defer; the D5 net below catches a wrong commit as a type mismatch at the
///   call's range.
#[test]
fn open_of_extension_bearing_namespace_defers_only_colliding_names() {
    // (source, binder, Some(committed type) | None if it must defer)
    let cases: &[(&str, &str, Option<&str>)] = &[
        // ── Coverage: a non-colliding name commits, open or not. ──────────────
        (
            "module M\nlet s = \"hi\"\nlet a = s.IndexOf('h')\n",
            "a",
            Some("System.Int32"),
        ),
        (
            "module M\nopen ExtColl\nlet s = \"hi\"\nlet a = s.IndexOf('h')\n",
            "a",
            Some("System.Int32"),
        ),
        // ── The name whose decision the open flips: committed without it… ─────
        (
            "module M\nlet s = \"hi\"\nlet a = s.Substring(1)\n",
            "a",
            Some("System.String"),
        ),
        // …deferred with it (the extension `Substring` is now in scope). ───────
        (
            "module M\nopen ExtColl\nlet s = \"hi\"\nlet a = s.Substring(1)\n",
            "a",
            None,
        ),
        // ── The landmine: FCS picks the extension; we must defer, never commit
        //    the inapplicable intrinsic. ────────────────────────────────────────
        (
            "module M\nopen ExtColl\nlet s = \"hi\"\nlet a = s.Substring(1.5)\n",
            "a",
            None,
        ),
    ];

    let fixture = ensure_assembly_fixture_built();
    for (src, binder, expected) in cases {
        let env = bcl_and_assembly_fixture_env();
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors in {src:?}: {:?}",
            parsed.errors
        );
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let resolved = resolve_file(&file, &ProjectItems::default(), &env);
        let inferred = infer_file(&file, &resolved, &env);
        let def_types: HashMap<String, String> = inferred
            .def_types()
            .iter()
            .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
            .collect();
        assert_eq!(
            def_types.get(*binder).map(String::as_str),
            *expected,
            "EX-2 commit/defer decision wrong for `{binder}` in {src:?}"
        );

        // D5 soundness: FCS references the same fixture, so every type we publish
        // must match FCS at its exact range. This is what catches a WRONG-OVERLOAD
        // commit — were the gate to miss the opened `Substring` extension, we would
        // publish `System.String` at the `Substring(1.5)` call where FCS has
        // `System.Int64`, and this loop would fail.
        let path = temp_fs_file("ex2_open_ext", src);
        let json = invoke_fcs_dump_with_refs("types", &path, &[fixture]);
        let _ = std::fs::remove_file(&path);
        let fcs = parse_fcs_types(&json, src);
        for (range, ty) in inferred.types() {
            let key = (usize::from(range.start()), usize::from(range.end()));
            let fcs_ty = fcs
                .get(&key)
                .unwrap_or_else(|| panic!("we typed {key:?} but FCS has no node there in {src:?}"));
            assert_eq!(&ty.render(), fcs_ty, "type mismatch at {key:?} in {src:?}");
        }
    }
}

/// EX-2 soundness (codex P1): a dropped TypeDef at a **prefix split** of an opened
/// namespace path must make the open extension-unknowable, even though a namespace
/// reading survives at the exact path. `open A.B.C` with a dropped type at `A.B`
/// may be opening a same-FQN *module* `A.B.C` FCS merges into scope, whose
/// extensions are invisible; and `extension_named_in_scope` queries only `A.B.C`,
/// where the marker (recorded under its enclosing namespace `A.B`) does not sit.
///
/// Driven at the resolver, no FCS: `mark_namespace_dropped_type` is what the LSP
/// host calls after projection, so setting it directly reproduces the drop.
#[test]
fn open_of_namespace_with_a_dropped_prefix_split_is_unknowable() {
    let fixture = ensure_assembly_fixture_built();
    let fixture_bytes = std::fs::read(fixture).expect("read SemaAssemblyEnvFixture.dll");
    let build_env = || {
        let view = Ecma335Assembly::parse(&fixture_bytes).expect("parse fixture");
        AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
    };
    // `Demo.Sub.Extra` is a real namespace in the C# fixture.
    let src = "module M\nopen Demo.Sub.Extra\n";
    let opened = vec!["Demo".to_owned(), "Sub".to_owned(), "Extra".to_owned()];

    // Control: with no drop, the open is name-keyed to the exact namespace.
    let clean_env = build_env();
    let parsed = parse(src);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let clean = resolve_file(&file, &ProjectItems::default(), &clean_env);
    assert!(
        !clean.open_extension_unknowable(),
        "a clean `open <namespace>` is name-keyed, not unknowable"
    );
    assert_eq!(
        clean.open_extension_namespaces(),
        std::slice::from_ref(&opened),
        "the opened assembly namespace is exported by name"
    );

    // A dropped type at the prefix split `Demo.Sub` must flip the open to
    // unknowable — the exact path `Demo.Sub.Extra` still resolves, so the value-side
    // `names_uncovered_dropped_path` alone would not catch it.
    let mut dropped_env = build_env();
    dropped_env.mark_namespace_dropped_type(vec!["Demo".to_owned(), "Sub".to_owned()]);
    let dropped = resolve_file(&file, &ProjectItems::default(), &dropped_env);
    assert!(
        dropped.open_extension_unknowable(),
        "a dropped type at a prefix split of the opened namespace must defer the whole file"
    );
}

/// Infer `src` against the BCL env.
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
/// assert the FCS `types` oracle agrees at that exact range (D5 soundness). FCS
/// references the same real BCL, so `s.Length` is `System.Int32` on both sides.
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

    let path = temp_fs_file("member_diff", src);
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

// ===== Differentials =====

#[test]
fn value_receiver_length_matches_fcs() {
    // `let s = "hi"` then `let n = s.Length` ⇒ `n : int`; the whole `s.Length`
    // node is `System.Int32` and the receiver `s` is `System.String`, both agreeing
    // with FCS at their ranges. `n`'s binder type is int.
    let src = "module M\nlet s = \"hi\"\nlet n = s.Length\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("n").map(String::as_str), Some("System.Int32"));
}

// NOTE: an *overridden Object method* (`s.GetHashCode()`) is NOT a clean OV-3
// differential against the ref BCL: `GetHashCode` is an `is_object_method_name`,
// and `System.Object` is only forwarded (not defined) in `System.Runtime.dll`,
// so the chain is `ObjectCapped` and the call defers on that rule regardless of
// the dedup. Narrowing that Object-cap defer (using the dedup to see the
// invisible `Object` member as a duplicate of the visible override) is the
// separate OV-3 side-payoff left for a follow-up. The partial-signature dedup
// itself is pinned by `assembly_env::instance_method_resolves_overridden_method`
// against the controlled `Demo.Base`/`Demo.Derived` fixture (a non-`Object`
// chain, both levels present), where the override's signature is known exactly.

#[test]
fn literal_receiver_length_matches_fcs() {
    // `let n = "hi".Length` (a `DotGet`) ⇒ `n : int`; the `"hi"` literal node is
    // `System.String`, the whole `"hi".Length` is `System.Int32`.
    let src = "module M\nlet n = \"hi\".Length\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("n").map(String::as_str), Some("System.Int32"));
}

#[test]
fn member_result_in_a_tuple_matches_fcs() {
    // `let t = (s.Length, s)` ⇒ the tuple `int * string`, with the `s.Length`
    // element `int` and the receiver / second element `string`. All agree with FCS.
    let src = "module M\nlet s = \"hi\"\nlet t = (s.Length, s)\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("t").map(String::as_str),
        Some("System.Int32 * System.String")
    );
}

#[test]
fn member_result_feeding_an_if_matches_fcs() {
    // `let r = if true then s.Length else 0` ⇒ `r : int`; the then-branch is the
    // member access `s.Length : int`, and the whole `if` is int.
    let src = "module M\nlet s = \"hi\"\nlet r = if true then s.Length else 0\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("r").map(String::as_str), Some("System.Int32"));
}

// ===== Behaviour: OUT cases defer silently =====

#[test]
fn unknown_member_defers() {
    // `s.Nonexistent` — no such member on `System.String`. The lookup misses, so
    // the access defers (D5): `n` is not published, and nothing wrong is emitted.
    let src = "module M\nlet s = \"hi\"\nlet n = s.Nonexistent\n";
    let (inferred, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("n"), None, "unknown member defers");
    // No expression type is emitted for the `s.Nonexistent` result either.
    assert!(
        !inferred
            .types()
            .values()
            .any(|t| t.render() == "System.Int32"),
        "no bogus int leaks from an unknown member"
    );
}

#[test]
fn method_name_defers() {
    // `s.ToString` is a method (an overloaded method group), not a data member —
    // multiple public instance members share the name, so the lookup is ambiguous
    // and defers. (FCS types this as a function value; we say nothing.)
    let src = "module M\nlet s = \"hi\"\nlet m = s.ToString\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("m"), None, "a method name defers");
}

#[test]
fn indexer_property_defers() {
    // `System.String.Chars` is an *indexer* property (it carries an index
    // parameter), not a plain data member — so a member access `s.Chars` defers.
    let src = "module M\nlet s = \"hi\"\nlet c = s.Chars\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("c"), None, "an indexer property defers");
}

#[test]
fn open_receiver_defers() {
    // An unannotated parameter used as a receiver (`x.Length`) has an *open*
    // receiver var, so the `HasMember` never wakes — the access defers, and the
    // function stays untyped (its parameter type is unknown).
    let src = "module M\nlet f x = x.Length\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("f"), None, "an open receiver defers");
}

#[test]
fn static_member_resolution_is_unaffected() {
    // `System.String.Empty` is a fully-qualified *static* path — the resolver
    // records a `Member` resolution for it (not something our member-access typing
    // touches, since the head `System` is not an in-file value binder). Our
    // inference must not type `x` via the member-access path (no regression): the
    // static field access is out of scope, so `x` defers.
    let src = "module M\nlet x = System.String.Empty\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("x"),
        None,
        "a static member access is not typed by member-access inference"
    );
}

// ===== Behaviour: the poison payoff =====

#[test]
fn unwoken_member_access_blocks_generalisation() {
    // `let f x = (x.Foo, x)`: the member access `x.Foo` on the open parameter `x`
    // never wakes (x's head is not concrete), poisoning both `x` and the member
    // result. The function is otherwise a complete tuple binding, but the poison
    // blocks generalisation — FCS would infer a member constraint on `x`, not a
    // free `'a`, so a bogus `'a -> 'b * 'a` scheme must not be published. We defer.
    let src = "module M\nlet f x = (x.Foo, x)\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("f"),
        None,
        "an unwoken member access must block generalisation of the receiver"
    );
}

#[test]
fn cross_binding_ground_receiver_wakes() {
    // The cross-binding case: `let s = "hi"` grounds `s` in its own batch, so when
    // `let n = s.Length` is generated in a *later* binding, the receiver is already
    // `System.String` — the member wakes immediately. This pins that a receiver
    // from an earlier binding drives the worklist.
    let (_, def_types) = infer_bcl("module M\nlet s = \"hi\"\nlet n = s.Length\n");
    assert_eq!(def_types.get("n").map(String::as_str), Some("System.Int32"));
}

#[test]
fn multi_dot_chain_wakes_segment_by_segment() {
    // `let s = "hi"` then `let n = s.Length.ToString` — wait: chain a member on a
    // member result. `s.Length : int`; `int` has no *data* member `Foo`, so the
    // second link defers. But the first link still types the intermediate. We pin
    // that a chain does not panic and defers where the second member is absent.
    let (_, def_types) = infer_bcl("module M\nlet s = \"hi\"\nlet n = s.Length.Nope\n");
    assert_eq!(def_types.get("n"), None, "the second chain link defers");
}

// ===== Stage 3.3b: member_resolutions side-table =====

/// [`infer_bcl_full`] over [`bcl_and_fsharp_core_env`] — for tests whose
/// annotations (`let n : int = …`) type through FSharp.Core's abbreviation
/// markers, which the BCL-only env cannot supply.
fn infer_core_full(src: &str) -> (InferredFile, ResolvedFile) {
    let env = bcl_and_fsharp_core_env();
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    (inferred, resolved)
}

/// Infer `src` against the BCL env, returning the resolved file too so a test can
/// locate ranges and cross-check `member_resolutions`.
fn infer_bcl_full(src: &str) -> (InferredFile, ResolvedFile) {
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
    (inferred, resolved)
}

/// The source range of the first `IDENT_TOK` with text `name` in `src`, found by
/// re-parsing (the same tree inference ran over). Panics if absent.
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

#[test]
fn member_resolution_recorded_on_wake() {
    // On a successful `HasMember` wake, the resolved member identity is recorded
    // at the member-name use range — the same `Resolution::Member` shape the
    // resolver produces for a static path, so every LSP path stays uniform.
    let src = "module M\nlet s = \"hi\"\nlet n = s.Length\n";
    let (inferred, _) = infer_bcl_full(src);
    let length = ident_range(src, "Length");
    let res = inferred
        .member_resolution_at(length)
        .expect("member resolution recorded at `Length`");
    match res {
        Resolution::Member { .. } => {}
        other => panic!("expected Resolution::Member, got {other:?}"),
    }
}

#[test]
fn member_resolution_recorded_for_literal_receiver() {
    // The `DotGet` shape (`"hi".Length`) records the member resolution too.
    let src = "module M\nlet n = \"hi\".Length\n";
    let (inferred, _) = infer_bcl_full(src);
    let length = ident_range(src, "Length");
    assert!(
        matches!(
            inferred.member_resolution_at(length),
            Some(Resolution::Member { .. })
        ),
        "member resolution recorded for a literal receiver"
    );
}

#[test]
fn no_member_resolution_on_defer() {
    // An unknown member never wakes, so nothing is recorded at its range (D5).
    let src = "module M\nlet s = \"hi\"\nlet n = s.Nonexistent\n";
    let (inferred, _) = infer_bcl_full(src);
    let seg = ident_range(src, "Nonexistent");
    assert_eq!(
        inferred.member_resolution_at(seg),
        None,
        "an unwoken member records no resolution"
    );
}

#[test]
fn no_member_resolution_on_ambiguity() {
    // A method group (`s.ToString`) is ambiguous — the wake looks it up, finds
    // more than one public instance member, and declines. No resolution recorded.
    let src = "module M\nlet s = \"hi\"\nlet m = s.ToString\n";
    let (inferred, _) = infer_bcl_full(src);
    let seg = ident_range(src, "ToString");
    assert_eq!(
        inferred.member_resolution_at(seg),
        None,
        "an ambiguous member records no resolution"
    );
}

#[test]
fn no_member_resolution_on_open_receiver() {
    // An open receiver (`x.Length` on an unannotated parameter) never wakes.
    let src = "module M\nlet f x = x.Length\n";
    let (inferred, _) = infer_bcl_full(src);
    assert!(
        inferred.member_resolutions().is_empty(),
        "an open receiver records no member resolution"
    );
}

// ===== Stage 3.3d: single-candidate method-call typing =====

#[test]
fn value_receiver_method_call_matches_fcs() {
    // `let s = "hi"` then `let a = s.ToLowerInvariant()` ⇒ `a : string`. The whole
    // `s.ToLowerInvariant()` application node is `System.String` (the method's return
    // type) and the receiver `s` is `System.String`, both agreeing with FCS.
    // `ToLowerInvariant` is a single-candidate instance method.
    let src = "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant()\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("a").map(String::as_str),
        Some("System.String")
    );
}

#[test]
fn literal_receiver_method_call_matches_fcs() {
    // The `DotGet` shape (`"hi".ToLowerInvariant()`) ⇒ `string`.
    let src = "module M\nlet a = \"hi\".ToLowerInvariant()\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("a").map(String::as_str),
        Some("System.String")
    );
}

#[test]
fn method_call_named_return_matches_fcs() {
    // A method whose return is a *named* type (not a primitive): `s.GetTypeCode()`
    // ⇒ `System.TypeCode`, the same non-generic-named bridge fields use.
    let src = "module M\nlet s = \"hi\"\nlet c = s.GetTypeCode()\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("c").map(String::as_str),
        Some("System.TypeCode")
    );
}

#[test]
fn multi_parameter_c_sharp_method_call_commits() {
    // OV-6.1: a multi-parameter method from a **non-F#** assembly is provably a
    // single argument group (C#/VB cannot curry), so the projector stamps it
    // `arg_group_count: Some(1)` and the curried-member gate lets it commit. The
    // BCL's `String.Insert(int, string)` is such a method; FCS types
    // `s.Insert(0, "z")` `System.String`, and so do we — restoring the coverage
    // OV-6 conservatively deferred as "possibly curried". See
    // `docs/completed/ov-6.1-curry-detection-plan.md`.
    let src = "module M\nlet s = \"hi\"\nlet r = s.Insert(0, \"z\")\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("r").map(String::as_str),
        Some("System.String"),
        "a multi-parameter C# method is a single argument group and commits"
    );
}

#[test]
fn method_result_in_a_tuple_matches_fcs() {
    // `(s.ToUpperInvariant(), s)` ⇒ `string * string`; the method result and the
    // receiver both `string`, agreeing with FCS at their ranges.
    let src = "module M\nlet s = \"hi\"\nlet t = (s.ToUpperInvariant(), s)\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("t").map(String::as_str),
        Some("System.String * System.String")
    );
}

#[test]
fn method_then_member_chain_matches_fcs() {
    // A method result feeds a further member access: `s.ToLowerInvariant().Length`
    // ⇒ `int`. The call grounds to `string` (the worklist wakes the method), then
    // `.Length` wakes on the now-ground `string` — the chain resolves through the
    // fixpoint, no special deep-chain machinery.
    let src = "module M\nlet s = \"hi\"\nlet n = s.ToLowerInvariant().Length\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("n").map(String::as_str), Some("System.Int32"));
}

#[test]
fn method_resolution_recorded_on_wake() {
    // A successful method-call wake records the method at its name range in the same
    // `Resolution::Member` shape a field does, so the 3.3b LSP hover / go-to-def
    // path serves a called method name with no LSP change.
    let src = "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant()\n";
    let (inferred, _) = infer_bcl_full(src);
    assert!(
        matches!(
            inferred.member_resolution_at(ident_range(src, "ToLowerInvariant")),
            Some(Resolution::Member { .. })
        ),
        "a method resolution is recorded on a successful wake"
    );
}

// ===== Stage 3.x-inh: inherited (base-class-walk) method calls =====

#[test]
fn inherited_method_call_matches_fcs() {
    // `s.GetType()` — `GetType` is declared on `System.Object` and *inherited* by
    // `System.String` (not overridden). The 3.3d exact-entity scan misses it (String
    // has no `GetType`); the base-class walk finds `Object.GetType()` — a single
    // candidate across the whole `String → Object` chain — and types the call as its
    // return `System.Type`, agreeing with FCS.
    let src = "module M\nlet s = \"hi\"\nlet t = s.GetType()\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("t").map(String::as_str), Some("System.Type"));
}

#[test]
fn inherited_method_call_through_value_type_matches_fcs() {
    // A *multi-level* chain: `n : int` is `System.Int32 → System.ValueType →
    // System.Object`. `GetType` is declared only on `Object`, three hops up — the
    // walk resolves each non-generic base in turn and finds the single candidate,
    // typing `n.GetType()` ⇒ `System.Type`, agreeing with FCS.
    let src = "module M\nlet n = 1\nlet t = n.GetType()\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("t").map(String::as_str), Some("System.Type"));
}

#[test]
fn inherited_method_resolution_recorded() {
    // The inherited call records a `Resolution::Member` at the method-name range, so
    // hover / go-to-def light up on `GetType` (pointing at the declaring base type).
    let src = "module M\nlet s = \"hi\"\nlet t = s.GetType()\n";
    let (inferred, _) = infer_bcl_full(src);
    assert!(
        matches!(
            inferred.member_resolution_at(ident_range(src, "GetType")),
            Some(Resolution::Member { .. })
        ),
        "an inherited method resolution is recorded on wake"
    );
}

#[test]
fn overloaded_across_chain_zero_arg_commits() {
    // OV-6 (probe P13): `s.ToString()` — `String` declares `ToString()` and
    // `ToString(IFormatProvider)` (`Object.ToString()` dedups against the override).
    // The complete group is ≥ 2, but the one-parameter overload is arity-refuted at a
    // zero-argument call, leaving `ToString()` the unique applicable candidate that
    // `must_apply` affirms. The engine commits it ⇒ `r : System.String`, agreeing
    // with FCS, and records the chosen member.
    let src = "module M\nlet s = \"hi\"\nlet r = s.ToString()\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("r").map(String::as_str),
        Some("System.String"),
        "the zero-arg call refutes the format-provider overload (OV-6)"
    );
    let (inferred, _) = infer_bcl_full(src);
    assert!(
        matches!(
            inferred.member_resolution_at(ident_range(src, "ToString")),
            Some(Resolution::Member { .. })
        ),
        "a committed overload records its chosen member"
    );
}

#[test]
fn overloaded_call_type_refuted_loser_commits() {
    // OV-6 (probe P12 flavour): `s.StartsWith "h"` — `StartsWith` is overloaded on
    // `String` with `StartsWith(string)`, `StartsWith(char)`, and multi-argument
    // forms. A `string` argument arity-refutes the multi-arg overloads and
    // **type-refutes** `StartsWith(char)` (no `string → char` channel), leaving
    // `StartsWith(string)` the unique applicable candidate `must_apply` affirms ⇒
    // `b : System.Boolean`, agreeing with FCS.
    let src = "module M\nlet s = \"hi\"\nlet b = s.StartsWith \"h\"\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("b").map(String::as_str),
        Some("System.Boolean"),
        "the char overload is type-refuted, leaving StartsWith(string) (OV-6)"
    );
}

#[test]
fn overloaded_call_with_char_arg_type_refutes_string_overload() {
    // The complementary type-prong direction: `s.IndexOf('h')` — a `char` argument
    // refutes `IndexOf(string)` (no `char → string` channel) and arity-refutes the
    // multi-argument `IndexOf(char, …)` forms, leaving `IndexOf(char)` the unique
    // applicable candidate ⇒ `n : System.Int32`, agreeing with FCS.
    let src = "module M\nlet s = \"hi\"\nlet n = s.IndexOf('h')\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("n").map(String::as_str),
        Some("System.Int32"),
        "the string overload is type-refuted, leaving IndexOf(char) (OV-6)"
    );
}

#[test]
fn overloaded_call_with_unrefutable_obj_loser_defers() {
    // OV-6 (probe P5's `M("hi")` flavour): `s.Equals("y")` — the group is
    // `Equals(object)` (String's `Object.Equals` override) and `Equals(string)`
    // (plus arity-refuted multi-arg forms). A `string` argument is applicable to
    // **both** the `string` and the `obj` overload (`string :> obj` boxes), and an
    // `obj` parameter is *open* — `may_apply` cannot refute it. Two survivors ⇒ FCS
    // would run betterness (which v1 does not model) ⇒ defer. `n` is not published
    // and no resolution is recorded.
    let src = "module M\nlet s = \"hi\"\nlet n = s.Equals(\"y\")\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("n"),
        None,
        "an unrefutable obj-parameter overload keeps the group ambiguous (OV-6)"
    );
    let (inferred, _) = infer_bcl_full(src);
    assert_eq!(
        inferred.member_resolution_at(ident_range(src, "Equals")),
        None,
        "a deferred overload records no resolution"
    );
}

// ===== Behaviour: the extension-absence gate (OV-6 §4.1(4)) =====

#[test]
fn explicit_open_of_a_clean_namespace_commits() {
    // EX-2 (`docs/extension-scope-enumeration-plan.md`): an `open <namespace>` is
    // now name-keyed — it defers only calls whose name that namespace's extensions
    // declare, not every call in the file (which is what the pre-EX-2 "any open ⇒
    // defer" gate did). `System` declares no `Substring` extension, so `open
    // System; s.Substring(1)` commits, and FCS agrees. The colliding-name and P15
    // landmine directions are pinned by
    // `open_of_extension_bearing_namespace_defers_only_colliding_names`.
    let src = "module M\nopen System\nlet s = \"hi\"\nlet n = s.Substring(1)\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("n").map(String::as_str),
        Some("System.String"),
        "EX-2: opening a namespace with no `Substring` extension still commits the overload"
    );
    assert_sound(src);
}

#[test]
fn same_file_extension_declaration_defers_the_overload_gate() {
    // OV-6 review (GPT-5.6): a same-file C#-style `[<Extension>]` declaration with a
    // FULLY-QUALIFIED attribute (so there is no `open` node, and it is not a
    // `type … with` augmentation) is in scope and can beat a less-specific
    // intrinsic overload — so any `[<Extension>]` in the file forces a defer.
    let src = "module M\n\
               [<System.Runtime.CompilerServices.Extension>]\n\
               type Exts =\n    static member Foo(x: int) : int = x\n\
               let s = \"hi\"\nlet n = s.Substring(1)\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("n"),
        None,
        "a same-file [<Extension>] declaration makes the extension scope unknowable"
    );
    // Control: drop the `[<Extension>]` attribute (a plain type) → the overload
    // commits again, so the deferral is attributable to the extension attribute.
    let control = "module M\n\
                   type Exts =\n    static member Foo(x: int) : int = x\n\
                   let s = \"hi\"\nlet n = s.Substring(1)\n";
    let (_, control_types) = infer_bcl(control);
    assert_eq!(
        control_types.get("n").map(String::as_str),
        Some("System.String"),
        "the same file without [<Extension>] commits the overload"
    );
}

/// A BCL env with a synthetic C#-style `[<Extension>]` class
/// `<namespace>.Extensions.Substring(this String, string)` grafted in — an
/// extension member of `Substring` sitting in `namespace`.
fn env_with_substring_extension_in(namespace: &[&str]) -> AssemblyEnv {
    use borzoi_assembly::{
        Access, Entity, Member, Nullability, ParamDefault, Parameter, Primitive, TypeRef,
    };
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let asm = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let mut entities = asm
        .enumerate_type_defs()
        .expect("enumerate System.Runtime types");
    // Clone a real public instance method as a template, then reshape it into a
    // static `[<Extension>]` `Substring(String, string)` on a new `Demo.Extensions`.
    let template = entities
        .iter()
        .find(|e| e.namespace == ["System"] && e.name == "String")
        .and_then(|s| {
            s.members.iter().find_map(|m| match m {
                Member::Method(mm) if !mm.is_static && mm.access == Access::Public => {
                    Some(mm.clone())
                }
                _ => None,
            })
        })
        .expect("a public instance method on String to clone");
    let mut ext = template;
    ext.name = "Substring".to_string();
    ext.source_name = None;
    ext.is_static = true;
    ext.is_extension_method = true;
    ext.generic_parameters = vec![];
    let string_param = |ty| Parameter {
        name: None,
        ty,
        is_byref: false,
        is_out: false,
        is_readonly_ref: false,
        default: ParamDefault::None,
        is_param_array: false,
        nullability: Nullability::Oblivious,
    };
    ext.signature.parameters = vec![
        string_param(TypeRef::Primitive(Primitive::String)),
        string_param(TypeRef::Primitive(Primitive::String)),
    ];
    // A dummy `Demo.Extensions` static class carrying the extension.
    let template_ent = entities
        .iter()
        .find(|e| e.namespace == ["System"] && e.name == "String")
        .expect("String entity template")
        .clone();
    let extensions = Entity {
        namespace: namespace.iter().map(|s| (*s).to_string()).collect(),
        name: "Extensions".to_string(),
        members: vec![Member::Method(ext)],
        nested_types: vec![],
        base_type: None,
        interfaces: vec![],
        extension_member_names: vec![],
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        ..template_ent
    };
    entities.push(extensions);
    AssemblyEnv::from_entities(entities)
}

#[test]
fn enclosing_namespace_extension_defers_the_overload_gate() {
    // OV-6 review (GPT-5.6): F# treats the file's enclosing namespace as an
    // extension-method scope with no explicit `open`. Inside `namespace Demo`, the
    // referenced `Demo.Extensions.Substring(this String, string)` competes for
    // `s.Substring(1)`, so the gate (checking the enclosing namespace) must defer —
    // even though there is no `open` and no augmentation in the file.
    let env = env_with_substring_extension_in(&["Demo"]);
    let src = "namespace Demo\nmodule M =\n    let s = \"hi\"\n    let n = s.Substring(1)\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        def_types.get("n"),
        None,
        "an extension in the enclosing namespace makes the scope unknowable"
    );

    // Control: the same file in an UNRELATED namespace (no `Demo` extension in
    // scope) commits the intrinsic `Substring(int)`.
    let control = "namespace Other\nmodule M =\n    let s = \"hi\"\n    let n = s.Substring(1)\n";
    let cparsed = parse(control);
    let cfile = ImplFile::cast(cparsed.root).expect("impl file");
    let cresolved = resolve_file(&cfile, &ProjectItems::default(), &env);
    let cinferred = infer_file(&cfile, &cresolved, &env);
    let ctypes: HashMap<String, String> = cinferred
        .def_types()
        .iter()
        .map(|(id, ty)| (cresolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        ctypes.get("n").map(String::as_str),
        Some("System.String"),
        "a file in an unrelated namespace still commits the overload"
    );
}

/// A BCL env with a synthetic single-candidate `[<ParamArray>]` instance method
/// `V([<ParamArray>] xs: int[]) : bool` grafted onto `System.String`.
fn string_env_with_params_method(name: &str) -> AssemblyEnv {
    use borzoi_assembly::{Access, Member, Nullability, ParamDefault, Parameter, TypeRef};
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let asm = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let mut entities = asm
        .enumerate_type_defs()
        .expect("enumerate System.Runtime types");
    let string_ent = entities
        .iter_mut()
        .find(|e| e.namespace == ["System"] && e.name == "String")
        .expect("System.String entity");
    let mut m = string_ent
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(mm)
                if !mm.is_static && !mm.is_constructor && mm.access == Access::Public =>
            {
                Some(mm.clone())
            }
            _ => None,
        })
        .expect("a public instance method on String to clone");
    m.name = name.to_string();
    m.source_name = None;
    m.generic_parameters = vec![];
    m.signature.return_type = TypeRef::Primitive(borzoi_assembly::Primitive::Bool);
    m.signature.parameters = vec![Parameter {
        name: None,
        ty: TypeRef::Array {
            element: Box::new(borzoi_assembly::NullableType {
                ty: TypeRef::Primitive(borzoi_assembly::Primitive::I4),
                nullability: Nullability::Oblivious,
            }),
            rank: 1,
            sizes: vec![],
            lower_bounds: vec![],
        },
        is_byref: false,
        is_out: false,
        is_readonly_ref: false,
        default: ParamDefault::None,
        is_param_array: true,
        nullability: Nullability::Oblivious,
    }];
    string_ent.members.push(Member::Method(m));
    AssemblyEnv::from_entities(entities)
}

#[test]
fn single_params_method_routes_through_the_matcher() {
    // OV-6 review (GPT-5.6): a single declared `[<ParamArray>]` method is *two* FCS
    // candidates (expanded + direct-array), so the arity-only single-candidate
    // shortcut is unsound — it would commit `V("x")` on `V(params int[])` from arity
    // alone. Routed through the matcher: `V(1, 2)` commits (expanded form affirms) ⇒
    // `Bool`, while `V("x")` refutes both forms and defers.
    let env = string_env_with_params_method("V");
    let infer = |src: &str| {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let resolved = resolve_file(&file, &ProjectItems::default(), &env);
        let inferred = infer_file(&file, &resolved, &env);
        inferred
            .def_types()
            .iter()
            .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
            .collect::<HashMap<String, String>>()
    };
    assert_eq!(
        infer("module M\nlet s = \"hi\"\nlet n = s.V(1, 2)\n")
            .get("n")
            .map(String::as_str),
        Some("System.Boolean"),
        "V(1, 2) affirms the expanded params form"
    );
    assert_eq!(
        infer("module M\nlet s = \"hi\"\nlet n = s.V(\"x\")\n").get("n"),
        None,
        "V(\"x\") is applicable to neither the expanded nor the direct-array form"
    );
}

#[test]
fn interface_receiver_method_call_defers() {
    // OV-6 review (GPT-5.6): FCS builds an interface receiver's method group from
    // `System.Object`'s members *plus* all transitively inherited interfaces (§2.1),
    // but `base_chain` walks only `base_type` — which an interface lacks. With
    // `IDerived : IBase`, `IDerived.M(string)` and `IBase.M(int)`, the group would be
    // `IDerived`'s alone, and `i.M(1)` would wrongly commit `IDerived.M`'s `String`
    // (FCS picks the inherited `IBase.M(int)` ⇒ `Int32`). Interface receivers defer
    // outright (§5) until the interface hierarchy walk lands.
    use borzoi_assembly::{
        Access, Entity, EntityKind, Member, MethodLike, MethodSignature, Nullability, ParamDefault,
        Parameter, Primitive, TypeRef,
    };
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let asm = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let mut entities = asm
        .enumerate_type_defs()
        .expect("enumerate System.Runtime types");
    let template = entities
        .iter()
        .find(|e| e.namespace == ["System"] && e.name == "String")
        .expect("String template")
        .clone();
    let method = |name: &str, param: TypeRef, ret: TypeRef| {
        Member::Method(MethodLike {
            definition_range: None,
            name: name.to_string(),
            access: Access::Public,
            signature: MethodSignature {
                parameters: vec![Parameter {
                    name: None,
                    ty: param,
                    is_byref: false,
                    is_out: false,
                    is_readonly_ref: false,
                    default: ParamDefault::None,
                    is_param_array: false,
                    nullability: Nullability::Oblivious,
                }],
                return_type: ret,
                return_nullability: Nullability::Oblivious,
            },
            arg_group_count: Some(1),
            is_static: false,
            is_virtual: true,
            is_abstract: true,
            is_final: false,
            is_newslot: true,
            is_hide_by_sig: true,
            is_constructor: false,
            is_extension_method: false,
            augmentation: Augmentation::No,
            module_value: None,
            is_module_value_binding: false,
            generic_parameters: vec![],
            obsolete: None,
            experimental: None,
            sets_required_members: false,
            compiler_feature_required: vec![],
            source_name: None,
            custom_attrs: vec![],
            metadata_token: 0,
            implements: vec![],
            unclassified_impls: vec![],
        })
    };
    let iface = |name: &str, members: Vec<Member>, ifaces: Vec<TypeRef>| Entity {
        namespace: vec!["Demo".to_string()],
        name: name.to_string(),
        kind: EntityKind::Interface,
        members,
        nested_types: vec![],
        base_type: None,
        interfaces: ifaces,
        extension_member_names: vec![],
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        ..template.clone()
    };
    let ibase = iface(
        "IBase",
        vec![method(
            "M",
            TypeRef::Primitive(Primitive::I4),
            TypeRef::Primitive(Primitive::I4),
        )],
        vec![],
    );
    let iderived = iface(
        "IDerived",
        vec![method(
            "M",
            TypeRef::Primitive(Primitive::String),
            TypeRef::Primitive(Primitive::String),
        )],
        vec![TypeRef::Named {
            assembly: None,
            namespace: vec!["Demo".to_string()],
            name: "IBase".to_string(),
            type_args: vec![],
            segment_arities: vec![0],
        }],
    );
    entities.push(ibase);
    entities.push(iderived);
    let env = AssemblyEnv::from_entities(entities);

    let src = "module M\nlet f (i: Demo.IDerived) = i.M(1)\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    assert_eq!(
        inferred.member_resolution_at(ident_range(src, "M")),
        None,
        "an interface receiver's method group is incomplete, so the call defers"
    );
    assert!(
        !inferred
            .types()
            .values()
            .any(|t| t.render() == "System.String"),
        "no String leaks from the incomplete interface group"
    );
}

/// Builds an env with a `Demo.C` class carrying one two-parameter method
/// `M(int, int): bool` whose [`MethodLike::arg_group_count`] is `agc`, then infers
/// `let f (c: Demo.C) = c.M(1, 2)` and returns the inferred type of the call range
/// (`None` when the call defers). The two-parameter shape is exactly the
/// curried/tupled ambiguity: `Some(1)` proves a single argument group (commits),
/// `None` leaves it possibly curried (defers). See the OV-6.1 plan.
fn infer_two_param_method_call(agc: Option<usize>) -> Option<String> {
    use borzoi_assembly::{
        Access, Entity, EntityKind, Member, MethodLike, MethodSignature, Nullability, ParamDefault,
        Parameter, Primitive, TypeRef,
    };
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let asm = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let mut entities = asm
        .enumerate_type_defs()
        .expect("enumerate System.Runtime types");
    let template = entities
        .iter()
        .find(|e| e.namespace == ["System"] && e.name == "String")
        .expect("String template")
        .clone();
    let i4_param = || Parameter {
        name: None,
        ty: TypeRef::Primitive(Primitive::I4),
        is_byref: false,
        is_out: false,
        is_readonly_ref: false,
        default: ParamDefault::None,
        is_param_array: false,
        nullability: Nullability::Oblivious,
    };
    let m = Member::Method(MethodLike {
        definition_range: None,
        name: "M".to_string(),
        access: Access::Public,
        signature: MethodSignature {
            parameters: vec![i4_param(), i4_param()],
            return_type: TypeRef::Primitive(Primitive::Bool),
            return_nullability: Nullability::Oblivious,
        },
        arg_group_count: agc,
        is_static: false,
        is_virtual: false,
        is_abstract: false,
        is_final: false,
        is_newslot: false,
        is_hide_by_sig: true,
        is_constructor: false,
        is_extension_method: false,
        augmentation: Augmentation::No,
        module_value: None,
        is_module_value_binding: false,
        generic_parameters: vec![],
        obsolete: None,
        experimental: None,
        sets_required_members: false,
        compiler_feature_required: vec![],
        source_name: None,
        custom_attrs: vec![],
        metadata_token: 0,
        implements: Vec::new(),
        unclassified_impls: Vec::new(),
    });
    let class = Entity {
        namespace: vec!["Demo".to_string()],
        name: "C".to_string(),
        kind: EntityKind::Class,
        members: vec![m],
        nested_types: vec![],
        base_type: None,
        interfaces: vec![],
        extension_member_names: vec![],
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        ..template
    };
    entities.push(class);
    let env = AssemblyEnv::from_entities(entities);

    let src = "module M\nlet f (c: Demo.C) = c.M(1, 2)\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    inferred
        .types()
        .values()
        .map(|t| t.render())
        .find(|r| r == "System.Boolean")
}

#[test]
fn single_group_two_param_method_call_commits() {
    // OV-6.1: a two-parameter method proven to be a single argument group
    // (`arg_group_count: Some(1)`, the C#/VB fact) commits its return type —
    // `c.M(1, 2): bool`.
    assert_eq!(
        infer_two_param_method_call(Some(1)).as_deref(),
        Some("System.Boolean"),
        "a provably single-group two-parameter method commits"
    );
}

#[test]
fn possibly_curried_two_param_method_call_defers() {
    // OV-6.1 (GPT-5.6 curried-loser hole): a two-parameter method whose argument
    // grouping is *unknown* (`arg_group_count: None`, as blanked for every F#
    // assembly) is possibly curried — `member x.M a b` and `member x.M(a, b)` are
    // indistinguishable in metadata — so the whole call defers rather than commit a
    // return FCS would type `obj` under FS0816. This is the *direct* guarantee, not
    // the FSharp.Core auto-open coincidence, so it holds even in this
    // FSharp.Core-free synthetic env. See `docs/completed/ov-6.1-curry-detection-plan.md`.
    assert_eq!(
        infer_two_param_method_call(None),
        None,
        "an unknown-grouping two-parameter method may be curried, so it defers"
    );
}

#[test]
fn aliased_extension_attribute_defers_the_overload_gate() {
    // OV-6 review (GPT-5.6): an attribute may reach `ExtensionAttribute` through a
    // type abbreviation (`type ExtAttr = System.Runtime.CompilerServices.ExtensionAttribute`
    // then `[<ExtAttr>]`) — FCS honours it as an extension declaration, and the alias
    // may even shadow an innocuous name. Matching the *written* name is therefore
    // unsound; the gate defers on **any** attribute. `s.Substring(1)` (otherwise
    // committable) defers here even though no `Extension` token appears.
    let src = "module M\n\
               type ExtAttr = System.Runtime.CompilerServices.ExtensionAttribute\n\
               [<ExtAttr>]\n\
               type Exts =\n    static member Foo(x: int) : int = x\n\
               let s = \"hi\"\nlet n = s.Substring(1)\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("n"),
        None,
        "an aliased [<Extension>] attribute makes the extension scope unknowable"
    );
    // Control: the abbreviation alone (no attribute *use*) still commits.
    let control = "module M\n\
                   type ExtAttr = System.Runtime.CompilerServices.ExtensionAttribute\n\
                   let s = \"hi\"\nlet n = s.Substring(1)\n";
    let (_, control_types) = infer_bcl(control);
    assert_eq!(
        control_types.get("n").map(String::as_str),
        Some("System.String"),
        "an unused abbreviation is not an attribute, so the overload commits"
    );
}

#[test]
fn skipped_member_in_scope_defers_the_overload_gate() {
    // OV-6 review (GPT-5.6): a member the reader could not decode (recorded in
    // `Entity::skipped_members`) may carry `[<Extension>]` — its signature is
    // unknown — so a type with any skipped member in the file's in-scope namespace
    // makes the extension surface uncertain. A root-namespace type with a skipped
    // member therefore defers `s.Substring(1)` in a `module M` file (which the plain
    // BCL env — whose root has no skipped members — commits).
    use borzoi_assembly::{Entity, SkippedMember};
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let asm = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let mut entities = asm
        .enumerate_type_defs()
        .expect("enumerate System.Runtime types");
    let template = entities
        .iter()
        .find(|e| e.namespace == ["System"] && e.name == "String")
        .expect("String template")
        .clone();
    // A dummy root-namespace type carrying an undecodable (skipped) member.
    let with_skip = Entity {
        namespace: vec![],
        name: "RootThing".to_string(),
        members: vec![],
        nested_types: vec![],
        base_type: None,
        interfaces: vec![],
        extension_member_names: vec![],
        union_case_names: None,
        static_extension_member_names: Vec::new(),
        is_extension_container: false,
        skipped_members: vec![SkippedMember {
            name: "MaybeExt".to_string(),
            reason: "unsupported signature element".to_string(),
        }],
        ..template
    };
    entities.push(with_skip);
    let env = AssemblyEnv::from_entities(entities);

    let src = "module M\nlet s = \"hi\"\nlet n = s.Substring(1)\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        def_types.get("n"),
        None,
        "a skipped member (possibly an extension) in the in-scope root namespace defers"
    );
}

#[test]
fn dropped_type_uncertainty_is_namespace_scoped() {
    // OV-6 review (GPT-5.6): a **dropped** type may be a C#-style `[<Extension>]`
    // class the entity tree no longer shows, so its namespace is possibly
    // extension-bearing — but *namespace-scoped*: a `namespace Demo` file defers
    // while a root `module M` file (whose in-scope namespace saw no drop) commits.
    // (This is what keeps method-call hovers alive despite the real BCL dropping
    // ~38 namespaced generic types, none in root.)
    let commit = |env: &AssemblyEnv, src: &str| -> Option<String> {
        let parsed = parse(src);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let resolved = resolve_file(&file, &ProjectItems::default(), env);
        let inferred = infer_file(&file, &resolved, env);
        inferred
            .def_types()
            .iter()
            .find(|(id, _)| resolved.def(**id).name == "n")
            .map(|(_, ty)| ty.render())
    };

    let mut env = bcl_env();
    env.mark_namespace_dropped_type(vec!["Demo".to_string()]);

    // A file in `namespace Demo` sees the dropped-type uncertainty and defers.
    assert_eq!(
        commit(
            &env,
            "namespace Demo\nmodule M =\n    let s = \"hi\"\n    let n = s.Substring(1)\n"
        ),
        None,
        "a drop in the file's in-scope namespace defers the overload"
    );
    // A root `module M` file — the drop was in `Demo`, not root — still commits.
    assert_eq!(
        commit(&env, "module M\nlet s = \"hi\"\nlet n = s.Substring(1)\n").as_deref(),
        Some("System.String"),
        "a drop in an unrelated namespace does not defer a root-namespace file"
    );
}

#[test]
fn unknowable_auto_opens_defer_the_overload_gate() {
    // OV-6 review (GPT-5.6): when a referenced assembly's assembly-level `[<AutoOpen>]`
    // list could not be read, its implicit-open surface is *unknown* — the host marks
    // the env `mark_extension_surface_unknowable`, and the gate must defer (the DLL might
    // auto-open a namespace with a competing extension). `s.Substring(1)`, which
    // commits in the plain BCL env, defers once the env is marked.
    let mut env = bcl_env();
    env.mark_extension_surface_unknowable();
    let src = "module M\nlet s = \"hi\"\nlet n = s.Substring(1)\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        def_types.get("n"),
        None,
        "an unreadable AutoOpen list makes the implicit-open surface unknowable, so the overload defers"
    );
}

#[test]
fn root_namespace_extension_defers_the_overload_gate() {
    // OV-6 review (GPT-5.6): the **root** namespace is always in scope with no
    // `open` (a `module M` file omits it from `namespace_paths`). A referenced
    // assembly's global `[<Extension>]` `Substring` therefore competes for
    // `s.Substring(1)`, so the gate — which checks the root `[]` explicitly — must
    // defer. (The real BCL has *no* root extensions, so ordinary `module M` files
    // still commit — see the many committing differentials above.)
    let env = env_with_substring_extension_in(&[]);
    let src = "module M\nlet s = \"hi\"\nlet n = s.Substring(1)\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        def_types.get("n"),
        None,
        "a referenced extension in the always-in-scope root namespace defers"
    );
}

#[test]
fn cross_file_extension_class_defers_the_overload_gate() {
    // OV-6 review (GPT-5.6): a preceding file's C#-style `[<Extension>]` class (not
    // an auto-open module) is a project extension source a later same-namespace file
    // sees with no `open`. The compile-order cross-file signal defers the later
    // file; the reversed order (call file first) commits.
    use borzoi_sema::resolve_project;
    let env = bcl_env();
    let f1 = "module M1\n\
              [<System.Runtime.CompilerServices.Extension>]\n\
              type Exts =\n    static member Foo(x: int) : int = x\n";
    let f2 = "module M2\nlet s = \"hi\"\nlet n = s.Substring(1)\n";
    let parse_impl = |src: &str| {
        let p = parse(src);
        assert!(p.errors.is_empty(), "parse errors: {:?}", p.errors);
        ImplFile::cast(p.root).expect("impl file")
    };

    let files = [parse_impl(f1), parse_impl(f2)];
    let project = resolve_project(&files, &env);
    let f2_resolved = project.file(1);
    let inferred = infer_file(&files[1], f2_resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (f2_resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        def_types.get("n"),
        None,
        "a preceding-file [<Extension>] class defers the later file's overload"
    );

    // Reversed Compile order: the call file precedes the `[<Extension>]` file, so
    // no *preceding* extension source is in scope — the overload commits.
    let reordered = [parse_impl(f2), parse_impl(f1)];
    let project2 = resolve_project(&reordered, &env);
    let call_first = project2.file(0);
    let inferred2 = infer_file(&reordered[0], call_first, &env);
    let types2: HashMap<String, String> = inferred2
        .def_types()
        .iter()
        .map(|(id, ty)| (call_first.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        types2.get("n").map(String::as_str),
        Some("System.String"),
        "a file compiled before the [<Extension>] class still commits the overload"
    );
}

#[test]
fn in_file_augmentation_defers_the_overload_gate() {
    // EX-3 §2(a): an in-file type augmentation contributes exactly its member
    // names to the extension scope. `type Foo with member _.Y` is unrelated to
    // `Substring`, so `s.Substring(1)` now COMMITS (this test pinned the old
    // wholesale over-approximation, which §2(a) removes); an augmentation
    // declaring `Substring` itself still defers it.
    let src = "module M\ntype Foo() =\n    member _.X = 1\ntype Foo with\n    member _.Y = 2\nlet s = \"hi\"\nlet n = s.Substring(1)\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("n").map(String::as_str),
        Some("System.String"),
        "an augmentation of an unrelated name no longer defers the call"
    );
    let src = "module M\ntype Foo() =\n    member _.X = 1\ntype Foo with\n    member _.Substring = 2\nlet s = \"hi\"\nlet n = s.Substring(1)\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("n"),
        None,
        "an augmentation declaring `Substring` still defers the call"
    );
}

// ===== Behaviour: OUT cases defer silently =====

#[test]
fn overloaded_call_arity_refuted_loser_commits() {
    // OV-6 (probe P4): `s.Substring(1)` — `Substring(int)` / `Substring(int, int)`
    // is a genuine overload set, but the two-argument overload is arity-refuted at a
    // one-argument call (it has no optional / `params` shape), leaving `Substring(int)`
    // the unique applicable candidate, which `must_apply` affirms (ground `int` arg,
    // exact arity). The engine commits it ⇒ `n : System.String`, agreeing with FCS,
    // and records the member at the call-name range.
    let src = "module M\nlet s = \"hi\"\nlet n = s.Substring(1)\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("n").map(String::as_str),
        Some("System.String"),
        "the arity-refuted loser leaves a unique applicable candidate (OV-6)"
    );
    let (inferred, _) = infer_bcl_full(src);
    assert!(
        matches!(
            inferred.member_resolution_at(ident_range(src, "Substring")),
            Some(Resolution::Member { .. })
        ),
        "a committed overload records its chosen member"
    );
}

#[test]
fn wrong_arity_method_call_defers() {
    // An **ill-arity** call must not be typed as the method return: FCS types it as
    // `obj` (a `call:function` fallback), not the method's return type, so publishing
    // the return would be unsound (mid-edit hazards codex reviews caught). Each of
    // these defers — the whole-call type is not published and the method records no
    // resolution (unlike a bridge failure, an ill-formed call has no FCS-agreed
    // identity). A correct-arity call still types (`method_call_with_tupled_args`).
    for (src, method) in [
        // Too many / too few positional arguments.
        (
            "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant(1)\n",
            "ToLowerInvariant",
        ),
        ("module M\nlet s = \"hi\"\nlet a = s.Insert()\n", "Insert"),
        // A *parenthesized* unit `(())` is one explicit unit argument, not zero — so
        // it is ill-arity for the 0-parameter `ToLowerInvariant` (FCS ⇒ `obj`).
        (
            "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant(())\n",
            "ToLowerInvariant",
        ),
        // A **named argument** is not validated against the parameters here, so any
        // named-argument call defers (FCS types the bad names as `obj`).
        (
            "module M\nlet s = \"hi\"\nlet a = s.Insert(foo = 0, bar = \"z\")\n",
            "Insert",
        ),
    ] {
        let (_, def_types) = infer_bcl(src);
        assert_eq!(
            def_types.get("a"),
            None,
            "an ill-formed method call defers: {src:?}"
        );
        let (inferred, _) = infer_bcl_full(src);
        assert_eq!(
            inferred.member_resolution_at(ident_range(src, method)),
            None,
            "an ill-formed method call records no resolution: {src:?}"
        );
    }
}

/// The source range of the **receiver** `s` in a method call — the `IDENT_TOK "s"`
/// immediately followed by a `DOT_TOK` (so, not the `let s` binder, which is
/// followed by whitespace / `=`).
fn receiver_range(src: &str) -> TextRange {
    let parsed = parse(src);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let toks: Vec<_> = file
        .syntax()
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .collect();
    for (i, t) in toks.iter().enumerate() {
        if t.kind() == SyntaxKind::IDENT_TOK
            && t.text() == "s"
            && toks.get(i + 1).map(|n| n.kind()) == Some(SyntaxKind::DOT_TOK)
        {
            return t.text_range();
        }
    }
    panic!("no receiver `s` (an `s` followed by `.`) in {src:?}");
}

/// The source range of the argument-receiver `t` in `…(t.Length)` — the
/// `IDENT_TOK "t"` immediately followed by a `DOT_TOK`.
fn arg_receiver_range(src: &str, name: &str) -> TextRange {
    let parsed = parse(src);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let toks: Vec<_> = file
        .syntax()
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .collect();
    toks.iter()
        .enumerate()
        .find_map(|(i, t)| {
            (t.kind() == SyntaxKind::IDENT_TOK
                && t.text() == name
                && toks.get(i + 1).map(|n| n.kind()) == Some(SyntaxKind::DOT_TOK))
            .then(|| t.text_range())
        })
        .unwrap_or_else(|| panic!("no `{name}.` in {src:?}"))
}

#[test]
fn method_call_emits_no_receiver_or_argument_node() {
    // Codex rounds 3–6: a method call emits **no node inside itself** — not its
    // receiver, not its argument's sub-expressions. FCS lowers a rejected / ill-formed
    // call to a single node and emits nothing inside it, and a receiver's `result` can
    // even be grounded by a *surrounding* constraint (so gating on the receiver's own
    // groundness is unsound). We discard those emissions; the receiver's type stays
    // available via name resolution.

    // Accepted call: no receiver node, but the whole call still types.
    let accepted = "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant()\n";
    let (inferred, def_types) = infer_bcl(accepted);
    assert_eq!(
        inferred.type_at(receiver_range(accepted)),
        None,
        "no receiver node inside a method call"
    );
    assert_eq!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "the accepted call still types via its whole-call node"
    );

    // Deferred (ill-arity) call: nothing inside.
    let deferred = "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant(1)\n";
    let (inferred, _) = infer_bcl(deferred);
    assert_eq!(
        inferred.type_at(receiver_range(deferred)),
        None,
        "no receiver node for a deferred call"
    );

    // Round 6, finding 1: a rejected call whose result is grounded by a *surrounding*
    // constraint (`f (s.ToLowerInvariant(1))`, `f : bool -> _`) must still emit no
    // receiver node — the external grounding must not resurrect it.
    let ext = "module M\nlet f (x: bool) = x\nlet s = \"hi\"\nlet a = f (s.ToLowerInvariant(1))\n";
    let (inferred, _) = infer_bcl(ext);
    assert_eq!(
        inferred.type_at(receiver_range(ext)),
        None,
        "an externally-grounded rejected call emits no receiver node"
    );

    // Round 6, finding 2: a rejected call must not emit its argument's sub-expression
    // nodes (`t`, the receiver of the argument `t.Length`).
    let arg = "module M\nlet s = \"hi\"\nlet t = \"x\"\nlet a = s.ToLowerInvariant(t.Length)\n";
    let (inferred, _) = infer_bcl(arg);
    assert_eq!(
        inferred.type_at(arg_receiver_range(arg, "t")),
        None,
        "a rejected call emits no argument sub-expression node"
    );
}

#[test]
fn nested_deferred_method_call_emits_nothing() {
    // A nested chain whose **outer** call defers (`s.ToLowerInvariant().Length.ToString(1)`
    // — an ill-arity `ToString`): FCS reports only the failed whole call and no nodes
    // inside it. The inner (valid) `s.ToLowerInvariant()` / `.Length` nodes are typed
    // while resolving the outer receiver but then discarded with everything else, so
    // `a` defers and nothing (no receiver `s`) leaks.
    let src = "module M\nlet s = \"hi\"\nlet a = s.ToLowerInvariant().Length.ToString(1)\n";
    let (inferred, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("a"),
        None,
        "the outer deferred call publishes no type"
    );
    assert_eq!(
        inferred.type_at(receiver_range(src)),
        None,
        "no node leaks inside a deferred nested call"
    );
}

#[test]
fn double_paren_tuple_method_call_defers() {
    // A tupled argument in *extra* parentheses (`s.Insert((0, "z"))`) makes the tuple
    // a single value, which FCS elaborates as a method-value `application` (not a
    // `call:instance`) — a shape whose receiver node FCS also drops. We peel only the
    // one call-parenthesis layer, so this counts as **1** argument, mismatches the
    // 2-parameter `Insert`, and defers (sound — no wrong type, no over-claimed node).
    let src = "module M\nlet s = \"hi\"\nlet r = s.Insert((0, \"z\"))\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("r"),
        None,
        "an extra-parenthesized tuple argument defers"
    );
}

#[test]
fn trailing_comma_argument_list_defers() {
    // Codex round 5: a trailing / doubled comma in a method argument list is a parser
    // *recovery* on a malformed call (`s.Insert(0, "z",)`), which FCS types as `obj`
    // (no method). Counting only the present elements would over-accept the arity and
    // publish `string`; the well-formedness check (elements == commas + 1) defers.
    // This is a **parse-error** input (the LSP sees such mid-edit buffers), so we
    // infer directly rather than through the error-free `infer_bcl` helper.
    let env = bcl_env();
    let src = "module M\nlet s = \"hi\"\nlet a = s.Insert(0, \"z\",)\n";
    let parsed = parse(src);
    assert!(
        !parsed.errors.is_empty(),
        "the trailing comma is a parse error"
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        def_types.get("a"),
        None,
        "a trailing-comma (malformed) argument list defers"
    );
    assert!(
        inferred.member_resolutions().is_empty(),
        "no method resolution is recorded for a malformed call"
    );
}

// NOTE: the 3.3d-era `static_method_call_is_unaffected` pin (a fully-qualified
// static call `System.String.IsNullOrEmpty s` defers) was **flipped by OV-7**:
// static method calls are now typed by the overload engine. The positive
// differential lives in `infer_static_call_diff.rs`
// (`single_candidate_static_call_value_arg_matches_fcs`).

#[test]
fn open_receiver_method_call_defers() {
    // `let f x = x.ToLowerInvariant()`: the receiver `x` is an open parameter, so the
    // method never wakes — the call defers and `f` stays untyped.
    let src = "module M\nlet f x = x.ToLowerInvariant()\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(def_types.get("f"), None, "an open receiver defers");
}

// ===== Behaviour: the poison payoff =====

#[test]
fn unwoken_method_call_blocks_generalisation() {
    // `let f x = (x.ToLowerInvariant(), x)`: the method call on the open parameter
    // `x` never wakes, poisoning `x`. FCS would infer a member constraint on `x`,
    // not a free `'a`, so a bogus `'a -> _ * 'a` scheme must not be published — the
    // poison blocks generalisation and `f` defers.
    let src = "module M\nlet f x = (x.ToLowerInvariant(), x)\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("f"),
        None,
        "an unwoken method call must block generalisation of the receiver"
    );
}

#[test]
fn method_argument_use_blocks_generalisation() {
    // `let s = "hi"` then `let f x = (s.Insert(x, "z"), x)`: the call grounds to
    // `string` regardless of `x`, but `x` flows into the method argument (a
    // subsumption we drop). FCS grounds `x : int` there, so `f` must NOT generalise
    // to `'a -> string * 'a`; the argument poison blocks it and `f` defers.
    let src = "module M\nlet s = \"hi\"\nlet f x = (s.Insert(x, \"z\"), x)\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("f"),
        None,
        "a method-argument parameter use must block generalisation"
    );
}

#[test]
fn method_arg_nested_application_of_param_blocks_generalisation() {
    // The subtle case: a parameter flows through a *nested application* in the method
    // argument (`s.Insert(id y, "z")` with an in-file generic `id`). FCS grounds
    // `y : int` through `Insert`'s parameter, so `f : int -> string` — not
    // `'a -> string`. The nested `id y` is walked in check mode, so its result var
    // (which for `id : 'a -> 'a` is its domain) is poisoned, and the application wake
    // unifies `y` with that poisoned var — poisoning `y` transitively. `f` defers
    // rather than publishing a wrong `'a -> string` (the check-mode poison reaching a
    // param through a nested application, D5).
    let src = "module M\nlet id x = x\nlet s = \"hi\"\nlet f y = s.Insert(id y, \"z\")\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("f"),
        None,
        "a parameter used inside a nested application in a method argument must not generalise"
    );
}

#[test]
fn method_call_unit_arg_does_not_block_generalisation() {
    // The complementary payoff: a **unit**-argument method call does *not* mark the
    // binding incomplete, so an otherwise-free parameter still generalises —
    // `let f x = (s.ToLowerInvariant(), x)` ⇒ `'a -> string * 'a`, matching FCS
    // (`x` is genuinely unconstrained; only the ground `string` method result flows).
    let src = "module M\nlet s = \"hi\"\nlet f x = (s.ToLowerInvariant(), x)\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("f").map(String::as_str),
        Some("'a -> System.String * 'a"),
        "a unit-argument method call must not block generalisation of a free parameter"
    );
}

// ===== Behaviour: void return defers the type but records the identity =====

/// A [`AssemblyEnv`] over the real BCL, with a synthetic single-candidate **`void`**
/// instance method `name` grafted onto `System.String` (cloned from a real public
/// instance method, then flipped to a void, no-parameter, non-generic signature).
/// Lets the void-return path be exercised end-to-end from a `"hi"` receiver, since
/// the immutable BCL `String` ships no single-candidate void method of its own.
fn string_env_with_void_method(name: &str) -> AssemblyEnv {
    use borzoi_assembly::{Access, Member, Primitive, TypeRef};
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let asm = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let mut entities = asm
        .enumerate_type_defs()
        .expect("enumerate System.Runtime types");
    let string_ent = entities
        .iter_mut()
        .find(|e| e.namespace == ["System"] && e.name == "String")
        .expect("System.String entity");
    let mut void_method = string_ent
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(mm)
                if !mm.is_static && !mm.is_constructor && mm.access == Access::Public =>
            {
                Some(mm.clone())
            }
            _ => None,
        })
        .expect("a public instance method on String to clone");
    void_method.name = name.to_string();
    void_method.source_name = None;
    void_method.signature.parameters = vec![];
    void_method.signature.return_type = TypeRef::Primitive(Primitive::Void);
    void_method.generic_parameters = vec![];
    string_ent.members.push(Member::Method(void_method));
    AssemblyEnv::from_entities(entities)
}

#[test]
fn void_method_call_defers_type_but_records_resolution() {
    // A `void`-returning method's call type is `unit` (unmodelled), so the type
    // defers — but the member's identity is still recorded (hover / go-to-def), and
    // crucially no `System.Void` (the bridge's literal mapping) is ever emitted.
    let env = string_env_with_void_method("Poke");
    let src = "module M\nlet n = \"hi\".Poke()\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);

    // The identity is recorded so hover / go-to-def works on the method name…
    assert!(
        matches!(
            inferred.member_resolution_at(ident_range(src, "Poke")),
            Some(Resolution::Member { .. })
        ),
        "a void method's identity is recorded even though its type defers"
    );
    // …but the (unit) call type defers, and no `System.Void` leaks anywhere.
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(def_types.get("n"), None, "a void call type defers (unit)");
    assert!(
        !inferred
            .types()
            .values()
            .any(|t| t.render() == "System.Void"),
        "no System.Void is ever emitted from a void method call"
    );
}

// ===== OV-6 review fixes: out-omission, named-arg poison, nested-call retry =====

/// A BCL env with a synthetic single-candidate instance method
/// `name(x: int, out y: int) : bool` grafted onto `System.String`. FCS folds an
/// **omitted** trailing `out` argument into a tuple return (`bool * int`), so a
/// shortened call must never be typed as the raw `bool`.
fn string_env_with_out_method(name: &str) -> AssemblyEnv {
    use borzoi_assembly::{
        Access, Member, Nullability, ParamDefault, Parameter, Primitive, TypeRef,
    };
    let dll = ensure_system_runtime_dll();
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let asm = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let mut entities = asm
        .enumerate_type_defs()
        .expect("enumerate System.Runtime types");
    let string_ent = entities
        .iter_mut()
        .find(|e| e.namespace == ["System"] && e.name == "String")
        .expect("System.String entity");
    let mut m = string_ent
        .members
        .iter()
        .find_map(|m| match m {
            Member::Method(mm)
                if !mm.is_static && !mm.is_constructor && mm.access == Access::Public =>
            {
                Some(mm.clone())
            }
            _ => None,
        })
        .expect("a public instance method on String to clone");
    let out_param = Parameter {
        name: None,
        ty: TypeRef::Primitive(Primitive::I4),
        is_byref: true,
        is_out: true,
        is_readonly_ref: false,
        default: ParamDefault::None,
        is_param_array: false,
        nullability: Nullability::Oblivious,
    };
    let in_param = Parameter {
        name: None,
        ty: TypeRef::Primitive(Primitive::I4),
        is_byref: false,
        is_out: false,
        is_readonly_ref: false,
        default: ParamDefault::None,
        is_param_array: false,
        nullability: Nullability::Oblivious,
    };
    m.name = name.to_string();
    m.source_name = None;
    m.signature.parameters = vec![in_param, out_param];
    m.signature.return_type = TypeRef::Primitive(Primitive::Bool);
    m.generic_parameters = vec![];
    string_ent.members.push(Member::Method(m));
    AssemblyEnv::from_entities(entities)
}

#[test]
fn omitted_out_parameter_single_candidate_defers() {
    // A single candidate `Probe(int, out int) : bool` called omitting the out arg
    // (`"hi".Probe(1)`): FCS folds the omitted out into a tuple return
    // (`bool * int`), which this stage does not model — so the call must DEFER,
    // never publish the raw `bool`. (§5: a byref/out winner defers regardless.)
    let env = string_env_with_out_method("Probe");
    let src = "module M\nlet n = \"hi\".Probe(1)\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        def_types.get("n"),
        None,
        "an omitted-out single candidate defers (its return is out-folded, unmodelled)"
    );
    assert!(
        !inferred
            .types()
            .values()
            .any(|t| t.render() == "System.Boolean"),
        "no raw Boolean leaks from an omitted-out call"
    );
}

#[test]
fn omitted_out_parameter_fully_supplied_also_defers() {
    // Even fully supplied (`"hi".Probe(1, ...)`), a byref/out winner is a §5
    // deferral — the single-candidate shortcut must decline it. Here the call is
    // still 1 argument short of the out (F# would want `&v`); either way it defers.
    let env = string_env_with_out_method("Probe");
    let src = "module M\nlet s = \"hi\"\nlet n = s.Probe(1)\n";
    let (inferred, _) = {
        let parsed = parse(src);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        let resolved = resolve_file(&file, &ProjectItems::default(), &env);
        let inferred = infer_file(&file, &resolved, &env);
        (inferred, resolved)
    };
    // No resolution recorded — a byref/out winner never commits.
    assert_eq!(
        inferred.member_resolution_at(ident_range(src, "Probe")),
        None,
        "a byref/out single candidate is not committed (§5)"
    );
}

#[test]
fn method_named_argument_use_blocks_generalisation() {
    // OV-6 review (P2): a parameter used only inside a NAMED method argument must
    // still be poisoned (and the call marked incomplete). Here the annotation
    // grounds the *result* to `string`, so the method-result poison is not what
    // saves us — `x` is: `let f x : string = s.Insert(startIndex = x, value = "z")`.
    // The named-arg call defers (we do not validate names), but FCS grounds
    // `x : int` through `Insert`'s parameter, so `f` must NOT generalise to
    // `'a -> string`. The argument walk poisons `x` and marks the call incomplete.
    let src =
        "module M\nlet s = \"hi\"\nlet f x : string = s.Insert(startIndex = x, value = \"z\")\n";
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("f"),
        None,
        "a parameter used inside a named method argument must not generalise"
    );
}

#[test]
fn overload_argument_grounded_by_argcheck_commits() {
    // OV-6 review (P2 retry): the overload argument grounds via an `ArgCheck` wake,
    // which fires only after the outer `StartsWith` is first scanned:
    // `let id x = x` then `s.StartsWith(id "h")`. On the first scan the argument
    // `id "h"` is still `Ty::Var`, so the overload must **retry** (not drop) and
    // commit once the ArgCheck grounds it to `string` ⇒ `b : System.Boolean`,
    // agreeing with FCS.
    let src = "module M\nlet id x = x\nlet s = \"hi\"\nlet b = s.StartsWith(id \"h\")\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("b").map(String::as_str),
        Some("System.Boolean"),
        "an overload argument grounded by a later ArgCheck wake commits (OV-6 retry)"
    );
}

#[test]
fn nested_member_call_argument_grounds_and_commits() {
    // OV-6 review (P3): the overload argument is itself a suspended member call,
    // `s.StartsWith(s.Substring(1))`. The inner `s.Substring(1)` grounds to
    // `System.String` on its own wake; the outer `StartsWith` overload must retry
    // once its argument is ground and then commit `StartsWith(string)` ⇒
    // `n : System.Boolean`, agreeing with FCS.
    let src = "module M\nlet s = \"hi\"\nlet n = s.StartsWith(s.Substring(1))\n";
    assert_sound(src);
    let (_, def_types) = infer_bcl(src);
    assert_eq!(
        def_types.get("n").map(String::as_str),
        Some("System.Boolean"),
        "a nested member-call argument grounds and the outer overload commits (OV-6)"
    );
}

// ===== Stage R2-e — the annotated binding's RHS check-walk (coverage) =====

#[test]
fn annotated_binding_rhs_member_wakes_and_records() {
    // `let n : int = s.Length`: pre-R2-e the annotated binding skipped its RHS
    // entirely, losing the `Length` hover. The check-walk restores it — the
    // member wakes and records its identity — while the binder still types
    // from the annotation and the RHS root emits no node (check mode).
    // Everything we do emit (the receiver `s`) agrees with FCS.
    let src = "module M\nlet s = \"hi\"\nlet n : int = s.Length\n";
    let (inferred, _) = infer_core_full(src);
    assert!(
        matches!(
            inferred.member_resolution_at(ident_range(src, "Length")),
            Some(Resolution::Member { .. })
        ),
        "the annotated binding's member access wakes and records"
    );
    let def_types = assert_sound_core(src);
    assert_eq!(def_types.get("n").map(String::as_str), Some("System.Int32"));
    assert_eq!(
        def_types.get("s").map(String::as_str),
        Some("System.String")
    );
}

#[test]
fn annotated_binding_rhs_root_stays_silent() {
    // The RHS root node is never emitted under an annotation — the coercion
    // may re-type it (the 3.2b-1 lesson): `let o : obj = s` has the use `s`
    // elaborated as `obj` in FCS, so emitting the binder's `string` there
    // would be wrong. The check-walk suppresses it; the binder `o` types from
    // the annotation.
    let src = "module M\nlet s = \"hi\"\nlet o : obj = s\n";
    let (inferred, resolved) = infer_core_full(src);
    let use_at = src.rfind("= s").expect("use") + 2;
    assert!(
        inferred
            .types()
            .keys()
            .all(|r| usize::from(r.start()) != use_at),
        "the coerced RHS root must not be emitted"
    );
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        def_types.get("o").map(String::as_str),
        Some("System.Object")
    );
    assert_sound_core(src);
}

#[test]
fn ill_typed_annotated_rhs_still_sound() {
    // `let n : int64 = s.Length` is ill-typed (`Length : int`); FCS keeps
    // `n : int64` (the annotation wins on the binder). The member identity is
    // still recorded (it *is* `String.Length` regardless of the binding
    // error), and nothing wrong is emitted at any range.
    let src = "module M\nlet s = \"hi\"\nlet n : int64 = s.Length\n";
    let (inferred, resolved) = infer_core_full(src);
    assert!(
        matches!(
            inferred.member_resolution_at(ident_range(src, "Length")),
            Some(Resolution::Member { .. })
        ),
        "the member identity is recorded even on an ill-typed binding"
    );
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(def_types.get("n").map(String::as_str), Some("System.Int64"));
    assert_sound_core(src);
}

#[test]
fn typed_pattern_rhs_member_wakes_too() {
    // The trivial typed-pattern form rides the same walk:
    // `let (n : int) = s.Length` records the member and types the binder.
    let src = "module M\nlet s = \"hi\"\nlet (n : int) = s.Length\n";
    let (inferred, _) = infer_core_full(src);
    assert!(
        matches!(
            inferred.member_resolution_at(ident_range(src, "Length")),
            Some(Resolution::Member { .. })
        ),
        "the typed-pattern binding's member access wakes and records"
    );
    let def_types = assert_sound_core(src);
    assert_eq!(def_types.get("n").map(String::as_str), Some("System.Int32"));
}

// ===== EX-3 §2(d) stage 5: the gate consumes the attribute resolutions =====

/// The stage's coverage win, end-to-end through the gate with the D5 net: a
/// file whose only attribute is a provably-non-extension one (`[<Literal>]`,
/// resolved to FSharp.Core's `LiteralAttribute` by the stage-3 query) now
/// **commits** an overloaded call the presence-based trigger deferred, and
/// FCS agrees at every typed range.
#[test]
fn non_extension_attribute_no_longer_defers_the_overload() {
    let src = "module M\n[<Literal>]\nlet Lit = 1\nlet s = \"hi\"\nlet a = s.Substring(1)\n";
    let env = bcl_and_fsharp_core_env();
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "a non-extension attribute must not defer the overloaded `Substring(int)` call"
    );

    let path = temp_fs_file("ex3_attr_commit", src);
    let json = invoke_fcs_dump("types", &path);
    let _ = std::fs::remove_file(&path);
    let fcs = parse_fcs_types(&json, src);
    for (range, ty) in inferred.types() {
        let key = (usize::from(range.start()), usize::from(range.end()));
        let fcs_ty = fcs
            .get(&key)
            .unwrap_or_else(|| panic!("we typed {key:?} but FCS has no node there in {src:?}"));
        assert_eq!(&ty.render(), fcs_ty, "type mismatch at {key:?} in {src:?}");
    }
}

/// A real C#-style `[<Extension>]` (the `open` brings SRCS into scope, so the
/// suffixed candidate resolves to the genuine `ExtensionAttribute`) still
/// defers the overloaded call — resolving *to* the marker keeps the presence
/// defer; name-keying its members is (a)–(c) work.
#[test]
fn a_real_extension_attribute_still_defers_the_overload() {
    let src = "module M\nopen System.Runtime.CompilerServices\n[<Extension>]\ntype Helpers =\n    static member Twice (s: string) = s + s\nlet s = \"hi\"\nlet a = s.Substring(1)\n";
    let env = bcl_and_fsharp_core_env();
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_ne!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "a file declaring a genuine [<Extension>] must keep deferring the call"
    );
}

// ===== EX-3 §2(a): the same-file augmentation trigger goes name-keyed =====

/// A same-file augmentation defers only calls of the member names it
/// declares: `type System.String with member this.Twice…` no longer defers
/// `s.Substring(1)` (FCS-diffed commit), while `s.Twice(…)` — the name the
/// augmentation actually contributes — still defers.
#[test]
fn augmentation_defers_only_its_own_member_names() {
    let src = "module M\ntype System.String with\n    member this.Twice (x: int) = x + x\nlet s = \"hi\"\nlet a = s.Substring(1)\n";
    let env = bcl_and_fsharp_core_env();
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_eq!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "an augmentation of `Twice` must not defer the overloaded `Substring(int)` call"
    );

    let path = temp_fs_file("ex3a_aug_commit", src);
    let json = invoke_fcs_dump("types", &path);
    let _ = std::fs::remove_file(&path);
    let fcs = parse_fcs_types(&json, src);
    for (range, ty) in inferred.types() {
        let key = (usize::from(range.start()), usize::from(range.end()));
        let fcs_ty = fcs
            .get(&key)
            .unwrap_or_else(|| panic!("we typed {key:?} but FCS has no node there in {src:?}"));
        assert_eq!(&ty.render(), fcs_ty, "type mismatch at {key:?} in {src:?}");
    }

    // The declared name itself still defers: `Twice` joins the instance group.
    let src = "module M\ntype System.String with\n    member this.Substring (x: bool) = 7.0\nlet s = \"hi\"\nlet a = s.Substring(1)\n";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty());
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_ne!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "an augmentation OF `Substring` must keep deferring `s.Substring(1)`"
    );
}

/// An augmentation member whose name the walker cannot extract (an operator)
/// keeps the wholesale defer — the unknowable bit, not a silent skip.
#[test]
fn augmentation_with_unnameable_member_defers_everything() {
    let src = "module M\ntype System.String with\n    static member (+.) (a: string, b: int) = a\nlet s = \"hi\"\nlet a = s.Substring(1)\n";
    let env = bcl_and_fsharp_core_env();
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_ne!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "an un-nameable augmentation member must defer every call"
    );
}

// ===== EX-3 §2(b): the cross-file augmentation trigger goes name-keyed =====

/// A preceding Compile-order file's augmentation defers only calls of the
/// member names it declares: file 1 augmenting `Twice` no longer defers file
/// 2's `s.Substring(1)`; file 1 augmenting `Substring` still does; an
/// un-nameable member (an operator) in file 1 defers file 2 wholesale.
#[test]
fn preceding_augmentation_defers_only_its_member_names() {
    use borzoi_sema::resolve_project;
    let env = bcl_and_fsharp_core_env();
    let run = |f1: &str, f2: &str| -> Option<String> {
        let files: Vec<ImplFile> = [f1, f2]
            .iter()
            .map(|s| {
                let p = parse(s);
                assert!(p.errors.is_empty(), "parse errors in {s:?}: {:?}", p.errors);
                ImplFile::cast(p.root).expect("impl file")
            })
            .collect();
        let project = resolve_project(&files, &env);
        let rf2 = &project.files()[1];
        let inferred = infer_file(&files[1], rf2, &env);
        inferred
            .def_types()
            .iter()
            .find(|(id, _)| rf2.def(*(*id)).name == "n")
            .map(|(_, ty)| ty.render())
    };

    let unrelated = "module A\ntype System.String with\n    member this.Twice (x: int) = x + x\n";
    let caller = "module B\nlet s = \"hi\"\nlet n = s.Substring(1)\n";
    assert_eq!(
        run(unrelated, caller).as_deref(),
        Some("System.String"),
        "a preceding augmentation of `Twice` must not defer `Substring`"
    );

    let colliding =
        "module A\ntype System.String with\n    member this.Substring (x: bool) = 7.0\n";
    assert_eq!(
        run(colliding, caller),
        None,
        "a preceding augmentation OF `Substring` must defer it"
    );

    let unnameable =
        "module A\ntype System.String with\n    static member (+.) (a: string, b: int) = a\n";
    assert_eq!(
        run(unnameable, caller),
        None,
        "a preceding un-nameable augmentation member must defer wholesale"
    );
}

// ===== Stage IW: interface-receiver member resolution =====
//
// FCS's intrinsic member walk gives a receiver whose *static type is an
// interface* `System.Object`'s members **plus** all transitively inherited
// interfaces (`overload-resolution-plan.md` §2.1); a class/struct receiver does
// **not** walk its interfaces (`followInterfaces=false`), so 3.x-inh's base-class
// walk is already complete there. These cases exercise the missing interface
// surface — an interface parameter's own member, an inherited-interface member,
// and an `Object` member reached through the interface — all currently DEFER
// (`method_group` bails on an interface kind; an interface's `base_chain` is just
// itself). The FCS ground-truth types are pinned in `docs/interface-walk-plan.md`.
#[test]
fn interface_receiver_members_match_fcs() {
    // (source, the member-access expression's source text, FCS `TypeCanon`)
    let cases: &[(&str, &str, &str)] = &[
        // An `Object` method reached through an interface receiver (probe P6).
        (
            "module M\nlet f (e : System.Collections.IEnumerable) = e.GetHashCode()\n",
            "e.GetHashCode()",
            "System.Int32",
        ),
        // An interface's own method.
        (
            "module M\nlet f (e : System.Collections.IEnumerable) = e.GetEnumerator()\n",
            "e.GetEnumerator()",
            "System.Collections.IEnumerator",
        ),
        // An inherited-interface data member (`IList` → `ICollection.Count`).
        (
            "module M\nlet f (l : System.Collections.IList) = l.Count\n",
            "l.Count",
            "System.Int32",
        ),
        // An interface's own readable property (this one already resolves via the
        // own-level check — it pins the pre-existing behaviour under the new walk).
        (
            "module M\nlet f (l : System.Collections.IList) = l.IsReadOnly\n",
            "l.IsReadOnly",
            "System.Boolean",
        ),
        // A two-level inherited-interface method (`IList` → … → `IEnumerable.GetEnumerator`).
        (
            "module M\nlet f (l : System.Collections.IList) = l.GetEnumerator()\n",
            "l.GetEnumerator()",
            "System.Collections.IEnumerator",
        ),
        // An `Object` method through a different interface receiver.
        (
            "module M\nlet f (d : System.IDisposable) = d.GetHashCode()\n",
            "d.GetHashCode()",
            "System.Int32",
        ),
    ];
    for &(src, access, expected) in cases {
        // D5 soundness: whatever we publish, FCS agrees at that exact range.
        assert_sound(src);
        // Positive: we resolved the interface member access to `expected`. Keyed by
        // the expression's source text so the check is independent of the exact
        // range convention.
        let (inferred, _) = infer_bcl(src);
        let published: Vec<(String, String)> = inferred
            .types()
            .iter()
            .map(|(r, t)| {
                (
                    src[usize::from(r.start())..usize::from(r.end())].to_string(),
                    t.render(),
                )
            })
            .collect();
        assert!(
            published
                .iter()
                .any(|(txt, ty)| txt == access && ty == expected),
            "interface member `{access}` should type as `{expected}`; \
             published types were {published:?} in {src:?}"
        );
    }
}

// ===== EX-3 AO-1: the auto-open presence triggers are subsumed =====

/// Infer `src` against BCL + FSharp.Core and, for every expression type we
/// produced, assert the FCS `types` oracle agrees at that exact range — the
/// [`assert_sound`] shape, but in the env where `[<AutoOpen>]` itself resolves
/// (the §2(d) machinery needs FSharp.Core's `AutoOpenAttribute` to prove the
/// attribute is not an `ExtensionAttribute` alias).
fn assert_sound_core(src: &str) -> HashMap<String, String> {
    let env = bcl_and_fsharp_core_env();
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);

    let path = temp_fs_file("ao1_commit", src);
    let json = invoke_fcs_dump("types", &path);
    let _ = std::fs::remove_file(&path);
    let fcs = parse_fcs_types(&json, src);
    for (range, ty) in inferred.types() {
        let key = (usize::from(range.start()), usize::from(range.end()));
        let fcs_ty = fcs
            .get(&key)
            .unwrap_or_else(|| panic!("we typed {key:?} but FCS has no node there in {src:?}"));
        assert_eq!(&ty.render(), fcs_ty, "type mismatch at {key:?} in {src:?}");
    }
    inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect()
}

/// AO-1: a same-file `[<AutoOpen>]` module of plain `let`s is no extension
/// source — its only extension-capable content kinds (a `type … with`
/// augmentation, a `[<Extension>]` attribute) are covered file-globally by the
/// §2(a) name sets and the §2(d) attribute verdicts, so the presence trigger
/// was pure over-approximation and the overloaded call commits (FCS-diffed).
/// A `private` auto-open module behaves identically for the own-file case.
#[test]
fn same_file_plain_auto_open_no_longer_defers_the_overload() {
    let src = "module M\n[<AutoOpen>]\nmodule Helpers =\n    let helper (x: int) = x + 1\nlet s = \"hi\"\nlet a = s.Substring(1)\n";
    let def_types = assert_sound_core(src);
    assert_eq!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "an auto-open module of plain lets must not defer the overloaded `Substring(int)` call"
    );

    let private_src = "module M\n[<AutoOpen>]\nmodule private Helpers =\n    let helper (x: int) = x + 1\nlet s = \"hi\"\nlet a = s.Substring(1)\n";
    let def_types = assert_sound_core(private_src);
    assert_eq!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "a PRIVATE auto-open module of plain lets must not defer either"
    );
}

/// AO-1 content kind 1: an augmentation *inside* an auto-open module is
/// collected by the §2(a) walk (which runs inside nested modules), so the file
/// defers exactly the augmented names — `Twice` does not defer `Substring`
/// (FCS-diffed commit), `Substring` does.
#[test]
fn auto_open_module_augmentation_defers_only_its_member_names() {
    let src = "module M\n[<AutoOpen>]\nmodule Helpers =\n    type System.String with\n        member this.Twice (x: int) = x + x\nlet s = \"hi\"\nlet a = s.Substring(1)\n";
    let def_types = assert_sound_core(src);
    assert_eq!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "an auto-open module augmenting `Twice` must not defer `Substring(int)`"
    );

    let colliding = "module M\n[<AutoOpen>]\nmodule Helpers =\n    type System.String with\n        member this.Substring (x: bool) = 7.0\nlet s = \"hi\"\nlet a = s.Substring(1)\n";
    let env = bcl_and_fsharp_core_env();
    let parsed = parse(colliding);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_ne!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "an auto-open module augmenting `Substring` must keep deferring `s.Substring(1)`"
    );
}

/// AO-1 content kind 2: a `[<Extension>]` type *inside* an auto-open module is
/// seen by the §2(d) attribute walk (nested-module bodies resolve inside the
/// recursion), resolves to the genuine marker, and keeps the wholesale defer —
/// own-file and threaded to later Compile-order files alike.
#[test]
fn auto_open_module_with_extension_attribute_still_defers_wholesale() {
    let src = "module M\nopen System.Runtime.CompilerServices\n[<AutoOpen>]\nmodule Helpers =\n    [<Extension>]\n    type H =\n        static member Twice (s: string) = s + s\nlet s = \"hi\"\nlet a = s.Substring(1)\n";
    let env = bcl_and_fsharp_core_env();
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);
    let def_types: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();
    assert_ne!(
        def_types.get("a").map(String::as_str),
        Some("System.String"),
        "a [<Extension>] type inside an auto-open module must defer wholesale"
    );

    // Cross-file: the marker resolution threads through the wholesale bit.
    use borzoi_sema::resolve_project;
    let f1 = "module M1\nopen System.Runtime.CompilerServices\n[<AutoOpen>]\nmodule Helpers =\n    [<Extension>]\n    type H =\n        static member Twice (s: string) = s + s\n";
    let f2 = "module M2\nlet s = \"hi\"\nlet n = s.Substring(1)\n";
    let files: Vec<ImplFile> = [f1, f2]
        .iter()
        .map(|s| {
            let p = parse(s);
            assert!(p.errors.is_empty(), "parse errors in {s:?}: {:?}", p.errors);
            ImplFile::cast(p.root).expect("impl file")
        })
        .collect();
    let project = resolve_project(&files, &env);
    let rf2 = &project.files()[1];
    let inferred = infer_file(&files[1], rf2, &env);
    let n_ty = inferred
        .def_types()
        .iter()
        .find(|(id, _)| rf2.def(*(*id)).name == "n")
        .map(|(_, ty)| ty.render());
    assert_eq!(
        n_ty, None,
        "a preceding file's [<Extension>]-in-auto-open must defer later files wholesale"
    );
}

/// AO-1 cross-file: a preceding file's auto-open module defers exactly what
/// its *contents* contribute — plain lets nothing, an augmentation its member
/// names — instead of the old path-presence wholesale defer.
#[test]
fn preceding_plain_auto_open_no_longer_defers_the_overload() {
    use borzoi_sema::resolve_project;
    let env = bcl_and_fsharp_core_env();
    let run = |f1: &str, f2: &str| -> Option<String> {
        let files: Vec<ImplFile> = [f1, f2]
            .iter()
            .map(|s| {
                let p = parse(s);
                assert!(p.errors.is_empty(), "parse errors in {s:?}: {:?}", p.errors);
                ImplFile::cast(p.root).expect("impl file")
            })
            .collect();
        let project = resolve_project(&files, &env);
        let rf2 = &project.files()[1];
        let inferred = infer_file(&files[1], rf2, &env);
        inferred
            .def_types()
            .iter()
            .find(|(id, _)| rf2.def(*(*id)).name == "n")
            .map(|(_, ty)| ty.render())
    };
    let caller = "module B\nlet s = \"hi\"\nlet n = s.Substring(1)\n";

    let plain = "module A\n[<AutoOpen>]\nmodule Helpers =\n    let helper (x: int) = x + 1\n";
    assert_eq!(
        run(plain, caller).as_deref(),
        Some("System.String"),
        "a preceding auto-open module of plain lets must not defer `Substring`"
    );

    let unrelated = "module A\n[<AutoOpen>]\nmodule Helpers =\n    type System.String with\n        member this.Twice (x: int) = x + x\n";
    assert_eq!(
        run(unrelated, caller).as_deref(),
        Some("System.String"),
        "a preceding auto-open augmentation of `Twice` must not defer `Substring`"
    );

    let colliding = "module A\n[<AutoOpen>]\nmodule Helpers =\n    type System.String with\n        member this.Substring (x: bool) = 7.0\n";
    assert_eq!(
        run(colliding, caller),
        None,
        "a preceding auto-open augmentation OF `Substring` must defer it"
    );

    // Compile-order: the auto-open only affects LATER files. The colliding
    // augmentation compiled AFTER the call file does not defer it.
    let files: Vec<ImplFile> = [caller, colliding]
        .iter()
        .map(|s| {
            let p = parse(s);
            assert!(p.errors.is_empty());
            ImplFile::cast(p.root).expect("impl file")
        })
        .collect();
    let project = resolve_project(&files, &env);
    let rf1 = &project.files()[0];
    let inferred = infer_file(&files[0], rf1, &env);
    let n_ty = inferred
        .def_types()
        .iter()
        .find(|(id, _)| rf1.def(*(*id)).name == "n")
        .map(|(_, ty)| ty.render());
    assert_eq!(
        n_ty.as_deref(),
        Some("System.String"),
        "a file compiled before the colliding auto-open augmentation still commits"
    );
}

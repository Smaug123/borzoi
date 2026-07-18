//! FCS-free tests for name resolution **inside type-member bodies** — the slice
//! that binds a member's self-identifier, parameters, the type's primary-
//! constructor parameters, and its class-level `let`/`do` fields, then resolves
//! the body against that scope.
//!
//! Before this slice the resolver indexed member *names* (so `Type.Member`
//! resolves from outside) but never descended into member bodies, so every
//! local, parameter, and self-identifier use inside a `member` / `new` / property
//! accessor was deferred. That single gap was ~91% of the whole in-file
//! no-inference (B1) worklist (see `docs/member-body-resolution-plan.md`).
//!
//! Each expected target range here was read off FCS (`fcs-dump uses`); the
//! differential counterpart lives in `resolve_diff.rs`'s `CORPUS` and the corpus
//! sweep (`resolve_corpus_diff.rs`), which gates divergences to zero.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, ResolvedFile, resolve_file};
use rowan::{TextRange, TextSize};

fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
}

/// The byte range of the `ident` **token** located inside the (unique)
/// `context` substring of `src`. Matches only at identifier-token boundaries so
/// a one-letter identifier is not found inside a longer word (`n` inside `int`).
fn ident_at(src: &str, context: &str, ident: &str) -> TextRange {
    let ci = src
        .find(context)
        .unwrap_or_else(|| panic!("context {context:?} not found"));
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let ctx = &src[ci..ci + context.len()];
    let bytes = src.as_bytes();
    let mut search = 0;
    loop {
        let rel = ctx[search..]
            .find(ident)
            .unwrap_or_else(|| panic!("token {ident:?} not in context {context:?}"));
        let start = ci + search + rel;
        let end = start + ident.len();
        let before_ok = start == 0 || !is_word(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_word(bytes[end]);
        if before_ok && after_ok {
            return TextRange::new(
                TextSize::from(u32::try_from(start).unwrap()),
                TextSize::from(u32::try_from(end).unwrap()),
            );
        }
        search += rel + ident.len();
    }
}

#[track_caller]
fn assert_binds(rf: &ResolvedFile, use_r: TextRange, def_r: TextRange, label: &str) {
    let res = rf
        .resolution_at(use_r)
        .unwrap_or_else(|| panic!("{label}: nothing resolved at {use_r:?}"));
    assert!(
        matches!(res, Resolution::Local(_) | Resolution::Item(_)),
        "{label}: expected a local/item binder at {use_r:?}, got {res:?}"
    );
    let def = rf
        .resolved_def(res)
        .unwrap_or_else(|| panic!("{label}: resolution names no in-file def"));
    assert_eq!(def.range, def_r, "{label}: bound the wrong binder");
}

#[track_caller]
fn assert_defers(rf: &ResolvedFile, use_r: TextRange, label: &str) {
    match rf.resolution_at(use_r) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("{label}: expected a deferral at {use_r:?}, got {other:?}"),
    }
}

#[test]
fn member_body_binds_param_self_field_and_ctor_param() {
    let src = "\
module M
type T(seed: int) =
    let acc = seed
    member x.Add(n: int) : int = acc + n + seed
";
    let rf = resolve(src);
    let seed_def = ident_at(src, "(seed: int)", "seed");
    let acc_def = ident_at(src, "let acc", "acc");
    let n_def = ident_at(src, "(n: int)", "n");

    // `seed` used in the field RHS `let acc = seed`
    assert_binds(
        &rf,
        ident_at(src, "= seed", "seed"),
        seed_def,
        "field-RHS ctor-param",
    );
    // body: `acc + n + seed`
    assert_binds(
        &rf,
        ident_at(src, "= acc + n + seed", "acc"),
        acc_def,
        "body class-field",
    );
    assert_binds(&rf, ident_at(src, "+ n +", "n"), n_def, "body param");
    assert_binds(
        &rf,
        ident_at(src, "+ seed", "seed"),
        seed_def,
        "body ctor-param",
    );
}

#[test]
fn member_param_shadows_module_value() {
    // Soundness (D5): the body use of `n` must bind the *parameter*, never the
    // module value of the same name.
    let src = "\
module M
let n = 99
type T() =
    member x.M(n: int) : int = n
";
    let rf = resolve(src);
    let param_def = ident_at(src, "(n: int)", "n");
    assert_binds(
        &rf,
        ident_at(src, "int = n", "n"),
        param_def,
        "param shadows module value",
    );
}

#[test]
fn bare_self_identifier_use_resolves_to_member_self() {
    let src = "\
module M
type T() =
    member this.Self() : T = this
";
    let rf = resolve(src);
    let self_def = ident_at(src, "this.Self", "this");
    assert_binds(
        &rf,
        ident_at(src, "= this", "this"),
        self_def,
        "bare self use",
    );
}

#[test]
fn member_body_sees_enclosing_module_value() {
    let src = "\
module M
let outer = 1
type T() =
    member this.UsesOuter() : int = outer
";
    let rf = resolve(src);
    let outer_def = ident_at(src, "let outer", "outer");
    assert_binds(
        &rf,
        ident_at(src, "= outer", "outer"),
        outer_def,
        "module value from member",
    );
}

#[test]
fn static_member_body_resolves_param_no_self() {
    let src = "\
module M
type T() =
    static member Twice(v: int) : int = v
";
    let rf = resolve(src);
    let v_def = ident_at(src, "(v: int)", "v");
    assert_binds(
        &rf,
        ident_at(src, "int = v", "v"),
        v_def,
        "static member param",
    );
}

#[test]
fn secondary_constructor_body_resolves_params() {
    let src = "\
module M
type T(seed: int) =
    let acc = seed
    new(a: int, b: int) = T(a + b)
";
    let rf = resolve(src);
    let a_def = ident_at(src, "(a: int", "a");
    let b_def = ident_at(src, "b: int)", "b");
    assert_binds(
        &rf,
        ident_at(src, "T(a + b)", "a"),
        a_def,
        "secondary-ctor param a",
    );
    assert_binds(
        &rf,
        ident_at(src, "a + b)", "b"),
        b_def,
        "secondary-ctor param b",
    );
}

#[test]
fn property_getter_and_setter_bodies_resolve() {
    let src = "\
module M
type T() =
    let mutable acc = 0
    member x.Prop with get () : int = acc and set (v: int) = acc <- v
";
    let rf = resolve(src);
    let acc_def = ident_at(src, "mutable acc", "acc");
    let v_def = ident_at(src, "(v: int)", "v");
    // getter body `= acc`
    assert_binds(
        &rf,
        ident_at(src, "int = acc", "acc"),
        acc_def,
        "getter class-field",
    );
    // setter body `acc <- v`
    assert_binds(
        &rf,
        ident_at(src, "= acc <- v", "acc"),
        acc_def,
        "setter class-field",
    );
    assert_binds(&rf, ident_at(src, "<- v", "v"), v_def, "setter param");
}

#[test]
fn class_let_field_visible_across_later_members() {
    let src = "\
module M
type T() =
    let secret = 41
    member x.A() : int = secret
    member y.B() : int = secret
";
    let rf = resolve(src);
    let secret_def = ident_at(src, "let secret", "secret");
    assert_binds(
        &rf,
        ident_at(src, "A() : int = secret", "secret"),
        secret_def,
        "field in member A",
    );
    assert_binds(
        &rf,
        ident_at(src, "B() : int = secret", "secret"),
        secret_def,
        "field in member B",
    );
}

#[test]
fn augmentation_member_body_does_not_wrongly_bind_module_value() {
    // Soundness: an augmentation body is not walked with the original type's
    // private-field scope (we don't have it), so its field references must
    // *defer*, never bind a same-named module value. Here `acc` in the
    // augmentation would be a wrong bind to the module `acc` if we walked it
    // blind — FCS resolves it to the type's private field. We defer instead.
    let src = "\
module M
let acc = 1
type T() =
    let acc = 2
    member x.A() : int = acc
type T with
    member x.B() : int = acc
";
    let rf = resolve(src);
    // The genuine member A binds the class field (`let acc = 2`).
    let field_def = ident_at(src, "let acc = 2", "acc");
    assert_binds(
        &rf,
        ident_at(src, "A() : int = acc", "acc"),
        field_def,
        "genuine member field",
    );
    // The augmentation member B must NOT bind the module `acc`.
    assert_defers(
        &rf,
        ident_at(src, "B() : int = acc", "acc"),
        "augmentation body defers",
    );
}

// --- static contexts do not see the instance scope (D5 soundness) ---
//
// FCS resolves references from a `static member` / `static let` / static
// auto-property initialiser / secondary constructor against the *static* scope:
// the primary-ctor parameters, `as self`, and non-static class-`let` fields are
// **not** in scope there, so a same-named outer binding wins. Verified with
// `fcs-dump uses` (`x` → `M.x`, not the ctor param).

#[test]
fn static_member_body_binds_module_value_not_ctor_param() {
    let src = "\
module M
let x = 0
type T(x: int) =
    static member S : int = x
";
    let rf = resolve(src);
    let module_x = ident_at(src, "let x = 0", "x");
    assert_binds(
        &rf,
        ident_at(src, "S : int = x", "x"),
        module_x,
        "static member binds module value, not ctor param",
    );
}

#[test]
fn secondary_ctor_body_binds_module_value_not_ctor_param() {
    // A secondary `new(...)` initialiser runs before the object exists, so the
    // primary-ctor parameter `x` is not in scope; `x` binds the module value.
    let src = "\
module M
let x = 0
type T(x: int) =
    new(z: int) = T(x + z)
";
    let rf = resolve(src);
    let module_x = ident_at(src, "let x = 0", "x");
    assert_binds(
        &rf,
        ident_at(src, "T(x + z)", "x"),
        module_x,
        "secondary ctor binds module value, not ctor param",
    );
    // its own parameter `z` still resolves.
    let z_def = ident_at(src, "(z: int)", "z");
    assert_binds(
        &rf,
        ident_at(src, "+ z)", "z"),
        z_def,
        "secondary ctor own param",
    );
}

#[test]
fn static_auto_property_init_binds_module_value_not_ctor_param() {
    let src = "\
module M
let x = 0
type T(x: int) =
    static member val SP : int = x with get, set
";
    let rf = resolve(src);
    let module_x = ident_at(src, "let x = 0", "x");
    assert_binds(
        &rf,
        ident_at(src, "= x with", "x"),
        module_x,
        "static auto-property init binds module value, not ctor param",
    );
}

#[test]
fn static_member_sees_static_let_but_not_instance_let() {
    let src = "\
module M
let inst = 1
type T() =
    static let s = 7
    let inst = 2
    static member S : int = s + inst
";
    let rf = resolve(src);
    let static_let = ident_at(src, "static let s", "s");
    let module_inst = ident_at(src, "let inst = 1", "inst");
    // static member sees the static let `s`
    assert_binds(
        &rf,
        ident_at(src, "= s + inst", "s"),
        static_let,
        "static member sees static let",
    );
    // but NOT the instance `let inst = 2` — it binds the module `inst`
    assert_binds(
        &rf,
        ident_at(src, "s + inst", "inst"),
        module_inst,
        "static member skips instance let, binds module",
    );
}

#[test]
fn instance_member_sees_both_static_and_instance_let() {
    let src = "\
module M
type T() =
    static let s = 7
    let inst = 2
    member this.M : int = s + inst
";
    let rf = resolve(src);
    let static_let = ident_at(src, "static let s", "s");
    let instance_let = ident_at(src, "let inst = 2", "inst");
    assert_binds(
        &rf,
        ident_at(src, "= s + inst", "s"),
        static_let,
        "instance member sees static let",
    );
    assert_binds(
        &rf,
        ident_at(src, "s + inst", "inst"),
        instance_let,
        "instance member sees instance let",
    );
}

#[test]
fn instance_member_binds_later_static_field_shadowing_ctor_param() {
    // Source order across the static/instance split: a `static let x` declared
    // *after* the primary-ctor param `x` shadows it in an instance body (F#'s
    // latest-wins), so the body use binds the static field, not the ctor param
    // (FCS `fcs-dump uses`: decl at the `static let x`).
    let src = "\
module M
type T(x: int) =
    static let x = 2
    member _.M : int = x
";
    let rf = resolve(src);
    let static_x = ident_at(src, "static let x", "x");
    assert_binds(
        &rf,
        ident_at(src, "int = x", "x"),
        static_x,
        "later static field shadows ctor param in instance body",
    );
}

#[test]
fn member_param_pattern_uses_class_local_active_pattern() {
    // A member parameter pattern that names a class-local active pattern must
    // resolve it against the class fields (pushed before the parameter patterns),
    // not a same-named *module* recognizer. FCS binds `Hit` here to the class's
    // `(|Hit|)`. Soundness (D5): we must not commit the module one.
    let src = "\
module M
let (|Hit|) x = 99
type T() =
    let (|Hit|) x = x
    member _.M(Hit y) : int = y
";
    let rf = resolve(src);
    let module_hit = ident_at(src, "let (|Hit|) x = 99", "Hit");
    let use_r = ident_at(src, "M(Hit y)", "Hit");
    // Whatever we bind, it must NOT be the module recognizer.
    if let Some(def) = rf.resolution_at(use_r).and_then(|res| rf.resolved_def(res)) {
        assert_ne!(
            def.range, module_hit,
            "member param bound the module recognizer, not the class-local one"
        );
    }
}

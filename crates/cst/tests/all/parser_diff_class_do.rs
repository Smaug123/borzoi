//! Differential test (`parser::parse` vs FCS): class-body `do` bindings —
//! FCS's `SynMemberDefn.LetBindings([SynBinding(kind = Do, …)], isStatic, …)`
//! (the `do`-binding `classDefnBindings` arm, `pars.fsy`). A `do <expr>` in a
//! type body runs at construction time; FCS models it as a single-binding
//! `LetBindings` whose binding has `SynBindingKind.Do`, a synthetic
//! `SynPat.Const(Unit)` head pattern, the body in `SynBinding.expr`, and a
//! `Do` / `StaticDo` `SynLeadingKeyword`. A `do`-only body is what flags a
//! primary-constructor class as a class (so the ctor is no longer "Only class
//! types may take value arguments").

use crate::common::assert_asts_match;

/// The motivating case: a generic, accessibility-modified primary-constructor
/// class whose entire body is a `do` binding (`type internal Foo<'T> (c) = do
/// …`). FCS: `ObjectModel(Unspecified, [ImplicitCtor; LetBindings([Do …])], _)`.
#[test]
fn diff_internal_generic_ctor_do_body() {
    assert_asts_match("type internal Foo<'T> (c: int) =\n    do printfn \"%d\" c\n");
}

/// The minimal form — a primary-constructor class with a single `do` body.
#[test]
fn diff_ctor_class_do_body() {
    assert_asts_match("type Foo (c: int) =\n    do printfn \"%d\" c\n");
}

/// A `do` body in a no-argument class (`type Foo () = do …`).
#[test]
fn diff_no_arg_class_do_body() {
    assert_asts_match("type Foo () =\n    do printfn \"hi\"\n");
}

/// `do ()` — the body is a unit literal (`SynBinding.expr = Const(Unit)`).
#[test]
fn diff_class_do_unit() {
    assert_asts_match("type Foo () =\n    do ()\n");
}

/// `do` followed by a `member` — the `do` is one object-model member, the
/// `member` another; the `do`'s offside terminator must not swallow the next.
#[test]
fn diff_class_do_then_member() {
    assert_asts_match("type Foo () =\n    do printfn \"hi\"\n    member _.X = 1\n");
}

/// `member` followed by a `do` — the trailing `do` is the last member.
#[test]
fn diff_class_member_then_do() {
    assert_asts_match("type Foo () =\n    member _.X = 1\n    do printfn \"hi\"\n");
}

/// A class-local `let` followed by a `do` (the common init idiom: `let` to
/// capture, `do` to run a side effect over it).
#[test]
fn diff_class_let_then_do() {
    assert_asts_match("type Foo (c: int) =\n    let x = c\n    do ignore x\n");
}

/// Two consecutive `do` bindings — each is its own `LetBindings` member.
#[test]
fn diff_class_two_do() {
    assert_asts_match("type Foo () =\n    do printfn \"a\"\n    do printfn \"b\"\n");
}

/// A class-local `let` with a post-`let` attribute — `let [<Literal>] x = 1`
/// inside a type body. FCS accepts this (unlike a *pre*-`let` attribute at
/// class-local position, which it rejects), homing the run on the binding's
/// `SynBinding.attributes`. Exercises the `MemberDefn::LetBindings` normaliser
/// reading the binding's own attribute lists.
#[test]
fn diff_class_let_binding_attribute() {
    assert_asts_match("type Foo () =\n    let [<Literal>] x = 1\n    member _.X = x\n");
}

/// `static do` — FCS's `LetBindings([Do …], isStatic = true)` with a
/// `StaticDo` leading keyword.
#[test]
fn diff_class_static_do() {
    assert_asts_match("type Foo () =\n    static do printfn \"hi\"\n");
}

/// A `do` inside an explicit `class … end` repr (the kind-marked form, not the
/// lightweight one).
#[test]
fn diff_class_end_do() {
    assert_asts_match("type Foo () =\n    class\n        do printfn \"hi\"\n    end\n");
}

/// A multi-statement `do` body (a sequential block under the `do`).
#[test]
fn diff_class_do_seq_body() {
    assert_asts_match("type Foo () =\n    do\n        printfn \"a\"\n        printfn \"b\"\n");
}

// --- bare trailing `do` (phase 9.13b): a `do` augmentation after a simple repr,
// FCS's `tyconDefnRhs opt_OBLOCKSEP classDefnMembers`. The offside `do` arrives
// as a `Virtual::Do` after the body's `OBLOCKSEP`, so it routes to the outer
// `SynTypeDefn.members` (the same slot as a bare trailing `member`). FCS parses
// these (they are type-checker-rejected later — a record/union/enum has no
// constructor to run the `do` — but the *parse* is well-defined). ---

/// A bare trailing `do` after an offside record repr.
#[test]
fn diff_record_bare_trailing_do() {
    assert_asts_match("type R =\n    { X: int }\n    do printfn \"hi\"\n");
}

/// A bare trailing `do` after an offside union repr.
#[test]
fn diff_union_bare_trailing_do() {
    assert_asts_match("type U =\n    | A\n    | B\n    do printfn \"hi\"\n");
}

/// A bare trailing `do` after an offside enum repr.
#[test]
fn diff_enum_bare_trailing_do() {
    assert_asts_match("type E =\n    | A = 0\n    do printfn \"hi\"\n");
}

/// A bare trailing `do` then a bare trailing `member` after a record repr —
/// both route to the outer members slot, in order.
#[test]
fn diff_record_bare_trailing_do_then_member() {
    assert_asts_match("type R =\n    { X: int }\n    do printfn \"hi\"\n    member _.M = 1\n");
}

/// The inline `=`-line form `type R = { X: int }` then an offside `do` is FCS
/// *invalid* (the body-close `OBLOCKEND` precedes the `do`, so it is not a bare
/// trailing member): both FCS and our parser must report an error rather than
/// silently attach it. (Error-recovery shapes differ, so this asserts only the
/// error, not an AST match.)
#[test]
fn diff_record_eqline_do_is_error() {
    let parse = borzoi_cst::parser::parse("type R = { X: int }\n    do printfn \"hi\"\n");
    assert!(
        !parse.errors.is_empty(),
        "`type R = {{ … }}`⏎`  do …` is FCS-invalid and must error, not parse a \
         clean bare trailing do; got no errors",
    );
}

//! Differential test: our intra-file resolution against FCS as the oracle.
//!
//! The headline Stage C property (`docs/completed/sema-phase1-impl-plan.md`): for every
//! symbol use FCS resolves whose declaration is **in this file**, our
//! resolution at that use's range is a `Local` / `Item` pointing at a binder
//! whose range equals FCS's declaration range. We never return `Unresolved`
//! where FCS resolved in-file, and never point at the wrong binder.
//!
//! Uses FCS resolves into referenced assemblies or FSharp.Core (operators,
//! `printfn`, …) declare *outside* this file; `parse_fcs_uses` reports those
//! with `decl == None`. They are out of this slice's positive-resolution scope,
//! but still participate in the D5 soundness check: we may defer them or say
//! nothing, but must never bind them to an in-file `Local` / `Item`. The implicit
//! anonymous-module symbol is reported at a zero-width range and is skipped.
//!
//! The corpus is a curated set of snippets within the current parser subset,
//! where every in-file name resolves — so the assertion is the *strict* form
//! (every in-file use must be a matching `Local` / `Item`). `resolve_scoping.rs`
//! separately stress-tests the scoping model FCS-free.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::common::generator::generate;
use crate::common::{invoke_fcs_dump, parse_fcs_uses};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, resolve_file};
use rowan::TextRange;

/// Snippets within the parser subset whose every in-file name resolves.
const CORPUS: &[&str] = &[
    // value + reference
    "let x = 1\nlet y = x\n",
    // function definition + application of it
    "let f a b = a\nlet z = f 1 2\n",
    // recursive function references its own binder
    "let rec fac n = fac n\n",
    // non-rec self-reference resolves to the *outer* shadowed binding
    "let g = 1\nlet g = g\n",
    // position-ordered shadowing: use resolves to the latest prior binder
    "let x = 1\nlet x = 2\nlet y = x\n",
    // mutually-recursive `let rec … and …`
    "let rec a = b\nand b = a\n",
    // lambda parameter shadows the enclosing parameter
    "let q a = fun a -> a\n",
    // match-clause binder visible in the result; scrutinee sees the parameter
    "let m q = match q with w -> w\n",
    // `when` guard: the clause binder `w` and the parameter `p` are both used
    // in the guard `p w` (which precedes the result), so both the guard and
    // the result uses must resolve — the guard is resolved inside the clause's
    // pattern frame.
    "let m q p = match q with w when p w -> w\n",
    // `match!` (computation-expression match binder) scopes exactly like
    // `match`: the clause binder `w` is visible in the result, and the
    // scrutinee `m` sees the enclosing `let!` binder. `async` is out-of-file.
    "let f m = async {\n    let! r = match! m with w -> w\n    return r\n}\n",
    // `match!` with a `when` guard — the guard `p w` uses both the clause
    // binder `w` and the parameter `p`, mirroring the plain-`match` guard case.
    "let f m p = async {\n    let! r = match! m with w when p w -> w\n    return r\n}\n",
    // nullary constructor pattern: `None` is out-of-file (skipped); `opt`/`r`
    // resolve in-file — guards that the constructor head is not bound as a local
    "let r opt = match opt with None -> opt\n",
    // operator use resolves out-of-file (skipped); parameters resolve in-file
    "let add a b = a + b\n",
    // tuple of references to parameters
    "let pair a b = (a, b)\n",
    // if/then/else over a parameter
    "let h x = if x then x else x\n",
    // `while … do …` loop: the condition `c` and the body call `g ()` both
    // resolve to the enclosing parameters (a `while` binds no names of its own).
    "let f c g = while c do g ()\n",
    // `while!` (computation-expression while binder) scopes exactly like
    // `while`: the condition `c` and the body call `g ()` resolve to the
    // enclosing parameters. Inside `async { … }` so it typechecks (`async` is
    // out-of-file).
    "let f c g = async { while! c do g () }\n",
    // `for … in … do …` loop: the collection `xs` resolves to the enclosing
    // parameter (it cannot see the loop variable), and the body use `x` resolves
    // to the binder introduced by the `for` pattern (a fresh frame, like a
    // match-clause binder).
    "let f xs g = for x in xs do g x\n",
    // `for`-loop binder shadows an enclosing parameter of the same name: the
    // body use `x` resolves to the loop binder, not the outer `x`.
    "let f x xs = for x in xs do x\n",
    // `for … -> …` comprehension arrow: the collection `xs` and the function
    // `g` resolve in the enclosing scope and the yielded body use `x` resolves
    // to the loop binder, exactly as the `do` form (the body is an implicit
    // `yield`).
    "let f xs g = seq { for x in xs -> g x }\n",
    // `for i = a to b do …` range loop: both bounds (`a`, `b`) resolve to the
    // enclosing parameters, and the body use `i` resolves to the loop variable
    // introduced by the `for` (a fresh pattern-local frame).
    "let f a b = for i = a to b do g i\n",
    // record copy-and-update: the copy source `r` and the field value `v` both
    // resolve to the enclosing parameters. The field *name* `F` is a label of
    // `r`'s (external/unknown) record type, so FCS reports no in-file decl for
    // it — only `r`/`v` are checked, which our `Record` arm resolves.
    "let f r v = { r with F = v }\n",
    // NOTE: anonymous-record expressions (`{| A = a |}`) are intentionally
    // *not* in this strict corpus. Unlike a regular record's labels (external
    // decl, skipped), FCS treats an anon-record field *name* as an in-file
    // symbol that declares itself, so the strict "every FCS in-file use must
    // resolve" property would require modelling anon-field-name resolution —
    // a sema concern beyond the parser slice that introduced `AnonRecd`. The
    // `resolve_expr` `AnonRecd` arm still resolves field *values* and the copy
    // source (so those are never `Unresolved`); the parser side is covered by
    // `parser_diff_anon_recd`.
    // cross-binding reference: a later binding uses an earlier function
    "let f a = a\nlet g b = f b\n",
    // curried multi-argument application, all operands parameters
    "let compute a b c = a b c\n",
    // a parameter referenced several times in one expression
    "let dup a = a a a\n",
    // sequential references inside a parenthesised tuple
    "let s a b = (a, b, a)\n",
    // computation-expression `let!` binder: the RHS `a` resolves to the
    // enclosing parameter (outer scope), and the body's `x` resolves to the
    // `let!` binder (local). `async` is out-of-file (skipped).
    "let f a = async {\n    let! x = a\n    return x\n}\n",
    // applicative `let! … and! …`: both `x` and `y` bind for the body (one
    // `LetOrUse`), and the RHSs `a`/`b` resolve to the enclosing parameters.
    "let f a b = async {\n    let! x = a\n    and! y = b\n    return (x, y)\n}\n",
    // dotted indexer read `arr.[i]` (phase 10.16a): both the indexed object
    // `arr` and the index `i` resolve to the enclosing parameters. The indexer
    // member itself is on `arr`'s (unknown) type, so FCS reports no in-file
    // decl for it — only `arr`/`i` are checked, exercising our
    // `DotIndexedGet` arm.
    "let idx arr i = arr.[i]\n",
    // postfix member access over a paren-app `(g x).Length` (phase 10.16a):
    // the `DotGet` arm recurses into the LHS `App`, so `g` and `x` both
    // resolve to the enclosing parameters. The member `.Length` is a label of
    // the (unknown) result type — FCS reports no in-file decl for it.
    "let call g x = (g x).Length\n",
    // list literal `[a; b]` (phase 10.19): the `ArrayOrList` arm walks the
    // `Sequential` body, so both elements resolve to the earlier bindings.
    "let a = 1\nlet b = 2\nlet xs = [a; b]\n",
    // list element expressions over enclosing parameters — `[f x; g y]`
    // resolves `f`/`x`/`g`/`y` through the element `App`s.
    "let mk f g x y = [f x; g y]\n",
    // type abbreviation referencing an earlier in-file type: the `A` use in
    // `type B = A` resolves to `type A`'s binder (intra-file type go-to-def).
    // `int` is out-of-file (skipped). NOTE: *record* snippets are kept out of
    // this *strict* corpus — FCS reports a record field name as an in-file
    // self-declaring symbol, which sema does not yet intern, so the "every
    // in-file use resolves" property would fail on the field names (mirrors the
    // anon-record-field note above). *Union* snippets, by contrast, now fit: a
    // union case is interned as a `DefKind::UnionCase` value binder (the union
    // snippets at the end of this corpus), so its self-declaring name and its
    // uses both resolve. Abbreviations have no self-declaring members either.
    "type A = int\ntype B = A\n",
    // the type use nested in a compound type still resolves: the postfix
    // application head `list` is out-of-file; the argument `A` resolves through
    // the `App` recursion.
    "type A = int\ntype B = A list\n",
    // function- and tuple-type abbreviations: every `A` occurrence resolves
    // through the `Fun` / `Tuple` recursion.
    "type A = int\ntype B = A -> A\n",
    "type A = int\ntype B = A * A\n",
    // a later type references an earlier one across separate (non-`and`) decls,
    // and the chain composes: `C`'s use of `B` resolves to `type B`.
    "type A = int\ntype B = A\ntype C = B\n",
    // type-name uses in annotation positions resolve to the in-file type: a
    // value return-type annotation, a function parameter annotation, a lambda
    // parameter annotation, and an `Expr::Typed` annotation. Every in-file use
    // (the type def, the binding, the parameter, the annotation) resolves.
    "type A = int\nlet x : A = 0\n",
    "type A = int\nlet f (x : A) = x\n",
    "type A = int\nlet g = fun (x : A) -> x\n",
    "type A = int\nlet y = (0 : A)\n",
    // union cases interned as value binders: the case definitions `Red`/`Green`
    // self-resolve, and a constructor use in an expression resolves to the case.
    "type Color = Red | Green\nlet c = Red\n",
    // union cases in `match` patterns: the nullary head `A` and the applied head
    // `B` (in `B n`) both resolve to their case defs; `n` binds and its use
    // resolves to it; the scrutinee `t` resolves to the parameter. The payload
    // type `int` is out-of-file (skipped).
    "type T = A | B of int\nlet f t = match t with A -> 0 | B n -> n\n",
    // value/case source-order shadowing: a later case shadows an earlier value
    // (`let Red = 0` then the union; `let c = Red` resolves to the case), and the
    // reverse — a later value shadows an earlier case. FCS resolves the use to the
    // *latest* binding either way; cases live in the same position-ordered value
    // frame as values, so `lookup`'s latest-wins matches.
    "let Red = 0\ntype Color = Red | Green\nlet c = Red\n",
    "type Color = Red | Green\nlet Red = 0\nlet c = Red\n",
    // pattern-position shadowing: a same-named value does NOT shadow a case in a
    // *pattern* head (F#'s constructor namespace), so `match c with Red -> …`
    // resolves `Red` to the case even with `let Red = 0` in expression scope —
    // unlike the expression `let c = Red` above, which sees the value.
    "type Color = Red | Green\nlet Red = 0\nlet f c = match c with Red -> 1 | Green -> 2\n",
    // exception constructors interned as value binders, exactly like union cases:
    // the constructor definition `MyErr` self-resolves and a constructor use in an
    // expression resolves to it. The payload type `string` is out-of-file (skipped).
    "exception MyErr of string\nlet e = MyErr \"x\"\n",
    // a nullary exception used both as an expression value and as a `match`
    // pattern head: both `Bang` uses resolve to the exception def.
    "exception Bang\nlet f x = match x with Bang -> 1 | _ -> 0\n",
    // a payload exception in a `match` pattern: the head `MyErr` resolves to the
    // exception, the payload binder `z` binds and its use resolves to it, and the
    // scrutinee `x` resolves to the parameter.
    "exception MyErr of string\nlet f x = match x with MyErr z -> z | _ -> \"\"\n",
    // exception/value source-order shadowing in an *expression*: a later value
    // shadows the earlier exception constructor (`let MyErr = 0` then `let y =
    // MyErr` resolves to the value), latest-wins via `lookup` — the same
    // position-ordered value frame as union cases.
    "exception MyErr of string\nlet MyErr = 0\nlet y = MyErr\n",
    // pattern-position: a same-named value does NOT shadow the exception in a
    // *pattern* head (F#'s constructor namespace), so `match x with MyErr z -> …`
    // resolves `MyErr` to the exception even with `let MyErr = 0` in scope.
    "exception MyErr of string\nlet MyErr = 0\nlet f x = match x with MyErr z -> z | _ -> \"\"\n",
    // an exception abbreviation introduces a new constructor `Alias` in the value
    // namespace (FCS reports it as its own in-file symbol); `Alias "x"` resolves
    // to that definition.
    "exception MyErr of string\nexception Alias = MyErr\nlet e = Alias \"x\"\n",
    // a partial active pattern: the recognizer `(|Parse|_|)` and the case token
    // `Parse` self-resolve (the trailing `_` is not a case), the matched-value
    // param `s` resolves, and `Parse` used as an applied `match` head resolves to
    // the recognizer span while the payload binder `v` binds. (`Some`/`None`/`=`
    // are FSharp.Core, out-of-file.) Total active patterns are *not* in the strict
    // corpus: their body necessarily constructs cases (`… then Even`), which FCS
    // resolves but we leave deferred (cases are pattern-only; an expression use of
    // a case is FS0039) — a sound coverage gap, covered FCS-free in
    // `resolve_active_patterns.rs`. Partial patterns construct `Some`/`None`, so
    // every in-file use here resolves.
    "let (|Parse|_|) s = if s = \"\" then None else Some s\nlet f x = match x with Parse v -> v | _ -> \"\"\n",
    // a parameterized active pattern: `d`/`n` bind as params; in `match n with
    // DivBy 3 -> …` the head resolves to the recognizer and the literal `3` binds
    // nothing. (A *named* argument like `DivBy d` is a pre-existing binder gap —
    // see `define_active_pattern` — so only a literal-argument use is pinned here.)
    "let (|DivBy|_|) d n = if n % d = 0 then Some() else None\nlet h n = match n with DivBy 3 -> 1 | _ -> 0\n",
    // `let rec` puts a recognizer in scope in its own body, so a case used as a
    // *pattern* there resolves to the recognizer span (unlike the non-`rec` form,
    // where it is a fresh variable — pinned FCS-free in `resolve_active_patterns.rs`).
    "let rec (|Even|Odd|) n = match n with Even -> 1 | Odd -> 2\n",
    // an enum: the case tokens `Red`/`Green` self-resolve; a qualified `Color.Red`
    // resolves its head `Color` to the enum type and the whole span to the case.
    // (Enum cases are require-qualified — a bare `Red` is FS0039 — pinned FCS-free
    // in `resolve_enums.rs`.)
    "type Color = Red = 0 | Green = 1\nlet c = Color.Red\n",
    // qualifier latest-wins across value/type: a *later* value shadows the enum
    // type, so `Color.Red` is member access on the value (`Color` → the value),
    // not the enum case (FCS-verified).
    "type Color = Red = 0 | Green = 1\nlet Color = 0\nlet c = Color.Red\n",
    // …and the reverse order: a value *before* the enum type, so the later enum
    // type wins the qualifier — `Color` → the type and `Color.Red` → the case.
    "let Color = 0\ntype Color = Red = 0 | Green = 1\nlet c = Color.Red\n",
    // a qualified enum case as a `match` pattern head resolves like the expression
    // form: head `Color` → the type, whole `Color.Red` → the case.
    "type Color = Red = 0 | Green = 1\nlet f c = match c with Color.Red -> 1 | _ -> 0\n",
    // (The nested-module enclosing-container case — `Color.Red` / `(x : Color)`
    // inside `module Inner` seeing `Outer`'s `Color` — is covered FCS-free in
    // `resolve_enums.rs` / `resolve_types.rs`: it cannot enter this strict corpus
    // because we do not yet intern named-module headers as defs, so FCS's `Outer`
    // / `Inner` module-name symbols would be unresolved here.)
];

/// Write `source` to a uniquely-named temp `.fs` file (parallel-safe) and
/// return the path. Left on disk for the duration of the `fcs-dump` child.
fn temp_fs_file(source: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "borzoi_sema_resolve_diff_{}_{n}.fs",
        std::process::id()
    ));
    let mut f = std::fs::File::create(&path).expect("create temp .fs");
    f.write_all(source.as_bytes()).expect("write temp .fs");
    path
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// Resolve `source`, run FCS over it, and assert the headline property.
fn assert_matches_fcs(source: &str) {
    // Our resolution (parse first — a cheap failure before the costly FCS run).
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors (outside the subset?): {source:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());

    // FCS oracle.
    let path = temp_fs_file(source);
    let json = invoke_fcs_dump("uses", &path);
    let _ = std::fs::remove_file(&path);
    let uses = parse_fcs_uses(&json, source);

    let mut checked = 0usize;
    for u in &uses {
        // Skip the implicit anonymous-module symbol (zero-width range).
        if u.start == u.end {
            continue;
        }
        let use_range = span(u.start, u.end);
        let text = &source[u.start..u.end];

        // Positive side: every use FCS resolves to an in-file declaration must
        // resolve to the exact same binder. Negative side: when FCS resolves the
        // use out-of-file, sema may honestly defer / say nothing, but it must not
        // point at a local/project item instead.
        let Some((ds, de)) = u.decl else {
            let res = rf.resolution_at(use_range);
            assert!(
                !matches!(res, Some(Resolution::Local(_) | Resolution::Item(_))),
                "FCS resolved out-of-file use {text:?} at {use_range:?}, but we gave {res:?}; {source:?}"
            );
            assert!(
                !matches!(res, Some(Resolution::Unresolved)),
                "returned Unresolved where FCS resolved out-of-file: {text:?} at {use_range:?}; {source:?}"
            );
            continue;
        };

        let res = rf.resolution_at(use_range).unwrap_or_else(|| {
            panic!("FCS resolved in-file use {text:?} at {use_range:?} but we recorded nothing; {source:?}")
        });
        assert!(
            matches!(res, Resolution::Local(_) | Resolution::Item(_)),
            "FCS resolved in-file use {text:?} at {use_range:?}, but we gave {res:?}; {source:?}"
        );
        assert!(
            !matches!(res, Resolution::Unresolved),
            "returned Unresolved where FCS resolved in-file: {text:?} at {use_range:?}; {source:?}"
        );
        let def = rf
            .resolved_def(res)
            .expect("a Local/Item resolution names an in-file def");
        let expected = span(ds, de);
        assert_eq!(
            def.range, expected,
            "use {text:?} at {use_range:?}: we point at {:?}, FCS declares at {expected:?}; {source:?}",
            def.range
        );
        checked += 1;
    }

    // Every snippet must exercise at least one in-file resolution, else the
    // loop above is a silent no-op and proves nothing.
    assert!(checked > 0, "no in-file uses checked for {source:?}");
}

#[test]
fn resolution_agrees_with_fcs_over_the_corpus() {
    for source in CORPUS {
        assert_matches_fcs(source);
    }
}

/// The same headline property, but over a handful of *randomly generated*
/// well-scoped programs (`common::generator`), checked against FCS. This closes
/// the gap the curated corpus leaves: the generated programs compose the
/// scoping primitives (rec, shadowing, parameters, lambdas) in combinations the
/// curated set does not enumerate, and FCS is the independent oracle for each.
///
/// Fixed seeds (not proptest) keep the FCS-call count — and the wall-clock —
/// bounded and deterministic; the FCS-free `resolve_scoping.rs` property does
/// the high-volume sweep against the same generator.
#[test]
fn resolution_agrees_with_fcs_on_generated_programs() {
    // Varied multipliers so the seeds exercise different tapes.
    for seed in 0u32..12 {
        let nums: Vec<u32> = (0..256)
            .map(|i| {
                (seed.wrapping_add(1))
                    .wrapping_mul(2_654_435_761)
                    .wrapping_add(i * 40_503)
            })
            .collect();
        let g = generate(nums);
        // Generated programs are always in-subset; a parse failure is a
        // generator bug, surfaced loudly by the shared assertion.
        assert_matches_fcs(&g.src);
    }
}

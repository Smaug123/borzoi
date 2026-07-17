use super::super::*;
use super::*;

#[test]
fn empty_file_produces_empty_anon_module() {
    let parse = parse("");
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..0
  MODULE_OR_NAMESPACE@0..0
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless("", &parse);
}

/// Phase 10.11 — an empty signature file is a `SIG_FILE` root holding one empty
/// `MODULE_OR_NAMESPACE` (the implicit `AnonModule`), mirroring the empty `.fs`.
#[test]
fn sig_empty_green_shape() {
    let parse = parse_sig("");
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
SIG_FILE@0..0
  MODULE_OR_NAMESPACE@0..0
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless("", &parse);
}

/// Phase 10.11 — `parse_sig` on a `module M` / `namespace N` header exposes the
/// same `ModuleOrNamespace` facade as the impl side, under a `SigFile` root.
#[test]
fn sig_header_facade() {
    use crate::syntax::{AstNode, ModuleOrNamespaceKind, SigFile};

    let parse = parse_sig("module M\n");
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::NamedModule);
    let segs: Vec<String> = module
        .long_id()
        .expect("named module has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["M"]);
    assert_lossless("module M\n", &parse);

    let ns = parse_sig("namespace A.B\n");
    assert!(ns.errors.is_empty(), "errors: {:?}", ns.errors);
    let ns_file = SigFile::cast(ns.root.clone()).expect("a SIG_FILE root");
    let ns_mod = ns_file.modules().next().expect("a MODULE_OR_NAMESPACE");
    assert_eq!(ns_mod.kind(), ModuleOrNamespaceKind::DeclaredNamespace);
    assert_lossless("namespace A.B\n", &ns);
}

/// Phase 10.11 — a *second* whole-file header after an existing one
/// (`module Foo`⏎`[<A>]`⏎`module Bar`) is not a second `NamedModule`: FCS keeps
/// one `NamedModule(Foo)` and errors on the rest. The `header_parsed` latch
/// (seeded from `parse_optional_file_header`) makes the attributed `module Bar`
/// fall to the deferred arm rather than being claimed as a fresh header.
#[test]
fn sig_second_header_is_not_claimed() {
    use crate::syntax::{AstNode, ModuleOrNamespaceKind, SigFile};
    let source = "module Foo\n[<A>]\nmodule Bar\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "a second whole-file header must be flagged",
    );
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let modules: Vec<_> = file.modules().collect();
    assert_eq!(modules.len(), 1, "exactly one MODULE_OR_NAMESPACE (Foo)");
    assert_eq!(modules[0].kind(), ModuleOrNamespaceKind::NamedModule);
    let segs: Vec<String> = modules[0]
        .long_id()
        .expect("named module has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo"], "the header is Foo, not a second Bar");
    assert_lossless(source, &parse);
}

/// Phase 10.11 — a leading-attributed `module` after body content has begun
/// (`val x : int`⏎`[<AutoOpen>]`⏎`module M`) is *not* retroactively claimed as
/// the whole-file header: the `seen_decl` gate keeps the segment an `AnonModule`.
/// (`val` is itself a later-slice spec, so it errors here too — the point is the
/// segment must not become `NamedModule(M)`.)
#[test]
fn sig_header_not_claimed_after_body() {
    use crate::syntax::{AstNode, ModuleOrNamespaceKind, SigFile};
    let source = "val x : int\n[<AutoOpen>]\nmodule M\n";
    let parse = parse_sig(source);
    assert!(!parse.errors.is_empty(), "body specs/headers are flagged");
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let modules: Vec<_> = file.modules().collect();
    assert_eq!(modules.len(), 1, "one MODULE_OR_NAMESPACE");
    assert_eq!(
        modules[0].kind(),
        ModuleOrNamespaceKind::Anon,
        "the trailing `module M` must not become the file header",
    );
    assert_lossless(source, &parse);
}

/// File-form mixing is gated on a *real* top-level namespace (the loop producing
/// a 2nd segment), not a raw `namespace` token anywhere. A whole-file module
/// whose body merely mentions `namespace` in an expression (here an
/// interpolation fill) stays a single `NamedModule(M)` — it must **not** be
/// bailed to an empty `AnonModule`. (FCS's recovery for the stray keyword is
/// pathological for the diff harness, so this is asserted our-side only.)
#[test]
fn module_with_namespace_token_in_expr_is_not_mixing() {
    use crate::syntax::{AstNode, ImplFile, ModuleOrNamespaceKind};
    let source = "module M\nlet s = $\"{namespace}\"\n";
    let parse = parse(source);
    let file = ImplFile::cast(parse.root.clone()).expect("an IMPL_FILE root");
    let modules: Vec<_> = file.modules().collect();
    assert_eq!(modules.len(), 1, "one segment (no top-level namespace)");
    assert_eq!(
        modules[0].kind(),
        ModuleOrNamespaceKind::NamedModule,
        "the module is kept, not bailed to an empty AnonModule",
    );
    let segs: Vec<String> = modules[0]
        .long_id()
        .expect("named module has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["M"]);
    assert_lossless(source, &parse);
}

/// Phase 10.13a — an `open` in a `.fsi` projects to a `SigDecl::Open` (reusing
/// the impl `OPEN_DECL` node); the `open type` form is distinguished by the
/// target's `is_type`. Pins the sig-decl dispatch + `sig_decls()` accessor.
#[test]
fn sig_open_decl_facade() {
    use crate::syntax::{AstNode, OpenDecl, SigDecl, SigFile};
    let source = "open System\nopen type System.Math\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(decls.len(), 2, "two open decls");
    let SigDecl::Open(plain) = &decls[0] else {
        panic!("first decl is an open");
    };
    assert!(!OpenDecl::is_type(plain), "plain `open System`");
    let SigDecl::Open(typed) = &decls[1] else {
        panic!("second decl is an open");
    };
    assert!(OpenDecl::is_type(typed), "`open type System.Math`");
    assert_lossless(source, &parse);
}

/// Phase 10.12a — a `val x : int` in a `.fsi` projects to a `SigDecl::Val`
/// (`VAL_DECL > [VAL_TOK, VAL_SIG]`); the name and type are read off the shared
/// `VAL_SIG` carrier via `ValDecl::val_sig()`. `val mutable` sets the carrier's
/// `mutable` token (a leading modifier before the name); the type is the full
/// `parse_type` (here an arrow). Pins the dispatch + facade.
#[test]
fn sig_val_decl_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile, SyntaxKind};
    let source = "module M\nval x : int\nval mutable f : int -> string\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(decls.len(), 2, "two val decls");

    let SigDecl::Val(v0) = &decls[0] else {
        panic!("first decl is a val");
    };
    let vs0 = v0.val_sig().expect("VAL_DECL has a VAL_SIG");
    assert_eq!(vs0.ident().expect("val name").text(), "x");
    assert_eq!(
        vs0.ty().expect("val type").syntax().kind(),
        SyntaxKind::LONG_IDENT_TYPE,
        "`int` is a LongIdent type"
    );

    let SigDecl::Val(v1) = &decls[1] else {
        panic!("second decl is a val");
    };
    let vs1 = v1.val_sig().expect("VAL_DECL has a VAL_SIG");
    assert_eq!(vs1.ident().expect("val name").text(), "f");
    assert_eq!(
        vs1.ty().expect("val type").syntax().kind(),
        SyntaxKind::FUN_TYPE,
        "`int -> string` is a Fun type"
    );
    // The `mutable` keyword is a child of the VAL_SIG (a leading modifier).
    assert!(
        vs1.syntax()
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::MUTABLE_TOK),
        "`val mutable` carries a MUTABLE_TOK"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.12a — leading trivia before a `val` keyword is a *sibling* of
/// `VAL_DECL`, not a child (the `open`/`let` convention). The risk cases have no
/// layout virtual to drain the trivia first: a `val` at file start, and a `val`
/// after a same-line `;` separator with an interleaved comment. In both the
/// comment must sit outside `VAL_DECL`, and the decl's range must start at `val`.
#[test]
fn sig_val_leading_trivia_is_sibling_not_child() {
    use crate::syntax::{AstNode, SigDecl, SigFile, SyntaxKind};
    // `val` at file start (whole-file anonymous module) preceded by a comment.
    let source = "(*lead*) val x : int\nval y : int; (*c*) val z : int\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    for decl in module.sig_decls() {
        let SigDecl::Val(v) = &decl else {
            panic!("each decl is a val");
        };
        // No comment token is a child/descendant of the VAL_DECL.
        assert!(
            !v.syntax()
                .descendants_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| matches!(
                    t.kind(),
                    SyntaxKind::BLOCK_COMMENT | SyntaxKind::LINE_COMMENT
                )),
            "leading comment must not be inside VAL_DECL"
        );
        // The decl's first token is the `val` keyword (range starts at `val`).
        assert_eq!(
            v.syntax()
                .first_token()
                .expect("VAL_DECL has a first token")
                .kind(),
            SyntaxKind::VAL_TOK,
            "VAL_DECL begins at the `val` keyword"
        );
    }
    assert_lossless(source, &parse);
}

/// Phase 10.12a — a `val`/member inside a `type …` signature body must NOT be
/// promoted to a top-level `SigDecl::Val` (a phantom export), whether the member
/// is now *supported* (parsed as a member sig — `member`/`abstract`/`val`-field/
/// `inherit`/`interface`, slices 3a/3b; a bodyless `with`-augmentation member,
/// slice 4; an `and`-chain continuation, slice 5; a trailing `with`/bare-member
/// sig on a structural repr, slice 6; or an attributed `[<…>] member`, slice 8)
/// or still *unsupported* (an indented continuation of an opaque type, or the
/// invalid blockless column-0 attribute regime — skipped as ERROR). Either way a
/// *newline*-separated dedented spec survives. Each case
/// lists the exact surviving top-level `val` names and whether the body emits a
/// diagnostic (`must_error`: supported bodies parse cleanly, unsupported/invalid
/// ones flag).
#[test]
fn sig_val_in_unsupported_type_body_does_not_leak() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    fn top_level_vals(parse: &Parse) -> Vec<String> {
        let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
        let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
        module
            .sig_decls()
            .filter_map(|d| match d {
                SigDecl::Val(v) => Some(
                    v.val_sig()
                        .and_then(|vs| vs.ident())
                        .map(|t| t.text().to_string())
                        .unwrap_or_default(),
                ),
                _ => None,
            })
            .collect()
    }
    // (source, surviving top-level `val` names, must_error). The indented `val`s
    // (`x`, `a`, `b`, `p`, `q`) are type *members* and must never leak; the
    // newline-separated `val z`/`val y` are genuine top-level specs and survive.
    for (source, expected, must_error) in [
        // Offside `val`-field member body (slice 3b) — `val x` is a member, `val y`
        // top-level. Parses cleanly.
        (
            "module M\ntype C =\n  val x : int\nval y : int\n",
            vec!["y"],
            false,
        ),
        // `;`-separated `val`-field member run — neither `a` nor `b` leaks.
        (
            "module M\ntype C =\n  val a : int; val b : int\nval z : int\n",
            vec!["z"],
            false,
        ),
        // Generic header — the typar `<'T>` is a header virtual, not a boundary;
        // `val x` (member) must not leak.
        (
            "module M\ntype C<'T> =\n  val x : int\nval y : int\n",
            vec!["y"],
            false,
        ),
        // Mutually-recursive `and`-chain (slice 5) — the head's `val p` and the
        // `and B` continuation's `val q` are both members of their own definition,
        // so neither leaks; `val z` survives. Parses cleanly now.
        (
            "module M\ntype A =\n  val p : int\nand B =\n  val q : int\nval z : int\n",
            vec!["z"],
            false,
        ),
        // `and` after a one-line abbreviation (`int` RHS, then `and B` with a
        // val-field body, slice 5). The continuation's `val q` is a member; `val z`
        // survives. Parses cleanly now.
        (
            "module M\ntype A = int\nand B =\n  val q : int\nval z : int\n",
            vec!["z"],
            false,
        ),
        // The blockless column-0 after-keyword-attribute regime (name on the next
        // line) — invalid F#; the body is skipped (errors), `val y` survives.
        (
            "module M\ntype [<A>]\nC =\n  val x : int\nval y : int\n",
            vec!["y"],
            true,
        ),
        // After-keyword attribute, name on the *same* line — a normal block body, so
        // `val x` parses as a member; `val y` survives. Parses cleanly.
        (
            "module M\ntype [<A>] C =\n  val x : int\nval y : int\n",
            vec!["y"],
            false,
        ),
        // A record repr with a *trailing* `with`-augmentation member sig (slice 6):
        // the repr parses, the member sig lands in the outer slot (not leaked), and
        // `val z` survives. Parses cleanly now.
        (
            "module M\ntype C = { x: int; y: int } with member M : int\nval z : int\n",
            vec!["z"],
            false,
        ),
        // A `val`-field member body inside a nested module sig — `val x` is a member,
        // the module-level `val z` survives. Parses cleanly.
        (
            "module M\nmodule Inner =\n  type C =\n    val x : int\nval z : int\n",
            vec!["z"],
            false,
        ),
        // An *indented* continuation of an opaque type (`type T`⏎`  val x …`, no
        // `=`): invalid F#. The lex-filter leaves the indented spec at the cursor
        // with no separating virtual (it continues the type's line), so the
        // opaque-type parse must NOT treat it as a completed type and let `val x`
        // leak — it errors and skips the continuation; `val y` (offside-aligned)
        // survives.
        (
            "module M\ntype T\n  val x : int\nval y : int\n",
            vec!["y"],
            true,
        ),
        // Same, with an indented `member` continuation (also skipped, not leaked).
        (
            "module M\ntype T\n  member X : int\nval y : int\n",
            vec!["y"],
            true,
        ),
        // A mixed member body (slice 3a member + slice 3b `val`-field) — both parse,
        // `val y` survives. Parses cleanly.
        (
            "module M\ntype T =\n  member A : int\n  val x : int\nval y : int\n",
            vec!["y"],
            false,
        ),
        // The *blockless* column-0 regime with a member body — invalid F#; the
        // same-column member run is skipped (errors), the dedented `val y` survives.
        (
            "module M\ntype [<A>]\nC =\n  member M : int\n  val x : int\nval y : int\n",
            vec!["y"],
            true,
        ),
        // An *attributed* member sig (`[<DefaultValue>] val …`, slice 8) — now
        // supported: the attribute attaches to the val-field member and `val x`
        // does not leak; `val y` survives. Parses cleanly now.
        (
            "module M\ntype C =\n  [<DefaultValue>] val mutable x : int\nval y : int\n",
            vec!["y"],
            false,
        ),
        // A `new`-ctor sig inside an explicit `class … end` body (slice 3e) — now
        // a *supported* member; it parses cleanly and the dedented `val z` survives
        // as a module-level sibling (not leaked).
        (
            "module M\ntype T = class\n  new : unit -> T\nend\nval z : int\n",
            vec!["z"],
            false,
        ),
        // An unsupported member with its *own* nested `… end` (a `static type N =
        // class … end` member sig) — the skip-to-`end` depth-tracks so the nested
        // `end` is not mistaken for the outer body's closer; the outer `end` and
        // the dedented `val z` survive.
        (
            "module M\ntype T = class\n  static type N = class\n    abstract X : int\n  end\nend\nval z : int\n",
            vec!["z"],
            true,
        ),
        // An attributed member whose type uses `struct (…)` tuple syntax — both the
        // attribute (slice 8) and the struct-tuple type are now supported, so the
        // member parses cleanly. The `struct (` is a *type*, not an `end`-delimited
        // body, so it does not bump the explicit-`class … end` depth: the outer
        // `end` closes the body and the dedented `val z` survives as a module
        // sibling (FCS accepts the whole file).
        (
            "module M\ntype T = class\n  [<A>] member M : struct (int * int) -> int\nend\nval z : int\n",
            vec!["z"],
            false,
        ),
    ] {
        let parse = parse_sig(source);
        if must_error {
            assert!(
                !parse.errors.is_empty(),
                "the unsupported/invalid body errors: {source:?}"
            );
        } else {
            assert!(
                parse.errors.is_empty(),
                "the supported member body parses cleanly: {source:?} — {:?}",
                parse.errors
            );
        }
        assert_eq!(
            top_level_vals(&parse),
            expected,
            "only the genuine top-level vals survive (no member leak): {source:?}",
        );
        assert_lossless(source, &parse);
    }

    // Opaque/bodyless types (slice 2a) parse cleanly — no error, a `Types` decl,
    // and the *following* `val` survives as a top-level spec (not swallowed). The
    // `;`-separated form (`type T; val z`, valid F#: opaque spec then sibling) is
    // covered too. Each lists the surviving top-level `val` names.
    for (source, vals) in [
        ("module M\ntype T\nval z : int\n", vec!["z"]),
        ("module M\ntype T; val z : int\n", vec!["z"]),
    ] {
        let parse = parse_sig(source);
        assert!(
            parse.errors.is_empty(),
            "an opaque type parses cleanly: {source:?} — {:?}",
            parse.errors
        );
        let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
        let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
        assert!(
            module.sig_decls().any(|d| matches!(d, SigDecl::Types(_))),
            "the opaque type is a `Types` decl: {source:?}"
        );
        assert_eq!(top_level_vals(&parse), vals, "the following val survives");
        assert_lossless(source, &parse);
    }

    // A *stray* `and` (not continuing a type — `val x`⏎`and y`) is invalid F#;
    // it is skipped as a type continuation but must record a diagnostic (never
    // silently accepted). `val x` survives; the `and y` does not become a decl.
    let stray = "module M\nval x : int\nand y : int\n";
    let parse = parse_sig(stray);
    assert!(
        !parse.errors.is_empty(),
        "a stray `and` records a diagnostic"
    );
    assert_eq!(top_level_vals(&parse), vec!["x".to_string()]);
    assert_lossless(stray, &parse);

    // *Invalid* F# (FCS errors on the `;` and drops the type): a same-line top
    // separator after a one-line type abbreviation. Now that abbreviations parse
    // (phase 10.14, first slice), our coarse recovery keeps *both* the `type C =
    // int` and the `val y` — a recovery divergence from FCS (which drops the
    // type) on invalid input, acceptable here. Pinned only as no-panic +
    // lossless to document the boundary without over-promising FCS parity.
    let parse = parse_sig("module M\ntype C = int; val y : int\n");
    assert_lossless("module M\ntype C = int; val y : int\n", &parse);
}

/// Phase 10.12 (typars) — explicit value type parameters (`val f<'T, 'U> : …`)
/// parse into a `TYPAR_DECLS` child of the `VAL_SIG`, read via
/// `ValSig::typar_decls()`; an inside-`<>` `when` clause is read via
/// `ValSig::constraints()`. Reuses the phase-9.3 postfix typar-decls node.
#[test]
fn sig_val_explicit_typars_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    // Two typars, the second carrying an inside-`<>` constraint.
    let source = "module M\nval f<'T, 'U when 'U : comparison> : 'T -> 'U\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Val(v) = module.sig_decls().next().expect("a val decl") else {
        panic!("the decl is a `val`");
    };
    let vs = v.val_sig().expect("VAL_DECL has a VAL_SIG");
    let typars: Vec<String> = vs
        .typar_decls()
        .expect("explicit typars")
        .typars()
        .map(|t| t.ident().expect("typar name").text().to_string())
        .collect();
    assert_eq!(typars, vec!["T".to_string(), "U".to_string()]);
    assert_eq!(
        vs.constraints().count(),
        1,
        "one inside-`<>` `when` constraint"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.12 (literal) — a `= <literal>` value (`val x : int = 1`) parses into
/// an expression child of the `VAL_SIG`, read via `ValSig::literal_value()`
/// (distinct from the `Type` child read by `ty()`); a `val` without a literal
/// reports `None`.
#[test]
fn sig_val_literal_value_facade() {
    use crate::syntax::{AstNode, Expr, SigDecl, SigFile};
    let source = "module M\nval x : int = 1\nval y : int\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let vals: Vec<_> = module
        .sig_decls()
        .filter_map(|d| match d {
            SigDecl::Val(v) => v.val_sig(),
            _ => None,
        })
        .collect();
    assert_eq!(vals.len(), 2, "two `val` decls");
    // `val x : int = 1` carries a literal; `val y : int` does not.
    let x_lit = vals[0].literal_value();
    assert!(
        matches!(x_lit, Some(Expr::Const(_))),
        "`val x` has a const literal value: {x_lit:?}"
    );
    assert!(
        vals[1].literal_value().is_none(),
        "`val y` has no literal value"
    );
    // The literal does not leak as a sibling decl — still exactly two vals.
    assert_eq!(
        module
            .sig_decls()
            .filter(|d| matches!(d, SigDecl::Val(_)))
            .count(),
        2
    );
    assert_lossless(source, &parse);
}

/// Phase 10.12 (literal) — a `;`-separated sibling spec *after* a literal-value
/// `val` (`val x : int = 1; val y : int`) is accepted leniently and losslessly,
/// where FCS rejects it (FS0010 "Unexpected symbol ';' in value signature").
/// The `;` lands *inside* the literal RHS's offside expression block (LexFilter
/// closes the block before the `val` keyword, so the trailing `;` precedes the
/// close); the shared `parse_seq_block_body` gatherer tolerates that trailing
/// separator — the same already-documented "Repeated sequence separators"
/// leniency that accepts `let x = a; ; b`. So `val x`'s literal stays exactly
/// `1` and `val y` parses as a normal sibling (contrast the *non-literal*
/// `val x : int; val y : int`, where `;` is a genuine `topSeparators` —
/// `diff_sig_val_semi_separated`). Tightening belongs in the shared gatherer,
/// not as a val-sig special case. Pins a codex-review point (whose premise — that
/// FCS keeps the sibling — the fcs-dump oracle refutes).
#[test]
fn sig_val_literal_then_semi_sibling_is_lenient_lossless() {
    use crate::syntax::{AstNode, Expr, SigDecl, SigFile};
    let source = "module M\nval x : int = 1; val y : int\n";
    let parse = parse_sig(source);
    // Lenient: no error (FCS would emit FS0010 here).
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let vals: Vec<_> = module
        .sig_decls()
        .filter_map(|d| match d {
            SigDecl::Val(v) => v.val_sig(),
            _ => None,
        })
        .collect();
    assert_eq!(vals.len(), 2, "both `val`s present (no sibling swallowed)");
    // The trailing `;` did not corrupt `val x`'s literal — it is still just `1`.
    assert!(
        matches!(vals[0].literal_value(), Some(Expr::Const(_))),
        "`val x` literal is a lone const, not a `;`-joined sequence"
    );
    assert!(vals[1].literal_value().is_none(), "`val y` has no literal");
    assert_lossless(source, &parse);
}

/// Phase 10.12 (member-literal) — a `= <literal>` value on a *member* signature
/// (`type X =`⏎`  member a : int = 10`) parses into the member sig's `VAL_SIG`
/// expression child, read via `MemberSig::val_sig().literal_value()` (the same
/// accessor a module-level `val` literal uses). A sibling member sig without a
/// literal reports `None` and is not swallowed by the literal RHS's offside
/// block. Mirrors `sig_val_literal_value_facade`.
#[test]
fn member_sig_literal_value_facade() {
    use crate::syntax::{AstNode, Expr, MemberDefn, SigDecl, SigFile, TypeDefnRepr};
    let source = "module M\ntype X =\n  member a : int = 10\n  member b : int\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let types = module
        .sig_decls()
        .find_map(|d| match d {
            SigDecl::Types(t) => Some(t),
            _ => None,
        })
        .expect("a Types decl");
    let defn = types.defns().next().expect("a type defn");
    // The object-model member sigs nest inside the repr (not the outer/augment
    // `TypeDefn::members` slot), mirroring the normaliser's navigation.
    let Some(TypeDefnRepr::ObjectModel(om)) = defn.repr() else {
        panic!("expected an object-model repr");
    };
    let member_sigs: Vec<_> = om
        .members()
        .filter_map(|m| match m {
            MemberDefn::MemberSig(ms) => ms.val_sig(),
            _ => None,
        })
        .collect();
    assert_eq!(
        member_sigs.len(),
        2,
        "two member sigs (sibling not swallowed)"
    );
    // `member a : int = 10` carries a const literal; `member b : int` does not.
    assert!(
        matches!(member_sigs[0].literal_value(), Some(Expr::Const(_))),
        "`member a` has a const literal value: {:?}",
        member_sigs[0].literal_value()
    );
    assert!(
        member_sigs[1].literal_value().is_none(),
        "`member b` has no literal value"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.12 (member-literal) — a `new`-ctor signature has **no**
/// `optLiteralValueSpfn` slot in FCS's grammar, so `new : T = …` is rejected
/// (FS0010, fcs-dump-verified); the `=` is left for the member-block loop, which
/// flags it. We must record a diagnostic and stay lossless rather than leniently
/// accept a literal there.
#[test]
fn member_sig_new_ctor_literal_is_rejected() {
    let source = "module M\ntype X =\n  new : X = 1\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "a literal on a `new`-ctor sig is rejected (no FCS literal slot)"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.12 (typars) — the *adjacent* empty form `val f<> : int` errors,
/// matching FCS: the lexer reads `<>` as the not-equal operator (not a `Less`),
/// so it never opens a typar list. (The *spaced* `val f< > : int` is valid — FCS's
/// `valSpfn` permits an empty typar core — covered by the
/// `diff_sig_val_empty_typar_list_spaced` diff test.) Pins a codex-review point;
/// stays lossless either way.
#[test]
fn sig_val_adjacent_empty_typar_is_rejected() {
    let source = "module M\nval f<> : int\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "the adjacent `<>` (not-equal operator) must be rejected, matching FCS"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.12 (typars) — the `<…, ..>` flexible-typar tail is **not** a valid
/// value signature: our oracle FCS rejects `val f<'T, ..> : 'T -> 'T` ("Unexpected
/// infix operator in value signature" — the `..` is read as an infix operator
/// here, not the flex-typar tail). Our parser likewise errors and stays lossless
/// (no silent accept, no panic). Pins a codex-review point. (`parse_typar_decls_‑
/// postfix` does not model the `, ..` tail; it is reported, not corrupted.)
#[test]
fn sig_val_flex_typar_tail_is_rejected() {
    let source = "module M\nval f<'T, ..> : 'T -> 'T\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "the `<…, ..>` flex tail is rejected in a value sig, matching FCS"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.12b — a named / optional parameter in a `val` signature type projects
/// to a `Type::SignatureParameter` at each arrow-argument position. `val f : x:
/// int -> ?y: string -> bool` is `Fun(SigParam(x, int), Fun(SigParam(?y, string),
/// bool))`; the facade reads each param's `is_optional` / `name` / `value_type`.
#[test]
fn sig_val_named_param_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile, Type};
    let source = "module M\nval f : x: int -> ?y: string -> bool\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Val(v) = module.sig_decls().next().expect("a val decl") else {
        panic!("the decl is a `val`");
    };
    let ty = v
        .val_sig()
        .and_then(|vs| vs.ty())
        .expect("the val sig has a type");
    // Walk the arrow spine collecting each labelled parameter.
    let mut params: Vec<(bool, String)> = Vec::new();
    let mut cur = ty;
    while let Type::Fun(f) = &cur {
        let arg = f.arg().expect("FUN_TYPE has an arg");
        if let Type::SignatureParameter(p) = &arg {
            params.push((
                p.is_optional(),
                p.name().expect("a param name").text().to_string(),
            ));
        }
        let ret = f.ret().expect("FUN_TYPE has a ret");
        cur = ret;
    }
    assert_eq!(
        params,
        vec![(false, "x".to_string()), (true, "y".to_string())],
        "x is named, y is optional"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.12b — scope boundary: a parenthesised `(x: int)` resets to the
/// general `typ` grammar, where a labelled argument is **not** a
/// `SignatureParameter` (FCS rejects it). Our parser likewise does not treat it
/// as one — it records an error and stays lossless, not silently accepted.
#[test]
fn sig_val_paren_param_not_signature_parameter() {
    let source = "module M\nval f : (x: int) -> int\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "a labelled arg inside parens is not a SignatureParameter — must error"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.12b — an *incomplete* labelled parameter (the `:` followed by a
/// non-type token, common during an in-progress edit) must record a clean error
/// and stay lossless, **not** panic. The value-type parse is guarded so it never
/// reaches `parse_atomic_type`'s `unreachable!`. (Regression for a codex-review
/// finding.)
#[test]
fn sig_val_incomplete_param_stays_lossless() {
    for source in [
        "module M\nval f : x: -> int\n",           // `:` then `->`
        "namespace N\ntype D = delegate of ?x:\n", // optional, `:` then EOF
        "module M\nval f : x:\n",                  // named, `:` then newline
    ] {
        let parse = parse_sig(source);
        assert!(
            !parse.errors.is_empty(),
            "an incomplete labelled param must record a diagnostic: {source:?}"
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 10.12b — the sig-param lookahead must not cross a layout boundary: a
/// dedented sibling after a dangling `val f :` (`val f :`⏎`val g : int`) must
/// **not** be stolen as `f`'s labelled parameter — `g` arrives behind a layout
/// virtual, so the filtered-cursor gate declines. Both `val f` and `val g` survive
/// as separate decls. (Regression for a codex-review finding on the raw-only
/// lookahead.)
#[test]
fn sig_val_dangling_colon_does_not_steal_sibling() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "module M\nval f :\nval g : int\n";
    let parse = parse_sig(source);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let vals: Vec<String> = module
        .sig_decls()
        .filter_map(|d| match d {
            SigDecl::Val(v) => Some(
                v.val_sig()
                    .and_then(|s| s.ident())
                    .map(|t| t.text().to_string())
                    .unwrap_or_default(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(
        vals,
        vec!["f".to_string(), "g".to_string()],
        "`val g` survives as its own decl, not stolen as `f`'s parameter label"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.15 (first slice) — an exception signature (`exception E of int`) in a
/// `.fsi` projects to `SigDecl::Exception`, reusing the impl `EXCEPTION_DEFN` node:
/// the case name + `of` fields read via `ExceptionDefnDecl::union_case`, and an
/// abbreviation target via `abbrev_path`.
#[test]
fn sig_exception_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "module M\nexception E of int\nexception Alias = SomeExn\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(decls.len(), 2, "two exception sig decls");

    let SigDecl::Exception(e0) = &decls[0] else {
        panic!("first decl is an exception: {:?}", decls[0]);
    };
    let case = e0.union_case().expect("the exception has a case");
    assert_eq!(
        case.ident().expect("case name").text(),
        "E",
        "exception case name"
    );
    assert!(
        e0.abbrev_path().is_none(),
        "the `of` form is not an abbreviation"
    );

    let SigDecl::Exception(e1) = &decls[1] else {
        panic!("second decl is an exception: {:?}", decls[1]);
    };
    let abbrev: Vec<String> = e1
        .abbrev_path()
        .expect("the abbreviation target")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(abbrev, vec!["SomeExn"], "abbreviation RHS path");
    assert_lossless(source, &parse);
}

/// Phase 10.15 (second slice) — the `with member …` exception-sig augmentation
/// (member *sigs*, FCS's `opt_classSpfn`) projects its members into the outer
/// `SynExceptionSig.members` slot, read via `ExceptionDefnDecl::members()` (the
/// `MEMBER_SIG` children after the `WITH_TOK`). The augment's member sigs are not
/// leaked as sibling top-level decls.
#[test]
fn sig_exception_with_members_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "module M\nexception E of int\n  with member M : int\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "only the exception decl — no leaked member sig: {decls:?}"
    );
    let SigDecl::Exception(e) = &decls[0] else {
        panic!("the single decl is the exception: {:?}", decls[0]);
    };
    let members: Vec<_> = e.members().collect();
    assert_eq!(members.len(), 1, "the augmentation's one member sig");
    assert!(
        matches!(members[0], crate::syntax::MemberDefn::MemberSig(_)),
        "the augment member is a MEMBER_SIG, not a member body"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.15 (second slice) — a `with` augmentation member kind this slice does
/// not model is *contained* inside the `EXCEPTION_DEFN` rather than escaping as a
/// sibling top-level spec, for the same-line `OWITH … OEND` LexFilter form
/// (`Virtual::With`, contained by `skip_owith_block_as_error`): a same-line
/// `[<A>] member …` (attribute) or `private member …` (leading access). (The
/// *offside* attributed form `with`⏎`  [<A>] member …` is now supported — slice 8;
/// see `sig_exception_augment_attributed_member_supported`.)
///
/// Each records a diagnostic, keeps the deferred member *within* the exception
/// node's span, and lets the following sibling `val` survive as its own decl.
#[test]
fn sig_exception_augment_unsupported_member_stays_contained() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    for source in [
        // same-line attribute → `OWITH` form
        "module M\nexception E with [<System.Obsolete>] member M : int\nval f : int\n",
        // same-line leading access → `OWITH` form
        "module M\nexception E with private member M : int\nval f : int\n",
    ] {
        let parse = parse_sig(source);
        assert!(
            !parse.errors.is_empty(),
            "a deferred augment member kind should record a diagnostic: {source:?}"
        );
        let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
        let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
        let decls: Vec<_> = module.sig_decls().collect();
        assert_eq!(
            decls.len(),
            2,
            "the exception (containing the deferred member) and the sibling `val` — \
             no leaked member spec: {source:?} → {decls:?}"
        );
        let SigDecl::Exception(e) = &decls[0] else {
            panic!(
                "{source:?}: first decl is the exception, got {:?}",
                decls[0]
            );
        };
        // Containment: the deferred `member …` text sits *inside* the exception
        // node's span (not escaped to a sibling). Distinguishes containment from a
        // mere absence of extra `SigDecl`s.
        assert!(
            e.syntax().text().to_string().contains("member"),
            "{source:?}: the deferred member stays inside the EXCEPTION_DEFN"
        );
        assert!(
            matches!(decls[1], SigDecl::Val(_)),
            "{source:?}: the sibling `val` survives as its own decl"
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 10.14 (slice 8) — an *offside* attributed member sig in an exception
/// `with`-augmentation (`exception E with`⏎`  [<A>] member M : int`) is now
/// supported: it parses cleanly into the outer `SynExceptionSig.members` slot as a
/// `MemberSig` carrying its attribute list; the sibling `val` survives.
#[test]
fn sig_exception_augment_attributed_member_supported() {
    use crate::syntax::{AstNode, MemberDefn, SigDecl, SigFile};
    let source = "module M\nexception E with\n  [<System.Obsolete>] member M : int\nval f : int\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(decls.len(), 2, "the exception + sibling val: {decls:?}");
    let SigDecl::Exception(e) = &decls[0] else {
        panic!("first decl is the exception: {:?}", decls[0]);
    };
    let member = e.members().next().expect("one augment member");
    let MemberDefn::MemberSig(ms) = member else {
        panic!("the augment member is a `MemberSig`: {member:?}");
    };
    assert_eq!(
        ms.attributes().count(),
        1,
        "the member sig carries its attribute"
    );
    assert!(
        matches!(decls[1], SigDecl::Val(_)),
        "the sibling `val` survives"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.13b — a nested module sig (`module M =`⏎`  open System`) and a module
/// abbreviation (`module A = B.C`) in a `.fsi` project to `SigDecl::NestedModule`
/// / `SigDecl::ModuleAbbrev` (reusing the impl nodes); the nested body is read
/// via `NestedModuleDecl::sig_decls()`.
#[test]
fn sig_nested_module_and_abbrev_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "module M =\n  open System\nmodule A = B.C\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(decls.len(), 2, "a nested module and an abbreviation");

    let SigDecl::NestedModule(nm) = &decls[0] else {
        panic!("first decl is a nested module");
    };
    let nm_name: Vec<String> = nm
        .long_id()
        .expect("nested module has a name")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(nm_name, vec!["M"]);
    let body: Vec<_> = nm.sig_decls().collect();
    assert_eq!(body.len(), 1, "the body's `open`");
    assert!(matches!(body[0], SigDecl::Open(_)), "body decl is an open");

    let SigDecl::ModuleAbbrev(ab) = &decls[1] else {
        panic!("second decl is an abbreviation");
    };
    let lhs: Vec<String> = ab
        .ident()
        .expect("abbrev LHS")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    let rhs: Vec<String> = ab
        .long_id()
        .expect("abbrev RHS")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(lhs, vec!["A"]);
    assert_eq!(rhs, vec!["B", "C"]);
    assert_lossless(source, &parse);
}

/// Phase 10.14 (first slice) — a type-abbreviation signature (`type Alias = int`)
/// in a `.fsi` projects to `SigDecl::Types` reusing the impl `TYPE_DEFNS` node:
/// one `TypeDefn` named `Alias` whose repr is a `TypeDefnRepr::Abbrev` over the
/// RHS type. Two *separate* `type` specs stay two `SigDecl::Types` groups.
#[test]
fn sig_type_abbrev_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile, TypeDefnRepr};
    let source = "module M\ntype Alias = int\ntype Other = string\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(decls.len(), 2, "two separate type-abbreviation groups");

    let names: Vec<(String, String)> = decls
        .iter()
        .map(|d| {
            let SigDecl::Types(t) = d else {
                panic!("each decl is a `Types` group: {d:?}");
            };
            let defns: Vec<_> = t.defns().collect();
            assert_eq!(defns.len(), 1, "one definition per group (no `and`-chain)");
            let defn = &defns[0];
            let name = defn
                .long_id()
                .expect("the definition has a name")
                .idents()
                .map(|tok| tok.text().to_string())
                .collect::<Vec<_>>()
                .join(".");
            let TypeDefnRepr::Abbrev(ab) = defn.repr().expect("an abbreviation repr") else {
                panic!("the repr is a type abbreviation");
            };
            let rhs = ab
                .ty()
                .expect("the abbreviation RHS type")
                .syntax()
                .text()
                .to_string();
            (name, rhs)
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("Alias".to_string(), "int".to_string()),
            ("Other".to_string(), "string".to_string()),
        ],
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 2a) — an opaque/bodyless type signature (`type T`,
/// `type Box<'a>`) projects to `SigDecl::Types` whose single `TypeDefn` has a
/// name (and any type parameters) but **no** repr (`repr() == None`,
/// `SynTypeDefnSimpleRepr.None`).
#[test]
fn sig_type_opaque_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "namespace N\ntype T\ntype Box<'a>\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(decls.len(), 2, "two separate opaque-type groups");

    let shapes: Vec<(String, usize, bool)> = decls
        .iter()
        .map(|d| {
            let SigDecl::Types(t) = d else {
                panic!("each decl is a `Types` group: {d:?}");
            };
            let defns: Vec<_> = t.defns().collect();
            assert_eq!(defns.len(), 1, "one definition per group");
            let defn = &defns[0];
            let name = defn
                .long_id()
                .expect("the definition has a name")
                .idents()
                .map(|tok| tok.text().to_string())
                .collect::<Vec<_>>()
                .join(".");
            let typars = defn
                .typar_decls()
                .map(|ds| ds.typars().count())
                .unwrap_or(0);
            (name, typars, defn.repr().is_some())
        })
        .collect();
    assert_eq!(
        shapes,
        vec![("T".to_string(), 0, false), ("Box".to_string(), 1, false),],
        "opaque types carry name + typars but no repr",
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 2b) — record / union / enum signature reprs project to
/// `SigDecl::Types` whose `TypeDefn` carries the matching `TypeDefnRepr`
/// (`Record` / `Union` / `Enum`), reusing the impl-side repr nodes. The record's
/// fields and the union/enum's cases are read through the shared accessors.
#[test]
fn sig_type_structural_repr_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile, TypeDefnRepr};
    let source =
        "namespace N\ntype R = { X : int }\ntype U = A | B of int\ntype E = X = 0 | Y = 1\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(decls.len(), 3, "record, union, enum — three `Types` groups");

    let reprs: Vec<(String, String, usize)> = decls
        .iter()
        .map(|d| {
            let SigDecl::Types(t) = d else {
                panic!("each decl is a `Types` group: {d:?}");
            };
            let defn = t.defns().next().expect("one definition per group");
            let name = defn
                .long_id()
                .expect("name")
                .idents()
                .map(|tok| tok.text().to_string())
                .collect::<Vec<_>>()
                .join(".");
            let (kind, count) = match defn.repr().expect("a repr") {
                TypeDefnRepr::Record(r) => ("record", r.fields().count()),
                TypeDefnRepr::Union(u) => ("union", u.cases().count()),
                TypeDefnRepr::Enum(e) => ("enum", e.cases().count()),
                other => panic!("unexpected repr {other:?}"),
            };
            (name, kind.to_string(), count)
        })
        .collect();
    assert_eq!(
        reprs,
        vec![
            ("R".to_string(), "record".to_string(), 1),
            ("U".to_string(), "union".to_string(), 2),
            ("E".to_string(), "enum".to_string(), 2),
        ],
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 3a) — `member`/`abstract`/`static member` signatures in a
/// lightweight object-model body project to `SigDecl::Types` whose `TypeDefn`
/// carries an `ObjectModel` repr of [`MemberSig`](crate::syntax::MemberSig)
/// members, each reading its name (via the `VAL_SIG` carrier) and leading
/// keyword.
#[test]
fn sig_type_member_sig_facade() {
    use crate::syntax::{AstNode, MemberDefn, MemberSigLeading, SigDecl, SigFile, TypeDefnRepr};
    let source = "namespace N\ntype I =\n  abstract Name : string\n  member Compute : int\n  static member Make : int -> I\n  abstract member M : int\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
        panic!("the decl is a `Types` group");
    };
    let defn = t.defns().next().expect("one definition");
    let TypeDefnRepr::ObjectModel(o) = defn.repr().expect("an object-model repr") else {
        panic!("the repr is an object model");
    };
    let members: Vec<(MemberSigLeading, String)> = o
        .members()
        .map(|m| {
            let MemberDefn::MemberSig(ms) = m else {
                panic!("each member is a `MemberSig`: {m:?}");
            };
            let name = ms
                .val_sig()
                .and_then(|vs| vs.ident())
                .map(|t| t.text().to_string())
                .expect("a member name");
            (ms.leading_keyword(), name)
        })
        .collect();
    assert_eq!(
        members,
        vec![
            (MemberSigLeading::Abstract, "Name".to_string()),
            (MemberSigLeading::Member, "Compute".to_string()),
            (MemberSigLeading::StaticMember, "Make".to_string()),
            (MemberSigLeading::AbstractMember, "M".to_string()),
        ],
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 3b) — `inherit` / `interface` / `val`-field member sigs in a
/// signature object-model body project to the reused impl-side member nodes
/// (`MemberDefn::Inherit` / `Interface` / `ValField`) inside the `OBJECT_MODEL_REPR`,
/// interleaved with the slice-3a `MemberSig`s.
#[test]
fn sig_type_member_3b_facade() {
    use crate::syntax::{AstNode, MemberDefn, SigDecl, SigFile, TypeDefnRepr};
    let source = "namespace N\ntype T =\n  inherit Base\n  interface IFoo\n  val x : int\n  abstract M : int\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
        panic!("the decl is a `Types` group");
    };
    let defn = t.defns().next().expect("one definition");
    let TypeDefnRepr::ObjectModel(o) = defn.repr().expect("an object-model repr") else {
        panic!("the repr is an object model");
    };
    let kinds: Vec<&str> = o
        .members()
        .map(|m| match m {
            MemberDefn::Inherit(_) => "inherit",
            MemberDefn::Interface(_) => "interface",
            MemberDefn::ValField(_) => "valfield",
            MemberDefn::MemberSig(_) => "membersig",
            other => panic!("unexpected member kind: {other:?}"),
        })
        .collect();
    assert_eq!(kinds, vec!["inherit", "interface", "valfield", "membersig"],);
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 4) — a `with`-augmentation on a bodyless type
/// (`type T with member …`) projects to `SigDecl::Types` whose `TypeDefn` has
/// **no** repr (`repr() == None`, FCS's `Simple(SynTypeDefnSimpleRepr.None)`) and
/// whose augmentation member *sigs* land in the *outer* `members()` slot (the
/// `MEMBER_SIG` children after the `WITH_TOK`) — *not* in an `ObjectModel` repr,
/// unlike the impl-side `Augmentation`. The members are not leaked as sibling
/// top-level decls.
#[test]
fn sig_type_with_augmentation_facade() {
    use crate::syntax::{AstNode, MemberDefn, MemberSigLeading, SigDecl, SigFile};
    let source = "namespace N\ntype T with\n  member M : int\n  static member Make : int -> T\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "only the type decl — no leaked member sig: {decls:?}"
    );
    let SigDecl::Types(t) = &decls[0] else {
        panic!("the single decl is a `Types` group: {:?}", decls[0]);
    };
    let defn = t.defns().next().expect("one definition");
    assert!(
        defn.repr().is_none(),
        "a `with`-augmentation leaves the repr absent (Simple(None)): {:?}",
        defn.repr()
    );
    let members: Vec<(MemberSigLeading, String)> = defn
        .members()
        .map(|m| {
            let MemberDefn::MemberSig(ms) = m else {
                panic!("each augment member is a `MemberSig`: {m:?}");
            };
            let name = ms
                .val_sig()
                .and_then(|vs| vs.ident())
                .map(|t| t.text().to_string())
                .expect("a member name");
            (ms.leading_keyword(), name)
        })
        .collect();
    assert_eq!(
        members,
        vec![
            (MemberSigLeading::Member, "M".to_string()),
            (MemberSigLeading::StaticMember, "Make".to_string()),
        ],
        "the augmentation's member sigs land in the outer slot",
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 4) — a `with`-augmentation member kind this slice does not
/// model is *contained* inside the `TYPE_DEFN` rather than escaping as a sibling
/// top-level spec, for the same-line `OWITH … OEND` LexFilter form
/// (`Virtual::With`, contained by `skip_owith_block_as_error`): a same-line
/// `[<A>] member …` (attribute) or `private member …` (leading access). (The
/// *offside* attributed form `with`⏎`  [<A>] member …` is now supported — slice 8;
/// see `sig_type_augment_attributed_member_supported`.)
///
/// Each records a diagnostic, keeps the deferred member *within* the type node's
/// span, and lets the following sibling `val` survive as its own decl.
#[test]
fn sig_type_augment_unsupported_member_stays_contained() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    for source in [
        // same-line attribute → `OWITH` form
        "module M\ntype T with [<System.Obsolete>] member M : int\nval f : int\n",
        // same-line leading access → `OWITH` form
        "module M\ntype T with private member M : int\nval f : int\n",
    ] {
        let parse = parse_sig(source);
        assert!(
            !parse.errors.is_empty(),
            "a deferred augment member kind should record a diagnostic: {source:?}"
        );
        let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
        let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
        let decls: Vec<_> = module.sig_decls().collect();
        assert_eq!(
            decls.len(),
            2,
            "the type (containing the deferred member) and the sibling `val` — \
             no leaked member spec: {source:?} → {decls:?}"
        );
        let SigDecl::Types(t) = &decls[0] else {
            panic!(
                "{source:?}: first decl is the type group, got {:?}",
                decls[0]
            );
        };
        // Containment: the deferred `member …` text sits *inside* the type group's
        // span (not escaped to a sibling).
        assert!(
            t.syntax().text().to_string().contains("member"),
            "{source:?}: the deferred member stays inside the TYPE_DEFNS"
        );
        assert!(
            matches!(decls[1], SigDecl::Val(_)),
            "{source:?}: the sibling `val` survives as its own decl"
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 10.14 (slice 8) — an *offside* attributed member sig in a bodyless type
/// `with`-augmentation (`type T with`⏎`  [<A>] member M : int`) is now supported:
/// it parses cleanly into the outer `SynTypeDefnSig.members` slot as a `MemberSig`
/// carrying its attribute list; the sibling `val` survives.
#[test]
fn sig_type_augment_attributed_member_supported() {
    use crate::syntax::{AstNode, MemberDefn, SigDecl, SigFile};
    let source = "module M\ntype T with\n  [<System.Obsolete>] member M : int\nval f : int\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(decls.len(), 2, "the type + sibling val: {decls:?}");
    let SigDecl::Types(t) = &decls[0] else {
        panic!("first decl is the type group: {:?}", decls[0]);
    };
    let defn = t.defns().next().expect("one definition");
    let member = defn.members().next().expect("one augment member");
    let MemberDefn::MemberSig(ms) = member else {
        panic!("the augment member is a `MemberSig`: {member:?}");
    };
    assert_eq!(
        ms.attributes().count(),
        1,
        "the member sig carries its attribute"
    );
    assert!(
        matches!(decls[1], SigDecl::Val(_)),
        "the sibling `val` survives"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 5) — an `and`-chained type signature is **one**
/// `SigDecl::Types` group holding several `TypeDefn`s, each continuation leading
/// with an `AND_TOK` (mirroring the impl-side `type_and_chain_facade`). Only a
/// fresh `type` keyword starts a new group.
#[test]
fn sig_type_and_chain_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "namespace N\ntype A = int\nand B = string\nand C = bool\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let groups: Vec<_> = module
        .sig_decls()
        .filter_map(|d| match d {
            SigDecl::Types(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(groups.len(), 1, "an `and`-chain is one Types group");
    let names: Vec<String> = groups[0]
        .defns()
        .map(|d| {
            d.long_id()
                .expect("each defn has a name")
                .idents()
                .map(|t| t.text().to_string())
                .collect::<Vec<_>>()
                .join(".")
        })
        .collect();
    assert_eq!(
        names,
        vec!["A", "B", "C"],
        "all three definitions in one group"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 5) — a *single-line* `and`-chain in a `.fsi` is invalid F#
/// (FCS rejects it; the inline `and` stays inside the first body's still-open
/// offside block). The parser must **not** splice it into a chain — it records an
/// error and leaves the `and` for the enclosing loop, rather than silently
/// accepting it. Mirrors the impl-side `inline_type_and_chain_is_rejected`.
#[test]
fn inline_sig_type_and_chain_is_rejected() {
    let source = "namespace N\ntype A = int and B = string\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "an inline `type … and …` sig must be rejected, not silently accepted"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 6) — a structural repr carrying a trailing member sig (a
/// `with`-augmentation, or a bare offside member) keeps the repr (`Record`) and
/// homes the member sig in the *outer* `members()` slot of the `TYPE_DEFN` (a
/// `MEMBER_SIG` child after the repr node, like the slice-4 bodyless augmentation
/// but over a non-`None` repr). The member is not leaked as a sibling decl.
#[test]
fn sig_type_trailing_member_facade() {
    use crate::syntax::{AstNode, MemberDefn, SigDecl, SigFile, TypeDefnRepr};
    for source in [
        // `with`-augmentation form.
        "namespace N\ntype R = { x : int } with member M : int\n",
        // bare offside form.
        "namespace N\ntype R =\n  { x : int }\n  member M : int\n",
    ] {
        let parse = parse_sig(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?}: errors: {:?}",
            parse.errors
        );
        let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
        let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
        let decls: Vec<_> = module.sig_decls().collect();
        assert_eq!(decls.len(), 1, "{source:?}: only the type decl: {decls:?}");
        let SigDecl::Types(t) = &decls[0] else {
            panic!("{source:?}: the decl is a `Types` group: {:?}", decls[0]);
        };
        let defn = t.defns().next().expect("one definition");
        assert!(
            matches!(defn.repr(), Some(TypeDefnRepr::Record(_))),
            "{source:?}: the repr stays a record: {:?}",
            defn.repr()
        );
        let member_names: Vec<String> = defn
            .members()
            .map(|m| {
                let MemberDefn::MemberSig(ms) = m else {
                    panic!("{source:?}: the outer member is a `MemberSig`: {m:?}");
                };
                ms.val_sig()
                    .and_then(|vs| vs.ident())
                    .map(|t| t.text().to_string())
                    .expect("a member name")
            })
            .collect();
        assert_eq!(
            member_names,
            vec!["M".to_string()],
            "{source:?}: the trailing member sig lands in the outer slot"
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 10.14 (slice 6) — a trailing-member-bearing structural head composes
/// with the slice-5 `and`-chain: `type R = { … } with member …`⏎`and U =`⏎`  val
/// q` is one `Types` group of two definitions, the head's member sig is in `R`'s
/// outer slot, the continuation's `val q` is `U`'s member, and only the genuine
/// sibling `val z` is top-level (no phantom export).
#[test]
fn sig_type_trailing_member_head_then_and_chain() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    for source in [
        // Trailing `with`-augmentation on a record head, then `and U`.
        "module M\ntype R = { x : int } with member M : int\nand U =\n  val q : int\nval z : int\n",
        // Bare trailing member sig on an offside record head, then `and U`.
        "module M\ntype R =\n  { x : int }\n  member M : int\nand U =\n  val q : int\nval z : int\n",
    ] {
        let parse = parse_sig(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?}: errors: {:?}",
            parse.errors
        );
        let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
        let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
        let top_vals: Vec<String> = module
            .sig_decls()
            .filter_map(|d| match d {
                SigDecl::Val(v) => Some(
                    v.val_sig()
                        .and_then(|vs| vs.ident())
                        .map(|t| t.text().to_string())
                        .unwrap_or_default(),
                ),
                _ => None,
            })
            .collect();
        assert_eq!(
            top_vals,
            vec!["z".to_string()],
            "{source:?}: only `val z` is top-level; `val q` is `U`'s member"
        );
        let groups: Vec<_> = module
            .sig_decls()
            .filter_map(|d| match d {
                SigDecl::Types(t) => Some(t.defns().count()),
                _ => None,
            })
            .collect();
        assert_eq!(
            groups,
            vec![2],
            "{source:?}: head + `and` continuation stay one Types group of two defns"
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 10.14 (slice 6) — the FCS-*invalid* layout `type R = { … }`⏎`  member …`
/// (an inline record body with an indented member) must stay rejected, not
/// silently accepted as a bare trailing member. LexFilter closes the record's
/// block (`OBLOCKEND`) *before* the indented member, so neither bare-trailing gate
/// fires; the repr is kept (no outer members) and the stray `member` is flagged by
/// the module loop. Lossless, errors present, no phantom export. (Mirrors FCS,
/// which errors here while accepting the offside `type R =`⏎`  { … }`⏎`  member`.)
#[test]
fn sig_type_inline_record_indented_member_is_rejected() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "namespace N\ntype R = { x : int }\n  member M : int\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "the inline-record + indented-member layout must be rejected, not accepted"
    );
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
        panic!("the decl is a `Types` group");
    };
    let defn = t.defns().next().expect("one definition");
    assert_eq!(
        defn.members().count(),
        0,
        "the indented member is not accepted into the outer slot"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 6) — in the blockless column-0 after-keyword-attribute
/// regime (`type [<A>]`⏎`T = int`), a dedented sibling `val y` must **not** be
/// swallowed as a bare trailing member of `T` (which would lose the top-level
/// export). The bare-member gate is guarded on `opened_block`, so with no body
/// block the `val` stays a module-level sibling. (Regression for a codex-review
/// finding on the trailing-members slice; this regime has no body block, so the
/// `OBLOCKSEP`-before-`val` cannot be told from a real member layout.)
#[test]
fn sig_type_col0_attr_regime_does_not_swallow_sibling_val() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "module M\ntype [<A>]\nT = int\nval y : int\n";
    let parse = parse_sig(source);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let top_vals: Vec<String> = module
        .sig_decls()
        .filter_map(|d| match d {
            SigDecl::Val(v) => Some(
                v.val_sig()
                    .and_then(|vs| vs.ident())
                    .map(|t| t.text().to_string())
                    .unwrap_or_default(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(
        top_vals,
        vec!["y".to_string()],
        "the dedented `val y` stays a top-level sibling, not a phantom member"
    );
    let outer_members: usize = module
        .sig_decls()
        .filter_map(|d| match d {
            SigDecl::Types(t) => Some(t.defns().map(|x| x.members().count()).sum::<usize>()),
            _ => None,
        })
        .sum();
    assert_eq!(
        outer_members, 0,
        "`val y` was not swallowed as `T`'s member"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 6) — a bare trailing-member run *followed by* a `with`
/// augmentation is FCS's `checkForMultipleAugmentations` error ("At most one
/// 'with' augmentation is permitted"). We flag it (mirroring the impl) and still
/// parse the block losslessly; no member leaks to a sibling decl. (Regression for
/// a codex-review finding on the trailing-members slice.)
#[test]
fn sig_type_bare_members_then_with_augment_errors() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "namespace N\ntype R =\n  { x : int }\n  member A : int\n  with member B : int\n";
    let parse = parse_sig(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("At most one 'with' augmentation")),
        "the double augmentation must be flagged: {:?}",
        parse.errors
    );
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "only the type decl — no member leaked as a sibling: {decls:?}"
    );
    assert!(matches!(decls[0], SigDecl::Types(_)));
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 7) — a delegate signature (`type T = delegate of int ->
/// int`) projects to `SigDecl::Types` whose `TypeDefn` carries a
/// `TypeDefnRepr::Delegate` wrapping the signature type, with no outer members
/// (reusing the impl-side `DELEGATE_REPR` node).
#[test]
fn sig_type_delegate_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile, TypeDefnRepr};
    let source = "namespace N\ntype T = delegate of int -> int\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
        panic!("the decl is a `Types` group");
    };
    let defn = t.defns().next().expect("one definition");
    assert!(
        matches!(defn.repr(), Some(TypeDefnRepr::Delegate(_))),
        "the repr is a delegate: {:?}",
        defn.repr()
    );
    assert_eq!(
        defn.members().count(),
        0,
        "a delegate sig has no outer members"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 7) — FCS forbids an augmentation on a delegate type
/// (`parsAugmentationsIllegalOnDelegateType`). A `type T = delegate of … with
/// member …` is flagged (mirroring the impl) and still parses losslessly; no
/// member leaks as a sibling decl.
#[test]
fn sig_type_delegate_augmentation_errors() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "namespace N\ntype T = delegate of int -> int with member M : int\n";
    let parse = parse_sig(source);
    assert!(
        parse.errors.iter().any(|e| e.message.contains("delegate")),
        "the illegal delegate augmentation must be flagged: {:?}",
        parse.errors
    );
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "only the type decl — no member leaked as a sibling: {decls:?}"
    );
    assert!(matches!(decls[0], SigDecl::Types(_)));
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 8) — an attributed member sig (`[<CLIEvent>] abstract M`)
/// homes its attribute list on the `MemberSig` node (FCS's `SynValSig.attributes`),
/// read via `MemberSig::attributes()`. The attribute is *not* leaked as a separate
/// `SigDecl::Attributes` / bare sibling.
#[test]
fn sig_member_attributed_facade() {
    use crate::syntax::{AstNode, MemberDefn, SigDecl, SigFile, TypeDefnRepr};
    let source = "namespace N\ntype T =\n  [<CLIEvent>] abstract M : int\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
        panic!("the decl is a `Types` group");
    };
    let defn = t.defns().next().expect("one definition");
    let TypeDefnRepr::ObjectModel(o) = defn.repr().expect("an object-model repr") else {
        panic!("the repr is an object model");
    };
    let member = o.members().next().expect("one member sig");
    let MemberDefn::MemberSig(ms) = member else {
        panic!("the member is a `MemberSig`: {member:?}");
    };
    assert_eq!(
        ms.attributes().count(),
        1,
        "the member sig carries its one attribute list"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 8) — the same-line `OWITH` attributed-augment form
/// (`type T with [<A>] member …`) stays deferred: LexFilter emits an `OWITH …
/// OEND` block (distinct from the offside `Raw(with) OBLOCKBEGIN …`), which this
/// slice does not model. It is flagged and contained (no member leaked as a
/// sibling), not silently accepted.
#[test]
fn sig_member_attr_same_line_owith_augment_deferred() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "module M\ntype T with [<System.Obsolete>] member M : int\nval f : int\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "the same-line OWITH attributed augment must be flagged, not accepted"
    );
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    // The type (containing the deferred member) and the sibling `val` — no leak.
    assert_eq!(
        decls.len(),
        2,
        "type + sibling val, no leaked member: {decls:?}"
    );
    assert!(matches!(decls[0], SigDecl::Types(_)));
    assert!(matches!(decls[1], SigDecl::Val(_)));
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 8) — a *dangling* in-body attribute (its offside block
/// closes before the following spec, which is a dedented module-level sibling)
/// must NOT be claimed as an attributed member: the attribute-aware lookahead
/// works on the *filtered* stream, so a `BlockEnd` between the `[<…>]` run and the
/// next keyword stops it. Both the first-item form (`type T =`⏎`  [<A>]`⏎`val y`)
/// and the after-member form (`abstract M`⏎`  [<A>]`⏎`val y`) are malformed (FCS
/// rejects them); we flag the dangling attribute, keep `val y` as a genuine
/// top-level sibling (it sits at column 0), and never swallow it as a phantom
/// member of `T`. (Regression for a codex-review finding on the attribute
/// lookahead crossing block boundaries.)
#[test]
fn sig_member_dangling_attribute_does_not_swallow_sibling() {
    use crate::syntax::{AstNode, MemberDefn, SigDecl, SigFile, TypeDefnRepr};
    for source in [
        "namespace N\ntype T =\n  [<System.Obsolete>]\nval y : int\n",
        "namespace N\ntype T =\n  abstract M : int\n  [<System.Obsolete>]\nval y : int\n",
    ] {
        let parse = parse_sig(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?}: the dangling in-body attribute must be flagged, not accepted"
        );
        let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
        let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
        // `val y` survives as a genuine top-level sibling (col 0), not swallowed.
        let top_vals: Vec<String> = module
            .sig_decls()
            .filter_map(|d| match d {
                SigDecl::Val(v) => Some(
                    v.val_sig()
                        .and_then(|vs| vs.ident())
                        .map(|t| t.text().to_string())
                        .unwrap_or_default(),
                ),
                _ => None,
            })
            .collect();
        assert_eq!(
            top_vals,
            vec!["y".to_string()],
            "{source:?}: `val y` stays a top-level sibling"
        );
        // The dangling attribute is not swallowed as a member of `T`.
        if let Some(SigDecl::Types(t)) = module.sig_decls().next()
            && let Some(defn) = t.defns().next()
        {
            let outer = defn.members().count();
            let repr_members = match defn.repr() {
                Some(TypeDefnRepr::ObjectModel(o)) => o
                    .members()
                    .filter(|m| matches!(m, MemberDefn::MemberSig(_)))
                    .count(),
                _ => 0,
            };
            // The after-member form legitimately has `abstract M` as a member; the
            // first-item form has none. Either way `val y` is not among them.
            assert!(
                outer + repr_members <= 1,
                "{source:?}: the dangling attribute was swallowed as a member"
            );
        }
        assert_lossless(source, &parse);
    }
}

/// Phase 10.14 (slice 8) — a dangling attribute inside an explicit `class … end`
/// signature body (`type T = class`⏎`  [<A>]`⏎`end`) is malformed (FCS rejects it).
/// The member-block loop flags it rather than letting `parse_sig_kind_marked_repr`
/// reach `end` cleanly with no diagnostic. (Regression for a codex-review finding.)
#[test]
fn sig_member_dangling_attribute_in_class_end_is_flagged() {
    let source = "namespace N\ntype T = class\n  [<System.Obsolete>]\nend\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "a dangling attribute in a class…end body must be flagged, not accepted"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 8) — `static` is a two-token introducer prefix, so a bare
/// `[<A>] static` whose real head (`val`/`member`) sits across an `OBLOCKEND` (a
/// dedented sibling) must NOT be claimed: the attribute-aware lookahead consumes
/// `static` and requires its continuation in the *same* scope. This malformed
/// input is flagged (matching FCS, which rejects it), not silently accepted.
/// (Regression for a codex-review finding.)
#[test]
fn sig_member_attr_static_across_block_is_flagged() {
    let source = "namespace N\ntype T =\n  [<System.Obsolete>] static\nval y : int\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "a bare `static` crossing the block boundary must be flagged, not accepted"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 8) — an attributed `interface` member sig is deferred
/// (`interface` in member position is the `Virtual::InterfaceMember` relabel), but
/// the attribute-aware lookahead must still recognise it so a *later* valid member
/// after it is not skipped. Here `abstract A` and `abstract B` both survive, with
/// the deferred attributed `interface I` flagged and contained between them.
#[test]
fn sig_member_attributed_interface_does_not_drop_later_member() {
    use crate::syntax::{AstNode, MemberDefn, SigDecl, SigFile, TypeDefnRepr};
    let source = "namespace N\ntype T =\n  abstract A : int\n  [<System.Obsolete>] interface I\n  abstract B : int\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "the attributed interface (deferred) must be flagged"
    );
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
        panic!("the decl is a `Types` group");
    };
    let defn = t.defns().next().expect("one definition");
    let TypeDefnRepr::ObjectModel(o) = defn.repr().expect("an object-model repr") else {
        panic!("the repr is an object model");
    };
    let names: Vec<String> = o
        .members()
        .filter_map(|m| match m {
            MemberDefn::MemberSig(ms) => ms
                .val_sig()
                .and_then(|v| v.ident())
                .map(|t| t.text().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec!["A".to_string(), "B".to_string()],
        "both abstract members survive; the attributed interface does not drop `B`"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 5) — a *stray* top-level `and` (a continuation with no
/// preceding type definition, e.g. after a `val` spec) is malformed F# (FCS
/// rejects it and drops the continuation). The module-decl loop skips the `and`
/// plus its header + body as one ERROR, so a nested member spec (`and B =`⏎`  val
/// q`) stays *contained* rather than leaking as a phantom top-level
/// `SigDecl::Val`; the genuine `val x` survives. (Regression for a codex-review
/// finding: deleting the old `and` recovery when adding chains must not
/// reintroduce phantom exports on malformed input.)
#[test]
fn sig_stray_and_continuation_stays_contained() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "module M\nval x : int\nand B =\n  val q : int\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "a stray `and` continuation must be flagged, not silently accepted"
    );
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let top_vals: Vec<String> = module
        .sig_decls()
        .filter_map(|d| match d {
            SigDecl::Val(v) => Some(
                v.val_sig()
                    .and_then(|vs| vs.ident())
                    .map(|t| t.text().to_string())
                    .unwrap_or_default(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(
        top_vals,
        vec!["x".to_string()],
        "only the genuine `val x` is top-level; the stray continuation's `val q` \
         must not leak"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 3b) — a signature `interface` member sig
/// (`SynMemberSig.Interface`) has **no** member list: a trailing `with member …`
/// block (invalid in a `.fsi`) must NOT be consumed as impl-style interface
/// members (which would parse `= <expr>` bodies and build the wrong CST). The
/// interface clause parses (`members() == None`); the stray `with` block is left
/// to the trailing-body handling, which flags it. Pinned as no-panic + lossless +
/// the interface carries no `with`.
#[test]
fn sig_interface_member_rejects_with_block() {
    use crate::syntax::{AstNode, MemberDefn, SigDecl, SigFile, TypeDefnRepr};
    let source = "module M\ntype T =\n  interface IFoo with member M : int = ()\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "the impl-style `with` block on a sig interface is rejected, not accepted"
    );
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
        panic!("the decl is a `Types` group");
    };
    let defn = t.defns().next().expect("one definition");
    let TypeDefnRepr::ObjectModel(o) = defn.repr().expect("an object-model repr") else {
        panic!("the repr is an object model");
    };
    let iface = o
        .members()
        .find_map(|m| match m {
            MemberDefn::Interface(i) => Some(i),
            _ => None,
        })
        .expect("an interface member");
    assert!(
        !iface.has_with(),
        "the sig interface carries no `with` member list"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 3c) — explicit `class`/`struct`/`interface … end` signature
/// bodies project to an `OBJECT_MODEL_REPR` carrying the matching kind marker
/// (`is_class`/`is_struct`/`is_interface`) and the member sigs as children.
#[test]
fn sig_type_kind_marked_facade() {
    use crate::syntax::{AstNode, SigDecl, SigFile, TypeDefnRepr};
    let cases = [
        (
            "class",
            "namespace N\ntype T = class\n  abstract M : int\nend\n",
        ),
        (
            "struct",
            "namespace N\ntype T = struct\n  val x : int\nend\n",
        ),
        (
            "interface",
            "namespace N\ntype T = interface\n  abstract M : int\nend\n",
        ),
    ];
    for (want_kind, source) in cases {
        let parse = parse_sig(source);
        assert!(
            parse.errors.is_empty(),
            "errors for {source:?}: {:?}",
            parse.errors
        );
        let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
        let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
        let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
            panic!("the decl is a `Types` group: {source:?}");
        };
        let defn = t.defns().next().expect("one definition");
        let TypeDefnRepr::ObjectModel(om) = defn.repr().expect("an object-model repr") else {
            panic!("the repr is an object model: {source:?}");
        };
        let got_kind = if om.is_class() {
            "class"
        } else if om.is_struct() {
            "struct"
        } else if om.is_interface() {
            "interface"
        } else {
            "unspecified"
        };
        assert_eq!(got_kind, want_kind, "kind marker for {source:?}");
        assert_eq!(om.members().count(), 1, "one member sig for {source:?}");
        assert_lossless(source, &parse);
    }
}

/// Phase 10.14 (slice 3e) — a `new`-ctor member sig and a property `… with get, set`
/// clause both project to `MemberSig`. The `new`-ctor carries the synthetic name
/// `"new"` and the [`MemberSigLeading::New`] leading keyword (no `member`/`abstract`
/// keyword precedes it); the get/set property reads its name via the `VAL_SIG`
/// carrier as usual.
#[test]
fn sig_type_member_3e_facade() {
    use crate::syntax::{AstNode, MemberDefn, MemberSigLeading, SigDecl, SigFile, TypeDefnRepr};
    let source = "namespace N\ntype T =\n  new : unit -> T\n  member P : int with get, set\n  abstract Q : int with get\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
        panic!("the decl is a `Types` group");
    };
    let defn = t.defns().next().expect("one definition");
    let TypeDefnRepr::ObjectModel(o) = defn.repr().expect("an object-model repr") else {
        panic!("the repr is an object model");
    };
    let members: Vec<(MemberSigLeading, String)> = o
        .members()
        .map(|m| {
            let MemberDefn::MemberSig(ms) = m else {
                panic!("each member is a `MemberSig`: {m:?}");
            };
            let name = if ms.leading_keyword() == MemberSigLeading::New {
                "new".to_string()
            } else {
                ms.val_sig()
                    .and_then(|vs| vs.ident())
                    .map(|t| t.text().to_string())
                    .expect("a member name")
            };
            (ms.leading_keyword(), name)
        })
        .collect();
    assert_eq!(
        members,
        vec![
            (MemberSigLeading::New, "new".to_string()),
            (MemberSigLeading::Member, "P".to_string()),
            (MemberSigLeading::Abstract, "Q".to_string()),
        ],
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice 3e) — a `with get`/`set` accessor clause on a `new`-ctor sig
/// is invalid F# (FCS's constructor production is `NEW COLON topType`, no accessor
/// tail). We flag it with a diagnostic but still consume the clause so it is
/// captured inside the `MEMBER_SIG` (lossless) rather than leaking to the block
/// loop; the `new`-ctor member survives.
#[test]
fn sig_new_ctor_with_accessor_is_flagged() {
    use crate::syntax::{AstNode, MemberDefn, MemberSigLeading, SigDecl, SigFile, TypeDefnRepr};
    let source = "namespace N\ntype T = class\n  new : unit -> T with get\nend\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "a `new`-ctor with a get/set clause must be flagged"
    );
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
        panic!("the decl is a `Types` group");
    };
    let defn = t.defns().next().expect("one definition");
    let TypeDefnRepr::ObjectModel(o) = defn.repr().expect("an object-model repr") else {
        panic!("the repr is an object model");
    };
    let mut members = o.members();
    let MemberDefn::MemberSig(ms) = members.next().expect("a member") else {
        panic!("the member is a `MemberSig`");
    };
    assert_eq!(ms.leading_keyword(), MemberSigLeading::New);
    assert!(members.next().is_none(), "only the `new`-ctor member");
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice V1) — a name-position accessibility modifier on a member sig
/// (`member private M`) is consumed as an `ACCESS_TOK` inside the `MEMBER_SIG` (so
/// it is captured, not dropped), the member parses cleanly, and its name/leading
/// keyword are unaffected. On an *abstract* member the modifier is rejected
/// (`parsAccessibilityModsIllegalForAbstract`) — flagged with a diagnostic but
/// still captured, the abstract slot surviving.
#[test]
fn sig_member_access_facade() {
    use crate::syntax::{
        AstNode, MemberDefn, MemberSigLeading, SigDecl, SigFile, SyntaxKind, TypeDefnRepr,
    };

    let one_member = |source: &str| {
        let parse = parse_sig(source);
        let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
        let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
        let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
            panic!("the decl is a `Types` group: {source:?}");
        };
        let defn = t.defns().next().expect("one definition");
        let TypeDefnRepr::ObjectModel(o) = defn.repr().expect("an object-model repr") else {
            panic!("the repr is an object model: {source:?}");
        };
        let MemberDefn::MemberSig(ms) = o.members().next().expect("a member") else {
            panic!("the member is a `MemberSig`: {source:?}");
        };
        // The modifier lives in the `VAL_SIG` (FCS's `SynValSig.accessibility`),
        // matching the top-level `val` sig — so search descendants, not just the
        // `MEMBER_SIG`'s direct children.
        let has_access = ms
            .syntax()
            .descendants_with_tokens()
            .any(|c| c.kind() == SyntaxKind::ACCESS_TOK);
        let name = ms
            .val_sig()
            .and_then(|vs| vs.ident())
            .map(|t| t.text().to_string());
        (parse, ms.leading_keyword(), name, has_access)
    };

    // A concrete `private` member — clean parse, `ACCESS_TOK` captured.
    let source = "namespace N\ntype T =\n  member private M : int\n";
    let (parse, leading, name, has_access) = one_member(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(leading, MemberSigLeading::Member);
    assert_eq!(name.as_deref(), Some("M"));
    assert!(
        has_access,
        "the `private` modifier is captured as an ACCESS_TOK"
    );
    assert_lossless(source, &parse);

    // An `abstract` member rejects accessibility — flagged, but still captured and
    // the abstract slot survives.
    let source = "namespace N\ntype T =\n  abstract member private M : int\n";
    let (parse, leading, name, has_access) = one_member(source);
    assert!(
        !parse.errors.is_empty(),
        "accessibility on an abstract member must be flagged"
    );
    assert_eq!(leading, MemberSigLeading::AbstractMember);
    assert_eq!(name.as_deref(), Some("M"));
    assert!(
        has_access,
        "the rejected modifier is still captured (lossless)"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.14 (slice V2) — a *leading* accessibility modifier on a `new` ctor
/// (`private new : …`) is consumed as an `ACCESS_TOK` (a `MEMBER_SIG`-level token,
/// before the `NEW_TOK`), the ctor parses cleanly, and its synthetic name `"new"`
/// and `New` leading keyword are unaffected. Unlike a name-position modifier
/// (slice V1, inside the `VAL_SIG`), the `new` ctor's modifier is leading.
#[test]
fn sig_new_ctor_leading_access_facade() {
    use crate::syntax::{
        AstNode, MemberDefn, MemberSigLeading, SigDecl, SigFile, SyntaxKind, TypeDefnRepr,
    };
    let source = "namespace N\ntype T = class\n  private new : unit -> T\nend\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let SigDecl::Types(t) = module.sig_decls().next().expect("a Types decl") else {
        panic!("the decl is a `Types` group");
    };
    let defn = t.defns().next().expect("one definition");
    let TypeDefnRepr::ObjectModel(o) = defn.repr().expect("an object-model repr") else {
        panic!("the repr is an object model");
    };
    let MemberDefn::MemberSig(ms) = o.members().next().expect("a member") else {
        panic!("the member is a `MemberSig`");
    };
    assert_eq!(ms.leading_keyword(), MemberSigLeading::New);
    // The leading modifier is a direct `MEMBER_SIG` child (before the `new`
    // keyword), not inside the `VAL_SIG` as a name-position modifier would be.
    assert!(
        ms.syntax()
            .children_with_tokens()
            .any(|c| c.kind() == SyntaxKind::ACCESS_TOK),
        "the leading `private` is captured as a MEMBER_SIG-level ACCESS_TOK"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.13b — end-of-scope guard: a dangling attribute run at the end of a
/// nested sig module's body must NOT let the raw `module` lookahead cross the
/// body-closing `OBLOCKEND` and reparent an *outer* sibling `module`. Here the
/// `[<AutoOpen>]` ends `A`'s body and `module B` is a dedented sibling — `B` must
/// stay a top-level sibling of `A`, not a nested child. (FCS errors on the
/// dangling attr; we assert structure our-side, since the recovery shapes
/// differ.)
#[test]
fn sig_dangling_attr_does_not_reparent_sibling_module() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "module A =\n  open System\n  [<AutoOpen>]\nmodule B =\n  open System.IO\n";
    let parse = parse_sig(source);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let names: Vec<Vec<String>> = module
        .sig_decls()
        .filter_map(|d| match d {
            SigDecl::NestedModule(nm) => Some(
                nm.long_id()
                    .map(|li| li.idents().map(|t| t.text().to_string()).collect())
                    .unwrap_or_default(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![vec!["A".to_string()], vec!["B".to_string()]],
        "A and B are sibling nested modules; B is not reparented inside A",
    );
    assert_lossless(source, &parse);
}

/// Phase 10.13b — a no-`=` `module` head inside a nested sig body is *not* a
/// whole-file header (that is only valid at file scope). Here
/// `module Outer =`⏎`  [<AutoOpen>]`⏎`  module Inner`⏎`  open System`: the
/// `module Inner` (no `=`) must not be claimed as a `NamedModule` header — the
/// file must stay one `AnonModule` containing `Outer`, and the parse errors.
#[test]
fn sig_no_whole_file_header_inside_nested_body() {
    use crate::syntax::{AstNode, ModuleOrNamespaceKind, SigFile};
    let source = "module Outer =\n  [<AutoOpen>]\n  module Inner\n  open System\n";
    let parse = parse_sig(source);
    assert!(
        !parse.errors.is_empty(),
        "the no-`=` nested `module` errors"
    );
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let modules: Vec<_> = file.modules().collect();
    assert_eq!(modules.len(), 1, "one top-level segment");
    assert_eq!(
        modules[0].kind(),
        ModuleOrNamespaceKind::Anon,
        "the file is the implicit AnonModule, not a mis-claimed NamedModule",
    );
    assert_lossless(source, &parse);
}

/// Phase 10.13b — an indented `namespace` inside a nested sig module body must
/// not escape to the outer file loop (a `namespace` is only a segment boundary
/// at file scope). For `module M =`⏎`  namespace N`, the file stays a single
/// `AnonModule` (the `namespace` is errored *inside* M's body, not parsed as a
/// top-level segment), and the parse errors.
#[test]
fn sig_namespace_in_nested_body_does_not_escape() {
    use crate::syntax::{AstNode, ModuleOrNamespaceKind, SigFile};
    let source = "module M =\n  namespace N\n";
    let parse = parse_sig(source);
    assert!(!parse.errors.is_empty(), "a nested `namespace` errors");
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let modules: Vec<_> = file.modules().collect();
    assert_eq!(
        modules.len(),
        1,
        "one segment — the namespace did not escape"
    );
    assert_eq!(
        modules[0].kind(),
        ModuleOrNamespaceKind::Anon,
        "still the implicit AnonModule, not a top-level DeclaredNamespace",
    );
    assert_lossless(source, &parse);
}

/// Phase 10.13b — a nested sig body that is a single swallowed-keyword decl
/// (`module M =`⏎`  type T`, an opaque type) must NOT be misclassified as a
/// module abbreviation (`module M = T`): LexFilter swallows `type`, so the body
/// looks like the bare longident `T`. The raw-stream guard in
/// `body_is_module_abbrev` keeps it a `NestedModule` (whose `type` body is an
/// unsupported spec until phase 10.14, so the parse errors) — not a clean,
/// wrong `ModuleAbbrev`. Asserted our-side (full `type`-spec support is 10.14).
#[test]
fn sig_nested_type_body_is_not_an_abbreviation() {
    use crate::syntax::{AstNode, SigDecl, SigFile};
    let source = "module M =\n  type T\n";
    let parse = parse_sig(source);
    let file = SigFile::cast(parse.root.clone()).expect("a SIG_FILE root");
    let module = file.modules().next().expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.sig_decls().collect();
    assert!(
        matches!(decls.as_slice(), [SigDecl::NestedModule(_)]),
        "`module M =⏎  type T` is a nested module, not a ModuleAbbrev; got {decls:?}",
    );
    assert_lossless(source, &parse);
}

/// Two-decls case across a newline, but with bools. Mirrors
/// `two_int_literals_across_newline_is_two_decls` to confirm the
/// expression-start-set dispatch path applies to bool too.
#[test]
fn two_bool_literals_across_newline_is_two_decls() {
    let source = "true\nfalse\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..11
  MODULE_OR_NAMESPACE@0..11
    EXPR_DECL@0..4
      CONST_EXPR@0..4
        BOOL_LIT@0..4 \"true\"
    NEWLINE@4..5 \"\\n\"
    ERROR@5..5 \"\"
    EXPR_DECL@5..10
      CONST_EXPR@5..10
        BOOL_LIT@5..10 \"false\"
    NEWLINE@10..11 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `f x\ng y\n` — top-level app followed by a *new* top-level app
/// (separated by a `Virtual::BlockSep`). The first `parse_app_expr`
/// stops at the BlockSep (which is not in `peek_is_expr_start`),
/// the outer `parse_impl_file` loop bumps the BlockSep as an ERROR
/// placeholder, then the next iteration parses the second app.
/// Verifies the decl-boundary detection has moved from a separate
/// gate into `peek_is_expr_start`'s implicit handling of virtuals.
#[test]
fn two_apps_across_newline_are_two_decls() {
    let source = "f x\ng y\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 2, "expected two top-level decls");
    for d in &decls {
        let crate::syntax::ModuleDecl::Expr(decl) = d else {
            panic!("expected ModuleDecl::Expr")
        };
        assert!(
            matches!(decl.expr(), Some(crate::syntax::Expr::App(_))),
            "decl should be App, got {:?}",
            decl.expr(),
        );
    }
    assert_lossless(source, &parse);
}

/// A two-segment dotted path `Foo.Bar` is `SynExpr.LongIdent`. Shape:
/// `EXPR_DECL > LONG_IDENT_EXPR > LONG_IDENT > [IDENT, DOT, IDENT]`.
/// Single-segment paths take the optimised `SynExpr.Ident`
/// representation (covered by `lone_ident_expression`); the two-segment
/// case is the minimum that exercises the dot-loop.
#[test]
fn two_segment_long_ident() {
    let source = "Foo.Bar\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..8
  MODULE_OR_NAMESPACE@0..8
    EXPR_DECL@0..7
      LONG_IDENT_EXPR@0..7
        LONG_IDENT@0..7
          IDENT_TOK@0..3 \"Foo\"
          DOT_TOK@3..4 \".\"
          IDENT_TOK@4..7 \"Bar\"
    NEWLINE@7..8 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Three-segment path exercises the loop body more than once. Pins that
/// the dot-bump+ident-bump pair repeats correctly and no stray nodes
/// appear between segments.
#[test]
fn three_segment_long_ident() {
    let source = "Foo.Bar.Baz\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..12
  MODULE_OR_NAMESPACE@0..12
    EXPR_DECL@0..11
      LONG_IDENT_EXPR@0..11
        LONG_IDENT@0..11
          IDENT_TOK@0..3 \"Foo\"
          DOT_TOK@3..4 \".\"
          IDENT_TOK@4..7 \"Bar\"
          DOT_TOK@7..8 \".\"
          IDENT_TOK@8..11 \"Baz\"
    NEWLINE@11..12 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 8.1 — `open Foo.Bar` builds `OPEN_DECL > [OPEN_TOK, LONG_IDENT]`
/// with the inter-token whitespace draining out as a sibling of the
/// `LONG_IDENT` (which stays tight around the path). No trailing decl, so
/// the file body is the single Open.
#[test]
fn open_module_or_namespace_decl() {
    let source = "open Foo.Bar\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..13
  MODULE_OR_NAMESPACE@0..13
    OPEN_DECL@0..12
      OPEN_TOK@0..4 \"open\"
      WHITESPACE@4..5 \" \"
      LONG_IDENT@5..12
        IDENT_TOK@5..8 \"Foo\"
        DOT_TOK@8..9 \".\"
        IDENT_TOK@9..12 \"Bar\"
    NEWLINE@12..13 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// An `open` indented *inside* a union type's body is invalid F# (FCS's
/// FS0058). The lex-filter leaves the `open` inside the still-open type body
/// block (an `OBLOCKSEP`, then the raw `open`, *before* the body-close
/// `OBLOCKEND`), so `parse_type_defn_repr` can see it and flag it — a stray
/// `open` is not a member, so the member-block gate declines it.
#[test]
fn open_inside_union_type_body_is_error() {
    let source = "module Module\n\ntype A =\n    | A\n    open System\n";
    let parse = parse(source);
    let open_errs: Vec<_> = parse
        .errors
        .iter()
        .filter(|e| {
            e.message
                .contains("'open' declarations must appear at module level")
        })
        .collect();
    assert_eq!(
        open_errs.len(),
        1,
        "expected exactly one open-inside-type error, got: {:?}",
        parse.errors
    );
    // The span points at the offending `open` keyword.
    assert_eq!(&source[open_errs[0].span.clone()], "open");
    assert_lossless(source, &parse);
}

/// Same diagnostic for the object-model (member) body shape: `type A =⏎ member
/// … ⏎ open System`. Here the member-block loop itself breaks on the `open`
/// (the member's terminator has consumed the inter-item separators, leaving the
/// raw `open` at the cursor, still ahead of the body-close `OBLOCKEND`).
#[test]
fn open_inside_member_type_body_is_error() {
    let source = "module Module\n\ntype A =\n    member x.M = 1\n    open System\n";
    let parse = parse(source);
    let open_errs: Vec<_> = parse
        .errors
        .iter()
        .filter(|e| {
            e.message
                .contains("'open' declarations must appear at module level")
        })
        .collect();
    assert_eq!(
        open_errs.len(),
        1,
        "expected exactly one open-inside-type error, got: {:?}",
        parse.errors
    );
    assert_eq!(&source[open_errs[0].span.clone()], "open");
    assert_lossless(source, &parse);
}

/// Negative control: an `open` dedented to module level *after* a type is
/// valid. The lex-filter closes the type body (`OBLOCKEND`) *before* the
/// `open`, so it never looks like it is inside the type — no error.
#[test]
fn open_at_module_level_after_type_is_ok() {
    let source = "module Module\n\ntype A =\n    | A\nopen System\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
}

// ---- FS0058: nested type / module / exception inside a type body ----------
//
// FCS's `checkForInvalidDeclsInTypeDefn` (`LexFilter.fs:1392-1470`) reports
// FS0058 for a `type`/`module`/`exception` keyword indented *inside* a type
// definition (a later line at a greater column than the type, not in a
// member/augmentation context). Our lex-filter mirrors it (`pushes.rs`); the
// diagnostic is emitted as a recoverable error, gated on the F# 10
// `ErrorOnInvalidDeclsInTypeDefinitions` feature. (`open` is handled
// separately, parser-side — see the `open_inside_*` tests above.) Tested by
// diagnostic presence + span, like the `open` case: FCS recovers by hoisting
// the construct to module level, which our recovery does not reproduce, so the
// trees diverge and only the diagnostic is asserted.

/// `type` nested inside a type body → the FS0058 "Nested type definitions"
/// diagnostic, at the inner `type` keyword.
#[test]
fn nested_type_defn_in_type_body_is_error() {
    let source = "type Outer =\n    type Nested = int\n";
    let parse = parse(source);
    let errs: Vec<_> = parse
        .errors
        .iter()
        .filter(|e| {
            e.message
                .contains("Nested type definitions are not allowed")
        })
        .collect();
    assert_eq!(
        errs.len(),
        1,
        "expected one nested-type error: {:?}",
        parse.errors
    );
    assert_eq!(&source[errs[0].span.clone()], "type");
    assert_lossless(source, &parse);
}

/// `module` nested inside a type body → FS0058 "Modules cannot be nested".
#[test]
fn nested_module_in_type_body_is_error() {
    let source = "type Outer =\n    module M = begin end\n";
    let parse = parse(source);
    let errs: Vec<_> = parse
        .errors
        .iter()
        .filter(|e| e.message.contains("Modules cannot be nested inside types"))
        .collect();
    assert_eq!(
        errs.len(),
        1,
        "expected one nested-module error: {:?}",
        parse.errors
    );
    assert_eq!(&source[errs[0].span.clone()], "module");
    assert_lossless(source, &parse);
}

/// `exception` nested inside a type body → FS0058 "Exceptions must be defined
/// at module level".
#[test]
fn nested_exception_in_type_body_is_error() {
    let source = "type Outer =\n    exception E\n";
    let parse = parse(source);
    let errs: Vec<_> = parse
        .errors
        .iter()
        .filter(|e| {
            e.message
                .contains("Exceptions must be defined at module level")
        })
        .collect();
    assert_eq!(
        errs.len(),
        1,
        "expected one nested-exception error: {:?}",
        parse.errors
    );
    assert_eq!(&source[errs[0].span.clone()], "exception");
    assert_lossless(source, &parse);
}

/// Negative controls: the check fires only for a *later line, greater column*
/// nesting outside a member context. Same-line sequential type defns
/// (`type A = A type B = A`), a `type`/`exception` at module level, and a
/// `type` inside a `member` body must *not* be flagged. (`nested_fs58` matches
/// the three messages this stage emits.)
#[test]
fn nested_decl_fs58_negative_controls() {
    let nested_fs58 = |src: &str| {
        parse(src)
            .errors
            .into_iter()
            .filter(|e| {
                e.message
                    .contains("Nested type definitions are not allowed")
                    || e.message.contains("Modules cannot be nested inside types")
                    || e.message
                        .contains("Exceptions must be defined at module level")
            })
            .count()
    };
    // Same-line declarations are sequential, not nested.
    assert_eq!(nested_fs58("type A = A type B = A\n"), 0);
    // Module-level type / exception.
    assert_eq!(nested_fs58("module M\ntype T = int\nexception E\n"), 0);
    // A member body is a valid nesting context.
    assert_eq!(
        nested_fs58("type A =\n    member x.M =\n        let y = 1\n        y\n"),
        0
    );
}

/// The nested-decl checks are gated on the F# 10
/// `ErrorOnInvalidDeclsInTypeDefinitions` feature: at F# 9 they emit nothing
/// (FCS parses the same source with no diagnostic under `withLangVersion90`).
#[test]
fn nested_decl_fs58_gated_off_below_f10() {
    use std::collections::HashSet;
    let source = "type Outer =\n    type Nested = int\n";
    let symbols = HashSet::new();
    let parse = parse_with_options(
        source,
        ParseOptions {
            file_kind: FileKind::Impl,
            symbols: &symbols,
            lang: LanguageVersion::V9_0,
        },
    );
    assert!(
        !parse.errors.iter().any(|e| e
            .message
            .contains("Nested type definitions are not allowed")),
        "F# 9 must not emit the nested-type FS0058: {:?}",
        parse.errors
    );
}

/// Phase 8.2 — whole-file `module Foo` (no `=`) is a `NamedModule`: the
/// swallowed raw `module` is claimed as `MODULE_TOK`, the name lands in a
/// bare `LONG_IDENT`, and the body (`let x = 1`) flows in as the module's
/// decls. Pins the full green shape including the header-before-body
/// layout (the inter-keyword whitespace drains as a sibling).
#[test]
fn named_module_header_whole_file() {
    let source = "module Foo\nlet x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..21
  MODULE_OR_NAMESPACE@0..21
    MODULE_TOK@0..6 \"module\"
    WHITESPACE@6..7 \" \"
    LONG_IDENT@7..10
      IDENT_TOK@7..10 \"Foo\"
    NEWLINE@10..11 \"\\n\"
    ERROR@11..11 \"\"
    LET_DECL@11..20
      LET_TOK@11..14 \"let\"
      BINDING@14..20
        NAMED_PAT@14..16
          WHITESPACE@14..15 \" \"
          IDENT_TOK@15..16 \"x\"
        WHITESPACE@16..17 \" \"
        EQUALS_TOK@17..18 \"=\"
        WHITESPACE@18..19 \" \"
        ERROR@19..19 \"\"
        CONST_EXPR@19..20
          INT32_LIT@19..20 \"1\"
    NEWLINE@20..21 \"\\n\"
    ERROR@21..21 \"\"
    ERROR@21..21 \"\"
    ERROR@21..21 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 8.2 — a bare `module Foo` with no body still produces a
/// `NamedModule` whose decls list is empty. Confirms the header parser
/// composes with an empty body loop.
#[test]
fn named_module_header_empty_body() {
    use crate::syntax::{AstNode, ImplFile, ModuleOrNamespaceKind};
    let source = "module Foo\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::NamedModule);
    assert!(!module.is_rec());
    assert_eq!(module.decls().count(), 0, "empty body has no decls");
    let segs: Vec<String> = module
        .long_id()
        .expect("named module has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo"]);
    assert_lossless(source, &parse);
}

/// Phase 8.2 — dotted `module Foo.Bar.Baz` carries every segment in the
/// header `LONG_IDENT`.
#[test]
fn named_module_header_dotted_path() {
    use crate::syntax::{AstNode, ImplFile};
    let source = "module Foo.Bar.Baz\nlet x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let segs: Vec<String> = module
        .long_id()
        .expect("named module has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo", "Bar", "Baz"]);
    assert_lossless(source, &parse);
}

/// Phase 8.2 — `module rec Foo` sets `isRecursive`; the `rec` keyword sits
/// between `MODULE_TOK` and the name as a `REC_TOK`.
#[test]
fn named_module_header_rec() {
    use crate::syntax::{AstNode, ImplFile, ModuleOrNamespaceKind};
    let source = "module rec Foo\nlet x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::NamedModule);
    assert!(module.is_rec(), "module rec must set isRecursive");
    assert_lossless(source, &parse);
}

/// Phase 8.2 — `module internal Foo`: the access modifier is consumed as
/// `ACCESS_TOK` (kept out of ERROR) but elided by the normaliser. The
/// module is still a `NamedModule` named `Foo`.
#[test]
fn named_module_header_access_modifier() {
    use crate::syntax::{AstNode, ImplFile, ModuleOrNamespaceKind};
    let source = "module internal Foo\nlet x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        parse
            .root
            .descendants_with_tokens()
            .any(|el| el.kind() == SyntaxKind::ACCESS_TOK),
        "the access modifier must be claimed as ACCESS_TOK, not ERROR",
    );
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::NamedModule);
    assert!(!module.is_rec());
    assert_lossless(source, &parse);
}

/// Phase 8.2 — `namespace Foo` is a `DeclaredNamespace`. `namespace`
/// flows through as a real `NAMESPACE_TOK` (it is not swallowed), the
/// name lands in a `LONG_IDENT`, and a body `let` is accepted at parse
/// time (FCS rejects `let`-in-namespace only in a later semantic pass).
#[test]
fn namespace_header_declared() {
    use crate::syntax::{AstNode, ImplFile, ModuleOrNamespaceKind};
    let source = "namespace Foo\nlet x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::DeclaredNamespace);
    assert!(!module.is_rec());
    let segs: Vec<String> = module
        .long_id()
        .expect("namespace has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo"]);
    assert_lossless(source, &parse);
}

/// Phase 8.3 — a file with two `namespace` blocks yields **two**
/// `MODULE_OR_NAMESPACE`s from `ImplFile::modules()`, each its own
/// `DeclaredNamespace` carrying its name and its own body decl.
#[test]
fn two_namespaces_yield_two_modules() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, ModuleOrNamespaceKind};
    let source = "namespace A\nlet x = 1\nnamespace B\nlet y = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let modules: Vec<_> = ImplFile::cast(parse.root.clone())
        .expect("ImplFile")
        .modules()
        .collect();
    assert_eq!(modules.len(), 2, "two namespace segments");
    for (m, name) in modules.iter().zip(["A", "B"]) {
        assert_eq!(m.kind(), ModuleOrNamespaceKind::DeclaredNamespace);
        let segs: Vec<String> = m
            .long_id()
            .expect("namespace has a LONG_IDENT")
            .idents()
            .map(|t| t.text().to_string())
            .collect();
        assert_eq!(segs, vec![name]);
        // Each namespace owns exactly its one `let`.
        assert_eq!(
            m.decls()
                .filter(|d| matches!(d, ModuleDecl::Let(_)))
                .count(),
            1,
        );
    }
    assert_lossless(source, &parse);
}

/// Phase 8.2 — `namespace global` is a `GlobalNamespace`: the bare
/// `global` is emitted as `GLOBAL_TOK` (not a path), and the kind has no
/// `LONG_IDENT` child.
#[test]
fn namespace_header_global() {
    use crate::syntax::{AstNode, ImplFile, ModuleOrNamespaceKind};
    let source = "namespace global\nlet x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        parse
            .root
            .descendants_with_tokens()
            .any(|el| el.kind() == SyntaxKind::GLOBAL_TOK),
        "bare `global` must be a GLOBAL_TOK",
    );
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::GlobalNamespace);
    assert!(
        module.long_id().is_none(),
        "GlobalNamespace carries no path",
    );
    assert_lossless(source, &parse);
}

/// Phase 8.2 — `namespace rec A.B` sets `isRecursive` and carries the
/// dotted path.
#[test]
fn namespace_header_rec_dotted() {
    use crate::syntax::{AstNode, ImplFile, ModuleOrNamespaceKind};
    let source = "namespace rec A.B\nlet x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::DeclaredNamespace);
    assert!(module.is_rec());
    let segs: Vec<String> = module
        .long_id()
        .expect("namespace has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["A", "B"]);
    assert_lossless(source, &parse);
}

/// Phase 8.4 — a nested `module Foo = …` (with `=`) keeps the *file* an
/// `AnonModule` (the `=` lookahead in `raw_leading_named_module` vetoes the
/// whole-file `NamedModule` header) but its sole decl is now a
/// `NESTED_MODULE_DECL`: the swallowed `module` is claimed as `MODULE_TOK`,
/// the name lands in a `LONG_IDENT`, and the offside body flows in as the
/// nested module's decls. Pins the full green shape — including the trailing
/// `OBLOCKEND`/`ODECLEND`/`OBLOCKEND` virtuals kept as zero-width `ERROR`s.
#[test]
fn nested_module_green_shape() {
    let source = "module Foo =\n    let x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..27
  MODULE_OR_NAMESPACE@0..27
    NESTED_MODULE_DECL@0..27
      MODULE_TOK@0..6 \"module\"
      WHITESPACE@6..7 \" \"
      LONG_IDENT@7..10
        IDENT_TOK@7..10 \"Foo\"
      WHITESPACE@10..11 \" \"
      EQUALS_TOK@11..12 \"=\"
      NEWLINE@12..13 \"\\n\"
      WHITESPACE@13..17 \"    \"
      ERROR@17..17 \"\"
      LET_DECL@17..26
        LET_TOK@17..20 \"let\"
        BINDING@20..26
          NAMED_PAT@20..22
            WHITESPACE@20..21 \" \"
            IDENT_TOK@21..22 \"x\"
          WHITESPACE@22..23 \" \"
          EQUALS_TOK@23..24 \"=\"
          WHITESPACE@24..25 \" \"
          ERROR@25..25 \"\"
          CONST_EXPR@25..26
            INT32_LIT@25..26 \"1\"
      NEWLINE@26..27 \"\\n\"
      ERROR@27..27 \"\"
      ERROR@27..27 \"\"
      ERROR@27..27 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// A `class … end` explicit-kind type followed by a *sibling* `let` in the same
/// nested module: the `let` must stay **inside** the module. The explicit-`end`
/// form emits an extra `OBLOCKSEP` (the indentation before `end`) inside the
/// type-definition block, ahead of that block's closing `OBLOCKEND`; if the
/// parser doesn't skip the separator it leaks the type-defn block's `OBLOCKEND`
/// to the module loop, which misreads it as the *module body's* close and pops
/// the `let` out a level. (Regression: corpus `PropSetAfterConstrn*` cluster.)
#[test]
fn class_end_type_then_sibling_let_stays_in_module() {
    let source = "module M =\n    type S =\n        class\n            val mutable x : int\n        end\n    let y = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::NESTED_MODULE_DECL)
        .expect("a NESTED_MODULE_DECL");
    assert!(
        module_node
            .descendants()
            .any(|n| n.kind() == SyntaxKind::LET_DECL),
        "the sibling `let` must be nested inside the module, not popped out:\n{}",
        debug_tree(&parse.root)
    );
    assert_lossless(source, &parse);
}

/// Same for `struct … end` (no inner block-begin, but the same trailing
/// `OBLOCKSEP` before the type-defn block close).
#[test]
fn struct_end_type_then_sibling_let_stays_in_module() {
    let source = "module M =\n    type S =\n        struct\n            val mutable x : int\n        end\n    let y = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::NESTED_MODULE_DECL)
        .expect("a NESTED_MODULE_DECL");
    assert!(
        module_node
            .descendants()
            .any(|n| n.kind() == SyntaxKind::LET_DECL),
        "the sibling `let` must be nested inside the module:\n{}",
        debug_tree(&parse.root)
    );
    assert_lossless(source, &parse);
}

/// A `class … end` type followed by an `and`-chained type: the explicit-`end`
/// form must report its block as *closed* so the `and` continuation is taken
/// (two `TYPE_DEFN`s in one `TYPE_DEFNS`), rather than leaving the `and` stray.
#[test]
fn class_end_type_then_and_chain() {
    let source = "module M =\n    type S =\n        class\n            val mutable x : int\n        end\n    and T = int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let type_defns = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TYPE_DEFNS)
        .expect("a TYPE_DEFNS");
    let n_defns = type_defns
        .children()
        .filter(|n| n.kind() == SyntaxKind::TYPE_DEFN)
        .count();
    assert_eq!(
        n_defns,
        2,
        "the `and` continuation must join the chain (2 TYPE_DEFNs):\n{}",
        debug_tree(&parse.root)
    );
    assert_lossless(source, &parse);
}

/// The explicit-`end` separator-skip must stay gated on an *opened* body block.
/// In the column-0 offside-attribute regime (`type [<A>]⏎T = class … end`) the
/// body is blockless (`=` opens no `OBLOCKBEGIN`), so the `OBLOCKSEP` after `end`
/// is the *module* declaration separator before the sibling `let`, not the
/// type-defn block's internal separator. Skipping it there would swallow the
/// separator and make the module loop reject the `let`. FCS parses this form
/// cleanly. (Regression guard for the `class…end` sibling-decl fix.)
#[test]
fn column0_attributed_class_end_then_sibling_let() {
    let source = "type [<System.Obsolete>]\nT = class\n    member _.M = 1\n    end\nlet y = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module_node = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::MODULE_OR_NAMESPACE)
        .expect("a MODULE_OR_NAMESPACE");
    assert!(
        module_node
            .descendants()
            .any(|n| n.kind() == SyntaxKind::LET_DECL),
        "the sibling `let` must parse as a top-level decl:\n{}",
        debug_tree(&parse.root)
    );
    assert_lossless(source, &parse);
}

/// Phase 8.4 — facade smoke test: the file is an `AnonModule` whose single
/// decl is a `NestedModule` named `Foo`, carrying one inner decl, not
/// recursive.
#[test]
fn nested_module_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, ModuleOrNamespaceKind};
    let source = "module Foo =\n    let x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::Anon);
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1, "AnonModule holds exactly one decl");
    let ModuleDecl::NestedModule(nm) = &decls[0] else {
        panic!("expected a NestedModule decl, got {:?}", decls[0]);
    };
    assert!(!nm.is_rec());
    let segs: Vec<String> = nm
        .long_id()
        .expect("nested module has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo"]);
    assert!(
        matches!(nm.decls().next(), Some(ModuleDecl::Let(_))),
        "nested module body holds the `let`",
    );
    assert_lossless(source, &parse);
}

/// Phase 8.4 — `module rec Inner =` sets the nested module's `isRecursive`
/// (the `rec` keyword sits between `MODULE_TOK` and the name as a `REC_TOK`).
#[test]
fn nested_module_rec_sets_is_rec() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "module rec Inner =\n    let x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::NestedModule(nm) = module.decls().next().expect("one decl") else {
        panic!("expected a NestedModule decl");
    };
    assert!(nm.is_rec(), "`module rec` must set isRecursive");
    assert_lossless(source, &parse);
}

/// Phase 8.4 — a multi-`let` body: the body loop walks both bindings and
/// terminates at the body's lone `OBLOCKEND` rather than at the first
/// binding's `OBLOCKEND·ODECLEND` pair. Guards the adjacency terminator.
#[test]
fn nested_module_multi_decl_body_terminates() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "module Foo =\n    let x = 1\n    let y = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    // The two `let`s stay *inside* the nested module (one AnonModule decl).
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(
        decls.len(),
        1,
        "the file's AnonModule holds one nested module"
    );
    let ModuleDecl::NestedModule(nm) = &decls[0] else {
        panic!("expected a NestedModule decl");
    };
    assert_eq!(
        nm.decls()
            .filter(|d| matches!(d, ModuleDecl::Let(_)))
            .count(),
        2,
        "both `let`s land inside the nested module",
    );
    assert_lossless(source, &parse);
}

/// Phase 8.4 — a doubly-nested `module A =\n module B =\n  let x = 1`. The
/// closing run of three adjacent `OBLOCKEND`s unwinds to the right nesting:
/// `A` holds `B`, `B` holds the `let`.
#[test]
fn nested_module_doubly_nested_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "module A =\n    module B =\n        let x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::NestedModule(a) = module.decls().next().expect("module A") else {
        panic!("expected nested module A");
    };
    let ModuleDecl::NestedModule(b) = a.decls().next().expect("module B inside A") else {
        panic!("expected nested module B inside A");
    };
    assert!(matches!(b.decls().next(), Some(ModuleDecl::Let(_))));
    assert_lossless(source, &parse);
}

/// Phase 8.5 — `module Foo = Bar.Baz` is a `MODULE_ABBREV_DECL`: the LHS name
/// is the first `LONG_IDENT`, the abbreviated path the second (parsed as a bare
/// path, mirroring FCS's `ident: Ident * longId: LongIdent`). Pins the full
/// green shape including the `OBLOCKBEGIN`/`OBLOCKEND` zero-width `ERROR`s.
#[test]
fn module_abbrev_green_shape() {
    let source = "module Foo = Bar.Baz\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..21
  MODULE_OR_NAMESPACE@0..21
    MODULE_ABBREV_DECL@0..20
      MODULE_TOK@0..6 \"module\"
      WHITESPACE@6..7 \" \"
      LONG_IDENT@7..10
        IDENT_TOK@7..10 \"Foo\"
      WHITESPACE@10..11 \" \"
      EQUALS_TOK@11..12 \"=\"
      WHITESPACE@12..13 \" \"
      ERROR@13..13 \"\"
      LONG_IDENT@13..20
        IDENT_TOK@13..16 \"Bar\"
        DOT_TOK@16..17 \".\"
        IDENT_TOK@17..20 \"Baz\"
      ERROR@20..20 \"\"
    NEWLINE@20..21 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 8.5 — facade: the file's `AnonModule` holds one `ModuleAbbrev` decl
/// with `ident = "Foo"` and `long_id = ["Bar", "Baz"]`.
#[test]
fn module_abbrev_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "module Foo = Bar.Baz\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1);
    let ModuleDecl::ModuleAbbrev(a) = &decls[0] else {
        panic!("expected a ModuleAbbrev decl, got {:?}", decls[0]);
    };
    let segs = |li: Option<crate::syntax::LongIdent>| -> Vec<String> {
        li.expect("a LONG_IDENT")
            .idents()
            .map(|t| t.text().to_string())
            .collect()
    };
    assert_eq!(segs(a.ident()), vec!["Foo"]);
    assert_eq!(segs(a.long_id()), vec!["Bar", "Baz"]);
    assert_lossless(source, &parse);
}

/// Phase 8.5 — the three *invalid* abbreviation forms (a dotted LHS, `rec`, an
/// access modifier) each record an error and produce **no** decl (an `ERROR`
/// node, not a `MODULE_ABBREV_DECL`), matching FCS, which emits a diagnostic
/// and an empty module. The tree stays lossless.
#[test]
fn module_abbrev_invalid_forms_are_clean_errors() {
    use crate::syntax::{AstNode, ImplFile};
    for source in [
        "module X.Y = Z\n",
        "module rec Foo = Bar\n",
        "module internal Foo = Bar\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "invalid abbreviation must error: {source:?}",
        );
        assert!(
            !parse
                .root
                .descendants()
                .any(|n| n.kind() == SyntaxKind::MODULE_ABBREV_DECL),
            "invalid abbreviation must NOT be a MODULE_ABBREV_DECL: {source:?}",
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        assert_eq!(
            module.decls().count(),
            0,
            "invalid abbreviation must produce no decl: {source:?}",
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 8.2 — a `global`-headed module (`module global` or
/// `module global.Foo`) is NOT a valid named module: FCS emits *no*
/// `SynModuleOrNamespace` for either (bare → FS0244 "Invalid module or
/// namespace name"; dotted → FS0010, because `CtxtModuleHead` ends the
/// head at `global` and the `.Foo` dangles — verified via `fcs-dump`). We
/// mirror the rejection by not claiming the swallowed `module` as a
/// `MODULE_TOK` header, so the file errors out rather than synthesising a
/// bogus `NamedModule [global; …]` (what naively accepting `global` as a
/// path head would produce — a divergence, since FCS produces none). Both
/// sides error; the tree still round-trips losslessly. Guards against
/// "fixing" `raw_leading_named_module` to admit a `global` module head.
/// (`namespace global` — where `global` *is* meaningful — is handled
/// separately as a `GlobalNamespace`.)
#[test]
fn module_global_head_is_not_a_named_module() {
    for source in [
        "module global.Foo\nlet x = 1\n",
        "module global\nlet x = 1\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "a `global`-headed module must error (FCS emits no module): {source:?}",
        );
        assert!(
            !parse
                .root
                .descendants_with_tokens()
                .any(|el| el.kind() == SyntaxKind::MODULE_TOK),
            "the `global`-headed `module` must not be claimed as a NamedModule header: {source:?}",
        );
        assert_lossless(source, &parse);
    }
}

/// A trailing dot `Foo.\n` is a Phase 2 parse error — FCS supports
/// trailing-dot recovery for IntelliSense ("complete me a member here")
/// but we don't model that yet. The tree still round-trips losslessly:
/// the dot lands inside the LONG_IDENT but no second ident segment
/// follows.
#[test]
fn trailing_dot_long_ident_is_error() {
    let source = "Foo.\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("trailing dot")),
        "errors: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// A backticked segment can appear anywhere in the path. Pins that the
/// dot-loop accepts `QuotedIdent` for trailing segments, not just
/// `Ident`. FCS's `Ident.idText` strips backticks; the green tree keeps
/// them.
#[test]
fn long_ident_with_backticked_segment() {
    let source = "Foo.``bar baz``\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..16
  MODULE_OR_NAMESPACE@0..16
    EXPR_DECL@0..15
      LONG_IDENT_EXPR@0..15
        LONG_IDENT@0..15
          IDENT_TOK@0..3 \"Foo\"
          DOT_TOK@3..4 \".\"
          IDENT_TOK@4..15 \"``bar baz``\"
    NEWLINE@15..16 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Two top-level identifiers across a newline are two top-level decls.
/// Mirrors `two_int_literals_across_newline_is_two_decls` and
/// `two_bool_literals_across_newline_is_two_decls`; pins that the
/// expression-start-set dispatch path applies to idents too.
#[test]
fn two_idents_across_newline_is_two_decls() {
    let source = "x\ny\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..4
  MODULE_OR_NAMESPACE@0..4
    EXPR_DECL@0..1
      IDENT_EXPR@0..1
        IDENT_TOK@0..1 \"x\"
    NEWLINE@1..2 \"\\n\"
    ERROR@2..2 \"\"
    EXPR_DECL@2..3
      IDENT_EXPR@2..3
        IDENT_TOK@2..3 \"y\"
    NEWLINE@3..4 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Leading and trailing trivia must survive into the green tree with
/// the right source offsets — and leading trivia must land as a
/// *sibling* of the decl, not a child. LSP range queries walk
/// ancestors-of-a-token; if `// c\n` or leading whitespace lived
/// inside `EXPR_DECL`/`CONST_EXPR`, the trivia would report the
/// declaration as its enclosing scope.
#[test]
fn trivia_surrounding_literal_preserved() {
    let source = "  42\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..5
  MODULE_OR_NAMESPACE@0..5
    WHITESPACE@0..2 \"  \"
    EXPR_DECL@2..4
      CONST_EXPR@2..4
        INT32_LIT@2..4 \"42\"
    NEWLINE@4..5 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Line comments are trivia too: they should appear in the tree, and
/// the literal's source offset should reflect everything skipped.
/// Additionally, the comment must land as a sibling of `EXPR_DECL`, not
/// inside it — so an LSP "find enclosing declaration" ancestor walk
/// starting from the comment finds only the module, not the decl.
#[test]
fn line_comment_before_literal_preserved() {
    let source = "// hi\n42\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let mut int_tok = None;
    let mut comment_tok = None;
    for el in parse.root.descendants_with_tokens() {
        if let rowan::NodeOrToken::Token(t) = el {
            match t.kind() {
                SyntaxKind::INT32_LIT => int_tok = Some(t),
                SyntaxKind::LINE_COMMENT => comment_tok = Some(t),
                _ => {}
            }
        }
    }
    let int_tok = int_tok.expect("INT32_LIT token");
    let comment_tok = comment_tok.expect("LINE_COMMENT token");

    let r = int_tok.text_range();
    assert_eq!(
        (u32::from(r.start()), u32::from(r.end())),
        (6, 8),
        "literal range should reflect skipped trivia",
    );

    // The comment is leading trivia: an ancestors walk must NOT find
    // EXPR_DECL/CONST_EXPR. That's the LSP-fidelity property the
    // pre-`start_node` drain restores.
    let ancestor_kinds: Vec<_> = comment_tok.parent_ancestors().map(|n| n.kind()).collect();
    assert!(
        !ancestor_kinds.contains(&SyntaxKind::EXPR_DECL)
            && !ancestor_kinds.contains(&SyntaxKind::CONST_EXPR),
        "leading line comment should not be inside the expression decl; ancestors: {:?}",
        ancestor_kinds,
    );
}

/// Same two literals across a newline ARE two top-level decls in
/// Phase 1's grammar. LexFilter inserts a `Virtual::BlockSep` between
/// them; we surface it as a zero-width ERROR placeholder so the tree
/// stays a faithful witness of what the filter produced — Phase 1
/// doesn't yet model the SeqBlock semantics that would consume it.
#[test]
fn two_int_literals_across_newline_is_two_decls() {
    let source = "42\n43\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..6
  MODULE_OR_NAMESPACE@0..6
    EXPR_DECL@0..2
      CONST_EXPR@0..2
        INT32_LIT@0..2 \"42\"
    NEWLINE@2..3 \"\\n\"
    ERROR@3..3 \"\"
    EXPR_DECL@3..5
      CONST_EXPR@3..5
        INT32_LIT@3..5 \"43\"
    NEWLINE@5..6 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// LexFilter splits a single `Op(">>")` raw token into two filtered
/// `Greater` tokens when it sits at the close of nested generic
/// applications (`typars_close_op_split`). The first split piece
/// covers only the front half of the raw span, so the raw cursor
/// must NOT advance after it — otherwise the second piece's text
/// would be drained as ERROR while the parser also emitted it.
/// Phase 1 doesn't model generics, so every token lands as ERROR;
/// the test pins lossless round-trip, which is what would break
/// under a too-aggressive raw advance.
#[test]
fn split_close_typars_is_lossless() {
    let source = "Foo<Bar<int>>";
    let parse = parse(source);
    assert_lossless(source, &parse);
}

/// LexFilter splits a `RQuoteBarRBrace` raw token (`@>|}`) into an
/// FCS-faithful *overlapping* pair: `RQUOTE=[start, end-2)` and
/// `BAR_RBRACE=[start+1, end)` — both cover the `>` byte. Without the
/// `raw_consumed_end` clamp, the parser would re-emit that byte and
/// `text(tree) == source` would fail. Phase 1 doesn't parse quotation
/// or anonymous-record syntax; this test only pins lossless round-trip.
#[test]
fn split_overlapping_rquote_barrbrace_is_lossless() {
    let source = "{| F = <@ 1 @>|}";
    let parse = parse(source);
    assert_lossless(source, &parse);
}

/// A top-level `do e` is a valid `SynModuleDecl.Expr(SynExpr.Do(e, _), _)`
/// (`hardwhiteDoBinding` → `SynExpr.Do`, `pars.fsy:4211`). `do 1` parses
/// cleanly into a `DO_EXPR` (the body is a type error at *check* time, not a
/// parse error — FCS reports no parse diagnostic either). This replaces the
/// former negative guard from when top-level `do` was unimplemented.
#[test]
fn do_int_parses_cleanly() {
    let source = "do 1\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "top-level `do` should parse cleanly, got: {:?}",
        parse.errors,
    );
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::DO_EXPR),
        "expected a DO_EXPR for `do 1`",
    );
    assert_lossless(source, &parse);
}

/// CRLF line endings produce a raw `Newline` token at `[2..4)` and a
/// `Virtual::BlockSep` at `[3..4)` — the virtual's span sits *inside*
/// the raw. Draining only to `span.start` (3) wouldn't flush the `\r\n`
/// and the zero-width BlockSep would land before the newline in the
/// green tree, breaking source-order/range fidelity. The virtual must
/// appear after the NEWLINE token.
#[test]
fn crlf_blocksep_lands_after_newline() {
    let source = "42\r\n43\r\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    // Collect tokens in tree order and check NEWLINE comes *before* the
    // ERROR (the BlockSep). Looking only at the kinds between the two
    // INT32_LITs is enough to detect the regression.
    let tokens: Vec<_> = parse
        .root
        .descendants_with_tokens()
        .filter_map(|el| match el {
            rowan::NodeOrToken::Token(t) => Some(t.kind()),
            _ => None,
        })
        .collect();
    let between: Vec<_> = tokens
        .iter()
        .skip_while(|&&k| k != SyntaxKind::INT32_LIT)
        .skip(1)
        .take_while(|&&k| k != SyntaxKind::INT32_LIT)
        .copied()
        .collect();
    let newline_idx = between
        .iter()
        .position(|&k| k == SyntaxKind::NEWLINE)
        .expect("NEWLINE between INT32_LITs");
    let error_idx = between
        .iter()
        .position(|&k| k == SyntaxKind::ERROR)
        .expect("ERROR (BlockSep) between INT32_LITs");
    assert!(
        newline_idx < error_idx,
        "Virtual BlockSep (ERROR) must sit *after* the CRLF newline; \
             got between-tokens {between:?}",
    );
}

// ---- Phase 9.1: type abbreviations -------------------------------------

/// `TYPE_DEFNS > TYPE_DEFN`: the swallowed `type` is claimed as `TYPE_TOK`, the
/// name lands in a `LONG_IDENT`, and the abbreviation RHS is wrapped in a
/// `TYPE_ABBREV` repr node. Pins the full green shape — including the
/// `OBLOCKBEGIN`/`OBLOCKEND` virtuals kept as zero-width `ERROR`s.
#[test]
fn type_abbrev_green_shape() {
    let source = "type T = int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..13
  MODULE_OR_NAMESPACE@0..13
    TYPE_DEFNS@0..12
      TYPE_DEFN@0..12
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        WHITESPACE@8..9 \" \"
        ERROR@9..9 \"\"
        TYPE_ABBREV@9..12
          LONG_IDENT_TYPE@9..12
            LONG_IDENT@9..12
              IDENT_TOK@9..12 \"int\"
        ERROR@12..12 \"\"
    NEWLINE@12..13 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.1 — facade smoke test: the file is an `AnonModule` whose single decl
/// is a `Types` group carrying one `TypeDefn` named `T` whose repr is an
/// abbreviation of `int`.
#[test]
fn type_abbrev_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, ModuleOrNamespaceKind, Type, TypeDefnRepr};
    let source = "type T = int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::Anon);
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1, "AnonModule holds exactly one decl");
    let ModuleDecl::Types(t) = &decls[0] else {
        panic!("expected a Types decl, got {:?}", decls[0]);
    };
    let defns: Vec<_> = t.defns().collect();
    assert_eq!(defns.len(), 1, "one type definition in the group");
    let segs: Vec<String> = defns[0]
        .long_id()
        .expect("type defn has a LONG_IDENT name")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["T"]);
    let Some(TypeDefnRepr::Abbrev(abbrev)) = defns[0].repr() else {
        panic!("expected an Abbrev repr, got {:?}", defns[0].repr());
    };
    assert!(
        matches!(abbrev.ty(), Some(Type::LongIdent(_))),
        "the abbreviation RHS is the long-ident type `int`",
    );
    assert_lossless(source, &parse);
}

/// A **bodyless** type definition — `[<Measure>] type m` (no `=`, no body).
/// FCS's `tyconDefn` reduces to its bare `typeNameInfo` alternative, yielding a
/// `SynTypeDefnSimpleRepr.None` repr with **no parse error**; the `Measure`
/// attribute is an ordinary type-header attribute (the bodyless-vs-required
/// distinction is a type-checker concern, not a parser one). The green
/// `TYPE_DEFN` carries the attribute list, the `type` keyword and the name, and
/// **no repr node** and no `EQUALS_TOK`. Before this the parser demanded an `=`
/// and flagged "expected `=` in type definition".
#[test]
fn type_bodyless_measure_green_shape() {
    let source = "[<Measure>] type m\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..19
  MODULE_OR_NAMESPACE@0..19
    TYPE_DEFNS@0..18
      TYPE_DEFN@0..18
        ATTRIBUTE_LIST@0..11
          LBRACK_LESS_TOK@0..2 \"[<\"
          ATTRIBUTE@2..9
            LONG_IDENT@2..9
              IDENT_TOK@2..9 \"Measure\"
          GREATER_RBRACK_TOK@9..11 \">]\"
        WHITESPACE@11..12 \" \"
        TYPE_TOK@12..16 \"type\"
        WHITESPACE@16..17 \" \"
        LONG_IDENT@17..18
          IDENT_TOK@17..18 \"m\"
    NEWLINE@18..19 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Facade — a bare bodyless `type Foo` (no attribute) is a `Types` group with
/// one `TypeDefn` named `Foo` whose `repr()` is `None` (the
/// `SynTypeDefnSimpleRepr.None` form). No errors.
#[test]
fn type_bodyless_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "type Foo\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defns: Vec<_> = t.defns().collect();
    assert_eq!(defns.len(), 1, "one type definition in the group");
    let segs: Vec<String> = defns[0]
        .long_id()
        .expect("type defn has a LONG_IDENT name")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo"]);
    assert!(
        defns[0].repr().is_none(),
        "a bodyless type has no repr node, got {:?}",
        defns[0].repr(),
    );
    assert_lossless(source, &parse);
}

/// A bodyless type with a primary constructor but no `=` — `type C(x)`. FCS's
/// `recover` alternative accepts this without error: the repr stays
/// `SynTypeDefnSimpleRepr.None` and the constructor is parsed (it lands in the
/// outer members slot on FCS's side). We keep the `IMPLICIT_CTOR` green child
/// and emit no repr node and no "only class types may take value arguments"
/// diagnostic (that fires only on the `= <non-class-repr>` path).
#[test]
fn type_bodyless_ctor_no_error() {
    let source = "type C(x)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let type_defn = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TYPE_DEFN)
        .expect("a TYPE_DEFN node");
    assert!(
        type_defn
            .children()
            .any(|n| n.kind() == SyntaxKind::IMPLICIT_CTOR),
        "the primary constructor is kept as an IMPLICIT_CTOR child",
    );
    assert!(
        !type_defn.children().any(|n| {
            use crate::syntax::{AstNode, TypeDefnRepr};
            TypeDefnRepr::cast(n).is_some()
        }),
        "a bodyless type emits no repr node",
    );
    assert_lossless(source, &parse);
}

/// The canonical delegate body — `type T = delegate of int -> int`. Pins the
/// exact green shape: a `DELEGATE_REPR` holding `[DELEGATE_TOK, OF_TOK,
/// FUN_TYPE]`, no errors. The differential test covers the normalised AST; this
/// guards the surface tree the LSP serves.
#[test]
fn delegate_basic_green_shape() {
    let source = "type T = delegate of int -> int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..32
  MODULE_OR_NAMESPACE@0..32
    TYPE_DEFNS@0..31
      TYPE_DEFN@0..31
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        WHITESPACE@8..9 \" \"
        ERROR@9..9 \"\"
        DELEGATE_REPR@9..31
          DELEGATE_TOK@9..17 \"delegate\"
          WHITESPACE@17..18 \" \"
          OF_TOK@18..20 \"of\"
          WHITESPACE@20..21 \" \"
          FUN_TYPE@21..31
            LONG_IDENT_TYPE@21..24
              LONG_IDENT@21..24
                IDENT_TOK@21..24 \"int\"
            WHITESPACE@24..25 \" \"
            RARROW_TOK@25..27 \"->\"
            WHITESPACE@27..28 \" \"
            LONG_IDENT_TYPE@28..31
              LONG_IDENT@28..31
                IDENT_TOK@28..31 \"int\"
        ERROR@31..31 \"\"
    NEWLINE@31..32 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// `type T = delegate` with no `of` — a malformed delegate body. We still emit a
/// `DELEGATE_REPR` (carrying the `delegate` keyword) for recovery and record an
/// "expected `of`" error; the tree stays lossless.
#[test]
fn delegate_missing_of_errors() {
    let source = "type T = delegate\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("expected `of`")),
        "missing `of` is flagged: {:?}",
        parse.errors,
    );
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::DELEGATE_REPR),
        "the partial delegate is still kept as a DELEGATE_REPR",
    );
    assert_lossless(source, &parse);
}

/// A primary constructor on a delegate — `type D(x) = delegate of int -> int`.
/// FCS folds the ctor into the delegate's `ObjectModel` members with **no**
/// error (delegate is an object-model repr), so we must *not* emit the "Only
/// class types may take value arguments" diagnostic. The ctor stays as an
/// `IMPLICIT_CTOR` child alongside the `DELEGATE_REPR`.
#[test]
fn delegate_with_primary_ctor_no_error() {
    let source = "type D(x) = delegate of int -> int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let type_defn = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TYPE_DEFN)
        .expect("a TYPE_DEFN node");
    assert!(
        type_defn
            .children()
            .any(|n| n.kind() == SyntaxKind::IMPLICIT_CTOR),
        "the primary constructor is kept as an IMPLICIT_CTOR child",
    );
    assert!(
        type_defn
            .children()
            .any(|n| n.kind() == SyntaxKind::DELEGATE_REPR),
        "the delegate body is a DELEGATE_REPR child",
    );
    assert_lossless(source, &parse);
}

/// A `with`-augmentation on a delegate is illegal in FCS
/// (`parsAugmentationsIllegalOnDelegateType`). We record that error (and parse
/// the block anyway, losslessly).
#[test]
fn delegate_augmentation_errors() {
    let source = "type T = delegate of int -> int with member _.M = 1\n";
    let parse = parse(source);
    assert!(
        parse.errors.iter().any(|e| e
            .message
            .contains("Augmentations are not permitted on delegate")),
        "the illegal delegate augmentation is flagged: {:?}",
        parse.errors,
    );
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::DELEGATE_REPR),
        "the delegate body is still kept",
    );
    assert_lossless(source, &parse);
}

/// A *bare* (no `with`) member block after a delegate body is an augmentation
/// too in FCS — same `parsAugmentationsIllegalOnDelegateType` error. We consume
/// the member (as an outer `MEMBER_DEFN`, clean recovery) and emit **one**
/// targeted error rather than letting it spill to the module loop as generic
/// "unexpected token" noise.
#[test]
fn delegate_bare_augmentation_errors() {
    let source = "type D =\n  delegate of int -> int\n  member _.M = 1\n";
    let parse = parse(source);
    let aug_errors: Vec<_> = parse
        .errors
        .iter()
        .filter(|e| {
            e.message
                .contains("Augmentations are not permitted on delegate")
        })
        .collect();
    assert_eq!(
        aug_errors.len(),
        1,
        "exactly one delegate-augmentation error: {:?}",
        parse.errors,
    );
    assert!(
        !parse.errors.iter().any(|e| e.message == "unexpected token"),
        "no generic stray-token spew: {:?}",
        parse.errors,
    );
    let type_defn = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TYPE_DEFN)
        .expect("a TYPE_DEFN node");
    assert!(
        type_defn
            .children()
            .any(|n| n.kind() == SyntaxKind::DELEGATE_REPR),
        "the delegate body is kept as a DELEGATE_REPR child",
    );
    assert!(
        type_defn
            .children()
            .any(|n| n.kind() == SyntaxKind::MEMBER_DEFN),
        "the illegal augmentation member is consumed into the outer members slot",
    );
    assert_lossless(source, &parse);
}

/// An `and`-chain of bodyless types — `type a\nand b`. FCS keeps these in **one**
/// `SynModuleDecl.Types` (the `and` continues the chain even when both bodies are
/// absent), so the green tree is a single `TYPE_DEFNS` holding two `TYPE_DEFN`s,
/// each with no repr node. Guards that the bodyless path still feeds the
/// `and`-continuation gate.
#[test]
fn type_bodyless_and_chain_green_shape() {
    let source = "type a\nand b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..13
  MODULE_OR_NAMESPACE@0..13
    TYPE_DEFNS@0..12
      TYPE_DEFN@0..6
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"a\"
      NEWLINE@6..7 \"\\n\"
      TYPE_DEFN@7..12
        AND_TOK@7..10 \"and\"
        WHITESPACE@10..11 \" \"
        LONG_IDENT@11..12
          IDENT_TOK@11..12 \"b\"
    NEWLINE@12..13 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Two consecutive bodyless `type` declarations are **two** separate `Types`
/// nodes (only `and` aggregates). Pins that the bodyless path does not greedily
/// swallow a following fresh `type` into the chain.
#[test]
fn type_bodyless_two_decls_are_two_groups() {
    use crate::syntax::{AstNode, ImplFile};
    let source = "type a\ntype b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let groups = module.decls().count();
    assert_eq!(groups, 2, "two bodyless `type` decls are two Types groups");
    assert_lossless(source, &parse);
}

/// Type-header accessibility — `type internal Foo = int`. The modifier (FCS's
/// `tyconNameAndTyparDecls: opt_access path`) is consumed as an `ACCESS_TOK`
/// that is a direct child token of `TYPE_DEFN`, positioned *between* the
/// leading `TYPE_TOK` and the name's `LONG_IDENT` (a sibling token, mirroring
/// the other accessibility sites — invisible to the node-based header
/// projection). No errors; the facade still reads the name `Foo`.
#[test]
fn type_header_access_modifier() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "type internal Foo = int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let type_defn = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TYPE_DEFN)
        .expect("a TYPE_DEFN node");
    // The modifier is claimed as a direct ACCESS_TOK child of TYPE_DEFN, never
    // ERROR.
    let access_idx = type_defn
        .children_with_tokens()
        .position(|el| el.kind() == SyntaxKind::ACCESS_TOK)
        .expect("an ACCESS_TOK direct child of TYPE_DEFN");
    let type_tok_idx = type_defn
        .children_with_tokens()
        .position(|el| el.kind() == SyntaxKind::TYPE_TOK)
        .expect("a TYPE_TOK");
    let long_ident_idx = type_defn
        .children_with_tokens()
        .position(|el| el.kind() == SyntaxKind::LONG_IDENT)
        .expect("a LONG_IDENT name");
    assert!(
        type_tok_idx < access_idx && access_idx < long_ident_idx,
        "ACCESS_TOK must sit between TYPE_TOK and the name LONG_IDENT \
         (type_tok={type_tok_idx}, access={access_idx}, long_ident={long_ident_idx})",
    );
    // The typed AST still reads the plain header name.
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl")
    };
    let segs: Vec<String> = t
        .defns()
        .next()
        .expect("one type defn")
        .long_id()
        .expect("a LONG_IDENT name")
        .idents()
        .map(|tok| tok.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo"]);
    assert_lossless(source, &parse);
}

/// After-name constructor accessibility with no parens — `type C private =
/// int`. FCS parses and discards the modifier (no `ImplicitCtor`,
/// `ComponentInfo.accessibility` stays `None`), so the repr is a plain
/// abbreviation. We consume it as an `ACCESS_TOK` direct child of `TYPE_DEFN`,
/// positioned *after* the name `LONG_IDENT` and before the repr's `EQUALS_TOK`
/// — distinct from the before-name header access (which precedes the
/// `LONG_IDENT`). No errors; the facade reads an `Abbrev` repr.
#[test]
fn type_after_name_access_modifier() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type C private = int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let type_defn = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::TYPE_DEFN)
        .expect("a TYPE_DEFN node");
    let long_ident_idx = type_defn
        .children_with_tokens()
        .position(|el| el.kind() == SyntaxKind::LONG_IDENT)
        .expect("a LONG_IDENT name");
    let access_idx = type_defn
        .children_with_tokens()
        .position(|el| el.kind() == SyntaxKind::ACCESS_TOK)
        .expect("an ACCESS_TOK direct child of TYPE_DEFN");
    let equals_idx = type_defn
        .children_with_tokens()
        .position(|el| el.kind() == SyntaxKind::EQUALS_TOK)
        .expect("an EQUALS_TOK");
    assert!(
        long_ident_idx < access_idx && access_idx < equals_idx,
        "ACCESS_TOK must sit after the name LONG_IDENT and before EQUALS_TOK \
         (long_ident={long_ident_idx}, access={access_idx}, equals={equals_idx})",
    );
    // The repr is an unperturbed abbreviation of `int`.
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl")
    };
    assert!(
        matches!(
            t.defns().next().expect("one defn").repr(),
            Some(TypeDefnRepr::Abbrev(_))
        ),
        "the repr stays a plain abbreviation",
    );
    assert_lossless(source, &parse);
}

/// Regression: the after-name access discard must be gated on reaching the
/// repr's `=`, not consumed before just any token. FCS's after-name `opt_access`
/// belongs to the `… EQUALS …` production; the augmentation production
/// (`typeNameInfo WITH …`) has *no* `opt_access` slot, so `type T private with
/// …` is an FCS error ("Unexpected keyword 'with' … Expected '='"). If the
/// modifier were swallowed unconditionally it would expose the `with` and parse
/// a *valid* augmentation (zero errors) — a silent divergence. With the `=`
/// gate the `private` stays unconsumed and the declaration errors, mirroring
/// FCS.
#[test]
fn type_after_name_access_not_consumed_before_with() {
    let source = "type T private with member _.M = 1\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "`type T private with …` is invalid in FCS and must not parse as a \
         clean augmentation; got no errors",
    );
    assert_lossless(source, &parse);
}

/// Phase 9.1 — two consecutive `type` declarations are **two** separate
/// `TYPE_DEFNS` carriers (only `and` aggregates, which is phase 9.2). Guards
/// the swallowed-`type` preservation through the inter-decl `OBLOCKSEP`.
#[test]
fn two_type_decls_are_two_groups() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "type T = int\ntype U = string\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let groups: Vec<_> = module
        .decls()
        .filter(|d| matches!(d, ModuleDecl::Types(_)))
        .collect();
    assert_eq!(groups.len(), 2, "two separate Types groups");
    assert_lossless(source, &parse);
}

/// Phase 10.7a — a type-header attribute (`[<Foo>] type T = int`) attaches to
/// the type definition's `SynComponentInfo` (its leading `ATTRIBUTE_LIST`
/// children of the first `TYPE_DEFN`), parsing cleanly. Pins the facade
/// (`TypeDefn::attributes()`) and that the `type T = int` body is unaffected.
#[test]
fn type_defn_attribute_green_shape() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "[<Foo>] type T = int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defns: Vec<_> = t.defns().collect();
    assert_eq!(defns.len(), 1, "one type definition");
    let lists: Vec<_> = defns[0].attributes().collect();
    assert_eq!(
        lists.len(),
        1,
        "one attribute list on the type header, got {lists:?}"
    );
    assert_eq!(
        lists[0].attributes().count(),
        1,
        "one attribute in the list",
    );
    let segs: Vec<String> = defns[0]
        .long_id()
        .expect("type defn has a LONG_IDENT name")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["T"], "the `T = int` body is unaffected");
}

/// Phase 10.7a follow-up — an offside *name* after an after-keyword attribute
/// (`type [<A>]⏎T = int`, name aligned at column 0 on a fresh line) parses
/// cleanly: the attribute attaches to `T`'s `SynComponentInfo` and the
/// abbreviation body is unaffected. FCS accepts this because the attribute
/// production's trailing `opt_OBLOCKSEP` absorbs the inter-line separator the
/// column-0 name emits; we mirror that by draining the `BlockSep` after the
/// after-keyword attribute list in `parse_type_defn_header`.
#[test]
fn type_attr_offside_name_after_keyword_green_shape() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "type [<A>]\nT = int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defns: Vec<_> = t.defns().collect();
    assert_eq!(defns.len(), 1, "one type definition");
    let lists: Vec<_> = defns[0].attributes().collect();
    assert_eq!(
        lists.len(),
        1,
        "one attribute list on the type header, got {lists:?}"
    );
    let segs: Vec<String> = defns[0]
        .long_id()
        .expect("type defn has a LONG_IDENT name")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["T"], "the col-0 `T = int` body parses as `T`");
}

/// Phase 10.7a — an offside *name* after `[<A>] type` (`[<A>] type⏎T = int`) is
/// a parse error in FCS too (`ParseHadErrors = true` — the name must follow
/// `type`). The dispatch skips only a `BlockSep` that *precedes* the `type`
/// keyword, so the post-`type` separator is left in place and the name parse
/// records a clean error — we must **not** silently accept it. (Distinct from
/// the *after*-keyword offside name `type [<A>]⏎T`, which FCS accepts and we
/// support — see `type_attr_offside_name_after_keyword_green_shape`.) Pinned
/// lossless + erroring.
#[test]
fn type_attr_offside_name_after_type_is_error() {
    let source = "[<A>] type\nT = int\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "an offside name after `[<A>] type` must error (as FCS does)",
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7a follow-up — an `and`-chain *after* a column-0 offside name
/// (`type [<A>]⏎T = int⏎and U = string`) parses as the whole group: the
/// attribute attaches to `T`, the continuation `U` is a second `TYPE_DEFN` with
/// empty attributes. The column-0 name makes every body blockless (no
/// `OBLOCKEND`), so the `and`-chain loop relies on the column-0 regime
/// (established by the first definition's drained attribute separator) rather
/// than `prev_closed` to keep going — matching FCS.
#[test]
fn type_attr_offside_name_after_keyword_and_chain_green_shape() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "type [<A>]\nT = int\nand U = string\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defns: Vec<_> = t.defns().collect();
    assert_eq!(defns.len(), 2, "two type definitions in the chain");
    assert_eq!(
        defns[0].attributes().count(),
        1,
        "the attribute attaches to the first definition `T`",
    );
    assert_eq!(
        defns[1].attributes().count(),
        0,
        "the `and`-chained `U` carries no attributes",
    );
    let names: Vec<Vec<String>> = defns
        .iter()
        .map(|d| {
            d.long_id()
                .expect("type defn has a name")
                .idents()
                .map(|t| t.text().to_string())
                .collect()
        })
        .collect();
    assert_eq!(
        names,
        vec![vec!["T"], vec!["U"]],
        "the chain is `T` then `U`"
    );
}

/// Phase 10.7a follow-up — the column-0 `and`-chain is licensed *only* by the
/// attribute (its trailing `opt_OBLOCKSEP`). Without it, the column-0 name
/// `type⏎T = int⏎and U = string` is an FCS parse error, and our column-0 regime
/// must **not** kick in (it is gated on the drained attribute separator), so the
/// `and U …` is left as a clean error rather than spliced into a bogus chain.
/// Pins the gate against over-accepting. Lossless + erroring.
#[test]
fn type_no_attr_offside_name_and_chain_is_error() {
    let source = "type\nT = int\nand U = string\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a bare column-0 `and`-chain (no attribute) must stay an error, as FCS does",
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7a follow-up — a column-0 offside name whose body's repr *also* sits
/// at column 0 (`type [<A>]⏎T =⏎| A`, the union case `|` un-indented) is an FCS
/// parse error: the offside-name fix licenses the column-0 *name*, but the
/// blockless body's repr must still be offside-indented (FCS accepts the same
/// type with the `|` indented — pinned by
/// `diff_ast_type_attr_offside_name_union_body`). We must not over-accept the
/// un-indented form; it stays a clean error. Lossless + erroring.
#[test]
fn type_attr_offside_name_col0_union_body_is_error() {
    let source = "type [<A>]\nT =\n| A\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a column-0 union body (un-indented `|`) must stay an error, as FCS does",
    );
    assert_lossless(source, &parse);
}

/// Phase 9.2 — an `and`-chain is **one** `TYPE_DEFNS` carrier holding several
/// `TYPE_DEFN`, each leading with its keyword (`TYPE_TOK` then `AND_TOK`). Pins
/// the full green shape, including the per-definition `OBLOCKBEGIN`/`OBLOCKEND`
/// virtuals (zero-width `ERROR`s) and the inter-definition newline kept as a
/// `TYPE_DEFNS`-level sibling.
#[test]
fn type_and_chain_green_shape() {
    let source = "type T = int\nand U = string\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..28
  MODULE_OR_NAMESPACE@0..28
    TYPE_DEFNS@0..27
      TYPE_DEFN@0..12
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        WHITESPACE@8..9 \" \"
        ERROR@9..9 \"\"
        TYPE_ABBREV@9..12
          LONG_IDENT_TYPE@9..12
            LONG_IDENT@9..12
              IDENT_TOK@9..12 \"int\"
        ERROR@12..12 \"\"
      NEWLINE@12..13 \"\\n\"
      TYPE_DEFN@13..27
        AND_TOK@13..16 \"and\"
        WHITESPACE@16..17 \" \"
        LONG_IDENT@17..18
          IDENT_TOK@17..18 \"U\"
        WHITESPACE@18..19 \" \"
        EQUALS_TOK@19..20 \"=\"
        WHITESPACE@20..21 \" \"
        ERROR@21..21 \"\"
        TYPE_ABBREV@21..27
          LONG_IDENT_TYPE@21..27
            LONG_IDENT@21..27
              IDENT_TOK@21..27 \"string\"
        ERROR@27..27 \"\"
    NEWLINE@27..28 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.2 — facade: one `Types` group with two `TypeDefn`s named `T` and
/// `U`, both abbreviations.
#[test]
fn type_and_chain_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "type T = int\nand U = string\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let groups: Vec<_> = module
        .decls()
        .filter_map(|d| match d {
            ModuleDecl::Types(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(groups.len(), 1, "an `and`-chain is one Types group");
    let names: Vec<String> = groups[0]
        .defns()
        .map(|d| {
            d.long_id()
                .expect("each defn has a name")
                .idents()
                .map(|t| t.text().to_string())
                .collect::<Vec<_>>()
                .join(".")
        })
        .collect();
    assert_eq!(names, vec!["T", "U"], "both definitions in one group");
    assert_lossless(source, &parse);
}

/// Phase 9.2 — a *single-line* `and`-chain is invalid F# (FCS: "Unexpected
/// keyword 'and' in member definition"); the inline `and` stays inside the
/// first body's still-open offside block. The parser must **not** splice it
/// into a chain — it records an error rather than producing a clean
/// multi-definition group. (Exact recovery shape is phase-11 territory; this
/// only pins that we reject rather than silently accept.)
#[test]
fn inline_type_and_chain_is_rejected() {
    let source = "type T = int and U = string\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "an inline `type … and …` must be rejected, not silently accepted"
    );
    assert_lossless(source, &parse);
}

// ---- Phase 9.3: type parameters ----------------------------------------

/// Phase 9.3 — postfix `type T<'a>`: a `TYPAR_DECLS` (with the
/// `HighPrecedenceTyApp` virtual as a zero-width `ERROR`, then `<`, the decls,
/// `>`) sits after the name. Pins the full green shape.
#[test]
fn type_param_postfix_green_shape() {
    let source = "type T<'a> = 'a list\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..21
  MODULE_OR_NAMESPACE@0..21
    TYPE_DEFNS@0..20
      TYPE_DEFN@0..20
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        TYPAR_DECLS@6..10
          ERROR@6..6 \"\"
          LESS_TOK@6..7 \"<\"
          TYPAR_DECL@7..9
            QUOTE_TOK@7..8 \"'\"
            IDENT_TOK@8..9 \"a\"
          GREATER_TOK@9..10 \">\"
        WHITESPACE@10..11 \" \"
        EQUALS_TOK@11..12 \"=\"
        WHITESPACE@12..13 \" \"
        ERROR@13..13 \"\"
        TYPE_ABBREV@13..20
          APP_TYPE@13..20
            VAR_TYPE@13..15
              QUOTE_TOK@13..14 \"'\"
              IDENT_TOK@14..15 \"a\"
            LONG_IDENT_TYPE@15..20
              LONG_IDENT@15..20
                WHITESPACE@15..16 \" \"
                IDENT_TOK@16..20 \"list\"
        ERROR@20..20 \"\"
    NEWLINE@20..21 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.3 — facade: postfix and prefix both expose the type parameters via
/// `TypeDefn::typar_decls()`, and a head-typar `^a` reads `is_head_type`.
#[test]
fn type_param_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let collect = |source: &str| -> Vec<(String, bool)> {
        let parse = parse(source);
        assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let defn = t.defns().next().expect("one defn");
        defn.typar_decls()
            .expect("generic defn has typar decls")
            .typars()
            .map(|d| {
                (
                    d.ident().expect("typar ident").text().to_string(),
                    d.is_head_type(),
                )
            })
            .collect()
    };
    assert_eq!(
        collect("type T<'a> = 'a list\n"),
        vec![("a".to_string(), false)]
    );
    assert_eq!(
        collect("type 'a T = 'a list\n"),
        vec![("a".to_string(), false)]
    );
    assert_eq!(
        collect("type T<'a, 'b> = 'a\n"),
        vec![("a".to_string(), false), ("b".to_string(), false)]
    );
    assert_eq!(collect("type T<^a> = int\n"), vec![("a".to_string(), true)]);
}

/// Phase 9.3 — parenthesised-prefix `('a, 'b) T` is deferred (its `)` is
/// LexFilter-swallowed). A `(` after `type` is not claimed as a typar group; it
/// falls through to the name parser and records a clean "expected identifier"
/// error rather than panicking.
#[test]
fn prefix_list_typars_are_a_clean_error() {
    let source = "type ('a, 'b) T = 'a\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "parenthesised-prefix typars are deferred; expected a clean error"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.3 — a malformed postfix type-parameter list is rejected (FCS's
/// `postfixTyparDecls` requires a non-empty `typarDeclList` closed by `>`): a
/// missing close (`type T<'a = 'a`) and an empty list (`type T< > = int`) each
/// record an error rather than being silently accepted as a generic type.
#[test]
fn malformed_postfix_typars_are_rejected() {
    for source in ["type T<'a = 'a\n", "type T< > = int\n"] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "malformed postfix typars must be rejected, not silently accepted: {source:?}"
        );
        assert_lossless(source, &parse);
    }
}

// ---- Phase 9.4: record types -------------------------------------------

/// Phase 9.4 — `RECORD_REPR > [LBRACE_TOK, RECORD_FIELD_DECL, RBRACE_TOK]`: the `{`
/// is a real token, the `}` is the LexFilter-swallowed close recovered as
/// `RBRACE_TOK`, and the field is `[IDENT_TOK, COLON_TOK, <typ>]`. Pins the
/// full green shape.
#[test]
fn record_green_shape() {
    let source = "type T = { X : int }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..21
  MODULE_OR_NAMESPACE@0..21
    TYPE_DEFNS@0..20
      TYPE_DEFN@0..20
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        WHITESPACE@8..9 \" \"
        ERROR@9..9 \"\"
        RECORD_REPR@9..20
          LBRACE_TOK@9..10 \"{\"
          RECORD_FIELD_DECL@10..18
            WHITESPACE@10..11 \" \"
            IDENT_TOK@11..12 \"X\"
            WHITESPACE@12..13 \" \"
            COLON_TOK@13..14 \":\"
            WHITESPACE@14..15 \" \"
            LONG_IDENT_TYPE@15..18
              LONG_IDENT@15..18
                IDENT_TOK@15..18 \"int\"
          WHITESPACE@18..19 \" \"
          RBRACE_TOK@19..20 \"}\"
        ERROR@20..20 \"\"
    NEWLINE@20..21 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// A *repeated* field separator inside a record type-def is invalid. FCS's
/// `seps_block` is a single separator group, so `type T = { F : int; ; G : int }`
/// is a parse error (`ParseHadErrors: true`, verified against `fcs-dump ast`).
/// The parser consumes exactly one group per gap, so the stray second `;` trips
/// the field parser's recovery — pinning that we do *not* silently accept the
/// malformed run. Single-separator, offside, and `}`-on-own-line forms stay
/// valid (covered by the diff tests).
#[test]
fn record_repr_repeated_separator_errors() {
    let source = "type T = { F : int; ; G : int }\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        !parse.errors.is_empty(),
        "a repeated record type-def separator must record a parse error",
    );
}

/// A record field name must be gated on the *filtered* token: when layout
/// intervenes between `mutable`/access and the name (`type R = {\n  mutable\n  x
/// : int\n}`), a raw lookahead would see `x` through the pending `OBLOCKSEP` and
/// `bump_into` would consume the zero-width virtual as the name, corrupting the
/// field shape. The filtered cursor stops at the virtual, so no zero-width
/// `IDENT_TOK` is synthesised and the malformed field is reported, not masked.
#[test]
fn record_field_name_does_not_consume_layout_virtual() {
    let source = "type R = {\n  mutable\n  x : int\n}\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "the layout-broken record field should record a parse error"
    );
    for tok in parse
        .root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        if tok.kind() == SyntaxKind::IDENT_TOK {
            assert!(
                !tok.text_range().is_empty(),
                "a zero-width IDENT_TOK was synthesised from a layout virtual"
            );
        }
    }
    assert_lossless(source, &parse);
}

/// Phase 9.4 — facade: a `Record` repr exposes its fields, each with a name,
/// type, and mutability flag.
#[test]
fn record_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, Type, TypeDefnRepr};
    let source = "type T = { X : int; mutable Y : string }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defn = t.defns().next().expect("one defn");
    let Some(TypeDefnRepr::Record(rec)) = defn.repr() else {
        panic!("expected a Record repr, got {:?}", defn.repr());
    };
    let fields: Vec<_> = rec.fields().collect();
    assert_eq!(fields.len(), 2, "two record fields");
    assert_eq!(fields[0].ident().expect("name").text(), "X");
    assert!(!fields[0].is_mutable());
    assert!(matches!(fields[0].ty(), Some(Type::LongIdent(_))));
    assert_eq!(fields[1].ident().expect("name").text(), "Y");
    assert!(fields[1].is_mutable(), "second field is `mutable`");
    assert_lossless(source, &parse);
}

/// Phase 9.4 — an empty record `{ }` is invalid (FCS's `braceFieldDeclList` has
/// no empty production); the parser records an error rather than producing a
/// zero-field record.
#[test]
fn empty_record_is_rejected() {
    let source = "type T = { }\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "an empty record `{{ }}` must be rejected, not silently accepted"
    );
    assert_lossless(source, &parse);
}

// ---- Phase 9.5: discriminated unions -----------------------------------

/// Phase 9.5 — `UNION_REPR > [UNION_CASE, BAR_TOK, UNION_CASE > [IDENT_TOK,
/// OF_TOK, UNION_CASE_FIELD]]`: a nullary case, a `Bar` separator, and a case
/// with one `of` field. Pins the full green shape.
#[test]
fn union_green_shape() {
    let source = "type T = A | B of int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..22
  MODULE_OR_NAMESPACE@0..22
    TYPE_DEFNS@0..21
      TYPE_DEFN@0..21
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        WHITESPACE@8..9 \" \"
        ERROR@9..9 \"\"
        UNION_REPR@9..21
          UNION_CASE@9..10
            IDENT_TOK@9..10 \"A\"
          WHITESPACE@10..11 \" \"
          BAR_TOK@11..12 \"|\"
          UNION_CASE@12..21
            WHITESPACE@12..13 \" \"
            IDENT_TOK@13..14 \"B\"
            WHITESPACE@14..15 \" \"
            OF_TOK@15..17 \"of\"
            UNION_CASE_FIELD@17..21
              WHITESPACE@17..18 \" \"
              LONG_IDENT_TYPE@18..21
                LONG_IDENT@18..21
                  IDENT_TOK@18..21 \"int\"
        ERROR@21..21 \"\"
    NEWLINE@21..22 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// The union-case `FullType` signature form (`SynUnionCaseKind.FullType`,
/// FSharp.Core's `Option`/`Choice`): a bar-less single case `A : int -> T`
/// becomes a single-case `UNION_REPR` (not an abbreviation), and the case holds
/// `[IDENT_TOK, COLON_TOK, <type>]` — the `topType` signature in place of `of`
/// fields.
#[test]
fn union_fulltype_green_shape() {
    let source = "type T = A : int -> T\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..22
  MODULE_OR_NAMESPACE@0..22
    TYPE_DEFNS@0..21
      TYPE_DEFN@0..21
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        WHITESPACE@8..9 \" \"
        ERROR@9..9 \"\"
        UNION_REPR@9..21
          UNION_CASE@9..21
            IDENT_TOK@9..10 \"A\"
            WHITESPACE@10..11 \" \"
            COLON_TOK@11..12 \":\"
            WHITESPACE@12..13 \" \"
            FUN_TYPE@13..21
              LONG_IDENT_TYPE@13..16
                LONG_IDENT@13..16
                  IDENT_TOK@13..16 \"int\"
              WHITESPACE@16..17 \" \"
              RARROW_TOK@17..19 \"->\"
              WHITESPACE@19..20 \" \"
              LONG_IDENT_TYPE@20..21
                LONG_IDENT@20..21
                  IDENT_TOK@20..21 \"T\"
        ERROR@21..21 \"\"
    NEWLINE@21..22 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Operator union-case names (FCS's `unionCaseName` operator forms, FSharp.Core's
/// `list`): `([])` (`op_Nil`) is `[LPAREN_TOK, LBRACK_TOK, RBRACK_TOK,
/// RPAREN_TOK]` and `( :: )` (`op_ColonColon`) is `[LPAREN_TOK, COLON_COLON_TOK,
/// RPAREN_TOK]`, the closing `)` LexFilter-swallowed and recovered. A bar-less
/// leading `([])` is a single-case union (not an array-type abbreviation). Pins
/// the `op_Nil` green shape.
#[test]
fn union_op_nil_green_shape() {
    let source = "type T = ([]) : int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..20
  MODULE_OR_NAMESPACE@0..20
    TYPE_DEFNS@0..19
      TYPE_DEFN@0..19
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        WHITESPACE@8..9 \" \"
        ERROR@9..9 \"\"
        UNION_REPR@9..19
          UNION_CASE@9..19
            LPAREN_TOK@9..10 \"(\"
            LBRACK_TOK@10..11 \"[\"
            RBRACK_TOK@11..12 \"]\"
            RPAREN_TOK@12..13 \")\"
            WHITESPACE@13..14 \" \"
            COLON_TOK@14..15 \":\"
            WHITESPACE@15..16 \" \"
            LONG_IDENT_TYPE@16..19
              LONG_IDENT@16..19
                IDENT_TOK@16..19 \"int\"
        ERROR@19..19 \"\"
    NEWLINE@19..20 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Both operator cases parse cleanly and losslessly in the real `list` shape
/// (`| ([]) : … | ( :: ) : Head:… * Tail:… -> …`), combining the operator names
/// with the `FullType` signature form.
#[test]
fn union_op_cases_list_shape_is_clean() {
    let source =
        "type List<'T> =\n   | ([]) : 'T list\n   | ( :: ) : Head:'T * Tail:'T list -> 'T list\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
    let cases = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::UNION_CASE)
        .count();
    assert_eq!(cases, 2, "two operator union cases");
}

/// The `FullType` form reaches the AST: a `Union` case whose `full_type()` is
/// the signature type and whose `fields()` are empty (the `Option`-shaped
/// definition with both a nullary and a labelled-signature case).
#[test]
fn union_fulltype_reaches_ast() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type Opt<'T> =\n    | None : 'T option\n    | Some : Value:'T -> 'T option\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);

    let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let defn = module
        .decls()
        .find_map(|d| match d {
            ModuleDecl::Types(t) => t.defns().next(),
            _ => None,
        })
        .expect("a type definition");
    let Some(TypeDefnRepr::Union(u)) = defn.repr() else {
        panic!("expected a union repr, got {:?}", defn.repr());
    };
    let cases: Vec<_> = u.cases().collect();
    assert_eq!(cases.len(), 2, "two FullType cases");
    for case in &cases {
        assert_eq!(
            case.fields().count(),
            0,
            "a FullType case has no `of` fields"
        );
        assert!(
            case.full_type().is_some(),
            "a FullType case exposes its signature type",
        );
    }
    assert_eq!(
        cases[0].ident().map(|t| t.text().to_string()),
        Some("None".to_string()),
    );
}

/// Phase 9.5 — facade: a `Union` repr exposes cases; a case exposes its name
/// and `of` fields (anonymous and named).
#[test]
fn union_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type T = A | B of int * y:string\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::Union(u)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected a Union repr");
    };
    let cases: Vec<_> = u.cases().collect();
    assert_eq!(cases.len(), 2, "two cases");
    assert_eq!(cases[0].ident().expect("name").text(), "A");
    assert_eq!(cases[0].fields().count(), 0, "A is nullary");
    assert_eq!(cases[1].ident().expect("name").text(), "B");
    let b_fields: Vec<_> = cases[1].fields().collect();
    assert_eq!(b_fields.len(), 2, "B has two fields");
    assert!(b_fields[0].ident().is_none(), "first field is anonymous");
    assert_eq!(
        b_fields[1].ident().expect("named field").text(),
        "y",
        "second field is named `y`"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.5 recovery — an incomplete `type T = A of` (no field type) must be a
/// *recoverable* error, never a panic: FCS recovers with case `A` and zero
/// fields. The case-field parse is gated on a type-starter to avoid reaching
/// `parse_atomic_type`'s `unreachable!` arm at EOF.
#[test]
fn union_incomplete_of_is_recoverable() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type T = A of\n";
    let parse = parse(source); // must not panic
    assert!(
        !parse.errors.is_empty(),
        "`A of` with no field type should record an error"
    );
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::Union(u)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected a Union repr");
    };
    let cases: Vec<_> = u.cases().collect();
    assert_eq!(cases.len(), 1, "case `A` still parses");
    assert_eq!(cases[0].ident().expect("name").text(), "A");
    assert_lossless(source, &parse);
}

/// Phase 9.5 — a bare (unparenthesised) `T | null` union-case field is invalid
/// F# (FCS: "Unexpected symbol '|' (directly before 'null')"): the `| null` is
/// not absorbed into the field type. The parser uses `parse_app_type` (not the
/// can-be-nullable variant), so the `|` terminates the field and `null` (not a
/// valid case name) errors. Pins that we reject rather than silently accept.
#[test]
fn union_unparenthesized_nullable_field_errors() {
    let source = "type T = A of string | null\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "an unparenthesised `| null` union-case field must be rejected"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.5 recovery — a (not-permitted) accessibility modifier on a union
/// case (`type T = A | private B`): FCS consumes the `private`, reports it as
/// not permitted, and recovers with both cases. The parser mirrors that
/// (consume `ACCESS_TOK` + a diagnostic) so case `B` still parses, rather than
/// stranding `private` and ending the union early.
#[test]
fn union_case_access_recovers() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type T = A | private B\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a union-case accessibility modifier should record an error"
    );
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::Union(u)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected a Union repr");
    };
    let names: Vec<String> = u
        .cases()
        .map(|c| c.ident().expect("case name").text().to_string())
        .collect();
    assert_eq!(names, vec!["A", "B"], "both cases recover");
    assert_lossless(source, &parse);
}

// ---- Phase 9.6: enums --------------------------------------------------

/// Phase 9.6 — `ENUM_REPR > [ENUM_CASE > [IDENT_TOK, EQUALS_TOK, <value>],
/// BAR_TOK, ENUM_CASE]`: the repr is chosen post-hoc (any `= value` case ⇒
/// enum). Pins the full green shape.
#[test]
fn enum_green_shape() {
    let source = "type T = A = 0 | B = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..23
  MODULE_OR_NAMESPACE@0..23
    TYPE_DEFNS@0..22
      TYPE_DEFN@0..22
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        WHITESPACE@8..9 \" \"
        ERROR@9..9 \"\"
        ENUM_REPR@9..22
          ENUM_CASE@9..14
            IDENT_TOK@9..10 \"A\"
            WHITESPACE@10..11 \" \"
            EQUALS_TOK@11..12 \"=\"
            WHITESPACE@12..13 \" \"
            CONST_EXPR@13..14
              INT32_LIT@13..14 \"0\"
          WHITESPACE@14..15 \" \"
          BAR_TOK@15..16 \"|\"
          ENUM_CASE@16..22
            WHITESPACE@16..17 \" \"
            IDENT_TOK@17..18 \"B\"
            WHITESPACE@18..19 \" \"
            EQUALS_TOK@19..20 \"=\"
            WHITESPACE@20..21 \" \"
            CONST_EXPR@21..22
              INT32_LIT@21..22 \"1\"
        ERROR@22..22 \"\"
    NEWLINE@22..23 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.6 — facade: an `Enum` repr exposes its cases, each with a name and a
/// value expression.
#[test]
fn enum_facade() {
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type T = A = 0 | B = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::Enum(e)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an Enum repr");
    };
    let cases: Vec<_> = e.cases().collect();
    assert_eq!(cases.len(), 2, "two enum cases");
    assert_eq!(cases[0].ident().expect("name").text(), "A");
    assert!(
        matches!(cases[0].value(), Some(Expr::Const(_))),
        "value is a const"
    );
    assert_eq!(cases[1].ident().expect("name").text(), "B");
    assert_lossless(source, &parse);
}

/// Phase 10.7 — attributes on type-repr elements: `UnionCase::attributes()`,
/// `EnumCase::attributes()`, and `RecordFieldDecl::attributes()` expose the
/// leading `[<…>]` lists; an unattributed case/field yields none.
#[test]
fn case_field_attributes_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefn, TypeDefnRepr};
    // Project the single `TypeDefn` of a one-type-decl source.
    fn sole_repr(source: &str) -> (crate::parser::Parse, TypeDefnRepr) {
        let parsed = parse(source);
        assert!(
            parsed.errors.is_empty(),
            "{source:?} errors: {:?}",
            parsed.errors
        );
        let repr = ImplFile::cast(parsed.root.clone())
            .and_then(|f| f.modules().next())
            .and_then(|m| m.decls().next())
            .and_then(|d| match d {
                ModuleDecl::Types(t) => t.defns().next(),
                _ => None,
            })
            .as_ref()
            .and_then(TypeDefn::repr)
            .expect("a type definition repr");
        (parsed, repr)
    }

    // Union: one attributed case, one plain.
    let (_p, repr) = sole_repr("type T = | [<A>] X | Y\n");
    let TypeDefnRepr::Union(u) = repr else {
        panic!("Union repr");
    };
    let cases: Vec<_> = u.cases().collect();
    assert_eq!(cases[0].attributes().count(), 1, "X has one attribute list");
    assert_eq!(cases[1].attributes().count(), 0, "Y has no attributes");

    // Enum: an attributed case.
    let (_p, repr) = sole_repr("type E = | [<A>] A = 0\n");
    let TypeDefnRepr::Enum(e) = repr else {
        panic!("Enum repr");
    };
    assert_eq!(
        e.cases().next().expect("case").attributes().count(),
        1,
        "enum case has one attribute list"
    );

    // Record: an attributed field and a plain field.
    let (_p, repr) = sole_repr("type R = { [<A>] X : int; Y : string }\n");
    let TypeDefnRepr::Record(r) = repr else {
        panic!("Record repr");
    };
    let fields: Vec<_> = r.fields().collect();
    assert_eq!(
        fields[0].attributes().count(),
        1,
        "X has one attribute list"
    );
    assert_eq!(fields[1].attributes().count(), 0, "Y has no attributes");
}

/// Phase 10.7 — a first union case attributed *without a leading `|`*
/// (`type T = [<A>] X | Y`) is an FCS parse error (the case attr must follow a
/// bar). Our repr dispatch likewise never treats a leading `[<` as a union
/// start, so it errors — but stays lossless (clean error, no corruption).
#[test]
fn union_case_attr_without_leading_bar_errors_losslessly() {
    let source = "type T = [<A>] X | Y\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "an attributed first case without a leading `|` is an FCS error"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7 — a standalone `[<assembly: …>]` (not attached to a carrier) parses
/// into a `ModuleDecl::Attributes` whose `attributes()` yields the lists; the
/// following declaration parses separately. Bare-at-EOF likewise yields an
/// `Attributes` decl with no error on our side.
#[test]
fn standalone_attributes_facade() {
    use crate::syntax::{AstNode, AttributesDecl, ImplFile, ModuleDecl};
    let source = "[<assembly: A>]\n[<assembly: B>]\nignore 0\n";
    let multi = parse(source);
    assert!(multi.errors.is_empty(), "errors: {:?}", multi.errors);
    let module = ImplFile::cast(multi.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let mut decls = module.decls();
    let ModuleDecl::Attributes(a): ModuleDecl = decls.next().expect("first decl") else {
        panic!("first decl should be a ModuleDecl::Attributes");
    };
    assert_eq!(
        a.attributes().count(),
        2,
        "two consecutive `[<…>]` lists in one Attributes decl"
    );
    assert!(
        matches!(decls.next(), Some(ModuleDecl::Expr(_))),
        "the following `ignore 0` is a separate Expr decl"
    );
    assert_lossless(source, &multi);

    // Bare at EOF — one Attributes decl, parsed without error on our side.
    let eof = "[<assembly: Foo>]\n";
    let eof_parse = parse(eof);
    assert!(
        eof_parse.errors.is_empty(),
        "EOF errors: {:?}",
        eof_parse.errors
    );
    let first = ImplFile::cast(eof_parse.root.clone())
        .and_then(|f| f.modules().next())
        .and_then(|m| m.decls().next());
    assert!(
        matches!(first, Some(ModuleDecl::Attributes(_))),
        "a bare `[<assembly: …>]` at EOF is a ModuleDecl::Attributes"
    );
    let _ = AttributesDecl::can_cast(crate::syntax::SyntaxKind::ATTRIBUTES_DECL);
    assert_lossless(eof, &eof_parse);
}

/// Phase 10.7 — the canonical AssemblyInfo idiom `[<assembly: Foo>]⏎ do ()`: the
/// standalone `Attributes` decl is the first decl, followed by the top-level
/// `do` as its own `Expr` decl (`SynModuleDecl.Expr(SynExpr.Do …)`). The parse
/// is clean (no errors) and lossless.
#[test]
fn standalone_attributes_before_do() {
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl};
    let source = "[<assembly: Foo>]\ndo ()\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let mut decls = module.decls();
    assert!(
        matches!(decls.next(), Some(ModuleDecl::Attributes(_))),
        "the `Attributes` decl is first"
    );
    let Some(ModuleDecl::Expr(expr_decl)) = decls.next() else {
        panic!("the top-level `do` follows as an Expr decl");
    };
    assert!(
        matches!(expr_decl.expr(), Some(Expr::Do(_))),
        "the Expr decl wraps a DoExpr, got {:?}",
        expr_decl.expr(),
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7d — an attributed nested module header: `NestedModuleDecl::attributes()`
/// yields the leading lists (two here — the first multi-attribute) without
/// disturbing `long_id` / `decls`.
#[test]
fn nested_module_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "[<AutoOpen; CompiledName(\"X\")>]\n[<Foo>]\nmodule Inner =\n    let x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::NestedModule(nm) = module.decls().next().expect("one decl") else {
        panic!("expected a NestedModule decl");
    };
    let lists: Vec<usize> = nm.attributes().map(|l| l.attributes().count()).collect();
    assert_eq!(
        lists,
        vec![2, 1],
        "two ATTRIBUTE_LISTs: `[<AutoOpen; CompiledName>]` then `[<Foo>]`"
    );
    let segs: Vec<String> = nm
        .long_id()
        .expect("nested module has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Inner"], "attrs don't disturb the name");
    assert!(
        matches!(nm.decls().next(), Some(ModuleDecl::Let(_))),
        "the body `let` is still the first inner decl"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7d — a plain (unattributed) nested module exposes *no* attribute
/// lists: `attributes()` must not pick up anything from the body.
#[test]
fn nested_module_no_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "module Inner =\n    let x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::NestedModule(nm) = module.decls().next().expect("one decl") else {
        panic!("expected a NestedModule decl");
    };
    assert_eq!(nm.attributes().count(), 0, "no header attributes");
    assert_lossless(source, &parse);
}

/// Phase 10.7d — the *same-line* `[<AutoOpen>] module Inner =` form is an FCS
/// *parse error* (the attribute must precede `module` on its own line), so it is
/// out of scope for the diff harness. Our parser accepts it leniently: it still
/// produces a `NestedModule` carrying the attribute, and the parse is lossless —
/// pinning that the missing offside `BlockSep` doesn't corrupt the tree.
#[test]
fn nested_module_attr_same_line_lossless() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "[<AutoOpen>] module Inner =\n    let x = 1\n";
    let parse = parse(source);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::NestedModule(nm) = module.decls().next().expect("one decl") else {
        panic!("expected a NestedModule decl");
    };
    assert_eq!(
        nm.attributes().count(),
        1,
        "the same-line attribute still attaches to the module"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7d — an attributed module *abbreviation* (`[<A>]⏎module M = N`) is
/// rejected (FCS error 535, the decl dropped). Our parser records an error and
/// emits an `ERROR` node — *not* a `MODULE_ABBREV_DECL` — so the file projects no
/// decl, and the parse stays lossless.
#[test]
fn nested_module_attr_abbrev_is_error() {
    use crate::syntax::{AstNode, ImplFile};
    let source = "[<A>]\nmodule M = N\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "attributes on a module abbreviation must be flagged",
    );
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(
        module.decls().count(),
        0,
        "the rejected abbreviation projects no decl (the ERROR node isn't cast)"
    );
    assert!(
        !parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::MODULE_ABBREV_DECL),
        "no MODULE_ABBREV_DECL is emitted for the attributed abbreviation",
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7d — `NestedModuleDecl::attributes()` reports *only* header
/// attributes (those before `MODULE_TOK`). A body decl that fails attribute
/// recovery — here `[<A>] open System`, a deferred carrier — leaves a bare
/// `ATTRIBUTE_LIST` as a later direct child of the `NESTED_MODULE_DECL`; it must
/// not be mistaken for a header attribute of `M` (whose header carries none).
#[test]
fn nested_module_attr_accessor_excludes_body() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "module M =\n    [<A>] open System\n";
    let parse = parse(source);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::NestedModule(nm) = module.decls().next().expect("one decl") else {
        panic!("expected a NestedModule decl");
    };
    assert_eq!(
        nm.attributes().count(),
        0,
        "the body's recovery `[<A>]` is not a header attribute of `M`"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7k — an *after-keyword* nested attribute (`module [<A; B>] M = …`):
/// `NestedModuleDecl::attributes()` reports the list between `MODULE_TOK` and the
/// name, without disturbing `long_id` / `decls`.
#[test]
fn nested_module_after_kw_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "module [<A; B>] Inner =\n    let x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::NestedModule(nm) = module.decls().next().expect("one decl") else {
        panic!("expected a NestedModule decl");
    };
    let lists: Vec<usize> = nm.attributes().map(|l| l.attributes().count()).collect();
    assert_eq!(lists, vec![2], "one `[<A; B>]` list after `module`");
    let segs: Vec<String> = nm
        .long_id()
        .expect("nested module has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Inner"], "attrs don't disturb the name");
    assert!(
        matches!(nm.decls().next(), Some(ModuleDecl::Let(_))),
        "the body `let` is still the first inner decl"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7e — a whole-file `[<…>] module Foo` header exposes its attribute
/// lists via `ModuleOrNamespace::attributes()` (FCS's `SynModuleOrNamespace.attribs`)
/// without disturbing `kind()` / `long_id()` / the body.
#[test]
fn wholefile_module_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, ModuleOrNamespaceKind};
    let source = "[<AutoOpen; CompiledName(\"X\")>]\n[<Foo>]\nmodule Foo\nlet x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::NamedModule);
    let lists: Vec<usize> = module
        .attributes()
        .map(|l| l.attributes().count())
        .collect();
    assert_eq!(
        lists,
        vec![2, 1],
        "two ATTRIBUTE_LISTs: `[<AutoOpen; CompiledName>]` then `[<Foo>]`"
    );
    let segs: Vec<String> = module
        .long_id()
        .expect("named module has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo"], "attrs don't disturb the header name");
    assert!(
        matches!(module.decls().next(), Some(ModuleDecl::Let(_))),
        "the body `let` follows the header"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7k — an *after-keyword* whole-file attribute
/// (`module [<RequireQualifiedAccess>] Foo`): `ModuleOrNamespace::attributes()`
/// reports the list between `MODULE_TOK` and the name.
#[test]
fn wholefile_module_after_kw_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, ModuleOrNamespaceKind};
    let source = "module [<RequireQualifiedAccess>] Foo\nlet x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.kind(), ModuleOrNamespaceKind::NamedModule);
    let lists: Vec<usize> = module
        .attributes()
        .map(|l| l.attributes().count())
        .collect();
    assert_eq!(lists, vec![1], "one `[<RequireQualifiedAccess>]` list");
    let segs: Vec<String> = module
        .long_id()
        .expect("named module has a LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["Foo"], "attrs don't disturb the header name");
    assert!(
        matches!(module.decls().next(), Some(ModuleDecl::Let(_))),
        "the body `let` follows the header"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7e — a plain whole-file `module Foo` header carries no attributes.
#[test]
fn wholefile_module_no_attr_facade() {
    use crate::syntax::{AstNode, ImplFile};
    let source = "module Foo\nlet x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(module.attributes().count(), 0, "no header attributes");
    assert_lossless(source, &parse);
}

/// Phase 10.7e — `ModuleOrNamespace::attributes()` is empty for a non-named
/// module. An AnonModule's file-scope body recovery can leave a bare
/// `ATTRIBUTE_LIST` as a direct child (`[<A>] open System`, a deferred carrier);
/// it is *not* a header attribute. A `namespace` likewise carries none.
#[test]
fn non_named_module_attributes_empty() {
    use crate::syntax::{AstNode, ImplFile, ModuleOrNamespaceKind};
    // AnonModule with a body-recovery bare ATTRIBUTE_LIST.
    let anon = parse("[<A>] open System\n");
    let anon_mod = ImplFile::cast(anon.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(anon_mod.kind(), ModuleOrNamespaceKind::Anon);
    assert_eq!(
        anon_mod.attributes().count(),
        0,
        "the body recovery `[<A>]` is not a header attribute of the AnonModule"
    );
    // A namespace carries no attributes.
    let ns = parse("namespace Foo\nlet x = 1\n");
    let ns_mod = ImplFile::cast(ns.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert_eq!(ns_mod.kind(), ModuleOrNamespaceKind::DeclaredNamespace);
    assert_eq!(
        ns_mod.attributes().count(),
        0,
        "namespace has no attributes"
    );
}

/// Phase 9.6 — a negative enum value (`B = -1`). The adjacent sign-fold pass
/// (`sign_fold`) merges `-1` into one signed literal token, so the value is a
/// `Const` matching FCS (the former prefix-`App` divergence is gone). Pins the
/// folded shape from the typed facade; the FCS diff lives in
/// `diff_ast_enum_negative_value`.
#[test]
fn enum_negative_value_parses() {
    use crate::syntax::{AstNode, Expr, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type T = A = 0 | B = -1\n";
    let parse = parse(source); // must not panic
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::Enum(e)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an Enum repr");
    };
    let cases: Vec<_> = e.cases().collect();
    assert_eq!(cases.len(), 2, "both cases parse");
    assert!(
        matches!(cases[1].value(), Some(Expr::Const(_))),
        "the folded `-1` value is a Const, not a prefix-minus App"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.6 — repr-level visibility on an *enum* is an FCS error
/// (`parsEnumTypesCannotHaveVisibilityDeclarations`), unlike on a union
/// (`type T = private A | B`, valid). The modifier is consumed but, once the
/// body is known to be an enum, the error is recorded.
#[test]
fn enum_repr_visibility_is_error() {
    let source = "type E = private A = 0\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "repr-level visibility on an enum should record an error"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.6 — an enum value is FCS's `atomicExpr`, so a non-atomic value is an
/// error: a control-flow value (`A = if …`) is rejected outright, and a
/// whitespace application (`A = f x`) parses the atomic head `f` and leaves `x`
/// for the enclosing loop to reject (FCS accepts only the *high-precedence*
/// application `f(x)`, not `f x`). Neither panics.
#[test]
fn enum_nonatomic_value_is_error() {
    for source in ["type E = A = if true then 1 else 0\n", "type E = A = f x\n"] {
        let parse = parse(source); // must not panic
        assert!(
            !parse.errors.is_empty(),
            "a non-atomic enum value must be rejected: {source:?}"
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 9.6 — sign-folding (`sign_fold`) merges an *adjacent* sign on a
/// *foldable numeric literal* into one signed literal token, so `A = -1` /
/// `A = +1` reach the parser as a single `Const` and are accepted. Anything
/// else keeps the `+`/`-` as an `Op` token, which FCS rejects as a
/// non-`atomicExpr` value (`Unexpected symbol`/`prefix operator in union
/// case`): a *spaced* sign (`A = - 1`), a non-numeric operand (`A = -foo` /
/// `A = +foo`), an unsigned suffix (`A = -1uy`, outside FCS's fold set), or a
/// *trailing* atom after a folded literal (`A = -1 2` / `A = -1(2)`, since the
/// value is one `atomicExpr`). None panic.
#[test]
fn enum_signed_value_gating() {
    for ok in ["type E = A = -1\n", "type E = A = +1\n"] {
        let parse = parse(ok); // must not panic
        assert!(parse.errors.is_empty(), "{ok:?} errors: {:?}", parse.errors);
        assert_lossless(ok, &parse);
    }
    for bad in [
        "type E = A = - 1\n",
        "type E = A = -foo\n",
        "type E = A = +foo\n",
        // Unsigned suffix is outside FCS's fold set, so the `-` stays an `Op`.
        "type E = A = -1uy\n",
        // A folded literal is one atom; a *following* atom is not part of the
        // value (`atomicExpr`), so FCS rejects these and we must too.
        "type E = A = -1 2\n",
        "type E = A = -1(2)\n",
    ] {
        let parse = parse(bad); // must not panic
        assert!(
            !parse.errors.is_empty(),
            "a non-adjacent / non-numeric sign or trailing atom must be rejected: {bad:?}"
        );
        assert_lossless(bad, &parse);
    }
}

/// Phase 9.6 — `atomicExpr` is self-recursive on `HIGH_PRECEDENCE_PAREN_APP`
/// (`pars.fsy:5247`), so a high-precedence paren application `f(1)` is a valid
/// enum value (FCS accepts it). We consume it as part of the value — the same
/// `APP_EXPR > [head, ⟨HPA marker⟩, paren]` shape the app-expression parser
/// builds — rather than leaving `(1)` to misparse as a separate declaration.
#[test]
fn enum_high_precedence_paren_app_value_parses() {
    let source = "type E = A = f(1)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);
}

/// Phase 9.6 — a mixed group (`A = 0 | B | C`) reports
/// `parsAllEnumFieldsRequireValues` *once per value-less case*, anchored on that
/// case's name span (matching FCS's `SynUnionCase.range`), rather than a single
/// zero-width error at EOF. The LSP maps parser spans straight to ranges, so the
/// user is pointed at the case that needs a value.
#[test]
fn enum_mixed_error_anchored_on_cases() {
    let source = "type E = A = 0 | B | C\n";
    let parse = parse(source);
    let value_errs: Vec<_> = parse
        .errors
        .iter()
        .filter(|e| e.message == "all enum cases must be given values")
        .collect();
    assert_eq!(value_errs.len(), 2, "one error per value-less case");
    // Each error points at the offending case name (`B`, then `C`) — never EOF.
    let slices: Vec<&str> = value_errs.iter().map(|e| &source[e.span.clone()]).collect();
    assert_eq!(slices, vec!["B", "C"], "errors anchored on the cases");
    assert_lossless(source, &parse);
}

/// Phase 9.3b — green shape of an inside-`<>` constraint clause. The
/// `TYPAR_CONSTRAINTS` node nests inside `TYPAR_DECLS` (before `GREATER_TOK`),
/// each `TYPAR_CONSTRAINT` carrying the subject `TYPAR_DECL` then the
/// operator/keyword tokens. Inter-token spaces are siblings (lossless).
#[test]
fn typar_constraint_green_shape() {
    let source = "type T<'a when 'a : comparison> = 'a list\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..42
  MODULE_OR_NAMESPACE@0..42
    TYPE_DEFNS@0..41
      TYPE_DEFN@0..41
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        TYPAR_DECLS@6..31
          ERROR@6..6 \"\"
          LESS_TOK@6..7 \"<\"
          TYPAR_DECL@7..9
            QUOTE_TOK@7..8 \"'\"
            IDENT_TOK@8..9 \"a\"
          TYPAR_CONSTRAINTS@9..30
            WHITESPACE@9..10 \" \"
            WHEN_TOK@10..14 \"when\"
            TYPAR_CONSTRAINT@14..30
              TYPAR_DECL@14..17
                WHITESPACE@14..15 \" \"
                QUOTE_TOK@15..16 \"'\"
                IDENT_TOK@16..17 \"a\"
              WHITESPACE@17..18 \" \"
              COLON_TOK@18..19 \":\"
              WHITESPACE@19..20 \" \"
              IDENT_TOK@20..30 \"comparison\"
          GREATER_TOK@30..31 \">\"
        WHITESPACE@31..32 \" \"
        EQUALS_TOK@32..33 \"=\"
        WHITESPACE@33..34 \" \"
        ERROR@34..34 \"\"
        TYPE_ABBREV@34..41
          APP_TYPE@34..41
            VAR_TYPE@34..36
              QUOTE_TOK@34..35 \"'\"
              IDENT_TOK@35..36 \"a\"
            LONG_IDENT_TYPE@36..41
              LONG_IDENT@36..41
                WHITESPACE@36..37 \" \"
                IDENT_TOK@37..41 \"list\"
        ERROR@41..41 \"\"
    NEWLINE@41..42 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.3b — the typed facade: `TypeDefn::constraints()` yields every
/// constraint (inside-`<>` and after-decls), each exposing its subject typar
/// and kind.
#[test]
fn typar_constraint_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TyparConstraintKind};
    let defn = |source: &str| {
        let parse = parse(source);
        assert!(parse.errors.is_empty(), "{source:?}: {:?}", parse.errors);
        let module = ImplFile::cast(parse.root)
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        t.defns().next().expect("one defn")
    };

    // Inside-`<>` single constraint.
    let d = defn("type T<'a when 'a : comparison> = 'a list\n");
    let cs: Vec<_> = d.constraints().collect();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].typar().and_then(|t| t.ident()).unwrap().text(), "a");
    assert_eq!(cs[0].kind(), Some(TyparConstraintKind::Comparable));

    // `and`-chain — two constraints, in order.
    let d = defn("type T<'a when 'a : comparison and 'a : equality> = 'a list\n");
    let kinds: Vec<_> = d.constraints().map(|c| c.kind()).collect();
    assert_eq!(
        kinds,
        vec![
            Some(TyparConstraintKind::Comparable),
            Some(TyparConstraintKind::Equatable)
        ]
    );

    // After-decls position is collected too.
    let d = defn("type T<'a> when 'a : unmanaged = 'a list\n");
    let cs: Vec<_> = d.constraints().collect();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].kind(), Some(TyparConstraintKind::Unmanaged));

    // `not struct` / `not null` read the leading `not` ident.
    let d = defn("type T<'a when 'a : not struct> = 'a list\n");
    assert_eq!(
        d.constraints().next().unwrap().kind(),
        Some(TyparConstraintKind::ReferenceType)
    );
    let d = defn("type T<'a when 'a : not null> = 'a list\n");
    assert_eq!(
        d.constraints().next().unwrap().kind(),
        Some(TyparConstraintKind::NotSupportsNull)
    );

    // A backticked constraint keyword is classified by its de-quoted text.
    let d = defn("type T<'a when 'a : ``comparison``> = 'a list\n");
    assert_eq!(
        d.constraints().next().unwrap().kind(),
        Some(TyparConstraintKind::Comparable)
    );
    let d = defn("type T<'a when 'a : ``not`` null> = 'a list\n");
    assert_eq!(
        d.constraints().next().unwrap().kind(),
        Some(TyparConstraintKind::NotSupportsNull)
    );
}

/// Phase 9.3b — the subtype constraint `'a :> T` reports `SubtypeOf` and
/// carries the constraint type.
#[test]
fn typar_constraint_subtype_carries_type() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TyparConstraintKind};
    let source = "type T<'a when 'a :> System.IDisposable> = 'a list\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let c = t
        .defns()
        .next()
        .unwrap()
        .constraints()
        .next()
        .expect("one constraint");
    assert_eq!(c.kind(), Some(TyparConstraintKind::SubtypeOf));
    assert!(c.ty().is_some(), "a `:>` constraint carries its type");
    assert_lossless(source, &parse);
}

/// Phase 9.3b — deferred constraint forms (`default 'a : t`) and an unknown
/// ident constraint are recoverable errors, never panics. Pins the bound
/// documented on `TYPAR_CONSTRAINT`. (The SRTP `(member …)` form is now
/// supported — see `parser_diff_when_constraints`'s `diff_srtp_*` tests; the
/// `enum<…>` / `delegate<…>` forms are now supported too — see the
/// `diff_enum_*` / `diff_delegate_*` tests.)
#[test]
fn typar_constraint_deferred_forms_error_without_panic() {
    for source in [
        "type T<'a when default 'a : int> = 'a\n",
        "type T<'a when 'a : someUnknownThing> = 'a\n",
        // An SRTP member constraint with no member introducer (FCS's
        // `memberSpecFlags`) stays on the error path: a bare name, or an
        // access-only prefix — neither is a `static`/`member`/`abstract`/`new`.
        // (A lone `static` *is* a valid introducer — `(static Zero : …)` — and an
        // operator name is a follow-up; both are covered by `parser_diff_*`.)
        "type T<'a when 'a : (Zero : 'a)> = 'a\n",
        "type T<'a when 'a : (public Zero : 'a)> = 'a\n",
        // A parenthesised `typeAlts` subject admits *only* a `(member …)` RHS —
        // an ordinary `struct` / `null` / `enum<…>` constraint after it is an FCS
        // parse error. (A concrete-type alternative such as `(^T or int)` is now
        // *accepted* — FCS's `typeAlts` operands are `appTypeWithoutNull` — see
        // `parser_diff_when_constraints`'s `diff_srtp_general_type_alt_*` tests.)
        "type T< ^a, ^b when (^a or ^b) : struct> = class end\n",
    ] {
        let parse = parse(source); // must not panic
        assert!(
            !parse.errors.is_empty(),
            "a deferred/unknown constraint must be rejected: {source:?}"
        );
        assert_lossless(source, &parse);
    }
}

/// The two SRTP supports take *different* alternative operands, and FCS is strict
/// about it: the member *constraint*'s `typeAlts` (`pars.fsy:2705`) takes
/// `appTypeWithoutNull`, so a `| null` alternative is a parse error there
/// ("Unexpected symbol '|' (directly before 'null') in type name") — even though
/// the trait-call *expression*'s `typarAlts` takes `appTypeCanBeNullable` and
/// accepts exactly that (`parser_diff_trait_call`'s `diff_trait_call_alts_nullable`).
/// Pins the asymmetry: the constraint side must keep rejecting, and must not
/// panic or silently absorb the suffix.
#[test]
fn srtp_constraint_support_alternative_rejects_nullable() {
    let source = "type C< ^T when (^T or string | null) : (static member A: int)> = class end\n";
    let parse = parse(source); // must not panic
    assert!(
        !parse.errors.is_empty(),
        "a `| null` constraint alternative is an FCS parse error: {source:?}"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.7 — green-tree shape of the canonical instance member. The
/// `OBJECT_MODEL_REPR` holds one `MEMBER_DEFN` (`[MEMBER_TOK, BINDING]`); the
/// binding's head is a dotted `LONG_IDENT_PAT` (`this.M`) and its RHS is the
/// shared `= <expr>` machinery. The member's own RHS-close `OBLOCKEND` is a
/// zero-width `ERROR` inside `OBJECT_MODEL_REPR`; the body-closing `OBLOCKEND`
/// is the trailing `ERROR` owned by the `TYPE_DEFN`.
#[test]
fn member_green_shape() {
    let source = "type T =\n  member this.M = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..29
  MODULE_OR_NAMESPACE@0..29
    TYPE_DEFNS@0..29
      TYPE_DEFN@0..29
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        NEWLINE@8..9 \"\\n\"
        WHITESPACE@9..11 \"  \"
        ERROR@11..11 \"\"
        OBJECT_MODEL_REPR@11..29
          MEMBER_DEFN@11..28
            MEMBER_TOK@11..17 \"member\"
            BINDING@17..28
              LONG_IDENT_PAT@17..24
                WHITESPACE@17..18 \" \"
                LONG_IDENT@18..24
                  IDENT_TOK@18..22 \"this\"
                  DOT_TOK@22..23 \".\"
                  IDENT_TOK@23..24 \"M\"
              WHITESPACE@24..25 \" \"
              EQUALS_TOK@25..26 \"=\"
              WHITESPACE@26..27 \" \"
              ERROR@27..27 \"\"
              CONST_EXPR@27..28
                INT32_LIT@27..28 \"1\"
          NEWLINE@28..29 \"\\n\"
          ERROR@29..29 \"\"
        ERROR@29..29 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// An `inline` member — FCS's `memberCore` is `opt_inline bindingPattern …`,
/// so the `inline` sits between the `member` keyword and the head pattern
/// (`member inline this.M = 1`). The `INLINE_TOK` lands *inside* the member's
/// `BINDING` (ahead of the head `LONG_IDENT_PAT`), where `Binding::is_inline`
/// reads it back as FCS's `SynBinding.isInline`. No errors.
#[test]
fn member_inline_green_shape() {
    use crate::syntax::{AstNode, Binding};

    let source = "type T =\n  member inline this.M = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..36
  MODULE_OR_NAMESPACE@0..36
    TYPE_DEFNS@0..36
      TYPE_DEFN@0..36
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        NEWLINE@8..9 \"\\n\"
        WHITESPACE@9..11 \"  \"
        ERROR@11..11 \"\"
        OBJECT_MODEL_REPR@11..36
          MEMBER_DEFN@11..35
            MEMBER_TOK@11..17 \"member\"
            BINDING@17..35
              WHITESPACE@17..18 \" \"
              INLINE_TOK@18..24 \"inline\"
              LONG_IDENT_PAT@24..31
                WHITESPACE@24..25 \" \"
                LONG_IDENT@25..31
                  IDENT_TOK@25..29 \"this\"
                  DOT_TOK@29..30 \".\"
                  IDENT_TOK@30..31 \"M\"
              WHITESPACE@31..32 \" \"
              EQUALS_TOK@32..33 \"=\"
              WHITESPACE@33..34 \" \"
              ERROR@34..34 \"\"
              CONST_EXPR@34..35
                INT32_LIT@34..35 \"1\"
          NEWLINE@35..36 \"\\n\"
          ERROR@36..36 \"\"
        ERROR@36..36 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);

    // The `inline` flag is reachable via the binding facade.
    let binding = parse
        .root
        .descendants()
        .find_map(Binding::cast)
        .expect("a member BINDING");
    assert!(binding.is_inline(), "member binding should be is_inline");
}

/// A return-type annotation on a member head — `member this.M : int = 1`.
/// FCS's `memberCore` shares `localBinding`'s return-info production, so the
/// type lands in a `BINDING_RETURN_INFO > [COLON_TOK, <type>]` child of the
/// member's `BINDING`, between the head `LONG_IDENT_PAT` and `=`. No errors.
#[test]
fn member_return_type_green_shape() {
    let source = "type T =\n  member this.M : int = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..35
  MODULE_OR_NAMESPACE@0..35
    TYPE_DEFNS@0..35
      TYPE_DEFN@0..35
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        WHITESPACE@6..7 \" \"
        EQUALS_TOK@7..8 \"=\"
        NEWLINE@8..9 \"\\n\"
        WHITESPACE@9..11 \"  \"
        ERROR@11..11 \"\"
        OBJECT_MODEL_REPR@11..35
          MEMBER_DEFN@11..34
            MEMBER_TOK@11..17 \"member\"
            BINDING@17..34
              LONG_IDENT_PAT@17..24
                WHITESPACE@17..18 \" \"
                LONG_IDENT@18..24
                  IDENT_TOK@18..22 \"this\"
                  DOT_TOK@22..23 \".\"
                  IDENT_TOK@23..24 \"M\"
              BINDING_RETURN_INFO@24..30
                WHITESPACE@24..25 \" \"
                COLON_TOK@25..26 \":\"
                WHITESPACE@26..27 \" \"
                LONG_IDENT_TYPE@27..30
                  LONG_IDENT@27..30
                    IDENT_TOK@27..30 \"int\"
              WHITESPACE@30..31 \" \"
              EQUALS_TOK@31..32 \"=\"
              WHITESPACE@32..33 \" \"
              ERROR@33..33 \"\"
              CONST_EXPR@33..34
                INT32_LIT@33..34 \"1\"
          NEWLINE@34..35 \"\\n\"
          ERROR@35..35 \"\"
        ERROR@35..35 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// A type-annotated get/set property head — `member this.P : int with get`
/// — is an FCS *parse error* ("Type annotations on property getters and
/// setters must be given after the accessor"). The return-info consumption is
/// placed after the `with` check, so the colon is consumed as a member return
/// type and the trailing `with` falls into the "expected `=`" recovery: we
/// error too (rather than parsing a divergent `GET_SET_MEMBER`), and the tree
/// stays lossless. Pins that boundary.
#[test]
fn type_annotated_get_set_head_is_error() {
    let source = "type C() =\n    member _.P : int with get() = 1\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "type-annotated get/set head should be an error (FCS rejects it)",
    );
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("expected `=` after binding pattern")),
        "the `with` should hit the `expected =` recovery; got: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// A getter without a parenthesised argument list (`with get = e`, missing the
/// `()`) is an FCS *parse error* — FS0557 "A getter property is expected to be a
/// function, e.g. 'get() = ...' or 'get(index) = ...'". We were too lenient,
/// silently accepting it as a valid `GET_SET_MEMBER`. Emit the diagnostic (the
/// tree stays lossless); recovery is unchanged. (Regression: corpus
/// `E_PropertyInvalidGetter01.fs` / `E_MissingArgumentForGetterProp01.fs`.)
#[test]
fn parenless_getter_is_error() {
    for source in [
        "type C() =\n    member this.P with get = 1\n",
        "type C() =\n    member this.P with get = 1 and set v = ()\n",
        // the `set` order swapped — still only the getter is flagged
        "type C() =\n    member this.P with set v = () and get = 1\n",
    ] {
        let parse = parse(source);
        let fs0557: Vec<_> = parse
            .errors
            .iter()
            .filter(|e| {
                e.message
                    .contains("getter property is expected to be a function")
            })
            .collect();
        assert_eq!(
            fs0557.len(),
            1,
            "{source:?}: expected exactly one FS0557, got: {:?}",
            parse.errors
        );
        assert_eq!(&source[fs0557[0].span.clone()], "get");
        assert_lossless(source, &parse);
    }
}

/// Negative control: a getter *with* `()` (or an indexer `get(i)`) is valid —
/// no FS0557. The setter `set v = e` (a bare value param, no parens) is also
/// valid and never triggers the getter error.
#[test]
fn paren_getter_and_setter_are_ok() {
    for source in [
        "type C() =\n    member this.P with get() = 1\n",
        "type C() =\n    member this.P with get(i) = i\n",
        "type C() =\n    member this.P with get() = 1 and set v = ()\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.iter().any(|e| e
                .message
                .contains("getter property is expected to be a function")),
            "{source:?}: a parenthesised getter must not trigger FS0557; got: {:?}",
            parse.errors
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 9.7 — facade: an `ObjectModel` repr exposes its members; each member
/// is a `MemberDefn::Member` wrapping a `SynBinding` whose head is a dotted
/// `LongIdentPat` (`this.M`) and whose RHS is the body expression.
#[test]
fn member_facade() {
    use crate::syntax::{AstNode, Expr, ImplFile, MemberDefn, ModuleDecl, Pat, TypeDefnRepr};
    let source = "type T =\n  member this.M = 1\n  member this.Add a b = a + b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    assert_eq!(members.len(), 2, "two members");

    // First member: `this.M` (no args), body `1`.
    let MemberDefn::Member(m0) = &members[0] else {
        panic!("first member is a method");
    };
    let b0 = m0.binding().expect("member binding");
    let Some(Pat::LongIdent(head0)) = b0.pat() else {
        panic!("member head is a LongIdentPat");
    };
    let segs0: Vec<_> = head0
        .head()
        .expect("head long-ident")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs0, ["this", "M"], "dotted self.member head");
    assert_eq!(head0.args().count(), 0, "no curried args");
    assert!(matches!(b0.expr(), Some(Expr::Const(_))), "body is `1`");

    // Second member: `this.Add a b`, two curried args.
    let MemberDefn::Member(m1) = &members[1] else {
        panic!("second member is a method");
    };
    let b1 = m1.binding().expect("member binding");
    let Some(Pat::LongIdent(head1)) = b1.pat() else {
        panic!("member head is a LongIdentPat");
    };
    let segs1: Vec<_> = head1
        .head()
        .expect("head long-ident")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs1, ["this", "Add"]);
    assert_eq!(head1.args().count(), 2, "two curried args");
    assert_lossless(source, &parse);
}

/// Phase 10.7f — `MemberMethod::attributes()` exposes the member's leading lists
/// (FCS's `SynBinding.attributes`); a plain member yields none.
#[test]
fn member_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source =
        "type T() =\n    [<A; B>]\n    [<C>]\n    member this.M() = 1\n    member this.N = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    // Members: the implicit ctor `()`, then the two methods.
    let methods: Vec<_> = members
        .iter()
        .filter_map(|m| match m {
            MemberDefn::Member(mm) => Some(mm),
            _ => None,
        })
        .collect();
    assert_eq!(methods.len(), 2, "two `member` methods");
    let attr0: Vec<usize> = methods[0]
        .attributes()
        .map(|l| l.attributes().count())
        .collect();
    assert_eq!(
        attr0,
        vec![2, 1],
        "`[<A; B>]` then `[<C>]` on the first member"
    );
    assert_eq!(
        methods[1].attributes().count(),
        0,
        "the plain member carries none"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7f — a get/set property's leading attribute is exposed via
/// `GetSetMember::attributes()` (FCS duplicates it onto both accessor bindings).
#[test]
fn member_attr_get_set_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T() =\n    [<A>] member this.G with get() = 1 and set v = ()\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let gsm = om
        .members()
        .find_map(|m| match m {
            MemberDefn::GetSetMember(g) => Some(g),
            _ => None,
        })
        .expect("a get/set member");
    assert_eq!(gsm.attributes().count(), 1, "one leading `[<A>]` list");
    assert!(gsm.getter().is_some() && gsm.setter().is_some());
    assert_lossless(source, &parse);
}

/// Phase 10.7f — attributes on the `interface` member carrier are not yet
/// attached: they are flagged (their own later slice) and the parse stays
/// lossless with the attrs as bare siblings. (The class-local `let` carrier is
/// now handled by 10.7l, abstract slots by 10.7g, auto-properties by 10.7h,
/// `val` fields by 10.7i.)
#[test]
fn member_attr_deferred_carrier_flagged() {
    // A virtual-only carrier as the *first* body item: the entry gate must
    // recognise the leading `[<` (an `InterfaceMember` virtual, invisible to the
    // raw classifier) so the member block is entered and the attribute flagged —
    // rather than mis-parsing the body as a bad type.
    let source = "type T() =\n    [<A>] interface I\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("phase-10.7 slice")),
        "expected a deferred-member diagnostic for {source:?}, got: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7l — a class-local `let` carries its leading `[<…>]` run on the
/// `MEMBER_LET_BINDINGS` node (FCS's head-`SynBinding.attributes`). Covers both
/// the same-line form (the `let` arrives as a raw `Token::Let` once the run is
/// consumed) and the offside `[<A>]⏎let` form (a `Virtual::Let` after a
/// `BlockSep`); the parse is error-free and lossless, and the facade exposes the
/// list. The `[<A>] let` as the *first* body item also exercises the entry gate
/// recognising the virtual-only carrier behind the leading `[<`.
#[test]
fn class_local_let_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    for source in [
        "type T() =\n    [<A>] let x = 1\n    member this.M() = x\n",
        "type T() =\n    [<A>]\n    let mutable x = 0\n    member this.M() = x\n",
        "type T() =\n    [<A>] static let x = 1\n    member this.M() = x\n",
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "errors for {source:?}: {:?}",
            parse.errors
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("expected an ObjectModel repr");
        };
        let lb = om
            .members()
            .find_map(|m| match m {
                MemberDefn::LetBindings(l) => Some(l),
                _ => None,
            })
            .expect("a class-local LetBindings member");
        assert_eq!(
            lb.attributes().count(),
            1,
            "one leading `[<A>]` list for {source:?}"
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 10.7g — `AbstractSlot::attributes()` exposes the slot's leading lists
/// (FCS's `SynValSig.attributes`); a plain abstract slot yields none.
#[test]
fn abstract_slot_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T =\n    [<A; B>] abstract member M : int\n    abstract N : string\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let slots: Vec<_> = om
        .members()
        .filter_map(|m| match m {
            MemberDefn::AbstractSlot(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(slots.len(), 2, "two abstract slots");
    let attr0: Vec<usize> = slots[0]
        .attributes()
        .map(|l| l.attributes().count())
        .collect();
    assert_eq!(
        attr0,
        vec![2],
        "one `[<A; B>]` list of two attributes on the first slot"
    );
    assert_eq!(
        slots[1].attributes().count(),
        0,
        "the plain slot carries none"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.7 — the wildcard self-identifier `member _.M` parses with the `_` as
/// the first head segment (FCS's `SynPat.LongIdent` `idText = "_"`). Pins the
/// facade head; the FCS diff lives in `diff_ast_member_wildcard_self`.
#[test]
fn member_wildcard_self_head() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, Pat, TypeDefnRepr};
    let source = "type T =\n  member _.M = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let MemberDefn::Member(m) = om.members().next().expect("one member") else {
        panic!("member is a method");
    };
    let Some(Pat::LongIdent(head)) = m.binding().expect("binding").pat() else {
        panic!("member head is a LongIdentPat");
    };
    let segs: Vec<_> = head
        .head()
        .expect("head long-ident")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(
        segs,
        ["_", "M"],
        "wildcard self-id is the first head segment"
    );
    assert_lossless(source, &parse);
}

/// *Adjacent* parenthesised member arguments (`member this.M()` /
/// `member this.M(x)`) parse via the shared HPA-aware curried-args sweep — the
/// `HighPrecedenceParenApp` virtual before the `(` is skipped so the paren
/// pattern is collected. The arg is a `PAREN_PAT` (FCS's `SynPat.Paren`); the
/// FCS diffs live in `diff_ast_member_paren_arg` / `diff_ast_member_unit_arg`.
#[test]
fn member_adjacent_paren_args_parse() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, Pat, TypeDefnRepr};
    let source = "type T =\n  member this.M(x) = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let MemberDefn::Member(m) = om.members().next().expect("one member") else {
        panic!("member is a method");
    };
    let Some(Pat::LongIdent(head)) = m.binding().expect("binding").pat() else {
        panic!("member head is a LongIdentPat");
    };
    let args: Vec<_> = head.args().collect();
    assert_eq!(args.len(), 1, "one curried arg");
    assert!(
        matches!(args[0], Pat::Paren(_)),
        "the arg is a paren pattern"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.8a — green-tree shape of an implicit constructor. The `IMPLICIT_CTOR`
/// is a child of `TYPE_DEFN` (parsed in the header, before the repr); the empty
/// `()` is a bare `CONST_PAT` owning the `(`/`)` (no `PAREN_PAT` wrapper).
#[test]
fn implicit_ctor_green_shape() {
    let source = "type T() =\n  member _.X = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..28
  MODULE_OR_NAMESPACE@0..28
    TYPE_DEFNS@0..28
      TYPE_DEFN@0..28
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        IMPLICIT_CTOR@6..8
          ERROR@6..6 \"\"
          CONST_PAT@6..8
            LPAREN_TOK@6..7 \"(\"
            RPAREN_TOK@7..8 \")\"
        WHITESPACE@8..9 \" \"
        EQUALS_TOK@9..10 \"=\"
        NEWLINE@10..11 \"\\n\"
        WHITESPACE@11..13 \"  \"
        ERROR@13..13 \"\"
        OBJECT_MODEL_REPR@13..28
          MEMBER_DEFN@13..27
            MEMBER_TOK@13..19 \"member\"
            BINDING@19..27
              LONG_IDENT_PAT@19..23
                WHITESPACE@19..20 \" \"
                LONG_IDENT@20..23
                  IDENT_TOK@20..21 \"_\"
                  DOT_TOK@21..22 \".\"
                  IDENT_TOK@22..23 \"X\"
              WHITESPACE@23..24 \" \"
              EQUALS_TOK@24..25 \"=\"
              WHITESPACE@25..26 \" \"
              ERROR@26..26 \"\"
              CONST_EXPR@26..27
                INT32_LIT@26..27 \"1\"
          NEWLINE@27..28 \"\\n\"
          ERROR@28..28 \"\"
        ERROR@28..28 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.8a — facade: `TypeDefn::implicit_ctor()` exposes the constructor; its
/// `args()` is the (paren) argument pattern and `self_id()` the `as` self-id.
#[test]
fn implicit_ctor_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, Pat};
    let source = "type T(x: int) as self =\n  member _.X = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defn = t.defns().next().expect("one defn");
    let ctor = defn.implicit_ctor().expect("an implicit constructor");
    assert!(
        matches!(ctor.args(), Some(Pat::Paren(_))),
        "ctor args are a paren pattern"
    );
    assert_eq!(ctor.self_id().expect("a self-id").text(), "self", "as self");
    assert_lossless(source, &parse);
}

/// Phase 10.7j — facade: `ImplicitCtor::attributes()` exposes the ctor's leading
/// lists (FCS's `ImplicitCtor.attributes`); a plain ctor yields none. The
/// attribute does not leak into the type-header `TypeDefn::attributes()`.
#[test]
fn implicit_ctor_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "type T [<A; B>] (x: int) = class end\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defn = t.defns().next().expect("one defn");
    let ctor = defn.implicit_ctor().expect("an implicit constructor");
    let attrs: Vec<usize> = ctor.attributes().map(|l| l.attributes().count()).collect();
    assert_eq!(
        attrs,
        vec![2],
        "one `[<A; B>]` list of two attributes on the ctor"
    );
    assert_eq!(
        defn.attributes().count(),
        0,
        "the post-name ctor attribute must not appear as a type-header attribute"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7j — a plain (unattributed) primary constructor yields no ctor
/// attribute lists.
#[test]
fn implicit_ctor_no_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "type T (x: int) = class end\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let ctor = t
        .defns()
        .next()
        .expect("one defn")
        .implicit_ctor()
        .expect("an implicit constructor");
    assert_eq!(ctor.attributes().count(), 0, "no attribute lists");
    assert_lossless(source, &parse);
}

/// Phase 9.8a — a definition without a primary constructor has no
/// `implicit_ctor()` (the 9.7 plain `type T = member …` form).
#[test]
fn no_implicit_ctor_when_absent() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "type T =\n  member this.M = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    assert!(
        t.defns()
            .next()
            .expect("one defn")
            .implicit_ctor()
            .is_none(),
        "no primary constructor"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.8a — a primary constructor on a non-class repr (`type R(x) = {…}` /
/// `type A(x) = int`) records FCS's "Only class types may take value arguments"
/// (recoverable, no panic), while keeping the ctor and the repr (lossless),
/// matching FCS's recovery.
#[test]
fn implicit_ctor_on_non_class_repr_is_error() {
    for source in ["type R(x: int) = { X: int }\n", "type A(x: int) = int\n"] {
        let parse = parse(source); // must not panic
        assert!(
            parse.errors.iter().any(|e| e
                .message
                .contains("Only class types may take value arguments")),
            "expected the value-arguments diagnostic for {source:?}: {:?}",
            parse.errors
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 9.8a — an accessibility modifier on the constructor (`type C private
/// (x) = …`) parses cleanly (no error) and still exposes the ctor.
#[test]
fn implicit_ctor_with_access_parses() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "type C private (x: int) =\n  member _.X = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    assert!(
        t.defns()
            .next()
            .expect("one defn")
            .implicit_ctor()
            .is_some(),
        "the private ctor is present"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.8b — facade: a class-local `let` is a `MemberDefn::LetBindings` in
/// the object-model repr, exposing its bindings and `is_rec`.
#[test]
fn class_local_let_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T() =\n  let rec a = 1\n  and b = 2\n  member _.S = a\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    assert_eq!(members.len(), 2, "a LetBindings then a Member");
    let MemberDefn::LetBindings(lb) = &members[0] else {
        panic!("first member is class-local let bindings");
    };
    assert!(lb.is_rec(), "let rec");
    assert_eq!(lb.bindings().count(), 2, "the `and`-chained pair");
    assert!(
        matches!(&members[1], MemberDefn::Member(_)),
        "the trailing member"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.8c — facade: a `static let` is the same `MemberDefn::LetBindings` as a
/// class-local `let` (FCS's `STATIC classDefnBindings`, `pars.fsy:2009`), with
/// `is_static()` reporting the leading `STATIC_TOK`.
#[test]
fn static_class_local_let_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T() =\n  static let x = 1\n  member _.X = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    assert_eq!(members.len(), 2, "a LetBindings then a Member");
    let MemberDefn::LetBindings(lb) = &members[0] else {
        panic!("first member is static class-local let bindings");
    };
    assert!(lb.is_static(), "static let");
    assert!(!lb.is_rec(), "not rec");
    assert_eq!(lb.bindings().count(), 1, "a single binding");
    assert!(
        matches!(&members[1], MemberDefn::Member(_)),
        "the trailing member"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.8c — facade: a `static let rec … and …` group is a static
/// `MemberDefn::LetBindings` (`is_static() && is_rec()`) carrying the
/// `and`-chained pair, exactly like the non-static `let rec` form.
#[test]
fn static_class_local_let_rec_and_chain_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T() =\n  static let rec a = 1\n  and b = 2\n  member _.S = a\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    let MemberDefn::LetBindings(lb) = &members[0] else {
        panic!("first member is static class-local let bindings");
    };
    assert!(lb.is_static(), "static let rec");
    assert!(lb.is_rec(), "let rec");
    assert_eq!(lb.bindings().count(), 2, "the `and`-chained pair");
    assert_lossless(source, &parse);
}

/// Phase 9.8d — facade: a class-body `do <expr>` is a `MemberDefn::Do` in the
/// object-model repr, exposing its body expression and (here false)
/// `is_static()`. The reused `DO_EXPR` is held under the `MEMBER_DO`; the facade
/// digs through to the body. A `do`-only body is what marks the type a class, so
/// the primary constructor parses cleanly (no "Only class types …" error).
#[test]
fn class_do_facade() {
    use crate::syntax::{AstNode, Expr, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T(c: int) =\n    do printfn \"%d\" c\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    assert_eq!(members.len(), 1, "a single Do member");
    let MemberDefn::Do(d) = &members[0] else {
        panic!("the member is a `do` binding");
    };
    assert!(!d.is_static(), "a plain `do`, not `static do`");
    assert!(
        matches!(d.expr(), Some(Expr::App(_))),
        "the `do` body is the `printfn …` application",
    );
    assert_lossless(source, &parse);
}

/// Phase 9.8d — facade: a `static do` is the same `MemberDefn::Do` with
/// `is_static()` reporting the leading `STATIC_TOK` (FCS's `StaticDo`).
#[test]
fn class_static_do_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T() =\n    static do printfn \"hi\"\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    assert_eq!(members.len(), 1, "a single Do member");
    let MemberDefn::Do(d) = &members[0] else {
        panic!("the member is a `do` binding");
    };
    assert!(d.is_static(), "a `static do`");
    assert_lossless(source, &parse);
}

/// Phase 9.8d — a `do` interleaved with a `let` and a `member` keeps the member
/// list shape: `let`, `do`, `member` are three distinct object-model items (the
/// `do`'s offside terminator must not swallow the following `member`).
#[test]
fn class_let_do_member_sequence() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T(c: int) =\n    let x = c\n    do ignore x\n    member _.X = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    assert_eq!(members.len(), 3, "let, do, member");
    assert!(matches!(&members[0], MemberDefn::LetBindings(_)), "the let");
    assert!(matches!(&members[1], MemberDefn::Do(_)), "the do");
    assert!(matches!(&members[2], MemberDefn::Member(_)), "the member");
    assert_lossless(source, &parse);
}

/// Phase 9.8d — a layout break between `static` and `do` (`static`⏎`do`) is a
/// recovered error, not a misparse: the LexFilter only relabels the raw
/// `Token::Do` to `Virtual::Do` when adjacent to `static`, so the break leaves
/// an `OBLOCKSEP` before the `Virtual::Do` (FCS reports "Incomplete structured
/// construct" — `ParseHadErrors`). The raw classifier is virtual-blind and still
/// selects the `static do` arm, so the guard records an error and recovers with
/// a bare `STATIC_TOK`-only `MEMBER_DO`, the trailing `do` re-parsing as a plain
/// class-body `do` member. Without the guard `parse_do_expr` would build a
/// malformed nested `DO_EXPR` around the non-adjacent `do`. Mirrors
/// [`static_then_offside_let_is_recovered_error`].
#[test]
fn static_then_offside_do_is_recovered_error() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T() =\n    static\n    do printfn \"hi\"\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "`static`⏎`do` is incomplete in FCS and must error, not parse a clean \
         static do; got no errors",
    );
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    // Recovery: a bare `static` Do member, then the re-classified plain `do`.
    let members: Vec<_> = om.members().collect();
    assert_eq!(
        members.len(),
        2,
        "the recovered `static` then the plain `do`"
    );
    let MemberDefn::Do(stale) = &members[0] else {
        panic!("the recovered bare `static` is a Do member");
    };
    assert!(stale.is_static(), "the bare recovery carries the `static`");
    assert!(
        stale.expr().is_none(),
        "the bare recovery has no do-body (the `do` re-parsed separately)",
    );
    let MemberDefn::Do(plain) = &members[1] else {
        panic!("the trailing `do` re-parsed as a plain Do member");
    };
    assert!(!plain.is_static(), "the re-parsed `do` is not static");
    assert!(
        plain.expr().is_some(),
        "the re-parsed `do` carries its body"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.8c — a layout break between `static` and `let` (`static`⏎`let`) is a
/// recovered error, not a panic: the LexFilter leaves an `OBLOCKSEP` before the
/// `Virtual::Let`, so the static binding is incomplete (FCS reports "Incomplete
/// structured construct"). The parser records an error and recovers, the
/// trailing `let` re-parsing as a plain class-local `let`. Pins the guard that
/// keeps `parse_let_decl_at`'s let-at-cursor invariant from tripping.
#[test]
fn static_then_offside_let_is_recovered_error() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T() =\n    static\n    let x = 1\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a layout-broken `static`⏎`let` is an error"
    );
    // Recovery still yields a parseable tree: the trailing `let x = 1` lands as a
    // (non-static) class-local `let`.
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let trailing_let = om.members().any(|m| {
        matches!(&m, MemberDefn::LetBindings(lb) if !lb.is_static() && lb.bindings().count() == 1)
    });
    assert!(
        trailing_let,
        "the offside `let x = 1` recovers as a class-local let"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.9a — green shape + facade of a `static member`: a `MEMBER_DEFN` with
/// a leading `STATIC_TOK` before `MEMBER_TOK`, and a single-segment head
/// (`["M"]`, no self-id). `MemberMethod::is_static()` reports the `static`.
#[test]
fn static_member_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, Pat, TypeDefnRepr};
    let source = "type T =\n  static member M = 1\n  member this.N = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    assert_eq!(members.len(), 2);

    // First: `static member M` — static, single-segment head.
    let MemberDefn::Member(m0) = &members[0] else {
        panic!("first member is a method");
    };
    assert!(m0.is_static(), "static member");
    let Some(Pat::LongIdent(head0)) = m0.binding().expect("binding").pat() else {
        panic!("head is a LongIdentPat");
    };
    let segs0: Vec<_> = head0
        .head()
        .expect("head long-ident")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs0, ["M"], "static member head has no self-id");

    // Second: `member this.N` — instance, dotted head.
    let MemberDefn::Member(m1) = &members[1] else {
        panic!("second member is a method");
    };
    assert!(!m1.is_static(), "instance member");
    assert_lossless(source, &parse);
}

/// A `static member` whose name is a *lowercase* single identifier with no
/// curried arguments and no self-id is a property-style value member: FCS routes
/// it through the same `mkSynPatMaybeVar` classifier as a `let` value binding and
/// produces `SynPat.Named`, not `SynPat.LongIdent`. We mirror that — the head is
/// a `NAMED_PAT`. (Regression: corpus `static member testCases: Object[][]` in
/// the `FSharp.Editor.Tests` `*ServiceTests` files.)
#[test]
fn static_member_lowercase_value_head_is_named() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, Pat, TypeDefnRepr};
    // Covers: bare `= e`, a return-type `: T = e`, and a backtick-quoted name —
    // all `Named` in FCS. The value (RHS) is irrelevant to the head shape.
    for source in [
        "type T =\n    static member foo = 1\n",
        "type T =\n    static member foo: int = 1\n",
        "type T =\n    static member ``foo bar``: int = 1\n",
        "type T =\n    static member ``\u{0345}``: int = 1\n",
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} errors: {:?}",
            parse.errors
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("expected an ObjectModel repr");
        };
        let m = om.members().next().expect("one member");
        let MemberDefn::Member(m) = &m else {
            panic!("a method member")
        };
        assert!(
            matches!(m.binding().expect("binding").pat(), Some(Pat::Named(_))),
            "{source:?}: head must be a NAMED_PAT, got {:?}\n{}",
            m.binding().and_then(|b| b.pat()),
            debug_tree(&parse.root)
        );
        assert_lossless(source, &parse);
    }
}

/// The `NAMED_PAT` choice for a lowercase value member must *not* fire when the
/// name is uppercase (a potential literal/constructor → `LongIdent`), has curried
/// arguments (function-form → `LongIdent`), or has a self-id (instance member,
/// dotted `LongIdent`). All three stay `LONG_IDENT_PAT`, matching FCS.
#[test]
fn member_long_ident_head_cases_unchanged() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, Pat, TypeDefnRepr};
    for source in [
        "type T =\n    static member Foo = 1\n",   // uppercase name
        "type T =\n    static member foo x = 1\n", // curried arg
        "type T() =\n    member this.foo = 1\n",   // self-id (instance)
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} errors: {:?}",
            parse.errors
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("expected an ObjectModel repr");
        };
        let m = om.members().next().expect("one member");
        let MemberDefn::Member(m) = &m else {
            panic!("a method member")
        };
        assert!(
            matches!(m.binding().expect("binding").pat(), Some(Pat::LongIdent(_))),
            "{source:?}: head must stay a LONG_IDENT_PAT, got {:?}\n{}",
            m.binding().and_then(|b| b.pat()),
            debug_tree(&parse.root)
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 9.10a — `override`/`default` members parse into the *same*
/// `MEMBER_DEFN`/`SynMemberDefn.Member` node as a plain `member`, carrying the
/// `Override`/`Default` leading keyword (no `member` keyword token). The head is
/// the usual dotted `this.M`.
#[test]
fn override_default_member_facade() {
    use crate::syntax::{
        AstNode, ImplFile, MemberDefn, MemberLeading, ModuleDecl, Pat, TypeDefnRepr,
    };
    for (source, expected) in [
        ("type T =\n  override this.M = 1\n", MemberLeading::Override),
        ("type T =\n  default this.M = 1\n", MemberLeading::Default),
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} errors: {:?}",
            parse.errors
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("expected an ObjectModel repr");
        };
        let MemberDefn::Member(m) = om.members().next().expect("one member") else {
            panic!("{source:?} should be a member method");
        };
        assert_eq!(m.leading_keyword(), expected, "{source:?} leading keyword");
        assert!(!m.is_static(), "{source:?} not static");
        let Some(Pat::LongIdent(head)) = m.binding().expect("binding").pat() else {
            panic!("head is a LongIdentPat");
        };
        let segs: Vec<_> = head
            .head()
            .expect("head long-ident")
            .idents()
            .map(|t| t.text().to_string())
            .collect();
        assert_eq!(segs, ["this", "M"], "{source:?} dotted self.member head");
        assert_lossless(source, &parse);
    }
}

/// Phase 9.10b — an explicit constructor `new(a) = …` parses into a
/// `MEMBER_DEFN`/`SynMemberDefn.Member` whose head is the `new` keyword (a single
/// `LONG_IDENT` segment, read back as `"new"`) and whose leading keyword is
/// `New`. The argument is a `Paren` atomic pattern.
#[test]
fn new_ctor_facade() {
    use crate::syntax::{
        AstNode, ImplFile, MemberDefn, MemberLeading, ModuleDecl, Pat, TypeDefnRepr,
    };
    let source = "type T =\n  val x : int\n  new(a) = { x = a }\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    // The repr's second member is the `new` ctor (the first is the `val` field).
    let MemberDefn::Member(m) = om.members().nth(1).expect("a second member") else {
        panic!("the `new` ctor should be a MemberDefn::Member");
    };
    assert_eq!(
        m.leading_keyword(),
        MemberLeading::New,
        "leading keyword is New"
    );
    assert!(!m.is_static(), "a ctor is not static");
    let Some(Pat::LongIdent(head)) = m.binding().expect("binding").pat() else {
        panic!("head is a LongIdentPat");
    };
    let segs: Vec<_> = head
        .head()
        .expect("head long-ident")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, ["new"], "the ctor head is the `new` keyword");
    assert!(
        matches!(head.args().next(), Some(Pat::Paren(_))),
        "the ctor arg is a paren pattern"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.10c — `abstract [member] M : T` parses into a `MemberDefn::AbstractSlot`
/// holding a `VAL_SIG` (name + `: <type>`, no `= <expr>` body); `is_abstract_member`
/// reports whether the `member` keyword was present.
#[test]
fn abstract_slot_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    for (source, member_kw) in [
        ("type T =\n  abstract M : int -> int\n", false),
        ("type T =\n  abstract member M : int -> int\n", true),
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} errors: {:?}",
            parse.errors
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("expected an ObjectModel repr");
        };
        let MemberDefn::AbstractSlot(a) = om.members().next().expect("one member") else {
            panic!("{source:?} should be a MemberDefn::AbstractSlot");
        };
        assert_eq!(
            a.is_abstract_member(),
            member_kw,
            "{source:?} `member` keyword presence"
        );
        let vs = a.val_sig().expect("a VAL_SIG");
        assert_eq!(
            vs.ident().expect("a name").text(),
            "M",
            "{source:?} slot name"
        );
        assert!(vs.ty().is_some(), "{source:?} has a signature type");
        assert_lossless(source, &parse);
    }
}

/// Phase 9.10c — the deferred abstract-slot head forms each produce a *clean,
/// lossless* error (never a panic or corruption — the correctness bar), rather
/// than a malformed slot: the value-signature-only explicit-typar forms the
/// type-header postfix parser doesn't model — the `, ..` flex list and the empty
/// `<>` — which the proper `SynValTyparDecls` parser handles in phase 10.12.
///
/// The common generic form (`abstract M<'U> : …`) *is* handled (see the diff
/// tests), as are operator (`abstract (+) : …`) and active-pattern
/// (`abstract (|Foo|_|) : …`) `nameop` heads; only these value-sig-specific
/// typar extensions defer.
#[test]
fn abstract_slot_deferred_heads_stay_lossless() {
    for source in [
        "type T =\n  abstract M<'T, ..> : 'T -> 'T\n",
        "type T =\n  abstract M<> : int\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?} is a deferred head form, so it should error"
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 9.11a — `inherit Base()` / `inherit Base` parse into a
/// `MemberDefn::Inherit`; `is_implicit()` distinguishes the `ImplicitInherit`
/// (args present) from the `Inherit` (no args) form, and `base_type()` /
/// `args()` expose the base type and the optional constructor arguments.
#[test]
fn inherit_member_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    for (source, implicit) in [
        ("type C() =\n  inherit Base()\n", true),
        ("type C() =\n  inherit Base\n", false),
        // The quoted `` ``base`` `` alias is the one error-free `as` form.
        ("type C() =\n  inherit Base() as ``base``\n", true),
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} errors: {:?}",
            parse.errors
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("expected an ObjectModel repr");
        };
        // The implicit ctor lives in slot 3, not the repr member list, so the
        // sole repr member here is the inherit clause.
        let MemberDefn::Inherit(inh) = om.members().next().expect("one member") else {
            panic!("{source:?} should be a MemberDefn::Inherit");
        };
        assert_eq!(
            inh.is_implicit(),
            implicit,
            "{source:?} implicit-inherit (args present)"
        );
        assert!(inh.base_type().is_some(), "{source:?} has a base type",);
        assert_eq!(
            inh.args().is_some(),
            implicit,
            "{source:?} args present iff implicit",
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 9.11a — an `as` binding on `inherit` is always an FCS parse error
/// (FS0564); we record it and still recover the `Inherit` member, staying
/// lossless. The alias-less `inherit Base() as` (no alias token) likewise
/// recovers without corruption.
#[test]
fn inherit_as_binding_errors_but_recovers() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    for source in [
        "type C() =\n  inherit Base() as base\n",
        "type C() =\n  inherit Base() as\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?} an `as` binding on inherit is an FCS error",
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("expected an ObjectModel repr");
        };
        assert!(
            matches!(om.members().next(), Some(MemberDefn::Inherit(_))),
            "{source:?} still recovers an Inherit member",
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 9.11b — `interface I` / `interface I with member …` parse into a
/// `MemberDefn::Interface`; `has_with()` distinguishes the with-block form (FCS's
/// `members = Some`) from the bare form (`None`), `interface_type()` exposes the
/// implemented interface, and `members()` yields the nested member list.
#[test]
fn interface_member_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    // (source, has_with, member_count)
    for (source, has_with, members) in [
        ("type C() =\n  inherit obj()\n  interface I\n", false, 0),
        (
            "type C() =\n  interface I with\n    member this.M = 1\n",
            true,
            1,
        ),
        (
            "type C() =\n  interface I with\n    member this.M = 1\n    member this.N = 2\n",
            true,
            2,
        ),
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} errors: {:?}",
            parse.errors
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("expected an ObjectModel repr");
        };
        // The interface is the last repr member (after any `inherit`).
        let MemberDefn::Interface(i) = om.members().last().expect("at least one member") else {
            panic!("{source:?} should end with a MemberDefn::Interface");
        };
        assert!(
            i.interface_type().is_some(),
            "{source:?} has an interface type",
        );
        assert_eq!(i.has_with(), has_with, "{source:?} `with` clause presence");
        assert_eq!(
            i.members().count(),
            members,
            "{source:?} interface member count",
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 9.14 — `member this.P with get() = … [and set …]` parses into a
/// `MemberDefn::GetSetMember`; `name()` is the property path, `getter()`/`setter()`
/// expose the present accessors (by their `get`/`set` token, not position), and
/// each accessor's `args()`/`body()` hold its parameters and expression.
#[test]
fn get_set_member_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    // (source, has_get, has_set, get_arg_count)
    for (source, has_get, has_set, get_args) in [
        (
            "type T() =\n  member this.P with get() = 1\n",
            true,
            false,
            1,
        ),
        (
            "type T() =\n  member this.P with set v = ()\n",
            false,
            true,
            0,
        ),
        (
            "type T() =\n  member this.P with get() = 1 and set v = ()\n",
            true,
            true,
            1,
        ),
        // Reversed order — the slot is keyed off the `get`/`set` token.
        (
            "type T() =\n  member this.P with set v = () and get() = 1\n",
            true,
            true,
            1,
        ),
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} errors: {:?}",
            parse.errors
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("{source:?} expected an ObjectModel repr");
        };
        let MemberDefn::GetSetMember(gsm) = om.members().next().expect("one member") else {
            panic!("{source:?} should be a MemberDefn::GetSetMember");
        };
        assert_eq!(
            gsm.name().expect("a property name").idents().count(),
            2,
            "{source:?} property path `this.P`"
        );
        assert_eq!(
            gsm.getter().is_some(),
            has_get,
            "{source:?} getter presence"
        );
        assert_eq!(
            gsm.setter().is_some(),
            has_set,
            "{source:?} setter presence"
        );
        if let Some(g) = gsm.getter() {
            assert_eq!(g.args().count(), get_args, "{source:?} getter arg count");
            assert!(g.body().is_some(), "{source:?} getter has a body");
        }
        assert_lossless(source, &parse);
    }
}

/// A return-type annotation on a get/set accessor — `with get() : int = 1`.
/// The `: int` is consumed into a `BINDING_RETURN_INFO` child of the
/// `GET_SET_ACCESSOR` (FCS models each accessor as a `SynBinding`, so this is
/// its `returnInfo`). The facade exposes it via `return_type()`, independently
/// per accessor, while `body()` still returns the unwrapped body expression.
#[test]
fn get_set_accessor_return_type_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T() =\n  member this.P with get() : int = 1 and set (v: int) = ()\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let MemberDefn::GetSetMember(gsm) = om.members().next().expect("one member") else {
        panic!("should be a GetSetMember");
    };
    let getter = gsm.getter().expect("a getter");
    assert!(
        getter.return_type().is_some(),
        "the typed getter exposes a return_type",
    );
    assert!(getter.body().is_some(), "the getter still has a body");
    let setter = gsm.setter().expect("a setter");
    assert!(
        setter.return_type().is_none(),
        "the untyped setter has no return_type (per-accessor independence)",
    );
    assert_lossless(source, &parse);
}

/// Phase 9.14 — a *regular* member (no `with`) is unaffected by the get/set
/// checkpoint dispatch: `member this.M = 1` still parses to a
/// `MemberDefn::Member`, not a `GetSetMember`.
#[test]
fn regular_member_still_parses_after_get_set() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T() =\n  member this.M = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    assert!(
        matches!(om.members().next(), Some(MemberDefn::Member(_))),
        "a plain member stays a MemberDefn::Member"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.9c — an auto-property `[static] member val …` parses into an
/// `AUTO_PROPERTY` (it is *not* a member method, despite the leading `member`).
/// This was a deliberate clean-error gap in 9.9a/9.9b; 9.9c closes it. The forms
/// parse without error and stay lossless.
#[test]
fn member_val_parses_as_auto_property() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    for source in [
        "type T() =\n  member val X = 0 with get, set\n",
        "type T() =\n  static member val X = 0 with get, set\n",
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} errors: {:?}",
            parse.errors
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("expected an ObjectModel repr");
        };
        assert!(
            matches!(om.members().next(), Some(MemberDefn::AutoProperty(_))),
            "{source:?} should be an AutoProperty member"
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 9.9c — facade: an `AUTO_PROPERTY` exposes its name, static-ness,
/// optional type annotation, the getter/setter `prop_kind`, and the initialiser
/// expression. Covers the typed `static`/`get, set` form and a bare instance one.
#[test]
fn auto_property_facade() {
    use crate::syntax::{
        AstNode, AutoPropertyKind, Expr, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr,
    };
    let source = "type T() =\n  member val X = 0\n  static member val Y : int = 1 with get, set\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    assert_eq!(members.len(), 2, "two auto-properties");

    let MemberDefn::AutoProperty(a) = &members[0] else {
        panic!("first member is an auto-property");
    };
    assert_eq!(a.ident().expect("name").text(), "X");
    assert!(!a.is_static(), "instance auto-property");
    assert!(a.ty().is_none(), "no type annotation");
    assert_eq!(a.prop_kind(), AutoPropertyKind::Member, "no with-clause");
    assert!(matches!(a.expr(), Some(Expr::Const(_))), "RHS is `0`");

    let MemberDefn::AutoProperty(b) = &members[1] else {
        panic!("second member is an auto-property");
    };
    assert_eq!(b.ident().expect("name").text(), "Y");
    assert!(b.is_static(), "static auto-property");
    assert!(b.ty().is_some(), "type annotation present");
    assert_eq!(
        b.prop_kind(),
        AutoPropertyKind::PropertyGetSet,
        "with get, set"
    );
    assert_lossless(source, &parse);
}

/// Phase 10.7h — `AutoProperty::attributes()` exposes the property's leading
/// lists (FCS's `SynMemberDefn.AutoProperty.attributes`); a plain auto-property
/// yields none.
#[test]
fn auto_property_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T() =\n    [<A; B>] member val X = 0 with get, set\n    member val Y = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let props: Vec<_> = om
        .members()
        .filter_map(|m| match m {
            MemberDefn::AutoProperty(p) => Some(p),
            _ => None,
        })
        .collect();
    assert_eq!(props.len(), 2, "two auto-properties");
    let attr0: Vec<usize> = props[0]
        .attributes()
        .map(|l| l.attributes().count())
        .collect();
    assert_eq!(
        attr0,
        vec![2],
        "one `[<A; B>]` list of two attributes on the first auto-property"
    );
    assert_eq!(
        props[1].attributes().count(),
        0,
        "the plain auto-property has no attribute lists"
    );
    assert_lossless(source, &parse);
}

// ---- Phase 9.15a: exception definitions --------------------------------

/// Phase 9.15a — `EXCEPTION_DEFN > [EXCEPTION_TOK, UNION_CASE]` for the bare
/// `exception E`: the `exception` keyword passes through (it is not swallowed),
/// and the name lands in a reused (nullary) `UNION_CASE` — FCS's
/// `SynExceptionDefnRepr.caseName: SynUnionCase`. Pins the full green shape.
#[test]
fn exception_bare_green_shape() {
    let source = "exception E\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..12
  MODULE_OR_NAMESPACE@0..12
    EXCEPTION_DEFN@0..11
      EXCEPTION_TOK@0..9 \"exception\"
      UNION_CASE@9..11
        WHITESPACE@9..10 \" \"
        IDENT_TOK@10..11 \"E\"
    NEWLINE@11..12 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.15a — `exception E of int`: the `of` payload reuses the 9.5
/// `UNION_CASE_FIELD` machinery inside the case node. Pins the full green shape.
#[test]
fn exception_of_int_green_shape() {
    let source = "exception E of int\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..19
  MODULE_OR_NAMESPACE@0..19
    EXCEPTION_DEFN@0..18
      EXCEPTION_TOK@0..9 \"exception\"
      UNION_CASE@9..18
        WHITESPACE@9..10 \" \"
        IDENT_TOK@10..11 \"E\"
        WHITESPACE@11..12 \" \"
        OF_TOK@12..14 \"of\"
        UNION_CASE_FIELD@14..18
          WHITESPACE@14..15 \" \"
          LONG_IDENT_TYPE@15..18
            LONG_IDENT@15..18
              IDENT_TOK@15..18 \"int\"
    NEWLINE@18..19 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.15a — `exception E = SomeExn`: the `=` introduces the abbreviation
/// `longId` target (a `LONG_IDENT` sibling of the nullary case), *not* an enum
/// value. Pins the full green shape.
#[test]
fn exception_abbrev_green_shape() {
    let source = "exception E = SomeExn\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..22
  MODULE_OR_NAMESPACE@0..22
    EXCEPTION_DEFN@0..21
      EXCEPTION_TOK@0..9 \"exception\"
      UNION_CASE@9..11
        WHITESPACE@9..10 \" \"
        IDENT_TOK@10..11 \"E\"
      WHITESPACE@11..12 \" \"
      EQUALS_TOK@12..13 \"=\"
      WHITESPACE@13..14 \" \"
      LONG_IDENT@14..21
        IDENT_TOK@14..21 \"SomeExn\"
    NEWLINE@21..22 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.15a — facade: a `ModuleDecl::Exception` exposes its case (name +
/// `of` fields) and, for the abbreviation form, its target path.
#[test]
fn exception_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    // Payload form: the case exposes its name and one anonymous field.
    let payload_parse = parse("exception E of int\n");
    assert!(
        payload_parse.errors.is_empty(),
        "errors: {:?}",
        payload_parse.errors
    );
    let module = ImplFile::cast(payload_parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Exception(e) = module.decls().next().expect("a decl") else {
        panic!("expected an Exception decl");
    };
    let case = e.union_case().expect("a UNION_CASE");
    assert_eq!(case.ident().expect("case name").text(), "E");
    assert_eq!(case.fields().count(), 1, "one `of` field");
    assert!(
        e.abbrev_path().is_none(),
        "payload form has no abbreviation"
    );
    assert!(
        e.members().next().is_none(),
        "no augmentation members in 9.15a"
    );

    // Abbreviation form: the target path is exposed; the case is nullary.
    let abbrev_parse = parse("exception E = A.B\n");
    assert!(
        abbrev_parse.errors.is_empty(),
        "errors: {:?}",
        abbrev_parse.errors
    );
    let module = ImplFile::cast(abbrev_parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Exception(e) = module.decls().next().expect("a decl") else {
        panic!("expected an Exception decl");
    };
    assert_eq!(e.union_case().expect("a UNION_CASE").fields().count(), 0);
    let segs: Vec<String> = e
        .abbrev_path()
        .expect("an abbreviation path")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(segs, vec!["A", "B"]);
}

/// Phase 9.15a sad path — `exception` with no name records a clean error (no
/// panic) and still produces an `EXCEPTION_DEFN` carrier. The following `let`
/// parses as its own decl.
#[test]
fn exception_no_name_is_clean_error() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl};
    let source = "exception = A\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a nameless exception should record an error",
    );
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    assert!(
        module
            .decls()
            .any(|d| matches!(d, ModuleDecl::Exception(_))),
        "still produces an EXCEPTION_DEFN carrier",
    );
    assert_lossless(source, &parse);
}

// ---- Phase 9.15b: exception augmentation (`exception E with member …`) ---

/// Phase 9.15b — `exception E with member this.M = 1`: the `with` (FCS's
/// `SynExceptionDefn.withKeyword`) is a plain `WITH_TOK` direct child of the
/// `EXCEPTION_DEFN` (no repr — unlike the type augmentation's `OBJECT_MODEL_REPR`
/// marker), and the member lands as a direct `MEMBER_DEFN` child (the outer
/// `members` slot). Pins the full green shape, including the close-virtual drain.
#[test]
fn exception_augment_green_shape() {
    let source = "exception E with member this.M = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..35
  MODULE_OR_NAMESPACE@0..35
    EXCEPTION_DEFN@0..35
      EXCEPTION_TOK@0..9 \"exception\"
      UNION_CASE@9..11
        WHITESPACE@9..10 \" \"
        IDENT_TOK@10..11 \"E\"
      WHITESPACE@11..12 \" \"
      WITH_TOK@12..16 \"with\"
      WHITESPACE@16..17 \" \"
      ERROR@17..17 \"\"
      MEMBER_DEFN@17..34
        MEMBER_TOK@17..23 \"member\"
        BINDING@23..34
          LONG_IDENT_PAT@23..30
            WHITESPACE@23..24 \" \"
            LONG_IDENT@24..30
              IDENT_TOK@24..28 \"this\"
              DOT_TOK@28..29 \".\"
              IDENT_TOK@29..30 \"M\"
          WHITESPACE@30..31 \" \"
          EQUALS_TOK@31..32 \"=\"
          WHITESPACE@32..33 \" \"
          ERROR@33..33 \"\"
          CONST_EXPR@33..34
            INT32_LIT@33..34 \"1\"
      NEWLINE@34..35 \"\\n\"
      ERROR@35..35 \"\"
      ERROR@35..35 \"\"
      ERROR@35..35 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.15b — facade: an augmented exception exposes its `with`-block members
/// via `ExceptionDefnDecl::members()` (the outer slot), each a
/// `MemberDefn::Member`, alongside the unchanged case/abbreviation accessors.
#[test]
fn exception_augment_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl};
    let source = "exception E with\n  member this.M = 1\n  member this.N = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Exception(e) = module.decls().next().expect("a decl") else {
        panic!("expected an Exception decl");
    };
    assert_eq!(
        e.union_case()
            .expect("a UNION_CASE")
            .ident()
            .expect("name")
            .text(),
        "E"
    );
    assert!(
        e.abbrev_path().is_none(),
        "augmentation is not an abbreviation"
    );
    let members: Vec<_> = e.members().collect();
    assert_eq!(members.len(), 2, "two augmentation members");
    for m in &members {
        assert!(
            matches!(m, MemberDefn::Member(_)),
            "each augmentation member is a MemberDefn::Member",
        );
    }
    assert_lossless(source, &parse);
}

/// Phase 9.9b — facade: a `val` field is a `MemberDefn::ValField` exposing its
/// name, mutability, static-ness, and type. Covers `static val mutable y` and a
/// plain `val x` in the same body.
#[test]
fn val_field_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T =\n  val x : int\n  static val mutable y : string\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    assert_eq!(members.len(), 2, "two val fields");

    let MemberDefn::ValField(a) = &members[0] else {
        panic!("first member is a val field");
    };
    assert_eq!(a.ident().expect("name").text(), "x");
    assert!(!a.is_mutable() && !a.is_static(), "plain val");
    assert!(a.ty().is_some(), "field type present");

    let MemberDefn::ValField(b) = &members[1] else {
        panic!("second member is a val field");
    };
    assert_eq!(b.ident().expect("name").text(), "y");
    assert!(b.is_mutable() && b.is_static(), "static val mutable");
    assert_lossless(source, &parse);
}

/// Phase 10.7i — `ValField::attributes()` exposes the field's leading lists
/// (FCS's `SynField.attributes`); a plain `val` field yields none.
#[test]
fn val_field_attr_facade() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T =\n  [<A; B>] val mutable x : int\n  val y : string\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let fields: Vec<_> = om
        .members()
        .filter_map(|m| match m {
            MemberDefn::ValField(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(fields.len(), 2, "two val fields");
    let attr0: Vec<usize> = fields[0]
        .attributes()
        .map(|l| l.attributes().count())
        .collect();
    assert_eq!(
        attr0,
        vec![2],
        "one `[<A; B>]` list of two attributes on the first val field"
    );
    assert_eq!(
        fields[1].attributes().count(),
        0,
        "the plain val field has no attribute lists"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.9b — the object-model item classifier must not "see through" a
/// pending body-close `OBLOCKEND` (a filtered virtual) to a later raw
/// `member`/`val`. A col-0 `member` after the type body closes the type rather
/// than being absorbed into it (which previously produced a phantom zero-width
/// `MEMBER_DEFN`). The type body ends at the first member; the col-0 line is a
/// separate (erroring) construct, and the tree stays lossless.
#[test]
fn object_model_does_not_classify_through_body_close() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type T =\n  member this.M = 1\nmember this.N = 2\n";
    let parse = parse(source);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    // Only the first member belongs to the type; the col-0 `member this.N` is
    // *not* absorbed (and there is no phantom member from consuming the close).
    assert_eq!(om.members().count(), 1, "the col-0 member is not part of T");
    assert_lossless(source, &parse);
}

/// Phase 9.9b — `opt_seps` between object-model items is a *single* group, so a
/// *repeated* separator (`val x : int; ; val y : int`) is a parse error. FCS
/// reports "Unexpected symbol ';'" yet still recovers both fields; we mirror
/// that — one recorded error, both `val` fields, lossless tree.
#[test]
fn object_model_repeated_separator_errors_but_recovers() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type T =\n  val x : int; ; val y : int\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "the repeated separator should record a parse error"
    );
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    assert_eq!(om.members().count(), 2, "both val fields recover");
    assert_lossless(source, &parse);
}

/// Phase 9.9b — a `val` with a missing name must not "see through" the pending
/// body-close `OBLOCKEND` to a later col-0 identifier (`type T =\n  val\nfoo`).
/// The field-name lookahead is gated on the *filtered* token, so the virtual
/// close is left for the terminator: the name is reported missing, the col-0
/// `foo` is not absorbed into the type, and no zero-width `IDENT_TOK` is emitted.
#[test]
fn val_field_missing_name_does_not_consume_body_close() {
    use crate::syntax::{AstNode, ImplFile, MemberDefn, ModuleDecl, TypeDefnRepr};
    let source = "type T =\n  val\nfoo\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "the missing field name should record a parse error"
    );
    // No `IDENT_TOK` was synthesised from the zero-width virtual close.
    for tok in parse
        .root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        if tok.kind() == SyntaxKind::IDENT_TOK {
            assert!(
                !tok.text_range().is_empty(),
                "a zero-width IDENT_TOK was emitted from a layout virtual"
            );
        }
    }
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
        panic!("expected an ObjectModel repr");
    };
    let members: Vec<_> = om.members().collect();
    assert_eq!(members.len(), 1, "only the nameless val belongs to T");
    let MemberDefn::ValField(v) = &members[0] else {
        panic!("the member is a val field");
    };
    assert!(v.ident().is_none(), "the field name is missing");
    assert_lossless(source, &parse);
}

// ---- Phase 9.13a: type augmentation (`type T with member …`) ------------

/// Phase 9.13a — `type T with member this.M = 1`: the `with` (replacing `=`)
/// becomes an empty `OBJECT_MODEL_REPR` carrying just the `WITH_TOK`
/// (the `Augmentation` marker), and the member lands as a direct `MEMBER_DEFN`
/// child of the `TYPE_DEFN` (the outer slot). Pins the full green shape.
#[test]
fn type_augment_green_shape() {
    let source = "type T with\n  member this.M = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..32
  MODULE_OR_NAMESPACE@0..32
    TYPE_DEFNS@0..32
      TYPE_DEFN@0..32
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..6
          IDENT_TOK@5..6 \"T\"
        OBJECT_MODEL_REPR@6..11
          WHITESPACE@6..7 \" \"
          WITH_TOK@7..11 \"with\"
        NEWLINE@11..12 \"\\n\"
        WHITESPACE@12..14 \"  \"
        ERROR@14..14 \"\"
        MEMBER_DEFN@14..31
          MEMBER_TOK@14..20 \"member\"
          BINDING@20..31
            LONG_IDENT_PAT@20..27
              WHITESPACE@20..21 \" \"
              LONG_IDENT@21..27
                IDENT_TOK@21..25 \"this\"
                DOT_TOK@25..26 \".\"
                IDENT_TOK@26..27 \"M\"
            WHITESPACE@27..28 \" \"
            EQUALS_TOK@28..29 \"=\"
            WHITESPACE@29..30 \" \"
            ERROR@30..30 \"\"
            CONST_EXPR@30..31
              INT32_LIT@30..31 \"1\"
        NEWLINE@31..32 \"\\n\"
        ERROR@32..32 \"\"
        ERROR@32..32 \"\"
        ERROR@32..32 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 9.13a — an `and`-chain of augmentations (`type T with … and U with …`)
/// is **one** `Types` group of two `TYPE_DEFN`s, each an augmentation. Guards the
/// augment's `declEnd` drain (without it, the `and` continuation is never
/// reached and the chain breaks into stray-token errors).
#[test]
fn type_augment_and_chain_is_one_group() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type T with\n  member this.M = 1\nand U with\n  member this.N = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let groups: Vec<_> = module
        .decls()
        .filter(|d| matches!(d, ModuleDecl::Types(_)))
        .collect();
    assert_eq!(groups.len(), 1, "the `and`-chain is one Types group");
    let ModuleDecl::Types(t) = &groups[0] else {
        unreachable!()
    };
    let defns: Vec<_> = t.defns().collect();
    assert_eq!(defns.len(), 2, "two definitions in the chain");
    for (defn, name) in defns.iter().zip(["T", "U"]) {
        let segs: Vec<String> = defn
            .long_id()
            .expect("a name")
            .idents()
            .map(|t| t.text().to_string())
            .collect();
        assert_eq!(segs, vec![name]);
        assert_eq!(defn.members().count(), 1, "each augment has its member");
        assert!(
            matches!(defn.repr(), Some(TypeDefnRepr::ObjectModel(om)) if om.is_augmentation()),
            "each definition is an augmentation",
        );
    }
    assert_lossless(source, &parse);
}

/// Phase 9.13a — facade: an augmentation's repr is an empty object model marked
/// `is_augmentation()`, and its members are exposed via `TypeDefn::members()`
/// (the outer slot), *not* the repr's own (empty) member list.
#[test]
fn type_augment_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type T with\n  member this.M = 1\n  member this.N = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defn = t.defns().next().expect("one type definition");
    // The repr is an augmentation object model with no members of its own.
    let Some(TypeDefnRepr::ObjectModel(om)) = defn.repr() else {
        panic!("expected an ObjectModel repr, got {:?}", defn.repr());
    };
    assert!(om.is_augmentation(), "the `with` marks an Augmentation");
    assert_eq!(om.members().count(), 0, "repr's own member list is empty");
    // The two members live in the outer slot.
    let outer: Vec<_> = defn.members().collect();
    assert_eq!(outer.len(), 2, "both members in the outer slot");
    assert!(defn.implicit_ctor().is_none(), "no primary constructor");
    assert_lossless(source, &parse);
}

/// Phase 9.13a — a bare `type T = member …` (phase 9.7, object model) is *not*
/// an augmentation: its repr carries the members and `is_augmentation()` is
/// false, while `TypeDefn::members()` (the outer slot) is empty. Guards the
/// slot routing against the augmentation form.
#[test]
fn object_model_is_not_augmentation() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type C() =\n  member this.M = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defn = t.defns().next().expect("one type definition");
    let Some(TypeDefnRepr::ObjectModel(om)) = defn.repr() else {
        panic!("expected an ObjectModel repr");
    };
    assert!(
        !om.is_augmentation(),
        "a `= member` body is not an augmentation"
    );
    // The implicit ctor + the member live in the repr; the outer slot is empty.
    assert_eq!(
        defn.members().count(),
        0,
        "pure object model: outer slot empty"
    );
    assert_lossless(source, &parse);
}

/// Phase 9.12 — `type T = class/struct/interface … end` parses into an
/// `ObjectModel` repr whose `is_class()`/`is_struct()`/`is_interface()` reports
/// the explicit kind marker; the members live in the repr (slot 1), and the body
/// is delimited by `end`. (Distinct from `is_augmentation()`, the `with` form.)
#[test]
fn kind_marked_repr_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    // (source, is_class, is_struct, is_interface, member_count)
    for (source, class, strct, iface, members) in [
        (
            "type T =\n  class\n    member this.M = 1\n  end\n",
            true,
            false,
            false,
            1,
        ),
        (
            "type T =\n  struct\n    val x : int\n  end\n",
            false,
            true,
            false,
            1,
        ),
        (
            "type T =\n  interface\n    abstract M : int\n  end\n",
            false,
            false,
            true,
            1,
        ),
        ("type T =\n  class\n  end\n", true, false, false, 0),
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} errors: {:?}",
            parse.errors
        );
        let module = ImplFile::cast(parse.root.clone())
            .and_then(|f| f.modules().next())
            .expect("a MODULE_OR_NAMESPACE");
        let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
            panic!("expected a Types decl");
        };
        let Some(TypeDefnRepr::ObjectModel(om)) = t.defns().next().expect("one defn").repr() else {
            panic!("{source:?} expected an ObjectModel repr");
        };
        assert_eq!(om.is_class(), class, "{source:?} is_class");
        assert_eq!(om.is_struct(), strct, "{source:?} is_struct");
        assert_eq!(om.is_interface(), iface, "{source:?} is_interface");
        assert!(
            !om.is_augmentation(),
            "{source:?} a kind-marked repr is not an augmentation"
        );
        assert_eq!(om.members().count(), members, "{source:?} member count");
        assert_lossless(source, &parse);
    }
}

/// Phase 9.12 — the `struct` *type* forms (the struct tuple `struct (T * U)` and
/// the struct anon-record `struct {| … |}`, phase 7.9) are **not** explicit-kind
/// markers: the `struct (`/`struct {|` lookahead exclusion in
/// `peek_is_kind_marked_repr_start` keeps them out of the
/// `class/struct/interface … end` arm (which would mis-expect an `end`). Both
/// stay lossless and neither emits the kind-marker "expected `end`" diagnostic.
/// The anon-record form routes cleanly to `parse_type` as a `TypeDefnRepr::Abbrev`;
/// the tuple form hits a *pre-existing* `peek_starts_type` gap (an abbreviation-
/// path error, orthogonal to this slice).
#[test]
fn struct_type_forms_are_not_kind_markers() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    for source in [
        "type T = struct (int * int)\n",
        "type T = struct {| X : int |}\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse
                .errors
                .iter()
                .any(|e| e.message.contains("expected `end`")),
            "{source:?} must not be misrouted to the kind-marker arm: {:?}",
            parse.errors,
        );
        assert_lossless(source, &parse);
    }
    // The anon-record form specifically is a clean `Abbrev` (the regression fix).
    let source = "type T = struct {| X : int |}\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "{source:?} errors: {:?}",
        parse.errors
    );
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    assert!(
        matches!(
            t.defns().next().expect("one defn").repr(),
            Some(TypeDefnRepr::Abbrev(_))
        ),
        "`struct {{| … |}}` is a struct anon-record type abbreviation",
    );
}

// ---- Phase 9.13b: members trailing a repr (`= <repr> [with] member …`) ----

/// Phase 9.13b — facade: a record with a trailing `with member …` keeps its
/// `Record` repr (it is *not* an augmentation) and exposes the member via the
/// outer slot.
#[test]
fn record_with_trailing_member_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type R = { X: int } with member this.M = this.X\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defn = t.defns().next().expect("one type definition");
    assert!(
        matches!(defn.repr(), Some(TypeDefnRepr::Record(_))),
        "repr stays Record, got {:?}",
        defn.repr()
    );
    assert_eq!(defn.members().count(), 1, "the member is in the outer slot");
    assert!(defn.implicit_ctor().is_none());
    assert_lossless(source, &parse);
}

/// Phase 9.13b — facade: *bare* trailing members (no `with`) on a record land
/// in the outer slot with the repr unchanged.
#[test]
fn record_bare_trailing_members_facade() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type R =\n    { X: int }\n    member this.M = this.X\n    static member S = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defn = t.defns().next().expect("one type definition");
    assert!(
        matches!(defn.repr(), Some(TypeDefnRepr::Record(_))),
        "repr stays Record, got {:?}",
        defn.repr()
    );
    assert_eq!(defn.members().count(), 2, "both members in the outer slot");
    assert_lossless(source, &parse);
}

/// Phase 9.13b — the forms FCS *rejects* stay clean errors for us too:
/// * a single-line repr with a member on the next line (the member arrives
///   after the body block's `OBLOCKEND`, so it is outside the type — FCS's
///   "Unexpected keyword 'member'"), top-level and nested;
/// * bare members after a type *abbreviation* (FCS: "Unexpected keyword
///   'member' in type definition. Expected '|' or other token.");
/// * bare members after a *zero-bar* union (`X of int` — same FCS error; a
///   bar-carrying union admits them, see the diff tests).
#[test]
fn invalid_bare_trailing_members_stay_lossless() {
    for source in [
        "type R = { X: int }\n    member this.M = this.X\n",
        "module M =\n    type R = { X: int }\n    member this.M = 1\n",
        "type T =\n    int\n    member this.M = 1\n",
        "type U =\n    X of int\n    member this.M = 1\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?} is FCS-invalid, so it should error"
        );
        assert_lossless(source, &parse);
    }
}

/// Phase 9.13b — bare trailing members followed by a `with`-augment is FCS's
/// "At most one 'with' augmentation is permitted". We record that error and
/// still parse both member groups into the outer slot (lossless recovery; FCS
/// drops the whole declaration, so there is no shape to diff against).
#[test]
fn bare_members_then_with_augment_errors() {
    let source = "type R =\n    { X: int }\n    member this.A = 1\n    with member this.B = 2\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("At most one 'with' augmentation")),
        "expected the multiple-augmentation error, got {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);
}

/// Phase 9.13b — a trailing `with` with no members (`type R = { X: int } with`
/// at EOF) recovers with an empty outer slot, mirroring the 9.13a empty-augment
/// recovery. FCS reports a strict-indentation FS0058 (the augment body is empty,
/// so the body block's anchor — the EOF at line 2, column 0 — is offside of the
/// `with` context) but produces the same record-with-no-members shape. Since the
/// offside-diagnostics stage (`docs/offside-diagnostics-plan.md`, §A) landed, we
/// emit the matching FS0058 at that same EOF byte offset while still recovering
/// the record shape.
#[test]
fn record_empty_trailing_with_recovers() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    let source = "type R = { X: int } with\n";
    let parse = parse(source);
    // FS0058 offside at the empty augment body (EOF, byte 25 = line 2 col 0),
    // matching FCS. The record shape is still recovered below.
    assert_eq!(
        parse.errors.len(),
        1,
        "expected exactly the offside FS0058, got: {:?}",
        parse.errors,
    );
    assert_eq!(parse.errors[0].span, 25..25, "offside span at EOF");
    let module = ImplFile::cast(parse.root.clone())
        .and_then(|f| f.modules().next())
        .expect("a MODULE_OR_NAMESPACE");
    let ModuleDecl::Types(t) = module.decls().next().expect("a decl") else {
        panic!("expected a Types decl");
    };
    let defn = t.defns().next().expect("one type definition");
    assert!(matches!(defn.repr(), Some(TypeDefnRepr::Record(_))));
    assert_eq!(defn.members().count(), 0, "no members in the empty augment");
    assert_lossless(source, &parse);
}

/// `begin e end` builds a `PAREN_EXPR` whose `begin`/`end` are real
/// `BEGIN_TOK`/`END_TOK` children straddling the inner expression (FCS's
/// `SynExpr.Paren`). Pins the green-tree token placement the normalised diff
/// (`parser_diff_begin_end`) can't see, plus the lossless round-trip.
#[test]
fn begin_end_expr_green_shape() {
    let src = "let x = begin 1 end\n";
    let parse = parse(src);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..20
  MODULE_OR_NAMESPACE@0..20
    LET_DECL@0..19
      LET_TOK@0..3 \"let\"
      BINDING@3..19
        NAMED_PAT@3..5
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..5 \"x\"
        WHITESPACE@5..6 \" \"
        EQUALS_TOK@6..7 \"=\"
        WHITESPACE@7..8 \" \"
        ERROR@8..8 \"\"
        PAREN_EXPR@8..19
          BEGIN_TOK@8..13 \"begin\"
          WHITESPACE@13..14 \" \"
          CONST_EXPR@14..15
            INT32_LIT@14..15 \"1\"
          WHITESPACE@15..16 \" \"
          END_TOK@16..19 \"end\"
    NEWLINE@19..20 \"\\n\"
    ERROR@20..20 \"\"
    ERROR@20..20 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(src, &parse);
}

/// The empty `begin end` is the unit constant (`SynConst.Unit`), a
/// `CONST_EXPR` holding just the `BEGIN_TOK`/`END_TOK` pair — *not* a
/// `PAREN_EXPR`.
#[test]
fn begin_end_empty_is_unit_green_shape() {
    let src = "let u = begin end\n";
    let parse = parse(src);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..18
  MODULE_OR_NAMESPACE@0..18
    LET_DECL@0..17
      LET_TOK@0..3 \"let\"
      BINDING@3..17
        NAMED_PAT@3..5
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..5 \"u\"
        WHITESPACE@5..6 \" \"
        EQUALS_TOK@6..7 \"=\"
        WHITESPACE@7..8 \" \"
        ERROR@8..8 \"\"
        CONST_EXPR@8..17
          BEGIN_TOK@8..13 \"begin\"
          WHITESPACE@13..14 \" \"
          END_TOK@14..17 \"end\"
    NEWLINE@17..18 \"\\n\"
    ERROR@18..18 \"\"
    ERROR@18..18 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(src, &parse);
}

/// `module X = begin … end` (verbose-syntax module body): the `begin`/`end`
/// ride as `BEGIN_TOK`/`END_TOK` marker children of the `NESTED_MODULE_DECL`,
/// straddling the body decls. The interleaved zero-width `ERROR`s are the
/// LexFilter `OBLOCKBEGIN`/`OBLOCKEND`/`ODECLEND` scaffolding.
#[test]
fn module_begin_end_green_shape() {
    let src = "module X = begin\n    let y = 1\nend\n";
    let parse = parse(src);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..35
  MODULE_OR_NAMESPACE@0..35
    NESTED_MODULE_DECL@0..34
      MODULE_TOK@0..6 \"module\"
      WHITESPACE@6..7 \" \"
      LONG_IDENT@7..8
        IDENT_TOK@7..8 \"X\"
      WHITESPACE@8..9 \" \"
      EQUALS_TOK@9..10 \"=\"
      WHITESPACE@10..11 \" \"
      ERROR@11..11 \"\"
      BEGIN_TOK@11..16 \"begin\"
      NEWLINE@16..17 \"\\n\"
      WHITESPACE@17..21 \"    \"
      LET_DECL@21..30
        LET_TOK@21..24 \"let\"
        BINDING@24..30
          NAMED_PAT@24..26
            WHITESPACE@24..25 \" \"
            IDENT_TOK@25..26 \"y\"
          WHITESPACE@26..27 \" \"
          EQUALS_TOK@27..28 \"=\"
          WHITESPACE@28..29 \" \"
          ERROR@29..29 \"\"
          CONST_EXPR@29..30
            INT32_LIT@29..30 \"1\"
      NEWLINE@30..31 \"\\n\"
      ERROR@31..31 \"\"
      ERROR@31..31 \"\"
      END_TOK@31..34 \"end\"
      ERROR@34..34 \"\"
    NEWLINE@34..35 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(src, &parse);
}

/// A standalone attribute as the last item before the verbose `end`
/// (`module A = begin [<assembly: Foo>] end`) recovers to an `ATTRIBUTES_DECL`
/// — the `begin`-delimited `end` is the end-of-scope sentinel for the
/// standalone-attributes branch, exactly like an ordinary body's `OBLOCKEND`.
/// FCS produces the same `NestedModule > [Attributes]` shape (while also
/// reporting a recovery diagnostic we don't model). Without the
/// `begin_delimited` end-of-scope check this fell to the deferred-error arm and
/// left bare `ATTRIBUTE_LIST` siblings instead.
#[test]
fn module_begin_end_trailing_attributes_recover() {
    let src = "module A = begin\n    [<assembly: Foo>]\nend\n";
    let parse = parse(src);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let module = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::NESTED_MODULE_DECL)
        .expect("a NESTED_MODULE_DECL");
    // The body recovered to an `ATTRIBUTES_DECL`, not a bare `ATTRIBUTE_LIST`.
    assert!(
        module
            .children()
            .any(|n| n.kind() == SyntaxKind::ATTRIBUTES_DECL),
        "expected an ATTRIBUTES_DECL child; tree:\n{}",
        debug_tree(&parse.root),
    );
    assert!(
        !module
            .children()
            .any(|n| n.kind() == SyntaxKind::ATTRIBUTE_LIST),
        "attribute list should be wrapped in ATTRIBUTES_DECL, not a direct child",
    );
    // The `begin`/`end` markers still straddle the body.
    assert!(
        module
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::BEGIN_TOK),
        "expected a BEGIN_TOK marker",
    );
    assert!(
        module
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::END_TOK),
        "expected an END_TOK marker",
    );
    assert_lossless(src, &parse);
}

/// FSharp.Core's inline-IL **type** definition — `type byref<'T> = (# "!0&" #)`
/// (FCS's `SynTypeDefnSimpleRepr.LibraryOnlyILAssembly`, `pars.fsy:2483`'s
/// `LPAREN HASH string HASH rparen`). Unlike the expression form, the `(`/`)`
/// belong to the `INLINE_IL_REPR` itself (no `Paren` wrapper); the closing `)`
/// is LexFilter-swallowed and recovered as `RPAREN_TOK`. The instruction string
/// is a bare `STRING_LIT` (not a type node). The surrounding zero-width `ERROR`s
/// are the body-opening/closing `OBLOCKBEGIN`/`OBLOCKEND`, as for every repr.
#[test]
fn inline_il_type_repr_green_shape() {
    let source = "type byref<'T> = (# \"!0&\" #)\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..29
  MODULE_OR_NAMESPACE@0..29
    TYPE_DEFNS@0..28
      TYPE_DEFN@0..28
        TYPE_TOK@0..4 \"type\"
        WHITESPACE@4..5 \" \"
        LONG_IDENT@5..10
          IDENT_TOK@5..10 \"byref\"
        TYPAR_DECLS@10..14
          ERROR@10..10 \"\"
          LESS_TOK@10..11 \"<\"
          TYPAR_DECL@11..13
            QUOTE_TOK@11..12 \"'\"
            IDENT_TOK@12..13 \"T\"
          GREATER_TOK@13..14 \">\"
        WHITESPACE@14..15 \" \"
        EQUALS_TOK@15..16 \"=\"
        WHITESPACE@16..17 \" \"
        ERROR@17..17 \"\"
        INLINE_IL_REPR@17..28
          LPAREN_TOK@17..18 \"(\"
          HASH_TOK@18..19 \"#\"
          WHITESPACE@19..20 \" \"
          STRING_LIT@20..25 \"\\\"!0&\\\"\"
          WHITESPACE@25..26 \" \"
          HASH_TOK@26..27 \"#\"
          RPAREN_TOK@27..28 \")\"
        ERROR@28..28 \"\"
    NEWLINE@28..29 \"\\n\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// The inline-IL type repr is reachable through the typed AST as
/// `TypeDefnRepr::InlineIl`, and round-trips losslessly for both the single-
/// and multi-parameter `byref` forms that appear in `prim-types.fs`. (Both are
/// `byref`, the only inline-IL *type* abbreviations in FSharp.Core; the dozens
/// of `(# … #)` inside value bindings are the separate expression form.)
#[test]
fn inline_il_type_repr_reaches_ast() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    for source in [
        "type byref<'T> = (# \"!0&\" #)\n",
        "type byref<'T, 'Kind> = (# \"!0&\" #)\n",
    ] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} produced errors: {:?}",
            parse.errors
        );
        assert_lossless(source, &parse);

        let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
        let module = file.modules().next().expect("module");
        let defn = module
            .decls()
            .find_map(|d| match d {
                ModuleDecl::Types(t) => t.defns().next(),
                _ => None,
            })
            .expect("a type definition");
        assert!(
            matches!(defn.repr(), Some(TypeDefnRepr::InlineIl(_))),
            "{source:?} repr should be inline IL, got {:?}",
            defn.repr(),
        );
    }
}

/// Malformed inline-IL type reprs recover with an error but stay lossless and
/// never panic (the fail-loud-but-keep-the-tree discipline of the expression
/// form's `inline_il_malformed_recovers_losslessly`): a missing instruction
/// string (`(# #)`), a missing closing `#` (`(# "x" )`), and an unterminated
/// body (`(# "x"` at EOF). Each must report at least one error and round-trip.
#[test]
fn inline_il_type_repr_malformed_recovers_losslessly() {
    for source in [
        "type T = (# #)\n",      // no instruction string
        "type T = (# \"x\" )\n", // missing closing `#`
        "type T = (# \"x\"\n",   // unterminated (EOF before `#)`)
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?} should report a recovery error"
        );
        assert_lossless(source, &parse);
    }
}

/// The inline-IL gate must not steal a *parenthesised flexible type*
/// abbreviation — `type T = (#int)` / `type T = (#IDisposable)` — which also
/// opens with `( #` but is an ordinary abbreviation to a `(#ty)` flexible type,
/// not inline IL (inline IL requires a *string* after the `#`). These must
/// still parse cleanly as a `TypeDefnRepr::Abbrev`, not be misrouted to the
/// inline-IL parser.
#[test]
fn inline_il_gate_does_not_steal_flexible_type_abbrev() {
    use crate::syntax::{AstNode, ImplFile, ModuleDecl, TypeDefnRepr};
    for source in ["type T = (#int)\n", "type T = (#IDisposable)\n"] {
        let parse = parse(source);
        assert!(
            parse.errors.is_empty(),
            "{source:?} produced errors: {:?}",
            parse.errors
        );
        assert_lossless(source, &parse);

        let file = ImplFile::cast(parse.root.clone()).expect("ImplFile");
        let module = file.modules().next().expect("module");
        let defn = module
            .decls()
            .find_map(|d| match d {
                ModuleDecl::Types(t) => t.defns().next(),
                _ => None,
            })
            .expect("a type definition");
        assert!(
            matches!(defn.repr(), Some(TypeDefnRepr::Abbrev(_))),
            "{source:?} should be a flexible-type abbreviation, got {:?}",
            defn.repr(),
        );
    }
}

/// The inline-IL type repr appears in `prim-types.fsi` too
/// (`type byref<'T> = (# "!0&" #)`), so the **signature** type-spec dispatch
/// must recognise it just like the implementation path — otherwise the `.fsi`
/// still errors where the `.fs` parses. Must round-trip and reach
/// `TypeDefnRepr::InlineIl`.
#[test]
fn inline_il_type_repr_in_sig_file_is_clean() {
    use crate::syntax::{AstNode, SigDecl, SigFile, TypeDefnRepr};
    let source = "namespace N\ntype byref<'T> = (# \"!0&\" #)\n";
    let parse = parse_sig(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    let file = SigFile::cast(parse.root.clone()).expect("SigFile");
    let module = file.modules().next().expect("a sig module");
    let defn = module
        .sig_decls()
        .find_map(|d| match d {
            SigDecl::Types(t) => t.defns().next(),
            _ => None,
        })
        .expect("a type definition");
    assert!(
        matches!(defn.repr(), Some(TypeDefnRepr::InlineIl(_))),
        "the sig repr should be inline IL, got {:?}",
        defn.repr(),
    );
}

/// A bar-less operator union case is admitted by FCS *only* through the
/// `unionCaseName COLON topType` (`FullType`) production (`pars.fsy:2855`); the
/// nullary and `of` operator forms require a leading `|` (FCS's
/// `firstUnionCaseDecl` uses a bare `ident` there, not `unionCaseName`). So a
/// bar-less `([])` / `( :: ) of …` must *not* be silently accepted as a union —
/// it falls through to the abbreviation path and reports an error, matching FCS.
#[test]
fn bar_less_operator_union_case_requires_fulltype() {
    for source in ["type T = ([])\n", "type T = ( :: ) of int\n"] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "{source:?} should error (FCS admits a bar-less operator case only as `: topType`)",
        );
        assert!(
            !parse
                .root
                .descendants()
                .any(|n| n.kind() == SyntaxKind::UNION_REPR),
            "{source:?} must not parse as a union repr",
        );
        assert_lossless(source, &parse);
    }
    // The `FullType` (`: topType`) form *is* a valid bar-less operator case.
    let ok = "type T = ([]) : int\n";
    let parse = parse(ok);
    assert!(
        parse.errors.is_empty(),
        "`([]) : int` errors: {:?}",
        parse.errors
    );
}

/// In a mixed enum/union group, a value-less operator case anchors the
/// `parsAllEnumFieldsRequireValues` diagnostic on the *whole* operator name
/// (`([])`), not just the opening `(` — matching how an ident case anchors on
/// its name.
#[test]
fn mixed_enum_operator_case_diagnostic_spans_name() {
    let source = "type E = | ([]) | A = 0\n";
    let parse = parse(source);
    let err = parse
        .errors
        .iter()
        .find(|e| e.message.contains("all enum cases must be given values"))
        .expect("the mixed-enum diagnostic");
    assert_eq!(
        &source[err.span.clone()],
        "([])",
        "anchors on the operator name"
    );
    assert_lossless(source, &parse);
}

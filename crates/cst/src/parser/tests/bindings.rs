use super::super::*;
use super::*;

/// Phase 4.1 — `let x = 1` produces a clean `LET_DECL` containing
/// `LET_TOK BINDING(NAMED_PAT(IDENT_TOK) EQUALS_TOK <CONST_EXPR>)`.
/// The `Virtual::Let` is consumed by emitting the underlying raw
/// `Token::Let` as `LET_TOK`; the `Virtual::BlockBegin` between `=`
/// and the RHS lands as a zero-width `ERROR` placeholder (no semantic
/// role at this layer); the trailing `Virtual::BlockEnd` / `DeclEnd`
/// fall through the impl-file loop as zero-width `ERROR` placeholders.
/// Most importantly: no `ParseError`s.
#[test]
fn let_binding_int_literal() {
    let source = "let x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..10
  MODULE_OR_NAMESPACE@0..10
    LET_DECL@0..9
      LET_TOK@0..3 \"let\"
      BINDING@3..9
        NAMED_PAT@3..5
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..5 \"x\"
        WHITESPACE@5..6 \" \"
        EQUALS_TOK@6..7 \"=\"
        WHITESPACE@7..8 \" \"
        ERROR@8..8 \"\"
        CONST_EXPR@8..9
          INT32_LIT@8..9 \"1\"
    NEWLINE@9..10 \"\\n\"
    ERROR@10..10 \"\"
    ERROR@10..10 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Phase 4.1 — `let x = y` accepts an ident on the RHS. The point
/// is that `parse_let_equals_rhs` defers to `parse_expr`, which
/// already handles `Ident` atoms, so a binding's RHS isn't a
/// pattern-restricted form.
#[test]
fn let_binding_ident_rhs() {
    let source = "let x = y\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::LET_DECL),
        "expected a LET_DECL in the tree",
    );
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::IDENT_EXPR),
        "expected an IDENT_EXPR (rhs `y`) in the tree",
    );
    assert_lossless(source, &parse);

    // Inspect the typed AST: one LET_DECL whose binding's pattern is
    // `Named("x")` and whose RHS is `Ident("y")`.
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let, got {decl:?}")
    };
    assert!(!let_decl.is_rec(), "Phase 4.1 only emits non-rec bindings");
    let bindings: Vec<_> = let_decl.bindings().collect();
    assert_eq!(bindings.len(), 1, "expected one binding, got {bindings:?}");
    let b = &bindings[0];
    assert!(!b.is_mutable());
    assert!(!b.is_inline());
    let pat = b.pat().expect("binding pat");
    let crate::syntax::Pat::Named(named) = pat else {
        panic!("expected NAMED_PAT, got {pat:?}");
    };
    let ident = named.ident().expect("NAMED_PAT has IDENT_TOK");
    assert_eq!(ident.text(), "x");
    let rhs = b.expr().expect("binding rhs");
    let crate::syntax::Expr::Ident(rhs_ident) = rhs else {
        panic!("expected Ident rhs, got {rhs:?}");
    };
    assert_eq!(rhs_ident.ident().expect("ident").text(), "y");
}

/// Accessibility on a value-form binding head — `let private x = 1`. The
/// modifier (FCS's `atomicPatternLongIdent: access pathOp`) is consumed as an
/// `ACCESS_TOK` that is a **direct child of `BINDING`** and a *sibling* of the
/// `NAMED_PAT` (mirroring the exception / union-case / record-field
/// convention), so it stays out of `ERROR`, is invisible to the node-based
/// `Binding::pat()` accessor, and is elided by the normaliser. No errors.
#[test]
fn let_binding_value_access_modifier() {
    use crate::syntax::AstNode;
    let source = "let private x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    // The modifier is claimed as ACCESS_TOK, never ERROR.
    assert!(
        parse
            .root
            .descendants_with_tokens()
            .any(|el| el.kind() == SyntaxKind::ACCESS_TOK),
        "the access modifier must be claimed as ACCESS_TOK, not ERROR",
    );
    // It is a direct token child of BINDING (a sibling of the pattern node),
    // not nested inside the NAMED_PAT.
    let binding = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::BINDING)
        .expect("a BINDING node");
    assert!(
        binding
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::ACCESS_TOK),
        "ACCESS_TOK must be a direct child token of BINDING",
    );
    assert!(
        parse
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::NAMED_PAT)
            .expect("a NAMED_PAT")
            .descendants_with_tokens()
            .all(|el| el.kind() != SyntaxKind::ACCESS_TOK),
        "ACCESS_TOK must not be nested inside the NAMED_PAT",
    );
    // The typed AST still sees the plain head pattern `Named("x")`.
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let crate::syntax::ModuleDecl::Let(let_decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Let")
    };
    let b = let_decl.bindings().next().expect("a binding");
    let crate::syntax::Pat::Named(named) = b.pat().expect("binding pat") else {
        panic!("expected NAMED_PAT head")
    };
    assert_eq!(named.ident().expect("IDENT_TOK").text(), "x");
    assert_lossless(source, &parse);
}

/// Accessibility on a function-form binding head — `let private f a = a`. The
/// modifier rides as an `ACCESS_TOK` sibling of the `LONG_IDENT_PAT`; the
/// function-form promotion (`f` + arg `a`) is otherwise untouched.
#[test]
fn let_binding_function_access_modifier() {
    use crate::syntax::AstNode;
    let source = "let private f a = a\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        parse
            .root
            .descendants_with_tokens()
            .any(|el| el.kind() == SyntaxKind::ACCESS_TOK),
        "the access modifier must be claimed as ACCESS_TOK, not ERROR",
    );
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let crate::syntax::ModuleDecl::Let(let_decl) = module.decls().next().expect("decl") else {
        panic!("expected ModuleDecl::Let")
    };
    let b = let_decl.bindings().next().expect("a binding");
    let crate::syntax::Pat::LongIdent(_) = b.pat().expect("binding pat") else {
        panic!("expected LONG_IDENT_PAT head")
    };
    assert_lossless(source, &parse);
}

/// Regression: the access-modifier lookahead must NOT cross a
/// LexFilter-swallowed `)`. In `let f (private) x = x` the inner `private`
/// is followed (on the *raw* stream) by `)`, not an identifier — there is no
/// `access pathOp` here. A filtered-only lookahead would skip the swallowed
/// `)` and see the *outer* `x`, wrongly consume `private` as `ACCESS_TOK`, and
/// then steal that `x` into the parens while dropping the `)` as `ERROR`. The
/// raw-stream gate (the next significant raw token after the keyword must be an
/// ident) rejects it, so no `ACCESS_TOK` is emitted for this malformed input.
#[test]
fn let_access_modifier_does_not_cross_swallowed_paren() {
    let source = "let f (private) x = x\n";
    let parse = parse(source);
    assert!(
        parse
            .root
            .descendants_with_tokens()
            .all(|el| el.kind() != SyntaxKind::ACCESS_TOK),
        "`private` before a swallowed `)` is not an access position; \
         no ACCESS_TOK must be claimed",
    );
    assert_lossless(source, &parse);
}

/// Phase 4.1 — `let x = 1 + 2` uses the infix-operator path. Sanity-
/// check that `parse_let_equals_rhs` doesn't terminate the expression
/// prematurely (i.e. the Pratt climb continues past `+`).
#[test]
fn let_binding_infix_rhs() {
    let source = "let x = 1 + 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::LET_DECL),
        "expected a LET_DECL in the tree",
    );
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::INFIX_APP_EXPR),
        "expected an INFIX_APP_EXPR (`1 + 2`) in the tree",
    );
    assert_lossless(source, &parse);
}

/// LexFilter rewrites both `Token::Let` and `Token::Use` (LexFilter.fs:2157)
/// to the same `Virtual::Let`. `parse_let_head_and_bindings` must accept either
/// as the backing raw token rather than panic the debug-assert.
///
/// At this layer we don't model the `isUse` distinction yet (Phase 4.1
/// only emits the let-binding shape — `use` is a known limitation),
/// so the keyword is emitted as `LET_TOK` and the rest of the binding
/// proceeds identically. The point of this test is "doesn't panic".
#[test]
fn use_x_eq_int_does_not_panic() {
    let source = "use x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        tree_contains_kind(&parse.root, SyntaxKind::LET_DECL),
        "expected a LET_DECL in the tree",
    );
    assert_lossless(source, &parse);
    // The `use` keyword survives as LET_TOK's text — verify so a future
    // change distinguishing `use` (e.g. via a `USE_TOK` kind or an `isUse`
    // marker on BINDING) trips this assertion and forces the test to be
    // revised rather than silently regressing.
    let let_tok_text = parse
        .root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::LET_TOK)
        .map(|t| t.text().to_string())
        .expect("expected a LET_TOK in the tree");
    assert_eq!(let_tok_text, "use");
}

/// Multi-line offside RHS — `2` must stay inside the binding rather
/// than escape to the impl-file loop as a fresh top-level decl.
///
/// FCS parses the RHS as `SynExpr.Sequential(1, 2)`; the binding-RHS path
/// now gathers the offside block through [`Parser::parse_seq_block_body`],
/// so the two statements wrap in a single `SEQUENTIAL_EXPR`. No errors, the
/// lossless invariant holds (every source byte still lands somewhere in the
/// green tree), and `2` does not escape as a sibling `EXPR_DECL`.
#[test]
fn let_binding_offside_rhs_keeps_trailing_inside() {
    let source = "let x =\n    1\n    2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    // Exactly one decl — the binding. `2` must not have escaped as its
    // own EXPR_DECL.
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1, "expected exactly one decl; got: {decls:?}");
    let crate::syntax::ModuleDecl::Let(let_decl) = &decls[0] else {
        panic!(
            "expected the lone decl to be a LET_DECL, got {:?}",
            decls[0]
        );
    };
    // The RHS is a two-statement `Sequential`, holding `2` inside the binding.
    let rhs = let_decl
        .bindings()
        .next()
        .expect("binding")
        .expr()
        .expect("binding RHS expr");
    let crate::syntax::Expr::Sequential(seq) = rhs else {
        panic!("expected a Sequential RHS, got {rhs:?}");
    };
    assert_eq!(seq.statements().count(), 2, "two sequenced statements");
    assert_lossless(source, &parse);
}

/// Phase 6.1 — `let 1\n`: an integer-literal head is a valid
/// `SynPat.Const` (FCS rejects it semantically as not a value
/// definition, but the parser accepts the shape). The binding then
/// fails on the missing `=` and the error surfaces there. Confirms
/// the parser neither panics nor loses tokens, and that the
/// const-pat head reaches the tree before the trailing-`=` check.
#[test]
fn let_then_int_records_error() {
    let source = "let 1\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("expected `=` after binding pattern")),
        "expected an 'expected `=` after binding pattern' error; got: {:?}",
        parse.errors,
    );
    // The CONST_PAT head still landed in the tree.
    assert!(
        parse.root.descendants_with_tokens().any(|el| el
            .into_node()
            .is_some_and(|n| n.kind() == SyntaxKind::CONST_PAT)),
        "expected a CONST_PAT in the tree for the integer-literal head",
    );
    assert_lossless(source, &parse);
}

/// Phase 4.2 — `let rec f = 1\n` consumes the `rec` keyword as a
/// distinct `REC_TOK` child of `LET_DECL`, sitting between `LET_TOK`
/// and the first `BINDING`. `LetDecl::is_rec` projects this presence
/// to the FCS `isRec = true` flag.
#[test]
fn let_rec_single_binding() {
    let source = "let rec f = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..14
  MODULE_OR_NAMESPACE@0..14
    LET_DECL@0..13
      LET_TOK@0..3 \"let\"
      WHITESPACE@3..4 \" \"
      REC_TOK@4..7 \"rec\"
      BINDING@7..13
        NAMED_PAT@7..9
          WHITESPACE@7..8 \" \"
          IDENT_TOK@8..9 \"f\"
        WHITESPACE@9..10 \" \"
        EQUALS_TOK@10..11 \"=\"
        WHITESPACE@11..12 \" \"
        ERROR@12..12 \"\"
        CONST_EXPR@12..13
          INT32_LIT@12..13 \"1\"
    NEWLINE@13..14 \"\\n\"
    ERROR@14..14 \"\"
    ERROR@14..14 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let, got {decl:?}")
    };
    assert!(let_decl.is_rec(), "expected is_rec = true for `let rec`");
    let bindings: Vec<_> = let_decl.bindings().collect();
    assert_eq!(bindings.len(), 1);
}

/// Phase 4.2 — `let f = 1\nand g = 2\n` joins two bindings into a
/// single `LET_DECL`. The `Virtual::BlockEnd` between the first
/// binding's RHS and the `and` is consumed *inside* `LET_DECL` (as a
/// zero-width `ERROR`) only because an `and`-continuation followed;
/// without `and`, that same `BlockEnd` would have fallen out to the
/// impl-file loop's virtual-fallthrough arm (sibling of `LET_DECL`).
///
/// `isRec` is false — FCS rejects this form at the parser with error
/// FS0576 (`parsLetAndForNonRecBindings`, pars.fsy:3073) but still
/// emits a single `SynModuleDecl.Let` with `isRec = false` and both
/// bindings. We mirror both: AST shape *and* the parse-error
/// diagnostic at the `let` keyword span.
#[test]
fn let_and_chain_without_rec() {
    let source = "let f = 1\nand g = 2\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("non-recursive `let ... and ...`")),
        "expected the non-rec `let ... and ...` diagnostic; got: {:?}",
        parse.errors,
    );
    assert_eq!(
        parse.errors.len(),
        1,
        "expected exactly one diagnostic (the non-rec-and error); got: {:?}",
        parse.errors,
    );
    // The diagnostic should be at the `let` keyword.
    assert_eq!(parse.errors[0].span, 0..3);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1, "expected exactly one decl; got: {decls:?}");
    let crate::syntax::ModuleDecl::Let(let_decl) = &decls[0] else {
        panic!("expected ModuleDecl::Let, got {:?}", decls[0])
    };
    assert!(!let_decl.is_rec(), "no `rec` keyword → is_rec = false");
    let bindings: Vec<_> = let_decl.bindings().collect();
    assert_eq!(bindings.len(), 2);
    let names: Vec<String> = bindings
        .iter()
        .map(|b| {
            let pat = b.pat().expect("binding pat");
            let crate::syntax::Pat::Named(named) = pat else {
                panic!("expected NAMED_PAT, got {pat:?}");
            };
            named
                .ident()
                .expect("NAMED_PAT has IDENT_TOK")
                .text()
                .to_string()
        })
        .collect();
    assert_eq!(names, vec!["f", "g"]);
}

/// Phase 4.2 — `let rec f = 1\nand g = 2\n`: both `rec` and `and`
/// in the same group. Verifies the chain still works when the LET
/// already has a `REC_TOK` child.
#[test]
fn let_rec_and_chain() {
    let source = "let rec f = 1\nand g = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1);
    let crate::syntax::ModuleDecl::Let(let_decl) = &decls[0] else {
        panic!("expected ModuleDecl::Let")
    };
    assert!(let_decl.is_rec());
    let bindings: Vec<_> = let_decl.bindings().collect();
    assert_eq!(bindings.len(), 2);
}

/// Phase 4.2 — chains can extend beyond two bindings. Still triggers
/// FS0576 (no `rec`), reported once at the `let` keyword regardless
/// of how many `and`s follow.
#[test]
fn let_and_chain_three_bindings() {
    let source = "let f = 1\nand g = 2\nand h = 3\n";
    let parse = parse(source);
    assert_eq!(
        parse.errors.len(),
        1,
        "expected exactly one diagnostic (the non-rec-and error reported once); got: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    assert_eq!(decls.len(), 1, "expected one decl; got: {decls:?}");
    let crate::syntax::ModuleDecl::Let(let_decl) = &decls[0] else {
        panic!("expected ModuleDecl::Let")
    };
    let bindings: Vec<_> = let_decl.bindings().collect();
    assert_eq!(bindings.len(), 3);
    let names: Vec<String> = bindings
        .iter()
        .map(|b| {
            let pat = b.pat().expect("pat");
            let crate::syntax::Pat::Named(named) = pat else {
                panic!("expected NAMED_PAT, got {pat:?}");
            };
            named.ident().expect("ident").text().to_string()
        })
        .collect();
    assert_eq!(names, vec!["f", "g", "h"]);
}

/// Phase 4.2 — an `and` whose column is *strictly less* than `let`'s
/// is offside: LexFilter pops `CtxtLetDecl` and emits a
/// `Virtual::DeclEnd` *before* the raw `and`, signalling that the
/// declaration has closed. The continuation scan must stop at
/// `DeclEnd` (rather than skipping every virtual) so that the trailing
/// `and` falls out to the impl-file loop and is rejected there,
/// matching FCS's "Unexpected keyword 'and' in implementation file"
/// behaviour.
#[test]
fn let_rec_does_not_fold_offside_and() {
    let source = "  let rec f = 1\nand g = 2\n";
    let parse = parse(source);
    // The LET_DECL must contain only one BINDING (`f`); the offside
    // `and g = 2` is not part of it.
    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decls: Vec<_> = module.decls().collect();
    let let_decl = decls
        .iter()
        .find_map(|d| match d {
            crate::syntax::ModuleDecl::Let(l) => Some(l),
            _ => None,
        })
        .expect("expected a LET_DECL");
    let bindings: Vec<_> = let_decl.bindings().collect();
    assert_eq!(
        bindings.len(),
        1,
        "expected the offside `and` to NOT be folded; got bindings: {bindings:?}",
    );
    // And the parser should have flagged the trailing `and` as an
    // unexpected token.
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("unexpected token")),
        "expected an 'unexpected token' error for the trailing `and`; got: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
}

/// Phase 4.2 error path — `let rec = 1\n`: the `rec` keyword is
/// accepted, but the next token (`=`) is not a valid head pattern.
/// `parse_named_pat` records "expected identifier after `let`",
/// `parse_let_equals_rhs` is skipped, the first `BINDING` closes
/// zero-width, and the `=` plus RHS escape to the impl-file loop. The
/// `REC_TOK` is still emitted inside `LET_DECL` so the typed-AST
/// projector observes `is_rec = true`. The test is mainly "doesn't
/// panic and the lossless invariant holds".
#[test]
fn let_rec_without_ident_records_error() {
    let source = "let rec = 1\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("expected pattern after `let`")),
        "expected an 'expected pattern after `let`' error; got: {:?}",
        parse.errors,
    );
    assert_lossless(source, &parse);
    // The REC_TOK still made it into the tree.
    assert!(
        parse
            .root
            .descendants_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::REC_TOK),
        "expected a REC_TOK in the tree even after the pat error",
    );
}

/// Phase 4.3 — `let mutable x = 1\n`: the `mutable` keyword is consumed
/// as a [`SyntaxKind::MUTABLE_TOK`] child of [`SyntaxKind::BINDING`]
/// (sitting between the BINDING's start and its `NAMED_PAT`). The
/// typed-AST projector reads the child's presence and reports
/// `is_mutable = true`, matching FCS's `SynBinding.isMutable`.
#[test]
fn let_binding_mutable() {
    let source = "let mutable x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..18
  MODULE_OR_NAMESPACE@0..18
    LET_DECL@0..17
      LET_TOK@0..3 \"let\"
      BINDING@3..17
        WHITESPACE@3..4 \" \"
        MUTABLE_TOK@4..11 \"mutable\"
        NAMED_PAT@11..13
          WHITESPACE@11..12 \" \"
          IDENT_TOK@12..13 \"x\"
        WHITESPACE@13..14 \" \"
        EQUALS_TOK@14..15 \"=\"
        WHITESPACE@15..16 \" \"
        ERROR@16..16 \"\"
        CONST_EXPR@16..17
          INT32_LIT@16..17 \"1\"
    NEWLINE@17..18 \"\\n\"
    ERROR@18..18 \"\"
    ERROR@18..18 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    assert!(binding.is_mutable(), "expected is_mutable = true");
    assert!(!binding.is_inline(), "expected is_inline = false");
}

/// Phase 4.3 — `let inline f = 1\n`: mirror of [`let_binding_mutable`]
/// for the `inline` modifier. Tree shape and typed-AST flag track the
/// presence of [`SyntaxKind::INLINE_TOK`].
#[test]
fn let_binding_inline() {
    let source = "let inline f = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..17
  MODULE_OR_NAMESPACE@0..17
    LET_DECL@0..16
      LET_TOK@0..3 \"let\"
      BINDING@3..16
        WHITESPACE@3..4 \" \"
        INLINE_TOK@4..10 \"inline\"
        NAMED_PAT@10..12
          WHITESPACE@10..11 \" \"
          IDENT_TOK@11..12 \"f\"
        WHITESPACE@12..13 \" \"
        EQUALS_TOK@13..14 \"=\"
        WHITESPACE@14..15 \" \"
        ERROR@15..15 \"\"
        CONST_EXPR@15..16
          INT32_LIT@15..16 \"1\"
    NEWLINE@16..17 \"\\n\"
    ERROR@17..17 \"\"
    ERROR@17..17 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    assert!(!binding.is_mutable());
    assert!(binding.is_inline());
}

/// Phase 4.3 — `let inline mutable x = 1\n`: FCS accepts the
/// canonical `opt_inline opt_mutable` order. Both flags project
/// `true` from the typed AST and both tokens land as siblings inside
/// the `BINDING` in source order.
#[test]
fn let_binding_inline_mutable() {
    let source = "let inline mutable x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    assert!(binding.is_inline(), "expected is_inline = true");
    assert!(binding.is_mutable(), "expected is_mutable = true");

    // The tokens appear in source order — INLINE_TOK before MUTABLE_TOK —
    // and both as children of the BINDING (not the LET_DECL).
    let binding_node = let_decl
        .syntax()
        .children()
        .find(|c| c.kind() == SyntaxKind::BINDING)
        .expect("BINDING child");
    let modifier_kinds: Vec<SyntaxKind> = binding_node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .map(|t| t.kind())
        .filter(|k| matches!(k, SyntaxKind::INLINE_TOK | SyntaxKind::MUTABLE_TOK))
        .collect();
    assert_eq!(
        modifier_kinds,
        vec![SyntaxKind::INLINE_TOK, SyntaxKind::MUTABLE_TOK],
    );
}

/// Phase 4.3 — `let mutable inline x = 1\n`: FCS rejects this ordering
/// with FS0010 ("Unexpected keyword 'inline' in binding"). We mirror
/// the diagnostic but recover by consuming both tokens, so the typed-AST
/// projector still reports both `is_inline` and `is_mutable` as `true`.
/// The error span covers the misplaced `inline` keyword.
#[test]
fn let_binding_mutable_inline_records_error() {
    let source = "let mutable inline x = 1\n";
    let parse = parse(source);
    assert!(
        parse
            .errors
            .iter()
            .any(|e| e.message.contains("`inline` must precede `mutable`")),
        "expected the reversed-modifier diagnostic; got: {:?}",
        parse.errors,
    );
    assert_eq!(
        parse.errors.len(),
        1,
        "expected exactly one diagnostic; got: {:?}",
        parse.errors,
    );
    // The diagnostic should pin the misplaced `inline` token (cols 12..18).
    assert_eq!(parse.errors[0].span, 12..18);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    // Both flags survive — the green tree is lossless and the typed-AST
    // projector reads token presence, not order.
    assert!(binding.is_mutable());
    assert!(binding.is_inline());
}

/// Phase 4.3 — `let rec inline f = 1\n`: `rec` is a LET_DECL-level
/// modifier while `inline` is a per-binding modifier. The two coexist
/// without interfering: `is_rec` and `is_inline` are independent and
/// the tree carries `REC_TOK` at LET_DECL level *and* `INLINE_TOK`
/// inside BINDING.
#[test]
fn let_rec_inline() {
    let source = "let rec inline f = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    assert!(let_decl.is_rec());
    let binding = let_decl.bindings().next().expect("binding");
    assert!(binding.is_inline());
    assert!(!binding.is_mutable());
}

/// Phase 4.3 — `let rec f = 1\nand mutable g = 2\n`: modifiers are
/// per-binding, not per-`let`. The first binding has no modifiers; the
/// second carries `mutable`. Verifies the `and`-chain loop calls
/// `parse_binding_modifiers` for *each* binding independently.
#[test]
fn let_and_chain_with_per_binding_modifiers() {
    let source = "let rec f = 1\nand mutable g = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let bindings: Vec<_> = let_decl.bindings().collect();
    assert_eq!(bindings.len(), 2);
    assert!(!bindings[0].is_mutable());
    assert!(!bindings[0].is_inline());
    assert!(bindings[1].is_mutable(), "second binding should be mutable");
    assert!(!bindings[1].is_inline());
}

/// Phase 4.4 — `let f x = 1\n`: function-form binding with a single
/// curried arg. The head pattern is `LONG_IDENT_PAT > [LONG_IDENT > "f",
/// NAMED_PAT > "x"]` (FCS's `SynPat.LongIdent` with `SynArgPats.Pats`
/// holding one `SynPat.Named`). Verifies the parser took the
/// function-form branch and produced the right children, and that the
/// typed-AST exposes `head()` / `args()` correctly.
#[test]
fn let_binding_function_form_single_arg() {
    let source = "let f x = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT, got {pat:?}");
    };
    let head = long_pat.head().expect("LONG_IDENT_PAT has LONG_IDENT");
    let head_idents: Vec<String> = head.idents().map(|t| t.text().to_string()).collect();
    assert_eq!(head_idents, vec!["f".to_string()]);
    let args: Vec<_> = long_pat.args().collect();
    assert_eq!(args.len(), 1);
    let crate::syntax::Pat::Named(arg0) = &args[0] else {
        panic!("expected NAMED_PAT arg, got {:?}", args[0]);
    };
    assert_eq!(arg0.ident().expect("arg ident").text(), "x");
}

/// Phase 4.4 — `let f x y z = 1\n`: function-form binding with three
/// curried args. Confirms the parser's arg-sweep loop pulls in every
/// trailing ident before the `=`.
#[test]
fn let_binding_function_form_multiple_args() {
    let source = "let f x y z = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT, got {pat:?}");
    };
    let arg_texts: Vec<String> = long_pat
        .args()
        .map(|a| {
            let crate::syntax::Pat::Named(named) = a else {
                panic!("expected NAMED_PAT arg, got {a:?}");
            };
            named.ident().expect("arg ident").text().to_string()
        })
        .collect();
    assert_eq!(arg_texts, vec!["x", "y", "z"]);
}

/// Explicit value-typar declarations on a function-form head —
/// `let identity<'a> (x: 'a) = x\n`. The `<'a>` promotes the head to
/// `LONG_IDENT_PAT` (FCS's `SynPat.LongIdent` with a `Some` `typars` slot) and
/// is parsed into a `TYPAR_DECLS` child sitting between the head `LONG_IDENT`
/// and the curried args. Verifies the head ident, the typar list, and that the
/// trailing arg still parses.
#[test]
fn let_binding_generic_function_head() {
    let source = "let identity<'a> (x: 'a) = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT, got {pat:?}");
    };
    let head = long_pat.head().expect("LONG_IDENT_PAT has LONG_IDENT");
    assert_eq!(
        head.idents()
            .map(|t| t.text().to_string())
            .collect::<Vec<_>>(),
        vec!["identity".to_string()]
    );
    let typar_decls = long_pat
        .typar_decls()
        .expect("generic head has TYPAR_DECLS");
    let typar_names: Vec<String> = typar_decls
        .typars()
        .map(|t| t.ident().expect("typar ident").text().to_string())
        .collect();
    assert_eq!(typar_names, vec!["a".to_string()]);
    // The `(x: 'a)` curried arg still parses (a single paren arg).
    assert_eq!(long_pat.args().count(), 1);
}

/// A *value-form* head carrying explicit typars and **no** curried args —
/// `let h<'a> = 3\n`. FCS promotes this to `SynPat.LongIdent` with empty
/// `args` (not a `SynPat.Named`), so the head must take the long-ident branch
/// on the strength of the `<` alone: `LONG_IDENT_PAT > [LONG_IDENT > "h",
/// TYPAR_DECLS]` with zero `Pat` args.
#[test]
fn let_binding_generic_value_head_no_args() {
    let source = "let h<'a> = 3\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT (generic value head), got {pat:?}");
    };
    let head = long_pat.head().expect("LONG_IDENT_PAT has LONG_IDENT");
    assert_eq!(
        head.idents()
            .map(|t| t.text().to_string())
            .collect::<Vec<_>>(),
        vec!["h".to_string()]
    );
    assert!(
        long_pat.typar_decls().is_some(),
        "value-form generic head carries TYPAR_DECLS"
    );
    assert_eq!(long_pat.args().count(), 0, "value-form head has no args");
}

/// Phase 4.4 — `let inline f x = x\n`: function-form binding combined
/// with `inline`. Both the modifier flag and the LONG_IDENT_PAT shape
/// must coexist — the modifier is consumed before
/// `parse_head_binding_pat`, which then sees `f x = …` and takes the
/// function-form branch.
#[test]
fn let_binding_inline_function_form() {
    let source = "let inline f x = x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    assert!(binding.is_inline());
    assert!(!binding.is_mutable());
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT, got {pat:?}");
    };
    let head = long_pat.head().expect("LONG_IDENT_PAT has LONG_IDENT");
    assert_eq!(
        head.idents()
            .map(|t| t.text().to_string())
            .collect::<Vec<_>>(),
        vec!["f".to_string()]
    );
    let args: Vec<_> = long_pat.args().collect();
    assert_eq!(args.len(), 1);
    let crate::syntax::Pat::Named(arg0) = &args[0] else {
        panic!("expected NAMED_PAT arg, got {:?}", args[0]);
    };
    assert_eq!(arg0.ident().expect("arg ident").text(), "x");
}

/// Phase 4.4 — `let rec f x = f x\n`: `rec` plus function-form. The
/// LET_DECL carries `is_rec = true` and the binding holds the
/// `LONG_IDENT_PAT` head; the RHS is a (recursive) application.
#[test]
fn let_rec_binding_function_form() {
    let source = "let rec f x = f x\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    assert!(let_decl.is_rec());
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT, got {pat:?}");
    };
    let args: Vec<_> = long_pat.args().collect();
    assert_eq!(args.len(), 1);
    let crate::syntax::Pat::Named(arg0) = &args[0] else {
        panic!("expected NAMED_PAT arg, got {:?}", args[0]);
    };
    assert_eq!(arg0.ident().expect("arg ident").text(), "x");
}

/// Phase 4.5 — `let _ = 1\n`: value-form wildcard binding. The head
/// pattern is `WILDCARD_PAT > [UNDERSCORE_TOK]`, mirroring FCS's
/// `SynPat.Wild`. Confirms the head-token branch in
/// `parse_head_binding_pat` picks `_` correctly without venturing
/// near the function-form path.
#[test]
fn let_binding_wildcard_head() {
    let source = "let _ = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::Wildcard(wild) = pat else {
        panic!("expected WILDCARD_PAT, got {pat:?}");
    };
    let tok = wild.underscore().expect("WILDCARD_PAT has UNDERSCORE_TOK");
    assert_eq!(tok.text(), "_");
}

/// Phase 4.5 — `let f _ = 1\n`: function-form binding with a single
/// wildcard arg. The head is `f` (LONG_IDENT_PAT), the lone arg is
/// `WILDCARD_PAT`. Confirms the arg-sweep loop's atomic-pat helper
/// handles `_`.
#[test]
fn let_binding_function_form_wildcard_arg() {
    let source = "let f _ = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT, got {pat:?}");
    };
    let args: Vec<_> = long_pat.args().collect();
    assert_eq!(args.len(), 1);
    assert!(
        matches!(&args[0], crate::syntax::Pat::Wildcard(_)),
        "expected WILDCARD_PAT arg, got {:?}",
        args[0]
    );
}

/// Phase 4.5 — `let f x _ y = 1\n`: function-form binding with a
/// mixed sequence of named and wildcard curried args. Verifies the
/// arg sweep correctly produces `[NAMED_PAT, WILDCARD_PAT,
/// NAMED_PAT]` in source order.
#[test]
fn let_binding_function_form_mixed_args() {
    let source = "let f x _ y = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT, got {pat:?}");
    };
    let kinds: Vec<&'static str> = long_pat
        .args()
        .map(|a| match a {
            crate::syntax::Pat::Named(_) => "named",
            crate::syntax::Pat::Wildcard(_) => "wildcard",
            crate::syntax::Pat::LongIdent(_) => "long_ident",
            crate::syntax::Pat::Paren(_) => "paren",
            crate::syntax::Pat::Const(_) => "const",
            crate::syntax::Pat::Null(_) => "null",
            crate::syntax::Pat::Typed(_) => "typed",
            crate::syntax::Pat::Tuple(_) => "tuple",
            crate::syntax::Pat::As(_) => "as",
            crate::syntax::Pat::ArrayOrList(_) => "array_or_list",
            crate::syntax::Pat::Record(_) => "record",
            crate::syntax::Pat::IsInst(_) => "isinst",
            crate::syntax::Pat::ListCons(_) => "list_cons",
            crate::syntax::Pat::Ands(_) => "ands",
            crate::syntax::Pat::Or(_) => "or",
            crate::syntax::Pat::Attrib(_) => "attrib",
            crate::syntax::Pat::OptionalVal(_) => "optional_val",
            crate::syntax::Pat::Quote(_) => "quote",
        })
        .collect();
    assert_eq!(kinds, vec!["named", "wildcard", "named"]);
}

/// Phase 4.5 — `let _ x = 1\n`: wildcard head with a trailing ident
/// is *not* function form in FCS (`SynPat.Wild` head + FS0010 on the
/// `x`). Confirms our parser mirrors that: the head is
/// `WILDCARD_PAT` and an error appears for the unexpected `x`.
#[test]
fn let_binding_wildcard_head_with_trailing_ident_errors() {
    let source = "let _ x = 1\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "expected a parse error on the trailing ident",
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    assert!(
        matches!(pat, crate::syntax::Pat::Wildcard(_)),
        "expected WILDCARD_PAT head (FCS does not promote `_` to function form), got {pat:?}",
    );
}

/// Phase 6.1 — `let () = ()` has a unit-pat head. FCS produces
/// `SynPat.Paren(SynPat.Const(SynConst.Unit, _), _)` (`pars.fsy:3832`,
/// 3873); our CST mirrors that as `PAREN_PAT > [LPAREN_TOK,
/// CONST_PAT (empty), RPAREN_TOK]`. The inner `CONST_PAT` is a
/// synthetic, tokenless node — the source parens belong to the
/// outer `PAREN_PAT`.
#[test]
fn let_binding_unit_value_head_emits_paren_around_empty_const() {
    let source = "let () = ()\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let inner = paren.inner().expect("PAREN_PAT must wrap an inner pat");
    let crate::syntax::Pat::Const(const_pat) = inner else {
        panic!("expected CONST_PAT inside PAREN_PAT for `()`, got {inner:?}");
    };
    assert!(
        const_pat.literal().is_none(),
        "the synthetic unit CONST_PAT must have no literal-token child",
    );
}

/// Phase 6.1 — `let (x) = 1` is a paren-wrapped named-pat head.
/// FCS produces `SynPat.Paren(SynPat.Named "x", _)`.
#[test]
fn let_binding_paren_named_head() {
    let source = "let (x) = 1\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let inner = paren.inner().expect("PAREN_PAT must wrap an inner pat");
    assert!(
        matches!(inner, crate::syntax::Pat::Named(_)),
        "expected NAMED_PAT inside PAREN_PAT for `(x)`, got {inner:?}",
    );
}

/// Phase 6.1 — `let null = 1` heads on the `null` keyword. FCS
/// gives `SynPat.Null _` (`pars.fsy:3829`).
#[test]
fn let_binding_null_value_head() {
    let source = "let null = 1\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    assert!(
        matches!(pat, crate::syntax::Pat::Null(_)),
        "expected NULL_PAT head, got {pat:?}",
    );
}

/// Phase 6.1 — `let 0 = 1` heads on an int literal. FCS gives
/// `SynPat.Const(SynConst.Int32 0, _)`. Value-form, no error.
#[test]
fn let_binding_int_lit_value_head() {
    let source = "let 0 = 1\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::Const(const_pat) = pat else {
        panic!("expected CONST_PAT head, got {pat:?}");
    };
    // For non-unit literals the CONST_PAT owns its literal token.
    assert!(
        const_pat.literal().is_some(),
        "non-unit CONST_PAT must contain its literal token",
    );
}

/// Phase 6.1 — `let f () = 1`: function form with a unit argument.
/// The single arg should be a `PAREN_PAT` wrapping the synthetic
/// unit `CONST_PAT`, matching FCS's `SynPat.LongIdent("f", [Paren(Const Unit)])`.
#[test]
fn let_binding_function_form_unit_arg() {
    let source = "let f () = 1\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let args: Vec<_> = long_pat.args().collect();
    assert_eq!(args.len(), 1, "expected exactly one arg, got {args:?}");
    let crate::syntax::Pat::Paren(paren) = &args[0] else {
        panic!("expected PAREN_PAT arg, got {:?}", args[0]);
    };
    let inner = paren.inner().expect("PAREN_PAT must wrap an inner pat");
    let crate::syntax::Pat::Const(const_pat) = inner else {
        panic!("expected CONST_PAT inside PAREN_PAT for `()`, got {inner:?}");
    };
    assert!(
        const_pat.literal().is_none(),
        "the synthetic unit CONST_PAT must have no literal-token child",
    );
}

/// Phase 5 Gap B — `let f (x) (y) = z`: two simple curried paren args.
/// Regression guard for the swallowed-`)` sweep gate — the fix must
/// still sweep *both* parens as separate args (each paren's swallowed
/// `)` ends only that arg, not the whole sweep).
#[test]
fn head_two_paren_args() {
    let source = "let f (x) (y) = z\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let args: Vec<_> = long_pat.args().collect();
    assert_eq!(
        args.len(),
        2,
        "expected two curried paren args, got {args:?}"
    );
    for (i, arg) in args.iter().enumerate() {
        assert!(
            matches!(arg, crate::syntax::Pat::Paren(_)),
            "arg #{i} should be PAREN_PAT, got {arg:?}",
        );
    }
}

/// Phase 5 Gap B — `let f (Some x) (Some y) = z`: two *curried* ctor-app
/// paren args. The single-checkpoint sweep used to peek the filtered
/// stream (where the first paren's `)` is swallowed) and fold
/// `(Some y)` into `Some`'s args. The raw-stream gate stops the sweep at
/// the `)`, so the head has two distinct `PAREN_PAT` args.
#[test]
fn head_curried_ctor_app_two_paren_args() {
    let source = "let f (Some x) (Some y) = z\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    let pat = first_binding_head_pat(&parse.root);
    let crate::syntax::Pat::LongIdent(long_pat) = pat else {
        panic!("expected LONG_IDENT_PAT head, got {pat:?}");
    };
    let head_idents: Vec<String> = long_pat
        .head()
        .expect("LONG_IDENT_PAT has LONG_IDENT")
        .idents()
        .map(|t| t.text().to_string())
        .collect();
    assert_eq!(
        head_idents,
        vec!["f".to_string()],
        "head should be just `f`"
    );
    let args: Vec<_> = long_pat.args().collect();
    assert_eq!(
        args.len(),
        2,
        "expected two curried ctor-app args, got {args:?}"
    );
    for (i, arg) in args.iter().enumerate() {
        let crate::syntax::Pat::Paren(paren) = arg else {
            panic!("arg #{i} should be PAREN_PAT, got {arg:?}");
        };
        let inner = paren.inner().expect("PAREN_PAT must wrap an inner pat");
        assert!(
            matches!(inner, crate::syntax::Pat::LongIdent(_)),
            "arg #{i} inner should be LONG_IDENT_PAT (`Some _`), got {inner:?}",
        );
    }
}

/// Phase 6.3 — `let x, y = 1, 2`: minimal binary tuple at the value
/// head. Asserts the flat `TUPLE_PAT > [NAMED_PAT, COMMA_TOK,
/// NAMED_PAT]` shape (FCS's `SynPat.Tuple` is a flat list, not
/// nested pairs).
#[test]
fn let_binding_tuple_value_head() {
    let source = "let x, y = 1, 2\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::Tuple(tuple) = pat else {
        panic!("expected TUPLE_PAT head, got {pat:?}");
    };
    let elements: Vec<_> = tuple.elements().collect();
    assert_eq!(
        elements.len(),
        2,
        "binary tuple has exactly two elements, got {elements:?}"
    );
    for (i, el) in elements.iter().enumerate() {
        assert!(
            matches!(el, crate::syntax::Pat::Named(_)),
            "tuple element {i}: expected NAMED_PAT, got {el:?}",
        );
    }
    let commas = tuple
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::COMMA_TOK)
        .count();
    assert_eq!(
        commas, 1,
        "binary TUPLE_PAT has exactly one COMMA_TOK separator",
    );
}

/// Phase 6.3 — `let (x, y) = 1, 2`: paren-wrapped binary tuple. The
/// outer head is `PAREN_PAT`, whose inner is the `TUPLE_PAT` of two
/// named elements.
#[test]
fn let_binding_paren_tuple_head() {
    let source = "let (x, y) = 1, 2\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::Paren(paren) = pat else {
        panic!("expected PAREN_PAT head, got {pat:?}");
    };
    let inner = paren.inner().expect("PAREN_PAT must wrap an inner pat");
    let crate::syntax::Pat::Tuple(tuple) = inner else {
        panic!("expected TUPLE_PAT inside PAREN_PAT, got {inner:?}");
    };
    let elements: Vec<_> = tuple.elements().collect();
    assert_eq!(elements.len(), 2, "binary tuple has exactly two elements");
}

/// Phase 6.3 — `let x, y, z = 1, 2, 3`: ternary tuple value head.
/// Pins the flat-list shape — three `NAMED_PAT` children with two
/// `COMMA_TOK`s in source order, *not* a nested pair.
#[test]
fn let_binding_ternary_tuple_value_head() {
    let source = "let x, y, z = 1, 2, 3\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "unexpected errors: {:?}",
        parse.errors
    );
    assert_lossless(source, &parse);

    use crate::syntax::AstNode;
    let file = crate::syntax::ImplFile::cast(parse.root.clone()).expect("ImplFile");
    let module = file.modules().next().expect("module");
    let decl = module.decls().next().expect("decl");
    let crate::syntax::ModuleDecl::Let(let_decl) = decl else {
        panic!("expected ModuleDecl::Let")
    };
    let binding = let_decl.bindings().next().expect("binding");
    let pat = binding.pat().expect("binding pat");
    let crate::syntax::Pat::Tuple(tuple) = pat else {
        panic!("expected TUPLE_PAT head, got {pat:?}");
    };
    let elements: Vec<_> = tuple.elements().collect();
    assert_eq!(elements.len(), 3, "ternary tuple has three elements");
    // No nested TUPLE_PAT in any element — flat shape.
    for el in &elements {
        assert!(
            !matches!(el, crate::syntax::Pat::Tuple(_)),
            "ternary TUPLE_PAT is flat — no element should itself be a tuple, got {el:?}",
        );
    }
    let commas = tuple
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::COMMA_TOK)
        .count();
    assert_eq!(commas, 2, "ternary TUPLE_PAT has two COMMA_TOK separators");
}

/// Phase 5.M.1 — `match x with A -> 1`: the minimal single-clause
/// form (no leading `|`, no `when` guard). Pins the full green shape:
/// `MATCH_EXPR > [MATCH_TOK, <scrutinee>, WITH_TOK, MATCH_CLAUSE >
/// [<pat>, RARROW_TOK, <result>, ε]]`. The two zero-width `ERROR`
/// leaves are the drained `Virtual::RightBlockEnd` (clause SeqBlock
/// close) and `Virtual::End` (`CtxtMatchClauses` close). A bare
/// capitalised clause head (`A`) is a ctor reference, so it parses as
/// `LONG_IDENT_PAT`, mirroring FCS's `SynPat.LongIdent`.
/// Phase 10.5 — full green-shape pin for a bare attribute carried on a
/// `let` binding. The `[< … >]` list is a *leading child* of the
/// `LET_DECL` (before `LET_TOK`, matching source order); the attribute
/// holds only its `LONG_IDENT` path (the `ArgExpr` is the normaliser's
/// synthetic unit, not a tree node). The binding parses cleanly with no
/// attribute-related errors.
#[test]
fn attribute_on_let_binding_tree_shape() {
    let source = "[<Foo>] let x = 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "attributed let should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..18
  MODULE_OR_NAMESPACE@0..18
    LET_DECL@0..17
      ATTRIBUTE_LIST@0..7
        LBRACK_LESS_TOK@0..2 \"[<\"
        ATTRIBUTE@2..5
          LONG_IDENT@2..5
            IDENT_TOK@2..5 \"Foo\"
        GREATER_RBRACK_TOK@5..7 \">]\"
      WHITESPACE@7..8 \" \"
      LET_TOK@8..11 \"let\"
      BINDING@11..17
        NAMED_PAT@11..13
          WHITESPACE@11..12 \" \"
          IDENT_TOK@12..13 \"x\"
        WHITESPACE@13..14 \" \"
        EQUALS_TOK@14..15 \"=\"
        WHITESPACE@15..16 \" \"
        ERROR@16..16 \"\"
        CONST_EXPR@16..17
          INT32_LIT@16..17 \"1\"
    NEWLINE@17..18 \"\\n\"
    ERROR@18..18 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.5a — green-shape pin for two `;`-separated attributes in one
/// `[<A; B>]` list. Both `ATTRIBUTE` nodes sit inside a single `ATTRIBUTE_LIST`,
/// separated by a `SEMI_TOK`, distinct from the two-list `[<A>] [<B>]` form. The
/// binding parses cleanly with no attribute-related errors.
#[test]
fn attribute_list_semicolon_separated_tree_shape() {
    let source = "[<A; B>] let x = 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "semicolon-separated attribute list should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..19
  MODULE_OR_NAMESPACE@0..19
    LET_DECL@0..18
      ATTRIBUTE_LIST@0..8
        LBRACK_LESS_TOK@0..2 \"[<\"
        ATTRIBUTE@2..3
          LONG_IDENT@2..3
            IDENT_TOK@2..3 \"A\"
        SEMI_TOK@3..4 \";\"
        ATTRIBUTE@4..6
          LONG_IDENT@4..6
            WHITESPACE@4..5 \" \"
            IDENT_TOK@5..6 \"B\"
        GREATER_RBRACK_TOK@6..8 \">]\"
      WHITESPACE@8..9 \" \"
      LET_TOK@9..12 \"let\"
      BINDING@12..18
        NAMED_PAT@12..14
          WHITESPACE@12..13 \" \"
          IDENT_TOK@13..14 \"x\"
        WHITESPACE@14..15 \" \"
        EQUALS_TOK@15..16 \"=\"
        WHITESPACE@16..17 \" \"
        ERROR@17..17 \"\"
        CONST_EXPR@17..18
          INT32_LIT@17..18 \"1\"
    NEWLINE@18..19 \"\\n\"
    ERROR@19..19 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.5b — green-shape pin for an attribute *argument*. `[<Foo(1, 2)>]`
/// is `ATTRIBUTE > [LONG_IDENT, <HPP-marker ERROR>, PAREN_EXPR]`: the adjacent
/// `(` is preceded by LexFilter's `HighPrecedenceParenApp` marker, consumed as
/// a zero-width `ERROR`, then the parenthesised tuple is the argument atom. The
/// binding parses cleanly with no errors.
#[test]
fn attribute_arg_paren_tuple_tree_shape() {
    let source = "[<Foo(1, 2)>] let x = 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "attribute with a paren argument should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..24
  MODULE_OR_NAMESPACE@0..24
    LET_DECL@0..23
      ATTRIBUTE_LIST@0..13
        LBRACK_LESS_TOK@0..2 \"[<\"
        ATTRIBUTE@2..11
          LONG_IDENT@2..5
            IDENT_TOK@2..5 \"Foo\"
          ERROR@5..5 \"\"
          PAREN_EXPR@5..11
            LPAREN_TOK@5..6 \"(\"
            TUPLE_EXPR@6..10
              CONST_EXPR@6..7
                INT32_LIT@6..7 \"1\"
              COMMA_TOK@7..8 \",\"
              CONST_EXPR@8..10
                WHITESPACE@8..9 \" \"
                INT32_LIT@9..10 \"2\"
            RPAREN_TOK@10..11 \")\"
        GREATER_RBRACK_TOK@11..13 \">]\"
      WHITESPACE@13..14 \" \"
      LET_TOK@14..17 \"let\"
      BINDING@17..23
        NAMED_PAT@17..19
          WHITESPACE@17..18 \" \"
          IDENT_TOK@18..19 \"x\"
        WHITESPACE@19..20 \" \"
        EQUALS_TOK@20..21 \"=\"
        WHITESPACE@21..22 \" \"
        ERROR@22..22 \"\"
        CONST_EXPR@22..23
          INT32_LIT@22..23 \"1\"
    NEWLINE@23..24 \"\\n\"
    ERROR@24..24 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.5c — green-shape pin for an attribute *target*. `[<assembly: Foo>]`
/// is `ATTRIBUTE > [ATTRIBUTE_TARGET > [IDENT_TOK, COLON_TOK], LONG_IDENT]`: the
/// `assembly:` prefix precedes the `Foo` path. Parses cleanly with no errors.
#[test]
fn attribute_target_tree_shape() {
    let source = "[<assembly: Foo>] let x = 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "attribute with a target should parse cleanly, got: {:?}",
        parse.errors,
    );
    let expected = "\
IMPL_FILE@0..28
  MODULE_OR_NAMESPACE@0..28
    LET_DECL@0..27
      ATTRIBUTE_LIST@0..17
        LBRACK_LESS_TOK@0..2 \"[<\"
        ATTRIBUTE@2..15
          ATTRIBUTE_TARGET@2..11
            IDENT_TOK@2..10 \"assembly\"
            COLON_TOK@10..11 \":\"
          LONG_IDENT@11..15
            WHITESPACE@11..12 \" \"
            IDENT_TOK@12..15 \"Foo\"
        GREATER_RBRACK_TOK@15..17 \">]\"
      WHITESPACE@17..18 \" \"
      LET_TOK@18..21 \"let\"
      BINDING@21..27
        NAMED_PAT@21..23
          WHITESPACE@21..22 \" \"
          IDENT_TOK@22..23 \"x\"
        WHITESPACE@23..24 \" \"
        EQUALS_TOK@24..25 \"=\"
        WHITESPACE@25..26 \" \"
        ERROR@26..26 \"\"
        CONST_EXPR@26..27
          INT32_LIT@26..27 \"1\"
    NEWLINE@27..28 \"\\n\"
    ERROR@28..28 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
}

/// Phase 10.5b — a *source-identifier* argument (`[<Foo __LINE__>]`).
/// `__LINE__` / `__SOURCE_FILE__` lex as `Token::KeywordString` and reach FCS's
/// `atomicExprAfterType` through `constant` → `sourceIdentifier` (verified
/// `ParseHadErrors: false`, `ArgExpr = Const(SourceIdentifier)`), so they are a
/// valid attribute argument — distinct from a bare ident (`[<Foo Bar>]`, which
/// is an FCS parse error). This pins the exact green-tree shape
/// (`SOURCE_IDENTIFIER_LIT`); the cross-check against FCS lives in the
/// differential `diff_ast_attribute_source_identifier_arg` (the normaliser now
/// models the `SourceIdentifier` const).
#[test]
fn attribute_arg_source_identifier() {
    let source = "[<Foo __LINE__>] let x = 1\n";
    let parse = parse(source);
    assert_lossless(source, &parse);
    assert!(
        parse.errors.is_empty(),
        "a source-identifier attribute argument should parse cleanly, got: {:?}",
        parse.errors,
    );
    assert!(
        parse
            .root
            .descendants_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::SOURCE_IDENTIFIER_LIT),
        "expected the `__LINE__` argument as a SOURCE_IDENTIFIER_LIT, got tree:\n{}",
        debug_tree(&parse.root),
    );
}

/// Phase 10.5c — the `module:` attribute target is **not** supported and is
/// flagged, matching FCS — `[<module: Foo>]` is itself an FCS parse error
/// (`ParseHadErrors: true`, verified) because the `module` keyword drives
/// LexFilter's module-head machinery even inside `[< … >]`, making the grammar's
/// `moduleKeyword COLON` arm unreachable here. `module` is excluded from the
/// target-head set, so it falls through to the path parser's "expected attribute
/// name" recovery. (`type:` covers the keyword-target path.)
#[test]
fn attribute_target_module_is_flagged() {
    let parse = parse("[<module: Foo>] let x = 1\n");
    assert!(
        !parse.errors.is_empty(),
        "the unsupported `module:` attribute target must record a parse error",
    );
}

/// Phase 10.5a — malformed attribute separators are flagged, matching FCS
/// (`ParseHadErrors: true`, all verified against `fcs-dump ast`). FCS's `seps`
/// is a single separator group, so a *repeated* separator (`[<A; ; B>]`,
/// `[<A; ;>]`) is invalid: consuming exactly one group per gap leaves the extra
/// to trip `parse_attribute`'s "expected attribute name" recovery. An *indented*
/// continuation with no `;` (`[<A\n  B>]`) emits no offside `Virtual::BlockSep`,
/// so there is no separator at all and the stray attribute trips the `>]` check.
/// (The column-0-aligned no-`;` form `[<A\nB>]` *does* emit a `BlockSep` and is
/// accepted — see the diff tests.)
#[test]
fn attribute_list_malformed_separator_is_flagged() {
    for source in [
        "[<A; ; B>] let x = 1\n",
        "[<A; ;>] let x = 1\n",
        "[<A\n  B>] let x = 1\n",
    ] {
        let parse = parse(source);
        assert!(
            !parse.errors.is_empty(),
            "malformed attribute separators in {source:?} should be flagged, got no errors",
        );
    }
}

/// Phase 10.7 — a leading `[< … >]` not attached to a carrier is now a standalone
/// `SynModuleDecl.Attributes` (`[<assembly: Foo>]`, no longer flagged — see the
/// standalone diff tests); a *nested* `module M = …` header carries its attrs
/// (10.7d); a whole-file `module Foo` header carries its attrs (10.7e); and an
/// `exception` carries its attrs (10.7m). The *remaining* deferred carriers — a
/// `namespace` header (FCS error 530 rejects attributes there) and `open`
/// attributes — are still flagged loudly rather than emitting a divergent shape.
/// Pins the boundary; superseded as those carriers land.
#[test]
fn attribute_on_deferred_carrier_is_flagged() {
    for source in [
        "[<AutoOpen>]\nnamespace Foo\n", // namespace header attrs (FCS error 530)
        "[<A>] open System\n",           // FCS rejects attrs before `open`
    ] {
        let parse = parse(source);
        assert!(
            parse
                .errors
                .iter()
                .any(|e| e.message.contains("phase-10.7 slice")),
            "expected a deferred-carrier diagnostic for {source:?}, got: {:?}",
            parse.errors,
        );
    }
}

/// Phase 10.7e — a no-`=` `module` head appearing *after* an existing
/// `module`/`namespace` header is not a second whole-file header (FCS rejects it,
/// error 10). The whole-file attributed-header branch is gated on `!header_parsed`
/// (not just `!seen_decl` — the file header isn't a loop decl, so `seen_decl`
/// stays false), so these stay in the deferred-carrier path rather than emitting
/// a spurious second header.
#[test]
fn attribute_on_non_leading_module_header_is_flagged() {
    for source in [
        "module Foo\n[<A>]\nmodule Bar\nlet x = 1\n", // header already a `module`
        "namespace N\n[<A>]\nmodule M\nlet x = 1\n",  // header already a `namespace`
    ] {
        let parse = parse(source);
        assert!(
            parse
                .errors
                .iter()
                .any(|e| e.message.contains("phase-10.7 slice")),
            "expected a deferred-carrier diagnostic for {source:?}, got: {:?}",
            parse.errors,
        );
    }
}

// ---------------------------------------------------------------------------
// Sequential `let`/function-binding RHS (stage 3). Complements the FCS diff
// tests in `tests/all/parser_diff_let_bindings.rs` with tree-shape + lossless
// guards that don't need the oracle.
// ---------------------------------------------------------------------------

/// First `BINDING` node's RHS `Expr`, for shape assertions.
fn first_binding_rhs(parse: &Parse) -> crate::syntax::Expr {
    use crate::syntax::AstNode;
    let binding = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::BINDING)
        .and_then(crate::syntax::Binding::cast)
        .expect("a BINDING node");
    binding.expr().expect("binding RHS expr")
}

/// `let x = printf "a"; 1` — explicit `;` sequences the RHS into a
/// `SEQUENTIAL_EXPR` with two statements, no errors, lossless.
#[test]
fn let_rhs_semi_sequential_shape() {
    let source = "let x = printf \"a\"; 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    match first_binding_rhs(&parse) {
        crate::syntax::Expr::Sequential(s) => {
            assert_eq!(s.statements().count(), 2, "two sequenced statements");
        }
        other => panic!("expected Sequential RHS, got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// `let f y =⏎    printf "a"⏎    y` — offside layout sequences the function
/// body into a two-statement `SEQUENTIAL_EXPR`, no errors, lossless.
#[test]
fn let_rhs_offside_sequential_shape() {
    let source = "let f y =\n    printf \"a\"\n    y\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    match first_binding_rhs(&parse) {
        crate::syntax::Expr::Sequential(s) => {
            assert_eq!(s.statements().count(), 2, "two sequenced statements");
        }
        other => panic!("expected Sequential RHS, got {other:?}"),
    }
    assert_lossless(source, &parse);
}

/// `let x = a;; let y = b` — `;;` (`Token::SemiSemi`) is a top-level decl
/// *separator* (`topSeparator: SEMICOLON_SEMICOLON`, `pars.fsy:6967`), not a
/// sequential. It must NOT absorb the following decl: the RHS of the first
/// binding stays a bare `IDENT_EXPR`, the tree has two `LET_DECL`s and no
/// `SEQUENTIAL_EXPR`. FCS emits no diagnostic for a post-decl `;;`, so neither
/// do we — the `;;` lands as a `SEMISEMI_TOK` between the two decls.
#[test]
fn let_rhs_semisemi_does_not_sequence() {
    let source = "let x = a;; let y = b\n";
    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "post-decl `;;` is a clean separator; errors: {:?}",
        parse.errors,
    );
    assert!(
        !tree_contains_kind(&parse.root, SyntaxKind::SEQUENTIAL_EXPR),
        "the `;;` must terminate, not sequence: {}",
        debug_tree(&parse.root),
    );
    let let_decls = parse
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::LET_DECL)
        .count();
    assert_eq!(let_decls, 2, "two separate let decls");
    assert_eq!(
        token_count(&parse.root, SyntaxKind::SEMISEMI_TOK),
        1,
        "the `;;` separator is a SEMISEMI_TOK: {}",
        debug_tree(&parse.root),
    );
    assert_lossless(source, &parse);
}

/// A trailing top-level `;;` (`let x = 1;;`) is a clean separator: no error,
/// one `LET_DECL`, one `SEMISEMI_TOK`. FCS emits no diagnostic.
#[test]
fn trailing_semisemi_is_clean() {
    let source = "let x = 1;;\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::LET_DECL)
            .count(),
        1,
        "one let decl",
    );
    assert_eq!(token_count(&parse.root, SyntaxKind::SEMISEMI_TOK), 1);
    assert_lossless(source, &parse);
}

/// A run of top-level separators (`;;;;`) is allowed (`topSeparators:
/// topSeparator topSeparators`, `pars.fsy:6972`): each `;;` lands as its own
/// `SEMISEMI_TOK`, the two decls survive, and no error is raised.
#[test]
fn semisemi_run_is_clean() {
    let source = "let x = 1;;;;\nlet y = 2\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert_eq!(
        parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::LET_DECL)
            .count(),
        2,
        "two let decls",
    );
    assert_eq!(
        token_count(&parse.root, SyntaxKind::SEMISEMI_TOK),
        2,
        "two `;;` separators",
    );
    assert_lossless(source, &parse);
}

/// `;;` separates non-`let` decls too. After `open System` there is no
/// `BlockEnd` virtual before the `;;` (so the loop's `needs_sep` is still
/// set when it arrives) — the separator must clear it so the following `let`
/// parses cleanly rather than cascading into "unsupported token Let" errors.
#[test]
fn semisemi_after_open_is_clean() {
    let source = "open System;; let y = b\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    assert!(
        parse
            .root
            .descendants()
            .any(|n| n.kind() == SyntaxKind::OPEN_DECL),
        "open decl survives: {}",
        debug_tree(&parse.root),
    );
    assert_eq!(
        parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::LET_DECL)
            .count(),
        1,
        "the let decl after `;;` parses",
    );
    assert_eq!(token_count(&parse.root, SyntaxKind::SEMISEMI_TOK), 1);
    assert_lossless(source, &parse);
}

/// A *leading* top-level `;;` (before any decl) is NOT a separator: FCS
/// rejects it ("Unexpected symbol ';;' in implementation file"), since
/// `topSeparators` only follows a `moduleDefnOrDirective`. We keep emitting
/// an error there and must NOT silently swallow it as a `SEMISEMI_TOK`.
#[test]
fn leading_semisemi_still_errors() {
    let source = ";;\nlet x = 1\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a leading `;;` is an error (FCS rejects it)",
    );
    assert_eq!(
        token_count(&parse.root, SyntaxKind::SEMISEMI_TOK),
        0,
        "a leading `;;` is not treated as a separator token",
    );
    // The recovery still surfaces the trailing decl.
    assert_eq!(
        parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::LET_DECL)
            .count(),
        1,
        "the trailing let decl survives recovery",
    );
    assert_lossless(source, &parse);
}

/// A *leading* single `;` is likewise not a separator — `topSeparators` only
/// follows a `moduleDefnOrDirective`, so FCS rejects it. We error and must NOT
/// emit a `SEMI_TOK` separator (the post-decl `;` path is gated on `seen_decl`).
#[test]
fn leading_semi_still_errors() {
    let source = "; let x = 1\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a leading `;` is an error (FCS rejects it)",
    );
    assert_eq!(
        token_count(&parse.root, SyntaxKind::SEMI_TOK),
        0,
        "a leading `;` is not treated as a separator token",
    );
    assert_lossless(source, &parse);
}

/// A single `;` immediately after a *type definition* is still inside the
/// type's offside block (`CtxtTypeDefns`; only `;;` closes it,
/// `LexFilter.fs:1806`), so FCS rejects `type T = int; open System`. The decl
/// loop's single-`;` separator is gated on the offside-block `depth == 0`, so at
/// the type body's positive depth the `;` is *not* swallowed as a clean separator —
/// it errors, and emits no `SEMI_TOK`. (`;;` *does* close the block, so
/// `type T = int;; open System` stays clean — covered by the diff tests.)
#[test]
fn semi_inside_type_block_errors() {
    let source = "type T = int; open System\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a `;` inside an unclosed type block is an error (FCS rejects it)",
    );
    assert_eq!(
        token_count(&parse.root, SyntaxKind::SEMI_TOK),
        0,
        "a type-body `;` is not treated as a top separator token",
    );
    assert_lossless(source, &parse);
}

/// A raw inline `let` after a `;` that was *rejected* inside an unclosed type
/// body must not be promoted to a module `LET_DECL`. In
/// `type T =`⏎`    | A`⏎`    ; let x = 1` the body's `OBLOCKSEP` clears
/// `needs_sep` while the offside-block `depth` stays positive (the union body
/// has not closed), and the `;` falls through as an error. The raw-`let` decl
/// arm is gated on `depth == 0`, so the `let` is *not* lifted out of the
/// malformed body as a top-level binding (FCS also rejects this input). It
/// falls through to the generic expression recovery instead — the point is only
/// that no module `LET_DECL` escapes the open block.
#[test]
fn raw_let_after_rejected_semi_in_type_body_is_contained() {
    let source = "type T =\n    | A\n    ; let x = 1\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a `;` inside an unclosed type body is an error (FCS rejects it)",
    );
    assert_eq!(
        parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::LET_DECL)
            .count(),
        0,
        "the inline `let` is not promoted to a module-level binding at positive depth",
    );
    assert_lossless(source, &parse);
}

/// A module abbreviation with a trailing single `;` (`module M = N;`). FCS
/// classifies the body as a `ModuleAbbrev` (the `;` is a `topSeparator`), but
/// the `;` does not close the body's offside block. A fully FCS-faithful
/// `module M = N; <sibling>` (where the sibling scopes to the *enclosing*
/// module) needs the abbreviation body's block extent reworked; instead we
/// classify `M = N` as a `MODULE_ABBREV_DECL` and drain the trailing content
/// (and the body's `OBLOCKEND`) as recovery, failing *loudly* rather than
/// silently reparenting the trailing decls inside `M`. (`module M = N;;` *does*
/// close the block and is clean — covered by the diff tests.)
#[test]
fn module_abbrev_trailing_semi_errors_loudly() {
    let source = "module M = N; open System\n";
    let parse = parse(source);
    assert!(
        !parse.errors.is_empty(),
        "a `;` inside an unclosed module-abbreviation body is an error",
    );
    assert_eq!(
        parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::MODULE_ABBREV_DECL)
            .count(),
        1,
        "`module M = N` stays a module abbreviation (not a nested module)",
    );
    assert_lossless(source, &parse);
}

/// The abbreviation body's `OBLOCKEND` must be drained *inside* the abbreviation,
/// not leaked. When `module M = N;` is nested in another module
/// (`module Outer =`⏎`    module M = N; open System`⏎`    let z = 1`), leaving
/// `M`'s pending `OBLOCKEND` for the outer loop would, at `BodyScope::Nested`,
/// terminate `Outer` and reparent `let z = 1` outside it. The drain keeps the
/// failure local, so the trailing `let z = 1` stays a child of `Outer`.
#[test]
fn module_abbrev_trailing_semi_does_not_escape_enclosing_module() {
    let source = "module Outer =\n    module M = N; open System\n    let z = 1\n";
    let parse = parse(source);
    // `Outer` is the sole top-level module/namespace; `let z = 1` is *its* child,
    // a sibling of the `module M = N` abbreviation — not reparented to file scope.
    let outer = parse
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::NESTED_MODULE_DECL)
        .expect("`module Outer` parses as a nested module decl");
    assert_eq!(
        outer
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::LET_DECL)
            .count(),
        1,
        "`let z = 1` stays inside `Outer` (M's OBLOCKEND did not leak)",
    );
    assert_eq!(
        parse
            .root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::LET_DECL)
            .count(),
        1,
        "exactly one `let` overall — it did not escape to file scope",
    );
    assert_lossless(source, &parse);
}

/// Return-type annotation on a value binding head — `let bar: int = 1`.
/// The colon binds the type to the binding (FCS's `SynBindingReturnInfo`),
/// not to the head pattern, so `bar` stays a bare `NAMED_PAT` and the type
/// lands in a sibling `BINDING_RETURN_INFO > [COLON_TOK, <type>]` between the
/// pattern and `=`. No parse errors.
#[test]
fn let_binding_value_return_type() {
    let source = "let bar: int = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..17
  MODULE_OR_NAMESPACE@0..17
    LET_DECL@0..16
      LET_TOK@0..3 \"let\"
      BINDING@3..16
        NAMED_PAT@3..7
          WHITESPACE@3..4 \" \"
          IDENT_TOK@4..7 \"bar\"
        BINDING_RETURN_INFO@7..12
          COLON_TOK@7..8 \":\"
          WHITESPACE@8..9 \" \"
          LONG_IDENT_TYPE@9..12
            LONG_IDENT@9..12
              IDENT_TOK@9..12 \"int\"
        WHITESPACE@12..13 \" \"
        EQUALS_TOK@13..14 \"=\"
        WHITESPACE@14..15 \" \"
        ERROR@15..15 \"\"
        CONST_EXPR@15..16
          INT32_LIT@15..16 \"1\"
    NEWLINE@16..17 \"\\n\"
    ERROR@17..17 \"\"
    ERROR@17..17 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Operator-named binding head, applied form — `let (+) a = a`. The
/// parenthesised operator name (FCS's `opName`) promotes to a function-form
/// `LONG_IDENT_PAT` whose `LONG_IDENT` head carries the operator token as
/// `[LPAREN_TOK, IDENT_TOK("+"), RPAREN_TOK]` (the pattern analogue of the
/// expression-side operator-value), followed by the swept curried arg `a`.
/// Pins the green shape and the lossless round-trip; the FCS-equivalence
/// is covered by `diff_ast_let_operator_head_*`.
#[test]
fn let_binding_operator_head_applied() {
    let source = "let (+) a = a\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..14
  MODULE_OR_NAMESPACE@0..14
    LET_DECL@0..13
      LET_TOK@0..3 \"let\"
      BINDING@3..13
        LONG_IDENT_PAT@3..9
          LONG_IDENT@3..7
            WHITESPACE@3..4 \" \"
            LPAREN_TOK@4..5 \"(\"
            IDENT_TOK@5..6 \"+\"
            RPAREN_TOK@6..7 \")\"
          NAMED_PAT@7..9
            WHITESPACE@7..8 \" \"
            IDENT_TOK@8..9 \"a\"
        WHITESPACE@9..10 \" \"
        EQUALS_TOK@10..11 \"=\"
        WHITESPACE@11..12 \" \"
        ERROR@12..12 \"\"
        IDENT_EXPR@12..13
          IDENT_TOK@12..13 \"a\"
    NEWLINE@13..14 \"\\n\"
    ERROR@14..14 \"\"
    ERROR@14..14 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

/// Operator-named binding head, nullary form — `let (+) = 1`. With no
/// curried args the operator name stays a value-form `NAMED_PAT` (FCS's
/// `SynPat.Named`, *not* `LongIdent`), carrying the operator token as
/// `[LPAREN_TOK, IDENT_TOK("+"), RPAREN_TOK]` directly — no `LONG_IDENT`
/// wrapper. Pins that the args lookahead correctly declines promotion.
#[test]
fn let_binding_operator_head_nullary() {
    let source = "let (+) = 1\n";
    let parse = parse(source);
    assert!(parse.errors.is_empty(), "errors: {:?}", parse.errors);
    let expected = "\
IMPL_FILE@0..12
  MODULE_OR_NAMESPACE@0..12
    LET_DECL@0..11
      LET_TOK@0..3 \"let\"
      BINDING@3..11
        NAMED_PAT@3..7
          WHITESPACE@3..4 \" \"
          LPAREN_TOK@4..5 \"(\"
          IDENT_TOK@5..6 \"+\"
          RPAREN_TOK@6..7 \")\"
        WHITESPACE@7..8 \" \"
        EQUALS_TOK@8..9 \"=\"
        WHITESPACE@9..10 \" \"
        ERROR@10..10 \"\"
        CONST_EXPR@10..11
          INT32_LIT@10..11 \"1\"
    NEWLINE@11..12 \"\\n\"
    ERROR@12..12 \"\"
    ERROR@12..12 \"\"
";
    assert_eq!(debug_tree(&parse.root), expected);
    assert_lossless(source, &parse);
}

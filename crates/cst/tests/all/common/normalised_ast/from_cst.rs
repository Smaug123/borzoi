//! Our-side projector: walk the typed AST under a [`Parse`] into the
//! normalised model.

use borzoi_cst::directives::{Directive, recognise_directive};
use borzoi_cst::parser::Parse;
use borzoi_cst::syntax::{
    ActivePatName, AddressOfExpr, AppExpr, AppType, AsPat, AssignExpr, AstNode, Attribute,
    AttributeList, AutoPropertyKind, Binding, ConsExpr, ConstExpr, ConstPat, DotGetExpr,
    DotIndexedGetExpr, DowncastExpr, DynamicExpr, EnumCase, ExceptionDefnDecl, Expr,
    ExternCTypeBase, ExternCTypeSuffix, ExternDecl, FunExpr, GetSetAccessor, HashDirectiveDecl,
    IdentExpr, IfThenElseExpr, ImplFile, ImplicitCtor, IndexRangeExpr, InterpStringExpr,
    InterpStringPart, JoinInExpr, LibraryOnlyFieldGetExpr, LongIdent, LongIdentAppType,
    LongIdentExpr, LongIdentPat, LongIdentType, MatchBangExpr, MatchClause, MatchExpr,
    MatchLambdaExpr, Measure, MeasureLitExpr, MeasurePowerType, MemberDefn, MemberLeading,
    MemberSig, MemberSigLeading, ModuleDecl, ModuleOrNamespace, ModuleOrNamespaceKind, NamedPat,
    NewExpr, ObjExpr, OpenDecl, ParenExpr, ParenPat, Pat, RationalConst, RecordField,
    RecordFieldDecl, SigDecl, SigFile, StaticOptCondition, StaticOptimizationExpr, SyntaxKind,
    SyntaxToken, TraitCallExpr, TupleExpr, TuplePat, TupleSegment, TupleType, TyparConstraint,
    TyparConstraintKind, TyparDecl, Type, TypeAppExpr, TypeDefn, TypeDefnRepr, TypeTestExpr,
    TypedExpr, TypedPat, UnionCase, UnionCaseField, UpcastExpr, ValField, VarType,
};

use super::decode::*;
use super::model::*;

// ============================================================================
// Our-side projector: walk the typed AST under a [`Parse`].
// ============================================================================

/// Map an `ACCESS_TOK` token (whose text is the `private`/`internal`/`public`
/// keyword our parser renamed it from) to a [`NormalisedAccess`].
fn cst_access_from_token(t: &SyntaxToken) -> NormalisedAccess {
    match t.text() {
        "private" => NormalisedAccess::Private,
        "internal" => NormalisedAccess::Internal,
        "public" => NormalisedAccess::Public,
        other => panic!("unexpected ACCESS_TOK text {other:?}"),
    }
}

/// Read the accessibility modifier a facade node captured as a *direct-child*
/// `ACCESS_TOK` token. `None` when the node has no such child. Every access
/// site except the type header (which has two candidate slots — see
/// [`cst_type_header_access`]) carries at most one such token.
fn cst_access(node: &borzoi_cst::syntax::SyntaxNode) -> Option<NormalisedAccess> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::ACCESS_TOK)
        .map(|t| cst_access_from_token(&t))
}

/// The type definition's *own* before-name access (`type internal Foo`): the
/// `ACCESS_TOK` that precedes the name's `LONG_IDENT`. A `TYPE_DEFN` can also
/// carry an after-name `ACCESS_TOK` (`type C private = …`, which FCS *discards*
/// — no `ImplicitCtor`, `SynComponentInfo.accessibility` stays `None`), so this
/// stops at the name and ignores any access at or after it.
fn cst_type_header_access(node: &borzoi_cst::syntax::SyntaxNode) -> Option<NormalisedAccess> {
    for elem in node.children_with_tokens() {
        match elem {
            rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::ACCESS_TOK => {
                return Some(cst_access_from_token(&t));
            }
            // The name is the first `LONG_IDENT` child; a before-name access
            // must precede it. Anything from here on is the after-name slot.
            rowan::NodeOrToken::Node(n) if n.kind() == SyntaxKind::LONG_IDENT => return None,
            _ => {}
        }
    }
    None
}

/// The access modifier appearing *before* the first `IDENT_TOK` (a construct's
/// name). Used for an auto-property, whose *overall* access (`member val
/// private X`) sits before the name, while a trailing `with private get`
/// accessor modifier — which FCS homes on the getter slot, not the property's
/// overall `SynValSigAccess` field — sits after and must be ignored.
fn cst_access_before_name_ident(node: &borzoi_cst::syntax::SyntaxNode) -> Option<NormalisedAccess> {
    for elem in node.children_with_tokens() {
        if let rowan::NodeOrToken::Token(t) = elem {
            match t.kind() {
                SyntaxKind::ACCESS_TOK => return Some(cst_access_from_token(&t)),
                SyntaxKind::IDENT_TOK => return None,
                _ => {}
            }
        }
    }
    None
}

/// The *leading* accessibility of a signature member (`SynMemberSig.Member`),
/// which FCS keeps in `SynValSig.accessibility` (the overall `SynValSigAccess`
/// slot). A `private new : …` ctor sig captures the modifier as a `MEMBER_SIG`
/// child *before* the `VAL_SIG`; a `member internal M : …` captures it inside
/// the `VAL_SIG`, before the name. A *trailing* accessor modifier
/// (`member P : int with private get`) sits after the `VAL_SIG` and is
/// deliberately ignored — FCS homes it on the per-accessor slot, leaving the
/// overall access `None`.
fn cst_member_sig_access(
    ms: &borzoi_cst::syntax::SyntaxNode,
    vs: &borzoi_cst::syntax::SyntaxNode,
) -> Option<NormalisedAccess> {
    for elem in ms.children_with_tokens() {
        match elem {
            rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::ACCESS_TOK => {
                return Some(cst_access_from_token(&t));
            }
            // The `VAL_SIG` holds the name (and any `with get/set` accessor
            // access after it); a leading modifier must precede it.
            rowan::NodeOrToken::Node(n) if n.kind() == SyntaxKind::VAL_SIG => break,
            _ => {}
        }
    }
    cst_access_before_name_ident(vs)
}

pub fn normalise_parse(parse: &Parse) -> NormalisedRoot {
    let root = &parse.root;
    match root.kind() {
        SyntaxKind::IMPL_FILE => {
            let file = ImplFile::cast(root.clone()).expect("kind already checked");
            NormalisedRoot::Impl(normalise_impl_file(&file))
        }
        SyntaxKind::SIG_FILE => {
            let file = SigFile::cast(root.clone()).expect("kind already checked");
            NormalisedRoot::Sig(normalise_sig_file(&file))
        }
        other => panic!("unexpected root kind {other:?} — expected IMPL_FILE or SIG_FILE"),
    }
}

fn normalise_impl_file(file: &ImplFile) -> NormalisedImplFile {
    NormalisedImplFile {
        warn_directives: normalise_warn_directives(file.syntax()),
        modules: file.modules().map(|m| normalise_module(&m)).collect(),
    }
}

/// Phase 10.11 — a signature file. Headers mirror the impl side (the parser
/// emits the same `MODULE_OR_NAMESPACE` node); bodies are empty until 10.12+.
fn normalise_sig_file(file: &SigFile) -> NormalisedSigFile {
    NormalisedSigFile {
        warn_directives: normalise_warn_directives(file.syntax()),
        modules: file.modules().map(|m| normalise_sig_module(&m)).collect(),
    }
}

fn normalise_warn_directives(
    root: &borzoi_cst::syntax::SyntaxNode,
) -> Vec<NormalisedWarnDirectiveKind> {
    root.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::WARN_DIRECTIVE)
        .filter_map(|t| match recognise_directive(t.text(), 0) {
            Some(Ok(r)) => match r.directive {
                Directive::NoWarn { .. } => Some(NormalisedWarnDirectiveKind::Nowarn),
                Directive::WarnOn { .. } => Some(NormalisedWarnDirectiveKind::Warnon),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn normalise_sig_module(m: &ModuleOrNamespace) -> NormalisedSigModule {
    // The `_argN` counter is local to the header (mirrors `normalise_module`).
    let mut local = 0u32;
    NormalisedSigModule {
        kind: module_kind(m),
        is_rec: m.is_rec(),
        attributes: normalise_attribute_lists(m.attributes(), &mut local),
        access: cst_access(m.syntax()),
        decls: m
            .sig_decls()
            .filter(|d| !is_light_sig_hash_directive(d))
            .map(|d| normalise_sig_decl(&d))
            .collect(),
    }
}

/// Project one `SynModuleSigDecl` (Block D). Phase 10.13a handles `open`; 10.13b
/// adds nested modules + abbreviations (reusing the impl-side projections);
/// 10.12a adds `val` (reusing the shared `VAL_SIG` carrier).
fn normalise_sig_decl(d: &SigDecl) -> NormalisedSigDecl {
    match d {
        SigDecl::Open(o) => NormalisedSigDecl::Open {
            target: normalise_open_target(o),
        },
        SigDecl::Val(v) => {
            // `SynModuleSigDecl.Val` (10.12a). The `VAL_SIG` carrier holds the
            // name and `: <type>` — the same node the abstract slot (9.10c) uses,
            // so the projection mirrors that arm. Leading `ATTRIBUTE_LIST`
            // children of the `VAL_DECL` are `SynValSig.attributes`.
            let mut counter = 0u32;
            let attributes = normalise_attribute_lists(v.attributes(), &mut counter);
            let vs = v.val_sig().expect("VAL_DECL must contain a VAL_SIG child");
            // An active-pattern-named value (`val (|Foo|_|) : …`) carries an
            // `ACTIVE_PAT_NAME` child, not a bare `IDENT_TOK`; FCS folds it to a
            // single `idText` (`"|Foo|_|"`). An operator-named value
            // (`val (+) : …`) keeps the bare operator under `IDENT_TOK`, which
            // `vs.ident()` returns directly (matching FCS's unwrapped
            // `OriginalNotationWithParen`).
            let name = if let Some(active) = vs.active_pat_name() {
                active_pat_id_text(&active)
            } else {
                strip_backticks(
                    vs.ident()
                        .expect("a `val` signature must have a name")
                        .text(),
                )
                .to_string()
            };
            // Explicit value typars (`val f<'T> : …`, phase 10.12) + their
            // inside-`<>` `when` constraints — the same `TYPAR_DECLS` projection a
            // type-definition header uses.
            let typars = vs
                .typar_decls()
                .map(|ds| ds.typars().map(|t| normalise_typar(&t)).collect())
                .unwrap_or_default();
            let constraints = vs
                .constraints()
                .map(|c| normalise_type_constraint(&c))
                .collect();
            let ty = normalise_type(
                &vs.ty()
                    .expect("a `val` signature must have a `: <type>` signature"),
            );
            // The `= <literal>` value (`val x : int = 1`, phase 10.12) — the
            // `VAL_SIG`'s expression child, projected via the shared expr
            // normaliser (reusing the `counter` for any `_argN` fun-lowering).
            let literal = vs
                .literal_value()
                .map(|e| Box::new(normalise_expr(&e, &mut counter)));
            NormalisedSigDecl::Val {
                attributes,
                name,
                access: cst_access(vs.syntax()),
                typars,
                constraints,
                ty,
                literal,
            }
        }
        SigDecl::NestedModule(nm) => {
            let long_id = nm
                .long_id()
                .map(|li| {
                    li.idents()
                        .map(|tok| strip_backticks(tok.text()).to_string())
                        .collect()
                })
                .unwrap_or_default();
            // Header attribute lists (phase 10.7d) — local `_argN` counter (a bare
            // attribute consumes none; the body's decls re-seed their own).
            let mut local = 0u32;
            let attributes = normalise_attribute_lists(nm.attributes(), &mut local);
            NormalisedSigDecl::NestedModule {
                long_id,
                is_rec: nm.is_rec(),
                attributes,
                access: cst_access(nm.syntax()),
                decls: nm
                    .sig_decls()
                    .filter(|d| !is_light_sig_hash_directive(d))
                    .map(|d| normalise_sig_decl(&d))
                    .collect(),
            }
        }
        SigDecl::ModuleAbbrev(a) => {
            let segs = |li: Option<LongIdent>| -> Vec<String> {
                li.map(|li| {
                    li.idents()
                        .map(|tok| strip_backticks(tok.text()).to_string())
                        .collect()
                })
                .unwrap_or_default()
            };
            NormalisedSigDecl::ModuleAbbrev {
                ident: segs(a.ident()).into_iter().next().unwrap_or_default(),
                long_id: segs(a.long_id()),
            }
        }
        SigDecl::Types(t) => {
            // `SynModuleSigDecl.Types` (10.14, first slice). Reuses the impl
            // `TYPE_DEFNS` node and `normalise_type_defn` — the abbreviation
            // repr lives in the shared `TYPE_ABBREV` child. (A `SynTypeDefnSig`
            // has no implicit-constructor slot, so the projection's
            // `implicit_ctor` is always `None` for an abbreviation.)
            NormalisedSigDecl::Types(t.defns().map(|d| normalise_type_defn(&d)).collect())
        }
        SigDecl::Exception(e) => {
            // `SynModuleSigDecl.Exception` (10.15). Reuses the impl `EXCEPTION_DEFN`
            // node and `normalise_exception_defn` — the `exconCore` repr (case +
            // optional `of` fields + `= path` abbrev) is shared, and the `with
            // member …` augmentation's `members` slot projects through the shared
            // `normalise_member` (its `MemberSig` arm handles the `MEMBER_SIG`
            // children).
            NormalisedSigDecl::Exception(normalise_exception_defn(e))
        }
        SigDecl::HashDirective(h) => {
            let (ident, args) = normalise_hash_directive_payload(h);
            NormalisedSigDecl::HashDirective { ident, args }
        }
    }
}

/// The `SynModuleOrNamespaceKind` projection — shared by the impl
/// ([`normalise_module`]) and sig ([`normalise_sig_module`]) headers, which use
/// the same `MODULE_OR_NAMESPACE` node.
fn module_kind(m: &ModuleOrNamespace) -> NormalisedModuleKind {
    // The header's dotted name, or empty for `namespace global` / a missing
    // path (`backticks` stripped to match FCS's `idText`).
    let long_id = || -> Vec<String> {
        m.long_id()
            .map(|li| {
                li.idents()
                    .map(|tok| strip_backticks(tok.text()).to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    match m.kind() {
        ModuleOrNamespaceKind::Anon => NormalisedModuleKind::Anon,
        ModuleOrNamespaceKind::NamedModule => NormalisedModuleKind::Named {
            long_id: long_id(),
            kind: NamedKind::Module,
        },
        ModuleOrNamespaceKind::DeclaredNamespace => NormalisedModuleKind::Named {
            long_id: long_id(),
            kind: NamedKind::Namespace,
        },
        ModuleOrNamespaceKind::GlobalNamespace => NormalisedModuleKind::Named {
            long_id: Vec::new(),
            kind: NamedKind::GlobalNamespace,
        },
    }
}

fn normalise_module(m: &ModuleOrNamespace) -> NormalisedModule {
    // Whole-file module-header attribute lists (phase 10.7e) — `MODULE_OR_NAMESPACE`
    // children before `MODULE_TOK`. The `_argN` counter is local (as the type /
    // nested-module header projections; the body re-seeds its own).
    let mut local = 0u32;
    let attributes = normalise_attribute_lists(m.attributes(), &mut local);
    NormalisedModule {
        kind: module_kind(m),
        is_rec: m.is_rec(),
        attributes,
        access: cst_access(m.syntax()),
        decls: m
            .decls()
            .filter(|d| !is_light_hash_directive(d))
            .map(|d| normalise_decl(&d))
            .collect(),
    }
}

/// Each module-level definition is one FCS `moduleDefn`, at whose head
/// the `SynArgNameGenerator` is `.Reset()` (`pars.fsy:1310`/`:1318`). So
/// the `_argN` counter for `fun`-lambda body-lowering ([`normalise_fun`])
/// starts fresh per decl and is *shared* across everything inside it —
/// including an `and`-chain (one `ModuleDecl::Let` with several bindings)
/// and any nested/sibling lambdas in a binding RHS.
fn normalise_decl(d: &ModuleDecl) -> NormalisedDecl {
    let mut counter = 0u32;
    match d {
        ModuleDecl::Expr(e) => {
            let expr = e
                .expr()
                .expect("EXPR_DECL must contain exactly one expression child");
            NormalisedDecl::Expr(normalise_expr(&expr, &mut counter))
        }
        ModuleDecl::Let(l) => {
            // The head binding's leading keyword folds in `rec`
            // (`LetRec`/`UseRec`); every `and`-chained binding is `And`. FCS
            // carries this per-binding in `SynBinding.Trivia.LeadingKeyword`
            // alongside the redundant `SynModuleDecl.Let.isRec` — we mirror
            // both (the `is_rec` field below stays).
            let head = match (l.is_use(), l.is_rec()) {
                (false, false) => NormalisedLeadingKeyword::Let,
                (false, true) => NormalisedLeadingKeyword::LetRec,
                (true, false) => NormalisedLeadingKeyword::Use,
                (true, true) => NormalisedLeadingKeyword::UseRec,
            };
            // Normalise the attribute lists *before* the bindings: the `[< … >]`
            // lists precede the binding keyword in source, and the shared
            // `_argN` lambda-lowering counter must be consumed in source order to
            // match FCS (which lowers an attribute-arg lambda — e.g.
            // `[<A(fun (x, y) -> x)>]` — before the binding body's). Our green
            // tree keeps the lists as leading `LET_DECL` children; FCS models
            // them on the *first* `SynBinding` of the group, so they are
            // projected there below.
            let attributes: Vec<Vec<NormalisedAttribute>> = l
                .attributes()
                .map(|list| {
                    list.attributes()
                        .map(|a| normalise_attribute(&a, &mut counter))
                        .collect()
                })
                .collect();
            let mut bindings: Vec<NormalisedBinding> = l
                .bindings()
                .enumerate()
                .map(|(i, b)| {
                    let lk = if i == 0 {
                        head
                    } else {
                        NormalisedLeadingKeyword::And
                    };
                    normalise_binding(&b, lk, &mut counter)
                })
                .collect();
            if let Some(first) = bindings.first_mut() {
                // The pre-`let` attributes precede any post-`let` run the binding
                // itself carries (`[<A>] let [<B>] x` → `[A, B]` in source order),
                // so prepend rather than overwrite.
                let mut combined = attributes;
                combined.append(&mut first.attributes);
                first.attributes = combined;
            }
            NormalisedDecl::Let {
                is_rec: l.is_rec(),
                bindings,
            }
        }
        ModuleDecl::Open(o) => NormalisedDecl::Open {
            target: normalise_open_target(o),
        },
        ModuleDecl::NestedModule(nm) => {
            // Each inner decl recurses through `normalise_decl`, which re-seeds
            // its own `_argN` counter — matching FCS's per-`moduleDefn`
            // `SynArgNameGenerator.Reset()` inside the nested body, so we do
            // *not* thread this decl's `counter` inward.
            let long_id = nm
                .long_id()
                .map(|li| {
                    li.idents()
                        .map(|tok| strip_backticks(tok.text()).to_string())
                        .collect()
                })
                .unwrap_or_default();
            // Header attribute lists (phase 10.7d) — leading `ATTRIBUTE_LIST`
            // children of the `NESTED_MODULE_DECL`. The `_argN` counter is local
            // (mirroring `normalise_type_defn`: a bare attribute consumes none,
            // and the body's own decls re-seed their counters).
            let mut local = 0u32;
            let attributes = normalise_attribute_lists(nm.attributes(), &mut local);
            NormalisedDecl::NestedModule {
                long_id,
                is_rec: nm.is_rec(),
                attributes,
                access: cst_access(nm.syntax()),
                decls: nm
                    .decls()
                    .filter(|d| !is_light_hash_directive(d))
                    .map(|d| normalise_decl(&d))
                    .collect(),
            }
        }
        ModuleDecl::ModuleAbbrev(a) => {
            let segs = |li: Option<LongIdent>| -> Vec<String> {
                li.map(|li| {
                    li.idents()
                        .map(|tok| strip_backticks(tok.text()).to_string())
                        .collect()
                })
                .unwrap_or_default()
            };
            NormalisedDecl::ModuleAbbrev {
                // The LHS is a single name; take its sole segment.
                ident: segs(a.ident()).into_iter().next().unwrap_or_default(),
                long_id: segs(a.long_id()),
            }
        }
        ModuleDecl::Types(t) => {
            NormalisedDecl::Types(t.defns().map(|d| normalise_type_defn(&d)).collect())
        }
        ModuleDecl::Exception(e) => NormalisedDecl::Exception(normalise_exception_defn(e)),
        ModuleDecl::Extern(e) => normalise_extern(e),
        ModuleDecl::Attributes(a) => {
            // `SynModuleDecl.Attributes` (10.7) — standalone `[<assembly: …>]`.
            let mut counter = 0u32;
            NormalisedDecl::Attributes(normalise_attribute_lists(a.attributes(), &mut counter))
        }
        ModuleDecl::HashDirective(h) => normalise_hash_directive(h),
    }
}

/// `true` for a `#light` / `#light "off"` directive. FCS's lexer consumes the
/// light-syntax directive and emits **no** `SynModuleDecl.HashDirective` for it
/// (unlike ordinary directives such as `#I` / `#load`, which do reach the
/// parser). Our lexer surfaces `#light` as `# light`, so the parser builds a
/// `HASH_DIRECTIVE_DECL` (keeping the tree lossless); it is dropped from the
/// projected decl list here so the result matches FCS.
fn is_light_hash_directive(d: &ModuleDecl) -> bool {
    let ModuleDecl::HashDirective(h) = d else {
        return false;
    };
    hash_directive_is_adjacent_light(h)
}

fn is_light_sig_hash_directive(d: &SigDecl) -> bool {
    let SigDecl::HashDirective(h) = d else {
        return false;
    };
    hash_directive_is_adjacent_light(h)
}

fn hash_directive_is_adjacent_light(h: &HashDirectiveDecl) -> bool {
    let node = h.syntax();
    let tok = |k| {
        node.children_with_tokens()
            .filter_map(|e| e.into_token())
            .find(move |t| t.kind() == k)
    };
    match (tok(SyntaxKind::HASH_TOK), tok(SyntaxKind::IDENT_TOK)) {
        // Only the *adjacent* `#light` spelling is the lexer-consumed light-syntax
        // directive; a spaced `# light` is an ordinary `#`-directive that FCS keeps
        // as a `HashDirective`, so require `#` and `light` to abut.
        (Some(hash), Some(ident)) => {
            // The raw text must be the bare `light` keyword (a quoted `` `light` ``
            // is an ordinary directive), adjacent to `#`.
            hash.text_range().end() == ident.text_range().start() && ident.text() == "light"
        }
        _ => false,
    }
}

/// Project a `#`-directive (FCS's `SynModuleDecl.HashDirective`). The directive
/// name is the first `IDENT_TOK`; each argument (in source order) is a string /
/// `int32` literal (a `CONST_EXPR`, decoded via [`normalise_const`]) or a source
/// identifier (a later `IDENT_TOK`, e.g. `__SOURCE_DIRECTORY__`).
fn normalise_hash_directive(h: &HashDirectiveDecl) -> NormalisedDecl {
    let (ident, args) = normalise_hash_directive_payload(h);
    NormalisedDecl::HashDirective { ident, args }
}

fn normalise_hash_directive_payload(
    h: &HashDirectiveDecl,
) -> (String, Vec<NormalisedHashDirectiveArg>) {
    let mut ident = String::new();
    let mut seen_name = false;
    let mut args = Vec::new();
    for elem in h.syntax().children_with_tokens() {
        match elem {
            rowan::NodeOrToken::Token(t) if t.kind() == SyntaxKind::IDENT_TOK => {
                if seen_name {
                    // The three magic source identifiers are `SourceIdentifier` —
                    // but *only* the bare keyword-string spelling. A backtick-quoted
                    // `` `__SOURCE_DIRECTORY__` `` is an ordinary `Ident` in FCS, so
                    // test the *raw* token text (before backtick stripping); every
                    // other identifier argument (`#nowarn FS`, `#time on`) is `Ident`.
                    let arg = if matches!(
                        t.text(),
                        "__SOURCE_DIRECTORY__" | "__SOURCE_FILE__" | "__LINE__"
                    ) {
                        NormalisedHashDirectiveArg::SourceIdentifier {
                            ident: t.text().to_string(),
                            value: source_identifier_value(&t),
                        }
                    } else {
                        NormalisedHashDirectiveArg::Ident(strip_backticks(t.text()).to_string())
                    };
                    args.push(arg);
                } else {
                    ident = strip_backticks(t.text()).to_string();
                    seen_name = true;
                }
            }
            rowan::NodeOrToken::Node(n) => {
                if let Some(c) = ConstExpr::cast(n) {
                    match normalise_const(&c) {
                        NormalisedConst::String { value, kind } => {
                            args.push(NormalisedHashDirectiveArg::String { value, kind })
                        }
                        NormalisedConst::Int32(v) => {
                            args.push(NormalisedHashDirectiveArg::Int32(v))
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    (ident, args)
}

/// Project an exception definition (phase 9.15a): the attribute lists (phase
/// 10.7m), the case data (reusing the union-case projector), the optional
/// `= path` abbreviation target, and any augmentation members (phase 9.15b;
/// empty in 9.15a).
fn normalise_exception_defn(e: &ExceptionDefnDecl) -> NormalisedExnDefn {
    // The exception's attribute lists — leading `[<A>] exception …` and
    // after-keyword `exception [<B>] …` `ATTRIBUTE_LIST` children of the
    // `EXCEPTION_DEFN`, in source order (FCS's `SynExceptionDefnRepr.attributes`,
    // `$1 @ cas`). The `_argN` counter is local, mirroring the type-header
    // projection (a bare attribute consumes none).
    let mut local = 0u32;
    let attributes = normalise_attribute_lists(e.attributes(), &mut local);
    let case = e
        .union_case()
        .map(|c| normalise_union_case(&c))
        .unwrap_or_else(|| NormalisedUnionCase {
            attributes: Vec::new(),
            ident: String::new(),
            kind: NormalisedUnionCaseKind::Fields(Vec::new()),
        });
    let abbrev = e.abbrev_path().map(|li| {
        li.idents()
            .map(|tok| strip_backticks(tok.text()).to_string())
            .collect()
    });
    let members = e.members().map(|m| normalise_member(&m)).collect();
    NormalisedExnDefn {
        attributes,
        access: cst_access(e.syntax()),
        case,
        abbrev,
        members,
    }
}

/// Project an `EXTERN_DECL` (FCS's `cPrototype`, `pars.fsy:3186`) to the
/// `SynModuleDecl.Let` FCS lowers it to: one `SynBinding` with
/// `SynLeadingKeyword.Extern`, a `LongIdent(name, Pats[Tuple[…]])` head pattern of
/// C-typed arguments, and a synthetic `(failwith "…" : cRetType)` RHS. Our green
/// tree keeps only the structural bits — the synthetic RHS is *not* in the source
/// text (the tree stays lossless), and a C type `int` is stored as a plain path —
/// so both the RHS and FCS's `App(SynType.LongIdent(path), [])` C-type wrapping are
/// reconstructed here to match FCS's natural projection.
fn normalise_extern(e: &ExternDecl) -> NormalisedDecl {
    // The leading `[<DllImport(…)>]` lists → the binding's attributes. A local
    // `_argN` counter (an attribute-arg lambda inside an extern is a niche edge
    // with no oracle), mirroring the other isolated attribute homes.
    let mut counter = 0u32;
    let attributes = normalise_attribute_lists(e.attributes(), &mut counter);

    let head: Vec<String> = e.name().map(long_ident_segments).unwrap_or_default();

    // Each `externArg` → `Attrib { Typed(Named|Wildcard, App(cType, [])), attrs }`.
    // FCS's `externArg` applies `addAttribs`, which wraps the `Typed` pattern in a
    // `SynPat.Attrib` *even with no attributes*. The whole argument list is FCS's
    // single `SynArgPats.Pats [SynPat.Tuple …]`, so it is the head pattern's sole
    // `Pats` entry — a `Tuple` (empty for `extern f()`).
    let elements: Vec<NormalisedPat> = e
        .args()
        .map(|a| {
            let mut c = 0u32;
            let attrs = normalise_attribute_lists(a.attributes(), &mut c);
            let ty = extern_ctype(a.c_type_base(), a.c_type_suffixes());
            let inner = match a.name() {
                Some(tok) => NormalisedPat::Named(strip_backticks(tok.text()).to_string()),
                None => NormalisedPat::Wildcard,
            };
            NormalisedPat::Attrib {
                pat: Box::new(NormalisedPat::Typed {
                    pat: Box::new(inner),
                    ty,
                }),
                attributes: attrs,
            }
        })
        .collect();

    let pat = NormalisedPat::LongIdent {
        head,
        typars: Vec::new(),
        args: NormalisedArgPats::Pats(vec![NormalisedPat::Tuple {
            is_struct: false,
            elements,
        }]),
    };

    // The return type is the annotation of the RHS's `Typed` wrapper: FCS's
    // `mkSynBinding` wraps the synthetic `failwith` in `Typed(rhs, cRetType)` when
    // `returnInfo` is present. `void` projects as the surface `void` path (FCS
    // keeps the `unit` ident's `OriginalNotation "void"`, which the normaliser reads
    // as the segment text).
    let ret_ty = e
        .return_info()
        .map(|r| extern_ctype(r.c_type_base(), r.c_type_suffixes()))
        .unwrap_or_else(|| app_type(Vec::new()));

    let expr = NormalisedExpr::Typed {
        expr: Box::new(NormalisedExpr::App {
            is_atomic: false,
            is_infix: false,
            func: Box::new(NormalisedExpr::Ident("failwith".to_string())),
            arg: Box::new(NormalisedExpr::Const(NormalisedConst::String {
                value: "extern was not given a DllImport attribute"
                    .encode_utf16()
                    .collect(),
                kind: SynStringKind::Regular,
            })),
        }),
        ty: ret_ty,
    };

    NormalisedDecl::Let {
        is_rec: false,
        bindings: vec![NormalisedBinding {
            leading_keyword: NormalisedLeadingKeyword::Extern,
            is_mutable: false,
            is_inline: false,
            attributes,
            // `extern [access] ret Name(…)` — the modifier is an `ACCESS_TOK`
            // child of the `EXTERN_DECL`; FCS homes it on the lowered binding's
            // `LongIdent` head pattern.
            access: cst_access(e.syntax()),
            pat,
            expr,
        }],
    }
}

/// The `LongIdent` path segments of `li` (backticks stripped) — the shared
/// projection used across the decl normalisers.
fn long_ident_segments(li: LongIdent) -> Vec<String> {
    li.idents()
        .map(|tok| strip_backticks(tok.text()).to_string())
        .collect()
}

/// An extern C type (`cType`) → FCS's `SynType.App` representation. Even a plain
/// path is *App-wrapped* with no type args; the C-only suffix forms become the
/// synthetic type applications FCS reports (`T&` → postfix `&<T>`, `T*` →
/// postfix `*<T>`, `T[]` → postfix `[]<T>`, `void*` → postfix `void*`). FCS's
/// suffix grammar is recursive, so fold suffixes in source order.
fn extern_ctype(
    base: Option<ExternCTypeBase>,
    suffixes: impl IntoIterator<Item = ExternCTypeSuffix>,
) -> NormalisedType {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Suffix {
        Byref,
        Pointer,
        Array,
    }

    let suffixes: Vec<_> = suffixes
        .into_iter()
        .map(|suffix| match suffix {
            ExternCTypeSuffix::Byref(_) => Suffix::Byref,
            ExternCTypeSuffix::Pointer(_) => Suffix::Pointer,
            ExternCTypeSuffix::Array(_) => Suffix::Array,
        })
        .collect();

    let (mut ty, suffix_start) = if matches!(&base, Some(ExternCTypeBase::Void(_)))
        && suffixes.first() == Some(&Suffix::Pointer)
    {
        (
            NormalisedType::App {
                type_name: Box::new(NormalisedType::LongIdent(vec!["void*".to_string()])),
                type_args: Vec::new(),
                is_postfix: true,
            },
            1,
        )
    } else if matches!(&base, Some(ExternCTypeBase::Void(_))) {
        (app_type(vec!["void".to_string()]), 0)
    } else {
        (
            app_type(match base {
                Some(ExternCTypeBase::Path(path)) => long_ident_segments(path),
                Some(ExternCTypeBase::Void(_)) | None => Vec::new(),
            }),
            0,
        )
    };

    for suffix in suffixes.into_iter().skip(suffix_start) {
        let name = match suffix {
            Suffix::Byref => "&",
            Suffix::Pointer => "*",
            Suffix::Array => "[]",
        };
        ty = NormalisedType::App {
            type_name: Box::new(NormalisedType::LongIdent(vec![name.to_string()])),
            type_args: vec![ty],
            is_postfix: true,
        };
    }

    ty
}

/// `SynType.App(SynType.LongIdent(segments), typeArgs = [], isPostfix = false)`.
fn app_type(segments: Vec<String>) -> NormalisedType {
    NormalisedType::App {
        type_name: Box::new(NormalisedType::LongIdent(segments)),
        type_args: Vec::new(),
        is_postfix: false,
    }
}

/// Project one `SynTypeDefn`: the name (`SynComponentInfo.longId`), the type
/// parameters (`typeParams`, phase 9.3), and the repr. Members / the implicit
/// constructor arrive with the object-model slices.
fn normalise_type_defn(d: &TypeDefn) -> NormalisedTypeDefn {
    // The header attribute lists (phase 10.7a) — leading `ATTRIBUTE_LIST`
    // children of the first `TYPE_DEFN`. The `_argN` counter is local (a bare
    // attribute consumes none; an arg-lambda inside a type-header attribute is
    // a niche edge with no oracle), mirroring the other isolated attribute homes.
    let mut local = 0u32;
    let attributes = normalise_attribute_lists(d.attributes(), &mut local);
    let long_id = d
        .long_id()
        .map(|li| {
            li.idents()
                .map(|tok| strip_backticks(tok.text()).to_string())
                .collect()
        })
        .unwrap_or_default();
    let typars = d
        .typar_decls()
        .map(|ds| ds.typars().map(|t| normalise_typar(&t)).collect())
        .unwrap_or_default();
    let constraints = d
        .constraints()
        .map(|c| normalise_type_constraint(&c))
        .collect();
    let ctor = d.implicit_ctor().map(|c| normalise_implicit_ctor(&c));
    // The repr — `None` for a **bodyless** type (`SynTypeDefnSimpleRepr.None`,
    // no `=`): `[<Measure>] type m`, `type Foo`, the `recover`-path `type C(x)`.
    // Otherwise the repr node (abbrev / record / union / enum / object model).
    let mut repr = match d.repr() {
        Some(r) => normalise_type_repr(&r),
        None => NormalisedTypeRepr::None,
    };
    // The *outer* member list (phase 9.13) — an augmentation's members, or
    // trailing members on a simple repr. These are the direct `MEMBER_DEFN`
    // children of the `TYPE_DEFN`; a pure object model nests its members inside
    // the repr node, so `members()` is empty there.
    let mut members: Vec<_> = d.members().map(|m| normalise_member(&m)).collect();
    // The implicit constructor's home depends on the repr (verified against
    // `fcs-dump`). Our green tree carries it once, on the `TYPE_DEFN`; mirror
    // FCS's placement here:
    //  * object model (`type C(x) = member …`): duplicated — both the
    //    `implicitConstructor` slot *and* prepended to the repr's member list.
    //  * `None` repr (`type C(x)` with no `=`, FCS's `recover` alternative): the
    //    ctor lands *only* in the outer members slot; `implicitConstructor` is
    //    `None`.
    //  * other simple reprs (abbrev / record / union / enum): the
    //    `implicitConstructor` slot alone.
    let implicit_ctor = match (ctor, &mut repr) {
        (Some(ic), NormalisedTypeRepr::ObjectModel { members, .. }) => {
            members.insert(0, ic.clone());
            Some(ic)
        }
        (Some(ic), NormalisedTypeRepr::None) => {
            members.insert(0, ic);
            None
        }
        (ic, _) => ic,
    };
    NormalisedTypeDefn {
        attributes,
        access: cst_type_header_access(d.syntax()),
        long_id,
        typars,
        constraints,
        repr,
        members,
        implicit_ctor,
    }
}

/// Project an `IMPLICIT_CTOR` (phase 9.8a) to a
/// [`NormalisedMember::ImplicitCtor`]: the constructor argument pattern (a
/// `SynPat`, via the shared [`normalise_pat`]) and the optional `as <self>`
/// identifier.
fn normalise_implicit_ctor(c: &ImplicitCtor) -> NormalisedMember {
    // The ctor's attribute lists (phase 10.7j) — leading `IMPLICIT_CTOR` children,
    // homed by FCS in `ImplicitCtor.attributes` (field 1). Local `_argN` counter,
    // as the other member homes (a bare attribute consumes none).
    let mut counter = 0u32;
    let attributes = normalise_attribute_lists(c.attributes(), &mut counter);
    let args = normalise_pat(
        &c.args()
            .expect("IMPLICIT_CTOR must contain a constructor argument pattern"),
        &mut counter,
    );
    let self_id = c.self_id().map(|t| strip_backticks(t.text()).to_string());
    NormalisedMember::ImplicitCtor {
        args,
        self_id,
        attributes,
        access: cst_access(c.syntax()),
    }
}

/// Project one `SynTypeConstraint` (phase 9.3b) from a [`TyparConstraint`]: its
/// subject typar plus, for the subtype form, the constraint type. The kind is
/// read from the constraint's operator/keyword tokens via
/// [`TyparConstraint::kind`].
fn normalise_type_constraint(c: &TyparConstraint) -> NormalisedTypeConstraint {
    // A bare self-constraint `when IFoo<'T>` (F# 7 IWSAM shorthand) carries a
    // `SELF_CONSTRAINT`-wrapped type and *no* subject typar — check it before
    // the typar-bearing forms, whose `typar()`/`kind()` `expect`s would fire.
    if let Some(ty) = c.self_constraint() {
        return NormalisedTypeConstraint::SelfConstrained(normalise_type(&ty));
    }
    // An SRTP member constraint `^T : (static member M : sig)` carries a
    // `MEMBER_SIG` child (no operator/keyword `kind`), projected through the
    // shared member-sig normaliser. Its support is either the single subject
    // typar `^T` (a `TYPAR_DECL` — FCS's `SynType.Var`) or, for the
    // parenthesised alternatives `(^a or ^b) : (…)` / `(Witnesses or ^T) : (…)`,
    // the `or`-list of operand *types* (direct `Type` children, FCS's
    // `Paren(Or(…))`). The two are disjoint: a paren-alts constraint has no
    // subject `TYPAR_DECL`.
    if let Some(ms) = c.member_sig() {
        let support = match c.typar() {
            Some(subject) => vec![typar_decl_to_var_type(&subject)],
            None => c.support_types().map(|t| normalise_type(&t)).collect(),
        };
        return NormalisedTypeConstraint::SupportsMember {
            support,
            member: Box::new(normalise_member_sig(&ms)),
        };
    }
    let typar = normalise_typar(
        &c.typar()
            .expect("TYPAR_CONSTRAINT must have a subject TYPAR_DECL"),
    );
    let kind = c
        .kind()
        .expect("a parsed TYPAR_CONSTRAINT must have a supported kind");
    match kind {
        TyparConstraintKind::SubtypeOf => NormalisedTypeConstraint::SubtypeOf {
            typar,
            ty: normalise_type(
                &c.ty()
                    .expect("a `:>` constraint must carry a constraint type"),
            ),
        },
        TyparConstraintKind::ValueType => NormalisedTypeConstraint::IsValueType(typar),
        TyparConstraintKind::ReferenceType => NormalisedTypeConstraint::IsReferenceType(typar),
        TyparConstraintKind::SupportsNull => NormalisedTypeConstraint::SupportsNull(typar),
        TyparConstraintKind::NotSupportsNull => NormalisedTypeConstraint::NotSupportsNull(typar),
        TyparConstraintKind::Comparable => NormalisedTypeConstraint::IsComparable(typar),
        TyparConstraintKind::Equatable => NormalisedTypeConstraint::IsEquatable(typar),
        TyparConstraintKind::Unmanaged => NormalisedTypeConstraint::IsUnmanaged(typar),
        TyparConstraintKind::Enum => NormalisedTypeConstraint::IsEnum {
            typar,
            args: c.type_args().map(|t| normalise_type(&t)).collect(),
        },
        TyparConstraintKind::Delegate => NormalisedTypeConstraint::IsDelegate {
            typar,
            args: c.type_args().map(|t| normalise_type(&t)).collect(),
        },
    }
}

/// Project one `SynTyparDecl` to its typar (phase 9.3): the name, the head-type
/// flag (`^a` vs `'a`), and the leading attribute run (`[<Measure>] 'a`). Mirrors
/// [`normalise_var_type`]. The `_argN` lambda-lowering counter is local to this
/// typar (each `SynTyparDecl` is its own attribute carrier; an arg-lambda inside
/// a typar attribute is a niche edge with no oracle), mirroring the type-header
/// and other isolated attribute projections.
fn normalise_typar(t: &TyparDecl) -> NormalisedTypar {
    let name = t
        .ident()
        .expect("TYPAR_DECL must contain an IDENT_TOK child");
    let mut local = 0u32;
    NormalisedTypar {
        name: strip_backticks(name.text()).to_string(),
        head_type: t.is_head_type(),
        attributes: normalise_attribute_lists(t.attributes(), &mut local),
        // `SynTyparDecl.intersectionConstraints` — the `& #seq<int>` run, each a
        // flexible `Type` child of the `TYPAR_DECL`.
        intersection_constraints: t
            .intersection_constraints()
            .map(|ty| normalise_type(&ty))
            .collect(),
    }
}

/// Reduce a [`VarType`] (`SynType.Var`) to its [`NormalisedTypar`] — the same
/// (name, head-type) pair as [`normalise_typar`], for the trait-call support
/// where the typars are `VAR_TYPE`s rather than `TYPAR_DECL`s.
fn var_type_to_typar(v: &VarType) -> NormalisedTypar {
    let name = v.ident().expect("VAR_TYPE must contain an IDENT_TOK child");
    NormalisedTypar {
        name: strip_backticks(name.text()).to_string(),
        head_type: v.is_head_type(),
        // A `SynType.Var` has no `SynTyparDecl` wrapper, hence no attributes and
        // no intersection constraints.
        attributes: Vec::new(),
        intersection_constraints: Vec::new(),
    }
}

/// Reduce a single-typar SRTP constraint subject [`TyparDecl`] (`^T`) to the
/// [`NormalisedType::Var`] FCS records as `WhereTyparSupportsMember`'s support
/// (field 0 is `SynType.Var`, not a bare `SynTypar`). The parenthesised
/// alternatives form keeps its operands as `Type` nodes already; this bridges
/// the single-typar form, whose subject shares the `TYPAR_DECL` shape of every
/// other constraint.
fn typar_decl_to_var_type(t: &TyparDecl) -> NormalisedType {
    let name = t
        .ident()
        .expect("SRTP constraint subject TYPAR_DECL must contain an IDENT_TOK child");
    NormalisedType::Var {
        name: strip_backticks(name.text()).to_string(),
        head_type: t.is_head_type(),
    }
}

/// Project a `SynTypeDefnRepr`: the `TypeAbbrev` form (phase 9.1, whose
/// `rhsType` reuses the phase-7 [`normalise_type`] projector) or the `Record`
/// form (phase 9.4, a list of [`normalise_field`]).
fn normalise_type_repr(r: &TypeDefnRepr) -> NormalisedTypeRepr {
    match r {
        TypeDefnRepr::Abbrev(a) => NormalisedTypeRepr::Abbrev(normalise_type(
            &a.ty()
                .expect("TYPE_ABBREV must wrap a type (its `rhsType`)"),
        )),
        TypeDefnRepr::Record(rec) => NormalisedTypeRepr::Record {
            access: cst_access(rec.syntax()),
            fields: rec.fields().map(|f| normalise_field(&f)).collect(),
        },
        TypeDefnRepr::Union(u) => NormalisedTypeRepr::Union {
            access: cst_access(u.syntax()),
            cases: u.cases().map(|c| normalise_union_case(&c)).collect(),
        },
        TypeDefnRepr::Enum(e) => {
            NormalisedTypeRepr::Enum(e.cases().map(|c| normalise_enum_case(&c)).collect())
        }
        TypeDefnRepr::Delegate(d) => NormalisedTypeRepr::Delegate(normalise_type(
            &d.ty()
                .expect("DELEGATE_REPR must wrap a signature type (its `of <type>`)"),
        )),
        // Inline-IL type repr `( # "instr" # )`
        // (`SynTypeDefnSimpleRepr.LibraryOnlyILAssembly`) is not modelled in the
        // diff oracle, for the same reason as the expression form
        // (`Expr::InlineIl`): FCS boxes the parsed IL (`ilCode: obj`), which the
        // dump cannot round-trip for an equality check. Both sides `panic!`, so
        // the corpus sweep's `catch_unwind` counts the file as unmodeled and
        // neither side reaches the equality assertion.
        TypeDefnRepr::InlineIl(_) => {
            panic!("inline IL (SynTypeDefnSimpleRepr.LibraryOnlyILAssembly) is not modelled")
        }
        TypeDefnRepr::ObjectModel(om) => NormalisedTypeRepr::ObjectModel {
            // An augmentation repr (`type T with member …`, phase 9.13a) is
            // marked by the `with` (its members live in the outer slot, so the
            // repr's own member list is empty). A bare `type T = member …` (phase
            // 9.7) is `Unspecified`; the explicit `class`/`struct`/`interface … end`
            // markers (phase 9.12) carry the corresponding kind token.
            kind: if om.is_augmentation() {
                NormalisedTypeDefnKind::Augmentation
            } else if om.is_class() {
                NormalisedTypeDefnKind::Class
            } else if om.is_struct() {
                NormalisedTypeDefnKind::Struct
            } else if om.is_interface() {
                NormalisedTypeDefnKind::Interface
            } else {
                NormalisedTypeDefnKind::Unspecified
            },
            members: om.members().map(|m| normalise_member(&m)).collect(),
        },
    }
}

/// Project one `SynMemberDefn` (phase 9.7): the [`Member`](MemberDefn::Member)
/// form reuses [`normalise_binding`] with a `Member` leading keyword. Each
/// member gets a fresh `_argN` counter (a member has no sibling `fun`-lambda to
/// share one with; the counter only matters for the deferred non-simple
/// `fun`-arg lowering, so this is a forward guard rather than an active check).
fn normalise_member(m: &MemberDefn) -> NormalisedMember {
    match m {
        MemberDefn::Member(mm) => {
            let binding = mm
                .binding()
                .expect("MEMBER_DEFN must contain a BINDING child");
            // The leading keyword carries the member's flavour (the `IsInstance`/
            // `IsOverrideOrExplicitImpl`/`MemberKind` `SynMemberFlags` are elided):
            // `member` (9.7), `static member` (9.9a), `override`/`default` (9.10a).
            let leading = match mm.leading_keyword() {
                MemberLeading::Member => NormalisedLeadingKeyword::Member,
                MemberLeading::StaticMember => NormalisedLeadingKeyword::StaticMember,
                MemberLeading::Override => NormalisedLeadingKeyword::Override,
                MemberLeading::Default => NormalisedLeadingKeyword::Default,
                MemberLeading::New => NormalisedLeadingKeyword::New,
            };
            let mut counter = 0u32;
            // The member's attribute lists (phase 10.7f) — leading `MEMBER_DEFN`
            // children, homed by FCS in `SynBinding.attributes`. Normalise them
            // *before* the binding (shared `_argN` counter, source order, as the
            // let case), then set them on the projected binding.
            let attributes = normalise_attribute_lists(mm.attributes(), &mut counter);
            let mut nb = normalise_binding(&binding, leading, &mut counter);
            nb.attributes = attributes;
            NormalisedMember::Member(nb)
        }
        MemberDefn::LetBindings(lb) => {
            // Class-local `let`/`let rec` (9.8b) — mirror the module-level
            // `Let` projection: the head binding folds in `rec`
            // (`LetRec`/`UseRec`), every `and`-chained binding is `And`. A fresh
            // `_argN` counter per member (no shared sibling lambda).
            //
            // A `static let`/`static let rec` (9.8c) rewrites the *head*
            // binding's leading keyword `Let`/`LetRec` → `StaticLet`/
            // `StaticLetRec` (FCS's `mkClassMemberLocalBindings`), leaving the
            // `and`-chained continuations as `And`. The static rewrite only
            // touches the `let` forms — `static use` keeps `Use`/`UseRec` (FCS
            // flags it an error but does not restamp the keyword).
            let is_rec = lb.is_rec();
            let head = match (lb.is_static(), lb.is_use(), is_rec) {
                (true, false, false) => NormalisedLeadingKeyword::StaticLet,
                (true, false, true) => NormalisedLeadingKeyword::StaticLetRec,
                (_, false, false) => NormalisedLeadingKeyword::Let,
                (_, false, true) => NormalisedLeadingKeyword::LetRec,
                (_, true, false) => NormalisedLeadingKeyword::Use,
                (_, true, true) => NormalisedLeadingKeyword::UseRec,
            };
            let mut counter = 0u32;
            // The binding group's attribute lists (phase 10.7l) — leading
            // `MEMBER_LET_BINDINGS` children, homed by FCS on the *first*
            // `SynBinding.attributes`. Normalise them *before* the bindings
            // (shared `_argN` counter, source order — as the module-level `Let`
            // and the `member` carrier), then set them on the head binding.
            let attributes = normalise_attribute_lists(lb.attributes(), &mut counter);
            let mut bindings: Vec<NormalisedBinding> = lb
                .bindings()
                .enumerate()
                .map(|(i, b)| {
                    let lk = if i == 0 {
                        head
                    } else {
                        NormalisedLeadingKeyword::And
                    };
                    normalise_binding(&b, lk, &mut counter)
                })
                .collect();
            if let Some(first) = bindings.first_mut() {
                // Prepend the group's pre-`let` attributes to any post-`let` run
                // the first binding carries (`[<A>] let [<B>] x` → `[A, B]`),
                // matching the module-level `Let` arm.
                let mut combined = attributes;
                combined.append(&mut first.attributes);
                first.attributes = combined;
            }
            NormalisedMember::LetBindings { is_rec, bindings }
        }
        MemberDefn::Do(d) => {
            // A class-body `[static] do <expr>` (9.8d). FCS models it as a
            // single-binding `LetBindings` whose binding has kind `Do`: a
            // synthetic `SynPat.Const(Unit)` head, the `do` body in
            // `SynBinding.expr`, and a `Do` / `StaticDo` leading keyword
            // (`isStatic`/`isRecursive` themselves elided — the static-ness rides
            // on the keyword, exactly as `static let` rides on `StaticLet`). We
            // mirror that: a `LetBindings { is_rec: false }` with one such binding.
            let leading = if d.is_static() {
                NormalisedLeadingKeyword::StaticDo
            } else {
                NormalisedLeadingKeyword::Do
            };
            let mut counter = 0u32;
            let expr = normalise_expr(
                &d.expr()
                    .expect("MEMBER_DO must contain a do-body expression"),
                &mut counter,
            );
            let binding = NormalisedBinding {
                leading_keyword: leading,
                is_mutable: false,
                is_inline: false,
                attributes: Vec::new(),
                // A `do` binding takes no access modifier.
                access: None,
                // FCS's synthetic head pattern for a `do` binding.
                pat: NormalisedPat::Const(NormalisedConst::Unit),
                expr,
            };
            NormalisedMember::LetBindings {
                is_rec: false,
                bindings: vec![binding],
            }
        }
        MemberDefn::ValField(vf) => NormalisedMember::ValField(normalise_val_field(vf)),
        MemberDefn::Inherit(inh) => {
            // `SynMemberDefn.Inherit` / `ImplicitInherit` (9.11a). The presence
            // of a constructor-args expression discriminates the two: with args
            // (`inherit Base()`) → `ImplicitInherit`; without (`inherit Base`) →
            // `Inherit`. The `as base` alias is elided on both sides.
            let base_type = inh.base_type().map(|t| normalise_type(&t));
            match inh.args() {
                Some(args) => {
                    let mut counter = 0u32;
                    NormalisedMember::ImplicitInherit {
                        base_type: base_type
                            .expect("an `inherit Base(args)` clause has a base type"),
                        args: normalise_expr(&args, &mut counter),
                    }
                }
                None => NormalisedMember::Inherit { base_type },
            }
        }
        MemberDefn::Interface(i) => {
            // `SynMemberDefn.Interface` (9.11b). The `with` clause's presence is
            // FCS's `members: SynMemberDefns option`: a `with` block →
            // `Some(members)` (possibly empty), no `with` → `None`. The
            // interface's own members nest inside the `INTERFACE_IMPL` node.
            let interface_type = normalise_type(
                &i.interface_type()
                    .expect("an interface implementation has an interface type"),
            );
            let members = i
                .has_with()
                .then(|| i.members().map(|m| normalise_member(&m)).collect());
            NormalisedMember::Interface {
                interface_type,
                members,
            }
        }
        MemberDefn::GetSetMember(gsm) => {
            // `SynMemberDefn.GetSetMember` (9.14). The property path is shared by
            // both accessors (FCS duplicates it into each binding's headPat); we
            // project it once from the member head. Each present accessor's args
            // and body reuse the shared pat/expr projectors.
            // Read the whole head pattern (not just `name()`'s `LONG_IDENT`) so a
            // dotted operator / active-pattern property name (`member x.(+) with
            // …`, `member x.(|Foo|_|) with …`) keeps every segment — the
            // active-pattern segment lives on a sibling `ACTIVE_PAT_NAME`.
            let name = gsm
                .head_pat()
                .map(|p| long_ident_pat_head_segments(&p))
                .unwrap_or_default();
            // A member-level modifier (`member private this.P with …`) is a
            // direct `ACCESS_TOK` child of the `GET_SET_MEMBER`; FCS folds it
            // onto *every* present accessor binding's head pattern. A
            // per-accessor `and private set` modifier lives inside the
            // `GET_SET_ACCESSOR` and takes precedence.
            let member_access = cst_access(gsm.syntax());
            let accessor = |a: GetSetAccessor, is_setter: bool| {
                let mut counter = 0u32;
                // The accessor binding's `SynBinding.attributes` (phase 10.7f) is the
                // property's leading `[<…>]` (duplicated by FCS onto *both* accessors)
                // followed by the accessor's *own* `with [<A>] get …` lists — in that
                // source order. Lowered afresh per accessor (its own binding's arg
                // generator), before the body.
                let mut attributes = normalise_attribute_lists(gsm.attributes(), &mut counter);
                attributes.extend(normalise_attribute_lists(a.attributes(), &mut counter));
                let mut args: Vec<NormalisedPat> =
                    a.args().map(|p| normalise_pat(&p, &mut counter)).collect();
                // An *indexer setter* `set <index> <value>` is bundled by FCS into
                // a single `SynPat.Tuple` arg (the paren-tuple index flattened, the
                // value appended), where our parser leaves the two space-separated
                // params as separate curried pats. Reconstruct that here; a plain
                // `set v` (one param) and the getter are unchanged.
                if is_setter && args.len() == 2 {
                    let value = args.pop().expect("setter value param");
                    let index = args.pop().expect("setter index param");
                    let mut elements = flatten_setter_index(index);
                    elements.push(value);
                    args = vec![NormalisedPat::Tuple {
                        is_struct: false,
                        elements,
                    }];
                }
                // A `: T` return type on the accessor (`get() : int = …`) is the
                // accessor binding's `returnInfo`; FCS also wraps the body in
                // `SynExpr.Typed(body, T)` (read straight from `bf[9]` on the FCS
                // side), so reconstruct that wrapper here — per accessor, since
                // each carries its own return info independently.
                let body = normalise_expr(
                    &a.body().expect("a get/set accessor has a body"),
                    &mut counter,
                );
                let body = match a.return_type() {
                    Some(ty) => NormalisedExpr::Typed {
                        expr: Box::new(body),
                        ty: normalise_type(&ty),
                    },
                    None => body,
                };
                NormalisedAccessor {
                    attributes,
                    access: cst_access(a.syntax()).or(member_access),
                    args,
                    body,
                }
            };
            NormalisedMember::GetSetMember {
                name,
                get: gsm.getter().map(|a| accessor(a, false)),
                set: gsm.setter().map(|a| accessor(a, true)),
            }
        }
        MemberDefn::AutoProperty(ap) => {
            // `SynMemberDefn.AutoProperty` (9.9c). `prop_kind` is read off the
            // `with get[, set]` clause; `ty` is the optional `: T` annotation;
            // `expr` is the `= <expr>` initialiser (a fresh `_argN` counter, no
            // shared sibling lambda).
            let prop_kind = match ap.prop_kind() {
                AutoPropertyKind::Member => NormalisedPropKind::Member,
                AutoPropertyKind::PropertyGet => NormalisedPropKind::PropertyGet,
                AutoPropertyKind::PropertySet => NormalisedPropKind::PropertySet,
                AutoPropertyKind::PropertyGetSet => NormalisedPropKind::PropertyGetSet,
            };
            let mut counter = 0u32;
            // The auto-property's attribute lists (phase 10.7h) — leading
            // `AUTO_PROPERTY` children, homed by FCS in
            // `SynMemberDefn.AutoProperty.attributes` (field 0). Normalise them
            // *before* the initialiser (shared `_argN` counter, source order, as
            // the member home).
            let attributes = normalise_attribute_lists(ap.attributes(), &mut counter);
            let expr = normalise_expr(
                &ap.expr()
                    .expect("AUTO_PROPERTY must contain an initialiser expression"),
                &mut counter,
            );
            NormalisedMember::AutoProperty {
                name: strip_backticks(
                    ap.ident()
                        .expect("AUTO_PROPERTY must have a property name")
                        .text(),
                )
                .to_string(),
                is_static: ap.is_static(),
                ty: ap.ty().map(|t| normalise_type(&t)),
                prop_kind,
                expr,
                attributes,
                // The *overall* access (`member val private X`), before the
                // name — not a trailing `with private get` (which FCS homes on
                // the per-accessor slot of the `SynValSigAccess`, leaving the
                // overall access `None`).
                access: cst_access_before_name_ident(ap.syntax()),
            }
        }
        MemberDefn::AbstractSlot(a) => {
            // `SynMemberDefn.AbstractSlot` (9.10c). The `VAL_SIG` carries the name
            // and `: <type>`; the optional `member` keyword distinguishes
            // `AbstractMember` from a bare `Abstract`.
            let vs = a
                .val_sig()
                .expect("ABSTRACT_SLOT must contain a VAL_SIG child");
            // An active-pattern-named slot (`abstract (|Foo|_|) : …`) carries an
            // `ACTIVE_PAT_NAME` child, folded to FCS's single `idText`
            // (`"|Foo|_|"`); every other name (ident or operator, the latter kept
            // as the bare operator under `IDENT_TOK`) reads from the `VAL_SIG`'s
            // ident — same rule as the member-sig name above.
            let name = if let Some(active) = vs.active_pat_name() {
                active_pat_id_text(&active)
            } else {
                strip_backticks(
                    vs.ident()
                        .expect("an abstract slot must have a name")
                        .text(),
                )
                .to_string()
            };
            let ty = normalise_type(
                &vs.ty()
                    .expect("an abstract slot must have a `: <type>` signature"),
            );
            let leading_keyword = match (a.is_static(), a.is_abstract_member()) {
                (true, true) => NormalisedLeadingKeyword::StaticAbstractMember,
                (true, false) => NormalisedLeadingKeyword::StaticAbstract,
                (false, true) => NormalisedLeadingKeyword::AbstractMember,
                (false, false) => NormalisedLeadingKeyword::Abstract,
            };
            // The slot's attribute lists (phase 10.7g) — leading `ABSTRACT_SLOT`
            // children, homed by FCS in `SynValSig.attributes`. Local `_argN`
            // counter (a bare attribute consumes none), as the other member homes.
            let mut counter = 0u32;
            let attributes = normalise_attribute_lists(a.attributes(), &mut counter);
            NormalisedMember::AbstractSlot {
                name,
                ty,
                leading_keyword,
                attributes,
                // An impl-side abstract slot is bodyless (FCS rejects
                // `abstract M : int = 1`); only sig member sigs carry a literal.
                literal: None,
                // An access modifier on an *impl* abstract slot is illegal;
                // FCS always discards it (`SynValSig.accessibility` = `None`
                // even for `abstract member internal M`), so mirror that rather
                // than projecting the recovery `ACCESS_TOK` our tree captures.
                access: None,
            }
        }
        MemberDefn::MemberSig(ms) => normalise_member_sig(ms),
    }
}

/// Project a `SynMemberSig.Member` (phase 10.14, slice 3a) — a signature member
/// sig — to the shared [`NormalisedMember::AbstractSlot`] shape. The `VAL_SIG`
/// carrier holds the name and `: <type>` (like an abstract slot / `val` sig);
/// the leading keyword tokens (`member`/`abstract`/`static member`) select the
/// kind. FCS elides the `SynMemberSig`-vs-`SynMemberDefn` distinction at the
/// normalised level, keying on the leading keyword. Shared by the member-block
/// projection above and the SRTP member-constraint
/// (`^T : (static member M : sig)`) — both carry the identical `MEMBER_SIG`.
fn normalise_member_sig(ms: &MemberSig) -> NormalisedMember {
    let vs = ms
        .val_sig()
        .expect("MEMBER_SIG must contain a VAL_SIG child");
    let leading = ms.leading_keyword();
    let leading_keyword = match leading {
        MemberSigLeading::Member => NormalisedLeadingKeyword::Member,
        MemberSigLeading::Static => NormalisedLeadingKeyword::Static,
        MemberSigLeading::StaticMember => NormalisedLeadingKeyword::StaticMember,
        MemberSigLeading::Abstract => NormalisedLeadingKeyword::Abstract,
        MemberSigLeading::AbstractMember => NormalisedLeadingKeyword::AbstractMember,
        MemberSigLeading::StaticAbstract => NormalisedLeadingKeyword::StaticAbstract,
        MemberSigLeading::StaticAbstractMember => NormalisedLeadingKeyword::StaticAbstractMember,
        MemberSigLeading::Override => NormalisedLeadingKeyword::Override,
        MemberSigLeading::Default => NormalisedLeadingKeyword::Default,
        MemberSigLeading::New => NormalisedLeadingKeyword::New,
    };
    // A `new`-ctor sig (slice 3e) has no `IDENT` — the `new` keyword is
    // both the leading marker and the name; FCS names it "new". An
    // active-pattern-named member sig (`member (|Foo|_|) : …`) carries an
    // `ACTIVE_PAT_NAME` child, folded to FCS's single `idText` (`"|Foo|_|"`).
    // Every other member sig reads its name from the `VAL_SIG`'s ident (an
    // operator name keeps the bare operator under `IDENT_TOK`, like a `val` sig).
    let name = if leading == MemberSigLeading::New {
        "new".to_string()
    } else if let Some(active) = vs.active_pat_name() {
        active_pat_id_text(&active)
    } else {
        strip_backticks(
            vs.ident()
                .expect("a member signature must have a name")
                .text(),
        )
        .to_string()
    };
    let ty = normalise_type(
        &vs.ty()
            .expect("a member signature must have a `: <type>` signature"),
    );
    // Attribute lists (FCS's `SynValSig.attributes`) — leading `MEMBER_SIG`
    // children; local `_argN` counter, as the other member homes.
    let mut counter = 0u32;
    let attributes = normalise_attribute_lists(ms.attributes(), &mut counter);
    // The `= <literal>` value (phase 10.12 member-literal) — the `VAL_SIG`'s
    // `Expr` child (FCS's `SynValSig.synExpr`), read via the shared accessor and
    // projected through the expression normaliser. `None` for a bodyless sig.
    let literal = vs
        .literal_value()
        .map(|e| Box::new(normalise_expr(&e, &mut counter)));
    NormalisedMember::AbstractSlot {
        name,
        ty,
        leading_keyword,
        attributes,
        literal,
        // FCS keeps a legal sig member's *overall* access in
        // `SynValSig.accessibility`; a trailing `with private get` accessor
        // modifier lives on the per-accessor slot (overall stays `None`).
        access: cst_member_sig_access(ms.syntax(), vs.syntax()),
    }
}

/// Flatten an indexer setter's index pattern for the FCS-faithful arg tuple
/// (phase 9.14b). FCS builds a single `SynPat.Tuple` from the index params plus
/// the value, *flattening only a parenthesised-tuple* index: `set (i, j) v` →
/// `Tuple(i, j, v)` (the `(i, j)` paren-tuple's elements are spread). Every other
/// index contributes a single element **unchanged** — a bare `i` (`set i v` →
/// `Tuple(i, v)`) and, crucially, a parenthesised *singleton* `(i)` whose `Paren`
/// FCS keeps (`set (i) v` → `Tuple(Paren(i), v)`), so do not unwrap it.
///
/// Only a *non-struct* paren-tuple index is flattened: FCS's
/// `[ SynPat.Paren(SynPat.Tuple(false, …), _); valuePat ]` arm matches
/// `isStruct = false` explicitly (`pars.fsy` get/set adjustment), so a
/// parenthesised *struct* tuple index `set (struct (i, j)) v` stays a single
/// `Paren(Tuple(true, …))` argument (FCS:
/// `Tuple(false, [Paren(Tuple(true, [i; j])); v])`).
fn flatten_setter_index(index: NormalisedPat) -> Vec<NormalisedPat> {
    match index {
        NormalisedPat::Paren(inner)
            if matches!(
                *inner,
                NormalisedPat::Tuple {
                    is_struct: false,
                    ..
                }
            ) =>
        {
            match *inner {
                NormalisedPat::Tuple { elements, .. } => elements,
                _ => unreachable!("guarded by the matches! above"),
            }
        }
        other => vec![other],
    }
}

/// Project one `SynEnumCase` (phase 9.6): the case name and its value
/// expression. Enum values are `atomicExpr` (compile-time constants — no `fun`
/// lambdas), so a fresh `_argN` counter suffices; a `Types` decl has no sibling
/// lambdas to share one with.
fn normalise_enum_case(c: &EnumCase) -> NormalisedEnumCase {
    let mut counter = 0u32;
    NormalisedEnumCase {
        attributes: normalise_attribute_lists(c.attributes(), &mut counter),
        // An operator-named enum case (`| ([]) = 0`) is valid (FCS's bar-led
        // `unionCaseName EQUALS atomicExpr`), with the same `op_Nil` /
        // `op_ColonColon` name as a union case — the shared production accessor.
        ident: c
            .ident()
            .map(|t| strip_backticks(t.text()).to_string())
            .or_else(|| c.operator_name().map(|(n, _)| n.to_string()))
            .unwrap_or_default(),
        value: normalise_expr(
            &c.value()
                .expect("ENUM_CASE must contain a value expression"),
            &mut counter,
        ),
    }
}

/// Project one `SynUnionCase` (phase 9.5): the case name and its `of` fields.
fn normalise_union_case(c: &UnionCase) -> NormalisedUnionCase {
    let mut counter = 0u32;
    // The `FullType` signature form (`Name : topType`) carries a `Type` child in
    // place of the `of`-field list; the ordinary `Fields` form carries the
    // fields (possibly none, for a nullary case).
    let kind = match c.full_type() {
        Some(ty) => NormalisedUnionCaseKind::FullType(normalise_type(&ty)),
        None => NormalisedUnionCaseKind::Fields(
            c.fields().map(|f| normalise_union_case_field(&f)).collect(),
        ),
    };
    NormalisedUnionCase {
        attributes: normalise_attribute_lists(c.attributes(), &mut counter),
        ident: c
            .ident()
            .map(|t| strip_backticks(t.text()).to_string())
            .or_else(|| c.operator_name().map(|(n, _)| n.to_string()))
            .unwrap_or_default(),
        kind,
    }
}

/// Project one union-case field `SynField` (phase 9.5): the optional name and
/// the field type. Union-case fields are never `mutable`. Attributes on `of`
/// fields (`X of [<A>] int`) are a later slice, so `attributes` is left empty
/// here (FCS-match only for unattributed `of` fields).
fn normalise_union_case_field(f: &UnionCaseField) -> NormalisedField {
    NormalisedField {
        attributes: Vec::new(),
        // Union-case `of` fields take no access modifier (FCS's `SynField`
        // accessibility is `None` here).
        access: None,
        name: f.ident().map(|t| strip_backticks(t.text()).to_string()),
        ty: normalise_type(&f.ty().expect("UNION_CASE_FIELD must contain a field type")),
        is_mutable: false,
        is_static: false,
    }
}

/// Project one record `SynField` (phase 9.4): the attributes (phase 10.7), name,
/// mutability, and field type.
fn normalise_field(f: &RecordFieldDecl) -> NormalisedField {
    let mut counter = 0u32;
    NormalisedField {
        attributes: normalise_attribute_lists(f.attributes(), &mut counter),
        // A record field takes no access modifier in F# syntax; a stray one
        // (`{ private X : int }`) is a recovery case FCS discards, leaving
        // `SynField.accessibility = None`. Mirror that (like union-case fields)
        // rather than projecting the recovery `ACCESS_TOK` our tree captures.
        access: None,
        name: f.ident().map(|t| strip_backticks(t.text()).to_string()),
        ty: normalise_type(&f.ty().expect("RECORD_FIELD_DECL must contain a field type")),
        is_mutable: f.is_mutable(),
        is_static: false,
    }
}

/// Project one `val` field `SynField` (phase 9.9b): the attributes (phase 10.7i),
/// name, mutability, the `static` flag, and the field type.
fn normalise_val_field(f: &ValField) -> NormalisedField {
    // Local `_argN` counter, as the other field/member homes (a bare attribute
    // consumes none; a `val` field has no sibling lambda to share one with).
    let mut counter = 0u32;
    NormalisedField {
        attributes: normalise_attribute_lists(f.attributes(), &mut counter),
        // `SynField.accessibility` (field 6) — `val mutable internal x : int`.
        access: cst_access(f.syntax()),
        name: f.ident().map(|t| strip_backticks(t.text()).to_string()),
        ty: normalise_type(&f.ty().expect("VAL_FIELD must contain a field type")),
        is_mutable: f.is_mutable(),
        is_static: f.is_static(),
    }
}

/// Project an `OPEN_DECL`'s target. `open type T` carries a [`Type`] child
/// (`SynOpenDeclTarget.Type`); the plain `open Foo.Bar` form carries a bare
/// [`LongIdent`] child (`SynOpenDeclTarget.ModuleOrNamespace`).
fn normalise_open_target(o: &OpenDecl) -> NormalisedOpenTarget {
    if let Some(ty) = o.ty() {
        NormalisedOpenTarget::Type(normalise_type(&ty))
    } else {
        let path = o
            .long_ident()
            .expect("OPEN_DECL module/namespace target must contain a LONG_IDENT child")
            .idents()
            .map(|tok| strip_backticks(tok.text()).to_string())
            .collect();
        NormalisedOpenTarget::ModuleOrNamespace(path)
    }
}

/// Project a `BINDING`. The `leading_keyword` is supplied by the caller, which
/// owns the keyword token (`LET_TOK`/`AND_TOK` in a `LET_DECL`,
/// `BINDER_TOK`/`AND_BANG_TOK` in a `LET_OR_USE_EXPR`) and the binding's
/// position in the group — the `BINDING` node itself does not contain it.
fn normalise_binding(
    b: &Binding,
    leading_keyword: NormalisedLeadingKeyword,
    counter: &mut u32,
) -> NormalisedBinding {
    let pat = b
        .pat()
        .expect("BINDING must contain exactly one pattern child");
    // The binding's own attribute lists — the `let [<Literal>] x = …` form, where
    // the run sits between the keyword and the pattern (`SynBinding.attributes`).
    // The pre-`let` form (`[<A>] let x`) instead carries its attributes on the
    // enclosing `LET_DECL`; `normalise_decl` prepends those to the first binding.
    // Normalise in source order before the pattern/RHS so an attribute argument
    // lambda consumes the shared `_argN` counter exactly where FCS parses it.
    let attributes = normalise_attribute_lists(b.attributes(), counter);
    let pat = normalise_pat(&pat, counter);
    // The RHS is absent only on a recovered (parser-bailed) binding — e.g.
    // `let x =` with nothing after the `=`, which our parser recovers as a
    // `BINDING` with no `Expr` child (a zero-width `ERROR` placeholder). FCS
    // recovers the same hole as `SynExpr.ArbitraryAfterError`; both project to
    // `NormalisedExpr::Error` (the Phase 11 recovery marker). A well-formed
    // binding always has an RHS, so this only fires under `allow_errors`. The
    // marker still flows through the return-type wrapping below: FCS wraps even
    // a recovered RHS in `Typed` when annotated (`let x : int =` ⇒
    // `Typed(ArbitraryAfterError, int)`), so a bare `Error` here would diverge.
    let expr = match b.expr() {
        Some(e) => normalise_expr(&e, counter),
        None => NormalisedExpr::Error,
    };
    // A return-type annotation (`let x : T = …`) lives in a sibling
    // `BINDING_RETURN_INFO` node, but FCS records it by *also* wrapping the
    // RHS in `SynExpr.Typed(rhs, T)` (`mkSynBindingRhs`, `SyntaxTreeOps.fs:747`)
    // with the same type — and our FCS-side projector reads only that wrapped
    // `expr` field. Reconstruct the same wrapper here so both sides agree
    // without modelling `returnInfo` as a distinct field. The type carries no
    // `fun`-lambdas, so it doesn't perturb the shared `_argN` counter.
    //
    // A *bang* binder (`let! x : T = …`, `AllowTypedLetUseAndBang`) is the
    // exception: FCS's `ceBindingCore` sets `returnInfo` but does **not** wrap
    // the RHS in `Typed`, so the FCS-side `expr` stays bare. Skip the synthesis
    // for the bang leading keywords, leaving the return type elided on both
    // sides (we carry no distinct `returnInfo` field), matching FCS.
    let is_bang = matches!(
        leading_keyword,
        NormalisedLeadingKeyword::LetBang
            | NormalisedLeadingKeyword::UseBang
            | NormalisedLeadingKeyword::AndBang
    );
    let expr = match b.return_type() {
        Some(ty) if !is_bang => NormalisedExpr::Typed {
            expr: Box::new(expr),
            ty: normalise_type(&ty),
        },
        _ => expr,
    };
    NormalisedBinding {
        leading_keyword,
        is_mutable: b.is_mutable(),
        is_inline: b.is_inline(),
        attributes,
        // FCS homes a binding's access on its head pattern; our tree captures it
        // as an `ACCESS_TOK` child of the `BINDING` node (a sibling of the
        // pattern), so read it here.
        access: cst_access(b.syntax()),
        pat,
        expr,
    }
}

/// Project an `ATTRIBUTE` node. The path segments come from the `LONG_IDENT`
/// child (backticks stripped). The optional `attributeTarget` word (phase
/// 10.5c) is the `ATTRIBUTE_TARGET` child's ident text — matching FCS's
/// `Target` idText. The optional argument expression (phase 10.5b) is projected
/// from the `Expr` child when present; a bare attribute has no such child and
/// so carries FCS's synthetic `mkSynUnit` (`NormalisedExpr::Const` of
/// [`NormalisedConst::Unit`]).
/// Project a run of `ATTRIBUTE_LIST` nodes (phase 10.7) to
/// `Vec<Vec<NormalisedAttribute>>` — one inner vec per `SynAttributeList`. Shared
/// by every attribute carrier (type headers, union/enum cases, record fields).
fn normalise_attribute_lists(
    lists: impl Iterator<Item = AttributeList>,
    counter: &mut u32,
) -> Vec<Vec<NormalisedAttribute>> {
    lists
        .map(|list| {
            list.attributes()
                .map(|a| normalise_attribute(&a, counter))
                .collect()
        })
        .collect()
}

fn normalise_attribute(a: &Attribute, counter: &mut u32) -> NormalisedAttribute {
    let path = a
        .type_name()
        .expect("ATTRIBUTE must contain a LONG_IDENT type-name child");
    let type_name = path
        .idents()
        .map(|tok| strip_backticks(tok.text()).to_string())
        .collect();
    let target = a
        .target()
        .map(|tok| strip_backticks(tok.text()).to_string());
    let arg = match a.arg() {
        Some(e) => normalise_expr(&e, counter),
        None => NormalisedExpr::Const(NormalisedConst::Unit),
    };
    NormalisedAttribute {
        type_name,
        target,
        arg,
    }
}

fn normalise_pat(p: &Pat, counter: &mut u32) -> NormalisedPat {
    match p {
        Pat::Named(n) => NormalisedPat::Named(normalise_named_pat(n)),
        Pat::LongIdent(l) => normalise_long_ident_pat(l, counter),
        Pat::Wildcard(_) => NormalisedPat::Wildcard,
        Pat::Paren(p) => NormalisedPat::Paren(Box::new(normalise_paren_pat(p, counter))),
        Pat::Const(c) => NormalisedPat::Const(normalise_const_pat(c)),
        Pat::Null(_) => NormalisedPat::Null,
        Pat::Typed(t) => normalise_typed_pat(t, counter),
        Pat::Tuple(t) => normalise_tuple_pat(t, counter),
        Pat::As(a) => normalise_as_pat(a, counter),
        Pat::ArrayOrList(a) => NormalisedPat::ArrayOrList {
            is_array: a.is_array(),
            elements: a.elements().map(|e| normalise_pat(&e, counter)).collect(),
        },
        Pat::Record(r) => NormalisedPat::Record {
            fields: r
                .fields()
                .map(|f| {
                    let name = f
                        .name()
                        .expect("RECORD_PAT_FIELD must contain a field-name LONG_IDENT")
                        .idents()
                        .map(|tok| strip_backticks(tok.text()).to_string())
                        .collect();
                    let pat = f
                        .pat()
                        .expect("RECORD_PAT_FIELD must contain a value pattern");
                    (name, normalise_pat(&pat, counter))
                })
                .collect(),
        },
        Pat::IsInst(i) => NormalisedPat::IsInst {
            ty: normalise_type(&i.ty().expect("IS_INST_PAT must contain a tested type")),
        },
        Pat::ListCons(c) => NormalisedPat::ListCons {
            lhs: Box::new(normalise_pat(
                &c.lhs().expect("LIST_CONS_PAT must contain a head"),
                counter,
            )),
            rhs: Box::new(normalise_pat(
                &c.rhs().expect("LIST_CONS_PAT must contain a tail"),
                counter,
            )),
        },
        Pat::Ands(a) => NormalisedPat::Ands {
            pats: a.operands().map(|p| normalise_pat(&p, counter)).collect(),
        },
        Pat::Or(o) => NormalisedPat::Or {
            lhs: Box::new(normalise_pat(
                &o.lhs().expect("OR_PAT must contain a left operand"),
                counter,
            )),
            rhs: Box::new(normalise_pat(
                &o.rhs().expect("OR_PAT must contain a right operand"),
                counter,
            )),
        },
        Pat::Attrib(a) => {
            // Attribute arguments are expressions that occur before the wrapped
            // pattern in source, so they use the enclosing `_argN` counter and
            // advance it before any nested attributed sub-patterns.
            let attributes = a
                .attributes()
                .map(|list| {
                    list.attributes()
                        .map(|att| normalise_attribute(&att, counter))
                        .collect()
                })
                .collect();
            let pat = Box::new(normalise_pat(
                &a.pat().expect("ATTRIB_PAT must wrap an inner pattern"),
                counter,
            ));
            NormalisedPat::Attrib { pat, attributes }
        }
        Pat::OptionalVal(o) => {
            // `?ident` — `SynPat.OptionalVal`. The `IDENT_TOK` text (backticks
            // stripped) is FCS's `Ident.idText`, mirroring `normalise_named_pat`.
            let tok = o
                .ident()
                .expect("OPTIONAL_VAL_PAT must contain an IDENT_TOK");
            NormalisedPat::OptionalVal(strip_backticks(tok.text()).to_string())
        }
        Pat::Quote(q) => {
            // `<@ … @>` in pattern position — `SynPat.QuoteExpr(SynExpr, range)`.
            // `QUOTE_PAT` wraps one `QUOTE_EXPR`, which normalises to
            // `NormalisedExpr::Quote`; reuse the expr projector wholesale.
            //
            // The `_argN` counter is *local* here, exactly as in the `Attrib`
            // arm: a `fun`-lambda inside a pattern-position quotation
            // (`SpecificCall <@ fun x -> x @> …`) is a corner with no oracle, and
            // threading the enclosing lambda counter through `normalise_pat`
            // would ripple across every pattern caller for it. Realistic
            // active-pattern-argument quotations name a value (`<@ SomeCall @>`)
            // and consume no slot. Documented limitation, never reached by a
            // diff test.
            let inner = q.inner().expect("QUOTE_PAT must wrap a QUOTE_EXPR");
            let mut local = 0u32;
            NormalisedPat::QuoteExpr(Box::new(normalise_expr(&inner, &mut local)))
        }
    }
}

fn normalise_as_pat(a: &AsPat, counter: &mut u32) -> NormalisedPat {
    let lhs = a.lhs().expect("AS_PAT must contain a left-hand pattern");
    let rhs = a.rhs().expect("AS_PAT must contain a right-hand pattern");
    NormalisedPat::As {
        lhs: Box::new(normalise_pat(&lhs, counter)),
        rhs: Box::new(normalise_pat(&rhs, counter)),
    }
}

fn normalise_paren_pat(p: &ParenPat, counter: &mut u32) -> NormalisedPat {
    let inner = p.inner().expect("PAREN_PAT must wrap an inner pattern");
    normalise_pat(&inner, counter)
}

fn normalise_typed_pat(t: &TypedPat, counter: &mut u32) -> NormalisedPat {
    let inner = t.pat().expect("TYPED_PAT must contain an inner pattern");
    let ty = t.ty().expect("TYPED_PAT must contain a type annotation");
    NormalisedPat::Typed {
        pat: Box::new(normalise_pat(&inner, counter)),
        ty: normalise_type(&ty),
    }
}

fn normalise_tuple_pat(t: &TuplePat, counter: &mut u32) -> NormalisedPat {
    let elements: Vec<NormalisedPat> = t.elements().map(|e| normalise_pat(&e, counter)).collect();
    debug_assert!(
        elements.len() >= 2,
        "TUPLE_PAT must have at least two element patterns, got {}",
        elements.len(),
    );
    NormalisedPat::Tuple {
        is_struct: t.is_struct(),
        elements,
    }
}

fn normalise_const_pat(p: &ConstPat) -> NormalisedConst {
    // A `CONST_PAT` with no literal-token child is the synthetic empty
    // body emitted under `PAREN_PAT` for `()` — mirrors FCS's
    // `parenPatternBody → /* empty */ → SynPat.Const(SynConst.Unit, _)`
    // (`pars.fsy:3873`). For non-unit literals the token is mandatory.
    match p.literal() {
        Some(lit) => normalise_const_lit(&lit),
        None => NormalisedConst::Unit,
    }
}

fn normalise_named_pat(p: &NamedPat) -> String {
    // A nullary active-pattern occurrence collapses to `SynPat.Named` (FCS's
    // maybe-var rule); its name is an `ACTIVE_PAT_NAME`, not an `IDENT_TOK`.
    if let Some(active) = p.active_pat_name() {
        return active_pat_id_text(&active);
    }
    let tok = p.ident().expect("NAMED_PAT must contain an IDENT_TOK");
    strip_backticks(tok.text()).to_string()
}

/// Rebuild FCS's single `idText` for an active-pattern name from its case
/// tokens: `"|"` + the case texts joined by `"|"` + `"|"` (a total
/// `(|Foo|Bar|)` → `"|Foo|Bar|"`, a partial `(|Foo|_|)` → `"|Foo|_|"`).
fn active_pat_id_text(active: &borzoi_cst::syntax::ActivePatName) -> String {
    let cases: Vec<String> = active
        .case_tokens()
        .map(|tok| strip_backticks(tok.text()).to_string())
        .collect();
    format!("|{}|", cases.join("|"))
}

/// Project our `LONG_IDENT_PAT > [LONG_IDENT, <args…>]` back into FCS's
/// `SynPat.LongIdent` shape: a head `Vec<String>` of long-ident segments plus
/// an `args: NormalisedArgPats`. The curried form (`LONG_IDENT_PAT > [LONG_IDENT,
/// <arg-pat>, …]`) maps to `SynArgPats.Pats`; the named-field form
/// (`LONG_IDENT_PAT > [LONG_IDENT, NAME_PAT_PAIRS]`) to `SynArgPats.NamePatPairs`.
/// The long-ident-pattern head segments — FCS's `SynPat.LongIdent.longDotId`
/// folded to `Vec<String>`. An active-pattern head (`(|Foo|Bar|)`) carries an
/// `ACTIVE_PAT_NAME` child whose whole name FCS folds into one `SynLongIdent`
/// segment (`"|Foo|Bar|"`, partial `"|Foo|_|"`), rebuilt from the case tokens. A
/// *dotted self-id* active-pattern member (`member x.(|Foo|Bar|)`) carries *both*
/// a `LONG_IDENT` (the path segments) and the `ACTIVE_PAT_NAME` — FCS's
/// `["x"; "|Foo|Bar|"]` — so the folded segment is appended after the path. The
/// operator dotted form (`member x.(+)`) keeps its `( op )` tokens inside the
/// `LONG_IDENT`, so it has no `ACTIVE_PAT_NAME` and reads through `idents()` as
/// `["x", "+"]`. Shared by [`normalise_long_ident_pat`] and the get/set-member
/// name projection (a dotted op/active-pattern property name uses the same head).
fn long_ident_pat_head_segments(p: &LongIdentPat) -> Vec<String> {
    match (p.head(), p.active_pat_name()) {
        (head_node, Some(active)) => {
            let mut segs: Vec<String> = Vec::new();
            if let Some(li) = head_node {
                segs.extend(
                    li.idents()
                        .map(|tok| strip_backticks(tok.text()).to_string()),
                );
            }
            segs.push(active_pat_id_text(&active));
            segs
        }
        (Some(head_node), None) => head_node
            .idents()
            .map(|tok| strip_backticks(tok.text()).to_string())
            .collect(),
        (None, None) => {
            panic!("LONG_IDENT_PAT must contain a LONG_IDENT or ACTIVE_PAT_NAME head child")
        }
    }
}

fn normalise_long_ident_pat(p: &LongIdentPat, counter: &mut u32) -> NormalisedPat {
    let head = long_ident_pat_head_segments(p);
    // Explicit value-typar declarations (`let f<'a> …`) — the optional
    // `TYPAR_DECLS` child between the head and the args. Flattened to the typar
    // list (the `PostfixList`/etc. variant is elided), mirroring the type-defn
    // header projection. Empty for a non-generic head.
    let typars = p
        .typar_decls()
        .map(|ds| ds.typars().map(|t| normalise_typar(&t)).collect())
        .unwrap_or_default();
    let args = match p.name_pat_pairs() {
        Some(group) => NormalisedArgPats::NamePatPairs(
            group
                .pairs()
                .map(|pair| {
                    let name = strip_backticks(
                        pair.name()
                            .expect("NAME_PAT_PAIR must contain a field-name IDENT_TOK")
                            .text(),
                    )
                    .to_string();
                    let pat = pair
                        .pat()
                        .expect("NAME_PAT_PAIR must contain a value pattern");
                    (name, normalise_pat(&pat, counter))
                })
                .collect(),
        ),
        None => NormalisedArgPats::Pats(p.args().map(|a| normalise_pat(&a, counter)).collect()),
    };
    NormalisedPat::LongIdent { head, typars, args }
}

/// `counter` is the shared `_argN` generator for any `fun`-lambda found
/// while walking this expression (see [`normalise_decl`] for the reset
/// boundary and [`normalise_fun`] for how it's consumed). It threads
/// through every sub-expression in source order so nested and sibling
/// lambdas claim numbers in FCS's parse-reduction order.
fn normalise_expr(e: &Expr, counter: &mut u32) -> NormalisedExpr {
    match e {
        Expr::Const(c) => NormalisedExpr::Const(normalise_const(c)),
        Expr::MeasureLit(m) => NormalisedExpr::Const(normalise_measure_lit(m)),
        Expr::Null(_) => NormalisedExpr::Null,
        Expr::Ident(i) => NormalisedExpr::Ident(normalise_ident(i)),
        Expr::Typar(t) => NormalisedExpr::Typar({
            let tok = t.ident().expect("TYPAR_EXPR must contain an IDENT_TOK");
            strip_backticks(tok.text()).to_string()
        }),
        Expr::LongIdent(l) => NormalisedExpr::LongIdent(normalise_long_ident(l)),
        Expr::Paren(p) => NormalisedExpr::Paren(Box::new(normalise_paren(p, counter))),
        Expr::Tuple(t) => normalise_tuple(t, counter),
        Expr::App(a) => normalise_app(a, counter),
        Expr::DotGet(d) => normalise_dot_get(d, counter),
        Expr::Dynamic(d) => normalise_dynamic(d, counter),
        Expr::DotLambda(d) => NormalisedExpr::DotLambda {
            expr: Box::new(normalise_expr(
                &d.expr()
                    .expect("DOT_LAMBDA_EXPR must contain a body Expr child"),
                counter,
            )),
        },
        Expr::DotIndexedGet(d) => normalise_dot_indexed_get(d, counter),
        Expr::IndexRange(r) => normalise_index_range(r, counter),
        Expr::IndexFromEnd(e) => NormalisedExpr::IndexFromEnd {
            expr: Box::new(normalise_expr(
                &e.expr()
                    .expect("INDEX_FROM_END_EXPR must contain a bound expr"),
                counter,
            )),
        },
        Expr::AddressOf(a) => normalise_address_of(a, counter),
        Expr::New(n) => normalise_new(n, counter),
        Expr::ObjExpr(o) => normalise_obj_expr(o, counter),
        Expr::InferredUpcast(u) => NormalisedExpr::InferredUpcast {
            expr: Box::new(normalise_expr(
                &u.expr()
                    .expect("INFERRED_UPCAST_EXPR must contain an inner Expr child"),
                counter,
            )),
        },
        Expr::InferredDowncast(d) => NormalisedExpr::InferredDowncast {
            expr: Box::new(normalise_expr(
                &d.expr()
                    .expect("INFERRED_DOWNCAST_EXPR must contain an inner Expr child"),
                counter,
            )),
        },
        Expr::Lazy(l) => NormalisedExpr::Lazy {
            expr: Box::new(normalise_expr(
                &l.expr()
                    .expect("LAZY_EXPR must contain an inner Expr child"),
                counter,
            )),
        },
        Expr::Assert(a) => NormalisedExpr::Assert {
            expr: Box::new(normalise_expr(
                &a.expr()
                    .expect("ASSERT_EXPR must contain an inner Expr child"),
                counter,
            )),
        },
        Expr::Fixed(f) => NormalisedExpr::Fixed {
            expr: Box::new(normalise_expr(
                &f.expr()
                    .expect("FIXED_EXPR must contain an inner Expr child"),
                counter,
            )),
        },
        Expr::TypeApp(t) => normalise_type_app(t, counter),
        Expr::Assign(a) => normalise_assign(a, counter),
        Expr::Typed(t) => normalise_typed(t, counter),
        Expr::TypeTest(t) => normalise_type_test(t, counter),
        Expr::Upcast(u) => normalise_upcast(u, counter),
        Expr::Downcast(d) => normalise_downcast(d, counter),
        Expr::Cons(c) => normalise_cons(c, counter),
        Expr::JoinIn(j) => normalise_join_in(j, counter),
        Expr::IfThenElse(i) => normalise_if_then_else(i, counter),
        Expr::Sequential(s) => NormalisedExpr::Sequential(
            s.statements()
                .map(|e| normalise_expr(&e, counter))
                .collect(),
        ),
        Expr::InterpString(s) => normalise_interp_string(s, counter),
        Expr::Fun(f) => normalise_fun(f, counter),
        // Inline IL `(# … #)` (`SynExpr.LibraryOnlyILAssembly`) is not modelled
        // in the diff oracle: FCS boxes the parsed IL instructions
        // (`ilCode: obj`), which the dump cannot round-trip for an equality
        // check. Treated as a closed-world gap — the corpus sweep wraps this in
        // `catch_unwind` and counts the file as unmodeled (FCS's `from_fcs`
        // side likewise `panic!`s on the `LibraryOnlyILAssembly` SynExpr case),
        // so neither side reaches the equality assertion.
        Expr::InlineIl(_) => panic!("inline IL (SynExpr.LibraryOnlyILAssembly) is not modelled"),
        Expr::StaticOptimization(s) => normalise_static_optimization(s, counter),
        Expr::LibraryOnlyFieldGet(g) => normalise_library_only_field_get(g, counter),
        Expr::TraitCall(t) => normalise_trait_call(t, counter),
        Expr::Quote(q) => NormalisedExpr::Quote {
            is_raw: q.is_raw(),
            inner: Box::new(normalise_expr(
                &q.inner()
                    .expect("QUOTE_EXPR must contain an inner Expr child"),
                counter,
            )),
        },
        Expr::Computation(c) => NormalisedExpr::ComputationExpr(Box::new(normalise_expr(
            &c.inner()
                .expect("COMPUTATION_EXPR must contain an inner Expr child"),
            counter,
        ))),
        Expr::Record(r) => {
            // `{ inherit Base(args); … }` — FCS's `baseInfo`. The base type and the
            // args expression; FCS synthesises `Const(Unit)` for a bare
            // `inherit Base` / `inherit Base()` (no args in our tree), so mirror
            // that. Projected before the fields for source-order `_argN` counting.
            let inherit_info = r.inherit().map(|inh| {
                let ty = inh
                    .base_type()
                    .map(|t| normalise_type(&t))
                    .unwrap_or(NormalisedType::Anon);
                let args = match inh.args() {
                    Some(a) => normalise_expr(&a, counter),
                    None => NormalisedExpr::Const(NormalisedConst::Unit),
                };
                (ty, Box::new(args))
            });
            // Copy source first (source order), then each field's value, so the
            // shared `_argN` counter advances left-to-right.
            let copy = r
                .copy_source()
                .map(|src| Box::new(normalise_expr(&src, counter)));
            let fields = normalise_record_fields(r.fields(), counter);
            NormalisedExpr::Record {
                inherit_info,
                copy,
                fields,
            }
        }
        Expr::AnonRecd(r) => {
            // Same shape as `Record`, minus `baseInfo`. `is_struct` reads the
            // leading `STRUCT_TOK` (`struct {| … |}`). Copy source before field
            // values (source order) for the shared lambda counter.
            let copy = r
                .copy_source()
                .map(|src| Box::new(normalise_expr(&src, counter)));
            let fields = normalise_record_fields(r.fields(), counter);
            NormalisedExpr::AnonRecd {
                is_struct: r.is_struct(),
                copy,
                fields,
            }
        }
        Expr::ArrayOrList(a) => {
            // FCS splits the two AST variants by emptiness: an empty `[]` /
            // `[||]` (no body child) is `SynExpr.ArrayOrList(isArray, [], _)`,
            // a non-empty bracket is `ArrayOrListComputed(isArray, body, _)`
            // whose body is the single `SEQUENTIAL_EXPR`/element child.
            match a.inner() {
                Some(inner) => NormalisedExpr::ArrayOrListComputed {
                    is_array: a.is_array(),
                    inner: Box::new(normalise_expr(&inner, counter)),
                },
                None => NormalisedExpr::ArrayOrList {
                    is_array: a.is_array(),
                    elements: vec![],
                },
            }
        }
        Expr::Yield(y) => {
            let is_yield = y.is_yield();
            NormalisedExpr::YieldOrReturn {
                // FCS's flag pair is `(isYield, !isYield)` for the
                // source-written forms (`yield`/`return`/`yield!`/`return!`).
                flags: (is_yield, !is_yield),
                from: y.is_from(),
                inner: Box::new(normalise_expr(
                    &y.inner()
                        .expect("YIELD_OR_RETURN[_FROM]_EXPR must contain an inner Expr child"),
                    counter,
                )),
            }
        }
        Expr::DoBang(d) => NormalisedExpr::DoBang(Box::new(normalise_expr(
            &d.inner()
                .expect("DO_BANG_EXPR must contain an inner Expr child"),
            counter,
        ))),
        Expr::Do(d) => NormalisedExpr::Do(Box::new(normalise_expr(
            &d.inner().expect("DO_EXPR must contain an inner Expr child"),
            counter,
        ))),
        Expr::LetOrUse(l) => {
            // Two forms share `LET_OR_USE_EXPR`, told apart by the head token:
            // the bang form (`let!`/`use!`, head `BINDER_TOK`, never recursive)
            // and the plain expression-level form (`let`/`use`, head `LET_TOK`,
            // possibly `rec`). The follower keyword matches (`AndBang` vs `And`).
            // Bindings are projected before the body so the shared `_argN`
            // counter advances in source order.
            let is_rec = l.is_rec();
            let (head, follower) = match (l.is_bang(), l.is_use(), is_rec) {
                (true, false, _) => (
                    NormalisedLeadingKeyword::LetBang,
                    NormalisedLeadingKeyword::AndBang,
                ),
                (true, true, _) => (
                    NormalisedLeadingKeyword::UseBang,
                    NormalisedLeadingKeyword::AndBang,
                ),
                (false, false, false) => {
                    (NormalisedLeadingKeyword::Let, NormalisedLeadingKeyword::And)
                }
                (false, false, true) => (
                    NormalisedLeadingKeyword::LetRec,
                    NormalisedLeadingKeyword::And,
                ),
                (false, true, false) => {
                    (NormalisedLeadingKeyword::Use, NormalisedLeadingKeyword::And)
                }
                (false, true, true) => (
                    NormalisedLeadingKeyword::UseRec,
                    NormalisedLeadingKeyword::And,
                ),
            };
            let bindings = l
                .bindings()
                .enumerate()
                .map(|(i, b)| {
                    let lk = if i == 0 { head } else { follower };
                    normalise_binding(&b, lk, counter)
                })
                .collect();
            // A missing body is the `let z = e in` / `let z =` recovery hole
            // (the binding parsed, but nothing follows as the block body): FCS
            // fills it with `SynExpr.ArbitraryAfterError`, so both sides project
            // to `NormalisedExpr::Error` (Phase 11). Only fires under
            // `allow_errors`; well-formed input always has a body.
            let body = match l.body() {
                Some(b) => normalise_expr(&b, counter),
                None => NormalisedExpr::Error,
            };
            NormalisedExpr::LetOrUse {
                is_rec,
                bindings,
                body: Box::new(body),
            }
        }
        Expr::Match(m) => normalise_match(m, counter),
        Expr::MatchLambda(m) => normalise_match_lambda(m, counter),
        Expr::MatchBang(m) => normalise_match_bang(m, counter),
        Expr::While(w) => {
            // `while cond do body` — cond then body, in source order, so the
            // shared `_argN` counter advances left-to-right.
            let cond = w
                .cond()
                .expect("WHILE_EXPR must contain a condition Expr child");
            let cond = Box::new(normalise_expr(&cond, counter));
            let body = w.body().expect("WHILE_EXPR must contain a body Expr child");
            NormalisedExpr::While {
                cond,
                body: Box::new(normalise_expr(&body, counter)),
            }
        }
        Expr::WhileBang(w) => {
            // `while! cond do body` — mirrors `While` exactly (cond then body,
            // source order); only the wrapping variant differs, keeping
            // `while!` distinct from `while` in the diff.
            let cond = w
                .cond()
                .expect("WHILE_BANG_EXPR must contain a condition Expr child");
            let cond = Box::new(normalise_expr(&cond, counter));
            let body = w
                .body()
                .expect("WHILE_BANG_EXPR must contain a body Expr child");
            NormalisedExpr::WhileBang {
                cond,
                body: Box::new(normalise_expr(&body, counter)),
            }
        }
        Expr::ForEach(f) => {
            // `for pat in enumExpr do body` — binder pattern (including any
            // attributed-pattern argument expressions), collection, then body in
            // source order so the shared `_argN` counter advances left-to-right
            // (matching FCS's parse order).
            let pat = f
                .pat()
                .expect("FOR_EACH_EXPR must contain a binder Pat child");
            let pat = normalise_pat(&pat, counter);
            let enum_expr = f
                .enum_expr()
                .expect("FOR_EACH_EXPR must contain an enumerable Expr child");
            let enum_expr = Box::new(normalise_expr(&enum_expr, counter));
            let body = f
                .body()
                .expect("FOR_EACH_EXPR must contain a body Expr child");
            NormalisedExpr::ForEach {
                pat,
                enum_expr,
                body: Box::new(normalise_expr(&body, counter)),
            }
        }
        Expr::For(f) => {
            // `for ident = from to/downto to do body` — the two bounds then the
            // body in source order so the shared `_argN` counter advances
            // left-to-right. The loop variable is `idText` (backticks stripped
            // to match FCS's `Ident.idText`).
            let ident = f
                .ident()
                .map(|t| strip_backticks(t.text()).to_string())
                .unwrap_or_default();
            let from = f
                .from_expr()
                .expect("FOR_EXPR must contain a start-bound Expr child");
            let from = Box::new(normalise_expr(&from, counter));
            let ascending = f.is_ascending();
            let to = f
                .to_expr()
                .expect("FOR_EXPR must contain an end-bound Expr child");
            let to = Box::new(normalise_expr(&to, counter));
            let body = f.body().expect("FOR_EXPR must contain a body Expr child");
            NormalisedExpr::For {
                ident,
                from,
                ascending,
                to,
                body: Box::new(normalise_expr(&body, counter)),
            }
        }
        Expr::Try(t) => {
            // `try body with <clauses>` / `try body finally cleanup` —
            // discriminated by `FINALLY_TOK` presence. In both, the protected
            // body is normalised first, then the trailing part (handler clauses
            // or the finally body) in source order so the shared `_argN` counter
            // advances left-to-right (body, then clause results / guards or the
            // finally body, can all contain `fun`-lambdas).
            let body = t
                .try_expr()
                .expect("TRY_EXPR must contain a body Expr child");
            let body = Box::new(normalise_expr(&body, counter));
            if t.is_try_finally() {
                let finally = t
                    .finally_expr()
                    .expect("try/finally TRY_EXPR must contain a finally Expr child");
                let finally = Box::new(normalise_expr(&finally, counter));
                NormalisedExpr::TryFinally { body, finally }
            } else {
                // The handler list reuses `normalise_match_clause` verbatim
                // (FCS's `withCases` is the same `SynMatchClause list` as a
                // `match`).
                let clauses = normalise_match_clauses(t.with_clauses(), counter);
                NormalisedExpr::TryWith { body, clauses }
            }
        }
    }
}

/// Project our flat `FUN_EXPR > [FUN_TOK, <pat>+, RARROW_TOK, <body>]`
/// into the `parsedData`-shaped view: args list + real body. FCS's
/// runtime AST nests `Lambda` per curried arg (with single-arg
/// `SynSimplePats` payloads) but caches the parsed flat view on the
/// outermost node — projecting to the flat shape on our side and
/// digging the flat shape out of FCS's JSON dump on the other gives
/// the diff oracle a single ground truth.
///
/// The `args` slot is the *original* argument patterns (FCS's
/// `parsedData[0]`), but the `body` slot is the *lowered* body
/// (`parsedData[1]`): FCS's `PushCurriedPatternsToExpr`
/// (`SyntaxTreeOps.fs:420`) replaces every non-simple parameter pattern
/// with a compiler-generated `_argN` and wraps the body in
/// `match _argN with <pat> -> …`. We reproduce that lowering with a
/// right-to-left counter (`SynArgNameGenerator.New()` pre-increments
/// from 0, so the rightmost counter-consuming param claims `_arg1`).
///
/// `counter` is FCS's `SynArgNameGenerator`, which lives on the lexbuf and
/// is `.Reset()` only at each module-level definition (`pars.fsy:1310`),
/// so it is *shared across every `fun` in one definition* — threaded in
/// from [`normalise_decl`] via [`normalise_expr`]. Attribute arguments embedded
/// in the surface patterns are expression subtrees, so [`normalise_pat`] visits
/// them before the body in source order; the outer parameter *lowering* still
/// happens after the body, mirroring FCS's bottom-up parse-reduction order:
/// `fun 0 -> fun 1 -> 2` lowers to outer `_arg2` / inner `_arg1` because
/// the inner lambda reduces first.
///
/// Known gap (unprojected, no test): function-form binding heads
/// (`let f 0 = …`) also consume this generator before their RHS, but
/// [`normalise_binding`] does no binding-head lowering yet. Value bindings
/// (`let f = fun …`) are unaffected — the `Named` head is simple and
/// claims no slot, so the RHS lambda still starts at `_arg1`.
fn normalise_fun(f: &FunExpr, counter: &mut u32) -> NormalisedExpr {
    let args: Vec<NormalisedPat> = f.args().map(|p| normalise_pat(&p, counter)).collect();
    // A missing body is the `fun … ->` recovery hole — FCS fills it with
    // `SynExpr.ArbitraryAfterError`, which both sides project to
    // `NormalisedExpr::Error` (Phase 11). The args still fold around it below
    // (the `_argN` match-lowering wraps the `Error` body exactly as it wraps a
    // real one, staying symmetric with FCS's recovered lambda). Only fires under
    // `allow_errors`; a well-formed lambda always has a body.
    let mut lowered = match f.body() {
        Some(body) => normalise_expr(&body, counter),
        None => NormalisedExpr::Error,
    };
    for arg in args.iter().rev() {
        lowered = lower_fun_arg(arg, counter, lowered);
    }
    NormalisedExpr::Lambda {
        args,
        body: Box::new(lowered),
    }
}

/// Project a surface `MATCH_EXPR` to [`NormalisedExpr::Match`] — FCS's
/// `SynExpr.Match(_, scrutinee, clauses, _, _)` (`SyntaxTree.fsi:728`).
/// The `match` node itself claims no `_argN` slot, but its scrutinee and
/// clause patterns (via attributes), guards, and results can contain
/// `fun`-lambdas, so `counter` threads through them in source order
/// (scrutinee first, then each clause pattern / guard / result) to preserve
/// FCS's shared-generator numbering. Phase 5.M.1 produces a single clause with
/// no `when` guard (`when: None`); the clause pattern reuses the shared
/// [`normalise_pat`] projector.
fn normalise_match(m: &MatchExpr, counter: &mut u32) -> NormalisedExpr {
    let scrutinee = m
        .scrutinee()
        .expect("MATCH_EXPR must contain a scrutinee Expr child");
    let scrutinee = Box::new(super::canonicalise_scrutinee(normalise_expr(
        &scrutinee, counter,
    )));
    let clauses = normalise_match_clauses(m.clauses(), counter);
    NormalisedExpr::Match { scrutinee, clauses }
}

/// Project a surface `MATCH_BANG_EXPR` to [`NormalisedExpr::MatchBang`] —
/// FCS's `SynExpr.MatchBang(_, scrutinee, clauses, _, _)`
/// (`SyntaxTree.fsi:916`). Mirrors [`normalise_match`] exactly (same
/// scrutinee-then-clauses `_argN` threading); only the wrapping variant
/// differs, keeping `match!` distinct from `match` in the diff.
fn normalise_match_bang(m: &MatchBangExpr, counter: &mut u32) -> NormalisedExpr {
    let scrutinee = m
        .scrutinee()
        .expect("MATCH_BANG_EXPR must contain a scrutinee Expr child");
    let scrutinee = Box::new(super::canonicalise_scrutinee(normalise_expr(
        &scrutinee, counter,
    )));
    let clauses = normalise_match_clauses(m.clauses(), counter);
    NormalisedExpr::MatchBang { scrutinee, clauses }
}

/// Project a surface `MATCH_LAMBDA_EXPR` to [`NormalisedExpr::MatchLambda`]
/// — FCS's `SynExpr.MatchLambda`. Mirrors [`normalise_match`] minus the
/// scrutinee: the `function` node claims no `_argN` slot of its own, but
/// its clause results / guards can contain `fun`-lambdas, so `counter`
/// threads through the clauses in source order (reusing
/// [`normalise_match_clause`]) to preserve FCS's shared-generator
/// numbering.
fn normalise_match_lambda(m: &MatchLambdaExpr, counter: &mut u32) -> NormalisedExpr {
    let clauses = normalise_match_clauses(m.clauses(), counter);
    NormalisedExpr::MatchLambda { clauses }
}

/// Project one surface `MATCH_CLAUSE` to a [`NormalisedMatchClause`]. The
/// Projects the optional `when` guard (phase 5.M.3) into `when`. The guard
/// is normalised *before* the result because it precedes the result in
/// source order: the shared `_argN` counter (bumped by nested `fun`-lambda
/// lowering) must advance left-to-right to match FCS's numbering.
fn normalise_match_clause(c: &MatchClause, counter: &mut u32) -> NormalisedMatchClause {
    let pat = c.pat().expect("MATCH_CLAUSE must contain a pattern");
    let pat = normalise_pat(&pat, counter);
    let when = c.guard().map(|g| Box::new(normalise_expr(&g, counter)));
    // A missing result is the `match e with A ->` recovery hole — FCS fills it
    // with `SynExpr.ArbitraryAfterError`, which both sides project to
    // `NormalisedExpr::Error` (Phase 11). Only fires under `allow_errors`; a
    // well-formed clause always has a result.
    let result = match c.result() {
        Some(r) => normalise_expr(&r, counter),
        None => NormalisedExpr::Error,
    };
    NormalisedMatchClause {
        pat,
        when,
        result: Box::new(result),
    }
}

/// Project a clause list, dropping the spurious empty clause our parser leaves
/// at the `with`/`function` boundary on recovery — a `MATCH_CLAUSE` with no
/// pattern, emitted for `match e with` (and `function`) followed by nothing.
/// FCS recovers those as *zero* clauses, so the projection must drop it (a
/// well-formed clause always has a pattern, so this only fires under
/// `allow_errors`). Shared by every clause-list site (`match`, `match!`,
/// `function`, and the `try … with` handler) so the drop is uniform.
fn normalise_match_clauses(
    clauses: impl Iterator<Item = MatchClause>,
    counter: &mut u32,
) -> Vec<NormalisedMatchClause> {
    clauses
        .filter(|c| c.pat().is_some())
        .map(|c| normalise_match_clause(&c, counter))
        .collect()
}

/// Lower one whole `fun` parameter pattern, mirroring FCS's
/// `SimplePatsOfPat` (`SyntaxTreeOps.fs:383`) — the *per-arg entry
/// point*, distinct from the recursive `SimplePatOfPat` ([`classify_simple_pat`]).
/// The distinction matters: `SimplePatsOfPat` special-cases a *single*
/// `Paren(Tuple)` / `Paren(Const Unit)` layer; anything else (including
/// a *double* paren like `((x, y))` or `(())`) falls through to
/// `SimplePatOfPat`, which strips parens one at a time and scaffolds a
/// `match`. Stripping every paren up front (as a naive projector would)
/// drops that scaffold and diverges from FCS.
///
/// - `Const(Unit)` and `Paren(Const Unit)` → empty `SynSimplePats`
///   (`SyntaxTreeOps.fs:397-399`): no counter, no `match`.
/// - `Tuple` and `Paren(Tuple)` (non-struct) → lower each element via
///   `SimplePatOfPat` ([`lower_tuple`]).
/// - everything else (including `Paren(Paren(_))`, a bare non-unit
///   `Const`, `Null`, a nullary `LongIdent`, …) → [`lower_simple_pat`].
fn lower_fun_arg(pat: &NormalisedPat, counter: &mut u32, body: NormalisedExpr) -> NormalisedExpr {
    match pat {
        NormalisedPat::Const(NormalisedConst::Unit) => body,
        // Only the *non-struct* tuple is special-cased; FCS's `SimplePatsOfPat`
        // matches `SynPat.Tuple(false, …)` / `Paren(Tuple(false, …))` explicitly
        // (`SyntaxTreeOps.fs:387-389`), so a `struct (a, b)` tuple
        // (`isStruct=true`) falls through to the `match`-scaffolding
        // `SimplePatOfPat` ([`lower_simple_pat`]).
        NormalisedPat::Tuple {
            is_struct: false,
            elements,
        } => lower_tuple(elements, counter, body),
        NormalisedPat::Paren(inner) => match inner.as_ref() {
            NormalisedPat::Const(NormalisedConst::Unit) => body,
            NormalisedPat::Tuple {
                is_struct: false,
                elements,
            } => lower_tuple(elements, counter, body),
            // Double paren / paren-around-non-tuple / paren-around-struct-tuple:
            // FCS's `Paren(Tuple(false, …))` and `Paren(Const Unit)` arms don't
            // fire, so the whole arg (paren included) goes through
            // `SimplePatOfPat`, which peels the parens itself.
            _ => lower_simple_pat(pat, counter, body),
        },
        _ => lower_simple_pat(pat, counter, body),
    }
}

/// Lower the elements of a top-level tuple parameter, mirroring the
/// `Tuple` arm of FCS's `SimplePatsOfPat` (`SyntaxTreeOps.fs:387-395`).
/// FCS's `List.map (SimplePatOfPat …) ps` claims counter slots
/// left-to-right, then `List.foldBack composeFunOpt` nests the resulting
/// `match` wrappers *leftmost-outermost*. We reproduce both: phase one
/// claims slots left-to-right; phase two folds the scaffolds in reverse
/// so the leftmost element's `match` ends up outermost.
fn lower_tuple(
    elements: &[NormalisedPat],
    counter: &mut u32,
    body: NormalisedExpr,
) -> NormalisedExpr {
    let scaffolds: Vec<Option<(NormalisedExpr, NormalisedPat)>> = elements
        .iter()
        .map(|el| classify_simple_pat(el, counter))
        .collect();
    scaffolds
        .into_iter()
        .rev()
        .flatten()
        .fold(body, |acc, (scrutinee, clause_pat)| {
            wrap_match(scrutinee, clause_pat, acc)
        })
}

/// Lower one pattern via FCS's recursive `SimplePatOfPat`
/// ([`classify_simple_pat`]) and wrap `body` in the resulting `match`
/// (or return `body` unchanged when the pattern is already simple).
fn lower_simple_pat(
    pat: &NormalisedPat,
    counter: &mut u32,
    body: NormalisedExpr,
) -> NormalisedExpr {
    match classify_simple_pat(pat, counter) {
        None => body,
        Some((scrutinee, clause_pat)) => wrap_match(scrutinee, clause_pat, body),
    }
}

/// Build the synthetic `match <scrutinee> with <clause_pat> -> <body>`
/// that FCS's `SimplePatOfPat` emits for a non-simple parameter
/// (`SyntaxTreeOps.fs:357-369`).
fn wrap_match(
    scrutinee: NormalisedExpr,
    clause_pat: NormalisedPat,
    body: NormalisedExpr,
) -> NormalisedExpr {
    NormalisedExpr::Match {
        // The scrutinee is the generated `_arg<N>` (or, for the `As(_, Named id)`
        // case, the user name) — canonicalise it so the index, which FCS assigns
        // with a stateful counter we don't replicate, doesn't reach the diff.
        scrutinee: Box::new(super::canonicalise_scrutinee(scrutinee)),
        clauses: vec![NormalisedMatchClause {
            pat: clause_pat,
            when: None,
            result: Box::new(body),
        }],
    }
}

/// Classify one pattern the way FCS's recursive `SimplePatOfPat`
/// (`SyntaxTreeOps.fs:309`) does, threading the shared `_argN` counter.
/// Returns `None` when the pattern is already a `SynSimplePat` (no
/// `match` needed), or `Some((scrutinee, clause_pat))` describing the
/// synthetic `match` to wrap the body in.
///
/// - `Typed` / `Paren` are transparent: recurse into the inner pat
///   (`SyntaxTreeOps.fs:311,323`); the clause carries the *unwrapped*
///   pat at the level the catch-all fires.
/// - `Named` is already simple: `None`, and no counter is consumed
///   (`SyntaxTreeOps.fs:319`).
/// - `Wild` falls into the catch-all and *does* call `New()` (consuming
///   a slot), but its `fn` is `None` (`SyntaxTreeOps.fs:355`), so it
///   scaffolds no `match`.
/// - a *nullary single-segment* `LongIdent` (a capitalised name like
///   `X` / `None`) hits FCS's union-case arm (`SyntaxTreeOps.fs:332`):
///   it claims a slot via the alt-name cell, but the synthetic scrutinee
///   keeps the *original* name (`SynExpr.LongIdent [head]`), not `_argN`
///   (the `_argN` lives only in the elided `altNameRefCell`).
/// - everything else (a non-unit `Const`, `Null`, a `LongIdent` with
///   args or multiple segments, …) hits the generic catch-all
///   (`SyntaxTreeOps.fs:347`): it claims a slot and scrutinises the
///   compiler-generated `Ident _argN`. A multi-segment `LongIdent` head now
///   parses (dotted union-case patterns), but a bare `fun` arg is *atomic*, so
///   reaching this arm through a lambda still needs the body-lowering
///   scaffolding we don't yet emit (a known divergence); the differential
///   corpus exercises only the const/null sub-cases here.
fn classify_simple_pat(
    pat: &NormalisedPat,
    counter: &mut u32,
) -> Option<(NormalisedExpr, NormalisedPat)> {
    match pat {
        NormalisedPat::Typed { pat, .. } => classify_simple_pat(pat, counter),
        // `Attrib` is transparent to the lowering, exactly like `Typed`: FCS's
        // `SimplePatOfPat` recurses through `SynPat.Attrib(p', _)` into `p'`
        // (`SyntaxTreeOps.fs:315`), so the body-lowering decision (and the
        // synthesised clause pattern) is taken by the inner pat, with the
        // `Attrib` surviving only on the elided `SynSimplePat.Attrib`.
        NormalisedPat::Attrib { pat, .. } => classify_simple_pat(pat, counter),
        NormalisedPat::Paren(inner) => classify_simple_pat(inner, counter),
        NormalisedPat::Named(_) => None,
        // `?x` — FCS's `SimplePatOfPat` maps `SynPat.OptionalVal(v, _)` directly
        // to `SynSimplePat.Id(v, …, isOptArg=true, _)` with *no* generated match
        // (`SyntaxTreeOps.fs:321`), exactly like `Named` becomes a direct
        // parameter. So it claims no `_argN` slot and synthesises no clause
        // pattern — the lambda body rides through unchanged.
        NormalisedPat::OptionalVal(_) => None,
        // `As(_, Named id)` is FCS's special case (`SyntaxTreeOps.fs:340-341`):
        // the scrutinee is `mkSynIdGet id` (a `SynExpr.Ident`) and **no**
        // `SynArgNameGenerator.New()` slot is claimed. Any other RHS shape
        // (a capitalised `LongIdent`, etc.) falls to the generic catch-all,
        // which claims an `_argN` slot. The whole `As` rides into the clause
        // pattern unchanged in both arms (the LHS is not separately lowered).
        NormalisedPat::As { rhs, .. } => match rhs.as_ref() {
            NormalisedPat::Named(name) => Some((NormalisedExpr::Ident(name.clone()), pat.clone())),
            _ => {
                *counter += 1;
                Some((NormalisedExpr::Ident(format!("_arg{counter}")), pat.clone()))
            }
        },
        NormalisedPat::Wildcard => {
            *counter += 1;
            None
        }
        NormalisedPat::LongIdent {
            head,
            args: NormalisedArgPats::Pats(args),
            ..
        } if head.len() == 1 && args.is_empty() => {
            *counter += 1;
            Some((NormalisedExpr::LongIdent(head.clone()), pat.clone()))
        }
        _ => {
            *counter += 1;
            Some((NormalisedExpr::Ident(format!("_arg{counter}")), pat.clone()))
        }
    }
}

/// Walk an [`InterpStringExpr`]'s `parts()` and emit the FCS-shaped
/// part list: each fragment token contributes a `String` part whose
/// value is the decoded text between this fragment's opening and
/// closing delimiters, while each fill `Expr` child contributes a
/// `FillExpr` part. The single-quoted form `$"…"` maps to
/// `SynStringKind::Regular` and the triple-quoted form `$"""…"""` to
/// `SynStringKind::TripleQuote`; the style is detected from the
/// leading fragment's delimiter and shared across all fragments (FCS
/// stamps the whole `SynExpr.InterpolatedString` with one kind). The
/// verbatim `$@"…"` form rides in alongside multi-fill later.
fn normalise_interp_string(node: &InterpStringExpr, counter: &mut u32) -> NormalisedExpr {
    let raw_parts = node.parts();
    let style = raw_parts
        .first()
        .and_then(|p| match p {
            InterpStringPart::Fragment(tok) => Some(detect_interp_style(tok.text())),
            InterpStringPart::Fill { .. } => None,
        })
        .unwrap_or(InterpFragmentStyle::Single);
    let mut parts: Vec<NormalisedInterpPart> = Vec::with_capacity(raw_parts.len());
    for part in raw_parts {
        match part {
            InterpStringPart::Fragment(tok) => {
                parts.push(NormalisedInterpPart::String(decode_interp_fragment(
                    tok.text(),
                    style,
                )));
            }
            InterpStringPart::Fill { expr, qualifier } => {
                parts.push(NormalisedInterpPart::FillExpr {
                    expr: normalise_expr(&expr, counter),
                    qualifier: qualifier.map(|t| t.text().to_string()),
                });
            }
        }
    }
    NormalisedExpr::InterpolatedString {
        parts,
        kind: match style {
            InterpFragmentStyle::Single => SynStringKind::Regular,
            // Extended (`$$"""…`) is triple-like; FCS stamps `TripleQuote`.
            InterpFragmentStyle::Triple | InterpFragmentStyle::Extended { .. } => {
                SynStringKind::TripleQuote
            }
            InterpFragmentStyle::Verbatim => SynStringKind::Verbatim,
        },
    }
}

fn normalise_if_then_else(i: &IfThenElseExpr, counter: &mut u32) -> NormalisedExpr {
    // The branch accessors are keyword-relative (see `IfThenElseExpr`), so an
    // error-recovery hole in any branch surfaces as `None` and is attributed to
    // the correct slot. FCS fills each hole with `SynExpr.ArbitraryAfterError`,
    // which both sides project to `NormalisedExpr::Error` (Phase 11):
    //   * `if c then`            → then `Error`, no else
    //   * `if c then a else`     → else `Some(Error)` (keyword present)
    //   * `if c then else b`     → then `Error`, else `Some(b)`
    // Consume the shared lambda counter in source order (cond, then, else) so
    // any lambdas in the branches match FCS's left-to-right reduction. Holes
    // only occur under `allow_errors`.
    let condition = Box::new(match i.condition() {
        Some(c) => normalise_expr(&c, counter),
        None => NormalisedExpr::Error,
    });
    let then_branch = Box::new(match i.then_branch() {
        Some(t) => normalise_expr(&t, counter),
        None => NormalisedExpr::Error,
    });
    // `has_else` distinguishes the no-`else` form (→ `None`) from an `else`
    // keyword whose expression is a recovery hole (→ `Some(Error)`).
    let else_branch = i.has_else().then(|| {
        Box::new(match i.else_branch() {
            Some(e) => normalise_expr(&e, counter),
            None => NormalisedExpr::Error,
        })
    });
    NormalisedExpr::IfThenElse {
        condition,
        then_branch,
        else_branch,
    }
}

/// Project `SynExpr.Typed` to its `(expr, type)` pair. The `TYPED_EXPR`
/// node always carries one `Expr` child and one `Type` child — both must
/// be present on a well-formed tree.
fn normalise_typed(t: &TypedExpr, counter: &mut u32) -> NormalisedExpr {
    let expr = t
        .expr()
        .expect("TYPED_EXPR must contain an inner expression child");
    let ty = t
        .ty()
        .expect("TYPED_EXPR must contain a type annotation child");
    NormalisedExpr::Typed {
        expr: Box::new(normalise_expr(&expr, counter)),
        ty: normalise_type(&ty),
    }
}

/// Project `SynExpr.TypeTest` (`e :? T`) to its `(expr, type)` pair. On a
/// well-formed tree both children are present; the type is absent only on the
/// `COLON_QMARK recover` path, which the differential harness does not feed.
fn normalise_type_test(t: &TypeTestExpr, counter: &mut u32) -> NormalisedExpr {
    let expr = t
        .expr()
        .expect("TYPE_TEST_EXPR must contain an inner expression child");
    let ty = t
        .ty()
        .expect("TYPE_TEST_EXPR must contain a target-type child");
    NormalisedExpr::TypeTest {
        expr: Box::new(normalise_expr(&expr, counter)),
        ty: normalise_type(&ty),
    }
}

/// Project `SynExpr.Upcast` (`e :> T`) to its `(expr, type)` pair. See
/// [`normalise_type_test`] for the recovery caveat.
fn normalise_upcast(u: &UpcastExpr, counter: &mut u32) -> NormalisedExpr {
    let expr = u
        .expr()
        .expect("UPCAST_EXPR must contain an inner expression child");
    let ty = u
        .ty()
        .expect("UPCAST_EXPR must contain a target-type child");
    NormalisedExpr::Upcast {
        expr: Box::new(normalise_expr(&expr, counter)),
        ty: normalise_type(&ty),
    }
}

/// Project `SynExpr.Downcast` (`e :?> T`) to its `(expr, type)` pair. See
/// [`normalise_type_test`] for the recovery caveat.
fn normalise_downcast(d: &DowncastExpr, counter: &mut u32) -> NormalisedExpr {
    let expr = d
        .expr()
        .expect("DOWNCAST_EXPR must contain an inner expression child");
    let ty = d
        .ty()
        .expect("DOWNCAST_EXPR must contain a target-type child");
    NormalisedExpr::Downcast {
        expr: Box::new(normalise_expr(&expr, counter)),
        ty: normalise_type(&ty),
    }
}

/// Project a [`Type`] to its [`NormalisedType`] form. Phase 7.1–7.5
/// cover the atomic shapes, type variables, function arrows, tuple
/// types, and postfix type-applications; the panic-on-anything-else
/// default will surface as a clear test failure when a later phase
/// adds variants without updating this projector.
fn normalise_type(t: &Type) -> NormalisedType {
    match t {
        Type::LongIdent(l) => NormalisedType::LongIdent(normalise_long_ident_type(l)),
        Type::Anon(_) => NormalisedType::Anon,
        Type::Paren(p) => {
            let inner = p
                .inner()
                .expect("PAREN_TYPE must contain an inner type child");
            NormalisedType::Paren(Box::new(normalise_type(&inner)))
        }
        Type::Var(v) => normalise_var_type(v),
        Type::Fun(f) => {
            let arg = f
                .arg()
                .expect("FUN_TYPE must contain an argument-type child");
            let ret = f.ret().expect("FUN_TYPE must contain a return-type child");
            NormalisedType::Fun {
                arg: Box::new(normalise_type(&arg)),
                ret: Box::new(normalise_type(&ret)),
            }
        }
        Type::Tuple(t) => normalise_tuple_type(t),
        Type::App(a) => normalise_app_type(a),
        Type::Array(a) => {
            let element_type = a
                .element_type()
                .expect("ARRAY_TYPE must contain an element-type child");
            NormalisedType::Array {
                rank: a.rank(),
                element_type: Box::new(normalise_type(&element_type)),
            }
        }
        Type::Hash(h) => {
            let inner = h
                .inner()
                .expect("HASH_CONSTRAINT_TYPE must contain an inner type child");
            NormalisedType::Hash {
                inner: Box::new(normalise_type(&inner)),
            }
        }
        Type::AnonRecd(a) => {
            let fields = a
                .fields()
                .map(|f| {
                    let ident = f
                        .ident()
                        .expect("ANON_RECD_TYPE_FIELD must contain an IDENT_TOK child");
                    let ty = f
                        .ty()
                        .expect("ANON_RECD_TYPE_FIELD must contain a field-type child");
                    (
                        strip_backticks(ident.text()).to_string(),
                        normalise_type(&ty),
                    )
                })
                .collect::<Vec<_>>();
            NormalisedType::AnonRecd {
                is_struct: a.is_struct(),
                fields,
            }
        }
        Type::LongIdentApp(l) => normalise_long_ident_app_type(l),
        Type::WithNull(w) => {
            let inner = w
                .inner()
                .expect("WITH_NULL_TYPE must contain an inner type child");
            NormalisedType::WithNull {
                inner: Box::new(normalise_type(&inner)),
            }
        }
        Type::Constrained(c) => {
            let base = c
                .base()
                .expect("CONSTRAINED_TYPE must contain a base type child");
            // The `'a :> T` subtype shorthand: FCS folds it to
            // `WithGlobalConstraints(Var 'a, [WhereTyparSubtypeOfType('a, T)])` —
            // the constraint subject typar is shared with the base (written once
            // in source), so synthesise it from the base typar.
            if let Some(sub) = c.subtype() {
                // FCS only accepts a *typar* base for `:>`; a non-typar base is
                // invalid F# (FCS rejects it, so it never reaches a match test).
                // Synthesise the subtype constraint when the base is a typar;
                // otherwise degrade to no constraint (lossless, no panic on the
                // corpus sweep).
                let constraints = match &base {
                    Type::Var(v) => vec![NormalisedTypeConstraint::SubtypeOf {
                        typar: var_type_to_typar(v),
                        ty: normalise_type(&sub),
                    }],
                    _ => Vec::new(),
                };
                return NormalisedType::WithGlobalConstraints {
                    base: Box::new(normalise_type(&base)),
                    constraints,
                };
            }
            let constraints = c
                .constraints()
                .map(|cs| {
                    cs.constraints()
                        .map(|tc| normalise_type_constraint(&tc))
                        .collect()
                })
                .unwrap_or_default();
            NormalisedType::WithGlobalConstraints {
                base: Box::new(normalise_type(&base)),
                constraints,
            }
        }
        Type::Intersection(i) => {
            // The head typar (`Some` for `'T & …`) lives in the dedicated slot;
            // `types()` already excludes it. A hash-head form (`#A & …`) reports
            // `typar = None` and keeps `#A` as the first `types` element.
            let typar = i.typar().map(|v| {
                let ident = v
                    .ident()
                    .expect("intersection head VAR_TYPE must contain an IDENT_TOK child");
                NormalisedTypar {
                    name: strip_backticks(ident.text()).to_string(),
                    head_type: v.is_head_type(),
                    // The intersection head is a bare `SynType.Var`, not a
                    // `SynTyparDecl`, so it carries no attributes and no
                    // (declaration-level) intersection constraints.
                    attributes: Vec::new(),
                    intersection_constraints: Vec::new(),
                }
            });
            let types = i.types().map(|t| normalise_type(&t)).collect();
            NormalisedType::Intersection { typar, types }
        }
        Type::MeasurePower(m) => normalise_measure_power_type(m),
        Type::StaticConst(s) => {
            let lit = s
                .literal()
                .expect("STATIC_CONST_TYPE must contain a literal token");
            NormalisedType::StaticConstant(normalise_const_lit(&lit))
        }
        Type::StaticConstExpr(s) => {
            let expr = s
                .expr()
                .expect("STATIC_CONST_EXPR_TYPE must contain an inner expression");
            // A const-expr type argument is an `atomicExpr`, which cannot
            // itself be a lambda, so it never advances FCS's file-global
            // `SynArgNameGenerator`; a fresh counter is sufficient. (A lambda
            // reachable only via a parenthesised atomic — `const (fun x -> x)`
            // — is far outside the phase-10.9 surface and not pinned.)
            let mut counter = 0;
            NormalisedType::StaticConstantExpr(Box::new(normalise_expr(&expr, &mut counter)))
        }
        Type::StaticConstNamed(s) => {
            let ident = s
                .ident()
                .expect("STATIC_CONST_NAMED_TYPE must contain a name type");
            let value = s
                .value()
                .expect("STATIC_CONST_NAMED_TYPE must contain a value type");
            NormalisedType::StaticConstantNamed {
                ident: Box::new(normalise_type(&ident)),
                value: Box::new(normalise_type(&value)),
            }
        }
        Type::StaticConstNull(_) => NormalisedType::StaticConstantNull,
        Type::SignatureParameter(p) => {
            let used = p
                .value_type()
                .expect("SIGNATURE_PARAMETER_TYPE must contain a value type child");
            let id = p.name().map(|t| strip_backticks(t.text()).to_string());
            // Leading `[<A>] …` attribute lists — FCS's
            // `SynType.SignatureParameter.attributes`. Local `_argN` counter, like
            // every other attribute home.
            let mut counter = 0u32;
            let attributes = normalise_attribute_lists(p.attributes(), &mut counter);
            NormalisedType::SignatureParameter {
                attributes,
                is_optional: p.is_optional(),
                id,
                used_type: Box::new(normalise_type(&used)),
            }
        }
    }
}

/// Project a `SynType.MeasurePower` to `NormalisedType::MeasurePower`
/// (phase 10.8). The base is the sole inner `Type`; the exponent is the
/// `SynRationalConst` child. The `^-` operator spelling contributes a
/// `Negate` wrapper that has no green node of its own (the parser records
/// it only as the operator token), so it is reconstructed here from
/// [`MeasurePowerType::is_negated`].
fn normalise_measure_power_type(m: &MeasurePowerType) -> NormalisedType {
    let base = m
        .base()
        .expect("MEASURE_POWER_TYPE must contain a base-type child");
    let exponent = m
        .exponent()
        .expect("MEASURE_POWER_TYPE must contain a rational-const exponent child");
    let exponent = normalise_rational_const(&exponent);
    let exponent = if m.is_negated() {
        NormalisedRationalConst::Negate(Box::new(exponent))
    } else {
        exponent
    };
    NormalisedType::MeasurePower {
        base: Box::new(normalise_type(&base)),
        exponent,
    }
}

/// Project a `SynRationalConst` green node to [`NormalisedRationalConst`].
/// Integer / numerator / denominator literal texts are decoded the same way
/// as an `INT32_LIT` const (sign-fold-aware via [`split_num_sign`]).
fn normalise_rational_const(r: &RationalConst) -> NormalisedRationalConst {
    match r {
        RationalConst::Integer(i) => {
            let tok = i
                .value_token()
                .expect("RATIONAL_CONST_INTEGER must contain an INT32_LIT child");
            NormalisedRationalConst::Integer(decode_rational_int(tok.text()))
        }
        RationalConst::Rational(r) => {
            let num = r
                .numerator()
                .expect("RATIONAL_CONST_RATIONAL must contain a numerator INT32_LIT");
            let denom = r
                .denominator()
                .expect("RATIONAL_CONST_RATIONAL must contain a denominator INT32_LIT");
            NormalisedRationalConst::Rational {
                num: decode_rational_int(num.text()),
                denom: decode_rational_int(denom.text()),
            }
        }
        RationalConst::Negate(n) => {
            let inner = n
                .inner()
                .expect("RATIONAL_CONST_NEGATE must contain an inner rational-const child");
            NormalisedRationalConst::Negate(Box::new(normalise_rational_const(&inner)))
        }
        RationalConst::Paren(p) => {
            let inner = p
                .inner()
                .expect("RATIONAL_CONST_PAREN must contain an inner rational-const child");
            NormalisedRationalConst::Paren(Box::new(normalise_rational_const(&inner)))
        }
    }
}

/// Decode a measure-exponent `INT32_LIT` token text to `i32`, honouring a
/// [`sign_fold`](borzoi_cst::parser)-merged `-`. Mirrors the
/// `INT32_LIT` arm of [`normalise_const_lit`].
fn decode_rational_int(text: &str) -> i32 {
    let (minus, body) = split_num_sign(text);
    let v = decode_int_body(body) as u32 as i32;
    if minus { v.wrapping_neg() } else { v }
}

/// Project a `SynType.App` to `NormalisedType::App`. Phases 7.5 +
/// 7.6 together: postfix `int list` (no `<…>` token children) and
/// prefix `Foo<int>` (with `LESS_TOK` / `GREATER_TOK` plus interior
/// `COMMA_TOK` separators); `AppType::is_postfix` discriminates on
/// the `LESS_TOK` presence. The token children are filtered out of
/// the head/args projection by going through the typed `Type` cast
/// (the facade accessors do this already).
fn normalise_app_type(a: &AppType) -> NormalisedType {
    let type_name = a
        .type_name()
        .expect("APP_TYPE must contain a head-type child");
    let type_args = a.type_args().iter().map(normalise_type).collect::<Vec<_>>();
    NormalisedType::App {
        type_name: Box::new(normalise_type(&type_name)),
        type_args,
        is_postfix: a.is_postfix(),
    }
}

/// Project a `SynType.LongIdentApp` to `NormalisedType::LongIdentApp`.
/// Phase 7.10: the `atomType DOT path [<…>]` shape whose LHS is itself
/// a non-`path` atomic type. `root` is the first `Type` child captured
/// by the parser's left-associative dot-chain loop, `path` is the
/// post-dot ident sequence (read from the sole `LONG_IDENT` child,
/// backticks stripped), and `type_args` is the optional `<…>` block.
fn normalise_long_ident_app_type(l: &LongIdentAppType) -> NormalisedType {
    let root = l
        .root()
        .expect("LONG_IDENT_APP_TYPE must contain a root-type child");
    let path = l
        .path()
        .expect("LONG_IDENT_APP_TYPE must contain a LONG_IDENT child")
        .idents()
        .map(|tok| strip_backticks(tok.text()).to_string())
        .collect::<Vec<_>>();
    let type_args = l.type_args().iter().map(normalise_type).collect::<Vec<_>>();
    NormalisedType::LongIdentApp {
        root: Box::new(normalise_type(&root)),
        path,
        type_args,
    }
}

/// Project a `SynType.Tuple` to a flat `Vec<NormalisedTupleSegment>`. The
/// `struct (T * U)` form sets `is_struct` (FCS's `SynType.Tuple.isStruct`),
/// read off the `TUPLE_TYPE`'s leading `STRUCT_TOK`; the `struct`/parens tokens
/// are not segments, so `segments()` yields the same flat `path` either way.
fn normalise_tuple_type(t: &TupleType) -> NormalisedType {
    let path = t
        .segments()
        .into_iter()
        .map(|s| match s {
            TupleSegment::Type(ty) => NormalisedTupleSegment::Type(normalise_type(&ty)),
            TupleSegment::Star(_) => NormalisedTupleSegment::Star,
            TupleSegment::Slash(_) => NormalisedTupleSegment::Slash,
        })
        .collect();
    NormalisedType::Tuple {
        is_struct: t.is_struct(),
        path,
    }
}

/// Project a `SynType.Var` to its `(name, head_type)` pair. The
/// `IDENT_TOK` child carries the typar name (backticks stripped); the
/// sigil-token kind picks `TyparStaticReq.None` vs `HeadType`.
fn normalise_var_type(v: &VarType) -> NormalisedType {
    let ident = v.ident().expect("VAR_TYPE must contain an IDENT_TOK child");
    NormalisedType::Var {
        name: strip_backticks(ident.text()).to_string(),
        head_type: v.is_head_type(),
    }
}

/// Project a `SynType.LongIdent` to its sequence of `Ident.idText`
/// strings. Sibling of [`normalise_long_ident`] for expressions; both
/// strip backticks the same way so the FCS-side `idText` comparison
/// holds.
fn normalise_long_ident_type(l: &LongIdentType) -> Vec<String> {
    let inner = l
        .long_ident()
        .expect("LONG_IDENT_TYPE must contain a LONG_IDENT child");
    inner
        .idents()
        .map(|tok| strip_backticks(tok.text()).to_string())
        .collect()
}

fn normalise_address_of(a: &AddressOfExpr, counter: &mut u32) -> NormalisedExpr {
    let inner = a
        .expr()
        .expect("ADDRESS_OF_EXPR must contain an inner expr");
    NormalisedExpr::AddressOf {
        is_byref: a.is_byref(),
        expr: Box::new(normalise_expr(&inner, counter)),
    }
}

/// Project a surface `NEW_EXPR` to [`NormalisedExpr::New`] — FCS's
/// `SynExpr.New(isProtected, targetType, expr, range)`. The expression form
/// always yields `is_protected = false` (the `true` case is `inherit`-style
/// base construction, which we don't emit here). The target type is projected
/// before the argument, matching FCS's source order for the shared lambda
/// counter.
fn normalise_new(n: &NewExpr, counter: &mut u32) -> NormalisedExpr {
    let ty = n
        .target_type()
        .expect("NEW_EXPR must contain a target-type child");
    let arg = n
        .arg()
        .expect("NEW_EXPR must contain an argument expr child");
    NormalisedExpr::New {
        is_protected: false,
        ty: normalise_type(&ty),
        arg: Box::new(normalise_expr(&arg, counter)),
    }
}

/// Project a surface `OBJ_EXPR` to [`NormalisedExpr::ObjExpr`] — FCS's
/// `SynExpr.ObjExpr(objType, argOptions, …, members, extraImpls, …)`. The object
/// type and optional constructor argument are read through the leading
/// `NEW_EXPR` carrier ([`ObjExpr::obj_type`] / [`ObjExpr::arg`]); the
/// `with member …` block is the `MEMBER_DEFN`/`GET_SET_MEMBER` children; the
/// `extraImpls` are the trailing `INTERFACE_IMPL` children. The type comes first
/// (source order for the shared lambda counter), then the argument, then the
/// members and extra interfaces (each of which uses its own fresh `_argN`
/// counter, like a type-definition member).
fn normalise_obj_expr(o: &ObjExpr, counter: &mut u32) -> NormalisedExpr {
    let ty = o
        .obj_type()
        .expect("OBJ_EXPR must carry an object type in its NEW_EXPR child");
    let nty = normalise_type(&ty);
    let arg = o.arg().map(|a| Box::new(normalise_expr(&a, counter)));
    // The value-binding form (`{ new T() with X = e [and …] }`, FCS's
    // `bindings` slot) — the `BINDING` children. The head binding carries
    // `SynLeadingKeyword.Synthetic` (the shared `with` is not a per-binding
    // keyword); every `and`-chained binding is `And` — the same head/tail rule
    // a `let … and …` group uses. Projected after the constructor argument and
    // before the members (FCS field order) so the shared `_argN` lambda counter
    // visits in source order.
    let bindings = o
        .bindings()
        .enumerate()
        .map(|(i, b)| {
            let lk = if i == 0 {
                NormalisedLeadingKeyword::Synthetic
            } else {
                NormalisedLeadingKeyword::And
            };
            normalise_binding(&b, lk, counter)
        })
        .collect();
    let members = o.members().map(|m| normalise_member(&m)).collect();
    // Extra interface implementations (`extraImpls`) — each `INTERFACE_IMPL`
    // child, yielded as a `MemberDefn::Interface`, reuses the shared member
    // projection (a `NormalisedMember::Interface`).
    let extra_impls = o.extra_impls().map(|m| normalise_member(&m)).collect();
    NormalisedExpr::ObjExpr {
        ty: nty,
        arg,
        bindings,
        members,
        extra_impls,
    }
}

/// Project a `TYPE_APP_EXPR` to [`NormalisedExpr::TypeApp`] — FCS's
/// `SynExpr.TypeApp(expr, lessRange, typeArgs, …)`. The head expression is
/// projected before the type arguments (matching FCS's source order for the
/// shared lambda counter); the type arguments are counter-free (types never
/// contain lambdas).
fn normalise_type_app(t: &TypeAppExpr, counter: &mut u32) -> NormalisedExpr {
    let head = t
        .expr()
        .expect("TYPE_APP_EXPR must contain a head expr child");
    let expr = Box::new(normalise_expr(&head, counter));
    let type_args = t.type_args().iter().map(normalise_type).collect::<Vec<_>>();
    NormalisedExpr::TypeApp { expr, type_args }
}

/// Project `SynExpr.App` to its `funcExpr` / `argExpr` pair. The
/// whitespace-separated prefix form `f x` is `NonAtomic` (`is_atomic:
/// false`); the adjacent form `f(x)` is `Atomic` — recovered via
/// [`AppExpr::is_atomic`], which reads the
/// [`SyntaxKind::HIGH_PRECEDENCE_PAREN_APP_TOK`] marker the parser stamps
/// inside an atomic application. The infix form (the inner App produced by
/// `mkSynInfix`) lives under [`SyntaxKind::INFIX_APP_EXPR`], so `is_infix`
/// reads back via [`AppExpr::is_infix`] — which inspects the node's kind
/// rather than carrying a separate flag.
fn normalise_app(a: &AppExpr, counter: &mut u32) -> NormalisedExpr {
    let func = a.func().expect("APP_EXPR must have a func child");
    let arg = a.arg().expect("APP_EXPR must have an arg child");
    // Struct fields evaluate in source order, so `func` consumes the shared
    // lambda counter before `arg` — matching FCS's left-to-right reduction.
    NormalisedExpr::App {
        is_atomic: a.is_atomic(),
        is_infix: a.is_infix(),
        func: Box::new(normalise_expr(&func, counter)),
        arg: Box::new(normalise_expr(&arg, counter)),
    }
}

/// Project our `CONS_EXPR` (`a :: b`) to FCS's lowered shape. FCS's
/// `declExpr COLON_COLON declExpr` (`pars.fsy:4765`) does *not* use the two-tier
/// `mkSynInfix` form: it builds a single `App(NonAtomic, isInfix = true,
/// op_ColonColon, Tuple(false, [lhs; rhs]))` — the cons operator applied to a
/// synthesised pair. The operator long-ident has `idText = "op_ColonColon"` and
/// `IdentTrivia.OriginalNotation "::"`, which the FCS-side normaliser unwraps to
/// `LongIdent(["::"])`; we project the operator directly to that here. The
/// synthesised tuple's lone comma range (`[mOp]`) and `isStruct = false` are
/// elided / fixed, matching the FCS-side `Tuple` projection.
///
/// Sub-expressions are projected in source order (lhs before rhs) so the shared
/// `_argN` lambda counter advances left-to-right; the operator long-ident is
/// lambda-free and consumes no counter.
fn normalise_cons(c: &ConsExpr, counter: &mut u32) -> NormalisedExpr {
    let lhs = c.lhs().expect("CONS_EXPR must have a head expr");
    let rhs = c.rhs().expect("CONS_EXPR must have a tail expr");
    NormalisedExpr::App {
        is_atomic: false,
        is_infix: true,
        func: Box::new(NormalisedExpr::LongIdent(vec!["::".to_string()])),
        arg: Box::new(NormalisedExpr::Tuple {
            is_struct: false,
            elements: vec![normalise_expr(&lhs, counter), normalise_expr(&rhs, counter)],
        }),
    }
}

/// Project a `JOIN_IN_EXPR` (`SynExpr.JoinIn`) to its two operands. The node's
/// children are `[<lhs-expr>, IN_TOK, <rhs-expr>]`; both operands are ordinary
/// expressions projected in source order (so the shared `_argN` lambda counter
/// threads left-to-right).
fn normalise_join_in(j: &JoinInExpr, counter: &mut u32) -> NormalisedExpr {
    let lhs = j.lhs().expect("JOIN_IN_EXPR must have a left operand");
    let rhs = j.rhs().expect("JOIN_IN_EXPR must have a right operand");
    NormalisedExpr::JoinIn {
        lhs: Box::new(normalise_expr(&lhs, counter)),
        rhs: Box::new(normalise_expr(&rhs, counter)),
    }
}

/// Replay FCS's `mkSynAssign` (`SyntaxTreeOps.fs:518`) on our `ASSIGN_EXPR`:
/// the concrete `SynExpr.*Set` variant is a projection of the LHS *shape*,
/// not a parse-time decision. The LHS shapes our parser can build:
/// - identifier path (`Ident`/`LongIdent`, the `LongOrSingleIdent` arm) →
///   [`NormalisedExpr::LongIdentSet`];
/// - application with a `LongIdent` function (`Type.Items(i) <- e`) →
///   [`NormalisedExpr::NamedIndexedPropertySet`]; a *single-ident* function is
///   `SynExpr.Ident`, not `LongIdent`, so it falls through to `Set`;
/// - anything else (the `mkSynAssign` fallback) → [`NormalisedExpr::Set`].
///
/// The `DotGet` → [`NormalisedExpr::DotSet`] and `DotIndexedGet` →
/// [`NormalisedExpr::DotIndexedSet`] arms (`SyntaxTreeOps.fs:525-526`) became
/// reachable with phase 10.16a's postfix `.member` / `arr.[i]` parsing — a
/// pure ident chain `a.b <- e` is still `LongIdentSet` (its LHS is a
/// `LongIdent`, not a `DotGet`). `DotNamedIndexedPropertySet` still can't arise
/// (needs `expr.Member(i) <- e`). Sub-expressions are projected in source order
/// (object/index/target before value) so the shared `_argN` lambda counter
/// advances left-to-right; an identifier-path target is lambda-free and
/// consumes no counter.
fn normalise_assign(a: &AssignExpr, counter: &mut u32) -> NormalisedExpr {
    let target = a
        .target()
        .expect("ASSIGN_EXPR must have a target Expr child");
    let value = a.value().expect("ASSIGN_EXPR must have a value Expr child");
    match &target {
        Expr::Ident(i) => NormalisedExpr::LongIdentSet {
            long_dot_id: vec![normalise_ident(i)],
            value: Box::new(normalise_expr(&value, counter)),
        },
        Expr::LongIdent(l) => NormalisedExpr::LongIdentSet {
            long_dot_id: normalise_long_ident(l),
            value: Box::new(normalise_expr(&value, counter)),
        },
        Expr::DotIndexedGet(d) => {
            // `DotIndexedGet(obj, index) <- value` → `DotIndexedSet`. Project
            // object then index (source order) before the value.
            let object = d
                .object()
                .expect("DOT_INDEXED_GET_EXPR must contain an object expr");
            let index = d
                .index()
                .expect("DOT_INDEXED_GET_EXPR must contain an index expr");
            NormalisedExpr::DotIndexedSet {
                object: Box::new(normalise_expr(&object, counter)),
                index: Box::new(normalise_expr(&index, counter)),
                value: Box::new(normalise_expr(&value, counter)),
            }
        }
        Expr::DotGet(d) => {
            // `DotGet(obj, longDotId) <- value` → `DotSet`. Project the object
            // before the value; the member path carries no sub-expressions.
            let object = d.expr().expect("DOT_GET_EXPR must contain an inner expr");
            let inner = d
                .long_ident()
                .expect("DOT_GET_EXPR must contain a LONG_IDENT child");
            let long_dot_id = inner
                .idents()
                .map(|tok| strip_backticks(tok.text()).to_string())
                .collect();
            NormalisedExpr::DotSet {
                expr: Box::new(normalise_expr(&object, counter)),
                long_dot_id,
                value: Box::new(normalise_expr(&value, counter)),
            }
        }
        Expr::LibraryOnlyFieldGet(g) => {
            // `LibraryOnlyUnionCaseFieldGet(obj, fieldNum) <- value` →
            // `LibraryOnlyUnionCaseFieldSet` (FCS's `mkSynAssign`,
            // `SyntaxTreeOps.fs:526`). Object then value, source order.
            let object = g
                .object()
                .expect("LIBRARY_ONLY_FIELD_GET_EXPR must contain an object expr");
            NormalisedExpr::LibraryOnlyUnionCaseFieldSet {
                expr: Box::new(normalise_expr(&object, counter)),
                field_num: g
                    .field_num()
                    .expect("LIBRARY_ONLY_FIELD_GET_EXPR must contain a field number"),
                value: Box::new(normalise_expr(&value, counter)),
            }
        }
        Expr::App(app) if !app.is_infix() && matches!(app.func(), Some(Expr::LongIdent(_))) => {
            // `App(_, _, LongIdent v, x, _) -> NamedIndexedPropertySet(v, x, r)`
            // (`SyntaxTreeOps.fs:529`). The application's atomic flag is dropped
            // by `mkSynAssign`, so `Foo.Bar(3)` and `Foo.Bar 3` agree here.
            let Some(Expr::LongIdent(lid)) = app.func() else {
                unreachable!("guarded by the `matches!` above")
            };
            let arg = app.arg().expect("APP_EXPR must have an arg child");
            NormalisedExpr::NamedIndexedPropertySet {
                long_dot_id: normalise_long_ident(&lid),
                expr1: Box::new(normalise_expr(&arg, counter)),
                expr2: Box::new(normalise_expr(&value, counter)),
            }
        }
        Expr::App(app) if !app.is_infix() && matches!(app.func(), Some(Expr::DotGet(_))) => {
            // `App(_, _, DotGet(e, _, v, _), x, _) ->
            //  DotNamedIndexedPropertySet(e, v, x, r)` (`SyntaxTreeOps.fs:530`)
            // — `(obj).P(i) <- v`, reachable since phase 10.16a's `DotGet` tail.
            // A `LongIdent`-function application (ident receiver, `obj.P(i)`) is
            // the `NamedIndexedPropertySet` arm above. Project receiver object →
            // index arg → value (source order) for the shared lambda counter.
            let Some(Expr::DotGet(dg)) = app.func() else {
                unreachable!("guarded by the `matches!` above")
            };
            let object = dg.expr().expect("DOT_GET_EXPR must contain an inner expr");
            let inner = dg
                .long_ident()
                .expect("DOT_GET_EXPR must contain a LONG_IDENT child");
            let long_dot_id = inner
                .idents()
                .map(|tok| strip_backticks(tok.text()).to_string())
                .collect();
            let arg = app.arg().expect("APP_EXPR must have an arg child");
            NormalisedExpr::DotNamedIndexedPropertySet {
                target: Box::new(normalise_expr(&object, counter)),
                long_dot_id,
                expr1: Box::new(normalise_expr(&arg, counter)),
                expr2: Box::new(normalise_expr(&value, counter)),
            }
        }
        _ => {
            let target = normalise_expr(&target, counter);
            NormalisedExpr::Set {
                target: Box::new(target),
                value: Box::new(normalise_expr(&value, counter)),
            }
        }
    }
}

/// Project a `RECORD_FIELD` list (shared by `SynExpr.Record` and
/// `SynExpr.AnonRecd`): each field's `SynLongIdent` name segments (backticks
/// stripped) and value expression, in source order so the shared `_argN`
/// lambda counter advances left-to-right.
fn normalise_record_fields(
    fields: impl Iterator<Item = RecordField>,
    counter: &mut u32,
) -> Vec<NormalisedRecordField> {
    fields
        .map(|f| {
            let name = f
                .field_name()
                .map(|li| {
                    li.idents()
                        .map(|tok| strip_backticks(tok.text()).to_string())
                        .collect()
                })
                .unwrap_or_default();
            let value = f.value().map(|v| Box::new(normalise_expr(&v, counter)));
            NormalisedRecordField { name, value }
        })
        .collect()
}

/// Project `SynExpr.DotGet` (postfix `expr.Member`, phase 10.16a): the LHS
/// expression plus the member path's `Ident.idText` segments (backticks
/// stripped, like [`normalise_long_ident`]).
fn normalise_dot_get(d: &DotGetExpr, counter: &mut u32) -> NormalisedExpr {
    let expr = d.expr().expect("DOT_GET_EXPR must contain an inner expr");
    let inner = d
        .long_ident()
        .expect("DOT_GET_EXPR must contain a LONG_IDENT child");
    let long_dot_id = long_ident_segment_texts(&inner);
    NormalisedExpr::DotGet {
        expr: Box::new(normalise_expr(&expr, counter)),
        long_dot_id,
    }
}

/// Project `SynExpr.Dynamic` (`a?b`): the LHS `funcExpr` and the `argExpr`
/// (an `Ident` member name or a `Paren`). The LHS consumes the shared lambda
/// counter before the argument, matching source order.
fn normalise_dynamic(d: &DynamicExpr, counter: &mut u32) -> NormalisedExpr {
    let lhs = d.lhs().expect("DYNAMIC_EXPR must contain an LHS expr");
    let arg = d.arg().expect("DYNAMIC_EXPR must contain an argument expr");
    NormalisedExpr::Dynamic {
        lhs: Box::new(normalise_expr(&lhs, counter)),
        arg: Box::new(normalise_expr(&arg, counter)),
    }
}

/// Project `SynExpr.DotIndexedGet` (`expr.[index]`, phase 10.16a): the
/// indexed object and the (single) index expression. The object consumes
/// the shared lambda counter before the index, matching source order.
fn normalise_dot_indexed_get(d: &DotIndexedGetExpr, counter: &mut u32) -> NormalisedExpr {
    let object = d
        .object()
        .expect("DOT_INDEXED_GET_EXPR must contain an object expr");
    let index = d
        .index()
        .expect("DOT_INDEXED_GET_EXPR must contain an index expr");
    NormalisedExpr::DotIndexedGet {
        object: Box::new(normalise_expr(&object, counter)),
        index: Box::new(normalise_expr(&index, counter)),
    }
}

/// Project `SynExpr.LibraryOnlyUnionCaseFieldGet` (`expr.( :: ).<int>`, the
/// FSharp.Core cons-cell field read): the object and the field number. The
/// union-case name is always `op_ColonColon` (grammar-fixed), so it is elided.
fn normalise_library_only_field_get(
    g: &LibraryOnlyFieldGetExpr,
    counter: &mut u32,
) -> NormalisedExpr {
    let object = g
        .object()
        .expect("LIBRARY_ONLY_FIELD_GET_EXPR must contain an object expr");
    NormalisedExpr::LibraryOnlyUnionCaseFieldGet {
        expr: Box::new(normalise_expr(&object, counter)),
        field_num: g
            .field_num()
            .expect("LIBRARY_ONLY_FIELD_GET_EXPR must contain a field number"),
    }
}

/// Project `SynExpr.IndexRange` (`lower..upper`, phase 10.22): the two
/// optional bounds, lower before upper so the shared lambda counter advances
/// in source order. An absent bound (open range, `2..` / `..3`) projects to
/// `None`.
fn normalise_index_range(r: &IndexRangeExpr, counter: &mut u32) -> NormalisedExpr {
    let lower = r.lower().map(|e| Box::new(normalise_expr(&e, counter)));
    let upper = r.upper().map(|e| Box::new(normalise_expr(&e, counter)));
    NormalisedExpr::IndexRange { lower, upper }
}

/// Project `SynExpr.Tuple` to its element list. Our parser doesn't yet
/// produce struct tuples; when it does, this projector will need to
/// distinguish them via a marker token child.
fn normalise_tuple(t: &TupleExpr, counter: &mut u32) -> NormalisedExpr {
    NormalisedExpr::Tuple {
        is_struct: t.is_struct(),
        elements: t.elements().map(|e| normalise_expr(&e, counter)).collect(),
    }
}

/// Project `SynExpr.Paren` to its inner expression by recursing into the
/// `PAREN_EXPR > <inner-expr>` child. The wrapping `Paren` node is
/// preserved in the normalised representation so it diffs against FCS's
/// `SynExpr.Paren` rather than collapsing through.
fn normalise_paren(p: &ParenExpr, counter: &mut u32) -> NormalisedExpr {
    let inner = p
        .inner()
        .expect("PAREN_EXPR must contain an inner expression child");
    normalise_expr(&inner, counter)
}

/// Project `SynExpr.TraitCall` — the SRTP trait call
/// `( ^a : (static member M : sig) arg )`. The support type and member sig are
/// projected through the shared [`normalise_type`] / [`normalise_member_sig`]
/// (the member sig is the same `classMemberSpfn` payload as the SRTP member
/// constraint). The argument expression advances the shared `_argN` counter
/// last, in source order (after the member sig, which carries no expressions).
fn normalise_trait_call(t: &TraitCallExpr, counter: &mut u32) -> NormalisedExpr {
    // The support is the head-type list — one `^a`, or the `or`-separated
    // alternatives of `((^a or int) : …)`, whose first operand is a typar and
    // whose later ones are arbitrary types (FCS's `typarAlts`). Each operand is a
    // `Type`, projected via the shared `normalise_type`, matching FCS's flattened
    // `fcs_support_types`.
    let support = t.support_types().map(|ty| normalise_type(&ty)).collect();
    let member = Box::new(normalise_member_sig(
        &t.member_sig()
            .expect("TRAIT_CALL_EXPR must contain a MEMBER_SIG child"),
    ));
    let arg = Box::new(normalise_expr(
        &t.arg()
            .expect("TRAIT_CALL_EXPR must contain an argument expression child"),
        counter,
    ));
    NormalisedExpr::TraitCall {
        support,
        member,
        arg,
    }
}

/// `STATIC_OPTIMIZATION_EXPR` → the nested [`NormalisedExpr::StaticOptimization`]
/// fold FCS builds in `SyntaxTreeOps.mkSynBindingRhs`. The flat CST shape is
/// `[<main-expr>, STATIC_OPT_WHEN_CLAUSE+]`; FCS's `List.foldBack` wraps each
/// clause around the fallthrough so the *first* clause is outermost.
///
/// The synthetic-`fun`-arg `counter` is consumed in **source order** to match
/// FCS's left-to-right `_argN` assignment (see [`normalise_binding`]): the main
/// expression parses before the `when` clauses, so it is normalised first, then
/// each clause's branch in forward order. The right-associative nest is then
/// built separately (folding the already-normalised clauses in reverse), so the
/// structure has the first clause outermost without disturbing the counter order.
fn normalise_static_optimization(s: &StaticOptimizationExpr, counter: &mut u32) -> NormalisedExpr {
    let main = normalise_expr(
        &s.main_expr()
            .expect("STATIC_OPTIMIZATION_EXPR must contain a main (fallthrough) expression"),
        counter,
    );
    let clauses: Vec<(Vec<NormalisedStaticOptConstraint>, NormalisedExpr)> = s
        .clauses()
        .map(|clause| {
            let constraints = clause
                .conditions()
                .map(|c| normalise_static_opt_condition(&c))
                .collect();
            let branch = normalise_expr(
                &clause
                    .branch()
                    .expect("STATIC_OPT_WHEN_CLAUSE must contain a branch expression"),
                counter,
            );
            (constraints, branch)
        })
        .collect();
    clauses
        .into_iter()
        .rev()
        .fold(main, |acc, (constraints, expr)| {
            NormalisedExpr::StaticOptimization {
                constraints,
                expr: Box::new(expr),
                optimized_expr: Box::new(acc),
            }
        })
}

/// One `STATIC_OPT_CONDITION` → its [`NormalisedStaticOptConstraint`]: the bare
/// `'T struct` form ([`StaticOptCondition::is_struct`]) is `WhenTyparIsStruct`,
/// else `'T : ty` is `WhenTyparTyconEqualsTycon`.
fn normalise_static_opt_condition(c: &StaticOptCondition) -> NormalisedStaticOptConstraint {
    let typar = normalise_typar(
        &c.typar()
            .expect("STATIC_OPT_CONDITION must contain a subject typar"),
    );
    if c.is_struct() {
        NormalisedStaticOptConstraint::WhenTyparIsStruct { typar }
    } else {
        NormalisedStaticOptConstraint::WhenTyparTyconEqualsTycon {
            typar,
            rhs_type: normalise_type(
                &c.ty()
                    .expect("a `'T : ty` STATIC_OPT_CONDITION must contain a type"),
            ),
        }
    }
}

/// Project a `SynExpr.LongIdent` to its sequence of `Ident.idText` strings,
/// matching FCS's representation. Backticks come off here so the diff
/// against `fcs-dump ast` doesn't need to know about source-form vs idText.
fn normalise_long_ident(l: &LongIdentExpr) -> Vec<String> {
    let inner = l
        .long_ident()
        .expect("LONG_IDENT_EXPR must contain a LONG_IDENT child");
    long_ident_segment_texts(&inner)
}

/// Project a `LONG_IDENT` node to its ordered segment texts — FCS's
/// `SynLongIdent` ident list. A segment is an `IDENT_TOK`/`NEW_TOK` (a plain
/// ident, an operator-value's mangled token, or the `new` ctor keyword) *or* an
/// `ACTIVE_PAT_NAME` node — the active-pattern-name `opName` segment
/// (`(|Foo|_|)`), which FCS folds into a single ident (`"|Foo|_|"`). The
/// `idents()` token projection can't see the latter (it's a node), so walk the
/// children in order, rebuilding the name's `idText` from its case tokens.
/// `DOT_TOK` / paren / bar separators and trivia are skipped. Shared by the
/// `LONG_IDENT_EXPR` and `DOT_GET_EXPR` projections, both of which carry such a
/// `LONG_IDENT` (and a `DotGet` member list can be an active-pattern `opName`
/// off a non-ident head — `(id 1).(|Bar|_|)`).
fn long_ident_segment_texts(inner: &LongIdent) -> Vec<String> {
    inner
        .syntax()
        .children_with_tokens()
        .filter_map(|el| {
            if let Some(tok) = el.as_token() {
                matches!(tok.kind(), SyntaxKind::IDENT_TOK | SyntaxKind::NEW_TOK)
                    .then(|| strip_backticks(tok.text()).to_string())
            } else if let Some(node) = el.as_node() {
                (node.kind() == SyntaxKind::ACTIVE_PAT_NAME)
                    .then(|| active_pat_id_text(&ActivePatName::cast(node.clone()).unwrap()))
            } else {
                None
            }
        })
        .collect()
}

/// FCS's `Ident.idText` for a one-segment ident: strips the surrounding
/// double-backticks from a `` ``foo bar`` `` quoted ident, leaves plain
/// idents alone.
fn normalise_ident(i: &IdentExpr) -> String {
    let tok = i.ident().expect("IDENT_EXPR must contain an IDENT_TOK");
    strip_backticks(tok.text()).to_string()
}

fn normalise_const(c: &ConstExpr) -> NormalisedConst {
    let lit = c
        .literal()
        .expect("CONST_EXPR must contain a literal token");
    normalise_const_lit(&lit)
}

/// Project a `MEASURE_LIT_EXPR` to `NormalisedConst::Measure` — FCS's
/// `SynConst.Measure(constant, _, synMeasure, _)`. The constant is the inner
/// `CONST_EXPR`; the measure is the `<…>` annotation.
fn normalise_measure_lit(m: &MeasureLitExpr) -> NormalisedConst {
    let constant = m
        .const_expr()
        .expect("MEASURE_LIT_EXPR must contain a CONST_EXPR child");
    let measure = m
        .measure()
        .expect("MEASURE_LIT_EXPR must contain a measure child");
    NormalisedConst::Measure {
        constant: Box::new(normalise_const(&constant)),
        measure: normalise_measure(&measure),
    }
}

/// Project a [`Measure`] green node to [`NormalisedMeasure`], mirroring FCS's
/// `SynMeasure` (`measureTypeExpr`). The `^-` power spelling contributes a
/// `Negate` wrapper with no green node of its own (recorded only as the
/// operator token), reconstructed via [`MeasurePower::is_negated`] — exactly as
/// [`normalise_measure_power_type`] does for the type-side power.
fn normalise_measure(m: &Measure) -> NormalisedMeasure {
    match m {
        Measure::Named(n) => {
            let segments = n
                .path()
                .map(|li| {
                    li.idents()
                        .map(|tok| strip_backticks(tok.text()).to_string())
                        .collect()
                })
                .unwrap_or_default();
            NormalisedMeasure::Named(segments)
        }
        Measure::Var(v) => {
            let name = v
                .name()
                .map(|tok| strip_backticks(tok.text()).to_string())
                .unwrap_or_default();
            // `'u` is a plain typar (`TyparStaticReq.None`); `^u` is the
            // head-type form (`TyparStaticReq.HeadType`).
            NormalisedMeasure::Var(NormalisedTypar {
                name,
                head_type: v.is_head_type(),
                // A measure typar is a bare `SynTypar`, not a `SynTyparDecl`, so
                // no attributes and no intersection constraints.
                attributes: Vec::new(),
                intersection_constraints: Vec::new(),
            })
        }
        Measure::One(_) => NormalisedMeasure::One,
        Measure::Anon(_) => NormalisedMeasure::Anon,
        Measure::Seq(s) => {
            NormalisedMeasure::Seq(s.measures().map(|m| normalise_measure(&m)).collect())
        }
        Measure::Product(p) => {
            let lhs = p.lhs().expect("MEASURE_PRODUCT must have a left factor");
            let rhs = p.rhs().expect("MEASURE_PRODUCT must have a right factor");
            NormalisedMeasure::Product(
                Box::new(normalise_measure(&lhs)),
                Box::new(normalise_measure(&rhs)),
            )
        }
        Measure::Divide(d) => {
            let denom = d
                .denominator()
                .expect("MEASURE_DIVIDE must have a denominator");
            NormalisedMeasure::Divide(
                d.numerator().map(|n| Box::new(normalise_measure(&n))),
                Box::new(normalise_measure(&denom)),
            )
        }
        Measure::Power(p) => {
            let base = p.base().expect("MEASURE_POWER must have a base measure");
            let exponent = p
                .exponent()
                .expect("MEASURE_POWER must have a rational-const exponent");
            let exponent = normalise_rational_const(&exponent);
            let exponent = if p.is_negated() {
                NormalisedRationalConst::Negate(Box::new(exponent))
            } else {
                exponent
            };
            NormalisedMeasure::Power(Box::new(normalise_measure(&base)), exponent)
        }
        Measure::Paren(p) => {
            let inner = p.inner().expect("MEASURE_PAREN must have an inner measure");
            NormalisedMeasure::Paren(Box::new(normalise_measure(&inner)))
        }
    }
}

/// Shared body of [`normalise_const`] (for `CONST_EXPR`) and the
/// pattern-side equivalent (for `CONST_PAT`). Decodes the literal
/// token's text + kind to a `NormalisedConst`. Both surfaces produce
/// the same FCS `SynConst` payload, so the projection is identical.
fn normalise_const_lit(lit: &SyntaxToken) -> NormalisedConst {
    let text = lit.text();
    // Signed kinds use `as uN as iN` so a hex/oct/bin body's bit
    // pattern reinterprets as two's complement (e.g. `0xFFy` → `-1`,
    // `0x80000000l` → `i32::MIN`). For decimal bodies the truncation is
    // a no-op because the parser already range-checked the value into
    // the signed half. Unsigned kinds narrow via `TryFrom<u64>`.
    //
    // A folded `±` sign (the parser's `sign_fold` pass merges an adjacent
    // `+`/`-` into the literal token) is stripped via `split_num_sign`; a
    // `-` then `wrapping_neg`s the typed magnitude, so `-2147483648` →
    // `i32::MIN` and `-128y` → `i8::MIN` round-trip, matching FCS's
    // token-layer fold. The sign never reaches the *unsigned* kinds — the
    // fold pass excludes unsigned suffixes — so those arms stay magnitude-only.
    match lit.kind() {
        SyntaxKind::INT32_LIT => {
            let (minus, body) = split_num_sign(text);
            let v = decode_int_body(body) as u32 as i32;
            NormalisedConst::Int32(if minus { v.wrapping_neg() } else { v })
        }
        SyntaxKind::SBYTE_LIT => {
            let (minus, body) = split_num_sign(text);
            let v = decode_int_body(body) as u8 as i8;
            NormalisedConst::SByte(if minus { v.wrapping_neg() } else { v })
        }
        SyntaxKind::BYTE_LIT => {
            // BYTE_LIT funnels both the integer-suffix form (`255uy`,
            // `0xFFuy`) and the byte-char form (`'a'B`). Source-form
            // dispatch: a leading `'` means the char-decoder; anything
            // else is a digit literal.
            if text.starts_with('\'') {
                let code = u32::from(decode_char_literal(strip_byte_suffix(text)));
                NormalisedConst::Byte(
                    u8::try_from(code)
                        .unwrap_or_else(|_| panic!("byte-char {text:?} out of u8 range")),
                )
            } else {
                NormalisedConst::Byte(parse_suffixed_int(text))
            }
        }
        SyntaxKind::INT16_LIT => {
            let (minus, body) = split_num_sign(text);
            let v = decode_int_body(body) as u16 as i16;
            NormalisedConst::Int16(if minus { v.wrapping_neg() } else { v })
        }
        SyntaxKind::UINT16_LIT => NormalisedConst::UInt16(parse_suffixed_int(text)),
        SyntaxKind::CHAR_LIT => NormalisedConst::Char(decode_char_literal(text)),
        SyntaxKind::STRING_LIT => NormalisedConst::String {
            value: decode_string_literal(text),
            kind: SynStringKind::Regular,
        },
        SyntaxKind::VERBATIM_STRING_LIT => NormalisedConst::String {
            value: decode_verbatim_string_literal(text)
                .encode_utf16()
                .collect(),
            kind: SynStringKind::Verbatim,
        },
        SyntaxKind::TRIPLE_STRING_LIT => NormalisedConst::String {
            value: decode_triple_quote_string_literal(text)
                .encode_utf16()
                .collect(),
            kind: SynStringKind::TripleQuote,
        },
        SyntaxKind::BYTE_STRING_LIT => NormalisedConst::Bytes {
            value: units_to_bytes(&decode_byte_string_body(text, InterpFragmentStyle::Single)),
            kind: SynByteStringKind::Regular,
        },
        SyntaxKind::VERBATIM_BYTE_STRING_LIT => NormalisedConst::Bytes {
            value: units_to_bytes(&decode_byte_string_body(
                text,
                InterpFragmentStyle::Verbatim,
            )),
            kind: SynByteStringKind::Verbatim,
        },
        // Triple-quoted byte strings map to FCS `SynByteStringKind.Regular`
        // (see `lex.fsl:135-136`) — there's no `TripleQuote` byte-string
        // case, even though the source form differs.
        SyntaxKind::TRIPLE_BYTE_STRING_LIT => NormalisedConst::Bytes {
            value: units_to_bytes(&decode_byte_string_body(text, InterpFragmentStyle::Triple)),
            kind: SynByteStringKind::Regular,
        },
        SyntaxKind::UINT32_LIT => NormalisedConst::UInt32(parse_suffixed_int(text)),
        SyntaxKind::INT64_LIT => {
            let (minus, body) = split_num_sign(text);
            let v = decode_int_body(body) as i64;
            NormalisedConst::Int64(if minus { v.wrapping_neg() } else { v })
        }
        SyntaxKind::UINT64_LIT => NormalisedConst::UInt64(parse_suffixed_int(text)),
        SyntaxKind::INTPTR_LIT => {
            let (minus, body) = split_num_sign(text);
            let v = decode_int_body(body) as i64;
            NormalisedConst::IntPtr(if minus { v.wrapping_neg() } else { v })
        }
        SyntaxKind::UINTPTR_LIT => NormalisedConst::UIntPtr(parse_suffixed_int(text)),
        SyntaxKind::DECIMAL_LIT => {
            // FCS folds a `-` via `Decimal.op_UnaryNegation`, whose
            // `ToString(InvariantCulture)` is the canonical form FCS dumps.
            // Canonicalise the magnitude, then prepend `-` unless the value
            // is zero (`-0.0m` ⇒ `"0.0"`, matching .NET — `System.Decimal`
            // has no negative zero). `+` is a no-op.
            let (minus, body) = split_num_sign(text);
            let canon = canonicalise_decimal_source(body);
            let is_zero = canon.chars().all(|c| c == '0' || c == '.');
            NormalisedConst::Decimal(if minus && !is_zero {
                format!("-{canon}")
            } else {
                canon
            })
        }
        SyntaxKind::USER_NUM_LIT => {
            // FCS's bignum fold prepends `"-"` to the value string for a `-`
            // and drops a `+` (`LexFilter.fs:2748`). Strip the folded sign,
            // split the trailing-alpha suffix off the magnitude, then
            // re-attach `-` for the minus case (no zero suppression — FCS
            // keeps `-0I` as `"-0"`). Suffix chars are ASCII, so byte
            // indexing on the magnitude is safe.
            let (minus, body) = split_num_sign(text);
            let (digits, suffix) = body.split_at(body.len() - 1);
            let mag: String = digits.chars().filter(|c| *c != '_').collect();
            NormalisedConst::UserNum {
                value: if minus { format!("-{mag}") } else { mag },
                suffix: suffix.to_string(),
            }
        }
        SyntaxKind::IEEE64_LIT => NormalisedConst::Double(decode_ieee64(text)),
        SyntaxKind::IEEE32_LIT => NormalisedConst::Single(decode_ieee32(text)),
        SyntaxKind::BOOL_LIT => match text {
            "true" => NormalisedConst::Bool(true),
            "false" => NormalisedConst::Bool(false),
            other => panic!("BOOL_LIT token text must be `true` or `false`, got {other:?}"),
        },
        // Unit literal is the multi-token form `( … )` where the literal()
        // accessor returns the leading `LPAREN_TOK`; the matching `RPAREN_TOK`
        // is a later child. We don't read the token's text — its kind alone
        // identifies `SynConst.Unit`.
        SyntaxKind::LPAREN_TOK => NormalisedConst::Unit,
        // The empty verbose-syntax block `begin end` is *also* `SynConst.Unit`
        // (FCS's `mkSynUnit`, `pars.fsy:5430`): the `CONST_EXPR` holds the
        // `BEGIN_TOK`/`END_TOK` pair, with `literal()` returning the leading
        // `BEGIN_TOK`. Its kind alone identifies the unit, like the `LPAREN_TOK`
        // form above.
        SyntaxKind::BEGIN_TOK => NormalisedConst::Unit,
        // `__SOURCE_DIRECTORY__` / `__SOURCE_FILE__` / `__LINE__` —
        // `SynConst.SourceIdentifier`. The token text *is* the spelling; the
        // physical `__LINE__` expansion is computed from the token offset, while
        // path-valued forms are canonicalised because this side has no filename.
        SyntaxKind::SOURCE_IDENTIFIER_LIT => NormalisedConst::SourceIdentifier {
            constant: text.to_string(),
            value: source_identifier_value(lit),
        },
        other => panic!("Phase 2: unsupported CONST_EXPR literal kind {other:?}"),
    }
}

fn source_identifier_value(lit: &SyntaxToken) -> NormalisedSourceIdentifierValue {
    match lit.text() {
        "__SOURCE_DIRECTORY__" => NormalisedSourceIdentifierValue::SourceDirectory,
        "__SOURCE_FILE__" => NormalisedSourceIdentifierValue::SourceFile,
        "__LINE__" => NormalisedSourceIdentifierValue::Line(source_line_number(lit).to_string()),
        other => panic!("unsupported source identifier literal {other:?}"),
    }
}

fn source_line_number(lit: &SyntaxToken) -> u32 {
    let offset = usize::from(lit.text_range().start());
    let root = lit
        .parent()
        .expect("normalised token must have a parent")
        .ancestors()
        .last()
        .expect("parent ancestor chain includes the root");
    let source = root.text().to_string();
    line_number_at_offset(&source, offset)
}

fn line_number_at_offset(source: &str, offset: usize) -> u32 {
    let bytes = source.as_bytes();
    let mut line = 1u32;
    let mut i = 0usize;
    let end = offset.min(bytes.len());
    while i < end {
        match bytes[i] {
            b'\r' => {
                line += 1;
                i += if i + 1 < end && bytes[i + 1] == b'\n' {
                    2
                } else {
                    1
                };
            }
            b'\n' => {
                line += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    line
}

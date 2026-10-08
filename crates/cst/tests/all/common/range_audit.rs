use std::ops::Range;

use borzoi_cst::parser::{Parse, parse, parse_sig};
use borzoi_cst::syntax::{
    AstNode, Binding, ExceptionDefnDecl, Expr, HashDirectiveDecl, ImplFile, LetDecl, MemberDefn,
    ModuleDecl, ModuleOrNamespace, ModuleOrNamespaceKind, NestedModuleDecl, Pat, SigDecl, SigFile,
    SyntaxKind, SyntaxNode, SyntaxToken, TypeDefn, TypeDefnRepr, TypeDefnsDecl,
};
use serde_json::Value;
use tempfile::NamedTempFile;

use super::normalised_ast::{normalise_fcs_dump, normalise_parse};
use super::{LineIndex, assert_fcs_parse_clean, fcs_ast_batch};

#[derive(Debug, Clone, PartialEq, Eq)]
struct AstRangeFact {
    path: String,
    kind: String,
    range: Range<usize>,
}

/// Assert that FCS and our CST agree on the source ranges for the AST layer this
/// harness currently audits: file segments/modules and module/signature
/// declarations, recursively through nested modules.
///
/// This is intentionally separate from the normalised AST equality comparison.
/// The normaliser still compares structure only; this helper makes range
/// equality a distinct proof obligation that can grow without polluting every
/// model enum variant with per-case range fields.
pub fn assert_ast_ranges_match(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fs").expect("create tempfile");
    std::io::Write::write_all(&mut tmp, source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_batch(path);
    assert_fcs_parse_clean(&json, source);
    let fcs = normalise_fcs_dump(&json);

    let parse = parse(source);
    assert!(
        parse.errors.is_empty(),
        "rust parser produced errors for {source:?}: {:?}",
        parse.errors,
    );
    let rust = normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
    assert_range_facts_match(&parse, &json, source);
}

/// Signature-file counterpart of [`assert_ast_ranges_match`].
pub fn assert_sig_ast_ranges_match(source: &str) {
    let mut tmp = NamedTempFile::with_suffix(".fsi").expect("create tempfile");
    std::io::Write::write_all(&mut tmp, source.as_bytes()).expect("write source");
    let path = tmp.path();

    let json = fcs_ast_batch(path);
    assert_fcs_parse_clean(&json, source);
    let fcs = normalise_fcs_dump(&json);

    let parse = parse_sig(source);
    assert!(
        parse.errors.is_empty(),
        "rust parser produced errors for sig {source:?}: {:?}",
        parse.errors,
    );
    let rust = normalise_parse(&parse);

    assert_eq!(
        rust, fcs,
        "sig AST divergence for source {source:?}\n  rust: {rust:#?}\n  fcs:  {fcs:#?}",
    );
    assert_range_facts_match(&parse, &json, source);
}

fn assert_range_facts_match(parse: &Parse, fcs_json: &str, source: &str) {
    if let Err(message) = ast_ranges_match(parse, fcs_json, source) {
        panic!("AST range divergence for source {source:?}\n{message}");
    }
}

/// Compare audited AST ranges and return a diagnostic instead of panicking.
///
/// The ignored corpus sweep uses this to distinguish files whose normalised AST
/// shape matches FCS but whose audited source spans still diverge.
pub fn ast_ranges_match(parse: &Parse, fcs_json: &str, source: &str) -> Result<(), String> {
    let rust = collect_cst_range_facts(parse);
    let fcs = collect_fcs_range_facts(fcs_json, source);

    if rust == fcs {
        Ok(())
    } else {
        Err(format_range_mismatch(&rust, &fcs))
    }
}

fn format_range_mismatch(rust: &[AstRangeFact], fcs: &[AstRangeFact]) -> String {
    use std::fmt::Write as _;

    let mut out = format!(
        "{} rust range facts, {} FCS range facts",
        rust.len(),
        fcs.len()
    );
    for i in 0..rust.len().max(fcs.len()) {
        let rust_fact = rust.get(i);
        let fcs_fact = fcs.get(i);
        if rust_fact != fcs_fact {
            let _ = write!(out, "\n  first mismatch at fact[{i}]");
            match rust_fact {
                Some(fact) => {
                    let _ = write!(out, "\n  rust: {}", format_range_fact(fact));
                }
                None => out.push_str("\n  rust: <missing>"),
            }
            match fcs_fact {
                Some(fact) => {
                    let _ = write!(out, "\n  fcs:  {}", format_range_fact(fact));
                }
                None => out.push_str("\n  fcs:  <missing>"),
            }
            break;
        }
    }
    out
}

fn format_range_fact(fact: &AstRangeFact) -> String {
    format!(
        "{} {} [{}..{})",
        fact.path, fact.kind, fact.range.start, fact.range.end
    )
}

fn collect_cst_range_facts(parse: &Parse) -> Vec<AstRangeFact> {
    match parse.root.kind() {
        SyntaxKind::IMPL_FILE => {
            let file = ImplFile::cast(parse.root.clone()).expect("kind already checked");
            let mut out = Vec::new();
            for (i, module) in file.modules().enumerate() {
                collect_cst_impl_module(&module, format!("module[{i}]"), &mut out);
            }
            out
        }
        SyntaxKind::SIG_FILE => {
            let file = SigFile::cast(parse.root.clone()).expect("kind already checked");
            let mut out = Vec::new();
            for (i, module) in file.modules().enumerate() {
                collect_cst_sig_module(&module, format!("module[{i}]"), &mut out);
            }
            out
        }
        other => panic!("unexpected root kind {other:?}"),
    }
}

fn collect_cst_impl_module(module: &ModuleOrNamespace, path: String, out: &mut Vec<AstRangeFact>) {
    let decls: Vec<_> = module
        .decls()
        .filter(|d| !is_light_hash_directive(d.syntax()))
        .collect();
    push_cst_module_fact(
        out,
        path.clone(),
        module,
        !decls.is_empty(),
        impl_module_includes_trailing_trivia(module.syntax(), &decls),
        !matches!(
            decls.last(),
            Some(ModuleDecl::HashDirective(hash)) if hash_directive_is_argumentless(hash.syntax())
        ),
        impl_decls_range_end_override(decls.iter()),
    );
    for (i, decl) in decls.iter().enumerate() {
        collect_cst_impl_decl(decl, format!("{path}.decl[{i}]"), out);
    }
}

fn collect_cst_sig_module(module: &ModuleOrNamespace, path: String, out: &mut Vec<AstRangeFact>) {
    let decls: Vec<_> = module
        .sig_decls()
        .filter(|d| !is_light_hash_directive(d.syntax()))
        .collect();
    push_cst_sig_module_fact(
        out,
        path.clone(),
        module,
        !decls.is_empty(),
        sig_decls_range_end_override(decls.iter()),
    );
    for (i, decl) in decls.iter().enumerate() {
        collect_cst_sig_decl(decl, format!("{path}.decl[{i}]"), out);
    }
}

fn collect_cst_impl_decl(decl: &ModuleDecl, path: String, out: &mut Vec<AstRangeFact>) {
    out.push(AstRangeFact {
        path: path.clone(),
        kind: impl_decl_kind(decl).to_string(),
        range: impl_decl_ast_range(decl),
    });
    collect_cst_sub_facts(decl, &path, out);
    if let ModuleDecl::NestedModule(nested) = decl {
        collect_cst_nested_impl_module(nested, path, out);
    }
}

fn collect_cst_sig_decl(decl: &SigDecl, path: String, out: &mut Vec<AstRangeFact>) {
    out.push(AstRangeFact {
        path: path.clone(),
        kind: sig_decl_kind(decl).to_string(),
        range: sig_decl_ast_range(decl),
    });
    if let SigDecl::NestedModule(nested) = decl {
        collect_cst_nested_sig_module(nested, path, out);
    }
}

fn collect_cst_nested_impl_module(
    nested: &NestedModuleDecl,
    path: String,
    out: &mut Vec<AstRangeFact>,
) {
    for (i, decl) in nested
        .decls()
        .filter(|d| !is_light_hash_directive(d.syntax()))
        .enumerate()
    {
        collect_cst_impl_decl(&decl, format!("{path}.decl[{i}]"), out);
    }
}

fn collect_cst_nested_sig_module(
    nested: &NestedModuleDecl,
    path: String,
    out: &mut Vec<AstRangeFact>,
) {
    for (i, decl) in nested
        .sig_decls()
        .filter(|d| !is_light_hash_directive(d.syntax()))
        .enumerate()
    {
        collect_cst_sig_decl(&decl, format!("{path}.decl[{i}]"), out);
    }
}

pub(super) fn impl_decl_kind(decl: &ModuleDecl) -> &'static str {
    match decl {
        ModuleDecl::Expr(_) => "Expr",
        ModuleDecl::Let(_) => "Let",
        ModuleDecl::Open(_) => "Open",
        ModuleDecl::NestedModule(_) => "NestedModule",
        ModuleDecl::ModuleAbbrev(_) => "ModuleAbbrev",
        ModuleDecl::Types(_) => "Types",
        ModuleDecl::Exception(_) => "Exception",
        // FCS lowers extern declarations to `SynModuleDecl.Let`, so the
        // structure normaliser does the same. Use the oracle's case name here.
        ModuleDecl::Extern(_) => "Let",
        ModuleDecl::Attributes(_) => "Attributes",
        ModuleDecl::HashDirective(_) => "HashDirective",
    }
}

pub(super) fn sig_decl_kind(decl: &SigDecl) -> &'static str {
    match decl {
        SigDecl::Open(_) => "Open",
        SigDecl::NestedModule(_) => "NestedModule",
        SigDecl::ModuleAbbrev(_) => "ModuleAbbrev",
        SigDecl::Val(_) => "Val",
        SigDecl::Types(_) => "Types",
        SigDecl::Exception(_) => "Exception",
        SigDecl::HashDirective(_) => "HashDirective",
    }
}

fn impl_decl_owns_xml_doc(decl: &ModuleDecl) -> bool {
    matches!(
        decl,
        ModuleDecl::Let(_)
            | ModuleDecl::NestedModule(_)
            | ModuleDecl::Types(_)
            | ModuleDecl::Exception(_)
            | ModuleDecl::Extern(_)
    )
}

fn sig_decl_owns_xml_doc(decl: &SigDecl) -> bool {
    matches!(
        decl,
        SigDecl::NestedModule(_) | SigDecl::Val(_) | SigDecl::Types(_) | SigDecl::Exception(_)
    )
}

fn impl_module_includes_trailing_trivia(module: &SyntaxNode, decls: &[ModuleDecl]) -> bool {
    if node_ends_with_token(module, SyntaxKind::SEMISEMI_TOK) {
        return false;
    }
    match decls.last() {
        Some(ModuleDecl::Expr(expr)) => expr_decl_includes_trailing_trivia(expr.syntax()),
        Some(ModuleDecl::NestedModule(nested)) => {
            !(nested_module_has_own_end_token(nested)
                && has_final_line_indent_tail(nested.syntax()))
        }
        Some(ModuleDecl::Exception(exception)) => !exception_decl_has_trailing_end_trim(exception),
        Some(ModuleDecl::Open(_) | ModuleDecl::Extern(_) | ModuleDecl::HashDirective(_)) => false,
        _ => true,
    }
}

fn nested_module_has_own_end_token(nested: &NestedModuleDecl) -> bool {
    nested
        .syntax()
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|token| token.kind() == SyntaxKind::END_TOK && !token.text_range().is_empty())
}

fn expr_decl_includes_trailing_trivia(expr_decl: &SyntaxNode) -> bool {
    expr_decl
        .children()
        .any(|child| expr_node_includes_trailing_trivia(&child))
}

fn expr_node_includes_trailing_trivia(node: &SyntaxNode) -> bool {
    expr_kind_includes_trailing_trivia(node.kind()) || app_expr_has_direct_trailing_trivia_arg(node)
}

fn expr_kind_includes_trailing_trivia(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::DO_EXPR
            | SyntaxKind::FOR_EACH_EXPR
            | SyntaxKind::FOR_EXPR
            | SyntaxKind::IF_THEN_ELSE_EXPR
            | SyntaxKind::MATCH_EXPR
            | SyntaxKind::MATCH_LAMBDA_EXPR
            | SyntaxKind::TRY_EXPR
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::WHILE_BANG_EXPR
    )
}

fn app_expr_has_direct_trailing_trivia_arg(node: &SyntaxNode) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::APP_EXPR | SyntaxKind::INFIX_APP_EXPR
    ) && node
        .children()
        .any(|child| expr_kind_includes_trailing_trivia(child.kind()))
}

fn push_cst_module_fact(
    out: &mut Vec<AstRangeFact>,
    path: String,
    module: &ModuleOrNamespace,
    has_audited_decls: bool,
    include_trailing_trivia: bool,
    trim_trailing_statement_separator: bool,
    end_override: Option<usize>,
) {
    out.push(AstRangeFact {
        path,
        kind: "Module".to_string(),
        range: module_ast_range(
            module,
            has_audited_decls,
            include_trailing_trivia,
            trim_trailing_statement_separator,
            end_override,
        ),
    });
}

fn push_cst_sig_module_fact(
    out: &mut Vec<AstRangeFact>,
    path: String,
    module: &ModuleOrNamespace,
    has_audited_decls: bool,
    end_override: Option<usize>,
) {
    out.push(AstRangeFact {
        path,
        kind: "Module".to_string(),
        range: sig_module_ast_range(module, has_audited_decls, end_override),
    });
}

fn module_ast_range(
    module: &ModuleOrNamespace,
    has_audited_decls: bool,
    include_trailing_trivia: bool,
    trim_trailing_statement_separator: bool,
    end_override: Option<usize>,
) -> Range<usize> {
    let kind = module.kind();
    if kind == ModuleOrNamespaceKind::Anon
        && !has_audited_decls
        && !has_non_trivia_tokens(module.syntax())
    {
        let end = root_text_end(module.syntax());
        return end..end;
    }
    let mut range = node_source_range(module.syntax());
    if kind != ModuleOrNamespaceKind::Anon
        && let Some(end) = trim_trailing_statement_separator
            .then(|| trailing_statement_separator_trim_end(module.syntax()))
            .flatten()
    {
        range.end = range.end.min(end);
    }
    if kind != ModuleOrNamespaceKind::Anon
        && let Some(end) = end_override
    {
        range.end = range.end.min(end);
    }
    node_ast_range_from_source_range(
        module.syntax(),
        range,
        kind == ModuleOrNamespaceKind::NamedModule,
        kind == ModuleOrNamespaceKind::Anon && include_trailing_trivia,
    )
}

fn sig_module_ast_range(
    module: &ModuleOrNamespace,
    has_audited_decls: bool,
    end_override: Option<usize>,
) -> Range<usize> {
    let kind = module.kind();
    if kind == ModuleOrNamespaceKind::Anon
        && !has_audited_decls
        && !has_non_trivia_tokens(module.syntax())
    {
        let end = root_text_end(module.syntax());
        return end..end;
    }
    if kind == ModuleOrNamespaceKind::Anon {
        return node_ast_range(module.syntax(), false, true);
    }
    let mut range = node_source_range_excluding_trailing_end(module.syntax());
    if let Some(end) = end_override {
        range.end = range.end.min(end);
    }
    node_ast_range_from_source_range(
        module.syntax(),
        range,
        kind == ModuleOrNamespaceKind::NamedModule,
        false,
    )
}

pub(super) fn impl_decl_ast_range(decl: &ModuleDecl) -> Range<usize> {
    let mut range = match decl {
        ModuleDecl::HashDirective(hash) => hash_directive_source_range(hash.syntax()),
        ModuleDecl::Exception(exception) => exception_decl_source_range(exception),
        ModuleDecl::Let(let_decl) => let_decl_source_range(let_decl),
        _ => node_source_range(decl.syntax()),
    };
    if let Some(end) = impl_decl_range_end_override(decl) {
        range.end = range.end.min(end);
    }
    node_ast_range_from_source_range(decl.syntax(), range, impl_decl_owns_xml_doc(decl), false)
}

pub(super) fn sig_decl_ast_range(decl: &SigDecl) -> Range<usize> {
    let mut range = match decl {
        SigDecl::HashDirective(hash) => hash_directive_source_range(hash.syntax()),
        SigDecl::Types(types) => {
            return sig_type_decl_ast_range(types, sig_decl_owns_xml_doc(decl));
        }
        SigDecl::Exception(exception) => exception_decl_source_range(exception),
        _ => node_source_range(decl.syntax()),
    };
    if let Some(end) = sig_decl_range_end_override(decl) {
        range.end = range.end.min(end);
    }
    node_ast_range_from_source_range(decl.syntax(), range, sig_decl_owns_xml_doc(decl), false)
}

fn impl_decls_range_end_override<'a>(
    decls: impl IntoIterator<Item = &'a ModuleDecl>,
) -> Option<usize> {
    let mut last = None;
    for decl in decls {
        last = impl_decl_range_end_override(decl);
    }
    last
}

fn impl_decl_range_end_override(decl: &ModuleDecl) -> Option<usize> {
    let child_end = match decl {
        ModuleDecl::Let(let_decl) => let_decl_static_optimization_main_expr_end(let_decl),
        ModuleDecl::NestedModule(nested) => nested_module_range_end_override(nested),
        ModuleDecl::Types(types) => type_decl_trailing_end_trim_end(types),
        ModuleDecl::Exception(exception) => exception_decl_range_end_override(exception),
        _ => None,
    };
    [child_end, trailing_semi_trim_end(decl.syntax())]
        .into_iter()
        .flatten()
        .min()
}

fn sig_decls_range_end_override<'a>(decls: impl IntoIterator<Item = &'a SigDecl>) -> Option<usize> {
    let mut last = None;
    for decl in decls {
        last = sig_decl_range_end_override(decl);
    }
    last
}

fn sig_decl_range_end_override(decl: &SigDecl) -> Option<usize> {
    match decl {
        SigDecl::NestedModule(nested) => nested_sig_module_range_end_override(nested),
        SigDecl::Exception(exception) => exception_decl_range_end_override(exception),
        _ => None,
    }
}

fn nested_module_range_end_override(nested: &NestedModuleDecl) -> Option<usize> {
    let mut last = None;
    for decl in nested
        .decls()
        .filter(|d| !is_light_hash_directive(d.syntax()))
    {
        last = impl_decl_range_end_override(&decl);
    }
    last
}

fn nested_sig_module_range_end_override(nested: &NestedModuleDecl) -> Option<usize> {
    let mut last = None;
    for decl in nested
        .sig_decls()
        .filter(|d| !is_light_hash_directive(d.syntax()))
    {
        last = sig_decl_range_end_override(&decl);
    }
    last
}

fn let_decl_source_range(let_decl: &LetDecl) -> Range<usize> {
    let node = let_decl.syntax();
    let mut range = node_source_range(node);
    if let Some(end) = immediately_following_same_line_in_end(node, range.end) {
        range.end = end;
    }
    range
}

fn trailing_semi_trim_end(node: &SyntaxNode) -> Option<usize> {
    trailing_token_trim_end(node, |kind| kind == SyntaxKind::SEMI_TOK)
}

fn trailing_statement_separator_trim_end(node: &SyntaxNode) -> Option<usize> {
    trailing_token_trim_end(node, |kind| {
        matches!(kind, SyntaxKind::SEMI_TOK | SyntaxKind::SEMISEMI_TOK)
    })
}

fn trailing_token_trim_end(
    node: &SyntaxNode,
    should_trim: impl Fn(SyntaxKind) -> bool,
) -> Option<usize> {
    let mut previous_end = None;
    let mut last_token = None;

    for token in node
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .filter(|t| !t.text_range().is_empty())
    {
        previous_end = last_token
            .as_ref()
            .map(|last: &SyntaxToken| usize::from(last.text_range().end()));
        last_token = Some(token);
    }

    match (last_token, previous_end) {
        (Some(last), Some(end)) if should_trim(last.kind()) => Some(end),
        _ => None,
    }
}

fn node_ends_with_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    node.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .filter(|t| !t.text_range().is_empty())
        .last()
        .is_some_and(|token| token.kind() == kind)
}

fn let_decl_static_optimization_main_expr_end(let_decl: &LetDecl) -> Option<usize> {
    let mut last_binding_expr = None;
    for binding in let_decl.bindings() {
        last_binding_expr = binding.expr();
    }
    match last_binding_expr? {
        Expr::StaticOptimization(static_opt) => static_opt
            .main_expr()
            .map(|expr| node_source_range(expr.syntax()).end),
        _ => None,
    }
}

fn node_ast_range(
    node: &SyntaxNode,
    include_xml_doc_prefix: bool,
    include_trailing_anon_module_trivia: bool,
) -> Range<usize> {
    node_ast_range_from_source_range(
        node,
        node_source_range(node),
        include_xml_doc_prefix,
        include_trailing_anon_module_trivia,
    )
}

fn node_ast_range_from_source_range(
    node: &SyntaxNode,
    mut range: Range<usize>,
    include_xml_doc_prefix: bool,
    include_trailing_anon_module_trivia: bool,
) -> Range<usize> {
    if include_xml_doc_prefix && let Some(start) = leading_xml_doc_start(node, range.start) {
        range.start = start;
    }
    if include_trailing_anon_module_trivia && has_only_trivia_after(node, range.end) {
        range.end = root_text_end(node);
    }
    range
}

fn exception_decl_source_range(exception: &ExceptionDefnDecl) -> Range<usize> {
    let node = exception.syntax();
    let mut range = node_source_range(node);
    if let Some(end) = exception_decl_range_end_override(exception) {
        range.end = end;
    }
    range
}

fn exception_decl_range_end_override(exception: &ExceptionDefnDecl) -> Option<usize> {
    if exception.members().next().is_some() {
        direct_trailing_end_trim_end(exception.syntax())
    } else {
        empty_exception_augmentation_trim_end(exception.syntax())
    }
}

fn exception_decl_has_trailing_end_trim(exception: &ExceptionDefnDecl) -> bool {
    exception_decl_range_end_override(exception).is_some()
}

fn direct_trailing_end_trim_end(node: &SyntaxNode) -> Option<usize> {
    let mut before_previous_end = None;
    let mut previous_token = None;
    let mut last_token = None;

    for token in node
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .filter(|t| !t.text_range().is_empty())
    {
        before_previous_end = previous_token
            .as_ref()
            .map(|previous: &SyntaxToken| usize::from(previous.text_range().end()));
        previous_token = last_token;
        last_token = Some(token);
    }

    let last = last_token?;
    if last.kind() == SyntaxKind::END_TOK && last.parent() == Some(node.clone()) {
        match previous_token {
            Some(previous) if previous.kind() == SyntaxKind::SEMI_TOK => before_previous_end,
            Some(previous) => Some(usize::from(previous.text_range().end())),
            None => None,
        }
    } else {
        None
    }
}

fn empty_exception_augmentation_trim_end(node: &SyntaxNode) -> Option<usize> {
    let mut with_start = None;
    let mut saw_end = false;

    for token in node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .filter(|t| !t.text_range().is_empty())
    {
        if with_start.is_some() {
            if saw_end || token.kind() != SyntaxKind::END_TOK {
                return None;
            }
            saw_end = true;
        } else if token.kind() == SyntaxKind::WITH_TOK {
            with_start = Some(usize::from(token.text_range().start()));
        }
    }

    let with_start = with_start?;
    if !saw_end {
        return None;
    }

    node.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .filter(|t| !t.text_range().is_empty())
        .take_while(|token| usize::from(token.text_range().start()) < with_start)
        .last()
        .map(|token| usize::from(token.text_range().end()))
}

fn hash_directive_source_range(node: &SyntaxNode) -> Range<usize> {
    let mut range = node_source_range(node);
    if hash_directive_is_argumentless(node)
        && let Some(end) = immediately_following_semisemi_end(node, range.end)
    {
        range.end = end;
    }
    range
}

fn hash_directive_is_argumentless(node: &SyntaxNode) -> bool {
    let mut saw_name = false;
    for elem in node.children_with_tokens() {
        match elem {
            rowan::NodeOrToken::Token(token)
                if token.kind().is_trivia() || token.text_range().is_empty() => {}
            rowan::NodeOrToken::Token(token) if token.kind() == SyntaxKind::HASH_TOK => {}
            rowan::NodeOrToken::Token(token)
                if token.kind() == SyntaxKind::IDENT_TOK && !saw_name =>
            {
                saw_name = true;
            }
            rowan::NodeOrToken::Token(_) | rowan::NodeOrToken::Node(_) if saw_name => {
                return false;
            }
            rowan::NodeOrToken::Token(_) | rowan::NodeOrToken::Node(_) => {}
        }
    }
    saw_name
}

/// The first significant (non-trivia, non-empty) token of `node`'s tree that
/// starts at or after `offset`. Walks forward from `offset` rather than from the
/// root: the recovered-tree sweeps call this once per declaration, and a scan
/// from the root makes that quadratic in the file.
fn first_significant_token_from(node: &SyntaxNode, offset: usize) -> Option<SyntaxToken> {
    let root = node.ancestors().last().unwrap_or_else(|| node.clone());
    let at = rowan::TextSize::try_from(offset).expect("offset fits a TextSize");
    if at >= root.text_range().end() {
        return None;
    }
    let first = root.token_at_offset(at).left_biased()?;
    std::iter::successors(Some(first), |t| t.next_token()).find(|token| {
        !token.kind().is_trivia()
            && !token.text_range().is_empty()
            && usize::from(token.text_range().start()) >= offset
    })
}

fn immediately_following_semisemi_end(node: &SyntaxNode, end: usize) -> Option<usize> {
    first_significant_token_from(node, end)
        .filter(|token| {
            token.kind() == SyntaxKind::SEMISEMI_TOK
                && usize::from(token.text_range().start()) == end
        })
        .map(|token| usize::from(token.text_range().end()))
}

fn immediately_following_same_line_in_end(node: &SyntaxNode, end: usize) -> Option<usize> {
    first_significant_token_from(node, end)
        .filter(|token| {
            token.kind() == SyntaxKind::IN_TOK
                && !has_newline_between(node, end, usize::from(token.text_range().start()))
        })
        .map(|token| usize::from(token.text_range().end()))
}

fn sig_type_decl_ast_range(types: &TypeDefnsDecl, include_xml_doc_prefix: bool) -> Range<usize> {
    let node = types.syntax();
    let mut range = node_source_range_excluding_trailing_end(node);
    if let Some(end) = bodyless_sig_type_typars_trim_end(types) {
        range.end = range.end.min(end);
    }

    node_ast_range_from_source_range(node, range, include_xml_doc_prefix, false)
}

fn type_decl_trailing_end_trim_end(types: &TypeDefnsDecl) -> Option<usize> {
    let defn = types.defns().last()?;
    let end = type_defn_constituents_end(&defn)?;
    (end < node_source_range(types.syntax()).end).then_some(end)
}

/// FCS computes a `SynTypeDefn`'s range compositionally: the header unioned
/// with the repr's range and each outer member's range (pars.fsy's
/// `unionRangeWithListBy`). Tokens that belong to no constituent — member
/// separators and an augmentation tail's closing `end` — never enter the
/// range. A bare augmentation's `with` *is* included (FCS homes it in the
/// `Augmentation` object-model repr), whereas the `with` introducing trailing
/// members on a simple repr is trivia; our CST mirrors that split, so the
/// declaration ends at its last constituent node. A `class`/`interface` body's
/// own `end` sits inside the repr node and is therefore kept.
///
/// `None` when there is nothing to exclude: a bodyless defn (`type Foo`) ends
/// at its header, and a malformed defn keeps its full span.
fn type_defn_constituents_end(defn: &TypeDefn) -> Option<usize> {
    if let Some(member) = defn.members().last() {
        let node = member.syntax();
        let end = node_source_range(node).end;
        // FCS ends the member at its body; a separator lexed into the member's
        // binding (`member _.M = 1; end`) is excluded.
        return Some(trailing_semi_trim_end(node).map_or(end, |trimmed| trimmed.min(end)));
    }
    let repr = defn.repr()?;
    Some(node_source_range(repr.syntax()).end)
}

fn bodyless_sig_type_typars_trim_end(types: &TypeDefnsDecl) -> Option<usize> {
    let mut defns = types.defns();
    let defn = defns.next()?;
    if defns.next().is_some() || defn.repr().is_some() {
        return None;
    }

    let typars = defn.typar_decls()?;
    let typar_range = typars.syntax().text_range();
    let typar_start = usize::from(typar_range.start());
    let typar_end = usize::from(typar_range.end());

    if defn
        .syntax()
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|token| !token.kind().is_trivia())
        .filter(|token| !token.text_range().is_empty())
        .any(|token| usize::from(token.text_range().start()) >= typar_end)
    {
        return None;
    }

    defn.syntax()
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|token| !token.kind().is_trivia())
        .filter(|token| !token.text_range().is_empty())
        .take_while(|token| usize::from(token.text_range().start()) < typar_start)
        .last()
        .map(|token| usize::from(token.text_range().end()))
}

pub(super) fn node_source_range(node: &SyntaxNode) -> Range<usize> {
    let mut tokens = node
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .filter(|t| !t.text_range().is_empty());
    match tokens.next() {
        Some(first) => {
            let start = usize::from(first.text_range().start());
            let end = tokens
                .last()
                .unwrap_or_else(|| first.clone())
                .text_range()
                .end();
            start..usize::from(end)
        }
        None => {
            let range = node.text_range();
            usize::from(range.start())..usize::from(range.end())
        }
    }
}

fn node_source_range_excluding_trailing_end(node: &SyntaxNode) -> Range<usize> {
    let mut first_start = None;
    let mut previous_end = None;
    let mut last_token = None;

    for token in node
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .filter(|t| !t.text_range().is_empty())
    {
        first_start.get_or_insert_with(|| usize::from(token.text_range().start()));
        previous_end = last_token
            .as_ref()
            .map(|last: &SyntaxToken| usize::from(last.text_range().end()));
        last_token = Some(token);
    }

    match (first_start, last_token, previous_end) {
        (Some(start), Some(last), Some(end))
            if last.kind() == SyntaxKind::END_TOK
                && has_newline_between(node, end, usize::from(last.text_range().start()))
                && trailing_end_closes_nonempty_object_model_body(&last) =>
        {
            start..end
        }
        (Some(start), Some(last), _) => start..usize::from(last.text_range().end()),
        _ => {
            let range = node.text_range();
            usize::from(range.start())..usize::from(range.end())
        }
    }
}

fn trailing_end_closes_nonempty_object_model_body(end: &SyntaxToken) -> bool {
    let Some(parent) = end.parent() else {
        return false;
    };
    if parent.kind() != SyntaxKind::OBJECT_MODEL_REPR {
        return false;
    }

    let end_start = end.text_range().start();
    parent
        .children()
        .any(|child| child.text_range().end() <= end_start && MemberDefn::can_cast(child.kind()))
}

fn has_newline_between(node: &SyntaxNode, start: usize, end: usize) -> bool {
    let root = node.ancestors().last().unwrap_or_else(|| node.clone());
    let len = usize::from(root.text_range().len());
    if start > end || end > len {
        return false;
    }
    let range = rowan::TextRange::new(
        rowan::TextSize::try_from(start).expect("offset fits a TextSize"),
        rowan::TextSize::try_from(end).expect("offset fits a TextSize"),
    );
    root.text().slice(range).contains_char('\n')
}

fn root_text_end(node: &SyntaxNode) -> usize {
    let root = node.ancestors().last().unwrap_or_else(|| node.clone());
    usize::from(root.text_range().end())
}

fn has_final_line_indent_tail(node: &SyntaxNode) -> bool {
    let end = node_source_range(node).end;
    let root = node.ancestors().last().unwrap_or_else(|| node.clone());
    let text = root.text().to_string();
    let Some(tail) = text.get(end..) else {
        return false;
    };
    let Some(after_line_break) = tail
        .strip_prefix("\r\n")
        .or_else(|| tail.strip_prefix('\n'))
        .or_else(|| tail.strip_prefix('\r'))
    else {
        return false;
    };

    !after_line_break.is_empty() && after_line_break.chars().all(|ch| matches!(ch, ' ' | '\t'))
}

fn has_non_trivia_tokens(node: &SyntaxNode) -> bool {
    node.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|token| !token.kind().is_trivia() && !token.text_range().is_empty())
}

fn leading_xml_doc_start(node: &SyntaxNode, node_start: usize) -> Option<usize> {
    let root = node.ancestors().last().unwrap_or_else(|| node.clone());
    // Walk back from `node_start` rather than forward from the root: the
    // recovered-tree sweeps call this once per declaration, and a forward scan
    // makes that quadratic in the file.
    let offset = rowan::TextSize::try_from(node_start).expect("offset fits a TextSize");
    let at = if offset >= root.text_range().end() {
        root.last_token()
    } else {
        root.token_at_offset(offset).left_biased()
    };
    let preceding_tokens = std::iter::successors(at, |t| t.prev_token())
        .filter(|token| !token.text_range().is_empty())
        .filter(|token| usize::from(token.text_range().end()) <= node_start);

    let mut start = None;
    for token in preceding_tokens {
        match token.kind() {
            SyntaxKind::LINE_COMMENT if is_xml_doc_comment(token.text()) => {
                start = Some(usize::from(token.text_range().start()));
            }
            SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT if start.is_some() => break,
            kind if kind.is_trivia() => {}
            _ => break,
        }
    }
    start
}

fn has_only_trivia_after(node: &SyntaxNode, end: usize) -> bool {
    let root = node.ancestors().last().unwrap_or_else(|| node.clone());
    root.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|token| usize::from(token.text_range().start()) >= end)
        .all(|token| token.kind().is_trivia() || token.text_range().is_empty())
}

fn is_xml_doc_comment(text: &str) -> bool {
    text.starts_with("///") && !text.starts_with("////")
}

pub(super) fn is_light_hash_directive(node: &SyntaxNode) -> bool {
    if !HashDirectiveDecl::can_cast(node.kind()) {
        return false;
    }
    let token = |kind| {
        node.children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(move |token| token.kind() == kind)
    };
    match (token(SyntaxKind::HASH_TOK), token(SyntaxKind::IDENT_TOK)) {
        (Some(hash), Some(ident)) => {
            hash.text_range().end() == ident.text_range().start() && ident.text() == "light"
        }
        _ => false,
    }
}

// ============================================================================
// Sub-declaration range facts
// ============================================================================
//
// Below the declaration layer, the facts are the ranges consumers read: the
// *names* that are definition sites. Each type definition's name in a
// `type … and …` group, each union case's, record field's and `member`'s name,
// and each `let` (or `extern`) binding's binder. Ours is the range of the token
// the facade returns (`UnionCase::ident`, `RecordFieldDecl::ident`, the last
// segment of a binding's head pattern, `ActivePatName::name_range` for an
// active-pattern binder); FCS's is the `Ident`'s `idRange`. A wrong token with
// the right text (the right-hand `x` of `let x = x` as the binder, say)
// normalises identically and differs only here.
//
// The nodes' own ranges are not audited: no consumer reads them, and FCS
// draws them inconsistently (a `SynUnionCase` covers its attributes when it
// has no fields and not when it has some).

fn token_range(token: &SyntaxToken) -> Range<usize> {
    let r = token.text_range();
    usize::from(r.start())..usize::from(r.end())
}

fn push_cst_name(out: &mut Vec<AstRangeFact>, path: String, kind: &str, range: Range<usize>) {
    out.push(AstRangeFact {
        path,
        kind: kind.to_string(),
        range,
    });
}

fn collect_cst_sub_facts(decl: &ModuleDecl, path: &str, out: &mut Vec<AstRangeFact>) {
    match decl {
        ModuleDecl::Types(types) => {
            for (j, defn) in types.defns().enumerate() {
                collect_cst_type_defn(&defn, &format!("{path}.type[{j}]"), out);
            }
        }
        ModuleDecl::Let(let_decl) => {
            for (j, binding) in let_decl.bindings().enumerate() {
                if let Some(range) = cst_binder_range(&binding) {
                    push_cst_name(out, format!("{path}.binding[{j}]"), "Binder", range);
                }
            }
        }
        // FCS lowers `extern` to a `Let` whose one binding is named by it.
        ModuleDecl::Extern(e) => {
            if let Some(name) = e.name().and_then(|li| li.idents().last()) {
                push_cst_name(
                    out,
                    format!("{path}.binding[0]"),
                    "Binder",
                    token_range(&name),
                );
            }
        }
        _ => {}
    }
}

fn collect_cst_type_defn(defn: &TypeDefn, path: &str, out: &mut Vec<AstRangeFact>) {
    if let Some(name) = defn.long_id().and_then(|li| li.idents().last()) {
        push_cst_name(out, path.to_string(), "TypeName", token_range(&name));
    }
    match defn.repr() {
        Some(TypeDefnRepr::Union(u)) => {
            for (k, case) in u.cases().enumerate() {
                if let Some(name) = case.ident() {
                    push_cst_name(
                        out,
                        format!("{path}.case[{k}]"),
                        "UnionCase",
                        token_range(&name),
                    );
                }
            }
        }
        Some(TypeDefnRepr::Record(r)) => {
            for (k, field) in r.fields().enumerate() {
                if let Some(name) = field.ident() {
                    push_cst_name(
                        out,
                        format!("{path}.field[{k}]"),
                        "Field",
                        token_range(&name),
                    );
                }
            }
        }
        Some(TypeDefnRepr::ObjectModel(om)) => {
            collect_cst_members(om.members(), &format!("{path}.repr"), out);
        }
        _ => {}
    }
    collect_cst_members(defn.members(), path, out);
}

fn collect_cst_members(
    members: impl Iterator<Item = MemberDefn>,
    path: &str,
    out: &mut Vec<AstRangeFact>,
) {
    let members = members.filter_map(|m| match m {
        MemberDefn::Member(m) => Some(m),
        _ => None,
    });
    for (k, m) in members.enumerate() {
        if let Some(range) = m.binding().as_ref().and_then(cst_binder_range) {
            push_cst_name(out, format!("{path}.member[{k}]"), "Member", range);
        }
    }
}

/// The binder name of a binding whose head is a plain name (`let x`), a
/// function or member head (`let f a b`, `member this.M x`), or an active
/// pattern: the name's range.
fn cst_binder_range(binding: &Binding) -> Option<Range<usize>> {
    let range = match binding.pat()? {
        Pat::Named(named) => match named.active_pat_name() {
            Some(active) => active.name_range()?,
            None => named.ident()?.text_range(),
        },
        Pat::LongIdent(head) => match head.active_pat_name() {
            Some(active) => active.name_range()?,
            None => head.head()?.idents().last()?.text_range(),
        },
        _ => return None,
    };
    Some(usize::from(range.start())..usize::from(range.end()))
}

fn collect_fcs_sub_facts(
    kind: &str,
    decl_fields: &[Value],
    path: &str,
    line_index: &LineIndex<'_>,
    out: &mut Vec<AstRangeFact>,
) {
    match kind {
        "Types" => {
            let defns = decl_fields[0]
                .as_array()
                .expect("SynModuleDecl.Types holds a SynTypeDefn list");
            for (j, defn) in defns.iter().enumerate() {
                collect_fcs_type_defn(defn, &format!("{path}.type[{j}]"), line_index, out);
            }
        }
        "Let" => {
            let bindings = decl_fields[1]
                .as_array()
                .expect("SynModuleDecl.Let holds a SynBinding list");
            for (j, binding) in bindings.iter().enumerate() {
                if let Some(range) = fcs_binder_range(binding) {
                    push_fcs_fact(
                        out,
                        format!("{path}.binding[{j}]"),
                        "Binder",
                        range,
                        line_index,
                    );
                }
            }
        }
        _ => {}
    }
}

fn id_range(ident: &Value) -> &Value {
    ident.get("idRange").expect("an Ident has an idRange")
}

fn collect_fcs_type_defn(
    defn: &Value,
    path: &str,
    line_index: &LineIndex<'_>,
    out: &mut Vec<AstRangeFact>,
) {
    // `SynTypeDefn(typeInfo, typeRepr, members, implicitConstructor, range,
    // trivia)`; `SynComponentInfo(attributes, typeParams, constraints, longId,
    // …)`.
    let f = fields(defn);
    let long_id = fields(&f[0])[3]
        .as_array()
        .expect("SynComponentInfo.longId");
    if let Some(name) = long_id.last() {
        push_fcs_fact(
            out,
            path.to_string(),
            "TypeName",
            id_range(name),
            line_index,
        );
    }
    let repr = &f[1];
    match case_name(repr) {
        "Simple" => {
            let simple = &fields(repr)[0];
            match case_name(simple) {
                // `Union(accessibility, unionCases, range)`;
                // `SynUnionCase(attributes, ident: SynIdent, …)`. An
                // operator-named case (`([])`) has no name token on our side.
                "Union" => {
                    let cases = fields(simple)[1].as_array().expect("union cases");
                    for (k, case) in cases.iter().enumerate() {
                        let ident = &fields(&fields(case)[1])[0];
                        let text = ident.get("idText").and_then(Value::as_str);
                        if text.is_some_and(|t| t.starts_with("op_")) {
                            continue;
                        }
                        push_fcs_fact(
                            out,
                            format!("{path}.case[{k}]"),
                            "UnionCase",
                            id_range(ident),
                            line_index,
                        );
                    }
                }
                // `Record(accessibility, recordFields, range)`;
                // `SynField(attributes, isStatic, idOpt, …)`.
                "Record" => {
                    let rfields = fields(simple)[1].as_array().expect("record fields");
                    for (k, field) in rfields.iter().enumerate() {
                        let id_opt = &fields(field)[2];
                        if !id_opt.is_null() && case_name(id_opt) == "Some" {
                            push_fcs_fact(
                                out,
                                format!("{path}.field[{k}]"),
                                "Field",
                                id_range(&fields(id_opt)[0]),
                                line_index,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        // `ObjectModel(kind, members, range)`.
        "ObjectModel" => {
            collect_fcs_members(&fields(repr)[1], &format!("{path}.repr"), line_index, out);
        }
        _ => {}
    }
    collect_fcs_members(&f[2], path, line_index, out);
}

fn collect_fcs_members(
    members: &Value,
    path: &str,
    line_index: &LineIndex<'_>,
    out: &mut Vec<AstRangeFact>,
) {
    let members = members.as_array().expect("a SynMemberDefn list");
    // Only `SynMemberDefn.Member(memberDefn, range)`, counted among
    // themselves: FCS lists an implicit constructor among the members, and we
    // do not.
    let members = members.iter().filter(|m| case_name(m) == "Member");
    for (k, member) in members.enumerate() {
        if let Some(range) = fcs_binder_range(&fields(member)[0]) {
            push_fcs_fact(
                out,
                format!("{path}.member[{k}]"),
                "Member",
                range,
                line_index,
            );
        }
    }
}

/// `SynBinding.headPat` (field 7): a `Named(SynIdent(ident, _), …)` or a
/// `LongIdent(SynLongIdent(idents, …), …)` head's last ident.
fn fcs_binder_range(binding: &Value) -> Option<&Value> {
    let head = &fields(binding)[7];
    let ident = match case_name(head) {
        "Named" => &fields(&fields(head)[0])[0],
        "LongIdent" => fields(&fields(head)[0])[0].as_array()?.last()?,
        _ => return None,
    };
    ident.get("idRange")
}

fn collect_fcs_range_facts(json: &str, source: &str) -> Vec<AstRangeFact> {
    let dump: Value = serde_json::from_str(json).expect("fcs-dump JSON shape");
    let parse_tree = dump
        .get("ParseTree")
        .expect("fcs-dump payload missing ParseTree");
    let line_index = LineIndex::new(source);
    match case_name(parse_tree) {
        "ImplFile" => {
            let impl_file = &fields(parse_tree)[0];
            let modules = fields(impl_file)[4]
                .as_array()
                .expect("ParsedImplFileInput.modules must be array");
            let mut out = Vec::new();
            for (i, module) in modules.iter().enumerate() {
                collect_fcs_impl_module(module, format!("module[{i}]"), &line_index, &mut out);
            }
            out
        }
        "SigFile" => {
            let sig_file = &fields(parse_tree)[0];
            let modules = fields(sig_file)[3]
                .as_array()
                .expect("ParsedSigFileInput.contents must be array");
            let mut out = Vec::new();
            for (i, module) in modules.iter().enumerate() {
                collect_fcs_sig_module(module, format!("module[{i}]"), &line_index, &mut out);
            }
            out
        }
        other => panic!("unknown ParsedInput case {other:?}"),
    }
}

fn collect_fcs_impl_module(
    module: &Value,
    path: String,
    line_index: &LineIndex<'_>,
    out: &mut Vec<AstRangeFact>,
) {
    let module_fields = fields(module);
    push_fcs_fact(out, path.clone(), "Module", &module_fields[7], line_index);
    let decls = module_fields[3]
        .as_array()
        .expect("SynModuleOrNamespace.decls must be array");
    for (i, decl) in decls.iter().enumerate() {
        collect_fcs_impl_decl(decl, format!("{path}.decl[{i}]"), line_index, out);
    }
}

fn collect_fcs_sig_module(
    module: &Value,
    path: String,
    line_index: &LineIndex<'_>,
    out: &mut Vec<AstRangeFact>,
) {
    let module_fields = fields(module);
    push_fcs_fact(out, path.clone(), "Module", &module_fields[7], line_index);
    let decls = module_fields[3]
        .as_array()
        .expect("SynModuleOrNamespaceSig.decls must be array");
    for (i, decl) in decls.iter().enumerate() {
        collect_fcs_sig_decl(decl, format!("{path}.decl[{i}]"), line_index, out);
    }
}

fn collect_fcs_impl_decl(
    decl: &Value,
    path: String,
    line_index: &LineIndex<'_>,
    out: &mut Vec<AstRangeFact>,
) {
    let kind = case_name(decl);
    let decl_fields = fields(decl);
    push_fcs_fact(
        out,
        path.clone(),
        kind,
        fcs_impl_decl_range(kind, decl_fields),
        line_index,
    );
    collect_fcs_sub_facts(kind, decl_fields, &path, line_index, out);
    if kind == "NestedModule" {
        let nested = decl_fields[2]
            .as_array()
            .expect("SynModuleDecl.NestedModule decls must be array");
        for (i, child) in nested.iter().enumerate() {
            collect_fcs_impl_decl(child, format!("{path}.decl[{i}]"), line_index, out);
        }
    }
}

fn collect_fcs_sig_decl(
    decl: &Value,
    path: String,
    line_index: &LineIndex<'_>,
    out: &mut Vec<AstRangeFact>,
) {
    let kind = case_name(decl);
    let decl_fields = fields(decl);
    push_fcs_fact(
        out,
        path.clone(),
        kind,
        fcs_sig_decl_range(kind, decl_fields),
        line_index,
    );
    if kind == "NestedModule" {
        let nested = decl_fields[2]
            .as_array()
            .expect("SynModuleSigDecl.NestedModule decls must be array");
        for (i, child) in nested.iter().enumerate() {
            collect_fcs_sig_decl(child, format!("{path}.decl[{i}]"), line_index, out);
        }
    }
}

pub(super) fn fcs_impl_decl_range<'a>(kind: &str, fields: &'a [Value]) -> &'a Value {
    match kind {
        "Expr" | "Open" | "Types" | "Exception" | "Attributes" | "HashDirective" => &fields[1],
        "Let" | "ModuleAbbrev" => &fields[2],
        "NestedModule" => &fields[4],
        other => panic!("unsupported SynModuleDecl case for range audit: {other}"),
    }
}

pub(super) fn fcs_sig_decl_range<'a>(kind: &str, fields: &'a [Value]) -> &'a Value {
    match kind {
        "Open" | "Val" | "Types" | "Exception" | "HashDirective" => &fields[1],
        "ModuleAbbrev" => &fields[2],
        "NestedModule" => &fields[3],
        other => panic!("unsupported SynModuleSigDecl case for range audit: {other}"),
    }
}

fn push_fcs_fact(
    out: &mut Vec<AstRangeFact>,
    path: String,
    kind: &str,
    range: &Value,
    line_index: &LineIndex<'_>,
) {
    out.push(AstRangeFact {
        path,
        kind: kind.to_string(),
        range: fcs_byte_range(range, line_index),
    });
}

pub(super) fn fcs_byte_range(range: &Value, line_index: &LineIndex<'_>) -> Range<usize> {
    let start = range.get("Start").expect("FCS range missing Start");
    let end = range.get("End").expect("FCS range missing End");
    let start_line = start
        .get("Line")
        .and_then(Value::as_u64)
        .expect("FCS range Start.Line must be a number") as u32;
    let start_col = start
        .get("Col")
        .and_then(Value::as_u64)
        .expect("FCS range Start.Col must be a number") as u32;
    let end_line = end
        .get("Line")
        .and_then(Value::as_u64)
        .expect("FCS range End.Line must be a number") as u32;
    let end_col = end
        .get("Col")
        .and_then(Value::as_u64)
        .expect("FCS range End.Col must be a number") as u32;
    line_index.offset(start_line, start_col)..line_index.offset(end_line, end_col)
}

fn case_name(v: &Value) -> &str {
    v.get("Case")
        .and_then(Value::as_str)
        .expect("FCS union value missing Case string")
}

fn fields(v: &Value) -> &Vec<Value> {
    v.get("Fields")
        .and_then(Value::as_array)
        .expect("FCS union value missing Fields array")
}

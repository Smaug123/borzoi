//! `use rec` rejection — the resolver's first always-sound diagnostic.
//!
//! `use rec x = …` is syntactically decidable (a binding group carries both a
//! `use` and a `rec` keyword), so it needs no inference and can never be a
//! false positive. FCS reports the same as `FS0821`
//! (`tcBindingCannotBeUseAndRec`) during type-checking; we report it from the
//! resolver. These tests are FCS-free: the AST predicate
//! `is_use() && is_rec()` *is* the oracle.
//!
//! Two layers, mirroring `resolve_scoping.rs`:
//! * targeted examples pin the positive case, the negatives that must stay
//!   silent (`use`, `let rec`, `let`, `use!`), the single-diagnostic-per-group
//!   `and`-chain, and the keyword anchor range;
//! * a property generates random sequences of local `let`/`use`(`rec`?)
//!   bindings and asserts the diagnostic count equals the number of
//!   `use rec` groups — equivalently, the number of `LetOrUseExpr` nodes with
//!   `is_use() && is_rec()`.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile, LetOrUseExpr};
use borzoi_sema::{AssemblyEnv, ProjectItems, SemaDiagnostic, SemaDiagnosticKind, resolve_file};
use proptest::prelude::*;
use rowan::TextRange;

// ============================================================================
// Helpers
// ============================================================================

/// Parse `source` (asserting it parses cleanly — `use rec` is a *semantic*, not
/// a parse, error) and resolve it, returning the diagnostics in source order.
fn diagnostics(source: &str) -> Vec<SemaDiagnostic> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "unexpected parse errors for {source:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("root is an impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());
    resolved.diagnostics().to_vec()
}

/// Every `UseAndRec` diagnostic range in `source`, in source order.
fn use_and_rec_ranges(source: &str) -> Vec<TextRange> {
    diagnostics(source)
        .into_iter()
        .filter(|d| d.kind == SemaDiagnosticKind::UseAndRec)
        .map(|d| d.range)
        .collect()
}

/// Byte range of the `n`-th (0-based) whole-word occurrence of `needle`.
fn nth(source: &str, needle: &str, n: usize) -> TextRange {
    fn is_ident(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_' || b == b'\''
    }
    let bytes = source.as_bytes();
    let mut found = 0usize;
    let mut from = 0usize;
    while let Some(rel) = source[from..].find(needle) {
        let at = from + rel;
        let end = at + needle.len();
        let before_ok = at == 0 || !is_ident(bytes[at - 1]);
        let after_ok = end == bytes.len() || !is_ident(bytes[end]);
        if before_ok && after_ok {
            if found == n {
                return TextRange::new(
                    u32::try_from(at).unwrap().into(),
                    u32::try_from(end).unwrap().into(),
                );
            }
            found += 1;
        }
        from = end;
    }
    panic!("occurrence {n} of {needle:?} not found in {source:?}");
}

/// Count of `LetOrUseExpr` nodes that are both `use` and `rec` — the AST
/// predicate the diagnostic must track exactly.
fn use_rec_node_count(source: &str) -> usize {
    let parsed = parse(source);
    parsed
        .root
        .descendants()
        .filter_map(LetOrUseExpr::cast)
        .filter(|e| e.is_use() && e.is_rec())
        .count()
}

// ============================================================================
// Targeted examples
// ============================================================================

/// The positive case: an expression-level `use rec` produces exactly one
/// `UseAndRec` diagnostic, anchored at the `use` keyword.
#[test]
fn use_rec_is_rejected() {
    let source = "let outer () =\n    use rec x = x\n    x\n";
    let ranges = use_and_rec_ranges(source);
    assert_eq!(
        ranges,
        vec![nth(source, "use", 0)],
        "diag at the `use` keyword"
    );
}

/// `use` without `rec` is fine (FCS accepts it; only the `rec` makes it an
/// error).
#[test]
fn plain_use_is_accepted() {
    let source = "let outer () =\n    use x = x\n    x\n";
    assert!(use_and_rec_ranges(source).is_empty());
}

/// `let rec` without `use` is fine — recursion is exactly what `let rec` is for.
#[test]
fn let_rec_is_accepted() {
    let source = "let outer () =\n    let rec x = x\n    x\n";
    assert!(use_and_rec_ranges(source).is_empty());
}

/// Plain `let` is fine.
#[test]
fn plain_let_is_accepted() {
    let source = "let outer () =\n    let x = 1\n    x\n";
    assert!(use_and_rec_ranges(source).is_empty());
}

/// `use!` is the computation-expression bang binder — never recursive
/// (`is_rec()` is always false there), so it never trips the check even though
/// `is_use()` is true.
#[test]
fn use_bang_is_not_use_rec() {
    let source = "async {\n    use! x = e\n    return x\n}\n";
    assert!(use_and_rec_ranges(source).is_empty());
}

/// `use rec a = … and b = …` is one binding *group*, so it yields exactly one
/// diagnostic (FCS reports `FS0821` once, at the `LetOrUse`, not per binding).
#[test]
fn use_rec_and_chain_is_one_diagnostic() {
    let source = "let outer () =\n    use rec a = b\n    and b = a\n    a\n";
    let ranges = use_and_rec_ranges(source);
    assert_eq!(
        ranges,
        vec![nth(source, "use", 0)],
        "one diagnostic for the group"
    );
}

/// Two independent `use rec` groups (nested local lets) each diagnose.
#[test]
fn two_use_rec_groups_each_diagnose() {
    let source = "let outer () =\n    use rec x = x\n    use rec y = y\n    x\n";
    let ranges = use_and_rec_ranges(source);
    assert_eq!(ranges, vec![nth(source, "use", 0), nth(source, "use", 1)]);
}

// ============================================================================
// Property: the diagnostic tracks `is_use() && is_rec()` exactly
// ============================================================================

/// One local binding's keyword choice.
#[derive(Debug, Clone, Copy)]
struct Form {
    is_use: bool,
    is_rec: bool,
}

fn form_strategy() -> impl Strategy<Value = Form> {
    (any::<bool>(), any::<bool>()).prop_map(|(is_use, is_rec)| Form { is_use, is_rec })
}

/// Render a sequence of local bindings inside a function body, e.g.
/// `let outer () =\n    use rec n0 = 1\n    let n1 = 1\n    1\n`.
fn render(forms: &[Form]) -> String {
    let mut s = String::from("let outer () =\n");
    for (i, f) in forms.iter().enumerate() {
        let kw = if f.is_use { "use" } else { "let" };
        let rec = if f.is_rec { "rec " } else { "" };
        s.push_str(&format!("    {kw} {rec}n{i} = 1\n"));
    }
    s.push_str("    1\n");
    s
}

proptest! {
    /// For any sequence of local `let`/`use`(`rec`?) bindings, the number of
    /// `UseAndRec` diagnostics equals the number of `use rec` bindings — and
    /// that, in turn, equals the number of `LetOrUseExpr` AST nodes with
    /// `is_use() && is_rec()`. The diagnostic is exactly the syntactic predicate.
    #[test]
    fn diagnostic_count_tracks_use_rec(forms in prop::collection::vec(form_strategy(), 1..5)) {
        let source = render(&forms);
        let expected = forms.iter().filter(|f| f.is_use && f.is_rec).count();

        prop_assert_eq!(use_rec_node_count(&source), expected, "AST predicate baseline");
        prop_assert_eq!(use_and_rec_ranges(&source).len(), expected, "diagnostic count");
    }
}

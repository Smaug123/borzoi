//! Recovered-tree diffs: inputs both parsers reject, compared whole
//! (`assert_asts_match_allow_errors`), recovery placeholders included.
//!
//! The `foo.` group is the shape an editor buffer is in while a member access
//! is being typed, and so the one completion is driven by. FCS recovers it as
//! `SynExpr.DiscardAfterMissingQualificationAfterDot(receiver)` and keeps
//! parsing; every case here also requires the declaration after it to survive.

use std::collections::BTreeMap;

use crate::common::assert_asts_match_allow_errors;
use crate::common::recovery::{Relation, layout_grid, our_units, recovered_verdict};

/// `foo.` — a one-segment receiver. FCS keeps it as `Ident`, not a one-segment
/// `LongIdent`.
#[test]
fn diff_ast_dot_missing_after_ident() {
    assert_asts_match_allow_errors("let x = foo.\nlet y = 2\n");
}

/// `A.B.` — a dotted receiver.
#[test]
fn diff_ast_dot_missing_after_long_ident() {
    assert_asts_match_allow_errors("let x = A.B.\nlet y = 2\n");
}

/// `(f x).` — a parenthesised receiver.
#[test]
fn diff_ast_dot_missing_after_paren() {
    assert_asts_match_allow_errors("let x = (f x).\nlet y = 2\n");
}

/// `foo.Bar(1).` — a method-call receiver.
#[test]
fn diff_ast_dot_missing_after_method_call() {
    assert_asts_match_allow_errors("let x = foo.Bar(1).\nlet y = 2\n");
}

/// `"hi".Length.` — a member-access receiver off a literal.
#[test]
fn diff_ast_dot_missing_after_dot_get() {
    assert_asts_match_allow_errors("let x = \"hi\".Length.\nlet y = 2\n");
}

/// `xs.[0].` — an indexer receiver.
#[test]
fn diff_ast_dot_missing_after_indexer() {
    assert_asts_match_allow_errors("let x = xs.[0].\nlet y = 2\n");
}

/// A `foo.` statement in a sequential block, the rest of the block intact.
#[test]
fn diff_ast_dot_missing_in_sequential_block() {
    assert_asts_match_allow_errors("let f () =\n    foo.\n    1\nlet y = 2\n");
}

/// A `foo.` as an application argument.
#[test]
fn diff_ast_dot_missing_as_argument() {
    assert_asts_match_allow_errors("let x = f foo.\nlet y = 2\n");
}

/// A `foo.` as the module-level expression itself.
#[test]
fn diff_ast_dot_missing_as_module_expr() {
    assert_asts_match_allow_errors("foo.\nlet y = 2\n");
}

/// The beyond-FCS exemption anchors a module's offside column on a declaration
/// both parsers agree on, never on one of ours that FCS does not have. Here FCS
/// keeps only `N`'s header, and our only unit inside `N` is the spill of a stray
/// `member` (`this.Foo`, off the body's column); anchoring on it would align it
/// with itself and pass it as agreement.
#[test]
fn recovery_oracle_does_not_anchor_the_offside_column_on_an_unconfirmed_unit() {
    let verdict =
        crate::common::recovery::recovered_verdict("module M\nmodule N =\n    member this.Foo\n");
    assert_eq!(
        verdict.relation,
        crate::common::recovery::Relation::Divergent,
        "{}",
        verdict.token()
    );
}

/// A file-start BOM is not indentation: `this.Foo`, three columns into line 2,
/// is off the anonymous module's offside line (column 0 of line 1, after the
/// BOM), however wide the BOM is in bytes.
#[test]
fn recovery_oracle_does_not_count_a_bom_as_indentation() {
    let verdict = recovered_verdict("\u{FEFF}member\n   this.Foo\n");
    assert_eq!(verdict.relation, Relation::Divergent, "{}", verdict.token());
}

/// A backward line directive maps the error before it to two lines, one of
/// them past the end of the file: only the one in the file is damage, so `let
/// good` after the directive is still compared.
#[test]
fn recovery_oracle_maps_diagnostics_back_only_into_the_file() {
    crate::common::recovery::assert_recovered_trees_agree(
        "module M\nlet bad = (1 + )\n# 0 \"gen.fs\"\nlet good = 2\n",
        2,
    );
}

/// Over the layout grid ([`layout_grid`]), graded against FCS. FCS keeps
/// nothing of a stray `member this.Foo`, and its `this.Foo` sits off the body's
/// offside line, so the trees diverge exactly when our tree has a unit starting
/// there, however the body is wrapped. And a doc comment or an attribute on the
/// declaration after it moves nothing in the verdict, on whatever column the
/// comment sits.
#[test]
fn recovery_verdicts_follow_the_generated_layout() {
    let mut failures = Vec::new();
    let mut families: BTreeMap<String, (String, String)> = BTreeMap::new();
    for case in layout_grid() {
        let verdict = recovered_verdict(&case.source);
        let token = verdict.token();
        let debris = case.spill.is_some_and(|at| {
            our_units(&borzoi_cst::parser::parse(&case.source))
                .iter()
                .any(|u| u.key.start == at)
        });
        if (verdict.relation == Relation::Divergent) != debris {
            failures.push(format!(
                "{:?}: {token} {}",
                case.source,
                verdict.first_divergence.as_deref().unwrap_or("")
            ));
        }
        let (first, first_token) = families
            .entry(case.family.clone())
            .or_insert_with(|| (case.source.clone(), token.clone()));
        if *first_token != token {
            failures.push(format!(
                "{:?}: {token}, but {first:?}: {first_token}",
                case.source
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

//! Differential test (`parser::parse` vs FCS): a type definition's
//! **parenthesised prefix** type parameters, `type ('a, 'b) T` — FCS's
//! `prefixTyparDecls: LPAREN typarDeclList rparen` (`SynTyparDecls.PrefixList`),
//! the OCaml-compatible spelling of `type T<'a, 'b>`. FCS accepts it without a
//! warning, with or without `--mlcompatibility` and at every language version,
//! in implementation and signature files alike, wherever a type header may
//! stand (`type`, `and`, nested modules); `exception` takes no type parameters.
//!
//! The normalised AST carries the `SynTyparDecls` case
//! ([`NormalisedTyparForm`](crate::common::normalised_ast::NormalisedTyparForm)),
//! so `('a) T`, `'a T` and `T<'a>` — the same parameter list — are told apart.
//!
//! The hand-written fixtures name the shapes; [`diff_prefix_typar_grid`]
//! generates the list contents, separators and layouts, the headers and bodies
//! around them, and malformed lists, and leaves the verdict to FCS.

use crate::common::{
    assert_asts_match, assert_filtered_streams_match, assert_parse_verdicts_match,
    assert_sig_asts_match,
};
use borzoi_oracle_harness::panic_silence::{catch_unwind_silent, take_silenced_panic};

/// Two parameters.
#[test]
fn diff_prefix_list_two() {
    assert_asts_match("type ('a, 'b) T = 'a\n");
}

/// One parameter in parentheses is still a `PrefixList`, not a
/// `SinglePrefix`.
#[test]
fn diff_prefix_list_single() {
    assert_asts_match("type ('a) T = 'a\n");
}

/// Unspaced, three parameters, the name adjacent to the `)`.
#[test]
fn diff_prefix_list_three_unspaced() {
    assert_asts_match("type ('a,'b,'c)T = 'a * 'b * 'c\n");
}

/// A head-type (`^a`) parameter and a quoted-identifier one.
#[test]
fn diff_prefix_list_head_type_and_quoted() {
    assert_asts_match("type (^a, '``b c``) T = int\n");
}

/// Attributes on the parameters (`SynTyparDecl.attributes`), first and later.
#[test]
fn diff_prefix_list_attributed_typars() {
    assert_asts_match("type ([<Measure>] 'a, 'b) T = 'b\n");
    assert_asts_match("type ('a, [<A>] 'b) T = int\n");
}

/// An intersection constraint on a parameter (`'a & #I`), as in the postfix
/// list.
#[test]
fn diff_prefix_list_intersection_constraint() {
    assert_asts_match("type ('a & #System.IDisposable, 'b) T = 'b\n");
}

/// A `when` clause follows the name (`SynComponentInfo.constraints`); the
/// prefix list has no inside-the-parens clause.
#[test]
fn diff_prefix_list_when_after_name() {
    assert_asts_match("type ('a, 'b) T when 'a : equality = 'a\n");
}

/// The header's own accessibility precedes the list; header attributes precede
/// that.
#[test]
fn diff_prefix_list_access_and_attributes() {
    assert_asts_match("type internal ('a, 'b) T = 'a\n");
    assert_asts_match("[<AbstractClass>]\ntype private ('a, 'b) T() = class end\n");
    assert_asts_match("type [<A>] ('a, 'b) T = int\n");
}

/// The list before a dotted name.
#[test]
fn diff_prefix_list_dotted_name() {
    assert_asts_match("type ('a, 'b) A.T = int\n");
}

/// Reprs after the header: union, record (in a nested module), delegate,
/// class with a primary constructor, augmentation, and none.
#[test]
fn diff_prefix_list_reprs() {
    assert_asts_match("type ('a,'b) T = A of 'a | B of 'b\n");
    assert_asts_match("module M =\n    type ('a, 'b) T = { X : 'a; Y : 'b }\n");
    assert_asts_match("type ('a, 'b) T = delegate of 'a -> 'b\n");
    assert_asts_match("type ('a, 'b) T() =\n    member _.M (x: 'a) = x\n");
    assert_asts_match("type ('a, 'b) T with\n    member _.M = 1\n");
    assert_asts_match("type ('a, 'b) T\n");
    assert_asts_match("type ('a, 'b) T =\n    int\n");
}

/// `and`-chained definitions with prefix lists, at either end of the chain.
#[test]
fn diff_prefix_list_and_chain() {
    assert_asts_match("type T = int\nand ('a, 'b) U = 'a\n");
    assert_asts_match("type ('a, 'b) T =\n    | A of 'a\n    | B of 'b\nand ('c) U = 'c T\n");
    assert_asts_match("type ('a, 'b) T = 'a\n    and U = int\n");
}

/// The list spread over lines, the continuation either after the comma (at the
/// first parameter's column) or before it.
#[test]
fn diff_prefix_list_multiline() {
    assert_asts_match("type ('a,\n      'b) T = 'a\n");
    assert_asts_match("type ('a\n  , 'b) T = int\n");
    assert_asts_match("type ( 'a , 'b ) T = 'a\n");
}

/// The token stream FCS's parser reads around a prefix list, multi-line
/// layouts included: the `)` is swallowed, and no offside virtual lands inside
/// the parentheses.
#[test]
fn diff_prefix_list_filtered_streams() {
    for source in [
        "type ('a, 'b) T = 'a\n",
        "type ('a,\n      'b) T = 'a\n",
        "type ('a\n  , 'b) T = int\n",
        "type T = int\nand ('a, 'b) U = 'a\n",
    ] {
        assert_filtered_streams_match(source);
    }
}

/// Signature files share the header.
#[test]
fn diff_prefix_list_signature() {
    assert_sig_asts_match(
        "module M\ntype ('a, 'b) T = 'a\ntype ('a, 'b) U =\n    abstract M : 'a -> 'b\n",
    );
    assert_sig_asts_match("namespace N\ntype ('a, 'b) T\n");
    assert_sig_asts_match("namespace N\ntype ('a) Box = { Value : 'a }\n");
}

/// The single-prefix and postfix forms keep their own `SynTyparDecls` case.
#[test]
fn diff_other_typar_forms_keep_their_case() {
    assert_asts_match("type 'a T = 'a list\n");
    assert_asts_match("type T<'a> = 'a list\n");
}

/// Shapes FCS rejects; we must reject them too.
#[test]
fn diff_prefix_list_rejections() {
    for source in [
        "type () T = int\n",
        "type ('a, ) T = int\n",
        "type ('a 'b) T = int\n",
        "type ('a, 'b T = int\n",
        "type ('a * 'b) with\n    member _.M = 1\n",
        "type ('a, 'b) T<'c> = int\n",
        "exception ('a) E of 'a\n",
        "type struct ('a, 'b) T = int\n",
    ] {
        assert!(
            !assert_parse_verdicts_match(source),
            "FCS was expected to reject {source:?}",
        );
    }
}

/// Typar declarations a list element may be. Each is a whole `typarDecl`.
const ELEMENTS: &[&str] = &[
    "'a",
    "^b",
    "[<M>] 'c",
    "'``d e``",
    "'f & #System.IDisposable",
];

/// What separates two elements: spacing, and a line break on either side of
/// the comma.
const SEPARATORS: &[&str] = &[", ", ",", " , ", ",\n      ", "\n    , "];

/// What may precede the list after `type`.
const HEADS: &[&str] = &[
    "type ",
    "type internal ",
    "type [<A>] ",
    "type T0 = int\nand ",
];

/// What may follow the list.
const TAILS: &[&str] = &[
    " T = int\n",
    " T\n",
    " T() =\n    member _.M = 1\n",
    " T when 'a : equality = int\n",
    "T = int\n",
    " A.T = int\n",
];

/// Malformed or out-of-grammar lists, each inside `type … T = int`.
const MALFORMED: &[&str] = &[
    "()",
    "( )",
    "('a, )",
    "(, 'a)",
    "('a,, 'b)",
    "('a 'b)",
    "('a",
    "('a, 'b",
    "('a * 'b)",
    "(int)",
    "('a when 'a : equality)",
    "('a) ('b)",
    "('a) 'b",
    "'a ('b)",
    "(('a))",
    "('a; 'b)",
    "([<M>])",
];

/// Every grid cell. Each list of one or two elements in every separator, with
/// a plain head and tail; then every head with every tail around a fixed list;
/// then the malformed lists.
fn cells() -> Vec<String> {
    let mut lists: Vec<String> = ELEMENTS.iter().map(|e| format!("({e})")).collect();
    for a in ELEMENTS {
        for b in ELEMENTS {
            for sep in SEPARATORS {
                lists.push(format!("({a}{sep}{b})"));
            }
        }
    }
    let mut out: Vec<String> = lists
        .iter()
        .map(|list| format!("type {list} T = int\n"))
        .collect();
    for head in HEADS {
        for tail in TAILS {
            out.push(format!("{head}('a, 'b){tail}"));
        }
    }
    for list in MALFORMED {
        out.push(format!("type {list} T = int\n"));
    }
    out
}

/// The generated grid under the verdict oracle: a cell we wrongly reject,
/// wrongly accept, or accept with a divergent tree fails. Divergences are
/// collected rather than stopping at the first.
#[test]
fn diff_prefix_typar_grid() {
    let mut total = 0;
    let mut accepted = 0;
    let mut failures = Vec::new();
    for source in cells() {
        total += 1;
        match catch_unwind_silent(|| assert_parse_verdicts_match(&source)) {
            Ok(verdict) => accepted += usize::from(verdict),
            Err(_) => {
                let message = take_silenced_panic().map_or_else(String::new, |p| p.message);
                failures.push(message.lines().next().unwrap_or_default().to_owned());
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} prefix-typar cells diverge from FCS:\n{}",
        failures.len(),
        failures.join("\n"),
    );
    // All-accept or all-reject would pass while testing nothing about the
    // other verdict.
    assert!(
        accepted > 0 && accepted < total,
        "the prefix-typar grid should both accept and reject, but accepted {accepted} of {total}",
    );
}

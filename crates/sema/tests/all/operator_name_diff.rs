//! Operator names against FCS: one name, two spellings.
//!
//! FCS's parser stores an operator's *compiled* name in the identifier — `+`
//! is `op_Addition` wherever it is written — so its name environment holds
//! one entry that both spellings reach. `let (+) a b = a` is what
//! `op_Addition 1 2` calls, `let op_Addition a b = a` is what `1 + 2` calls,
//! and with neither in scope `1 + 2` is FSharp.Core's `Operators.(+)`. A
//! quoted `` ``+`` `` is a third, *different* name: the identifier `+`.
//!
//! The space is generated rather than curated: every operator, in every
//! definition form, used in every form, each in a module of its own so no
//! two interfere. FCS is the oracle, run over the same text with FSharp.Core
//! and the BCL referenced on both sides. Two properties, checked separately:
//!
//! - **certain-implies-exact**: wherever we commit a resolution at an operator
//!   or an `op_*` name, FCS reports a use at exactly that range naming the same
//!   declaration — the in-file binder, or the assembly member by its logical
//!   name and declaring entity;
//! - **no silence where it is owed**: wherever FCS binds an infix, `( op )` or
//!   `op_*` use to an in-file definition, we commit; and FSharp.Core's
//!   operators are reached.
//!
//! A *prefix* application (`-x`) is the one form we decline: FCS names it
//! `~-`, after the position, and the resolver leaves it deferred.

use std::collections::HashMap;

use crate::common::{NormalisedUse, full_bcl_env, invoke_fcs_dump, parse_fcs_uses, temp_fs_file};
use borzoi_assembly::op_name::operator_display_name;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, DeferredReason, ProjectItems, Resolution, ResolvedFile, SyntaxRecovery,
    resolve_file,
};
use rowan::TextRange;

/// FSharp.Core's binary operators with a dedicated compiled name, each usable
/// infix between two unannotated parameters.
const TABLE_OPS: &[&str] = &[
    "+", "-", "*", "/", "%", "**", "@", "^", "&&&", "|||", "^^^", "<<<", ">>>", "&&", "||", "<=",
    "=", "<>", ">=", "<", ">", "|>", "||>", "<|", "<||", ">>", "<<", ":=",
];

/// Operators with no table entry, whose compiled name spells each character.
const CUSTOM_OPS: &[&str] = &[
    "+.", ">>=", "<*>", "<!>", "|>>", "<+>", "=>>", "<|>", "-->", "%%", "^.", "@@", "**.", "$",
    "<&>", "/%", ">=>",
];

fn all_ops() -> impl Iterator<Item = &'static str> {
    TABLE_OPS.iter().chain(CUSTOM_OPS).copied()
}

/// One generated file and what FCS and we each made of it.
struct Checked {
    src: String,
    uses: Vec<NormalisedUse>,
    resolved: ResolvedFile,
}

fn check(label: &str, src: String) -> Checked {
    let env = full_bcl_env();
    let parsed = parse(&src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors in generated {label}: {:?}\n{src}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let resolved = resolve_file(&file, &ProjectItems::default(), env, &recovery);
    let path = temp_fs_file(label, &src);
    let json = invoke_fcs_dump("uses", &path);
    let _ = std::fs::remove_file(&path);
    let uses = parse_fcs_uses(&json, &src);
    Checked {
        src,
        uses,
        resolved,
    }
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// The compiled name FCS gives each operator, read off its own definition
/// sites rather than re-derived: the oracle supplies the `op_*` spellings the
/// other scenarios write.
fn compiled_names() -> HashMap<&'static str, String> {
    let mut src = String::from("module Defs\n");
    for (i, op) in all_ops().enumerate() {
        src.push_str(&format!("module M{i} =\n    let ( {op} ) a b = a\n"));
    }
    let c = check("op_defs", src);
    let mut names = HashMap::new();
    for (i, op) in all_ops().enumerate() {
        let header = format!("module M{i} =\n    let ( {op} )");
        let at = c.src.find(&header).expect("generated header") + header.len() - 2 - op.len();
        let u = c
            .uses
            .iter()
            .find(|u| u.is_from_definition && u.start == at)
            .unwrap_or_else(|| panic!("FCS reports no definition of ( {op} )"));
        let logical = u.logical_name.clone().expect("a value has a logical name");
        assert!(logical.starts_with("op_"), "( {op} ) compiles to {logical}");
        names.insert(op, logical);
    }
    names
}

/// Whether `text` is a use site this harness grades: an operator spelling, an
/// `op_*` name, or a quoted operator spelling (`` ``+`` ``).
fn is_operator_site(text: &str) -> bool {
    let text = text
        .strip_prefix("``")
        .and_then(|t| t.strip_suffix("``"))
        .unwrap_or(text);
    // A qualified path (`op_Addition.w`) is graded segment by segment by FCS,
    // so only a single `op_*` identifier is a site here.
    (text.starts_with("op_") && text.chars().all(|c| c.is_alphanumeric() || c == '_'))
        || (!text.is_empty() && text.chars().all(|c| "!$%&*+-./<=>?@^|~:".contains(c)))
}

/// The declaring entity's full name FCS renders for a member, with the
/// member's own rendering (`.(+)`) removed.
fn fcs_declaring(u: &NormalisedUse) -> Option<&str> {
    u.full_name
        .as_deref()?
        .strip_suffix(&format!(".{}", u.name))
}

#[derive(Default, Debug)]
struct Tally {
    in_file_agreed: usize,
    assembly_agreed: usize,
    prefix_declined: usize,
}

/// Certain-implies-exact over every operator site we commit at, and the
/// owed-commit property over every in-file binding FCS reports.
fn assert_agrees(c: &Checked, env: &AssemblyEnv, tally: &mut Tally) {
    let by_range: HashMap<(usize, usize), &NormalisedUse> = c
        .uses
        .iter()
        .filter(|u| !u.is_from_definition && u.start < u.end)
        .map(|u| ((u.start, u.end), u))
        .collect();
    for (&range, &res) in c.resolved.resolutions() {
        let (s, e) = (usize::from(range.start()), usize::from(range.end()));
        let text = &c.src[s..e];
        if !is_operator_site(text) {
            continue;
        }
        if matches!(res, Resolution::Deferred(DeferredReason::PrefixOperator)) {
            tally.prefix_declined += 1;
            continue;
        }
        // A binder's own occurrence is a definition, not a use.
        if c.resolved
            .resolved_def(res)
            .is_some_and(|d| d.range == range)
        {
            continue;
        }
        let committed = matches!(
            res,
            Resolution::Local(_)
                | Resolution::Item(_)
                | Resolution::Member { .. }
                | Resolution::Entity(_)
        );
        if !committed {
            continue;
        }
        let Some(u) = by_range.get(&(s, e)) else {
            panic!(
                "we commit {res:?} at {text:?} {range:?}, where FCS reports no use\n{}",
                c.src
            );
        };
        match res {
            Resolution::Local(_) | Resolution::Item(_) => {
                let def = c.resolved.resolved_def(res).expect("an in-file def");
                assert_eq!(
                    u.decl.map(|(a, b)| span(a, b)),
                    Some(def.range),
                    "{text:?} at {range:?}: we bind the binder at {:?}, FCS binds {:?} ({:?})\n{}",
                    def.range,
                    u.decl,
                    u.full_name,
                    c.src
                );
                tally.in_file_agreed += 1;
            }
            Resolution::Member { parent, idx } => {
                assert_eq!(
                    u.decl, None,
                    "{text:?} at {range:?}: we bind an assembly member, FCS an in-file binder\n{}",
                    c.src
                );
                assert_eq!(
                    u.logical_name.as_deref(),
                    Some(env.member_display_name(parent, idx)),
                    "{text:?} at {range:?}: we bind {}, FCS {:?}\n{}",
                    env.member_display_name(parent, idx),
                    u.full_name,
                    c.src
                );
                assert_eq!(
                    fcs_declaring(u),
                    Some(env.entity_full_name(parent).as_str()),
                    "{text:?} at {range:?}: declared in a different entity\n{}",
                    c.src
                );
                tally.assembly_agreed += 1;
            }
            other => panic!(
                "{text:?} at {range:?}: we commit {other:?}, FCS {u:?}\n{}",
                c.src
            ),
        }
    }
    // Hover shows an operator as FCS does: its logical name, decompiled.
    for u in &c.uses {
        if let Some(logical) = u.logical_name.as_deref().filter(|l| l.starts_with("op_")) {
            assert_eq!(
                operator_display_name(logical).as_deref(),
                Some(u.name.as_str()),
                "FCS shows {logical} as {:?}",
                u.name
            );
        }
    }
    // Owed: every non-prefix operator site FCS binds in-file, we bind too.
    for u in by_range.values() {
        let text = &c.src[u.start..u.end];
        if u.decl.is_none() || !is_operator_site(text) {
            continue;
        }
        // A module qualifier (`` ``+``.v ``) is not modelled as a resolution
        // of its own, operator or not: the head of a path records
        // `QualifiedAccess`.
        if u.kind.as_deref() == Some("module") {
            continue;
        }
        // `( * )` parses as a parenthesised index-range wildcard
        // (`PAREN_EXPR > INDEX_RANGE_EXPR`), where FCS reads the operator
        // value: a parser divergence, so the resolver never sees the name.
        if text == "*" && c.src[..u.start].ends_with("( ") {
            continue;
        }
        let res = c.resolved.resolution_at(span(u.start, u.end));
        assert!(
            matches!(res, Some(Resolution::Local(_) | Resolution::Item(_))),
            "FCS binds {text:?} at {}..{} in-file, we gave {res:?}\n{}",
            u.start,
            u.end,
            c.src
        );
    }
}

/// Each operator defined by its spelling, then used infix, as a value, and by
/// its compiled name — all three reach the one definition.
#[test]
fn a_spelled_definition_answers_to_both_names() {
    let names = compiled_names();
    let mut src = String::from("module Spelled\n");
    for (i, op) in all_ops().enumerate() {
        let compiled = &names[op];
        src.push_str(&format!(
            "module M{i} =\n    let ( {op} ) a b = a\n    let infix a b = a {op} b\n    \
             let paren a b = ( {op} ) a b\n    let compiled a b = {compiled} a b\n"
        ));
    }
    let mut tally = Tally::default();
    assert_agrees(&check("op_spelled", src), full_bcl_env(), &mut tally);
    // Three in-file uses per operator, less `( * )` (see `assert_agrees`).
    assert_eq!(tally.in_file_agreed, 3 * all_ops().count() - 1, "{tally:?}");
}

/// Each operator defined by its compiled name, then used by its spelling.
#[test]
fn a_compiled_definition_answers_to_the_spelling() {
    let names = compiled_names();
    let mut src = String::from("module Compiled\n");
    for (i, op) in all_ops().enumerate() {
        let compiled = &names[op];
        src.push_str(&format!(
            "module M{i} =\n    let {compiled} a b = a\n    let infix a b = a {op} b\n    \
             let paren a b = ( {op} ) a b\n    let compiled a b = {compiled} a b\n"
        ));
    }
    let mut tally = Tally::default();
    assert_agrees(&check("op_compiled", src), full_bcl_env(), &mut tally);
    assert_eq!(tally.in_file_agreed, 3 * all_ops().count() - 1, "{tally:?}");
}

/// A quoted `` ``+`` `` is the identifier `+`, not the operator: the infix and
/// compiled uses reach FSharp.Core (or nothing), only the quoted use reaches
/// the quoted definition.
#[test]
fn a_quoted_definition_is_a_different_name() {
    let names = compiled_names();
    let mut src = String::from("module Quoted\n");
    for (i, op) in all_ops().enumerate() {
        let compiled = &names[op];
        src.push_str(&format!(
            "module M{i} =\n    let ``{op}`` a b = a\n    let infix a b = a {op} b\n    \
             let quoted a b = ``{op}`` a b\n    let compiled a b = {compiled} a b\n"
        ));
    }
    let mut tally = Tally::default();
    assert_agrees(&check("op_quoted", src), full_bcl_env(), &mut tally);
    // Every quoted use binds its own definition; the table operators' infix and
    // compiled uses reach FSharp.Core.
    assert_eq!(tally.in_file_agreed, all_ops().count(), "{tally:?}");
    assert!(
        tally.assembly_agreed >= 2 * TABLE_OPS.len() - 4,
        "{tally:?}"
    );
}

/// With nothing in-file, the table operators are FSharp.Core's, by either
/// spelling.
#[test]
fn fsharp_core_operators_resolve_by_either_name() {
    let names = compiled_names();
    let mut src = String::from("module Core\n");
    for (i, op) in TABLE_OPS.iter().enumerate() {
        let compiled = &names[op];
        src.push_str(&format!(
            "module M{i} =\n    let infix a b = a {op} b\n    \
             let paren a b = ( {op} ) a b\n    let compiled a b = {compiled} a b\n"
        ));
    }
    let mut tally = Tally::default();
    assert_agrees(&check("op_core", src), full_bcl_env(), &mut tally);
    // Measured: 77 of the 84 sites. The floor guards against losing them.
    assert!(
        tally.assembly_agreed >= 77,
        "too few FSharp.Core agreements: {tally:?}"
    );
}

/// An in-file operator opened into scope, then FSharp.Core's `Operators`
/// reopened over it: the latest open wins, whichever spelling defined it.
#[test]
fn a_reopened_core_module_reshadows_an_opened_definition() {
    let names = compiled_names();
    let mut src = String::from("module Reopen\n");
    for (i, op) in TABLE_OPS.iter().enumerate() {
        let compiled = &names[op];
        src.push_str(&format!(
            "module D{i} =\n    let ( {op} ) a b = a\n\
             module E{i} =\n    let {compiled} a b = a\n\
             module U{i} =\n    open D{i}\n    let viaSpelling a b = a {op} b\n    \
             open E{i}\n    let viaCompiled a b = a {op} b\n    \
             open Microsoft.FSharp.Core.Operators\n    let viaCore a b = a {op} b\n"
        ));
    }
    let mut tally = Tally::default();
    assert_agrees(&check("op_reopen", src), full_bcl_env(), &mut tally);
    // Both opened definitions bind, and the reopened `Operators` takes every
    // operator back but `&&` and `||`: FSharp.Core declares those in
    // `LanguagePrimitives.IntrinsicOperators`, so the in-file one still wins.
    assert_eq!(tally.in_file_agreed, 2 * TABLE_OPS.len() + 2, "{tally:?}");
    assert_eq!(tally.assembly_agreed, TABLE_OPS.len() - 2, "{tally:?}");
}

/// A prefix application names its operator after the position (`-x` is
/// `~-`), so it declines — never the binary operator its token spells.
#[test]
fn a_prefix_operator_declines() {
    let src =
        "module Prefix\nlet neg (x: int) = - x\nlet neg2 (x: int) = -x\nlet pos (x: int) = +x\n\
               let deref (r: int ref) = !r\nlet lnot (x: int) = ~~~x\nlet fneg (x: float) = -.x\n\
               let lit = -1\nlet ( - ) a b = a\nlet shadowed (x: int) = -x\n"
            .to_string();
    let mut tally = Tally::default();
    assert_agrees(&check("op_prefix", src), full_bcl_env(), &mut tally);
    assert_eq!(tally.prefix_declined, 7, "{tally:?}");
}

/// A quoted operator spelling stays the identifier it quotes in every
/// namespace, beside a declaration under the compiled name: a type, a module,
/// a union case and a value each named `` ``+`` `` are not `op_Addition`.
/// Normalising a name that is already canonical a second time would compile
/// `+` to `op_Addition` and bind the wrong one.
#[test]
fn a_quoted_operator_name_is_never_its_compiled_name() {
    let names = compiled_names();
    let mut src = String::from("module QuotedNames\n");
    for (i, op) in all_ops().enumerate() {
        let compiled = &names[op];
        src.push_str(&format!(
            "module T{i} =\n    type {compiled} = {{ a: int }}\n    type ``{op}`` = {{ b: int }}\n    \
             let viaQuoted (x: ``{op}``) = x\n    let viaCompiled (x: {compiled}) = x\n\
             module ``{op}`` =\n    let v{i} = {i}\n\
             module {compiled} =\n    let w{i} = {i}\n\
             module U{i} =\n    let viaQuoted = ``{op}``.v{i}\n    let viaCompiled = {compiled}.w{i}\n    \
             let ``{op}`` = 1\n    let {compiled} = 2\n    let quotedValue = ``{op}``\n    \
             let compiledValue = {compiled}\n"
        ));
    }
    let c = check("op_quoted_names", src);
    let mut tally = Tally::default();
    assert_agrees(&c, full_bcl_env(), &mut tally);
    // Every quoted and compiled use above binds in-file, FCS-checked; and none
    // of ours points at the other spelling's declaration.
    let mut graded = 0;
    for u in c
        .uses
        .iter()
        .filter(|u| !u.is_from_definition && u.decl.is_some())
    {
        let text = &c.src[u.start..u.end];
        if !is_operator_site(text) {
            continue;
        }
        let res = c.resolved.resolution_at(span(u.start, u.end));
        if let Some(def) = res.and_then(|r| c.resolved.resolved_def(r)) {
            assert_eq!(
                Some(def.range),
                u.decl.map(|(a, b)| span(a, b)),
                "{text:?} at {}..{}: we bind {:?}, FCS {:?}\n{}",
                u.start,
                u.end,
                def.name,
                u.full_name,
                c.src
            );
            graded += 1;
        }
    }
    assert!(
        graded >= 4 * all_ops().count(),
        "graded {graded}: {tally:?}"
    );
}

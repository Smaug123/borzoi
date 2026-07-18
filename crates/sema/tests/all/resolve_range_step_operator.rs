//! Direct (FCS-free) tests for **range-step operator name resolution**: a
//! `let (.. ..) … = …` definition binds the operator `op_RangeStep` under FCS's
//! fixed canonical notation `.. ..` (regardless of the inter-dot layout the
//! source uses), and a `(.. ..)` reference resolves to it.
//!
//! The load-bearing case is *cross-layout* resolution: because the name is a
//! `RANGE_STEP_OP` node canonicalised by its presence (never its layout-dependent
//! text), a glued `let (....)` definition and a spaced `(.. ..)` reference name
//! the same value — where a source-text key would record `....` and look up
//! `.. ..`, leaving the reference unbound.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, DefKind, ProjectItems, ResolvedFile, resolve_file};
use rowan::TextRange;

fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
}

/// The byte range of the first occurrence of `needle` in `src`.
fn range_of(src: &str, needle: &str) -> TextRange {
    let i = src.find(needle).expect("occurrence");
    TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + needle.len()).unwrap().into(),
    )
}

/// Assert the `use_spelling` occurrence of the range-step operator resolves to an
/// in-file def whose range is the `def_spelling` occurrence, with `kind`.
fn assert_range_step_resolves(src: &str, use_spelling: &str, def_spelling: &str, kind: DefKind) {
    let rf = resolve(src);
    let use_range = range_of(src, use_spelling);
    let res = rf
        .resolution_at(use_range)
        .unwrap_or_else(|| panic!("no resolution at the `{use_spelling}` use in {src:?}"));
    let def = rf
        .resolved_def(res)
        .unwrap_or_else(|| panic!("the `{use_spelling}` use names no in-file def in {src:?}"));
    assert_eq!(
        def.range,
        range_of(src, def_spelling),
        "the `{use_spelling}` use points at the wrong def in {src:?}"
    );
    assert_eq!(def.kind, kind, "wrong def kind in {src:?}");
}

/// A `(.. ..)` reference resolves to a same-layout `let (.. ..)` function def.
#[test]
fn spaced_reference_resolves_to_spaced_function_def() {
    // The def head `(.. ..)` and the use `(.. ..)` both span `.. ..`; the use
    // occurrence is the second (`find` from the def would collide), so give the
    // reference its own surrounding text.
    let src = "let (.. ..) a b = a\nlet z = op (.. ..)\n";
    // The def's `.. ..` is the first occurrence; the use's is the second.
    let rf = resolve(src);
    let def_range = range_of(src, ".. ..");
    let use_range = {
        let first = src.find(".. ..").unwrap();
        let second = src[first + 1..].find(".. ..").unwrap() + first + 1;
        TextRange::new(
            u32::try_from(second).unwrap().into(),
            u32::try_from(second + ".. ..".len()).unwrap().into(),
        )
    };
    let res = rf.resolution_at(use_range).expect("use resolves");
    let def = rf.resolved_def(res).expect("names an in-file def");
    assert_eq!(def.range, def_range, "use points at the def");
    assert_eq!(def.kind, DefKind::Value { is_function: true });
}

/// **Cross-layout** — a spaced `(.. ..)` reference resolves to a *glued*
/// `let (....)` function def. The regression guard for the layout-independent,
/// node-keyed canonical name: a source-text key would leave the reference unbound.
#[test]
fn spaced_reference_resolves_to_glued_def() {
    let src = "let (....) a b = a\nlet z = op (.. ..)\n";
    assert_range_step_resolves(src, ".. ..", "....", DefKind::Value { is_function: true });
}

/// A nullary `let (.. ..) = …` value def is bound too (the `NAMED_PAT` /
/// `RANGE_STEP_OP` path), and a reference resolves to it.
#[test]
fn reference_resolves_to_nullary_value_def() {
    let src = "let (.. ..) = id\nlet z = op (.. ..)\n";
    let rf = resolve(src);
    let def_range = range_of(src, ".. ..");
    let use_range = {
        let first = src.find(".. ..").unwrap();
        let second = src[first + 1..].find(".. ..").unwrap() + first + 1;
        TextRange::new(
            u32::try_from(second).unwrap().into(),
            u32::try_from(second + ".. ..".len()).unwrap().into(),
        )
    };
    let res = rf.resolution_at(use_range).expect("use resolves");
    let def = rf.resolved_def(res).expect("names an in-file def");
    assert_eq!(def.range, def_range);
    assert_eq!(def.kind, DefKind::Value { is_function: false });
}

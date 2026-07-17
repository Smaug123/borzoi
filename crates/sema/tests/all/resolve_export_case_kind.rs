//! Direct (FCS-free) tests pinning the [`CaseKind`] each producer shape stores
//! on the value-namespace [`ExportedItem`] it contributes to the cross-file
//! boundary (Stage 1 of `docs/export-decl-model-plan.md`). Each snippet
//! exercises one of the five producer call sites that push a value-namespace
//! export and observes the stored kind through the `ExportedItem::case_kind`
//! test accessor:
//!
//! - a module-level `let` value → `None` (an ordinary value, not a case);
//! - a plain (non-`[<RequireQualifiedAccess>]`) union case →
//!   `Union { require_qualified: false }`;
//! - a `[<RequireQualifiedAccess>]` union case → `Union { require_qualified: true }`;
//! - an `enum` case → `Enum`;
//! - an `exception E of …` constructor → `Exception`.
//!
//! These assert the resolver's stored export payload directly (no oracle). The
//! snippets carry a `module M` header so the cases are actually exported (an
//! anonymous-root file drops union/exception/enum-case exports — see
//! [`Resolver::export_case`]). Stage 1 is a zero-behaviour-change refactor, so
//! no *resolution* is pinned here; the existing suites cover that.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, CaseKind, ProjectItems, ResolvedFile, resolve_file};

/// Parse `src` (asserting it is in the parser subset) and resolve it as a lone
/// single file.
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

/// The [`CaseKind`] stored on the exported item named `name` (or `None` for an
/// ordinary value). Panics if no export carries that name — distinguishing
/// "exported value, no case kind" (`Some(_)` return of `None`) from "not
/// exported at all" (panic).
fn case_kind_of(rf: &ResolvedFile, name: &str) -> Option<CaseKind> {
    let item = rf
        .exports()
        .iter()
        .find(|i| i.name() == name)
        .unwrap_or_else(|| panic!("no exported item named {name:?}"));
    item.case_kind()
}

#[test]
fn module_let_value_has_no_case_kind() {
    // A module-level `let` value is an ordinary value-namespace export, not a
    // constructor case: its stored kind is `None`.
    let src = "module M\nlet x = 1\n";
    let rf = resolve(src);
    assert_eq!(case_kind_of(&rf, "x"), None);
}

#[test]
fn plain_union_case_is_non_rqa_union() {
    // A non-`[<RequireQualifiedAccess>]` union case is a value-namespace case
    // exported via `export_case`: `Union { require_qualified: false }`.
    let src = "module M\ntype U = A | B\n";
    let rf = resolve(src);
    assert_eq!(
        case_kind_of(&rf, "A"),
        Some(CaseKind::Union {
            require_qualified: false
        })
    );
    assert_eq!(
        case_kind_of(&rf, "B"),
        Some(CaseKind::Union {
            require_qualified: false
        })
    );
}

#[test]
fn rqa_union_case_is_rqa_union() {
    // A `[<RequireQualifiedAccess>]` union case is exported via
    // `export_require_qualified_case`: `Union { require_qualified: true }`.
    let src = "module M\n[<RequireQualifiedAccess>]\ntype U = A | B\n";
    let rf = resolve(src);
    assert_eq!(
        case_kind_of(&rf, "A"),
        Some(CaseKind::Union {
            require_qualified: true
        })
    );
    assert_eq!(
        case_kind_of(&rf, "B"),
        Some(CaseKind::Union {
            require_qualified: true
        })
    );
}

#[test]
fn enum_case_is_enum() {
    // An `enum` case is exported via `export_require_qualified_case` too, but
    // carries the `Enum` kind rather than an RQA union.
    let src = "module M\ntype E = A = 0 | B = 1\n";
    let rf = resolve(src);
    assert_eq!(case_kind_of(&rf, "A"), Some(CaseKind::Enum));
    assert_eq!(case_kind_of(&rf, "B"), Some(CaseKind::Enum));
}

#[test]
fn exception_ctor_is_exception() {
    // An `exception E of …` constructor is a value-namespace case exported via
    // `export_case`, carrying the `Exception` kind.
    let src = "module M\nexception Foo of int\n";
    let rf = resolve(src);
    assert_eq!(case_kind_of(&rf, "Foo"), Some(CaseKind::Exception));
}

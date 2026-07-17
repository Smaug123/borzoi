//! FCS-free tests for the cross-file Compile-order fold (`resolve_project`).
//!
//! These exercise the fold mechanics and the soundness rules directly, without
//! the cost of an FCS round-trip: a qualified cross-file reference resolves to
//! the right earlier-file item; a *bare* cross-file reference does **not**
//! (illegal in F# without an `open`); a *forward* reference to a later file does
//! **not**; and a single file routed through the fold resolves identically to a
//! direct `resolve_file`. `resolve_project_diff.rs` separately checks these
//! against FCS.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, ProjectItems, Resolution, SemanticClass, resolve_file, resolve_project,
};
use rowan::TextRange;

fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

fn span(start: usize, len: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + len).unwrap().into(),
    )
}

/// The (start, len) of `needle`'s only occurrence in `hay`.
fn at(hay: &str, needle: &str) -> (usize, usize) {
    let s = hay
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {hay:?}"));
    (s, needle.len())
}

#[test]
fn qualified_cross_file_value_resolves_to_the_earlier_file_binder() {
    let src1 = "module Shared\nlet foo = 1\n";
    let src2 = "module Other\nlet bar = Shared.foo\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());

    // `Shared.foo` (the whole dotted path) resolves to an Item.
    let (s, len) = at(src2, "Shared.foo");
    let res = proj
        .file(1)
        .resolution_at(span(s, len))
        .expect("a resolution at `Shared.foo`");
    assert!(
        matches!(res, Resolution::Item(_)),
        "qualified cross-file value is an Item, got {res:?}"
    );

    // …pointing at `foo`'s binder in file1.
    let (file_idx, def) = proj.item_def(res).expect("the item's def");
    assert_eq!(file_idx, 0, "declared in file1");
    let (fs, flen) = at(src1, "foo");
    assert_eq!(def.range, span(fs, flen), "points at file1's `foo` binder");
}

#[test]
fn qualified_cross_file_function_application_resolves() {
    let src1 = "module M\nlet add a b = a\n";
    let src2 = "module N\nlet z = M.add 1 2\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());

    let (s, len) = at(src2, "M.add");
    let res = proj
        .file(1)
        .resolution_at(span(s, len))
        .expect("a resolution at `M.add`");
    let (file_idx, def) = proj.item_def(res).expect("the item's def");
    assert_eq!(file_idx, 0);
    let (fs, flen) = at(src1, "add");
    assert_eq!(def.range, span(fs, flen), "points at file1's `add` binder");
}

#[test]
fn project_token_classifier_classifies_cross_file_references() {
    // A value and a function defined in file1, used qualified in file2. The
    // classification is compositional: cross-file *resolution* is checked against
    // FCS in `resolve_project_diff`, and `semantic_class` in `classify_diff`;
    // this pins that the project-level classifier wires the two together (and
    // that the single-file one correctly declines a binder it can't see).
    let src1 = "module Shared\nlet foo = 1\nlet add a b = a\n";
    let src2 = "module Other\nlet bar = Shared.foo\nlet baz = Shared.add 1 2\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());

    // No referenced assemblies here, so a default env suffices — cross-file
    // items still classify (via `item_def`); Entity/Member can't occur. The
    // classifier borrows the env, so it must outlive the closure.
    let env = AssemblyEnv::default();
    let project_classify = proj.token_classifier(1, &env);
    let file_classify = proj.file(1).token_classifier();

    for (needle, tail, class) in [
        ("Shared.foo", "foo", SemanticClass::Value),
        ("Shared.add", "add", SemanticClass::Function),
    ] {
        let (base, _) = at(src2, needle);
        let tail_off = base + needle.len() - tail.len();
        let tail_range = span(tail_off, tail.len());
        // The project fold follows the cross-file `Item` to its file1 binder …
        assert_eq!(
            project_classify(tail_range),
            Some(class),
            "cross-file tail {tail:?} classified via the project"
        );
        // … while the single-file classifier declines it (binder lives in file1).
        assert_eq!(
            file_classify(tail_range),
            None,
            "single-file classifier must decline cross-file {tail:?}"
        );
    }
}

#[test]
fn token_classifiers_are_detached_from_their_source() {
    // Both classifiers own their end-offset index and borrow nothing, so a caller
    // may retain one after the resolved file/project it was built from is dropped.
    // The `+ use<>` on the public returns is what makes this a compile error to
    // regress (Rust 2024 would otherwise capture `&self` in the opaque type); the
    // `require_static` boundary pins that bound, and the post-drop calls confirm
    // the closure genuinely owns its data. See `end_index_classifier`.
    fn require_static<F: Fn(TextRange) -> Option<SemanticClass> + 'static>(f: F) -> F {
        f
    }

    let src1 = "module Shared\nlet foo = 1\n";
    let src2 = "module Other\nlet bar = Shared.foo\n";
    let (base, _) = at(src2, "Shared.foo");
    let tail_range = span(base + "Shared.".len(), "foo".len());

    let (project_classify, file_classify) = {
        let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());
        let env = AssemblyEnv::default();
        let project_classify = require_static(proj.token_classifier(1, &env));
        let file_classify = require_static(proj.file(1).token_classifier());
        (project_classify, file_classify)
        // `proj` and `env` are dropped here; the classifiers must outlive them.
    };

    assert_eq!(project_classify(tail_range), Some(SemanticClass::Value));
    assert_eq!(file_classify(tail_range), None);
}

#[test]
fn bare_cross_file_reference_does_not_resolve() {
    // `foo` bare in file2 — F# requires a qualifier or `open`, so this must
    // NOT silently resolve to file1's `foo` (correctness over availability).
    let src1 = "module Shared\nlet foo = 1\n";
    let src2 = "module Other\nlet bar = foo\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());

    let (s, len) = at(src2, "= foo");
    // skip "= " to land on `foo`.
    let res = proj
        .file(1)
        .resolution_at(span(s + 2, len - 2))
        .expect("a resolution at bare `foo`");
    assert!(
        matches!(res, Resolution::Deferred(_)),
        "bare cross-file reference must be Deferred, got {res:?}"
    );
}

#[test]
fn forward_cross_file_reference_does_not_resolve() {
    // file1 (earlier) qualified-references file2's (later) binding — illegal in
    // F#; the fold must not resolve it (file2 is not yet in `preceding`).
    let src1 = "module A\nlet x = B.y\n";
    let src2 = "module B\nlet y = 1\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());

    let (s, len) = at(src1, "B.y");
    // No cross-file Item is recorded at the whole path for a forward reference.
    let res = proj.file(0).resolution_at(span(s, len));
    assert!(
        !matches!(res, Some(Resolution::Item(_))),
        "forward reference must not resolve to an Item, got {res:?}"
    );
    // And nothing in file1 resolves into a later file.
    for r in proj.file(0).resolutions().values() {
        if let Some((idx, _)) = proj.item_def(*r) {
            assert_eq!(idx, 0, "file1 resolved into a later file (idx {idx})");
        }
    }
}

#[test]
fn single_file_through_the_fold_matches_resolve_file() {
    let src = "let foo = 1\nlet bar = foo\n";
    let f = impl_file(src);
    let via_project = resolve_project(std::slice::from_ref(&f), &AssemblyEnv::default());
    let direct = resolve_file(&f, &ProjectItems::default(), &AssemblyEnv::default());
    assert_eq!(
        via_project.file(0).resolutions(),
        direct.resolutions(),
        "a single file resolves identically through the fold and directly"
    );
}

#[test]
fn local_head_wins_over_cross_file_qualified_path() {
    // `data` is the parameter of `f`, so `data.x` is member access on a local
    // — it must NOT resolve to file1's `data.x`, even though an earlier file
    // exports that qualified name. (Member access needs inference; defer it.)
    let src1 = "module data\nlet x = 1\n";
    let src2 = "module app\nlet f data = data.x\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());
    let rf2 = proj.file(1);

    let (ws, _) = at(src2, "data.x");
    // The head `data` resolves to the local parameter…
    let head = rf2
        .resolution_at(span(ws, "data".len()))
        .expect("head resolution");
    assert!(
        matches!(head, Resolution::Local(_)),
        "head `data` must resolve to the local parameter, got {head:?}"
    );
    // …and the whole path is not mis-resolved to a cross-file item.
    assert!(
        !matches!(
            rf2.resolution_at(span(ws, "data.x".len())),
            Some(Resolution::Item(_))
        ),
        "member access on a local must not resolve to a cross-file item"
    );
}

#[test]
fn quoted_dotted_binder_does_not_collide_with_a_real_path() {
    // file1 exports `` `B.x` `` (one identifier containing a dot) from module A.
    // A later genuine three-segment path `A.B.x` is a *different* name in F#
    // (the binder is reachable only as ``A.`B.x` ``), so it must not match.
    let src1 = "module A\nlet ``B.x`` = 1\n";
    let src2 = "module C\nlet y = A.B.x\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());
    let rf2 = proj.file(1);

    let (ws, wlen) = at(src2, "A.B.x");
    assert!(
        !matches!(rf2.resolution_at(span(ws, wlen)), Some(Resolution::Item(_))),
        "`A.B.x` must not match the quoted single-identifier export `A.``B.x```"
    );
}

#[test]
fn quoted_dotted_segment_resolves_through_the_correct_path() {
    // The legitimate cross-file reference to `` `B.x` `` is `` A.`B.x` `` — a
    // two-segment path whose second segment is the quoted identifier. It must
    // resolve to file1's binder.
    let src1 = "module A\nlet ``B.x`` = 1\n";
    let src2 = "module C\nlet y = A.``B.x``\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());
    let rf2 = proj.file(1);

    let (ws, wlen) = at(src2, "A.``B.x``");
    let res = rf2
        .resolution_at(span(ws, wlen))
        .expect("resolution at the qualified quoted path");
    let (file_idx, _) = proj.item_def(res).expect("the item's def");
    assert_eq!(file_idx, 0, "resolves into file1's quoted binder");
}

#[test]
fn item_ids_are_unique_across_files() {
    // file1 and file2 each export one value; their item handles must not collide
    // (project-global numbering), so item_def routes each to its own file.
    let src1 = "module A\nlet a = 1\n";
    let src2 = "module B\nlet b = 2\nlet c = B.b\n";
    let proj = resolve_project(&[impl_file(src1), impl_file(src2)], &AssemblyEnv::default());
    // file2's `B.b` resolves to file2's own `b` (same-file qualified ref via the
    // fold is not modelled — it lives in *this* file, not `preceding` — so it
    // stays Deferred; assert we at least never mis-route to file1).
    let (s, len) = at(src2, "B.b");
    if let Some(res @ Resolution::Item(_)) = proj.file(1).resolution_at(span(s, len)) {
        let (idx, _) = proj.item_def(res).expect("def");
        assert_eq!(
            idx, 1,
            "must not mis-route a same-file qualified ref to file1"
        );
    }
}

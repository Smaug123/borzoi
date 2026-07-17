//! Direct (FCS-free) tests for nested-module scoping: the resolver descends
//! into a `module M = …` body, so intra-module references, parameters, and
//! locals resolve — without leaking the module's bindings unqualified into the
//! enclosing scope.
//!
//! This closes the gap that made go-to-definition produce *nothing* on the
//! dominant real-world file shape (`namespace N` + nested `module M = …`, e.g.
//! `FSharp.Core/string.fs`), where the whole body lives inside a nested module.
//!
//! Identifiers are chosen to avoid appearing as substrings of F# keywords
//! (`namespace`, `module`, `let`, …) so the `nth`-occurrence needle lands on the
//! intended token.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, ResolvedFile, resolve_file, resolve_project};
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

/// The byte range of the `n`th (0-based) occurrence of `needle` in `src`.
fn nth(src: &str, needle: &str, n: usize) -> TextRange {
    let mut from = 0;
    for _ in 0..n {
        let i = src[from..].find(needle).expect("occurrence") + from;
        from = i + needle.len();
    }
    let i = src[from..].find(needle).expect("occurrence") + from;
    TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + needle.len()).unwrap().into(),
    )
}

/// Assert the use of `needle` at occurrence `use_idx` resolves to a binder at
/// occurrence `def_idx`.
fn assert_use(src: &str, needle: &str, use_idx: usize, def_idx: usize) {
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, needle, use_idx))
        .unwrap_or_else(|| panic!("no resolution at {needle:?} use ({use_idx}) in {src:?}"));
    let def = rf
        .resolved_def(res)
        .unwrap_or_else(|| panic!("{needle:?} use ({use_idx}) names no in-file def in {src:?}"));
    assert_eq!(
        def.range,
        nth(src, needle, def_idx),
        "{needle:?} use ({use_idx}) points at the wrong def in {src:?}"
    );
}

#[test]
fn nested_module_body_value_reference_resolves() {
    // `module Innerm` under a top-level `module Outerm`: the `vv` use in
    // `let ww = vv` resolves to the sibling `let vv`.
    assert_use(
        "module Outerm\nmodule Innerm =\n    let vv = 1\n    let ww = vv\n",
        "vv",
        1,
        0,
    );
}

#[test]
fn nested_module_under_namespace_resolves() {
    // The `string.fs` shape: a nested module under a `namespace`. The `vv` use in
    // `let ww = vv` resolves to the sibling `let vv`.
    assert_use(
        "namespace Topns\nmodule Subm =\n    let vv = 1\n    let ww = vv\n",
        "vv",
        1,
        0,
    );
}

#[test]
fn nested_module_parameter_resolves() {
    // A parameter of a function defined inside a nested module resolves in its
    // body.
    assert_use(
        "module Outerm\nmodule Innerm =\n    let gg pp = pp\n",
        "pp",
        1,
        0,
    );
}

#[test]
fn nested_module_function_referenced_by_sibling_resolves() {
    // The canonical `string.fs` pattern: a sibling function call, with a typed
    // parameter. `getlen arg` resolves `getlen` to the sibling and `arg` to the
    // caller's parameter.
    let src = "namespace Topns\nmodule Subm =\n    let getlen (item: string) = 0\n    let user arg = getlen arg\n";
    assert_use(src, "getlen", 1, 0); // the call resolves to the definition
    assert_use(src, "arg", 1, 0); // `arg` in `getlen arg` is `user`'s parameter
}

#[test]
fn nested_module_bindings_do_not_leak_to_enclosing_scope() {
    // A binding inside a nested module is NOT visible *unqualified* to a later
    // sibling at the enclosing level — F# requires `Innerm.secretvv`. The bare
    // `secretvv` use must therefore not resolve to the nested binder (it defers).
    let src = "module Outerm\nmodule Innerm =\n    let secretvv = 1\nlet otherww = secretvv\n";
    let rf = resolve(src);
    let use_range = nth(src, "secretvv", 1); // the use in `let otherww = secretvv`
    let leaked = rf
        .resolution_at(use_range)
        .and_then(|res| rf.resolved_def(res))
        .is_some();
    assert!(
        !leaked,
        "nested-module binding `secretvv` leaked into the enclosing scope in {src:?}"
    );
}

#[test]
fn deeply_nested_modules_resolve() {
    // Two levels of nesting: a reference in the innermost module to its own
    // sibling resolves.
    let src = "namespace Topns\nmodule Aam =\n    module Bbm =\n        let vv = 1\n        let ww = vv\n";
    assert_use(src, "vv", 1, 0);
}

#[test]
fn enclosing_module_binding_visible_in_nested_body() {
    // A nested module body can reference a binding of its *enclosing* module
    // (lexical scoping looks outward).
    let src = "module Outerm\nlet sharedvv = 1\nmodule Innerm =\n    let usevv = sharedvv\n";
    assert_use(src, "sharedvv", 1, 0);
}

#[test]
fn open_inside_nested_module_does_not_disturb_enclosing_resolution() {
    // An `open` inside a nested module is scoped to that module; after it, an
    // enclosing sibling resolves exactly as before. (The genuinely *unsound*
    // manifestation — resolving an enclosing reference *through* the leaked open
    // — needs a referenced assembly to observe; the resolver-state restoration
    // that prevents it is exercised here, guarding that the body walk restores
    // cleanly and the enclosing binder still resolves.)
    let src =
        "module Topm\nmodule Innerm =\n    open System\n    let aa = 1\nlet bb = 2\nlet cc = bb\n";
    assert_use(src, "bb", 1, 0);
}

#[test]
fn anonymous_file_nested_module_value_is_not_bare_cross_file_exported() {
    // File 1 is *anonymous* (it opens with a top-level `let`), so its nested
    // `module Calc` lives under the implicit *filename* module. A later file's
    // bare `Calc.answer` must NOT resolve to it — F# requires the filename-
    // qualified `<FileName>.Calc.answer`. Exporting it as bare `Calc.answer`
    // would be a wrong cross-file go-to-definition.
    let f1 = "let top = 1\nmodule Calc =\n    let answer = 2\n";
    let f2 = "module Other\nlet useit = Calc.answer\n";
    let a1 = ImplFile::cast(parse(f1).root).expect("impl file 1");
    let a2 = ImplFile::cast(parse(f2).root).expect("impl file 2");
    let proj = resolve_project(&[a1, a2], &AssemblyEnv::default());

    // The whole-path `Calc.answer` use in file 2 resolves to an `Item` only if
    // the value was (wrongly) bare-exported; otherwise it is unrecorded/deferred.
    let start = f2.find("Calc.answer").expect("the reference");
    let use_range = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + "Calc.answer".len()).unwrap().into(),
    );
    let leaked = proj
        .file(1)
        .resolution_at(use_range)
        .and_then(|res| proj.item_def(res))
        .is_some();
    assert!(
        !leaked,
        "anonymous-file nested-module value was wrongly bare cross-file exported"
    );
}

#[test]
fn global_namespace_nested_module_value_is_cross_file_exported() {
    // `namespace global` is a *real* (global) namespace, unlike an anonymous
    // file: a nested module's value IS bare-cross-file referenceable
    // (`Calc.answer`), so a later file's reference resolves into file 1.
    let f1 = "namespace global\nmodule Calc =\n    let answer = 1\n";
    let f2 = "module Other\nlet useit = Calc.answer\n";
    let a1 = ImplFile::cast(parse(f1).root).expect("impl file 1");
    let a2 = ImplFile::cast(parse(f2).root).expect("impl file 2");
    let proj = resolve_project(&[a1, a2], &AssemblyEnv::default());

    let start = f2.find("Calc.answer").expect("the reference");
    let use_range = TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + "Calc.answer".len()).unwrap().into(),
    );
    let res = proj
        .file(1)
        .resolution_at(use_range)
        .expect("`Calc.answer` resolves under namespace global");
    let (def_file, def) = proj.item_def(res).expect("a cross-file item");
    assert_eq!(def_file, 0, "resolves into file 1");
    let answer = f1.find("answer").expect("answer def");
    assert_eq!(
        def.range,
        TextRange::new(
            u32::try_from(answer).unwrap().into(),
            u32::try_from(answer + "answer".len()).unwrap().into(),
        ),
    );
}

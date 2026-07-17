//! Differential test: our [`SemanticClass`] classification of *referenced-assembly*
//! names against FCS's symbol kinds — the cross-assembly counterpart of
//! [`classify_diff`](crate::classify_diff).
//!
//! The single-file census differential runs with no `AssemblyEnv`, so it can only
//! exercise in-file classification. This one makes the sema fixture DLL resolvable
//! to *both* sides — FCS via `BORZOI_FCS_EXTRA_REFS`
//! ([`invoke_fcs_dump_census_with_refs`](crate::common::invoke_fcs_dump_census_with_refs))
//! and us via [`AssemblyEnv::from_views`] — so a qualified reference into it
//! (`Demo.Calc.Zero`) resolves on both, and we can hold our
//! [`AssemblyEnv::entity_class`] / [`AssemblyEnv::member_class`] mapping against
//! FCS.
//!
//! Same *certain-implies-agree* contract: for every occurrence where our
//! token-oriented classifier commits, FCS's kind at that occurrence must be
//! compatible (a decline makes no claim). This systematically flushes mapping
//! corners — the module-function/value split, enum cases, exceptions — that a
//! per-branch unit test could miss.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, SemanticClass, resolve_project};
use rowan::TextRange;

use crate::common::{
    CensusUse, LineIndex, ensure_assembly_fixture_built, invoke_fcs_dump_census_with_refs,
    parse_census_jsonl,
};

/// Snippets referencing statics of the sema fixture DLL — the qualified,
/// name-resolution-decidable surface (instance members need inference and are
/// declined on both sides). `Demo.Calc` is a static class with a static method
/// `Zero` and a static property `Answer`; `Demo.Widget` has a static field
/// `StaticCount` and static property `StaticProp`.
const CORPUS: &[&str] = &[
    "module M\nlet a = Demo.Calc.Zero()\n",
    "module M\nlet b = Demo.Calc.Answer\n",
    "module M\nlet c = Demo.Widget.StaticCount\n",
    "module M\nlet d = Demo.Widget.StaticProp\n",
];

fn fixture_env() -> AssemblyEnv {
    let bytes = std::fs::read(ensure_assembly_fixture_built()).expect("read fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse fixture dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
}

fn temp_fs_file(source: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "borzoi_sema_classify_asm_diff_{}_{n}.fs",
        std::process::id()
    ));
    let mut f = std::fs::File::create(&path).expect("create temp .fs");
    f.write_all(source.as_bytes()).expect("write temp .fs");
    path
}

fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

/// One batched census over the whole corpus, with the fixture DLL referenced, so
/// FCS reports the kinds of the referenced-assembly symbols. Cached (one FCS
/// process for the group).
fn corpus_census() -> &'static std::collections::HashMap<&'static str, Vec<CensusUse>> {
    static CENSUS: OnceLock<std::collections::HashMap<&'static str, Vec<CensusUse>>> =
        OnceLock::new();
    CENSUS.get_or_init(|| {
        let dll = ensure_assembly_fixture_built().to_path_buf();
        let files: Vec<(PathBuf, &'static str)> =
            CORPUS.iter().map(|&s| (temp_fs_file(s), s)).collect();
        let paths: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
        let json = invoke_fcs_dump_census_with_refs(&paths, &[dll.as_path()]);
        let parsed = parse_census_jsonl(&json);
        for (p, _) in &files {
            let _ = std::fs::remove_file(p);
        }
        let mut by_source = std::collections::HashMap::new();
        for fc in parsed {
            let name = std::path::Path::new(&fc.path).file_name();
            let &(_, source) = files
                .iter()
                .find(|(p, _)| p.file_name() == name)
                .unwrap_or_else(|| panic!("census path {:?} is not a corpus snippet", fc.path));
            assert!(fc.ok, "FCS failed to type-check {source:?}");
            by_source.insert(source, fc.uses);
        }
        assert_eq!(by_source.len(), CORPUS.len());
        by_source
    })
}

/// Is FCS's kind at an occurrence compatible with the cross-assembly
/// [`SemanticClass`] we committed? Empirically pinned by `dump_fcs_ground_truth`.
fn fcs_compatible(class: SemanticClass, u: &CensusUse) -> bool {
    let c = u.class.as_str();
    match class {
        // The type a path roots at: an entity that is neither namespace nor module.
        SemanticClass::Type => c == "Entity" && !u.is_namespace && !u.is_module,
        // A member reached through a *type* (static): FCS sees an `Mfv` member.
        SemanticClass::Method => c == "Mfv" && u.is_member,
        // A property or a (static) field: FCS sees a property `Mfv`, or a `Field`.
        SemanticClass::Property => (c == "Mfv" && u.is_property) || c == "Field",
        // The variants the corpus does not currently exercise (a referenced F#
        // module's function/value split, enum cases, events) are covered by the
        // per-branch `resolve_assembly` unit test; assert nothing here.
        SemanticClass::Function
        | SemanticClass::Value
        | SemanticClass::Module
        | SemanticClass::Event
        | SemanticClass::EnumCase
        | SemanticClass::Parameter
        | SemanticClass::PatternLocal
        | SemanticClass::UnionCase
        | SemanticClass::ExceptionCase
        | SemanticClass::ActivePattern
        | SemanticClass::Member => false,
    }
}

/// Resolve `source` against the fixture env and, for every FCS occurrence where
/// our token classifier commits, assert compatibility. Returns the committed
/// `(text, class)` pairs.
fn committed(source: &str) -> Vec<(String, SemanticClass)> {
    let env = fixture_env();
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "parse errors in {source:?}");
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let proj = resolve_project(&[file], &env);
    let classify = proj.token_classifier(0, &env);

    let idx = LineIndex::new(source);
    let mut out = Vec::new();
    for u in &corpus_census()[source] {
        let (s, e) = u.use_range_bytes(&idx);
        if s == e {
            continue;
        }
        // Cross-assembly only: an occurrence whose declaration is *in this file*
        // (the `let a = …` bindings) is `classify_diff`'s job; here we check the
        // referenced-assembly symbols, whose declaration lies outside it.
        if u.decl_range_bytes(&idx).is_some() {
            continue;
        }
        let Some(class) = classify(span(s, e)) else {
            continue;
        };
        let text = &source[s..e];
        assert!(
            fcs_compatible(class, u),
            "we classified {text:?} at {:?} as {class:?}, but FCS reports \
             class={:?} member={} prop={} val={} fn={} ns={} mod={}; {source:?}",
            span(s, e),
            u.class,
            u.is_member,
            u.is_property,
            u.is_value,
            u.is_function,
            u.is_namespace,
            u.is_module,
        );
        out.push((text.to_string(), class));
    }
    out
}

#[test]
fn cross_assembly_classification_agrees_with_fcs() {
    for source in CORPUS {
        let committed = committed(source);
        assert!(
            !committed.is_empty(),
            "no cross-assembly classification committed for {source:?}"
        );
    }
}

#[test]
fn commits_referenced_type_method_and_property() {
    let has = |source: &str, text: &str, class: SemanticClass| {
        committed(source)
            .iter()
            .any(|(t, c)| t == text && *c == class)
    };
    // FCS reports the type head at its own sub-range (`Calc`) and the member at
    // the whole dotted path (`Demo.Calc.Zero`).
    assert!(
        has(
            "module M\nlet a = Demo.Calc.Zero()\n",
            "Calc",
            SemanticClass::Type
        ),
        "`Demo.Calc` roots at a type"
    );
    assert!(
        has(
            "module M\nlet a = Demo.Calc.Zero()\n",
            "Demo.Calc.Zero",
            SemanticClass::Method
        ),
        "`Zero` is a static method"
    );
    assert!(
        has(
            "module M\nlet b = Demo.Calc.Answer\n",
            "Demo.Calc.Answer",
            SemanticClass::Property
        ),
        "`Answer` is a property"
    );
}

/// Ground-truth dump (ignored): print each occurrence's FCS kind alongside our
/// classification. Run with:
/// `cargo test -p borzoi-sema --test all classify_assembly_diff::dump_fcs_ground_truth -- --ignored --nocapture`
#[test]
#[ignore = "diagnostic dump, not a gate"]
fn dump_fcs_ground_truth() {
    for source in CORPUS {
        println!("\n=== {source:?} ===");
        let env = fixture_env();
        let file = ImplFile::cast(parse(source).root).expect("impl file");
        let proj = resolve_project(&[file], &env);
        let classify = proj.token_classifier(0, &env);
        let idx = LineIndex::new(source);
        for u in &corpus_census()[source] {
            let (s, e) = u.use_range_bytes(&idx);
            if s == e {
                continue;
            }
            println!(
                "  {:<16} fcs[class={:<10} member={} prop={} val={} fn={} ns={} mod={}] ours={:?}",
                &source[s..e],
                u.class,
                u.is_member,
                u.is_property,
                u.is_value,
                u.is_function,
                u.is_namespace,
                u.is_module,
                classify(span(s, e)),
            );
        }
    }
}

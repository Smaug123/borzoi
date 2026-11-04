//! The FCS-free halves of the accessor-coverage check
//! (`common::accessor_coverage`). The check itself needs the corpus and FCS, so
//! it runs at the end of `parser_corpus_diff`; these keep its two inputs honest
//! on every `cargo test`.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::accessor_trace::record;
use borzoi_cst::syntax::{AstNode, Expr, ImplFile, ModuleDecl};

use crate::common::accessor_coverage::{
    NOT_PROJECTED, accessor_universe, assert_projection_covers_consumers, consumer_reads, facade,
    fixture_reads, pinned_projection,
};

/// Every public accessor opens with its own `accessor!` call, so none is
/// invisible to the trace.
#[test]
fn every_public_accessor_is_traced() {
    let universe = accessor_universe();
    assert!(
        universe.len() > 300,
        "the accessor scan found only {} accessors; is it still reading syntax/mod.rs?",
        universe.len()
    );
}

/// The consumer scan finds reads it must: a plain method call
/// (`is_bracket_indexer`, in the type inferrer), and a method reference by
/// path. An exemption must name a real accessor.
#[test]
fn consumer_scan_sees_known_reads() {
    let universe = accessor_universe();
    let reads = consumer_reads(&facade());
    eprintln!(
        "{} accessors, {} read by consumers",
        universe.len(),
        reads.len()
    );
    for expected in ["AppExpr::is_bracket_indexer", "AppExpr::is_infix"] {
        assert!(
            reads.contains_key(expected),
            "the consumer scan missed {expected}"
        );
    }
    for (accessor, _) in NOT_PROJECTED {
        assert!(
            universe.contains(*accessor),
            "NOT_PROJECTED names {accessor}, which is not an accessor"
        );
    }
}

/// The trace logs the accessor a caller called, not the accessors that one
/// calls in turn: `AppExpr::func` reads `AppExpr::is_infix` internally, and
/// only `func` was read by the caller.
#[test]
fn the_trace_logs_outermost_reads_only() {
    let parse = parse("let z = f x\n");
    let file = ImplFile::cast(parse.root.clone()).expect("an implementation file");
    let module = file.modules().next().expect("one module");
    let Some(ModuleDecl::Let(decl)) = module.decls().next() else {
        panic!("one let declaration");
    };
    let binding = decl.bindings().next().expect("one binding");
    let Some(Expr::App(app)) = binding.expr() else {
        panic!("the body is an application");
    };
    let ((), read) = record(|| {
        let _ = app.func();
    });
    assert_eq!(read.into_iter().collect::<Vec<_>>(), vec!["AppExpr::func"]);
    // Outside a recording nothing is logged, and a recording sees only its own.
    let _ = app.is_infix();
    let ((), read) = record(|| {});
    assert!(read.is_empty());
}

/// Every accessor sema or the LSP reads is one the parser differential compares
/// with FCS (as pinned by `parser_corpus_diff`), or is exempt with a reason.
#[test]
fn consumers_read_only_compared_accessors() {
    assert_projection_covers_consumers(&pinned_projection());
}

/// The accessor fixtures (constructs no matching corpus file holds) agree with
/// FCS and read what they claim to.
#[test]
fn accessor_fixtures_match_fcs() {
    let _ = fixture_reads();
}

//! Walk coverage over the corpus: where does the resolver record **nothing**?
//!
//! Every expression the declaration walk reaches gets a record at the head of
//! each long identifier it names — a binding, a deferral, or `Unresolved`. A
//! head with no record at all is in a body the walk never entered. The FCS
//! differentials cannot tell those apart from an ordinary deferral: both are a
//! use FCS resolves and we do not answer. So a construct whose bodies are
//! skipped wholesale shows up there as thousands of unrelated-looking gaps,
//! and in the whole-project gate as a file whose uses are all unexplained.
//!
//! This sweep names them instead. Each silent head is attributed to the
//! innermost enclosing container that explains it (its *bucket*), and the
//! manifest pins, exactly and in both directions, how many silent heads each
//! file has in each bucket: a skipped construct is a block of lines sharing a
//! bucket, and walking it removes the block. The run prints a few heads of
//! each bucket for triage. It needs no oracle: walk coverage is a property of
//! our resolver alone.
//!
//! Heads that are not names are left out: operators (resolved separately),
//! `base`, and the left of an argument `x = …` or `?x = …`, which may be a
//! named-argument label rather than a use. (An equality operand `f (x = y)` has
//! the same shape, so this sweep cannot see one left silent.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use borzoi_cst::parser::{Parse, parse};
use borzoi_cst::syntax::{AstNode, Expr, ImplFile, SyntaxKind, SyntaxNode, TypeDefn, TypeDefnRepr};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::{AssemblyEnv, ProjectItems, SyntaxRecovery, resolve_file};
use rowan::TextRange;

use crate::common::corpus_manifest::{
    Positions, check_manifest, corpus_relative, regenerate_ignored, sort_by_corpus_key,
};

const MANIFEST: &str = "silent_region_sweep";

/// Heads printed per bucket.
const SAMPLE: usize = 5;

/// The containers a silent head can be attributed to; the innermost one
/// enclosing it wins.
const CONTAINERS: &[SyntaxKind] = &[
    SyntaxKind::OBJ_EXPR,
    SyntaxKind::MEMBER_DEFN,
    SyntaxKind::ATTRIBUTE,
    SyntaxKind::TYPE_DEFN,
    SyntaxKind::LET_DECL,
    SyntaxKind::EXCEPTION_DEFN,
    SyntaxKind::NESTED_MODULE_DECL,
];

fn collect_fs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_fs(&path, out);
        } else if path.extension().is_some_and(|e| e == "fs") {
            out.push(path);
        }
    }
}

/// The bucket a silent head in `expr` is attributed to: its innermost
/// container, and for a member, what hosts the member.
fn bucket(expr: &SyntaxNode) -> String {
    let Some(container) = expr.ancestors().find(|a| CONTAINERS.contains(&a.kind())) else {
        return "top".to_string();
    };
    if container.kind() != SyntaxKind::MEMBER_DEFN {
        return format!("{:?}", container.kind()).to_lowercase();
    }
    let host = container
        .ancestors()
        .skip(1)
        .find(|h| matches!(h.kind(), SyntaxKind::OBJ_EXPR | SyntaxKind::TYPE_DEFN));
    match host {
        Some(h) if h.kind() == SyntaxKind::OBJ_EXPR => "member-of-object-expression".to_string(),
        Some(h) => match TypeDefn::cast(h).and_then(|d| d.repr()) {
            Some(TypeDefnRepr::ObjectModel(om)) if om.is_augmentation() => {
                "member-of-augmentation".to_string()
            }
            _ => "member-of-type".to_string(),
        },
        None => "member".to_string(),
    }
}

/// Whether `expr` is the left of an argument element `expr = value` (F#'s
/// `App[ InfixApp[expr, "="], value ]`): a named-argument label, or an equality
/// operand of the same shape.
fn is_argument_label(expr: &SyntaxNode) -> bool {
    let Some(Expr::App(op_app)) = expr.parent().and_then(Expr::cast) else {
        return false;
    };
    let Some(Expr::App(outer)) = op_app.syntax().parent().and_then(Expr::cast) else {
        return false;
    };
    op_app.is_infix()
        && op_app
            .func()
            .is_some_and(|op| op.syntax().text().to_string().trim() == "=")
        && op_app.arg().is_some_and(|lhs| lhs.syntax() == expr)
        && !outer.is_infix()
        && outer.func().is_some_and(|f| f.syntax() == op_app.syntax())
}

/// Whether some recorded range contains `r`. `starts` is sorted, and
/// `max_end[i]` is the largest end among the first `i + 1` records.
fn covered(starts: &[TextRange], max_end: &[rowan::TextSize], r: TextRange) -> bool {
    let i = starts.partition_point(|s| s.start() <= r.start());
    i > 0 && max_end[i - 1] >= r.end()
}

/// A head the resolver recorded nothing at.
#[derive(Debug)]
struct SilentHead {
    /// `line:col`.
    at: String,
    name: String,
    bucket: String,
}

/// The silent heads of one file, `parsed` being its clean parse.
fn silent_heads(src: &str, parsed: &Parse) -> Vec<SilentHead> {
    let root = &parsed.root;
    let file = ImplFile::cast(root.clone()).expect("an implementation file");
    let rf = resolve_file(
        &file,
        &ProjectItems::default(),
        &AssemblyEnv::default(),
        &SyntaxRecovery::of(parsed),
    );
    let mut recorded: Vec<TextRange> = rf
        .resolutions()
        .keys()
        .chain(rf.attribute_resolutions().keys())
        .copied()
        .collect();
    recorded.sort_by_key(|r| r.start());
    let max_end: Vec<_> = recorded
        .iter()
        .scan(rowan::TextSize::from(0), |m, r| {
            *m = (*m).max(r.end());
            Some(*m)
        })
        .collect();
    let positions = Positions::new(src);
    let mut out = Vec::new();
    for node in root.descendants() {
        if !matches!(
            node.kind(),
            SyntaxKind::IDENT_EXPR | SyntaxKind::LONG_IDENT_EXPR
        ) {
            continue;
        }
        let tokens: Vec<_> = node
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .collect();
        if tokens.iter().any(|t| t.kind() == SyntaxKind::QMARK_TOK) || is_argument_label(&node) {
            continue;
        }
        let Some(head) = tokens.iter().find(|t| t.kind() == SyntaxKind::IDENT_TOK) else {
            continue;
        };
        let text = head.text();
        let is_name = text
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '`');
        if !is_name || text == "base" {
            continue;
        }
        if covered(&recorded, &max_end, head.text_range()) {
            continue;
        }
        out.push(SilentHead {
            at: positions
                .at(usize::from(head.text_range().start()))
                .to_string(),
            name: text.to_string(),
            bucket: bucket(&node),
        });
    }
    out
}

#[test]
#[ignore = "corpus sweep: needs BORZOI_CORPUS; run with --ignored under nix develop"]
fn silent_regions_over_corpus() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!("BORZOI_CORPUS unset; skipping the silent-region sweep.");
        return;
    };
    let root = PathBuf::from(root);
    let mut files = Vec::new();
    collect_fs(&root, &mut files);
    sort_by_corpus_key(&root, &mut files);
    assert!(!files.is_empty(), "no .fs files under {root:?}");

    let mut items = Vec::new();
    let mut by_bucket: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    let mut clean = 0usize;
    for path in &files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        // A file our parser rejects is the parser gate's; its tree is
        // recovery, and silence there says nothing about the walk.
        let parsed = parse(&src);
        if !parsed.errors.is_empty() {
            continue;
        }
        if ImplFile::cast(parsed.root.clone()).is_none() {
            continue;
        }
        clean += 1;
        let rel = corpus_relative(&root, path);
        for h in silent_heads(&src, &parsed) {
            items.push(format!("{rel} {}", h.bucket));
            let (n, sample) = by_bucket.entry(h.bucket).or_default();
            *n += 1;
            if sample.len() < SAMPLE {
                sample.push(format!("{rel}:{} {:?}", h.at, h.name));
            }
        }
    }
    eprintln!(
        "silent-region sweep: {clean} of {} files parsed clean; {} silent heads",
        files.len(),
        items.len()
    );
    for (b, (n, sample)) in &by_bucket {
        eprintln!("  {b}: {n}");
        for s in sample {
            eprintln!("      {s}");
        }
    }
    assert!(clean > 0, "no corpus file parsed clean");
    let manifest = Manifest::from_counted(items).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check_manifest(
        MANIFEST,
        &manifest,
        &regenerate_ignored("silent_region_sweep"),
    );
}

/// Every head in a body the walk enters has a record: a genuine type's
/// member, an augmentation's (intrinsic and optional) and a module `let`.
#[test]
fn walked_bodies_have_no_silent_heads() {
    let src = "\
module M
let f (a : int) = a
type T() =
    member _.M (p : int) = f p
type T with
    member _.N (q : int) = f q
    static member S (r : int) = f r
module N =
    type T with
        member this.O (s : int) = f (this.M s)
";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let silent = silent_heads(src, &parsed);
    assert!(silent.is_empty(), "{silent:#?}");
}

/// A head is attributed to the innermost container, and a member to its host.
#[test]
fn a_silent_head_is_bucketed_by_its_members_host() {
    let src = "\
module M
type T() =
    member _.M (p : int) = p
type T with
    member _.N (q : int) = q
let o = { new System.Object() with member _.ToString () = string o }
";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let buckets: Vec<(String, String)> = parsed
        .root
        .descendants()
        .filter(|n| {
            matches!(
                n.kind(),
                SyntaxKind::IDENT_EXPR | SyntaxKind::LONG_IDENT_EXPR
            )
        })
        .map(|n| (n.text().to_string().trim().to_string(), bucket(&n)))
        .collect();
    let want = |name: &str, b: &str| (name.to_string(), b.to_string());
    assert_eq!(
        buckets,
        vec![
            want("p", "member-of-type"),
            want("q", "member-of-augmentation"),
            want("string", "member-of-object-expression"),
            want("o", "member-of-object-expression"),
        ]
    );
}

#[test]
fn covered_is_containment_by_any_record() {
    let r = |a: u32, b: u32| TextRange::new(a.into(), b.into());
    let starts = vec![r(0, 10), r(2, 3), r(5, 6)];
    let max_end: Vec<_> = vec![10.into(), 10.into(), 10.into()];
    assert!(covered(&starts, &max_end, r(5, 6)));
    assert!(
        covered(&starts, &max_end, r(8, 9)),
        "inside the first, long record"
    );
    assert!(!covered(&starts, &max_end, r(9, 11)), "past every end");
    let starts = vec![r(2, 3), r(5, 6)];
    let max_end: Vec<_> = vec![3.into(), 6.into()];
    assert!(!covered(&starts, &max_end, r(0, 1)), "before every start");
    assert!(!covered(&starts, &max_end, r(4, 5)), "between records");
}

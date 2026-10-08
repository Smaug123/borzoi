//! Recovery under deliberate damage: take a corpus file our parser reads
//! cleanly, delete one token, reparse, and check what survived.
//!
//! The corpus is well-formed by construction, so the parser corpus gates only
//! ever grade recovery on the few hundred files that are broken on purpose.
//! An editor buffer is broken most of the time it is being edited, and the
//! commonest break is a member access being typed. This sweep manufactures
//! that state from the clean corpus.
//!
//! # The damage
//!
//! Two families of single deletions, each a [`Case`]:
//!
//! * **token** — any significant token, at positions drawn deterministically
//!   per file ([`token_positions`]).
//! * **member** — the member name right after a `.` in an expression, where
//!   it ends the access on its line (`xs.Length` → `xs.`): the `foo.` shape
//!   completion is driven by.
//!
//! # The properties
//!
//! Damage is the deleted token's span widened back to the end of the token
//! before it (see [`Damage`]). The FCS-free properties compare two of our own
//! trees, so they key units by CST node ([`CstUnit`]): the module headers and
//! the declarations, recursively through nested modules.
//!
//! * **Prefix stability** (both families, no FCS): every unit of the clean tree
//!   that lies wholly before the damage is in the damaged tree unchanged — same
//!   key, same range, the same green subtree (for a header, the same name and
//!   attribute nodes). Nothing after a position can change how the text
//!   before it parsed, except through the one token of lookahead the widening
//!   accounts for, and the `in` of a module-level `let … in` (which
//!   [`with_trailing_in`] counts as the declaration's).
//! * **Suffix stability** (member family only, no FCS): every unit wholly
//!   *after* the damage is in the damaged tree too, shifted by the deleted
//!   length. Deleting a member name changes no lexing and no layout, so the
//!   declaration after a `foo.` must survive it. A recovery that swallows the
//!   rest of the file, or eats the next declaration, fails here.
//! * **The recovery relation against FCS** (both families,
//!   [`recovered_trees_match_fcs_under_deletion`]): one file in
//!   [`FCS_FILE_STRIDE`] whose clean trees already agree with FCS, one case of
//!   each family per file, graded with `common::recovery::grade` and pinned in
//!   `tests/manifests/recovery_sweep.txt`.
//!
//! The FCS-free halves are pure. They draw [`TOKEN_POSITIONS_PER_FILE`] and
//! [`MEMBER_POSITIONS_PER_FILE`] cases from every clean file by default;
//! `BORZOI_RECOVERY_SOAK=all` runs every token and every member name of every
//! file instead (a long soak, not a gate).
//!
//! `BORZOI_RECOVERY_EXPLAIN=<path> … explain_recovered_file -- --ignored
//! --nocapture` prints one file's units on both sides, which are damaged, and
//! the verdict: the triage tool for a divergent manifest line.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use borzoi_cst::parser::{Parse, parse_sig_with_symbols, parse_with_symbols};
use borzoi_cst::syntax::{AstNode, ModuleDecl, SigDecl, SyntaxKind, SyntaxNode};
use borzoi_oracle_harness::manifest::Manifest;
use tempfile::NamedTempFile;

use crate::common::corpus_manifest::{
    check_manifest, corpus_relative, regenerate_ignored, sort_by_corpus_key,
};
use crate::common::normalised_ast::{normalise_fcs_dump, normalise_parse};
use crate::common::recovery::{Damage, grade};
use crate::common::{
    catch_unwind_silent, collect_fsharp_corpus_files, corpus_root, fcs_ast_batch,
    fcs_parse_had_errors, read_corpus_source,
};

/// Token positions drawn per file by the default (gating) FCS-free sample.
const TOKEN_POSITIONS_PER_FILE: usize = 2;

/// Member-name deletions drawn per file by the default FCS-free sample.
const MEMBER_POSITIONS_PER_FILE: usize = 2;

/// How many divergent FCS cases to print for investigation.
const DIVERGENCE_SAMPLE: usize = 100;

/// One file in this many is in the FCS sample.
const FCS_FILE_STRIDE: u64 = 12;

/// The deletion families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Token,
    Member,
}

impl Family {
    fn name(self) -> &'static str {
        match self {
            Family::Token => "token",
            Family::Member => "member",
        }
    }
}

/// One deletion: the bytes `deleted` removed from a clean file.
struct Case {
    family: Family,
    deleted: Range<usize>,
}

/// A corpus file our parser reads cleanly.
struct CleanFile {
    path: PathBuf,
    key: String,
    src: String,
    parse: Parse,
}

/// FNV-1a: a stable, dependency-free hash for deterministic sampling.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn symbols_for(path: &Path) -> HashSet<String> {
    let script = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("fsx"));
    let set: &[&str] = if script {
        &["INTERACTIVE", "EDITING"]
    } else {
        &["COMPILED", "EDITING"]
    };
    set.iter().map(|s| s.to_string()).collect()
}

fn is_sig(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("fsi"))
}

fn parse_as(path: &Path, src: &str) -> Option<Parse> {
    let symbols = symbols_for(path);
    catch_unwind_silent(|| {
        if is_sig(path) {
            parse_sig_with_symbols(src, &symbols)
        } else {
            parse_with_symbols(src, &symbols)
        }
    })
    .ok()
}

/// Every corpus file our parser reads without an error, in corpus-key order.
fn clean_files(root: &Path) -> Vec<CleanFile> {
    let mut files = collect_fsharp_corpus_files(root)
        .unwrap_or_else(|err| panic!("walk F# corpus under {}: {err}", root.display()));
    sort_by_corpus_key(root, &mut files);
    files
        .into_iter()
        .filter_map(|path| {
            let key = corpus_relative(root, &path);
            let src = read_corpus_source(&path).ok()?;
            let parse = parse_as(&path, &src)?;
            parse.errors.is_empty().then_some(CleanFile {
                path,
                key,
                src,
                parse,
            })
        })
        .collect()
}

/// Significant (non-trivia, non-empty) tokens in source order.
fn significant_tokens(root: &SyntaxNode) -> Vec<(SyntaxKind, Range<usize>)> {
    root.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia() && !t.text_range().is_empty())
        .map(|t| {
            let r = t.text_range();
            (t.kind(), usize::from(r.start())..usize::from(r.end()))
        })
        .collect()
}

/// Up to `k` distinct indices below `n`, drawn from `key`'s hash.
fn token_positions(key: &str, salt: &str, n: usize, k: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0u64;
    while out.len() < k.min(n) {
        let h = fnv(format!("{key}\0{salt}\0{i}").as_bytes());
        let pos = (h % n as u64) as usize;
        if !out.contains(&pos) {
            out.push(pos);
        }
        i += 1;
    }
    out.sort_unstable();
    out
}

/// The member-name tokens of an expression access: an `IDENT_TOK` inside the
/// `LONG_IDENT` of a `LONG_IDENT_EXPR` / `DOT_GET_EXPR`, glued to the `.` before
/// it, and followed on its line by nothing but a closer or the end of the line,
/// so deleting it leaves `recv.` where a member name is being typed. (`recv. [`
/// and `recv. (` are not that shape: F# reads both as a dotted indexer.)
fn member_name_tokens(file: &CleanFile) -> Vec<Range<usize>> {
    let src = file.src.as_bytes();
    file.parse
        .root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT_TOK)
        .filter(|t| {
            let Some(path) = t.parent() else { return false };
            path.kind() == SyntaxKind::LONG_IDENT
                && path.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        SyntaxKind::LONG_IDENT_EXPR | SyntaxKind::DOT_GET_EXPR
                    )
                })
                && t.prev_token().is_some_and(|d| {
                    d.kind() == SyntaxKind::DOT_TOK
                        && d.text_range().end() == t.text_range().start()
                })
        })
        .map(|t| usize::from(t.text_range().start())..usize::from(t.text_range().end()))
        .filter(|r| {
            src[r.end..]
                .iter()
                .find(|&&c| !matches!(c, b' ' | b'\t'))
                .is_none_or(|&c| matches!(c, b'\r' | b'\n' | b')' | b']' | b'}' | b',' | b';'))
        })
        .collect()
}

/// The cases drawn from `file`: up to `token_k` token deletions and `member_k`
/// member-name deletions (`None`: all of them).
fn cases_for(file: &CleanFile, token_k: Option<usize>, member_k: Option<usize>) -> Vec<Case> {
    let tokens = significant_tokens(&file.parse.root);
    let token_idx: Vec<usize> = match token_k {
        Some(k) => token_positions(&file.key, "token", tokens.len(), k),
        None => (0..tokens.len()).collect(),
    };
    let members = member_name_tokens(file);
    let member_idx: Vec<usize> = match member_k {
        Some(k) => token_positions(&file.key, "member", members.len(), k),
        None => (0..members.len()).collect(),
    };
    token_idx
        .into_iter()
        .map(|i| Case {
            family: Family::Token,
            deleted: tokens[i].1.clone(),
        })
        .chain(member_idx.into_iter().map(|i| Case {
            family: Family::Member,
            deleted: members[i].clone(),
        }))
        .collect()
}

fn delete(src: &str, r: &Range<usize>) -> String {
    format!("{}{}", &src[..r.start], &src[r.end..])
}

/// One unit of our own tree, for comparing two of our trees with each other:
/// a module or nested-module header, or a whole declaration.
///
/// The FCS-faithful ranges of `common::recovery` are not needed here (both
/// sides are ours) and cost several walks per declaration, so this keys and
/// ranges a unit by its CST node directly: the enclosing nested modules' start
/// offsets, its own start, and its kind.
struct CstUnit {
    key: (Vec<usize>, usize, SyntaxKind),
    /// A header spans from its container's start to its first declaration.
    range: Range<usize>,
    node: SyntaxNode,
    header: bool,
}

impl CstUnit {
    /// What must be unchanged: a declaration's whole green subtree; for a
    /// header, the green child nodes inside `range` (name, attributes) — read
    /// through the clean unit's range on both sides, since a damaged first
    /// declaration can leave nodes of its own in front of the next one.
    fn identity(&self, range: &Range<usize>) -> Vec<rowan::GreenNode> {
        if !self.header {
            return vec![self.node.green().into_owned()];
        }
        self.node
            .children()
            .filter(|c| !is_decl(c))
            .filter(|c| {
                let r = significant_range(c);
                range.start <= r.start && r.end <= range.end
            })
            .map(|c| c.green().into_owned())
            .collect()
    }
}

/// The first and last significant tokens' extent of `node`, or its text range
/// when it has none.
fn significant_range(node: &SyntaxNode) -> Range<usize> {
    let significant =
        |t: &borzoi_cst::syntax::SyntaxToken| !t.kind().is_trivia() && !t.text_range().is_empty();
    let first = std::iter::successors(node.first_token(), |t| t.next_token())
        .take_while(|t| t.text_range().start() < node.text_range().end())
        .find(significant);
    let last = std::iter::successors(node.last_token(), |t| t.prev_token())
        .take_while(|t| t.text_range().end() > node.text_range().start())
        .find(significant);
    match (first, last) {
        (Some(f), Some(l)) => {
            usize::from(f.text_range().start())..usize::from(l.text_range().end())
        }
        _ => usize::from(node.text_range().start())..usize::from(node.text_range().end()),
    }
}

fn is_decl(node: &SyntaxNode) -> bool {
    ModuleDecl::can_cast(node.kind()) || SigDecl::can_cast(node.kind())
}

/// A module-level `let … in body` leaves its `in` outside the `LET_DECL` node,
/// but the declaration is not finished until the body after it: deleting a
/// token there can still change it. So its range runs over the `in`.
fn with_trailing_in(decl: &SyntaxNode, range: Range<usize>) -> Range<usize> {
    let next = std::iter::successors(decl.last_token(), |t| t.next_token())
        .skip(1)
        .find(|t| !t.kind().is_trivia() && !t.text_range().is_empty());
    match next {
        Some(t) if t.kind() == SyntaxKind::IN_TOK => range.start..usize::from(t.text_range().end()),
        _ => range,
    }
}

/// A header unit's range: its container's start to its first declaration's.
fn header_range(container: &SyntaxNode) -> Range<usize> {
    let whole = significant_range(container);
    let first = container
        .children()
        .find(is_decl)
        .map_or(whole.end, |d| significant_range(&d).start);
    whole.start..first.max(whole.start)
}

fn cst_units(parse: &Parse) -> Vec<CstUnit> {
    fn walk(container: &SyntaxNode, scope: &mut Vec<usize>, out: &mut Vec<CstUnit>) {
        for child in container.children().filter(is_decl) {
            let range = significant_range(&child);
            if child.kind() == SyntaxKind::NESTED_MODULE_DECL {
                out.push(CstUnit {
                    key: (scope.clone(), range.start, child.kind()),
                    range: header_range(&child),
                    node: child.clone(),
                    header: true,
                });
                scope.push(range.start);
                walk(&child, scope, out);
                scope.pop();
            } else {
                out.push(CstUnit {
                    key: (scope.clone(), range.start, child.kind()),
                    range: with_trailing_in(&child, range),
                    node: child.clone(),
                    header: false,
                });
            }
        }
    }
    let mut out = Vec::new();
    for (i, module) in parse
        .root
        .children()
        .filter(|c| c.kind() == SyntaxKind::MODULE_OR_NAMESPACE)
        .enumerate()
    {
        out.push(CstUnit {
            key: (Vec::new(), i, SyntaxKind::MODULE_OR_NAMESPACE),
            range: header_range(&module),
            node: module.clone(),
            header: true,
        });
        walk(&module, &mut vec![i], &mut out);
    }
    out
}

/// `key` with every offset past the deletion moved back by its length (the
/// first scope entry is a module ordinal, not an offset).
fn shifted(
    key: &(Vec<usize>, usize, SyntaxKind),
    deleted: &Range<usize>,
) -> (Vec<usize>, usize, SyntaxKind) {
    let len = deleted.end - deleted.start;
    let shift = |at: usize| if at >= deleted.end { at - len } else { at };
    let (scope, start, kind) = key;
    let scope = scope
        .iter()
        .enumerate()
        .map(|(i, &at)| if i == 0 { at } else { shift(at) })
        .collect::<Vec<_>>();
    let start = if scope.is_empty() {
        *start
    } else {
        shift(*start)
    };
    (scope, start, *kind)
}

/// The FCS-free violations of `case` against `file`, as messages.
fn stability_violations(file: &CleanFile, clean: &[CstUnit], case: &Case) -> Vec<String> {
    let damaged_src = delete(&file.src, &case.deleted);
    let Some(damaged) = parse_as(&file.path, &damaged_src) else {
        return vec!["damaged source panics the parser".to_string()];
    };
    assert_eq!(
        damaged.root.text().to_string(),
        damaged_src,
        "the damaged parse must round-trip"
    );
    let damage = Damage::new(&file.parse, [case.deleted.clone()]);
    let (first, last) = (
        damage.start().expect("one span"),
        damage.end().expect("one span"),
    );
    let after: BTreeMap<_, CstUnit> = cst_units(&damaged)
        .into_iter()
        .map(|u| (u.key.clone(), u))
        .collect();
    let len = case.deleted.end - case.deleted.start;
    let mut out = Vec::new();
    for unit in clean {
        let before_damage = unit.range.end < first;
        let after_damage = unit.range.start > last;
        if !(before_damage || (after_damage && case.family == Family::Member)) {
            continue;
        }
        let (key, delta) = if before_damage {
            (unit.key.clone(), 0)
        } else {
            (shifted(&unit.key, &case.deleted), len)
        };
        let side = if before_damage { "prefix" } else { "suffix" };
        let moved = unit.range.start - delta..unit.range.end - delta;
        match after.get(&key) {
            Some(d)
                if (unit.header || d.range == moved)
                    && d.identity(&moved) == unit.identity(&unit.range) => {}
            Some(d) => out.push(format!(
                "{side} {}: {:?} at {:?} changed (now {:?})",
                case.family.name(),
                unit.key.2,
                unit.range,
                d.range
            )),
            None => out.push(format!(
                "{side} {}: {:?} at {:?} is gone",
                case.family.name(),
                unit.key.2,
                unit.range
            )),
        }
    }
    out
}

fn soak_all() -> bool {
    std::env::var("BORZOI_RECOVERY_SOAK").is_ok_and(|v| v == "all")
}

/// Prefix stability for both families and suffix stability for the member
/// family, over the clean corpus. FCS-free.
#[test]
#[ignore = "corpus sweep; run with --ignored under nix develop"]
fn deletion_keeps_undamaged_declarations() {
    let root = corpus_root();
    let files = clean_files(&root);
    assert!(!files.is_empty(), "no clean corpus files under {root:?}");
    let (token_k, member_k) = if soak_all() {
        (None, None)
    } else {
        (
            Some(TOKEN_POSITIONS_PER_FILE),
            Some(MEMBER_POSITIONS_PER_FILE),
        )
    };
    let mut cases = 0usize;
    let mut violations: Vec<String> = Vec::new();
    let started = std::time::Instant::now();
    for file in &files {
        let clean = cst_units(&file.parse);
        for case in cases_for(file, token_k, member_k) {
            cases += 1;
            for v in stability_violations(file, &clean, &case) {
                violations.push(format!("{}@{} {v}", file.key, case.deleted.start));
            }
        }
    }
    eprintln!(
        "deletion stability: {} clean files, {cases} cases, {} violations, {:.1?}",
        files.len(),
        violations.len(),
        started.elapsed()
    );
    for v in violations.iter().take(60) {
        eprintln!("  {v}");
    }
    assert!(
        violations.is_empty(),
        "{} declarations outside the damage changed or vanished",
        violations.len()
    );
}

/// The recovery relation against FCS, over a deterministic sample of files
/// whose clean trees already agree.
#[test]
#[ignore = "corpus sweep against FCS; run with --ignored under nix develop"]
fn recovered_trees_match_fcs_under_deletion() {
    let root = corpus_root();
    let files = clean_files(&root);
    let mut entries: Vec<String> = Vec::new();
    let mut baselines = 0usize;
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    let mut divergences: Vec<String> = Vec::new();
    for file in files
        .iter()
        .filter(|f| fnv(f.key.as_bytes()).is_multiple_of(FCS_FILE_STRIDE))
    {
        // The baseline: FCS reads the clean file cleanly, and both trees
        // agree. Otherwise a divergence under damage says nothing new.
        let clean_json = fcs_ast_batch(&file.path);
        if fcs_parse_had_errors(&clean_json) {
            continue;
        }
        // FCS's side first: its JSON read fails cleanly on a tree too deep
        // for `serde_json`, which keeps our own recursive projections off
        // input deep enough to overflow the stack.
        let Ok(fcs_norm) = catch_unwind_silent(|| normalise_fcs_dump(&clean_json)) else {
            continue;
        };
        if catch_unwind_silent(|| normalise_parse(&file.parse)).ok() != Some(fcs_norm) {
            continue;
        }
        baselines += 1;
        let ext = file
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("fs");
        for case in cases_for(file, Some(1), Some(1)) {
            let damaged_src = delete(&file.src, &case.deleted);
            let mut tmp = NamedTempFile::with_suffix(format!(".{ext}")).expect("tempfile");
            std::io::Write::write_all(&mut tmp, damaged_src.as_bytes()).expect("write");
            let json = fcs_ast_batch(tmp.path());
            let damaged = parse_as(&file.path, &damaged_src).expect("damaged parse panicked");
            let token = match catch_unwind_silent(|| grade(&damaged, &json, &damaged_src)) {
                Ok(Ok(v)) => {
                    if let Some(d) = &v.first_divergence {
                        divergences.push(format!(
                            "{}@{} {}: {}",
                            file.key,
                            case.deleted.start,
                            case.family.name(),
                            d.lines().next().unwrap_or("")
                        ));
                    }
                    v.token()
                }
                Ok(Err(_)) => "fcs-unreadable".to_string(),
                Err(_) => "grade-panicked".to_string(),
            };
            *tally
                .entry(format!(
                    "{} {}",
                    case.family.name(),
                    token.split(' ').next().unwrap()
                ))
                .or_default() += 1;
            entries.push(format!(
                "{}@{} {} {token}",
                file.key,
                case.deleted.start,
                case.family.name()
            ));
        }
    }
    eprintln!(
        "recovery vs FCS: {baselines} baseline files, {} cases",
        entries.len()
    );
    for (k, n) in &tally {
        eprintln!("  {n:5} {k}");
    }
    for d in divergences.iter().take(DIVERGENCE_SAMPLE) {
        eprintln!("  divergent: {d}");
    }
    let manifest =
        Manifest::from_entries(entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    check_manifest(
        "recovery_sweep",
        &manifest,
        &regenerate_ignored("recovery_sweep"),
    );
}

/// Print one file's units on both sides, the damage, and the verdict. The
/// triage tool for a divergent line in either manifest; a no-op unless
/// `BORZOI_RECOVERY_EXPLAIN` names a file.
#[test]
#[ignore = "triage tool; set BORZOI_RECOVERY_EXPLAIN=<path>"]
fn explain_recovered_file() {
    use crate::common::recovery::{fcs_error_spans, fcs_units, our_units};
    let Some(path) = std::env::var_os("BORZOI_RECOVERY_EXPLAIN").map(PathBuf::from) else {
        return;
    };
    let src = std::fs::read_to_string(&path).expect("read the file to explain");
    let parse = parse_as(&path, &src).expect("our parser panicked");
    let json = fcs_ast_batch(&path);
    let fcs_errors = fcs_error_spans(&json, &src);
    for e in &parse.errors {
        eprintln!(
            "our error {:?}: {}",
            e.span,
            e.message.lines().next().unwrap_or("")
        );
    }
    eprintln!("fcs errors: {fcs_errors:?}");
    let damage = Damage::new(
        &parse,
        parse
            .errors
            .iter()
            .map(|e| e.span.clone())
            .chain(fcs_errors),
    );
    let fcs = fcs_units(&json, &src).expect("FCS record is readable");
    for (side, units) in [("fcs ", fcs), ("ours", our_units(&parse))] {
        for u in units {
            let s = u.range.start.min(src.len());
            let e = u.range.end.min(src.len()).min(s + 60).max(s);
            eprintln!(
                "{side} {} {:?} damaged={} {:?}{}",
                u.key,
                u.range,
                damage.touches(&u.range),
                &src[s..e],
                match &u.shape {
                    Ok(_) => String::new(),
                    Err(e) => format!(" unmodelled: {}", e.lines().next().unwrap_or("")),
                }
            );
        }
    }
    eprintln!("{:#?}", grade(&parse, &json, &src));
}

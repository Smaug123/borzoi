//! Resolution and inference survive the tallest tree the parser returns, on
//! half of [`WALK_STACK_BYTES`].
//!
//! Both walks recurse once per level of the tree, and a stack overflow aborts
//! the process: the language server with every open document. The parser
//! bounds the tree's height ([`MAX_TREE_HEIGHT`]), so the stack a walk needs
//! is bounded too, by its cost per level times that height; what this checks
//! is that the bound sits inside [`WALK_STACK_BYTES`] with a margin of two.
//!
//! Every repetition form of the grammar
//! ([`repetition_shapes`](borzoi_oracle_harness::repetition_shapes)), alone and
//! under a stack of nested modules, is walked in two forms:
//!
//! - **long**: built past the bound, so the parse stops there with its depth
//!   diagnostic. Resolution walks the whole tree whatever the parse
//!   reported; inference declines a declaration a recovery touched, so this
//!   form reaches only resolution's depth.
//! - **tallest clean**: the longest repeat count the parser accepts without a
//!   diagnostic, which inference does walk.
//!
//! Each case runs in a child copy of this binary
//! ([`stack_probe`](borzoi_oracle_harness::stack_probe)), on a thread with
//! half of [`WALK_STACK_BYTES`], so an overflow names its case instead of
//! aborting the suite. A walk whose cost per level grows past the margin fails
//! here, naming the shape that grew it.
//!
//! A walk that gives up near the top of a tree survives any stack, so the
//! cases bind the names and operators the shapes use
//! ([`PRELUDE`](borzoi_oracle_harness::repetition_shapes::PRELUDE)), and how
//! deep each walk reached into each case (the deepest occurrence resolution
//! recorded, and the most expression levels inference had open) is pinned
//! exactly in `tests/manifests/walk_stack_bound.txt`.

use std::path::PathBuf;

use borzoi_cst::parser::{MAX_TREE_HEIGHT, Parse, parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile, SyntaxNode};
use borzoi_oracle_harness::manifest::{self, Manifest};
use borzoi_oracle_harness::par_map::par_map;
use borzoi_oracle_harness::repetition_shapes::{Case, Form, Kind, cases, tallest_clean_count};
use borzoi_oracle_harness::stack_probe::{self, Outcome};
use borzoi_sema::{
    AssemblyEnv, ProjectFile, SourceFile, SyntaxRecovery, WALK_STACK_BYTES, infer_project,
    qualified_names, resolve_project_files,
};
use rowan::{GreenNode, NodeOrToken, WalkEvent};

/// The stack every case is walked on: half of [`WALK_STACK_BYTES`], the
/// margin the bound promises.
const PROBE_STACK_BYTES: usize = WALK_STACK_BYTES / 2;

/// The child entry's full test path, for [`stack_probe::run_case`].
const CHILD: &str = "walk_stack_bound::probe_child";

/// The variable that hands a child its case.
const CASE_VAR: &str = "BORZOI_SEMA_WALK_STACK_CASE";

/// Node height of `root`, the root counting as 1. Iterative, so measuring a
/// tall tree cannot overflow.
fn tree_height(root: &SyntaxNode) -> usize {
    let mut depth = 0usize;
    let mut height = 0usize;
    for event in root.preorder() {
        match event {
            WalkEvent::Enter(_) => {
                depth += 1;
                height = height.max(depth);
            }
            WalkEvent::Leave(_) => depth -= 1,
        }
    }
    height
}

fn parse_kind(kind: Kind, src: &str) -> Parse {
    match kind {
        Kind::Impl => parse(src),
        Kind::Sig => parse_sig(src),
    }
}

/// The parse of `case` its form asks for.
fn case_parse(case: Case) -> Parse {
    let kind = case.shape.kind;
    let n = match case.form {
        Form::Long => case.long(),
        Form::TallestClean => {
            tallest_clean_count(&case, |src| parse_kind(kind, src).errors.is_empty())
        }
    };
    parse_kind(kind, &case.source(n))
}

/// Resolve and infer the tree `green` as a one-file project, the fold the
/// language server runs on every file, and return how deep each walk reached:
/// the node depth of the deepest occurrence resolution recorded (0 for none),
/// and the most expression levels inference had open at once.
fn walk(kind: Kind, green: GreenNode, recovery: SyntaxRecovery) -> (usize, usize) {
    let root = SyntaxNode::new_root(green);
    let file = match kind {
        Kind::Impl => SourceFile::Impl(ImplFile::cast(root.clone()).expect("impl root")),
        Kind::Sig => SourceFile::Sig(SigFile::cast(root.clone()).expect("sig root")),
    };
    let path = PathBuf::from(match kind {
        Kind::Impl => "/probe/Probe.fs",
        Kind::Sig => "/probe/Probe.fsi",
    });
    let qnof = qualified_names(std::slice::from_ref(&file), &[path])
        .pop()
        .expect("one name per file");
    let files = vec![ProjectFile::new(file, qnof, recovery)];
    let env = AssemblyEnv::default();
    let resolved = resolve_project_files(&files, &env);
    let inferred = infer_project(&files, &resolved, &env);
    let resolved_depth = resolved
        .file(0)
        .resolutions()
        .keys()
        .map(|&range| match root.covering_element(range) {
            NodeOrToken::Node(node) => node.ancestors().count(),
            NodeOrToken::Token(token) => token.parent_ancestors().count(),
        })
        .max()
        .unwrap_or(0);
    let inferred_nesting = inferred
        .file(0)
        .map_or(0, |file| file.deepest_expression_walk());
    (resolved_depth, inferred_nesting)
}

/// What a child reports of its case: the tree it walked, and how deep each
/// walk reached into it ([`walk`]).
#[derive(Debug, PartialEq, Eq)]
struct Facts {
    height: usize,
    clean: bool,
    resolved_depth: usize,
    inferred_nesting: usize,
}

impl Facts {
    fn spell(&self) -> String {
        format!(
            "{} {} {} {}",
            self.height, self.clean, self.resolved_depth, self.inferred_nesting
        )
    }

    fn read(s: &str) -> Facts {
        let fields: Vec<&str> = s.split(' ').collect();
        let [height, clean, resolved_depth, inferred_nesting] = fields[..] else {
            panic!("four fields: {s:?}")
        };
        Facts {
            height: height.parse().expect("a height"),
            clean: clean.parse().expect("a bool"),
            resolved_depth: resolved_depth.parse().expect("a depth"),
            inferred_nesting: inferred_nesting.parse().expect("a nesting"),
        }
    }

    /// The case's manifest entry.
    fn entry(&self, case: &Case) -> String {
        let stopped = if self.clean { "" } else { " (stopped)" };
        format!(
            "{}\theight {}{stopped}, resolved at depth {}, inference nested {}",
            case.name(),
            self.height,
            self.resolved_depth,
            self.inferred_nesting
        )
    }
}

/// A case spelled for the child: its index and the stack to walk it on.
fn spell_case(index: usize, stack_bytes: usize) -> String {
    format!("{index} {stack_bytes}")
}

/// Child entry: resolve and infer one case on a thread of the stack the parent
/// asked for, then report the tree it walked.
///
/// The case is parsed first, on a thread of the full [`WALK_STACK_BYTES`]: the
/// parser's own recursion is bounded and tested in `borzoi-cst`, and only the
/// walks are measured here. Its tree crosses to the walk's thread as the
/// (`Send`) green tree, and this thread keeps a handle on it, so the walk's
/// thread only drops a reference rather than the tree.
#[test]
#[ignore = "child-process entry for walk_stack_bound"]
fn probe_child() {
    let Some(spec) = stack_probe::requested_case(CASE_VAR) else {
        return;
    };
    let (index, stack_bytes) = spec.split_once(' ').expect("index and stack");
    let case = cases()[index.parse::<usize>().expect("a case index")];
    let stack_bytes: usize = stack_bytes.parse().expect("a stack size");
    let (green, recovery, height, clean) = stack_probe::on_stack(WALK_STACK_BYTES, move || {
        let parsed = case_parse(case);
        let recovery = SyntaxRecovery::of(&parsed);
        let height = tree_height(&parsed.root);
        let clean = parsed.errors.is_empty();
        (parsed.root.green().to_owned(), recovery, height, clean)
    });
    let tree = green.clone();
    let (resolved_depth, inferred_nesting) =
        stack_probe::on_stack(stack_bytes, move || walk(case.shape.kind, tree, recovery));
    drop(green);
    let facts = Facts {
        height,
        clean,
        resolved_depth,
        inferred_nesting,
    };
    stack_probe::report(&facts.spell());
}

/// The tallest clean tree among the cases must come this close to
/// [`MAX_TREE_HEIGHT`], or the cases do not reach the height the bound is
/// about. (The parse stops a little short of the bound, and a clean tree sits
/// below the stop.)
const CLEAN_REACH: usize = MAX_TREE_HEIGHT - 128;

#[test]
fn every_walk_fits_half_the_walk_stack() {
    let cases = cases();
    let outcomes = par_map(cases.len(), |i| {
        stack_probe::run_case(CHILD, CASE_VAR, &spell_case(i, PROBE_STACK_BYTES))
    });
    let mut overflowed = Vec::new();
    let mut entries = Vec::new();
    let (mut tallest, mut tallest_clean) = (0, 0);
    let (mut deepest_resolved, mut deepest_inferred) = (0, 0);
    for (case, outcome) in cases.iter().zip(&outcomes) {
        match outcome {
            Outcome::Overflowed => overflowed.push(case.name()),
            Outcome::Survived(facts) => {
                let facts = Facts::read(facts);
                tallest = tallest.max(facts.height);
                if facts.clean {
                    tallest_clean = tallest_clean.max(facts.height);
                }
                deepest_resolved = deepest_resolved.max(facts.resolved_depth);
                deepest_inferred = deepest_inferred.max(facts.inferred_nesting);
                entries.push(facts.entry(case));
            }
        }
    }
    assert!(
        overflowed.is_empty(),
        "these cases overflowed a {} MiB stack, half of WALK_STACK_BYTES: some walk's \
         cost per tree level has outgrown the margin (shrink the frame on its \
         recursion, or raise WALK_STACK_BYTES, which every caller's thread follows):\n{}",
        PROBE_STACK_BYTES >> 20,
        overflowed.join("\n")
    );
    // How deep inference reaches into each case is pinned exactly: a walk that
    // gives up near the top survives any stack, so a case it stopped reaching
    // would otherwise pass this test vacuously.
    let manifest =
        Manifest::from_entries(entries).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    manifest::check(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/manifests/walk_stack_bound.txt"),
        &manifest,
        "BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-sema --test all \
         walk_stack_bound::every_walk_fits_half_the_walk_stack",
    );
    assert!(
        [tallest, tallest_clean, deepest_resolved, deepest_inferred]
            .iter()
            .all(|&reach| reach >= CLEAN_REACH),
        "no case reached the height the bound is about: tallest tree {tallest}, tallest \
         clean tree {tallest_clean}, deepest resolution {deepest_resolved}, deepest \
         inference nesting {deepest_inferred}, against {CLEAN_REACH}"
    );
}

/// For calibrating [`WALK_STACK_BYTES`]: prints each case's tree and the least
/// stack, to 16 KiB, it survives on (`!` marks a tree the parse stopped).
/// `BORZOI_SEMA_WALK_STACK_ONLY` narrows the cases to names starting with any
/// of its comma-separated prefixes.
#[test]
#[ignore = "calibration report"]
fn report_walk_stack() {
    const GRAIN: usize = 16 << 10;
    let only = std::env::var("BORZOI_SEMA_WALK_STACK_ONLY").ok();
    let all = cases();
    let picked: Vec<usize> = (0..all.len())
        .filter(|&i| {
            only.as_deref()
                .is_none_or(|o| o.split(',').any(|p| all[i].name().starts_with(p)))
        })
        .collect();
    let rows = par_map(picked.len(), |k| {
        let i = picked[k];
        let run = |stack| stack_probe::run_case(CHILD, CASE_VAR, &spell_case(i, stack));
        let (mut lo, mut hi) = (GRAIN, WALK_STACK_BYTES * 4);
        let Outcome::Survived(facts) = run(hi) else {
            return format!("{:<48} overflows {} MiB", all[i].name(), hi >> 20);
        };
        while hi - lo > GRAIN {
            let mid = (lo + hi) / 2 / GRAIN * GRAIN;
            match run(mid) {
                Outcome::Survived(_) => hi = mid,
                Outcome::Overflowed => lo = mid,
            }
        }
        let facts = Facts::read(&facts);
        format!(
            "{:<48} height {:>4}{} resolved {:>4} inferred {:>4} needs {:>6} KiB",
            all[i].name(),
            facts.height,
            if facts.clean { " " } else { "!" },
            facts.resolved_depth,
            facts.inferred_nesting,
            hi >> 10
        )
    });
    for row in rows {
        println!("{row}");
    }
}

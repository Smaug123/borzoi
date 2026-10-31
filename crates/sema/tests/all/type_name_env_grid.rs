//! Which `Action` a type annotation names, against FCS, over every short
//! sequence of the writes that put a type of that name into scope.
//!
//! FCS's type-name environment is last-write-wins across kinds: a `type`
//! declaration, an `open` of a module or namespace declaring the name, an
//! `[<AutoOpen>]` module (folded into its parent, and folded again by each
//! later block of the same namespace), and a namespace block boundary each
//! write it. Among the writes it then prefers the written arity, and a bare
//! name prefers a non-generic type. The resolver's in-file tier commits the
//! latest in-file declaration of the name, so each write it does not model
//! must make it decline instead.
//!
//! The grid enumerates every sequence of up to three writes from that
//! alphabet — an in-file `Action` or `Action<'a>`, a module declaring one,
//! the same `[<AutoOpen>]`, an `open` of the latest module, `open System`
//! (which declares `Action` at arities 0 to 16), a new `namespace N` block, a
//! block of the child namespace `N.Inner` —
//! followed by a signature naming `Action` bare or as `Action<int>`. Every
//! commitment must be FCS's; and where the only in-file `Action` is the last
//! write, at the written arity, the use must be committed, so the grid cannot
//! pass by declining. `BORZOI_TYPE_ENV_DEPTH=<n>` lengthens the sequences.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::common::{
    CensusDecl, census_resolve_uses, env_usize_or, full_bcl_env, invoke_fcs_dump_census,
    parse_census_jsonl, temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{ProjectItems, Resolution, SyntaxRecovery, resolve_file};
use rowan::TextRange;

/// One write into the type-name environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Write {
    /// `type Action = …` (arity 0) or `type Action<'a> = …` (arity 1).
    Decl(usize),
    /// `module Mi = type Action… = …`, auto-opened or not.
    Module { arity: usize, auto_open: bool },
    /// `open N.Mi`, the latest module.
    OpenModule,
    /// `open System`.
    OpenSystem,
    /// A new `namespace N` block.
    Block,
    /// A new block of the child namespace `N.Inner`, which does not see
    /// `N`'s types unopened.
    ChildBlock,
}

const ALPHABET: [Write; 10] = [
    Write::Decl(0),
    Write::Decl(1),
    Write::Module {
        arity: 0,
        auto_open: false,
    },
    Write::Module {
        arity: 1,
        auto_open: false,
    },
    Write::Module {
        arity: 0,
        auto_open: true,
    },
    Write::Module {
        arity: 1,
        auto_open: true,
    },
    Write::OpenModule,
    Write::OpenSystem,
    Write::Block,
    Write::ChildBlock,
];

/// The names the writes declare: `Action`, which `open System` also brings at
/// arities 0 to 16, and `int`, which FSharp.Core's implicit opens already
/// declare (at arity 0, and as `int<'Measure>`).
const NAMES: [&str; 2] = ["Action", "int"];

fn type_params(arity: usize) -> &'static str {
    if arity == 0 { "" } else { "<'a>" }
}

struct Program {
    src: String,
    /// The `Action` the final signature writes.
    use_range: TextRange,
    must_commit: bool,
}

/// Render `writes` and a final use of `Action` at `use_arity`, or `None` when
/// the sequence opens a module before any is declared.
fn render(name: &str, writes: &[Write], use_arity: usize) -> Option<Program> {
    let mut src = String::from("namespace N\n");
    let mut namespace = "N";
    // Each module's full path, in declaration order.
    let mut modules: Vec<String> = Vec::new();
    for (i, w) in writes.iter().enumerate() {
        match *w {
            Write::Decl(a) => {
                let field = if a == 0 { "string" } else { "'a" };
                writeln!(src, "type {name}{} = {{ D{i}: {field} }}", type_params(a)).unwrap();
            }
            Write::Module { arity, auto_open } => {
                let module = format!("M{}", modules.len() + 1);
                if auto_open {
                    src.push_str("[<AutoOpen>]\n");
                }
                let field = if arity == 0 { "string" } else { "'a" };
                writeln!(
                    src,
                    "module {module} =\n    type {name}{} = {{ E{i}: {field} }}",
                    type_params(arity)
                )
                .unwrap();
                modules.push(format!("{namespace}.{module}"));
            }
            Write::OpenModule => {
                let path = modules.last()?;
                writeln!(src, "open {path}").unwrap();
            }
            Write::OpenSystem => src.push_str("open System\n"),
            Write::Block => {
                namespace = "N";
                src.push_str("namespace N\n");
            }
            Write::ChildBlock => {
                namespace = "N.Inner";
                src.push_str("namespace N.Inner\n");
            }
        }
    }
    src.push_str("type I =\n    abstract M : ");
    let start = src.len();
    src.push_str(name);
    let use_range = range_of(start, src.len());
    src.push_str(if use_arity == 0 { "" } else { "<int>" });
    src.push_str(" -> unit\n");
    let decls: Vec<usize> = writes
        .iter()
        .filter_map(|w| match w {
            Write::Decl(a) => Some(*a),
            _ => None,
        })
        .collect();
    let must_commit = decls.len() == 1
        && writes.last() == Some(&Write::Decl(use_arity))
        && !writes.iter().any(|w| matches!(w, Write::Module { .. }));
    Some(Program {
        src,
        use_range,
        must_commit,
    })
}

fn range_of(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

fn programs() -> Vec<Program> {
    let mut sequences: Vec<Vec<Write>> = vec![Vec::new()];
    let mut frontier = sequences.clone();
    for _ in 0..env_usize_or("BORZOI_TYPE_ENV_DEPTH", 3) {
        frontier = frontier
            .iter()
            .flat_map(|s| {
                ALPHABET.iter().map(move |w| {
                    let mut s = s.clone();
                    s.push(*w);
                    s
                })
            })
            .collect();
        sequences.extend(frontier.iter().cloned());
    }
    sequences
        .iter()
        .flat_map(|s| {
            NAMES
                .iter()
                .flat_map(move |name| [0, 1].into_iter().filter_map(move |a| render(name, s, a)))
        })
        .collect()
}

#[test]
fn in_file_type_names_agree_with_fcs_over_every_write_sequence() {
    let programs = programs();
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|p| temp_fs_file("type_name_env", &p.src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), programs.len(), "one census line per program");

    let mut wrong = Vec::new();
    let mut undercommitted = Vec::new();
    let mut must_commit_points = 0usize;
    let mut committed_uses = 0usize;
    for (program, file) in programs.iter().zip(&census) {
        let src = &program.src;
        let clean = file.ok && !file.has_check_errors;
        let parsed = parse(src);
        let recovery = SyntaxRecovery::of(&parsed);
        let impl_file = ImplFile::cast(parsed.root).expect("impl file");
        let rf = resolve_file(
            &impl_file,
            &ProjectItems::default(),
            full_bcl_env(),
            &recovery,
        );
        let uses = census_resolve_uses(file, src);
        for (range, res) in rf.resolutions() {
            let range = *range;
            let at: Vec<_> = match res {
                Resolution::Local(_) | Resolution::Item(_) => uses
                    .iter()
                    .filter(|u| range_of(u.start, u.end) == range)
                    .collect(),
                // FCS spans a dotted type path from its first segment; the
                // two share an end.
                Resolution::Entity(_) | Resolution::Member { .. } => uses
                    .iter()
                    .filter(|u| {
                        u.end == usize::from(range.end()) && u.start <= range.start().into()
                    })
                    .collect(),
                Resolution::Deferred(_) => continue,
                Resolution::Unresolved => {
                    wrong.push(format!("Unresolved at {range:?}\n{src}"));
                    continue;
                }
            };
            let ours = rf.resolved_def(*res).map(|d| d.range);
            if at.is_empty() && (ours == Some(range) || !clean) {
                continue;
            }
            let agrees = !at.is_empty()
                && at.iter().all(|u| match (res, u.decl) {
                    (_, CensusDecl::Absent) => true,
                    (Resolution::Local(_) | Resolution::Item(_), CensusDecl::InFile(s, e)) => {
                        ours == Some(range_of(s, e))
                    }
                    (Resolution::Entity(_) | Resolution::Member { .. }, CensusDecl::OtherFile) => {
                        true
                    }
                    _ => false,
                });
            if !agrees {
                wrong.push(format!(
                    "{:?} at {range:?}: we gave {res:?} ({ours:?}), FCS {:?}\n{src}",
                    &src[range],
                    at.iter().map(|u| u.decl).collect::<Vec<_>>()
                ));
            } else if range == program.use_range {
                committed_uses += 1;
            }
        }
        if clean && program.must_commit {
            must_commit_points += 1;
            let committed = rf
                .resolution_at(program.use_range)
                .is_some_and(|r| matches!(r, Resolution::Local(_)));
            if !committed {
                undercommitted.push(src.clone());
            }
        }
    }
    eprintln!(
        "type-name environment grid: {} programs, {must_commit_points} must-commit, \
         {committed_uses} uses committed",
        programs.len()
    );
    assert!(
        wrong.is_empty(),
        "{} commitments FCS does not make:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    assert!(
        undercommitted.is_empty(),
        "{} must-commit uses were declined:\n{}",
        undercommitted.len(),
        undercommitted.join("\n")
    );
    assert!(
        must_commit_points >= 52,
        "the must-commit set shrank to {must_commit_points}"
    );
}

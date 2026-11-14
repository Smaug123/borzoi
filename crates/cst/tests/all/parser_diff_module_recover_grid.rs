//! FCS's error recovery inside a nested module body, graded by where each
//! declaration FCS keeps lands: for every cell, each named declaration FCS
//! keeps must be ours too, of the same kind and in the same module.
//!
//! FsYacc takes default reductions before it detects an error, suppresses new
//! errors until three tokens have shifted, and discards tokens flat in panic
//! mode (`Facilities/prim-parsing.fs`). In a module body that gives FCS's
//! recovery its shape: whether the body's declaration list can take the
//! offending token, whether the error is fresh, and which `OBLOCKEND` comes
//! next decide whether the body closes there, closes at the next block end,
//! or swallows the declarations after it. The dimensions are those inputs:
//! - the list's state before the junk: at the body's start, after a
//!   definition, after an expression, after a `;;`, inside a bracket a
//!   definition's right-hand side left open (closed by a bracket of another
//!   kind or not), or after an error inside a `match` clause in one;
//! - the junk: each definition starter, and tokens that start nothing;
//! - whether the junk shares the prefix's line;
//! - an earlier error (`)`) some tokens before the junk, or none;
//! - whether a line deeper than the body follows the junk;
//! - the nesting depth, and the host: a named file, an anonymous file, or
//!   a `begin … end` body.
//!
//! Each divergent cell and name is pinned in
//! `tests/manifests/module_recover_grid.txt`. A declaration FCS keeps that we
//! lose, or keep as another kind, may be lost only where `main`'s parser loses
//! it too (`module_recover_grid.base.txt`, `tools/cst-grid-base.sh`); one we
//! keep in another module is pinned but not held to `main`, unless FCS
//! accepts the cell: clean input is held exactly
//! (`docs/parser-recovery-oracle.md`, "What recovery must match").

use crate::common::recovery::{Unit, fcs_units, our_units};

/// The body's declaration list before the junk.
const PREFIXES: &[(&str, &str)] = &[
    ("start", ""),
    ("after-def", "let a = 0"),
    ("after-expr", "a"),
    ("after-sep", "let a = 0;;"),
    ("in-open-paren", "let a = (0"),
    ("in-mismatched-paren", "let a = (0 ]"),
    ("in-match-clause", "let a = match 0 with | _ -> else 1"),
];

/// What follows the prefix: definition starters, then tokens that start no
/// declaration.
const JUNK: &[&str] = &[
    "type T = int",
    "module B = List",
    "open System",
    "exception E",
    "let b = 1",
    ")",
    "]",
    ";;",
    "end",
    "->",
    "with",
];

/// The declarations graded, by the text that starts them.
const NAMES: &[&str] = &[
    "let a",
    "type T",
    "module B",
    "open System",
    "exception E",
    "let b",
    "let x",
    "let y",
    "let z",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Host {
    Named,
    Anonymous,
    Begin,
}

impl Host {
    fn label(self) -> &'static str {
        match self {
            Host::Named => "named",
            Host::Anonymous => "anon",
            Host::Begin => "begin",
        }
    }
}

struct Cell {
    key: String,
    source: String,
    /// The modules enclosing a declaration, outermost first, by nesting.
    modules: Vec<&'static str>,
}

fn cells() -> Vec<Cell> {
    let mut out = Vec::new();
    for host in [Host::Named, Host::Anonymous, Host::Begin] {
        for depth in 1..=3usize {
            for &(prefix_label, prefix) in PREFIXES {
                for &junk in JUNK {
                    for same_line in [false, true] {
                        if same_line && prefix.is_empty() {
                            continue;
                        }
                        // An earlier error `k` tokens before the junk; depth 3
                        // only without one, to keep the grid affordable.
                        let priors: &[Option<usize>] = if depth == 3 {
                            &[None]
                        } else {
                            &[None, Some(0), Some(1), Some(2), Some(3)]
                        };
                        for &prior in priors {
                            for deeper in [false, true] {
                                out.push(cell(
                                    host,
                                    depth,
                                    (prefix_label, prefix),
                                    junk,
                                    same_line,
                                    prior,
                                    deeper,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

fn cell(
    host: Host,
    depth: usize,
    (prefix_label, prefix): (&str, &str),
    junk: &str,
    same_line: bool,
    prior: Option<usize>,
    deeper: bool,
) -> Cell {
    let outer: Vec<&'static str> = ["O", "P"][..depth - 1].to_vec();
    let a_indent = "    ".repeat(depth - 1);
    let body = "    ".repeat(depth);
    let mut src = String::new();
    if host != Host::Anonymous {
        src.push_str("module M\n");
    }
    for (i, name) in outer.iter().enumerate() {
        src.push_str(&format!("{}module {name} =\n", "    ".repeat(i)));
    }
    let open = if host == Host::Begin { " begin" } else { "" };
    src.push_str(&format!("{a_indent}module A ={open}\n"));
    let junk_line = match prior {
        None => junk.to_string(),
        Some(k) => {
            let atoms: Vec<String> = (1..=k).map(|i| format!("q{i}")).collect();
            format!("x ) {} {junk}", atoms.join(" "))
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }
    };
    if prefix.is_empty() {
        src.push_str(&format!("{body}{junk_line}\n"));
    } else if same_line {
        src.push_str(&format!("{body}{prefix} {junk_line}\n"));
    } else {
        src.push_str(&format!("{body}{prefix}\n{body}{junk_line}\n"));
    }
    if deeper {
        src.push_str(&format!("{body}    w\n"));
    }
    src.push_str(&format!("{body}let x = 1\n"));
    if host == Host::Begin {
        src.push_str(&format!("{a_indent}end\n"));
    }
    if depth >= 2 {
        src.push_str(&format!("{}let y = 2\n", "    ".repeat(depth - 2)));
    }
    src.push_str("let z = 3\n");
    let mut modules = vec![if host == Host::Anonymous { "file" } else { "M" }];
    modules.extend(outer);
    modules.push("A");
    let key = format!(
        "{} d{depth} | {prefix_label}{}{}{} | {junk}",
        host.label(),
        if same_line { " same-line" } else { "" },
        prior.map_or(String::new(), |k| format!(" prior{k}")),
        if deeper { " +deeper" } else { "" },
    );
    Cell {
        key,
        source: src,
        modules,
    }
}

/// Every cell: each named declaration FCS keeps must be ours too, of the same
/// kind and in the same module.
#[test]
fn every_declaration_lands_where_fcs_puts_it() {
    use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV};
    let mut divergences = Vec::new();
    // Each lost declaration, and each divergence in a cell FCS accepts, keyed
    // without our side's placement.
    let mut keys = Vec::new();
    let all = cells();
    assert!(all.len() > 1000, "the grid shrank to {} cells", all.len());
    for cell in &all {
        let src = &cell.source;
        let (fcs, fcs_clean) = {
            let mut tmp = tempfile::NamedTempFile::with_suffix(".fs").expect("create tempfile");
            std::io::Write::write_all(&mut tmp, src.as_bytes()).expect("write source");
            let json = crate::common::fcs_ast_batch(tmp.path());
            (
                fcs_units(&json, src),
                !crate::common::fcs_parse_had_errors(&json),
            )
        };
        let ours = our_units(&borzoi_cst::parser::parse(src));
        for &name in NAMES {
            let Some(at) = src.find(name) else { continue };
            // The declaration's kind and the module it lands in.
            let place_of = |units: &[Unit]| {
                units
                    .iter()
                    .find(|u| u.key.start == at)
                    .map_or("nowhere".to_string(), |u| {
                        let module = cell
                            .modules
                            .get(u.key.scope.len() - 1)
                            .copied()
                            .unwrap_or("deeper");
                        format!("{} {module}", u.key.kind)
                    })
            };
            let (want, got) = (place_of(&fcs), place_of(&ours));
            if want != "nowhere" && got != want {
                let key = format!("{} | `{name}`: FCS in {want}", cell.key);
                if fcs_clean || got.split(' ').next() != want.split(' ').next() {
                    keys.push(key.clone());
                }
                divergences.push(format!("{key}, ours in {got}"));
            }
        }
    }
    let actual = Manifest::from_entries(divergences).expect("one line per cell and name");
    crate::common::corpus_manifest::check_manifest(
        "module_recover_grid",
        &actual,
        &format!(
            "{UPDATE_ENV}=1 nix develop -c cargo test -p borzoi-cst --test all parser_diff_module_recover_grid::"
        ),
    );
    crate::common::corpus_manifest::check_no_worse_than_base("module_recover_grid", &keys, &[]);
}

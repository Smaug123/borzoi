//! **A module's short name**: FCS enters the nested modules of every module it
//! opens or folds under their short names (`AddModuleOrNamespaceRefsToNameEnv`),
//! a nested module enters itself where it closes (`AddLocalSubModule`), and an
//! `open` of a short name opens every module the name holds, the latest entry
//! folding last. Each case is graded against FCS; `must_commit` says whether
//! the walk must name FCS's declaration or may decline.
//!
//! The ranking cases are the shape a tiered walk gets wrong quietly: a module
//! the enclosing container declares *after* an `open` outranks the opened
//! container's module of its name, and one declared before loses to it.

use std::path::{Path, PathBuf};

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::{AssemblyEnv, resolve_project};
use rowan::TextRange;

use crate::common::{invoke_fcs_dump_project, parse_fcs_uses_project, temp_fs_file};

/// One graded shape. The probe is the last occurrence of `probe` in the last
/// file, a name or a dotted path whose last segment is the use; FCS must bind
/// it to the last occurrence of `want` in file `want_file`.
struct Case {
    label: &'static str,
    files: &'static [&'static str],
    probe: &'static str,
    want_file: usize,
    want: &'static str,
    must_commit: bool,
}

/// FCS's binding of a probe and ours, as `(file, declaration start)`.
struct Graded {
    /// `Err` when FCS reports a diagnostic in any file, or binds the probe to
    /// no project declaration.
    fcs: Result<(usize, usize), String>,
    ours: Option<(usize, usize)>,
    /// Our raw resolution, for messages.
    raw: String,
}

/// Grade the last occurrence of `probe` in the last of `files`, a name or a
/// dotted path whose last segment is the use. FCS reports a dotted use over
/// the whole access, so its record is the one ending where the probe ends.
fn grade(label: &str, files: &[&str], probe: &str) -> Graded {
    let paths: Vec<PathBuf> = files
        .iter()
        .enumerate()
        .map(|(i, src)| temp_fs_file(&format!("{label}_{i}"), src))
        .collect();
    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let sources: Vec<(PathBuf, String)> = paths
        .iter()
        .cloned()
        .zip(files.iter().map(|s| s.to_string()))
        .collect();
    let fcs_files = parse_fcs_uses_project(&invoke_fcs_dump_project(&refs), &sources);
    let last = files.len() - 1;
    let at = files[last].rfind(probe).expect("the probe");
    let end = at + probe.len();
    let errors: Vec<_> = fcs_files.iter().flat_map(|f| f.errors.iter()).collect();
    let fcs = if errors.is_empty() {
        fcs_files[last]
            .uses
            .iter()
            .find(|u| u.end == end && u.start >= at)
            .and_then(|u| u.decl.as_ref())
            .and_then(|decl| {
                let file = paths
                    .iter()
                    .position(|p| p.file_name() == decl.file.file_name())?;
                Some((file, decl.start))
            })
            .ok_or_else(|| "FCS binds no project declaration at the probe".to_string())
    } else {
        Err(format!("FCS errors: {errors:?}"))
    };

    let impls: Vec<ImplFile> = files
        .iter()
        .map(|s| {
            let parsed = parse(s);
            assert!(parsed.errors.is_empty(), "{label}: parse errors");
            ImplFile::cast(parsed.root).expect("impl file")
        })
        .collect();
    let proj = resolve_project(&impls, &AssemblyEnv::default());
    let segment = at + probe.rfind('.').map_or(0, |i| i + 1);
    let span = TextRange::new(
        u32::try_from(segment).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    );
    let res = proj.file(last).resolution_at(span);
    let ours = res.and_then(|res| {
        proj.item_def(res)
            .or_else(|| proj.file(last).resolved_def(res).map(|d| (last, d)))
            .map(|(file, def)| (file, usize::from(def.range.start())))
    });
    Graded {
        fcs,
        ours,
        raw: format!("{res:?}"),
    }
}

impl Case {
    /// FCS's answer is pinned, and ours must be FCS's or nothing (only
    /// FCS's, with `must_commit`).
    fn grade(&self) {
        let label = self.label;
        let graded = grade(label, self.files, self.probe);
        let fcs = graded.fcs.unwrap_or_else(|e| panic!("{label}: {e}"));
        let want_at = self.files[self.want_file]
            .rfind(self.want)
            .expect("the wanted decl");
        assert_eq!(
            fcs,
            (self.want_file, want_at),
            "{label}: FCS binds something other than the pinned declaration"
        );
        match graded.ours {
            Some(ours) => assert_eq!(
                ours, fcs,
                "{label}: we bind a different declaration from FCS ({})",
                graded.raw
            ),
            None => assert!(
                !self.must_commit,
                "{label}: we decline where FCS binds {:?} ({})",
                self.want, graded.raw
            ),
        }
    }
}

/// Run every case, so a regression names each one rather than the first.
fn run(cases: &[Case]) {
    let failed: Vec<&str> = cases
        .iter()
        .filter(|c| std::panic::catch_unwind(|| c.grade()).is_err())
        .map(|c| c.label)
        .collect();
    assert!(failed.is_empty(), "graded wrong: {failed:?}");
}

#[test]
fn a_local_module_ranks_by_where_it_closes() {
    run(&[
        Case {
            label: "short_open_then_local_child",
            files: &[
                "module Demo\n\nmodule H =\n    module Child =\n        let Nn = 1\n\nopen H\n\n\
                      module Child =\n    let Nn = 2\n\nopen Child\n\nlet probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 2",
            must_commit: true,
        },
        Case {
            label: "short_local_child_then_open",
            files: &[
                "module Demo\n\nmodule Child =\n    let Nn = 2\n\nmodule H =\n    module Child =\n        \
                      let Nn = 1\n\nopen H\n\nopen Child\n\nlet probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: true,
        },
        Case {
            label: "short_fold_then_local_child",
            files: &[
                "module Demo\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        let Nn = 1\n\n\
                      module Child =\n    let Nn = 2\n\nopen Child\n\nlet probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 2",
            must_commit: true,
        },
        Case {
            label: "short_local_child_then_fold",
            files: &[
                "module Demo\n\nmodule Child =\n    let Nn = 2\n\n[<AutoOpen>]\nmodule A =\n    \
                      module Child =\n        let Nn = 1\n\nopen Child\n\nlet probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: true,
        },
        // Inside `module Child`, the module has not closed, so its name is
        // not yet entered: `open Child` is the opened `H.Child` alone.
        Case {
            label: "short_open_inside_the_local_child",
            files: &[
                "module Demo\n\nmodule H =\n    module Child =\n        let Nn = 1\n\nopen H\n\n\
                      module Child =\n    let Nn = 2\n    open Child\n    let probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: false,
        },
    ]);
}

#[test]
fn every_fold_enters_its_fragments_children() {
    run(&[
        Case {
            label: "short_chain",
            files: &[
                "module Demo\n\nlet Nn = 99\n\n[<AutoOpen>]\nmodule A =\n    [<AutoOpen>]\n    \
                      module B =\n        module Child =\n            let Nn = 2\n\nopen Child\n\n\
                      let probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 2",
            must_commit: true,
        },
        Case {
            label: "short_implicit_namespace",
            files: &[
                "namespace N\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        let Nn = 2\n",
                "namespace N\n\nmodule Demo =\n    let Nn = 99\n    open Child\n    let probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 2",
            must_commit: true,
        },
        // A fold inside a module enters its children into that module only.
        Case {
            label: "short_fold_in_a_nested_scope",
            files: &[
                "module Demo\n\nmodule Child =\n    let Nn = 99\n\nmodule Outer =\n    [<AutoOpen>]\n    \
                      module A =\n        module Child =\n            let Nn = 2\n\nopen Child\n\n\
                      let probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 99",
            must_commit: false,
        },
    ]);
}

/// A container holding both a module `Child` and an `[<AutoOpen>]` module
/// holding its own: the auto-open child entered after the container's own
/// wins, through every channel.
#[test]
fn an_auto_open_childs_module_outranks_the_containers_own() {
    run(&[
        Case {
            label: "short_contest_same_file",
            files: &[
                "module Demo\n\nmodule H =\n    module Child =\n        let Nn = 2\n    [<AutoOpen>]\n    \
                      module A =\n        module Child =\n            let Nn = 1\n\nopen H\nopen Child\n\n\
                      let probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: true,
        },
        Case {
            label: "short_contest_cross_file",
            files: &[
                "module H\n\nmodule Child =\n    let Nn = 2\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        \
                 let Nn = 1\n",
                "module Demo\n\nopen H\nopen Child\n\nlet probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: true,
        },
        Case {
            label: "short_contest_namespace",
            files: &[
                "namespace N\n\nmodule Child =\n    let Nn = 2\n\n[<AutoOpen>]\nmodule A =\n    \
                 module Child =\n        let Nn = 1\n",
                "module Demo\n\nopen N\nopen Child\n\nlet probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: true,
        },
        Case {
            label: "short_contest_implicit_namespace",
            files: &[
                "namespace N\n\nmodule Child =\n    let Nn = 2\n\n[<AutoOpen>]\nmodule A =\n    \
                 module Child =\n        let Nn = 1\n",
                "namespace N\n\nmodule Demo =\n    open Child\n    let probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: true,
        },
    ]);
}

/// Folding `[<AutoOpen>] module N.A` enters the modules its own declaration
/// nests, not every module path under `N.A`: a second `module A` in another
/// file is another fragment, and its `Pick` stays out of reach. The walk must
/// not open it; it declines, as the guard for short names it cannot follow
/// (`may_name_unlent_module`) reads parents by path.
#[test]
fn another_fragments_module_is_not_entered() {
    run(&[Case {
        label: "short_other_fragment",
        files: &[
            "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let x = 1\n",
            "namespace N\n\nmodule A =\n    module Pick =\n        let Nn = 2\n",
            "module Demo\n\nmodule Pick =\n    let Nn = 99\n\nopen N\nopen Pick\n\nlet probe = Nn\n",
        ],
        probe: "Nn",
        want_file: 2,
        want: "Nn = 99",
        must_commit: false,
    }]);
}

/// A dotted head reaches a short name too. The walk does not follow it there
/// yet; it must not name anything FCS does not.
#[test]
fn a_dotted_head_through_a_short_name_is_never_wrong() {
    run(&[
        Case {
            label: "short_dotted_fold",
            files: &[
                "module Demo\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        let Nn = 1\n\n\
                      let probe = Child.Nn\n",
            ],
            probe: "Child.Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: false,
        },
        Case {
            label: "short_dotted_fold_then_local",
            files: &[
                "module Demo\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        let Nn = 1\n\n\
                      module Child =\n    let Nn = 2\n\nlet probe = Child.Nn\n",
            ],
            probe: "Child.Nn",
            want_file: 0,
            want: "Nn = 2",
            must_commit: false,
        },
        Case {
            label: "short_dotted_local_then_fold",
            files: &[
                "module Demo\n\nmodule Child =\n    let Nn = 2\n\n[<AutoOpen>]\nmodule A =\n    \
                      module Child =\n        let Nn = 1\n\nlet probe = Child.Nn\n",
            ],
            probe: "Child.Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: false,
        },
        Case {
            label: "short_dotted_open_then_local",
            files: &[
                "module Demo\n\nmodule H =\n    module Child =\n        let Nn = 1\n\nopen H\n\n\
                      module Child =\n    let Nn = 2\n\nlet probe = Child.Nn\n",
            ],
            probe: "Child.Nn",
            want_file: 0,
            want: "Nn = 2",
            must_commit: false,
        },
        Case {
            label: "short_dotted_namespace",
            files: &[
                "namespace N\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        let Nn = 2\n",
                "module Demo\n\nopen N\n\nlet probe = Child.Nn\n",
            ],
            probe: "Child.Nn",
            want_file: 0,
            want: "Nn = 2",
            must_commit: false,
        },
    ]);
}

/// The `[<AutoOpen>] module A` whose `Child` a short name may reach, in
/// namespace `N` or in the global namespace.
const FRAGMENTS: &[&str] = &[
    "namespace N\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        let Nn = 1\n",
    "namespace global\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        let Nn = 1\n",
];

/// Another module `Child` that contests the short name, and how it may be
/// laid out.
#[derive(Clone, Copy, Debug)]
enum Competitor {
    /// `N.Child`, a direct child of the namespace.
    Direct,
    /// A second, plain `module N.A` holding its own `Child`: another
    /// fragment of the path, which folding the auto-open one does not reach.
    PlainFragment,
    /// A second `[<AutoOpen>]` module of the namespace holding a `Child`.
    SecondFragment,
    /// A `Child` in the global namespace, reached as written.
    Global,
    /// A top-level `module Child` file.
    RootFile,
    /// A `module N.A.Child` header file.
    HeaderFile,
}

impl Competitor {
    const ALL: &'static [Competitor] = &[
        Competitor::Direct,
        Competitor::PlainFragment,
        Competitor::SecondFragment,
        Competitor::Global,
        Competitor::RootFile,
        Competitor::HeaderFile,
    ];

    fn block(self) -> &'static str {
        match self {
            Competitor::Direct => "namespace N\n\nmodule Child =\n    let Nn = 2\n",
            Competitor::PlainFragment => {
                "namespace N\n\nmodule A =\n    module Child =\n        let Nn = 2\n"
            }
            Competitor::SecondFragment => {
                "namespace N\n\n[<AutoOpen>]\nmodule B =\n    module Child =\n        let Nn = 2\n"
            }
            Competitor::Global => "namespace global\n\nmodule Child =\n    let Nn = 2\n",
            Competitor::RootFile => "module Child\n\nlet Nn = 2\n",
            Competitor::HeaderFile => "module N.A.Child\n\nlet Nn = 2\n",
        }
    }

    /// A top-level module must be a file's only block.
    fn owns_its_file(self) -> bool {
        matches!(self, Competitor::RootFile | Competitor::HeaderFile)
    }
}

/// Where the `open Child` is, and what the probing block declares itself.
#[derive(Clone, Copy, Debug)]
enum Opener {
    /// A later `module Demo` file opening `Child` alone.
    Bare,
    /// A later `module Demo` file: `open N` then `open Child`.
    Explicit,
    /// As [`Self::Explicit`], with a local `Child` declared between the
    /// two `open`s.
    ExplicitLocalAfter,
    /// As [`Self::Explicit`], with a local `Child` declared first.
    ExplicitLocalBefore,
    /// A later `namespace N` file, folding the namespace implicitly.
    ImplicitFile,
    /// A further `namespace N` block in the file of the last declaration.
    ImplicitBlock,
}

impl Opener {
    const ALL: &'static [Opener] = &[
        Opener::Bare,
        Opener::Explicit,
        Opener::ExplicitLocalAfter,
        Opener::ExplicitLocalBefore,
        Opener::ImplicitFile,
        Opener::ImplicitBlock,
    ];

    fn block(self) -> &'static str {
        match self {
            Opener::Bare => "module Demo\n\nlet Nn = 99\n\nopen Child\n\nlet probe = Nn\n",
            Opener::Explicit => {
                "module Demo\n\nlet Nn = 99\n\nopen N\nopen Child\n\nlet probe = Nn\n"
            }
            Opener::ExplicitLocalAfter => {
                "module Demo\n\nlet Nn = 99\n\nopen N\n\nmodule Child =\n    let Nn = 3\n\n\
                 open Child\n\nlet probe = Nn\n"
            }
            Opener::ExplicitLocalBefore => {
                "module Demo\n\nlet Nn = 99\n\nmodule Child =\n    let Nn = 3\n\nopen N\n\
                 open Child\n\nlet probe = Nn\n"
            }
            Opener::ImplicitFile | Opener::ImplicitBlock => {
                "namespace N\n\nmodule Demo =\n    let Nn = 99\n    open Child\n    let probe = Nn\n"
            }
        }
    }
}

/// One grid cell's files, or `None` where the layout cannot be written.
fn layout(
    fragment: &str,
    competitor: Competitor,
    fragment_first: bool,
    share_file: bool,
    opener: Opener,
) -> Option<Vec<String>> {
    if share_file && competitor.owns_its_file() {
        return None;
    }
    let (first, second) = if fragment_first {
        (fragment, competitor.block())
    } else {
        (competitor.block(), fragment)
    };
    let mut files: Vec<String> = if share_file {
        vec![format!("{first}\n{second}")]
    } else {
        vec![first.to_string(), second.to_string()]
    };
    match opener {
        Opener::ImplicitBlock => {
            let last = files.last_mut().expect("a file");
            if last.starts_with("module ") {
                return None;
            }
            last.push('\n');
            last.push_str(opener.block());
        }
        _ => files.push(opener.block().to_string()),
    }
    Some(files)
}

/// Every placement of a contesting `Child` against the folded `A.Child`, in
/// every order, sharing a file or not, under every way of opening the short
/// name: the walk names what FCS names, or nothing. FCS errors (a layout F#
/// rejects) skip the cell. Every cell's verdict is pinned in the exact
/// manifest `tests/manifests/auto_open_short_name_grid.txt`, so a lost commit
/// fails as loudly as a wrong answer.
///
/// The global-namespace fragment declines throughout: the walk has no channel
/// for an earlier file's global `[<AutoOpen>]` module, so an `open` of a
/// short name it may lend goes opaque.
#[test]
fn every_contest_for_a_short_name_agrees_with_fcs() {
    let mut wrong: Vec<String> = Vec::new();
    let mut verdicts: Vec<String> = Vec::new();
    let (mut commits, mut declines, mut skipped) = (0usize, 0usize, 0usize);
    let mut cell = 0usize;
    for (fragment_tag, fragment) in ["in-N", "global"].into_iter().zip(FRAGMENTS) {
        for &competitor in Competitor::ALL {
            for fragment_first in [true, false] {
                for share_file in [false, true] {
                    for &opener in Opener::ALL {
                        let Some(files) =
                            layout(fragment, competitor, fragment_first, share_file, opener)
                        else {
                            continue;
                        };
                        cell += 1;
                        let tag = format!(
                            "{fragment_tag} {competitor:?} fragment_first={fragment_first} \
                         share={share_file} {opener:?}"
                        );
                        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
                        let graded = grade(&format!("short_grid_{cell}"), &refs, "Nn");
                        match (graded.fcs, graded.ours) {
                            (Err(_), _) => {
                                skipped += 1;
                                verdicts.push(format!("{tag}: fcs-error"));
                            }
                            (Ok(_), None) => {
                                declines += 1;
                                verdicts.push(format!("{tag}: decline"));
                            }
                            (Ok(fcs), Some(ours)) if fcs == ours => {
                                commits += 1;
                                verdicts.push(format!("{tag}: commit"));
                            }
                            (Ok(fcs), Some(ours)) => wrong.push(format!(
                                "{tag}: FCS {fcs:?}, ours {ours:?} ({})",
                                graded.raw
                            )),
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "short-name grid: {cell} cells, {commits} commits, {declines} declines, \
         {skipped} skipped (FCS errors)"
    );
    assert!(wrong.is_empty(), "wrong answers:\n{}", wrong.join("\n"));
    let manifest =
        Manifest::from_entries(verdicts).unwrap_or_else(|e| panic!("manifest entry: {e}"));
    borzoi_oracle_harness::manifest::check(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/manifests/auto_open_short_name_grid.txt"),
        &manifest,
        "BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-sema --test all \
         auto_open_short_name::every_contest_for_a_short_name_agrees_with_fcs",
    );
}

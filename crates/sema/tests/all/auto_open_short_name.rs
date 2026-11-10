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

use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_oracle_harness::manifest::Manifest;
use borzoi_sema::{AssemblyEnv, Resolution, resolve_project};
use rowan::TextRange;

use crate::common::{
    ensure_autoopen_fixture_built, invoke_fcs_dump_project_with_refs, parse_fcs_uses_project,
    temp_fs_file,
};

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

/// A [`Case`] against the auto-open fixture assembly, referenced on both
/// sides.
fn grade_against_fixture(case: &Case) {
    let dll = ensure_autoopen_fixture_built();
    let bytes = std::fs::read(dll).expect("read autoopen fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse autoopen fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");
    case.check(grade_with(case.label, case.files, case.probe, &[dll], &env));
}

/// FCS's binding of a probe and ours, as `(file, declaration start)`.
struct Graded {
    /// `Err` when FCS reports a diagnostic in any file, or binds the probe to
    /// no project declaration.
    fcs: Result<(usize, usize), String>,
    /// `Ok(None)` for a decline; `Err` for an answer that names no project
    /// declaration (a referenced assembly's member), which disagrees with
    /// every project binding FCS can make.
    ours: Result<Option<(usize, usize)>, String>,
    /// Our raw resolution, for messages.
    raw: String,
}

/// Grade the last occurrence of `probe` in the last of `files`, a name or a
/// dotted path whose last segment is the use. FCS reports a dotted use over
/// the whole access, so its record is the one ending where the probe ends.
fn grade(label: &str, files: &[&str], probe: &str) -> Graded {
    grade_with(label, files, probe, &[], &AssemblyEnv::default())
}

/// [`grade`] with `refs` referenced on FCS's side and `env` on ours.
fn grade_with(
    label: &str,
    files: &[&str],
    probe: &str,
    refs: &[&Path],
    env: &AssemblyEnv,
) -> Graded {
    let paths: Vec<PathBuf> = files
        .iter()
        .enumerate()
        .map(|(i, src)| temp_fs_file(&format!("{label}_{i}"), src))
        .collect();
    let file_refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let sources: Vec<(PathBuf, String)> = paths
        .iter()
        .cloned()
        .zip(files.iter().map(|s| s.to_string()))
        .collect();
    let fcs_files = parse_fcs_uses_project(
        &invoke_fcs_dump_project_with_refs(&file_refs, refs),
        &sources,
    );
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
    let proj = resolve_project(&impls, env);
    // A dotted use's resolution is recorded at its last segment or over the
    // whole path, by channel; a commitment at either is an answer.
    let segment = at + probe.rfind('.').map_or(0, |i| i + 1);
    let span = |start: usize| {
        TextRange::new(
            u32::try_from(start).unwrap().into(),
            u32::try_from(end).unwrap().into(),
        )
    };
    let mut spans = vec![span(segment)];
    if segment != at {
        spans.push(span(at));
    }
    let committed: Vec<Resolution> = spans
        .iter()
        .filter_map(|&s| proj.file(last).resolution_at(s))
        .filter(|res| !matches!(res, Resolution::Deferred(_)))
        .collect();
    let raw: Vec<String> = spans
        .iter()
        .map(|&s| format!("{s:?}: {:?}", proj.file(last).resolution_at(s)))
        .collect();
    let target = |res: &Resolution| {
        proj.item_def(*res)
            .or_else(|| proj.file(last).resolved_def(*res).map(|d| (last, d)))
            .map(|(file, def)| (file, usize::from(def.range.start())))
            .ok_or_else(|| format!("{res:?}"))
    };
    let ours = match committed.as_slice() {
        [] => Ok(None),
        [res] => target(res).map(Some),
        [a, b] => match (target(a), target(b)) {
            (Ok(x), Ok(y)) if x == y => Ok(Some(x)),
            _ => Err(format!("two answers: {a:?} and {b:?}")),
        },
        _ => unreachable!("at most two spans"),
    };
    Graded {
        fcs,
        ours,
        raw: raw.join("; "),
    }
}

impl Case {
    /// FCS's answer is pinned, and ours must be FCS's or nothing (only
    /// FCS's, with `must_commit`).
    fn grade(&self) {
        self.check(grade(self.label, self.files, self.probe));
    }

    fn check(&self, graded: Graded) {
        let label = self.label;
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
            Err(other) => panic!("{label}: we bind {other} where FCS binds {:?}", self.want),
            Ok(Some(ours)) => assert_eq!(
                ours, fcs,
                "{label}: we bind a different declaration from FCS ({})",
                graded.raw
            ),
            Ok(None) => assert!(
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

/// Nor is the child entered when another declaration shares a path beneath
/// it: the module `open` of `Child.Grand`, or of `Grand` after `open Child`,
/// folds every declaration at its path, FCS only the fragment's own.
#[test]
fn another_fragments_descendant_is_not_entered() {
    const FRAGMENT: &str = "namespace N\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        module Grand =\n            \
         let Nn = 1\n";
    const HEADER: &str = "module N.A.Child.Grand\n\nlet Nn = 2\n";
    run(&[
        Case {
            label: "short_descendant_dotted_open",
            files: &[
                FRAGMENT,
                HEADER,
                "module Demo\n\nopen N\nopen Child.Grand\n\nlet probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: false,
        },
        Case {
            label: "short_descendant_chained_open",
            files: &[
                FRAGMENT,
                HEADER,
                "module Demo\n\nopen N\nopen Child\nopen Grand\n\nlet probe = Nn\n",
            ],
            probe: "Nn",
            want_file: 0,
            want: "Nn = 1",
            must_commit: false,
        },
    ]);
}

/// Nor when a referenced assembly declares something at or beneath the
/// child's path: the fixture's `Demo.Auto.Extra.ExtraShorten` sits beneath the
/// project's `[<AutoOpen>] module Demo.Auto`'s `Extra`, so a chained `open
/// ExtraShorten` after `open Extra` would open the assembly's too, where FCS
/// opens only the project's, which holds no `extraShortenTarget`.
#[test]
fn an_assemblys_entity_beneath_the_child_keeps_it_out() {
    grade_against_fixture(&Case {
        label: "short_assembly_beneath",
        files: &[
            "namespace Demo\n\n[<AutoOpen>]\nmodule Auto =\n    module Extra =\n        \
             module ExtraShorten =\n            let other = 1\n",
            "module Client\n\nlet extraShortenTarget () = 99\n\nopen Demo\nopen Extra\n\
             open ExtraShorten\n\nlet probe = extraShortenTarget ()\n",
        ],
        probe: "extraShortenTarget",
        want_file: 1,
        want: "extraShortenTarget () = 99",
        must_commit: false,
    });
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
        // The control: the grader sees a dotted commitment.
        Case {
            label: "dotted_control_local",
            files: &["module Demo\n\nmodule Child =\n    let Nn = 2\n\nlet probe = Child.Nn\n"],
            probe: "Child.Nn",
            want_file: 0,
            want: "Nn = 2",
            must_commit: true,
        },
        Case {
            label: "short_dotted_fold_lacks_member",
            files: &[
                "module Child\n\nlet Nn = 99\n",
                "module Demo\n\nmodule Child =\n    let Nn = 2\n\n[<AutoOpen>]\nmodule A =\n    \
                      module Child =\n        let other = 1\n\nlet probe = Child.Nn\n",
            ],
            probe: "Child.Nn",
            want_file: 1,
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
/// namespace `N` or in the global namespace. `Child` nests a `Grand`, which
/// the dotted and chained openers reach through it.
const FRAGMENTS: &[&str] = &[
    "namespace N\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        let Nn = 1\n        \
     module Grand =\n            let Nn = 4\n",
    "namespace global\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        let Nn = 1\n        \
     module Grand =\n            let Nn = 4\n",
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
    /// A `module N.A.Child.Grand` header file: another declaration beneath
    /// the fragment's child.
    DescendantHeaderFile,
    /// A `namespace N.A.Child` declaring an exception `Nn`.
    NamespaceAtTheChild,
}

impl Competitor {
    const ALL: &'static [Competitor] = &[
        Competitor::Direct,
        Competitor::PlainFragment,
        Competitor::SecondFragment,
        Competitor::Global,
        Competitor::RootFile,
        Competitor::HeaderFile,
        Competitor::DescendantHeaderFile,
        Competitor::NamespaceAtTheChild,
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
            Competitor::DescendantHeaderFile => "module N.A.Child.Grand\n\nlet Nn = 2\n",
            Competitor::NamespaceAtTheChild => "namespace N.A.Child\n\nexception Nn of int\n",
        }
    }

    /// A top-level module must be a file's only block.
    fn owns_its_file(self) -> bool {
        matches!(
            self,
            Competitor::RootFile | Competitor::HeaderFile | Competitor::DescendantHeaderFile
        )
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
    /// A later `module Demo` file: `open N` then `open Child.Grand`.
    Dotted,
    /// A later `module Demo` file: `open N`, `open Child`, then `open Grand`.
    Chained,
}

impl Opener {
    const ALL: &'static [Opener] = &[
        Opener::Bare,
        Opener::Explicit,
        Opener::ExplicitLocalAfter,
        Opener::ExplicitLocalBefore,
        Opener::ImplicitFile,
        Opener::ImplicitBlock,
        Opener::Dotted,
        Opener::Chained,
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
            Opener::Dotted => {
                "module Demo\n\nlet Nn = 99\n\nopen N\nopen Child.Grand\n\nlet probe = Nn\n"
            }
            Opener::Chained => {
                "module Demo\n\nlet Nn = 99\n\nopen N\nopen Child\nopen Grand\n\nlet probe = Nn\n"
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
                            (Ok(_), Ok(None)) => {
                                declines += 1;
                                verdicts.push(format!("{tag}: decline"));
                            }
                            (Ok(fcs), Ok(Some(ours))) if fcs == ours => {
                                commits += 1;
                                verdicts.push(format!("{tag}: commit"));
                            }
                            (Ok(fcs), ours) => wrong.push(format!(
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

/// A dotted value path through a short name that holds more than one module:
/// FCS searches every module the name holds, highest priority first, and
/// binds the first that has the value, so a higher-ranked module that lacks
/// it leaves the path to the next. Generated over what ranks the higher
/// module, what it declares of the name, where the lower module lives, and
/// whether the path goes through a module alias, and graded against FCS:
/// never a target FCS does not bind, and, where the higher module declares
/// nothing of the name and the path is written directly, FCS's target. (An
/// `open` of a project module leaves every dotted head it could supply to
/// the conservative `dotted_opacity` gate, so the `open` cells may decline.
/// The `type` cells rank an opened namespace's type `Child` highest, which
/// no module search sees.)
#[test]
fn a_dotted_value_searches_every_module_of_its_short_name() {
    // What puts the higher-ranked `Child` above the lower ones.
    // `type`: an opened namespace's type `Child`, which no module search
    // sees, so its body is irrelevant.
    let shadow_text = |shadow: &str, body: &str| match shadow {
        "fold" => format!("[<AutoOpen>]\nmodule A =\n    module Child =\n        {body}\n\n"),
        "open" => format!("module H =\n    module Child =\n        {body}\n\nopen H\n\n"),
        _ => "open LibNs\n\n".to_string(),
    };
    let type_file = "namespace LibNs\n\ntype Child() =\n    static member Other = 0\n";
    // What the higher-ranked `Child` declares; `lacks` is whether that is
    // nothing named `Nn`.
    let bodies = [
        ("other", "let other = 1", true),
        ("value", "let Nn = 1", false),
        ("private", "let private Nn = 1", false),
        ("type", "type Nn() = class end", false),
        ("module", "module Nn =\n            let x = 1", false),
    ];
    // Where the lower-ranked `Child`s are: this file's, before the shadow,
    // and an earlier file's root module.
    let lowers = [
        ("local", true, false),
        ("root", false, true),
        ("both", true, true),
    ];
    // How the path is written.
    let accesses = [
        ("direct", "let probe = Child.Nn\n", "Child.Nn"),
        ("alias", "module M = Child\n\nlet probe = M.Nn\n", "M.Nn"),
    ];
    let mut failures = Vec::new();
    let mut census: std::collections::BTreeMap<&str, usize> = Default::default();
    for shadow in ["fold", "open", "type"] {
        for (body_tag, body, lacks) in bodies {
            if shadow == "type" && body_tag != "other" {
                continue;
            }
            for (lower, local, root) in lowers {
                for (access, use_text, probe) in accesses {
                    let label = format!("dotted_search_{shadow}_{body_tag}_{lower}_{access}");
                    let mut last = String::from("module Demo\n\n");
                    if local {
                        last.push_str("module Child =\n    let Nn = 2\n\n");
                    }
                    last.push_str(&shadow_text(shadow, body));
                    last.push_str(use_text);
                    let root_file = "module Child\n\nlet Nn = 99\n";
                    let mut files: Vec<&str> = Vec::new();
                    if root {
                        files.push(root_file);
                    }
                    if shadow == "type" {
                        files.push(type_file);
                    }
                    files.push(&last);
                    let graded = grade(&label, &files, probe);
                    let verdict = match (&graded.fcs, &graded.ours) {
                        (_, Ok(None)) => "declined",
                        (Ok(fcs), Ok(Some(ours))) if fcs == ours => "agreed",
                        _ => "WRONG",
                    };
                    *census.entry(verdict).or_default() += 1;
                    if verdict == "WRONG" {
                        failures.push(format!(
                            "{label}: FCS {:?} | ours {:?} ({})",
                            graded.fcs, graded.ours, graded.raw
                        ));
                    } else if verdict == "declined"
                        && lacks
                        && shadow != "open"
                        && access == "direct"
                        && graded.fcs.is_ok()
                    {
                        failures.push(format!(
                            "{label}: we decline where FCS binds {:?} ({})",
                            graded.fcs, graded.raw
                        ));
                    }
                }
            }
        }
    }
    eprintln!("dotted short-name search census: {census:?}");
    assert!(
        failures.is_empty(),
        "dotted short-name search:\n{}",
        failures.join("\n")
    );
}

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

impl Case {
    /// FCS's answer is pinned, and ours must be FCS's or nothing (only
    /// FCS's, with `must_commit`). FCS reports a dotted use over the whole
    /// access, so its record is the one ending where the probe ends.
    fn grade(&self) {
        let label = self.label;
        let paths: Vec<PathBuf> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, src)| temp_fs_file(&format!("{label}_{i}"), src))
            .collect();
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        let sources: Vec<(PathBuf, String)> = paths
            .iter()
            .cloned()
            .zip(self.files.iter().map(|s| s.to_string()))
            .collect();
        let fcs = parse_fcs_uses_project(&invoke_fcs_dump_project(&refs), &sources);
        for fu in &fcs {
            assert!(fu.errors.is_empty(), "{label}: FCS errors: {:?}", fu.errors);
        }
        let last = self.files.len() - 1;
        let at = self.files[last].rfind(self.probe).expect("the probe");
        let end = at + self.probe.len();
        let decl = fcs[last]
            .uses
            .iter()
            .find(|u| u.end == end && u.start >= at)
            .and_then(|u| u.decl.as_ref())
            .unwrap_or_else(|| panic!("{label}: FCS binds no in-project decl at the probe"));
        let fcs_file = paths
            .iter()
            .position(|p| p.file_name() == decl.file.file_name())
            .expect("a project file");
        let want_at = self.files[self.want_file]
            .rfind(self.want)
            .expect("the wanted decl");
        assert_eq!(
            (fcs_file, decl.start),
            (self.want_file, want_at),
            "{label}: FCS binds something other than the pinned declaration"
        );

        let impls: Vec<ImplFile> = self
            .files
            .iter()
            .map(|s| {
                let parsed = parse(s);
                assert!(parsed.errors.is_empty(), "{label}: parse errors");
                ImplFile::cast(parsed.root).expect("impl file")
            })
            .collect();
        let proj = resolve_project(&impls, &AssemblyEnv::default());
        let segment = at + self.probe.rfind('.').map_or(0, |i| i + 1);
        let span = TextRange::new(
            u32::try_from(segment).unwrap().into(),
            u32::try_from(end).unwrap().into(),
        );
        let ours = proj.file(last).resolution_at(span);
        let named = ours.and_then(|res| {
            proj.item_def(res)
                .or_else(|| proj.file(last).resolved_def(res).map(|d| (last, d)))
                .map(|(file, def)| (file, usize::from(def.range.start())))
        });
        match named {
            Some(ours_at) => assert_eq!(
                ours_at,
                (fcs_file, decl.start),
                "{label}: we bind a different declaration from FCS ({ours:?})"
            ),
            None => assert!(
                !self.must_commit,
                "{label}: we decline where FCS binds {:?} ({ours:?})",
                self.want
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

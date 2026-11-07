//! **How an `open` orders a path's members**, where the
//! `auto_open_member_sweep` grid cannot reach: across blocks and files, and
//! with three members of one name.
//!
//! Within one block, FCS adds a
//! module's or namespace's members by kind — exceptions, then tycons and
//! their union cases, then values — so a union case outranks an exception of
//! its name in either order. That ranking spans exactly one *fragment*:
//!
//! - in the file being checked, each top-level block declaring the opened
//!   path is its own fragment, added in turn, so a later block's exception
//!   takes the name from an earlier block's case;
//! - an earlier file's blocks are merged into one fragment by the time a
//!   later file opens the path, so its case outranks its exception whichever
//!   block holds which;
//! - earlier files stay separate fragments, added in Compile order.
//!
//! A value outranks a union case in expression position only: an ordinary
//! `let` is not in the pattern table, so in a pattern the case still takes the
//! name from any other union's.
//!
//! Each case here is graded against FCS (`fcs-dump uses-project`). FCS's
//! answer is pinned too, so a fixture that stops exercising the rule fails
//! rather than passing vacuously.

use std::path::{Path, PathBuf};

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, resolve_project};
use rowan::TextRange;

use crate::common::{invoke_fcs_dump_project, parse_fcs_uses_project, temp_fs_file};

/// `files` in Compile order; the probe is the last occurrence of `probe` in
/// the last file, and FCS must bind it to the last occurrence of `want` in
/// file `want_file`. We must name the same declaration, or nothing.
fn assert_open_binds(label: &str, files: &[&str], probe: &str, want_file: usize, want: &str) {
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
    let fcs = parse_fcs_uses_project(&invoke_fcs_dump_project(&refs), &sources);

    let last = files.len() - 1;
    let src = files[last];
    let at = src.rfind(probe).expect("the probe");
    let span = TextRange::at(
        u32::try_from(at).unwrap().into(),
        u32::try_from(probe.len()).unwrap().into(),
    );
    for fu in &fcs {
        assert!(fu.errors.is_empty(), "{label}: FCS errors: {:?}", fu.errors);
    }
    let fcs_use = fcs[last]
        .uses
        .iter()
        .find(|u| u.start == at && u.end == at + probe.len())
        .unwrap_or_else(|| panic!("{label}: FCS reports no use at the probe"));
    let decl = fcs_use.decl.as_ref().expect("FCS binds an in-project decl");
    let fcs_file = paths
        .iter()
        .position(|p| p.file_name() == decl.file.file_name())
        .expect("a project file");
    let want_at = files[want_file].rfind(want).expect("the wanted decl");
    assert_eq!(
        (fcs_file, decl.start),
        (want_file, want_at),
        "{label}: FCS binds something other than the pinned declaration"
    );

    let impls: Vec<ImplFile> = files
        .iter()
        .map(|s| {
            let parsed = parse(s);
            assert!(parsed.errors.is_empty(), "{label}: parse errors");
            ImplFile::cast(parsed.root).expect("impl file")
        })
        .collect();
    let proj = resolve_project(&impls, &AssemblyEnv::default());
    let ours = proj.file(last).resolution_at(span);
    if let Some(res) = ours
        && let Some((file, def)) = proj
            .item_def(res)
            .or_else(|| proj.file(last).resolved_def(res).map(|d| (last, d)))
    {
        assert_eq!(
            (file, usize::from(def.range.start())),
            (fcs_file, decl.start),
            "{label}: we bind a different declaration from FCS ({res:?})"
        );
    }
}

#[test]
fn a_later_blocks_exception_outranks_an_earlier_blocks_case() {
    assert_open_binds(
        "open_member_order_same_file",
        &[
            "namespace Demo\n\ntype Holder =\n    | Target\n\nnamespace Demo\n\n\
           exception Target of int\n\nnamespace Other\n\nopen Demo\n\nmodule M =\n    \
           let x = Target\n",
        ],
        "Target",
        0,
        "Target of int",
    );
}

#[test]
fn an_earlier_files_blocks_merge_before_an_open_ranks_them() {
    assert_open_binds(
        "open_member_order_cross_file",
        &[
            "namespace Demo\n\ntype Holder =\n    | Target\n\nnamespace Demo\n\n\
             exception Target of int\n",
            "module Other\n\nopen Demo\n\nlet x = Target\n",
        ],
        "Target",
        0,
        "Target\n",
    );
}

#[test]
fn earlier_files_keep_their_order_through_an_open() {
    assert_open_binds(
        "open_member_order_two_files",
        &[
            "namespace Demo\n\ntype Holder =\n    | Target\n",
            "namespace Demo\n\nexception Target of int\n",
            "module Other\n\nopen Demo\n\nlet x = Target\n",
        ],
        "Target",
        1,
        "Target of int",
    );
}

#[test]
fn within_one_block_the_case_outranks_a_later_exception() {
    assert_open_binds(
        "open_member_order_one_block",
        &[
            "namespace Demo\n\ntype Holder =\n    | Target\n\nexception Target of int\n",
            "module Other\n\nopen Demo\n\nlet x = Target\n",
        ],
        "Target",
        0,
        "Target\n",
    );
}

#[test]
fn a_value_leaves_a_barless_case_the_pattern_namespace() {
    assert_open_binds(
        "open_member_order_barless_pattern",
        &[
            "module Demo\n\nmodule M =\n    type U =\n        | Target\n    let Target = 1\n    \
           type T = Target\n\nopen M\n\nlet f x =\n    match x with\n    | Target -> 0\n",
        ],
        "Target",
        0,
        "Target\n\nopen",
    );
}

#[test]
fn an_earlier_files_value_leaves_a_barless_case_the_pattern_namespace() {
    assert_open_binds(
        "open_member_order_barless_pattern_cross_file",
        &[
            "module M\n\ntype U =\n    | Target\nlet Target = 1\ntype T = Target\n",
            "module Other\n\nopen M\n\nlet f x =\n    match x with\n    | Target -> 0\n",
        ],
        "Target",
        0,
        "Target\n",
    );
}

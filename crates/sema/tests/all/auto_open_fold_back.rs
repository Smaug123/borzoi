//! **An `[<AutoOpen>]` module's members, where the walk can name them**: at
//! the module's close (`AddLocalSubModule` then its auto-open), through an
//! explicit `open` of the module holding it, and through an `open` of that
//! module from a later file. Each case must *commit* FCS's answer, not merely
//! avoid contradicting it: `auto_open_member_sweep` grades every shape, and
//! accepts a decline; these pin that the plain shapes are answered.

use crate::open_member_order::assert_fcs_graded_bind;

fn assert_commits(label: &str, files: &[&str], probe: &str, want_file: usize, want: &str) {
    assert_fcs_graded_bind(label, files, probe, want_file, want, true);
}

#[test]
fn a_value_folds_back_at_the_modules_close() {
    assert_commits(
        "fold_back_value",
        &[
            "module Demo\n\nlet Nn = 99\n\n[<AutoOpen>]\nmodule A =\n    let Nn = 1\n\n\
           let probe = Nn\n",
        ],
        "Nn",
        0,
        "Nn = 1",
    );
}

#[test]
fn a_private_value_stays_in_its_module() {
    assert_commits(
        "fold_back_private_value",
        &[
            "module Demo\n\nlet Nn = 99\n\n[<AutoOpen>]\nmodule A =\n    let private Nn = 1\n    \
           let other = Nn\n\nlet probe = Nn\n",
        ],
        "Nn",
        0,
        "Nn = 99",
    );
}

#[test]
fn a_union_case_folds_back_into_pattern_position() {
    assert_commits(
        "fold_back_case_pattern",
        &[
            "module Demo\n\n[<AutoOpen>]\nmodule A =\n    type U =\n        | Nn\n        \
           | Other\n\nlet probe x =\n    match x with\n    | Nn -> 0\n    | Other -> 1\n",
        ],
        "Nn",
        0,
        "Nn\n",
    );
}

/// The nested module folds back with its parent, but it is also a module a
/// later `open B` could name by its short name, which the walk cannot follow
/// (task #30), so the fold may only decline.
#[test]
fn a_nested_auto_open_module_folds_back_with_its_parent() {
    assert_fcs_graded_bind(
        "fold_back_nested",
        &[
            "module Demo\n\nlet Nn = 99\n\n[<AutoOpen>]\nmodule A =\n    [<AutoOpen>]\n    \
           module B =\n        let Nn = 1\n\nlet probe = Nn\n",
        ],
        "Nn",
        0,
        "Nn = 1",
        false,
    );
}

#[test]
fn an_open_folds_the_modules_auto_open_child() {
    assert_commits(
        "fold_back_open_same_file",
        &[
            "module Demo\n\nmodule H =\n    [<AutoOpen>]\n    module A =\n        let Nn = 1\n\n\
           let Nn = 99\n\nopen H\n\nlet probe = Nn\n",
        ],
        "Nn",
        0,
        "Nn = 1",
    );
}

#[test]
fn an_open_from_a_later_file_folds_the_auto_open_child() {
    assert_commits(
        "fold_back_open_cross_file",
        &[
            "module H\n\n[<AutoOpen>]\nmodule A =\n    let Nn = 1\n",
            "module Demo\n\nlet Nn = 99\n\nopen H\n\nlet probe = Nn\n",
        ],
        "Nn",
        0,
        "Nn = 1",
    );
}

#[test]
fn the_child_folds_after_the_modules_own_values() {
    assert_commits(
        "fold_back_open_child_after_parent",
        &[
            "module H\n\n[<AutoOpen>]\nmodule A =\n    let Nn = 1\n\nlet Nn = 2\n",
            "module Demo\n\nopen H\n\nlet probe = Nn\n",
        ],
        "Nn",
        0,
        "Nn = 1",
    );
}

/// A record never takes a value's unqualified slot, so a value in scope keeps
/// the qualifier of a dotted path from a later record of its name — whether
/// the value is written in the block or folded back into it.
#[test]
fn a_later_record_leaves_the_value_the_qualifier() {
    for (label, src) in [
        (
            "record_qualifier_inline",
            "module GridProbe\nlet id = \"contributed\"\ntype id =\n    { GridF: int }\n    \
             static member Length = 0\nlet probe = id.Length\n",
        ),
        (
            "record_qualifier_fold_back",
            "module GridProbe\n[<AutoOpen>]\nmodule GridAuto =\n    let id = \"contributed\"\n\
             type id =\n    { GridF: int }\n    static member Length = 0\nlet probe = id.Length\n",
        ),
    ] {
        assert_commits(label, &[src], "id", 0, "id = ");
    }
}

/// FCS enters every module nested in a folded one under its short name, so a
/// later `open Child` reaches it and its values outrank everything in scope:
/// the fold's, and a value from before the fold, which a `private` member or a
/// module folding nothing leaves standing. A project `[<AutoOpen>]` module
/// lends no shortening prefix (task #30), so the walk must not commit what that
/// `open` could replace. Every case runs, so a regression names each one.
#[test]
fn a_nested_module_of_a_folded_module_can_be_opened_by_its_short_name() {
    let cases: &[(&str, &[&str])] = &[
        (
            "fold_back_short_open",
            &[
                "module Demo\n\n[<AutoOpen>]\nmodule A =\n    let Nn = 1\n    module Child =\n        \
               let Nn = 2\n\nopen Child\n\nlet probe = Nn\n",
            ],
        ),
        (
            "open_child_short_open",
            &[
                "module Demo\n\nmodule H =\n    [<AutoOpen>]\n    module A =\n        let Nn = 1\n        \
               module Child =\n            let Nn = 2\n\nopen H\nopen Child\n\nlet probe = Nn\n",
            ],
        ),
        (
            "namespace_child_short_open",
            &[
                "namespace N\n\n[<AutoOpen>]\nmodule A =\n    let Nn = 1\n    module Child =\n        \
                 let Nn = 2\n",
                "module Demo\n\nopen N\nopen Child\n\nlet probe = Nn\n",
            ],
        ),
        (
            "short_open_private_fold",
            &[
                "module Demo\n\nlet Nn = 99\n\n[<AutoOpen>]\nmodule A =\n    let private Nn = 1\n    \
               module Child =\n        let Nn = 2\n\nopen Child\n\nlet probe = Nn\n",
            ],
        ),
        (
            "short_open_nothing_folds",
            &[
                "module Demo\n\nlet Nn = 99\n\n[<AutoOpen>]\nmodule A =\n    module Child =\n        \
               let Nn = 2\n\nopen Child\n\nlet probe = Nn\n",
            ],
        ),
        (
            "short_open_aliased_attribute",
            &[
                "module Demo\n\ntype AO = AutoOpenAttribute\n\nlet Nn = 99\n\n[<AO>]\nmodule A =\n    \
               module Child =\n        let Nn = 2\n\nopen Child\n\nlet probe = Nn\n",
            ],
        ),
    ];
    let failed: Vec<&str> = cases
        .iter()
        .filter(|(label, files)| {
            std::panic::catch_unwind(|| {
                assert_fcs_graded_bind(label, files, "Nn", 0, "Nn = 2", false)
            })
            .is_err()
        })
        .map(|(label, _)| *label)
        .collect();
    assert!(
        failed.is_empty(),
        "short-name opens graded wrong: {failed:?}"
    );
}

/// A project type spelled `AutoOpenAttribute` takes the spelling, so a child
/// marked `[<AutoOpen>]` is not auto-opened: no fold may trust the spelling.
#[test]
fn an_impersonated_auto_open_attribute_folds_nothing() {
    assert_fcs_graded_bind(
        "open_child_impersonated",
        &[
            "module Demo\n\ntype AutoOpenAttribute() =\n    inherit System.Attribute()\n\n\
           module H =\n    let Nn = 99\n    [<AutoOpen>]\n    module A =\n        let Nn = 1\n\n\
           open H\n\nlet probe = Nn\n",
        ],
        "Nn",
        0,
        "Nn = 99",
        false,
    );
    assert_fcs_graded_bind(
        "namespace_fold_impersonated",
        &[
            "namespace N\n\ntype AutoOpenAttribute() =\n    inherit System.Attribute()\n\n\
             [<AutoOpen>]\nmodule A =\n    let Nn = 1\n",
            "module Demo\n\nlet Nn = 99\n\nopen N\n\nlet probe = Nn\n",
        ],
        "Nn",
        1,
        "Nn = 99",
        false,
    );
}

/// An `open` of a project module makes its nested modules openable by short
/// name, with or without `[<AutoOpen>]`.
#[test]
fn a_short_open_through_a_plain_module_open() {
    assert_fcs_graded_bind(
        "short_open_plain_module",
        &[
            "module Demo\n\nmodule H =\n    module Child =\n        let Nn = 2\n\nlet Nn = 99\n\n\
           open H\nopen Child\n\nlet probe = Nn\n",
        ],
        "Nn",
        0,
        "Nn = 2",
        false,
    );
    assert_fcs_graded_bind(
        "short_open_plain_module_cross_file",
        &[
            "module H\n\nmodule Child =\n    let Nn = 2\n",
            "module Demo\n\nlet Nn = 99\n\nopen H\nopen Child\n\nlet probe = Nn\n",
        ],
        "Nn",
        0,
        "Nn = 2",
        false,
    );
}

/// A value evicted from the unqualified slot leaves `Nn.Length` to the latest
/// type of the name, which an `open` may bring after the lexical one.
#[test]
fn an_evicted_value_leaves_the_qualifier_to_the_latest_type() {
    assert_fcs_graded_bind(
        "evicted_by_opened_class",
        &[
            "namespace Lib\n\ntype Nn() =\n    static member Length = 1\n",
            "module Demo\n\ntype Nn =\n    { F: int }\n    static member Length = 2\n\n\
             let Nn = \"str\"\n\nopen Lib\n\nlet probe = Nn.Length\n",
        ],
        "Nn",
        0,
        "Nn()",
        false,
    );
}

/// An opaque `open` hides what a later `open` brings, but the later `open`'s
/// names still shadow everything before them: a binding written between the
/// two must not take the name back.
#[test]
fn an_opaque_open_does_not_expose_what_a_later_fold_shadows() {
    let cases: &[(&str, &[&str])] = &[
        (
            "opaque_open_then_fold_back",
            &[
                "module Demo\n\ntype T() =\n    class end\n\nopen type T\n\nlet Nn = 99\n\n\
               [<AutoOpen>]\nmodule A =\n    let Nn = 1\n\nlet probe = Nn\n",
            ],
        ),
        (
            "opaque_open_then_explicit_open",
            &[
                "module Demo\n\nmodule M =\n    let Nn = 1\n\ntype T() =\n    class end\n\n\
               open type T\n\nlet Nn = 99\n\nopen M\n\nlet probe = Nn\n",
            ],
        ),
    ];
    let failed: Vec<&str> = cases
        .iter()
        .filter(|(label, files)| {
            std::panic::catch_unwind(|| {
                assert_fcs_graded_bind(label, files, "Nn", 0, "Nn = 1", false)
            })
            .is_err()
        })
        .map(|(label, _)| *label)
        .collect();
    assert!(failed.is_empty(), "graded wrong: {failed:?}");
}

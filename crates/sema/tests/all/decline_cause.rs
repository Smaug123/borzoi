//! The decline census: every name-use we decline records *which guard declined
//! it* and *where in the referenced-assembly precedence ladder that guard sat*.
//!
//! The resolver already computes both — `decide_type_path` alone has a dozen
//! distinct `Deferred` branches, each behind a differently-named predicate —
//! and until now threw them away, collapsing all of them into one
//! indistinguishable [`DeferredReason::ShadowableType`]. That is what forced
//! every tier-ladder experiment to be priced by disabling one arm at a time and
//! re-running a whole-project differential: the aggregate could not say which
//! model owned which share of the loss.
//!
//! These are the FCS-free unit cases for the recording itself. The coverage
//! that matters rides on `tier_order_diff`'s ratchet, which records the
//! `(cause, tier)` pair per case and so fails when a change keeps a case
//! deferring but moves the guard that did it.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, DeclineCause, DeclineTier, DeferredReason, ProjectItems, Resolution, ResolvedFile,
    resolve_file,
};

fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
}

/// Every `(cause, tier)` the file recorded, deduplicated and sorted so a case
/// asserts on the *set* of guards that fired rather than on a range the
/// recording shell happens to key them by.
fn declines(src: &str) -> Vec<(DeclineCause, DeclineTier)> {
    let resolved = resolve(src);
    let mut seen: Vec<_> = resolved
        .decline_sites()
        .map(|(_, site)| (site.cause, site.tier))
        .collect();
    seen.sort_unstable();
    seen.dedup();
    seen
}

#[test]
fn a_bare_annotation_inside_a_rec_module_declines_as_a_forward_declaration() {
    let src = "module rec DeclineRec\n\nlet value : Widget = failwith \"x\"\n";
    assert_eq!(
        declines(src),
        vec![(DeclineCause::RecursiveModuleActive, DeclineTier::PreWalk)]
    );
}

#[test]
fn a_dotted_head_naming_a_rec_block_module_declines_as_a_forward_declaration() {
    let src = "\
module rec DeclineRecHead

module Sub =
    let inner = 1

let value : Sub.Calc = failwith \"x\"
";
    assert_eq!(
        declines(src),
        vec![(DeclineCause::RecursiveModuleHead, DeclineTier::PreWalk)]
    );
}

#[test]
fn a_path_descending_into_a_nested_module_declines_as_that_descent() {
    let src = "\
module DeclineNested

module Sub =
    let inner = 1

let value : Sub.Calc = failwith \"x\"
";
    assert_eq!(
        declines(src),
        vec![(DeclineCause::DescendsIntoNestedModule, DeclineTier::PreWalk)]
    );
}

/// The census's no-noise direction: a path that *resolves*, and a path that
/// genuinely matches nothing, both record no site. A decline site means "we
/// declined, and here is the guard"; a name with no site is not a decline the
/// census silently lost.
#[test]
fn a_resolved_or_absent_name_records_no_decline_site() {
    let src = "\
module DeclineNone

type Widget = { Field : int }

let value : Widget = { Field = 1 }
";
    assert_eq!(declines(src), vec![]);
}

/// Totality, which is the property the census actually rests on: a
/// [`DeferredReason::ShadowableType`] is exactly the type-position decline
/// these guards produce, so every one of them must carry a site. A guard added
/// later without a cause would show up here as an unattributed decline.
#[test]
fn every_shadowable_type_deferral_carries_a_site() {
    for src in [
        "module rec DeclineRec\n\nlet value : Widget = failwith \"x\"\n",
        "module DeclineNested\n\nmodule Sub =\n    let inner = 1\n\nlet value : Sub.Calc = failwith \"x\"\n",
        "module rec DeclineRecHead\n\nmodule Sub =\n    let inner = 1\n\nlet value : Sub.Calc = failwith \"x\"\n",
    ] {
        let resolved = resolve(src);
        for (&range, &resolution) in resolved.resolutions() {
            if resolution == Resolution::Deferred(DeferredReason::ShadowableType) {
                assert!(
                    resolved.decline_site(range).is_some(),
                    "unattributed ShadowableType deferral at {range:?} in {src:?}"
                );
            }
        }
    }
}

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
    AssemblyEnv, DeclineCause, DeclineSite, DeclineTier, DeferredReason, ProjectItems, Resolution,
    ResolvedFile, SyntaxRecovery, resolve_file,
};
use rowan::{TextRange, TextSize};

use crate::attr_resolution_diff::fsharp_core_env;

fn resolve(src: &str) -> ResolvedFile {
    resolve_in(src, &AssemblyEnv::default())
}

fn resolve_in(src: &str, env: &AssemblyEnv) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {src:?}: {:?}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), env, &recovery)
}

/// The site recorded at the one occurrence of `needle` inside the one
/// occurrence of `context` in `src`.
fn site_at(resolved: &ResolvedFile, src: &str, context: &str, needle: &str) -> Option<DeclineSite> {
    let mut at = src.match_indices(context);
    let (start, _) = at
        .next()
        .unwrap_or_else(|| panic!("{context:?} is not in the snippet"));
    assert!(at.next().is_none(), "{context:?} is not unique");
    let start = start + context.find(needle).expect("needle inside its context");
    let range = TextRange::new(
        TextSize::try_from(start).expect("fits"),
        TextSize::try_from(start + needle.len()).expect("fits"),
    );
    resolved.decline_site(range)
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

/// A dotted **value** path can be declined before the assembly walk is reached
/// at all — an opaque `open` could supply its head. The walk never runs, so
/// nothing downstream of it can name the guard; the census has to record it
/// where the fallback defers.
#[test]
fn an_opaque_open_names_the_guard_that_declined_a_dotted_value_head() {
    let src = "\
module DeclineOpaqueValue

module Sub =
    let inner = 1

open Sub

let value = Zed.foo
";
    assert!(
        declines(src).contains(&(DeclineCause::OpaqueValueHead, DeclineTier::PreWalk)),
        "expected an opaque-value-head decline, got {:?}",
        declines(src)
    );
}

/// An attribute name is a *type* use resolved through the same walk and the
/// same shadow guards as a written annotation, so a census blind to it would
/// under-report exactly the guards it exists to weigh.
#[test]
fn a_declined_attribute_names_its_guard_too() {
    let src = "\
module DeclineAttr

module Sub =
    let inner = 1

open Sub

[<Widget>]
let value = 1
";
    let sites: Vec<_> = declines(src);
    assert!(
        sites.iter().any(|(cause, _)| matches!(
            cause,
            DeclineCause::OpaqueOpen
                | DeclineCause::AttributeUnrulable
                | DeclineCause::AttributeInFileUnreliable
                | DeclineCause::ProjectTypeShadow
                | DeclineCause::AttributeOpaqueLeaf
        )),
        "expected the declined attribute to name a guard, got {sites:?}"
    );
}

/// A **bare** name no scope entry binds, though an assembly surface F# opens
/// with no source `open` supplies it, names that: `MatchFailureException` is an
/// exception of the implicitly opened `Microsoft.FSharp.Core`, whose exceptions
/// sema folds no entry for (its union cases it does fold — `Some` beside it
/// commits). Recorded at the name, since a bare name is the whole of its own
/// path.
#[test]
fn an_implicitly_opened_exception_names_the_surface_sema_does_not_fold() {
    let src = "\
module DeclineCase

let e = MatchFailureException (\"f\", 1, 2)
let o = Some 1
";
    let resolved = resolve_in(src, &fsharp_core_env());
    assert_eq!(
        site_at(
            &resolved,
            src,
            "= MatchFailureException",
            "MatchFailureException"
        ),
        Some(DeclineSite {
            cause: DeclineCause::ImplicitSurfaceUnfolded,
            tier: DeclineTier::PreWalk,
        }),
    );
    assert_eq!(site_at(&resolved, src, "= Some 1", "Some"), None);
}

/// An entry an explicit `open` put in scope **declining** names why the open
/// declined it: an assembly active-pattern tag has no member to point at.
#[test]
fn an_opened_assembly_pattern_tag_names_why_it_commits_nothing() {
    let src = "\
module DeclineTag

open Microsoft.FSharp.Quotations.Patterns

let f (q : Microsoft.FSharp.Quotations.Expr) =
    match q with
    | Lambda (v, b) -> 1
    | _ -> 0
";
    let resolved = resolve_in(src, &fsharp_core_env());
    assert_eq!(
        site_at(&resolved, src, "| Lambda", "Lambda"),
        Some(DeclineSite {
            cause: DeclineCause::AssemblyCaseUntargeted,
            tier: DeclineTier::PreWalk,
        })
    );
}

/// An opaque `open` withdrawing an opened entry the name would otherwise have
/// found is that name's guard — the bare-name face of an `open` whose values
/// cannot be listed (`open type` of a project type here; on real projects an
/// assembly module the projection cannot enumerate).
#[test]
fn an_opaque_open_names_the_bare_names_it_withdraws() {
    let src = "\
module DeclineOpaqueBare

type T =
    static member Make () = 1

open type T

let w = failwith \"x\"
";
    let resolved = resolve_in(src, &fsharp_core_env());
    let opaque = Some(DeclineSite {
        cause: DeclineCause::OpaqueOpen,
        tier: DeclineTier::PreWalk,
    });
    assert_eq!(site_at(&resolved, src, "= failwith", "failwith"), opaque);
}

/// The no-noise direction for bare names: one nothing in scope binds records
/// no site, whatever opens are live — an opaque `open` is no evidence it is
/// why *this* name found nothing.
#[test]
fn a_bare_name_nothing_binds_records_no_site() {
    let src = "\
module DeclineAbsent

type T =
    static member Make () = 1

open type T

let v = nowhere
";
    let resolved = resolve_in(src, &fsharp_core_env());
    assert_eq!(site_at(&resolved, src, "= nowhere", "nowhere"), None);
}

//! Two-pass evaluation of a parsed `.fsproj` XML tree, mirroring
//! MSBuild's own pass ordering:
//!
//!   1. **Property pass** — a document-order walk over the project body
//!      and (in the with-imports variant) every followed import.
//!      `<PropertyGroup>`s evaluate as encountered, so a property VALUE's
//!      forward reference to a name defined later in the file reads empty
//!      and emits [`DiagnosticKind::UndefinedProperty`]. Each
//!      `<ItemGroup>` encountered is *deferred* — recorded, in encounter
//!      order, for the second pass ([`State::defer_item_group`]).
//!   2. **Item pass** ([`item_pass::replay_deferred_item_groups`]) — the
//!      deferred groups evaluate against the now-final property table, so
//!      an item's `Include`, `Condition`, and metadata see properties no
//!      matter where in the import graph they were written. This matches
//!      MSBuild, which finalises every property before evaluating any
//!      item.
//!
//! `Condition` attributes are evaluated on every gated construct
//! (`<ItemGroup>`, `<PropertyGroup>`, individual `<Compile>` items,
//! individual property elements), each against the property table of its
//! own pass. Conditions outside the supported subset surface as
//! [`DiagnosticKind::UnsupportedCondition`] and treat the containing
//! construct as **excluded** — plan D5's "never silently include"
//! stance, since we can't tell whether the construct would have
//! contributed any items. Conditions that we *can* evaluate but only
//! by treating an unknown `$(Name)` reference as the empty string
//! emit [`DiagnosticKind::UndefinedProperty`] for each such name, so
//! the project is marked partial: our property map may be missing
//! values MSBuild itself would have seen.
//!
//! Property values and item attributes have `$(Name)` substituted
//! against a map seeded with caller globals and well-known MSBuild
//! path properties, then extended by the project's own
//! `<PropertyGroup>` writes.
//!
//! What the pure entry point still doesn't model: `<Import>`, `<Choose>`,
//! glob expansion, item-list `@(...)` and metadata `%(...)` references.
//! Each emits a [`Diagnostic`] and marks the project partial.
//!
//! ## Two entry points
//!
//! [`walk`] is the pure document-order walker reached from
//! [`super::parse_fsproj`]: no IO, `<Import>` elements emit
//! [`DiagnosticKind::UnresolvedImport`].
//!
//! [`walk_with_imports`] is the filesystem-touching variant reached from
//! [`super::parse_fsproj_with_imports`]. It follows explicit `<Import>`
//! (substituting `$(...)` in the path, resolving relative to the
//! importing file, recursing depth-limited, with duplicate imports —
//! cycles included — skipped exactly as MSBuild skips them) and
//! also splices in the nearest `Directory.Build.props` /
//! `Directory.Build.targets` discovered by
//! [`super::detect_implicit_imports`]. Walked-imported files share the
//! same [`State`] as the project body, so properties they define
//! become visible to substitutions further down the project file.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use roxmltree::{Document, Node};

use super::condition;
use super::diagnostic::{
    CompileConditionReason, CompileConditionUncertainty, CompileItemUncertaintyCause,
    CompileItemUncertaintyCauseKind, Diagnostic, DiagnosticKind, DiagnosticOrigin,
    ImportFailReason, PackageReferenceUncertaintyCause, PackageReferenceUncertaintyCauseKind,
    StructuralCompileItemUncertainty, StructuralPackageReferenceUncertainty,
};
use super::imports::normalise;
use super::properties::escaping::Escaped;
use super::properties::{self, Issue, PropertyMap};
use super::{
    FrameworkReference, GlobRequest, GlobResolver, GlobalPackageReference, ItemKind,
    ItemMetadataValue, PackageRefOp, PackageReference, PackageVersion, ParsedProject, ResolvedItem,
    SdkPaths, SdkResolution, SdkResolveError, SdkResolver,
};

mod item_pass;
use item_pass::{
    CapturedPackageReference, CurrentFile, DeferredGroupKind, DeferredItemGroup,
    RetainedImportedFile, item_key, replay_deferred_item_groups,
};

/// Maximum recursion depth for `<Import>` following. MSBuild itself
/// caps at ~100; we pick a number well above any sane real-world
/// project's chain depth (project → `Directory.Build.props` → maybe
/// one or two more is typical) but well below the stack-overflow
/// danger zone for hostile inputs.
const MAX_IMPORT_DEPTH: usize = 64;

/// Pure walker reachable from [`super::parse_fsproj`]: no IO, every
/// `<Import>` element produces an [`DiagnosticKind::UnresolvedImport`]
/// diagnostic and is *not* followed.
pub fn walk(
    doc: &Document<'_>,
    project_path: &Path,
    extra_properties: &HashMap<String, String>,
    environment: &HashMap<String, String>,
) -> ParsedProject {
    let root = doc.root_element();
    let local_overrides = collect_local_overrides(root);
    let mut state = State::new(
        project_path,
        extra_properties,
        environment,
        &local_overrides,
        false,
        None,
        None,
    );
    let project_dir = project_path.parent().unwrap_or_else(|| Path::new(""));
    // SDK resolution requires IO, which the pure walker doesn't do.
    // Surface the attribute so the caller knows the result may be
    // incomplete (Compile defaults, etc.). Mirrors what
    // `walk_with_imports` does in the no-resolver branch — including marking
    // the Compile set uncertain, since the SDK's (possibly default-item)
    // contributions never ran.
    if let Some(sdk) = root.attribute("Sdk") {
        state.push(
            DiagnosticKind::UnsupportedConstruct {
                element: format!("Project Sdk={sdk:?}"),
            },
            root.range(),
        );
        state.mark_structural_skip(
            StructuralCompileItemUncertainty::ProjectSdkUnsupported {
                sdk: sdk.to_string(),
            },
            root.range(),
        );
    }
    walk_doc_body(root, project_dir, &mut state);
    replay_deferred_item_groups(doc, &mut state);
    state.into_project()
}

/// Whether the deferred second pass in [`walk_with_imports`] could change
/// the result, so is worth running. It cannot when the entry already has
/// its own SDK (pass 1's before-body splice is already faithful), nor when
/// there is no SDK resolver — then no nested `<Project Sdk=...>` can
/// resolve, the deferred `Directory.Build.props` splice can never be
/// consumed, and pass 2 would dangle straight back to pass 1. Both cases
/// would discard pass 2's entire walk, so we skip it.
fn deferred_pass_can_change_result(entry_has_sdk: bool, has_sdk_resolver: bool) -> bool {
    !entry_has_sdk && has_sdk_resolver
}

/// With-imports walker reachable from [`super::parse_fsproj_with_imports`].
/// Walks (in order): the implicit `Directory.Build.props` if any, then
/// the project body, then the implicit `Directory.Build.targets` if any.
/// All three share one [`State`] so properties written by an earlier
/// stage are visible to substitutions in later stages — matching
/// MSBuild's left-to-right evaluation. Explicit `<Import>` elements
/// inside any of those bodies are followed too.
///
/// When `sdk_resolver` is supplied AND the project root carries an
/// `Sdk` attribute that the resolver maps to `Some(SdkPaths)`, the
/// walker also splices `Sdk.props` (outermost-before-body) and
/// `Sdk.targets` (outermost-after-body) around the Directory.Build.*
/// pair: `Sdk.props → Directory.Build.props → body →
/// Directory.Build.targets → Sdk.targets`. That matches MSBuild's
/// effective ordering, where `Microsoft.Common.props` imports
/// Directory.Build.props *after* the SDK chain has already set
/// properties like `UsingMicrosoftNETSdk`. The Directory.Build.*
/// splice stays live even when the SDK resolves, because MSBuild's
/// imports of those files are produced by deeper SDK machinery; the
/// explicit splice keeps that import point under this evaluator's direct
/// control.
/// When the resolver returns `None` (SDK unknown to the caller) the
/// walker emits [`DiagnosticKind::SdkNotFound`] and the body still
/// gets the Directory.Build.* splice. When `sdk_resolver` is `None`,
/// the SDK attribute surfaces as `UnsupportedConstruct` — same as
/// the pure walker.
///
/// **Directory.Build.props position (two-pass).** MSBuild imports
/// `Directory.Build.props` exactly once, right after the *first*
/// `Sdk.props` to run (that `Sdk.props` re-enters
/// `Microsoft.Common.props`, which imports `Directory.Build.props`).
/// When the entry project has its own SDK, the first `Sdk.props` is the
/// entry's and the eager before-body splice already matches. The one
/// shape that diverges is an entry project with *no* resolvable SDK but
/// a *nested* imported file that does — MSBuild's first `Sdk.props` is
/// then the nested one (mid-body), so `Directory.Build.props` belongs
/// right after it. For an entry with no SDK (and an SDK resolver present,
/// so a nested SDK *can* resolve) we therefore always run a second pass
/// with the splice deferred to the first body-reached nested `Sdk.props`
/// (the faithful model), keeping the eager first pass only as the
/// fallback for the *dangle* case where no such nested `Sdk.props` runs at
/// all (see the in-function comment).
#[allow(clippy::too_many_arguments)]
pub fn walk_with_imports<'r>(
    doc: &Document<'_>,
    project_path: &Path,
    extra_properties: &HashMap<String, String>,
    environment: &HashMap<String, String>,
    implicit_props: Option<&Path>,
    implicit_targets: Option<&Path>,
    detected_packages_props: Option<&Path>,
    sdk_resolver: Option<&'r SdkResolver<'r>>,
    glob_resolver: Option<&'r GlobResolver<'r>>,
) -> ParsedProject {
    // Pass 1: the eager before-body `Directory.Build.props` splice
    // (historical behaviour). This is *only* a fallback result for the
    // dangle case below — we deliberately read no detection signal off it
    // except `entry_has_sdk`, which is resolved from the root `Sdk=`
    // attribute / explicit promotion *before* any `Directory.Build.props`
    // or body runs and so is identical in both passes. Every other pass-1
    // observation is contaminated: pass 1 runs `Directory.Build.props` in
    // the wrong (before-body) position, so a property it sets can flip a
    // body `<Import>` condition and change which nested SDKs are reached
    // — making "did a nested `Sdk.props` fire?" unreliable in pass 1.
    let (pass1, obs1) = walk_once(
        doc,
        project_path,
        extra_properties,
        environment,
        implicit_props,
        implicit_targets,
        detected_packages_props,
        sdk_resolver,
        glob_resolver,
        false,
    );
    // Skip the deferred second pass whenever it could not possibly change
    // the result — when the entry already has its own SDK (pass 1 is
    // faithful), or when there is no SDK resolver (no nested
    // `<Project Sdk=...>` can resolve, so the deferred splice can never be
    // consumed and pass 2 would dangle straight back to pass 1). Otherwise
    // we always run it: we cannot decide cheaply from pass 1 whether the
    // faithful order reaches a nested `Sdk.props`, because pass 1's eager
    // `Directory.Build.props` may itself suppress (or spuriously enable)
    // the import that leads there. Only the deferred walk evaluates body
    // import conditions in MSBuild's real order, so only it can answer.
    if !deferred_pass_can_change_result(obs1.entry_has_sdk, sdk_resolver.is_some()) {
        return pass1;
    }
    // Pass 2: defer the `Directory.Build.props` splice to the first
    // body-reached nested `Sdk.props`.
    let (pass2, obs2) = walk_once(
        doc,
        project_path,
        extra_properties,
        environment,
        implicit_props,
        implicit_targets,
        detected_packages_props,
        sdk_resolver,
        glob_resolver,
        true,
    );
    // Pass 2 is the faithful single-pass model: it fires
    // `Directory.Build.props` at exactly the first body-reached nested
    // `Sdk.props`, re-evaluating the import gate/override there (just as
    // MSBuild does, after the body and the nested `Sdk.props` have run).
    // So body imports *before* that point don't see Directory.Build.props
    // and imports *after* do — matching MSBuild, including when those
    // properties flip a conditional `<Import>`. The one shape pass 2
    // cannot model is a *dangle*: no body-reached nested `Sdk.props` ever
    // runs (either there is none, or the import that would reach it is
    // gated on a property only `Directory.Build.props` sets, so deferring
    // it suppresses the nested SDK). MSBuild would then import no
    // `Directory.Build.props` at all — but emitting nothing is a
    // surprising regression versus the historical before-body splice, so
    // we fall back to pass 1. We detect the dangle precisely:
    // `pending_directory_build_props` is still `Some`, i.e. never
    // `take()`n.
    if obs2.directory_build_props_pending_unconsumed {
        pass1
    } else {
        pass2
    }
}

/// Observations read off [`State`] at the end of one [`walk_once`] pass,
/// driving the two-pass decision in [`walk_with_imports`]. None are part
/// of [`ParsedProject`].
struct WalkObservations {
    /// The entry project resolved an SDK of its own (root `Sdk=`
    /// shorthand or explicit-form promotion). When true the eager
    /// before-body splice already matches MSBuild — no second pass.
    entry_has_sdk: bool,
    /// Pass 2 only: the deferred `Directory.Build.props` was stashed but
    /// never fired (no body-reached nested `Sdk.props` consumed it). The
    /// dangle case — the orchestrator falls back to pass 1 rather than
    /// emit a project with no `Directory.Build.props`. Always `false` on
    /// pass 1 (nothing is ever deferred there).
    directory_build_props_pending_unconsumed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OrderedResolvedItem {
    order: usize,
    item: ResolvedItem,
}

fn into_resolved_items(items: Vec<OrderedResolvedItem>) -> Vec<ResolvedItem> {
    items.into_iter().map(|ordered| ordered.item).collect()
}

/// One evaluation pass. `defer_directory_build_props` controls whether
/// the entry `Directory.Build.props` is spliced eagerly before the body
/// (`false`) or stashed in `State::pending_directory_build_props` to be
/// fired at the first nested `Sdk.props` (`true`). The public
/// [`walk_with_imports`] is the two-pass orchestrator over this.
#[allow(clippy::too_many_arguments)]
fn walk_once<'r>(
    doc: &Document<'_>,
    project_path: &Path,
    extra_properties: &HashMap<String, String>,
    environment: &HashMap<String, String>,
    implicit_props: Option<&Path>,
    implicit_targets: Option<&Path>,
    detected_packages_props: Option<&Path>,
    sdk_resolver: Option<&'r SdkResolver<'r>>,
    glob_resolver: Option<&'r GlobResolver<'r>>,
    defer_directory_build_props: bool,
) -> (ParsedProject, WalkObservations) {
    let root = doc.root_element();
    let local_overrides = collect_local_overrides(root);
    let mut state = State::new(
        project_path,
        extra_properties,
        environment,
        &local_overrides,
        true,
        sdk_resolver,
        glob_resolver,
    );
    // Seed `imports_seen` with the entry project so an import that names
    // the entry — directly or through a chain — is skipped the way
    // MSBuild skips it (warning MSB4210, "attempting to import itself";
    // the evaluation succeeds).
    state.imports_seen.insert(import_dedup_key(project_path));
    state
        .imports_seen_fuzzy
        .insert(import_dedup_fuzzy_key(project_path));
    state
        .walked_files
        .insert(canonicalise_or_normalise(project_path));
    state.implicit_directory_build_props_fallback = implicit_props.map(Path::to_path_buf);
    let project_dir = project_path.parent().unwrap_or_else(|| Path::new(""));
    state.directory_build_props_splice_pending = true;
    if let Some(Resolution { path, .. }) = resolve_directory_build_path(
        &state,
        "DirectoryBuildPropsPath",
        implicit_props,
        project_dir,
    ) && should_import_default_true(
        state
            .lookup
            .get_unescaped("ImportDirectoryBuildProps")
            .as_deref(),
        state.is_sticky_global("ImportDirectoryBuildProps"),
    ) {
        // Sdk.props can contain the standard Directory.Build.props rediscovery
        // import before this walker reaches its explicit splice. Pre-record
        // the import point we currently own so that standard rediscovery is
        // still suppressed, while later SDK retargets to a different path keep
        // flowing through normal import evaluation.
        state.directory_build_props_splice_path = Some(canonicalise_or_normalise(&path));
    }

    // Resolve the project root's SDK shorthand (if any). The returned
    // paths, when `Some`, are spliced as the OUTERMOST pair around the
    // Directory.Build.* splice, matching MSBuild's effective ordering:
    // Sdk.props → Directory.Build.props → body → Directory.Build.targets
    // → Sdk.targets. MSBuild gets this ordering naturally because the
    // SDK chain itself imports Directory.Build.* (from inside
    // `Microsoft.Common.props`) *after* setting its own properties such
    // as `UsingMicrosoftNETSdk` — so a Directory.Build.props that
    // conditions on those names sees them defined. We keep the
    // Directory.Build.* splice live even when the SDK resolves so that
    // the implicit import point is handled in one controlled place instead
    // of relying on deeper SDK files to rediscover and import the same
    // Directory.Build.* files. Splicing Sdk.props *before*
    // Directory.Build.props is the crucial bit: a Directory.Build.props
    // that checks
    // `<PropertyGroup Condition="'$(UsingMicrosoftNETSdk)' == 'true'">`
    // would otherwise emit UndefinedProperty here even though MSBuild
    // would have entered the group.
    // Two ways the SDK chain can attach: the root-`Sdk` shorthand
    // (handled by [`resolve_project_sdk`]) and MSBuild's explicit form
    // — `<Project>` with no `Sdk` attribute but body `<Import Sdk="X"
    // Project="Sdk.props"/>` / `Sdk.targets`. The explicit form is
    // rarer but semantically equivalent, and the body walk would
    // otherwise see the SDK imports *after* Directory.Build.props has
    // already run (which itself depends on SDK-supplied properties).
    // [`find_explicit_sdk_promotion`] hoists those body Imports to
    // root-equivalent positions; the ranges it returns get marked in
    // `state.hoisted_sdk_imports` so the body walk skips them on its
    // pass (otherwise the body walk would re-reach the spliced files:
    // harmless for `Sdk.props` — the top splice already registered it,
    // so the duplicate-import skip fires — but `Sdk.targets` splices at
    // the *bottom*, so a body-position walk would run it first and the
    // splice itself would become the skipped duplicate, inverting
    // MSBuild's order).
    //
    // Splice the props/targets *only when* the project asked for it.
    // For the root-Sdk shorthand both always splice (the shorthand
    // expands to `Sdk.props` *and* `Sdk.targets`); the import-site
    // span attributes to the root `<Project>` element, which is
    // where the `Sdk="X"` attribute lives. For the explicit form we
    // honour what the user wrote: `Sdk.props` is required to trigger
    // promotion at all; `Sdk.targets` only splices if the body has a
    // matching `<Import Sdk=... Project="Sdk.targets"/>`. The
    // import-site span for those splices points at the user's
    // actual body `<Import>` element so diagnostics/items
    // contributed by the spliced SDK file are attributed to the
    // source location the user wrote, not the whole `<Project>`.
    let (sdk_props_to_splice, sdk_targets_to_splice) = match resolve_project_sdk(root, &mut state) {
        Some(sdk_paths) => {
            // The entry project's own root `Sdk` *is* the framework SDK —
            // record it (the one place we do).
            state.record_sdk_root(&sdk_paths.root);
            (
                Some((sdk_paths.props, root.range())),
                Some((sdk_paths.targets, root.range())),
            )
        }
        None => match find_explicit_sdk_promotion(root, &state) {
            Some(promotion) => {
                // The entry's promoted explicit-form SDK — the same framework
                // SDK in `<Import Sdk=... Project="Sdk.props"/>` spelling.
                state.record_sdk_root(&promotion.sdk_paths.root);
                state
                    .hoisted_sdk_imports
                    .insert(promotion.props_range.clone());
                let targets = promotion.targets_range.map(|range| {
                    state.hoisted_sdk_imports.insert(range.clone());
                    (promotion.sdk_paths.targets, range)
                });
                (
                    Some((promotion.sdk_paths.props, promotion.props_range)),
                    targets,
                )
            }
            None => (None, None),
        },
    };

    if let Some((props, span)) = sdk_props_to_splice.as_ref() {
        walk_external_file(props, span.clone(), &mut state);
    }

    // MSBuild gates the implicit Directory.Build files on two
    // well-known properties. `ImportDirectoryBuildProps` is checked
    // *before* the project body, so its value must already be set
    // when we look (the caller's `extra_properties`, or
    // unset/empty meaning "import"). `ImportDirectoryBuildTargets`
    // is checked *after* the body, so the project itself can write
    // it via `<PropertyGroup>` to opt out.
    //
    // `Directory.Build{Props,Targets}Path` are MSBuild's escape
    // hatch for redirecting the implicit import to a specific file.
    // When set (case-insensitively non-empty) and the file exists,
    // MSBuild imports that path instead of walking the tree to find
    // the nearest `Directory.Build.*`. The override is resolved at
    // the same point as the gate check — before-body for props,
    // after-body for targets — so a project body can redirect the
    // targets import but not the props one.
    if defer_directory_build_props {
        // Pass 2: defer the *entire* import decision — the
        // `ImportDirectoryBuildProps` gate, the `DirectoryBuildPropsPath`
        // override, and the resolved path — to the first body-reached
        // nested `Sdk.props` (in `walk_external_file`), where MSBuild
        // actually performs it (after the body and the nested `Sdk.props`
        // have run). Stash only the fallback path that the fire site
        // cannot otherwise reach; everything else is re-read from live
        // state there.
        state.pending_directory_build_props = Some(DeferredDirectoryBuildProps {
            fallback: implicit_props.map(Path::to_path_buf),
        });
    } else if state.directory_build_props_splice_pending {
        // Fallback position for SDK chains that never pass through the
        // real `Microsoft.Common.props` Directory.Build.props import
        // point (synthetic SDKs, or no SDK at all): fire the splice
        // here, right after `Sdk.props` — the historical position. When
        // the SDK walk above DID reach that import point, the splice
        // already fired there (see `follow_explicit_import`) and the
        // pending flag is already down.
        fire_entry_directory_build_props_splice(&mut state);
    }

    // Only nested SDKs reached *during the body walk* count as MSBuild's
    // "first `Sdk.props`" (and may fire the deferred splice). Nested SDKs
    // reached while splicing the before-body `Directory.Build.props`
    // (above) or the after-body `Directory.Build.targets` / entry
    // `Sdk.targets` (below) sit on the wrong side of that import.
    state.in_entry_body = true;
    walk_doc_body(root, project_dir, &mut state);
    state.in_entry_body = false;

    let targets_to_import = resolve_directory_build_path(
        &state,
        "DirectoryBuildTargetsPath",
        implicit_targets,
        project_dir,
    );
    if let Some(Resolution { path, source }) = targets_to_import.as_ref()
        && should_import_default_true(
            state
                .lookup
                .get_unescaped("ImportDirectoryBuildTargets")
                .as_deref(),
            state.is_sticky_global("ImportDirectoryBuildTargets"),
        )
    {
        if matches!(source, ResolutionSource::Fallback) {
            seed_directory_build_path(&mut state, "DirectoryBuildTargetsPath", path);
        }
        walk_directory_build_file(path, 0..0, DirectoryBuildFile::Targets, &mut state);
    }
    if let Some((targets, span)) = sdk_targets_to_splice.as_ref() {
        walk_external_file(targets, span.clone(), &mut state);
    }

    // The property pass is complete — every import has been followed and
    // every `<PropertyGroup>` evaluated. Run the item pass against the
    // final property table.
    replay_deferred_item_groups(doc, &mut state);

    // A `Directory.Packages.props` detected up-tree holds central package
    // versions (and can enable CPM), so a walk that never reached it has
    // an incomplete/unversioned captured set. When the walk *did* import
    // it — via the real chain `Sdk.props` → `Microsoft.Common.props` →
    // `NuGet.props` → `$(DirectoryPackagesPropsPath)` — its contents are
    // ordinary captured state and need no blanket flag; anything still
    // untrustworthy about them is reported by its own precise cause.
    // Conservative on purpose in the other direction: the walk may have
    // skipped the file because NuGet's own gates said so (e.g.
    // `ImportDirectoryPackagesProps=false`), which a real build would
    // also skip — distinguishing that certain skip from "chain never
    // reached NuGet.props" is a future refinement.
    // A walked central-package import point also discharges: when a repo
    // redirects `DirectoryPackagesPropsPath` away from the detected
    // ancestor and the chain imported the redirect target, that ancestor
    // is not part of the real build either, and the file that IS got
    // captured like any other import. The check is *provenance* — the
    // file walked through the `$(DirectoryPackagesPropsPath)`-shaped
    // import, cross-checked against the final property value — because
    // every final-state signal (the `CentralPackageVersionsFileImported`
    // marker, a path value that happens to name some walked file) can be
    // written by a project without any central file having been
    // evaluated.
    let redirected_central_file_walked = state
        .walked_directory_packages_props_import
        .as_ref()
        .is_some_and(|walked| {
            state
                .lookup
                .get_unescaped("DirectoryPackagesPropsPath")
                .map(|raw| raw.trim().replace('\\', "/"))
                .filter(|path| !path.is_empty())
                .is_some_and(|path| canonicalise_or_normalise(Path::new(&path)) == *walked)
        });
    if let Some(path) = detected_packages_props
        && !redirected_central_file_walked
        && !state
            .walked_files
            .contains(&canonicalise_or_normalise(path))
    {
        state.package_references_uncertain = true;
        state
            .package_reference_uncertainties
            .push(PackageReferenceUncertaintyCause {
                kind: PackageReferenceUncertaintyCauseKind::DirectoryPackagesProps {
                    path: path.to_path_buf(),
                },
                span: 0..0,
                origin: DiagnosticOrigin::Imported,
            });
    }

    let observations = WalkObservations {
        entry_has_sdk: sdk_props_to_splice.is_some(),
        directory_build_props_pending_unconsumed: state.pending_directory_build_props.is_some(),
    };
    (state.into_project(), observations)
}

/// Examine the project root's `Sdk` attribute and decide whether the
/// SDK chain will handle the import job. Four outcomes:
///   * no attribute → returns `None`, caller takes the Directory.Build.*
///     path silently;
///   * attribute present, resolver supplied and returns `Ok(paths)` →
///     returns those paths, no diagnostic;
///   * attribute present but resolver missing → emits
///     [`DiagnosticKind::UnsupportedConstruct`] and returns `None`;
///   * attribute present and resolver returned an `Err` → emits the
///     diagnostic matching the [`SdkResolveError`] variant (see
///     [`crate::SdkResolver`] for the mapping) and returns `None`.
///
/// Fall-back semantics let an unknown SDK degrade gracefully rather
/// than producing a project with no imports at all: the real build
/// would fail outright, but as a parser we'd rather still surface a
/// best-effort Compile list and let the caller decide what to do with
/// the diagnostic.
fn resolve_project_sdk(root: Node<'_, '_>, state: &mut State<'_>) -> Option<SdkPaths> {
    let sdk = root.attribute("Sdk")?;
    let Some(resolver) = state.sdk_resolver else {
        state.push(
            DiagnosticKind::UnsupportedConstruct {
                element: format!("Project Sdk={sdk:?}"),
            },
            root.range(),
        );
        // The SDK couldn't be evaluated (no resolver — e.g. the LSP found no
        // `dotnet`), so its default-item machinery never ran. Our Compile list
        // is then a *subset* (the body's explicit items only); for a project
        // relying on default items it's incomplete, so the Compile set is
        // untrustworthy. (Cross-assembly resolution needs the SDK packs anyway,
        // which are equally absent.)
        state.mark_structural_skip(
            StructuralCompileItemUncertainty::ProjectSdkUnsupported {
                sdk: sdk.to_string(),
            },
            root.range(),
        );
        return None;
    };
    match resolver(sdk) {
        // SDK-root *recording* (for `ParsedProject::resolved_sdk_root`) is the
        // entry caller's responsibility; but the *tolerance* set must include
        // every resolved SDK — entry, nested, and via `<Import Sdk=…>` — so its
        // own conditional default-item machinery isn't mistaken for user intent.
        Ok(SdkResolution::Single(paths)) => {
            state.note_sdk_tolerance(&paths.root);
            Some(paths)
        }
        Ok(SdkResolution::Roots(_)) => {
            // A locator-style resolution has no Sdk.props/Sdk.targets entry
            // points, so it cannot back the `<Project Sdk="…">` shorthand.
            // No real project names a locator here; degrade like an
            // unresolvable SDK rather than inventing entry points.
            state.push(
                DiagnosticKind::UnsupportedConstruct {
                    element: format!("Project Sdk={sdk:?} (locator-style resolution)"),
                },
                root.range(),
            );
            state.mark_structural_skip(
                StructuralCompileItemUncertainty::ProjectSdkUnsupported {
                    sdk: sdk.to_string(),
                },
                root.range(),
            );
            None
        }
        Err(err) => {
            state.push(sdk_error_to_diagnostic(sdk, err), root.range());
            None
        }
    }
}

/// The body pre-scan's result when a project uses MSBuild's explicit
/// SDK form (no root `Sdk` attribute, `<Import Sdk="X" Project="Sdk.props"/>`
/// inside the body) and we want to splice the SDK chain at root-equivalent
/// positions. `props_range` is the byte range of the body Import node we
/// promoted; `targets_range` is the matching `Sdk.targets` body Import
/// if one exists (`None` if the project only declared `Sdk.props`).
/// Both ranges are added to `state.hoisted_sdk_imports` so the body
/// walk skips them on second encounter — without that, a body-position
/// walk of `Sdk.targets` would register the file before the bottom
/// splice, turning the splice into the skipped duplicate.
struct ExplicitSdkPromotion {
    sdk_paths: SdkPaths,
    props_range: Range<usize>,
    targets_range: Option<Range<usize>>,
}

/// When the project root has *no* `Sdk` attribute and its body opens
/// with `<Import Sdk="X" Project="Sdk.props"/>` *as the very first
/// element child*, hoist that import — and the matching trailing
/// `Sdk.targets` — to the OUTERMOST splice positions, just like the
/// root-Sdk shorthand. Without this, `walk_with_imports` would splice
/// `Directory.Build.props` before the body, and the body's
/// `Sdk.props` import would walk *after* Directory.Build.props — the
/// reverse of MSBuild's effective ordering, causing
/// `UndefinedProperty` diagnostics from a Directory.Build.props that
/// conditions on SDK-supplied names.
///
/// Canonical-position discipline (deliberately narrow, so promotion
/// never reorders user-visible work):
///   * `Sdk.props` must be the **first** element child of root. Any
///     preceding `<PropertyGroup>`, `<ItemGroup>`, etc. would in
///     MSBuild evaluate *before* the SDK chain; promoting silently
///     reverses that order.
///   * `Sdk.targets` (if promoted at all) must be the **last** element
///     child of root, and carry the same SDK name. Anything after it
///     would in MSBuild run *after* Sdk.targets; promotion reverses
///     that too. When something trails the import we leave it for the
///     body walk in-place.
///   * `<ImportGroup>` wrappers are not promotion candidates: the
///     root-Sdk shorthand expands to bare `<Import>` elements at root
///     position, so an `<ImportGroup>`-wrapped import is not
///     semantically equivalent.
///   * Conditional imports (any `Condition` attribute) are left to the
///     body walk — promoting would silently bypass the gate.
///   * If the SDK resolver is missing or returns `Err`, we return
///     `None` and let the body walk emit the diagnostic naturally at
///     its in-body position.
///
/// Emits no diagnostics: promotion is silent. Callers gate the splice
/// on the returned `Some(_)` and mark `state.hoisted_sdk_imports` for
/// the skip-on-body-walk contract.
fn find_explicit_sdk_promotion(
    root: Node<'_, '_>,
    state: &State<'_>,
) -> Option<ExplicitSdkPromotion> {
    if root.attribute("Sdk").is_some() {
        return None;
    }
    let resolver = state.sdk_resolver?;

    let mut element_children = root.children().filter(Node::is_element);
    let first = element_children.next()?;
    let (props_node, sdk_name) = canonical_sdk_import(first, "Sdk.props")?;
    // Promotion needs the canonical entry points; a locator-style
    // resolution has none, so it is not a promotion candidate (the body
    // walk handles the import naturally).
    let sdk_paths = match resolver(sdk_name).ok()? {
        SdkResolution::Single(paths) => paths,
        SdkResolution::Roots(_) => return None,
    };

    // `Sdk.targets` only promotes when it is the *last* element child
    // of root. Re-scan to find the last element; if it's different from
    // `props_node` and is a matching canonical `Sdk.targets`, promote
    // it. (A project with only `Sdk.props` and no `Sdk.targets` still
    // promotes the props import; targets stays unspliced.)
    let last = root.children().rfind(Node::is_element)?;
    let targets_range = if last.range() == props_node.range() {
        // Only one element child total — `Sdk.props` itself. No
        // `Sdk.targets` to promote.
        None
    } else {
        canonical_sdk_import(last, "Sdk.targets")
            .filter(|(_, name)| *name == sdk_name)
            .map(|(node, _)| node.range())
    };

    Some(ExplicitSdkPromotion {
        sdk_paths,
        props_range: props_node.range(),
        targets_range,
    })
}

/// Treat `node` as a canonical `<Import Sdk="X" Project="<expected_project>"/>`:
/// it must be an `<Import>` element with no `Condition` attribute, an
/// `Sdk` attribute, and a `Project` attribute matching
/// `expected_project`. Returns the node and the `Sdk` name on success.
fn canonical_sdk_import<'a, 'input>(
    node: Node<'a, 'input>,
    expected_project: &str,
) -> Option<(Node<'a, 'input>, &'a str)> {
    if node.tag_name().name() != "Import" {
        return None;
    }
    if node.attribute("Condition").is_some() {
        return None;
    }
    let sdk_name = node.attribute("Sdk")?;
    if node.attribute("Project") != Some(expected_project) {
        return None;
    }
    Some((node, sdk_name))
}

/// Map a resolver-side [`SdkResolveError`] to the matching
/// [`DiagnosticKind`]. Pulled out so the project-root and `<Import Sdk=>`
/// handlers stay consistent.
fn sdk_error_to_diagnostic(sdk_name: &str, err: SdkResolveError) -> DiagnosticKind {
    match err {
        SdkResolveError::NotFound => DiagnosticKind::SdkNotFound {
            name: sdk_name.to_string(),
        },
        SdkResolveError::VersionNotSatisfied { spec, available } => {
            DiagnosticKind::SdkVersionNotSatisfied {
                name: sdk_name.to_string(),
                spec,
                available,
            }
        }
        SdkResolveError::UnsupportedLayout { reason } => DiagnosticKind::SdkResolutionUnsupported {
            name: sdk_name.to_string(),
            reason,
        },
    }
}

/// How `resolve_directory_build_path` arrived at its returned path.
/// `Override` means the caller (or project body) set `Directory.Build.*Path`
/// explicitly — MSBuild preserves that value verbatim, so we must not
/// rewrite it. `Fallback` means we used the precomputed nearest-ancestor
/// file because no override was set — MSBuild's `Microsoft.Common.props`
/// assigns the matching `*Path` property to this file before importing,
/// so the caller seeds the lookup *if and only if* the import gate is open.
enum ResolutionSource {
    Override,
    Fallback,
}

struct Resolution {
    path: PathBuf,
    source: ResolutionSource,
}

/// The entry `Directory.Build.props` import, deferred to fire at the
/// first body-reached nested `Sdk.props` (pass 2 of the two-pass walk).
/// Carries only the inputs [`walk_external_file`] cannot otherwise reach:
/// the precomputed nearest-ancestor fallback (`walk_once`'s
/// `implicit_props`, or `None` when there is none). The actual import
/// decision — the `ImportDirectoryBuildProps` gate, the
/// `DirectoryBuildPropsPath` override, and the resulting path — is
/// re-resolved against *live* state at the fire point, because MSBuild
/// evaluates them there (after the body and the nested `Sdk.props` run),
/// not at the before-body position.
struct DeferredDirectoryBuildProps {
    fallback: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirectoryBuildFile {
    Props,
    Targets,
}

/// Write `value` (the resolved file path, forward-slash normalised) to
/// `state.lookup` under `property_name`. Used to mirror MSBuild's
/// implicit `DirectoryBuild{Props,Targets}Path` assignment that happens
/// inside the gated import block in `Microsoft.Common.props`. Bypasses
/// the `protected` set on purpose: callers seed only when their own
/// gate check already decided to import, and `DirectoryBuildPropsPath` /
/// `DirectoryBuildTargetsPath` are not reserved names.
fn seed_directory_build_path(state: &mut State<'_>, property_name: &str, path: &Path) {
    let value = path.to_string_lossy().replace('\\', "/");
    // A path *we* discovered on disk, not project XML: it enters the escaped
    // domain through `insert_computed`, so a `%`, `;` or `(` in it is inert
    // rather than an escape, a list separator, or an expression delimiter.
    state.lookup.insert_computed(property_name, value);
}

/// Pick the file path to use for an implicit `Directory.Build.*`
/// import: the explicit `Directory.Build{Props,Targets}Path` override
/// (when set and pointing at an existing file) wins; otherwise the
/// precomputed nearest-ancestor `fallback`. Returns `None` when the
/// override is set but its file doesn't exist (MSBuild's
/// `Microsoft.Common.props` gates the import on `Exists('$(...)')`,
/// so a stale override silently skips rather than falling back to
/// the nearest sibling — that would be a worse divergence). Relative
/// override paths resolve against `project_dir`.
///
/// An *empty override supplied as a read-only global* also returns
/// `None`: MSBuild assigns `DirectoryBuild*Path` to the discovered
/// path only when the property is unset, and a global cannot be
/// written through, so the value stays "" and `Exists('')` is false —
/// the import is skipped with no fallback. An empty value that is
/// *not* a sticky global (unset, or written by the project body —
/// where the default-fill can take effect) keeps falling back to the
/// discovered file.
///
/// Pure: this never mutates `state`. The caller is responsible for
/// seeding `$(<override_property>)` to the resolved path when the
/// result is [`ResolutionSource::Fallback`] *and* the import gate
/// passes — MSBuild's assignment of `DirectoryBuild*Path` sits inside
/// the gated import block in `Microsoft.Common.props`, so doing it
/// here would leak a phantom path when the gate is closed.
fn resolve_directory_build_path(
    state: &State<'_>,
    override_property: &str,
    fallback: Option<&Path>,
    project_dir: &Path,
) -> Option<Resolution> {
    // An override that names a file on disk: a point of use. Padding is trimmed
    // *in the domain*, before decoding — an escaped `%20` is part of the
    // filename, not padding (see `Escaped::trimmed_unescaped`).
    if let Some(trimmed) = state
        .lookup
        .get(override_property)
        .map(|v| v.trimmed_unescaped())
    {
        if !trimmed.is_empty() {
            // MSBuild accepts both `\` and `/` on either platform; explicit
            // `<Import>` resolution normalises to `/` before joining, and
            // overrides must follow the same rule or `alt\Custom.targets`
            // would probe a literal-backslash filename on Unix and silently
            // skip the import.
            let normalised = trimmed.replace('\\', "/");
            let candidate = Path::new(&normalised);
            let resolved = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                project_dir.join(candidate)
            };
            return resolved.exists().then_some(Resolution {
                path: resolved,
                source: ResolutionSource::Override,
            });
        }
        // Empty value. A read-only global stays empty (the
        // `Microsoft.Common.props` discovery assigns this property only
        // when unset, and cannot write through a global), so
        // `Exists('')` is false and the import is skipped — no
        // fallback. A non-global empty (unset / body-written) falls
        // through to the discovered file below.
        if state.is_sticky_global(override_property) {
            return None;
        }
    }
    Some(Resolution {
        path: fallback?.to_path_buf(),
        source: ResolutionSource::Fallback,
    })
}

/// Reproduce MSBuild's `Microsoft.Common.props` gate for
/// `Directory.Build.*`: the property defaults to "true" when empty
/// or unset, and the actual import only fires when the value (case-
/// insensitively) equals "true". Anything else — `"false"`, `"0"`,
/// `"no"`, a typo — skips the import. Treating "anything except
/// false" as opt-in (our earlier rule) was too permissive and would
/// import where MSBuild wouldn't.
///
/// `is_sticky_global` distinguishes a read-only global from an
/// unset/body-written value. MSBuild's default-fill
/// (`<Prop Condition="'$(Prop)' == ''">true</Prop>`) cannot write
/// through a global, so an *empty global* stays empty — the gate
/// `'$(Prop)' == 'true'` is then false and the import is skipped.
/// Only the empty case differs: a non-empty global ("true", "0", …)
/// compares identically either way.
fn should_import_default_true(value: Option<&str>, is_sticky_global: bool) -> bool {
    match value {
        // Read-only global: no default-fill, so the value is taken
        // verbatim and only the literal "true" opens the gate.
        Some(s) if is_sticky_global => s.eq_ignore_ascii_case("true"),
        None => true,
        Some(s) if s.trim().is_empty() => true,
        Some(s) => s.eq_ignore_ascii_case("true"),
    }
}

/// `TreatAsLocalProperty="Foo;Bar"` on `<Project>` is MSBuild's escape
/// hatch: names listed here can be locally reassigned even when
/// supplied as global properties. Without honouring it we'd silently
/// drop the project's writes and keep substituting the caller's value
/// — a real-world divergence (the F# SDK uses this attribute for
/// `RestoreAdditionalProjectSources`). Names are stored lowercased to
/// match MSBuild's OrdinalIgnoreCase property-name comparison.
fn collect_local_overrides(root: Node<'_, '_>) -> HashSet<String> {
    root.attribute("TreatAsLocalProperty")
        .map(|s| {
            s.split(';')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default()
}

/// Walk every element child of `root` as a top-level construct.
/// Shared by the project body and every imported file — the only
/// per-file state is `current_file_dir`, which becomes the base for
/// relative `<Import Project="...">` resolution.
///
/// The `Sdk` attribute on the project root is handled by the callers
/// ([`walk`] and [`walk_with_imports`]), not here — those know
/// whether IO and an SDK resolver are available. `walk_doc_body` is
/// also invoked on external files via [`walk_external_file`], and SDK
/// props/targets stubs aren't expected to carry an `Sdk` attribute of
/// their own.
fn walk_doc_body(root: Node<'_, '_>, current_file_dir: &Path, state: &mut State<'_>) {
    // A top-level `<Sdk Name="X"/>` element is the `Sdk` attribute's
    // sibling form: MSBuild imports X's `Sdk.props` before everything in
    // this file and `Sdk.targets` after it, wherever the element sits
    // (probed on dotnet msbuild 10.0.301: `$(UsingMicrosoftNETSdk)` reads
    // `true` even when the element follows the read). The element form is
    // not modelled, so degrade — as a pre-scan, before walking any child,
    // or a name the un-imported SDK chain defines would be read as
    // exactly-undefined by properties earlier in the document.
    for child in root.children().filter(Node::is_element) {
        if child.tag_name().name() == "Sdk" {
            let sdk = child.attribute("Name").unwrap_or_default().to_string();
            state.push(
                DiagnosticKind::UnsupportedConstruct {
                    element: format!("Sdk (Name={sdk:?})"),
                },
                child.range(),
            );
            state.mark_structural_skip_respecting_sdk_compile_tolerance(
                StructuralCompileItemUncertainty::ProjectSdkUnsupported { sdk },
                child.range(),
            );
        }
    }
    for child in root.children().filter(Node::is_element) {
        walk_top_level(child, current_file_dir, state);
    }
}

/// Try [`std::fs::canonicalize`]; on failure (file doesn't exist yet,
/// permission error, etc.) fall back to lexical `.`/`..` collapse.
/// We never compare these paths to anything other than other
/// canonicalised paths.
fn canonicalise_or_normalise(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalise(path))
}

/// First tier of the import-dedup identity, modelling MSBuild's
/// `Evaluator._importsSeen` key: the **lexically** normalised path —
/// `.`/`..` collapsed as strings, no symlink resolution (probed on dotnet
/// msbuild 10.0.301: a symlink alias of an already-imported file imports
/// *again*, `sub/../a.props` dedups against `a.props`) — **ASCII**
/// case-folded. MSBuild compares with `StringComparer.OrdinalIgnoreCase`
/// (Evaluator.cs; probed: `A.PROPS` dedups against `a.props`, string-level,
/// before any file IO), and ASCII-fold equality implies ordinal-ignore-case
/// equality on every .NET, so a hit on this key is a *certain* duplicate.
///
/// Non-ASCII case pairs are deliberately **not** folded here: .NET's
/// ordinal casing table is neither Unicode's simple nor full uppercase
/// (probed on dotnet 10: `ı`≠`I`, `ſ`≠`S`, `İ`≠`I`, `ß`≠`ẞ`, Kelvin
/// `K`≠`K` — yet `σ`==`ς`==`Σ`), so committing either verdict from a Rust
/// fold would be a wrong commit in one direction or the other. Pairs that
/// only a Unicode fold would equate land in the second tier
/// ([`import_dedup_fuzzy_key`]) and *decline* instead of guessing.
fn import_dedup_key(path: &Path) -> String {
    normalise(path).to_string_lossy().to_ascii_uppercase()
}

/// Second tier: the char-wise full Unicode uppercase of the same
/// normalised path. This fold **over-approximates** .NET's
/// ordinal-ignore-case equality: .NET-equal pairs are equal under
/// Unicode's simple uppercase (.NET's table is simple-uppercase minus
/// carve-outs like the Turkish-I family), simple-equal pairs of
/// non-expanding chars are full-fold-equal, and expanding chars (`ß`,
/// the ligatures) are .NET-equal only to themselves — which any fold
/// preserves. So: **not** fuzzy-equal ⟹ .NET treats the paths as
/// distinct imports (safe to walk both), while fuzzy-equal without a
/// first-tier hit means MSBuild's verdict is unknowable from here — the
/// caller declines rather than committing a skip *or* a second walk.
fn import_dedup_fuzzy_key(path: &Path) -> String {
    normalise(path)
        .to_string_lossy()
        .chars()
        .flat_map(char::to_uppercase)
        .collect()
}

struct State<'r> {
    /// Full lookup map: well-known seeds, caller globals, and any
    /// project-defined properties added during the walk. Used for every
    /// `$(Name)` substitution. Keyed case-insensitively, matching
    /// MSBuild's OrdinalIgnoreCase property-name comparison.
    lookup: PropertyMap,
    /// Lowercased names whose project-side writes must be discarded —
    /// `properties::well_known` seeds (always) plus the caller's
    /// `extra_properties` (unless `TreatAsLocalProperty` opts them out).
    /// Writes targeting these names — under any casing — are silently
    /// ignored, matching MSBuild's "global properties override project
    /// properties" / "reserved properties are read-only" rules.
    protected: HashSet<String>,
    /// Lowercased names of MSBuild reserved well-known properties
    /// (everything seeded by [`properties::well_known`]). Snapshot
    /// taken at construction time. Used by [`walk_external_file`] to
    /// distinguish "protected because reserved" from "protected
    /// because supplied as a global property" when an imported
    /// `<Project TreatAsLocalProperty="...">` asks to unprotect a
    /// name: MSBuild's `TreatAsLocalProperty` only applies to
    /// globals, not reserved names, so we never unprotect anything
    /// in this set even if an import lists it.
    reserved: HashSet<String>,
    /// Lowercased names supplied as *read-only globals*
    /// (`extra_properties`, minus the entry project's
    /// `TreatAsLocalProperty` opt-outs). Immutable for the whole walk:
    /// unlike `protected`, it is never touched by `walk_external_file`'s
    /// per-file `TreatAsLocalProperty` handling, so it is a stable
    /// answer to "is this property a sticky global?". A global's value
    /// cannot be rewritten by MSBuild's `Microsoft.Common.props`
    /// default-fill assignments, so an *empty* sticky global stays
    /// empty rather than defaulting to "true" / the discovered path.
    /// Consulted by the `Directory.Build.*` import gates
    /// ([`should_import_default_true`], [`resolve_directory_build_path`]).
    sticky_globals: HashSet<String>,
    /// What the caller's environment said about `MSBuildExtensionsPath`, held
    /// aside rather than promoted: whether that value survives is the one
    /// toolset behaviour that differs across the SDKs we support, so it cannot
    /// be settled until an SDK resolves and names the toolset. See
    /// [`EnvExtensionsPath`] and [`State::seed_toolset_properties`].
    env_extensions_path: EnvExtensionsPath,
    /// Map from lowercased name to the *project's* canonical casing
    /// for every property successfully assigned by a `<PropertyGroup>`
    /// write. Used to filter and key [`ParsedProject::properties`] —
    /// the result records *only what the project wrote*, under the
    /// spelling that appeared in the XML. Extras that were
    /// treated-as-local but never reassigned by the project must not
    /// surface here (their value remains in `lookup` for substitution
    /// but never enters this map); and when a project does override an
    /// extras-supplied name with different casing, the project's
    /// casing wins for the exported map (the extras casing would
    /// surprise callers doing exact `properties.get("Configuration")`
    /// lookups against what they read in the XML).
    written: HashMap<String, String>,
    compile_first: Vec<OrderedResolvedItem>,
    explicit_compile_before: Vec<OrderedResolvedItem>,
    compile_before: Vec<OrderedResolvedItem>,
    compile_main: Vec<OrderedResolvedItem>,
    compile_after: Vec<OrderedResolvedItem>,
    explicit_compile_after: Vec<OrderedResolvedItem>,
    compile_last: Vec<OrderedResolvedItem>,
    /// Compile items whose current `CompileOrder` metadata is not one of the
    /// values the F# target buckets. Kept internally so a later literal
    /// `<Compile Update=... CompileOrder=...>` can still mutate the existing
    /// item back into a bucket that is emitted.
    compile_excluded: Vec<OrderedResolvedItem>,
    next_item_order: usize,
    /// `<ProjectReference>` items. Separate bucket from the Compile
    /// trio because ProjectReference is not a Compile input — it
    /// describes an inter-project dependency the downstream binder
    /// must resolve, and mixing it into [`ParsedProject::items`] would
    /// silently change every consumer's notion of "files to compile".
    project_references: Vec<OrderedResolvedItem>,
    /// `<PackageReference>` items as captured during the item pass, before
    /// Include + `Update` collapse — [`item_pass::finalize_package_references`]
    /// folds these into [`Self::package_references`] once every item is seen.
    captured_package_references: Vec<CapturedPackageReference>,
    /// `<PackageReference>` / `<FrameworkReference>` items — the NuGet
    /// dependency set. Separate buckets from Compile/ProjectReference: a
    /// package reference is neither a source input nor an inter-project
    /// edge, and it carries version/asset metadata the others don't. The
    /// effective (post-merge) `PackageReference` set; populated by the item
    /// pass's `finalize_package_references`.
    package_references: Vec<PackageReference>,
    package_versions: Vec<PackageVersion>,
    package_versions_untracked: bool,
    global_package_references: Vec<GlobalPackageReference>,
    framework_references: Vec<FrameworkReference>,
    /// Static item lists whose item type is not otherwise modelled by this
    /// evaluator. This intentionally starts narrow: package/framework reference
    /// capture can consume safe lists like `@(MIBCPackage)` without changing
    /// Compile item semantics or surfacing arbitrary item diagnostics.
    evaluated_items: HashMap<String, Vec<EvaluatedItem>>,
    /// Item types whose static declarations could not be fully evaluated.
    /// Consumers that reference one of these lists should keep whatever clean
    /// entries were captured, but mark their own result uncertain.
    tainted_item_lists: HashSet<String>,
    /// Item types whose live MSBuild item table can contain entries we did not
    /// copy into `evaluated_items`. A later exact `@(Type)` reference must not
    /// treat a missing captured list as confidently empty.
    untracked_item_lists: HashSet<String>,
    /// Package-metadata defaults declared by `<ItemDefinitionGroup>` for
    /// helper item types. These do not change helper identities, but they can
    /// supply metadata to helper items that a later `<PackageReference
    /// Include="@(Helper)">` inherits.
    helper_item_definition_defaults: HashMap<String, HashMap<String, HelperMetadataUncertainty>>,
    diagnostics: Vec<Diagnostic>,
    /// Whether `<Import>` elements should be followed by reading the
    /// referenced file (`true`, set by [`walk_with_imports`]) or just
    /// reported as [`DiagnosticKind::UnresolvedImport`] (`false`, set by
    /// the pure [`walk`]). The flag lives on `State` rather than being
    /// threaded everywhere because every `<Import>` site needs it and
    /// the alternative is a parallel parameter list.
    follow_imports: bool,
    /// Our model of MSBuild's `Evaluator._importsSeen`: the
    /// [`import_dedup_key`] of every import this walk has performed, the
    /// entry project included (seeded in [`walk_once`]). MSBuild registers
    /// an import *before* evaluating the imported file's contents and
    /// skips any later import that resolves to a registered path —
    /// warning MSB4011, or MSB4210 when the target is the entry project —
    /// with the evaluation succeeding, so a duplicate here (a repeated
    /// list segment, a diamond, a cycle's back-edge) is a clean silent
    /// skip, not a degrade. Entries are never removed: the set is
    /// per-evaluation, exactly like MSBuild's. Termination for the
    /// pathological spelling-generating cases the string key cannot close
    /// (a directory symlink to `.` growing a fresh spelling per level)
    /// falls to [`MAX_IMPORT_DEPTH`], which degrades conservatively.
    imports_seen: HashSet<String>,
    /// Shadow of [`Self::imports_seen`] under the wider
    /// [`import_dedup_fuzzy_key`] fold. A hit here without a hit on the
    /// certain key means the import *might* be a duplicate under .NET's
    /// ordinal casing (whose non-ASCII table we cannot reproduce
    /// exactly), so the walk declines it — structural skip, not a
    /// silent skip and not a second walk.
    imports_seen_fuzzy: HashSet<String>,
    /// Every file the walk has *ever* entered (canonical), entry project
    /// included. Unlike [`Self::imports_seen`] — whose key is the
    /// *lexically* normalised spelling, because that is what MSBuild
    /// dedups on — this records symlink-resolved identity, so it can
    /// answer "was this file part of the evaluation?" after the walk
    /// completes (the `Directory.Packages.props` discharge check reads
    /// it).
    walked_files: HashSet<PathBuf>,
    /// The nearest-ancestor `Directory.Build.props` detected by the
    /// caller, stashed so [`fire_entry_directory_build_props_splice`]
    /// can reach it from whichever position consumes the entry splice
    /// (the real `Microsoft.Common.props` import point, or the
    /// after-`Sdk.props` fallback).
    implicit_directory_build_props_fallback: Option<PathBuf>,
    /// The canonical file successfully walked through an
    /// `<Import Project="$(DirectoryPackagesPropsPath)">`-shaped import —
    /// NuGet's central-package import point (only `NuGet.props` uses that
    /// shape in practice, and a user file spelling it out behaves
    /// identically: the walked file is exactly what a real build's
    /// `NuGet.props` would import for the same property value).
    /// Provenance, not final property state, so it cannot be spoofed by
    /// writing marker/path properties — the discharge check compares it
    /// against the *final* `DirectoryPackagesPropsPath` value to also
    /// catch a post-import reassignment.
    walked_directory_packages_props_import: Option<PathBuf>,
    /// Current import nesting depth. `0` while we're walking the
    /// entry project's body; `+1` for each [`walk_external_file`]
    /// frame on the stack. Compared against [`MAX_IMPORT_DEPTH`] as a
    /// stack-overflow safeguard against hostile input.
    depth: usize,
    /// Directory of the entry project file. Pinned for the whole walk:
    /// MSBuild resolves an unqualified `<Compile Include="Generated.fs"
    /// />` *appearing in an imported file* relative to the entry
    /// project's directory, not the importing file's directory. (The
    /// `$(MSBuildThisFileDirectory)` prefix is how an import opts in
    /// to its own folder.) Distinct from the per-call
    /// `current_file_dir`, which is what relative `<Import Project="
    /// ...">` paths resolve against.
    entry_project_dir: PathBuf,
    /// `Some(range)` while walking an imported file; `None` while
    /// walking the entry project's body. The range is the span of the
    /// top-level `<Import>` element *in the entry project* that
    /// ultimately led to the current file. Diagnostics and items
    /// produced inside an imported buffer would otherwise carry
    /// `node.range()` byte offsets *into that buffer*, which are
    /// meaningless to callers who only ever see the entry project's
    /// source. We collapse all imported-file spans to this single
    /// project-source location so the byte-offset contract holds:
    /// every span in the output is a valid offset into the source
    /// the caller handed in. The first descent into an external file
    /// sets it; nested descents preserve it (so spans always point at
    /// the *top-level* import site, not the most-recent one); unwind
    /// restores the prior value.
    import_site_span: Option<Range<usize>>,
    /// Caller-supplied SDK resolver, if any. Consulted by
    /// [`resolve_project_sdk`] for the project root's `Sdk` attribute
    /// and by [`follow_explicit_import`] for `<Import Sdk="..."/>`.
    /// `None` means the caller declined to do SDK resolution; any
    /// `Sdk` attribute then becomes an `UnsupportedConstruct`.
    sdk_resolver: Option<&'r SdkResolver<'r>>,
    /// Caller-supplied glob resolver, if any. Consulted by the item
    /// walk for a `<Compile>` / `<ProjectReference>` whose `Include`
    /// contains an MSBuild wildcard, or that carries an `Exclude`.
    /// `None` means the caller declined glob expansion; a wildcard then
    /// becomes [`DiagnosticKind::UnsupportedGlob`] and an `Exclude`
    /// [`DiagnosticKind::UnsupportedItemOperation`]. See
    /// [`crate::GlobResolver`].
    glob_resolver: Option<&'r GlobResolver<'r>>,
    /// Byte ranges of *entry-project* body `<Import Sdk="X"
    /// Project="Sdk.{props,targets}"/>` nodes that the pre-scan in
    /// [`find_explicit_sdk_promotion`] has hoisted to the root SDK
    /// splice positions. When the entry project's body walk later
    /// reaches one of these nodes, [`follow_explicit_import`] must
    /// skip it — for `Sdk.props` the walk would be a harmless
    /// duplicate skip (the top splice already registered the file),
    /// but `Sdk.targets` splices at the *bottom*, so walking it at
    /// body position would register it first and make the splice the
    /// skipped duplicate. Empty unless we're in the explicit-only
    /// promotion case.
    ///
    /// The ranges are byte offsets into the *entry project's* XML.
    /// [`follow_explicit_import`] gates the skip on
    /// `import_site_span.is_none()` (i.e. "we're inside the entry
    /// project body, not an imported file") so an `<Import>` at the
    /// same byte offset inside `Directory.Build.props` or `Sdk.props`
    /// is never accidentally matched.
    hoisted_sdk_imports: HashSet<Range<usize>>,
    /// Canonical path of the entry project's `Directory.Build.props` import
    /// point owned by this walker. SDK props can contain the standard
    /// rediscovery import before the explicit splice has fired; when that
    /// import points at this same path, following it would double-walk user
    /// props.
    directory_build_props_splice_path: Option<PathBuf>,
    /// Canonical path of the entry project's `Directory.Build.targets` import
    /// point already consumed by the explicit after-body splice. SDK targets
    /// also contain the standard rediscovery import for this property; when it
    /// points at this same path, following it would double-walk user targets.
    directory_build_targets_splice_path: Option<PathBuf>,
    /// Set while walking the explicit Directory.Build import point and its
    /// transitive imports. If that import rewrites `DirectoryBuild*Path`, the
    /// later SDK rediscovery line must still be suppressed: MSBuild has only
    /// one such import point, and rewrites inside the imported file do not
    /// create a second one.
    active_directory_build_splice: Option<DirectoryBuildFile>,
    /// True before the explicit Directory.Build.props import point has been
    /// consumed or skipped. While this is true, an SDK props file's standard
    /// rediscovery import should be suppressed if it points at the same live
    /// path that the explicit splice is about to handle.
    directory_build_props_splice_pending: bool,
    directory_build_props_path_written_by_splice: bool,
    directory_build_targets_path_written_by_splice: bool,
    /// The entry project's `Directory.Build.props` import, deferred from
    /// its usual before-body position. `Some` only on the orchestrator's
    /// second pass (`defer_directory_build_props = true`) and only
    /// between the before-body point and the first body-reached nested
    /// `Sdk.props` that fires it. [`walk_external_file`] `take()`s it at
    /// that first nested `Sdk.props` (so the import is attempted at most
    /// once) and re-resolves the gate/override/path against live state
    /// there. If it is still `Some` at the end of the walk the deferred
    /// import never fired — the import leading to the nested SDK was
    /// itself gated on a `Directory.Build.props`-set property, so deferral
    /// suppressed the nested SDK too — and the orchestrator falls back to
    /// the before-body splice (pass 1). Not part of [`ParsedProject`].
    pending_directory_build_props: Option<DeferredDirectoryBuildProps>,
    /// `true` only while walking the *entry project's own body* (not its
    /// before/after-body `Directory.Build.*` / `Sdk.*` splices, and not
    /// any imported file's body). Gates the nested-SDK deferral logic in
    /// [`walk_external_file`]: only a nested `Sdk.props` reached *through
    /// the entry body* is the "first `Sdk.props`" MSBuild would run, so
    /// only such a one should fire the deferred `Directory.Build.props`.
    /// A nested SDK reached while
    /// walking an already-spliced `Directory.Build.props` or
    /// `Directory.Build.targets` must not reposition the entry
    /// `Directory.Build.props`. Not part of [`ParsedProject`].
    in_entry_body: bool,
    /// The [`SdkPaths::root`] of the entry project's own (root / promoted)
    /// SDK, recorded by [`Self::record_sdk_root`]; `None` for an SDK-less
    /// entry. Surfaced on [`ParsedProject::resolved_sdk_root`] by
    /// [`Self::into_project`].
    resolved_sdk_root: Option<PathBuf>,
    /// `true` while the walk is inside a context whose uncertainty would
    /// affect the **Compile item set**: a source-set-changing
    /// `<Compile>`/`<CompileBefore>`/`<CompileAfter>` element, or an
    /// `<ItemGroup>` that has such a child (metadata-only `Update` is ignored
    /// unless it writes `CompileOrder`). Any diagnostic
    /// [`Self::push`]ed while this is set flips [`Self::items_uncertain`], and
    /// a Compile-gating condition recorded here becomes a
    /// [`CompileConditionUncertainty`]. Scoped save/restore by the callers, so
    /// it tracks the *position* in the walk rather than a kind.
    compile_context: bool,
    /// The package-set analogue of [`Self::compile_context`]: `true` while
    /// positioned at a `<PackageReference>`/`<FrameworkReference>` element,
    /// or an `<ItemGroup>` that has such a child. A condition here that is
    /// unsupported or undefined-property-laden flips
    /// [`Self::package_references_uncertain`] — we can't trust whether the
    /// gated package is in the set. Scoped save/restore by the callers.
    package_context: bool,
    /// `true` while walking a file that physically lives inside the entry
    /// SDK's installation tree (see [`Self::sdk_tolerance_root`]). The .NET
    /// SDK's own targets/props are full of conditional Compile machinery
    /// (`<ItemGroup Condition="'$(EnableDefaultItems)' == 'true'">…`, the
    /// link-metadata `<Compile Update=…>` group) gated on properties we can't
    /// resolve; treating those as Compile-affecting would flag essentially
    /// every real project. So Compile-affecting uncertainty inside the SDK tree
    /// is *tolerated* (it never decides which hand-written files compile),
    /// while the same uncertainty in the entry project or a user-authored
    /// import (`Directory.Build.*`, an explicit `<Import>`) is respected. Path-
    /// based, so it's independent of *how* a file was reached — a user file
    /// pulled in through the SDK's import chain is still judged by its path.
    in_sdk_subtree: bool,
    /// Directories under which a file counts as SDK-installed — one per resolved
    /// SDK *root's parent* (e.g. `…/Sdks/Microsoft.NET.Sdk`, which holds both
    /// `Sdk/Sdk.props` and `targets/…DefaultItems…`), canonicalised. A *set*
    /// because an SDK variant pulls in others: `Microsoft.NET.Sdk.Web`'s
    /// `Sdk.props` does `<Import Sdk="Microsoft.NET.Sdk">`, whose files live
    /// under a *sibling* dir — both must be tolerated. Populated by
    /// [`Self::note_sdk_tolerance`] at every successful SDK resolution (entry,
    /// nested, and `<Import Sdk=…>`); empty for an SDK-less entry (then
    /// [`Self::in_sdk_subtree`] is always `false`).
    sdk_tolerance_roots: Vec<PathBuf>,
    /// Accumulates into [`ParsedProject::items_uncertain`].
    items_uncertain: bool,
    /// Whether the captured `<ProjectReference>` list may diverge from
    /// MSBuild (an unmodelled `Update`/`Remove` mutation). Accumulates into
    /// [`ParsedProject::project_references_uncertain`].
    project_references_uncertain: bool,
    /// Whether the captured package/framework-reference set may diverge from
    /// MSBuild. Accumulates into [`ParsedProject::package_references_uncertain`].
    package_references_uncertain: bool,
    /// Accumulates into [`ParsedProject::package_reference_uncertainties`].
    package_reference_uncertainties: Vec<PackageReferenceUncertaintyCause>,
    /// Lowercased property names whose current value came from the SDK property
    /// pass, or from an SDK property write evaluated with an untrusted condition
    /// or value in this single-pass walker. The taint is package-specific:
    /// Compile uncertainty deliberately tolerates SDK property machinery, but a
    /// later `<PackageReference Version="$(Name)">` consuming such a value must
    /// not be reported as trustworthy because MSBuild evaluates project
    /// properties before project items. Mutated only via
    /// [`Self::apply_property_provenance`]; see [`PropertyProvenance`] for how
    /// this channel relates to [`Self::unpinned_value_properties`].
    sdk_package_tainted_properties: HashMap<String, SdkPackagePropertyTaint>,
    /// The preprocessor-symbol analogue of [`Self::compile_context`]: `true`
    /// while resolving a user-authored `<DefineConstants>` write or the
    /// `<PropertyGroup>` condition gating one. A diagnostic [`Self::push`]ed
    /// here means the evaluated `$(DefineConstants)` may diverge from MSBuild,
    /// so files would parse under the wrong `#if` branches. SDK-internal define
    /// manipulation is tolerated (gated `!in_sdk_subtree` at the set sites, like
    /// `compile_context`) — we already don't model the framework defines the
    /// SDK adds in targets.
    define_context: bool,
    /// `true` only while expanding a `<DefineConstants>` *value* (not its
    /// condition). The `$(DefineConstants)` self-reference exemption
    /// ([`is_define_self_reference`]) applies only here — the append idiom is a
    /// value construct; a *condition* referencing `$(DefineConstants)` is a
    /// genuine branch decision that must still flag.
    in_define_value: bool,
    /// Accumulates into [`ParsedProject::define_constants_uncertain`].
    define_constants_uncertain: bool,
    /// `true` while evaluating an `<Import>` / `<ImportGroup>` `Condition` in a
    /// user-authored file (gated `!in_sdk_subtree` at the set sites). An import
    /// skipped by a condition we couldn't trust (unsupported, or relying on an
    /// undefined property) might have contributed `<Compile>` items or gating
    /// properties, so any diagnostic [`Self::push`]ed here flips
    /// [`Self::items_uncertain`]. SDK chains condition imports constantly, hence
    /// the provenance gate.
    import_gate_context: bool,
    /// Package-reference analogue of [`Self::import_gate_context`], but **not**
    /// SDK-gated: SDK imports are a normal source of implicit package/framework
    /// references, so an import skipped by an untrusted condition can hide
    /// dependency items even when Compile uncertainty remains tolerated.
    package_import_gate_context: bool,
    /// Accumulates into [`ParsedProject::compile_condition_uncertainties`].
    compile_condition_uncertainties: Vec<CompileConditionUncertainty>,
    /// Accumulates into [`ParsedProject::compile_item_uncertainties`].
    compile_item_uncertainties: Vec<CompileItemUncertaintyCause>,
    /// The file the property pass is currently positioned in — the target
    /// of [`Self::defer_item_group`]'s retention. Saved/replaced/restored
    /// by [`walk_external_file`] alongside the `MSBuildThisFile` frame.
    current_file: CurrentFile,
    /// `<ItemGroup>`s recorded by the property pass, in encounter order,
    /// awaiting the item pass ([`replay_deferred_item_groups`]). Empty
    /// again by [`Self::into_project`].
    deferred_item_groups: Vec<DeferredItemGroup>,
    /// Sources of imported files that own at least one deferred group,
    /// kept so the item pass can re-parse them. Indexed by
    /// [`CurrentFile::Imported::retained`].
    retained_imported_files: Vec<RetainedImportedFile>,
    /// Properties whose stored value the property pass could not pin down —
    /// keyed by lowercase property name, valued by the root cause
    /// ([`UnpinnedRoot`]): the value chain substituted an undefined
    /// reference, read another unpinned property, or a write in the chain
    /// sat behind a gate we couldn't evaluate. The stored value is our best
    /// evaluation, but a real build can diverge, so every read — a `$(…)`
    /// expansion ([`State::expand`]) or a condition
    /// ([`evaluate_condition_inner`]) — re-surfaces the root as a
    /// diagnostic under the active contexts, degrading compile/package
    /// certainty exactly like a direct undefined reference. A later clean
    /// overwrite re-pins the property. Mutated only via
    /// [`Self::apply_property_provenance`]; see [`PropertyProvenance`] for how
    /// this channel relates to [`Self::sdk_package_tainted_properties`].
    unpinned_value_properties: HashMap<String, UnpinnedRoot>,
    /// Lowercased referenceable names present in the caller's environment
    /// snapshot — *including* names promotion skipped (case collisions,
    /// toolset overwrites): the real build has *some* value for every one
    /// of them, so an undefined read of such a name is never exact
    /// (see [`State::undefined_read_is_exact`]).
    env_property_names: HashSet<String>,
    /// Whether the walk has passed a construct that could hide arbitrary
    /// property writes: an unfollowed/unresolved/failed import or SDK, or
    /// an import whose gate we could not decide. Once set it never
    /// clears — every later undefined read could be of a name the hidden
    /// content defined, so none is exact. Reads *before* the opaque
    /// construct are unaffected (property evaluation is a single forward
    /// pass on both sides), which the "so far" reading gives for free;
    /// the item pass runs after the whole property pass, so it correctly
    /// sees the final flag.
    walk_opaque: bool,
    /// Lowercased names whose write was *refused* — the value carried an
    /// expression, item reference, or metadata reference we can't
    /// evaluate, so the binding was removed rather than stored. The real
    /// build stores that value, so a later undefined read of the name is
    /// not exact.
    unevaluable_written: HashSet<String>,
}

#[derive(Debug, Clone)]
struct EvaluatedItem {
    identity: String,
    metadata: HashMap<String, String>,
    metadata_uncertainties: Vec<HelperMetadataUncertainty>,
}

impl EvaluatedItem {
    fn metadata(&self, name: &str) -> Option<String> {
        self.metadata.get(&name.to_ascii_lowercase()).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum HelperMetadataUncertaintyKind {
    UnevaluableValue,
    ItemDefinitionDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HelperMetadataUncertainty {
    name: String,
    value: String,
    kind: HelperMetadataUncertaintyKind,
}

impl HelperMetadataUncertainty {
    fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.to_string(),
            value: value.to_string(),
            kind: HelperMetadataUncertaintyKind::UnevaluableValue,
        }
    }

    fn item_definition_default(name: &str) -> Self {
        Self {
            name: name.to_string(),
            value: String::new(),
            kind: HelperMetadataUncertaintyKind::ItemDefinitionDefault,
        }
    }
}

/// Why a stored property value cannot be trusted as final (see
/// [`State::unpinned_value_properties`]): the divergence a real build could
/// introduce, re-surfaced as a diagnostic at every read of the value.
#[derive(Clone, Debug, PartialEq, Eq)]
enum UnpinnedRoot {
    /// The value chain substituted `$(Name)` while `Name` was undefined —
    /// a real build may supply it (environment variables are MSBuild
    /// initial properties) and produce a different value.
    Undefined(String),
    /// A write in the value chain was gated on a condition outside the
    /// supported grammar, so we cannot know whether it ran.
    UnsupportedCondition(String),
}

impl UnpinnedRoot {
    fn to_diagnostic(&self) -> DiagnosticKind {
        match self {
            UnpinnedRoot::Undefined(name) => {
                DiagnosticKind::UndefinedProperty { name: name.clone() }
            }
            UnpinnedRoot::UnsupportedCondition(condition) => DiagnosticKind::UnsupportedCondition {
                condition: condition.clone(),
            },
        }
    }
}

/// Result of expanding `$(...)` in a raw attribute or element value. The
/// two issue flags are tracked separately because they have different
/// downstream consequences:
///   * `had_undefined` means a reference substituted to "" — the value is
///     still well-formed, just missing data. Safe to store as a property.
///   * `had_unsupported` means an expression like `$([X]::Y())` was left
///     literal in the output. Storing such a value would let later
///     `$(Name)` references silently expand to that residual, corrupting
///     downstream paths. Property writes refuse to record it.
struct Expansion {
    /// The expanded text, still in MSBuild's escaped domain — it is stored as a
    /// property without leaving it, exactly as MSBuild stores it, and each
    /// consumer unescapes at its own point of use.
    value: Escaped,
    had_undefined: bool,
    had_unsupported: bool,
    /// Why the produced value cannot be trusted as final, if it can't: a
    /// direct undefined `$(Name)` substitution, or a reference to a
    /// property that is itself unpinned. Recorded when the value is stored
    /// as a property ([`State::unpinned_value_properties`]).
    unpinned_root: Option<UnpinnedRoot>,
}

impl Expansion {
    fn had_issue(&self) -> bool {
        self.had_undefined || self.had_unsupported
    }
}

#[derive(Debug, Clone)]
struct SdkPackagePropertyTaint {
    span: Range<usize>,
    origin: DiagnosticOrigin,
}

/// What a single property write does to the SDK-package taint channel
/// ([`State::sdk_package_tainted_properties`]).
enum TaintOutcome {
    /// Mark the property tainted at `span` (a write we can't trust for a
    /// later package read).
    Set(Range<usize>),
    /// Clear any existing taint — a clean write re-pins the name.
    Clear,
    /// Leave the existing taint mark as-is.
    Keep,
}

/// What a single property write does to the unpinned channel
/// ([`State::unpinned_value_properties`]).
enum UnpinnedOutcome {
    /// Record `root` — every later read re-surfaces it as a diagnostic.
    Set(UnpinnedRoot),
    /// Clear any existing root — a clean write under a clean gate re-pins.
    Clear,
    /// Leave the existing root as-is.
    Keep,
}

/// The provenance verdict for a single property write: what happens to
/// **both** forward-uncertainty channels, applied through the one method
/// ([`State::apply_property_provenance`]) that mutates either map.
///
/// The two channels stay deliberately separate — this type pairs them at
/// the *decision* point, not the concept:
/// * [`State::unpinned_value_properties`] rides the diagnostic pipeline: a
///   read re-surfaces its root under the active context, so it can flip
///   [`State::items_uncertain`] and [`State::define_constants_uncertain`].
/// * [`State::sdk_package_tainted_properties`] is a silent marker checked
///   only at package/item sites and propagates through *clean* reads; it
///   deliberately **never** reaches [`State::items_uncertain`] (Compile
///   evaluation tolerates SDK property machinery).
///
/// Both union into [`ParsedProject::untrusted_properties`]
/// ([`State::property_provenance_untrusted`]). Because a write must name an
/// outcome for each channel, a new write path cannot update one map and
/// silently forget the other.
struct PropertyProvenance {
    taint: TaintOutcome,
    unpinned: UnpinnedOutcome,
}

impl TaintOutcome {
    /// The taint outcome of a write the property pass performed: taint it
    /// when the value/condition is untrusted, else clear unless a prior
    /// taint must be preserved (an earlier untrusted write to the same
    /// name whose divergence still stands).
    fn after_write(taints_property: bool, span: Range<usize>, preserve_existing: bool) -> Self {
        if taints_property {
            TaintOutcome::Set(span)
        } else if preserve_existing {
            TaintOutcome::Keep
        } else {
            TaintOutcome::Clear
        }
    }
}

impl UnpinnedOutcome {
    /// The unpinned outcome of a write the property pass performed:
    /// `unpinned_by` is the root cause when the new value (or the gate it
    /// sat behind) leans on one; a clean value under a clean gate re-pins,
    /// while a clean value under a still-uncertain gate leaves the prior
    /// state untouched.
    fn after_write(unpinned_by: Option<UnpinnedRoot>, write_condition_maybe_wrong: bool) -> Self {
        match unpinned_by {
            Some(root) => UnpinnedOutcome::Set(root),
            None if !write_condition_maybe_wrong => UnpinnedOutcome::Clear,
            None => UnpinnedOutcome::Keep,
        }
    }
}

impl<'r> State<'r> {
    fn new(
        project_path: &Path,
        extra_properties: &HashMap<String, String>,
        environment: &HashMap<String, String>,
        local_overrides: &HashSet<String>,
        follow_imports: bool,
        sdk_resolver: Option<&'r SdkResolver<'r>>,
        glob_resolver: Option<&'r GlobResolver<'r>>,
    ) -> Self {
        let mut lookup = properties::well_known(project_path);
        // Reserved well-known names are unconditionally protected;
        // `TreatAsLocalProperty` is documented to apply to *global*
        // properties only, so listing a reserved name there must not
        // unprotect it.
        let mut reserved: HashSet<String> = lookup
            .canonical_keys()
            .map(|k| k.to_ascii_lowercase())
            .collect();
        // Reserved from process start: MSBuild derives the ChangeWaves
        // threshold from the `MSBUILDDISABLEFEATURESFROMVERSION`
        // environment variable and rejects every project write to the
        // property name, so a write must never reach
        // `AreFeaturesEnabled`'s guard. Its *value* is seeded from the
        // caller's environment snapshot below — the one trusted source.
        reserved.insert("msbuilddisablefeaturesfromversion".to_string());
        let mut protected: HashSet<String> = reserved.clone();
        // The `OS` pseudo-environment property. MSBuild *synthesises* it only
        // on non-Windows hosts (dotnet/msbuild
        // `src/Build/Evaluation/Evaluator.cs`: `// Fake OS env variables when
        // not on Windows` / `if (!NativeMethodsShared.IsWindows)` →
        // `SetBuiltInProperty(osName, "Unix")`). On Windows there is no such
        // built-in at all: `OS=Windows_NT` reaches MSBuild as an ordinary
        // environment variable, so it must come from the *snapshot* — seeding
        // it unconditionally would invent `Windows_NT` for an env-cleared
        // Windows caller, where the real `$(OS)` is empty.
        //
        // Unlike the reserved well-known names above it is freely overridable:
        // a project `<OS>` write wins over the default, and a caller global
        // (the loop below runs later, so it overwrites) wins over both (pinned
        // against `dotnet msbuild` 10.0.301; see the "`OS` pseudo-environment
        // property" tests). Corpus projects gate TargetFrameworks on
        // `'$(OS)' == 'Unix'`, so leaving it unset mis-evaluates them on every
        // unix host.
        //
        // Seeded *before* the environment promotion below, mirroring MSBuild's
        // `AddBuiltInProperties()` → `AddEnvironmentProperties()` order: `OS`
        // is not reserved, so a genuine `OS` variable in the snapshot
        // overwrites this default rather than being skipped.
        if !cfg!(windows) {
            lookup.insert("OS".to_string(), "Unix".to_string());
        }
        // Environment promotion (pinned against dotnet msbuild
        // 10.0.300): env-backed properties are readable from the start,
        // lose to same-named globals (inserted after, overwriting), and
        // are overridable by project writes (NOT protected). Skipped:
        // every name MSBuild treats as reserved (see
        // `is_msbuild_reserved_name` — MSBuild filters the environment
        // against the *whole* reserved set in
        // `Utilities.GetEnvironmentProperties`, not just the subset we
        // seed, so a spoofed `MSBuildThisFileFullPath` must stay
        // unreadable), toolset-computed names MSBuild overwrites after
        // promotion (see `is_env_ignored_toolset_name`), names our
        // `$(…)` grammar cannot reference (nothing could read them),
        // and any name with a case-insensitive collision in the
        // snapshot — probed: with `ZZZTEST`/`zzztest`/`ZZZTest` all
        // set, the property's winner *changed* when the environ order
        // was reversed, so MSBuild's pick is unspecified and seeding
        // any of them could commit us to a value the real build
        // doesn't have. An unseeded name reads as undefined, which is
        // always conservative (diagnostic + unpinned value).
        let mut env_name_counts: HashMap<String, usize> = HashMap::new();
        for key in environment.keys() {
            *env_name_counts.entry(key.to_ascii_lowercase()).or_default() += 1;
        }
        let mut env_extensions_path = EnvExtensionsPath::Absent;
        let mut env_property_names: HashSet<String> = HashSet::new();
        for (key, value) in environment {
            if !properties::is_referenceable_name(key) {
                continue;
            }
            let lower = key.to_ascii_lowercase();
            // Every referenceable variable is remembered — promoted or
            // not — because the real build defines *some* property for
            // it, which the exact-undefined-read guard must respect.
            env_property_names.insert(lower.clone());

            // --- Names MSBuild does *not* fold in. Any default we seeded for
            // them is the real build's value too, so it stands. ---

            // Reserved names are filtered out of the environment by MSBuild
            // itself, so the path-derived value we seeded survives.
            if reserved.contains(&lower) || is_msbuild_reserved_name(&lower) {
                continue;
            }

            // MSBuild promotes this one, but whether the promoted value
            // *survives* is decided by the toolset, and no SDK has named the
            // toolset yet. Park it — including the case-collision verdict,
            // which a toolset that overwrites the value makes moot — and let
            // `seed_toolset_properties` adjudicate once it knows the version.
            // Until then the name stays undefined, so a read before any SDK
            // resolves declines rather than committing a version-specific
            // answer.
            if lower == "msbuildextensionspath" {
                env_extensions_path = if env_name_counts[&lower] > 1 {
                    EnvExtensionsPath::Unspecified
                } else {
                    EnvExtensionsPath::Value(value.clone())
                };
                lookup.remove(key);
                continue;
            }

            // --- From here MSBuild *does* fold the name in, and promotion
            // runs after the built-in defaults. So any default we seeded for
            // this name (in practice `OS`) has been overwritten in the real
            // build. Whenever we cannot model the value MSBuild ends up with,
            // it is not enough to skip the insert: the stale default would
            // still be read, and a read of a seeded property *commits*. Drop
            // it, so the read degrades to undefined instead. ---
            let unmodellable =
                // The winner of a case-collision is unspecified — probed: with
                // `OS=Windows_NT` and `os=lowercase` both set, `$(OS)` changed
                // value when the environ order was reversed.
                env_name_counts[&lower] > 1
                // MSBuild promotes it, then the toolset overwrites it with a
                // host fact this crate cannot know.
                || is_env_ignored_toolset_name(&lower);

            if unmodellable {
                lookup.remove(key);
                continue;
            }
            // An environment value is *already* in MSBuild's escaped domain and
            // is stored verbatim: MSBuild folds in
            // `((IProperty)envProperty).EvaluatedValueEscaped`
            // (`Evaluator.cs::AddEnvironmentProperties`), so a `%XX` in it is an
            // escape, not literal text — probed: `FOO=%54rue` makes
            // `'$(FOO)' == 'True'` fire, and `OS=%57indows_NT` reads
            // `Windows_NT`. `insert` is exactly that domain (`Escaped::from_xml`,
            // verbatim), the same one caller globals arrive in — *not*
            // `insert_computed`, which would escape the `%` on the way in and
            // make it inert. E1's unescape-at-the-point-of-use then produces
            // MSBuild's value for free.
            lookup.insert(key.clone(), value.clone());
        }
        // The ChangeWaves threshold. MSBuild reads the
        // `MSBUILDDISABLEFEATURESFROMVERSION` variable through
        // `Environment.GetEnvironmentVariable`, whose name lookup follows
        // the *host*: case-sensitive on Unix (probed: a lowercase or
        // mixed-case spelling is ignored), case-insensitive on Windows.
        // It exposes the *normalized* result as the reserved property:
        //   * unset or set-but-empty → the enable-all sentinel
        //     `999.999` (probed: `$(MSBuildDisableFeaturesFromVersion)`
        //     reads `999.999`, and `== ''` is False);
        //   * set to anything else → the value clamped against the
        //     version-dependent wave rotation (probed: `17.4` → `17.10`,
        //     `5.0` → `17.10`, `banana` → `999.999`), which we do not
        //     model — the property is left *undefined* so every read
        //     surfaces conservatively and `AreFeaturesEnabled` declines.
        // Seeded even for an empty snapshot — an empty environment is
        // itself a claim (see `parse_fsproj`'s docs).
        match changewave_env_value(environment, cfg!(windows)) {
            None | Some("") => {
                lookup.insert("MSBuildDisableFeaturesFromVersion", "999.999".to_string());
            }
            Some(_) => {}
        }
        let mut sticky_globals: HashSet<String> = HashSet::new();
        for (k, v) in extra_properties {
            // A caller global is *not* already-evaluated text: MSBuild unescapes
            // global property values on the way in (`-p:G=a%20b` makes `$(G)`
            // the string `a b` — probed against dotnet msbuild 10.0.301), so an
            // escape in one is live, exactly as in project XML. Plain insert.
            lookup.insert(k.clone(), v.clone());
            let lower = k.to_ascii_lowercase();
            if !local_overrides.contains(&lower) {
                protected.insert(lower.clone());
                // A non-treated-as-local global is read-only for the
                // whole walk: record it so the import gates can tell an
                // empty global (sticky) from an unset/body-written empty
                // (default-fillable).
                sticky_globals.insert(lower);
            }
        }
        let entry_project_dir = project_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Self {
            lookup,
            protected,
            reserved,
            sticky_globals,
            env_extensions_path,
            written: HashMap::new(),
            compile_first: Vec::new(),
            explicit_compile_before: Vec::new(),
            compile_before: Vec::new(),
            compile_main: Vec::new(),
            compile_after: Vec::new(),
            explicit_compile_after: Vec::new(),
            compile_last: Vec::new(),
            compile_excluded: Vec::new(),
            next_item_order: 0,
            project_references: Vec::new(),
            captured_package_references: Vec::new(),
            package_references: Vec::new(),
            package_versions: Vec::new(),
            package_versions_untracked: false,
            global_package_references: Vec::new(),
            framework_references: Vec::new(),
            evaluated_items: HashMap::new(),
            tainted_item_lists: HashSet::new(),
            untracked_item_lists: HashSet::new(),
            helper_item_definition_defaults: HashMap::new(),
            diagnostics: Vec::new(),
            follow_imports,
            imports_seen: HashSet::new(),
            imports_seen_fuzzy: HashSet::new(),
            walked_files: HashSet::new(),
            implicit_directory_build_props_fallback: None,
            walked_directory_packages_props_import: None,
            depth: 0,
            entry_project_dir,
            import_site_span: None,
            sdk_resolver,
            glob_resolver,
            hoisted_sdk_imports: HashSet::new(),
            directory_build_props_splice_path: None,
            directory_build_targets_splice_path: None,
            active_directory_build_splice: None,
            directory_build_props_splice_pending: false,
            directory_build_props_path_written_by_splice: false,
            directory_build_targets_path_written_by_splice: false,
            pending_directory_build_props: None,
            in_entry_body: false,
            resolved_sdk_root: None,
            compile_context: false,
            in_sdk_subtree: false,
            sdk_tolerance_roots: Vec::new(),
            items_uncertain: false,
            define_context: false,
            in_define_value: false,
            define_constants_uncertain: false,
            import_gate_context: false,
            package_import_gate_context: false,
            compile_condition_uncertainties: Vec::new(),
            compile_item_uncertainties: Vec::new(),
            package_context: false,
            project_references_uncertain: false,
            package_references_uncertain: false,
            package_reference_uncertainties: Vec::new(),
            sdk_package_tainted_properties: HashMap::new(),
            current_file: CurrentFile::Entry,
            deferred_item_groups: Vec::new(),
            retained_imported_files: Vec::new(),
            unpinned_value_properties: HashMap::new(),
            env_property_names,
            walk_opaque: false,
            unevaluable_written: HashSet::new(),
        }
    }

    /// Whether an undefined `$(name)` read is *exactly* the empty string
    /// in the real build too — the C.2b guard (see
    /// `docs/completed/sdk-chain-exactness-plan.md`). True only when every input
    /// that could have supplied the name says it doesn't exist:
    ///   * the walk has hidden no content (`walk_opaque`);
    ///   * no undecided or refused write could have set it
    ///     (`unpinned_value_properties`, `unevaluable_written`, SDK
    ///     package taint — the tainted map covers maybe-skipped writes
    ///     whose gate leaned on tainted input);
    ///   * the name is not in the environment snapshot (a skipped
    ///     promotion — collision or toolset overwrite — still means the
    ///     real build defines *something*);
    ///   * the name is not a toolset-computed initial property.
    fn undefined_read_is_exact(&self, name: &str) -> bool {
        if self.walk_opaque {
            return false;
        }
        let lower = name.to_ascii_lowercase();
        // Consumer-contract carve-outs, not exactness facts. The define
        // machinery treats an undefined `$(DefineConstants)` as "the SDK
        // may set DEBUG/TRACE" (its uncertainty axis feeds the LSP's
        // `#if` handling), and an undefined `$(TargetFramework)` is the
        // multi-TFM shape whose truth is per-inner-build — the LSP
        // re-evaluates with an entry-TFM global, and the crate keeps the
        // conservative signal for callers that don't.
        if matches!(lower.as_str(), "defineconstants" | "targetframework") {
            return false;
        }
        !self.env_property_names.contains(&lower)
            && !self.unpinned_value_properties.contains_key(&lower)
            && !self.unevaluable_written.contains(&lower)
            && !self.sdk_package_property_is_tainted(name)
            && !is_toolset_initial_property_name(&lower)
    }

    /// Record the [`SdkPaths::root`] of the entry project's own SDK.
    ///
    /// Called **only** for the entry root's `Sdk` shorthand and its
    /// promoted explicit form — the unambiguous framework SDK. We deliberately
    /// do *not* record SDKs reached anywhere else (nested imports, an imported
    /// file's own root `Sdk`, `Directory.Build.{props,targets}` helpers): which
    /// of a body's many `<Import Sdk=...>` / `Sdk.props` / `Sdk.targets` hits
    /// "establishes" the framework is subtle and order-dependent, so rather
    /// than guess we leave [`ParsedProject::resolved_sdk_root`] `None` for
    /// SDK-less entries and let the consumer fall back to its own default-root
    /// probe. `first`-wins is therefore moot in practice (one entry SDK), but
    /// kept so a future explicit-form change can't double-record.
    fn record_sdk_root(&mut self, root: &Path) {
        self.resolved_sdk_root
            .get_or_insert_with(|| root.to_path_buf());
        // Also tolerate it: this is the entry chokepoint for both the `Sdk`
        // shorthand and the promoted explicit form (`<Import Sdk=… Project=
        // "Sdk.props"/>` as the first body element), and the latter resolves on
        // a path that never reaches `resolve_project_sdk`. Without this its SDK
        // props/targets would be judged user-authored.
        self.note_sdk_tolerance(root);
    }

    /// Add an SDK root to the tolerance set ([`Self::sdk_tolerance_roots`]).
    /// Called at *every* successful SDK resolution — the entry SDK, a nested
    /// imported file's root `Sdk`, and an `<Import Sdk=…>` — so an SDK variant
    /// that imports a base SDK from a sibling directory is tolerated too.
    ///
    /// For the canonical .NET layout (`…/sdk/<version>/Sdks/<name>/Sdk`) the
    /// conditional default-item files (`targets/…DefaultItems…`) sit in the
    /// *sibling* `…/Sdks/<name>/targets/`, and the SDK import graph also reaches
    /// shared version-level files such as `Current/Microsoft.Common.props`,
    /// `Microsoft.Common.targets`, and language targets. So we tolerate both the
    /// SDK-specific parent (`…/Sdks/<name>`) and the version directory
    /// (`…/sdk/<version>`). For any other layout — a custom/local resolver that
    /// returns a self-contained `root` directly holding `Sdk.{props,targets}`
    /// ([`SdkPaths`] allows this) — we tolerate `root` itself: broadening to its
    /// parent would wrongly mark a user-authored `Directory.Build.props` (or
    /// explicit import) sitting beside it as SDK-internal. Canonical so it
    /// compares against the canonicalised paths [`walk_external_file`] checks
    /// (both the file's own canonical path and the canonicalisation of an
    /// imported file's parent directory). Deduplicated (resolution sites
    /// overlap).
    fn note_sdk_tolerance(&mut self, root: &Path) {
        let boundaries: Vec<&Path> = if let Some(version_dir) = dotnet_sdk_version_dir(root) {
            self.seed_toolset_properties(version_dir);
            vec![root.parent().unwrap_or(root), version_dir]
        } else {
            vec![root]
        };
        for boundary in boundaries {
            let canon = std::fs::canonicalize(boundary).unwrap_or_else(|_| boundary.to_path_buf());
            if !self.sdk_tolerance_roots.contains(&canon) {
                self.sdk_tolerance_roots.push(canon);
            }
        }
    }

    /// Seed the MSBuild toolset properties from a canonical .NET SDK
    /// version directory. In a real `dotnet msbuild` these exist from
    /// process start; this walker only learns where the toolset lives
    /// when an SDK resolves to the canonical layout, so the seed happens
    /// here — before the first SDK file is walked, which is also the
    /// first place anything can read them. Without them the SDK's own
    /// `Sdk.props` line
    /// `<Import Project="$(MSBuildExtensionsPath)\$(MSBuildToolsVersion)\Microsoft.Common.props"/>`
    /// cannot resolve, and nothing downstream of `Microsoft.Common.props`
    /// (`NuGet.props`, `Directory.Packages.props`) is ever reached.
    ///
    /// Values verified against `dotnet msbuild -getProperty:…` (10.0.300):
    /// the three path properties equal the version directory —
    /// `MSBuildExtensionsPath` *with* a trailing separator, the others
    /// without — `MSBuildSDKsPath` is its `Sdks` child, and
    /// `MSBuildToolsVersion`/`MSBuildRuntimeType` are the fixed strings
    /// `Current`/`Core`. Writability was probed the same way:
    /// `MSBuildToolsPath`, `MSBuildBinPath`, `MSBuildToolsVersion` and
    /// `MSBuildRuntimeType` are reserved (a project write is MSB4004;
    /// this walker's model for protected names is to drop the write),
    /// while `MSBuildExtensionsPath`, `MSBuildExtensionsPath32` and
    /// `MSBuildSDKsPath` accept project writes.
    ///
    /// The reserved group replaces any prior *project* write (real
    /// MSBuild rejects those outright — MSB4004 — so the stored value is
    /// one no real evaluation ever sees) and cannot arrive via
    /// `extra_properties` (`validate_inputs` rejects the names). The
    /// overridable group is insert-if-absent: caller globals and project
    /// writes legitimately win there. Either way the first canonical SDK
    /// resolution's seed wins over later ones.
    fn seed_toolset_properties(&mut self, version_dir: &Path) {
        // Real `dotnet msbuild` reports its own fully-resolved location
        // (the process resolves symlinks to find itself), so canonicalise
        // for the same shape; the tolerance roots are canonical too.
        let version_dir =
            std::fs::canonicalize(version_dir).unwrap_or_else(|_| version_dir.to_path_buf());
        let dir = version_dir.to_string_lossy().replace('\\', "/");
        let reserved = [
            ("MSBuildToolsPath", dir.clone()),
            ("MSBuildBinPath", dir.clone()),
            ("MSBuildToolsVersion", "Current".to_string()),
            ("MSBuildRuntimeType", "Core".to_string()),
        ];
        for (name, value) in reserved {
            let lower = name.to_ascii_lowercase();
            // First canonical seed wins over later SDK resolutions, and a
            // caller-supplied global (the documented environment model)
            // wins over the seed. A *project* write that landed before
            // the first SDK resolved does NOT win: real MSBuild has
            // these names reserved from process start and rejects the
            // write outright (MSB4004), so the stored value is one no
            // real evaluation ever sees — replace it and scrub its
            // bookkeeping so stale unpinned/taint marks don't outlive
            // the value they described.
            // (`validate_inputs` rejects these names in `extra_properties`,
            // so no caller-global can reach here.)
            if self.reserved.contains(&lower) {
                continue;
            }
            // Evaluator-computed (SDK/toolset paths and toolset constants), not
            // project XML: percents in them are literal.
            self.lookup.insert_computed(name, value);
            self.written.remove(&lower);
            // Scrub both provenance marks so a stale taint/unpinned entry
            // doesn't outlive the value it described.
            self.apply_property_provenance(
                name,
                &lower,
                PropertyProvenance {
                    taint: TaintOutcome::Clear,
                    unpinned: UnpinnedOutcome::Clear,
                },
            );
            self.reserved.insert(lower.clone());
            self.protected.insert(lower);
        }
        let overridable = [
            ("MSBuildExtensionsPath32", dir.clone()),
            ("MSBuildSDKsPath", format!("{dir}/Sdks")),
        ];
        for (name, value) in overridable {
            if self.lookup.get(name).is_none() {
                // Same provenance as the reserved seeds above: our own paths.
                self.lookup.insert_computed(name, value);
            }
        }
        // `MSBuildExtensionsPath` is overridable in the same way — a caller
        // global or a project write wins on every toolset (probed against
        // 8.0.420: `-p:MSBuildExtensionsPath=/SPOOF` redirects the `Sdk.props`
        // import even where an environment variable of the same name would
        // have been overwritten) — so an occupied slot still stands.
        //
        // The *environment* is the special case, and the only one in the whole
        // environment model whose answer depends on the toolset version: see
        // `toolset_honours_env_extensions_path`. Now that an SDK has named the
        // toolset, the value parked in `State::new` can be adjudicated.
        if self.lookup.get("MSBuildExtensionsPath").is_none() {
            let toolset_value = format!("{dir}/");
            match (
                &self.env_extensions_path,
                toolset_honours_env_extensions_path(&version_dir),
            ) {
                // No environment value: every toolset computes the property
                // from its own directory, so there is nothing to adjudicate.
                (EnvExtensionsPath::Absent, _) => {
                    self.lookup
                        .insert_computed("MSBuildExtensionsPath", toolset_value);
                }
                // An environment value the toolset overwrites — the toolset's
                // own directory is the value the real build reads.
                (_, Some(false)) => {
                    self.lookup
                        .insert_computed("MSBuildExtensionsPath", toolset_value);
                }
                // MSBuild 18 lets it stand. Escaped-domain text, exactly as it
                // arrived (see the promotion loop in `State::new`), so `insert`
                // rather than `insert_computed`.
                (EnvExtensionsPath::Value(value), Some(true)) => {
                    self.lookup
                        .insert("MSBuildExtensionsPath", value.to_string());
                }
                // The toolset would honour a value we cannot name (colliding
                // spellings), or we cannot read the toolset's version and so
                // cannot say which of the two behaviours applies. Either way,
                // leave the property undefined: the read declines instead of
                // committing a guess.
                (EnvExtensionsPath::Unspecified, Some(true)) | (_, None) => {}
            }
        }
    }

    fn into_project(self) -> ParsedProject {
        debug_assert!(
            self.deferred_item_groups.is_empty(),
            "the item pass must consume every deferred ItemGroup before \
             the walk produces its result"
        );
        // The output-name verdict and the untrusted-provenance set need
        // `&self` (the pin/taint maps), so compute them before the
        // destructure below consumes them.
        let target_name = self.target_name_verdict();
        let untrusted_properties: std::collections::HashSet<String> = self
            .unpinned_value_properties
            .keys()
            .chain(self.sdk_package_tainted_properties.keys())
            .cloned()
            .collect();
        let State {
            lookup,
            protected: _,
            reserved: _,
            sticky_globals: _,
            env_extensions_path: _,
            written,
            compile_first,
            explicit_compile_before,
            compile_before,
            compile_main,
            compile_after,
            explicit_compile_after,
            compile_last,
            compile_excluded: _,
            next_item_order: _,
            project_references,
            // Already collapsed into `package_references` by the item pass's
            // `finalize_package_references`.
            captured_package_references: _,
            package_references,
            package_versions,
            package_versions_untracked,
            global_package_references,
            framework_references,
            evaluated_items: _,
            tainted_item_lists: _,
            untracked_item_lists: _,
            helper_item_definition_defaults: _,
            diagnostics,
            follow_imports: _,
            imports_seen: _,
            imports_seen_fuzzy: _,
            walked_files: _,
            implicit_directory_build_props_fallback: _,
            walked_directory_packages_props_import: _,
            depth: _,
            entry_project_dir: _,
            import_site_span: _,
            sdk_resolver: _,
            glob_resolver: _,
            hoisted_sdk_imports: _,
            directory_build_props_splice_path: _,
            directory_build_targets_splice_path: _,
            active_directory_build_splice: _,
            directory_build_props_splice_pending: _,
            directory_build_props_path_written_by_splice: _,
            directory_build_targets_path_written_by_splice: _,
            pending_directory_build_props: _,
            in_entry_body: _,
            resolved_sdk_root,
            compile_context: _,
            in_sdk_subtree: _,
            sdk_tolerance_roots: _,
            items_uncertain,
            define_context: _,
            in_define_value: _,
            define_constants_uncertain,
            import_gate_context: _,
            package_import_gate_context: _,
            compile_condition_uncertainties,
            compile_item_uncertainties,
            package_context: _,
            project_references_uncertain,
            package_references_uncertain,
            package_reference_uncertainties,
            sdk_package_tainted_properties: _,
            current_file: _,
            deferred_item_groups: _,
            retained_imported_files: _,
            unpinned_value_properties: _,
            env_property_names: _,
            walk_opaque: _,
            unevaluable_written: _,
        } = self;
        // Central Package Management opt-in: a versionless
        // `<PackageReference Include="X"/>` may receive its effective version
        // from `<PackageVersion>` items. Start conservative, then let the
        // inline CPM pass below clear the uncertainty only for the exact subset
        // it can prove from already-captured items.
        let manages_versions_centrally = lookup
            .get_unescaped("ManagePackageVersionsCentrally")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let central_package_versions_file_imported = lookup
            .get_unescaped("CentralPackageVersionsFileImported")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let central_package_version_override_enabled = !lookup
            .get_unescaped("CentralPackageVersionOverrideEnabled")
            .is_some_and(|v| v.eq_ignore_ascii_case("false"));
        // `package_references` is already the effective (Include + Update
        // collapsed) set, with versionless uncertainty detected on it, from the
        // item pass's `finalize_package_references`.
        let mut package_references = package_references;
        let mut package_references_uncertain = package_references_uncertain;
        let mut package_reference_uncertainties = package_reference_uncertainties;
        if manages_versions_centrally {
            package_references_uncertain = true;
            package_reference_uncertainties.push(PackageReferenceUncertaintyCause {
                kind: PackageReferenceUncertaintyCauseKind::ManagePackageVersionsCentrally,
                span: 0..0,
                origin: DiagnosticOrigin::Buffer,
            });
        }
        apply_inline_cpm_versions(
            manages_versions_centrally && central_package_versions_file_imported,
            &mut package_references,
            &package_versions,
            package_versions_untracked,
            &global_package_references,
            central_package_version_override_enabled,
            &mut package_reference_uncertainties,
        );
        if package_reference_uncertainties.is_empty() {
            package_references_uncertain = false;
        }
        let is_partial = !diagnostics.is_empty();
        let mut items = into_resolved_items(compile_first);
        items.extend(into_resolved_items(explicit_compile_before));
        items.extend(into_resolved_items(compile_before));
        items.extend(into_resolved_items(compile_main));
        items.extend(into_resolved_items(compile_after));
        items.extend(into_resolved_items(explicit_compile_after));
        items.extend(into_resolved_items(compile_last));
        let project_references = into_resolved_items(project_references);
        // `properties` is documented as "what the project wrote". Use
        // the project's canonical casing as recorded in `written` for
        // the output keys — otherwise a treated-as-local override
        // where extras supplied a different casing would surface the
        // extras casing here, and an exact `properties.get("Foo")`
        // would miss the project's write. Look the value up via
        // case-insensitive `PropertyMap::get`, which handles the same
        // mismatch on the value side.
        let properties = written
            .into_values()
            .filter_map(|project_name| {
                lookup
                    .get_unescaped(&project_name)
                    .map(|v| (project_name, v))
            })
            .collect();
        // `define_constants` reflects the *evaluated* `$(DefineConstants)`
        // value: globals supplied via `extra_properties` and reserved
        // well-knowns are deliberately absent from `properties` (which
        // documents "what the project wrote"), but the F# preprocessor
        // sees the merged effect. Source the value from `lookup`, which
        // is the same map MSBuild would consult when expanding
        // `$(DefineConstants)` itself.
        // The list is split on the semicolons of the **escaped** value and each
        // fragment decoded after — MSBuild's property-to-list conversion, which
        // makes `A%3bB` the single define `A;B` rather than two
        // (oracle-pinned 2026-07-12). Decoding first would split it in two.
        let define_constants = lookup
            .get("DefineConstants")
            .map(crate::define_constants_from_escaped)
            .unwrap_or_default();
        // Same rule, same reason: split the escaped value, decode each fragment.
        let target_frameworks: Vec<String> = lookup
            .get("TargetFrameworks")
            .map(|v| {
                v.split_list()
                    .map(|f| f.unescape().trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        // `<LangVersion>` from the same merged map. Trim and drop empties so an
        // unset (or whitespace-only) value reads as `None` rather than `Some("")`.
        let lang_version = lookup
            .get_unescaped("LangVersion")
            .map(|v| v.trim().to_string())
            .filter(|s| !s.is_empty());
        ParsedProject {
            target_frameworks,
            items,
            project_references,
            project_references_uncertain,
            package_references,
            package_versions,
            global_package_references,
            framework_references,
            properties,
            define_constants,
            lang_version,
            target_name,
            untrusted_properties,
            diagnostics,
            is_partial,
            items_uncertain,
            define_constants_uncertain,
            compile_condition_uncertainties,
            compile_item_uncertainties,
            package_references_uncertain,
            package_reference_uncertainties,
            resolved_sdk_root,
        }
    }

    fn record_directory_build_path_write(&mut self, name: &str) {
        let written_by_splice = self.active_directory_build_splice.is_some();
        if name.eq_ignore_ascii_case("DirectoryBuildPropsPath") {
            self.directory_build_props_path_written_by_splice = written_by_splice;
        } else if name.eq_ignore_ascii_case("DirectoryBuildTargetsPath") {
            self.directory_build_targets_path_written_by_splice = written_by_splice;
        }
    }

    /// Whether `name` (case-insensitive) is *currently* a read-only
    /// global property — i.e. one MSBuild's `Microsoft.Common.props`
    /// default-fill cannot write through. See [`State::sticky_globals`].
    ///
    /// A name only counts as sticky while it is *also* still protected:
    /// an imported root's `TreatAsLocalProperty` unprotects a global for
    /// that file's scope (removing it from [`State::protected`]), which
    /// makes it locally writable there — so the default-fill *can* write
    /// through and the sticky-global short-circuit must stand down. This
    /// keeps the gate decision in lockstep with the `TreatAsLocalProperty`
    /// model `walk_external_file` already implements via `protected`.
    fn is_sticky_global(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.sticky_globals.contains(&lower) && self.protected.contains(&lower)
    }

    /// Flag the package/framework-reference set as untrustworthy for a
    /// divergence that raises no diagnostic (an ignored `Remove`, an
    /// `@(…)`/`%(…)` reference in an `Include`, a substitution issue). Only
    /// effective in a `package_context`, matching the diagnostic-driven flip in
    /// [`Self::push`].
    fn note_package_uncertain(
        &mut self,
        kind: PackageReferenceUncertaintyCauseKind,
        span: Range<usize>,
    ) {
        if self.package_context {
            self.package_references_uncertain = true;
            self.record_package_reference_uncertainty(kind, span);
        }
    }

    /// Mark only the package/framework-reference set untrustworthy because a
    /// structural construct that can carry dependency items was skipped. This
    /// is used for SDK-internal structural skips: they are tolerated for the
    /// Compile set, but SDK files are exactly where implicit dependency items
    /// commonly live.
    fn mark_package_structural_skip(
        &mut self,
        kind: StructuralPackageReferenceUncertainty,
        span: Range<usize>,
    ) {
        self.package_references_uncertain = true;
        // Every structural skip funnels through here, and each one hides
        // content that could carry property writes — an unfollowable SDK
        // or import, an un-descended Choose. (The Choose case is
        // over-conservative for properties — its branch writes are also
        // enumerated per-name — but a sound over-taint.)
        self.walk_opaque = true;
        self.record_package_reference_uncertainty(
            PackageReferenceUncertaintyCauseKind::Structural(kind),
            span,
        );
    }

    /// Mark a structural skip, preserving the SDK Compile carve-out. Outside
    /// SDK files the skip can affect both Compile and dependency items; inside
    /// SDK files Compile uncertainty is tolerated, but package uncertainty is
    /// still recorded.
    fn mark_structural_skip_respecting_sdk_compile_tolerance(
        &mut self,
        kind: StructuralCompileItemUncertainty,
        span: Range<usize>,
    ) {
        if self.in_sdk_subtree {
            self.mark_package_structural_skip(
                package_structural_uncertainty_from_compile(&kind),
                span,
            );
        } else {
            self.mark_structural_skip(kind, span);
        }
    }

    /// Mark *both* captured item sets untrustworthy because a structural
    /// construct that can carry them was skipped — a dropped/unfollowable
    /// `<Import>` or SDK, or an un-descended `<Choose>`. Such a construct can
    /// hold `<Compile>` items *and* `<PackageReference>`/`<FrameworkReference>`
    /// items, so leaving one flag set without the other would let a resolver
    /// trust an incomplete package set. (Build-time-only containers like
    /// `<Target>` never call this — they contribute to neither *static* set,
    /// matching MSBuild's static item evaluation.) Call sites already gate on
    /// `!in_sdk_subtree` where SDK-internal skips are Compile-tolerated.
    fn mark_structural_skip(&mut self, kind: StructuralCompileItemUncertainty, span: Range<usize>) {
        self.items_uncertain = true;
        // A skipped import / unresolved SDK can carry `<ProjectReference>`
        // mutations as easily as Compile items. The un-descended `<Choose>`
        // is the exception: `handle_choose` scans its still-possible
        // branches for reference mutations itself, so an Include-only
        // branch stays at worst a missed reference.
        if !matches!(kind, StructuralCompileItemUncertainty::UnsupportedChoose) {
            self.project_references_uncertain = true;
        }
        self.mark_package_structural_skip(
            package_structural_uncertainty_from_compile(&kind),
            span.clone(),
        );
        self.record_compile_item_uncertainty(
            CompileItemUncertaintyCauseKind::Structural(kind),
            span,
        );
    }

    fn push(&mut self, kind: DiagnosticKind, span: Range<usize>) {
        // `import_site_span.is_some()` iff we're currently walking an
        // imported file — `walk_external_file` sets it on first
        // descent and preserves it on nested descents. That's the
        // same condition `effective_span` already keys on, so the two
        // stay in lockstep without extra state: any diagnostic whose
        // span was remapped also gets `Imported`, and any diagnostic
        // produced from the entry project keeps its native span and
        // `Buffer`.
        // Any diagnostic raised while resolving a Compile item/group concerns
        // *that* item's inclusion, so it makes the Compile set untrustworthy.
        // Outside that context, a kind that can itself carry Compile items (a
        // failed import / unresolved SDK — see [`is_structural_compile_risk`])
        // does too, but only in a user-authored file: inside the SDK tree such
        // failures are part of the machinery we tolerate (an SDK sub-import we
        // can't follow never drops a *hand-written* source). Everything else (an
        // undefined property or skipped `<Target>` in an SDK file) leaves the
        // Compile set intact, even though it still flips `is_partial`.
        // A structural kind means a file's worth of content did not enter
        // the walk — after it, an undefined read could be of a name that
        // hidden content defined (see [`State::walk_opaque`]).
        if is_structural_compile_risk(&kind) {
            self.walk_opaque = true;
        }
        let compile_uncertain = self.compile_context
            || self.import_gate_context
            || (!self.in_sdk_subtree && is_structural_compile_risk(&kind));
        if compile_uncertain {
            self.items_uncertain = true;
        }
        // The preprocessor-symbol analogue: uncertainty while resolving a user
        // `<DefineConstants>` write (or its gating condition) means the `#if`
        // symbol set may be wrong. `define_context` is only set outside the SDK
        // tree, so no further provenance check. The one exception is a
        // `$(DefineConstants)` *self-reference* substituting to "" — the
        // universal `$(DefineConstants);FOO` append idiom — but only in the
        // value (`in_define_value`); in a *condition* it's a real branch
        // decision, so it still flags.
        if self.define_context && !(self.in_define_value && is_define_self_reference(&kind)) {
            self.define_constants_uncertain = true;
        }
        // The package-set analogue of the Compile rule above, with the same
        // shape: a diagnostic raised while resolving a package/framework
        // reference or its gating group (`package_context`) makes the set
        // untrustworthy, and so does a structural construct that can *carry*
        // such references — an unresolved/failed import or missing SDK —
        // anywhere (`is_structural_compile_risk` names Compile but the same
        // imports can hold `<PackageReference>` items). Unlike Compile
        // uncertainty, package structural risk is not SDK-gated: SDK imports
        // are a normal source of implicit dependency items.
        let package_uncertain = self.package_context
            || self.package_import_gate_context
            || is_structural_compile_risk(&kind);
        if package_uncertain {
            self.package_references_uncertain = true;
        }
        // The reference-list analogue of the structural Compile rule: a
        // failed or unfollowable import in a user-authored file may carry
        // `<ProjectReference>` mutations we never saw, so the captured list
        // may claim references the real build strips. SDK-gated like the
        // Compile rule — SDK sub-imports don't declare user project
        // references at evaluation time.
        if !self.in_sdk_subtree && is_structural_compile_risk(&kind) {
            self.project_references_uncertain = true;
        }
        let origin = self.current_origin();
        let span = self.effective_span(span);
        if compile_uncertain {
            self.compile_item_uncertainties
                .push(CompileItemUncertaintyCause {
                    kind: CompileItemUncertaintyCauseKind::Diagnostic(kind.clone()),
                    span: span.clone(),
                    origin: origin.clone(),
                });
        }
        if package_uncertain {
            self.package_reference_uncertainties
                .push(PackageReferenceUncertaintyCause {
                    kind: PackageReferenceUncertaintyCauseKind::Diagnostic(kind.clone()),
                    span: span.clone(),
                    origin: origin.clone(),
                });
        }
        self.diagnostics.push(Diagnostic { kind, span, origin });
    }

    /// The [`DiagnosticOrigin`] for a construct reached at the current walk
    /// position: `Imported` iff we're inside an imported file (the same
    /// `import_site_span` test [`Self::push`] uses).
    fn current_origin(&self) -> DiagnosticOrigin {
        if self.import_site_span.is_some() {
            DiagnosticOrigin::Imported
        } else {
            DiagnosticOrigin::Buffer
        }
    }

    /// Record a [`CompileConditionUncertainty`] for a Compile item/group whose
    /// `Condition` we couldn't trust. Span/origin follow the same remap rules
    /// as [`Self::push`]. Callers gate on [`Self::compile_context`].
    fn record_compile_condition_uncertainty(
        &mut self,
        condition: &str,
        reason: CompileConditionReason,
        span: Range<usize>,
    ) {
        let origin = self.current_origin();
        let span = self.effective_span(span);
        self.compile_condition_uncertainties
            .push(CompileConditionUncertainty {
                condition: condition.to_string(),
                reason,
                span,
                origin,
            });
    }

    fn record_compile_item_uncertainty(
        &mut self,
        kind: CompileItemUncertaintyCauseKind,
        span: Range<usize>,
    ) {
        let origin = self.current_origin();
        let span = self.effective_span(span);
        self.compile_item_uncertainties
            .push(CompileItemUncertaintyCause { kind, span, origin });
    }

    fn record_package_reference_uncertainty(
        &mut self,
        kind: PackageReferenceUncertaintyCauseKind,
        span: Range<usize>,
    ) {
        let origin = self.current_origin();
        let span = self.effective_span(span);
        self.package_reference_uncertainties
            .push(PackageReferenceUncertaintyCause { kind, span, origin });
    }

    fn taint_item_list(&mut self, item_type: &str) {
        self.tainted_item_lists.insert(item_key(item_type));
    }

    fn invalidate_item_list(&mut self, item_type: &str) {
        let key = item_key(item_type);
        self.evaluated_items.remove(&key);
        self.tainted_item_lists.insert(key);
    }

    fn mark_untracked_item_list(&mut self, item_type: &str) {
        self.untracked_item_lists.insert(item_key(item_type));
    }

    fn record_helper_item_definition_default(&mut self, item_type: &str, metadata_name: &str) {
        self.helper_item_definition_defaults
            .entry(item_key(item_type))
            .or_default()
            .entry(item_key(metadata_name))
            .or_insert_with(|| HelperMetadataUncertainty::item_definition_default(metadata_name));
    }

    fn mark_sdk_package_property_tainted(&mut self, name: &str, span: Range<usize>) {
        let lower = name.to_ascii_lowercase();
        let span = self.effective_span(span);
        let origin = self.current_origin();
        self.sdk_package_tainted_properties
            .insert(lower, SdkPackagePropertyTaint { span, origin });
    }

    fn clear_sdk_package_property_taint(&mut self, name: &str) {
        self.sdk_package_tainted_properties
            .remove(&name.to_ascii_lowercase());
    }

    fn sdk_package_property_is_tainted(&self, name: &str) -> bool {
        self.sdk_package_tainted_properties
            .contains_key(&name.to_ascii_lowercase())
    }

    /// The single point that mutates either forward-uncertainty channel:
    /// apply a [`PropertyProvenance`] verdict to both maps. `name` keys the
    /// taint map (lowercased internally, carrying the write span/origin);
    /// `lower` keys the unpinned map. Every taint/unpinned mutation flows
    /// through here so the two channels cannot drift at population time.
    fn apply_property_provenance(
        &mut self,
        name: &str,
        lower: &str,
        provenance: PropertyProvenance,
    ) {
        match provenance.taint {
            TaintOutcome::Set(span) => self.mark_sdk_package_property_tainted(name, span),
            TaintOutcome::Clear => self.clear_sdk_package_property_taint(name),
            TaintOutcome::Keep => {}
        }
        match provenance.unpinned {
            UnpinnedOutcome::Set(root) => {
                self.unpinned_value_properties
                    .insert(lower.to_string(), root);
            }
            UnpinnedOutcome::Clear => {
                self.unpinned_value_properties.remove(lower);
            }
            UnpinnedOutcome::Keep => {}
        }
    }

    fn sdk_package_taint_for_raw(&self, raw: &str) -> Option<SdkPackagePropertyTaint> {
        simple_property_references(raw).find_map(|name| {
            self.sdk_package_tainted_properties
                .get(&name.to_ascii_lowercase())
                .cloned()
        })
    }

    fn raw_uses_sdk_package_taint(&self, raw: &str) -> bool {
        self.sdk_package_taint_for_raw(raw).is_some()
    }

    /// The root cause behind the first unpinned property `raw` references,
    /// if any (see [`Self::unpinned_value_properties`]).
    fn unpinned_root_for_raw(&self, raw: &str) -> Option<UnpinnedRoot> {
        simple_property_references(raw).find_map(|name| {
            self.unpinned_value_properties
                .get(&name.to_ascii_lowercase())
                .cloned()
        })
    }

    /// Whether `name`'s end-of-evaluation value provenance is untrusted —
    /// unpinned (some write to it sat behind a gate we couldn't evaluate,
    /// or its value leaned on one) or SDK-package-tainted. The basis of
    /// [`ParsedProject::untrusted_properties`].
    fn property_provenance_untrusted(&self, name: &str) -> bool {
        self.unpinned_value_properties
            .contains_key(&name.to_ascii_lowercase())
            || self.sdk_package_property_is_tainted(name)
    }

    /// The [`ParsedProject::target_name`] verdict: the output file's base
    /// name is `$(TargetName)`, defaulting to `$(AssemblyName)` (the common
    /// targets' `<TargetName Condition="'$(TargetName)' == ''">` write —
    /// probed, dotnet 10.0.301: an explicit `TargetName` beats
    /// `AssemblyName` in the output filename). Each candidate in turn:
    /// untrusted provenance or residual `$(...)` decides `Unknown` (a
    /// consumer locating an output DLL by a wrong name fabricates);
    /// whitespace-only is `Unknown` too (MSBuild preserves padding in the
    /// filename — probed, ` Padded .dll` — but the default gate compares
    /// `== ''` exactly, so a whitespace-only spelling names the output
    /// file something we refuse to guess); a clean non-empty value wins
    /// **verbatim** (never trimmed); empty/unset falls to the next
    /// candidate. Neither set is `Known(None)`: MSBuild's default
    /// (`$(MSBuildProjectName)`, the project-file stem) applies.
    fn target_name_verdict(&self) -> ItemMetadataValue {
        for name in ["TargetName", "AssemblyName"] {
            if self.property_provenance_untrusted(name) {
                return ItemMetadataValue::Unknown;
            }
            match self.lookup.get(name) {
                None => continue,
                Some(v) if v.is_empty() => continue,
                Some(v) if v.as_escaped().contains("$(") => return ItemMetadataValue::Unknown,
                Some(v) if v.unescape().trim().is_empty() => return ItemMetadataValue::Unknown,
                Some(v) => return ItemMetadataValue::known(v.unescape()),
            }
        }
        ItemMetadataValue::Known(None)
    }

    /// Whether a node's `Condition` leans on a value that may differ in a
    /// real build — an *unpinned* property (written under a gate we couldn't
    /// evaluate) or an SDK-package-tainted one — making even a cleanly
    /// decided True/False an untrustworthy branch decision. Undefined,
    /// never-written properties are deliberately NOT untrusted: under the
    /// walker's environment model (caller `extra_properties` ARE the
    /// environment) they are genuinely unset, so treating them as empty is
    /// exact — the same commitment the default-fill exemption and the
    /// pinned `'$(NoSuchProp)' == 'on'` reference-mutation cases make.
    fn condition_reads_untrusted_value(&self, node: Node<'_, '_>) -> bool {
        node.attribute("Condition").is_some_and(|cond| {
            self.unpinned_root_for_raw(cond).is_some() || self.raw_uses_sdk_package_taint(cond)
        })
    }

    /// The [`UnpinnedRoot`] to record for a write gated on a condition the
    /// property pass could not pin down: the root cause the condition's
    /// evaluation just surfaced as diagnostics (scanned from
    /// `diagnostics[since..]`), preferring an undefined property name over
    /// an unsupported-condition report.
    fn unpinned_root_from_recent_diagnostics(&self, since: usize) -> Option<UnpinnedRoot> {
        let recent = &self.diagnostics[since..];
        recent
            .iter()
            .find_map(|diagnostic| match &diagnostic.kind {
                DiagnosticKind::UndefinedProperty { name } => {
                    Some(UnpinnedRoot::Undefined(name.clone()))
                }
                _ => None,
            })
            .or_else(|| {
                recent.iter().find_map(|diagnostic| match &diagnostic.kind {
                    DiagnosticKind::UnsupportedCondition { condition } => {
                        Some(UnpinnedRoot::UnsupportedCondition(condition.clone()))
                    }
                    _ => None,
                })
            })
    }

    fn note_package_uncertain_from_sdk_property_taint(&mut self, raw: &str) {
        let Some(taint) = self.sdk_package_taint_for_raw(raw) else {
            return;
        };
        self.package_references_uncertain = true;
        self.package_reference_uncertainties
            .push(PackageReferenceUncertaintyCause {
                kind: PackageReferenceUncertaintyCauseKind::SdkDependencyItemPropertyEvaluation,
                span: taint.span,
                origin: taint.origin,
            });
    }

    fn note_package_uncertain_if_condition_uses_sdk_taint(&mut self, node: Node<'_, '_>) {
        if let Some(condition) = node.attribute("Condition") {
            self.note_package_uncertain_from_sdk_property_taint(condition);
        }
    }

    /// Override `span` with `import_site_span` if we're currently
    /// walking an imported file. Callers pass `node.range()` from
    /// whatever XML buffer they're walking; while inside an external
    /// file those offsets are not valid for the entry-project source
    /// the caller will eventually consult — see [`Self::import_site_span`].
    fn effective_span(&self, span: Range<usize>) -> Range<usize> {
        self.import_site_span.clone().unwrap_or(span)
    }

    /// Substitute `$(...)` in `raw` and push any issues as diagnostics
    /// attributed to `span`. The returned [`Expansion`] separates
    /// undefined-reference issues (safe — value is empty but well-formed)
    /// from unsupported-expression issues (unsafe — value contains
    /// residual `$(...)` literal text) so callers can take different
    /// actions for each.
    fn expand(&mut self, raw: &str, span: Range<usize>) -> Expansion {
        self.expand_inner(raw, span, None)
    }

    /// Expand a property-pass value.
    /// `writing_property` is the destination name: an unpinned
    /// self-reference (the `$(OtherFlags) --flag` accumulator idiom) still
    /// propagates the pin state but is not re-surfaced as a diagnostic —
    /// the root was already reported when the property first became
    /// unpinned, and each append hop adds no new information.
    fn expand_property_pass_value(
        &mut self,
        raw: &str,
        span: Range<usize>,
        writing_property: &str,
    ) -> Expansion {
        self.expand_inner(raw, span, Some(writing_property))
    }

    fn expand_inner(
        &mut self,
        raw: &str,
        span: Range<usize>,
        writing_property: Option<&str>,
    ) -> Expansion {
        // FS-probing property functions are with-imports-only; the pure
        // surface documents "no filesystem access".
        let (value, issues) = if self.follow_imports {
            properties::substitute_with_fs(raw, &self.lookup)
        } else {
            properties::substitute(raw, &self.lookup)
        };
        let mut had_undefined = false;
        let mut had_unsupported = false;
        let mut direct_undefined: Vec<String> = Vec::new();
        for issue in issues {
            let kind = match issue {
                Issue::Undefined { name } => {
                    // An undefined read the walk can prove is undefined
                    // in the real build too is *exactly* the empty
                    // string MSBuild substitutes — not a divergence, so
                    // no diagnostic and no unpinning (C.2b).
                    if self.undefined_read_is_exact(&name) {
                        continue;
                    }
                    had_undefined = true;
                    direct_undefined.push(name.clone());
                    DiagnosticKind::UndefinedProperty { name }
                }
                Issue::Unsupported { expression } => {
                    had_unsupported = true;
                    DiagnosticKind::UnsupportedPropertyExpression { expression }
                }
            };
            self.push(kind, span.clone());
        }
        // A reference to an *unpinned* property (its stored value leaned on
        // an undefined name or an unevaluable gate — see
        // [`State::unpinned_value_properties`]) carries the same divergence
        // risk as a direct undefined reference, but plain substitution
        // cannot see it: the property IS defined. Re-surface each root here
        // so every read — property-pass or item-pass — reports it under the
        // active contexts, exactly like the direct case.
        let mut unpinned_root = direct_undefined
            .first()
            .cloned()
            .map(UnpinnedRoot::Undefined);
        let mut surfaced: Vec<UnpinnedRoot> = Vec::new();
        for reference in simple_property_references(raw) {
            let Some(root) = self
                .unpinned_value_properties
                .get(&reference.to_ascii_lowercase())
                .cloned()
            else {
                continue;
            };
            if unpinned_root.is_none() {
                unpinned_root = Some(root.clone());
            }
            if writing_property.is_some_and(|dest| dest.eq_ignore_ascii_case(reference)) {
                // Self-append accumulator idiom: propagate the pin state
                // (done just above) without re-reporting the root at every
                // hop.
                continue;
            }
            if let UnpinnedRoot::Undefined(name) = &root
                && direct_undefined
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(name))
            {
                // The same root already surfaced as a direct issue above.
                continue;
            }
            if surfaced.contains(&root) {
                continue;
            }
            self.push(root.to_diagnostic(), span.clone());
            surfaced.push(root);
        }
        Expansion {
            value,
            had_undefined,
            had_unsupported,
            unpinned_root,
        }
    }

    /// Rebind `MSBuildThisFile` and `MSBuildThisFileDirectory` to the
    /// imported file. MSBuild rebinds these for the duration of the
    /// import; references like `$(MSBuildThisFileDirectory)foo.props`
    /// in a `Directory.Build.props` should resolve against *that*
    /// file's directory, not the entry project's.
    ///
    /// Both names are seeded by [`properties::well_known`], so the
    /// saved values are always present; the [`ThisFileFrame`] just
    /// records them verbatim for [`Self::exit_this_file`] to restore.
    fn enter_this_file(&mut self, file_path: &Path) -> ThisFileFrame {
        let saved_this_file = self
            .lookup
            .get("MSBuildThisFile")
            .cloned()
            .unwrap_or_default();
        let saved_this_file_directory = self
            .lookup
            .get("MSBuildThisFileDirectory")
            .cloned()
            .unwrap_or_default();
        let file = file_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // `MSBuildThisFileDirectory` carries a trailing separator (per
        // MSBuild docs). Match the convention from
        // `properties::well_known` so substitutions like
        // `$(MSBuildThisFileDirectory)foo.props` work identically here.
        let dir = file_path
            .parent()
            .map(|p| {
                let s = p.to_string_lossy().into_owned();
                if s.is_empty() {
                    String::new()
                } else {
                    format!("{s}/")
                }
            })
            .unwrap_or_default();
        // Evaluator-computed reserved values, not project XML: a `%` in an
        // imported file's path is literal, exactly as for `well_known`'s seeds.
        self.lookup.insert_computed("MSBuildThisFile", file);
        self.lookup.insert_computed("MSBuildThisFileDirectory", dir);
        ThisFileFrame {
            saved_this_file,
            saved_this_file_directory,
        }
    }

    fn exit_this_file(&mut self, frame: ThisFileFrame) {
        self.lookup
            .insert_escaped("MSBuildThisFile", frame.saved_this_file);
        self.lookup
            .insert_escaped("MSBuildThisFileDirectory", frame.saved_this_file_directory);
    }
}

/// Apply the CPM subset that is exact from already-captured items:
/// NuGet-marked Central Package Management, unique inline `PackageVersion`
/// items, no local `PackageReference Version` metadata, no central-version
/// mutations, and no global implicit refs. (`Update` items have already been
/// folded into their `Include` by the item pass's
/// `finalize_package_references`, so a version an `Update` supplied is seen
/// here as the `Include`'s own.)
/// `VersionOverride` is applied only when NuGet's override switch permits it.
/// Anything outside that envelope leaves the existing uncertainty causes in
/// place so the NuGet resolver still declines rather than guessing.
fn apply_inline_cpm_versions(
    central_package_management_enabled: bool,
    package_references: &mut [PackageReference],
    package_versions: &[PackageVersion],
    package_versions_untracked: bool,
    global_package_references: &[GlobalPackageReference],
    central_package_version_override_enabled: bool,
    package_reference_uncertainties: &mut Vec<PackageReferenceUncertaintyCause>,
) {
    if !central_package_management_enabled
        || package_versions_untracked
        || !global_package_references.is_empty()
    {
        return;
    }
    let Some(central_versions) = unique_central_versions(package_versions) else {
        return;
    };

    let mut all_includes_resolved = true;
    for reference in package_references.iter_mut() {
        if reference.version.is_some() {
            all_includes_resolved = false;
            continue;
        }
        let key = reference.id.to_ascii_lowercase();
        if let Some(version_override) = &reference.version_override {
            if central_package_version_override_enabled {
                reference.version = Some(version_override.clone());
            } else {
                all_includes_resolved = false;
            }
            continue;
        }
        if let Some(version) = central_versions.get(&key) {
            reference.version = Some(version.clone());
            continue;
        }
        if reference.version.is_none() {
            all_includes_resolved = false;
        }
    }

    if all_includes_resolved {
        package_reference_uncertainties
            .retain(|cause| !is_discharged_inline_cpm_uncertainty(&cause.kind));
    }
}

fn unique_central_versions(package_versions: &[PackageVersion]) -> Option<HashMap<String, String>> {
    let mut central_versions = HashMap::new();
    for package_version in package_versions {
        let key = package_version.id.to_ascii_lowercase();
        if central_versions.contains_key(&key) {
            return None;
        }
        let version = package_version.version.clone()?;
        central_versions.insert(key, version);
    }
    Some(central_versions)
}

fn is_discharged_inline_cpm_uncertainty(kind: &PackageReferenceUncertaintyCauseKind) -> bool {
    matches!(
        kind,
        PackageReferenceUncertaintyCauseKind::ManagePackageVersionsCentrally
            | PackageReferenceUncertaintyCauseKind::PackageVersion
            | PackageReferenceUncertaintyCauseKind::VersionlessPackageReference { .. }
    )
}

/// Saved values for [`State::exit_this_file`] to restore after an
/// imported file's body has been walked. See [`State::enter_this_file`].
struct ThisFileFrame {
    /// Saved **escaped**, and restored escaped: the pair never leaves the
    /// domain, because `escape(unescape(s))` is not `s` in general — a
    /// round-trip through the unescaped form would corrupt a path containing
    /// any reserved character.
    saved_this_file: Escaped,
    saved_this_file_directory: Escaped,
}
fn walk_top_level(node: Node<'_, '_>, current_file_dir: &Path, state: &mut State<'_>) {
    match node.tag_name().name() {
        // Items belong to a later evaluation pass than properties: MSBuild
        // finalises every property (across the whole import graph) before
        // evaluating any `<ItemGroup>`. Record the group; the item pass
        // ([`replay_deferred_item_groups`]) evaluates it against the final
        // property table once the property pass has fully completed.
        "ItemGroup" => state.defer_item_group(node, DeferredGroupKind::ItemGroup),
        "PropertyGroup" => {
            // Plan D5: an unsupported condition treats the containing
            // group as *excluded*. We never silently include items —
            // but the same logic, applied to property writes, would
            // mark every project partial for the standard
            // "default-if-empty" idiom:
            //   <PropertyGroup Condition="'$(Configuration)' == ''">
            //     <Configuration>Debug</Configuration>
            //   </PropertyGroup>
            // When every child writes to a protected name, the writes
            // are already silently discarded regardless of the
            // condition; the group is effectively a no-op whether the
            // condition fires or not. Suppress diagnostics and skip
            // walking in that case — there's nothing the condition
            // could affect. (Empty groups trivially satisfy this.)
            let all_protected = node.children().filter(Node::is_element).all(|c| {
                state
                    .protected
                    .contains(&c.tag_name().name().to_ascii_lowercase())
            });
            if all_protected {
                return;
            }
            // A group that writes `<DefineConstants>` makes its own condition
            // *preprocessor-affecting*: an undefined/unmodeled condition here
            // (e.g. `'$(TargetFramework)' == 'net6.0'` in a multi-targeted
            // project) decides whether those defines apply. Mirror the
            // `compile_context` discipline (entry/user files only). Children
            // each manage their own context in `walk_property_child`.
            let prev = state.define_context;
            state.define_context =
                prev || (!state.in_sdk_subtree && property_group_writes_define_constants(node));
            // The same discipline for a group writing a CPM flag: its condition
            // decides whether Central Package Management turns on, or whether
            // the central versions import marker can be trusted. Setting
            // `package_context` across the evaluation makes an undefined /
            // unsupported condition flip `package_references_uncertain` via
            // `push` (a clean, defined-property condition raises no diagnostic,
            // so a genuinely-off opt-in stays certain). Reused rather than a
            // bespoke flag — `package_context` is exactly "a package decision
            // hangs on this". Reset before walking children (the write itself is
            // handled by the final-value check in `into_project`).
            let prev_pkg = state.package_context;
            let writes_cpm_flag = property_group_writes_cpm_flag(node);
            state.package_context = prev_pkg || writes_cpm_flag;
            if writes_cpm_flag {
                // A condition depending on an SDK property may evaluate cleanly
                // (no diagnostic) to false in this document-order walk, while
                // MSBuild's property pass would make it true before items run.
                state.note_package_uncertain_if_condition_uses_sdk_taint(node);
            }
            let group_condition_uses_sdk_taint = node
                .attribute("Condition")
                .is_some_and(|condition| state.raw_uses_sdk_package_taint(condition));
            let diagnostics_before_condition = state.diagnostics.len();
            let gate = evaluate_property_group_condition(node, current_file_dir, state);
            let group_condition_maybe_wrong = state.diagnostics.len()
                != diagnostics_before_condition
                || matches!(&gate, CondGate::Unsupported);
            let sdk_group_condition_taint = group_condition_uses_sdk_taint
                || (state.in_sdk_subtree && group_condition_maybe_wrong);
            // The root cause behind an unpinnable group gate — surfaced by
            // the condition evaluation just above — recorded against every
            // property the group writes (Run: the write may not happen in a
            // real build; Skip: it may).
            let group_unpinned_root = if group_condition_maybe_wrong {
                state
                    .unpinned_root_from_recent_diagnostics(diagnostics_before_condition)
                    .or_else(|| {
                        node.attribute("Condition")
                            .map(|c| UnpinnedRoot::UnsupportedCondition(c.to_string()))
                    })
            } else {
                None
            };
            match gate {
                CondGate::Run => {
                    state.define_context = prev;
                    state.package_context = prev_pkg;
                    for child in node.children().filter(Node::is_element) {
                        walk_property_child(
                            child,
                            sdk_group_condition_taint,
                            group_condition_maybe_wrong,
                            group_unpinned_root.as_ref(),
                            state,
                        );
                    }
                }
                CondGate::Skip => {
                    if group_condition_maybe_wrong || sdk_group_condition_taint {
                        // The gate could not be pinned down, so the group's
                        // writes may actually run in the real build — the
                        // written properties' final values are untrustworthy
                        // for package metadata. Taint them for the item
                        // pass's package reads, and unpin them so every
                        // item-pass read re-surfaces the root cause.
                        mark_property_group_children_provenance(
                            node,
                            group_unpinned_root.as_ref(),
                            state,
                        );
                    }
                    state.define_context = prev;
                    state.package_context = prev_pkg;
                }
                CondGate::Unsupported => {
                    mark_property_group_children_provenance(
                        node,
                        group_unpinned_root.as_ref(),
                        state,
                    );
                    emit_unsupported_condition(node, state);
                    state.define_context = prev;
                    state.package_context = prev_pkg;
                }
            }
        }
        "Import" => handle_import(node, current_file_dir, state),
        "ImportGroup" => {
            // The group's `Condition` gates its imports; an untrusted one in a
            // user file may hide imports that carry Compile items (same as a
            // bare `<Import>` condition). Compile uncertainty is gated to
            // non-SDK files, while package uncertainty applies in SDK files too
            // because skipped imports may hide dependency items.
            let prev_gate = state.import_gate_context;
            let prev_pkg_gate = state.package_import_gate_context;
            state.import_gate_context = prev_gate || !state.in_sdk_subtree;
            state.package_import_gate_context = true;
            let diagnostics_before_gate = state.diagnostics.len();
            let gate = evaluate_condition(node, current_file_dir, state);
            state.note_package_uncertain_if_condition_uses_sdk_taint(node);
            // Same reference-list rule as a bare `<Import>`'s own gate: an
            // untrusted decision here hides (or phantom-includes) whatever
            // the group's imports carry, `<ProjectReference>` mutations
            // included.
            if !state.in_sdk_subtree
                && (matches!(gate, CondGate::Unsupported)
                    || state.condition_reads_untrusted_value(node))
            {
                state.project_references_uncertain = true;
            }
            // Same rule as `follow_explicit_import`: an undecided group
            // gate may bring in or omit whole files of property writes.
            if state.diagnostics.len() != diagnostics_before_gate
                || matches!(gate, CondGate::Unsupported)
            {
                state.walk_opaque = true;
            }
            match gate {
                CondGate::Run => {
                    state.import_gate_context = prev_gate;
                    state.package_import_gate_context = prev_pkg_gate;
                    // MSBuild lets imports nest inside an <ImportGroup
                    // Condition="..."> wrapper. The group adds gating but
                    // nothing else we model — every Import inside still
                    // routes through `handle_import`.
                    for child in node.children().filter(Node::is_element) {
                        if child.tag_name().name() == "Import" {
                            handle_import(child, current_file_dir, state);
                        } else {
                            state.push(
                                DiagnosticKind::UnsupportedConstruct {
                                    element: child.tag_name().name().to_string(),
                                },
                                child.range(),
                            );
                        }
                    }
                }
                CondGate::Skip => {
                    state.import_gate_context = prev_gate;
                    state.package_import_gate_context = prev_pkg_gate;
                }
                CondGate::Unsupported => {
                    emit_unsupported_condition(node, state);
                    state.import_gate_context = prev_gate;
                    state.package_import_gate_context = prev_pkg_gate;
                }
            }
        }
        "Choose" => handle_choose(node, current_file_dir, state),
        // Item definitions belong to MSBuild's pass 2 — after every property
        // (pass 1), before any item (pass 3). Defer alongside `<ItemGroup>`s;
        // the replay runs all definition groups before any item group.
        "ItemDefinitionGroup" => {
            state.defer_item_group(node, DeferredGroupKind::ItemDefinitionGroup)
        }
        "Target" | "UsingTask" => {
            // Neither contributes to the *static* Compile item set —
            // `<Target>` items are build-time, `<UsingTask>` declares a
            // task. They flip `is_partial` only.
            state.push(
                DiagnosticKind::UnsupportedConstruct {
                    element: node.tag_name().name().to_string(),
                },
                node.range(),
            );
        }
        _ => {
            // Unknown top-level element: silently ignore. The MSBuild schema
            // is open; unknown extensions almost never affect Compile order.
            // (The top-level `<Sdk>` element is not one of these — the
            // pre-scan in [`walk_doc_body`] degrades it before any child
            // walks.)
        }
    }
}

/// `<Choose>`: a first-match-wins chain of condition-gated branches.
///
/// Semantics pinned against `dotnet msbuild` with per-case stub projects
/// (2026-07-09; docs/completed/sdk-chain-exactness-plan.md, Stage A):
///
///   * `<When>` conditions evaluate during the **property pass**, in
///     document order, against the table as it stands at the `<Choose>`'s
///     position. The first true gate wins; conditions after the match are
///     never evaluated (an MSBuild-illegal condition there does not even
///     error).
///   * The branch decision is **reused by the item pass**: the chosen
///     branch's `<ItemGroup>`s defer exactly like body groups (each still
///     evaluating its own `Condition` against the final table); the other
///     branches' contents are never looked at, and their writes never
///     land.
///   * A malformed shape — a `<When>` without `Condition` (MSB4035), an
///     `<Otherwise>` out of last position, multiple `<Otherwise>`s, a
///     stray child of the `<Choose>`, no `<When>` at all, or a
///     schema-illegal element *anywhere inside any branch* (MSB4067,
///     even a branch that would never be chosen — MSBuild validates the
///     whole tree at load time, pinned with stub projects) — fails the
///     real evaluation before anything runs; we degrade conservatively
///     without evaluating any part of the `<Choose>`.
///
/// A *reached* gate we cannot pin down (unsupported grammar, a read of an
/// undefined or tainted property) makes the decision itself
/// untrustworthy: nothing from that point on is descended. Every
/// still-possible branch's property writes are tainted/unpinned (any of
/// them may run in a real build), and the structural
/// [`StructuralCompileItemUncertainty::UnsupportedChoose`] skip applies
/// only when a still-possible branch could carry items — a
/// properties-only `<Choose>` (the SDK's pervasive `DefineConstants`
/// default-fill in `Microsoft.FSharp.NetSdk.props`) leaves item and
/// package certainty to the taint machinery instead of poisoning both
/// sets structurally.
fn handle_choose(node: Node<'_, '_>, current_file_dir: &Path, state: &mut State<'_>) {
    // Full-tree shape validation up front: MSBuild rejects a malformed
    // `<Choose>` at *load* time — before any evaluation, and regardless
    // of which branch would be chosen — so nothing from this element may
    // land when any part of it is malformed.
    if !choose_shape_is_valid(node) {
        state.push(
            DiagnosticKind::UnsupportedConstruct {
                element: "Choose".to_string(),
            },
            node.range(),
        );
        state.mark_structural_skip_respecting_sdk_compile_tolerance(
            StructuralCompileItemUncertainty::UnsupportedChoose,
            node.range(),
        );
        // A malformed Choose fails MSBuild's whole evaluation at load time —
        // there is no build for the captured reference list to describe. The
        // UnsupportedChoose carve-out in `mark_structural_skip` defers to the
        // undecided-branch mutation scan, which never runs on this path, so
        // set the flag structurally (same provenance gate as Compile).
        if !state.in_sdk_subtree {
            state.project_references_uncertain = true;
        }
        return;
    }
    let whens: Vec<Node<'_, '_>> = node
        .children()
        .filter(Node::is_element)
        .filter(|c| c.tag_name().name() == "When")
        .collect();
    let otherwise: Option<Node<'_, '_>> = node
        .children()
        .filter(Node::is_element)
        .find(|c| c.tag_name().name() == "Otherwise");

    // Mirror the `<PropertyGroup>` gate discipline while the `<When>`
    // conditions evaluate: a branch write to `DefineConstants` (or a CPM
    // flag) makes a gate preprocessor-/package-affecting, so `push`
    // attributes an undecidable gate's diagnostics to those axes. The
    // scan is recursive — a write inside a *nested* Choose is just as
    // much decided by this gate as a direct child's — and per-gate: by
    // the time gate `i` evaluates, branches before `i` were *cleanly
    // false*, so only writes in the still-possible branches (`i..` plus
    // `<Otherwise>`) can hang on it. Suffix-OR over per-branch scans.
    let writes_after = |pred: &dyn Fn(Node<'_, '_>) -> bool| -> Vec<bool> {
        let otherwise_writes = otherwise.is_some_and(|o| choose_branch_writes(o, pred));
        let mut suffix = vec![false; whens.len()];
        let mut acc = otherwise_writes;
        for (slot, when) in suffix.iter_mut().zip(&whens).rev() {
            acc = acc || choose_branch_writes(*when, pred);
            *slot = acc;
        }
        suffix
    };
    let defines_still_possible = writes_after(&property_group_writes_define_constants);
    let cpm_still_possible = writes_after(&property_group_writes_cpm_flag);
    let prev_define = state.define_context;
    let prev_package = state.package_context;

    let mut chosen: Option<Node<'_, '_>> = None;
    let mut undecided: Option<(usize, Option<UnpinnedRoot>)> = None;
    for (index, when) in whens.iter().enumerate() {
        state.define_context =
            prev_define || (!state.in_sdk_subtree && defines_still_possible[index]);
        state.package_context = prev_package || cpm_still_possible[index];
        if cpm_still_possible[index] {
            state.note_package_uncertain_if_condition_uses_sdk_taint(*when);
        }
        let uses_sdk_taint = when
            .attribute("Condition")
            .is_some_and(|condition| state.raw_uses_sdk_package_taint(condition));
        let diagnostics_before = state.diagnostics.len();
        let gate = evaluate_condition(*when, current_file_dir, state);
        let maybe_wrong =
            state.diagnostics.len() != diagnostics_before || matches!(gate, CondGate::Unsupported);
        if maybe_wrong || uses_sdk_taint {
            if matches!(gate, CondGate::Unsupported) {
                emit_unsupported_condition(*when, state);
            }
            let unpinned_root = state
                .unpinned_root_from_recent_diagnostics(diagnostics_before)
                .or_else(|| {
                    when.attribute("Condition")
                        .map(|c| UnpinnedRoot::UnsupportedCondition(c.to_string()))
                });
            undecided = Some((index, unpinned_root));
            break;
        }
        if matches!(gate, CondGate::Run) {
            chosen = Some(*when);
            break;
        }
        // A cleanly-false gate: MSBuild skips this branch too; keep going.
    }
    state.define_context = prev_define;
    state.package_context = prev_package;

    if let Some((from, unpinned_root)) = undecided {
        // Branches from the first unpinnable gate on may or may not run in
        // a real build. (Branches before it were cleanly false — MSBuild
        // skips those too, so leaving them untouched is exact.)
        let mut any_items = false;
        let mut reference_mutation = false;
        for branch in whens[from..].iter().copied().chain(otherwise) {
            scan_undecided_choose_branch(
                branch,
                unpinned_root.as_ref(),
                state,
                &mut any_items,
                &mut reference_mutation,
            );
        }
        if any_items {
            state.mark_structural_skip_respecting_sdk_compile_tolerance(
                StructuralCompileItemUncertainty::UnsupportedChoose,
                node.range(),
            );
        }
        // A still-possible branch mutating `<ProjectReference>` may run in
        // the real build, leaving earlier Includes captured un-mutated —
        // the list can't be trusted. (An Include-only branch we skip is at
        // worst a missed reference, so it does not poison the list.)
        if reference_mutation {
            state.project_references_uncertain = true;
        }
        return;
    }

    let Some(branch) = chosen.or(otherwise) else {
        // Every gate cleanly false and no <Otherwise>: an exact no-op.
        return;
    };
    for child in branch.children().filter(Node::is_element) {
        // `choose_shape_is_valid` guaranteed every branch child is a
        // `PropertyGroup`, `ItemGroup`, or nested `Choose`; each evaluates
        // exactly as if it appeared at the Choose's position.
        walk_top_level(child, current_file_dir, state);
    }
}

/// Whether a `<Choose>` subtree is shaped the way MSBuild's loader
/// requires: one or more `<When>`s each with a non-empty `Condition`
/// (MSB4035 calls an empty one out explicitly), at most one
/// `<Otherwise>` and only in last position, no attribute beyond the
/// recognized set (`<Choose>` and `<Otherwise>` take *none* — even
/// `Label` is MSB4066 there, pinned; `<When>` takes `Condition` and
/// `Label`), and branch children drawn exclusively from
/// `PropertyGroup` / `ItemGroup` / nested `Choose` (anything else is
/// MSB4067) — with nested `Choose`s held to the same rules. MSBuild
/// enforces all of this at load time, before evaluating anything, so
/// one violation anywhere invalidates the whole element.
///
/// Deliberately *not* validated: attributes on the branch children
/// themselves. `<PropertyGroup Foo="bar">` inside a branch is MSB4066
/// at load time — but it is exactly as much MSB4066 at top level
/// (pinned), where this walker has always ignored unknown group
/// attributes and evaluated best-effort. Holding branch groups to a
/// stricter standard than the same element one level up would be
/// inconsistent; modelling MSBuild's full load-time attribute schema
/// is an evaluator-wide question, out of scope for the Choose stage
/// (docs/completed/sdk-chain-exactness-plan.md, Stage A).
fn choose_shape_is_valid(node: Node<'_, '_>) -> bool {
    // `<When>` recognizes `Condition` and `Label`; `<Choose>` and
    // `<Otherwise>` recognize no attributes at all.
    let attrs_ok = |element: Node<'_, '_>, is_when: bool| {
        element
            .attributes()
            .all(|a| is_when && (a.name() == "Label" || a.name() == "Condition"))
    };
    if !attrs_ok(node, false) {
        return false;
    }
    let mut saw_when = false;
    let mut saw_otherwise = false;
    for child in node.children().filter(Node::is_element) {
        let branch = match child.tag_name().name() {
            "When" => {
                if saw_otherwise
                    || !attrs_ok(child, true)
                    || child
                        .attribute("Condition")
                        .is_none_or(|c| c.trim().is_empty())
                {
                    return false;
                }
                saw_when = true;
                child
            }
            "Otherwise" => {
                if saw_otherwise || !attrs_ok(child, false) {
                    return false;
                }
                saw_otherwise = true;
                child
            }
            _ => return false,
        };
        for grandchild in branch.children().filter(Node::is_element) {
            match grandchild.tag_name().name() {
                "PropertyGroup" | "ItemGroup" => {}
                "Choose" => {
                    if !choose_shape_is_valid(grandchild) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }
    saw_when
}

/// Whether any `<PropertyGroup>` reachable in this branch — directly or
/// through nested `<Choose>` branches — satisfies `pred`. Used to decide
/// whether a `<When>` gate is preprocessor- or package-affecting.
fn choose_branch_writes(branch: Node<'_, '_>, pred: &dyn Fn(Node<'_, '_>) -> bool) -> bool {
    branch
        .children()
        .filter(Node::is_element)
        .any(|child| match child.tag_name().name() {
            "PropertyGroup" => pred(child),
            "Choose" => child
                .children()
                .filter(Node::is_element)
                .any(|nested| choose_branch_writes(nested, pred)),
            _ => false,
        })
}

/// Taint every property write in an undecided `<Choose>` branch (the
/// write may or may not land in a real build, so its stored value — and
/// its absence — are both untrustworthy) and record whether the branch
/// could contribute items, and whether it could *mutate* the
/// `<ProjectReference>` list (Update/Remove — an Include is at worst a
/// missed reference, a mutation falsifies what's already captured).
fn scan_undecided_choose_branch(
    branch: Node<'_, '_>,
    unpinned_root: Option<&UnpinnedRoot>,
    state: &mut State<'_>,
    any_items: &mut bool,
    reference_mutation: &mut bool,
) {
    for child in branch.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "PropertyGroup" => {
                mark_property_group_children_provenance(child, unpinned_root, state);
            }
            "ItemGroup" => {
                *any_items = true;
                if item_pass::item_group_contains_project_reference_mutation(child) {
                    *reference_mutation = true;
                }
            }
            "Choose" => {
                // A nested Choose's branches are all undecided too.
                for nested in child.children().filter(Node::is_element) {
                    scan_undecided_choose_branch(
                        nested,
                        unpinned_root,
                        state,
                        any_items,
                        reference_mutation,
                    );
                }
            }
            // Not schema-legal here; treat as full-risk conservatively.
            _ => {
                *any_items = true;
                *reference_mutation = true;
            }
        }
    }
}

fn walk_property_child(
    node: Node<'_, '_>,
    inherited_sdk_package_taint: bool,
    inherited_condition_maybe_wrong: bool,
    inherited_unpinned_root: Option<&UnpinnedRoot>,
    state: &mut State<'_>,
) {
    let name = node.tag_name().name().to_string();
    let lower = name.to_ascii_lowercase();
    if state.protected.contains(&lower) {
        // Reserved (well-known) or caller-supplied (extra_properties) name.
        // MSBuild forbids the project from rebinding these — and crucially,
        // it discards the whole assignment *without* evaluating the
        // Condition or the value. Mirror that: emit no diagnostics for
        // either, since the project is using a standard idiom (e.g.
        // `<Configuration Condition="'$(Configuration)' == ''">Debug</…>`)
        // that we'd otherwise misreport as a partial-evaluation failure.
        return;
    }
    // A `<DefineConstants>` write feeds the `#if` symbol set, via both its
    // condition (which branch applies) and its value. Scope `define_context` to
    // the whole element (entry/user files only); `push` then flags any
    // uncertainty here — an unresolvable condition, an unsupported/item/metadata
    // value reference — except the `$(DefineConstants)` self-append, which
    // matches MSBuild (see `is_define_self_reference`). Single restore.
    let prev_define = state.define_context;
    state.define_context = prev_define || (lower == "defineconstants" && !state.in_sdk_subtree);
    // A CPM flag's own Condition and value are package-affecting for the same
    // reason as its containing group's Condition: if either is only evaluated by
    // treating unknown input as empty, the later inline CPM pass must retain a
    // non-CPM-specific cause after discharging `PackageVersion` / versionless
    // reference uncertainty.
    let prev_package = state.package_context;
    state.package_context = prev_package || is_cpm_flag_property_name(&name);
    walk_property_child_inner(
        node,
        name,
        lower,
        inherited_sdk_package_taint,
        inherited_condition_maybe_wrong,
        inherited_unpinned_root,
        state,
    );
    state.package_context = prev_package;
    state.define_context = prev_define;
}

fn walk_property_child_inner(
    node: Node<'_, '_>,
    name: String,
    lower: String,
    inherited_sdk_package_taint: bool,
    inherited_condition_maybe_wrong: bool,
    inherited_unpinned_root: Option<&UnpinnedRoot>,
    state: &mut State<'_>,
) {
    let had_prior_sdk_package_taint = state.sdk_package_property_is_tainted(&name);
    let condition_uses_sdk_taint = node
        .attribute("Condition")
        .is_some_and(|condition| state.raw_uses_sdk_package_taint(condition));
    let diagnostics_before_condition = state.diagnostics.len();
    let gate = evaluate_property_condition(node, state);
    let own_condition_maybe_wrong = state.diagnostics.len() != diagnostics_before_condition
        || matches!(&gate, CondGate::Unsupported);
    let write_condition_maybe_wrong = inherited_condition_maybe_wrong || own_condition_maybe_wrong;
    let condition_taints_property = inherited_sdk_package_taint
        || condition_uses_sdk_taint
        || (state.in_sdk_subtree && own_condition_maybe_wrong);
    match gate {
        CondGate::Run => {}
        CondGate::Skip => {
            // A write skipped under a gate our property pass could not pin
            // down (an undefined property the real build may supply, an
            // unsupported condition, or SDK-tainted input) may actually run
            // in the real build — so the property's final value is not
            // trustworthy. Taint it for package reads, and unpin it so every
            // item-pass read re-surfaces the gate's root cause.
            let taint = if condition_taints_property || own_condition_maybe_wrong {
                TaintOutcome::Set(node.range())
            } else {
                TaintOutcome::Keep
            };
            let unpinned = if own_condition_maybe_wrong {
                match state
                    .unpinned_root_from_recent_diagnostics(diagnostics_before_condition)
                    .or_else(|| {
                        node.attribute("Condition")
                            .map(|c| UnpinnedRoot::UnsupportedCondition(c.to_string()))
                    }) {
                    Some(root) => UnpinnedOutcome::Set(root),
                    None => UnpinnedOutcome::Keep,
                }
            } else {
                UnpinnedOutcome::Keep
            };
            state.apply_property_provenance(&name, &lower, PropertyProvenance { taint, unpinned });
            return;
        }
        CondGate::Unsupported => {
            let unpinned = match node.attribute("Condition") {
                Some(condition) => {
                    UnpinnedOutcome::Set(UnpinnedRoot::UnsupportedCondition(condition.to_string()))
                }
                None => UnpinnedOutcome::Keep,
            };
            state.apply_property_provenance(
                &name,
                &lower,
                PropertyProvenance {
                    taint: TaintOutcome::Set(node.range()),
                    unpinned,
                },
            );
            emit_unsupported_condition(node, state);
            return;
        }
    }
    // The element's value is its *full inner text* with literal-whitespace-only
    // text children dropped as insignificant — both rules, and the two shapes
    // whose value we cannot derive, live in [`collect_element_text`]. Empty
    // content (`<Foo/>`, `<Foo></Foo>`, `<Foo> </Foo>`) yields the empty string
    // — distinct from "undefined", and used in real projects to clear an
    // inherited value.
    let Some(raw) = collect_element_text(node) else {
        // A body we cannot model (CDATA, entity-encoded whitespace). Same
        // treatment as an unevaluable expansion below: drop any prior binding
        // rather than let a stale value stand in for a write whose result we
        // don't know, and unpin the name so every later read re-surfaces it.
        state.push(
            DiagnosticKind::UnsupportedPropertyExpression {
                expression: format!("<{name}> body"),
            },
            node.range(),
        );
        state.lookup.remove(&name);
        state.written.remove(&lower);
        state.record_directory_build_path_write(&name);
        state.apply_property_provenance(
            &name,
            &lower,
            PropertyProvenance {
                taint: TaintOutcome::Set(node.range()),
                unpinned: UnpinnedOutcome::Set(UnpinnedRoot::UnsupportedCondition(format!(
                    "<{name}> body"
                ))),
            },
        );
        return;
    };
    // While `define_context` is set (a user `<DefineConstants>` write), any
    // uncertainty `state.expand`/`push` raises here flags
    // `define_constants_uncertain` — except a bare `$(DefineConstants)` self-
    // append (see `is_define_self_reference`), which is recognised only because
    // `in_define_value` is set: an unsupported expression / item / metadata ref
    // diverges (residual/refused), an undefined non-self ref may be set in the
    // real build, and an undefined self-ref matches MSBuild.
    let prev_in_value = state.in_define_value;
    state.in_define_value = true;
    let value_uses_sdk_taint = state.raw_uses_sdk_package_taint(&raw);
    let expansion = state.expand_property_pass_value(&raw, node.range(), &name);
    state.in_define_value = prev_in_value;
    let value_taints_property = condition_taints_property
        || value_uses_sdk_taint
        || (state.in_sdk_subtree && expansion.had_issue());
    let preserve_existing_sdk_taint =
        had_prior_sdk_package_taint && (write_condition_maybe_wrong || expansion.had_issue());
    if expansion.had_unsupported {
        // The substituted value contains residual `$(...)` text from an
        // expression we couldn't evaluate. Storing it would smuggle
        // unevaluated MSBuild syntax into downstream paths. But we also
        // can't leave any *prior* binding in place: MSBuild would have
        // overwritten the old value with whatever the new expression
        // produced, never the old value, so trusting the stale entry
        // would diverge from MSBuild just as much as trusting the
        // residual. Remove any existing binding so the next reference
        // emits Undefined, and forget any prior project-side write so
        // the tainted name doesn't appear in `properties`.
        state.lookup.remove(&name);
        state.written.remove(&lower);
        // The real build stores the value we refused to compute, so later
        // undefined reads of this name are never exact (C.2b).
        state.unevaluable_written.insert(lower.clone());
        state.record_directory_build_path_write(&name);
        let taint = TaintOutcome::after_write(
            value_taints_property || state.in_sdk_subtree,
            node.range(),
            preserve_existing_sdk_taint,
        );
        state.apply_property_provenance(
            &name,
            &lower,
            PropertyProvenance {
                taint,
                unpinned: UnpinnedOutcome::Clear,
            },
        );
        return;
    }
    // Item-list (`@(Items)`) and metadata (`%(Identity)`) references
    // survive substitution untouched — they're not `$(...)`. The
    // properties map is documented as evaluated project state, so
    // recording an unevaluated `@(...)` here would silently lie. Mirror
    // the diagnostics path used for Include attributes and refuse the
    // write. We diagnose item refs first to match how Include
    // attributes order the checks; the categorisation matters less than
    // emitting *some* diagnostic that flips `is_partial`.
    if contains_item_reference(expansion.value.as_escaped()) {
        state.push(
            DiagnosticKind::UnresolvedItemReference {
                reference: expansion.value.as_escaped().to_string(),
            },
            node.range(),
        );
        state.lookup.remove(&name);
        state.written.remove(&lower);
        // The real build stores the value we refused to compute, so later
        // undefined reads of this name are never exact (C.2b).
        state.unevaluable_written.insert(lower.clone());
        state.record_directory_build_path_write(&name);
        let taint = TaintOutcome::after_write(
            value_taints_property || state.in_sdk_subtree,
            node.range(),
            true,
        );
        state.apply_property_provenance(
            &name,
            &lower,
            PropertyProvenance {
                taint,
                unpinned: UnpinnedOutcome::Clear,
            },
        );
        return;
    }
    if contains_metadata_reference(expansion.value.as_escaped()) {
        state.push(
            DiagnosticKind::UnresolvedMetadataReference {
                reference: expansion.value.as_escaped().to_string(),
            },
            node.range(),
        );
        state.lookup.remove(&name);
        state.written.remove(&lower);
        // The real build stores the value we refused to compute, so later
        // undefined reads of this name are never exact (C.2b).
        state.unevaluable_written.insert(lower.clone());
        state.record_directory_build_path_write(&name);
        let taint = TaintOutcome::after_write(
            value_taints_property || state.in_sdk_subtree,
            node.range(),
            true,
        );
        state.apply_property_provenance(
            &name,
            &lower,
            PropertyProvenance {
                taint,
                unpinned: UnpinnedOutcome::Clear,
            },
        );
        return;
    }
    // Pin bookkeeping: a value that leaned on an undefined reference or an
    // unpinned property records that root; a clean value written under a
    // gate we couldn't pin down (its own, or the containing group's)
    // records the GATE's root — the write may not happen in a real build,
    // leaving a different value live. A clean value under a clean gate
    // re-pins.
    let gate_unpinned_root = if own_condition_maybe_wrong {
        state
            .unpinned_root_from_recent_diagnostics(diagnostics_before_condition)
            .or_else(|| {
                node.attribute("Condition")
                    .map(|c| UnpinnedRoot::UnsupportedCondition(c.to_string()))
            })
    } else if inherited_condition_maybe_wrong {
        inherited_unpinned_root.cloned()
    } else {
        None
    };
    let unpinned_by = expansion.unpinned_root.clone().or(gate_unpinned_root);
    // Unlike the unevaluable-value paths above, a cleanly-expanded value
    // under a pinned gate is exact even inside an SDK file: the property
    // pass computes the same final value MSBuild would, so plain SDK
    // provenance is not a taint. Only `value_taints_property`'s targeted
    // conditions (an untrusted gate, tainted input, or an expansion issue
    // inside the SDK) poison the write.
    let provenance = PropertyProvenance {
        taint: TaintOutcome::after_write(
            value_taints_property,
            node.range(),
            preserve_existing_sdk_taint,
        ),
        unpinned: UnpinnedOutcome::after_write(unpinned_by, write_condition_maybe_wrong),
    };
    state.lookup.insert_escaped(name.clone(), expansion.value);
    state.written.insert(lower.clone(), name.clone());
    state.record_directory_build_path_write(&name);
    state.apply_property_provenance(&name, &lower, provenance);
}

/// Result of evaluating a node's `Condition` attribute.
enum CondGate {
    /// No condition, or a condition that evaluated to true. Walk the
    /// node's contents.
    Run,
    /// Condition evaluated to false. Skip the node silently — MSBuild
    /// would have done the same, and a "successful exclusion" is not
    /// a divergence to report.
    Skip,
    /// Condition couldn't be evaluated within our grammar (plan D5
    /// "fail loudly"). Caller must emit
    /// [`DiagnosticKind::UnsupportedCondition`] AND skip the node —
    /// proceeding as if true would silently leak items / properties
    /// MSBuild might have excluded.
    Unsupported,
}

fn evaluate_condition(
    node: Node<'_, '_>,
    current_file_dir: &Path,
    state: &mut State<'_>,
) -> CondGate {
    evaluate_condition_with_exemptions(node, current_file_dir, state, &[])
}

/// The `<PropertyGroup>` variant of [`evaluate_condition`]: the names the
/// group's children write are default-fill exempt in the group's own
/// condition — see [`evaluate_condition_with_exemptions`].
fn evaluate_property_group_condition(
    node: Node<'_, '_>,
    current_file_dir: &Path,
    state: &mut State<'_>,
) -> CondGate {
    let written_names: Vec<String> = node
        .children()
        .filter(Node::is_element)
        .map(|child| child.tag_name().name().to_string())
        .collect();
    evaluate_condition_with_exemptions(node, current_file_dir, state, &written_names)
}

/// `self_default_names`: property names whose *undefined* reads in this
/// condition are the MSBuild default-fill idiom —
/// `<X Condition="'$(X)' == ''">default</X>` (or the group-level
/// `<PropertyGroup Condition="'$(Configuration)' == ''">` variant) — and
/// therefore deterministic rather than a divergence risk: under this
/// walker's environment model (caller `extra_properties` ARE the
/// environment; a name absent from them and never written is genuinely
/// unset — the same model `should_import_default_true` commits to for the
/// `Directory.Build.*` gates) an undefined self-name means the default
/// fires, exactly as in a real build. Only the *undefined* case is
/// exempt: a defined-but-unpinned self-name still re-surfaces its root
/// below, because there the stored value itself is untrustworthy.
/// The SDK relies on this idiom pervasively (`NuGet.props` gates every
/// `Directory.Packages.props` discovery property with it), and flagging
/// it would leave those writes unpinned and the central import refused.
fn evaluate_condition_with_exemptions(
    node: Node<'_, '_>,
    current_file_dir: &Path,
    state: &mut State<'_>,
    self_default_names: &[String],
) -> CondGate {
    let Some(cond) = node.attribute("Condition") else {
        return CondGate::Run;
    };
    let mut eval = if state.follow_imports {
        let exists = |path: &str| condition_exists(path, current_file_dir);
        condition::evaluate_with_exists(cond, &state.lookup, &exists)
    } else {
        condition::evaluate(cond, &state.lookup)
    };
    // Two carve-outs from the exemption. `DefineConstants`: the define
    // machinery deliberately does not model the SDK's own define
    // manipulation, so an undefined `$(DefineConstants)` here does NOT
    // mean "empty in a real build" (the SDK sets e.g. DEBUG) — see
    // `define_constants_self_reference_in_a_condition_is_uncertain`.
    // And any occurrence *outside* an empty-literal comparison
    // (`'$(X)' != 'bar'`, `Exists('$(X)')`, …): only the is-it-set shape
    // is the default-fill idiom; everything else is a genuine branch
    // decision on the unknown value.
    let outside_empty_comparison = eval.undefined_outside_empty_comparison.clone();
    eval.undefined_properties.retain(|name| {
        name.eq_ignore_ascii_case("DefineConstants")
            || outside_empty_comparison
                .iter()
                .any(|n| n.eq_ignore_ascii_case(name))
            || !self_default_names
                .iter()
                .any(|written| written.eq_ignore_ascii_case(name))
    });
    // C.2b: an undefined name the walk can prove is undefined in the
    // real build too substitutes to exactly the "" the evaluation used —
    // the True/False outcome is exact, not a divergence. Drop such names
    // before the carve-outs and diagnostics below so the gate reads as
    // trusted. (`DefineConstants` and `TargetFramework` never pass the
    // guard — see `undefined_read_is_exact`'s carve-outs.)
    eval.undefined_properties
        .retain(|name| !state.undefined_read_is_exact(name));
    // A condition reading an *unpinned* property (see
    // [`State::unpinned_value_properties`]) may take the other branch in a
    // real build — the same risk as a direct undefined reference, invisible
    // to `condition::evaluate` because the property is defined. Collect the
    // roots first so both the diagnostics and the compile carve-outs below
    // see them; downstream maybe-wrong detection (which watches the
    // diagnostics emitted here) then treats the gate accordingly.
    let mut unpinned_roots: Vec<UnpinnedRoot> = Vec::new();
    for reference in simple_property_references(cond) {
        let Some(root) = state
            .unpinned_value_properties
            .get(&reference.to_ascii_lowercase())
            .cloned()
        else {
            continue;
        };
        if let UnpinnedRoot::Undefined(name) = &root
            && eval
                .undefined_properties
                .iter()
                .any(|n| n.eq_ignore_ascii_case(name))
        {
            continue;
        }
        if !unpinned_roots.contains(&root) {
            unpinned_roots.push(root);
        }
    }
    // Inside a Compile context this condition decides whether source files
    // compile, and it only resolved by treating unknown properties as "" (or
    // leaning on an unpinned value) — so the include/exclude verdict may be
    // wrong. Record the correctness carve-out (separately surfaceable, e.g.
    // by the LSP's compile-uncertainty warning) *before* the flat
    // per-property diagnostics below.
    if state.compile_context {
        let mut undefined_names = eval.undefined_properties.clone();
        let mut unpinned_unsupported = false;
        for root in &unpinned_roots {
            match root {
                UnpinnedRoot::Undefined(name) => undefined_names.push(name.clone()),
                UnpinnedRoot::UnsupportedCondition(_) => unpinned_unsupported = true,
            }
        }
        if !undefined_names.is_empty() {
            state.record_compile_condition_uncertainty(
                cond,
                CompileConditionReason::UndefinedProperties(undefined_names),
                node.range(),
            );
        }
        if unpinned_unsupported {
            state.record_compile_condition_uncertainty(
                cond,
                CompileConditionReason::Unsupported,
                node.range(),
            );
        }
    }
    // Surface every undefined `$(...)` reference the condition relied
    // on as an UndefinedProperty diagnostic. Plan D5: a condition we
    // *could* evaluate but only by treating an unknown property as ""
    // is a divergence risk — MSBuild may have the value, we don't —
    // and the user needs to know. We only emit these for True/False;
    // for Unsupported the `condition::evaluate` contract guarantees
    // an empty list, since the caller is about to emit
    // UnsupportedCondition which subsumes the per-property concern.
    // Unpinned roots surface alongside, for the same reason.
    for root in unpinned_roots {
        state.push(root.to_diagnostic(), node.range());
    }
    for name in eval.undefined_properties {
        state.push(DiagnosticKind::UndefinedProperty { name }, node.range());
    }
    match eval.outcome {
        condition::Outcome::True => CondGate::Run,
        condition::Outcome::False => CondGate::Skip,
        condition::Outcome::Unsupported => CondGate::Unsupported,
    }
}

fn evaluate_property_condition(node: Node<'_, '_>, state: &mut State<'_>) -> CondGate {
    // MSBuild evaluates individual property-element conditions relative to the
    // entry project directory, even when the property was declared in an
    // imported props/targets file. This is deliberately not the same as the
    // containing `PropertyGroup` condition.
    let base_dir = state.entry_project_dir.clone();
    // The element's own name is default-fill exempt in its own condition
    // (`<X Condition="'$(X)' == ''">…`) — see
    // `evaluate_condition_with_exemptions`.
    let self_name = [node.tag_name().name().to_string()];
    evaluate_condition_with_exemptions(node, &base_dir, state, &self_name)
}

fn condition_exists(path: &str, current_file_dir: &Path) -> bool {
    if path.is_empty() {
        return false;
    }
    let normalised = path.replace('\\', "/");
    let path = Path::new(&normalised);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_file_dir.join(path)
    };
    resolved.exists()
}

fn emit_unsupported_condition(node: Node<'_, '_>, state: &mut State<'_>) {
    // The diagnostic carries the raw condition text so callers can
    // surface it to users without re-parsing — see
    // `DiagnosticKind::UnsupportedCondition`. We only reach this
    // path when `evaluate_condition` returned `Unsupported`, so the
    // attribute is present by construction.
    let cond = node.attribute("Condition").unwrap_or("");
    // In a Compile context the unmodeled condition was treated as exclusionary,
    // so any gated Compile items were dropped — possibly wrongly. Record the
    // carve-out so a consumer can surface *why* the Compile set is incomplete.
    if state.compile_context {
        state.record_compile_condition_uncertainty(
            cond,
            CompileConditionReason::Unsupported,
            node.range(),
        );
    }
    state.push(
        DiagnosticKind::UnsupportedCondition {
            condition: cond.to_string(),
        },
        node.range(),
    );
}

/// Names MSBuild defines *before* evaluation starts from toolset or host
/// facts we don't model, so "undefined in our walk" never means "empty in
/// the real build". The `msbuild` prefix covers the whole reserved +
/// toolset + behavioural-variable family (`MSBuildBinPath`,
/// `MSBuildUserExtensionsPath`, `MSBUILDDISABLEFEATURESFROMVERSION`, …) —
/// over-matching a user property that merely starts with "msbuild" costs
/// a spurious degrade, never a wrong commit. `OS` is here for non-unix
/// hosts only (unix seeds it exactly); `DOTNET_HOST_PATH` is injected by
/// the dotnet muxer for its children, so a build run via `dotnet` sees it
/// even when the caller's snapshot lacks it.
///
/// One pinned exception to the prefix: `MSBuildIsRestoring` (probed:
/// `[$(MSBuildIsRestoring)]` on the build/`-getProperty` entrypoint reads
/// empty under a scrubbed environment). NuGet's *restore* entrypoint
/// injects it as a global, but this walker models the build-time
/// evaluation — the same one the `-getItem` differential oracle sees —
/// where the name is genuinely undefined. `NuGet.props` gates early
/// import groups on it, so keeping it conservative would latch
/// [`State::walk_opaque`] at the top of every real SDK chain and blind
/// the entire downstream walk.
fn is_toolset_initial_property_name(lower: &str) -> bool {
    if lower == "msbuildisrestoring" {
        return false;
    }
    lower.starts_with("msbuild")
        || matches!(
            lower,
            "visualstudioversion" | "roslyntargetspath" | "os" | "dotnet_host_path"
        )
}

/// Toolset-computed property names MSBuild *overwrites* after folding the
/// environment in, so a same-named environment variable is invisible to
/// projects. Probed one name at a time against dotnet msbuild 10.0.300
/// (spoof each via the environment, read it back): these read their real
/// toolset values, while — notably — `MSBuildExtensionsPath{,32,64}`,
/// `MSBuildUserExtensionsPath`, `MSBuildSDKsPath`,
/// `MSBuildFrameworkToolsRoot`, `MSBuildAllProjects`,
/// `MSBuildSemanticVersion`, `MSBuildFileVersion`, `VisualStudioVersion`,
/// and `OS` all honour the spoof, so they must stay promotable. We do not
/// seed these names with their real values (they are host/toolset facts
/// this crate cannot know), so reads surface as undefined — conservative.
/// Lowercase, matched against the lowercased variable name: the property
/// table is case-insensitive, so the toolset overwrite displaces every
/// spelling.
///
/// `DOTNET_HOST_PATH` is here for the same reason one step earlier in the
/// pipeline: the `dotnet` *host* rewrites it to the muxer it actually selected
/// before MSBuild is even loaded, so the value our caller inherited never
/// reaches the evaluation. Probed: `DOTNET_HOST_PATH=SPOOF dotnet msbuild …`
/// reads the real dotnet path, not `SPOOF`. The F# SDK's own targets read the
/// property, so promoting the inherited value would commit a wrong path.
/// What the caller's environment snapshot said about `MSBuildExtensionsPath`
/// (see [`State::env_extensions_path`]). MSBuild promotes the name from the
/// environment, so it is *not* in [`is_env_ignored_toolset_name`] — but
/// whether the promoted value then survives the toolset depends on the
/// toolset's version, which is unknown until an SDK resolves. So the value is
/// parked here and adjudicated in [`State::seed_toolset_properties`].
enum EnvExtensionsPath {
    /// The snapshot did not bind the name. Every toolset then computes the
    /// property from its own directory, so there is nothing version-specific
    /// to decide.
    Absent,
    /// The snapshot bound it exactly once, to this (escaped-domain) value.
    Value(String),
    /// The snapshot bound it under two or more spellings, and MSBuild's pick
    /// among case-colliding variables is unspecified (probed — see
    /// [`State::new`]). A toolset that overwrites the value makes the
    /// collision moot; one that honours it leaves us unable to say which
    /// value won.
    Unspecified,
}

/// Whether this toolset lets an *environment-supplied* `MSBuildExtensionsPath`
/// stand, keyed on the major version of a canonical .NET SDK version directory
/// (`…/sdk/<version>`). `None` when the directory does not name a version we
/// can read — we then know nothing about the toolset, and both answers would
/// be a guess.
///
/// MSBuild ≤ 17 promotes the environment value and then *overwrites* it with
/// the toolset's own directory before the project is evaluated; MSBuild 18
/// leaves it standing, so it steers the `Sdk.props` import of
/// `$(MSBuildExtensionsPath)\$(MSBuildToolsVersion)\Microsoft.Common.props`.
/// Probed one SDK at a time —
/// `MSBuildExtensionsPath=/SPOOF dotnet msbuild -getProperty:MSBuildExtensionsPath`
/// reads the SDK version directory under 8.0.420 (MSBuild 17.11) and 9.0.315,
/// and `/SPOOF` under 10.0.301 (MSBuild 18.6). Not ChangeWave-gated (probed:
/// `MSBUILDDISABLEFEATURESFROMVERSION=17.0` does not restore the old behaviour
/// on 10.0.301), so the SDK's major version is the whole signal. Newer SDKs
/// are assumed to keep MSBuild 18's behaviour: it is the one that *respects*
/// its input, and a revert would be a breaking change.
///
/// The sibling names are not version-specific — the same sweep found
/// `MSBuildExtensionsPath32`, `MSBuildExtensionsPath64`,
/// `MSBuildUserExtensionsPath`, `MSBuildSDKsPath` and `VisualStudioVersion`
/// honouring the spoof on both 8.0.420 and 10.0.301.
fn toolset_honours_env_extensions_path(version_dir: &Path) -> Option<bool> {
    let major: u32 = version_dir
        .file_name()?
        .to_str()?
        .split('.')
        .next()?
        .parse()
        .ok()?;
    Some(major >= 10)
}

fn is_env_ignored_toolset_name(lower: &str) -> bool {
    matches!(
        lower,
        "msbuildbinpath"
            | "msbuildtoolspath"
            | "msbuildtoolsversion"
            | "msbuildruntimetype"
            | "msbuildassemblyversion"
            | "msbuildversion"
            | "msbuildprogramfiles32"
            | "msbuildnodecount"
            | "msbuildstartupdirectory"
            | "msbuildloadmicrosofttargetsreadonly"
            | "roslyntargetspath"
            | "dotnet_host_path"
    )
}

/// The names MSBuild treats as *reserved*, transcribed from
/// `ReservedPropertyNames.ReservedProperties` (dotnet/msbuild
/// `src/Build/Resources/Constants.cs`; the set is built with
/// `MSBuildNameIgnoreCaseComparer`, hence the lowercase match).
///
/// `Utilities.GetEnvironmentProperties` filters the environment against this
/// whole set — `!ReservedPropertyNames.IsReservedProperty(name)` — so a
/// reserved name in the environment is never promoted, *including* names this
/// crate does not model and leaves undefined. Filtering only on the names we
/// happen to seed would let a spoofed `MSBuildThisFileFullPath` become
/// readable and commit an import path the real build never sees.
///
/// Deliberately absent, and so still promotable (each probed): `OS`,
/// `MSBuildExtensionsPath{,32,64}`, `MSBuildUserExtensionsPath`,
/// `MSBuildSDKsPath`, `MSBuildFrameworkToolsRoot`, `LocalAppData`,
/// `VisualStudioVersion` — MSBuild deliberately keeps these overridable from
/// the environment.
fn is_msbuild_reserved_name(lower: &str) -> bool {
    matches!(
        lower,
        "msbuildprojectdirectory"
            | "msbuildprojectdirectorynoroot"
            | "msbuildprojectfile"
            | "msbuildprojectextension"
            | "msbuildprojectfullpath"
            | "msbuildprojectname"
            | "msbuildthisfiledirectory"
            | "msbuildthisfiledirectorynoroot"
            | "msbuildthisfile"
            | "msbuildthisfileextension"
            | "msbuildthisfilefullpath"
            | "msbuildthisfilename"
            | "msbuildbinpath"
            | "msbuildprojectdefaulttargets"
            | "msbuildtoolspath"
            | "msbuildtoolsversion"
            | "msbuildruntimetype"
            | "msbuildstartupdirectory"
            | "msbuildnodecount"
            | "msbuildlasttaskresult"
            | "msbuildprogramfiles32"
            | "msbuildassemblyversion"
            | "msbuildversion"
            | "msbuildinteractive"
            | "msbuilddisablefeaturesfromversion"
    )
}

/// The `MSBUILDDISABLEFEATURESFROMVERSION` variable's value, looked up the way
/// the host would.
///
/// MSBuild reads it through `Environment.GetEnvironmentVariable`, whose name
/// lookup is case-sensitive on Unix but case-insensitive on Windows. Taking
/// `case_insensitive` as an argument rather than reading `cfg!(windows)` here
/// keeps both host behaviours testable from either host.
///
/// On Windows the environment block cannot hold two names differing only in
/// case, so the case-insensitive arm cannot be ambiguous.
pub(crate) fn changewave_env_value(
    environment: &HashMap<String, String>,
    case_insensitive: bool,
) -> Option<&str> {
    const NAME: &str = "MSBUILDDISABLEFEATURESFROMVERSION";
    if case_insensitive {
        environment
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(NAME))
            .map(|(_, value)| value.as_str())
    } else {
        environment.get(NAME).map(String::as_str)
    }
}

/// Whether a [`DiagnosticKind`] can itself carry Compile items into the
/// project, so emitting it makes the Compile set untrustworthy *even outside*
/// a [`State::compile_context`]. A failed import or unresolved SDK may have
/// contributed `<Compile>` items — or properties that gate them — we never saw.
fn is_structural_compile_risk(kind: &DiagnosticKind) -> bool {
    matches!(
        kind,
        DiagnosticKind::ImportFailed { .. }
            | DiagnosticKind::UnresolvedImport { .. }
            | DiagnosticKind::SdkNotFound { .. }
            | DiagnosticKind::SdkVersionNotSatisfied { .. }
            | DiagnosticKind::SdkResolutionUnsupported { .. }
            | DiagnosticKind::ImplicitImportPresent { .. }
    )
}

fn package_structural_uncertainty_from_compile(
    kind: &StructuralCompileItemUncertainty,
) -> StructuralPackageReferenceUncertainty {
    match kind {
        StructuralCompileItemUncertainty::ProjectSdkUnsupported { sdk } => {
            StructuralPackageReferenceUncertainty::ProjectSdkUnsupported { sdk: sdk.clone() }
        }
        StructuralCompileItemUncertainty::ExplicitSdkUnsupported { sdk } => {
            StructuralPackageReferenceUncertainty::ExplicitSdkUnsupported { sdk: sdk.clone() }
        }
        StructuralCompileItemUncertainty::SdkImportProjectUnresolved { sdk, project } => {
            StructuralPackageReferenceUncertainty::SdkImportProjectUnresolved {
                sdk: sdk.clone(),
                project: project.clone(),
            }
        }
        StructuralCompileItemUncertainty::SdkImportProjectRejected { sdk, project } => {
            StructuralPackageReferenceUncertainty::SdkImportProjectRejected {
                sdk: sdk.clone(),
                project: project.clone(),
            }
        }
        StructuralCompileItemUncertainty::ImportProjectUnresolved { project } => {
            StructuralPackageReferenceUncertainty::ImportProjectUnresolved {
                project: project.clone(),
            }
        }
        StructuralCompileItemUncertainty::UnsupportedChoose => {
            StructuralPackageReferenceUncertainty::UnsupportedChoose
        }
    }
}

/// Return the SDK version directory for the canonical .NET SDK
/// `…/sdk/<version>/Sdks/<name>/Sdk` layout. Only that layout justifies
/// tolerating shared SDK files outside the specific `Sdks/<name>` subtree (see
/// [`State::note_sdk_tolerance`]). Case-insensitive, matching the host FS.
fn dotnet_sdk_version_dir(root: &Path) -> Option<&Path> {
    if !root
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("Sdk"))
    {
        return None;
    }
    let sdk_name_dir = root.parent()?;
    let sdks_dir = sdk_name_dir.parent()?;
    if !sdks_dir
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("Sdks"))
    {
        return None;
    }
    let version_dir = sdks_dir.parent()?;
    let sdk_dir = version_dir.parent()?;
    if !sdk_dir
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("sdk"))
    {
        return None;
    }
    Some(version_dir)
}

/// Whether a diagnostic is just a `$(DefineConstants)` self-reference resolving
/// to "" — the accumulator append idiom (`$(DefineConstants);FOO`), which
/// matches MSBuild and so must not flag [`State::define_constants_uncertain`].
/// Every *other* undefined/unsupported reference in a define value or condition
/// is a genuine divergence (the referenced property may be set in the real
/// build, or the expression evaluated).
fn is_define_self_reference(kind: &DiagnosticKind) -> bool {
    matches!(
        kind,
        DiagnosticKind::UndefinedProperty { name } if name.eq_ignore_ascii_case("DefineConstants")
    )
}

/// Whether a `<PropertyGroup>` writes `<DefineConstants>` — the trigger for
/// treating the group's own condition as preprocessor-affecting (case-
/// insensitive, matching MSBuild's property-name comparison).
fn property_group_writes_define_constants(node: Node<'_, '_>) -> bool {
    node.children()
        .filter(Node::is_element)
        .any(|c| c.tag_name().name().eq_ignore_ascii_case("DefineConstants"))
}

/// Whether a `<PropertyGroup>` writes a Central Package Management flag. A write
/// under an unevaluable condition leaves CPM state unknown, which changes how
/// the package set's versions are interpreted, so the group's condition is
/// package-affecting (case-insensitive, matching MSBuild's property-name
/// comparison).
fn property_group_writes_cpm_flag(node: Node<'_, '_>) -> bool {
    node.children()
        .filter(Node::is_element)
        .any(|c| is_cpm_flag_property_name(c.tag_name().name()))
}

fn is_cpm_flag_property_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("ManagePackageVersionsCentrally")
        || name.eq_ignore_ascii_case("CentralPackageVersionsFileImported")
        || name.eq_ignore_ascii_case("CentralPackageVersionOverrideEnabled")
}

/// Whether a property-group child could write at all if its group ran.
/// `false` only on exact knowledge: the child's own condition evaluates
/// cleanly false — defined properties, supported grammar, no unpinned
/// reads — so the write cannot happen regardless of the group gate.
fn property_child_could_write(child: Node<'_, '_>, state: &State<'_>) -> bool {
    let Some(cond) = child.attribute("Condition") else {
        return true;
    };
    let mut eval = if state.follow_imports {
        let exists = |path: &str| condition_exists(path, &state.entry_project_dir);
        condition::evaluate_with_exists(cond, &state.lookup, &exists)
    } else {
        condition::evaluate(cond, &state.lookup)
    };
    // C.2b: an undefined read the walk can prove is undefined in the real
    // build too substitutes to exactly "", so the outcome that used it is
    // exact — the same exemption `evaluate_condition_with_exemptions`
    // applies at the read sites. Without it a child gated on a provably-false
    // `'$(NoSuch)' != ''` reads as maybe-writing and needlessly unpins the
    // property, which downstream (e.g. `property_provenance_untrusted`) then
    // refuses on.
    eval.undefined_properties
        .retain(|name| !state.undefined_read_is_exact(name));
    !(eval.outcome == condition::Outcome::False
        && eval.undefined_properties.is_empty()
        && state.unpinned_root_for_raw(cond).is_none())
}

/// Mark every unprotected, possibly-writing child of a maybe-run/maybe-skipped
/// `<PropertyGroup>` with the group gate's provenance in one pass, so the two
/// channels cannot diverge on which children they cover: SDK-package taint
/// always (the gate could not be pinned down, so a child's final value is
/// untrustworthy for a later package read whichever branch our evaluation
/// took), and the unpinned `unpinned_root` when the gate itself was
/// unpinnable (so every item-pass read re-surfaces its root cause).
///
/// Protected names are exempt: MSBuild discards those writes without
/// consulting the gate at all, so a maybe-skipped write cannot change them —
/// marking one would falsely degrade a value the caller pinned. Children whose
/// own condition is cleanly false are exempt too — they cannot write whichever
/// way the group gate goes (see [`property_child_could_write`]).
fn mark_property_group_children_provenance(
    node: Node<'_, '_>,
    unpinned_root: Option<&UnpinnedRoot>,
    state: &mut State<'_>,
) {
    for child in node.children().filter(Node::is_element) {
        if !property_child_could_write(child, state) {
            continue;
        }
        let name = child.tag_name().name();
        if state.protected.contains(&name.to_ascii_lowercase()) {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        state.apply_property_provenance(
            name,
            &lower,
            PropertyProvenance {
                taint: TaintOutcome::Set(child.range()),
                unpinned: match unpinned_root {
                    Some(root) => UnpinnedOutcome::Set(root.clone()),
                    None => UnpinnedOutcome::Keep,
                },
            },
        );
    }
}

pub(crate) fn simple_property_references(raw: &str) -> impl Iterator<Item = &str> {
    let is_ws = |c: char| c.is_ascii_whitespace();
    let mut refs = Vec::new();
    let mut search_from = 0;
    while let Some(relative_idx) = raw[search_from..].find("$(") {
        let open = search_from + relative_idx + 2;
        // Always resume just past this opener: each iteration then makes
        // progress regardless of what follows, and the whitespace-tolerant
        // scan below no longer has to compute an exact skip offset.
        search_from = open;
        // MSBuild tolerates whitespace inside `$( … )` — around the name and
        // before the `(`/`)` — and the condition tokeniser trims it before
        // resolving the reference. Trim here too so a gate like
        // `$( MaybeUnpinned )` is seen by every taint/unpinned scan that
        // shares this extractor; otherwise a shape readable by evaluation
        // but invisible here would let an untrustworthy property gate a
        // construct without flagging it.
        let after = raw[open..].trim_start_matches(is_ws);
        let id_len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            .count();
        if id_len == 0 {
            continue;
        }
        let id = &after[..id_len];
        let rest = after[id_len..].trim_start_matches(is_ws);
        if rest.starts_with(')') {
            refs.push(id);
        } else if rest.starts_with('(') {
            // Method-call shapes whose base property the condition/property
            // layers can actually resolve: `$(X.TrimStart(…))`, the
            // `$(X.Split('-')[0])` idiom, and the `$(X.Contains/StartsWith/
            // EndsWith(…))` string predicates. Member names match
            // case-insensitively, mirroring the evaluators.
            if let Some(property) = id.strip_suffix(".TrimStart")
                && !property.is_empty()
            {
                refs.push(property);
            } else if let Some(base) = [".Split", ".Contains", ".StartsWith", ".EndsWith"]
                .into_iter()
                .find_map(|method| {
                    id.len()
                        .checked_sub(method.len())
                        .filter(|&cut| cut > 0 && id[cut..].eq_ignore_ascii_case(method))
                        .map(|cut| &id[..cut])
                })
            {
                refs.push(base);
            }
        }
    }
    refs.into_iter()
}

fn diagnose_item_op(node: Node<'_, '_>, attr: &str, state: &mut State<'_>) -> bool {
    let Some(value) = node.attribute(attr) else {
        return false;
    };
    state.push(
        DiagnosticKind::UnsupportedItemOperation {
            operation: format!("{attr}={value}"),
        },
        node.range(),
    );
    true
}

/// Dispatch a `<Import>` element. In pure mode (`follow_imports=false`)
/// every import becomes an [`DiagnosticKind::UnresolvedImport`]; with
/// imports enabled we evaluate `Condition`, refuse `Sdk` attributes
/// (phase 7b territory — they require an SDK resolver), and finally
/// recurse into the referenced file via [`walk_external_file`].
fn handle_import(node: Node<'_, '_>, current_file_dir: &Path, state: &mut State<'_>) {
    if !state.follow_imports {
        let project = node.attribute("Project").unwrap_or("").to_string();
        state.push(
            DiagnosticKind::UnresolvedImport { path: project },
            node.range(),
        );
        return;
    }
    follow_explicit_import(node, current_file_dir, state);
}

fn follow_explicit_import(node: Node<'_, '_>, current_file_dir: &Path, state: &mut State<'_>) {
    // The pre-scan in [`find_explicit_sdk_promotion`] may have already
    // promoted this body `<Import Sdk=X Project="Sdk.{props,targets}"/>`
    // to a root-equivalent splice position. Re-walking it here would
    // misplace `Sdk.targets`: its splice runs at the *bottom*, so a
    // body-position walk would register the file first and turn the
    // splice into the skipped duplicate. Silent skip mirrors MSBuild's
    // effective behaviour: the SDK chain only enters once, at splice
    // position.
    //
    // The skip is gated on "currently walking the entry project's
    // body": `state.hoisted_sdk_imports` is populated with ranges
    // into the *entry project's* XML, but `node.range()` is a byte
    // offset into whatever XML buffer the current frame is walking.
    // An imported file's `<Import>` that happens to share a byte
    // offset with a hoisted entry-project import must not get
    // silently dropped here. `import_site_span` is `None` exactly
    // while we're inside the entry project body — see [`State`].
    if state.import_site_span.is_none() && state.hoisted_sdk_imports.contains(&node.range()) {
        return;
    }
    if is_sdk_directory_build_rediscovery_import(node, current_file_dir, state) {
        // This IS MSBuild's real `Directory.Build.props` import position
        // (inside `Microsoft.Common.props`, notably *before* the
        // `NuGet.props` block). When the walker still owes the splice,
        // fire it here so properties the file sets are visible to the
        // rest of the SDK props chain (`NuGet.props` reads
        // `ImportDirectoryPackagesProps`, and a repo's
        // `Directory.Build.props` legitimately sets it). Two owed
        // shapes: the entry-SDK props phase (pre-body, pending flag
        // up), and the deferred pass's first *body-reached* nested
        // `Sdk.props` (the stash is consumed here rather than at the
        // nested file's return — same position MSBuild uses). Everywhere
        // else this stays the usual rediscovery suppression.
        let is_props_rediscovery = node
            .attribute("Project")
            .and_then(simple_property_reference)
            .is_some_and(|name| name.eq_ignore_ascii_case("DirectoryBuildPropsPath"));
        if is_props_rediscovery {
            if !state.in_entry_body && state.directory_build_props_splice_pending {
                fire_entry_directory_build_props_splice(state);
            } else if state.in_entry_body && state.pending_directory_build_props.take().is_some() {
                // `fire_…` reads the same fallback the stash carried
                // (both copies of `implicit_props` from `walk_once`),
                // and lowers `directory_build_props_splice_pending`, so
                // the post-`Sdk.props` consumption in
                // `walk_external_file` (which `take()`s the stash) is a
                // no-op afterwards.
                fire_entry_directory_build_props_splice(state);
            }
        }
        return;
    }
    // The import's own `Condition` gates whether it (and any Compile items /
    // gating properties it carries) is brought in. If we can't trust it — an
    // unsupported condition, or one relying on an undefined property — a
    // user-authored import we skip may leave the Compile set incomplete. SDK
    // chains condition imports constantly, so Compile uncertainty is gated to
    // non-SDK files. Package uncertainty is not gated: SDK imports can carry
    // implicit dependency items.
    let prev_gate = state.import_gate_context;
    let prev_pkg_gate = state.package_import_gate_context;
    state.import_gate_context = prev_gate || !state.in_sdk_subtree;
    state.package_import_gate_context = true;
    let diagnostics_before_gate = state.diagnostics.len();
    let gate = evaluate_condition(node, current_file_dir, state);
    state.note_package_uncertain_if_condition_uses_sdk_taint(node);
    state.import_gate_context = prev_gate;
    state.package_import_gate_context = prev_pkg_gate;
    // A user-authored import decided by an untrusted read may go the other
    // way in a real build: a skipped file could mutate the reference list,
    // and a followed file's references may not actually be brought in —
    // either way `project_references` can't be trusted. A gate outside our
    // grammar (Unsupported) is untrusted by construction.
    if !state.in_sdk_subtree
        && (matches!(gate, CondGate::Unsupported) || state.condition_reads_untrusted_value(node))
    {
        state.project_references_uncertain = true;
    }
    // An import gate we could not decide exactly (its evaluation raised
    // diagnostics, or it is outside the grammar) may bring in — or omit —
    // a whole file of property writes, whichever way we resolved it:
    // Skip may hide writes the real build performs, Run may perform
    // writes the real build skips. Either way no later undefined read
    // can claim exactness.
    if state.diagnostics.len() != diagnostics_before_gate || matches!(gate, CondGate::Unsupported) {
        state.walk_opaque = true;
    }
    match gate {
        CondGate::Run => {}
        CondGate::Skip => return,
        CondGate::Unsupported => {
            // Re-enter the gate context so the `UnsupportedCondition` flags.
            state.import_gate_context = prev_gate || !state.in_sdk_subtree;
            state.package_import_gate_context = true;
            emit_unsupported_condition(node, state);
            state.import_gate_context = prev_gate;
            state.package_import_gate_context = prev_pkg_gate;
            return;
        }
    }
    // `<Import Sdk="Microsoft.NET.Sdk" Project="Sdk.props" />` (or
    // `Sdk.targets`) is MSBuild's explicit way to import the SDK in two
    // pieces, sidestepping the `<Project Sdk=...>` shorthand. We
    // resolve via the caller-supplied SDK resolver and walk the chosen
    // file *at this position in the body*. The two well-known stems
    // use the explicit `SdkPaths::{props, targets}` fields; any other
    // `Project` value (e.g. `Sdk.Web.props` shipped by SDK variants)
    // is treated as a relative path under [`SdkPaths::root`]. Path
    // components are vetted before any FS touch: `..`, empty segments,
    // and absolute paths are rejected as `UnsupportedConstruct` so a
    // malformed or hostile fsproj cannot escape the SDK directory. A
    // missing-but-well-formed entry point falls through to the
    // existing IO path and surfaces as `ImportFailed::NotFound`.
    // Without a resolver, every Sdk import is unsupported, same as
    // phase 7a.
    if let Some(sdk_name) = node.attribute("Sdk") {
        let Some(resolver) = state.sdk_resolver else {
            state.push(
                DiagnosticKind::UnsupportedConstruct {
                    element: format!("Import Sdk={sdk_name:?}"),
                },
                node.range(),
            );
            // An SDK import we couldn't evaluate (no resolver) may have carried
            // default-item machinery; in a user-authored file that leaves the
            // Compile set incomplete. Inside the SDK tree Compile uncertainty is
            // tolerated, but dependency items could still be hidden.
            state.mark_structural_skip_respecting_sdk_compile_tolerance(
                StructuralCompileItemUncertainty::ExplicitSdkUnsupported {
                    sdk: sdk_name.to_string(),
                },
                node.range(),
            );
            return;
        };
        let resolution = match resolver(sdk_name) {
            Ok(resolution) => resolution,
            Err(err) => {
                state.push(sdk_error_to_diagnostic(sdk_name, err), node.range());
                return;
            }
        };
        let project_attr_raw = node.attribute("Project").unwrap_or("");
        // MSBuild expands `$(...)` in `Import` `Project` attributes
        // before resolving them. The two well-known stems are matched
        // against the *expanded* value so a project that writes
        // `Project="$(SdkPropsName)"` (with `SdkPropsName=Sdk.props`)
        // still picks the explicit field, and a project that uses a
        // property to switch SDK variants
        // (`Project="Sdk.$(Flavor).props"`) resolves to the expanded
        // relative path. Expansion issues (residual `$(...)` or
        // empty substitution) skip the import silently, mirroring the
        // non-SDK path below.
        let expansion = state.expand(project_attr_raw, node.range());
        if expansion.had_issue() || expansion.unpinned_root.is_some() {
            // An SDK import whose Project path we couldn't resolve is dropped;
            // an *unpinned* path (assembled from a property the property pass
            // couldn't pin down) is just as ambiguous — a real build may
            // resolve a different file entirely, so best-effort following
            // would be an over-resolve.
            // it could have carried Compile items or gating properties (same
            // reasoning as the plain `<Import>` path). Inside the SDK tree
            // Compile uncertainty is tolerated, but dependency items could
            // still be hidden.
            state.mark_structural_skip_respecting_sdk_compile_tolerance(
                StructuralCompileItemUncertainty::SdkImportProjectUnresolved {
                    sdk: sdk_name.to_string(),
                    project: project_attr_raw.to_string(),
                },
                node.range(),
            );
            return;
        }
        let project_attr_unescaped = expansion.value.unescape();
        let project_attr = project_attr_unescaped.as_str();
        // The two well-known stems are only meaningful for an ordinary
        // single-root SDK; everything else — including *every* `Project`
        // value under a locator-style resolution — is a relative path
        // vetted before any FS touch.
        let vetted_relative = |state: &mut State<'_>, value: &str| -> Option<String> {
            if !is_safe_sdk_relative_path(value) {
                state.push(
                    DiagnosticKind::UnsupportedConstruct {
                        element: format!("Import Sdk={sdk_name:?} Project={value:?}"),
                    },
                    node.range(),
                );
                // A rejected SDK import is dropped and could have carried
                // Compile items (same reasoning as the unresolved-path arm
                // above). Inside the SDK tree Compile uncertainty is
                // tolerated, but dependency items could still be hidden.
                state.mark_structural_skip_respecting_sdk_compile_tolerance(
                    StructuralCompileItemUncertainty::SdkImportProjectRejected {
                        sdk: sdk_name.to_string(),
                        project: value.to_string(),
                    },
                    node.range(),
                );
                return None;
            }
            Some(value.replace('\\', "/"))
        };
        match resolution {
            SdkResolution::Single(sdk_paths) => {
                // Tolerate this SDK's own files too — the
                // `Microsoft.NET.Sdk.Web` → base `Microsoft.NET.Sdk`
                // (sibling dir) case (P2.1).
                state.note_sdk_tolerance(&sdk_paths.root);
                let chosen: PathBuf = match project_attr {
                    "Sdk.props" => sdk_paths.props.clone(),
                    "Sdk.targets" => sdk_paths.targets.clone(),
                    other => {
                        let Some(normalised) = vetted_relative(state, other) else {
                            return;
                        };
                        sdk_paths.root.join(normalised)
                    }
                };
                walk_external_file(&chosen, node.range(), state);
            }
            SdkResolution::Roots(roots) => {
                // MSBuild's multi-path resolver contract: import `Project`
                // relative to every returned root, in order (all sharing
                // this walk's state); zero roots contribute nothing and
                // that is *exact* — MSBuild treats an empty workload
                // resolver result the same way.
                let Some(normalised) = vetted_relative(state, project_attr) else {
                    return;
                };
                for root in roots {
                    state.note_sdk_tolerance(&root);
                    walk_external_file(&root.join(&normalised), node.range(), state);
                }
            }
        }
        return;
    }
    let Some(raw_path) = node.attribute("Project") else {
        // `<Import>` with no Project attribute is malformed; surfacing
        // as an UnsupportedConstruct keeps the diagnostic shape
        // uniform without inventing a new variant for this corner.
        state.push(
            DiagnosticKind::UnsupportedConstruct {
                element: "Import (no Project attribute)".to_string(),
            },
            node.range(),
        );
        return;
    };
    let expansion = expand_import_project(raw_path, current_file_dir, node.range(), state);
    if expansion.had_issue() || expansion.unpinned_root.is_some() {
        // The expanded path has residual `$(...)`, substituted to "", or
        // leaned on an *unpinned* property (a value the property pass could
        // not pin down — a real build may resolve a different file entirely)
        // — every case is too ambiguous to follow safely. In a user-authored
        // file the import we just dropped could have contributed `<Compile>`
        // items, so the Compile set is no longer trustworthy. Inside the SDK
        // tree Compile uncertainty is tolerated, but the dropped import can
        // still hide dependency items.
        state.mark_structural_skip_respecting_sdk_compile_tolerance(
            StructuralCompileItemUncertainty::ImportProjectUnresolved {
                project: raw_path.to_string(),
            },
            node.range(),
        );
        return;
    }
    // The `Project` attribute is a semicolon-separated *list* of paths
    // (pinned against dotnet msbuild 10.0.300 with stub projects):
    // segments are whitespace-trimmed, empty segments are skipped — the
    // SDK's own `$(CustomAfterDirectoryBuildProps);…` accumulator idiom
    // yields a leading `;`, and a fully-empty list (`";"`) is a silent
    // no-op — files import left to right, a missing non-wildcard segment
    // fails the evaluation (MSB4019; our `ImportFailed` degrade), and each
    // segment may carry its own wildcard. The `;` split, trim, and
    // empty-segment skip all happen **in the escaped domain**, so an escaped
    // `%3b` stays data and does not split (see [`Escaped::split_list`]).
    //
    // One case the split alone gets wrong: an *empty or whitespace-only whole*
    // value (`Project=""`, or `$(Missing)` expanding exactly to "") is an
    // error in MSBuild (MSB4035/MSB4020), not a no-op — probed: `dotnet
    // msbuild -getProperty` on `<Import Project="$(Empty)"/>` exits 1. Only a
    // *separator-bearing* list whose entries are all empty (`";"`, the SDK's
    // possibly-empty `$(CustomAfterDirectoryBuildProps)` accumulator) is the
    // silent no-op. Distinguish by the presence of a `;` before splitting; the
    // empty whole value degrades structurally exactly as the residual/unpinned
    // path above does.
    let escaped = expansion.value.as_escaped();
    if escaped.trim().is_empty() && !escaped.contains(';') {
        // Push the diagnostic too, not just the uncertainty: this branch
        // can be reached with a *clean* expansion (a literal `Project=""`,
        // or an exact undefined read substituting "" without any
        // diagnostic of its own — C.2b), and `is_partial` derives solely
        // from `diagnostics`, so silently returning would report full
        // fidelity for a project MSBuild refuses to evaluate.
        state.push(
            DiagnosticKind::UnsupportedConstruct {
                element: format!(
                    "Import Project={raw_path:?} (expands to an empty path, \
                     an MSBuild evaluation error)"
                ),
            },
            node.range(),
        );
        state.mark_structural_skip_respecting_sdk_compile_tolerance(
            StructuralCompileItemUncertainty::ImportProjectUnresolved {
                project: raw_path.to_string(),
            },
            node.range(),
        );
        return;
    }
    for segment in expansion.value.split_list() {
        // Wildcard-ness is classified on the **escaped** segment, before
        // decoding: an escaped `%2a` is a literal star in a filename, and
        // MSBuild imports that one file rather than globbing (classify, then
        // unescape — the same order item specs use). Decoding first would turn
        // `star%2afile.props` into a glob and silently import every match.
        //
        // Backslash-to-forward-slash normalisation matches what
        // `push_include_entry` does for `Include` attributes; MSBuild accepts
        // both on either platform.
        let normalised = segment.unescape().replace('\\', "/");
        if segment.has_live_wildcard() {
            // A *decoded* `*`/`?` alongside a live one is not expressible as a
            // glob pattern — after decoding we could not tell the literal star
            // from the wildcard — so decline rather than import the wrong set.
            if properties::escaping::decodes_to_any(segment.as_escaped(), &['*', '?']) {
                state.push(
                    DiagnosticKind::UnsupportedGlob {
                        pattern: segment.as_escaped().to_string(),
                    },
                    node.range(),
                );
                // A declined import is a file's worth of content we did not
                // enter — it could match (MSBuild imports a literal
                // `*.props*` for `Project="*.props%2a"`) and define
                // properties. Latch opacity so a later undefined read of such
                // a name is not wrongly classified exact (C.2b).
                state.walk_opaque = true;
                continue;
            }
            follow_wildcard_import(&normalised, current_file_dir, node.range(), state);
            continue;
        }
        let path = current_file_dir.join(normalised);
        walk_external_file(&path, node.range(), state);
        // Record NuGet's central-package import point by its *shape* —
        // membership in `walked_files` afterwards means the walk genuinely
        // entered the file, on this import or an earlier one (failed
        // walks never insert; a duplicate-skipped one was inserted by
        // its first walk).
        if node
            .attribute("Project")
            .and_then(simple_property_reference)
            .is_some_and(|name| name.eq_ignore_ascii_case("DirectoryPackagesPropsPath"))
        {
            let canon = canonicalise_or_normalise(&path);
            if state.walked_files.contains(&canon) {
                state.walked_directory_packages_props_import = Some(canon);
            }
        }
    }
}

/// A wildcard `<Import Project=…>`. MSBuild expands the wildcard and —
/// unlike a literal path, where a missing file is an error — **silently
/// skips** the import when nothing matches. The SDK relies on this: the
/// unconditional `Microsoft.VisualStudioVersion.v*.Common.props` import in
/// `Microsoft.Common.props` matches nothing on a clean dotnet host, and the
/// `Imports\…\ImportBefore\*` hooks usually match nothing too.
///
/// Only a wildcard confined to the final path component is modelled (that
/// covers every SDK use). Matches are walked in name order — MSBuild's
/// FileMatcher yields an OS listing that its import loop consumes as a
/// sorted set, and a deterministic order is required here anyway.
/// A wildcard *directory* component falls back to the conservative
/// structural skip: the un-followed files could have carried anything.
fn follow_wildcard_import(
    normalised: &str,
    current_file_dir: &Path,
    span: Range<usize>,
    state: &mut State<'_>,
) {
    let path = current_file_dir.join(normalised);
    let file_pattern = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = path.parent().unwrap_or(current_file_dir);
    if dir.to_string_lossy().contains(['*', '?']) || file_pattern.is_empty() {
        // Push the diagnostic too, not just the uncertainty: silently
        // returning would leave `is_partial` false with imports dropped.
        state.push(
            DiagnosticKind::UnsupportedConstruct {
                element: format!("Import Project={normalised:?} (wildcard directory component)"),
            },
            span.clone(),
        );
        state.mark_structural_skip_respecting_sdk_compile_tolerance(
            StructuralCompileItemUncertainty::ImportProjectUnresolved {
                project: normalised.to_string(),
            },
            span,
        );
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            // Missing directory (or a file where a directory was named)
            // ≡ zero matches: MSBuild-silent.
            return;
        }
        Err(e) => {
            // Unreadable directory is NOT zero matches — files may well
            // be there, and the dropped imports could have carried
            // anything. Diagnostic + uncertainty, same as the wildcard-
            // directory arm above.
            state.push(
                DiagnosticKind::ImportFailed {
                    path: dir.to_path_buf(),
                    reason: ImportFailReason::Io {
                        message: e.to_string(),
                    },
                },
                span.clone(),
            );
            state.mark_structural_skip_respecting_sdk_compile_tolerance(
                StructuralCompileItemUncertainty::ImportProjectUnresolved {
                    project: normalised.to_string(),
                },
                span,
            );
            return;
        }
    };
    let mut matches: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| !t.is_dir()))
        .filter(|entry| wildcard_matches(&file_pattern, &entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    // MSBuild consumes wildcard matches in ordinal-ignore-case order
    // (verified against `dotnet msbuild`: `a.props` imports before
    // `B.props`); byte order would put `B` first on unix. Byte order
    // breaks ties so equal-ignoring-case names still sort
    // deterministically.
    matches.sort_by(|a, b| {
        let key = |p: &PathBuf| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        key(a).cmp(&key(b)).then_with(|| a.cmp(b))
    });
    for matched in matches {
        walk_external_file(&matched, span.clone(), state);
    }
}

/// Match `pattern` (with `*` = any run, `?` = any single char) against
/// `name`, case-insensitively (MSBuild file matching is case-insensitive
/// even on case-sensitive filesystems). Classic two-pointer glob with
/// star backtracking.
fn wildcard_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let name: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    let (mut p, mut n) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((star_p, star_n)) = star {
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, star_n + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Fire the walker-owned entry `Directory.Build.props` splice: resolve
/// the override/fallback, check the gate, seed the path property when the
/// import came from discovery, walk the file, and lower the pending flag.
/// Callable from two positions: MSBuild's real import point inside
/// `Microsoft.Common.props` (preferred — properties the file sets are then
/// visible to the rest of the SDK props chain, e.g. the `NuGet.props`
/// gates), or right after `Sdk.props` as the fallback for SDK chains that
/// never reach a real `Microsoft.Common.props`.
fn fire_entry_directory_build_props_splice(state: &mut State<'_>) {
    let project_dir = state.entry_project_dir.clone();
    let fallback = state.implicit_directory_build_props_fallback.clone();
    // Null `import_site_span` across the splice so diagnostics and items
    // are attributed identically from both call positions (origin
    // `Imported`, span `0..0`) — a no-op at the after-`Sdk.props`
    // position where it is already `None`.
    let saved_import_site_span = state.import_site_span.take();
    let props_to_import = resolve_directory_build_path(
        state,
        "DirectoryBuildPropsPath",
        fallback.as_deref(),
        &project_dir,
    );
    if let Some(Resolution { path, source }) = props_to_import.as_ref()
        && should_import_default_true(
            state
                .lookup
                .get_unescaped("ImportDirectoryBuildProps")
                .as_deref(),
            state.is_sticky_global("ImportDirectoryBuildProps"),
        )
    {
        // MSBuild assigns `DirectoryBuildPropsPath` only when the
        // discovery-and-import path actually fires (the seeding sits
        // inside the gated block in `Microsoft.Common.props`), so
        // skip seeding when the import is gated out — otherwise the
        // body sees a phantom path the real build never assigned.
        if matches!(source, ResolutionSource::Fallback) {
            seed_directory_build_path(state, "DirectoryBuildPropsPath", path);
        }
        walk_directory_build_file(path, 0..0, DirectoryBuildFile::Props, state);
    }
    state.directory_build_props_splice_pending = false;
    state.import_site_span = saved_import_site_span;
}

fn is_sdk_directory_build_rediscovery_import(
    node: Node<'_, '_>,
    current_file_dir: &Path,
    state: &State<'_>,
) -> bool {
    // The SDK's Microsoft.Common.* files rediscover Directory.Build.* through
    // these path properties. This walker already owns that import point and
    // splices those files explicitly, so following the SDK rediscovery import
    // would double-walk user props/targets files. Keep this narrow: an SDK can
    // also use these property names for its own custom imports, and those must
    // still go through normal condition/path evaluation.
    if !state.in_sdk_subtree {
        return false;
    }
    let Some(raw_project) = node.attribute("Project") else {
        return false;
    };
    let Some(property_name) = simple_property_reference(raw_project) else {
        return false;
    };
    if !condition_has_exists_for_property(node.attribute("Condition"), property_name) {
        return false;
    }
    let (splice_path, written_by_splice) =
        if property_name.eq_ignore_ascii_case("DirectoryBuildPropsPath") {
            (
                state.directory_build_props_splice_path.as_ref(),
                state.directory_build_props_path_written_by_splice,
            )
        } else if property_name.eq_ignore_ascii_case("DirectoryBuildTargetsPath") {
            (
                state.directory_build_targets_splice_path.as_ref(),
                state.directory_build_targets_path_written_by_splice,
            )
        } else {
            return false;
        };
    let Some(resolved) = resolve_property_import_target(property_name, current_file_dir, state)
    else {
        // Unset/empty path — the common no-`Directory.Build.*` case (or
        // an explicit `ImportDirectoryBuild*=false`, which leaves the
        // discovery group skipped). MSBuild's own `exists('$(…)')` gate
        // makes this import a clean skip; evaluating the condition here
        // would only manufacture an `UndefinedProperty` diagnostic for
        // perfectly ordinary projects. The walker owns this import
        // point, so treat it as the rediscovery it is.
        return true;
    };
    if written_by_splice {
        return true;
    }
    let resolved = canonicalise_or_normalise(&resolved);
    if splice_path.is_some_and(|path| resolved == *path) {
        return true;
    }
    property_name.eq_ignore_ascii_case("DirectoryBuildPropsPath")
        && pending_directory_build_props_splice_matches(&resolved, state)
}

fn pending_directory_build_props_splice_matches(resolved_import: &Path, state: &State<'_>) -> bool {
    if !state.directory_build_props_splice_pending
        || !should_import_default_true(
            state
                .lookup
                .get_unescaped("ImportDirectoryBuildProps")
                .as_deref(),
            state.is_sticky_global("ImportDirectoryBuildProps"),
        )
    {
        return false;
    }
    let Some(Resolution { path, .. }) = resolve_directory_build_path(
        state,
        "DirectoryBuildPropsPath",
        None,
        &state.entry_project_dir,
    ) else {
        return false;
    };
    canonicalise_or_normalise(&path) == resolved_import
}

fn simple_property_reference(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("$(")?.strip_suffix(')')?;
    if inner.is_empty()
        || !inner
            .bytes()
            .all(|b| b == b'_' || b == b'.' || b.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(inner)
}

fn condition_has_exists_for_property(condition: Option<&str>, property_name: &str) -> bool {
    let Some(condition) = condition else {
        return false;
    };
    for (idx, _) in condition.char_indices() {
        let Some(rest) = condition[idx..].strip_prefix("Exists").or_else(|| {
            condition[idx..]
                .get(..6)
                .filter(|s| s.eq_ignore_ascii_case("Exists"))
                .map(|_| &condition[idx + 6..])
        }) else {
            continue;
        };
        if !keyword_boundary(condition, idx, 6) {
            continue;
        }
        let Some(rest) = rest.trim_start().strip_prefix('(') else {
            continue;
        };
        let Some((arg, rest)) = parse_single_quoted(rest.trim_start()) else {
            continue;
        };
        if !rest.trim_start().starts_with(')') {
            continue;
        }
        if simple_property_reference(arg)
            .is_some_and(|name| name.eq_ignore_ascii_case(property_name))
        {
            return true;
        }
    }
    false
}

fn keyword_boundary(source: &str, start: usize, len: usize) -> bool {
    let bytes = source.as_bytes();
    let before = start
        .checked_sub(1)
        .and_then(|idx| bytes.get(idx))
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    let after = bytes
        .get(start + len)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    !before && !after
}

fn resolve_property_import_target(
    property_name: &str,
    current_file_dir: &Path,
    state: &State<'_>,
) -> Option<PathBuf> {
    // The property names a file on disk: a point of use, trimmed in the domain
    // (an escaped `%20` is filename data, not padding).
    let trimmed = state.lookup.get(property_name)?.trimmed_unescaped();
    if trimmed.is_empty() {
        return None;
    }
    let normalised = trimmed.replace('\\', "/");
    let path = Path::new(&normalised);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_file_dir.join(path)
    })
}

fn expand_import_project(
    raw_path: &str,
    current_file_dir: &Path,
    span: Range<usize>,
    state: &mut State<'_>,
) -> Expansion {
    if let Some(expansion) =
        expand_get_path_of_file_above_import(raw_path, current_file_dir, span.clone(), state)
    {
        return expansion;
    }
    state.expand(raw_path, span)
}

fn expand_get_path_of_file_above_import(
    raw_path: &str,
    current_file_dir: &Path,
    span: Range<usize>,
    state: &mut State<'_>,
) -> Option<Expansion> {
    const PREFIX: &str = "$([MSBuild]::GetPathOfFileAbove(";
    let trimmed = raw_path.trim();
    let args = trimmed.strip_prefix(PREFIX)?.strip_suffix("))")?;
    let (file_arg, start_arg) = parse_two_single_quoted_args(args)?;
    let file = state.expand(file_arg, span.clone());
    let start = state.expand(start_arg, span);
    if file.had_issue() || start.had_issue() {
        return Some(Expansion {
            value: Escaped::default(),
            had_undefined: file.had_undefined || start.had_undefined,
            had_unsupported: file.had_unsupported || start.had_unsupported,
            unpinned_root: file.unpinned_root.or(start.unpinned_root),
        });
    }
    // Both arguments are points of use: they name a file and a directory on
    // disk, so they leave the domain here.
    let start_dir = current_file_dir.join(start.value.unescape().replace('\\', "/"));
    let value = find_file_above(&file.value.unescape(), &start_dir)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A path found by walking the filesystem is computed text: it re-enters the
    // escaped domain, so a `%` or `(` in it is inert — same rule as the
    // `well_known` seeds.
    let value = Escaped::from_computed(&value);
    Some(Expansion {
        value,
        had_undefined: false,
        had_unsupported: false,
        // An unpinned argument makes the *resolved path* unpinned too: a
        // real build could search from a different directory (or for a
        // different file) and import something else entirely.
        unpinned_root: file.unpinned_root.or(start.unpinned_root),
    })
}

fn parse_two_single_quoted_args(args: &str) -> Option<(&str, &str)> {
    let (first, rest) = parse_single_quoted(args.trim_start())?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(',')?.trim_start();
    let (second, rest) = parse_single_quoted(rest)?;
    rest.trim().is_empty().then_some((first, second))
}

fn parse_single_quoted(input: &str) -> Option<(&str, &str)> {
    let rest = input.strip_prefix('\'')?;
    let end = rest.find('\'')?;
    Some((&rest[..end], &rest[end + 1..]))
}

fn find_file_above(file: &str, start_dir: &Path) -> Option<PathBuf> {
    if file.is_empty()
        || Path::new(file)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(file);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Returns true if `attr` is a safe relative path under an SDK root —
/// i.e. it has at least one non-empty component, no `..` components,
/// no leading or interior empty components, no Windows drive or UNC
/// prefix, and is not absolute. MSBuild accepts both forward and back
/// slashes as separators, so we vet against both.
///
/// Used to guard `<Import Sdk="X" Project="..."/>` for `Project`
/// values other than the two well-known stems: a hostile or malformed
/// fsproj must not be able to walk outside the SDK directory.
///
/// The Windows prefix check matters even when the parser runs on
/// Linux: `PathBuf::join("C:..")` on any platform produces a path
/// that, when later canonicalised on Windows, can replace the SDK
/// root with the drive prefix. `Path::is_absolute()` doesn't catch
/// `C:foo` (no root separator) so we reject the drive prefix
/// explicitly. The check is host-independent so a project authored
/// on Windows can still be lex-checked on a Linux LSP host without
/// silently bypassing the guard.
fn is_safe_sdk_relative_path(attr: &str) -> bool {
    if attr.is_empty() {
        return false;
    }
    if Path::new(attr).is_absolute() {
        return false;
    }
    // Normalise so the component scan also catches `..` after a
    // backslash and empty segments from `//` runs of either separator.
    // UNC paths (`\\server\share\...`) become `//server/share/...`
    // and are caught by the leading-empty-component check.
    let normalised = attr.replace('\\', "/");
    if normalised.starts_with('/') {
        return false;
    }
    normalised.split('/').all(|component| {
        if component.is_empty() || component == ".." {
            return false;
        }
        // Reject `C:`, `C:foo`, etc. — any component that starts
        // with a single ASCII letter followed by a colon is a
        // Windows drive prefix and would re-root the join.
        let bytes = component.as_bytes();
        !(bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    })
}

/// Read, parse, and recursively walk an external MSBuild file.
/// Caller-supplied `span` attributes any failure diagnostic to the
/// source location of the *importer* (the `<Import>` element's
/// range, or `0..0` for an implicit import).
///
/// Self-contained: both failure modes (depth, IO/parse) emit a single
/// [`DiagnosticKind::ImportFailed`] and return without disturbing
/// `state.depth`; a duplicate of an already-performed import returns
/// silently (see below). The depth increment/decrement pair only runs
/// on the happy path.
fn walk_external_file(path: &Path, span: Range<usize>, state: &mut State<'_>) {
    // MSBuild's duplicate-import skip: an import that resolves to an
    // already-imported path (same lexically-normalised, case-insensitive
    // spelling — see [`import_dedup_key`]) is ignored with a warning
    // (MSB4011; MSB4210 for the entry project) and the evaluation
    // succeeds. That cuts repeated list segments, diamonds, *and*
    // cycles — MSBuild registers an import before walking the file, so
    // a cycle's back-edge is just another duplicate (probed: a↔b
    // evaluates cleanly with each body run once). The skip is faithful,
    // so it is silent here: pushing a diagnostic would flip
    // `is_partial` for evaluations MSBuild completes exactly.
    let dedup_key = import_dedup_key(path);
    if state.imports_seen.contains(&dedup_key) {
        return;
    }
    // A near-duplicate under the wider Unicode fold: the pair differs in
    // non-ASCII casing, where .NET's ordinal table (probed: `ı`≠`I` yet
    // `σ`==`Σ`) is not reproducible from Rust's folds. MSBuild might skip
    // this import or walk it; committing either would risk a wrong-clean
    // result, so decline — the diagnostic flips `is_partial` and the
    // structural skip records that a file's worth of content may be
    // missing (which also latches `walk_opaque` for C.2b).
    let fuzzy_key = import_dedup_fuzzy_key(path);
    if state.imports_seen_fuzzy.contains(&fuzzy_key) {
        state.push(
            DiagnosticKind::UnsupportedConstruct {
                element: format!(
                    "Import Project={} (near-duplicate of an already-imported path \
                     under Unicode case folding)",
                    path.display()
                ),
            },
            span.clone(),
        );
        state.mark_structural_skip_respecting_sdk_compile_tolerance(
            StructuralCompileItemUncertainty::ImportProjectUnresolved {
                project: path.display().to_string(),
            },
            span,
        );
        return;
    }
    if state.depth >= MAX_IMPORT_DEPTH {
        state.push(
            DiagnosticKind::ImportFailed {
                path: path.to_path_buf(),
                reason: ImportFailReason::DepthLimit { depth: state.depth },
            },
            span,
        );
        return;
    }
    // Canonical (symlink-resolved) identity for `walked_files` and the
    // SDK-subtree verdict below. Deliberately *not* the dedup key: MSBuild
    // never resolves symlinks when deduping, so an alias of an imported
    // file must still walk (its dedup key differs), while `walked_files`
    // wants the file's real identity.
    let canon = match std::fs::canonicalize(path) {
        Ok(c) => c,
        Err(e) => {
            let reason = if e.kind() == std::io::ErrorKind::NotFound {
                ImportFailReason::NotFound
            } else {
                ImportFailReason::Io {
                    message: e.to_string(),
                }
            };
            state.push(
                DiagnosticKind::ImportFailed {
                    path: path.to_path_buf(),
                    reason,
                },
                span,
            );
            return;
        }
    };
    let source = match std::fs::read_to_string(&canon) {
        Ok(s) => s,
        Err(e) => {
            let reason = if e.kind() == std::io::ErrorKind::NotFound {
                ImportFailReason::NotFound
            } else {
                ImportFailReason::Io {
                    message: e.to_string(),
                }
            };
            state.push(
                DiagnosticKind::ImportFailed {
                    path: canon,
                    reason,
                },
                span,
            );
            return;
        }
    };
    let doc = match roxmltree::Document::parse(&source) {
        Ok(d) => d,
        Err(e) => {
            state.push(
                DiagnosticKind::ImportFailed {
                    path: canon,
                    reason: ImportFailReason::MalformedXml {
                        message: e.to_string(),
                    },
                },
                span,
            );
            return;
        }
    };
    // Rebind `MSBuildThisFile{,Directory}` using the *pre-canonical*
    // path for the same reason nested `<Import>` resolution does
    // below: when MSBuild reaches a file through a symlink, it
    // treats the symlink path — not the OS-canonical target — as
    // the file's identity for path purposes. Using `&canon` here
    // would make `$(MSBuildThisFileDirectory)local.props` resolve
    // against the link target, picking the wrong sibling file.
    // `canon` is reserved for `walked_files` identity.
    let frame = state.enter_this_file(path);
    // Deferred `<ItemGroup>`s recorded while inside this file must know
    // which document to replay against — same pre-canonical-path identity
    // as the `MSBuildThisFile` frame above. `retained: None` until the
    // first deferral in this frame forces the source to be kept.
    let saved_current_file = std::mem::replace(
        &mut state.current_file,
        CurrentFile::Imported {
            path: path.to_path_buf(),
            retained: None,
        },
    );
    state.imports_seen.insert(dedup_key);
    state.imports_seen_fuzzy.insert(fuzzy_key);
    state.walked_files.insert(canon.clone());
    state.depth += 1;
    // Judge SDK-provenance by this file's *own* canonical path, independent of
    // how it was reached. A user `Directory.Build.props` pulled in through the
    // SDK's `Microsoft.Common.props` chain is still scored by its path (not
    // under the SDK tree → respected); an SDK target reached from anywhere is
    // tolerated. Save/restore so the parent file's verdict resumes on return.
    //
    // Two arms are ORed, both compared against the recorded (canonical) roots:
    //
    //  1. the file's own canonical path — the primary check (normal SDK files,
    //     diamond imports through differently-spelt paths that agree only after
    //     canonicalisation);
    //  2. the canonicalisation of the reach path's *parent directory*.
    //
    // Arm 2 exists for the file-level symlink-merge layout (Nix's combined dotnet
    // tree, but any store-of-symlinks qualifies): there the merge tree's
    // directories are real while its leaf files are per-file symlinks back to the
    // originating store, so a workload `WorkloadManifest.targets` canonicalises
    // into a *different* store than its own directory does. Arm 1 alone then
    // wrongly scores that SDK-internal manifest as a user file and lets its
    // `<ImportGroup>` conditions (e.g. `'$(TargetPlatformIdentifier)' ==
    // 'android'`) flip `items_uncertain`, refusing the fold for every real SDK
    // project (`sdk_project_fold_e2e`). Canonicalising the *directory* (which is
    // real in the merge tree) recovers tolerance.
    //
    // Crucially arm 2 canonicalises the directory, so it is not fooled by a
    // reach path that only *lexically* sits under a root: an SDK file importing
    // `../user.props` (parent canonicalises to the SDK's parent, outside every
    // root) or a directory symlink inside the SDK tree pointing out (parent
    // canonicalises to the escape target) are both correctly *not* tolerated —
    // only a genuine leaf-file symlink out of a real in-tree directory is.
    let reach_parent_canon = path
        .parent()
        .map(|dir| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()));
    let saved_in_sdk_subtree = state.in_sdk_subtree;
    state.in_sdk_subtree = state.sdk_tolerance_roots.iter().any(|root| {
        canon.starts_with(root)
            || reach_parent_canon
                .as_deref()
                .is_some_and(|dir| dir.starts_with(root))
    });
    // Pin `import_site_span` on the *first* descent (when it's None)
    // and preserve it on nested descents. All diagnostics and items
    // produced inside this — and any further nested — file will be
    // attributed to this top-level `<Import>` element in the entry
    // project's source, keeping `Diagnostic::span` /
    // `ResolvedItem::span` valid byte offsets into the source the
    // caller handed in. This must happen before we record imported-file
    // uncertainty causes below, so those causes get `Imported`
    // provenance too. See [`State::import_site_span`].
    let saved_import_site_span = state.import_site_span.clone();
    if saved_import_site_span.is_none() {
        state.import_site_span = Some(span.clone());
    }
    // For nested `<Import>` resolution, use the *pre-canonical*
    // path's parent — the directory MSBuild reached this file
    // through, not the directory the OS symlink-resolves to. When
    // `links/common.props` is a symlink to `/elsewhere/common.props`
    // and that file imports `local.props`, MSBuild reads
    // `links/local.props`; canonicalising would have made us read
    // `/elsewhere/local.props`. `canon` is still used (above) for
    // `walked_files` — the one place resolved identity matters.
    let file_dir: PathBuf = path.parent().map(Path::to_path_buf).unwrap_or_default();
    // `TreatAsLocalProperty` on an imported root unprotects names
    // for the scope of that file. MSBuild specifies it only applies
    // to *global* properties — not reserved well-known names — so
    // we exclude names in `state.reserved` from the unprotection.
    // Track exactly what we removed so the corresponding restore
    // doesn't re-add names that the entry project itself had marked
    // local (and which therefore were never in `state.protected`).
    let imported_overrides = collect_local_overrides(doc.root_element());
    let unprotected: Vec<String> = imported_overrides
        .into_iter()
        .filter(|name| !state.reserved.contains(name) && state.protected.remove(name))
        .collect();
    // Nested SDK roots are spliced through the same machinery the
    // entry project uses: `resolve_project_sdk` returns paths on a
    // happy resolve and pushes the appropriate diagnostic for every
    // failure mode (no resolver supplied, `Err(NotFound)`,
    // `Err(VersionNotSatisfied)`). The recursive `walk_external_file`
    // calls re-enter this same function, so the duplicate-import skip,
    // depth limits, and the import-site span pin (already `Some` here
    // for any descent beyond the first) all just work.
    //
    // The entry `Directory.Build.{props,targets}` splice is *not*
    // re-done around each imported file — re-splicing would double-count
    // it (MSBuild imports it once, via path-dedup). But MSBuild does
    // import `Directory.Build.props` right after the *first* `Sdk.props`
    // to run (that re-enters `Microsoft.Common.props`). When the entry
    // project has no SDK of its own, that first `Sdk.props` is a nested
    // one reached here — so the two-pass orchestrator in
    // [`walk_with_imports`] defers the entry `Directory.Build.props` and
    // we fire it below, once, right after the nested `Sdk.props`. (When
    // the entry *has* an SDK, the eager before-body splice already ran
    // and `pending_directory_build_props` is `None`, so nothing fires
    // here.)
    let nested_sdk = resolve_project_sdk(doc.root_element(), state);
    if let Some(paths) = nested_sdk.as_ref() {
        walk_external_file(&paths.props, doc.root_element().range(), state);
        // Only a nested `Sdk.props` reached *through the entry project's
        // body* is the "first `Sdk.props`" MSBuild would run, and hence
        // the point at which it imports `Directory.Build.props`. A nested
        // SDK reached while walking an already-spliced
        // `Directory.Build.{props,targets}` (outside the body) must not
        // reposition the entry `Directory.Build.props`.
        if state.in_entry_body {
            // `take()` unconditionally so the orchestrator can tell "fire
            // point reached" (consumed) from "dangle" (never consumed) —
            // and so the import is attempted at most once regardless of
            // how the gate resolves.
            if let Some(DeferredDirectoryBuildProps { fallback }) =
                state.pending_directory_build_props.take()
            {
                // Resolve exactly where MSBuild does — here, against live
                // state — so a body/SDK property set before this nested
                // `Sdk.props` (e.g. `ImportDirectoryBuildProps=false` or a
                // redirected `DirectoryBuildPropsPath`) is honoured.
                let project_dir = state.entry_project_dir.clone();
                let resolved = resolve_directory_build_path(
                    state,
                    "DirectoryBuildPropsPath",
                    fallback.as_deref(),
                    &project_dir,
                );
                if let Some(Resolution { path, source }) = resolved
                    && should_import_default_true(
                        state
                            .lookup
                            .get_unescaped("ImportDirectoryBuildProps")
                            .as_deref(),
                        state.is_sticky_global("ImportDirectoryBuildProps"),
                    )
                {
                    // Null `import_site_span` across the splice so it is
                    // byte-for-byte identical to the eager before-body
                    // call site (where it is `None`): Directory.Build.props
                    // items and diagnostics get origin `Imported` + span
                    // `0..0`, not this nested file's import-site span.
                    let saved = state.import_site_span.take();
                    if matches!(source, ResolutionSource::Fallback) {
                        seed_directory_build_path(state, "DirectoryBuildPropsPath", &path);
                    }
                    walk_directory_build_file(&path, 0..0, DirectoryBuildFile::Props, state);
                    state.import_site_span = saved;
                }
                state.directory_build_props_splice_pending = false;
            }
        }
    }
    walk_doc_body(doc.root_element(), &file_dir, state);
    if let Some(paths) = nested_sdk.as_ref() {
        walk_external_file(&paths.targets, doc.root_element().range(), state);
    }
    for name in unprotected {
        state.protected.insert(name);
    }
    state.import_site_span = saved_import_site_span;
    state.in_sdk_subtree = saved_in_sdk_subtree;
    state.depth -= 1;
    state.current_file = saved_current_file;
    state.exit_this_file(frame);
}

fn walk_directory_build_file(
    path: &Path,
    span: Range<usize>,
    kind: DirectoryBuildFile,
    state: &mut State<'_>,
) {
    match kind {
        DirectoryBuildFile::Props => {
            state.directory_build_props_splice_path = Some(canonicalise_or_normalise(path));
            state.directory_build_props_splice_pending = false;
        }
        DirectoryBuildFile::Targets => {
            state.directory_build_targets_splice_path = Some(canonicalise_or_normalise(path));
        }
    }
    let saved = state.active_directory_build_splice;
    state.active_directory_build_splice = Some(kind);
    walk_external_file(path, span, state);
    state.active_directory_build_splice = saved;
}

fn contains_glob(s: &str) -> bool {
    s.bytes().any(|b| b == b'*' || b == b'?')
}

fn contains_item_reference(s: &str) -> bool {
    s.contains("@(")
}

fn contains_metadata_reference(s: &str) -> bool {
    s.contains("%(")
}

/// Concatenate every text child of `node`, matching MSBuild's "element's
/// full inner text" rule. roxmltree splits the children whenever a
/// comment, processing instruction, or CDATA boundary intervenes, and
/// [`Node::text`] only exposes the first text child — so a value of the
/// form `A<!-- … -->B` would silently lose the `B` half. Iterating every
/// text child preserves the whole value. CDATA sections are exposed by
/// roxmltree as [`NodeType::Text`], so this loop captures them too.
///
/// A text child that is *literally* nothing but XML whitespace in the source is
/// **insignificant** and contributes nothing, matching MSBuild's XML layer.
/// This is a per-text-node rule, not a whole-value one, and all of it is pinned
/// against `dotnet msbuild` 10.0.301 (2026-07-11):
///
/// - `<P> </P>` and a tab-only body → `""` (so `'$(P)' == ''` is *true*).
/// - `<Q>  x  </Q>` → `"  x  "`: padding around content stays verbatim.
/// - `<R>  <!-- c -->x</R>` → `"x"`: a comment splits the text children, and
///   the whitespace-only one drops on its own — which is why the test must be
///   per-node (a whole-value collapse would wrongly keep `"  x"` here).
/// - `<P> $(Undefined) </P>` → `"  "`: the rule is pre-expansion, and is not
///   re-applied to an expansion result.
///
/// "XML whitespace" is exactly space/tab/CR/LF — a non-breaking space is
/// content (`&#160;` has length 1) — so this must not use `char::is_whitespace`,
/// which is Unicode-wide.
///
/// Returns `None` for the two shapes whose value we cannot derive, so callers
/// degrade rather than commit a guess:
///
/// - **Any CDATA in the body.** roxmltree exposes CDATA as a text node *and*
///   merges it with adjacent literal text, truncating the merged node's source
///   range — so `<P> <![CDATA[ ]]> </P>` (MSBuild: `" "` — the literal
///   whitespace insignificant, the CDATA space kept) is indistinguishable in
///   the decoded view from `<P>   </P>` (MSBuild: `""`). CDATA in a property
///   body is vanishingly rare: one file in the entire SDK chain, zero in the
///   F# corpus, and that one is an `@(…)`-bearing value that already degrades.
/// - **Entity-encoded whitespace** (a text node whose *decoded* value is all
///   whitespace but whose source is not, e.g. `&#32;`). MSBuild is inconsistent
///   here — `&#32;` and `&#9;` are kept (length 1) while `&#x20;` is dropped
///   (length 0) — so neither the source nor the decoded test predicts it, and
///   we decline rather than pick a side.
fn collect_element_text(node: Node<'_, '_>) -> Option<String> {
    let source = node.document().input_text();
    // Element-level check: a merged text node's own range would not reveal the
    // CDATA (roxmltree truncates it to the first chunk), so ask the element.
    if source[node.range()].contains("<![CDATA[") {
        return None;
    }
    let is_xml_ws = |b: u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n');
    let mut out = String::new();
    for child in node.children() {
        if !child.is_text() {
            continue;
        }
        let Some(text) = child.text() else { continue };
        if !text.is_empty() && text.bytes().all(is_xml_ws) {
            if source[child.range()].bytes().all(is_xml_ws) {
                // Insignificant literal whitespace: contributes nothing.
                continue;
            }
            return None;
        }
        out.push_str(text);
    }
    Some(out)
}

#[cfg(test)]
mod orchestrator_tests {
    use super::deferred_pass_can_change_result;

    #[test]
    fn deferred_pass_only_worth_running_with_resolver_and_no_entry_sdk() {
        // The deferred second pass can only change the result by firing
        // the stashed `Directory.Build.props` at a body-reached nested
        // `<Project Sdk=...>`. That needs both (a) the entry to lack its
        // own SDK — else pass 1's before-body splice is already faithful —
        // and (b) an SDK resolver — else no nested `<Project Sdk=...>` can
        // resolve, so the deferred splice can never be consumed and pass 2
        // always dangles straight back to pass 1. Every other combination
        // must skip the (otherwise discarded) second walk.
        assert!(deferred_pass_can_change_result(false, true));
        assert!(!deferred_pass_can_change_result(true, true));
        assert!(!deferred_pass_can_change_result(false, false));
        assert!(!deferred_pass_can_change_result(true, false));
    }
}

#[cfg(test)]
mod simple_property_references_tests {
    use super::simple_property_references;

    fn refs(raw: &str) -> Vec<&str> {
        simple_property_references(raw).collect()
    }

    #[test]
    fn plain_and_method_references_are_extracted() {
        assert_eq!(refs("$(Foo)"), vec!["Foo"]);
        assert_eq!(refs("$(A)/$(B)"), vec!["A", "B"]);
        assert_eq!(refs("$(V.TrimStart('vV'))"), vec!["V"]);
        assert_eq!(refs("$(V.Split('-')[0])"), vec!["V"]);
        assert_eq!(refs("$(P.Contains('{'))"), vec!["P"]);
        assert_eq!(refs("$(P.contains('{'))"), vec!["P"]);
    }

    #[test]
    fn interior_whitespace_is_tolerated() {
        // MSBuild-legal `$( … )` spellings must still expose the receiver so
        // the taint/unpinned scans that share this extractor stay in sync
        // with the whitespace-trimming condition tokeniser.
        assert_eq!(refs("$( Foo )"), vec!["Foo"]);
        assert_eq!(refs("$(  Foo)"), vec!["Foo"]);
        assert_eq!(refs("$(P.Contains ('x'))"), vec!["P"]);
        assert_eq!(refs("$( P.Contains('x') )"), vec!["P"]);
    }
}

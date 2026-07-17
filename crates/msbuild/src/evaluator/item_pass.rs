//! The MSBuild *item pass*: evaluation of `<ItemGroup>` contents —
//! Compile/CompileBefore/CompileAfter ordering, `<ProjectReference>`,
//! `<PackageReference>` / `<FrameworkReference>` / CPM item capture,
//! generic helper item lists, item conditions, and item metadata.
//!
//! Split out of the parent walker so the property/import pass and the item
//! pass are separate units: MSBuild finalises every property (across the
//! whole import graph) before evaluating any item, and keeping the two
//! passes in one module made that ordering easy to violate.

use super::*;
use crate::properties::escaping;

/// The fragments of an expanded item spec.
///
/// MSBuild splits an item spec on the `;` characters of the **escaped** text,
/// so an escaped `%3b` is a literal semicolon that does *not* split the list —
/// and the same goes for every other classification the spec undergoes
/// (`*`/`?` globbing, `@(…)` item references, `%(…)` metadata references): all
/// are decided on escaped text, which is why `%2a` is a literal star rather
/// than a wildcard. Fragments therefore come back **still escaped**; each one
/// leaves the domain at its own point of use, below.
fn spec_fragments(spec: &Escaped) -> impl Iterator<Item = &str> {
    spec.as_escaped()
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// A fragment at its point of use: unescaped exactly once, giving the identity
/// MSBuild records (an `Include="a%20b.fs"` names the file `a b.fs`).
fn fragment_identity(fragment: &str) -> String {
    escaping::unescape(fragment)
}

/// A fragment handed to the **glob resolver**, which splits on `;` and parses
/// `*`/`?` itself, downstream of us.
///
/// Unescaping a `%3b`/`%2a`/`%3f` before that seam would smuggle a
/// metacharacter past the classification that has already happened — the
/// resolver would re-split one item into two, or glob a literal star. Those
/// fragments decline (`None`), which is what they did before the escaped domain
/// existed. Stage E4 of `docs/msbuild-escaped-value-plan.md` deletes this guard
/// by handing the resolver a fragment list it never re-scans; until then the
/// guard is what keeps the seam sound.
fn fragment_for_resolver(fragment: &str) -> Option<String> {
    (!decodes_to_metacharacter(fragment)).then(|| escaping::unescape(fragment))
}

/// Whether any `%XX` in `fragment` decodes to a character the glob resolver
/// would re-scan as syntax. See [`fragment_for_resolver`].
fn decodes_to_metacharacter(fragment: &str) -> bool {
    escaping::decodes_to_any(fragment, &[';', '*', '?'])
}

/// A scalar value (item metadata, a `CompileOrder`) at its point of use, as the
/// pair every such leaf needs: the trimmed **escaped** text to scan and to echo
/// in diagnostics, and the unescaped value to record.
///
/// The order is load-bearing. Trimming happens *in* the domain, because an
/// escaped `%20` is a literal space MSBuild keeps — trimming after unescaping
/// would eat it. The `@(…)`/`%(…)` scan is likewise on escaped text, so an
/// escaped `%25(` is not a metadata reference. The value leaves the domain last.
fn scalar_use(value: &Escaped) -> (&str, String) {
    let escaped = value.as_escaped().trim();
    (escaped, escaping::unescape(escaped))
}

/// Which file the walker is currently positioned in, for the purpose of
/// deferring that file's `<ItemGroup>`s to the item pass. [`State`] holds
/// one; [`super::walk_external_file`] saves/replaces/restores it around each
/// imported file, in the same frame discipline as `MSBuildThisFile`.
#[derive(Clone)]
pub(super) enum CurrentFile {
    /// The entry project. Its `Document` outlives the whole walk (the
    /// caller owns it), so deferred groups replay against it directly.
    Entry,
    /// An imported file, whose `Document` is dropped when the property
    /// pass leaves it. `retained` is the index into
    /// [`State::retained_imported_files`] once the first deferred group
    /// forces this walk-frame's source to be kept (lazily — files with no
    /// item groups are never retained).
    Imported {
        /// Pre-canonical path — MSBuild's identity for the file, and the
        /// source of its `MSBuildThisFile{,Directory}` binding at replay.
        path: PathBuf,
        retained: Option<usize>,
    },
}

/// An imported file whose full source is kept alive so its deferred
/// `<ItemGroup>`s can be re-parsed and replayed in the item pass. Retaining
/// the *whole file* (rather than slicing out group text) keeps every byte
/// range in the replayed document identical to the property-pass parse — no
/// span arithmetic, no namespace reconstruction.
pub(super) struct RetainedImportedFile {
    path: PathBuf,
    source: String,
}

/// Which retained document a [`DeferredItemGroup`] lives in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeferredFileKey {
    Entry,
    Imported(usize),
}

/// Which construct a [`DeferredItemGroup`] is, deciding both its replay
/// order and its handler. MSBuild evaluates ALL `<ItemDefinitionGroup>`s
/// (its pass 2) before ANY `<ItemGroup>` (pass 3), so the replay runs the
/// definition groups first regardless of document interleaving.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DeferredGroupKind {
    ItemDefinitionGroup,
    ItemGroup,
}

impl DeferredGroupKind {
    fn tag_name(self) -> &'static str {
        match self {
            DeferredGroupKind::ItemDefinitionGroup => "ItemDefinitionGroup",
            DeferredGroupKind::ItemGroup => "ItemGroup",
        }
    }
}

/// One `<ItemGroup>` / `<ItemDefinitionGroup>` recorded by the property
/// pass for later evaluation. roxmltree parsing is deterministic, so
/// `node_id` resolves to the same element when the retained source is
/// re-parsed; `range` cross-checks that at replay (a mismatch would mean
/// we're about to evaluate the wrong element, which must fail fast rather
/// than corrupt the item set).
pub(super) struct DeferredItemGroup {
    kind: DeferredGroupKind,
    file: DeferredFileKey,
    node_id: roxmltree::NodeId,
    range: Range<usize>,
    /// The values [`State::import_site_span`] / [`State::in_sdk_subtree`]
    /// held when the property pass encountered the group — reinstated
    /// around its replay so spans, origins, and SDK tolerance behave
    /// exactly as they would have under inline evaluation.
    import_site_span: Option<Range<usize>>,
    in_sdk_subtree: bool,
}

impl<'r> State<'r> {
    /// Record an `<ItemGroup>` / `<ItemDefinitionGroup>` for the item pass
    /// instead of evaluating it now. MSBuild finalises every property
    /// (across the entire import graph) before evaluating any item
    /// definition or item, so the group's condition, includes, and metadata
    /// must not be resolved against the properties-so-far table the
    /// property pass is still building.
    pub(super) fn defer_item_group(&mut self, node: Node<'_, '_>, kind: DeferredGroupKind) {
        let file = match self.current_file.clone() {
            CurrentFile::Entry => DeferredFileKey::Entry,
            CurrentFile::Imported { path, retained } => {
                let index = match retained {
                    Some(index) => index,
                    None => {
                        let index = self.retained_imported_files.len();
                        self.retained_imported_files.push(RetainedImportedFile {
                            path: path.clone(),
                            source: node.document().input_text().to_string(),
                        });
                        self.current_file = CurrentFile::Imported {
                            path,
                            retained: Some(index),
                        };
                        index
                    }
                };
                DeferredFileKey::Imported(index)
            }
        };
        self.deferred_item_groups.push(DeferredItemGroup {
            kind,
            file,
            node_id: node.id(),
            range: node.range(),
            import_site_span: self.import_site_span.clone(),
            in_sdk_subtree: self.in_sdk_subtree,
        });
    }
}

/// The item pass: evaluate every deferred group against the now-final
/// property table — all `<ItemDefinitionGroup>`s first (MSBuild's pass 2),
/// then all `<ItemGroup>`s (pass 3), each set in the order the property
/// pass encountered it. Called once per walk, after the property pass has
/// consumed the entry body and every import.
pub(super) fn replay_deferred_item_groups(entry_doc: &Document<'_>, state: &mut State<'_>) {
    let groups = std::mem::take(&mut state.deferred_item_groups);
    let files = std::mem::take(&mut state.retained_imported_files);
    let definition_groups = groups
        .iter()
        .filter(|group| group.kind == DeferredGroupKind::ItemDefinitionGroup);
    let item_groups = groups
        .iter()
        .filter(|group| group.kind == DeferredGroupKind::ItemGroup);
    for group in definition_groups.chain(item_groups) {
        // Reinstate the positional context the group was recorded under, so
        // diagnostics/origins/SDK tolerance match inline evaluation.
        let saved_span =
            std::mem::replace(&mut state.import_site_span, group.import_site_span.clone());
        let saved_sdk = std::mem::replace(&mut state.in_sdk_subtree, group.in_sdk_subtree);
        let evaluate = match group.kind {
            DeferredGroupKind::ItemDefinitionGroup => evaluate_item_definition_group,
            DeferredGroupKind::ItemGroup => evaluate_item_group,
        };
        match group.file {
            DeferredFileKey::Entry => {
                // The entry file's `MSBuildThisFile*` bindings are already
                // live (every property-pass frame restored them on exit).
                evaluate(resolve_deferred_node(entry_doc, group), state);
            }
            DeferredFileKey::Imported(index) => {
                let file = &files[index];
                let doc = Document::parse(&file.source)
                    .expect("retained source parsed successfully in the property pass");
                // MSBuild rebinds `MSBuildThisFile{,Directory}` to the
                // *defining* file during the item pass too — an imported
                // item's `$(MSBuildThisFileDirectory)Extra.fs` resolves
                // against the import's directory, not the entry project's.
                let frame = state.enter_this_file(&file.path);
                evaluate(resolve_deferred_node(&doc, group), state);
                state.exit_this_file(frame);
            }
        }
        state.import_site_span = saved_span;
        state.in_sdk_subtree = saved_sdk;
    }
    // All items captured: collapse Include + Update into the effective set and
    // detect the versionless symptom on it (both need every item present).
    finalize_package_references(state);
}

/// Resolve a deferred group's node in a (re-)parsed document, verifying it
/// is the element the property pass recorded. roxmltree is deterministic so
/// this cannot fail for a faithfully retained source; if it ever does,
/// evaluating a different element would silently corrupt the item set, so
/// crash instead (correctness over availability).
fn resolve_deferred_node<'a, 'input>(
    doc: &'a Document<'input>,
    group: &DeferredItemGroup,
) -> Node<'a, 'input> {
    let node = doc
        .get_node(group.node_id)
        .expect("deferred node id resolves in its own document");
    assert_eq!(
        node.range(),
        group.range,
        "re-parsed document diverged from the property-pass parse"
    );
    assert_eq!(node.tag_name().name(), group.kind.tag_name());
    node
}

/// Evaluate one `<ItemDefinitionGroup>` against the final property table
/// (MSBuild's pass 2 — after all properties, before any item).
///
/// Item-definition *defaults* are not threaded into captured items; the
/// group is still an `UnsupportedConstruct` (flipping `is_partial`), a
/// default `CompileOrder` makes the Compile order untrustworthy, and a
/// default on a modelled dependency item type marks the package set
/// uncertain. Defaults for custom helper item types are recorded by
/// metadata name so a later `PackageReference Include="@(Helper)"` that
/// would inherit one degrades at consumption.
fn evaluate_item_definition_group(node: Node<'_, '_>, state: &mut State<'_>) {
    // The group's condition reads the FINAL property table here, so a
    // cleanly false gate means MSBuild skips the definitions in every
    // build — no default can apply, and no uncertainty follows. Only the
    // UnsupportedConstruct diagnostic below stays unconditional: the
    // construct itself is unmodelled, and `is_partial` reports that
    // independently of whether this particular gate fired.
    let may_run = item_child_condition_may_run(node, state);
    let compile_order_default_affecting =
        may_run && !state.in_sdk_subtree && item_definition_group_sets_compile_order(node);
    let prev_compile = state.compile_context;
    state.compile_context = prev_compile || compile_order_default_affecting;
    state.push(
        DiagnosticKind::UnsupportedConstruct {
            element: node.tag_name().name().to_string(),
        },
        node.range(),
    );
    state.compile_context = prev_compile;
    if !may_run {
        return;
    }
    // A default on a modelled dependency item type — e.g.
    // `<PackageReference><Version>1.2.3</Version></PackageReference>` gives
    // every `<PackageReference Include="A"/>` version 1.2.3. We don't
    // thread item-definition defaults into the capture, so a package ref
    // relying on one would be recorded with the wrong metadata; mark the
    // set uncertain instead. But only a default on a metadata we actually
    // *capture* can perturb the captured set — a default on an uncaptured
    // metadata (the F# SDK's `<PackageReference><GeneratePathProperty>` is the
    // motivating case) leaves every captured field untouched, so it is inert.
    if item_definition_defines_captured_package_metadata(node, state) {
        state.package_references_uncertain = true;
        state.record_package_reference_uncertainty(
            PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault,
            node.range(),
        );
    }
    // A `<ProjectReference>` definition rewrites reference-semantics
    // metadata on EVERY ProjectReference item, before or after it in
    // document order (pass 2 precedes pass 3). Probed (dotnet 10,
    // 2026-07-10): a `ReferenceOutputAssembly=false` default lands on the
    // evaluated item (`-getItem:ProjectReference`) and empties
    // `ReferencePath`, while our captured metadata still reads as a full
    // reference — the captured list can't be trusted. Only an *edge-affecting*
    // default with a real value does this, though: the real SDK's own inert
    // ProjectReference item-definition (blank `Targets`, empty `OutputItemType`,
    // `ReferenceSourceTarget`) must not poison the list.
    if item_definition_defines_project_reference_metadata(node, state) {
        state.project_references_uncertain = true;
    }
    record_helper_item_definition_defaults(node, state);
}

/// Whether an `<ItemDefinitionGroup>` declares at least one
/// `<ProjectReference>` metadata default that could change what a reference
/// contributes in a real build. The group gate was already screened by the
/// caller; re-screen it and each definition gate under the stricter
/// reference-list trust policy (a *trusted* clean false cannot apply; a
/// merely-undefined read is exact under the environment model).
///
/// A default flags the list only when it could change what a reference
/// contributes. Two things make it inert, and we require *both* to be false:
///
/// - **The name is inert in MSBuild's P2P protocol regardless of value**
///   ([`is_inert_project_reference_metadata`]). This is a deliberately tiny,
///   verified denylist rather than an allowlist of *significant* names: the
///   compile-time reference set (`_ResolvedProjectReferencePaths`) is gated
///   solely on `ReferenceOutputAssembly=='true'`, and a long tail of names
///   (`SetTargetFramework`, `SkipGetPlatformProperties`, `Properties`, …)
///   redirect which build of the target is produced. Enumerating that tail to
///   stay sound is a standing completeness burden; treating every *un*-listed
///   name as potentially edge-affecting is the conservative direction
///   (under-resolve, never wrong) and needs no such enumeration.
/// - **The evaluated value is empty** — MSBuild's own default for every
///   relevant metadatum (`ReferenceOutputAssembly` absent → `true`, the asset
///   lists absent → their defaults, `Targets` blank → the default targets), so
///   an empty default cannot differ from no default at all.
///
/// Together these keep the real F# SDK's own `<ItemDefinitionGroup>
/// <ProjectReference>` — which sets only `<Targets>$(ProjectReferenceBuildTargets)
/// </Targets>` (blank), an empty `<OutputItemType/>`, and
/// `<ReferenceSourceTarget>` — from poisoning the reference list of essentially
/// every real project. An untrusted/inexact value declines (the merge can't
/// reproduce it), and the value read runs through [`resolve_string_metadata`]
/// so metadatum-level `Condition`s and the escaped-value domain are handled
/// exactly as in item capture.
fn item_definition_defines_project_reference_metadata(
    node: Node<'_, '_>,
    state: &mut State<'_>,
) -> bool {
    if !reference_gate_may_run(node, state) {
        return false;
    }
    // Collect the definition children first: the value read below borrows
    // `state` mutably, so it cannot run inside a `state`-borrowing iterator.
    let definitions: Vec<Node<'_, '_>> = node
        .children()
        .filter(Node::is_element)
        .filter(|child| modelled_item_kind_for_element(*child) == Some(ItemKind::ProjectReference))
        .collect();
    for definition in definitions {
        if !reference_gate_may_run(definition, state) {
            continue;
        }
        // The distinct not-inert-by-name metadata names this definition
        // declares (as attributes or child elements). Collected first (owned)
        // because the value read borrows `state` mutably.
        let names = potentially_edge_affecting_metadata_names_on(definition);
        for name in names {
            // Empty / absent ⇒ `Known(None)` (MSBuild's own default, inert);
            // a real value ⇒ `Known(Some(_))`; an untrusted read ⇒ `Unknown`.
            // Flag the latter two.
            if matches!(
                resolve_string_metadata(definition, state, &name),
                ItemMetadataValue::Known(Some(_)) | ItemMetadataValue::Unknown
            ) {
                return true;
            }
        }
    }
    false
}

/// Metadata that is inert on a `<ProjectReference>` in MSBuild's P2P protocol
/// *regardless of value* — verified against `Microsoft.Common.CurrentVersion.targets`
/// (dotnet 8/10). Neither removes the target from the compile-time reference set
/// (`_ResolvedProjectReferencePaths` is gated only on
/// `ReferenceOutputAssembly=='true'`) nor redirects which build of the target is
/// produced: `OutputItemType` merely *adds* the output to an extra item type
/// (removing it from `ReferencePath` needs `ReferenceOutputAssembly=false` too),
/// and `ReferenceSourceTarget` is the provenance marker the protocol itself
/// stamps (`ProjectReference`). Kept deliberately small — an over-short list only
/// costs precision (a genuinely-inert name declines conservatively), never
/// soundness, whereas a too-long one could trust a redirected edge. `Private`
/// (copy-local) is inert too but omitted on purpose: declining a blanket
/// `<Private>` default is a cheap, rare under-resolution.
fn is_inert_project_reference_metadata(name: &str) -> bool {
    const INERT: &[&str] = &["OutputItemType", "ReferenceSourceTarget"];
    INERT.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// The distinct (case-insensitive) *not-inert-by-name* metadata names a
/// `<ProjectReference>` item-definition declares, whether as attributes or
/// child elements. Deduped so each is value-checked once.
fn potentially_edge_affecting_metadata_names_on(definition: Node<'_, '_>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        if !is_inert_project_reference_metadata(name)
            && !names.iter().any(|seen| seen.eq_ignore_ascii_case(name))
        {
            names.push(name.to_string());
        }
    };
    for attr in definition.attributes() {
        if is_item_metadata_attribute(attr.name()) {
            push(attr.name());
        }
    }
    for child in definition.children().filter(Node::is_element) {
        push(child.tag_name().name());
    }
    names
}

/// The metadata names a captured [`crate::PackageReference`] /
/// [`crate::GlobalPackageReference`] / [`crate::PackageVersion`] /
/// [`crate::FrameworkReference`] actually records. The uncertainty contract is
/// about *these* fields: `package_references_uncertain == false` promises the
/// resolver that our captured id + version + assets match MSBuild's. A metadata
/// outside this set is one we never read, so nothing we produce can depend on
/// it. Keep in lock-step with the struct fields in `lib.rs`.
fn captured_package_metadata_names(kind: PackageItemKind) -> &'static [&'static str] {
    match kind {
        // Both carry the full metadata set (`lib.rs` `PackageReference` /
        // `GlobalPackageReference`).
        PackageItemKind::PackageReference | PackageItemKind::GlobalPackageReference => &[
            "version",
            "versionoverride",
            "includeassets",
            "excludeassets",
            "privateassets",
        ],
        // `PackageVersion` records only its `Version` (central-package metadata).
        PackageItemKind::PackageVersion => &["version"],
        // `FrameworkReference` records only its identity — no metadata at all,
        // so every default on it is inert.
        PackageItemKind::FrameworkReference => &[],
    }
}

/// Whether an `<ItemDefinitionGroup>` declares a default for a metadata we
/// *capture* on a modelled dependency item type — the only kind of default
/// that can make the captured package set diverge from MSBuild's (we do not
/// thread item-definition defaults into the capture). A default on a metadata
/// we never record (e.g. `GeneratePathProperty`, `NoWarn`) cannot perturb any
/// captured field, and an item definition never adds or removes items, so
/// identity is safe too — hence such a group is inert and raises no
/// uncertainty. The group gate was already screened by the caller; screen each
/// child's and each metadatum's own `Condition` (a cleanly-false gate cannot
/// apply; an untrusted one still might), mirroring
/// [`item_definition_defines_project_reference_metadata`]. Metadata appears as
/// child elements or as attributes ([`is_item_metadata_attribute`]).
fn item_definition_defines_captured_package_metadata(
    node: Node<'_, '_>,
    state: &State<'_>,
) -> bool {
    node.children()
        .filter(Node::is_element)
        .filter_map(|child| package_item_kind_for_element(child).map(|kind| (child, kind)))
        .filter(|(child, _)| reference_gate_may_run(*child, state))
        .any(|(child, kind)| {
            let captured = captured_package_metadata_names(kind);
            let names_a_captured = |name: &str| {
                captured
                    .iter()
                    .any(|captured| name.eq_ignore_ascii_case(captured))
            };
            child.attributes().any(|attr| {
                is_item_metadata_attribute(attr.name()) && names_a_captured(attr.name())
            }) || child
                .children()
                .filter(Node::is_element)
                .filter(|metadata| reference_gate_may_run(*metadata, state))
                .any(|metadata| names_a_captured(metadata.tag_name().name()))
        })
}

/// Evaluate one `<ItemGroup>` — its own `Condition` gate and then each
/// child item — against the current property table.
fn evaluate_item_group(node: Node<'_, '_>, state: &mut State<'_>) {
    // A group that contributes Compile items makes its own condition
    // Compile-affecting: an undefined-property or unmodeled condition
    // here decides whether those files compile. Set the context around
    // both the condition check and the body walk (so each Compile
    // child's own condition is covered too), then restore.
    //
    // Not inside the SDK installation tree (`in_sdk_subtree`): the SDK's
    // own targets/props are full of conditional default-item machinery
    // (`<ItemGroup Condition="'$(EnableDefaultItems)' == 'true'"><Compile
    // Include="**/*.fs"/></ItemGroup>`, the link-metadata `<Compile
    // Update=…>` group) gated on properties we don't resolve. Treating
    // those as Compile-affecting would flag essentially every real
    // project (the very breakage this distinction fixes) and they never
    // decide which *hand-written* sources compile. The same construct in
    // the entry project or a user import (`Directory.Build.*`, an
    // explicit `<Import>`) is respected.
    // Set `compile_context` for the group's *own* condition only (it
    // gates the inclusion of any Compile children), then restore before
    // walking the children — each child manages its own context in
    // `walk_item_child`, so a non-Compile sibling isn't tainted.
    // Set both context flags for the group's *own* condition (it
    // gates the inclusion of any Compile / package-reference
    // children), then restore before walking the children — each
    // child re-manages its own context in `walk_item_child`.
    let prev = state.compile_context;
    let prev_pkg = state.package_context;
    let has_package_child = item_group_has_package_child(node);
    // A helper-only or modelled-item group can still feed the package
    // set later via `<PackageReference Include="@(SomeItem)" />`,
    // including from SDK props/targets. Taint helper lists and mark
    // modelled lists untracked when the group gate is untrusted; the
    // package flag flips only if a package/framework reference
    // actually consumes the list.
    let item_lists_tainted_by_group_condition = item_lists_gated_by_group_condition(node, state);
    let group_condition_tainted = !item_lists_tainted_by_group_condition.is_empty()
        && evaluate_item_condition_silent_with_sdk_taint(node, state).1;
    // The group's own gate can hide (or phantom-include) whole reference
    // list operations; work out what its children could do before deciding
    // the gate, so each arm below can poison the list appropriately.
    let reference_risk = project_reference_group_risk(node, state);
    let group_reads_untrusted = state.condition_reads_untrusted_value(node);
    state.compile_context = prev || (!state.in_sdk_subtree && item_group_has_compile_child(node));
    state.package_context = prev_pkg || has_package_child;
    if has_package_child {
        state.note_package_uncertain_if_condition_uses_sdk_taint(node);
    }
    let gate = evaluate_item_condition(node, state);
    if group_condition_tainted {
        item_lists_tainted_by_group_condition.apply(state);
    }
    let restore = |state: &mut State<'_>| {
        state.compile_context = prev;
        state.package_context = prev_pkg;
    };
    match gate {
        CondGate::Run => {
            // Walked only on an untrusted true: any reference the children
            // add may be absent from the real build — a phantom edge.
            // (Mutating children flag themselves when walked.)
            if group_reads_untrusted && reference_risk.list_op {
                state.project_references_uncertain = true;
            }
            restore(state);
            for child in node.children().filter(Node::is_element) {
                walk_item_child(child, state);
            }
        }
        CondGate::Skip => {
            // Skipped only on an untrusted false: a hidden mutation may run
            // in the real build against the list we captured. Skipped
            // Includes are at worst missed references.
            if group_reads_untrusted && reference_risk.mutation {
                state.project_references_uncertain = true;
            }
            restore(state);
        }
        CondGate::Unsupported => {
            // Record the carve-outs while both contexts are still set.
            emit_unsupported_condition(node, state);
            // An unsupported gate is untrusted by construction: probed
            // (dotnet 10, 2026-07-10) — an `<ItemGroup>` on a
            // property-function condition containing `<ProjectReference
            // Update ReferenceOutputAssembly=false>` empties
            // `ReferencePath` while we still carry the un-mutated Include.
            if reference_risk.mutation {
                state.project_references_uncertain = true;
            }
            restore(state);
        }
    }
}

/// What the `<ProjectReference>` children of an `<ItemGroup>` could do to
/// the reference list if the group runs, for deciding whether the group's
/// own gate can hide a divergence. A child whose own condition is a
/// *trusted* clean false can never run (environment model — see
/// [`State::condition_reads_untrusted_value`]); anything else may.
struct ProjectReferenceGroupRisk {
    /// A child may Update/Remove — falsifying references already captured.
    mutation: bool,
    /// A child may perform any list operation (Include/Update/Remove).
    list_op: bool,
}

fn project_reference_group_risk(
    node: Node<'_, '_>,
    state: &State<'_>,
) -> ProjectReferenceGroupRisk {
    let mut risk = ProjectReferenceGroupRisk {
        mutation: false,
        list_op: false,
    };
    for child in node.children().filter(Node::is_element) {
        if modelled_item_kind_for_element(child) != Some(ItemKind::ProjectReference) {
            continue;
        }
        let mutation = child.attribute("Update").is_some() || child.attribute("Remove").is_some();
        if !mutation && child.attribute("Include").is_none() {
            continue;
        }
        if !reference_gate_may_run(child, state) {
            continue;
        }
        risk.list_op = true;
        risk.mutation = risk.mutation || mutation;
    }
    risk
}

/// Whether a node's own `Condition` could let it run in a real build, under
/// the reference-list trust policy: only a *trusted* clean false — decided
/// without unpinned or SDK-tainted reads — proves it cannot.
fn reference_gate_may_run(node: Node<'_, '_>, state: &State<'_>) -> bool {
    match evaluate_item_condition_silent(node, state).0 {
        CondGate::Run | CondGate::Unsupported => true,
        CondGate::Skip => state.condition_reads_untrusted_value(node),
    }
}

/// Whether an `<ItemGroup>` (e.g. an undecided `<Choose>` branch's) contains
/// a `<ProjectReference Update/Remove>` child. Purely structural — inside an
/// undecided branch the property table the child's own `Condition` would
/// read is itself unsettled, so no gate there is trustworthy.
pub(super) fn item_group_contains_project_reference_mutation(node: Node<'_, '_>) -> bool {
    node.children().filter(Node::is_element).any(|child| {
        modelled_item_kind_for_element(child) == Some(ItemKind::ProjectReference)
            && (child.attribute("Update").is_some() || child.attribute("Remove").is_some())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompileOrderSlot {
    CompileFirst,
    ExplicitCompileBefore,
    CompileBefore,
    Compile,
    CompileAfter,
    ExplicitCompileAfter,
    CompileLast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompileOrderEffect {
    Slot(CompileOrderSlot),
    Excluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemPlacement {
    Compile(CompileOrderEffect),
    ProjectReference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageItemKind {
    PackageReference,
    FrameworkReference,
    PackageVersion,
    GlobalPackageReference,
}

fn walk_item_child(node: Node<'_, '_>, state: &mut State<'_>) {
    // PackageReference/FrameworkReference are the NuGet dependency set — a
    // separate concern from Compile order / project edges, with their own
    // bucket, metadata, and uncertainty flag. SDK dependency items participate
    // in the same capture machinery as user-authored ones: they evaluate
    // against the final property table, so a cleanly-evaluated SDK item is
    // captured with certainty, and only concrete unevaluable constructs (or
    // taint from an untrusted SDK property write) degrade the set.
    match package_item_kind_for_element(node) {
        Some(PackageItemKind::PackageReference) => {
            let prev = state.package_context;
            state.package_context = true;
            walk_package_reference(node, state);
            state.package_context = prev;
            return;
        }
        Some(PackageItemKind::FrameworkReference) => {
            let prev = state.package_context;
            state.package_context = true;
            walk_framework_reference(node, state);
            state.package_context = prev;
            return;
        }
        Some(
            kind @ (PackageItemKind::PackageVersion | PackageItemKind::GlobalPackageReference),
        ) => {
            let prev = state.package_context;
            state.package_context = true;
            match kind {
                PackageItemKind::PackageVersion => walk_package_version(node, state),
                PackageItemKind::GlobalPackageReference => {
                    walk_global_package_reference(node, state)
                }
                PackageItemKind::PackageReference | PackageItemKind::FrameworkReference => {
                    unreachable!("unexpected dependency item")
                }
            }
            state.package_context = prev;
            return;
        }
        None => {}
    }
    let Some(kind) = modelled_item_kind_for_element(node) else {
        walk_generic_item_child(node, state);
        return;
    };
    // Only a Compile-flavoured item's *inclusion* decisions (its condition,
    // item operations, and Include path) can change which sources compile, and
    // only outside the SDK tree. Scope `compile_context` to exactly that span,
    // so an unrelated diagnostic in the same group (a `<ProjectReference>`
    // problem, or `Link="$(Missing)"` display metadata) does not spuriously
    // mark the Compile set uncertain. Single restore on return.
    let compile_affecting = is_compile_item_kind(kind)
        && !state.in_sdk_subtree
        && (!is_metadata_only_item_update(node)
            || (kind == ItemKind::Compile && compile_item_sets_compile_order(node)));
    let prev = state.compile_context;
    state.compile_context = compile_affecting;
    walk_item_child_inner(node, kind, state);
    state.compile_context = prev;
}

fn walk_item_child_inner(node: Node<'_, '_>, kind: ItemKind, state: &mut State<'_>) {
    let item_type = modelled_item_type(kind);
    if modelled_item_list_operation_may_change_list(node, kind) {
        let (_, condition_tainted) = evaluate_item_condition_silent_with_sdk_taint(node, state);
        if condition_tainted {
            state.mark_untracked_item_list(item_type);
        }
    }
    // A reference list operation decided by an *untrusted* read (an unpinned
    // or SDK-tainted property — not a merely-undefined one, which the
    // environment model resolves exactly) may go the other way in a real
    // build. Which direction poisons the list depends on the outcome below.
    let untrusted_reference_op = kind == ItemKind::ProjectReference
        && modelled_item_list_operation_may_change_list(node, kind)
        && state.condition_reads_untrusted_value(node);
    let is_mutation = node.attribute("Update").is_some() || node.attribute("Remove").is_some();
    match evaluate_item_condition(node, state) {
        CondGate::Run => {
            // Kept only on an untrusted true: an Include the real build may
            // not make would be a phantom edge. (A mutation that runs is
            // flagged by the Update/Remove gate below regardless.)
            if untrusted_reference_op {
                state.project_references_uncertain = true;
            }
        }
        CondGate::Skip => {
            // Excluded only by an untrusted false: the real build may run
            // the mutation against the list we captured. A dropped Include
            // is at worst a missed reference, which never fabricates.
            if untrusted_reference_op && is_mutation {
                state.project_references_uncertain = true;
            }
            return;
        }
        CondGate::Unsupported => {
            emit_unsupported_condition(node, state);
            // A mutation behind a condition we can't evaluate may still run
            // in the real build — the captured list can't be trusted either
            // way (see the Update/Remove gate below).
            if kind == ItemKind::ProjectReference && is_mutation {
                state.project_references_uncertain = true;
            }
            return;
        }
    }
    if is_compile_item_kind(kind) && is_metadata_only_item_update(node) {
        if kind == ItemKind::Compile && compile_item_sets_compile_order(node) {
            apply_compile_order_update(node, state);
        }
        return;
    }
    if node.attribute("Include").is_some()
        || node.attribute("Update").is_some()
        || node.attribute("Remove").is_some()
    {
        state.mark_untracked_item_list(item_type);
    }
    if diagnose_item_op(node, "Update", state) || diagnose_item_op(node, "Remove", state) {
        // The mutation is skipped, so earlier `Include`s stand un-mutated in
        // the captured list. MSBuild honours both (dotnet 10 probe: a
        // `Remove`, or an `Update` writing `ReferenceOutputAssembly=false`,
        // strips the reference from `ReferencePath`), so the list may now
        // claim references the real build doesn't make — flag it rather than
        // let a reference-semantics consumer fold a DLL from it.
        if kind == ItemKind::ProjectReference {
            state.project_references_uncertain = true;
        }
        return;
    }
    // `Exclude` is only an unsupported operation when there's no glob
    // resolver to honour it; with one present the element routes through
    // the glob seam below (Exclude applies to literal includes too).
    if state.glob_resolver.is_none() && diagnose_item_op(node, "Exclude", state) {
        return;
    }
    let placement = match kind {
        ItemKind::ProjectReference => ItemPlacement::ProjectReference,
        ItemKind::Compile | ItemKind::CompileBefore | ItemKind::CompileAfter => {
            let Some(effect) = compile_order_effect(node, kind, state) else {
                return;
            };
            ItemPlacement::Compile(effect)
        }
    };
    let Some(include) = node.attribute("Include") else {
        return;
    };
    // `<Link>` metadata is meaningful for Compile items (it controls
    // the path shown in IDEs / solution explorers); MSBuild does not
    // treat it as significant for `<ProjectReference>`, and exposing
    // a `Some(...)` link there would invite consumers to use a value
    // that has no effect on a real build. Conversely
    // `ReferenceOutputAssembly` / `ExcludeAssets` shape what a
    // `<ProjectReference>` contributes to the consumer's reference set and
    // are meaningless on Compile items.
    let metadata = match kind {
        ItemKind::ProjectReference => ItemMetadata {
            link: None,
            reference_output_assembly: resolve_string_metadata(
                node,
                state,
                "ReferenceOutputAssembly",
            ),
            exclude_assets: resolve_string_metadata(node, state, "ExcludeAssets"),
            include_assets: resolve_string_metadata(node, state, "IncludeAssets"),
            private_assets: resolve_string_metadata(node, state, "PrivateAssets"),
            unmodelled_reference_metadata: project_reference_has_unmodelled_significant_metadata(
                node, state,
            ),
        },
        ItemKind::Compile | ItemKind::CompileBefore | ItemKind::CompileAfter => {
            // `<Link>` controls only the display path, never which file
            // compiles — clear `compile_context` so an undefined property in
            // the link doesn't mark the (already-known) Compile item uncertain.
            let saved = state.compile_context;
            state.compile_context = false;
            // Display-only metadata: an Unknown resolution degrades to "no
            // link" rather than poisoning anything (ResolvedItem::link docs).
            let link = match resolve_string_metadata(node, state, "Link") {
                ItemMetadataValue::Known(value) => value,
                ItemMetadataValue::Unknown => None,
            };
            state.compile_context = saved;
            ItemMetadata {
                link,
                reference_output_assembly: ItemMetadataValue::ABSENT,
                exclude_assets: ItemMetadataValue::ABSENT,
                include_assets: ItemMetadataValue::ABSENT,
                private_assets: ItemMetadataValue::ABSENT,
                unmodelled_reference_metadata: false,
            }
        }
    };
    // Substitute $(...) FIRST, then split on ';'. Property values are
    // allowed to be semicolon-delimited lists in MSBuild, so
    // `Include="$(ExtraFiles)"` may expand into multiple items even
    // though the raw attribute looks like one.
    let expansion = state.expand(include, node.range());
    if expansion.had_issue() {
        // Substitution failure leaves a path with empty fragments or
        // residual `$(...)`. Either way the result is corrupt; we refuse
        // to emit a misleading item.
        return;
    }
    // The Include VALUE can lean on an untrusted read the same way the
    // gate can: a cleanly-expanded `$(RefPath)` whose source was written
    // under a gate we couldn't pin may hold a different path — or none —
    // in the real build, so the captured edge may be phantom. Flag rather
    // than drop: the declared-structure walk keeps the element.
    taint_reference_list_on_untrusted_value(kind, include, &expansion, state);
    match state.glob_resolver {
        None => {
            for entry in spec_fragments(&expansion.value) {
                push_include_entry(node, kind, placement, entry, &metadata, state);
            }
        }
        Some(resolver) => {
            route_item_through_resolver(
                node,
                kind,
                placement,
                &expansion.value,
                &metadata,
                resolver,
                state,
            );
        }
    }
}

/// Per-element item metadata, resolved once and copied onto every
/// [`ResolvedItem`] the element's `Include` expands to.
struct ItemMetadata {
    link: Option<String>,
    reference_output_assembly: ItemMetadataValue,
    exclude_assets: ItemMetadataValue,
    include_assets: ItemMetadataValue,
    private_assets: ItemMetadataValue,
    unmodelled_reference_metadata: bool,
}

/// `<ProjectReference>` metadata this evaluator does not model but the SDK's
/// P2P protocol treats as significant for what (if anything) the reference
/// puts on `ReferencePath`. Probed (dotnet 10, 2026-07-10, prebuilt target,
/// entry edge): `BuildReference="false"` and `Targets="Clean"` both remove
/// the target from `ReferencePath`; the `Set*` / property-list names mutate
/// the referenced project's *evaluation*, so the DLL the real build
/// references may not be the one a trust-the-capture walk would locate.
/// Unrecognized names are inert in the protocol (probed: custom metadata,
/// `OutputItemType`, and `Private="false"` all keep the target on
/// `ReferencePath`), so this is a closed vocabulary, not a catch-all —
/// presence-based, value ignored (a benign value like `BuildReference="true"`
/// is vanishingly rare and dropping it only under-resolves).
fn is_unmodelled_significant_reference_metadata(name: &str) -> bool {
    [
        "BuildReference",
        "Targets",
        "SetConfiguration",
        "SetPlatform",
        "SetTargetFramework",
        "AdditionalProperties",
        "UndefineProperties",
        "GlobalPropertiesToRemove",
        "SkipGetTargetFrameworkProperties",
    ]
    .iter()
    .any(|n| n.eq_ignore_ascii_case(name))
}

/// Whether a `<ProjectReference>` element carries any
/// [`is_unmodelled_significant_reference_metadata`] name that could apply in
/// a real build — as a metadata attribute, or as a child element whose gate
/// is not a trusted clean false.
fn project_reference_has_unmodelled_significant_metadata(
    node: Node<'_, '_>,
    state: &State<'_>,
) -> bool {
    node.attributes().any(|attr| {
        is_item_metadata_attribute(attr.name())
            && is_unmodelled_significant_reference_metadata(attr.name())
    }) || node.children().filter(Node::is_element).any(|child| {
        is_unmodelled_significant_reference_metadata(child.tag_name().name())
            && reference_gate_may_run(child, state)
    })
}

/// Capture static item declarations for item types this evaluator otherwise
/// ignores. The immediate consumer is package capture: SDKs and props files
/// sometimes materialise package ids as helper items, then write
/// `<PackageReference Include="@(ThatHelper)" />`. We only keep identities and
/// literal package metadata that can be evaluated without diagnostics.
/// Unsupported constructs that affect identities taint the source list.
/// Unsupported modelled package metadata is tracked separately, because the
/// consuming dependency item may override or ignore it.
fn walk_generic_item_child(node: Node<'_, '_>, state: &mut State<'_>) {
    let item_type = node.tag_name().name();
    let has_update = node.attribute("Update").is_some();
    let remove = node.attribute("Remove");
    // The item's own condition gates the operation. A cleanly-false Remove or
    // Update is not an item-list uncertainty; MSBuild ignores it entirely.
    let (gate, condition_tainted) = evaluate_item_condition_silent_with_sdk_taint(node, state);
    if condition_tainted {
        if has_update || remove.is_some() {
            state.invalidate_item_list(item_type);
            return;
        }
        state.taint_item_list(item_type);
    }
    match gate {
        CondGate::Run => {}
        CondGate::Skip | CondGate::Unsupported => return,
    }
    if let Some(remove) = remove {
        apply_generic_item_remove(item_type, remove, state);
        return;
    }
    if has_update {
        state.invalidate_item_list(item_type);
        return;
    }
    let Some(include) = node.attribute("Include") else {
        return;
    };
    let identity_uses_sdk_taint = state.raw_uses_sdk_package_taint(include);
    let expansion = expand_silent(include, state);
    if identity_uses_sdk_taint {
        state.taint_item_list(item_type);
    }
    if expansion.had_issue()
        || contains_item_reference(expansion.value.as_escaped())
        || contains_metadata_reference(expansion.value.as_escaped())
    {
        state.taint_item_list(item_type);
        return;
    }
    let Some(excluded) = resolve_generic_exclude_set(node, state) else {
        return;
    };
    let metadata = read_generic_item_metadata(node, state);
    for identity in spec_fragments(&expansion.value) {
        if contains_glob(identity) {
            state.taint_item_list(item_type);
            continue;
        }
        // Classified on escaped text, recorded decoded — the identity is a point
        // of use, and the exclude set was decoded for the same reason.
        let identity = fragment_identity(identity);
        if excluded.contains(&identity.to_ascii_lowercase()) {
            continue;
        }
        state
            .evaluated_items
            .entry(item_key(item_type))
            .or_default()
            .push(EvaluatedItem {
                identity,
                metadata: metadata.values.clone(),
                metadata_uncertainties: metadata.uncertainties.clone(),
            });
    }
}

fn apply_generic_item_remove(item_type: &str, remove: &str, state: &mut State<'_>) {
    let Some(targets) = resolve_literal_generic_item_identities(item_type, remove, state) else {
        state.invalidate_item_list(item_type);
        return;
    };
    if targets.is_empty() {
        return;
    }
    if let Some(items) = state.evaluated_items.get_mut(&item_key(item_type)) {
        items.retain(|item| !targets.contains(&item.identity.to_ascii_lowercase()));
    }
}

fn resolve_generic_exclude_set(
    node: Node<'_, '_>,
    state: &mut State<'_>,
) -> Option<HashSet<String>> {
    let Some(exclude) = node.attribute("Exclude") else {
        return Some(HashSet::new());
    };
    let item_type = node.tag_name().name();
    let Some(excluded) = resolve_literal_generic_item_identities(item_type, exclude, state) else {
        state.taint_item_list(item_type);
        return None;
    };
    Some(excluded)
}

fn resolve_literal_generic_item_identities(
    item_type: &str,
    raw: &str,
    state: &mut State<'_>,
) -> Option<HashSet<String>> {
    let raw_uses_sdk_taint = state.raw_uses_sdk_package_taint(raw);
    let expansion = expand_silent(raw, state);
    if raw_uses_sdk_taint {
        state.taint_item_list(item_type);
    }
    if expansion.had_issue()
        || contains_item_reference(expansion.value.as_escaped())
        || contains_metadata_reference(expansion.value.as_escaped())
    {
        return None;
    }
    let mut identities = HashSet::new();
    for identity in spec_fragments(&expansion.value) {
        if contains_glob(identity) {
            state.taint_item_list(item_type);
            return None;
        }
        // Decoded, because the identities these are compared against were
        // decoded when their `Include` captured them: `Remove="Foo%2eBar"` must
        // match the item `Foo.Bar`.
        identities.insert(fragment_identity(identity).to_ascii_lowercase());
    }
    Some(identities)
}

fn compile_order_effect(
    node: Node<'_, '_>,
    kind: ItemKind,
    state: &mut State<'_>,
) -> Option<CompileOrderEffect> {
    match kind {
        ItemKind::CompileBefore => Some(CompileOrderEffect::Slot(
            CompileOrderSlot::ExplicitCompileBefore,
        )),
        ItemKind::CompileAfter => Some(CompileOrderEffect::Slot(
            CompileOrderSlot::ExplicitCompileAfter,
        )),
        ItemKind::ProjectReference => None,
        ItemKind::Compile => {
            let order = read_compile_order_metadata_write(node, state)?.unwrap_or_default();
            Some(compile_order_effect_from_value(&order))
        }
    }
}

fn compile_order_effect_from_value(order: &str) -> CompileOrderEffect {
    if order.eq_ignore_ascii_case("CompileFirst") {
        CompileOrderEffect::Slot(CompileOrderSlot::CompileFirst)
    } else if order.eq_ignore_ascii_case("CompileBefore") {
        CompileOrderEffect::Slot(CompileOrderSlot::CompileBefore)
    } else if order.is_empty() {
        CompileOrderEffect::Slot(CompileOrderSlot::Compile)
    } else if order.eq_ignore_ascii_case("CompileAfter") {
        CompileOrderEffect::Slot(CompileOrderSlot::CompileAfter)
    } else if order.eq_ignore_ascii_case("CompileLast") {
        CompileOrderEffect::Slot(CompileOrderSlot::CompileLast)
    } else {
        CompileOrderEffect::Excluded
    }
}

/// Read F#'s `CompileOrder` item metadata from a `<Compile>` element.
///
/// The F# SDK target `FSharpSourceCodeCompileOrder` does not use document-order
/// `@(Compile)` directly. It re-sorts items by their `CompileOrder` metadata
/// into `CompileFirst`, `CompileBefore`, empty, `CompileAfter`, and
/// `CompileLast` buckets, interleaved with the explicit `CompileBefore` /
/// `CompileAfter` item lists. Attribute metadata is the first write and child
/// metadata elements are later writes, so the last condition-true child wins.
///
/// The outer `Option` is the unsupported/unresolved case. The inner `Option`
/// distinguishes no effective metadata write from a write of the empty string,
/// which is significant for `<Compile Update=...>`: no write leaves the item
/// alone, while an empty write moves it to the ordinary `Compile` bucket.
fn read_compile_order_metadata_write(
    node: Node<'_, '_>,
    state: &mut State<'_>,
) -> Option<Option<String>> {
    fn expand_compile_order_value(
        raw: &str,
        span: Range<usize>,
        state: &mut State<'_>,
    ) -> Option<String> {
        let expansion = state.expand(raw, span.clone());
        if expansion.had_issue() {
            return None;
        }
        let (escaped, value) = scalar_use(&expansion.value);
        if contains_item_reference(escaped) {
            state.push(
                DiagnosticKind::UnresolvedItemReference {
                    reference: escaped.to_string(),
                },
                span,
            );
            return None;
        }
        if contains_metadata_reference(escaped) {
            state.push(
                DiagnosticKind::UnresolvedMetadataReference {
                    reference: escaped.to_string(),
                },
                span,
            );
            return None;
        }
        Some(value)
    }

    let mut chosen = match node
        .attributes()
        .rfind(|attr| attr.name().eq_ignore_ascii_case("CompileOrder"))
    {
        Some(attr) => Some(expand_compile_order_value(
            attr.value(),
            node.range(),
            state,
        )?),
        None => None,
    };
    for child in node
        .children()
        .filter(Node::is_element)
        .filter(|n| n.tag_name().name().eq_ignore_ascii_case("CompileOrder"))
    {
        match evaluate_item_condition(child, state) {
            CondGate::Run => {
                // A body we cannot model (CDATA / entity-encoded whitespace —
                // see `collect_element_text`) degrades the whole order update,
                // exactly like an unsupported condition below.
                let Some(raw) = collect_element_text(child) else {
                    state.push(
                        DiagnosticKind::UnsupportedItemOperation {
                            operation: "CompileOrder with an unmodellable element body".to_string(),
                        },
                        child.range(),
                    );
                    return None;
                };
                chosen = Some(expand_compile_order_value(&raw, child.range(), state)?);
            }
            CondGate::Skip => {}
            CondGate::Unsupported => {
                emit_unsupported_condition(child, state);
                return None;
            }
        }
    }
    Some(chosen)
}

fn apply_compile_order_update(node: Node<'_, '_>, state: &mut State<'_>) {
    let Some(order) = read_compile_order_metadata_write(node, state) else {
        return;
    };
    let Some(order) = order else {
        return;
    };
    let Some(targets) = resolve_compile_update_targets(node, state) else {
        return;
    };
    if targets.is_empty() {
        return;
    }
    let effect = compile_order_effect_from_value(&order);
    let mut moved = take_matching_compile_items(state, &targets);
    moved.sort_by_key(|item| item.order);
    for item in moved {
        insert_ordered_compile_item(state, effect, item);
    }
}

fn resolve_compile_update_targets(
    node: Node<'_, '_>,
    state: &mut State<'_>,
) -> Option<Vec<PathBuf>> {
    if diagnose_item_op(node, "Exclude", state) {
        return None;
    }
    let raw = node.attribute("Update")?;
    let expansion = state.expand(raw, node.range());
    if expansion.had_issue() {
        return None;
    }
    let mut targets = Vec::new();
    for entry in spec_fragments(&expansion.value) {
        if contains_glob(entry) {
            state.push(
                DiagnosticKind::UnsupportedGlob {
                    pattern: entry.to_string(),
                },
                node.range(),
            );
            return None;
        }
        if contains_item_reference(entry) {
            state.push(
                DiagnosticKind::UnresolvedItemReference {
                    reference: entry.to_string(),
                },
                node.range(),
            );
            return None;
        }
        if contains_metadata_reference(entry) {
            state.push(
                DiagnosticKind::UnresolvedMetadataReference {
                    reference: entry.to_string(),
                },
                node.range(),
            );
            return None;
        }
        // The identity this must match was decoded when the `Include` captured
        // it, so the target decodes too — an `Update="a%20b.fs"` names the same
        // file as `Include="a%20b.fs"`, and comparing escaped against decoded
        // would silently match nothing.
        let normalised = fragment_identity(entry).replace('\\', "/");
        targets.push(state.entry_project_dir.join(normalised));
    }
    Some(targets)
}

fn take_matching_compile_items(
    state: &mut State<'_>,
    targets: &[PathBuf],
) -> Vec<OrderedResolvedItem> {
    let mut moved = Vec::new();
    take_matching_compile_items_from_bucket(&mut state.compile_first, targets, &mut moved);
    take_matching_compile_items_from_bucket(
        &mut state.explicit_compile_before,
        targets,
        &mut moved,
    );
    take_matching_compile_items_from_bucket(&mut state.compile_before, targets, &mut moved);
    take_matching_compile_items_from_bucket(&mut state.compile_main, targets, &mut moved);
    take_matching_compile_items_from_bucket(&mut state.compile_after, targets, &mut moved);
    take_matching_compile_items_from_bucket(&mut state.explicit_compile_after, targets, &mut moved);
    take_matching_compile_items_from_bucket(&mut state.compile_last, targets, &mut moved);
    take_matching_compile_items_from_bucket(&mut state.compile_excluded, targets, &mut moved);
    moved
}

fn take_matching_compile_items_from_bucket(
    bucket: &mut Vec<OrderedResolvedItem>,
    targets: &[PathBuf],
    moved: &mut Vec<OrderedResolvedItem>,
) {
    let mut index = 0;
    while index < bucket.len() {
        let matches = bucket[index].item.kind == ItemKind::Compile
            && targets
                .iter()
                .any(|target| bucket[index].item.include == *target);
        if matches {
            moved.push(bucket.remove(index));
        } else {
            index += 1;
        }
    }
}

fn insert_ordered_compile_item(
    state: &mut State<'_>,
    effect: CompileOrderEffect,
    item: OrderedResolvedItem,
) {
    let bucket = bucket_for_compile_effect(state, effect);
    let index = bucket.partition_point(|existing| existing.order <= item.order);
    bucket.insert(index, item);
}

/// Capture a `<PackageReference>` (Include or Update form). Version and
/// asset metadata are read (attribute or child element) and `$(…)`-expanded
/// but not interpreted — that's the NuGet resolver's job. An unsupported /
/// undefined-property condition, an ignored `Remove`, or an Include that
/// expands to a corrupt/item-referencing value flips
/// [`State::package_references_uncertain`].
fn walk_package_reference(node: Node<'_, '_>, state: &mut State<'_>) {
    state.note_package_uncertain_if_condition_uses_sdk_taint(node);
    match evaluate_item_condition(node, state) {
        CondGate::Run => {}
        CondGate::Skip => return,
        CondGate::Unsupported => {
            emit_unsupported_condition(node, state);
            return;
        }
    }
    let (op, target) = if let Some(inc) = node.attribute("Include") {
        (PackageRefOp::Include, inc)
    } else if let Some(upd) = node.attribute("Update") {
        (PackageRefOp::Update, upd)
    } else {
        // Remove (or a malformed reference with no target): we don't model
        // item removal, so a Remove may make the captured set wrong.
        if node.attribute("Remove").is_some() {
            state.note_package_uncertain(
                PackageReferenceUncertaintyCauseKind::UnsupportedItemOperation {
                    item: "PackageReference".to_string(),
                    operation: "Remove".to_string(),
                },
                node.range(),
            );
        }
        return;
    };
    // An `Update` that writes no metadatum we capture cannot perturb the
    // captured set whatever it matches — an `Update` never changes identity —
    // so it is inert. Screen this *before* any target-derived side effect
    // (taint / identity resolution), since a self-referential
    // `Update="@(PackageReference)"` would otherwise be flagged unevaluable
    // even though it only stamps an uncaptured `AllowExplicitVersion`.
    if op == PackageRefOp::Update && !update_writes_captured_metadata(node, state) {
        return;
    }
    state.note_package_uncertain_from_sdk_property_taint(target);
    let sources = resolve_dependency_identity_sources(target, node, state);
    if op == PackageRefOp::Update {
        // Our `Update`→`Include` matching — the inert shortcut below and the
        // merge in `finalize_package_references` — compares raw identities with
        // `eq_ignore_ascii_case`. MSBuild instead matches item identities as
        // *normalized paths* under full Unicode `OrdinalIgnoreCase`, via
        // `Path.GetFullPath` (probed dotnet 10.0.301: `Update="ångström"`
        // matches `Include="Ångström"`; `"./A"` / `".\A"` / `"Sub/../A"` /
        // `"A%20"` — a decoded trailing space Windows trims — all match `"A"`).
        // Reproducing that per-platform normalizer is a rabbit hole, so instead
        // of a deny-list of known-bad spellings we take the inert shortcut only
        // for identities in a positive allow-list where the raw compare is
        // *provably* faithful on every platform: the NuGet package-id shape —
        // ASCII `[A-Za-z0-9._-]`, non-empty, no leading/trailing `.`. Such a
        // token contains no separator, whitespace, or trimmable boundary, so
        // `GetFullPath(dir/id)` leaves it verbatim and the shared `dir/` prefix
        // cancels; and for two ASCII strings `eq_ignore_ascii_case` *is*
        // `OrdinalIgnoreCase`. Every real package id (`Newtonsoft.Json`,
        // `Microsoft.AspNetCore.App`) qualifies; anything else — on the
        // `Update` side *or* a captured `Include` it might match — declines,
        // since a false-inert proof or a mis-dropped merge are both
        // certain-but-wrong.
        let match_is_faithful = |id: &str| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                && !id.starts_with('.')
                && !id.ends_with('.')
        };
        let has_hazard = sources
            .iter()
            .any(|source| !match_is_faithful(&source.identity))
            || state.captured_package_references.iter().any(|captured| {
                captured.op == PackageRefOp::Include && !match_is_faithful(&captured.id)
            });
        if has_hazard {
            state.note_package_uncertain(
                PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                    value: target.to_string(),
                },
                node.range(),
            );
        // An `Update` modifies only the *prior* `Include`s that share its
        // identity (probed: an `Update` declared before its `Include` does not
        // apply). So an `Update` whose resolved identities are all absent from
        // the already-captured `Include` set — with no within-spec duplicate,
        // the one shape MSBuild resolves position-independently and can thus
        // apply to a *later* `Include` — changes nothing in the effective set.
        // Reading its metadata (and evaluating any unsupported
        // `%(…)`-conditioned child, e.g. the SDK's AspNetCore
        // `<PrivateAssets Condition="'%(PackageReference.Version)' == ''">`)
        // would only manufacture a spurious uncertainty, so skip it entirely.
        } else if update_matches_no_captured_include(&sources, state) {
            return;
        }
    }
    let local_metadata = read_package_metadata(node, state);
    let excluded = resolve_exclude_set(node, state);
    let span = state.effective_span(node.range());
    let origin = state.current_origin();
    let package_count_before = state.captured_package_references.len();
    let mut reported_helper_metadata_uncertainties = HashSet::new();
    // MSBuild quirk (dotnet 10 probe): an `Update` spec that names the same
    // identity more than once is applied through the lazy evaluator's
    // dictionary path, which is position-independent (it modifies `Include`s
    // declared *later* too), while a unique spec modifies only prior ones.
    // `finalize_package_references` models the ordered semantics only, so a
    // duplicate-identity Update poisons the captured set.
    let mut update_ids_seen: HashSet<String> = HashSet::new();
    for source in sources {
        let id = source.identity.clone();
        if op == PackageRefOp::Update && !update_ids_seen.insert(id.to_ascii_lowercase()) {
            state.note_package_uncertain(
                PackageReferenceUncertaintyCauseKind::DuplicateUpdateIdentity { id: id.clone() },
                node.range(),
            );
        }
        // MSBuild removes `Exclude`d identities from the `Include` set,
        // case-insensitively (`excluded` is lowercased).
        if excluded.contains(&id.to_ascii_lowercase()) {
            continue;
        }
        // `Update="@(Helper)"` uses helper items only to select identities.
        // MSBuild does not transfer the helper item's package metadata.
        let metadata = match op {
            PackageRefOp::Include => source.metadata.with_overrides(&local_metadata),
            PackageRefOp::Update => PackageMetadata::default().with_overrides(&local_metadata),
        };
        if op == PackageRefOp::Include {
            note_inherited_helper_metadata_uncertainties(
                &source.metadata_uncertainties,
                &local_metadata,
                node,
                state,
                &mut reported_helper_metadata_uncertainties,
            );
        }
        // Capture Include and Update in document order, keeping the three-state
        // [`MetadataValue`] (`Inherit`/`Clear`/`Value`) intact:
        // [`finalize_package_references`] collapses Update onto prior Include
        // (where `Clear` correctly erases, unlike a lossy `Option`) and only
        // then detects the versionless symptom on the effective set.
        state
            .captured_package_references
            .push(CapturedPackageReference {
                op,
                id,
                metadata,
                span: span.clone(),
                origin: origin.clone(),
            });
    }
    if op == PackageRefOp::Include && state.captured_package_references.len() > package_count_before
    {
        state.mark_untracked_item_list("PackageReference");
    }
}

/// Whether a `<PackageReference>` `Update` writes any metadatum we *capture*
/// (`version`/`versionoverride`/`*assets`). An `Update` that names only
/// uncaptured metadata (`AllowExplicitVersion`, `NoWarn`, …) cannot perturb a
/// captured field however many `Include`s it matches — the same argument as
/// the item-definition-default carve-out
/// ([`item_definition_defines_captured_package_metadata`]) — and an `Update`
/// never adds or removes items, so identity is safe too. This is what makes
/// the SDK's `<PackageReference Update="@(PackageReference)"
/// AllowExplicitVersion="true"/>` (emitted when
/// `DisableImplicitFrameworkReferences` is set) inert despite its
/// self-referential, otherwise-unevaluable target. Metadata appears as
/// attributes ([`is_item_metadata_attribute`]) or child elements; a captured
/// metadatum whose own gate is *cleanly false* cannot apply, but an untrusted
/// gate still might (conservative: count it). An unknown item kind is treated
/// conservatively as writing captured metadata.
fn update_writes_captured_metadata(node: Node<'_, '_>, state: &State<'_>) -> bool {
    let Some(kind) = package_item_kind_for_element(node) else {
        return true;
    };
    let captured = captured_package_metadata_names(kind);
    let names_a_captured = |name: &str| captured.iter().any(|c| name.eq_ignore_ascii_case(c));
    node.attributes()
        .any(|attr| is_item_metadata_attribute(attr.name()) && names_a_captured(attr.name()))
        || node
            .children()
            .filter(Node::is_element)
            .filter(|metadata| reference_gate_may_run(*metadata, state))
            .any(|metadata| names_a_captured(metadata.tag_name().name()))
}

/// Whether an `Update`'s resolved identities provably touch no captured
/// `Include`, so the whole `Update` — metadata and all — is inert. True only
/// when every identity is unique *within this spec* (a same-spec duplicate
/// like `Update="A;A"` goes through MSBuild's position-independent dictionary
/// path and can modify a *later* `Include`, so it is never provably inert —
/// probed dotnet 10.0.301) and none matches an already-captured `Include`.
/// Since an `Update` applies only to prior `Include`s, the set captured at
/// walk time is exactly the set it could modify; an empty `sources` (target
/// expanded to nothing, or already flagged unevaluable) is vacuously inert. A
/// `false` return is always the conservative choice: the caller then reads the
/// metadata and any genuine uncertainty still surfaces.
///
/// Comparison is a raw `eq_ignore_ascii_case`, which the caller guarantees is
/// exact here by only invoking this once every identity in play matches the
/// package-id allow-list (ASCII `[A-Za-z0-9._-]`, no `.` boundary) — MSBuild
/// matches item identities as `GetFullPath`-normalized paths under full-Unicode
/// `OrdinalIgnoreCase`, so any identity that could normalize differently is
/// declined upstream rather than compared approximately.
fn update_matches_no_captured_include(
    sources: &[DependencyIdentitySource],
    state: &State<'_>,
) -> bool {
    let mut seen = HashSet::new();
    for source in sources {
        if !seen.insert(source.identity.to_ascii_lowercase()) {
            return false;
        }
    }
    !sources.iter().any(|source| {
        state.captured_package_references.iter().any(|captured| {
            captured.op == PackageRefOp::Include
                && captured.id.eq_ignore_ascii_case(&source.identity)
        })
    })
}

/// A `<PackageReference>` as captured during the item pass, before Include +
/// `Update` collapse. Retains the three-state [`PackageMetadata`] (rather than
/// the lossy `Option` of the public [`PackageReference`]) so
/// [`finalize_package_references`] can merge `Update`s with MSBuild's exact
/// clear/overwrite semantics. `op` and `origin` drive the merge and its
/// versionless-uncertainty reporting; both are internal — a public
/// `PackageReference` is always the post-merge effective `Include`.
pub(super) struct CapturedPackageReference {
    op: PackageRefOp,
    id: String,
    metadata: PackageMetadata,
    span: Range<usize>,
    origin: DiagnosticOrigin,
}

/// Collapse captured `Update`s onto the `Include`s they modify and publish the
/// effective [`State::package_references`]. MSBuild applies each `Update` to
/// every *prior* `Include` of the same (case-insensitive) id, overwriting each
/// specified metadatum (`with_overrides` honours `Clear` as an erase and
/// `Inherit` as leave-alone); a lone `Update` matching no prior `Include`
/// modifies nothing and is dropped. Only *after* the merge is the versionless
/// symptom detected on the effective set — a reference whose effective version
/// is unresolved (its version might come from CPM, an `ItemDefinitionGroup`
/// default, or an SDK) marks the set uncertain until the inline CPM pass can
/// prove it (a bare `VersionOverride` does not count: it is inert outside CPM
/// and discharged by the CPM pass, not here).
pub(super) fn finalize_package_references(state: &mut State<'_>) {
    let captured = std::mem::take(&mut state.captured_package_references);
    let mut effective: Vec<CapturedPackageReference> = Vec::new();
    for item in captured {
        match item.op {
            PackageRefOp::Include => effective.push(item),
            PackageRefOp::Update => {
                for include in effective
                    .iter_mut()
                    .filter(|include| include.id.eq_ignore_ascii_case(&item.id))
                {
                    include.metadata = include.metadata.with_overrides(&item.metadata);
                }
                // A lone Update matching no prior Include is dropped here. Any
                // uncertainty its own metadata/identity evaluation recorded
                // stays (we don't attribute uncertainties per item), so a
                // dropped Update with an unevaluable value conservatively keeps
                // the set uncertain — a decline, never a wrong resolution.
            }
        }
    }
    for reference in &effective {
        if !reference.metadata.version.has_value() {
            state.package_references_uncertain = true;
            state
                .package_reference_uncertainties
                .push(PackageReferenceUncertaintyCause {
                    kind: PackageReferenceUncertaintyCauseKind::VersionlessPackageReference {
                        id: reference.id.clone(),
                    },
                    span: reference.span.clone(),
                    origin: reference.origin.clone(),
                });
        }
    }
    state.package_references = effective
        .into_iter()
        .map(|reference| PackageReference {
            op: PackageRefOp::Include,
            id: reference.id,
            version: reference.metadata.version.into_option(),
            version_override: reference.metadata.version_override.into_option(),
            include_assets: reference.metadata.include_assets.into_option(),
            exclude_assets: reference.metadata.exclude_assets.into_option(),
            private_assets: reference.metadata.private_assets.into_option(),
            span: reference.span,
        })
        .collect();
}

/// Capture a `<PackageVersion Include="...">` item. This is a CPM input, not
/// a direct dependency; until effective CPM application lands it still marks
/// the package set uncertain, but callers can inspect the central versions.
fn walk_package_version(node: Node<'_, '_>, state: &mut State<'_>) {
    state.note_package_uncertain_if_condition_uses_sdk_taint(node);
    match evaluate_item_condition(node, state) {
        CondGate::Run => {}
        CondGate::Skip => return,
        CondGate::Unsupported => {
            emit_unsupported_condition(node, state);
            return;
        }
    }
    note_cpm_item_uncertain(
        PackageReferenceUncertaintyCauseKind::PackageVersion,
        node,
        state,
    );
    let Some(include) = node.attribute("Include") else {
        if node.attribute("Remove").is_some() || node.attribute("Update").is_some() {
            state.package_versions_untracked = true;
            state.mark_untracked_item_list("PackageVersion");
        }
        return;
    };
    state.note_package_uncertain_from_sdk_property_taint(include);
    let local_version = read_item_metadata(node, "Version", state);
    let excluded = resolve_exclude_set(node, state);
    let span = state.effective_span(node.range());
    let package_version_count_before = state.package_versions.len();
    let mut reported_helper_metadata_uncertainties = HashSet::new();
    for source in resolve_dependency_identity_sources(include, node, state) {
        if excluded.contains(&source.identity.to_ascii_lowercase()) {
            continue;
        }
        note_inherited_helper_version_uncertainties(
            &source.metadata_uncertainties,
            &local_version,
            node,
            state,
            &mut reported_helper_metadata_uncertainties,
        );
        let version = source
            .metadata
            .version
            .with_override(&local_version)
            .into_option();
        state.package_versions.push(PackageVersion {
            id: source.identity,
            version,
            span: span.clone(),
        });
    }
    if state.package_versions.len() > package_version_count_before {
        state.mark_untracked_item_list("PackageVersion");
    }
}

/// Capture a `<GlobalPackageReference Include="...">` item. Like
/// `<PackageReference Include="@(Helper)">`, helper item metadata flows into
/// package metadata and local metadata on the global ref overrides it.
fn walk_global_package_reference(node: Node<'_, '_>, state: &mut State<'_>) {
    state.note_package_uncertain_if_condition_uses_sdk_taint(node);
    match evaluate_item_condition(node, state) {
        CondGate::Run => {}
        CondGate::Skip => return,
        CondGate::Unsupported => {
            emit_unsupported_condition(node, state);
            return;
        }
    }
    note_cpm_item_uncertain(
        PackageReferenceUncertaintyCauseKind::GlobalPackageReference,
        node,
        state,
    );
    let Some(include) = node.attribute("Include") else {
        if node.attribute("Remove").is_some() || node.attribute("Update").is_some() {
            state.mark_untracked_item_list("GlobalPackageReference");
        }
        return;
    };
    state.note_package_uncertain_from_sdk_property_taint(include);
    let local_metadata = read_package_metadata(node, state);
    let excluded = resolve_exclude_set(node, state);
    let span = state.effective_span(node.range());
    let global_count_before = state.global_package_references.len();
    let mut reported_helper_metadata_uncertainties = HashSet::new();
    for source in resolve_dependency_identity_sources(include, node, state) {
        if excluded.contains(&source.identity.to_ascii_lowercase()) {
            continue;
        }
        note_inherited_helper_metadata_uncertainties(
            &source.metadata_uncertainties,
            &local_metadata,
            node,
            state,
            &mut reported_helper_metadata_uncertainties,
        );
        let metadata = source.metadata.with_overrides(&local_metadata);
        state
            .global_package_references
            .push(GlobalPackageReference {
                id: source.identity,
                version: metadata.version.into_option(),
                version_override: metadata.version_override.into_option(),
                include_assets: metadata.include_assets.into_option(),
                exclude_assets: metadata.exclude_assets.into_option(),
                private_assets: metadata.private_assets.into_option(),
                span: span.clone(),
            });
    }
    if state.global_package_references.len() > global_count_before {
        state.mark_untracked_item_list("GlobalPackageReference");
    }
}

/// A CPM item is a conservative uncertainty wherever it appears — SDK files
/// included, since SDK provenance no longer carries a blanket cause of its
/// own. The inline-CPM pass discharges the exact subset it proves applied.
fn note_cpm_item_uncertain(
    kind: PackageReferenceUncertaintyCauseKind,
    node: Node<'_, '_>,
    state: &mut State<'_>,
) {
    state.package_references_uncertain = true;
    state.record_package_reference_uncertainty(kind, node.range());
}

fn note_inherited_helper_version_uncertainties(
    uncertainties: &[HelperMetadataUncertainty],
    local_version: &MetadataValue,
    node: Node<'_, '_>,
    state: &mut State<'_>,
    reported: &mut HashSet<(HelperMetadataUncertaintyKind, String, String)>,
) {
    if local_version.is_override() {
        return;
    }
    for uncertainty in uncertainties {
        if !uncertainty.name.eq_ignore_ascii_case("Version") {
            continue;
        }
        let key = (
            uncertainty.kind.clone(),
            item_key(&uncertainty.name),
            uncertainty.value.clone(),
        );
        if !reported.insert(key) {
            continue;
        }
        match uncertainty.kind {
            HelperMetadataUncertaintyKind::UnevaluableValue => {
                state.note_package_uncertain(
                    PackageReferenceUncertaintyCauseKind::UnevaluableMetadata {
                        name: uncertainty.name.clone(),
                        value: uncertainty.value.clone(),
                    },
                    node.range(),
                );
            }
            HelperMetadataUncertaintyKind::ItemDefinitionDefault => {
                state.note_package_uncertain(
                    PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault,
                    node.range(),
                );
            }
        }
    }
}

fn note_inherited_helper_metadata_uncertainties(
    uncertainties: &[HelperMetadataUncertainty],
    local_metadata: &PackageMetadata,
    node: Node<'_, '_>,
    state: &mut State<'_>,
    reported: &mut HashSet<(HelperMetadataUncertaintyKind, String, String)>,
) {
    for uncertainty in uncertainties {
        if local_metadata.overrides_name(&uncertainty.name) {
            continue;
        }
        let key = (
            uncertainty.kind.clone(),
            item_key(&uncertainty.name),
            uncertainty.value.clone(),
        );
        if !reported.insert(key) {
            continue;
        }
        let cause = match &uncertainty.kind {
            HelperMetadataUncertaintyKind::UnevaluableValue => {
                PackageReferenceUncertaintyCauseKind::UnevaluableMetadata {
                    name: uncertainty.name.clone(),
                    value: uncertainty.value.clone(),
                }
            }
            HelperMetadataUncertaintyKind::ItemDefinitionDefault => {
                PackageReferenceUncertaintyCauseKind::ItemDefinitionDefault
            }
        };
        state.note_package_uncertain(cause, node.range());
    }
}

/// Capture a `<FrameworkReference Include="...">` (name only). Same
/// condition/uncertainty handling as [`walk_package_reference`].
fn walk_framework_reference(node: Node<'_, '_>, state: &mut State<'_>) {
    state.note_package_uncertain_if_condition_uses_sdk_taint(node);
    match evaluate_item_condition(node, state) {
        CondGate::Run => {}
        CondGate::Skip => return,
        CondGate::Unsupported => {
            emit_unsupported_condition(node, state);
            return;
        }
    }
    let Some(target) = node.attribute("Include") else {
        if let Some(operation) = node
            .attribute("Remove")
            .map(|_| "Remove")
            .or_else(|| node.attribute("Update").map(|_| "Update"))
        {
            state.note_package_uncertain(
                PackageReferenceUncertaintyCauseKind::UnsupportedItemOperation {
                    item: "FrameworkReference".to_string(),
                    operation: operation.to_string(),
                },
                node.range(),
            );
        }
        return;
    };
    state.note_package_uncertain_from_sdk_property_taint(target);
    let excluded = resolve_exclude_set(node, state);
    let span = state.effective_span(node.range());
    let framework_count_before = state.framework_references.len();
    for source in resolve_dependency_identity_sources(target, node, state) {
        let name = source.identity;
        if excluded.contains(&name.to_ascii_lowercase()) {
            continue;
        }
        state.framework_references.push(FrameworkReference {
            name: name.to_owned(),
            span: span.clone(),
        });
    }
    if state.framework_references.len() > framework_count_before {
        state.mark_untracked_item_list("FrameworkReference");
    }
}

/// The identity set to remove from a package/framework reference's
/// `Include`, from its `Exclude` attribute (MSBuild item semantics —
/// `Include="A;B" Exclude="B"` yields only `A`). Identities are matched
/// case-insensitively — MSBuild item identity comparison is `OrdinalIgnoreCase`,
/// so `Exclude="b"` removes `B` — hence the set holds ASCII-lowercased ids and
/// the include loops lowercase before testing membership. If the `Exclude`
/// value carries a glob, an `@(…)`/`%(…)` reference, or a substitution issue —
/// anything we can't reduce to a literal id set — the package set is marked
/// uncertain and an empty set returned, so nothing is silently mis-excluded
/// (and, crucially, nothing an `Exclude` would remove is left in as an
/// over-resolution).
fn resolve_exclude_set(node: Node<'_, '_>, state: &mut State<'_>) -> HashSet<String> {
    let Some(raw) = node.attribute("Exclude") else {
        return HashSet::new();
    };
    state.note_package_uncertain_from_sdk_property_taint(raw);
    let expansion = state.expand(raw, node.range());
    let parts: Vec<String> = spec_fragments(&expansion.value)
        .map(str::to_owned)
        .collect();
    if expansion.had_issue()
        || parts.iter().any(|s| {
            contains_glob(s) || contains_item_reference(s) || contains_metadata_reference(s)
        })
    {
        state.note_package_uncertain(
            PackageReferenceUncertaintyCauseKind::UnsupportedExclude {
                value: raw.to_string(),
            },
            node.range(),
        );
        return HashSet::new();
    }
    // The classifications above ran on escaped text (an escaped `%2a` is a
    // literal star, not a glob); the ids themselves are a point of use, and the
    // identities they are compared against were decoded when their `Include`
    // captured them. Comparing escaped against decoded would silently retain a
    // package an `Exclude` names — a phantom dependency, the over-resolution
    // this set exists to prevent.
    parts
        .iter()
        .map(|s| fragment_identity(s).to_ascii_lowercase())
        .collect()
}

/// Read one item metadata value — a `Name="..."` attribute or a
/// `<Name>...</Name>` child element — with `$(…)` substitution applied and
/// surrounding whitespace trimmed. Unlike `<Link>`, no path normalisation:
/// version and asset-list strings are opaque to this crate.
///
/// The private return type distinguishes "absent, inherit source-item
/// metadata" from "present but empty/unresolvable, clear source-item metadata".
/// Public `PackageReference` fields still collapse both to `None`.
///
/// Child elements are evaluated like MSBuild item metadata: each same-named
/// child's `Condition` is evaluated and the *last* one whose condition holds
/// wins (metadata is last-write). A false-conditioned child is ignored; an
/// unsupported/undefined-property condition flips the package-set uncertainty
/// via the diagnostics `evaluate_condition` / `emit_unsupported_condition`
/// push.
fn read_item_metadata(node: Node<'_, '_>, name: &str, state: &mut State<'_>) -> MetadataValue {
    fn expand_trimmed(
        raw: &str,
        name: &str,
        range: Range<usize>,
        state: &mut State<'_>,
    ) -> MetadataValue {
        state.note_package_uncertain_from_sdk_property_taint(raw);
        let expansion = state.expand(raw, range.clone());
        if expansion.had_issue() {
            return MetadataValue::Clear;
        }
        let (escaped, v) = scalar_use(&expansion.value);
        if escaped.is_empty() {
            return MetadataValue::Clear;
        }
        // An `@(…)`/`%(…)` reference surviving `$()` expansion means the value
        // depends on item/metadata evaluation we don't perform — MSBuild would
        // resolve it, so returning the raw expression would be *wrong*, not
        // merely partial. Drop the metadata and mark the set uncertain, exactly
        // as the `Include` id path does.
        if contains_item_reference(escaped) || contains_metadata_reference(escaped) {
            state.note_package_uncertain(
                PackageReferenceUncertaintyCauseKind::UnevaluableMetadata {
                    name: name.to_string(),
                    value: escaped.to_string(),
                },
                range,
            );
            return MetadataValue::Clear;
        }
        MetadataValue::Value(v)
    }
    // The attribute form is the *first* item-metadata write (no per-metadata
    // condition — the item's own `Condition` gates the whole element). Within
    // that attribute write, case-variant duplicate metadata names are still
    // last-write-wins, so expand only the last matching attribute. Child
    // elements are later writes that override it, so an attribute and a
    // same-named child together resolve to the child.
    let mut chosen = node
        .attributes()
        .rfind(|attr| attr.name().eq_ignore_ascii_case(name))
        .map_or(MetadataValue::Inherit, |attr| {
            expand_trimmed(attr.value(), name, node.range(), state)
        });
    for child in node
        .children()
        .filter(Node::is_element)
        .filter(|n| n.tag_name().name().eq_ignore_ascii_case(name))
    {
        state.note_package_uncertain_if_condition_uses_sdk_taint(child);
        match evaluate_item_condition(child, state) {
            CondGate::Run => {
                // A body we cannot model (CDATA / entity-encoded whitespace —
                // see `collect_element_text`): degrade the dependency set, and
                // clear the metadatum rather than let a prior write stand for a
                // value we don't know. Same shape as `expand_trimmed`'s
                // had-issue path.
                let Some(raw) = collect_element_text(child) else {
                    state.note_package_uncertain(
                        PackageReferenceUncertaintyCauseKind::Diagnostic(
                            DiagnosticKind::UnsupportedItemOperation {
                                operation: format!("<{name}> with an unmodellable element body"),
                            },
                        ),
                        child.range(),
                    );
                    chosen = MetadataValue::Clear;
                    continue;
                };
                // An empty (or "") last write clears the value, matching
                // last-write-wins semantics.
                chosen = if raw.is_empty() {
                    MetadataValue::Clear
                } else {
                    expand_trimmed(&raw, name, child.range(), state)
                };
            }
            CondGate::Skip => {}
            CondGate::Unsupported => emit_unsupported_condition(child, state),
        }
    }
    chosen
}

#[derive(Debug, Clone, Default)]
enum MetadataValue {
    #[default]
    Inherit,
    Clear,
    Value(String),
}

impl MetadataValue {
    fn from_captured(value: Option<String>) -> Self {
        match value {
            Some(value) if value.is_empty() => Self::Clear,
            Some(value) => Self::Value(value),
            None => Self::Inherit,
        }
    }

    fn with_override(&self, override_value: &Self) -> Self {
        match override_value {
            Self::Inherit => self.clone(),
            Self::Clear | Self::Value(_) => override_value.clone(),
        }
    }

    fn has_value(&self) -> bool {
        matches!(self, Self::Value(_))
    }

    fn is_override(&self) -> bool {
        !matches!(self, Self::Inherit)
    }

    fn into_option(self) -> Option<String> {
        match self {
            Self::Value(value) => Some(value),
            Self::Inherit | Self::Clear => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct PackageMetadata {
    version: MetadataValue,
    version_override: MetadataValue,
    include_assets: MetadataValue,
    exclude_assets: MetadataValue,
    private_assets: MetadataValue,
}

impl PackageMetadata {
    fn from_item(item: &EvaluatedItem) -> Self {
        Self {
            version: MetadataValue::from_captured(item.metadata("Version")),
            version_override: MetadataValue::from_captured(item.metadata("VersionOverride")),
            include_assets: MetadataValue::from_captured(item.metadata("IncludeAssets")),
            exclude_assets: MetadataValue::from_captured(item.metadata("ExcludeAssets")),
            private_assets: MetadataValue::from_captured(item.metadata("PrivateAssets")),
        }
    }

    fn with_overrides(&self, overrides: &PackageMetadata) -> Self {
        Self {
            version: self.version.with_override(&overrides.version),
            version_override: self
                .version_override
                .with_override(&overrides.version_override),
            include_assets: self.include_assets.with_override(&overrides.include_assets),
            exclude_assets: self.exclude_assets.with_override(&overrides.exclude_assets),
            private_assets: self.private_assets.with_override(&overrides.private_assets),
        }
    }

    fn overrides_name(&self, name: &str) -> bool {
        match name.to_ascii_lowercase().as_str() {
            "version" => self.version.is_override(),
            "versionoverride" => self.version_override.is_override(),
            "includeassets" => self.include_assets.is_override(),
            "excludeassets" => self.exclude_assets.is_override(),
            "privateassets" => self.private_assets.is_override(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
struct DependencyIdentitySource {
    identity: String,
    metadata: PackageMetadata,
    metadata_uncertainties: Vec<HelperMetadataUncertainty>,
}

fn read_package_metadata(node: Node<'_, '_>, state: &mut State<'_>) -> PackageMetadata {
    PackageMetadata {
        version: read_item_metadata(node, "Version", state),
        version_override: read_item_metadata(node, "VersionOverride", state),
        include_assets: read_item_metadata(node, "IncludeAssets", state),
        exclude_assets: read_item_metadata(node, "ExcludeAssets", state),
        private_assets: read_item_metadata(node, "PrivateAssets", state),
    }
}

fn resolve_dependency_identity_sources(
    raw: &str,
    node: Node<'_, '_>,
    state: &mut State<'_>,
) -> Vec<DependencyIdentitySource> {
    let expansion = state.expand(raw, node.range());
    if expansion.had_issue() {
        state.note_package_uncertain(
            PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                value: raw.to_string(),
            },
            node.range(),
        );
        return Vec::new();
    }

    let mut resolved = Vec::new();
    for entry in spec_fragments(&expansion.value) {
        if let Some(item_type) = exact_item_list_reference(entry) {
            let key = item_key(item_type);
            if state.tainted_item_lists.contains(&key) || state.untracked_item_lists.contains(&key)
            {
                state.note_package_uncertain(
                    PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                        value: entry.to_string(),
                    },
                    node.range(),
                );
            }
            let items = state.evaluated_items.get(&key).cloned().unwrap_or_default();
            for item in items {
                let metadata = PackageMetadata::from_item(&item);
                let mut metadata_uncertainties = item.metadata_uncertainties.clone();
                metadata_uncertainties.extend(helper_item_definition_default_uncertainties(
                    &key, &item, state,
                ));
                resolved.push(DependencyIdentitySource {
                    identity: item.identity,
                    metadata,
                    metadata_uncertainties,
                });
            }
            continue;
        }

        // An `@(...)`/`%(...)` reference surviving expansion means the identity
        // set depends on item/metadata evaluation we don't evaluate. Exact
        // item-list references were handled above; transforms, item functions,
        // and concatenated item refs remain unsupported.
        if contains_item_reference(entry) || contains_metadata_reference(entry) {
            state.note_package_uncertain(
                PackageReferenceUncertaintyCauseKind::UnevaluableIdentity {
                    value: entry.to_string(),
                },
                node.range(),
            );
            continue;
        }
        // MSBuild applies filesystem glob semantics to *any* item's `Include`,
        // so a wildcard id (`NoSuch*`) resolves to the matching files — for a
        // dependency identity, usually nothing. We don't glob the filesystem for
        // package/framework ids, so a literal capture would be wrong.
        if contains_glob(entry) {
            state.note_package_uncertain(
                PackageReferenceUncertaintyCauseKind::UnsupportedGlob {
                    pattern: entry.to_string(),
                },
                node.range(),
            );
            continue;
        }
        // The reference and glob classifications above ran on escaped text (an
        // escaped `%2a` is a literal star, not a wildcard); the identity itself
        // is a point of use, so it decodes here. `Include="Foo%2eBar"` is the
        // package `Foo.Bar`, which is also what an `Update`/`Remove` naming it
        // either way must compare against.
        resolved.push(DependencyIdentitySource {
            identity: fragment_identity(entry),
            metadata: PackageMetadata::default(),
            metadata_uncertainties: Vec::new(),
        });
    }
    resolved
}

fn helper_item_definition_default_uncertainties(
    item_type_key: &str,
    item: &EvaluatedItem,
    state: &State<'_>,
) -> Vec<HelperMetadataUncertainty> {
    let Some(defaults) = state.helper_item_definition_defaults.get(item_type_key) else {
        return Vec::new();
    };
    let mut result: Vec<_> = defaults
        .values()
        .filter(|uncertainty| !helper_item_has_package_metadata_name(item, &uncertainty.name))
        .cloned()
        .collect();
    result.sort_by_key(|uncertainty| item_key(&uncertainty.name));
    result
}

fn helper_item_has_package_metadata_name(item: &EvaluatedItem, name: &str) -> bool {
    let key = item_key(name);
    item.metadata.contains_key(&key)
        || item
            .metadata_uncertainties
            .iter()
            .any(|uncertainty| item_key(&uncertainty.name) == key)
}

#[derive(Debug, Clone, Default)]
struct GenericItemMetadata {
    values: HashMap<String, String>,
    uncertainties: Vec<HelperMetadataUncertainty>,
}

#[derive(Default)]
struct GenericItemMetadataBuilder {
    values: HashMap<String, String>,
    uncertainties: HashMap<String, HelperMetadataUncertainty>,
}

impl GenericItemMetadataBuilder {
    fn finish(self) -> GenericItemMetadata {
        let mut uncertainties: Vec<_> = self.uncertainties.into_values().collect();
        uncertainties.sort_by(|left, right| left.name.cmp(&right.name));
        GenericItemMetadata {
            values: self.values,
            uncertainties,
        }
    }
}

fn read_generic_item_metadata(node: Node<'_, '_>, state: &mut State<'_>) -> GenericItemMetadata {
    let mut metadata = GenericItemMetadataBuilder::default();
    for attr in node.attributes() {
        if is_item_metadata_attribute(attr.name()) && is_package_metadata_name(attr.name()) {
            set_generic_metadata_value(attr.name(), attr.value(), false, state, &mut metadata);
        }
    }
    for child in node.children().filter(Node::is_element) {
        let name = child.tag_name().name();
        if !is_package_metadata_name(name) {
            continue;
        }
        let (gate, condition_tainted) = evaluate_item_condition_silent_with_sdk_taint(child, state);
        match gate {
            CondGate::Run => {
                // An unmodellable body (CDATA / entity-encoded whitespace —
                // see `collect_element_text`) drops the metadatum and marks it
                // uncertain, exactly like an expansion we couldn't evaluate.
                match collect_element_text(child) {
                    Some(raw) => set_generic_metadata_value(
                        name,
                        &raw,
                        condition_tainted,
                        state,
                        &mut metadata,
                    ),
                    None => {
                        metadata.values.remove(&item_key(name));
                        mark_generic_metadata_uncertain(name, "", &mut metadata.uncertainties);
                    }
                }
            }
            CondGate::Skip | CondGate::Unsupported => {
                if condition_tainted {
                    let raw = collect_element_text(child).unwrap_or_default();
                    mark_generic_metadata_uncertain(name, &raw, &mut metadata.uncertainties);
                }
            }
        }
    }
    metadata.finish()
}

fn set_generic_metadata_value(
    name: &str,
    raw: &str,
    condition_tainted: bool,
    state: &mut State<'_>,
    metadata: &mut GenericItemMetadataBuilder,
) {
    debug_assert!(is_package_metadata_name(name));
    let key = item_key(name);
    let metadata_tainted = condition_tainted || state.raw_uses_sdk_package_taint(raw);
    if raw.is_empty() {
        metadata.values.insert(key.clone(), String::new());
        if metadata_tainted {
            mark_generic_metadata_uncertain(name, raw, &mut metadata.uncertainties);
        } else {
            metadata.uncertainties.remove(&key);
        }
        return;
    }
    let expansion = expand_silent(raw, state);
    if expansion.had_issue() {
        metadata.values.remove(&key);
        mark_generic_metadata_uncertain(name, raw, &mut metadata.uncertainties);
        return;
    }
    let (escaped, value) = scalar_use(&expansion.value);
    if escaped.is_empty() {
        metadata.values.insert(key.clone(), String::new());
        if metadata_tainted {
            mark_generic_metadata_uncertain(name, raw, &mut metadata.uncertainties);
        } else {
            metadata.uncertainties.remove(&key);
        }
        return;
    }
    if contains_item_reference(escaped) || contains_metadata_reference(escaped) {
        metadata.values.remove(&key);
        mark_generic_metadata_uncertain(name, escaped, &mut metadata.uncertainties);
        return;
    }
    metadata.values.insert(key.clone(), value);
    if metadata_tainted {
        mark_generic_metadata_uncertain(name, raw, &mut metadata.uncertainties);
    } else {
        metadata.uncertainties.remove(&key);
    }
}

fn mark_generic_metadata_uncertain(
    name: &str,
    value: &str,
    uncertainties: &mut HashMap<String, HelperMetadataUncertainty>,
) {
    uncertainties.insert(item_key(name), HelperMetadataUncertainty::new(name, value));
}

fn evaluate_item_condition_silent(node: Node<'_, '_>, state: &State<'_>) -> (CondGate, bool) {
    let Some(cond) = node.attribute("Condition") else {
        return (CondGate::Run, false);
    };
    let eval = if state.follow_imports {
        let exists = |path: &str| condition_exists(path, &state.entry_project_dir);
        condition::evaluate_with_exists(cond, &state.lookup, &exists)
    } else {
        condition::evaluate(cond, &state.lookup)
    };
    let tainted = eval
        .undefined_properties
        .iter()
        .any(|name| !state.undefined_read_is_exact(name))
        || eval.outcome == condition::Outcome::Unsupported
        || state.unpinned_root_for_raw(cond).is_some();
    let gate = match eval.outcome {
        condition::Outcome::True => CondGate::Run,
        condition::Outcome::False => CondGate::Skip,
        condition::Outcome::Unsupported => CondGate::Unsupported,
    };
    (gate, tainted)
}

fn evaluate_item_condition_silent_with_sdk_taint(
    node: Node<'_, '_>,
    state: &State<'_>,
) -> (CondGate, bool) {
    let (gate, condition_tainted) = evaluate_item_condition_silent(node, state);
    let sdk_tainted = node
        .attribute("Condition")
        .is_some_and(|condition| state.raw_uses_sdk_package_taint(condition));
    (gate, condition_tainted || sdk_tainted)
}

fn expand_silent(raw: &str, state: &State<'_>) -> Expansion {
    let (value, issues) = if state.follow_imports {
        properties::substitute_with_fs(raw, &state.lookup)
    } else {
        properties::substitute(raw, &state.lookup)
    };
    let mut had_undefined = false;
    let mut had_unsupported = false;
    let mut unpinned_root = None;
    for issue in issues {
        match issue {
            Issue::Undefined { name } => {
                // Exact undefined reads substitute to exactly the ""
                // MSBuild uses — not a divergence (C.2b), so no flag.
                if state.undefined_read_is_exact(&name) {
                    continue;
                }
                had_undefined = true;
                if unpinned_root.is_none() {
                    unpinned_root = Some(UnpinnedRoot::Undefined(name));
                }
            }
            Issue::Unsupported { .. } => had_unsupported = true,
        }
    }
    // Silent expansion still reports (via flags, not diagnostics) a read of
    // an unpinned property: the produced value inherits the same divergence
    // risk as a direct undefined reference, and callers' existing
    // had-issue handling is exactly the treatment that risk needs.
    if let Some(root) = state.unpinned_root_for_raw(raw) {
        had_undefined = true;
        if unpinned_root.is_none() {
            unpinned_root = Some(root);
        }
    }
    Expansion {
        value,
        had_undefined,
        had_unsupported,
        unpinned_root,
    }
}

fn exact_item_list_reference(value: &str) -> Option<&str> {
    let inner = value.strip_prefix("@(")?.strip_suffix(')')?.trim();
    if is_item_type_name(inner) {
        Some(inner)
    } else {
        None
    }
}

fn is_item_type_name(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c == '-' || c.is_ascii_alphanumeric())
}

fn is_item_metadata_attribute(name: &str) -> bool {
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "condition"
            | "exclude"
            | "include"
            | "keepduplicates"
            | "keepmetadata"
            | "matchonmetadata"
            | "matchonmetadataoptions"
            | "remove"
            | "removemetadata"
            | "update"
    )
}

pub(super) fn item_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// Handle one item element when a [`GlobResolver`] is present.
///
/// Splits the (already `$(...)`-expanded) `Include` into fragments,
/// diagnosing-and-stripping `@()`/`%()` references (the resolver only
/// deals in path fragments). A pure-literal include with no `Exclude`
/// keeps the literal fast path; otherwise the surviving fragments and
/// the split excludes are handed to the resolver and its output is
/// spliced verbatim, each path as an item of `kind`. An unresolved
/// `@()`/`%()` in the exclude list means we cannot know what to remove,
/// so the whole element is skipped rather than risk over-inclusion.
/// Flag [`ParsedProject::project_references_uncertain`] when the expanded
/// VALUE of a `<ProjectReference>` list attribute (`Include` or `Exclude`)
/// leans on an untrusted read — an unpinned property (written under a gate
/// we couldn't evaluate) or an SDK-package-tainted one. The expansion is
/// clean, so without consulting the pin state the captured list would look
/// trustworthy — but the real build's value may name a different target
/// (Include) or strip one we kept (Exclude), either way a phantom edge for
/// a reference-semantics consumer. Flag rather than drop: the
/// declared-structure walk keeps the element.
fn taint_reference_list_on_untrusted_value(
    kind: ItemKind,
    raw: &str,
    expansion: &Expansion,
    state: &mut State<'_>,
) {
    if kind == ItemKind::ProjectReference
        && (expansion.unpinned_root.is_some() || state.raw_uses_sdk_package_taint(raw))
    {
        state.project_references_uncertain = true;
    }
}

fn route_item_through_resolver(
    node: Node<'_, '_>,
    kind: ItemKind,
    placement: ItemPlacement,
    include_value: &Escaped,
    metadata: &ItemMetadata,
    resolver: &GlobResolver<'_>,
    state: &mut State<'_>,
) {
    let mut include_specs: Vec<&str> = Vec::new();
    for entry in spec_fragments(include_value) {
        if contains_item_reference(entry) {
            state.push(
                DiagnosticKind::UnresolvedItemReference {
                    reference: entry.to_string(),
                },
                node.range(),
            );
            continue;
        }
        if contains_metadata_reference(entry) {
            state.push(
                DiagnosticKind::UnresolvedMetadataReference {
                    reference: entry.to_string(),
                },
                node.range(),
            );
            continue;
        }
        include_specs.push(entry);
    }
    let any_glob = include_specs.iter().any(|s| contains_glob(s));
    let exclude_attr = node.attribute("Exclude");
    // A pure-literal include with no Exclude can't gain anything from the
    // resolver — keep the literal fast path (same items, no IO).
    if !any_glob && exclude_attr.is_none() {
        for entry in include_specs {
            push_include_entry(node, kind, placement, entry, metadata, state);
        }
        return;
    }
    let mut excludes: Vec<String> = Vec::new();
    if let Some(raw) = exclude_attr {
        let expansion = state.expand(raw, node.range());
        if expansion.had_issue() {
            return;
        }
        // The Exclude VALUE gets the same trust check as the Include's: an
        // untrusted `$(Skip)` that expanded (cleanly) to empty here may
        // exclude the target in the real build, leaving the captured list
        // claiming an edge the real build strips (a phantom edge for a
        // reference-semantics consumer). The best-effort exclusion below
        // still applies whatever we did expand.
        taint_reference_list_on_untrusted_value(kind, raw, &expansion, state);
        for entry in spec_fragments(&expansion.value) {
            if contains_item_reference(entry) {
                state.push(
                    DiagnosticKind::UnresolvedItemReference {
                        reference: entry.to_string(),
                    },
                    node.range(),
                );
                return;
            }
            if contains_metadata_reference(entry) {
                state.push(
                    DiagnosticKind::UnresolvedMetadataReference {
                        reference: entry.to_string(),
                    },
                    node.range(),
                );
                return;
            }
            let Some(exclude) = fragment_for_resolver(entry) else {
                unsupported_across_resolver_seam(node, entry, state);
                return;
            };
            excludes.push(exclude);
        }
    }
    // Across the seam the resolver re-splits on `;` and parses `*`/`?`, so each
    // fragment leaves the domain here — declining the ones whose decoded form
    // would smuggle a metacharacter past the classification above.
    let mut resolver_specs: Vec<String> = Vec::with_capacity(include_specs.len());
    for entry in &include_specs {
        let Some(spec) = fragment_for_resolver(entry) else {
            unsupported_across_resolver_seam(node, entry, state);
            return;
        };
        resolver_specs.push(spec);
    }
    let include_joined = resolver_specs.join(";");
    // Scope the request so its immutable borrow of `state` ends before we
    // mutably push the results.
    let matched = {
        let request = GlobRequest {
            base_dir: &state.entry_project_dir,
            include: &include_joined,
            excludes: &excludes,
        };
        resolver(&request)
    };
    let span = state.effective_span(node.range());
    for path in matched {
        push_resolved_item(
            state,
            placement,
            ResolvedItem {
                kind,
                include: path,
                link: metadata.link.clone(),
                reference_output_assembly: metadata.reference_output_assembly.clone(),
                exclude_assets: metadata.exclude_assets.clone(),
                include_assets: metadata.include_assets.clone(),
                private_assets: metadata.private_assets.clone(),
                unmodelled_reference_metadata: metadata.unmodelled_reference_metadata,
                span: span.clone(),
            },
        );
    }
}

/// Decline an item spec whose decoded form cannot cross the glob-resolver seam
/// (see [`fragment_for_resolver`]).
///
/// This is the same diagnostic — and so the same fail-safe consumer behaviour —
/// that the substitution-level escape withdrawal raised before the escaped
/// domain existed, so nothing downstream sees a new class of failure. Stage E4
/// of `docs/msbuild-escaped-value-plan.md` removes it, by giving the resolver a
/// fragment list it never re-scans.
fn unsupported_across_resolver_seam(node: Node<'_, '_>, entry: &str, state: &mut State<'_>) {
    state.push(
        DiagnosticKind::UnsupportedPropertyExpression {
            expression: entry.to_string(),
        },
        node.range(),
    );
}

/// `entry` is an **escaped** fragment: the glob classification below has to run
/// before decoding (an escaped `%2a` is a literal star in a filename, not a
/// wildcard — decoding first turns data into syntax), so the decode happens here,
/// once, after the scan.
fn push_include_entry(
    node: Node<'_, '_>,
    kind: ItemKind,
    placement: ItemPlacement,
    entry: &str,
    metadata: &ItemMetadata,
    state: &mut State<'_>,
) {
    if contains_glob(entry) {
        state.push(
            DiagnosticKind::UnsupportedGlob {
                pattern: entry.to_string(),
            },
            node.range(),
        );
        return;
    }
    if contains_item_reference(entry) {
        state.push(
            DiagnosticKind::UnresolvedItemReference {
                reference: entry.to_string(),
            },
            node.range(),
        );
        return;
    }
    if contains_metadata_reference(entry) {
        state.push(
            DiagnosticKind::UnresolvedMetadataReference {
                reference: entry.to_string(),
            },
            node.range(),
        );
        return;
    }
    // MSBuild Include paths are platform-independent; backslashes are
    // legal separators even on POSIX. Normalise so PathBuf::join produces
    // a well-formed path on either OS. Resolve relative includes
    // against the *entry* project's directory, not the importing
    // file's: an unqualified `<Compile Include="Generated.fs" />`
    // appearing in an imported `.props`/`.targets` file is resolved
    // by MSBuild relative to `$(MSBuildProjectDirectory)`.
    // The scans above ran on escaped text; the identity is a point of use.
    let normalised = fragment_identity(entry).replace('\\', "/");
    let path = state.entry_project_dir.join(normalised);
    let span = state.effective_span(node.range());
    push_resolved_item(
        state,
        placement,
        ResolvedItem {
            kind,
            include: path,
            link: metadata.link.clone(),
            reference_output_assembly: metadata.reference_output_assembly.clone(),
            exclude_assets: metadata.exclude_assets.clone(),
            include_assets: metadata.include_assets.clone(),
            private_assets: metadata.private_assets.clone(),
            unmodelled_reference_metadata: metadata.unmodelled_reference_metadata,
            span,
        },
    );
}

fn bucket_for_placement<'s, 'r>(
    state: &'s mut State<'r>,
    placement: ItemPlacement,
) -> &'s mut Vec<OrderedResolvedItem> {
    match placement {
        ItemPlacement::Compile(effect) => bucket_for_compile_effect(state, effect),
        ItemPlacement::ProjectReference => &mut state.project_references,
    }
}

fn bucket_for_compile_effect<'s, 'r>(
    state: &'s mut State<'r>,
    effect: CompileOrderEffect,
) -> &'s mut Vec<OrderedResolvedItem> {
    match effect {
        CompileOrderEffect::Slot(CompileOrderSlot::CompileFirst) => &mut state.compile_first,
        CompileOrderEffect::Slot(CompileOrderSlot::ExplicitCompileBefore) => {
            &mut state.explicit_compile_before
        }
        CompileOrderEffect::Slot(CompileOrderSlot::CompileBefore) => &mut state.compile_before,
        CompileOrderEffect::Slot(CompileOrderSlot::Compile) => &mut state.compile_main,
        CompileOrderEffect::Slot(CompileOrderSlot::CompileAfter) => &mut state.compile_after,
        CompileOrderEffect::Slot(CompileOrderSlot::ExplicitCompileAfter) => {
            &mut state.explicit_compile_after
        }
        CompileOrderEffect::Slot(CompileOrderSlot::CompileLast) => &mut state.compile_last,
        CompileOrderEffect::Excluded => &mut state.compile_excluded,
    }
}

fn push_resolved_item(state: &mut State<'_>, placement: ItemPlacement, item: ResolvedItem) {
    let order = state.next_item_order;
    state.next_item_order += 1;
    bucket_for_placement(state, placement).push(OrderedResolvedItem { order, item });
}

fn evaluate_item_condition(node: Node<'_, '_>, state: &mut State<'_>) -> CondGate {
    // MSBuild evaluates item conditions (`ItemGroup`, item elements, and item
    // metadata) relative to the entry project directory, even when the item was
    // declared in an imported props/targets file. Import and `PropertyGroup`
    // conditions keep using the file currently being walked.
    let base_dir = state.entry_project_dir.clone();
    evaluate_condition(node, &base_dir, state)
}

/// Whether an `<ItemGroup>` has at least one Compile-flavoured child
/// (`<Compile>` / `<CompileBefore>` / `<CompileAfter>`) that can change the
/// source set — the trigger for treating the group's own condition as
/// Compile-affecting.
fn item_group_has_compile_child(node: Node<'_, '_>) -> bool {
    node.children()
        .filter(Node::is_element)
        .any(compile_child_can_change_source_set)
}

pub(super) fn item_definition_group_sets_compile_order(node: Node<'_, '_>) -> bool {
    node.children()
        .filter(Node::is_element)
        .filter(|&c| modelled_item_kind_for_element(c) == Some(ItemKind::Compile))
        .any(compile_item_sets_compile_order)
}

fn compile_child_can_change_source_set(node: Node<'_, '_>) -> bool {
    let Some(kind) = modelled_item_kind_for_element(node) else {
        return false;
    };
    is_compile_item_kind(kind)
        && (!is_metadata_only_item_update(node)
            || (kind == ItemKind::Compile && compile_item_sets_compile_order(node)))
}

fn compile_item_sets_compile_order(node: Node<'_, '_>) -> bool {
    node.attributes()
        .any(|attr| attr.name().eq_ignore_ascii_case("CompileOrder"))
        || node.children().filter(Node::is_element).any(|metadata| {
            metadata
                .tag_name()
                .name()
                .eq_ignore_ascii_case("CompileOrder")
        })
}

fn is_compile_item_kind(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Compile | ItemKind::CompileBefore | ItemKind::CompileAfter
    )
}

fn modelled_item_type(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Compile => "Compile",
        ItemKind::CompileBefore => "CompileBefore",
        ItemKind::CompileAfter => "CompileAfter",
        ItemKind::ProjectReference => "ProjectReference",
    }
}

fn modelled_item_kind_for_element(node: Node<'_, '_>) -> Option<ItemKind> {
    let name = node.tag_name().name();
    if name.eq_ignore_ascii_case("Compile") {
        Some(ItemKind::Compile)
    } else if name.eq_ignore_ascii_case("CompileBefore") {
        Some(ItemKind::CompileBefore)
    } else if name.eq_ignore_ascii_case("CompileAfter") {
        Some(ItemKind::CompileAfter)
    } else if name.eq_ignore_ascii_case("ProjectReference") {
        Some(ItemKind::ProjectReference)
    } else {
        None
    }
}

fn package_item_kind_for_element(node: Node<'_, '_>) -> Option<PackageItemKind> {
    let name = node.tag_name().name();
    if name.eq_ignore_ascii_case("PackageReference") {
        Some(PackageItemKind::PackageReference)
    } else if name.eq_ignore_ascii_case("FrameworkReference") {
        Some(PackageItemKind::FrameworkReference)
    } else if name.eq_ignore_ascii_case("PackageVersion") {
        Some(PackageItemKind::PackageVersion)
    } else if name.eq_ignore_ascii_case("GlobalPackageReference") {
        Some(PackageItemKind::GlobalPackageReference)
    } else {
        None
    }
}

fn is_metadata_only_item_update(node: Node<'_, '_>) -> bool {
    node.attribute("Update").is_some()
        && node.attribute("Include").is_none()
        && node.attribute("Remove").is_none()
}

fn modelled_item_list_operation_may_change_list(node: Node<'_, '_>, kind: ItemKind) -> bool {
    if is_compile_item_kind(kind) && is_metadata_only_item_update(node) {
        return false;
    }
    node.attribute("Include").is_some()
        || node.attribute("Update").is_some()
        || node.attribute("Remove").is_some()
}

fn generic_helper_item_type(node: Node<'_, '_>) -> Option<String> {
    let name = node.tag_name().name();
    if modelled_item_kind_for_element(node).is_some()
        || package_item_kind_for_element(node).is_some()
    {
        None
    } else {
        Some(name.to_string())
    }
}

#[derive(Default)]
struct ItemListsGatedByGroupCondition {
    helper_item_types: Vec<String>,
    modelled_item_types: Vec<&'static str>,
}

impl ItemListsGatedByGroupCondition {
    fn is_empty(&self) -> bool {
        self.helper_item_types.is_empty() && self.modelled_item_types.is_empty()
    }

    fn apply(self, state: &mut State<'_>) {
        for item_type in self.helper_item_types {
            state.taint_item_list(&item_type);
        }
        for item_type in self.modelled_item_types {
            state.mark_untracked_item_list(item_type);
        }
    }
}

fn item_lists_gated_by_group_condition(
    node: Node<'_, '_>,
    state: &State<'_>,
) -> ItemListsGatedByGroupCondition {
    let mut result = ItemListsGatedByGroupCondition::default();
    for child in node.children().filter(Node::is_element) {
        let item_has_operation = child.attribute("Include").is_some()
            || child.attribute("Update").is_some()
            || child.attribute("Remove").is_some()
            || child.attribute("Exclude").is_some();
        if !item_has_operation || !item_child_condition_may_run(child, state) {
            continue;
        }
        if let Some(kind) = modelled_item_kind_for_element(child) {
            if modelled_item_list_operation_may_change_list(child, kind) {
                result.modelled_item_types.push(modelled_item_type(kind));
            }
            continue;
        }
        if let Some(item_type) = generic_helper_item_type(child) {
            result.helper_item_types.push(item_type);
        }
    }
    result
}

fn item_child_condition_may_run(node: Node<'_, '_>, state: &State<'_>) -> bool {
    let (gate, condition_tainted) = evaluate_item_condition_silent_with_sdk_taint(node, state);
    if condition_tainted {
        return true;
    }
    matches!(gate, CondGate::Run)
}

fn record_helper_item_definition_defaults(node: Node<'_, '_>, state: &mut State<'_>) {
    // Conditions here read the FINAL property table (the group replays in
    // the item pass), so "may run" is exact for clean gates: a cleanly
    // false condition means the default cannot apply in any build, and an
    // unpinnable one (undefined/unpinned/SDK-tainted reads, unsupported
    // grammar) conservatively counts as may-run — the default is recorded
    // and flags at consumption.
    if !item_child_condition_may_run(node, state) {
        return;
    }
    for child in node.children().filter(Node::is_element) {
        if !item_child_condition_may_run(child, state) {
            continue;
        }
        let Some(item_type) = generic_helper_item_type(child) else {
            continue;
        };
        for metadata in item_definition_child_package_metadata_names(child, state) {
            state.record_helper_item_definition_default(&item_type, &metadata.name);
        }
    }
}

struct ItemDefinitionPackageMetadataName {
    name: String,
}

fn item_definition_child_package_metadata_names(
    node: Node<'_, '_>,
    state: &State<'_>,
) -> Vec<ItemDefinitionPackageMetadataName> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for attr in node.attributes() {
        if is_package_metadata_name(attr.name()) && seen.insert(item_key(attr.name())) {
            names.push(ItemDefinitionPackageMetadataName {
                name: attr.name().to_string(),
            });
        }
    }
    for child in node.children().filter(Node::is_element) {
        let name = child.tag_name().name();
        if !is_package_metadata_name(name) || !item_child_condition_may_run(child, state) {
            continue;
        }
        if !seen.insert(item_key(name)) {
            continue;
        }
        names.push(ItemDefinitionPackageMetadataName {
            name: name.to_string(),
        });
    }
    names
}

const PACKAGE_METADATA_NAMES: [&str; 5] = [
    "Version",
    "VersionOverride",
    "IncludeAssets",
    "ExcludeAssets",
    "PrivateAssets",
];

fn is_package_metadata_name(name: &str) -> bool {
    PACKAGE_METADATA_NAMES
        .iter()
        .any(|metadata_name| name.eq_ignore_ascii_case(metadata_name))
}

/// Whether an `<ItemGroup>` directly contains a `<PackageReference>` or
/// `<FrameworkReference>` — the trigger for treating the group's own
/// condition as package-set-affecting.
pub(super) fn item_group_has_package_child(node: Node<'_, '_>) -> bool {
    node.children()
        .filter(Node::is_element)
        // Direct dependency items *and* the CPM item types: an `<ItemGroup>`
        // whose only package children are `<PackageVersion>` /
        // `<GlobalPackageReference>` is still package-affecting, so an
        // unevaluable `Condition` on it must set `package_context` (and thus
        // flip uncertainty) rather than silently skip central versions /
        // global refs.
        .any(|c| package_item_kind_for_element(c).is_some())
}

/// Resolve one string-valued item metadatum (`<Link>`,
/// `<ReferenceOutputAssembly>`, `<ExcludeAssets>`, …) from either an
/// attribute on the item or a child element with text content. In either
/// form, $(...) substitution applies —
/// `Link="$(Configuration)/$(MSBuildProjectName).fs"` is legal MSBuild.
///
/// MSBuild item-metadata semantics: names compare **case-insensitively**,
/// the attribute form is the *first* assignment, and child elements are
/// later writes processed in document order, each gated on its own
/// `Condition` (evaluated like every item-level condition) — so the last
/// assignment whose condition holds wins and a false-conditioned child is
/// simply not an assignment.
///
/// An **empty** child is still a later write (dotnet 10 probe:
/// `Bar="attr"` followed by `<Bar/>` leaves `%(Bar)` empty) — it clears an
/// earlier attribute value. Whitespace-only inner text reads as empty too
/// (MSBuild's XML loader treats it as insignificant), as does comment-only
/// content (`collect_element_text` yields `""` for it). And since
/// MSBuild's own `GetMetadataValue` reports `""` for set-empty and unset
/// alike, an effective value that is empty — cleared, expanded-to-empty,
/// or an `Attr=""` spelling — resolves to `Known(None)`, never
/// `Known(Some(""))`.
///
/// A write we cannot evaluate — an unsupported `Condition` (which the real
/// build may satisfy), a `$(...)` expansion issue, or an `@(...)`/`%(...)`
/// reference in the value — makes the resolution
/// [`ItemMetadataValue::Unknown`] (with the corresponding diagnostic
/// emitted): its effect on the effective value is unknowable, and reading
/// it as "no write" would let a reference-semantics consumer keep an edge
/// the real build drops. A *later* write that evaluates cleanly overwrites
/// whatever the unevaluable one did, so it restores `Known`.
fn resolve_string_metadata(
    node: Node<'_, '_>,
    state: &mut State<'_>,
    name: &str,
) -> ItemMetadataValue {
    let mut resolved = ItemMetadataValue::Known(None);
    // Case-variant duplicate attributes are valid XML and MSBuild's
    // case-insensitive metadata names make them last-write-wins (probed,
    // dotnet 10: `<X Foo="one" foo="two"/>` evaluates to two) — same rule
    // the package capture's attribute read commits to. `rfind` = the last
    // in document order.
    if let Some(attr) = node
        .attributes()
        .rfind(|a| a.name().eq_ignore_ascii_case(name))
    {
        let expansion = state.expand(attr.value(), node.range());
        resolved = if expansion.had_issue()
            || expansion.unpinned_root.is_some()
            || state.raw_uses_sdk_package_taint(attr.value())
        {
            // A value leaning on an unpinned/SDK-tainted read expands
            // cleanly but may differ in the real build — Unknown, same as
            // an outright expansion failure.
            ItemMetadataValue::Unknown
        } else {
            match finalize_metadata_value(expansion.value, node.range(), state) {
                Some(value) => ItemMetadataValue::Known(Some(value)),
                None => ItemMetadataValue::Unknown,
            }
        };
    }
    let children: Vec<Node<'_, '_>> = node
        .children()
        .filter(Node::is_element)
        .filter(|n| n.tag_name().name().eq_ignore_ascii_case(name))
        .collect();
    for child in children {
        // A gate decided only by an unpinned or SDK-tainted read may go the
        // other way in a real build — in EITHER direction: an untrusted
        // false may apply the write we skipped, an untrusted true may skip
        // the write we applied. Both leave the effective value unknowable.
        // (A clean decision over undefined-but-never-written properties
        // stays exact under the environment model — the pinned
        // `'$(NoSuchProp)'` cases.)
        let untrusted_gate = state.condition_reads_untrusted_value(child);
        match evaluate_item_condition(child, state) {
            CondGate::Run => {
                if untrusted_gate {
                    resolved = ItemMetadataValue::Unknown;
                    continue;
                }
            }
            CondGate::Skip => {
                if untrusted_gate {
                    resolved = ItemMetadataValue::Unknown;
                }
                continue;
            }
            CondGate::Unsupported => {
                emit_unsupported_condition(child, state);
                resolved = ItemMetadataValue::Unknown;
                continue;
            }
        }
        // An unmodellable body (CDATA / entity-encoded whitespace — see
        // `collect_element_text`) is a write whose effect we can't determine:
        // `Unknown`, like an unevaluable expansion below.
        let Some(raw) = collect_element_text(child) else {
            resolved = ItemMetadataValue::Unknown;
            continue;
        };
        if raw.trim().is_empty() {
            resolved = ItemMetadataValue::Known(None);
            continue;
        }
        let expansion = state.expand(&raw, child.range());
        if expansion.had_issue()
            || expansion.unpinned_root.is_some()
            || state.raw_uses_sdk_package_taint(&raw)
        {
            resolved = ItemMetadataValue::Unknown;
            continue;
        }
        resolved = match finalize_metadata_value(expansion.value, child.range(), state) {
            Some(value) => ItemMetadataValue::Known(Some(value)),
            None => ItemMetadataValue::Unknown,
        };
    }
    match resolved {
        ItemMetadataValue::Known(Some(v)) if v.is_empty() => ItemMetadataValue::Known(None),
        other => other,
    }
}

/// After `$(...)` expansion, a metadata value may still contain `@(Items)`
/// or `%(Identity)` — neither of which we evaluate. Treat them the
/// same way Include attributes and `<PropertyGroup>` children do:
/// emit a diagnostic and drop the value, rather than silently
/// exposing unevaluated MSBuild syntax in
/// [`ResolvedItem`](super::ResolvedItem) fields.
fn finalize_metadata_value(
    value: Escaped,
    span: Range<usize>,
    state: &mut State<'_>,
) -> Option<String> {
    // **No trim here.** This finalises a metadata value whose padding is
    // significant: MSBuild preserves authored whitespace in item metadata and
    // compares it untrimmed, so `ReferenceOutputAssembly=" true "` is not the
    // boolean `true`, and a padded `Link` is a different logical path. (The
    // callers that *should* trim — a `CompileOrder` slot name, a package version
    // — do so through `scalar_use`, which trims in the domain.) The scans still
    // run on escaped text; only the value leaves it.
    let escaped = value.as_escaped();
    if contains_item_reference(escaped) {
        state.push(
            DiagnosticKind::UnresolvedItemReference {
                reference: escaped.to_string(),
            },
            span,
        );
        return None;
    }
    if contains_metadata_reference(escaped) {
        state.push(
            DiagnosticKind::UnresolvedMetadataReference {
                reference: escaped.to_string(),
            },
            span,
        );
        return None;
    }
    Some(value.unescape())
}

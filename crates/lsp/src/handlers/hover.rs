//! `textDocument/hover` — a one-line description of the symbol under the
//! cursor.
//!
//! Surfaces what the resolver, inference, and the referenced-assembly model know
//! today: the name and `DefKind` for a project-local binder — plus its inferred
//! type when we have one (`` `x : int` — value ``, from [`infer_file`]) — or, for
//! a symbol from a referenced DLL, read out of the project's [`AssemblyEnv`], its
//! declared shape. An **entity** shows its F# declaration (`type List<'T>`,
//! `[<Struct>] type Vector2`, `module Operators`) via
//! [`borzoi_assembly::format_entity_header`], with the namespace as context
//! (and the kind the `type` keyword collapses); a **member** shows its F#
//! signature (`static member WriteLine: value: string -> unit`,
//! `val mutable x: int`) via [`borzoi_assembly::format_member`], with the
//! declaring type as context (prefixed `extension member,` / `required member,`
//! for the C#-isms the F# signature has no keyword for). Both then show the
//! declaring assembly's identity and any `[<Obsolete>]` / `[<Experimental>]`
//! marker — all declared metadata, needing no inference — and, when the
//! assembly's PDB records a source position, a `Defined in <file>, line N` line
//! (`append_defined_in`, via the DLL's embedded or sidecar PDB) so a symbol
//! whose source the LSP can't open still reports where it lives. A project-local
//! *value* binder additionally shows its inferred type where inference has one
//! (3.2b-1: literal-bound values and the chains they feed). Remaining richer
//! hover (annotated/function-binder types, XML doc summary) is tracked in
//! `docs/hover-signature-plan.md`.

use borzoi_assembly::{
    AssemblyIdentity, Augmentation, Entity, EntityKind, Experimental, Member, Obsolete,
    format_entity_header, format_fsharp_name, format_member, join_quoted,
};
use borzoi_cst::syntax::{AstNode, ImplFile, SyntaxKind, SyntaxNode};
use borzoi_sema::{
    AssemblyEnv, DefKind, EntityHandle, InferredFile, MemberIndex, ProjectItems, Resolution,
    ResolvedFile, ResolvedProject, SemanticClass, Ty, infer_file, resolve_file,
};
use lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};
use rowan::TextRange;

use crate::cst_panic_safe::parse_with_symbols;
use crate::goto_source::DefinitionDocument;
use crate::handlers::definition::{entity_definition_document, member_definition_document};
use crate::handlers::definition_availability::{classify, explanation_range};
use crate::handlers::{
    range_to_lsp, smallest_inferred_type_with_range, smallest_member_resolution_with_range,
    smallest_resolution_with_range,
};
use crate::paths::{lexically_normalize, paths_equal};
use crate::position::position_to_offset;
use crate::semantic::SemanticState;
use crate::server::State;

/// Run the hover handler. Returns `None` only when there's no buffer or the
/// cursor is on nothing name-like. A symbol we *can* describe yields its
/// signature/type; a name whose definition we *can't* resolve
/// ([`Resolution::Deferred`] / [`Resolution::Unresolved`], or an identifier the
/// resolver never recorded) now yields an *explanation* of why go-to-definition
/// finds nothing there (via [`definition_availability`](crate::handlers::definition_availability)),
/// rather than the previous silent `None`.
pub fn handle(state: &mut State, params: HoverParams) -> Option<Hover> {
    let pos = params.text_document_position_params.position;
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .clone();
    let text = state.docs.get(&uri).cloned()?;
    let byte = position_to_offset(&text, pos);

    if let Some(hover) = project_hover(state, &uri, &text, byte) {
        return Some(hover);
    }
    if let Some(hover) = single_file_hover(state, &uri, &text, byte) {
        return Some(hover);
    }
    // Neither path could *describe* the cursor. If it sits on a name we declined
    // to resolve, explain why instead of staying silent.
    definition_unavailable_hover(state, &uri, &text, byte)
}

/// Whether a position was classified against a real evaluated project. Lets
/// [`definition_unavailable_hover`] fall back to single-file classification
/// *only* when the file truly isn't in a project — never re-classifying (with a
/// misleading "degraded" note) a project position that simply had nothing to say.
enum ProjectClassify {
    /// The file isn't in an evaluated project; try single-file classification.
    NotInProject,
    /// The file is in an evaluated project; this is the authoritative answer
    /// (`Some` explanation, or `None` = nothing name-like here).
    InProject(Option<Hover>),
}

/// Explain why go-to-definition finds nothing at the cursor, preferring project
/// context (authoritative, no degraded note) and falling back to single-file
/// classification for an orphan / unevaluated-project buffer.
fn definition_unavailable_hover(
    state: &mut State,
    uri: &lsp_types::Url,
    text: &str,
    byte: usize,
) -> Option<Hover> {
    match project_unavailable(state, uri, text, byte) {
        ProjectClassify::InProject(hover) => hover,
        ProjectClassify::NotInProject => single_file_unavailable(state, uri, text, byte),
    }
}

/// Classify the cursor against the project's resolution (non-degraded). Returns
/// [`ProjectClassify::NotInProject`] whenever the file can't be located in an
/// evaluated project, so the caller falls back to single-file mode.
fn project_unavailable(
    state: &mut State,
    uri: &lsp_types::Url,
    text: &str,
    byte: usize,
) -> ProjectClassify {
    let Ok(path) = uri.to_file_path() else {
        return ProjectClassify::NotInProject;
    };
    let Some(project) = state.workspace.owning_project(&path) else {
        return ProjectClassify::NotInProject;
    };
    let State {
        semantic,
        workspace,
        docs,
        ..
    } = state;
    let Some(parses) = semantic.parses_for_project(&project, workspace, docs) else {
        return ProjectClassify::NotInProject;
    };
    let parses = parses.clone();
    let Some(idx) = parses
        .paths
        .iter()
        .position(|p| paths_equal(&lexically_normalize(p), &lexically_normalize(&path)))
    else {
        return ProjectClassify::NotInProject;
    };
    // Only this file's own resolution is inspected (`resolved.file(idx)`), so fold
    // just the prefix up to it — the same slice as `project_hover`. The env comes
    // from the same paired call for the same reason it does there: inference must
    // run against the exact env the fold resolved against.
    let Some((resolved, env)) =
        semantic.resolved_prefix_and_env_for(&project, idx, workspace, docs)
    else {
        return ProjectClassify::NotInProject;
    };
    let file = resolved.file(idx);
    // A `.fsi` Compile slot is inert in Stage 1 (no resolutions, no impl
    // tree); classify as out-of-project so the caller serves the degraded
    // single-file answer for a signature buffer.
    let Some(impl_file) = parses.files[idx].file.as_impl() else {
        return ProjectClassify::NotInProject;
    };
    // The explanation must be about the verdict the LSP would *act* on, and
    // go-to-definition acts on inference's member resolution where the resolver
    // deferred. This runs only for a cursor both `project_hover` and
    // `single_file_hover` already declined, so it is the cold path — but those
    // two inferred as well, and nothing carries a solved file between them.
    // Threading one `InferredFile` through the three would need `project_hover`
    // to classify from its own pass, which changes when `single_file_hover` gets
    // a look at a project cursor; tracked rather than done here.
    let inferred = {
        let _span = tracing::info_span!("infer_file").entered();
        infer_file(impl_file, file, &env)
    };
    let root = impl_file.syntax();
    ProjectClassify::InProject(unavailable_hover(
        file,
        Some(&inferred),
        root,
        text,
        byte,
        false,
    ))
}

/// Single-file fallback classification for an orphan / unevaluated-project
/// buffer — mirrors [`single_file_hover`]'s parse + resolve, then classifies
/// with the degraded flag set (cross-file / assembly symbols are out of reach).
fn single_file_unavailable(
    state: &mut State,
    uri: &lsp_types::Url,
    text: &str,
    byte: usize,
) -> Option<Hover> {
    let symbols = state.symbols_for_uri(uri);
    let lang = state.lang_version_for_uri(uri);
    let parse = parse_with_symbols(text, &symbols, lang)?;
    let file = ImplFile::cast(parse.root)?;
    let resolved = resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default());
    // No project, so no assembly env worth inferring against: the member
    // side-table would be empty anyway.
    unavailable_hover(&resolved, None, file.syntax(), text, byte, true)
}

/// Turn a [`classify`] verdict into an explanatory hover, anchored to the
/// symbol / identifier it concerns. `None` when there's nothing to explain.
fn unavailable_hover(
    file: &ResolvedFile,
    inferred: Option<&InferredFile>,
    root: &SyntaxNode,
    text: &str,
    byte: usize,
    degraded_single_file: bool,
) -> Option<Hover> {
    let explanation = classify(file, inferred, root, byte, degraded_single_file)?;
    let range = explanation_range(file, inferred, root, byte)?;
    Some(make_hover(explanation.explain(), text, range))
}

/// Project-level hover: the cursor's file is in a fully-evaluated project,
/// so we can resolve cross-file `Item`s and assembly entities too.
fn project_hover(
    state: &mut State,
    uri: &lsp_types::Url,
    text: &str,
    byte: usize,
) -> Option<Hover> {
    let path = uri.to_file_path().ok()?;
    let project = state.workspace.owning_project(&path)?;
    let State {
        semantic,
        workspace,
        docs,
        ..
    } = state;
    // This file's Compile-order index, computed *before* resolving so we fold
    // only the prefix up to it: F# is order-sensitive, so the hovered name (and
    // any cross-file `Item` / earlier binder it resolves to) lives at an index
    // `<= target_file_idx` — the suffix fold can't change this file's answer.
    let parses = semantic
        .parses_for_project(&project, workspace, docs)?
        .clone();
    let target_file_idx = parses
        .paths
        .iter()
        .position(|p| paths_equal(&lexically_normalize(p), &lexically_normalize(&path)))?;
    // The project's `AssemblyEnv` is an input to inference (Stage 3.3a: a member
    // access `recv.Name` resolves the member against it) and to rendering an
    // `Entity` / `Member` resolution. It must be the *exact* env the fold
    // resolved against — a re-fetched env can shift the handles the resolution
    // recorded (see `resolved_prefix_and_env_for`) — so take both from one
    // paired call.
    let (resolved, env) =
        semantic.resolved_prefix_and_env_for(&project, target_file_idx, workspace, docs)?;
    let file = resolved.file(target_file_idx);
    // A `.fsi` Compile slot is inert in Stage 1 (no resolutions, no impl tree
    // to infer over) — decline so the caller's single-file fallback answers
    // for a signature buffer.
    let impl_file = parses.files[target_file_idx].file.as_impl()?;

    // Inference is per-file and pure (no IO — it reads only the parsed file, its
    // resolution, and the assembly env), so run it once up front: it enriches a
    // resolved-name hover with the binder's type *and* serves the literal /
    // member-access fallback below.
    let inferred = {
        let _span = tracing::info_span!("infer_file").entered();
        infer_file(impl_file, file, &env)
    };

    // A name resolution under the cursor takes precedence (it's the more specific
    // answer; we enrich it with the binder's inferred type when we have one).
    if let Some((range, res)) = smallest_resolution_with_range(file, byte) {
        // Entity/member resolutions render against the same `AssemblyEnv`; a plain
        // local/value renders its inferred type. The `resolved`/`parses` handles
        // above are an `Arc` + owned clone, so `semantic`/`workspace` are free to
        // re-borrow here — the same discipline `project_definition` uses.
        let body = if matches!(res, Resolution::Entity(_) | Resolution::Member { .. }) {
            hover_body(semantic, &resolved, file, res, &env, None)
        } else {
            let ty = file
                .resolved_def_id(res)
                .and_then(|def| inferred.def_type(def));
            hover_body(semantic, &resolved, file, res, &AssemblyEnv::default(), ty)
        };
        if let Some(body) = body {
            return Some(make_hover(body, text, range));
        }
    }

    // Member-resolution enrichment (Stage 3.3b): where the resolver left a
    // member-name (`recv.Name`) as `Deferred(QualifiedAccess)` — so the block
    // above produced no body — inference may have resolved the member against the
    // receiver's type. Render it via the same `Resolution::Member` path an
    // assembly-path member (`System.Console.WriteLine`) takes, so hover on
    // `Length` in `s.Length` shows the member, not just the whole access's type.
    if let Some((range, res)) = smallest_member_resolution_with_range(&inferred, byte)
        && let Some(body) = hover_body(semantic, &resolved, file, res, &env, None)
    {
        return Some(make_hover(body, text, range));
    }

    // Fallback: an inferred expression type (a literal or a tuple), where no
    // name resolution surfaced a hover.
    let (range, ty) = smallest_inferred_type_with_range(&inferred, byte)?;
    let root = impl_file.syntax();
    Some(make_hover(
        literal_hover_body(text, root, range, ty),
        text,
        range,
    ))
}

/// Single-file fallback for orphan / partial-project buffers. Without
/// project context, only locals/parameters and same-file top-level
/// bindings yield a useful hover.
fn single_file_hover(
    state: &mut State,
    uri: &lsp_types::Url,
    text: &str,
    byte: usize,
) -> Option<Hover> {
    let symbols = state.symbols_for_uri(uri);
    let lang = state.lang_version_for_uri(uri);
    let parse = parse_with_symbols(text, &symbols, lang)?;
    let file = ImplFile::cast(parse.root)?;
    // Single-file (orphan) hover: no project, so no referenced assemblies —
    // member-access typing has no env to resolve into and simply defers.
    let env = AssemblyEnv::default();
    let resolved = resolve_file(&file, &ProjectItems::default(), &env);
    let inferred = infer_file(&file, &resolved, &env);

    // Name resolution first (locals / same-file top-level bindings), enriched
    // with the binder's inferred type when we have one.
    if let Some((range, res)) = smallest_resolution_with_range(&resolved, byte)
        && let Some(def) = resolved.resolved_def(res)
    {
        let ty = resolved
            .resolved_def_id(res)
            .and_then(|d| inferred.def_type(d));
        return Some(make_hover(format_def(&def.name, def.kind, ty), text, range));
    }

    // Fallback: inferred expression type (a literal or a tuple) — works on an
    // orphan buffer from the single-file resolution we just computed.
    let (range, ty) = smallest_inferred_type_with_range(&inferred, byte)?;
    Some(make_hover(
        literal_hover_body(text, file.syntax(), range, ty),
        text,
        range,
    ))
}

/// Format a hover body for the project-resolved `Resolution`. `None` when
/// the resolution doesn't carry surfaceable detail today (deferred /
/// unresolved). `ty` is the binder's inferred type (for the `Local` / `Item`
/// value arms; `None` when inference deferred it or the symbol isn't a binder).
/// `env` is only read for the referenced-assembly arms
/// ([`Resolution::Entity`] / [`Resolution::Member`]); the others ignore it.
fn hover_body(
    semantic: &mut SemanticState,
    resolved: &ResolvedProject,
    file: &ResolvedFile,
    res: Resolution,
    env: &AssemblyEnv,
    ty: Option<&Ty>,
) -> Option<String> {
    match res {
        Resolution::Local(id) => {
            let def = file.def(id);
            Some(format_def(&def.name, def.kind, ty))
        }
        Resolution::Item(_) => {
            let (_, def) = resolved.item_def(res)?;
            Some(format_def(&def.name, def.kind, ty))
        }
        Resolution::Entity(handle) => {
            let mut body = entity_hover_label(env, handle);
            append_defined_in(&mut body, entity_definition_document(semantic, env, handle));
            Some(body)
        }
        Resolution::Member { parent, idx } => {
            let mut body = member_hover_label(env, parent, idx);
            append_defined_in(
                &mut body,
                member_definition_document(semantic, env, parent, idx),
            );
            Some(body)
        }
        Resolution::Deferred(_) | Resolution::Unresolved => None,
    }
}

/// Append a `Defined in …` paragraph to a referenced-assembly hover body when
/// the PDB told us where the symbol is declared. A no-op when there's no
/// location (no PDB, no sequence point) — the rest of the hover (signature,
/// declaring type, provenance) still stands.
fn append_defined_in(body: &mut String, document: Option<DefinitionDocument>) {
    if let Some(document) = document {
        body.push_str("\n\n");
        body.push_str(&defined_in_line(&document));
    }
}

/// ``Defined in `<file>`, line <n>`` — the source origin of a referenced-assembly
/// symbol, for when we know *where* it is but can't open it (no embedded source
/// / SourceLink). Only the document's file name is shown: the recorded path is
/// the build machine's absolute path ([`file_name_of`]), useless on this host.
fn defined_in_line(document: &DefinitionDocument) -> String {
    format!(
        "Defined in `{}`, line {}",
        file_name_of(&document.document),
        document.line
    )
}

/// The final path component of `path`, splitting on both `/` and `\` (a PDB
/// document path is usually a *Windows* absolute path, which `std::path` on a
/// Unix host would not split). Falls back to the whole string if it has no
/// separator.
fn file_name_of(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// A string that has been through [`code_span`], and so is safe to place in a
/// Markdown paragraph even if it contains F# backticks.
///
/// Only [`code_span`] constructs one. Every line of a hover body that carries a
/// rendered F# name — head or context — is typed as this, so a new line added
/// later cannot skip the fencing and silently have its quotes eaten: that is a
/// compile error rather than a rendering nobody looks at twice.
#[derive(Debug, PartialEq, Eq)]
struct Fenced(String);

impl PartialEq<&str> for Fenced {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl std::fmt::Display for Fenced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Wrap `content` in a Markdown code span whose fence is long enough to hold it.
///
/// A code span may not contain a backtick run as long as its own fence, and F#'s
/// quoted identifiers are made of backticks (`` ``Circle Case`` ``): the usual
/// single-backtick wrapper would end the span in the middle of the name. The
/// fence is therefore one longer than the longest run inside, and a content that
/// starts or ends with a backtick is padded with a space on *both* sides —
/// CommonMark strips one space from each end only when both are present.
fn code_span(content: &str) -> Fenced {
    let longest_run = content.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest_run + 1);
    let pad = if content.starts_with('`') || content.ends_with('`') {
        " "
    } else {
        ""
    };
    Fenced(format!("{fence}{pad}{content}{pad}{fence}"))
}

/// Hover body for a referenced-assembly entity: the F# **declaration** as the
/// head (`type List<'T>`, `[<Struct>] type Vector2`, `module Operators`, …),
/// then the namespace as context and the assembly + any obsolete/experimental
/// marker — e.g.
///
/// ```text
/// `type List<'T>`
///
/// class, in System.Collections.Generic
///
/// from System.Collections v9.0.0.0
/// ```
///
/// The declaration keyword (`type` / `module` / `exception`) and attribute
/// prefixes carry most of the kind; the part `type` collapses (class / record /
/// union / enum / delegate / abbreviation) is preserved on the context line by
/// `entity_context`. Public so the integration tests can drive it against a
/// real [`AssemblyEnv`] without reconstructing a whole project.
pub fn entity_hover_label(env: &AssemblyEnv, handle: EntityHandle) -> String {
    if env.entity_class(handle) == Some(SemanticClass::UnionCase) {
        return union_case_hover_label(env, handle);
    }
    let entity = env.entity(handle);
    let head = code_span(&format_entity_header(entity));
    assemble_body(
        head,
        entity_context(entity),
        &entity.assembly,
        entity.obsolete.as_ref(),
        entity.experimental.as_ref(),
    )
}

/// Hover body for a **field-carrying union case** of a referenced assembly —
/// e.g.
///
/// ```text
/// `union case Choice1Of2`
///
/// in Microsoft.FSharp.Core.Choice<'T1, 'T2>
///
/// from FSharp.Core v9.0.0.0
/// ```
///
/// fsc compiles such a case to a nested class, so the projected entity carries
/// [`EntityKind::Class`] and nothing of its own says "case"; the fact lives on
/// the *parent* union and reaches us through
/// [`AssemblyEnv::entity_class`](borzoi_sema::AssemblyEnv::entity_class), which
/// is deliberately the same predicate the semantic-token classifier asks. One
/// predicate means the two surfaces cannot drift into calling one handle a class
/// and a union case (the defect this arm fixes), and its declines — a
/// non-authoritative F# signature, a union whose cases could not be recovered —
/// are inherited rather than restated here.
///
/// Deliberately *not* [`format_entity_header`]: a nested type re-declares its
/// enclosing type's generic parameters as its own, so the carrier's are the
/// union's and the header would render `union case Choice1Of2<'T1, 'T2>` — type
/// parameters on a case name, which F# never writes. They belong to the union,
/// which is where the context line renders them. The name itself still goes
/// through [`format_fsharp_name`], as every other declaration head does: a case
/// may be declared with a quoted identifier, and its metadata name then carries
/// the spaces (`` union case ``Circle Case`` ``).
///
/// An `[<Obsolete>]` / `[<Experimental>]` marker on the *case* never surfaces:
/// fsc puts a case's attributes on its `New<Case>` maker method, which is not in
/// [`Entity::members`] at all (a union's constructors live in `union_cases`), so
/// neither the carrier nor its union carries the marker — checked against a
/// purpose-built `[<Obsolete>]`-cased union, whose carrier projects
/// `obsolete: None`. The entity's own markers are still passed through: they are
/// empty today, and reading them keeps this arm honest if that changes.
///
/// [`Entity::members`]: borzoi_assembly::Entity::members
///
/// [`EntityKind::Class`]: borzoi_assembly::EntityKind::Class
fn union_case_hover_label(env: &AssemblyEnv, handle: EntityHandle) -> String {
    let entity = env.entity(handle);
    let name = format_fsharp_name(entity.source_name.as_deref().unwrap_or(&entity.name));
    // The case's payload (`of string * SynStringKind * range`) is not rendered:
    // it survives only as the carrier's generated `Item`/`ItemN` (or named-field)
    // properties, and reconstructing an `of` clause from those is a guess. An
    // absent payload under-states the case; a wrong one misreports it.
    let head = code_span(&format!("union case {name}"));
    let context = declaring_union(env, handle)
        .map(|union| format!("in {}", code_span(&entity_fqn(env, union))));
    assemble_body(
        head,
        context,
        &entity.assembly,
        entity.obsolete.as_ref(),
        entity.experimental.as_ref(),
    )
}

/// The union a case carrier is nested in — the last entity before `handle` on
/// its enclosing chain. `None` only if `handle` is unreachable from any
/// top-level type (which [`AssemblyEnv::enclosing_chain`] reports as a one-entry
/// chain), leaving the case to render without a context line rather than
/// inventing a declaring type.
///
/// [`AssemblyEnv::enclosing_chain`]: borzoi_sema::AssemblyEnv::enclosing_chain
fn declaring_union(env: &AssemblyEnv, handle: EntityHandle) -> Option<EntityHandle> {
    let chain = env.enclosing_chain(handle);
    let (last, rest) = chain.split_last()?;
    debug_assert_eq!(
        *last, handle,
        "the chain ends at the entity it was asked for"
    );
    rest.last().copied()
}

/// The entity context line: `<kind>, in <namespace>` — the kind (only when the
/// declaration keyword doesn't already carry it) and the declaring namespace.
/// `None` for a global-namespace entity whose keyword is the kind (e.g. a
/// top-level `module`).
fn entity_context(entity: &Entity) -> Option<String> {
    let namespace = join_quoted(entity.namespace.iter().map(String::as_str));
    match (entity_qualifier(entity), namespace.is_empty()) {
        (Some(kind), false) => Some(format!("{kind}, in {}", code_span(&namespace))),
        (Some(kind), true) => Some(kind.to_string()),
        (None, false) => Some(format!("in {}", code_span(&namespace))),
        (None, true) => None,
    }
}

/// The kind word for the context line, for the kinds the `type` keyword
/// collapses. `None` when the declaration head already conveys the kind — a
/// `module` / `exception` keyword, or the `[<Struct>]` / `[<Measure>]` attribute.
fn entity_qualifier(entity: &Entity) -> Option<&'static str> {
    match entity.kind {
        EntityKind::Module | EntityKind::Exception | EntityKind::Struct | EntityKind::Measure => {
            None
        }
        other => Some(entity_kind_word(other)),
    }
}

/// Hover body for a referenced-assembly member: the F# **signature** as the
/// head (`static member WriteLine: value: string -> unit`,
/// `member Count: int with get`, `val mutable x: int`, …), then the declaring
/// type as context and the assembly + any obsolete/experimental marker — e.g.
///
/// ```text
/// `member Add: item: 'T -> unit`
///
/// in System.Collections.Generic.List<'T>
///
/// from System.Collections v9.0.0.0
/// ```
///
/// The signature keyword (`member` / `static member` / `val` / `new`, plus
/// `with get, set`) carries the kind, so there is no separate `— method` label;
/// the member name is its F# *source* name (`printfn`, not the compiled
/// `PrintFormatLine`). The context line is prefixed `extension member,` /
/// `required member,` (`member_qualifier`) for facts the F# signature has no
/// keyword for. Provenance comes from the *parent* entity — members carry no
/// assembly identity of their own.
pub fn member_hover_label(env: &AssemblyEnv, parent: EntityHandle, idx: MemberIndex) -> String {
    let entity = env.entity(parent);
    let member = env.member_at(parent, idx);
    let head = code_span(&format_member(member, entity));
    let context = match member_qualifier(member) {
        Some(qualifier) => format!("{qualifier}, in {}", code_span(&entity_fqn(env, parent))),
        None => format!("in {}", code_span(&entity_fqn(env, parent))),
    };
    assemble_body(
        head,
        Some(context),
        &entity.assembly,
        member_obsolete(member),
        member_experimental(member),
    )
}

/// Join the head line with an optional context line (`in <declaring type>` for
/// members), the provenance line, and any obsolete/experimental banners into a
/// single markdown body. Paragraphs are `\n\n`-separated so every LSP client
/// renders them on their own line (a single `\n` is folded in markdown). The
/// order — head, context, provenance, then warnings — keeps the at-a-glance
/// identity first.
fn assemble_body(
    head: Fenced,
    context: Option<String>,
    assembly: &AssemblyIdentity,
    obsolete: Option<&Obsolete>,
    experimental: Option<&Experimental>,
) -> String {
    let mut lines = vec![head.0];
    if let Some(context) = context {
        lines.push(context);
    }
    lines.push(assembly_provenance(assembly));
    if let Some(o) = obsolete {
        lines.push(obsolete_banner(o));
    }
    if let Some(e) = experimental {
        lines.push(experimental_banner(e));
    }
    lines.join("\n\n")
}

/// `from <name> v<major>.<minor>.<build>.<revision>[, PublicKeyToken=<hex>]` —
/// the declaring assembly's logical identity (always present in the metadata,
/// independent of how the env was built). The strong-name token is included when
/// present so two references sharing a simple name + version but signed
/// differently don't render identically.
fn assembly_provenance(assembly: &AssemblyIdentity) -> String {
    let v = assembly.version;
    let mut s = format!(
        "from {} v{}.{}.{}.{}",
        assembly.name, v.major, v.minor, v.build, v.revision
    );
    if let Some(token) = assembly.public_key_token {
        let hex: String = token.iter().map(|byte| format!("{byte:02x}")).collect();
        s.push_str(", PublicKeyToken=");
        s.push_str(&hex);
    }
    s
}

/// `⚠ Obsolete[ (error)][: <message>]` — the `[<Obsolete>]` marker. `(error)`
/// distinguishes a hard `error: true` deprecation (using it fails to compile)
/// from a plain warning.
fn obsolete_banner(obsolete: &Obsolete) -> String {
    let severity = if obsolete.is_error { " (error)" } else { "" };
    match &obsolete.message {
        Some(message) => format!("⚠ Obsolete{severity}: {message}"),
        None => format!("⚠ Obsolete{severity}"),
    }
}

/// `⚠ Experimental[ (<diagnostic-id>)][: <message>]` — the `[<Experimental>]`
/// marker (.NET 8+). The diagnostic id is the attribute's required argument; the
/// message is optional.
fn experimental_banner(experimental: &Experimental) -> String {
    let id = match &experimental.diagnostic_id {
        Some(id) => format!(" ({id})"),
        None => String::new(),
    };
    match &experimental.message {
        Some(message) => format!("⚠ Experimental{id}: {message}"),
        None => format!("⚠ Experimental{id}"),
    }
}

/// A short qualifier for a member fact the F# *signature* has no keyword for,
/// shown on the context line (`<qualifier>, in <type>`) rather than cluttering
/// the signature head: an extension method, or a C# 11 `required` field or
/// property. `None` leaves the bare `in <type>` line. (These were on #589's
/// `— kind` label, which the signature head replaced.)
fn member_qualifier(member: &Member) -> Option<&'static str> {
    match member {
        // Either extension channel: the CLR `[Extension]` / F#-native *instance*
        // surface flag, or the F#-native augmentation flag (which alone carries a
        // `type T with static member …`).
        Member::Method(m) if m.is_extension_method || m.augmentation == Augmentation::Certain => {
            Some("extension member")
        }
        Member::Field(f) if f.is_required => Some("required member"),
        Member::Property(p) if p.is_required => Some("required member"),
        _ => None,
    }
}

/// The typed `[<Obsolete>]` marker a member carries, if any. Only methods model
/// it as a typed field today; fields/properties/events never report obsolete.
fn member_obsolete(member: &Member) -> Option<&Obsolete> {
    match member {
        Member::Method(m) => m.obsolete.as_ref(),
        Member::Field(_) | Member::Property(_) | Member::Event(_) => None,
    }
}

/// The typed `[<Experimental>]` marker a member carries, if any. As with
/// [`member_obsolete`], only methods model it as a typed field today.
fn member_experimental(member: &Member) -> Option<&Experimental> {
    match member {
        Member::Method(m) => m.experimental.as_ref(),
        Member::Field(_) | Member::Property(_) | Member::Event(_) => None,
    }
}

/// The fully-qualified, F#-rendered name of an entity: its full dotted name (F#
/// source names throughout, so a suffixed module reads `List`, not the compiled
/// `ListModule`), with `<'T, 'U>` appended when generic.
///
/// The dotted part is built from the enclosing chain — a *nested* ECMA TypeDef
/// declares no namespace of its own, so composing from `Entity::namespace` alone
/// drops every enclosing entity and leaves a bare `Choice1Of2` naming nothing a
/// reader could look up. Every segment goes through [`format_fsharp_name`]
/// individually, which is why this does not simply call
/// [`AssemblyEnv::entity_full_name`](borzoi_sema::AssemblyEnv::entity_full_name):
/// that returns one pre-joined string of *raw metadata* names (it is the currency
/// of the FCS differentials, which compare against unquoted full names), and a
/// segment needing backticks cannot be recovered from the join. The awkward
/// segment is usually not the last one — fsc's generated types nest under names
/// like `System-Collections-Generic-IDictionary<'Key, 'T>-get_Keys@60`.
///
/// The type parameters are the entity's *own*, which for a nested type are the
/// ones it re-declares from its enclosing type, so they land on the last segment
/// (`Choice.Choice1Of2<'T1, 'T2>`) rather than on the entity that introduced
/// them. This is a context line rather than source, so that reads acceptably;
/// distributing parameters across the chain is a further refinement.
fn entity_fqn(env: &AssemblyEnv, handle: EntityHandle) -> String {
    let entity = env.entity(handle);
    let typars: Vec<&str> = entity
        .generic_parameters
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    append_typars(entity_dotted_name(env, handle), &typars)
}

/// The dotted name of an entity with every segment rendered as an F# identifier:
/// the outermost enclosing entity's namespace, then one segment per entry of the
/// enclosing chain (which ends at `handle` itself).
///
/// A handle unreachable from any top-level type yields a one-entry chain, so this
/// degrades to the entity's own namespace and name — the same fallback
/// `AssemblyEnv::entity_full_name` takes.
fn entity_dotted_name(env: &AssemblyEnv, handle: EntityHandle) -> String {
    let chain = env.enclosing_chain(handle);
    let namespace = chain
        .first()
        .map(|&outermost| env.entity(outermost).namespace.as_slice())
        .unwrap_or_default();
    join_quoted(
        namespace
            .iter()
            .map(String::as_str)
            .chain(chain.iter().map(|&h| entity_source_name(env.entity(h)))),
    )
}

/// The name an entity renders under: its F# source name where metadata records
/// one (`List`, not the compiled `ListModule`).
fn entity_source_name(entity: &Entity) -> &str {
    entity.source_name.as_deref().unwrap_or(&entity.name)
}

/// Append (bare) type-parameter names to a dotted name in the F# leading-
/// apostrophe convention: `System.Collections.Generic.List` + `["T"]` →
/// `System.Collections.Generic.List<'T>`. A non-generic entity gets the name
/// back unchanged.
fn append_typars(name: String, typars: &[&str]) -> String {
    let mut s = name;
    if !typars.is_empty() {
        s.push('<');
        for (i, typar) in typars.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push('\'');
            s.push_str(typar);
        }
        s.push('>');
    }
    s
}

/// The bare kind noun for an [`EntityKind`], before modifiers.
fn entity_kind_word(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Class => "class",
        EntityKind::Struct => "struct",
        EntityKind::Interface => "interface",
        EntityKind::Enum => "enum",
        EntityKind::Delegate => "delegate",
        EntityKind::Module => "module",
        EntityKind::Union => "union",
        EntityKind::Record => "record",
        EntityKind::Abbreviation => "type abbreviation",
        EntityKind::Exception => "exception",
        EntityKind::Measure => "unit of measure",
    }
}

/// Format a resolved binder's hover: `` `name` — kind ``, or, when inference
/// assigned the binder a type, `` `name : ty` — kind `` (the F#-aliased type,
/// via [`Ty::render_fsharp`]). The type is the binder's own — valid at every
/// occurrence — so it shows the same whether the cursor is on the definition or
/// any use, and is unaffected by a subsumption coercion at a particular use
/// site. `value` binders are typed from 3.2b-1; monomorphic `function` binders
/// from 3.2c-2b (`` `f : bool -> int` — function ``). A polymorphic function, a
/// parameter (whose type is surfaced only inside its function's signature, not
/// standalone), or any binder inference never grounds, passes `None` (the bare
/// `` `name` — kind `` form).
fn format_def(name: &str, kind: DefKind, ty: Option<&Ty>) -> String {
    let kind_label = match kind {
        DefKind::Value { is_function: true } => "function",
        DefKind::Value { is_function: false } => "value",
        DefKind::Parameter => "parameter",
        DefKind::PatternLocal => "pattern local",
        DefKind::Type => "type",
        DefKind::UnionCase => "union case",
        DefKind::ExceptionCase => "exception",
        DefKind::ActivePattern => "active pattern",
        DefKind::ActivePatternCase => "active pattern case",
        DefKind::EnumCase => "enum case",
        DefKind::Member => "static member",
        DefKind::TypeParam => "type parameter",
    };
    match ty {
        Some(ty) => format!(
            "{} — {kind_label}",
            code_span(&format!("{name} : {}", ty.render_fsharp()))
        ),
        None => format!("{} — {kind_label}", code_span(name)),
    }
}

/// Hover body for an inferred expression type (the fallback when no name
/// resolves): the expression's own source text in a code span, then its F# type
/// (the same alias the assembly-member hovers use, via [`Ty::render_fsharp`]),
/// mirroring [`format_def`]'s `` `name` — kind `` shape. A *literal* adds the
/// `literal` descriptor (`` `42` — int literal ``); a compound expression (a
/// tuple) shows just its type (`` `1, "hi"` — int * string ``), since "literal"
/// would misdescribe it — see [`covers_literal`]. The source text is taken
/// verbatim from `range`; a string literal carrying a backtick could nominally
/// unbalance the code span, but that is a cosmetic rendering edge, never wrong
/// information.
fn literal_hover_body(text: &str, root: &SyntaxNode, range: TextRange, ty: &Ty) -> String {
    let value = &text[usize::from(range.start())..usize::from(range.end())];
    // A *literal* keeps its descriptor (`` `42` — int literal ``); any other
    // inferred expression — a tuple today, a call once 3.2c lands — shows just
    // its type (`` `1, "hi"` — int * string ``), since "literal" would
    // misdescribe it. The kind is read off the covering syntax node rather than
    // guessed from the `Ty` (a `Ty::Named` is produced by both a literal and a
    // future call).
    let value = code_span(value);
    let ty = code_span(&ty.render_fsharp());
    if covers_literal(root, range) {
        format!("{value} — {ty} literal")
    } else {
        format!("{value} — {ty}")
    }
}

/// Whether the inferred-type `range` is a literal expression. Inference records
/// a literal at its *token* range (parented by `CONST_EXPR`); a compound
/// expression (a tuple) records its node range, so the covering element
/// distinguishes them.
fn covers_literal(root: &SyntaxNode, range: TextRange) -> bool {
    let element = root.covering_element(range);
    match element.as_token() {
        Some(token) => token
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::CONST_EXPR),
        None => element
            .as_node()
            .is_some_and(|n| n.kind() == SyntaxKind::CONST_EXPR),
    }
}

/// Wrap the body in an LSP [`Hover`] using markdown content. Including the
/// `range` field scopes the editor tooltip to the cursor's symbol.
/// Whether every line of `body` carrying an F# quoted identifier is fenced past
/// it — a two-backtick run inside a one-backtick span, or in a bare paragraph, is
/// consumed by CommonMark as a delimiter, so the quotes vanish from what the user
/// reads and the name silently becomes a different one.
///
/// The [`Fenced`] type makes this unskippable for the bodies built by
/// [`assemble_body`], but a hover body assembled any other way (an inferred
/// expression's type, say) has no such gate, so the invariant is also asserted
/// here, at the one place every hover passes through. A `debug_assert` because
/// the test suite is where it must fire: shipping a mangled hover is bad,
/// panicking in a user's editor over it is worse.
fn quotes_are_fenced(body: &str) -> bool {
    body.lines()
        .filter(|line| line.contains("``"))
        .all(|line| line.contains("```"))
}

fn make_hover(body: String, text: &str, range: TextRange) -> Hover {
    debug_assert!(
        quotes_are_fenced(&body),
        "a hover body carries F# backticks on a line Markdown will eat them \
         from; wrap that part in `code_span`:\n{body}"
    );
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: body,
        }),
        range: Some(range_to_lsp(text, range)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borzoi_assembly::{
        Access, Field, MethodLike, MethodSignature, Nullability, Primitive, Property, TypeRef,
        Version,
    };

    fn method(is_extension_method: bool) -> Member {
        Member::Method(MethodLike {
            definition_range: None,
            name: "M".to_string(),
            access: Access::Public,
            signature: MethodSignature {
                parameters: vec![],
                return_type: TypeRef::Primitive(Primitive::Void),
                return_nullability: Nullability::Oblivious,
            },
            augmentation: Augmentation::No,
            arg_group_count: Some(1),
            is_static: true,
            is_virtual: false,
            is_abstract: false,
            is_constructor: false,
            module_value: None,
            is_module_value_binding: false,
            is_extension_method,
            is_final: false,
            is_newslot: false,
            is_hide_by_sig: false,
            generic_parameters: vec![],
            obsolete: None,
            experimental: None,
            sets_required_members: false,
            compiler_feature_required: vec![],
            source_name: None,
            custom_attrs: vec![],
            metadata_token: 0,
            implements: Vec::new(),
            unclassified_impls: Vec::new(),
        })
    }

    fn field(is_required: bool) -> Member {
        Member::Field(Field {
            name: "F".to_string(),
            access: Access::Public,
            ty: TypeRef::Primitive(Primitive::I4),
            is_static: false,
            is_init_only: false,
            is_volatile: false,
            is_literal: false,
            is_required,
            compiler_feature_required: vec![],
            nullability: Nullability::Oblivious,
            custom_attrs: vec![],
        })
    }

    fn property(is_required: bool) -> Member {
        Member::Property(Property {
            name: "P".to_string(),
            access: Access::Public,
            ty: TypeRef::Primitive(Primitive::I4),
            parameters: vec![],
            is_static: false,
            has_getter: true,
            has_setter: false,
            getter_access: Some(Access::Public),
            is_required,
            compiler_feature_required: vec![],
            nullability: Nullability::Oblivious,
            custom_attrs: vec![],
            implements: Vec::new(),
            unclassified_impls: Vec::new(),
        })
    }

    #[test]
    fn file_name_of_takes_the_last_path_component() {
        assert_eq!(
            file_name_of(r"D:\github\repos\FsUnit\src\FsUnit.NUnit\FsUnitTyped.fs"),
            "FsUnitTyped.fs"
        );
        assert_eq!(
            file_name_of("/_/src/fslib-extra-pervasives.fs"),
            "fslib-extra-pervasives.fs"
        );
        assert_eq!(file_name_of("bare.fs"), "bare.fs");
    }

    #[test]
    fn defined_in_line_shows_basename_and_line() {
        let doc = DefinitionDocument {
            document: r"D:\github\repos\FsUnit\src\FsUnit.NUnit\FsUnitTyped.fs".to_string(),
            line: 10,
            column: 5,
        };
        assert_eq!(
            defined_in_line(&doc),
            "Defined in `FsUnitTyped.fs`, line 10"
        );
    }

    #[test]
    fn append_defined_in_is_a_noop_without_a_location() {
        let mut body = "head".to_string();
        append_defined_in(&mut body, None);
        assert_eq!(body, "head");
    }

    #[test]
    fn member_qualifier_marks_extension_and_required() {
        assert_eq!(member_qualifier(&method(true)), Some("extension member"));
        assert_eq!(member_qualifier(&method(false)), None);
        assert_eq!(member_qualifier(&field(true)), Some("required member"));
        assert_eq!(member_qualifier(&field(false)), None);
        assert_eq!(member_qualifier(&property(true)), Some("required member"));
        assert_eq!(member_qualifier(&property(false)), None);
    }

    /// An F# quoted identifier is made of backticks, so the usual
    /// single-backtick code span would end in the middle of the name. The fence
    /// grows past the longest run inside, and content that starts or ends with a
    /// backtick is padded (CommonMark strips the padding when rendering).
    #[test]
    fn code_span_fences_past_any_backticks_inside() {
        assert_eq!(code_span("type List<'T>"), "`type List<'T>`");
        assert_eq!(
            code_span("union case ``Circle Case``"),
            "``` union case ``Circle Case`` ```"
        );
        assert_eq!(code_span("``Odd``"), "``` ``Odd`` ```");
    }

    #[test]
    fn append_typars_uses_the_apostrophe_convention() {
        assert_eq!(
            append_typars("System.Collections.Generic.List".to_string(), &["T"]),
            "System.Collections.Generic.List<'T>"
        );
        assert_eq!(
            append_typars("NS.Map".to_string(), &["TKey", "TValue"]),
            "NS.Map<'TKey, 'TValue>"
        );
    }

    #[test]
    fn append_typars_leaves_a_non_generic_name_alone() {
        assert_eq!(
            append_typars("System.Console".to_string(), &[]),
            "System.Console"
        );
    }

    #[test]
    fn entity_kind_words_are_stable() {
        assert_eq!(entity_kind_word(EntityKind::Class), "class");
        assert_eq!(entity_kind_word(EntityKind::Struct), "struct");
        assert_eq!(entity_kind_word(EntityKind::Interface), "interface");
        assert_eq!(entity_kind_word(EntityKind::Enum), "enum");
        assert_eq!(entity_kind_word(EntityKind::Delegate), "delegate");
        assert_eq!(entity_kind_word(EntityKind::Module), "module");
        assert_eq!(entity_kind_word(EntityKind::Union), "union");
        assert_eq!(entity_kind_word(EntityKind::Record), "record");
        assert_eq!(
            entity_kind_word(EntityKind::Abbreviation),
            "type abbreviation"
        );
        assert_eq!(entity_kind_word(EntityKind::Exception), "exception");
        assert_eq!(entity_kind_word(EntityKind::Measure), "unit of measure");
    }

    fn identity(name: &str, version: Version) -> AssemblyIdentity {
        AssemblyIdentity {
            name: name.to_string(),
            version,
            public_key_token: None,
        }
    }

    #[test]
    fn assembly_provenance_renders_name_and_four_part_version() {
        let id = identity(
            "System.Console",
            Version {
                major: 9,
                minor: 0,
                build: 0,
                revision: 0,
            },
        );
        assert_eq!(assembly_provenance(&id), "from System.Console v9.0.0.0");
    }

    #[test]
    fn assembly_provenance_includes_strong_name_token_as_lowercase_hex() {
        let id = AssemblyIdentity {
            name: "FSharp.Core".to_string(),
            version: Version {
                major: 9,
                minor: 0,
                build: 0,
                revision: 0,
            },
            public_key_token: Some([0xb0, 0x3f, 0x5f, 0x7f, 0x11, 0xd5, 0x0a, 0x3a]),
        };
        assert_eq!(
            assembly_provenance(&id),
            "from FSharp.Core v9.0.0.0, PublicKeyToken=b03f5f7f11d50a3a"
        );
    }

    #[test]
    fn obsolete_banner_distinguishes_error_from_warning_and_optional_message() {
        assert_eq!(
            obsolete_banner(&Obsolete {
                message: Some("use Span instead".to_string()),
                is_error: true,
            }),
            "⚠ Obsolete (error): use Span instead"
        );
        assert_eq!(
            obsolete_banner(&Obsolete {
                message: Some("deprecated".to_string()),
                is_error: false,
            }),
            "⚠ Obsolete: deprecated"
        );
        assert_eq!(
            obsolete_banner(&Obsolete {
                message: None,
                is_error: false,
            }),
            "⚠ Obsolete"
        );
    }

    #[test]
    fn experimental_banner_combines_diagnostic_id_and_message() {
        assert_eq!(
            experimental_banner(&Experimental {
                diagnostic_id: Some("FS0057".to_string()),
                url_format: None,
                message: Some("may change".to_string()),
            }),
            "⚠ Experimental (FS0057): may change"
        );
        assert_eq!(
            experimental_banner(&Experimental {
                diagnostic_id: Some("FS0057".to_string()),
                url_format: None,
                message: None,
            }),
            "⚠ Experimental (FS0057)"
        );
        assert_eq!(
            experimental_banner(&Experimental {
                diagnostic_id: None,
                url_format: None,
                message: None,
            }),
            "⚠ Experimental"
        );
    }
}

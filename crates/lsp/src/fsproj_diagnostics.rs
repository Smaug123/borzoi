//! `.fsproj`-buffer diagnostics for `textDocument/publishDiagnostics`.
//!
//! Parses the buffer with [`parse_fsproj_with_imports`], spliced with an
//! [`SdkDiscovery`]-derived resolver (so `<Project Sdk="X">` references
//! get resolved against the host's `$DOTNET_ROOT`/`$NUGET_PACKAGES` and
//! any ancestor `global.json` pin), and translates every emitted
//! [`borzoi_msbuild::Diagnostic`] into an LSP one.
//!
//! On top of that it adds reference diagnostics: warnings for the buffer's
//! own `<ProjectReference>` elements whose target is missing or is a project
//! kind we don't model (consumer #3 stage 3.2).
//!
//! Severities are uniformly `WARNING`: the parser is by-design best-effort
//! and each diagnostic records a *divergence from MSBuild*, not a fatal
//! syntax error. The one `ERROR` case is when the XML itself doesn't
//! parse — `parse_fsproj_with_imports` returns `Err(ParseError::Xml)`,
//! which surfaces as a single error at the document head with the
//! roxmltree message.
//!
//! Discovery errors ([`crate::sdk_discovery::DiscoveryError`]) are
//! *not* surfaced. A missing `$DOTNET_ROOT` (sandbox without a `dotnet`
//! install) and an unreadable/unparseable `global.json` both demote us
//! to the no-SDK code path; the user notices because the SDK attribute
//! then surfaces as `UnsupportedConstruct`. Pointing at an ancestor
//! `global.json` from the fsproj buffer would be misleading — different
//! file.
//!
//! See [`crate::position::offset_to_position`] for how byte spans (from
//! roxmltree, by way of the msbuild parser) become UTF-16 LSP positions.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use lsp_types::{Diagnostic, DiagnosticSeverity, Range};

use borzoi_msbuild::{
    DiagnosticKind, DiagnosticOrigin, GlobResolver, ImportFailReason, ParseError, ResolvedItem,
    SdkResolver, SdkVersion, VersionSpec, parse_fsproj_with_imports,
};

use crate::paths::lexically_normalize;
use crate::position::offset_to_position;
use crate::project_graph::{ProjectGraph, ProjectKind, classify};
use crate::sdk_discovery::{SdkDiscovery, SdkDiscoveryEnv};

/// Translate one fsproj buffer into LSP diagnostics. See module-level
/// docs for severity/surfacing conventions.
///
/// `project_path` must be the on-disk path to the fsproj being parsed:
/// it seeds MSBuild's path-derived reserved properties, anchors relative
/// `<Import Project="…">` resolution, and is the starting point for the
/// `global.json` walk inside [`SdkDiscovery::for_project`].
pub fn diagnostics_for(text: &str, project_path: &Path, env: &SdkDiscoveryEnv) -> Vec<Diagnostic> {
    let extras = default_global_properties();
    // The environment the *caller* declared, not the ambient process: a
    // workspace constructed with an explicit `SdkDiscoveryEnv` has asked not to
    // see host variables, and reading `std::env` here would leak them back into
    // `$(…)` evaluation. `SdkDiscoveryEnv::from_process_env` is what fills this
    // with the real environment for production callers.
    let environment = env.build_environment.clone();
    // Glob resolution is independent of SDK discovery, so both arms get the
    // filesystem-backed resolver. It borrows nothing, so it outlives the match.
    let glob_resolver: &GlobResolver<'_> = &crate::glob_resolver::resolve;
    let parse_result = match SdkDiscovery::for_project(project_path, env) {
        Ok(disc) => {
            // The closure borrows `disc`; both stay live until the
            // match arm exits, which is after `parse_fsproj_with_imports`
            // has finished using the resolver.
            let resolver: &SdkResolver<'_> = &|name| disc.resolve(name);
            parse_fsproj_with_imports(
                text,
                project_path,
                &extras,
                &environment,
                Some(resolver),
                Some(glob_resolver),
            )
        }
        Err(_) => parse_fsproj_with_imports(
            text,
            project_path,
            &extras,
            &environment,
            None,
            Some(glob_resolver),
        ),
    };
    match parse_result {
        Ok(parsed) => {
            let mut diags: Vec<Diagnostic> = parsed
                .diagnostics
                .into_iter()
                // Drop *content-level* diagnostics whose underlying cause
                // lives inside an imported file (SDK targets, ancestor
                // `Directory.Build.*`, a followed `<Import>` chain). Their
                // spans were remapped to the buffer's `<Import>` site so
                // the byte offset is valid, but the user can't act on a
                // `<Target>` or `Exists(...)` condition in
                // `Microsoft.NET.Sdk` from their own `.fsproj`.
                //
                // Meta-import diagnostics (`ImportFailed`, `SdkNotFound`,
                // `SdkVersionNotSatisfied`, `UnresolvedImport`) describe
                // the *integrity of the import chain*. Top-level ones are
                // already `Buffer`-origin because the walker pushes them
                // before descending. But a *nested* import that itself
                // fails — `Directory.Build.props` importing a missing
                // child — is pushed while the walker is inside the first
                // import, so origin=`Imported`. We still want the user to
                // see it: a broken nested import means evaluation is
                // partial just as much as a broken top-level one.
                .filter(|d| {
                    matches!(d.origin, DiagnosticOrigin::Buffer)
                        || matches!(
                            d.kind,
                            DiagnosticKind::ImportFailed { .. }
                                | DiagnosticKind::SdkNotFound { .. }
                                | DiagnosticKind::SdkVersionNotSatisfied { .. }
                                | DiagnosticKind::SdkResolutionUnsupported { .. }
                                | DiagnosticKind::UnresolvedImport { .. }
                        )
                })
                .map(|d| to_lsp_diagnostic(text, &d.kind, &d.span))
                .collect();
            diags.extend(reference_diagnostics(text, &parsed.project_references));
            // RestoreStale (consumer #3 stage 3.2): a real, SDK-resolved project
            // whose `obj/project.assets.json` is absent — the file the semantic
            // layer reads for cross-assembly resolution. `resolved_sdk_root` gates
            // the check to SDK-style projects (a bare `<Project>` has nothing to
            // restore, so it never warns).
            let unrestored = parsed.resolved_sdk_root.is_some() && !assets_present(project_path);
            if let Some(d) = restore_diagnostic(unrestored) {
                diags.push(d);
            }
            diags
        }
        Err(ParseError::Xml(e)) => vec![lsp_error_at_origin(format!("malformed XML: {e}"))],
        // `parse_fsproj_with_imports` rejects only three other inputs:
        // a relative `project_path` (we always pass a rooted one from
        // the LSP URL), a reserved-name in `extra_properties`, and
        // case-insensitive duplicate `extra_properties` keys (we pass
        // an empty map). None of these are reachable from this entry
        // point. We still surface them rather than panic so a future
        // call-site change doesn't silently swallow a real bug.
        Err(other) => vec![lsp_error_at_origin(format!("fsproj parse failed: {other}"))],
    }
}

/// Diagnostics for the buffer's own `<ProjectReference>` elements (consumer #3
/// stage 3.2, `docs/completed/fsproj-project-graph-plan.md`): a reference whose target
/// file is missing, or whose target is a project kind we don't model.
///
/// `references` is the **with-imports** evaluation's `project_references`, so
/// `Condition`s are evaluated against the same properties (`Directory.Build.*`,
/// SDK, `global.json`) MSBuild would use — a reference gated out by an imported
/// property simply isn't in the list, and we don't warn on it.
///
/// We diagnose only the buffer's *own* references: a reference is kept when its
/// span points at a `<ProjectReference` element in *this* buffer's text (see
/// [`span_points_at_project_reference`]). That filters out references
/// contributed by imported files (their spans don't land on a buffer
/// `<ProjectReference>`) and guarantees the span is a valid buffer offset for
/// the squiggle. Transitive problems (a cycle's back-edge, a dependency's own
/// broken reference) live in *other* files — anchoring those needs cross-file
/// publishing and is deferred. Existence is checked against the
/// lexically-normalised path, the identity [`crate::project_graph`] uses.
fn reference_diagnostics(text: &str, references: &[ResolvedItem]) -> Vec<Diagnostic> {
    references
        .iter()
        .filter(|item| span_points_at_project_reference(text, &item.span))
        .filter_map(|item| {
            reference_problem(&item.include)
                .map(|message| reference_diagnostic(text, &item.span, message))
        })
        .collect()
}

/// Whether `span` selects a `ProjectReference` element in `text` — i.e. the
/// reference was authored in *this* buffer (not contributed by an imported
/// file, whose item spans don't land on a buffer `<ProjectReference>`). Also
/// rejects spans that aren't valid `str` slice boundaries, so the caller never
/// indexes outside the buffer.
///
/// Matches the element's **local name** exactly, so a namespace prefix
/// (`<msb:ProjectReference>`, which the msbuild parser records by local name)
/// is accepted while a look-alike (`<ProjectReferenceGroup>`) is not.
fn span_points_at_project_reference(text: &str, span: &std::ops::Range<usize>) -> bool {
    let Some(rest) = text.get(span.start..span.end).map(str::trim_start) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('<') else {
        return false;
    };
    // The element name runs up to the first whitespace, `/`, or `>`.
    let name_end = rest
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .unwrap_or(rest.len());
    // Strip an optional `prefix:` to compare the local name.
    let local = rest[..name_end].rsplit(':').next().unwrap_or("");
    local == "ProjectReference"
}

/// The warning message for a `<ProjectReference>` target, or `None` when the
/// target is fine (an existing `.fsproj`/`.csproj`).
fn reference_problem(target: &Path) -> Option<String> {
    let normalized = lexically_normalize(target);
    match classify(&normalized) {
        ProjectKind::Other => Some(format!(
            "unsupported project reference: {} (only .fsproj and .csproj are modelled)",
            normalized.display(),
        )),
        ProjectKind::FSharp | ProjectKind::CSharp => {
            if normalized.exists() {
                None
            } else {
                Some(format!(
                    "referenced project does not exist: {}",
                    normalized.display(),
                ))
            }
        }
    }
}

/// Graph-derived diagnostics for the **open** `.fsproj` buffer: a
/// `ReferenceCycle` WARNING on each of the entry's own `<ProjectReference>`s that
/// participates in a project-reference cycle (consumer #3 stage 3.2, Stage B).
///
/// **Entry-anchored** (`docs/completed/fsproj-project-graph-plan.md`): an edge `entry → d`
/// participates in a cycle *through the entry* exactly when `d` can reach the
/// entry again. We compute that by **reachability over the graph's node
/// adjacency** — not from any single recorded cycle path — so an
/// *unanchorable* edge (e.g. one contributed by an `<Import>`, whose span is
/// remapped to the import site) can never hide an anchorable local edge into the
/// same cycle. We anchor on the entry's **own** buffer `<ProjectReference>`,
/// never on a back-edge in another file, so no cross-file publishing and no
/// cross-entry dedup are needed.
///
/// `graph` is built from disk (the `Workspace` cache); we anchor a span only
/// when it still selects a `<ProjectReference>` in *this* buffer's `text`, so an
/// unsaved buffer that has diverged from disk degrades to no squiggle rather than
/// a misplaced one (the caller additionally gates on buffer == disk).
pub fn graph_diagnostics(text: &str, entry: &Path, graph: &ProjectGraph) -> Vec<Diagnostic> {
    let entry_key = lexically_normalize(entry);
    let Some(entry_node) = graph.nodes.iter().find(|n| n.path == entry_key) else {
        return Vec::new();
    };
    // Adjacency over the discovered nodes (project → its reference targets).
    let adjacency: HashMap<&Path, Vec<&Path>> = graph
        .nodes
        .iter()
        .map(|n| {
            (
                n.path.as_path(),
                n.references.iter().map(|e| e.target.as_path()).collect(),
            )
        })
        .collect();

    let mut out = Vec::new();
    let mut seen_edges: HashSet<usize> = HashSet::new();
    for edge in &entry_node.references {
        // Only the entry's own buffer `<ProjectReference>` edges are anchorable.
        if !span_points_at_project_reference(text, &edge.span) {
            continue;
        }
        // This edge is in a cycle through the entry iff its target reaches back.
        if !reaches(&adjacency, &edge.target, &entry_key) {
            continue;
        }
        if !seen_edges.insert(edge.span.start) {
            continue;
        }
        out.push(reference_diagnostic(
            text,
            &edge.span,
            cycle_message(entry, &edge.target),
        ));
    }
    out
}

/// Whether `target` is reachable from `from` by following `adjacency` edges
/// (≥1 hop). A breadth/depth-first walk with a visited set, so it terminates on
/// cyclic graphs.
fn reaches(adjacency: &HashMap<&Path, Vec<&Path>>, from: &Path, target: &Path) -> bool {
    let mut stack: Vec<&Path> = vec![from];
    let mut visited: HashSet<&Path> = HashSet::new();
    while let Some(node) = stack.pop() {
        if let Some(neighbours) = adjacency.get(node) {
            for &next in neighbours {
                if next == target {
                    return true;
                }
                if visited.insert(next) {
                    stack.push(next);
                }
            }
        }
    }
    false
}

/// Render a cycle anchored on `entry`'s reference to `first_hop` as
/// `project reference cycle: A.fsproj → B.fsproj → … → A.fsproj` (file names).
fn cycle_message(entry: &Path, first_hop: &Path) -> String {
    fn name(p: &Path) -> &str {
        p.file_name().and_then(|n| n.to_str()).unwrap_or("?")
    }
    format!(
        "project reference cycle: {} → {} → … → {}",
        name(entry),
        name(first_hop),
        name(entry),
    )
}

/// Build a `WARNING` diagnostic at `span` (consistent with the module's
/// uniform-warning convention). Unlike [`to_lsp_diagnostic`] the message is
/// supplied directly — these are LSP-level reference problems, not msbuild
/// [`DiagnosticKind`]s.
fn reference_diagnostic(text: &str, span: &std::ops::Range<usize>, message: String) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: offset_to_position(text, span.start),
            end: offset_to_position(text, span.end),
        },
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some("borzoi".to_string()),
        message,
        ..Default::default()
    }
}

/// Whether the LSP can find the project's restore output — i.e. whether
/// `<project_dir>/obj/project.assets.json` exists. The only IO in the
/// RestoreStale path.
///
/// Deliberately the **same `<project>/obj/project.assets.json` lookup the
/// semantic layer uses** ([`crate::semantic::SemanticState`]'s
/// `build_assembly_env`): the warning's job is to explain *the LSP's* degraded
/// cross-assembly resolution, which is driven by what that layer reads. A
/// project that relocates its assets (via `ProjectAssetsFile` /
/// `MSBuildProjectExtensionsPath` / `BaseIntermediateOutputPath`) is genuinely
/// unresolvable by the LSP today — the warning correctly fires — so the two must
/// move together if custom asset paths are ever supported.
fn assets_present(project_path: &Path) -> bool {
    project_path
        .parent()
        .map(|dir| dir.join("obj").join("project.assets.json").exists())
        .unwrap_or(false)
}

/// A project-level WARNING when `unrestored`. Anchored at the file head
/// (`Range::default()`) — the module's convention for whole-file diagnostics
/// (see the malformed-XML error) — which also avoids mis-anchoring inside a
/// leading comment. Pure: the SDK / restore-state checks are the caller's.
fn restore_diagnostic(unrestored: bool) -> Option<Diagnostic> {
    if !unrestored {
        return None;
    }
    Some(Diagnostic {
        range: Range::default(),
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some("borzoi".to_string()),
        message: "no obj/project.assets.json — the LSP reads restore output there for \
                  cross-assembly resolution, which is degraded until the project is restored \
                  (run `dotnet restore`)"
            .to_string(),
        ..Default::default()
    })
}

/// Seed MSBuild "global" properties matching the defaults `dotnet build`
/// uses when invoked with no `-c`/`-p:Platform` flags. Without these,
/// the most common condition shape in real `.fsproj` files —
/// `Condition="'$(Configuration)|$(Platform)' == 'Debug|AnyCPU'"` —
/// emits an `UndefinedProperty` diagnostic on every clean buffer.
///
/// These are intentionally a tiny set. Each addition trades a class
/// of false positives for the risk of evaluating user conditions
/// against a value the user didn't pick. For an editor surface, the
/// "what would `dotnet build` do?" defaults are the least surprising
/// reading.
fn default_global_properties() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert(
        "Configuration".to_string(),
        crate::BUILD_CONFIGURATION.to_string(),
    );
    m.insert("Platform".to_string(), "AnyCPU".to_string());
    m
}

/// Snapshot of the server's process environment, handed to
/// [`parse_fsproj_with_imports`] as its `environment` parameter. MSBuild
/// folds every environment variable in as an initial (project-overridable)
/// property; the server's own environment — typically inherited from the
/// editor, which inherited it from the user's shell — is the closest
/// available stand-in for what that user's `dotnet build` would see.
///
/// Built from `vars_os`, not `vars`: Unix permits non-Unicode variable
/// names and values, and `std::env::vars` *panics* on them — merely
/// opening a project must not take the server down. Lossy conversion
/// mirrors .NET's own view (it decodes the environ as UTF-8 with
/// replacement characters); a name that needed replacement can never be
/// referenced from `$(…)` anyway, so the evaluator skips it.
pub(crate) fn process_environment() -> HashMap<String, String> {
    std::env::vars_os()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// .NET's `LocalApplicationData` special folder, derived per-platform the way
/// `Environment.GetFolderPath` does but from explicit inputs, so the server
/// never shells out. `None` when the base cannot be determined (no home
/// directory, or Windows) — the caller then leaves `MSBuildUserExtensionsPath`
/// unseeded and the evaluation degrades, rather than committing to a guessed
/// path.
///
/// **Windows declines.** There, `GetFolderPath(LocalApplicationData)` reads the
/// `FOLDERID_LocalAppData` *known-folder* API, not the `%LOCALAPPDATA%`
/// environment variable — and the two can diverge (a redirected or stale env
/// var). Seeding from `%LOCALAPPDATA%` could therefore point at a path MSBuild
/// does not use and silently miss real `ImportBefore`/`ImportAfter` extensions
/// while reporting certainty — an over-resolution. Rather than query the Win32
/// API from this Unix-first server, decline (degrade to today's behaviour); a
/// Windows host that wants certainty can pass the real value in explicitly.
///
/// Unix note — a deliberate, bounded approximation. .NET resolves this folder
/// from the OS *account* profile, not `$HOME`: on macOS via `NSSearchPath`
/// (probed: redirecting `$HOME`/`$XDG_DATA_HOME` does not move it), on Linux via
/// the account database for the `$HOME/.local/share` fallback. We approximate it
/// with the resolved home directory (`SdkDiscoveryEnv::home_dir`, `$HOME`-first).
/// For an *editor-launched* server — the only deployment this runs in — `$HOME`
/// is the account home, so the derivation is exact; and when `$HOME` is unset the
/// home resolution falls back to the same account database .NET reads. The two
/// diverge only under a deliberately-redirected, non-empty `$HOME` (or a Linux
/// process with `USERPROFILE` set but `HOME` unset), and even then only for a
/// user who has installed global MSBuild `ImportBefore`/`ImportAfter` extensions
/// under their account-home MSBuild directory — the common case (no such
/// extensions) resolves identically because both paths are absent. Reproducing
/// the native lookup *exactly* would need `getpwuid` FFI or a `dotnet msbuild`
/// probe; both are declined here (Unix-first server, no `libc` dependency, no
/// production subprocess), so this stays a documented approximation rather than
/// an exact reproduction.
fn local_application_data(
    home_dir: Option<&Path>,
    get_env: &impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    if cfg!(windows) {
        // The known-folder API is not `%LOCALAPPDATA%`; decline (see doc).
        None
    } else if cfg!(target_os = "macos") {
        home_dir.map(|h| h.join("Library").join("Application Support"))
    } else {
        // Linux and other Unix follow the XDG spec: `$XDG_DATA_HOME` when it is
        // an absolute path, else `$HOME/.local/share`.
        get_env("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home_dir.map(|h| h.join(".local").join("share")))
    }
}

/// MSBuild's computed `MSBuildUserExtensionsPath` reserved property, in the
/// **raw** form MSBuild stores it: `{LocalApplicationData}` joined with the
/// single literal segment `Microsoft\MSBuild`. The backslash is part of that
/// segment, not a separator, so on Unix the stored value genuinely contains a
/// backslash (MSBuild converts it to `/` only when the value is later expanded
/// into a path); handing the evaluator this verbatim value matches the property
/// table the real MSBuild builds.
///
/// This is the seed a real editor evaluation needs to reach the same certainty
/// the msbuild crate's differential fixtures do: without it,
/// `Microsoft.Common.props`'s user-extension import gates read an undefined
/// property, the SDK-chain walk turns opaque, and the whole PackageReference
/// set degrades to uncertain (so the in-house NuGet resolver would decline).
/// `None` (⇒ leave unseeded ⇒ degrade) when the `LocalApplicationData` base
/// cannot be determined. Exposed so the integration test can pin it against
/// `dotnet msbuild -getProperty:MSBuildUserExtensionsPath`.
pub fn msbuild_user_extensions_path(
    home_dir: Option<&Path>,
    get_env: impl Fn(&str) -> Option<OsString>,
) -> Option<String> {
    let base = local_application_data(home_dir, &get_env)?;
    // `Path.Combine(base, "Microsoft\\MSBuild")`: append the verbatim segment
    // with the platform separator, without doubling one already present.
    let sep = std::path::MAIN_SEPARATOR;
    let base = base.to_string_lossy();
    let base = base.strip_suffix(sep).unwrap_or(&base);
    Some(format!("{base}{sep}Microsoft\\MSBuild"))
}

/// The MSBuild property-seed environment for a process: the raw environment
/// snapshot ([`process_environment`]) plus the reserved-ish properties MSBuild
/// computes for itself ([`msbuild_user_extensions_path`]). A real environment
/// variable of the same name wins (MSBuild folds the environment in over its
/// own computed default — probed: an environment `MSBuildUserExtensionsPath`
/// displaces the computed one).
pub(crate) fn msbuild_property_environment(
    mut env: HashMap<String, String>,
    home_dir: Option<&Path>,
    get_env: impl Fn(&str) -> Option<OsString>,
) -> HashMap<String, String> {
    // Only seed the computed default when the process supplied no
    // `MSBuildUserExtensionsPath` at all. The check is case-insensitive:
    // MSBuild treats property names case-insensitively, so a differently-cased
    // env var (`msbuilduserextensionspath` — a distinct name on case-sensitive
    // Unix) is a genuine override that must win. Adding a second, canonical-case
    // key would instead create a collision the evaluator drops as unmodellable,
    // turning a valid override into an uncertain walk.
    let already_present = env
        .keys()
        .any(|key| key.eq_ignore_ascii_case("MSBuildUserExtensionsPath"));
    if !already_present && let Some(value) = msbuild_user_extensions_path(home_dir, get_env) {
        // The derived value is a *literal* filesystem path; the `environment`
        // map is the escaped-value domain (the evaluator unescapes on use, so a
        // real env var's `%XX` decodes). Escape the computed default the way
        // MSBuild escapes its toolset seeds, so a `%`/`;`/`(` in the path is
        // treated as a literal rather than decoded — matching MSBuild's own
        // `MSBuildUserExtensionsPath`. A path with no special characters (the
        // common case) is returned unchanged.
        env.insert(
            "MSBuildUserExtensionsPath".to_string(),
            borzoi_msbuild::escape(&value),
        );
    }
    env
}

fn lsp_error_at_origin(message: String) -> Diagnostic {
    Diagnostic {
        range: Range::default(),
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("borzoi".to_string()),
        message,
        ..Default::default()
    }
}

fn to_lsp_diagnostic(
    text: &str,
    kind: &DiagnosticKind,
    span: &std::ops::Range<usize>,
) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: offset_to_position(text, span.start),
            end: offset_to_position(text, span.end),
        },
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some("borzoi".to_string()),
        message: message(kind),
        ..Default::default()
    }
}

/// User-facing message per diagnostic kind. Exhaustive match: a new
/// `DiagnosticKind` variant added to the msbuild crate must come back
/// here, otherwise the LSP would silently drop it.
fn message(kind: &DiagnosticKind) -> String {
    match kind {
        DiagnosticKind::UnresolvedImport { path } => {
            format!(
                "unresolved <Import Project=\"{path}\"> — the pure parser does not follow imports"
            )
        }
        DiagnosticKind::ImportFailed { path, reason } => {
            format!(
                "failed to follow import {}: {}",
                path.display(),
                import_fail_message(reason),
            )
        }
        DiagnosticKind::UnsupportedConstruct { element } => {
            format!("unsupported MSBuild construct: <{element}>")
        }
        DiagnosticKind::UnsupportedGlob { pattern } => {
            format!("glob pattern not expanded: {pattern}")
        }
        DiagnosticKind::UndefinedProperty { name } => {
            format!("$({name}) is not defined — substituted as empty")
        }
        DiagnosticKind::UnsupportedPropertyExpression { expression } => {
            format!("$(...) expression not understood, left literal: {expression}")
        }
        DiagnosticKind::UnresolvedItemReference { reference } => {
            // `reference` is the full expanded value as it appears in
            // the source, including its `@(...)` syntax. Wrapping it
            // again here would render `@(@(Other))`, pointing the
            // user at text that isn't in their buffer.
            format!("item reference not expanded: {reference} — item evaluation is out of scope")
        }
        DiagnosticKind::UnresolvedMetadataReference { reference } => {
            format!("metadata reference not expanded: {reference}")
        }
        DiagnosticKind::UnsupportedCondition { condition } => {
            format!(
                "Condition=\"{condition}\" uses syntax beyond the supported subset — \
                 treated as false (exclusionary)"
            )
        }
        DiagnosticKind::UnsupportedItemOperation { operation } => {
            format!("item operation not supported: {operation}")
        }
        DiagnosticKind::SdkNotFound { name } => {
            format!(
                "SDK '{name}' not found — check $DOTNET_ROOT and ~/.nuget/packages, \
                 or that the SDK is installed"
            )
        }
        DiagnosticKind::SdkVersionNotSatisfied {
            name,
            spec,
            available,
        } => {
            format!(
                "SDK '{name}' has no version satisfying {} (available: {})",
                describe_spec(spec),
                describe_available(available),
            )
        }
        DiagnosticKind::SdkResolutionUnsupported { name, reason } => {
            format!(
                "SDK '{name}' resolution declined — the on-disk state is \
                 outside what can be resolved exactly: {reason}"
            )
        }
        DiagnosticKind::ImplicitImportPresent { path, kind } => {
            // Should not appear via parse_fsproj_with_imports (it
            // splices implicit imports itself and suppresses this
            // diagnostic). Render conservatively in case the contract
            // ever changes.
            format!(
                "implicit import discovered: {:?} at {}",
                kind,
                path.display()
            )
        }
    }
}

fn import_fail_message(reason: &ImportFailReason) -> String {
    match reason {
        ImportFailReason::NotFound => "file does not exist".to_string(),
        ImportFailReason::DepthLimit { depth } => {
            format!("import depth limit hit (depth={depth})")
        }
        ImportFailReason::MalformedXml { message } => format!("malformed XML: {message}"),
        ImportFailReason::Io { message } => format!("I/O error: {message}"),
    }
}

fn describe_spec(spec: &VersionSpec) -> String {
    match spec.version() {
        Some(v) => format!(
            "{v} (rollForward={:?}, allowPrerelease={})",
            spec.roll_forward(),
            spec.allow_prerelease()
        ),
        None => format!("any version (allowPrerelease={})", spec.allow_prerelease()),
    }
}

fn describe_available(versions: &[SdkVersion]) -> String {
    if versions.is_empty() {
        return "none".to_string();
    }
    let mut s = String::new();
    for (i, v) in versions.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&v.to_string());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A `get_env` closure supplying `%LOCALAPPDATA%` (used only on Windows;
    /// the Unix branches read the home directory, not the environment).
    fn fake_localappdata(value: &'static str) -> impl Fn(&str) -> Option<OsString> {
        move |name: &str| (name == "LOCALAPPDATA").then(|| OsString::from(value))
    }

    /// A home directory and matching `%LOCALAPPDATA%` so the derivation yields
    /// *some* value on every platform.
    fn platform_base_inputs() -> (PathBuf, impl Fn(&str) -> Option<OsString>) {
        if cfg!(windows) {
            (
                PathBuf::from(r"C:\Users\t"),
                fake_localappdata(r"C:\Users\t\AppData\Local"),
            )
        } else {
            (PathBuf::from("/home/t"), fake_localappdata("/unused"))
        }
    }

    // Value derivation is Unix-only (Windows declines — see
    // `local_application_data`), so these assert a produced value under `cfg(unix)`.
    #[test]
    #[cfg(unix)]
    fn user_extensions_path_appends_the_literal_microsoft_msbuild_segment() {
        let (home, get) = platform_base_inputs();
        let value = msbuild_user_extensions_path(Some(&home), &get).expect("a value");
        // The stored form keeps a literal backslash before `MSBuild` on every
        // platform (on Unix it is a normal path character, not a separator) —
        // this is exactly what `dotnet msbuild -getProperty` reports.
        let expected_suffix = format!("{}Microsoft\\MSBuild", std::path::MAIN_SEPARATOR);
        assert!(
            value.ends_with(&expected_suffix),
            "{value} should end with {expected_suffix}"
        );
        assert!(value.contains("Microsoft\\MSBuild"));
    }

    #[test]
    fn user_extensions_path_degrades_without_a_base() {
        // No home directory and no `%LOCALAPPDATA%` ⇒ nothing to derive from ⇒
        // `None`, so the caller leaves the property unseeded and degrades.
        assert_eq!(
            msbuild_user_extensions_path(None, |_: &str| None),
            None,
            "with no base folder the derivation must decline, not guess a path"
        );
    }

    #[test]
    #[cfg(unix)]
    fn property_environment_seeds_the_computed_value_when_absent() {
        let (home, get) = platform_base_inputs();
        let out = msbuild_property_environment(HashMap::new(), Some(&home), &get);
        // Seeded in the escaped-value domain (the common path has no special
        // characters, so `escape` is the identity here).
        let expected =
            borzoi_msbuild::escape(&msbuild_user_extensions_path(Some(&home), &get).unwrap());
        assert_eq!(
            out.get("MSBuildUserExtensionsPath"),
            Some(&expected),
            "the computed value must be seeded when the environment lacks it"
        );
    }

    #[test]
    #[cfg(unix)]
    fn property_environment_escapes_the_computed_value() {
        // A base folder carrying an MSBuild escape character (`$`) must be
        // seeded *escaped*, so the evaluator's unescape-on-use recovers the
        // literal path rather than mis-decoding it.
        let home = PathBuf::from("/home/t$x");
        let get = |_: &str| None;
        let value = msbuild_user_extensions_path(Some(&home), get).unwrap();
        assert!(
            value.contains('$'),
            "precondition: raw value has a `$`: {value}"
        );
        let out = msbuild_property_environment(HashMap::new(), Some(&home), get);
        let seeded = out.get("MSBuildUserExtensionsPath").unwrap();
        assert!(
            seeded.contains("%24") && !seeded.contains('$'),
            "the `$` must be escaped to %24 in the seeded value: {seeded}"
        );
        assert_eq!(*seeded, borzoi_msbuild::escape(&value));
    }

    #[test]
    fn property_environment_keeps_a_real_env_var_over_the_computed_value() {
        let (home, get) = platform_base_inputs();
        let mut raw = HashMap::new();
        raw.insert(
            "MSBuildUserExtensionsPath".to_string(),
            "from-real-env".to_string(),
        );
        let out = msbuild_property_environment(raw, Some(&home), &get);
        assert_eq!(
            out.get("MSBuildUserExtensionsPath").map(String::as_str),
            Some("from-real-env"),
            "a genuine environment variable must win, matching MSBuild"
        );
    }

    #[test]
    fn property_environment_respects_a_differently_cased_override() {
        // MSBuild property names are case-insensitive, so a lowercased env var
        // is a genuine override. Seeding a second canonical-case key would make
        // the evaluator drop both as an unmodellable collision, so we must not
        // add one: the existing (lowercased) key is left untouched and no
        // canonical-case key appears.
        let (home, get) = platform_base_inputs();
        let mut raw = HashMap::new();
        raw.insert(
            "msbuilduserextensionspath".to_string(),
            "from-lowercased-env".to_string(),
        );
        let out = msbuild_property_environment(raw, Some(&home), &get);
        assert_eq!(
            out.get("msbuilduserextensionspath").map(String::as_str),
            Some("from-lowercased-env"),
            "the differently-cased override must be preserved"
        );
        assert!(
            !out.contains_key("MSBuildUserExtensionsPath"),
            "no canonical-case duplicate may be added (it would collide)"
        );
    }

    /// Mirror of the helper in `sdk_discovery/tests.rs`. Keeping a local
    /// copy avoids exposing a test-only helper across modules; the cost
    /// is 10 duplicated lines.
    fn install_sdk(dotnet_root: &Path, version: &str, sdk_name: &str) {
        let sdk_root = dotnet_root
            .join("sdk")
            .join(version)
            .join("Sdks")
            .join(sdk_name)
            .join("Sdk");
        fs::create_dir_all(&sdk_root).unwrap();
        fs::write(sdk_root.join("Sdk.props"), "<Project/>").unwrap();
        fs::write(sdk_root.join("Sdk.targets"), "<Project/>").unwrap();
    }

    /// Build an `SdkDiscoveryEnv` with every field `None` unless the
    /// caller sets it. Same pattern as `env_with` in
    /// `sdk_discovery/tests.rs` — keeps tests from leaking the host's
    /// real env vars.
    fn env_with(f: impl FnOnce(&mut SdkDiscoveryEnv)) -> SdkDiscoveryEnv {
        let mut env = SdkDiscoveryEnv {
            host_default_allow_prerelease: true,
            ..SdkDiscoveryEnv::default()
        };
        f(&mut env);
        env
    }

    /// Mark a project's directory as restored (a stub `obj/project.assets.json`)
    /// so the RestoreStale check doesn't fire — the realistic "clean project"
    /// baseline. Only existence is checked, so the content is irrelevant.
    fn mark_restored(project_dir: &Path) {
        let obj = project_dir.join("obj");
        fs::create_dir_all(&obj).unwrap();
        fs::write(obj.join("project.assets.json"), "{}").unwrap();
    }

    #[test]
    fn clean_fsproj_no_diagnostics() {
        let tmp = TempDir::new().unwrap();
        let dotnet = tmp.path().join("dotnet");
        install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

        let project_dir = tmp.path().join("proj");
        fs::create_dir_all(&project_dir).unwrap();
        mark_restored(&project_dir);
        let project = project_dir.join("App.fsproj");
        let text = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <Compile Include="Library.fs" />
  </ItemGroup>
</Project>"#;
        fs::write(&project, text).unwrap();

        let env = env_with(|e| e.dotnet_root = Some(dotnet));
        let diags = diagnostics_for(text, &project, &env);
        assert!(
            diags.is_empty(),
            "expected no diagnostics for a clean fsproj, got {diags:#?}"
        );
    }

    #[test]
    fn malformed_xml_returns_single_error_at_origin() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = "<Project this is not valid xml";

        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));
        let diags = diagnostics_for(text, &project, &env);

        assert_eq!(diags.len(), 1, "{diags:#?}");
        let d = &diags[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.range, Range::default());
        assert!(
            d.message.starts_with("malformed XML:"),
            "unexpected message: {}",
            d.message
        );
    }

    #[test]
    fn missing_sdk_emits_warning() {
        // DOTNET_ROOT is set but no SDK is installed under it. The
        // resolver returns `NotFound`, surfacing as `SdkNotFound`.
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup><Compile Include="Library.fs" /></ItemGroup>
</Project>"#;

        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("empty-dotnet")));
        let diags = diagnostics_for(text, &project, &env);

        // At minimum one SdkNotFound warning; the exact count depends
        // on whether the parser surfaces additional cascaded diagnostics.
        let sdk_diags: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("SDK 'Microsoft.NET.Sdk' not found"))
            .collect();
        assert_eq!(
            sdk_diags.len(),
            1,
            "expected exactly one SdkNotFound diagnostic, all diags: {diags:#?}"
        );
        let d = sdk_diags[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::WARNING));
        // The span should cover the Project element's Sdk attribute,
        // i.e. start at the opening `<` (offset 0) of the root.
        assert_eq!(d.range.start.line, 0);
    }

    #[test]
    fn build_env_msbuild_sdks_path_declines_sdk_resolution() {
        // A build-environment `MSBuildSDKsPath` reroutes MSBuild's own SDK
        // resolution (probed: a non-existent value fails a `Microsoft.NET.Sdk`
        // project with MSB4236). We do not model the redirect, so rather than
        // resolve through the installed SDK as if it were absent — which would
        // import a chain the real build never uses — discovery declines, and
        // the buffer surfaces the decline instead of a clean parse.
        let tmp = TempDir::new().unwrap();
        let dotnet = tmp.path().join("dotnet");
        install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

        let project_dir = tmp.path().join("proj");
        fs::create_dir_all(&project_dir).unwrap();
        mark_restored(&project_dir);
        let project = project_dir.join("App.fsproj");
        let text = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup><Compile Include="Library.fs" /></ItemGroup>
</Project>"#;
        fs::write(&project, text).unwrap();

        let env = env_with(|e| {
            e.dotnet_root = Some(dotnet);
            e.build_environment = HashMap::from([(
                "MSBuildSDKsPath".to_string(),
                "/somewhere/else/Sdks".to_string(),
            )]);
        });
        let diags = diagnostics_for(text, &project, &env);

        let declined: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("resolution declined"))
            .collect();
        assert_eq!(
            declined.len(),
            1,
            "expected the SDK resolution to decline, all diags: {diags:#?}"
        );
        assert!(
            declined[0].message.contains("MSBuildSDKsPath"),
            "the decline should name the culprit, got: {}",
            declined[0].message
        );
    }

    #[test]
    fn import_failed_emits_warning_at_span() {
        // `parse_fsproj_with_imports` tries to follow `<Import>` and
        // produces ImportFailed when the file isn't there (UnresolvedImport
        // is reserved for the pure entry point that doesn't follow).
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = r#"<Project>
  <Import Project="./does-not-exist.props" />
</Project>"#;

        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));
        let diags = diagnostics_for(text, &project, &env);

        let import_diags: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("failed to follow import"))
            .collect();
        assert_eq!(
            import_diags.len(),
            1,
            "expected one ImportFailed, all diags: {diags:#?}"
        );
        let d = import_diags[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::WARNING));
        assert!(
            d.message.contains("file does not exist"),
            "expected NotFound reason, got: {}",
            d.message
        );
        // Span covers the <Import> element on line 1 (zero-indexed).
        assert_eq!(d.range.start.line, 1);
    }

    #[test]
    fn utf16_position_after_multibyte_text() {
        // The diagnostic lands on `$(TargetFramework)` (carved out of
        // exact undefined reads, so it always diagnoses), which sits after a
        // comment containing `À🦀`. The two non-ASCII chars are
        // 2 + 4 = 6 UTF-8 bytes but 1 + 2 = 3 UTF-16 units. If the
        // position translation conflated the two, the LSP column would
        // be inflated by 3.
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = "<Project>\n<!-- À🦀 -->\n<ItemGroup><Compile Include=\"$(TargetFramework).fs\" /></ItemGroup>\n</Project>";

        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));
        let diags = diagnostics_for(text, &project, &env);

        let prop_diags: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("$(TargetFramework)"))
            .collect();
        assert_eq!(
            prop_diags.len(),
            1,
            "expected one UndefinedProperty, all diags: {diags:#?}"
        );
        let d = prop_diags[0];
        // Line 2 is the ItemGroup line. The character column points at
        // the `<Compile` start. Critically, columns on line 1 (which
        // contains the multibyte text) would only be checked if the
        // diagnostic landed there; we deliberately put the diagnostic
        // on a later, ASCII-only line so the multibyte text only
        // affects byte offsets, not UTF-16 columns on the diag line.
        assert_eq!(d.range.start.line, 2);
        // The column on the diag line is pure ASCII, so byte and UTF-16
        // counts agree. The real test is that `offset_to_position`
        // walked past the multibyte text correctly to land on line 2;
        // an off-by-bytes implementation would land on line 1 or
        // overshoot.
        assert!(d.range.start.character < 100);
    }

    #[test]
    fn every_diagnostic_range_is_within_source() {
        // Property mirror of the lexer-diagnostics property check.
        // Each input is crafted to produce *some* fsproj diagnostic
        // (well-formed XML, but with constructs the parser can't fully
        // evaluate). For every diagnostic that comes out, both ends of
        // the range must sit inside the source's line grid.
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));

        let inputs: &[&str] = &[
            // UndefinedProperty
            "<Project><PropertyGroup><X>$(TargetFramework)</X></PropertyGroup></Project>",
            // UnsupportedConstruct
            "<Project><Choose><When Condition=\"true\"/></Choose></Project>",
            // ImportFailed
            "<Project>\n<Import Project=\"./nope.props\" />\n</Project>",
            // SdkNotFound (DOTNET_ROOT points nowhere useful)
            "<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup><Compile Include=\"X.fs\"/></ItemGroup></Project>",
            // Mixed multibyte
            "<Project>\n<!-- À🦀 -->\n<PropertyGroup><X>$(TargetFramework)</X></PropertyGroup>\n</Project>",
            // CR-only line terminator (lexer agrees, but verify here too)
            "<Project>\r<PropertyGroup><X>$(TargetFramework)</X></PropertyGroup>\r</Project>",
        ];

        // Track how many of the inputs produced at least one diagnostic;
        // a 0 count would mean the corpus stopped exercising the
        // property and the test would silently pass.
        let mut producing = 0usize;
        for &src in inputs {
            let diags = diagnostics_for(src, &project, &env);
            if !diags.is_empty() {
                producing += 1;
            }
            let lines = split_lsp_lines(src);
            let utf16_lines: Vec<u32> = lines
                .iter()
                .map(|l| l.encode_utf16().count() as u32)
                .collect();
            for d in diags {
                for p in [d.range.start, d.range.end] {
                    let line = p.line as usize;
                    assert!(line < utf16_lines.len(), "line {line} OOB in {src:?}");
                    assert!(
                        p.character <= utf16_lines[line],
                        "col {} > line len {} in {src:?}",
                        p.character,
                        utf16_lines[line]
                    );
                }
            }
        }
        assert!(
            producing >= inputs.len() - 1,
            "expected nearly every corpus input to produce diagnostics; \
             only {producing}/{} did. The corpus has drifted away from \
             exercising the property.",
            inputs.len()
        );
    }

    /// Like `str::lines`, but treats lone `\r` as a line terminator too,
    /// matching the lexer's newline regex and LSP's spec.
    fn split_lsp_lines(text: &str) -> Vec<&str> {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\r' => {
                    out.push(&text[start..i]);
                    i += if bytes.get(i + 1) == Some(&b'\n') {
                        2
                    } else {
                        1
                    };
                    start = i;
                }
                b'\n' => {
                    out.push(&text[start..i]);
                    i += 1;
                    start = i;
                }
                _ => i += 1,
            }
        }
        out.push(&text[start..]);
        out
    }

    #[test]
    fn noisy_sdk_files_do_not_leak_into_buffer_diagnostics() {
        // Regression for codex review (P2): real Microsoft.NET.Sdk
        // Sdk.props/Sdk.targets contain constructs the msbuild parser
        // reports as UnsupportedCondition/UnsupportedConstruct
        // (e.g. `Condition="Exists(...)"` and `<Target>`s). When the
        // LSP parses through `parse_fsproj_with_imports` against a real
        // SDK on disk, every such diagnostic gets its span remapped to
        // the import site in the user's buffer and looks like a buffer
        // problem. A clean fsproj should not emit any diagnostic.
        //
        // We fabricate a tiny "SDK" whose Sdk.props contains a
        // `<Target>` element and an `Exists(...)` condition — the
        // signatures of two of the noisiest real-SDK constructs.
        let tmp = TempDir::new().unwrap();
        let dotnet = tmp.path().join("dotnet");
        let sdk_root = dotnet
            .join("sdk")
            .join("8.0.401")
            .join("Sdks")
            .join("Microsoft.NET.Sdk")
            .join("Sdk");
        fs::create_dir_all(&sdk_root).unwrap();
        fs::write(
            sdk_root.join("Sdk.props"),
            r#"<Project>
  <PropertyGroup Condition="Exists('nonsense.props')">
    <UsingMicrosoftNETSdk>true</UsingMicrosoftNETSdk>
  </PropertyGroup>
  <Target Name="DoStuff" />
</Project>"#,
        )
        .unwrap();
        fs::write(sdk_root.join("Sdk.targets"), "<Project/>").unwrap();

        let project_dir = tmp.path().join("proj");
        fs::create_dir_all(&project_dir).unwrap();
        mark_restored(&project_dir);
        let project = project_dir.join("App.fsproj");
        let text = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <Compile Include="Program.fs" />
  </ItemGroup>
</Project>"#;
        fs::write(&project, text).unwrap();

        let env = env_with(|e| e.dotnet_root = Some(dotnet));
        let diags = diagnostics_for(text, &project, &env);
        assert!(
            diags.is_empty(),
            "Sdk.props internals must not surface as buffer diagnostics, got:\n{diags:#?}"
        );
    }

    #[test]
    fn common_configuration_condition_does_not_warn() {
        // Regression for codex review (round 3, P2): a clean fsproj
        // that conditions a PropertyGroup on $(Configuration) is the
        // most common shape in real .NET projects. With no seeded
        // defaults the condition evaluator emits
        // UndefinedProperty{name="Configuration"} and the LSP
        // republishes it as a warning on an otherwise clean buffer.
        // Seeding `Configuration=Debug` / `Platform=AnyCPU` (the same
        // defaults `dotnet build` uses with no `-c`/`-p:Platform`
        // flags) lets the condition evaluate cleanly.
        let tmp = TempDir::new().unwrap();
        let dotnet = tmp.path().join("dotnet");
        install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

        let project_dir = tmp.path().join("proj");
        fs::create_dir_all(&project_dir).unwrap();
        mark_restored(&project_dir);
        let project = project_dir.join("App.fsproj");
        let text = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup Condition="'$(Configuration)|$(Platform)' == 'Debug|AnyCPU'">
    <DebugSymbols>true</DebugSymbols>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="Program.fs" />
  </ItemGroup>
</Project>"#;
        fs::write(&project, text).unwrap();

        let env = env_with(|e| e.dotnet_root = Some(dotnet));
        let diags = diagnostics_for(text, &project, &env);
        assert!(
            diags.is_empty(),
            "expected no diagnostics for a clean Debug/AnyCPU project, got:\n{diags:#?}"
        );
    }

    #[test]
    fn item_and_metadata_reference_messages_are_not_double_wrapped() {
        // Regression for codex review (round 4, P3): the msbuild crate
        // stores the full expanded value (including its `@(...)` /
        // `%(...)` syntax) in the `reference` field. Earlier message
        // rendering wrapped that in another `@(...)` / `%(...)`,
        // producing nonsense like `@(@(Other))`.
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");

        // `@(Other)` in an Include — substituted-but-not-expanded,
        // surfacing as UnresolvedItemReference.
        let with_item_ref = r#"<Project>
  <ItemGroup>
    <Compile Include="@(Other).fs" />
  </ItemGroup>
</Project>"#;
        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));
        let diags = diagnostics_for(with_item_ref, &project, &env);
        let item_diag = diags
            .iter()
            .find(|d| d.message.contains("item reference"))
            .unwrap_or_else(|| panic!("expected item-reference diagnostic, got: {diags:#?}"));
        // The reference verbatim — single `@(` — appears in the
        // message, and we never see a double-wrap.
        assert!(
            item_diag.message.contains("@(Other)"),
            "message should mention `@(Other)` verbatim: {}",
            item_diag.message,
        );
        assert!(
            !item_diag.message.contains("@(@("),
            "message must not double-wrap: {}",
            item_diag.message,
        );

        // `%(Filename)` — UnresolvedMetadataReference. We put it
        // in a property value to avoid colliding with the item-ref
        // branch in the same Include expansion.
        let with_meta_ref = r#"<Project>
  <PropertyGroup>
    <BaseName>%(Filename)</BaseName>
  </PropertyGroup>
</Project>"#;
        let diags = diagnostics_for(with_meta_ref, &project, &env);
        let meta_diag = diags
            .iter()
            .find(|d| d.message.contains("metadata reference"))
            .unwrap_or_else(|| panic!("expected metadata-reference diagnostic, got: {diags:#?}"));
        assert!(
            meta_diag.message.contains("%(Filename)"),
            "message should mention `%(Filename)` verbatim: {}",
            meta_diag.message,
        );
        assert!(
            !meta_diag.message.contains("%(%("),
            "message must not double-wrap: {}",
            meta_diag.message,
        );
    }

    #[test]
    fn nested_import_failure_surfaces_even_though_origin_is_imported() {
        // Regression for codex review (round 5, P2): if an imported
        // file has its own broken `<Import>`, that ImportFailed is
        // generated while the walker is inside the first import, so
        // it's tagged `Imported`. A blanket suppression of
        // Imported-origin diagnostics would hide the broken chain.
        // Meta-import diagnostics describe the chain itself and
        // should always reach the user.
        let tmp = TempDir::new().unwrap();
        // A Directory.Build.props that itself tries to import a file
        // that doesn't exist. The walker splices Directory.Build.props
        // before the project body, descends into it, and tries to
        // follow `child.props` from there.
        fs::write(
            tmp.path().join("Directory.Build.props"),
            r#"<Project>
  <Import Project="./does-not-exist-nested.props" />
</Project>"#,
        )
        .unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = r#"<Project>
  <ItemGroup>
    <Compile Include="Program.fs" />
  </ItemGroup>
</Project>"#;
        fs::write(&project, text).unwrap();

        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));
        let diags = diagnostics_for(text, &project, &env);
        let nested_fail: Vec<_> = diags
            .iter()
            .filter(|d| {
                d.message.contains("failed to follow import")
                    && d.message.contains("does-not-exist-nested.props")
            })
            .collect();
        assert_eq!(
            nested_fail.len(),
            1,
            "expected the nested ImportFailed to survive filtering, all diags: {diags:#?}"
        );
    }

    #[test]
    fn discovery_failure_falls_back_to_no_resolver() {
        // No DOTNET_ROOT, no $PATH ⇒ SdkDiscovery::for_project fails
        // with MissingDotnetRoot. We should still parse the fsproj and
        // surface the SDK attribute as `UnsupportedConstruct` (because
        // we passed None as the resolver), not panic.
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup><Compile Include="X.fs"/></ItemGroup>
</Project>"#;

        let env = env_with(|_| {}); // no dotnet_root, no search_path
        let diags = diagnostics_for(text, &project, &env);

        // The Sdk attribute survives as an UnsupportedConstruct since
        // no resolver was wired in.
        let unsupported: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("unsupported MSBuild construct"))
            .collect();
        assert!(
            !unsupported.is_empty(),
            "expected at least one UnsupportedConstruct from the Sdk attribute, \
             got: {diags:#?}"
        );
    }

    // ----- Stage 3.2: <ProjectReference> diagnostics -----

    const TWO_LINE_REF: &str = "<Project>\n  <ItemGroup>\n    <ProjectReference Include=\"{REF}\" />\n  </ItemGroup>\n</Project>";

    /// A buffer with one `<ProjectReference Include="{r}">` on line 2.
    fn fsproj_referencing(r: &str) -> String {
        TWO_LINE_REF.replace("{REF}", r)
    }

    #[test]
    fn missing_project_reference_warns() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = fsproj_referencing("Lib/Lib.fsproj"); // not created
        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));

        let diags = diagnostics_for(&text, &project, &env);
        let refs: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("does not exist"))
            .collect();
        assert_eq!(refs.len(), 1, "{diags:#?}");
        assert_eq!(refs[0].severity, Some(DiagnosticSeverity::WARNING));
        // Anchored on the `<ProjectReference>` element (line 2, zero-indexed).
        assert_eq!(refs[0].range.start.line, 2);
        assert!(refs[0].message.contains("Lib"), "{}", refs[0].message);
    }

    #[test]
    fn missing_csproj_reference_warns() {
        // A missing .csproj is reported too — existence is checked at the
        // boundary even though we never recurse into C# (mirrors project_graph).
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = fsproj_referencing("Cs/Cs.csproj"); // not created
        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));

        let diags = diagnostics_for(&text, &project, &env);
        assert_eq!(
            diags
                .iter()
                .filter(|d| d.message.contains("does not exist"))
                .count(),
            1,
            "{diags:#?}"
        );
    }

    #[test]
    fn present_project_reference_no_warning() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let lib = tmp.path().join("Lib").join("Lib.fsproj");
        fs::create_dir_all(lib.parent().unwrap()).unwrap();
        fs::write(&lib, "<Project></Project>").unwrap();
        let text = fsproj_referencing("Lib/Lib.fsproj");
        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));

        let diags = diagnostics_for(&text, &project, &env);
        assert!(
            diags.iter().all(|d| {
                !d.message.contains("does not exist")
                    && !d.message.contains("unsupported project reference")
            }),
            "expected no reference diagnostics, got {diags:#?}"
        );
    }

    #[test]
    fn existing_csproj_reference_no_warning() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let cs = tmp.path().join("Cs").join("Cs.csproj");
        fs::create_dir_all(cs.parent().unwrap()).unwrap();
        fs::write(&cs, "<Project></Project>").unwrap();
        let text = fsproj_referencing("Cs/Cs.csproj");
        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));

        let diags = diagnostics_for(&text, &project, &env);
        assert!(
            diags.iter().all(|d| {
                !d.message.contains("does not exist")
                    && !d.message.contains("unsupported project reference")
            }),
            "expected no reference diagnostics for an existing csproj, got {diags:#?}"
        );
    }

    #[test]
    fn span_guard_matches_local_name_with_optional_prefix() {
        let accepts = |t: &str| span_points_at_project_reference(t, &(0..t.len()));
        // Plain and namespace-prefixed elements are both accepted (the msbuild
        // parser records items by local name).
        assert!(accepts("<ProjectReference Include=\"X\" />"));
        assert!(accepts("<msb:ProjectReference Include=\"X\" />"));
        assert!(accepts("<ProjectReference/>"));
        // A different element, or a look-alike whose local name only starts
        // with "ProjectReference", is rejected.
        assert!(!accepts("<Import Project=\"X\" />"));
        assert!(!accepts("<ProjectReferenceGroup />"));
        // An out-of-bounds / non-boundary span is rejected without panicking.
        assert!(!span_points_at_project_reference("ab", &(0..99)));
    }

    #[test]
    fn conditioned_out_reference_does_not_warn() {
        // Directory.Build.props sets UseLib=false; the buffer guards a (missing)
        // reference on that property. The real (with-imports) evaluation honors
        // the imported property and skips the reference, so we must not warn.
        // A buffer-only pure parse would treat $(UseLib) as empty, include the
        // reference, and emit a bogus "does not exist".
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("Directory.Build.props"),
            "<Project><PropertyGroup><UseLib>false</UseLib></PropertyGroup></Project>",
        )
        .unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = "<Project>\n  <ItemGroup Condition=\"'$(UseLib)' != 'false'\">\n    <ProjectReference Include=\"Gone/Gone.fsproj\" />\n  </ItemGroup>\n</Project>";
        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));

        let diags = diagnostics_for(text, &project, &env);
        assert!(
            diags.iter().all(|d| !d.message.contains("does not exist")),
            "conditioned-out reference must not warn, got {diags:#?}"
        );
    }

    #[test]
    fn prefixed_project_reference_is_diagnosed() {
        // A namespace-prefixed `<msb:ProjectReference>` is recorded by the
        // parser (by local name) and is a buffer-authored reference, so a
        // missing target must still warn — end-to-end check of the guard's
        // local-name matching.
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = "<Project xmlns:msb=\"http://schemas.microsoft.com/developer/msbuild/2003\">\n  <ItemGroup>\n    <msb:ProjectReference Include=\"Gone/Gone.fsproj\" />\n  </ItemGroup>\n</Project>";
        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));

        let diags = diagnostics_for(text, &project, &env);
        assert_eq!(
            diags
                .iter()
                .filter(|d| d.message.contains("does not exist"))
                .count(),
            1,
            "{diags:#?}"
        );
    }

    #[test]
    fn unsupported_project_reference_kind_warns() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = fsproj_referencing("Legacy/Legacy.vbproj");
        let env = env_with(|e| e.dotnet_root = Some(tmp.path().join("dotnet")));

        let diags = diagnostics_for(&text, &project, &env);
        let refs: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("unsupported project reference"))
            .collect();
        assert_eq!(refs.len(), 1, "{diags:#?}");
        assert_eq!(refs[0].severity, Some(DiagnosticSeverity::WARNING));
        assert!(refs[0].message.contains("vbproj"), "{}", refs[0].message);
    }

    // --- RestoreStale (Stage 3.2 Stage A) ----------------------------------

    #[test]
    fn restore_diagnostic_is_gated_and_file_level() {
        assert!(restore_diagnostic(false).is_none());

        let d = restore_diagnostic(true).expect("a warning when unrestored");
        assert_eq!(d.severity, Some(DiagnosticSeverity::WARNING));
        assert!(
            d.message.contains("obj/project.assets.json"),
            "{}",
            d.message
        );
        // Whole-file diagnostic, anchored at the head (module convention).
        assert_eq!(d.range, Range::default());
    }

    #[test]
    fn unrestored_sdk_project_warns_then_clears_when_restored() {
        let tmp = TempDir::new().unwrap();
        let dotnet = tmp.path().join("dotnet");
        install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

        let project_dir = tmp.path().join("proj");
        fs::create_dir_all(&project_dir).unwrap();
        // Deliberately *not* restored (no obj/project.assets.json).
        let project = project_dir.join("App.fsproj");
        let text = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <Compile Include="Library.fs" />
  </ItemGroup>
</Project>"#;
        fs::write(&project, text).unwrap();

        let env = env_with(|e| e.dotnet_root = Some(dotnet));
        let diags = diagnostics_for(text, &project, &env);
        let restore: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("obj/project.assets.json"))
            .collect();
        assert_eq!(restore.len(), 1, "expected one restore warning: {diags:#?}");
        assert_eq!(restore[0].severity, Some(DiagnosticSeverity::WARNING));

        // Restoring (writing obj/project.assets.json) clears the warning.
        mark_restored(&project_dir);
        let diags = diagnostics_for(text, &project, &env);
        assert!(
            diags
                .iter()
                .all(|d| !d.message.contains("obj/project.assets.json")),
            "{diags:#?}"
        );
    }

    #[test]
    fn bare_project_without_sdk_never_warns_restore_stale() {
        // A bare `<Project>` (no `Sdk`) has nothing to restore — no warning even
        // with no `obj/project.assets.json`. (resolved_sdk_root gates it out.)
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("App.fsproj");
        let text = "<Project><ItemGroup><Compile Include=\"A.fs\" /></ItemGroup></Project>";
        fs::write(&project, text).unwrap();

        let env = env_with(|_| {});
        let diags = diagnostics_for(text, &project, &env);
        assert!(
            diags
                .iter()
                .all(|d| !d.message.contains("obj/project.assets.json")),
            "{diags:#?}"
        );
    }

    /// The warning is aligned with the semantic layer's lookup: it reads
    /// `<project>/obj/project.assets.json`, so a project that relocates its
    /// assets is genuinely unresolvable by the LSP and *still warns* (restoring
    /// to a custom path doesn't help the LSP today). Pins the deliberate
    /// alignment so the two move together if custom paths are ever supported.
    #[test]
    fn custom_assets_path_still_warns_because_lsp_reads_obj() {
        let tmp = TempDir::new().unwrap();
        let dotnet = tmp.path().join("dotnet");
        install_sdk(&dotnet, "8.0.401", "Microsoft.NET.Sdk");

        let project_dir = tmp.path().join("proj");
        fs::create_dir_all(&project_dir).unwrap();
        let project = project_dir.join("App.fsproj");
        let text = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <BaseIntermediateOutputPath>custom_obj/</BaseIntermediateOutputPath>
  </PropertyGroup>
</Project>"#;
        fs::write(&project, text).unwrap();
        // "Restored" to the custom path — but the LSP reads obj/, which is empty.
        let custom = project_dir.join("custom_obj");
        fs::create_dir_all(&custom).unwrap();
        fs::write(custom.join("project.assets.json"), "{}").unwrap();

        let env = env_with(|e| e.dotnet_root = Some(dotnet));
        let diags = diagnostics_for(text, &project, &env);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("obj/project.assets.json")),
            "a custom-assets-path project still warns (LSP reads obj/): {diags:#?}"
        );
    }

    // --- ReferenceCycle (Stage 3.2 Stage B) --------------------------------

    use crate::project_graph::{Edge, EdgeKind, NodeTfm, ProjectNode};
    use std::path::PathBuf;

    /// The `<ProjectReference Include="…">` element's byte span in `text`.
    fn project_ref_span(text: &str) -> std::ops::Range<usize> {
        let start = text.find("<ProjectReference").unwrap();
        let end = text[start..].find("/>").unwrap() + start + 2;
        start..end
    }

    fn pb(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// A graph node for `path` with edges to `targets` — dummy spans (`0..0`),
    /// adequate for the dependency nodes whose only role is reachability.
    fn dep(path: &str, targets: &[&str]) -> ProjectNode {
        ProjectNode {
            path: pb(path),
            kind: ProjectKind::FSharp,
            tfm: NodeTfm::NotEvaluated,
            output_name: None,
            references: targets
                .iter()
                .map(|t| Edge {
                    target: pb(t),
                    span: 0..0,
                    kind: EdgeKind::Full,
                })
                .collect(),
        }
    }

    /// A `ProjectGraph` whose `problems` are irrelevant to `graph_diagnostics`
    /// (it uses node adjacency), so they're left empty.
    fn graph_of(nodes: Vec<ProjectNode>) -> ProjectGraph {
        ProjectGraph {
            nodes,
            problems: Vec::new(),
        }
    }

    #[test]
    fn cycle_through_entry_anchors_on_its_own_reference() {
        // A → B → A. (`/p/A.fsproj` is the entry.)
        let text = "<Project>\n  <ItemGroup>\n    <ProjectReference Include=\"B.fsproj\" />\n  </ItemGroup>\n</Project>";
        let entry = Path::new("/p/A.fsproj");
        let span = project_ref_span(text);
        let graph = graph_of(vec![
            ProjectNode {
                path: lexically_normalize(entry),
                kind: ProjectKind::FSharp,
                tfm: NodeTfm::NotEvaluated,
                output_name: None,
                references: vec![Edge {
                    target: pb("/p/B.fsproj"),
                    span: span.clone(),
                    kind: EdgeKind::Full,
                }],
            },
            dep("/p/B.fsproj", &["/p/A.fsproj"]),
        ]);

        let diags = graph_diagnostics(text, entry, &graph);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
        assert!(
            diags[0]
                .message
                .contains("project reference cycle: A.fsproj → B.fsproj → … → A.fsproj"),
            "{}",
            diags[0].message
        );
        assert_eq!(diags[0].range.start, offset_to_position(text, span.start));
        assert_eq!(diags[0].range.end, offset_to_position(text, span.end));
    }

    #[test]
    fn cycle_not_through_entry_is_not_reported() {
        // A → B, and B → C → B (a cycle the entry A is not part of).
        let text =
            "<Project><ItemGroup><ProjectReference Include=\"B.fsproj\" /></ItemGroup></Project>";
        let entry = Path::new("/p/A.fsproj");
        let graph = graph_of(vec![
            ProjectNode {
                path: lexically_normalize(entry),
                kind: ProjectKind::FSharp,
                tfm: NodeTfm::NotEvaluated,
                output_name: None,
                references: vec![Edge {
                    target: pb("/p/B.fsproj"),
                    span: project_ref_span(text),
                    kind: EdgeKind::Full,
                }],
            },
            dep("/p/B.fsproj", &["/p/C.fsproj"]),
            dep("/p/C.fsproj", &["/p/B.fsproj"]),
        ]);
        assert!(graph_diagnostics(text, entry, &graph).is_empty());
    }

    #[test]
    fn self_reference_cycle_anchors_on_its_self_edge() {
        let text =
            "<Project><ItemGroup><ProjectReference Include=\"A.fsproj\" /></ItemGroup></Project>";
        let entry = Path::new("/p/A.fsproj");
        let span = project_ref_span(text);
        let graph = graph_of(vec![ProjectNode {
            path: lexically_normalize(entry),
            kind: ProjectKind::FSharp,
            tfm: NodeTfm::NotEvaluated,
            output_name: None,
            references: vec![Edge {
                target: pb("/p/A.fsproj"),
                span: span.clone(),
                kind: EdgeKind::Full,
            }],
        }]);
        let diags = graph_diagnostics(text, entry, &graph);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(diags[0].range.start, offset_to_position(text, span.start));
    }

    /// The reviewer's case: the entry imports X (unanchorable) *and* locally
    /// references B; X → C, B → C, C → A. The local A → B edge participates in a
    /// cycle (B reaches A), so it must be warned even though the imported A → X
    /// edge discovers the cycle first.
    #[test]
    fn imported_path_does_not_hide_an_anchorable_local_cycle() {
        let text =
            "<Project><ItemGroup><ProjectReference Include=\"B.fsproj\" /></ItemGroup></Project>";
        let entry = Path::new("/p/A.fsproj");
        let local_span = project_ref_span(text);
        let graph = graph_of(vec![
            ProjectNode {
                path: lexically_normalize(entry),
                kind: ProjectKind::FSharp,
                tfm: NodeTfm::NotEvaluated,
                output_name: None,
                references: vec![
                    Edge {
                        target: pb("/p/X.fsproj"),
                        span: 0..1, // imported edge: not a buffer <ProjectReference>
                        kind: EdgeKind::Full,
                    },
                    Edge {
                        target: pb("/p/B.fsproj"),
                        span: local_span.clone(),
                        kind: EdgeKind::Full,
                    },
                ],
            },
            dep("/p/X.fsproj", &["/p/C.fsproj"]),
            dep("/p/B.fsproj", &["/p/C.fsproj"]),
            dep("/p/C.fsproj", &["/p/A.fsproj"]),
        ]);
        let diags = graph_diagnostics(text, entry, &graph);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        // Anchored on the local `<ProjectReference>` to B, not the imported X edge.
        assert_eq!(
            diags[0].range.start,
            offset_to_position(text, local_span.start)
        );
    }

    #[test]
    fn span_not_matching_buffer_is_skipped() {
        // A graph (disk) edge whose span no longer points at a `<ProjectReference>`
        // in this buffer (e.g. unsaved edit) degrades to no diagnostic.
        let text = "<Project></Project>";
        let entry = Path::new("/p/A.fsproj");
        let graph = graph_of(vec![
            ProjectNode {
                path: lexically_normalize(entry),
                kind: ProjectKind::FSharp,
                tfm: NodeTfm::NotEvaluated,
                output_name: None,
                references: vec![Edge {
                    target: pb("/p/B.fsproj"),
                    span: 2..9,
                    kind: EdgeKind::Full,
                }],
            },
            dep("/p/B.fsproj", &["/p/A.fsproj"]),
        ]);
        assert!(graph_diagnostics(text, entry, &graph).is_empty());
    }
}

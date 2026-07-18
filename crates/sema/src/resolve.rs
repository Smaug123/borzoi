//! Intra-file name resolution: build a position-ordered scope tree from a
//! parsed file and resolve every name *use* the current parser subset can
//! express to the binder it refers to.
//!
//! The entry point is [`resolve_file`]. It is pure (values in, values out — no
//! IO, no mutable context threaded through every function), per
//! `docs/type-checker-plan.md` D2: the caller decides where the `preceding`
//! project items and `assemblies` environment come from. `preceding` resolves
//! qualified references to earlier Compile-order files; `assemblies` resolves
//! fully-qualified paths into referenced assemblies. Both are empty for a
//! lone single file with no references.
//!
//! ## Soundness (D5)
//!
//! A use we cannot resolve in any scope we *do* model becomes
//! [`Resolution::Deferred`], never [`Resolution::Unresolved`]: we lack the
//! import / assembly environment, so a name we don't bind locally may still be
//! a perfectly valid reference into FSharp.Core or a referenced `.dll`. Saying
//! nothing (`Deferred`) keeps the layer honest — a wrong "undefined name" is
//! worse than no answer. [`Resolution::Unresolved`] is reserved for the
//! diagnostics phase (Phase 4) and is never produced here.
//!
//! ## Provisional pattern heads
//!
//! A nullary single-segment `LongIdent` pattern head — `None` in `match x with
//! None -> …`, `let (x, None) = …`, `fun None -> …` — is constructor-shaped (the
//! parser routes lower-case idents to `Named`, so these are always upper-case)
//! and [`binders`](crate::binders) flags it [`provisional`](crate::Def::provisional).
//! Whether it is a real binder or a constructor reference depends on whether the
//! name resolves to a nullary constructor / literal: FCS binds an upper-case
//! *variable* like `X` in `let f X = X` (with warning FS0049) but treats `None`
//! as the `FSharp.Core` constructor.
//!
//! An in-file **union case** in scope *is* now resolved: a provisional head is
//! looked up via [`Resolver::case_reference`], and if it names a
//! [`DefKind`](crate::DefKind)`::UnionCase` the head records that resolution (a
//! case reference, not a binder) — so `Red` in `match c with Red -> …`, given an
//! in-file `type Color = Red | Green`, points at the case. Anything else still
//! declines to bind and records nothing, so the name falls through to
//! `Deferred`: a constructor from a referenced assembly / `FSharp.Core` (`None`,
//! `Some` — telling those apart needs the import / assembly environment a later
//! slice will consult), or a genuine upper-case *variable* pattern (rare,
//! FS0049). Binding those unconditionally would point the result `None` in
//! `match x with None -> None` at a fabricated binder (a wrong go-to-definition);
//! per correctness-over-availability we decline. The dropped non-case heads are
//! a coverage gap, never a wrong answer.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use borzoi_cst::syntax::{
    ActivePatName, AstNode, AttributeList, ExceptionDefnDecl, ImplFile, LongIdent, ModuleDecl,
    ModuleOrNamespace, ModuleOrNamespaceKind, NestedModuleDecl, Pat, SigDecl, SigFile, SyntaxNode,
    SyntaxToken, Type, TypeDefn, TypeDefnRepr,
};
use rowan::TextRange;

use crate::assembly_env::{AssemblyEnv, EntityHandle};
use crate::def::{Def, DefId};
use crate::qnof::QualifiedNameOfFile;

mod assembly;
mod bindings;
mod decls;
mod exprs;
mod lookup;
mod model;
mod state;
mod types;

pub use model::{
    CaseKind, DeferredReason, ExportedItem, ExportedItems, ItemId, OpenOpacity, OpenTrace,
    ProjectItems, Resolution, ResolutionTrace, ResolvedFile, ResolvedProject,
};
pub use state::ActivePatternShape;
use state::{Resolver, implicit_open_groups, implicit_open_namespaces};

/// Resolve every name use in `file` to its defining binder — within this file,
/// through `preceding` to module-qualified bindings of earlier Compile-order
/// files, and through `assemblies` to fully-qualified types/members of
/// referenced assemblies.
///
/// Pure. `preceding` carries the exports of earlier files (empty for a
/// single-file caller; see [`resolve_project`] for the fold). `assemblies` is
/// the name index over referenced assemblies (empty `AssemblyEnv::default()`
/// when there are none).
pub fn resolve_file(
    file: &ImplFile,
    preceding: &ProjectItems,
    assemblies: &AssemblyEnv,
) -> ResolvedFile {
    // Coarse phase spans (otel only) so a slow `resolve_file` on a large file
    // splits into resolver setup / the declaration walk / finish. Each guard is
    // dropped at its boundary so the phases sequence rather than nest; the whole
    // scaffolding is `#[cfg]`'d out (and `tracing`-free) in the default build.
    #[cfg(feature = "otel")]
    let _phase = tracing::info_span!("resolver_new").entered();
    let mut r = Resolver::new(preceding, assemblies);
    #[cfg(feature = "otel")]
    drop(_phase);
    #[cfg(feature = "otel")]
    let _phase = tracing::info_span!("resolve_walk").entered();
    // EX-3 §2(d): pre-scan every type definition's simple name for the
    // attribute resolution's project-type guard. Whole-file and up front (not
    // accumulated during the walk) so it is order-independent — a `[<Foo>]`
    // candidate must defer whether the potentially-aliasing `type
    // FooAttribute` is declared before or after it, or in a sibling block.
    // Over-approximate (an augmentation target's last segment counts too),
    // which is sound: a spurious match only adds a defer.
    for defn in file.syntax().descendants().filter_map(TypeDefn::cast) {
        if let Some(name) = defn.long_id().and_then(|li| li.idents().last()) {
            r.own_type_simple_names
                .insert(id_text(name.text()).to_string());
            // A generic declaration of the name disqualifies an in-file
            // attribute commit: FCS's attribute lookup is arity-0
            // (`DefiniteEmpty`), so it skips a generic local and binds an
            // assembly arity-0 type instead, while `lookup_type_def` is
            // arity-agnostic and would hand the generic local back
            // (codex round 7).
            if defn.typar_decls().is_some() {
                r.own_generic_type_simple_names
                    .insert(id_text(name.text()).to_string());
            }
            // An in-file ABBREVIATION could alias `ExtensionAttribute` (the
            // resolver records the local without chasing its target), so the
            // gate's derivation must know which local names are abbreviations
            // — a committed `Local` of one is a possible extension marker,
            // where a concrete in-file class is its own tycon and provably
            // not (EX-3 §2(d) stage 5).
            if matches!(defn.repr(), Some(TypeDefnRepr::Abbrev(_))) {
                r.own_abbrev_type_simple_names
                    .insert(id_text(name.text()).to_string());
            }
        }
    }
    // An `exception E` occupies F#'s *type* namespace too (FCS resolves
    // `[<E>]` to the exception, then errors on its constructor), so exception
    // names join the same guard (codex round 6) — and their own set: an
    // exception is never in `type_defs`, so an in-file *type* hit for a name
    // an exception also declares may be resolving past the closer exception
    // FCS would bind (codex on stage 4).
    for exn in file
        .syntax()
        .descendants()
        .filter_map(ExceptionDefnDecl::cast)
    {
        if let Some(name) = exn.union_case().and_then(|c| c.ident()) {
            r.own_type_simple_names
                .insert(id_text(name.text()).to_string());
            r.own_exception_simple_names
                .insert(id_text(name.text()).to_string());
        }
    }
    // The type/exception names declared **directly inside an `[<AutoOpen>]`
    // module** (AO-2): an in-file attribute hit for one of these must defer —
    // the auto-open import contests it positionally in FCS, across blocks the
    // block-scoped shadow set cannot see. Direct children only: a nested
    // plain module's types are not bare-visible through the auto-open, and a
    // nested auto-open module is itself a `NestedModuleDecl` this descendants
    // walk visits. Like every pre-scan above, file-global and
    // order-independent by design.
    for nm in file
        .syntax()
        .descendants()
        .filter_map(NestedModuleDecl::cast)
    {
        if !attrs_auto_open(nm.attributes()) {
            continue;
        }
        for decl in nm.decls() {
            match decl {
                ModuleDecl::Types(types) => {
                    for defn in types.defns() {
                        if let Some(name) = defn.long_id().and_then(|li| li.idents().last()) {
                            r.own_auto_open_type_names
                                .insert(id_text(name.text()).to_string());
                        }
                    }
                }
                ModuleDecl::Exception(exn) => {
                    if let Some(name) = exn.union_case().and_then(|c| c.ident()) {
                        r.own_auto_open_type_names
                            .insert(id_text(name.text()).to_string());
                    }
                }
                _ => {}
            }
        }
    }
    // The top-level value scope is *per container*, not one shared base frame: F#
    // merges same-named `namespace N` blocks and isolates distinct ones, and the
    // value-namespace entries that *can* sit at namespace level — union cases (and
    // later `exception` constructors) — must respect that. Each top-level block
    // activates its container's frame from [`Resolver::top_level`] (pushing it on
    // the scope stack) for the span of the block and stores it back after, so a
    // later same-named block re-takes the accumulated, position-ordered frame
    // while a distinct namespace gets a fresh one. A *nested* `module M = …`
    // pushes its own frame on top ([`Resolver::nested_module`]), so it sees the
    // enclosing container's bindings without leaking into it. The per-segment
    // qualification (`module *path*` for exports, `container_path`) is set per
    // module below and in `nested_module`.
    for module in file.modules() {
        // Imports are scoped to a *single* top-level block: an `open` in one
        // `namespace`/`module` block does not carry to the next (FCS-verified,
        // even for two same-named `namespace N` blocks), so reset the open state
        // to the implicit auto-opens at each block — mirroring the per-nested-
        // module save/restore in [`Resolver::nested_module`].
        r.imports = implicit_open_groups(r.assemblies);
        r.open_shortening_prefixes = implicit_open_namespaces(r.assemblies);
        // Block-scoped like every other open state: an incomplete module opened in one
        // top-level block must not veto a sibling block's opens (review round 11).
        r.incomplete_open_prefixes = Vec::new();
        r.explicit_open_prefixes = Vec::new();
        r.module_open_prefixes = Vec::new();
        r.assembly_open_prefixes = Vec::new();
        r.open_generation = 0;
        r.latest_open_pos = 0;
        r.pattern_suppressed_case_ids = HashSet::new();
        r.unmodelled_open_active = false;
        r.opaque_value_open = false;
        r.opaque_dotted_open = false;
        r.recursive_module_active = module.is_rec();
        r.auto_open_type_shadow_names.clear();
        r.rec_module_names.clear();
        if module.is_rec() {
            collect_nested_module_names(module.decls(), &mut r.rec_module_names);
        }
        // An anonymous top-level module roots its nested modules under the
        // implicit filename module (unmodeled), so they are not bare-cross-file
        // exportable; a `namespace`/`module` header (including `namespace
        // global`) is a real root. See [`Resolver::anonymous_root`].
        r.anonymous_root = matches!(module.kind(), ModuleOrNamespaceKind::Anon);
        let prefix = module_prefix(&module);
        if let Some(path) = &prefix {
            // A declared module shadows a same-named assembly type even with no
            // exports, so record it here, not from the exported values.
            r.module_paths.push(path.clone());
            let header_auto_open = attrs_auto_open(module.attributes());
            let header_private = decls::header_is_private(module.syntax());
            if header_auto_open && !r.anonymous_root {
                r.record_auto_open_module(path.clone(), header_private);
            }
            // The export-decl-list twin of the two lines above: one top-level
            // header decl carrying its `[<AutoOpen>]`/`private` bits, so
            // `module_headers` and `auto_open_module_paths` both derive (plan
            // Stage 2). A `NamedModule` is never the anonymous root, so this is
            // unconditionally exported.
            let header_pos = module_header_pos(&module);
            r.push_export_decl(
                path.clone(),
                header_pos,
                model::ExportDeclKind::Module {
                    header: true,
                    auto_open: header_auto_open,
                    private: header_private,
                },
            );
        }
        r.module_path = prefix;
        // The container prefix under which this header's nested modules are
        // qualified for *cross-file* reference — the header's `longId` for both
        // a `module` and a `namespace` (empty for an anonymous module /
        // `namespace global`). Unlike `module_path` it is set for namespaces.
        r.container_path = module
            .long_id()
            .map(|li| li.idents().map(|t| id_text(t.text()).to_string()).collect())
            .unwrap_or_default();
        // Record the project namespaces this header declares — so a later (or
        // same-project) file's `open <namespace>` resolves relatively
        // ([`open_interpretations`](Resolver::open_interpretations)),
        // including the chained form `open Inner; open Deep`. F# namespaces are
        // hierarchical, so each ancestor prefix counts too:
        // - a `namespace Outer.Inner.Deep` header makes `Outer`, `Outer.Inner`, and
        //   `Outer.Inner.Deep` namespaces (`1..=len`);
        // - a *dotted top-level module* `module Outer.Inner.Helpers` makes the
        //   segments *before* the final module name — `Outer`, `Outer.Inner` —
        //   namespaces (`1..len`); the final segment is the module itself.
        let ns_upto = match module.kind() {
            ModuleOrNamespaceKind::DeclaredNamespace => r.container_path.len(),
            ModuleOrNamespaceKind::NamedModule => r.container_path.len().saturating_sub(1),
            ModuleOrNamespaceKind::Anon | ModuleOrNamespaceKind::GlobalNamespace => 0,
        };
        for k in 1..=ns_upto {
            let ns = r.container_path[..k].to_vec();
            r.namespace_paths.push(ns.clone());
            // The export-decl-list twin: one `Namespace` decl per ancestor prefix,
            // reproducing the `ns_upto` bound verbatim (plan pitfall 4).
            r.push_export_decl(
                ns,
                module_header_pos(&module),
                model::ExportDeclKind::Namespace,
            );
        }
        // The enclosing-namespace depth of this block — the leading segments of
        // `container_path` that are namespaces. Constant as the walk descends into
        // nested modules (a nested module appends to `container_path` but not to the
        // namespace), so set once here at the top-level header.
        r.namespace_depth = ns_upto;
        // A `module private Foo` root scopes its contents to the module's parent
        // (see [`Resolver::access_floor`]); a namespace / anonymous root is always
        // public. The parent is `container_path` minus the module's own name.
        r.access_floor = (matches!(module.kind(), ModuleOrNamespaceKind::NamedModule)
            && decls::header_is_private(module.syntax()))
        .then(|| r.container_path.len().saturating_sub(1));
        // Activate this container's value frame for the block — merging same-named
        // `namespace` blocks (re-take the accumulated frame) and isolating distinct
        // ones (a fresh frame) — then store it back after. The nested-module
        // *shadow* set is container-scoped the same way: a distinct block must not
        // inherit a sibling's `module Sub` (it would veto a valid assembly
        // `Sub.…` there — FCS resolves it), while a same-named block must (its
        // `Sub.…` really is the project's — FCS).
        r.nested_module_locals = r
            .top_level_nested_locals
            .remove(&r.container_path)
            .unwrap_or_default();
        let frame = r.top_level.remove(&r.container_path).unwrap_or_default();
        r.scopes.push(frame);
        // The block frame is now active: seed the implicit auto-opens'
        // `[<AutoOpen>]` modules (FSharp.Core's `Operators` /
        // `ExtraTopLevelOperators`) as source-ordered `opened` entries so bare
        // `printfn` / `id` / operators resolve. Like every open these are
        // `from_open` and dropped by the per-block leak guard below; nested
        // modules see them through the live scope stack.
        for ns in implicit_open_namespaces(r.assemblies) {
            // Implicit auto-opens precede every declaration: slot position 0.
            r.open_auto_open_modules_in(&ns, 0, true);
        }
        // EX-3 §2(d): the block header's own attributes (`[<AutoOpen>] module
        // Test`) resolve in the block's opening scope — the implicit auto-opens
        // just seeded, no explicit `open` yet — which is FCS's env for them.
        r.resolve_attribute_lists(module.attributes());
        for decl in module.decls() {
            r.module_decl(&decl);
        }
        let mut frame = r
            .scopes
            .pop()
            .expect("container value frame is on the stack");
        // Opens are scoped to this one block: drop the entries an `open` brought
        // in ([`ScopeEntry::from_open`]) before storing the container frame back,
        // so a later same-named `namespace N` block re-takes only the value/case
        // *bindings* and not this block's imports (FCS-verified — opens do not
        // leak across same-named blocks, but namespace values merge). A *nested*
        // module's frame is popped and discarded, so its opened entries vanish
        // with no filter needed.
        frame.entries.retain(|e| !e.from_open);
        // The surviving bindings re-enter a later same-named block at that
        // block's generation zero: their stamps are THIS block's history, and
        // carrying them over would let a fresh block's counter (reset to 0)
        // misread them as stale. Semantically they precede everything the next
        // block declares, which is exactly generation 0.
        for e in &mut frame.entries {
            e.generation = 0;
        }
        r.top_level.insert(r.container_path.clone(), frame);
        let locals = std::mem::take(&mut r.nested_module_locals);
        r.top_level_nested_locals
            .insert(r.container_path.clone(), locals);
    }
    #[cfg(feature = "otel")]
    drop(_phase);
    #[cfg(feature = "otel")]
    let _phase = tracing::info_span!("resolve_finish").entered();
    r.finish()
}

/// One Compile item as the project fold consumes it: an implementation file
/// (`.fs`) or a **signature file** (`.fsi`), in Compile order
/// (`docs/fsi-signature-restriction-plan.md`). msbuild and FCS both keep a
/// signature in the Compile list immediately before (not necessarily
/// adjacent to) the implementation it constrains; the fold pairs them by
/// [`QualifiedNameOfFile`].
#[derive(Debug, Clone)]
pub enum SourceFile {
    Impl(ImplFile),
    Sig(SigFile),
}

impl SourceFile {
    pub fn as_impl(&self) -> Option<&ImplFile> {
        match self {
            SourceFile::Impl(f) => Some(f),
            SourceFile::Sig(_) => None,
        }
    }

    pub fn as_sig(&self) -> Option<&SigFile> {
        match self {
            SourceFile::Sig(f) => Some(f),
            SourceFile::Impl(_) => None,
        }
    }

    /// The file's root syntax node (rowan node equality is *identity*, so this
    /// is the incremental fold's reuse currency).
    pub fn syntax(&self) -> &SyntaxNode {
        match self {
            SourceFile::Impl(f) => f.syntax(),
            SourceFile::Sig(f) => f.syntax(),
        }
    }

    /// The dotted path of the file's single top-level `module` header, when
    /// the file is **module-headed** — exactly one fragment, of kind
    /// [`ModuleOrNamespaceKind::NamedModule`]. The AST-derived half of FCS's
    /// `QualifiedNameOfFile` (`QualFileNameOfImpls` / `QualFileNameOfSpecs`:
    /// a singleton module fragment names the file; an anonymous, namespace-
    /// headed, or multi-fragment file falls back to the filename). A leading
    /// `global` segment is stripped, as FCS's post-parse does.
    pub(crate) fn module_header_path(&self) -> Option<Vec<String>> {
        let fragments: Vec<ModuleOrNamespace> = match self {
            SourceFile::Impl(f) => f.modules().collect(),
            SourceFile::Sig(f) => f.modules().collect(),
        };
        match &fragments[..] {
            [only] if only.kind() == ModuleOrNamespaceKind::NamedModule => {
                header_long_id_path(only)
            }
            _ => None,
        }
    }

    /// Whether the file is **headerless** — its whole body lives in the
    /// implicit anonymous module (exactly one fragment, of kind
    /// [`ModuleOrNamespaceKind::Anon`]).
    fn is_headerless(&self) -> bool {
        let fragments: Vec<ModuleOrNamespace> = match self {
            SourceFile::Impl(f) => f.modules().collect(),
            SourceFile::Sig(f) => f.modules().collect(),
        };
        matches!(&fragments[..], [only] if only.kind() == ModuleOrNamespaceKind::Anon)
    }
}

/// The implicit anonymous-module path a headerless file's contents live
/// under: the QNOF's undeduplicated text, dots splitting into segments
/// (FCS's `ComputeAnonModuleName`). Empty for an impl-only wrapper's
/// placeholder QNOF.
fn implicit_module_path(qnof: &QualifiedNameOfFile) -> Vec<String> {
    qnof.undeduplicated_text()
        .split('.')
        .filter(|seg| !seg.is_empty())
        .map(str::to_string)
        .collect()
}

impl From<ImplFile> for SourceFile {
    fn from(file: ImplFile) -> Self {
        SourceFile::Impl(file)
    }
}

impl From<SigFile> for SourceFile {
    fn from(file: SigFile) -> Self {
        SourceFile::Sig(file)
    }
}

/// The dotted `idText` path of a top-level header's `LongIdent`, with a
/// leading `global` **keyword** segment stripped (FCS's post-parse drops the
/// mangled global-namespace head). The check is on the *raw* token spelling:
/// `global` is a keyword, so an ordinary module cannot be named it without
/// backticks, and an escaped `` ``global`` `` head is a genuine identifier
/// that must survive (codex round 4 — `id_text` conflates the two). `None`
/// for a header with no (or an empty) name.
fn header_long_id_path(fragment: &ModuleOrNamespace) -> Option<Vec<String>> {
    let li = fragment.long_id()?;
    let idents: Vec<SyntaxToken> = li.idents().collect();
    let strip_global = idents.len() > 1 && idents[0].text() == "global";
    let segments: Vec<String> = idents
        .iter()
        .skip(usize::from(strip_global))
        .map(|t| id_text(t.text()).to_string())
        .collect();
    (!segments.is_empty()).then_some(segments)
}

/// One Compile item plus its [`QualifiedNameOfFile`] — the input row of
/// [`resolve_project_files`]. The QNOF participates only in sig ↔ impl
/// pairing; compute it with [`crate::qnof::qualified_names`] (the fold cannot
/// derive it itself — the filename-derived case needs the file's path, which
/// only the caller holds).
#[derive(Debug, Clone)]
pub struct ProjectFile {
    pub file: SourceFile,
    pub qnof: QualifiedNameOfFile,
}

impl ProjectFile {
    pub fn new(file: SourceFile, qnof: QualifiedNameOfFile) -> Self {
        ProjectFile { file, qnof }
    }

    /// Wrap an implementation file for an impl-only fold. Pairing starts at a
    /// signature, so with no `.fsi` in the input the QNOF is never consulted
    /// and a placeholder suffices.
    fn impl_only(file: ImplFile) -> Self {
        ProjectFile {
            file: SourceFile::Impl(file),
            qnof: QualifiedNameOfFile::placeholder(),
        }
    }
}

/// Wrap an impl-only Compile list for the general fold (rowan clones are
/// reference-counted handles, so this is cheap).
fn impl_only_files(files: &[ImplFile]) -> Vec<ProjectFile> {
    files.iter().cloned().map(ProjectFile::impl_only).collect()
}

/// For each Compile index, the index of its pairing **partner**: for an
/// implementation, the signature that constrains it; for a signature, the
/// implementation that consumes it. A signature pairs with the first
/// following implementation of equal [`QualifiedNameOfFile`] (probe X3;
/// FCS's `tcsRootSigs` consumed by the next same-QNOF impl). A later
/// same-QNOF signature replaces an unconsumed earlier one, and a second
/// same-QNOF implementation pairs with nothing — both are FCS *errors*
/// (duplicate signature / implementation); sema is lenient and keeps the
/// deterministic reading. An **unpaired** signature constrains nothing: its
/// screen is not published (FCS-probed: an unpaired `module M` sig leaves a
/// same-QNOF-deduplicated impl's members resolving to the impl), and its
/// partner stays `None`.
fn pairing_partners(files: &[ProjectFile]) -> Vec<Option<usize>> {
    let mut pending: HashMap<&str, usize> = HashMap::new();
    let mut partner = vec![None; files.len()];
    for (i, pf) in files.iter().enumerate() {
        match &pf.file {
            SourceFile::Sig(_) => {
                pending.insert(pf.qnof.text(), i);
            }
            SourceFile::Impl(_) => {
                if let Some(sig) = pending.remove(pf.qnof.text()) {
                    partner[i] = Some(sig);
                    partner[sig] = Some(i);
                }
            }
        }
    }
    partner
}

/// A `.fsi` file's Stage-1 contribution — see
/// [`SigScreen`](model::SigScreen): the module roots it constrains (top-level
/// `module` headers; modules directly under a `namespace` fragment), the
/// signature's `[<AutoOpen>]` verdicts (authoritative — conclusion 6), and
/// the over-approximated name set (every non-trivia token's `idText` plus its
/// ident-shaped pieces, so a name inside a composite token — an
/// active-pattern `(|Even|Odd|)` — is covered however the lexer tokenises
/// it). Over-approximation only ever defers.
fn signature_screen(sig: &SigFile, qnof: &QualifiedNameOfFile) -> Arc<model::SigScreen> {
    let mut roots = Vec::new();
    let mut auto_open_nested = Vec::new();
    let mut value_paths = Vec::new();
    for fragment in sig.modules() {
        match fragment.kind() {
            ModuleOrNamespaceKind::NamedModule => {
                if let Some(path) = header_long_id_path(&fragment) {
                    roots.push(model::SigRoot {
                        path,
                        auto_open: attrs_auto_open(fragment.attributes()),
                    });
                }
            }
            ModuleOrNamespaceKind::DeclaredNamespace | ModuleOrNamespaceKind::GlobalNamespace => {
                let ns: Vec<String> = fragment
                    .long_id()
                    .map(|li| li.idents().map(|t| id_text(t.text()).to_string()).collect())
                    .unwrap_or_default();
                for decl in fragment.sig_decls() {
                    match decl {
                        SigDecl::NestedModule(nm) => {
                            let Some(li) = nm.long_id() else { continue };
                            let mut path = ns.clone();
                            path.extend(li.idents().map(|t| id_text(t.text()).to_string()));
                            let auto_open = attrs_auto_open(nm.attributes());
                            if auto_open {
                                auto_open_nested.push(path.clone());
                            }
                            roots.push(model::SigRoot { path, auto_open });
                        }
                        // Value-namespace members declared *directly under
                        // the namespace* — union/enum case names and
                        // exception constructors (a `val` cannot sit there).
                        // These live outside every module root, so they get
                        // their own exposed value paths (RQA-blind: an
                        // over-collected RQA case only defers more).
                        SigDecl::Types(types) => {
                            for defn in types.defns() {
                                let case_idents: Vec<SyntaxToken> = match defn.repr() {
                                    Some(TypeDefnRepr::Union(u)) => {
                                        u.cases().filter_map(|c| c.ident()).collect()
                                    }
                                    Some(TypeDefnRepr::Enum(e)) => {
                                        e.cases().filter_map(|c| c.ident()).collect()
                                    }
                                    // `type Color = Shared` parses as an
                                    // abbreviation, but FCS reads a
                                    // single-ident target that names no type
                                    // as a nullary union case
                                    // (`TyconCoreAbbrevThatIsReallyAUnion`).
                                    // Over-approximate: treat any
                                    // single-ident target as possibly a
                                    // case (a genuine abbreviation target
                                    // only defers more).
                                    Some(TypeDefnRepr::Abbrev(a)) => a
                                        .ty()
                                        .as_ref()
                                        .and_then(abbrev_target_single_ident)
                                        .into_iter()
                                        .collect(),
                                    _ => Vec::new(),
                                };
                                for ident in case_idents {
                                    let mut path = ns.clone();
                                    path.push(id_text(ident.text()).to_string());
                                    value_paths.push(path);
                                }
                            }
                        }
                        SigDecl::Exception(exn) => {
                            if let Some(ident) = exn.union_case().and_then(|c| c.ident()) {
                                let mut path = ns.clone();
                                path.push(id_text(ident.text()).to_string());
                                value_paths.push(path);
                            }
                        }
                        _ => {}
                    }
                }
            }
            ModuleOrNamespaceKind::Anon => {
                // A headerless signature restricts the **implicit filename
                // module** (FCS's `ComputeAnonModuleName`: the canonicalised
                // stem, dots splitting into path segments — which the QNOF
                // carries, dedup suffix stripped). Without this root a
                // paired headerless `A.fsi` would screen nothing and a
                // sig-exposed name could commit to a colliding assembly
                // member (codex round 3).
                let path: Vec<String> = qnof
                    .undeduplicated_text()
                    .split('.')
                    .filter(|seg| !seg.is_empty())
                    .map(str::to_string)
                    .collect();
                if !path.is_empty() {
                    roots.push(model::SigRoot {
                        path,
                        auto_open: false,
                    });
                }
            }
        }
    }
    Arc::new(model::SigScreen {
        roots,
        names: sig_token_names(sig),
        auto_open_nested,
        value_paths,
    })
}

/// The signature's over-approximated exposable-name set: any name a `.fsi`
/// can expose (a `val`, a union case, a record field, an exception, an
/// active-pattern case name, …) necessarily appears in its token stream, so
/// collecting every non-trivia token's `idText` — plus each token's
/// ident-shaped pieces — is a sound over-approximation. Trivia (whitespace,
/// comments, directives, inactive code) is excluded: a name mentioned only in
/// a comment must not screen.
fn sig_token_names(sig: &SigFile) -> HashSet<String> {
    let mut names = HashSet::new();
    for token in sig
        .syntax()
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
    {
        if token.kind().is_trivia() {
            continue;
        }
        let text = id_text(token.text());
        names.insert(text.to_string());
        for piece in text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\'')) {
            if !piece.is_empty() && piece != text {
                names.insert(piece.to_string());
            }
        }
    }
    names
}

/// Resolve a whole project: fold [`resolve_file`] over the files in Compile
/// order (F# is order-sensitive across files — a file references only itself
/// and earlier files), threading each file's exports into the next file's
/// [`ProjectItems`]. Pure: the caller (the LSP shell) supplies the
/// Compile-ordered parsed files and the assembly environment.
///
/// The impl-only convenience form; a project whose Compile list carries
/// `.fsi` signature files goes through [`resolve_project_files`].
pub fn resolve_project(files: &[ImplFile], assemblies: &AssemblyEnv) -> ResolvedProject {
    resolve_project_files_impl(&impl_only_files(files), files.len(), assemblies, None)
}

/// Resolve a whole project whose Compile list may interleave `.fsi`
/// **signature files** (`docs/fsi-signature-restriction-plan.md` Stage 1).
/// A signature occupies its Compile slot with an inert [`ResolvedFile`]
/// (empty resolutions and exports — Stage 2 gives it a real surface) and
/// contributes its screen; the paired implementation (first following equal
/// QNOF) resolves exactly as an unsigned file but its value/case identity
/// exports are dropped at the boundary. Unsigned files are untouched.
pub fn resolve_project_files(files: &[ProjectFile], assemblies: &AssemblyEnv) -> ResolvedProject {
    resolve_project_files_impl(files, files.len(), assemblies, None)
}

/// Fold only the first `len` Compile items of `files` — the cost-bounded
/// form a per-file query uses (file `i` can reference only files `0..i`) —
/// while the sig ↔ impl **pairing still derives from the whole list**. A
/// signature whose implementation lies past the prefix is still *paired*
/// (its screen publishes), so the prefix fold is prefix-monotone: `file(i)`
/// equals the full fold's for every `i < len`. Pairing from the sliced list
/// instead would flip such a signature to unpaired and let the answer for
/// one file depend on the query depth that populated a cache (codex round
/// 2).
///
/// # Panics
///
/// Panics when `len > files.len()`.
pub fn resolve_project_files_prefix(
    files: &[ProjectFile],
    len: usize,
    assemblies: &AssemblyEnv,
) -> ResolvedProject {
    resolve_project_files_impl(files, len, assemblies, None)
}

/// Like [`resolve_project_files_prefix`], but tags each folded file's
/// `resolve_file` span with its path (`labels[i]`, parallel to `files`) so a
/// profiling build can attribute the fold's cost per Compile item in the
/// trace. The resolution result is identical — `labels` affects only the
/// emitted span. Present only under the `otel` feature (the sole build that
/// installs a subscriber).
#[cfg(feature = "otel")]
pub fn resolve_project_files_prefix_labeled(
    files: &[ProjectFile],
    len: usize,
    labels: &[String],
    assemblies: &AssemblyEnv,
) -> ResolvedProject {
    resolve_project_files_impl(files, len, assemblies, Some(labels))
}

/// The shared Compile-order fold over `files[..horizon]`, with pairing from
/// the whole `files` list. `_labels`, when present, tags each file's
/// `resolve_file` span (`file`) for trace attribution; it is read only under
/// the `otel` feature and never influences the resolution result.
fn resolve_project_files_impl(
    files: &[ProjectFile],
    horizon: usize,
    assemblies: &AssemblyEnv,
    _labels: Option<&[String]>,
) -> ResolvedProject {
    assert!(
        horizon <= files.len(),
        "fold horizon {horizon} exceeds the Compile list ({})",
        files.len()
    );
    let partners = pairing_partners(files);
    let mut preceding = ProjectItems::default();
    let mut resolved: Vec<Arc<ResolvedFile>> = Vec::with_capacity(files.len());
    // Accumulate the OV-6 cross-file **extension-source** signal in **Compile
    // order**: F# is order-sensitive, so a project extension source (a
    // `[<Extension>]` class or a `type … with` augmentation, which a
    // same-namespace later file sees with no `open`) is in scope only in files
    // that come *after* the one declaring it. Each file's gate signal is
    // therefore "a **preceding** file declares a project extension source"; a
    // file's *own* sources are covered separately (the gate's own file-walk).
    // Conservative: namespace-blind (a preceding source in an unrelated
    // namespace defers too) — the honest cost of soundness, with
    // namespace-scoping the OV-9 refinement.
    let mut ext = ExtThreading::default();
    for (index, pf) in files.iter().take(horizon).enumerate() {
        let rf = match &pf.file {
            // A signature file is inert in Stage 1: it owns no `ItemId` range
            // and records nothing; only its screen crosses the boundary.
            SourceFile::Sig(sig) => ResolvedFile::inert_signature(
                preceding.next_base(),
                signature_screen(sig, &pf.qnof),
            ),
            SourceFile::Impl(file) => {
                // Per-file span so the fold's cost is attributable in the LSP's
                // traces: `file` is the Compile item's path (empty when
                // unlabelled), `index` its Compile-order position, `bytes` its
                // source length as a size proxy. Only compiled under `otel`; the
                // default build has no `tracing` dependency.
                #[cfg(feature = "otel")]
                let _span = {
                    let file_path = _labels
                        .and_then(|l| l.get(index))
                        .map(String::as_str)
                        .unwrap_or_default();
                    tracing::info_span!(
                        "resolve_file",
                        index = index,
                        bytes = u32::from(file.syntax().text_range().len()),
                        file = file_path,
                    )
                    .entered()
                };
                let mut rf = resolve_file(file, &preceding, assemblies);
                rf.preceding_declares_extension_source = ext.wholesale;
                rf.preceding_augmentation_instance_names = ext.instance_names.clone();
                rf.preceding_augmentation_static_names = ext.static_names.clone();
                rf
            }
        };
        // The screen of the signature this file is the paired implementation
        // of, if any — the sig sits strictly earlier in Compile order, so its
        // resolved slot already exists. (A signature's own partner is the
        // *later* impl, so for a sig this is always `None`.)
        let screen = partners[index]
            .filter(|&p| p < index)
            .and_then(|sig| resolved[sig].sig_screen.clone());
        thread_forward(
            &mut preceding,
            &mut ext,
            &rf,
            assemblies,
            screen.as_deref(),
            partners[index].is_some(),
        );
        // An unpaired headerless implementation: its values live under the
        // implicit filename module, which sema's export model cannot address
        // — shadow the path so a colliding assembly member defers rather
        // than committing where FCS binds the project value. (A paired
        // headerless impl is screened per-name by its signature instead.)
        if partners[index].is_none()
            && matches!(&pf.file, SourceFile::Impl(_))
            && pf.file.is_headerless()
        {
            let path = implicit_module_path(&pf.qnof);
            if !path.is_empty() {
                preceding.note_implicit_module_shadow(path);
            }
        }
        resolved.push(Arc::new(rf));
    }
    ResolvedProject { files: resolved }
}

/// The Compile-order **extension-source** state threaded through the fold
/// (EX-3 §2(b)): the wholesale presence bit for sources that stay
/// presence-based (an un-nameable augmentation member, an attribute that may
/// declare an extension), plus the accumulated **augmentation member names**
/// by call shape — a preceding file's `type … with` member joins its own
/// name's group only, so later files defer exactly those names
/// (namespace-blind and `private`-blind, like the bit: over-deferral only).
#[derive(Default)]
struct ExtThreading {
    wholesale: bool,
    instance_names: HashSet<String>,
    static_names: HashSet<String>,
}

/// Advance the Compile-order threaded state past `rf` (the resolution of
/// `file`), exactly as the fold does: fold this file's exports into `preceding`
/// and its own extension-source contribution into `ext`. The single writer
/// of the forward threading — shared by the cold ([`resolve_project_impl`]) and
/// incremental ([`resolve_project_incremental`]) folds so the two can never
/// disagree on what a file contributes downstream.
///
/// The caller stamps `rf`'s `preceding_*` fields from the value of `ext`
/// *entering* the file before calling this (the fields record the state the
/// file saw, whereas this bumps `ext` for the *next* file).
///
/// `paired_screen` is the screen of the signature `rf` is the paired
/// implementation of, if any — it parameterises the boundary derivation
/// (the paired impl's value/case identity exports are dropped; see
/// [`ProjectItems::extend_with`]). `partnered` is whether `rf`'s Compile
/// item has a pairing partner at all: a signature publishes its screen only
/// when a following implementation consumes it (an unpaired signature
/// constrains nothing — FCS-probed).
fn thread_forward(
    preceding: &mut ProjectItems,
    ext: &mut ExtThreading,
    rf: &ResolvedFile,
    assemblies: &AssemblyEnv,
    paired_screen: Option<&model::SigScreen>,
    partnered: bool,
) {
    preceding.extend_with(rf, paired_screen, partnered);
    ext.wholesale |= wholesale_extension_contribution(rf, assemblies);
    ext.instance_names
        .extend(rf.augmentation_instance_names.iter().cloned());
    ext.static_names
        .extend(rf.augmentation_static_names.iter().cloned());
}

/// The **wholesale** extension-source signal one file threads to later
/// Compile-order files: an augmentation member whose *name* was not walkable
/// (EX-3 §2(a) — the walkable names thread as sets, [`ExtThreading`]), or an
/// attribute that **may declare an extension** — EX-3 §2(d) stage 5: the
/// attribute half is no longer "any attribute" but the resolver's
/// per-attribute verdicts, exactly as the gate's own-file trigger reads them
/// (an attribute resolving to a concrete non-`ExtensionAttribute` type
/// provably marks nothing for later files either). An `[<AutoOpen>]` module
/// *as such* contributes nothing (AO-1): the only extension-capable contents
/// it can carry are exactly these two signals — a `type … with` augmentation
/// (collected file-globally by the §2(a) walk, nested modules included) or a
/// `[<Extension>]` attribute (resolved file-globally by the §2(d) walk) —
/// while a module-level `[<Extension>] let` folds through *vals*, where the
/// C#-style extension predicate never runs (fsi-verified; pinned by the
/// `CoreExtAttrLets` fixture). One function so the fold's threading
/// ([`thread_forward`]) and the incremental fold's in-sync comparison can
/// never disagree about what a file contributes.
fn wholesale_extension_contribution(rf: &ResolvedFile, assemblies: &AssemblyEnv) -> bool {
    rf.augmentation_names_unknowable || rf.attributes_may_declare_extension(assemblies)
}

/// Whether two Compile items are the **same file**: equal kind and
/// [`QualifiedNameOfFile`] (a rename can change the QNOF and with it the
/// pairing, so it must invalidate reuse), and the same **tree instance** —
/// rowan [`SyntaxNode`] equality is *identity*, not structural (pinned by the
/// LSP's `syntax_node_equality_is_identity_not_structural` test): two clones
/// of one parsed tree compare equal, two independent parses of identical text
/// do not. So this answers "is `new` the very tree that produced `prev`'s
/// resolution?" — true only when the file was reused verbatim (the LSP's
/// stage-1 per-file parse cache hands back a clone on a hit). A *false*
/// answer is always safe (the file is recomputed); a false *positive* is
/// impossible (distinct instances never compare equal), so a reuse decision
/// built on this can never serve a resolution from the wrong tree.
fn same_tree(prev: &ProjectFile, new: &ProjectFile) -> bool {
    prev.qnof == new.qnof
        && match (&prev.file, &new.file) {
            (SourceFile::Impl(a), SourceFile::Impl(b)) => a.syntax() == b.syntax(),
            (SourceFile::Sig(a), SourceFile::Sig(b)) => a.syntax() == b.syntax(),
            _ => false,
        }
}

/// Incrementally re-fold a project after an edit: like [`resolve_project`], but
/// reuses per-file [`ResolvedFile`]s from a previous fold (`prev`, over
/// `prev_files`) wherever an edit cannot have changed them, so a single-file
/// edit re-resolves only the changed file (and any file whose *inputs* the
/// change shifted) instead of the whole Compile order.
///
/// Returns exactly what a cold [`resolve_project`] of `new_files` would — the
/// `resolve_incremental_diff.rs` differential asserts `incremental ≡ batch` over
/// generated edit sequences. Reuse is sound because `resolve_file`'s output is a
/// pure function of `(file, preceding, assemblies)`:
///
/// - **`prev_files`/`prev` must pair up:** `prev` must be the result of folding
///   `prev_files` (in that order) against `assemblies`. The LSP stores them
///   together.
/// - **`assemblies` must be the environment `prev` was folded against.** A file
///   reused verbatim keeps resolutions computed against the *old* env, so a
///   changed env would make them stale. The LSP enforces this by only calling
///   here when the current env is the *same `Arc`* it folded `prev` against (a
///   rebuilt env — invalidation or a referenced-DLL change — is a fresh `Arc`,
///   forcing a cold fold). This function cannot check it, so it is a
///   precondition, not a guard.
///
/// The reuse logic threads a single `in_sync` flag: while the state entering
/// file `i` still matches `prev`'s (a monotone prefix property — once a file's
/// contribution diverges, every later file sees a different `preceding`), a file
/// whose tree is byte-identical (`same_tree`) is reused verbatim, and a
/// recomputed file that leaves the threaded state unchanged
/// (`ResolvedFile::same_export_contribution` plus an unchanged own
/// extension-source signal) keeps the tail reusable too.
pub fn resolve_project_incremental(
    prev_files: &[ImplFile],
    prev: &ResolvedProject,
    new_files: &[ImplFile],
    assemblies: &AssemblyEnv,
) -> ResolvedProject {
    resolve_project_files_incremental_impl(
        &impl_only_files(prev_files),
        prev,
        &impl_only_files(new_files),
        new_files.len(),
        assemblies,
        None,
    )
    .0
}

/// The signature-aware incremental fold: like [`resolve_project_incremental`]
/// over [`ProjectFile`]s, returning the reuse vector of
/// [`resolve_project_incremental_with_reuse`]. `prev` must be the result of
/// folding `prev_files` (with their QNOFs) against `assemblies` — the
/// [`resolve_project_files`] counterpart of the impl-only preconditions. A
/// `.fsi` edit re-derives the paired implementation's boundary contribution
/// (the screen is part of the signature's own threaded contribution, and the
/// pairing-partner index is part of the reuse condition), so it invalidates
/// exactly the suffix a cold fold would change.
pub fn resolve_project_files_incremental(
    prev_files: &[ProjectFile],
    prev: &ResolvedProject,
    new_files: &[ProjectFile],
    assemblies: &AssemblyEnv,
) -> (ResolvedProject, Vec<bool>) {
    resolve_project_files_incremental_impl(
        prev_files,
        prev,
        new_files,
        new_files.len(),
        assemblies,
        None,
    )
}

/// The prefix form of [`resolve_project_files_incremental`]: fold only
/// `new_files[..len]`, with pairing derived from the **whole** `prev_files`
/// / `new_files` lists (see [`resolve_project_files_prefix`] — `prev` may
/// itself cover a shorter or deeper prefix of `prev_files`; reuse is bounded
/// by what it holds).
///
/// # Panics
///
/// Panics when `len > new_files.len()`.
pub fn resolve_project_files_prefix_incremental(
    prev_files: &[ProjectFile],
    prev: &ResolvedProject,
    new_files: &[ProjectFile],
    len: usize,
    assemblies: &AssemblyEnv,
) -> (ResolvedProject, Vec<bool>) {
    resolve_project_files_incremental_impl(prev_files, prev, new_files, len, assemblies, None)
}

/// Like [`resolve_project_incremental`], but also returns, per Compile-order
/// index, whether that file's previous [`ResolvedFile`] was **reused verbatim**
/// (`true`) or re-resolved (`false`). `reused.len() == new_files.len()`.
///
/// The reuse vector is the observable the incremental fold exists to produce — a
/// keystroke should reuse every file the edit didn't touch — so tests can assert
/// reuse *actually happens* (not merely that the result matches a cold fold,
/// which holds even when nothing is reused) and a profiling caller can report
/// how much a fold saved. The preconditions are [`resolve_project_incremental`]'s.
pub fn resolve_project_incremental_with_reuse(
    prev_files: &[ImplFile],
    prev: &ResolvedProject,
    new_files: &[ImplFile],
    assemblies: &AssemblyEnv,
) -> (ResolvedProject, Vec<bool>) {
    resolve_project_files_incremental_impl(
        &impl_only_files(prev_files),
        prev,
        &impl_only_files(new_files),
        new_files.len(),
        assemblies,
        None,
    )
}

/// Like [`resolve_project_files_incremental`], but also tags each
/// *recomputed* file's `resolve_file` span with its path (`labels[i]`, parallel
/// to `new_files`) for per-Compile-item trace attribution — the incremental
/// counterpart of [`resolve_project_files_prefix_labeled`]. A *reused* file does no
/// `resolve_file` work and emits no span, so under a profiling build the spans
/// that appear are exactly the files the edit forced to re-resolve (and the
/// reused ones' absence is the signal that the edit didn't touch them). The
/// resolution result and reuse vector are identical to the unlabelled variants —
/// `labels` affects only the emitted spans. Present only under the `otel`
/// feature (the sole build that installs a subscriber).
#[cfg(feature = "otel")]
pub fn resolve_project_files_prefix_incremental_labeled(
    prev_files: &[ProjectFile],
    prev: &ResolvedProject,
    new_files: &[ProjectFile],
    len: usize,
    labels: &[String],
    assemblies: &AssemblyEnv,
) -> (ResolvedProject, Vec<bool>) {
    resolve_project_files_incremental_impl(
        prev_files,
        prev,
        new_files,
        len,
        assemblies,
        Some(labels),
    )
}

/// The shared incremental fold, returning the result and a per-file reuse vector
/// (`reused[i]` = file `i` was reused verbatim). `_labels`, when present, tags
/// each *recomputed* file's `resolve_file` span (`file`) for trace attribution;
/// it is read only under the `otel` feature and never influences the result.
fn resolve_project_files_incremental_impl(
    prev_files: &[ProjectFile],
    prev: &ResolvedProject,
    new_files: &[ProjectFile],
    horizon: usize,
    assemblies: &AssemblyEnv,
    _labels: Option<&[String]>,
) -> (ResolvedProject, Vec<bool>) {
    assert!(
        horizon <= new_files.len(),
        "fold horizon {horizon} exceeds the Compile list ({})",
        new_files.len()
    );
    let partners_prev = pairing_partners(prev_files);
    let partners_new = pairing_partners(new_files);
    let mut preceding = ProjectItems::default();
    let mut ext = ExtThreading::default();
    let mut resolved: Vec<Arc<ResolvedFile>> = Vec::with_capacity(horizon);
    let mut reused = Vec::with_capacity(horizon);
    // True while the threaded state (`preceding`, `ext`) entering this file still
    // equals prev's at the same index — the reuse precondition. Monotone
    // true→false: once a recomputed file's contribution differs, every later file
    // sees a different `preceding`, so reuse can never resume.
    let mut in_sync = true;
    for (i, pf) in new_files.iter().take(horizon).enumerate() {
        let have_prev = i < prev.files.len() && i < prev_files.len();
        // Pairing is an input to the file's boundary *contribution* (a paired
        // implementation's value/case exports are dropped, a paired
        // signature publishes its screen — `ProjectItems::extend_with`), so
        // both reuse and sync require the pairing-partner index to match.
        // While the prefix is in sync an equal index implies an equal
        // screen: the signature's own contribution (its screen,
        // `same_export_contribution`) was compared at the signature's
        // earlier slot.
        let same_pairing = have_prev
            && partners_prev[i] == partners_new[i]
            // The implicit-module shadow of an unpaired headerless file
            // derives from its QNOF, so a rename must invalidate exactly
            // like a pairing change (reuse already requires it via
            // `same_tree`; this covers the recomputed-but-in-sync path).
            && prev_files[i].qnof == pf.qnof;
        let reuse = in_sync && same_pairing && same_tree(&prev_files[i], pf);
        reused.push(reuse);
        // Per-file span *only* for a recomputed file (otel): mirrors the cold
        // labeled fold's attribution (`file`/`index`/`bytes`) so an edited
        // project's re-resolution cost is still traceable per Compile item. A
        // reused file does no `resolve_file` work, so it gets no span — its
        // absence is the diagnostic that the edit left it untouched. The inner
        // `resolve_file` phase spans nest under this, exactly as in the cold fold.
        #[cfg(feature = "otel")]
        let _span = (!reuse).then(|| {
            let file_path = _labels
                .and_then(|l| l.get(i))
                .map(String::as_str)
                .unwrap_or_default();
            tracing::info_span!(
                "resolve_file",
                index = i,
                bytes = u32::from(pf.file.syntax().text_range().len()),
                file = file_path,
            )
            .entered()
        });
        let rf: Arc<ResolvedFile> = if reuse {
            // Same entering state + same tree ⇒ `resolve_file` would return an
            // identical `ResolvedFile`. Reuse it — an `Arc` refcount bump, *not* a
            // deep clone of the resolution map / arena / exports, so a keystroke's
            // reuse is O(1) per file rather than O(occurrences). `in_sync` stays
            // true (its contribution is prev's own); the shared
            // `preceding_declares_extension_source` already equals `ext`, since
            // `ext` matches prev's entering value while `in_sync`.
            Arc::clone(&prev.files[i])
        } else {
            let rf = match &pf.file {
                SourceFile::Sig(sig) => ResolvedFile::inert_signature(
                    preceding.next_base(),
                    signature_screen(sig, &pf.qnof),
                ),
                SourceFile::Impl(file) => {
                    let mut rf = resolve_file(file, &preceding, assemblies);
                    rf.preceding_declares_extension_source = ext.wholesale;
                    rf.preceding_augmentation_instance_names = ext.instance_names.clone();
                    rf.preceding_augmentation_static_names = ext.static_names.clone();
                    rf
                }
            };
            if in_sync {
                // Entering state matched prev's, so the item base matched and the
                // exports are comparable. Does the recompute leave the threaded
                // state unchanged (all halves: the export contribution — a
                // signature's screen included — the pairing, the wholesale
                // extension-source signal, and the augmentation name sets)?
                // If so, the tail stays reusable. The quantities compared are
                // exactly what `thread_forward` folds, so the incremental
                // fold can never disagree with the cold one about a file's
                // downstream contribution.
                in_sync = same_pairing
                    && rf.same_export_contribution(&prev.files[i])
                    && wholesale_extension_contribution(&rf, assemblies)
                        == wholesale_extension_contribution(&prev.files[i], assemblies)
                    && rf.augmentation_instance_names == prev.files[i].augmentation_instance_names
                    && rf.augmentation_static_names == prev.files[i].augmentation_static_names;
            }
            Arc::new(rf)
        };
        let screen = partners_new[i]
            .filter(|&p| p < i)
            .and_then(|sig| resolved[sig].sig_screen.clone());
        thread_forward(
            &mut preceding,
            &mut ext,
            &rf,
            assemblies,
            screen.as_deref(),
            partners_new[i].is_some(),
        );
        // The unpaired-headerless implicit-module shadow — exactly as the
        // cold fold pushes it (see `resolve_project_files_impl`).
        if partners_new[i].is_none()
            && matches!(&pf.file, SourceFile::Impl(_))
            && pf.file.is_headerless()
        {
            let path = implicit_module_path(&pf.qnof);
            if !path.is_empty() {
                preceding.note_implicit_module_shadow(path);
            }
        }
        resolved.push(rf);
    }
    (ResolvedProject { files: resolved }, reused)
}

/// The source position that stamps a top-level module / namespace header's
/// [`ExportDecl`](model::ExportDecl)s — its `longId` start, or the node start for
/// an anonymous / empty header. Provenance only; no Stage-2 derivation reads it.
fn module_header_pos(module: &ModuleOrNamespace) -> rowan::TextSize {
    module
        .long_id()
        .and_then(|li| li.idents().next())
        .map(|t| t.text_range().start())
        .unwrap_or_else(|| module.syntax().text_range().start())
}

/// The module-path prefix that qualifies a file's exports, or `None` for an
/// anonymous (header-less) module. A `namespace` carries no directly-bound
/// values (only modules/types live under it), so only a `NamedModule`
/// contributes a value-qualifying prefix.
fn module_prefix(module: &ModuleOrNamespace) -> Option<Vec<String>> {
    match module.kind() {
        ModuleOrNamespaceKind::NamedModule => module
            .long_id()
            .map(|li| li.idents().map(|t| id_text(t.text()).to_string()).collect()),
        ModuleOrNamespaceKind::Anon
        | ModuleOrNamespaceKind::DeclaredNamespace
        | ModuleOrNamespaceKind::GlobalNamespace => None,
    }
}

impl<'a> Resolver<'a> {
    fn new(preceding: &'a ProjectItems, assemblies: &'a AssemblyEnv) -> Self {
        Resolver {
            defs: Vec::new(),
            items: Vec::new(),
            resolutions: HashMap::new(),
            scopes: Vec::new(),
            typar_scopes: Vec::new(),
            type_defs: HashMap::new(),
            type_slot_classes: HashMap::new(),
            type_access_roots: HashMap::new(),
            container_decls: HashMap::new(),
            type_cases: HashMap::new(),
            type_members: HashMap::new(),
            unindexed_augmented_names: HashSet::new(),
            module_like_names: HashMap::new(),
            top_level: HashMap::new(),
            preceding,
            assemblies,
            item_base: preceding.next_base(),
            module_path: None,
            container_path: Vec::new(),
            namespace_depth: 0,
            module_paths: Vec::new(),
            namespace_paths: Vec::new(),
            nested_module_locals: Vec::new(),
            top_level_nested_locals: HashMap::new(),
            nested_module_exports: Vec::new(),
            real_nested_module_exports: Vec::new(),
            type_path_exports: Vec::new(),
            imports: implicit_open_groups(assemblies),
            open_shortening_prefixes: implicit_open_namespaces(assemblies),
            incomplete_open_prefixes: Vec::new(),
            explicit_open_prefixes: Vec::new(),
            module_open_prefixes: Vec::new(),
            assembly_open_prefixes: Vec::new(),
            open_generation: 0,
            pattern_suppressed_case_ids: HashSet::new(),
            modules_with_hidden_values: HashSet::new(),
            auto_open_module_paths: Vec::new(),
            open_extension_namespaces: Vec::new(),
            open_extension_unknowable: false,
            attribute_resolutions: HashMap::new(),
            own_type_simple_names: HashSet::new(),
            own_generic_type_simple_names: HashSet::new(),
            own_exception_simple_names: HashSet::new(),
            own_abbrev_type_simple_names: HashSet::new(),
            own_auto_open_type_names: HashSet::new(),
            attribute_shape_unknowable: false,
            augmentation_instance_names: HashSet::new(),
            augmentation_static_names: HashSet::new(),
            augmentation_names_unknowable: false,
            latest_open_pos: 0,
            module_aliases: HashMap::new(),
            unmodelled_open_active: false,
            opaque_value_open: false,
            opaque_dotted_open: false,
            recursive_module_active: false,
            rec_module_names: HashSet::new(),
            auto_open_type_shadow_names: HashMap::new(),
            anonymous_root: false,
            access_floor: None,
            pending_items: HashSet::new(),
            ap_body_case_names: HashSet::new(),
            active_pattern_shape: HashMap::new(),
            excluded_param_ranges: HashSet::new(),
            decline_binding_head_param_exprs: false,
            diagnostics: Vec::new(),
            trace_opens: Vec::new(),
            export_decls: Vec::new(),
        }
    }

    /// Append one [`ExportDecl`] to the file's source-ordered declaration list,
    /// stamping it with the current [`Self::anonymous_root`]. The single
    /// append point (`docs/export-decl-model-plan.md` Stage 2): a decl is added
    /// wherever a legacy export writer fires, so the cross-file derivations in
    /// [`ProjectItems::extend_with`](model::ProjectItems::extend_with) reproduce
    /// today's ordering by construction.
    pub(super) fn push_export_decl(
        &mut self,
        path: Vec<String>,
        pos: rowan::TextSize,
        kind: model::ExportDeclKind,
    ) {
        self.export_decls.push(model::ExportDecl {
            path,
            pos,
            anonymous_root: self.anonymous_root,
            kind,
        });
    }

    /// Attach a case's type-qualified path to the most recently appended
    /// [`ExportDecl`], which must be the [`model::ExportDeclKind::Item`] just
    /// pushed for this case (both
    /// [`export_type_qualified_case`](Self::export_type_qualified_case) call sites
    /// invoke it immediately after the item's decl append — no other decl can
    /// intervene). Feeds the `type_qualified_cases` derivation.
    pub(super) fn set_last_decl_type_qualified(&mut self, tq: Vec<String>) {
        match self.export_decls.last_mut().map(|d| &mut d.kind) {
            Some(model::ExportDeclKind::Item { type_qualified, .. }) => {
                debug_assert!(
                    type_qualified.is_none(),
                    "type-qualified path set twice on one case item decl"
                );
                *type_qualified = Some(tq);
            }
            other => debug_assert!(
                false,
                "set_last_decl_type_qualified expects a trailing Item decl, found {other:?}"
            ),
        }
    }

    /// Bring the `[<AutoOpen>]` modules of `namespace` into scope: opening a
    /// namespace also opens any auto-open module it declares, so each such
    /// module's public static names enter the current frame as source-ordered
    /// `opened` entries — exactly as `open type Module` does
    /// ([`Self::open_type_statics`]), reached through
    /// [`AssemblyEnv::auto_open_modules_in_namespace`]. This is how FSharp.Core's
    /// `printfn` / `id` / operators resolve unqualified.
    ///
    /// `certain` is withheld when this namespace is *also* an assembly **module** whose
    /// surface we cannot fully enumerate — a hidden module name could contest any of
    /// these, and FCS folds the two halves in reference order (review round 17).
    fn open_auto_open_modules_in(&mut self, namespace: &[String], open_pos: u32, certain: bool) {
        // Collect first — the slice borrows `self.assemblies`, while
        // `open_type_statics` borrows `self` mutably.
        let handles: Vec<EntityHandle> = self
            .assemblies
            .auto_open_modules_in_namespace(namespace)
            .to_vec();
        for handle in handles {
            self.open_type_statics(handle, open_pos, certain);
        }
    }

    /// Whether a **project** `[<AutoOpen>]` module sits directly in `namespace`
    /// — checked per-namespace at each type-position lookup
    /// ([`Self::unmodelled_type_shadow_at`]), not pre-aggregated: sema does not
    /// model such a module's nested types, so it may provide a type name not
    /// in the normal project type index.
    fn project_auto_open_module_in_namespace(&self, namespace: &[String]) -> bool {
        self.auto_open_module_paths
            .iter()
            .any(|(p, _)| model::is_directly_in(p, namespace))
            || self.preceding.has_auto_open_module_in_namespace(namespace)
    }

    /// Record one `[<AutoOpen>]` module declaration — the single writer for
    /// [`Self::auto_open_module_paths`], so the same-file view and the
    /// cross-file export (the `finish()` privacy filter) can never disagree
    /// on what was declared.
    pub(super) fn record_auto_open_module(&mut self, path: Vec<String>, is_private: bool) {
        self.auto_open_module_paths.push((path, is_private));
    }

    fn finish(self) -> ResolvedFile {
        ResolvedFile {
            defs: self.defs,
            resolutions: self.resolutions,
            attribute_resolutions: self.attribute_resolutions,
            own_type_simple_names: self.own_type_simple_names,
            own_abbrev_type_simple_names: self.own_abbrev_type_simple_names,
            attribute_shape_unknowable: self.attribute_shape_unknowable,
            augmentation_instance_names: self.augmentation_instance_names,
            augmentation_static_names: self.augmentation_static_names,
            augmentation_names_unknowable: self.augmentation_names_unknowable,
            // Stamped by the Compile-order fold; a single file has no
            // preceding files.
            preceding_augmentation_instance_names: HashSet::new(),
            preceding_augmentation_static_names: HashSet::new(),
            exports: ExportedItems { items: self.items },
            item_base: self.item_base,
            namespace_paths: self.namespace_paths,
            // The *cross-file* (preceding-files) signal is set by
            // `resolve_project`; a single file has no preceding files.
            preceding_declares_extension_source: false,
            open_extension_namespaces: self.open_extension_namespaces,
            open_extension_unknowable: self.open_extension_unknowable,
            active_pattern_shape: self.active_pattern_shape,
            diagnostics: self.diagnostics,
            resolution_trace: model::ResolutionTrace {
                opens: self.trace_opens,
            },
            export_decls: self.export_decls,
            sig_screen: None,
        }
    }

    fn intern(&mut self, def: Def) -> DefId {
        let id = DefId::new(self.defs.len());
        self.defs.push(def);
        id
    }

    fn record(&mut self, range: TextRange, res: Resolution) {
        self.resolutions.insert(range, res);
    }
}

/// Whether a `type` definition is an *augmentation* (`type T with member …`):
/// its head names an *existing* type, not a new definition, so it must not be
/// re-interned as a fresh binder. FCS encodes it as an object-model repr
/// carrying a `with` in place of the `=`.
fn is_type_augmentation(defn: &TypeDefn) -> bool {
    matches!(defn.repr(), Some(TypeDefnRepr::ObjectModel(om)) if om.is_augmentation())
}

/// The sole identifier token of a single-segment type name (`type T`), or
/// `None` for a qualified head (`type A.B with …` — an augmentation of an
/// existing nested type, never a fresh definition) or a malformed/empty name.
fn single_ident(li: LongIdent) -> Option<SyntaxToken> {
    let mut idents = li.idents();
    let first = idents.next()?;
    idents.next().is_none().then_some(first)
}

/// The single, unqualified identifier a type-abbreviation right-hand side names,
/// after stripping enclosing parens — FCS's `StripParenTypes (SynType.LongIdent
/// (SynLongIdent([name], _, _)))` shape. `None` for a qualified, generic, or
/// otherwise compound target. This is the syntactic half of FCS's
/// `TyconCoreAbbrevThatIsReallyAUnion` (`CheckDeclarations.fs`): such a target is
/// a union *case* rather than a type reference when it either names no type in
/// scope or equals the type being defined (`type X = X`).
fn abbrev_target_single_ident(ty: &Type) -> Option<SyntaxToken> {
    match ty {
        Type::Paren(p) => p.inner().as_ref().and_then(abbrev_target_single_ident),
        Type::LongIdent(t) => t.long_ident().and_then(single_ident),
        _ => None,
    }
}

/// The active-pattern name of a `let`-binding head, if it is one — either the
/// nullary form (`let (|Foo|Bar|) = …`, a [`Pat::Named`] carrying an
/// [`ActivePatName`]) or the function form (`let (|Foo|Bar|) x = …`, a
/// [`Pat::LongIdent`] carrying one). `None` for any ordinary head.
fn active_pat_name_of(pat: &Pat) -> Option<ActivePatName> {
    match pat {
        Pat::Named(p) => p.active_pat_name(),
        Pat::LongIdent(p) => p.active_pat_name(),
        _ => None,
    }
}

/// The curried-**parameter** count of an active-pattern `let`-binding head — its
/// [`ActivePatternShape::arity`]. The two head forms `active_pat_name_of`
/// accepts differ here: the *function* form (`let (|DivBy|_|) d n = …`, a
/// [`Pat::LongIdent`]) carries its curried args syntactically, and the last one
/// is the matched value, so the parameter count is `args().count() − 1`
/// (`checked_sub` guards the degenerate arg-less shape). The *bare-name*
/// (point-free) form (`let (|P|_|) = …`, a [`Pat::Named`]) carries **no**
/// syntactic argument list — its parameter count is invisible here (FCS derives
/// it from the inferred type) — so it is `None`, not `Some(0)`. Any non-head
/// pattern (never reached, since `active_pat_name_of` already filtered) is
/// likewise `None`.
fn active_pattern_param_arity(head: &Pat) -> Option<usize> {
    match head {
        Pat::LongIdent(p) => p.args().count().checked_sub(1),
        _ => None,
    }
}

/// The dotted-path segments (`idText`-normalised) of a plain [`Type::LongIdent`]
/// — `Demo.Calc` of `open type Demo.Calc`. `None` for any compound / generic /
/// exotic type form (`open type Foo<int>`, an array, …), which we do not model.
fn type_long_ident_path(ty: &Type) -> Option<Vec<String>> {
    let Type::LongIdent(t) = ty else {
        return None;
    };
    let segs: Vec<String> = t
        .long_ident()?
        .idents()
        .map(|t| id_text(t.text()).to_string())
        .collect();
    (!segs.is_empty()).then_some(segs)
}

/// Whether any attribute in `attrs` is `[<RequireQualifiedAccess>]` (or its
/// `…Attribute` long form), matched on the attribute name's last segment. Such a
/// union keeps its cases out of the unqualified value scope, so they must not be
/// added to the case index.
fn attrs_require_qualified_access(attrs: impl Iterator<Item = AttributeList>) -> bool {
    attrs
        .flat_map(|list| list.attributes().collect::<Vec<_>>())
        .filter_map(|attr| attr.type_name())
        .filter_map(|li| li.idents().last())
        .any(|seg| {
            matches!(
                id_text(seg.text()),
                "RequireQualifiedAccess" | "RequireQualifiedAccessAttribute"
            )
        })
}

/// Whether the attribute lists mark the type `[<Struct>]` — a struct
/// union/record is a struct type (`isStructTy`), which puts its name in FCS's
/// unqualified slot (probe M20m of `docs/project-type-member-plan.md`).
fn attrs_mark_struct(attrs: impl Iterator<Item = AttributeList>) -> bool {
    attrs
        .flat_map(|list| list.attributes().collect::<Vec<_>>())
        .filter_map(|attr| attr.type_name())
        .filter_map(|li| li.idents().last())
        .any(|seg| matches!(id_text(seg.text()), "Struct" | "StructAttribute"))
}

/// Every nested-module name declared anywhere under `decls` (all depths),
/// for the `rec`-block pre-scan ([`Resolver::rec_module_names`]): inside
/// `module rec` / `namespace rec`, a later-declared module is already in
/// scope, so its name must veto multi-segment type paths the source-ordered
/// walk would otherwise resolve into a same-path assembly type. Every header
/// segment counts (a compound `module A.B = …` declares both), and non-rec
/// nested modules are scanned too — the rec scope makes them all
/// forward-visible.
pub(crate) fn collect_nested_module_names(
    decls: impl Iterator<Item = borzoi_cst::syntax::ModuleDecl>,
    out: &mut HashSet<String>,
) {
    use borzoi_cst::syntax::ModuleDecl;
    for decl in decls {
        if let ModuleDecl::NestedModule(nm) = decl {
            if let Some(li) = nm.long_id() {
                for seg in li.idents() {
                    out.insert(id_text(seg.text()).to_string());
                }
            }
            collect_nested_module_names(nm.decls(), out);
        }
    }
}

/// Whether the attribute lists mark a module `[<AutoOpen>]`.
fn attrs_auto_open(attrs: impl Iterator<Item = AttributeList>) -> bool {
    attrs
        .flat_map(|list| list.attributes().collect::<Vec<_>>())
        .filter_map(|attr| attr.type_name())
        .filter_map(|li| li.idents().last())
        .any(|seg| matches!(id_text(seg.text()), "AutoOpen" | "AutoOpenAttribute"))
}

/// FCS `idText` semantics: a double-backtick-quoted identifier
/// (`` ``my name`` ``) denotes the same name as the bare form, so strip the
/// surrounding delimiters for name *identity*. Non-quoted text is returned
/// unchanged. `pub(crate)` so inference's annotation gate
/// ([`crate::infer_file`], Stage R2-a) applies the same name identity when it
/// looks an annotation head up in the primitive-alias table.
pub(crate) fn id_text(raw: &str) -> &str {
    raw.strip_prefix("``")
        .and_then(|s| s.strip_suffix("``"))
        .unwrap_or(raw)
}

#[cfg(test)]
mod contribution_tests {
    use super::*;
    use borzoi_cst::parser::parse;

    fn resolve_one(src: &str) -> ResolvedFile {
        let p = parse(src);
        assert!(
            p.errors.is_empty(),
            "parse errors in {src:?}: {:?}",
            p.errors
        );
        let file = ImplFile::cast(p.root).expect("impl file");
        resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
    }

    /// A body-only edit that adds a *local* binder must not count as an export
    /// contribution change: it shifts a later top-level export's file-local
    /// `DefId` but leaves its qualified path, `ItemId`, accessibility, and
    /// case-ness untouched — so a downstream file's resolution is unaffected and
    /// the incremental fold must keep the suffix reusable. Regression for the
    /// codex round-2 finding that whole-`ExportedItem` comparison (which includes
    /// `def`) defeated suffix reuse for this common edit. The paired `assert_ne!`
    /// pins that the files *do* differ — so the contribution check is genuinely
    /// ignoring the local shift, not the whole diff.
    #[test]
    fn body_local_binder_does_not_change_export_contribution() {
        // `g`'s binder is interned after `f`'s body; adding a local `y` there
        // shifts `g`'s `DefId` (`f`=0 in both; `g`=1 before, =2 after `y`).
        let before = resolve_one("module M\nlet f x = x\nlet g = 1\n");
        let after = resolve_one("module M\nlet f x = (let y = x in y)\nlet g = 1\n");
        assert!(
            before.same_export_contribution(&after),
            "a body-local binder must not change the downstream contribution"
        );
        assert_ne!(
            before, after,
            "the files genuinely differ (defs/resolutions); the contribution check \
             must ignore that local difference, not miss a real one"
        );
    }

    /// The counterpart: an edit that adds a *top-level* export IS a contribution
    /// change (a new qualified path / `ItemId`), so the check must return false.
    #[test]
    fn added_export_changes_contribution() {
        let before = resolve_one("module M\nlet f = 1\n");
        let after = resolve_one("module M\nlet f = 1\nlet h = 2\n");
        assert!(
            !before.same_export_contribution(&after),
            "a new top-level export must change the contribution"
        );
    }
}

/// Env-free unit tests for the resolution-explain trace
/// ([`ResolutionTrace`](model::ResolutionTrace)) — the per-`open`
/// perturbs-resolution record. They pin the trace *mechanics* (which mechanisms
/// an open kind triggers, source order, `global.` stripping, transition
/// attribution, the generation barrier) against an empty [`AssemblyEnv`], where
/// an unresolvable `open` deterministically perturbs resolution; the end-to-end
/// "a perturbing open explains a deferred dotted head" is exercised by the
/// corpus-diff consumer against a real project.
#[cfg(test)]
mod trace_tests {
    use super::*;
    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::{AstNode, ImplFile};
    use model::ProjectItems;

    fn resolve_one(src: &str) -> ResolvedFile {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "snippet has parse errors {src:?}: {:?}",
            parsed.errors
        );
        let file = ImplFile::cast(parsed.root).expect("impl file");
        resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
    }

    #[test]
    fn an_open_type_sets_unmodelled_and_perturbs_resolution() {
        // `open type Foo` always sets `unmodelled_open_active` (its nested types
        // are unmodelled, so qualified paths defer); with `Foo` unresolvable in
        // an empty env it also sets `opaque_value_open` (bare names) and raises
        // the generation barrier. So it perturbs resolution.
        let rf = resolve_one("module M\nopen type Foo\n");
        let opens = &rf.resolution_trace().opens;
        assert_eq!(opens.len(), 1);
        let o = &opens[0];
        assert!(o.is_type);
        assert_eq!(o.path, vec!["Foo".to_string()]);
        assert!(o.opacity.unmodelled);
        assert!(o.opacity.opaque_value);
        assert!(o.opacity.perturbs_resolution());
    }

    #[test]
    fn a_namespace_open_that_brings_nothing_does_not_perturb_resolution() {
        // `open System` in an empty env resolves to no entity — it brings
        // nothing into scope and raises no barrier, so it perturbs no later
        // resolution: none of the four mechanisms triggers.
        let rf = resolve_one("module M\nopen System\n");
        let opens = &rf.resolution_trace().opens;
        assert_eq!(opens.len(), 1);
        assert!(!opens[0].is_type);
        assert_eq!(opens[0].path, vec!["System".to_string()]);
        assert!(
            !opens[0].opacity.perturbs_resolution(),
            "a bring-nothing namespace open must not read as perturbing"
        );
    }

    #[test]
    fn opens_are_traced_in_source_order_with_global_stripped() {
        let rf = resolve_one("module M\nopen System\nopen global.Foo.Bar\n");
        let opens = &rf.resolution_trace().opens;
        assert_eq!(opens.len(), 2);
        assert_eq!(opens[0].path, vec!["System".to_string()]);
        // `global.` is stripped to the namespace actually opened.
        assert_eq!(opens[1].path, vec!["Foo".to_string(), "Bar".to_string()]);
        assert!(
            opens[0].range.start() < opens[1].range.start(),
            "opens are recorded in source order"
        );
    }

    #[test]
    fn a_generation_only_barrier_is_not_labelled_clean() {
        // The three opaque flags are monotone within a block, so of two `open
        // type`s only the FIRST flips them false→true; the SECOND records no
        // boolean transition. But the generation barrier bumps on EVERY such
        // open, so the second's `staled_earlier` fires — and it perturbs
        // resolution (it stales earlier entries). Capturing the barrier is what
        // stops the second open being mislabelled `clean` (codex review round 3:
        // an all-false-boolean open that still defers a later head).
        let rf = resolve_one("module M\nopen type A\nopen type B\n");
        let opens = &rf.resolution_trace().opens;
        assert_eq!(opens.len(), 2);
        assert!(opens[0].opacity.unmodelled, "the first open flips the flag");
        // The second flips no boolean flag (monotone) …
        assert!(!opens[1].opacity.opaque_value);
        assert!(!opens[1].opacity.opaque_dotted);
        assert!(!opens[1].opacity.unmodelled);
        // … yet it raised the barrier, so it is not clean.
        assert!(
            opens[1].opacity.staled_earlier,
            "the generation barrier bumps on the second open too"
        );
        assert!(
            opens[1].opacity.perturbs_resolution(),
            "a barrier-only open must not read as clean"
        );
    }
}

/// Direct (FCS-free) unit tests pinning the trickiest
/// [`ExportDecl`](model::ExportDecl) derivations of `extend_with`
/// (`docs/export-decl-model-plan.md` Stage 2). These reach the `pub(super)`
/// boundary internals (the decl list and the folded [`ProjectItems`] indices)
/// that the external test crate cannot, so they can fail meaningfully on a
/// broken derivation rather than only through end-to-end resolution. The
/// whole-corpus scaffold proves equivalence at scale; these lock the three
/// derivations a naive migration would silently change (the plan's pitfalls).
#[cfg(test)]
mod export_decl_tests {
    use super::*;
    use borzoi_cst::parser::parse;
    use borzoi_cst::syntax::{AstNode, ImplFile};
    use model::{ExportDeclKind, ProjectItems};

    fn resolve_one(src: &str) -> ResolvedFile {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "snippet has parse errors {src:?}: {:?}",
            parsed.errors
        );
        let file = ImplFile::cast(parsed.root).expect("impl file");
        resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
    }

    /// Resolve `src` tolerating parse errors — for recovery / malformed cases.
    fn resolve_lenient(src: &str) -> ResolvedFile {
        let parsed = parse(src);
        let file = ImplFile::cast(parsed.root).expect("impl file");
        resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
    }

    fn segs(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn anonymous_root_union_case_marks_hidden_and_exports_nothing() {
        // A union case under an anonymous (header-less) top-level module has no
        // cross-file `ExportedItem` (pitfall 1): `export_case` records an
        // `Item { item: None }` decl solely to carry its container's hidden-value
        // marker. Nothing named `A`/`B` is exported, yet the empty container
        // (the anonymous root) folds into `modules_with_hidden_values`.
        let rf = resolve_one("type U = A | B\n");
        assert!(
            rf.exports
                .items
                .iter()
                .all(|i| i.name != "A" && i.name != "B"),
            "an anonymous-root union case must not be exported"
        );
        let marker_decls: Vec<_> = rf
            .export_decls
            .iter()
            .filter(|d| {
                d.anonymous_root && matches!(d.kind, ExportDeclKind::Item { item: None, .. })
            })
            .collect();
        assert_eq!(
            marker_decls.len(),
            2,
            "expected an anon hidden-marker decl per case, got {:?}",
            rf.export_decls
        );

        let mut pi = ProjectItems::default();
        pi.extend_with(&rf, None, false);
        assert!(
            pi.modules_with_hidden_values
                .contains(&Vec::<String>::new()),
            "the anonymous-root container must be hidden: {:?}",
            pi.modules_with_hidden_values
        );
        // And, being anonymous-root, it contributes no value / case exports.
        assert!(pi.value_exports.is_empty());
        assert!(pi.case_item_ids.is_empty());
    }

    #[test]
    fn private_auto_open_module_is_counted_but_not_exportable() {
        // A `private [<AutoOpen>]` module is file-local: it is *recorded* in the
        // file's export-decl list (its `[<AutoOpen>]`/`private` bits carried) but
        // *excluded* from the cross-file `auto_open_module_paths` (F# does not
        // bring a `private` module into another file's `open` scope) — pitfall 3.
        let rf = resolve_one("[<AutoOpen>]\nmodule private M\nlet x = 1\n");
        assert!(
            rf.export_decls.iter().any(|d| matches!(
                &d.kind,
                ExportDeclKind::Module {
                    header: true,
                    auto_open: true,
                    private: true,
                }
            )),
            "expected a private auto-open header decl: {:?}",
            rf.export_decls
        );

        let mut pi = ProjectItems::default();
        pi.extend_with(&rf, None, false);
        assert!(
            pi.auto_open_module_paths.is_empty(),
            "a private auto-open module must not be exportable: {:?}",
            pi.auto_open_module_paths
        );

        // A *non-private* auto-open module, by contrast, is recorded and exported
        // in order.
        let rf2 = resolve_one("[<AutoOpen>]\nmodule M\nlet x = 1\n");
        assert!(
            rf2.export_decls.iter().any(|d| matches!(
                &d.kind,
                ExportDeclKind::Module {
                    header: true,
                    auto_open: true,
                    private: false,
                }
            )),
            "expected a non-private auto-open header decl: {:?}",
            rf2.export_decls
        );
        let mut pi2 = ProjectItems::default();
        pi2.extend_with(&rf2, None, false);
        assert_eq!(pi2.auto_open_module_paths, vec![(segs(&["M"]), 0)]);
    }

    #[test]
    fn nameless_recovered_type_does_not_shadow_its_container() {
        // A nameless recovered type (`type = int`, `type exception`) has an empty
        // `long_id`; the legacy name-shadow writer skips empty segments, so the
        // decl derivation must not append a `Type` shadow at the *container* path —
        // else folding would add `M` to `nested_module_paths` and spuriously defer
        // later-file assembly references rooted there (codex fuzz find).
        for src in ["module M\ntype exception\n", "module M\ntype = int\n"] {
            let rf = resolve_lenient(src);
            assert!(
                rf.export_decls
                    .iter()
                    .all(|d| !matches!(d.kind, ExportDeclKind::Type { .. })),
                "a nameless type must contribute no Type decl for {src:?}: {:?}",
                rf.export_decls
            );
            let mut pi = ProjectItems::default();
            pi.extend_with(&rf, None, false);
            assert!(
                !pi.nested_module_paths.contains(&segs(&["M"])),
                "nameless type spuriously shadowed its container for {src:?}"
            );
        }
    }

    #[test]
    fn dotted_module_header_records_namespace_ancestors() {
        // `module A.B.C` makes the *ancestor* prefixes `A` and `A.B` namespaces
        // (the `ns_upto = len - 1` bound, pitfall 4) — the final segment `C` is
        // the module itself, not a namespace.
        let rf = resolve_one("module A.B.C\nlet x = 1\n");
        assert_eq!(
            rf.namespace_paths(),
            &[segs(&["A"]), segs(&["A", "B"])],
            "dotted module header must record ancestor namespace prefixes"
        );

        let mut pi = ProjectItems::default();
        pi.extend_with(&rf, None, false);
        assert!(pi.is_namespace(&segs(&["A"])));
        assert!(pi.is_namespace(&segs(&["A", "B"])));
        assert!(
            !pi.is_namespace(&segs(&["A", "B", "C"])),
            "the final module segment is not a namespace"
        );
        // The full path is the module header, not a namespace.
        assert!(pi.is_exact_project_module(&segs(&["A", "B", "C"])));
    }
}

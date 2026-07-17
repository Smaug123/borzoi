//! Parser for MSBuild `.fsproj` files, narrowly scoped to extracting the
//! source-file compile order an F# project would feed to the compiler.
//!
//! See `docs/completed/fsproj-parser-plan.md` for design rationale. This module
//! implements phases 1–3: a two-pass evaluation (properties finalise
//! before any item evaluates, matching MSBuild's pass ordering) that
//! pulls out
//! `<Compile>` / `<CompileBefore>` / `<CompileAfter>` items, applies
//! F#'s `CompileOrder` metadata ordering, evaluates
//! `<PropertyGroup>` writes with `$(Name)` substitution, and gates
//! every condition-bearing construct on the subset of `Condition`
//! syntax described in plan D5 (string equality with `==` / `!=`,
//! `And` / `Or`, parens, `true` / `false`, plus `$(...)` substitution
//! inside string literals). It does NOT expand globs, does NOT follow
//! `<Import>`, does NOT model property functions
//! (`$([Type]::Method(...))`), and does NOT resolve item-list or
//! metadata references. Anything we'd need one of those for emits a
//! [`Diagnostic`] and sets [`ParsedProject::is_partial`].

mod condition;
mod diagnostic;
mod evaluator;
mod imports;
mod properties;
mod sdk_resolver;
mod target_frameworks;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use diagnostic::{
    CompileConditionReason, CompileConditionUncertainty, CompileItemUncertaintyCause,
    CompileItemUncertaintyCauseKind, Diagnostic, DiagnosticKind, DiagnosticOrigin,
    ImplicitImportKind, ImportFailReason, PackageReferenceUncertaintyCause,
    PackageReferenceUncertaintyCauseKind, StructuralCompileItemUncertainty,
    StructuralPackageReferenceUncertainty,
};
pub use imports::detect_implicit_imports;
/// Escape text that entered from *outside* MSBuild's escaped-value domain — a
/// filesystem path, a computed toolset seed — so that seeding it as a property
/// value (which the evaluator unescapes on use) round-trips to the literal.
/// MSBuild does exactly this when it seeds its toolset paths. A host that
/// derives a computed property (e.g. `MSBuildUserExtensionsPath`) and hands it
/// to [`parse_fsproj_with_imports`] through the `environment` map must escape it
/// this way, or a `%`/`;`/`(` in the path would be mis-decoded.
pub use properties::escaping::escape;
pub use sdk_resolver::global_json::{
    GlobalJson, GlobalJsonError, GlobalJsonSettings, SdkPathEntry, find_global_json,
    parse_global_json,
};
pub use sdk_resolver::version_spec::{RollForward, VersionSpec};
pub use sdk_resolver::{SdkResolveError, SdkVersion, locate_dotnet_sdk, resolve_sdk, workloads};
pub use target_frameworks::target_frameworks;

/// MSBuild's boolean vocabulary (`ConversionUtilities.TryConvertStringToBool`):
/// `true`/`on`/`yes` and `false`/`off`/`no`, each optionally negated with a
/// leading `!`, all case-insensitive — anything else (including `"0"`/`"1"`
/// and whitespace-padded spellings; the conversion does not trim) is `None`.
///
/// This is the comparison the SDK's P2P protocol applies to
/// `ReferenceOutputAssembly`: the common targets admit a reference's output
/// onto `ReferencePath` only under `'%(ReferenceOutputAssembly)'=='true'`,
/// and MSBuild `==` coerces both sides through this vocabulary before
/// falling back to string equality. Probed (dotnet 10.0.301, 2026-07-10,
/// prebuilt target, entry edge): `on`/`yes`/`!false`/`TRUE` keep the DLL on
/// `ReferencePath`; `0`/`1`/`off`/`no`/`" true "`/`" false "` remove it.
pub fn msbuild_boolean(value: &str) -> Option<bool> {
    condition::parse_msbuild_bool(value)
}

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// The on-disk files an MSBuild SDK contributes to a project: the
/// `Sdk.props` MSBuild imports before the project body, the
/// `Sdk.targets` it imports after, and the directory those two live in
/// (used to resolve custom entry points like `Sdk.Web.props`). Returned
/// from an SDK resolver supplied to [`parse_fsproj_with_imports`].
///
/// `props` and `targets` are required: real .NET SDKs
/// (`Microsoft.NET.Sdk`, the Web/Worker variants, …) all ship both,
/// and a custom SDK that doesn't can point one at an empty
/// `<Project/>` stub.
///
/// `root` is the directory the walker resolves
/// `<Import Sdk="X" Project="Sdk.Web.props"/>`-style custom entry
/// points against (only `Project` values other than the two well-known
/// stems hit this path; `Sdk.props` / `Sdk.targets` always use the
/// explicit fields). By convention `root` is the directory containing
/// both well-known files, but the contract is just "the SDK's
/// import-root for custom entry points" — a resolver is free to point
/// it elsewhere as long as the relative paths in the importing fsproj
/// make sense against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdkPaths {
    pub root: PathBuf,
    pub props: PathBuf,
    pub targets: PathBuf,
}

/// What an SDK identifier resolved to. Mirrors MSBuild's own resolver
/// contract, where an SDK resolver returns a *list* of paths and
/// `<Import Project="P" Sdk="S"/>` imports `P` relative to each.
///
/// Ordinary on-disk SDKs are [`SdkResolution::Single`]: exactly one
/// root, with the two well-known entry points the `<Project Sdk="…">`
/// shorthand needs. Resolver-backed locator SDKs — today the workload
/// locators `Microsoft.NET.SDK.WorkloadManifestTargetsLocator` /
/// `Microsoft.NET.SDK.WorkloadAutoImportPropsLocator` — are
/// [`SdkResolution::Roots`]: zero or more roots, in import order, each
/// of which the import's `Project` is resolved against (zero roots
/// means the import cleanly contributes nothing, exactly as MSBuild
/// treats an empty workload-resolver result). `Roots` carries no
/// `Sdk.props`/`Sdk.targets` entry points, so it cannot back the
/// `<Project Sdk="…">` shorthand — the walker degrades if a project
/// tries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkResolution {
    /// An ordinary SDK: one root plus its canonical entry points.
    Single(SdkPaths),
    /// A locator-style SDK: `Project` resolves against each root in
    /// order; empty means "nothing to import" (not an error).
    Roots(Vec<PathBuf>),
}

impl From<SdkPaths> for SdkResolution {
    fn from(paths: SdkPaths) -> Self {
        SdkResolution::Single(paths)
    }
}

/// Resolver function the caller hands [`parse_fsproj_with_imports`] to
/// turn an SDK identifier (e.g. `"Microsoft.NET.Sdk"`) into its on-disk
/// [`SdkResolution`]. The walker dispatches on the [`SdkResolveError`]
/// variant:
///
/// - `Err(SdkResolveError::NotFound)` →
///   [`DiagnosticKind::SdkNotFound`]: the user named an SDK that
///   isn't installed (or whose well-known files aren't where we
///   expected). Remediation: install it / check the spelling.
/// - `Err(SdkResolveError::VersionNotSatisfied { .. })` →
///   [`DiagnosticKind::SdkVersionNotSatisfied`]: the SDK exists but
///   the version constraint from `global.json` (or an MSBuild
///   `Sdk="Name/Version"` pin) admits none of the installed
///   versions. Remediation: install a matching version, or relax
///   the constraint.
/// - `Err(SdkResolveError::UnsupportedLayout { .. })` →
///   [`DiagnosticKind::SdkResolutionUnsupported`]: the SDK is a
///   resolver-backed locator whose on-disk state is outside the
///   layout envelope we can resolve exactly (workload sets, install
///   state, ambiguous manifest versions, …). Remediation: none —
///   this is the deliberate "degrade, don't guess" path.
///
/// In all error cases the walker falls back to the no-SDK code path
/// (so `Directory.Build.*` is still spliced) and `is_partial` flips.
///
/// Locating SDKs requires knowing `$DOTNET_ROOT` (or running MSBuild's
/// own SDK resolver), which is policy the parser stays out of — see
/// the gospel "dependency rejection" principle. The shell decides
/// where SDKs come from; the parser only consumes the result.
pub type SdkResolver<'r> = dyn Fn(&str) -> Result<SdkResolution, SdkResolveError> + 'r;

/// A request to expand one `<Compile>` / `<ProjectReference>` element's
/// file set, handed to a caller-supplied [`GlobResolver`].
///
/// The evaluator routes an element through the resolver when its
/// `Include` (after `$(...)` expansion) contains an MSBuild wildcard
/// (`*`, `?`, `**`) or when it carries an `Exclude` attribute. Item-list
/// (`@(...)`) and metadata (`%(...)`) references are *not* the resolver's
/// concern: the evaluator diagnoses and strips them before building the
/// request, so `include` and `excludes` only ever contain literal or
/// wildcard path fragments.
pub struct GlobRequest<'a> {
    /// The entry project's directory — the base every relative fragment
    /// resolves against (MSBuild resolves item includes relative to
    /// `$(MSBuildProjectDirectory)`, even inside imported files). Always
    /// absolute (the entry `project_path` is required to be rooted).
    pub base_dir: &'a Path,
    /// The surviving `Include` spec: the `;`-joined literal/wildcard
    /// fragments in document order, `$(...)`-expanded, with `@()`/`%()`
    /// references removed. May name files that do not exist (MSBuild
    /// passes literal includes through regardless); the resolver decides.
    pub include: &'a str,
    /// The `Exclude` fragments, `$(...)`-expanded and split on `;`, in
    /// document order. Empty when the element has no `Exclude`. Applies
    /// to literal and wildcard includes alike.
    pub excludes: &'a [String],
}

/// Resolver function the caller hands [`parse_fsproj_with_imports`] to
/// expand globbing / excluding item elements into concrete files.
///
/// Returns the final, ordered list of absolute paths to splice as items
/// — each already joined onto [`GlobRequest::base_dir`], with excludes
/// applied and ordering finalised (F# compile order is load-bearing, so
/// the resolver owns a deterministic order). An empty result means the
/// element contributed no files; the evaluator emits no diagnostic for
/// that (MSBuild is silent when a glob matches nothing).
///
/// Expanding globs requires touching the filesystem (and matching
/// MSBuild's `FileMatcher` semantics), which is policy the parser stays
/// out of — see the gospel "dependency rejection" principle. When the
/// caller supplies no resolver, a wildcard `Include` surfaces as
/// [`DiagnosticKind::UnsupportedGlob`] and an `Exclude` as
/// [`DiagnosticKind::UnsupportedItemOperation`] (the phase-8 behaviour).
pub type GlobResolver<'r> = dyn Fn(&GlobRequest<'_>) -> Vec<PathBuf> + 'r;

/// Which item element produced a [`ResolvedItem`]. This is provenance, not
/// the whole ordering model: F#'s effective source order also considers
/// `CompileOrder` metadata on `<Compile>` items. `ProjectReference` lives on
/// its own bucket at [`ParsedProject::project_references`] and is *not* mixed
/// into `items` (it represents an inter-project dependency, not a Compile
/// input).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Compile,
    CompileBefore,
    CompileAfter,
    ProjectReference,
}

/// The resolution of one string-valued item metadatum (attribute plus child
/// elements, last applicable write wins).
///
/// The distinction between [`ItemMetadataValue::Known`] and
/// [`ItemMetadataValue::Unknown`] is load-bearing for consumers that make
/// *decisions* on metadata (the LSP's compile-closure walk): a write whose
/// applicability or value this evaluator cannot pin down (an unsupported
/// `Condition`, a `$(...)` expansion issue, an `@(...)`/`%(...)` reference)
/// may well take effect in the real build, so treating it as "absent" would
/// let the walk keep an edge MSBuild drops. `Unknown` forces the consumer to
/// choose a conservative reading instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemMetadataValue {
    /// Every potentially-applicable write evaluated cleanly; this is the
    /// effective value. `None` means unset, written empty, or cleared —
    /// MSBuild's `%()` reads `""` for all three.
    Known(Option<String>),
    /// The last potentially-applicable write could not be evaluated, so the
    /// real build's effective value is unknowable here. (A later write that
    /// evaluates cleanly overwrites whatever the unevaluable one did, so it
    /// restores `Known`.)
    Unknown,
}

impl ItemMetadataValue {
    /// Shorthand for `Known(Some(value))` — the shape almost every test
    /// expectation takes.
    pub fn known(value: impl Into<String>) -> Self {
        ItemMetadataValue::Known(Some(value.into()))
    }

    /// The known-absent resolution (`Known(None)`).
    pub const ABSENT: ItemMetadataValue = ItemMetadataValue::Known(None);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedItem {
    pub kind: ItemKind,
    /// `Include` value joined onto the project directory. Backslashes in
    /// the original attribute are normalised to forward slashes before
    /// joining; the result is *not* canonicalised (no filesystem touch),
    /// so `..` components may be present.
    pub include: PathBuf,
    /// `<Link>` metadata if present (as an attribute or child element).
    /// Display-only (it never changes what compiles or is referenced), so —
    /// unlike the asset-control metadata below — an unevaluable write
    /// degrades to `None` rather than carrying
    /// [`ItemMetadataValue::Unknown`].
    pub link: Option<String>,
    /// `ReferenceOutputAssembly` metadata (attribute or child element),
    /// `$(...)`-expanded, kept raw. Only populated for
    /// [`ItemKind::ProjectReference`] items — MSBuild treats a (trimmed,
    /// case-insensitive) `false` as "build dependency only": the target is
    /// built but its output never reaches the compiler's reference path.
    pub reference_output_assembly: ItemMetadataValue,
    /// `ExcludeAssets` metadata (attribute or child element),
    /// `$(...)`-expanded, kept raw (a `;`-separated asset-kind list, as on
    /// [`PackageReference`]). Only populated for
    /// [`ItemKind::ProjectReference`] items — a list containing `compile`
    /// (or `all`) excludes the reference from the consumer's compile
    /// references.
    pub exclude_assets: ItemMetadataValue,
    /// `IncludeAssets` metadata, same shape and population rules as
    /// [`ResolvedItem::exclude_assets`]. Known-absent means MSBuild's
    /// default of `all`; a list *not* covering `compile` stops the compile
    /// assets from flowing through the reference (the direct output still
    /// lands on the owner's own `ReferencePath`).
    pub include_assets: ItemMetadataValue,
    /// `PrivateAssets` metadata, same shape and population rules as
    /// [`ResolvedItem::exclude_assets`]. A list covering `compile` keeps the
    /// referenced project consumable by *this* project but stops it flowing
    /// to this project's own consumers. (MSBuild's default,
    /// `contentfiles;analyzers;build`, does not cover `compile`.)
    pub private_assets: ItemMetadataValue,
    /// `true` when a [`ItemKind::ProjectReference`] element carries P2P
    /// metadata this evaluator recognises as *significant but unmodelled* —
    /// `BuildReference`, `Targets`, `SetConfiguration`/`SetPlatform`/
    /// `SetTargetFramework`, `AdditionalProperties`, `UndefineProperties`,
    /// `GlobalPropertiesToRemove`, `SkipGetTargetFrameworkProperties` — via
    /// an attribute or an applicable child element. Probed (dotnet 10):
    /// `BuildReference="false"` and `Targets="Clean"` remove the target from
    /// `ReferencePath` outright, and the `Set*`/property-list names change
    /// which build of the target the real compiler sees, so a
    /// reference-semantics consumer must not treat the edge as a normal
    /// compile reference. Unrecognized metadata names are inert in the P2P
    /// protocol (probed) and do not set this. Always `false` on non-
    /// `ProjectReference` items.
    pub unmodelled_reference_metadata: bool,
    /// Byte span of the originating XML element in the project source.
    pub span: Range<usize>,
}

/// Whether a [`PackageReference`] targets packages via `Include` (declaring
/// a dependency) or `Update` (adjusting metadata — chiefly the version — of
/// a dependency declared elsewhere, the shape Central Package Management
/// uses). The distinction drives capture-time merging: an `Update` is folded
/// onto every *prior* `Include` of the same id (see
/// [`ParsedProject::package_references`]), so a captured [`PackageReference`]
/// exposed on a [`ParsedProject`] is always [`PackageRefOp::Include`] — the
/// `Update` variant only exists during evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageRefOp {
    Include,
    Update,
}

/// A captured `<PackageReference>` item. The version and asset-control fields
/// are evaluated strings (after `$(...)` substitution); this crate does not
/// parse versions or interpret asset lists, which is the NuGet resolver's job
/// (`borzoi-nuget`). In the exact inline Central Package Management
/// subset this evaluator can prove, `version` is the effective version after
/// applying `VersionOverride` / matching `PackageVersion`. Otherwise it is the
/// raw local `Version` metadata and `package_references_uncertain` remains set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageReference {
    /// Always [`PackageRefOp::Include`] on a [`ParsedProject`]: matching
    /// `Update` items are folded into the `Include` they modify during
    /// evaluation and never surface as standalone references.
    pub op: PackageRefOp,
    /// The package id — the `Include` value, after `$(…)` expansion and
    /// `;`-splitting (a single element per id).
    pub id: String,
    /// Effective package version when known; otherwise the local `Version`
    /// metadata (attribute or child element), `$(...)`-expanded, or `None`.
    pub version: Option<String>,
    /// `VersionOverride` metadata — under CPM, overrides the central
    /// version for this id. Raw evaluated string.
    pub version_override: Option<String>,
    /// `IncludeAssets` / `ExcludeAssets` / `PrivateAssets` metadata, kept as
    /// opaque evaluated strings (asset-list semantics are the resolver's).
    pub include_assets: Option<String>,
    pub exclude_assets: Option<String>,
    pub private_assets: Option<String>,
    /// Byte span of the originating XML element in the entry project source.
    pub span: Range<usize>,
}

/// A captured `<PackageVersion>` item from Central Package Management. The
/// version is raw evaluated metadata; parsing NuGet version ranges is left to
/// `borzoi-nuget`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageVersion {
    /// The package id from `Include`, after `$(...)` expansion and
    /// `;`-splitting.
    pub id: String,
    /// `Version` metadata (attribute or child element), `$(...)`-expanded.
    pub version: Option<String>,
    /// Byte span of the originating XML element in the entry project source.
    pub span: Range<usize>,
}

/// A captured `<GlobalPackageReference>` item from Central Package Management.
/// It contributes a package reference implicitly; the NuGet resolver decides
/// how to fold it into the effective dependency set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalPackageReference {
    /// The package id from `Include`, after `$(...)` expansion and
    /// `;`-splitting.
    pub id: String,
    /// Raw evaluated package metadata.
    pub version: Option<String>,
    pub version_override: Option<String>,
    pub include_assets: Option<String>,
    pub exclude_assets: Option<String>,
    pub private_assets: Option<String>,
    /// Byte span of the originating XML element in the entry project source.
    pub span: Range<usize>,
}

/// A captured `<FrameworkReference>` item (e.g.
/// `Microsoft.AspNetCore.App`). Only the name is modelled — a framework
/// reference resolves to a shared-framework/targeting pack, not a NuGet
/// package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameworkReference {
    /// The `Include` value, after `$(…)` expansion and `;`-splitting.
    pub name: String,
    /// Byte span of the originating XML element in the entry project source.
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedProject {
    /// Compile items in compile order:
    /// `[Compile CompileOrder=CompileFirst…, CompileBefore…, Compile
    /// CompileOrder=CompileBefore…, Compile…, Compile CompileOrder=CompileAfter…,
    /// CompileAfter…, Compile CompileOrder=CompileLast…]`. Within each
    /// effective category, document order is preserved. `ProjectReference`
    /// items are *not* in this list — they live on
    /// [`Self::project_references`].
    pub items: Vec<ResolvedItem>,
    /// `<ProjectReference Include="...">` items in document order.
    /// Each `include` is the path to a referenced project file (csproj,
    /// fsproj, etc.), resolved against the entry project's directory
    /// the same way `<Compile>` includes are. The
    /// [`ResolvedItem::link`] field is always `None` here — MSBuild
    /// does not treat `<Link>` as meaningful on a ProjectReference.
    pub project_references: Vec<ResolvedItem>,
    /// `true` if [`Self::project_references`] may diverge from MSBuild's
    /// evaluated item list. Consumers deriving *reference semantics* from
    /// the list — the LSP's compile-closure walk — must not trust it when
    /// set. The causes, each verified against `dotnet build`/`dotnet
    /// msbuild` probes:
    ///
    /// - A `<ProjectReference Update="…">` or `Remove="…">` that may run
    ///   (this evaluator does not model item mutation, so earlier
    ///   `Include`s stand un-mutated in the list). "May run" includes a
    ///   mutation behind a condition we could not evaluate — on the element
    ///   itself, on its enclosing `<ItemGroup>`, or an undecided
    ///   `<Choose>`/`<When>` chain — since the real build may execute it (a
    ///   probed `Update` writing `ReferenceOutputAssembly=false`, or a
    ///   `Remove`, really does strip the reference from `ReferencePath`).
    /// - A gate decided only by an *unpinned* property (one written under a
    ///   condition we couldn't evaluate) or an SDK-tainted read — or an
    ///   `Include` **value** expanded from one (the captured path may hold a
    ///   different target, or none, in the real build): the real build may
    ///   take the other branch, running a skipped mutation or dropping a
    ///   captured `Include` (a phantom edge). A cleanly-decided gate over
    ///   undefined-but-never-written properties is trusted: under the
    ///   environment model (`extra_properties` ARE the environment) those
    ///   are genuinely unset. (Untrusted reads in *metadata* gates or
    ///   values degrade that item's metadata to
    ///   [`ItemMetadataValue::Unknown`] instead of flipping this flag.)
    /// - An `<ItemDefinitionGroup>` declaring `<ProjectReference>` metadata
    ///   defaults: probed, the default lands on every ProjectReference item
    ///   (regardless of document order) and e.g. a
    ///   `ReferenceOutputAssembly=false` default empties `ReferencePath`,
    ///   while the captured items still read as full references.
    /// - A user-authored `<Import>` we could not follow (unresolved,
    ///   failed, or gated on an untrusted condition): the imported file may
    ///   carry any of the above.
    ///
    /// Deliberately out of contract: `<Target>`-time item mutation. This
    /// field describes the *evaluated* (static) item list; whether a target
    /// runs is a build-graph question no static evaluator can settle, and
    /// the same boundary already applies to Compile items from targets.
    ///
    /// A corresponding diagnostic ([`DiagnosticKind::UnsupportedItemOperation`],
    /// [`DiagnosticKind::UnsupportedCondition`],
    /// [`DiagnosticKind::UnresolvedImport`], …) is emitted alongside.
    pub project_references_uncertain: bool,
    /// The effective `<PackageReference>` dependency set in document order,
    /// after collapsing `Update` items onto the `Include` they modify: each
    /// `<PackageReference Update="X" …>` overwrites the matching (case-
    /// insensitive) metadata of every `Include="X"` declared *before* it, and
    /// a lone `Update` matching no prior `Include` is dropped — mirroring
    /// MSBuild's evaluated item view. Every entry is therefore
    /// [`PackageRefOp::Include`]. The direct dependencies the NuGet resolver
    /// folds over; when [`Self::package_references_uncertain`] is set this list
    /// may not faithfully reflect the project's package set — the resolver
    /// should decline rather than resolve a possibly-wrong closure.
    pub package_references: Vec<PackageReference>,
    /// `<PackageVersion>` items in document order. These are central package
    /// versions, not direct dependencies on their own.
    pub package_versions: Vec<PackageVersion>,
    /// `<GlobalPackageReference>` items in document order.
    pub global_package_references: Vec<GlobalPackageReference>,
    /// `<FrameworkReference>` items in document order.
    pub framework_references: Vec<FrameworkReference>,
    /// Project-defined properties, i.e. names assigned by
    /// `<PropertyGroup>` elements after `$(...)` expansion. Reserved
    /// MSBuild well-known properties and caller-supplied
    /// `extra_properties` are deliberately *excluded* from this map so
    /// callers can distinguish project-side state from inputs they
    /// already know about. Both are still consulted during substitution.
    pub properties: HashMap<String, String>,
    /// F# preprocessor symbols from the evaluated `$(DefineConstants)`,
    /// parsed as a `;`-separated list with whitespace trimmed and empty
    /// segments dropped. Order and duplicates from the source value are
    /// preserved — callers wanting set semantics (e.g. feeding the
    /// preprocessor) can collect into a `HashSet`.
    ///
    /// Unlike [`Self::properties`] this *does* incorporate
    /// `extra_properties` (MSBuild global properties — e.g. when the
    /// caller passes `DefineConstants=DEBUG` to model `-p:DefineConstants=DEBUG`),
    /// so the list reflects the effective evaluated value rather than
    /// only what the project file itself wrote. Property-name lookup is
    /// case-insensitive (MSBuild semantics); individual segment casings
    /// are preserved (F# preprocessor symbols are case-sensitive).
    pub define_constants: Vec<String>,
    /// The declared target frameworks, in document order: `<TargetFrameworks>`
    /// split into entries, else `<TargetFramework>` as a single entry, else
    /// empty. Read it through [`target_frameworks`], which is the supported
    /// accessor.
    ///
    /// Computed by the evaluator rather than derived from [`Self::properties`],
    /// because the split is on the **escaped** value: `net8.0%3bnet9.0` is one
    /// (bogus) framework whose name contains a semicolon, not two
    /// (oracle-pinned 2026-07-12), and `properties` has already been unescaped
    /// for its consumers.
    pub target_frameworks: Vec<String>,
    /// The evaluated `$(LangVersion)` (the project's `<LangVersion>`), trimmed,
    /// or `None` when unset/empty. Raw text — `"8.0"`, `"latest"`, `"preview"`,
    /// `"11"`, etc. — left for the consumer to resolve (e.g. the LSP maps it via
    /// `borzoi_cst::language_version::LanguageVersion::from_lang_version_text`).
    /// Sourced from the same merged `lookup` as [`Self::define_constants`], so it
    /// reflects globals/`extra_properties`, not only what the project file wrote.
    pub lang_version: Option<String>,
    /// The evaluated output-file base name — MSBuild writes
    /// `$(TargetName)$(TargetExt)`, and `TargetName` defaults to
    /// `$(AssemblyName)` (probed, dotnet 10.0.301, 2026-07-10:
    /// `<AssemblyName>Identity</AssemblyName><TargetName>FileName</TargetName>`
    /// produces `FileName.dll`; a padded `<AssemblyName> Padded </AssemblyName>`
    /// produces ` Padded .dll`, so the value is reported **verbatim**, never
    /// trimmed). Carries trust semantics like reference metadata, because a
    /// consumer locating an output DLL by this name fabricates when the name
    /// is wrong:
    ///
    /// - [`ItemMetadataValue::Known`]`(Some(name))`: the effective
    ///   `TargetName` (or, unset, `AssemblyName`), and the deciding
    ///   property's provenance was trustworthy under the environment model.
    /// - [`ItemMetadataValue::Known`]`(None)`: neither set (or set to empty)
    ///   — MSBuild's default applies (`$(MSBuildProjectName)`, the
    ///   project-file stem).
    /// - [`ItemMetadataValue::Unknown`]: the deciding value leans on an
    ///   *unpinned* property (written under a gate we couldn't evaluate) or
    ///   an SDK-package-tainted read, still contains unexpanded `$(...)`, or
    ///   is whitespace-only (a degenerate spelling MSBuild honours in the
    ///   filename but whose default gate `'$(TargetName)' == ''` does NOT
    ///   fire for) — the real build's name may differ; decline rather than
    ///   guess.
    ///
    /// Sourced from the merged `lookup` (same as [`Self::lang_version`]), so a
    /// caller-supplied global participates.
    pub target_name: ItemMetadataValue,
    /// Lowercased names of properties whose end-of-evaluation value
    /// provenance is **untrusted**: the stored value (or a gate it sat
    /// behind) leaned on an *unpinned* property or an SDK-package-tainted
    /// read, so the real build may hold a different value even though the
    /// captured one expanded cleanly. Consumers reading an evaluated
    /// property with reference semantics (e.g. `TargetFramework` deciding
    /// which output directory to fold) must check
    /// [`Self::property_provenance_untrusted`] and decline rather than
    /// trust the captured spelling. Undefined-but-never-written properties
    /// are absent (the environment model resolves them exactly).
    pub untrusted_properties: std::collections::HashSet<String>,
    pub diagnostics: Vec<Diagnostic>,
    /// `true` if any diagnostic was emitted — i.e. the result may diverge
    /// from what MSBuild itself would produce, including in ways that don't
    /// affect the Compile item set (a skipped `<Target>`, an undefined
    /// property in an imported SDK target, a property function we don't model).
    /// For "can I trust the *Compile order*?" — the question a consumer folding
    /// over `items` for name resolution actually asks — use the narrower
    /// [`Self::items_uncertain`].
    pub is_partial: bool,
    /// `true` if the resolved [`Self::items`] set may diverge from MSBuild *in
    /// which source files it contains* — the correctness-relevant subset of
    /// [`Self::is_partial`]. Set when a `<Compile>`/`<CompileBefore>`/
    /// `<CompileAfter>` item or its `<ItemGroup>` couldn't be included/excluded
    /// faithfully: an unmodeled or undefined-property condition gating one
    /// ([`Self::compile_condition_uncertainties`]), an ignored source-set item
    /// operation (`Remove`/`Exclude`; metadata-only `Update` is harmless unless
    /// it changes `CompileOrder`), an
    /// unexpanded glob or item/metadata reference in an `Include`, a skipped
    /// `<Choose>` (which can carry items), or a failed import / unresolved SDK
    /// that could contribute items.
    ///
    /// Unlike [`Self::is_partial`], it stays `false` for divergences that
    /// cannot change the Compile set — the common case for real SDK projects,
    /// whose imported targets always emit harmless property/`<Target>`
    /// diagnostics. A consumer that needs a trustworthy Compile order (and
    /// otherwise falls back to per-file handling) should gate on this, not
    /// `is_partial`.
    ///
    /// **SDK provenance.** Compile-affecting uncertainty in the entry SDK's own
    /// installation tree is *tolerated* (not counted), because that tree's
    /// conditional default-item machinery is present in every project and never
    /// decides which hand-written sources compile. The same uncertainty in the
    /// entry project or a user-authored import (`Directory.Build.*`, an explicit
    /// `<Import>`) is respected.
    ///
    /// **Known gaps** (deliberately *not* flagged; each is an "under-resolve,
    /// possibly wrong in a rare contrived case" rather than a common hazard):
    /// - A user property defined from an undefined reference (so it evaluates to
    ///   `""`) and then consumed by a `<Compile Include="$(That)/X.fs">`: the
    ///   include resolves to a wrong path with no diagnostic at the include
    ///   site (the undefinedness was upstream). Closing this needs taint
    ///   tracking we don't do.
    /// - A user `<Import>` skipped by an *unsupported/undefined condition*
    ///   (rather than an unresolved path or a missing file, both of which *are*
    ///   flagged) could hide Compile items.
    /// - A `<Target>` that mutates `@(Compile)` (adds/removes items at build
    ///   time) is *deliberately* not flagged. We never run targets, so a
    ///   target-added source is invisible — but this is the common, intended
    ///   pattern for *generated* sources (the SDK's `AssemblyInfo`, Nerdbank
    ///   GitVersioning's `ThisAssembly`, source generators). Flagging would
    ///   disable resolution for every such project; instead we under-resolve
    ///   the generated file and keep the hand-written ones working.
    pub items_uncertain: bool,
    /// The specific Compile items/groups whose inclusion couldn't be
    /// determined from their `Condition` — the explainable subset of why
    /// [`Self::items_uncertain`] is set. Empty unless a Compile-gating
    /// condition was undefined-property-laden or unmodeled. See
    /// [`CompileConditionUncertainty`].
    pub compile_condition_uncertainties: Vec<CompileConditionUncertainty>,
    /// Concrete events that made [`Self::items_uncertain`] true. This is the
    /// diagnostic/debug channel for corpus runners and LSP fallbacks: unlike the
    /// broad [`Self::diagnostics`] list, every entry here is causally tied to
    /// Compile item uncertainty.
    pub compile_item_uncertainties: Vec<CompileItemUncertaintyCause>,
    /// `true` if the resolved package/framework dependency inputs
    /// ([`Self::package_references`], [`Self::package_versions`],
    /// [`Self::global_package_references`], [`Self::framework_references`])
    /// may diverge from MSBuild in *which dependencies it contains, or their
    /// versions/metadata*. A consumer building the dependency closure should
    /// decline and fall back when this is set. The captured references are
    /// still returned — only their trustworthiness is in question.
    ///
    /// Set when a package-affecting item (`<PackageReference>`,
    /// `<PackageVersion>`, `<GlobalPackageReference>`, `<FrameworkReference>`)
    /// or its `<ItemGroup>` couldn't be resolved faithfully: an unmodeled or
    /// undefined-property condition gating one, an ignored `Remove`, an
    /// unexpanded/wildcarded `Include`, an unexpanded item/metadata reference
    /// in metadata, an `Exclude`/item-definition default we can't reduce, or a
    /// dropped structural container (`<Import>`/SDK/`<Choose>`) that could carry
    /// references.
    ///
    /// Unlike [`Self::items_uncertain`], SDK-tree dependency risks are **not**
    /// tolerated: the SDK is exactly where implicit dependency references live
    /// (`Microsoft.NETCore.App`, implicit `FSharp.Core`). Followed SDK files
    /// are evaluated through the same package-reference machinery as user
    /// files — every property (SDK files included) finalises before any item
    /// evaluates, so a cleanly-evaluated SDK dependency item is captured
    /// exactly and does *not* set this flag. What does: an unresolvable SDK
    /// (structural), and any SDK construct the walker genuinely cannot pin
    /// down — a condition or property write leaning on an undefined property,
    /// an unsupported expression, a dropped import — each adding its own
    /// concrete cause below.
    pub package_references_uncertain: bool,
    /// Concrete events that made [`Self::package_references_uncertain`] true.
    /// This is the diagnostic/debug channel for corpus runners and LSP
    /// fallbacks: unlike the broad [`Self::diagnostics`] list, every entry here
    /// is causally tied to package/framework-reference uncertainty.
    pub package_reference_uncertainties: Vec<PackageReferenceUncertaintyCause>,
    /// `true` if the evaluated [`Self::define_constants`] may diverge from
    /// MSBuild — a *user-authored* `<DefineConstants>` write (or the
    /// `<PropertyGroup>` condition gating one) relied on a property we couldn't
    /// resolve (e.g. a `'$(TargetFramework)' == …`-conditioned define in a
    /// multi-targeted project). The preprocessor-symbol analogue of
    /// [`Self::items_uncertain`]: a consumer that folds files under these `#if`
    /// symbols (cross-file name resolution) should refuse and fall back when
    /// this is set, since the wrong branches would otherwise be taken.
    ///
    /// Like `items_uncertain`, uncertainty *inside the SDK's own files* is
    /// tolerated — we already don't model the framework defines (`NET6_0`, …)
    /// the SDK injects in targets, so our `define_constants` is a consistent
    /// (if framework-define-free) view rather than a per-branch-corrupt one.
    ///
    /// **Accepted limitation — SDK-supplied defines are not modelled.** Because
    /// we never run SDK targets, the standard SDK F# defines (`DEBUG`/`TRACE`,
    /// set via `Choose`/property-function syntax in `Microsoft.FSharp.NetSdk.props`
    /// / the FSharp targets shim, and the per-TFM `NET6_0`/`NETCOREAPP…` family)
    /// are absent and **not** flagged here. This is the same incompleteness the
    /// single-file resolution path already has — `#if DEBUG`/`#if NET6_0` resolve
    /// to the branch taken when those symbols are *undefined*. Modelling them
    /// would require replicating the SDK's per-config/per-TFM implicit-define
    /// logic; flagging them would force single-file fallback for **every** SDK
    /// project (they all set these), defeating cross-file resolution. We
    /// deliberately accept the bounded divergence instead — it is consistent
    /// across single-file and cross-file resolution. Only *user-authored*
    /// define uncertainty (an unevaluable condition, an unsupported/`@()`/`%()`
    /// value reference) is flagged.
    pub define_constants_uncertain: bool,
    /// The [`SdkPaths::root`] of the entry project's **own** SDK — its
    /// `<Project Sdk="X">` shorthand or the equivalent promoted explicit form
    /// (`<Import Sdk="X" Project="Sdk.props"/>` as the first body element).
    /// `None` when the entry declares no SDK of its own.
    ///
    /// Deliberately narrow: an SDK-less entry may still pull an SDK in via a
    /// nested `<Import>` or a `Directory.Build.{props,targets}` helper, but
    /// *which* of those establishes the framework is subtle and
    /// order-dependent, so this stays `None` rather than guess — the consumer
    /// should fall back to its own default-root discovery there.
    ///
    /// This is the SDK *import directory*, not the .NET install root; a
    /// consumer that wants the install (for `packs/` etc.) recovers it from
    /// this path's known layout.
    pub resolved_sdk_root: Option<PathBuf>,
}

impl ParsedProject {
    /// Whether `name`'s end-of-evaluation value provenance is untrusted —
    /// see [`Self::untrusted_properties`]. Case-insensitive (MSBuild
    /// property names compare OrdinalIgnoreCase). `false` for a property
    /// that was never written: under the environment model those are
    /// genuinely unset, so reading them as absent/empty is exact.
    pub fn property_provenance_untrusted(&self, name: &str) -> bool {
        self.untrusted_properties
            .contains(&name.to_ascii_lowercase())
    }
}

/// Parse the `DefineConstants` MSBuild value into the F# preprocessor's
/// symbol list. Used by the evaluator at project-exit time to populate
/// [`ParsedProject::define_constants`].
/// The `DefineConstants` list, built the way MSBuild builds a list out of a
/// property: split on the semicolons of the **escaped** value, then decode each
/// fragment. So `A%3bB` is the single define `A;B`, not two
/// (oracle-pinned 2026-07-12) — decoding first would split it.
pub(crate) fn define_constants_from_escaped(
    value: &crate::properties::escaping::Escaped,
) -> Vec<String> {
    value
        .split_list()
        .map(|fragment| fragment.unescape().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[derive(Debug)]
pub enum ParseError {
    /// The project file is not well-formed XML.
    Xml(roxmltree::Error),
    /// `project_path` had no filesystem root (`/foo` on Unix, `C:\foo`
    /// or `\\server\share\foo` on Windows). Every Include is resolved
    /// by joining against the project directory, and
    /// `$(MSBuildProjectDirectory)` substitutes into Include attributes
    /// — so a path without a root would silently double-join the
    /// directory component into the final path. We use [`Path::has_root`]
    /// rather than [`Path::is_absolute`] so that rooted-but-not-drive-
    /// qualified paths (`/foo` on Windows) — which still *replace* on
    /// `PathBuf::join`, so don't double-join — are accepted.
    RelativeProjectPath(PathBuf),
    /// `extra_properties` contained a key that names an MSBuild
    /// reserved property — path-derived (`MSBuildProjectName`,
    /// `MSBuildProjectDirectory`, …) or toolset (`MSBuildToolsPath`,
    /// `MSBuildToolsVersion`, …). MSBuild rejects this at the CLI
    /// (`MSB4177: …property name is reserved`); we do the same rather
    /// than let the caller's value silently displace the seed and break
    /// every `$(MSBuildProjectName)` reference. The stored name is the
    /// caller's original casing.
    ReservedPropertyInExtras(String),
    /// `extra_properties` contained two keys that compare equal under
    /// MSBuild's `OrdinalIgnoreCase` property-name comparison
    /// (e.g., `Configuration` and `configuration`). The substitution
    /// map is keyed case-insensitively, and `HashMap` iteration order
    /// is unspecified — picking one silently would let
    /// `$(Configuration)` resolve to a different value across runs.
    /// Stored as a lexicographically-sorted pair so the error is
    /// deterministic regardless of iteration order.
    DuplicateExtraProperty { first: String, second: String },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Xml(e) => write!(f, "malformed XML: {e}"),
            ParseError::RelativeProjectPath(p) => {
                write!(
                    f,
                    "project_path must be rooted (have a filesystem root), got {}",
                    p.display()
                )
            }
            ParseError::ReservedPropertyInExtras(name) => {
                write!(
                    f,
                    "extra_properties contains the reserved MSBuild property name {name:?}"
                )
            }
            ParseError::DuplicateExtraProperty { first, second } => {
                write!(
                    f,
                    "extra_properties contains case-insensitive duplicate keys {first:?} and {second:?} (MSBuild property names compare OrdinalIgnoreCase)"
                )
            }
        }
    }
}

impl Error for ParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ParseError::Xml(e) => Some(e),
            ParseError::RelativeProjectPath(_) => None,
            ParseError::ReservedPropertyInExtras(_) => None,
            ParseError::DuplicateExtraProperty { .. } => None,
        }
    }
}

impl From<roxmltree::Error> for ParseError {
    fn from(e: roxmltree::Error) -> Self {
        ParseError::Xml(e)
    }
}

/// Parse a `.fsproj` source string and extract its Compile items.
///
/// `project_path` is used both to resolve `Include` paths relative to
/// the project file's directory and to seed the well-known MSBuild
/// path properties (`MSBuildProjectDirectory`, `MSBuildProjectName`,
/// etc.). **Must be absolute** — see
/// [`ParseError::RelativeProjectPath`]. No filesystem access is
/// performed.
///
/// `extra_properties` plays the role of MSBuild's *global properties*:
/// values supplied here are visible to substitution and cannot be
/// overridden by the project file's own `<PropertyGroup>` writes
/// (matching MSBuild's command-line-property semantics).
///
/// `environment` plays the role of the process environment MSBuild
/// promotes into initial properties. Its semantics differ from
/// `extra_properties` exactly as MSBuild's differ (all pinned against
/// `dotnet msbuild` 10.0.300): environment-backed properties are
/// readable from the start, are **overridable** by project property
/// writes, and lose to global properties of the same name. Skipped and
/// left undefined: reserved names, toolset-computed names MSBuild
/// overwrites after promotion (`MSBuildToolsPath` and friends), and
/// names with a case-insensitive collision in the snapshot (MSBuild's
/// winner is unspecified).
///
/// `MSBuildExtensionsPath` is the one name whose fate depends on the
/// *toolset*: MSBuild ≤ 17 (SDK 8 and 9) promotes it and then overwrites it
/// with the toolset's own directory, while MSBuild 18 (SDK 10) lets the
/// environment value stand — where it steers `Sdk.props`'s import of
/// `Microsoft.Common.props`. The walker learns which toolset applies only when
/// an SDK resolves to the canonical dotnet layout, so through this entry point
/// — which resolves no SDK — an environment-supplied `MSBuildExtensionsPath`
/// is left undefined rather than committed to either behaviour. Callers that
/// want it honoured must go through [`parse_fsproj_with_imports`] with an SDK
/// resolver.
///
/// Pass the (filtered) environment the real
/// build would run under; an empty map means "no environment
/// variables", which is itself a claim — with the exact-undefined-read
/// model, a name absent from every input reads as exactly empty
/// wherever the walk has stayed exact.
pub fn parse_fsproj(
    source: &str,
    project_path: &Path,
    extra_properties: &HashMap<String, String>,
    environment: &HashMap<String, String>,
) -> Result<ParsedProject, ParseError> {
    validate_inputs(project_path, extra_properties)?;
    let doc = roxmltree::Document::parse(source)?;
    Ok(evaluator::walk(
        &doc,
        project_path,
        extra_properties,
        environment,
    ))
}

/// Filesystem-touching counterpart to [`parse_fsproj`]: follows explicit
/// `<Import Project="...">` references (substituting `$(...)`, resolving
/// relative paths against the importing file's directory, depth-checked,
/// with duplicate imports — cycles included — silently skipped exactly
/// as MSBuild skips them under warning MSB4011/MSB4210) and splices in
/// the nearest implicit `Directory.Build.props` (before the project
/// body) and `Directory.Build.targets` (after) discovered by
/// [`detect_implicit_imports`].
///
/// Failures during follow (file missing, malformed XML, depth limit,
/// IO error) are reported via [`DiagnosticKind::ImportFailed`].
/// Successful follows produce *no* diagnostic — the merged result is
/// itself the report. In particular,
/// [`DiagnosticKind::ImplicitImportPresent`] is **never** emitted by
/// this entry point; that diagnostic is a property of the pure
/// "what did we skip" surfacing in [`detect_implicit_imports`].
///
/// `sdk_resolver`, if supplied, turns the project's `<Project Sdk="X">`
/// shorthand (and `<Import Sdk="X" Project="Sdk.props|Sdk.targets" />`
/// elements inside the body) into on-disk [`SdkPaths`] the walker can
/// splice in. When the **root** carries the `Sdk` attribute and the
/// resolver returns `Some`, the walker interleaves the SDK's pair
/// with the Directory.Build.* pair, matching MSBuild's effective
/// ordering:
///
/// ```text
/// Sdk.props → Directory.Build.props → body
///           → Directory.Build.targets → Sdk.targets
/// ```
///
/// The Directory.Build.* splice stays live even under a resolved SDK so this
/// evaluator owns the implicit import point directly rather than relying on
/// deeper SDK files to rediscover and import the same files. Splicing
/// `Sdk.props` first is what makes properties the SDK sets (e.g.
/// `UsingMicrosoftNETSdk`) visible to a `Directory.Build.props` that
/// conditions on them. When the resolver returns `None`, a
/// [`DiagnosticKind::SdkNotFound`] diagnostic is emitted and the body
/// still gets the Directory.Build.* splice. When `sdk_resolver` itself
/// is `None`, any `Sdk` attribute surfaces as
/// [`DiagnosticKind::UnsupportedConstruct`] (the phase-7a behaviour).
///
/// **Explicit-only SDK projects.** A project using the *explicit*
/// form `<Project><Import Sdk="X" Project="Sdk.props"/>...</Project>`
/// with no root `Sdk` attribute is handled by a body pre-scan: the
/// first unconditional `<Import Sdk="X" Project="Sdk.props"/>` (and
/// the matching trailing `Sdk.targets`, if present) is promoted to
/// the same OUTERMOST splice positions the root-`Sdk` shorthand uses,
/// so `Directory.Build.props` observes SDK-supplied properties just
/// as it would under the shorthand. Promotion is conservative — it
/// only fires when `Sdk.props` is the first element child of root
/// (and `Sdk.targets` the last), so it never reorders user-visible
/// work; anything else is walked at its in-body position.
///
/// **Nested SDK roots in imported files.** When an `<Import>` is
/// chased into a file whose own `<Project>` carries `Sdk="X"`, that
/// SDK is resolved through the same resolver and its
/// `Sdk.props` / `Sdk.targets` are spliced before / after the
/// imported file's body. The `Directory.Build.*` splice is still done
/// exactly once (MSBuild walks ancestor dirs once from the entry
/// project's location, not around each imported file). Spans for
/// SDK-contributed items/diagnostics collapse to the entry project's
/// `<Import>` site, like any other imported content.
///
/// MSBuild imports `Directory.Build.props` exactly once, right after
/// whichever `Sdk.props` first re-enters `Microsoft.Common.props`.
/// When the entry project carries (or promotes) its own SDK, that is
/// the entry `Sdk.props` and the entry splice above already puts
/// `Directory.Build.props` after it. When the entry project sets no
/// such SDK but a *nested* imported `<Project Sdk="X">` does, the first
/// `Sdk.props` is that nested one (mid-body): the walker defers the
/// entry `Directory.Build.props` and fires it right after the nested
/// `Sdk.props`, so a `Directory.Build.props` that conditions on (or
/// substitutes) a nested-SDK-set property such as
/// `$(UsingMicrosoftNETSdk)` observes it — matching MSBuild. For an entry
/// with no SDK the walker always runs this deferred second pass: it cannot
/// decide cheaply from the eager first pass whether the faithful order
/// reaches a nested `Sdk.props`, because that pass runs
/// `Directory.Build.props` in the wrong (before-body) position and a
/// property it sets can itself flip the very `<Import>` that leads to the
/// nested SDK. At the deferred fire point the import gate
/// (`ImportDirectoryBuildProps`), the `DirectoryBuildPropsPath` override,
/// and the resolved path are all re-evaluated against live state — exactly
/// where MSBuild evaluates them — so a body/SDK property set before the
/// nested `Sdk.props` (disabling the import or redirecting it) is honoured.
/// The splice fires at most once.
///
/// Because the deferred import is fired and resolved at MSBuild's own
/// position, the second pass is the faithful model: body imports before
/// the nested `Sdk.props` don't see `Directory.Build.props` and those
/// after it do, even when that flips a conditional `<Import>`. The one
/// shape it cannot model is a *dangle* — see below — where the walker
/// falls back to the historical before-body splice.
///
/// Two residual approximations remain in this nested case, both rare:
///   * **Pathological gate (dangle).** If the very `<Import>` that
///     reaches the nested SDK is itself conditioned on a property that
///     *only* `Directory.Build.props` sets, deferring it suppresses the
///     nested SDK too, so the deferred splice never fires. (MSBuild can't
///     bootstrap that config either, but emitting *no*
///     `Directory.Build.props` is a surprising regression.) The walker
///     detects the unconsumed splice and falls back to the before-body
///     position rather than drop the file.
///   * **Non-promoted explicit body imports.** An explicit-form
///     `<Import Sdk="X" Project="Sdk.props"/>` that is *not* promoted
///     (because it is conditional or not the first/last element child)
///     runs at its in-body position; the deferred `Directory.Build.props`
///     repositioning keys on nested *root* `<Project Sdk=…>` files, not
///     these in-body SDK imports.
///
/// **Glob expansion.** When `glob_resolver` is supplied, an item
/// element whose `Include` contains an MSBuild wildcard (or that carries
/// an `Exclude`) is routed through it: the evaluator builds a
/// [`GlobRequest`] (project dir + ref-stripped include spec + split
/// excludes) and splices the returned paths verbatim, each as an item of
/// the element's kind. When it is `None`, a wildcard `Include` surfaces
/// as [`DiagnosticKind::UnsupportedGlob`] and an `Exclude` as
/// [`DiagnosticKind::UnsupportedItemOperation`] — see [`GlobResolver`].
///
/// Same input validation contract as [`parse_fsproj`]: `project_path`
/// must be rooted, and `extra_properties` must neither name a reserved
/// MSBuild property nor contain case-insensitive duplicates.
/// `environment` carries the same semantics as on [`parse_fsproj`]
/// (overridable environment-backed properties; reserved names ignored).
pub fn parse_fsproj_with_imports(
    source: &str,
    project_path: &Path,
    extra_properties: &HashMap<String, String>,
    environment: &HashMap<String, String>,
    sdk_resolver: Option<&SdkResolver<'_>>,
    glob_resolver: Option<&GlobResolver<'_>>,
) -> Result<ParsedProject, ParseError> {
    validate_inputs(project_path, extra_properties)?;
    // Detection only — we follow the props/targets ourselves and
    // suppress the per-kind `ImplicitImportPresent` diagnostic that
    // `detect_implicit_imports` would otherwise add to the result.
    // `DirectoryPackagesProps` is *not spliced* here: NuGet's own
    // `NuGet.props` (reached through `Sdk.props` →
    // `Microsoft.Common.props` when an SDK resolves to the canonical
    // dotnet layout) owns that import point, and the walker evaluates
    // those real files. The detected path is passed down so the walk
    // keeps the conservative "central versions not folded in" cause
    // whenever it never reached the file (no resolver, non-SDK project,
    // or a break anywhere along the chain).
    let mut implicit_props = None;
    let mut implicit_targets = None;
    let mut cpm_props_path = None;
    for diag in imports::detect_implicit_imports(project_path) {
        if let DiagnosticKind::ImplicitImportPresent { path, kind } = diag.kind {
            match kind {
                ImplicitImportKind::DirectoryBuildProps => implicit_props = Some(path),
                ImplicitImportKind::DirectoryBuildTargets => implicit_targets = Some(path),
                ImplicitImportKind::DirectoryPackagesProps => cpm_props_path = Some(path),
            }
        }
    }
    let doc = roxmltree::Document::parse(source)?;
    Ok(evaluator::walk_with_imports(
        &doc,
        project_path,
        extra_properties,
        environment,
        implicit_props.as_deref(),
        implicit_targets.as_deref(),
        cpm_props_path.as_deref(),
        sdk_resolver,
        glob_resolver,
    ))
}

/// Input-validation shared between the pure and with-imports entry
/// points. Both reject the same set of malformed `extra_properties`
/// and non-rooted project paths up front, before any XML parsing.
fn validate_inputs(
    project_path: &Path,
    extra_properties: &HashMap<String, String>,
) -> Result<(), ParseError> {
    // We check `has_root`, not `is_absolute`. The hazard we're
    // guarding against is `project_dir.join(Include)` double-joining
    // the project directory into the Include path — that only happens
    // when the joined-onto base is *relative*. On Windows, `/foo` is
    // rooted-but-not-absolute (no drive prefix); `PathBuf::join`
    // replaces the base when joining a rooted path, so it doesn't
    // double-join, and we shouldn't reject it. `is_absolute` would
    // reject it, breaking tests and Windows callers with no upside.
    if !project_path.has_root() {
        return Err(ParseError::RelativeProjectPath(project_path.to_path_buf()));
    }
    // Property lookup is OrdinalIgnoreCase, but `HashMap<String,_>` is
    // case-sensitive — so the caller can hand us `Configuration` and
    // `configuration` as two distinct entries, then `State::new`
    // inserts whichever HashMap iteration happens to visit last,
    // making `$(Configuration)` non-deterministic across runs. Reject
    // collisions up front. We sort the pair so the error message is
    // stable regardless of iteration order.
    let mut seen: HashMap<String, &String> = HashMap::with_capacity(extra_properties.len());
    for key in extra_properties.keys() {
        let lower = key.to_ascii_lowercase();
        if let Some(prior) = seen.get(&lower) {
            let (first, second) = if prior.as_str() <= key.as_str() {
                ((*prior).clone(), key.clone())
            } else {
                (key.clone(), (*prior).clone())
            };
            return Err(ParseError::DuplicateExtraProperty { first, second });
        }
        seen.insert(lower, key);
    }
    // MSBuild treats the path-derived properties as reserved; even
    // command-line `-p:MSBuildProjectName=...` errors out with MSB4177.
    // Mirror that strictness, since silently honouring the override
    // would make every `$(MSBuildProjectName)` reference (and friends)
    // resolve against the caller's value instead of the actual path.
    let reserved = properties::well_known(project_path);
    let mut reserved_lower: std::collections::HashSet<String> = reserved
        .canonical_keys()
        .map(|k| k.to_ascii_lowercase())
        .collect();
    // The toolset names the evaluator seeds at SDK resolution are just as
    // reserved as the path-derived ones (`dotnet msbuild
    // -p:MSBuildToolsVersion=Foo` errors); accepting them would let a
    // caller global redirect the SDK's own
    // `$(MSBuildExtensionsPath)\$(MSBuildToolsVersion)\…` imports.
    for name in [
        "msbuildtoolspath",
        "msbuildbinpath",
        "msbuildtoolsversion",
        "msbuildruntimetype",
        // The ChangeWaves threshold property: MSBuild derives it from
        // the MSBUILDDISABLEFEATURESFROMVERSION environment variable
        // and rejects both project writes and property injection
        // ("property is reserved, and cannot be modified").
        "msbuilddisablefeaturesfromversion",
    ] {
        reserved_lower.insert(name.to_string());
    }
    for key in extra_properties.keys() {
        if reserved_lower.contains(&key.to_ascii_lowercase()) {
            return Err(ParseError::ReservedPropertyInExtras(key.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod with_imports_tests;

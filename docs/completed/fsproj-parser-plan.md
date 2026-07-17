# .fsproj parser plan

> **Status: parser complete; consumption is the open work.** Phases 1–9
> — including 9b, the filesystem-backed glob resolver — and the standalone
> D9 target-framework enumeration have all landed (see per-phase markers
> below). The parser also already runs *at runtime* inside the LSP: every
> per-file lookup evaluates the owning `.fsproj` via
> `parse_fsproj_with_imports`, wired with both the SDK resolver
> ([`crates/lsp/src/sdk_discovery.rs`](../../crates/lsp/src/sdk_discovery.rs))
> and the glob resolver
> ([`crates/lsp/src/glob_resolver.rs`](../../crates/lsp/src/glob_resolver.rs)).
>
> What the LSP does **not** yet consume is the parser's headline output.
> Of the `ParsedProject` fields, only `define_constants` (→ preprocessor
> symbols, in [`crates/lsp/src/workspace.rs`](../../crates/lsp/src/workspace.rs))
> and `diagnostics` (→ `.fsproj`-buffer squiggles, in
> [`crates/lsp/src/fsproj_diagnostics.rs`](../../crates/lsp/src/fsproj_diagnostics.rs))
> are used. The ordered `items` (the Compile list) and `project_references`
> are recomputed on every evaluation and discarded. Wiring those two into
> behaviour is the remaining work, tracked separately in
> [`fsproj-consumption-plan.md`](../fsproj-consumption-plan.md).

Design doc for a parser that reads an MSBuild `.fsproj` file and produces the
ordered list of source files the F# compiler would compile, along with byte
spans into the project XML so the LSP can map results back to declarations.
Captures decisions made before implementation started so future work can
resume from a cold pickup.

## Scope

- **Input.** Text of a single `.fsproj` file, its on-disk path, and a
  caller-supplied property bag (e.g. `Configuration=Release`,
  `TargetFramework=net8.0`).
- **Output.** An ordered `Vec<ResolvedItem>` (Compile / CompileBefore /
  CompileAfter), each carrying its resolved path and a byte span into the
  XML, plus a list of `Diagnostic`s for anything we couldn't faithfully
  evaluate.
- **Non-goal.** Becoming an MSBuild reimplementation. We deliberately fail
  loudly on constructs we don't model, never silently producing the wrong
  list.

## Settled decisions

### D1. XML parsing: third-party crate with byte spans

Default to `roxmltree` (read-only DOM, spans on every node, ergonomic for a
small structural projection). Fall back to `quick-xml` if we ever need
streaming. Either is fine for this scope.

Rejected:
- **Hand-roll an XML lexer.** Speculative generality; no advantage over a
  maintained crate for fsproj-shape XML.

### D2. Output shape: small typed data, not a green tree

The F# source parser uses rowan because trivia retention + incremental
reparse matter for editor-grade interaction. `.fsproj` files are small,
parsed whole, and never reformatted by us; only spans are needed for LSP
navigation. A plain struct beats green/red here.

```rust
pub struct ParsedProject {
    pub items: Vec<ResolvedItem>,           // compile order: Before -> Main -> After
    pub properties: HashMap<String, String>,
    pub diagnostics: Vec<Diagnostic>,
    pub is_partial: bool,                   // true if we hit something we couldn't model
}

pub struct ResolvedItem {
    pub kind: ItemKind,                     // Compile | CompileBefore | CompileAfter
    pub include: PathBuf,                   // joined onto project dir, normalised
    pub link: Option<String>,
    pub span: std::ops::Range<usize>,       // span of the <Compile .../> element
}

pub fn parse_fsproj(
    source: &str,
    project_path: &Path,
    extra_properties: &HashMap<String, String>,
) -> Result<ParsedProject, ParseError>;
```

Pure function. No filesystem access in the core — callers that want
`Directory.Build.props` handling pass the text in themselves (see D8).

### D3. Fail loudly on unsupported constructs

Each of the following produces a `Diagnostic` with a span, and sets
`is_partial = true`; nothing is silently dropped or silently included:

- `<Import Project="..."/>` (and the implicit SDK imports) → `UnresolvedImport`.
- `<Choose>/<When>/<Otherwise>` → `UnsupportedConstruct`.
- Glob characters (`*`, `?`, `**`) in `Include` → `UnsupportedGlob`
  *when no `glob_resolver` is supplied* (see phase 9a). When a resolver
  is supplied, glob/Exclude item specs are routed through it instead and
  no diagnostic is raised.
- `$(Undefined)` reference → `UndefinedProperty`.
- Condition syntax we don't model (function calls like `Exists(...)`, item
  refs `@(...)`, arithmetic) → `UnsupportedCondition`.
- Ancestor `Directory.Build.props` / `.targets` /
  `Directory.Packages.props` detected (separate helper, opt-in) →
  `ImplicitImportPresent`.

Hard errors (malformed XML, unbalanced tags) return `Err(ParseError)` —
there's nothing useful a caller can do with a partial XML tree.

### D4. Property substitution (minimal MSBuild semantics)

Single forward pass over `<PropertyGroup>` children in document order. For
each property, substitute `$(Name)` using:

1. The property bag built so far in this pass.
2. The caller's `extra_properties`.
3. A small seed of well-known properties derived from `project_path`:
   `MSBuildThisFile`, `MSBuildThisFileDirectory`, `MSBuildProjectName`,
   `MSBuildProjectDirectory`, `MSBuildProjectExtension`,
   `MSBuildProjectFullPath`.

Then walk `<ItemGroup>` elements, expanding `$(...)` in `Include` and
`Condition`. This handles the common case
(`<Compile Include="$(IntermediateOutputPath)…"/>`) without re-evaluation
loops.

Self-reference (`<Foo>$(Foo);bar</Foo>`) uses the prior value, as MSBuild
does. Forward references (a property defined later in the file) produce an
`UndefinedProperty` diagnostic, deliberately — we don't do a fixed-point
pass.

### D5. Condition evaluator (tiny, explicit grammar)

Purpose-built parser + evaluator for MSBuild conditions, restricted to:

- String literals `'…'` (with `$(...)` expansion inside).
- `==`, `!=` (string equality, case-insensitive per MSBuild).
- `And`, `Or`, parens.
- Bare `true` / `false`.

Anything else (`Exists`, `HasTrailingSlash`, arithmetic, item references)
→ `UnsupportedCondition`. The containing `<PropertyGroup>` /
`<ItemGroup>` is treated as **excluded** and the project is marked partial.
That way we never silently include items that should have been gated out.

Enough for FSharp.Core's `Condition="'$(Configuration)' == 'Proto'"` style.

### D6. Item kinds we model

- `Compile` — the main case.
- `CompileBefore`, `CompileAfter` — F#-specific, used by FSharp.Core.
- `ProjectReference` — inter-project dependency (csproj/fsproj of a
  sibling project). Captured because the LSP needs the dependency
  graph to drive metadata-only builds via the C# sidecar; landed in
  a separate `project_references` bucket on `ParsedProject` so it is
  never silently mixed into `items` (Compile inputs vs. dependencies
  are different categories of fact).
- Ignored silently (no diagnostic): `EmbeddedResource`, `None`, `Content`,
  `PackageReference`, `Reference`. They don't affect source compile
  order or the inter-project dependency graph we model.
- `Update` / `Remove` on `Compile` → `UnsupportedItemOperation` diagnostic
  (rare in fsproj; primarily an SDK-default-exclusion lever, which doesn't
  apply for F# since the SDK doesn't inject Compile defaults).

`ResolvedItem.kind` carries the originating XML item kind; `items` is returned
in the effective F# source order, including `CompileOrder` metadata on
`<Compile>` items. Callers can filter by `kind` if they need provenance.
`project_references` is its own field (document order within), not interleaved
into `items`.

### D7. Module layout

```
crates/msbuild/src/
  lib.rs           # public API: parse_fsproj{,_with_imports}, types
  condition.rs     # condition lexer + parser + evaluator
  evaluator.rs     # property/item walk (pure + with-imports)
  imports.rs       # detect_implicit_imports + lexical path helper
  properties.rs    # PropertyMap, well_known seeds, $(...) substitution
  diagnostic.rs    # Diagnostic / DiagnosticKind / ImportFailReason
```

Separate from `crates/cst/src/parser/` (F# source parsing). No shared code
worth abstracting now.

### D8. Diagnostics and spans

Each `Diagnostic` carries a kind + byte `Range<usize>` + optional context
(e.g. the unresolved property name). Row/column conversion is the LSP
layer's job, using the same `LineMap` pattern already used elsewhere; the
fsproj core stays in byte offsets.

**Pure vs. IO split.** `parse_fsproj` is filesystem-free: it walks
the supplied XML string, emits `UnresolvedImport` for every
`<Import>` it sees, and emits no `ImplicitImportPresent` (it doesn't
know what's on disk). `parse_fsproj_with_imports` (phase 7a) and the
free function `detect_implicit_imports` are the IO-touching entry
points; they live alongside the pure core but the type system
doesn't enforce the split. Callers that need deterministic /
sandboxable evaluation should stick with `parse_fsproj` and probe
explicitly with `detect_implicit_imports` if they want to know what
they're missing.

### D9. Target framework enumeration

A separate, policy-free entry point `target_frameworks(&ParsedProject) -> Vec<String>`
reports the TFMs the project *declares*, after the usual `$(…)` substitution
and condition evaluation have run. Preference order matches MSBuild's
outer/inner build dance: `<TargetFrameworks>` (plural, semicolon-separated)
wins when it resolves to a non-empty list; otherwise `<TargetFramework>`
(singular) is returned as a one-element vec; otherwise empty.

**Why enumeration only, not selection.** The natural follow-up
("pick *one* TFM for this build / for the LSP to ask the sidecar about")
mixes policy with the parse view, and the right answer depends on the
caller. The LSP layer might want "auto-pick if exactly one, error
otherwise"; a CLI consumer might want "default to first, allow
override"; a `dotnet build --framework` invocation wants whatever the
user typed. Keeping `target_frameworks` enumeration-only matches the
[`find_global_json`](../../crates/msbuild/src/sdk_resolver/global_json.rs) /
[`parse_global_json`](../../crates/msbuild/src/sdk_resolver/global_json.rs) split:
discovery is one concern, selection another. Consumers that need a
single TFM build that policy on top of `target_frameworks(&p)`.

**Why the plural-wins, singular-fallback policy.** MSBuild's own SDK
targets gate on `'$(TargetFrameworks)' == ''` to mean "treat as
undeclared and use TargetFramework instead." Empty after substitution
and trimming is operationally indistinguishable from "not present" —
so a conditional plural that evaluates to nothing falls through to the
singular value rather than shadowing it. Doubled or trailing
semicolons drop out for the same reason (the canonical
`$(MyOptionalTfm);$(TargetFrameworks)` idiom relies on this).

**Why a `&ParsedProject` parameter, not `&Path`.** The caller is
already parsing the project for other reasons (Compile items,
implicit-import detection); making `target_frameworks` consume the
parsed shape avoids re-parsing and keeps the dependency graph one-way:
parse first, query views afterwards. Gospel principle 1 (local
reasoning): the function does one thing, takes the value it needs, and
returns the answer.

**Oracle.** [`tests/fsproj_target_frameworks_diff.rs`](../../crates/msbuild/tests/fsproj_target_frameworks_diff.rs)
runs `dotnet msbuild -getProperty:TargetFrameworks,TargetFramework`
against the vendored F# corpus and asserts byte-equal enumeration for
the three fixtures that exercise multi-TFM shapes (`FSharp.Core` Proto
+ Release, `FSharp.Compiler.Service` Release). The FCS case primes
`FSharpNetCoreProductTargetFramework=net10.0` on both sides to
sidestep an orthogonal import-resolution gap (`eng/TargetFrameworks.props`
isn't chased end-to-end yet); that priming is the only deviation from
plain MSBuild equivalence and is documented inline.

**Broad corpus oracle.** [`tests/fsproj_msbuild_corpus_diff.rs`](../../crates/msbuild/tests/fsproj_msbuild_corpus_diff.rs)
is an ignored, focused MSBuild corpus runner. It recursively discovers real
`.fsproj` files, evaluates them through `parse_fsproj_with_imports` with SDK
resolution wired from `global.json` / `$DOTNET_ROOT` / `$NUGET_PACKAGES`, and
diffs the modelled facets against `dotnet msbuild -getItem/-getProperty`.
Unlike the fixed fixture tests, it reports skipped facets separately from
matches, so an uncertain Compile or package set is not counted as evidence.
For `DefineConstants`, MSBuild-only extras are accepted only when every extra
symbol is a known SDK-injected constant (`DEBUG`/`TRACE` or a TFM/platform
symbol), so project-authored omissions still surface as divergences.
Run it under `nix develop` with:

```text
cargo test -p borzoi-msbuild --test fsproj_msbuild_corpus_diff -- --ignored --nocapture
```

Use `BORZOI_MSBUILD_CORPUS` (default fallback: `BORZOI_CORPUS`) or
`BORZOI_MSBUILD_PROJECT_LIST`, plus
`BORZOI_MSBUILD_EXHAUSTIVE=1` / `BORZOI_MSBUILD_LIMIT` /
`BORZOI_MSBUILD_REPORT_JSONL`, to control sweep breadth and reporting.
The explicit project list takes precedence over the fallback `BORZOI_CORPUS`
that `nix develop` sets; it conflicts only with an explicit
`BORZOI_MSBUILD_CORPUS`, and is never sampled. The stride/limit knobs apply
only to discovered corpora.
The default ratchets are strict (`MAX_DIVERGENCES=0`, `MAX_ERRORS=0`,
`MIN_COMPARED_PROJECTS=1`) so the runner can be used as a real gate when a
project list is known-good.

## Phased implementation

One PR per phase. Each adds types, evaluator code, and snapshot tests
against fsproj from the vendored corpus.

1. **Scaffold + lexical extract (done).** Add the XML crate dep. `crates/msbuild/src/`
   scaffolded. `parse_fsproj` walks document order, collects `<Compile>`
   items unconditionally, joins paths to project dir, emits
   `UnsupportedConstruct` for `<Import>` / `<Choose>`. Snapshot tests
   against ~5 simple fsproj from the corpus (e.g. `fcs-dump.fsproj` plus a
   handful from the F# corpus's `tests/`).
2. **Property substitution (done).** Property walk + `$(...)` expansion in
   `Include` + well-known properties seeded from path.
   `UndefinedProperty` diagnostic when expansion fails. Snapshot tests
   pinning substitution behaviour.
3. **Condition evaluator (done).** New `condition.rs`. Apply conditions to
   `<PropertyGroup>` and `<ItemGroup>`. Caller-supplied properties as the
   override layer. Property tests for the evaluator: equivalence of two
   implementations, or against a small hand-written truth table for the
   primitive cases.
4. **F# source ordering (done).** Recognise `CompileBefore` /
   `CompileAfter` and F#'s `CompileOrder` metadata; emit the effective
   source order the `FSharpSourceCodeCompileOrder` target builds. Snapshot
   tests against `FSharp.Core.fsproj` with `Configuration=Release` and with
   `Configuration=Proto` should produce sensible non-duplicate lists.
5. **Implicit-import detection (done)** (separable helper, not in core).
   `fn detect_implicit_imports(project_path: &Path) -> Vec<Diagnostic>`
   walks ancestors for `Directory.Build.props` / `.targets` /
   `Directory.Packages.props`; emits `ImplicitImportPresent` so callers
   know the result may be incomplete. Does *not* parse them. Reports
   only the **nearest** match per kind (matching MSBuild's
   "first-ancestor wins" semantics). Diagnostics carry a `0..0` span
   because the discovery is out-of-band — there's no source location
   in the project XML.
6. **(Optional) Differential test against `dotnet msbuild` (done).** Gated on
   `dotnet` being on PATH; produces an oracle Compile list per fsproj via
   `dotnet msbuild -getItem:Compile`. Validates against the vendored
   corpus. Skipped silently when `dotnet` is missing.
7. **`<Import>` support.** Split into two slices because SDK
   resolution is structurally different from path following.
   - **7a (done):** Follow explicit `<Import Project="…">` —
     `$(...)` substitution in the path, importer-relative resolution,
     cycle detection (canonicalised path on a visited stack), depth
     limit (`MAX_IMPORT_DEPTH = 64`), and `MSBuildThisFile` /
     `MSBuildThisFileDirectory` rebinding for the duration of each
     follow. Also splices in the nearest `Directory.Build.props`
     (before the project body) and `Directory.Build.targets` (after)
     discovered by `detect_implicit_imports`. New public entry point
     `parse_fsproj_with_imports`; pure `parse_fsproj` keeps its
     IO-free contract (D8) and still emits `UnresolvedImport` for
     every `<Import>` it encounters. `<Import Sdk="…">` surfaces as
     `UnsupportedConstruct` — see 7b.
   - **7b-v0 (done):** SDK attribute resolution via a caller-supplied
     `SdkResolver`: `Option<&dyn Fn(&str) -> Option<SdkPaths>>` on
     `parse_fsproj_with_imports`. When the project root carries
     `Sdk="X"` and the resolver returns `Some(paths)`, the walker
     interleaves the SDK's pair with the `Directory.Build.*` pair —
     `Sdk.props → Directory.Build.props → body →
     Directory.Build.targets → Sdk.targets` — matching MSBuild's
     effective order (MSBuild itself pulls `Directory.Build.props`
     from inside `Microsoft.Common.props` *after* the SDK has set
     its own properties, but the import sits behind an `Exists(...)`
     gate this evaluator treats as unsupported, so the splice has
     to be done explicitly). `<Import Sdk="X"
     Project="Sdk.{props,targets}" />` is the explicit form — same
     resolver, walked at its body position. Resolver returning
     `None` emits `SdkNotFound`; the body still gets the
     `Directory.Build.*` splice so we produce a best-effort result.
     No resolver supplied preserves the phase-7a
     `UnsupportedConstruct` behaviour. Imported files that themselves
     declare `<Project Sdk="...">` surface as `UnsupportedConstruct`
     (splicing for imported SDK roots is v1 scope).

     **Known v0 limitation.** A project using the explicit form
     `<Project><Import Sdk="X" Project="Sdk.props"/>...</Project>`
     with *no* root `Sdk` attribute still gets
     `Directory.Build.props → Sdk.props → ...` rather than the
     MSBuild-correct `Sdk.props → Directory.Build.props → ...`. The
     root-`Sdk` form (recommended by Microsoft, used by every project
     in the .NET corpus tests) is unaffected. Closing this gap
     requires pre-scanning the body for explicit SDK imports;
     deferred to v1.

     Custom-SDK entry points other than `Sdk.props` / `Sdk.targets`
     (e.g. `Sdk.Web.props`) are deferred to 7b-v1 — they surface as
     `UnsupportedConstruct`. The resolver itself (locating SDKs under
     `$DOTNET_ROOT/sdk/...`) is shell policy; this slice only exposes
     the seam.
   - **7b-v1a (done):** First production resolver, exported as
     `locate_dotnet_sdk(dotnet_root, sdk_name) -> Option<SdkPaths>`.
     Walks `{dotnet_root}/sdk/{version}/Sdks/{sdk_name}/Sdk/` for
     `Sdk.props` *and* `Sdk.targets`, picking the highest installed
     SDK version whose directory contains both files. Version
     ordering: numeric tuple compare with trailing zeros normalised
     out (so `8.0` == `8.0.0`); stable releases beat any prerelease
     (`-suffix`) at the same numeric prefix; lex compare within
     prereleases (`-rc.2` beats `-preview.1`). `dotnet_root` is an
     explicit input — discovery (`$DOTNET_ROOT` env var, `dotnet
     --info` shelling, etc.) stays in the LSP shell per the gospel
     "dependency rejection" principle.
   - **7b-v1b (done):** Multi-TFM SDK variants —
     `Microsoft.NET.Sdk.Web`, `Microsoft.NET.Sdk.Worker`,
     `Microsoft.NET.Sdk.Razor`. The directory layout is identical to
     v1a (`{dotnet_root}/sdk/{version}/Sdks/{name}/Sdk/`) and the
     resolver / evaluator both treat the `Sdk` attribute as an
     opaque key, so v1a's plumbing already routes these correctly.
     Covered by the `variant_sdks_resolve_to_their_own_directories`
     unit test (variants installed side-by-side resolve to disjoint
     directories) and `end_to_end_resolver_splices_variant_sdk`
     (the evaluator wires a variant SDK's `Sdk.props` /
     `Sdk.targets` through `parse_fsproj_with_imports`).
   - **7b-v1c (done):** Wider scope.
     - **Done:** `global.json` and `rollForward` semantics so the
       resolver honours pinned SDK versions instead of always
       picking the highest. NuGet fallback
       (`~/.nuget/packages/{sdk-name}.../Sdk/`) for custom SDKs,
       gated on a per-import pin or `msbuild-sdks` entry as the
       version source. `global.json` `msbuild-sdks` map honoured as
       a project-wide version source for unversioned `<Project
       Sdk="…"/>` references. Custom-SDK entry points other than
       `Sdk.{props,targets}` (e.g. `Sdk.Web.props`) resolved against
       a new `SdkPaths::root` field: `<Import Sdk="X" Project="…"/>`
       with a non-canonical stem joins the attribute against the
       SDK root and walks the result through the existing import
       pipeline. Path components are vetted before any FS touch
       (`..`, empty segments, and absolute paths are rejected as
       `UnsupportedConstruct`); a missing-but-well-formed entry
       point surfaces as `ImportFailed::NotFound` via the standard
       IO path. Splicing for explicit-only SDK projects (no root
       `Sdk`): a body pre-scan (`find_explicit_sdk_promotion`)
       hoists the first unconditional `<Import Sdk="X"
       Project="Sdk.props"/>` (and its matching `Sdk.targets`
       companion, if present) to the OUTERMOST splice positions, so
       `Directory.Build.props` sees the same SDK-supplied properties
       it would under the root-`Sdk` shorthand. The body-walk skip
       list (`hoisted_sdk_imports`) keeps cycle detection from
       re-flagging the promoted nodes. Splicing for nested SDK roots
       inside imported files: when `walk_external_file` chases an
       `<Import>` into a file whose own `<Project>` carries
       `Sdk="X"`, it now resolves that SDK through the same
       `resolve_project_sdk` the entry project uses and splices
       `Sdk.props` before / `Sdk.targets` after the imported file's
       body. The Directory.Build.* splice still fires exactly once
       (MSBuild walks ancestor dirs once from the entry project's
       location, not around each imported file); the recursive
       `walk_external_file` calls reuse the existing cycle/depth/
       import-site-span machinery, so SDK-contributed items collapse
       to the entry project's `<Import>` site. `Directory.Build.props`
       is now *position-faithful* for the nested case: MSBuild imports
       it right after the *first* `Sdk.props` to run, so when the entry
       project has no SDK of its own but a nested imported
       `<Project Sdk="X">` does, the walker defers the entry
       `Directory.Build.props` and fires it right after that nested
       `Sdk.props` — a `Directory.Build.props` that conditions on (or
       substitutes) a nested-SDK-set property such as
       `$(UsingMicrosoftNETSdk)` now observes it. Detection is a
       *trial walk* (`walk_with_imports` runs `walk_once` up to twice)
       rather than a bespoke pre-scan, so it can never disagree with the
       real evaluator about which imports fire; `take()` on the stashed
       splice keeps it single-import. At the deferred fire point the
       import gate (`ImportDirectoryBuildProps`), the
       `DirectoryBuildPropsPath` override, and the resolved path are all
       re-evaluated against live state — exactly where MSBuild evaluates
       them — so a body/SDK property set before the nested `Sdk.props`
       (disabling or redirecting the import) is honoured. Pass 2 is thus
       the faithful model and is returned whenever the shape is detected.
       Two rare residual approximations remain: (a) the *pathological
       gate (dangle)* — the `<Import>` reaching the nested SDK is itself
       gated on a property only `Directory.Build.props` sets, so deferral
       suppresses the nested SDK and the splice never fires; the walker
       detects the unconsumed splice and falls back to the before-body
       position rather than drop the file; (b) *non-promoted explicit
       body imports* — an in-body
       `<Import Sdk="X" Project="Sdk.props"/>` that promotion declined
       (conditional, or not first/last child) is not a nested *root*
       and so does not trigger the repositioning.
     - **Done:** .NET-10 `global.json` `sdk.paths` field (with
       `$host$`) for repo-local SDK installs. The `global.json` schema
       carries it as `GlobalJsonSettings::paths: Option<Vec<SdkPathEntry>>`
       where `SdkPathEntry` is `Host` (the exact-case `"$host$"` token)
       or `Relative(String)`; absent/`null` stays the host-only default
       and an explicit empty array is the strict opt-out. Consumption
       lives in the LSP's imperative shell, not in `locate_dotnet_sdk`:
       the msbuild primitive stays single-root (dependency rejection),
       and `SdkDiscovery` instead carries an ordered `roots:
       Vec<PathBuf>`. `expand_sdk_paths` projects the field — `$host$`
       expands to the discovered host root (via a restricted
       `resolve_host_dotnet_root` that never runs `dotnet --info`, so
       the workspace's own `sdk.paths` can't feed back into `$host$`;
       skipped with a log line when the host can't be found), `Relative`
       entries join against the `global.json` file's directory, and
       `paths: []` (or an all-`$host$` list with no host) yields zero
       roots so every lookup is `NotFound`. `resolve_across_roots`
       iterates the roots first-satisfying-root-wins and, on all-error
       outcomes, returns the dedup-sorted union of `VersionNotSatisfied`
       availabilities; it is extracted as a pure fold and pinned by a
       reference-fold property test. The `global.json` `sdk.version` /
       `rollForward` spec applies per root. Residual approximation: when
       `rollForward` could roll across roots, the .NET host may prefer
       the highest satisfying version across *all* roots whereas we take
       the first root with any satisfying version — a defensible,
       documented choice that doesn't affect the LSP's
       `Sdk.props`/`Sdk.targets` lookup in practice.
8. **ProjectReference tracking (done).** Extend the item walker to
   recognise `<ProjectReference Include="...">` under the same
   evaluation rules as `<Compile>` (`$(...)` substitution, `;`
   splitting, backslash normalisation, condition gating, items from
   imported files visible to the merged result). Results land on a
   new `ParsedProject::project_references` field — *not* mixed into
   `items` — so downstream consumers (the C# sidecar driver, an
   eventual binder) can walk the inter-project dependency graph
   without confusing it with Compile inputs. `<Link>` is dropped for
   ProjectReference because MSBuild does not treat it as significant
   there. Snapshot corpus refreshed; bundled e2e test now asserts the
   ProjectReference resolves to the sibling csproj rather than
   deriving the path from the fixture layout.
9. **Glob expansion.**
   - **Done (9a — the seam).** `parse_fsproj_with_imports` takes an
     optional `glob_resolver: Option<&GlobResolver<'_>>`, where
     `GlobResolver = dyn Fn(&GlobRequest<'_>) -> Vec<PathBuf>`. A
     `GlobRequest` carries the entry project directory (`base_dir`, always
     absolute), the `;`-joined surviving include fragments (`include`),
     and the split, expanded `Exclude` list (`excludes`). The item walk
     routes any spec with a glob character (`*`, `?`, `**`) *or* an
     `Exclude` attribute through the resolver, splicing the returned
     absolute paths back into the correct compile bucket
     (Before/Main/After/ProjectReference) with the element's span and
     `Link`. `@(...)` item refs and `%(...)` metadata refs inside an
     Include are still diagnosed and stripped before the request is built.
     Pure literal includes with no `Exclude` keep the fast path (no
     resolver call). With **no** resolver supplied, behaviour is
     unchanged: globs raise `UnsupportedGlob` and `Exclude` raises
     `UnsupportedItemOperation`. The resolver owns deterministic ordering
     (load-bearing for F# compile order), which is why it stays a caller
     seam rather than living in the dep-light core.
   - **Done (9b — a real resolver).** A filesystem-backed,
     `FileMatcher`-style resolver lives in the LSP shell
     ([`crates/lsp/src/glob_resolver.rs`](../../crates/lsp/src/glob_resolver.rs))
     and is wired into `parse_fsproj_with_imports` on every project
     evaluation ([`crates/lsp/src/workspace.rs`](../../crates/lsp/src/workspace.rs)).
     The dep-light core stays pure; the LSP shell owns the filesystem walk
     and the deterministic ordering (load-bearing for F# compile order).
     Oracle:
     [`crates/lsp/tests/glob_msbuild_diff.rs`](../../crates/lsp/tests/glob_msbuild_diff.rs)
     diffs the resolver's output against real `dotnet msbuild`.

## Testing strategy

- **Snapshot tests** over the F# corpus's `**/*.fsproj` (~20+ real
  projects). Pin the resolved compile order and the diagnostics produced.
  Re-running the suite after any change shows what shifted.
- **Property tests** on the condition evaluator. Equivalence of two
  implementations, or against an independent oracle.
- **Hand-written unit tests** for each diagnostic kind, each well-known
  property, and each item kind.
- **Optional MSBuild oracle** (phase 6) for end-to-end confidence on the
  vendored corpus.

## Risks carried forward

- **F# source ordering is target-mediated.** FSharp.Core routes between
  `CompileBefore` (Proto config) and `Compile` (other configs) via
  condition, and the F# SDK also honours `CompileOrder` metadata on
  `<Compile>` items. The parser now models those static ordering rules;
  generated target-time additions remain intentionally out of scope.
- **Property evaluation order.** Real MSBuild iterates to fixed point in
  some scenarios. We do one forward pass. The snapshot corpus will tell
  us if this bites in practice.
- **`Update` / `Remove` items.** SDK-style projects sometimes use these to
  tweak SDK defaults. Since the F# SDK doesn't inject Compile defaults
  the impact is small, but worth verifying against the corpus.
- **The SDK attribute is a hidden import.** `<Project Sdk="…">` morally
  imports `Sdk.props` and `Sdk.targets`. For F# fsproj the practical
  impact on the Compile list is normally nil, but we should at minimum
  emit an info-level diagnostic so users aren't misled about completeness.
- **Empty global properties are modelled as read-only (resolved).**
  Caller-supplied globals (via `extra_properties`) with an empty string
  value are now sticky-empty: MSBuild's default-fill assignments in
  `Microsoft.Common.props` cannot write through a global, so the value
  stays `""` and the downstream condition does not flip. The
  `Directory.Build.*` import gates consult a dedicated, immutable
  `State::sticky_globals` set (names supplied as globals minus the entry
  project's `TreatAsLocalProperty` opt-outs) via
  `should_import_default_true` and `resolve_directory_build_path`. The
  formerly-divergent cases now match `dotnet msbuild`:
  - `ImportDirectoryBuildProps=""` / `ImportDirectoryBuildTargets=""`
    as globals: the gate stays `""`, so
    `<Import Condition="'$(...)' == 'true'">` is false and the implicit
    import is **skipped** (R8-2, R9-A).
  - `DirectoryBuildPropsPath=""` / `DirectoryBuildTargetsPath=""` as
    globals: the path stays empty, so `Exists('$(...)')` is false and
    the import is **skipped** with no fallback to the discovered file
    (R9-B).
  Only the empty *global* case changed; unset, body-written-empty, and
  non-empty global values are unchanged. Body-written empties (rare)
  still default-fill (fall through to the discovered file / `true`
  gate), which is arguably more correct since project-side writes to a
  non-reserved property *can* be overwritten by the default-fill. A
  fuller `Microsoft.Common.props` emulation remains out of scope; the
  targeted provenance set covers the divergences observed in practice.

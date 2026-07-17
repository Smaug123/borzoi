# Plan: resolving FSharp.Core auto-opened names (`printfn`, operators, `List.map`)

> **Status: the value surface is landed; operators remain.** The `printfn`
> milestone (A1/A2 + S1/S2), A3/S3 (assembly-level `[<AutoOpen>]` driving the
> implicit opens), and the Slice-D extension-member soundness fix are all
> implemented and green. Still open: **A4/S4** (operator demangling,
> `op_Addition` ⇒ `+`) and the pickle-only "later" list in
> [§Still to do](#still-to-do). Everything below the "Landed stages" list has
> detail *only* on what remains.

## Mechanism (reference)

The framing was "mature the unpickler so sema can resolve `printfn`."
Investigation showed the unpickler was **not** the blocker: everything needed to
resolve FSharp.Core's auto-opened *value/function/operator* surface was
recoverable from plain ECMA-335 metadata that `borzoi-assembly` already
read (source names from `CompilationSourceName`, module-suffix strip from
`CompilationRepresentation(ModuleSuffix)`, auto-open flags, assembly-level
`[<AutoOpen>]`). FCS reads F# references from the pickle, but for *name
resolution* the IL view plus a few attributes suffices. (Since the milestone the
authoritative F# projection has moved to the signature pickle for source-name,
extension, and measure facts — Stream 2, below — but that does not change the
original conclusion.)

Resolution chain for an unqualified `printfn`: FSharp.Core's manifest carries
assembly-level `[<AutoOpen("Microsoft.FSharp.Core")>]` (read by FCS's
`ApplyAssemblyLevelAutoOpenAttributeToTcEnv`, no hardcoded list); opening that
namespace brings in the `[<AutoOpen>]` module `ExtraTopLevelOperators`, whose
member `PrintFormatLine` carries `[<CompilationSourceName("printfn")>]`. Sema
mirrors this: auto-opening a module is the same operation as `open type Module`,
feeding `opened_static_member`, which defers soundly on cross-open ambiguity.

The assembly-level AutoOpen set on FSharp.Core v10.1 (verified by reflection):

```
AutoOpen("Microsoft.FSharp")
AutoOpen("Microsoft.FSharp.Core")
AutoOpen("Microsoft.FSharp.Collections")
AutoOpen("Microsoft.FSharp.Control")
AutoOpen("Microsoft.FSharp.Core.LanguagePrimitives.IntrinsicOperators")   ← a *module* path, not a namespace
AutoOpen("Microsoft.FSharp.Control.TaskBuilderExtensions.{Low,LowPlus,Medium,High}Priority")
AutoOpen("Microsoft.FSharp.Linq.QueryRunExtensions.{Low,High}Priority")
```

Note an AutoOpen path may name a **namespace** *or* a **module**
(`…IntrinsicOperators`). The namespace-shaped entries are opened; the
module-shaped ones surface only operators (A4/S4) and *extension members*, and
are deliberately **not** opened yet — see [§Still to do](#still-to-do).
`ListModule`/`ArrayModule`/`SeqModule` are `[<RequireQualifiedAccess>]`, hence
`List.map`, never bare `map`.

## Landed stages (one line each)

Stage IDs (A = `borzoi-assembly`, S = `borzoi-sema`) are cited from
`crates/assembly/tests/all/assembly_auto_opens.rs` and
`crates/sema/src/assembly_env.rs`; keep them.

- **A1 + A2 + S1 + S2** (PR #539) — the `printfn` milestone.
  A1: `MethodLike::source_name` from `CompilationSourceNameAttribute` (IL `name`
  preserved for the Roslyn differential). A2: `Entity::source_name` from
  `CompilationRepresentation(ModuleSuffix)` (strip `"Module"`, `ListModule` ⇒
  `List`). S1: auto-open modules of an opened namespace fold into `open_types`.
  S2: member/entity lookup keys on `source_name` when present. Compiler-generated
  `$W` witness twins filtered (`is_fsharp_witness_duplicate`); generic module
  members kept.
- **A3 + S3** (PR #869) — assembly-level `[<AutoOpen>]` drives the implicit opens.
  A3: `EcmaView::assembly_auto_opens` (decoded from the manifest's attribute
  rows, mirroring FCS's `TryFindAutoOpenAttr`; single-string-arg contributes,
  else skip-not-error). S3: `AssemblyEnv::implicit_open_namespace_paths` (built by
  `record_assembly_auto_opens`) feeds `implicit_open_namespaces` in
  `resolve/state.rs`; the old hardcoded three-namespace list survives only as a
  deduped fallback for envs whose FSharp.Core stand-in carries no manifest
  attributes. FCS facts baked in and oracle/fsi-verified: `Microsoft` is
  prepended for FSharp.Core itself (makes `FSharp.Collections.List.map` resolve);
  per-assembly deref (a path is resolved within its contributing CCU); a
  duplicate re-establishes latest-open precedence (dedup keeps the last
  occurrence); contested namespaces (a sibling declares the same name) are
  dropped entirely — a data-driven entry applies only when no sibling declares
  the namespace, so env-wide ≡ contributor-scoped. Module-shaped AutoOpen paths
  are deliberately not opened (see below).
- **Slice D** (PR #916) — the extension-member D5 soundness fix. Bare
  `Force`/`Create` no longer resolve into an auto-open module's extension
  augmentations where FCS reports FS0039. The rule as the fsi oracle reports it:
  FCS admits *no* extension member to the unqualified environment — F#-native
  augmentations (instance or static) are FS0039 bare *and* qualified; C#-style
  `[<Extension>]` statics are FS0039 bare but resolve qualified; plain statics
  resolve both ways. Sema mirrors both FCS filters (`AddValRefsToItems`'
  `not IsMember`, `ChooseMethInfosForNameEnv`'s
  `IsMethInfoPlainCSharpStyleExtensionMember`) in
  `AssemblyEnv::open_static_entries`, backed by `MethodLike::augmentation`
  (per-member, instance *and* static) and `Entity::is_extension_container`
  (the enclosing-type `[<Extension>]` marker). The filter is per *member* not per
  *name* (a module may declare both `let M` and augmentation `M`); the C#-style
  predicate is scoped to non-module entities (an `[<Extension>]` module's `let`s
  are vals, not members, and stay bare-resolvable).
- **Stream 2 — pickle-driven member list** (PRs #551, #556/#558, #909, #913, #916)
  — **complete**, tracked in
  [`completed/fsharp-pickle-member-projection-plan.md`](completed/fsharp-pickle-member-projection-plan.md).
  The F# member list now comes from the signature pickle's module vals, so the
  A1/A2 source-name and extension facts run on the authoritative pickle path with
  the ECMA-335 attribute decode kept only as fallback for images whose pickle
  does not decode. This closed the §7 generic-extension and witness-exclusion
  gaps (the `$W` filter was measured durable, not bridge code, and kept — with it
  removed FSharp.Core records 153 skipped members, poisoning the overload
  extension-absence gate).

## Still to do

### A4 / S4 — operators (`op_Addition` ⇒ `+`)

The one outstanding stage of the original plan. Not on the `printfn` critical
path; deferred behind the function milestone. `assembly_env.rs` already notes
module-shaped AutoOpen entries stay closed "until the A4/S4 demangle slice."

- **A4 (assembly).** A deterministic demangle table: port the `opNameTable` /
  `decompileOpName` subset of
  `dotnet/fsharp/src/Compiler/SyntaxTree/PrettyNaming.fs`. Set the demangled name
  as a method `source_name` on `op_*` methods. Living in the assembly crate keeps
  all source-name recovery in one place; sema is an alternative.
- **S4 (sema).** Once `op_*` methods carry a demangled `source_name`, operator
  uses resolve through the existing `opened_static_member` path with no further
  change. The parser models operator uses as expressions; confirm the resolver
  reaches them as names.
- **Revisit module-shaped AutoOpen entries when A4/S4 lands.** Open
  `Microsoft.FSharp.Core.LanguagePrimitives.IntrinsicOperators` first — it is
  operators-only (`&&`/`||`). The `TaskBuilderExtensions.{Low,LowPlus,Medium,High}Priority`
  and `QueryRunExtensions.{Low,High}Priority` entries additionally rely on the
  Slice-D extension-member filter so their statics (`Bind`/`Source`) stay
  un-bare-resolvable — opening them via `open_type_statics` before Slice D would
  have made bare `Bind`/`Source` wrongly resolve.

### Assembly-scoped namespace opens (follow-up slice)

Sema's open machinery is path-based (assembly-blind), so S3 drops a *contested*
namespace entry entirely (deferrals where FCS resolves, never a new wrong
resolution) and the hardcoded fallback keeps its historical env-wide application.
An `OpenGroup` reading that carries its contributing assembly through the tiered
walk would lift both the contested-namespace narrowing and the fallback's
residual env-wide divergence.

### The pickle-only "later" list

These are **out of scope** for this plan and cannot be recovered from ECMA-335
metadata — they are the real future justification for maturing the F#
signature-data unpickler further. Start this only when a pickle-only feature
forces the pickle anyway (type-abbreviation resolution, or Phase-3 inference);
doing it earlier front-loads divergence-prone unpickler work for no user-visible
gain.

- **Type abbreviations** — `seq<'T> = IEnumerable<'T>`, `single = float32`, etc.
  Abbreviations have no IL representation; resolving the *type* name `seq`
  (auto-opened via `Microsoft.FSharp.Collections`) needs the pickle. Some
  abbreviation decode already exists as shadow markers (#848) and is a documented
  divergence in `docs/fcs-divergences.md`. Blocks type-name resolution and later
  inference, not the value surface.
- **Units of measure** — partially handled (the `Measure` `EntityKind` overlay);
  full measure signatures need the pickle.
- **Active-pattern case structure** — a multi-case active pattern's individual
  case tags are pickle-only. Single-case recognizers like `(|Lazy|)` carry
  `CompilationSourceName` (`LazyPattern` ⇒ `|Lazy|`) and already resolve.
- **Precise curried/inline F# signatures for type inference** — IL gives the
  compiled (tupled/witness-passing) shape; the F# curried signature, optional
  args, and inline bodies are pickle-only. This is Phase 3 (inference) territory
  in [`type-checker-plan.md`](type-checker-plan.md), not name resolution.

### Residual (documented in `docs/fcs-divergences.md`)

The augmentation flag is pickle-driven, so an assembly whose pickle does not
decode (`ExtensionMembers::Unknowable`) falls back to the projector's dot-name
heuristic, which misses a `[<CompiledName>]`-renamed augmentation.

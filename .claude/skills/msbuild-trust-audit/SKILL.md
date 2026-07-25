---
name: msbuild-trust-audit
description: How to decide whether an MSBuild-evaluated value may be trusted, and the complete ten-entry checklist an item capture must consult before committing to a value. Use when adding or changing a consumer of a `borzoi-msbuild` evaluated item or property (ProjectReference edges, TargetFramework, output-file names, DefineConstants), when adding a `*_uncertain` flag or any trust gate, and when a reviewer flags one untrusted gate or value flow — audit all ten entries in that one round instead of patching the one.
---

# Trusting an MSBuild-evaluated value

Two distinct jobs, often confused:

- **Reference semantics** — "this value locates a file / names an edge". The
  generic seam `ParsedProject::property_provenance_untrusted` is the right
  signal.
- **Fold gates** — "refuse to analyse this project at all". The generic seam is
  **unusable** here alone. See §2.

## 1. The ten-entry item-trust checklist

When making a captured MSBuild item list trustworthy, *every* one of these must
consult trust state. A reviewer finds exactly one missing entry per round; each
looks like a one-off and is the same family. Sweep all ten at once.

1. The element's own `Condition` — **both directions**: an untrusted-false may
   skip a mutation that really runs; an untrusted-true may capture a phantom
   `Include`.
2. The enclosing `<ItemGroup>`'s `Condition` — same two directions.
3. Undecided `<Choose>` / `<When>` branches — mutations in still-possible branches.
4. `<Import>` / `<ImportGroup>` gates, and unresolved or failed imports.
5. `<ItemDefinitionGroup>` metadata defaults — these apply to **all** items
   regardless of document order (pass 2 precedes pass 3).
6. The `Include` **value** expansion. `Expansion::had_issue()` deliberately
   excludes `unpinned_root`, so a cleanly-expanded but unpinned value must be
   checked explicitly: `expansion.unpinned_root` + `raw_uses_sdk_package_taint`.
7. The `Exclude` **value** expansion — the same check as `Include`. (An untrusted
   `$(Skip)` expanding to empty keeps an edge the real build strips.) Shared
   helper: `taint_reference_list_on_untrusted_value`.
8. Metadata **gates** (both directions) and metadata **values**, in both the
   child-element and attribute forms — and case-variant duplicate attributes are
   last-write-wins (`rfind`, not `find`).
9. Metadata **names** you don't model but the consuming protocol treats as
   significant. For `ProjectReference`: `BuildReference`, `Targets`, and the
   `Set*` / property-list mutators. Unrecognised names are inert (probed), so
   this is a closed deny-list, not a catch-all.
10. Any **property the consumer locates artifacts by**. The output *file* name is
    `$(TargetName)` (defaulting to `$(AssemblyName)`; padding is preserved
    verbatim in the filename — probed), exposed as `ParsedProject::target_name`
    with `ItemMetadataValue` trust semantics. NuGet's assets record the
    **AssemblyName** stem, *not* TargetName, so the graph-evaluated name must
    out-rank the assets name when locating files. Likewise a body
    `TargetFramework` written under an untrusted gate must not become
    `NodeTfm::Known` — but the outer-gated **plural** (the arcade idiom) is
    unpinned by construction and must stay trusted, because the TFM-invariant
    intersection never believes a single branch anyway.

**Rule of thumb:** if a consumer starts reading a new evaluated property with
reference semantics, give it the same Known/Unknown verdict, or the next review
round will.

### MSBuild booleans

Boolean-valued metadata is compared with MSBuild `==` semantics, not string
equality. `ReferenceOutputAssembly` is `'v' == 'true'` after empty→true, so
`on` / `yes` / `!false` are **true**, and `0` / `1` / padded spellings are
**not** (probed dotnet 10.0.301). Use `borzoi_msbuild::msbuild_boolean`. Never
model an MSBuild boolean as `eq_ignore_ascii_case("true")`.

Trust policy, as pinned by tests: cleanly-decided conditions over never-written
properties are exact (the environment model); unsupported grammar, unpinned
properties, and SDK-tainted reads are not. `Choose` gates are stricter — any
diagnostic undecides. The catalogue lives in the
`ParsedProject::project_references_uncertain` docs.

## 2. Never gate a fold on the generic provenance seam

`ParsedProject::property_provenance_untrusted` fires **unconditionally for real
SDK projects**: probed on dotnet 10.0.301, `LangVersion` reads untrusted for
every SDK project, even one with a cleanly body-pinned value. SDK-file
uncertainty is tolerated by design, so a wholesale refusal (`build_parses` and
friends) needs a second signal separating real risk from SDK noise.

Two patterns that work:

- **A user-authored-only channel.** `define_constants_uncertain` sets
  `!in_sdk_subtree` at every set site.
- **A consequence-side flag**, computed where the uncertain value is consumed —
  often cheaper *and* exact. The LangVersion case used
  `Parse::shape_depends_on_language_version`: the lex-filter records reaching a
  version-gated offside push. `false` **proves** the tree is version-invariant
  (first-divergence argument); `true` over-approximates (EOF-anchored pushes
  reconverge — `module M =` mid-edit). So the fold refuses only on untrusted
  provenance × flagged file × a *verified* green-tree difference against a
  re-parse from the other side of the boundary (rowan green equality is
  structural). **Trigger cheap, verify exact.**

## 3. Test any new trust gate against a real SDK

SDK-less unit fixtures never produce SDK taint, so they stay green while every
real project breaks. Use `Workspace::new()` under `nix develop`;
`sdk_project_fold_e2e` pins both directions (plain folds; straddling + taint
refuses).

To find shape-divergent fixtures, don't hand-craft them: proptest an indentation
soup and assert the flag's soundness property
(`crates/cst/tests/all/shape_sensitivity.rs`). The shrunk counterexample
(`"match x with\n"`) is the reusable fixture.

For ground-truthing any claim about MSBuild's own semantics, use the
`msbuild-condition-oracle` skill rather than reasoning from docs.

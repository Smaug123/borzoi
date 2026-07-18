# `.fsi` signature files restrict the paired `.fs`'s cross-file exports

> **Status:** design + implementation plan. Not started. Branch
> `fix-signature-hidden-union-case-names` / worktree `fsi-restricts-exports`.
>
> Grounded in a full producer/consumer survey of the cross-file boundary and
> an FCS `uses-project` probe sweep (2026-07-18); the file:line anchors and the
> probe verdicts below are from that survey. Every semantic claim here was
> checked against FCS through `tools/fcs-dump uses-project`, not assumed.

## The feature

In F#, a signature file `M.fsi` constrains the *public interface* of the
implementation that declares the same top-level module `M`. A `let` in the
`.fs` that has no matching `val` in the `.fsi` is **hidden** — invisible to
every later Compile-order file. A `type` the signature declares *opaquely*
(`type Color`, no representation) hides its union cases / record fields, even
though the implementation spells them out. The implementation may declare
private helpers the signature never mentions; the signature exposes a subset.

Today sema does not model this at all, and the LSP papers over the gap by
**refusing to fold any project that contains a `.fsi`** (semantic.rs:1085),
degrading every such project to single-file resolution. This plan makes sema
honour the signature restriction, so a `.fsi`-bearing project folds correctly
instead of not at all.

## Current state (survey)

- **sema is signature-blind.** `resolve_file` / `resolve_project` take
  `&ImplFile` / `&[ImplFile]` exclusively (resolve.rs:86, resolve.rs:371).
  There is *zero* signature handling in the crate — every `fsi` token in
  `crates/sema/src` is a `dotnet fsi` oracle comment, not a signature file.
- **The cross-file boundary is a single derived currency.** Each file's
  downstream contribution is a source-ordered `Vec<ExportDecl>`
  (`ResolvedFile::export_decls`, model.rs:1455). `FileExportIndices::from_decls`
  (model.rs:816) derives every cross-file index from it, and
  `ProjectItems::extend_with` (model.rs:587) folds those into the threaded
  `ProjectItems` accumulator. The fold is `resolve_project_impl` (resolve.rs:392)
  and its single forward-threading writer `thread_forward` (resolve.rs:466),
  shared with the incremental fold (`resolve_project_incremental_impl`,
  resolve.rs:596) so the two cannot disagree.
- **An export's identity is an `ItemId` → `(file, DefId)`.** A cross-file
  `Resolution::Item` is mapped to its declaring file and binder by
  `ResolvedProject::item_def` (model.rs:1875), which finds the file whose
  contiguous `ItemId` range contains the handle. So *whichever* file produces an
  `ExportedItem` owns the go-to-def target — this is the hook the design turns.
- **The CST already parses signatures fully.** `parse_sig_with_symbols`
  (parser/mod.rs:253) produces a `SIG_FILE` root; `SigFile::modules()`
  (syntax/mod.rs:201) reuses the impl header machinery, and
  `ModuleOrNamespace::sig_decls()` (syntax/mod.rs:233) yields
  `SigDecl = Open | NestedModule | ModuleAbbrev | Val | Types | Exception |
  HashDirective` (generated/union_decls.rs:895). `ValDecl::val_sig()`
  (syntax/mod.rs:323) → `ValSig` (syntax/mod.rs:1100) exposes `ident()`,
  `active_pat_name()`, `ty()`, `literal_value()`, `attributes()`. The signature
  surface is fully addressable; nothing new is needed in `borzoi-cst`.
- **msbuild carries `.fsi` files through untouched**, in Compile order
  immediately before (or, per the probes below, merely *earlier* than) their
  `.fs` (`ParsedProject.items`, msbuild/src/lib.rs:415). No extension filtering.
- **The LSP refuses.** `build_parses` (semantic.rs:1010) scans the Compile
  includes and returns `None` on the first `.fsi` (semantic.rs:1085), pinned by
  `project_with_fsi_signature_yields_none` (semantic.rs:2266) and
  `project_with_uppercase_fsi_signature_yields_none` (semantic.rs:2296). The
  fold-facing parser wrapper `cst_panic_safe::parse_with_symbols`
  (cst_panic_safe.rs:24) hardcodes `FileKind::Impl`.

## FCS-grounded semantics (the probe sweep)

Every row below is a `tools/fcs-dump uses-project` verdict over a multi-file
project. `A.shown` etc. are the *cross-file* uses in a downstream `Use.fs`; the
"decl" column is `DeclRange.File` (which file FCS says the use is declared in).

| fixture | use | FCS verdict |
|---|---|---|
| `A.fsi{val shown}` `A.fs{let shown; let hidden}` | `A.shown` | resolves, **decl = `A.fsi`** (the `val` ident) |
| same | `A.hidden` | **FS0039 "not defined"**, *no symbol use emitted* |
| `Col.fsi{type Color = Red\|Green}` `Col.fs{same}` | `Col.Color.Red` | resolves, **decl = `Col.fsi`** (the case ident) |
| `Op.fsi{type Color}` (opaque) `Op.fs{type Color = Red\|Green}` | `Op.Red` | **FS0039**, *no symbol use* |
| same | `Op.Color` as a ctor | **FS1133 "no constructors available"** |

Six load-bearing conclusions, each disambiguated by a dedicated probe:

1. **World A — the signature is the declaration.** A cross-file use of a
   signature-exposed value/type/case resolves to the **`.fsi`** ident, not the
   `.fs`. Any design that keeps the impl's identity for surviving exports
   (a "filter the impl" model) would point go-to-def at the wrong file and
   *fail* the differential (a wrong range, not an honest `Deferred`).
2. **The impl's own body is unchanged.** In `A.fs`, the binder occurrences of
   `shown`/`hidden` still declare in `A.fs` (`IsFromDefinition`, decl = `A.fs`).
   So the impl's *intra-file* `resolutions` are untouched by this feature — only
   its *cross-file export contribution* changes.
3. **Pairing is by FCS's `QualifiedNameOfFile` (QNOF)**, disambiguated by two
   probes: a **module-headed** file pairs by its top module's qualified name —
   `Sig.fsi{module A}` restricts `Impl.fs{module A}` despite mismatched
   filenames — while a **namespace-headed** file pairs by its **filename stem**:
   `TheSig.fsi{namespace N; module M}` does *not* restrict
   `TheImpl.fs{namespace N; module M}` (mismatched stems), but same-stem
   `NsMod.fsi`/`NsMod.fs` does. This is exactly FCS's QNOF (module name when a
   file leads with `module M`, else the filename). Consequence: sema can pair
   module-headed files path-free, but pairing a namespace-headed signature
   needs the **filename** — so the fold input must carry a per-file identity
   (see the pairing rule below). A signature restricts *only* its paired module;
   a sibling module in the same namespace, or any unsigned module, exports fully
   (probes J, M).
4. **A signatured module becomes visible at its *implementation's* Compile
   position, not its signature's.** A file sitting *between* `A.fsi` and `A.fs`
   cannot reference `A.shown` — FS0039 "A is not defined" (probe L). And a
   self-qualified reference to the *current* module (`A.shown` inside `A.fs`,
   `N.M.x` inside its own `module M`) is FS0039 *independently of signatures*
   (probes K, K2). So the sig's exports must be folded into `preceding` at the
   **impl's** position, *after* the impl itself is resolved — which is exactly
   what keeps intervening files and self-references `Deferred`.
5. **`[<AutoOpen>]` on the signature is authoritative.** A bare cross-file use of
   an auto-opened value resolved with the attribute present on the **`.fsi`
   only** (impl un-attributed, probe F). So sema must read the auto-open bit from
   the signature when one exists.
6. **Omission and opacity both mean "undefined downstream".** A `let` with no
   `val`, a case of an opaque union, and a field of an opaque record each produce
   FS0039 and *no* symbol use — the certain-implies-exact target is: sema must
   **not** resolve them (`Deferred`/unrecorded), which suppressing the impl's
   export achieves.

### Stage-3 decl-kind probe verdicts

The full sweep (all `uses-project`, all fixtures diagnostics-clean unless noted)
pins the identity of every signature declaration kind — every surviving export
lands on the **`.fsi`**:

| signature decl | cross-file use | verdict |
|---|---|---|
| `val internal x` | `A.x` | resolves, decl `.fsi` (internal = project-visible) |
| `val private x` | `A.x` | resolves to `.fsi` **but FS1094 inaccessible** — a private cross-file use is always an error, so it never appears in a clean fixture; the existing `access_root_len` machinery models it |
| `module internal M` / `module Shown` | `A.M.y` | resolves, decl `.fsi` (internal project-visible) |
| `val (\|Even\|Odd\|)` (active pattern) | `open A; match … with Even` | `Even`/`Odd` resolve to the recognizer span in the `.fsi` |
| `val (\|DivBy\|_\|)` (partial, param) | `DivBy 3` | resolves to `.fsi`, applied form splits |
| `exception E of int` | `A.E 3` | resolves, decl `.fsi` |
| `type Alias = int` | `A.Alias` | resolves, decl `.fsi` |
| `type R = { X:int }` (visible) | `r.X` (field) | resolves, decl = the field in the `.fsi` |
| `type R` (opaque) | `r.X` | **FS0039** — field hidden; the type name itself still resolves to `.fsi` |
| nested `module Inner =` with `val` | `A.Inner.shown` | resolves `.fsi`; an omitted `A.Inner.hidden` → FS0039 |
| `[<AutoOpen>] module M` (sig only) | bare `shown` after `open Ns` | resolves via the sig's auto-open, decl `.fsi` |

Nothing in the sweep is un-modelled by the design below; the staging is about
landing them in reviewable slices, not about unresolved semantics.

## Design

The `ItemId → (file, def)` routing (point 2 above) hands us the whole design for
free: **make the signature file a first-class Compile-order file that produces
the module's cross-file exports, with signature identity, and suppress the
paired implementation's export contribution.** The impl still resolves its own
body; it just stops leaking anything across the boundary. Everything the
signature exposes flows through the *existing* `ExportDecl` currency, now emitted
from `SigDecl`s instead of `ModuleDecl`s.

### Input model: interleave signature files into the fold

Change the fold's input from "implementation files" to "Compile-order source
files", each an impl or a sig:

```rust
// crates/sema/src/resolve/model.rs (or resolve.rs)
pub enum SourceFile {
    Impl(ImplFile),
    Sig(SigFile),
}

pub fn resolve_project(files: &[SourceFile], assemblies: &AssemblyEnv) -> ResolvedProject;
```

This mirrors msbuild's Compile order (which already emits `.fsi` before `.fs`)
and FCS's own file list exactly, and it keeps `ResolvedProject::file(i)` /
`item_def` working unchanged: a signature file is a real file with a real index
and a real `Def` arena, so a cross-file `Item` naturally routes to
`(sig_idx, sig_def)`. Keep a thin `resolve_project` overload (or a
`From<ImplFile> for SourceFile`) for the impl-only common case so the large
population of single-file / impl-only tests need only a mechanical wrapper, not
a rewrite — an impl-only project is a legitimately common shape, not a
half-migration.

Why interleave rather than a paired `&[(ImplFile, Option<SigFile>)]`: the export
identity must be the *signature's* binder (World A), which means the signature
needs its own `Def` arena and its own `ItemId` range — i.e. it must *be* a file
in the `ResolvedProject`, not a sidecar hanging off the impl's slot. Interleaving
is the honest representation of that; a sidecar would force `item_def` to invent
a second addressing scheme.

### Pairing rule (by `QualifiedNameOfFile`)

Pair each impl with the *earlier* signature that shares its **QNOF** (probe
conclusion 3). QNOF is **not** something to hand-approximate — it is FCS's
`QualifiedNameOfFile`, and the plan must port it faithfully, because a mismatch
in either direction is a correctness bug (over-pairing suppresses an unrelated
impl; under-pairing leaks names the signature hides). FCS's rule
(`ParseAndCheckInputs.fs`):

- A file that **leads with `module M`** has QNOF = the module's qualified name
  `M` (`ModuleOrNamespaceKind::NamedModule`, syntax/mod.rs:211; via
  `modules().long_id()`). sema derives this from the AST alone.
- Any other file (namespace-headed, multi-fragment, anonymous) has QNOF derived
  from the **filename**, via FCS's `CanonicalizeFilename` — which **capitalises**
  the stem (so `foo.fsi` and `Foo.fs` both → `Foo` and *do* pair, codex review
  finding 3) — followed by `DeduplicateModuleName`, which **disambiguates equal
  names by containing directory** (so `d1/Part.fsi` pairs `d1/Part.fs` but *not*
  `d2/Part.fs`, codex review finding 4). The raw path stem is wrong on both
  counts.

Therefore the fold's per-file input is a `(SourceFile, qnof: QualifiedName)`
pair; the caller supplies QNOF because the filename-derived case needs the path,
which the LSP holds (`ProjectParses.paths`, semantic.rs:97). **The QNOF
computation is itself FCS-differential-tested**, not reasoned: a fixture sweep
feeds FCS a file set and asserts sema's sig/impl pairing matches FCS's (observed
through which names a downstream file can and cannot resolve) — the systematic
guard that turns "reproduce `CanonicalizeFilename`/`DeduplicateModuleName`
correctly" from a judgement call into a checked property, so cases like the two
above are caught by construction rather than by review. A signature-paired impl
contributes **no** cross-file *identities*; its paired signature contributes them
instead (Stage 2). An unpaired signature (no impl with a matching QNOF) is inert,
and an unsigned impl exports normally (probes J, M). Namespace-headed signatures
pair through the same QNOF path (probes G, G2), so the blanket `.fsi` refusal is
removed outright — no narrowed-refusal fallback is needed.

### Correctness-over-availability framing

The signature can only ever *remove* names from the boundary (the impl must
implement everything the signature exposes; the signature exposes a subset). So
every step here moves monotonically toward FCS:

- Suppressing the impl's *value/case identity* exports turns previously-committed
  cross-file `Item`s into `Deferred` — and today, *before* the LSP even calls the
  fold on a `.fsi` project, those commits don't exist (the project is refused).
  So there is no regression to fear; there is only the over-export **bug** (a
  `.fs`-private `hidden` resolving cross-file) to fix and the fold to re-enable
  for the project's other files.
- Re-emitting the identities from the signature is a set of *new, certain*
  commits: emit an `ExportedItem` only when the signature decl's path, identity
  range, and kind are certain (a plain `val x : T` under `module M` → path
  `[M, x]`, identity = the `x` ident in the `.fsi`). Any signature decl kind not
  yet modelled emits no *identity*, so the name stays `Deferred` downstream — an
  honest coverage gap, never a wrong answer.

**Hiding a value means recording it *inaccessible*, not dropping it (codex
review, findings 1 + 2 — one mechanism).** Two failure modes rule out simply
deleting a hidden value's export:

1. **Assembly fallthrough.** `value_exports` is the *only* per-path tripwire that
   stops a qualified value reference from falling through to a colliding
   referenced-assembly symbol: `module_headers` blocks only the *exact* module
   path (not `Foo.bar`), and `modules_with_hidden_values` is a bare-`open`
   generation bump, not consulted for a qualified `Foo.bar`. Drop the `Item` for a
   signature-hidden `let bar` and a downstream `Foo.bar` wrongly binds the
   assembly's `Foo.bar` where FCS binds the `.fsi`/errors.
2. **Multi-fragment recovery.** A module `N.A` split across an unsigned `First.fs`
   (public `let x`) and a signatured `Pair.fs` (private `x`) must resolve a
   downstream `N.A.x` to **`First.fs`** — FCS skips the private binder and takes
   the earlier public one (probe: decl = `First.fs`). Emitting the later private
   `x` as an accessible latest-wins `Item` would mis-resolve to `Pair`.

Both fall out of the machinery the codebase already built for exactly this: the
per-path `value_exports` **history** plus `access_root_len`. Record a
signature-hidden value as an `ExportRecord` with a **module-scoped access root**
(`access_root_len = Some(module_len)` — private to its own module). Then
`is_project_value_prefixed` (model.rs:352), which is **accessibility-independent**
(it asks only "is there a value at this path"), still fires → the assembly is
blocked; while `latest_accessible_value`, which *is* accessibility-gated, skips
the hidden entry → it never commits cross-file, and recovers an accessible
earlier-fragment public export if one exists (failure mode 2, exactly the
"public export under a later inaccessible private is still selectable" property,
model.rs:82-85). Structural decls (`Module`, `Type`, `Namespace`, …) are still
kept as shadow tripwires for the *non*-value paths (nested modules, types) the
same way.

## Implementation plan

Each stage is its own branch, reviewable in isolation, gated on the full suite
plus the ignored corpus differentials.

### Stage 1: interleave signatures; suppress paired impls (pure restriction)

**Dependencies:** none. **Behaviour change:** removes wrong commits + re-enables
the fold; adds no new commits.

- Introduce `SourceFile` and rework `resolve_project` / the incremental fold /
  `thread_forward` to iterate `&[(SourceFile, QualifiedName)]` (the QNOF per
  file, pairing rule above). A `SourceFile::Sig` produces an inert-for-*values*
  `ResolvedFile` (its own `resolutions` left `Deferred`; Stage 2 fills them) and,
  in Stage 1, contributes nothing itself — the paired impl carries the blockers
  (next bullet).
- Compute the signature-paired set by QNOF. A paired `SourceFile::Impl` is
  resolved exactly as today (internal `resolutions` unchanged — probe point 2),
  and `thread_forward` folds a **hidden** contribution for it: its value/case
  exports are re-stamped with a **module-scoped `access_root_len`** (inaccessible
  cross-file — the framing above) rather than dropped, so they shadow the
  assembly and recover earlier public fragments but never commit; its structural
  decls (`Module`, `Type`, `Namespace`, `ModuleAbbrev`, `ExceptionTycon`,
  `Extern`) are kept verbatim as shadow tripwires for the non-value paths. This
  keeps the impl at its own (correct) fold position, so no provenance question
  arises in Stage 1 (finding 2 of the first round is a Stage-2 concern). The
  over-approximation (a private nested module the sig hides still shadows) only
  adds `Deferred`s — sound.
- LSP: delete the `.fsi` refusal (semantic.rs:1085) outright; parse each Compile
  item with the grammar its extension selects (`is_signature_file`,
  semantic.rs:2133 → a new panic-safe `parse_sig_with_symbols` beside
  `cst_panic_safe.rs:24`), compute each file's QNOF (module name, or the
  canonicalised + directory-disambiguated filename per the pairing rule), and
  build the interleaved input. `ProjectParses` (semantic.rs:97) carries
  `SourceFile` + QNOF instead of `ImplFile`.
- Replace the pinned refusal tests (semantic.rs:2266/2296) with folds-correctly
  assertions for both a `module M`- and a `namespace N; module M`-headed
  `.fsi` project.

**Why it is sound:** the fold strictly loses *value/case* commits (paired impl
identities re-stamped inaccessible) and gains no new commit — the hidden entries
and structural decls only ever *block* (cause `Deferred`s), never resolve to a
def, since types/modules carry no cross-file identity today (first-round finding
3). Certain-implies-exact holds, and the visibility *timing* is free: the paired
module publishes no *accessible* identity, so intervening files (probe L) and
self-qualified references (probes K/K2) see nothing — exactly FCS's FS0039.
Paired modules under-resolve (their public names go `Deferred` cross-file) — the
honest D5 cost, paid until Stage 2. Every unsigned module (probes J, M) folds for
the first time.

**Oracle:** FCS-free `resolve_project` unit tests (a hidden `let` no longer
resolves cross-file; a non-`.fsi` sibling module still does); an
**assembly-collision** fixture — a signatured `module Foo` whose path collides
with a referenced-assembly symbol, asserting a downstream `Foo.bar` stays
`Deferred` rather than committing to the assembly (finding 1's regression, gated);
an LSP e2e that a `module M`-headed `.fsi` project folds where it previously
returned `None`; the ignored `resolve_corpus_diff` / `resolve_project_diff` gates
stay green.

### Stage 2: the signature becomes the exporter (signature identity)

**Dependencies:** Stage 1. **Behaviour change:** first new commits — cross-file
uses of a signature's surface resolve to the `.fsi`.

- Give `SourceFile::Sig` a real `Def` arena for its ident ranges, and produce the
  module's **value/case identity** exports from `sig_decls()` — the surface the
  existing `Item` currency can carry a def for (probe conclusion 1, World A):
  - `SigDecl::Val` with a plain `ident()` → an `Item` value export at
    `[module.., name]`, `def` = the `x` ident in the `.fsi`. Skip
    active-pattern-named and operator-named vals for now (Stage 3).
    **Read the `private` marker here, not in Stage 3 (codex review, finding 2).**
    A `val private x` must be emitted with a module-scoped `access_root_len` (the
    same inaccessible shape Stage 1 gives a hidden value), *not* as an accessible
    public `Item` — otherwise, when an earlier fragment exports a public `x` at
    the same path, the accessible latest-wins query would mis-resolve the
    downstream use to the private `.fsi` binder where FCS takes the earlier public
    one. Emitting it inaccessible makes `latest_accessible_value` recover the
    public fragment, exactly as in Stage 1's framing. (The finer `internal`-vs-
    public distinction can wait for Stage 3; only the private→inaccessible bit is
    load-bearing for soundness and must land with the first identities.)
  - `SigDecl::Types` with a *visible* union/enum representation → the case
    `Item`s + type-qualified case paths, reusing the existing `CaseKind` /
    `type_qualified_cases` machinery. An **opaque** representation (bodyless
    `type Color`) emits **no** case identities (opaque hides members) — the crux
    the impl walk cannot express, and the reason the signature must be the
    exporter. (The type-path *shadow* index is already emitted structurally in
    Stage 1; here we withhold the *cases*.)
  - The `[<AutoOpen>]` bit is read from the **signature** header (probe
    conclusion 5), so an auto-opened `val` folds as an auto-open value.
- **Decouple def-ownership from fold-provenance (codex review, finding 2).** The
  export's *definition* is the signature's binder (`item_def` must return the
  `.fsi`), but its *fold provenance* — the Compile position that drives
  `item_file_bases` / `file_of`, auto-open ordering, and direct-tier latest-file
  collisions — must be the **implementation's** slot, because FCS publishes the
  module at the impl's position (probe conclusion 4; codex's own probe:
  `[A.fsi{[<AutoOpen>] val Red}, B.fs{exception Red}, A.fs]` resolves a downstream
  `Red` to `A`'s auto-open member, so A's contribution is ordered *after* B.fs).
  So: on reaching a `Sig`, stash its identity `export_decls`; contribute nothing.
  On reaching the paired `Impl`, resolve the impl against a `preceding` that does
  *not* include the sig (self-refs stay `Deferred`), then fold the sig's stashed
  identities **at the impl's slot** — the `ItemId` range, `item_base`, and
  `item_file_bases` push all stay monotonic and attributed to the *impl's* file
  index, exactly as an ordinary file. What changes is only the `def` target:
  `ExportedItem::def` must address the **signature's** file+arena, not the owning
  file's. Since `ExportedItem::def` is today a bare `DefId` resolved within the
  owning file (`resolved_def`, model.rs), Stage 2 extends it to an explicit
  cross-file `(file_idx, DefId)` for signatured exports, and `item_def`
  (model.rs:1875) follows that pointer instead of assuming the def lives in the
  `ItemId`-owning file. Cover with a test that a cross-file sig export's
  `item_def` returns the `.fsi`'s index **and** that a colliding later-file
  contribution loses to the auto-opened sig member (the provenance direction).
- **Type-name and module-qualifier go-to-def stays `Deferred` — narrow the claim
  (codex review, finding 3).** `ExportDeclKind::Type` / `Module` / `Namespace`
  carry no `ItemId`/`DefId`, and cross-file *type* and *module-qualifier* uses are
  already `Deferred` for impl files today (the `resolve_project_diff` header notes
  module qualifiers are "not modelled as a def yet"). Stage 2 therefore makes
  *value and case* uses resolve to the `.fsi`, and honours *opacity* (via the
  withheld case identities), but a downstream `A.SomeType` / `open A` /
  `A` qualifier remains `Deferred` — matching, not regressing, today's impl-file
  behaviour. Making type/module qualifiers resolve to the `.fsi` binder needs a
  model extension (identities on the `Type`/`Module`/`Namespace` exports) and is
  scoped to Stage 3+.

**Oracle:** a new **signature-aware** `resolve_project_diff` harness: extend
`temp_fs_file` (common/mod.rs:817) to honour a `.fsi` label and feed
`invoke_fcs_dump_project` (common/mod.rs:257) an interleaved sig/impl path list;
assert certain-implies-exact against `uses-project` for the whole probe matrix
(exposed val/case → `.fsi` decl; hidden/opaque → `Deferred`/unrecorded). Include
the **non-adjacent auto-open collision** fixture (provenance = impl slot) and the
**public-fragment recovery** fixture — an earlier unsigned `First.fs` public `x`
plus a later signatured `Pair` private `x`, asserting `N.A.x` resolves to
`First.fs`, not the `.fsi` (finding 2). Keep every fixture
`uses-project`-diagnostics-clean. Corpus gates green.

### Stage 3+: enrich the modelled signature surface

Each its own FCS-differential-gated slice; the semantics are already pinned by
the sweep above, so these are landing order, not open questions:

- **Accessibility (finer half)** — `val internal` / `module internal`
  (project-visible → exported as *accessible*, distinct from the
  private→inaccessible bit already landed in Stage 2), threaded through the
  existing `access_root_len` machinery. A private cross-file use is always an
  FS1094 error, so it never rides a clean differential fixture; Stage 2 already
  exports private-sig values with the module access-root, so the filter declines
  the outside use — this stage adds the `internal` accessibility level on top.
- **Active-pattern `val`s** — `val (|Even|Odd|) : …` and partial/parameterized
  `val (|DivBy|_|) : …`, wired to the Stage-3a active-pattern-case export path
  (`docs/export-decl-model-plan.md`), with the recognizer span in the `.fsi` as
  the identity.
- **Exceptions, module abbreviations, type abbreviations, records (visible field
  identity + opaque-record field hiding), and nested-module signatures**
  (recursive `sig_decls()`).

## Resolved questions and remaining risks

Two design questions the earlier draft left open are now settled by the sweep:

- **Impl-sees-its-own-signature** is a non-issue *given the fold-at-impl-position
  rule* (Stage 2, probe conclusion 4): the impl is resolved before its sig folds,
  so a self-qualified `M.foo` inside `M.fs` never sees the sig and stays
  `Deferred` — matching FCS, which rejects it as FS0039 regardless of signatures
  (probes K/K2). The rule that makes intervening files correct (probe L) makes
  self-references correct for free.
- **Namespace-file pairing** works through the same QNOF path as module files
  (probes G, G2, I) — namespace-headed files pair by their FCS-canonicalised,
  directory-disambiguated filename (see the pairing rule), module-headed by module
  name. No refusal is needed; the only cost is threading the QNOF from the LSP
  (which has the path), and differential-testing that computation against FCS.

Remaining risks to treat as first-class:

- **Incremental fold reuse.** `resolve_project_incremental_*` compares trees for
  reuse (resolve.rs `same_tree`); it must treat a sig and an impl as distinct and
  re-fold a module when *either* half changes, and — because the sig folds at the
  impl's position — invalidate the impl's downstream contribution when the *sig*
  edits even though the impl's tree is unchanged. A correctness tripwire; cover it
  with an incremental-≡-batch test that edits only the `.fsi`.
- **Cross-file `def` addressing (Stage 2).** Extending `ExportedItem::def` to an
  explicit `(file_idx, DefId)` for signatured exports (so `item_def` reaches the
  `.fsi` while provenance stays at the impl slot — finding 2) touches every reader
  of the export's def. Keep `ItemId`/`item_file_bases` monotonic and
  impl-attributed; the *only* signatured-specific behaviour is the def redirect.
  Audit `resolved_def` / `token_classifier` for a latent "def lives in the
  `ItemId`-owning file" assumption.
- **Type/module identity is a deliberate gap** (finding 3): value/case uses
  resolve to the `.fsi`; type-name and module-qualifier uses stay `Deferred`
  (as for impl files today). Do not let a later stage quietly promise more without
  extending the export/`Resolution` model and its differential.
- **Bonus surface: `.fsi` buffers as query targets.** Once a signature is a real
  `ResolvedFile`, project queries (hover, document symbols, semantic tokens) on a
  `.fsi` buffer get a real resolution instead of the single-file fallback. Stage 1
  keeps sig `resolutions` inert, so guard the LSP export consumers
  (semantic.rs:2441, 4448) against an empty sig surface; treat richer `.fsi` query
  support as out of scope until it has its own plan.

## References

- Fold + threading: resolve.rs:371 (`resolve_project`), :392
  (`resolve_project_impl`), :466 (`thread_forward`), :596 (incremental).
- Boundary currency: model.rs:587 (`extend_with`), :816
  (`FileExportIndices::from_decls`), :1050/:1071 (`ExportDecl`/`ExportDeclKind`),
  :1228 (`ExportedItem`), :1455 (`export_decls`), :1875 (`item_def`).
- CST signature surface: parser/mod.rs:253 (`parse_sig_with_symbols`),
  syntax/mod.rs:201 (`SigFile::modules`), :233 (`sig_decls`), :323
  (`ValDecl::val_sig`), :1100 (`ValSig`), generated/union_decls.rs:895
  (`SigDecl`).
- LSP wiring: semantic.rs:1010 (`build_parses`), :1085 (the refusal), :2133
  (`is_signature_file`), :2266/:2296 (refusal tests), :97 (`ProjectParses`);
  cst_panic_safe.rs:24; diagnostics.rs:53 (`SourceKind`).
- Test harness: common/mod.rs:257 (`invoke_fcs_dump_project`), :817
  (`temp_fs_file`), :1528 (`parse_fcs_uses_project`); `resolve_project_diff.rs`.
- Related boundary design: `docs/export-decl-model-plan.md`.

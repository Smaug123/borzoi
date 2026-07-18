# `.fsi` signature files restrict the paired `.fs`'s cross-file exports

> **Status:** Stage 1 implemented (with the §screen correction below —
> a 2026-07-18 probe refuted one cell of the original Stage-1 matrix);
> Stages 2/3+ not started.
>
> **Grounded in an FCS `uses-project` probe sweep (2026-07-18), including
> reference-assembly-collision probes** (a built `RefLib.dll`). Every semantic
> claim below was checked against FCS, not assumed. This matters: an earlier
> draft, following reviewer intuition, added elaborate "block the assembly /
> record hidden values inaccessibly" machinery — which a collision probe then
> *refuted* (FCS merges the module with the assembly and lets hidden members fall
> through). The design here is the simpler one the probes actually support:
> **hide = drop.** Where a claim was reviewer conjecture that a probe overturned,
> it is flagged.

## The feature

In F#, a signature file `M.fsi` constrains the *public interface* of the
implementation that declares the same top-level module `M`. A `let` in the
`.fs` with no matching `val` in the `.fsi` is **hidden** — invisible to every
later Compile-order file. A `type` the signature declares *opaquely*
(`type Color`, no representation) hides its union cases / record fields, even
though the implementation spells them out. The implementation may declare
private helpers the signature never mentions; the signature exposes a subset.

Today sema does not model this, and the LSP papers over the gap by **refusing to
fold any project that contains a `.fsi`** (semantic.rs:1085), degrading every
such project to single-file resolution. This plan makes sema honour the
restriction so a `.fsi`-bearing project folds correctly instead of not at all.

## Current state (survey)

- **sema is signature-blind.** `resolve_file` / `resolve_project` take
  `&ImplFile` / `&[ImplFile]` (resolve.rs:86, resolve.rs:371). No signature
  handling anywhere in the crate.
- **The cross-file boundary is one derived currency.** Each file's downstream
  contribution is a source-ordered `Vec<ExportDecl>`
  (`ResolvedFile::export_decls`, model.rs:1455); `FileExportIndices::from_decls`
  (model.rs:816) derives every cross-file index from it, and
  `ProjectItems::extend_with` (model.rs:587) folds those into the threaded
  `ProjectItems`. The fold is `resolve_project_impl` (resolve.rs:392); its single
  forward-threading writer is `thread_forward` (resolve.rs:466), shared with the
  incremental fold (`resolve_project_incremental_impl`, resolve.rs:596).
- **An export's identity is an `ItemId` → `(file, DefId)`.** A cross-file
  `Resolution::Item` maps to its declaring file and binder by
  `ResolvedProject::item_def` (model.rs:1875), which finds the file whose
  contiguous `ItemId` range contains the handle. *Whichever* file produces an
  `ExportedItem` owns the go-to-def target — the hook the design turns.
- **The CST already parses signatures fully.** `parse_sig_with_symbols`
  (parser/mod.rs:253) produces a `SIG_FILE` root; `SigFile::modules()`
  (syntax/mod.rs:201) reuses the impl header machinery; `sig_decls()`
  (syntax/mod.rs:233) yields `SigDecl = Open | NestedModule | ModuleAbbrev | Val |
  Types | Exception | HashDirective` (generated/union_decls.rs:895).
  `ValDecl::val_sig()` (syntax/mod.rs:323) → `ValSig` (syntax/mod.rs:1100) exposes
  `ident()`, `active_pat_name()`, `ty()`, `literal_value()`, `attributes()`.
  Nothing new is needed in `borzoi-cst`.
- **msbuild carries `.fsi` through untouched**, in Compile order before its
  `.fs` (`ParsedProject.items`, msbuild/src/lib.rs:415). No extension filtering.
- **The LSP refuses.** `build_parses` (semantic.rs:1010) returns `None` on the
  first `.fsi` (semantic.rs:1085), pinned by
  `project_with_fsi_signature_yields_none` (semantic.rs:2266). The fold-facing
  parser wrapper `cst_panic_safe::parse_with_symbols` (cst_panic_safe.rs:24)
  hardcodes `FileKind::Impl`. Cross-file *type* and *module-qualifier* uses are
  already `Deferred` (the `resolve_project_diff` header: module qualifiers are
  "not modelled as a def yet") — only *values* and *cases* carry a cross-file
  `Item` identity today.

## FCS-grounded semantics (the probe sweep)

All rows are `tools/fcs-dump uses-project` verdicts over multi-file projects;
the assembly rows reference a built `RefLib.dll` via `BORZOI_FCS_EXTRA_REFS`.

### Core restriction

| fixture | cross-file use | verdict |
|---|---|---|
| `A.fsi{val shown}` `A.fs{let shown; let hidden}` | `A.shown` | resolves, **decl = `A.fsi`** |
| same | `A.hidden` | **FS0039**, *no symbol use* |
| `Col.fsi{type Color = Red\|Green}` (visible) | `Col.Color.Red` | resolves, decl = the case in `Col.fsi` |
| `Op.fsi{type Color}` (opaque) | `Op.Red` / `r.X` | **FS0039** — opacity hides cases / record fields |

### The assembly merge — the probe that overturned the earlier draft

A **built reference assembly** `RefLib.dll` exporting `ProbeNs.Shared.bar` /
`ProbeNs.Shared.asmOnly`, plus a project `module Shared` (both namespaced and
top-level forms) whose `.fsi` exposes only `shown`:

| use | verdict |
|---|---|
| `Shared.shown` (in sig) | resolves to the **`.fsi`** (project) |
| `Shared.shown` (in sig, **also in the assembly**) | resolves to the **`.fsi`** — the sig-exposed member shadows the merged assembly member (probe 2026-07-18) |
| `Shared.bar` (hidden by sig, **also in the assembly**) | resolves to the **assembly** — `RefLib`, **no diagnostic** |
| `Shared.asmOnly` (assembly only) | resolves to the assembly |
| `Shared.shown` / `Shared.bar` from a file **between** the sig and the impl (assembly collision) | both resolve to the **assembly** — the merged module publishes only at the impl's slot (probe 2026-07-18) |

Confirmed for both a namespaced `namespace ProbeNs; module Shared` and a
top-level `module Foo`. **A signatured project module merges with a same-named
referenced assembly; members the signature hides fall through to the assembly
member** (or are FS0039 when no assembly provides them). So the sound model for a
hidden member is not "block it" but **"drop it"** — a dropped export naturally
falls through to the assembly env (matching FCS) or becomes `Deferred` (the
honest D5 gap where FCS errors). The earlier draft's assembly-shadow blockers and
inaccessible-entry recording asserted the *opposite* and are removed.

### Identity, timing, pairing

Six conclusions, each probe-disambiguated:

1. **World A — the signature is the declaration.** A cross-file use of a
   signature-exposed value/case resolves to the **`.fsi`** ident, so the
   signature (not the impl) is the exporter of what survives.
2. **The impl's own body is unchanged.** In `A.fs`, the binders of
   `shown`/`hidden` still declare in `A.fs`; only the *cross-file export
   contribution* changes.
3. **Timing: a signatured module publishes at its *implementation's* Compile
   position.** A file *between* `A.fsi` and `A.fs` cannot see `A.shown` (FS0039,
   probe L). A self-qualified reference to the current module (`A.shown` inside
   `A.fs`) is FS0039 *independently of signatures* (probes K/K2). So exports fold
   at the impl's position, after the impl is resolved — which keeps intervening
   files and self-references `Deferred` for free.
4. **Provenance ≠ def-ownership.** FCS orders the module's contribution at the
   impl's slot: with `[A.fsi{[<AutoOpen>] val Red}, B.fs{exception Red}, A.fs]`, a
   downstream `Red` binds A's auto-open member (published at `A.fs`, *after*
   B.fs). So auto-open ordering / latest-file collisions / `item_file_bases` must
   use the **impl** slot, while go-to-def (`item_def`) must reach the **signature**.
5. **Pairing is per-file, by FCS's `QualifiedNameOfFile` (QNOF).** A file leading
   with `module M` has QNOF `M` (module qualified name, AST-derivable); any other
   file has QNOF derived from its **filename** via FCS's `CanonicalizeFilename`
   (capitalises the stem — `foo.fsi` pairs `Foo.fs`) then
   `DeduplicateParsedInputModuleName` (disambiguates equal names). A signature
   pairs with the **first following** impl of the same QNOF: with
   `[d1/Pair.fsi{module M}, d1/Pair.fs{module M}, d2/Extra.fs{module M}]`,
   `M.shown`→`Pair.fsi`, `M.hidden`→FS0039, but `M.extra`→`Extra.fs` — the *other*
   same-name fragment is **not** suppressed (probe X3). Sibling and unsigned
   modules export fully (probes J, M).
6. **`[<AutoOpen>]` on the signature is authoritative** (bare cross-file use
   resolved with the attribute on the `.fsi` only, probe F).

### Stage-3 decl-kind identities (all land on the `.fsi`)

| signature decl | use | verdict |
|---|---|---|
| `val internal x` | `A.x` | resolves → `.fsi` (project-visible) |
| `val private x` | `A.x` | resolves → `.fsi` **+ FS1094 inaccessible** (never in a clean fixture) |
| `module internal M` | `A.M.y` | resolves → `.fsi` |
| `val (\|Even\|Odd\|)` / `val (\|DivBy\|_\|)` | `Even` / `DivBy 3` | recognizer span in `.fsi` |
| `exception E` / `type Alias = int` | `A.E`, `A.Alias` | resolves → `.fsi` |
| `type R = { X }` (visible) | `r.X` | field ident in `.fsi` |
| nested `module Inner` + `val` | `A.Inner.shown` / `.hidden` | shown → `.fsi`; hidden → FS0039 |

## Design

The `ItemId → (file, def)` routing hands us the design: **the signature exports
the module's surviving surface with signature identity; the implementation's
hidden members are dropped; the exports fold at the implementation's Compile
position.** Everything flows through the existing `ExportDecl` / `ExportedItem`
currency, emitted from `SigDecl`s.

### Input model: interleave signature files into the fold

`resolve_project(&[(SourceFile, QualifiedName)], &AssemblyEnv)` where
`SourceFile = Impl(ImplFile) | Sig(SigFile)`, in Compile order (as msbuild and
FCS already order them), each tagged with its QNOF (§pairing). Keep a thin
impl-only overload / `From<ImplFile>` so the impl-only test population needs only
a mechanical wrapper.

The signature is a real file in `ResolvedProject` — it needs a `Def` arena its
exports' cross-file def pointers address — but it **owns no `ItemId` range of its
own**: its surviving exports fold at the *impl's* slot (§Stage 2). So
`ResolvedProject.file(sig_idx).exports()` is empty; the sig contributes only
`Def`s reached through the exposed items' cross-file def pointers. (This corrects
an earlier draft that gave the sig its own `ItemId` range *and* asked
`item_def` to follow a cross-file pointer — mutually inconsistent; the impl owns
the range and provenance, the sig owns the def.)

### Pairing rule (FCS's `QualifiedNameOfFile`)

QNOF is FCS's `QualifiedNameOfFile` (`ParseAndCheckInputs.fs`), not a
hand-approximation — a mismatch either over-pairs (suppresses an unrelated impl)
or under-pairs (leaks hidden names):

- **module-headed** (`module M`) → QNOF = `M`, AST-derived.
- **otherwise** (namespace-headed, multi-fragment, anonymous) → QNOF from the
  filename via `CanonicalizeFilename` (capitalise stem) then
  `DeduplicateParsedInputModuleName` (directory/order disambiguation of equal
  names). The raw path stem is wrong (misses both).

A signature pairs with the **first following** impl of equal QNOF (probe X3);
later same-QNOF impls are independent unsigned fragments. The fold input carries
QNOF because the filename-derived case needs the path (the LSP holds it,
`ProjectParses.paths`). **The QNOF/pairing computation is itself
FCS-differential-tested** — a fixture sweep feeds FCS a file set and asserts
sema's pairing matches (observed through which names a downstream file can and
cannot resolve) — so `CanonicalizeFilename`/`DeduplicateParsedInputModuleName`
fidelity is a checked property, not a judgement call. Namespace-headed signatures
pair through the same path (probes G, G2), so the blanket `.fsi` refusal is
removed outright.

### Correctness-over-availability framing

The signature only ever *removes* names from the boundary, so every step moves
monotonically toward FCS:

- **Hide = drop.** A member the signature omits, or declares `private`, or
  declares opaquely (a case/field of an opaque type) simply produces **no
  export**. It then resolves exactly as FCS resolves it: to the merged
  referenced-assembly member if one collides (probe: `Shared.bar`→assembly), else
  `Deferred` where FCS errors (FS0039) — an honest D5 gap, never a wrong commit.
  No blockers, no inaccessible entries: those would force `Deferred` where FCS
  binds the assembly.
- **A multi-fragment public export is untouched.** When `module N.A` is split
  across an unsigned `First.fs` (public `let x`) and a signatured `Pair.fs`
  (hidden/private `x`), dropping `Pair`'s `x` leaves `First`'s public `x` as the
  sole `value_exports[[N,A,x]]` entry → `N.A.x` resolves to `First.fs`, matching
  FCS (probe). No special recovery machinery — dropping *is* the recovery.
- **Expose = a certain commit.** Emit an `ExportedItem` only where the signature
  decl's path, identity range, and kind are certain (a plain `val x : T` under
  `module M` → path `[M, x]`, def = the `x` ident in the `.fsi`). Any decl kind
  not yet modelled emits no identity → the name stays `Deferred`/assembly, an
  honest coverage gap.

## Implementation plan

Each stage is its own branch, reviewable in isolation, gated on the full suite
plus the ignored corpus differentials.

### Stage 1: interleave signatures; drop paired impls' cross-file exports

**Dependencies:** none. **Behaviour change:** removes the over-export bug +
re-enables the fold; adds no new *project* commits (assembly fall-through
commits are gated by the screen below).

**Correction (2026-07-18, probe-forced): the signature screen.** The original
Stage-1 matrix had a hole: with every value export dropped and no signature
identity emitted, a **sig-exposed** name that *also* collides with a merged
referenced-assembly member would fall through and commit to the assembly —
but FCS binds the **`.fsi`** there (probe: `Shared.shown` with a colliding
`RefLib.dll` → the `.fsi`; the original sweep's RefLib exported only
`bar`/`asmOnly`, so this cell was never probed). Since Stage 1 cannot tell a
hidden member from an exposed one without reading the signature, it carries a
**screen** per `.fsi` (`SigScreen`): the module roots the signature constrains
plus a sound *over-approximation* of every name it could expose (each
non-trivia token's `idText` and its ident-shaped pieces). An assembly reading
under a screened root whose residual segments touch the name set **defers**
(`ProjectItems::sig_screened_path`, consulted by the tiered walk's shadow
predicate, plus the open-fold counterpart); a residual whose names are absent
from the whole signature text provably cannot be signature-exposed and falls
through exactly as FCS does. The screen is pushed at the **signature's**
Compile slot — which over-defers *intervening* files (FCS resolves those to
the assembly, probed above); deferral is the sound direction, and it keeps
the screen inside the signature's own threaded contribution for the
incremental fold. Bare names after an `open` of a signatured module all defer
(the module is marked hidden-valued, so the conservative project-module-open
machinery shadows earlier opens — load-bearing: a sig-exposed name must
shadow an earlier open's same-named value); the qualified forms keep the
per-name fall-through.

- Introduce `SourceFile` and rework `resolve_project` / the incremental fold /
  `thread_forward` to iterate `&[(SourceFile, QualifiedName)]`. A
  `SourceFile::Sig` produces a `ResolvedFile` whose own `resolutions` are
  `Deferred` (Stage 2 fills them) and which contributes nothing to `preceding`.
- Pair by QNOF (first-following, §pairing). A paired `SourceFile::Impl` is
  resolved exactly as today (internal `resolutions` unchanged — conclusion 2),
  but `thread_forward` **drops its value/case identity exports** (`Item`,
  `ActivePatternCase`). Keep only its `Module`/`Namespace` *header* decls, so
  `open M` and the module qualifier still see the module exists and the
  exact-module-path merge behaves as today. No value/case shadow is kept — a
  hidden `Foo.bar` must fall through to the merged assembly (probe), which
  dropping achieves.
- LSP: delete the `.fsi` refusal (semantic.rs:1085); parse each Compile item with
  the grammar its extension selects (`is_signature_file`, semantic.rs:2133 → a
  panic-safe `parse_sig_with_symbols` beside `cst_panic_safe.rs:24`), compute each
  file's QNOF, build the interleaved input. `ProjectParses` (semantic.rs:97)
  carries `SourceFile` + QNOF.
- Replace the pinned refusal tests (semantic.rs:2266/2296) with folds-correctly
  assertions for a `module M`- and a `namespace N; module M`-headed `.fsi`
  project.

**Why it is sound:** the paired impl's value/case identities are dropped and the
sig emits none yet, so the fold gains **no new project commit**; a member the
signature provably cannot expose resolves to the merged assembly or `Deferred`,
exactly as FCS (probe), while a possibly-exposed one is screen-deferred (FCS
binds the `.fsi`; committing the assembly there would be wrong — the
correction above). Timing is free: the module publishes no identity, so
intervening files (probe L) and self-references (probes K/K2) see nothing —
FCS's FS0039. Paired modules under-resolve (their exposed names stay
`Deferred` until Stage 2) — the honest D5 cost. Unsigned modules (probes J,
M, X3) fold for the first time.

**Oracle:** FCS-free `resolve_project` unit tests (a hidden `let` no longer
resolves to the impl binder; a non-`.fsi` sibling still does); an
**assembly-fall-through** fixture (built ref DLL via `BORZOI_FCS_EXTRA_REFS`)
asserting a hidden `Shared.bar` resolves to the *assembly*, not `Deferred` and
not the impl — the exact behaviour the earlier draft got backwards; an LSP e2e
that a `module M`-headed `.fsi` project folds where it returned `None`; the
ignored `resolve_corpus_diff` / `resolve_project_diff` gates stay green.

### Stage 2: the signature exports its surviving surface (signature identity)

**Dependencies:** Stage 1. **Behaviour change:** first new commits — cross-file
uses of a signature's *exposed* surface resolve to the `.fsi`.

- Give `SourceFile::Sig` a `Def` arena for its ident ranges, and produce the
  module's **value/case identity** exports from `sig_decls()` — the surface the
  existing `Item` currency carries a def for (conclusion 1):
  - `SigDecl::Val` with a plain `ident()` → an `Item` value export at
    `[module.., name]`, def = the `x` ident in the `.fsi`. Skip active-pattern-
    and operator-named vals for now (Stage 3). **Skip `val private`** — a private
    value is FS1094 cross-file (never a clean commit) and, on collision, falls
    through to the assembly; dropping it keeps a same-path earlier public fragment
    resolving (probe), so no access-root modelling is needed here.
  - `SigDecl::Types` with a *visible* union/enum representation → the case
    `Item`s + type-qualified case paths (existing `CaseKind` /
    `type_qualified_cases`). An **opaque** representation emits **no** case/field
    identities (opacity hides members) — the crux the impl walk cannot express.
  - `[<AutoOpen>]` is read from the **signature** header (conclusion 6).
- **Fold at the impl's slot; redirect only the def (conclusion 4).** On reaching
  a `Sig`, stash its identity `export_decls`. On reaching the paired `Impl`,
  resolve the impl against a `preceding` *without* the sig (self-refs stay
  `Deferred`), then fold the sig's stashed identities **as the impl's
  contribution**: the `ItemId` range, `item_base`, and `item_file_bases` push all
  belong to the **impl's** file index, monotonic, exactly like an ordinary file.
  The *only* signatured-specific change: `ExportedItem::def` becomes an explicit
  cross-file `(sig_file_idx, DefId)`, and `item_def` (model.rs:1875) follows it
  instead of resolving within the `ItemId`-owning file. So provenance = impl,
  def = sig, consistently. Test: `item_def` on a cross-file sig export returns the
  `.fsi`'s index, **and** a colliding later-file contribution loses to the
  auto-opened sig member (the provenance direction, conclusion 4).
- **Type-name and module-qualifier go-to-def stays `Deferred`.**
  `Type`/`Module`/`Namespace` exports carry no identity, and such uses are already
  `Deferred` for impl files today. Stage 2 makes *value/case* uses resolve to the
  `.fsi` and honours *opacity*; a downstream `A.SomeType` / `open A` qualifier
  remains `Deferred` — matching, not regressing, today. Type/module identity is a
  Stage-3+ model extension.

**Oracle:** a **signature-aware** `resolve_project_diff` harness — extend
`temp_fs_file` (common/mod.rs:817) to honour a `.fsi` label and feed
`invoke_fcs_dump_project` (common/mod.rs:257) an interleaved path list; assert
certain-implies-exact against `uses-project` for the whole probe matrix (exposed
val/case → `.fsi`; hidden/opaque → assembly or `Deferred`/unrecorded). Include the
**non-adjacent auto-open collision** fixture (provenance = impl slot) and the
**multi-fragment** fixture (earlier unsigned public `x` wins over a later
signatured hidden `x`). Every fixture `uses-project`-diagnostics-clean. Corpus
gates green.

### Stage 3+: enrich the modelled signature surface

Each an FCS-differential-gated slice; the semantics are pinned by the sweep:

- **Accessibility (finer half)** — `val internal` / `module internal`
  (project-visible → accessible export), on top of the private→drop of Stage 2,
  via `access_root_len`.
- **Active-pattern `val`s** — `val (|Even|Odd|)` / `val (|DivBy|_|)`, wired to the
  Stage-3a active-pattern-case export path (`docs/export-decl-model-plan.md`),
  recognizer span in the `.fsi` as identity.
- **Exceptions, module/type abbreviations, records (visible field identity +
  opaque-record hiding), nested-module signatures** (recursive `sig_decls()`).
- **Type / module-qualifier identity** — extend `Type`/`Module`/`Namespace`
  exports (and `Resolution`) so `A.SomeType` / `open A` resolve to the `.fsi`.

## Risks

- **QNOF fidelity.** `CanonicalizeFilename` + `DeduplicateParsedInputModuleName`
  are stateful (dedup depends on prior files); port them faithfully and pin with
  the pairing differential (§pairing). The first-following-impl pairing must match
  FCS's, including the multi-same-name-fragment case (probe X3).
- **Cross-file `def` addressing (Stage 2).** Extending `ExportedItem::def` to
  `(file_idx, DefId)` touches every reader of the export's def; keep
  `ItemId`/`item_file_bases` monotonic and impl-attributed — the *only*
  signatured-specific behaviour is the def redirect. Audit `resolved_def` /
  `token_classifier` for a latent "def lives in the `ItemId`-owning file"
  assumption.
- **Incremental fold reuse.** `resolve_project_incremental_*` (`same_tree`) must
  treat sig and impl as distinct and re-fold a module when *either* half changes
  — and, since the sig folds at the impl's position, invalidate the impl's
  contribution when the *sig* edits though the impl tree is unchanged. Cover with
  an incremental-≡-batch test that edits only the `.fsi`.
- **Bonus surface: `.fsi` buffers as query targets.** Once a signature is a real
  `ResolvedFile`, project queries on a `.fsi` buffer get a real resolution instead
  of the single-file fallback. Stage 1 keeps sig `resolutions` inert, so guard the
  LSP export consumers (semantic.rs:2441, 4448) against an empty sig surface;
  richer `.fsi` query support is out of scope until it has its own plan.

## References

- Fold + threading: resolve.rs:371 (`resolve_project`), :392
  (`resolve_project_impl`), :466 (`thread_forward`), :596 (incremental).
- Boundary currency: model.rs:587 (`extend_with`), :816
  (`FileExportIndices::from_decls`), :1050/:1071 (`ExportDecl`/`ExportDeclKind`),
  :1228 (`ExportedItem`), :1455 (`export_decls`), :1875 (`item_def`), :352
  (`is_project_value_prefixed`), :87-115 (module/assembly merge rules).
- CST signature surface: parser/mod.rs:253 (`parse_sig_with_symbols`),
  syntax/mod.rs:201 (`SigFile::modules`), :233 (`sig_decls`), :323
  (`ValDecl::val_sig`), :1100 (`ValSig`), generated/union_decls.rs:895 (`SigDecl`).
- LSP wiring: semantic.rs:1010 (`build_parses`), :1085 (the refusal), :2133
  (`is_signature_file`), :2266/:2296 (refusal tests), :97 (`ProjectParses`);
  cst_panic_safe.rs:24; diagnostics.rs:53 (`SourceKind`).
- Test harness: common/mod.rs:257 (`invoke_fcs_dump_project`, `BORZOI_FCS_EXTRA_REFS`
  for the assembly fixtures), :817 (`temp_fs_file`), :1528
  (`parse_fcs_uses_project`); `resolve_project_diff.rs`.
- Related boundary design: `docs/export-decl-model-plan.md`.

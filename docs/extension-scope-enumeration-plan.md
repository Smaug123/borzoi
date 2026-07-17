# Extension-scope enumeration plan — from *presence* to *by name*

> **Status:** EX-0 (static extension names in the assembly index, #935), EX-1
> (name-keyed assembly sources, #937), EX-2 (name-keyed `open`s, #975), and —
> after the abandonment recorded below was salvaged — **EX-3 §2(d)** (the
> name-keyed attribute trigger; the five-stage arc #994 → #996 → #999 → #1002 →
> the gate-consumption stage) have landed. The gate's attribute trigger now
> reads the resolver's per-attribute **type resolutions** (suffix-first through
> the shared FCS-validated walk, exported as
> `ResolvedFile::attribute_resolutions` and gated by the `attrs` differential +
> the generative matrix + the corpus sweep): an attribute resolving to a
> concrete non-`ExtensionAttribute` type no longer defers the file. §2(a)/(b)
> (the augmentation name sets, own- and cross-file — #1011/#1010) and **AO-2**
> (the attribute side's project-auto-open presence defer deleted as redundant;
> see "§2(c) revisited") have landed since. Remaining EX-3 work: AO-1 (the
> extension gate's auto-open presence triggers, stacked on AO-2), §2(c)'s
> remnant (an attribute resolving *to* `ExtensionAttribute` still
> presence-defers — name-keying its container's members), and the documented
> §2(d) coverage frontiers — multi-segment attribute paths and bare cross-file
> project attribute types defer wholesale. An `open` the resolver cannot
> name-key — a project module/namespace, an assembly module / `open type`, or
> an opaque / dropped-path open — still defers the whole file.
>
> EX-1's real cap was not the name-keyed queries (mechanical) but a single
> *global* "defer wholesale" bit for a *contested* auto-open: FSharp.Core
> auto-opens `Microsoft`, which the BCL also declares, so it fired for every real
> project. The fix mirrors FCS — a contested auto-open is applied
> **contributor-scoped** (only the contributing CCU's namespace entity opens), so
> the env records `(contributing assembly, namespace)` and the gate asks whether
> *that* assembly's content there declares the called name. Follow-up review then
> closed five more completeness/soundness gaps in the same spirit (dropped
> descendants, sibling-only contested targets, an unread/non-authoritative
> FSharp.Core extension index, and the resolver's hardcoded implicit-open
> fallback) — see the EX-1 entry under
> [Landed stages](#landed-stages-one-line-each).

This follows up [OV-6 / OV-9](overload-resolution-plan.md). The overload
engine's extension-absence gate deferred a call whenever *any* extension source
was in scope, even one that provably cannot contribute the called name — what
zeroed overload coverage on every real project (they all reference FSharp.Core,
whose implicit auto-opens are always a surface; and they nearly all have an
`open`). OV-9's differential is the instrument that makes refining it safe.

## 1. Why *by name* is the right granularity (probed, not assumed)

Probed 2026-07-12 through `fcs-dump overloads`:

```fsharp
module Ext =
    type System.String with
        static member Compare (x: int) = 3.0
        member this.Substring (x: bool) = 7.0
open Ext
let a = System.String.Compare("a", "b")   // call:static-overloaded, System.String  ⇒ Int32
let b = System.String.Compare 1           // call:extension,        P2.Ext          ⇒ Double
let c = "hi".Substring 1                  // call:instance-overloaded, System.String ⇒ String
let d = "hi".Substring true               // call:extension,        P2.Ext          ⇒ Double
```

An in-scope extension member of a name joins that name's group *flat* (plan §2.1)
and competes: the intrinsic wins where it applies, the extension wins where only
it does. It **cannot** affect a call of any *other* name. So the sound commit
precondition is exactly:

> no in-scope extension member **named `M`** exists

— not "no extension source exists". The gate's current conflation of the two is
pure over-approximation, and it is where the coverage went.

## 2. What the refinement must not lose

OV-6 chose presence over enumeration *after eight review rounds*, each of which
surfaced one more un-enumerated source (nested / module-shaped / contested
auto-opens, enclosing namespaces, same-file / `private` auto-opens, same-file
`[<Extension>]`), and **every omission is an unsound commit**. That history is
the design constraint, so the refinement is deliberately *not* a re-derivation of
FCS's extension-scope construction. Instead:

**Keep the source set exactly as the presence gate already computes it** — that
enumeration is complete by construction and is not touched — and refine each
source from a *boolean* to a **name set**:

| source (unchanged) | today | after |
|---|---|---|
| assembly-level auto-open surface | `has_any_assembly_auto_open()` ⇒ defer | the extension names those surfaces contribute; `Unknowable` ⇒ defer |
| a referenced extension in the file's in-scope namespace chain | `namespace_has_extension(ns)` ⇒ defer | `namespace_extension_names(ns)`; a dropped type / skipped member in that namespace ⇒ `Unknowable` |
| an explicit `open` | *any* `open` ⇒ defer | the opened target's extension names; an unresolved / project-module / opaque open ⇒ `Unknowable` (EX-2) |
| project extension sources (own `[<AutoOpen>]`, preceding file's auto-open / `[<Extension>]` / augmentation, own `type … with`, **any attribute**) | ⇒ defer | unchanged — still presence-based (EX-3, later) |

A source whose name set cannot be computed **stays a wholesale defer**. So every
step strictly *shrinks* the deferred set and can never grow the committed one
beyond what a complete name enumeration licenses; a bug in a name set can only
show up as a *missing* name, which is why EX-0 exists and why the OV-9
differential and `extension_visibility_matrix` gate every stage.

## Landed stages (one line each)

- **EX-0** (#935) — static extension-member names in the assembly index: a
  parallel [`Entity::static_extension_member_names`](../crates/assembly/src/model.rs)
  list read from the same pickled val flags with the `IsInstance` bit *cleared*,
  `Unknowable`-bounded like the instance index and pinned by the `FsExtIndex`
  fixture. Prerequisite: OV-0.5's instance-only index would report "no extension
  named `Compare`" for a module declaring `static member Compare`, so a
  name-keyed static-call gate must not read an instance-only index. (C#-style
  `[<Extension>]` methods stay on the instance side — FCS hard-wires their
  `IsInstance` to true.)

- **EX-1** (#937) — name-keyed assembly sources: `AssemblyEnv::extension_named_in_scope`
  walks the same three sources the presence gate did (global unknowables; the
  auto-open surfaces; the file's in-scope namespace chain), refining each from a
  boolean to a per-name test keyed by call shape (`is_static`), unioning EX-0's
  two F#-native indexes with the C#-style `[<Extension>]` method names. The real
  cap was the *global contested-auto-open* bit (FSharp.Core auto-opens `Microsoft`,
  the BCL also declares it), now applied **contributor-scoped** (see the status
  note above). An unknowable surface still defers for every name.

- **EX-2** — name-keyed `open`s: the resolver classifies each explicit `open`,
  exporting the **assembly** namespace paths a clean `open <namespace>` brings into
  scope ([`ResolvedFile::open_extension_namespaces`](../crates/sema/src/resolve/model.rs))
  and a wholesale [`open_extension_unknowable`](../crates/sema/src/resolve/model.rs)
  bit for anything it cannot name-key (a project module/namespace — EX-3 — an
  assembly module or `open type`, or an opaque / vetoed / dropped-path open). The
  gate folds the opened namespaces into its in-scope namespace set (opening a
  namespace makes its extensions in-scope exactly as an enclosing one does, so the
  same `namespace_extension_names` query serves both) and defers the file when the
  unknowable bit is set. The accumulation is *file-global* — an `open` in any
  nested module folds in for the whole file, an over-approximation that only adds
  deferrals — matching the file-global `ExtensionScope`.

---

## Still to do

### EX-3 — project-side extension sources (later)

The file's own `type … with`, a preceding Compile-order file's auto-open /
`[<Extension>]` / augmentation, and the **any-attribute** trigger (an attribute may
*alias* `ExtensionAttribute` — `type ExtAttr = …ExtensionAttribute` — so matching
the written name is unsound; the fix is resolving each attribute's type through
abbreviations, which the resolver can do). Each needs the resolver to export
per-module extension-member *names* for project code, the mirror of OV-0.5's
index for the assembly side.

#### Grounded decomposition (probed 2026-07-16, against the post-EX-2 resolver)

The four project-side triggers do **not** decompose into independent wins: their
real-world value is gated on the *hardest* one. The dominant defer cause in a real
multi-file project is [`preceding_declares_extension_source`](../crates/sema/src/resolve.rs)
— it is namespace-blind and **accumulates**, and it ORs in *any* preceding file's
`[<Extension>]` / augmentation / auto-open **or any attribute at all**
(`impl_file_declares_extension_or_augmentation`). So once one early file carries a
single attribute (`[<EntryPoint>]`, `[<Literal>]`, …), every later file defers. And
the same-file `any-attribute` trigger backstops the C#-style-`[<Extension>]` case
for name-keying elsewhere, so a file with any attribute defers regardless of how
well augmentations/auto-opens are name-keyed. **Net: EX-3's real coverage hinges on
name-keying the attribute trigger (below); the rest is sound but narrow in
isolation.**

Sub-parts, cheapest first, with the machinery gap each faces:

- **(a) Same-file `type … with` augmentation → name set.** *Tractable, low-risk,
  narrow.* The member walk (`add_type_members`), per-shape name extraction, the
  instance/static bit, and the un-nameable→`suppress_member_emit` (⇒ `Unknowable`)
  signal all already exist in `crates/sema/src/resolve/types.rs`. The resolver even
  splits own-type intrinsic augmentation (names kept in `type_members`) from
  optional/type extension (`index_augmentation_members`'s second branch, which today
  **discards** names into `unindexed_augmented_names`). Work: run the walk for that
  second branch too, collect a raw `(name, is_static)` set + an unknowable bit
  (mirroring `Entity::{extension_member_names, static_extension_member_names}`), and
  refine the gate's augmentation arm. Both augmentation kinds are extension-like for
  our gate (an own-type augmentation compiles to a module static our intrinsic group
  does not see), so both are collected. Value is narrow: it only bites when the file
  has an augmentation **and no attribute anywhere** (else the any-attribute trigger
  fires) — uncommon.
- **(b) Cross-file (preceding) augmentation / auto-open → name set.** *Mechanical,
  low-risk.* A `bool → name-set` generalization of the accumulator in
  [`resolve_project`](../crates/sema/src/resolve.rs) (Pattern B: stamp the
  accumulated-so-far set onto each `ResolvedFile`). Reuses (a)'s walk. Value still
  gated on the attribute contribution staying presence-based.
- **(c) Auto-open module members → name set.** *Medium.* No per-module project name
  index exists (only paths, `auto_open_module_paths`); needs one, but its
  augmentation members reuse (a)'s walk. A **module-level `[<Extension>] let` is a
  *value*, not an extension** (FCS adds it through vals — see the `CoreExtAttrLets`
  fixture), so those are *not* collected; a **nested `[<Extension>] type`** inside
  the module is C#-style and needs (d).
- **(d) The any-attribute trigger → resolve attribute types.** *High — substantial
  new machinery, and the value-bearing one.* Attributes are matched **only by
  written last segment** today (`attrs_auto_open` etc.), and there is **no**
  type-abbreviation alias table (`resolve_type_defn` resolves an `Abbrev`'s target
  but records no `alias → target` mapping; `module_aliases` covers only *module*
  abbreviations). Name-keying the attribute trigger soundly needs: a new attribute
  **type** resolution (through local `type X = …ExtensionAttribute` chains and
  cross-file/assembly aliases) that recognises `ExtensionAttribute`, then — for a
  recognised C#-style `[<Extension>]` container — its static method names. This is
  the piece to sequence **last** and to design deliberately (cross-file/assembly
  abbreviation resolution has real soundness edges); it is *not* a mechanical
  change, and building it before deciding its shape is the wrong-direction risk.

  **Attempted "minimal" (2026-07-16) and abandoned as unsound — two ways.** A
  first cut classified only *type-definition* attributes (a `[<Extension>]`
  container type), certifying an attribute "not an extension" when it resolved via
  `opened_type_target` to a concrete non-abbreviation assembly type ≠
  `ExtensionAttribute`. GPT-5.6 found two soundness holes, both confirmed:
  1. **Member-level `[<Extension>]`.** Under `CSharpExtensionAttributeNotRequired`
     (the language default on the toolchains FCS runs here), a `[<Extension>]`
     *static member* in a plain, non-`[<Extension>]` type **is** an extension — no
     container attribute needed. So classifying only container attributes is a
     false negative; member attributes must count too.
  2. **Bare-name aliasing is unrulable-out.** To *commit* in the presence of a bare
     `[<Foo>]`, one must prove neither `Foo` nor `FooAttribute` is (an alias of)
     `ExtensionAttribute`. But `opened_type_target` deliberately **declines** bare
     single-segment type resolution in a namespace ("we do not index project types
     across files"), so a cross-file `type FooAttribute =
     …ExtensionAttribute` in scope is invisible — and FCS would resolve `[<Foo>]`
     to it. Certifying `Foo` safe from the assembly side is then a false negative.
     The only bare attributes provably safe are those whose name no project type
     could shadow — which, without a **cross-file project-type-by-name index**, is
     none of the short forms real code writes (`[<Literal>]`, `[<CLIMutable>]`, …).

  So §2 is **blocked on infrastructure**, not minimal: it needs the resolver to
  *faithfully* resolve an attribute's type (bare names included, member-level
  included) — i.e. export attribute-type resolutions the way it exports value
  resolutions — rather than a bespoke classifier re-deriving F#'s attribute-name
  precedence (the EX-1 "don't re-derive the resolver's decisions" lesson, again).
  The old "any attribute ⇒ defer" trigger stays until then: sound, because it makes
  no commit claim.

  **Second attempt (2026-07-16) — closed holes 1–2 but hit two more; abandoned.**
  Added the member-level descendants walk (hole 1) and a project-wide
  type-simple-name index (`ProjectItems::project_type_simple_names` + resolver
  `own_type_simple_names`) so any attribute a project type could shadow defers
  (hole 2, *more* complete than the resolver's own bare cross-file bound). GPT-5.6
  then found two more, both from the classifier's assembly side using
  `opened_type_target` rather than the real type-path resolver:
  3. **Qualified project alias.** The project-name guard ran only for
     single-segment candidates, so `[<System.Object>]` with a project
     `System.ObjectAttribute = …ExtensionAttribute` certified safe (the concrete
     `System.Object` set "resolved"; the shadowed suffix contributed nothing). Fix
     is cheap — check the last segment for every candidate — but see hole 4.
  4. **Assembly auto-open type shadow.** `opened_type_target` does **not** apply
     `resolve_type_path`'s `ShadowVeto` (an in-scope assembly `[<AutoOpen>]` module,
     or an unknowable-abbreviation namespace, that could supply `ObjectAttribute =
     ExtensionAttribute`). So `[<Object>]` next to such a module certified safe. No
     amount of name-indexing catches an *assembly-side* aliased attribute — only
     applying the veto does.

  **Conclusion — the required change is precise.** The classifier must resolve each
  attribute candidate (the written name *and* the synthesized `…Attribute` suffix)
  through **`resolve_type_path`'s own tiered walk *with* its `ShadowVeto`** — the
  only thing that applies every shadow source (auto-open, unknowable-abbrev,
  project) uniformly. Because the suffix candidate has no source token,
  `assembly_type_path_records` / `resolve_assembly_path_tiered` must first be made
  **token-free / path-based** (a decision core returning the resolved handle +
  owns-path, with a thin recording shell over it) — a refactor of the hottest,
  most precedence-sensitive resolution code, guarded by the resolve differentials
  (`resolve_assembly_diff`, the corpus sweep, `extension_visibility_matrix`). That
  refactor is the actual EX-3 §2 infrastructure; four bespoke shortcuts have each
  missed a distinct shadow source, so it is not skippable.

  **ABANDONED after a six-round doom loop (2026-07-16). §2(d) is intractable as a
  per-attribute absence proof; it needs a different foundation.** The tiered-walk
  refactor prescribed just above *was* carried out — and §2(d) still failed. Two more
  attempts followed the finding above:
  - A *token-free refactor* of the tiered type-path walk (`assembly_type_path_core`,
    generic `AssemblyPath`, `resolve_type_path` = `decide_type_path` + shell) so the
    classifier could resolve each candidate (written + `…Attribute` suffix) through
    the resolver's own walk. The refactor was behaviour-preserving and green, but the
    classifier on top failed **four** reviews (rounds 1–4): an unquoted `global.`
    marker; an unknowable-abbreviation namespace at a qualified candidate; a quoted
    `` ``global`` `` module; a dropped `ExtensionAttribute`; a higher-priority tier's
    hidden alias masked by a lower concrete match; a nameless `[<>]`; a cross-assembly
    colliding type key hidden behind an inaccessible first-wins slot.
  - A *pivot* to "prove absence with complete queries only" — name-key **only
    single-segment** attributes (defer every multi-segment / nameless one), and check
    each searched namespace with complete queries (`public_entities_named` not the
    first-wins `lookup_type`; dropped types; unknowable abbreviations; auto-open
    modules; assembly-module/namespace merges). It reverted the refactor entirely.
    This too failed review (rounds 5–6): the assembly-module/namespace merge; then
    manifest auto-opens that `record_assembly_auto_opens` moves into
    `auto_open_module_handles` / contested `contested_auto_opens` (invisible to
    `assembly_prefixes_by_priority`); and dropped types at a *module-path split*
    (a dropped same-FQN module whose nested alias merges into the namespace).

  **Root cause (definitive).** Proving a *universal negative* — "no in-scope type
  named `X` or `XAttribute` is (an alias of) `ExtensionAttribute`" — requires
  enumerating **every** implicit type-scope source F# has (namespace types, opened
  namespaces/modules, assembly-module/namespace merges, manifest + contested +
  same-file auto-opens, dropped types at every split, unknowable abbreviations,
  cross-assembly collisions, project/in-file aliases, …). The resolver's data
  structures scatter these across many indexes, several *deliberately lossy*
  (first-wins `by_type`, accessibility filter, single handle) because they exist to
  *find a binding*, not to *prove nothing matches*. Every review round surfaced one
  more source the proof missed — the exact shape of OV-6's original eight rounds, and
  the reason OV-6 chose presence over enumeration. Six rounds here confirm the same
  verdict for the attribute trigger: a per-attribute absence proof cannot reach the
  strict soundness invariant, and each patch only defers the next hole.

  **Conclusion / the actual path forward.** The old "any attribute ⇒ defer" trigger
  stays (sound; it makes no commit claim). Name-keying §2(d) is **not worth pursuing
  as a classifier**. If it is revisited, the only sound foundation is to have the
  **resolver itself** answer "does the bare type name `X` resolve, in this scope, to
  (an alias of) `ExtensionAttribute`?" as a first-class query — sharing the *one*
  resolution path that `resolve_assembly_diff`'s *certain-implies-exact* property
  already validates against FCS — and to gate that query behind a **new FCS
  attribute-resolution differential** (the "new differential per stage" this plan
  always demanded, which was skipped and is exactly why six rounds of intelligence
  were needed to find these holes). Without that differential and that shared query,
  §2(d) should not be attempted again. The abandoned pivot (single-segment +
  complete-query, sound for all *realistic* code but with the documented theoretical
  residuals above) is preserved on branch `ex3-attr-single-seg-attempt` should the
  soundness/coverage trade-off ever be reconsidered for this agent-focused LSP.
  *That foundation is now being built — see
  [§2(d) revisited — the salvage plan](#2d-revisited--the-salvage-plan-2026-07-17).*

Recommended order: (a) → (b) → (c) → (d), but note (a)–(c) move the *new* EX-3
differential without moving a real corpus until (d) lands. Consider whether the
whole-project name-resolution differential (`resolve-real-project-diff`) is the
right instrument to measure EX-3, since the OV-9 corpus is open/attribute-free by
construction.

#### §2(c) revisited — crushing the auto-open triggers (planned 2026-07-17)

With §2(a)/(b) (augmentation name sets, own- and cross-file) and §2(d)
(attribute-type resolution) landed, the auto-open **presence** triggers are in
a different position than when this plan was written: the per-module member
index §2(c) originally called for turned out to be **needed nowhere** — not
for the extension gate (whose auto-open triggers §2(a)/(b)/(d) subsume), and,
as AO-2's probing showed, not for the attribute-resolution side either (the
type-name flavour already exists three ways over). Two stages, each with its
own oracle, each a deletion; AO-2 must land first (see AO-1's dependency
note).

**The subsumption argument (to be probed, not assumed — stage AO-1's first
job).** A project `[<AutoOpen>]` module can contribute an in-scope extension
member through exactly three content kinds, and every one is already covered
by a *name-keyed or attribute-keyed* signal that is collected **file-globally**
(both walks run inside nested modules, auto-open or not, and §2(b) threads
them cross-file):

1. a `type … with` augmentation inside it →
   [`collect_augmentation_extension_names`](../crates/sema/src/resolve/types.rs)
   collects its member names into the §2(a)/(b) sets;
2. a C#-style `[<Extension>]` type (or member) inside it → the §2(d)
   attribute machinery resolves the attribute to `ExtensionAttribute` and
   sets `attributes_may_declare_extension` — the wholesale bit fires for that
   file and all later ones;
3. a module-level `[<Extension>] let` → **a value, not an extension** (FCS
   folds module contents through vals, where the C#-style predicate never
   runs — fsi-verified, pinned by the `CoreExtAttrLets` fixture) — and even
   if a toolchain change ever revisited that, the `[<Extension>]` *attribute*
   already trips signal 2. Plain `let`s / `type`s contribute nothing to any
   method group.

So the gate's `own_declares_auto_open` and the cross-file
`!exportable_auto_open_module_paths.is_empty()` terms are pure
over-approximation on top of signals 1–2.

- **Stage AO-1 — delete the extension gate's auto-open presence triggers.**
  *Dependencies: §2(a)/(b) (landed), and **AO-2** — a correction to this
  plan, which called the stages independent: every `[<AutoOpen>]` module
  necessarily carries the `[<AutoOpen>]` attribute, and before AO-2 the
  stage-4 narrowing deferred that very attribute in exactly the files AO-1
  targets, keeping the wholesale defer alive through
  `attributes_may_declare_extension`. AO-1 alone is observationally
  invisible.* Drop the two terms from `ExtensionScope::of` /
  `wholesale_extension_contribution`; the signals themselves stay (their
  other consumers — the type-shadow veto, the open fold — are untouched).
  Oracle: an fcs-dump probe of the three content kinds above (the plan's
  rule: probed, not assumed), then E2E cases pinning each — an auto-open
  module of plain `let`s no longer defers a later file's overloaded call
  (FCS-diffed commit); one containing an augmentation defers exactly the
  augmented names; one containing `[<Extension>]` defers wholesale via the
  attribute bit; a **private** auto-open module behaves identically for the
  own-file case — plus the extension-visibility matrix, the OV-9 floors, and
  the corpus differential unchanged.

- **Stage AO-2 — delete the attribute side's project-auto-open presence
  defer (landed; executed as a *deletion*, not the index this plan
  prescribed).** The plan called for a per-auto-open-module type-name index;
  probing before building showed the index already exists in three
  overlapping forms, making the presence defer at the head of
  `attribute_candidate` fully redundant:

  1. *Supplying* the candidate: the §2(d) pre-scan is
     `file.syntax().descendants()` — file-global at **all depths**, every
     block, exceptions included — so any name an auto-open module declares is
     in `own_type_simple_names`, threads cross-file as
     `project_type_simple_names`, and `project_type_named` already defers
     those candidates in every non-in-file arm.
  2. *Contesting* an in-file hit (an auto-open module redeclaring the name
     later — FCS binds latest-wins): `decide_type_path`'s first check,
     `auto_open_type_shadow_names`, models exactly that positional contest
     for a same-block auto-open, name-keyed and populated regardless of the
     anonymous root (probed: the contest already deferred under an anonymous
     root, where the presence defer was never recorded). An auto-open
     `exception` is covered by the in-file arm's file-global exception
     guard.
  3. Straddles: a preceding *file's* auto-open import position is always
     earlier than any current-file definition, so an in-file hit is FCS's
     winner there; a candidate those modules could supply is covered by (1).
     FCS probes recorded on the way: a **same-named** later `namespace` block
     sees the earlier block's auto-open; a *differently*-named block does
     **not** (FCS errors).

  One genuine hole in that argument survived to review (codex, first round):
  the **three-block straddle** — a block-1 *direct* type of the name, a
  block-2 auto-open redeclaration, the attribute in block 3. FCS binds the
  auto-open's type (its import outranks the earlier direct definition), while
  `lookup_type_def` retains block 1's and the block-scoped shadow guard was
  cleared — a wrong-binder commit. The fix is the own-file, position-blind
  slice of the index this stage originally prescribed: a file-global pre-scan
  of the type/exception names declared **directly inside any `[<AutoOpen>]`
  module** (`own_auto_open_type_names` — direct children only; a nested
  plain module's types are not bare-visible, a nested auto-open pre-scans
  itself), deferring the in-file arm for exactly those names. Position-blind
  over-approximation: an in-file def declared after the import would win in
  FCS and could commit — declining there is sound.

  Landed as: the presence check deleted; regression tests in
  `attr_resolution_diff` (`*auto_open*`: recovery, supplying, contest,
  anonymous-root, cross-block straddle both flavours, cross-file
  name-keying); an auto-open dimension in the generative matrix (84 → 164
  cells, commits 52 → 172, floor ratcheted 8 → 160, zero disagreements); the
  corpus sweep re-measured at **149** exact commits (from 117 after the
  stage-4 narrowing, 150 before it — the drop recovered), zero
  disagreements.

- **Stage AO-3 (out of scope, pointer only).** The same per-module index
  extended to *value* names could eventually lift the resolver's
  `opaque_dotted_open` / hidden-value conservatism for `open <project
  module>` and cross-file auto-open folding — a much larger arc through the
  open machinery that deserves its own plan and its own FCS differentials.
  Explicitly not part of this one.

Adjacent follow-up (unblocked by §2(d), independent of AO-1/2): a container
the attribute machinery *resolves to* `[<Extension>]` currently keeps the
wholesale defer; collecting its static-member names with the §2(a) walk would
name-key the last presence-based project trigger.

#### §2(d) revisited — the salvage plan (2026-07-17)

The path forward prescribed above was probed and is **feasible**; this stages it.
Implement each stage on its own branch, stacked as necessary on previous
branches, so that a reviewer can review each branch in isolation.

**The reframe that dissolves the universal negative.** FCS resolves an
attribute deterministically: `ResolveAttributeType`
(`CheckExpressions.fs`) tries the written last segment with the `Attribute`
suffix appended *first*, then the name as written, through the **general**
`ResolveTypeLongIdent` — one concrete winning type per occurrence, abbreviation
aliases chased by construction. So the gate never needed "no in-scope type
named `X`/`XAttribute` aliases `ExtensionAttribute`" (the absence proof six
rounds could not close). It needs: *resolve this attribute occurrence* through
the resolver's own tiered walk — the one path that applies every shadow source
uniformly and that `resolve_assembly_diff` already validates as
certain-implies-exact — and check whether the *one* resolved terminal type is
`ExtensionAttribute`. A committed resolution is a positive, per-occurrence,
FCS-checkable claim; the only remaining negative (no higher tier shadows the
match) is the walk's own reasoning, exercised by every existing differential.
An occurrence the walk cannot resolve defers, exactly as an unknowable open
does today.

**The oracle is cheap.** FCS records every attribute-type resolution to the
name-resolution sink (`ItemOccurrence.UseInAttribute`, at the written name's
range) and `GetAllUsesOfAllSymbolsInFile` surfaces it as an `FSharpEntity`
carrying `(Assembly.SimpleName, FullName)` — the exact currency
`resolve_assembly_diff` diffs on. An attribute FCS cannot resolve sinks
nothing: decline-by-absence, matching the property shape. The doom loop's six
rounds also left the generator alphabet: every documented hole is a fixture or
generator dimension for the differential that was skipped the first time.

- **Stage 1 — the `attrs` oracle op + pinning tests.**
  *Dependencies: none. Implements: the "new FCS attribute-resolution
  differential" instrument.* `fcs-dump attrs <file>`: `dumpUses` filtered to
  `IsFromAttribute` entity uses, plus `Errors`, plus
  `TargetAssembly`/`TargetFullName` chasing an abbreviation chain to its
  terminal entity (probed: FCS reports the *abbreviation* entity itself, with
  a null `FullName`, for `type MyExt = ExtensionAttribute` — the terminal
  fields are what a consumer keys on). Oracle: hand-written pinning tests
  (`crates/sema/tests/all/attr_resolution_diff.rs`) — suffix-first synthesis,
  qualified paths, the alias chain's terminal type, member-level attributes,
  decline-by-absence with errors, the speculative module-attribute double-pass
  collapsed to one record per attribute.

- **Stage 2 — re-land the token-free tiered-walk refactor,
  behaviour-preserving.** *Dependencies: none (parallel with stage 1).
  Implements: the "required infrastructure" of the first finding.* The
  refactor from the doom loop (`assembly_type_path_core`, generic
  `AssemblyPath`/`TieredResolution`, `resolve_type_path` = `decide_type_path`
  + recording shell) — preserved at tag `ex3-doomloop-refactor-tip`, never
  reviewed in isolation because it landed bundled with the classifier. Land it
  *alone*, with zero test changes. Oracle: the entire existing suite
  (`resolve_assembly_diff` both directions, the corpus sweeps,
  `extension_visibility_matrix`, `overload_corpus_diff`) green, unchanged.

- **Stage 3 — the resolver resolves attribute types; the differential goes
  live.** *Dependencies: stages 1–2.* For each attribute occurrence, resolve
  the FCS candidate order (suffix first, then as written) through
  `decide_type_path`, and export the resolutions the way value resolutions are
  exported (they also serve go-to-definition/hover/SemanticClass on
  attributes). In-file attribute types resolve; a bare name a cross-file
  project type could supply defers (no cross-file bare-name project-type
  index — the pivot's `project_type_simple_names` is the over-approximating
  veto). Oracle: the differential proper — every doom-loop hole as a case,
  certain-implies-exact against `attrs` (our commit names FCS's
  `(assembly, full name)`; our decline makes no claim), plus a completeness
  floor (`[<Literal>]` must commit, or the gate stage gains nothing).

- **Stage 4 — generative + corpus sweep.** *Dependencies: stage 3.* A
  generative differential over the shadow-source alphabet (aliases at each
  tier, auto-open/contested/manifest surfaces, dropped types at splits,
  module/namespace merges, cross-assembly collisions, `global.`-rooted,
  nameless, member-level), plus an `#[ignore]`d `BORZOI_CORPUS` sweep
  reporting commit/decline rates over real attributes. Oracle: zero
  violations; the sweep's commit floor ratchets.

- **Stage 5 — the gate consumes it.** *Dependencies: stage 3 (4 gates the
  ratchet).* `ExtensionScope` drops `file_declares_any_attribute`; a file's
  attributes all resolving to non-`ExtensionAttribute` terminals contribute
  nothing; any resolving *to* `ExtensionAttribute` keeps the presence defer
  (name-keying its container's members is later work with (a)–(c)); any
  deferred resolution defers as today. `resolve_project`'s accumulator
  carries the same refinement cross-file. Oracle: the pivot branch's fixture
  matrix (project/assembly shadow sources, multi-segment, nameless) adapted
  from `ex3-attr-single-seg-attempt`, the `[<Literal>]`-only end-to-end
  overload-commit case, and the stage-3/4 differentials still green.

Coverage note: attributes resolving to *project-defined* types (custom
project attributes) still defer after stage 5 — bare cross-file project-type
resolution does not exist, and building it (an `alias → target` export for
project abbreviations) is a separate, later decision the stage-4 corpus
numbers should inform.

### Verification (non-negotiable, per stage)

Every stage must keep green, and each is expected to *move* the OV-9 number:

- [`crates/sema/tests/all/overload_corpus_diff.rs`](../crates/sema/tests/all/overload_corpus_diff.rs)
  — the OV-9 landmine detector (both approximation directions), plus its commit
  floors, which ratchet **up** as coverage lands;
- [`crates/sema/tests/all/extension_visibility_matrix.rs`](../crates/sema/tests/all/extension_visibility_matrix.rs)
  — the extension-visibility matrix;
- a **new** differential per stage in the OV-9 mould: a corpus where the fixture
  assembly *does* declare extension members (F#-native instance + static, C#-style
  `[<Extension>]`) and the F# calls both colliding and non-colliding names — the
  property being the same one: **our commit agrees with FCS's chosen overload, or
  we deferred**. A name-keyed gate that misses a name shows up there as a
  *wrong-overload commit*, which is precisely the failure this whole document is
  designed around.

# The export declaration model: one typed currency for the cross-file boundary

> **Status:** design + implementation plan. Not started. Successor to the
> per-feature export accretion in `crates/sema/src/resolve/model.rs`; the
> enabling infrastructure for `docs/parameterized-active-pattern-args-plan.md`
> Stage 3 (cross-file active-pattern shape) and for the
> constructor-namespace index memory
> (`cross-file-constructor-namespace`: "the proper next piece is a separate
> constructor-namespace index that resolves them, not a 6th guard").
>
> Grounded in a full producer/consumer survey of the boundary (2026-07-17);
> the file:line references below are from that survey.

Implement this plan with each stage on its own branch, reviewable in
isolation. Stages 1 and 2 are **zero-behaviour-change** refactors gated on
the full suite and the ignored corpus differentials; Stage 3 is the first
behaviour change (new sound commits/declines) and carries its own FCS
oracle.

## The problem

The inter-file boundary (`ResolvedFile` → `ProjectItems`, folded in Compile
order by `resolve_project`) carries **twelve parallel per-file fields** and
**~ten side indices**, each added by one feature's review cycle:

- `ResolvedFile`: `exports` (the value-namespace items, `is_case: bool` the
  only semantic payload), `nested_module_paths` (a *conflated shadow set* —
  types, exceptions, abbreviations, `extern`s, real modules),
  `real_nested_module_paths`, `type_qualified_case_exports`,
  `type_path_exports`, `module_paths`, `namespace_paths`,
  `modules_with_hidden_values`, `exportable_auto_open_module_paths`,
  `own_declares_auto_open`, plus the non-declaration file facts
  (`preceding_declares_extension_source`, `open_extension_namespaces`,
  `open_extension_unknowable`).
- `ProjectItems`: `value_exports`, `module_headers`, `nested_module_paths`,
  `real_nested_modules`, `namespace_paths`, `modules_with_hidden_values`,
  `auto_open_module_paths`, `case_item_ids`, `type_qualified_cases`,
  `type_paths`, `count`/`item_file_bases`.

The boundary currency is **environment-shaped projections** of the file's
declarations, when the thing being communicated is **entity-shaped**: a set
of typed declarations with identity, access, and provenance. Every feature
whose use-site meaning depends on a declaration's *nature* (case kind,
active-pattern shape, RQA, `[<Literal>]`, …) must today widen the boundary
with another field — and, worse, the resolver's cross-file conservatisms
exist precisely because absence-of-information is indistinguishable from
absence-of-declaration. A correctness-over-availability resolver needs
**proof of absence** to commit (memory r16: "indices are complete → false =
proof of absence"); a lossy export makes whole categories of sound commits
unreachable.

Three in-repo precedents say the fix is a complete typed model, not another
field:

1. **`container_decls`** (same-file, memory point 11b): guard-accretion
   failed for four review rounds; the complete per-container `DeclKinds`
   view converged it, and "the infra never needed rework".
2. **`value_exports`** (model.rs:59-76): the per-path export *history*
   replaced four older latest-wins/stopgap structures in one move — the
   same shape of consolidation, one level down.
3. **`borzoi-assembly`**: the assembly boundary already exports the full
   entity model; nobody has ever needed to add an `is_case` to it.

## Design

### The record

One source-ordered list per file, one typed record per declaration:

```rust
// crates/sema/src/resolve/model.rs (or a sibling export_decl.rs submodule)

/// One declaration a file contributes to the cross-file boundary, in source
/// order. The single currency `ProjectItems::extend_with` folds; every
/// cross-file index derives from these.
pub(super) struct ExportDecl {
    /// Qualified path: container segments + the declaration's own name.
    pub path: Vec<String>,
    /// Source position of the declaring occurrence (start). Provenance for
    /// positional contests and the future same-file convergence (DeclKinds'
    /// `module_pos` generalised).
    pub pos: TextSize,
    /// Whether the declaration sits under an anonymous top-level module
    /// (`Resolver::anonymous_root`). Such declarations are NOT
    /// cross-file-addressable (today they are simply not exported), but some
    /// facts about them still cross the boundary (a hidden-value marker for
    /// an anonymous-root union case — types.rs:116/234). Recording them
    /// with the flag keeps the derivations faithful AND stops losing the
    /// information.
    pub anonymous_root: bool,
    pub kind: ExportDeclKind,
}

pub(super) enum ExportDeclKind {
    /// A value-namespace item (today's `ExportedItem`): a `let` value, or a
    /// constructor case. `id` is allocated exactly as today — decl kinds
    /// with no value-namespace presence carry no `ItemId`, so id allocation
    /// is untouched by this refactor.
    Item {
        id: ItemId,
        def: DefId,
        access_root_len: Option<usize>,
        case: Option<CaseKind>, // None = ordinary value
        /// `Some` iff the item is module-qualified-addressable (today's
        /// `ExportedItem::qualified` — `None` for RQA/enum cases, which are
        /// reachable only through the type).
        qualified: bool,
    },
    /// A `type` definition (decls.rs:440): today's `type_path_exports`
    /// payload plus the shadow-set membership.
    Type { cases_enumerable: bool, slot: SlotClass },
    /// A real nested `module M = …` (decls.rs:1357/1369) or a top-level
    /// module header (resolve.rs:136). `auto_open` and `private` carried so
    /// `exportable_auto_open_module_paths` (non-private only) AND
    /// `own_declares_auto_open` (private included!) both derive.
    Module { header: bool, auto_open: bool, private: bool },
    /// A module abbreviation `module P = Target` (decls.rs:351/342).
    ModuleAbbrev,
    /// An `exception E` constructor's tycon-side presence (decls.rs:531) —
    /// the value-namespace ctor is a separate `Item` record.
    ExceptionTycon,
    /// An `extern` declaration (decls.rs:578/591).
    Extern,
    /// A `namespace` header ancestor prefix (resolve.rs:166).
    Namespace,
    /// A module-level active-pattern case (bindings.rs:52). Stage 3 attaches
    /// `ActivePatternShape`; Stages 1–2 record only the name (the
    /// hidden-value derivation needs it).
    ActivePatternCase, // Stage 3: ActivePatternCase { shape: ActivePatternShape }
}

pub(super) enum CaseKind {
    Union { require_qualified: bool },
    Enum,
    Exception,
    // Stage 3: ActivePattern { shape: ActivePatternShape }
}
```

Design rules:

- **A list, not a literal tree.** Containment derives from `path` prefixes;
  what everything downstream actually keys on is *source order and file
  provenance* (`value_exports` history order, the auto-open `Vec`'s
  Compile-order semantics, latest-wins insertion order), and a flat ordered
  list preserves those by construction. `extend_with` stamps the file index
  exactly as today.
- **`Option<ItemId>`-style id discipline** (here: ids only on `Item`).
  Stages 1–2 must not allocate or renumber any `ItemId`; that is what makes
  "derived indices are byte-identical" checkable.
- **File-level facts stay out.** `preceding_declares_extension_source`
  (fold-time accumulation, resolve.rs:255), `open_extension_namespaces` /
  `open_extension_unknowable` (facts about *opens*, decls.rs:666/923/927)
  are not declarations; they remain separate `ResolvedFile` fields and keep
  their public accessors (`infer.rs`'s `ExtensionScope::of` consumes them).
- **Public API is preserved.** All `ProjectItems` queries are `pub(super)`
  (survey §4: no external consumer). `ResolvedFile::exports()` (consumed by
  `crates/lsp`) keeps returning `ExportedItems`; it becomes a view derived
  from the `Item` decls (or `ExportedItems` stays the storage in Stage 1
  and is subsumed in Stage 2 — implementer's choice, but the end state has
  ONE stored list).

### The derivation table (what replaces what)

Every existing structure becomes a pure function of the decl list. The
derivations must reproduce today's behaviour **exactly**, including the
conservatisms and the ordering:

| legacy structure | derivation over decls |
|---|---|
| `exports` / `value_exports` / `case_item_ids` | `Item` decls in list order; `is_case` = `case.is_some()`; only `qualified` items enter `value_exports` (RQA/enum cases still reach `case_item_ids`) |
| `module_paths` → `module_headers` | `Module { header: true }`, non-anonymous-root |
| `real_nested_module_paths` → `real_nested_modules` | `Module { header: false }`, non-anonymous-root |
| `nested_module_paths` (conflated shadow set) | decls of kind `Module{header:false} ∪ Type ∪ ExceptionTycon ∪ ModuleAbbrev ∪ Extern`, non-anonymous-root (the five `record_project_name_shadow` triggers, decls.rs:351/382/531/578/1357) |
| `type_qualified_case_exports` → `type_qualified_cases` | `Item` decls with `case: Some(Union{..}\|Enum)` and their type-qualified path — carry the type path on the case decl (today threaded via `export_type_qualified_case`, decls.rs:211); latest-wins by list order |
| `type_path_exports` → `type_paths` | `Type` decls, non-anonymous-root; latest-wins by list order (note the `private → SlotClass::Keeps` forcing happens at the *producer*, decls.rs:435 — keep it there) |
| `namespace_paths` | `Namespace` decls |
| `modules_with_hidden_values` | containers having a decl of kind `ActivePatternCase ∪ ModuleAbbrev ∪ Extern ∪ Module{auto_open} ∪ (Item with case, anonymous_root)` — the six `note_hidden_value_module` triggers (bindings.rs:52, types.rs:116/234, decls.rs:342/409/591). **This is the poster child**: an ad-hoc conservatism set becomes a documented derivation |
| `exportable_auto_open_module_paths` → `auto_open_module_paths` | `Module { auto_open: true, private: false }`, non-anonymous-root, in list order (the Vec's Compile-order-determinism requirement, model.rs:148-155, is preserved by list order) |
| `own_declares_auto_open` | any `Module { auto_open: true }` **including private** (resolve.rs:409 derives from the unfiltered set — a known trap; the flag on the decl keeps both derivable) |

Pitfalls the implementer must treat as first-class (each is a place a naive
migration silently changes behaviour):

1. **Anonymous root**: every export writer is guarded `!anonymous_root`
   EXCEPT the hidden-value markers at types.rs:116/234, which fire *only*
   under it. Hence the `anonymous_root` flag on the record rather than a
   skip.
2. **Ordering**: `value_exports` history order, `auto_open_module_paths`
   order, and the latest-wins insertion orders of `type_qualified_cases` /
   `type_paths` must all match today's. The discipline that guarantees it:
   **one decl append per legacy push site, at the same program point** —
   the writer functions (`record_project_name_shadow`,
   `export_type_qualified_case`, `export_type_path`,
   `note_hidden_value_module`, `record_auto_open_module`, the header sites
   in resolve.rs:136/166, and the three `ExportedItem` producers at
   bindings.rs:146 / decls.rs:181 / decls.rs:254) are the append points.
3. **`private` auto-open**: filtered from the exportable list
   (resolve.rs:410-415) but counted by `own_declares_auto_open` — see the
   table.
4. **Dotted-module ancestors**: `namespace_paths` records ancestor
   *prefixes* with the `ns_upto` bound (resolve.rs:160-164); reproduce, do
   not "simplify".

### What this does NOT do

- **No precedence change.** The straddle fold, latest-wins slots, open
  generations, and every conservatism keep their exact semantics; only the
  *source* of the indices changes. Enabling *new* commits (e.g. hidden-value
  sets shrinking because AP cases are now enumerable) is deliberately
  deferred to Stage 3+ — a migration stage that "incidentally" improves
  behaviour is a migration stage that can't be verified.
- **No assembly-boundary change.** `borzoi-assembly` already exports the
  full entity model; its gaps (AP name demangle, pickle) are separate.
- **No same-file `container_decls` change** until the optional Stage 4.

## Implementation plan

### Stage 1: kind-typed item exports

**Dependencies**: none. **Behaviour change**: none.

Introduce `CaseKind` and replace `ExportedItem::is_case: bool` with
`case: Option<CaseKind>` (an `is_case()` method preserves every consumer
textually). Thread the kind from the five producer call sites — the writers
already know it: `module_let` values (bindings.rs:146 → `None`),
`export_case` callers (non-RQA union types.rs:108 → `Union { require_qualified:
false }`; exception types.rs:231 → `Exception`), `export_require_qualified_case`
callers (RQA union types.rs:103 → `Union { require_qualified: true }`; enum
types.rs:186 → `Enum`). `ExportRecord` and `extend_with` unchanged except
`is_case: item.is_case()`.

This is small and lands the semantic payload where the near-term features
need it, before the bigger structural move.

**Oracle**: full suite green; a direct test pinning the kind for each of the
five producer shapes (module-level `let`, union case, RQA union case, enum
case, exception ctor) via a test accessor.

---

### Stage 2: the declaration list replaces the structural fields

**Dependencies**: Stage 1. **Behaviour change**: none.

Add `ExportDecl` / `ExportDeclKind`; give the `Resolver` one source-ordered
`export_decls: Vec<ExportDecl>`; convert each legacy writer function into a
decl append (same program point — pitfall 2); rewrite
`ProjectItems::extend_with` to derive every index from the decl list per
the derivation table; delete the nine legacy `ResolvedFile` structural
fields and their `Resolver` twins. `ResolvedFile` carries `export_decls` +
the file-level (non-declaration) facts + `exports` (which either stays as
storage with `Item` decls referencing it, or is folded into the decls with
`exports()` derived — pick whichever keeps the diff honest; end state must
have one stored list).

**Authoring scaffold** (in-PR, removed before merge, or as a first commit
the final commit deletes): dual-write both paths and
`debug_assert_eq!`-compare every derived index against the legacy-built one
inside `extend_with` — the full suite plus the corpus gates then check
equivalence on every real F# project the harness touches. Run the ignored
whole-project gates (`resolve_corpus_diff`, `classify_diff`, the
`resolve_project_diff` groups) at the scaffold commit AND at the final
commit.

**Oracle**: full suite + all differential gates green, byte-identical
behaviour (the scaffold proves the derivations; the gates prove the
end-to-end). Grep-proof that the nine legacy fields and their writers are
gone.

---

### Stage 3: kinds and shapes cross the boundary (first behaviour change)

**Dependencies**: Stage 2 (and the AP plan's Stages 1–2, already merged).

Restructured (2026-07-17) into three sub-stages, each its own branch/PR. The
design insight that forces the split: **a blanket decline of an unknown-shape
active pattern's arguments would *regress* the common case**, so the decline
(3c) must come *last*, after project (3a) and assembly (3b) shapes have shrunk
the unknown-shape residue to nearly nothing. An arity-0 total AP like
`KeyValue (k, v)` fabricates a binder for its argument *today*, and that binder
is **correct** (the argument is the result sub-pattern) — declining it would
replace a right commit with a defer. Decline is only right where *wrongness* is
possible (a parameter that FCS resolves to an outer value), which is exactly the
case 3a+3b make shape-certain. So the order is 3a → 3b → 3c, not "decline first,
refine later".

#### Stage 3a: project-side cross-file active-pattern cases, pattern-only, with shape

**Dependencies**: Stage 2 (this PR), the AP plan's Stages 1–2 (merged).

FCS-probed before coding (two files A defines / B uses, `fcs-dump uses-project`,
every fixture diagnostics-clean — see the branch's probe write-up). The verdicts
that pin the design:

- `open A; match x with Even` / `DivBy divisor` — FCS **resolves the head
  cross-file to the recognizer span** (the `|Even|Odd|` name range, parens
  excluded — identical to `ActivePatName::name_range` and to the same-file
  `use_def` range), full name `A.(|Even|Odd|).Even`. So go-to-definition points
  at the recognizer, and the parameterized partial's `divisor` (k = p = 1)
  resolves to the **outer value**, no fabricated binder.
- Bare `Even` in *expression* position after `open A` → **FS0039**: AP cases are
  pattern-namespace-only. Value-namespace queries must never see them.
- `A.Even` (module-qualified pattern) is *legal* and resolves to the recognizer,
  **but** it rides the type/module-qualified-case path AP cases do not populate;
  3a **declines** it (a sound coverage gap), noted as a possible follow-up.
- In pattern position the AP case **wins over a same-named value** (a local
  `let Even`, or a module value `A.Even` exported alongside the recognizer):
  constructor namespace, values do not shadow — matches `case_reference`.
- Two opened modules both exporting `Even` → **latest-open-wins** (the later
  `open`), handled by source-ordered frame entries; no generation bump needed
  once the module is no longer hidden.
- A module whose *only* hidden-value trigger is its AP cases: with the cases now
  enumerable, **nothing else about its fold is unenumerable** (its `let`s and its
  union cases are already indexed). So the AP hidden-trigger can be narrowed, and
  as a bonus its union/exception cases — today over-suppressed because the AP
  made the whole module hidden — become trustworthy too.

Design (as implemented — a **history-backed** model reached after a codex review
of an earlier separate-index draft, whose three defects — no straddle provenance,
no accessibility recovery, split same-file/cross-file identity — all traced to
*not* reusing the constructor-namespace machinery):

1. `ExportDeclKind::ActivePatternCase` gains `{ item: Option<usize>, shape:
   ActivePatternShape }`. `shape` is `define_active_pattern`'s stored shape
   (module-level recognizers only). `item` indexes `exports.items` for the AP
   case's own `ExportedItem` — `None` under an anonymous root (no cross-file
   handle; keeps today's hidden-marker behaviour there).
2. Each module-level AP case gets an `ExportedItem` with **`qualified: None`**
   (so the *same-file* `self.items` value queries — `qualified_value_in`, the
   same-file open value pass, the straddle's current-file branch — never see it,
   since they filter on `qualified`) and `case: None` (`CaseKind::ActivePattern`
   is *not* introduced; the AP-ness rides the `DefKind::ActivePattern` def and
   `case_item_ids`). Its `def` is the per-case `use_def` (ranged at the recognizer
   span), so a `Resolution::Item` points go-to-def at the recognizer, matching
   FCS. **One identity, same-file and cross-file**: the same-file case *use* now
   resolves to that `Resolution::Item` (the union-case precedent), so
   find-references / rename span both. The case's scope entry is marked
   **`pattern_only`** so `latest_entry` (expression lookup) skips it while
   `case_reference` (pattern position) still finds it — an AP case is FS0039 in
   expression position.
3. **AP cases ride `value_exports`** as **pattern-only** `ExportRecord`s
   (`is_case = true`, `pattern_only = true`), keyed by the value-namespace path
   (`["A", "Even"]`). This is the crux: they inherit the constructor namespace's
   Compile-order **provenance** (the per-path history's newest-file-wins) and its
   **accessibility recovery** (a public case under a later inaccessible `private`
   is still selectable) for free, exactly as a union case does. A side map
   `ProjectItems::active_pattern_shapes: HashMap<ItemId, ActivePatternShape>`
   carries the recognizer shape; `case_item_ids` gets the id too. The
   value-namespace queries (`latest_accessible_value`, `fragment_value_children`,
   `is_project_value_prefixed`, `ordinary_value_at`) filter `!pattern_only`; the
   constructor queries (`latest_accessible_case`, `direct`/`fragment_constructor_children`)
   keep `is_case`, so they include AP naturally.
4. **Narrow the AP hidden trigger — and only it.** The `ActivePatternCase`
   derivation stops pushing `modules_with_hidden_values` (the *cross-file*
   index; the same-file `note_hidden_value_module` at `module_let` is left, a
   sound same-file-`open` gap). A module hidden for another reason (alias,
   `extern`, anon-root case, `[<AutoOpen>]`) stays hidden.
5. **No dedicated AP fold pass** — AP cases flow through the *existing*
   `direct_constructor_children` (they are `is_case`), pushed as
   `opened_pattern_only` entries and suppressed exactly like a union case when the
   module is hidden. In the **namespace straddle**
   (`open_project_namespace_values`), AP cases now enter `submodule_contributions_at`'s
   `case` dimension (via `fragment_constructor_children`) but **not** `value_slot`
   — an AP is a case that is *not* a value, so the case-name loop's redundant
   `value_slot` feed is dropped (value-live union cases already feed it through
   `value_names`). The value and constructor namespaces are then decided
   **independently**: the old "wins the value slot ⇒ wins both" shortcut was
   false for a case-without-value, so a direct case that out-files the submodule's
   value but not its (later or tied) AP case wins the value namespace by its
   natural push while the submodule's active pattern wins the constructor slot.
6. **Use-site split**: generalise the same-file shape lookup — a helper
   `resolution_active_pattern_shape(res)` returns the shape for a
   `Resolution::Local` (anonymous-root/local, keyed in `active_pattern_shape`) or
   a `Resolution::Item` — same-file mapped through `self.items[..].def` to the
   shape, cross-file via `ProjectItems::active_pattern_shape_of`. The existing
   `split_active_pattern_args` then runs unchanged. Unknown shape keeps today's
   behaviour (no declines — that is 3c). The public `ResolvedFile::active_pattern_shape`
   accessor maps a same-file `Item` the same way.
7. **LSP consumers of the new identity**: `file_export_symbols` filters
   `DefKind::ActivePattern` exports out of the document/workspace-symbol outline
   (the per-case identity handles would otherwise list `(|Even|Odd|)` as two
   duplicate `FUNCTION` symbols at the recognizer span; the recognizer's own
   outline symbol is a separate concern). `textDocument/references` adds the
   declaration anchor explicitly when the client asks — the AP case's declaration
   span self-resolves to the recognizer, not the case `Item`, so `matching_in_file`
   would otherwise omit it.

**Behaviour changes, both FCS-differential-gated**: (a) cross-file AP cases
resolve in pattern position and split their args by shape; (b) union/exception
cases of an AP-declaring module stop being over-suppressed.

**Post-review fix — literal constant patterns contest the cases.** A review of
the shipped 3a found (and probes confirmed, all build-clean) that a
`[<Literal>]` value *is* a constant pattern: FCS's `ePatItems` holds exactly
the constructor cases and the literal values, latest-wins, so
`open A; [<Literal>] let Even = 7; match n with Even` binds the literal where
sema committed the opened case — an AP case, but equally a **union/exception
case** (the hole predates 3a). Two rules pin the model: the slot is
position-ordered (a literal *before* the `open` loses to it), and within ONE
opened module the literal wins **regardless of source order** (FCS folds a
module as exceptions → tycons → vals). The fix: attribute-**presence** on a
module-level `let` marks it maybe-literal (identity is unverifiable — a
`LiteralAttribute` alias shadow is undetectable; an *unattributed* value
provably cannot be a literal and still never contests), carried on scope
entries (`maybe_constant_pattern`) and into `ProjectItems`
(`ExportedItem::attributed`); the bare pattern scan (`case_reference`) defers
on meeting one before the case, and the open fold suppresses a case whose own
module exports an accessible maybe-literal (`pattern_suppressed_case_ids`, the
vals-after-tycons rule). Assembly-side the CLI `Literal` flag / Q17 decimal
rule gives the *exact* bit (`OpenFoldName::constant_pattern`). Exemptions,
both FCS-pinned: an **applied** head is never a literal on a clean program
(FS3191), so the applied split keeps committing; a **qualified** case pattern
(`A.Green`) resolves to the case, ignoring the literal (sema currently
declines that shape either way — committing it is a follow-up).

**Oracle**: FCS-free direct tests (head → recognizer decl; no expression-position
resolution; `DivBy divisor` → outer value, no binder; `Scale g` still binds `g`;
value-namespace queries provably exclude AP cases) + multi-file
`resolve_project_diff`-style fixtures for the whole probe matrix, every fixture
`uses-project`-diagnostics-clean; certain-implies-exact; the ignored
`resolve_corpus_diff` gate stays green.

#### Stage 3b: assembly-side active-pattern shape (as shipped)

**Dependencies**: 3a. Derive the assembly AP shape from the mangled `|A|B|` val
name — cases + totality + single-case — and attach it to the **fold's**
`opened_case` entries (an *explicit* `open <module>` / `open <namespace>`), so a
**total single-case** assembly recognizer (`(|Scale|)`) splits an applied use
frontAndBack exactly as a project one now does. Recognizers reached through the
*implicit* `[<assembly: AutoOpen>]` auto-open — including FSharp.Core's
`(|KeyValue|)` / `(|Failure|_|)` — are out of scope here (see "Scope" below).

**The `arity = params − 1` premise was wrong, and is dropped.** The plan hoped
the signature's curried arity would give the parameter count. It does not: F#
compiles a recognizer's *tupled* argument groups to **flattened** IL parameters,
so the metadata parameter count is an **upper bound** on FCS's type-derived
`paramCount` (`stripFunTy` on the recognizer's F# type — the number
`TcPatLongIdentActivePatternCase` actually splits on), not equal to it. And an
F# assembly's methods carry no `arg_group_count` (its `None` is exactly "cannot
tell curried from tupled from flattened IL"), so the divergence is
*undetectable* from metadata. An **over-estimated** arity is a wrong commit: a
use at `k = paramCount + 1` would treat the genuine result binder as a
parameter, resolving a name FCS binds to an outer value instead — a
certain-implies-exact violation. So **arity is `None` for every assembly
recognizer.** (The one metadata-certain case — `params == 1` ⇒ `arity == 0`,
since a single un-tuple-able parameter must be the matched value — makes no
observable difference anyway: a partial single-case AP's only legal applied use
is `k = 1`, which binds the result under both `arity == None` and `arity ==
0`.)

That leaves a clean, name-only derivation (verified by reading the built DLL
through the assembly reader, and FCS-probing the use side against it — the probe
matrix below). It follows FCS's own `ActivePatternInfoOfValName`
(`PrettyNaming.fs`): the IL method name **is** the mangled logical name, so
`total`/`single_case`/the case list are exactly what FCS computes.

Derivation (`active_pattern_banana` in `assembly_env.rs`, replacing
`active_pattern_tags`), for a well-formed `|…|` banana only (a malformed name
attaches **no** shape — residue, today's behaviour):

| IL metadata name | cases | `total` | `single_case` | `arity` |
|---|---|---|---|---|
| `\|Even\|Odd\|` | `[Even, Odd]` | `true` (no trailing `\|_\|`) | `false` | `None` |
| `\|Scale\|` | `[Scale]` | `true` | `true` | `None` |
| `\|DivBy\|_\|` | `[DivBy]` | `false` (trailing `\|_\|`) | `true` | `None` |
| `\|_\|` / `\|\|` / `\|A\|\|B\|` | — | — | — | (malformed → no shape) |

`total` = the **last** `\|`-segment is not `_` (FCS checks only the last
segment); `single_case` = exactly one remaining case; every case non-empty and
not `_`.

**Metadata facts pinned by the DLL dump** (params = flattened IL parameter
count):

| F# recognizer | IL name | params | note |
|---|---|---|---|
| `(\|Even\|Odd\|) n` | `\|Even\|Odd\|` | 1 | multi-case (arity irrelevant) |
| `(\|Scale\|) k n` | `\|Scale\|` | 2 | `params−1 = 1` = paramCount, but arity dropped |
| `(\|DivBy\|_\|) d n` | `\|DivBy\|_\|` | 2 | `params−1 = 1` = paramCount |
| `(\|Nonempty\|_\|) s` | `\|Nonempty\|_\|` | 1 | s IS the matched value; paramCount 0 |
| `(\|InRange\|_\|) (lo,hi) n` | `\|InRange\|_\|` | **3** | **paramCount 1** — `params−1 = 2` OVER-counts (tupling) |
| `(\|Positive\|_\|) = fun n->` | `\|Positive\|_\|` | 1 | point-free compiled as a 1-param **Method**, not a property |
| `(\|P3\|) a b n` | `\|P3\|` | 3 | `params−1 = 2` = paramCount |

`InRange` is the counter-example: `params−1 = 2 ≠` FCS's paramCount `1`.

**FCS use-side verdicts** (fsi- and `dotnet build`-verified against the built
fixture DLL — every consumer compiles clean):

- `open …Recognizers; match n with Scale factor v` → `factor` = the **outer
  value** (a parameter), `v` = the recognizer result (**binds**). frontAndBack
  holds cross-assembly, arity-free. *This is the Stage-3b behaviour change.*
- `Scale g` (k = 1) → `g` **binds** the partially-applied recognizer (`g 5` runs
  the recognizer, not an outer `g`).
- `DivBy divisor` (partial, paramCount 1, k = 1) → `divisor` = the outer value in
  FCS; with `arity == None` sema keeps today's fabricate-a-binder (a status-quo
  unsoundness, **not** a regression — the 3c residue).
- `InRange (1, 10) x` → `x` binds; today's behaviour already correct (the `(1,10)`
  const-tuple binds nothing), and `arity == None` preserves it.
- FSharp.Core `KeyValue (k, v)` / `Failure msg` are reached through the *implicit*
  auto-open, which this stage leaves untouched — they keep declining in pattern
  position (sound; the implicit-path follow-up in "Scope" below). An **explicit**
  `open Microsoft.FSharp.Core.Operators` folds them through the fold path, where
  the shape applies.

Wiring: `OpenFoldName` and `ScopeEntry` gain an `Option<ActivePatternShape>`
(set only on an AP-tag entry); the applied-head split site reads it via
`applied_active_pattern_case` (a `case_reference` companion), falling back to
`resolution_active_pattern_shape` for same-file/cross-file `Item`/`Local` heads.
The demangle stays in sema (`active_pattern_banana`, replacing
`active_pattern_tags`); no `borzoi-assembly` change, and no signature reading at
all (arity is `None`).

**Scope — the fold path only.** The shape is attached only for recognizers
folded in through the fold (`fold_container_into`, i.e. explicit `open <module>`
/ `open <namespace>`). That path already computes the pattern-namespace winner
through its demotions, and the shape must ride *every* one of them
(certain-implies-exact — a shape trusted where the tag is not the definite case
is a wrong split):

- **demoted / collided** fold entry (residue, cross-surface collision) → the
  writer drops the shape (`open_assembly_module_fold`, the `demoted` gate);
- **not an authoritative F# module** (`fsc --standalone` / undecoded pickle, where
  `EntityKind::Module` is only an IL heuristic and a banana `let` is really a
  method group) → no demangle (`fsharp_signature_unreliable`);
- **shadowed by a same-named value** (a `[<Literal>]` / constant is a *constant
  pattern* FCS's latest-wins puts in charge of the name, which `case_reference`
  skips as an ordinary value) → the **use-site split declines** whenever `lookup`
  finds any same-named value in scope — this open, a later open, a local `let`, or
  an auto-open child. Checked at the split site, not the fold, because the shadow
  can come from anywhere in the final scope; and only for assembly recognizers,
  since same-file / cross-file project ones (3a) resolve through the constructor
  namespace, which already models it. The applied form is FCS-illegal when the
  constant pattern actually wins, so declining is sound.

A well-formed **zero-tag** recognizer (the quoted `` `|_|` ``, a partial pattern
with no case names) demangles to an empty tag list — it contributes no case
entry but is *not* residue, so it never poisons the open surface.

The **implicit `[<assembly: AutoOpen>]` path** (`open_type_statics` — the one
FSharp.Core's `(|KeyValue|)` / `(|Failure|_|)` actually take) is **deliberately
left at today's behaviour**: it does not carry the fold's demotions, so trusting
a shape there could be a wrong commit. Its recognizers keep declining in pattern
position (sound — a coverage gap, not a regression). Routing that path through
the fold's demotions — so the named FSharp.Core examples split too — is a
follow-up, because doing it soundly means giving the implicit path the full
residue / collision / constant-pattern-shadow machinery the fold has, which is a
larger structural change than this stage. (This scoping was reached after a codex
review sequence: an earlier draft wired the implicit path directly and each
review surfaced another demotion it was missing — guard accretion that the
retreat to the fold-only core resolves.)

#### Stage 3c: barrier-decline for still-unknown-shape AP-certain heads

**Dependencies**: 3a, 3b. **Last, and only if the residue justifies it.** For an
applied head that is *certainly* an active pattern but whose shape is still
unknown (a residue 3a+3b should shrink to nearly nothing), decline its name
arguments rather than fabricate binders. Two subtleties this stage must honour:

- **Why last** (recorded above): declining an unknown-shape AP's args
  unconditionally regresses arity-0 total APs (`KeyValue (k, v)`), whose
  fabricated binders are correct. Decline only where wrongness is possible.
- **Body-use barrier**: declining a maybe-result binder must push a shadow
  **barrier** for that name in the arm scope (the `ap_case_barrier` precedent — a
  `Deferred` scope entry), because merely *skipping* the binder would let an
  arm-body use of the name wrongly commit an outer same-named value where FCS
  binds the pattern local.

**Oracle**: a cross-file/assembly parameterized AP used as `Foo bar` no longer
fabricates a binder for `bar`; a cross-file *union* case `Some x` still binds
`x`; certain-implies-exact over a multi-file fixture exercising both.

---

### Stage 4 (optional, separate decision): convergence

Directions, not commitments — evaluate after Stage 3:

- **Same-file convergence**: back `container_decls`' `DeclKinds` by the
  same records (a per-container view over in-progress decls), collapsing
  the same-file/cross-file duality. `module_pos` generalises to the decl's
  `pos`.
- **Hidden-value tightening**: with AP cases (and `extern`s) enumerable,
  some `modules_with_hidden_values` conservatism can be *narrowed* —
  each narrowing is its own FCS-differential-gated slice.
- **Assembly parity**: mark assembly AP tags with demangled shape where the
  arity metadata allows, so assembly heads graduate from decline to split.

## References

- Boundary model + fold: `crates/sema/src/resolve/model.rs` (`ProjectItems`
  :59-201, `extend_with` :505-560, `ExportedItem` :764-826).
- Producer writer functions: `record_project_name_shadow` (decls.rs:1574),
  `export_type_qualified_case` (decls.rs:211), `export_type_path`
  (decls.rs:229), `note_hidden_value_module` (lookup.rs:800),
  `record_auto_open_module` (resolve.rs:401), headers resolve.rs:136/166,
  item producers bindings.rs:146 / decls.rs:181 / decls.rs:254.
- `Resolver::finish`: resolve.rs:416-437; fold: resolve.rs:238-261.
- Same-file complete view: `DeclKinds` / `container_decls`
  (state.rs:293-330, :500), memory point 11b.
- Consumers of the extension/open file facts: `infer.rs:675-705`
  (`ExtensionScope::of`).
- The AP consumer this enables: `docs/parameterized-active-pattern-args-plan.md`
  Stage 3.
- Memory: `sema-resolver-name-resolution-correctness` (points 11b, 14, r16
  "proof of absence"), `cross-file-constructor-namespace`.

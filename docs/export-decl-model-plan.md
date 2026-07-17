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

1. **Case kind at use sites**: `is_case_item` / `case_classification` gain
   kind access (`case_kind_of(id) -> Option<CaseKind>`), replacing
   boolean-only classification. This is the prerequisite the
   `cross-file-constructor-namespace` memory names; the constructor-namespace
   index itself remains a separate follow-up.
2. **Active-pattern shape crosses the boundary**: `ActivePatternCase` decls
   gain `shape: ActivePatternShape`; `open M` folding a module with AP cases
   can now push *named, shape-carrying, pattern-only* entries (like the
   assembly fold's opaque `opened_case` entries, but with shape) instead of
   relying solely on the blunt `modules_with_hidden_values` opacity. Scope
   control: it is acceptable (and simplest) to keep the value-namespace
   opacity exactly as-is and add ONLY constructor-namespace entries.
3. **Implement the AP plan's Stage 3** on top: an applied head resolving to
   a cross-file/opened AP case with known shape splits its args (same logic
   as same-file); unknown shape (assembly opaque tags — mark them at the
   fold, assembly_env.rs:1748-1763) **declines** the name args instead of
   fabricating binders; cross-file *union* cases (now kind-certain) keep
   binding their args.

**Oracle**: multi-file FCS differentials (`resolve_project_diff`-style
fixtures): parameterized AP used cross-file via `open` and via qualified
path — args resolve/decline per shape, never fabricate; union case `Some x`
still binds; `dotnet build`-verify every fixture (fcs-dump tolerates type
errors). The whole-project corpus gates stay green. Each new commit path
must satisfy certain-implies-exact under `classify_diff`.

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

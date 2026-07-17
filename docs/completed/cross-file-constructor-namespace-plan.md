# Cross-file constructor namespace (case-resolution unification)

Status: **done — landed as #593** (commit `abbb0cf`, 2026-06-27), a follow-up to
#574 (cross-file case kind). Resolves the value-shadowed cross-file case in pattern
position and **retired** the `value_shadowing_case_ids` guard (it no longer exists
anywhere in `crates/sema/src/`); the active-pattern and opaque-open defers stay
(genuinely unmodelled). The design below shipped essentially as written — the
constructor index (`ProjectItems::constructors` + `direct_constructor_children`),
the `ScopeEntry::pattern_only` entry, the two independent projections in
`open_module_values`, and the suppression of `pattern_only` ids under a hidden
module are all in `crates/sema/src/resolve.rs`. The TDD flip-tests below also landed
in `crates/sema/tests/resolve_cross_file_cases.rs`
(`value_shadowed_case_resolves_expression_to_value_and_pattern_to_case`,
`value_shadowed_later_open_case_wins_over_an_earlier_open_in_pattern`,
`repeatedly_value_shadowed_case_resolves_in_pattern`, plus the High-1
`hidden_module_value_shadowed_case_defers_in_pattern` and High-2
`cross_file_module_augmentation_splits_value_and_constructor`).

## Why

F# keeps the **constructor namespace** (union cases, exception constructors,
active patterns — what a *pattern* head resolves against) separate from the
**value namespace** (`let`s — what an *expression* resolves against). A union /
exception case lives in *both*: `let x = Red` and `match … with Red` both work.

#574 modelled cross-file cases by exporting them through the **value** index
(`ProjectItems::by_qualified_path`, keyed by path, **latest-wins**,
namespace-blind). So a case and a same-named `let` at one path
(`type T = Red` then `let Red = 0`) collapse to the value, and the constructor is
lost cross-file. #574 took **five `codex review` rounds**, each a different
symptom of that conflation, each patched with a sound *defer* guard
(`case_item_ids`, `pattern_suppressed_case_ids`, the `opaque_value_open` mirror,
`value_shadowing_case_ids` + its transitive propagation). The accumulating guards
are the signal: the fix is to stop routing constructors through the value index.

## FCS-probed semantics (the oracle — re-probe with `fcs-dump uses-project`)

| Project (`open` then use)                                              | Expression `Red` | Pattern `match … with Red` |
|-----------------------------------------------------------------------|------------------|----------------------------|
| `module M` = `type T = Red`; later file `open M`                      | `M.T.Red` (case) | `M.T.Red` (case)           |
| `module M` = `type T = Red`; `let Red = 0`; later `open M`            | `M.Red` (value)  | **`M.T.Red` (case)**       |
| `open A` (case `Red`); `open M` (value-shadowed case); use            | `M.Red` (value)  | **`M.Cm.Red`** (M's case)  |
| `open M` where M has case `Red` **and** active pattern `(\|Red\|_\|)` | `M.T.Red` (case) | `M.(\|Red\|_\|).Red` (AP)  |
| `[<RequireQualifiedAccess>]` case, bare `Red`                         | FS0039           | not a constructor          |

So in **pattern** position a `let` does **not** shadow a constructor (rows 2–3),
the **latest open's** constructor wins (row 3), and an **active pattern** shadows
a union case (row 4 — but we do not model active patterns cross-file, so that one
stays a *defer*).

## Design

Route cross-file **constructors** through their own index and their own
scope-frame entries, so they survive a same-named value and `lookup`
(expression) / `case_reference` (pattern) each see only their namespace.

**Core invariant** (the rule the whole design turns on): value entries and
constructor entries are *two independent namespace projections over the same
source order* — **not one projection filtered through the other**. A same-file
`let` shadowing a cross-file case must block it in the *value* projection but not
the *constructor* projection, and vice versa.

1. **`ProjectItems::constructors: HashMap<Vec<String>, ItemId>`** — the
   constructor at each value-namespace path (`[M, Red]` → the case `ItemId`),
   populated in `extend_with` for `is_case` items. **Never overwritten by a
   value** (unlike `by_qualified_path`). `case_item_ids` stays (it answers
   id→is-case for `case_classification`); `value_shadowing_case_ids` is removed.
   Add `direct_constructor_children(module_path)` mirroring
   `direct_value_children`.

2. **`ScopeEntry` gains `pattern_only: bool`** — generalising the existing
   same-file active-pattern skip in [`lookup`] (`continue; // pattern-only`). A
   `pattern_only` entry is in the *constructor* namespace only: [`lookup`]
   (expression) skips it; [`case_reference`] (pattern) includes it (it is already
   a case `Item`, so `case_classification` returns `Some(true)`).

3. **`open_module_values`** runs the **two projections with independent dedup**.
   Today one `seen: HashSet<String>` over names is shared; split it:
   - *Value projection* — the same-file pass records **all** same-file `[M, …]`
     child names in `seen_values` and pushes them; the cross-file
     `direct_value_children` pass dedups against `seen_values` (a same-file name —
     value **or** case — shadows the cross-file value). Unchanged from today.
   - *Constructor projection* — the same-file pass records same-file **case**
     child names (`is_case`) in `seen_ctors`; the cross-file
     `direct_constructor_children` pass dedups against `seen_ctors` **only**, and
     pushes each cross-file case as a **`pattern_only`** entry **unless** that case
     is already the `by_qualified_path` value entry at its path (an unshadowed case
     is one normal entry serving both namespaces — do not double-push). Crucially
     a same-file `let` in `seen_values` does **not** block a cross-file case here
     (High-2: `Ns.M` augmented with a file-0 case and a file-1 `let` — pattern
     resolves to the file-0 case while the expression takes the file-1 value).

4. **Retire `value_shadowing_case_ids`** and its `case_reference` defer: the
   `pattern_only` constructor entry now lets `case_reference` *resolve* the
   value-shadowed case (rows 2–3), so there is nothing to defer.

5. **Keep** `pattern_suppressed_case_ids` (a *hidden* module's cases — it also
   brings an unenumerable **active pattern** that we do not model and that could
   shadow them, row 4) and the `opaque_value_open` / generation-staleness skip in
   `case_reference` (a later opaque/unmodelled open could shadow with a
   constructor). These stay *defers* — the unmodelled-constructor cases.

   **The new `pattern_only` entries are subject to the *same* suppression.** Today
   suppression is attached while enumerating *value* entries (`hidden &&
   is_case_item(value_id)`), but a hidden module's case can be **value-shadowed**
   (`type T = Red`, `let Red = 0`, `let (|Red|_|) …`): the value entry is then the
   `let` (not a case, not suppressed), so the constructor pass would resolve the
   union case — wrong (High-1: FCS resolves the pattern to the active pattern). So
   when the opened module is hidden, add every `pattern_only` constructor id to
   `pattern_suppressed_case_ids` in the constructor pass.

### Worked example (row 3)

`open A` (case `Red`), `open M` (case `Red` shadowed by `let Red = 0`),
`match x with Red` / `let v = Red`:
- `open A`: `Red` not shadowed → one normal entry (A's case).
- `open M`: value entry `M.Red` (the `let`, normal) **+** `pattern_only` entry
  `M.Cm.Red` (the case, since `constructors[[M,Red]]` ≠ `by_qualified_path[[M,Red]]`).
- `lookup("Red")` (expression) skips the `pattern_only` entry → latest normal →
  `M.Red` (the value). ✓
- `case_reference("Red")` (pattern) → latest entry is the `pattern_only` case
  `M.Cm.Red`. ✓ (The earlier `A.Red` is correctly not reached.)

## Tests to write first (TDD)

Flip the #574 *boundary* tests in `resolve_cross_file_cases.rs` from defer to
resolution, FCS-pinned:
- `value_shadowed_case_resolves_expression_to_value_and_defers_the_pattern` →
  pattern now resolves to the **case** (expression still the value).
- `value_shadowed_case_does_not_leak_an_earlier_open_in_pattern` /
  `repeatedly_shadowed_case_still_defers_in_pattern` → pattern now resolves to the
  **latest open's case**.
- **Keep** (must still defer): `hidden_module_case_defers_in_cross_file_pattern`
  (active pattern), `opened_case_defers_in_pattern_under_an_opaque_open`.
- **New, High-1 cross-product** — a module with a union case `Red`, a same-named
  `let Red`, *and* an active pattern `(|Red|_|)`; `open M; match … with Red` must
  **defer** (FCS: the active pattern), not resolve the now-`pattern_only` union
  case. (Pins the suppression of the new entries.)
- **New, High-2 cross-file augmentation** — `namespace Ns; module M` split across
  files (file 0: `type T = Red | Blue`; file 1: `let Red = 0`) with file 1's
  `module N` doing `open M`: expression `Red` resolves to **file 1's value**
  (`Ns.M.Red`), pattern `Red` to **file 0's case** (`Ns.M.T.Red`). (Pins the
  independent constructor dedup — the file-1 `let` in `seen_values` must not block
  the file-0 case.)
- Regression: every `resolve_cross_file_cases` + `resolve_diff` /
  `resolve_project_diff` case still green; same-file pattern matching unaffected
  (it does not use the new index).

## Risk / scope

Touches `ScopeEntry` (widely used) and `open_module_values` (core) — the compiler
catches the `ScopeEntry` field addition; the suite is the safety net for the
enumeration. **Out of scope (separate, larger):** exporting **active patterns**
cross-file (would turn row 4's defer into resolution — needs the recognizer
modelled), the type-qualified `Lib.Color.Red` form (cross-file *type*
resolution), and bare `open <namespace>; Case` (a project-namespace path index).
Expect a multi-round `codex review`; the conservatism story
(`pattern_suppressed`, opaque, generation) is unchanged, so the new surface is
just the `pattern_only` enumeration.

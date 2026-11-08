---
name: assembly-surface-check
description: How to find out what `borzoi-assembly` *actually* projects from a referenced DLL, rather than assuming. Use when a type or member from a referenced assembly is mysteriously "not found" by the LSP (it may have been dropped, not absent — `EcmaView::enumerate_type_defs` discards the whole-type drop records that `enumerate_type_defs_with_skips` returns), or when picking a BCL member for a sema/differential test (the sema tests read the reference pack, but `fcs-dump` type-checks against the SDK runtime BCL, and the two disagree about which methods are single-candidate).
---

# What does our reader actually see in that DLL?

Two failure modes, one method: **enumerate the members through our own
`Ecma335Assembly` reader and look**, instead of trusting either the DLL's
documented surface or an `fcs-dump` probe.

## 1. "Type/member not found" may mean *dropped*, not absent

The projector isolates undecodable items: a single member or type it cannot
decode is dropped and recorded, rather than sinking the whole assembly. So a
missing entity is ambiguous between "not in the DLL" and "we refused it".

- **Member drops** are recorded on `Entity::skipped_members`.
- **Whole-type drops** (the type's own shape is undecodable) have no entity to
  hang a record on, so they land in `AssemblyProjectionSkips::dropped_types`,
  next to `skipped_fsharp_overlays`. Both come back from the `EcmaView` trait's
  required method, `enumerate_type_defs_with_skips() -> Result<(Vec<Entity>,
  AssemblyProjectionSkips), ImportError>` (`use borzoi_assembly::EcmaView` to
  call it on an `Ecma335Assembly`).

The trap is the sugar `EcmaView::enumerate_type_defs`, which discards the
`AssemblyProjectionSkips`: code and tests written against it cannot tell a
dropped type from an absent one. Probe with the with-skips form.

### What is still refused

Don't keep a list here; it goes stale. Two gates measure it:

- `crates/assembly/tests/manifests/bcl_ref_pack_projection.txt` pins every
  type in the .NET reference pack, with a `dropped-member` / dropped-type line
  per refusal and its reason. `grep -v 'members=[0-9]*$'` on it is the
  current census.
- `projection_skip_sweep` (ignored, `BORZOI_DLL_SWEEP_ROOT`) classifies the
  losses in a NuGet cache by cause; see `AGENTS.md`.

Structural corruption (cyclic nesting) stays fatal, via
`ImportError::CyclicTypeNesting`.

**Before treating a refusal as a coverage gap, dump the actual failing
`(signature, byte-count)` pairs.** An apparent gap can be a decoder bug: the "NullableAttribute byte[]
length mismatch" drops looked like a coverage gap but were `walk_nullable_sig`
skipping the pointer node in the `[Nullable]` pre-order flag walk, so `T*` /
`T*[]` positions came up a byte short (Roslyn visits the pointer node as
oblivious `0`, then the pointee).

Edge worth knowing: the F# overlays (`apply_measure_overlay` and friends in
`fsharp_pickle_merge.rs`) run on the *kept* subset, so a dropped measure/module
type could in principle re-escalate to `FsharpPickleMergeMismatch`. Not
reachable today (measure types are empty classes and never dropped; the BCL has
no pickle).

## 2. The ref pack and the SDK runtime BCL are different surfaces

- `fcs-dump types` type-checks against the **SDK runtime BCL** (default
  framework references).
- The sema member-access tests build their `AssemblyEnv` from the **reference
  assembly** — `packs/Microsoft.NETCore.App.Ref/*/ref/net10.0/System.Runtime.dll`,
  via `ensure_system_runtime_dll` (`crates/sema/tests/all/common/mod.rs`).

They disagree on overload sets. On the ref DLL's `System.String`,
`GetHashCode` and `ToString` are **overloaded** (2 instance candidates) and
`GetType` is **inherited from `Object`** (not declared on `String`, and there is
no base-class walk) — so all three **defer** in our resolver, even though an
SDK-BCL `fcs-dump` probe reports `s.GetHashCode()` as a single `call:instance`.

**So: when a differential test needs a single-candidate BCL method, verify it
against the ref DLL** by enumerating that type's members through
`Ecma335Assembly`, not against an `fcs-dump` probe.

Confirmed single-candidate, non-generic instance methods on `System.String` in
the ref pack:

| Member | Returns |
|---|---|
| `ToLowerInvariant()` | `string` |
| `ToUpperInvariant()` | `string` |
| `GetTypeCode()` | `System.TypeCode` |
| `Insert(int, string)` | `string` |

Related: `member-resolution-soundness` (why the absence of a base-class walk
matters for soundness rather than just coverage).

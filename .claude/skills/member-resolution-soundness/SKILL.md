---
name: member-resolution-soundness
description: The recurring soundness traps in `borzoi-sema` code that walks base chains or compares member signatures through `AssemblyEnv` — type identity ignoring the referencing assembly, name hiding across static/instance and kind, and structural `TypeRef` comparison. Use before writing or changing any member-access, inheritance, completion-through-inheritance, or overload-resolution logic in `crates/sema`, and when a reviewer flags one such soundness hole (audit all three at once instead of patching the one).
---

# AssemblyEnv member resolution: the three traps

Any sema code that walks base chains or compares member signatures through
`AssemblyEnv` hits the same family of soundness holes. On the inheritance slice
a reviewer found one facet per round for four rounds; they are all instances of
the three below. **Audit all three in one pass** rather than fixing the flagged
one.

## 1. `AssemblyEnv` resolves types by `(namespace, name, arity)` — it ignores `TypeRef::Named.assembly`

Only the first-enumerated definition per key is kept. So when two referenced
assemblies define the same full type name — or when a base's assembly is absent
while a same-named type from another assembly is present — the `by_type` slot
can hold the *wrong type*, and a base-chain walk silently walks into it.

The shipped fix in `resolve_base`: resolve a base only when the candidate's
assembly **name** matches the base's referenced assembly (or, when
`assembly: None`, the declaring type's own assembly).

Name — **not** the full `(name, version, public_key_token)` identity. A base is
compiled against a possibly-different version than the one loaded, and the
compiler binds by name with version redirection; full-identity matching
over-defers the ordinary case.

## 2. Name hiding is by *nearest declaring level*, across kind AND static/instance

A derived public **static** (or a non-method) named `M` hides an inherited
public **instance** `M`, but is unreachable through a value receiver — FCS
leaves `d.M` / `d.M()` as `obj`. So the "who declares this name?" check must:

- **include statics** — a same-name static at the *owning* level blocks, so defer;
- **exclude non-public members** — cross-assembly, an inaccessible derived member
  is removed *before* hiding applies, so a private derived `M` does not shadow a
  public inherited `M`;
- **ignore mere coexistence** — a static that simply shares the owning level with
  an instance method as part of one overload set (e.g. `Object.Equals(object)`
  alongside static `Equals(object, object)`) does **not** hide. Don't defer on it.

(The static-hiding claim is FCS-probed: `HDerived().M()` ⇒ `call:function` / `obj`.)

## 3. Comparing two `TypeRef` signatures to decide "same overload" is a rabbit hole — don't

Structural `TypeRef` equality is unsound here for at least three independent
reasons: cross-assembly meaning (`assembly: None` is relative to the *declaring*
assembly), `byref`/`out` param flags being stripped off `ty` onto
`Parameter::is_byref` / `is_out`, and nested-type ambiguity.

The inheritance slice originally deduped overrides this way and a reviewer found
a hole every round. The resolution was to **delete the dedup entirely**: collect
the group without deduping and defer any group of ≥ 2, so an overridden method
spanning levels simply defers. No differential coverage needed the dedup (all
typed cases are single-level).

**Lesson:** this was speculative complexity. When signature comparison keeps
generating soundness holes, delete it rather than keep hardening it.

## Working notes

- Probe FCS before trusting a review claim about FCS behaviour — the traps above
  are stated as *probed* facts, and claims that sound equally plausible have
  turned out false.
- The tests read the **reference pack**, not the SDK runtime BCL; see
  `assembly-surface-check` before choosing a BCL member to assert on.
- Deferral is the safe direction. Prefer "defer this group" over a clever rule
  that is right in the cases you thought of.

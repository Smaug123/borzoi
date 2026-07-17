# Plan: pickle-driven F# member projection (member-migration, post-PR1)

> **Status: complete (2026-07).** Stream-2 of
> [`fsharp-core-autoopen-resolution-plan.md`](../fsharp-core-autoopen-resolution-plan.md)
> §9.2/§9.3, sibling to
> [`completed/fsharp-pickle-expr-decoder-plan.md`](fsharp-pickle-expr-decoder-plan.md).
> The F# *member list itself* now comes from the signature pickle's module vals,
> subsuming the residual ECMA-335 module-member heuristics. All four slices
> landed; kept for history.

## Landed stages (one line each)

Prerequisites: the unpickler decodes real FSharp.Core (#546, #548) and PR1 (#551,
`apply_source_name_overlay`) made F# *source names* pickle-driven.

- **Slice A** (#556, #558) — IL reader gains bounded multidim arrays
  (`TypeSig::Array`, carrying the full `ArrayShape`) and pointer types
  (`TypeSig::Ptr`, incl. `void*`), clearing the `ELEMENT_TYPE_ARRAY` (`0x14`)
  reject so FSharp.Core projects end-to-end.
- **Slice B** (#909) — `collect_module_member_targets`
  (`crates/assembly/src/fsharp_pickle_merge.rs`): the read-only ordered
  module member-val index, each entry keyed by its OSGN `val_index` and carrying
  `arg_group_count` / `is_literal` / extension facts.
- **Slice C** (#913) — `apply_module_member_projection`
  (`crates/assembly/src/fsharp_pickle_merge.rs`): rebuilds every module's member
  list from the Slice B index on the authoritative path, each non-literal val
  claiming its IL member by `(il_name, shape, arity)` and stamping the val's
  source name + extension flag (generic vals included, closing the §7
  generic-extension gap). Retires the module-member source-name/extension
  overlays and the `is_generic_module_method` normaliser filter (fcs-dump now
  renders generic module `let`s; only generic *extension* members and
  IL-visibly-constrained generic bindings — `is_unmirrorable_generic_module_method` —
  stay elided).
- **Slice D** (#916) — `MethodLike::augmentation` (the per-member
  F#-native-extension fact, instance *and* static) + `Entity::is_extension_container`
  (the C#-style `[<Extension>]` marker); sema keeps every extension member out of
  unqualified scope and every F#-native one out of qualified static lookups too
  (`AssemblyEnv::open_static_entries` / `static_member`), closing the ⚠ D5 bug of
  `fsharp-core-autoopen-resolution-plan.md`. The `$W` witness filter
  (`is_fsharp_witness_duplicate`) was measured durable, not bridge code, and kept.

## Scope (§2)

Scoped to **module** members only. Every migrated ECMA-335 residual lived on the
module member path, and module member-vals live in `module_type.vals`.
Classes/unions/records keep their current IL projection — their structural
members (fields/cases) and augmentations project correctly through IL, and their
member-vals live in `tcaug` (a `vref`-resolution path with no current residual to
justify migrating it). Member *signatures* still come from the IL MethodDef
cross-referenced by compiled name; only the member *list* and its F# facts moved
to the pickle.

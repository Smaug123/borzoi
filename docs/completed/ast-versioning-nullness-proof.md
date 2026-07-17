# AST versioning — typed-facade proof via nullness (9.0)

> **Status (2026-06-28). ✅ Implemented (#618).** `crates/cst/src/syntax/projection.rs`
> (the surface predicate, `v8::Type`, `first_out_of_surface_type`) +
> `crates/cst/tests/ast_projection.rs` (the property suite). A concrete instantiation of
> [`ast-versioning-plan.md`](../ast-versioning-plan.md) Stage 3 — its first deferred
> stage — chosen to *prove the typed-facade projection mechanism* on a real,
> already-modelled node delta without waiting for F# to ship a new structural
> feature. Companion to that plan; read it first.

## Why this exists

Stages 0–2 shipped the *gate* (a language version flagging out-of-surface
constructs) and wired it into the LSP. The unproven half is the **typed facade**:
a frozen, exhaustive, per-version view that is a genuine *projection* of the
union, not a re-export. The plan deferred it because **F# 10.0 and 11.0 added no
new AST node kinds** (an empirical finding — F# evolution is overwhelmingly
semantic or relaxed-restriction; new node kinds are rare and concentrated in
8.0/9.0, all already modelled here). So there is no `v10`↔`preview` *typed* delta
to project across.

There is exactly one already-modelled node delta available: **nullness**
([`SyntaxKind::WITH_NULL_TYPE`], `string | null`), introduced in **F# 9.0**
(FCS `LanguageFeature.NullnessChecking`). It is the cleanest possible proof
subject because the delta touches **exactly one dispatch enum** ([`Type`]) and
**one node kind** — so the projection is one hand-written enum, not a forked
facade.

## The decision nullness forces (and how we settle it)

`#elif` (Stage 1) was *tree-invariant*: the gate was a post-hoc diagnostic scan;
the green tree was identical at every version. **Nullness is not** — FCS gates it
as a **parse divergence**: `LexFilter.fs` only emits the `BAR_JUST_BEFORE_NULL`
token the `WithNull` production consumes when `SupportsFeature(NullnessChecking)`,
so under < 9.0 `string | null` parses as something else entirely. Our parser, by
contrast, is *all-on* — it always parses `WITH_NULL_TYPE` by lookahead.

**Settled stance (D-proof-1): the green tree is always the maximal (preview)
parse; language version is a *lens* (which typed facade) plus a *diagnostic*
layer (what is out-of-surface), never a reshape of the tree.** This preserves the
clean invariant Stage 1 established (the tree does not depend on `lang`) and
makes the projection mechanism — the thing we want to prove — the whole story.

Consequence, and it is the load-bearing insight of this proof:

> For a **trivia** feature (`#elif`) the gate is a *separate* scan. For a
> **typed-node** feature (nullness) the gate **is** the projection's totality
> check: "the tree contains a node outside version N's surface" is identically
> "the `vN` projection is not total here" is identically "emit a diagnostic / the
> consumer drops to `.syntax()`". One mechanism, surfaced two ways.

### The lens's precondition: tree-monotonicity

The lens is sound **iff parsing is *tree-monotonic* across versions** — each
version's valid parse is the maximal parse restricted to that version's node
vocabulary. Gated parser changes fall into three classes; only the third breaks
the lens:

1. **Tree-invariant** — same tree at all versions; version affects only later
   stages. `#elif` (trivia); *and, notably, `arr[i]` indexer notation* — the
   canonical "means something different now" change (application pre-6.0,
   indexing post-6.0), which **FCS resolves in the type-checker
   (`CheckExpressions.fs`), not the parser** — the syntax tree is identical at
   both versions. Lens trivially sound; and as we do no full inference, such
   semantic reinterpretations are not even ours to represent.
2. **Monotonic addition (Case A)** — post-N syntax that was a *syntax error*
   before N. **Nullness is here**, confirmed by construction: the only type-level
   `BAR` production in `pars.fsy` is `… BAR_JUST_BEFORE_NULL NULL`, so pre-9.0
   `string | null` is not a rival valid type — it is an error. The maximal tree
   carries `WithNull`; old versions simply lack the vocabulary. Lens sound: there
   is no old meaning to misrepresent, only an out-of-surface node to diagnose.
3. **Reinterpretation (Case B)** — *same source, valid at both versions, different
   parse tree*. The **only** class the lens cannot serve, because the maximal
   tree commits to the new structure and the projection cannot recover the old
   one. Empirically F# keeps these *out of the parser* (see `arr[i]` above) — the
   same backward-compat discipline that forbids silently changing the meaning of
   valid code. No gated parser feature in F#'s history is known to be Case B.

This diverges from FCS for *invalid-at-vN* code (FCS errors differently on
pre-9.0 `string | null`; we produce the maximal tree + a diagnostic). Sound under
D7 ("incomplete, never wrong"): a post-N construct under an N pin is invalid at
N, so we owe it a *diagnostic*, not a faithful N-tree.

**Escalation path.** Should a future feature ever be a parser-level Case B, the
architecture already accommodates it without disturbing the facades: thread
`lang` into the parser *for that one production* (D3's "parse divergence" second
role). The lens and parse-divergence coexist — lens for the additive/invariant
majority, parse-divergence for the rare reinterpretation. **Detection caveat:**
catching such a violation automatically needs *version-aware* differential
testing (diff the corpus at several pinned langversions, not just the default);
the current single-version corpus diff would not flag it. Closing that is a
prerequisite to *claiming* full version-accuracy — tracked, not assumed.

## Settled decisions

### D-proof-2. Additive — no privatisation, no consumer churn

The production Stage 3 privatises the typed facade under `pub(crate) mod union`
so external consumers cannot bypass versioning. **The proof does not.** It *adds*
a sibling projection module alongside the existing `syntax` facade. Nothing
existing moves; sema/lsp are untouched. Privatisation is a production step with
real (if cheap) consumer impact; the proof's job is to validate the *mechanism*,
so it stays purely additive and zero-risk.

### D-proof-3. Project `Type` only; `≥ 9.0` surface == the union

The nullness delta touches only [`Type`]. So the proof hand-writes **one**
distinct enum, `v8::Type` (the 19 `Type` variants minus `WithNull`), and uses the
existing `union`/`syntax` [`Type`] as the `≥ 9.0` surface verbatim (it is
structurally identical — 9.0/10.0/11.0/preview add no further `Type` nodes). No
`v9`/`v10` types are written; they would be re-exports.

### D-proof-4. The surface predicate is the D5 interval-table seed

Gate and projection both consult one predicate — `type_kind_introduced(kind) ->
Option<LanguageVersion>` — with a single non-trivial row (`WITH_NULL_TYPE →
9.0`). This *is* the first row of the Stage-4 interval table (D5), written by
hand now; codegen generalises it later. Structuring the proof this way makes it a
faithful instance of the general mechanism, not a nullness one-off.

### D-proof-5. No accessor re-versioning

`v8::Type` is a *classification* (`cast` + the enum), **not** a facade with its
own accessors. The ~40 accessors that return [`Type`] keep returning the union
[`Type`]; a consumer that navigates into a child type still gets the union view.
Re-versioning every accessor so a `v8` walk stays in `v8` is precisely the
codegen-scale work (Stage 4) and is **out of scope** — the proof validates the
projection + properties, which need only `cast` and a tree walk.

## Artifacts

A new module (proposed `crates/cst/src/syntax/projection.rs`, `pub mod
projection`), depending only on existing `syntax` + `language_version`:

```rust
/// The language version a `Type` SyntaxKind first became legal. `None` ⇒ present
/// since before our floor (always in surface). One real row today — the seed of
/// the D5 interval table.
fn type_kind_introduced(kind: SyntaxKind) -> Option<LanguageVersion> {
    match kind {
        SyntaxKind::WITH_NULL_TYPE => Some(LanguageVersion::V9_0),
        _ => None,
    }
}

/// Whether a `Type` node of `kind` is legal at `lang`.
pub fn type_kind_in_surface(kind: SyntaxKind, lang: LanguageVersion) -> bool {
    type_kind_introduced(kind).is_none_or(|intro| lang >= intro)
}

pub mod v8 {
    /// The F# 8.0 `Type` surface: every `union::Type` variant except `WithNull`.
    /// A genuinely distinct, exhaustive enum — matching on it is total over
    /// *8.0-legal* type syntax, and the compiler enforces that a future variant
    /// is handled. The projection, not a re-export.
    pub enum Type {
        LongIdent(LongIdentType), Anon(AnonType), /* … 17 more … */ SignatureParameter(SignatureParameterType),
        // NO WithNull.
    }
    impl Type {
        /// `Some` iff `node` is a `Type` node whose kind is in the 8.0 surface;
        /// `None` for `WITH_NULL_TYPE` (a 9.0 node) and for non-type nodes.
        pub fn cast(node: SyntaxNode) -> Option<Self> { /* kind match, WITH_NULL_TYPE ⇒ None */ }
    }
}

/// Gate / totality check (the LSP-facing surface): the first `Type` node in
/// `root` that is outside `lang`'s surface, or `None` if the whole tree is
/// viewable at `lang`. Drives a "nullness types require F# 9.0" diagnostic and
/// *is* the executable statement of `vN`-projection totality.
pub fn first_out_of_surface_type(root: &SyntaxNode, lang: LanguageVersion) -> Option<SyntaxNode>;
```

## Properties (instantiating P1–P4 over `{v8, union}`)

Property-based, driven by the parser corpus plus targeted nullness fixtures
(`string | null`, `int list | null`, `string | null * int`, nested, none).

- **P-exclude / P-project (P2 totality, unconditional form).** For *every*
  `Type`-kind node `n` in any parse: `v8::Type::cast(n).is_some()
  == type_kind_in_surface(n.kind(), V8_0)` — i.e. `Some` for all 19 surface
  kinds, `None` exactly on `WITH_NULL_TYPE`. Combines totality (over 8.0-valid
  nodes) and exclusion (of the 9.0 node) in one corpus-driven assertion.
- **P-union-total.** `union::Type::cast(n)` is `Some` for every `Type`-kind node
  — the union is total (it must be; it is the maximal surface).
- **P-roundtrip (P3).** For every non-`WithNull` `Type` node `n`:
  `v8::Type::cast(n).unwrap().syntax() == &n` (same underlying `SyntaxNode`), and
  the union and v8 casts agree on that node. "The projection has not coarsened."
- **P-no-coarsen (P4).** The v8 kind→variant map is injective and agrees with the
  union on every shared kind — no two distinct surface kinds collapse to one v8
  variant. (Structural, asserted by construction + a kind-coverage test.)
- **P-gate (P1, LSP-facing).** `first_out_of_surface_type` finds the `WithNull`
  node under `V8_0` (and ≤ 8.0) and finds nothing under `V9_0`/`Preview`; a
  corpus file's nullness-free regions are never flagged.

## What it proves — and what it explicitly does not

**Proves:** the typed-facade projection is real and sound on an actual node delta
— a distinct exhaustive `vN` type, an exact projection of the union, the
totality/round-trip/no-coarsen properties, and the gate-as-totality-check
insight. That is the entire Stage-3 *mechanism*, validated, on already-shipped
code, in one hand-written enum + one predicate + a property suite.

**Does not (deferred, by design):**
- *Accessor re-versioning* — a `v8` walk staying in `v8` (D-proof-5) → Stage 4 codegen.
- *Privatising the union* / a public `ast::v8` surface (D-proof-2) → production Stage 3.
- *Parse-divergence* — replicating FCS's exact < 9.0 tree for `string | null`
  (D-proof-1) → a separate step (D3's second role), not needed for the mechanism.
- *Backfilling 4.6–9.0 as supported surfaces* — `v8` here is a proof instance,
  not a committed public version.

## Staging — done

One reviewable slice (additive, cst-only):

1. ✅ `type_kind_introduced` / `type_kind_in_surface` + `v8::Type` (enum + `cast` +
   `syntax`) + `first_out_of_surface_type`, in `syntax/projection.rs`.
2. ✅ The property suite in `tests/ast_projection.rs`: P-exclude/P-union-total/
   P-roundtrip and P-gate≡totality as proptests over **generated programs** (one
   `let` binding per generated `(base-shape, has-null?)` pair — a deterministic
   stand-in for the corpus that pins the nullness count exactly), plus
   example-based smoke tests. P-no-coarsen is asserted as kind-preservation
   (`v.syntax().kind() == n.kind()`) inside the round-trip property.
3. ✅ Plan cross-reference — see [`ast-versioning-plan.md`](../ast-versioning-plan.md)
   Stage 3, updated to record this proof and the two findings it banks: the
   gate-is-the-totality-check equivalence for typed-node features, and the
   tree-monotonicity precondition (D-proof-1) as the design basis for production
   Stage 3/4.

# MSBuild value-carried provenance plan (make the value carry its trust)

> **Status:** planning. No stage landed. Stage P0 is a *failing* test that
> pins a confirmed soundness hole; land it red-then-green before anything else.

## The rule

**A value's trust is produced by the code that reads the value, and travels
with the value. It is never re-derived from the text.**

That is the whole model, and it is the direct analogue of the escaped-value
rule (*scan and split before you decode; trim in the domain; decode at the
leaf* — `docs/msbuild-escaped-value-plan.md`). The escaped-value work removed a
side-channel (`literal_percents`) by making the *type* carry the escaping rule.
This work removes a side-channel (`unpinned_value_properties` +
`sdk_package_tainted_properties`, re-derived per use site by a second parser) by
making the *value* carry the trust rule.

## Why: the contract, and the half of it nothing checks

`borzoi-msbuild`'s contract is **certain-implies-exact**: when we commit a
value, MSBuild must agree exactly; a decline makes no claim. That is two
propositions:

1. *Where we commit, we're right.* Semantic. The five differential harnesses
   (`condition_diff`, `property_expr_diff`, `fsproj_property_table_diff`,
   `fsproj_item_escape_generative_diff`, `fsproj_global_perturbation_diff`)
   check this against real MSBuild.
2. *Where we're unsure, we actually decline.* Structural — a statement about
   the taint lattice, not about MSBuild.

**Nothing checks (2).** A differential is blind to it by construction: a wrong
decline and a right decline both pass. This is already recorded as a known
blind spot ("certain-implies-exact cannot see denials"). Everything below is
about (2).

## The defect, confirmed

Trust lives in **side maps keyed by lowercased property name**, disjoint from
the value:

- `State::unpinned_value_properties: HashMap<String, UnpinnedRoot>`
  (`evaluator.rs:~1150`) — rides the diagnostic pipeline; can flip
  `items_uncertain` / `define_constants_uncertain`.
- `State::sdk_package_tainted_properties` — a silent channel checked only at
  package/item sites, which deliberately *never* reaches `items_uncertain`.

At each **use** site, trust is re-derived from raw text by
`simple_property_references(raw)` (`evaluator.rs:4571`) — a *second,
independent, approximate* parser of `$( … )` syntax, distinct from the real
expander (`properties::substitute_impl` → `properties/expr.rs::evaluate`).
Call sites: `State::unpinned_root_for_raw` and
`State::sdk_package_taint_for_raw` (`evaluator.rs:~2738`, `~2752`),
`evaluator.rs:2941`, `evaluator.rs:4062`, and four in `condition.rs`
(~235–269).

Two parsers of one syntax must agree. Nothing makes them, and **they do not**.
`simple_property_references` recognises a method-call receiver via a hardcoded
five-method allow-list — `.TrimStart`, `.Split`, `.Contains`, `.StartsWith`,
`.EndsWith` — while `expr.rs::evaluate` supports more instance members than
that, including `Length` and `ToString`.

It fails in **two structurally different ways**, and neither is a mere coverage
gap:

- **Paren-less member (`$(Marked.Length)`)** — the identifier scan at
  `evaluator.rs:4591` accepts `.` as an identifier character, so it folds the
  member into the name and pushes the **bogus key `"Marked.Length"`**. That key
  then 404s against `unpinned_value_properties` forever, because a property
  name can never contain a dot (`properties/mod.rs:254-258` says so
  explicitly — inside `$(…)` a dot is always member access). A silent
  mis-parse, not a miss.
- **Parenthesised member outside the allow-list (`$(Marked.ToString())`)** —
  falls to the `rest.starts_with('(')` branch, matches no allow-list entry, and
  pushes **nothing at all**.

So the scanner is a hand-rolled approximation of a parser the crate already
has, and it is wrong in two different directions at once. That framing matters
for the fix: extending the allow-list would not even repair the first case.

Its own doc comment states the obligation it cannot discharge: *"otherwise a
shape readable by evaluation but invisible here would let an untrustworthy
property gate a construct without flagging it."* That is exactly what happens.

### Measured, with `Marked` untrusted (written under an unsupported gate)

| document | result | verdict |
| --- | --- | --- |
| `<Derived>$(Marked.Length)</Derived>` | `Derived = "6"`, untrusted = **false** | laundered |
| `<Derived>$(Marked.ToString())</Derived>` | `Derived = "net8.0"`, untrusted = **false** | laundered |
| `<ItemGroup Condition="'$(Marked.Length)' == '6'">` | `items_uncertain = **false**`, Compile item published | **certain-implies-exact violation on the item set** |
| `<Derived>$(Marked.TrimStart('x'))</Derived>` | untrusted = true | correct (allow-listed) |
| `<ItemGroup Condition="'$(Marked.Contains(\`net\`))' == 'True'">` | `items_uncertain = true` | correct (allow-listed) |

`.Major` / `.Minor` / `.Build` on a string receiver return no value at all —
the expression is unsupported, `had_unsupported` refuses the write, and the
safe path is taken by accident rather than by design.

The third row is the severe one: the Compile item set is what the LSP consumes
as its semantic fold order.

### Severity: the trap is latent and armed by ordinary progress

Censused over the pinned SDK 9.0.200 `.props`/`.targets` — every member access
spelled on a bare property receiver, `$(Name.Member…)`:

| member | SDK occurrences | in `expr.rs`? | in the scanner's allow-list? | today |
| --- | ---: | --- | --- | --- |
| `StartsWith` | 31 | yes | yes | propagates |
| `Contains` | 28 | yes | yes | propagates |
| `EndsWith` | 18 | yes | yes | propagates |
| `Split` | 11 | yes | yes | propagates |
| `TrimStart` | 5 | yes | yes | propagates |
| **`Length`** | **1** | **yes** | **no** | **launders** |
| `Replace` | 31 | no | no | declines (safe) |
| `ToLowerInvariant` | 20 | no | no | declines (safe) |
| `ToUpperInvariant` | 6 | no | no | declines (safe) |
| `Substring` | 3 | no | no | declines (safe) |
| `ToLower` / `Equals` | 2 each | no | no | declines (safe) |
| `Trim` / `ToUpper` / `LastIndexOf` | 1 each | no | no | declines (safe) |

So the *realised* leak in the pinned SDK is one site
(`$(MSBuildCopyMarkerName.Length)`). The argument for doing this work is
therefore **structural, not incident-driven**, and it is stronger for it:

- `expr.rs`'s supported-member list and the scanner's allow-list are two
  independently-edited lists that must agree, with **no mechanism making them
  agree** and no test asserting it.
- **68 SDK occurrences are sitting behind members `expr.rs` does not yet
  support.** Adding `Replace` to `expr.rs` — an ordinary coverage improvement —
  silently arms 31 laundering sites unless the author also happens to know
  about the allow-list in a different file.
- Worse, `sdk_chain_expression_census.rs` maintains a **coverage ratchet** that
  explicitly rewards raising the committed fraction. The gate that measures
  progress is the gate that arms the trap. The incentive structure points at
  the bug.

That is the case for P1: not "fix `Length`", but *delete the second list so the
next member addition cannot arm anything*.

### A third route, with no expansion to instrument

Twelve sites in `evaluator.rs` read a property **directly** via
`lookup.get_unescaped("Name")`, bypassing both the expander and the taint scan
entirely — `ImportDirectoryBuildProps`, `ManagePackageVersionsCentrally`,
`CentralPackageVersionsFileImported`, `CentralPackageVersionOverrideEnabled`,
`DirectoryPackagesPropsPath`, `LangVersion`,
`MSBuildDisableFeaturesFromVersion` (in `expr.rs`), and others. None consults
trust. `LangVersion` is published on `ParsedProject` with no trust gate;
`ManagePackageVersionsCentrally` gates whether package uncertainty is raised at
all, so an untrusted `false`/absent reading silently skips the uncertainty it
exists to add.

This route matters for staging: **no amount of instrumenting the expander
closes it**, because there is no expansion. Only trust living on the value
does. It is the load-bearing argument for Stage P3.

## Why this is a class, not three bugs

The repo's own record names the class: *"absent vs unread is the recurring
defect — four wrong-answer classes across two crates, all a field conflating
'provably none' with 'we did not look'"*, and *"allow-list by subtraction is
the wrong shape — three rounds of 'and this shape too' means measure the
subset, don't guard it."* The five-method allow-list is precisely an allow-list
by subtraction. The fix is to **measure what was read**, not to extend the
guard.

`PropertyProvenance { taint, unpinned }` (`evaluator.rs:1678`) is the existing
mitigation: a struct whose entire purpose is to force both channels to be named
at each write, so "a new write path cannot update one map and silently forget
the other." That is a discipline patch on a representational problem — the two
channels are separate objects joined by a string key, and the type system is
being asked to enforce, by convention, something the representation should make
unrepresentable.

## Stages

Each stage is an independently reviewable PR that is green on its own.

### The two buckets — read this before touching any call site

The ~8 `simple_property_references` call sites are **not interchangeable**.
They split along a boundary that decides what a correct fix looks like:

- **Bucket A — value bodies (safe to make exact).** The evaluator write-time
  sites (`unpinned_root_for_raw`/`sdk_package_taint_for_raw` at ~2738/2752,
  and the inline scans at ~2941 and ~4062) scan the raw text of a *single value
  body*. `substitute_impl` processes every `$(` in that text unconditionally in
  a `while` loop with no branching; within a block, `eval_root` always reads the
  root property before walking the member chain, and the chain walk is a linear
  `for link in &expr.links`. Nothing is computed then discarded. Here an
  evaluation-order read set is genuinely exact.

- **Bucket B — conditions (must stay over-approximate).**
  `condition.rs:206`'s `refs_outside_empty_comparison` walks the **whole parsed
  boolean tree**, and its comment states exactly why: *"evaluation above
  short-circuits (`'$(X)' == '' Or '$(X)' == 'x'` never expands the second arm
  when the first is true), so evaluation-order records would under-report
  non-default uses."* This is a **control-dependency** problem sitting *above*
  the expansion layer: the skip happens in `eval_bool`'s `And`/`Or` handling,
  before `expand_for_condition` is ever called on the untaken arm.

**Consequence: replacing Bucket B's scan with "names actually read" would
reintroduce precisely the under-reporting that code was written to prevent.**
The related `short_circuit_skips_undefined_collection_on_decided_branch` test
(`condition.rs:1469`) already pins the correct behaviour for the sibling
undefined-collection question.

Note the layering: in Bucket B the *tree walk* solves the branch-context
problem, and `simple_property_references` is only the **leaf extractor** applied
to each leaf's raw text. So the tree walk is right and must stay; the leaf
extractor is broken in both buckets. That is what makes one fix serve both.

### P0 — the failing test (red first)

Two properties, one per bucket — not one, because they have opposite safety
directions:

- **A:** for a value body `s`, every name `substitute` actually looks up is
  present in the syntactic scan of `s`. Fails today on `.Length` and
  `.ToString()`.
- **B:** for a condition `c`, `refs_outside_empty_comparison(c)` is a superset
  of every name *any* branch could read, short-circuited or not. Passes today —
  land it as a **regression** test so the P1 work cannot quietly break it.

Plus the three concrete cases from the measured table above.

A caveat on shape: containment is the right assertion *today*, but a pure
containment test can never catch a future regression in which the scan starts
under-reporting — which is the direction that actually matters. Design the
fixture set so it can be tightened toward equality for Bucket A once P1 lands.

**Sized:** small. No production change.

### P1 — delete the second parser, don't build a third channel

The crate already has a parser for `$( … )`: `expr::parse` (`expr.rs:200`),
producing `Root` / `Link` / `Member`. `simple_property_references` is a
hand-rolled re-implementation of it that is wrong in two directions. The fix is
to **delete the hand-rolled one and walk the real parse tree**:

for every `$(…)` span, parse it; if the root is `Root::Property(name)`, record
`name`; recurse into every `Link::Member` argument list and every
`Root::Static` argument list for nested `$(…)`.

This is purely **syntactic** — no evaluation needs to succeed — so it stays in
the safe over-approximating direction and is immune to the short-circuit
hazard. Which means **it serves both buckets**, unlike exact-reads plumbing.

**Caveat: `expr::parse` returns `Option`, so it is partial.** A shape it cannot
parse must not silently yield an empty reference set — that is the current bug
in a new costume. The `None` arm needs a deliberately crude fallback (every
identifier-shaped token following a `$(` in the span, dots split at the first
`.`), documented as intentionally over-approximate.

Why this replaces the "report what was actually read" design I first drafted:
that plumbing changes `substitute`'s and `expr::evaluate`'s return types and so
moves a share of the ~21.6k lines of dependent tests, while *not* being usable
for Bucket B at all. The parse-tree walk is one function body, no signature
churn, and closes the confirmed hole everywhere.

**Sized:** small-to-medium. One function replaced, one fallback added.

### P1′ — exact reads (precision, not soundness) — deferred

Threading the actually-read set out of `substitute` / `expr::evaluate` remains
worth doing *for Bucket A only*, but its benefit is **fewer spurious declines**,
not soundness, once P1 lands. Re-sequenced after P2/P3 and explicitly labelled
a precision improvement so nobody reviews it as a fix.

### P2 — one lattice instead of two maps

Replace the two parallel side maps with a single `Trust` value — a product of
the two channels, since they have genuinely different downstream semantics and
collapsing them would change behaviour:

```rust
struct Trust {
    unpinned: Option<UnpinnedRoot>,
    sdk_package: Option<SdkPackagePropertyTaint>,
}
```

with a sealed `join` (per channel, first cause wins — matching today's
`find_map`). `PropertyProvenance`, `TaintOutcome` and `UnpinnedOutcome`
collapse into it. The "forgot one channel" hazard becomes unrepresentable
rather than convention-enforced.

**Property-test the algebra:** `join` is associative, commutative, idempotent,
with `Trust::CERTAIN` as identity, and *monotone* — `join` never returns
`CERTAIN` unless both inputs were. That last one is the soundness property and
the only one that matters.

**Sized:** medium. Mechanical; churn concentrated in `evaluator.rs`.

### P2′ — audit the twelve direct-read sites (small, standalone)

Before committing to P3's blast radius, close the *known* instance of the
direct-read route cheaply: audit the twelve named `get_unescaped` sites against
the eleven-entry `msbuild-trust-audit` checklist, in one PR, touching no types.

A second instance of the same class, found while planning:
`seed_toolset_properties` (`evaluator.rs:~2149-2158`) calls `insert_computed`
with **no** matching `apply_property_provenance`. It is harmless today only
because of the `if get(name).is_none()` fresh-insert guard. Note what this
proves about P2: pairing the two channels in one struct forces both to be named
*at the call site that already remembers to call it* — it does nothing to stop
a different write path from skipping the call entirely. That gap is P3's
argument, not P2's.

**Then re-run the SDK census and decide P3's priority from residual risk**,
rather than committing to it up front.

### P3 — value-carried

`PropertyMap`'s `Entry` gains `trust: Trust` beside its `Escaped` value.
`PropertyMap::get` returns the pair; `substitute` returns
`Provenanced<Escaped>`. `Trust::CERTAIN` becomes unforgeable outside a sealed
module — the only public constructors are `join` and the "read from a certain
source" entry points.

This is the stage that closes the **direct-read route** (twelve
`get_unescaped` sites), because it makes ignoring trust a thing you have to
write code to do rather than a thing you get by default.

**Sized:** large. The public-ish surface of `PropertyMap` changes, so the
~21.6k lines of tests in `src/tests.rs` + `src/with_imports_tests/` are the
real cost. Mitigation: keep `get_unescaped` as a `#[deprecated]`-style
trust-discarding accessor named to say so (`get_unescaped_ignoring_trust`), so
the migration is mechanical and each remaining call site is a visible,
greppable decision rather than a silent default.

**Two questions to settle before starting P3, not during:**

1. **Trust is not a substitute for consequence-side flags.** The
   `msbuild-trust-audit` skill warns *never gate a fold on the generic
   provenance seam* — `property_provenance_untrusted` fires for essentially
   every real SDK project, which is why `LangVersion`'s fold-safety today uses
   a bespoke consequence-side mechanism (`shape_depends_on_language_version`)
   instead. So P3 must say, per site, whether carried `Trust` **replaces** or
   **coexists with** the bespoke signal. At least one of the twelve needs
   coexistence. A uniform "P3 closes all twelve the same way" is wrong.
2. **`Provenanced<Escaped>` must not create a new way to strip either
   guarantee.** `Escaped` is deliberately crate-internal so "no consumer
   outside the evaluator can pick the wrong one". A bare `.value` field access
   that silently discards `Trust` would be the escaped-value hole re-created one
   level up. Whatever `Provenanced` looks like, leaving trust must be as
   explicit as leaving the escape domain — that symmetry is the design
   constraint, since this plan is explicitly modelled on that one.

### P4 — the same treatment for items and metadata

`tainted_item_lists`, `untracked_item_lists`, `HelperMetadataUncertainty`, and
the `*_uncertain: bool` fields. Turn every *absent vs unread* boolean into a
DU. This is where the recurring defect class is finally closed rather than
narrowed.

**Sized:** large; do not start it before P1–P3 have settled.

## The systematic gate

Per `CLAUDE.md`: what structural testing would have made noticing this
unnecessary?

**Metamorphic taint-closure property.** This is not speculative — the precedent
is this branch's own immediately-preceding commit. `dc3cdc20` ("a Compile
item's `Link` carries a knowability verdict") fixed a structurally identical
*absent-vs-unread* conflation, guarded by `tests/fsproj_link_metadata_diff.rs`,
a generative differential over
{SDK kind × placement × declaration × gate × include form}. Its commit message
records the payoff exactly:

> It reports **36 wrong commits** without this change, against the single
> instance the whole-project corpus sweep had shown — which is the argument for
> sweeping the axes rather than fixing the instance.

That is the same argument, one layer down, and it is in-repo evidence rather
than analogy. Here:

> For a generated project document `D` and a property `P` that `D` writes:
> wrapping `P`'s write in an unsupported gate must make **every** value whose
> expansion read `P` untrusted, and must set `items_uncertain` if any
> Compile-affecting site read `P`.

No oracle needed — this is internal monotonicity, checkable against ourselves.
It subsumes the whole allow-list question: any member access the expander
supports is, by construction, a shape the perturbation must propagate through.

**The generator already exists, and it already spells the laundering shapes.**
`tests/common/mod.rs::gen_grammar_link` (line 1218) draws from
`PARENLESS = ["Major", "Minor", "Build", "Length", "Bogus"]` and
`METHODS = ["Contains", "StartsWith", "TrimStart", "Split", "Substring",
"ToString", "Nope"]` — i.e. `$(Foo.Length)` and `$(Foo.ToString())`, the two
confirmed laundering shapes, are *already generated today*, alongside negative
controls. So the metamorphic gate can reuse `gen_grammar_value` verbatim and
the usual vacuity risk (*"a shape the generator can't build makes both
implementations agree vacuously"*) is largely pre-paid.

**Which makes the real finding sharper: the generator is not the problem — the
question is.** `property_expr_diff.rs` generates `$(Foo.Length)` today and
passes, because it asks *"does our expansion equal MSBuild's?"* (it does) while
evaluating a bare expression against a supplied property table, with no notion
of an untrusted property. It never asks *"was the taint carried?"*. This is the
"certain-implies-exact cannot see denials" blind spot in its most concrete
form: **full shape coverage, zero soundness coverage**, because the harness
evaluates expressions rather than projects.

The metamorphic gate must therefore be a **project-level** harness
(`parse_fsproj`-shaped, like `fsproj_property_table_diff`), reusing the
expression grammar for the value bodies. That is the missing seam, not a
missing generator.

One coupling still worth adding: have `expr.rs`'s dispatch and the generator's
`METHODS`/`PARENLESS` tables derive from a single shared list, so a member added
to the evaluator is automatically generated. Without it, the *next* member is
covered only if its author remembers two files — which is the same failure mode
one level up.

Also required, per the "perturbation floors must exclude swept inputs" note: the
floor must be on the *derived complement* (values that read `P`), not on `P`
itself, or it is circular.

### Change what the census ratchet measures

Whichever stage lands the fix must also change `sdk_chain_expression_census.rs`.
It currently ratchets on *coverage of `expr.rs`'s supported members*, with no
check that the reference scanner stayed in step — so the next contributor to add
`Replace` (31 SDK occurrences waiting) re-arms the bug **and the ratchet
applauds them for it**. Re-point it at "zero disagreement sites between the
reference scanner and the real evaluator", or the fix decays exactly as the
"commit-count floors rot when the product grows" note predicts.

## Consultation record

Reviewed by a second model against the code. Three findings changed the plan:

1. **The original P1 was unsound as scoped** — it would have replaced
   `condition.rs`'s deliberately over-approximating whole-tree scan with an
   under-approximating evaluation-order read set, regressing a short-circuit
   safety property the codebase had already built on purpose. Verified against
   `condition.rs:202-206` and the comment stating the reason. This produced the
   two-bucket split, which is now the plan's central structural claim.
2. **A smaller fix dominates the one I drafted** — reusing `expr::parse` for
   the leaf extractor closes both buckets with no signature churn, where
   exact-reads plumbing closes one bucket at the cost of test churn. Exact
   reads demoted to P1′ and relabelled a precision improvement. (Reviewer
   called the parse walk "total"; it is not — `expr::parse` returns `Option`,
   hence the mandatory fallback noted in P1.)
3. **P3 was asserted rather than justified** — the twelve direct-read sites can
   be audited cheaply and standalone (P2′), so P3's marginal value is
   *durability against the thirteenth site*, which should be priced after the
   audit rather than assumed. P3 also inherits two unresolved design questions
   now recorded against it.

The `.Length` failure was also mis-described in the first draft as "no
reference extracted"; it is a mis-parse producing the bogus key
`"Marked.Length"`. Corrected above, because it changes the fix: extending the
allow-list would not have repaired it.

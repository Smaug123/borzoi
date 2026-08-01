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
that, including `Length` and `ToString`. A receiver outside the allow-list
yields **no reference at all**, so the taint scan sees nothing to propagate.

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

### P0 — the failing test (red first)

The cheapest statement of the obligation the code leaves implicit, as a
one-sided containment:

> For every expression shape the generator produces,
> `simple_property_references(s)` ⊇ { names `substitute` actually looked up }.

Over-approximation is the safe direction for taint, so containment (not
equality) is the property. It fails today on `.Length` and `.ToString()`.

Land it `#[ignore]`d-red or as a documented expected-failure, whichever the
crate's convention supports, plus the three concrete regression cases from the
table above. **Sized:** small. No production change.

### P1 — exactness at the source

Make expansion report the set of property names it **actually looked up**.
This is far cheaper than it sounds: there are exactly **two** `props.get` call
sites in the whole crate — the fast path in `substitute_impl`
(`properties/mod.rs:266`) and `Root::Property` in `expr.rs:512`. Both already
sit inside functions that thread a `Vec<Issue>` out, so the read-set rides the
same channel.

- `substitute` / `substitute_with_fs` return the read-set alongside
  `(Escaped, Vec<Issue>)`.
- `expr::Evaluated` gains the read-set next to `value` / `issues`.
- Every `simple_property_references`-based taint rescan consumes the reported
  set instead.
- `simple_property_references` is deleted, or demoted to a diagnostics-only
  helper with no taint consumer.

**Open design question — exact vs over-approximate.** The reported set is
*exact*; the syntactic scan is an *over-approximation*, and for taint
over-approximation is the safe direction. Where evaluation bails out (an
unsupported expression whose residual text still references a tainted
property), or short-circuits, an exact read-set is *smaller* than the truth.
The likely formulation is
`(names actually read) ∪ (names syntactically referenced inside any
sub-expression not fully evaluated)` — exactness where we evaluated,
over-approximation where we did not. **This is under review; see "Consultation"
below.**

**Sized:** medium. ~8 taint call sites, 2 read sites, 2 return types.

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

### P4 — the same treatment for items and metadata

`tainted_item_lists`, `untracked_item_lists`, `HelperMetadataUncertainty`, and
the `*_uncertain: bool` fields. Turn every *absent vs unread* boolean into a
DU. This is where the recurring defect class is finally closed rather than
narrowed.

**Sized:** large; do not start it before P1–P3 have settled.

## The systematic gate

Per `CLAUDE.md`: what structural testing would have made noticing this
unnecessary?

**Metamorphic taint-closure property**, in the mould of `borzoi-assembly`'s
`modifier_metamorphic` probe (decorate every signature node and re-project;
a `modopt` must move nothing). Here:

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

## Consultation

Design review requested from a second model on: whether P1's exact read-set is
sound versus the current over-approximation (bail-out, short-circuit, discarded
sub-expressions, `Exists()`); whether P3 earns its cost once P1+P2 land; the
vacuity failure modes of the metamorphic gate; and the staging cut. Findings to
be folded in before P0 is written.

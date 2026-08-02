# MSBuild value-carried provenance plan (make the value carry its trust)

> **Status:** **P0, P1, P2′, P2″ and P2‴ landed.** The reference scan is
> a walk of the same parse tree evaluation uses, so a member access can no
> longer launder its receiver's provenance (#265); the gates read MSBuild
> booleans rather than strings (#266); an undecided `Directory.Build.*`
> gate now withdraws every item facet instead of publishing a wrong one; and
> the third forward-uncertainty channel is inside `PropertyProvenance` rather
> than beside it, so it can no longer drift from the other two.
>
> **Open, in the order I would take them:**
>
> 1. P2 — the lattice. P2‴ found the live evidence for it: the "forgot a
>    channel" hazard had already happened, to a channel the struct did not
>    name.
> 2. The three remaining **precision** items under P2″ — declines, not wrong
>    answers, and each carries regression risk (see the fifth-round entry).
> 3. P1′, P4.
>
> **P3 is deferred on measurement, not on effort** — the census (re-run
> 2026-08-02, zero wrong commits) shows we commit on 17% of the SDK's call
> expressions and 5% of its conditions, so the surface a laundering defect
> could reach is small. Its trigger is the committed fraction rising; see
> "The census, re-run".

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

### P0 — the failing test (red first) — **landed**

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

### P1 — delete the second parser, don't build a third channel — **landed**

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

**As landed.** `simple_property_references` moved from `evaluator.rs` into
`properties/expr.rs` beside the parser it approximated, and its body became a
walk of `parse`'s tree: report `Root::Property`'s name, ignore a `Root::Static`
(its arguments' `$(…)` are spans the outer scan reaches in their own right),
and on a parse failure fall back to the leading name run. Interiors are trimmed
before parsing, because MSBuild tolerates `$( Foo )` and the condition tokeniser
resolves it. Both the observed failures — `$(X.Length)` yielding the bogus key
`"X.Length"`, and `$(X.ToString())` yielding nothing — are gone, and no member
list is left to maintain.

### P1′ — exact reads (precision, not soundness) — deferred

Threading the actually-read set out of `substitute` / `expr::evaluate` remains
worth doing *for Bucket A only*, but its benefit is **fewer spurious declines**,
not soundness, once P1 lands. Re-sequenced after P2/P3 and explicitly labelled
a precision improvement so nobody reviews it as a fix.

### P2 — one lattice instead of three maps

Replace the parallel side maps with a single `Trust` value — a *product* of the
channels, since they have genuinely different downstream semantics and
collapsing them would change behaviour:

```rust
struct Trust {
    unpinned: Option<UnpinnedRoot>,
    sdk_package: Option<SdkPackagePropertyTaint>,
    refused: bool,
}
```

with a sealed `join` (per channel, first cause wins — matching today's
`find_map`). `PropertyProvenance`, `TaintOutcome`, `UnpinnedOutcome` and
`RefusedOutcome` collapse into it. The "forgot one channel" hazard becomes
unrepresentable rather than convention-enforced.

There are **three** channels, not two, and P2‴ is why the count is written down
here: `unevaluable_written` was outside `PropertyProvenance` and drifted from
its siblings for as long as it was outside. That is the concrete instance of the
hazard this stage abolishes, so P2 is no longer speculative tidying — it is the
generalisation of a defect that has now actually happened.

**Property-test the algebra:** `join` is associative, commutative, idempotent,
with `Trust::CERTAIN` as identity, and *monotone* — `join` never returns
`CERTAIN` unless both inputs were. That last one is the soundness property and
the only one that matters.

**Sized:** medium. Mechanical; churn concentrated in `evaluator.rs`.

### P2′ — audit the twelve direct-read sites — **landed**

Before committing to P3's blast radius, close the *known* instance of the
direct-read route cheaply: audit the twelve named `get_unescaped` sites against
the eleven-entry `msbuild-trust-audit` checklist, in one PR, touching no types.

**Then re-run the SDK census and decide P3's priority from residual risk**,
rather than committing to it up front. Done — see below.

#### The `seed_toolset_properties` "gap" is not one — the asymmetry is required

An earlier draft of this plan flagged that `seed_toolset_properties` calls
`insert_computed` with no matching `apply_property_provenance`, "harmless today
only because of the `if get(name).is_none()` fresh-insert guard", and filed it
as evidence for P3. **That was wrong, and acting on it would have introduced a
wrong commit.**

The function seeds two groups and treats them differently on purpose:

| group | names | seeded | provenance |
| --- | --- | --- | --- |
| reserved | `MSBuildToolsPath`, `MSBuildBinPath`, `MSBuildToolsVersion`, `MSBuildRuntimeType` | over anything | **scrubbed** |
| overridable | `MSBuildSDKsPath`, `MSBuildExtensionsPath32`, `MSBuildExtensionsPath` | into an empty slot only | **left alone** |

Ground truth (dotnet 10.0.301, probed 2026-08-02 — a plain `<Project>` writing
each name, read back with `-getProperty`):

```text
<MSBuildToolsPath>/SPOOFED</…>          → error MSB4004: … reserved, and cannot be modified
<MSBuildSDKsPath>/SPOOFED</…>           → "/SPOOFED"
<MSBuildExtensionsPath32>/SPOOFED32</…> → "/SPOOFED32"
```

That is the whole explanation. A reserved name's accumulated value and taint
describe a write real MSBuild *rejects*, so they describe nothing and are
scrubbed. An overridable name's refused write may really have set the property
in the real build, so our computed path is a **fallback, not the answer**, and
the surviving mark is exactly right. Clearing it — the natural "tidy-up", and
what the old TODO invited — turns a correct decline into the walker publishing
its own guessed path as certain.

Verified, not argued: `tests/toolset_seed_provenance.rs` pins both directions
(refused write before any SDK resolves ⇒ overridable stays untrusted, reserved
does not), and the tidy-up fails it.

The general lesson is worth more than the case: **an omitted call is not
evidence of an oversight.** Reading the two groups as one shape is what
produced the bogus TODO.

#### P2′ verdicts — the audit is closed

Every direct-read site, with what it turned out to need. Two were defects, one
was a bogus TODO, and the rest were already discharged — recorded here so the
next reader does not re-derive them.

| # | site | read | verdict |
| ---: | --- | --- | --- |
| 1–2, 9–11 | `evaluator.rs` ×5 | `ImportDirectoryBuild{Props,Targets}` | **defect, fixed** — read MSBuild booleans (#266); consult trust at the splice (P2″) |
| 3 | `evaluator.rs` | `DirectoryPackagesPropsPath` | **no consumer to protect** — probed on a real SDK chain, CPM references are uncertain under an undecidable redirect *and* both cleanly-decided controls, because the inline discharge never fires there |
| 4–5 | `evaluator.rs` | `ManagePackageVersionsCentrally`, `CentralPackageVersionsFileImported` | **defect, fixed** (#266); trust already covered at the write site by `package_context`, pinned by `inline_cpm_tainted_{manage_flag,import_marker}_stays_uncertain` |
| 6 | `evaluator.rs` | `CentralPackageVersionOverrideEnabled` | **conservative by construction** — the SDK forwards it to the restore task rather than comparing it, so its vocabulary is NuGet's C# and this crate cannot ground-truth it; widened to treat either reading's `false` as opting out, which retains uncertainty under both |
| 7 | `evaluator.rs` | the `written` loop building `ParsedProject::properties` | **already discharged** — published beside `untrusted_properties`, and both real consumers (`workspace.rs`, reading `TargetFramework`) compute `property_provenance_untrusted` and thread it |
| 8 | `evaluator.rs` | `LangVersion` | **already discharged** — `semantic.rs` reads `property_provenance_untrusted("LangVersion")` into the fold's cache key, alongside the bespoke consequence-side `shape_depends_on_language_version` the `msbuild-trust-audit` skill describes |
| 12 | `properties/expr.rs` | `MSBuildDisableFeaturesFromVersion` | **fail-safe by construction** — evaluates only when the table holds exactly the `999.999` sentinel, and returns `Unsupported` for every other reading including an untrusted one |
| — | `evaluator.rs` | `seed_toolset_properties` | **not a gap** — see above; the omitted call is required |

Two defects out, both confirmed against the oracle before any code changed.

1. **The gates read MSBuild booleans as strings** (#266, landed as its own
   change). `'$(ImportDirectoryBuildProps)' == 'true'` is an MSBuild `==`, so
   `on`/`yes`/`!false` open it; three sites decided such comparisons with
   `eq_ignore_ascii_case("true")`. The reachable half was
   `ManagePackageVersionsCentrally`, where reading an opted-in project as
   opted-out skips `package_references_uncertain` entirely.
2. **The gates do not consult trust at all** — P2″ below.

### P2″ — an undecided gate write leaves the item set wrongly certain

The confirmed repro. `Directory.Build.targets` contributes a Compile item; the
entry project writes the gate under a condition we cannot evaluate:

```xml
<PropertyGroup>
  <X>abc</X>
  <ImportDirectoryBuildTargets Condition="'$(X.Substring(0,1))' == 'a'">false</ImportDirectoryBuildTargets>
</PropertyGroup>
```

MSBuild evaluates that condition to `true` (oracle-confirmed), so it writes the
gate `false` and **skips** `Directory.Build.targets`. We cannot evaluate
`Substring`, so we never write the gate, read it as absent, default it to true,
import the file, and publish its Compile item with `items_uncertain = false`.

That is a wrong *item set* — the thing the LSP folds over — and it is worse
than the boolean defect it was found next to: it needs no exotic spelling, only
a property function in a condition, and it is reachable on ordinary SDK
projects.

#### The measurement that says this is affordable

The obvious fear is the `msbuild-trust-audit` skill's §2: *never gate a fold on
the generic provenance seam*, because it fires for essentially every real SDK
project. Probed before designing anything (real `Microsoft.NET.Sdk` project,
dotnet 10.0.301, both `Directory.Build.*` files present):

| property | `property_provenance_untrusted` | value |
|---|---|---|
| `ImportDirectoryBuildProps` | false | `true` |
| `ImportDirectoryBuildTargets` | false | `true` |
| `DirectoryBuildPropsPath` | false | *(the real path)* |
| `DirectoryBuildTargetsPath` | false | *(the real path)* |
| `ImportDirectoryPackagesProps` | false | `true` |

**85 other properties on that same project are untrusted; none of these are.**
The SDK writes the gate names cleanly. So a trust-consulting gate does not
collapse into a wholesale decline, and this does *not* need the bespoke
consequence-side mechanism `LangVersion` needed. That is the single fact which
makes P2″ a small change rather than a research project, and it is why the
probe comes before the design.

#### The fix is a **read-site** check, and two review rounds were needed to get there

The first cut latched at the *write*, copying `define_context` /
`package_context`: set a context around writes of the four gate names, and let
`push` turn a diagnostic into `items_uncertain`. Review killed it, twice, and
the second round is the instructive one.

- **Round 1 — incomplete.** The context covered only the write's own condition.
  An enclosing `<PropertyGroup Condition>` or `<When>` is evaluated *before* the
  walker reaches the property child, and a skipped branch never reaches one at
  all, so those routes still committed. (Checklist entries 2 and 3, found
  exactly as the skill predicts: one missing entry per round.)
- **Round 2 — wrong seam.** Write-site latching over-declines in two concrete
  ways. A later clean unconditional overwrite re-pins the value, so both sides
  make the same import decision and the decline is spurious. And a body read
  that happens *before* the import is exactly empty on both sides regardless of
  the gate — probed (dotnet 10.0.301, 2026-08-01): with
  `Directory.Build.targets` present and defining `FromDirBuild`, a body
  `<Reads>[$(FromDirBuild)]</Reads>` reads `[]` while the final `FromDirBuild`
  reads `set`. A test written in round 1 asserted that spurious degrade *as a
  requirement*; it was wrong and was replaced.

Why the precedent misled: `define_context` and `package_context` latch at the
write and that is *equivalent* for them, because their values are consumed at
`into_project` — every write precedes the read. The gate is consumed
**mid-walk**, so the two differ. Which is this plan's own rule, stated at the
top and then not followed: *trust is produced by the code that **reads** the
value*. The final design asks the question once, of the final value, at the
moment the splice consumes it (`State::gate_value_is_exact` +
`note_directory_build_splice_decision`).

That collapses all three placements into one check — a group or `Choose`
condition that could not be decided already leaves its writes unpinned, so the
read site sees it without needing to know where the write was.

#### The predicate is name-keyed, *not* the generic exactness question

`gate_value_is_exact` deliberately consults only the three name-keyed channels
(`unpinned_value_properties`, `unevaluable_written`, SDK package taint) and
**not** `undefined_read_is_exact`, which folds in `walk_opaque`. Routing
opacity in here made six existing tests fail with "SDK structural package
uncertainty must not reintroduce Compile uncertainty" — opacity is latched by
ordinary SDK structure, so this was the generic-seam wholesale decline arriving
by a different route. The narrow question ("was a write *of this name*
undecided, refused, or SDK-tainted?") is the one the splice needs, and it holds
whether or not the name currently has a value.

#### Every consumption point, and the two precision items left open

The check sits at all four points a splice decision is made: the
pre-`Sdk.targets` snapshot (which serves the fallback splice), the chain's own
`Directory.Build.targets` import point, the entry props splice, and the
deferred nested-SDK props splice — that last one resolves and imports directly
rather than calling `fire_entry_directory_build_props_splice`, so it needed its
own. Enumerating these by hand is what review kept catching, which is why the
property names now live in two constants the production code reads and the
sweep enumerates.

A fourth round raised the sharpest of these and it *was* fixed: the
pre-`Sdk.targets` snapshot's verdict was being recorded unconditionally, even
though the snapshot is discarded when the chain reaches its own import point —
so an SDK that cleanly re-pins the gate before that point would have declined a
project for nothing. The verdict now travels to the fallback branch that
actually uses it.

A fifth round found more of that family, fixed — and then a **sixth round
found that the fifth's fix had opened a soundness hole**, which is the useful
part of this record. Guarding the verdict on "a file resolved" is wrong when
the *path* is the unpinned input: our resolution finding nothing is no evidence
when MSBuild resolved from a different value and imported something. The two
inputs are not symmetric, and the rule is now stated as such —

  * unpinned **path** ⇒ uncertain whatever resolved;
  * unpinned **gate** ⇒ uncertain only once a file has actually resolved.

That oscillation (too lax → too eager → too lax) is the signal that per-site
reasoning about "can this decision change the outcome?" is the expensive part,
not the plumbing. The regression is pinned by
`an_undecided_path_redirect_flags_even_when_nothing_resolves_here`, which fails
against the fifth round's shape.

The sixth round also found the facet gap: the verdict set only
`items_uncertain`, leaving `project_references_uncertain` and
`package_references_uncertain` trusted, though a `Directory.Build.*` file
declares `<ProjectReference>` and `<PackageReference>` as readily as
`<Compile>`. It now routes through `mark_structural_skip`, the existing "an
import that could not be resolved to a definite decision" primitive, which
marks every facet and records a cause — rather than a bespoke flag pair, which
is how the gap arose.

What the fifth round was reaching for is still true and still applied, just
correctly scoped: with an *exact* path resolving to nothing, MSBuild's own
`exists('$(DirectoryBuild*Path)')` is false whatever the gate says, so both
sides skip and no decline is owed. That `items_uncertain` is not a cheap flag —
it stops the LSP folding the project — is what makes the distinction worth
getting right rather than rounding to "always decline".

Four **precision** items were knowingly left, none a wrong commit — they cost
declines, not exactness. Given the fifth round's regression, the bar for
chasing a decline is now "pin it with a test that fails against the looser
shape", not "it looks unnecessary". The first is **done** (see P2‴ below); the
rest stand:

- ~~`unevaluable_written` is insert-only, never cleared by a later clean write,
  so a refused write followed by a clean unconditional overwrite still declines
  although the final value is exact.~~ Fixed by P2‴ — and the interesting part
  is that it was not a precision item at all, but the visible end of a third
  channel sitting outside the discipline that exists to prevent exactly that.
- `is_sdk_directory_build_targets_import_point` also accepts an `Exists`-only
  custom-SDK import, whose condition does not read `ImportDirectoryBuildTargets`
  at all — so for that shape an undecidable write to the gate declines for
  nothing. Deriving the checked inputs from the actual condition text would fix
  it, and is more per-site reasoning of exactly the kind that produced the
  fifth-round regression, so it is recorded rather than attempted.
- An exactly-resolved file that was *already walked* makes both possible
  decisions identical (import-dedup makes the duplicate a no-op), so the
  verdict could be skipped there.
- `note_directory_build_splice_decision` ORs the gate and path verdicts, so an
  *exactly* `false` gate — which proves the file cannot be imported — still
  declines if the path is untrusted, though the path is then irrelevant on both
  sides. Basing the decision on the resolved outcome rather than on both inputs
  would recover it.

#### The props/targets asymmetry is real and is pinned

A body write of `ImportDirectoryBuildProps` cannot change the props import on
*either* side — `Directory.Build.props` is imported before the body. Probed:
a body `<ImportDirectoryBuildProps>false</…>` still leaves the file's
`FromProps` reading `set`. So the sweep asserts the targets pair *flags* on an
undecidable body write and the props pair *does not*, with the partition
asserted exhaustive against the declared name list — a name added to the
walker's directly-read set without a decision about when it is consumed fails
the test rather than going untested.

#### The open question, settled by measurement: **no SDK-subtree tolerance**

`define_context` is deliberately set only when `!in_sdk_subtree`, and
`compile_context` tolerance is justified by "an SDK sub-import we can't follow
never drops a *hand-written* source". **That justification does not transfer**:
a wrongly-skipped `Directory.Build.props` drops hand-written Compile items by
construction. No such exemption was added. Note this is a *different* question
from the `walk_opaque` one above: the read-site predicate never asks the
generic exactness question, so SDK noise does not reach it, and the strictness
that remains is name-keyed. Measured rather than assumed:

- the whole `borzoi-msbuild` suite passes unchanged;
- a real `Microsoft.NET.Sdk` project still commits — `items_uncertain=false`,
  its Compile item published, 85 properties untrusted as before;
- the MSBuild corpus differential is **unchanged at `skipped_facets=11`,
  `divergences=0`** (`matched_facets` 30→31 is #263's fix, not this change).

So the strict reading costs nothing measurable, and tolerance stays out.
Adding it would have silently reintroduced the defect for every project whose
SDK conditions the gate.

#### Sweep the siblings in the same change — **both resolved without code**

Per the checklist's "a reviewer finds exactly one missing entry per round"
discipline, the other direct reads with the same shape were swept too. Neither
needed a change, and the reasons are worth keeping because they are the two
standard ways this audit *should* terminate.

- `DirectoryPackagesPropsPath` (`evaluator.rs:~596`) decides
  `redirected_central_file_walked`, which **suppresses**
  `package_references_uncertain` — so an untrusted value looks like the same
  defect one consumer over. **It is not, because the consumer never commits.**
  Probed on the real SDK chain with CPM enabled: `package_references_uncertain`
  is `true` under an undecidable redirect, under a cleanly-decided-false one,
  *and* under a cleanly-decided-true one. The inline CPM discharge does not
  fire on a real SDK project at all, so there is no wrong commit to prevent.
  This is `check-for-a-consumer-before-paying-for-a-trust-verdict` in its
  literal form. **Landmine for later:** if the discharge is ever made to fire
  on real projects, this trust verdict becomes load-bearing and must be added
  with it.

  The first attempt at a test for this was **vacuous** and was deleted rather
  than shipped: in the SDK-less unit harness `Directory.Packages.props` is
  never imported, so `package_references_uncertain` was `true` whatever the
  condition said. It passed identically with the redirect decidable and
  undecidable. Any test in this family needs its control arm run before it is
  believed.

- `MSBuildDisableFeaturesFromVersion` (`properties/expr.rs:~1006`) is
  fail-safe by construction: it evaluates *only* when the table holds exactly
  the `999.999` sentinel and returns `Unsupported` for every other reading,
  including an untrusted one. Audited, no change.

#### The systematic gate

A unit test per gate name is what this codebase already tried; the boolean
defect sat behind one. The generative shape instead: **for each property name
the walker reads directly, a case that writes it under an undecidable
condition, asserting the consumer's uncertainty flag is set.** The name list is
closed and already exists in the source (`is_cpm_flag_property_name` and the
gate names), so the sweep can be driven off it rather than hand-maintained —
which means adding a new directly-read name to the walker without also
declaring it forces a failure.

Note what a whole-project differential cannot do here, learned from #266:
MSBuild agrees with us on the *value* of a gate property in all these cases;
the divergence is in what we then *do* with it. A value-witness harness is
blind to it, exactly as `fsproj_derived_tfm_diff` is blind to a moved import.
The oracle's role is to establish the ground truth for the fixture (as it did
for `Substring` above), not to be the assertion.

**Sized:** small — one context flag, four names, ~5 call sites, plus the two
siblings. Strictly smaller than P3, and it closes the *reachable* half of what
P3 would close.

### P2‴ — the third channel, brought under the discipline — **landed**

The first precision item above turned out not to be a precision item. Chasing
it asked the obvious question — *why is this map insert-only when its two
siblings clear?* — and the answer is that it is not a sibling at all in the
code: there are **three** name-keyed forward-uncertainty channels, and
`PropertyProvenance`, the struct whose stated purpose is that "a new write path
cannot update one map and silently forget the other", named only two.
`unevaluable_written` was mutated by three hand-written `insert` calls that
bypassed `apply_property_provenance` entirely — so the one rule the discipline
was built to enforce could not apply to it, and the missing `Clear` is exactly
the drift the struct exists to prevent, one level out.

That is the same finding this plan opens with, applied to itself: *"a discipline
patch on a representational problem."* A channel that is not in the type is not
disciplined by the type.

**The fix.** A third field, `refused: RefusedOutcome`, with `Set`/`Clear`/`Keep`
like its siblings; the refusal sites name `Set` instead of inserting; and the
clean-write
site derives `Clear` from the sibling verdict:

```rust
fn after_write(unpinned: &UnpinnedOutcome) -> Self {
    match unpinned {
        UnpinnedOutcome::Clear => RefusedOutcome::Clear,   // clean value, clean gate
        UnpinnedOutcome::Set(_) | UnpinnedOutcome::Keep => RefusedOutcome::Keep,
    }
}
```

The channels share the predicate but not a field, and the reason is worth
stating because it is why a mechanical `derive`-style collapse would be wrong:
the refusal sites record `Set` here while recording `UnpinnedOutcome::Clear`
there, because they *remove* the binding rather than storing an unpinned value.
Riding on `UnpinnedOutcome::Clear` is also the safety argument — that predicate
("clean value under a clean gate re-pins") is already audited and pinned by
existing tests, so no new judgement about when a write is trustworthy was
invented here.

**The test is a sweep over *why* a write is untrusted**, which is the axis the
existing gate sweep does not vary (it varies *where* the deciding condition
sits). Five kinds — undecidable condition on the write, on the group, and
unevaluable value via expression / item reference / metadata reference — each
asserted twice: left alone it must decline, cleanly overwritten it must commit
*and* produce the right item set. Red before the fix on exactly the three
value-kinds and green on the two condition-kinds, which is the asymmetry stated
as a failure. Ground truth for all three (dotnet 10.0.301, 2026-08-02): MSBuild
reports the overwrite, `false`, in every case — it evaluates `@(Foo)` and
`%(Bar.Identity)` in a property body without complaint at a point where no item
exists.

**What it buys, measured before writing it.** Instrumenting the clean-write site
and running the whole `borzoi-msbuild` suite: the refused-then-cleanly-rewritten
shape occurs for exactly five names, all from the real SDK chain —
`RootNamespace`, `OutputPath`, `IntermediateOutputPath`,
`TargetFrameworkVersion`, `TargetFrameworkIdentifier` — and **none of them is a
name either consumer asks about while the stale mark stands**
(`undefined_read_is_exact` is only reached for a name with no binding, and
`gate_value_is_exact` is asked only of the four splice properties). Both
censuses are unchanged by the fix: 66/396 and 139/2758, still exactly on their
floors.

So the honest accounting is that this recovers **zero declines today**, and it
is still the right change: the value is that the third channel can no longer
drift from the other two, and the next person to add a write path is forced to
say what it does to all three. A decline recovered would have been the smaller
prize.

Two of the newly-named outcomes are likewise **inert**, and are recorded as such
rather than defended as fixes: the reserved-toolset seed now scrubs the refused
mark alongside the other two (its own comment already promised to scrub "both
provenance marks", and leaving one of three is the drift being fixed), and the
unmodellable-body refusal now says `Set` where the unpinned root it already
records declines for every consumer anyway. Neither is observable — a reserved
name is never a splice property, and the unpinned root subsumes the refusal —
so neither carries a test. Naming an outcome that cannot be wrong is the price
of a struct that forces every write to name one.

### The census, re-run — and what it says about P3's priority

Measured 2026-08-02 against the pinned SDK **10.0.301** (the P1 member table
above is 9.0.200 and is left as the record of that round). Every harness green;
**zero wrong commits anywhere.**

| census | population | committed | declined |
| --- | ---: | ---: | ---: |
| SDK-chain call expressions | 396 distinct | **66** (17%) | 330 |
| SDK-chain conditions | 2 758 distinct | **139** (5%) | 2 619 |

Both coverage ratchets sit *exactly* on their floors (66 and 139), so the gate
is tight: any regression fails, and the numbers have not drifted.

Global-perturbation movement, the other half of the picture:

| sweep | committed (name, globals) pairs | move in MSBuild | we track |
| --- | ---: | ---: | ---: |
| corner routes | 132 | 14 | **14** (0 declined) |
| generated documents | 1 470 | 152 | 108 |
| real SDK chain | 215 | 36 | 11 |

#### The decision: **P3 is not urgent, and the trigger to revisit it is not a date**

P3's value was priced in the consultation record as *durability against the
thirteenth direct-read site* — making "ignore the trust" something you must
write code to do. Weigh that against what the census actually shows:

1. **The committed surface is small.** We commit on 17% of the SDK's call
   expressions and 5% of its conditions. A trust-laundering defect can only
   bite where we commit, so today's blast radius is bounded to that slice — and
   in the rest, an unrelated decline masks it anyway.
2. **The residual risk P3 addresses is not currently realised.** P2′ audited
   all twelve direct-read sites: two were defects (both fixed), one TODO was
   bogus, the rest were already discharged. There is no known thirteenth site.
3. **The declines are dominated by something else entirely.** Both census
   comments record that the remaining declines are mostly undefined *reserved*
   receivers, which trusted seeding turns on wholesale. That is the coverage
   lever, and it is unrelated to trust plumbing.

Point 3 is the interesting one, because it inverts the sequencing. **Seeding
is exactly what makes P3 worth doing**: it multiplies the committed fraction,
and the committed fraction *is* the blast radius. Doing P3 first pays for
durability over a surface we mostly decline; doing it after seeding pays for
durability over the surface that seeding just opened up.

So: **P3 is deferred, and its trigger is the committed fraction rising** — i.e.
land trusted seeding, re-run this census, and re-price P3 against the new
numbers. Should either ratchet floor be raised for any other reason, that is
the same signal.

The one thing that would override this is a *new* direct-read site appearing
without the audit noticing. The P2″ sweep is driven off the splice-property
constants for exactly that reason, but it only covers the names it knows;
a genuinely new read is still caught by review rather than by the machine.
That, not the current numbers, is P3's standing argument.

### P3 — value-carried — **deferred on measurement** (see "The census, re-run")

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

### The census ratchet — resolved by construction, not by a change

This section originally required re-pointing `sdk_chain_expression_census.rs`
from *coverage of `expr.rs`'s supported members* to *zero disagreement between
the scanner and the evaluator*, on the grounds that the coverage ratchet rewards
exactly the change that arms the bug.

**P1 makes that moot, and it is worth being explicit about why**, since the
reasoning is the argument for having deleted the parser rather than fixed it:
there is no longer a second list to disagree with. Teaching `expr.rs` `Replace`
now makes `property_references` report the receiver *in the same edit*, because
both read one parse tree. The ratchet cannot arm a trap that no longer exists.

Had P1 instead extended the allow-list — the obvious small fix — this section
would still be required, permanently, as the standing guard on a coupling no
type enforces. That difference is the whole return on choosing the structural
fix over the local one.

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

# MSBuild's unix path fixup (`MaybeAdjustFilePath`)

> Found by the E0 decoder differential in `docs/msbuild-escaped-value-plan.md`,
> on its first run against real MSBuild. It is **not** an escaping bug and does
> not belong to that stack: it is a separate, host- and cwd-dependent
> normalisation.

> **Status:** P0 (#948) and the P3 keystone (#953) have landed. The keystone
> needed only *two path functions* — `[System.IO.Path]::Combine` and
> `IsPathRooted` — to stop declining on backslash and instead commit
> cwd-independently, which is all the SDK chain's fixup-ambiguity ever reaches.
> The full **provenance dimension** — a stored fixup-divergent value being made
> exact for the property table / `DefineConstants` / `target_name` — is *not* on
> the critical path and is deferred until a consumer needs it. Everything below
> "Landed stages" has detail only on that remaining work.

## The behaviour (reference)

On **non-Windows hosts only**, MSBuild's expander passes every expanded piece
through `FileUtilities.MaybeAdjustFilePath` (`Expander.cs:186,225,229`;
`FileUtilities.cs:608`). For a piece that

- is not empty, and does not start with `$(`, `@(` or `\\`, and
- contains a `\`,

MSBuild converts **every** `\` to `/` *and collapses runs of slashes*
(`CollapseSlashes`, a `[\\/]+` → `/` rewrite — so `a\\b` and `a\/b` both become
`a/b`), then keeps the rewrite **iff the result "looks like a unix file path"**
(`LooksLikeUnixFilePath`, `FileUtilities.cs:718`): its first segment exists **as
a directory**, probed with `Directory.Exists(Path.Combine(baseDirectory, seg))`.

The `baseDirectory` is the load-bearing part, and it differs by call site:

| call site | `baseDirectory` | decidable by us? |
| --- | --- | --- |
| property values (`Expander`'s builder) | **`""` → the MSBuild process's current working directory** | **No** |
| item metadata (`LazyItemOperation.cs:215,252`) | the containing project's directory | Yes |

So **the evaluated property table is not a function of the project text**: the
same project evaluates differently depending on the directory `dotnet build` was
invoked from. We have no MSBuild process and therefore no cwd to consult — the
value is *genuinely ambiguous*, not merely unknown. An *escaped* backslash is
invisible to the fixup (`.%5cx` keeps its backslash where `.\x` would not): the
fixup runs on escaped text, upstream of unescaping.

## Why declining is wrong (reference)

"A backslash-bearing property value is ambiguous, so decline it" would poison
the SDK chain: `Microsoft.Common.props` writes `<BaseIntermediateOutputPath …>obj\</…>`
(one of ~46 backslash-bearing SDK property writes), and `BaseIntermediateOutputPath`
feeds `MSBuildProjectExtensionsPath`, which discovers the `NuGet.props` →
`Directory.Packages.props` chain. Declining it would take package resolution down
for every project — to fix a divergence with no consumer-visible consequence:
`obj\` and `obj/` name the same directory once used as a path, and we normalise
separators at every path use. The ambiguity is real but usually **immaterial**.

The model that follows: for each eligible piece the fixup has exactly **two**
possible results (probe hits ⇒ `converted`, misses ⇒ `raw`). Evaluate the
consumer under each world and commit iff they agree. Path uses are provably
immaterial (both worlds normalise to the same `Path::components()`), conditions
bracket ≤ 2^k worlds and commit on unanimity, exact-value consumers degrade only
on genuine disagreement. A key refinement the keystone forced: a fixup-eligible
piece consumed *through a path function* collapses its two worlds back to one
(`Combine`'s result crosses an expansion boundary and meets `MaybeAdjustFilePath`
again), so a value whose worlds agree is *committed determinate*, not degraded.

## Landed stages (one line each)

- **doc** (#928) — pin the behaviour against real MSBuild and plan the response.
- **P0** (#948) — `properties/path_fixup.rs`: `fn worlds(value) -> Option<String>`
  (the eligibility + `\`→`/`/slash-collapse rewrite, ported from `FileUtilities.cs`
  with citations), plus the two-cwd `path_fixup_diff.rs` oracle that runs `dotnet
  msbuild` from a hit-cwd and a miss-cwd and asserts our two worlds bracket both
  answers exactly. Inert on Windows.
- **P3 keystone** (#953) — the fixup-ambiguity in the MSBPEP chain never reaches a
  *stored* exact value; it is consumed entirely inside two functions whose
  committed answer is cwd-independent (oracle-pinned both cwds, guarded by the
  two-cwd expression differential in `path_fixup_diff.rs`):
  - `[System.IO.Path]::Combine` converts a **live** backslash unconditionally, so
    `combine_path` (joins with `/`, rewrites each part) *is* the answer; the old
    "declines any backslash-bearing part" guard was removed. An **escaped** `%5c`
    survives as a literal `\` that `combine_path` cannot represent, so the shared
    path-arg layer (`eval_exact_path_arg`) declines it *for Combine only* (flag
    `reject_escaped_backslash`). `NormalizePath`/`GetDirectoryNameOfFileAbove`
    normalise via `GetFullPath` (converts a backslash however spelled) and keep
    committing.
  - `[System.IO.Path]::IsPathRooted` commits every **non-leading** backslash
    (rootedness is fixed by the first character, which the fixup leaves alone) and
    declines only a **leading** one (a live `\a`→`/a` is indistinguishable from an
    escaped `%5ca` once decoded).

  This pins `MSBuildProjectExtensionsPath` to `<projdir>/obj/`, keeps
  `walk_opaque` from latching at the `Common.props` import gate, un-ignores
  `tests::msbuild_project_extensions_path_normalises_the_obj_default`, and drops
  the `sdk_style_netcoreapp_fsharp_implicit_dependencies_match_msbuild` Stage-C
  fixture **270 → 52** causes (the `TargetPlatformVersion`/`PublishAot`/
  `RuntimeIdentifier`/`TargetsNet*` cascades were all collateral of this one
  decline). No world-set, provenance bit, or table degradation was needed.

The three superseded stages — the original P1 (conditions bracket the worlds),
P2 (exact-value consumers degrade), and their "scan the evaluated property table"
framing — are folded into the deferred provenance work below. They were wrong to
treat fixup-ambiguity as a property of the *final evaluated string*; it is a
property of the *derivation* (see the three defects below).

## Still to do

### The provenance dimension (deferred until a stored divergent value must be exact)

The keystone made every fixup-ambiguity in the SDK chain collapse before it was
stored, so nothing exact-valued currently observes divergence. When a consumer
*does* need a stored fixup-divergent value to be exact — the concrete trigger is
`BaseIntermediateOutputPath`, which stays genuinely divergent (`obj\` vs `obj/` by
cwd), is never consumed as an exact string in the chain, yet **is** exported
through `ParsedProject::properties` and today is committed as `obj\` on `main`
(a pre-existing latent gap, undetected because no test runs from an `obj/`-present
cwd) — model fixup-ambiguity as its own provenance dimension, exactly like the
existing `unpinned_root` / `untrusted_properties` provenance:

- **Record at the write.** When a *project-body* property write's expanded value
  has `worlds() == Some`, mark it fixup-ambiguous and carry its worlds.
- **Propagate through reads** via the same `Expansion` root-tracking that carries
  `unpinned_root`.
- **Consume uniformly.** Every exact-value consumer (property table,
  `DefineConstants`, `target_name`/`AssemblyName`) reads one bit and degrades;
  path uses ignore it (proved immaterial); conditions bracket the tracked worlds.

Three facts the design must respect (each is why the superseded "scan the final
string" framing was unsound):

1. **Ambiguity propagates through references; the final string has lost it.**
   `<A>obj\</A><B>\\server$(A)</B>`: `B` is cwd-dependent (`\\serverobj/` vs
   `\\serverobj\`) yet `worlds(B)` is `None` because `B` starts `\\`. Provenance
   must track the derivation, not re-inspect the result.
2. **Command-line globals bypass the fixup.** `-p:G=obj\Debug\` stays `obj\Debug\`
   for every cwd (globals never go through the `Expander` builder), so `worlds(G)`
   is `Some` but the value is not ambiguous. The bit must key off *where a value
   came from* (a project-body expansion), not its final shape.
3. **Exact-value consumers are not one chokepoint.** `target_name` is computed
   before any table-wide scan would run; the property table, `DefineConstants`,
   `target_name`, and conditions each consume independently and must read the
   *same* bit.

And the sharpest form of defect 1: **worlds are per *expansion piece*, not per
composed value.** MSBuild runs `MaybeAdjustFilePath` at each expansion-piece
boundary as the value is built, so the world set is the product over eligible
pieces. Oracle-pinned counter-example: `<Empty/><P>a\x;$(Empty)b\y</P>` under a
cwd where only `b/` exists evaluates to `a\x;b/y` (first literal missed the fixup,
second hit it); a `{raw, worlds(raw)}` model of the finished string tracks only
`a\x;b\y` / `a/x;b/y` and would wrongly commit `'$(P)' == 'a\x;b/y'` **false**.
So the provenance must preserve literal/splice boundaries and bracket each
eligible piece independently. The k-cap (suggest 4 → 16 evaluations, decline
beyond) bounds the *tracked* set between collapses.

**Unpinned prerequisite — the collapse rule.** The unanimity/collapse rule is
*not* `path_fixup::worlds` applied to a composed result:
`worlds('/repo/proj/obj\')` is `{…/obj\, …/obj/}`, which does not reduce to a
singleton, and dropping the backslash is unsound (a *literal*
`/definitely-missing/proj/obj\` stays raw — oracle round-2). Yet
`[System.IO.Path]::Combine('/definitely-missing/proj','obj\')` evaluates to
`/definitely-missing/proj/obj/` (converted) *even though the root is missing* —
a `Combine` **result** does not follow the same `LooksLikeUnixFilePath`
existence-probe as a bare literal. The precise rule (when a `Combine`/expansion
result collapses vs stays two-valued) is a `MaybeAdjustFilePath` semantic this doc
has **not** pinned. Milestone 1's first task is to pin it with a differential
oracle, not to assume one.

**Milestone 1: collapse-aware bracketing + the property table.** Track the
world-set through property/condition expansion; commit the single agreed value on
a singleton-after-collapse, carry the provenance bit otherwise. In the *same*
change, either omit fixup-divergent entries from `ParsedProject::properties` or
route them through a new `untrusted` channel — committing a divergent value's
representative text as trusted violates the table's certain-implies-exact contract
in the other cwd. **The guarding oracle does not exist yet and building it is part
of this milestone:** today's `fsproj_property_table_diff.rs` runs the resident
oracle from a single fixed cwd with no backslash / `BaseIntermediateOutputPath`
case; a hit-cwd/miss-cwd table (or evaluator) differential must be added *first*,
as it pins both the collapse rule above and this degradation (and closes the
`obj\`-on-`main` latent gap).

**Milestone 2: the non-table exact consumers.** `DefineConstants`,
`target_name`/`AssemblyName` read the same provenance bit and degrade — but only
earn their place once a fixture actually routes a divergent value into them.
Defer until milestone 1 is in and such a fixture demands it.

### Note on item metadata

Item metadata takes the *project directory* as its probe base, so it is fully
decidable — we can model it exactly rather than bracket it (probe the filesystem,
which the with-imports evaluator already does). Out of scope until a consumer
needs metadata exactness; recorded here so the asymmetry is not rediscovered.

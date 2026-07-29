# Locating a redirected project-reference producer — **ABANDONED**

> **Status: abandoned (2026-07-30).** Two attempts, ~10 review rounds across
> three reviewers, no landing. The approach — read the producer's output
> directory out of its `.fsproj` by static evaluation — is the thing that
> failed, not any particular implementation of it. This is a post-mortem: what
> the gap is, why document analysis cannot close it, and what a third attempt
> should do differently. Nothing from either attempt is on `main`.

## The gap

A `<ProjectReference>` producer is located by
[`locate_fsharp_output_dll`](../crates/lsp/src/semantic.rs), which scans
`<project_dir>/bin/<config>/<tfm>/<output_name>.dll`. A producer that redirects
its output —

```xml
<PropertyGroup><OutDir>artifacts/</OutDir></PropertyGroup>
```

— has an empty `bin` tree, so the scan finds nothing and the reference is
reported unbuilt. Every symbol the consumer takes from it goes unresolved.

The scan is *sound*: it sweeps every configuration on disk and either finds a
DLL under the standard layout or declines. It is only incomplete, and only for
redirecting producers.

## What both attempts tried

Give `ParsedProject` an output-directory verdict from the same evaluation that
already yields `tfm` and `output_name`, and have the env fold locate against it:

```rust
enum OutputDirVerdict { Default, Declared { path: String }, Unknown }
```

`Declared` the only claim; `Default` and `Unknown` both fall through to the
`bin` scan. So the contract is one-sided by construction — a producer we fail to
classify is missed exactly as it is today, never resolved to a *wrong*
directory. The whole design rests on `Declared` being trustworthy.

The certification argument for `Declared` was: commit only for a **sole
unconditioned `<OutDir>` whose body contains no `$(...)`**, plus "a document we
did not scan cannot be certified against" (a completeness flag,
`out_dir_scan_incomplete`, set wherever the walk skips or fails to follow a
document). Certify the *write*, and you need not enumerate the ways a value can
move.

## Why it does not work

**The construction certifies the write. The consumer needs the value. They are
not the same thing, and the SDK is what separates them.**

Verified against real MSBuild on 2026-07-30, with a sole unconditioned `<OutDir>`
literal carrying no property reference at all:

```xml
<OutDir>artifacts/</OutDir>
<GenerateProjectSpecificOutputFolder Condition="'$(Configuration)' == 'Debug'">true</GenerateProjectSpecificOutputFolder>
```

| `-p:Configuration=` | MSBuild's `OutDir` |
| --- | --- |
| `Debug` | `artifacts/P/` |
| `Release` | `artifacts/` |

`GenerateProjectSpecificOutputFolder` makes the SDK **rewrite a non-empty user
`OutDir`**, appending `$(ProjectName)`. The `<OutDir>` write satisfies every
clause of the certification — sole, unconditioned, literal — and the value it
produces still moves with the build configuration. The second attempt's walker
commits `Declared { path: "artifacts/Lib\\" }` for this project, which is right
for the Debug the LSP pins as a global and wrong for the Release the user may
have built. That is precisely the class the attempt's own tests refused
everywhere they could see it — a redirect that varies with a build dimension —
arriving through a route no property in the document mentions.

No completeness flag saves this. The document was scanned perfectly. The write
was certified correctly. The value simply is not the write.

## The review sequence, which is the real evidence

The first attempt (`outdir-aware-ref-location`) was parked after roughly six
rounds, each finding a new route by which a committed value could be wrong. It
was parked on a *stated* objection — "a later document can overwrite the entry
literal last-write-wins" — which the second attempt measured through the
resident MSBuild oracle and found does **not** hold: every overwriting route
through a document the walk can pin already declined.

So the second attempt revived it, and four more distinct leak classes arrived
after it had been declared sound:

| found by | route | axis |
| --- | --- | --- |
| Fable | `Microsoft.Common.targets` imports a *user* hook via `$(CustomBeforeMicrosoftCommonTargets)`; the SDK-subtree exemption tests the **site**, not the document | completeness |
| codex P1a | **`GenerateProjectSpecificOutputFolder` rewrites the certified literal** | **construction** |
| codex P1b | a skipped `<ImportGroup>` / unselected `<Choose>` arm containing imports | completeness |
| codex P1c | a hook whose path is itself user-selected (`$(Configuration).targets`) | completeness |
| codex P2 | the `canonicalize` error path | completeness |

Three of those four are "set the flag at one more site". That is the
enumerate-the-leak-routes game the design's own prose claimed to have escaped —
it had not escaped it, it had renamed it. And the fourth says the escape was
never available: certifying a write does not certify a value, when the SDK
chain that runs afterwards is entitled to read that value and write it back.

Ten-ish rounds, three reviewers, a new class each time, converging on nothing. The
signal is about the approach, not about the remaining bugs.

One sub-finding is worth keeping on its own. The first fix for the Fable route
assumed the gate had read an *undefined* value, and did not work. Instrumenting
showed why: a write the walker refuses does not leave the name undefined, it
leaves **the SDK's own default** standing. The gate then reads a defined value,
decides cleanly, raises no diagnostic, and looks exact — while the value it read
is not the one the real build has. Only `unevaluable_written` records that. Any
future exactness claim in [`evaluator.rs`](../crates/msbuild/src/evaluator.rs)
that reasons "the read was defined, so the decision was exact" has this bug.

## What a third attempt should do

**Ask MSBuild, do not model it.**

```sh
dotnet msbuild <producer>.fsproj -p:Configuration=Debug -getProperty:OutDir
```

This is exact by construction in the way static evaluation is not: the answer
comes from the same evaluator that will run the build, so every route above —
SDK rewrites, unpinnable hooks, `<Choose>` arms, property functions — is not
modelled, it is *executed*. There is no model to leak from and no completeness
flag to maintain.

Costs and prerequisites, so a third attempt starts with them priced:

- **One process spawn per producer**, on the order of a second. Cache it on the
  `Workspace` project memo beside the existing evaluation, invalidated by the
  `workspace/didChangeWatchedFiles` machinery already in place
  ([`file-watch-invalidation-plan.md`](completed/file-watch-invalidation-plan.md)).
- **It must go through [`borzoi-spawn`](../crates/spawn/)** under a deadline, as
  everything that shells out does; `clippy.toml` enforces it.
- **A timeout, a missing SDK, or a non-zero exit yields `Unknown`**, which falls
  through to the `bin` scan — the same one-sided contract, but now the only way
  to lose is to fail to *ask*, not to answer wrongly.
- **The configuration residual survives** and is not solvable by any method: the
  LSP asks under `Configuration=Debug` because it has to pick, and a user who
  built Release built somewhere else. The `bin` scan is robust here precisely by
  being dumb — it sweeps every configuration present on disk. So even an exact
  `OutDir` should probably be treated as *one more directory to scan* rather
  than as the answer.

That last point is the one to think about first, because it suggests the
smaller and better-shaped feature: not "compute the output directory" but
"widen the set of directories the scan sweeps". A scan is inherently one-sided;
a computed path is a claim, and this exercise is a record of how expensive
claims are.

## Why this is not urgent

The `bin` scan covers every producer that does not redirect, and redirecting is
the minority shape. A redirecting one degrades to under-resolution — symbols
taken from that reference go unresolved — which is the failure mode the whole
env fold is built around tolerating, and the same one an unbuilt producer
already produces. Nobody is served a wrong answer today, which is why a
mechanism that can serve one is a bad trade for this much coverage.

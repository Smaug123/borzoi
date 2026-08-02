# Reserved-name seeding: what it is actually worth

> **Status:** planned, and deliberately *not* started. The measurement below
> was made to price the work, and it came out at a few per cent of what the
> comments claimed. What follows is the case for doing a small, well-bounded
> slice of it and for cutting the rest loose from the plan it was blocking.

## Why this document exists

Five comments in `crates/msbuild/tests/` forward-referenced "Stage C.2's
trusted seeding" as the lever that would raise the SDK-chain census, and
[`msbuild-value-carried-provenance-plan.md`](msbuild-value-carried-provenance-plan.md)
deferred its **P3** stage behind that lever: *"land trusted seeding, re-run this
census, and re-price P3 against the new numbers."*

Two things were wrong with that.

1. **The stage did not exist.** The only document owning a "Stage C" is
   [`completed/sdk-chain-exactness-plan.md`](completed/sdk-chain-exactness-plan.md),
   which is marked COMPLETE with C.2a, C.2b and C.2c all landed. Nothing
   scheduled the reserved-name seeding those comments pointed at, so P3's
   trigger condition could not fire — not because the measurement said wait,
   but because no work existed to move the measurement.
2. **The attribution behind it was false.** The census's decline column was
   described as "dominated by undefined *reserved* receivers". It is not: the
   reserved population is 13 of 330 expression declines and 42 of 2 619
   condition withdrawals, both upper bounds. The error was an artefact of
   reading a histogram bucketed by *function name* as though it were bucketed
   by *cause*.

## The measurement

`crates/msbuild/tests/sdk_chain_decline_attribution.rs`, against the pinned SDK
**10.0.301**, decomposing the census's own populations by why each item was
declined. Its committed counts are asserted equal to the census's (66 and 139),
so the two harnesses cannot drift apart and quietly measure different corpora.

**The classifier does not read issue tags**, because they do not carry the
distinction it needs. `$([System.IO.Path]::Combine($(Undefined), 'x'))` reports
`Unsupported` and nothing else, though `Combine` *is* modelled and the real
blocker is the operand. So each declined item is instead **re-evaluated with
every name it references defined**, under several fillers; if it commits then,
a richer property table would reach it.

| SDK-chain call expressions | 396 distinct |
| --- | ---: |
| committed | **66** |
| declined — **no property table can reach** | **193** |
| declined — operands missing | 137 |
|   …blocked purely by reserved names | **13** |
|   …reserved mixed with ordinary | 13 |
|   …no reserved name involved | **111** |

| SDK-chain conditions | 2 758 distinct |
| --- | ---: |
| committed | **139** |
| withdrawn — no property table can reach | 157 |
| withdrawn — operands missing | 2 462 |
|   …blocked purely by reserved names | **42** |
|   …reserved mixed with ordinary | 65 |
|   …no reserved name involved | **2 355** |

**Reserved-name seeding can reach at most 13 of 330 expression declines and 42
of 2 619 condition withdrawals — 3.9% and 1.6%.** Both are *upper* bounds twice
over: the census seeds no reserved name at all, so those counts include names a
real walk already supplies (`MSBuildProjectDirectory`, `MSBuildToolsVersion`,
`MSBuildRuntimeType`); and the reachability probe counts an item as reachable if
*any* filler commits, which over-states what a real table would do. Even folding
in the mixed bucket — which seeding alone cannot unblock, since those items also
need ordinary names — the ceiling is 26 of 330 and 107 of 2 619.

What the declines actually are:

- **193 of the 330 expression declines reach no property table at all** —
  property functions outside the modelled subset. The census's function
  histogram names them: `Regex::Replace` ×37, `Path::Combine` ×35 (modelled, but
  many call sites nest an unmodelled inner call), `.Contains` ×24, `.Replace` ×23.
- **2 355 of the 2 619 condition withdrawals involve no reserved name** — they
  are blocked by ordinary SDK-computed names: `_TargetFrameworkVersionWithoutV`
  ×87, `OutputType` ×44, `Language` ×38, `TargetPlatformIdentifier` ×29,
  `BuildingInsideVisualStudio` ×25, `PublishSingleFile` ×25. No toolset seed
  supplies any of them.

### The census is context-free, and that inflates its undefined column

The census evaluates each expression and condition **in isolation** against a
fixed seed table. A real walk does not: by the time the SDK chain reaches a
condition on `$(OutputType)`, its own props have written it. Probed on a plain
`net10.0` SDK project, our walker defines `OutputType=Library`, `Language=F#`,
`TargetPlatformIdentifier=`, `_TargetFrameworkVersionWithoutV=10.0`,
`EnableDefaultItems=true` and `TargetExt=.dll` — six of the census's top
blockers, all resolved, none of them reserved. `_TargetFrameworkVersionWithoutV`
alone accounts for 87 of the withdrawals the census attributes to a missing
operand.

So the census's undefined-bearing column measures *how much of the SDK's text
depends on values established elsewhere in the SDK*, which is a fact about the
SDK, not a defect list. **It is the wrong instrument for pricing seeding work**,
and P3's trigger should never have been keyed to it.

## What a real walk actually leaves empty

Probed by expansion inside the project body — not by table membership, since the
published property table deliberately omits evaluator-computed seeds, so an
absent key there says nothing about whether `$(Name)` resolves. Ground truth is
`dotnet msbuild -getProperty:` (10.0.301) except where noted.

Of 26 reserved names, we agree with MSBuild on 16 (11 exactly, plus the 5 below
that the in-process oracle disputes and the real CLI does not — see the caveat).
The ten we leave empty split three ways, and **the split is the plan**:

### (a) Path-derivable — do these

| name | value on the probe | how |
| --- | --- | --- |
| `MSBuildThisFileFullPath` | `…/Demo.fsproj` | the file currently being walked |
| `MSBuildThisFileName` | `Demo` | its stem |
| `MSBuildThisFileExtension` | `.fsproj` | its extension |
| `MSBuildProjectDirectoryNoRoot` | `tmp/…/probe` | project directory minus the filesystem root |

Exact, no new inputs, and the same family as the eight `well_known` already
seeds — `MSBuildThisFile` and `MSBuildThisFileDirectory` are there, and these
three are their siblings, omitted for no recorded reason. The `ThisFile` family
must reframe per imported file, which the walker already does for the two it
has; extending it is mechanical.

**Sized:** small. One function, four names, and a differential case per name
against `-getProperty:` from *inside an imported file* as well as the body,
since that is where the reframing can be wrong.

### (b) Toolset-derivable — each needs its own probe, and its own reason

| name | value | risk |
| --- | --- | --- |
| `MSBuildVersion` | `18.6.4` | the MSBuild binary's version, not the SDK's |
| `MSBuildAssemblyVersion` | `18.0` | ditto, truncated |
| `VisualStudioVersion` | `18.0` | a toolset constant that tracks the above |
| `MSBuildProgramFiles32` | `/Applications` on macOS | MSBuild's own platform logic |
| `MSBuildNodeCount` | `1` | a *build* fact, and we do not build |

None is path-derivable; each would be a constant or a new probe of the SDK
layout, and each drifts with the SDK. Committing a wrong one is a wrong answer
of exactly the kind the crate exists to avoid, and the census says the whole
group is worth single-digit declines. **Do not do these** until something other
than the census asks for them — a real consumer reading the value, per the
"check for a consumer before paying for a trust verdict" rule.

### (c) Unknowable — and this is the interesting one

`MSBuildStartupDirectory` is the working directory of the MSBuild *process*. For
an LSP there is no such process: the value depends on where a build the user has
not run would have been launched from. It is not expensive to model, it is
**undefined for us**, permanently.

That matters beyond the one name, because it refutes the framing the census
comments used. "Trusted seeding turns the reserved names on wholesale" cannot be
true of a set with a permanently-unknowable member. The reserved set is not
uniformly seedable, so any plan phrased as *seed the reserved names* is
mis-shaped; the shape that survives is *seed the path-derivable ones, decline
the rest, and say which is which*.

## Caveat, recorded because it cost time: the oracle is not an oracle for this

The in-process `msbuild-condition-oracle` (`project` op, MSBuildLocator) reports
**its own** load location for `MSBuildToolsPath`, `MSBuildBinPath`,
`MSBuildSDKsPath`, `MSBuildExtensionsPath` and `MSBuildExtensionsPath32`. Under
the nix devshell that is the wrapper path
(`…-dotnet-wrapped-combined/…`), while our walker canonicalises through the
symlink to `…-dotnet-sdk-10.0.301/…`. Diffing against the oracle therefore shows
five *committed* values disagreeing with "MSBuild", which reads exactly like a
certain-implies-exact violation on a real SDK project.

It is not. `dotnet msbuild -getProperty:` — the real CLI — returns our value for
all five, trailing slash on `MSBuildExtensionsPath` included. The oracle's
answer is a fact about the oracle's host.

**So: toolset-location properties must be ground-truthed against the CLI, never
against the `project` op.** Any future differential that sweeps property names
broadly will hit this, and will read it as five wrong commits.

## What this changes elsewhere

1. **P3's trigger is void and needs re-deriving.** It was "the committed
   fraction rises, via trusted seeding". Seeding moves that fraction by a few
   per cent at most, so the trigger would never fire on its own terms. P3 should be re-priced
   against what its durability is actually worth, or against the *modelling*
   lever (the 286 unmodelled shapes), which is the thing that would genuinely
   grow the committed surface — and therefore genuinely grow P3's blast radius.
2. **The five comments are corrected**, and the attribution is now a checked
   two-sided number rather than a sentence — pinned over the *whole* partition,
   so work that merely moves items between buckets still forces a restatement.
   Improving the evaluator will fail `sdk_chain_decline_attribution.rs` and
   force the figures to be restated with a date and a direction, which is the
   discipline `parser_corpus`'s `CLEAN_PARSES` already applies and for the same
   reason.
3. **The real coverage worklist is the function list**, already printed by the
   census and now labelled as what it is: 193 of the 330 expression declines
   reach no property table at all, so modelling is the only lever that moves
   them.

## The recommendation

Do **(a)**, the four path-derivable names, as a small self-contained change.
Do not do **(b)**. Record **(c)** as permanently declined.

Then treat the property-function worklist as the coverage question it always
was, priced from the census's function histogram rather than from a claim about
reserved names — and re-derive P3's trigger from that, since it is the surface
whose growth actually changes P3's value.

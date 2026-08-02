# Reserved-name seeding: what it is actually worth

> **Status:** planned, and deliberately *not* started. The measurement below
> prices the work at **+4 call expressions and +30 conditions** on the SDK-chain
> census, against populations of 396 and 2 758, with a sound ceiling of 52 and
> 145 on everything reserved-name seeding could ever reach. What follows is the
> case for doing a small, well-bounded slice of it and cutting the rest loose
> from the plan it was blocking.

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
   described as "dominated by undefined *reserved* receivers". At most 52 of
   330 expression declines and 145 of 2 619 condition withdrawals read a
   reserved name at all, and the work would deliver +4 and +30. The error was an
   artefact of reading a histogram bucketed by *function name* as though it were
   bucketed by *cause*.

## The measurement

`crates/msbuild/tests/sdk_chain_decline_attribution.rs`, against the pinned SDK
**10.0.301**. It does not classify declines. It runs the census's own population
under three property tables and reports the steps between them:

| | census seeds | + what a real walk already seeds | + the names still left empty |
| --- | ---: | ---: | ---: |
| call expressions committed (of 396) | 66 | 76 | **80** |
| conditions committed (of 2 758) | 139 | 155 | **185** |

**The seeding work is the last step: +4 call expressions and +30 conditions.**
The middle step (+10 and +16) is not work at all — it is the census not seeding
reserved names on its own side, since MSBuild rejects `-p:` for them.

And that +4/+30 is an over-statement: the table it measures includes
`MSBuildStartupDirectory`, which cannot be known at all (see (c) below), and the
whole toolset group that (b) recommends against.

### The ceiling, which is the bound the conclusion actually needs

A measured delta is a **floor** — one seed table, one count. "This lever is
small" is an *upper*-bound claim, and no finite experiment establishes one. The
ceiling that does is structural: an item that reads no reserved name cannot be
turned on by any choice of reserved values, whatever they are.

**At most 52 of the 330 expression declines and 145 of the 2 619 condition
withdrawals read any reserved name at all.** That is the most seeding could ever
be worth, and it is checked rather than argued.

So the lever is bounded: **+4/+30 measured, ≤52/≤145 possible.**

### Why this is a delta and not a classification

Three versions of this measurement preceded it, and the first two were wrong the
same way: they inferred a *universal* ("no property table reaches this") from a
*finite* probe. Recorded because the shape recurs.

- **Reading `Issue` tags does not work.** `substitute` reports `Unsupported` and
  nothing else when a *modelled* function has an undefined operand, so
  `$([System.IO.Path]::Combine($(Undefined), 'x'))` is indistinguishable from a
  function never implemented — while the same call with the operand defined
  reduces cleanly.
- **Re-probing with fixed filler values does not work either.**
  `'$(X)' != '' and Exists('$(X)')` commits when `X` is defined **empty**, which
  a set of non-empty fillers never tries; `$(Rid.Split('-')[1])` commits only for
  a hyphen-bearing value.
- **A hand-written seed list understates the lever.** Review found two omissions
  (`MSBuildDisableFeaturesFromVersion`, seeded `999.999` in an empty
  environment; `MSBuildProjectDefaultTargets`). The harness now derives any
  reserved name the corpus reads that the hand lists miss and seeds it too, so
  the next omission cannot bias the figure.

### The census is context-free, which is why it looked otherwise

The census evaluates each expression and condition **in isolation**. A real walk
does not: by the time the SDK chain reaches a condition on `$(OutputType)`, its
own props have written it. Probed on a plain `net10.0` SDK project, our walker
defines `OutputType=Library`, `Language=F#`, `TargetPlatformIdentifier=`,
`_TargetFrameworkVersionWithoutV=10.0`, `EnableDefaultItems=true` and
`TargetExt=.dll` — six of the census's top blockers, all resolved, none of them
reserved.

So the census's undefined column measures *how much of the SDK's text depends on
values established elsewhere in the SDK*, which is a fact about the SDK rather
than a defect list.

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
   fraction rises, via trusted seeding". The work moves it by +4 and +30, and
   could not move it by more than +52 and +145, so the trigger would never fire
   on its own terms. P3 should be re-priced
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
   census and now labelled as what it is. What this document establishes is only
   that *seeding* is not the lever; which of the remaining declines are
   modelling work and which are richer-table work is not measured here, and the
   two failed attempts above are why no number for that split is quoted.

## The recommendation

Do **(a)**, the four path-derivable names, as a small self-contained change.
Do not do **(b)**. Record **(c)** as permanently declined.

Then treat the property-function worklist as the coverage question it always
was, priced from the census's function histogram rather than from a claim about
reserved names — and re-derive P3's trigger from that, since it is the surface
whose growth actually changes P3's value.

# Continuous measurements

`.github/workflows/stats.yml` runs the expensive parser, name-resolution,
find-references, scoping-census, overload-coverage and whole-project corpus
reports after every push to `main` (and on manual dispatch from `main`). It is
an observational workflow, not a merge gate.

Five of them sweep files in isolation against a pinned F# compiler source tree:
`parser-divergence`, `resolution-divergence`, `find-references-differential`,
`uses-census` and `types-census`. `project-corpus-diff` loads real restored
projects through the LSP's own runtime chain and diffs them against FCS.
`overload-coverage` walks neither — its matrix is generated from this
repository. See [Three corpora] below for why the three are set up differently.

[Three corpora]: #three-corpora

Every measurement here has a **gate counterpart** that asserts on the same
machinery in `ci.yml`, before the commit lands; see [Gates and measurements].

[Gates and measurements]: #gates-and-measurements

The workflow has four distinct products:

1. Each report directory is uploaded as a 90-day Actions artifact. These are
   the detailed, reproducible worklists used for investigation.
2. Each generator writes a compact `summary.json`. The workflow wraps it with
   the Borzoi commit, F# corpus revision, `flake.lock` hash, measurement time,
   and workflow identity and creation order.
3. The wrapped observation is committed to the orphan `stats-data` branch at
   `observations/<measurement>/<series>/<borzoi-sha>.json`. The current branch
   tree contains the complete durable dataset; Git history is an additional
   audit trail, not the only place old observations survive.
4. `borzoi-stats site` validates every observation and builds the disposable
   GitHub Pages dashboard. Pages contains no authoritative state and can be
   rebuilt entirely from `stats-data`.

The workflow bootstraps `stats-data` on its first successful run. Configure
GitHub Pages to use **GitHub Actions** as its source, and protect `stats-data`
against deletion and force-push once it exists.

The project-corpus measurement runs in its own job because it reaches
nuget.org and several external repositories, so it can fail for reasons that
say nothing about Borzoi. When it does, `persist` still publishes the three
measurements that succeeded rather than losing the commit's whole observation.

## Gates and measurements

A sweep can *assert* or it can *record*, and the two want opposite failure
behaviour. An assertion belongs where a red run stops something: on the pull
request, in `ci.yml`. A measurement must not fail on what it found, because
that would withhold the observation in exactly the run that found something —
the argument [Measurement, not gate] makes for the project corpus, applied
everywhere.

Both halves exist for every axis, and the split is:

| axis | gate (`ci.yml`) | measurement (`stats.yml`) |
|---|---|---|
| parser | `parser_corpus`, `parser_corpus_diff` | `parser-divergence` |
| in-file resolution | `resolve_corpus_diff` | `resolution-divergence` |
| attributes | `attr_resolution_sweep` | — (rides in the resolution series) |
| find references | the crate's own suite | `find-references-differential` |
| whole project | `corpus-diff` job | `project-corpus-diff` |
| assembly projection | `projection_skip_sweep` | — |
| LSP robustness | `parser_corpus_sweep` | — |
| NuGet parsers | `soak`, `randomised_soundness_soak` | — |
| scoping | — | `uses-census`, `types-census` |
| overload engine | `overload_corpus_diff` | `overload-coverage` |
| MSBuild on real projects | `fsproj_msbuild_corpus_diff` | — |

`fsproj_msbuild_corpus_diff` gates at the strict defaults —
`BORZOI_MSBUILD_MAX_DIVERGENCES=0`, `BORZOI_MSBUILD_MAX_ERRORS=0` — over a
six-project sample of the pinned corpus, currently
`compared_projects=6 matched_facets=31 skipped_facets=11 divergences=0
error_projects=0`. Evaluation is pre-execution, so it needs no restore and
costs seconds; it rides in `test-msbuild` under the same change filter as the
crate.

It was the last row to be wired, and the reason is worth keeping. A ceiling set
to the divergence count you happen to have ratifies a known-wrong value as the
expected state, which is how a differential stops being one — so the row stayed
*unwired and named* while two known divergences stood, rather than being
green-lit at `MAX_DIVERGENCES=2` or quietly omitted. An unwired sweep with no
entry is indistinguishable from one nobody thought of.

Both of those divergences were the same species, and it is the species this
sweep exists for. Every other MSBuild differential in the crate is
**generative**: it authors a document and diffs it. Such a harness can only find
defects in shapes someone thought to generate, and it evaluates both sides under
the same property table. Neither of these was reachable that way:

- `DefineConstants = …;NETSTANDARD;FX_NO_WINFORMS` on
  `FSharp.ProjectSystem.FSharp.fsproj` ([PR #260]) — `FSharp.Profiles.props`
  gates those symbols on a `<Choose>` testing
  `'$(TargetFrameworkIdentifier)' == '.NETFramework'`, a property the common
  targets *derive*. We read it as empty and trusted, so the `Otherwise` arm won
  and a wrong value was **committed**. This is the one that mattered, and not
  because `net472` is a target worth serving (it is not — see AGENTS.md):
  `define_constants` is *consumed*, deciding which `#if` branch the whole
  semantic layer sees.
- `Link = NullHelpers.fs` on `FSharp.Build.fsproj` ([PR #263]) — a `Compile`
  item whose `Include` escapes the project cone (`..\Compiler\Utilities\`).
  The `.fsproj` carries no `<Link>`; the SDK's
  `Microsoft.NET.Sdk.DefaultItems.targets` synthesises one at evaluation time
  through a metadata-bearing `<Compile Update="@(Compile)">`, which this
  evaluator does not execute. Fixed by *declining* rather than modelling: an
  out-of-cone item's `Link` is now `ItemMetadataValue::Unknown`, so the facet
  compares kind, path and order and makes no claim about a value it did not
  compute. `tests/fsproj_link_metadata_diff.rs` sweeps the axes and found 36
  wrong commits of that shape, against the corpus's one.

The pattern in both: a real project read something nobody wrote down. That is
what an un-chosen input buys, and why this row is worth a gate rather than a
series.

[PR #260]: https://github.com/Smaug123/borzoi/pull/260
[PR #263]: https://github.com/Smaug123/borzoi/pull/263

### The rest, and why they are not here

Three more `#[ignore]`d sweeps exist and are deliberately left off both
workflows. Recording the reason is the point — "nobody got to it" and "it does
not belong" look identical from a workflow file.

- `uses_census_project` (the isolation-bias probe) **cannot move in response to
  a Borzoi commit**. It runs the corpus one file at a time and then as one
  project, and both passes are FCS; the delta calibrates how much the two
  censuses above understate, but nothing we write changes it. A series that only
  moves when the corpus pin does is a constant with extra steps. Run it by hand
  when a pin changes.
- `lexfilter_corpus` is a divergence *histogram*, not an assertion, and its FCS
  side costs ~1.65 s per file — hours over the corpus. It points at the next
  LexFilter arm worth porting, which is a question you ask deliberately.
- The per-area report generators (`tier_order_diff`, `companion_head_diff`,
  `classify_diff`, `classify_assembly_diff`, `extension_shadow_diff`,
  `fsharp_member_diff`, `member_hiding_diff`) are worklists whose value is the
  text they print. There is no number in them to trend, and a worklist that
  regenerates itself unread is just a slower way to store the corpus.

The parser and resolution rows are the ones worth understanding, because they
were the gap this table closed. `stats.yml` swept both corpora and published
the divergence buckets from the start, but the *ratcheted* halves — the lossless
round-trip invariant, the AST-match floors, `MAX_RESOLUTION_DIVERGENCES = 0` —
ran nowhere at all. A series that only records is a number nobody has a reason
to establish the meaning of; the same lesson [Measurement, not gate] draws from
#200–#204, reached by a different route.

What that cost is measurable. `MIN_CLEAN_PARSES` had been measured once, in June
2026, and sat 2,401 files below the truth by the time anything ran it again — a
regression could have un-parsed a third of the corpus and still passed. The
resolution ratchets, which *were* maintained (other work re-ran them by hand),
were current to within a few hundred. That is the difference a gate makes, and
it is why the ratchets are now pinned to their exact measured values rather than
left with slack nobody re-measures.

Two things follow for anyone adding a sweep:

- **A gate that never runs is not a gate**, and a stale floor is worse than no
  floor: it reads as a check while asserting something the code passed years
  ago. If you write a ratchet, wire it into `ci.yml` in the same commit.
- **A sweep that skips itself must not report green.** `parser_corpus_sweep`
  read a sibling `../fsharp` checkout that no runner has, so it would have
  returned success having swept nothing. It reads `BORZOI_CORPUS` first for
  exactly that reason. The generator contract's *"every key, every run, always a
  number"* rule is the same principle one layer up.

The NuGet soaks are the odd row: they gate, but they are not ratchets. Their
seed is the wall clock, so each run explores somewhere the committed fixed-seed
corpora never reach — running them per commit is the whole point, and the fixed
corpora remain the reviewable field-by-field pin. The flip side is that a
failure there need not be caused by the diff in front of you; it can be a latent
bug that run happened to reach first. Reproduce with the printed seed
(`BORZOI_NUGET_SOAK_SEED`) before concluding anything about the change.

[Measurement, not gate]: #measurement-not-gate

## Generator contract

A measurement generator writes this shape:

```json
{
  "schema_version": 1,
  "measurement": "parser-divergence",
  "configuration": {
    "corpus": "fsharp-src"
  },
  "statistics": {
    "matches": 123,
    "divergences": 4
  }
}
```

`measurement` is a lowercase kebab-case path segment. `configuration` and
`statistics` are JSON objects. `statistics` must contain at least one number
and cannot contain arrays; use nested objects for structured metric families.
An optional `retired_statistics` array names metrics the generator deliberately
stopped emitting — see [Retiring a metric] below.
The series identity is a deterministic digest of the generator schema,
measurement name, pinned corpus revision, `flake.lock` hash, and complete
configuration. Changing the corpus, toolchain inputs, stride, scope, defines,
or another configuration field therefore starts a new comparable series rather
than silently joining unlike points.

[Retiring a metric]: #retiring-a-metric

Nested numeric statistics are discovered automatically by the dashboard. A
future typed-AST census only needs to emit this contract and add its report
command to the workflow; the history and site code do not need a measurement-
specific branch.

`statistics` is a *metric namespace*: the dashboard offers one plottable metric
per nested number, named by its path. So a key has to mean the same thing in
every run of a series. Arrays are rejected outright, and open-ended string keys
should be treated as if they were — a map keyed by, say, a skip reason that
embeds a path or an oracle error mints a fresh metric per run and none of them
are comparable. Keep those in the report artifact and put closed enumerations
(asset status, error kind) in `statistics`.

**Every key, every run, always a number.** A metric is not merely a number; it
is a number that is *always there*. The dashboard plots one metric per nested
*number*, so it ignores a `null` exactly as it ignores an absent key: either
way the observation is skipped and the previous point still reads as "Latest",
which means a run that measured nothing masquerades as the last run that
measured something — the precise failure this workflow exists to catch. Two
ways to breach it, and they are the same bug:

- *A sparse map.* A closed enumeration emitted only for the variants that
  occurred is an open one. Iterate the variants and emit zeros; never
  `counts.iter().map(…).collect()` over the observed ones.
- *A nullable field.* An `Option` ratio serialises as `null` the moment its
  denominator is empty. Emit a defined value instead — `0` is unambiguous
  because the denominator is emitted beside it, so "0 of 0" stays
  distinguishable from "0 of many".

Nothing in the recorder can enforce this *within* a run: a summary with a
missing key is indistinguishable from a measurement that genuinely has fewer
metrics, so the generator has to be exhaustive by construction.
`borzoi-corpus-diff` guards it
with `no_statistic_is_ever_null_however_empty_the_run`, which walks the whole
rendered tree on a deliberately degenerate run rather than naming the fields it
knows about — the field nobody thought to name is exactly the one that breaks.

The resolution generator's two histograms are the worked example. `matches`
is counted per bucket and `gap_b1` per `classify` sub-tag, and both are seeded
with their whole closed key set so a bucket or sub-tag that stops occurring
reads as `0`. Counting only what occurred would report the *closing* of a gap —
the outcome the whole sweep is for — as a metric going missing. The sub-tag list
is not hand-checked against the taxonomy beside it:
`b1_tags_are_exactly_the_tags_classify_pairs_with_b1` enumerates `classify` over
all 8,192 combinations of the inputs it reads and asserts the list is exactly
what comes back, so it can be neither short nor long.

*Across* runs it can, and does. Two consecutive observations of one series
measure the same thing over the same corpus with the same toolchain, so a
metric present in one and absent from the next is a change in what is
measured — and only the generator knows whether it meant it. `record` therefore
compares each observation against the one it will follow on the dashboard and
refuses to publish one that drops a metric its predecessor carried, unless it
says so. It reads the statistics exactly as the dashboard does, one metric per
nested *number*, so a field that starts serialising as `null` counts as dropped
for the same reason the dashboard would stop plotting it.

A rerun is checked against the observation it **replaces** as well, and that
check is the strict one: exact equality of the metric namespace, in both
directions, ignoring `retired_statistics` entirely. Two attempts of one run
measure the same commit with the same generator over the same corpus, so their
namespaces must agree by construction — a metric appearing only on the second
attempt is a namespace depending on something other than the code just as surely
as one that disappears, and a retirement declared here would be a false claim,
since nothing changed to retire. It is the one place the within-run rule above
can be *checked* rather than merely required of the generator. It also matters
because re-recording deletes the old file outright, so a metric only it carried
would otherwise leave no trace anywhere.

The comparison is otherwise against the **predecessor**, not the newest recorded
observation, because runs finish out of order and observations are ordered by
workflow creation. An older run landing late has a smaller metric namespace
because a *later* commit widened it, which is not a drop; comparing against the
newest would refuse it for one.

What the check therefore claims is narrow, and worth stating exactly: an
observation is compared against the greatest **already recorded** observation
below it. A drop escapes whenever the observations carrying the metric have not
been recorded yet, and once the first post-drop observation escapes so does
every later one, because each is then compared against an already-gapped
predecessor. That is not confined to the start of a series — a sufficiently
reordered arrival escapes at any depth — though in practice a live series has
its whole prefix long since published, so a new commit's run always has a
carrying predecessor to be measured against. The check firing is a claim; its
not firing is not.

Closing that gap by validating the **successor** too is a trap, and the reason
is worth recording. The observation such a check would refuse is the innocent
one: in the mirror case — a metric added and removed across two commits, the
adding run landing late — the late run emitted a strict superset of both its
neighbours, and the drop it would be refused for belongs to an observation that
is already published and immutable. Nothing anyone could change would discharge
the refusal, so a correct observation would be permanently unpublishable and its
run permanently red. The gap is accepted instead, and
`a_drop_escapes_when_the_runs_carrying_it_are_recorded_after_it` pins it so it
is not later "fixed" into that trap.

The boundary is established by enumeration rather than by the argument above,
because arrival order is precisely what replaying the real history cannot vary —
it only ever exercises the order that happened.
`every_arrival_order_that_records_a_carrier_first_catches_the_drop` runs all 24
orders of a four-observation series and asserts the rule directly: every order
that records a carrying observation before the first dropping one is refused,
and the eight that escape all begin with a dropping observation. That count is
pinned, not as a target but as the size of the accepted gap — a change that
moves it in either direction has changed what the recorder claims.

Accepting it costs nothing a reader can see, because retirement is rendered from
presence rather than from the declaration: a metric absent from the newest
observation is labelled retired whether or not anyone said so. What escapes is
the record of intent, and the chance to catch a generator regression at the
moment it happened.

### Retiring a metric

A metric is retired by naming it in the observation's `retired_statistics`:

```json
{
  "schema_version": 1,
  "measurement": "project-corpus-diff",
  "configuration": { "selection": { "source": "corpus" } },
  "statistics": { "divergences": 0, "deferrals": { "total": 41 } },
  "retired_statistics": ["decline_census.project.by_cause.occupied"]
}
```

`statistics` must still contain at least one number, so the last metric of a
measurement cannot be retired. That is deliberate: a measurement with nothing
left to measure should be removed from the workflow, not published as an
observation of nothing.

Each entry is a dotted metric path in the spelling the dashboard names it by,
and it must **not** also appear in `statistics` — a retirement says the metric
is gone, and one that is still measured would licence dropping it later without
notice.

**Leave the declaration in place once written.** It is only *consulted* at the
transition, since by the next run the predecessor already lacks the key — but
the run that publishes the transition can fail, and this job fails for reasons
that say nothing about Borzoi. If that happens, the next observation's
predecessor is still the one from before the retirement, and a generator that
dropped the marker on the strength of "needed once" is refused, as is every
observation after it, until someone puts the marker back. Keeping it costs a
line, and a stale entry cannot quietly license anything: a name that is retired
*and* emitted is refused outright, so a metric that comes back forces the marker
to be removed deliberately.

The declaration is deliberately not
part of the series digest, and that is the whole reason it exists rather than a
schema bump: retiring one metric must not restart the trend of the metrics
beside it, which are still measuring exactly what they measured before. The
`schema_version` is global to every generator, so bumping it would restart the
parser, resolution and find-references series too; and `configuration` is
all-or-nothing, so splitting there would discard the divergence and deferral
history that the PR gate and the tier-reorder plan both read.

Because the workflow runs on push to `main`, forgetting the declaration fails
*after* the merge: the record step exits non-zero, so that commit's observation
— along with any the same step would have recorded after it — is not published,
and the run goes red. So add the declaration in the same commit that removes the
key. A lost point in a forty-point series costs little; a metric that silently
freezes at a stale value costs the thing this workflow exists for.

Renaming is retirement plus introduction, and the halves are not symmetrical.
The new key starts mid-series, which the dashboard shows honestly — the chart
begins where the metric does. The old key is the half that needs saying out
loud.

The dashboard labels a metric the newest observation of the selected series
does not carry as `(retired)`, and its "Latest" card reads "Last measured".
That covers the retirements predating this check as well as the declared ones:
liveness is read off the observations themselves, so it needs no declaration to
be right. The label and the check are two halves of one fact — the label makes
an absence legible, the check makes it deliberate.

## Three corpora

A corpus is identified by `corpus.source` (an `OWNER/NAME` label) and
`corpus.revision` (40 hex characters). The revision is what enters the series
digest; the source does not, because a measurement walks exactly one corpus and
the measurement name is already digested.

| | `dotnet/fsharp` | `<repo>-project-corpus` | `<repo>-overload-matrix` |
|---|---|---|---|
| measurements | parser, resolution, find-references, uses census, types census | `project-corpus-diff` | `overload-coverage` |
| what it is | one pinned F# compiler tree | several pinned real projects | a matrix this repo generates |
| pinned by | the `fsharp-src` flake input | `nix/project-corpus.json` | nothing — it is generated |
| revision | that input's locked commit | a digest over every pin | a digest over the generated sources |
| needs restoring | no | yes, from nuget.org | no |

The third is the odd one, and the reason is worth stating: its corpus is not
checked out, it is *emitted* by `crates/sema/tests/all/common/overload_corpus.rs`
at run time. Neither of the other two revisions identifies it — the F# tree's
commit would restart the series every time `fsharp-src` was bumped, for a
measurement that never reads that tree, and would hold it fixed across an edit
to the matrix itself, which is exactly backwards. So the generator hashes the
two sources it renders and publishes the digest in `configuration`, and the
workflow passes that as the observation's corpus revision. Editing the matrix
starts a new series; editing anything else does not.

The digest is length-prefixed across the two sources, so moving a declaration
from the C# side to the F# side cannot leave it unchanged —
`the_matrix_digest_is_a_40_hex_identity_of_the_generated_sources` pins that,
along with determinism, which a seeded generator would break and which every
rerun depends on.

The project corpus is pinned as data rather than as flake inputs because only
this workflow consumes it — `nix develop` has no use for it, and vendoring five
unrelated projects' NuGet closures into `nix/deps.json` would conflate that
file's purpose (making Borzoi's *own* builds offline) with this one's.
`borzoi-stats corpus --pins nix/project-corpus.json --emit digest|plan`
validates the file and derives both the corpus revision and the workflow's
checkout plan, so a malformed pin fails in-process rather than several minutes
into a clone.

### Changing the project corpus

Bumping a revision, adding a project, or dropping one changes the digest and so
**starts a new series**. That is the intent — the old points measured a
different body of code — but it does mean the dashboard's trend restarts, so
bump deliberately rather than routinely.

A candidate project must restore under the pinned SDK and be worth measuring:
multi-file, with imported-assembly uses.

**Two pins that name one project must be caught.** If they are not, the second
gets its own checkout and is measured twice, doubling every count it
contributes — while the job's comparable-count assertion still passes, because
both pins *did* become comparable. The corpus is therefore compared by
**identity**, never by text, in both `validate_project_corpus` and the digest:

- The repository identity is its ASCII case-folding, because GitHub resolves
  owner and repository names case-insensitively. Folding is complete rather
  than a guess: a repository component may only contain ASCII alphanumerics,
  `-`, `_` and `.`, so there is no further case equivalence to discover.
- The project path is deliberately **not** folded. It names a file on the
  runner's case-sensitive filesystem, where `A/B.fsproj` and `A/b.fsproj`
  really are different files.

Spellings that cannot be canonicalised are refused outright, so the file and
the digest agree by construction: `.`/`..` components, a `.git` suffix
(`owner/repo` and `owner/repo.git` are one repository), the path-list
separators `:` and `;` that the workflow uses to hand the list to the runner,
and uppercase revisions (Git resolves an uppercase object ID to the same
commit, so two casings would check out identical code under two digests and
split one corpus across two series).

The distinction matters for what happens when a pin is *re-spelled*: an
identity-keyed digest leaves the series intact, where a text-keyed one would
restart the trend for a change that points at exactly the same code.

**Take the repository and revision from the project's own remote**, not from a
name that looks right. Forks abound, and a fork's default branch can sit years
behind the canonical one; the pin will clone and check out perfectly while
measuring code nobody has touched since. If you are working from a local
checkout, read `git remote get-url origin` and `git rev-parse HEAD` there
rather than guessing the owner.

Then rehearse it, because the workflow's own failure is the slow way to find
out:

```sh
git clone --no-checkout https://github.com/OWNER/REPO /tmp/candidate
git -C /tmp/candidate checkout --detach REVISION
nix develop -c env RestoreSources=https://api.nuget.org/v3/index.json \
  NUGET_PACKAGES=/tmp/candidate-packages \
  dotnet restore /tmp/candidate/PATH/TO.fsproj
BORZOI_PROJECT_LIST=/tmp/candidate/PATH/TO.fsproj \
  nix develop -c cargo run -p borzoi-corpus-diff
```

Not every revision survives this, and an old one is likelier not to: a project
that pins `FSharp.Core` with `Include` while the SDK also adds it implicitly
fails restore outright under the .NET 10 SDK (`NU1504`, duplicate
`PackageReference`). That is a property of the revision rather than of Borzoi,
and it is exactly the kind of thing a stale fork pin walks into.

### Measurement, not gate

The runner's own ratchets (`BORZOI_PROJECT_EXPECT_DIVERGENCES` and friends) are
a gate, and they gate in `ci.yml`'s `corpus-diff` job, on the pull request,
before the commit lands. This job is a measurement. It therefore does not fail
on the runner's exit code, because doing so would withhold the observation in
exactly the run that found something. Divergence counts ride in `statistics` and
the series carries the finding.

The split matters, and it is not belt-and-braces. Between #200 and #204 the
series alone carried the divergence counts: they went from 3 to 33 at a single
commit while *coverage rose*. Nothing was ever asked to act on that, and for
eight commits nobody did.

What the 30 turned out to be is the better argument for the gate rather than a
weaker one. They were **not** wrong targets: `corpus-diff` adjudicates by
comparing rendered full names, FCS writes a member's enclosing generic type with
its type *arguments* and our entity model carries only its *parameters*, so a
correct resolution scored as a divergence. #204 made generic types reachable
from a value-path head for the first time and walked straight into that blind
spot. A gate would not have known which of those it was — it would have stopped
the run and made someone find out, eight commits earlier, which is the entire
job. A number that only ever gets recorded is a number whose meaning nobody has
a reason to establish.

Both jobs materialise the corpus through `tools/ci/project-corpus.sh`, so they
cannot come to measure different things.

What must still fail loudly is a run that did not *measure* — an unrestorable
project, a wedged oracle. The generator summary is written before the ratchets
are checked, so the job asserts against its contents instead: every pinned
project must have become comparable.

### Reading the project series

The gate on this runner already allows zero divergences, so the divergence
series should read zero and is only interesting when it does not. **Deferrals
are the series to watch.** A deferral is a use we resolved to nothing concrete
where FCS resolved to something: not a wrong answer, so nothing fails, which is
precisely why a rise in them is otherwise invisible. Watch
`deferrals.total` against `uses.total_considered` and `projects.comparable` —
a deferral count only means something against the population it was drawn from,
and all three travel together in every observation.

`decline_census` says what those deferrals were *to*, on the same
project/assembly axis the totals already have — a merged census could not
explain either bucket, and a swap between them would move nothing it reports.
Within each: `by_cause` names the guard that declined, `by_tier` the position in
the referenced-assembly precedence ladder it spoke from, and `by_pair` the two
together. The pairs are not
redundant: if a ladder change moves equal numbers of two causes between two
tiers, every decline in the corpus changed and both marginals read identically,
so the marginals alone cannot see the one thing the census is for. The totals move whenever the resolver gets more or less
timid; the census says which model owns the move, which is the question every
change to that ladder asks and which no aggregate can answer. Both maps carry
every variant including the zeros — a cause that stops *occurring* must read as
`0`, not vanish, because a zero and an absence say opposite things and only one
of them is a measurement. A cause that stops *existing* is a different event and
has to be declared: see [Retiring a metric]. Narrowing either map retires every
pair it removes, permanently — which is why `by_pair` publishes the whole
product rather than each cause's reachable tiers. That narrowing is not
available at all: a tier is fixed where the decline *site* is built, not where
the cause is named, so which tiers a cause can speak from is not a function of
the cause. `DeclineCensus::pairs` carries the three shapes that defeat it.

`decline_census.unattributed` is the census's own honesty check, and it is a
count rather than a residual. A decline site is a claim and its absence is not:
many deferrals have no causing guard at all — a dotted path's tail segments
defer because member resolution is a later phase — so the census attributes
what the ladder and its pre-walk gates do and reports the rest as unattributed.
Read `unattributed` against `attributed`; a *rise* in the ratio means a new
decline path appeared that no guard accounts for, which is worth looking at even
though nothing failed.

`uses.attribute_commits_compared` is a **coverage** number, not a quality one:
how many of the compared uses were answered out of name resolution's attribute
commit map rather than its main one. It is published because the failure it
guards against is silent in every other number here. Attribute types are
recorded separately (they answer FCS's suffix-first candidate walk) and are
served to users like any other name, so a comparison that reads only the main
map sees an attribute answer as *silence* — and silence is what this runner
banks as a deferral, which claims nothing. Every headline would hold: the
divergence gate stays at zero, and the deferral count merely rises, which is
exactly the shape of ordinary timidity. So a fall in this number towards zero on
a corpus that still contains attributes means a committed surface stopped being
diffed, and nothing else would say so.

`uses.member_commits_compared` is the same kind of number for the third surface
the LSP answers from: the member table **inference** fills in
(`InferredFile::member_resolutions`), which `handlers/definition.rs` layers over
the resolver's `Deferred(QualifiedAccess)` at a member name. An entry there is a
go-to-definition target, and read through the resolver alone the site still
looks deferred, so a wrong member answer could stand forever without moving a
number. It reads **0** on the pinned corpus today: every member answer inference
commits there is one the resolver already committed itself, so nothing is
answered by inference alone. That is the honest state of the surface, not a
fault — the guard is in place for when inference starts answering where the
resolver cannot, and the fixtures in `project_resolution.rs` are what exercise
the grading meanwhile. The two sides key one answer at different spans (FCS
reports the whole access, inference the member name), so the comparison aligns
on the span's **end**; keying on whole ranges compares nothing while reporting a
clean run.

Two skip buckets travel with the attribute number, because asking the oracle
about a range for the first time exposed that it can answer more than once.
`skipped_uses.shadowed_constructor_use` counts the sites where FCS reported both
the name the author wrote and the **constructor** that name invokes — `[<Alias>]`,
`inherit Base(1)`, `Foo()`. Sema resolves the written name and models no separate
resolution for the constructor, so the name's record grades the site and the
constructor's steps aside. This is a coverage-preserving reading, not a decline:
the site is still compared, just against the record that answers the question
sema was asked. Grading against the constructor instead would pass only when its
declaration range happened to coincide with its type's, and in the reverse
direction would *ratify* a resolution to the constructed type at a site whose
written name is something else.

`skipped_uses.ambiguous_oracle_range` is what survives that: a range where two
records still disagree about the declaration after the constructor has stepped
aside, so the oracle genuinely does not say what the site resolves to. It is
decided on the oracle's answers alone, never on whether ours agrees, so it cannot
become the bucket a real disagreement escapes into. Watch it against
`attribute_commits_compared`: a rise here alongside a fall there is coverage
draining into "unadjudicable", which is the honest bucket but not a free one.

## Local validation

```sh
nix develop -c cargo test -p borzoi-stats
nix run nixpkgs#actionlint -- .github/workflows/stats.yml
```

`borzoi-stats`' test suite parses the checked-in `nix/project-corpus.json`, so
a malformed pin is a test failure rather than a workflow failure.
`borzoi-corpus-diff`'s suite hands its generator summary to the real
`record_observation`, so the contract is checked by the validator that will
publish it rather than by a second copy of its rules.

The recorder is deliberately strict: malformed SHAs and timestamps, unknown
schema versions, unsafe measurement names, symlinks, and observation files whose
paths disagree with their contents all fail the publication. Concurrent main
runs write disjoint commit paths; the workflow bounds fetch/rebase/push retries
when `stats-data` advances during publication. Observations are ordered by
GitHub's per-workflow run number, so a slow older commit remains before a newer
commit even if its measurement completes later. Dashboard jobs hold a shared
Pages lock while fetching the current branch tip, rendering, and deploying, so
a late-arriving run cannot overwrite the dashboard with its older snapshot.

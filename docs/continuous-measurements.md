# Continuous measurements

`.github/workflows/stats.yml` runs the expensive parser, name-resolution,
find-references, and whole-project corpus reports after every push to `main`
(and on manual dispatch from `main`). It is an observational workflow, not a
merge gate.

The first three sweep files in isolation against a pinned F# compiler source
tree. The fourth, `project-corpus-diff`, loads real restored projects through
the LSP's own runtime chain and diffs them against FCS; see [Two corpora] below
for why it is set up differently from the other three.

[Two corpora]: #two-corpora

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
The series identity is a deterministic digest of the generator schema,
measurement name, pinned corpus revision, `flake.lock` hash, and complete
configuration. Changing the corpus, toolchain inputs, stride, scope, defines,
or another configuration field therefore starts a new comparable series rather
than silently joining unlike points.

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

Nothing in the recorder can enforce this: a summary with a missing key is
indistinguishable from a measurement that genuinely has fewer metrics, so the
generator has to be exhaustive by construction. `borzoi-corpus-diff` guards it
with `no_statistic_is_ever_null_however_empty_the_run`, which walks the whole
rendered tree on a deliberately degenerate run rather than naming the fields it
knows about — the field nobody thought to name is exactly the one that breaks.

## Two corpora

A corpus is identified by `corpus.source` (an `OWNER/NAME` label) and
`corpus.revision` (40 hex characters). The revision is what enters the series
digest; the source does not, because a measurement walks exactly one corpus and
the measurement name is already digested.

| | `dotnet/fsharp` | `<repo>-project-corpus` |
|---|---|---|
| measurements | parser, resolution, find-references | `project-corpus-diff` |
| what it is | one pinned F# compiler tree | several pinned real projects |
| pinned by | the `fsharp-src` flake input | `nix/project-corpus.json` |
| revision | that input's locked commit | a digest over every pin |
| needs restoring | no | yes, from nuget.org |

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

The runner's own ratchets (`BORZOI_PROJECT_MAX_DIVERGENCES` and friends,
defaulting to zero divergences) are a gate; this job is a measurement. It
therefore does not fail on the runner's exit code, because doing so would
withhold the observation in exactly the run that found something. Divergence
counts ride in `statistics` and the series carries the finding.

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

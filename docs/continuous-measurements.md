# Continuous measurements

`.github/workflows/stats.yml` runs the expensive parser and name-resolution
corpus reports after every push to `main` (and on manual dispatch from `main`).
It is an observational workflow, not a merge gate.

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

## Local validation

```sh
nix develop -c cargo test -p borzoi-stats
nix run nixpkgs#actionlint -- .github/workflows/stats.yml
```

The recorder is deliberately strict: malformed SHAs and timestamps, unknown
schema versions, unsafe measurement names, symlinks, and observation files whose
paths disagree with their contents all fail the publication. Concurrent main
runs write disjoint commit paths; the workflow bounds fetch/rebase/push retries
when `stats-data` advances during publication. Observations are ordered by
GitHub's per-workflow run number, so a slow older commit remains before a newer
commit even if its measurement completes later. Dashboard jobs hold a shared
Pages lock while fetching the current branch tip, rendering, and deploying, so
a late-arriving run cannot overwrite the dashboard with its older snapshot.

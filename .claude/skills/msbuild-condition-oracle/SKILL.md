---
name: msbuild-condition-oracle
description: How to ground-truth MSBuild Condition semantics against real `dotnet msbuild` with per-case stub projects, and where MSBuild's authoritative evaluator sources live. Use whenever changing crates/msbuild/src/condition.rs, or whenever a claim about MSBuild condition behaviour (yours, a reviewer's, or a plan's) needs verifying rather than trusting.
---

# Ground-truthing MSBuild condition semantics

Never reason your way to what MSBuild does with a `Condition` string — run it.
Wrong guesses here produce *wrong gates* (items silently included/excluded),
which is worse than `Outcome::Unsupported` (fail-safe exclusion plus a
diagnostic). Reviewer claims about MSBuild behaviour must be verified the same
way before acting on them.

## The oracle: one stub project per case

```xml
<Project>
  <PropertyGroup>
    <R>FALSE</R>
    <R Condition="ESCAPED_CONDITION">TRUE</R>
  </PropertyGroup>
</Project>
```

then

```sh
dotnet msbuild stub.proj -getProperty:R
```

prints `TRUE`, `FALSE`, or an `MSB4086`-style error for MSBuild-illegal
conditions (an error maps to our `Outcome::Unsupported`).

Practicalities:

- **One project file per case.** One illegal condition aborts the whole
  evaluation, so batching cases into one project loses every other answer.
- Each invocation costs ~1s; parallelise with `&` / `wait` in batches of ~8.
- XML-escape the condition: `<` → `&lt;`, `>` → `&gt;`; when generating files
  from `printf`, `&#39;` is the least painful spelling for single quotes.
- Properties can participate: define them in the same `<PropertyGroup>`
  above the conditioned write.
- Conditions on items work too: `-getItem:X` with
  `<X Include="hit" Condition="..."/>`.

Pin every verified behaviour as a unit test citing the oracle (see the
"Pinned against `dotnet msbuild`" tests in `crates/msbuild/src/condition.rs`)
so the fact survives the session that established it.

## Authoritative sources

The evaluator's semantics are small and readable in dotnet/msbuild:

- `src/Build/Evaluation/Conditionals/MultipleComparisonNode.cs` — `==`/`!=`
  dispatch: empty short-circuit → numeric (double) → MSBuild boolean
  (`'on' == 'yes'` is true) → ordinal-ignore-case string. **Never versions.**
- `src/Build/Evaluation/Conditionals/NumericComparisonExpressionNode.cs` and
  the per-operator nodes (`LessThanExpressionNode.cs`, …) — relational
  dispatch: both-double → both-`Version.TryParse` → mixed number/version
  (major-only, version wins ties) → else project error.
- `src/Build/Evaluation/Conditionals/Scanner.cs` — bare-token lexing
  (greedy numerics: hex, sign/dot-led, multi-dot).
- `src/Framework/Utilities/ConversionUtilities.cs` — numeric grammar
  (doubles without whitespace/exponent; `0x` hex as 32-bit signed
  reinterpretation) and the boolean vocabulary.

Fetch with:

```sh
gh api repos/dotnet/msbuild/contents/<path> --jq '.content' | base64 -d
```

Derive the model from source, then confirm each non-obvious consequence with
the oracle — both steps, not either alone (the source tells you the rule; the
oracle catches the .NET-runtime subtleties like `Version.TryParse` accepting
`'+2.5'` and whitespace-padded components).

## The permanent differential harness

The per-case stub-project trick above is for *interactive* verification of a
specific claim. For regression coverage there is now a permanent harness that
automates the whole class:

- **`tools/msbuild-condition-oracle`** — a long-lived JSONL batch child in the
  `tools/fcs-dump` / `tools/nuget-oracle` mould, but evaluating conditions
  *in-process* via the MSBuild API (`Microsoft.Build.Evaluation.Project`,
  loaded against the SDK's real MSBuild through `Microsoft.Build.Locator`).
  Same stub-project idea, ~7000 evals/sec instead of one process spawn per
  case. One op: `{"op":"eval","condition":s,"properties":{…}}` →
  `{"ok":true,"value":bool}` (MSBuild evaluated it) | `{"ok":false}` (MSBuild
  rejects it as illegal). Smoke-test it directly:
  ```sh
  printf '%s\n' '{"op":"eval","condition":"6.0.0.0 > 6"}' \
    | nix develop -c tools/msbuild-condition-oracle/bin/Release/net10.0/msbuild-condition-oracle
  ```
- **`crates/msbuild/tests/condition_diff.rs`** — hand-picked corners plus a
  fixed-seed generative sweep, and **`condition_properties.rs`** — a proptest
  that shrinks divergences to a minimal witness. Both assert
  *certain-implies-exact*: whenever our evaluator commits to `True`/`False`,
  MSBuild must agree with that exact boolean; `Unsupported` makes no claim
  (the fail-safe superset over both MSBuild-illegal and legal-but-unmodelled).
  The evaluator is reached through the `test-support`-feature `test_support`
  module. Run them with:
  ```sh
  nix develop -c cargo test -p borzoi-msbuild --test condition_diff
  nix develop -c cargo test -p borzoi-msbuild --test condition_properties
  ```

When you pin a new behaviour, prefer adding it to `condition_diff.rs`'s corner
list (it round-trips through the real evaluator on every run) over a bare
`assert_eq!` unit test that can only ever re-assert what you believed at the
time. The stub-project trick remains the right tool for a one-off "what does
MSBuild do here?" during development.

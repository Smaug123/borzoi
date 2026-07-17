# Property-expression parser plan (the `$( … )` sub-language)

> **Status:** complete (Stages 1–3, PRs #857, #862, #868). A general parser for
> MSBuild's `$(…)` expression sub-language — property references, static
> property functions (`$([Type]::Member(args…))`), instance member chains
> (methods, paren-less properties, indexers), and nested `$(…)` inside
> string-literal arguments — with an **allowlisted, individually pinned**
> evaluator. Parsing commits to no values, so the never-over-resolve invariant
> lives entirely in the evaluator's dispatch table: a miss anywhere in a chain
> aborts to `Issue::Unsupported` (expression left literal, project marked
> partial). The parser + typed values + dispatch live in
> [`crates/msbuild/src/properties/expr.rs`](../../crates/msbuild/src/properties/expr.rs);
> `substitute`/`substitute_with_fs` kept their signatures and `Issue` contract.
> The motivating consumer was the F# SDK's `Microsoft.FSharp.Core.NetSdk.props`
> shape
> `$([System.Version]::Parse('$(FSCorePackageVersion.Split('-')[0])').Major)`.
> The §D2 ground-truth block below is retained reference, cited by prose from
> the tests (`property_expr_diff.rs`) and the evaluator.

## Landed stages (one line each)

- **Stage 1** (PR #857) — oracle `expand` op + `crates/msbuild/tests/property_expr_diff.rs`
  differential harness (certain-implies-exact against today's `substitute`),
  with the §D2 rows unit-pinned so the facts survive offline.
- **Stage 2** (PR #862) — the quote-aware tokenizer, `Expr`/`Link`/`Arg` AST,
  typed `Value` model, and dispatch table; every already-pinned function
  (`GetTargetFramework*`, `GetDirectoryNameOfFileAbove`, `NormalizePath`,
  `Combine`, `IsRunningFromVisualStudio`, `TrimStart`,
  `Contains`/`StartsWith`/`EndsWith`) migrated onto dispatch; dotted property
  names (`$(A.B)`) now parse as member access rather than `Undefined{"A.B"}`.
- **Stage 3** (PR #868) — the new pinned evaluators (string `.Split(char-set)`,
  array/string indexers, `.Length`, `.ToString()`, `[System.Version]::Parse` +
  `.Major`/`.Minor`/`.Build`, `[MSBuild]::EnsureTrailingSlash`) plus the
  value-typed string-argument evaluator; the verbatim
  `Microsoft.FSharp.Core.NetSdk.props` consumer fixture
  (`crates/msbuild/src/with_imports_tests/fsharp_core_netsdk_props.rs`,
  deriving `FSharpCoreMaximumMajorVersion` exactly); and the targeted real-SDK
  cause-list check `sdk_style_netcoreapp_fsharp_property_functions_leave_no_cause`
  (in `crates/msbuild/tests/fsproj_packageref_diff.rs`).

## D1. Scope

In scope: the `$( … )` expression language as MSBuild's `Expander` scans it —
property references, static property functions, instance member chains, and
nested `$(…)` inside string-literal arguments.

Out of scope (each stays a visible refusal): `@( … )` item vectors and
`%( … )` metadata (a different, item-typed language the property pass cannot
see); `$(Registry:…)`; `%XX` escape unescaping (a different MSBuild layer);
unifying `condition.rs`'s operand grammar onto this AST (a plausible later
consumer — the public seams keep their signatures); and any BCL surface not
individually pinned against the oracle.

## D2. Ground truth (reference)

Each fact was pinned by a stub project (`<R>expr</R>` + `-getProperty:R`,
dotnet msbuild 10.0.300) and is re-verified by the differential harness on
every run.

**Scanning / grammar:**

- The `$(…)` extent scanner is **quote-aware**: `$(Foo.Contains(')'))` and
  `$(V.Split(')')[0])` evaluate correctly (a quoted `)` does not close the
  expression). `find_balanced_close` is the tokenizer core.
- A dot inside `$()` is **always member access, never part of a property
  name**. Dotted property names are illegal at every source (XML element →
  MSB5016; `-p:A.B=v` → MSB4177; env var `E.V` unreachable because `$(E.V)`
  parses as member `V` on property `E`). (`-` remains legal in property names;
  only `.` changes meaning.)
- Member names are case-insensitive (`$(Foo.LENGTH)` → `3`).
- Arguments may be single-quoted literals (which may contain nested `$(…)`,
  including quotes), unquoted nested `$(…)`
  (`EnsureTrailingSlash($(Base))`), or bare integers (`Substring(1,2)`).

**Evaluation semantics:**

- Chains are typed, left-to-right: `$(V.Split('-')[0].Length)` → `8`;
  `$(Foo.Length.ToString())` → `3`.
- Paren-less instance properties work: `$(Foo.Length)` → `3`.
- A member access on an undefined property operates on `""`
  (`$(Undef.Length)` → `0`).
- `Split` with a string argument binds to **`String.Split(params char[])`**:
  the argument is a *set of characters*, not a separator string.
  `'a--b'.Split('--')` → `[a, "", b]`; `'a-b_c'.Split('-_')[1]` → `b`.
- A *terminal* `Split` array renders as .NET `Array.ToString()`
  (`"System.String[]"`, host/runtime-shaped), so a chain ending in an array
  stays `Unsupported`. An array is only usable via an indexer
  (`$(V.Split('-')[0])`) or `.Length` (`$(V.Split('-').Length)`). A string
  indexer yields a char (`$(Foo[0])` → `a`); an out-of-range indexer is an
  MSB4184 build error → `Unsupported`.
- Booleans render `True`/`False`. `Version` renders via .NET
  `Version.ToString()` (components joined with `.`, leading zeros dropped).
- `[System.Version]::Parse` requires 2–4 components, each `≤ Int32.MaxValue`
  (`Parse('10')`, `Parse('')`, `Parse('1.2.3.4.5')`, `Parse('2147483648.1')`,
  `Parse('-1.2')` all MSB4184). `.Major`/`.Minor`/`.Build` behave as .NET
  (`10.1.300` → `10`/`1`/`300`); an **absent** `.Build` (a 2-component
  version) is `-1`, not an error.
- A nested `$(…)` inside a **string-literal argument** is coerced to the
  parameter's `System.String`; MSBuild performs **no implicit conversion** of
  a non-string result, so the nested expression must reduce to a string:
  `Contains('$(V.Split('-')[0])')` and `Parse('$(V.TrimStart('v'))')`
  evaluate, while `Contains('$(V.Length)')` (int),
  `Contains('$(V.Split('-'))')` (array), and
  `Contains('$([Version]::Parse('1.2').Major)')` (int) all error the build.
  (This is why the argument evaluator is value-typed and admits a nested
  `$(…)` only when it yields a `Value::Str`.) A `Char` from a string indexer
  is accepted by MSBuild here, but we conservatively decline it.
- `[MSBuild]::EnsureTrailingSlash`: appends the host separator if absent,
  **normalises `\` → `/`** on a unix host (`'a\b'` → `a/b/`, `'a\'` → `a/`),
  and maps `''` → `''`.
- MSBuild **errors the whole build** on unknown members (MSB4184),
  out-of-range indexers, and `Version.Parse` failures. We do not reproduce
  errors; every MSBuild-error shape maps to `Issue::Unsupported` (literal
  passthrough + partial), the fail-safe direction.

## D3. Architecture (as landed)

`properties/expr.rs` holds the parser + typed values + dispatch, with
`substitute`/`substitute_with_fs` keeping their exact signatures and `Issue`
contract:

- **AST:** `Expr := PropertyRef | StaticCall { type_token, member, args } |
  Chain { base, links }`, `Link := Member { name, args } | Index(arg)`,
  `Arg := StrTemplate | Int | Nested(Expr)`. The parser is **total** over the
  extent the quote-aware scanner found; anything it cannot shape becomes
  `Unsupported` with today's literal-passthrough behaviour.
- **Typed value model:** `Value := Str | Int | Bool | Char | Version |
  StrArray`, with the §D2 rendering rules. Types flow through chains
  (`.Major` only on a `Version`) — parse-don't-validate for method chains.
- **Dispatch table:** `(receiver type, lowercase member, arg shape) → eval fn`
  for instance links; `(type token, lowercase member) → eval fn` for static
  calls. The table is the *entire* evaluation surface; a miss aborts to
  `Unsupported`. Filesystem-probing entries stay gated on `fs_probes_allowed`.
- **`Issue` contract unchanged:** `Undefined` → empty substitution + issue;
  `Unsupported` → literal passthrough + issue; unbalanced `$(` → tail
  passthrough.

The one deliberate behaviour change was the dotted-name correction (§D2):
`$(A.B)` moved from `Undefined{"A.B"}` + `""` to a member-access parse
(`Unsupported` + literal unless `B` is pinned). MSBuild errors the build on
these, so no working build's closure changed; both old and new mark the
project partial.

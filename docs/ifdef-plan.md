# Conditional-compilation (`#if` / `#else` / `#elif` / `#endif`)

> **Status: implemented.** Stages 1–8 and the Stage 8b sub-stream
> (8b.1 / 8b.2a / 8b.2b) all landed, along with the three "Deferred
> follow-ups" below. Two threads remain outstanding: the structured-payload
> *consumer* half for `#nowarn`/`#warnon` (their warning-number lists are
> still unread), and the inverse (virtual → generated) `#line` map for request
> handlers. Everything below the "Landed stages" list has detail *only* on
> what remains.

## Goal (reference)

Process `#if` / `#else` / `#elif` / `#endif` so that bytes inside inactive
branches are never tokenised — unblocking the malformed-inactive-branch corpus
fixtures (`ConditionalCompilation/{InComment01,InStringLiteral03}.fs`) and, more
importantly, real F# projects (`#if DEBUG`, `#if NETSTANDARD`, …).

A preprocessor layer sits between `lex` and `filter`
(`crates/cst/src/directives/`), leaving the Logos lexer a context-free
tokeniser and the lexfilter unchanged. Public entry point
`lex_with_symbols(source, &symbols)` (`driver.rs`); a state machine mirrors
FCS's `token` ↔ `ifdefSkip` rules, so inactive bodies are never lexed. The
expression grammar and `#if`-expression lexer follow FCS's `pppars.fsy` /
`pplex.fsl` (`!` tightest, then `&&`, then `||`; `true`/`false` are ordinary
identifiers).

**Symbol set** (from the consumer): default `COMPILED` (+ `EDITING`) for
compiled sources, `INTERACTIVE` (+ `EDITING`) for scripts, plus the project's
evaluated `<DefineConstants>`.

**Empty-roots semantics** (referenced by
`crates/lsp/tests/all/ifdef_diagnostics_integration.rs`): if `global.json`
`sdk.paths` resolves to zero usable roots (an explicit `paths: []`, or the sole
`$host$` token failing to resolve), `SdkDiscovery::resolve` returns `NotFound`
and the msbuild walker emits `SdkNotFound`. Body `<DefineConstants>` still
surface; a compiled file's `#if` symbols are then `{COMPILED, EDITING} ∪
body_defines` (scripts `{INTERACTIVE, EDITING}`). This is the strict reading of
the .NET host: omitting `$host$` opts out of the host install, and the LSP must
not paper over it with a silent fallback, or the symbol set would disagree with
`dotnet build`.

## Landed stages (one line each)

- **Stage 1** (PR #72) — directive expression AST + pure `parse_if_expr(&str)`.
- **Stage 2** (PR #74) — symbol-set evaluator `eval(&Expr, &HashSet<String>) -> bool`.
- **Stage 3** (PR #79) — line-oriented directive recogniser (`^ *# (if|else|elif|endif)`, `#` first non-ws token).
- **Stage 4** (PR #92) — stateful preprocessor driver `lex_with_symbols`: `ifdef_stack` + `mode`, yields active-branch tokens only, reports recoverable structural errors.
- **Stage 5** (PR #99) — corpus integration: `tests/corpus.rs` runs `lex_with_symbols`; the two malformed `ConditionalCompilation` allow-list entries removed; whole-directory sweep added.
- **Stage 6** (PR #119) — single-line directives (`#nowarn` / `#warnon` / `#line`) recognised and swallowed as trivia; `swallowed_directive_lines` workaround deleted.
- **Stage 7** (PR #103) — msbuild extracts evaluated `$(DefineConstants)` (`;`-separated) onto the project model, differentially tested against `dotnet msbuild -getProperty`.
- **Stage 8** (PR #123) — LSP passes the per-project symbol set into `lex_with_symbols` for diagnostics (landed with `sdk_resolver = None`; 8b closes that gap).
- **Stage 8b.1** (PR #128; `dotnet --info` wrapper-layout fallback PR #134) — `SdkResolver`/`SdkDiscoveryEnv` wired into `Workspace`; falls back to no-resolver on `DiscoveryError`; `default_build_properties()` seeds `Configuration=Debug,Platform=AnyCPU`.
- **Stage 8b.2a** (PR #136) — msbuild parses `global.json` `sdk.paths` into `Option<Vec<SdkPathEntry>>` (`Host` / `Relative`), host-faithfully lenient (non-string entries skipped; `InvalidType` only when present-and-not-array/null).
- **Stage 8b.2b** (PR #141) — lsp honours `sdk.paths` in `SdkDiscovery`: `roots: Vec<PathBuf>` expanded against the `global.json` dir, `resolve` iterates first-match-wins, empty roots ⇒ `NotFound` (see "Empty-roots semantics").

## Deferred follow-ups

Three follow-ups (in the order the cross-references assume — the third being
`#line` effects on diagnostic spans) have since landed; the remaining
outstanding work is under "Still outstanding".

- **Structured payloads for `NoWarn` / `WarnOn` / `Line` — data model** (PR #157): the recogniser now parses and carries the warning-number lists and the `#line` number+filename, with round-trip PBTs in `crates/cst/src/directives/line.rs`.
- **`HashLine` / `WarnDirective` syntax token kinds** (infrastructure `docs/completed/hashline-warndirective-trivia-plan.md`; green-tree consumer `docs/completed/parser-ifdef-plan.md`): the FCS `HASH_LINE` / `WARN_DIRECTIVE` (and `HASH_IF … INACTIVECODE`) trivia kinds now reach the rowan tree via the ifdef-aware `parse_with_symbols`, so dead `#if` branches no longer squiggle.
- **`#line` effects on diagnostic spans (`LineDirectiveStore`)** (stages 1–4, PRs #211/#213/#219/#229/#233, `docs/completed/line-directive-remap-plan.md`): the driver captures active-branch `#line` directives into a `LineDirectiveStore` and the LSP remaps diagnostic spans same-file and cross-file (publish-by-URI). The emitted trivia tokens remain payload-free (matching FCS, which keeps the line number on the lexbuf).

## Still outstanding

### `#nowarn` / `#warnon` consumer half

The parse layer is in place (PR #157 above), but the driver still classifies
all three single-line directives as trivia (`Directive::is_trivia`,
`crates/cst/src/directives/driver.rs`) and nothing reads the `NoWarn` / `WarnOn`
`.numbers` lists. **Trigger to revisit**: when the LSP starts surfacing per-line
warning suppression to the client (a "disabled warnings on this range"
feature). The remaining work is wiring the driver to retain the warning-number
payloads (rather than discarding them as trivia) and threading them to the
relevant request handler.

### Inverse (virtual → generated) `#line` map

`LineDirectiveStore` implements only the forward (generated → virtual) remap
used to report diagnostics against `#line`-asserted coordinates. The inverse
map that request handlers (hover, go-to-definition) need to translate a client
position back into a real source offset is still unimplemented — see Q4 in
`docs/completed/line-directive-remap-plan.md`.

### SDK-discovery deferrals (Post-8b.2)

- Caching `SdkDiscovery` instances across sibling projects (each project rebuilds one at evaluation time).
- Surfacing `DiscoveryError` itself as an editor diagnostic on the `.fsproj` buffer (today it only logs to stderr and falls back to no-resolver).
- Watching external SDK install-directory changes for invalidation (workspace/project-structure file-watch invalidation landed in `docs/completed/file-watch-invalidation-plan.md`; the SDK install dir is not watched).

## Notes

The deliberate `E_*` fixtures (`E_MustBeIdent01.fs`, `E_UnmatchedEndif01.fs`,
etc.) stay on the lexer allow-list — they are meant to fail and exercise
diagnostics; promoting them to a "lex with errors but recover" test is out of
scope here.

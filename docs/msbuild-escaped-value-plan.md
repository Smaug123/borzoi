# MSBuild escaped-value plan (make the type carry the rule)

> **Status:** Stages E0–E3 landed (#927, #932, #938, #941) — every property, item
> spec and metadatum is now stored *escaped* and unescaped exactly once at its
> point of use, replacing the old `literal_percents` corollary-enumeration
> side-channel. **E4 (the item/glob-resolver seam) and E5 (delete the decline
> machinery) remain** — full detail under "Still to do". AGENTS.md points here
> for the durable rule (*scan and split before you decode; trim in the domain;
> decode at the leaf*), restated next.

## The rule

**MSBuild stores every property, item spec and metadatum value *escaped*, and
unescapes it exactly once, at the point of use.**

That is the whole model. Verified in dotnet/msbuild (checkout at
`~/Documents/GitHub/dotnet/src/msbuild`):

- `src/Shared/EscapingUtilities.cs:310` —
  `s_charsToEscape = { '%', '*', '?', '@', '$', '(', ')', ';', '\'' }`.
  `Escape()` rewrites each of those **nine** characters to `%XX`.
- `EscapingUtilities.cs:59` — `UnescapeAll()` decodes `%` + two hex digits to a
  single **UTF-16 char** (`(char)((digit1 << 4) + digit2)`, line 112), scanning
  left to right and **never re-scanning decoded output** (`%2525` → `%25`, not
  `%`). A `%` with any other suffix stays literal. So `%E2` is U+00E2, not a
  UTF-8 byte.
- `src/Build/Evaluation/Evaluator.cs:1186–1189` and
  `src/Build/Definition/Toolset.cs:802,805` — reserved path properties are
  `Escape()`d **when seeded**, so evaluator-computed text enters the table
  already escaped.
- `src/Build/Definition/ProjectProperty.cs:89` and
  `Instance/ProjectPropertyInstance.cs:70` — `EvaluatedValue` is
  `UnescapeAll(escapedValue)`. The unescape happens at the *read*.

Three value sources, therefore three ways a string enters the escaped domain,
and exactly one way it leaves:

| source | how it enters | pinned by |
| --- | --- | --- |
| project XML body/attribute text | **verbatim** — XML text *is* escaped-domain text | `<P>a%20b</P>` evaluates to `a b` |
| caller globals (`-p:`, `extra_properties`) | **verbatim** — same domain as XML | `evaluator.rs` comment, oracle-pinned |
| anything the evaluator computes from the world (project path, `MSBuildThisFile*`, SDK/toolset dirs, property-function results) | `Escape()` — all nine chars | `Evaluator.cs:1186`, `Toolset.cs:802` |
| **leaving** (property table, item identity, condition operand, function argument, filesystem path) | `UnescapeAll()` — exactly once | `ProjectProperty.cs:89` |

Plus one deliberate hole, which is MSBuild's, not ours: a `Char` returned by a
string indexer (`$(P[3])`) is **not** re-escaped on the way back into the
buffer, so its `%` can still compose an escape with what follows. It survives as
the single explicitly raw splice (`Escaped::push_unescaped_raw`).

## Design: `Escaped` (landed in `crates/msbuild/src/properties/escaping.rs`)

`Escaped(String)` — a value in MSBuild's escaped domain, with **no** `Display`,
`Deref`, or `AsRef<str>`, so every read is a deliberate choice between two exits:

- `from_xml` — project XML body/attribute text or a caller global, verbatim.
- `from_computed` — text the evaluator computed from the world; `Escape()`s the
  nine reserved chars.
- `unescape` → `String` — the point of use; the only decode, exactly once.
- `as_escaped` → `&str` — for splicing into another escaped buffer and for
  scanning (`;` splits, glob classification, `%(…)`/`@(…)` detection), which
  MSBuild all performs on escaped text.
- `push` / `push_unescaped_raw` — the latter is MSBuild's indexer hole above.

The point of the type: double-unescape does not typecheck, and every leaf's
domain choice is a finite, greppable audit rather than percent-by-percent
provenance reasoning.

## Leaf inventory (the per-leaf domain checklist)

Every consumer chooses `unescape()` (MSBuild unescapes here) or `as_escaped()`
(MSBuild scans/splices here, still escaped):

| leaf | domain | stage |
| --- | --- | --- |
| evaluated property table (walker output) | `unescape` | E1 |
| `DefineConstants` | split escaped on `;`, then `unescape` each | E1 |
| `TargetFramework(s)` (`target_frameworks.rs`) | split escaped, `unescape` each | E1 |
| `Import` `Project=` / SDK paths, `Directory.Build.*`, `DirectoryPackagesPropsPath` | `unescape` before touching the filesystem | E1 |
| central package versions, package ids/versions | `unescape` (or degrade) | E1 |
| condition operands | `unescape` at the operand leaf | E1 (folded in) |
| property-function arguments and receivers | `unescape` in, `from_computed` out | E3 |
| path functions (`eval_exact_path_arg`) | `unescape` args | E3 |
| item `Include`/`Exclude`/`Update`/`Remove` specs | split on escaped `;`, classify globs and `%(…)`/`@(…)` on escaped text, `unescape` surviving literals into identities | E4 |
| item metadata values, `Link` | `unescape` | E4 |
| `GlobRequest` (the LSP resolver seam) | escaped fragments, resolver-side unescape | E4 |

## Landed stages (one line each)

- **E0** (#927) — `escaping.rs`: `escape`, `unescape`, the `Escaped` newtype;
  faithful `UnescapeAll` scan; property-tested (`unescape(escape(s)) == s`) and
  pinned against MSBuild by the `escaping_diff.rs` differential over the `expand`
  op.
- **E1** (#932) — the keystone: `PropertyMap` stores `Escaped` via `insert_xml` /
  `insert_computed`, `substitute` builds an escaped buffer, `literal_percents`
  and its machinery deleted; each leaf gained `unescape()` or a decline guard.
  Closed the sixth wrong-commit (a reserved char in the project directory no
  longer splits item lists — guarded by
  `a_reserved_character_in_the_project_directory_does_not_split_items` in
  `fsproj_msbuild_diff.rs`); condition operands were folded in here (leaving them
  escaped would have been a regression).
- **E2** (#938) — conditions stop degrading on escape-bearing *source*; a bare
  `%` still lexes to `Token::Unknown` → `Unsupported` (matching MSBuild's
  MSB4090), so only quoted/spliced operands start committing.
- **E3** (#941) — the expression evaluator runs *inside* the escaped domain, as
  MSBuild's `Expander` does: property splices contribute escaped text, a `.NET`
  method unescapes receiver + args once and re-escapes its string result, the
  indexer `Char` is the one raw splice. `property_expr_diff.rs`'s `%XX` exclusion
  was deleted so the escape dimension is now generated.
- **item-escape generative differential** (#933) — `fsproj_item_escape_generative_diff.rs`:
  item specs over an escape-bearing alphabet, run through both item seams; guards
  the current (pre-E4) decline behaviour and will guard E4's commit.

## Still to do

### E4: the item/glob-resolver seam

**Dependencies**: E1 (E2/E3 parallel). **Supersedes the decoder half of
compile-item-fidelity-plan stage F1** — F1's seam analysis stands verbatim and is
the hard part; it just consumes `Escaped` instead of re-deriving a decoder.

Most of the item pass already moved into the escaped domain with E1–E3:
`spec_fragments` (item_pass.rs) splits on the escaped `;`, `fragment_identity`
and `scalar_use` unescape at the leaf, and glob / `@(…)` / `%(…)` classification
runs on escaped text (so `%2a` is a literal star and `%25(` is not a metadata
reference).

**What remains is the glob-resolver seam.** `GlobRequest::include`
(`crates/msbuild/src/lib.rs:186`) is still a `;`-joined string the LSP resolver
re-splits and re-scans for `*`/`?`. Because the resolver re-scans, item_pass.rs
must decline any fragment whose *decoded* form would smuggle a metacharacter past
the classification already done:

- `fragment_for_resolver` (item_pass.rs:46) → `None` when
  `decodes_to_metacharacter` (item_pass.rs:52) finds a `%XX` decoding to `; * ?`;
- the caller raises `unsupported_across_resolver_seam` (item_pass.rs:2665), the
  same fail-safe the old substitution-level withdrawal produced;
- the include list is assembled as `resolver_specs.join(";")` (item_pass.rs:2626).

E4 makes `GlobRequest::include` carry a **fragment list in the escaped domain**
that the resolver never re-splits or re-scans — it unescapes each fragment
resolver-side, at its own point of use — then deletes the three guards above.

**Correctness oracle**: fidelity-plan F1's oracle (differential fixtures
`a%20b.fs`, `a%3bb.fs`, `a%2ab.fs`) plus the `fsproj_item_escape_generative_diff.rs`
harness (#933) with its escape dimension on, and the property F1 specifies for
the seam (fragment count and literal/glob classification preserved end-to-end).
The certain-fraction of the generative harness should **rise**; a fall is a bug.

### E5: delete the decline machinery

**Dependencies**: E4.

With E4 done, the only escape-motivated decline left in the tree is the trio E4
removes (`fragment_for_resolver`, `decodes_to_metacharacter`,
`unsupported_across_resolver_seam`). E5 is the closing sweep: confirm a grep for
each returns nothing, state the "no escape hatch left" invariant in the
`escaping.rs` module doc, and remove any remaining escape-motivated
`Issue::Unsupported` withdrawal. (The original plan named temporary methods
`contains_msbuild_escape` / `decline_if_live_escape`; those never materialised
under those names — the live machinery is the item-seam trio, so E5 may collapse
into E4's final commit.)

## Risks (durable invariants)

- **Double-unescape** is foreclosed by the type: `unescape` consumes an `Escaped`
  and yields a `String`, and an `Escaped` can only be built by escaping
  (`from_computed`) or by taking known-escaped text (`from_xml`).
- **Escaping `$` and `(` in computed seeds** is correct only because we expand in
  a single pass and never re-expand a spliced value. Any future fixed-point
  iteration must unescape first — asserted in the module doc.
- **`%(…)` metadata references** interact with `%`-escaping: scanning must happen
  on escaped text (E4). Pin `%25(Foo)` as a corner.
- **Ordering**: `escape` and `unescape` are not inverses the other way
  (`escape(unescape(s)) != s`), so a leaf that unescapes and re-stores would
  corrupt. There is no such leaf; keep it that way.

## Related

- The **unix path fixup** (`MaybeAdjustFilePath`), found by E0's decoder
  differential, is **not** an escaping bug; it has its own doc and branch,
  `docs/msbuild-unix-path-fixup-plan.md`. E0 pinned only what the escaped stack
  needs from it: an escaped backslash is invisible to the fixup, so the escaped
  domain is the outermost layer.
- `docs/compile-item-fidelity-plan.md` stage F1 is superseded in part by E4.

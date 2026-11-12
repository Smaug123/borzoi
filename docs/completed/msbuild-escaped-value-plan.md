# MSBuild escaped-value plan (make the type carry the rule)

> **Status: complete.** Stages E0–E3 (#927, #932, #938, #941) store every
> property, item spec and metadatum *escaped* and unescape it exactly once at
> its point of use. E4 carries item fragments across the glob-resolver seam
> still split and still escaped, and E5 deleted the declines that seam needed,
> so no escape-motivated decline is left. AGENTS.md points here for the durable
> rule (*scan and split before you decode; trim in the domain; decode at the
> leaf*), restated next, and for where an escaped wildcard is data ("Filespec
> classification").

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
| item `Include`/`Exclude`/`Update`/`Remove` specs | split on escaped `;`, classify globs and `%(…)`/`@(…)` on escaped text, `unescape` surviving literals into identities | E1–E3 |
| item metadata values, `Link` | `unescape` | E1–E3 |
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
  item specs over an escape-bearing alphabet, run through both item seams.
- **E4** — the glob-resolver seam. `GlobRequest::include`/`excludes` carry
  `SpecFragment`s: already split, still escaped, each classified on its escaped
  text and decoded by the resolver at its own point of use, so nothing across
  the seam can re-split an escaped `%3b` or glob an escaped `%2a`. The resolver
  applies an `Exclude` as MSBuild does, which differs by the include it meets
  (see "Filespec classification" below). The generative harness now drives the
  shipped resolver (`glob_resolver::resolve`) over live globs as well as
  escapes, and every generated case commits through that seam; the seam property
  (fragment count and classification preserved end-to-end) is
  `with_imports_tests/globs.rs`'s `route_splices_resolver_output_verbatim`.
- **E5** — the decline machinery is gone: `fragment_for_resolver`,
  `decodes_to_metacharacter`, `unsupported_across_resolver_seam` and
  `escaping::decodes_to_any`. The last escape-motivated decline, a wildcard
  `<Import>` mixing a live and an escaped wildcard, imports the literal file
  MSBuild imports instead.

## Filespec classification (where an escaped wildcard is data)

Whether MSBuild globs an item or import filespec is decided on its **escaped**
text (`EngineFileUtilities.FilespecHasWildcards`; ported as
`escaping::filespec_has_wildcards`): a live `*`/`?` and **no** escaped
`%2a`/`%3f`. A filespec with both kinds is literal, whole — `Include="a%2a*.fs"`
is the one item `a**.fs`, `Import Project="*.props%2a"` imports the file named
`*.props*` — because once decoded MSBuild could not tell them apart.

An `Exclude` is then applied two ways, by the include fragment it meets:

| include fragment | exclude classified by | an escaped `%2a` in the exclude |
| --- | --- | --- |
| literal (`ItemSpec` `ValueFragment`) | `FileSpecMatcherTester.Parse`: glob iff `FilespecHasWildcards` | a literal star in a name |
| glob (`GlobFragment` → `GetFileList`) | `IsValidExclude` drops one with both kinds; the rest are decoded and globbed | a **wildcard** |

So `Include="*.fs" Exclude="a%2ab.fs"` excludes `axb.fs`, while
`Include="axb.fs;a%2ab.fs" Exclude="a%2ab.fs"` excludes only `a*b.fs`.

Admitting escaped excludes put more of the file matcher's pattern language in
reach, and two rounds of review found it was not all modelled — mostly in
shapes live wildcards reach too. Fixing each quirk as found was a deny-list
with no end, so the resolver's gate is an **allow-list**
(`glob::unmodelled_shape`): it commits only when every spec it matches
against, and every name a glob would match, is printable ASCII with no
trailing separator, no segment ending in `.` or a space, and none of `"<>|:`.
Anything else declines (`GlobDecline::OutsideModelledShape`) — the UTF-16
width of a `?`, .NET's non-ASCII case folding, Windows trailing-dot rules and
the rest among it, unmodelled and unreachable. (A non-ASCII name found by a
glob is admitted where it cannot matter: no `?` in play, and no `ı`/`ſ`,
the two characters `OrdinalIgnoreCase` folds onto ASCII.) An exclude with a
wildcard directory part (`**/.*/**`, `bin/**`) has two structural rules of
its own: it must be anchored as the glob include it filters is (both absolute
or both relative, `Unmodelled::MixedAnchoring`), and it must not meet an
include with a `..` segment, whose `..` its `.*` would otherwise take
(`Unmodelled::ParentSegmentUnderWildcardExclude` — on `main` the SDK's own
`**/.*/**` swallowed a `../shared/*.fs` glob). A broader rule declining every
wildcard directory segment was tried and measured: it declined the SDK's
default excludes in six corpus projects that match MSBuild exactly, and the
generative differential showed it guarded nothing the two narrow rules miss.
Inside the allow-list, the resolver follows MSBuild's source, or declines by
name:

- **A final `**`** is `**/*.*` (`FileMatcher.SplitFileSpec`): `bin/**` names
  the files below `bin/`, not a file called `bin`.

- **Legality** (`FileMatcher.RawFileSpecIsValid`/`IsLegalFileSpec`, ported as
  `glob::is_legal_filespec`): a `**` that is not a whole segment (`a**b.fs`,
  live or decoded from `a%2a%2ab.fs`), a `..` after a wildcard, a `...`, a `:`
  past the second character. MSBuild falls back differently per caller, so the
  resolver declines (`GlobDecline::IllegalFilespec`).
- **All files** (`IsAllFilesWildcard`): a filename part of `*.*` matches
  extensionless files too, as `*` does — except in `FileSpecMatcherTester`'s
  filename-only branch (`**/*.*` against a literal include), which needs a dot.
- **Literal names** against a literal include compare as full paths
  (`./ab.fs` is `ab.fs`), unless either spelling holds a character Windows
  forbids in a filename (`*?:<>|"`), which compares as written
  (`FileUtilities.PathIsInvalid`): `./a*b.fs` and `a*b.fs` differ. A wildcard
  exclude's fixed directory there is a full path too (`sub/../*.fs` is `*.fs`).
- **Against a glob include** the file matcher relates excludes by spelling: a
  `..` segment is compared as written (which the resolver does), and a `.`
  segment too (which `Pattern` would drop, so it declines,
  `GlobDecline::CurDirSegmentExclude`).

All of it is pinned against `dotnet msbuild` 10.0.301 by the generative
harness's hand-picked corners, and its generated sweep reaches every row.

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

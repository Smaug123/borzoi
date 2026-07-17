# FCS divergences

This catalogues the deliberate ways `borzoi-*` differs from FCS (the F#
Compiler Service, checked out at `../fsharp`). It covers documented divergences —
those called out in a code comment, commit message, or PR description — across
the lexer, lex-filter, parser, directive layer, assembly reader, MSBuild
evaluator, and LSP. Each entry cites the commit that introduced (or last touched)
it so the reasoning is recoverable from history.

Two kinds of difference are deliberately *not* counted as divergences:
- pure fidelity notes ("mirrors FCS exactly"), and
- cases where we reproduce an FCS quirk on purpose (e.g. the lexer diff harness
  maps `42L` to `UInt64`, mirroring an FCS bug at `ServiceLexing.fs:1556`).

## Conventions

- **Oracle-side workaround.** Several differences live only in the
  `tools/fcs-dump` differential-test oracle, which compensates for lossiness in
  FCS's *public* API (e.g. FCS reporting IL accessor visibility as `Public`).
  These are not divergences in our reimplementation; they have their own section.
- **Language-version pinning.** Since `42265b3` (#613) the LSP resolves each
  project's `<LangVersion>` (absent → 10.0, FCS's default; orphan/non-file
  buffers → `preview`) and threads it through the parse, so wired feature gates
  such as `#elif`/FS3350 and nullness honour it and *match* FCS (not a
  divergence). The gates that are *not yet* version-aware are implemented at
  their latest-version form unconditionally, so they diverge only when a project
  sets an older `<LangVersion>`. See
  [Language-version pinning](#language-version-pinning).
- **Correctness over availability.** Where FCS warns-and-continues on malformed
  metadata, we usually error loudly instead, so a future format change is not
  silently masked. These are flagged inline.

## How this catalogue was built

A sweep over `main` (one Sonnet agent per ~12-commit range for the first 188
commits, then one agent per commit for the 19 that followed, up to `0531f24`)
collected every documented divergence; Opus then deduplicated recurring themes,
separated oracle-side workarounds, and cross-checked every survivor against the
*current* tree (many early divergences — most of the lex-filter offside gaps,
interpolated strings, byte-string literals, F# entity-kind projection — have
since been closed; see [Resolved since introduction](#resolved-since-introduction)).
The raw per-range tables live in this file's git history. Plans for the
partially implemented `borzoi-sema` type-checker / inference work describe
further intended divergences; those are omitted here until the relevant code
exists.

---

## Lexer

- **`///` XML-doc comments are not a distinct token** — emitted as plain
  `LineComment`, folded in with `//`; FCS distinguishes XML doc. (`4c10d1d`)
- **Operator taxonomy collapsed** — `lex.fsl`'s `INFIX_STAR_STAR_OP`,
  `INFIX_AT_HAT_OP`, … are all emitted as a single opaque `Op(&str)`. (`4c10d1d`)
- **Shebang `#!` accepted anywhere** — FCS restricts it to line 1; we accept it
  as a comment at any position (Logos can't express the positional anchor, and
  accepting it is harmless). (`4c10d1d`)
- **Trailing-separator numerals accepted** — `1_`, `0xFF_`, `1e_` lex as one
  numeric token; FCS's strict `digit ((digit|sep)* digit)?` shape rejects them.
  (`4c10d1d`)

## Lex-filter (offside)

- **Only the strict (modern `StrictIndentation`) dialect is ported** — there is
  no language-version switch selecting the legacy non-strict layout. (`b321b15`)
- **EOF-closed virtual spans** — synthetic end tokens at EOF copy the EOF token's
  actual span; FCS uses `Position.ColumnMinusOne` of the EOF lookahead
  (`LexFilter.fs:644`). Also, the `COMING_SOON`/`IS_HERE` synthetic tokens
  (`LexFilter.fs:1577-1636`) are not modelled — they map to
  `FSharpTokenKind.None` and are filtered from the diffed stream anyway.
  (`7282b20`, `de65b90`)

## Parser

- **`(or)` parenthesised operator name accepted** — `let (or) e1 e2 = …` (and the
  bare value `(or)`). Current FCS has removed `OR` from its `operatorName`
  grammar, so it now reports a *parse error* ("Unexpected keyword 'or' in
  pattern"); we accept `(or)` as the ML-compat boolean operator name. It is real,
  shipped FSharp.Core source reachable via SourceLink and parses warn-only (FS0086
  "operator should not normally be redefined", a *semantic* diagnostic) under
  FsAutoComplete, so an LSP serving real source must read it (D7 "incomplete,
  never wrong"). `(&)` (`Token::Amp`) was already accepted and still parses under
  the differential oracle (FCS 43.12.204), so it is not a divergence today. No
  differential coverage — FCS errors on `(or)`. (`is_paren_operator_name`,
  `crates/cst/src/parser/classify.rs`)
- **Dotted longident head in lambda-arg position not parsed** — `fun X.Y -> …`.
  Verified live 2026-05-31. (`a5b00d8`)
- **Sign-folding: overflow recovery values on genuine errors** — adjacent-sign
  folding itself now covers the FCS fold set (see
  [Resolved since introduction](#resolved-since-introduction)). For a *genuine*
  overflow — `-2147483649`, or a leading-zero `MaxValue + 1` spelling like
  `-02147483648` (the `isInt32BadMax`/`isInt64BadMax` rescue is
  spelling-sensitive for int32/int64, so both error) — the two sides emit a
  diagnostic but the recovered `SynConst` value diverges (FCS recovers
  `Int32 0` from its lexer fallback; we keep the wrapped value), so those error
  cases aren't asserted as AST matches. (`2c511cd`, sign-fold landing)
- **Anonymous-record-type fields admit only `ident : typ`** — FCS's `recdFieldDecl`
  grammar accepts attributes/`mutable`/access modifiers (pars.fsy:2978-2980) then
  errors on them in a post-pass (pars.fsy:6526-6529); we match the *accepted*
  language without reproducing the accept-then-reject quirk. (`64cbe96`)
- **`TRIPLE_BYTE_STRING_LIT` is a CST kind absent from FCS** — FCS has no
  `TripleQuote` variant in `SynByteStringKind` (`"""abc"""B` is `Regular`,
  SyntaxTree.fs:132-135); we keep a dedicated token kind so the normaliser knows
  which decoder to run. (`14b820a`)
- **Some semantic-validation diagnostics scoped out** — escape-validity and
  decimal mantissa overflow (>28 significant digits) are not surfaced at parse
  time (the integer out-of-32-bit-range check *is* now emitted). (`00e07d3`)
- **Invalid prefix-operator shapes not diagnosed** — shapes like `??+` parse via
  the normal grammar rule; FCS emits an "invalid prefix operator" diagnostic we
  don't model at the parser layer. (`2c511cd`)
- **Non-block inline `use … in` and `let!`/`use!` operands are unsupported** — a
  *non-block* `let … in` (mid-expression: an infix RHS `a && let x = e in b`, a
  tuple element, a `lazy`/`assert`/`fixed` operand) now parses: `parse_minus_expr`
  dispatches the raw `Token::Let` to `parse_let_or_use_expr` (the same
  `SynExpr.LetOrUse` the block form produces). Two residual cases stay deferred.
  (1) `use … in` in this position: FCS's inline production relabels the binding's
  leading keyword to `Let` (the block form keeps `Use`), which our text-based
  `is_use` can't reproduce without misrepresenting the source — so a non-block
  `use … in` operand stays a rare reject rather than a wrong-AST. (2) The bang
  binders `let!`/`use!` surface as a raw `Token::LetBang`/`UseBang` and are not
  dispatched (`non_block_bang_binders_reject_without_panicking`); they need the
  raw-`BINDER … IN` production with `and!` grouping.
- **Recursion-depth cap rejects pathologically-nested input FCS accepts** — the
  hand-written parser bounds recursion at `MAX_PARSE_DEPTH` (512 counter ticks;
  several chokepoints stack, so the effective nesting bound is ~150). Past it the
  breach is recorded once, the remaining input drained to EOF as one ERROR node,
  and `parse_inner` collapses the result to a single "nesting too deep"
  diagnostic (the tree stays lossless). FCS parses such input — its own 8 MiB
  stack overflows only at ~2000–4000 levels — but a stack overflow *aborts the
  process* (it doesn't unwind, so the LSP's `catch_unwind` parser wrapper can't
  catch it), so we cap far lower (correctness over availability). A no-op below
  the threshold, so real source is byte-identical. (`95b8956`, #569)
- **Diagnostic message text is not reproduced verbatim** — diff tests compare AST
  projections and require only a non-empty error list, not FCS's exact wording.
  (general testing-fidelity caveat; `edfb565`)
- **Malformed opaque sig header + indented `val`: recovery tree differs** — a
  bodyless opaque `type T` in a `.fsi` followed by an *indented* `val` is promoted
  out of the type to a module-level `Val` (the `ProvidedTypes.fsi` idiom, #115).
  When the header is *malformed* in the one specific shape `type T when` (a bare
  trailing `when` with an empty constraint clause and no type parameters) followed
  by the abutting `val`, the input is invalid on **both** sides (both emit parse
  errors), but the recovery *shape* diverges: we still promote a phantom
  module-level `VAL_DECL`, whereas FCS consumes the `val` into the erroring type
  name ("Unexpected keyword 'val' in type name") and emits no `Val`. The divergence
  is confined to that one shape — `type T<'a` (missing `>`), `type T<'a when`, and
  `type T when 'a` all *do* promote on both sides (oracle-verified), so matching FCS
  would mean gating promotion on an *empty trailing `when`-clause*, a narrow
  grammar-recovery quirk on input no one writes. Left as-is: correctness is intact
  (both reject; only the recovered tree of already-invalid input differs), and a
  header-error gate that is any broader regresses the three promoting shapes.
  (`parse_sig_type_defn_repr`, `crates/cst/src/parser/decls_sig.rs`; surfaced by the
  codex review of #115, fix attempt #120 abandoned)

## Directives / preprocessor

- **`#nowarn` / `#warnon` numbers parsed but not consumed** — the warning numbers
  are decoded into structured payloads, but nothing consumes them to suppress
  diagnostics. (`3c84c65`, structured payloads in PR #157)
- **`#line` request-coordinate inverse mapping remains deferred** — diagnostic
  generated→virtual remapping is implemented through `LineDirectiveStore`, but
  request handlers do not yet map virtual positions back to generated source
  coordinates. (`3c84c65`, remap follow-up)
- **`#if` identifiers are ASCII-only** — FCS accepts Unicode identifiers
  (letter/digit/`_`/`'`); the corpus is ASCII-only. (`b6c902a`)
- **Directives are recognised in a dedicated layer**, not swallowed at the lexer
  under `Compiling | SkipTrivia` as FCS does (`#` lexes as `Hash`). (`4c10d1d`)

## Assembly reader (model + owned ECMA-335 backend)

The public view is the in-crate `Ecma335Assembly`, over
`crates/assembly/src/ecma335_assembly.rs` + `reader/`. Two early divergences that
existed only because of bugs in a previous reader are gone with the move to the
owned reader — see [Resolved since introduction](#resolved-since-introduction).

- **Type abbreviations not projected from the pickle merge** — `type IntId = int`
  is inlined by fsc (no ECMA TypeDef row), so the measure-overlay merge emits
  nothing for it; deferred. (`ffb2dd4`)
- **Incomplete-unpickler decode failures are recorded F#-overlay skips** — when
  the host signature pickle can't be decoded, enumeration returns the
  un-enriched ECMA tree and records a skipped F# overlay for source names,
  extension flags, and measures, including the decode reason. A `[<Measure>]`
  type then surfaces as `EntityKind::Class` (IL truth) instead of `Measure`, and
  source-name / extension facts fall back to the existing IL heuristics; callers
  can distinguish this from a fully enriched assembly via
  `AssemblyProjectionSkips::skipped_fsharp_overlays`. A successfully decoded
  pickle that contradicts the ECMA tree is still fatal. The `u_expr` decoder now
  decodes most arms for alignment and keeps structured values only for the
  subset the merge needs; it still refuses payload-heavy `Match`/`Obj`, selected
  `u_op` / IL-instruction forms, and other shapes that do not yet have a
  consumer. (`ffb2dd4`, `e591f0c`)
- **`IndexerNameAttribute` deliberately not decoded** — the indexer↔default-member
  binding is read from `MethodSemantics` instead, and the attribute cannot be
  added to the well-known-attributes catalogue because that catalogue mirrors
  fsc's `WellKnownILAttributes` enum exactly, which omits it. (`c1619ca`)
- **Union `Tags` nested class filtered by name heuristic** — parent=Union ∧
  child="Tags"; IL carries no marker attribute and FCS doesn't surface it.
  (`a660bc9`)
- **F#-native extension-method heuristic — remaining holes** — the `extension`
  flag is now read authoritatively from the F# signature pickle's
  `IsExtensionMember` bit, which closed the `[<CompiledName("A.B")>]`
  misclassification (see [Resolved since introduction](#resolved-since-introduction)).
  Two holes remain, both in the IL member *projection* rather than the flag:
  **nested-type augmentations** and **generic-target augmentations**
  (`type T<'a> with member …`). The generic-target case is dropped on *both*
  sides — the `fcs-dump` oracle also skips F#-native extensions on generic
  targets, because re-prepending the receiver would need the target's typars
  threaded through (`tools/fcs-dump/Program.fs`), so closing it is entangled
  with the generic-entity projection limitation rather than being a pure
  reader gap. (`6215f4c`, pickle-flag slice)
- **Type-level `[RequiredMember]` not projected** — deliberately redundant with
  the per-member flag. (`4776881`)
- **We emit all members, including private** — an LSP needs private decls; the
  diff oracle compares only the `AccessibleFromSomeFSharpCode` subset. (`56cb8bc`)
- **Malformed `FSharpInterfaceDataVersionAttribute` errors loudly** — FCS warns
  and returns `false`; we raise `ImportError::UnsupportedEcmaLayout` (correctness
  over availability). (`b56dade`)
- **XML doc-comment ID generation follows Roslyn/ECMA, not FCS** — `doc_id`
  reconstructs each `<member name="…">` key (`T:System.Console`,
  `M:System.Console.WriteLine(System.String)`, …) from our own `Entity`/`Member`
  model, decoupling doc lookup from any one compiler. It mirrors fsc's IL-reader
  path (`GetXmlDocSigOf*`) *except* for multidimensional arrays, where FCS
  encodes nonconformantly (`[0:]` for a 2-D array, so FCS can't even find their
  docs); we deliberately emit the Roslyn/ECMA-conformant form (`int[,]` →
  `[0:,0:]`), validated against real Roslyn-emitted `.xml` rather than FCS's own
  generation path. (`50c3c35`, #586)

### Coverage gaps (refused-but-isolated)

Since `#708` a member or type whose signature uses a construct the reader does
not model yet is **dropped and recorded** rather than aborting the enclosing
assembly (the reader plan's "bound uncertainty"). Member drops land on
`Entity::skipped_members`; whole-type drops (the type's own *shape* is
undecodable) are reported through `Ecma335Assembly::enumerate_type_defs_with_skips`
— the `EcmaView::enumerate_type_defs` trait method returns only the kept types.
Before `#708` any one of these zeroed the whole DLL, so a modern BCL was almost
entirely invisible (e.g. `System.Private.CoreLib` and the ref pack's
`System.Runtime.dll` — which hold `System.Object`/`String`/`Int32` — each
contributed *zero* types); now they project all but the handful of items they
genuinely cannot represent.

This is the enumerated set of what stays refused (each is a coverage gap in the
signature/attribute decoders, **not** silent mismodelling — the projector still
refuses to fabricate a value; it just refuses one item, not the assembly). The
`projector_malformed_metadata.rs` fixtures pin each shape's refusal.

| Construct | Encoding | Scope | Emitted by |
| --- | --- | --- | --- |
| `allows ref struct` — attributed-constraint form | a `GenericParamConstraint` row carrying a custom attribute | member (method typar) **or whole type** (type typar) | .NET 9+ `where T : allows ref struct`. *Distinct from the `AllowByRefLike` flag-bit form, which we do decode* — see the `TypeParameter::allows_ref_struct` entry under [Resolved since introduction](#resolved-since-introduction). This is the only construct that drops whole *types*. |
| Unrecognised `modreq` | a required custom modifier other than the four the projector understands (`InAttribute`, `IsVolatile`, `IsExternalInit`, `UnmanagedType`) | member | nothing in the .NET 10 runtime or ref pack — the whole BCL uses only those. C++/CLI's `IsConst`/`IsLong`/… would land here. ECMA-335 II.7.1.1 *requires* refusal: a `modreq` must be understood, so an unknown one may not be dropped. (An unknown `modopt` **is** dropped — the same clause says an optional modifier may be ignored.) |
| Modified `void` pointee | `PTR cmod* VOID` (`SigError::UnexpectedVoid`) | member | a custom-modified `void*` (C++/CLI's `modopt(IsConst) void*`). Zero occurrences in the runtime or ref pack; the unmodified `void*` and the `modreq(IsExternalInit) void` return are both projected. |
| Function pointers | `ELEMENT_TYPE_FNPTR` (`0x1B`) | member | `delegate*<…>` |
| varargs calling convention | `IMAGE_CEE_CS_CALLCONV_VARARG` (`0x05`) | member | C++/CLI, `__arglist` |

Counts drift with the target framework; as an illustration, the 10.0.9 shared
runtime leaves ~265 members and ~52 whole types refused-but-isolated across
~13,300 kept types, and the matching ref pack ~142 members / ~52 types across
~3,900 kept. Point
`cargo run -p borzoi-assembly --example enum_sweep -- <dir>` at a runtime or
ref-pack directory to reproduce the tally; it prints the drops bucketed by
reason, which is how the list above is kept honest. Custom modifiers used to be
the largest bucket by a distance — 397 of the runtime's then-662 member drops —
and are now empty (see [Resolved](#resolved-since-introduction)); what remains is
function pointers (~110) and the attributed-`allows ref struct` constraint
(~155), which is also the only construct that drops whole types.

Not in the table above: the `NullableAttribute byte[] … length mismatch` that
this section originally flagged as a *possible latent bug* was one — the nullable
pre-order walk skipped the pointer node, so every `[Nullable(byte[])]` covering a
pointer position (e.g. the `T*` / `T*[]` accessors throughout
`System.Private.CoreLib`) was short by a byte and spuriously refused. Roslyn's
walk visits the pointer node (an oblivious `0`) then the pointee; the walk now
does the same (`crates/assembly/src/ecma335_assembly.rs` `walk_nullable_sig`),
recovering ~36 members per framework. It was never a feature gap. (`#711`)

## Assembly: F# pickle reader

- **`TType_anon` (tag 9) unsupported** — raises `UnsupportedPickleTag` (no
  fixture exercises it). (`5190d33`)
- **SRTP trait solutions (`u_trait_sln`) partially modelled** — common solutions
  are decoded for alignment, but payload-heavy arms 4/5 remain refused.
  (`5190d33`)
- **IL `Local` scope-ref not rescoped** — FCS rescopes `Local` against the
  importing reader's scope (`:1233`); we leave it as `Local`, deferring rescope
  to the projection boundary. (`20dd226`)
- **`Namespace true` payload collapsed** — FCS's tag-2 invariant `Namespace true`
  payload is consumed but flattened to a payload-free `Namespace`. (`20dd226`)
- **Malformed reserved/used-space bytes error loudly** — FCS warns and continues
  (`read_space` / `read_used_space1`); we error (correctness over availability).
  (`20dd226`)
- **Extension-member exclusion carries its uncertainty** — FCS admits no extension
  member to the unqualified name environment, and no F#-native augmentation to a
  module-qualified path either (both FS0039). Sema mirrors this per *member*
  (`MethodLike::augmentation`, from the pickled `IsExtensionMember ∧ IsMember`
  bit; and FCS's full `IsMethInfoPlainCSharpStyleExtensionMember` — method
  attribute + a **non-generic** `Entity::is_extension_container`
  (`IsTyconRefUsedForCSharpStyleExtensionMembers`'s `isNil (tcref.Typars m)`) +
  exactly one argument group with ≥ 1 argument, scoped to non-module entities). Two shapes are **undecidable from
  IL alone**, and rather than guess (both guesses are wrong resolutions — hide a
  value FCS resolves, or surface a member FCS hides) the name enters scope but
  resolves to nothing (a deferral, D5):
  - an image whose pickle does not decode: fsc mangles an augmentation to
    `Type.Member`, but `[<CompiledName("A.B")>]` on an ordinary `let` is legal and
    yields the same IL name (`Augmentation::Possible`);
  - a *curried* `[<Extension>] static member M x y` in an **F#** assembly: FCS
    keeps it in scope (its predicate needs exactly one argument group), but an F#
    assembly's flattened IL signature cannot distinguish curried from tupled
    (`MethodLike::arg_group_count` is `None`). A Roslyn extension method always has
    one group, so C# assemblies are exact.
  Both are pinned as *known gaps* in `crates/sema/tests/extension_visibility_matrix.rs`,
  which asserts they stay deferrals and never become wrong targets.
- **An `open` of an assembly module/namespace folds a complete-or-opaque surface**
  (`docs/assembly-module-open-plan.md`, "the fold") — `open M` brings the module's
  values, its non-RQA unions' cases, exception constructors, active-pattern tags,
  nested type names, and its `[<AutoOpen>]` submodules' contents (recursively) into
  scope in FCS's fold order, and `open Ns` folds a referenced-assembly namespace's
  own tycon tier the same way. What stays conservative — each a deferral, never a
  wrong target, pinned by the `namespace_fold_matrix` ratchet:
  - union cases and active-pattern tags fold **opaque** (in scope, shadowing by
    position, naming nothing); the **type-qualified** reading (`UnionShape.UCaseB`,
    and `RqaShape.RqaA` — where `[<RequireQualifiedAccess>]` makes it the *only*
    reading) defers the same way, in expression and pattern position both;
  - a **namespace**-level exception constructor folds opaque too (§8 of the plan:
    FCS can re-order the bare name against a later constructible type's constructor
    slot, or against a same-surface `[<Literal>]` as a constant pattern, and
    bare-name lookup models neither eviction); a *module*-level exception still
    commits its entity — unless a later same-surface value **may be a literal**
    (the CLI `Literal` flag, or any `System.Decimal` field — a C# `const decimal`
    carries no flag, Q17), in which case it folds opaque and the pattern defers
    where FCS binds the constant (`demote_pattern_shadowed_exceptions`);
  - a name two assemblies (or the module and namespace halves of one FQN) both
    supply demotes per-name — reference order is not modelled;
  - name-unknown residue (an unknowable pickle, an undecodable member, a
    case-nameless union, an `[<AutoOpen>]` *type*) raises the generation barrier
    and demotes its group;
  - the cross-kind-type generation barrier (round 4) is coarser than FCS: a later
    open whose namespace half carries a type stales *every* earlier opened entry,
    so an unrelated earlier module-half value defers where FCS still binds it
    (dotted heads stay live per-head — `head_entry_staled` vetoes only a head whose
    own entry was staled, round 10);
  - record **labels** stay unmodelled, and a submodule or nested type as a
    *dotted head* (`open M` then `Sub.f` / `C.Stat`) defers (Slice B of the plan);
  - an opened `[<Literal>]` in **pattern** position reads as a fresh binder,
    where FCS reads a constant pattern (literal-ness is undetectable in
    general — Q17; the expression position binds it exactly).
  An `[<RequireQualifiedAccess>]` module is imported like any other (FCS reports
  FS0892 on the `open` and binds its contents regardless); we do not yet *emit*
  that diagnostic.

## MSBuild / fsproj

- **Single-pass evaluator** — does not iterate to a fixed point, so forward
  references (`$(Foo)` before `<Foo>`) are diagnosed where real MSBuild's pre-pass
  catches some. Deliberate for predictability. (`4aa7871`)
- **Well-known properties: path-derivable subset only** — SDK-dependent paths
  (`MSBuildBinPath`, `MSBuildExtensionsPath`, `MSBuildThisFile*`, …) are absent.
  (`4aa7871`)
- **In-body explicit SDK imports not repositioned** — an explicit
  `<Import Sdk="X" Project="Sdk.props|targets"/>` that promotion declines (because
  it's conditional or not the first/last element child) runs at its literal
  in-body position; the `Directory.Build.props` repositioning keys only on nested
  *root* `<Project Sdk=…>`, not in-body SDK imports, so MSBuild's order differs.
  (`6d87b41`, `bfbb307`)
- **Nested-SDK `Directory.Build.props` bootstrap fallback** — when the `<Import>`
  reaching a nested SDK is itself conditioned on a property that *only*
  `Directory.Build.props` sets, the walker falls back to the historical
  before-body splice (MSBuild would drop `Directory.Build.props` in this circular
  case); the resulting compile order diverges, but avoids emitting no
  `Directory.Build.props` at all. (`bfbb307`)
- **Glob expansion remains resolver-optional** — the core MSBuild crate stays
  filesystem-free and callers that pass no glob resolver still get
  `UnsupportedGlob` / `UnsupportedItemOperation`. The LSP and `.fsproj`
  diagnostics now pass the filesystem resolver, so runtime project evaluation
  expands supported globs. (`0531f24`, resolver wiring)
- **`@(…)` / `%(…)` references not evaluated** — item-list and metadata
  references in `Include` are diagnosed and stripped (MSBuild expands them); an
  unresolvable `@()`/`%()` in `Exclude` skips the whole item element, conservative
  to avoid over-inclusion. (`0531f24`)
- **Cross-root `rollForward` resolution** — first-satisfying-root-wins across
  `sdk.paths` roots, whereas the .NET host picks the highest satisfying version
  across all roots; doesn't affect `Sdk.props`/`Sdk.targets` lookup in practice.
  (`6c01892`)

## LSP

- **Project ownership: alphabetical only as a fallback** —
  `Workspace::owning_project` prefers the project whose evaluated `<Compile>`
  list contains the file (FCS's rule), climbing ancestor directories
  nearest-first. The alphabetically-first-filename heuristic
  (`find_owning_project`) takes over whenever the climb finds no conclusive
  owner: either **(a)** every ancestor project evaluated completely and excluded
  the file (e.g. a brand-new file not yet added to its project), or **(b)** the
  climb reaches a project whose membership is *inconclusive* — `membership`
  returns `Unknown` because the project's Compile-item set is untrustworthy
  (`items_uncertain` — narrowed at `97afac4` (#611) from the broad `is_partial`,
  which flipped on for essentially every real SDK project from its hundreds of
  harmless imported-target diagnostics; the FCS-faithful Compile-set rule now
  engages for real projects instead of always falling back) or it failed to
  evaluate at all, so its item list proves nothing either way. In
  case (b) the climb stops at that nearer project rather than risk preferring a
  farther one, **even if** a farther ancestor would cleanly list the file
  (pinned by `owning_project_does_not_climb_past_a_partial_nearer_project`). A
  *sibling* project that links a shared file it does not sit above is likewise
  never preferred (the workspace-wide ownership index was explored and shelved —
  see `docs/workspace-index-plan.md`). However ownership resolves, `symbols_for`
  uses the chosen project's `DefineConstants` — which, whenever the fallback
  fired, may be the wrong project's — and yields only the implicit symbol set
  for the file kind (e.g. `{COMPILED, EDITING}` for compiled files) when no
  ancestor `.fsproj` is found at all or the chosen one fails to evaluate.
  (`91ebdea`, `3087ae4`, `97afac4` (#611))

## Language-version pinning

The LSP now resolves each project's `<LangVersion>` and threads it through the
parse (`6e9377f`/#606 added the seam, `42265b3`/#613 wired it into the LSP):
`#elif`/FS3350 and nullness gates honour it and *match* FCS, so they are **not**
divergences (absent `<LangVersion>` → 10.0, FCS's default; orphan/non-file
buffers → `preview` to avoid guess-flagging). The gates below are *not yet*
version-aware — they are implemented at their latest-version form
unconditionally, so they diverge only when a project sets an older
`<LangVersion>`:

- **`relaxWhitespace2` offside grace treated as always-on** — the `+1` column
  bonus and the `MatchClauses :: CtxtMatch` undentation arm fire unconditionally;
  FCS gates them on `relaxWhitespace2` (F# 6.0+). (`1a730fd`, `c5e3676`)
- **`#nowarn` / `#warnon` argument forms accepted regardless of version** — FCS
  gates the unquoted and `FS`-prefixed forms on `ParsedHashDirectiveArgumentNonQuotes`
  (F# 9.0+); we accept quoted, unquoted, and `FS` forms always, so we diverge
  only at `<LangVersion>` ≤ 8. (documentation-only; PR #159)
- **Older typed-node feature pins are incomplete** — the interval gate covers
  nullness, but other typed-node features introduced below the currently modelled
  surfaces may still be treated as always-on until their interval rows are added.

## Oracle-side workarounds (`tools/fcs-dump`)

These compensate for lossiness in FCS's *public* API; they are not divergences in
our reimplementation.

- **`System.Object` abbreviation stripped** — FCS renders it via the `obj`
  abbreviation (`TryFullName`=None); the oracle strips `AbbreviatedType` layers.
  (`4f003eb`)
- **Accessibility approximations** — FCS collapses ECMA-335 `Family` on ctors to
  `Public` and `FamORAssem` to `Protected`, and reports `IsExtensionMember=false`
  for C#-style IL extensions (must also check `[ExtensionAttribute]`); the oracle
  reaches through reflection to raw IL. (`56cb8bc`, `3b8c85c`)
- **IL field projection** — FCS `FSharpFields` emits only `value__` for IL enums
  (empty for plain IL classes); the oracle walks raw `ILTyconRawMetadata.Fields`.
  (`088fdfc`)
- **IL property / event accessor accessibility hard-coded `Public`** — the oracle
  walks raw `ILTypeDef.Properties` / `.Events`. (`d6b55c1`, `bcebc40`)
- **No `IsEventRaiseMethod` predicate** — fire-accessor methods are filtered out
  explicitly to avoid a phantom method. (`bcebc40`)
- **`IsReadOnly` / `IsByRefLike` not typed properties** — the oracle walks
  `Attributes` by full name. (`7472e69`)
- **CAs on IL-backed field/property members not on the FCS surface** — the oracle
  reaches raw IL reflection. (`4776881`)
- **Method-parameter `NullableAttribute` stripped / reported `Oblivious`** — no
  route from the FCS view back to the `byte[]` encoding, so the oracle reads raw
  `ILParameter`/`ILReturn`. (Our reimplementation likewise pins F# record/
  exception field nullability to `Oblivious` to match.) (`5950da7`, `68c7b0c`)
- **Synthetic unit-parameter normalisation** — FCS surfaces nullary `let f () = …`
  with a synthetic `unit` param though IL has none; the oracle strips it. (This
  over-strips wildcard-unit params `let f (_: unit) = …`, an oracle limitation.)
  (`6641cd9`, `927667f`)
- **Constructor return normalised to IL truth** — `() -> Foo` (FCS symbol-level)
  vs `() -> System.Void` (IL); both sides report IL truth. (`af0d5b7`)
- **F# generic-entity fixtures skipped** — FCS's `FSharpGenericParameter` surface
  can't be projected to mirror IL *entity* typars (variance/constraint rows).
  Generic module-level `let` bindings, previously dropped on both sides for the
  same reason, are now projected on both: the fcs-dump side renders their
  method typars name-only from the FCS surface (invariant in IL; the constraint
  kinds module `let`s carry — SRTP, comparison — are IL-erased), failing loudly
  on an IL-visible constraint. Only *generic extension members* and
  IL-visibly-constrained generic bindings (the `array2D` flexible-`#seq`
  shape) stay elided (`is_unmirrorable_generic_module_method`), mirroring
  fcs-dump's rendering limits. (`42f1e4d`, `c6fd998`, pickle plan Slice C)
- **`[<Measure>]` kind agreement** — `entityKindString` checks `IsMeasure` before
  `IsClass`, else a measure type round-trips as `"Class"`. (`ffb2dd4`)
- **`CompilerFeatureRequiredAttribute` on fields/properties not mirrored** — FCS
  exposes these only as raw IL blobs (not decoded attributes), so the oracle fails
  loud rather than silently diverge from the (correct) Rust projection.
  (`2a52321`)
- **CST diff-normaliser `_argN` binding-head gap** — the `normalised_ast.rs`
  normaliser shares FCS's `SynArgNameGenerator` per module decl, but binding-head
  patterns (`let f 0 = …`) don't yet consume it, so a non-simple arg in a binding
  head mis-numbers an RHS lambda (no test exercises this). (`5595b91`)

## Resolved since introduction

Documented early, since closed — listed so readers don't chase ghosts.

- **Lex-filter multi-line cursor:** the line/column cursor counts the `\n`
  bytes *inside* each token (mirroring FCS's `incrLine` on `newline = '\n' |
  '\r' '\n'`, `lex.fsl:315`), so block comments, triple/verbatim strings, and
  `\`-newline-continuation strings advance it correctly — not just the
  standalone `Newline` token. A lone `\r` is not a break, matching FCS.
  (`d9c28cb`)
- **Lex-filter offside gaps, all ported:** `use`/`CtxtLetDecl` (`isUse`),
  `isLetContinuator`, `do!`/`CtxtDo` and the `done`-terminator balancing,
  `isWhileBlockContinuator`, for/while comprehension arrows, `struct … end` /
  `interface … end` blocks, the `CtxtWithAsAugment` / `CtxtInterfaceHead` /
  `CtxtMemberHead` / `CtxtMemberBody` family, the `isSemiSemi` short-circuits, the
  `relaxWhitespace2OffsideRule` `+1` grace, and `END` as an if-block continuator
  (`RPAREN` is omitted but provably inert). (`f41e4a5`, `2cada1d`, `8d4b192`,
  `a6233b7`, `904556c`, and the rows-97–132 completion work)
- **Lexer:** interpolated strings implemented in all four FCS shapes —
  single-quoted (`$"…"`), triple-quoted (`$"""…"""`), verbatim (`$@"…"` /
  `@$"…"`), and extended bracket-count (`$$"""…"""`, `$$$"""…"""`, …) — including
  multi-fill (`$"a={x}b={y}"`) and nested (`$"x={ $"y" }"`) forms with the
  FS3373/FS3374 nesting diagnostics. (`df79cde`, `3456b8d`, `b39a6cc`,
  `1f2e45e`, `73a19c3`)
- **Parser:** byte-string literals typed as `SynConst.Bytes`; `elif` / no-else /
  `else if` chains; `HighPrecedenceTyApp` and postfix type application;
  `SynType.LongIdentApp` (the `atomType DOT path [<…>]` path); the integer
  out-of-32-bit-range diagnostic; the fun-lambda `_argN` counter now mirrors FCS's
  per-definition `SynArgNameGenerator`. (`dce0b8e`, `14b820a`, `b47be74`,
  `0cab760`, `5595b91`)
- **Parser (typed-shape gaps):** adjacent `f(x)` atomic application is recorded
  via `HIGH_PRECEDENCE_PAREN_APP_TOK` / `AppExpr::is_atomic`; plain `use`
  bindings carry the `isUse` distinction; struct tuple types and patterns,
  unit-of-measure slash segments, measure-power type application, `JoinIn`, and
  SRTP `Or` / type-alternative forms have differential coverage.
- **Parser (sign-folding):** an adjacent `+`/`-` before a numeric literal folds
  into the literal token (`crates/cst/src/parser/sign_fold.rs`, a parser-input
  pass mirroring FCS's `LexFilter.fs:2694`), so `-1` is `Const(Int32 -1)` not
  `App(~-, 1)`. A token-layer transform with no *grammar* change, it covers
  expression, pattern, argument, and paren positions (with matching
  folded-literal recognition in the pattern-start lookahead gates for
  continuation/nested positions like `Some -1`, `let f -1 = …`). The adjacency
  guard ports FCS's `isAtomicExprEndToken` rule (`LexFilter.fs:394`) against the
  raw stream, so `f-1`/`x-1` stay infix and spaced `- 1` stays a prefix op. The
  signed-int `MinValue` rescue mirrors FCS's `isInt*BadMax` — value-based for
  int8/int16 (so `-0128y` is rescued), exact-string for int32/int64/nativeint
  (so leading-zero `-02147483648` still overflows). Hex-bit-pattern floats fold
  by bit-casting and flipping the IEEE sign bit; range-adjacent forms (`-1..2`)
  fold after the lex-filter splits `INT32_DOT_DOT`. The genuine-overflow
  recovery-value divergence is kept under [Parser](#parser). (`2c511cd`)
- **Parser (curried binding / lambda argument patterns):** three
  `a5b00d8`/`67180b8`-era divergences are closed (verified against the current
  tree 2026-05-31). Paren/tuple/typed args parse in *any* curried position
  (`let f a (b: int) = b`); `as`-patterns parse in argument position
  (`fun (x as y) -> y`); curried paren-constructor-app args no longer over-reach
  (`fun (Some x) (Some y) -> …` keeps two distinct paren args, the head sweep
  stopping at the LexFilter-swallowed `)`, `pat.rs:144-165`). The only
  `a5b00d8`-era gap still open is the dotted longident head `fun X.Y -> …`, kept
  under [Parser](#parser). (`a5b00d8`, `67180b8`)
- **Parser (sequential expressions in offside bodies):** the `let` /
  function-binding RHS parses a multi-statement body as `SynExpr.Sequential`
  instead of draining it as ERROR (`let x = a; b`, multi-line offside bodies),
  closing the let-RHS divergence. The four duplicated offside-block gather loops
  (`if`/`then`/`else`, `fun` body, `match`-clause result, `let!`/`use!` body)
  were unified into `Parser::parse_seq_block_body`, which also handles explicit
  `;` (so `fun x -> a; b`, `match … -> a; b`, `if c then a; b` sequence). The
  parenthesised body routes through the same gatherer, so `(a; b)` is
  `Paren(Sequential(a, b))` (FCS's `parenExprBody` is a full
  `typedSequentialExpr`, `pars.fsy:5531`), and `(a; b : int)` →
  `Paren(Typed(Sequential, int))`. (`ecdfca4`)
- **Parser (top-level `;;` and `;` separators):** the impl-file / module-body
  decl loop (`Parser::parse_module_decls`) accepts `;;` as a top-level
  declaration separator (`topSeparator: SEMICOLON_SEMICOLON`, `pars.fsy:6967`),
  emitting an inert `SEMISEMI_TOK` and clearing pending-separator state so the
  following decl parses cleanly (fixing not just `let x = a;; let y = b` but the
  `open`/`type`/expr cases that used to cascade into spurious errors). The single
  top-level `;` is also accepted (`ec984e6`, #605): `open X;`, `open X; open Y`,
  `a; b`, `let x = 1;` parse as FCS does — an inert `SEMI_TOK`, gated on
  offside-block `depth == 0`. A *leading* `;`/`;;` stays an error, matching FCS
  (`topSeparators` only follows a `moduleDefnOrDirective`). Two single-`;`
  residual divergences remain: (i) `module M = N; <sibling>` — FCS recovers
  `ModuleAbbrev` + the sibling, but a faithful fix needs the abbreviation body's
  block extent reworked, so we instead **fail loudly and contained inside `M`**
  rather than reparent the siblings; and (ii) `let x = a; let y = b` stays out of
  scope — the first binding's `typedSequentialExpr` RHS swallows the `;`, so it is
  one let-in-sequential decl, not two. Verified against the FCS `ast` oracle
  2026-06-01 (`;;`) and 2026-06-28 (`;`).
- **Assembly:** F# entity kinds (Module/Union/Record/Abbreviation/Exception) via
  `CompilationMappingAttribute`; same-assembly attribute decoding; `[<Struct>]`
  DUs and primary-ctor struct classes; the `where T : unmanaged` constraint;
  pickle `TType_forall`, local `u_tcref`, and SRTP `MayResolveMember`; indexer
  projection with index-parameter nullability read from the getter; measure-type
  projection. (`e2dfd3f`, `5f5b992`, `ffb2dd4`)
- **Assembly (`TypedReference` / byref-like intrinsics):** the signature decoder
  models the token-free `ELEMENT_TYPE_TYPEDBYREF` (`0x16`) element as a dedicated
  `TypeSig::TypedByRef`, projected to the `System.TypedReference` value type — the
  same unification FCS applies (`ilread.fs:2671`). The element names no assembly
  on the wire, so it is attributed to the image's core-library `AssemblyRef`
  (well-known name: `System.Private.CoreLib`/`mscorlib`/`System.Runtime`/
  `netstandard`); `None` (same-assembly) only when the image *is* corlib. An
  image that carries a `typedref` yet neither references nor is a core library is
  refused (correctness over availability) rather than misrepresented as a
  same-assembly `TypeDef` — only malformed metadata hits this. This closes the
  whole `TypedReference` row of the coverage-gaps table
  (`System.TypedReference`/`ArgIterator`/`RuntimeArgumentHandle`-typed members).
  The `fcs-dump` oracle needed a matching fix: FCS's `FSharpEntity.IsByRef` is
  `true` not only for real `byref<T>`/`inref`/`outref` but *also* for those three
  zero-arg byref-like intrinsics, so `isRealByref` gates the byref branch on
  exactly one generic argument, and a zero-arg `IsByRef` entity renders as its
  plain named type (passed by value). `ArgIterator`/`RuntimeArgumentHandle` were
  never refused on the Rust side (ordinary `VALUETYPE <token>` refs); they only
  tripped the oracle.
- **Assembly (byref fields / byref-returning properties):** an outer
  `ELEMENT_TYPE_BYREF` on a field or property type — a `ref` field in a `ref
  struct`, or a `ref`-returning property/indexer (`Span<T>.this[i]`,
  `List.ValueRef`) — is kept as `TypeRef::ByRef`, exactly as a `ref` method return
  already was. `project_field`/`project_property` route through the shared
  `walk_byref_position` (also used by `project_return`): the byref wrapper is
  never annotable, so the referent is walked under the position's `[Nullable]`
  and re-wrapped, rendering `T&{suffix}` (`ref string?` → `System.String&?`). The
  `fcs-dump` oracle's `renderPositionTypeWithByref` unifies fields/properties onto
  the return convention. **Still refused:** byref *event* delegate types, byref
  F# record fields, and a byref-to-byref referent (all malformed or nonsensical;
  `walk_byref_position` keeps the last fail-loud). (`ref readonly`
  fields/indexers, once refused here as a `modreq` gap, are now modelled — see
  the custom-modifiers entry below.)
- **Assembly (`init`-only setters):** a C# 9 `init` accessor compiles to a
  `set_X` whose *void* return carries
  `modreq(System.Runtime.CompilerServices.IsExternalInit)`. The decoder used to
  reach the trailing `VOID` where a type is required and refuse it
  (`SigError::UnexpectedVoid`), sinking the whole property; the return position
  now carries its modifier run like any other (`RetType::Void(mods)`). The
  projector accepts a modified `void` only on a **property setter** and only when
  its single modifier is `IsExternalInit`; the same marker on any other method, a
  getter, or an event accessor is refused. The setter then projects as a plain
  void accessor, so the property recovers with `has_setter`. Neither the model
  nor FCS's IL-property view distinguishes `init` from `set`, so it reads as an
  ordinary `get;set;` on both sides (the `fcs-dump` oracle's
  `validateAccessorReturnType` matches). This leaves only the pathological
  modified-`void` shapes no real compiler emits (see the
  `modreq`/pointer-before-`void` coverage-gap row).
- **Assembly (custom modifiers — `modreq`/`modopt`):** the reader decodes both
  modifier bytes and the projector applies **ECMA-335 II.7.1.1** to them: *an
  optional modifier may be ignored by a tool that does not understand it; a
  required one must be understood.* So an unrecognised `modopt` is dropped and an
  unrecognised `modreq` is refused **by name**, rather than the previous blanket
  refusal of every modified signature; `SigError::UnsupportedModifier` is gone —
  policy lives in the projector, which is where the type *names* are.

  **The modifiers live on the position, not in front of the type.** A signature
  position is `ModifiedType { mods: Vec<CustomMod>, ty: TypeSig }` — ECMA-335's
  own `CustomMod* Type` — so `mt.ty` *is* the head and there is nowhere for a
  modifier to hide. A `TypeSig::Modified` *wrapper* variant would be a trap the
  compiler cannot see: a guard written `matches!(sig, TypeSig::ByRef(_))` stays
  well-typed but silently stops firing once a modifier node sits in front of the
  byref. The drop-`modopt`/refuse-`modreq`-at-*every*-position policy is checked
  as a metamorphic property over real assemblies
  (`crates/assembly/src/modifier_metamorphic.rs`): decorate every signature node
  with an unrecognised modifier and re-project — a `modopt` must move nothing, a
  `modreq` must leave no member standing. (FCS is laxer: `import.fs:305` ignores
  both kinds, which would silently mismodel a `volatile` field as an ordinary
  one.)

  Exactly **two** `modreq`s occur across the whole .NET 10 runtime + ref pack, and
  both are understood:

  - `modreq(System.Runtime.InteropServices.InAttribute)` over a byref — a
    **read-only reference** (C# `in` / `ref readonly`, F# `inref<'T>`) →
    `TypeRef::ByRef { readonly: true }`, or `Parameter::is_readonly_ref` for a
    parameter. Read-only-ness has *two* metadata encodings and the model unions
    them: the `modreq` goes in the signature for a byref **return** (and the
    property/indexer type mirroring it) and for an `in` parameter of a
    **virtual/abstract/interface** member; everywhere else — an `in` parameter of
    an ordinary method, a `ref readonly` **field** — the signature is a *plain*
    byref and the fact rides an `[IsReadOnly]` / `[RequiresLocation]` attribute
    (`has_readonly_ref_attribute`). Reading only the modifier would make the same
    source project as `inref` on a virtual member and a writable `byref` on an
    ordinary one.
  - `modreq(System.Runtime.CompilerServices.IsVolatile)` on a **field** type — the
    sole encoding of C# `volatile` → `Field::is_volatile`. Recognised *only* on a
    field: the same marker on a parameter, property or return is refused.

  A modifier consumes no `[Nullable]` byte (Roslyn's encoder walks past it, as
  does FCS's `ImportILTypeWithNullness`), so the peel happens before the nullness
  walk and `in string?` / `volatile string?` keep the annotation on the referent.
  The `fcs-dump` oracle moved in lockstep: `peelIlModifiers` mirrors the II.7.1.1
  rule and the two-encoding union. **Effect:** the 10.0.9 shared runtime's member
  drops fell from 662 to 265 (the modifier bucket was 397 and is now empty) and
  the ref pack's from 162 to 142; the `bcl_ref_pack_sweep` budget ratchets
  200 → 180.

- **Assembly (F#-native extension-member flag from the pickle):** the
  `is_extension_method` flag for F#-native module augmentations
  (`type T with member …`) was set by an IL-name heuristic — a single-dot
  `<Type>.<Member>` module-method name taken to be an instance extension. That
  mis-flagged a plain module `let` whose `[<CompiledName>]` contained a dot
  (`[<CompiledName("A.B")>] let f x = x` compiles to an IL method literally named
  `A.B`, indistinguishable in pure IL from a genuine `Counter.Tripled`
  augmentation — F# emits no `[ExtensionAttribute]` on these). The reader now
  reads the authoritative `IsExtensionMember` bit (`ValFlags`, `TypedTree.fs:192`)
  from the host CCU's signature pickle. Since the pickle member-list cutover
  (`docs/completed/fsharp-pickle-member-projection-plan.md` Slice C) the carrier
  is `apply_module_member_projection`: it walks the pickle entity tree, locates
  each module's ECMA TypeDef by FQN, then rebuilds the member list by letting each
  val *claim* its projected IL method by compiled name (breaking a name collision
  by compiled arity), stamping the claimed member with the val's
  `IsExtensionMember ∧ IsInstance` verdict. Generic vals claim their generic IL
  methods too, so generic F#-native extensions are flagged (the former §7 gap).
  The match is **scoped to the declaring module**, so a
  `[<CompiledName("Counter.Tripled")>] let` in one module is not mis-flagged when
  a *different* module genuinely augments `Counter.Tripled`. Correctness envelope:
  the overlay runs only when the host pickle describes the whole image (a
  single-CCU assembly); when no host pickle decodes, **or** the image is an
  `fsc --standalone` build embedding foreign CCU pickles, projection keeps the
  IL-name heuristic (`foreign_signature_data_present` is the gate). One narrow
  residual remains and is out of scope: when an in-module compiled-name collision
  is *also* an arity collision whose vals *disagree* (an instance extension and a
  non-extension `let` sharing both `[<CompiledName>]` and compiled arity), the
  pass **under-sets** (declines to assert the bit) rather than over-flag; an
  all-extension same-arity overload set keeps its unanimous flag. Closing it needs
  signature-level (not just arity) matching, the `PickledType → model` bridge the
  deferred merge work will build anyway. (`6215f4c`, arity-disambiguation slice)
- **Assembly (`typeof<…>` attribute-argument decode):** the `u_expr` decoder
  (`crates/assembly/src/fsharp_pickle/expr.rs`) was constrained to `Expr.Const`
  (tag 0) and hard-errored on every other arm; because attributes are decoded
  eagerly, a single non-constant attribute argument *anywhere* in the host
  signature pickle failed the whole CCU decode, so enumeration recorded skipped
  F# overlays and any `[<Measure>]` type stayed at its IL truth,
  `EntityKind::Class`. The decoder now models the attribute-argument subset a
  real fixture trips: `Expr.Val` (tag 1, the `typeof` intrinsic head) and
  `Expr.App` (tag 6, `typeof<int>`). Other `u_expr` arms keep the
  loud-error-then-recorded-overlay behaviour — see
  [Assembly reader](#assembly-reader-model--owned-ecma-335-backend). (`ffb2dd4`)
- **Assembly (`Expr.Op` attribute-argument decode — array + coercion):** the
  `u_expr` tag-2 `Expr.Op` arm now covers the two `Expr.Op` shapes FCS's
  `CheckAttribArgExpr` (`PostInferenceChecks.fs`) admits in attribute position:
  `TOp.Array` (`u_op` tag 19, the array literal `[<Attr([| … |])>]`) and
  `TOp.Coerce` (`u_op` tag 15, a transparent up-cast reached when a constructor
  parameter is typed `obj`). Like the `typeof` gap, a single such argument
  previously failed the whole CCU decode. Operands recurse through the same
  attribute-argument subset (a `typeof<T>[]` nests a tag-6 `App` inside the
  array); every other `u_op` tag raises `UnsupportedPickleTag`. With this the
  decoder is *complete* for `CheckAttribArgExpr`: its remaining shapes
  (`typeof`/`typedefof`, enum conversion, bitwise-or) are all `Expr.App` and land
  via the tag-6 arm — which is why the original divergence's "enum bitwise-or"
  example never tripped (such arguments constant-fold to `Expr.Const` or survive
  as a tag-6 `App`). (`ffb2dd4`)
- **Assembly (`u_const` complete tag set):** the `u_const` decoder
  (`crates/assembly/src/fsharp_pickle/consts.rs`) handled only
  Bool/Int32/String/Unit/Zero (tags 0/5/14/15/16) and hard-errored on the other
  thirteen. Since `read_const` backs every `[<Literal>]` val and record field as
  well as attribute-argument `Expr.Const` nodes, a single literal or constant
  attribute argument of any other type anywhere in the host signature pickle
  failed the whole CCU decode, leaving every `[<Measure>]` type at
  `EntityKind::Class` and falling back to IL heuristics for extension flags. The
  decoder now mirrors the *complete* FCS dispatcher
  (`TypedTreePickle.fs:3394-3416`, tags 0–17). The one asymmetry is preserved:
  tag 2 (`Byte`) reads a *raw* `u_byte`, not the compressed `u_int32` its integer
  siblings use. Floats keep the wire's raw IEEE bits (so the tree stays `Eq`-able
  and constant identity is bit-exact), `Char` is a raw `u16` (a lone UTF-16
  surrogate survives), `Decimal` is the four `System.Decimal.GetBits` words. Tags
  outside `0..=17` still hard-error, matching FCS's `ufailwith`. Complete — no
  residual. (`20dd226`)
- **Assembly (owned ECMA-335 reader):** the crate now uses an in-crate ECMA-335
  reader (`crates/assembly/src/ecma335_assembly.rs` + `reader/`, public view
  `Ecma335Assembly`), which closed two divergences driven by bugs in the
  previous reader:
  - **Long custom-attribute strings no longer degraded** — the owned `SerString`
    length read handles the ≥128-byte band correctly, so `ObsoleteAttribute` /
    `ExperimentalAttribute` payloads keep their full text instead of degrading to
    presence-only. `SAFE_CA_STRING_LEN` is gone. (`737d784`, `99cf605` →
    `6376b74`)
  - **`NullableAttribute(byte[])` composite form decoded** — the member-position
    nullability walk consumes the per-position vector encoding (one byte per
    annotable node, with a length-mismatch structural error), not just the
    scalar form. (`68c7b0c`, `b72338b`, `543591e`)
- **Assembly (`allows ref struct` typar anti-constraint):** the owned ECMA-335
  reader decodes the `AllowByRefLike` (`0x0020`) `GenericParam` flag bit — FCS's
  `ILGenericParameterDef.HasAllowsRefStruct` — into the `TypeParameter::allows_ref_struct`
  slot, rendered by both the diff normaliser and `fcs-dump` as the additive
  `allows ref struct` constraint token. It is an independent anti-constraint
  (orthogonal to the value-type bit), so no additivity guard. The F#-*pickle*
  reader already modelled the same notion separately (`AllowsRefStruct`, B-stream
  tag 2). This is the flag-bit form; the *attributed*-constraint form of
  `allows ref struct` is still refused — see the coverage-gaps table.
  (`a857b6a`, `2a52321`; owned-reader cutover `6376b74`)
- **MSBuild:** explicit `<Import Sdk="…">` support; `Sdk.props` →
  `Directory.Build.props` ordering corrected, including position-faithful
  `Directory.Build.props` for nested SDK roots; empty global properties are
  sticky, so `ImportDirectoryBuildProps=""` suppresses the implicit import.
  (`bfbb307`)
- **LSP:** platform-qualified TFM suffix (e.g. `-windows`) preserved.
- **Directives:** `#nowarn` / `#warnon` / `#line` arguments now parsed into
  structured payloads, and diagnostics now use generated→virtual `#line`
  remapping. Warning suppression and inverse virtual→generated mapping remain —
  see above. (PR #157)
- **Parser (conditional-compilation-aware parsing):** the parser no longer
  parses the raw token stream across every `#if` branch. `parse_with_symbols`
  drives the full-trivia preprocessor with the project's `<DefineConstants>` and
  feeds the grammar only the **active**-branch tokens; inactive regions and
  directive lines are kept as trivia for losslessness but never parsed, and
  structural directive errors are dropped from the parser's own stream (the LSP
  owns them — see below). So no structural-error squiggles land in inactive
  branches, and the active-branch AST matches FCS (pinned by `parser_diff_ifdef.rs`).
  (`717ae5e`, `bc646b5`, `41e0427` (#260))
- **Parser (uppercase atomic-pattern classification):** the `SynPat.LongIdent`
  vs `Named` split was promoted from an approximate hand-rolled rule to an exact
  BMP mirror of FCS's parse-time classifier
  `String.isLeadingIdentifierCharacterUpperCase` (`ident_text_leads_uppercase`,
  `parser/classify.rs`): backtick-stripped, BMP-only like .NET's UTF-16
  `Char.IsUpper`, with Rust's derived Unicode properties corrected (subtracting
  `Other_Uppercase`, `Other_Lowercase`, `Nl`, BMP `Other_Alphabetic`
  non-letters). Uppercase head → `SynPat.LongIdent`, else `SynPat.Named`; the
  constructor-vs-binding question is deferred to name resolution. Also covers
  FS0623 active-pattern case-name validation. (`f16a201` (#150), `d0952fd` (#279))
- **LSP / Directives (structural preprocessor errors surfaced):** every
  non-lex `PreprocError` variant — `UnmatchedEndIf`, `OrphanElse`, `OrphanElif`,
  `DoubleElse`, `ElifAfterElse`, `UnclosedIfAtEof`, and malformed directive
  shapes (`Directive`) — is now emitted as an LSP diagnostic (`diagnostics.rs`).
  The parser still drops these from its raw stream (their bytes are covered by the
  directive's trivia token), but the LSP's lexer producer surfaces them, so the
  product no longer silently drops preprocessor errors. (`f27de96` (#175))

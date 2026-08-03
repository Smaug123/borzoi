# Telling the user why a project went quiet

When the LSP evaluates a `.fsproj` and cannot trust the result, it stops serving
project-wide features for that project and falls back to single-file resolution.
The user sees go-to-definition, cross-file references and imported-assembly
lookups simply stop working, with no explanation. This plan closes that.

## The defect, measured

`server::warn_compile_uncertainty` already exists and already sends a
`window/showMessage`. It reads exactly one of the evaluator's cause channels:
`ParsedProject::compile_condition_uncertainties`.

A census over **a 401-`.fsproj` sample** under `~/Documents/GitHub` (a bounded
walk — the cap is why it is a sample, not the population — evaluated through
`Workspace::project`, i.e. the LSP's own runtime path, 2026-08-03):

| axis | projects |
| --- | --- |
| evaluation failed outright | 0 |
| `items_uncertain` | 3 |
| …of which `compile_condition_uncertainties` non-empty | **0** |
| …of which `compile_item_uncertainties` non-empty | 3 |
| `define_constants_uncertain` | 0 |
| `project_references_uncertain` | 3 (the same 3) |
| `package_references_uncertain` | 3 (the same 3) |

So on this corpus the fallback fires three times and the message fires **never**.
The existing warning is not merely incomplete; it is empirically inert. The
three causes were all legible ones a user could act on:

- `UnsupportedCondition { "Exists($([MSBuild]::GetPathOfFileAbove('Directory.Build.props', …)))" }` (×2)
- `SdkNotFound { "Microsoft.Build.NoTargets/1.0.80" }`

The frequency also settles the noise question: at 3/401 a toast is not spam.

Rendering those same three projects (plus `ApiSurface`, which the sample's cap
had excluded and which turns out to defer for the same reason) through the new
code gives, e.g.:

> `analyzers.fsproj`: falling back to single-file analysis: go-to-definition,
> find-references and hover won't see other files in this project or its
> referenced assemblies — why: the SDK `Microsoft.Build.NoTargets/1.0.80` isn't
> installed where I can find it; a condition I can't evaluate:
> `Exists($([MSBuild]::GetPathOfFileAbove('Directory.Build.props', …)))`, in an
> imported file. …

Each names a construct the user can go and look at. The *guarantee* that a
decline always renders something is not carried by this census, which is a
sample: it is carried by `a_declined_capability_always_produces_a_message` in
`crates/lsp/src/project_deferral/tests.rs`, a property over generated flag and
cause combinations.

## Why it happened, and the structural fix

The decision to defer and the decision to explain were computed in two different
places from two different inputs — `semantic::build_parses` gates on
`items_uncertain || define_constants_uncertain`, while the message gates on a
cause vector that neither flag implies. Two predicates that must agree, kept in
agreement by discipline, and they did not agree.

The fix is one function. `project_deferral::deferrals` maps a project evaluation
to the list of capabilities the LSP declines and why; `build_parses` asks it
whether to decline, and the server asks it what to say. They cannot disagree
because there is only one of them.

**The input has to be wide enough for that to mean anything.** The first cut of
this took a `&ParsedProject`, and two declines promptly escaped it — a review
found both:

- `workspace::references_suppressed` also drops edges when `not_an_inner_build`
  (the outer dispatch build of a multi-targeted project), which *no evaluator
  flag records*: a multi-targeted document never writes the singular
  `TargetFramework`, so it decides `'$(TargetFramework)' == ''` perfectly
  cleanly. So `ProjectEvaluation::Evaluated` carries it, `drops_reference_edges`
  is the one predicate both the graph walk and the message use, and
  `Workspace::project_evaluation` is the only supported way to build the value.
- `build_parses` has four more exits *after* the evaluation gate, reached only by
  reading and parsing the Compile items: an unreadable item, a parser panic, an
  F# 8 shape straddle, an unexpected parse root. It now returns
  `Result<_, FoldRefusal>` rather than `Option`, so every exit is a value
  `deferrals` can fold into the same capability. `SemanticState::fold_refusal`
  hands the verdict to the server (caching the *success*, never the refusal — a
  refusal turns into a success the moment a file appears, and a cached negative
  would need its own invalidation hook on every such event).

Both were the original defect wearing a different hat: a decline computed
somewhere the explainer couldn't see. The lesson is the input type, not the two
patches — a narrow one lets the next such decline escape silently too.

**And wide enough in time, not only in data.** A second review round found the
same shape once more: a refusal introduced *after* the file was opened (a
sibling Compile item deleted, an edit that makes one straddle the F# 8
indentation boundary) was reached only from a request handler, which has no
connection to the client. `SemanticState::fold` — now the single place the fold
runs — records every refusal, and the shell drains it
(`flush_observed_fold_refusals`) *before* enqueueing the reply, since the refusal
is precisely why that reply is degraded. Reporting on the keystroke instead would
mean folding the project on every edit just to check, and would toast before
anything had actually failed.

The dedup moved with it: keyed on the **message text** rather than the project,
so "don't re-toast the same problem" and "do report a new problem" are one rule
instead of two that must be kept in agreement. A project with nothing to report
is forgotten, so one that is fixed and later re-broken reports again. The key is
canonicalised — the same project arrives spelled `/var/…` from a buffer and
`/private/var/…` from a drained refusal, and keying those apart sent one
project's message twice (caught by the e2e tests, not by inspection).

Two shapes carry the rest of the enforcement:

- **A deferral's `Causes` are a two-armed enum**, `Recorded(Vec<String>)` /
  `Unrecorded`, built only through a private constructor that collapses the
  empty vector to `Unrecorded`. A set flag whose cause vector is empty therefore
  cannot silently render as a blank "why" — it renders as an explicit stated
  absence, and a caller can tell the two apart by matching rather than by
  reading prose. Distinguishing "no cause" from "we didn't look" is the
  recurring defect class in this repo, so it is made unrepresentable here rather
  than documented.
- **Cause rendering is a wildcard-free match** over the msbuild cause
  vocabulary, and the test that supplies one sample per variant is wildcard-free
  too. Adding a variant to `borzoi-msbuild` is then a compile error in both
  places, not a silently-unrendered cause.

## Scope

Capabilities reported (each has a live consumer that declines today):

| capability | trigger | consumer that declines |
| --- | --- | --- |
| `ProjectFold` | evaluation failed, `items_uncertain`, `define_constants_uncertain`, or any `FoldRefusal` (unreadable Compile item, parser panic, F# 8 shape straddle, unexpected parse root) | `semantic::build_parses` → single-file fallback |
| `ProjectReferenceEdges` | `project_references_uncertain` **or** `not_an_inner_build` | `workspace::references_suppressed` → `ProjectNode::references_uncertain` → edges dropped from the `AssemblyEnv` |

`package_references_uncertain` is **not** reported: it has no consumer in
`crates/lsp` (grep), so nothing is declined and there is no user-visible loss to
explain. Reporting it would be a claim we cannot back.

Delivery is `window/showMessage`, extending the existing channel rather than
adding a second one. Publishing the causes as `.fsproj` diagnostics anchored on
their spans is a natural follow-up (the spans are already carried) but is not
this change: it would move the `.fsproj` diagnostic set, which has its own
served-TFM semantics (E7, `fsproj_diagnostics.rs`).

## Changes

1. **`borzoi-msbuild`** — add `ParsedProject::define_constants_uncertainties`,
   the define-axis twin of `compile_item_uncertainties`. That axis is set at
   exactly one site (`State::push` under `define_context`) and recorded no cause
   at all, so it was the one axis that could not answer "why" even in principle.
   One site means the invariant `define_constants_uncertain ==
   !define_constants_uncertainties.is_empty()` holds by construction; a test
   pins it, as does the weaker `items_uncertain ⟹ some compile cause` for the
   pre-existing axis.

2. **`crates/lsp/src/project_deferral.rs`** (new, pure, no IO) — `ProjectEvaluation`
   (evaluated / failed), `DeferredCapability`, `Deferral`, `deferrals`,
   `deferral_message`, and the cause renderers.

3. **Wiring** — `semantic::build_parses` gates through `deferrals`;
   `server::warn_compile_uncertainty` becomes `warn_project_deferral` and reads
   it; a `.fsproj` buffer opening now reports its own project (previously only
   `.fs`/`.fsi`/`.fsx` could trigger it, so opening the offending file itself
   said nothing); and a structural `didChangeWatchedFiles` clears the
   already-warned set, since the project is re-evaluated and may defer for a new
   reason.

## Known incompleteness, and what would close it

`project_references_uncertain` is raised at ~13 sites in `borzoi-msbuild`, none
of which records a cause — unlike the Compile axis (`compile_item_uncertainties`)
and, now, the define axis. The `ProjectReferenceEdges` deferral therefore borrows
the Compile axis's *structural* causes, which are true reasons the reference list
can't be trusted (a followed-through import can carry `<ProjectReference>`
mutations) and are raised by a site that flips both axes. Where none exists it
reports `Causes::Unrecorded`.

So that capability's "why" is **sound but possibly short**: an item-pass site of
its own (a `Remove`, an unevaluable Include) that co-occurs with a structural
Compile cause is not named. Closing it means giving the axis its own cause
channel, exactly as change 1 does for the define axis — 13 sites rather than 1,
which is why it is not in this change. The stated-absence arm is what keeps the
gap visible instead of letting it read as "nothing else was wrong".

Short is the cost; *wrong* would not be acceptable, and review found one. The
borrowed subset is `StructuralCompileItemUncertainty::hides_project_references`,
the evaluator's own rule, not "every structural cause": `UnsupportedChoose` is
deliberately exempt there (`handle_choose` scans a `<Choose>`'s still-possible
branches for reference mutations itself), so a Compile-only `<Choose>` alongside
an unrelated `<ProjectReference Remove>` would otherwise have been named as the
reason for a drop it did not cause. A confidently wrong explanation is worse
than the stated absence it displaces.

## Bounded output

A project can accumulate many causes. The message renders at most
`MAX_RENDERED_CAUSES` of them and states the residual count explicitly; the full
list goes to `tracing` at `warn`. A silent cap would read as "that was all of
them".

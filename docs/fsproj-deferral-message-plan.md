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

**And the reporting must be derived, not dispatched.** Two further review rounds
found four more bugs, every one the same shape: a decline reached through a path
that had no notify call on it.

- A refusal introduced *after* the file was opened — a sibling Compile item
  deleted, an edit that makes one straddle the F# 8 indentation boundary — was
  discovered only inside a request handler, which cannot talk to the client.
- A `.fsproj` opened clean and then edited reported nothing: only `didOpen`
  called the reporter, so the buffer-aware path worked exactly once per file.
- A fold that refused, recovered, then refused the same way was silent the
  second time, because the recovery cleared the fold's own record but not the
  server's sent-message record.

Patching each site would have been four more `warn_…` calls and a fifth bug
waiting. Instead the reporting became a **refresh over current state**:
`server::refresh_project_deferrals` recomputes every in-scope project's
explanation and diffs it against what was last sent, and the shell calls it after
every dispatched message. Nobody has to know which state changes can alter a
deferral. Started deferring → reported; stopped → forgotten (so re-breaking
reports again); different reason → replaces the old one: all three are the same
diff.

Two supporting shapes:

- `SemanticState::fold` is the one place the fold runs and records its outcome;
  `observed_fold_refusal` *reads* it rather than draining, which is what makes
  the message a pure function of state. The refresh never provokes a fold —
  folding every project on every keystroke would answer a question nothing has
  asked — so `ensure_folded` runs once on `didOpen` and handlers record the rest.
- The dedup is keyed on the **message text**, canonicalised path. Keying by
  project alone cannot express "same problem, don't repeat" and "new problem, do
  report" at once; not canonicalising sent one project's message twice, because
  it arrives spelled `/var/…` from a buffer and `/private/var/…` from the
  semantic layer (caught by the e2e tests, not by inspection).

The refresh runs *before* the reply is enqueued, since the refusal is precisely
why that reply is degraded and an explanation arriving afterwards reads as
unrelated.

### Cut: describing an unsaved `.fsproj` buffer

The refresh originally described an open `.fsproj` from its **buffer text**, so
an unsaved edit would toast immediately. A fourth review round produced five
findings and four were that one feature: a full import/SDK walk on every
dispatched message; the buffer evaluation *suppressing* a real fold refusal
(nothing in the buffer knows about the fold, so a clean buffer read as "nothing
declined"); the same for `not_an_inner_build`; and a lexical scope merge that
double-reported an aliased path.

Rather than patch four, the feature is gone. It was never worth its cost: an
open `.fsproj` already gets **span-anchored diagnostics on its own buffer text
every keystroke** — strictly better feedback than a toast, and pointing at the
offending element rather than naming it. The toast's unique job is the one
squiggles cannot do: telling a `.fs` buffer why its *project* went quiet. So
every project is now reported from the workspace's evaluation, and an open
`.fsproj` is in scope merely as a project like any other.

What that buys, beyond the four bugs: the whole refresh is now cached reads —
the workspace's evaluation memo and the last recorded fold outcome — which is
what makes running it after every dispatched message affordable at all.

The remaining fifth finding was ordinary: the log line moved behind the same
changed-message check as the notification, since a per-dispatch refresh would
otherwise repeat a permanently-deferred project's full cause list into stderr
(and into Loki under `otel`) thousands of times a session.

### Cut again: an open `.fsproj` is not in scope on its own account

A fifth round found three, all in *scope discovery*: recomputing the scope per
dispatch called `owning_project`, which walks ancestors with `read_dir` on every
keystroke per open buffer; including an open `.fsproj` populated `Workspace`'s
project memo from disk through a path text-sync deliberately does not invalidate
(pinning a stale Compile list — the thing
`fsproj_sync_does_not_pin_the_project_cache` exists to prevent); and
`owning_project`'s nearest-ancestor fallback claimed a standalone `.fsx`,
reporting an unrelated neighbour's problems against a script that has no project.

- **Scope is maintained, not recomputed**: `recompute_deferral_scope` runs on
  open, close, and structural watched changes. Ownership does not change when a
  source buffer is edited, so the per-dispatch refresh does no filesystem work at
  all. A project leaving scope is forgotten, so reopening reports afresh.
- **An open `.fsproj` is no longer in scope by itself** — the last of three
  narrowings in the same direction. It enters scope when one of its source files
  is open, which is the only situation where the toast says anything its own
  diagnostics don't.
- **`Workspace::compiling_project`** is `owning_project` plus the script guard
  `symbols_for` already applied privately (a `.fsx` needs a conclusive
  `Membership::Member`). Every caller answering "what does this file belong to?"
  should use it; `owning_project` answers the narrower "whose directory encloses
  it?".

### The fold outcome is three-valued

A sixth round found the recorded fold outcome surviving invalidation: after
reporting an unreadable `Missing.fs`, fixing the project and re-evaluating left
the *clean* new evaluation paired with the *stale* refusal, re-sending a warning
that named a file the project no longer mentioned.

`FoldOutcome` is now `Succeeded | Refused | Unknown`, and every invalidation
drops back to `Unknown`. That third state is load-bearing in both directions:
treating it as success clears a still-valid message and re-sends it the moment
anything folds; treating it as failure re-reports a problem that may already be
fixed. So `speaks_for_the_fold` tells the caller when an *empty* deferral list
may be trusted to clear a message — everything except "clean evaluation, no fold
since the inputs changed", where we make no claim at all. An evaluation-level
decline is knowable without folding and is always reported; only the silence is
untrustworthy.

(The same round found `recompute_deferral_scope` running *before*
`apply_watched_changes` cleared the project memo, so it recorded owners from the
pre-change Compile lists. Reordered.)

## Known coverage limit: graph-level reference suppression

`ProjectReferenceEdges` is reported from the entry project's own evaluation. The
compile-closure graph walk suppresses edges **per node**, on two facts that
input does not carry:

- a **later target framework** of a multi-targeted project — the walk evaluates
  additional/seeded TFMs, so a clean first TFM hides an uncertain second one;
- a **transitive** node — if open project A references B and *B's* reference
  list is untrustworthy, A's `AssemblyEnv` loses C while A itself is clean, and
  with no B source open nobody is told.

Both are **under-reporting, never mis-reporting**: nothing false is said, but a
user can lose a reference edge in silence. Closing them needs the graph's own
per-node verdict as an input to reporting, which means running
`Workspace::project_graph` — a deliberately *off-cache* multi-project walk, since
it must not pin the project memo — on a path that currently touches nothing but
memos. That is a new axis with a real cost, not a fix to this one, and is left
here rather than done.

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

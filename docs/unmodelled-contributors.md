# Contributors the walk does not model

The resolver models a binding by pushing a scope entry at the point F# adds it
to the name environment. F#'s environment is latest-write-wins, so a binding
added somewhere the walk pushes nothing takes the slot from whatever the walk
would have committed there: FSharp.Core's implicitly opened `id` or `Some`, or
an earlier same-named binder of the file's own. Each such route is a
*contributor the walk does not model*.

The routes are answered by one primitive, `Resolver::push_contribution`
(`crates/sema/src/resolve/contributors.rs`). At the position F# adds a
contributor's names, it pushes a **mark** for each name the contributor may
add. A mark declines, and shadows every earlier entry of its name exactly as
the contributor's own binding would. A use that the contributor could take
therefore finds the mark and declines instead of committing the entry it hid.
A contributor whose names are spelled nowhere in the source stales every
earlier entry through the open generation instead.

Name sets are closed over-approximations read off the syntax. Everything a
contributor adds is *declared* in it — a `let` binder, a union or exception
case, an active-pattern case, an `extern`, a constructible type's name, a
member — so collecting the declarations cannot miss one. A name that is
declared but not in fact brought into bare scope costs a decline.

A mark's reach follows FCS's two tables:

| Mark | Where it shadows | Supplied by |
|---|---|---|
| `Both` | bare use, dotted head, and pattern | cases, active-pattern cases, attributed `let`s (which may be `[<Literal>]`) |
| `Expression` | bare use and dotted head | values that cannot be literals (an unattributed `let`, an `extern`, a static), and constructible types' names |

## The closed list

The list is enumerated from where FCS extends the environment, not from review
findings. The sources are `CheckDeclarations.fs` (`AddLocalSubModule`,
`AddLocalTycons`, `AddLocalRootModuleOrNamespace`, `ImplicitlyOpenOwnNamespace`,
`OpenModuleOrNamespaceRefs`, `OpenTypeContent`, `AddNonLocalCcu`,
`TcMutRecDefns_Phase1`), `NameResolution.fs`
(`AddModuleOrNamespaceRefToNameEnv`'s recursive `[<AutoOpen>]` fold,
`CanAutoOpenTyconRef`, `AddStaticContentOfTypeToNameEnv`,
`AddTyconRefsToNameEnv`), and `CheckComputationExpressions.fs`
(`customOperationMethodsIndexedByKeyword`).

Every route has a row in `crates/sema/tests/all/unmodelled_contributor_grid.rs`.
The grid crosses each route with four kinds of shadowed name (an implicit value,
an own value, an implicit union case, an own union case) and with each way of
using the name: applied, as a first-class value, under a local scope, in a nested
module, as a dotted head, and as a pattern. Every cell is graded against FCS, and FCS's answer and ours are
pinned in `crates/sema/tests/manifests/unmodelled_contributor_grid.txt`.

| # | Route | FCS source | How the walk answers it |
|---|---|---|---|
| 1 | Same-file `[<AutoOpen>]` module, at any depth, `private` included, after it closes | `AddLocalSubModule` → `AddModuleOrNamespaceRefToNameEnv` (recursive) | marks at the module's end, in the enclosing frame |
| 2 | The attribute spelled any way: qualified, `…Attribute`, in a shared list, through an abbreviation | `CheckDeclarations` honours an aliased `AutoOpenAttribute` (and warns) | `may_auto_open`: any attribute counts while written names cannot be trusted (`attribute_spellings_trusted`: no file so far declares a type spelled like the attribute or an abbreviation of it, and no referenced assembly may) |
| 3 | Same-file `[<AutoOpen>]` non-generic type or exception: its own statics, not a base class's, including an intrinsic augmentation's, with the accessors of its properties and events | `AddLocalTycons` → `CanAutoOpenTyconRef`, `AddStaticContentOfTypeToNameEnv` | marks at the type group's start, and at each later augmentation of it. An abbreviation, a delegate or inline IL is unlisted. |
| 4 | An `open` of a project module: its `[<AutoOpen>]` children | `OpenModuleOrNamespaceRefs` → `AddModuleOrNamespaceContentsToNameEnv` | marks after the module's folded values (`FileContributions::module_opens`) |
| 5 | An `open`, or the implicit open, of a project namespace: aliased `[<AutoOpen>]` children and `[<AutoOpen>]` types | same | marks after the namespace fold (`FileContributions::namespace_opens`) |
| 6 | An earlier file's root: a top-level `[<AutoOpen>] module X`, and everything `namespace global` declares, with the attribute read from the signature when there is one | `AddLocalRootModuleOrNamespace` | marks at every later block's start, after the implicit auto-opens and before its own namespace (`FileContributions::root`, from the implementation and from the signature, `signature_contributions`) |
| 7 | A `module rec` / `namespace rec` block, a nested `module rec`, and every module inside one: every binder, from the block's start | `TcMutRecDefns_Phase1` | marks at the block's start. The binders are re-asserted after each `open`, because FCS adds them after every open. The nested modules' `[<AutoOpen>]` contents are re-asserted after each type or exception, because FCS adds modules after tycons and exceptions. |
| 8 | A constructible type (class, struct, enum, delegate, or an abbreviation of one) taking the unqualified slot from an earlier value or case: in the same file, from an earlier file's own or opened namespace, or from a contributor | `AddTyconRefsToNameEnv` (types with "potential use as a constructor") | the head-slot ordering (`value_slot`), asked at a bare use as well as at a dotted head, with the enclosing namespace's earlier-file types. A parameter or pattern local always holds: the types in scope where it is bound entered first. A contributor's own constructible types are marks. |
| 9 | An `extern` prototype | `AddLocalVals` | an `Expression` mark at the prototype, which the walk does not intern |
| 10 | `open type` statics the projection drops | `OpenTypeContent` → `AddStaticContentOfTypeToNameEnv` | `Expression` marks after the type's listed statics, for each `skipped_members` name |
| 11 | A union whose `[<RequireQualifiedAccess>]` may be spelled through an abbreviation | `AddTyconRefsToNameEnv` (RQA cases are not bare) | `Both` marks after the cases the walk put in bare scope, and at an `open` of the container |
| 12 | A computation expression's custom operations, which take bare names in the expression ahead of every binding outside it | `customOperationMethodsIndexedByKeyword` | a committed bare use inside a CE declines when an implicit open supplies it (`computation_expression_decline`); the project's own bindings are the one known gap below |

Routes the grid checks that add nothing, and which the walk must keep
committing through:

- an `[<AutoOpen>]` module declared after the use;
- a name only inside a plain module nested in an `[<AutoOpen>]` one;
- a generic `[<AutoOpen>]` type (`CanAutoOpenTyconRef` refuses it);
- an `[<AutoOpen>]` class's or an `open type`'s *inherited* statics;
- an earlier file's plain top-level module, not opened;
- the project's own `[<assembly: AutoOpen>]` (`AddCcuToTcEnv` applies it to referenced assemblies only);
- extension members of an `open type` target;
- a member parameter named like a later type of its own type group.

Each of these must commit: the grid holds them to the unshadowed answer.

Routes the walk already modelled are in the grid too, because "modelled" is
exactly the claim each cell checks. These are `open type` of a declared static,
an earlier file's `[<AutoOpen>]` module in the probe's own or an opened
namespace, a same-file earlier block of the same namespace, an exception, and an
active pattern.

## Not on the list

- **The project's own binding of a name a custom operation takes.** Inside
  `builder { … }`, a custom operation of the builder takes its name ahead of
  every binding outside the expression, the project's included. The walk does
  not know the builder's type, so it cannot list those operations. Withholding
  every name bound outside a computation expression is sound, but it costs
  every parameter used in an `async { … }`: 320 of the 22,247 matches
  `resolve_corpus_diff` grades, and 2 on the whole-project gate. So the walk
  withholds only the names an implicit open supplies, which costs nothing
  measurable. The grid pins the rest as its one known-wrong class
  (`is_known_wrong`). Closing it needs the builder's custom operations, which
  means projecting `CustomOperationAttribute` from referenced assemblies.

- **Signature files** restrict what an implementation exports. The earlier-file
  routes read the implementation's declarations, which over-approximate what the
  signature exposes. A signature's own attributes are the ones later files see.
  So a root-level module that only its signature marks `[<AutoOpen>]` contributes
  too. The grid's signed rows check both directions, plus the namespace and
  nested-module forms.
- **Referenced assemblies' contributors** — their `[<AutoOpen>]` modules,
  `[<assembly: AutoOpen>]`, and root namespace — are the assembly fold's
  business (`docs/assembly-module-open-plan.md`). They enter before every
  project contributor.
- **Module abbreviations** add a module name, never a value or a case.

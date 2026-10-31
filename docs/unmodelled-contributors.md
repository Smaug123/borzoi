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

Whether a name set holds every name FCS says a contributor introduces is
checked by a generated oracle, `crates/sema/tests/all/contributor_name_oracle.rs`.
It writes contributors of every kind (`[<AutoOpen>]` modules, aliased, nested,
`rec`, an earlier file's root, a signature-only `[<AutoOpen>]`) holding random
declarations of every kind, and binds every candidate name ahead of the
contributor. The candidates are the names the generator wrote, their accessor
forms, the compiled operator names, and every name FCS reports as defined in the
contributor. It then uses each candidate bare in expression position, and in
pattern position when capitalised. Wherever we commit, FCS must name the same
symbol. Its absolute half checks the generator against FCS: every declaration
and member kind the generator says introduces a bare name must do so in FCS
somewhere in the run.

## The closed list

The list is enumerated from where FCS extends the environment, not from review
findings. The sources are `CheckDeclarations.fs` (`AddLocalSubModule`,
`AddLocalTycons`, `AddLocalRootModuleOrNamespace`, `ImplicitlyOpenOwnNamespace`,
`OpenModuleOrNamespaceRefs`, `OpenTypeContent`, `AddNonLocalCcu`,
`TcMutRecDefns_Phase1`), `NameResolution.fs`
(`AddModuleOrNamespaceRefToNameEnv`'s recursive `[<AutoOpen>]` fold,
`CanAutoOpenTyconRef`, `AddStaticContentOfTypeToNameEnv`,
`AddTyconRefsToNameEnv`), and `CheckComputationExpressions.fs`
(`TcComputationExpression`, `getCustomOperationMethods`,
`CustomOperationClause`).

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
| 6 | An earlier file's root, or an earlier block's of the same file: a top-level `[<AutoOpen>] module X`, and everything `namespace global` declares, with the attribute read from the signature when there is one | `AddLocalRootModuleOrNamespace` | marks at every later block's start, after the implicit auto-opens and before its own namespace (`FileContributions::root`, from the implementation and from the signature, `signature_contributions`); an earlier block's sit below what a reopened block re-takes (`earlier_block_contributions`) |
| 7 | A `module rec` / `namespace rec` block, a nested `module rec`, and every module inside one: every binder, from the block's start | `TcMutRecDefns_Phase1` | marks at the block's start. The binders are re-asserted after each `open`, because FCS adds them after every open. The nested modules' `[<AutoOpen>]` contents are re-asserted after each type or exception, because FCS adds modules after tycons and exceptions. |
| 8 | A constructible type (class, struct, enum, delegate, or an abbreviation of one) taking the unqualified slot from an earlier value or case: in the same file, from an earlier file's own or opened namespace, or from a contributor | `AddTyconRefsToNameEnv` (types with "potential use as a constructor") | the head-slot ordering (`value_slot`), asked at a bare use as well as at a dotted head, with the enclosing namespace's earlier-file types. A parameter or pattern local always holds: the types in scope where it is bound entered first. A contributor's own constructible types are marks. |
| 9 | An `extern` prototype | `AddLocalVals` | an `Expression` mark at the prototype, which the walk does not intern |
| 10 | What `open type` brings beside the statics the projection lists: the statics it drops, its nested types, and the extension members in scope for the type, whether the project's own augmentations or a referenced assembly's | `OpenTypeContent` → `AddStaticContentOfTypeToNameEnv` | `Expression` marks after the listed statics: each `skipped_members` name, each nested type's name, and every augmentation member name of any target, since a `[<CompiledName>]` can hide which type an extension extends (`AssemblyEnv::augmentation_member_names`, `FileContributions::extension_statics`). Unlisted when any assembly module dropped a member, or any assembly dropped a type. |
| 11 | A union whose `[<RequireQualifiedAccess>]` may be spelled through an abbreviation | `AddTyconRefsToNameEnv` (RQA cases are not bare) | `Both` marks after the cases the walk put in bare scope, and at an `open` of the container |
| 12 | A computation expression's custom operations, which take bare names in the expression ahead of every binding outside it, and a clause's head whatever is bound | `TcComputationExpression` → `AddFakeNameToNameEnv`, `CustomOperationClause` | an `Expression` mark per name the builder's operations may take, in a frame of the expression's own, and each clause head of those names declined outright; a query's range variables, which the walk does not intern (`join v in …`, and any name after an `into`), are marked the same way (`crates/sema/src/resolve/custom_operations.rs`; below) |

Routes the grid checks that add nothing, and which the walk must keep
committing through:

- an `[<AutoOpen>]` module declared after the use;
- a name only inside a plain module nested in an `[<AutoOpen>]` one;
- a generic `[<AutoOpen>]` type (`CanAutoOpenTyconRef` refuses it);
- an `[<AutoOpen>]` class's or an `open type`'s *inherited* statics;
- an earlier file's plain top-level module, not opened;
- the project's own `[<assembly: AutoOpen>]` (`AddCcuToTcEnv` applies it to referenced assemblies only);
- a member parameter named like a later type of its own type group.

Each of these must commit: the grid holds them to the unshadowed answer.

Routes the walk already modelled are in the grid too, because "modelled" is
exactly the claim each cell checks. These are `open type` of a declared static,
an earlier file's `[<AutoOpen>]` module in the probe's own or an opened
namespace, a same-file earlier block of the same namespace, an exception, and an
active pattern.

## Computation expressions

Inside `b { … }`, FCS adds a name-environment entry for each of the builder's
custom operations before it checks the body, so an operation's name hides every
binding made outside the expression, the project's own included. A binding made
inside the expression comes later and wins, except at a clause head: the
translation reads `op args` at a statement position as the operation by name
alone (`CustomOperationClause`), and an `into` it applies and a join's
condition word the same way; a clause's other arguments are ordinary
expressions. Patterns are untouched (`ePatItems`), and a dotted head loses its
value. A nested expression over another builder adds that builder's operations
on top.

The walk lists the names a builder's operations may take, and marks them all:

- **The builder's type.** A referenced module value has the type its getter
  returns (`async`, `task`, `query`); a referenced function makes the braces a
  sequence expression, which has no custom operations (`seq`). A value of this
  file bound `let b = T()` with `T` a type of this file has `T`, decided by the
  type-position resolver behind the same guards as an assembly type used as a
  constructor. Anything else is unknown.
- **The operations.** A referenced type's are its methods' and its base types',
  read off `[<CustomOperation>]` (`MethodLike::custom_operation`); a project
  type's are its members' attributes and its base types'. The walk does not
  resolve a project type's `inherit`, so a project builder with a base class is
  unknown, and so is an abbreviation's.
- **Every extension, counted for every builder.** F# adds the operations of
  every extension of the builder in scope. The walk does not ask which extensions
  are in scope or what they extend: every `[<CustomOperation>]` a project type
  augmentation declares, or a project type's static member (which may be a
  C#-style `[<Extension>]` method), earlier files' and this one's, every one an
  earlier signature file declares anywhere (a signature's attributes are the
  ones later files see), and every one a referenced assembly declares on an
  extension member, is an operation of every builder.

The list is unknown — and every name spelled in the expression is marked —
when the builder's type is, when a referenced type dropped a member or has a
base that cannot be proven, when an F# module dropped a member mangled like an
augmentation, when a type was dropped, when the extension surface is
unknowable, or when an attribute's name cannot be read (a non-literal argument,
an escape). An attribute spelled any way may be `[<CustomOperation>]` once a
file or an assembly may rename it.

The grid's computation-expression axis
(`every_computation_expression_cell_agrees_with_fcs`) crosses each way of
supplying an operation with seven kinds of name (an implicit value and case, an
own value and case, a parameter, an outer local, an inner local) and nine
positions (a statement, `return`, under a lambda, as a first-class value, as a
dotted head, in a nested expression over `async`, nested in one, a `let`
right-hand side, a pattern). Each cell is graded against FCS and must check
cleanly or be marked recovered.

The empty-environment sweep `resolve_corpus_diff` cannot identify any
FSharp.Core builder, so every expression there is over an unknown builder.

## Not on the list

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

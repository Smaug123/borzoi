# Hover: F# member/type signatures

> **Status:** landed. `textDocument/hover` renders a referenced entity/member as
> an F# signature line with declaring-type + assembly provenance context. Two
> narrow gaps remain (below); everything else is done. Code comments in
> `crates/lsp/src/handlers/hover.rs` and `crates/assembly/src/display.rs` point
> here as the tracker for the remaining items.

Builds on the entity model (`borzoi-assembly`), the `AssemblyEnv`
resolution layer (`borzoi-sema`), and the F# type pretty-printer
`display::format_type` (#591).

## Landed (one line each)

- **#587** — hover renders fully-qualified name + kind for a referenced
  entity/member (`System.Console.WriteLine — method`).
- **#589** — assembly provenance (`from <asm> v<ver>`), obsolete / experimental
  banners, modifier-qualified kind labels (`static method`, `read-only
  property`, `required field`, `extension method`, …).
- **#591** — `display::format_type`: the F# type pretty-printer (primitives,
  generics, arrows, tuples, nested-generic placement).
- **#598** (Slice 2) — `display::format_member(member, owner)` renders an F#
  signature line as the hover **head**, declaring type + assembly as context;
  keyword family from `owner.kind` + member flags (module → `val [mutable]`;
  type → `member`/`static member`/`abstract member`/`new`; property → `… with
  get[, set]`; field → `val [mutable]`). Events keep `[<CLIEvent>]`.
- **#603** — F# vs .NET optional parameters rendered distinctly (`?name: T` vs
  `[<Optional>] name: T` / `name: T = value`), via `Parameter::has_default` →
  `ParamDefault`.
- **#608** (follow-up 8) — fcs-dump strips F# abbreviations for inner-position
  nullability inside `FSharpOption`, restoring the F#-optional differential
  fixture.
- **#610** (follow-up 1) — `extension` / `required` members surfaced on the
  context line via `hover::member_qualifier` (signature head stays pure).
- **#614** (follow-up 2) — `Resolution::Entity` rendered as an F# declaration
  head via `display::format_entity_header` (`type List<'T>`, `[<Struct>] type`,
  `module`, `exception`, `[<Measure>] type`, `[<AutoOpen>]` /
  `[<RequireQualifiedAccess>]` / `[<IsReadOnly>]` / `[<IsByRefLike>]` prefixes),
  collapsed kind folded onto the context line via `hover::entity_qualifier`.
- **#616** (follow-up 4) — `Field::is_literal`; literal/const fields render
  honestly (`[<Literal>] static val …`, never `mutable`); genuine `static
  mutable` fields now read `mutable`.
- **follow-up 3** — indexer properties: `IndexParameter { name, ty }` preserves
  the index name through projection; `format_property` renders the dimension
  before the element type (`member Item: index: int -> 'T with get, set`,
  multiple indices tupled with `*`).
- **follow-up 5 (non-generic)** — module value vs unit-function: the projector
  distinguishes them (value = rebranded getter, function = genuine method) and
  tags `MethodLike::module_value` (with `is_mutable` from the dropped setter);
  `format_method` renders `val [mutable] x: T` vs `val f: unit -> T`. This
  subsumed #624's `il_doc_kind`, so `doc_id` keys a module value's `P:` off
  `module_value.is_some()`.
- **follow-up 6** — nullability: an `Annotated` nullable-reference position
  renders the C#-postfix `?` (`string?`, `List<string?>`, `string?[]`), threaded
  by `display::format_nullable_type`. Display-only; FCS diff untouched.
- **follow-up 7** — C# default values: the reader decodes the parameter
  `Constant` blob (II.22.9) into `ConstantValue`, carried on
  `ParamDefault::Optional(Option<ConstantValue>)`, so a C# default renders its
  value (`x: int = 5`). Value-less `[Optional]`/COM optional still renders
  `[<Optional>] name: T`.
- **follow-up 9** — attribute-encoded default values: `decimal`/`DateTime`
  defaults (not primitive `ELEMENT_TYPE`s, so no `Constant` row) decoded from
  `[DecimalConstantAttribute]` / `[DateTimeConstantAttribute]` via
  `decode_attribute_default`, before the value-less fallback. New
  `ConstantValue::Decimal { negative, scale, mantissa }` (renders `1.5M`) and
  `ConstantValue::DateTime(i64)` (renders `System.DateTime(<ticks>L)`).
- **follow-up 10** — `[<ParamArray>]` surfaced: a `params T[]` parameter
  (`Parameter::is_param_array`) renders the `[<ParamArray>]` attribute prefix on
  the *specific* variadic parameter in the signature head
  (`member Sum: [<ParamArray>] values: int[] -> int`), mirroring `[<Optional>]`.
  It sits on the parameter — not the context line the member-level
  `extension`/`required` flags use — because it is a per-parameter fact, and F#
  writes it exactly as this parameter attribute. The marker is *orthogonal* to
  the optional/default forms rather than exclusive with them (F# allows
  `[<ParamArray; Optional>]` / `[<ParamArray; OptionalArgument>]`, and FCS renders
  `[<ParamArray>] ?xs: 'T[]`), so `format_param` prepends it to whatever the
  `ParamDefault` arm produces. It also rides the indexer path: `IndexParameter`
  gained `is_param_array` (threaded from the accessor parameter) so a
  `params`/`[<ParamArray>]` indexer surfaces the marker too.

All model/reader additions above are additive: the differential normaliser
renders any optional as `= ?` and reads index/nullable positions through its own
renderer, so the FCS diff stays byte-identical throughout.

---

## Still to do

Two narrow gaps, both deliberately deferred — hover renders the **F# signature**
view, so a fact with no F# signature syntax plus one rare disambiguation are not
yet surfaced.

### 1. XML doc summaries not wired into hover

`doc_id` generation exists (the `M:`/`P:`/`T:` key derivation), but sidecar
`.xml` documentation-file lookup and parsing are not wired into the hover
handler, so no summary text is shown. Requires: locating the companion `.xml`
next to the resolved assembly, parsing the `<member name="…">` entries, keying
by `doc_id`, and appending the summary to the hover body.

### 2. Follow-up 5 residual — generic 0-parameter unit-function

A *generic* module value (`let empty<'T> = …`) is emitted by F# as a
0-parameter generic *method*, not a property, so the rebrand never tags it with
`MethodLike::module_value`. `format_method` falls back to "0-parameter generic
module method ⇒ value", which is correct for FSharp.Core values
(`Array.empty`/`List.empty`) but mis-renders the vanishingly rare generic
0-parameter *unit-function* (`let f<'T> () = …`) as a value. Closing it needs
threading the pickle's `ValReprInfo` arity (`val_il_arity`, already decoded in
`crates/assembly/src/fsharp_pickle_merge.rs`) into this formatter/rebrand
decision. See the comment at `display.rs` lines ~208–213.

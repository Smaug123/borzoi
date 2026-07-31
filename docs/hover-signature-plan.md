# Hover: F# member/type signatures

> **Status:** landed. `textDocument/hover` renders a referenced entity/member as
> an F# signature line with declaring-type + assembly provenance context. Four
> gaps remain (below); everything else is done. Code comments in
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
- **follow-up 5 (generic residual)** — the generic case of the value-vs-unit
  -function split: `let empty<'T> = …` (value) and `let f<'T> () = …`
  (unit-function) both compile to a 0-parameter generic *method* with
  `module_value: None` (a CLR property cannot be generic), so `format_method`'s
  old "0-parameter generic module method ⇒ value" heuristic mis-rendered the
  unit-function as a value. Fixed by consulting `MethodLike::is_module_value_binding`
  (the pickle's argument-group *count*, already threaded and consumed by the sema
  classifier `AssemblyEnv::member_class`): 0 groups ⇒ value (`val empty<'T>:
  'T[]`), one `unit` group ⇒ function (`val f<'T>: unit -> int`). The group
  *count* — not `val_il_arity`'s *sum*, which is 0 for both since a `unit` group
  is empty — is the discriminator; the plan's original `val_il_arity` suggestion
  was insufficient. Grounded end-to-end against real fsc via two new `MiniLibFs`
  bindings (`projector_source_names::generic_module_value_vs_unit_function_split`).
  When the host pickle is *absent* (the `il_heuristic` fallback path — a
  `--standalone`/reference image, or an unreadable signature resource), the count
  is unavailable, so the projector presumes the ambiguous generic 0-parameter
  shape is a *value* (the common `Array.empty` case, matching the old default);
  the pickle merge overwrites that presumption authoritatively wherever it does
  cover the member (`ecma335_assembly::pickle_less_generic_module_method_is_presumed_a_value`).

- **union cases** — a field-carrying union case of a referenced assembly renders
  as a case, not as the nested class fsc compiled it to: `` `union case
  Choice1Of2` `` + `in Microsoft.FSharp.Core.Choice<'T1, 'T2>`
  (`hover::union_case_hover_label`). Gated on `AssemblyEnv::entity_class` —
  deliberately the predicate the semantic-token classifier already asks, so the
  two surfaces cannot drift into calling one handle a class and a union case
  (they had), and hover inherits its declines rather than restating them. Built
  without `format_entity_header`: a nested type re-declares its enclosing type's
  generic parameters, so the header would write them onto the case name.
  `AssemblyEnv::all_handles` lands with it, for the invariant sweep over every
  entity of a real FSharp.Core (`handlers_hover::hover_calls_an_entity_a_union_case_exactly_when_classification_does`).
- **nested-entity context lines** — `hover::entity_fqn` composes from
  `AssemblyEnv::entity_full_name`, so a member of a nested type names its whole
  enclosing chain (`in Microsoft.FSharp.Core.Choice.Choice1Of2<'T1, 'T2>`); a
  nested ECMA TypeDef declares no namespace of its own, so building from
  `Entity::namespace` had left a bare `Choice1Of2`.

All model/reader additions above are additive: the differential normaliser
renders any optional as `= ?` and reads index/nullable positions through its own
renderer, so the FCS diff stays byte-identical throughout.

---

## Still to do

Hover renders the **F# signature** view. The remaining gaps are facts that view
would show but the projection cannot currently supply, plus one rendering the
`type` keyword still over-claims.

### 1. XML doc summaries not wired into hover

`doc_id` generation exists (the `M:`/`P:`/`T:` key derivation), but sidecar
`.xml` documentation-file lookup and parsing are not wired into the hover
handler, so no summary text is shown. Requires: locating the companion `.xml`
next to the resolved assembly, parsing the `<member name="…">` entries, keying
by `doc_id`, and appending the summary to the hover body.

### 2. A union case's payload and its `[<Obsolete>]` marker

`union case Circle` stops at the name: neither the payload (`of radius: int`)
nor an `[<Obsolete>]`/`[<Experimental>]` marker on the *case* is rendered, and
the two have different causes.

The payload survives only as the carrier's generated `Item`/`ItemN` (or
named-field) properties, so reconstructing an `of` clause means guessing which
of them are fields of the case — an absent payload under-states the case, a
wrong one misreports it.

The marker is not reachable at all today: fsc attaches a case's attributes to
its `New<Case>` maker method, and a union's constructors are deliberately absent
from `Entity::members` (their names live in `union_cases`), so neither the
carrier nor the union carries it. Checked against a purpose-built
`[<Obsolete>]`-cased union, whose carrier projects `obsolete: None`. Surfacing
it means projecting the maker method, or lifting case attributes into the
pickle-derived case list.

### 3. `type X` over-claims for an undecidable nested type

A union whose cases could not be recovered (`UnionCases::Unknowable`) makes its
nested types undecidable: each is either a case's carrier or the generated
`Tags` class, and `AssemblyEnv::entity_class` declines them for exactly that
reason. Hover has no third rendering, so it falls to the ordinary entity arm and
says `type Circle` / `class` — the reading FCS contradicts on a carrier.

Such a handle does reach hover: expression position walks nested types through
the *ungated* path, unlike pattern position, which resolves a case only through
`authoritative_union_case`
(`resolve_assembly::an_undecidable_carrier_reaches_a_renderer_but_is_never_classified_a_case`
pins both). What holds today is that the case reading is never *claimed* where
it cannot be proved. Fixing the residue needs a rendering that withholds the
kind without inventing a state a user cannot act on.

### 4. The declaration head does not consult authoritative-signature knowledge

The same drift the union-case arm closed exists once more, unfixed: for an
assembly whose F# signature is not authoritative (`fsc --standalone`, an
undecodable pickle), `AssemblyEnv::entity_class` declines to call an entity a
`module` — FCS imports such an assembly through IL, where a module is a plain
type — but `format_entity_header` reads `EntityKind` directly and renders
`module X` regardless. It is the union-case bug in a different kind: two views
of one handle disagreeing. The invariant sweep is deliberately scoped to the
union-case question so it does not fail on this; widening it is the test for
this fix. FSharp.Core cannot exercise it (its pickle is authoritative), so a
fixture assembly is needed.

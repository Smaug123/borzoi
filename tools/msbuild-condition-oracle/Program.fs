/// Differential-test oracle over the *real* MSBuild condition evaluator
/// (test-only; never shipped in the LSP). It answers the one question
/// `crates/msbuild/src/condition.rs` reimplements: "given these properties,
/// does MSBuild consider this `Condition` string true, false, or illegal?"
///
/// Protocol: JSONL request/response over stdin/stdout, one response line per
/// request line, in order (the same long-lived-batch-child pattern as
/// tools/fcs-dump and tools/nuget-oracle). Two ops:
///
///   {"op":"eval","condition":s,"properties":{name:value, ..}}
///     -> {"ok":true,"value":true|false}   // MSBuild evaluated the condition
///      | {"ok":false}                     // MSBuild rejects it as illegal
///                                          //   (InvalidProjectFileException,
///                                          //    e.g. `==`, `1 <`, bad funcs)
///
///   {"op":"expand","value":s,"properties":{name:value, ..}}
///     -> {"ok":true,"value":s}            // MSBuild expanded the property value
///      | {"ok":false}                     // MSBuild errors evaluating it
///                                          //   (e.g. a property function that
///                                          //    throws: bad Version.Parse,
///                                          //    out-of-range indexer)
///     The `value` is placed verbatim as the text of a `<_ExpandResult>`
///     property and read back after evaluation, so `$(…)` property functions
///     inside it run exactly as a real build would run them. The caller MUST
///     NOT reference `_ExpandResult` inside `value` (it would self-collide);
///     the differential harness generates names from a controlled set disjoint
///     from it, exactly as for `eval`.
///
///   {"op":"items","xml":s,"path":s,"itemType":s}
///     -> {"ok":true,"items":[fullPath, ..]}     // MSBuild evaluated the items
///      | {"ok":false}                           // MSBuild rejects the project
///     The `project` op's item-side twin: the document is written to `path` and
///     loaded as that file (so the reserved path derivatives are computed from
///     the same path the Rust side uses), and the evaluated items of `itemType`
///     come back as their `FullPath` metadata, in evaluation order. This is what
///     `dotnet msbuild -getItem:` reports, but through the resident oracle — a
///     per-case `dotnet msbuild` pays .NET startup every time, which a
///     generative sweep cannot afford.
///
///   {"op":"itemsMeta","xml":s,"path":s,"itemType":s,"metadata":[name, ..]}
///     -> {"ok":true,"items":[{"identity":s,"metadata":{name:value, ..}}, ..]}
///      | {"ok":false}                           // MSBuild rejects the project
///     The `items` op enriched for the *dependency* set, where `FullPath` is
///     meaningless: each evaluated item of `itemType` comes back as its
///     `EvaluatedInclude` (the identity MSBuild matches `Update`/`Remove`
///     against) plus the requested metadata's evaluated values (unset reads
///     back as ""). This is what lets a generative sweep diff a
///     `<PackageReference>` `Include`+`Update` collapse — identity matching and
///     per-key metadata merge — against the real evaluator, resident.
///
///   {"op":"project","xml":s,"names":[..]}
///     -> {"ok":true,"values":{name:value, ..}}  // MSBuild evaluated the project
///      | {"ok":false}                           // MSBuild rejects the project
///     The caller's project XML is evaluated *verbatim*, so both sides read the
///     same document. Unlike `expand` — which hands MSBuild a property *body*
///     between sentinels, and is therefore blind to the XML layer that runs
///     before expansion (insignificant whitespace, entity decoding, CDATA,
///     comment-split text) — this op is what lets a differential see that
///     layer at all.
///
/// `properties` is optional; each entry becomes a `<name>value</name>` in the
/// stub project's property group. Property names MUST be valid MSBuild/XML
/// identifiers and SHOULD stay disjoint from process environment variables and
/// MSBuild reserved names: MSBuild evaluation folds the environment in as
/// properties, and the Rust side only knows the map it sent, so a collision
/// would make the two disagree for reasons unrelated to the condition grammar.
/// The differential harness scrubs the environment and generates names from a
/// controlled set to guarantee this.
///
/// How a boolean is obtained: the stub declares a single guarded *item* (see
/// `stubXml`); the item is present iff MSBuild considers the condition true, so
/// we read back its count. An item — not a property — deliberately, so the
/// result lives in a namespace (`@(...)`) disjoint from everything a condition
/// references (`$(...)`); no property name a caller supplies or a condition
/// mentions can perturb it. Evaluation happens in-process against the installed
/// SDK's MSBuild (see the .fsproj note on MSBuildLocator), so it is the same
/// evaluator `dotnet msbuild` would run over the same stub — but ~7000×/s
/// instead of one process spawn per case.
///
/// Any *unexpected* per-request exception (a harness/generator bug — malformed
/// XML from an invalid property name, say — never a legitimate differential
/// result) is reported as {"error":..} on that line; the process itself never
/// dies mid-batch. An illegal *condition* is not an error: it is the
/// first-class {"ok":false} answer.
module MsBuildConditionOracle.Program

open System
open System.IO
open System.Security
open System.Text
open System.Text.Json
open System.Xml
open Microsoft.Build.Evaluation
open Microsoft.Build.Exceptions
open Microsoft.Build.Locator

/// The item whose presence encodes the condition's truth. Deliberately an
/// *item*, not a property: the condition is expressed purely in the property
/// namespace (`$(Name)`), so a result held in the item namespace (`@(Name)`)
/// cannot collide with anything a condition references or a caller supplies —
/// even a caller property or `$(...)` reference literally named
/// `_ConditionResult` leaves this item untouched.
[<Literal>]
let private ResultItem = "_ConditionResult"

/// The property whose evaluated text is the `expand` op's answer. As with
/// `ResultItem`, callers must keep it out of the value under test (a
/// `$(_ExpandResult)` self-reference would perturb the answer). Unlike the
/// condition case there is no disjoint namespace to hide in — an *expanded
/// value* is a property, so this must be a property — so the discipline is on
/// the caller (the differential harness generates disjoint names).
[<Literal>]
let private ResultProperty = "_ExpandResult"

/// Build the stub project XML: the supplied properties in a `<PropertyGroup>`,
/// then a single `<_ConditionResult>` item guarded by `condition` — present iff
/// MSBuild considers the condition true. Everything is XML-escaped; property
/// *values* and the condition are attacker-controlled fuzz, so they go through
/// `SecurityElement.Escape`. Property *names* are trusted to be valid
/// identifiers (see the module docs) and are emitted verbatim.
let private stubXml (condition: string) (properties: (string * string) seq) : string =
    let sb = StringBuilder()
    sb.Append "<Project><PropertyGroup>" |> ignore

    for (name, value) in properties do
        sb.Append('<').Append(name).Append('>') |> ignore
        sb.Append(SecurityElement.Escape value) |> ignore
        sb.Append("</").Append(name).Append('>') |> ignore

    sb.Append "</PropertyGroup><ItemGroup><" |> ignore
    sb.Append(ResultItem).Append(" Include=\"x\" Condition=\"") |> ignore
    sb.Append(SecurityElement.Escape condition) |> ignore
    sb.Append "\" /></ItemGroup></Project>" |> ignore
    sb.ToString()

/// Evaluate `condition` against `properties` via a throwaway
/// `ProjectCollection` (so a long batch does not accumulate loaded projects in
/// the global collection). Returns `Some true`/`Some false` for a value MSBuild
/// computed, or `None` when MSBuild rejects the condition as illegal.
let private evalCondition (condition: string) (properties: (string * string) seq) : bool option =
    let xml = stubXml condition properties
    use collection = new ProjectCollection()
    use reader = XmlReader.Create(new StringReader(xml))
    // Create the root *in the throwaway collection* (the parameterless
    // overload caches it in the global collection instead, so a long batch
    // would accumulate thousands of roots there). Disposing `collection`
    // releases both the root and the evaluated project.
    let root = Microsoft.Build.Construction.ProjectRootElement.Create(reader, collection)

    try
        // `Project` construction evaluates eagerly — an illegal condition
        // throws here rather than at item read.
        let project = Project(root, null, null, collection)
        // `GetItems` returns post-condition items: the result item is present
        // iff MSBuild considered the guard true.
        Some(project.GetItems(ResultItem).Count > 0)
    with :? InvalidProjectFileException ->
        None

let private respondEval (root: JsonElement) : string =
    let condition = root.GetProperty("condition").GetString()

    let properties =
        match root.TryGetProperty "properties" with
        | true, props when props.ValueKind = JsonValueKind.Object ->
            props.EnumerateObject()
            |> Seq.map (fun p -> (p.Name, p.Value.GetString()))
            |> Seq.toArray
        | _ -> [||]

    match evalCondition condition properties with
    | Some value -> JsonSerializer.Serialize {| ok = true; value = value |}
    | None -> JsonSerializer.Serialize {| ok = false |}

/// Build the stub project XML for the `expand` op: the supplied properties,
/// then a `<_ExpandResult>` whose text is the value under test. Placed *after*
/// the supplied properties so a `$(Foo)` reference resolves to the value the
/// caller sent. Everything is XML-escaped; property values and the value under
/// test are attacker-controlled fuzz.
let private expandStubXml (value: string) (properties: (string * string) seq) : string =
    let sb = StringBuilder()
    sb.Append "<Project><PropertyGroup>" |> ignore

    for (name, propValue) in properties do
        sb.Append('<').Append(name).Append('>') |> ignore
        sb.Append(SecurityElement.Escape propValue) |> ignore
        sb.Append("</").Append(name).Append('>') |> ignore

    sb.Append('<').Append(ResultProperty).Append('>') |> ignore
    sb.Append(SecurityElement.Escape value) |> ignore
    sb.Append("</").Append(ResultProperty).Append('>') |> ignore
    sb.Append "</PropertyGroup></Project>" |> ignore
    sb.ToString()

/// Expand `value` as a property body against `properties`. `Some s` is the
/// evaluated text MSBuild produced (`""` when it reduces to empty — distinct
/// from an error); `None` when MSBuild throws evaluating it (a property
/// function that fails: an unparseable `Version`, an out-of-range indexer,
/// an unknown member). MSBuild evaluates properties eagerly at `Project`
/// construction, so those throw here, exactly like an illegal condition.
let private evalExpand (value: string) (properties: (string * string) seq) : string option =
    let xml = expandStubXml value properties
    use collection = new ProjectCollection()
    use reader = XmlReader.Create(new StringReader(xml))
    let root = Microsoft.Build.Construction.ProjectRootElement.Create(reader, collection)

    try
        let project = Project(root, null, null, collection)
        Some(project.GetPropertyValue ResultProperty)
    with :? InvalidProjectFileException ->
        None

let private respondExpand (root: JsonElement) : string =
    let value = root.GetProperty("value").GetString()

    let properties =
        match root.TryGetProperty "properties" with
        | true, props when props.ValueKind = JsonValueKind.Object ->
            props.EnumerateObject()
            |> Seq.map (fun p -> (p.Name, p.Value.GetString()))
            |> Seq.toArray
        | _ -> [||]

    match evalExpand value properties with
    | Some value -> JsonSerializer.Serialize {| ok = true; value = value |}
    | None -> JsonSerializer.Serialize {| ok = false |}

/// Evaluate a *whole project* — the caller's XML verbatim, byte for byte, so
/// both sides see the same document — and read back the named properties.
/// `Some map` is what MSBuild computed; `None` when it rejects the project.
///
/// This is the `expand` op's structural counterpart: `expand` deliberately
/// anchors its value between sentinels and hands MSBuild a *body*, which makes
/// it blind to everything the XML layer does before expansion (insignificant
/// whitespace, entity decoding, CDATA, comment-split text). Handing over the
/// document itself is the only way to compare that layer.
///
/// A name MSBuild never defines reads back as `""` — indistinguishable from a
/// defined-empty property through this API. The Rust harness only asserts on
/// names it *committed* a value for, so that ambiguity costs nothing: a name we
/// dropped makes no claim either way.
/// With a `path`, the XML is written there and loaded *as that file*, so
/// MSBuild's reserved path derivatives (`MSBuildProjectDirectory`,
/// `MSBuildProjectName`, …) are computed from the same path the Rust side
/// passes to its own parser. That is the only way to diff anything those
/// properties feed — including the one case where a `%XX` is *not* an escape,
/// because it came from the project's own directory name.
let private evalProject
    (path: string option)
    (xml: string)
    (names: string seq)
    : Map<string, string> option =
    use collection = new ProjectCollection()

    let project =
        try
            match path with
            | Some path ->
                Directory.CreateDirectory(Path.GetDirectoryName path: string) |> ignore
                File.WriteAllText(path, xml)
                Some(Project(path, null, null, collection))
            | None ->
                use reader = XmlReader.Create(new StringReader(xml))
                let root = Microsoft.Build.Construction.ProjectRootElement.Create(reader, collection)
                Some(Project(root, null, null, collection))
        with :? InvalidProjectFileException ->
            None

    project
    |> Option.map (fun project ->
        names
        |> Seq.map (fun name -> (name, project.GetPropertyValue name))
        |> Map.ofSeq)

/// The `project` op's item-side twin: evaluate the document *as a file at
/// `path`* and read back the `FullPath` of every `itemType` item, in evaluation
/// order. `None` when MSBuild rejects the project.
let private evalItems (path: string) (xml: string) (itemType: string) : string list option =
    use collection = new ProjectCollection()

    try
        Directory.CreateDirectory(Path.GetDirectoryName path: string) |> ignore
        File.WriteAllText(path, xml)
        let project = Project(path, null, null, collection)

        project.GetItems itemType
        |> Seq.map (fun item -> item.GetMetadataValue "FullPath")
        |> List.ofSeq
        |> Some
    with :? InvalidProjectFileException ->
        None

let private respondItems (root: JsonElement) : string =
    let xml = root.GetProperty("xml").GetString()
    let path = root.GetProperty("path").GetString()
    let itemType = root.GetProperty("itemType").GetString()

    match evalItems path xml itemType with
    | Some items -> JsonSerializer.Serialize {| ok = true; items = items |}
    | None -> JsonSerializer.Serialize {| ok = false |}

/// The `items` op enriched for dependency items: each item's `EvaluatedInclude`
/// (the identity `Update`/`Remove` match against) and the requested metadata's
/// evaluated values, in evaluation order. `None` when MSBuild rejects the
/// project.
let private evalItemsMeta
    (path: string)
    (xml: string)
    (itemType: string)
    (metadata: string list)
    : (string * Map<string, string>) list option =
    use collection = new ProjectCollection()

    try
        Directory.CreateDirectory(Path.GetDirectoryName path: string) |> ignore
        File.WriteAllText(path, xml)
        let project = Project(path, null, null, collection)

        project.GetItems itemType
        |> Seq.map (fun item ->
            let values =
                metadata
                |> List.map (fun name -> (name, item.GetMetadataValue name))
                |> Map.ofList

            (item.EvaluatedInclude, values))
        |> List.ofSeq
        |> Some
    with :? InvalidProjectFileException ->
        None

let private respondItemsMeta (root: JsonElement) : string =
    let xml = root.GetProperty("xml").GetString()
    let path = root.GetProperty("path").GetString()
    let itemType = root.GetProperty("itemType").GetString()

    let metadata =
        root.GetProperty("metadata").EnumerateArray()
        |> Seq.map (fun n -> n.GetString())
        |> List.ofSeq

    match evalItemsMeta path xml itemType metadata with
    | Some items ->
        JsonSerializer.Serialize
            {| ok = true
               items =
                items
                |> List.map (fun (identity, values) ->
                    {| identity = identity
                       metadata = values |> Map.toSeq |> dict |}) |}
    | None -> JsonSerializer.Serialize {| ok = false |}

let private respondProject (root: JsonElement) : string =
    let xml = root.GetProperty("xml").GetString()

    let path =
        match root.TryGetProperty "path" with
        | true, p when p.ValueKind = JsonValueKind.String -> Some(p.GetString())
        | _ -> None

    let names =
        root.GetProperty("names").EnumerateArray()
        |> Seq.map (fun n -> n.GetString())
        |> Seq.toArray

    match evalProject path xml names with
    | Some values ->
        JsonSerializer.Serialize
            {| ok = true
               values = values |> Map.toSeq |> dict |}
    | None -> JsonSerializer.Serialize {| ok = false |}

[<EntryPoint>]
let main _argv =
    // Redirect the assembly loader to the installed SDK's MSBuild before any
    // Microsoft.Build type is touched at runtime. The evaluation logic lives
    // behind `evalCondition`, which is not JIT-compiled until first called
    // (well after this line), so the pinned reference assemblies never load.
    MSBuildLocator.RegisterDefaults() |> ignore

    let mutable line = Console.In.ReadLine()

    while not (isNull line) do
        if line.Trim() <> "" then
            let response =
                try
                    use doc = JsonDocument.Parse line
                    let root = doc.RootElement

                    match root.GetProperty("op").GetString() with
                    | "eval" -> respondEval root
                    | "expand" -> respondExpand root
                    | "project" -> respondProject root
                    | "items" -> respondItems root
                    | "itemsMeta" -> respondItemsMeta root
                    | other -> JsonSerializer.Serialize {| error = $"unknown op: %s{other}" |}
                with ex ->
                    JsonSerializer.Serialize {| error = ex.Message |}

            Console.Out.WriteLine response
            Console.Out.Flush()

        line <- Console.In.ReadLine()

    0

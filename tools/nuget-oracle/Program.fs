/// Differential-test oracle over the real NuGet client libraries
/// (test-only; never shipped in the LSP — see docs/nuget-restore-plan.md).
///
/// Protocol: JSONL request/response over stdin/stdout, one response line per
/// request line, in order (the same long-lived-batch-child pattern as
/// tools/fcs-dump). Ops:
///
///   {"op":"parseVersion","input":s}
///     -> {"ok":true, "normalized":..,"full":..,"major":..,"minor":..,
///         "patch":..,"revision":..,"releaseLabels":[..],
///         "hasMetadata":..,"metadata":..,"isPrerelease":..}
///      | {"ok":false}
///   {"op":"compareVersions","a":s,"b":s}   (VersionComparer.Default)
///     -> {"ok":true,"cmp":-1|0|1,"eq":bool} | {"ok":false}
///   {"op":"parseRange","input":s}
///     -> {"ok":true, "normalized":..,"hasLowerBound":..,"isMinInclusive":..,
///         "minVersion":..,"hasUpperBound":..,"isMaxInclusive":..,
///         "maxVersion":..,"isFloating":..,"floatBehavior":..}
///      | {"ok":false}
///   {"op":"rangeSatisfies","range":s,"version":s}
///     -> {"ok":true,"satisfies":bool} | {"ok":false}
///   {"op":"parseFramework","input":s} / {"op":"parseFolder","input":s}
///     (NuGetFramework.Parse / NuGetFramework.ParseFolder)
///     -> {"ok":true, "shortFolderName":..,"framework":..,"version":..,
///         "platform":..,"platformVersion":..,"profile":..,
///         "isSpecificFramework":..,"isUnsupported":..,"isAny":..,
///         "isPCL":..,"hasPlatform":..,"hasProfile":..}
///      | {"ok":false}
///     (shortFolderName is "" when GetShortFolderName itself throws)
///   {"op":"isCompatible","project":s,"candidate":s}
///     (DefaultCompatibilityProvider; both sides NuGetFramework.Parse)
///     -> {"ok":true,"compatible":bool} | {"ok":false}
///   {"op":"getNearest","project":s,"candidates":[s..]}
///     (FrameworkReducer.GetNearest; candidates NuGetFramework.ParseFolder)
///     -> {"ok":true,"nearest":index-into-candidates | -1} | {"ok":false}
///   {"op":"readNuspec","input":s}
///     (NuspecReader over XDocument.Parse(s), dependency + reference groups)
///     -> {"ok":true,"groups":[{"targetFramework":short,
///          "dependencies":[{"id":..,"hasVersionRange":..,
///          "versionRange":..,"include":[..],"exclude":[..]}]}],
///         "references":[{"targetFramework":short,"files":[..]}]} | {"ok":false}
///   {"op":"selectCompileAssets","framework":tfm,"files":[path..],"nuspec":xml}
///     Compile-asset selection for one package, exactly as
///     `LockFileUtils.CreateLockFileTargetLibrary` computes
///     `CompileTimeAssemblies`: the content model
///     (`ManagedCodeConventions` + `ContentItemCollection.FindBestItemGroup`
///     over CompileRefAssemblies *then* CompileLibAssemblies — ref takes
///     precedence over lib), followed by the nuspec `<references>` filter
///     (`ApplyReferenceFilter`). `files` are package-relative, '/'-separated.
///     -> {"ok":true,"items":[path..]} | {"ok":false}
///     NOT modelled: `ApplyLibContract` (the legacy `lib/contract` hack),
///     AssetTargetFallback, and RID-specific criteria. The Rust side declines
///     on all three, so no such case is ever asked of this op.
///   {"op":"selectDependencyGroup","project":tfm,"input":s}
///     (NuspecReader dependency groups + FrameworkReducer.GetNearest)
///     -> {"ok":true,"nearest":index-into-groups | -1} | {"ok":false}
///   {"op":"resolve","framework":tfm,
///      "packages":[{"id":..,"version":..,"nuspec":xml}, ..],
///      "direct":[{"id":..,"range":..}, ..]}
///     The end-to-end offline resolver oracle: the *genuine* PackageReference
///     restore engine (RemoteDependencyWalker + GraphOperations.Analyze — what
///     `dotnet restore` runs for SDK-style projects), over a synthetic local
///     folder feed built from the supplied nuspecs plus a synthetic root
///     package depending on `direct`.
///     -> {"ok":true,"resolved":true,"packages":[{"id":lower,"version":norm}, ..]}
///          (sorted by lowercased id; the closure `dotnet restore` would write)
///      | {"ok":true,"resolved":false,"reason":"missing"|"cycle"|"conflict"|"downgrade"}
///          (the reason restore would fail — NU1101/NU1108/NU1107/NU1605)
///
/// Any per-request exception is reported as {"error":..} on that line; the
/// process itself never dies mid-batch.
module NuGetOracle.Program

open System
open System.IO
open System.IO.Compression
open System.Collections.Generic
open System.Text.Json
open System.Xml.Linq
open NuGet.Client
open NuGet.Common
open NuGet.Configuration
open NuGet.Commands
open NuGet.ContentModel
open NuGet.DependencyResolver
open NuGet.Frameworks
open NuGet.LibraryModel
open NuGet.Packaging
open NuGet.Protocol
open NuGet.Protocol.Core.Types
open NuGet.RuntimeModel
open NuGet.Versioning

let private respondParseVersion (root: JsonElement) : string =
    let input = root.GetProperty("input").GetString()

    // Explicit out-var: the overload set (NuGetVersion.TryParse hiding
    // SemanticVersion.TryParse) defeats inference on the tupled-match form.
    let mutable v: NuGetVersion = Unchecked.defaultof<NuGetVersion>

    if NuGetVersion.TryParse(input, &v) then
        JsonSerializer.Serialize
            {| ok = true
               normalized = v.ToNormalizedString()
               full = v.ToFullString()
               major = v.Major
               minor = v.Minor
               patch = v.Patch
               revision = v.Revision
               releaseLabels = Array.ofSeq v.ReleaseLabels
               hasMetadata = v.HasMetadata
               metadata = (if v.HasMetadata then v.Metadata else "")
               isPrerelease = v.IsPrerelease |}
    else
        JsonSerializer.Serialize {| ok = false |}

let private respondCompareVersions (root: JsonElement) : string =
    let a = root.GetProperty("a").GetString()
    let b = root.GetProperty("b").GetString()
    let mutable va: NuGetVersion = Unchecked.defaultof<NuGetVersion>
    let mutable vb: NuGetVersion = Unchecked.defaultof<NuGetVersion>

    if NuGetVersion.TryParse(a, &va) && NuGetVersion.TryParse(b, &vb) then
        JsonSerializer.Serialize
            {| ok = true
               cmp = Math.Sign(VersionComparer.Default.Compare(va, vb))
               eq = VersionComparer.Default.Equals(va, vb) |}
    else
        JsonSerializer.Serialize {| ok = false |}

let private respondParseRange (root: JsonElement) : string =
    let input = root.GetProperty("input").GetString()
    let mutable r: VersionRange = Unchecked.defaultof<VersionRange>

    if VersionRange.TryParse(input, &r) then
        JsonSerializer.Serialize
            {| ok = true
               normalized = r.ToNormalizedString()
               hasLowerBound = r.HasLowerBound
               isMinInclusive = r.IsMinInclusive
               minVersion = (if r.HasLowerBound then r.MinVersion.ToFullString() else "")
               hasUpperBound = r.HasUpperBound
               isMaxInclusive = r.IsMaxInclusive
               maxVersion = (if r.HasUpperBound then r.MaxVersion.ToFullString() else "")
               isFloating = r.IsFloating
               floatBehavior = (if r.IsFloating then string r.Float.FloatBehavior else "None") |}
    else
        JsonSerializer.Serialize {| ok = false |}

let private respondRangeSatisfies (root: JsonElement) : string =
    let range = root.GetProperty("range").GetString()
    let version = root.GetProperty("version").GetString()
    let mutable r: VersionRange = Unchecked.defaultof<VersionRange>
    let mutable v: NuGetVersion = Unchecked.defaultof<NuGetVersion>

    if VersionRange.TryParse(range, &r) && NuGetVersion.TryParse(version, &v) then
        JsonSerializer.Serialize {| ok = true; satisfies = r.Satisfies v |}
    else
        JsonSerializer.Serialize {| ok = false |}

/// Parse via the supplied entry point; both Parse and ParseFolder throw on
/// inputs they refuse outright (empty string), and return an "Unsupported"
/// framework for merely-unrecognised ones — the response distinguishes the
/// two (ok=false vs isUnsupported=true), mirroring what callers see.
let private respondParseFrameworkWith (parse: string -> NuGetFramework) (root: JsonElement) : string =
    let input = root.GetProperty("input").GetString()

    try
        let f = parse input

        let shortName =
            try
                f.GetShortFolderName()
            with _ ->
                ""

        JsonSerializer.Serialize
            {| ok = true
               shortFolderName = shortName
               framework = f.Framework
               version = string f.Version
               platform = f.Platform
               platformVersion = string f.PlatformVersion
               profile = f.Profile
               isSpecificFramework = f.IsSpecificFramework
               isUnsupported = f.IsUnsupported
               isAny = f.IsAny
               isPCL = f.IsPCL
               hasPlatform = f.HasPlatform
               hasProfile = f.HasProfile |}
    with _ ->
        JsonSerializer.Serialize {| ok = false |}

let private respondIsCompatible (root: JsonElement) : string =
    let project = root.GetProperty("project").GetString()
    let candidate = root.GetProperty("candidate").GetString()

    try
        let p = NuGetFramework.Parse project
        let c = NuGetFramework.Parse candidate

        JsonSerializer.Serialize
            {| ok = true
               compatible = DefaultCompatibilityProvider.Instance.IsCompatible(p, c) |}
    with _ ->
        JsonSerializer.Serialize {| ok = false |}

let private respondGetNearest (root: JsonElement) : string =
    let project = root.GetProperty("project").GetString()

    let candidates =
        root.GetProperty("candidates").EnumerateArray()
        |> Seq.map (fun e -> e.GetString())
        |> Seq.toArray

    try
        let p = NuGetFramework.Parse project
        let parsed = candidates |> Array.map NuGetFramework.ParseFolder
        let reducer = FrameworkReducer()
        let nearest = reducer.GetNearest(p, parsed)

        let index =
            if isNull (box nearest) then
                -1
            else
                parsed |> Array.findIndex (fun c -> obj.ReferenceEquals(c, nearest))

        JsonSerializer.Serialize {| ok = true; nearest = index |}
    with _ ->
        JsonSerializer.Serialize {| ok = false |}

let private shortFolderName (f: NuGetFramework) : string =
    try
        f.GetShortFolderName()
    with _ ->
        ""

let private stringArray (xs: System.Collections.Generic.IEnumerable<string>) : string array =
    if isNull (box xs) then
        [||]
    else
        xs |> Seq.toArray

let private respondReadNuspec (root: JsonElement) : string =
    let input = root.GetProperty("input").GetString()

    try
        let reader = NuspecReader(XDocument.Parse input)

        let groups =
            reader.GetDependencyGroups()
            |> Seq.map (fun g ->
                {| targetFramework = shortFolderName g.TargetFramework
                   dependencies =
                    g.Packages
                    |> Seq.map (fun d ->
                        {| id = d.Id
                           hasVersionRange = not (isNull (box d.VersionRange))
                           versionRange =
                            (if isNull (box d.VersionRange) then
                                 ""
                             else
                                 d.VersionRange.ToNormalizedString())
                           ``include`` = stringArray d.Include
                           exclude = stringArray d.Exclude |})
                    |> Seq.toArray |})
            |> Seq.toArray

        let references =
            reader.GetReferenceGroups()
            |> Seq.map (fun g ->
                {| targetFramework = shortFolderName g.TargetFramework
                   files = stringArray g.Items |})
            |> Seq.toArray

        JsonSerializer.Serialize
            {| ok = true
               groups = groups
               references = references |}
    with _ ->
        JsonSerializer.Serialize {| ok = false |}

/// `LocalPackageFileCache.IsAllowedLibraryFile`: restore strips the OPC
/// packaging apparatus from a package's file list before the content model sees
/// it, so the oracle must too — a `.psmdcp` left inside a framework folder would
/// otherwise form an asset group that restore never sees.
let private isAllowedLibraryFile (path: string) : bool =
    match path with
    | "_rels/.rels"
    | "[Content_Types].xml" -> false
    | _ ->
        not (path.EndsWith("/", StringComparison.Ordinal))
        && not (path.EndsWith(".psmdcp", StringComparison.Ordinal))

/// `LockFileUtils`' compile-asset selection for one package: the content model
/// picks the best `ref/{tfm}` group, falling back to `lib/{tfm}` only when no
/// ref group is compatible at all (note: a *compatible but empty* ref group
/// still wins, and yields no compile assets); the nuspec `<references>` filter
/// then removes any `lib/`-rooted assembly the nuspec does not name.
let private respondSelectCompileAssets (root: JsonElement) : string =
    let framework = root.GetProperty("framework").GetString()

    let files =
        root.GetProperty("files").EnumerateArray()
        |> Seq.map (fun e -> e.GetString())
        |> Seq.filter isAllowedLibraryFile
        |> Seq.toArray

    let nuspec = root.GetProperty("nuspec").GetString()

    try
        let fw = NuGetFramework.Parse framework
        let conventions = ManagedCodeConventions(null)
        let items = ContentItemCollection()
        items.Load files

        let criteria = conventions.Criteria.ForFramework fw

        let group =
            items.FindBestItemGroup(criteria, conventions.Patterns.CompileRefAssemblies, conventions.Patterns.CompileLibAssemblies)

        let compile =
            if isNull (box group) then
                [||]
            else
                group.Items |> Seq.map (fun i -> i.Path) |> Seq.toArray

        // ApplyReferenceFilter: only `lib/`-rooted paths are filtered.
        let reader = NuspecReader(XDocument.Parse nuspec)
        let referenceGroups = reader.GetReferenceGroups() |> Seq.toArray

        let filtered =
            if referenceGroups.Length = 0 then
                compile
            else
                let nearest =
                    NuGetFrameworkUtility.GetNearest(referenceGroups, fw, (fun g -> g.TargetFramework))

                if isNull (box nearest) then
                    compile
                else
                    let allowed = HashSet<string>(nearest.Items, StringComparer.OrdinalIgnoreCase)

                    compile
                    |> Array.filter (fun p ->
                        not (p.StartsWith("lib/", StringComparison.Ordinal))
                        || allowed.Contains(Path.GetFileName p))

        JsonSerializer.Serialize {| ok = true; items = filtered |}
    with _ ->
        JsonSerializer.Serialize {| ok = false |}

let private respondSelectDependencyGroup (root: JsonElement) : string =
    let project = root.GetProperty("project").GetString()
    let input = root.GetProperty("input").GetString()

    try
        let p = NuGetFramework.Parse project
        let reader = NuspecReader(XDocument.Parse input)
        let groups = reader.GetDependencyGroups() |> Seq.toArray
        let candidates = groups |> Array.map (fun g -> g.TargetFramework)
        let reducer = FrameworkReducer()
        let nearest = reducer.GetNearest(p, candidates)

        let index =
            if isNull (box nearest) then
                -1
            else
                candidates |> Array.findIndex (fun c -> obj.ReferenceEquals(c, nearest))

        JsonSerializer.Serialize {| ok = true; nearest = index |}
    with _ ->
        JsonSerializer.Serialize {| ok = false |}

/// Write a bare `.nupkg` (a zip whose only entry is the package's `.nuspec`)
/// into `feedDir`. The local-folder feed reads dependency info straight from
/// the root nuspec, so no lib/ assets or `[Content_Types].xml` are needed.
let private writeNupkg (feedDir: string) (id: string) (version: string) (nuspec: string) =
    let path =
        Path.Combine(feedDir, sprintf "%s.%s.nupkg" (id.ToLowerInvariant()) (version.ToLowerInvariant()))

    use fs = File.Create path
    use zip = new ZipArchive(fs, ZipArchiveMode.Create)
    let entry = zip.CreateEntry(sprintf "%s.nuspec" id)
    use w = new StreamWriter(entry.Open())
    w.Write nuspec

/// The synthetic project-as-package: its single dependency group for the
/// target framework carries the direct requirements, so walking it reproduces
/// what restoring a project with those `PackageReference`s would do. `rootId`
/// is chosen to be absent from the supplied universe so it cannot collide.
let private synthesizeRootNuspec (rootId: string) (framework: NuGetFramework) (direct: JsonElement) : string =
    let deps =
        direct.EnumerateArray()
        |> Seq.map (fun d ->
            sprintf
                "<dependency id=\"%s\" version=\"%s\" />"
                (d.GetProperty("id").GetString())
                (d.GetProperty("range").GetString()))
        |> String.concat ""

    sprintf
        "<?xml version=\"1.0\"?><package xmlns=\"http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd\"><metadata><id>%s</id><version>1.0.0</version><authors>a</authors><description>d</description><dependencies><group targetFramework=\"%s\">%s</group></dependencies></metadata></package>"
        rootId
        (framework.GetShortFolderName())
        deps

/// A synthetic-root package id guaranteed absent from every caller-supplied id:
/// `__root__` is itself a legal package id, so a universe *or a direct
/// requirement* naming it would otherwise overwrite the root nupkg (or make the
/// root depend on itself and read as a cycle). Package ids are case-insensitive,
/// so the exclusion set is `OrdinalIgnoreCase`.
let private freshRootId (packages: JsonElement) (direct: JsonElement) : string =
    let taken = HashSet<string>(StringComparer.OrdinalIgnoreCase)

    for pkg in packages.EnumerateArray() do
        taken.Add(pkg.GetProperty("id").GetString()) |> ignore

    for d in direct.EnumerateArray() do
        taken.Add(d.GetProperty("id").GetString()) |> ignore

    let mutable rootId = "__oracle_root__"

    while taken.Contains rootId do
        rootId <- rootId + "_"

    rootId

/// A cycle reported by `GraphOperations.Analyze` is only a real restore failure
/// (NU1108) when it is reachable through *accepted* nodes; a cycle confined to a
/// branch the conflict resolver rejected never reaches the output, so restore
/// succeeds. The cycle node itself carries `Disposition = Cycle`, so liveness is
/// read off its ancestor chain: dead iff any ancestor was rejected.
let private cycleIsLive (cycleNode: GraphNode<RemoteResolveResult>) : bool =
    let rec anyRejectedAncestor (n: GraphNode<RemoteResolveResult>) =
        if isNull (box n) then false
        elif string n.Disposition = "Rejected" then true
        else anyRejectedAncestor n.OuterNode

    not (anyRejectedAncestor cycleNode.OuterNode)

let private respondResolve (root: JsonElement) : string =
    let framework = NuGetFramework.Parse(root.GetProperty("framework").GetString())

    let feedDir =
        Path.Combine(Path.GetTempPath(), "nuget-oracle-resolve-" + Guid.NewGuid().ToString("N"))

    Directory.CreateDirectory feedDir |> ignore

    try
        let rootId = freshRootId (root.GetProperty("packages")) (root.GetProperty("direct"))
        writeNupkg feedDir rootId "1.0.0" (synthesizeRootNuspec rootId framework (root.GetProperty("direct")))

        for pkg in root.GetProperty("packages").EnumerateArray() do
            writeNupkg
                feedDir
                (pkg.GetProperty("id").GetString())
                (pkg.GetProperty("version").GetString())
                (pkg.GetProperty("nuspec").GetString())

        let source = Repository.Factory.GetCoreV3 feedDir
        use cache = new SourceCacheContext()
        let logger = NullLogger.Instance
        let ctx = RemoteWalkContext(cache, PackageSourceMapping.GetPackageSourceMapping NullSettings.Instance, logger)
        ctx.RemoteLibraryProviders.Add(SourceRepositoryDependencyProvider(source, logger, cache, true, true))
        let walker = RemoteDependencyWalker ctx

        let rootRange =
            LibraryRange(rootId, VersionRange.Parse "[1.0.0]", LibraryDependencyTarget.Package)

        let node =
            walker.WalkAsync(rootRange, framework, null, RuntimeGraph.Empty, true)
            |> Async.AwaitTask
            |> Async.RunSynchronously

        let analyze = GraphOperations.Analyze node

        // Flatten only the *accepted* package nodes — the losers of a version
        // conflict are marked Rejected and excluded, exactly as restore's
        // Flattened set is built. An unresolved library matters only when it is
        // itself accepted: an unresolved dependency dangling off a rejected
        // branch never reaches the restore output, so restore still succeeds.
        let resolved = SortedDictionary<string, string>(StringComparer.Ordinal)
        let mutable anyUnresolved = false

        let rec visit (n: GraphNode<RemoteResolveResult>) =
            if
                not (isNull (box n.Item))
                && not (isNull (box n.Item.Key))
                && string n.Disposition = "Accepted"
            then
                let key = n.Item.Key

                if string key.Type = "unresolved" then
                    if key.Name <> rootId then
                        anyUnresolved <- true
                elif key.Name <> rootId && not (isNull (box key.Version)) then
                    resolved.[key.Name.ToLowerInvariant()] <- key.Version.ToNormalizedString()

            for c in n.InnerNodes do
                visit c

        visit node

        // Restore fails (produces no closure) on any of these, but only when the
        // fault lies on the *accepted* graph — `GraphOperations.Analyze` also
        // surfaces cycles/conflicts/downgrades confined to branches the conflict
        // resolver rejected, which `dotnet restore` discards and still succeeds.
        // The accepted-only filters mirror restore's own error reporting; each
        // was cross-checked against a real `dotnet restore`. Priority
        // missing > cycle > conflict > downgrade (all mean "no closure").
        let liveCycle = analyze.Cycles |> Seq.exists cycleIsLive

        let liveConflict =
            analyze.VersionConflicts
            |> Seq.exists (fun c -> string c.Selected.Disposition = "Accepted")

        let liveDowngrade =
            analyze.Downgrades
            |> Seq.exists (fun d -> string d.DowngradedTo.Disposition = "Accepted")

        let reason =
            if anyUnresolved then Some "missing"
            elif liveCycle then Some "cycle"
            elif liveConflict then Some "conflict"
            elif liveDowngrade then Some "downgrade"
            else None

        match reason with
        | Some r -> JsonSerializer.Serialize {| ok = true; resolved = false; reason = r |}
        | None ->
            let packages =
                [| for kv in resolved -> {| id = kv.Key; version = kv.Value |} |]

            JsonSerializer.Serialize
                {| ok = true
                   resolved = true
                   packages = packages |}
    finally
        try
            Directory.Delete(feedDir, true)
        with _ ->
            ()

[<EntryPoint>]
let main _argv =
    let mutable line = Console.In.ReadLine()

    while not (isNull line) do
        if line.Trim() <> "" then
            let response =
                try
                    use doc = JsonDocument.Parse line
                    let root = doc.RootElement

                    match root.GetProperty("op").GetString() with
                    | "parseVersion" -> respondParseVersion root
                    | "compareVersions" -> respondCompareVersions root
                    | "parseRange" -> respondParseRange root
                    | "rangeSatisfies" -> respondRangeSatisfies root
                    | "parseFramework" -> respondParseFrameworkWith NuGetFramework.Parse root
                    | "parseFolder" -> respondParseFrameworkWith NuGetFramework.ParseFolder root
                    | "isCompatible" -> respondIsCompatible root
                    | "getNearest" -> respondGetNearest root
                    | "readNuspec" -> respondReadNuspec root
                    | "selectDependencyGroup" -> respondSelectDependencyGroup root
                    | "selectCompileAssets" -> respondSelectCompileAssets root
                    | "resolve" -> respondResolve root
                    | other -> JsonSerializer.Serialize {| error = $"unknown op: %s{other}" |}
                with ex ->
                    JsonSerializer.Serialize {| error = ex.Message |}

            Console.Out.WriteLine response
            Console.Out.Flush()

        line <- Console.In.ReadLine()

    0

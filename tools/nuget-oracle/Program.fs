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
///   {"op":"restore","engine":"legacy"|"default","framework":tfm,
///      "packages":[{"id":..,"version":..,"nuspec":xml}, ..],
///      "direct":[{"id":..,"range":..}, ..]}
///     The end-to-end offline resolver oracle: a real restore of a one-project
///     PackageReference graph through `RestoreRunner`, the entry point
///     `dotnet restore` reaches, over a local feed built from the supplied
///     nuspecs. `engine` selects NuGet's dependency resolver: "legacy"
///     (`RestoreUseLegacyDependencyResolver`: RemoteDependencyWalker +
///     GraphOperations) or "default" (the .NET 10 SDK's
///     `DependencyGraphResolver`).
///     -> {"ok":true,"resolved":true,"packages":[{"id":lower,"version":norm}, ..]}
///          (sorted by lowercased id; the packages the assets file lists)
///      | {"ok":true,"resolved":false,"errors":["NU1107", ..]}
///          (every error-level code restore logged)
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

/// A project name guaranteed absent from every caller-supplied package id:
/// `__oracle_root__` is itself a legal package id, so a universe *or a direct
/// requirement* naming it would otherwise make the project depend on itself
/// and read as a cycle. Package ids are case-insensitive, so the exclusion set
/// is `OrdinalIgnoreCase`.
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

/// The restore errors a failed restore reports, by the codes the resolver's
/// outcome classes are drawn from.
let private restoreErrorCodes (lockFile: NuGet.ProjectModel.LockFile) : string array =
    lockFile.LogMessages
    |> Seq.filter (fun m -> m.Level = LogLevel.Error)
    |> Seq.map (fun m -> m.Code.ToString())
    |> Seq.distinct
    |> Seq.sort
    |> Seq.toArray

/// A real `dotnet restore` of a one-project PackageReference graph, through
/// `RestoreRunner` — the same entry point `dotnet restore` reaches, fed the
/// same dependency-graph spec it builds — against a fresh local feed and a
/// fresh global packages folder.
///
/// `engine` picks NuGet's dependency resolver: "legacy" sets
/// `RestoreUseLegacyDependencyResolver` (RemoteDependencyWalker +
/// GraphOperations), "default" leaves it unset, which selects the
/// `DependencyGraphResolver` the .NET 10 SDK runs. The spec carries what the
/// SDK's restore-graph generation puts there that bears on resolution:
/// PackageReference style, the one target framework, and NU1605 promoted to an
/// error (the SDK's default `WarningsAsErrors`). Audit is disabled; the SDK's
/// framework references, fallback imports and pruning list are omitted, since
/// none of them names a package in the synthetic universe.
let private respondRestore (root: JsonElement) : string =
    let framework = NuGetFramework.Parse(root.GetProperty("framework").GetString())
    let legacy =
        match root.GetProperty("engine").GetString() with
        | "legacy" -> true
        | "default" -> false
        | other -> failwithf "unknown engine %s" other

    let workDir =
        Path.Combine(Path.GetTempPath(), "nuget-oracle-restore-" + Guid.NewGuid().ToString("N"))

    let feedDir = Path.Combine(workDir, "feed")
    let packagesDir = Path.Combine(workDir, "gp")
    let objDir = Path.Combine(workDir, "obj")
    Directory.CreateDirectory feedDir |> ignore

    try
        for pkg in root.GetProperty("packages").EnumerateArray() do
            writeNupkg
                feedDir
                (pkg.GetProperty("id").GetString())
                (pkg.GetProperty("version").GetString())
                (pkg.GetProperty("nuspec").GetString())

        let projectName = freshRootId (root.GetProperty("packages")) (root.GetProperty("direct"))
        let projectPath = Path.Combine(workDir, projectName + ".csproj")
        let tfm = framework.GetShortFolderName()

        let dependencies = Text.Json.Nodes.JsonObject()

        for d in root.GetProperty("direct").EnumerateArray() do
            let dep = Text.Json.Nodes.JsonObject()
            dep["target"] <- Text.Json.Nodes.JsonValue.Create "Package"
            dep["version"] <- Text.Json.Nodes.JsonValue.Create(d.GetProperty("range").GetString())
            dependencies[d.GetProperty("id").GetString()] <- dep

        let restoreMetadata =
            Text.Json.Nodes.JsonObject.Parse(
                JsonSerializer.Serialize
                    {| projectUniqueName = projectPath
                       projectName = projectName
                       projectPath = projectPath
                       packagesPath = packagesDir
                       outputPath = objDir + string Path.DirectorySeparatorChar
                       projectStyle = "PackageReference"
                       originalTargetFrameworks = [| tfm |]
                       warningProperties = {| warnAsError = [| "NU1605" |] |}
                       restoreAuditProperties = {| enableAudit = "false" |} |}
            )
            :?> Text.Json.Nodes.JsonObject

        let sources = Text.Json.Nodes.JsonObject()
        sources[feedDir] <- Text.Json.Nodes.JsonObject()
        restoreMetadata["sources"] <- sources
        let restoreFrameworks = Text.Json.Nodes.JsonObject()
        let restoreFramework = Text.Json.Nodes.JsonObject()
        restoreFramework["targetAlias"] <- Text.Json.Nodes.JsonValue.Create tfm
        restoreFramework["projectReferences"] <- Text.Json.Nodes.JsonObject()
        restoreFrameworks[tfm] <- restoreFramework
        restoreMetadata["frameworks"] <- restoreFrameworks

        if legacy then
            restoreMetadata["restoreUseLegacyDependencyResolver"] <- Text.Json.Nodes.JsonValue.Create true

        let frameworks = Text.Json.Nodes.JsonObject()
        let projectFramework = Text.Json.Nodes.JsonObject()
        projectFramework["targetAlias"] <- Text.Json.Nodes.JsonValue.Create tfm
        projectFramework["dependencies"] <- dependencies
        frameworks[tfm] <- projectFramework

        let project = Text.Json.Nodes.JsonObject()
        project["version"] <- Text.Json.Nodes.JsonValue.Create "1.0.0"
        project["restore"] <- restoreMetadata
        project["frameworks"] <- frameworks

        let projects = Text.Json.Nodes.JsonObject()
        projects[projectPath] <- project
        let restoreSet = Text.Json.Nodes.JsonObject()
        restoreSet[projectPath] <- Text.Json.Nodes.JsonObject()
        let dg = Text.Json.Nodes.JsonObject()
        dg["format"] <- Text.Json.Nodes.JsonValue.Create 1
        dg["restore"] <- restoreSet
        dg["projects"] <- projects

        let dgPath = Path.Combine(workDir, "dg.json")
        File.WriteAllText(dgPath, dg.ToJsonString())
        let dgSpec = NuGet.ProjectModel.DependencyGraphSpec.Load dgPath

        use cache = new SourceCacheContext(NoCache = true)

        let args =
            RestoreArgs(
                CacheContext = cache,
                Log = NullLogger.Instance,
                GlobalPackagesFolder = packagesDir,
                DisableParallel = true,
                AllowNoOp = false
            )

        args.PreLoadedRequestProviders.Add(
            DependencyGraphSpecRequestProvider(RestoreCommandProvidersCache(), dgSpec, NullSettings.Instance)
        )

        let requests =
            RestoreRunner.GetRequests args |> Async.AwaitTask |> Async.RunSynchronously

        let results =
            RestoreRunner.RunWithoutCommit(requests, args)
            |> Async.AwaitTask
            |> Async.RunSynchronously

        let result = (Seq.exactlyOne results).Result
        let lockFile = result.LockFile

        if result.Success then
            let packages =
                lockFile.Targets
                |> Seq.filter (fun t -> isNull t.RuntimeIdentifier)
                |> Seq.collect (fun t -> t.Libraries)
                |> Seq.filter (fun l -> l.Type = "package")
                |> Seq.map (fun l -> l.Name.ToLowerInvariant(), l.Version.ToNormalizedString())
                |> Seq.distinct
                |> Seq.sort
                |> Seq.map (fun (id, version) -> {| id = id; version = version |})
                |> Seq.toArray

            JsonSerializer.Serialize
                {| ok = true
                   resolved = true
                   packages = packages |}
        else
            JsonSerializer.Serialize
                {| ok = true
                   resolved = false
                   errors = restoreErrorCodes lockFile |}
    finally
        try
            Directory.Delete(workDir, true)
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
                    | "restore" -> respondRestore root
                    | other -> JsonSerializer.Serialize {| error = $"unknown op: %s{other}" |}
                with ex ->
                    JsonSerializer.Serialize {| error = ex.Message |}

            Console.Out.WriteLine response
            Console.Out.Flush()

        line <- Console.In.ReadLine()

    0

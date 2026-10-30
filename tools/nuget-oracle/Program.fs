/// Differential-test oracle over the real NuGet client libraries
/// (test-only; never shipped in the LSP — see docs/nuget-restore-plan.md).
///
/// Usage: `nuget-oracle --nuget-from <sdk-dir>`, where `<sdk-dir>` is one .NET
/// SDK's directory (`<dotnet-root>/sdk/<version>/`).
///
/// Which NuGet answers: the one in `<sdk-dir>`, loaded from there at runtime.
/// The project references the NuGet packages for compilation only; none of
/// their assemblies is copied beside the oracle (see nuget-oracle.fsproj), so
/// the default load context cannot find them, and the `Resolving` handler
/// installed at startup serves every such name from `<sdk-dir>`. An SDK
/// directory has no other NuGet to fall back to, and every response that runs
/// restore reports the identity of the `NuGet.Commands` that executed, after
/// checking that each loaded `NuGet.*` assembly came from `<sdk-dir>`.
///
/// The alternative — reference a package version proved identical to the SDK's
/// — is not available. An SDK bundles a NuGet release-candidate build that is
/// not published: SDK 10.0.401 ships `NuGet.Commands`
/// `7.9.0-rc.42413+e34a38d2ae1fc26406a317517196e55c68ff83ab`, nuget.org has no
/// `7.9.0-rc.*` package, and its `7.9.0` is
/// `7.9.0+977537e19c6be57fead1411e6cf05f936bf1baf4`: a different commit, and
/// different bytes. (Likewise SDK 10.0.112's `7.0.3-rc.42308+95017c71…`
/// against the published `7.0.3+64bdb478…`.) A NuGet release can change
/// resolution while emitting the same dependency-graph spec
/// (docs/nuget-dgspec-boundary-plan.md, *Per-SDK drift*), so an oracle that
/// vouches for what `dotnet restore` does must run the SDK's own build, not
/// the nearest published one.
///
/// The packages' version is a compile-time floor: the runtime refuses an
/// assembly older than the one referenced, so an SDK whose NuGet predates it
/// fails loudly at the first load rather than answering.
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
///     -> {"ok":true,"resolved":true,"packages":[{"id":lower,"version":norm}, ..],
///         "nugetCommands":identity}
///          (sorted by lowercased id; the packages the assets file lists)
///      | {"ok":true,"resolved":false,"errors":["NU1107", ..],
///         "nugetCommands":identity}
///          (every error-level code restore logged)
///     where identity = {"informationalVersion":s,"path":s}: the
///     `AssemblyInformationalVersion` and load path of the `NuGet.Commands`
///     assembly that ran the restore.
///   {"op":"normaliseDgSpec","text":s}
///     NuGet's reading of a dependency-graph spec: `DependencyGraphSpec.Load`
///     over `text`, projected to the canonical shape
///     `crates/nuget/tests/all/dgspec_diff.rs` compares with.
///     -> {"ok":true,"spec":{"restore":[..],"projects":[..]},"nugetCommands":identity}
///      | {"ok":false,"exceptionType":s,"message":s,"nugetCommands":identity}
///          (the reader refused the document)
///   {"op":"nuGetLogCodes"}
///     -> {"ok":true,"names":[..],"nugetCommands":identity}
///          (`NuGetLogCode`'s names, `Undefined` included)
///
/// Any per-request exception is reported as {"error":..} on that line; the
/// process itself never dies mid-batch.
module NuGetOracle.Program

open System
open System.IO
open System.Reflection
open System.Runtime.CompilerServices
open System.Runtime.Loader
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

/// The `NuGet.Commands` that is executing, as `{informationalVersion; path}`.
///
/// Fails if any loaded `NuGet.*` assembly came from anywhere but `sdkDir`: the
/// identity names one SDK's build, so it is only true of a run that used that
/// build throughout.
let private nugetCommandsIdentity (sdkDir: string) =
    for asm in AppDomain.CurrentDomain.GetAssemblies() do
        let name = asm.GetName().Name

        if name.StartsWith("NuGet.", StringComparison.Ordinal) then
            let dir = Path.GetDirectoryName asm.Location

            if not (String.Equals(dir, sdkDir, StringComparison.Ordinal)) then
                failwithf "%s was loaded from %s, not the SDK directory %s" name asm.Location sdkDir

    let commands = typeof<RestoreRunner>.Assembly

    {| informationalVersion =
        commands.GetCustomAttribute<AssemblyInformationalVersionAttribute>().InformationalVersion
       path = commands.Location |}

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
let private respondRestore (sdkDir: string) (root: JsonElement) : string =
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
                   packages = packages
                   nugetCommands = nugetCommandsIdentity sdkDir |}
        else
            JsonSerializer.Serialize
                {| ok = true
                   resolved = false
                   errors = restoreErrorCodes lockFile
                   nugetCommands = nugetCommandsIdentity sdkDir |}
    finally
        try
            Directory.Delete(workDir, true)
        with _ ->
            ()

/// A flags enum as its integer value: the canonical currency for
/// `LibraryIncludeFlags`, `LibraryDependencyTarget` and
/// `FrameworkDependencyFlags`, whose composite names (`All`, `None`) would
/// otherwise make two spellings of one value compare unequal.
let private flagsValue (x: 'e) : int64 = Convert.ToInt64(box x)

/// A property the SDK's NuGet has but the compile-time reference (see
/// nuget-oracle.fsproj) predates, read by name. Fails if it is absent, so an
/// SDK that lacks it errors rather than reporting a default.
let private runtimeProperty (o: obj) (name: string) : obj =
    match o.GetType().GetProperty(name) with
    | null -> failwithf "%s has no property %s" (o.GetType().FullName) name
    | p -> p.GetValue o

/// Every public instance property of each type `respondNormaliseDgSpec`
/// projects, as the projection accounts for it: projected, or excluded with the
/// reason given. The SDK's NuGet is newer than the one the oracle compiles
/// against, so a property it reads a dgspec key into could be invisible to the
/// projection's source; `checkProjectionCoverage` turns that into an error
/// instead of a silently partial projection.
let private projectionCoverage: (string * string list) list =
    [ "NuGet.ProjectModel.PackageSpec",
      [ "FilePath"; "Name"; "Version"; "IsDefaultVersion"; "TargetFrameworks"; "RuntimeGraph"
        "RestoreSettings"; "RestoreMetadata"
        // Derived from FilePath.
        "BaseDirectory" ]
      "NuGet.ProjectModel.TargetFrameworkInformation",
      [ "TargetAlias"; "FrameworkName"; "Dependencies"; "Imports"; "AssetTargetFallback"; "Warn"
        "DownloadDependencies"; "CentralPackageVersions"; "FrameworkReferences"; "PackagesToPrune"
        "RuntimeIdentifierGraphPath" ]
      "NuGet.ProjectModel.ProjectRestoreMetadata",
      [ "ProjectStyle"; "ProjectPath"; "ProjectJsonPath"; "OutputPath"; "ProjectName"
        "ProjectUniqueName"; "Sources"; "PackagesPath"; "CacheFilePath"; "FallbackFolders"
        "ConfigFilePaths"; "TargetFrameworks"; "OriginalTargetFrameworks"; "CrossTargeting"
        "LegacyPackagesDirectory"; "Files"; "ValidateRuntimeAssets"; "SkipContentFileWrite"
        "ProjectWideWarningProperties"; "RestoreLockProperties"; "CentralPackageVersionsEnabled"
        "CentralPackageVersionOverrideDisabled"; "CentralPackageFloatingVersionsEnabled"
        "CentralPackageTransitivePinningEnabled"; "RestoreAuditProperties"; "SdkAnalysisLevel"
        "UsingMicrosoftNETSdk"; "UseLegacyDependencyResolver"; "RestoreDoNotWriteDependencyGraphSpec" ]
      "NuGet.ProjectModel.ProjectRestoreMetadataFrameworkInfo",
      [ "TargetAlias"; "FrameworkName"; "ProjectReferences" ]
      "NuGet.ProjectModel.ProjectRestoreReference",
      [ "ProjectUniqueName"; "ProjectPath"; "IncludeAssets"; "ExcludeAssets"; "PrivateAssets" ]
      "NuGet.ProjectModel.WarningProperties",
      [ "WarningsAsErrors"; "NoWarn"; "AllWarningsAsErrors"; "WarningsNotAsErrors" ]
      "NuGet.ProjectModel.RestoreAuditProperties",
      [ "EnableAudit"; "AuditLevel"; "AuditMode"; "SuppressedAdvisories" ]
      "NuGet.ProjectModel.RestoreLockProperties",
      [ "RestorePackagesWithLockFile"; "NuGetLockFilePath"; "RestoreLockedMode" ]
      "NuGet.LibraryModel.LibraryDependency",
      [ "LibraryRange"; "IncludeType"; "SuppressParent"; "NoWarn"; "AutoReferenced"
        "VersionCentrallyManaged"; "ReferenceType"; "GeneratePathProperty"; "Aliases"
        "VersionOverride"
        // LibraryRange.Name.
        "Name" ]
      "NuGet.LibraryModel.LibraryRange", [ "Name"; "VersionRange"; "TypeConstraint" ]
      "NuGet.LibraryModel.DownloadDependency", [ "Name"; "VersionRange" ]
      "NuGet.LibraryModel.FrameworkDependency", [ "Name"; "PrivateAssets" ] ]

let private checkProjectionCoverage () =
    let anchor = typeof<NuGet.ProjectModel.PackageSpec>

    for (typeName, covered) in projectionCoverage do
        let t =
            [ anchor.Assembly; typeof<LibraryDependency>.Assembly ]
            |> List.tryPick (fun a -> Option.ofObj (a.GetType typeName))

        match t with
        | None -> failwithf "projection names %s, which the SDK's NuGet does not define" typeName
        | Some t ->
            let actual =
                t.GetProperties(BindingFlags.Public ||| BindingFlags.Instance)
                |> Array.map (fun p -> p.Name)
                |> Set.ofArray

            let missing = Set.difference actual (Set.ofList covered)

            if not missing.IsEmpty then
                failwithf "%s has properties the dgspec projection does not account for: %s" typeName (String.Join(", ", missing))

let private frameworkCanonical (f: NuGetFramework) : obj =
    if isNull (box f) then
        null
    else
        box
            {| framework = f.Framework
               version = string f.Version
               platform = f.Platform
               platformVersion = string f.PlatformVersion
               profile = f.Profile |}

let private rangeCanonical (r: VersionRange) : string =
    if isNull (box r) then null else r.ToNormalizedString()

let private versionCanonical (v: NuGetVersion) : string =
    if isNull (box v) then null else v.ToFullString()

let private stringsOrNull (xs: seq<string>) : string array =
    if isNull (box xs) then null else Seq.toArray xs

let private dependencyCanonical (d: LibraryDependency) =
    {| name = d.Name
       range = rangeCanonical d.LibraryRange.VersionRange
       target = flagsValue d.LibraryRange.TypeConstraint
       ``include`` = flagsValue d.IncludeType
       suppressParent = flagsValue d.SuppressParent
       noWarn = d.NoWarn |> Seq.map string |> Seq.toArray
       autoReferenced = d.AutoReferenced
       versionCentrallyManaged = d.VersionCentrallyManaged
       referenceType = string d.ReferenceType
       generatePathProperty = d.GeneratePathProperty
       aliases = d.Aliases
       versionOverride = rangeCanonical d.VersionOverride |}

let private targetFrameworkCanonical (t: NuGet.ProjectModel.TargetFrameworkInformation) =
    {| targetAlias = t.TargetAlias
       framework = frameworkCanonical t.FrameworkName
       dependencies = t.Dependencies |> Seq.map dependencyCanonical |> Seq.toArray
       imports = t.Imports |> Seq.map frameworkCanonical |> Seq.toArray
       assetTargetFallback = t.AssetTargetFallback
       warn = t.Warn
       downloadDependencies =
        t.DownloadDependencies
        |> Seq.map (fun d -> {| name = d.Name; range = rangeCanonical d.VersionRange |})
        |> Seq.toArray
       centralPackageVersions =
        if isNull t.CentralPackageVersions then
            null
        else
            t.CentralPackageVersions
            |> Seq.map (fun kv ->
                {| key = kv.Key
                   name = kv.Value.Name
                   range = rangeCanonical kv.Value.VersionRange |})
            |> Seq.toArray
       frameworkReferences =
        if isNull t.FrameworkReferences then
            null
        else
            t.FrameworkReferences
            |> Seq.map (fun f ->
                {| name = f.Name
                   privateAssets = flagsValue f.PrivateAssets |})
            |> Seq.toArray
       packagesToPrune =
        if isNull t.PackagesToPrune then
            null
        else
            t.PackagesToPrune
            |> Seq.map (fun kv ->
                {| key = kv.Key
                   name = kv.Value.Name
                   range = rangeCanonical kv.Value.VersionRange |})
            |> Seq.toArray
       runtimeIdentifierGraphPath = t.RuntimeIdentifierGraphPath |}

let private restoreMetadataCanonical (m: NuGet.ProjectModel.ProjectRestoreMetadata) : obj =
    if isNull m then
        null
    else
        let warnings = m.ProjectWideWarningProperties
        let lockProps = m.RestoreLockProperties
        let audit = m.RestoreAuditProperties

        box
            {| projectStyle = string m.ProjectStyle
               projectPath = m.ProjectPath
               projectJsonPath = m.ProjectJsonPath
               outputPath = m.OutputPath
               projectName = m.ProjectName
               projectUniqueName = m.ProjectUniqueName
               sources =
                if isNull m.Sources then
                    null
                else
                    m.Sources
                    |> Seq.map (fun s ->
                        {| name = s.Name
                           source = s.Source
                           isEnabled = s.IsEnabled
                           protocolVersion = s.ProtocolVersion |})
                    |> Seq.toArray
               packagesPath = m.PackagesPath
               cacheFilePath = m.CacheFilePath
               fallbackFolders = stringsOrNull m.FallbackFolders
               configFilePaths = stringsOrNull m.ConfigFilePaths
               frameworks =
                if isNull m.TargetFrameworks then
                    null
                else
                    m.TargetFrameworks
                    |> Seq.map (fun f ->
                        {| targetAlias = f.TargetAlias
                           framework = frameworkCanonical f.FrameworkName
                           projectReferences =
                            f.ProjectReferences
                            |> Seq.map (fun r ->
                                {| projectUniqueName = r.ProjectUniqueName
                                   projectPath = r.ProjectPath
                                   includeAssets = flagsValue r.IncludeAssets
                                   excludeAssets = flagsValue r.ExcludeAssets
                                   privateAssets = flagsValue r.PrivateAssets |})
                            |> Seq.toArray |})
                    |> Seq.toArray
               originalTargetFrameworks = stringsOrNull m.OriginalTargetFrameworks
               crossTargeting = m.CrossTargeting
               legacyPackagesDirectory = m.LegacyPackagesDirectory
               files = (if isNull m.Files then 0 else m.Files.Count)
               validateRuntimeAssets = m.ValidateRuntimeAssets
               skipContentFileWrite = m.SkipContentFileWrite
               warningProperties =
                if isNull warnings then
                    null
                else
                    box
                        {| allWarningsAsErrors = warnings.AllWarningsAsErrors
                           warningsAsErrors = warnings.WarningsAsErrors |> Seq.map string |> Seq.toArray
                           noWarn = warnings.NoWarn |> Seq.map string |> Seq.toArray
                           warningsNotAsErrors = warnings.WarningsNotAsErrors |> Seq.map string |> Seq.toArray |}
               restoreLockProperties =
                if isNull lockProps then
                    null
                else
                    box
                        {| restorePackagesWithLockFile = lockProps.RestorePackagesWithLockFile
                           nuGetLockFilePath = lockProps.NuGetLockFilePath
                           restoreLockedMode = lockProps.RestoreLockedMode |}
               centralPackageVersionsEnabled = m.CentralPackageVersionsEnabled
               centralPackageVersionOverrideDisabled = m.CentralPackageVersionOverrideDisabled
               centralPackageFloatingVersionsEnabled = m.CentralPackageFloatingVersionsEnabled
               centralPackageTransitivePinningEnabled = m.CentralPackageTransitivePinningEnabled
               restoreAuditProperties =
                if isNull audit then
                    null
                else
                    box
                        {| enableAudit = audit.EnableAudit
                           auditLevel = audit.AuditLevel
                           auditMode = audit.AuditMode
                           suppressedAdvisories =
                            (if isNull audit.SuppressedAdvisories then
                                 null
                             else
                                 Seq.toArray audit.SuppressedAdvisories) |}
               sdkAnalysisLevel = versionCanonical m.SdkAnalysisLevel
               usingMicrosoftNETSdk = m.UsingMicrosoftNETSdk
               useLegacyDependencyResolver = m.UseLegacyDependencyResolver
               restoreDoNotWriteDependencyGraphSpec = runtimeProperty m "RestoreDoNotWriteDependencyGraphSpec" |}

let private packageSpecCanonical (p: NuGet.ProjectModel.PackageSpec) =
    {| name = p.Name
       filePath = p.FilePath
       version = versionCanonical p.Version
       isDefaultVersion = p.IsDefaultVersion
       runtimeGraphEntries =
        (if isNull p.RuntimeGraph then
             0
         else
             p.RuntimeGraph.Runtimes.Count + p.RuntimeGraph.Supports.Count)
       hideWarningsAndErrors =
        (not (isNull p.RestoreSettings)) && p.RestoreSettings.HideWarningsAndErrors
       restore = restoreMetadataCanonical p.RestoreMetadata
       frameworks = p.TargetFrameworks |> Seq.map targetFrameworkCanonical |> Seq.toArray |}

/// NuGet's own reading of a dependency-graph spec: `DependencyGraphSpec.Load`
/// over `text` written to a file as UTF-8 without a BOM (a leading U+FEFF in
/// `text` therefore reaches the reader as BOM bytes), projected to the
/// canonical shape `crates/nuget/tests/all/dgspec_diff.rs` compares ours with.
/// Arrays keep NuGet's order; the comparison decides which of them are sets.
let private respondNormaliseDgSpec (sdkDir: string) (root: JsonElement) : string =
    checkProjectionCoverage ()
    let text = root.GetProperty("text").GetString()

    let path =
        Path.Combine(Path.GetTempPath(), "nuget-oracle-dgspec-" + Guid.NewGuid().ToString("N") + ".json")

    try
        File.WriteAllText(path, text, Text.UTF8Encoding(false))

        let loaded =
            try
                Ok(NuGet.ProjectModel.DependencyGraphSpec.Load path)
            with ex ->
                Error ex

        match loaded with
        | Ok spec ->
            JsonSerializer.Serialize
                {| ok = true
                   spec =
                    {| restore = Seq.toArray spec.Restore
                       projects = spec.Projects |> Seq.map packageSpecCanonical |> Seq.toArray |}
                   nugetCommands = nugetCommandsIdentity sdkDir |}
        | Error ex ->
            JsonSerializer.Serialize
                {| ok = false
                   exceptionType = ex.GetType().FullName
                   message = ex.Message
                   nugetCommands = nugetCommandsIdentity sdkDir |}
    finally
        try
            File.Delete path
        with _ ->
            ()

/// `NuGetLogCode`'s names, which decide which warning codes a dgspec's lists
/// keep (the reader drops any `Enum.TryParse` refuses).
let private respondNuGetLogCodes (sdkDir: string) : string =
    JsonSerializer.Serialize
        {| ok = true
           names = Enum.GetNames(typeof<NuGetLogCode>)
           nugetCommands = nugetCommandsIdentity sdkDir |}

/// The request loop. Kept out of `main` so that compiling `main` never needs a
/// NuGet type, which would load NuGet before the `Resolving` handler exists.
[<MethodImpl(MethodImplOptions.NoInlining)>]
let private serve (sdkDir: string) =
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
                    | "restore" -> respondRestore sdkDir root
                    | "normaliseDgSpec" -> respondNormaliseDgSpec sdkDir root
                    | "nuGetLogCodes" -> respondNuGetLogCodes sdkDir
                    | other -> JsonSerializer.Serialize {| error = $"unknown op: %s{other}" |}
                with ex ->
                    JsonSerializer.Serialize {| error = ex.Message |}

            Console.Out.WriteLine response
            Console.Out.Flush()

        line <- Console.In.ReadLine()

[<EntryPoint>]
let main argv =
    match argv with
    | [| "--nuget-from"; dir |] ->
        let sdkDir = Path.TrimEndingDirectorySeparator(Path.GetFullPath dir)

        if not (File.Exists(Path.Combine(sdkDir, "NuGet.Commands.dll"))) then
            eprintfn "nuget-oracle: %s has no NuGet.Commands.dll; expected an SDK directory" sdkDir
            2
        else
            // Only names the default context cannot place reach this handler:
            // the NuGet set and its dependencies, all excluded from the build
            // output. Each is served from the SDK, as the SDK itself does.
            AssemblyLoadContext.Default.add_Resolving (
                Func<AssemblyLoadContext, AssemblyName, Assembly>(fun context name ->
                    let candidate = Path.Combine(sdkDir, name.Name + ".dll")

                    if File.Exists candidate then
                        context.LoadFromAssemblyPath candidate
                    else
                        null)
            )

            serve sdkDir
            0
    | _ ->
        eprintfn "usage: nuget-oracle --nuget-from <sdk-dir>"
        2

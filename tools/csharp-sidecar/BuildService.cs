namespace CSharpSidecar;

using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Emit;
using Microsoft.CodeAnalysis.MSBuild;

/// <summary>
/// Loads csprojs through <see cref="MSBuildWorkspace"/> and drives the
/// metadata-only emit. Each <see cref="BuildMetadata"/> call uses a fresh
/// workspace pinned to the request's <c>Configuration</c> and
/// <c>TargetFramework</c> properties; MSBuild bakes globals at workspace
/// construction time, so per-call workspaces are the only way to honour
/// per-call properties without a global property cache. The JSON-RPC loop
/// in <see cref="Program"/> serialises requests, so workspace recreation
/// is not on a hot path.
/// </summary>
/// <remarks>
/// This class deliberately lives in its own file: the type metadata transitively
/// references <c>Microsoft.Build.*</c> assemblies, and the JIT only resolves
/// those references when the type is first touched. <see cref="Program"/>
/// registers <c>MSBuildLocator</c> before anything that mentions
/// <c>BuildService</c> by name, so the registration always wins the race
/// against assembly resolution (see the MSBuildLocator-coupling risk in
/// <c>docs/completed/csharp-sidecar-plan.md</c>).
/// </remarks>
internal sealed class BuildService
{
    private BuildService() { }

    public static BuildService Create() => new();

    /// <summary>
    /// Load the csproj at <paramref name="csprojPath"/> via MSBuild, derive the
    /// content-addressed cache key per <c>docs/completed/csharp-sidecar-plan.md</c> D6,
    /// and either return a cached metadata DLL or drive a fresh metadata-only
    /// emit and publish it atomically inside <paramref name="cacheRoot"/>.
    /// </summary>
    /// <param name="csprojPath">Top csproj. Must exist on disk and appear as a
    /// key in <paramref name="projectTfms"/>.</param>
    /// <param name="configuration">MSBuild <c>Configuration</c> property
    /// (typically <c>"Debug"</c> or <c>"Release"</c>).</param>
    /// <param name="targetFramework">Legacy hint preserved for the
    /// non-multi-TFM happy path; the per-project TFM actually applied to
    /// each closure node comes from <paramref name="projectTfms"/>. The two
    /// agree for the top csproj in well-formed callers, but the map wins.</param>
    /// <param name="projectTfms">Closure-wide TFM map: every csproj in the
    /// top's <c>&lt;ProjectReference&gt;</c> closure (top included) keyed to
    /// the short-form TFM NuGet's restore selected for it. A missing entry
    /// for any closure member is a hard error
    /// (<see cref="BuildMetadataOutcome.MissingProjectTfmOutcome"/>) per D5 of
    /// <c>docs/completed/multi-tfm-resolution-plan.md</c>.</param>
    /// <param name="cacheRoot">Directory under which the cache lives; must be
    /// creatable. The sidecar publishes DLLs into a two-character prefix
    /// shard underneath.</param>
    public BuildMetadataOutcome BuildMetadata(
        string csprojPath,
        string configuration,
        string targetFramework,
        IReadOnlyDictionary<string, string> projectTfms,
        string cacheRoot)
    {
        if (!File.Exists(csprojPath))
        {
            return BuildMetadataOutcome.NewNotFound(csprojPath);
        }

        // Canonicalise the top path and the projectTfms keys so that the
        // wire-side path form (Rust's closure walker emits
        // `top_dir/../leaf/Leaf.csproj`) matches Roslyn's canonical
        // `Project.FilePath` form (which strips `..`). Extracted as a static
        // helper so the policy can be exercised by xUnit without standing up
        // MSBuildWorkspace; the path normalisation is what makes the lookup
        // order-of-magnitudes-cheaper than `realpath`.
        var canonicalCsprojPath = Path.GetFullPath(csprojPath);
        var canonicalProjectTfms = CanonicaliseProjectTfms(projectTfms);

        try
        {
            Directory.CreateDirectory(cacheRoot);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return BuildMetadataOutcome.NewCacheUnwritable(cacheRoot, ex.Message);
        }

        // Hard-error if the top csproj is not in the closure map (D5). The
        // top's TFM is taken from the map even though `targetFramework` is
        // also on the wire — they should agree in well-formed callers; if
        // they don't, the map wins (it's what the Rust closure walker
        // actually saw on disk via project.assets.json).
        if (!canonicalProjectTfms.TryGetValue(canonicalCsprojPath, out var topTfm))
        {
            return BuildMetadataOutcome.NewMissingProjectTfm(canonicalCsprojPath);
        }

        // Open the top csproj under its own TFM. This workspace doubles as
        // the discovery surface: MSBuild loads the full <ProjectReference>
        // closure into the solution (some transitively-referenced csprojs
        // may evaluate under the wrong TFM here — that's fine, we only walk
        // their ProjectReference edges, never emit from this workspace).
        // The same workspace is then reused later for emitting the top
        // itself, after PE-substituting its direct refs to the leaves'
        // already-emitted DLLs.
        var topWorkspace = MakeWorkspace(configuration, topTfm);
        var loadResult = LoadProject(topWorkspace, canonicalCsprojPath);
        switch (loadResult)
        {
            case ProjectLoadOutcome.CsprojNotFound nf:
                topWorkspace.Dispose();
                return BuildMetadataOutcome.NewNotFound(nf.Path);
            case ProjectLoadOutcome.Failed f:
                topWorkspace.Dispose();
                return BuildMetadataOutcome.NewLoadFailed(f.Diagnostics);
            case ProjectLoadOutcome.LoadedOk loaded:
                try
                {
                    return EmitClosure(loaded, canonicalCsprojPath, configuration, canonicalProjectTfms, cacheRoot, topWorkspace);
                }
                finally
                {
                    // EmitClosure may have created and disposed per-project
                    // workspaces internally but holds the top one open
                    // throughout — release it now whether or not the closure
                    // emit succeeded.
                    topWorkspace.Dispose();
                }
            default:
                topWorkspace.Dispose();
                throw new InvalidOperationException($"Unexpected load outcome: {loadResult.GetType().Name}");
        }
    }

    /// <summary>
    /// Canonicalise every key in <paramref name="projectTfms"/> via
    /// <see cref="Path.GetFullPath(string)"/> so that callers passing
    /// non-canonical forms (Rust's closure walker emits
    /// <c>top_dir/../leaf/Leaf.csproj</c>) match the canonical
    /// <see cref="Project.FilePath"/> Roslyn returns. Later-key-wins on
    /// collision, which is the same precedence the underlying dictionary
    /// would have used; callers shouldn't construct colliding inputs in the
    /// first place (every <see cref="Path.GetFullPath(string)"/> output is
    /// unique for a unique on-disk csproj).
    /// </summary>
    /// <remarks>
    /// Exposed as <c>internal</c> so the xUnit suite can pin the policy
    /// without standing up MSBuildWorkspace — the production path is
    /// <see cref="BuildMetadata"/>.
    /// </remarks>
    internal static IReadOnlyDictionary<string, string> CanonicaliseProjectTfms(
        IReadOnlyDictionary<string, string> projectTfms)
    {
        var result = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var kv in projectTfms)
        {
            result[Path.GetFullPath(kv.Key)] = kv.Value;
        }
        return result;
    }

    /// <summary>
    /// Construct a fresh <see cref="MSBuildWorkspace"/> pinned to the supplied
    /// <c>Configuration</c> and <c>TargetFramework</c> properties. MSBuild
    /// bakes global properties at workspace construction time so per-call
    /// (and in the multi-TFM closure, per-project) workspaces are the only
    /// way to honour per-project TFMs.
    /// </summary>
    private static MSBuildWorkspace MakeWorkspace(string configuration, string targetFramework)
    {
        var properties = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)
        {
            ["Configuration"] = configuration,
            ["TargetFramework"] = targetFramework,
        };
        return MSBuildWorkspace.Create(properties);
    }

    /// <summary>
    /// Walk the loaded top project's <c>&lt;ProjectReference&gt;</c> closure
    /// (D7), emit each project leaves-first under its <em>own</em> TFM
    /// (taken from <paramref name="projectTfms"/>), and return the top's
    /// outcome with the list of transitive (csproj, dll) entries threaded
    /// through.
    /// </summary>
    /// <remarks>
    /// Phase 4 of <c>docs/completed/multi-tfm-resolution-plan.md</c> replaces the
    /// shared-workspace model (where all closure nodes loaded under the
    /// top's TFM) with one MSBuild workspace per project. The
    /// <paramref name="topWorkspace"/> stays alive throughout because we
    /// reuse it for the top's own emit at the end of the walk; non-top
    /// closure nodes get a fresh workspace each, opened under their own TFM
    /// and disposed once they have emitted. Cross-workspace dependency
    /// linkage is via emitted-DLL <see cref="PortableExecutableReference"/>:
    /// when we emit project P, every <c>&lt;ProjectReference&gt;</c> on P
    /// is rewritten in-solution to a PE ref pointing at the already-emitted
    /// downstream DLL. Roslyn's <see cref="EmitOptions"/>-aware
    /// deterministic key hashes that PE's MVID, so the cache cascade is the
    /// same Merkle structure the shared-workspace version had — just routed
    /// through file bytes instead of in-memory <c>CompilationReference</c>s.
    /// </remarks>
    private static BuildMetadataOutcome EmitClosure(
        ProjectLoadOutcome.LoadedOk loaded,
        string topCsprojPath,
        string configuration,
        IReadOnlyDictionary<string, string> projectTfms,
        string cacheRoot,
        MSBuildWorkspace topWorkspace)
    {
        // Per-project workspaces by canonical csproj path. The top is already
        // loaded in `topWorkspace`; every other closure node listed in
        // `projectTfms` gets a fresh workspace pinned to *its* TFM. These
        // workspaces are the source of truth for the project-reference graph:
        // a TFM-conditional `<ProjectReference Condition="...">` only
        // materialises in the workspace whose `TargetFramework` makes the
        // condition fire, so reading edges from the top's workspace alone
        // would silently drop TFM-conditional inner refs (the multi-tfm-cond
        // fixture pins this).
        var workspaces = new Dictionary<string, (MSBuildWorkspace Workspace, ProjectId Pid)>(StringComparer.Ordinal)
        {
            [topCsprojPath] = (topWorkspace, loaded.Project.Id),
        };
        var loadDiagsByPath = new Dictionary<string, WorkspaceDiagnosticDto[]>(StringComparer.Ordinal)
        {
            [topCsprojPath] = loaded.Diagnostics,
        };

        try
        {
            // Pre-open per-project workspaces for every non-top closure
            // member. We do this eagerly (rather than lazily during the
            // topo walk) because the edge-discovery pass below needs the
            // workspaces already loaded — we can't know the edges until
            // each project has been evaluated under its own TFM.
            foreach (var (path, tfm) in projectTfms)
            {
                if (string.Equals(path, topCsprojPath, StringComparison.Ordinal)) continue;
                var ws = MakeWorkspace(configuration, tfm);
                ProjectLoadOutcome leafLoad;
                try
                {
                    leafLoad = LoadProject(ws, path);
                }
                catch
                {
                    ws.Dispose();
                    throw;
                }
                switch (leafLoad)
                {
                    case ProjectLoadOutcome.CsprojNotFound nf:
                        ws.Dispose();
                        return BuildMetadataOutcome.NewNotFound(nf.Path);
                    case ProjectLoadOutcome.Failed f:
                        ws.Dispose();
                        return BuildMetadataOutcome.NewLoadFailed(f.Diagnostics);
                    case ProjectLoadOutcome.LoadedOk lo:
                        workspaces[path] = (ws, lo.Project.Id);
                        loadDiagsByPath[path] = lo.Diagnostics;
                        break;
                    default:
                        ws.Dispose();
                        throw new InvalidOperationException(
                            $"Unexpected leaf load outcome: {leafLoad.GetType().Name}");
                }
            }

            // Read direct ProjectReferences from each project's *own*
            // workspace view. This is the load-bearing change vs. the
            // prior shared-workspace implementation: a leaf evaluated
            // under its own TFM materialises the TFM-conditional refs the
            // top workspace would have skipped.
            var directDepsByPath = new Dictionary<string, List<string>>(StringComparer.Ordinal);
            foreach (var (path, (ws, pid)) in workspaces)
            {
                var p = ws.CurrentSolution.GetProject(pid)
                    ?? throw new InvalidOperationException(
                        $"Project {pid} missing from workspace solution for {path}");
                directDepsByPath[path] = p.ProjectReferences
                    .Select(r =>
                        ws.CurrentSolution.GetProject(r.ProjectId)?.FilePath is { } refFp
                            ? Path.GetFullPath(refFp)
                            : throw new InvalidOperationException(
                                $"ProjectReference on {p.Name} has no FilePath in its own workspace"))
                    .ToList();
            }

            // D5: every direct dep observed in a per-project workspace must
            // appear in projectTfms. If the Rust closure walker missed a
            // leaf — e.g. it didn't follow a TFM-conditional inner ref — we
            // surface the gap as MissingProjectTfm rather than silently
            // falling back to the consumer's TFM. The first missing path
            // wins; callers fix one at a time.
            foreach (var (_, deps) in directDepsByPath)
            {
                foreach (var dep in deps)
                {
                    if (!projectTfms.ContainsKey(dep))
                    {
                        return BuildMetadataOutcome.NewMissingProjectTfm(dep);
                    }
                }
            }

            // Topological sort over the path-keyed graph rooted at the top.
            List<string> orderedPaths;
            try
            {
                orderedPaths = TopoSortByPaths(topCsprojPath, directDepsByPath);
            }
            catch (CycleDetectedException ex)
            {
                return BuildMetadataOutcome.NewLoadFailed(loaded.Diagnostics.Concat(new[]
                {
                    new WorkspaceDiagnosticDto(
                        Kind: nameof(WorkspaceDiagnosticKind.Failure),
                        Message: $"Cycle in <ProjectReference> closure: {ex.Message}",
                        FilePath: topCsprojPath),
                }).ToArray());
            }

            // Tracks projects we've already emitted in this closure: maps
            // csproj path → (cache key, dll path). Topo order guarantees that
            // when we look up a dependency here, it's already present.
            var emitted = new Dictionary<string, (byte[] Key, string DllPath)>(StringComparer.Ordinal);
            var transitiveRefs = new List<TransitiveProjectRefDto>();
            // Compiler diagnostics from transitive emits get aggregated into
            // the top response — `buildMetadata` now drives those emits on
            // the caller's behalf, so the caller would otherwise have no
            // other channel to learn about a leaf's warnings. Cache hits
            // contribute nothing (Built.CompilerDiagnostics is empty in
            // that case), mirroring the top-level cache-hit policy:
            // diagnostics are attached to the call that actually
            // re-derived them.
            var transitiveCompilerDiagnostics = new List<CompilerDiagnosticDto>();
            BuildMetadataOutcome.Built? topBuilt = null;

            foreach (var projectCsproj in orderedPaths)
            {
                // D6 step 5: hash the keys of every transitively-referenced
                // project into this project's key. Roslyn's detkey already
                // cascades via the substituted PE refs' MVIDs (since each
                // leaf is emitted with Deterministic=true); this explicit
                // section is the belt-and-braces Merkle proof the wire
                // exposes via `transitive_project_refs`.
                var depKeys = new List<(string ProjectPath, byte[] Key)>();
                foreach (var depPath in TransitiveClosurePaths(projectCsproj, directDepsByPath))
                {
                    depKeys.Add((depPath, emitted[depPath].Key));
                }

                var (ws, pid) = workspaces[projectCsproj];
                var diags = loadDiagsByPath[projectCsproj];
                var outcome = EmitOneInWorkspace(
                    ws,
                    pid,
                    projectCsproj,
                    emitted,
                    depKeys,
                    cacheRoot,
                    diags);

                switch (outcome)
                {
                    case BuildMetadataOutcome.Built built:
                        emitted[projectCsproj] = (built.ContentHash, built.MetadataDllPath);
                        if (string.Equals(projectCsproj, topCsprojPath, StringComparison.Ordinal))
                        {
                            topBuilt = built;
                        }
                        else
                        {
                            transitiveRefs.Add(new TransitiveProjectRefDto(projectCsproj, built.MetadataDllPath));
                            transitiveCompilerDiagnostics.AddRange(built.CompilerDiagnostics);
                        }
                        break;

                    // Any non-Built outcome short-circuits the walk: a leaf
                    // that can't emit can't feed its dependents, and the
                    // user sees the root cause (the leaf's diagnostics)
                    // directly. Per D7: "the sidecar returns their
                    // diagnostics too" — we return the first failure
                    // verbatim, which carries the diagnostics that would
                    // otherwise be re-derived from a downstream failure.
                    case BuildMetadataOutcome.EmitFailed:
                    case BuildMetadataOutcome.LoadFailedOutcome:
                    case BuildMetadataOutcome.CacheUnwritableOutcome:
                    case BuildMetadataOutcome.MissingProjectTfmOutcome:
                        return outcome;

                    case BuildMetadataOutcome.CsprojNotFound:
                        // Unreachable: missing-on-disk csprojs are filtered
                        // at the start of BuildMetadata for the top and
                        // at the per-project pre-load loop above for
                        // leaves.
                        throw new InvalidOperationException(
                            $"Loaded project at {projectCsproj} reported as missing on disk during emit");
                }
            }

            if (topBuilt is null)
            {
                // Topo sort always includes the root, so this would mean
                // the topo helper has a bug rather than a user-input
                // failure.
                throw new InvalidOperationException(
                    "Topological walk did not visit the top project");
            }

            // Sort transitive entries by csproj path so the wire response is
            // deterministic across runs (the topo order itself is, but
            // defending against future re-orderings is cheap).
            transitiveRefs.Sort((a, b) =>
                string.CompareOrdinal(a.CsprojPath, b.CsprojPath));

            // Top's diagnostics first (their FilePath already disambiguates)
            // then the leaves'. Order isn't load-bearing for callers (they
            // render by FilePath/line) but topo order keeps the wire
            // response deterministic.
            var allCompilerDiagnostics = topBuilt.CompilerDiagnostics.Length == 0
                && transitiveCompilerDiagnostics.Count == 0
                ? Array.Empty<CompilerDiagnosticDto>()
                : topBuilt.CompilerDiagnostics.Concat(transitiveCompilerDiagnostics).ToArray();

            return new BuildMetadataOutcome.Built(
                topBuilt.MetadataDllPath,
                topBuilt.ContentHash,
                topBuilt.FromCache,
                allCompilerDiagnostics,
                topBuilt.WorkspaceDiagnostics,
                transitiveRefs.ToArray());
        }
        finally
        {
            // Dispose every per-project workspace we opened above. The top
            // workspace is owned by BuildMetadata and disposed there — we
            // skip it here so we don't double-dispose.
            foreach (var (path, (ws, _)) in workspaces)
            {
                if (!string.Equals(path, topCsprojPath, StringComparison.Ordinal))
                {
                    ws.Dispose();
                }
            }
        }
    }

    /// <summary>
    /// Force <c>Deterministic=true</c> on the project we're about to emit,
    /// substitute every <c>ProjectReference</c> with a
    /// <see cref="PortableExecutableReference"/> pointing at the
    /// already-emitted leaf DLL, and run
    /// <see cref="EmitOrCache(Project, string, string, IReadOnlyList{ValueTuple{string, byte[]}}, WorkspaceDiagnosticDto[])"/>
    /// against the rewritten compilation. The substitution is the link
    /// between per-project workspaces: without it, the emit would try to
    /// re-compile the leaf in this workspace under the wrong TFM.
    /// </summary>
    /// <remarks>
    /// Determinism is load-bearing for the cache key in two ways:
    /// (1) Roslyn's <c>GetDeterministicKey</c> output is only stable for
    /// stable inputs when <c>Deterministic=true</c> (otherwise it embeds a
    /// per-emit MVID); (2) the substituted PE ref's MVID is a content hash
    /// of the producer's inputs only when the producer used
    /// <c>Deterministic=true</c>. We force it on every project we emit, so
    /// the cascade is stable regardless of the underlying csproj contents.
    /// </remarks>
    private static BuildMetadataOutcome EmitOneInWorkspace(
        MSBuildWorkspace workspace,
        ProjectId pid,
        string csprojPath,
        IReadOnlyDictionary<string, (byte[] Key, string DllPath)> emitted,
        IReadOnlyList<(string ProjectPath, byte[] Key)> transitiveKeys,
        string cacheRoot,
        WorkspaceDiagnosticDto[] workspaceDiagnostics)
    {
        var solution = workspace.CurrentSolution;
        var project = solution.GetProject(pid)
            ?? throw new InvalidOperationException(
                $"Project {pid} missing from workspace solution for {csprojPath}");

        if (project.CompilationOptions is CSharpCompilationOptions opts && !opts.Deterministic)
        {
            solution = solution.WithProjectCompilationOptions(pid, opts.WithDeterministic(true));
            project = solution.GetProject(pid)
                ?? throw new InvalidOperationException(
                    $"Project {pid} disappeared after WithProjectCompilationOptions for {csprojPath}");
        }

        foreach (var pref in project.ProjectReferences.ToList())
        {
            var depPath = solution.GetProject(pref.ProjectId)?.FilePath is { } depFp
                ? Path.GetFullPath(depFp)
                : throw new InvalidOperationException(
                    $"ProjectReference on {project.Name} has no FilePath; cannot substitute");
            if (!emitted.TryGetValue(depPath, out var dep))
            {
                // Topo invariant says every direct dep has been emitted in a
                // prior iteration. If this fires, the toposort is broken or
                // the workspace solution has a different graph than what we
                // discovered through the top workspace — a sidecar bug.
                throw new InvalidOperationException(
                    $"Dependency {depPath} of {csprojPath} not emitted before substitution");
            }
            // Preserve the project ref's MetadataReferenceProperties (extern
            // aliases, embed-interop-types) on the substituted PE ref.
            // Dropping these would silently change the type-resolution surface
            // for the consumer — e.g. an `extern alias FooV1; extern alias
            // FooV2;` consumer that disambiguates two leaves by alias would
            // start seeing ambiguous references after PE substitution. The
            // image kind is always Assembly because that's what we just
            // emitted (modules can't be PE refs in a Roslyn solution anyway).
            var props = new MetadataReferenceProperties(
                kind: MetadataImageKind.Assembly,
                aliases: pref.Aliases,
                embedInteropTypes: pref.EmbedInteropTypes);
            solution = solution.RemoveProjectReference(pid, pref);
            solution = solution.AddMetadataReference(pid, MetadataReference.CreateFromFile(dep.DllPath, props));
        }

        var refreshedProject = solution.GetProject(pid)
            ?? throw new InvalidOperationException(
                $"Project {pid} disappeared after PE substitution for {csprojPath}");
        return EmitOrCache(refreshedProject, csprojPath, cacheRoot, transitiveKeys, workspaceDiagnostics);
    }

    /// <summary>
    /// Strict transitive closure (excluding <paramref name="rootCsproj"/>)
    /// of a project's <c>&lt;ProjectReference&gt;</c>s, by csproj path.
    /// Order of the returned list is unspecified — callers sort downstream.
    /// </summary>
    private static List<string> TransitiveClosurePaths(
        string rootCsproj,
        IReadOnlyDictionary<string, List<string>> directDepsByPath)
    {
        var visited = new HashSet<string>(StringComparer.Ordinal);
        var result = new List<string>();
        var stack = new Stack<string>();
        stack.Push(rootCsproj);
        while (stack.Count > 0)
        {
            var current = stack.Pop();
            if (!directDepsByPath.TryGetValue(current, out var deps)) continue;
            foreach (var dep in deps)
            {
                if (visited.Add(dep))
                {
                    result.Add(dep);
                    stack.Push(dep);
                }
            }
        }
        return result;
    }

    /// <summary>
    /// Cache-lookup-or-emit for one project in a (possibly transitively
    /// referenced) closure. Computes the content-addressed cache key,
    /// returns a cache hit if the keyed DLL is already on disk, otherwise
    /// drives the metadata-only emit and atomic publish. The caller is
    /// responsible for passing in <paramref name="transitiveProjectKeys"/>
    /// computed from the <c>&lt;ProjectReference&gt;</c> closure (empty list
    /// for a leaf).
    /// </summary>
    /// <remarks>
    /// Exposed as <c>internal</c> rather than <c>private</c> only so the
    /// xUnit hit-path test can drive it directly without standing up
    /// MSBuildWorkspace — the parallel <c>BuildMetadata</c> path is the
    /// production caller.
    /// </remarks>
    internal static BuildMetadataOutcome EmitOrCache(
        Project project,
        string csprojPath,
        string cacheRoot,
        IReadOnlyList<(string ProjectPath, byte[] Key)> transitiveProjectKeys,
        WorkspaceDiagnosticDto[] workspaceDiagnostics)
    {
        var compilation = project.GetCompilationAsync().GetAwaiter().GetResult();
        if (compilation is null)
        {
            // Should never happen for a C# project loaded successfully; treat
            // as a load fault rather than asserting because the wire protocol
            // already has a slot for it.
            return BuildMetadataOutcome.NewLoadFailed(workspaceDiagnostics.Concat(new[]
            {
                new WorkspaceDiagnosticDto(
                    Kind: nameof(WorkspaceDiagnosticKind.Failure),
                    Message: $"Roslyn returned no Compilation for project {project.FilePath}",
                    FilePath: project.FilePath),
            }).ToArray());
        }

        // D5: metadata-only with private+internal members preserved. The
        // latter is what makes the output safe for downstream IVT consumers —
        // a strict ref assembly would elide them. `tolerateErrors: true`
        // lets Roslyn still emit when errors are confined to method bodies,
        // which metadata-only emit doesn't analyse; the F# side only
        // consumes the public surface, so a body-only typo elsewhere in the
        // project must not strip the DLL we hand back.
        //
        // Constructed once here, then handed to both ComputeKey and Emit so
        // the cache key reflects the exact emit shape we use.
        var emitOptions = new EmitOptions(
            metadataOnly: true,
            includePrivateMembers: true,
            tolerateErrors: true);

        byte[] key;
        try
        {
            key = Cache.ComputeKey(
                project,
                compilation,
                emitOptions,
                csprojPath,
                transitiveProjectKeys);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // Reading source / reference content for the hash may itself hit
            // an IO fault. Treat that as a cache-side problem because the
            // user's project is otherwise loadable.
            return BuildMetadataOutcome.NewCacheUnwritable(cacheRoot, ex.Message);
        }

        var keyHex = Cache.ToHexLower(key);
        // Two-character prefix shard mirrors git's loose-object layout. Limits
        // the directory's fanout once a workspace accumulates many cached
        // entries, without introducing a heavyweight index.
        var prefix = keyHex[..2];
        var prefixDir = Path.Combine(cacheRoot, prefix);
        var finalPath = Path.Combine(prefixDir, $"{keyHex}.dll");

        if (File.Exists(finalPath))
        {
            // Cache hit. The file's content is a deterministic function of
            // its name, so this is the same DLL we would emit again — return
            // without re-driving Roslyn. We deliberately do not surface the
            // emit-time diagnostics from the prior call: they were attached
            // to that response and the caller has already seen them. A fresh
            // diagnostic pass would require re-driving the compiler, which
            // is what the cache exists to avoid.
            return BuildMetadataOutcome.NewBuilt(
                finalPath,
                key,
                fromCache: true,
                Array.Empty<CompilerDiagnosticDto>(),
                workspaceDiagnostics);
        }

        try
        {
            Directory.CreateDirectory(prefixDir);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return BuildMetadataOutcome.NewCacheUnwritable(prefixDir, ex.Message);
        }

        return Emit(compilation, emitOptions, workspaceDiagnostics, key, finalPath, prefixDir);
    }

    /// <summary>
    /// Post-order DFS over the path-keyed <c>&lt;ProjectReference&gt;</c>
    /// graph in <paramref name="directDepsByPath"/>, starting from
    /// <paramref name="rootCsproj"/> and returning csproj paths in
    /// leaves-first order. Throws <see cref="CycleDetectedException"/> on
    /// back edges — <c>&lt;ProjectReference&gt;</c> cycles are illegal in
    /// C# itself (csc rejects them), so this case is symptomatic of a
    /// malformed project graph rather than something we can build around.
    /// </summary>
    /// <remarks>
    /// Operates on csproj paths rather than Roslyn <see cref="ProjectId"/>s
    /// because the multi-TFM closure walk reads edges from per-project
    /// workspaces — each workspace has its own <see cref="ProjectId"/>
    /// allocations and the only identifier shared between them is the
    /// canonical csproj path.
    /// </remarks>
    private static List<string> TopoSortByPaths(
        string rootCsproj,
        IReadOnlyDictionary<string, List<string>> directDepsByPath)
    {
        var visited = new HashSet<string>(StringComparer.Ordinal);
        var onStack = new HashSet<string>(StringComparer.Ordinal);
        var order = new List<string>();
        Visit(rootCsproj);
        return order;

        void Visit(string path)
        {
            if (visited.Contains(path)) return;
            if (!onStack.Add(path))
            {
                throw new CycleDetectedException(path);
            }

            if (directDepsByPath.TryGetValue(path, out var deps))
            {
                // Order direct references by csproj path so the topo order
                // is byte-stable across runs — the cache key (which hashes
                // the transitive list) depends on this stability.
                foreach (var depPath in deps.OrderBy(p => p, StringComparer.Ordinal))
                {
                    Visit(depPath);
                }
            }

            onStack.Remove(path);
            visited.Add(path);
            order.Add(path);
        }
    }

    private static ProjectLoadOutcome LoadProject(MSBuildWorkspace workspace, string csprojPath)
    {
        Project project;
        try
        {
            project = workspace.OpenProjectAsync(csprojPath).GetAwaiter().GetResult();
        }
        catch (Exception ex)
        {
            // OpenProjectAsync throws for hard load failures (e.g., the csproj
            // XML is malformed before MSBuild even sees it). Treat the
            // exception message as a single diagnostic so the caller surfaces
            // it identically to any other load failure.
            return ProjectLoadOutcome.LoadFailed(new[]
            {
                new WorkspaceDiagnosticDto(
                    Kind: nameof(WorkspaceDiagnosticKind.Failure),
                    Message: ex.Message,
                    FilePath: csprojPath),
            });
        }

        var newDiags = workspace.Diagnostics
            .Select(d => new WorkspaceDiagnosticDto(
                Kind: d.Kind.ToString(),
                Message: d.Message,
                FilePath: csprojPath))
            .ToArray();

        var hasFailure = newDiags.Any(d =>
            string.Equals(d.Kind, nameof(WorkspaceDiagnosticKind.Failure), StringComparison.Ordinal));
        if (hasFailure)
        {
            return ProjectLoadOutcome.LoadFailed(newDiags);
        }

        return ProjectLoadOutcome.Loaded(
            project,
            new LoadedProjectInfo(
                AssemblyName: project.AssemblyName,
                Language: project.Language,
                SourceFileCount: project.Documents.Count(),
                MetadataReferenceCount: project.MetadataReferences.Count),
            newDiags);
    }

    private static BuildMetadataOutcome Emit(
        Compilation compilation,
        EmitOptions emitOptions,
        WorkspaceDiagnosticDto[] workspaceDiagnostics,
        byte[] key,
        string finalPath,
        string prefixDir)
    {
        // Unique-per-emit temp suffix: two sidecar processes emitting the
        // same csproj must not be able to truncate or rename each other's
        // in-progress write.
        var tempPath = $"{finalPath}.{Path.GetRandomFileName()}.tmp";

        EmitResult emitResult;
        try
        {
            using var stream = File.Create(tempPath);
            emitResult = compilation.Emit(stream, options: emitOptions);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            TryDeleteTemp(tempPath);
            return BuildMetadataOutcome.NewCacheUnwritable(prefixDir, ex.Message);
        }

        // Surface the union of compile + emit diagnostics. `EmitOptions` sets
        // `tolerateErrors: true` so a body-level CS0103 doesn't strip the DLL,
        // but that same flag also makes Roslyn skip body diagnostics inside
        // `EmitResult.Diagnostics`. The user still expects to see those, so
        // we pair the emit result with `compilation.GetDiagnostics()` — which
        // runs the full compile pass without the emit-side filter. Many
        // diagnostics appear in both sets (parse/declare); dedup by the
        // (id, severity, span, message) tuple so the wire response carries
        // each diagnostic at most once. Compilation diagnostics come first
        // so the wire order is "what's wrong with the source" then "what
        // the emit added on top of that," which is what the F# LSP renders.
        var diagnostics = MergeDiagnostics(compilation.GetDiagnostics(), emitResult.Diagnostics);

        if (!emitResult.Success)
        {
            // D8: surface diagnostics, no path. The content-addressed cache
            // means there is no "previously published DLL at this key" — a
            // failed emit at key K just doesn't write K.dll. Any K.dll
            // already on disk was emitted from inputs that hashed to K and
            // remains correct for those exact inputs.
            TryDeleteTemp(tempPath);
            return BuildMetadataOutcome.NewBuildFailed(diagnostics, workspaceDiagnostics);
        }

        try
        {
            // Atomic publish: rename is the visibility boundary. A concurrent
            // reader (the Rust assembly importer) must never see a half-written
            // DLL. `File.Move(overwrite: true)` is atomic on every modern
            // filesystem we care about (NTFS, APFS, ext4). Overwrite is safe
            // because two concurrent emits at the same key produce
            // byte-identical output (content-addressed) — whichever rename
            // wins, the file at finalPath is correct.
            File.Move(tempPath, finalPath, overwrite: true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            TryDeleteTemp(tempPath);
            return BuildMetadataOutcome.NewCacheUnwritable(prefixDir, ex.Message);
        }

        return BuildMetadataOutcome.NewBuilt(finalPath, key, fromCache: false, diagnostics, workspaceDiagnostics);
    }

    /// <summary>
    /// Union of two Roslyn diagnostic lists, deduplicated by an
    /// (id, severity, source span, message) tuple. The first list "wins" for
    /// ordering — entries in <paramref name="second"/> that are equal to any
    /// already-seen entry are skipped. The tuple is the same shape the F# LSP
    /// renders, so two diagnostics that compare equal here would render
    /// identically; collapsing them keeps the wire response from doubling up.
    /// </summary>
    private static CompilerDiagnosticDto[] MergeDiagnostics(
        IEnumerable<Diagnostic> first,
        IEnumerable<Diagnostic> second)
    {
        var seen = new HashSet<DiagDedupKey>();
        var result = new List<CompilerDiagnosticDto>();
        foreach (var d in first.Concat(second))
        {
            var key = new DiagDedupKey(
                d.Id,
                d.Severity,
                d.Location.GetLineSpan(),
                d.GetMessage());
            if (seen.Add(key))
            {
                result.Add(CompilerDiagnosticDto.FromRoslyn(d));
            }
        }
        return result.ToArray();
    }

    private readonly record struct DiagDedupKey(
        string Id,
        DiagnosticSeverity Severity,
        FileLinePositionSpan Span,
        string Message);

    /// <summary>
    /// Best-effort cleanup of a <c>.tmp</c> file we just tried to write.
    /// Leaving one of these around is strictly cosmetic — the deterministic
    /// finalPath is unaffected — so we swallow any failure rather than
    /// shadowing the original error.
    /// </summary>
    private static void TryDeleteTemp(string path)
    {
        try
        {
            if (File.Exists(path))
            {
                File.Delete(path);
            }
        }
        catch
        {
            // Ignored — cosmetic.
        }
    }
}

/// <summary>
/// Tagged outcome of <see cref="BuildService.BuildMetadata(string, string, string, string)"/>.
/// Encodes every wire-format kind the caller can return: a successful build, a
/// build that failed at emit, a load fault, a missing csproj, or a cache
/// directory we couldn't write to. See <c>docs/completed/csharp-sidecar-plan.md</c> D10.
/// </summary>
internal abstract record BuildMetadataOutcome
{
    public sealed record Built(
        string MetadataDllPath,
        byte[] ContentHash,
        bool FromCache,
        CompilerDiagnosticDto[] CompilerDiagnostics,
        WorkspaceDiagnosticDto[] WorkspaceDiagnostics,
        TransitiveProjectRefDto[] TransitiveProjectRefs) : BuildMetadataOutcome;

    public sealed record EmitFailed(
        CompilerDiagnosticDto[] CompilerDiagnostics,
        WorkspaceDiagnosticDto[] WorkspaceDiagnostics) : BuildMetadataOutcome;

    public sealed record LoadFailedOutcome(WorkspaceDiagnosticDto[] Diagnostics) : BuildMetadataOutcome;
    public sealed record CsprojNotFound(string Path) : BuildMetadataOutcome;
    public sealed record CacheUnwritableOutcome(string Path, string Detail) : BuildMetadataOutcome;
    /// <summary>
    /// The <c>projectTfms</c> map supplied with the request did not contain
    /// an entry for <paramref name="CsprojPath"/>. Phase 4 of
    /// <c>docs/completed/multi-tfm-resolution-plan.md</c> hard-errors here (D5) rather
    /// than silently falling back to the consumer TFM: a missing entry is
    /// a caller-side bug and we want it loud. <paramref name="CsprojPath"/>
    /// is the project the sidecar tried and failed to look up.
    /// </summary>
    public sealed record MissingProjectTfmOutcome(string CsprojPath) : BuildMetadataOutcome;

    public static BuildMetadataOutcome NewBuilt(
        string metadataDllPath,
        byte[] contentHash,
        bool fromCache,
        CompilerDiagnosticDto[] compilerDiagnostics,
        WorkspaceDiagnosticDto[] workspaceDiagnostics) =>
        new Built(
            metadataDllPath,
            contentHash,
            fromCache,
            compilerDiagnostics,
            workspaceDiagnostics,
            Array.Empty<TransitiveProjectRefDto>());

    public static BuildMetadataOutcome NewBuildFailed(
        CompilerDiagnosticDto[] compilerDiagnostics,
        WorkspaceDiagnosticDto[] workspaceDiagnostics) =>
        new EmitFailed(compilerDiagnostics, workspaceDiagnostics);

    public static BuildMetadataOutcome NewLoadFailed(WorkspaceDiagnosticDto[] diagnostics) =>
        new LoadFailedOutcome(diagnostics);

    public static BuildMetadataOutcome NewNotFound(string path) =>
        new CsprojNotFound(path);

    public static BuildMetadataOutcome NewCacheUnwritable(string path, string detail) =>
        new CacheUnwritableOutcome(path, detail);

    public static BuildMetadataOutcome NewMissingProjectTfm(string csprojPath) =>
        new MissingProjectTfmOutcome(csprojPath);
}

/// <summary>
/// Internal-only outcome of the <c>OpenProjectAsync</c> step. Successful loads
/// carry the Roslyn <see cref="Project"/> through to the emit path; failure
/// variants are flattened into <see cref="BuildMetadataOutcome"/> at the
/// boundary.
/// </summary>
internal abstract record ProjectLoadOutcome
{
    public sealed record LoadedOk(
        Project Project,
        LoadedProjectInfo Info,
        WorkspaceDiagnosticDto[] Diagnostics) : ProjectLoadOutcome;
    public sealed record Failed(WorkspaceDiagnosticDto[] Diagnostics) : ProjectLoadOutcome;
    public sealed record CsprojNotFound(string Path) : ProjectLoadOutcome;

    public static ProjectLoadOutcome Loaded(
        Project project,
        LoadedProjectInfo info,
        WorkspaceDiagnosticDto[] diagnostics) =>
        new LoadedOk(project, info, diagnostics);
    public static ProjectLoadOutcome LoadFailed(WorkspaceDiagnosticDto[] diagnostics) =>
        new Failed(diagnostics);
    public static ProjectLoadOutcome NotFound(string path) =>
        new CsprojNotFound(path);
}

/// <summary>
/// Coarse summary of a loaded project, retained from phase 2 because the
/// integration tests still verify the workspace did the load. Phase 4 keeps
/// the type on the <see cref="ProjectLoadOutcome.LoadedOk"/> path but the
/// on-the-wire response no longer carries it — the dll path is the proof now.
/// </summary>
internal sealed record LoadedProjectInfo(
    string? AssemblyName,
    string Language,
    int SourceFileCount,
    int MetadataReferenceCount);

/// <summary>
/// On-the-wire shape of a workspace diagnostic. Mirrors the relevant subset of
/// Roslyn's <see cref="WorkspaceDiagnostic"/> so the Rust side does not need
/// to model the enum.
/// </summary>
internal sealed record WorkspaceDiagnosticDto(
    string Kind,
    string Message,
    string? FilePath);

/// <summary>
/// On-the-wire shape of a Roslyn compiler diagnostic. Mirrors the shape
/// documented in <c>docs/completed/csharp-sidecar-plan.md</c> D8; the F# LSP can render
/// these through the same channel it uses for fsproj diagnostics.
/// </summary>
internal sealed record CompilerDiagnosticDto(
    string Id,
    string Severity,
    string Message,
    string? FilePath,
    DiagnosticRangeDto? Range)
{
    public static CompilerDiagnosticDto FromRoslyn(Diagnostic d)
    {
        var location = d.Location;
        DiagnosticRangeDto? range = null;
        string? filePath = null;
        if (location.IsInSource)
        {
            filePath = location.SourceTree?.FilePath;
            var span = location.GetLineSpan();
            var s = span.StartLinePosition;
            var e = span.EndLinePosition;
            range = new DiagnosticRangeDto(
                Start: new DiagnosticPositionDto(s.Line, s.Character),
                End: new DiagnosticPositionDto(e.Line, e.Character));
        }
        return new CompilerDiagnosticDto(
            Id: d.Id,
            Severity: d.Severity.ToString(),
            Message: d.GetMessage(),
            FilePath: filePath,
            Range: range);
    }
}

/// <summary>0-based line/character pair, matching the LSP convention.</summary>
internal sealed record DiagnosticPositionDto(int Line, int Character);

internal sealed record DiagnosticRangeDto(DiagnosticPositionDto Start, DiagnosticPositionDto End);

/// <summary>
/// Thrown by the topo walk when a <c>&lt;ProjectReference&gt;</c> back edge is
/// observed. Carries the name of the project that closed the cycle so the
/// caller can mention it in the surfaced workspace diagnostic.
/// </summary>
internal sealed class CycleDetectedException : Exception
{
    public CycleDetectedException(string projectName)
        : base($"project '{projectName}' is its own (transitive) <ProjectReference>")
    {
    }
}

namespace CSharpSidecar.Tests.TestHelpers;

using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Text;

/// <summary>
/// Builds a <see cref="Project"/> + <see cref="CSharpCompilation"/> pair in
/// memory without going through <c>MSBuildWorkspace</c>. The compilation
/// always has <see cref="CSharpCompilationOptions.Deterministic"/> set —
/// every consumer of <c>Cache.ComputeKey</c> in the sidecar relies on that,
/// and the tests have to mirror it to exercise the same code path.
/// </summary>
/// <remarks>
/// The reference set is intentionally minimal (just <c>System.Runtime</c>'s
/// nearest equivalent — <c>typeof(object).Assembly</c>). The cache-key
/// surface doesn't need a buildable compilation; it only needs Roslyn to
/// produce a stable deterministic key, which it will for any well-formed
/// <see cref="Compilation"/>.
/// </remarks>
internal static class AdhocCompilation
{
    internal sealed record Built(
        AdhocWorkspace Workspace,
        Project Project,
        CSharpCompilation Compilation);

    /// <summary>
    /// Construct a project with the supplied sources, additional documents,
    /// and analyzer-config documents. The csproj file at
    /// <paramref name="csprojPath"/> does not have to exist on disk; tests
    /// that hash csproj bytes should write whatever bytes they want at that
    /// path before calling <c>Cache.ComputeKey</c>.
    /// </summary>
    public static Built Build(
        string projectName,
        string csprojPath,
        IEnumerable<(string Name, string Source)>? sources = null,
        IEnumerable<(string Name, string Content)>? additionalDocs = null,
        IEnumerable<(string Name, string Content)>? analyzerConfigDocs = null)
    {
        var workspace = new AdhocWorkspace();
        var projectId = ProjectId.CreateNewId(debugName: projectName);

        var projectInfo = ProjectInfo.Create(
            id: projectId,
            version: VersionStamp.Default,
            name: projectName,
            assemblyName: projectName,
            language: LanguageNames.CSharp,
            filePath: csprojPath,
            compilationOptions: new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary)
                .WithDeterministic(true),
            parseOptions: new CSharpParseOptions(),
            metadataReferences: new[]
            {
                MetadataReference.CreateFromFile(typeof(object).Assembly.Location),
            });

        var solution = workspace.AddProject(projectInfo).Solution;

        foreach (var (name, source) in sources ?? Array.Empty<(string, string)>())
        {
            var docId = DocumentId.CreateNewId(projectId, name);
            solution = solution.AddDocument(docId, name, SourceText.From(source));
        }

        foreach (var (name, content) in additionalDocs ?? Array.Empty<(string, string)>())
        {
            var docId = DocumentId.CreateNewId(projectId, name);
            solution = solution.AddAdditionalDocument(docId, name, SourceText.From(content), filePath: name);
        }

        foreach (var (name, content) in analyzerConfigDocs ?? Array.Empty<(string, string)>())
        {
            var docId = DocumentId.CreateNewId(projectId, name);
            solution = solution.AddAnalyzerConfigDocument(docId, name, SourceText.From(content), filePath: name);
        }

        if (!workspace.TryApplyChanges(solution))
        {
            throw new InvalidOperationException(
                "AdhocWorkspace rejected the constructed solution");
        }

        var project = workspace.CurrentSolution.GetProject(projectId)
            ?? throw new InvalidOperationException(
                $"Project {projectId} disappeared after TryApplyChanges");
        var compilation = (CSharpCompilation)project.GetCompilationAsync().GetAwaiter().GetResult()!;
        return new Built(workspace, project, compilation);
    }
}

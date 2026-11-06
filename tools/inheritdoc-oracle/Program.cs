// Differential-test oracle for `<inheritdoc>` expansion (test-only; never a
// runtime dependency of the LSP — see crates/lsp/src/xml_doc/inherit.rs).
//
// It asks Roslyn's *IDE* expansion — what Visual Studio and the C# language
// server show on hover — for the documentation of metadata symbols:
// `Microsoft.CodeAnalysis.Shared.Extensions.ISymbolExtensions
// .GetDocumentationComment(symbol, compilation, culture, expandIncludes: true,
// expandInheritdoc: true, ct)`, internal to Microsoft.CodeAnalysis.Workspaces
// and so reached by reflection. Hover starts from the symbol's original
// definition and passes exactly these flags (`ISymbolExtensions_2
// .GetAppropriateDocumentationComment`), so this is the IDE's answer.
//
// Protocol: JSONL request/response over stdin/stdout, one response line per
// request line, in order (the resident-batch-child pattern of tools/fcs-dump).
//
//   {"op":"expand","references":[dll..],"assembly":dll,"ids":[docId..]}
//     Builds (or reuses — the last reference set is cached) a C# compilation
//     over `references`, each `MetadataReference.CreateFromFile(dll,
//     documentation: XmlDocumentationProvider.CreateFromFile(<dll>.xml))`,
//     with default options (so `MetadataImportOptions.Public`, as for any
//     project reference). For each id, the symbols
//     `DocumentationCommentId.GetSymbolsForDeclarationId` finds *in the
//     assembly read from `assembly`* (which must be one of `references`):
//     -> {"results":[
//          {"status":"ok","xml":<expanded FullXmlFragment>,
//           "rules":[{"rule":r,"candidate":docId|null}..]}
//        | {"status":"no-symbol"}
//        | {"status":"ambiguous","count":n} ..]}
//     `rules` describes each `<inheritdoc>` element of the symbol's *own*
//     entry, in document order, for the census only: which of Roslyn's
//     candidate rules applies (`cref`, `explicit-impl`, `override`, `ctor`,
//     `interface-impl`, `base-class`, `base-interface`, `none`) and the
//     candidate's documentation-comment ID. The `xml` is the authority.
//
//   {"op":"xpath","xml":s,"path":p}
//     The selection `RewriteInheritdocElement` makes: `XPathEvaluate` of `p`
//     over `xml` parsed with whitespace preserved.
//     -> {"ok":true,"selection":"<selection>…the selected nodes…</selection>"}
//      | {"ok":false,"why":..}   (where Roslyn's `TrySelectNodes` yields null)
//
//   {"op":"compile","source":s,"assemblyName":n,"outDir":d,
//    "references":[dll | {"path":dll,"alias":a} ..], "publicKeyFrom":dll?}
//     Compile one C# file (preview language, nullable on) to `d/n.dll` and its
//     documentation file `d/n.xml` — the compiler's own `.xml`, which keeps
//     `<inheritdoc>` verbatim and turns crefs into IDs — for purpose-built
//     fixtures. With `publicKeyFrom`, the assembly is public-signed with
//     that DLL's public key, so references to it carry the key's token.
//     -> {"ok":true} | {"ok":false,"diagnostics":[..errors..]}
//
//   {"op":"swap-method-names","assembly":dll,"a":m1,"b":m2}
//     Exchange, in place, the `Name` columns of the `MethodDef` rows named m1
//     and m2: metadata no compiler writes (an accessor named like another
//     property's), for fixtures `compile` cannot make.
//     -> {"ok":true}
//
//   {"op":"patch-assembly-ref","assembly":dll,"name":n,
//    "version":"a.b.c.d"?,"token":hex16?,"dropToken":bool?}
//     Rewrite, in place, the `AssemblyRef` row of `dll` named n: its version
//     columns, the bytes of its (8-byte, unshared) public-key token, or its
//     token cleared to the empty blob — references whose identity no loaded
//     assembly carries, for fixtures `compile` cannot make.
//     -> {"ok":true}
//
//   {"op":"emit",..}
//     Write an assembly from a JSON description (see Emit.cs).
//
// Any per-request exception is reported as {"error":..} on that line; the
// process itself never dies mid-batch.

using System.Collections;
using System.Globalization;
using System.Reflection;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Xml.Linq;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;

namespace InheritdocOracle;

internal static class Program
{
    private static readonly MethodInfo s_getDocumentationComment = FindExpansion();
    private static readonly Dictionary<string, MetadataReference> s_references = new();
    private static string? s_compilationKey;
    private static CSharpCompilation? s_compilation;

    private static MethodInfo FindExpansion()
    {
        var workspaces = typeof(Microsoft.CodeAnalysis.Workspace).Assembly;
        var extensions = workspaces.GetType("Microsoft.CodeAnalysis.Shared.Extensions.ISymbolExtensions", throwOnError: true)!;
        return extensions
            .GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static)
            .Single(m => m.Name == "GetDocumentationComment"
                && m.GetParameters().Select(p => p.ParameterType).SequenceEqual(new[]
                {
                    typeof(ISymbol), typeof(Compilation), typeof(CultureInfo), typeof(bool), typeof(bool), typeof(CancellationToken),
                }));
    }

    private static string Expand(ISymbol symbol, Compilation compilation)
    {
        var comment = s_getDocumentationComment.Invoke(null, new object?[]
        {
            symbol, compilation, null, true, true, CancellationToken.None,
        })!;
        return (string)comment.GetType().GetProperty("FullXmlFragment")!.GetValue(comment)!;
    }

    private static MetadataReference Reference(string dll)
    {
        if (!s_references.TryGetValue(dll, out var reference))
        {
            var xml = Path.ChangeExtension(dll, ".xml");
            reference = MetadataReference.CreateFromFile(dll, documentation: XmlDocumentationProvider.CreateFromFile(xml));
            s_references[dll] = reference;
        }
        return reference;
    }

    /// <summary>
    /// The compilation over <paramref name="references"/>, reused while the set is unchanged. Only the current
    /// set's metadata references stay cached: a test binary compiles thousands of fixtures, and each brings
    /// new DLLs, while the reference pack they share is carried from one set to the next.
    /// </summary>
    private static CSharpCompilation CompilationOf(IReadOnlyList<string> references)
    {
        var key = string.Join("\n", references);
        if (s_compilation is null || s_compilationKey != key)
        {
            var current = references.Select(Reference).ToList();
            foreach (var stale in s_references.Keys.Except(references).ToList())
            {
                s_references.Remove(stale);
            }
            s_compilation = CSharpCompilation.Create("InheritdocOracleConsumer", references: current);
            s_compilationKey = key;
        }
        return s_compilation;
    }

    /// <summary>
    /// Forget what was read from <paramref name="dll"/> (and its documentation file), which a request has just
    /// written: the next compilation over it reads it afresh.
    /// </summary>
    internal static void Rewritten(string dll)
    {
        s_references.Remove(dll);
        s_compilation = null;
        s_compilationKey = null;
    }

    private static string? DocId(ISymbol? symbol) => symbol?.GetDocumentationCommentId();

    private static (string Rule, ISymbol? Candidate) Rule(ISymbol symbol, XElement inheritdoc)
    {
        if (inheritdoc.Attribute("cref") is { } cref)
        {
            return ("cref", null);
        }
        var explicitImpls = symbol switch
        {
            IMethodSymbol m => m.ExplicitInterfaceImplementations.Cast<ISymbol>().ToList(),
            IPropertySymbol p => p.ExplicitInterfaceImplementations.Cast<ISymbol>().ToList(),
            IEventSymbol e => e.ExplicitInterfaceImplementations.Cast<ISymbol>().ToList(),
            _ => new List<ISymbol>(),
        };
        if (explicitImpls.Count > 0)
        {
            return ("explicit-impl", explicitImpls[0]);
        }
        if (symbol.IsOverride)
        {
            ISymbol? overridden = symbol switch
            {
                IMethodSymbol m => m.OverriddenMethod,
                IPropertySymbol p => p.OverriddenProperty,
                IEventSymbol e => e.OverriddenEvent,
                _ => null,
            };
            return ("override", overridden);
        }
        if (symbol is IMethodSymbol { MethodKind: MethodKind.Constructor or MethodKind.StaticConstructor })
        {
            return ("ctor", null);
        }
        if (symbol is INamedTypeSymbol type)
        {
            return type.TypeKind switch
            {
                TypeKind.Class => ("base-class", type.BaseType),
                TypeKind.Interface => ("base-interface", type.Interfaces.FirstOrDefault()),
                _ => ("none", null),
            };
        }
        if (symbol.Kind is SymbolKind.Method or SymbolKind.Property or SymbolKind.Event && symbol.ContainingType is { } containing)
        {
            foreach (var iface in containing.AllInterfaces)
            {
                foreach (var member in iface.GetMembers())
                {
                    if (SymbolEqualityComparer.Default.Equals(containing.FindImplementationForInterfaceMember(member), symbol))
                    {
                        return ("interface-impl", member);
                    }
                }
            }
            return ("interface-impl", null);
        }
        return ("none", null);
    }

    private static JsonObject ExpandOne(CSharpCompilation compilation, IAssemblySymbol assembly, string id)
    {
        var symbols = DocumentationCommentId.GetSymbolsForDeclarationId(id, compilation)
            .Where(s => SymbolEqualityComparer.Default.Equals(s.ContainingAssembly, assembly))
            .ToList();
        if (symbols.Count == 0)
        {
            return new JsonObject { ["status"] = "no-symbol" };
        }
        if (symbols.Count > 1)
        {
            return new JsonObject { ["status"] = "ambiguous", ["count"] = symbols.Count };
        }
        var symbol = symbols[0];
        var rules = new JsonArray();
        var own = symbol.GetDocumentationCommentXml(expandIncludes: false);
        if (!string.IsNullOrEmpty(own))
        {
            try
            {
                // Roslyn's `ElementNameIs`: no namespace, local name compared
                // ordinally ignoring case.
                var inheritdocs = XElement.Parse(own, LoadOptions.PreserveWhitespace)
                    .Descendants()
                    .Where(e => e.Name.NamespaceName.Length == 0
                        && string.Equals(e.Name.LocalName, "inheritdoc", StringComparison.OrdinalIgnoreCase));
                foreach (var inheritdoc in inheritdocs)
                {
                    var (rule, candidate) = Rule(symbol, inheritdoc);
                    rules.Add(new JsonObject { ["rule"] = rule, ["candidate"] = DocId(candidate) });
                }
            }
            catch (System.Xml.XmlException)
            {
            }
        }
        return new JsonObject
        {
            ["status"] = "ok",
            ["xml"] = Expand(symbol, compilation),
            ["rules"] = rules,
        };
    }

    private static JsonObject Handle(JsonObject request)
    {
        var op = (string?)request["op"];
        switch (op)
        {
            case "expand":
            {
                var references = request["references"]!.AsArray().Select(r => (string)r!).ToList();
                var assemblyPath = (string)request["assembly"]!;
                var compilation = CompilationOf(references);
                if (compilation.GetAssemblyOrModuleSymbol(Reference(assemblyPath)) is not IAssemblySymbol assembly)
                {
                    throw new InvalidOperationException($"{assemblyPath} is not one of the references");
                }
                var results = new JsonArray();
                foreach (var id in request["ids"]!.AsArray())
                {
                    results.Add(ExpandOne(compilation, assembly, (string)id!));
                }
                return new JsonObject { ["results"] = results };
            }
            case "compile":
            {
                // A reference is a path, or {"path":p,"alias":a} for one the
                // source reaches through `extern alias a`.
                var references = request["references"]!.AsArray().Select(r =>
                {
                    if (r is JsonObject o)
                    {
                        var reference = MetadataReference.CreateFromFile((string)o["path"]!);
                        return o["alias"] is { } alias
                            ? reference.WithAliases(new[] { (string)alias! })
                            : reference;
                    }
                    return MetadataReference.CreateFromFile((string)r!);
                }).ToList();
                var name = (string)request["assemblyName"]!;
                var outDir = (string)request["outDir"]!;
                var parse = new CSharpParseOptions(LanguageVersion.Preview, DocumentationMode.Diagnose);
                var tree = CSharpSyntaxTree.ParseText((string)request["source"]!, parse);
                // Unsafe code allowed, for fixtures whose members the projector
                // refuses (function pointers).
                var options = new CSharpCompilationOptions(
                    OutputKind.DynamicallyLinkedLibrary,
                    nullableContextOptions: NullableContextOptions.Enable,
                    allowUnsafe: true);
                // Strong-named by public signing with another assembly's key,
                // so a reference to this one carries that key's token.
                if (request["publicKeyFrom"] is { } keySource)
                {
                    using var keyPe = new System.Reflection.PortableExecutable.PEReader(File.OpenRead((string)keySource!));
                    var keyMd = System.Reflection.Metadata.PEReaderExtensions.GetMetadataReader(keyPe);
                    var key = keyMd.GetBlobContent(keyMd.GetAssemblyDefinition().PublicKey);
                    options = options.WithCryptoPublicKey(key).WithPublicSign(true);
                }
                var compilation = CSharpCompilation.Create(
                    name,
                    new[] { tree },
                    references,
                    options);
                Directory.CreateDirectory(outDir);
                using var dll = new MemoryStream();
                using var xml = new MemoryStream();
                var emit = compilation.Emit(dll, xmlDocumentationStream: xml);
                var errors = emit.Diagnostics
                    .Where(d => d.Severity == DiagnosticSeverity.Error)
                    .Select(d => (JsonNode)d.ToString())
                    .ToArray();
                if (!emit.Success)
                {
                    return new JsonObject { ["ok"] = false, ["diagnostics"] = new JsonArray(errors) };
                }
                File.WriteAllBytes(Path.Combine(outDir, name + ".dll"), dll.ToArray());
                File.WriteAllBytes(Path.Combine(outDir, name + ".xml"), xml.ToArray());
                Rewritten(Path.Combine(outDir, name + ".dll"));
                return new JsonObject { ["ok"] = true };
            }
            case "emit":
                return Emit.Run(request);
            case "swap-method-names":
            {
                // Exchange the `Name` columns of the two `MethodDef` rows named
                // `a` and `b`, in place: legal metadata no compiler writes (an
                // accessor named like another property's). The heap is left
                // alone, since a compiler's suffix-merged heap shares a
                // property's name with its accessor's.
                var path = (string)request["assembly"]!;
                var a = (string)request["a"]!;
                var b = (string)request["b"]!;
                var bytes = File.ReadAllBytes(path);
                int rowA, rowB, rowSize, tableStart, nameSize;
                using (var pe = new System.Reflection.PortableExecutable.PEReader(new MemoryStream(bytes)))
                {
                    var md = System.Reflection.Metadata.PEReaderExtensions.GetMetadataReader(pe);
                    var rows = md.MethodDefinitions
                        .Select(h => (Row: System.Reflection.Metadata.Ecma335.MetadataTokens.GetRowNumber(h), Name: md.GetString(md.GetMethodDefinition(h).Name)))
                        .ToList();
                    rowA = rows.Single(r => r.Name == a).Row;
                    rowB = rows.Single(r => r.Name == b).Row;
                    var table = System.Reflection.Metadata.Ecma335.TableIndex.MethodDef;
                    rowSize = System.Reflection.Metadata.Ecma335.MetadataReaderExtensions.GetTableRowSize(md, table);
                    tableStart = pe.PEHeaders.MetadataStartOffset
                        + System.Reflection.Metadata.Ecma335.MetadataReaderExtensions.GetTableMetadataOffset(md, table);
                    nameSize = System.Reflection.Metadata.Ecma335.MetadataReaderExtensions.GetHeapSize(md, System.Reflection.Metadata.Ecma335.HeapIndex.String) < 0x10000 ? 2 : 4;
                }
                // MethodDef columns: RVA (4), ImplFlags (2), Flags (2), Name.
                var offA = tableStart + (rowA - 1) * rowSize + 8;
                var offB = tableStart + (rowB - 1) * rowSize + 8;
                for (var i = 0; i < nameSize; i++)
                {
                    (bytes[offA + i], bytes[offB + i]) = (bytes[offB + i], bytes[offA + i]);
                }
                File.WriteAllBytes(path, bytes);
                Rewritten(path);
                return new JsonObject { ["ok"] = true };
            }
            case "patch-assembly-ref":
            {
                var path = (string)request["assembly"]!;
                var target = (string)request["name"]!;
                var bytes = File.ReadAllBytes(path);
                int rowOffset, blobIndexSize, tokenOffset, tokenLength;
                using (var pe = new System.Reflection.PortableExecutable.PEReader(new MemoryStream(bytes)))
                {
                    var md = System.Reflection.Metadata.PEReaderExtensions.GetMetadataReader(pe);
                    var refs = md.AssemblyReferences
                        .Select(h => (Handle: h, Ref: md.GetAssemblyReference(h)))
                        .ToList();
                    var (handle, row) = refs.Single(r => md.GetString(r.Ref.Name) == target);
                    var table = System.Reflection.Metadata.Ecma335.TableIndex.AssemblyRef;
                    var rowSize = System.Reflection.Metadata.Ecma335.MetadataReaderExtensions.GetTableRowSize(md, table);
                    var tableStart = pe.PEHeaders.MetadataStartOffset
                        + System.Reflection.Metadata.Ecma335.MetadataReaderExtensions.GetTableMetadataOffset(md, table);
                    rowOffset = tableStart + (System.Reflection.Metadata.Ecma335.MetadataTokens.GetRowNumber(handle) - 1) * rowSize;
                    blobIndexSize = System.Reflection.Metadata.Ecma335.MetadataReaderExtensions.GetHeapSize(md, System.Reflection.Metadata.Ecma335.HeapIndex.Blob) < 0x10000 ? 2 : 4;
                    var tokenHeapOffset = System.Reflection.Metadata.Ecma335.MetadataTokens.GetHeapOffset(row.PublicKeyOrToken);
                    tokenLength = md.GetBlobBytes(row.PublicKeyOrToken).Length;
                    // An 8-byte blob has a one-byte length prefix.
                    tokenOffset = pe.PEHeaders.MetadataStartOffset
                        + System.Reflection.Metadata.Ecma335.MetadataReaderExtensions.GetHeapMetadataOffset(md, System.Reflection.Metadata.Ecma335.HeapIndex.Blob)
                        + tokenHeapOffset + 1;
                    var shared = refs.Any(r => r.Handle != handle && r.Ref.PublicKeyOrToken == row.PublicKeyOrToken)
                        || (md.IsAssembly && md.GetAssemblyDefinition().PublicKey == row.PublicKeyOrToken);
                    if (request["token"] is { } && (tokenLength != 8 || shared))
                    {
                        throw new InvalidOperationException($"{target}'s token is not an unshared 8-byte blob");
                    }
                }
                // AssemblyRef columns: Major, Minor, Build, Revision (2 each),
                // Flags (4), PublicKeyOrToken (a blob index), Name, Culture,
                // HashValue.
                if (request["version"] is { } version)
                {
                    var parts = ((string)version!).Split('.').Select(ushort.Parse).ToArray();
                    for (var i = 0; i < 4; i++)
                    {
                        BitConverter.TryWriteBytes(bytes.AsSpan(rowOffset + 2 * i, 2), parts[i]);
                    }
                }
                if (request["token"] is { } token)
                {
                    Convert.FromHexString((string)token!).CopyTo(bytes, tokenOffset);
                }
                if (request["dropToken"] is { } drop && (bool)drop)
                {
                    bytes.AsSpan(rowOffset + 12, blobIndexSize).Clear();
                }
                File.WriteAllBytes(path, bytes);
                Rewritten(path);
                return new JsonObject { ["ok"] = true };
            }
            case "xpath":
            {
                // Exactly the selection `RewriteInheritdocElement` makes: the
                // document parsed with whitespace preserved, then
                // `TrySelectNodes`, whose `null` (an XPath error, or a
                // document-node result) Roslyn turns into "leave the
                // `<inheritdoc>` in place".
                var document = XDocument.Parse((string)request["xml"]!, LoadOptions.PreserveWhitespace);
                object result;
                try
                {
                    result = System.Xml.XPath.Extensions.XPathEvaluate(document, (string)request["path"]!);
                }
                catch (System.Xml.XPath.XPathException)
                {
                    return new JsonObject { ["ok"] = false, ["why"] = "xpath" };
                }
                if (result is not IEnumerable nodes)
                {
                    return new JsonObject { ["ok"] = false, ["why"] = "not-a-node-set" };
                }
                List<XNode> selected;
                try
                {
                    selected = nodes.Cast<XNode>().ToList();
                }
                catch (InvalidOperationException)
                {
                    return new JsonObject { ["ok"] = false, ["why"] = "document-node" };
                }
                catch (InvalidCastException)
                {
                    return new JsonObject { ["ok"] = false, ["why"] = "not-nodes" };
                }
                // `.` or `/` selects the document node, which no element can
                // hold: Roslyn's expansion fails on it and keeps the element.
                if (selected.Any(n => n is XDocument))
                {
                    return new JsonObject { ["ok"] = false, ["why"] = "document-node" };
                }
                var wrapper = new XElement("selection", selected);
                return new JsonObject { ["ok"] = true, ["selection"] = wrapper.ToString(SaveOptions.DisableFormatting) };
            }
            default:
                throw new InvalidOperationException($"unknown op {op}");
        }
    }

    private static int Main()
    {
        var stdin = Console.In;
        var stdout = new StreamWriter(Console.OpenStandardOutput()) { AutoFlush = false };
        string? line;
        while ((line = stdin.ReadLine()) is not null)
        {
            if (line.Length == 0)
            {
                continue;
            }
            JsonObject response;
            try
            {
                response = Handle(JsonNode.Parse(line)!.AsObject());
            }
            catch (Exception e)
            {
                response = new JsonObject { ["error"] = e.ToString() };
            }
            stdout.WriteLine(response.ToJsonString());
            stdout.Flush();
        }
        return 0;
    }
}

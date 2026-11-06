// The `emit` op: write an assembly straight from a JSON description, with
// System.Reflection.Metadata's MetadataBuilder — for metadata shapes no C#
// compiler writes (accessors named apart from their properties, assembly
// references of any version, culture or case, forwarders, custom modifiers,
// `MethodImpl` rows, arbitrary vtable flags, a hand-made core library, an
// event's `fire` accessor, a method type parameter its method lacks).
//
// Request:
//   {"op":"emit","path":p,
//    "assembly":{"name":n,"version":"a.b.c.d","culture":c},
//    "refs":[{"name":n,"version":v,"culture":c,"publicKeyToken":"hex"|null}..],
//    "types":[Type..], "forwarders":[{"ns":s,"name":n,"ref":i}..]}
//   Type   = {"ns":s,"name":n,"kind":"class"|"interface"|"struct",
//             "abstract":b,"sealed":b,"base":TypeRef|null,"interfaces":[TypeRef..],
//             "methods":[Method..],"properties":[{"name":n,"type":Sig,"getter":i}..],
//             "events":[{"name":n,"type":TypeRef,"add":i,"remove":i,"fire":i|null}..],
//             "generic":[name..]}                  (type parameters; the name carries its `n)
//           (`getter`, `add`, `remove`, `fire`: indices into the type's methods)
//   Method = {"name":n,"access":"public"|"private","static":b,"virtual":b,
//             "newslot":b,"abstract":b,"final":b,"specialName":b,
//             "ret":Sig|null (void),"params":[Sig..],
//             "impls":[{"parent":TypeRef,"name":n,"ret":Sig|null,"params":[Sig..]}..]}
//   Sig     = {"type":TypeRef,"mods":[{"optional":b,"type":TypeRef}..]}
//   TypeRef = {"prim":"I4"|"String"|"Object"|"Bool"}
//           | {"scope":-1|i,"ns":s,"name":n}   (-1: a type this module defines)
//           | {"szarray":TypeRef}               (the vector `T[]`)
//           | {"array":TypeRef}                 (rank 1, no bounds: `T[*]`)
//           | {"var":i} | {"mvar":i}            (`!i`, `!!i`, whatever the owner declares)
//           | {"inst":TypeRef,"args":[TypeRef..]} (a generic instantiation; as a base, a TypeSpec)
// Every non-abstract method gets the body `ret`: the bodies are never run.
// -> {"ok":true}

using System.Collections.Immutable;
using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;
using System.Text.Json.Nodes;

namespace InheritdocOracle;

internal static class Emit
{
    public static JsonObject Run(JsonObject request)
    {
        var mb = new MetadataBuilder();
        var il = new BlobBuilder();
        var bodies = new MethodBodyStreamEncoder(il);
        var asm = request["assembly"]!.AsObject();
        var name = (string)asm["name"]!;
        mb.AddModule(0, mb.GetOrAddString(name + ".dll"), mb.GetOrAddGuid(Guid.NewGuid()), default, default);
        mb.AddAssembly(
            mb.GetOrAddString(name),
            Version.Parse((string)asm["version"]!),
            Culture(mb, (string?)asm["culture"]),
            default,
            0,
            AssemblyHashAlgorithm.Sha1);

        var refs = new List<AssemblyReferenceHandle>();
        foreach (var r in request["refs"]!.AsArray())
        {
            var o = r!.AsObject();
            var token = (string?)o["publicKeyToken"];
            refs.Add(mb.AddAssemblyReference(
                mb.GetOrAddString((string)o["name"]!),
                Version.Parse((string)o["version"]!),
                Culture(mb, (string?)o["culture"]),
                token is null ? default : mb.GetOrAddBlob(Convert.FromHexString(token)),
                0,
                default));
        }

        var types = request["types"]!.AsArray().Select(t => t!.AsObject()).ToList();
        // `<Module>` is TypeDef row 1; the described types follow in order.
        var defs = new Dictionary<(string, string), TypeDefinitionHandle>();
        for (var i = 0; i < types.Count; i++)
        {
            defs[((string)types[i]["ns"]!, (string)types[i]["name"]!)] = MetadataTokens.TypeDefinitionHandle(i + 2);
        }
        var typeRefs = new Dictionary<(int, string, string), TypeReferenceHandle>();
        EntityHandle Ref(JsonNode node)
        {
            var o = node.AsObject();
            if (o["inst"] is JsonNode generic)
            {
                var blob = new BlobBuilder();
                var args = o["args"]!.AsArray();
                var arguments = new BlobEncoder(blob).TypeSpecificationSignature()
                    .GenericInstantiation(Ref(generic), args.Count, isValueType: false);
                foreach (var a in args)
                {
                    EncodeType(arguments.AddArgument(), a!);
                }
                return mb.AddTypeSpecification(mb.GetOrAddBlob(blob));
            }
            var scope = (int)o["scope"]!;
            var ns = (string)o["ns"]!;
            var n = (string)o["name"]!;
            if (scope < 0)
            {
                return defs[(ns, n)];
            }
            if (!typeRefs.TryGetValue((scope, ns, n), out var h))
            {
                h = mb.AddTypeReference(refs[scope], mb.GetOrAddString(ns), mb.GetOrAddString(n));
                typeRefs[(scope, ns, n)] = h;
            }
            return h;
        }
        void EncodeType(SignatureTypeEncoder enc, JsonNode t)
        {
            // `{"szarray": element}` is the vector `T[]`; `{"array": element}`
            // the rank-1 array with no bounds, `T[*]`.
            if (t["szarray"] is JsonNode vectorOf)
            {
                EncodeType(enc.SZArray(), vectorOf);
                return;
            }
            if (t["array"] is JsonNode arrayOf)
            {
                enc.Array(
                    e => EncodeType(e, arrayOf),
                    shape => shape.Shape(1, ImmutableArray<int>.Empty, ImmutableArray<int>.Empty));
                return;
            }
            if (t["inst"] is JsonNode generic)
            {
                var args = t["args"]!.AsArray();
                var arguments = enc.GenericInstantiation(Ref(generic), args.Count, isValueType: false);
                foreach (var a in args)
                {
                    EncodeType(arguments.AddArgument(), a!);
                }
                return;
            }
            if (t["var"] is JsonNode var)
            {
                enc.GenericTypeParameter((int)var);
                return;
            }
            if (t["mvar"] is JsonNode mvar)
            {
                enc.GenericMethodTypeParameter((int)mvar);
                return;
            }
            switch ((string?)t["prim"])
            {
                case "I4": enc.Int32(); return;
                case "String": enc.String(); return;
                case "Object": enc.Object(); return;
                case "Bool": enc.Boolean(); return;
                case null: enc.Type(Ref(t), isValueType: false); return;
                default: throw new InvalidOperationException($"unknown primitive {t["prim"]}");
            }
        }
        void Mods(CustomModifiersEncoder enc, JsonNode sig)
        {
            foreach (var m in sig["mods"]?.AsArray() ?? new JsonArray())
            {
                enc = enc.AddModifier(Ref(m!["type"]!), (bool)m["optional"]!);
            }
        }
        BlobHandle MethodSig(bool isStatic, JsonNode? ret, JsonArray ps)
        {
            var blob = new BlobBuilder();
            new BlobEncoder(blob).MethodSignature(isInstanceMethod: !isStatic).Parameters(
                ps.Count,
                r =>
                {
                    if (ret is null)
                    {
                        r.Void();
                    }
                    else
                    {
                        Mods(r.CustomModifiers(), ret);
                        EncodeType(r.Type(), ret["type"]!);
                    }
                },
                p =>
                {
                    foreach (var s in ps)
                    {
                        var pe = p.AddParameter();
                        Mods(pe.CustomModifiers(), s!);
                        EncodeType(pe.Type(), s!["type"]!);
                    }
                });
            return mb.GetOrAddBlob(blob);
        }

        mb.AddTypeDefinition(default, default, mb.GetOrAddString("<Module>"), default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        var methodRow = 1;
        var paramRow = 1;
        var propertyRow = 1;
        var eventRow = 1;
        var impls = new List<(TypeDefinitionHandle, MethodDefinitionHandle, JsonObject)>();
        // Each row as (association, its HasSemantics coded index, kind, method),
        // added in coded-index order, as the table is sorted.
        var semantics = new List<(EntityHandle, int, MethodSemanticsAttributes, MethodDefinitionHandle)>();
        var eventMaps = new List<(TypeDefinitionHandle, EventDefinitionHandle)>();
        // Added after the types, in type order, as the table is sorted by owner.
        var genericParams = new List<(TypeDefinitionHandle, string, int)>();
        var interfaces = new List<(TypeDefinitionHandle, EntityHandle)>();
        var propertyMaps = new List<(TypeDefinitionHandle, PropertyDefinitionHandle)>();
        // Method and parameter rows are laid out first, type by type, so each
        // TypeDef's method list points at its first method.
        var firstMethod = new List<MethodDefinitionHandle>();
        foreach (var t in types)
        {
            firstMethod.Add(MetadataTokens.MethodDefinitionHandle(methodRow));
            methodRow += t["methods"]!.AsArray().Count;
        }
        methodRow = 1;
        for (var i = 0; i < types.Count; i++)
        {
            var t = types[i];
            var handle = MetadataTokens.TypeDefinitionHandle(i + 2);
            var kind = (string)t["kind"]!;
            var attrs = TypeAttributes.Public;
            if (kind == "interface")
            {
                attrs |= TypeAttributes.Interface | TypeAttributes.Abstract;
            }
            if ((bool?)t["abstract"] == true)
            {
                attrs |= TypeAttributes.Abstract;
            }
            if ((bool?)t["sealed"] == true || kind == "struct")
            {
                attrs |= TypeAttributes.Sealed;
            }
            var baseType = t["base"] is { } b ? Ref(b) : default;
            mb.AddTypeDefinition(attrs, mb.GetOrAddString((string)t["ns"]!), mb.GetOrAddString((string)t["name"]!),
                baseType, MetadataTokens.FieldDefinitionHandle(1), firstMethod[i]);
            var typeParameters = t["generic"]?.AsArray() ?? new JsonArray();
            for (var g = 0; g < typeParameters.Count; g++)
            {
                genericParams.Add((handle, (string)typeParameters[g]!, g));
            }
            foreach (var iface in t["interfaces"]!.AsArray())
            {
                interfaces.Add((handle, Ref(iface!)));
            }
            var methods = new List<MethodDefinitionHandle>();
            foreach (var mnode in t["methods"]!.AsArray())
            {
                var m = mnode!.AsObject();
                var isStatic = (bool?)m["static"] == true;
                var isAbstract = (bool?)m["abstract"] == true;
                var mattrs = (string?)m["access"] == "private" ? MethodAttributes.Private : MethodAttributes.Public;
                mattrs |= MethodAttributes.HideBySig;
                if (isStatic) mattrs |= MethodAttributes.Static;
                if ((bool?)m["virtual"] == true) mattrs |= MethodAttributes.Virtual;
                if ((bool?)m["newslot"] == true) mattrs |= MethodAttributes.NewSlot;
                if (isAbstract) mattrs |= MethodAttributes.Abstract;
                if ((bool?)m["final"] == true) mattrs |= MethodAttributes.Final;
                if ((bool?)m["specialName"] == true) mattrs |= MethodAttributes.SpecialName;
                var ps = m["params"]!.AsArray();
                var sig = MethodSig(isStatic, m["ret"], ps);
                var body = -1;
                if (!isAbstract)
                {
                    var code = new InstructionEncoder(new BlobBuilder());
                    code.OpCode(ILOpCode.Ret);
                    body = bodies.AddMethodBody(code);
                }
                var firstParam = MetadataTokens.ParameterHandle(paramRow);
                for (var p = 0; p < ps.Count; p++)
                {
                    mb.AddParameter(ParameterAttributes.None, mb.GetOrAddString($"x{p}"), p + 1);
                    paramRow++;
                }
                var mh = mb.AddMethodDefinition(mattrs, MethodImplAttributes.IL, mb.GetOrAddString((string)m["name"]!),
                    sig, body, firstParam);
                methods.Add(mh);
                methodRow++;
                foreach (var impl in m["impls"]?.AsArray() ?? new JsonArray())
                {
                    impls.Add((handle, mh, impl!.AsObject()));
                }
            }
            var props = t["properties"]?.AsArray() ?? new JsonArray();
            if (props.Count > 0)
            {
                propertyMaps.Add((handle, MetadataTokens.PropertyDefinitionHandle(propertyRow)));
            }
            foreach (var pnode in props)
            {
                var p = pnode!.AsObject();
                var blob = new BlobBuilder();
                new BlobEncoder(blob).PropertySignature(isInstanceProperty: true).Parameters(
                    0,
                    r =>
                    {
                        Mods(r.CustomModifiers(), p["type"]!);
                        EncodeType(r.Type(), p["type"]!["type"]!);
                    },
                    _ => { });
                var ph = mb.AddProperty(PropertyAttributes.None, mb.GetOrAddString((string)p["name"]!), mb.GetOrAddBlob(blob));
                semantics.Add((ph, (propertyRow << 1) | 1, MethodSemanticsAttributes.Getter, methods[(int)p["getter"]!]));
                propertyRow++;
            }
            var events = t["events"]?.AsArray() ?? new JsonArray();
            if (events.Count > 0)
            {
                eventMaps.Add((handle, MetadataTokens.EventDefinitionHandle(eventRow)));
            }
            foreach (var enode in events)
            {
                var e = enode!.AsObject();
                var eh = mb.AddEvent(EventAttributes.None, mb.GetOrAddString((string)e["name"]!), Ref(e["type"]!));
                foreach (var (role, semantic) in new[]
                         {
                             ("add", MethodSemanticsAttributes.Adder),
                             ("remove", MethodSemanticsAttributes.Remover),
                             ("fire", MethodSemanticsAttributes.Raiser),
                         })
                {
                    if (e[role] is JsonNode index)
                    {
                        semantics.Add((eh, eventRow << 1, semantic, methods[(int)index]));
                    }
                }
                eventRow++;
            }
        }
        foreach (var (type, iface) in interfaces.OrderBy(x => MetadataTokens.GetRowNumber(x.Item1))
                     .ThenBy(x => CodedIndex.TypeDefOrRefOrSpec(x.Item2)))
        {
            mb.AddInterfaceImplementation(type, iface);
        }
        foreach (var (type, first) in propertyMaps)
        {
            mb.AddPropertyMap(type, first);
        }
        foreach (var (type, first) in eventMaps)
        {
            mb.AddEventMap(type, first);
        }
        foreach (var (owner, parameterName, index) in genericParams)
        {
            mb.AddGenericParameter(owner, GenericParameterAttributes.None, mb.GetOrAddString(parameterName), index);
        }
        foreach (var (association, _, kind, method) in semantics.OrderBy(x => x.Item2))
        {
            mb.AddMethodSemantics(association, kind, method);
        }
        foreach (var (type, body, decl) in impls)
        {
            var parent = Ref(decl["parent"]!);
            var sig = MethodSig(false, decl["ret"], decl["params"]!.AsArray());
            var declHandle = mb.AddMemberReference(parent, mb.GetOrAddString((string)decl["name"]!), sig);
            mb.AddMethodImplementation(type, body, declHandle);
        }
        foreach (var f in request["forwarders"]?.AsArray() ?? new JsonArray())
        {
            mb.AddExportedType((TypeAttributes)0x00200000 /* Forwarder */, mb.GetOrAddString((string)f!["ns"]!),
                mb.GetOrAddString((string)f["name"]!), refs[(int)f["ref"]!], 0);
        }

        var pe = new ManagedPEBuilder(PEHeaderBuilder.CreateLibraryHeader(), new MetadataRootBuilder(mb), il);
        var output = new BlobBuilder();
        pe.Serialize(output);
        var path = (string)request["path"]!;
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllBytes(path, output.ToArray());
        Program.Rewritten(path);
        return new JsonObject { ["ok"] = true };
    }

    private static StringHandle Culture(MetadataBuilder mb, string? culture) =>
        string.IsNullOrEmpty(culture) ? default : mb.GetOrAddString(culture);
}

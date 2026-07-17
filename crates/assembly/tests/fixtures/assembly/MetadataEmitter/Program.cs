using System;
using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;

// Emit a managed assembly whose metadata exhibits a shape no C#/F# compiler
// will produce, using the in-box (MIT-licensed) System.Reflection.Metadata.
// This is the mechanism for the defensive "fails-loud" projector tests.
//
// The shape is selected by argv[0]; the raw PE bytes go to stdout so the
// Rust test can feed them straight into `Ecma335Assembly::parse` with no temp
// files. See `crates/assembly/tests/all/projector_malformed_metadata.rs`.
internal static class Program
{
    private static int Main(string[] args)
    {
        string shape = args.Length > 0 ? args[0] : "";
        BlobBuilder pe = shape switch
        {
            "vararg" => EmitVararg(),
            "mixed_good_and_bad_member" => EmitMixedGoodAndBadMember(),
            "currentmodule_typeref_base" => EmitCurrentModuleTypeRefBase(),
            "linked_file_resource" => EmitLinkedFileResource(),
            "param_custom_modifier" => EmitParamCustomModifier(),
            "return_custom_modifier" => EmitReturnCustomModifier(),
            "param_optional_modifier" => EmitParamOptionalModifier(),
            "param_volatile_modreq" => EmitParamVolatileModreq(),
            "param_in_modreq_not_byref" => EmitParamInModreqNotByref(),
            "typed_reference_param" => EmitTypedReferenceParam(),
            "compiler_controlled_method" => EmitCompilerControlledMethod(),
            "external_module_typeref" => EmitExternalModuleTypeRef(),
            "bounded_array" => EmitArrayShape("BoundedArrayFixture", "boundarr", ArrayKind.Bounded),
            "rank_one_array" => EmitArrayShape("RankOneArrayFixture", "rankarr", ArrayKind.RankOne),
            "array_custom_modifier" => EmitArrayShape("ArrayModifierFixture", "modarr", ArrayKind.Modifier),
            "nullable_invalid_byte" => EmitNullableInvalidByte(),
            "nullable_byte_array_form" => EmitNullableByteArrayForm(),
            "duplicate_nullable_context" => EmitDuplicateNullableContext(),
            "nullable_named_args" => EmitNullableNamedArgs(),
            "nullable_vector_extra_bytes" => EmitNullableVector(VectorKind.ExtraBytes),
            "nullable_vector_insufficient_bytes" => EmitNullableVector(VectorKind.InsufficientBytes),
            "nullable_pointer_vector" => EmitNullablePointerVector(),
            "cfr_null_ctor_arg" => EmitCfrNullCtorArg(),
            "cfr_wrong_arity" => EmitCfrWrongArity(),
            "cfr_unexpected_named_arg" => EmitCfrUnexpectedNamedArg(),
            "cfr_non_bool_is_optional" => EmitCfrNonBoolIsOptional(),
            "default_member_named_args" => EmitDefaultMemberNamedArgs(),
            "default_member_null_ctor_arg" => EmitDefaultMemberNullCtorArg(),
            "property_generic_accessor" => EmitPropertyGenericGetter(),
            "property_init_only_setter" => EmitPropertyInitOnlySetter(),
            "method_init_marker_void" => EmitMethodInitMarkerVoid(),
            "method_modopt_void" => EmitMethodModoptVoid(),
            "event_other_accessor" => EmitEventOtherAccessor(),
            "event_disagreeing_static" => EmitEventDisagreeingStatic(),
            "event_modreq_accessor" => EmitEventModreqAccessor(),
            "event_generic_accessor" => EmitEventGenericAccessor(),
            "generic_arity_mismatch" => EmitGenericArityMismatch(),
            "methodimpl_unmangled_body" => EmitMethodImplUnmangledBody(),
            "methodimpl_multi_decl" => EmitMethodImplMultiDecl(),
            "methodimpl_external_iface_unmangled" => EmitMethodImplExternalIfaceUnmangled(),
            "methodimpl_dup_typeref" => EmitMethodImplDupTypeRef(),
            "methodimpl_split_property" => EmitMethodImplSplitProperty(),
            "methodimpl_iface_via_interface" => EmitMethodImplIfaceViaInterface(),
            "methodimpl_iface_via_base" => EmitMethodImplIfaceViaBase(),
            "methodimpl_unconventional_accessor" => EmitMethodImplUnconventionalAccessor(),
            "methodimpl_local_generic_iface_memberref" => EmitMethodImplLocalGenericIfaceMemberRef(),
            "methodimpl_external_class_decl" => EmitMethodImplExternalClassDecl(),
            "methodimpl_external_accessor_decl" => EmitMethodImplExternalAccessorDecl(),
            "methodimpl_class_mismatch" => EmitMethodImplClassMismatch(),
            "methodimpl_class_out_of_range" => EmitMethodImplClassOutOfRange(),
            "methodimpl_unrelated_local_iface" => EmitMethodImplUnrelatedLocalIface(),
            "methodimpl_external_inherited_iface" => EmitMethodImplExternalInheritedIface(),
            "methodimpl_generic_inherited_local_iface" => EmitMethodImplGenericInheritedLocalIface(),
            "methodimpl_fbounded_growth" => EmitMethodImplFBoundedGrowth(),
            "methodimpl_overloaded_external_accessor_decls" => EmitMethodImplOverloadedExternalAccessorDecls(),
            "methodimpl_shared_event_accessor" => EmitMethodImplSharedEventAccessor(),
            "methodimpl_reabstraction" => EmitMethodImplReabstraction(),
            "methodimpl_dup_typeref_sig" => EmitMethodImplDupTypeRefSig(),
            "methodimpl_event_fire_impl" => EmitMethodImplEventFireImpl(),
            "methodimpl_other_accessor_decl" => EmitMethodImplOtherAccessorDecl(),
            "methodimpl_multi_owner_accessor" => EmitMethodImplMultiOwnerAccessor(),
            "methodimpl_module_typeref_decl" => EmitMethodImplModuleTypeRefDecl(),
            "unmanaged_attribute_without_struct" => EmitUnmanagedAttributeWithoutStruct(),
            "constraint_modreq" => EmitConstraintModreq(),
            "constraint_unmanaged_modreq_non_value_type" => EmitConstraintUnmanagedModreqNonValueType(),
            "unmanaged_modreq_without_struct" => EmitUnmanagedModreqWithoutStruct(),
            "unmanaged_modreq_behind_modopt" => EmitUnmanagedModreqBehindModopt(),
            _ => throw new ArgumentException($"unknown shape: {shape}"),
        };
        using var stdout = Console.OpenStandardOutput();
        pe.WriteContentTo(stdout);
        return 0;
    }

    // ----- Element-type bytes (ECMA-335 II.23.1.16) --------------------------

    private const byte ELEMENT_TYPE_VOID = 0x01;
    private const byte ELEMENT_TYPE_BOOLEAN = 0x02;
    private const byte ELEMENT_TYPE_U1 = 0x05;
    private const byte ELEMENT_TYPE_I4 = 0x08;
    private const byte ELEMENT_TYPE_STRING = 0x0E;
    private const byte ELEMENT_TYPE_PTR = 0x0F;
    private const byte ELEMENT_TYPE_VALUETYPE = 0x11;
    private const byte ELEMENT_TYPE_CLASS = 0x12;
    private const byte ELEMENT_TYPE_ARRAY = 0x14;
    private const byte ELEMENT_TYPE_GENERICINST = 0x15;
    private const byte ELEMENT_TYPE_VAR = 0x13;
    private const byte ELEMENT_TYPE_TYPEDBYREF = 0x16;
    private const byte ELEMENT_TYPE_MVAR = 0x1E;
    private const byte ELEMENT_TYPE_SZARRAY = 0x1D;
    private const byte ELEMENT_TYPE_CMOD_REQD = 0x1F;
    private const byte ELEMENT_TYPE_CMOD_OPT = 0x20;
    private const byte CALLCONV_GENERIC = 0x10;
    private const byte CALLCONV_PROPERTY = 0x08;
    private const byte HASTHIS = 0x20;

    // Custom-attribute named-arg kind bytes (ECMA-335 II.23.3).
    private const byte CA_FIELD = 0x53;
    private const byte CA_PROPERTY = 0x54;

    private const MethodAttributes AbstractPublic =
        MethodAttributes.Public | MethodAttributes.HideBySig
            | MethodAttributes.NewSlot | MethodAttributes.Abstract | MethodAttributes.Virtual;

    // ----- Shared scaffolding ------------------------------------------------

    private static void Preamble(MetadataBuilder mb, string name, string mvid)
    {
        mb.AddModule(
            generation: 0,
            moduleName: mb.GetOrAddString(name + ".dll"),
            mvid: mb.GetOrAddGuid(MvidFor(mvid)),
            encId: default,
            encBaseId: default);

        mb.AddAssembly(
            name: mb.GetOrAddString(name),
            version: new Version(1, 0, 0, 0),
            culture: default,
            publicKey: default,
            flags: 0,
            hashAlgorithm: AssemblyHashAlgorithm.None);
    }

    // Deterministic MVID from a short tag; avoids threading real GUIDs through.
    private static Guid MvidFor(string tag)
    {
        var bytes = new byte[16];
        for (int i = 0; i < tag.Length && i < 16; i++)
        {
            bytes[i] = (byte)tag[i];
        }
        return new Guid(bytes);
    }

    // The synthetic `<Module>` type must occupy TypeDef row 1.
    private static void AddModuleType(MetadataBuilder mb) =>
        mb.AddTypeDefinition(
            default,
            default,
            mb.GetOrAddString("<Module>"),
            baseType: default,
            fieldList: MetadataTokens.FieldDefinitionHandle(1),
            methodList: MetadataTokens.MethodDefinitionHandle(1));

    private static AssemblyReferenceHandle AddMscorlib(MetadataBuilder mb) =>
        mb.AddAssemblyReference(
            mb.GetOrAddString("mscorlib"),
            new Version(4, 0, 0, 0),
            culture: default,
            publicKeyOrToken: default,
            flags: 0,
            hashValue: default);

    private static TypeReferenceHandle AddIsConst(MetadataBuilder mb) =>
        mb.AddTypeReference(
            AddMscorlib(mb),
            mb.GetOrAddString("System.Runtime.CompilerServices"),
            mb.GetOrAddString("IsConst"));

    private static TypeReferenceHandle AddIsVolatile(MetadataBuilder mb) =>
        mb.AddTypeReference(
            AddMscorlib(mb),
            mb.GetOrAddString("System.Runtime.CompilerServices"),
            mb.GetOrAddString("IsVolatile"));

    private static TypeReferenceHandle AddInAttribute(MetadataBuilder mb) =>
        mb.AddTypeReference(
            AddMscorlib(mb),
            mb.GetOrAddString("System.Runtime.InteropServices"),
            mb.GetOrAddString("InAttribute"));

    // A TypeDefOrRefOrSpec coded index, compressed into a signature blob.
    private static void WriteTypeToken(BlobBuilder sig, EntityHandle handle) =>
        sig.WriteCompressedInteger(CodedIndex.TypeDefOrRefOrSpec(handle));

    private static BlobBuilder Finish(MetadataBuilder mb)
    {
        var root = new MetadataRootBuilder(mb);
        var header = new PEHeaderBuilder(
            imageCharacteristics: Characteristics.ExecutableImage | Characteristics.Dll);
        var peBuilder = new ManagedPEBuilder(
            header,
            root,
            ilStream: new BlobBuilder(),
            entryPoint: default);

        var peBlob = new BlobBuilder();
        peBuilder.Serialize(peBlob);
        return peBlob;
    }

    // Build a public interface `Host` carrying one abstract method whose
    // MethodDefSig is produced by `buildSig` (which may add the TypeRefs the
    // signature references). Abstract ⇒ no IL body, so no body RVA / corlib
    // reference is needed to make the assembly parseable. `methodFlags` lets a
    // caller bend the accessibility (e.g. PrivateScope).
    private static BlobBuilder EmitAbstractMethod(
        string fixtureName,
        string mvid,
        string methodName,
        MethodAttributes methodFlags,
        Func<MetadataBuilder, BlobHandle> buildSig)
    {
        var mb = new MetadataBuilder();
        Preamble(mb, fixtureName, mvid);

        BlobHandle sig = buildSig(mb);
        mb.AddMethodDefinition(
            methodFlags,
            MethodImplAttributes.IL,
            mb.GetOrAddString(methodName),
            sig,
            bodyOffset: -1,
            parameterList: MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Interface | TypeAttributes.Abstract,
            default,
            mb.GetOrAddString("Host"),
            baseType: default,
            fieldList: MetadataTokens.FieldDefinitionHandle(1),
            methodList: MetadataTokens.MethodDefinitionHandle(1));

        return Finish(mb);
    }

    // ----- Method-signature shapes -------------------------------------------

    // A public interface `Logger` with one abstract method `Log` whose
    // signature carries the VARARG calling convention (0x05). C# cannot
    // express this; the projector must reject it as an unsupported signature.
    private static BlobBuilder EmitVararg() =>
        EmitAbstractMethod("VarargFixture", "vararg", "Log", AbstractPublic, mb =>
        {
            var sig = new BlobBuilder();
            sig.WriteByte(0x05); // IMAGE_CEE_CS_CALLCONV_VARARG
            sig.WriteByte(0x00); // parameter count
            sig.WriteByte(ELEMENT_TYPE_VOID);
            return mb.GetOrAddBlob(sig);
        });

    // A public interface `Host` with two abstract methods: a well-formed
    // `void Good()` and a `void Bad()` carrying the VARARG calling convention.
    // The projector must drop only `Bad` (recording it), keeping the type and
    // `Good` — the per-member "bound uncertainty" contract. Unlike the other
    // shapes here (one bad member each), this one pins that a *sibling* survives.
    private static BlobBuilder EmitMixedGoodAndBadMember()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MixedMemberFixture", "mixed-mem");

        // Method RID 1: `void Good()` — a plain nullary method that projects.
        var goodSig = new BlobBuilder();
        goodSig.WriteByte(HASTHIS);
        goodSig.WriteByte(0x00); // 0 params
        goodSig.WriteByte(ELEMENT_TYPE_VOID);
        mb.AddMethodDefinition(
            AbstractPublic,
            MethodImplAttributes.IL,
            mb.GetOrAddString("Good"),
            mb.GetOrAddBlob(goodSig),
            bodyOffset: -1,
            parameterList: MetadataTokens.ParameterHandle(1));

        // Method RID 2: `void Bad()` with VARARG (0x05) — refused, so dropped.
        var badSig = new BlobBuilder();
        badSig.WriteByte(0x05); // IMAGE_CEE_CS_CALLCONV_VARARG
        badSig.WriteByte(0x00); // parameter count
        badSig.WriteByte(ELEMENT_TYPE_VOID);
        mb.AddMethodDefinition(
            AbstractPublic,
            MethodImplAttributes.IL,
            mb.GetOrAddString("Bad"),
            mb.GetOrAddBlob(badSig),
            bodyOffset: -1,
            parameterList: MetadataTokens.ParameterHandle(1));

        AddModuleType(mb); // <Module> = TypeDef row 1, owns no methods.
        mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Interface | TypeAttributes.Abstract,
            default,
            mb.GetOrAddString("Host"),
            baseType: default,
            fieldList: MetadataTokens.FieldDefinitionHandle(1),
            methodList: MetadataTokens.MethodDefinitionHandle(1)); // owns methods 1..2

        return Finish(mb);
    }

    // `void Take(modreq(IsConst) int32)` — a custom modifier on a parameter.
    private static BlobBuilder EmitParamCustomModifier() =>
        EmitAbstractMethod("ParamCustomModFixture", "param-mod", "Take", AbstractPublic, mb =>
        {
            TypeReferenceHandle isConst = AddIsConst(mb);
            var sig = new BlobBuilder();
            sig.WriteByte(HASTHIS);
            sig.WriteByte(0x01); // 1 param
            sig.WriteByte(ELEMENT_TYPE_VOID); // return
            sig.WriteByte(ELEMENT_TYPE_CMOD_REQD);
            WriteTypeToken(sig, isConst);
            sig.WriteByte(ELEMENT_TYPE_I4);
            return mb.GetOrAddBlob(sig);
        });

    // `modreq(IsConst) int32 Get()` — a custom modifier on the return type.
    private static BlobBuilder EmitReturnCustomModifier() =>
        EmitAbstractMethod("ReturnCustomModFixture", "ret-mod", "Get", AbstractPublic, mb =>
        {
            TypeReferenceHandle isConst = AddIsConst(mb);
            var sig = new BlobBuilder();
            sig.WriteByte(HASTHIS);
            sig.WriteByte(0x00); // 0 params
            sig.WriteByte(ELEMENT_TYPE_CMOD_REQD);
            WriteTypeToken(sig, isConst);
            sig.WriteByte(ELEMENT_TYPE_I4);
            return mb.GetOrAddBlob(sig);
        });

    // `void Take(modopt(IsConst) int32)` — an *optional* modifier on a
    // parameter. ECMA-335 II.7.1.1: a tool that does not understand a `modopt`
    // may ignore it, so — unlike the `modreq` above — this projects cleanly with
    // the modifier dropped, and the parameter reads as a plain `int32`.
    private static BlobBuilder EmitParamOptionalModifier() =>
        EmitAbstractMethod("ParamOptionalModFixture", "param-modopt", "Take", AbstractPublic, mb =>
        {
            TypeReferenceHandle isConst = AddIsConst(mb);
            var sig = new BlobBuilder();
            sig.WriteByte(HASTHIS);
            sig.WriteByte(0x01); // 1 param
            sig.WriteByte(ELEMENT_TYPE_VOID); // return
            sig.WriteByte(ELEMENT_TYPE_CMOD_OPT);
            WriteTypeToken(sig, isConst);
            sig.WriteByte(ELEMENT_TYPE_I4);
            return mb.GetOrAddBlob(sig);
        });

    // `void Take(modreq(IsVolatile) int32)` — the `volatile` marker on a
    // *parameter*. It is understood only on a field type (it is the encoding of
    // C#'s `volatile`); a required modifier elsewhere must not be dropped, so
    // the member is refused.
    private static BlobBuilder EmitParamVolatileModreq() =>
        EmitAbstractMethod("ParamVolatileFixture", "param-volatile", "Take", AbstractPublic, mb =>
        {
            TypeReferenceHandle isVolatile = AddIsVolatile(mb);
            var sig = new BlobBuilder();
            sig.WriteByte(HASTHIS);
            sig.WriteByte(0x01); // 1 param
            sig.WriteByte(ELEMENT_TYPE_VOID); // return
            sig.WriteByte(ELEMENT_TYPE_CMOD_REQD);
            WriteTypeToken(sig, isVolatile);
            sig.WriteByte(ELEMENT_TYPE_I4);
            return mb.GetOrAddBlob(sig);
        });

    // `void Take(modreq(InAttribute) int32)` — the read-only-ref marker with no
    // byref under it. It qualifies a *reference*; over a plain value there is
    // nothing for it to mean, so the member is refused rather than have the
    // marker silently dropped.
    private static BlobBuilder EmitParamInModreqNotByref() =>
        EmitAbstractMethod("ParamInNotByrefFixture", "param-in", "Take", AbstractPublic, mb =>
        {
            TypeReferenceHandle inAttr = AddInAttribute(mb);
            var sig = new BlobBuilder();
            sig.WriteByte(HASTHIS);
            sig.WriteByte(0x01); // 1 param
            sig.WriteByte(ELEMENT_TYPE_VOID); // return
            sig.WriteByte(ELEMENT_TYPE_CMOD_REQD);
            WriteTypeToken(sig, inAttr);
            sig.WriteByte(ELEMENT_TYPE_I4);
            return mb.GetOrAddBlob(sig);
        });

    // `void Take(typedref)` — an ELEMENT_TYPE_TYPEDBYREF parameter (the
    // tri-mode value-type + managed-pointer + type-handle special).
    private static BlobBuilder EmitTypedReferenceParam() =>
        EmitAbstractMethod("TypedRefFixture", "typedref", "Take", AbstractPublic, mb =>
        {
            var sig = new BlobBuilder();
            sig.WriteByte(HASTHIS);
            sig.WriteByte(0x01); // 1 param
            sig.WriteByte(ELEMENT_TYPE_VOID);
            sig.WriteByte(ELEMENT_TYPE_TYPEDBYREF);
            return mb.GetOrAddBlob(sig);
        });

    // A method whose MethodAttributes MemberAccess mask is PrivateScope (0x0) —
    // the `compilercontrolled` accessibility no C#/F# compiler emits.
    private static BlobBuilder EmitCompilerControlledMethod() =>
        EmitAbstractMethod(
            "CompilerControlledFixture",
            "privscope",
            "Hidden",
            MethodAttributes.HideBySig | MethodAttributes.NewSlot
                | MethodAttributes.Abstract | MethodAttributes.Virtual,
            mb =>
            {
                var sig = new BlobBuilder();
                sig.WriteByte(HASTHIS);
                sig.WriteByte(0x00);
                sig.WriteByte(ELEMENT_TYPE_VOID);
                return mb.GetOrAddBlob(sig);
            });

    // ----- Array-shape shapes (malformed generic argument of an interface) ---

    private enum ArrayKind { Bounded, RankOne, Modifier }

    // A public class `Weird` implementing `IEnumerable<T>` whose `T` is a
    // not-vector array shape: a bounded `int[10..]`, a rank-1 `int[*]`, or a
    // `modreq(IsConst) int[]`. The TypeRef model carries only `rank`, so each
    // must fail loud rather than flatten to a plain `int[]`.
    private static BlobBuilder EmitArrayShape(string fixtureName, string mvid, ArrayKind kind)
    {
        var mb = new MetadataBuilder();
        Preamble(mb, fixtureName, mvid);

        AssemblyReferenceHandle mscorlib = AddMscorlib(mb);
        TypeReferenceHandle ienum = mb.AddTypeReference(
            mscorlib,
            mb.GetOrAddString("System.Collections.Generic"),
            mb.GetOrAddString("IEnumerable`1"));
        TypeReferenceHandle isConst = kind == ArrayKind.Modifier
            ? mb.AddTypeReference(
                mscorlib,
                mb.GetOrAddString("System.Runtime.CompilerServices"),
                mb.GetOrAddString("IsConst"))
            : default;

        // TypeSpec: GENERICINST CLASS IEnumerable`1 <1> <element-array>.
        var spec = new BlobBuilder();
        spec.WriteByte(ELEMENT_TYPE_GENERICINST);
        spec.WriteByte(ELEMENT_TYPE_CLASS);
        WriteTypeToken(spec, ienum);
        spec.WriteCompressedInteger(1); // generic argument count
        switch (kind)
        {
            case ArrayKind.Modifier:
                // SZARRAY CMOD_REQD(IsConst) I4
                spec.WriteByte(ELEMENT_TYPE_SZARRAY);
                spec.WriteByte(ELEMENT_TYPE_CMOD_REQD);
                WriteTypeToken(spec, isConst);
                spec.WriteByte(ELEMENT_TYPE_I4);
                break;
            case ArrayKind.Bounded:
                // ARRAY I4 rank=1 numSizes=1 size=10 numLoBounds=1 loBound=1
                spec.WriteByte(ELEMENT_TYPE_ARRAY);
                spec.WriteByte(ELEMENT_TYPE_I4);
                spec.WriteCompressedInteger(1);
                spec.WriteCompressedInteger(1);
                spec.WriteCompressedInteger(10);
                spec.WriteCompressedInteger(1);
                spec.WriteCompressedSignedInteger(1);
                break;
            case ArrayKind.RankOne:
                // ARRAY I4 rank=1 numSizes=0 numLoBounds=0  (T[*])
                spec.WriteByte(ELEMENT_TYPE_ARRAY);
                spec.WriteByte(ELEMENT_TYPE_I4);
                spec.WriteCompressedInteger(1);
                spec.WriteCompressedInteger(0);
                spec.WriteCompressedInteger(0);
                break;
        }
        TypeSpecificationHandle specHandle = mb.AddTypeSpecification(mb.GetOrAddBlob(spec));

        AddModuleType(mb);
        TypeDefinitionHandle weird = mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Class,
            default,
            mb.GetOrAddString("Weird"),
            baseType: default,
            fieldList: MetadataTokens.FieldDefinitionHandle(1),
            methodList: MetadataTokens.MethodDefinitionHandle(1));
        mb.AddInterfaceImplementation(weird, specHandle);

        return Finish(mb);
    }

    // ----- Structural shapes -------------------------------------------------

    // A public class `User` whose `extends` points at a TypeRef whose
    // ResolutionScope is an external ModuleRef (ECMA-335 II.22.38). C#/F#
    // never scope a base type to a sibling module, so the projector's
    // `ResolutionScope::ExternalModule` arm has no compiler fixture — this
    // fabricates one. The projection must fail loud (UnsupportedEcmaLayout).
    private static BlobBuilder EmitExternalModuleTypeRef()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "ExternalModuleFixture", "extmod");

        ModuleReferenceHandle modRef = mb.AddModuleReference(mb.GetOrAddString("OtherModule"));
        TypeReferenceHandle baseRef = mb.AddTypeReference(
            modRef,
            mb.GetOrAddString("App"),
            mb.GetOrAddString("Helper"));

        AddModuleType(mb);
        mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Class,
            default,
            mb.GetOrAddString("User"),
            baseType: baseRef,
            fieldList: MetadataTokens.FieldDefinitionHandle(1),
            methodList: MetadataTokens.MethodDefinitionHandle(1));

        return Finish(mb);
    }

    // A public class `User` whose `extends` points at a TypeRef whose
    // ResolutionScope is the current module (ECMA-335 II.22.38, Module-coded
    // index). C# names a same-module base through a direct TypeDef token, never
    // a CurrentModule-scoped TypeRef, so the projector's
    // `ResolutionScope::CurrentModule => SameAssembly` arm has no
    // compiler-produced fixture — this fabricates one. The projection must
    // collapse it to `assembly: None`, not a cross-asm ref to our own identity.
    private static BlobBuilder EmitCurrentModuleTypeRefBase()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "CurrentModuleFixture", "curmod");

        TypeReferenceHandle baseRef = mb.AddTypeReference(
            resolutionScope: MetadataTokens.Handle(TableIndex.Module, 1),
            @namespace: mb.GetOrAddString("App"),
            name: mb.GetOrAddString("Helper"));

        AddModuleType(mb);
        mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Class,
            default,
            mb.GetOrAddString("User"),
            baseType: baseRef,
            fieldList: MetadataTokens.FieldDefinitionHandle(1),
            methodList: MetadataTokens.MethodDefinitionHandle(1));

        return Finish(mb);
    }

    // A ManifestResource whose Implementation coded index names a File-table
    // row (ECMA-335 II.22.24), i.e. the resource lives in a *separate* file,
    // not embedded in this assembly. C#/F# `--resource` always embeds in the
    // current file (a null Implementation), so this shape has no compiler
    // fixture; it pins the reader's refusal of a non-CurrentFile resource.
    private static BlobBuilder EmitLinkedFileResource()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "LinkedResourceFixture", "linkres");

        AssemblyFileHandle file = mb.AddAssemblyFile(
            name: mb.GetOrAddString("payload.bin"),
            hashValue: default,
            containsMetadata: false);

        mb.AddManifestResource(
            attributes: ManifestResourceAttributes.Public,
            name: mb.GetOrAddString("LinkedResource"),
            implementation: file,
            offset: 0);

        AddModuleType(mb);
        return Finish(mb);
    }

    // ----- Custom-attribute blob shapes --------------------------------------
    //
    // the reader decodes the CA blob into a `DecodedAttribute` model
    // (fixed_args / named_args) per the ctor signature; our projector
    // then inspects that and refuses the malformed shapes below. Each is
    // fabricated as a raw CA Value blob (ECMA-335 II.23.3) attached via a
    // `.ctor` MemberRef on a `System.Runtime`-flavoured TypeRef.

    // A SerString (II.23.3): null is the single byte 0xFF; otherwise a
    // compressed length prefix followed by the UTF-8 bytes.
    private static void WriteSerString(BlobBuilder b, string? s)
    {
        if (s is null)
        {
            b.WriteByte(0xFF);
            return;
        }
        byte[] bytes = System.Text.Encoding.UTF8.GetBytes(s);
        b.WriteCompressedInteger(bytes.Length);
        b.WriteBytes(bytes);
    }

    // A CA Value blob: the 0x0001 prolog, then whatever `body` writes (fixed
    // args, the 2-byte NumNamed, then named args).
    private static BlobHandle CaBlob(MetadataBuilder mb, Action<BlobBuilder> body)
    {
        var b = new BlobBuilder();
        b.WriteUInt16(1); // prolog
        body(b);
        return mb.GetOrAddBlob(b);
    }

    // A `.ctor` MemberRef on `<ns>.<attrName>` (a TypeRef into mscorlib), whose
    // signature has the given parameter element-type byte sequences.
    private static MemberReferenceHandle AddAttrCtor(
        MetadataBuilder mb, string ns, string attrName, byte[][] paramTypes)
    {
        TypeReferenceHandle attrType = mb.AddTypeReference(
            AddMscorlib(mb), mb.GetOrAddString(ns), mb.GetOrAddString(attrName));
        var sig = new BlobBuilder();
        sig.WriteByte(HASTHIS);
        sig.WriteByte((byte)paramTypes.Length);
        sig.WriteByte(ELEMENT_TYPE_VOID); // a .ctor returns void
        foreach (byte[] pt in paramTypes)
        {
            sig.WriteBytes(pt);
        }
        return mb.AddMemberReference(attrType, mb.GetOrAddString(".ctor"), mb.GetOrAddBlob(sig));
    }

    // A public class `Subject` carrying one custom attribute.
    private static BlobBuilder EmitAttrOnType(
        string fixtureName, string mvid, string ns, string attrName,
        byte[][] ctorParamTypes, Action<BlobBuilder> caBody)
    {
        var mb = new MetadataBuilder();
        Preamble(mb, fixtureName, mvid);

        MemberReferenceHandle ctor = AddAttrCtor(mb, ns, attrName, ctorParamTypes);
        AddModuleType(mb);
        TypeDefinitionHandle td = mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Class,
            default,
            mb.GetOrAddString("Subject"),
            baseType: default,
            fieldList: MetadataTokens.FieldDefinitionHandle(1),
            methodList: MetadataTokens.MethodDefinitionHandle(1));
        mb.AddCustomAttribute(td, ctor, CaBlob(mb, caBody));
        return Finish(mb);
    }

    // A public interface `Host` with one abstract generic method `T Pick<T>()`;
    // `attach` hangs custom attributes off the method and/or its typar `T`.
    private static BlobBuilder EmitGenericMethodWithCa(
        string fixtureName, string mvid,
        Action<MetadataBuilder, MethodDefinitionHandle, GenericParameterHandle> attach)
    {
        var mb = new MetadataBuilder();
        Preamble(mb, fixtureName, mvid);

        // GENERIC|HASTHIS, 1 generic param, 0 params, ret MVAR 0.
        var sig = new BlobBuilder();
        sig.WriteByte(CALLCONV_GENERIC | HASTHIS);
        sig.WriteCompressedInteger(1);
        sig.WriteByte(0x00);
        sig.WriteByte(ELEMENT_TYPE_MVAR);
        sig.WriteCompressedInteger(0);
        MethodDefinitionHandle method = mb.AddMethodDefinition(
            AbstractPublic,
            MethodImplAttributes.IL,
            mb.GetOrAddString("Pick"),
            mb.GetOrAddBlob(sig),
            bodyOffset: -1,
            parameterList: MetadataTokens.ParameterHandle(1));
        GenericParameterHandle gp = mb.AddGenericParameter(
            method, GenericParameterAttributes.None, mb.GetOrAddString("T"), 0);

        attach(mb, method, gp);

        AddModuleType(mb);
        mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Interface | TypeAttributes.Abstract,
            default,
            mb.GetOrAddString("Host"),
            baseType: default,
            fieldList: MetadataTokens.FieldDefinitionHandle(1),
            methodList: MetadataTokens.MethodDefinitionHandle(1));
        return Finish(mb);
    }

    // GENERICINST CLASS List`1 <1> <element>.
    private static void WriteListOf(
        BlobBuilder sig, TypeReferenceHandle listRef, Action<BlobBuilder> writeElem)
    {
        sig.WriteByte(ELEMENT_TYPE_GENERICINST);
        sig.WriteByte(ELEMENT_TYPE_CLASS);
        WriteTypeToken(sig, listRef);
        sig.WriteCompressedInteger(1);
        writeElem(sig);
    }

    private static readonly byte[] CtorU1 = { ELEMENT_TYPE_U1 };
    private static readonly byte[] CtorU1Array = { ELEMENT_TYPE_SZARRAY, ELEMENT_TYPE_U1 };
    private static readonly byte[] CtorString = { ELEMENT_TYPE_STRING };

    // `[NullableAttribute(3)]` on a typar — byte 3 is not a documented nullable
    // state (only 0/1/2).
    private static BlobBuilder EmitNullableInvalidByte() =>
        EmitGenericMethodWithCa("NullableInvalidByteFixture", "nullinv", (mb, _, gp) =>
        {
            MemberReferenceHandle ctor = AddAttrCtor(
                mb, "System.Runtime.CompilerServices", "NullableAttribute", new[] { CtorU1 });
            mb.AddCustomAttribute(gp, ctor, CaBlob(mb, b =>
            {
                b.WriteByte(3);
                b.WriteUInt16(0);
            }));
        });

    // `[NullableAttribute(new byte[]{1})]` on a typar — the byte[] (composite)
    // overload, which we only model on composite type references, not a typar.
    private static BlobBuilder EmitNullableByteArrayForm() =>
        EmitGenericMethodWithCa("NullableByteArrayFixture", "nullarr", (mb, _, gp) =>
        {
            MemberReferenceHandle ctor = AddAttrCtor(
                mb, "System.Runtime.CompilerServices", "NullableAttribute", new[] { CtorU1Array });
            mb.AddCustomAttribute(gp, ctor, CaBlob(mb, b =>
            {
                b.WriteUInt32(1); // byte[] element count
                b.WriteByte(1);
                b.WriteUInt16(0);
            }));
        });

    // Two `[NullableContextAttribute]` rows on one method — at most one is legal.
    private static BlobBuilder EmitDuplicateNullableContext() =>
        EmitGenericMethodWithCa("DupNullableContextFixture", "dupctx", (mb, method, _) =>
        {
            MemberReferenceHandle ctor = AddAttrCtor(
                mb, "System.Runtime.CompilerServices", "NullableContextAttribute", new[] { CtorU1 });
            mb.AddCustomAttribute(method, ctor, CaBlob(mb, b =>
            {
                b.WriteByte(1);
                b.WriteUInt16(0);
            }));
            mb.AddCustomAttribute(method, ctor, CaBlob(mb, b =>
            {
                b.WriteByte(2);
                b.WriteUInt16(0);
            }));
        });

    // `[NullableAttribute(1, Flag = true)]` on a `string` parameter — Roslyn
    // never emits named args on NullableAttribute.
    private static BlobBuilder EmitNullableNamedArgs()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "NullableNamedArgsFixture", "nullnamed");

        MemberReferenceHandle ctor = AddAttrCtor(
            mb, "System.Runtime.CompilerServices", "NullableAttribute", new[] { CtorU1 });

        var sig = new BlobBuilder();
        sig.WriteByte(HASTHIS);
        sig.WriteByte(0x01); // 1 param
        sig.WriteByte(ELEMENT_TYPE_VOID);
        sig.WriteByte(ELEMENT_TYPE_STRING);
        MethodDefinitionHandle method = mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Take"),
            mb.GetOrAddBlob(sig), bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        ParameterHandle param = mb.AddParameter(ParameterAttributes.None, mb.GetOrAddString("s"), 1);
        mb.AddCustomAttribute(param, ctor, CaBlob(mb, b =>
        {
            b.WriteByte(1);   // ctor uint8 = 1
            b.WriteUInt16(1); // 1 named arg
            b.WriteByte(CA_FIELD);
            b.WriteByte(ELEMENT_TYPE_BOOLEAN);
            WriteSerString(b, "Flag");
            b.WriteByte(1);   // true
        }));

        AddModuleType(mb);
        mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Interface | TypeAttributes.Abstract,
            default, mb.GetOrAddString("Host"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        return Finish(mb);
    }

    private enum VectorKind { ExtraBytes, InsufficientBytes }

    // `[NullableAttribute(new byte[]{…})]` on a `List<string>` (extra trailing
    // bytes: 4 supplied, pre-order walk wants 2) or `List<List<string>>`
    // (insufficient: 2 supplied, walk wants 3) parameter.
    private static BlobBuilder EmitNullableVector(VectorKind kind)
    {
        bool extra = kind == VectorKind.ExtraBytes;
        var mb = new MetadataBuilder();
        Preamble(
            mb,
            extra ? "NullableVectorExtraFixture" : "NullableVectorShortFixture",
            extra ? "vecextra" : "vecshort");

        MemberReferenceHandle ctor = AddAttrCtor(
            mb, "System.Runtime.CompilerServices", "NullableAttribute", new[] { CtorU1Array });
        TypeReferenceHandle listRef = mb.AddTypeReference(
            AddMscorlib(mb),
            mb.GetOrAddString("System.Collections.Generic"),
            mb.GetOrAddString("List`1"));

        var sig = new BlobBuilder();
        sig.WriteByte(HASTHIS);
        sig.WriteByte(0x01);
        sig.WriteByte(ELEMENT_TYPE_VOID);
        if (extra)
        {
            WriteListOf(sig, listRef, b => b.WriteByte(ELEMENT_TYPE_STRING));
        }
        else
        {
            WriteListOf(sig, listRef, inner =>
                WriteListOf(inner, listRef, b => b.WriteByte(ELEMENT_TYPE_STRING)));
        }
        MethodDefinitionHandle method = mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Take"),
            mb.GetOrAddBlob(sig), bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        ParameterHandle param = mb.AddParameter(ParameterAttributes.None, mb.GetOrAddString("xs"), 1);

        byte[] payload = extra ? new byte[] { 1, 2, 1, 1 } : new byte[] { 1, 1 };
        mb.AddCustomAttribute(param, ctor, CaBlob(mb, b =>
        {
            b.WriteUInt32((uint)payload.Length);
            b.WriteBytes(payload);
            b.WriteUInt16(0);
        }));

        AddModuleType(mb);
        mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Interface | TypeAttributes.Abstract,
            default, mb.GetOrAddString("Host"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        return Finish(mb);
    }

    // `[NullableAttribute(new byte[]{0,0})]` on an `int**` parameter — a
    // *well-formed* nullable vector over a pointer position, the shape the reader
    // once mis-walked. Roslyn's pre-order flag walk visits each pointer node
    // (an oblivious `0`) then the pointee, so `int**` is two flags; the reader
    // must consume both (the inner `int` is a value type and adds none) and
    // project the member cleanly rather than refusing a spurious length
    // mismatch. Mirrors the `T*` / `T*[]` accessors in `System.Private.CoreLib`.
    private static BlobBuilder EmitNullablePointerVector()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "NullablePointerFixture", "vecptr");

        MemberReferenceHandle ctor = AddAttrCtor(
            mb, "System.Runtime.CompilerServices", "NullableAttribute", new[] { CtorU1Array });

        // `void Take(int** p)`.
        var sig = new BlobBuilder();
        sig.WriteByte(HASTHIS);
        sig.WriteByte(0x01); // 1 param
        sig.WriteByte(ELEMENT_TYPE_VOID); // return
        sig.WriteByte(ELEMENT_TYPE_PTR);
        sig.WriteByte(ELEMENT_TYPE_PTR);
        sig.WriteByte(ELEMENT_TYPE_I4);
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Take"),
            mb.GetOrAddBlob(sig), bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        ParameterHandle param = mb.AddParameter(ParameterAttributes.None, mb.GetOrAddString("p"), 1);

        byte[] payload = { 0, 0 }; // one oblivious flag per pointer node
        mb.AddCustomAttribute(param, ctor, CaBlob(mb, b =>
        {
            b.WriteUInt32((uint)payload.Length);
            b.WriteBytes(payload);
            b.WriteUInt16(0);
        }));

        AddModuleType(mb);
        mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Interface | TypeAttributes.Abstract,
            default, mb.GetOrAddString("Host"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        return Finish(mb);
    }

    // `[CompilerFeatureRequired((string)null)]` — a null feature name.
    private static BlobBuilder EmitCfrNullCtorArg() =>
        EmitAttrOnType("CfrNullFixture", "cfrnull",
            "System.Runtime.CompilerServices", "CompilerFeatureRequiredAttribute",
            new[] { CtorString },
            b =>
            {
                WriteSerString(b, null);
                b.WriteUInt16(0);
            });

    // `[CompilerFeatureRequired("RefStructs", "extra")]` — arity 2, not 1.
    private static BlobBuilder EmitCfrWrongArity() =>
        EmitAttrOnType("CfrArityFixture", "cfrarity",
            "System.Runtime.CompilerServices", "CompilerFeatureRequiredAttribute",
            new[] { CtorString, CtorString },
            b =>
            {
                WriteSerString(b, "RefStructs");
                WriteSerString(b, "extra");
                b.WriteUInt16(0);
            });

    // `[CompilerFeatureRequired("RefStructs", Bogus = true)]` — unknown named.
    private static BlobBuilder EmitCfrUnexpectedNamedArg() =>
        EmitAttrOnType("CfrNamedFixture", "cfrnamed",
            "System.Runtime.CompilerServices", "CompilerFeatureRequiredAttribute",
            new[] { CtorString },
            b =>
            {
                WriteSerString(b, "RefStructs");
                b.WriteUInt16(1);
                b.WriteByte(CA_PROPERTY);
                b.WriteByte(ELEMENT_TYPE_BOOLEAN);
                WriteSerString(b, "Bogus");
                b.WriteByte(1);
            });

    // `[CompilerFeatureRequired("RefStructs", IsOptional = "true")]` — the
    // documented `IsOptional` property carrying a string, not a bool.
    private static BlobBuilder EmitCfrNonBoolIsOptional() =>
        EmitAttrOnType("CfrIsOptFixture", "cfrisopt",
            "System.Runtime.CompilerServices", "CompilerFeatureRequiredAttribute",
            new[] { CtorString },
            b =>
            {
                WriteSerString(b, "RefStructs");
                b.WriteUInt16(1);
                b.WriteByte(CA_PROPERTY);
                b.WriteByte(ELEMENT_TYPE_STRING);
                WriteSerString(b, "IsOptional");
                WriteSerString(b, "true");
            });

    // `[DefaultMember("Item", Whatever = "nope")]` — named args on DefaultMember.
    private static BlobBuilder EmitDefaultMemberNamedArgs() =>
        EmitAttrOnType("DefMemberNamedFixture", "dmnamed",
            "System.Reflection", "DefaultMemberAttribute",
            new[] { CtorString },
            b =>
            {
                WriteSerString(b, "Item");
                b.WriteUInt16(1);
                b.WriteByte(CA_PROPERTY);
                b.WriteByte(ELEMENT_TYPE_STRING);
                WriteSerString(b, "Whatever");
                WriteSerString(b, "nope");
            });

    // `[DefaultMember((string)null)]` — a null member name.
    private static BlobBuilder EmitDefaultMemberNullCtorArg() =>
        EmitAttrOnType("DefMemberNullFixture", "dmnull",
            "System.Reflection", "DefaultMemberAttribute",
            new[] { CtorString },
            b =>
            {
                WriteSerString(b, null);
                b.WriteUInt16(0);
            });

    // ----- Property / event accessor shapes ----------------------------------
    //
    // Properties and events project through their accessors (linked via the
    // MethodSemantics table); the projector validates each accessor's
    // signature. Exotic accessor shapes no C#/F# compiler emits are refused.
    // The host is an interface so its abstract accessors need no IL body.

    private const MethodAttributes AccessorFlags =
        MethodAttributes.Public | MethodAttributes.HideBySig | MethodAttributes.NewSlot
            | MethodAttributes.Abstract | MethodAttributes.Virtual | MethodAttributes.SpecialName;

    // Add the interface `Host` (TypeDef row 2; <Module> is row 1) owning the
    // methods/properties/events added so far, and finish.
    private static BlobBuilder FinishWithHostInterface(MetadataBuilder mb, Action<TypeDefinitionHandle> map)
    {
        AddModuleType(mb);
        TypeDefinitionHandle host = mb.AddTypeDefinition(
            TypeAttributes.Public | TypeAttributes.Interface | TypeAttributes.Abstract,
            default,
            mb.GetOrAddString("Host"),
            baseType: default,
            fieldList: MetadataTokens.FieldDefinitionHandle(1),
            methodList: MetadataTokens.MethodDefinitionHandle(1));
        map(host);
        return Finish(mb);
    }

    // `int P { get; }` where `get_P` is a generic method — a generic accessor.
    private static BlobBuilder EmitPropertyGenericGetter()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "PropertyGenericGetterFixture", "propgen");

        // T get_P<T>() : GENERIC|HASTHIS, 1 generic param, 0 params, ret I4.
        var getterSig = new BlobBuilder();
        getterSig.WriteByte(CALLCONV_GENERIC | HASTHIS);
        getterSig.WriteCompressedInteger(1);
        getterSig.WriteByte(0x00);
        getterSig.WriteByte(ELEMENT_TYPE_I4);
        MethodDefinitionHandle getter = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("get_P"),
            mb.GetOrAddBlob(getterSig), bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddGenericParameter(getter, GenericParameterAttributes.None, mb.GetOrAddString("T"), 0);

        var propSig = new BlobBuilder();
        propSig.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSig.WriteByte(0x00); // 0 index params
        propSig.WriteByte(ELEMENT_TYPE_I4);
        PropertyDefinitionHandle prop = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), mb.GetOrAddBlob(propSig));

        return FinishWithHostInterface(mb, host =>
        {
            mb.AddPropertyMap(host, prop);
            mb.AddMethodSemantics(prop, MethodSemanticsAttributes.Getter, getter);
        });
    }

    // `int P { init; }` — a setter whose return type carries
    // `modreq(IsExternalInit)`, the C# `init` encoding. The projector recognises
    // this specific `modreq(IsExternalInit) void` shape and projects the setter
    // as a plain void return, so the property enumerates cleanly.
    private static BlobBuilder EmitPropertyInitOnlySetter()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "PropertyInitOnlyFixture", "propinit");

        TypeReferenceHandle isExternalInit = mb.AddTypeReference(
            AddMscorlib(mb),
            mb.GetOrAddString("System.Runtime.CompilerServices"),
            mb.GetOrAddString("IsExternalInit"));

        // void set_P(int) with modreq(IsExternalInit) on the (void) return.
        var setterSig = new BlobBuilder();
        setterSig.WriteByte(HASTHIS);
        setterSig.WriteByte(0x01); // 1 param
        setterSig.WriteByte(ELEMENT_TYPE_CMOD_REQD);
        WriteTypeToken(setterSig, isExternalInit);
        setterSig.WriteByte(ELEMENT_TYPE_VOID);
        setterSig.WriteByte(ELEMENT_TYPE_I4);
        MethodDefinitionHandle setter = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("set_P"),
            mb.GetOrAddBlob(setterSig), bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        var propSig = new BlobBuilder();
        propSig.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSig.WriteByte(0x00);
        propSig.WriteByte(ELEMENT_TYPE_I4);
        PropertyDefinitionHandle prop = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), mb.GetOrAddBlob(propSig));

        return FinishWithHostInterface(mb, host =>
        {
            mb.AddPropertyMap(host, prop);
            mb.AddMethodSemantics(prop, MethodSemanticsAttributes.Setter, setter);
        });
    }

    // `void M()` with `modreq(IsExternalInit)` on its *void* return, on a plain
    // method rather than a property setter. The `init` marker is only meaningful
    // (and only accepted by the projector) on a set accessor; on any other
    // method it is a signature-significant modifier the model can't carry, so
    // the projector must refuse the member rather than flatten it to plain
    // `void`. No real compiler emits this — it takes the raw emitter.
    // `modopt(IsConst) void M()` — an *optional* modifier before a `void` return.
    // The `init` marker (`modreq(IsExternalInit)`) is accepted only on a property
    // setter, but that is a rule about *required* modifiers: an ignorable `modopt`
    // before `void` leaves an ordinary `void` return on any method (II.7.1.1), so
    // this projects cleanly.
    private static BlobBuilder EmitMethodModoptVoid() =>
        EmitAbstractMethod("MethodModoptVoidFixture", "methodmodopt", "M", AbstractPublic, mb =>
        {
            TypeReferenceHandle isConst = AddIsConst(mb);
            var sig = new BlobBuilder();
            sig.WriteByte(HASTHIS);
            sig.WriteByte(0x00); // 0 params
            sig.WriteByte(ELEMENT_TYPE_CMOD_OPT);
            WriteTypeToken(sig, isConst);
            sig.WriteByte(ELEMENT_TYPE_VOID);
            return mb.GetOrAddBlob(sig);
        });

    private static BlobBuilder EmitMethodInitMarkerVoid() =>
        EmitAbstractMethod("MethodInitMarkerFixture", "methodinit", "M", AbstractPublic, mb =>
        {
            TypeReferenceHandle isExternalInit = mb.AddTypeReference(
                AddMscorlib(mb),
                mb.GetOrAddString("System.Runtime.CompilerServices"),
                mb.GetOrAddString("IsExternalInit"));
            var sig = new BlobBuilder();
            sig.WriteByte(HASTHIS);
            sig.WriteByte(0x00); // 0 params
            sig.WriteByte(ELEMENT_TYPE_CMOD_REQD);
            WriteTypeToken(sig, isExternalInit);
            sig.WriteByte(ELEMENT_TYPE_VOID);
            return mb.GetOrAddBlob(sig);
        });

    // A `void add_X(EventHandler)` / `remove_X(EventHandler)` accessor sig.
    private static BlobHandle EventAccessorSig(MetadataBuilder mb, TypeReferenceHandle handler, bool instance)
    {
        var sig = new BlobBuilder();
        sig.WriteByte(instance ? HASTHIS : (byte)0x00);
        sig.WriteByte(0x01); // 1 param
        sig.WriteByte(ELEMENT_TYPE_VOID);
        sig.WriteByte(ELEMENT_TYPE_CLASS);
        WriteTypeToken(sig, handler);
        return mb.GetOrAddBlob(sig);
    }

    private static MethodDefinitionHandle AddEventAccessor(
        MetadataBuilder mb, string name, BlobHandle sig, bool isStatic) =>
        mb.AddMethodDefinition(
            isStatic ? AccessorFlags | MethodAttributes.Static : AccessorFlags,
            MethodImplAttributes.IL, mb.GetOrAddString(name), sig,
            bodyOffset: -1, MetadataTokens.ParameterHandle(1));

    private static TypeReferenceHandle AddEventHandler(MetadataBuilder mb) =>
        mb.AddTypeReference(AddMscorlib(mb), mb.GetOrAddString("System"), mb.GetOrAddString("EventHandler"));

    // An event `Tick` with an extra OtherMethods accessor — the open-ended slot
    // the v1 model can't carry.
    private static BlobBuilder EmitEventOtherAccessor()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "EventOtherFixture", "evtother");
        TypeReferenceHandle handler = AddEventHandler(mb);

        BlobHandle sig = EventAccessorSig(mb, handler, instance: true);
        MethodDefinitionHandle add = AddEventAccessor(mb, "add_Tick", sig, false);
        MethodDefinitionHandle remove = AddEventAccessor(mb, "remove_Tick", sig, false);
        MethodDefinitionHandle other = AddEventAccessor(mb, "other_Tick", sig, false);

        EventDefinitionHandle ev = mb.AddEvent(
            EventAttributes.None, mb.GetOrAddString("Tick"), handler);

        return FinishWithHostInterface(mb, host =>
        {
            mb.AddEventMap(host, ev);
            mb.AddMethodSemantics(ev, MethodSemanticsAttributes.Adder, add);
            mb.AddMethodSemantics(ev, MethodSemanticsAttributes.Remover, remove);
            mb.AddMethodSemantics(ev, MethodSemanticsAttributes.Other, other);
        });
    }

    // An event whose add accessor is static but remove accessor is instance.
    private static BlobBuilder EmitEventDisagreeingStatic()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "EventStaticFixture", "evtstatic");
        TypeReferenceHandle handler = AddEventHandler(mb);

        MethodDefinitionHandle add = AddEventAccessor(
            mb, "add_Tick", EventAccessorSig(mb, handler, instance: false), isStatic: true);
        MethodDefinitionHandle remove = AddEventAccessor(
            mb, "remove_Tick", EventAccessorSig(mb, handler, instance: true), isStatic: false);

        EventDefinitionHandle ev = mb.AddEvent(
            EventAttributes.None, mb.GetOrAddString("Tick"), handler);

        return FinishWithHostInterface(mb, host =>
        {
            mb.AddEventMap(host, ev);
            mb.AddMethodSemantics(ev, MethodSemanticsAttributes.Adder, add);
            mb.AddMethodSemantics(ev, MethodSemanticsAttributes.Remover, remove);
        });
    }

    // An event whose add accessor's (void) return carries modreq(IsConst).
    private static BlobBuilder EmitEventModreqAccessor()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "EventModreqFixture", "evtmodreq");
        TypeReferenceHandle handler = AddEventHandler(mb);
        TypeReferenceHandle isConst = mb.AddTypeReference(
            AddMscorlib(mb),
            mb.GetOrAddString("System.Runtime.CompilerServices"),
            mb.GetOrAddString("IsConst"));

        var addSig = new BlobBuilder();
        addSig.WriteByte(HASTHIS);
        addSig.WriteByte(0x01);
        addSig.WriteByte(ELEMENT_TYPE_CMOD_REQD);
        WriteTypeToken(addSig, isConst);
        addSig.WriteByte(ELEMENT_TYPE_VOID);
        addSig.WriteByte(ELEMENT_TYPE_CLASS);
        WriteTypeToken(addSig, handler);
        MethodDefinitionHandle add = AddEventAccessor(mb, "add_Tick", mb.GetOrAddBlob(addSig), false);
        MethodDefinitionHandle remove = AddEventAccessor(
            mb, "remove_Tick", EventAccessorSig(mb, handler, instance: true), false);

        EventDefinitionHandle ev = mb.AddEvent(
            EventAttributes.None, mb.GetOrAddString("Tick"), handler);

        return FinishWithHostInterface(mb, host =>
        {
            mb.AddEventMap(host, ev);
            mb.AddMethodSemantics(ev, MethodSemanticsAttributes.Adder, add);
            mb.AddMethodSemantics(ev, MethodSemanticsAttributes.Remover, remove);
        });
    }

    // An event whose add accessor is a generic method.
    private static BlobBuilder EmitEventGenericAccessor()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "EventGenericFixture", "evtgen");
        TypeReferenceHandle handler = AddEventHandler(mb);

        // void add_Tick<T>(EventHandler) : GENERIC|HASTHIS, 1 gen param, 1 param.
        var addSig = new BlobBuilder();
        addSig.WriteByte(CALLCONV_GENERIC | HASTHIS);
        addSig.WriteCompressedInteger(1);
        addSig.WriteByte(0x01);
        addSig.WriteByte(ELEMENT_TYPE_VOID);
        addSig.WriteByte(ELEMENT_TYPE_CLASS);
        WriteTypeToken(addSig, handler);
        MethodDefinitionHandle add = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("add_Tick"),
            mb.GetOrAddBlob(addSig), bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddGenericParameter(add, GenericParameterAttributes.None, mb.GetOrAddString("T"), 0);
        MethodDefinitionHandle remove = AddEventAccessor(
            mb, "remove_Tick", EventAccessorSig(mb, handler, instance: true), false);

        EventDefinitionHandle ev = mb.AddEvent(
            EventAttributes.None, mb.GetOrAddString("Tick"), handler);

        return FinishWithHostInterface(mb, host =>
        {
            mb.AddEventMap(host, ev);
            mb.AddMethodSemantics(ev, MethodSemanticsAttributes.Adder, add);
            mb.AddMethodSemantics(ev, MethodSemanticsAttributes.Remover, remove);
        });
    }

    // ----- MethodImpl classification shapes -----------------------------------
    //
    // Valid IL exercising `MethodImpl` (ECMA-335 II.22.27) rows that C#/F#
    // never emit. The CLR does not require an explicit interface
    // implementation's body method to carry the interface-qualified
    // (`IFace.Member`) name — that is a compiler convention (VB, for one,
    // freely emits plain names via `Implements`) — so the reader must classify
    // a row by its *declaration* target, never by the body's name shape.

    // `void ()` instance method signature: HASTHIS, 0 params, void return.
    private static BlobHandle InstanceVoidSig(MetadataBuilder mb)
    {
        var sig = new BlobBuilder();
        sig.WriteByte(HASTHIS);
        sig.WriteByte(0x00);
        sig.WriteByte(ELEMENT_TYPE_VOID);
        return mb.GetOrAddBlob(sig);
    }

    private static readonly TypeAttributes PublicInterface =
        TypeAttributes.Public | TypeAttributes.Interface | TypeAttributes.Abstract;

    private static readonly TypeAttributes PublicAbstractClass =
        TypeAttributes.Public | TypeAttributes.Class | TypeAttributes.Abstract;

    // An abstract class `Widget : IFoo` whose MethodImpl maps a body method
    // with the *plain* name `Impl` onto `IFoo::M`. Roslyn would name the body
    // `IFoo.M`; the plain name pins that classification keys off the
    // declaration (an in-module interface TypeDef), not the name.
    private static BlobBuilder EmitMethodImplUnmangledBody()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplUnmangledFixture", "miunmang");

        BlobHandle voidSig = InstanceVoidSig(mb);
        // Method RID 1: IFoo::M. RID 2: Widget::Impl.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Impl"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle ifoo = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IFoo"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle widget = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        mb.AddInterfaceImplementation(widget, ifoo);
        mb.AddMethodImplementation(
            widget,
            MetadataTokens.MethodDefinitionHandle(2),
            MetadataTokens.MethodDefinitionHandle(1));
        return Finish(mb);
    }

    // One body method satisfying *two* interface members: `Widget : IFoo, IBar`
    // with a single plain-named `Impl` and two MethodImpl rows sharing it as
    // their body (VB's `Implements IFoo.M, IBar.M`). Both declarations must be
    // surfaced — the model is a list, not a single slot the second row
    // overwrites.
    private static BlobBuilder EmitMethodImplMultiDecl()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplMultiDeclFixture", "mimulti");

        BlobHandle voidSig = InstanceVoidSig(mb);
        // Method RID 1: IFoo::M. RID 2: IBar::M. RID 3: Widget::Impl.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Impl"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle ifoo = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IFoo"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle ibar = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IBar"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        TypeDefinitionHandle widget = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(3));
        mb.AddInterfaceImplementation(widget, ifoo);
        mb.AddInterfaceImplementation(widget, ibar);
        mb.AddMethodImplementation(
            widget,
            MetadataTokens.MethodDefinitionHandle(3),
            MetadataTokens.MethodDefinitionHandle(1));
        mb.AddMethodImplementation(
            widget,
            MetadataTokens.MethodDefinitionHandle(3),
            MetadataTokens.MethodDefinitionHandle(2));
        return Finish(mb);
    }

    // An explicit implementation of an *external* interface
    // (`System.IDisposable`, a TypeRef) through a plain-named body. The
    // declaration parent's interface-ness cannot be read from flags in this
    // module; the classifier must recognise it via the implementing type's
    // InterfaceImpl row instead.
    private static BlobBuilder EmitMethodImplExternalIfaceUnmangled()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplExternalIfaceFixture", "miextif");

        TypeReferenceHandle idisposable = mb.AddTypeReference(
            AddMscorlib(mb), mb.GetOrAddString("System"), mb.GetOrAddString("IDisposable"));
        BlobHandle voidSig = InstanceVoidSig(mb);
        MemberReferenceHandle dispose = mb.AddMemberReference(
            idisposable, mb.GetOrAddString("Dispose"), voidSig);
        // Method RID 1: Widget::DoDispose.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("DoDispose"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle widget = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        mb.AddInterfaceImplementation(widget, idisposable);
        mb.AddMethodImplementation(widget, MetadataTokens.MethodDefinitionHandle(1), dispose);
        return Finish(mb);
    }

    // Like `methodimpl_external_iface_unmangled`, but the InterfaceImpl row and
    // the MethodImpl declaration reach `System.IDisposable` through two
    // *duplicate* TypeRef rows (same scope, namespace, and name — ECMA-335
    // permits this and IL weavers produce it). The membership check must
    // compare the referenced type's identity, not TypeRef row identity.
    private static BlobBuilder EmitMethodImplDupTypeRef()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplDupTypeRefFixture", "midupref");

        AssemblyReferenceHandle mscorlib = AddMscorlib(mb);
        TypeReferenceHandle idisposableA = mb.AddTypeReference(
            mscorlib, mb.GetOrAddString("System"), mb.GetOrAddString("IDisposable"));
        TypeReferenceHandle idisposableB = mb.AddTypeReference(
            mscorlib, mb.GetOrAddString("System"), mb.GetOrAddString("IDisposable"));
        BlobHandle voidSig = InstanceVoidSig(mb);
        MemberReferenceHandle dispose = mb.AddMemberReference(
            idisposableB, mb.GetOrAddString("Dispose"), voidSig);
        // Method RID 1: Widget::DoDispose.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("DoDispose"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle widget = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        mb.AddInterfaceImplementation(widget, idisposableA);
        mb.AddMethodImplementation(widget, MetadataTokens.MethodDefinitionHandle(1), dispose);
        return Finish(mb);
    }

    // A class property whose *getter* and *setter* satisfy properties of two
    // different interfaces (VB's `Property P … Implements IRead.P, IWrite.P`
    // with a get-only `IRead.P` and a set-only `IWrite.P`), plus a second
    // property implementing one get+set interface property through both
    // accessors (the shape whose two rows must dedup to one entry). The
    // projection must union the accessor's MethodImpls, not prefer the getter.
    private static BlobBuilder EmitMethodImplSplitProperty()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplSplitPropertyFixture", "misplitp");

        // Signatures: `int get()` / `void set(int)` accessors, `int` property.
        var getSigB = new BlobBuilder();
        getSigB.WriteByte(HASTHIS);
        getSigB.WriteByte(0x00);
        getSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle getSig = mb.GetOrAddBlob(getSigB);
        var setSigB = new BlobBuilder();
        setSigB.WriteByte(HASTHIS);
        setSigB.WriteByte(0x01);
        setSigB.WriteByte(ELEMENT_TYPE_VOID);
        setSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle setSig = mb.GetOrAddBlob(setSigB);
        var propSigB = new BlobBuilder();
        propSigB.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSigB.WriteByte(0x00);
        propSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle propSig = mb.GetOrAddBlob(propSigB);

        MethodDefinitionHandle Accessor(string name, BlobHandle sig) =>
            mb.AddMethodDefinition(
                AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString(name),
                sig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        // Methods, RID order: IRead::get_P (1), IWrite::set_P (2),
        // IBoth::get_B (3), IBoth::set_B (4), then C's bodies (5..8).
        MethodDefinitionHandle iReadGet = Accessor("get_P", getSig);
        MethodDefinitionHandle iWriteSet = Accessor("set_P", setSig);
        MethodDefinitionHandle iBothGet = Accessor("get_B", getSig);
        MethodDefinitionHandle iBothSet = Accessor("set_B", setSig);
        MethodDefinitionHandle cGetP = Accessor("get_P", getSig);
        MethodDefinitionHandle cSetP = Accessor("set_P", setSig);
        MethodDefinitionHandle cGetB = Accessor("get_B", getSig);
        MethodDefinitionHandle cSetB = Accessor("set_B", setSig);

        // TypeDefs: <Module> (1), IRead (2), IWrite (3), IBoth (4), C (5).
        AddModuleType(mb);
        TypeDefinitionHandle iRead = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IRead"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle iWrite = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IWrite"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        TypeDefinitionHandle iBoth = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IBoth"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(3));
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(5));

        mb.AddInterfaceImplementation(c, iRead);
        mb.AddInterfaceImplementation(c, iWrite);
        mb.AddInterfaceImplementation(c, iBoth);

        // Properties (RID order) + per-type PropertyMap (sorted by Parent) +
        // MethodSemantics (sorted by Association, i.e. property RID order).
        PropertyDefinitionHandle Prop(string name) =>
            mb.AddProperty(PropertyAttributes.None, mb.GetOrAddString(name), propSig);
        PropertyDefinitionHandle iReadP = Prop("P");
        PropertyDefinitionHandle iWriteP = Prop("P");
        PropertyDefinitionHandle iBothB = Prop("B");
        PropertyDefinitionHandle cP = Prop("P");
        PropertyDefinitionHandle cB = Prop("B");
        mb.AddPropertyMap(iRead, iReadP);
        mb.AddPropertyMap(iWrite, iWriteP);
        mb.AddPropertyMap(iBoth, iBothB);
        mb.AddPropertyMap(c, cP);
        mb.AddMethodSemantics(iReadP, MethodSemanticsAttributes.Getter, iReadGet);
        mb.AddMethodSemantics(iWriteP, MethodSemanticsAttributes.Setter, iWriteSet);
        mb.AddMethodSemantics(iBothB, MethodSemanticsAttributes.Getter, iBothGet);
        mb.AddMethodSemantics(iBothB, MethodSemanticsAttributes.Setter, iBothSet);
        mb.AddMethodSemantics(cP, MethodSemanticsAttributes.Getter, cGetP);
        mb.AddMethodSemantics(cP, MethodSemanticsAttributes.Setter, cSetP);
        mb.AddMethodSemantics(cB, MethodSemanticsAttributes.Getter, cGetB);
        mb.AddMethodSemantics(cB, MethodSemanticsAttributes.Setter, cSetB);

        // MethodImpl rows (all on C): the split property's accessors each
        // satisfy a different interface; B's satisfy one interface twice.
        mb.AddMethodImplementation(c, cGetP, iReadGet);
        mb.AddMethodImplementation(c, cSetP, iWriteSet);
        mb.AddMethodImplementation(c, cGetB, iBothGet);
        mb.AddMethodImplementation(c, cSetB, iBothSet);
        return Finish(mb);
    }

    // The implementing type's *direct* InterfaceImpl rows list only the
    // in-module `IMid`, whose own InterfaceImpl rows list the external
    // `System.IDisposable`; the MethodImpl declaration targets IDisposable.
    // The CLR places declarations against the full interface map (the
    // transitive closure), and Roslyn's habit of flattening the closure into
    // the class's direct rows is a convention — the classifier must expand
    // through in-module interface edges.
    private static BlobBuilder EmitMethodImplIfaceViaInterface()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplIfaceViaIfaceFixture", "miviaif");

        TypeReferenceHandle idisposable = mb.AddTypeReference(
            AddMscorlib(mb), mb.GetOrAddString("System"), mb.GetOrAddString("IDisposable"));
        BlobHandle voidSig = InstanceVoidSig(mb);
        MemberReferenceHandle dispose = mb.AddMemberReference(
            idisposable, mb.GetOrAddString("Dispose"), voidSig);
        // Method RID 1: Widget::DoDispose.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("DoDispose"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle imid = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IMid"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle widget = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        // Sorted by Class: IMid (TypeDef row 2), then Widget (row 3).
        mb.AddInterfaceImplementation(imid, idisposable);
        mb.AddInterfaceImplementation(widget, imid);
        mb.AddMethodImplementation(widget, MetadataTokens.MethodDefinitionHandle(1), dispose);
        return Finish(mb);
    }

    // The implementing type has *no* InterfaceImpl rows of its own: the
    // external `System.IDisposable` is implemented by its in-module base class
    // `BaseW`, and the MethodImpl declaration targets IDisposable. The CLR's
    // interface map includes base-class interfaces, so the row is loadable —
    // the classifier must expand through the in-module `Extends` chain.
    private static BlobBuilder EmitMethodImplIfaceViaBase()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplIfaceViaBaseFixture", "miviabase");

        TypeReferenceHandle idisposable = mb.AddTypeReference(
            AddMscorlib(mb), mb.GetOrAddString("System"), mb.GetOrAddString("IDisposable"));
        BlobHandle voidSig = InstanceVoidSig(mb);
        MemberReferenceHandle dispose = mb.AddMemberReference(
            idisposable, mb.GetOrAddString("Dispose"), voidSig);
        // Method RID 1: Widget::DoDispose.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("DoDispose"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle baseW = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("BaseW"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle widget = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: baseW,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        mb.AddInterfaceImplementation(baseW, idisposable);
        mb.AddMethodImplementation(widget, MetadataTokens.MethodDefinitionHandle(1), dispose);
        return Finish(mb);
    }

    // Three in-module `IProp` members exercising every direction in which an
    // accessor's *name* misleads: only MethodSemantics says what is an
    // accessor, and of what.
    //   * `IProp::P`'s getter is named `Read`, not the CLS-conventional
    //     `get_P`: the interface member name must come from the declaration's
    //     owning property through MethodSemantics (`P`), not from
    //     prefix-stripping the accessor name.
    //   * `IProp::get_Value` is a *property* whose own name starts with `get_`:
    //     the MethodSemantics-resolved name must be kept verbatim — stripping
    //     the conventional prefix off an already-authoritative name would
    //     corrupt it to `Value`.
    //   * `IProp::get_Q` is an ordinary *method* whose name merely looks like
    //     an accessor's; no MethodSemantics row claims it. A class property's
    //     getter may implement it, and the implemented member is then the
    //     method `get_Q`, not a property `Q` that does not exist.
    // Class `C` implements all three, each from one of its property getters.
    private static BlobBuilder EmitMethodImplUnconventionalAccessor()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplOddAccessorFixture", "mioddacc");

        var getSigB = new BlobBuilder();
        getSigB.WriteByte(HASTHIS);
        getSigB.WriteByte(0x00);
        getSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle getSig = mb.GetOrAddBlob(getSigB);
        var propSigB = new BlobBuilder();
        propSigB.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSigB.WriteByte(0x00);
        propSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle propSig = mb.GetOrAddBlob(propSigB);

        // Method RID 1: IProp::Read (the unconventionally-named getter of P).
        // RID 2: IProp::gv (getter of the property literally named `get_Value`).
        // RID 3: IProp::get_Q (an ordinary method, no MethodSemantics row —
        // hence no SpecialName). RID 4: C::get_P. RID 5: C::get_get_Value.
        // RID 6: C::Fetch (getter of C's property Q).
        MethodDefinitionHandle read = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("Read"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle gv = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("gv"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle iGetQ = mb.AddMethodDefinition(
            AccessorFlags & ~MethodAttributes.SpecialName, MethodImplAttributes.IL,
            mb.GetOrAddString("get_Q"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle cGetP = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("get_P"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle cGetGv = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("get_get_Value"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle cFetch = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("Fetch"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle iprop = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IProp"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(4));
        mb.AddInterfaceImplementation(c, iprop);

        PropertyDefinitionHandle ipropP = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), propSig);
        PropertyDefinitionHandle ipropGetValue = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("get_Value"), propSig);
        PropertyDefinitionHandle cP = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), propSig);
        PropertyDefinitionHandle cGetValue = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("get_Value"), propSig);
        PropertyDefinitionHandle cQ = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("Q"), propSig);
        mb.AddPropertyMap(iprop, ipropP);
        mb.AddPropertyMap(c, cP);
        mb.AddMethodSemantics(ipropP, MethodSemanticsAttributes.Getter, read);
        mb.AddMethodSemantics(ipropGetValue, MethodSemanticsAttributes.Getter, gv);
        mb.AddMethodSemantics(cP, MethodSemanticsAttributes.Getter, cGetP);
        mb.AddMethodSemantics(cGetValue, MethodSemanticsAttributes.Getter, cGetGv);
        mb.AddMethodSemantics(cQ, MethodSemanticsAttributes.Getter, cFetch);

        mb.AddMethodImplementation(c, cGetP, read);
        mb.AddMethodImplementation(c, cGetGv, gv);
        mb.AddMethodImplementation(c, cFetch, iGetQ);
        return Finish(mb);
    }

    // An explicit impl of an interface that is *generic and in this module*.
    // Because the declaration parent is an instantiation, it must be spelled as
    // a MemberRef over a TypeSpec (`IGen`1<int32>::Read`) rather than a
    // MethodDef token — this is the shape every compiler emits for an explicit
    // impl of a same-assembly generic interface. The declaration's
    // MethodSemantics is nonetheless right here in this module, so the
    // implemented member must still resolve to the owning property `P`, not to
    // a convention-stripped mangling of the getter's name `Read`.
    private static BlobBuilder EmitMethodImplLocalGenericIfaceMemberRef()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplLocalGenericIfaceFixture", "migenmr");

        var getSigB = new BlobBuilder();
        getSigB.WriteByte(HASTHIS);
        getSigB.WriteByte(0x00);
        getSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle getSig = mb.GetOrAddBlob(getSigB);
        var propSigB = new BlobBuilder();
        propSigB.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSigB.WriteByte(0x00);
        propSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle propSig = mb.GetOrAddBlob(propSigB);

        // Method RID 1: IGen`1::Read (the unconventionally-named getter of P).
        // RID 2: C::get_P (getter of C's property P).
        MethodDefinitionHandle read = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("Read"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle cGetP = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("get_P"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle igen = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IGen`1"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        mb.AddGenericParameter(igen, GenericParameterAttributes.None, mb.GetOrAddString("T"), 0);

        // TypeSpec: GENERICINST CLASS IGen`1 <1> int32.
        var specB = new BlobBuilder();
        specB.WriteByte(ELEMENT_TYPE_GENERICINST);
        specB.WriteByte(ELEMENT_TYPE_CLASS);
        specB.WriteCompressedInteger(CodedIndex.TypeDefOrRefOrSpec(igen));
        specB.WriteCompressedInteger(1);
        specB.WriteByte(ELEMENT_TYPE_I4);
        TypeSpecificationHandle spec = mb.AddTypeSpecification(mb.GetOrAddBlob(specB));
        mb.AddInterfaceImplementation(c, spec);

        // The MemberRef's signature is the one from the *definition* (II.22.25),
        // so it is byte-identical to `IGen`1::Read`'s MethodDef signature.
        MemberReferenceHandle readRef = mb.AddMemberReference(
            spec, mb.GetOrAddString("Read"), getSig);

        PropertyDefinitionHandle igenP = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), propSig);
        PropertyDefinitionHandle cP = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), propSig);
        mb.AddPropertyMap(igen, igenP);
        mb.AddPropertyMap(c, cP);
        mb.AddMethodSemantics(igenP, MethodSemanticsAttributes.Getter, read);
        mb.AddMethodSemantics(cP, MethodSemanticsAttributes.Getter, cGetP);

        mb.AddMethodImplementation(c, cGetP, readRef);
        return Finish(mb);
    }

    // A MethodImpl whose declaration parent is an external *class*
    // (`System.Object::ToString`) — the external analogue of a C#
    // covariant-return override. The parent is not in the implementing type's
    // InterfaceImpl rows, so it must not be classified as an explicit
    // interface implementation.
    private static BlobBuilder EmitMethodImplExternalClassDecl()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplExternalClassFixture", "miextcls");

        TypeReferenceHandle systemObject = mb.AddTypeReference(
            AddMscorlib(mb), mb.GetOrAddString("System"), mb.GetOrAddString("Object"));
        var stringSig = new BlobBuilder();
        stringSig.WriteByte(HASTHIS);
        stringSig.WriteByte(0x00);
        stringSig.WriteByte(ELEMENT_TYPE_STRING);
        BlobHandle toStringSig = mb.GetOrAddBlob(stringSig);
        MemberReferenceHandle decl = mb.AddMemberReference(
            systemObject, mb.GetOrAddString("ToString"), toStringSig);
        // Method RID 1: Widget::ToString (an override: virtual, no newslot).
        mb.AddMethodDefinition(
            MethodAttributes.Public | MethodAttributes.HideBySig
                | MethodAttributes.Abstract | MethodAttributes.Virtual,
            MethodImplAttributes.IL, mb.GetOrAddString("ToString"),
            toStringSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle widget = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: systemObject,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        mb.AddMethodImplementation(widget, MetadataTokens.MethodDefinitionHandle(1), decl);
        return Finish(mb);
    }

    // An explicit impl whose declaration is a MemberRef into *another
    // assembly* (`[mscorlib]System.IExt::get_Q`): the referenced interface's
    // MethodSemantics is not locally readable, so whether `get_Q` is a
    // property accessor or an ordinary method that merely looks like one is
    // unknowable from this module. The implementing body is `C`'s property
    // getter, so the entry lands on property `P` — but its member must stay
    // the verbatim unresolved `get_Q`, never a prefix-stripped guess `Q`
    // (which would fabricate an interface property that may not exist).
    private static BlobBuilder EmitMethodImplExternalAccessorDecl()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplExternalAccessorFixture", "miextacc");

        var getSigB = new BlobBuilder();
        getSigB.WriteByte(HASTHIS);
        getSigB.WriteByte(0x00);
        getSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle getSig = mb.GetOrAddBlob(getSigB);
        var propSigB = new BlobBuilder();
        propSigB.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSigB.WriteByte(0x00);
        propSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle propSig = mb.GetOrAddBlob(propSigB);

        TypeReferenceHandle iext = mb.AddTypeReference(
            AddMscorlib(mb), mb.GetOrAddString("System"), mb.GetOrAddString("IExt"));
        MemberReferenceHandle iextGetQ = mb.AddMemberReference(
            iext, mb.GetOrAddString("get_Q"), getSig);

        // Method RID 1: C::get_P (getter of C's property P).
        MethodDefinitionHandle cGetP = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("get_P"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        mb.AddInterfaceImplementation(c, iext);

        PropertyDefinitionHandle cP = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), propSig);
        mb.AddPropertyMap(c, cP);
        mb.AddMethodSemantics(cP, MethodSemanticsAttributes.Getter, cGetP);

        mb.AddMethodImplementation(c, cGetP, iextGetQ);
        return Finish(mb);
    }

    // A *malformed* row: `Class` names `Other` but the body method lives on
    // `Widget` (II.22.27 requires the body to be a method of `Class`). The body
    // even carries a Roslyn-style mangled name, so a name-keyed reader would
    // stamp `Widget::IFoo.M` — attributing the impl to a type the row does not
    // name. The row must be skipped.
    private static BlobBuilder EmitMethodImplClassMismatch()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplMismatchFixture", "mismatch");

        BlobHandle voidSig = InstanceVoidSig(mb);
        // Method RID 1: IFoo::M. RID 2: Widget::"IFoo.M".
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("IFoo.M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle ifoo = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IFoo"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        TypeDefinitionHandle other = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Other"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(3));
        mb.AddInterfaceImplementation(other, ifoo);
        mb.AddMethodImplementation(
            other,
            MetadataTokens.MethodDefinitionHandle(2),
            MetadataTokens.MethodDefinitionHandle(1));
        return Finish(mb);
    }

    // A MethodImpl whose declaration parent is a local interface `IBar` that
    // `Widget` does *not* implement (its InterfaceImpl row lists only `IFoo`).
    // ECMA-335 §II.22.27 requires the declaration to be on `Class`'s ancestor
    // chain or interface tree, and the CLR resolves declarations against the
    // computed interface map, so this row cannot load — publishing it would
    // claim an implementation relationship the type does not have. The
    // interface-ness of `IBar` (its flag) is not enough; membership must be
    // checked for local declaration parents just as it is for external ones.
    private static BlobBuilder EmitMethodImplUnrelatedLocalIface()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplUnrelatedIfaceFixture", "miunrel");

        BlobHandle voidSig = InstanceVoidSig(mb);
        // Method RID 1: IFoo::M. RID 2: IBar::M. RID 3: Widget::Impl.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Impl"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle ifoo = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IFoo"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IBar"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        TypeDefinitionHandle widget = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(3));
        mb.AddInterfaceImplementation(widget, ifoo);
        mb.AddMethodImplementation(
            widget,
            MetadataTokens.MethodDefinitionHandle(3),
            MetadataTokens.MethodDefinitionHandle(2));
        return Finish(mb);
    }

    // The shape F# and VB emit for a member of an *inherited external*
    // interface implemented through the derived interface's clause (verified
    // against both compilers: `interface IDerived with member _.M()` in F#,
    // `Implements IDerived` + `Sub Body() Implements IBase.M` in VB, where
    // external `IDerived : IBase`): InterfaceImpl lists only `IDerived`,
    // while the MethodImpl declaration targets `IBase::M`. Neither the
    // interface-ness of `IBase` nor its membership in C's interface map is
    // provable from this image — the identical in-image shape is also what a
    // C# covariant-return override targeting a *non-direct* external
    // ancestor produces (verified: Roslyn points the declaration at the
    // original declarer, not the direct base) — so the row must surface as
    // *unclassified*, never dropped and never published as `implements`.
    // `C.Body` exercises the method path; `C.P`'s getter (implementing
    // `IBase::get_Q`) exercises the accessor-union path onto the property.
    private static BlobBuilder EmitMethodImplExternalInheritedIface()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplInheritedIfaceFixture", "miinhif");

        AssemblyReferenceHandle mscorlib = AddMscorlib(mb);
        TypeReferenceHandle iderived = mb.AddTypeReference(
            mscorlib, mb.GetOrAddString("Ext"), mb.GetOrAddString("IDerived"));
        TypeReferenceHandle ibase = mb.AddTypeReference(
            mscorlib, mb.GetOrAddString("Ext"), mb.GetOrAddString("IBase"));

        BlobHandle voidSig = InstanceVoidSig(mb);
        var getSigB = new BlobBuilder();
        getSigB.WriteByte(HASTHIS);
        getSigB.WriteByte(0x00);
        getSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle getSig = mb.GetOrAddBlob(getSigB);
        var propSigB = new BlobBuilder();
        propSigB.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSigB.WriteByte(0x00);
        propSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle propSig = mb.GetOrAddBlob(propSigB);

        MemberReferenceHandle ibaseM = mb.AddMemberReference(
            ibase, mb.GetOrAddString("M"), voidSig);
        MemberReferenceHandle ibaseGetQ = mb.AddMemberReference(
            ibase, mb.GetOrAddString("get_Q"), getSig);

        // Method RID 1: C::Body. RID 2: C::get_P.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Body"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle cGetP = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("get_P"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        mb.AddInterfaceImplementation(c, iderived);

        PropertyDefinitionHandle cP = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), propSig);
        mb.AddPropertyMap(c, cP);
        mb.AddMethodSemantics(cP, MethodSemanticsAttributes.Getter, cGetP);

        mb.AddMethodImplementation(c, MetadataTokens.MethodDefinitionHandle(1), ibaseM);
        mb.AddMethodImplementation(c, cGetP, ibaseGetQ);
        return Finish(mb);
    }

    // The generic same-module analogue of the inherited-external shape,
    // mirrored from real F# output (`interface IDerived<int> with member
    // _.M()` where `IDerived<'T> :> IBase<'T>` is same-module): InterfaceImpl
    // lists only `IDerived<int32>` on `C`, `IDerived`1`'s own row lists
    // `IBase<!0>` (the *definition's* type parameter), and the MethodImpl
    // declaration targets the *constructed* `IBase<int32>`. The closure walk
    // must substitute `IDerived`1`'s instantiation through its interface rows
    // — traversing the bare definition would contribute `IBase<!0>`, mismatch
    // the constructed declaration, and drop a real F# implementation.
    private static BlobBuilder EmitMethodImplGenericInheritedLocalIface()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplGenericInheritedFixture", "migeninh");

        BlobHandle voidSig = InstanceVoidSig(mb);
        // Method RID 1: IBase`1::M. RID 2: C::Impl.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Impl"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle ibase = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IBase`1"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle iderived = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IDerived`1"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        mb.AddGenericParameter(ibase, GenericParameterAttributes.None, mb.GetOrAddString("T"), 0);
        mb.AddGenericParameter(iderived, GenericParameterAttributes.None, mb.GetOrAddString("T"), 0);

        // TypeSpec: IBase`1<!0> — IDerived`1's own interface row, spelled with
        // IDerived`1's type parameter.
        var baseOfDerivedB = new BlobBuilder();
        baseOfDerivedB.WriteByte(ELEMENT_TYPE_GENERICINST);
        baseOfDerivedB.WriteByte(ELEMENT_TYPE_CLASS);
        baseOfDerivedB.WriteCompressedInteger(CodedIndex.TypeDefOrRefOrSpec(ibase));
        baseOfDerivedB.WriteCompressedInteger(1);
        baseOfDerivedB.WriteByte(ELEMENT_TYPE_VAR);
        baseOfDerivedB.WriteCompressedInteger(0);
        TypeSpecificationHandle baseOfDerived =
            mb.AddTypeSpecification(mb.GetOrAddBlob(baseOfDerivedB));

        // TypeSpec: IDerived`1<int32> — C's only InterfaceImpl row.
        var derivedOfIntB = new BlobBuilder();
        derivedOfIntB.WriteByte(ELEMENT_TYPE_GENERICINST);
        derivedOfIntB.WriteByte(ELEMENT_TYPE_CLASS);
        derivedOfIntB.WriteCompressedInteger(CodedIndex.TypeDefOrRefOrSpec(iderived));
        derivedOfIntB.WriteCompressedInteger(1);
        derivedOfIntB.WriteByte(ELEMENT_TYPE_I4);
        TypeSpecificationHandle derivedOfInt =
            mb.AddTypeSpecification(mb.GetOrAddBlob(derivedOfIntB));

        // TypeSpec: IBase`1<int32> — the MethodImpl declaration's parent.
        var baseOfIntB = new BlobBuilder();
        baseOfIntB.WriteByte(ELEMENT_TYPE_GENERICINST);
        baseOfIntB.WriteByte(ELEMENT_TYPE_CLASS);
        baseOfIntB.WriteCompressedInteger(CodedIndex.TypeDefOrRefOrSpec(ibase));
        baseOfIntB.WriteCompressedInteger(1);
        baseOfIntB.WriteByte(ELEMENT_TYPE_I4);
        TypeSpecificationHandle baseOfInt =
            mb.AddTypeSpecification(mb.GetOrAddBlob(baseOfIntB));

        // Sorted by Class: IDerived`1 (TypeDef row 3), then C (row 4).
        mb.AddInterfaceImplementation(iderived, baseOfDerived);
        mb.AddInterfaceImplementation(c, derivedOfInt);

        MemberReferenceHandle declM = mb.AddMemberReference(
            baseOfInt, mb.GetOrAddString("M"), voidSig);
        mb.AddMethodImplementation(
            c, MetadataTokens.MethodDefinitionHandle(2), declM);
        return Finish(mb);
    }

    // *Hostile* metadata: an F-bounded self-growing interface row,
    // `I`1<T> : I`1<Pair`2<T,T>>`. Every closure frame doubles the
    // instantiated tree, so an unbudgeted substitution walk allocates
    // exponentially (the frame cap alone would not save it — the growth
    // hides inside cloned replacement arguments, not in frame count). The
    // reader must complete quickly with a partial closure; the direct
    // `I<int32>` row still classifies C's impl.
    private static BlobBuilder EmitMethodImplFBoundedGrowth()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplFBoundedFixture", "mifbound");

        BlobHandle voidSig = InstanceVoidSig(mb);
        // Method RID 1: I`1::M. RID 2: C::Impl.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Impl"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle pair = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Pair`2"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle iface = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("I`1"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        mb.AddGenericParameter(pair, GenericParameterAttributes.None, mb.GetOrAddString("T"), 0);
        mb.AddGenericParameter(pair, GenericParameterAttributes.None, mb.GetOrAddString("U"), 1);
        mb.AddGenericParameter(iface, GenericParameterAttributes.None, mb.GetOrAddString("T"), 0);

        // TypeSpec: I`1<Pair`2<!0,!0>> — the self-growing row.
        var growB = new BlobBuilder();
        growB.WriteByte(ELEMENT_TYPE_GENERICINST);
        growB.WriteByte(ELEMENT_TYPE_CLASS);
        growB.WriteCompressedInteger(CodedIndex.TypeDefOrRefOrSpec(iface));
        growB.WriteCompressedInteger(1);
        growB.WriteByte(ELEMENT_TYPE_GENERICINST);
        growB.WriteByte(ELEMENT_TYPE_CLASS);
        growB.WriteCompressedInteger(CodedIndex.TypeDefOrRefOrSpec(pair));
        growB.WriteCompressedInteger(2);
        growB.WriteByte(ELEMENT_TYPE_VAR);
        growB.WriteCompressedInteger(0);
        growB.WriteByte(ELEMENT_TYPE_VAR);
        growB.WriteCompressedInteger(0);
        TypeSpecificationHandle grow = mb.AddTypeSpecification(mb.GetOrAddBlob(growB));

        // TypeSpec: I`1<int32>.
        var iOfIntB = new BlobBuilder();
        iOfIntB.WriteByte(ELEMENT_TYPE_GENERICINST);
        iOfIntB.WriteByte(ELEMENT_TYPE_CLASS);
        iOfIntB.WriteCompressedInteger(CodedIndex.TypeDefOrRefOrSpec(iface));
        iOfIntB.WriteCompressedInteger(1);
        iOfIntB.WriteByte(ELEMENT_TYPE_I4);
        TypeSpecificationHandle iOfInt = mb.AddTypeSpecification(mb.GetOrAddBlob(iOfIntB));

        // Sorted by Class: I`1 (TypeDef row 3), then C (row 4).
        mb.AddInterfaceImplementation(iface, grow);
        mb.AddInterfaceImplementation(c, iOfInt);

        MemberReferenceHandle declM = mb.AddMemberReference(
            iOfInt, mb.GetOrAddString("M"), voidSig);
        mb.AddMethodImplementation(
            c, MetadataTokens.MethodDefinitionHandle(2), declM);
        return Finish(mb);
    }

    // A property whose getter and setter implement two *distinct overloads*
    // of the same-named external member: `C.P`'s getter maps to
    // `IExt::X(): int32` and its setter to `IExt::X(int32): void` — two
    // MemberRefs sharing the name `X` with different signatures (interfaces
    // may overload). Both rows are real and distinct; the accessor union
    // must not collapse the two identical-looking `Unresolved("X")` entries,
    // because name equality cannot prove two external declarations are one
    // member.
    private static BlobBuilder EmitMethodImplOverloadedExternalAccessorDecls()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplOverloadedDeclsFixture", "mioverl");

        TypeReferenceHandle iext = mb.AddTypeReference(
            AddMscorlib(mb), mb.GetOrAddString("System"), mb.GetOrAddString("IExt"));

        var getSigB = new BlobBuilder();
        getSigB.WriteByte(HASTHIS);
        getSigB.WriteByte(0x00);
        getSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle getSig = mb.GetOrAddBlob(getSigB);
        var setSigB = new BlobBuilder();
        setSigB.WriteByte(HASTHIS);
        setSigB.WriteByte(0x01);
        setSigB.WriteByte(ELEMENT_TYPE_VOID);
        setSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle setSig = mb.GetOrAddBlob(setSigB);
        var propSigB = new BlobBuilder();
        propSigB.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSigB.WriteByte(0x00);
        propSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle propSig = mb.GetOrAddBlob(propSigB);

        MemberReferenceHandle xGet = mb.AddMemberReference(
            iext, mb.GetOrAddString("X"), getSig);
        MemberReferenceHandle xSet = mb.AddMemberReference(
            iext, mb.GetOrAddString("X"), setSig);

        // Method RID 1: C::get_P. RID 2: C::set_P.
        MethodDefinitionHandle cGetP = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("get_P"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle cSetP = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("set_P"),
            setSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        mb.AddInterfaceImplementation(c, iext);

        PropertyDefinitionHandle cP = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), propSig);
        mb.AddPropertyMap(c, cP);
        mb.AddMethodSemantics(cP, MethodSemanticsAttributes.Getter, cGetP);
        mb.AddMethodSemantics(cP, MethodSemanticsAttributes.Setter, cSetP);

        mb.AddMethodImplementation(c, cGetP, xGet);
        mb.AddMethodImplementation(c, cSetP, xSet);
        return Finish(mb);
    }

    // One MethodDef serving *both* event roles: `C.E`'s Adder and Remover
    // MethodSemantics rows both name `both_E` (crafted IL; no compiler emits
    // it). That method carries exactly one MethodImpl row, to external
    // `IExt::add_X`. The projection must contribute the shared accessor
    // *once* — projecting it per role would make the single metadata row
    // look like two implementations, since unresolved entries are
    // (deliberately) never deduplicated by value.
    private static BlobBuilder EmitMethodImplSharedEventAccessor()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplSharedAccessorFixture", "mishared");

        TypeReferenceHandle iext = mb.AddTypeReference(
            AddMscorlib(mb), mb.GetOrAddString("System"), mb.GetOrAddString("IExt"));
        TypeReferenceHandle handler = AddEventHandler(mb);
        BlobHandle sig = EventAccessorSig(mb, handler, instance: true);
        MemberReferenceHandle addX = mb.AddMemberReference(
            iext, mb.GetOrAddString("add_X"), sig);

        // Method RID 1: C::both_E (adder *and* remover).
        MethodDefinitionHandle both = AddEventAccessor(mb, "both_E", sig, false);

        AddModuleType(mb);
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        mb.AddInterfaceImplementation(c, iext);

        EventDefinitionHandle cE = mb.AddEvent(
            EventAttributes.None, mb.GetOrAddString("E"), handler);
        mb.AddEventMap(c, cE);
        mb.AddMethodSemantics(cE, MethodSemanticsAttributes.Adder, both);
        mb.AddMethodSemantics(cE, MethodSemanticsAttributes.Remover, both);

        mb.AddMethodImplementation(c, both, addX);
        return Finish(mb);
    }

    // C# 8 default-interface-method *reabstraction*, mirrored byte-for-byte
    // from what Roslyn emits for `interface I2 : I1 { abstract void I1.M(); }`:
    // a MethodImpl on the *interface* `I2` whose body is I2's own abstract
    // (RVA-0) method `I1.M`, redeclaring `I1::M` as abstract for I2's
    // implementors. The abstract body is valid, loadable, compiler-emitted
    // metadata — a reader gate requiring an executable body would wrongly
    // drop it (as it would VB's `MustOverride Sub M() Implements IFoo.M`,
    // the class-side analogue, which the runtime also loads).
    private static BlobBuilder EmitMethodImplReabstraction()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplReabstractionFixture", "mireabs");

        BlobHandle voidSig = InstanceVoidSig(mb);
        // Method RID 1: I1::M. RID 2: I2::"I1.M" (abstract, RVA 0).
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            MethodAttributes.Private | MethodAttributes.HideBySig
                | MethodAttributes.NewSlot | MethodAttributes.Abstract
                | MethodAttributes.Virtual | MethodAttributes.Final,
            MethodImplAttributes.IL, mb.GetOrAddString("I1.M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle i1 = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("I1"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle i2 = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("I2"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        mb.AddInterfaceImplementation(i2, i1);
        mb.AddMethodImplementation(
            i2,
            MetadataTokens.MethodDefinitionHandle(2),
            MetadataTokens.MethodDefinitionHandle(1));
        return Finish(mb);
    }

    // As `methodimpl_local_generic_iface_memberref`, but the MemberRef's
    // *signature* blob reaches `System.Object` through a *duplicate* TypeRef
    // row while the MethodDef's signature uses the original — so the blobs
    // are not byte-identical even though the signatures are the same method
    // signature (the shape an IL weaver produces; the CLR compares MemberRef
    // signatures semantically, not byte-wise). The declaration must still
    // resolve to the owning property `P` through MethodSemantics.
    private static BlobBuilder EmitMethodImplDupTypeRefSig()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplDupSigFixture", "midupsig");

        AssemblyReferenceHandle mscorlib = AddMscorlib(mb);
        TypeReferenceHandle objectA = mb.AddTypeReference(
            mscorlib, mb.GetOrAddString("System"), mb.GetOrAddString("Object"));
        TypeReferenceHandle objectB = mb.AddTypeReference(
            mscorlib, mb.GetOrAddString("System"), mb.GetOrAddString("Object"));

        // Getter signature returning `class System.Object`, once per TypeRef.
        BlobHandle getSigA = ObjectGetterSig(mb, objectA);
        BlobHandle getSigB = ObjectGetterSig(mb, objectB);
        var propSigB2 = new BlobBuilder();
        propSigB2.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSigB2.WriteByte(0x00);
        propSigB2.WriteByte(ELEMENT_TYPE_CLASS);
        WriteTypeToken(propSigB2, objectA);
        BlobHandle propSig = mb.GetOrAddBlob(propSigB2);

        // Method RID 1: IGen`1::Read (getter of P, returns Object via TypeRef A).
        // RID 2: C::get_P (getter of C's P).
        MethodDefinitionHandle read = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("Read"),
            getSigA, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle cGetP = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("get_P"),
            getSigA, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle igen = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IGen`1"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        mb.AddGenericParameter(igen, GenericParameterAttributes.None, mb.GetOrAddString("T"), 0);

        // TypeSpec: GENERICINST CLASS IGen`1 <1> int32 — shared by the
        // InterfaceImpl row and the MemberRef parent.
        var specB = new BlobBuilder();
        specB.WriteByte(ELEMENT_TYPE_GENERICINST);
        specB.WriteByte(ELEMENT_TYPE_CLASS);
        specB.WriteCompressedInteger(CodedIndex.TypeDefOrRefOrSpec(igen));
        specB.WriteCompressedInteger(1);
        specB.WriteByte(ELEMENT_TYPE_I4);
        TypeSpecificationHandle spec = mb.AddTypeSpecification(mb.GetOrAddBlob(specB));
        mb.AddInterfaceImplementation(c, spec);

        // The MemberRef's signature spells the same getter through the
        // *duplicate* TypeRef, so the blob differs from RID 1's byte-wise.
        MemberReferenceHandle readRef = mb.AddMemberReference(
            spec, mb.GetOrAddString("Read"), getSigB);

        PropertyDefinitionHandle igenP = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), propSig);
        PropertyDefinitionHandle cP = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), propSig);
        mb.AddPropertyMap(igen, igenP);
        mb.AddPropertyMap(c, cP);
        mb.AddMethodSemantics(igenP, MethodSemanticsAttributes.Getter, read);
        mb.AddMethodSemantics(cP, MethodSemanticsAttributes.Getter, cGetP);

        mb.AddMethodImplementation(c, cGetP, readRef);
        return Finish(mb);
    }

    // A getter signature `class System.Object <instance, 0 params>` spelled
    // through the given TypeRef.
    private static BlobHandle ObjectGetterSig(MetadataBuilder mb, TypeReferenceHandle objectRef)
    {
        var sig = new BlobBuilder();
        sig.WriteByte(HASTHIS);
        sig.WriteByte(0x00);
        sig.WriteByte(ELEMENT_TYPE_CLASS);
        WriteTypeToken(sig, objectRef);
        return mb.GetOrAddBlob(sig);
    }

    // An explicit interface *event* implementation wired only through the
    // event's *fire* accessor: `C.E`'s add/remove carry no MethodImpl; the
    // fire accessor alone maps to `IEvt`'s fire accessor. Fire is a
    // first-class event semantic (§II.22.28), so the implementation must
    // surface on `C.E` — a union over add/remove only would lose it.
    private static BlobBuilder EmitMethodImplEventFireImpl()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplEventFireFixture", "mievfire");
        TypeReferenceHandle handler = AddEventHandler(mb);
        BlobHandle sig = EventAccessorSig(mb, handler, instance: true);

        // Method RID 1-3: IEvt::add_E/remove_E/fire_E. RID 4-6: C's.
        MethodDefinitionHandle iAdd = AddEventAccessor(mb, "add_E", sig, false);
        MethodDefinitionHandle iRemove = AddEventAccessor(mb, "remove_E", sig, false);
        MethodDefinitionHandle iFire = AddEventAccessor(mb, "fire_E", sig, false);
        MethodDefinitionHandle cAdd = AddEventAccessor(mb, "add_E", sig, false);
        MethodDefinitionHandle cRemove = AddEventAccessor(mb, "remove_E", sig, false);
        MethodDefinitionHandle cFire = AddEventAccessor(mb, "fire_E", sig, false);

        AddModuleType(mb);
        TypeDefinitionHandle ievt = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IEvt"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(4));
        mb.AddInterfaceImplementation(c, ievt);

        EventDefinitionHandle iE = mb.AddEvent(
            EventAttributes.None, mb.GetOrAddString("E"), handler);
        EventDefinitionHandle cE = mb.AddEvent(
            EventAttributes.None, mb.GetOrAddString("E"), handler);
        mb.AddEventMap(ievt, iE);
        mb.AddEventMap(c, cE);
        mb.AddMethodSemantics(iE, MethodSemanticsAttributes.Adder, iAdd);
        mb.AddMethodSemantics(iE, MethodSemanticsAttributes.Remover, iRemove);
        mb.AddMethodSemantics(iE, MethodSemanticsAttributes.Raiser, iFire);
        mb.AddMethodSemantics(cE, MethodSemanticsAttributes.Adder, cAdd);
        mb.AddMethodSemantics(cE, MethodSemanticsAttributes.Remover, cRemove);
        mb.AddMethodSemantics(cE, MethodSemanticsAttributes.Raiser, cFire);

        mb.AddMethodImplementation(c, cFire, iFire);
        return Finish(mb);
    }

    // A MethodImpl declaration that is an *Other*-semantics accessor:
    // `IProp::P` has getter `Read` and an `Other` accessor `Aux` (both
    // MethodSemantics rows). `C.Q`'s getter implements `Aux`. `Other` is an
    // authoritative association (§II.22.28), so the implemented member is
    // property `P` — reading `Aux` as an ordinary interface method would
    // misstate what MethodSemantics says.
    private static BlobBuilder EmitMethodImplOtherAccessorDecl()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplOtherAccessorFixture", "miothacc");

        var getSigB = new BlobBuilder();
        getSigB.WriteByte(HASTHIS);
        getSigB.WriteByte(0x00);
        getSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle getSig = mb.GetOrAddBlob(getSigB);
        var propSigB = new BlobBuilder();
        propSigB.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSigB.WriteByte(0x00);
        propSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle propSig = mb.GetOrAddBlob(propSigB);

        // Method RID 1: IProp::Read. RID 2: IProp::Aux. RID 3: C::Fetch.
        MethodDefinitionHandle read = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("Read"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle aux = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("Aux"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle cFetch = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("Fetch"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle iprop = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IProp"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(3));
        mb.AddInterfaceImplementation(c, iprop);

        PropertyDefinitionHandle ipropP = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P"), propSig);
        PropertyDefinitionHandle cQ = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("Q"), propSig);
        mb.AddPropertyMap(iprop, ipropP);
        mb.AddPropertyMap(c, cQ);
        mb.AddMethodSemantics(ipropP, MethodSemanticsAttributes.Getter, read);
        mb.AddMethodSemantics(ipropP, MethodSemanticsAttributes.Other, aux);
        mb.AddMethodSemantics(cQ, MethodSemanticsAttributes.Getter, cFetch);

        mb.AddMethodImplementation(c, cFetch, aux);
        return Finish(mb);
    }

    // A MethodImpl declaration claimed as an accessor by *two* properties:
    // `MethodSemantics` does not make `Method` unique (§II.22.28), so
    // `IProp::G` is the getter of both `P1` and `P2`. `C.R`'s getter
    // implements `G`; both owners must surface — keeping only the first
    // silently loses an authoritative association.
    private static BlobBuilder EmitMethodImplMultiOwnerAccessor()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplMultiOwnerFixture", "mimulown");

        var getSigB = new BlobBuilder();
        getSigB.WriteByte(HASTHIS);
        getSigB.WriteByte(0x00);
        getSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle getSig = mb.GetOrAddBlob(getSigB);
        var propSigB = new BlobBuilder();
        propSigB.WriteByte(CALLCONV_PROPERTY | HASTHIS);
        propSigB.WriteByte(0x00);
        propSigB.WriteByte(ELEMENT_TYPE_I4);
        BlobHandle propSig = mb.GetOrAddBlob(propSigB);

        // Method RID 1: IProp::G. RID 2: C::Fetch.
        MethodDefinitionHandle g = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("G"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        MethodDefinitionHandle cFetch = mb.AddMethodDefinition(
            AccessorFlags, MethodImplAttributes.IL, mb.GetOrAddString("Fetch"),
            getSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle iprop = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IProp"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle c = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("C"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        mb.AddInterfaceImplementation(c, iprop);

        PropertyDefinitionHandle p1 = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P1"), propSig);
        PropertyDefinitionHandle p2 = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("P2"), propSig);
        PropertyDefinitionHandle cR = mb.AddProperty(
            PropertyAttributes.None, mb.GetOrAddString("R"), propSig);
        mb.AddPropertyMap(iprop, p1);
        mb.AddPropertyMap(c, cR);
        mb.AddMethodSemantics(p1, MethodSemanticsAttributes.Getter, g);
        mb.AddMethodSemantics(p2, MethodSemanticsAttributes.Getter, g);
        mb.AddMethodSemantics(cR, MethodSemanticsAttributes.Getter, cFetch);

        mb.AddMethodImplementation(c, cFetch, g);
        return Finish(mb);
    }

    // A MethodImpl declaration whose parent is a *module-scoped* TypeRef — a
    // TypeRef whose ResolutionScope is the current module, aliasing the
    // in-module `IFoo` TypeDef. ECMA-335 permits the shape but recommends the
    // TypeDef token instead, and no observed compiler emits it (probed:
    // FSharp.Core, FSharp.Compiler.Service, MiniLibFs, the whole
    // net10.0 ref pack — zero in MethodImpl/InterfaceImpl). The reader
    // deliberately does not name-resolve such aliases; this fixture pins the
    // fail-soft outcome: the row is skipped (the InterfaceImpl row holds the
    // TypeDef token, which never compares equal to a Reference-scoped alias),
    // never misclassified.
    private static BlobBuilder EmitMethodImplModuleTypeRefDecl()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplModuleTypeRefFixture", "mimodref");

        BlobHandle voidSig = InstanceVoidSig(mb);
        // Method RID 1: IFoo::M. RID 2: Widget::Impl.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Impl"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle ifoo = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IFoo"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle widget = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        mb.AddInterfaceImplementation(widget, ifoo);

        // A module-scoped TypeRef aliasing IFoo (ResolutionScope = the Module
        // row), and a MemberRef for `M` through it.
        TypeReferenceHandle ifooAlias = mb.AddTypeReference(
            EntityHandle.ModuleDefinition, default, mb.GetOrAddString("IFoo"));
        MemberReferenceHandle declM = mb.AddMemberReference(
            ifooAlias, mb.GetOrAddString("M"), voidSig);
        mb.AddMethodImplementation(
            widget, MetadataTokens.MethodDefinitionHandle(2), declM);
        return Finish(mb);
    }

    // A *structurally* malformed row: `Class` is TypeDef RID 100 in a module
    // whose TypeDef table has 3 rows. Unlike the semantic mismatch above (a
    // well-formed index naming the wrong type, which is skipped), an index
    // past the end of a table violates the reader's structural contract and
    // must propagate as an error, not vanish as a silent skip.
    private static BlobBuilder EmitMethodImplClassOutOfRange()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "MethodImplClassRangeFixture", "mirange");

        BlobHandle voidSig = InstanceVoidSig(mb);
        // Method RID 1: IFoo::M. RID 2: Widget::Impl.
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("M"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Impl"),
            voidSig, bodyOffset: -1, MetadataTokens.ParameterHandle(1));

        AddModuleType(mb);
        TypeDefinitionHandle ifoo = mb.AddTypeDefinition(
            PublicInterface, default, mb.GetOrAddString("IFoo"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        TypeDefinitionHandle widget = mb.AddTypeDefinition(
            PublicAbstractClass, default, mb.GetOrAddString("Widget"), baseType: default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(2));
        mb.AddInterfaceImplementation(widget, ifoo);
        mb.AddMethodImplementation(
            MetadataTokens.TypeDefinitionHandle(100),
            MetadataTokens.MethodDefinitionHandle(2),
            MetadataTokens.MethodDefinitionHandle(1));
        return Finish(mb);
    }

    // ----- Generic-parameter decode shapes -----------------------------------

    // `T Pick<T, U>()` whose MethodDefSig declares 2 generic params but only one
    // GenericParam row exists — the calling-convention arity disagrees with the
    // table.
    private static BlobBuilder EmitGenericArityMismatch()
    {
        var mb = new MetadataBuilder();
        Preamble(mb, "GenericArityFixture", "genarity");

        var sig = new BlobBuilder();
        sig.WriteByte(CALLCONV_GENERIC | HASTHIS);
        sig.WriteCompressedInteger(2); // sig claims 2 generic params
        sig.WriteByte(0x00);
        sig.WriteByte(ELEMENT_TYPE_MVAR);
        sig.WriteCompressedInteger(0);
        MethodDefinitionHandle method = mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Pick"),
            mb.GetOrAddBlob(sig), bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        mb.AddGenericParameter(method, GenericParameterAttributes.None, mb.GetOrAddString("T"), 0);

        return FinishWithHostInterface(mb, _ => { });
    }

    // `[IsUnmanagedAttribute]` on a typar that lacks the value-type special
    // constraint — the attribute is only meaningful as a refinement of `struct`.
    private static BlobBuilder EmitUnmanagedAttributeWithoutStruct() =>
        EmitGenericMethodWithCa("UnmanagedAttrFixture", "unmattr", (mb, _, gp) =>
        {
            MemberReferenceHandle ctor = AddAttrCtor(
                mb, "System.Runtime.CompilerServices", "IsUnmanagedAttribute", Array.Empty<byte[]>());
            mb.AddCustomAttribute(gp, ctor, CaBlob(mb, b => b.WriteUInt16(0)));
        });

    // `T Pick<T>()` with one type constraint whose TypeSpec blob is produced by
    // `writeConstraint`; `gpAttrs` sets the typar's special-constraint bits.
    private static BlobBuilder EmitGenericMethodWithConstraint(
        string fixtureName, string mvid, GenericParameterAttributes gpAttrs,
        Action<MetadataBuilder, BlobBuilder> writeConstraint)
    {
        var mb = new MetadataBuilder();
        Preamble(mb, fixtureName, mvid);

        var sig = new BlobBuilder();
        sig.WriteByte(CALLCONV_GENERIC | HASTHIS);
        sig.WriteCompressedInteger(1);
        sig.WriteByte(0x00);
        sig.WriteByte(ELEMENT_TYPE_MVAR);
        sig.WriteCompressedInteger(0);
        MethodDefinitionHandle method = mb.AddMethodDefinition(
            AbstractPublic, MethodImplAttributes.IL, mb.GetOrAddString("Pick"),
            mb.GetOrAddBlob(sig), bodyOffset: -1, MetadataTokens.ParameterHandle(1));
        GenericParameterHandle gp = mb.AddGenericParameter(method, gpAttrs, mb.GetOrAddString("T"), 0);

        var specBlob = new BlobBuilder();
        writeConstraint(mb, specBlob);
        TypeSpecificationHandle spec = mb.AddTypeSpecification(mb.GetOrAddBlob(specBlob));
        mb.AddGenericParameterConstraint(gp, spec);

        return FinishWithHostInterface(mb, _ => { });
    }

    // A type constraint carrying `modreq(IsConst) class IComparable` — a custom
    // modifier on a constraint the projector refuses.
    private static BlobBuilder EmitConstraintModreq() =>
        EmitGenericMethodWithConstraint(
            "ConstraintModreqFixture", "cmodreq", GenericParameterAttributes.None, (mb, b) =>
            {
                AssemblyReferenceHandle mscorlib = AddMscorlib(mb);
                TypeReferenceHandle isConst = mb.AddTypeReference(
                    mscorlib,
                    mb.GetOrAddString("System.Runtime.CompilerServices"),
                    mb.GetOrAddString("IsConst"));
                TypeReferenceHandle iComparable = mb.AddTypeReference(
                    mscorlib, mb.GetOrAddString("System"), mb.GetOrAddString("IComparable"));
                b.WriteByte(ELEMENT_TYPE_CMOD_REQD);
                WriteTypeToken(b, isConst);
                b.WriteByte(ELEMENT_TYPE_CLASS);
                WriteTypeToken(b, iComparable);
            });

    // `modreq(UnmanagedType) class IComparable` — the unmanaged modreq decode
    // must only fire on the canonical `System.ValueType` shape; on any other
    // constraint it is just an unsupported custom modifier.
    private static BlobBuilder EmitConstraintUnmanagedModreqNonValueType() =>
        EmitGenericMethodWithConstraint(
            "ConstraintUnmModreqFixture", "cunmmodreq", GenericParameterAttributes.None, (mb, b) =>
            {
                AssemblyReferenceHandle mscorlib = AddMscorlib(mb);
                TypeReferenceHandle unmanagedType = mb.AddTypeReference(
                    mscorlib,
                    mb.GetOrAddString("System.Runtime.InteropServices"),
                    mb.GetOrAddString("UnmanagedType"));
                TypeReferenceHandle iComparable = mb.AddTypeReference(
                    mscorlib, mb.GetOrAddString("System"), mb.GetOrAddString("IComparable"));
                b.WriteByte(ELEMENT_TYPE_CMOD_REQD);
                WriteTypeToken(b, unmanagedType);
                b.WriteByte(ELEMENT_TYPE_CLASS);
                WriteTypeToken(b, iComparable);
            });

    // `modopt(IsConst) modreq(UnmanagedType) valuetype System.ValueType` on a
    // `struct` typar — the canonical `unmanaged` marker with an *ignorable*
    // modifier in front of it. II.7.1.1 says the `modopt` may be ignored, so what
    // remains is exactly the marker: the constraint is consumed (setting
    // `is_unmanaged`) rather than refused as an unrecognised `modreq`, and no
    // stray `System.ValueType` interface is surfaced.
    private static BlobBuilder EmitUnmanagedModreqBehindModopt() =>
        EmitGenericMethodWithConstraint(
            "UnmModreqModoptFixture", "unmmodopt",
            GenericParameterAttributes.NotNullableValueTypeConstraint
                | GenericParameterAttributes.DefaultConstructorConstraint,
            (mb, b) =>
            {
                AssemblyReferenceHandle mscorlib = AddMscorlib(mb);
                TypeReferenceHandle isConst = mb.AddTypeReference(
                    mscorlib,
                    mb.GetOrAddString("System.Runtime.CompilerServices"),
                    mb.GetOrAddString("IsConst"));
                TypeReferenceHandle unmanagedType = mb.AddTypeReference(
                    mscorlib,
                    mb.GetOrAddString("System.Runtime.InteropServices"),
                    mb.GetOrAddString("UnmanagedType"));
                TypeReferenceHandle valueType = mb.AddTypeReference(
                    mscorlib, mb.GetOrAddString("System"), mb.GetOrAddString("ValueType"));
                b.WriteByte(ELEMENT_TYPE_CMOD_OPT);
                WriteTypeToken(b, isConst);
                b.WriteByte(ELEMENT_TYPE_CMOD_REQD);
                WriteTypeToken(b, unmanagedType);
                b.WriteByte(ELEMENT_TYPE_VALUETYPE);
                WriteTypeToken(b, valueType);
            });

    // The canonical `modreq(UnmanagedType) valuetype System.ValueType` unmanaged
    // shape but on a typar WITHOUT the value-type special-constraint bit — an
    // inconsistent typar the projector refuses.
    private static BlobBuilder EmitUnmanagedModreqWithoutStruct() =>
        EmitGenericMethodWithConstraint(
            "UnmModreqNoStructFixture", "unmmodreq", GenericParameterAttributes.None, (mb, b) =>
            {
                AssemblyReferenceHandle mscorlib = AddMscorlib(mb);
                TypeReferenceHandle unmanagedType = mb.AddTypeReference(
                    mscorlib,
                    mb.GetOrAddString("System.Runtime.InteropServices"),
                    mb.GetOrAddString("UnmanagedType"));
                TypeReferenceHandle valueType = mb.AddTypeReference(
                    mscorlib, mb.GetOrAddString("System"), mb.GetOrAddString("ValueType"));
                b.WriteByte(ELEMENT_TYPE_CMOD_REQD);
                WriteTypeToken(b, unmanagedType);
                b.WriteByte(ELEMENT_TYPE_VALUETYPE);
                WriteTypeToken(b, valueType);
            });
}

//! IL-side pickle decoders.
//!
//! Phase 6b2 implemented `u_ILScopeRef` and its `u_ILModuleRef` /
//! `u_ILAssemblyRef` / `u_ILVersion` / `u_ILPublicKey` dependencies —
//! everything `u_cpath` needs. Phase 6b3 extends this with the wider
//! IL chain that `u_attribs` transitively requires through its
//! `ILAttrib` branch: `u_ILTypeRef`, `u_ILHasThis`, `u_ILBasicCallConv`,
//! `u_ILCallConv`, `u_ILArrayShape`, `u_ILTypeSpec`, `u_ILCallSig`,
//! `u_ILType` (all 9 tags), `u_ILTypes`, `u_ILMethodRef`.
//!
//! ### FCS source map
//!
//! - `u_ILScopeRef` — `TypedTreePickle.fs:1223-1231`.
//! - `u_ILModuleRef` — `:1205-1207`: `u_tup3 u_string u_bool (u_option u_bytes)`.
//! - `u_ILAssemblyRef` — `:1209-1218`: tag byte (must be `0`), then
//!   `u_tup6 u_string (u_option u_bytes) (u_option u_ILPublicKey) u_bool
//!   (u_option u_ILVersion) (u_option u_string)`.
//! - `u_ILVersion` — `:1201-1203`: `u_tup4 u_uint16 u_uint16 u_uint16 u_uint16`.
//! - `u_ILPublicKey` — `:1193-1199`: tag byte (0 = full key, 1 = token)
//!   + length-prefixed bytes.
//! - `u_strings` — `:832`: `u_list u_string`.
//! - `u_ILBasicCallConv` — `:1300-1308`: `u_byte` tag, 6 variants.
//! - `u_ILHasThis` — `:1310-1315`: `u_byte` tag, 3 variants.
//! - `u_ILCallConv` — `:1317-1319`: `u_tup2 u_ILHasThis u_ILBasicCallConv`.
//! - `u_ILTypeRef` — `:1321-1323`: `u_tup3 u_ILScopeRef u_strings u_string`.
//! - `u_ILArrayShape` — `:1325-1326`: `u_list (u_tup2 (u_option u_int32) (u_option u_int32))`.
//! - `u_ILType` — `:1328-1341`: 9-tag dispatcher.
//! - `u_ILTypes` — `:1343`: `u_list u_ILType`.
//! - `u_ILCallSig` — `:1345-1353`: `u_tup3 u_ILCallConv u_ILTypes u_ILType`.
//! - `u_ILTypeSpec` — `:1355-1357`: `u_tup2 u_ILTypeRef u_ILTypes`.
//! - `u_ILMethodRef` — `:1412-1416`: `u_tup6 u_ILTypeRef u_ILCallConv u_int u_string u_ILTypes u_ILType`.

use crate::error::ImportError;
use crate::fsharp_pickle::model::{
    PickledILArrayShape, PickledILAssemblyRef, PickledILBasicCallConv, PickledILCallConv,
    PickledILCallSig, PickledILHasThis, PickledILMethodRef, PickledILModuleRef, PickledILPublicKey,
    PickledILScopeRef, PickledILType, PickledILTypeRef, PickledILTypeSpec, PickledILVersion,
};
use crate::fsharp_pickle::reader::PickleReader;

/// `u_ILPublicKey` (`:1193-1199`): tag byte (0 = full key, 1 = token)
/// followed by `u_bytes` (a length-prefixed byte blob).
///
/// The tag is validated *before* consuming the payload so a malformed
/// pickle reports the unsupported-tag error directly rather than
/// surfacing an `UnexpectedEndOfStream` or a fabricated length read
/// from arbitrary downstream bytes.
pub(crate) fn read_il_public_key(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILPublicKey, ImportError> {
    let tag = reader.read_byte("u_ILPublicKey tag")?;
    match tag {
        0 => Ok(PickledILPublicKey::PublicKey(
            reader.read_byte_memory("u_ILPublicKey bytes")?.to_vec(),
        )),
        1 => Ok(PickledILPublicKey::PublicKeyToken(
            reader.read_byte_memory("u_ILPublicKey bytes")?.to_vec(),
        )),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_ILPublicKey tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_ILVersion` (`:1201-1203`): four `u_uint16` words. Under the hood
/// `u_uint16` is `u_int32` truncated (`:438`), so each component is a
/// compressed int even though the field is logically 16-bit.
pub(crate) fn read_il_version(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILVersion, ImportError> {
    let major = reader.read_uint32("u_ILVersion major")? as u16;
    let minor = reader.read_uint32("u_ILVersion minor")? as u16;
    let build = reader.read_uint32("u_ILVersion build")? as u16;
    let revision = reader.read_uint32("u_ILVersion revision")? as u16;
    Ok(PickledILVersion {
        major,
        minor,
        build,
        revision,
    })
}

/// `u_ILModuleRef` (`:1205-1207`): `u_tup3 u_string u_bool (u_option u_bytes)`.
/// The fields are name, has-metadata, optional hash blob.
pub(crate) fn read_il_module_ref(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILModuleRef, ImportError> {
    let name = reader.read_string("u_ILModuleRef name")?;
    let has_metadata = reader.read_bool("u_ILModuleRef hasMetadata")?;
    let hash = reader.read_option("u_ILModuleRef hash option-tag", |r| {
        Ok(r.read_byte_memory("u_ILModuleRef hash bytes")?.to_vec())
    })?;
    Ok(PickledILModuleRef {
        name,
        has_metadata,
        hash,
    })
}

/// `u_ILAssemblyRef` (`:1209-1218`): leading tag byte (FCS only emits
/// `0`; any other value is malformed), then `u_tup6` of name, optional
/// hash, optional public key, retargetable bool, optional version,
/// optional locale.
pub(crate) fn read_il_assembly_ref(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILAssemblyRef, ImportError> {
    let tag = reader.read_byte("u_ILAssemblyRef tag")?;
    if tag != 0 {
        return Err(ImportError::UnsupportedPickleTag {
            context: "u_ILAssemblyRef tag",
            tag: u32::from(tag),
        });
    }
    let name = reader.read_string("u_ILAssemblyRef name")?;
    let hash = reader.read_option("u_ILAssemblyRef hash option-tag", |r| {
        Ok(r.read_byte_memory("u_ILAssemblyRef hash bytes")?.to_vec())
    })?;
    let public_key =
        reader.read_option("u_ILAssemblyRef publicKey option-tag", read_il_public_key)?;
    let retargetable = reader.read_bool("u_ILAssemblyRef retargetable")?;
    let version = reader.read_option("u_ILAssemblyRef version option-tag", read_il_version)?;
    let locale = reader.read_option("u_ILAssemblyRef locale option-tag", |r| {
        r.read_string("u_ILAssemblyRef locale")
    })?;
    Ok(PickledILAssemblyRef {
        name,
        hash,
        public_key,
        retargetable,
        version,
        locale,
    })
}

/// `u_ILScopeRef` (`:1223-1231`): tag byte dispatching three variants.
/// FCS's call site also rescopes `Local` against the importing reader's
/// own scope (`:1233`); we don't model that — `Local` survives as
/// `Local`, and any consumer that needs the rescope applies it at the
/// projection boundary.
pub(crate) fn read_il_scope_ref(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILScopeRef, ImportError> {
    let tag = reader.read_byte("u_ILScopeRef tag")?;
    match tag {
        0 => Ok(PickledILScopeRef::Local),
        1 => Ok(PickledILScopeRef::Module(read_il_module_ref(reader)?)),
        2 => Ok(PickledILScopeRef::Assembly(read_il_assembly_ref(reader)?)),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_ILScopeRef tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_strings` (`:832`): `u_list u_string`. Each element is resolved
/// through the strings table at read time.
pub(crate) fn read_strings(reader: &mut PickleReader<'_>) -> Result<Vec<String>, ImportError> {
    reader.read_list("u_strings element", |r| r.read_string("u_strings element"))
}

/// `u_ILHasThis` (`:1310-1315`): single byte tag.
pub(crate) fn read_il_has_this(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILHasThis, ImportError> {
    let tag = reader.read_byte("u_ILHasThis tag")?;
    match tag {
        0 => Ok(PickledILHasThis::Instance),
        1 => Ok(PickledILHasThis::InstanceExplicit),
        2 => Ok(PickledILHasThis::Static),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_ILHasThis tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_ILBasicCallConv` (`:1300-1308`): single byte tag, 6 variants.
pub(crate) fn read_il_basic_call_conv(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILBasicCallConv, ImportError> {
    let tag = reader.read_byte("u_ILBasicCallConv tag")?;
    match tag {
        0 => Ok(PickledILBasicCallConv::Default),
        1 => Ok(PickledILBasicCallConv::CDecl),
        2 => Ok(PickledILBasicCallConv::StdCall),
        3 => Ok(PickledILBasicCallConv::ThisCall),
        4 => Ok(PickledILBasicCallConv::FastCall),
        5 => Ok(PickledILBasicCallConv::VarArg),
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_ILBasicCallConv tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_ILCallConv` (`:1317-1319`): `u_tup2 u_ILHasThis u_ILBasicCallConv`.
pub(crate) fn read_il_call_conv(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILCallConv, ImportError> {
    let has_this = read_il_has_this(reader)?;
    let basic = read_il_basic_call_conv(reader)?;
    Ok(PickledILCallConv { has_this, basic })
}

/// `u_ILTypeRef` (`:1321-1323`): `u_tup3 u_ILScopeRef u_strings u_string`.
pub(crate) fn read_il_type_ref(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILTypeRef, ImportError> {
    let scope = read_il_scope_ref(reader)?;
    let enclosing = read_strings(reader)?;
    let name = reader.read_string("u_ILTypeRef name")?;
    Ok(PickledILTypeRef {
        scope,
        enclosing,
        name,
    })
}

/// `u_ILArrayShape` (`:1325-1326`): `u_list (u_tup2 (u_option u_int32)
/// (u_option u_int32))`.
pub(crate) fn read_il_array_shape(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILArrayShape, ImportError> {
    let bounds = reader.read_list("u_ILArrayShape element", |r| {
        let lo = r.read_option("u_ILArrayShape lower-bound option-tag", |r| {
            r.read_int32("u_ILArrayShape lower-bound")
        })?;
        let size = r.read_option("u_ILArrayShape size option-tag", |r| {
            r.read_int32("u_ILArrayShape size")
        })?;
        Ok((lo, size))
    })?;
    Ok(PickledILArrayShape { bounds })
}

/// `u_ILType` (`:1328-1341`): 9-tag dispatcher. Every tag is decoded
/// eagerly — per D6.5 we never silently consume an unknown opcode.
///
/// Depth-guarded: tags 4 (`Ptr`) and 5 (`Byref`) self-recurse after
/// consuming a single byte, so a malformed run of tag bytes drives one
/// stack frame per byte without the guard.
pub(crate) fn read_il_type(reader: &mut PickleReader<'_>) -> Result<PickledILType, ImportError> {
    reader.enter_recursion("u_ILType")?;
    let result = read_il_type_body(reader);
    reader.exit_recursion();
    result
}

fn read_il_type_body(reader: &mut PickleReader<'_>) -> Result<PickledILType, ImportError> {
    let tag = reader.read_byte("u_ILType tag")?;
    match tag {
        0 => Ok(PickledILType::Void),
        1 => {
            let shape = read_il_array_shape(reader)?;
            let elt = read_il_type(reader)?;
            Ok(PickledILType::Array(shape, Box::new(elt)))
        }
        2 => Ok(PickledILType::Value(read_il_type_spec(reader)?)),
        3 => Ok(PickledILType::Boxed(read_il_type_spec(reader)?)),
        4 => Ok(PickledILType::Ptr(Box::new(read_il_type(reader)?))),
        5 => Ok(PickledILType::Byref(Box::new(read_il_type(reader)?))),
        6 => Ok(PickledILType::FunctionPointer(read_il_call_sig(reader)?)),
        7 => {
            let idx = reader.read_uint32("u_ILType Tyvar index")? as u16;
            Ok(PickledILType::Tyvar(idx))
        }
        8 => {
            let required = reader.read_bool("u_ILType Modified required")?;
            let modifier = read_il_type_ref(reader)?;
            let ty = read_il_type(reader)?;
            Ok(PickledILType::Modified {
                required,
                modifier,
                ty: Box::new(ty),
            })
        }
        other => Err(ImportError::UnsupportedPickleTag {
            context: "u_ILType tag",
            tag: u32::from(other),
        }),
    }
}

/// `u_ILTypes` (`:1343`): `u_list u_ILType`.
pub(crate) fn read_il_types(
    reader: &mut PickleReader<'_>,
) -> Result<Vec<PickledILType>, ImportError> {
    reader.read_list("u_ILTypes element", read_il_type)
}

/// `u_ILCallSig` (`:1345-1353`): `u_tup3 u_ILCallConv u_ILTypes u_ILType`.
pub(crate) fn read_il_call_sig(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILCallSig, ImportError> {
    let call_conv = read_il_call_conv(reader)?;
    let args = read_il_types(reader)?;
    let return_type = Box::new(read_il_type(reader)?);
    Ok(PickledILCallSig {
        call_conv,
        args,
        return_type,
    })
}

/// `u_ILTypeSpec` (`:1355-1357`): `u_tup2 u_ILTypeRef u_ILTypes`.
pub(crate) fn read_il_type_spec(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILTypeSpec, ImportError> {
    let type_ref = read_il_type_ref(reader)?;
    let generic_args = read_il_types(reader)?;
    Ok(PickledILTypeSpec {
        type_ref,
        generic_args,
    })
}

/// `u_ILMethodRef` (`:1412-1416`): `u_tup6 u_ILTypeRef u_ILCallConv
/// u_int u_string u_ILTypes u_ILType`.
///
/// FCS's `ILMethodRef.Create` call at `:1416` reorders the
/// `genericArity` and `name` arguments in the *construction* call;
/// the *wire* order matches the `u_tup6` declaration we follow here.
pub(crate) fn read_il_method_ref(
    reader: &mut PickleReader<'_>,
) -> Result<PickledILMethodRef, ImportError> {
    let parent = read_il_type_ref(reader)?;
    let call_conv = read_il_call_conv(reader)?;
    let generic_arity = reader.read_uint32("u_ILMethodRef genericArity")?;
    let name = reader.read_string("u_ILMethodRef name")?;
    let arg_types = read_il_types(reader)?;
    let return_type = read_il_type(reader)?;
    Ok(PickledILMethodRef {
        parent,
        call_conv,
        generic_arity,
        name,
        arg_types,
        return_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc_str_idx(idx: u32) -> Vec<u8> {
        // Strings are 1-byte literal for any idx < 0x80.
        assert!(idx < 0x80, "test fixture string-index out of literal range");
        vec![idx as u8]
    }

    fn enc_bytes(bs: &[u8]) -> Vec<u8> {
        // length-prefixed (compressed-int length, then raw bytes)
        let mut out = vec![bs.len() as u8];
        out.extend_from_slice(bs);
        out
    }

    fn make_reader<'a>(bytes: &'a [u8], strings: &'a [String]) -> PickleReader<'a> {
        let mut r = PickleReader::new(bytes);
        // SAFETY: tests own both borrows; pubpaths unused here.
        let pubpaths: &'a [Vec<u32>] = &[];
        r.attach_tables(strings, pubpaths);
        r
    }

    #[test]
    fn il_scope_ref_local() {
        let bytes = [0u8];
        let strings: Vec<String> = vec![];
        let mut r = PickleReader::new(&bytes);
        let pubpaths: Vec<Vec<u32>> = vec![];
        r.attach_tables(&strings, &pubpaths);
        assert_eq!(read_il_scope_ref(&mut r).unwrap(), PickledILScopeRef::Local);
        assert!(r.is_eof());
    }

    #[test]
    fn il_module_ref_round_trip() {
        // tag 1 (Module), then u_string idx 0, u_bool 1, u_option None.
        let strings = vec!["MyModule".to_string()];
        let mut bytes = vec![1u8];
        bytes.extend(enc_str_idx(0)); // name
        bytes.push(0x01); // hasMetadata = true
        bytes.push(0x00); // option tag = None
        let mut r = make_reader(&bytes, &strings);
        let scope = read_il_scope_ref(&mut r).unwrap();
        assert_eq!(
            scope,
            PickledILScopeRef::Module(PickledILModuleRef {
                name: "MyModule".to_string(),
                has_metadata: true,
                hash: None,
            })
        );
        assert!(r.is_eof());
    }

    #[test]
    fn il_module_ref_with_hash() {
        let strings = vec!["M".to_string()];
        let mut bytes = vec![1u8];
        bytes.extend(enc_str_idx(0));
        bytes.push(0x00); // hasMetadata = false
        bytes.push(0x01); // option tag = Some
        bytes.extend(enc_bytes(&[0xDE, 0xAD, 0xBE, 0xEF]));
        let mut r = make_reader(&bytes, &strings);
        let scope = read_il_scope_ref(&mut r).unwrap();
        assert_eq!(
            scope,
            PickledILScopeRef::Module(PickledILModuleRef {
                name: "M".to_string(),
                has_metadata: false,
                hash: Some(vec![0xDE, 0xAD, 0xBE, 0xEF]),
            })
        );
    }

    #[test]
    fn il_assembly_ref_minimal_all_none() {
        // tag 2 (Assembly) + inner tag 0 + name + all 5 options = None.
        let strings = vec!["A".to_string()];
        let mut bytes = vec![2u8, 0u8];
        bytes.extend(enc_str_idx(0)); // name
        bytes.push(0x00); // hash = None
        bytes.push(0x00); // publicKey = None
        bytes.push(0x00); // retargetable = false
        bytes.push(0x00); // version = None
        bytes.push(0x00); // locale = None
        let mut r = make_reader(&bytes, &strings);
        let scope = read_il_scope_ref(&mut r).unwrap();
        assert_eq!(
            scope,
            PickledILScopeRef::Assembly(PickledILAssemblyRef {
                name: "A".to_string(),
                hash: None,
                public_key: None,
                retargetable: false,
                version: None,
                locale: None,
            })
        );
    }

    #[test]
    fn il_assembly_ref_with_version_and_public_key_token() {
        let strings = vec!["mscorlib".to_string(), "neutral".to_string()];
        let mut bytes = vec![2u8, 0u8];
        bytes.extend(enc_str_idx(0)); // name = "mscorlib"
        bytes.push(0x00); // hash = None
        // publicKey = Some(PublicKeyToken([0xB7, 0x7A, 0x5C, 0x56, 0x19, 0x34, 0xE0, 0x89]))
        bytes.push(0x01); // option Some
        bytes.push(0x01); // u_ILPublicKey tag 1 = Token
        bytes.extend(enc_bytes(&[0xB7, 0x7A, 0x5C, 0x56, 0x19, 0x34, 0xE0, 0x89]));
        bytes.push(0x00); // retargetable = false
        // version = Some(4.0.0.0)
        bytes.push(0x01); // option Some
        bytes.push(0x04); // major = 4
        bytes.push(0x00); // minor
        bytes.push(0x00); // build
        bytes.push(0x00); // revision
        // locale = Some("neutral")
        bytes.push(0x01);
        bytes.extend(enc_str_idx(1));
        let mut r = make_reader(&bytes, &strings);
        let scope = read_il_scope_ref(&mut r).unwrap();
        let PickledILScopeRef::Assembly(a) = scope else {
            panic!("expected Assembly");
        };
        assert_eq!(a.name, "mscorlib");
        assert!(a.hash.is_none());
        assert_eq!(
            a.public_key,
            Some(PickledILPublicKey::PublicKeyToken(vec![
                0xB7, 0x7A, 0x5C, 0x56, 0x19, 0x34, 0xE0, 0x89,
            ]))
        );
        assert!(!a.retargetable);
        assert_eq!(
            a.version,
            Some(PickledILVersion {
                major: 4,
                minor: 0,
                build: 0,
                revision: 0,
            })
        );
        assert_eq!(a.locale.as_deref(), Some("neutral"));
        assert!(r.is_eof());
    }

    #[test]
    fn il_assembly_ref_rejects_non_zero_outer_tag() {
        // tag 2 (Assembly), then inner tag = 5 (not 0).
        let strings: Vec<String> = vec![];
        let bytes = vec![2u8, 5u8];
        let mut r = make_reader(&bytes, &strings);
        match read_il_scope_ref(&mut r) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_ILAssemblyRef tag",
                tag: 5,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn il_scope_ref_unknown_outer_tag() {
        let strings: Vec<String> = vec![];
        let bytes = vec![9u8];
        let mut r = make_reader(&bytes, &strings);
        match read_il_scope_ref(&mut r) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_ILScopeRef tag",
                tag: 9,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn il_public_key_full_vs_token() {
        let strings: Vec<String> = vec![];
        // PublicKey (tag 0)
        let mut bytes = vec![0u8];
        bytes.extend(enc_bytes(&[1, 2, 3]));
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(
            read_il_public_key(&mut r).unwrap(),
            PickledILPublicKey::PublicKey(vec![1, 2, 3])
        );

        // PublicKeyToken (tag 1)
        let mut bytes = vec![1u8];
        bytes.extend(enc_bytes(&[9, 8, 7]));
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(
            read_il_public_key(&mut r).unwrap(),
            PickledILPublicKey::PublicKeyToken(vec![9, 8, 7])
        );

        // Unknown tag — the decoder must reject on the tag alone,
        // without consuming the trailing byte (which would otherwise
        // be parsed as `u_bytes` and could over-read or surface a
        // misleading EOF on a corrupt resource).
        let bytes = vec![5u8, 0xAA];
        let mut r = make_reader(&bytes, &strings);
        match read_il_public_key(&mut r) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_ILPublicKey tag",
                tag: 5,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
        // The trailing byte must still be available — the failed
        // decoder consumed only the tag.
        assert_eq!(r.read_byte("trailing").unwrap(), 0xAA);
    }

    #[test]
    fn il_version_reads_four_u_uint16() {
        let strings: Vec<String> = vec![];
        // 1.2.3.4 — all fit in single-byte literal
        let bytes = vec![1u8, 2, 3, 4];
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(
            read_il_version(&mut r).unwrap(),
            PickledILVersion {
                major: 1,
                minor: 2,
                build: 3,
                revision: 4,
            }
        );
    }

    // ----- 6b3: IL chain tests -----

    #[test]
    fn il_has_this_each_tag() {
        for (tag, want) in [
            (0u8, PickledILHasThis::Instance),
            (1, PickledILHasThis::InstanceExplicit),
            (2, PickledILHasThis::Static),
        ] {
            let bytes = vec![tag];
            let strings: Vec<String> = vec![];
            let mut r = make_reader(&bytes, &strings);
            assert_eq!(read_il_has_this(&mut r).unwrap(), want);
        }
    }

    #[test]
    fn il_has_this_rejects_unknown_tag() {
        let strings: Vec<String> = vec![];
        let bytes = vec![5u8];
        let mut r = make_reader(&bytes, &strings);
        match read_il_has_this(&mut r) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_ILHasThis tag",
                tag: 5,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn il_basic_call_conv_each_tag() {
        for (tag, want) in [
            (0u8, PickledILBasicCallConv::Default),
            (1, PickledILBasicCallConv::CDecl),
            (2, PickledILBasicCallConv::StdCall),
            (3, PickledILBasicCallConv::ThisCall),
            (4, PickledILBasicCallConv::FastCall),
            (5, PickledILBasicCallConv::VarArg),
        ] {
            let bytes = vec![tag];
            let strings: Vec<String> = vec![];
            let mut r = make_reader(&bytes, &strings);
            assert_eq!(read_il_basic_call_conv(&mut r).unwrap(), want);
        }
    }

    #[test]
    fn il_basic_call_conv_rejects_unknown_tag() {
        let strings: Vec<String> = vec![];
        let bytes = vec![9u8];
        let mut r = make_reader(&bytes, &strings);
        match read_il_basic_call_conv(&mut r) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_ILBasicCallConv tag",
                tag: 9,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn il_call_conv_round_trip() {
        let strings: Vec<String> = vec![];
        // Static + Default
        let bytes = vec![2u8, 0u8];
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(
            read_il_call_conv(&mut r).unwrap(),
            PickledILCallConv {
                has_this: PickledILHasThis::Static,
                basic: PickledILBasicCallConv::Default,
            }
        );
    }

    #[test]
    fn il_type_ref_round_trip() {
        // scope = Local, enclosing = ["Microsoft", "FSharp", "Core"], name = "CompilationMappingAttribute"
        let strings = vec![
            "Microsoft".to_string(),
            "FSharp".to_string(),
            "Core".to_string(),
            "CompilationMappingAttribute".to_string(),
        ];
        let mut bytes = vec![0u8]; // u_ILScopeRef Local
        bytes.push(3); // u_strings length
        bytes.extend(enc_str_idx(0));
        bytes.extend(enc_str_idx(1));
        bytes.extend(enc_str_idx(2));
        bytes.extend(enc_str_idx(3));
        let mut r = make_reader(&bytes, &strings);
        let tr = read_il_type_ref(&mut r).unwrap();
        assert_eq!(tr.scope, PickledILScopeRef::Local);
        assert_eq!(
            tr.enclosing,
            vec![
                "Microsoft".to_string(),
                "FSharp".to_string(),
                "Core".to_string(),
            ]
        );
        assert_eq!(tr.name, "CompilationMappingAttribute");
        assert!(r.is_eof());
    }

    #[test]
    fn il_array_shape_empty() {
        let strings: Vec<String> = vec![];
        let bytes = vec![0u8]; // list length 0
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(
            read_il_array_shape(&mut r).unwrap(),
            PickledILArrayShape { bounds: vec![] }
        );
    }

    #[test]
    fn il_array_shape_with_bounds() {
        // 2 dims: (Some(0), Some(10)), (None, None)
        let strings: Vec<String> = vec![];
        let bytes = vec![
            2u8, // list length
            // dim 0: lo = Some(0), size = Some(10)
            1, 0, // option Some, int 0
            1, 10, // option Some, int 10
            // dim 1: lo = None, size = None
            0, 0,
        ];
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(
            read_il_array_shape(&mut r).unwrap(),
            PickledILArrayShape {
                bounds: vec![(Some(0), Some(10)), (None, None)],
            }
        );
    }

    #[test]
    fn il_type_void() {
        let strings: Vec<String> = vec![];
        let bytes = vec![0u8];
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(read_il_type(&mut r).unwrap(), PickledILType::Void);
    }

    #[test]
    fn il_type_tyvar() {
        let strings: Vec<String> = vec![];
        let bytes = vec![7u8, 3]; // tag 7, idx 3
        let mut r = make_reader(&bytes, &strings);
        assert_eq!(read_il_type(&mut r).unwrap(), PickledILType::Tyvar(3));
    }

    #[test]
    fn il_type_value_typespec() {
        // ILType.Value(typespec) — tag 2 — typespec = (tyref(Local, [], "X"), [])
        let strings = vec!["X".to_string()];
        let mut bytes = vec![2u8]; // tag 2 = Value
        // typespec: tyref + generic_args list
        bytes.push(0u8); // scope = Local
        bytes.push(0u8); // enclosing strings list len = 0
        bytes.extend(enc_str_idx(0)); // name = "X"
        bytes.push(0u8); // generic_args list len = 0
        let mut r = make_reader(&bytes, &strings);
        let t = read_il_type(&mut r).unwrap();
        match t {
            PickledILType::Value(spec) => {
                assert_eq!(spec.type_ref.name, "X");
                assert_eq!(spec.type_ref.scope, PickledILScopeRef::Local);
                assert!(spec.generic_args.is_empty());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn il_type_boxed_typespec() {
        // ILType.Boxed via tag 3
        let strings = vec!["Y".to_string()];
        let mut bytes = vec![3u8, 0u8, 0u8];
        bytes.extend(enc_str_idx(0));
        bytes.push(0u8);
        let mut r = make_reader(&bytes, &strings);
        let t = read_il_type(&mut r).unwrap();
        assert!(matches!(t, PickledILType::Boxed(_)));
    }

    #[test]
    fn il_type_array() {
        // ILType.Array of (shape=[(None,None)], elt=Void)
        let strings: Vec<String> = vec![];
        let bytes = vec![
            1u8, // tag 1 = Array
            1u8, // shape list len = 1
            0, 0,   // (None, None)
            0u8, // elt = ILType.Void
        ];
        let mut r = make_reader(&bytes, &strings);
        let t = read_il_type(&mut r).unwrap();
        match t {
            PickledILType::Array(shape, elt) => {
                assert_eq!(shape.bounds, vec![(None, None)]);
                assert_eq!(*elt, PickledILType::Void);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn il_type_ptr_and_byref_recursive() {
        let strings: Vec<String> = vec![];
        // Ptr(Byref(Void))
        let bytes = vec![4u8, 5u8, 0u8];
        let mut r = make_reader(&bytes, &strings);
        let t = read_il_type(&mut r).unwrap();
        match t {
            PickledILType::Ptr(inner) => match *inner {
                PickledILType::Byref(inner2) => assert_eq!(*inner2, PickledILType::Void),
                other => panic!("unexpected inner: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn il_type_function_pointer() {
        // FunctionPointer { call_conv = Static+Default, args = [Void], ret = Void }
        let strings: Vec<String> = vec![];
        let bytes = vec![
            6u8, // tag 6 = FunctionPointer
            2u8, // has_this = Static
            0u8, // basic = Default
            1u8, // args list len = 1
            0u8, // ILType.Void
            0u8, // ret = ILType.Void
        ];
        let mut r = make_reader(&bytes, &strings);
        let t = read_il_type(&mut r).unwrap();
        match t {
            PickledILType::FunctionPointer(sig) => {
                assert_eq!(
                    sig.call_conv,
                    PickledILCallConv {
                        has_this: PickledILHasThis::Static,
                        basic: PickledILBasicCallConv::Default,
                    }
                );
                assert_eq!(sig.args, vec![PickledILType::Void]);
                assert_eq!(*sig.return_type, PickledILType::Void);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn il_type_modified_carries_required_bool() {
        // Modified { required = true, modifier = (Local,[],"M"), ty = Void }
        let strings = vec!["M".to_string()];
        let mut bytes = vec![8u8]; // tag 8 = Modified
        bytes.push(0x01); // required = true
        bytes.push(0u8); // modifier scope = Local
        bytes.push(0u8); // enclosing list len = 0
        bytes.extend(enc_str_idx(0)); // modifier name
        bytes.push(0u8); // ty = Void
        let mut r = make_reader(&bytes, &strings);
        let t = read_il_type(&mut r).unwrap();
        match t {
            PickledILType::Modified {
                required,
                modifier,
                ty,
            } => {
                assert!(required);
                assert_eq!(modifier.name, "M");
                assert_eq!(*ty, PickledILType::Void);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn il_type_rejects_unknown_tag() {
        let strings: Vec<String> = vec![];
        let bytes = vec![42u8];
        let mut r = make_reader(&bytes, &strings);
        match read_il_type(&mut r) {
            Err(ImportError::UnsupportedPickleTag {
                context: "u_ILType tag",
                tag: 42,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn il_method_ref_attribute_ctor() {
        // Typical attribute ctor:
        //   parent: scope=Local, enclosing=["NS"], name="Attr"
        //   call_conv: Instance + Default
        //   generic_arity: 0
        //   name: ".ctor"
        //   arg_types: []
        //   return_type: Void
        let strings = vec!["NS".to_string(), "Attr".to_string(), ".ctor".to_string()];
        let mut bytes = vec![];
        // parent ILTypeRef
        bytes.push(0u8); // scope = Local
        bytes.push(1u8); // enclosing list len = 1
        bytes.extend(enc_str_idx(0)); // "NS"
        bytes.extend(enc_str_idx(1)); // name "Attr"
        // call_conv = (Instance, Default)
        bytes.push(0u8); // has_this = Instance
        bytes.push(0u8); // basic = Default
        // generic_arity
        bytes.push(0u8);
        // name = ".ctor"
        bytes.extend(enc_str_idx(2));
        // arg_types: empty list
        bytes.push(0u8);
        // return_type: Void
        bytes.push(0u8);
        let mut r = make_reader(&bytes, &strings);
        let mref = read_il_method_ref(&mut r).unwrap();
        assert_eq!(mref.parent.name, "Attr");
        assert_eq!(mref.parent.enclosing, vec!["NS".to_string()]);
        assert_eq!(mref.parent.scope, PickledILScopeRef::Local);
        assert_eq!(mref.call_conv.has_this, PickledILHasThis::Instance);
        assert_eq!(mref.call_conv.basic, PickledILBasicCallConv::Default);
        assert_eq!(mref.generic_arity, 0);
        assert_eq!(mref.name, ".ctor");
        assert!(mref.arg_types.is_empty());
        assert_eq!(mref.return_type, PickledILType::Void);
        assert!(r.is_eof());
    }
}

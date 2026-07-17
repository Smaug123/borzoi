//! Byte-cursor primitives for reading F# pickle streams.
//!
//! Mirrors the `u_*` primitives in
//! `dotnet/fsharp/src/Compiler/TypedTree/TypedTreePickle.fs:366-465`. The
//! encoding is a CLR-style compressed integer with three size tiers, plus
//! length-prefixed UTF-8 strings and length-prefixed byte blobs. All other
//! sub-decoders compose these primitives.
//!
//! The reader owns a borrow into the caller's payload and threads a single
//! `pos` cursor. Over-reads return `ImportError::UnexpectedEndOfStream`
//! with a static context string; the call site supplies the string so the
//! error message names the failing decoder rather than a byte offset.
//!
//! ### The B-stream cursor
//!
//! From F# 9 onwards, the signature pickle carries a sibling byte buffer
//! (the `FSharpSignatureDataB` / `…CompressedDataB` resource) that holds
//! interleaved nullness annotations and the F# 9+ typar-constraint tail
//! (`NotSupportsNull`, `AllowsRefStruct`). FCS reads it through the
//! `u_byteB` / `u_intB` / `u_listB` family at
//! `TypedTreePickle.fs:370-371,388-393,408-421,759-762`; the defining
//! property is that the B reads **never fail on EOF** — they implicitly
//! return `0` (or an empty list) so that legacy assemblies without a
//! B stream decode identically. We mirror that here: the B cursor is
//! optional, and the `*_b` primitives never produce an
//! `UnexpectedEndOfStream` error.
//!
//! ### Phase-2 interning tables
//!
//! FCS's `ReaderState` carries `istrings` / `ipubpaths` / `inlerefs`
//! globally (`TypedTreePickle.fs:194-208`); every `u_string`, `u_pubpath`,
//! and `u_nleref` is a compressed-int index + lookup. We mirror that
//! design: optional table borrows live on the reader and are attached
//! by `attach_tables` between the phase-2 header decode and the phase-1
//! body decode. Calling a table-consuming primitive (`read_string`,
//! `read_pubpath`) before the tables are attached returns
//! `MalformedPickleHeader` — that's a programmer error in the unpickle
//! driver, not a corrupt-input case, but we surface it as a hard error
//! anyway because silent-fallback to "string 0" is exactly what D5
//! forbids.

use crate::error::ImportError;

/// Bound on the phase-1 walk's recursion depth, enforced through
/// [`PickleReader::enter_recursion`] by the self-recursive decoders
/// (`u_ty`, `u_measure_expr`, `u_expr`, `u_ILType`, `u_entity_spec` —
/// every recursion cycle in the walker passes through at least one of
/// them, and they all share this one counter).
///
/// Why a bound at all: the byte-remaining caps on list lengths bound
/// *breadth*, but a malformed stream can encode one recursion level per
/// byte (a run of `u_ty` tag-3 `TType_fun` bytes, or measure tag-1
/// `Inv` bytes), so without a depth bound a few megabytes of resource
/// payload abort the process with a stack overflow — a DoS when the
/// LSP reads a corrupt or hostile DLL. FCS has no such bound, but FCS
/// only re-reads pickles its own compiler wrote.
///
/// Why 1024 — both sides of the bound are *measured*, not guessed:
///
/// - Headroom over reality: the walk depth of valid compiler output
///   scales with source shape, not just source *size* — a curried
///   function's signature is a `TType_fun` chain one level per
///   parameter, so depth tracks the largest curried arity anywhere in
///   the assembly. The `DeepCurry` fixture (200-parameter curried
///   `let`, plausible for machine-generated code) walks at depth ~200;
///   FSharp.Core — much the deepest *hand-written* F# signature we
///   ingest — peaks at 19; MiniLibFs and the corpus pickles peak at 7.
///   1024 clears the generated-code fixture 5× over.
/// - Room below the stack: per-level native-stack cost is toolchain-
///   controlled and large in unoptimised builds (a `u_ty` level
///   measures ~8 KiB, a `u_entity_spec` level upward of 16 KiB), so
///   the walk runs on a dedicated thread whose reservation
///   (`PICKLE_WALK_STACK_BYTES` in `mod.rs`) comfortably covers 1024
///   levels of the heaviest chain. `tests/all/fsharp_pickle_fail_loud.rs`
///   drives both chains to exactly this bound, so "reaching the bound
///   does not overflow the reserved stack" is pinned by CI rather
///   than asserted here.
const MAX_RECURSION_DEPTH: u32 = 1_024;

/// A cursor over the optional sibling B-stream. Kept as a sibling field of
/// `PickleReader` rather than a wrapper so callers can interleave primary
/// and B reads inline (which is how `u_ty` and `u_tyar_constraints` use
/// them).
struct BCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

/// A cursor over a borrowed byte slice. Primitives advance the cursor on
/// success and leave it untouched on failure.
pub(crate) struct PickleReader<'a> {
    bytes: &'a [u8],
    pos: usize,
    b: Option<BCursor<'a>>,
    /// Borrow of the phase-2 strings table, attached after the header
    /// decode. `None` until `attach_tables` is called.
    strings: Option<&'a [String]>,
    /// Borrow of the phase-2 pubpaths table. Each entry is a `Vec<u32>`
    /// of string-table indices.
    pubpaths: Option<&'a [Vec<u32>]>,
    /// Current recursion depth of the phase-1 walk; see
    /// [`MAX_RECURSION_DEPTH`]. Lives on the reader (not
    /// `PhaseOneState`) because `u_ILType` recursion runs on the bare
    /// reader.
    depth: u32,
}

impl<'a> PickleReader<'a> {
    /// Single-stream convenience constructor (no B-stream). Lib callers
    /// use `new_dual` so the B cursor is wired through up front; this
    /// constructor stays for the existing test fixtures, which read
    /// pure-primary phase-2 blobs.
    #[allow(dead_code)]
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self::new_dual(bytes, None)
    }

    /// Construct a reader with both primary and (optional) B-stream
    /// cursors. Matches FCS's `ReaderState { is; isB }` pairing at
    /// `TypedTreePickle.fs:194-208`. When `b` is `None`, the `*_b`
    /// primitives behave as if the B stream were empty (every read returns
    /// `0`).
    pub(crate) fn new_dual(bytes: &'a [u8], b: Option<&'a [u8]>) -> Self {
        Self {
            bytes,
            pos: 0,
            b: b.map(|bs| BCursor { bytes: bs, pos: 0 }),
            strings: None,
            pubpaths: None,
            depth: 0,
        }
    }

    /// Enter one level of decoder recursion; must be paired with
    /// [`exit_recursion`](Self::exit_recursion) after the recursive call
    /// returns (on `Ok` *and* `Err` — the error path unwinds through the
    /// same wrappers, so the pairing holds). Trips
    /// [`ImportError::PickleRecursionLimitExceeded`] past
    /// [`MAX_RECURSION_DEPTH`].
    pub(crate) fn enter_recursion(&mut self, context: &'static str) -> Result<(), ImportError> {
        if self.depth >= MAX_RECURSION_DEPTH {
            return Err(ImportError::PickleRecursionLimitExceeded {
                context,
                limit: MAX_RECURSION_DEPTH,
            });
        }
        self.depth += 1;
        Ok(())
    }

    /// Leave one level of decoder recursion.
    pub(crate) fn exit_recursion(&mut self) {
        debug_assert!(self.depth > 0, "exit_recursion without enter_recursion");
        self.depth = self.depth.saturating_sub(1);
    }

    /// Attach the phase-2 interning tables so subsequent phase-1 reads
    /// can resolve `u_string` / `u_pubpath` indices. Mirrors how FCS
    /// initialises `ReaderState.istrings` / `ipubpaths` after the header
    /// decode (`TypedTreePickle.fs:194-208,1062-1085`).
    #[allow(dead_code)]
    pub(crate) fn attach_tables(&mut self, strings: &'a [String], pubpaths: &'a [Vec<u32>]) {
        self.strings = Some(strings);
        self.pubpaths = Some(pubpaths);
    }

    #[allow(dead_code)]
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    /// Total length of the primary stream. Used by `PhaseOneState::new`
    /// to bound OSGN table sizes against the bytes available — every
    /// linked slot has to be reachable from a body in this stream, so
    /// `ntycons + ntypars + nvals` cannot exceed this length.
    pub(crate) fn total_len(&self) -> usize {
        self.bytes.len()
    }

    /// Bytes remaining in the B cursor (`0` when there is no B stream
    /// or it is already at EOF). Used to bound B-stream list lengths
    /// before allocating capacity — `Vec::with_capacity` from a
    /// malformed compressed-int marker is otherwise an OOM vector.
    pub(crate) fn b_remaining(&self) -> usize {
        match &self.b {
            Some(c) => c.bytes.len().saturating_sub(c.pos),
            None => 0,
        }
    }

    pub(crate) fn is_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    /// Consume one raw byte (matches `u_byte` at `:366`).
    pub(crate) fn read_byte(&mut self, context: &'static str) -> Result<u8, ImportError> {
        if self.pos >= self.bytes.len() {
            return Err(ImportError::UnexpectedEndOfStream { context });
        }
        let b = self.bytes[self.pos];
        self.pos += 1;
        Ok(b)
    }

    /// Consume `n` raw bytes as a no-copy slice (matches the body of
    /// `u_byte_memory` at `:423-425`).
    pub(crate) fn read_bytes(
        &mut self,
        n: usize,
        context: &'static str,
    ) -> Result<&'a [u8], ImportError> {
        if self.bytes.len() - self.pos < n {
            return Err(ImportError::UnexpectedEndOfStream { context });
        }
        let slice = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    /// Compressed unsigned integer (matches `u_int32` at `:395-406`).
    ///
    /// Three-tier encoding written by `p_int32` at `:240-248`:
    /// - `0x00..=0x7F` — literal one-byte value.
    /// - `0x80..=0xBF` — two bytes; combine as `(b0 & 0x7F) << 8 | b1`.
    ///   Encodes values in `0x80..=0x3FFF`.
    /// - `0xFF` — marker followed by four little-endian raw bytes
    ///   (`prim_u_int32`).
    ///
    /// Bytes `0xC0..=0xFE` are never emitted; the F# reader's `assert`
    /// would mis-decode them in release. We reject them outright.
    pub(crate) fn read_uint32(&mut self, context: &'static str) -> Result<u32, ImportError> {
        let b0 = self.read_byte(context)?;
        if b0 <= 0x7F {
            Ok(u32::from(b0))
        } else if b0 <= 0xBF {
            let b1 = self.read_byte(context)?;
            Ok((u32::from(b0 & 0x7F) << 8) | u32::from(b1))
        } else if b0 == 0xFF {
            let raw = self.read_bytes(4, context)?;
            Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
        } else {
            Err(ImportError::UnsupportedPickleTag {
                context,
                tag: u32::from(b0),
            })
        }
    }

    /// Compressed signed integer (matches `u_int` at `:433`, which is
    /// `u_int32` cast to `int`).
    pub(crate) fn read_int32(&mut self, context: &'static str) -> Result<i32, ImportError> {
        self.read_uint32(context).map(|v| v as i32)
    }

    /// Compressed 64-bit integer (matches `u_int64` at `:441-444`): two
    /// consecutive `u_int32` words, low then high.
    pub(crate) fn read_int64(&mut self, context: &'static str) -> Result<i64, ImportError> {
        let lo = self.read_uint32(context)?;
        let hi = self.read_uint32(context)?;
        Ok((i64::from(hi) << 32) | i64::from(lo))
    }

    /// Boolean (matches `u_bool` at `:375-377`): one byte, `0` or `1`.
    /// Anything else is a malformed tag.
    #[allow(dead_code)]
    pub(crate) fn read_bool(&mut self, context: &'static str) -> Result<bool, ImportError> {
        let b = self.read_byte(context)?;
        match b {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(ImportError::UnsupportedPickleTag {
                context,
                tag: u32::from(other),
            }),
        }
    }

    /// UTF-8 string with a compressed-int length prefix (matches
    /// `u_prim_string` at `:429-431`).
    pub(crate) fn read_string_raw(&mut self, context: &'static str) -> Result<String, ImportError> {
        let len = self.read_uint32(context)? as usize;
        let bytes = self.read_bytes(len, context)?;
        String::from_utf8(bytes.to_vec()).map_err(|e| ImportError::MalformedPickleHeader {
            detail: format!("invalid UTF-8 in {context}: {e}"),
        })
    }

    /// Length-prefixed byte blob (matches `u_byte_memory` at `:423-425`).
    /// Returns a no-copy borrow.
    pub(crate) fn read_byte_memory(
        &mut self,
        context: &'static str,
    ) -> Result<&'a [u8], ImportError> {
        let len = self.read_uint32(context)? as usize;
        self.read_bytes(len, context)
    }

    /// Length-prefixed array (matches `u_array` at `:749-751`). The closure
    /// decodes one element; the prefix is a compressed int.
    ///
    /// Defends against malformed length prefixes: a corrupt resource can
    /// advertise an array length of up to `u32::MAX`, and a naive
    /// `Vec::with_capacity(n)` would attempt to reserve gigabytes before
    /// any per-element read fails. The smallest legal element a closure
    /// can consume is one byte (the literal-byte form of `read_uint32`,
    /// or a `read_byte` directly), so the element count is bounded above
    /// by the bytes still in the stream. Counts above that bound are
    /// rejected as `MalformedPickleHeader` before allocation.
    pub(crate) fn read_array<T>(
        &mut self,
        context: &'static str,
        mut elt: impl FnMut(&mut PickleReader<'a>) -> Result<T, ImportError>,
    ) -> Result<Vec<T>, ImportError> {
        let n = self.read_uint32(context)? as usize;
        let remaining = self.remaining();
        if n > remaining {
            return Err(ImportError::MalformedPickleHeader {
                detail: format!("{context}: array length {n} exceeds remaining bytes {remaining}"),
            });
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(elt(self)?);
        }
        Ok(out)
    }

    /// Option (matches `u_option` at `:789-795`): tag byte `0`/`1`, with
    /// `1` followed by the payload.
    #[allow(dead_code)]
    pub(crate) fn read_option<T>(
        &mut self,
        context: &'static str,
        mut payload: impl FnMut(&mut PickleReader<'a>) -> Result<T, ImportError>,
    ) -> Result<Option<T>, ImportError> {
        let tag = self.read_byte(context)?;
        match tag {
            0 => Ok(None),
            1 => Ok(Some(payload(self)?)),
            other => Err(ImportError::UnsupportedPickleTag {
                context,
                tag: u32::from(other),
            }),
        }
    }

    /// Consume one byte from the sibling B stream, returning `0` if the
    /// stream is absent or already at EOF. Matches `u_byteB` at
    /// `TypedTreePickle.fs:370-371`: `if st.isB.IsEOF then 0 else …`. The
    /// `_context` parameter is accepted for symmetry with `read_byte` (so
    /// call sites can document which decoder is consuming the B byte) but
    /// is never reflected in a returned error — B reads cannot fail.
    pub(crate) fn read_byte_b(&mut self, _context: &'static str) -> u8 {
        match self.b.as_mut() {
            Some(b) if b.pos < b.bytes.len() => {
                let v = b.bytes[b.pos];
                b.pos += 1;
                v
            }
            _ => 0,
        }
    }

    /// Compressed unsigned integer from the B stream. Matches `u_int32B`
    /// at `:410-421`. When the B stream is absent, the leading `read_byte_b`
    /// returns `0`, which is the literal-byte encoding of `0` — so the
    /// whole call yields `0` without ever consuming from the (absent) tail.
    pub(crate) fn read_uint32_b(&mut self, context: &'static str) -> u32 {
        let b0 = self.read_byte_b(context);
        if b0 <= 0x7F {
            u32::from(b0)
        } else if b0 <= 0xBF {
            let b1 = self.read_byte_b(context);
            (u32::from(b0 & 0x7F) << 8) | u32::from(b1)
        } else {
            // `0xC0..=0xFF`: FCS asserts `b0 == 0xFF` in debug and then
            // dispatches to `prim_u_int32B`, which consumes 4 raw bytes
            // (separately from `b0`) regardless of the marker value
            // (`TypedTreePickle.fs:404-406,419-421,388-393`). Release
            // builds therefore advance the cursor by exactly 5 bytes for
            // *any* `b0 > 0xBF`. We match that: a reserved-range first
            // byte still consumes the trailing 4-byte body so the cursor
            // stays aligned with what FCS would have read.
            let b1 = self.read_byte_b(context);
            let b2 = self.read_byte_b(context);
            let b3 = self.read_byte_b(context);
            let b4 = self.read_byte_b(context);
            u32::from(b1) | (u32::from(b2) << 8) | (u32::from(b3) << 16) | (u32::from(b4) << 24)
        }
    }

    /// Uncompressed 4-byte little-endian unsigned integer (matches
    /// `prim_u_int32` at `TypedTreePickle.fs:380-385`). Distinct from
    /// `read_uint32`, which decodes the variable-length compressed form;
    /// FCS uses the raw 4-byte form inside `u_lazy`'s 7-word framing
    /// header and inside the `0xFF`-marker tail of the compressed form.
    #[allow(dead_code)]
    pub(crate) fn read_uint32_le(&mut self, context: &'static str) -> Result<u32, ImportError> {
        let raw = self.read_bytes(4, context)?;
        Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }

    /// Length-prefixed list (matches `u_list` at
    /// `TypedTreePickle.fs:755-757`, equivalently `u_array` with a
    /// compressed-int prefix at `:749-751`). Identical wire format to
    /// `read_array`; the duplicate spelling exists so call sites can
    /// document whether the FCS source called `u_list` or `u_array`,
    /// matching the upstream idiom of "list ≈ array in everything but
    /// the collection type." Bounds the length against remaining bytes
    /// before allocating, for the same reason `read_array` does.
    #[allow(dead_code)]
    pub(crate) fn read_list<T>(
        &mut self,
        context: &'static str,
        elt: impl FnMut(&mut PickleReader<'a>) -> Result<T, ImportError>,
    ) -> Result<Vec<T>, ImportError> {
        self.read_array(context, elt)
    }

    /// `u_string` (`TypedTreePickle.fs:831`): compressed-int index into
    /// the phase-2 strings table, dereferenced. Requires
    /// `attach_tables` to have been called first.
    #[allow(dead_code)]
    pub(crate) fn read_string(&mut self, context: &'static str) -> Result<String, ImportError> {
        let idx = self.read_uint32(context)?;
        let table = self.strings.ok_or(ImportError::MalformedPickleHeader {
            detail: format!("{context}: strings table not attached"),
        })?;
        table
            .get(idx as usize)
            .cloned()
            .ok_or(ImportError::DanglingPickleRef {
                kind: "string",
                index: idx,
            })
    }

    /// Like `read_string` but returns just the index without resolving
    /// — useful when the call site wants to record the raw wire value
    /// (e.g. for storing in a `PickledRange.file: u32` field that 6c
    /// resolves lazily). Still validates the index is in range so
    /// downstream consumers can assume the lookup will succeed.
    #[allow(dead_code)]
    pub(crate) fn read_string_index(&mut self, context: &'static str) -> Result<u32, ImportError> {
        let idx = self.read_uint32(context)?;
        let table = self.strings.ok_or(ImportError::MalformedPickleHeader {
            detail: format!("{context}: strings table not attached"),
        })?;
        if (idx as usize) >= table.len() {
            return Err(ImportError::DanglingPickleRef {
                kind: "string",
                index: idx,
            });
        }
        Ok(idx)
    }

    /// `u_pubpath` (`:861`): compressed-int index into the phase-2
    /// pubpaths table, returning the pre-resolved path of string
    /// indices.
    #[allow(dead_code)]
    pub(crate) fn read_pubpath(&mut self, context: &'static str) -> Result<Vec<u32>, ImportError> {
        let idx = self.read_uint32(context)?;
        let table = self.pubpaths.ok_or(ImportError::MalformedPickleHeader {
            detail: format!("{context}: pubpaths table not attached"),
        })?;
        table
            .get(idx as usize)
            .cloned()
            .ok_or(ImportError::DanglingPickleRef {
                kind: "pubpath",
                index: idx,
            })
    }

    /// `u_space n` (`TypedTreePickle.fs:456-461`): consume exactly `n`
    /// reserved bytes, each of which must be `0`. FCS warns on a
    /// non-zero byte and continues; we error, per D5 (silent-fallback
    /// is exactly what we reject).
    #[allow(dead_code)]
    pub(crate) fn read_space(
        &mut self,
        n: usize,
        context: &'static str,
    ) -> Result<(), ImportError> {
        for _ in 0..n {
            let b = self.read_byte(context)?;
            if b != 0 {
                return Err(ImportError::MalformedPickleHeader {
                    detail: format!("{context}: reserved-space byte was {b:#x}, expected 0"),
                });
            }
        }
        Ok(())
    }

    /// `u_used_space1` (`TypedTreePickle.fs:464-475`): future-extension
    /// framing — a single tag byte. `0` means absent (no body); `1`
    /// means present, then run the closure and consume one more
    /// reserved-zero byte. Any other tag is malformed (FCS warns and
    /// returns `None`; we reject — silent-fallback would mask a
    /// newer-format-than-us divergence).
    #[allow(dead_code)]
    pub(crate) fn read_used_space1<T>(
        &mut self,
        context: &'static str,
        mut f: impl FnMut(&mut PickleReader<'a>) -> Result<T, ImportError>,
    ) -> Result<Option<T>, ImportError> {
        let tag = self.read_byte(context)?;
        match tag {
            0 => Ok(None),
            1 => {
                let v = f(self)?;
                self.read_space(1, context)?;
                Ok(Some(v))
            }
            other => Err(ImportError::MalformedPickleHeader {
                detail: format!(
                    "{context}: u_used_space1 unexpected tag {other:#x} (expected 0 or 1)",
                ),
            }),
        }
    }

    /// Assert the cursor is at end of input. Used at the end of phase-2
    /// decode to pin that the entire stream was consumed.
    pub(crate) fn expect_eof(&self, context: &'static str) -> Result<(), ImportError> {
        if self.is_eof() {
            Ok(())
        } else {
            Err(ImportError::MalformedPickleHeader {
                detail: format!(
                    "{context}: {} trailing bytes after expected EOF (cursor at {}/{})",
                    self.remaining(),
                    self.pos,
                    self.bytes.len()
                ),
            })
        }
    }

    /// Assert the B cursor is at end of input (or absent). Mirrors
    /// [`expect_eof`] for the sibling phase-1 B stream — D5 says we
    /// fail loud on any wire content we don't understand, so trailing
    /// B bytes after the walker finishes have to surface as an error
    /// rather than be silently dropped.
    pub(crate) fn expect_eof_b(&self, context: &'static str) -> Result<(), ImportError> {
        let Some(b) = &self.b else {
            return Ok(());
        };
        if b.pos >= b.bytes.len() {
            Ok(())
        } else {
            Err(ImportError::MalformedPickleHeader {
                detail: format!(
                    "{context}: {} trailing bytes in B stream after expected EOF (cursor at {}/{})",
                    b.bytes.len() - b.pos,
                    b.pos,
                    b.bytes.len()
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_byte_advances_and_errors_on_eof() {
        let mut r = PickleReader::new(&[0x42, 0xAB]);
        assert_eq!(r.read_byte("t").unwrap(), 0x42);
        assert_eq!(r.read_byte("t").unwrap(), 0xAB);
        assert!(matches!(
            r.read_byte("ctx"),
            Err(ImportError::UnexpectedEndOfStream { context: "ctx" })
        ));
    }

    #[test]
    fn read_uint32_one_byte_literal_range() {
        for v in [0u32, 1, 0x40, 0x7F] {
            let bytes = [v as u8];
            let mut r = PickleReader::new(&bytes);
            assert_eq!(r.read_uint32("t").unwrap(), v);
            assert!(r.is_eof());
        }
    }

    #[test]
    fn read_uint32_two_byte_range() {
        // 0x80: encoded as 0x80, 0x80 → (0x00 << 8) | 0x80 = 0x80
        let bytes = [0x80, 0x80];
        let mut r = PickleReader::new(&bytes);
        assert_eq!(r.read_uint32("t").unwrap(), 0x80);

        // 0x3FFF: encoded as 0xBF, 0xFF → ((0xBF & 0x7F) << 8) | 0xFF = 0x3FFF
        let bytes = [0xBF, 0xFF];
        let mut r = PickleReader::new(&bytes);
        assert_eq!(r.read_uint32("t").unwrap(), 0x3FFF);

        // 0x1234: 0x80 | (0x1234 >> 8) = 0x92, then 0x34
        let bytes = [0x92, 0x34];
        let mut r = PickleReader::new(&bytes);
        assert_eq!(r.read_uint32("t").unwrap(), 0x1234);
    }

    #[test]
    fn read_uint32_marker_form() {
        // 0xFF then 4 LE bytes = 0x00BADA55
        let bytes = [0xFF, 0x55, 0xDA, 0xBA, 0x00];
        let mut r = PickleReader::new(&bytes);
        assert_eq!(r.read_uint32("t").unwrap(), 0x00BADA55);

        // i32::MAX = 0x7FFF_FFFF
        let bytes = [0xFF, 0xFF, 0xFF, 0xFF, 0x7F];
        let mut r = PickleReader::new(&bytes);
        assert_eq!(r.read_uint32("t").unwrap(), 0x7FFF_FFFFu32);

        // i32::MIN bit pattern = 0x8000_0000 → cast to i32 yields i32::MIN
        let bytes = [0xFF, 0x00, 0x00, 0x00, 0x80];
        let mut r = PickleReader::new(&bytes);
        let v = r.read_int32("t").unwrap();
        assert_eq!(v, i32::MIN);
    }

    #[test]
    fn read_uint32_rejects_reserved_first_byte() {
        // 0xC0..=0xFE are never emitted by p_int32; we reject.
        for b in [0xC0u8, 0xD5, 0xFE] {
            let bytes = [b];
            let mut r = PickleReader::new(&bytes);
            match r.read_uint32("ctx") {
                Err(ImportError::UnsupportedPickleTag {
                    context: "ctx",
                    tag,
                }) => {
                    assert_eq!(tag, u32::from(b));
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn read_uint32_two_byte_eof() {
        let bytes = [0x80];
        let mut r = PickleReader::new(&bytes);
        assert!(matches!(
            r.read_uint32("ctx"),
            Err(ImportError::UnexpectedEndOfStream { context: "ctx" })
        ));
    }

    #[test]
    fn read_uint32_marker_form_eof() {
        let bytes = [0xFF, 0x01, 0x02];
        let mut r = PickleReader::new(&bytes);
        assert!(matches!(
            r.read_uint32("ctx"),
            Err(ImportError::UnexpectedEndOfStream { context: "ctx" })
        ));
    }

    #[test]
    fn read_int64_word_order_low_then_high() {
        // 0x0000_0001_0000_0002: low word 0x0000_0002, high word 0x0000_0001.
        // Each word is itself compressed; here both fit in one byte.
        let bytes = [0x02, 0x01];
        let mut r = PickleReader::new(&bytes);
        assert_eq!(r.read_int64("t").unwrap(), 0x0000_0001_0000_0002i64);
    }

    #[test]
    fn read_int64_negative_round_trip() {
        // -1 in i64 = 0xFFFF_FFFF_FFFF_FFFF: low = 0xFFFF_FFFF (5-byte form),
        // high = 0xFFFF_FFFF (5-byte form).
        let mut buf = Vec::new();
        // low word
        buf.push(0xFF);
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        // high word
        buf.push(0xFF);
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let mut r = PickleReader::new(&buf);
        assert_eq!(r.read_int64("t").unwrap(), -1i64);
    }

    #[test]
    fn read_bool_accepts_0_and_1_only() {
        let bytes = [0, 1];
        let mut r = PickleReader::new(&bytes);
        assert!(!r.read_bool("t").unwrap());
        assert!(r.read_bool("t").unwrap());

        let bytes = [2];
        let mut r = PickleReader::new(&bytes);
        assert!(matches!(
            r.read_bool("ctx"),
            Err(ImportError::UnsupportedPickleTag {
                context: "ctx",
                tag: 2
            })
        ));
    }

    #[test]
    fn read_string_raw_round_trips() {
        // "hello" — length 5, then ASCII bytes
        let bytes = [0x05, b'h', b'e', b'l', b'l', b'o'];
        let mut r = PickleReader::new(&bytes);
        assert_eq!(r.read_string_raw("t").unwrap(), "hello");
    }

    #[test]
    fn read_string_raw_empty() {
        let bytes = [0x00];
        let mut r = PickleReader::new(&bytes);
        assert_eq!(r.read_string_raw("t").unwrap(), "");
    }

    #[test]
    fn read_string_raw_rejects_bad_utf8() {
        let bytes = [0x02, 0xFF, 0xFE];
        let mut r = PickleReader::new(&bytes);
        assert!(matches!(
            r.read_string_raw("t"),
            Err(ImportError::MalformedPickleHeader { .. })
        ));
    }

    #[test]
    fn read_array_rejects_length_exceeding_remaining_bytes() {
        // Malicious input: advertise an array of 1 million elements when
        // only 5 bytes of payload remain. Naive `Vec::with_capacity(1M)`
        // would reserve 4 MB; a marker-form `0xFF`-tagged length could go
        // up to 4 GB. Cap the count before allocation.
        let mut bytes = Vec::new();
        bytes.push(0xFF); // marker form
        bytes.extend_from_slice(&1_000_000u32.to_le_bytes());
        bytes.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05]);
        let mut r = PickleReader::new(&bytes);
        match r.read_array::<u32>("ctx", |r| r.read_uint32("e")) {
            Err(ImportError::MalformedPickleHeader { detail }) => {
                assert!(
                    detail.contains("array length 1000000"),
                    "detail should name the offending count: {detail}",
                );
            }
            other => panic!("expected MalformedPickleHeader, got {other:?}"),
        }
    }

    #[test]
    fn read_array_at_capacity_succeeds() {
        // The cap is *up to* `remaining()` bytes, not strictly less than.
        // A 3-byte tail can legitimately encode an array of three
        // single-byte elements.
        let bytes = [0x03, 0x01, 0x02, 0x03];
        let mut r = PickleReader::new(&bytes);
        let v: Vec<u32> = r.read_array("t", |r| r.read_uint32("e")).unwrap();
        assert_eq!(v, vec![1, 2, 3]);
    }

    #[test]
    fn read_array_empty_single_many() {
        let bytes = [0x00];
        let mut r = PickleReader::new(&bytes);
        let v: Vec<u32> = r.read_array("t", |r| r.read_uint32("e")).unwrap();
        assert_eq!(v, Vec::<u32>::new());

        let bytes = [0x01, 0x05];
        let mut r = PickleReader::new(&bytes);
        let v: Vec<u32> = r.read_array("t", |r| r.read_uint32("e")).unwrap();
        assert_eq!(v, vec![5]);

        let bytes = [0x03, 0x01, 0x02, 0x03];
        let mut r = PickleReader::new(&bytes);
        let v: Vec<u32> = r.read_array("t", |r| r.read_uint32("e")).unwrap();
        assert_eq!(v, vec![1, 2, 3]);
    }

    #[test]
    fn read_option_none_some() {
        let bytes = [0x00];
        let mut r = PickleReader::new(&bytes);
        let v: Option<u32> = r.read_option("t", |r| r.read_uint32("e")).unwrap();
        assert_eq!(v, None);

        let bytes = [0x01, 0x42];
        let mut r = PickleReader::new(&bytes);
        let v: Option<u32> = r.read_option("t", |r| r.read_uint32("e")).unwrap();
        assert_eq!(v, Some(0x42));

        let bytes = [0x02];
        let mut r = PickleReader::new(&bytes);
        assert!(matches!(
            r.read_option::<u32>("ctx", |r| r.read_uint32("e")),
            Err(ImportError::UnsupportedPickleTag {
                context: "ctx",
                tag: 2
            })
        ));
    }

    #[test]
    fn read_byte_b_returns_zero_when_b_absent() {
        let bytes = [0xAAu8];
        let mut r = PickleReader::new_dual(&bytes, None);
        assert_eq!(r.read_byte_b("ctx"), 0);
        assert_eq!(r.read_byte_b("ctx"), 0);
        // Primary cursor untouched.
        assert_eq!(r.read_byte("ctx").unwrap(), 0xAA);
    }

    #[test]
    fn read_byte_b_returns_zero_when_b_empty() {
        let bytes = [0u8];
        let bb: [u8; 0] = [];
        let mut r = PickleReader::new_dual(&bytes, Some(&bb));
        assert_eq!(r.read_byte_b("ctx"), 0);
        assert_eq!(r.read_byte_b("ctx"), 0);
    }

    #[test]
    fn read_byte_b_consumes_then_returns_zero_at_eof() {
        let bytes = [0u8];
        let bb = [0x11u8, 0x22];
        let mut r = PickleReader::new_dual(&bytes, Some(&bb));
        assert_eq!(r.read_byte_b("ctx"), 0x11);
        assert_eq!(r.read_byte_b("ctx"), 0x22);
        // Implicit-zero past EOF.
        assert_eq!(r.read_byte_b("ctx"), 0);
        assert_eq!(r.read_byte_b("ctx"), 0);
    }

    #[test]
    fn read_uint32_b_returns_zero_when_b_absent() {
        let bytes = [0u8];
        let mut r = PickleReader::new_dual(&bytes, None);
        assert_eq!(r.read_uint32_b("ctx"), 0);
    }

    #[test]
    fn read_uint32_b_one_and_two_byte_forms() {
        // Literal one-byte: 0x42.
        let bytes = [0u8];
        let bb = [0x42u8];
        let mut r = PickleReader::new_dual(&bytes, Some(&bb));
        assert_eq!(r.read_uint32_b("ctx"), 0x42);

        // Two-byte: 0x80 | (0x1234 >> 8) = 0x92, then 0x34.
        let bb = [0x92u8, 0x34];
        let mut r = PickleReader::new_dual(&bytes, Some(&bb));
        assert_eq!(r.read_uint32_b("ctx"), 0x1234);
    }

    #[test]
    fn read_uint32_b_two_byte_form_truncated_yields_zero_tail() {
        // Leading 0x80 with no second byte: FCS would read 0 as the
        // implicit tail (b0 & 0x7F) << 8 | 0 = 0.
        let bytes = [0u8];
        let bb = [0x80u8];
        let mut r = PickleReader::new_dual(&bytes, Some(&bb));
        assert_eq!(r.read_uint32_b("ctx"), 0);
    }

    #[test]
    fn read_uint32_b_ff_marker_consumes_marker_plus_four() {
        // Canonical FF-marker encoding: marker byte + 4 LE bytes for the
        // value 0x01020304. Total consumption = 5 bytes.
        let bytes = [0u8];
        let bb = [0xFFu8, 0x04, 0x03, 0x02, 0x01, 0x42];
        let mut r = PickleReader::new_dual(&bytes, Some(&bb));
        assert_eq!(r.read_uint32_b("ctx"), 0x0102_0304);
        // The trailing 0x42 is still in the cursor — proves we consumed
        // exactly 5 bytes, not more.
        assert_eq!(r.read_byte_b("trailer"), 0x42);
    }

    #[test]
    fn read_uint32_b_reserved_marker_still_consumes_four_bytes() {
        // Pin the regression: a reserved-range first byte (0xC0..=0xFE)
        // must still consume the trailing 4 bytes so the B cursor stays
        // aligned with FCS's `prim_u_int32B` behaviour. If we returned
        // `b0` without consuming the body, the next read would see the
        // first body byte instead of the byte that follows the body.
        let bytes = [0u8];
        // 0xC5 is a reserved marker; followed by 0x04 0x03 0x02 0x01
        // (a valid LE 0x01020304), then a sentinel 0x77.
        let bb = [0xC5u8, 0x04, 0x03, 0x02, 0x01, 0x77];
        let mut r = PickleReader::new_dual(&bytes, Some(&bb));
        // Behaviour matches FCS release: `b0` is discarded, the 4
        // trailing bytes become the value.
        assert_eq!(r.read_uint32_b("ctx"), 0x0102_0304);
        // Critically, the sentinel is what's next — not a body byte.
        assert_eq!(r.read_byte_b("trailer"), 0x77);
    }

    #[test]
    fn read_uint32_b_reserved_marker_near_eof_still_advances() {
        // Reserved marker with no trailing body bytes: the four
        // `read_byte_b` calls each saturate to 0, yielding the value 0,
        // but the cursor advances to EOF rather than leaving a stale
        // marker byte behind.
        let bytes = [0u8];
        let bb = [0xD0u8];
        let mut r = PickleReader::new_dual(&bytes, Some(&bb));
        assert_eq!(r.read_uint32_b("ctx"), 0);
    }

    #[test]
    fn read_uint32_le_round_trips() {
        let v = 0x0102_0304u32;
        let bytes = v.to_le_bytes();
        let mut r = PickleReader::new(&bytes);
        assert_eq!(r.read_uint32_le("t").unwrap(), v);
        assert!(r.is_eof());
    }

    #[test]
    fn read_uint32_le_errors_on_short_input() {
        let bytes = [0x01u8, 0x02, 0x03];
        let mut r = PickleReader::new(&bytes);
        assert!(matches!(
            r.read_uint32_le("ctx"),
            Err(ImportError::UnexpectedEndOfStream { context: "ctx" })
        ));
    }

    #[test]
    fn read_list_round_trips_like_array() {
        let bytes = [0x03u8, 0x05, 0x06, 0x07];
        let mut r = PickleReader::new(&bytes);
        let v: Vec<u8> = r.read_list("t", |r| r.read_byte("e")).unwrap();
        assert_eq!(v, vec![5, 6, 7]);
    }

    #[test]
    fn read_string_errors_when_table_not_attached() {
        let bytes = [0x00u8];
        let mut r = PickleReader::new(&bytes);
        match r.read_string("ctx") {
            Err(ImportError::MalformedPickleHeader { detail }) => {
                assert!(detail.contains("strings table not attached"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn read_string_resolves_against_attached_table() {
        let bytes = [0x01u8]; // index 1
        let mut r = PickleReader::new(&bytes);
        let strings = vec!["zero".to_string(), "one".to_string(), "two".to_string()];
        let pubpaths: Vec<Vec<u32>> = vec![];
        r.attach_tables(&strings, &pubpaths);
        assert_eq!(r.read_string("ctx").unwrap(), "one");
    }

    #[test]
    fn read_string_errors_on_out_of_range_index() {
        let bytes = [0x05u8]; // index 5, table has 2 entries
        let mut r = PickleReader::new(&bytes);
        let strings = vec!["a".to_string(), "b".to_string()];
        let pubpaths: Vec<Vec<u32>> = vec![];
        r.attach_tables(&strings, &pubpaths);
        match r.read_string("ctx") {
            Err(ImportError::DanglingPickleRef {
                kind: "string",
                index: 5,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn read_string_index_returns_raw_index_after_bounds_check() {
        let bytes = [0x02u8];
        let mut r = PickleReader::new(&bytes);
        let strings = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let pubpaths: Vec<Vec<u32>> = vec![];
        r.attach_tables(&strings, &pubpaths);
        assert_eq!(r.read_string_index("ctx").unwrap(), 2);
    }

    #[test]
    fn read_string_index_errors_on_out_of_range() {
        let bytes = [0x09u8];
        let mut r = PickleReader::new(&bytes);
        let strings = vec!["a".to_string()];
        let pubpaths: Vec<Vec<u32>> = vec![];
        r.attach_tables(&strings, &pubpaths);
        match r.read_string_index("ctx") {
            Err(ImportError::DanglingPickleRef {
                kind: "string",
                index: 9,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn read_pubpath_resolves_against_attached_table() {
        let bytes = [0x00u8]; // index 0
        let mut r = PickleReader::new(&bytes);
        let strings: Vec<String> = vec![];
        let pubpaths = vec![vec![1u32, 2, 3], vec![4u32]];
        r.attach_tables(&strings, &pubpaths);
        assert_eq!(r.read_pubpath("ctx").unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn read_pubpath_errors_on_out_of_range_index() {
        let bytes = [0x09u8];
        let mut r = PickleReader::new(&bytes);
        let strings: Vec<String> = vec![];
        let pubpaths = vec![vec![1u32]];
        r.attach_tables(&strings, &pubpaths);
        match r.read_pubpath("ctx") {
            Err(ImportError::DanglingPickleRef {
                kind: "pubpath",
                index: 9,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn read_space_consumes_only_zero_bytes() {
        let bytes = [0u8, 0, 0, 0x42];
        let mut r = PickleReader::new(&bytes);
        r.read_space(3, "ctx").unwrap();
        // 4th byte is still there.
        assert_eq!(r.read_byte("t").unwrap(), 0x42);
    }

    #[test]
    fn read_space_errors_on_non_zero() {
        let bytes = [0u8, 0xAA];
        let mut r = PickleReader::new(&bytes);
        match r.read_space(2, "ctx") {
            Err(ImportError::MalformedPickleHeader { detail }) => {
                assert!(detail.contains("0xaa"), "detail: {detail}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn read_used_space1_absent_returns_none_and_consumes_one_byte() {
        let bytes = [0u8, 0xAB];
        let mut r = PickleReader::new(&bytes);
        let v: Option<u8> = r.read_used_space1("ctx", |r| r.read_byte("e")).unwrap();
        assert_eq!(v, None);
        // Trailing 0xAB is still there.
        assert_eq!(r.read_byte("t").unwrap(), 0xAB);
    }

    #[test]
    fn read_used_space1_present_runs_closure_then_consumes_one_reserved_zero() {
        // tag = 1, closure reads 0x42, then one reserved zero, then sentinel.
        let bytes = [1u8, 0x42, 0x00, 0x99];
        let mut r = PickleReader::new(&bytes);
        let v: Option<u8> = r.read_used_space1("ctx", |r| r.read_byte("e")).unwrap();
        assert_eq!(v, Some(0x42));
        assert_eq!(r.read_byte("t").unwrap(), 0x99);
    }

    #[test]
    fn read_used_space1_errors_on_unexpected_tag() {
        let bytes = [2u8];
        let mut r = PickleReader::new(&bytes);
        match r.read_used_space1::<u8>("ctx", |_| Ok(0)) {
            Err(ImportError::MalformedPickleHeader { detail }) => {
                assert!(detail.contains("u_used_space1"), "detail: {detail}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn read_used_space1_errors_on_non_zero_reserved_byte() {
        let bytes = [1u8, 0x42, 0xFF];
        let mut r = PickleReader::new(&bytes);
        match r.read_used_space1::<u8>("ctx", |r| r.read_byte("e")) {
            Err(ImportError::MalformedPickleHeader { detail }) => {
                assert!(detail.contains("reserved-space"), "detail: {detail}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn expect_eof_at_end_ok_with_trailing_bytes_errors() {
        let bytes = [];
        let r = PickleReader::new(&bytes);
        assert!(r.expect_eof("end").is_ok());

        let bytes = [0xFFu8];
        let r = PickleReader::new(&bytes);
        assert!(matches!(
            r.expect_eof("end"),
            Err(ImportError::MalformedPickleHeader { .. })
        ));
    }

    #[test]
    fn expect_eof_b_succeeds_when_b_absent_or_drained() {
        // No B stream attached.
        let bytes = [];
        let r = PickleReader::new(&bytes);
        assert!(r.expect_eof_b("end").is_ok());

        // B stream attached but already at EOF.
        let primary = [];
        let bb: [u8; 0] = [];
        let r = PickleReader::new_dual(&primary, Some(&bb));
        assert!(r.expect_eof_b("end").is_ok());
    }

    #[test]
    fn expect_eof_b_errors_on_trailing_b_bytes() {
        let primary = [];
        let bb = [0x01u8, 0x02];
        let r = PickleReader::new_dual(&primary, Some(&bb));
        match r.expect_eof_b("phase 1") {
            Err(ImportError::MalformedPickleHeader { detail }) => {
                assert!(detail.contains("B stream"), "detail: {detail}");
                assert!(detail.contains("phase 1"), "detail: {detail}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}

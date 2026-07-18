//! Computing *where a referenced-assembly member is defined in source* — the
//! pure core of go-to-definition into a `.dll`.
//!
//! Given a referenced method's owning DLL bytes and its `MethodDef` metadata
//! token, [`definition_source`] reads the DLL's embedded portable PDB and
//! produces a [`DefinitionSource`] describing how to obtain the source:
//! [`DefinitionSource::Embedded`] when the PDB embeds the text (offline), or
//! [`DefinitionSource::Remote`] with a SourceLink URL when it does not.
//!
//! This is deliberately *pure* (no IO beyond the bytes already in hand, no
//! network): it computes a description of the source, and the handler shell acts
//! on it (writing a temp doc for embedded text, or — behind a feature —
//! performing the SourceLink fetch). Modelling the network step as a value the
//! core emits, rather than an effect the core performs, is what lets the whole
//! mapping be tested in a sandbox with no network.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use borzoi_assembly::pdb::{
    PdbError, PortablePdb, SequencePoint, codeview_pdb_reference, embedded_portable_pdb,
};

/// `MethodDef` metadata-table tag in a token's high byte.
const METHOD_DEF_TAG: u32 = 0x06;

/// Where a referenced method's source lives. `line`/`column` are 1-based (the
/// portable-PDB convention); the caller converts to the editor's 0-based
/// positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionSource {
    /// The source text is embedded in the PDB — usable offline. `document` is
    /// the file name the compiler recorded (for display / the temp-doc name).
    Embedded {
        document: String,
        text: String,
        line: u32,
        column: u32,
    },
    /// The source is not embedded; `url` is its SourceLink location (e.g. a
    /// GitHub raw URL). Fetching it is an effect the handler performs (or
    /// surfaces) — never done here.
    Remote {
        document: String,
        url: String,
        line: u32,
        column: u32,
    },
}

/// *Where* a referenced definition lives — its document and 1-based position —
/// independent of whether the source text itself can be obtained. A PDB that
/// carries sequence points but neither embedded source nor SourceLink (FsUnit's
/// shipped `.pdb`) still yields this, so the LSP can *say* "defined in
/// FsUnitTyped.fs, line 10" even when it can't open the file. `document` is the
/// path the compiler recorded (typically a build-machine absolute path); the
/// caller takes its file name for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionDocument {
    pub document: String,
    pub line: u32,
    pub column: u32,
}

/// Resolve a referenced method's [`DefinitionSource`] from its owning DLL's
/// bytes and `MethodDef` metadata token (`borzoi_assembly::MethodLike::metadata_token`).
///
/// `Ok(None)` — not a `MethodDef` token, the DLL has no embedded PDB, the method
/// carries no sequence points, or its document is neither embedded nor mapped by
/// SourceLink. `Err` only on a structurally malformed PDB.
///
/// Pure: reads only `dll_bytes`; no filesystem, no network.
pub fn definition_source(
    dll_bytes: &[u8],
    metadata_token: u32,
) -> Result<Option<DefinitionSource>, PdbError> {
    // A non-`MethodDef` token is a cheap no-op regardless of the bytes — reject
    // it before reading the PE, so e.g. a TypeDef token never turns malformed
    // `dll_bytes` into an `Err` (the documented `Ok(None)` contract).
    if metadata_token >> 24 != METHOD_DEF_TAG {
        return Ok(None);
    }
    match embedded_portable_pdb(dll_bytes)? {
        Some(image) => definition_source_in_pdb(&image, metadata_token),
        None => Ok(None), // No embedded PDB → caller may try a sidecar.
    }
}

/// Resolve a referenced method's [`DefinitionSource`] from an already-obtained
/// portable-PDB *metadata image* (`pdb_image`) and its `MethodDef` token —
/// whether that image came from the DLL's *embedded* PDB or a *sidecar* `.pdb`
/// (both are the same `BSJB`-rooted container). [`definition_source`] is the
/// embedded convenience wrapper; the handler shell, which also handles the
/// sidecar case, calls this directly so it controls where the image comes from.
///
/// `Ok(None)` — not a `MethodDef` token, the method carries no sequence points,
/// or its document is neither embedded nor mapped by SourceLink. `Err` only on
/// a structurally malformed PDB. Pure: reads only `pdb_image`.
pub fn definition_source_in_pdb(
    pdb_image: &[u8],
    metadata_token: u32,
) -> Result<Option<DefinitionSource>, PdbError> {
    // Only `MethodDef` tokens index the PDB's parallel `MethodDebugInformation`.
    if metadata_token >> 24 != METHOD_DEF_TAG {
        return Ok(None);
    }
    let rid = metadata_token & 0x00FF_FFFF;

    let pdb = PortablePdb::read(pdb_image)?;
    let Some(point) = pdb.method_first_sequence_point(rid)? else {
        return Ok(None); // No source mapping for this method.
    };
    source_for_point(&pdb, point)
}

/// Resolve a referenced *entity*'s (type or module) [`DefinitionSource`] from
/// its owning DLL's bytes and the `MethodDef` tokens of the methods physically
/// declared on it ([`borzoi_assembly::Entity::method_def_tokens`]).
///
/// A portable PDB maps source positions per *method*, never per type, so
/// go-to-definition on a type/module navigates to one of its methods: the
/// **lowest-rid** method that carries a sequence point — the type's
/// first-declared executable member, whose first sequence point sits at (or
/// just below) the declaration. Passing the *physical* token set (not the
/// resolution-oriented `members`) matters: a single-case union's only
/// source-mapped method is the user-written property getter the member
/// projection drops, and it is exactly what lets the type be navigated.
///
/// `Ok(None)` — no candidate is a `MethodDef`, the DLL has no embedded PDB, or
/// none of the candidates carry a sequence point (D5: say nothing rather than
/// guess). `Err` only on a structurally malformed PDB.
///
/// Pure: reads only `dll_bytes`; no filesystem, no network.
pub fn entity_definition_source(
    dll_bytes: &[u8],
    method_tokens: &[u32],
) -> Result<Option<DefinitionSource>, PdbError> {
    match embedded_portable_pdb(dll_bytes)? {
        Some(image) => entity_definition_source_in_pdb(&image, method_tokens),
        None => Ok(None), // No embedded PDB → caller may try a sidecar.
    }
}

/// The image-taking counterpart of [`entity_definition_source`], mirroring
/// [`definition_source_in_pdb`]: navigate a referenced *entity* (type/module) to
/// its lowest-rid source-mapped method given an already-obtained portable-PDB
/// image (embedded or sidecar). `Ok(None)` when no candidate carries a sequence
/// point. Pure: reads only `pdb_image`.
pub fn entity_definition_source_in_pdb(
    pdb_image: &[u8],
    method_tokens: &[u32],
) -> Result<Option<DefinitionSource>, PdbError> {
    let pdb = PortablePdb::read(pdb_image)?;
    match entity_first_sequence_point(&pdb, method_tokens)? {
        Some(point) => source_for_point(&pdb, point),
        None => Ok(None), // No candidate carries source.
    }
}

/// The first [`SequencePoint`] of an entity's lowest-rid source-mapped method —
/// the navigation target shared by [`entity_definition_source_in_pdb`] and
/// [`entity_definition_document_in_pdb`].
///
/// Lowest rid = earliest in the type's `MethodList`, i.e. the first method
/// declared on the type; for an F# type/module its first sequence point lands at
/// the declaration. Picking one whole method (rather than the min source line
/// across methods) keeps the chosen document and position self-consistent even
/// if a type's methods span several documents (C# `partial`), per D5.
fn entity_first_sequence_point(
    pdb: &PortablePdb<'_>,
    method_tokens: &[u32],
) -> Result<Option<SequencePoint>, PdbError> {
    let mut best: Option<u32> = None;
    for &token in method_tokens {
        if token >> 24 != METHOD_DEF_TAG {
            continue; // Not a MethodDef — cannot index the PDB.
        }
        let rid = token & 0x00FF_FFFF;
        if best.is_some_and(|b| rid >= b) {
            continue; // Already have a smaller (or equal) candidate.
        }
        if pdb.method_first_sequence_point(rid)?.is_some() {
            best = Some(rid);
        }
    }
    let Some(rid) = best else {
        return Ok(None);
    };
    Ok(Some(pdb.method_first_sequence_point(rid)?.expect(
        "the winning rid was just confirmed to have a sequence point",
    )))
}

/// The [`DefinitionDocument`] for a referenced **method**: the document + 1-based
/// position of its first sequence point, from an already-obtained PDB image —
/// *without* needing the source itself to be embedded or SourceLink-mapped.
/// `Ok(None)` for a non-`MethodDef` token or a method with no sequence point.
/// Pure: reads only `pdb_image`.
pub fn definition_document_in_pdb(
    pdb_image: &[u8],
    metadata_token: u32,
) -> Result<Option<DefinitionDocument>, PdbError> {
    if metadata_token >> 24 != METHOD_DEF_TAG {
        return Ok(None);
    }
    let rid = metadata_token & 0x00FF_FFFF;
    let pdb = PortablePdb::read(pdb_image)?;
    match pdb.method_first_sequence_point(rid)? {
        Some(point) => Ok(Some(document_for_point(&pdb, point)?)),
        None => Ok(None),
    }
}

/// The [`DefinitionDocument`] for a referenced **entity** (type/module): the
/// document + position of its lowest-rid source-mapped method
/// (`entity_first_sequence_point`), the doc-only counterpart of
/// [`entity_definition_source_in_pdb`]. Pure: reads only `pdb_image`.
pub fn entity_definition_document_in_pdb(
    pdb_image: &[u8],
    method_tokens: &[u32],
) -> Result<Option<DefinitionDocument>, PdbError> {
    let pdb = PortablePdb::read(pdb_image)?;
    match entity_first_sequence_point(&pdb, method_tokens)? {
        Some(point) => Ok(Some(document_for_point(&pdb, point)?)),
        None => Ok(None),
    }
}

/// Resolve one [`SequencePoint`] to its [`DefinitionDocument`] — the document
/// name plus the point's 1-based line/column.
fn document_for_point(
    pdb: &PortablePdb<'_>,
    point: SequencePoint,
) -> Result<DefinitionDocument, PdbError> {
    Ok(DefinitionDocument {
        document: pdb.document_name(point.document)?,
        line: point.start_line,
        column: point.start_column,
    })
}

/// Resolve a definition source from a **pickled source range**
/// ([`borzoi_assembly::FsharpSourceRange`]) rather than a method token — the navigation path
/// for an F# module *value*: its getter MethodDef carries no sequence point
/// (the initialiser lives in the module's `.cctor`), but the assembly's
/// signature pickle records the binding's `DefinitionRange`, and the PDB
/// still says how to obtain the file. Resolution order:
///
/// 1. the range's file is a PDB document with **embedded source** → offline
///    [`DefinitionSource::Embedded`];
/// 2. the file maps through **SourceLink** → [`DefinitionSource::Remote`].
///    This deliberately does *not* require the file to appear in the PDB's
///    `Document` table: an `.fsi`-constrained assembly can pickle ranges in
///    files no sequence point ever names, yet the SourceLink prefix map still
///    covers them.
///
/// The pickled 0-based column becomes the 1-based [`DefinitionSource`]
/// convention here. `Ok(None)` when the file is neither embedded nor
/// SourceLink-mapped (D5: say nothing rather than guess). Pure: reads only
/// `pdb_image`.
pub fn range_definition_source_in_pdb(
    pdb_image: &[u8],
    range: &borzoi_assembly::FsharpSourceRange,
) -> Result<Option<DefinitionSource>, PdbError> {
    let pdb = PortablePdb::read(pdb_image)?;
    let rid = document_rid_by_name(&pdb, &range.file)?;
    source_for_document(
        &pdb,
        range.file.clone(),
        rid,
        range.start_line,
        range.start_column.saturating_add(1),
    )
}

/// The `Document` table row whose name is exactly `name`, or `None` when the
/// PDB records no such document (e.g. an `.fsi` file — signature files carry
/// no sequence points, so no document row ever names them).
fn document_rid_by_name(pdb: &PortablePdb<'_>, name: &str) -> Result<Option<u32>, PdbError> {
    for rid in 1..=pdb.document_count() {
        if pdb.document_name(rid)? == name {
            return Ok(Some(rid));
        }
    }
    Ok(None)
}

/// The token path with the pickled-range fallback: a method that carries a
/// sequence point resolves exactly as [`definition_source_in_pdb`]; one that
/// does not (an F# module value's getter) falls back to
/// [`range_definition_source_in_pdb`] when a range is available. `Ok(None)`
/// when neither says anything.
pub fn definition_source_with_range_fallback(
    pdb_image: &[u8],
    metadata_token: u32,
    range: Option<&borzoi_assembly::FsharpSourceRange>,
) -> Result<Option<DefinitionSource>, PdbError> {
    if let Some(source) = definition_source_in_pdb(pdb_image, metadata_token)? {
        return Ok(Some(source));
    }
    match range {
        Some(range) => range_definition_source_in_pdb(pdb_image, range),
        None => Ok(None),
    }
}

/// The [`DefinitionDocument`] view of a pickled range — the hover-side "say
/// where it is" counterpart of [`range_definition_source_in_pdb`]. No PDB is
/// needed: the range itself names the document and position; only the
/// 0-based pickled column is converted to the 1-based document convention.
pub fn definition_document_for_range(
    range: &borzoi_assembly::FsharpSourceRange,
) -> DefinitionDocument {
    DefinitionDocument {
        document: range.file.clone(),
        line: range.start_line,
        column: range.start_column.saturating_add(1),
    }
}

/// The file name of the *sidecar* `.pdb` a DLL points at via its CodeView debug
/// entry, or `None` for a DLL with no such pointer. Only the file name (not the
/// recorded absolute build-machine path) is meaningful: the sidecar is looked
/// for next to the DLL. Pure: reads only `dll_bytes`.
pub fn sidecar_pdb_name(dll_bytes: &[u8]) -> Option<String> {
    Some(
        codeview_pdb_reference(dll_bytes)
            .ok()??
            .file_name()?
            .to_string(),
    )
}

/// Whether `sidecar_bytes` is the portable PDB this DLL expects: its
/// [`PortablePdb::id`] equals the DLL's CodeView id. A mismatch means a stale or
/// foreign `.pdb` left beside the DLL — rejected so go-to-definition never
/// navigates to the wrong source (D5). Pure: reads only the two byte slices.
pub fn sidecar_pdb_matches(dll_bytes: &[u8], sidecar_bytes: &[u8]) -> bool {
    let Ok(Some(reference)) = codeview_pdb_reference(dll_bytes) else {
        return false;
    };
    PortablePdb::read(sidecar_bytes).is_ok_and(|pdb| pdb.id() == reference.id)
}

/// Turn one method's first [`SequencePoint`] into a [`DefinitionSource`]:
/// embedded source when the PDB carries it (offline), else a SourceLink URL,
/// else `None`. Shared by the per-method ([`definition_source`]) and per-entity
/// ([`entity_definition_source`]) entry points.
fn source_for_point(
    pdb: &PortablePdb<'_>,
    point: SequencePoint,
) -> Result<Option<DefinitionSource>, PdbError> {
    let document = pdb.document_name(point.document)?;
    source_for_document(
        pdb,
        document,
        Some(point.document),
        point.start_line,
        point.start_column,
    )
}

/// The document-resolution core shared by the sequence-point
/// ([`source_for_point`]) and pickled-range
/// ([`range_definition_source_in_pdb`]) paths: embedded source when the
/// document has a `Document` row carrying it (offline, no network), else a
/// SourceLink URL, else `None`. `rid` is the document's row when it has one —
/// a pickled range can name a file no sequence point does (an `.fsi`), which
/// then can only resolve through SourceLink. `line`/`column` are 1-based.
fn source_for_document(
    pdb: &PortablePdb<'_>,
    document: String,
    rid: Option<u32>,
    line: u32,
    column: u32,
) -> Result<Option<DefinitionSource>, PdbError> {
    // Prefer embedded source — it needs no network.
    if let Some(rid) = rid
        && let Some(text) = pdb.document_embedded_source(rid)?
    {
        return Ok(Some(DefinitionSource::Embedded {
            document,
            text,
            line,
            column,
        }));
    }

    // Otherwise map the document to a URL via SourceLink.
    if let Some(json) = pdb.sourcelink_json()?
        && let Some(url) = sourcelink_url(&json, &document)
    {
        return Ok(Some(DefinitionSource::Remote {
            document,
            url,
            line,
            column,
        }));
    }

    Ok(None)
}

/// Map a `document` path to its URL through a SourceLink JSON document
/// (`{ "documents": { "<prefix>*": "<url-prefix>*", "<exact>": "<url>" } }`).
///
/// A `*`-terminated key matches any document with that prefix; the matched
/// suffix (with `\` rewritten to `/` for the URL) replaces the `*` in the value.
/// A key without `*` matches exactly. When several keys match, the one with the
/// longest prefix wins (the SourceLink "most specific" rule). Matching is
/// ASCII-case-insensitive, as SourceLink keys are on Windows builds.
fn sourcelink_url(json: &str, document: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let documents = value.get("documents")?.as_object()?;

    let mut best: Option<(usize, String)> = None;
    for (pattern, replacement) in documents {
        let Some(replacement) = replacement.as_str() else {
            continue;
        };
        if let Some((prefix_len, url)) = match_document(pattern, replacement, document)
            && best
                .as_ref()
                .is_none_or(|(best_len, _)| prefix_len > *best_len)
        {
            best = Some((prefix_len, url));
        }
    }
    best.map(|(_, url)| url)
}

/// Try one SourceLink `(pattern, replacement)` against `document`, returning the
/// matched-prefix length (for the longest-match tiebreak) and the resolved URL.
fn match_document(pattern: &str, replacement: &str, document: &str) -> Option<(usize, String)> {
    match pattern.strip_suffix('*') {
        Some(prefix) => {
            // Prefix (wildcard) match, ASCII-case-insensitive on the prefix only.
            let head = document.as_bytes().get(..prefix.len())?;
            if !head.eq_ignore_ascii_case(prefix.as_bytes()) {
                return None;
            }
            // The suffix keeps its original case but becomes a URL path
            // segment-sequence: `\` → `/`, then percent-encode so spaces / `#` /
            // non-ASCII in a source path can't break (or truncate) the URL.
            let suffix = encode_url_path(&document.get(prefix.len()..)?.replace('\\', "/"));
            Some((prefix.len(), replacement.replace('*', &suffix)))
        }
        None => {
            // Exact file match.
            document
                .eq_ignore_ascii_case(pattern)
                .then(|| (pattern.len(), replacement.to_string()))
        }
    }
}

/// Percent-encode a `/`-separated URL path: keep RFC 3986 unreserved bytes
/// (`A-Z a-z 0-9 - . _ ~`) and the `/` separators verbatim, `%XX`-encode every
/// other byte (including spaces, `#`, and the bytes of non-ASCII characters).
/// Conservative over-encoding is safe — the server decodes it back.
fn encode_url_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        match b {
            b'/' | b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Fetch the bytes at a URL — the SourceLink network *effect*, injected so the
/// materialiser can be exercised without a network (a fake in tests; the real
/// HTTP client only behind the `sourcelink-fetch` feature). `Err` carries a
/// human-readable reason.
///
/// `Send + Sync` so the dispatch shell can hand a fetcher to a worker thread and
/// perform the SourceLink fetch off the request loop (the real `MinreqFetcher`
/// is a ZST, trivially both).
pub trait SourceFetcher: Send + Sync {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Where a definition's source was materialised to, with a 1-based position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceTarget {
    /// A local file (embedded source, or fetched source written to the cache).
    File {
        path: PathBuf,
        line: u32,
        column: u32,
    },
    /// The source URL itself — returned for a `Remote` source when no fetcher is
    /// supplied (the default build), for the client to open.
    Url { url: String, line: u32, column: u32 },
}

/// What [`plan_source`] determined about obtaining a definition's source,
/// separating the local-only outcome (ready to open now) from the network
/// effect the shell must perform. Deliberately **fetcher-agnostic**: whether a
/// fetcher exists is a shell decision (the dispatch loop performs the fetch off
/// the request thread, or — with no fetcher — surfaces the URL), so the planner
/// never needs one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourcePlan {
    /// The source is already available locally: embedded text just written to
    /// the cache, or a remote source whose cache file already exists.
    Ready(SourceTarget),
    /// A remote source not yet cached. Fetch `url` and write it to `dest` — the
    /// content/URL-keyed cache path the cache-hit check reads, so a deferred
    /// write lands exactly where a later synchronous lookup expects it — after
    /// which it is a [`SourceTarget::File`] at `line`/`column`.
    NeedsFetch {
        url: String,
        dest: PathBuf,
        line: u32,
        column: u32,
    },
}

/// Turn a [`DefinitionSource`] into a [`SourceTarget`], performing the fetch
/// effect for [`DefinitionSource::Remote`] **only when `fetcher` is `Some`**:
///
/// - `Embedded` → write the text into `cache_dir`, return its `File`.
/// - `Remote` + `Some(fetcher)` → fetch (once; cached on disk by document) →
///   write into `cache_dir`, return its `File`.
/// - `Remote` + `None` → return the `Url` unchanged (the caller surfaces it).
///
/// The cache key includes the source *identity* (the embedded text, or the
/// fetch URL) — not just the document path — so two assemblies (or package
/// versions) that record the *same* PDB document path but different source never
/// alias onto one cache file. A repeated fetch of the same URL is a no-op.
pub fn materialize(
    source: DefinitionSource,
    cache_dir: &Path,
    fetcher: Option<&dyn SourceFetcher>,
) -> std::io::Result<SourceTarget> {
    match plan_source(source, cache_dir)? {
        SourcePlan::Ready(target) => Ok(target),
        SourcePlan::NeedsFetch {
            url,
            dest,
            line,
            column,
        } => match fetcher {
            None => Ok(SourceTarget::Url { url, line, column }),
            Some(fetcher) => {
                let bytes = fetcher.fetch(&url).map_err(std::io::Error::other)?;
                write_if_absent(&dest, &bytes)?;
                Ok(SourceTarget::File {
                    path: dest,
                    line,
                    column,
                })
            }
        },
    }
}

/// Plan how to obtain a definition's source, **without** performing any network
/// fetch — the pure-ish core the dispatch shell drives (it performs the fetch on
/// a worker thread, or surfaces the URL when no fetcher is configured). Embedded
/// text is written to the cache here (local IO only) and is always [`Ready`]; a
/// remote source is [`Ready`] when its cache file already exists and
/// [`NeedsFetch`] otherwise.
///
/// The cache key (both the embedded-content path and the remote `dest`) is the
/// same content/URL-keyed cache path [`materialize`] uses, so two assemblies
/// (or package versions) recording the same PDB document path but different
/// source never alias onto one file, and a deferred fetch writes exactly where a
/// later cache-hit lookup reads.
///
/// [`Ready`]: SourcePlan::Ready
/// [`NeedsFetch`]: SourcePlan::NeedsFetch
pub fn plan_source(source: DefinitionSource, cache_dir: &Path) -> std::io::Result<SourcePlan> {
    match source {
        DefinitionSource::Embedded {
            document,
            text,
            line,
            column,
        } => {
            let path = cache_path(cache_dir, &document, text.as_bytes());
            write_if_absent(&path, text.as_bytes())?;
            Ok(SourcePlan::Ready(SourceTarget::File { path, line, column }))
        }
        DefinitionSource::Remote {
            document,
            url,
            line,
            column,
        } => {
            // Keyed by URL: the SourceLink URL carries the repo/commit, so
            // different assemblies/versions never collide; an existing cache file
            // means the same URL was already fetched.
            let dest = cache_path(cache_dir, &document, url.as_bytes());
            if dest.exists() {
                Ok(SourcePlan::Ready(SourceTarget::File {
                    path: dest,
                    line,
                    column,
                }))
            } else {
                Ok(SourcePlan::NeedsFetch {
                    url,
                    dest,
                    line,
                    column,
                })
            }
        }
    }
}

/// Cache path for a source: a subdir hashed from the document path **and** the
/// source `identity` (its content or URL — so distinct sources sharing a
/// document path don't alias), holding a file named by the document's basename
/// (so the editor shows a sensible name).
fn cache_path(cache_dir: &Path, document: &str, identity: &[u8]) -> PathBuf {
    let basename = document
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("source");
    let mut hasher = DefaultHasher::new();
    document.hash(&mut hasher);
    identity.hash(&mut hasher);
    cache_dir
        .join(format!("{:016x}", hasher.finish()))
        .join(basename)
}

/// Write `bytes` to `path` (creating parent dirs) unless it already exists —
/// the cache is content/URL-keyed, so an existing file already holds exactly
/// these bytes. `pub` so the dispatch shell's fetch worker writes a deferred
/// fetch to the same cache the synchronous path reads.
///
/// The publish is **atomic**: bytes go to a unique temp file in the same
/// directory, then `rename` swaps it into place, so a concurrent reader (the
/// cache-hit check is just `path.exists()`) sees either no file or the complete
/// one — never a half-written one. `rename` is atomic on every supported
/// filesystem, including FAT/exFAT and network mounts (where `hard_link` is not
/// available).
///
/// Concurrent writers of the *same* path are not a hazard, so no-clobber is not
/// needed: the cache key is derived from the content (embedded text) or the URL
/// (remote — and SourceLink URLs pin a commit), so the same path always means
/// the same bytes. A race therefore only ever replaces the file with identical
/// content (harmless), and an already-published destination — which on Windows
/// makes `rename` error rather than replace — is treated as success.
pub fn write_if_absent(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = unique_temp_path(path);
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp); // don't leak the temp on failure
            // A racing writer may have already published `dest` (and on Windows
            // `rename` refuses to replace an existing file, so a race always
            // errors here). If the destination now exists, the cache entry is
            // complete and content-correct — success, not a URL fallback.
            if path.exists() { Ok(()) } else { Err(err) }
        }
    }
}

/// A unique sibling path of `dest` for the atomic-write temp file. Unique within
/// a process via an atomic counter, and across processes via the pid — enough to
/// avoid collisions between concurrent fetch workers writing the same cache dir.
fn unique_temp_path(dest: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let base = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("source");
    parent.join(format!(".{base}.tmp.{}.{n}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_maps_path_to_url_with_forward_slashes() {
        let json = r#"{"documents":{"C:\\src\\*":"https://example.com/repo/*"}}"#;
        let url = sourcelink_url(json, r"C:\src\dir\file.fs").unwrap();
        // The matched suffix `dir\file.fs` becomes `dir/file.fs` in the URL.
        assert_eq!(url, "https://example.com/repo/dir/file.fs");
    }

    #[test]
    fn longest_prefix_wins() {
        // A general root and a more specific subdir both match; the subdir wins.
        let json = r#"{"documents":{
            "C:\\src\\*":"https://example.com/all/*",
            "C:\\src\\fsharp\\*":"https://example.com/fsharp/*"
        }}"#;
        let url = sourcelink_url(json, r"C:\src\fsharp\printf.fs").unwrap();
        assert_eq!(url, "https://example.com/fsharp/printf.fs");
    }

    #[test]
    fn exact_match_without_wildcard() {
        let json = r#"{"documents":{"C:\\gen\\AssemblyInfo.fs":"https://example.com/ai.fs"}}"#;
        assert_eq!(
            sourcelink_url(json, r"C:\gen\AssemblyInfo.fs").unwrap(),
            "https://example.com/ai.fs"
        );
        assert!(sourcelink_url(json, r"C:\gen\Other.fs").is_none());
    }

    #[test]
    fn case_insensitive_prefix() {
        let json = r#"{"documents":{"D:\\A\\*":"https://example.com/*"}}"#;
        // Document differs only in case on the prefix.
        let url = sourcelink_url(json, r"d:\a\Lib\X.fs").unwrap();
        assert_eq!(url, "https://example.com/Lib/X.fs");
    }

    #[test]
    fn suffix_is_percent_encoded() {
        let json = r#"{"documents":{"C:\\src\\*":"https://example.com/r/*"}}"#;
        // A space and a `#` in the path must be encoded, `/` kept as separators.
        let url = sourcelink_url(json, r"C:\src\my dir\a#b.fs").unwrap();
        assert_eq!(url, "https://example.com/r/my%20dir/a%23b.fs");
    }

    #[test]
    fn non_ascii_suffix_is_utf8_percent_encoded() {
        let json = r#"{"documents":{"C:\\src\\*":"https://example.com/r/*"}}"#;
        // `é` is U+00E9 → UTF-8 0xC3 0xA9 → `%C3%A9`.
        let url = sourcelink_url(json, "C:\\src\\café.fs").unwrap();
        assert_eq!(url, "https://example.com/r/caf%C3%A9.fs");
    }

    #[test]
    fn no_documents_or_no_match_is_none() {
        assert!(sourcelink_url(r#"{"documents":{}}"#, "anything").is_none());
        assert!(sourcelink_url(r#"{}"#, "anything").is_none());
        assert!(sourcelink_url("not json", "anything").is_none());
    }

    // --- materialize + the fetch effect (no network) ------------------------

    use std::sync::atomic::{AtomicU32, Ordering};

    /// A `SourceFetcher` that returns canned bytes and counts its calls, so a
    /// test can assert the fetch effect happened (and was cached) without a
    /// network. `AtomicU32` (not `Cell`) so it satisfies the trait's `Send +
    /// Sync` bound.
    struct FakeFetcher {
        body: Vec<u8>,
        calls: AtomicU32,
    }
    impl SourceFetcher for FakeFetcher {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.body.clone())
        }
    }

    fn cache_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn embedded_is_written_to_the_cache() {
        let dir = cache_dir();
        let source = DefinitionSource::Embedded {
            document: r"C:\src\a\Lib.fs".into(),
            text: "let x = 1\n".into(),
            line: 3,
            column: 5,
        };
        let target = materialize(source, dir.path(), None).unwrap();
        match target {
            SourceTarget::File { path, line, column } => {
                assert_eq!((line, column), (3, 5));
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "let x = 1\n");
                // The cache file keeps the document's basename.
                assert_eq!(path.file_name().unwrap(), "Lib.fs");
            }
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn remote_without_fetcher_surfaces_the_url() {
        let dir = cache_dir();
        let source = DefinitionSource::Remote {
            document: "x.fs".into(),
            url: "https://example.com/x.fs".into(),
            line: 1,
            column: 1,
        };
        let target = materialize(source, dir.path(), None).unwrap();
        assert_eq!(
            target,
            SourceTarget::Url {
                url: "https://example.com/x.fs".into(),
                line: 1,
                column: 1
            }
        );
    }

    #[test]
    fn remote_with_fetcher_fetches_once_then_caches() {
        let dir = cache_dir();
        let fetcher = FakeFetcher {
            body: b"let printfn fmt = ...\n".to_vec(),
            calls: AtomicU32::new(0),
        };
        let make = || DefinitionSource::Remote {
            document: r"D:\repo\printf.fs".into(),
            url: "https://example.com/printf.fs".into(),
            line: 42,
            column: 7,
        };

        // First call performs the fetch effect and writes the cache.
        let first = materialize(make(), dir.path(), Some(&fetcher)).unwrap();
        let path = match first {
            SourceTarget::File { path, line, column } => {
                assert_eq!((line, column), (42, 7));
                assert_eq!(
                    std::fs::read_to_string(&path).unwrap(),
                    "let printfn fmt = ...\n"
                );
                path
            }
            other => panic!("expected File, got {other:?}"),
        };
        assert_eq!(
            fetcher.calls.load(Ordering::Relaxed),
            1,
            "fetch happens once"
        );

        // Second call for the same document hits the cache — no further fetch.
        let second = materialize(make(), dir.path(), Some(&fetcher)).unwrap();
        assert_eq!(
            second,
            SourceTarget::File {
                path,
                line: 42,
                column: 7
            }
        );
        assert_eq!(
            fetcher.calls.load(Ordering::Relaxed),
            1,
            "cached: no second fetch"
        );
    }

    #[test]
    fn same_document_different_content_does_not_alias() {
        let dir = cache_dir();
        // Two assemblies record the same PDB document path but embed different
        // source — they must land on distinct cache files, not overwrite.
        let embed = |text: &str| DefinitionSource::Embedded {
            document: r"D:\build\Shared.fs".into(),
            text: text.into(),
            line: 1,
            column: 1,
        };
        let a = materialize(embed("// assembly A\n"), dir.path(), None).unwrap();
        let b = materialize(embed("// assembly B\n"), dir.path(), None).unwrap();
        let (SourceTarget::File { path: pa, .. }, SourceTarget::File { path: pb, .. }) = (&a, &b)
        else {
            panic!("both embedded → File");
        };
        assert_ne!(pa, pb, "distinct content must not share a cache file");
        assert_eq!(std::fs::read_to_string(pa).unwrap(), "// assembly A\n");
        assert_eq!(std::fs::read_to_string(pb).unwrap(), "// assembly B\n");
    }

    #[test]
    fn same_document_different_url_fetches_each() {
        let dir = cache_dir();
        // Same document path, different SourceLink URLs (e.g. two package
        // versions): each must be fetched and cached separately, not aliased.
        let fetcher = FakeFetcher {
            body: b"x\n".to_vec(),
            calls: AtomicU32::new(0),
        };
        let remote = |url: &str| DefinitionSource::Remote {
            document: r"D:\build\Shared.fs".into(),
            url: url.into(),
            line: 1,
            column: 1,
        };
        let a = materialize(remote("https://h/v1/Shared.fs"), dir.path(), Some(&fetcher)).unwrap();
        let b = materialize(remote("https://h/v2/Shared.fs"), dir.path(), Some(&fetcher)).unwrap();
        assert_ne!(a, b, "different URLs must not alias to one cache file");
        assert_eq!(
            fetcher.calls.load(Ordering::Relaxed),
            2,
            "each distinct URL is fetched"
        );
    }

    // --- plan_source (the network-free planner the dispatch shell drives) ----

    #[test]
    fn plan_embedded_is_ready_and_written() {
        let dir = cache_dir();
        let source = DefinitionSource::Embedded {
            document: r"C:\src\a\Lib.fs".into(),
            text: "let x = 1\n".into(),
            line: 3,
            column: 5,
        };
        match plan_source(source, dir.path()).unwrap() {
            SourcePlan::Ready(SourceTarget::File { path, line, column }) => {
                assert_eq!((line, column), (3, 5));
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "let x = 1\n");
            }
            other => panic!("expected Ready(File), got {other:?}"),
        }
    }

    #[test]
    fn plan_remote_uncached_needs_fetch_at_the_cache_path() {
        let dir = cache_dir();
        let document = r"D:\repo\printf.fs";
        let url = "https://example.com/printf.fs";
        let source = DefinitionSource::Remote {
            document: document.into(),
            url: url.into(),
            line: 42,
            column: 7,
        };
        match plan_source(source, dir.path()).unwrap() {
            SourcePlan::NeedsFetch {
                url: u,
                dest,
                line,
                column,
            } => {
                assert_eq!((line, column), (42, 7));
                assert_eq!(u, url);
                // The dest MUST be the same content/URL-keyed path the cache-hit
                // check (and `materialize`) reads, or a deferred fetch's write is
                // invisible to later lookups.
                assert_eq!(dest, cache_path(dir.path(), document, url.as_bytes()));
            }
            other => panic!("expected NeedsFetch, got {other:?}"),
        }
    }

    #[test]
    fn plan_remote_cached_is_ready() {
        let dir = cache_dir();
        let document = r"D:\repo\printf.fs";
        let url = "https://example.com/printf.fs";
        // Pre-populate the cache file at the keyed path.
        let dest = cache_path(dir.path(), document, url.as_bytes());
        write_if_absent(&dest, b"cached body\n").unwrap();

        let source = DefinitionSource::Remote {
            document: document.into(),
            url: url.into(),
            line: 1,
            column: 1,
        };
        match plan_source(source, dir.path()).unwrap() {
            SourcePlan::Ready(SourceTarget::File { path, .. }) => assert_eq!(path, dest),
            other => panic!("expected Ready(File) on cache hit, got {other:?}"),
        }
    }

    // --- atomic cache write (no torn reads under concurrent workers) ---------

    #[test]
    fn write_if_absent_writes_full_content_and_leaves_no_temp() {
        let dir = cache_dir();
        let dest = dir.path().join("sub").join("Lib.fs");
        write_if_absent(&dest, b"complete\n").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"complete\n");
        // The temp file used for the atomic rename must not linger.
        let leftovers: Vec<_> = std::fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn write_if_absent_does_not_overwrite_an_existing_file() {
        let dir = cache_dir();
        let dest = dir.path().join("Lib.fs");
        write_if_absent(&dest, b"first\n").unwrap();
        // Content-keyed cache: a present file already holds the right bytes, so a
        // second write is a no-op (and must not truncate/replace it).
        write_if_absent(&dest, b"second\n").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"first\n");
    }

    #[test]
    fn write_if_absent_is_atomic_under_concurrent_writers() {
        // Many threads write the same dest concurrently; every observed read is
        // either absent or the *complete* content — never a torn/empty file.
        let dir = cache_dir();
        let dest = dir.path().join("hot").join("Race.fs");
        let body = vec![b'x'; 256 * 1024]; // large enough that a non-atomic fill would be observably partial
        std::thread::scope(|s| {
            for _ in 0..8 {
                let dest = dest.clone();
                let body = body.clone();
                s.spawn(move || write_if_absent(&dest, &body).unwrap());
            }
            // Concurrent readers: a present file must already be complete.
            for _ in 0..64 {
                if let Ok(seen) = std::fs::read(&dest) {
                    assert_eq!(seen.len(), body.len(), "observed a torn cache file");
                }
            }
        });
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }
}

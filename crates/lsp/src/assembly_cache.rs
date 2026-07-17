//! An on-disk cache of *projected* referenced assemblies, so a warm server
//! restart skips the parse+project of every referenced DLL (the bulk of a cold
//! project resolve — see `semantic::build_env_from_dll_paths`).
//!
//! The cached value for a DLL is exactly what `semantic::enumerate_dll_type_defs`
//! would compute for it: the projected `Vec<Entity>` (abbreviation markers
//! included) plus the per-assembly abbreviation-visibility flag fed to
//! [`borzoi_sema::AssemblyEnv`]. A hit returns that
//! identically, so the cache is **correctness-preserving**: it never changes a
//! resolution, only how fast the env is built.
//!
//! ## Keying
//!
//! The referenced set is exclusively package-cache + framework-pack DLLs (the LSP
//! ignores project references), and those are **immutable at their path** — the
//! absolute path already encodes package id+version+TFM
//! (`…/fantomas.fcs/6.3.15/lib/netstandard2.0/Fantomas.FCS.dll`) or the SDK
//! version (`…/Microsoft.NETCore.App/9.0.2/System.Runtime.dll`). So the
//! **canonical absolute path is the content key**. (Assembly *fullname* would be
//! weaker — `AssemblyVersion` rarely changes across rebuilds, and one fullname
//! resolves to several TFM variants that project differently.) Against an
//! in-place overwrite that breaks that immutability (a mutable local feed
//! republishing a version), the entry also carries the DLL's **size and mtime**
//! from a single near-free `stat`: a changed size or mtime is a miss, so a
//! same-length rewrite that size alone would miss is still caught by the mtime.
//! A benign mtime bump with unchanged content just recomputes once —
//! correctness-preserving, only a little slower.
//!
//! The invalidation that *does* matter is a **projector change**: a stale entry
//! would then misrepresent the DLL. That is covered by the validity `tag` —
//! `CACHE_SCHEMA` plus the running binary's identity (its path, size, and mtime,
//! robust to reproducible-build mtime normalization), so a rebuilt LSP
//! auto-invalidates without a manual bump, while a released binary keeps its
//! cache across restarts.
//!
//! ## Disable / read-only
//!
//! Governed by `BORZOI_LSP_CACHE_DIR`: unset → a default under
//! `$XDG_CACHE_HOME`/`~/.cache`; **empty → disabled** (no reads, no writes); a
//! path → that directory. Everything is **best-effort**: a missing/corrupt entry
//! is a miss, and any write failure (a read-only filesystem, a full disk) is
//! silently skipped — the cache can never fail a request or crash the server, and
//! a read-only deployment can leave it entirely untouched.

use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use bincode::Options;
use borzoi_assembly::Entity;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Bump when the on-disk framing or the interpretation of a cached entry changes
/// in a way a stale entry would misread. The projected `Entity` *shape* is
/// covered separately by the binary-mtime component of [`AssemblyCache::tag`] (a
/// projector change means a rebuilt binary), so this only needs bumping for
/// changes to *this* module's framing.
const CACHE_SCHEMA: u32 = 2;

/// Leading marker in every entry — a corrupt/foreign file fails the check and is
/// treated as a miss rather than mis-deserialized.
const MAGIC: u32 = 0xDF5A_C0DE;

/// The environment variable selecting the cache directory. Empty disables the
/// cache entirely; unset falls back to a default location. Shared with the
/// SourceLink source cache ([`crate::handlers::definition`]), which reads the
/// same variable so one knob governs both on-disk caches.
pub(crate) const CACHE_DIR_ENV: &str = "BORZOI_LSP_CACHE_DIR";

/// An on-disk cache of projected assemblies. Cheap to construct and clone-free;
/// `dir == None` is the disabled state (the default, and what an empty
/// `BORZOI_LSP_CACHE_DIR` selects).
#[derive(Debug, Clone)]
pub struct AssemblyCache {
    /// The directory holding one entry file per DLL, or `None` when disabled.
    dir: Option<PathBuf>,
    /// Validity tag: schema + crate version + this binary's mtime. An entry whose
    /// stored tag differs is a miss (covers a projector change: rebuilt binary ⇒
    /// new mtime ⇒ new tag).
    tag: u64,
}

/// The fixed-size front matter of an entry, deserialized and validated before the
/// (larger) entity payload is read, so a stale/foreign/mismatched entry is
/// rejected cheaply.
#[derive(Serialize, serde::Deserialize)]
struct Header {
    magic: u32,
    tag: u64,
    /// The canonical DLL path this entry was computed from — a guard so a
    /// filename-hash collision cannot hand back another DLL's entities.
    path: String,
    /// The DLL's size in bytes when it was cached (a `stat`, no read) — cheap
    /// insurance against an in-place overwrite at the same path.
    size: u64,
    /// The DLL's mtime in nanoseconds since the Unix epoch when it was cached,
    /// or `None` where the platform/filesystem reports no mtime (or it predates
    /// the epoch). Together with `size`, a `stat`-only staleness guard: a same-
    /// length in-place overwrite (a mutable local feed republishing a version)
    /// slips past `size` but changes `mtime`, so a mismatch ⇒ a miss. A benign
    /// mtime bump with unchanged content just recomputes once — correctness-
    /// preserving, only a little slower.
    mtime: Option<u128>,
}

impl Default for AssemblyCache {
    /// Disabled — so a `#[derive(Default)]` holder (e.g. `SemanticState`) stays
    /// off-disk until the server explicitly opts in via [`Self::from_env`].
    fn default() -> Self {
        Self::disabled()
    }
}

impl AssemblyCache {
    /// A disabled cache — every `get` misses and every `put` is a no-op. The
    /// default for [`crate::semantic::SemanticState::new`], so tests and any
    /// consumer that has not opted in stay off-disk.
    pub fn disabled() -> Self {
        AssemblyCache { dir: None, tag: 0 }
    }

    /// Resolve the cache from the environment (the server's opt-in). `unset` →
    /// default directory; empty `BORZOI_LSP_CACHE_DIR` → [`Self::disabled`];
    /// a path → that directory.
    pub fn from_env() -> Self {
        match std::env::var_os(CACHE_DIR_ENV) {
            Some(v) if v.is_empty() => Self::disabled(),
            Some(v) => Self::enabled(PathBuf::from(v)),
            None => match default_cache_dir() {
                Some(dir) => Self::enabled(dir),
                None => Self::disabled(),
            },
        }
    }

    /// Enabled at an explicit directory, ignoring the environment. For an
    /// embedder that manages its own cache location (and for tests that want an
    /// isolated temp dir); the server itself uses [`Self::from_env`].
    pub fn at(dir: PathBuf) -> Self {
        Self::enabled(dir)
    }

    /// Construct an *enabled* cache — the one path that requires POSIX. The
    /// cache's atomic replace ([`Self::write`]) relies on `rename` overwriting
    /// its destination, which Windows refuses (a stale entry would silently
    /// persist). Rather than misbehave there, degrade to a [`Self::disabled`]
    /// cache off-Unix: the server runs everywhere, simply without the cache, and
    /// the on-disk format never risks a stale entry on an OS whose `rename` can't
    /// overwrite one.
    fn enabled(dir: PathBuf) -> Self {
        Self::enabled_when(dir, cfg!(unix))
    }

    /// [`Self::enabled`] with the POSIX gate injected, so both outcomes are
    /// exercisable on any host (`cfg!(unix)` compiles one branch out). When
    /// `posix_rename` is false the cache degrades to [`Self::disabled`].
    fn enabled_when(dir: PathBuf, posix_rename: bool) -> Self {
        if !posix_rename {
            return Self::disabled();
        }
        AssemblyCache {
            dir: Some(dir),
            tag: compute_tag(),
        }
    }

    /// The cached projection for `dll`, or `None` on a miss (disabled, absent,
    /// stale tag, changed size, corrupt, or an IO error). Never fails.
    pub fn get(&self, dll: &Path) -> Option<Vec<Entity>> {
        self.read(dll)
    }

    /// Store the projection for `dll`. Best-effort: any failure (disabled,
    /// unwritable directory, IO error) is silently dropped.
    pub fn put(&self, dll: &Path, entities: &[Entity]) {
        self.write(dll, entities);
    }

    /// Return the cached value for `dll`, or run `compute` to produce it and
    /// (best-effort) persist the result for the next warm start.
    ///
    /// The staleness stat stored with a fresh entry is snapshotted *before*
    /// `compute` runs and the entry is persisted only if the DLL is still
    /// byte-identical (same size+mtime) afterwards — so a DLL overwritten *while*
    /// `compute` reads its bytes (a concurrent restore) is never cached with the
    /// post-overwrite metadata against the pre-overwrite value it would then hand
    /// back on a warm start. The snapshot itself is stored (not a later
    /// re-`stat`), so nothing that happens after the check can record mismatched
    /// metadata. `compute` returning `None` (an unreadable/unparseable DLL) is
    /// propagated and caches nothing.
    ///
    /// A disabled cache always misses, never `stat`s, and never writes, so this
    /// is exactly `compute()` with an off-by-default fast path.
    pub fn get_or_populate<T: Serialize + DeserializeOwned>(
        &self,
        dll: &Path,
        compute: impl FnOnce() -> Option<T>,
    ) -> Option<T> {
        if let Some(hit) = self.read::<T>(dll) {
            tracing::debug!(dll = %dll.display(), "assembly-cache hit");
            return Some(hit);
        }
        // Only snapshot when the cache is live, so a disabled cache pays for no
        // `stat`. `None` here (disabled, or an unstattable DLL) ⇒ skip the write.
        let before = self.dir.as_ref().and_then(|_| dll_stat(dll));
        let value = compute()?;
        if let Some(before) = before
            && dll_stat(dll) == Some(before)
        {
            self.write_with_stat(dll, before, &value);
        }
        Some(value)
    }

    /// The entry file path for a canonical DLL path — its hash under the cache
    /// dir. `None` when disabled.
    fn entry_path(&self, canonical: &str) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        canonical.hash(&mut h);
        Some(dir.join(format!("{:016x}.bin", h.finish())))
    }

    fn read<T: DeserializeOwned>(&self, dll: &Path) -> Option<T> {
        // Disabled ⇒ miss without touching the filesystem: bail before
        // `canonical_string` (a `canonicalize` syscall per DLL) so the opt-out
        // path stays free, as the default for tests/embedders demands.
        self.dir.as_ref()?;
        let canonical = canonical_string(dll);
        let entry = self.entry_path(&canonical)?;
        let file = std::fs::File::open(&entry).ok()?;
        // Bound every deserialize by the entry's own byte length: a value can't
        // decode to more bytes than the file holds, so a corrupt/foreign entry
        // with a bogus huge length prefix (in the `path` string or a payload
        // `Vec`) is rejected *before* allocating, keeping "corrupt ⇒ miss" honest
        // rather than OOM-ing on a bad file in the cache dir.
        let limit = file.metadata().ok()?.len();
        let mut r = std::io::BufReader::new(file);
        let header: Header = read_bounded(&mut r, limit)?;
        if header.magic != MAGIC || header.tag != self.tag || header.path != canonical {
            return None;
        }
        // Staleness guard: a single `stat` of the DLL (no read). A changed size
        // *or* mtime ⇒ the DLL was overwritten in place ⇒ recompute. mtime
        // catches a same-length rewrite that size alone would miss.
        let cur = dll_stat(dll)?;
        if header.size != cur.size || header.mtime != cur.mtime {
            return None;
        }
        read_bounded(&mut r, limit)
    }

    /// Store `value` under `dll`, re-`stat`ing it now for the staleness header.
    /// Best-effort; a no-op when disabled or the DLL can't be `stat`ed. The
    /// populate path uses [`Self::write_with_stat`] instead so the header records
    /// the stat taken *before* the bytes were read, not a later re-`stat`.
    fn write<T: Serialize + ?Sized>(&self, dll: &Path, value: &T) {
        if let Some(stat) = dll_stat(dll) {
            self.write_with_stat(dll, stat, value);
        }
    }

    fn write_with_stat<T: Serialize + ?Sized>(&self, dll: &Path, stat: DllStat, value: &T) {
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let canonical = canonical_string(dll);
        let Some(entry) = self.entry_path(&canonical) else {
            return;
        };
        let header = Header {
            magic: MAGIC,
            tag: self.tag,
            path: canonical,
            size: stat.size,
            mtime: stat.mtime,
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        // Write to a per-process temp file then atomically rename, so a concurrent
        // reader (another server instance) never observes a half-written entry.
        let tmp = entry.with_extension(format!("{}.tmp", std::process::id()));
        let ok = (|| -> std::io::Result<()> {
            let file = std::fs::File::create(&tmp)?;
            let mut w = std::io::BufWriter::new(file);
            bincode_opts()
                .serialize_into(&mut w, &header)
                .map_err(std::io::Error::other)?;
            bincode_opts()
                .serialize_into(&mut w, value)
                .map_err(std::io::Error::other)?;
            w.flush()?;
            // No `fsync`: the atomic rename gives crash-consistency (a torn temp
            // never becomes the entry), and durability is pointless for a cache —
            // a lost write just recomputes. Fsyncing all ~212 entries would add
            // real latency to the cold build we are trying to speed up.
            w.into_inner().map_err(std::io::Error::other)?;
            Ok(())
        })();
        if ok.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        // Atomic replace: on POSIX `rename` overwrites the destination, so a
        // stale entry (tag/size change) is replaced in place. This POSIX
        // dependency is enforced up front in `Self::enabled` (an enabled cache
        // degrades to disabled off-Unix, so a `dir` is only ever set on Unix),
        // so by here we are on Unix. A failure just leaves the temp file
        // (harmless — overwritten later) and skips this entry.
        let _ = std::fs::rename(&tmp, &entry);
    }
}

/// The bincode encoding shared by cache reads and writes — explicit (not the
/// top-level `bincode::serialize` helpers) so a read can bound allocation with
/// `.with_limit` without changing the on-disk format. Fixint keeps the framing
/// simple and stable.
fn bincode_opts() -> impl bincode::Options {
    bincode::DefaultOptions::new().with_fixint_encoding()
}

/// Deserialize one value, refusing to read (or allocate for) more than `limit`
/// bytes — so a corrupt length prefix is an `Err` (→ a cache miss), never a
/// runaway allocation. `None` on any decode/IO error.
fn read_bounded<T: DeserializeOwned, R: std::io::Read>(reader: R, limit: u64) -> Option<T> {
    bincode_opts()
        .with_limit(limit)
        .deserialize_from(reader)
        .ok()
}

/// The DLL's size and mtime — the cheap staleness signals stored in, and
/// re-checked against, every entry. `mtime` is `None` where the platform/
/// filesystem reports none (or it predates the epoch), in which case the entry
/// falls back to the size guard alone. Nanoseconds since the epoch fit `u128`
/// for any real timestamp, so no precision is lost.
#[derive(Clone, Copy, PartialEq, Eq)]
struct DllStat {
    size: u64,
    mtime: Option<u128>,
}

/// [`DllStat`] from a single `stat` of `dll`, or `None` when it can't be
/// `stat`ed at all (a write skips the entry; a read misses).
fn dll_stat(dll: &Path) -> Option<DllStat> {
    let meta = std::fs::metadata(dll).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos());
    Some(DllStat {
        size: meta.len(),
        mtime,
    })
}

/// The canonical string form of a DLL path — symlink-resolved for a stable key,
/// falling back to the lossy path if canonicalization fails (a missing file,
/// which `enumerate_dll_type_defs` would also fail to read).
fn canonical_string(dll: &Path) -> String {
    std::fs::canonicalize(dll)
        .unwrap_or_else(|_| dll.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The shared cache **root** selected by the environment, *without* a per-cache
/// sub-namespace: `$XDG_CACHE_HOME/borzoi`, else
/// `$HOME/.cache/borzoi`, else `None` (nowhere to put a cache). Both env
/// vars are treated as unset when empty. A single global location (not
/// per-project) so a file shared across projects (e.g. `FSharp.Core`) is cached
/// once.
///
/// The assembly-projection cache and the SourceLink source cache
/// ([`crate::handlers::definition`]) live in sibling sub-namespaces under this
/// root (`entities` / `sources`), so the XDG/HOME base governs both together.
pub(crate) fn default_cache_root() -> Option<PathBuf> {
    default_cache_root_from(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
}

/// Pure form of [`default_cache_root`] (the two env values injected), so the
/// XDG-then-HOME precedence is unit-testable without mutating process env.
fn default_cache_root_from(
    xdg_cache_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let root = |base: PathBuf| base.join("borzoi");
    if let Some(x) = xdg_cache_home.filter(|v| !v.is_empty()) {
        return Some(root(PathBuf::from(x)));
    }
    if let Some(home) = home.filter(|v| !v.is_empty()) {
        return Some(root(PathBuf::from(home).join(".cache")));
    }
    None
}

/// The default cache directory for projected assemblies —
/// `<cache-root>/entities` ([`default_cache_root`]) — or `None` when there is no
/// rootable location (disabled: nowhere to put it).
fn default_cache_dir() -> Option<PathBuf> {
    default_cache_root().map(|root| root.join("entities"))
}

/// The per-entry validity tag: the format schema, the crate version, and the
/// running binary's identity. A rebuilt projector ⇒ a new tag ⇒ stale entries
/// miss, so no manual [`CACHE_SCHEMA`] bump is needed for a projection change.
///
/// The binary identity mixes three cheap signals so it is robust across the two
/// real deployment modes without hashing the whole executable:
/// - **path** — under a content/input-addressed packager (Nix: `/nix/store/
///   <hash>-…/bin/…`) the store hash embeds the build, so a changed projector
///   lands at a different path even though such packagers normalize mtimes;
/// - **size** and **mtime** — a plain `cargo` rebuild at a stable path keeps the
///   path but changes these.
///
/// Best-effort: if the executable can't be located/`stat`ed, the schema + crate
/// version still guard framing changes (bump [`CACHE_SCHEMA`] for a projection
/// change that somehow leaves all three unchanged — not expected in practice).
fn compute_tag() -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    CACHE_SCHEMA.hash(&mut h);
    env!("CARGO_PKG_VERSION").hash(&mut h);
    if let Ok(exe) = std::env::current_exe() {
        exe.hash(&mut h);
        if let Ok(meta) = std::fs::metadata(&exe) {
            meta.len().hash(&mut h);
            if let Ok(mtime) = meta.modified()
                && let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH)
            {
                dur.as_nanos().hash(&mut h);
            }
        }
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny stand-in payload — the cache's framing/validation logic is generic
    /// over the serialized type, so the `Entity`-specific `get`/`put` need no
    /// hand-built `Entity` to exercise the interesting paths.
    fn dll_fixture(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// Force a file's mtime to a known instant so the mtime staleness guard can
    /// be exercised deterministically (independent of filesystem timestamp
    /// resolution). Uses whole seconds far apart so any resolution preserves the
    /// distinction.
    fn set_mtime(path: &Path, secs_since_epoch: u64) {
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs_since_epoch))
            .unwrap();
    }

    /// An enabled cache at `dir`, bypassing the platform gate that `at` applies
    /// so the framing/serialization machinery is exercised on every host — the
    /// gate itself is covered by `off_posix_rename_degrades_to_disabled`. The
    /// round-trips below each write an entry at most once (a create, never the
    /// overwrite-on-`rename` that only POSIX guarantees), so they hold off-Unix
    /// too; going through `at` instead would make them silently pass by getting a
    /// disabled cache on such a host.
    fn enabled_cache(dir: PathBuf) -> AssemblyCache {
        AssemblyCache::enabled_when(dir, true)
    }

    #[test]
    fn round_trips_a_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = enabled_cache(tmp.path().join("cache"));
        let dll = dll_fixture(tmp.path(), "a.dll", b"some bytes");

        cache.write(&dll, &vec![1u32, 2, 3, 4]);
        assert_eq!(cache.read::<Vec<u32>>(&dll), Some(vec![1, 2, 3, 4]));
    }

    #[test]
    fn miss_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = enabled_cache(tmp.path().join("cache"));
        let dll = dll_fixture(tmp.path(), "a.dll", b"bytes");
        assert_eq!(cache.read::<Vec<u32>>(&dll), None);
    }

    #[test]
    fn miss_on_size_change() {
        // A DLL overwritten in place at the same path (different size) must not
        // return the stale entry — the size guard catches it without mtime.
        let tmp = tempfile::tempdir().unwrap();
        let cache = enabled_cache(tmp.path().join("cache"));
        let dll = dll_fixture(tmp.path(), "a.dll", b"original");
        cache.write(&dll, &vec![9u32]);
        assert_eq!(cache.read::<Vec<u32>>(&dll), Some(vec![9]));

        std::fs::write(&dll, b"a different length entirely").unwrap();
        assert_eq!(cache.read::<Vec<u32>>(&dll), None);
    }

    #[test]
    fn miss_on_mtime_change() {
        // A DLL overwritten in place with the *same byte length* but new contents
        // (a mutable local feed republishing a version) slips past the size guard;
        // the mtime guard catches it. Modelled by moving the mtime directly, with
        // size/content held constant, so this isolates the mtime check.
        let tmp = tempfile::tempdir().unwrap();
        let cache = enabled_cache(tmp.path().join("cache"));
        let dll = dll_fixture(tmp.path(), "a.dll", b"identical length");
        set_mtime(&dll, 1_000);
        cache.write(&dll, &vec![5u32]);
        assert_eq!(cache.read::<Vec<u32>>(&dll), Some(vec![5]));

        // Same size, new mtime ⇒ overwritten in place ⇒ recompute (a miss).
        set_mtime(&dll, 2_000);
        assert_eq!(cache.read::<Vec<u32>>(&dll), None);
    }

    #[test]
    fn get_or_populate_caches_a_miss_and_hits_the_next_time() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = enabled_cache(tmp.path().join("cache"));
        let dll = dll_fixture(tmp.path(), "a.dll", b"stable bytes");
        set_mtime(&dll, 1_000);

        // Miss ⇒ compute runs and the result is persisted.
        assert_eq!(
            cache.get_or_populate(&dll, || Some(vec![7u32])),
            Some(vec![7])
        );
        // Warm hit ⇒ compute must not run again (the file is unchanged).
        assert_eq!(
            cache.get_or_populate::<Vec<u32>>(&dll, || panic!("should have hit the cache")),
            Some(vec![7])
        );
    }

    #[test]
    fn get_or_populate_does_not_persist_a_mid_compute_overwrite() {
        // TOCTOU: a DLL overwritten *while* its bytes are read+parsed yields a
        // value from the *old* bytes, but a stat-at-store would record the *new*
        // file's metadata — a warm start would then hand back the stale value.
        // `get_or_populate` snapshots the stat before compute and refuses to
        // persist when the file moved, which we drive from inside compute.
        let tmp = tempfile::tempdir().unwrap();
        let cache = enabled_cache(tmp.path().join("cache"));
        let dll = dll_fixture(tmp.path(), "a.dll", b"old contents..");
        set_mtime(&dll, 1_000);

        let got = cache.get_or_populate(&dll, || {
            // A concurrent restore rewrites the DLL (same length, new mtime)
            // during the read+parse this closure stands in for.
            std::fs::write(&dll, b"new contents..").unwrap();
            set_mtime(&dll, 2_000);
            Some(vec![42u32]) // "projected from the old bytes"
        });
        assert_eq!(got, Some(vec![42]), "the computed value is still returned");

        // Nothing was persisted: a fresh read misses, so a warm start recomputes
        // rather than trusting an entry whose metadata never matched its value.
        assert_eq!(
            cache.read::<Vec<u32>>(&dll),
            None,
            "a mid-compute overwrite must not be cached"
        );
    }

    #[test]
    fn miss_on_tag_change() {
        // An entry written under one tag is not read back under another (models a
        // projector change ⇒ new binary mtime ⇒ new tag).
        let tmp = tempfile::tempdir().unwrap();
        let dll = dll_fixture(tmp.path(), "a.dll", b"bytes");
        let dir = tmp.path().join("cache");

        let old = AssemblyCache {
            dir: Some(dir.clone()),
            tag: 111,
        };
        old.write(&dll, &vec![7u32]);
        assert_eq!(old.read::<Vec<u32>>(&dll), Some(vec![7]));

        let new = AssemblyCache {
            dir: Some(dir),
            tag: 222,
        };
        assert_eq!(new.read::<Vec<u32>>(&dll), None);
    }

    #[test]
    fn corrupt_entry_is_a_miss_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = enabled_cache(tmp.path().join("cache"));
        let dll = dll_fixture(tmp.path(), "a.dll", b"bytes");
        cache.write(&dll, &vec![1u32]);

        // Truncate/garble the entry file.
        let entry = cache.entry_path(&canonical_string(&dll)).unwrap();
        std::fs::write(&entry, b"garbage").unwrap();
        assert_eq!(cache.read::<Vec<u32>>(&dll), None);
    }

    #[test]
    fn huge_length_prefix_is_a_bounded_miss_not_an_oom() {
        // A tiny entry whose framing claims a `u64::MAX`-long string must be
        // rejected by the file-size limit *before* allocating — a miss, not a
        // multi-exabyte allocation. Framing: fixint LE magic(u32) + tag(u64) +
        // then the `path` String's length(u64).
        let tmp = tempfile::tempdir().unwrap();
        let cache = enabled_cache(tmp.path().join("cache"));
        let dll = dll_fixture(tmp.path(), "a.dll", b"bytes");
        let entry = cache.entry_path(&canonical_string(&dll)).unwrap();
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC.to_le_bytes()); // magic: u32
        bytes.extend_from_slice(&0u64.to_le_bytes()); // tag: u64
        bytes.extend_from_slice(&u64::MAX.to_le_bytes()); // path len: u64 — absurd
        std::fs::write(&entry, &bytes).unwrap();

        assert_eq!(cache.read::<Vec<u32>>(&dll), None);
    }

    #[test]
    fn off_posix_rename_degrades_to_disabled() {
        // On an OS whose `rename` can't overwrite (Windows), an enabled cache
        // must degrade to disabled rather than risk a stale entry the atomic
        // replace can't overwrite. The POSIX gate is injected so this exercises
        // the non-Unix branch on a Unix host.
        let dir = PathBuf::from("/some/cache/dir");
        assert!(
            AssemblyCache::enabled_when(dir.clone(), false)
                .dir
                .is_none(),
            "off-POSIX an enabled cache must be disabled (no dir)"
        );
        assert_eq!(
            AssemblyCache::enabled_when(dir.clone(), true).dir,
            Some(dir),
            "on POSIX the cache keeps its directory"
        );
    }

    #[test]
    fn disabled_never_touches_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let dll = dll_fixture(tmp.path(), "a.dll", b"bytes");
        let cache = AssemblyCache::disabled();
        cache.write(&dll, &vec![1u32]);
        assert_eq!(cache.read::<Vec<u32>>(&dll), None);
        assert!(cache.entry_path("anything").is_none());
    }

    #[test]
    fn empty_env_var_disables() {
        // Can't safely mutate process env in parallel tests; assert the mapping
        // directly on the resolver instead.
        assert_eq!(default_cache_dir_from(Some(String::new())), None);
        assert_eq!(
            default_cache_dir_from(Some("/x".to_string())),
            Some(PathBuf::from("/x"))
        );
    }

    /// Pure form of the `from_env` directory decision, for the env-mapping test
    /// (the real `from_env` reads the process environment).
    fn default_cache_dir_from(var: Option<String>) -> Option<PathBuf> {
        match var {
            Some(v) if v.is_empty() => None,
            Some(v) => Some(PathBuf::from(v)),
            None => default_cache_dir(),
        }
    }

    #[test]
    fn cache_root_prefers_xdg_then_home() {
        use std::ffi::OsString;
        // XDG wins when present.
        assert_eq!(
            default_cache_root_from(
                Some(OsString::from("/xdg")),
                Some(OsString::from("/home/u"))
            ),
            Some(PathBuf::from("/xdg/borzoi"))
        );
        // Falls back to `$HOME/.cache` when XDG is unset (or empty).
        assert_eq!(
            default_cache_root_from(None, Some(OsString::from("/home/u"))),
            Some(PathBuf::from("/home/u/.cache/borzoi"))
        );
        assert_eq!(
            default_cache_root_from(Some(OsString::new()), Some(OsString::from("/home/u"))),
            Some(PathBuf::from("/home/u/.cache/borzoi"))
        );
        // Nothing rootable → no cache root.
        assert_eq!(default_cache_root_from(None, None), None);
        assert_eq!(
            default_cache_root_from(Some(OsString::new()), Some(OsString::new())),
            None
        );
        // The assembly cache's `entities` leaf hangs off the shared root, a
        // sibling of the source cache's `sources`.
        assert_eq!(
            default_cache_root_from(Some(OsString::from("/xdg")), None).map(|r| r.join("entities")),
            Some(PathBuf::from("/xdg/borzoi/entities"))
        );
    }
}

//! Differential for documentation-comment IDs on **C#-compiled** assemblies,
//! against the keys shipped in the targeting packs a project compiles against.
//!
//! Hover finds a referenced member's documentation by computing its ID with
//! [`walk_doc_ids`] and looking it up in the DLL's sidecar `.xml`. The packs are
//! the docs a real F# project actually reads: `Microsoft.NETCore.App.Ref`, and
//! `Microsoft.AspNetCore.App.Ref` for a web project. Each is graded the same
//! way: **every shipped `<member name>` key must be among the IDs we generate
//! for that DLL**, and every key that is not is pinned, with the cause of the
//! miss, in an exact per-pack manifest under `tests/manifests/doc_id_csharp/`.
//! Movement in either direction fails with a line diff; an intended movement is
//! acknowledged by regenerating:
//!
//! ```sh
//! BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-assembly --test all doc_id_csharp_diff::
//! ```
//!
//! # Every miss carries a mechanically decided cause
//!
//! The pack XMLs are not fresh Roslyn output. They come from the .NET docs
//! pipeline, which keys some members in its own dialect, and they document
//! members the reference assembly does not carry. A miss is therefore not by
//! itself a generator bug, but nothing here labels one by hand. [`classify`]
//! decides each miss's cause by predicate, and a miss no predicate explains is
//! `unexplained`, which fails the run *before* the manifest is consulted, so
//! regeneration cannot bless it. A cause is one of two kinds:
//!
//! - **A spelling** ([`Rewrite`]): a set of named rewrites, applied to both the
//!   shipped key and our IDs, makes the key equal to an ID we generate (and that
//!   ID is not itself a shipped key, so it is no other entry's). The smallest
//!   such set is the cause, so every rewrite named in it was needed. Most are
//!   the explicit-interface spellings that issue #96's lookup retry targets: an
//!   `@` between interface type arguments, `System#IntPtr` for `nint`, and
//!   literal `<…>` for `{…}`.
//! - **An absence**: the declaring type, the member name, or an overload of the
//!   key's shape (generic arity, parameter count, each parameter's outer type)
//!   is not in this DLL — implementation internals such as `System.SR`, or an
//!   overload from another version. Absence is judged against our own
//!   projection, so a member the reader dropped reads as absent; the reader's
//!   own records of what it dropped (`skipped_members`, the dropped-type list)
//!   are consulted first and get their own buckets, and `bcl_ref_pack_sweep`
//!   pins the projection itself exactly.
//!
//! A predicate states a textual relationship, not who is at fault: a generator
//! regression that, say, started emitting a spurious `@` would land its keys in
//! `receiver-byref-omitted`. The manifest is what catches that — the bucket's
//! lines move — and the bucket on each line says where to look.
//!
//! The absence predicate is coarse in the safe direction: an absent overload
//! whose every parameter has the outer type of a present overload's is not
//! recognised, and fails the run as `unexplained` rather than being blessed.
//!
//! The classifier is checked against `System.Runtime`'s real IDs:
//! [`classifier_names_a_planted_dialect_spelling`] plants each spelling into
//! them and requires that cause back, and
//! [`classifier_does_not_explain_an_absent_overload_as_a_spelling`] removes each
//! method in turn and requires that its own ID is not then explained as a
//! spelling of a sibling.
//!
//! # What pins the input
//!
//! As in `bcl_ref_pack_sweep`: the packs are those of the SDK on `PATH`, which
//! under `nix develop` (how CI and every documented command run this) is the
//! flake's pinned SDK. The manifest's first line names the pack version, so on
//! another SDK the run fails on that line first.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use borzoi_assembly::doc_id::{TypeDocName, member_doc_id, type_doc_name, walk_doc_ids};
use borzoi_assembly::{Access, Ecma335Assembly, EcmaView, Entity, Member};
use borzoi_oracle_harness::manifest::{self, Manifest};

use crate::common::sdk_targeting_pack_dir;

const REGENERATE: &str = "BORZOI_UPDATE_MANIFESTS=1 nix develop -c cargo test -p borzoi-assembly \
     --test all doc_id_csharp_diff::";

// ============================================================================
// Key structure
// ============================================================================

/// A documentation-comment ID split at the boundaries the rewrites act on:
/// `M:<decl>.<member>(<params>)~<ret>`. A `T:` key has an empty `member` and no
/// parameters.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    prefix: char,
    /// The declaring type's doc name (for `T:`, the type itself).
    decl: String,
    /// The member-name portion, including a method's ``` ``n ``` arity. For an
    /// explicit-interface implementation it embeds the interface
    /// (`System#IComparable{System#Byte}#CompareTo`).
    member: String,
    params: Option<Vec<String>>,
    ret: Option<String>,
}

/// Bracket depth deltas for every bracket a key can carry: braces (generic
/// arguments), angle brackets (a docs-pipeline explicit-interface name), square
/// brackets (array shapes) and parentheses.
fn depth_delta(c: char) -> i32 {
    match c {
        '{' | '<' | '[' | '(' => 1,
        '}' | '>' | ']' | ')' => -1,
        _ => 0,
    }
}

/// Split `s` at every `sep` outside brackets.
fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        depth += depth_delta(c);
        if c == sep && depth == 0 {
            out.push(s[start..i].to_string());
            start = i + c.len_utf8();
        }
    }
    out.push(s[start..].to_string());
    out
}

impl Key {
    /// Parse a key, or `None` when it is not shaped like a documentation-comment
    /// ID at all (an unknown prefix, unbalanced brackets, a member key with no
    /// declaring type).
    fn parse(key: &str) -> Option<Key> {
        let (prefix, body) = key.split_once(':')?;
        let prefix = match prefix {
            "T" | "M" | "P" | "F" | "E" => prefix.chars().next()?,
            _ => return None,
        };
        // The head ends at the first `(` or `~` outside brackets.
        let mut depth = 0;
        let mut cut = body.len();
        for (i, c) in body.char_indices() {
            if depth == 0 && (c == '(' || c == '~') {
                cut = i;
                break;
            }
            depth += depth_delta(c);
        }
        let (head, mut tail) = body.split_at(cut);
        let (decl, member) = if prefix == 'T' {
            (head.to_string(), String::new())
        } else {
            let mut depth = 0;
            let mut last_dot = None;
            for (i, c) in head.char_indices() {
                depth += depth_delta(c);
                if c == '.' && depth == 0 {
                    last_dot = Some(i);
                }
            }
            let dot = last_dot?;
            (head[..dot].to_string(), head[dot + 1..].to_string())
        };
        let params = match tail.strip_prefix('(') {
            Some(rest) => {
                let mut depth = 1;
                let close = rest.char_indices().find_map(|(i, c)| {
                    depth += depth_delta(c);
                    (depth == 0).then_some(i)
                })?;
                tail = &rest[close + 1..];
                Some(split_top_level(&rest[..close], ','))
            }
            None => None,
        };
        let ret = match tail {
            "" => None,
            t => Some(t.strip_prefix('~')?.to_string()),
        };
        Some(Key {
            prefix,
            decl,
            member,
            params,
            ret,
        })
    }

    /// The member name and its method generic arity: a trailing ``` ``n ```
    /// (one inside an explicit-interface name's brackets is a type argument).
    fn split_arity(&self) -> (&str, usize) {
        match self.member.rsplit_once("``") {
            Some((name, n))
                if !n.is_empty()
                    && n.bytes().all(|b| b.is_ascii_digit())
                    && name.chars().map(depth_delta).sum::<i32>() == 0 =>
            {
                (name, n.parse().expect("ascii digits"))
            }
            _ => (&self.member, 0),
        }
    }

    /// The member name without its method generic arity.
    fn bare_member(&self) -> &str {
        self.split_arity().0
    }

    /// The method generic arity, `0` when there is none.
    fn method_arity(&self) -> usize {
        self.split_arity().1
    }

    /// The overload shape an absence is judged by: method generic arity,
    /// parameter count, and each parameter's outer type name (everything before
    /// its first generic, array, byref, pointer or modifier mark).
    fn shape(&self) -> (usize, Option<Vec<String>>) {
        let heads = self.params.as_ref().map(|ps| {
            ps.iter()
                .map(|p| {
                    let end = p.find(['{', '[', '@', '*', '|', '!']).unwrap_or(p.len());
                    p[..end].to_string()
                })
                .collect()
        });
        (self.method_arity(), heads)
    }
}

// ============================================================================
// Spelling causes
// ============================================================================

/// One spelling difference between a shipped key and the ID our generator
/// computes for the same member. Each is applied to *both* sides, so its
/// direction does not matter: it maps both spellings to one form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rewrite {
    /// An explicit-interface name keeps the interface's `<…>` rather than
    /// mapping it to `{…}` (`IBinaryInteger<System#Byte>#GetShortestBitLength`).
    EiAngleBrackets,
    /// An explicit-interface name separates the interface's type arguments with
    /// `@` rather than `,` (the NETCore packs' spelling, in every version).
    EiAtSeparator,
    /// An explicit-interface name spells a native-sized integer argument
    /// `System#IntPtr`/`System#UIntPtr` where the IL name (C#'s rendering, which
    /// our ID keeps) says `nint`/`nuint`.
    EiNativeIntAlias,
    /// The key encodes a parameter's custom modifier
    /// (`System.Guid@|System.Runtime.InteropServices.InAttribute`).
    CustomModifier,
    /// The key omits the `@` of a by-reference *first* parameter — an extension
    /// method's `ref this` / `in this` receiver.
    ReceiverByref,
    /// The key names a type parameter (`T[]`) where the ID uses its position
    /// (`` `0[] ``). Judged with every type-parameter token wildcarded on both
    /// sides, since the name used need not be the declared one.
    TyparByName,
    /// The key's method generic arity differs from the method's.
    MethodArity,
}

impl Rewrite {
    const ALL: [Rewrite; 7] = [
        Rewrite::EiAngleBrackets,
        Rewrite::EiAtSeparator,
        Rewrite::EiNativeIntAlias,
        Rewrite::CustomModifier,
        Rewrite::ReceiverByref,
        Rewrite::TyparByName,
        Rewrite::MethodArity,
    ];

    fn label(self) -> &'static str {
        match self {
            Rewrite::EiAngleBrackets => "ei-angle-brackets",
            Rewrite::EiAtSeparator => "ei-at-separator",
            Rewrite::EiNativeIntAlias => "ei-nint-alias",
            Rewrite::CustomModifier => "custom-modifier-in-key",
            Rewrite::ReceiverByref => "receiver-byref-omitted",
            Rewrite::TyparByName => "typar-by-name",
            Rewrite::MethodArity => "method-arity-differs",
        }
    }

    fn apply(self, key: &mut Key) {
        match self {
            Rewrite::EiAngleBrackets => {
                key.member = key.member.replace('<', "{").replace('>', "}");
            }
            Rewrite::EiAtSeparator => {
                let mut depth = 0;
                key.member = key
                    .member
                    .chars()
                    .map(|c| {
                        depth += depth_delta(c);
                        if c == '@' && depth > 0 { ',' } else { c }
                    })
                    .collect();
            }
            Rewrite::EiNativeIntAlias => {
                key.member = alias_native_ints(&key.member);
            }
            Rewrite::CustomModifier => {
                map_signature(key, strip_modifiers);
            }
            Rewrite::ReceiverByref => {
                if let Some(first) = key.params.as_mut().and_then(|ps| ps.first_mut())
                    && first.ends_with('@')
                {
                    first.pop();
                }
            }
            Rewrite::TyparByName => {
                map_signature(key, wildcard_typars);
            }
            Rewrite::MethodArity => {
                key.member = key.bare_member().to_string();
            }
        }
    }
}

/// Apply `f` to every parameter and the return type.
fn map_signature(key: &mut Key, f: fn(&str) -> String) {
    if let Some(ps) = key.params.as_mut() {
        for p in ps.iter_mut() {
            *p = f(p);
        }
    }
    if let Some(r) = key.ret.as_mut() {
        *r = f(r);
    }
}

/// Inside the brackets of an explicit-interface name, the type-argument tokens
/// `System#IntPtr`/`System#UIntPtr` become `nint`/`nuint`.
fn alias_native_ints(member: &str) -> String {
    let mut out = String::new();
    let mut token = String::new();
    let mut depth = 0;
    let flush = |token: &mut String, out: &mut String, depth: i32| {
        let aliased = match token.as_str() {
            "System#IntPtr" if depth > 0 => "nint",
            "System#UIntPtr" if depth > 0 => "nuint",
            t => t,
        };
        out.push_str(aliased);
        token.clear();
    };
    for c in member.chars() {
        if matches!(c, '{' | '}' | '<' | '>' | ',' | '@' | '[' | ']' | '*') {
            flush(&mut token, &mut out, depth);
            depth += depth_delta(c);
            out.push(c);
        } else {
            token.push(c);
        }
    }
    flush(&mut token, &mut out, depth);
    out
}

/// Drop every `|Modifier` / `!Modifier` suffix from a type encoding.
fn strip_modifiers(ty: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for c in ty.chars() {
        if c == '|' || c == '!' {
            skipping = true;
        } else if skipping && matches!(c, '{' | '}' | '[' | ']' | '@' | '*' | ',') {
            skipping = false;
            out.push(c);
        } else if !skipping {
            out.push(c);
        }
    }
    out
}

/// Replace every type-parameter token in a type encoding — positional
/// (`` `0 ``, ``` ``1 ```) or by name (a token with no namespace dot) — by `?`.
fn wildcard_typars(ty: &str) -> String {
    let mut out = String::new();
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        let is_typar = token.starts_with('`')
            || (!token.is_empty()
                && !token.contains('.')
                && !token.chars().all(|c| c.is_ascii_digit() || c == ':'));
        out.push_str(if is_typar { "?" } else { token });
        token.clear();
    };
    for c in ty.chars() {
        if matches!(c, '{' | '}' | '[' | ']' | ',' | '@' | '*') {
            flush(&mut token, &mut out);
            out.push(c);
        } else {
            token.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

/// Every non-empty set of rewrites, smallest first (then in [`Rewrite::ALL`]
/// order), each as its members in application order.
fn rewrite_sets() -> Vec<Vec<Rewrite>> {
    let n = Rewrite::ALL.len();
    let mut sets: Vec<Vec<Rewrite>> = (1u32..(1 << n))
        .map(|mask| {
            Rewrite::ALL
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, r)| *r)
                .collect()
        })
        .collect();
    sets.sort_by_key(|s| s.len());
    sets
}

fn rewritten(key: &Key, set: &[Rewrite]) -> Key {
    let mut k = key.clone();
    for r in set {
        r.apply(&mut k);
    }
    k
}

// ============================================================================
// One DLL's IDs
// ============================================================================

/// What we generate for one DLL, indexed for [`classify`].
struct Ours {
    /// Every generated ID with its multiplicity (more than one is a collision).
    ids: BTreeMap<String, usize>,
    /// The parsed IDs, by declaring type.
    by_decl: HashMap<String, Vec<Sibling>>,
    /// Every type's doc name.
    types: HashSet<String>,
    /// Members the reader dropped, as `(declaring type, escaped member name)`.
    dropped_members: HashSet<(String, String)>,
    /// Types the reader dropped, by IL name.
    dropped_types: Vec<String>,
}

/// One generated ID, parsed, with its [`coarse`] form.
#[derive(Clone)]
struct Sibling {
    id: String,
    key: Key,
    coarse: Key,
}

/// `key` under every [`Rewrite`] at once. Two keys that some set of rewrites
/// makes equal are equal here too — the rewrites act on disjoint parts of a key,
/// or commute where they share one — so [`classify`] searches the rewrite sets
/// only among the candidates that agree with the shipped key on this form.
fn coarse(key: &Key) -> Key {
    rewritten(key, &Rewrite::ALL)
}

impl Ours {
    fn new(entities: &[Entity], dropped_types: Vec<String>) -> Ours {
        fn walk(
            e: &Entity,
            enclosing: Option<&TypeDocName>,
            dropped: &mut HashSet<(String, String)>,
        ) {
            let decl = type_doc_name(e, enclosing);
            for m in &e.skipped_members {
                let escaped = m.name.replace('.', "#").replace('<', "{").replace('>', "}");
                dropped.insert((decl.full().to_string(), escaped));
            }
            for n in &e.nested_types {
                walk(n, Some(&decl), dropped);
            }
        }
        let mut ids: BTreeMap<String, usize> = BTreeMap::new();
        let mut dropped_members = HashSet::new();
        for e in entities {
            walk_doc_ids(e, None, &mut |id| *ids.entry(id).or_default() += 1);
            walk(e, None, &mut dropped_members);
        }
        let mut by_decl: HashMap<String, Vec<Sibling>> = HashMap::new();
        let mut types = HashSet::new();
        for id in ids.keys() {
            let key = Key::parse(id).unwrap_or_else(|| panic!("our ID does not parse: {id}"));
            if key.prefix == 'T' {
                types.insert(key.decl.clone());
            }
            by_decl.entry(key.decl.clone()).or_default().push(Sibling {
                id: id.clone(),
                coarse: coarse(&key),
                key,
            });
        }
        Ours {
            ids,
            by_decl,
            types,
            dropped_members,
            dropped_types,
        }
    }
}

/// Why a shipped key is not among our IDs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Cause {
    /// The smallest set of [`Rewrite`]s that maps the key onto one of our IDs.
    Spelling(Vec<Rewrite>),
    /// No type of the key's declaring-type name is in the DLL.
    TypeAbsent,
    /// The reader dropped a type whose name matches the declaring type.
    TypeDroppedByReader,
    /// The reader dropped a member of this name from the declaring type.
    MemberDroppedByReader,
    /// The declaring type has no member of this kind and name.
    MemberAbsent,
    /// The declaring type has members of this kind and name, but none of the
    /// key's overload shape ([`Key::shape`]).
    OverloadAbsent,
    /// None of the above. Fails the run.
    Unexplained(&'static str),
}

impl Cause {
    fn label(&self) -> String {
        match self {
            Cause::Spelling(set) => set.iter().map(|r| r.label()).collect::<Vec<_>>().join("+"),
            Cause::TypeAbsent => "type-absent".into(),
            Cause::TypeDroppedByReader => "type-dropped-by-reader".into(),
            Cause::MemberDroppedByReader => "member-dropped-by-reader".into(),
            Cause::MemberAbsent => "member-absent".into(),
            Cause::OverloadAbsent => "overload-absent".into(),
            Cause::Unexplained(why) => format!("unexplained({why})"),
        }
    }
}

/// The member name an absence is judged by: arity dropped, and the
/// explicit-interface spellings folded, so a docs-pipeline spelling of a
/// present member is not read as absent.
fn absence_name(key: &Key) -> String {
    let mut k = key.clone();
    for r in [
        Rewrite::EiAngleBrackets,
        Rewrite::EiAtSeparator,
        Rewrite::EiNativeIntAlias,
        Rewrite::MethodArity,
    ] {
        r.apply(&mut k);
    }
    k.member
}

/// Decide why `key`, a shipped key that is not among `ours.ids`, misses.
/// `shipped` is every key of the same file: an ID that is itself a shipped key
/// documents its own member, so no rewrite may land on it.
fn classify(key: &str, ours: &Ours, shipped: &BTreeSet<String>, sets: &[Vec<Rewrite>]) -> Cause {
    let Some(parsed) = Key::parse(key) else {
        return Cause::Unexplained("not a documentation-comment ID");
    };
    let siblings: &[Sibling] = ours.by_decl.get(&parsed.decl).map_or(&[], Vec::as_slice);
    let parsed_coarse = coarse(&parsed);
    let candidates: Vec<&Key> = siblings
        .iter()
        .filter(|s| s.coarse == parsed_coarse && !shipped.contains(&s.id))
        .map(|s| &s.key)
        .collect();
    if !candidates.is_empty() {
        for set in sets {
            let target = rewritten(&parsed, set);
            if candidates.iter().any(|c| rewritten(c, set) == target) {
                return Cause::Spelling(set.clone());
            }
        }
    }

    if !ours.types.contains(&parsed.decl) {
        let flat = |s: &str| -> String {
            s.chars()
                .filter(|c| !matches!(c, '.' | '/' | '+'))
                .collect()
        };
        let decl = flat(&parsed.decl);
        return if ours.dropped_types.iter().any(|t| flat(t) == decl) {
            Cause::TypeDroppedByReader
        } else {
            Cause::TypeAbsent
        };
    }
    if parsed.prefix == 'T' {
        // Its own name is a type we generate, so it would have been a hit.
        return Cause::Unexplained("type key for a type we name");
    }
    let name = absence_name(&parsed);
    if ours
        .dropped_members
        .contains(&(parsed.decl.clone(), parsed.bare_member().to_string()))
    {
        return Cause::MemberDroppedByReader;
    }
    let same_name: Vec<&Key> = siblings
        .iter()
        .map(|s| &s.key)
        .filter(|k| absence_name(k) == name)
        .collect();
    if same_name.is_empty() {
        return Cause::MemberAbsent;
    }
    let same_kind: Vec<&&Key> = same_name
        .iter()
        .filter(|k| k.prefix == parsed.prefix)
        .collect();
    if same_kind.is_empty() {
        return Cause::Unexplained("a member of this name exists, of another kind");
    }
    let shape = parsed.shape();
    if same_kind.iter().all(|k| k.shape() != shape) {
        return Cause::OverloadAbsent;
    }
    Cause::Unexplained("an overload of this shape exists, spelled otherwise")
}

fn render(k: &Key) -> String {
    let mut s = format!("{}:{}", k.prefix, k.decl);
    if k.prefix != 'T' {
        s.push('.');
        s.push_str(&k.member);
    }
    if let Some(ps) = &k.params {
        s.push('(');
        s.push_str(&ps.join(","));
        s.push(')');
    }
    if let Some(r) = &k.ret {
        s.push('~');
        s.push_str(r);
    }
    s
}

// ============================================================================
// Shipped keys
// ============================================================================

/// The keys of a doc file, with their multiplicities, read the way the LSP's
/// index reads them: every `<member>` under a `<members>` element of the
/// `<doc>`, by its non-blank `name` attribute (entity-decoded, so a
/// docs-pipeline `&lt;` arrives as `<`).
fn shipped_keys(xml_path: &Path) -> BTreeMap<String, usize> {
    let text = std::fs::read_to_string(xml_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", xml_path.display()));
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let doc = roxmltree::Document::parse(text)
        .unwrap_or_else(|e| panic!("parse {}: {e}", xml_path.display()));
    let mut keys = BTreeMap::new();
    for member in doc
        .descendants()
        .filter(|n| n.has_tag_name("member"))
        .filter(|m| m.ancestors().any(|a| a.has_tag_name("members")))
    {
        if let Some(name) = member.attribute("name").filter(|n| !n.trim().is_empty()) {
            *keys.entry(name.to_string()).or_default() += 1;
        }
    }
    keys
}

// ============================================================================
// Grading
// ============================================================================

/// The pack's identity as a manifest entry: `(pack) <name> <version>/ref/<tfm>`.
fn pack_entry(pack: &str, dir: &Path) -> String {
    let tail: Vec<String> = dir
        .components()
        .rev()
        .take(3)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let tail: Vec<&str> = tail.iter().rev().map(String::as_str).collect();
    format!("(pack) {pack} {}", tail.join("/"))
}

/// One graded DLL: its manifest lines and the misses no cause explains.
struct Graded {
    entries: Vec<String>,
    unexplained: Vec<String>,
    causes: BTreeMap<String, usize>,
    keys: usize,
}

/// Grade one DLL against the doc XML beside it.
fn grade(dll_name: &str, dll: &Path, sets: &[Vec<Rewrite>]) -> Graded {
    let xml = dll.with_extension("xml");
    let bytes = std::fs::read(dll).unwrap_or_else(|e| panic!("read {dll_name}: {e}"));
    let view = Ecma335Assembly::parse(&bytes)
        .unwrap_or_else(|e| panic!("{dll_name} failed to parse: {e}"));
    // The scope is C#-compiled assemblies; an F# one keys in fsc's dialect,
    // which `doc_id_fsharp_diff` grades.
    let fsharp = view
        .fsharp_resources()
        .unwrap_or_else(|e| panic!("{dll_name}: read resources: {e}"));
    assert!(
        fsharp.is_empty(),
        "{dll_name} carries F# signature resources; this differential grades C#-compiled \
         assemblies only"
    );
    let (entities, skips) = view
        .enumerate_type_defs_with_skips()
        .unwrap_or_else(|e| panic!("{dll_name} failed to enumerate: {e}"));
    let ours = Ours::new(
        &entities,
        skips.dropped_types.iter().map(|t| t.name.clone()).collect(),
    );

    let mut graded = Graded {
        entries: Vec::new(),
        unexplained: Vec::new(),
        causes: BTreeMap::new(),
        keys: 0,
    };
    for (id, n) in ours.ids.iter().filter(|(_, n)| **n > 1) {
        graded.entries.push(format!("{dll_name} dup {id} x{n}"));
    }
    if !xml.is_file() {
        graded.entries.push(format!("{dll_name} no-xml"));
        return graded;
    }
    let counted = shipped_keys(&xml);
    for (key, n) in counted.iter().filter(|(_, n)| **n > 1) {
        graded
            .entries
            .push(format!("{dll_name} shipped-dup {key} x{n}"));
    }
    let shipped: BTreeSet<String> = counted.into_keys().collect();
    graded.keys = shipped.len();
    let mut hits = 0usize;
    for key in &shipped {
        if ours.ids.contains_key(key) {
            hits += 1;
            continue;
        }
        let cause = classify(key, &ours, &shipped, sets);
        let label = cause.label();
        if matches!(cause, Cause::Unexplained(_)) {
            graded.unexplained.push(format!("{dll_name} {label} {key}"));
        }
        *graded.causes.entry(label.clone()).or_default() += 1;
        graded
            .entries
            .push(format!("{dll_name} miss {label} {key}"));
    }
    graded
        .entries
        .push(format!("{dll_name} keys={} hits={hits}", shipped.len()));
    graded
}

/// Every `.dll` in a pack directory, sorted by file name.
fn pack_dlls(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut dlls: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read pack dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dll"))
        .map(|p| {
            let name = p
                .file_name()
                .expect("dll path has a file name")
                .to_string_lossy()
                .into_owned();
            (name, p)
        })
        .collect();
    dlls.sort();
    assert!(!dlls.is_empty(), "no assemblies in {dir:?}");
    dlls
}

fn manifest_path(pack: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("manifests")
        .join("doc_id_csharp")
        .join(format!("{pack}.txt"))
}

/// Grade every DLL of `pack` and check the result against its manifest.
fn check_pack(pack: &str) {
    let dir = sdk_targeting_pack_dir(pack);
    let sets = rewrite_sets();
    let mut entries = vec![pack_entry(pack, &dir)];
    let mut unexplained = Vec::new();
    let mut causes: BTreeMap<String, usize> = BTreeMap::new();
    let mut keys = 0usize;
    for (name, path) in pack_dlls(&dir) {
        let graded = grade(&name, &path, &sets);
        keys += graded.keys;
        entries.extend(graded.entries);
        unexplained.extend(graded.unexplained);
        for (label, n) in graded.causes {
            *causes.entry(label).or_default() += n;
        }
    }
    let missed: usize = causes.values().sum();
    eprintln!(
        "[doc_id_csharp_diff] {}: {keys} shipped keys, {missed} missed, by cause:",
        pack_entry(pack, &dir)
    );
    for (label, n) in &causes {
        eprintln!("  {n:>6} {label}");
    }
    // Asserted before the manifest, so regeneration cannot bless them: a pack
    // with next to no documentation grades nothing, and a miss whose cause no
    // predicate states is unattributed.
    assert!(
        keys >= 10_000,
        "{pack}: only {keys} shipped keys — the grading would be vacuous"
    );
    assert!(
        unexplained.is_empty(),
        "{} shipped keys miss for a cause no predicate explains:\n{}",
        unexplained.len(),
        unexplained.join("\n")
    );
    let actual = Manifest::from_entries(entries).unwrap_or_else(|e| panic!("{pack}: {e}"));
    manifest::check(&manifest_path(pack), &actual, REGENERATE);
}

#[test]
fn netcore_app_ref() {
    check_pack("Microsoft.NETCore.App.Ref");
}

#[test]
fn aspnetcore_app_ref() {
    check_pack("Microsoft.AspNetCore.App.Ref");
}

/// Every ID an explicit-interface spelling rewrite can change belongs to a
/// member no F# use resolves to.
///
/// The `ei-*` causes — most of the misses — concern IDs whose member-name
/// portion has brackets in it (`System#IComparable{System#Byte}#CompareTo`); a
/// rewrite that re-spells them (issue #96's lookup retry) changes no other ID.
/// This pins, over both packs, that such a member is either
///
/// - an explicit interface implementation (its IL name is dotted), and every
///   one of those is non-public — C# emits them `private`; or
/// - compiler-generated (`<>9`, `<>1__state`: its IL name has a `<`), which no
///   doc file documents.
///
/// Name resolution (`borzoi-sema`) names an assembly member as a use's target
/// only through lookups that keep `public` members (`member_is_public` in its
/// `assembly_env`), and hover asks for the docs of exactly that target. A use
/// of `CompareTo` through the interface names `IComparable`1.CompareTo`, whose
/// ID has no brackets. So hover never computes an ID these rewrites change.
#[test]
fn explicit_interface_ids_belong_to_non_public_members() {
    #[derive(Default)]
    struct Tally {
        explicit_impls: usize,
        compiler_generated: usize,
        violations: Vec<String>,
    }
    fn walk(e: &Entity, enclosing: Option<&TypeDocName>, tally: &mut Tally) {
        let decl = type_doc_name(e, enclosing);
        for m in &e.members {
            let id = member_doc_id(&decl, m);
            let key = Key::parse(&id).unwrap_or_else(|| panic!("our ID does not parse: {id}"));
            let (il_name, access) = match m {
                Member::Method(m) => (&m.name, m.access),
                Member::Field(f) => (&f.name, f.access),
                Member::Property(p) => (&p.name, p.access),
                Member::Event(ev) => (&ev.name, ev.access),
            };
            let explicit_impl = il_name.contains('.') && !il_name.starts_with('.');
            if explicit_impl {
                tally.explicit_impls += 1;
                if access == Access::Public {
                    tally.violations.push(format!("public explicit impl: {id}"));
                }
            } else if key.bare_member().contains(['{', '<']) {
                if il_name.contains('<') {
                    tally.compiler_generated += 1;
                } else {
                    tally
                        .violations
                        .push(format!("bracketed ID, neither impl nor generated: {id}"));
                }
            }
        }
        for n in &e.nested_types {
            walk(n, Some(&decl), tally);
        }
    }
    let mut tally = Tally::default();
    for pack in ["Microsoft.NETCore.App.Ref", "Microsoft.AspNetCore.App.Ref"] {
        for (name, path) in pack_dlls(&sdk_targeting_pack_dir(pack)) {
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
            let view = Ecma335Assembly::parse(&bytes)
                .unwrap_or_else(|e| panic!("{name} failed to parse: {e}"));
            let entities = view
                .enumerate_type_defs()
                .unwrap_or_else(|e| panic!("{name} failed to enumerate: {e}"));
            for e in &entities {
                walk(e, None, &mut tally);
            }
        }
    }
    eprintln!(
        "[doc_id_csharp_diff] {} explicit interface impls (none public), {} compiler-generated \
         bracketed IDs",
        tally.explicit_impls, tally.compiler_generated
    );
    assert!(
        tally.explicit_impls > 1_000,
        "only {} explicit interface impls — vacuous",
        tally.explicit_impls
    );
    assert!(
        tally.violations.is_empty(),
        "{} members break the premise:\n{}",
        tally.violations.len(),
        tally.violations.join("\n")
    );
}

// ============================================================================
// The classifier itself
// ============================================================================

/// The inverse of each spelling [`Rewrite`]: the docs-pipeline spelling of one
/// of our IDs, or `None` when the rewrite does not apply to it.
fn plant(rewrite: Rewrite, ours: &Key) -> Option<Key> {
    let mut k = ours.clone();
    let in_ei_braces = |member: &str, f: &dyn Fn(char) -> Option<String>| -> String {
        let mut depth = 0;
        let mut out = String::new();
        for c in member.chars() {
            depth += depth_delta(c);
            match f(c) {
                Some(s) if depth > 0 => out.push_str(&s),
                _ => out.push(c),
            }
        }
        out
    };
    match rewrite {
        Rewrite::EiAngleBrackets => {
            k.member = k.member.replace('{', "<").replace('}', ">");
        }
        Rewrite::EiAtSeparator => {
            k.member = in_ei_braces(&k.member, &|c| (c == ',').then(|| "@".to_string()));
        }
        Rewrite::EiNativeIntAlias => {
            let parts: Vec<String> = split_ei_tokens(&k.member);
            k.member = parts
                .iter()
                .map(|t| match t.as_str() {
                    "nint" => "System#IntPtr".to_string(),
                    "nuint" => "System#UIntPtr".to_string(),
                    t => t.to_string(),
                })
                .collect();
        }
        Rewrite::CustomModifier => {
            let first = k.params.as_mut()?.first_mut()?;
            first.push_str("|System.Runtime.InteropServices.InAttribute");
        }
        Rewrite::ReceiverByref => {
            let first = k.params.as_mut()?.first_mut()?;
            first.strip_suffix('@')?;
            first.pop();
        }
        Rewrite::TyparByName => {
            let renamed: Vec<String> = k
                .params
                .as_ref()?
                .iter()
                .map(|p| p.replace("``0", "TMethod").replace("`0", "T"))
                .collect();
            k.params = Some(renamed);
        }
        Rewrite::MethodArity => {
            let arity = k.method_arity();
            if arity == 0 {
                return None;
            }
            k.member = format!("{}``{}", k.bare_member(), arity + 1);
        }
    }
    (k != *ours).then_some(k)
}

/// `member` split into tokens and the bracket/separator characters between
/// them, so concatenating the pieces gives it back.
fn split_ei_tokens(member: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut token = String::new();
    for c in member.chars() {
        if matches!(c, '{' | '}' | ',' | '[' | ']' | '*') {
            out.push(std::mem::take(&mut token));
            out.push(c.to_string());
        } else {
            token.push(c);
        }
    }
    out.push(token);
    out
}

/// The classifier names each planted spelling. For every ID we generate for
/// `System.Runtime.dll` and every [`Rewrite`] that applies to it, spell the ID
/// the docs pipeline's way, and require [`classify`] to answer exactly that
/// rewrite — over the generator's real output rather than hand-made keys, so
/// the shapes are the ones the packs carry.
///
/// A planted key that collides with another real ID, or that a *smaller* set
/// already explains, is skipped (the minimal-set rule then names the other
/// cause, correctly). The test fails if a rewrite was never exercised, so it
/// cannot pass on a generator that stopped producing the shape.
#[test]
fn classifier_names_a_planted_dialect_spelling() {
    let dll = sdk_targeting_pack_dir("Microsoft.NETCore.App.Ref").join("System.Runtime.dll");
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let entities = view
        .enumerate_type_defs()
        .expect("enumerate System.Runtime");
    let ours = Ours::new(&entities, Vec::new());
    let sets = rewrite_sets();
    let mut exercised: BTreeMap<Rewrite, usize> = BTreeMap::new();
    let mut wrong = Vec::new();
    for id in ours.ids.keys() {
        let key = Key::parse(id).expect("our ID parses");
        for rewrite in Rewrite::ALL {
            let Some(planted) = plant(rewrite, &key) else {
                continue;
            };
            let planted = render(&planted);
            if ours.ids.contains_key(&planted) {
                continue;
            }
            // Grade it as the only shipped key, so the original ID is a
            // candidate the classifier may land on.
            let shipped = BTreeSet::from([planted.clone()]);
            let cause = classify(&planted, &ours, &shipped, &sets);
            *exercised.entry(rewrite).or_default() += 1;
            if cause != Cause::Spelling(vec![rewrite]) {
                wrong.push(format!(
                    "{}: planted {planted} (from {id}), classified {}",
                    rewrite.label(),
                    cause.label()
                ));
            }
        }
    }
    let unexercised: Vec<&str> = Rewrite::ALL
        .iter()
        .filter(|r| !exercised.contains_key(r))
        .map(|r| r.label())
        .collect();
    assert!(
        unexercised.is_empty(),
        "rewrites never planted: {unexercised:?}"
    );
    assert!(
        wrong.is_empty(),
        "{} planted spellings misclassified:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// An absence is not mistaken for a spelling. Remove each method ID from our
/// set in turn (as if the DLL lacked that overload) and grade the ID itself as
/// the shipped key: no rewrite may claim it, since the only member it could be
/// a spelling of is gone — unless a sibling overload really does differ from
/// it by exactly one rewrite's difference, which is then counted, and checked
/// to be rare.
#[test]
fn classifier_does_not_explain_an_absent_overload_as_a_spelling() {
    let dll = sdk_targeting_pack_dir("Microsoft.NETCore.App.Ref").join("System.Runtime.dll");
    let bytes = std::fs::read(&dll).expect("read System.Runtime.dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse System.Runtime.dll");
    let entities = view
        .enumerate_type_defs()
        .expect("enumerate System.Runtime");
    let mut ours = Ours::new(&entities, Vec::new());
    let sets = rewrite_sets();
    let mut outcomes: BTreeMap<String, usize> = BTreeMap::new();
    let decls: Vec<String> = ours.by_decl.keys().cloned().collect();
    for decl in decls {
        let count = ours.by_decl[&decl].len();
        for i in 0..count {
            if ours.by_decl[&decl][i].key.prefix != 'M' {
                continue;
            }
            let removed = ours.by_decl.get_mut(&decl).expect("decl").remove(i);
            let id = removed.id.clone();
            let shipped = BTreeSet::from([id.clone()]);
            let cause = classify(&id, &ours, &shipped, &sets);
            *outcomes.entry(cause.label()).or_default() += 1;
            ours.by_decl
                .get_mut(&decl)
                .expect("decl")
                .insert(i, removed);
        }
    }
    eprintln!("[doc_id_csharp_diff] removed-overload outcomes: {outcomes:#?}");
    let total: usize = outcomes.values().sum();
    assert!(total > 5_000, "only {total} methods removed — vacuous");
    // `unexplained` is a safe answer (the pack gate fails on it), and the
    // absences are right. A spelling is wrong — except `method-arity-differs`,
    // which cannot tell a removed `Foo``2(X)` from a misnumbered spelling of a
    // sibling `Foo``1(X)`.
    let misattributed: Vec<(&String, &usize)> = outcomes
        .iter()
        .filter(|(label, _)| {
            !label.starts_with("unexplained")
                && !matches!(
                    label.as_str(),
                    "member-absent" | "overload-absent" | "method-arity-differs"
                )
        })
        .collect();
    assert!(
        misattributed.is_empty(),
        "removed overloads read as a spelling of a sibling: {misattributed:?}"
    );
}

//! The XML-doc reader and renderer over real shipped documentation files.
//!
//! The property tests beside the renderer prove it total and its Markdown
//! faithful over *generated* trees; this sweep is the other half — the
//! vocabulary real files actually use. Over every doc file of the newest
//! targeting pack the SDK carries (`Microsoft.NETCore.App.Ref`,
//! `Microsoft.AspNetCore.App.Ref`, …) and the SDK's `FSharp.Core.xml` it
//! asserts:
//!
//! - every file indexes (none is malformed, foreign, or a redirect stub);
//! - every entry's range re-parse — the lazy path hover takes — yields exactly
//!   the element the whole-document parse holds (the equivalence
//!   `xml_doc::file` relies on);
//! - every entry renders, and its Markdown parses back with no structure the
//!   renderer never emits (a heading, a quote, raw HTML, a table, a rule): doc
//!   text that leaked into Markdown syntax;
//! - **no tag falls back**: every tag these files use has an explicit mapping
//!   (or an explicit "metadata, not rendered" decision).
//!
//! It prints the census — files, entries, fallbacks by tag, `<inheritdoc>` and
//! `<include>` prevalence, ambiguous keys — and the parse time of the largest
//! file. `BORZOI_XML_DOC_SWEEP_ROOT` (e.g. `~/.nuget/packages`) adds every
//! neutral-culture `.xml` with a sibling `.dll` under that root to the sweep:
//! there, a file that fails to index and a tag that falls back are *reported*,
//! not failed, because the wild vocabulary is open-ended (typos, HTML, MSBuild
//! snippets); everything else is still asserted.
//!
//! ```sh
//! nix develop -c cargo test -p borzoi --test all xml_doc_shipped_sweep:: -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use borzoi::xml_doc::file::{DocEntry, DocFile};
use borzoi::xml_doc::markdown::to_markdown;
use borzoi::xml_doc::render::render_member;
use borzoi::xml_doc::tree::DocElement;
use pulldown_cmark::{Event, Options, Parser, Tag};

use crate::common::ensure_system_runtime_dll;

#[derive(Default)]
struct Census {
    files: usize,
    unindexed: Vec<(PathBuf, String)>,
    entries: usize,
    ambiguous: usize,
    unknown: BTreeMap<String, usize>,
    inheritdoc: usize,
    include: usize,
    slowest: Option<(Duration, PathBuf, usize)>,
}

/// The SDK's `FSharp.Core.xml`, newest SDK first.
fn sdk_fsharp_core_xml() -> PathBuf {
    let dotnet_root = PathBuf::from(std::env::var_os("DOTNET_ROOT").expect("DOTNET_ROOT unset"));
    let mut found: Vec<PathBuf> = std::fs::read_dir(dotnet_root.join("sdk"))
        .expect("read sdk dir")
        .filter_map(Result::ok)
        .map(|e| e.path().join("FSharp/FSharp.Core.xml"))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found.pop().expect("an SDK ships FSharp/FSharp.Core.xml")
}

/// Every doc file of every targeting pack the SDK carries
/// (`packs/*.Ref/<version>/ref/<tfm>/*.xml`: NETCore, ASP.NET Core, …).
fn ref_pack_xmls() -> Vec<PathBuf> {
    let packs =
        PathBuf::from(std::env::var_os("DOTNET_ROOT").expect("DOTNET_ROOT unset")).join("packs");
    let children = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default()
    };
    let mut xmls: Vec<PathBuf> = children(&packs)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".Ref"))
        })
        .flat_map(|pack| children(&pack))
        .flat_map(|version| children(&version.join("ref")))
        .flat_map(|tfm| children(&tfm))
        .filter(|p| p.extension().is_some_and(|e| e == "xml") && p.with_extension("dll").is_file())
        .collect();
    xmls.sort();
    let runtime_pack = ensure_system_runtime_dll();
    assert!(
        xmls.contains(&runtime_pack.with_extension("xml")),
        "the sweep covers the reference pack the tests resolve against"
    );
    xmls
}

/// Every `.xml` under `root` with a sibling `.dll` of the same stem, skipping
/// per-culture subdirectories (whose `.dll` lives one level up).
fn paired_xmls(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(_)
                    if path.extension().is_some_and(|e| e == "xml")
                        && path.with_extension("dll").is_file() =>
                {
                    out.push(path);
                }
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// Markdown structure the renderer never emits: finding any means doc text
/// was read as Markdown syntax.
fn foreign_structure(markdown: &str) -> Option<String> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_GFM;
    Parser::new_ext(markdown, options).find_map(|event| match event {
        Event::Start(
            tag @ (Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::HtmlBlock
            | Tag::Table(_)
            | Tag::Image { .. }
            | Tag::FootnoteDefinition(_)
            | Tag::Strikethrough),
        ) => Some(format!("{tag:?}")),
        Event::Html(h) | Event::InlineHtml(h) => Some(format!("HTML {h:?}")),
        Event::Rule => Some("thematic break".to_string()),
        Event::TaskListMarker(_) => Some("task list marker".to_string()),
        Event::FootnoteReference(r) => Some(format!("footnote {r:?}")),
        _ => None,
    })
}

/// Sweep one file. `strict`: a file that does not index fails the sweep.
fn sweep_file(path: &Path, strict: bool, census: &mut Census) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let started = Instant::now();
    let parsed = DocFile::decode(&bytes).and_then(DocFile::parse);
    let elapsed = started.elapsed();
    let file = match parsed {
        Ok(file) => file,
        Err(e) => {
            assert!(!strict, "{} does not index: {e:?}", path.display());
            census
                .unindexed
                .push((path.to_path_buf(), format!("{e:?}")));
            return;
        }
    };
    census.files += 1;
    if census.slowest.as_ref().is_none_or(|(d, _, _)| elapsed > *d) {
        census.slowest = Some((elapsed, path.to_path_buf(), bytes.len()));
    }

    // The whole-document view, for the equivalence check: every `<member>`
    // element of the one full parse, by where it starts.
    let text = DocFile::decode(&bytes).unwrap();
    let whole = roxmltree::Document::parse(&text).unwrap();
    let by_start: BTreeMap<usize, DocElement> = whole
        .descendants()
        .filter(|n| n.has_tag_name("member"))
        .map(|m| {
            (
                m.range().start,
                DocElement::from_roxmltree(m).expect("shallow"),
            )
        })
        .collect();

    for key in file.keys() {
        census.entries += 1;
        let range = match file.entry(key) {
            Some(DocEntry::Unique(range)) => range.clone(),
            Some(DocEntry::Ambiguous) => {
                census.ambiguous += 1;
                continue;
            }
            None => unreachable!("listed keys are indexed"),
        };
        let start = range.start;
        let member = file
            .member_element(range)
            .unwrap_or_else(|e| panic!("{} entry {key} does not re-parse: {e:?}", path.display()));
        assert_eq!(
            Some(&member),
            by_start.get(&start),
            "{} entry {key}: the range re-parse differs from the whole-document parse",
            path.display()
        );
        let (blocks, report) = render_member(&member);
        let markdown = to_markdown(&blocks);
        if let Some(found) = foreign_structure(&markdown) {
            panic!(
                "{} entry {key}: rendered Markdown carries {found}:\n{markdown}",
                path.display()
            );
        }
        for tag in report.unknown_tags {
            *census.unknown.entry(tag).or_default() += 1;
        }
        census.inheritdoc += usize::from(report.unresolved_inheritdoc);
        census.include += usize::from(report.unresolved_include);
    }
}

fn print(label: &str, census: &Census) {
    eprintln!("== {label}");
    eprintln!(
        "files indexed: {}, not indexed: {}, entries: {}, ambiguous keys: {}",
        census.files,
        census.unindexed.len(),
        census.entries,
        census.ambiguous
    );
    eprintln!(
        "entries with unresolved <inheritdoc>: {}, with unresolved <include>: {}",
        census.inheritdoc, census.include
    );
    if let Some((elapsed, path, bytes)) = &census.slowest {
        eprintln!(
            "slowest index build: {:?} for {} ({:.1} MB)",
            elapsed,
            path.display(),
            *bytes as f64 / 1e6
        );
    }
    let mut unknown: Vec<_> = census.unknown.iter().collect();
    unknown.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    eprintln!("fallback tags ({} distinct): {unknown:?}", unknown.len());
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for (_, why) in &census.unindexed {
        let head = why.split(['(', ' ']).next().unwrap_or(why).to_string();
        *reasons.entry(head).or_default() += 1;
    }
    eprintln!("not indexed, by reason: {reasons:?}");
    for (path, why) in census.unindexed.iter().take(10) {
        eprintln!("  {}: {why}", path.display());
    }
}

#[test]
#[ignore = "sweeps the SDK's shipped doc files; run with --ignored"]
fn shipped_doc_files_index_and_render_with_no_fallback() {
    let mut census = Census::default();
    for xml in ref_pack_xmls().iter().chain([&sdk_fsharp_core_xml()]) {
        sweep_file(xml, true, &mut census);
    }
    print("reference pack + FSharp.Core", &census);
    assert!(
        census.entries >= 50_000,
        "a ref pack plus FSharp.Core documents ~60,000 symbols; swept {}",
        census.entries
    );
    assert!(
        census.unknown.is_empty(),
        "tags in shipped doc files fall back to their text; give each an explicit \
         mapping (or an explicit ignore) in `xml_doc::render`: {:?}",
        census.unknown
    );

    if let Some(root) = std::env::var_os("BORZOI_XML_DOC_SWEEP_ROOT") {
        let mut wild = Census::default();
        for xml in paired_xmls(Path::new(&root)) {
            sweep_file(&xml, false, &mut wild);
        }
        print(&format!("{}", Path::new(&root).display()), &wild);
    }
}

/// Hover's doc keys are injective over every target a real env exposes: an
/// env over the whole NETCore reference pack plus FSharp.Core, every entity
/// and every member keyed with its assembly's census, and no committed key
/// (within one assembly — one doc file) names two targets. The census is
/// what makes this hold by construction; the sweep checks it does, and
/// reports how many targets real assemblies have it refuse.
#[test]
#[ignore = "builds an env over the whole reference pack; run with --ignored"]
fn doc_keys_are_injective_over_a_real_env() {
    use borzoi::xml_doc::key::{DocTarget, KeyCensus, KeyError, doc_key};
    use borzoi_assembly::{Ecma335Assembly, EcmaView};
    use borzoi_sema::AssemblyEnv;

    let pack = ensure_system_runtime_dll()
        .parent()
        .expect("ref pack dir")
        .to_path_buf();
    let mut dlls: Vec<PathBuf> = std::fs::read_dir(&pack)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "dll"))
        .collect();
    dlls.push(sdk_fsharp_core_xml().with_extension("dll"));
    dlls.sort();
    let assemblies: Vec<_> = dlls
        .iter()
        .filter_map(|dll| {
            let bytes = std::fs::read(dll).ok()?;
            let types = Ecma335Assembly::parse(&bytes)
                .ok()?
                .enumerate_type_defs()
                .ok()?;
            Some((dll.clone(), types))
        })
        .collect();
    assert!(
        assemblies.len() >= 100,
        "read {} assemblies",
        assemblies.len()
    );
    let env = AssemblyEnv::from_assemblies(assemblies);

    let mut censuses: BTreeMap<PathBuf, KeyCensus> = BTreeMap::new();
    let mut keyed: BTreeMap<(PathBuf, String), DocTarget> = BTreeMap::new();
    let (mut targets, mut shared) = (0usize, 0usize);
    for handle in env.all_handles() {
        let dll = env
            .assembly_path(handle)
            .expect("path-bearing env")
            .to_path_buf();
        let census = censuses
            .entry(dll.clone())
            .or_insert_with(|| KeyCensus::of_assembly(&env, &dll));
        let members = env.member_indices(handle).map(|idx| DocTarget::Member {
            parent: handle,
            idx,
        });
        for target in std::iter::once(DocTarget::Entity(handle)).chain(members) {
            targets += 1;
            match doc_key(&env, census, target) {
                Ok(key) => {
                    if let Some(previous) = keyed.insert((dll.clone(), key.clone()), target) {
                        panic!(
                            "{key} in {} keys both {previous:?} and {target:?}",
                            dll.display()
                        );
                    }
                }
                Err(KeyError::Shared) => shared += 1,
                Err(KeyError::Unplaced) => panic!("{target:?} is unplaced in a real env"),
            }
        }
    }
    eprintln!(
        "{} assemblies, {targets} targets, {} keys committed, {shared} refused as sharing a key",
        censuses.len(),
        keyed.len()
    );
    assert!(keyed.len() >= 50_000, "keyed only {}", keyed.len());
}

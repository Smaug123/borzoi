//! NuGet's settings, read as far as the global packages folder they choose.
//!
//! [`global_packages_folder`] answers what
//! `SettingsUtility.GetGlobalPackagesFolder(Settings.LoadDefaultSettings(root,
//! null, XPlatMachineWideSetting()))` returns, which is how the restore task
//! (`GetRestoreSettingsTask`) finds the packages folder of a project in `root`
//! that sets no `RestorePackagesPath` and no `RestoreConfigFile`, and how the
//! NuGet MSBuild SDK resolver finds the folder it resolves SDK packages from.
//! It answers exactly, or declines. `tests/all/settings_diff.rs` grades it
//! against the SDK's own NuGet over generated config hierarchies.
//!
//! # What NuGet does
//!
//! - `NUGET_PACKAGES`, set and non-empty, is the folder. It must be rooted
//!   (NuGet throws otherwise).
//! - Otherwise the first `add` whose `key` is `globalPackagesFolder`, ignoring
//!   case, in the merged `config` section. Its `value` has `%NAME%` expanded
//!   from the environment and, when relative, resolves against the folder of
//!   the file that declared it. An empty value is the default, not a
//!   fall-through to a less significant file.
//! - Otherwise `<home>/.nuget/packages`, where the home is `DOTNET_CLI_HOME`
//!   if set and otherwise `HOME`, if the process can read that folder
//!   ([`nuget_home`]).
//!
//! The files, most significant first: a config file in `root` and in each of
//! its ancestors, the user's `<home>/.nuget/NuGet/NuGet.Config`, each
//! `*.config` file in `<home>/.nuget/NuGet/config/` in name order, and the
//! machine-wide config files. They merge least significant first, section by
//! section, and the merge is not what one would guess, so it is modelled
//! literally ([`merge_config`]): the least significant file holding a `config`
//! section seeds it verbatim, keeping all of a key's duplicates in place; each
//! later file's `add` replaces the item with the same key, compared *exactly*,
//! and moves to the end; a `clear` empties it. The lookup then compares keys
//! ignoring case. So a `GlobalPackagesFolder` in a project's config does not
//! override a `globalPackagesFolder` in the user's: both stay, and the user's
//! comes first. Within one file, NuGet first drops each item equal, ignoring
//! case, to an earlier one in its element name followed by its attribute
//! values (so a second `clear` is dropped, and `key="ab" value="c"` is a
//! duplicate of `key="a" value="bc"`), then lets each `clear` truncate.
//!
//! # The domain
//!
//! Each layer NuGet reads has a positive domain, and anything outside it
//! declines ([`GlobalPackagesFolderDecline`]):
//!
//! - **Host.** Unix. On Windows NuGet finds the user folder through `APPDATA`.
//! - **Root.** An absolute, lexically normal path in UTF-8.
//! - **Environment.** The home variable that decides is an absolute path
//!   (and `HOME` a folder that can be read);
//!   `NUGET_PACKAGES`, if set non-empty, is absolute; on Linux
//!   `NUGET_COMMON_APPLICATION_DATA`, which moves the machine-wide folder, is
//!   absolute if set non-empty.
//! - **Which files.** NuGet decides which spelling of `nuget.config` it loads
//!   from what it believes of the file system's case sensitivity, a belief it
//!   probes once per process. A folder holding a config file is admitted only
//!   where that belief cannot matter: on macOS every spelling must name one
//!   file (a case-insensitive volume), and on Linux the spellings present must
//!   be distinct files. In the two folders NuGet lists, every candidate (a
//!   regular file whose name ends in `.config` in any case) must end in
//!   `.config` or `.Config`, which both platforms' patterns match, must not be
//!   named `NuGet.Config` in any case, and must sort the same ignoring case as
//!   exactly; a symbolic link among them declines. The machine-wide folder
//!   holds at most one, since NuGet loads them in directory order.
//! - **Bytes.** UTF-8, with or without a byte order mark.
//! - **Declaration.** Absent, or `version="1.0"` with an `encoding` absent or
//!   `utf-8` and a `standalone` absent, `yes` or `no` (as for nuspecs:
//!   `System.Xml` throws on the rest).
//! - **Document.** What `roxmltree` parses with its default options (no DTD),
//!   with no namespace declaration anywhere.
//! - **Tree.** A root named `configuration` in any case, without attributes,
//!   whose sections' names and attribute values are ASCII. NuGet drops a
//!   section whose name and attribute values match an earlier one's ignoring
//!   case (so a `Config` section hides a later `config`), and throws on two
//!   left with one name; the first declines here.
//!   Sections other than `config` are not read: they cannot move the folder,
//!   only make NuGet fail to load the file, and then restore fails whatever
//!   folder it was handed. So what this module claims is conditional on
//!   NuGet's loading the settings at all.
//! - **The `config` section.** No attributes; children `add` (exactly a `key`
//!   and a `value`, spelt so, in either order) and `clear` (no attributes),
//!   each name in any case, holding no element and no text but whitespace. A
//!   key is printable ASCII and not all spaces (NuGet throws on a key that
//!   trims to nothing, and compares the rest untrimmed); a value is printable
//!   ASCII.
//! - **The value.** Every `%` pairs into a `%NAME%` that the environment sets
//!   (to non-empty UTF-8), and the expansion is empty or made of ASCII letters, digits,
//!   `-_./~+@,=` and inner spaces. That keeps clear of how .NET's `Uri`
//!   classifies a string as relative, which decides whether NuGet resolves it
//!   against its file at all.
//!
//! The folder is compared modulo a trailing separator: NuGet's default ends in
//! one and a configured value need not, and both name the same folder.

use crate::dgspec::nuspec::{NuspecDecline, admitted_declaration};

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// The settings section holding the global packages folder.
pub const CONFIG_SECTION: &str = "config";

/// The key of the global packages folder in the [`CONFIG_SECTION`].
pub const GLOBAL_PACKAGES_FOLDER_KEY: &str = "globalPackagesFolder";

/// The environment variable that overrides every config file.
pub const NUGET_PACKAGES: &str = "NUGET_PACKAGES";

/// The spellings of a config file NuGet looks for in a folder of the walk on a
/// case-sensitive file system, in its order of preference.
pub const WALK_FILE_NAMES: [&str; 3] = ["nuget.config", "NuGet.config", "NuGet.Config"];

/// The name of the user config file, and of the walk's file on a
/// case-insensitive file system.
const DEFAULT_FILE_NAME: &str = "NuGet.Config";

/// One item of a settings file's `config` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigItem {
    /// `<clear/>`.
    Clear,
    /// `<add key="…" value="…"/>`.
    Add { key: String, value: String },
}

/// What this module reads of one settings file: its `config` section, as
/// NuGet holds it once the file is parsed (duplicates dropped, `clear`s
/// applied).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SettingsFile {
    /// `None` when the file has no `config` section.
    pub config: Option<Vec<ConfigItem>>,
}

/// Why a settings file is outside the domain, by layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsFileDecline {
    /// The file could not be read.
    Unreadable(String),
    /// The bytes are not UTF-8.
    Bytes(String),
    /// The XML declaration is outside the domain; carries the declaration.
    Declaration(String),
    /// `roxmltree` rejected the document.
    Xml(String),
    /// A node outside the vocabulary.
    Vocabulary(String),
}

impl fmt::Display for SettingsFileDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsFileDecline::Unreadable(error) => write!(f, "unreadable: {error}"),
            SettingsFileDecline::Bytes(error) => write!(f, "not UTF-8: {error}"),
            SettingsFileDecline::Declaration(declaration) => {
                write!(f, "XML declaration outside the domain: {declaration}")
            }
            SettingsFileDecline::Xml(error) => write!(f, "not well-formed XML: {error}"),
            SettingsFileDecline::Vocabulary(reason) => write!(f, "{reason}"),
        }
    }
}

/// Parse a settings file's bytes, or decline.
pub fn parse_settings_file(bytes: &[u8]) -> Result<SettingsFile, SettingsFileDecline> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes)
        .map_err(|error| SettingsFileDecline::Bytes(error.to_string()))?;
    admitted_declaration(text).map_err(|decline| match decline {
        NuspecDecline::Declaration(declaration) => SettingsFileDecline::Declaration(declaration),
        other => SettingsFileDecline::Declaration(format!("{other:?}")),
    })?;
    let vocabulary = |reason: &str| SettingsFileDecline::Vocabulary(reason.to_owned());
    // `System.Xml.Linq` lists a namespace declaration among an element's
    // attributes, where NuGet reads it; `roxmltree` does not.
    if text.contains("xmlns") {
        return Err(vocabulary("a namespace declaration"));
    }
    let document = roxmltree::Document::parse(text)
        .map_err(|error| SettingsFileDecline::Xml(error.to_string()))?;
    let root = document.root_element();
    if root.tag_name().namespace().is_some()
        || !root.tag_name().name().eq_ignore_ascii_case("configuration")
    {
        return Err(vocabulary("the root is not `configuration`"));
    }
    if root.attributes().len() != 0 {
        return Err(vocabulary("an attribute on the root"));
    }
    // NuGet first drops each section whose identity (its name, then its
    // attribute values) matches an earlier one's ignoring case, so a `Config`
    // hides a later `config`; then it keys the rest by name exactly, and
    // throws on a repeated one.
    let mut identities: Vec<String> = Vec::new();
    let mut names = BTreeSet::new();
    let mut config = None;
    for section in element_children(root)? {
        if section.tag_name().namespace().is_some() {
            return Err(vocabulary("a namespaced section"));
        }
        let name = section.tag_name().name();
        let identity: String = std::iter::once(name)
            .chain(section.attributes().map(|attribute| attribute.value()))
            .collect();
        if !identity.is_ascii() {
            return Err(vocabulary("a section outside ASCII"));
        }
        if identities
            .iter()
            .any(|seen| seen.eq_ignore_ascii_case(&identity))
        {
            continue;
        }
        identities.push(identity);
        if !names.insert(name) {
            return Err(vocabulary("a repeated section"));
        }
        if name == CONFIG_SECTION {
            config = Some(parse_config_section(section)?);
        }
    }
    Ok(SettingsFile { config })
}

/// The element children of `node`, which holds no text but whitespace.
fn element_children<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
) -> Result<Vec<roxmltree::Node<'a, 'input>>, SettingsFileDecline> {
    let mut elements = Vec::new();
    for child in node.children() {
        if child.is_element() {
            elements.push(child);
        } else if child.is_text()
            && !child
                .text()
                .unwrap_or_default()
                .trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
                .is_empty()
        {
            return Err(SettingsFileDecline::Vocabulary(format!(
                "text in `{}`",
                node.tag_name().name()
            )));
        }
    }
    Ok(elements)
}

/// Printable ASCII: what a key or value may hold.
fn printable_ascii(text: &str) -> bool {
    text.bytes().all(|byte| (0x20..0x7f).contains(&byte))
}

/// The `config` section's items as NuGet's `SettingFactory.ParseChildren`
/// leaves them.
fn parse_config_section(
    section: roxmltree::Node<'_, '_>,
) -> Result<Vec<ConfigItem>, SettingsFileDecline> {
    let vocabulary = |reason: &str| SettingsFileDecline::Vocabulary(reason.to_owned());
    if section.attributes().len() != 0 {
        return Err(vocabulary("an attribute on the `config` section"));
    }
    // Each item with its identity for NuGet's duplicate check: the element
    // name NuGet gives it, then its attribute values in document order.
    let mut distinct: Vec<(String, ConfigItem)> = Vec::new();
    for child in element_children(section)? {
        if child.tag_name().namespace().is_some() {
            return Err(vocabulary("a namespaced item"));
        }
        if !element_children(child)?.is_empty() {
            return Err(vocabulary("an element inside a `config` item"));
        }
        let name = child.tag_name().name();
        let (identity, item) = if name.eq_ignore_ascii_case("add") {
            let mut key = None;
            let mut value = None;
            let mut identity = String::from("add");
            for attribute in child.attributes() {
                if attribute.namespace().is_some() {
                    return Err(vocabulary("a namespaced attribute"));
                }
                let slot = match attribute.name() {
                    "key" => &mut key,
                    "value" => &mut value,
                    _ => return Err(vocabulary("an attribute of `add` other than key and value")),
                };
                *slot = Some(attribute.value().to_owned());
                identity.push_str(attribute.value());
            }
            let (Some(key), Some(value)) = (key, value) else {
                return Err(vocabulary("an `add` without a key and a value"));
            };
            if key.trim_matches(' ').is_empty() || !printable_ascii(&key) {
                return Err(vocabulary("a key outside the domain"));
            }
            if !printable_ascii(&value) {
                return Err(vocabulary("a value outside the domain"));
            }
            (identity, ConfigItem::Add { key, value })
        } else if name.eq_ignore_ascii_case("clear") {
            if child.attributes().len() != 0 {
                return Err(vocabulary("an attribute on `clear`"));
            }
            ("clear".to_owned(), ConfigItem::Clear)
        } else {
            return Err(vocabulary("an item of `config` other than add and clear"));
        };
        if !distinct
            .iter()
            .any(|(seen, _)| seen.eq_ignore_ascii_case(&identity))
        {
            distinct.push((identity, item));
        }
    }
    let mut items = Vec::new();
    for (_, item) in distinct {
        if item == ConfigItem::Clear {
            items.clear();
        }
        items.push(item);
    }
    Ok(items)
}

/// The merged `config` section of `files`, given most significant first, as
/// `Settings` computes it; `None` if no file has one.
pub fn merge_config<'a>(
    files: impl DoubleEndedIterator<Item = &'a SettingsFile>,
) -> Option<Vec<ConfigItem>> {
    let files: Vec<&SettingsFile> = files.collect();
    merge_tagged(&files).map(|section| section.into_iter().map(|(_, item)| item).collect())
}

/// [`merge_config`], each item tagged with the index in `files` of the file it
/// came from.
fn merge_tagged(files: &[&SettingsFile]) -> Option<Vec<(usize, ConfigItem)>> {
    let mut merged: Option<Vec<(usize, ConfigItem)>> = None;
    for (file, settings) in files.iter().enumerate().rev() {
        let Some(items) = &settings.config else {
            continue;
        };
        let tag = |item: &ConfigItem| (file, item.clone());
        let Some(section) = &mut merged else {
            merged = Some(items.iter().map(tag).collect());
            continue;
        };
        for item in items {
            match item {
                ConfigItem::Clear => section.clear(),
                ConfigItem::Add { key, .. } => {
                    if let Some(position) = section.iter().position(
                        |(_, existing)| matches!(existing, ConfigItem::Add { key: k, .. } if k == key),
                    ) {
                        section.remove(position);
                    }
                }
            }
            section.push(tag(item));
        }
    }
    merged
}

/// The `globalPackagesFolder` value of a merged `config` section, and the
/// index of the item holding it: the first `add` whose key matches ignoring
/// case.
pub fn configured_value(section: &[ConfigItem]) -> Option<(usize, &str)> {
    section
        .iter()
        .enumerate()
        .find_map(|(index, item)| match item {
            ConfigItem::Add { key, value }
                if key.eq_ignore_ascii_case(GLOBAL_PACKAGES_FOLDER_KEY) =>
            {
                Some((index, value.as_str()))
            }
            _ => None,
        })
}

/// `value` with each `%NAME%` replaced from `environment`, as .NET's
/// `Environment.ExpandEnvironmentVariables` scans it, or `None` when a name is
/// one this module does not look up (empty, or holding `=` or NUL) or names a
/// value that is empty or not UTF-8. An unset name is left in place, `%`s and
/// all. An empty value declines because the oracle cannot grade it: .NET's
/// `Environment.SetEnvironmentVariable` removes a variable set to empty.
pub fn expand_environment(
    value: &str,
    environment: &BTreeMap<OsString, OsString>,
) -> Option<String> {
    let bytes = value.as_bytes();
    let mut result = String::new();
    let mut last = 0;
    while last < value.len() {
        let Some(offset) = bytes[last + 1..].iter().position(|&byte| byte == b'%') else {
            break;
        };
        let position = last + 1 + offset;
        if bytes[last] == b'%' {
            let name = &value[last + 1..position];
            if name.is_empty() || name.contains(['=', '\0']) {
                return None;
            }
            if let Some(found) = environment.get(OsStr::new(name)) {
                if found.is_empty() {
                    return None;
                }
                result.push_str(found.to_str()?);
                last = position + 1;
                continue;
            }
        }
        result.push_str(&value[last..position]);
        last = position;
    }
    result.push_str(&value[last..]);
    Some(result)
}

/// Whether an expanded value is in the domain.
fn value_admitted(value: &str) -> bool {
    value.trim_matches(' ') == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./~+@,= ".contains(&byte))
}

/// `path` with `.` and `..` resolved and separators collapsed, as
/// `Path.GetFullPath` treats an absolute path on Unix (lexically).
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::Normal(name) => out.push(name),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// The global packages folder, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalPackagesFolder {
    /// The folder, absolute and lexically normal, without a trailing
    /// separator.
    pub folder: PathBuf,
    /// Where it came from.
    pub origin: FolderOrigin,
    /// The config files NuGet loads, most significant first, as
    /// `ISettings.GetConfigFilePaths` lists them. The user config file is
    /// always among them: NuGet writes a default one where there is none.
    pub config_files: Vec<PathBuf>,
}

/// Where a [`GlobalPackagesFolder`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderOrigin {
    /// `NUGET_PACKAGES`.
    Environment,
    /// A `globalPackagesFolder` in this config file.
    Config { file: PathBuf },
    /// NuGet's default under the home folder (no `globalPackagesFolder`, or an
    /// empty one).
    Default,
}

/// Why [`global_packages_folder`] declined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalPackagesFolderDecline {
    /// NuGet locates the user folder on Windows through `APPDATA`, which is
    /// not modelled.
    WindowsHost,
    /// The root is not an absolute, lexically normal UTF-8 path.
    Root { root: PathBuf },
    /// The home variable that decides is empty or not an absolute path, or
    /// `HOME` names a folder that cannot be read, or
    /// neither is set.
    HomeUnknown,
    /// An environment variable NuGet reads holds a value outside the domain.
    Environment { name: &'static str },
    /// Which config files NuGet loads from this folder depends on what it
    /// believes of the file system, or the folder could not be listed.
    Folder { folder: PathBuf },
    /// A config file NuGet loads is outside the domain, or unreadable.
    ConfigFile {
        file: PathBuf,
        decline: SettingsFileDecline,
    },
    /// The `globalPackagesFolder` value that decides is outside the domain.
    Value { file: PathBuf, value: String },
}

impl fmt::Display for GlobalPackagesFolderDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GlobalPackagesFolderDecline::WindowsHost => {
                write!(f, "the NuGet settings of a Windows host are not modelled")
            }
            GlobalPackagesFolderDecline::Root { root } => {
                write!(f, "{} is not an absolute, normal path", root.display())
            }
            GlobalPackagesFolderDecline::HomeUnknown => {
                write!(f, "the environment does not locate the home folder")
            }
            GlobalPackagesFolderDecline::Environment { name } => {
                write!(f, "{name} holds a value outside the modelled domain")
            }
            GlobalPackagesFolderDecline::Folder { folder } => write!(
                f,
                "which config files NuGet loads from {} cannot be predicted",
                folder.display()
            ),
            GlobalPackagesFolderDecline::ConfigFile { file, decline } => {
                write!(f, "the config file {}: {decline}", file.display())
            }
            GlobalPackagesFolderDecline::Value { file, value } => write!(
                f,
                "the globalPackagesFolder value {value:?} in {} is outside the modelled domain",
                file.display()
            ),
        }
    }
}

/// The home folder NuGet uses under `environment`, or `None` where it is not
/// an absolute path this function can name.
///
/// NuGet takes `DOTNET_CLI_HOME` verbatim if it is set (so a set but empty one
/// is the working folder, not unset), and otherwise asks .NET for the user
/// profile folder, which is `HOME` (or, when `HOME` is unset or empty, the
/// password database, which is not modelled) *if the process can read it*:
/// .NET answers the empty string for a folder it cannot, and NuGet then works
/// under the working folder.
pub fn nuget_home(environment: &BTreeMap<OsString, OsString>) -> Option<PathBuf> {
    let absolute = |value: &OsString| {
        value
            .to_str()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(cli_home) = environment.get(OsStr::new("DOTNET_CLI_HOME")) {
        return absolute(cli_home);
    }
    let home = absolute(environment.get(OsStr::new("HOME"))?)?;
    std::fs::read_dir(&home).ok()?;
    Some(home)
}

/// A config file NuGet loads, and the folder its relative values resolve
/// against.
struct ConfigFileRef {
    path: PathBuf,
    folder: PathBuf,
    /// `false` for a user config NuGet will write: its default holds package
    /// sources alone.
    exists: bool,
}

/// The global packages folder restore uses for a project in `root` that sets
/// no `RestorePackagesPath` and no `RestoreConfigFile`, under `environment`
/// (the environment restore runs in, exactly), or why it cannot be said. See
/// the module documentation for the domain.
pub fn global_packages_folder(
    root: &Path,
    environment: &BTreeMap<OsString, OsString>,
) -> Result<GlobalPackagesFolder, GlobalPackagesFolderDecline> {
    if cfg!(windows) {
        return Err(GlobalPackagesFolderDecline::WindowsHost);
    }
    if root.to_str().is_none() || normalise(root) != root {
        return Err(GlobalPackagesFolderDecline::Root {
            root: root.to_owned(),
        });
    }
    let home = nuget_home(environment).ok_or(GlobalPackagesFolderDecline::HomeUnknown)?;
    let files = config_files(root, &home, environment)?;
    let mut parsed = Vec::with_capacity(files.len());
    for file in &files {
        parsed.push(if file.exists {
            read_config_file(&file.path)?
        } else {
            SettingsFile::default()
        });
    }
    let config_files: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();

    if let Some(value) = environment.get(OsStr::new(NUGET_PACKAGES)) {
        let value = value
            .to_str()
            .ok_or(GlobalPackagesFolderDecline::Environment {
                name: NUGET_PACKAGES,
            })?;
        if !value.is_empty() {
            if !value.starts_with('/') || value.contains('\0') {
                return Err(GlobalPackagesFolderDecline::Environment {
                    name: NUGET_PACKAGES,
                });
            }
            return Ok(GlobalPackagesFolder {
                folder: normalise(Path::new(value)),
                origin: FolderOrigin::Environment,
                config_files,
            });
        }
    }

    let default = || GlobalPackagesFolder {
        folder: normalise(&home.join(".nuget").join("packages")),
        origin: FolderOrigin::Default,
        config_files: config_files.clone(),
    };
    let parsed: Vec<&SettingsFile> = parsed.iter().collect();
    let Some(section) = merge_tagged(&parsed) else {
        return Ok(default());
    };
    let items: Vec<ConfigItem> = section.iter().map(|(_, item)| item.clone()).collect();
    let Some((index, value)) = configured_value(&items) else {
        return Ok(default());
    };
    let file = &files[section[index].0];
    let decline = || GlobalPackagesFolderDecline::Value {
        file: file.path.clone(),
        value: value.to_owned(),
    };
    let expanded = expand_environment(value, environment).ok_or_else(decline)?;
    if expanded.is_empty() {
        return Ok(default());
    }
    if !value_admitted(&expanded) {
        return Err(decline());
    }
    let joined = if expanded.starts_with('/') {
        PathBuf::from(&expanded)
    } else {
        file.folder.join(&expanded)
    };
    Ok(GlobalPackagesFolder {
        folder: normalise(&joined),
        origin: FolderOrigin::Config {
            file: file.path.clone(),
        },
        config_files,
    })
}

fn read_config_file(path: &Path) -> Result<SettingsFile, GlobalPackagesFolderDecline> {
    let bytes = std::fs::read(path).map_err(|error| GlobalPackagesFolderDecline::ConfigFile {
        file: path.to_owned(),
        decline: SettingsFileDecline::Unreadable(error.to_string()),
    })?;
    parse_settings_file(&bytes).map_err(|decline| GlobalPackagesFolderDecline::ConfigFile {
        file: path.to_owned(),
        decline,
    })
}

/// The config files NuGet loads for `root`, most significant first.
fn config_files(
    root: &Path,
    home: &Path,
    environment: &BTreeMap<OsString, OsString>,
) -> Result<Vec<ConfigFileRef>, GlobalPackagesFolderDecline> {
    let mut files = Vec::new();
    for folder in root.ancestors() {
        if let Some(path) = walk_config_file(folder)? {
            files.push(ConfigFileRef {
                path,
                folder: folder.to_owned(),
                exists: true,
            });
        }
    }

    let user_folder = home.join(".nuget").join("NuGet");
    let user_file = user_folder.join(DEFAULT_FILE_NAME);
    // NuGet writes a default config (package sources only) where there is
    // none, and fails where it cannot.
    let exists = match std::fs::metadata(&user_file) {
        Ok(metadata) if metadata.is_file() => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        _ => {
            return Err(GlobalPackagesFolderDecline::Folder {
                folder: user_folder,
            });
        }
    };
    files.push(ConfigFileRef {
        path: user_file,
        folder: user_folder.clone(),
        exists,
    });

    let additional = user_folder.join("config");
    for name in listed_config_files(&additional)? {
        files.push(ConfigFileRef {
            path: additional.join(name),
            folder: additional.clone(),
            exists: true,
        });
    }

    let machine_wide = machine_wide_folder(environment)?;
    let names = listed_config_files(&machine_wide)?;
    if names.len() > 1 {
        return Err(GlobalPackagesFolderDecline::Folder {
            folder: machine_wide,
        });
    }
    for name in names {
        files.push(ConfigFileRef {
            path: machine_wide.join(name),
            folder: machine_wide.clone(),
            exists: true,
        });
    }
    Ok(files)
}

/// The config file NuGet loads from `folder` in its walk up from the root.
fn walk_config_file(folder: &Path) -> Result<Option<PathBuf>, GlobalPackagesFolderDecline> {
    let present: Vec<(usize, std::fs::Metadata)> = WALK_FILE_NAMES
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            std::fs::metadata(folder.join(name))
                .ok()
                .filter(std::fs::Metadata::is_file)
                .map(|metadata| (index, metadata))
        })
        .collect();
    let Some((first, _)) = present.first() else {
        return Ok(None);
    };
    let unpredictable = || GlobalPackagesFolderDecline::Folder {
        folder: folder.to_owned(),
    };
    if cfg!(target_os = "macos") {
        // NuGet believes a macOS volume case-insensitive and looks for
        // `NuGet.Config` alone; that agrees with every belief only where all
        // three spellings name one file.
        if present.len() != WALK_FILE_NAMES.len() || !same_file(&present) {
            return Err(unpredictable());
        }
        Ok(Some(folder.join(DEFAULT_FILE_NAME)))
    } else {
        // NuGet believes a Linux volume case-sensitive and takes the first
        // spelling present; on a case-insensitive one every spelling is
        // present and names one file, and the belief may differ.
        if present.len() > 1 && same_file(&present) {
            return Err(unpredictable());
        }
        Ok(Some(folder.join(WALK_FILE_NAMES[*first])))
    }
}

/// Whether every entry of `present` is one file.
fn same_file(present: &[(usize, std::fs::Metadata)]) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        present
            .windows(2)
            .all(|pair| pair[0].1.dev() == pair[1].1.dev() && pair[0].1.ino() == pair[1].1.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = present;
        false
    }
}

/// The names of the config files NuGet loads from a folder it lists (the
/// user's `config` folder, the machine-wide folder), in its order, or a
/// decline where that order or set depends on the platform's case rules.
fn listed_config_files(folder: &Path) -> Result<Vec<String>, GlobalPackagesFolderDecline> {
    let unpredictable = || GlobalPackagesFolderDecline::Folder {
        folder: folder.to_owned(),
    };
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => {
            return if folder.is_dir() || folder.exists() {
                Err(unpredictable())
            } else {
                Ok(Vec::new())
            };
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| unpredictable())?;
        let name = entry.file_name();
        let candidate = name
            .as_encoded_bytes()
            .to_ascii_lowercase()
            .ends_with(b".config");
        if !candidate {
            continue;
        }
        let file_type = entry.file_type().map_err(|_| unpredictable())?;
        if file_type.is_dir() {
            continue;
        }
        let Some(name) = name.to_str().filter(|name| name.is_ascii()) else {
            return Err(unpredictable());
        };
        if file_type.is_symlink()
            || !file_type.is_file()
            || !(name.ends_with(".config") || name.ends_with(".Config"))
            || name.eq_ignore_ascii_case(DEFAULT_FILE_NAME)
        {
            return Err(unpredictable());
        }
        names.push(name.to_owned());
    }
    names.sort();
    let ignoring_case = |name: &String| name.to_ascii_uppercase();
    if names
        .windows(2)
        .any(|pair| ignoring_case(&pair[0]) >= ignoring_case(&pair[1]))
    {
        return Err(unpredictable());
    }
    Ok(names)
}

/// The machine-wide config folder: `NuGet/Config` under the common application
/// data folder, which on Linux `NUGET_COMMON_APPLICATION_DATA` moves.
fn machine_wide_folder(
    environment: &BTreeMap<OsString, OsString>,
) -> Result<PathBuf, GlobalPackagesFolderDecline> {
    let base = if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support")
    } else {
        const OVERRIDE: &str = "NUGET_COMMON_APPLICATION_DATA";
        match environment.get(OsStr::new(OVERRIDE)) {
            Some(value) if !value.is_empty() => {
                let path = value
                    .to_str()
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .ok_or(GlobalPackagesFolderDecline::Environment { name: OVERRIDE })?;
                normalise(&path)
            }
            _ => PathBuf::from("/etc/opt"),
        }
    };
    Ok(base.join("NuGet").join("Config"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> SettingsFile {
        parse_settings_file(text.as_bytes()).expect("admitted")
    }

    fn add(key: &str, value: &str) -> ConfigItem {
        ConfigItem::Add {
            key: key.to_owned(),
            value: value.to_owned(),
        }
    }

    #[test]
    fn a_second_clear_in_one_file_is_a_duplicate_and_does_nothing() {
        let parsed = file(
            "<configuration><config>\
             <add key=\"a\" value=\"1\"/><clear/><add key=\"b\" value=\"2\"/>\
             <clear/><add key=\"c\" value=\"3\"/>\
             </config></configuration>",
        );
        assert_eq!(
            parsed.config,
            Some(vec![ConfigItem::Clear, add("b", "2"), add("c", "3")])
        );
    }

    #[test]
    fn items_are_duplicates_by_their_concatenated_attribute_values() {
        let parsed = file(
            "<configuration><config>\
             <add key=\"ab\" value=\"c\"/><ADD key=\"A\" value=\"BC\"/>\
             </config></configuration>",
        );
        assert_eq!(parsed.config, Some(vec![add("ab", "c")]));
    }

    #[test]
    fn the_least_significant_file_keeps_its_first_duplicate_and_later_files_their_last() {
        let only = file(
            "<configuration><config>\
             <add key=\"globalPackagesFolder\" value=\"first\"/>\
             <add key=\"globalPackagesFolder\" value=\"second\"/>\
             </config></configuration>",
        );
        let section = merge_config([&only].into_iter()).unwrap();
        assert_eq!(configured_value(&section), Some((0, "first")));

        let empty = file("<configuration><config/></configuration>");
        let section = merge_config([&only, &empty].into_iter()).unwrap();
        assert_eq!(configured_value(&section), Some((0, "second")));
    }

    #[test]
    fn a_section_matching_an_earlier_one_ignoring_case_is_dropped() {
        let hidden = file(
            "<configuration><Config/><config><add key=\"globalPackagesFolder\" value=\"p\"/></config></configuration>",
        );
        assert_eq!(hidden.config, None);
        let repeated = parse_settings_file(
            b"<configuration><config a=\"1\"/><config><add key=\"k\" value=\"v\"/></config></configuration>",
        );
        assert!(matches!(repeated, Err(SettingsFileDecline::Vocabulary(_))));
    }

    #[test]
    fn a_key_in_another_case_does_not_replace_a_less_significant_one() {
        let user = file(
            "<configuration><config><add key=\"globalPackagesFolder\" value=\"user\"/></config></configuration>",
        );
        let project = file(
            "<configuration><config><add key=\"GlobalPackagesFolder\" value=\"project\"/></config></configuration>",
        );
        let section = merge_config([&project, &user].into_iter()).unwrap();
        assert_eq!(configured_value(&section), Some((0, "user")));
        assert_eq!(merge_tagged(&[&project, &user]).unwrap()[0].0, 1);
    }

    #[test]
    fn expansion_follows_the_dotnet_scan() {
        let environment: BTreeMap<OsString, OsString> = [("A", "x"), ("B", "y")]
            .into_iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect();
        let expand = |value| expand_environment(value, &environment);
        assert_eq!(expand("%A%%B%").as_deref(), Some("xy"));
        assert_eq!(expand("%U%A%").as_deref(), Some("%Ux"));
        assert_eq!(expand("a%A").as_deref(), Some("a%A"));
        assert_eq!(expand("%A%/%U%").as_deref(), Some("x/%U%"));
        assert_eq!(expand("%%"), None);
    }
}

//! `borzoi_nuget::settings::global_packages_folder` against NuGet's own
//! `SettingsUtility.GetGlobalPackagesFolder(Settings.LoadDefaultSettings(…))`,
//! over generated config hierarchies, held to *certain-implies-exact*: where
//! the model commits, NuGet loads the same config files, in the same order,
//! and names the same folder; where NuGet throws, the model declined.
//!
//! The generator varies what the model's domain is stated over, layer by
//! layer: which folders of the walk hold a config file and under which
//! spelling, the user config and the user's additional configs, (on Linux)
//! the machine-wide folder; the bytes (a byte order mark), the declaration,
//! the root's name, sections and their repetition, the `config` section's
//! items, element-name and key case, duplicates, `clear`s, attribute order,
//! stray attributes and text, truncation; and the value (relative, absolute,
//! `%NAME%` set, empty and unset, characters outside the domain), with
//! `NUGET_PACKAGES` unset, empty, absolute and relative.
//!
//! Every case runs both sides under one controlled environment: the oracle
//! child inherits this process's environment with the variables NuGet reads
//! for this purpose fixed (the home, the machine-wide override) or removed,
//! and each request sets the per-case ones (`NUGET_PACKAGES` and the
//! variables values name) for itself. The model is handed exactly that map.
//! NuGet reads the home and the machine-wide folder once per process, which
//! is why those are per-oracle.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use borzoi_nuget::settings::{
    FolderOrigin, GlobalPackagesFolder, GlobalPackagesFolderDecline, global_packages_folder,
};
use serde_json::json;

use crate::common::{EnvironmentChange, Oracle, SplitMix64};

/// The variables a case sets for its own request; every request names all of
/// them, so none leaks from one case into the next.
const CASE_VARIABLES: &[&str] = &[
    "NUGET_PACKAGES",
    "BORZOI_GPF_A",
    "BORZOI_GPF_B",
    "BORZOI_GPF_EMPTY",
];

/// The variables NuGet reads to find the folders, fixed per oracle.
const FIXED_VARIABLES: &[&str] = &[
    "DOTNET_CLI_HOME",
    "HOME",
    "NUGET_PACKAGES",
    "NUGET_COMMON_APPLICATION_DATA",
];

/// One oracle and the scratch tree its cases are written into.
struct Harness {
    oracle: Oracle,
    /// The environment the oracle child holds before a request's changes.
    environment: BTreeMap<OsString, OsString>,
    base: PathBuf,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// An oracle whose home is `<base>/home`, or `<base>/cli-home` through
    /// `DOTNET_CLI_HOME` when `cli_home` holds, and whose machine-wide folder
    /// on Linux is under `<base>/common`.
    fn spawn(cli_home: bool) -> Harness {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = std::fs::canonicalize(dir.path()).expect("canonical temp dir");
        let text = |path: PathBuf| path.to_str().expect("UTF-8 temp dir").to_owned();
        let home = text(base.join("home"));
        let cli = text(base.join("cli-home"));
        let common = text(base.join("common"));
        let mut changes: Vec<EnvironmentChange<'_>> = FIXED_VARIABLES
            .iter()
            .chain(CASE_VARIABLES)
            .map(|name| EnvironmentChange::Unset(name))
            .collect();
        changes.push(EnvironmentChange::Set("HOME", &home));
        changes.push(EnvironmentChange::Set(
            "NUGET_COMMON_APPLICATION_DATA",
            &common,
        ));
        if cli_home {
            changes.push(EnvironmentChange::Set("DOTNET_CLI_HOME", &cli));
        }
        let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
        for change in &changes {
            match change {
                EnvironmentChange::Set(name, value) => {
                    environment.insert(OsString::from(name), OsString::from(value));
                }
                EnvironmentChange::Unset(name) => {
                    environment.remove(OsStr::new(name));
                }
            }
        }
        Harness {
            oracle: Oracle::spawn_with_changes(&changes),
            environment,
            base,
            _dir: dir,
        }
    }
}

/// A generated case: the files to write (relative to the harness base), the
/// per-case variables, and the project folder.
#[derive(Debug, Clone)]
struct Case {
    files: Vec<(String, Vec<u8>)>,
    /// Whether a file holds something NuGet throws on in a section the model
    /// does not read: outside the model's claim, which is conditional on
    /// NuGet's loading the settings.
    unread_fault: bool,
    /// Folders made (empty) before the files are written: whether the home
    /// exists decides whether .NET hands NuGet `HOME` at all.
    folders: Vec<&'static str>,
    variables: Vec<(&'static str, Option<String>)>,
    root: String,
}

/// What grading one case found.
#[derive(Debug)]
enum Verdict {
    Committed(GlobalPackagesFolder),
    /// The model committed and NuGet threw on a section the model does not
    /// read, where the model's claim is silent.
    CommittedWhereNuGetThrewOnAnUnreadSection,
    /// The model declined; NuGet's answer (`None`), or the type and start of
    /// the message of what it threw.
    Declined {
        decline: GlobalPackagesFolderDecline,
        nuget_threw: Option<String>,
    },
}

/// Write `case` into a clean tree, ask both sides, and assert the contract.
fn grade(harness: &mut Harness, case: &Case) -> Verdict {
    for folder in ["tree", "home", "cli-home", "common"] {
        let path = harness.base.join(folder);
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("clean the case tree");
        }
    }
    std::fs::create_dir_all(harness.base.join(&case.root)).expect("create the root");
    for folder in &case.folders {
        std::fs::create_dir_all(harness.base.join(folder)).expect("create a folder");
    }
    for (path, bytes) in &case.files {
        let path = harness.base.join(path);
        std::fs::create_dir_all(path.parent().expect("a file has a folder")).expect("mkdir");
        std::fs::write(&path, bytes).expect("write a config file");
    }
    let root = harness.base.join(&case.root);
    let mut environment = harness.environment.clone();
    for (name, value) in &case.variables {
        match value {
            Some(value) => environment.insert(OsString::from(name), OsString::from(value)),
            None => environment.remove(OsStr::new(name)),
        };
    }
    // The model first: NuGet writes a default user config where there is none.
    let ours = global_packages_folder(&root, &environment);
    let request = json!({
        "op": "globalPackagesFolder",
        "root": root.to_str().expect("UTF-8"),
        "environment": case.variables.iter().map(|(name, value)| json!([name, value])).collect::<Vec<_>>(),
    });
    let theirs = harness.oracle.request(&request);
    let describe = || {
        let files: Vec<String> = case
            .files
            .iter()
            .map(|(path, bytes)| format!("--- {path}\n{}", String::from_utf8_lossy(bytes)))
            .collect();
        format!(
            "root {}\nvariables {:?}\n{}\nNuGet: {theirs}",
            case.root,
            case.variables,
            files.join("\n")
        )
    };
    let answered = theirs["ok"].as_bool().expect("ok is a bool");
    match ours {
        Ok(committed) => {
            if !answered {
                assert!(
                    case.unread_fault,
                    "the model committed to {committed:?} where NuGet threw\n{}",
                    describe()
                );
                return Verdict::CommittedWhereNuGetThrewOnAnUnreadSection;
            }
            let folder = normalise(theirs["folder"].as_str().expect("folder"));
            assert_eq!(
                committed.folder,
                folder,
                "the model committed to another folder\n{}",
                describe()
            );
            let files: Vec<PathBuf> = theirs["configFilePaths"]
                .as_array()
                .expect("configFilePaths")
                .iter()
                .map(|path| PathBuf::from(path.as_str().expect("a path")))
                .collect();
            assert_eq!(
                committed.config_files,
                files,
                "the model loads other config files\n{}",
                describe()
            );
            Verdict::Committed(committed)
        }
        Err(decline) => Verdict::Declined {
            decline,
            nuget_threw: (!answered).then(|| {
                let message = theirs["message"].as_str().unwrap_or("");
                let message: String = message.chars().take(60).collect();
                format!(
                    "{}: {message}",
                    theirs["exceptionType"].as_str().unwrap_or("?")
                )
            }),
        },
    }
}

/// An absolute path made lexically normal, without a trailing separator: the
/// currency both sides are compared in.
fn normalise(path: &str) -> PathBuf {
    let mut out = PathBuf::from("/");
    for component in Path::new(path).components() {
        match component {
            std::path::Component::Normal(name) => out.push(name),
            std::path::Component::ParentDir => {
                out.pop();
            }
            _ => {}
        }
    }
    out
}

// ----------------------------------------------------------------------------
// The generator
// ----------------------------------------------------------------------------

/// The spellings a config file is written under in a folder of the walk: the
/// three NuGet looks for on a case-sensitive volume, and two it never does.
const WALK_SPELLINGS: &[&str] = &[
    "nuget.config",
    "NuGet.config",
    "NuGet.Config",
    "NUGET.CONFIG",
    "nuget.Config",
];

/// Names in the user's additional config folder (and the machine-wide one).
const LISTED_NAMES: &[&str] = &[
    "a.config",
    "b.Config",
    "C.config",
    "_u.config",
    "d.CONFIG",
    "nuget.config",
    "e.config.bak",
    "f.txt",
];

fn gen_value(rng: &mut SplitMix64, base: &Path) -> String {
    let absolute = base.join("abs").join(format!("g{}", rng.below(3)));
    let absolute = absolute.to_str().expect("UTF-8");
    let values: [String; 22] = [
        "pkgs".into(),
        "./pkgs".into(),
        "../pkgs".into(),
        "a/../b".into(),
        "pkgs/".into(),
        "deep//er/./pkgs".into(),
        String::new(),
        "%BORZOI_GPF_A%".into(),
        "%BORZOI_GPF_A%/x".into(),
        "%BORZOI_GPF_B%/y".into(),
        "%BORZOI_GPF_UNSET%".into(),
        "%BORZOI_GPF_EMPTY%".into(),
        absolute.to_owned(),
        format!("{absolute}/"),
        format!("{absolute}/../h"),
        " pkgs".into(),
        "pk gs".into(),
        "c:pkgs".into(),
        "pk\\gs".into(),
        "~/pkgs".into(),
        "pkgs#1".into(),
        "%%".into(),
    ];
    // Bias towards values inside the domain.
    if rng.below(3) == 0 {
        rng.pick(&values).clone()
    } else {
        values[rng.below(15)].clone()
    }
}

/// One item of a `config`-like section, and whether NuGet throws on it.
fn gen_item(rng: &mut SplitMix64, base: &Path) -> (String, bool) {
    let key = *rng.pick(&[
        "globalPackagesFolder",
        "globalPackagesFolder",
        "globalPackagesFolder",
        "GlobalPackagesFolder",
        "GLOBALPACKAGESFOLDER",
        " globalPackagesFolder",
    ]);
    let value = gen_value(rng, base);
    let item = match rng.below(80) {
        20..=21 => format!("<add value=\"{value}\" key=\"{key}\" />"),
        22..=23 => format!("<ADD key=\"{key}\" value=\"{value}\" />"),
        24..=25 => "<add key=\"repositoryPath\" value=\"repo\" />".to_owned(),
        26..=27 => "<clear />".to_owned(),
        28 => "<Clear/>".to_owned(),
        29..=30 => format!("<add key=\"{key}\" value=\"{value}\"></add>"),
        31 => "<!-- a comment -->".to_owned(),
        // Inside the vocabulary NuGet reads, outside the one modelled.
        32..=35 => match rng.below(3) {
            0 => format!("<add key=\"{key}\" value=\"{value}\" extra=\"1\" />"),
            1 => "<remove key=\"x\" />".to_owned(),
            _ => "loose text".to_owned(),
        },
        // NuGet throws: its attribute names are exact, and an `add` needs a
        // key and a value.
        36 => return (format!("<add Key=\"{key}\" value=\"{value}\" />"), true),
        37 => return (format!("<add key=\"{key}\" />"), true),
        _ => format!("<add key=\"{key}\" value=\"{value}\" />"),
    };
    (item, false)
}

/// A config file's bytes, and whether it holds something NuGet throws on in a
/// section the model does not read (a `Config`, or a `config` hidden by an
/// earlier section of that name in another case).
fn gen_config(rng: &mut SplitMix64, base: &Path) -> (Vec<u8>, bool) {
    // At most one fault per file, so most cases are clean enough to commit.
    let fault = if rng.below(16) == 0 {
        1 + rng.below(6)
    } else {
        0
    };
    let declaration = match fault {
        1 => *rng.pick(&[
            "<?xml version=\"1.0\" encoding=\"utf-16\"?>\n",
            "<?xml version=\"1.1\"?>\n",
        ]),
        _ => *rng.pick(&[
            "",
            "",
            "<?xml version=\"1.0\"?>\n",
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
        ]),
    };
    let root = if fault == 2 {
        "settings"
    } else {
        *rng.pick(&[
            "configuration",
            "configuration",
            "configuration",
            "Configuration",
            "CONFIGURATION",
        ])
    };
    let mut body = String::new();
    let mut unread_fault = false;
    let mut config_like = false;
    for _ in 0..rng.below(4) {
        match rng.below(7) {
            0 => body.push_str(
                "<packageSources><clear /><add key=\"local\" value=\"/nonexistent/feed\" /></packageSources>",
            ),
            1 => {
                let (item, throws) = gen_item(rng, base);
                unread_fault |= throws;
                config_like = true;
                body.push_str(&format!("<Config>{item}</Config>"));
            }
            _ => {
                body.push_str("<config>");
                for _ in 0..rng.below(5) {
                    let (item, throws) = gen_item(rng, base);
                    unread_fault |= throws && config_like;
                    body.push_str(&item);
                }
                body.push_str("</config>");
                config_like = true;
            }
        }
    }
    match fault {
        3 => body.push_str("<config foo=\"1\" /><config />"),
        4 => body.push_str("<x:config xmlns:x=\"urn:x\" />"),
        _ => {}
    }
    let mut text = format!("{declaration}<{root}>{body}</{root}>\n");
    if fault == 5 {
        let cut = rng.below(text.len());
        text.truncate(cut);
    }
    let mut bytes = Vec::new();
    if rng.below(6) == 0 {
        bytes.extend_from_slice(b"\xEF\xBB\xBF");
    }
    bytes.extend_from_slice(text.as_bytes());
    if fault == 6 {
        // Not UTF-8.
        bytes.push(0xff);
    }
    (bytes, unread_fault)
}

fn gen_case(rng: &mut SplitMix64, base: &Path, cli_home: bool) -> Case {
    let depth = rng.below(4);
    let mut folders = vec!["tree".to_owned()];
    for level in 0..=depth {
        let parent = folders.last().expect("a folder").clone();
        folders.push(format!("{parent}/l{level}"));
    }
    let root = folders.last().expect("a folder").clone();
    let mut files = Vec::new();
    let mut unread_fault = false;
    let mut config = |rng: &mut SplitMix64| {
        let (bytes, fault) = gen_config(rng, base);
        unread_fault |= fault;
        bytes
    };
    for folder in &folders {
        if rng.below(5) < 2 {
            let spelling = rng.pick(WALK_SPELLINGS);
            files.push((format!("{folder}/{spelling}"), config(rng)));
            if rng.below(10) == 0 {
                let other = rng.pick(WALK_SPELLINGS);
                files.push((format!("{folder}/{other}"), config(rng)));
            }
        }
    }
    let home = if cli_home { "cli-home" } else { "home" };
    if rng.below(2) == 0 {
        let spelling = *rng.pick(&[
            "NuGet.Config",
            "NuGet.Config",
            "NuGet.Config",
            "nuget.config",
        ]);
        files.push((format!("{home}/.nuget/NuGet/{spelling}"), config(rng)));
    }
    let mut listed = |folder: &str, rng: &mut SplitMix64| {
        let mut names = BTreeSet::new();
        for _ in 0..=rng.below(3) {
            names.insert(*rng.pick(LISTED_NAMES));
        }
        for name in names {
            files.push((format!("{folder}/{name}"), config(rng)));
        }
    };
    if rng.below(4) == 0 {
        listed(&format!("{home}/.nuget/NuGet/config"), rng);
    }
    if rng.below(5) == 0 {
        listed("common/NuGet/Config", rng);
    }
    let absolute = base.join("env-abs");
    let absolute = absolute.to_str().expect("UTF-8").to_owned();
    let nuget_packages = match rng.below(8) {
        0 => Some(absolute.clone() + "/np"),
        1 => Some(String::new()),
        2 => Some("relative/np".to_owned()),
        _ => None,
    };
    let a = match rng.below(4) {
        0 => None,
        1 => Some("rel/a".to_owned()),
        2 => Some(String::new()),
        _ => Some(absolute + "/a"),
    };
    let mut folders = Vec::new();
    if rng.below(8) != 0 {
        folders.push("home");
    }
    if rng.below(2) == 0 {
        folders.push("cli-home");
    }
    Case {
        files,
        unread_fault,
        folders,
        variables: vec![
            ("NUGET_PACKAGES", nuget_packages),
            ("BORZOI_GPF_A", a),
            ("BORZOI_GPF_B", Some("sub".to_owned())),
            ("BORZOI_GPF_EMPTY", Some(String::new())),
        ],
        root,
    }
}

/// What a sweep saw, for the non-vacuity floors and the census it prints.
#[derive(Default, Debug)]
struct Census {
    cases: usize,
    environment: usize,
    config: usize,
    config_relative: usize,
    config_expanded: usize,
    config_beyond_first: usize,
    default: usize,
    many_files: usize,
    declined_nuget_threw: usize,
    unread_faults: usize,
    nuget_threw: BTreeMap<String, usize>,
    declined_nuget_answered: BTreeMap<String, usize>,
}

fn sweep(seed: u64, cases: usize, cli_home: bool) -> Census {
    let mut harness = Harness::spawn(cli_home);
    let mut rng = SplitMix64(seed);
    let mut census = Census::default();
    for _ in 0..cases {
        let case = gen_case(&mut rng, &harness.base.clone(), cli_home);
        census.cases += 1;
        match grade(&mut harness, &case) {
            Verdict::Committed(committed) => {
                if committed.config_files.len() >= 3 {
                    census.many_files += 1;
                }
                match &committed.origin {
                    FolderOrigin::Environment => census.environment += 1,
                    FolderOrigin::Default => census.default += 1,
                    FolderOrigin::Config { file } => {
                        census.config += 1;
                        if !committed.folder.starts_with(harness.base.join("abs"))
                            && !committed.folder.starts_with(harness.base.join("env-abs"))
                        {
                            census.config_relative += 1;
                        }
                        if committed.folder.starts_with(harness.base.join("env-abs"))
                            || committed.folder.ends_with("sub/y")
                        {
                            census.config_expanded += 1;
                        }
                        if Some(file) != committed.config_files.first() {
                            census.config_beyond_first += 1;
                        }
                    }
                }
            }
            Verdict::CommittedWhereNuGetThrewOnAnUnreadSection => census.unread_faults += 1,
            Verdict::Declined {
                decline,
                nuget_threw,
            } => match nuget_threw {
                None => {
                    let kind = match &decline {
                        GlobalPackagesFolderDecline::ConfigFile { decline, .. } => {
                            let reason = decline.to_string();
                            format!(
                                "ConfigFile: {}",
                                reason.chars().take(50).collect::<String>()
                            )
                        }
                        other => {
                            let kind = format!("{other:?}");
                            kind.split([' ', '{', '(']).next().unwrap_or("").to_owned()
                        }
                    };
                    *census.declined_nuget_answered.entry(kind).or_default() += 1;
                }
                Some(threw) => {
                    census.declined_nuget_threw += 1;
                    *census.nuget_threw.entry(threw).or_default() += 1;
                }
            },
        }
    }
    eprintln!("settings_diff census (seed {seed}, cli_home {cli_home}): {census:#?}");
    census
}

fn cases(default: usize) -> usize {
    std::env::var("BORZOI_SETTINGS_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(default)
}

fn assert_floors(census: &Census) {
    let committed = census.environment + census.config + census.default;
    assert!(
        committed * 3 >= census.cases,
        "fewer than a third of the cases committed: {census:#?}"
    );
    for (what, count) in [
        ("from NUGET_PACKAGES", census.environment),
        ("from a config file", census.config),
        ("from a relative value", census.config_relative),
        ("from an expanded value", census.config_expanded),
        (
            "from a file other than the most significant",
            census.config_beyond_first,
        ),
        ("from the default", census.default),
        ("over three or more config files", census.many_files),
        ("where NuGet threw", census.declined_nuget_threw),
    ] {
        assert!(count > 0, "no case graded {what}: {census:#?}");
    }
}

#[test]
fn global_packages_folder_matches_nuget() {
    let census = sweep(0x5e77_1465, cases(300), false);
    assert_floors(&census);
}

#[test]
fn global_packages_folder_matches_nuget_under_dotnet_cli_home() {
    let census = sweep(0xc11_4093, cases(150), true);
    assert_floors(&census);
}

#[test]
#[ignore = "deep sweep; run with --ignored"]
fn deep_global_packages_folder_matches_nuget() {
    for (seed, cli_home) in [(1, false), (2, true), (3, false)] {
        let census = sweep(seed, cases(3000), cli_home);
        assert_floors(&census);
    }
}

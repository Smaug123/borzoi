//! The identity of the NuGet that ran a restore: the
//! `AssemblyInformationalVersion` of the `NuGet.Commands.dll` its restore task
//! loaded (`docs/nuget-dgspec-boundary-plan.md`, *Per-SDK drift*). B₁ commits
//! only for an identity on its verified list, so this is the key its trust
//! turns on.
//!
//! It is read from what the restore did, not predicted. Which code a restore
//! runs is MSBuild's to decide, and a project, its imports or its environment
//! can move it: `NuGetRestoreTargets` and `RestoreTaskAssemblyFile` point it
//! at another SDK's targets and tasks, a `UsingTask Override` re-registers the
//! task, a global property can condition either. So the restore writes a
//! detailed log, in which MSBuild names the assembly each task ran from
//! (`Using "RestoreTask" task from assembly "…"`), and NuGet's task loads
//! `NuGet.Commands` from beside its own assembly. Anything other than exactly
//! one such assembly declines.
//!
//! The log line is MSBuild's message, in the CLI's language; a host whose
//! language is not English logs another spelling, which declines.
//!
//! `tests/all/restore_graph.rs` holds the answer to the identity the oracle
//! reports for the `NuGet.Commands` it loads from each installed SDK: under a
//! `global.json` pinning each in turn, and under each way of redirecting the
//! restore task to another.
//!
//! A spawn-free restore (stage 9) has no log to read; its identity is a
//! prediction of the same fact, which A₁'s model of the restore cone has to
//! make.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use borzoi_assembly::Ecma335Assembly;
use borzoi_nuget::dgspec::NuGetIdentity;

/// Why the restoring NuGet's identity cannot be said.
#[derive(Debug)]
pub enum IdentityDecline {
    /// The restore wrote no log, or it could not be read.
    NoLog(String),
    /// The log names no assembly the restore task ran from.
    NoRestoreTask,
    /// The log names more than one.
    SeveralRestoreTasks(BTreeSet<PathBuf>),
    /// `NuGet.Commands.dll` could not be read or names no version.
    Unreadable { path: PathBuf, why: String },
}

impl fmt::Display for IdentityDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLog(why) => write!(f, "no restore log: {why}"),
            Self::NoRestoreTask => f.write_str("the restore log names no RestoreTask assembly"),
            Self::SeveralRestoreTasks(paths) => {
                write!(
                    f,
                    "the restore log names several RestoreTask assemblies: {paths:?}"
                )
            }
            Self::Unreadable { path, why } => write!(f, "{}: {why}", path.display()),
        }
    }
}

/// The prefix and suffix MSBuild writes around the assembly NuGet's restore
/// task ran from (`TaskFoundFromFactory`).
const TASK_LINE_PREFIX: &str = "Using \"RestoreTask\" task from assembly \"";
const TASK_LINE_SUFFIX: &str = "\".";

/// Every assembly a restore's detailed `log` says NuGet's restore task ran
/// from.
pub fn restore_task_assemblies(log: &str) -> BTreeSet<PathBuf> {
    log.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix(TASK_LINE_PREFIX)?
                .strip_suffix(TASK_LINE_SUFFIX)
                .map(PathBuf::from)
        })
        .collect()
}

/// The identity of the NuGet that ran the restore whose detailed log is
/// `log`.
pub fn restoring_nuget(log: &str) -> Result<NuGetIdentity, IdentityDecline> {
    let assemblies = restore_task_assemblies(log);
    let mut iter = assemblies.iter();
    let (Some(task), None) = (iter.next(), iter.next()) else {
        return Err(if assemblies.is_empty() {
            IdentityDecline::NoRestoreTask
        } else {
            IdentityDecline::SeveralRestoreTasks(assemblies)
        });
    };
    let Some(directory) = task.parent() else {
        return Err(IdentityDecline::Unreadable {
            path: task.clone(),
            why: "the task assembly is in no directory".to_owned(),
        });
    };
    nuget_commands_identity(&directory.join("NuGet.Commands.dll"))
}

/// The informational version of the `NuGet.Commands.dll` at `path`.
pub fn nuget_commands_identity(path: &Path) -> Result<NuGetIdentity, IdentityDecline> {
    let unreadable = |why: String| IdentityDecline::Unreadable {
        path: path.to_path_buf(),
        why,
    };
    let bytes = std::fs::read(path).map_err(|e| unreadable(e.to_string()))?;
    let assembly = Ecma335Assembly::parse(&bytes).map_err(|e| unreadable(format!("{e:?}")))?;
    match assembly.informational_version() {
        Ok(Some(version)) => Ok(NuGetIdentity::new(version)),
        Ok(None) => Err(unreadable("no AssemblyInformationalVersion".to_owned())),
        Err(e) => Err(unreadable(format!("{e:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::{IdentityDecline, restore_task_assemblies, restoring_nuget};

    #[test]
    fn the_restore_task_lines_are_read_and_nothing_else() {
        let log = r#"Build started.
       Using "RestoreTask" task from assembly "/sdk/10.0.401/NuGet.Build.Tasks.dll".
       Using "RestoreTaskEx" task from assembly "/sdk/other/NuGet.Build.Tasks.dll".
       Using "GetRestoreSettingsTask" task from assembly "/sdk/other/NuGet.Build.Tasks.dll".
  Using "RestoreTask" task from assembly "/sdk/10.0.401/NuGet.Build.Tasks.dll".
Using "RestoreTask" task from assembly "/unterminated
"#;
        assert_eq!(
            restore_task_assemblies(log),
            BTreeSet::from([PathBuf::from("/sdk/10.0.401/NuGet.Build.Tasks.dll")])
        );
    }

    #[test]
    fn no_line_or_two_assemblies_decline() {
        assert!(matches!(
            restoring_nuget("Build started.\n"),
            Err(IdentityDecline::NoRestoreTask)
        ));
        let two = "Using \"RestoreTask\" task from assembly \"/a/NuGet.Build.Tasks.dll\".\n\
                   Using \"RestoreTask\" task from assembly \"/b/NuGet.Build.Tasks.dll\".\n";
        assert!(matches!(
            restoring_nuget(two),
            Err(IdentityDecline::SeveralRestoreTasks(paths)) if paths.len() == 2
        ));
    }
}

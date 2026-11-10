//! `Ecma335Assembly::informational_version`, held to the Win32 version
//! resource over every managed assembly of every installed SDK.
//!
//! The .NET build stamps the `ProductVersion` entry of an assembly's version
//! resource with the same string as its `AssemblyInformationalVersion`
//! attribute, so the two are independent readings of one fact: the resource
//! is read here by a byte scan, without the metadata reader. The LSP keys its
//! trust in the restoring NuGet on this string (`borzoi::restore::
//! restoring_nuget`), where the reflection reading of the attribute is the
//! oracle; this sweep covers the reader on the rest of the SDK.
//!
//! Requires `DOTNET_ROOT` (the Nix devShell sets it).

use std::path::{Path, PathBuf};

use borzoi_assembly::{Ecma335Assembly, ImportError};

/// The value of the one `ProductVersion` string in a PE file's version
/// resource, or `None` when the key does not occur exactly once.
///
/// A `StringFileInfo` entry is `{wLength, wValueLength, wType}` (three
/// `u16`s), the key as NUL-terminated UTF-16LE, padding to a 32-bit boundary,
/// then the value as NUL-terminated UTF-16LE. Resource data is 32-bit aligned
/// in the file, so the padding can be computed from the file offset.
fn product_version(bytes: &[u8]) -> Option<String> {
    let key: Vec<u8> = "ProductVersion\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let hits: Vec<usize> = bytes
        .windows(key.len())
        .enumerate()
        .filter(|(_, window)| *window == key.as_slice())
        .map(|(offset, _)| offset)
        .collect();
    let [hit] = hits.as_slice() else {
        return None;
    };
    let value_start = (hit + key.len()).next_multiple_of(4);
    let value: Vec<u16> = bytes[value_start..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16(&value).ok()
}

/// Every installed SDK's version directory.
fn sdk_dirs() -> Vec<PathBuf> {
    let root = std::env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .expect("DOTNET_ROOT unset — run under `nix develop`");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root.join("sdk"))
        .expect("DOTNET_ROOT has an sdk/ directory")
        .map(|entry| entry.expect("list sdk/").path())
        .filter(|path| path.join("NuGet.Commands.dll").is_file())
        .collect();
    dirs.sort();
    dirs
}

fn dlls_in(dir: &Path) -> Vec<PathBuf> {
    let mut dlls: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("list {dir:?}: {e}"))
        .map(|entry| entry.expect("list the SDK").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "dll"))
        .collect();
    dlls.sort();
    dlls
}

#[test]
fn the_informational_version_is_the_version_resources_product_version() {
    let sdks = sdk_dirs();
    assert!(!sdks.is_empty(), "no SDK under DOTNET_ROOT");
    for sdk in sdks {
        let mut compared = 0usize;
        let mut nuget_commands = None;
        for dll in dlls_in(&sdk) {
            let bytes = std::fs::read(&dll).unwrap_or_else(|e| panic!("read {dll:?}: {e}"));
            let assembly = match Ecma335Assembly::parse(&bytes) {
                Ok(assembly) => assembly,
                // A native library beside the managed ones.
                Err(ImportError::NotAManagedAssembly) => continue,
                Err(e) => panic!("{dll:?}: {e:?}"),
            };
            let ours = assembly
                .informational_version()
                .unwrap_or_else(|e| panic!("{dll:?}: {e:?}"));
            let Some(resource) = product_version(&bytes) else {
                continue;
            };
            assert_eq!(
                ours.as_deref(),
                Some(resource.as_str()),
                "{dll:?}: the attribute and the version resource disagree"
            );
            compared += 1;
            if dll
                .file_name()
                .is_some_and(|name| name == "NuGet.Commands.dll")
            {
                nuget_commands = ours;
            }
        }
        assert!(
            compared >= 20,
            "{sdk:?}: only {compared} assemblies compared"
        );
        let nuget_commands =
            nuget_commands.unwrap_or_else(|| panic!("{sdk:?}: NuGet.Commands.dll not compared"));
        assert!(
            nuget_commands.contains('+'),
            "{sdk:?}: NuGet.Commands names no commit: {nuget_commands}"
        );
    }
}

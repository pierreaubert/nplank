//! External tool launchers for the embedded terminal (bacon, mbx, …).
//! Detection is a pure PATH scan so it is testable without a real PATH.

use std::path::PathBuf;

/// One launchable external tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLauncher {
    /// Short display name (`bacon`).
    pub name: &'static str,
    /// Program to execute.
    pub program: &'static str,
    /// Fixed arguments.
    pub args: &'static [&'static str],
}

/// Tools offered per workspace when present on PATH.
pub fn known_tools() -> Vec<ToolLauncher> {
    vec![
        ToolLauncher {
            name: "bacon",
            program: "bacon",
            args: &[],
        },
        ToolLauncher {
            name: "mbx",
            program: "mbx",
            args: &[],
        },
    ]
}

fn is_executable(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Finds `program` in `path_dirs`, returning the first executable match.
pub fn find_on_path(program: &str, path_dirs: &[PathBuf]) -> Option<PathBuf> {
    path_dirs
        .iter()
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

/// Known tools that are actually installed on this machine's PATH.
pub fn available_tools() -> Vec<ToolLauncher> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    known_tools()
        .into_iter()
        .filter(|tool| find_on_path(tool.program, &dirs).is_some())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tools_cover_bacon_and_mbx() {
        let names: Vec<&str> = known_tools().iter().map(|tool| tool.name).collect();
        assert_eq!(names, ["bacon", "mbx"]);
    }

    #[test]
    fn find_on_path_requires_executable_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("bacon");
        std::fs::write(&file, "#!/bin/sh\n").expect("write");
        assert!(find_on_path("bacon", &[dir.path().to_path_buf()]).is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            assert_eq!(
                find_on_path("bacon", &[dir.path().to_path_buf()]),
                Some(file)
            );
        }
        assert!(find_on_path("mbx", &[dir.path().to_path_buf()]).is_none());
    }
}

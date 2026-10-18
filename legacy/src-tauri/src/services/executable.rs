use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// The first executable named `name` on PATH, with the usual install locations a GUI app misses
/// because it never inherited a login shell's environment.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH");
    let home = env::var_os("HOME");
    find_executable_in(name, path.as_deref(), home.as_deref())
}

fn find_executable_in(
    name: &str,
    path: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let mut candidates = path
        .map(|paths| {
            env::split_paths(paths)
                .map(|path| path.join(name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    candidates.extend(
        ["/opt/homebrew/bin", "/usr/local/bin"]
            .into_iter()
            .map(|directory| Path::new(directory).join(name)),
    );
    if let Some(home) = home {
        candidates.extend(
            [".opencode/bin", ".local/bin"]
                .into_iter()
                .map(|directory| Path::new(home).join(directory).join(name)),
        );
    }
    candidates.into_iter().find(|path| is_executable(path))
}

pub fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{find_executable, find_executable_in, is_executable};

    #[test]
    fn only_a_searchable_program_file_counts_as_executable() {
        let temp = tempdir().unwrap();
        let program = temp.path().join("muster-test-program-3c19");
        fs::write(&program, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        assert!(is_executable(&program));
        // A directory and a path that is not there are both not a program to run.
        assert!(!is_executable(temp.path()));
        assert!(!is_executable(
            &temp.path().join("muster-test-missing-91af")
        ));
        assert!(find_executable("muster-test-missing-91af").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn finds_gui_installs_outside_path_for_pi_and_opencode() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempdir().unwrap();
        for (directory, name) in [(".local/bin", "pi"), (".opencode/bin", "opencode")] {
            let directory = home.path().join(directory);
            fs::create_dir_all(&directory).unwrap();
            let program = directory.join(name);
            fs::write(&program, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(
                find_executable_in(name, None, Some(home.path().as_os_str())),
                Some(program)
            );
        }
    }
}

use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// The first executable named `name` on PATH, with the usual install locations a GUI app misses
/// because it never inherited a login shell's environment.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let mut candidates = env::var_os("PATH")
        .map(|paths| {
            env::split_paths(&paths)
                .map(|path| path.join(name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    candidates.extend(
        ["/opt/homebrew/bin", "/usr/local/bin"]
            .into_iter()
            .map(|directory| Path::new(directory).join(name)),
    );
    if let Some(home) = env::var_os("HOME") {
        candidates.extend(
            [".opencode/bin", ".local/bin"]
                .into_iter()
                .map(|directory| Path::new(&home).join(directory).join(name)),
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

    use super::{find_executable, is_executable};

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
}

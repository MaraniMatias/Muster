use std::{io::ErrorKind, path::Path};

use crate::domain::{
    folder::OpenedFolder,
    ipc::{IpcError, IpcErrorCode},
};

pub fn open_folder(path: &Path) -> Result<OpenedFolder, IpcError> {
    let canonical_path = path.canonicalize().map_err(|error| {
        let (code, message) = if error.kind() == ErrorKind::NotFound {
            (
                IpcErrorCode::FolderMissing,
                "selected folder no longer exists",
            )
        } else {
            (
                IpcErrorCode::InvalidPath,
                "could not canonicalize selected folder",
            )
        };
        IpcError::new(code, format!("{message}: {error}"))
    })?;
    if !canonical_path.is_dir() {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "selected path is not a folder",
        ));
    }

    let path = canonical_path.display().to_string();
    let name = canonical_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.clone());

    Ok(OpenedFolder { path, name })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::domain::ipc::IpcErrorCode;

    use super::open_folder;

    #[test]
    fn canonicalizes_an_existing_folder() {
        let temp = tempdir().expect("temporary directory");
        let folder = temp.path().join("test workspace");
        fs::create_dir(&folder).expect("create folder");

        let opened = open_folder(&folder).expect("folder should open");

        assert_eq!(
            opened.path,
            folder.canonicalize().unwrap().display().to_string()
        );
        assert_eq!(opened.name, "test workspace");
    }

    #[test]
    fn rejects_a_file() {
        let temp = tempdir().expect("temporary directory");
        let file = temp.path().join("file.txt");
        fs::write(&file, "test").expect("create file");

        assert!(matches!(
            open_folder(&file).unwrap_err().code,
            IpcErrorCode::InvalidPath
        ));
    }
}

use std::path::{Component, Path, PathBuf};

use crate::domain::{
    ipc::{IpcError, IpcErrorCode},
    workspace::{checkout_id_for_path, repo_id_for_path, Checkout, Repo},
};

pub(super) fn registered_checkout<'a>(
    repos: &'a [Repo],
    checkout_id: &str,
    not_found_message: &'static str,
) -> Result<(&'a Repo, &'a Checkout), IpcError> {
    repos
        .iter()
        .find_map(|repo| {
            repo.checkouts
                .iter()
                .find(|checkout| checkout.id == checkout_id)
                .map(|checkout| (repo, checkout))
        })
        .ok_or_else(|| IpcError::new(IpcErrorCode::InvalidCheckout, not_found_message))
}

/// Resolves an existing relative path only after confirming the checkout ID belongs to the repo.
/// This is a Rust service primitive; it is not exposed as a general filesystem command to Tauri.
pub fn resolve_checkout_path(
    repo: &Repo,
    checkout_id: &str,
    relative_path: &Path,
) -> Result<PathBuf, IpcError> {
    let checkout = repo
        .checkouts
        .iter()
        .find(|checkout| checkout.id == checkout_id)
        .ok_or_else(|| {
            IpcError::new(
                IpcErrorCode::InvalidCheckout,
                "checkout ID is not registered under this repo",
            )
        })?;
    let checkout_root = validate_checkout_owner(repo, checkout)?;

    if relative_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(IpcError::new(
            IpcErrorCode::InvalidPath,
            "checkout paths must be relative and cannot contain '..'",
        ));
    }

    let target = checkout_root
        .join(relative_path)
        .canonicalize()
        .map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::PermissionDenied {
                IpcErrorCode::PermissionDenied
            } else {
                IpcErrorCode::FolderMissing
            };
            IpcError::new(
                code,
                format!("requested checkout path is unavailable: {error}"),
            )
        })?;

    if !target.starts_with(&checkout_root) {
        return Err(IpcError::new(
            IpcErrorCode::PathOutsideCheckout,
            "requested path resolves outside the checkout",
        ));
    }

    Ok(target)
}

fn validate_checkout_owner(repo: &Repo, checkout: &Checkout) -> Result<PathBuf, IpcError> {
    if repo.id != repo_id_for_path(&repo.root) {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "repo ID does not match its registered root",
        ));
    }
    if checkout.repo_id != repo.id {
        return Err(IpcError::new(
            IpcErrorCode::CheckoutOwnershipMismatch,
            "checkout does not belong to the requested repo",
        ));
    }

    let checkout_root = Path::new(&checkout.canonical_path)
        .canonicalize()
        .map_err(|error| {
            let code = match error.kind() {
                std::io::ErrorKind::NotFound => IpcErrorCode::FolderMissing,
                std::io::ErrorKind::PermissionDenied => IpcErrorCode::PermissionDenied,
                _ => IpcErrorCode::InvalidCheckout,
            };
            IpcError::new(code, format!("checkout folder is unavailable: {error}"))
        })?;
    if !checkout_root.is_dir()
        || checkout.id != checkout_id_for_path(&checkout_root.to_string_lossy())
    {
        return Err(IpcError::new(
            IpcErrorCode::InvalidCheckout,
            "checkout ID does not match its canonical path",
        ));
    }

    Ok(checkout_root)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use crate::domain::{ipc::IpcErrorCode, workspace::Repo};

    use super::resolve_checkout_path;

    #[test]
    fn rejects_parent_directory_traversal() {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        fs::create_dir(&root).expect("create repo");
        let repo = Repo::plain(&root, "now").expect("plain repo");

        let error = resolve_checkout_path(&repo, &repo.checkouts[0].id, Path::new("../outside"))
            .unwrap_err();

        assert!(matches!(error.code, IpcErrorCode::InvalidPath));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_resolve_outside_checkout() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        let outside = temp.path().join("outside");
        fs::create_dir(&root).expect("create repo");
        fs::create_dir(&outside).expect("create outside directory");
        fs::write(outside.join("secret.txt"), "secret").expect("create external file");
        symlink(&outside, root.join("linked")).expect("create escaping symlink");
        let repo = Repo::plain(&root, "now").expect("plain repo");

        let error =
            resolve_checkout_path(&repo, &repo.checkouts[0].id, Path::new("linked/secret.txt"))
                .unwrap_err();

        assert!(matches!(error.code, IpcErrorCode::PathOutsideCheckout));
    }

    #[test]
    fn rejects_a_checkout_id_from_a_different_repo() {
        let temp = tempdir().expect("temporary directory");
        let first_path = temp.path().join("first");
        let second_path = temp.path().join("second");
        fs::create_dir(&first_path).expect("create first repo");
        fs::create_dir(&second_path).expect("create second repo");
        let first = Repo::plain(&first_path, "now").expect("first repo");
        let second = Repo::plain(&second_path, "now").expect("second repo");

        let error = resolve_checkout_path(&first, &second.checkouts[0].id, Path::new("file.txt"))
            .unwrap_err();

        assert!(matches!(error.code, IpcErrorCode::InvalidCheckout));
    }

    #[test]
    fn rejects_checkout_records_owned_by_another_repo() {
        let temp = tempdir().expect("temporary directory");
        let first_path = temp.path().join("first");
        let second_path = temp.path().join("second");
        fs::create_dir(&first_path).expect("create first repo");
        fs::create_dir(&second_path).expect("create second repo");
        let mut first = Repo::plain(&first_path, "now").expect("first repo");
        let second = Repo::plain(&second_path, "now").expect("second repo");
        let foreign_checkout = second.checkouts[0].clone();
        first.checkouts.push(foreign_checkout.clone());

        let error =
            resolve_checkout_path(&first, &foreign_checkout.id, Path::new("file.txt")).unwrap_err();

        assert!(matches!(
            error.code,
            IpcErrorCode::CheckoutOwnershipMismatch
        ));
    }

    #[test]
    fn preserves_metacharacters_as_a_filename_without_interpreting_them() {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        fs::create_dir(&root).expect("create repo");
        let filename = "$(touch must-not-run); *.txt";
        fs::write(root.join(filename), "content").expect("create unusual filename");
        let repo = Repo::plain(&root, "now").expect("plain repo");

        let resolved = resolve_checkout_path(&repo, &repo.checkouts[0].id, Path::new(filename))
            .expect("resolve literal filename");

        assert_eq!(resolved.file_name().unwrap(), filename);
        assert!(!temp.path().join("must-not-run").exists());
    }
}

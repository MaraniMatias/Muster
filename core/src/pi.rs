//! Pi process identity and the worktrees its live work protects.

use std::path::Path;

/// Whether the operating system names the foreground program as Pi.
pub fn is_pi(app: &str) -> bool {
    app.eq_ignore_ascii_case("pi")
}

/// Whether a Pi process working in `working_directory` is working in
/// `checkout`.
///
/// Fail-closed: a process whose directory the operating system will not say,
/// or a checkout that cannot be resolved, protects the checkout. A worktree
/// removed under a running Pi is a worktree somebody loses work in, and the
/// cost of refusing is one worktree that stays.
pub fn protects_checkout(working_directory: Option<&Path>, checkout: &Path) -> bool {
    let Some(working_directory) = working_directory.and_then(|path| path.canonicalize().ok())
    else {
        return true;
    };
    checkout
        .canonicalize()
        .map(|checkout| working_directory.starts_with(checkout))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn a_process_protects_the_checkout_it_is_working_in_and_fails_closed() {
        let temp = tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let nested = checkout.join("nested");
        fs::create_dir_all(&nested).unwrap();

        // A process inside the checkout is working in it.
        assert!(super::protects_checkout(Some(&nested), &checkout));
        assert!(super::protects_checkout(Some(&checkout), &checkout));
        // A process somewhere else is not.
        assert!(!super::protects_checkout(Some(temp.path()), &checkout));
        // A process whose directory the operating system will not say is
        // treated as one that is: a worktree that stays is a nuisance, and a
        // worktree that goes under a running process is a loss.
        assert!(super::protects_checkout(None, &checkout));
        assert!(super::protects_checkout(
            Some(&checkout.join("gone")),
            &checkout
        ));
    }
}

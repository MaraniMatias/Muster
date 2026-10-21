use super::*;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn a_shared_read_budget_does_not_restart_for_each_command_or_checkout() {
    let temp = tempdir().unwrap();
    // Leave room for process startup and loaded CI workers; the second command
    // still exceeds the same deadline because both sleeps share this budget.
    let within = GitReadBudget::new(Duration::from_secs(2));
    git_success(
        temp.path(),
        &["-c", "alias.delay=!sleep 1.2", "delay"],
        within,
    )
    .unwrap();
    let error = git_success(
        temp.path(),
        &["-c", "alias.delay=!sleep 1.2", "delay"],
        within,
    )
    .unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(open_directory_with_budget(temp.path(), within)
        .unwrap_err()
        .contains("timed out"));
}

#[test]
fn discovered_checkout_totals_revalidate_identity_and_share_discovery_budget() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    fs::write(root.join("new-file"), "one\ntwo\n").unwrap();
    let checkout = &repo.checkouts[0];
    assert_eq!(
        diff_totals_for_checkout(&repo, checkout, GitReadBudget::default()).unwrap(),
        DiffTotals {
            additions: 2,
            deletions: 0
        }
    );
    assert!(
        diff_totals_for_checkout(&repo, checkout, GitReadBudget::new(Duration::ZERO))
            .unwrap_err()
            .contains("timed out")
    );
    let other = temp.path().join("other");
    fs::create_dir(&other).unwrap();
    git(&other, &["init", "--quiet", "--initial-branch=main"]);
    let mut spoofed = checkout.clone();
    spoofed.path = other.canonicalize().unwrap().to_str().unwrap().to_owned();
    spoofed.canonical_path = spoofed.path.clone();
    spoofed.id = checkout_id_for_path(&spoofed.path);
    spoofed.is_primary = false;
    let mut changed = repo.clone();
    changed.checkouts.push(spoofed.clone());
    assert!(
        diff_totals_for_checkout(&changed, &spoofed, GitReadBudget::default())
            .unwrap_err()
            .contains("repository changed")
    );
    fs::rename(root.join(".git"), root.join("old-git")).unwrap();
    assert!(diff_totals_for_checkout(&repo, checkout, GitReadBudget::default()).is_err());
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository(base: &Path) -> PathBuf {
    let root = base.join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "--quiet", "--initial-branch=trunk"]);
    git(
        &root,
        &[
            "-c",
            "user.email=muster@example.test",
            "-c",
            "user.name=Muster",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "first",
        ],
    );
    root
}

#[test]
fn plain_directory_has_canonical_identity_and_no_sessions() {
    let temp = tempdir().unwrap();
    let opened = open_directory(temp.path()).unwrap();
    assert_eq!(opened.repo.kind, RepoKind::Plain);
    assert_eq!(
        opened.checkout_id,
        checkout_id_for_path(path_text(&temp.path().canonicalize().unwrap()).unwrap())
    );
    assert_eq!(opened.repo.checkouts.len(), 1);
    assert!(opened.repo.checkouts[0].sessions.is_empty());
    let file = temp.path().join("file");
    fs::write(&file, "not a directory").unwrap();
    assert!(open_directory(&file).is_err());
    assert!(open_directory(&temp.path().join("missing")).is_err());
}

#[test]
fn linked_worktree_subdirectory_opens_shared_repo_and_correct_checkout() {
    let temp = tempdir().unwrap();
    let root = repository(temp.path());
    let worktree = temp.path().join("tree with spaces");
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            worktree.to_str().unwrap(),
        ],
    );
    fs::create_dir(worktree.join("src")).unwrap();
    let primary = open_directory(&root).unwrap();
    let opened = open_directory(&worktree.join("src")).unwrap();
    assert_eq!(opened.repo.id, primary.repo.id);
    assert_eq!(opened.repo.checkouts.len(), 2);
    assert_eq!(
        opened.checkout_id,
        checkout_id_for_path(worktree.canonicalize().unwrap().to_str().unwrap())
    );
    assert_eq!(opened.repo.checkouts[0].branch.as_deref(), Some("trunk"));
    assert_eq!(opened.repo.checkouts[1].branch.as_deref(), Some("feature"));
    assert!(!opened.repo.checkouts[1].is_primary);
    assert!(opened
        .repo
        .checkouts
        .iter()
        .all(|checkout| checkout.sessions.is_empty()));
}

#[test]
fn detached_and_missing_worktrees_remain_registered() {
    let temp = tempdir().unwrap();
    let root = repository(temp.path());
    let tree = temp.path().join("detached");
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            tree.to_str().unwrap(),
        ],
    );
    let opened = open_directory(&tree).unwrap();
    assert_eq!(opened.repo.checkouts[1].branch, None);
    assert!(opened.repo.checkouts[1].head.is_some());
    fs::remove_dir_all(&tree).unwrap();
    let opened = open_directory(&root).unwrap();
    assert_eq!(opened.repo.checkouts.len(), 2);
    assert!(opened.repo.checkouts[1].is_missing);
    assert_eq!(checkout_for_directory(&[opened.repo], &tree), None);
}

#[cfg(unix)]
#[test]
fn nul_listing_and_aliases_preserve_path_identity() {
    use std::os::unix::fs::symlink;
    let temp = tempdir().unwrap();
    let root = repository(temp.path());
    let tree = temp.path().join("tree\n\"quoted\"");
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            tree.to_str().unwrap(),
        ],
    );
    let alias = temp.path().join("alias");
    symlink(&tree, &alias).unwrap();
    let direct = open_directory(&tree).unwrap();
    let other = open_directory(&alias).unwrap();
    assert_eq!(direct.checkout_id, other.checkout_id);
    assert_eq!(direct.repo.id, other.repo.id);
    assert_eq!(
        direct.repo.checkouts[1].path,
        tree.canonicalize().unwrap().to_str().unwrap()
    );
}

#[test]
fn broken_git_metadata_and_corrupt_config_are_not_plain_folders() {
    let temp = tempdir().unwrap();
    fs::write(temp.path().join(".git"), "gitdir: /does-not-exist\n").unwrap();
    assert!(open_directory(temp.path()).is_err());
    fs::remove_file(temp.path().join(".git")).unwrap();
    let root = repository(temp.path());
    fs::write(root.join(".git/config"), "[broken\n").unwrap();
    assert!(open_directory(&root).is_err());
}

#[test]
fn a_git_stub_above_a_plain_folder_is_not_a_checkout_marker() {
    let temp = tempdir().unwrap();
    // A home directory holding nothing but a global excludes file: Git
    // never called it a repository, so neither does the folder under it.
    let home = temp.path().join("home");
    fs::create_dir_all(home.join(".git/info")).unwrap();
    fs::write(home.join(".git/info/exclude"), ".worktrees/\n").unwrap();
    let folder = home.join("notes");
    fs::create_dir(&folder).unwrap();

    let opened = open_directory(&folder).unwrap();
    assert_eq!(opened.repo.kind, RepoKind::Plain);
    assert_eq!(
        opened.repo.root,
        folder.canonicalize().unwrap().display().to_string()
    );
}

#[test]
fn unborn_head_is_not_a_fabricated_commit() {
    let temp = tempdir().unwrap();
    git(temp.path(), &["init", "--quiet", "--initial-branch=trunk"]);
    let opened = open_directory(temp.path()).unwrap();
    assert_eq!(opened.repo.checkouts[0].branch.as_deref(), Some("trunk"));
    assert_eq!(opened.repo.checkouts[0].head, None);
}

#[test]
fn cwd_matching_is_segment_aware_deepest_and_unambiguous() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("root");
    let nested = root.join("nested");
    let neighbour = temp.path().join("root-other");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir(&neighbour).unwrap();
    let mut repo = Repo::plain(&root, "now").unwrap();
    let checkout = Checkout::new(repo.id.clone(), &nested, false).unwrap();
    let id = checkout.id.clone();
    repo.checkouts.push(checkout);
    assert_eq!(checkout_for_directory(&[repo.clone()], &nested), Some(id));
    assert_eq!(checkout_for_directory(&[repo.clone()], &neighbour), None);
    let mut duplicate = repo.checkouts[1].clone();
    duplicate.id = "different".into();
    repo.checkouts.push(duplicate);
    assert_eq!(checkout_for_directory(&[repo], &nested), None);
}

#[test]
fn state_roundtrips_without_sessions_and_replaces_atomically() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("profile/native-workspace.json");
    assert_eq!(load(&path).unwrap(), NavigationState::default());
    assert_eq!(
        NavigationState::default().review_storage,
        crate::review::ReviewStorage::Workdir
    );
    let root = temp
        .path()
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let mut state = NavigationState {
        roots: vec![root.clone()],
        active_checkout_id: Some(checkout_id_for_path(&root)),
        review_storage: crate::review::ReviewStorage::Workdir,
        ..NavigationState::default()
    };
    save(&path, &state).unwrap();
    assert_eq!(load(&path).unwrap(), state);
    state.sidebar_width = 400.0;
    save(&path, &state).unwrap();
    assert_eq!(load(&path).unwrap(), state);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("sessions"));
    assert!(!text.contains("sessionOrder"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn invalid_state_cannot_overwrite_last_good_file() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("native-workspace.json");
    save(&path, &NavigationState::default()).unwrap();
    let before = fs::read(&path).unwrap();
    for width in [0.0, 239.0, 501.0, f32::NAN, f32::INFINITY] {
        let invalid = NavigationState {
            sidebar_width: width,
            ..NavigationState::default()
        };
        assert!(save(&path, &invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let invalid = NavigationState {
        roots: vec!["relative".into()],
        ..NavigationState::default()
    };
    assert!(save(&path, &invalid).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn a_stamped_corrupt_and_oversized_file_is_refused_without_rewrite() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("native-workspace.json");
    // A stamped file is not this build's file: there is no number to accept and no
    // reader left that knows what a stamped one meant.
    let stamped = br#"{"schemaVersion":1,"roots":["/work/repo"],"archivedCheckoutIds":[],"activeCheckoutId":null,"sidebarWidth":240.0,"reviewStorage":"workdir"}"#;
    fs::write(&path, stamped).unwrap();
    assert!(load(&path).unwrap_err().contains("schemaVersion"));
    assert!(save(&path, &NavigationState::default()).is_err());
    assert_eq!(fs::read(&path).unwrap(), stamped);
    fs::write(&path, b"{}").unwrap();
    assert!(load(&path).is_err());
    fs::write(&path, vec![b' '; STATE_CAP as usize + 1]).unwrap();
    assert!(load(&path).is_err());
}

#[test]
fn malformed_checkout_references_duplicates_and_archived_selection_are_refused() {
    let root = "/work/repo".to_owned();
    let mut state = NavigationState {
        roots: vec![root.clone()],
        ..NavigationState::default()
    };
    state.active_checkout_id = Some("session:123".into());
    assert!(state.validate().is_err());
    state.active_checkout_id = Some("checkout:/work/../repo".into());
    assert!(state.validate().is_err());
    state.active_checkout_id = Some("checkout:/work/tree".into());
    state
        .archived_checkout_ids
        .push("checkout:/work/tree".into());
    assert!(state.validate().is_err());
    state.active_checkout_id = None;
    state.archived_checkout_ids.clear();
    state
        .archived_checkout_ids
        .push(checkout_id_for_path(&root));
    assert!(state.validate().is_err());
    state.archived_checkout_ids.clear();
    state.roots.push(root);
    assert!(state.validate().is_err());
}

#[cfg(unix)]
#[test]
fn state_symlinks_are_refused_without_touching_the_target() {
    use std::os::unix::fs::symlink;
    let temp = tempdir().unwrap();
    let target = temp.path().join("legacy-data");
    fs::write(&target, "do not touch").unwrap();
    let path = temp.path().join("native-workspace.json");
    symlink(&target, &path).unwrap();
    assert!(load(&path).is_err());
    assert!(save(&path, &NavigationState::default()).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "do not touch");
}

#[cfg(unix)]
#[test]
fn stuck_git_read_is_timed_out_and_its_process_is_reaped() {
    use std::os::unix::fs::OpenOptionsExt;
    let temp = tempdir().unwrap();
    let root = repository(temp.path());
    let pipe = temp.path().join("config-pipe");
    assert!(Command::new("mkfifo")
        .arg(&pipe)
        .status()
        .unwrap()
        .success());
    let mut config = OpenOptions::new()
        .append(true)
        .open(root.join(".git/config"))
        .unwrap();
    writeln!(config, "[include]\n\tpath = {}", pipe.display()).unwrap();
    let began = std::time::Instant::now();
    let error = open_directory_within(&root, Duration::from_millis(100)).unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(began.elapsed() < Duration::from_secs(5));
    assert!(OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&pipe)
        .is_err());
}

#[test]
fn bare_repositories_are_refused() {
    let temp = tempdir().unwrap();
    git(temp.path(), &["init", "--quiet", "--bare"]);
    assert!(open_directory(temp.path()).unwrap_err().contains("bare"));
}

#[test]
fn explicitly_opened_submodule_has_its_own_identity() {
    let temp = tempdir().unwrap();
    let source = repository(temp.path());
    let parent = temp.path().join("parent");
    fs::create_dir(&parent).unwrap();
    git(&parent, &["init", "--quiet"]);
    git(
        &parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            source.to_str().unwrap(),
            "module",
        ],
    );
    let module = parent.join("module");
    let opened = open_directory(&module).unwrap();
    assert_eq!(
        opened.repo.root,
        module.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(opened.repo.checkouts.len(), 1);
    assert_eq!(opened.repo.id, repo_id_for_path(&opened.repo.root));
    assert_eq!(opened.repo.checkouts[0].branch.as_deref(), Some("trunk"));
}

#[test]
fn schema_requires_explicit_selection_and_refuses_unknown_fields() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("native-workspace.json");
    let mut json = serde_json::to_value(NavigationState::default()).unwrap();
    json.as_object_mut().unwrap().remove("activeCheckoutId");
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(load(&path).is_err());
    json["activeCheckoutId"] = serde_json::Value::Null;
    json["sessions"] = serde_json::json!([]);
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(load(&path).is_err());
    for id in [
        "checkout:/work/./tree",
        "checkout:/work//tree",
        "checkout:/work/tree/",
    ] {
        let state = NavigationState {
            roots: vec!["/work/repo".into()],
            active_checkout_id: Some(id.into()),
            ..NavigationState::default()
        };
        assert!(state.validate().is_err(), "{id}");
    }
}

#[test]
fn oversized_save_preserves_last_good_file() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("native-workspace.json");
    save(&path, &NavigationState::default()).unwrap();
    let before = fs::read(&path).unwrap();
    let state = NavigationState {
        roots: (0..1024)
            .map(|index| format!("/{}-{index}", "a".repeat(3000)))
            .collect(),
        ..NavigationState::default()
    };
    assert!(state.validate().is_ok());
    assert!(save(&path, &state).unwrap_err().contains("1 MiB"));
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn overflowing_git_output_is_refused_instead_of_truncated() {
    let temp = tempdir().unwrap();
    let began = std::time::Instant::now();
    // Thirty-two megabytes of zeros, written as fast as the pipe takes them:
    // twice the cap, and quick. A flood that produces its bytes slowly lets
    // the deadline arrive before the cap, and the test would then be about the
    // deadline rather than about the cap.
    let error = git_output(
        temp.path(),
        &[
            "-c",
            "alias.flood=!head -c 33554432 /dev/zero",
            "flood",
        ],
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert!(error.contains("exceeded"), "{error}");
    assert!(began.elapsed() < Duration::from_secs(5));
}

fn main_repo(base: &Path) -> Repo {
    let root = repository(base);
    git(&root, &["branch", "-m", "main"]);
    open_directory(&root).unwrap().repo
}

#[test]
fn defaults_resolve_local_remote_master_and_explicit_branches() {
    let temp = tempdir().unwrap();
    let mut repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    let defaults = worktree_defaults(&repo).unwrap();
    assert_eq!(defaults.default_branch, "main");
    assert_eq!(defaults.start_ref, "refs/heads/main");
    assert_eq!(defaults.location, root.join(".worktrees"));
    git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    assert_eq!(
        worktree_defaults(&repo).unwrap().start_ref,
        "refs/remotes/origin/main"
    );
    git(root, &["update-ref", "-d", "refs/remotes/origin/main"]);
    git(root, &["branch", "-m", "master"]);
    assert_eq!(worktree_defaults(&repo).unwrap().default_branch, "master");
    git(root, &["branch", "-m", "trunk"]);
    assert!(worktree_defaults(&repo).is_err());
    repo.default_branch = Some("trunk".into());
    assert_eq!(
        worktree_defaults(&repo).unwrap().start_ref,
        "refs/heads/trunk"
    );
    git(root, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
    git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    repo.default_branch = None;
    assert_eq!(
        worktree_defaults(&repo).unwrap().start_ref,
        "refs/remotes/origin/trunk"
    );
}

#[test]
fn creates_sanitized_worktree_and_preserves_external_worktree_and_exclude() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    let external = temp.path().join("external");
    git(
        root,
        &[
            "worktree",
            "add",
            "-b",
            "external",
            external.to_str().unwrap(),
        ],
    );
    fs::write(root.join(".git/info/exclude"), "existing-rule").unwrap();
    let opened = create_worktree(&repo, "../new/task", "feature/task", "").unwrap();
    let checkout = opened
        .repo
        .checkouts
        .iter()
        .find(|checkout| checkout.id == opened.checkout_id)
        .unwrap();
    assert_eq!(
        checkout.path,
        root.join(".worktrees/-new-task").to_str().unwrap()
    );
    assert_eq!(checkout.branch.as_deref(), Some("feature/task"));
    assert_eq!(opened.repo.checkouts.len(), 3);
    assert_eq!(
        fs::read_to_string(root.join(".git/info/exclude")).unwrap(),
        "existing-rule\n/.worktrees/\n"
    );
    assert!(external.is_dir());
    create_worktree(&repo, "another", "feature/another", "").unwrap();
    assert_eq!(
        fs::read_to_string(root.join(".git/info/exclude"))
            .unwrap()
            .matches("/.worktrees/")
            .count(),
        1
    );
    let status = git_success(root, &["status", "--porcelain"], GIT_READ_TIMEOUT).unwrap();
    assert!(status.stdout.is_empty());
}

#[test]
fn creation_accepts_a_named_or_qualified_base_and_refuses_one_that_is_not_there() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    // A repository Git will not guess a default for is the ordinary case
    // here, so the host has to be able to hand the base in both ways.
    git(root, &["branch", "-m", "trunk"]);
    assert!(worktree_defaults(&repo).is_err());

    let opened = create_worktree(&repo, "named", "feature/named", "trunk").unwrap();
    assert_eq!(opened.repo.checkouts.len(), 2);
    assert_eq!(
        opened
            .repo
            .checkouts
            .iter()
            .find(|checkout| checkout.id == opened.checkout_id)
            .and_then(|checkout| checkout.branch.clone())
            .as_deref(),
        Some("feature/named")
    );

    let qualified =
        create_worktree(&repo, "qualified", "feature/qualified", "refs/heads/trunk").unwrap();
    assert!(qualified
        .repo
        .checkouts
        .iter()
        .any(|checkout| checkout.branch.as_deref() == Some("feature/qualified")));

    for base in ["nope", "refs/heads/nope", "--upload-pack=evil"] {
        assert!(
            create_worktree(&repo, "refused", "feature/refused", base).is_err(),
            "{base} was accepted as a base"
        );
    }
    assert!(!root.join(".worktrees/refused").exists());
}

#[test]
fn creation_rejects_taken_existing_and_nonliteral_branches_and_occupied_paths() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    assert!(create_worktree(&repo, "task", "main", "")
        .unwrap_err()
        .contains("already checked out"));
    git(Path::new(&repo.root), &["branch", "existing"]);
    assert!(create_worktree(&repo, "task", "existing", "")
        .unwrap_err()
        .contains("already exists"));
    for branch in ["--detach", "bad name", "bad..ref", "@{-1}", "HEAD", ""] {
        assert!(
            create_worktree(&repo, "task", branch, "").is_err(),
            "{branch}"
        );
    }
    for task in ["", "...", "   "] {
        assert!(
            create_worktree(&repo, task, "feature/task", "").is_err(),
            "{task}"
        );
    }
    fs::create_dir(Path::new(&repo.root).join(".worktrees")).unwrap();
    fs::create_dir(Path::new(&repo.root).join(".worktrees/occupied")).unwrap();
    assert!(create_worktree(&repo, "occupied", "feature/task", "")
        .unwrap_err()
        .contains("already exists"));
    assert!(optional_ref(
        Path::new(&repo.root),
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/heads/feature/task"
        ],
        GIT_READ_TIMEOUT
    )
    .unwrap()
    .is_none());
    let unicode = sanitize_component(&"界".repeat(100)).unwrap();
    assert!(unicode.chars().count() <= 80 && unicode.len() <= 180);
}

#[test]
fn mutation_revalidates_primary_and_refuses_plain_bare_missing_or_changed_identity() {
    let temp = tempdir().unwrap();
    let mut repo = main_repo(temp.path());
    repo.id = "repo:incorrect".into();
    assert!(worktree_defaults(&repo).is_err());
    assert!(!Path::new(&repo.root).join(".worktrees").exists());
    let plain = temp.path().join("plain");
    fs::create_dir(&plain).unwrap();
    assert!(worktree_defaults(&Repo::plain(&plain, "now").unwrap()).is_err());
    let bare = temp.path().join("bare");
    git(temp.path(), &["init", "--bare", bare.to_str().unwrap()]);
    repo.root = bare.to_str().unwrap().into();
    assert!(worktree_defaults(&repo).is_err());
    repo.root = temp.path().join("missing").to_str().unwrap().into();
    assert!(worktree_defaults(&repo).is_err());
    let real = open_directory(&temp.path().join("repo")).unwrap().repo;
    let linked = temp.path().join("linked");
    git(
        Path::new(&real.root),
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    repo = real;
    repo.root = linked.canonicalize().unwrap().to_str().unwrap().into();
    repo.id = repo_id_for_path(&repo.root);
    assert!(worktree_defaults(&repo).is_err());
}

fn removal_confirmation(
    info: &WorktreeRemovalInfo,
    confirm_dirty: bool,
    delete_branch: bool,
) -> WorktreeRemovalConfirmation {
    WorktreeRemovalConfirmation {
        checkout_id: info.checkout_id.clone(),
        path: info.path.clone(),
        branch: info.branch.clone(),
        head: info.head.clone(),
        dirty_files: info.dirty_files.clone(),
        default_ref: info.default_ref.clone(),
        unmerged_commits: info.unmerged_commits,
        confirm_dirty,
        confirm_unmerged: false,
        delete_branch,
    }
}

#[test]
fn detached_worktree_commits_require_explicit_confirmation_before_removal() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let path = temp.path().join("detached");
    git(
        Path::new(&repo.root),
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    fs::write(path.join("detached.txt"), "unique detached commit\n").unwrap();
    git(&path, &["add", "detached.txt"]);
    commit(&path, "detached-only commit");

    let opened = open_directory(&path).unwrap();
    let checkout = opened
        .repo
        .checkouts
        .iter()
        .find(|checkout| checkout.id == opened.checkout_id)
        .unwrap()
        .clone();
    let info = worktree_removal_info(&opened.repo, &checkout).unwrap();
    assert_eq!(info.branch, None);
    assert_eq!(info.unmerged_commits, 1);

    let mut confirmation = removal_confirmation(&info, false, false);
    assert!(remove_worktree(&opened.repo, &checkout, &confirmation)
        .unwrap_err()
        .contains("explicitly confirm"));
    assert!(path.is_dir());

    confirmation.confirm_unmerged = true;
    remove_worktree(&opened.repo, &checkout, &confirmation).unwrap();
    assert!(!path.exists());
}

#[test]
fn removal_previews_dirty_files_and_unmerged_commits_and_keeps_branch_by_default() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let opened = create_worktree(&repo, "task", "feature/task", "").unwrap();
    let checkout = opened
        .repo
        .checkouts
        .iter()
        .find(|checkout| checkout.id == opened.checkout_id)
        .unwrap()
        .clone();
    let path = Path::new(&checkout.path);
    fs::write(path.join("committed.txt"), "branch commit\n").unwrap();
    git(path, &["add", "committed.txt"]);
    commit(path, "unmerged commit");
    fs::write(path.join("uncommitted file.txt"), "keep or lose\n").unwrap();

    let info = worktree_removal_info(&opened.repo, &checkout).unwrap();
    assert_eq!(info.branch.as_deref(), Some("feature/task"));
    assert_eq!(info.unmerged_commits, 1);
    assert_eq!(info.default_branch.as_deref(), Some("main"));
    assert!(info
        .dirty_files
        .iter()
        .any(|file| file == "uncommitted file.txt"));

    let mut confirmation = removal_confirmation(&info, false, false);
    assert!(remove_worktree(&opened.repo, &checkout, &confirmation)
        .unwrap_err()
        .contains("explicitly confirm"));
    assert!(path.is_dir());
    confirmation.confirm_dirty = true;
    let removed = remove_worktree(&opened.repo, &checkout, &confirmation).unwrap();
    assert!(removed.warning.is_none());
    assert!(!path.exists());
    assert!(git_output(
        Path::new(&repo.root),
        &["show-ref", "--verify", "--quiet", "refs/heads/feature/task"],
        GIT_READ_TIMEOUT,
    )
    .unwrap()
    .status
    .success());
    assert!(Path::new(&repo.root).join(".git").exists());
}

#[test]
fn removal_deletes_a_branch_only_when_the_confirmation_asks_for_it() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let opened = create_worktree(&repo, "task", "feature/task", "").unwrap();
    let checkout = opened
        .repo
        .checkouts
        .iter()
        .find(|checkout| checkout.id == opened.checkout_id)
        .unwrap()
        .clone();
    let info = worktree_removal_info(&opened.repo, &checkout).unwrap();
    let confirmation = removal_confirmation(&info, false, true);

    let removed = remove_worktree(&opened.repo, &checkout, &confirmation).unwrap();
    assert!(removed.warning.is_none());
    assert!(!Path::new(&checkout.path).exists());
    assert!(!git_output(
        Path::new(&repo.root),
        &["show-ref", "--verify", "--quiet", "refs/heads/feature/task"],
        GIT_READ_TIMEOUT,
    )
    .unwrap()
    .status
    .success());
}

#[test]
fn removal_refuses_primary_missing_and_unregistered_paths_without_deleting_them() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let opened = create_worktree(&repo, "task", "feature/task", "").unwrap();
    let checkout = opened
        .repo
        .checkouts
        .iter()
        .find(|checkout| checkout.id == opened.checkout_id)
        .unwrap()
        .clone();
    let primary = opened
        .repo
        .checkouts
        .iter()
        .find(|checkout| checkout.is_primary)
        .unwrap();
    assert!(worktree_removal_info(&opened.repo, primary).is_err());

    let info = worktree_removal_info(&opened.repo, &checkout).unwrap();
    let confirmation = removal_confirmation(&info, false, false);
    git(
        Path::new(&repo.root),
        &["worktree", "remove", "--force", &checkout.path],
    );
    fs::create_dir(&checkout.path).unwrap();
    let sentinel = Path::new(&checkout.path).join("not-a-worktree.txt");
    fs::write(&sentinel, "leave me").unwrap();
    assert!(remove_worktree(&opened.repo, &checkout, &confirmation).is_err());
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "leave me");

    fs::remove_dir_all(&checkout.path).unwrap();
    git(
        Path::new(&repo.root),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature/missing",
            &checkout.path,
        ],
    );
    fs::remove_dir_all(&checkout.path).unwrap();
    let refreshed = open_directory(Path::new(&repo.root)).unwrap();
    let missing = refreshed
        .repo
        .checkouts
        .iter()
        .find(|item| item.id == checkout.id)
        .unwrap();
    assert!(missing.is_missing);
    assert!(worktree_removal_info(&refreshed.repo, missing).is_err());
}

#[cfg(unix)]
#[test]
fn worktree_creation_refuses_symlink_locations_destinations_and_exclude() {
    use std::os::unix::fs::symlink;
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    for target in [&outside, &temp.path().join("missing")] {
        symlink(target, root.join(".worktrees")).unwrap();
        assert!(create_worktree(&repo, "task", "feature/task", "").is_err());
        fs::remove_file(root.join(".worktrees")).unwrap();
    }
    fs::create_dir(root.join(".worktrees")).unwrap();
    symlink(temp.path().join("missing"), root.join(".worktrees/task")).unwrap();
    assert!(create_worktree(&repo, "task", "feature/task", "").is_err());
    fs::remove_file(root.join(".worktrees/task")).unwrap();
    let exclude = root.join(".git/info/exclude");
    fs::remove_file(&exclude).unwrap();
    let victim = outside.join("victim");
    fs::write(&victim, "/.worktrees/\n").unwrap();
    symlink(&victim, &exclude).unwrap();
    assert!(create_worktree(&repo, "task", "feature/task", "").is_err());
    assert_eq!(fs::read_to_string(&victim).unwrap(), "/.worktrees/\n");
    fs::remove_file(&exclude).unwrap();
    fs::remove_dir(root.join(".git/info")).unwrap();
    symlink(&outside, root.join(".git/info")).unwrap();
    assert!(create_worktree(&repo, "task", "feature/task", "")
        .unwrap_err()
        .contains("outside"));
    assert!(!outside.join("exclude").exists());
}

fn commit(root: &Path, message: &str) {
    git(
        root,
        &[
            "-c",
            "user.email=muster@example.test",
            "-c",
            "user.name=Muster",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
}

#[cfg(unix)]
#[test]
fn failed_discovery_after_creation_reports_partial_success_without_rollback() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    // A real post-checkout hook makes the new directory stop identifying as
    // its own checkout after Git creates it, exercising the production path.
    let hook = root.join(".git/hooks/post-checkout");
    fs::write(&hook, "#!/bin/sh\nrm -f .git\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let error = create_worktree(&repo, "partial", "feature/partial", "").unwrap_err();
    assert!(
        error.contains("Git created the worktree") && error.contains("left untouched"),
        "{error}"
    );
    assert!(root.join(".worktrees/partial").is_dir());
    assert!(optional_ref(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/heads/feature/partial"
        ],
        GIT_READ_TIMEOUT
    )
    .unwrap()
    .is_some());
}

#[test]
fn totals_count_staged_unstaged_and_untracked_once_and_skip_ignored_binary_files() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    fs::write(root.join("tracked"), "old\nkeep\n").unwrap();
    git(root, &["add", "tracked"]);
    commit(root, "tracked");
    fs::write(root.join("tracked"), "staged\nkeep\n").unwrap();
    git(root, &["add", "tracked"]);
    fs::write(root.join("tracked"), "final\nkeep\n").unwrap();
    fs::write(root.join("untracked\tfile\n"), "one\ntwo").unwrap();
    fs::write(root.join("binary"), b"binary\0\n").unwrap();
    fs::write(root.join(".git/info/exclude"), "ignored\n").unwrap();
    fs::write(root.join("ignored"), "ignore\n").unwrap();
    assert_eq!(
        diff_totals(root).unwrap(),
        DiffTotals {
            additions: 3,
            deletions: 1
        }
    );
    fs::remove_file(root.join("untracked\tfile\n")).unwrap();
    fs::write(root.join("tracked"), "old\nkeep\n").unwrap();
    // Staged change undone in the worktree is a net zero, not two diffs.
    assert_eq!(diff_totals(root).unwrap(), DiffTotals::default());
    assert!(diff_totals(temp.path()).is_err());
}

#[test]
fn totals_include_feature_commits_from_merge_base_and_renames() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    fs::write(root.join("original"), "one\ntwo\n").unwrap();
    git(root, &["add", "original"]);
    commit(root, "base file");
    let tree = temp.path().join("feature");
    git(
        root,
        &["worktree", "add", "-b", "feature", tree.to_str().unwrap()],
    );
    git(&tree, &["mv", "original", "renamed\tfile\n"]);
    fs::write(tree.join("added"), "three\n").unwrap();
    git(&tree, &["add", "."]);
    commit(&tree, "feature changes");
    assert_eq!(
        diff_totals(&tree).unwrap(),
        DiffTotals {
            additions: 1,
            deletions: 0
        }
    );
    assert_eq!(diff_totals(root).unwrap(), DiffTotals::default());
}

#[test]
fn unresolved_base_and_unborn_head_are_errors_not_clean_totals() {
    let temp = tempdir().unwrap();
    let root = repository(temp.path());
    git(&root, &["checkout", "--detach", "HEAD"]);
    assert!(diff_totals(&root).is_err());
    let unborn = temp.path().join("unborn");
    fs::create_dir(&unborn).unwrap();
    git(&unborn, &["init", "--quiet", "--initial-branch=main"]);
    assert!(diff_totals(&unborn).is_err());
    assert!(worktree_defaults(&open_directory(&unborn).unwrap().repo).is_err());
}

#[test]
fn invalid_numstat_and_count_overflow_are_not_reported_as_clean() {
    for bytes in [
        b"1\t1\tfile".as_slice(),
        b"-\t1\tfile\0",
        b"1\t2\t\0old\0",
        b"x\t2\tfile\0",
        b"18446744073709551615\t0\ta\0\x31\t0\tb\0",
    ] {
        assert!(parse_numstat_totals(bytes).is_err(), "{bytes:?}");
    }
    assert_eq!(
        parse_numstat_totals(b"2\t1\t\0old\0new\0-\t-\tbinary\0").unwrap(),
        DiffTotals {
            additions: 2,
            deletions: 1
        }
    );
}

#[cfg(unix)]
#[test]
fn untracked_count_is_contained_bounded_and_refuses_nonregular_files() {
    use std::os::unix::fs::symlink;
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    let outside = temp.path().join("secret");
    fs::write(&outside, "not counted\n").unwrap();
    symlink(&outside, root.join("link")).unwrap();
    assert_eq!(diff_totals(root).unwrap(), DiffTotals::default());
    let file = root.join("text");
    fs::write(&file, "one\ntwo").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    assert!(untracked_lines(root, Path::new("text"), &mut 3, deadline).is_err());
    assert!(untracked_lines(root, Path::new("text"), &mut 100, std::time::Instant::now()).is_err());
    assert!(untracked_lines(root, Path::new("."), &mut 100, deadline).is_err());
    fs::write(&file, b"\0binary").unwrap();
    assert!(untracked_lines(root, Path::new("text"), &mut 3, deadline).is_err());
    assert!(untracked_lines(root, Path::new("text"), &mut 100, std::time::Instant::now()).is_err());
    let mut late_binary = vec![b'a'; 8000];
    late_binary.extend_from_slice(b"\0\n");
    fs::write(&file, late_binary).unwrap();
    assert_eq!(
        untracked_lines(root, Path::new("text"), &mut 9000, deadline).unwrap(),
        Some(1)
    );
}

#[test]
fn mutation_drains_large_diagnostics_without_killing_or_timing_out() {
    let temp = tempdir().unwrap();
    let output = git_write(
        temp.path(),
        &[
            "-c",
            "alias.noisy=!dd if=/dev/zero bs=1024 count=80 1>&2",
            "noisy",
        ],
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stderr.len(), GIT_DIAGNOSTIC_CAP);
}

#[test]
fn incomplete_and_invalid_git_listings_fail_closed() {
    assert!(parse_worktrees(b"worktree /work/repo\0").is_err());
    assert!(parse_worktrees(b"worktree relative\0\0").is_err());
    assert!(parse_worktrees(b"worktree /work/repo\0HEAD invalid\0\0").is_err());
}

/// Write and commit a file, so a later commit has something to sit above.
fn write_and_commit(root: &Path, name: &str, body: &str) {
    fs::write(root.join(name), body).unwrap();
    git(root, &["add", name]);
    commit(root, name);
}

#[test]
fn status_reports_local_committed_untracked_and_renamed_files_with_one_base() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    write_and_commit(root, "committed.txt", "one\n");
    let checkout = &repo.checkouts[0];

    // A commit this checkout holds above its base is part of the same change set: the
    // panel would otherwise show nothing for work an agent pushed before it opened.
    git(root, &["checkout", "--quiet", "-b", "feature"]);
    write_and_commit(root, "pushed.txt", "pushed\n");
    fs::write(root.join("dirty.txt"), "dirty\nmore\n").unwrap();
    fs::write(root.join("untracked.txt"), "new\n").unwrap();
    git(root, &["mv", "committed.txt", "renamed.txt"]);

    let status = status(&repo, checkout, GitReadBudget::default()).unwrap();
    assert_eq!(status.default_branch, "main");
    assert_eq!(status.branch.as_deref(), Some("feature"));
    assert_eq!(status.ahead_count, 1);
    assert!(status.head.is_some());

    let file = |name: &str| {
        status
            .files
            .iter()
            .find(|file| file.path == name)
            .unwrap_or_else(|| panic!("{name} is missing from {:?}", status.files))
    };
    assert_eq!(file("pushed.txt").status, "A");
    assert_eq!(file("pushed.txt").additions, Some(1));
    assert_eq!(file("dirty.txt").status, "??");
    assert_eq!(file("dirty.txt").additions, Some(2));
    assert_eq!(file("dirty.txt").deletions, None);
    // A rename is one row carrying both names, which is what a reader needs to know that
    // a name left rather than that a file appeared.
    assert_eq!(
        file("renamed.txt").old_path.as_deref(),
        Some("committed.txt")
    );
    assert!(!status.files.is_empty());
}

#[test]
fn staged_and_unstaged_edits_of_one_file_are_counted_together_as_git_counts_them() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    write_and_commit(root, "file.txt", "one\ntwo\nthree\n");
    git(root, &["checkout", "--quiet", "-b", "feature"]);
    fs::write(root.join("file.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    git(root, &["add", "file.txt"]);
    fs::write(root.join("file.txt"), "one\nTWO\nthree\nfour\n").unwrap();

    let status = status(&repo, &repo.checkouts[0], GitReadBudget::default()).unwrap();
    let file = status
        .files
        .iter()
        .find(|file| file.path == "file.txt")
        .expect("the edited file is listed");
    assert_eq!(file.status, "MM");
    assert_eq!((file.additions, file.deletions), (Some(2), Some(1)));
    // The same base the sidebar's own total counts against.
    assert_eq!(
        diff_totals_for_checkout(&repo, &repo.checkouts[0], GitReadBudget::default()).unwrap(),
        DiffTotals {
            additions: 2,
            deletions: 1
        }
    );
}

#[test]
fn a_binary_change_is_listed_and_counted_by_nothing() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    fs::write(root.join("blob.bin"), [0, 1, 2]).unwrap();
    git(root, &["add", "blob.bin"]);
    commit(root, "blob");
    fs::write(root.join("blob.bin"), [0, 9, 2]).unwrap();

    let status = status(&repo, &repo.checkouts[0], GitReadBudget::default()).unwrap();
    let file = status
        .files
        .iter()
        .find(|file| file.path == "blob.bin")
        .expect("the binary file is listed");
    assert_eq!((file.additions, file.deletions), (None, None));

    let diff = file_diff(
        &repo,
        &repo.checkouts[0],
        "blob.bin",
        GitReadBudget::default(),
    )
    .unwrap();
    assert!(diff.is_binary);
    assert!(diff.patch.is_empty());
    assert!(!diff.revision.is_empty());
    // Three bytes with no name that claims otherwise are not a picture, and the
    // diff says nothing about one.
    assert_eq!(diff.image, None);
}

/// A change with no text is named by what its bytes are, so a reviewer is told
/// that a picture arrived rather than that a file stopped being readable.
#[test]
fn an_image_change_names_the_picture_rather_than_the_encoding() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
    // Added: the picture is new and untracked.
    fs::write(root.join("added.png"), png).unwrap();
    let added = file_diff(
        &repo,
        &repo.checkouts[0],
        "added.png",
        GitReadBudget::default(),
    )
    .expect("the new picture is diffable");
    assert!(added.is_binary);
    assert_eq!(added.image, Some(ImageFormat::Png));

    // The bytes decide, not the name: a JPEG called `.png` is a JPEG, and saying
    // otherwise would tell a reader to look for a picture that is not there.
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0];
    jpeg.extend_from_slice(&[0u8; 32]);
    fs::write(root.join("picture.png"), &jpeg).unwrap();
    let misnamed = file_diff(
        &repo,
        &repo.checkouts[0],
        "picture.png",
        GitReadBudget::default(),
    )
    .expect("the picture is diffable whatever it is called");
    assert_eq!(misnamed.image, Some(ImageFormat::Jpeg));

    // Nothing in the name: a file called `logo` that opens with the PNG signature is
    // the picture its bytes describe.
    fs::write(root.join("logo"), png).unwrap();
    let extensionless = file_diff(&repo, &repo.checkouts[0], "logo", GitReadBudget::default())
        .expect("the picture is diffable whatever it is called");
    assert_eq!(extensionless.image, Some(ImageFormat::Png));

    // WebP is a RIFF container: the form sits at bytes 8-11, behind the size field,
    // and a chunk follows it. A name that promises nothing does not stop the bytes
    // from being read at the offset the format puts them.
    fs::write(
        root.join("screenshot"),
        b"RIFF\x1a\x00\x00\x00WEBPVP8 \x00\x00\x00\x00",
    )
    .unwrap();
    let webp = file_diff(
        &repo,
        &repo.checkouts[0],
        "screenshot",
        GitReadBudget::default(),
    )
    .expect("the webp picture is diffable whatever it is called");
    assert!(webp.is_binary);
    assert_eq!(webp.image, Some(ImageFormat::WebP));

    // Removed: there are no bytes left to read, so the name is the only answer
    // there is, and it is believed only for a name it could be naming.
    git(root, &["add", "."]);
    commit(root, "pictures");
    fs::remove_file(root.join("picture.png")).unwrap();
    let removed = file_diff(
        &repo,
        &repo.checkouts[0],
        "picture.png",
        GitReadBudget::default(),
    )
    .expect("the removed picture is diffable");
    assert_eq!(removed.image, Some(ImageFormat::Png));
    assert!(removed.is_binary);
}

/// A file that is still there is named by its bytes or by nothing at all, because
/// a name is a claim about a file rather than evidence of one. A `.png` full of
/// Git LFS pointer text is text, an empty `.png` is an empty file rather than a
/// missing one, and `WEBP` at the end of some other RIFF payload is a coincidence
/// of suffixes.
#[test]
fn a_present_file_is_named_by_its_bytes_and_never_by_its_extension() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    let image = |name: &str| file_diff(&repo, &repo.checkouts[0], name, GitReadBudget::default());

    // Bytes that are not a picture, under a name that says they are.
    fs::write(
        root.join("not-a-picture.png"),
        b"\x00\x01\x02 not a picture \x00\xff",
    )
    .unwrap();
    let arbitrary = image("not-a-picture.png").expect("the blob is diffable");
    assert!(arbitrary.is_binary);
    assert_eq!(arbitrary.image, None);

    // A Git LFS pointer is text Git will print, so the change stays a text diff and
    // never reaches the presentation that would claim a picture arrived.
    fs::write(
        root.join("pointer.png"),
        "version https://git-lfs.github.com/spec/v1\n\
         oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
         size 12345\n",
    )
    .unwrap();
    let pointer = image("pointer.png").expect("the pointer is diffable");
    assert!(!pointer.is_binary);
    assert_eq!(pointer.image, None);
    assert!(pointer.patch.contains("git-lfs.github.com"));

    // Present but empty: there are bytes to read, there are none, and a file with no
    // bytes is not a picture. Reading it is not the same as finding it missing, which
    // is the only case the name is believed in.
    fs::write(root.join("empty.png"), b"").unwrap();
    let empty = image("empty.png").expect("an empty picture is still diffable");
    assert_eq!(empty.image, None);

    // RIFF, a size field, and then four bytes that are not the form: a `WEBP` at the
    // end of the header is not a WebP file.
    fs::write(
        root.join("wave.riff"),
        b"RIFF\x08\x00\x00\x00junkWEBP\x00\x00\x00\x00",
    )
    .unwrap();
    let wave = image("wave.riff").expect("the riff payload is diffable");
    assert!(wave.is_binary);
    assert_eq!(wave.image, None);

    // A signature cut short is a header that matched nothing, not a partial match:
    // reading it is not a panic and not a format.
    fs::write(root.join("truncated.png"), b"\x89PNG\r\n\x00\x00\x00\x00").unwrap();
    let truncated = image("truncated.png").expect("the truncated picture is diffable");
    assert!(truncated.is_binary);
    assert_eq!(truncated.image, None);
}

#[test]
fn a_diff_is_the_merge_base_change_and_refuses_a_path_outside_the_checkout() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    write_and_commit(root, "file.txt", "one\n");
    git(root, &["checkout", "--quiet", "-b", "feature"]);
    write_and_commit(root, "file.txt", "one\ntwo\n");
    let checkout = &repo.checkouts[0];

    let diff = file_diff(&repo, checkout, "file.txt", GitReadBudget::default()).unwrap();
    assert!(diff.patch.contains("+two"));
    assert_eq!(diff.total_lines, diff.patch.lines().count());
    assert_eq!(
        diff.hunks,
        vec![DiffHunk {
            start_line: 1,
            end_line: 3
        }]
    );
    assert!(!diff.large && !diff.too_large && !diff.is_binary);
    // Two reads of an unmoved change are one change, which is what keeps a reader from
    // throwing away what it is showing because the file was read again.
    assert_eq!(
        diff.revision,
        file_diff(&repo, checkout, "file.txt", GitReadBudget::default())
            .unwrap()
            .revision
    );

    #[cfg(unix)]
    {
        // A diff of a link answers with the link, not with the paths of both of its ends.
        fs::write(root.join("target.txt"), "t\n").unwrap();
        std::os::unix::fs::symlink("target.txt", root.join("link")).unwrap();
        let link = file_diff(&repo, checkout, "link", GitReadBudget::default()).unwrap();
        assert_eq!(link.symlink_target.as_deref(), Some("target.txt"));
        assert!(link.patch.is_empty());
    }

    for refused in ["../escape", "/etc/passwd", ".git/config", "missing.txt", ""] {
        assert!(
            file_diff(&repo, checkout, refused, GitReadBudget::default()).is_err(),
            "{refused} was not refused"
        );
    }
}

#[cfg(unix)]
#[test]
fn tracked_and_untracked_diffs_survive_ancestor_replacement_without_reading_outside() {
    for tracked in [false, true] {
        let temp = tempdir().unwrap();
        let repo = main_repo(temp.path());
        let root = Path::new(&repo.root);
        fs::create_dir(root.join("dir")).unwrap();
        if tracked {
            write_and_commit(root, "dir/probe.txt", "before\n");
        }
        fs::write(root.join("dir/probe.txt"), "inside\n").unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("probe.txt"), "OUTSIDE_SECRET\n").unwrap();
        let diff = changes::file_diff_before_git(
            &repo,
            &repo.checkouts[0],
            "dir/probe.txt",
            GitReadBudget::default(),
            || {
                fs::rename(root.join("dir"), root.join("saved")).unwrap();
                std::os::unix::fs::symlink(&outside, root.join("dir")).unwrap();
            },
        )
        .unwrap();
        assert!(diff.patch.contains("+inside"), "{}", diff.patch);
        assert!(!diff.patch.contains("OUTSIDE_SECRET"), "{}", diff.patch);
        if tracked {
            assert!(diff.patch.contains("-before"));
        }
    }
}

#[test]
fn uncountable_untracked_entries_do_not_hide_the_change_list_or_claim_zero() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    fs::create_dir(root.join("nested")).unwrap();
    git(
        &root.join("nested"),
        &["init", "--quiet", "--initial-branch=main"],
    );
    fs::write(root.join("ordinary.txt"), "one\ntwo\n").unwrap();
    let large = File::create(root.join("large.txt")).unwrap();
    large.set_len((17 * 1024 * 1024) as u64).unwrap();
    let reported = status(&repo, &repo.checkouts[0], GitReadBudget::default()).unwrap();
    let find = |name: &str| reported.files.iter().find(|f| f.path == name).unwrap();
    assert_eq!(find("nested/").additions, None);
    assert_eq!(find("large.txt").additions, None);
    assert_eq!(find("ordinary.txt").additions, Some(2));
    // Aggregate counts no longer share the per-file diff preview cap.
    fs::remove_file(root.join("large.txt")).unwrap();
    assert_eq!(
        diff_totals_for_checkout(&repo, &repo.checkouts[0], GitReadBudget::default())
            .unwrap()
            .additions,
        2
    );
}

#[cfg(unix)]
#[test]
fn a_rename_a_mode_change_and_a_deleted_parent_survive_the_diff_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    write_and_commit(root, "before.txt", "one\ntwo\nthree\n");
    git(root, &["mv", "before.txt", "after.txt"]);
    fs::write(root.join("after.txt"), "one\ntwo\nTHREE\n").unwrap();
    let diff = file_diff(
        &repo,
        &repo.checkouts[0],
        "after.txt",
        GitReadBudget::default(),
    )
    .unwrap();
    assert!(
        diff.patch.contains("rename from before.txt"),
        "{}",
        diff.patch
    );
    assert!(diff.patch.contains("+THREE"), "{}", diff.patch);
    write_and_commit(root, "mode.txt", "mode\n");
    fs::set_permissions(root.join("mode.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    let diff = file_diff(
        &repo,
        &repo.checkouts[0],
        "mode.txt",
        GitReadBudget::default(),
    )
    .unwrap();
    assert!(diff.patch.contains("new mode 100755"), "{}", diff.patch);
    fs::create_dir(root.join("removed")).unwrap();
    write_and_commit(root, "removed/gone.txt", "gone\n");
    fs::remove_dir_all(root.join("removed")).unwrap();
    let diff = file_diff(
        &repo,
        &repo.checkouts[0],
        "removed/gone.txt",
        GitReadBudget::default(),
    )
    .unwrap();
    assert!(diff.patch.contains("-gone"), "{}", diff.patch);
}

#[test]
fn a_linked_worktree_diff_uses_its_own_snapshot_and_stable_untracked_identity() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    write_and_commit(root, "tracked.txt", "before\n");
    let worktree = temp.path().join("linked");
    git(
        root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            worktree.to_str().unwrap(),
        ],
    );
    fs::write(worktree.join("tracked.txt"), "after\n").unwrap();
    fs::write(worktree.join("untracked.txt"), "new\n").unwrap();
    let opened = open_directory(&worktree).unwrap();
    let checkout = opened
        .repo
        .checkouts
        .iter()
        .find(|c| c.id == opened.checkout_id)
        .unwrap();
    let tracked = file_diff(
        &opened.repo,
        checkout,
        "tracked.txt",
        GitReadBudget::default(),
    )
    .unwrap();
    assert!(
        tracked.patch.contains("-before") && tracked.patch.contains("+after"),
        "{}",
        tracked.patch
    );
    let fresh = file_diff(
        &opened.repo,
        checkout,
        "untracked.txt",
        GitReadBudget::default(),
    )
    .unwrap();
    let again = file_diff(
        &opened.repo,
        checkout,
        "untracked.txt",
        GitReadBudget::default(),
    )
    .unwrap();
    assert_eq!(fresh.patch, again.patch);
    assert_eq!(fresh.revision, again.revision);
    assert!(fresh.patch.contains("+new"), "{}", fresh.patch);
}

#[test]
fn a_plain_directory_and_a_missing_checkout_have_no_changes_to_report() {
    let temp = tempdir().unwrap();
    let plain = temp.path().join("plain");
    fs::create_dir(&plain).unwrap();
    let repo = Repo::plain(&plain, "now").unwrap();
    assert!(status(&repo, &repo.checkouts[0], GitReadBudget::default()).is_err());

    let repo = main_repo(temp.path());
    let mut missing = repo.checkouts[0].clone();
    missing.is_missing = true;
    assert!(status(&repo, &missing, GitReadBudget::default()).is_err());
    // A budget with nothing left in it is not a clean change set.
    assert!(status(
        &repo,
        &repo.checkouts[0],
        GitReadBudget::new(Duration::ZERO)
    )
    .unwrap_err()
    .contains("timed out"));
}

#[test]
fn an_untracked_and_a_deleted_file_are_both_diffable() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    let checkout = &repo.checkouts[0];
    fs::write(root.join("fresh.txt"), "one\ntwo\n").unwrap();
    write_and_commit(root, "gone.txt", "x\ny\n");
    fs::remove_file(root.join("gone.txt")).unwrap();

    let reported = status(&repo, checkout, GitReadBudget::default()).unwrap();
    let untracked = reported
        .files
        .iter()
        .find(|file| file.path == "fresh.txt")
        .expect("the untracked file is reported");
    assert_eq!(untracked.status, "??");
    // A worktree-only deletion is `.D` in porcelain v2 and `D` to a reader, which is the
    // code the Files tree knows how to draw.
    let deleted = reported
        .files
        .iter()
        .find(|file| file.path == "gone.txt")
        .expect("the deleted file is reported");
    assert_eq!(deleted.status, "D");

    // `git diff --no-index` answers 1 for a difference, so an exit code that is not
    // treated as that would leave every new file unreadable.
    let fresh = file_diff(&repo, checkout, "fresh.txt", GitReadBudget::default()).unwrap();
    assert!(fresh.patch.contains("+one"), "{}", fresh.patch);
    assert!(!fresh.is_binary && !fresh.too_large && fresh.symlink_target.is_none());

    // A deleted path has no entry to canonicalize; refusing it would leave the change Git
    // reports most often the one change that cannot be read.
    let gone = file_diff(&repo, checkout, "gone.txt", GitReadBudget::default()).unwrap();
    assert!(gone.patch.contains("-x"), "{}", gone.patch);
}

#[test]
fn a_changed_path_is_asked_for_by_name_and_never_as_a_pattern() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    let checkout = &repo.checkouts[0];
    // `a[1].txt` is a pattern for `a1.txt` as far as an unqualified pathspec is
    // concerned, and both are dirty here on purpose.
    write_and_commit(root, "a1.txt", "uno\n");
    write_and_commit(root, "a[1].txt", "one\n");
    fs::write(root.join("a[1].txt"), "ONE\n").unwrap();
    fs::write(root.join("a1.txt"), "UNO\n").unwrap();

    let diff = file_diff(&repo, checkout, "a[1].txt", GitReadBudget::default()).unwrap();
    assert!(diff.patch.contains("+ONE"), "{}", diff.patch);
    assert!(
        !diff.patch.contains("UNO"),
        "the diff of one file carried another file's lines:\n{}",
        diff.patch
    );
}

#[cfg(unix)]
#[test]
fn a_link_is_the_path_git_reports_and_containment_refuses_what_is_behind_one() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "SECRET\n").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    let checkout = &repo.checkouts[0];

    // Git reports the link itself and never a path through it, so the panel has no row
    // that could name the file behind it, and the one row it does have is a link with no
    // added lines to count.
    let reported = status(&repo, checkout, GitReadBudget::default()).unwrap();
    assert!(
        !reported
            .files
            .iter()
            .any(|file| file.path.contains("secret")),
        "a path through a link was reported: {:?}",
        reported.files
    );
    let link = reported
        .files
        .iter()
        .find(|file| file.path == "escape")
        .expect("the link is reported as the untracked path");
    assert!(link.additions.is_none(), "{:?}", link.additions);

    // Containment is checked on the nearest part of the path that exists, which is what
    // makes a deletion readable while still refusing a link used as a parent.
    assert!(contained_existing_ancestor(root, &root.join("escape/secret.txt")).is_err());
    assert!(contained_existing_ancestor(root, &root.join("fresh/new.txt")).is_ok());
    assert!(contained_existing_ancestor(root, Path::new("/etc/passwd")).is_err());
}

#[test]
fn a_large_change_remains_complete_for_a_virtualized_reader() {
    let temp = tempdir().unwrap();
    let repo = main_repo(temp.path());
    let root = Path::new(&repo.root);
    write_and_commit(root, "big.txt", "seed\n");
    git(root, &["checkout", "--quiet", "-b", "feature"]);
    let body = (0..1200)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    fs::write(root.join("big.txt"), body).unwrap();
    let checkout = &repo.checkouts[0];

    let diff = file_diff(&repo, checkout, "big.txt", GitReadBudget::default()).unwrap();
    assert!(diff.large && !diff.too_large);
    assert!(diff.patch.contains("+line 0") && diff.patch.contains("+line 1199"));

    let revised = scan(
        "big.txt".to_owned(),
        b"diff --git a/big.txt b/big.txt\n--- a/big.txt\n+++ b/big.txt\n@@ -1 +1 @@\n-old\n+new\n",
    )
    .unwrap();
    let same_length = scan(
        "big.txt".to_owned(),
        b"diff --git a/big.txt b/big.txt\n--- a/big.txt\n+++ b/big.txt\n@@ -1 +1 @@\n-old\n+NEW\n",
    )
    .unwrap();
    assert_eq!(revised.patch.len(), same_length.patch.len());
    assert_ne!(revised.revision, same_length.revision);
    assert!(revised.patch.contains("+new"));
    assert!(same_length.patch.contains("+NEW"));
    assert!(!diff.revision.is_empty());
}

#[test]
fn scan_refuses_hard_caps_without_partial_patch() {
    let oversized = vec![b'x'; 16 * 1024 * 1024 + 1];
    let diff = scan("large".into(), &oversized).unwrap();
    assert!(diff.too_large);
    assert!(diff.patch.is_empty());

    let mut too_many_hunks = Vec::new();
    for _ in 0..10_001 {
        too_many_hunks.extend_from_slice(b"@@ -1 +1 @@\n+x\n");
    }
    let diff = scan("hunks".into(), &too_many_hunks).unwrap();
    assert!(diff.too_large);
    assert!(diff.patch.is_empty());
}

#[test]
fn an_unreadable_hunk_header_is_refused_rather_than_placed_at_the_top() {
    // A header this cannot read is a patch whose coordinates are unknown. Guessing
    // them puts every line at 0,0, which folds and scrolls against numbers that are
    // not the file's: a correct-looking diff of the wrong lines.
    let error = scan("file.txt".to_owned(), b"@@ not a hunk header @@\n+one\n").unwrap_err();
    assert!(error.contains("hunk header"), "{error}");
}

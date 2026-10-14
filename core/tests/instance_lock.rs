#![cfg(unix)]

use muster_core::navigation::{load, save, NativeInstanceLock, NavigationState};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
};
use tempfile::tempdir;

fn child(directory: &Path, mode: &str) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_child", "--nocapture"])
        .env("MUSTER_LOCK_TEST_DIRECTORY", directory)
        .env("MUSTER_LOCK_TEST_MODE", mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn lock_child() {
    let Some(directory) = std::env::var_os("MUSTER_LOCK_TEST_DIRECTORY") else {
        return;
    };
    let directory = Path::new(&directory);
    let Ok(_lock) = NativeInstanceLock::acquire_in(directory) else {
        std::process::exit(23)
    };
    match std::env::var("MUSTER_LOCK_TEST_MODE").unwrap().as_str() {
        "hold" => {
            println!("LOCKED");
            std::io::stdout().flush().unwrap();
            let mut byte = [0];
            std::io::stdin().read_exact(&mut byte).unwrap();
            // Final shutdown save must still exclude a competing process.
            save(
                &directory.join("native-workspace.json"),
                &NavigationState::default(),
            )
            .unwrap();
        }
        "startup-failure" => {
            assert!(load(&directory.join("native-workspace.json")).is_err());
            std::process::exit(24);
        }
        "probe" => {}
        _ => panic!("unknown child mode"),
    }
}

fn held(directory: &Path) -> Child {
    let mut process = child(directory, "hold");
    let mut reader = BufReader::new(process.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "lock child exited without acquiring"
        );
        if line.trim() == "LOCKED" {
            break;
        }
        line.clear();
    }
    process.stdout = Some(reader.into_inner());
    process
}

#[test]
fn concurrent_processes_are_excluded_until_shutdown_saves_finish() {
    let temp = tempdir().unwrap();
    let directory = temp.path().join(".muster");
    let mut first = held(&directory);
    assert_eq!(child(&directory, "probe").wait().unwrap().code(), Some(23));
    first.stdin.take().unwrap().write_all(b"x").unwrap();
    assert!(first.wait().unwrap().success());
    assert_eq!(
        load(&directory.join("native-workspace.json")).unwrap(),
        NavigationState::default()
    );
    assert!(child(&directory, "probe").wait().unwrap().success());
}

#[test]
fn abrupt_exit_and_startup_failure_release_without_deleting_lockfile() {
    let temp = tempdir().unwrap();
    let directory = temp.path().join(".muster");
    let mut first = held(&directory);
    first.kill().unwrap();
    first.wait().unwrap();
    assert!(directory.join("native-instance.lock").is_file());
    assert!(child(&directory, "probe").wait().unwrap().success());
    fs::write(directory.join("native-workspace.json"), b"corrupt").unwrap();
    assert_eq!(
        child(&directory, "startup-failure").wait().unwrap().code(),
        Some(24)
    );
    assert!(child(&directory, "probe").wait().unwrap().success());
}

#[test]
fn unsafe_directories_and_lockfiles_fail_closed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = tempdir().unwrap();
    let directory = temp.path().join(".muster");
    let guard = NativeInstanceLock::acquire_in(&directory).unwrap();
    drop(guard);
    let lock = directory.join("native-instance.lock");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(NativeInstanceLock::acquire_in(&directory).is_err());
    fs::remove_file(&lock).unwrap();
    let target = temp.path().join("target");
    fs::write(&target, b"untouched").unwrap();
    symlink(&target, &lock).unwrap();
    assert!(NativeInstanceLock::acquire_in(&directory).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"untouched");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    assert!(NativeInstanceLock::acquire_in(&directory).is_err());
    fs::remove_dir(&lock).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&target, &lock).unwrap();
    assert!(NativeInstanceLock::acquire_in(&directory).is_err());
    fs::remove_file(&lock).unwrap();
    assert!(Command::new("mkfifo")
        .arg(&lock)
        .status()
        .unwrap()
        .success());
    assert!(NativeInstanceLock::acquire_in(&directory).is_err());
    fs::remove_file(&lock).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(NativeInstanceLock::acquire_in(&directory).is_err());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let alias = temp.path().join("alias");
    symlink(&directory, &alias).unwrap();
    assert!(NativeInstanceLock::acquire_in(&alias).is_err());
}

use super::*;

/// OS-held lock, released by drop or process death. Never unlink the lockfile:
/// replacing its inode would let a second process acquire a different lock.
#[derive(Debug)]
pub struct NativeInstanceLock {
    _file: File,
}

impl NativeInstanceLock {
    /// Acquire before reading workspace state or creating a window; retain until
    /// terminal shutdown and the final state writer flush have completed.
    pub fn acquire() -> Result<Self, String> {
        let state = state_path()?;
        Self::acquire_in(state.parent().ok_or("native workspace has no directory")?)
    }

    #[cfg(unix)]
    pub fn acquire_in(directory: &Path) -> Result<Self, String> {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        };
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "could not create private native workspace directory: {error}"
                ))
            }
        }
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory)
            .map_err(|error| {
                format!("could not safely open native workspace directory: {error}")
            })?;
        let meta = dir.metadata().map_err(|error| error.to_string())?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(
                "native workspace directory must be private and owned by the current user".into(),
            );
        }
        // Resolve relative to the validated directory descriptor, not a path
        // whose last directory component could be exchanged between checks.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                c"native-instance.lock".as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(format!(
                "could not safely open native instance lock: {}",
                std::io::Error::last_os_error()
            ));
        }
        // openat returned a fresh owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        let meta = file.metadata().map_err(|error| error.to_string())?;
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
        {
            return Err(
                "native instance lock must be a private, owned regular file with one link".into(),
            );
        }
        file.try_lock().map_err(|error| {
            format!(
                "could not acquire native instance lock (another instance may be running): {error}"
            )
        })?;
        Ok(Self { _file: file })
    }

    #[cfg(not(unix))]
    pub fn acquire_in(_directory: &Path) -> Result<Self, String> {
        Err("native instance locking is supported on macOS and Linux only".into())
    }
}

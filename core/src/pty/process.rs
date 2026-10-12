//! The name of the program sitting in front of a terminal, read from the OS.
//!
//! Muster only ever launches a login shell, so every other program in a terminal is one the
//! user typed: `opencode`, `nvim`, `vim`. The PTY knows which process group holds the
//! foreground, and the OS knows what that group is running, so the two together name the
//! program without reading a single byte of terminal output.
//!
//! The same two questions are asked about the shell behind it: where it is. That is a
//! `chdir` away from telling which worktree a terminal is working in without asking the shell
//! to report it, which would mean asking every shell to be configured a particular way.

use std::path::Path;

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::path::PathBuf;

    use super::file_name;

    extern "C" {
        /// libproc, part of libSystem: the full path of the executable behind `pid`.
        ///
        /// Returns the bytes written, not counting the terminator, or 0 when the process is
        /// gone or owned by someone else.
        fn proc_pidpath(
            pid: libc::c_int,
            buffer: *mut c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }

    /// `MAXPATHLEN`, which libproc does not publish a constant for.
    pub(super) const MAXPATHLEN: usize = 1024;

    /// `struct vinfo_stat`, the node fields a path answer carries before the path itself.
    ///
    /// Declared here because the `libc` crate publishes `PROC_PIDVNODEPATHINFO` but not the structs
    /// it fills, and reading the path without them means reading it at an offset nobody wrote down.
    /// `repr(C)` and the field order are the C declaration in `<sys/proc_info.h>`; the test below
    /// pins the sizes C gives these, which is the only thing here that would break silently.
    #[repr(C)]
    pub(super) struct VinfoStat {
        vst_dev: u32,
        vst_mode: u16,
        vst_nlink: u16,
        vst_ino: u64,
        vst_uid: u32,
        vst_gid: u32,
        vst_atime: i64,
        vst_atimensec: i64,
        vst_mtime: i64,
        vst_mtimensec: i64,
        vst_ctime: i64,
        vst_ctimensec: i64,
        vst_birthtime: i64,
        vst_birthtimensec: i64,
        vst_size: i64,
        vst_blocks: i64,
        vst_blksize: i32,
        vst_flags: u32,
        vst_gen: u32,
        vst_rdev: u32,
        vst_qspare: [i64; 2],
    }

    /// `struct vnode_info`: the node, its type, and the filesystem it is on.
    #[repr(C)]
    pub(super) struct VnodeInfo {
        vi_stat: VinfoStat,
        vi_type: i32,
        vi_pad: i32,
        vi_fsid: [i32; 2],
    }

    /// `struct vnode_info_path`: a node and the path it was reached by.
    #[repr(C)]
    pub(super) struct VnodeInfoPath {
        vip_vi: VnodeInfo,
        vip_path: [libc::c_char; MAXPATHLEN],
    }

    /// `struct proc_vnodepathinfo`: the directory a process is in, and the root it started under.
    ///
    /// Only the first is asked for. The second is the process's root directory, which on macOS is
    /// always `/` and never a worktree, so reading the wrong one of the two would answer every
    /// terminal with the filesystem root.
    #[repr(C)]
    pub(super) struct VnodePathInfo {
        pvi_cdir: VnodeInfoPath,
        pvi_rdir: VnodeInfoPath,
    }

    pub fn executable_name(pid: u32) -> Option<String> {
        let mut buffer = [0_u8; 4096];
        // SAFETY: `buffer` is a live allocation of exactly the length handed over, and
        // libproc writes at most that many bytes into it. A pid we did not ask about, or one
        // that exited, is reported as a failure rather than read.
        let written = unsafe {
            proc_pidpath(
                pid as libc::c_int,
                buffer.as_mut_ptr().cast(),
                buffer.len() as libc::c_int,
            )
        };
        if written <= 0 {
            return None;
        }
        let path = std::str::from_utf8(&buffer[..usize::try_from(written).ok()?.min(buffer.len())])
            .ok()?;
        file_name(path)
    }
    pub fn wait_status_pid(info: &libc::siginfo_t) -> libc::pid_t {
        info.si_pid
    }

    /// The directory `pid` is sitting in, as the OS records it.
    ///
    /// `PROC_PIDVNODEPATHINFO` is the one libproc call that answers this without walking the file
    /// tree or spawning `lsof`, so it is cheap enough to ask on every status poll.
    pub fn current_directory(pid: u32) -> Option<PathBuf> {
        let mut info: VnodePathInfo = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live allocation of exactly the size handed over, and libproc fills
        // at most that many bytes into it. A pid we did not ask about, or one that exited, is
        // reported as a failure rather than read.
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&mut info as *mut VnodePathInfo).cast(),
                std::mem::size_of::<VnodePathInfo>() as libc::c_int,
            )
        };
        if written as usize != std::mem::size_of::<VnodePathInfo>() {
            return None;
        }
        let path = info
            .pvi_cdir
            .vip_path
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(MAXPATHLEN);
        // SAFETY: `c_char` is a byte here, and libproc wrote a NUL-terminated path into the
        // buffer it was handed, so every byte up to the terminator is a byte of that path.
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(info.pvi_cdir.vip_path.as_ptr().cast(), path) };
        path_from_bytes(bytes)
    }

    pub(super) fn path_from_bytes(bytes: &[u8]) -> Option<PathBuf> {
        let path = std::str::from_utf8(bytes).ok()?;
        (!path.is_empty()).then(|| PathBuf::from(path))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    use super::file_name;

    pub fn executable_name(pid: u32) -> Option<String> {
        // The kernel publishes the same name libproc would, truncated to 15 bytes.
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        file_name(comm.trim())
    }
    pub fn wait_status_pid(info: &libc::siginfo_t) -> libc::pid_t {
        // SAFETY: the caller owns the structure `waitid` reported into, and reading the pid out of
        // it is what libc exposes the accessor for.
        unsafe { info.si_pid() }
    }

    pub fn current_directory(pid: u32) -> Option<PathBuf> {
        // The kernel publishes the same directory libproc would, as the link the cwd entry points
        // at. Reading it is what `readlink /proc/<pid>/cwd` does, without spawning one.
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use std::path::PathBuf;

    pub fn executable_name(_pid: u32) -> Option<String> {
        None
    }

    /// No platform to read it from, so this reports the same thing as no report at all.
    pub fn wait_status_pid(_info: &libc::siginfo_t) -> libc::pid_t {
        0
    }

    pub fn current_directory(_pid: u32) -> Option<PathBuf> {
        None
    }
}

/// The last path component, which is what a row in the sidebar has room for.
fn file_name(path: &str) -> Option<String> {
    let name = Path::new(path).file_name()?.to_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The process a `waitid` report is about, on whichever way this platform spells it.
///
/// `si_pid` is a field on macOS and a method on Linux, so the same reading is a field access on one
/// and a call on the other, and code written the way one platform spells it does not compile on the
/// other. Both callers are deciding whether a report arrived at all, which is the one reading a
/// build that cannot compile takes the whole Linux build with it.
pub fn wait_status_pid(info: &libc::siginfo_t) -> libc::pid_t {
    imp::wait_status_pid(info)
}

pub use imp::{current_directory, executable_name};

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn only_the_last_component_of_a_path_is_a_program_name() {
        assert_eq!(
            file_name("/Users/matiasmarani/.opencode/bin/opencode").as_deref(),
            Some("opencode")
        );
        assert_eq!(file_name("/bin/zsh").as_deref(), Some("zsh"));
        // A kernel that reports a bare name, and a path with nothing left to name.
        assert_eq!(file_name("vim").as_deref(), Some("vim"));
        assert_eq!(file_name("/"), None);
        assert_eq!(file_name(""), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn names_this_process_and_nobody_at_all() {
        // The strongest claim available without spawning anything: we can name ourselves.
        // Cargo suffixes the test binary, so the stem is the fixed part.
        let named = super::executable_name(std::process::id()).expect("this process has a name");
        assert!(named.starts_with("muster"), "{named}");
        // u32::MAX is never a live pid, so the OS reports a failure and we say nothing.
        assert_eq!(super::executable_name(u32::MAX), None);
    }

    /// The directory this process is in is the one a shell in it would report, which is the whole
    /// claim: no shell has to be configured to be found out about.
    #[test]
    fn reads_the_directory_of_a_process_and_nothing_at_all() {
        let here =
            super::current_directory(std::process::id()).expect("this process has a directory");
        let actual = std::env::current_dir().expect("this process has a directory");
        // macOS reports `/private/var/...` for what the shell wrote as `/var/...`, so the two are
        // the same directory reached by different paths.
        assert_eq!(
            here.canonicalize().unwrap_or(here.clone()),
            actual.canonicalize().unwrap_or(actual)
        );
        assert_eq!(super::current_directory(u32::MAX), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn preserves_trailing_space_in_a_directory_path() {
        let path = std::env::temp_dir().join(format!("muster-cwd-{} ", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temporary directory");
        let decoded = super::imp::path_from_bytes(path.to_str().unwrap().as_bytes());
        std::fs::remove_dir(&path).expect("remove temporary directory");
        assert_eq!(decoded.as_deref(), Some(path.as_path()));
        assert_eq!(super::imp::path_from_bytes(b""), None);
        assert_eq!(super::imp::path_from_bytes(&[0xff]), None);
    }

    /// The one thing that would break silently if this machine's C headers ever disagreed with us:
    /// the path is not at offset zero, and a wrong prefix reads a plausible string out of the
    /// node's own fields. These are the sizes C reports for them on this platform.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_vnode_info_layout_matches_the_c_declaration() {
        assert_eq!(std::mem::size_of::<super::imp::VinfoStat>(), 136);
        assert_eq!(std::mem::size_of::<super::imp::VnodeInfo>(), 152);
        assert_eq!(
            std::mem::size_of::<super::imp::VnodeInfoPath>(),
            152 + super::imp::MAXPATHLEN
        );
        assert_eq!(
            std::mem::size_of::<super::imp::VnodePathInfo>(),
            2 * (152 + super::imp::MAXPATHLEN)
        );
    }
}

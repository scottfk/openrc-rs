//! The service state directory (`/run/openrc`): `rc_svcdir()` and
//! `rc_dirfd()` from `librc.c`.

use std::os::fd::BorrowedFd;
use std::sync::Mutex;

use openrc_compat::bcat;
use openrc_compat::env;
use openrc_compat::sys::{self, libc};

use crate::dirs;
use crate::paths::SVCDIR;

/// `enum rc_dir`, with C's values: the service state directory and its
/// subdirectories, then the configuration directories.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RcDir {
    Svcdir = 0,
    Starting,
    Started,
    Stopping,
    Inactive,
    WasInactive,
    Failed,
    Hotplugged,
    Daemons,
    Options,
    Exclusive,
    Scheduled,
    Environ,
    Initd,
    Tmp,
    /// `RC_DIR_SYSCONF` (= `RC_DIR_SYS_MAX`).
    Sysconf,
    Usrconf,
    Runlevel,
}

/// `RC_DIR_MAX`.
const RC_DIR_MAX: usize = 18;

impl RcDir {
    pub const ALL: [RcDir; RC_DIR_MAX] = [
        RcDir::Svcdir,
        RcDir::Starting,
        RcDir::Started,
        RcDir::Stopping,
        RcDir::Inactive,
        RcDir::WasInactive,
        RcDir::Failed,
        RcDir::Hotplugged,
        RcDir::Daemons,
        RcDir::Options,
        RcDir::Exclusive,
        RcDir::Scheduled,
        RcDir::Environ,
        RcDir::Initd,
        RcDir::Tmp,
        RcDir::Sysconf,
        RcDir::Usrconf,
        RcDir::Runlevel,
    ];

    /// From the C enum value.
    pub fn from_raw(v: i32) -> Option<RcDir> {
        usize::try_from(v).ok().and_then(|i| RcDir::ALL.get(i).copied())
    }

    /// Name under the svcdir (`dirnames[]`); empty for the others.
    pub fn name(self) -> &'static str {
        match self {
            RcDir::Starting => "starting",
            RcDir::Started => "started",
            RcDir::Stopping => "stopping",
            RcDir::Inactive => "inactive",
            RcDir::WasInactive => "wasinactive",
            RcDir::Failed => "failed",
            RcDir::Hotplugged => "hotplugged",
            RcDir::Daemons => "daemons",
            RcDir::Options => "options",
            RcDir::Exclusive => "exclusive",
            RcDir::Scheduled => "scheduled",
            RcDir::Initd => "init.d",
            RcDir::Tmp => "tmp",
            RcDir::Environ => "environ",
            RcDir::Svcdir | RcDir::Sysconf | RcDir::Usrconf | RcDir::Runlevel => "",
        }
    }
}

/// `rc_svcdir()`: the user-mode svcdir, else `$RC_SVCDIR` (read once),
/// else `/run/openrc`.
pub fn rc_svcdir() -> Vec<u8> {
    static CACHED: Mutex<Option<Vec<u8>>> = Mutex::new(None);
    if let Some(user) = dirs::user_svcdir() {
        return user;
    }
    let mut c = CACHED.lock().unwrap_or_else(|e| e.into_inner());
    c.get_or_insert_with(|| env::get("RC_SVCDIR").unwrap_or_else(|| SVCDIR.as_bytes().to_vec())).clone()
}

/// The C `static int dirfds[RC_DIR_MAX]`: zero-initialised, -1 after
/// `clear_dirfds()`, and only values > 0 count as cached.  Fds are kept
/// open until cleared.
static DIRFDS: Mutex<[i32; RC_DIR_MAX]> = Mutex::new([0; RC_DIR_MAX]);

fn open_dir(path: &[u8]) -> i32 {
    match sys::openat(None, path, libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY, 0) {
        Ok(fd) => std::os::fd::IntoRawFd::into_raw_fd(fd),
        Err(_) => -1,
    }
}

/// `rc_dirfd()` as the C ABI sees it: an fd for `dir` (creating svcdir and
/// its subdirectories, mode 0755, if missing), or -1.
pub fn rc_dirfd_raw(dir: RcDir) -> i32 {
    let i = dir as usize;
    {
        let fds = DIRFDS.lock().unwrap_or_else(|e| e.into_inner());
        if fds[i] > 0 {
            return fds[i];
        }
    }
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY;
    let fd = match dir {
        RcDir::Sysconf => open_dir(dirs::rc_sysconfdir().as_bytes()),
        // C passes a NULL path to open() when not in user mode: EFAULT.
        RcDir::Usrconf => dirs::rc_usrconfdir().map_or(-1, |d| open_dir(&d)),
        RcDir::Runlevel => open_dir(&dirs::rc_runleveldir()),
        RcDir::Svcdir => {
            let svcdir = rc_svcdir();
            match sys::mkdir(&svcdir, 0o755) {
                Err(e) if e.0 != libc::EEXIST => return -1,
                _ => open_dir(&svcdir),
            }
        }
        _ => {
            let parent = rc_dirfd_raw(RcDir::Svcdir);
            if parent < 0 {
                // C carries on with fd -1: openat fails with EBADF.
                return -1;
            }
            // SAFETY: parent is an open fd held by the cache.
            let parent = unsafe { BorrowedFd::borrow_raw(parent) };
            let name = dir.name().as_bytes();
            let mut r = sys::openat(Some(parent), name, flags, 0);
            if let Err(e) = &r
                && e.0 == libc::ENOENT
            {
                match sys::mkdirat(Some(parent), name, 0o755) {
                    Err(e) if e.0 != libc::EEXIST => return -1,
                    _ => r = sys::openat(Some(parent), name, flags, 0),
                }
            }
            r.map_or(-1, std::os::fd::IntoRawFd::into_raw_fd)
        }
    };
    DIRFDS.lock().unwrap_or_else(|e| e.into_inner())[i] = fd;
    fd
}

/// `rc_dirfd()`: a borrowed fd for `dir`, valid until `clear_dirfds()`.
pub fn rc_dirfd(dir: RcDir) -> Option<BorrowedFd<'static>> {
    let fd = rc_dirfd_raw(dir);
    // SAFETY: the cache owns the fd until clear_dirfds() closes it.
    (fd >= 0).then(|| unsafe { BorrowedFd::borrow_raw(fd) })
}

/// `clear_dirfds()`: close every cached directory fd.
pub fn clear_dirfds() {
    let mut fds = DIRFDS.lock().unwrap_or_else(|e| e.into_inner());
    for fd in fds.iter_mut() {
        // Fds <= 2 are never cached (zero-initialised entries).
        if *fd > 0 {
            // SAFETY: the cache owns this fd and drops it exactly once.
            drop(unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(*fd) });
        }
        *fd = -1;
    }
}

/// `$svcdir/<sub>/<rest>` as a path.
pub fn svc_path(sub: &str, rest: &[u8]) -> Vec<u8> {
    bcat!(rc_svcdir(), "/", sub, "/", rest)
}

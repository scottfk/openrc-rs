//! The service state directory (`/run/openrc`): `rc_svcdir()` and
//! `rc_dirfd()` from `librc.c`.

use std::os::fd::BorrowedFd;
use std::sync::Mutex;

use openrc_compat::bcat;
use openrc_compat::env;
use openrc_compat::sys::{self, libc};

use crate::dirs;
use crate::paths::SVCDIR;

/// Subdirectories of the service state directory (`enum rc_dir`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RcDir {
    Svcdir,
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
    Initd,
    Tmp,
    Environ,
}

impl RcDir {
    /// Name under the svcdir (`dirnames[]`).
    pub fn name(self) -> &'static str {
        match self {
            RcDir::Svcdir => "",
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

/// Opened directory fds, kept for the life of the process like C's
/// `static int dirfds[]`.  Failures are not cached (retried next call).
static DIRFDS: Mutex<Vec<(RcDir, BorrowedFd<'static>)>> = Mutex::new(Vec::new());

/// `rc_dirfd()`: an fd for `dir`, creating it (mode 0755, not recursively)
/// if missing.
pub fn rc_dirfd(dir: RcDir) -> Option<BorrowedFd<'static>> {
    if let Some(&(_, fd)) = DIRFDS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(d, _)| *d == dir) {
        return Some(fd);
    }
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY;
    let fd = if dir == RcDir::Svcdir {
        let svcdir = rc_svcdir();
        match sys::mkdir(&svcdir, 0o755) {
            Err(e) if e.0 != libc::EEXIST => return None,
            _ => {}
        }
        sys::openat(None, &svcdir, flags, 0).ok()?
    } else {
        // C carries on with fd -1, so every *at() call fails with EBADF;
        // None here would mean AT_FDCWD, so stop instead.
        let parent = Some(rc_dirfd(RcDir::Svcdir)?);
        let name = dir.name().as_bytes();
        match sys::openat(parent, name, flags, 0) {
            Ok(fd) => fd,
            Err(e) if e.0 == libc::ENOENT => {
                match sys::mkdirat(parent, name, 0o755) {
                    Err(e) if e.0 != libc::EEXIST => return None,
                    _ => {}
                }
                sys::openat(parent, name, flags, 0).ok()?
            }
            Err(_) => return None,
        }
    };
    let fd = sys::leak_fd(fd);
    DIRFDS.lock().unwrap_or_else(|e| e.into_inner()).push((dir, fd));
    Some(fd)
}

/// `$svcdir/<sub>/<rest>` as a path.
pub fn svc_path(sub: &str, rest: &[u8]) -> Vec<u8> {
    bcat!(rc_svcdir(), "/", sub, "/", rest)
}

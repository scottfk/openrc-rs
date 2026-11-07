//! `rc_newer_than()` / `rc_older_than()` (`librc-depend.c`): recursive
//! mtime comparison, used by the deptree cache and `is_newer_than`.

use std::os::fd::AsRawFd;

use openrc_compat::sys::{self, libc};

/// `deep_mtime_check()`.  Like the shell `test -nt`, a missing target
/// counts as passing.  `rel` is updated to the newest (oldest) mtime seen
/// when the check fails, and `file` to the file that had it (the name
/// relative to its directory, as C's strlcpy of `target`).  `dir` is a raw
/// fd as C passes it: `AT_FDCWD`, or -1 (EBADF) for an unopenable dirfd.
pub(crate) fn deep_check(dir: i32, target: &[u8], newer: bool, rel: &mut i64, file: &mut Option<Vec<u8>>) -> bool {
    let Ok(st) = sys::at::fstatat(dir, target, 0) else { return true };
    let mut retval = true;
    if (newer && *rel < st.mtime) || (!newer && *rel > st.mtime) {
        retval = false;
        *file = Some(target.to_vec());
        *rel = st.mtime;
    }
    if !st.is_dir() {
        return retval;
    }
    // do_opendirat(): openat(O_RDONLY|O_DIRECTORY), following symlinks.
    let Ok(fd) = sys::at::openat(dir, target, libc::O_RDONLY | libc::O_DIRECTORY, 0) else { return retval };
    let Ok(names) = sys::at::readdir_dup(fd.as_raw_fd()) else { return retval };
    for name in names {
        if name.first() == Some(&b'.') {
            continue;
        }
        if !deep_check(fd.as_raw_fd(), &name, newer, rel, file) {
            retval = false;
        }
    }
    retval
}

/// Result of an mtime check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MtimeCheck {
    pub ok: bool,
    /// The most different mtime found (`*newest` / `*oldest`).
    pub mtime: Option<i64>,
    pub file: Option<Vec<u8>>,
}

fn mtime_check(source: &[u8], target: &[u8], newer: bool) -> MtimeCheck {
    // We have to exist.
    let Ok(st) = sys::stat(source) else { return MtimeCheck { ok: false, mtime: None, file: None } };
    let mut rel = st.mtime;
    let mut file = None;
    let ok = deep_check(libc::AT_FDCWD, target, newer, &mut rel, &mut file);
    MtimeCheck { ok, mtime: Some(rel), file }
}

/// `rc_newer_than(source, target)`: is nothing in `target` (recursively)
/// newer than `source`?  False if `source` does not exist.
pub fn rc_newer_than(source: &[u8], target: &[u8]) -> MtimeCheck {
    mtime_check(source, target, true)
}

/// `rc_older_than(source, target)`: is nothing in `target` (recursively)
/// older than `source`?
pub fn rc_older_than(source: &[u8], target: &[u8]) -> MtimeCheck {
    mtime_check(source, target, false)
}

//! Per-service options and saved environment (`librc.c`:
//! `rc_service_value_get/set`, `rc_service_getenv/setenv`).
//!
//! Options live in `$svcdir/options/<service>/<name>`, re-exported
//! environment in `$svcdir/environ/<service>/<name>`.

use std::os::fd::AsFd;

use openrc_compat::bcat;
use openrc_compat::sys::{self, libc};

use crate::misc::rc_getfile;
use crate::svcdir::{RcDir, rc_dirfd, svc_path};

/// `do_fopenat(dirfd, path, O_WRONLY|O_CREAT|O_TRUNC)` + write: the file
/// is created 0666 (minus umask).  Returns false if it cannot be opened.
fn write_at(dir: std::os::fd::BorrowedFd<'_>, path: &[u8], value: &[u8]) -> bool {
    match sys::openat(Some(dir), path, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o666) {
        Ok(fd) => {
            // stdio errors are not checked by the C code (fprintf/fclose).
            sys::write_all_fd(fd.as_fd(), value);
            true
        }
        Err(_) => false,
    }
}

/// `rc_service_value_get()`: the contents of an option file, as a C
/// string would see them (up to the first NUL).
pub fn rc_service_value_get(service: &[u8], option: &[u8]) -> Option<Vec<u8>> {
    let file = svc_path("options", &bcat!(service, "/", option));
    rc_getfile(&file).map(|b| openrc_compat::bytes::c_str_prefix(&b).to_vec())
}

/// `rc_service_value_set()`: write an option; `None` removes it.
pub fn rc_service_value_set(service: &[u8], option: &[u8], value: Option<&[u8]>) -> bool {
    // With no options dir C's mkdirat(-1, ...) fails (EBADF).
    let Some(opts) = rc_dirfd(RcDir::Options) else { return false };
    let opts = Some(opts);
    if let Err(e) = sys::mkdirat(opts, service, 0o755)
        && e.0 != libc::EEXIST
    {
        return false;
    }
    let Ok(optdir) = sys::openat(opts, service, libc::O_RDONLY | libc::O_DIRECTORY, 0) else { return false };
    match value {
        None => {
            let _ = sys::unlinkat(Some(optdir.as_fd()), option, 0);
            true
        }
        Some(v) => write_at(optdir.as_fd(), option, openrc_compat::bytes::c_str_prefix(v)),
    }
}

/// `rc_service_setenv()`: save `name=value` for re-export to `service`.
pub fn rc_service_setenv(service: &[u8], name: &[u8], value: &[u8]) -> bool {
    let Some(dfd) = rc_dirfd(RcDir::Environ) else {
        // mkdirat(-1, ...) fails with EBADF in C.
        return false;
    };
    if let Err(e) = sys::mkdirat(Some(dfd), service, 0o700)
        && e.0 != libc::EEXIST
    {
        return false;
    }
    write_at(dfd, &bcat!(service, "/", name), openrc_compat::bytes::c_str_prefix(value))
}

/// `rc_service_getenv(service, name, NULL)`: the size of a saved
/// variable, or -1 if there is none.
pub fn rc_service_getenv_size(service: &[u8], name: &[u8]) -> i64 {
    let Some(dfd) = rc_dirfd(RcDir::Environ) else { return -1 };
    match sys::fstatat(Some(dfd), &bcat!(service, "/", name), 0) {
        Ok(st) => st.size,
        Err(_) => -1,
    }
}

//! Process environment access.  OpenRC uses the environment as state
//! (`EINFO_INDENT`, `EINFO_LASTCMD`, ...), and C programs linked against our
//! C ABI libraries must see the same `environ`, so this goes through libc's
//! `getenv`/`setenv` (which is what `std::env` uses underneath).

use std::ffi::OsStr;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// `getenv(name)`.
pub fn get(name: &str) -> Option<Vec<u8>> {
    std::env::var_os(name).map(OsStringExt::into_vec)
}

/// `getenv(name)` for an arbitrary byte-string name, exactly as libc does
/// it (std refuses names containing `=`, glibc happily matches them).
pub fn get_bytes(name: &[u8]) -> Option<Vec<u8>> {
    let name = crate::bytes::to_cstring(name);
    // SAFETY: name is a valid C string; the result is copied out at once
    // (single-threaded, nothing modifies the environment meanwhile).
    unsafe {
        let p = libc::getenv(name.as_ptr());
        (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p).to_bytes().to_vec())
    }
}

/// `setenv(name, value, 1)`.
pub fn set(name: &str, value: &[u8]) {
    // SAFETY: OpenRC programs are single-threaded, as are the C programs this
    // mirrors; nothing else is reading the environment concurrently.
    unsafe { std::env::set_var(name, OsStr::from_bytes(value)) }
}

/// `unsetenv(name)`.
pub fn unset(name: &str) {
    // SAFETY: see `set`.
    unsafe { std::env::remove_var(name) }
}

/// The raw `environ` entries, in order (normally `NAME=value`, but any
/// bytes a parent passed are kept as they are).
pub fn environ() -> Vec<Vec<u8>> {
    unsafe extern "C" {
        static environ: *const *const libc::c_char;
    }
    let mut out = Vec::new();
    // SAFETY: environ is a NULL-terminated array of C strings; single
    // threaded, so nothing changes it while we copy.
    unsafe {
        let mut p = environ;
        while !p.is_null() && !(*p).is_null() {
            out.push(std::ffi::CStr::from_ptr(*p).to_bytes().to_vec());
            p = p.add(1);
        }
    }
    out
}

/// `setenv(name, value, 1)` for any byte-string name (glibc rejects an
/// empty name or one containing `=` with EINVAL; so does this).
pub fn set_bytes(name: &[u8], value: &[u8]) -> bool {
    let (n, v) = (crate::bytes::to_cstring(name), crate::bytes::to_cstring(value));
    // SAFETY: valid C strings; see `set`.
    unsafe { libc::setenv(n.as_ptr(), v.as_ptr(), 1) == 0 }
}

/// `unsetenv(name)` for any byte-string name (EINVAL as glibc for an
/// empty name or one containing `=`).
pub fn unset_bytes(name: &[u8]) -> bool {
    let n = crate::bytes::to_cstring(name);
    // SAFETY: valid C string; see `set`.
    unsafe { libc::unsetenv(n.as_ptr()) == 0 }
}

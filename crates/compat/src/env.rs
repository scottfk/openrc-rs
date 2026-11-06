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

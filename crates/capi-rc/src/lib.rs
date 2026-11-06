//! C ABI for [`rc`]: the Rust half of a drop-in `librc.so.1`.
//!
//! `librc.so.1` is a hybrid while Phase 2 is in progress.  The functions
//! listed in `rust-owned.txt` are defined here; the rest still come from
//! upstream's C (`upstream/`), compiled with these symbols made weak so
//! that the Rust definitions win, including for calls from inside the C.
//! Each Phase 2 step moves more functions here.
//!
//! Memory handed to C callers is allocated with libc `malloc`, and lists
//! are built with the library's own `rc_stringlist_*`, so callers can
//! free and walk them as usual.  Strings that C returns from static or
//! cached storage are leaked `CStr`s here, which live for the process.

#![allow(clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Mutex;

use openrc_compat::bytes::to_cstring;
use rc::svcdir::RcDir;

unsafe extern "C" {
    fn rc_stringlist_new() -> *mut c_void;
    fn rc_stringlist_add(list: *mut c_void, value: *const c_char) -> *mut c_void;
}

fn set_errno(e: i32) {
    // SAFETY: errno is thread-local and always writable.
    unsafe { *libc::__errno_location() = e }
}

/// # Safety
/// `p` must be null or a valid C string that outlives `'a`.
unsafe fn arg<'a>(p: *const c_char) -> Option<&'a [u8]> {
    // SAFETY: guaranteed by the caller.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_bytes())
}

/// Copy `b` into a fresh `malloc`'d C string (C's `xstrdup`).
fn malloc_bytes(b: &[u8]) -> *mut c_char {
    // SAFETY: we allocate len+1 bytes and fill them all.
    unsafe {
        let p = libc::malloc(b.len() + 1).cast::<u8>();
        if p.is_null() {
            libc::abort();
        }
        std::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len());
        *p.add(b.len()) = 0;
        p.cast()
    }
}

/// An `RC_STRINGLIST *` built with the library's own list functions.
fn stringlist(items: &[Vec<u8>]) -> *mut c_void {
    // SAFETY: rc_stringlist_new/add are the C functions linked into this
    // library; add copies its argument.
    unsafe {
        let list = rc_stringlist_new();
        for item in items {
            let c = to_cstring(item);
            rc_stringlist_add(list, c.as_ptr());
        }
        list
    }
}

/// A process-lifetime C string for `value`, reused for equal values (for
/// functions that return `const char *` into static storage).
fn interned(value: &[u8]) -> *const c_char {
    static CACHE: Mutex<Option<HashMap<Vec<u8>, &'static CStr>>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = c.get_or_insert_with(HashMap::new);
    if let Some(s) = map.get(value) {
        return s.as_ptr();
    }
    let s: &'static CStr = Box::leak(to_cstring(value).into_boxed_c_str());
    map.insert(value.to_vec(), s);
    s.as_ptr()
}

// --- librc-misc.c ----------------------------------------------------------

/// `bool rc_yesno(const char *)`, including its errno.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_yesno(value: *const c_char) -> bool {
    // SAFETY: C string from the caller.
    let (yes, errno) = rc::misc::rc_yesno_errno(unsafe { arg(value) });
    if let Some(e) = errno {
        set_errno(e);
    }
    yes
}

/// `bool rc_getfile(const char *file, char **buffer, size_t *len)`.
///
/// Deviation (C has undefined behaviour here): when reading fails, C frees
/// `*buffer` but leaves the dangling pointer; we set it to NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_getfile(file: *const c_char, buffer: *mut *mut c_char, len: *mut usize) -> bool {
    use std::io::Read;
    // SAFETY: C string from the caller.
    let Some(path) = (unsafe { arg(file) }) else { return false };
    let Ok(mut f) = std::fs::File::open(openrc_compat::bytes::as_path(path)) else { return false };
    let data = f.metadata().ok().and_then(|m| {
        let mut v = Vec::new();
        f.by_ref().take(m.len()).read_to_end(&mut v).ok().map(|_| v)
    });
    // SAFETY: buffer and len are valid out-pointers per the API; *buffer
    // is NULL or malloc'd, so realloc/free are valid.
    unsafe {
        match data {
            Some(d) => {
                let p = libc::realloc((*buffer).cast(), d.len() + 1).cast::<u8>();
                if p.is_null() {
                    libc::abort();
                }
                std::ptr::copy_nonoverlapping(d.as_ptr(), p, d.len());
                *p.add(d.len()) = 0;
                *buffer = p.cast();
                *len = d.len() + 1;
                true
            }
            None => {
                libc::free((*buffer).cast());
                *buffer = std::ptr::null_mut();
                *len = 0;
                false
            }
        }
    }
}

/// `char *rc_proc_getent(const char *)`: malloc'd, or NULL with errno set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_proc_getent(ent: *const c_char) -> *mut c_char {
    // SAFETY: C string from the caller.
    let Some(ent) = (unsafe { arg(ent) }) else { return std::ptr::null_mut() };
    match rc::misc::rc_proc_getent(&String::from_utf8_lossy(ent)) {
        Some(v) => malloc_bytes(&v),
        None => {
            set_errno(libc::ENOENT);
            std::ptr::null_mut()
        }
    }
}

/// `RC_STRINGLIST *config_list(int dirfd, const char *pathname)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn config_list(dirfd: c_int, pathname: *const c_char) -> *mut c_void {
    // SAFETY: C string from the caller.
    let path = unsafe { arg(pathname) }.unwrap_or_default();
    if dirfd < 0 && dirfd != libc::AT_FDCWD {
        // openat() fails with EBADF in C, giving an empty list.
        set_errno(libc::EBADF);
        return stringlist(&[]);
    }
    let dir = (dirfd != libc::AT_FDCWD).then(|| {
        // SAFETY: the caller's fd, used only for this call.
        unsafe { std::os::fd::BorrowedFd::borrow_raw(dirfd) }
    });
    stringlist(&rc::conf::config_list_at(dir, path))
}

/// `RC_STRINGLIST *rc_config_list(const char *file)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_config_list(file: *const c_char) -> *mut c_void {
    // SAFETY: forwarded.
    unsafe { config_list(libc::AT_FDCWD, file) }
}

/// `RC_STRINGLIST *rc_config_load(const char *file)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_config_load(file: *const c_char) -> *mut c_void {
    // SAFETY: C string from the caller.
    let path = unsafe { arg(file) }.unwrap_or_default();
    stringlist(&rc::conf::rc_config_load(openrc_compat::bytes::as_path(path)))
}

/// `char *rc_conf_value(const char *)`: points into the cache; not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_conf_value(setting: *const c_char) -> *mut c_char {
    // SAFETY: C string from the caller.
    let Some(s) = (unsafe { arg(setting) }) else { return std::ptr::null_mut() };
    match rc::conf::rc_conf_value_ptr(&String::from_utf8_lossy(s)) {
        Some(p) => p as *mut c_char,
        None => std::ptr::null_mut(),
    }
}

// --- librc.c: directories and user mode -------------------------------------

/// `const char *rc_sys(void)`.
#[unsafe(no_mangle)]
pub extern "C" fn rc_sys() -> *const c_char {
    match rc::rc_sys() {
        Some(s) => interned(s.as_bytes()),
        None => std::ptr::null(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rc_svcdir() -> *const c_char {
    interned(&rc::svcdir::rc_svcdir())
}

#[unsafe(no_mangle)]
pub extern "C" fn rc_sysconfdir() -> *const c_char {
    interned(rc::dirs::rc_sysconfdir().as_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn rc_usrconfdir() -> *const c_char {
    rc::dirs::rc_usrconfdir().map_or(std::ptr::null(), |d| interned(&d))
}

#[unsafe(no_mangle)]
pub extern "C" fn rc_runleveldir() -> *const c_char {
    interned(&rc::dirs::rc_runleveldir())
}

/// `int rc_dirfd(enum rc_dir)`.
#[unsafe(no_mangle)]
pub extern "C" fn rc_dirfd(dir: c_int) -> c_int {
    RcDir::from_raw(dir).map_or(-1, rc::svcdir::rc_dirfd_raw)
}

#[unsafe(no_mangle)]
pub extern "C" fn clear_dirfds() {
    rc::svcdir::clear_dirfds();
}

#[unsafe(no_mangle)]
pub extern "C" fn rc_set_user() {
    rc::rc_set_user();
}

#[unsafe(no_mangle)]
pub extern "C" fn rc_is_user() -> bool {
    rc::rc_is_user()
}

/// `const char * const *rc_scriptdirs(void)`: NULL-terminated.
#[unsafe(no_mangle)]
pub extern "C" fn rc_scriptdirs() -> *const *const c_char {
    static CACHE: Mutex<Option<(Vec<Vec<u8>>, usize)>> = Mutex::new(None);
    let dirs = rc::dirs::rc_scriptdirs();
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((d, ptr)) = &*c
        && *d == dirs
    {
        return *ptr as *const *const c_char;
    }
    let mut ptrs: Vec<*const c_char> = dirs.iter().map(|d| interned(d)).collect();
    ptrs.push(std::ptr::null());
    let leaked: &'static [*const c_char] = Box::leak(ptrs.into_boxed_slice());
    *c = Some((dirs, leaked.as_ptr() as usize));
    leaked.as_ptr()
}

/// `size_t rc_scriptdirfds(const int **fds)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_scriptdirfds(fds: *mut *const c_int) -> usize {
    static EMPTY: [c_int; 1] = [-1];
    let list = rc::dirs::rc_scriptdirfds();
    if !fds.is_null() {
        // SAFETY: valid out-pointer per the API.
        unsafe { *fds = if list.is_empty() { EMPTY.as_ptr() } else { list.as_ptr() } };
    }
    list.len()
}

// --- librc-depend.c: mtime checks -------------------------------------------

/// Shared body of `rc_newer_than` / `rc_older_than`.
///
/// # Safety
/// As for those functions: C strings, and `rel`/`file` null or valid
/// (`file` with room for PATH_MAX bytes).
unsafe fn mtime(
    source: *const c_char,
    target: *const c_char,
    rel: *mut libc::time_t,
    file: *mut c_char,
    newer: bool,
) -> bool {
    // SAFETY: C strings from the caller.
    let (Some(s), Some(t)) = (unsafe { arg(source) }, unsafe { arg(target) }) else { return false };
    let r = if newer { rc::mtime::rc_newer_than(s, t) } else { rc::mtime::rc_older_than(s, t) };
    // SAFETY: out-pointers per the API.
    unsafe {
        if let (false, Some(m)) = (rel.is_null(), r.mtime) {
            *rel = m;
        }
        if let (false, Some(f)) = (file.is_null(), &r.file) {
            // strlcpy(file, target, PATH_MAX)
            let n = f.len().min(libc::PATH_MAX as usize - 1);
            std::ptr::copy_nonoverlapping(f.as_ptr(), file.cast::<u8>(), n);
            *file.add(n) = 0;
        }
    }
    r.ok
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_newer_than(
    s: *const c_char,
    t: *const c_char,
    newest: *mut libc::time_t,
    file: *mut c_char,
) -> bool {
    // SAFETY: forwarded.
    unsafe { mtime(s, t, newest, file, true) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_older_than(
    s: *const c_char,
    t: *const c_char,
    oldest: *mut libc::time_t,
    file: *mut c_char,
) -> bool {
    // SAFETY: forwarded.
    unsafe { mtime(s, t, oldest, file, false) }
}

// --- librc.c: service options and environment -------------------------------

/// `char *rc_service_value_get(const char *service, const char *option)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_service_value_get(service: *const c_char, option: *const c_char) -> *mut c_char {
    // SAFETY: C strings from the caller.
    let (Some(s), Some(o)) = (unsafe { arg(service) }, unsafe { arg(option) }) else { return std::ptr::null_mut() };
    rc::service::rc_service_value_get(s, o).map_or(std::ptr::null_mut(), |v| malloc_bytes(&v))
}

/// `bool rc_service_value_set(const char *service, const char *option, const char *value)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_service_value_set(
    service: *const c_char,
    option: *const c_char,
    value: *const c_char,
) -> bool {
    // SAFETY: C strings from the caller.
    let (Some(s), Some(o)) = (unsafe { arg(service) }, unsafe { arg(option) }) else { return false };
    // SAFETY: C string (or NULL, meaning delete).
    rc::service::rc_service_value_set(s, o, unsafe { arg(value) })
}

/// `bool rc_service_setenv(const char *service, const char *name, const char *value)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_service_setenv(service: *const c_char, name: *const c_char, value: *const c_char) -> bool {
    // SAFETY: C strings from the caller.
    let (Some(s), Some(n), Some(v)) = (unsafe { arg(service) }, unsafe { arg(name) }, unsafe { arg(value) }) else {
        return false;
    };
    rc::service::rc_service_setenv(s, n, v)
}

/// `ssize_t rc_service_getenv(const char *service, const char *name, char **buf)`:
/// with `buf == NULL`, just the size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_service_getenv(
    service: *const c_char,
    name: *const c_char,
    buf: *mut *mut c_char,
) -> isize {
    // SAFETY: C strings from the caller.
    let (Some(s), Some(n)) = (unsafe { arg(service) }, unsafe { arg(name) }) else { return -1 };
    if buf.is_null() {
        return rc::service::rc_service_getenv_size(s, n) as isize;
    }
    let Some(v) = rc::service::rc_service_getenv(s, n) else { return -1 };
    // SAFETY: *buf is NULL or malloc'd per the API.
    unsafe {
        let p = libc::realloc((*buf).cast(), v.len() + 1).cast::<u8>();
        if p.is_null() {
            libc::abort();
        }
        std::ptr::copy_nonoverlapping(v.as_ptr(), p, v.len());
        *p.add(v.len()) = 0;
        *buf = p.cast();
    }
    v.len() as isize
}

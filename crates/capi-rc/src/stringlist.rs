//! `librc-stringlist.c` and `rc_config_value()`: the `RC_STRINGLIST`
//! functions, on the `queue(3)` TAILQ layout that `rc.h` exposes.
//!
//! Callers walk these lists with the TAILQ macros, keep `RC_STRING`
//! pointers across calls and free nodes themselves, so this is a
//! mechanical port: the same `malloc`'d nodes and strings, the same
//! links, and `strcmp()` for comparisons (a NULL value crashes there,
//! as in C).

use std::ffi::{c_char, c_int, c_void};
use std::ptr::{self, null_mut};

/// `RC_STRING`: `{ char *value; TAILQ_ENTRY(rc_string) entries; }`.
#[derive(Debug)]
#[repr(C)]
pub struct RcString {
    pub value: *mut c_char,
    pub next: *mut RcString,
    pub prev: *mut *mut RcString,
}

/// `RC_STRINGLIST`: `TAILQ_HEAD(rc_stringlist, rc_string)`.
#[derive(Debug)]
#[repr(C)]
pub struct RcStringList {
    pub first: *mut RcString,
    pub last: *mut *mut RcString,
}

/// helpers.h's ERRX.
fn out_of_memory() -> ! {
    // SAFETY: plain libc calls.
    unsafe {
        libc::write(2, b"out of memory\n".as_ptr().cast(), 14);
        libc::exit(1)
    }
}

/// `xmalloc()`.
fn xmalloc(size: usize) -> *mut c_void {
    // SAFETY: plain malloc.
    let p = unsafe { libc::malloc(size) };
    if p.is_null() {
        out_of_memory();
    }
    p
}

/// `xstrdup()`: NULL gives NULL.
///
/// # Safety
/// `s` must be null or a valid C string.
unsafe fn xstrdup(s: *const c_char) -> *mut c_char {
    if s.is_null() {
        return null_mut();
    }
    // SAFETY: valid C string per the caller.
    let p = unsafe { libc::strdup(s) };
    if p.is_null() {
        out_of_memory();
    }
    p
}

/// `TAILQ_INIT`.
///
/// # Safety
/// `l` must be valid for writes.
unsafe fn init(l: *mut RcStringList) {
    // SAFETY: per the caller.
    unsafe {
        (*l).first = null_mut();
        (*l).last = &raw mut (*l).first;
    }
}

/// `TAILQ_INSERT_TAIL`.
///
/// # Safety
/// `l` must be a valid list and `s` a node not on any list.
unsafe fn insert_tail(l: *mut RcStringList, s: *mut RcString) {
    // SAFETY: per the caller.
    unsafe {
        (*s).next = null_mut();
        (*s).prev = (*l).last;
        *(*l).last = s;
        (*l).last = &raw mut (*s).next;
    }
}

/// `TAILQ_INSERT_HEAD`.
///
/// # Safety
/// As [`insert_tail`].
unsafe fn insert_head(l: *mut RcStringList, s: *mut RcString) {
    // SAFETY: per the caller.
    unsafe {
        (*s).next = (*l).first;
        if (*s).next.is_null() {
            (*l).last = &raw mut (*s).next;
        } else {
            (*(*l).first).prev = &raw mut (*s).next;
        }
        (*l).first = s;
        (*s).prev = &raw mut (*l).first;
    }
}

/// `TAILQ_INSERT_AFTER`.
///
/// # Safety
/// `after` must be on `l`; `s` on no list.
unsafe fn insert_after(l: *mut RcStringList, after: *mut RcString, s: *mut RcString) {
    // SAFETY: per the caller.
    unsafe {
        (*s).next = (*after).next;
        if (*s).next.is_null() {
            (*l).last = &raw mut (*s).next;
        } else {
            (*(*s).next).prev = &raw mut (*s).next;
        }
        (*after).next = s;
        (*s).prev = &raw mut (*after).next;
    }
}

/// `TAILQ_REMOVE`.
///
/// # Safety
/// `s` must be on `l`.
unsafe fn remove(l: *mut RcStringList, s: *mut RcString) {
    // SAFETY: per the caller.
    unsafe {
        if (*s).next.is_null() {
            (*l).last = (*s).prev;
        } else {
            (*(*s).next).prev = (*s).prev;
        }
        *(*s).prev = (*s).next;
    }
}

/// `strcmp(a, b) == 0`, crashing on NULL as C does.
///
/// # Safety
/// Both must be valid C strings.
unsafe fn eq(a: *const c_char, b: *const c_char) -> bool {
    // SAFETY: per the caller.
    unsafe { libc::strcmp(a, b) == 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn rc_stringlist_new() -> *mut RcStringList {
    let l = xmalloc(size_of::<RcStringList>()).cast::<RcStringList>();
    // SAFETY: freshly allocated.
    unsafe { init(l) };
    l
}

/// # Safety
/// `list` must be a valid list; `value` null or a C string (NULL adds a
/// node whose value is NULL, as `xstrdup(NULL)` does in C).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_stringlist_add(list: *mut RcStringList, value: *const c_char) -> *mut RcString {
    let s = xmalloc(size_of::<RcString>()).cast::<RcString>();
    // SAFETY: per the caller; `s` is fresh.
    unsafe {
        (*s).value = xstrdup(value);
        insert_tail(list, s);
    }
    s
}

/// # Safety
/// `list` must be a valid list and `value` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_stringlist_addu(list: *mut RcStringList, value: *const c_char) -> *mut RcString {
    // SAFETY: per the caller.
    unsafe {
        let mut s = (*list).first;
        while !s.is_null() {
            if eq((*s).value, value) {
                crate::set_errno(libc::EEXIST);
                return null_mut();
            }
            s = (*s).next;
        }
        rc_stringlist_add(list, value)
    }
}

/// # Safety
/// `list` must be a valid list and `value` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_stringlist_delete(list: *mut RcStringList, value: *const c_char) -> bool {
    // SAFETY: per the caller.
    unsafe {
        let mut s = (*list).first;
        while !s.is_null() {
            if eq((*s).value, value) {
                remove(list, s);
                libc::free((*s).value.cast());
                libc::free(s.cast());
                return true;
            }
            s = (*s).next;
        }
    }
    // QUIRK: EEXIST for "not found".
    crate::set_errno(libc::EEXIST);
    false
}

/// # Safety
/// `list` must be null or a valid list, and `value` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_stringlist_find(list: *mut RcStringList, value: *const c_char) -> *mut RcString {
    if list.is_null() {
        return null_mut();
    }
    // SAFETY: per the caller.
    unsafe {
        let mut s = (*list).first;
        while !s.is_null() {
            if eq((*s).value, value) {
                return s;
            }
            s = (*s).next;
        }
    }
    null_mut()
}

/// `strsep()` over a copy of `value`: every piece between bytes of `sep`,
/// empty ones included.  A NULL `value` gives an empty list.
///
/// # Safety
/// `value` must be null or a C string; `sep` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_stringlist_split(value: *const c_char, sep: *const c_char) -> *mut RcStringList {
    let list = rc_stringlist_new();
    if value.is_null() {
        return list;
    }
    // SAFETY: C strings per the caller.
    let (v, sep) = unsafe { (std::ffi::CStr::from_ptr(value).to_bytes(), std::ffi::CStr::from_ptr(sep).to_bytes()) };
    for token in v.split(|b| sep.contains(b)) {
        let c = openrc_compat::bytes::to_cstring(token);
        // SAFETY: `list` is valid; add copies the string.
        unsafe { rc_stringlist_add(list, c.as_ptr()) };
    }
    list
}

/// Insertion sort into a new head, moving the nodes (so `RC_STRING`
/// pointers stay valid) and freeing the old head.  Stable.
///
/// # Safety
/// `*list` must be a valid list allocated by `rc_stringlist_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_stringlist_sort(list: *mut *mut RcStringList) {
    // SAFETY: per the caller.
    unsafe {
        let l = *list;
        let new = rc_stringlist_new();
        let mut s = (*l).first;
        while !s.is_null() {
            let sn = (*s).next;
            remove(l, s);
            let mut last: *mut RcString = null_mut();
            let mut n = (*new).first;
            while !n.is_null() {
                if libc::strcmp((*s).value, (*n).value) < 0 {
                    break;
                }
                last = n;
                n = (*n).next;
            }
            if last.is_null() {
                insert_head(new, s);
            } else {
                insert_after(new, last, s);
            }
            s = sn;
        }
        libc::free(l.cast());
        *list = new;
    }
}

/// # Safety
/// `list` must be null or a valid list whose nodes and values are
/// `malloc`'d.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_stringlist_free(list: *mut RcStringList) {
    if list.is_null() {
        return;
    }
    // SAFETY: per the caller.
    unsafe {
        let mut s = (*list).first;
        while !s.is_null() {
            let next = (*s).next;
            libc::free((*s).value.cast());
            libc::free(s.cast());
            s = next;
        }
        libc::free(list.cast());
    }
}

/// The value of the first `entry=...` line, pointing into the caller's
/// list.  QUIRK: the value starts after the line's *first* '=', so an
/// `entry` that itself contains '=' gets the text after that one
/// (`"a=b"` on `"a=b=c"` gives `"b=c"`).
///
/// Deviation: a NULL list gives NULL; C dereferences it.
///
/// # Safety
/// `list` must be null or a valid list; `entry` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rc_config_value(list: *mut RcStringList, entry: *const c_char) -> *mut c_char {
    if list.is_null() {
        return null_mut();
    }
    // SAFETY: per the caller.
    unsafe {
        let len = libc::strlen(entry);
        let mut line = (*list).first;
        while !line.is_null() {
            let v = (*line).value;
            let p = libc::strchr(v, c_int::from(b'='));
            if !p.is_null() && libc::strncmp(entry, v, len) == 0 && *v.add(len) == b'=' as c_char {
                return p.add(1);
            }
            line = (*line).next;
        }
    }
    null_mut()
}

/// `FILE *rc_environ_fd` (librc.c): unused inside librc, exported for
/// programs that set it.
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static mut rc_environ_fd: *mut libc::FILE = ptr::null_mut();

/// Build a list from Rust items.
pub fn from_items(items: &[Vec<u8>]) -> *mut RcStringList {
    let list = rc_stringlist_new();
    for item in items {
        let c = openrc_compat::bytes::to_cstring(item);
        // SAFETY: `list` is valid; add copies the string.
        unsafe { rc_stringlist_add(list, c.as_ptr()) };
    }
    list
}

/// The items of a list (NULL gives none).
///
/// # Safety
/// `list` must be null or a valid list with non-NULL values.
pub unsafe fn items(list: *const RcStringList) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if list.is_null() {
        return out;
    }
    // SAFETY: per the caller.
    unsafe {
        let mut s = (*list).first;
        while !s.is_null() {
            out.push(std::ffi::CStr::from_ptr((*s).value).to_bytes().to_vec());
            s = (*s).next;
        }
    }
    out
}

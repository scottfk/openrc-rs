//! C ABI for [`openrc_einfo`]: the Rust half of a drop-in `libeinfo.so.1`.
//!
//! The non-variadic functions (`ecolor`, `eindent`, `eprefix`, ...) are
//! exported from here directly.  The printf-style ones are defined in
//! `shim/einfo-shim.c`, which formats and calls the `rs_*` entry points
//! below.  `make capi` links both into the shared library with
//! `libeinfo.map` as the export list.
//!
//! Behavioral difference from C, by construction: the shim formats the
//! message even when it will not be printed (e.g. `EINFO_QUIET`).

use std::ffi::{CStr, c_char, c_int};

use openrc_compat::stdio::{self, Stream};
use openrc_einfo as e;

unsafe extern "C" {
    fn rs_einfo_cwrite(stream: c_int, buf: *const c_char, len: usize);
}

fn cwrite(stream: Stream, data: &[u8]) {
    // SAFETY: data is a valid buffer; the C side only reads it.
    unsafe { rs_einfo_cwrite(stream.fd(), data.as_ptr().cast(), data.len()) }
}

/// Route output through the host program's stdio.
fn init() {
    stdio::set_backend(cwrite);
}

/// # Safety
/// `msg` must be null or point to `len` readable bytes that outlive `'a`.
unsafe fn message<'a>(msg: *const c_char, len: usize) -> Option<&'a [u8]> {
    // SAFETY: guaranteed by the caller.
    (!msg.is_null()).then(|| unsafe { std::slice::from_raw_parts(msg.cast(), len) })
}

/// # Safety
/// `msg` must be null or point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_einfo_msg(which: c_int, msg: *const c_char, len: usize) -> c_int {
    init();
    // SAFETY: forwarded from the shim, which passes its own buffer.
    let m = unsafe { message(msg, len) };
    let f: fn(Option<&[u8]>) -> i32 = match which {
        0 => e::einfon,
        1 => e::ewarnn,
        2 => e::eerrorn,
        3 => e::einfo,
        4 => e::ewarn,
        5 => e::eerror,
        6 => e::einfovn,
        7 => e::ewarnvn,
        8 => e::einfov,
        9 => e::ewarnv,
        10 => e::ebeginv,
        11 => e::ebegin,
        _ => return 0,
    };
    f(m)
}

/// # Safety
/// `msg` must be null or point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_einfo_end(which: c_int, retval: c_int, msg: *const c_char, len: usize) -> c_int {
    init();
    // SAFETY: forwarded from the shim, which passes its own buffer.
    let m = unsafe { message(msg, len) };
    let f: fn(i32, Option<&[u8]>) -> i32 = match which {
        0 => e::eend,
        1 => e::ewend,
        2 => e::eendv,
        3 => e::ewendv,
        _ => return retval,
    };
    f(retval, m)
}

/// # Safety
/// `msg` must be null or point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_einfo_exit(which: c_int, msg: *const c_char, len: usize) -> ! {
    init();
    // SAFETY: forwarded from the shim, which passes its own buffer.
    let m = unsafe { message(msg, len) };
    if which == 0 { e::ewarnx(m) } else { e::eerrorx(m) }
}

/// # Safety
/// `msg` must be null or point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_elog(level: c_int, msg: *const c_char, len: usize) {
    init();
    // SAFETY: forwarded from the shim, which passes its own buffer.
    e::elog(level, unsafe { message(msg, len) });
}

/// `const char *ecolor(ECOLOR)`.  The returned strings live forever.
#[unsafe(no_mangle)]
pub extern "C" fn ecolor(color: c_int) -> *const c_char {
    init();
    e::ecolor_raw(color).as_ptr()
}

/// `void ebracket(int col, ECOLOR color, const char *msg)`.
///
/// # Safety
/// `msg` must be null or a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ebracket(col: c_int, color: c_int, msg: *const c_char) {
    init();
    // SAFETY: guaranteed by the caller.
    let m = (!msg.is_null()).then(|| unsafe { CStr::from_ptr(msg) }.to_bytes());
    e::ebracket_raw(col, color, m);
}

/// `void eprefix(const char *prefix)`.  Unlike C, the string is copied, so
/// later changes to the caller's buffer are not seen.
///
/// # Safety
/// `prefix` must be null or a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eprefix(prefix: *const c_char) {
    init();
    // SAFETY: guaranteed by the caller.
    let p = (!prefix.is_null()).then(|| unsafe { CStr::from_ptr(prefix) }.to_bytes());
    e::eprefix(p);
}

#[unsafe(no_mangle)]
pub extern "C" fn eindent() {
    init();
    e::eindent();
}

#[unsafe(no_mangle)]
pub extern "C" fn eoutdent() {
    init();
    e::eoutdent();
}

#[unsafe(no_mangle)]
pub extern "C" fn eindentv() {
    init();
    e::eindentv();
}

#[unsafe(no_mangle)]
pub extern "C" fn eoutdentv() {
    init();
    e::eoutdentv();
}

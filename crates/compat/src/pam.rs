//! Linux-PAM, for the `pam` feature (Gentoo's `USE=pam`): the calls the
//! daemons, openrc-user and pam_openrc make, declared here and linked
//! against libpam.so.0.  No conversation function is ever supplied.

use std::ffi::{CStr, c_char, c_int, c_void};

use crate::bytes::to_cstring;

pub const PAM_SUCCESS: c_int = 0;
pub const PAM_SESSION_ERR: c_int = 14;
pub const PAM_SILENT: c_int = 0x8000;
pub const PAM_SERVICE: c_int = 1;
pub const PAM_USER: c_int = 2;

/// An opaque `pam_handle_t`.
#[repr(C)]
#[derive(Debug)]
pub struct PamHandle {
    _private: [u8; 0],
}

/// `struct pam_conv`.
#[repr(C)]
#[derive(Debug)]
struct PamConv {
    conv: *const c_void,
    appdata_ptr: *mut c_void,
}

/// `{ NULL, NULL }`: "we are not supporting authentication conversations".
static NO_CONV: PamConv = PamConv { conv: std::ptr::null(), appdata_ptr: std::ptr::null_mut() };
// SAFETY: two null pointers, never written.
unsafe impl Sync for PamConv {}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conv: *const PamConv, pamh: *mut *mut PamHandle)
    -> c_int;
    fn pam_end(pamh: *mut PamHandle, status: c_int) -> c_int;
    fn pam_acct_mgmt(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_open_session(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_close_session(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_strerror(pamh: *mut PamHandle, errnum: c_int) -> *const c_char;
    fn pam_getenvlist(pamh: *mut PamHandle) -> *mut *mut c_char;
    fn pam_get_item(pamh: *const PamHandle, item_type: c_int, item: *mut *const c_void) -> c_int;
    fn pam_putenv(pamh: *mut PamHandle, name_value: *const c_char) -> c_int;
}

/// A handle from `pam_start` (possibly NULL, as C may be left with when
/// it fails), or one PAM gave a module.
#[derive(Debug)]
pub struct Pam(pub *mut PamHandle);

impl Pam {
    /// `pam_start(service, user, &conv, &pamh)`: the handle (NULL if PAM
    /// couldn't allocate one) and the result.
    pub fn start(service: &[u8], user: &[u8]) -> (Pam, c_int) {
        let (s, u) = (to_cstring(service), to_cstring(user));
        let mut h = std::ptr::null_mut();
        // SAFETY: valid C strings, a static conversation, an out pointer.
        let r = unsafe { pam_start(s.as_ptr(), u.as_ptr(), &NO_CONV, &mut h) };
        (Pam(h), r)
    }

    pub fn acct_mgmt(&self, flags: c_int) -> c_int {
        // SAFETY: a handle from pam_start (PAM checks for NULL).
        unsafe { pam_acct_mgmt(self.0, flags) }
    }

    pub fn open_session(&self, flags: c_int) -> c_int {
        // SAFETY: as above.
        unsafe { pam_open_session(self.0, flags) }
    }

    pub fn close_session(&self, flags: c_int) -> c_int {
        // SAFETY: as above.
        unsafe { pam_close_session(self.0, flags) }
    }

    pub fn end(self, status: c_int) -> c_int {
        // SAFETY: as above; the handle isn't used again.
        unsafe { pam_end(self.0, status) }
    }

    /// `pam_strerror(pamh, errnum)`.
    pub fn strerror(&self, errnum: c_int) -> Vec<u8> {
        // SAFETY: PAM returns a static string (or NULL).
        let p = unsafe { pam_strerror(self.0, errnum) };
        if p.is_null() {
            return b"(null)".to_vec();
        }
        // SAFETY: a valid C string.
        unsafe { CStr::from_ptr(p) }.to_bytes().to_vec()
    }

    /// `pam_getenvlist()`: the PAM environment's entries (copies; the
    /// list PAM allocated is freed).
    pub fn getenvlist(&self) -> Vec<Vec<u8>> {
        // SAFETY: as above; the list is NULL-terminated and ours to free.
        unsafe {
            let list = pam_getenvlist(self.0);
            if list.is_null() {
                return Vec::new();
            }
            let mut out = Vec::new();
            let mut p = list;
            while !(*p).is_null() {
                out.push(CStr::from_ptr(*p).to_bytes().to_vec());
                libc::free((*p).cast());
                p = p.add(1);
            }
            libc::free(list.cast());
            out
        }
    }

    /// `pam_get_item(pamh, item, &value)` for a string item: Err(result)
    /// if PAM fails, else the value (None when unset).
    pub fn get_str_item(&self, item: c_int) -> Result<Option<Vec<u8>>, c_int> {
        let mut v: *const c_void = std::ptr::null();
        // SAFETY: a module's handle and an out pointer.
        let r = unsafe { pam_get_item(self.0, item, &mut v) };
        if r != PAM_SUCCESS {
            return Err(r);
        }
        // SAFETY: string items are C strings.
        Ok((!v.is_null()).then(|| unsafe { CStr::from_ptr(v.cast()) }.to_bytes().to_vec()))
    }

    /// `pam_putenv(pamh, "NAME=value")`.
    pub fn putenv(&self, name_value: &[u8]) -> c_int {
        let s = to_cstring(name_value);
        // SAFETY: as above.
        unsafe { pam_putenv(self.0, s.as_ptr()) }
    }
}

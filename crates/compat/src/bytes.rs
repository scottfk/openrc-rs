//! Byte-string helpers.  OpenRC deals in C strings, which need not be UTF-8,
//! so messages are assembled as `Vec<u8>` rather than `String`.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

/// Anything that can be viewed as raw bytes, for [`bcat!`](crate::bcat).
pub trait AsBytes {
    fn as_bytes_ref(&self) -> &[u8];
}

impl AsBytes for [u8] {
    fn as_bytes_ref(&self) -> &[u8] {
        self
    }
}
impl<const N: usize> AsBytes for [u8; N] {
    fn as_bytes_ref(&self) -> &[u8] {
        self
    }
}
impl AsBytes for Vec<u8> {
    fn as_bytes_ref(&self) -> &[u8] {
        self
    }
}
impl AsBytes for str {
    fn as_bytes_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl AsBytes for String {
    fn as_bytes_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl AsBytes for OsStr {
    fn as_bytes_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl AsBytes for OsString {
    fn as_bytes_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl AsBytes for CStr {
    fn as_bytes_ref(&self) -> &[u8] {
        self.to_bytes()
    }
}
impl AsBytes for CString {
    fn as_bytes_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl<T: AsBytes + ?Sized> AsBytes for &T {
    fn as_bytes_ref(&self) -> &[u8] {
        (**self).as_bytes_ref()
    }
}

/// Concatenate byte-ish values into a `Vec<u8>`:
/// `bcat!(applet, ": invalid mode '", arg, "'")`.
#[macro_export]
macro_rules! bcat {
    ($($e:expr),* $(,)?) => {{
        let mut v: ::std::vec::Vec<u8> = ::std::vec::Vec::new();
        $( v.extend_from_slice($crate::bytes::AsBytes::as_bytes_ref(&$e)); )*
        v
    }};
}

/// OpenRC's `basename_c()`: everything after the last `/`.  Never modifies
/// its argument, so a trailing slash yields an empty string.
pub fn basename_c(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// The part of `s` a C function would see: everything before the first NUL.
pub fn c_str_prefix(s: &[u8]) -> &[u8] {
    match s.iter().position(|&b| b == 0) {
        Some(i) => &s[..i],
        None => s,
    }
}

/// `strstr()`: index of the first occurrence of `needle` in `hay`.
pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// View a byte string as a filesystem path (no conversion, no loss).
pub fn as_path(b: &[u8]) -> &std::path::Path {
    std::path::Path::new(OsStr::from_bytes(b))
}

/// Build a `CString` for a syscall; C strings stop at the first NUL anyway.
pub fn to_cstring(s: &[u8]) -> CString {
    CString::new(c_str_prefix(s)).expect("NUL already stripped")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename() {
        assert_eq!(basename_c(b"/usr/bin/einfo"), b"einfo");
        assert_eq!(basename_c(b"einfo"), b"einfo");
        assert_eq!(basename_c(b"/a/b/"), b"");
    }

    #[test]
    fn concat() {
        let os = OsStr::new("x");
        assert_eq!(bcat!("a", b"b", String::from("c"), os), b"abcx");
    }
}

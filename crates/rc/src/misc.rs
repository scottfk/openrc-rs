//! Small helpers from `librc-misc.c`.

/// `rc_yesno()`: true for yes/y/true/1 (case-insensitive).  Unlike
/// libeinfo's private version, on/off are *not* recognized.
pub fn rc_yesno(value: Option<&[u8]>) -> bool {
    rc_yesno_errno(value).0
}

/// [`rc_yesno`] plus the `errno` the C function sets: ENOENT for a NULL
/// value, EINVAL for something that is neither yes nor no, untouched
/// (`None`) otherwise.  C callers test errno to tell "no" from "unset".
pub fn rc_yesno_errno(value: Option<&[u8]>) -> (bool, Option<i32>) {
    use openrc_compat::sys::libc;
    let Some(v) = value else { return (false, Some(libc::ENOENT)) };
    let is = |w: &&str| v.eq_ignore_ascii_case(w.as_bytes());
    if ["yes", "y", "true", "1"].iter().any(is) {
        return (true, None);
    }
    if ["no", "n", "false", "0"].iter().any(is) { (false, None) } else { (false, Some(libc::EINVAL)) }
}

/// `rc_getfile()`: a whole file, read as `st_size` bytes.
///
/// Deviations from C, both in cases where C misbehaves: if the file is
/// shorter than `st_size` when read, C loops forever, we return what was
/// read; if reading fails (e.g. a directory), C hands its caller a freed
/// buffer, we return `None`.
pub fn rc_getfile(path: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let p = openrc_compat::bytes::as_path(path);
    let mut f = std::fs::File::open(p).ok()?;
    let size = f.metadata().ok()?.len();
    let mut buf = Vec::with_capacity(size as usize);
    f.by_ref().take(size).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// `rc_proc_getent()`: the value of `ent` on the kernel command line.
/// `ent` alone yields an empty value; the last occurrence wins.
pub fn rc_proc_getent(ent: &str) -> Option<Vec<u8>> {
    let data = std::fs::read("/proc/cmdline").ok()?;
    // Only the first line, as a C string.
    let line = data.split(|&b| b == b'\n').next()?;
    if line.is_empty() && data.is_empty() {
        return None;
    }
    let line = openrc_compat::bytes::c_str_prefix(line);
    let ent = ent.as_bytes();
    let mut value = None;
    for p in line.split(|&b| b == b' ') {
        if p.starts_with(ent) && matches!(p.get(ent.len()), None | Some(b' ') | Some(b'=')) {
            let mut v = &p[ent.len()..];
            if v.first() == Some(&b'=') {
                v = &v[1..];
            }
            value = Some(v.to_vec());
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yesno() {
        assert!(rc_yesno(Some(b"YES")));
        assert!(!rc_yesno(Some(b"on")));
        assert!(!rc_yesno(None));
    }
}

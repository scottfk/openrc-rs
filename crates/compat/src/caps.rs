//! Process capabilities without libcap: the parts of libcap (2.78) that
//! start-stop-daemon and supervise-daemon use, on the raw syscalls.
//!
//! - [`Iab::from_text`] is `cap_iab_from_text()`: a comma-separated list of
//!   capability names or numbers, each with optional prefixes: `%`
//!   inheritable, `^` inheritable and ambient, `!` drop from the bounding
//!   set (no prefix means `%`).
//! - [`Iab::set_proc`] is `cap_iab_set_proc()`, [`setuid_keep_caps`] is
//!   `cap_setuid()`, [`set_secbits`] is `cap_set_secbits()`.

use crate::sys::{Errno, libc};

/// libcap's `__CAP_MAXBITS`: two 32-bit words.
const MAXBITS: usize = 64;
const CAP_SETUID: usize = 7;
const CAP_SETPCAP: usize = 8;
/// `_LINUX_CAPABILITY_VERSION_3`.
const VERSION_3: u32 = 0x2008_0522;

/// The names libcap knows (from the kernel's `linux/capability.h`), by
/// number.
const NAMES: &[&str] = &[
    "cap_chown",
    "cap_dac_override",
    "cap_dac_read_search",
    "cap_fowner",
    "cap_fsetid",
    "cap_kill",
    "cap_setgid",
    "cap_setuid",
    "cap_setpcap",
    "cap_linux_immutable",
    "cap_net_bind_service",
    "cap_net_broadcast",
    "cap_net_admin",
    "cap_net_raw",
    "cap_ipc_lock",
    "cap_ipc_owner",
    "cap_sys_module",
    "cap_sys_rawio",
    "cap_sys_chroot",
    "cap_sys_ptrace",
    "cap_sys_pacct",
    "cap_sys_admin",
    "cap_sys_boot",
    "cap_sys_nice",
    "cap_sys_resource",
    "cap_sys_time",
    "cap_sys_tty_config",
    "cap_mknod",
    "cap_lease",
    "cap_audit_write",
    "cap_audit_control",
    "cap_setfcap",
    "cap_mac_override",
    "cap_mac_admin",
    "cap_syslog",
    "cap_wake_alarm",
    "cap_block_suspend",
    "cap_audit_read",
    "cap_perfmon",
    "cap_bpf",
    "cap_checkpoint_restore",
];

/// An IAB tuple: inheritable, ambient, and not-bounding bits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Iab {
    i: [u32; 2],
    a: [u32; 2],
    nb: [u32; 2],
}

/// `strtoul(s, &end, 0)`: (value, bytes used), saturating like C.
fn strtoul0(s: &[u8]) -> (u64, usize) {
    let (radix, mut i) =
        if s.len() > 1 && s[0] == b'0' && (s[1] | 0x20) == b'x' && s.get(2).is_some_and(u8::is_ascii_hexdigit) {
            (16, 2)
        } else if s.first() == Some(&b'0') {
            (8, 0)
        } else {
            (10, 0)
        };
    let mut v: u64 = 0;
    let start = i;
    while let Some(d) = s.get(i).and_then(|&c| (c as char).to_digit(radix)) {
        v = v.saturating_mul(u64::from(radix)).saturating_add(u64::from(d));
        i += 1;
    }
    if i == start && radix == 16 {
        // "0x" with no hex digits: just the "0".
        return (0, 1);
    }
    (v, i)
}

/// `lookupname()`: a capability number or (case-insensitive) name at the
/// start of `s`; (value, bytes used).
fn lookupname(s: &[u8]) -> Option<(usize, usize)> {
    if s.first().is_some_and(u8::is_ascii_digit) {
        let (n, used) = strtoul0(s);
        return (n < MAXBITS as u64).then_some((n as usize, used));
    }
    let len = s.iter().take_while(|&&c| c.is_ascii_alphabetic() || c == b'_').count();
    let token = &s[..len];
    let max = max_bits().min(NAMES.len());
    // libcap tries the highest numbers first; names are unique anyway.
    (0..max).rev().find(|&n| NAMES[n].as_bytes().eq_ignore_ascii_case(token)).map(|n| (n, len))
}

impl Iab {
    /// `cap_iab_from_text()`; None is EINVAL.
    pub fn from_text(text: &[u8]) -> Option<Iab> {
        const I: u32 = 1;
        const A: u32 = 2;
        const NB: u32 = 4;
        let mut iab = Iab::default();
        let mut flags = 0;
        let mut p = 0;
        while p < text.len() {
            // Consume prefixes.
            match text[p] {
                b'!' => {
                    flags |= NB;
                    p += 1;
                    continue;
                }
                b'^' => {
                    flags |= I | A;
                    p += 1;
                    continue;
                }
                b'%' => {
                    flags |= I;
                    p += 1;
                    continue;
                }
                _ => {}
            }
            if flags == 0 {
                flags = I;
            }
            let (c, used) = lookupname(&text[p..])?;
            p += used;
            let (o, mask) = (c >> 5, 1u32 << (c & 31));
            if flags & I != 0 {
                iab.i[o] |= mask;
            }
            if flags & A != 0 {
                iab.a[o] |= mask;
            }
            if flags & NB != 0 {
                iab.nb[o] |= mask;
            }
            // The rest is the end or a comma.
            match text.get(p) {
                None => break,
                Some(b',') => {}
                Some(_) => return None,
            }
            flags = 0;
            p += 1;
        }
        Some(iab)
    }

    fn has(set: &[u32; 2], c: usize) -> bool {
        set[c >> 5] & (1 << (c & 31)) != 0
    }

    /// `cap_iab_set_proc()`: set the inheritable and ambient sets, and drop
    /// from the bounding set, raising CAP_SETPCAP while doing so if needed.
    pub fn set_proc(&self) -> Result<(), Errno> {
        let mut temp = Caps::get()?;
        let mut raising = 0u32;
        let mut check_bound = false;
        for i in 0..2 {
            let new_i = self.i[i];
            let old_ip = temp.0[i].inheritable | temp.0[i].permitted;
            raising |= new_i & !old_ip;
            if self.nb[i] != 0 {
                check_bound = true;
            }
            temp.0[i].inheritable = new_i;
        }
        let max = max_bits();
        if check_bound {
            check_bound = false;
            for c in (0..max).rev() {
                // (cap_get_bound's -1 counts as "in the set" too.)
                if Self::has(&self.nb, c) && capbset_read(c) != 0 {
                    raising = 1;
                    check_bound = true;
                    break;
                }
            }
        }
        let mut working = temp.clone();
        if raising != 0 {
            working.0[CAP_SETPCAP >> 5].effective |= 1 << (CAP_SETPCAP & 31);
        }
        working.set()?;
        let r = (|| {
            reset_ambient()?;
            for c in (0..max).rev() {
                if Self::has(&self.a, c) {
                    prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_RAISE as u64, c as u64)?;
                }
                if check_bound && Self::has(&self.nb, c) {
                    prctl(libc::PR_CAPBSET_DROP, c as u64, 0)?;
                }
            }
            Ok(())
        })();
        let _ = temp.set();
        r
    }
}

#[repr(C)]
struct Header {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct Data {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// The process's capability sets (`cap_t`).
#[derive(Debug, Clone, Default)]
struct Caps([Data; 2]);

impl Caps {
    /// `cap_get_proc()`.
    fn get() -> Result<Caps, Errno> {
        let mut hdr = Header { version: VERSION_3, pid: 0 };
        let mut data = [Data::default(); 2];
        // SAFETY: capget writes two Data structs (version 3) through valid
        // pointers.
        let r = unsafe { libc::syscall(libc::SYS_capget, &mut hdr as *mut Header, data.as_mut_ptr()) };
        if r < 0 { Err(Errno::last()) } else { Ok(Caps(data)) }
    }

    /// `cap_set_proc()`.
    fn set(&self) -> Result<(), Errno> {
        let mut hdr = Header { version: VERSION_3, pid: 0 };
        // SAFETY: capset reads two Data structs (version 3) through valid
        // pointers.
        let r = unsafe { libc::syscall(libc::SYS_capset, &mut hdr as *mut Header, self.0.as_ptr()) };
        if r < 0 { Err(Errno::last()) } else { Ok(()) }
    }
}

fn prctl(op: i32, a: u64, b: u64) -> Result<i32, Errno> {
    // SAFETY: prctl with integer arguments only.
    let r = unsafe { libc::prctl(op, a, b, 0u64, 0u64) };
    if r < 0 { Err(Errno::last()) } else { Ok(r) }
}

/// `cap_get_bound(c)`: 1 if in the bounding set, 0 if not, -1 on error.
fn capbset_read(c: usize) -> i32 {
    // SAFETY: prctl with integer arguments only.
    unsafe { libc::prctl(libc::PR_CAPBSET_READ, c as u64, 0u64, 0u64, 0u64) }
}

/// `cap_max_bits()`: how many capabilities the kernel knows.
fn max_bits() -> usize {
    (0..MAXBITS).find(|&c| capbset_read(c) < 0).unwrap_or(MAXBITS)
}

/// `_cap_reset_ambient()`: clear the ambient set, unless it's empty (or
/// can't be read).
fn reset_ambient() -> Result<(), Errno> {
    let saved = Errno::last();
    let mut c = 0u64;
    loop {
        // SAFETY: prctl with integer arguments only.
        let r = unsafe { libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_IS_SET as u64, c, 0u64, 0u64) };
        if r < 0 {
            crate::sys::set_errno(saved.0);
            return Ok(());
        }
        if r != 0 {
            break;
        }
        c += 1;
    }
    prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_CLEAR_ALL as u64, 0).map(drop)
}

/// `cap_setuid(uid)`: change uid keeping the permitted set (the effective
/// set ends up empty).
pub fn setuid_keep_caps(uid: u32) -> Result<(), Errno> {
    let mut working = Caps::get()?;
    working.0[CAP_SETUID >> 5].effective |= 1 << (CAP_SETUID & 31);
    let _ = prctl(libc::PR_SET_KEEPCAPS, 1, 0);
    let mut r = working.set();
    if r.is_ok() {
        // SAFETY: plain syscall.
        if unsafe { libc::setuid(uid) } != 0 {
            r = Err(Errno::last());
        }
    }
    let saved = Errno::last();
    let _ = prctl(libc::PR_SET_KEEPCAPS, 0, 0);
    for d in &mut working.0 {
        d.effective = 0;
    }
    let _ = working.set();
    crate::sys::set_errno(saved.0);
    r
}

/// `cap_set_secbits(bits)`.
pub fn set_secbits(bits: u32) -> Result<(), Errno> {
    prctl(libc::PR_SET_SECUREBITS, u64::from(bits), 0).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(list: &[usize]) -> [u32; 2] {
        let mut b = [0; 2];
        for &c in list {
            b[c >> 5] |= 1 << (c & 31);
        }
        b
    }

    #[test]
    fn parse() {
        let p = |s: &str| Iab::from_text(s.as_bytes());
        assert_eq!(p(""), Some(Iab::default()));
        assert_eq!(p("cap_chown").unwrap().i, bits(&[0]));
        let x = p("^CAP_NET_bind_service,!cap_sys_admin,%7").unwrap();
        assert_eq!((x.i, x.a, x.nb), (bits(&[10, 7]), bits(&[10]), bits(&[21])));
        assert_eq!(p("!%cap_kill,").unwrap().nb, bits(&[5]));
        assert_eq!(p("0x22").unwrap().i, bits(&[34]));
        assert_eq!(p("010").unwrap().i, bits(&[8]));
        // Only prefixes: nothing.
        assert_eq!(p("!"), Some(Iab::default()));
        for bad in [",", "cap_chown5", "nosuch", "64", "cap_chown;", "0x", "cap_chown,,"] {
            assert_eq!(p(bad), None, "{bad}");
        }
    }
}

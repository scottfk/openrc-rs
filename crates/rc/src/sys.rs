//! Container and VM detection: `rc_sys()` (`librc.c`), Linux only.

use crate::conf::with_rc_conf;

pub const RC_SYS_DOCKER: &str = "DOCKER";
pub const RC_SYS_PODMAN: &str = "PODMAN";
pub const RC_SYS_NONE: &str = "";
pub const RC_SYS_OPENVZ: &str = "OPENVZ";
pub const RC_SYS_LXC: &str = "LXC";
pub const RC_SYS_RKT: &str = "RKT";
pub const RC_SYS_SYSTEMD_NSPAWN: &str = "SYSTEMD-NSPAWN";
pub const RC_SYS_UML: &str = "UML";
pub const RC_SYS_VSERVER: &str = "VSERVER";
pub const RC_SYS_XEN0: &str = "XEN0";
pub const RC_SYS_XENU: &str = "XENU";
pub const RC_SYS_WSL: &str = "WSL";

/// The fixed POSIX EREs `file_regex()` is called with, as matchers.
#[derive(Debug, Clone, Copy)]
enum Pattern {
    /// A literal substring.
    Literal(&'static str),
    /// `(k1|k2|...):[[:space:]]*[1-9]`
    KeyNonZero(&'static [&'static str]),
}

impl Pattern {
    fn matches(self, s: &[u8]) -> bool {
        match self {
            Pattern::Literal(l) => openrc_compat::bytes::find(s, l.as_bytes()).is_some(),
            Pattern::KeyNonZero(keys) => keys.iter().any(|k| {
                let k = k.as_bytes();
                (0..s.len()).any(|i| {
                    let Some(rest) = s[i..].strip_prefix(k).and_then(|r| r.strip_prefix(b":")) else {
                        return false;
                    };
                    let j = rest.iter().position(|&b| !openrc_compat::num::is_c_space(b)).unwrap_or(rest.len());
                    matches!(rest.get(j), Some(b'1'..=b'9'))
                })
            }),
        }
    }
}

/// `file_regex()`: does any line (or NUL-separated piece of a line, for
/// files like `/proc/1/environ`) of `file` match?
fn file_regex(file: &str, pat: Pattern) -> bool {
    let Ok(data) = std::fs::read(file) else { return false };
    data.split(|&b| b == b'\n')
        .flat_map(|line| line.split(|&b| b == 0))
        .filter(|piece| !piece.is_empty())
        .any(|piece| pat.matches(piece))
}

fn exists(p: &str) -> bool {
    // access(F_OK)
    openrc_compat::sys::access(p.as_bytes(), openrc_compat::sys::libc::F_OK).is_ok()
}

/// `get_systype()`: `rc_sys` from rc.conf, uppercased.  QUIRK: like C,
/// this uppercases the cached rc.conf entry in place.
fn get_systype() -> Option<Vec<u8>> {
    with_rc_conf(|conf| {
        // The entry rc_config_value() would return.
        let i = conf.iter().position(|l| l.starts_with(b"rc_sys="))?;
        let line = &mut conf[i];
        line[b"rc_sys=".len()..].make_ascii_uppercase();
        Some(line[b"rc_sys=".len()..].to_vec())
    })
}

// The checks are kept separate, in upstream's order, even where two give
// the same answer.
#[allow(clippy::if_same_then_else)]
fn detect_container(systype: Option<&[u8]>) -> Option<&'static str> {
    if let Some(s) = systype {
        if s == RC_SYS_NONE.as_bytes() {
            return None;
        }
        for known in [
            RC_SYS_UML,
            RC_SYS_VSERVER,
            RC_SYS_OPENVZ,
            RC_SYS_LXC,
            RC_SYS_RKT,
            RC_SYS_SYSTEMD_NSPAWN,
            RC_SYS_DOCKER,
            RC_SYS_PODMAN,
            RC_SYS_WSL,
        ] {
            if s == known.as_bytes() {
                return Some(known);
            }
        }
    }

    use Pattern::{KeyNonZero, Literal};
    if file_regex("/proc/cpuinfo", Literal("UML")) {
        Some(RC_SYS_UML)
    } else if file_regex("/proc/self/status", KeyNonZero(&["s_context", "VxID"])) {
        Some(RC_SYS_VSERVER)
    } else if exists("/proc/vz/veinfo") && !exists("/proc/vz/version") {
        Some(RC_SYS_OPENVZ)
    } else if file_regex("/proc/self/status", KeyNonZero(&["envID"])) {
        Some(RC_SYS_OPENVZ)
    } else if file_regex("/proc/1/environ", Literal("container=lxc")) {
        Some(RC_SYS_LXC)
    } else if file_regex("/proc/1/environ", Literal("container=rkt")) {
        Some(RC_SYS_RKT)
    } else if file_regex("/proc/1/environ", Literal("container=systemd-nspawn")) {
        Some(RC_SYS_SYSTEMD_NSPAWN)
    } else if exists("/run/.containerenv") {
        Some(RC_SYS_PODMAN)
    } else if exists("/.dockerenv") {
        Some(RC_SYS_DOCKER)
    } else if file_regex("/proc/1/environ", Literal("container=docker")) {
        Some(RC_SYS_DOCKER)
    } else if file_regex("/proc/sys/kernel/osrelease", Literal("microsoft")) {
        Some(RC_SYS_WSL)
    } else if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        Some(RC_SYS_WSL)
    } else if exists("/proc/sys/fs/binfmt_misc/WSLInterop") {
        Some(RC_SYS_WSL)
    } else {
        None
    }
}

fn detect_vm(systype: Option<&[u8]>) -> Option<&'static str> {
    if let Some(s) = systype {
        if s == RC_SYS_NONE.as_bytes() {
            return None;
        }
        if s == RC_SYS_XEN0.as_bytes() {
            return Some(RC_SYS_XEN0);
        }
        if s == RC_SYS_XENU.as_bytes() {
            return Some(RC_SYS_XENU);
        }
    }
    if exists("/proc/xen") {
        if file_regex("/proc/xen/capabilities", Pattern::Literal("control_d")) {
            return Some(RC_SYS_XEN0);
        }
        return Some(RC_SYS_XENU);
    }
    None
}

/// `rc_sys()`: the kind of container or VM we are running in, if any
/// (`rc_sys` in rc.conf overrides detection; `rc_sys=""` disables it).
pub fn rc_sys() -> Option<&'static str> {
    let systype = get_systype();
    detect_container(systype.as_deref()).or_else(|| detect_vm(systype.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_nonzero() {
        let p = Pattern::KeyNonZero(&["s_context", "VxID"]);
        assert!(p.matches(b"VxID:\t 3"));
        assert!(!p.matches(b"VxID:\t0"));
        assert!(!p.matches(b"VxID 3"));
        assert!(p.matches(b"xs_context:1"));
    }
}

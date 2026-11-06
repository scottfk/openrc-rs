//! The applet table.

pub mod checkpath;
pub mod einfo;

/// Where upstream installs a program (relative to the prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallDir {
    /// `$libexecdir/rc/bin`: helpers used by init scripts.
    RcBin,
    /// `$libexecdir/rc/sbin`.
    #[allow(dead_code)]
    RcSbin,
    /// `$bindir`.
    #[allow(dead_code)]
    Bin,
    /// `$sbindir`.
    #[allow(dead_code)]
    Sbin,
}

impl InstallDir {
    pub fn as_str(self) -> &'static str {
        match self {
            InstallDir::RcBin => "rc-bin",
            InstallDir::RcSbin => "rc-sbin",
            InstallDir::Bin => "bin",
            InstallDir::Sbin => "sbin",
        }
    }
}

/// An applet entry point: `(applet name, argv) -> exit status`.
pub type Main = fn(&[u8], Vec<Vec<u8>>) -> i32;

#[derive(Debug)]
pub struct Applet {
    pub name: &'static str,
    pub dir: InstallDir,
    pub main: Main,
}

const fn rc_bin(name: &'static str, main: Main) -> Applet {
    Applet { name, dir: InstallDir::RcBin, main }
}

/// Every installed program, matching upstream's install set.
pub const APPLETS: &[Applet] = &[
    rc_bin("checkpath", checkpath::main),
    // src/einfo/meson.build einfo_execs:
    rc_bin("einfon", einfo::main),
    rc_bin("einfo", einfo::main),
    rc_bin("ewarnn", einfo::main),
    rc_bin("ewarn", einfo::main),
    rc_bin("eerrorn", einfo::main),
    rc_bin("eerror", einfo::main),
    rc_bin("ebegin", einfo::main),
    rc_bin("eend", einfo::main),
    rc_bin("ewend", einfo::main),
    rc_bin("eindent", einfo::main),
    rc_bin("eoutdent", einfo::main),
    rc_bin("esyslog", einfo::main),
    rc_bin("eval_ecolors", einfo::main),
    rc_bin("ewaitfile", einfo::main),
    rc_bin("veinfo", einfo::main),
    rc_bin("vewarn", einfo::main),
    rc_bin("vebegin", einfo::main),
    rc_bin("veend", einfo::main),
    rc_bin("vewend", einfo::main),
    rc_bin("veindent", einfo::main),
    rc_bin("veoutdent", einfo::main),
];

pub fn find(name: &[u8]) -> Option<&'static Applet> {
    APPLETS.iter().find(|a| a.name.as_bytes() == name)
}

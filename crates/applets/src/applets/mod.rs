//! The applet table.

pub mod checkpath;
pub mod einfo;
pub mod fstabinfo;
pub mod kill_all;
pub mod mark_service;
pub mod mountinfo;
pub mod newer;
pub mod openrc_run;
pub mod rc_depend;
pub mod rc_service;
pub mod rc_status;
pub mod rc_update;
pub mod seedrng;
pub mod service;
pub mod shell_var;
pub mod swclock;
pub mod value;

/// Where upstream installs a program (relative to the prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallDir {
    /// `$libexecdir/rc/bin`: helpers used by init scripts.
    RcBin,
    /// `$libexecdir/rc/sbin`.
    RcSbin,
    /// `$bindir`.
    Bin,
    /// `$sbindir`.
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

const fn bin(name: &'static str, main: Main) -> Applet {
    Applet { name, dir: InstallDir::Bin, main }
}

const fn sbin(name: &'static str, main: Main) -> Applet {
    Applet { name, dir: InstallDir::Sbin, main }
}

const fn rc_sbin(name: &'static str, main: Main) -> Applet {
    Applet { name, dir: InstallDir::RcSbin, main }
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
    rc_bin("fstabinfo", fstabinfo::main),
    rc_bin("mountinfo", mountinfo::main),
    rc_bin("do_unmount", mountinfo::main),
    rc_bin("is_newer_than", newer::is_newer_than),
    rc_bin("kill_all", kill_all::main),
    rc_bin("is_older_than", newer::is_older_than),
    rc_bin("rc-depend", rc_depend::main),
    rc_bin("shell_var", shell_var::main),
    // src/service/meson.build service_execs:
    rc_bin("service_starting", service::main),
    rc_bin("service_started", service::main),
    rc_bin("service_stopping", service::main),
    rc_bin("service_stopped", service::main),
    rc_bin("service_inactive", service::main),
    rc_bin("service_wasinactive", service::main),
    rc_bin("service_hotplugged", service::main),
    rc_bin("service_started_daemon", service::main),
    rc_bin("service_crashed", service::main),
    // src/value/meson.build value_execs:
    rc_bin("service_get_value", value::main),
    rc_bin("service_set_value", value::main),
    rc_bin("service_export", value::main),
    rc_bin("get_options", value::main),
    rc_bin("save_options", value::main),
    // src/mark_service/meson.build mark_service_execs:
    rc_sbin("mark_service_starting", mark_service::main),
    rc_sbin("mark_service_started", mark_service::main),
    rc_sbin("mark_service_stopping", mark_service::main),
    rc_sbin("mark_service_stopped", mark_service::main),
    rc_sbin("mark_service_inactive", mark_service::main),
    rc_sbin("mark_service_wasinactive", mark_service::main),
    rc_sbin("mark_service_hotplugged", mark_service::main),
    rc_sbin("mark_service_failed", mark_service::main),
    rc_sbin("mark_service_crashed", mark_service::main),
    rc_sbin("seedrng", seedrng::main),
    sbin("rc-service", rc_service::main),
    sbin("rc-update", rc_update::main),
    bin("rc-status", rc_status::main),
    sbin("openrc-run", openrc_run::main),
    rc_sbin("swclock", swclock::main),
];

pub fn find(name: &[u8]) -> Option<&'static Applet> {
    APPLETS.iter().find(|a| a.name.as_bytes() == name)
}

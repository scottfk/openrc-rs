//! rc-update (Phase 3 step 3): openrc-rs vs the C program, on the state
//! fixture.  The sandbox tree is compared after each run, so the links
//! add/del/stack make (or don't) are checked as well as the output.
//!
//! Error paths come from the fixture: an executable outside /etc/init.d
//! (can't go in boot: EPERM), a script under the service directory
//! (temporary: ENOENT), a service whose runlevel entry is a dangling link
//! (symlink: EEXIST) and a runlevel entry that is a directory (unlink:
//! EISDIR).  Permission errors can't be had: the tests run as (mapped)
//! root.

mod common;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use common::system::{system, system_ns};
use common::{Case, Ns};

fn exe(path: &Path) {
    fs::write(path, "#!/sbin/openrc-run\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn upd_system_with(r: &Path, rc_conf: &str, softlevel: Option<&str>) {
    system(r);
    fs::write(r.join("rc.conf"), rc_conf).unwrap();
    // Outside /etc/init.d, by absolute path.
    exe(&r.join("conf.d/local"));
    exe(&r.join("conf.d/gone"));
    // A temporary script, under the service directory.
    exe(&r.join("run/tmpsvc"));
    // Names longer than show's 26-byte column, and exactly 26.
    exe(&r.join("init.d/a-service-name-longer-than-26"));
    exe(&r.join("init.d/exactly-twenty-six-bytes"));
    symlink("/etc/init.d/exactly-twenty-six-bytes", r.join("runlevels/nonetwork/exactly-twenty-six-bytes")).unwrap();
    // A runlevel entry that is a directory.
    fs::create_dir(r.join("runlevels/default/dirsvc")).unwrap();
    // A stacked runlevel and an extra empty one.
    symlink("../nonetwork", r.join("runlevels/default/nonetwork")).unwrap();
    fs::create_dir(r.join("runlevels/extra")).unwrap();
    match softlevel {
        Some(s) => fs::write(r.join("run/softlevel"), s).unwrap(),
        None => fs::remove_file(r.join("run/softlevel")).unwrap(),
    }
}

fn upd_plain(r: &Path) {
    upd_system_with(r, "", Some("default"));
}
fn upd_softlevel_nonetwork(r: &Path) {
    upd_system_with(r, "", Some("nonetwork\n"));
}
fn upd_softlevel_missing(r: &Path) {
    upd_system_with(r, "", Some("nosuchlevel"));
}
fn upd_no_softlevel(r: &Path) {
    upd_system_with(r, "rc_default_runlevel=\"extra\"\n", None);
}
fn upd_no_softlevel_noconf(r: &Path) {
    upd_system_with(r, "", None);
}
/// Only empty runlevels: show prints nothing unless verbose.
fn upd_empty_levels(r: &Path) {
    upd_plain(r);
    let rl = r.join("runlevels");
    fs::remove_dir_all(&rl).unwrap();
    for l in ["default", "boot"] {
        fs::create_dir_all(rl.join(l)).unwrap();
    }
}
/// No runlevels at all: `-a` finds none.
fn upd_no_levels(r: &Path) {
    upd_plain(r);
    fs::remove_dir_all(r.join("runlevels")).unwrap();
    fs::create_dir(r.join("runlevels")).unwrap();
}

/// The deptree can't be written (a directory in its place): -u fails.
fn upd_deptree_dir(r: &Path) {
    upd_plain(r);
    fs::create_dir(r.join("run/deptree")).unwrap();
}

fn ns() -> Ns {
    let mut ns = system_ns();
    ns.binds.push(("$ROOT/rc.conf".into(), "/etc/rc.conf".into()));
    ns
}

fn upd(setup: fn(&Path), args: &[&str]) -> Case {
    Case::new("rc-update", args).sandbox(setup).ns(ns()).env("TERM", "dumb")
}

#[test]
fn rc_update_show() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &[][..],
        &["show"],
        &["-v"],
        &["-v", "show"],
        &["show", "default"],
        &["show", "boot", "default"],
        &["show", "default", "default"],
        &["show", "nonetwork"],
        &["-v", "show", "extra"],
        // The first is unchecked (QUIRK); the rest must exist.
        &["show", "nosuch"],
        &["-v", "show", "nosuch"],
        &["show", "default", "nosuch"],
        &["show", "nosuch", "default"],
        &["show", ""],
        &["-a", "show"],
        &["-s", "show"],
        &["-q", "show"],
        &["bogus"],
        &["Show"],
        &["show", "-v", "default"],
        &["-h"],
        &["-V"],
        &["-x"],
        &["--bogus"],
    ] {
        cases.push(upd(upd_plain, args));
    }
    cases.extend([
        upd(upd_plain, &["show"]).env("EINFO_VERBOSE", "yes"),
        upd(upd_plain, &["show"]).env("EINFO_VERBOSE", "no"),
        upd(upd_empty_levels, &["show"]),
        upd(upd_empty_levels, &["-v"]),
        upd(upd_no_levels, &["show"]),
        upd(upd_no_levels, &["-v", "show"]),
    ]);
    common::check_all("rc-update-show", &cases);
}

#[test]
fn rc_update_add_del() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &["add"][..],
        &["del"],
        &["delete"],
        // Into the softlevel's runlevel (default).
        &["add", "delta"],
        &["add", "alpha"],
        &["add", "nosuch"],
        &["add", "notexec"],
        &["add", "functions.sh"],
        &["add", "delta", "boot", "nonetwork", "extra"],
        &["add", "delta", "nosuch"],
        &["add", "delta", "boot", "nosuch"],
        &["add", "delta", ""],
        &["add", "/etc/init.d/delta", "extra"],
        &["add", "net.eth0", "boot"],
        &["add", "/etc/conf.d/local", "default"],
        &["add", "/etc/conf.d/local", "boot"],
        &["add", "/run/openrc/tmpsvc", "extra"],
        &["add", "/etc/conf.d/gone"],
        &["add", "-a", "delta"],
        &["-a", "add", "delta", "boot"],
        &["-q", "add", "delta"],
        &["-q", "add", "nosuch"],
        &["add", "delta", "-q", "boot"],
        &["-v", "add", "delta"],
        &["add", "delta", "extra", "extra"],
        // Deleting.
        &["del", "alpha"],
        &["delete", "alpha", "default", "boot"],
        &["del", "alpha", "boot"],
        &["del", "nosuch"],
        &["del", "nosuch", "extra"],
        &["del", "gone"],
        &["del", "/etc/init.d/gamma"],
        &["del", "dirsvc"],
        &["del", "-a", "beta"],
        &["-a", "del", "nosuch"],
        &["-a", "del", "net.eth0"],
        &["del", "alpha", "default", "default"],
        &["-q", "del", "nosuch"],
        // -u acts at once, whatever follows (QUIRK).
        &["-u"],
        &["-u", "add", "delta"],
        &["add", "delta", "-u"],
        &["--update", "bogus"],
        &["-q", "-u"],
    ] {
        cases.push(upd(upd_plain, args));
    }
    cases.extend([
        upd(upd_softlevel_nonetwork, &["add", "alpha"]),
        upd(upd_softlevel_nonetwork, &["del", "delta"]),
        upd(upd_softlevel_missing, &["add", "alpha"]),
        upd(upd_softlevel_missing, &["del", "alpha"]),
        upd(upd_softlevel_missing, &["-s", "add", "extra"]),
        upd(upd_no_softlevel, &["add", "alpha"]),
        upd(upd_no_softlevel, &["del", "alpha"]),
        upd(upd_no_softlevel_noconf, &["add", "delta"]),
        upd(upd_no_softlevel_noconf, &["del", "alpha"]),
        upd(upd_no_levels, &["-a", "del", "alpha"]),
        upd(upd_no_levels, &["add", "alpha"]),
        upd(upd_deptree_dir, &["-u"]),
        upd(upd_deptree_dir, &["-q", "-u"]),
    ]);
    common::check_all("rc-update-add-del", &cases);
}

#[test]
fn rc_update_stack() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &["-s", "add", "extra"][..],
        &["-s", "add", "extra", "nonetwork"],
        &["--stack", "add", "nonetwork"],
        &["-s", "add", "nosuch"],
        &["-s", "add", "default"],
        &["-s", "add", "default", "default"],
        &["-s", "add", "boot"],
        &["-s", "add", "extra", "boot"],
        &["-s", "add", "single"],
        &["-s", "add", "shutdown", "extra"],
        &["-s", "add", "sysinit"],
        &["-s", "add", "default", "extra", "nonetwork"],
        &["-s", "del", "nonetwork"],
        &["-s", "del", "extra"],
        &["-s", "del", "dirsvc"],
        &["-s", "del", "nonetwork", "extra"],
        &["-s", "-a", "del", "nonetwork"],
        &["-s", "-a", "add", "extra"],
        &["-s", "-q", "add", "extra"],
        &["-s", "del"],
    ] {
        cases.push(upd(upd_plain, args));
    }
    cases.push(upd(upd_softlevel_nonetwork, &["-s", "add", "extra"]));
    common::check_all("rc-update-stack", &cases);
}

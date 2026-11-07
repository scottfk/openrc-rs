//! rc-status (Phase 3 step 4): openrc-rs vs the C program, on the state
//! fixture extended with every state rc-status prints.
//!
//! Uptimes depend on the clock, so both programs run with a preloaded
//! `time()` that returns `$FAKE_NOW` (both import `time` from libc).

mod common;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use common::system::{script, system, system_ns};
use common::{Case, Ns};

/// 2026-11-07 12:00:00 UTC.
const NOW: &str = "1794052800";

fn time_shim() -> &'static Path {
    static SHIM: OnceLock<PathBuf> = OnceLock::new();
    SHIM.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("faketime");
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("faketime.c");
        fs::write(
            &src,
            "#include <stdlib.h>\n#include <time.h>\n\
             time_t time(time_t *t) {\n\
             \tconst char *e = getenv(\"FAKE_NOW\");\n\
             \ttime_t v = e ? (time_t)atoll(e) : 0;\n\
             \tif (t) *t = v;\n\treturn v;\n}\n",
        )
        .unwrap();
        let so = dir.join("faketime.so");
        let ok = Command::new("cc").args(["-shared", "-fPIC", "-O2", "-o"]).arg(&so).arg(&src).status().unwrap();
        assert!(ok.success(), "building the time() shim failed");
        so
    })
}

fn opt(r: &Path, svc: &str, name: &str, value: &str) {
    let d = r.join("run/options").join(svc);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join(name), value).unwrap();
}

fn daemon(r: &Path, svc: &str, exec: &str, pid: &str) {
    let pidfile = r.join(format!("run/{svc}.pid"));
    fs::write(&pidfile, pid).unwrap();
    fs::create_dir_all(r.join("run/daemons").join(svc)).unwrap();
    fs::write(
        r.join("run/daemons").join(svc).join("001"),
        format!("exec={exec}\nargv_0={exec}\npidfile={}\n", pidfile.display()),
    )
    .unwrap();
}

fn mark(r: &Path, dir: &str, svc: &str) {
    fs::create_dir_all(r.join("run").join(dir)).unwrap();
    symlink(format!("/etc/init.d/{svc}"), r.join("run").join(dir).join(svc)).unwrap();
}

/// The state fixture plus: a stacked runlevel; services outside every
/// runlevel that are started by hand, needed or wanted by a runlevel
/// service, hotplugged, stopping, scheduled, crashed and unsupervised;
/// and start times for uptimes.
fn st_system(r: &Path) {
    system(r);
    fs::write(r.join("rc.conf"), "").unwrap();
    let initd = r.join("init.d");
    script(&initd.join("web"), "depend() {\n\tneed needed\n\twant wanted\n\tafter alpha\n}");
    for s in ["needed", "wanted", "manual", "manual2", "hot", "stopper", "sched", "crashy", "unsup", "idle"] {
        script(&initd.join(s), "");
    }
    symlink("/etc/init.d/web", r.join("runlevels/default/web")).unwrap();
    // `after` only: start order o2, o3, o1 whatever the listing order.
    script(&initd.join("o1"), "depend() {\n\tafter o3\n}");
    script(&initd.join("o3"), "depend() {\n\tafter o2\n}");
    script(&initd.join("o2"), "");
    fs::create_dir(r.join("runlevels/order")).unwrap();
    for o in ["o1", "o2", "o3"] {
        symlink(format!("/etc/init.d/{o}"), r.join("runlevels/order").join(o)).unwrap();
    }
    // Started, in sysinit only: not "manual".
    script(&initd.join("early"), "");
    symlink("/etc/init.d/early", r.join("runlevels/sysinit/early")).unwrap();
    mark(r, "started", "early");
    symlink("../nonetwork", r.join("runlevels/default/nonetwork")).unwrap();
    fs::create_dir(r.join("runlevels/extra")).unwrap();
    symlink("../single", r.join("runlevels/extra/single")).unwrap();

    for s in ["web", "needed", "wanted", "manual", "manual2", "hot", "crashy", "unsup"] {
        mark(r, "started", s);
    }
    mark(r, "hotplugged", "hot");
    mark(r, "stopping", "stopper");
    mark(r, "failed", "manual2");
    fs::create_dir_all(r.join("run/scheduled/web")).unwrap();
    symlink("/etc/init.d/sched", r.join("run/scheduled/web/sched")).unwrap();

    // Crashed (no such pid) and not crashed (pid 1) daemons.
    daemon(r, "beta", "/usr/bin/beta-daemon", "999999999");
    daemon(r, "crashy", "/usr/bin/crashy", "999999998");
    daemon(r, "unsup", "/usr/bin/unsup", "999999997");
    daemon(r, "net.lo", "/usr/bin/lo-daemon", "1");
    opt(r, "unsup", "child_pid", "999999997");
    opt(r, "unsup", "start_time", "2026-11-07 11:00:00");
    opt(r, "crashy", "start_time", "2026-11-07 11:00:00");

    // Uptimes.
    opt(r, "net.lo", "start_time", "2026-11-04 09:58:57");
    opt(r, "net.lo", "start_count", "3");
    opt(r, "net.lo", "child_pid", "1");
    opt(r, "web", "start_time", "2026-11-07 11:59:30");
    opt(r, "web", "start_count", "1");
    opt(r, "needed", "start_time", "  2026-1-7  1:2:3");
    opt(r, "needed", "start_count", "12");
    opt(r, "wanted", "start_time", "2026-11-08 00:00:00");
    opt(r, "wanted", "start_count", "1");
    opt(r, "manual", "start_time", "yesterday");
    opt(r, "manual", "start_count", "2");
    opt(r, "hot", "start_time", "2026-11-07 11:00:00");
}

/// Uptime edge cases, one service each in the default runlevel.
fn st_uptimes(r: &Path) {
    st_system(r);
    for (svc, time, count) in [
        ("alpha", "2026-11-07 12:00:00", Some("1")),
        ("gamma", "2026-11-06 12:00:00", Some("x y")),
        // Five fields: to_time_t() gives -1.
        ("net.lo", "2026-11-07 12:00", Some("4")),
        ("web", "+2026-11-07 11:00:00", Some("5")),
        ("needed", "2026-11-07 11:00:00", None),
        // No white space between date and time; a negative month.
        ("hot", "2026-11-0711:00:00", Some("6")),
        ("wanted", "2026--1-07 11:00:00", Some("7")),
    ] {
        opt(r, svc, "start_time", time);
        let _ = fs::remove_file(r.join("run/options").join(svc).join("start_count"));
        if let Some(c) = count {
            opt(r, svc, "start_count", c);
        }
    }
    // alpha and gamma started too.
    for s in ["alpha", "gamma"] {
        let _ = fs::remove_file(r.join("run/starting").join(s));
        let _ = fs::remove_file(r.join("run/failed").join(s));
        mark(r, "started", s);
    }
}

fn st_softlevel_extra(r: &Path) {
    st_system(r);
    fs::write(r.join("run/softlevel"), "extra").unwrap();
}
fn st_no_softlevel(r: &Path) {
    st_system(r);
    fs::remove_file(r.join("run/softlevel")).unwrap();
    fs::write(r.join("rc.conf"), "rc_default_runlevel=\"nonetwork\"\n").unwrap();
}
/// A deptree that can't be written or read: it is loaded per call, and
/// services are listed in plain order.
fn st_no_deptree(r: &Path) {
    st_system(r);
    fs::create_dir(r.join("run/deptree")).unwrap();
}
/// Nothing crashed, for `-c`'s exit status.
fn st_nocrash(r: &Path) {
    st_system(r);
    for s in ["beta", "crashy", "unsup"] {
        fs::write(r.join(format!("run/{s}.pid")), "1").unwrap();
    }
}

fn ns() -> Ns {
    let mut ns = system_ns();
    ns.binds.push(("$ROOT/rc.conf".into(), "/etc/rc.conf".into()));
    ns
}

fn st(setup: fn(&Path), args: &[&str]) -> Case {
    Case::new("rc-status", args)
        .sandbox(setup)
        .ns(ns())
        .env("TERM", "dumb")
        .env("FAKE_NOW", NOW)
        .env("LD_PRELOAD", time_shim().to_str().unwrap())
}

#[test]
fn rc_status_levels() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &[][..],
        &["default"],
        &["boot"],
        &["boot", "default"],
        &["default", "default"],
        &["nosuch"],
        &["nosuch", "boot"],
        &["boot", "nosuch"],
        &["nonetwork"],
        &["extra"],
        &["single"],
        &["order"],
        &["-a"],
        &["--all"],
        &["-a", "boot"],
        &["-a", "-a"],
        &["-f", "ini"],
        &["--format=INI", "-a"],
        &["-f", "json"],
        &["-f", "ini", "boot"],
        &["-q"],
        &["-v", "boot"],
        &["-C"],
        &["-h"],
        &["-V"],
        &["-x"],
        &["--bogus"],
    ] {
        cases.push(st(st_system, args));
    }
    cases.extend([
        st(st_softlevel_extra, &[]),
        st(st_softlevel_extra, &["-a"]),
        st(st_no_softlevel, &[]),
        st(st_uptimes, &["default"]),
        st(st_uptimes, &["-a"]),
        st(st_uptimes, &["-f", "ini", "default"]),
        st(st_uptimes, &[]).env("FAKE_NOW", "0"),
        st(st_no_deptree, &[]),
        st(st_no_deptree, &["-s"]),
        st(st_no_deptree, &["-f", "ini"]),
        st(st_system, &[]).env("COLUMNS", "40"),
        st(st_system, &[]).env("RC_SVCNAME", "web"),
    ]);
    common::check_all("rc-status-levels", &cases);
}

#[test]
fn rc_status_modes() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &["-c"][..],
        &["--crashed", "-l"],
        &["-l"],
        &["-l", "-c"],
        &["-r"],
        &["--runlevel", "boot"],
        &["-s"],
        &["-s", "-f", "ini"],
        &["-f", "ini", "-s"],
        &["-S"],
        &["--supervised"],
        &["-u"],
        &["-f", "ini", "-u"],
        &["-m"],
        &["--manual"],
        &["-f", "ini", "-m"],
    ] {
        cases.push(st(st_system, args));
    }
    cases.extend([
        st(st_nocrash, &["-c"]),
        st(st_no_softlevel, &["-r"]),
        st(st_softlevel_extra, &["-r"]),
        st(st_uptimes, &["-s"]),
    ]);
    common::check_all("rc-status-modes", &cases);
}

#[test]
fn rc_status_in_state() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &["-i", "started"][..],
        &["-i", "stopped"],
        &["-i", "starting"],
        &["-i", "stopping"],
        &["-i", "inactive"],
        &["-i", "hotplugged"],
        &["-i", "failed"],
        &["-i", "scheduled"],
        &["-i", "crashed"],
        &["-i", "wasinactive"],
        &["-i", "nosuch"],
        &["-i", "Started"],
        &["-i", "started", "-i", "crashed"],
        &["-i", "crashed", "-i", "started"],
        &["-i", "stopped", "-i", "failed"],
        &["-i", "failed", "-i", "stopped"],
        &["--in-state=started", "-a"],
        &["-i", "started", "boot"],
        &["-i", "started", "-s"],
        &["-i", "started", "-f", "ini"],
    ] {
        cases.push(st(st_system, args));
    }
    common::check_all("rc-status-in-state", &cases);
}

/// On a terminal: the runlevel names are highlighted and each status is
/// right-aligned with colour.
#[test]
fn rc_status_pty() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        st(st_system, &[]).env("TERM", "xterm").pty(),
        st(st_system, &["-a"]).env("TERM", "xterm").pty(),
        st(st_system, &["-C"]).env("TERM", "xterm").pty(),
        st(st_system, &[]).env("TERM", "xterm").env("EINFO_COLOR", "NO").pty(),
        st(st_system, &[]).pty(),
        st(st_system, &["-f", "ini"]).env("TERM", "xterm").pty(),
        st(st_uptimes, &["-s"]).env("TERM", "xterm").pty(),
    ];
    common::check_all("rc-status-pty", &cases);
}

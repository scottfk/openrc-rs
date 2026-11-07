//! rc-service and the service_* helpers (Phase 3 step 2): openrc-rs vs
//! the C programs, on the state fixture.
//!
//! The fixture adds `envdump`, an executable that isn't an openrc-run
//! script: rc-service execs it directly, so it shows exactly the argv and
//! environment rc-service hands over (after env_filter and env_config),
//! under different rc.conf, profile.env, krunlevel and PATH settings.
//! Real openrc-run scripts are covered too, run by the host's C
//! openrc-run.

mod common;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::system::{system, system_ns};
use common::{Case, Ns};

const ENVDUMP: &str = "#!/bin/sh\necho \"argv0=$0\"\nfor a; do echo \"arg=[$a]\"; done\nenv | LC_ALL=C sort\n";

fn env_system_with(r: &Path, rc_conf: &str, profile: &str, krunlevel: Option<&str>) {
    system(r);
    let f = r.join("init.d/envdump");
    fs::write(&f, ENVDUMP).unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(r.join("rc.conf"), rc_conf).unwrap();
    fs::write(r.join("profile.env"), profile).unwrap();
    // Daemon records, with pidfiles: without one, the crash check scans
    // /proc and its result depends on errno left by whatever processes
    // happen to exist (see QUIRKS.md), which differs between the runs.
    // beta's daemon is gone (no such pid): crashed.  net.lo's is pid 1,
    // which exists (kill() fails with EPERM, not ESRCH): not crashed.
    for (svc, exec, pid) in [("beta", "/usr/bin/beta-daemon", "999999999"), ("net.lo", "/usr/bin/lo-daemon", "1")] {
        let pidfile = r.join(format!("run/{svc}.pid"));
        fs::write(&pidfile, pid).unwrap();
        fs::create_dir_all(r.join("run/daemons").join(svc)).unwrap();
        fs::write(
            r.join("run/daemons").join(svc).join("001"),
            format!("exec={exec}\nargv_0={exec}\npidfile={}\n", pidfile.display()),
        )
        .unwrap();
    }
    if let Some(k) = krunlevel {
        fs::write(r.join("run/krunlevel"), k).unwrap();
    }
}

const PROFILE: &str = "export PROFVAR='from profile'\nexport LANG='profile-lang'\nexport OTHER=\"x y\"\n";

fn env_plain(r: &Path) {
    env_system_with(r, "", PROFILE, None);
}
fn env_allow_some(r: &Path) {
    env_system_with(r, "rc_env_allow=\"FOO B*  .hid *dot\"\n", PROFILE, None);
}
fn env_allow_all(r: &Path) {
    env_system_with(r, "rc_env_allow=\"BAR *\"\n", PROFILE, None);
}
/// PATH isn't on env_filter's allow list, so only with rc_env_allow does
/// env_config get to merge the caller's PATH.
fn env_allow_path(r: &Path) {
    env_system_with(r, "rc_env_allow=\"PATH\"\n", "", None);
}
fn env_quiet(r: &Path) {
    env_system_with(r, "rc_quiet=\"YES\"\nrc_verbose=\"yes\"\nrc_color=\"no\"\n", "", None);
}
fn env_color_maybe(r: &Path) {
    env_system_with(r, "rc_color=\"maybe\"\nrc_quiet=\"no\"\n", "", None);
}
fn env_color_yes_nocolor(r: &Path) {
    env_system_with(r, "rc_color=\"yes\"\nrc_nocolor=\"1\"\n", "", None);
}
fn env_krunlevel(r: &Path) {
    env_system_with(r, "rc_default_runlevel=\"boot\"\n", "", Some("single\nsecond line\n"));
}
fn env_krunlevel_empty(r: &Path) {
    env_system_with(r, "", "", Some(""));
}
fn env_no_softlevel(r: &Path) {
    env_system_with(r, "rc_default_runlevel=\"nonetwork\"\n", "", None);
    fs::remove_file(r.join("run/softlevel")).unwrap();
}

fn ns() -> Ns {
    let mut ns = system_ns();
    ns.binds.push(("$ROOT/rc.conf".into(), "/etc/rc.conf".into()));
    if Path::new("/etc/profile.env").exists() {
        ns.binds.push(("$ROOT/profile.env".into(), "/etc/profile.env".into()));
    }
    ns
}

fn rcs(setup: fn(&Path), args: &[&str]) -> Case {
    Case::new("rc-service", args).sandbox(setup).ns(ns()).env("TERM", "dumb")
}

/// What reaches the exec'd program.
#[test]
fn rc_service_environment() {
    if common::reference_dir().is_none() {
        return;
    }
    let e = |setup: fn(&Path)| rcs(setup, &["envdump", "start", "x y", ""]);
    let cases = vec![
        e(env_plain),
        e(env_plain)
            .env("FOO", "1")
            .env("BAR", "2")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("LC_", "x")
            .env("XDG_X", "1")
            .env("HOME", "/h")
            .env("RC_SVCDIR", "/elsewhere")
            .env("EINFO_QUIET", "yes")
            .env("EERROR_QUIET", "yes")
            .env("IN_HOTPLUG", "1")
            .env("PROFVAR", "already"),
        e(env_allow_some)
            .env("FOO", "1")
            .env("BAR", "2")
            .env("BAZ", "3")
            .env("FOOD", "4")
            .env(".hid", "5")
            .env(".dot", "6")
            .env("xdot", "7"),
        e(env_allow_all).env("FOO", "1").env("ZZZ", "2"),
        e(env_quiet),
        e(env_color_maybe),
        e(env_color_yes_nocolor),
        e(env_krunlevel),
        e(env_krunlevel_empty),
        e(env_no_softlevel),
        // PATH is dropped by env_filter (QUIRK)...
        e(env_plain).env("PATH", "/x:/usr/bin"),
        // ...unless allowed: then prefixed with the system directories,
        // without repeats.
        e(env_allow_path).env("PATH", ""),
        e(env_allow_path).env("PATH", "/usr/bin"),
        e(env_allow_path).env("PATH", "/x::/bin:/y:/usr/libexec/rc/bin:/x"),
        e(env_allow_path).env("PATH", "/usr/libexec/rc/bin:/bin:/sbin:/usr/bin:/usr/sbin"),
        e(env_allow_path).env("PATH", "/usr/libexec/rc/bin:/bin:/sbin:/usr/bin:/usr/sbinX:/z"),
        e(env_allow_path).env("PATH", "/usr/libexec/rc/bin"),
        // Options that set variables.
        rcs(env_plain, &["-d", "-D", "-Z", "envdump", "a"]),
        rcs(env_plain, &["--debug", "--nodeps", "--dry-run", "envdump"]),
        rcs(env_plain, &["-q", "envdump"]),
        rcs(env_plain, &["-q", "-q", "envdump"]),
        rcs(env_plain, &["-v", "-C", "envdump"]),
        rcs(env_plain, &["envdump", "-x", "--", "y"]),
        rcs(env_plain, &["/etc/init.d/envdump", "stop"]),
    ];
    common::check_all("rc-service-env", &cases);
}

#[test]
fn rc_service_options() {
    if common::reference_dir().is_none() {
        return;
    }
    let r = |args: &[&str]| rcs(env_plain, args);
    let mut cases = Vec::new();
    for args in [
        &[][..],
        &["-e", "alpha"],
        &["-e", "nosuch"],
        &["--exists=net.eth0"],
        &["-e", "functions.sh"],
        &["-e", "/etc/init.d/nosuch"],
        &["-e", "nosuch", "-l"],
        &["-r", "alpha"],
        &["-r", "net.eth0"],
        &["-r", "nosuch"],
        &["--resolve", "/abs/path"],
        &["-l"],
        &["--list", "-e", "nosuch"],
        &["-i", "nosuch", "start"],
        &["--ifexists", "envdump"],
        &["nosuch", "start"],
        &["notexec", "start"],
        // State conditions (beta started, alpha starting, gamma failed,
        // delta inactive, epsilon stopped).
        &["-s", "envdump"],
        &["-S", "envdump"],
        &["-N", "envdump"],
        &["-I", "envdump"],
        &["-c", "envdump"],
        &["-s", "beta", "status"],
        &["-S", "beta", "status"],
        &["-N", "beta", "status"],
        &["-I", "delta", "status"],
        &["-I", "alpha", "status"],
        &["-S", "epsilon", "status"],
        &["-N", "alpha", "status"],
        &["-N", "delta", "status"],
        &["-s", "alpha", "status"],
        &["-c", "beta", "status"],
        &["-s", "-S", "beta", "status"],
        &["alpha", "describe"],
        &["beta", "status"],
        &["delta", "status"],
        &["gamma", "status"],
        &["beta"],
        &["-x"],
        &["--bogus"],
        &["-e"],
        &["-h"],
        &["-V"],
        &["-V", "envdump"],
        &["-U", "-l"],
    ] {
        cases.push(r(args));
    }
    common::check_all("rc-service-opts", &cases);
}

fn svc(applet: &'static str, args: &[&str]) -> Case {
    Case::new(applet, args).sandbox(env_plain).ns(ns())
}

#[test]
fn service_helpers() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut cases = Vec::new();
    for st in ["started", "starting", "stopping", "stopped", "inactive", "wasinactive", "hotplugged", "crashed"] {
        let applet: &'static str = Box::leak(format!("service_{st}").into_boxed_str());
        for s in ["alpha", "beta", "gamma", "delta", "epsilon", "net.eth0", "nosuch", "/etc/init.d/beta"] {
            cases.push(svc(applet, &[s]));
        }
        cases.push(svc(applet, &[]).env("RC_SVCNAME", "beta"));
        cases.push(svc(applet, &[]));
        cases.push(svc(applet, &[""]).env("RC_SVCNAME", "beta"));
    }
    cases.extend([
        svc("service_started_daemon", &[]),
        svc("service_started_daemon", &["beta", "/usr/bin/beta-daemon"]),
        svc("service_started_daemon", &["beta", "/usr/bin/beta-daemon", "1"]),
        svc("service_started_daemon", &["beta", "/usr/bin/beta-daemon", "2"]),
        svc("service_started_daemon", &["beta", "/usr/bin/beta-daemon", "x"]),
        svc("service_started_daemon", &["beta", "/usr/bin/other"]),
        svc("service_started_daemon", &["/usr/bin/beta-daemon"]).env("RC_SVCNAME", "beta"),
        svc("service_started_daemon", &["/usr/bin/beta-daemon", "1"]).env("RC_SVCNAME", "beta"),
        svc("service_started_daemon", &["/usr/bin/beta-daemon", "007"]).env("RC_SVCNAME", "beta"),
        svc("service_started_daemon", &["beta", "/usr/bin/beta-daemon"]).env("RC_SVCNAME", "alpha"),
        svc("service_started_daemon", &["nosuch", "/usr/bin/beta-daemon"]),
        svc("service_started_daemon", &["net.lo", "/usr/bin/lo-daemon", "1"]),
    ]);
    common::check_all("service-helpers", &cases);
}

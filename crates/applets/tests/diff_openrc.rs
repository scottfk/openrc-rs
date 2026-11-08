//! openrc, the runlevel engine (Phase 5 step 1): openrc vs the C program.
//!
//! The fixture is the openrc-run one (common/run.rs).  The services that
//! openrc stops and starts are run by the C openrc-run in both runs (bound
//! over /sbin/openrc-run), with the fake openrc-run.sh printing a line per
//! call, so only openrc itself differs.  openrc filters its environment,
//! so the setups let the fake's `START_*`/`STOP_*` variables through
//! (`rc_env_allow`).
//!
//! In the fixture the runlevel is default (alpha, gamma, net.eth0 and a
//! dangling entry); boot holds beta and net.lo, nonetwork delta, single
//! epsilon.  beta, net.lo and localmount are started, alpha starting,
//! gamma failed, delta and kappa inactive.

mod common;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use common::Case;
use common::run::*;
use common::system::script;

const ALLOW: &str = "rc_env_allow=\"START_* STOP_* PLUG_MODE\"\n";

fn with(r: &Path, conf: &str) {
    start_with(r, &format!("{ALLOW}{conf}"));
    // keepme never stops, noshut not on the way down; both are started.
    script(&r.join("init.d/keepme"), "depend() {\n\tkeyword -stop\n}");
    script(&r.join("init.d/noshut"), "depend() {\n\tkeyword -shutdown\n}");
    // tau provides net and sigma uses it; upsilon is a sysinit service: all
    // started.  rho (no deps, in no runlevel) is starting; phi (stopped) is
    // in nonetwork.
    script(&r.join("init.d/tau"), "depend() {\n\tprovide net\n}");
    script(&r.join("init.d/sigma"), "depend() {\n\tuse net\n}");
    script(&r.join("init.d/upsilon"), "");
    script(&r.join("init.d/rho"), "");
    script(&r.join("init.d/phi"), "");
    for s in ["keepme", "noshut", "tau", "sigma", "upsilon"] {
        symlink(format!("/etc/init.d/{s}"), r.join("run/started").join(s)).unwrap();
    }
    symlink("/etc/init.d/rho", r.join("run/starting/rho")).unwrap();
    symlink("/etc/init.d/upsilon", r.join("runlevels/sysinit/upsilon")).unwrap();
    symlink("/etc/init.d/phi", r.join("runlevels/nonetwork/phi")).unwrap();
}
fn plain(r: &Path) {
    with(r, "");
}
/// In parallel the services run at once, so each takes a different time
/// (0.5s apart) to make the output order deterministic.
fn parallel(r: &Path) {
    with(r, "rc_parallel=\"YES\"\n");
    let svcs = [
        "alpha",
        "beta",
        "delta",
        "epsilon",
        "gamma",
        "kappa",
        "localmount",
        "net.eth0",
        "net.lo",
        "noshut",
        "phi",
        "rho",
        "sigma",
        "tau",
    ];
    for (i, s) in svcs.iter().enumerate() {
        fs::write(r.join(format!("run/slow.{s}")), format!("{}", 0.1 + 0.5 * i as f64)).unwrap();
    }
}
fn nostop(r: &Path) {
    with(r, "rc_nostop=\"beta localmount\"\n");
}
fn not_strict(r: &Path) {
    with(r, "rc_depend_strict=\"NO\"\n");
}
/// default stacks nonetwork.
fn stacked(r: &Path) {
    plain(r);
    symlink("../nonetwork", r.join("runlevels/default/nonetwork")).unwrap();
}
/// The next runlevel is overridden to nonetwork.
fn krunlevel(r: &Path) {
    plain(r);
    fs::write(r.join("run/krunlevel"), "nonetwork\n").unwrap();
}
fn krunlevel_bad(r: &Path) {
    plain(r);
    fs::write(r.join("run/krunlevel"), "nosuch").unwrap();
}
/// beta has a default runlevel configuration, so it's restarted.
fn runlevel_conf_current(r: &Path) {
    plain(r);
    symlink("/etc/init.d/beta", r.join("runlevels/nonetwork/beta")).unwrap();
    fs::write(r.join("conf.d/beta.default"), "").unwrap();
}
/// net.eth0 is in nonetwork too: when default stacks it, a failure there
/// isn't retried.
fn stacked_twice(r: &Path) {
    stacked(r);
    symlink("/etc/init.d/net.eth0", r.join("runlevels/nonetwork/net.eth0")).unwrap();
}
/// gamma's lock can't be taken (a directory): starting stops there.
fn gamma_unlockable(r: &Path) {
    plain(r);
    fs::create_dir(r.join("run/exclusive/gamma")).unwrap();
}
/// Left from an interactive boot (with -q, not interactive).
fn interactive(r: &Path) {
    plain(r);
    fs::write(r.join("run/interactive"), "").unwrap();
}
fn lxc(r: &Path) {
    with(r, "rc_sys=\"lxc\"\n");
}
/// beta has a nonetwork runlevel configuration, so it's restarted.
fn runlevel_conf(r: &Path) {
    plain(r);
    symlink("/etc/init.d/beta", r.join("runlevels/nonetwork/beta")).unwrap();
    fs::write(r.join("conf.d/beta.nonetwork"), "").unwrap();
}
fn hotplugged(r: &Path) {
    plain(r);
    fs::create_dir_all(r.join("run/hotplugged")).unwrap();
    symlink("/etc/init.d/kappa", r.join("run/hotplugged/kappa")).unwrap();
}
/// Leftovers: a failed entry that can't be unlinked (a directory), a skewed
/// clock, the old config file.
fn leftovers(r: &Path) {
    plain(r);
    fs::create_dir(r.join("run/failed/gone")).unwrap();
    fs::write(r.join("run/clock-skewed"), "").unwrap();
    fs::write(r.join("conf.d/rc"), "").unwrap();
}
fn still_stopping(r: &Path) {
    plain(r);
    fs::create_dir(r.join("run/rc.stopping")).unwrap();
}
fn no_deptree(r: &Path) {
    plain(r);
    fs::remove_file(r.join("run/deptree")).ok();
    symlink("deptree", r.join("run/deptree")).unwrap();
}
/// A daemon record whose process is gone (pid 999999), for `svc`.
fn crashed_daemon(r: &Path, svc: &str) {
    fs::write(r.join(format!("run/{svc}.pid")), "999999").unwrap();
    fs::create_dir_all(r.join("run/daemons").join(svc)).unwrap();
    fs::write(
        r.join("run/daemons").join(svc).join("001"),
        format!("exec=/usr/sbin/{svc}d\nargv_0=/usr/sbin/{svc}d\npidfile=/run/openrc/{svc}.pid\n"),
    )
    .unwrap();
}
/// alpha's daemon crashed: started again (rc_crashed_start defaults to on).
fn crashed(r: &Path) {
    plain(r);
    crashed_daemon(r, "alpha");
}
fn crashed_no_start(r: &Path) {
    with(r, "rc_crashed_start=\"NO\"\n");
    crashed_daemon(r, "alpha");
}
/// beta's crashed and rc_crashed_stop: stopped even though it's wanted.
fn crashed_stop(r: &Path) {
    with(r, "rc_crashed_stop=\"YES\"\nrc_crashed_start=\"NO\"\n");
    crashed_daemon(r, "beta");
}
/// A user runlevel, for `-U`.
fn user_runlevel(r: &Path) {
    plain(r);
    fs::create_dir_all(r.join(".config/rc/runlevels/default")).unwrap();
}
fn plugins(r: &Path) {
    plain(r);
    let d = r.join("run/plugins.d");
    fs::create_dir_all(&d).unwrap();
    fs::copy(plugin_so(), d.join("test.so")).unwrap();
}

#[test]
fn openrc_runlevels() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases: Vec<Case> = vec![
        // The current runlevel, then changes: what isn't wanted stops (last
        // first), the new runlevel's services start.
        rc(plain, &[]),
        rc(plain, &["default"]),
        rc(plain, &["nonetwork"]),
        rc(plain, &["single"]),
        rc(plain, &["boot"]),
        rc(plain, &["loops"]),
        rc(plain, &["shutdown"]),
        rc(plain, &["reboot"]),
        rc(plain, &["nosuch"]),
        rc(plain, &["nonetwork", "ignored"]),
        rc(plain, &["-n", "nonetwork"]),
        rc(plain, &["--no-stop", "shutdown"]),
        rc(plain, &["-q", "nonetwork"]),
        rc(plain, &["-v", "single"]),
        rc(parallel, &["nonetwork"]),
        rc(parallel, &[]),
        rc(nostop, &["single"]),
        rc(not_strict, &["nonetwork"]),
        rc(stacked, &[]),
        rc(stacked, &["single"]),
        rc(runlevel_conf, &["nonetwork"]),
        rc(runlevel_conf_current, &["nonetwork"]),
        rc(stacked_twice, &[]).env("START_FAIL", "net.eth0"),
        rc(gamma_unlockable, &[]),
        rc(interactive, &["-q"]),
        rc(hotplugged, &["nonetwork"]),
        rc(hotplugged, &["single"]),
        // A zap while stopping drops the hotplugged mark; openrc puts it back.
        rc(hotplugged, &["single"]).env("STOP_RUN", "kappa:kappa,zap"),
        rc(leftovers, &[]),
        rc(still_stopping, &["nonetwork"]),
        rc(no_deptree, &["nonetwork"]),
        // Crashed daemons.
        rc(crashed, &[]),
        rc(crashed_no_start, &[]),
        rc(crashed_stop, &[]),
        // The krunlevel override: taken (and removed) unless going to boot.
        rc(krunlevel, &[]),
        rc(krunlevel, &["single"]),
        rc(krunlevel, &["boot"]),
        rc(krunlevel_bad, &[]),
        // Services fail, or stop us.
        rc(plain, &["nonetwork"]).env("STOP_FAIL", "noshut"),
        rc(plain, &[]).env("START_FAIL", "net.eth0"),
        rc(plain, &["single"]).env("STOP_FAIL", "localmount"),
        rc(plain, &[]).env("START_SIG", "net.eth0:USR1"),
        rc(plain, &[]).env("START_SIG", "net.eth0:TERM"),
        rc(plain, &["nonetwork"]).env("STOP_SIG", "localmount:INT"),
        rc(plain, &["nonetwork"]).env("STOP_SIG", "noshut:QUIT"),
        rc(plain, &["single"]).env("STOP_SIG", "localmount:USR1"),
    ];
    common::check_all("openrc", &cases);
}

#[test]
fn openrc_options() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases: Vec<Case> = vec![
        rc(plain, &["-S"]),
        rc(plain, &["--sys"]),
        rc(lxc, &["-S"]),
        rc(plain, &["-h"]),
        rc(plain, &["-V"]),
        rc(plain, &["-a", "x"]),
        rc(plain, &["-x"]),
        // --override.  (QUIRK: without RC_BOOTLEVEL C crashes; the
        // reference is a debug build, which runs gdb instead, so that
        // isn't compared.)
        rc(plain, &["-o", "nonetwork"]).env("RC_BOOTLEVEL", "boot"),
        rc(plain, &["-o", "boot"]).env("RC_BOOTLEVEL", "boot"),
        rc(krunlevel, &["-o", "single"]).env("RC_BOOTLEVEL", "boot"),
        rc(plain, &["--override", "nosuch"]),
        rc(plain, &["-o", ""]),
        rc(user_runlevel, &["-U", "-o", "default"]).env("HOME", "$ROOT").env("XDG_RUNTIME_DIR", "$ROOT/xdg"),
        // --service: exec the script with the rest of the arguments.
        rc(plain, &["-s", "beta", "describe"]),
        rc(plain, &["-s", "net.eth0", "start"]),
        rc(plain, &["--service", "alpha", "ineed", "iuse"]),
        rc(plain, &["-snet.eth0", "status"]),
        rc(plain, &["-v", "-s", "beta", "-q", "status"]),
        rc(plain, &["foo", "-s", "beta", "status"]),
        rc(plain, &["-s", "nosuch", "start"]),
        rc(plain, &["-s", "notexec", "start"]),
        rc(plain, &["-U", "sysinit"]).env("HOME", "$ROOT").env("XDG_RUNTIME_DIR", "$ROOT/xdg"),
        rc(plain, &["-U", "-s", "beta", "status"]).env("HOME", "$ROOT").env("XDG_RUNTIME_DIR", "$ROOT/xdg"),
    ];
    common::check_all("openrc-options", &cases);
}

#[test]
fn openrc_plugins() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        rc(plugins, &[]),
        rc(plugins, &["nonetwork"]),
        rc(plugins, &["shutdown"]),
        rc(plugins, &["nosuch"]),
        rc(plugins, &[]).env("START_SIG", "net.eth0:USR1"),
    ];
    common::check_all("openrc-plugins", &cases);
}

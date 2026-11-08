//! openrc-run stop, restart and condrestart (Phase 4 step 3): openrc-run
//! vs the C program.
//!
//! Same fixture as the start tests (common/run.rs): `/sbin/openrc-run`
//! inside the namespace is the program under test, so the services that a
//! stop takes down (what needs us) and a restart brings back are run by
//! the same implementation.  In the fixture beta is started and needed by
//! alpha (starting), the inactive delta (rc_need) and, through virt,
//! gamma (failed); net.lo and localmount are started too.

mod common;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::system::script;

use common::Case;
use common::run::*;

fn sp_sysinit(r: &Path) {
    st_plain(r);
    symlink("/etc/init.d/beta", r.join("runlevels/sysinit/beta")).unwrap();
}
/// The runlevel is stopping, and delta (inactive) failed.
fn sp_stopping(r: &Path) {
    st_plain(r);
    fs::write(r.join("run/rc.stopping"), "").unwrap();
    symlink("/etc/init.d/delta", r.join("run/failed/delta")).unwrap();
}
fn sp_hotplugged(r: &Path) {
    st_plain(r);
    fs::create_dir_all(r.join("run/hotplugged")).unwrap();
    symlink("/etc/init.d/beta", r.join("run/hotplugged/beta")).unwrap();
}
fn sp_wasinactive(r: &Path) {
    st_plain(r);
    fs::create_dir_all(r.join("run/wasinactive")).unwrap();
    symlink("/etc/init.d/beta", r.join("run/wasinactive/beta")).unwrap();
}
/// net.lo's lock file is a directory (EISDIR).
fn sp_no_exclusive(r: &Path) {
    st_plain(r);
    fs::create_dir(r.join("run/exclusive/net.lo")).unwrap();
}
/// pi and sigma (started) need beta; tau (started) uses it.
fn sp_dependents(r: &Path) {
    st_plain(r);
    for (svc, dep) in [("pi", "need beta"), ("sigma", "need beta"), ("tau", "use beta")] {
        script(&r.join("init.d").join(svc), &format!("depend() {{\n\t{dep}\n}}"));
        symlink(format!("/etc/init.d/{svc}"), r.join("run/started").join(svc)).unwrap();
    }
}
fn sp_dependents_stopping(r: &Path) {
    sp_dependents(r);
    fs::write(r.join("run/rc.stopping"), "").unwrap();
}
/// Hold `svc`'s exclusive lock from another process for a few seconds.
fn hold_lock(r: &Path, svc: &str) {
    let f = r.join("run/exclusive").join(svc);
    let mut holder = Command::new("flock")
        .arg("-x")
        .arg(&f)
        .args(["sleep", "3"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Reap it once it's done.
    std::thread::spawn(move || holder.wait());
    let t = Instant::now();
    while Command::new("flock").arg("-n").arg(&f).arg("true").status().unwrap().success() {
        assert!(t.elapsed() < Duration::from_secs(2), "flock never took {}", f.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn sp_locked_pi(r: &Path) {
    sp_dependents(r);
    hold_lock(r, "pi");
}
fn sp_locked_tau(r: &Path) {
    sp_dependents(r);
    hold_lock(r, "tau");
}
fn sp_hotplugged_stopping(r: &Path) {
    sp_hotplugged(r);
    fs::write(r.join("run/rc.stopping"), "").unwrap();
}
fn sp_plugins_no_deptree(r: &Path) {
    st_plugins(r);
    symlink("deptree", r.join("run/deptree")).unwrap();
}
/// For restarts: delta, which they bring back in the background, is slow,
/// so we have exited (flushing our output) before it prints.
fn sp_restart(r: &Path) {
    st_plain(r);
    fs::write(r.join("run/slow.delta"), "").unwrap();
}
fn sp_restart_parallel(r: &Path) {
    st_parallel(r);
    fs::write(r.join("run/slow.delta"), "").unwrap();
}
/// delta is slow to stop, so in parallel beta waits for it.
fn sp_parallel_slow(r: &Path) {
    st_parallel(r);
    // Outlasts the (shimmed, 600ms) wait.
    fs::write(r.join("run/slow.delta"), "2").unwrap();
}

#[test]
fn openrc_run_stop() {
    if common::reference_dir().is_none() {
        return;
    }
    let fast = alarm_shim().to_str().unwrap();
    let cases: Vec<Case> = vec![
        // beta: what needs it is stopped first (delta; alpha is starting,
        // gamma failed).  net.lo and beta are boot services.
        st(st_plain, "beta", &["stop"]),
        st(st_plain, "net.lo", &["stop"]),
        st(st_plain, "localmount", &["stop"]),
        st(st_plain, "delta", &["stop"]),
        st(st_plain, "alpha", &["stop"]),
        st(st_plain, "beta", &["stop", "stop"]),
        st(st_plain, "beta", &["stop", "start"]),
        st(st_plain, "beta", &["-q", "stop"]),
        st(st_strict_no, "beta", &["stop"]),
        st(st_parallel, "beta", &["stop"]),
        st(sp_sysinit, "beta", &["stop"]),
        // Already stopped.
        st(st_plain, "net.eth0", &["stop"]),
        st(st_plain, "gamma", &["stop"]),
        // Without deps: nothing else stopped, no reexports taken.
        st(st_plain, "beta", &["-D", "stop"]),
        st(st_plain, "alpha", &["--nodeps", "stop"]),
        st(st_plain, "alpha", &["-D", "ineed", "stop"]),
        st(st_plain, "beta", &["stop"]).env("RC_NODEPS", "yes"),
        // Dry runs.
        st(st_plain, "beta", &["-Z", "stop"]),
        st(st_plain, "net.eth0", &["-Z", "stop"]),
        st(st_plain, "beta", &["-Z", "-D", "stop"]),
        st(st_plain, "beta", &["-Z", "restart"]),
        // In the background: started becomes inactive, and what we stopped
        // is scheduled to start with us; inactive is refused (QUIRK).
        st(st_plain, "beta", &["stop"]).env("IN_BACKGROUND", "yes"),
        st(st_plain, "net.lo", &["stop"]).env("IN_BACKGROUND", "yes"),
        st(st_plain, "delta", &["stop"]).env("IN_BACKGROUND", "yes"),
        st(st_plain, "net.eth0", &["stop"]).env("IN_BACKGROUND", "yes"),
        st(st_plain, "beta", &["-D", "stop"]).env("IN_BACKGROUND", "yes"),
        st(sp_hotplugged, "beta", &["stop"]),
        st(sp_hotplugged, "beta", &["stop"]).env("IN_BACKGROUND", "yes"),
        st(sp_hotplugged, "beta", &["-D", "stop"]),
        st(sp_wasinactive, "beta", &["stop"]),
        // The script fails, or marks itself.
        st(st_plain, "beta", &["stop"]).env("STOP_FAIL", "beta"),
        st(st_plain, "beta", &["stop"]).env("STOP_FAIL", "delta"),
        st(st_plain, "beta", &["stop"]).env("STOP_MARK", "beta:started"),
        st(st_plain, "beta", &["stop"]).env("STOP_MARK", "delta:started"),
        st(st_plain, "beta", &["stop"]).env("STOP_MARK", "beta:inactive"),
        // The lock: a nested stop of ourselves while stopping (with
        // --nodeps that's fine), after marking ourselves started (which
        // drops the lock file), and while starting.
        st(st_plain, "beta", &["stop"]).env("STOP_RUN", "beta:beta,stop"),
        st(st_plain, "beta", &["stop"]).env("STOP_RUN", "beta:beta,--nodeps,stop"),
        st(st_plain, "beta", &["stop"]).env("STOP_MARK", "beta:started").env("STOP_RUN", "beta:beta,stop"),
        st(st_plain, "net.eth0", &["start"]).env("START_RUN", "net.eth0:net.eth0,stop"),
        st(sp_no_exclusive, "net.lo", &["stop"]),
        st(st_no_deptree, "beta", &["stop"]),
        st(st_no_deptree, "beta", &["-D", "stop"]),
        st(st_plain, "beta", &["-l", "99", "stop"]),
        // The runlevel is stopping: without RC_PID no deps; failed
        // services aren't stopped (again); a dependent that won't stop
        // fails us, unless shutting down.
        st(sp_stopping, "beta", &["stop"]),
        st(sp_stopping, "beta", &["stop"]).env("RC_PID", "1"),
        st(sp_stopping, "delta", &["stop"]).env("RC_PID", "1"),
        st(sp_stopping, "net.lo", &["stop"]).env("RC_PID", "1").env("STOP_MARK", "net.lo:started"),
        st(st_plain, "net.lo", &["stop"]).env("STOP_FAIL", "net.lo"),
        st(sp_stopping, "alpha", &["stop"]).env("RC_PID", "1").env("STOP_FAIL", "epsilon"),
        st(sp_stopping, "net.lo", &["stop"]).env("RC_PID", "1").env("STOP_FAIL", "net.lo"),
        st(sp_stopping, "beta", &["stop"]).env("RC_PID", "1").env("STOP_FAIL", "alpha"),
        st(sp_stopping, "beta", &["stop"]).env("RC_PID", "1").env("STOP_FAIL", "alpha").env("RC_RUNLEVEL", "shutdown"),
        st(sp_stopping, "beta", &["stop"]).env("RC_PID", "1").env("STOP_FAIL", "alpha").env("RC_RUNLEVEL", "single"),
        // More dependents: stopped in reverse order; one that won't stop.
        st(sp_dependents, "beta", &["stop"]),
        st(sp_dependents, "beta", &["-Z", "stop"]),
        st(sp_dependents, "beta", &["stop"]).env("STOP_FAIL", "pi"),
        st(sp_dependents_stopping, "beta", &["stop"]).env("RC_PID", "1").env("STOP_FAIL", "pi"),
        st(sp_dependents_stopping, "beta", &["stop"])
            .env("RC_PID", "1")
            .env("STOP_FAIL", "pi")
            .env("RC_RUNLEVEL", "shutdown"),
        st(sp_dependents_stopping, "beta", &["stop"])
            .env("RC_PID", "1")
            .env("STOP_FAIL", "pi")
            .env("RC_RUNLEVEL", "single"),
        st(sp_hotplugged_stopping, "beta", &["stop"]).env("RC_PID", "1"),
        st(st_plain, "beta", &["stop", "mycmd"]).env("IN_BACKGROUND", "yes"),
        // Someone else holds the lock of a service that needs us, or uses
        // us: we wait for it (and time out).
        st(sp_locked_pi, "beta", &["stop"]).env("LD_PRELOAD", fast),
        st(sp_locked_tau, "beta", &["stop"]).env("LD_PRELOAD", fast),
        // Waiting for a slow dependent in parallel: it times out.
        st(sp_parallel_slow, "beta", &["stop"]).env("LD_PRELOAD", fast),
        // pause isn't a command (QUIRK): it's passed to the script.
        st(st_plain, "beta", &["pause"]),
        // restart: stop (and what needs us), start, then start again what
        // was running.  (What a restart brings back starts in the
        // background, so no `restart restart`, nor restarts that bring back
        // two services: they race.)
        st(sp_restart, "beta", &["restart"]),
        st(sp_restart, "net.lo", &["restart"]),
        st(sp_restart, "net.eth0", &["restart"]),
        st(sp_restart, "delta", &["restart"]),
        st(sp_restart, "beta", &["-D", "restart"]),
        st(sp_restart, "beta", &["restart"]).env("STOP_FAIL", "beta"),
        st(sp_restart, "beta", &["restart"]).env("START_FAIL", "beta"),
        st(sp_restart, "beta", &["restart"]).env("STOP_FAIL", "delta"),
        st(sp_restart, "beta", &["restart"]).env("START_FAIL", "delta"),
        st(sp_restart, "beta", &["restart", "mycmd"]),
        st(sp_restart, "beta", &["restart"]).env("IN_BACKGROUND", "yes"),
        st(sp_restart_parallel, "beta", &["restart"]),
        // condrestart only when started.
        st(sp_restart, "beta", &["condrestart"]),
        st(sp_restart, "net.eth0", &["condrestart"]),
        st(sp_restart, "delta", &["conditionalrestart"]),
        st(sp_restart, "net.lo", &["conditionalrestart"]),
        st(sp_restart, "beta", &["-Z", "condrestart"]),
    ];
    common::check_all("openrc-run-stop", &cases);
}

#[test]
fn openrc_run_stop_plugins() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        st(st_plugins, "beta", &["stop"]),
        st(st_plugins, "net.lo", &["stop"]),
        st(st_plugins, "net.eth0", &["stop"]),
        st(st_plugins, "beta", &["-Z", "stop"]),
        st(st_plugins, "beta", &["stop"]).env("STOP_FAIL", "beta"),
        st(st_plugins, "beta", &["stop"]).env("STOP_FAIL", "delta"),
        st(st_plugins, "beta", &["stop"]).env("IN_BACKGROUND", "yes"),
        st(st_plugins, "delta", &["stop"]).env("IN_BACKGROUND", "yes"),
        st(st_plugins, "net.lo", &["restart"]),
        st(sp_plugins_no_deptree, "beta", &["stop"]),
        st(st_plugins, "net.lo", &["restart"]).env("PLUG_MODE", "unset"),
    ];
    common::check_all("openrc-run-stop-plugins", &cases);
}

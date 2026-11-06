//! Daemon tracking (Phase 2 step 3): librc.so.1 vs the C reference.
//!
//! Every case runs as PID 1 of a private PID namespace (so /proc shows
//! only what we put there): a bash prelude starts known processes and
//! writes their pidfiles, then rc-probe (or the reference
//! start-stop-daemon) runs with RC_SVCDIR in the sandbox.
//!
//! PIDs inside: 2 `sleep 300`, 3 `sleep 301` (own session), 4 a copy of
//! sleep named `averyveryverylongname` (comm is truncated to 15 bytes).

mod common;
use std::fs;
use std::path::{Path, PathBuf};

use common::Case;

const PRELUDE: &str = r#"sleep 300 &
echo $! > "$SANDBOX/two.pid"
setsid sleep 301 &
echo $! > "$SANDBOX/three.pid"
"$SANDBOX/averyveryverylongname" 302 &
echo 99999 > "$SANDBOX/dead.pid"
sleep 0.3"#;

fn sandbox(r: &Path) {
    fs::create_dir_all(r.join("run/daemons")).unwrap();
    fs::copy("/usr/bin/sleep", r.join("averyveryverylongname")).unwrap();
    // A chrooted daemon: its pidfile path is relative to the chroot.
    fs::create_dir_all(r.join("chroot")).unwrap();
    fs::write(r.join("chroot/x.pid"), "2\n").unwrap();
    fs::create_dir_all(r.join("run/options/chrooted")).unwrap();
    fs::write(r.join("run/options/chrooted/chroot"), r.join("chroot").display().to_string()).unwrap();
}

fn base(applet: &'static str, bins: Option<(PathBuf, PathBuf)>, args: &[&str]) -> Case {
    let mut c = Case::new(applet, args)
        .sandbox(sandbox)
        .pidns(PRELUDE)
        .env("SANDBOX", "$ROOT")
        .env("RC_SVCDIR", "$ROOT/run")
        .env("TERM", "dumb");
    c.bins = bins;
    c.ourlibs = true;
    c
}

#[test]
fn daemon_functions() {
    let Some(p) = common::rc_probe() else { return };
    let probe = |args: &[&str]| base("rc-probe", Some((p.clone(), p.clone())), args);
    let cases = vec![
        // rc_find_pids
        probe(&["find_pids", "sleep", "-", "0", "0"]),
        probe(&["find_pids", "/usr/bin/sleep", "-", "0", "0"]),
        probe(&["find_pids", "-", "sleep,300", "0", "0"]),
        probe(&["find_pids", "-", "sleep", "0", "0"]),
        probe(&["find_pids", "-", "sleep,30", "0", "0"]),
        probe(&["find_pids", "averyveryverylongname", "-", "0", "0"]),
        probe(&["find_pids", "averyveryverylo", "-", "0", "0"]),
        probe(&["find_pids", "-", "-", "0", "2"]),
        probe(&["find_pids", "-", "-", "0", "0"]),
        probe(&["find_pids", "sleep", "-", "0", "4"]),
        probe(&["find_pids", "-", "-", "65534", "0"]),
        probe(&["find_pids", "nosuch", "-", "0", "0"]),
        probe(&["find_pids", "sleep", "-", "0", "0"]).env("RC_OPENRC_PID", "2"),
        // rc_service_daemon_set / started_daemon / daemons_crashed
        probe(&[
            "daemon_set",
            "foo",
            "/usr/bin/sleep",
            "-",
            "$ROOT/two.pid",
            "1",
            "--",
            "started_daemon",
            "foo",
            "/usr/bin/sleep",
            "-",
            "0",
            "--",
            "crashed",
            "foo",
        ]),
        probe(&["daemon_set", "foo", "/usr/bin/sleep", "-", "$ROOT/dead.pid", "1", "--", "crashed", "foo"]),
        probe(&["daemon_set", "foo", "-", "-", "$ROOT/missing.pid", "1", "--", "crashed", "foo"]),
        probe(&[
            "daemon_set",
            "foo",
            "/usr/bin/sleep",
            "sleep,300",
            "-",
            "1",
            "--",
            "crashed",
            "foo",
            "--",
            "started_daemon",
            "foo",
            "/usr/bin/sleep",
            "sleep,300",
            "0",
            "--",
            "started_daemon",
            "foo",
            "/usr/bin/sleep",
            "sleep",
            "0",
            "--",
            "started_daemon",
            "foo",
            "/usr/bin/sleep",
            "-",
            "1",
            "--",
            "started_daemon",
            "foo",
            "/usr/bin/sleep",
            "-",
            "2",
        ]),
        probe(&["daemon_set", "foo", "/usr/bin/nosuchd", "-", "-", "1", "--", "crashed", "foo"]),
        probe(&[
            "daemon_set",
            "foo",
            "/a",
            "-",
            "-",
            "1",
            "--",
            "daemon_set",
            "foo",
            "/b",
            "-",
            "-",
            "1",
            "--",
            "daemon_set",
            "foo",
            "/c",
            "-",
            "-",
            "1",
            "--",
            "daemon_set",
            "foo",
            "/b",
            "-",
            "-",
            "0",
            "--",
            "started_daemon",
            "foo",
            "/c",
            "-",
            "2",
            "--",
            "started_daemon",
            "foo",
            "/c",
            "-",
            "3",
            "--",
            "daemon_set",
            "foo",
            "/d",
            "-",
            "-",
            "1",
        ]),
        probe(&[
            "daemon_set",
            "foo",
            "/a",
            "x",
            "-",
            "1",
            "--",
            "daemon_set",
            "foo",
            "/b",
            "-",
            "-",
            "1",
            "--",
            "started_daemon",
            "foo",
            "/b",
            "x",
            "0",
            "--",
            "started_daemon",
            "foo",
            "/a",
            "x",
            "0",
        ]),
        probe(&["daemon_set", "foo", "-", "-", "-", "1"]),
        probe(&["daemon_set", "foo", "/a", "-", "-", "0", "--", "crashed", "foo", "--", "crashed", "nosuch"]),
        probe(&["daemon_set", "chrooted", "/usr/bin/sleep", "-", "/x.pid", "1", "--", "crashed", "chrooted"]),
        probe(&[
            "daemon_set",
            "foo",
            "/usr/bin/sleep",
            "-",
            "$ROOT/two.pid",
            "1",
            "--",
            "daemon_set",
            "foo",
            "/usr/bin/sleep",
            "-",
            "$ROOT/dead.pid",
            "1",
            "--",
            "crashed",
            "foo",
        ]),
        probe(&["daemon_set", "/etc/init.d/foo", "/usr/bin/sleep", "-", "$ROOT/dead.pid", "1", "--", "crashed", "foo"]),
    ];
    common::check_all("daemon-probe", &cases);
}

#[test]
fn start_stop_daemon() {
    if common::rc_probe().is_none() {
        return;
    }
    let ssd = |args: &[&str]| base("start-stop-daemon", None, args).env("RC_SVCNAME", "foo");
    let cases = vec![
        ssd(&[
            "--start",
            "--background",
            "--make-pidfile",
            "--pidfile",
            "$ROOT/s.pid",
            "--exec",
            "/usr/bin/sleep",
            "--",
            "100",
        ]),
        ssd(&["--start", "--exec", "/usr/bin/sleep", "--test", "--", "100"]),
        ssd(&["--stop", "--exec", "/usr/bin/sleep"]),
        ssd(&["--stop", "--exec", "/usr/bin/sleep", "--test"]),
        ssd(&["--stop", "--name", "sleep", "--test"]),
        ssd(&["--stop", "--pidfile", "$ROOT/two.pid"]),
        ssd(&["--stop", "--pidfile", "$ROOT/dead.pid"]),
        ssd(&["--stop", "--exec", "/usr/bin/nosuchd"]),
        ssd(&["-v", "--stop", "--exec", "/usr/bin/sleep", "--retry", "TERM/1"]),
        ssd(&["--signal", "CONT", "--exec", "/usr/bin/sleep"]),
    ];
    common::check_all("daemon-ssd", &cases);
}

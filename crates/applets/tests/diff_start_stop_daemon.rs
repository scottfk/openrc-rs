//! start-stop-daemon (Phase 6 step 1): start-stop-daemon vs the C program.
//!
//! Every case runs as PID 1 of a private PID namespace (`Case::pidns`), so
//! `--stop --user 0` and friends can only reach the fake daemons the
//! prelude starts.  The prelude binds the sandbox's `run/` over
//! /run/openrc.  The fake daemon (common/fakedaemon.rs) appends a report of
//! how it was started (ids, umask, priorities, capabilities, fds, env...)
//! to `$ROOT/out`, which is compared.
//!
//! Changing user (`-u`) fails in both with "setgroups() failed": a user
//! namespace denies setgroups.  The VM exercise script covers it.
//!
//! Background starts use `--wait 300`: start-stop-daemon is the
//! namespace's init, and when it exits the daemon dies with it, so it
//! waits for the daemon to have written its report.

mod common;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::Case;
use common::fakedaemon::fakedaemon;

const P: &str = "mount --bind \"$ROOT/run\" /run/openrc";
// The victims run under a `sh` that waits for them (and we wait, without
// forking, for the pidfile: a fork racing the victim's would change its
// pid): start-stop-daemon is
// PID 1 here and never reaps, so a killed victim would otherwise linger
// as a zombie (which a pid lookup still finds).
/// One fake daemon running as `fakedaemon victim one`, pid in run/victim.pid.
const P_VICTIM: &str = "mount --bind \"$ROOT/run\" /run/openrc\n\
    FAKE_OUT= sh -c '\"$0\" victim one >/dev/null 2>&1 & echo $! > \"$1\"; wait' \
    \"$ROOT/bin/fakedaemon\" \"$ROOT/run/victim.pid\" &\n\
    while ! [ -s \"$ROOT/run/victim.pid\" ]; do :; done; sleep 0.2";
/// Two of them.
const P_TWO: &str = "mount --bind \"$ROOT/run\" /run/openrc\n\
    FAKE_OUT= sh -c '\"$0\" victim one >/dev/null 2>&1 & \"$0\" victim two >/dev/null 2>&1 & wait' \
    \"$ROOT/bin/fakedaemon\" &\n\
    sleep 0.2";
/// One that ignores SIGTERM.
const P_STUBBORN: &str = "mount --bind \"$ROOT/run\" /run/openrc\n\
    FAKE_OUT= FAKE_MODE=stubborn sh -c '\"$0\" victim >/dev/null 2>&1 & echo $! > \"$1\"; wait' \
    \"$ROOT/bin/fakedaemon\" \"$ROOT/run/victim.pid\" &\n\
    while ! [ -s \"$ROOT/run/victim.pid\" ]; do :; done; sleep 0.2";
/// One that ignores SIGTERM and SIGHUP and exits after 0.5s.
const P_SLOWEXIT: &str = "mount --bind \"$ROOT/run\" /run/openrc\n\
    FAKE_OUT= FAKE_MODE=stubborn-slowexit sh -c '\"$0\" victim >/dev/null 2>&1 & wait' \
    \"$ROOT/bin/fakedaemon\" &\n\
    sleep 0.2";

fn base(r: &Path) {
    let bin = r.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::copy(fakedaemon(), bin.join("fakedaemon")).unwrap();
    fs::create_dir_all(r.join("run")).unwrap();
    fs::create_dir_all(r.join("dir")).unwrap();
    fs::write(r.join("dir/in"), "input\n").unwrap();
    // Appended to, not truncated.
    fs::write(r.join("dir/o"), "earlier\n").unwrap();
    // An interpreted daemon, and a file that isn't one.
    let s = bin.join("script");
    fs::write(&s, "#!/bin/sh -e\nexec \"$(dirname \"$0\")/fakedaemon\" \"$@\"\n").unwrap();
    fs::set_permissions(&s, fs::Permissions::from_mode(0o755)).unwrap();
    // A tab before the interpreter, and no argument.
    let t = bin.join("tabscript");
    fs::write(&t, "#!\t/bin/sh\nexec \"$(dirname \"$0\")/fakedaemon\" \"$@\"\n").unwrap();
    fs::set_permissions(&t, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(bin.join("empty"), "").unwrap();
    fs::set_permissions(bin.join("empty"), fs::Permissions::from_mode(0o755)).unwrap();
}
fn with_pidfiles(r: &Path) {
    base(r);
    fs::write(r.join("run/empty.pid"), "").unwrap();
    fs::write(r.join("run/junk.pid"), "abc\n").unwrap();
    fs::write(r.join("run/gone.pid"), "999999\n").unwrap();
    fs::create_dir(r.join("run/dir.pid")).unwrap();
}

fn ssd(setup: fn(&Path), prelude: &'static str, args: &[&str]) -> Case {
    Case::new("start-stop-daemon", args)
        .sandbox(setup)
        .pidns(prelude)
        .env("FAKE_OUT", "$ROOT/out")
        .env("PATH", "$ROOT/bin:/usr/bin:/bin")
        .file("out")
}
fn plain(args: &[&str]) -> Case {
    ssd(base, P, args)
}
/// Start `fakedaemon` in the background, waiting for its report.
fn bg(extra: &[&str]) -> Case {
    let mut a = vec!["-S", "-b", "-w", "300"];
    a.extend_from_slice(extra);
    a.extend_from_slice(&["-x", "$ROOT/bin/fakedaemon", "--", "arg1", "arg 2"]);
    plain(&a)
}

#[test]
fn ssd_options() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        plain(&["-h"]),
        plain(&["-V"]),
        plain(&["--nosuch"]),
        plain(&[]),
        plain(&["-K"]),
        plain(&["-K", "-b", "-x", "x"]),
        plain(&["-K", "-m", "-x", "x"]),
        plain(&["-K", "-1", "f", "-x", "x"]),
        plain(&["-K", "-3", "cat", "-x", "x"]),
        plain(&["-K", "-w", "5", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-K", "-u", "nobody"]),
        plain(&["-S", "-2", "f", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-S", "-t", "-R", "5", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-s", "64", "-x", "$ROOT/bin/nosuch"]),
        plain(&["-s", "65", "-x", "$ROOT/bin/nosuch"]),
        plain(&["-S"]),
        plain(&["-S", "-m", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-S", "-1", "f", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-S", "-4", "cat", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-S", "-b", "-1", "f", "-3", "cat", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-S", "-b", "-2", "f", "-4", "cat", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-S", "-x", "$ROOT/bin/nosuch"]),
        plain(&["-S", "-x", "nosuch"]),
        plain(&["-S", "-x", "./nosuch"]),
        plain(&["-S", "-r", "$ROOT/dir", "-x", "/fakedaemon"]),
        plain(&["-S", "-u", "nosuchuser", "-x", "x"]),
        plain(&["-S", "-u", "0:nosuchgroup", "-x", "x"]),
        plain(&["-S", "-g", "nosuchgroup", "-x", "x"]),
        plain(&["-N", "abc", "-S", "-x", "x"]),
        plain(&["-I", "abc", "-S", "-x", "x"]),
        plain(&["--oom-score-adj", "x", "-S", "-x", "x"]),
        plain(&["-w", "x", "-S", "-x", "x"]),
        plain(&["-k", "999", "-S", "-x", "x"]),
        plain(&["-s", "BOGUS", "-x", "x"]),
        plain(&["-s", "99", "-x", "x"]),
        plain(&["-s", "-1", "-x", "x"]),
        plain(&["-R", "TERM/bogus", "-K", "-x", "x"]),
        plain(&["-R", "TERM", "-K", "-x", "x"]),
        plain(&["-R", "forever/forever", "-K", "-x", "x"]),
        plain(&["-R", "TERM/12345678901234567890", "-K", "-x", "x"]),
        plain(&["-R", "5/", "-K", "-x", "x"]),
        plain(&["--capabilities", "cap_nosuch", "-S", "-x", "x"]),
        plain(&["--capabilities", "cap_chown,,", "-S", "-x", "x"]),
        plain(&["--secbits", "", "-S", "-x", "x"]),
        plain(&["--secbits", "0x1g", "-S", "-x", "x"]),
        plain(&["--notify", "bogus", "-S", "-x", "x"]),
        plain(&["-c", "0", "-t", "-S", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-o", "-t", "-K", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-a", "$ROOT/bin/fakedaemon", "-t", "-S"]),
        plain(&["-t", "-S", "-x", "x"]).env("SSD_NICELEVEL", "x"),
        plain(&["-t", "-S", "-x", "x"]).env("SSD_IONICELEVEL", "x"),
        plain(&["-t", "-S", "-x", "x"]).env("SSD_OOM_SCORE_ADJ", "x"),
    ];
    common::check_all("ssd-options", &cases);
}

#[test]
fn ssd_test_mode() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        plain(&["-t", "-S", "-x", "$ROOT/bin/fakedaemon", "--", "a", "b"]),
        plain(&["-t", "-S", "-x", "fakedaemon"]),
        plain(&["-t", "-S", "-N", "0", "-x", "fakedaemon"]),
        plain(&["-t", "-S", "-N", "5", "-u", "0", "-g", "0", "-r", "/", "-d", "/tmp", "-n", "nm", "-x", "fakedaemon"]),
        plain(&["-t", "-S", "-u", "root:root", "-x", "fakedaemon"]),
        plain(&["-t", "-S", "-x", "fakedaemon"]).env("EINFO_QUIET", "yes"),
        // The process name only: it's also what's run.
        plain(&["-t", "-S", "-n", "fakedaemon"]),
        plain(&["-t", "-S", "--", "fakedaemon", "x"]),
        plain(&["-t", "-S", "-n", "nm", "--", "fakedaemon", "x"]),
        plain(&["-t", "-S", "-i", "-x", "$ROOT/bin/script", "--", "a"]),
        plain(&["-t", "-S", "-i", "-x", "$ROOT/bin/empty"]),
        plain(&["-t", "-S", "-d", "~/x", "-r", "~root", "-x", "~/nosuch"]),
        ssd(base, P_VICTIM, &["-t", "-S", "-x", "$ROOT/bin/fakedaemon"]),
        ssd(base, P_VICTIM, &["-t", "-S", "-p", "$ROOT/run/victim.pid", "-x", "x"]),
        ssd(base, P_VICTIM, &["-t", "-K", "-x", "$ROOT/bin/fakedaemon"]),
        ssd(base, P_TWO, &["-t", "-K", "-G", "-n", "victim"]),
    ];
    common::check_all("ssd-test", &cases);
}

#[test]
fn ssd_start() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        bg(&[]),
        bg(&["-v"]),
        bg(&["-m", "-p", "$ROOT/run/fake.pid"]).file("run/fake.pid"),
        bg(&["-d", "$ROOT/dir", "-k", "077"]),
        bg(&["-N", "5", "--oom-score-adj", "100", "-I", "2:4"]),
        bg(&["-I", "3"]),
        bg(&["-I", "3:2"]),
        bg(&["-I", "0:5"]),
        bg(&["-e", "FOO=bar", "-e", "HOME", "-e", "RC_X=1"]),
        bg(&[]).env("RC_X", "1").env("SSD_Y", "2").env("EINFO_Z", "3").env("SVCNAME", "s").env("KEEP", "4"),
        bg(&[]).env("PATH", "/usr/libexec/rc/bin:$ROOT/bin::/usr/libexec/rc/sbin:/usr/bin:/bin:"),
        bg(&["-0", "$ROOT/dir/in", "-1", "$ROOT/dir/o", "-2", "$ROOT/dir/e"]).file("dir/o").file("dir/e"),
        bg(&["-1", "$ROOT/dir/o"]).file("dir/o"),
        bg(&["-0", "$ROOT/nosuch"]),
        bg(&["-1", "/proc/nosuch/x"]),
        bg(&["-3", "cat > $ROOT/dir/logged", "-4", "cat > $ROOT/dir/elogged"]).file("dir/logged").file("dir/elogged"),
        bg(&["--capabilities", "^cap_net_admin,!cap_sys_admin,%cap_kill"]),
        bg(&["--capabilities", "!cap_chown,cap_bpf"]),
        bg(&["--capabilities", ""]),
        bg(&["--secbits", "0x2f"]),
        bg(&["--secbits", "4096"]),
        bg(&["--no-new-privs"]),
        bg(&["--scheduler", "batch"]),
        bg(&["--scheduler", "idle", "--scheduler-priority", "0"]),
        bg(&["--scheduler", "3"]),
        bg(&["--scheduler", "nosuch"]),
        bg(&["--scheduler", "fifo"]),
        bg(&["--scheduler-priority", "5"]),
        bg(&["-u", "0"]),
        bg(&["-u", "root:0", "-g", "0"]),
        bg(&[]).env("SSD_NICELEVEL", "7").env("SSD_IONICELEVEL", "2:1").env("SSD_OOM_SCORE_ADJ", "-5"),
        bg(&[]).env("SSD_IONICELEVEL", "x"),
        bg(&[]).env("SSD_IONICELEVEL", "3:2"),
        bg(&[]).env("RC_SVCNAME", "svc"),
        bg(&["-p", "$ROOT/run/fake.pid", "-m"]).env("RC_SVCNAME", "svc"),
        // The daemon forks itself (not --background): we wait for it.
        plain(&["-S", "-p", "$ROOT/run/f.pid", "-x", "$ROOT/bin/fakedaemon", "--", "x"])
            .env("FAKE_MODE", "fork")
            .env("FAKE_PIDFILE", "$ROOT/run/f.pid")
            .file("run/f.pid"),
        plain(&["-S", "-w", "200", "-p", "$ROOT/run/f.pid", "-x", "$ROOT/bin/fakedaemon"])
            .env("FAKE_MODE", "fork")
            .env("FAKE_PIDFILE", "$ROOT/run/f.pid"),
        plain(&["-S", "-w", "200", "-p", "$ROOT/run/none.pid", "-x", "$ROOT/bin/fakedaemon"]).env("FAKE_MODE", "fork"),
        plain(&["-S", "-w", "200", "-x", "$ROOT/bin/fakedaemon"]).env("FAKE_MODE", "fork"),
        plain(&["-S", "-x", "$ROOT/bin/fakedaemon"]).env("FAKE_MODE", "exit:3"),
        plain(&["-S", "-x", "$ROOT/bin/fakedaemon"]).env("FAKE_MODE", "exit:0").env("EINFO_QUIET", "yes"),
        // It dies early.
        bg(&[]).env("FAKE_MODE", "exit:0"),
        plain(&["-S", "-b", "-w", "100", "-x", "$ROOT/bin/fakedaemon"])
            .env("FAKE_MODE", "exit:0")
            .env("SSD_STARTWAIT", "x"),
        plain(&["-S", "-b", "-x", "$ROOT/bin/fakedaemon"]).env("FAKE_MODE", "slowexit").env("SSD_STARTWAIT", "200"),
        // Readiness.
        plain(&["-S", "-b", "--notify", "fd:4", "-x", "$ROOT/bin/fakedaemon"]).env("FAKE_MODE", "notify-fd:4"),
        plain(&["-S", "-b", "--notify", "fd:4", "-x", "$ROOT/bin/fakedaemon"]).env("FAKE_MODE", "exit:0"),
        plain(&["-S", "-b", "--notify", "socket:ready", "-x", "$ROOT/bin/fakedaemon"])
            .env("FAKE_MODE", "notify-socket"),
        plain(&["-S", "-b", "-w", "300", "--notify", "socket:x", "-x", "$ROOT/bin/fakedaemon"]),
        // Already running; a stale pidfile.
        ssd(base, P_VICTIM, &["-S", "-x", "$ROOT/bin/fakedaemon", "--", "victim", "one"]),
        ssd(base, P_VICTIM, &["-S", "-b", "-w", "300", "-x", "$ROOT/bin/fakedaemon"]),
        ssd(with_pidfiles, P, &["-S", "-b", "-w", "300", "-p", "$ROOT/run/gone.pid", "-x", "$ROOT/bin/fakedaemon"]),
        // Errors in the child (before stderr goes to /dev/null).
        plain(&["-S", "-r", "/proc/nosuch", "-x", "/"]),
        plain(&["-S", "-d", "/proc/nosuch", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-S", "-m", "-p", "/proc/nosuch/p", "-x", "$ROOT/bin/fakedaemon"]),
        plain(&["-S", "-x", "$ROOT/bin/empty"]),
        // The name only: argv runs on into the environment (QUIRK).
        plain(&["-S", "-b", "-w", "300", "-n", "fakedaemon"]),
        plain(&["-S", "-b", "-w", "300", "-n", "nm", "-x", "$ROOT/bin/fakedaemon", "--", "x"]),
        plain(&["-S", "-b", "-w", "300", "-i", "-x", "$ROOT/bin/script", "--", "s"]),
        // What's recorded for the service: the interpreter and its argument.
        plain(&["-S", "-b", "-w", "300", "-i", "-x", "$ROOT/bin/script", "--", "s"])
            .env("RC_SVCNAME", "svc")
            .file("run/daemons/svc/001"),
        plain(&["-S", "-b", "-w", "300", "-i", "-x", "$ROOT/bin/tabscript"])
            .env("RC_SVCNAME", "svc")
            .file("run/daemons/svc/001"),
        // The forked daemon dies before the wait is up.
        plain(&["-S", "-w", "200", "-x", "$ROOT/bin/fakedaemon"]).env("FAKE_MODE", "fork-die"),
        // Readiness that never comes: no newline before the fd closes.
        plain(&["-S", "-b", "--notify", "fd:4", "-x", "$ROOT/bin/fakedaemon"]).env("FAKE_MODE", "notify-partial"),
    ];
    common::check_all("ssd-start", &cases);
}

#[test]
fn ssd_stop() {
    if common::reference_dir().is_none() {
        return;
    }
    let v = |args: &[&str]| ssd(base, P_VICTIM, args);
    let cases = vec![
        v(&["-K", "-x", "$ROOT/bin/fakedaemon"]),
        v(&["-K", "-p", "$ROOT/run/victim.pid"]),
        v(&["-K", "-p", "$ROOT/run/victim.pid"]).file("run/victim.pid"),
        v(&["-K", "-n", "victim"]),
        v(&["-K", "-n", "nosuch"]),
        v(&["-K", "-u", "0"]),
        v(&["-K", "-x", "$ROOT/bin/fakedaemon", "--", "victim", "one"]),
        v(&["-K", "-x", "$ROOT/bin/fakedaemon", "--", "victim", "two"]),
        v(&["-K", "-s", "HUP", "-x", "$ROOT/bin/fakedaemon"]),
        v(&["-s", "USR1", "-x", "$ROOT/bin/fakedaemon"]),
        v(&["-s", "0", "-x", "$ROOT/bin/fakedaemon"]),
        v(&["-s", "SIGKILL", "-x", "$ROOT/bin/fakedaemon"]),
        v(&["-K", "-o", "-x", "$ROOT/bin/nosuch"]),
        v(&["-K", "-x", "$ROOT/bin/nosuch"]),
        v(&["-K", "-R", "TERM/1", "-x", "$ROOT/bin/fakedaemon"]),
        v(&["-K", "-G", "-x", "$ROOT/bin/fakedaemon"]),
        v(&["-K", "-x", "$ROOT/bin/fakedaemon"]).env("RC_SVCNAME", "svc"),
        ssd(base, P_TWO, &["-K", "-x", "$ROOT/bin/fakedaemon"]),
        ssd(base, P_TWO, &["-K", "-v", "-n", "victim"]),
        // It won't go: the schedule, with progress dots.
        ssd(base, P_STUBBORN, &["-K", "-R", "1", "-x", "$ROOT/bin/fakedaemon"]),
        ssd(base, P_STUBBORN, &["-K", "-P", "-R", "TERM/1/KILL/1", "-x", "$ROOT/bin/fakedaemon"]),
        // (`forever` waits for as long as the process lives, so only with
        // one that goes: this one exits on its own after a moment.)
        ssd(base, P_SLOWEXIT, &["-K", "-R", "HUP/forever/KILL/1", "-x", "$ROOT/bin/fakedaemon"]),
        ssd(base, P_STUBBORN, &["-K", "-R", "-HUP/0/KILL/0", "-x", "$ROOT/bin/fakedaemon"]),
        // A zero timeout ends the schedule (KILL isn't sent).
        ssd(base, P_STUBBORN, &["-K", "-v", "-R", "TERM/0/KILL/0", "-x", "$ROOT/bin/fakedaemon"]),
        // The default schedule: TERM, then 5 seconds (in dots).
        ssd(base, P_STUBBORN, &["-K", "-P", "-x", "$ROOT/bin/fakedaemon"]),
        // Verbose: each poll of a process that's still there says [ ok ]
        // (QUIRK), so only a stubborn one gives a fixed count.
        ssd(base, P_STUBBORN, &["-K", "-v", "-R", "TERM/1", "-p", "$ROOT/run/victim.pid"]),
        ssd(base, P_STUBBORN, &["-K", "-v", "-R", "TERM/1", "-x", "$ROOT/bin/fakedaemon"]),
        // Pidfiles that don't help.
        ssd(with_pidfiles, P_VICTIM, &["-K", "-p", "$ROOT/run/nosuch.pid", "-x", "$ROOT/bin/fakedaemon"]),
        ssd(with_pidfiles, P_VICTIM, &["-K", "-p", "$ROOT/run/empty.pid", "-x", "$ROOT/bin/fakedaemon"]),
        ssd(with_pidfiles, P_VICTIM, &["-K", "-p", "$ROOT/run/junk.pid"]),
        ssd(with_pidfiles, P_VICTIM, &["-K", "-p", "$ROOT/run/dir.pid"]),
        ssd(with_pidfiles, P_VICTIM, &["-K", "-v", "-p", "$ROOT/run/gone.pid"]),
        // A pidfile with no pid, or one that can't be opened, looks
        // missing unless verbose (QUIRK: ewarnv sets errno).
        ssd(with_pidfiles, P_VICTIM, &["-K", "-v", "-p", "$ROOT/run/junk.pid"]),
        ssd(with_pidfiles, P_VICTIM, &["-K", "-p", "$ROOT/run/empty.pid/x", "-x", "$ROOT/bin/fakedaemon"]),
        ssd(with_pidfiles, P_VICTIM, &["-K", "-v", "-p", "$ROOT/run/empty.pid/x", "-x", "$ROOT/bin/fakedaemon"]),
    ];
    common::check_all("ssd-stop", &cases);
}

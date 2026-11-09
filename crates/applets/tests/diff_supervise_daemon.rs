//! supervise-daemon (Phase 6 step 2): supervise-daemon vs the C program.
//!
//! supervise-daemon leaves a supervisor running, so each case is a
//! scenario: the PID namespace's prelude (bash, PID 1, which reaps what's
//! orphaned) runs the program under test (`sd`, i.e. `"$0"` with argv[0]
//! `supervise-daemon`) for each step, waits on the files it leaves, and
//! prints what happened.  The case's own command is then a no-op
//! (`supervise-daemon svc`).  Compared: everything printed, the fake
//! daemon's reports (common/fakedaemon.rs, one per (re)spawn), the
//! service's state and values in the sandbox, and the health check log.
//!
//! A daemon that dies is only noticed when its SIGCHLD interrupts the
//! supervisor's blocking open() of the control FIFO; arriving any other
//! time it's lost (QUIRK, in C too).  So daemons here die a while after
//! they start (`slow-exit:N`, USR2), never at once, and the checkpoints
//! of the timing cases allow for that 0.3s.
//!
//! /run is a fresh tmpfs (so the default pidfile under /var/run works),
//! with the sandbox's `run/` as /run/openrc and its `init.d/` as
//! /etc/init.d: the service is `svc`, whose script logs the health check
//! commands it's run with.

mod common;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::Case;
use common::fakedaemon::fakedaemon;

macro_rules! prelude {
    ($($body:expr),* $(,)?) => {
        concat!(
            "mount -t tmpfs tmpfs /run; mkdir /run/openrc\n",
            "mount --bind \"$ROOT/run\" /run/openrc; mount --bind \"$ROOT/init.d\" /etc/init.d\n",
            "BIN=\"$0\"\n",
            "sd() { (exec -a supervise-daemon \"$BIN\" \"$@\"); echo \"[sd $*: $?]\"; }\n",
            // Wait (up to 25s, 10s) for a file to go, or to appear.
            "gone() { i=0; while [ -e \"$1\" ] && [ $i -lt 250 ]; do sleep 0.1; i=$((i+1)); done; }\n",
            "there() { i=0; while ! [ -e \"$1\" ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done; }\n",
            "runs() { grep -c '^argv:' \"$FAKE_OUT\" 2>/dev/null || echo 0; }\n",
            // Is the supervisor still there?
            "alive() { [ -e \"$P\" ] && echo alive || echo gone; }\n",
            // Run a step, saying whether it took under 2 seconds.
            "timed() { t=$(date +%s%N); \"$@\"; t=$((($(date +%s%N) - t) / 1000000)); [ $t -lt 2000 ] && echo quick || echo slow; }\n",
            "D=\"$ROOT/bin/fakedaemon\"; P=\"$ROOT/run/sv.pid\"\n",
            $($body, "\n",)*
        )
    };
}

fn base(r: &Path) {
    let bin = r.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::copy(fakedaemon(), bin.join("fakedaemon")).unwrap();
    fs::create_dir_all(r.join("run")).unwrap();
    fs::create_dir_all(r.join("dir")).unwrap();
    let initd = r.join("init.d");
    fs::create_dir_all(&initd).unwrap();
    // The first health check fails (unless HC_OK), the rest pass.
    let svc = initd.join("svc");
    fs::write(
        &svc,
        format!(
            "#!/bin/sh\necho \"$1 $(cat /proc/self/status | grep ^SigBlk)\" >> ${{HC_LOG:-{root}/hc.log}}\n\
             case $1 in healthcheck)\n\
             \t[ -n \"$HC_OK\" ] && exit 0\n\
             \t[ -e {root}/hc.failed ] && exit 0\n\
             \ttouch {root}/hc.failed; exit 1 ;;\nesac\n",
            root = r.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&svc, fs::Permissions::from_mode(0o755)).unwrap();
}

fn sd(prelude: &'static str) -> Case {
    Case::new("supervise-daemon", &["svc"])
        .sandbox(base)
        .pidns(prelude)
        .env("RC_SVCNAME", "svc")
        .env("FAKE_OUT", "$ROOT/out")
        .env("PATH", "$ROOT/bin:/usr/bin:/bin")
        .file("out")
        .file("hc.log")
}
/// A timing scenario: only what it prints at its checkpoints is compared.
/// The daemon's reports (how many spawns happen before --stop drifts) and
/// the health check log go outside the sandbox, to the namespace's /run.
fn timed(prelude: &'static str) -> Case {
    let mut c = sd(prelude);
    c.files.clear();
    c.env.retain(|(k, _)| k != "FAKE_OUT");
    c.env("FAKE_OUT", "/run/fake.out").env("HC_LOG", "/run/hc.log")
}
/// One command, no scenario.
fn one(args: &[&str]) -> Case {
    Case::new("supervise-daemon", args)
        .sandbox(base)
        .pidns(prelude!())
        .env("RC_SVCNAME", "svc")
        .env("FAKE_OUT", "$ROOT/out")
        .env("PATH", "$ROOT/bin:/usr/bin:/bin")
}

#[test]
fn supervise_options() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut no_svcname = one(&["svc", "-S", "x"]);
    no_svcname.env.retain(|(k, _)| k != "RC_SVCNAME");
    let cases = vec![
        no_svcname,
        one(&[]),
        one(&["other", "-S"]),
        one(&["svc", "-h"]),
        one(&["svc", "-V"]),
        one(&["svc", "--nosuch"]),
        one(&["svc", "-P", "5"]),
        one(&["svc", "-S"]),
        one(&["svc", "-S", "$ROOT/bin/nosuch"]),
        one(&["svc", "-S", "nosuch"]),
        one(&["svc", "-S", "-r", "$ROOT/dir", "/fakedaemon"]),
        one(&["svc", "-a", "0", "-S", "x"]),
        one(&["svc", "-A", "x", "-S", "x"]),
        one(&["svc", "-m", "-1", "-S", "x"]),
        one(&["svc", "-D", "5x", "-S", "x"]),
        one(&["svc", "--respawn-period", "-1", "-S", "x"]),
        one(&["svc", "--respawn-delay-step", "x", "-S", "x"]),
        one(&["svc", "--respawn-delay-cap", "1y", "-S", "x"]),
        one(&["svc", "-N", "x", "-S", "x"]),
        one(&["svc", "-I", "x", "-S", "x"]),
        one(&["svc", "--oom-score-adj", "x", "-S", "x"]),
        one(&["svc", "-k", "999", "-S", "x"]),
        one(&["svc", "-s", "BOGUS"]),
        one(&["svc", "-u", "nosuchuser", "-S", "x"]),
        one(&["svc", "-g", "nosuchgroup", "-S", "x"]),
        one(&["svc", "--capabilities", "cap_nosuch", "-S", "x"]),
        one(&["svc", "--secbits", "", "-S", "x"]),
        one(&["svc", "--notify", "bogus", "-S", "x"]),
        one(&["svc", "-R", "TERM/x", "-S", "$ROOT/bin/fakedaemon"]),
        // No supervisor: --stop cleans up anyway; --signal can't reach it.
        one(&["svc", "-K", "-p", "$ROOT/run/sv.pid"]).file("run/sv.pid"),
        one(&["svc", "-K"]),
        one(&["svc", "-s", "HUP"]),
        one(&["svc", "--signal", "USR1", "-p", "$ROOT/run/sv.pid"]),
        one(&["svc", "-p", "/proc/nosuch/p", "-S", "$ROOT/bin/fakedaemon"]),
        // Nothing to do.
        one(&["svc"]),
        one(&["svc", "-v"]),
    ];
    common::check_all("supervise-options", &cases);
}

#[test]
fn supervise_run() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        // Start, signal through the FIFO (HUP kills it: respawned), stop.
        sd(prelude!(
            "sd svc -p \"$P\" -S \"$D\" -- one 'two words'",
            "there \"$ROOT/out\"; sleep 0.2; echo \"runs: $(runs)\"",
            "sd svc -p \"$P\" -S \"$D\"",
            "sd svc -p \"$P\" -s USR2; sleep 1; echo \"runs: $(runs)\"",
            "echo \"count: $(cat \"$ROOT/run/options/svc/start_count\")\"",
            "sd svc -p \"$P\" -s 0; sleep 1; echo \"runs: $(runs)\"",
            "timed sd svc -p \"$P\" -K; echo \"pidfile: $(ls \"$P\" 2>&1 | wc -l)\"; echo \"runs: $(runs)\"",
            "ls \"$ROOT/run\"",
        )),
        // The default pidfile, verbose, and options for the daemon.
        sd(prelude!(
            "sd svc -v -S -d \"$ROOT/dir\" -k 077 -N 4 -I 2:3 --oom-score-adj 50 -e FOO=bar \"$D\" x",
            "there \"$ROOT/out\"; sleep 0.2; ls /var/run",
            "sd svc -K; ls /var/run",
        )),
        sd(prelude!(
            "sd svc -p \"$P\" -S --capabilities '^cap_net_admin,!cap_sys_admin' --secbits 0x2f --no-new-privs \"$D\"",
            "there \"$ROOT/out\"; sleep 0.2; sd svc -p \"$P\" -K",
        )),
        // Redirections.
        sd(prelude!(
            "echo in > \"$ROOT/dir/in\"; echo earlier > \"$ROOT/dir/o\"",
            "sd svc -p \"$P\" -S -0 \"$ROOT/dir/in\" -1 \"$ROOT/dir/o\" -2 \"$ROOT/dir/e\" \"$D\"",
            "there \"$ROOT/out\"; sleep 0.2; sd svc -p \"$P\" -K; cat \"$ROOT/dir/o\" \"$ROOT/dir/e\"",
            "ls -l \"$ROOT/dir\" | sed 's/ .*:[0-9][0-9]//' | awk '{print $1, $NF}'",
        )),
        sd(prelude!(
            "sd svc -p \"$P\" -S --stdout-logger \"cat > $ROOT/dir/lo\" --stderr-logger \"cat > $ROOT/dir/le\" \"$D\"",
            "there \"$ROOT/out\"; sleep 0.2; sd svc -p \"$P\" -K; sleep 0.2; cat \"$ROOT/dir/lo\" \"$ROOT/dir/le\"",
        )),
        sd(prelude!(
            "sd svc -p \"$P\" -S \"$D\"",
            "there \"$ROOT/out\"; sleep 0.2; EINFO_QUIET=yes sd svc -p \"$P\" -K",
        ))
        .env("EINFO_QUIET", "yes"),
        // Already running.
        sd(prelude!(
            "sd svc -p \"$P\" -S \"$D\"; there \"$ROOT/out\"; sleep 0.2",
            "sd svc -p \"$P\" -S \"$D\"",
            "sd svc -p \"$P\" -K",
        )),
        // It keeps dying: respawned 10 times, then the supervisor gives up
        // and the service is failed.
        sd(prelude!(
            "FAKE_MODE=slow-exit:3 sd svc -p \"$P\" -S \"$D\"",
            "gone \"$P\"; echo \"runs: $(runs)\"; ls \"$ROOT/run\" \"$ROOT/run/failed\" \"$ROOT/run/options/svc\"",
        )),
        sd(prelude!("FAKE_MODE=slow-exit:0 sd svc -p \"$P\" -m 2 -S \"$D\"", "gone \"$P\"; echo \"runs: $(runs)\"",)),
        // Respawn delays (with the warnings), and the period resetting the
        // count: 0 means unlimited, so we stop it.
        sd(prelude!(
            "FAKE_MODE=slow-exit:0 sd svc -p \"$P\" -m 3 -D 100ms --respawn-delay-step 50ms --respawn-delay-cap 80ms -S \"$D\"",
            "gone \"$P\"; echo \"runs: $(runs)\"",
        )),
        sd(prelude!(
            "sd svc -p \"$P\" -m 2 -D 2s --respawn-period 1s -S \"$D\"",
            "sleep 0.3; sd svc -p \"$P\" -K; echo \"runs: $(runs)\"",
        )),
        timed(prelude!(
            "FAKE_MODE=slowexit sd svc -p \"$P\" -m 0 --respawn-delay-step 0 -S \"$D\"",
            "sleep 1.25; sd svc -p \"$P\" -K; [ $(runs) -ge 2 ] && echo respawning",
        )),
        // Health checks: the first fails (unhealthy, killed, respawned),
        // the next ones pass.  The kill waits out the retry timeout (QUIRK:
        // the killed daemon stays a zombie), so -R 1.
        timed(prelude!(
            // (-a 1: if the daemon is still dying when the kill gives up,
            // its SIGCHLD can be lost, the upstream race; the next check's
            // alarm then wakes the supervisor.)
            "sd svc -p \"$P\" -A 1 -a 1 -R 1 -S \"$D\"",
            // (Check at 1s, the kill's "1s", the respawn, the next check 1s
            // later.  QUIRK: the kill's timeout counts 50 polls, so it
            // stretches on a slow machine (5s seen on the VM): wait for the
            // respawn, then a little for the next check.)
            "i=0; while [ $(runs) -lt 2 ] && [ $i -lt 200 ]; do sleep 0.1; i=$((i+1)); done; sleep 1.5",
            "echo \"unhealthy: $(grep -c ^unhealthy \"$HC_LOG\")\"",
            "[ $(runs) -ge 2 ] && echo respawned; sd svc -p \"$P\" -K",
        )),
        sd(prelude!(
            // (Checks at 2s and 4s: one before the stop.)
            "HC_OK=1 sd svc -p \"$P\" -a 2 -S \"$D\"",
            "sleep 3; sd svc -p \"$P\" -K; echo \"runs: $(runs)\"",
        )),
        // Respawn timing, counted at fixed points well apart: a delay of
        // 2s; a step of 2s per respawn; a 2s step capped at 2.5s; a period
        // that keeps resetting the count, so -m 2 never gives up.  (The
        // test VM sometimes stalls for most of a second, so every
        // checkpoint is a second or more from a spawn.)
        timed(prelude!(
            // (Spawns at 0, 2.3 and 4.6s.)
            "FAKE_MODE=slow-exit:0 sd svc -p \"$P\" -m 3 -D 2s --respawn-delay-step 0 -S \"$D\"",
            "sleep 1.2; echo \"runs: $(runs)\"; sleep 2.3; echo \"runs: $(runs)\"; sleep 2.3; echo \"runs: $(runs)\"",
            "sd svc -p \"$P\" -K",
        )),
        timed(prelude!(
            // (Spawns at 0, 2.3 and 6.6s.)
            "FAKE_MODE=slow-exit:0 sd svc -p \"$P\" -m 3 --respawn-delay-step 2s -S \"$D\"",
            "sleep 1; echo \"runs: $(runs)\"; sleep 2.5; echo \"runs: $(runs)\"; sleep 2; echo \"runs: $(runs)\"",
            "sleep 2.5; echo \"runs: $(runs)\"; sd svc -p \"$P\" -K",
        )),
        timed(prelude!(
            "FAKE_MODE=slow-exit:0 sd svc -p \"$P\" -m 4 --respawn-delay-step 2s --respawn-delay-cap 2500ms -S \"$D\"",
            // (Spawns at 0, 2.3, 5.1 and 7.9s.)
            "sleep 1; echo \"runs: $(runs)\"; sleep 2.5; echo \"runs: $(runs)\"; sleep 2.8; echo \"runs: $(runs)\"",
            "sd svc -p \"$P\" -K",
        )),
        timed(prelude!(
            "FAKE_MODE=slow-exit:0 sd svc -p \"$P\" -m 2 -D 900ms --respawn-delay-step 0 --respawn-period 1s -S \"$D\"",
            "sleep 4; alive; sd svc -p \"$P\" -K",
        )),
        // Readiness.
        sd(prelude!(
            "FAKE_MODE=notify-fd:4 sd svc -p \"$P\" --notify fd:4 -S \"$D\"",
            "echo \"runs: $(runs)\"; sd svc -p \"$P\" -K",
        )),
        // On an fd that isn't the pipe's own: dup'ed there.
        sd(prelude!(
            "FAKE_MODE=notify-fd:7 sd svc -p \"$P\" --notify fd:7 -S \"$D\"",
            "echo \"runs: $(runs)\"; sd svc -p \"$P\" -K",
        )),
        sd(prelude!(
            "FAKE_MODE=notify-partial sd svc -p \"$P\" -m 1 --notify fd:4 -S \"$D\"",
            "gone \"$P\"; echo \"runs: $(runs)\"",
        )),
        sd(prelude!(
            "FAKE_MODE=notify-socket sd svc -p \"$P\" --notify socket:ready -S \"$D\"",
            "echo \"runs: $(runs)\"; ls \"$ROOT/run\"; sd svc -p \"$P\" -K",
        )),
        // Stopping a daemon that won't go: the retry schedule.
        sd(prelude!(
            "FAKE_MODE=stubborn sd svc -p \"$P\" -R TERM/1/KILL/1 -S \"$D\"",
            "there \"$ROOT/out\"; sleep 0.2; sd svc -p \"$P\" -K; echo \"runs: $(runs)\"",
        )),
        // The supervisor dies; another takes over the daemon (--reexec).
        sd(prelude!(
            "sd svc -p \"$P\" -S \"$D\"; there \"$ROOT/out\"; sleep 0.2",
            "kill -9 $(cat \"$P\"); sleep 0.2",
            "(exec -a supervise-daemon \"$BIN\" svc --reexec &) ; sleep 0.3",
            "sd svc -p \"$P\" -K; echo \"runs: $(runs)\"; ls \"$ROOT/run/options/svc\"",
        )),
    ];
    common::check_all("supervise-run", &cases);
}

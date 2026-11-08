//! openrc-run (Phase 4 step 1): openrc-run vs the C program, for
//! everything but start/stop/restart.
//!
//! Most cases put a fake `openrc-run.sh` in the service directory (which
//! openrc-run prefers to the installed one): it prints the argv and
//! environment it gets and acts on a few made-up commands (fail, killme,
//! hup, term).  Others run the real, installed openrc-run.sh for
//! describe and status.
//!
//! The fake sleeps after printing (and `lines` again after its lines):
//! openrc-run relays the pty byte by byte, behind the script, and C loses
//! bytes when the script's exit (SIGCHLD) interrupts a write.

mod common;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use common::system::{system, system_ns};
use common::{Case, Ns};

const FAKE: &str = r#"#!/bin/sh
echo "argv0=$0"
for a; do echo "arg=[$a]"; done
echo "cwd=$(pwd)"
env | LC_ALL=C sort | sed -e 's/^\(RC_OPENRC_PID\|RC_RUNSCRIPT_PID\)=.*/\1=<pid>/' -e '/^\(_\|SHLVL\|PWD\|OLDPWD\)=/d'
sleep 0.3
case "$2" in
fail) echo "failing" >&2; exit 3 ;;
killme) kill -9 $$ ;;
hup) kill -HUP "$RC_OPENRC_PID" ;;
fail2) exit 5 ;;
term) kill -TERM "$RC_OPENRC_PID"; sleep 1; echo "not killed" ;;
lines) printf 'one\ntwo\n\033[Aup\nthree'; sleep 0.2 ;;
esac
exit 0
"#;

fn rr_with(r: &Path, rc_conf: &str, fake: bool) {
    system(r);
    fs::write(r.join("rc.conf"), rc_conf).unwrap();
    if fake {
        let f = r.join("run/openrc-run.sh");
        fs::write(&f, FAKE).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::create_dir_all(r.join("run/hotplugged")).unwrap();
    symlink("/etc/init.d/delta", r.join("run/hotplugged/delta")).unwrap();
    fs::create_dir_all(r.join("run/exclusive")).unwrap();
    fs::write(r.join("run/exclusive/alpha"), "").unwrap();
    fs::write(r.join("run/exclusive/beta"), "").unwrap();
}
fn rr_fake(r: &Path) {
    rr_with(r, "", true);
}
fn rr_real(r: &Path) {
    rr_with(r, "", false);
}
fn rr_parallel(r: &Path) {
    rr_with(r, "rc_parallel=\"YES\"\n", true);
}
fn rr_strict_no(r: &Path) {
    rr_with(r, "rc_depend_strict=\"NO\"\n", true);
}
fn rr_hotplug(r: &Path) {
    rr_with(r, "rc_hotplug=\"!b* net.* *a\"\n", true);
}
fn rr_plug_services(r: &Path) {
    rr_with(r, "rc_plug_services=\"gamma\"\n", true);
}
/// Runlevel starting (rc.starting): no deps; restore_state marks failed.
fn rr_starting(r: &Path) {
    rr_fake(r);
    fs::write(r.join("run/rc.starting"), "").unwrap();
}
fn rr_env(r: &Path) {
    rr_with(r, "rc_env_allow=\"KEEP\"\n", true);
    fs::write(r.join("profile.env"), "export PROFVAR='p'\n").unwrap();
}

fn ns() -> Ns {
    let mut ns = system_ns();
    ns.binds.push(("$ROOT/rc.conf".into(), "/etc/rc.conf".into()));
    ns
}
fn ns_profile() -> Ns {
    let mut ns = ns();
    if Path::new("/etc/profile.env").exists() {
        ns.binds.push(("$ROOT/profile.env".into(), "/etc/profile.env".into()));
    }
    ns
}

/// `openrc-run /etc/init.d/<svc> args...`, with RC_RUNLEVEL set (as rc
/// and rc-service do) so the environment is passed on as is.
fn rr(setup: fn(&Path), svc: &str, args: &[&str]) -> Case {
    let mut a = vec![format!("/etc/init.d/{svc}")];
    a.extend(args.iter().map(|s| s.to_string()));
    let a: Vec<&str> = a.iter().map(String::as_str).collect();
    Case::new("openrc-run", &a).sandbox(setup).ns(ns()).env("TERM", "dumb").env("RC_RUNLEVEL", "default")
}

#[test]
fn openrc_run_args() {
    if common::reference_dir().is_none() {
        return;
    }
    let base = |args: &[&str]| Case::new("openrc-run", args).sandbox(rr_fake).ns(ns()).env("TERM", "dumb");
    let cases = vec![
        base(&[]),
        base(&["/etc/init.d/nosuch", "start"]),
        base(&["/etc/init.d/alpha"]),
        base(&["/etc/init.d/alpha", "-h"]),
        base(&["/etc/init.d/alpha", "-x", "describe"]),
        base(&["/etc/init.d/alpha", "--bogus"]),
        base(&["/etc/init.d/alpha", "-V"]),
        base(&["/etc/init.d/alpha", "-D"]),
        base(&["/etc/init.d/alpha", "-l"]),
        rr(rr_fake, "alpha", &["mycmd", "x"]),
        rr(rr_fake, "alpha", &["-d", "mycmd"]),
        rr(rr_fake, "alpha", &["--debug", "-D", "-Z", "mycmd"]),
        rr(rr_fake, "alpha", &["mycmd", "-D", "other"]),
        rr(rr_fake, "net.eth0", &["mycmd"]),
        rr(rr_fake, "alpha", &["-v", "mycmd"]),
        rr(rr_fake, "alpha", &["-q", "mycmd"]),
        rr(rr_fake, "alpha", &["-C", "mycmd"]),
        // -s / -S: beta is started, epsilon stopped, alpha starting.
        rr(rr_fake, "beta", &["-s", "mycmd"]),
        rr(rr_fake, "beta", &["-S", "mycmd"]),
        rr(rr_fake, "epsilon", &["--ifstopped", "mycmd"]),
        rr(rr_fake, "epsilon", &["--ifstarted", "mycmd"]),
        rr(rr_fake, "alpha", &["-s", "mycmd"]),
        rr(rr_fake, "alpha", &["-S", "mycmd"]),
        // Exit statuses: the last failing command's (QUIRK: a signal
        // death counts as success).
        rr(rr_fake, "alpha", &["fail"]),
        rr(rr_fake, "alpha", &["fail", "mycmd"]),
        rr(rr_fake, "alpha", &["fail", "fail2", "mycmd"]),
        rr(rr_fake, "alpha", &["fail2", "fail"]),
        rr(rr_fake, "alpha", &["killme"]),
        rr(rr_fake, "alpha", &["hup", "mycmd"]),
        rr(rr_fake, "alpha", &["term", "mycmd"]),
        // Environment handling.
        rr(rr_fake, "alpha", &["mycmd"]).env("IN_BACKGROUND", "yes").env("RC_NODEPS", "yes"),
        rr(rr_fake, "alpha", &["mycmd"]).env("IN_BACKGROUND", "no").env("RC_PID", "1"),
        rr(rr_fake, "alpha", &["mycmd"]).env("IN_DRYRUN", "yes").env("EINFO_LOG", "x"),
        rr(rr_fake, "alpha", &["mycmd"]).env("RC_SVCNAME", "other").env("RC_CMD", "old"),
        Case::new("openrc-run", &["/etc/init.d/alpha", "mycmd"])
            .sandbox(rr_env)
            .ns(ns_profile())
            .env("TERM", "dumb")
            .env("KEEP", "1")
            .env("DROP", "2"),
        Case::new("openrc-run", &["/etc/init.d/alpha", "mycmd"]).sandbox(rr_fake).ns(ns()).env("TERM", "dumb"),
        rr(rr_fake, "alpha", &["-U", "mycmd"]).env("HOME", "/"),
        rr(rr_fake, "alpha", &["-U", "mycmd"]),
        // User mode with its directories: cwd is $HOME, which must be set.
        rr(rr_fake, "alpha", &["-U", "mycmd"]).env("XDG_RUNTIME_DIR", "$ROOT/xdg").env("XDG_CONFIG_HOME", "$ROOT/cfg"),
        rr(rr_fake, "alpha", &["-U", "mycmd"])
            .env("XDG_RUNTIME_DIR", "$ROOT/xdg")
            .env("XDG_CONFIG_HOME", "$ROOT/cfg")
            .env("HOME", "$ROOT/run"),
        rr(rr_fake, "alpha", &["-U", "mycmd"])
            .env("XDG_RUNTIME_DIR", "$ROOT/xdg")
            .env("XDG_CONFIG_HOME", "$ROOT/cfg")
            .env("HOME", "/nonexistent"),
        // Hotplug.
        rr(rr_fake, "alpha", &["mycmd"]).env("IN_HOTPLUG", "1"),
        rr(rr_hotplug, "alpha", &["mycmd"]).env("IN_HOTPLUG", "1"),
        rr(rr_hotplug, "beta", &["mycmd"]).env("IN_HOTPLUG", "yes"),
        rr(rr_hotplug, "net.eth0", &["mycmd"]).env("IN_HOTPLUG", "yes"),
        rr(rr_hotplug, "gamma", &["mycmd"]).env("IN_HOTPLUG", "yes"),
        rr(rr_plug_services, "gamma", &["mycmd"]).env("IN_HOTPLUG", "yes"),
        rr(rr_plug_services, "delta", &["mycmd"]).env("IN_HOTPLUG", "yes"),
        // With the lock fd from rc, a half-started service is put back
        // on exit (and marked failed while the runlevel is starting).
        rr(rr_fake, "alpha", &["-l", "99", "mycmd"]),
        rr(rr_fake, "beta", &["--lockfd=99", "mycmd"]),
        rr(rr_starting, "alpha", &["-l", "99", "mycmd"]),
        rr(rr_starting, "alpha", &["-l", "99", "fail"]),
        // rc_parallel prefixes our own messages (no pty here).
        rr(rr_parallel, "alpha", &["zap"]),
        rr(rr_parallel, "alpha", &["lines"]),
        rr(rr_parallel, "alpha", &["describe"]),
    ];
    common::check_all("openrc-run-args", &cases);
}

#[test]
fn openrc_run_commands() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut cases = Vec::new();
    for svc in ["alpha", "beta", "gamma", "delta", "net.eth0", "epsilon"] {
        for cmd in ["describe", "status"] {
            cases.push(rr(rr_real, svc, &[cmd]));
        }
    }
    cases.extend([
        rr(rr_real, "beta", &["reload"]),
        rr(rr_real, "alpha", &["help"]),
        rr(rr_real, "alpha", &["depend"]),
        rr(rr_real, "alpha", &["describe", "status"]),
        rr(rr_real, "alpha", &["nosuchcmd"]),
        rr(rr_fake, "alpha", &["describe", "help", "depend", "status"]),
        // zap: alpha is starting, gamma failed, delta inactive and
        // hotplugged, epsilon stopped.
        rr(rr_fake, "alpha", &["zap"]),
        rr(rr_fake, "gamma", &["zap"]),
        rr(rr_fake, "delta", &["zap"]),
        rr(rr_fake, "epsilon", &["zap"]),
        rr(rr_fake, "net.eth0", &["zap", "mycmd"]),
        rr(rr_fake, "alpha", &["-q", "zap"]),
    ]);
    // Dependency queries.
    for svc in ["alpha", "beta", "gamma", "delta", "epsilon", "net.eth0", "loop1", "broken"] {
        for t in ["ineed", "iuse", "iwant", "needsme", "usesme", "wantsme", "iafter", "ibefore", "iprovide"] {
            cases.push(rr(rr_fake, svc, &[t]));
        }
    }
    cases.extend([
        rr(rr_fake, "alpha", &["ineed", "iuse", "usesme"]),
        rr(rr_strict_no, "alpha", &["ineed", "iuse"]),
        rr(rr_strict_no, "beta", &["needsme", "usesme", "ibefore"]),
        rr(rr_fake, "alpha", &["ineed"]).env("RC_RUNLEVEL", "nonetwork"),
        rr(rr_fake, "beta", &["needsme"]).env("RC_RUNLEVEL", "boot"),
        Case::new("openrc-run", &["/etc/init.d/beta", "needsme", "ibefore"]).sandbox(rr_fake).ns(ns()),
    ]);
    common::check_all("openrc-run-commands", &cases);
}

/// rc_parallel on a terminal: the script's output goes through a pty and
/// each line is prefixed (except eend's cursor-up lines).
#[test]
fn openrc_run_pty() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        rr(rr_parallel, "alpha", &["lines"]).pty(),
        rr(rr_parallel, "alpha", &["lines"]).env("TERM", "xterm").pty(),
        rr(rr_parallel, "net.eth0", &["mycmd"]).pty(),
        // describe & co run unprefixed.
        rr(rr_parallel, "alpha", &["describe", "lines"]).pty(),
        rr(rr_real, "beta", &["describe"]).env("TERM", "xterm").pty(),
        rr(rr_fake, "alpha", &["lines"]).pty(),
    ];
    common::check_all("openrc-run-pty", &cases);
}

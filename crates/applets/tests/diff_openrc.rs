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
//!
//! sysinit runs `$RC_LIBEXECDIR/sh/init.sh`: a fake is bound over that
//! directory, and a fake /proc/cmdline over the real one.  The logger's
//! log is compared byte for byte, with a `time()` that always returns the
//! same instant; interactive boot types its keys on a pty.

mod common;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

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

// --- sysinit, the logger, interactive boot -----------------------------------

/// `time()` fixed at 2001-09-09 01:46:40 UTC, for the log's timestamps.
fn fixed_time() -> &'static str {
    static SO: OnceLock<PathBuf> = OnceLock::new();
    SO.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(env!("CARGO_CRATE_NAME"));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("fixedtime.c");
        fs::write(&src, "#include <time.h>\ntime_t time(time_t *t) { if (t) *t = 1000000000; return 1000000000; }\n")
            .unwrap();
        let so = dir.join("fixedtime.so");
        let ok = Command::new("cc").args(["-shared", "-fPIC", "-O2", "-o"]).arg(&so).arg(&src).status().unwrap();
        assert!(ok.success(), "building the time() shim failed");
        so
    })
    .to_str()
    .unwrap()
}

/// A copy of the system's `$RC_LIBEXECDIR/sh` (gendepends.sh and the
/// rest are still needed) with fake init.sh and init-early.sh.
fn init_scripts(r: &Path, early: bool) {
    let sh = r.join("sh");
    fs::create_dir_all(&sh).unwrap();
    for e in fs::read_dir("/usr/libexec/rc/sh").unwrap().flatten() {
        if e.file_type().unwrap().is_file() && e.file_name() != "init-early.sh" {
            fs::copy(e.path(), sh.join(e.file_name())).unwrap();
        }
    }
    let body = |name: &str| {
        format!(
            "#!/bin/sh\necho \"[{name}] level=$RC_RUNLEVEL sys=${{RC_SYS-unset}} args=$# \\
             int=$(trap -p INT | wc -l)\"\n[ -n \"$INIT_FAIL\" ] && exit 3\n\
             [ -n \"$START_SOFTLEVEL\" ] && echo \"$START_SOFTLEVEL\" > /run/openrc/softlevel\nexit 0\n"
        )
    };
    let mut names = vec!["init.sh"];
    if early {
        names.push("init-early.sh");
    }
    for n in names {
        let f = sh.join(n);
        fs::write(&f, body(n)).unwrap();
        fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
}
fn sysinit(r: &Path) {
    plain(r);
    init_scripts(r, false);
}
fn sysinit_early(r: &Path) {
    plain(r);
    init_scripts(r, true);
}
/// omega (stopped) is a sysinit service, so it's asked about.
fn sysinit_interactive(r: &Path) {
    with(r, "rc_interactive=\"YES\"\n");
    init_scripts(r, false);
    script(&r.join("init.d/omega"), "");
    symlink("/etc/init.d/omega", r.join("runlevels/sysinit/omega")).unwrap();
}
/// init.sh can't be run.
fn sysinit_noexec(r: &Path) {
    plain(r);
    init_scripts(r, false);
    fs::set_permissions(r.join("sh/init.sh"), std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
}
fn sysinit_no_deptree(r: &Path) {
    no_deptree(r);
    init_scripts(r, false);
}
/// With the test plugin, which prints the runlevel each hook gets.
fn sysinit_plugins(r: &Path) {
    plugins(r);
    init_scripts(r, false);
}
fn sysinit_lxc(r: &Path) {
    with(r, "rc_sys=\"lxc\"\n");
    init_scripts(r, false);
}
fn cmdline_softlevel(r: &Path) {
    sysinit(r);
    fs::write(r.join("cmdline"), "ro softlevel=nonetwork quiet\n").unwrap();
}
fn cmdline_rc_runlevel(r: &Path) {
    sysinit(r);
    fs::write(r.join("cmdline"), "rc_runlevel=nonetwork softlevel=single\n").unwrap();
}
fn cmdline_single(r: &Path) {
    sysinit(r);
    fs::write(r.join("cmdline"), "softlevel=single\n").unwrap();
}
fn cmdline_noinit(r: &Path) {
    plain(r);
    // gamma's failed mark is cleaned first; noinit marks it started.
    fs::write(r.join("cmdline"), "noinit=gamma,nosuch,\n").unwrap();
}

const LOGGER: &str = "rc_logger=\"YES\"\nrc_log_path=\"/run/openrc/final.log\"\n";
/// The logger (with START_ESC, services print escapes, `\r`, a tab and
/// UTF-8).
fn logger(r: &Path) {
    with(r, LOGGER);
    fs::write(r.join("run/final.log"), "earlier\n").unwrap();
}
/// psi (stopped) is a shutdown service: it starts after the logger has
/// closed.
fn logger_shutdown(r: &Path) {
    logger(r);
    script(&r.join("init.d/psi"), "");
    symlink("/etc/init.d/psi", r.join("runlevels/shutdown/psi")).unwrap();
}
/// The temporary log is a dangling symlink until a service makes its
/// directory: until then the log is kept in memory.
fn logger_late(r: &Path) {
    logger(r);
    symlink("later/rc.log", r.join("run/rc.log")).unwrap();
}
/// The default rc_log_path, with /var/log bound to `$ROOT/varlog`.
fn logger_default_varlog(r: &Path) {
    logger_conf(r, "");
    fs::create_dir(r.join("varlog")).unwrap();
}
fn logger_init(r: &Path) {
    logger(r);
    init_scripts(r, false);
}
fn logger_conf(r: &Path, conf: &str) {
    with(r, &format!("rc_logger=\"YES\"\n{conf}"));
}
fn logger_self(r: &Path) {
    logger_conf(r, "rc_log_path=\"/run/openrc/rc.log\"\n");
}
fn logger_bad_path(r: &Path) {
    logger_conf(r, "rc_log_path=\"/proc/openrc-rs-nonexistent/rc.log\"\n");
}
fn logger_default(r: &Path) {
    logger_conf(r, "");
}
/// The temporary log is a directory: kept in memory, then not writable.
fn logger_tmp_dir(r: &Path) {
    logger(r);
    fs::create_dir(r.join("run/rc.log")).unwrap();
}
/// User services log to `$XDG_STATE_HOME/rc.log` or
/// `~/.local/state/rc.log`; only the last directory is made (QUIRK).
fn logger_user(r: &Path) {
    plain(r);
    fs::create_dir_all(r.join(".config/rc/runlevels/default")).unwrap();
    fs::create_dir_all(r.join("xdg")).unwrap();
    fs::write(r.join(".config/rc/rc.conf"), "rc_logger=\"YES\"\n").unwrap();
}
fn logger_user_local(r: &Path) {
    logger_user(r);
    fs::create_dir(r.join(".local")).unwrap();
}

/// Interactive boot: rc_shell prints a line.  chi (stopped) is in boot.
fn ask(r: &Path) {
    with(r, "rc_interactive=\"YES\"\nrc_shell=\"/run/openrc/shell\"\n");
    let f = r.join("run/shell");
    // (It pauses first: a shell that exits at once can beat openrc's wait,
    // a race in C too; see QUIRKS.md.)
    fs::write(&f, "#!/bin/sh\nsleep 0.3\necho \"[shell] args=$#\"\n").unwrap();
    fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    script(&r.join("init.d/chi"), "");
    symlink("/etc/init.d/chi", r.join("runlevels/boot/chi")).unwrap();
}
fn ask_marked(r: &Path) {
    ask(r);
    fs::write(r.join("run/interactive"), "").unwrap();
}
/// No rc_shell: $SHELL (let through the environment filter) is used.
fn ask_env_shell(r: &Path) {
    ask(r);
    fs::write(r.join("rc.conf"), "rc_interactive=\"YES\"\nrc_env_allow=\"START_* STOP_* SHELL\"\n").unwrap();
}
/// The shell fails (after a moment: one that can't even be exec'd exits
/// before openrc waits for it, which is a race, see QUIRKS.md).
fn ask_failing_shell(r: &Path) {
    ask(r);
    fs::write(r.join("run/shell"), "#!/bin/sh\nsleep 0.3\necho \"[shell] failing\"\nexit 7\n").unwrap();
}

fn bind(mut c: Case, src: &str, dst: &str) -> Case {
    c.ns.as_mut().unwrap().binds.push((src.into(), dst.into()));
    c
}
fn with_sh(c: Case) -> Case {
    bind(c, "$ROOT/sh", "/usr/libexec/rc/sh")
}
fn with_cmdline(c: Case) -> Case {
    bind(c, "$ROOT/cmdline", "/proc/cmdline")
}
fn logged(c: Case) -> Case {
    c.env("LD_PRELOAD", fixed_time()).file("run/final.log").file("run/rc.log")
}

#[test]
fn openrc_sysinit() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases: Vec<Case> = vec![
        with_sh(rc(sysinit, &["sysinit"])),
        with_sh(rc(sysinit_early, &["sysinit"])),
        with_sh(rc(sysinit, &["-q", "sysinit"])),
        with_sh(rc(sysinit, &["sysinit"])).env("INIT_FAIL", "1"),
        with_sh(rc(sysinit_interactive, &["sysinit"])),
        with_sh(rc(sysinit_interactive, &["-q", "sysinit"])),
        with_sh(rc(sysinit_interactive, &["sysinit"])).pty_input(b"I1").file("run/interactive"),
        with_sh(rc(sysinit_noexec, &["sysinit"])),
        with_sh(rc(sysinit_lxc, &["sysinit"])),
        // init.sh may set the runlevel: it's read again afterwards.
        with_sh(rc(sysinit_plugins, &["sysinit"])).env("START_SOFTLEVEL", "nonetwork"),
        with_sh(rc(sysinit_lxc, &["-q", "sysinit"])),
        with_sh(rc(sysinit, &["sysinit"])).pty().env("TERM", "xterm"),
        with_sh(rc(sysinit_no_deptree, &["sysinit"])),
        // The kernel command line: rc_runlevel= or softlevel= becomes the
        // next runlevel (unless it's single); noinit= skips services.
        with_cmdline(with_sh(rc(cmdline_softlevel, &["sysinit"]))).file("run/krunlevel"),
        with_cmdline(with_sh(rc(cmdline_rc_runlevel, &["sysinit"]))).file("run/krunlevel"),
        with_cmdline(with_sh(rc(cmdline_single, &["sysinit"]))).file("run/krunlevel"),
        with_cmdline(rc(cmdline_noinit, &[])),
        with_cmdline(rc(cmdline_noinit, &["single"])),
    ];
    common::check_all("openrc-sysinit", &cases);
}

#[test]
fn openrc_logger() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut var_log = common::Ns::default();
    var_log.tmpfs.push("/var/log".into());
    let mut default_path = rc(logger_default, &["nonetwork"]);
    default_path.ns.as_mut().unwrap().tmpfs.push("/var/log".into());
    let cases: Vec<Case> = vec![
        logged(rc(logger, &[])),
        logged(rc(logger, &[]).env("START_ESC", "1")),
        logged(rc(logger, &[]).env("START_ESC", "1").pty().env("TERM", "xterm")),
        logged(rc(logger_shutdown, &["shutdown"])).env("START_FAIL", "psi"),
        logged(rc(logger_shutdown, &["shutdown"])).pty(),
        logged(rc(logger_late, &[]))
            .env("START_MKDIR", "/run/openrc/later")
            .env("START_ESC", "1")
            .file("run/later/rc.log"),
        logged(rc(logger, &[])).pty().env("START_WINCH", "1"),
        bind(rc(logger_default_varlog, &["nonetwork"]), "$ROOT/varlog", "/var/log")
            .env("LD_PRELOAD", fixed_time())
            .file("varlog/rc.log"),
        logged(rc(logger, &["nonetwork"])),
        logged(rc(logger, &["shutdown"])),
        logged(rc(logger, &["-q"])),
        logged(rc(logger, &[]).pty().env("TERM", "xterm")),
        logged(with_sh(rc(logger_init, &["sysinit"]))),
        logged(rc(logger, &["nosuch"])),
        // (Not SIGUSR1: openrc exits without waiting for the services it
        // kills, so whether their last words reach the logger is a race.)
        logged(rc(logger_self, &[])),
        logged(rc(logger_bad_path, &[])),
        logged(rc(logger_bad_path, &["shutdown"])),
        logged(rc(logger_tmp_dir, &[])),
        default_path.file("run/rc.log"),
        logged(rc(logger_user, &["-U"]))
            .env("HOME", "$ROOT")
            .env("XDG_RUNTIME_DIR", "$ROOT/xdg")
            .file(".local/state/rc.log"),
        logged(rc(logger_user_local, &["-U"]))
            .env("HOME", "$ROOT")
            .env("XDG_RUNTIME_DIR", "$ROOT/xdg")
            .file(".local/state/rc.log"),
        logged(rc(logger_user, &["-U"]))
            .env("HOME", "$ROOT")
            .env("XDG_RUNTIME_DIR", "$ROOT/xdg")
            .env("XDG_STATE_HOME", "$ROOT/state/deep")
            .file("state/deep/rc.log"),
    ];
    common::check_all("openrc-logger", &cases);
}

#[test]
fn openrc_interactive() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases: Vec<Case> = vec![
        // Not asked: no I; not a terminal.
        rc(ask, &[]).pty_input(b"x"),
        rc(ask, &[]),
        // I, then for gamma and net.eth0: start / skip; continue; junk
        // keys first; the shell, then start.
        rc(ask, &[]).pty_input(b"I12"),
        rc(ask, &[]).pty_input(b"i22"),
        rc(ask, &[]).pty_input(b"I3"),
        rc(ask, &[]).pty_input(b"Iqz91x2"),
        rc(ask, &[]).pty_input(b"I413"),
        rc(ask, &[]).pty_input(b"I3").env("SHELL", "/bin/false"),
        // Quiet: never asked.
        rc(ask, &["-q"]).pty_input(b"I"),
        // The boot runlevel remembers it (`interactive`), then default
        // asks without the I.
        rc(ask, &["boot"]).pty_input(b"I1").file("run/interactive"),
        rc(ask_marked, &[]).pty_input(b"21").file("run/interactive"),
        rc(ask_marked, &["-q"]).pty().file("run/interactive"),
        rc(ask_failing_shell, &[]).pty_input(b"I43"),
        rc(ask_env_shell, &[]).pty_input(b"I43").env("SHELL", "/run/openrc/shell"),
        // The terminal is back to normal for the service a key started.
        rc(ask, &[]).pty_input(b"I12").env("START_STTY", "1"),
    ];
    common::check_all("openrc-interactive", &cases);
}

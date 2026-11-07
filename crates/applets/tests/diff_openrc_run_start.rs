//! openrc-run start (Phase 4 step 2): openrc-run vs the C program.
//!
//! Dependencies are started for real: `/sbin/openrc-run` inside the
//! namespace is the program under test (`$BIN`), so in each run the
//! services `start` spawns are run by the same implementation.  Each
//! script is a fake `openrc-run.sh` in the service directory that prints
//! a line per call and fails or marks itself on request (`START_FAIL`,
//! `START_MARK`).
//!
//! Plugins: the plugin directory is compiled in (`/usr/lib64/rc/plugins`,
//! which doesn't exist here and can't be created), so both programs are
//! run as copies with that string patched to the same-length
//! `/run/openrc/plugins.d`, inside the sandbox.  The test plugin is built
//! with cc; it prints each hook it gets and sends variables back.
//!
//! Waiting for a service times out after 60 seconds, warning every 10:
//! those cases preload an `alarm()` that counts milliseconds instead.

mod common;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use common::system::{script, system, system_ns};
use common::{Case, Ns};

const PLUGINDIR: &[u8] = b"/usr/lib64/rc/plugins";
const PATCHED: &[u8] = b"/run/openrc/plugins.d";

const FAKE: &str = r#"#!/bin/sh
[ -e "/run/openrc/slow.$RC_SVCNAME" ] && sleep 0.5
echo "[$RC_SVCNAME $2] argv0=$0 bg=${IN_BACKGROUND-unset} hotplug=${IN_HOTPLUG-unset} cmd=${RC_CMD-unset} reexport=${RC_REEXPORT-unset} plug=${PLUGVAR-unset} abc=${A-unset} beta=${BETAVAR-unset} level=${RC_RUNLEVEL-unset}"
case "$2" in
start)
	for s in $START_FAIL; do
		[ "$s" = "$RC_SVCNAME" ] && { echo "$RC_SVCNAME: start failed" >&2; exit 1; }
	done
	for s in $START_MARK; do
		case "$s" in "$RC_SVCNAME":*) /usr/libexec/rc/sbin/mark_service_"${s#*:}" ;; esac
	done
	for s in $START_RUN; do
		case "$s" in "$RC_SVCNAME":*) s=${s#*:}; /etc/init.d/${s%%,*} $(echo "${s#*,}" | tr , ' '); echo "nested: $?" ;; esac
	done
	;;
esac
exit 0
"#;

const PLUGIN: &str = r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>
extern FILE *rc_environ_fd;
static void put(const char *s) { fputs(s, rc_environ_fd); fputc(0, rc_environ_fd); }
int rc_plugin_hook(int hook, const char *name)
{
	const char *mode = getenv("PLUG_MODE");
	const char *v = getenv("PLUGVAR");
	printf("plugin %d %s plug=%s\n", hook, name, v ? v : "unset");
	if (hook == 105) {
		fprintf(rc_environ_fd, "PLUGVAR=in-%s", name);
		fputc(0, rc_environ_fd);
		put("A=B=C");
		put("RC_CMD=");
	}
	if (hook == 107 && mode && strcmp(mode, "unset") == 0)
		put("PLUGVAR=");
	if (hook == 106 && mode && strcmp(mode, "noeq") == 0)
		put("NOEQUALS");
	return hook == 108 ? 3 : 0;
}
"#;

fn build_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("openrc-run-start");
    fs::create_dir_all(&d).unwrap();
    d
}

/// Copy `from` to `to` with the plugin directory patched.
fn patch(from: &Path, to: &Path) {
    let mut data = fs::read(from).unwrap();
    let mut n = 0;
    let mut i = 0;
    while let Some(off) = data[i..].windows(PLUGINDIR.len()).position(|w| w == PLUGINDIR) {
        let at = i + off;
        data[at..at + PLUGINDIR.len()].copy_from_slice(PATCHED);
        i = at + PLUGINDIR.len();
        n += 1;
    }
    assert!(n > 0, "{}: plugin directory not found", from.display());
    let tmp = to.with_extension("tmp");
    fs::write(&tmp, data).unwrap();
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&tmp, to).unwrap();
}

/// The patched (C, Rust) programs.
fn programs() -> &'static (PathBuf, PathBuf) {
    static P: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    P.get_or_init(|| {
        let refdir = common::reference_dir().unwrap();
        // Next to the original, so its $ORIGIN RUNPATH still finds librc.
        let c = refdir.join("openrc-run/openrc-run.plugtest");
        patch(&refdir.join("openrc-run/openrc-run"), &c);
        let rust = build_dir().join("openrc-rs");
        patch(Path::new(common::OURS), &rust);
        (c, rust)
    })
}

/// The test plugin, linked against the reference librc (as plugins link
/// librc.so.1, for `rc_environ_fd`).
fn plugin_so() -> &'static Path {
    static SO: OnceLock<PathBuf> = OnceLock::new();
    SO.get_or_init(|| {
        let dir = build_dir();
        let src = dir.join("plugin.c");
        fs::write(&src, PLUGIN).unwrap();
        let so = dir.join("plugin.so");
        let librc = common::reference_dir().unwrap().join("librc");
        let ok = Command::new("cc")
            .args(["-shared", "-fPIC", "-O2", "-o"])
            .arg(&so)
            .arg(&src)
            .arg(format!("-L{}", librc.display()))
            .arg(format!("-Wl,-rpath,{}", librc.display()))
            .arg("-lrc")
            .status()
            .unwrap();
        assert!(ok.success(), "building the test plugin failed");
        so
    })
}

/// `alarm(n)` as `ualarm(n ms)`, so svc_wait's timeouts take 60ms.
fn alarm_shim() -> &'static Path {
    static SO: OnceLock<PathBuf> = OnceLock::new();
    SO.get_or_init(|| {
        let dir = build_dir();
        let src = dir.join("fastalarm.c");
        fs::write(
            &src,
            "#define _DEFAULT_SOURCE\n#include <unistd.h>\n\
             unsigned int alarm(unsigned int s) { return ualarm(s * 1000, 0) / 1000; }\n",
        )
        .unwrap();
        let so = dir.join("fastalarm.so");
        let ok = Command::new("cc").args(["-shared", "-fPIC", "-O2", "-o"]).arg(&so).arg(&src).status().unwrap();
        assert!(ok.success(), "building the alarm() shim failed");
        so
    })
}

fn start_with(r: &Path, rc_conf: &str) {
    system(r);
    fs::write(r.join("rc.conf"), rc_conf).unwrap();
    let f = r.join("run/openrc-run.sh");
    fs::write(&f, FAKE).unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    let initd = r.join("init.d");
    // alpha reexports two variables and beta one, which it has saved.
    script(
        &initd.join("alpha"),
        "description=\"The alpha service\"\ndepend() {\n\tneed beta\n\tuse gamma\n\tafter delta\n\treexport AVAR BVAR\n}",
    );
    script(
        &initd.join("beta"),
        "description=\"Beta\"\nextra_commands=\"reload check\"\ndepend() {\n\tprovide virt\n\tbefore gamma\n\treexport BETAVAR NOSAVED\n}",
    );
    fs::create_dir_all(r.join("run/environ/beta")).unwrap();
    fs::write(r.join("run/environ/beta/BETAVAR"), "from-beta").unwrap();
    // zeta needs the inactive delta; eta uses it; theta needs two
    // inactive services; iota needs the stopped net.eth0.
    script(&initd.join("zeta"), "depend() {\n\tneed delta\n}");
    script(&initd.join("eta"), "depend() {\n\tuse delta\n}");
    script(&initd.join("kappa"), "depend() {\n\tprovide kprov\n}");
    script(&initd.join("theta"), "depend() {\n\tneed delta kappa\n}");
    script(&initd.join("iota"), "depend() {\n\tneed net.eth0\n}");
    // lambda needs mu, nu uses gamma (which has no timeout); both are slow
    // to start, so in parallel we wait for them.
    script(&initd.join("lambda"), "depend() {\n\tneed mu\n}");
    script(&initd.join("mu"), "");
    script(&initd.join("nu"), "depend() {\n\tuse gamma\n}");
    // omicron is after the started beta (so takes its reexports); broken
    // needs two missing services.
    script(&initd.join("omicron"), "depend() {\n\tafter beta\n}");
    script(&initd.join("broken"), "depend() {\n\tneed nosuchservice nosuch2\n}");
    symlink("/etc/init.d/kappa", r.join("run/inactive/kappa")).unwrap();
    fs::write(r.join("run/exclusive/beta"), "").unwrap();
}
fn st_plain(r: &Path) {
    start_with(r, "");
}
fn st_parallel(r: &Path) {
    start_with(r, "rc_parallel=\"YES\"\n");
}
fn st_parallel_slow(r: &Path) {
    st_parallel(r);
    for svc in ["mu", "gamma"] {
        fs::write(r.join(format!("run/slow.{svc}")), "").unwrap();
    }
}
/// net.eth0 is stopping (with nobody holding its lock).
fn st_stopping(r: &Path) {
    st_plain(r);
    fs::create_dir_all(r.join("run/stopping")).unwrap();
    symlink("/etc/init.d/net.eth0", r.join("run/stopping/net.eth0")).unwrap();
}
fn st_hotplug(r: &Path) {
    start_with(r, "rc_hotplug=\"*\"\n");
}
fn st_strict_no(r: &Path) {
    start_with(r, "rc_depend_strict=\"NO\"\n");
}
/// Runlevel starting: deps only with RC_PID; use deps are not started.
fn st_starting(r: &Path) {
    st_plain(r);
    fs::write(r.join("run/rc.starting"), "").unwrap();
}
/// Services scheduled to start after net.eth0 and after "net" (which it
/// provides); the scheduled ones are slow, so their output comes last.
fn st_scheduled(r: &Path) {
    st_plain(r);
    for (after, svc) in [("net.eth0", "epsilon"), ("net.eth0", "beta"), ("net", "zeta")] {
        fs::create_dir_all(r.join("run/scheduled").join(after)).unwrap();
        symlink(format!("/etc/init.d/{svc}"), r.join("run/scheduled").join(after).join(svc)).unwrap();
        fs::write(r.join(format!("run/slow.{svc}")), "").unwrap();
    }
}
/// The lock files of net.eth0 and gamma are directories, kappa's a
/// symlink loop: the lock can't be taken (EISDIR, ELOOP).
fn st_no_exclusive(r: &Path) {
    st_plain(r);
    for svc in ["net.eth0", "gamma"] {
        fs::create_dir(r.join("run/exclusive").join(svc)).unwrap();
    }
    symlink("kappa", r.join("run/exclusive/kappa")).unwrap();
}
/// The deptree can't be loaded or written.
fn st_no_deptree(r: &Path) {
    st_plain(r);
    fs::create_dir(r.join("run/deptree")).unwrap();
}
fn st_plugins(r: &Path) {
    st_plain(r);
    let d = r.join("run/plugins.d");
    fs::create_dir_all(&d).unwrap();
    fs::copy(plugin_so(), d.join("test.so")).unwrap();
    // Skipped: a dot file.
    fs::write(d.join(".hidden.so"), "junk").unwrap();
}
fn st_bad_plugins(r: &Path) {
    st_plugins(r);
    let d = r.join("run/plugins.d");
    fs::write(d.join("junk.so"), "not an ELF file").unwrap();
    // A shared object without the hook.
    fs::copy("/usr/lib64/libeinfo.so.1", d.join("nohook.so"))
        .or_else(|_| fs::copy("/lib64/libeinfo.so.1", d.join("nohook.so")))
        .unwrap();
}

fn ns() -> Ns {
    let mut ns = system_ns();
    ns.binds.push(("$ROOT/rc.conf".into(), "/etc/rc.conf".into()));
    ns.binds.push(("$BIN".into(), "/sbin/openrc-run".into()));
    ns
}

/// `openrc-run /etc/init.d/<svc> args...` with RC_RUNLEVEL=default.
fn st(setup: fn(&Path), svc: &str, args: &[&str]) -> Case {
    let (c, rust) = programs().clone();
    let mut a = vec![format!("/etc/init.d/{svc}")];
    a.extend(args.iter().map(|s| s.to_string()));
    let a: Vec<&str> = a.iter().map(String::as_str).collect();
    Case::programs("openrc-run", c, rust, &a).sandbox(setup).ns(ns()).env("TERM", "dumb").env("RC_RUNLEVEL", "default")
}

#[test]
fn openrc_run_start() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        // Plain starts: net.eth0 and kappa have no deps.  alpha needs the
        // started beta (taking its reexports), starts gamma (failed, so
        // stopped) and is after the inactive delta.
        st(st_plain, "net.eth0", &["start"]),
        st(st_plain, "net.eth0", &["start", "start"]),
        st(st_plain, "net.eth0", &["start", "mycmd"]),
        st(st_plain, "alpha", &["start"]),
        st(st_plain, "gamma", &["start"]),
        st(st_plain, "iota", &["start"]),
        st(st_plain, "kappa", &["start"]).env("IN_BACKGROUND", "yes"),
        st(st_strict_no, "alpha", &["start"]),
        // Already started; inactive without IN_BACKGROUND.
        st(st_plain, "beta", &["start"]),
        st(st_plain, "delta", &["start"]),
        st(st_plain, "beta", &["-q", "start"]),
        // Needs: epsilon needs alpha, which is starting and doesn't start;
        // broken needs a service that doesn't exist.
        st(st_plain, "epsilon", &["start"]),
        st(st_plain, "broken", &["start"]),
        st(st_plain, "broken", &["-Z", "start"]),
        // Scheduling: zeta needs the inactive delta, theta that and the
        // inactive kappa (which provides kprov); eta only uses delta.
        st(st_plain, "zeta", &["start"]),
        st(st_plain, "theta", &["start"]),
        st(st_plain, "eta", &["start"]),
        st(st_plain, "omicron", &["start"]),
        // While the runlevel is starting, the scheduled service is failed.
        st(st_starting, "zeta", &["start"]).env("RC_PID", "1"),
        // Without deps.
        st(st_plain, "epsilon", &["-D", "start"]),
        st(st_plain, "alpha", &["--nodeps", "start"]),
        st(st_plain, "alpha", &["start"]).env("RC_NODEPS", "yes"),
        st(st_starting, "alpha", &["start"]),
        st(st_starting, "alpha", &["start"]).env("RC_PID", "1"),
        // Dry runs: what would be started.
        st(st_plain, "alpha", &["-Z", "start"]),
        st(st_plain, "alpha", &["--dry-run", "start"]),
        st(st_plain, "delta", &["start"]).env("IN_DRYRUN", "yes").env("IN_BACKGROUND", "yes"),
        st(st_plain, "net.eth0", &["-Z", "-D", "start"]),
        st(st_starting, "alpha", &["-Z", "start"]).env("RC_PID", "1"),
        // In the background: only from stopped or inactive; not in sysinit.
        st(st_plain, "delta", &["start"]).env("IN_BACKGROUND", "yes"),
        st(st_plain, "beta", &["start"]).env("IN_BACKGROUND", "true"),
        st(st_plain, "alpha", &["start"]).env("IN_BACKGROUND", "1"),
        st(st_plain, "net.eth0", &["start"]).env("IN_BACKGROUND", "no"),
        st(st_plain, "net.eth0", &["start"]).env("IN_BACKGROUND", "yes").env("RC_RUNLEVEL", "sysinit"),
        st(st_hotplug, "net.eth0", &["start"]).env("IN_HOTPLUG", "yes"),
        st(st_hotplug, "delta", &["start"]).env("IN_HOTPLUG", "yes"),
        // The script fails, or marks itself (and so we don't mark it).
        st(st_plain, "net.eth0", &["start"]).env("START_FAIL", "net.eth0"),
        st(st_plain, "alpha", &["start"]).env("START_FAIL", "gamma"),
        st(st_plain, "iota", &["start"]).env("START_FAIL", "net.eth0"),
        st(st_plain, "net.eth0", &["start", "mycmd"]).env("START_FAIL", "net.eth0"),
        st(st_plain, "net.eth0", &["start"]).env("START_MARK", "net.eth0:inactive"),
        st(st_plain, "net.eth0", &["start"]).env("START_MARK", "net.eth0:stopped"),
        st(st_plain, "net.eth0", &["start"]).env("START_MARK", "net.eth0:failed"),
        st(st_plain, "net.eth0", &["start"]).env("START_MARK", "net.eth0:inactive").env("START_FAIL", "net.eth0"),
        st(st_plain, "iota", &["start"]).env("START_MARK", "net.eth0:inactive"),
        // Scheduled services start once we have.
        st(st_scheduled, "net.eth0", &["start"]),
        st(st_scheduled, "net.eth0", &["start"]).env("START_FAIL", "net.eth0"),
        // rc_parallel: use deps aren't waited for, but then waited on.
        st(st_parallel, "alpha", &["start"]),
        // The lock is taken: a dependency's start runs ours again (with
        // --nodeps that's fine: exit 0).
        st(st_plain, "alpha", &["start"]).env("START_RUN", "gamma:alpha,start"),
        st(st_plain, "alpha", &["start"]).env("START_RUN", "gamma:alpha,--nodeps,start"),
        st(st_plain, "iota", &["start"]).env("START_RUN", "net.eth0:net.eth0,start"),
        st(st_plain, "iota", &["start"])
            .env("START_MARK", "net.eth0:stopping")
            .env("START_RUN", "net.eth0:net.eth0,start"),
        st(st_stopping, "net.eth0", &["start"]),
        // Waiting: mu takes too long (warnings, then a timeout); gamma has
        // no timeout.
        st(st_parallel_slow, "lambda", &["start"]).env("LD_PRELOAD", alarm_shim().to_str().unwrap()),
        st(st_parallel_slow, "nu", &["start"]).env("LD_PRELOAD", alarm_shim().to_str().unwrap()),
        st(st_parallel_slow, "lambda", &["-q", "start"]).env("LD_PRELOAD", alarm_shim().to_str().unwrap()),
        // Lock and deptree trouble.
        st(st_no_exclusive, "net.eth0", &["start"]),
        st(st_no_exclusive, "alpha", &["start"]),
        st(st_no_exclusive, "kappa", &["start"]).env("IN_BACKGROUND", "yes"),
        st(st_no_deptree, "net.eth0", &["start"]),
        st(st_no_deptree, "net.eth0", &["-D", "start"]),
        // With rc's lock.
        st(st_plain, "net.eth0", &["-l", "99", "start"]),
    ];
    common::check_all("openrc-run-start", &cases);
}

#[test]
fn openrc_run_start_plugins() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        st(st_plugins, "net.eth0", &["start"]),
        st(st_plugins, "alpha", &["start"]),
        st(st_plugins, "beta", &["start"]),
        st(st_plugins, "epsilon", &["start"]),
        st(st_plugins, "zeta", &["start"]),
        st(st_plugins, "net.eth0", &["-Z", "start"]),
        st(st_plugins, "net.eth0", &["start"]).env("START_FAIL", "net.eth0"),
        st(st_plugins, "net.eth0", &["start"]).env("START_MARK", "net.eth0:inactive"),
        st(st_plugins, "net.eth0", &["start", "mycmd"]).env("PLUG_MODE", "unset"),
        // A record without '=' (QUIRK: C crashes).
        st(st_plugins, "net.eth0", &["start"]).env("PLUG_MODE", "noeq"),
        // Hooks only run for start (and stop).
        st(st_plugins, "net.eth0", &["mycmd", "zap"]),
        st(st_bad_plugins, "net.eth0", &["start"]),
        st(st_bad_plugins, "net.eth0", &["describe"]),
    ];
    common::check_all("openrc-run-start-plugins", &cases);
}

//! The fixture shared by the openrc-run start and stop tests: the
//! `system()` services plus more, a fake `openrc-run.sh` (one line per
//! call; `START_FAIL`/`STOP_FAIL`, `*_MARK` and `*_RUN` make a service's
//! start or stop fail, mark itself, run a nested command, or `*_SIG`
//! signal openrc's `$RC_PID`), the
//! plugin-dir-patched programs, a test plugin and a fast `alarm()`.
//!
//! See diff_openrc_run_start.rs for how dependencies and plugins are run.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use super::system::{script, system, system_ns};
use super::{Case, Ns};

pub const PLUGINDIR: &[u8] = b"/usr/lib64/rc/plugins";
pub const PATCHED: &[u8] = b"/run/openrc/plugins.d";

pub const FAKE: &str = r#"#!/bin/sh
[ -e "/run/openrc/slow.$RC_SVCNAME" ] && { t=$(cat "/run/openrc/slow.$RC_SVCNAME"); sleep "${t:-0.5}"; }
echo "[$RC_SVCNAME $2] argv0=$0 bg=${IN_BACKGROUND-unset} hotplug=${IN_HOTPLUG-unset} cmd=${RC_CMD-unset} reexport=${RC_REEXPORT-unset} plug=${PLUGVAR-unset} abc=${A-unset} beta=${BETAVAR-unset} level=${RC_RUNLEVEL-unset} lc=${LC_ALL-unset} reboot=${RC_REBOOT-unset} down=${RC_GOINGDOWN-unset} rc=$(cd /run/openrc && ls -d rc.starting rc.stopping 2>/dev/null | tr '\n' ,) failed=$(ls /run/openrc/failed 2>/dev/null | tr '\n' ,)"
case "$2" in
start) fail=$START_FAIL mark=$START_MARK run=$START_RUN sig=$START_SIG ;;
stop) fail=$STOP_FAIL mark=$STOP_MARK run=$STOP_RUN sig=$STOP_SIG ;;
*) exit 0 ;;
esac
for s in $sig; do
	case "$s" in "$RC_SVCNAME":*) kill -"${s#*:}" "$RC_PID"; sleep 0.2 ;; esac
done
for s in $fail; do
	[ "$s" = "$RC_SVCNAME" ] && { echo "$RC_SVCNAME: $2 failed" >&2; exit 1; }
done
for s in $mark; do
	case "$s" in "$RC_SVCNAME":*) /usr/libexec/rc/sbin/mark_service_"${s#*:}" ;; esac
done
for s in $run; do
	case "$s" in "$RC_SVCNAME":*) s=${s#*:}; START_RUN= STOP_RUN= /etc/init.d/${s%%,*} $(echo "${s#*,}" | tr , ' '); echo "nested: $?" ;; esac
done
exit 0
"#;

pub const PLUGIN: &str = r#"#include <stdio.h>
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

/// Per test binary, so they don't race building the same files.
pub fn build_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(env!("CARGO_CRATE_NAME"));
    fs::create_dir_all(&d).unwrap();
    d
}

/// Copy `from` to `to` with the plugin directory patched.
pub fn patch(from: &Path, to: &Path) {
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
pub fn programs() -> &'static (PathBuf, PathBuf) {
    static P: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    P.get_or_init(|| {
        let refdir = super::reference_dir().unwrap();
        // Next to the original, so its $ORIGIN RUNPATH still finds librc.
        let c = refdir.join(concat!("openrc-run/openrc-run.plugtest-", env!("CARGO_CRATE_NAME")));
        patch(&refdir.join("openrc-run/openrc-run"), &c);
        let rust = build_dir().join("openrc-rs");
        patch(Path::new(super::OURS), &rust);
        (c, rust)
    })
}

/// The patched C openrc (the Rust one is the multi-call binary of
/// [`programs`]).
pub fn c_openrc() -> &'static Path {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| {
        let refdir = super::reference_dir().unwrap();
        let c = refdir.join(concat!("openrc/openrc.plugtest-", env!("CARGO_CRATE_NAME")));
        patch(&refdir.join("openrc/openrc"), &c);
        c
    })
}

/// The test plugin, linked against the reference librc (as plugins link
/// librc.so.1, for `rc_environ_fd`).
pub fn plugin_so() -> &'static Path {
    static SO: OnceLock<PathBuf> = OnceLock::new();
    SO.get_or_init(|| {
        let dir = build_dir();
        let src = dir.join("plugin.c");
        fs::write(&src, PLUGIN).unwrap();
        let so = dir.join("plugin.so");
        let librc = super::reference_dir().unwrap().join("librc");
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

/// `alarm(n)` as `ualarm(n * 10ms)`, so svc_wait's timeouts take 600ms.
/// (Much shorter ticks can fire before the wait has begun on a loaded
/// machine, and then the wait never times out.)
pub fn alarm_shim() -> &'static Path {
    static SO: OnceLock<PathBuf> = OnceLock::new();
    SO.get_or_init(|| {
        let dir = build_dir();
        let src = dir.join("fastalarm.c");
        fs::write(
            &src,
            "#define _DEFAULT_SOURCE\n#include <unistd.h>\n\
             unsigned int alarm(unsigned int s) { return ualarm(s * 10000, 0) / 10000; }\n",
        )
        .unwrap();
        let so = dir.join("fastalarm.so");
        let ok = Command::new("cc").args(["-shared", "-fPIC", "-O2", "-o"]).arg(&so).arg(&src).status().unwrap();
        assert!(ok.success(), "building the alarm() shim failed");
        so
    })
}

pub fn start_with(r: &Path, rc_conf: &str) {
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
    // localmount (started) stops with LC_ALL=C.
    script(&initd.join("localmount"), "");
    symlink("/etc/init.d/localmount", r.join("run/started/localmount")).unwrap();
    symlink("/etc/init.d/kappa", r.join("run/inactive/kappa")).unwrap();
    fs::write(r.join("run/exclusive/beta"), "").unwrap();
}
pub fn st_plain(r: &Path) {
    start_with(r, "");
}
pub fn st_parallel(r: &Path) {
    start_with(r, "rc_parallel=\"YES\"\n");
}
pub fn st_parallel_slow(r: &Path) {
    st_parallel(r);
    // mu outlasts the (shimmed, 600ms) wait.
    fs::write(r.join("run/slow.mu"), "2").unwrap();
    fs::write(r.join("run/slow.gamma"), "").unwrap();
}
/// net.eth0 is stopping (with nobody holding its lock).
pub fn st_stopping(r: &Path) {
    st_plain(r);
    fs::create_dir_all(r.join("run/stopping")).unwrap();
    symlink("/etc/init.d/net.eth0", r.join("run/stopping/net.eth0")).unwrap();
}
pub fn st_hotplug(r: &Path) {
    start_with(r, "rc_hotplug=\"*\"\n");
}
pub fn st_strict_no(r: &Path) {
    start_with(r, "rc_depend_strict=\"NO\"\n");
}
/// Runlevel starting: deps only with RC_PID; use deps are not started.
pub fn st_starting(r: &Path) {
    st_plain(r);
    fs::write(r.join("run/rc.starting"), "").unwrap();
}
/// Services scheduled to start after net.eth0 and after "net" (which it
/// provides); the scheduled ones are slow, so their output comes last.
pub fn st_scheduled(r: &Path) {
    st_plain(r);
    for (after, svc) in [("net.eth0", "epsilon"), ("net.eth0", "beta"), ("net", "zeta")] {
        fs::create_dir_all(r.join("run/scheduled").join(after)).unwrap();
        symlink(format!("/etc/init.d/{svc}"), r.join("run/scheduled").join(after).join(svc)).unwrap();
        fs::write(r.join(format!("run/slow.{svc}")), "").unwrap();
    }
}
/// The lock files of net.eth0 and gamma are directories, kappa's a
/// symlink loop: the lock can't be taken (EISDIR, ELOOP).
pub fn st_no_exclusive(r: &Path) {
    st_plain(r);
    for svc in ["net.eth0", "gamma"] {
        fs::create_dir(r.join("run/exclusive").join(svc)).unwrap();
    }
    symlink("kappa", r.join("run/exclusive/kappa")).unwrap();
}
/// The deptree can't be loaded or written: it's a symlink loop (a
/// directory would just read as empty).
pub fn st_no_deptree(r: &Path) {
    st_plain(r);
    symlink("deptree", r.join("run/deptree")).unwrap();
}
pub fn st_plugins(r: &Path) {
    st_plain(r);
    let d = r.join("run/plugins.d");
    fs::create_dir_all(&d).unwrap();
    fs::copy(plugin_so(), d.join("test.so")).unwrap();
    // Skipped: a dot file.
    fs::write(d.join(".hidden.so"), "junk").unwrap();
}
pub fn st_bad_plugins(r: &Path) {
    st_plugins(r);
    let d = r.join("run/plugins.d");
    fs::write(d.join("junk.so"), "not an ELF file").unwrap();
    // A shared object without the hook.
    fs::copy("/usr/lib64/libeinfo.so.1", d.join("nohook.so"))
        .or_else(|_| fs::copy("/lib64/libeinfo.so.1", d.join("nohook.so")))
        .unwrap();
}

pub fn ns() -> Ns {
    let mut ns = system_ns();
    ns.binds.push(("$ROOT/rc.conf".into(), "/etc/rc.conf".into()));
    ns.binds.push(("$BIN".into(), "/sbin/openrc-run".into()));
    ns
}

/// `openrc-run /etc/init.d/<svc> args...` with RC_RUNLEVEL=default.
pub fn st(setup: fn(&Path), svc: &str, args: &[&str]) -> Case {
    let (c, rust) = programs().clone();
    let mut a = vec![format!("/etc/init.d/{svc}")];
    a.extend(args.iter().map(|s| s.to_string()));
    let a: Vec<&str> = a.iter().map(String::as_str).collect();
    Case::programs("openrc-run", c, rust, &a).sandbox(setup).ns(ns()).env("TERM", "dumb").env("RC_RUNLEVEL", "default")
}

/// `openrc args...`.  The services it starts and stops are run by the
/// (patched) C openrc-run in both runs, so only openrc differs.
pub fn rc(setup: fn(&Path), args: &[&str]) -> Case {
    let (c_run, rust) = programs().clone();
    let mut ns = system_ns();
    ns.binds.push(("$ROOT/rc.conf".into(), "/etc/rc.conf".into()));
    ns.binds.push((c_run.display().to_string(), "/sbin/openrc-run".into()));
    Case::programs("openrc", c_openrc().to_path_buf(), rust, args).sandbox(setup).ns(ns).env("TERM", "dumb")
}

//! PAM (Phase 8 step 2): the `pam` build (target/pam, `make pam`) vs the
//! reference's `-Dpam=true` build (build-pam/), and openrc-user without
//! PAM vs the plain reference.
//!
//! PAM only comes in when a program changes user, which a user namespace
//! that maps only root can't do, so every case here runs as the real root
//! (on the test VM), in a private PID and mount namespace.  The prelude
//! gives each case a private /run, the sandbox's /etc/pam.d, and copies
//! of /etc/passwd, /etc/group and /etc/shells with a user `rcuser` added.
//! The PAM services log each call (pam_exec) and set `PAMVAR` (pam_env).

mod common;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use common::Case;

/// A PAM application: `pamclient SERVICE USER OP...`, with OP `open`,
/// `close` (flags 0) or `opens`/`closes` (PAM_SILENT); prints each result
/// and then the PAM environment.
const PAMCLIENT: &str = r#"
#include <security/pam_appl.h>
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv)
{
	struct pam_conv conv = { NULL, NULL };
	pam_handle_t *h;
	int r = pam_start(argv[1], argv[2], &conv, &h);
	printf("pam_start %d\n", r);
	for (int i = 3; i < argc; i++) {
		int silent = argv[i][strlen(argv[i]) - 1] == 's';
		if (!strncmp(argv[i], "open", 4))
			r = pam_open_session(h, silent ? PAM_SILENT : 0);
		else
			r = pam_close_session(h, silent ? PAM_SILENT : 0);
		printf("%s %d\n", argv[i], r);
		fflush(stdout);
	}
	for (char **e = pam_getenvlist(h); e && *e; e++)
		printf("env %s\n", *e);
	pam_end(h, r);
	return 0;
}
"#;

fn pamclient() -> &'static str {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(env!("CARGO_CRATE_NAME"));
        fs::create_dir_all(&dir).unwrap();
        let c = dir.join("pamclient.c");
        fs::write(&c, PAMCLIENT).unwrap();
        let out = dir.join("pamclient");
        let ok = Command::new("cc").args(["-O2", "-o"]).arg(&out).arg(&c).arg("-lpam").status().unwrap();
        assert!(ok.success(), "building pamclient failed");
        out
    })
    .to_str()
    .unwrap()
}

/// The reference's PAM build, and ours (`make pam`).
fn pam_builds() -> Option<(PathBuf, PathBuf)> {
    let refdir = common::reference_dir()?;
    let cref = refdir.parent().unwrap().parent().unwrap().join("build-pam/src");
    let ours = common::repo_root().join("target/pam/debug");
    if !cref.join("pam_openrc/pam_openrc.so").exists() || !ours.join("libpam_openrc.so").exists() {
        if std::env::var_os("OPENRC_REF_REQUIRED").is_some() {
            panic!("PAM builds missing: run `make reference pam`");
        }
        eprintln!("SKIPPED: PAM builds missing; run `make reference pam`");
        return None;
    }
    Some((cref, ours))
}

/// One test at a time: they log to the one /dev/log, where a capture
/// would see the others' messages.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Root only: the cases change user.
fn runnable() -> bool {
    if !common::is_root() {
        eprintln!("SKIPPED: PAM cases need root (they change user)");
        return false;
    }
    true
}

fn script(p: &Path, body: &str) {
    fs::write(p, body).unwrap();
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

/// PAM services that log (pam_exec) and set PAMVAR, a world-writable
/// tmp/ for daemons run as other users, a fake `user` service and
/// openrc-user.sh, and a state directory.
fn base(r: &Path) {
    let root = r.display();
    let pamd = r.join("pam.d");
    fs::create_dir_all(&pamd).unwrap();
    script(
        &r.join("pamlog.sh"),
        // (In /run/tmp, so it's written as other users too.)
        "#!/bin/sh\necho \"pam: $PAM_SERVICE $PAM_TYPE user=$PAM_USER uid=$(id -u)\" >> /run/tmp/pam.log\n",
    );
    fs::write(r.join("pam_env.conf"), "PAMVAR DEFAULT=from-pam\nRC_FROMPAM DEFAULT=kept\n").unwrap();
    let logged = format!(
        "account required pam_exec.so quiet {root}/pamlog.sh\n\
         session required pam_exec.so quiet {root}/pamlog.sh\n\
         session required pam_env.so readenv=0 user_readenv=0 conffile={root}/pam_env.conf\n"
    );
    for s in ["start-stop-daemon", "supervise-daemon", "openrc-user"] {
        fs::write(pamd.join(s), &logged).unwrap();
    }
    fs::write(pamd.join("other"), "account required pam_deny.so\nsession required pam_deny.so\n").unwrap();

    // openrc-user's script: what it's run as and with, and a line on its
    // stdout (fd 3 of openrc-user).
    script(
        &r.join("openrc-user.sh"),
        "#!/bin/sh\n{ echo \"user.sh $*\"; id; env | grep -E '^(USER|LOGNAME|HOME|SHELL|PAMVAR|RC_FROMPAM|EINFO_LOG)=' | sort; } >> /run/tmp/user.log\n\
         echo \"stdout $1\"\nexit ${USER_SH_EXIT:-0}\n",
    );

    // The `user` service pam_openrc starts as user.<name>.
    let initd = r.join("init.d");
    fs::create_dir_all(&initd).unwrap();
    script(
        &initd.join("user"),
        &format!(
            "#!/bin/sh\necho \"service $0 $3 [lockfd: ${{2:+yes}}, EINFO_QUIET=$EINFO_QUIET]\" >> {root}/svc.log\nexit ${{USER_EXIT:-0}}\n"
        ),
    );
    let run = r.join("run");
    for d in ["started", "starting", "stopping", "inactive", "failed", "hotplugged", "options", "exclusive", "daemons"]
    {
        fs::create_dir_all(run.join(d)).unwrap();
    }
    fs::create_dir_all(run.join("user/rcuser")).unwrap();
    fs::write(
        run.join("user/rcuser/environ"),
        b"DBUS_SESSION_BUS_ADDRESS=unix:path=/x\0WAYLAND_DISPLAY=w-1\0LAST=no-nul",
    )
    .unwrap();
    fs::write(r.join("rc.conf"), "").unwrap();
}

macro_rules! prelude {
    ($($body:expr),* $(,)?) => {
        concat!(
            // (Safety: real root, so only ever as PID 1 of the namespace,
            // with private mounts.)
            "[ $$ = 1 ] || exit 99\n",
            "mount -t tmpfs tmpfs /run || exit 99; mkdir /run/openrc; mount --bind \"$ROOT/run\" /run/openrc || exit 99\n",
            // (The sandbox is under /root, which other users can't enter:
            // their home and scratch space are in the private /run.)
            "mkdir -m 1777 /run/tmp; mkdir /run/home; chown 4321:4321 /run/home\n",
            "mount --bind \"$ROOT/pam.d\" /etc/pam.d || exit 99\n",
            "for f in passwd group shells; do cp /etc/$f \"$ROOT/$f\"; done\n",
            "echo \"rcuser:x:4321:4321:RC user:/run/home:/bin/sh\" >> \"$ROOT/passwd\"\n",
            "echo \"rcuser:x:4321:\" >> \"$ROOT/group\"; echo \"rcextra:x:4322:rcuser\" >> \"$ROOT/group\"\n",
            "for f in passwd group shells rc.conf; do mount --bind \"$ROOT/$f\" /etc/$f || exit 99; done\n",
            "mount --bind \"$ROOT/init.d\" /etc/init.d || exit 99\n",
            "mount --bind \"$ROOT/openrc-user.sh\" /usr/libexec/rc/sh/openrc-user.sh || exit 99\n",
            "BIN=\"$0\"\n",
            "show() { for f in /run/tmp/pam.log /run/tmp/env.out /run/tmp/user.log $ROOT/fd3.out $ROOT/svc.log; do [ -e \"$f\" ] && ",
            "{ echo \"== ${f##*/}\"; cat \"$f\"; }; done; true; }\n",
            $($body, "\n",)*
            "show; exit 0",
        )
    };
}

/// What a daemon run as another user reports.
const REPORT: &str = "id; env | grep -E '^(PAMVAR|RC_|SSD_|EINFO_|SVCNAME|HOME|USER|LOGNAME)' | sort";

fn ssd(cref: &Path, ours: &Path, prelude: &'static str) -> Case {
    Case::programs("start-stop-daemon", cref.join("start-stop-daemon/start-stop-daemon"), ours.join("openrc-rs"), &[])
        .sandbox(base)
        .pidns(prelude)
        .real_root()
        .env("REPORT", REPORT)
        .env("RC_SVCNAME", "svc")
        .env("SSD_X", "1")
        .env("EINFO_Y", "1")
}

macro_rules! ssd_prelude {
    ($($body:expr),* $(,)?) => {
        prelude!(
            "ssd() { (exec -a start-stop-daemon \"$BIN\" \"$@\"); echo \"[ssd $*: $?]\"; }",
            $($body,)*
        )
    };
}

#[test]
fn pam_start_stop_daemon() {
    let _serial = serial();
    if !runnable() {
        return;
    }
    let Some((cref, ours)) = pam_builds() else { return };
    let s = |p| ssd(&cref, &ours, p);
    let cases = vec![
        // A session as the user, and its environment (PAM's RC_ variable
        // survives the cleaning: it's added after).
        s(ssd_prelude!("ssd -S -u nobody -x /bin/sh -- -c \"$REPORT\"")),
        s(ssd_prelude!("ssd -S -u rcuser:rcextra -x /bin/sh -- -c \"$REPORT\"")),
        s(ssd_prelude!("ssd -S -c root -x /bin/sh -- -c \"$REPORT\"")),
        // No user: no PAM.
        s(ssd_prelude!("ssd -S -x /bin/sh -- -c 'echo no user'")),
        // PAM says no.
        s(ssd_prelude!(
            "echo 'account required pam_deny.so' > /etc/pam.d/start-stop-daemon",
            "ssd -S -u nobody -x /bin/true"
        )),
        s(ssd_prelude!(
            "printf 'account required pam_permit.so\\nsession required pam_deny.so\\n' > /etc/pam.d/start-stop-daemon",
            "ssd -S -u nobody -x /bin/true"
        )),
        s(ssd_prelude!("rm /etc/pam.d/start-stop-daemon", "ssd -S -u nobody -x /bin/true")),
        // The exec fails: the session is closed first.
        s(ssd_prelude!("touch /run/tmp/noexec; chmod 644 /run/tmp/noexec", "ssd -S -u nobody -x /run/tmp/noexec")),
    ];
    common::check_all("pam-ssd", &cases);
}

#[test]
fn pam_supervise_daemon() {
    let _serial = serial();
    if !runnable() {
        return;
    }
    let Some((cref, ours)) = pam_builds() else { return };
    let sd = |p| {
        Case::programs(
            "supervise-daemon",
            cref.join("supervise-daemon/supervise-daemon"),
            ours.join("openrc-rs"),
            &["svc"],
        )
        .sandbox(base)
        .pidns(p)
        .real_root()
        .env("REPORT", REPORT)
        .env("RC_SVCNAME", "svc")
        .env("SSD_X", "1")
    };
    macro_rules! sd_prelude {
        ($($body:expr),* $(,)?) => {
            prelude!(
                "sd() { (exec -a supervise-daemon \"$BIN\" \"$@\"); echo \"[sd $*: $?]\"; }",
                "P=\"$ROOT/run/sv.pid\"",
                $($body,)*
            )
        };
    }
    let cases = vec![
        // The daemon reports, then waits to be stopped.
        sd(sd_prelude!(
            "sd svc -p \"$P\" -u rcuser -S /bin/sh -- -c \"($REPORT) > /run/tmp/env.out; exec sleep 30\"",
            "i=0; while ! [ -s /run/tmp/env.out ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done",
            "sd svc -p \"$P\" -K"
        )),
        // PAM says no: the daemon never runs.
        sd(sd_prelude!(
            "echo 'account required pam_deny.so' > /etc/pam.d/supervise-daemon",
            "sd svc -p \"$P\" -u rcuser -S /bin/sh -- -c \"echo ran > /run/tmp/env.out; exec sleep 30\"",
            "sleep 1; sd svc -p \"$P\" -K"
        )),
    ];
    common::check_all("pam-supervise", &cases);
}

macro_rules! user_prelude {
    ($($body:expr),* $(,)?) => {
        prelude!(
            // Run openrc-user in the background (fd 3 to a file); once the
            // start script has run, TERM it.
            // (NOFD3: run it with fd 3 closed.)
            "ou() { if [ -n \"$NOFD3\" ]; then (exec -a openrc-user \"$BIN\" \"$@\" 3>&-) & else ",
            "(exec -a openrc-user \"$BIN\" \"$@\" 3> \"$ROOT/fd3.out\") & fi; U=$!; ",
            "i=0; while ! grep -qs 'user.sh' /run/tmp/user.log && kill -0 $U 2>/dev/null && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done; ",
            "sleep 0.3; kill -TERM $U 2>/dev/null; wait $U; echo \"[openrc-user $*: $?]\"; }",
            $($body,)*
        )
    };
}

fn user_cases(mk: &dyn Fn(&'static str) -> Case) -> Vec<Case> {
    vec![
        mk(user_prelude!("ou rcuser")),
        // The start script fails: no waiting, no stop.
        mk(user_prelude!("USER_SH_EXIT=1 ou rcuser")),
        mk(user_prelude!("ou nosuchuser")),
        mk(user_prelude!("ou")),
        mk(user_prelude!("ou rcuser extra")),
        // No fd 3 for the script's stdout.  QUIRK: the leaked /etc/shells
        // takes its place.
        mk(user_prelude!("NOFD3=1 ou rcuser")),
        mk(user_prelude!("mount --bind /dev/null /etc/shells", "NOFD3=1 ou rcuser")),
    ]
}

#[test]
fn openrc_user() {
    let _serial = serial();
    if !runnable() || common::reference_dir().is_none() {
        return;
    }
    let mk = |p: &'static str| Case::new("openrc-user", &[]).sandbox(base).pidns(p).real_root();
    let mut cases = user_cases(&mk);
    if common::syslog_capturable() {
        cases = cases.into_iter().map(Case::syslog).collect();
    }
    common::check_all("openrc-user", &cases);
}

#[test]
fn pam_openrc_user() {
    let _serial = serial();
    if !runnable() {
        return;
    }
    let Some((cref, ours)) = pam_builds() else { return };
    let mk = |p: &'static str| {
        Case::programs("openrc-user", cref.join("openrc-user/openrc-user"), ours.join("openrc-rs"), &[])
            .sandbox(base)
            .pidns(p)
            .real_root()
    };
    let mut cases = user_cases(&mk);
    // No PAM service of its own: "other" denies the session (logged, and
    // the user's services still run).
    cases.push(mk(user_prelude!("rm /etc/pam.d/openrc-user", "ou rcuser")));
    if common::syslog_capturable() {
        cases = cases.into_iter().map(Case::syslog).collect();
    }
    common::check_all("pam-openrc-user", &cases);
}

macro_rules! module_prelude {
    ($($body:expr),* $(,)?) => {
        prelude!(
            // A login service with the module under test ("$0", by a
            // fixed name so the file is the same for both).
            "ln -s \"$0\" /run/pam_openrc.so; echo 'session required /run/pam_openrc.so' > /etc/pam.d/test-login",
            "login() { \"$PAMCLIENT\" test-login \"$@\"; }",
            "state() { ls \"$ROOT/run/hotplugged\" \"$ROOT/run/options\" 2>&1; cat \"$ROOT\"/run/options/*/logins 2>/dev/null; echo; ls -l /etc/init.d/user.rcuser 2>&1 | sed 's/.* \\/etc/\\/etc/'; }",
            $($body,)*
            "state",
        )
    };
}

#[test]
fn pam_openrc_module() {
    let _serial = serial();
    if !runnable() {
        return;
    }
    let Some((cref, ours)) = pam_builds() else { return };
    let m = |p: &'static str| {
        Case::programs("pam_openrc", cref.join("pam_openrc/pam_openrc.so"), ours.join("libpam_openrc.so"), &[])
            .sandbox(base)
            .pidns(p)
            .real_root()
            .env("PAMCLIENT", pamclient())
    };
    let mut cases = vec![
        // First session starts user.rcuser (made from `user`), the last
        // one to close stops it; the user's exported environment comes in.
        m(module_prelude!("login rcuser open")),
        m(module_prelude!("login rcuser open opens close closes")),
        m(module_prelude!("login rcuser opens", "login rcuser closes")),
        // Closing with no sessions counted.
        m(module_prelude!("login rcuser close")),
        // rc_autostart_user: only "no" (or another false) turns it off.
        m(module_prelude!("echo rc_autostart_user=no > /etc/rc.conf", "login rcuser open close")),
        m(module_prelude!("echo rc_autostart_user=NO > /etc/rc.conf", "login rcuser open")),
        m(module_prelude!("echo rc_autostart_user=maybe > /etc/rc.conf", "login rcuser open")),
        m(module_prelude!("echo rc_autostart_user=yes > /etc/rc.conf", "login rcuser open")),
        // root, unknown users, openrc-user's own sessions: nothing.
        m(module_prelude!("login root open close")),
        m(module_prelude!("login nosuchuser open")),
        m(module_prelude!(
            "echo 'session required /run/pam_openrc.so' > /etc/pam.d/openrc-user",
            "\"$PAMCLIENT\" openrc-user rcuser open close"
        )),
        // user.rcuser already started (not hotplugged): skipped.
        m(module_prelude!(
            "ln -s /etc/init.d/user /etc/init.d/user.rcuser; ln -s /etc/init.d/user.rcuser \"$ROOT/run/started/user.rcuser\"",
            "login rcuser open close"
        )),
        // No `user` service, or it fails.
        m(module_prelude!("rm /etc/init.d/user", "login rcuser open")),
        m(module_prelude!("USER_EXIT=3 login rcuser open close")),
        // An existing user.rcuser, and a logins count.
        m(module_prelude!(
            "ln -s /etc/init.d/user /etc/init.d/user.rcuser; mkdir \"$ROOT/run/options/user.rcuser\"; echo 2 > \"$ROOT/run/options/user.rcuser/logins\"",
            "login rcuser close close"
        )),
        // No exported environment.
        m(module_prelude!("rm \"$ROOT/run/user/rcuser/environ\"", "login rcuser open")),
    ];
    if common::syslog_capturable() {
        cases = cases.into_iter().map(Case::syslog).collect();
    }
    common::check_all("pam-openrc", &cases);
}

//! openrc-init (Phase 7 step 2): openrc-init vs the C program.
//!
//! Each case runs openrc-init as PID 1 of a private PID and mount
//! namespace, where `reboot()` ends the namespace (with SIGHUP for a
//! restart, SIGINT for a halt) instead of the machine.  The prelude sets
//! up: /run a tmpfs (with the sandbox's `run/` as /run/openrc, where the
//! FIFO lives), /etc the sandbox's copy (rc.conf, runlevels, passwd),
//! /var/log a tmpfs (for wtmp), and fake `openrc`, `halt` and
//! `openrc-init` (the program under test, for reexec) bound over the real
//! ones.  The fake openrc prints how it was run, runs a case's hook for
//! its runlevel, and at shutdown prints wtmp (without timestamps).
//! Background helpers then send the case's commands, to the FIFO once it
//! exists or as signals.  On a pty, so output is line-buffered and
//! survives the end.

mod common;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::Case;

const FAKE_OPENRC: &str = r#"#!/bin/bash
read -r -a s < /proc/$$/stat
[ "${s[5]}" = $$ ] && sid=own || sid=inherited
echo "openrc($0) $* [session $sid, ppid ${s[3]}, $(env | grep -v '^_=' | sort | tr '\n' ' ')]"
echo "$1" >> /run/rl.log
[ -e "/run/hook.$1" ] && . "/run/hook.$1"
case $1 in shutdown|reboot)
	# wtmp: everything but ut_tv (bytes 340-347 of each record).
	echo wtmp:; n=$(($(stat -c %s /var/log/wtmp) / 384)); i=0
	while [ $i -lt $n ]; do
		dd if=/var/log/wtmp bs=1 skip=$((i*384)) count=340 2>/dev/null | od -c | sed 's/  */ /g'
		dd if=/var/log/wtmp bs=1 skip=$((i*384+348)) count=36 2>/dev/null | od -c | sed 's/  */ /g'
		i=$((i+1))
	done;;
esac
exit 0
"#;

const FAKE_SHELL: &str = "#!/bin/bash\necho \"shell($0) $* [PATH=$PATH]\"\nexit 3\n";
const FAKE_HALT: &str = "#!/bin/bash\necho \"halt($0) $* [pid $$]\"\nexit 7\n";

macro_rules! prelude {
    ($($body:expr),* $(,)?) => {
        concat!(
            // (Safety: only ever as PID 1 of the namespace, and never on
            // the real /run, /etc or wtmp.)
            "[ $$ = 1 ] || exit 99\n",
            "mount -t tmpfs tmpfs /run || exit 99; mkdir /run/openrc; mount --bind \"$ROOT/run\" /run/openrc || exit 99\n",
            "for f in passwd group nsswitch.conf localtime ld.so.cache; do ",
            "[ -e /etc/$f ] && cp -L /etc/$f \"$ROOT/etc/\"; done; mount --bind \"$ROOT/etc\" /etc || exit 99\n",
            "mount -t tmpfs tmpfs /var/log || exit 99; touch /var/log/wtmp\n",
            // (Where these are links to openrc-rs, a stand-in /usr/bin:
            // see common::symlink_farm.)
            "if [ -d \"$ROOT.farm\" ]; then mount --bind /usr/bin \"$ROOT.farm/orig\" && ",
            "mount --bind \"$ROOT.farm/bin\" /usr/bin || exit 99; fi\n",
            "for p in openrc halt; do mount --bind \"$ROOT/fake/$p\" /usr/bin/$p || exit 99; done\n",
            "mount --bind \"$0\" /usr/bin/openrc-init || exit 99\n",
            // Helpers, in the background: wait for init's FIFO (it's made
            // after the runlevels), then send a command or a signal.
            "waitfifo() { while [ ! -p /run/openrc/init.ctl ]; do sleep 0.05; done; sleep 0.1; }\n",
            "tofifo() { printf \"$1\" > /run/openrc/init.ctl; }\n",
            "send() { (waitfifo; for c; do tofifo \"$c\"; sleep 0.3; done) & }\n",
            "sig() { (waitfifo; kill -$1 1) & }\n",
            // Hooks for the fake openrc, by runlevel.
            "hook() { echo \"$2\" > /run/hook.$1; }\n",
            $($body, "\n",)*
        )
    };
}

fn script(p: &Path, body: &str) {
    fs::write(p, body).unwrap();
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn base(r: &Path) {
    fs::create_dir_all(r.join("run")).unwrap();
    for rl in ["sysinit", "boot", "default", "shutdown", "reboot", "single", "custom"] {
        fs::create_dir_all(r.join("etc/runlevels").join(rl)).unwrap();
    }
    fs::write(r.join("etc/rc.conf"), "").unwrap();
    fs::create_dir_all(r.join("fake")).unwrap();
    script(&r.join("fake/openrc"), FAKE_OPENRC);
    script(&r.join("fake/shell"), FAKE_SHELL);
    script(&r.join("fake/halt"), FAKE_HALT);
    let names = ["openrc", "halt", "openrc-init"].map(std::ffi::OsString::from);
    common::symlink_farm(&r.with_extension("farm"), Path::new("/usr/bin"), &names);
}

/// `openrc-init args` as PID 1, after the prelude's steps, on a pty.
fn init(prelude: &'static str, args: &[&str]) -> Case {
    // (After a reexec, the C program is /usr/bin/openrc-init, where its
    // $ORIGIN RUNPATH no longer finds the reference librc.)
    let refdir = common::reference_dir().unwrap();
    let libs = format!("{}/librc:{}/libeinfo", refdir.display(), refdir.display());
    Case::new("openrc-init", args).sandbox(base).pidns(prelude).env("LD_LIBRARY_PATH", &libs).pty()
}

fn piped(mut c: Case) -> Case {
    c.pty = false;
    c
}

#[test]
fn init_fifo() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        init(prelude!("send reboot"), &[]),
        init(prelude!("send halt"), &[]),
        init(prelude!("send poweroff"), &[]),
        // reboot(RB_KEXEC) fails in a namespace: reboot normally.
        init(prelude!("send kexec"), &[]),
        // Unknown commands, then reboot.
        init(prelude!("send bogus 'reboot\\n' REBOOT reboot"), &[]),
        // A command is what's before a NUL.
        init(prelude!("send 'reboot\\0junk'"), &[]),
        // Read 2047 bytes at a time.
        init(prelude!("send \"$(printf 'x%.0s' {1..3000})\" halt"), &[]),
        // Re-exec, keeping the FIFO; then reboot.
        init(
            prelude!(
                "(waitfifo; tofifo reexec; until tr '\\0' ' ' < /proc/1/cmdline | grep -q reexec; do sleep 0.05; done; ",
                "sleep 0.2; tofifo reboot) &"
            ),
            &[],
        ),
    ];
    common::check_all("init-fifo", &cases);
}

#[test]
fn init_signals() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        init(prelude!("sig INT"), &[]),
        init(prelude!("sig TERM"), &[]),
        init(prelude!("sig PWR"), &[]),
        // Blocked signals do nothing; then reboot.
        init(prelude!("(waitfifo; kill -USR1 1; kill -HUP 1; sleep 0.2; tofifo reboot) &"), &[]),
        // What's left gets TERM, then (half a second later, or when all
        // are gone, at most three seconds later) KILL.
        init(
            prelude!(
                "hook default \"(trap 'echo child got TERM; exit 0' TERM; while :; do sleep 0.05; done) 2>/dev/null & sleep 0.1\"",
                "send reboot"
            ),
            &[],
        ),
        init(
            prelude!(
                "hook default \"(trap '' TERM; exec sleep 30) & (trap 'echo child got TERM; sleep 1; echo child done' TERM; while :; do sleep 0.05; done) 2>/dev/null & sleep 0.1\"",
                "send reboot"
            ),
            &[],
        ),
        init(prelude!("hook default \"(trap '' TERM; exec sleep 30) &\"", "send halt"), &[]),
    ];
    common::check_all("init-signals", &cases);
}

#[test]
fn init_runlevels() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        init(prelude!("send reboot"), &["custom"]),
        init(prelude!("send reboot"), &["bogus"]),
        init(prelude!("send reboot"), &[""]),
        init(prelude!("echo rc_default_runlevel=custom > /etc/rc.conf", "send reboot"), &[]),
        init(prelude!("echo rc_default_runlevel=bogus > /etc/rc.conf", "send reboot"), &[]),
        init(prelude!("echo rc_default_runlevel=bogus > /etc/rc.conf", "send reboot"), &["custom"]),
        init(prelude!("rmdir /etc/runlevels/default", "send reboot"), &["bogus"]),
        init(prelude!("send reboot"), &[]).env("EINFO_QUIET", "yes"),
        init(prelude!("send halt"), &[]).env("EINFO_QUIET", "YES"),
        init(prelude!("send halt"), &[]).env("EINFO_QUIET", "no"),
        // openrc can't be run.
        init(prelude!("chmod -x \"$ROOT/fake/openrc\"", "send reboot"), &[]),
        // Fully buffered (and lost) output to a pipe.
        piped(init(prelude!("send reboot"), &[])),
    ];
    let mut cases = cases;
    // Not PID 1: nothing at all.
    cases.push(Case::new("openrc-init", &[]));
    cases.push(Case::new("openrc-init", &["reboot"]));
    common::check_all("init-runlevels", &cases);
}

#[test]
fn init_single() {
    if common::reference_dir().is_none() {
        return;
    }
    // Single user: the single runlevel, a shell, then boot again; reboot
    // once the default runlevel has run twice.
    macro_rules! single {
        ($($body:expr),*) => {
            prelude!(
                $($body,)*
                "(waitfifo; tofifo single; until [ \"$(grep -c default /run/rl.log)\" = 2 ]; do sleep 0.05; done; ",
                "sleep 0.3; tofifo reboot) &"
            )
        };
    }
    let cases = vec![
        init(single!("echo rc_shell=\"$ROOT/fake/shell\" > /etc/rc.conf"), &[]),
        init(single!(), &[]).env("SHELL", "$ROOT/fake/shell"),
        init(single!("sed -i \"s|^\\(root:.*:\\)[^:]*$|\\1$ROOT/fake/shell|\" /etc/passwd"), &[]),
        init(single!("echo rc_shell=\"$ROOT/fake/shell\" > /etc/rc.conf"), &[]).env("SHELL", "/bin/false"),
        // A shell that can't be run.
        init(single!("echo rc_shell=/nonexistent > /etc/rc.conf"), &[]),
        // VSERVER: halt -f instead (which replaces init).
        init(prelude!("echo rc_sys=VSERVER > /etc/rc.conf", "send single"), &[]),
    ];
    common::check_all("init-single", &cases);
}

/// Seconds from the end of the reboot runlevel (just before the final
/// TERM) until the namespace ends, for `bin` as PID 1 with `child` (bash,
/// started by the default runlevel, if any) still running at shutdown.
fn grace(bin: &Path, name: &str, child: &str) -> f64 {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(env!("CARGO_CRATE_NAME")).join(name);
    let _ = fs::remove_dir_all(&root);
    base(&root);
    let p = prelude!(
        "[ -n \"$CHILD\" ] && hook default \"($CHILD) > /dev/null 2>&1 & sleep 0.1\"",
        "hook reboot 'date +%s.%N > $T0'",
        // (A sender that's gone before the TERM: its SIGCHLD would end
        // the grace.)
        "(waitfifo; tofifo reboot) &",
        "exec -a openrc-init \"$0\""
    );
    let refdir = common::reference_dir().unwrap();
    let st = std::process::Command::new("/usr/bin/unshare")
        .args(["--user", "--map-root-user", "--pid", "--fork", "--mount", "--mount-proc", "/bin/bash", "-c"])
        .arg(p.replace("$ROOT", root.to_str().unwrap()))
        .arg(bin)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LD_LIBRARY_PATH", format!("{0}/librc:{0}/libeinfo", refdir.display()))
        .env("CHILD", child)
        .env("T0", root.join("t0"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    let end = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    assert!(st.code() != Some(99), "{name}: prelude failed");
    let t0: f64 = fs::read_to_string(root.join("t0")).unwrap().trim().parse().unwrap();
    end - t0
}

#[test]
fn init_grace() {
    let Some(refdir) = common::reference_dir() else { return };
    let c = refdir.join("openrc-init/openrc-init");
    let ours = Path::new(common::OURS);
    // Half a second's grace, then KILL once every child is gone, or after
    // three seconds.  QUIRK: a child exiting cuts the half second short.
    let cases = [
        ("none", "", 0.45, 1.2),
        ("gone", "trap 'exit 0' TERM; while :; do sleep 0.05; done", 0.0, 0.4),
        ("after-1s", "trap 'sleep 1; exit 0' TERM; while :; do sleep 0.05; done", 0.95, 1.4),
        ("stays", "trap '' TERM; while :; do sleep 0.05; done", 2.95, 4.2),
    ];
    for (i, (name, child, lo, hi)) in cases.iter().enumerate() {
        for (who, bin) in [("C", c.as_path()), ("Rust", ours)] {
            let t = grace(bin, &format!("init-grace-{i}"), child);
            assert!(
                (*lo..*hi).contains(&t),
                "{name}: {who} took {t:.2}s from the reboot runlevel to the end, not {lo}-{hi}s"
            );
        }
    }
    eprintln!("init-grace: {} cases within the C reference's timing", cases.len());
}

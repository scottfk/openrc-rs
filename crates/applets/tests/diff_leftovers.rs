//! rc-abort, rc-environ and sysv-initctl (Phase 8 step 1): openrc-rs vs
//! the C programs.
//!
//! rc-abort and sysv-initctl run in a private PID and mount namespace (the
//! prelude runs them and shows the results): rc-abort signals a process
//! there; sysv-initctl gets a private /run (a reader on openrc-init's
//! FIFO prints what it forwards) and a sandbox /dev for its /dev/initctl
//! link.  rc-environ runs on the state fixture with services that
//! re-export variables.

mod common;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use common::Case;
use common::system::{script, system, system_ns};

// ---- rc-abort ----

macro_rules! abort_prelude {
    ($($body:expr),* $(,)?) => {
        concat!(
            "[ $$ = 1 ] || exit 99\n",
            "BIN=\"$0\"\n",
            // (The status is printed once the target is done, after what
            // it says.)
            "ra() { (exec -a rc-abort \"$BIN\"); R=$?; }\n",
            // A process to signal, which says when it gets SIGUSR1.
            "(trap 'echo got USR1; exit 0' USR1; while :; do sleep 0.05; done) & P=$!; sleep 0.1\n",
            $($body, "\n",)*
            "kill $P 2>/dev/null; wait; echo \"[rc-abort: $R]\"; exit 0",
        )
    };
}

#[test]
fn rc_abort() {
    if common::reference_dir().is_none() {
        return;
    }
    let ra = |p: &'static str| Case::new("rc-abort", &[]).pidns(p);
    let cases = vec![
        ra(abort_prelude!("ra")),
        ra(abort_prelude!("RC_PID=$P ra; wait $P")),
        // sscanf("%d"): leading blanks and a sign, trailing junk ignored.
        ra(abort_prelude!("RC_PID=\" +$P.x\" ra; wait $P")),
        ra(abort_prelude!("RC_PID= ra")),
        ra(abort_prelude!("RC_PID=x$P ra")),
        ra(abort_prelude!("RC_PID=99999 ra")),
        // (int)strtol: 2^32 + P is P.
        ra(abort_prelude!("RC_PID=$((4294967296 + P)) ra; wait $P")),
        ra(abort_prelude!("RC_PID=-$P ra")),
    ];
    common::check_all("rc-abort", &cases);
}

// ---- rc-environ ----

/// The state fixture plus services that re-export variables, some saved.
fn env_system(r: &Path) {
    system(r);
    let initd = r.join("init.d");
    script(&initd.join("exp"), "depend() {\n\treexport FOO SPECIAL TABS EMPTY NUL MISSING\n}");
    script(&initd.join("exp2"), "depend() {\n\treexport FOOBAR QUOTE\n}");
    script(&initd.join("starter"), "depend() {\n\treexport FOO FOOBAR XX\n}");
    script(&initd.join("plain"), "");
    let run = r.join("run");
    for s in ["exp", "exp2", "plain"] {
        symlink(format!("/etc/init.d/{s}"), run.join("started").join(s)).unwrap();
    }
    symlink("/etc/init.d/starter", run.join("starting/starter")).unwrap();
    let env = run.join("environ");
    fs::create_dir_all(env.join("exp")).unwrap();
    fs::create_dir_all(env.join("exp2")).unwrap();
    for (name, value) in [
        ("FOO", &b"plain"[..]),
        ("SPECIAL", b"a b'c\"d\\e$f"),
        ("TABS", b"\t\n\r\x07\x08\x0b\x0c\x1b|x=y"),
        ("EMPTY", b""),
        ("NUL", b"ab\0c d"),
    ] {
        fs::write(env.join("exp").join(name), value).unwrap();
    }
    fs::write(env.join("exp2/FOOBAR"), "1=2").unwrap();
    fs::write(env.join("exp2/QUOTE"), "it's").unwrap();
    fs::create_dir_all(r.join("runlevels/custom")).unwrap();
    for s in ["exp2", "plain", "exp"] {
        symlink(format!("/etc/init.d/{s}"), r.join("runlevels/custom").join(s)).unwrap();
    }
}

fn environ(args: &[&str]) -> Case {
    Case::new("rc-environ", args).sandbox(env_system).ns(system_ns())
}

#[test]
fn rc_environ_print() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        environ(&[]),
        environ(&["-e"]),
        environ(&["-n"]),
        environ(&["-0"]),
        environ(&["--export", "--no-escape", "--null"]),
        environ(&["-s", "exp2"]),
        environ(&["-s", "exp2", "-s", "exp", "-s", "exp2"]),
        environ(&["-s", "nosuch", "-s", "plain"]),
        environ(&["-r", "custom"]),
        environ(&["-r", "custom", "-s", "exp"]),
        environ(&["-r", "custom", "-r", "custom"]),
        environ(&["-r", "nosuch"]),
        environ(&["-h"]),
        environ(&["-V"]),
        environ(&["-V", "-e"]),
        environ(&["--bogus"]),
        environ(&["-s"]),
        environ(&["-q"]),
        environ(&["-U"]),
    ];
    common::check_all("rc-environ-print", &cases);
}

#[test]
fn rc_environ_set() {
    if common::reference_dir().is_none() {
        return;
    }
    let set = |args: &[&str]| environ(args).file("run/environ/starter/FOO").file("run/environ/starter/FOOBAR");
    let cases = vec![
        set(&["starter", "FOO=bar", "FOOBAR=x=y", "XX=", "NOPE=1"]),
        // From our environment.
        set(&["starter", "FOO", "XX"]).env("FOO", "from env").env("XX", "e"),
        // QUIRK: a name matches any re-export it's a prefix of.
        set(&["starter", "FO=short", "F"]).env("F", "f"),
        set(&["starter", "=all"]),
        set(&["starter"]),
        // Only while it's starting.
        set(&["exp", "FOO=x"]),
        set(&["nosuch", "FOO=x"]),
        set(&["-e", "starter", "FOO=opt"]),
    ];
    common::check_all("rc-environ-set", &cases);
}

// ---- sysv-initctl ----

macro_rules! initctl_prelude {
    ($($body:expr),* $(,)?) => {
        concat!(
            // (Safety: a private /run, and /dev only in the sandbox.)
            "[ $$ = 1 ] || exit 99\n",
            "mount -t tmpfs tmpfs /run || exit 99; mkdir /run/openrc\n",
            "mkdir \"$ROOT/dev\"; for f in null zero log; do [ -e /dev/$f ] && touch \"$ROOT/dev/$f\" && ",
            "mount --bind /dev/$f \"$ROOT/dev/$f\"; done; mount --rbind \"$ROOT/dev\" /dev || exit 99\n",
            "BIN=\"$0\"\n",
            // openrc-init's FIFO, with a reader that never sees EOF.
            "initfifo() { mkfifo /run/openrc/init.ctl; exec 3<>/run/openrc/init.ctl; cat <&3 > /run/init.got & }\n",
            "start() { (exec -a sysv-initctl \"$BIN\") & S=$!; while [ ! -p /run/initctl ] && kill -0 $S 2>/dev/null; do sleep 0.02; done; }\n",
            // req CMD RUNLEVEL [DATA [MAGIC]]: one init_request, in one write.
            // (Little-endian, as printf escapes.)
            "le() { printf %08x $(($1 & 0xffffffff)) | sed 's/\\(..\\)\\(..\\)\\(..\\)\\(..\\)/\\\\x\\4\\\\x\\3\\\\x\\2\\\\x\\1/'; }\n",
            "mk() { { printf \"$(le ${4:-50927977})$(le $1)$(le $2)$(le 0)\"; printf \"$3\"; head -c 400 /dev/zero; } | head -c 384 > /run/req; }\n",
            "req() { mk \"$@\"; cat /run/req > /run/initctl; sleep 0.1; }\n",
            "rl() { req 1 $(printf %d \"'$1\"); }\n",
            "show() { sleep 0.2; kill $S 2>/dev/null; wait $S 2>/dev/null; echo \"[sysv-initctl: $?]\"; ",
            "[ -e /run/init.got ] && od -A d -c /run/init.got | sed 's/  */ /g'; ls -l /dev/initctl | sed 's/.* \\/dev/\\/dev/'; ls -F /run; true; }\n",
            $($body, "\n",)*
            "show; exit 0",
        )
    };
}

fn initctl(prelude: &'static str) -> Case {
    Case::new("sysv-initctl", &[]).sandbox(|r| fs::create_dir_all(r).unwrap()).pidns(prelude)
}

#[test]
fn sysv_initctl() {
    if common::reference_dir().is_none() {
        return;
    }
    let mut cases = vec![
        initctl(initctl_prelude!(
            "initfifo", "start", "rl 0", "rl 1", "rl S", "rl s", "rl 2", "rl 5", "rl 6", "rl q", "rl Q", "rl U", "rl u"
        )),
        // INIT_HALT=HALT makes runlevel 0 a halt; unsetting it or any other
        // value undoes that.
        initctl(initctl_prelude!(
            "initfifo",
            "start",
            "req 6 0 'FOO=1\\0INIT_HALT=HALT\\0'",
            "rl 0",
            "req 7 0 'INIT_HALT\\0'",
            "rl 0",
            "req 6 0 'INIT_HALT=HALT\\0'",
            "req 6 0 'INIT_HALT=\\0'",
            "rl 0",
            "req 6 0 'INIT_HALT=HALT\\0'",
            "req 6 0 'INIT_HALTX=1\\0'",
            "rl 0",
            // The scan stops at an empty string.
            "req 7 0 'INIT_HALT\\0'",
            "req 6 0 '\\0INIT_HALT=HALT\\0'",
            "rl 0",
            // The data's last byte is always a NUL: a variable running into
            // it is cut short (INIT_HALT=HAL).
            "req 6 0 'INIT_HALT=HALT\\0'",
            "req 6 0 \"$(printf %0353d 0 | tr 0 A)\\0INIT_HALT=HALT\"",
            "rl 0"
        )),
        // Other commands, a bad magic number, a short request: ignored.
        initctl(initctl_prelude!(
            "initfifo",
            "start",
            "req 0 54",
            "req 2 54",
            "req 1 54 '' 1234",
            "printf abc > /run/initctl; sleep 0.1",
            "mk 1 54; head -c 16 /run/req > /run/initctl; sleep 0.1",
            "rl 6"
        )),
        // No openrc-init FIFO, or nobody reading it.
        initctl(initctl_prelude!("start", "rl 6")),
        initctl(initctl_prelude!("mkfifo /run/openrc/init.ctl", "start", "rl 6")),
        // /run/initctl already there.
        initctl(initctl_prelude!(
            "initfifo",
            "mkfifo -m 644 /run/initctl",
            "start",
            "rl 6",
            "ls -l /run/initctl | cut -c1-10"
        )),
        initctl(initctl_prelude!("initfifo", "mkdir /run/initctl", "start")),
        // /dev/initctl taken.
        initctl(initctl_prelude!("initfifo", "touch /dev/initctl", "start", "rl 6")),
    ];
    if common::syslog_capturable() {
        for c in &mut cases {
            *c = c.clone().syslog();
        }
    }
    common::check_all("sysv-initctl", &cases);
}

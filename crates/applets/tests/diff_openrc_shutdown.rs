//! openrc-shutdown (Phase 7 step 1): openrc-shutdown vs the C program.
//!
//! Each case runs as root of a private PID and mount namespace, set up by
//! the prelude: /run is a tmpfs (with the sandbox's `run/` as /run/openrc,
//! where openrc-init's FIFO lives), /etc the sandbox's copy of what's
//! needed (for /etc/nologin), /var/log a tmpfs (for wtmp).  What's sent to
//! openrc-init's FIFO or to sysvinit's /run/initctl is read by `cat` and
//! printed (initctl's binary requests with od); wtmp records are printed
//! without their timestamps.  The prelude runs the case's command itself
//! (`osd "$@"`), shows the results and exits, so the final exec never
//! happens.
//!
//! A shim (built with cc) fixes `time()` and makes the minute-long sleeps
//! 50ms, so a delayed shutdown runs through its warnings at once; on a pty,
//! `mkutmp` logs the pty's user in, so the broadcasts reach the output.
//! (Those run with stdin from /dev/null: the header would name our own
//! tty, whose number differs between the two runs.)

mod common;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use common::Case;

const SHIM: &str = r#"
#define _GNU_SOURCE
#include <stdlib.h>
#include <time.h>
#include <unistd.h>
time_t time(time_t *t) { if (t) *t = 1000000000; return 1000000000; }
/* A "minute" takes SHIM_MINUTE_MS (default 50) milliseconds. */
int nanosleep(const struct timespec *req, struct timespec *rem)
{
	(void)rem;
	if (req->tv_sec >= 1) {
		const char *m = getenv("SHIM_MINUTE_MS");
		usleep((m ? atoi(m) : 50) * 1000);
		return 0;
	}
	return usleep(req->tv_nsec / 1000);
}
"#;

const MKUTMP: &str = r#"
#include <string.h>
#include <utmpx.h>
#include <stdlib.h>
/* mkutmp user line [id [type]]: add a login record (type 7, USER_PROCESS). */
int main(int argc, char **argv)
{
	struct utmpx u = { .ut_type = USER_PROCESS, .ut_pid = 1 };
	if (argc < 3) return 2;
	strncpy(u.ut_user, argv[1], sizeof(u.ut_user));
	strncpy(u.ut_line, argv[2], sizeof(u.ut_line));
	strncpy(u.ut_id, argc > 3 ? argv[3] : argv[2] + strlen(argv[2]) - 1, sizeof(u.ut_id));
	if (argc > 4) u.ut_type = atoi(argv[4]);
	setutxent();
	return pututxline(&u) ? 0 : 1;
}
"#;

fn build(name: &str, src: &str, shared: bool) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(env!("CARGO_CRATE_NAME"));
    fs::create_dir_all(&dir).unwrap();
    let c = dir.join(format!("{name}.c"));
    fs::write(&c, src).unwrap();
    let out = dir.join(name);
    let mut cmd = Command::new("cc");
    if shared {
        cmd.args(["-shared", "-fPIC"]);
    }
    let ok = cmd.args(["-O2", "-o"]).arg(&out).arg(&c).status().unwrap();
    assert!(ok.success(), "building {name} failed");
    out
}
fn shim() -> &'static str {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| build("shutdownshim.so", SHIM, true)).to_str().unwrap()
}
fn mkutmp() -> &'static str {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| build("mkutmp", MKUTMP, false)).to_str().unwrap()
}

macro_rules! prelude {
    ($($body:expr),* $(,)?) => {
        concat!(
            // (Safety: never reach a real /run/initctl or wtmp.)
            "mount -t tmpfs tmpfs /run || exit 99; mkdir /run/openrc; mount --bind \"$ROOT/run\" /run/openrc || exit 99\n",
            "mkdir -p \"$ROOT/etc\"; for f in passwd group nsswitch.conf localtime ld.so.cache; do ",
            "[ -e /etc/$f ] && cp -L /etc/$f \"$ROOT/etc/\"; done; mount --bind \"$ROOT/etc\" /etc || exit 99\n",
            "mount -t tmpfs tmpfs /var/log || exit 99\n",
            // (glibc's updwtmp() and pututxline() don't create them.)
            "touch /var/log/wtmp /run/utmp\n",
            "BIN=\"$0\"\n",
            "osd() { (exec -a openrc-shutdown \"$BIN\" \"$@\" < \"${OSD_STDIN:-/dev/stdin}\"); echo \"[osd $*: $?]\"; }\n",
            // A reader on openrc-init's FIFO, and on sysvinit's.
            "initfifo() { mkfifo /run/openrc/init.ctl; cat /run/openrc/init.ctl > /run/init.got & }\n",
            "initctl() { mkfifo /run/initctl; cat /run/initctl > /run/initctl.got & }\n",
            // A private devpts, so `script` gives us /dev/pts/0; run the
            // command on it, logged in there, and show what reached it
            // (sorted and counted: broadcasts come from children).
            // Logins on pts/0: ours (by short and full name), and ones
            // broadcast() skips: an empty user, a dead process, a `/../`
            // path, and /dev/null (not a terminal).
            "utmps() { \"$MKUTMP\" root pts/0 a; \"$MKUTMP\" root /dev/pts/0 b; \"$MKUTMP\" '' pts/0 c; ",
            "\"$MKUTMP\" root pts/0 d 8; \"$MKUTMP\" root pts/../pts/0 e; \"$MKUTMP\" root null f; }\n",
            "ptys() { mount -t devpts -o newinstance,ptmxmode=0666 devpts /dev/pts && mount --bind /dev/pts/ptmx /dev/ptmx || exit 99; }\n",
            // (The pause lets the last broadcast child finish writing before
            // script closes the pty.)
            "ontty() { script -qec \"(exec -a openrc-shutdown $BIN $*); r=\\$?; sleep 0.5; exit \\$r\" /run/tty.out > /dev/null; echo \"[on tty $*: $?]\"; ",
            "tr -d '\\r\\a' < /run/tty.out | grep -a . | grep -av '^Script ' | sort | uniq -c; }\n",
            "show() { sleep 0.2; for f in /run/init.got /run/initctl.got; do [ -e $f ] && ",
            "{ echo \"$f:\"; od -A d -t x1 -v $f | sed 's/  */ /g'; }; done; ",
            // wtmp: everything but ut_tv (bytes 340-347 of each record).
            "[ -e /var/log/wtmp ] && { echo wtmp:; n=$(($(stat -c %s /var/log/wtmp) / 384)); i=0; ",
            "while [ $i -lt $n ]; do dd if=/var/log/wtmp bs=1 skip=$((i*384)) count=340 2>/dev/null | od -c | sed 's/  */ /g'; ",
            "dd if=/var/log/wtmp bs=1 skip=$((i*384+348)) count=36 2>/dev/null | od -c | sed 's/  */ /g'; i=$((i+1)); done; }; ",
            "ls /run /etc/nologin 2>&1; true; }\n",
            $($body, "\n",)*
            // The case's own command, then what it did.
            "osd \"$@\"\nshow\nexit 0",
        )
    };
}

fn base(r: &Path) {
    fs::create_dir_all(r.join("run")).unwrap();
}

/// `openrc-shutdown args` after the prelude's steps.
fn osd(prelude: &'static str, args: &[&str]) -> Case {
    Case::new("openrc-shutdown", args).sandbox(base).pidns(prelude).env("LD_PRELOAD", shim())
}

#[test]
fn shutdown_options() {
    if common::reference_dir().is_none() {
        return;
    }
    let p = prelude!();
    let mut cases = vec![
        osd(p, &["-h"]),
        osd(p, &["-V"]),
        osd(p, &[]),
        osd(p, &["-r", "-p", "now"]),
        osd(p, &["-x"]),
        osd(p, &["-r"]),
        osd(p, &["-r", "x"]),
        osd(p, &["-r", "1x"]),
        osd(p, &["-r", "25:00"]),
        osd(p, &["-r", "12:60"]),
        osd(p, &["-r", ":"]),
        osd(p, &["-r", "1:2:3"]),
        osd(p, &["-r", "123:4"]),
        osd(p, &["-c"]),
        osd(p, &["-d"]),
    ];
    if !common::is_root() {
        // Outside the namespace, as a user.
        cases.push(Case::new("openrc-shutdown", &["-D", "-r", "now"]));
        cases.push(Case::new("openrc-shutdown", &["-x"]));
    }
    common::check_all("shutdown-options", &cases);
}

#[test]
fn shutdown_now() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        // To openrc-init: through its FIFO, or a plain file without one.
        osd(prelude!("initfifo"), &["-r", "now"]),
        osd(prelude!("initfifo"), &["-p", "NOW"]),
        osd(prelude!("initfifo"), &["-H", "+0"]),
        osd(prelude!("initfifo"), &["-K", "0"]),
        osd(prelude!("initfifo"), &["-s", "now"]),
        osd(prelude!("initfifo"), &["-R"]),
        osd(prelude!("initfifo"), &["-d", "-r", "now"]),
        osd(prelude!(), &["--reboot", "now"]).file("run/init.ctl"),
        osd(prelude!("rmdir /run/openrc 2>/dev/null; umount /run/openrc; rmdir /run/openrc"), &["-r", "now"]),
        osd(prelude!("initfifo"), &["-D", "-r", "now"]),
        osd(prelude!("initfifo"), &["-D", "-R"]),
        // To sysvinit, when /run/initctl is there.
        osd(prelude!("initctl"), &["-r", "now"]),
        osd(prelude!("initctl"), &["-p", "now"]),
        osd(prelude!("initctl"), &["-H", "now"]),
        osd(prelude!("initctl"), &["-s", "now"]),
        osd(prelude!("initctl", "initfifo"), &["-K", "now"]),
        osd(prelude!("touch /run/initctl"), &["-r", "now"]),
        // QUIRK: --dry-run doesn't stop the request to sysvinit.
        osd(prelude!("initctl", "initfifo"), &["-D", "-r", "now"]),
        osd(prelude!("initctl"), &["-D", "-p", "now"]),
        // wtmp only.
        osd(prelude!(), &["-w"]),
        osd(prelude!(), &["--write-only", "-d"]),
    ];
    common::check_all("shutdown-now", &cases);
}

#[test]
fn shutdown_tty() {
    if common::reference_dir().is_none() {
        return;
    }
    let t = |c: Case| c.env("MKUTMP", mkutmp());
    let cases = vec![
        t(osd(prelude!("initfifo", "ptys", "utmps", "ontty -r now"), &["-c"])),
        // The warning schedule: every hour above 3 hours, every 30 minutes
        // above one, every 15 above 10, then every minute.
        t(osd(prelude!("initfifo", "ptys", "utmps", "ontty -p 200"), &["-c"])).env("SHIM_MINUTE_MS", "5"),
        // hh:mm: %2d takes two digits (03:470 is 03:47, a minute away);
        // a time just gone means tomorrow.
        t(osd(prelude!("initfifo", "ptys", "utmps", "ontty -r 03:470"), &["-c"])),
        t(osd(prelude!("initfifo", "ptys", "utmps", "ontty -s 03:45"), &["-c"])).env("SHIM_MINUTE_MS", "1"),
        // /etc/nologin exists for the last 5 minutes (sampled midway).
        t(osd(
            prelude!(
                "initfifo",
                "(exec -a openrc-shutdown \"$BIN\" -H 3 > /run/bg.out 2>&1 &); sleep 0.6",
                "tr -d '\\r' < /etc/nologin",
                "while [ -e /run/openrc-shutdown.pid ]; do sleep 0.1; done; cat /run/bg.out",
            ),
            &["-c"],
        ))
        .env("SHIM_MINUTE_MS", "800"),
        // sysvinit's FIFO with nobody reading it.
        osd(prelude!("mkfifo /run/initctl"), &["-r", "now"]),
    ];
    common::check_all("shutdown-tty", &cases);
}

#[test]
fn shutdown_later() {
    if common::reference_dir().is_none() {
        return;
    }
    // (The shim's clock is 01:46:40 UTC; at hh:mm it's computed locally.)
    let cases = vec![
        // Warnings, to the logged-in pty, each "minute"; nologin at the
        // end.
        osd(prelude!("initfifo", "\"$MKUTMP\" root \"$(tty | sed s,/dev/,,)\""), &["-r", "3"])
            .env("MKUTMP", mkutmp())
            .env("OSD_STDIN", "/dev/null")
            .pty(),
        osd(prelude!("initfifo", "\"$MKUTMP\" root \"$(tty | sed s,/dev/,,)\""), &["-p", "+12"])
            .env("MKUTMP", mkutmp())
            .env("OSD_STDIN", "/dev/null")
            .pty(),
        osd(prelude!("initfifo"), &["-s", "2"]),
        osd(
            prelude!(
                "initfifo",
                "\"$MKUTMP\" root \"$(tty | sed s,/dev/,,)\"",
                "set -- -r $(date -d @1000000120 +%H:%M)"
            ),
            &["-r", "placeholder"],
        )
        .env("MKUTMP", mkutmp())
        .env("OSD_STDIN", "/dev/null")
        .pty(),
        // Cancelled: from another openrc-shutdown, and by a signal.
        osd(
            prelude!(
                "initfifo",
                "(LD_PRELOAD= exec -a openrc-shutdown \"$BIN\" -r 10 > /run/bg.out 2>&1 &); sleep 0.3",
                "osd -c; sleep 0.3; cat /run/bg.out; echo then:",
            ),
            &["-c"],
        ),
        osd(
            prelude!(
                "initfifo",
                "(LD_PRELOAD= exec -a openrc-shutdown \"$BIN\" -H 10 > /run/bg.out 2>&1 &); sleep 0.3",
                "ls /run; kill -INT $(cat /run/openrc-shutdown.pid); sleep 0.3; cat /run/bg.out; echo then:",
            ),
            &["-c"],
        ),
    ];
    common::check_all("shutdown-later", &cases);
}

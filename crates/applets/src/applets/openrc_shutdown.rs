//! `openrc-shutdown`: port of upstream's `src/openrc-shutdown/`
//! (`openrc-shutdown.c`, `broadcast.c`, `sysvinit.c`, and
//! `src/shared/wtmp.c`).  Halts, powers off, reboots or goes single-user,
//! now or after a delay (warning logged-in users, and creating
//! /etc/nologin for the last 5 minutes), by asking openrc-init through
//! its FIFO, or sysvinit through /run/initctl; also cancels a pending
//! shutdown, re-executes init, or writes a wtmp record.

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::{bcat, num, stdio, sys};
use openrc_einfo::{eerror, eerrorx, einfo};

use crate::shared::{Usage, get_pid, getoptstring_common};

const GETOPTSTRING: &str = concat!("cdDfFHKpRrsw", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("cancel", HasArg::No, b'c'),
    LongOpt::new("no-write", HasArg::No, b'd'),
    LongOpt::new("dry-run", HasArg::No, b'D'),
    LongOpt::new("halt", HasArg::No, b'H'),
    LongOpt::new("kexec", HasArg::No, b'K'),
    LongOpt::new("poweroff", HasArg::No, b'p'),
    LongOpt::new("reexec", HasArg::No, b'R'),
    LongOpt::new("reboot", HasArg::No, b'r'),
    LongOpt::new("single", HasArg::No, b's'),
    LongOpt::new("write-only", HasArg::No, b'w'),
];
const LONGOPTS_HELP: &[&str] = &[
    "cancel a pending shutdown",
    "do not write wtmp record",
    "print actions instead of executing them",
    "halt the system",
    "reboot the system using kexec",
    "power off the system",
    "re-execute init (use after upgrading)",
    "reboot the system",
    "single user mode",
    "write wtmp boot record and exit",
];
const USAGESTRING: &str = "Usage: openrc-shutdown -c | --cancel\n   or: openrc-shutdown -R | --reexec\n   or: \
     openrc-shutdown -w | --write-only\n   or: openrc-shutdown -H | --halt time\n   or: openrc-shutdown -K | \
     --kexec time\n   or: openrc-shutdown -p | --poweroff time\n   or: openrc-shutdown -r | --reboot time\n   \
     or: openrc-shutdown -s | --single time";
const EXCLUSIVE: &str =
    "Select one of --cancel, --halt, --kexec, --poweroff, --reexec, --reboot, --single or \n--write-only";
const SHUTDOWN_PID: &[u8] = b"/run/openrc-shutdown.pid";
/// utmp's `RUN_LVL`.
const RUN_LVL: i16 = 1;
const INIT_CMD_RUNLVL: i32 = 1;
const INIT_CMD_SETENV: i32 = 6;

fn nologin_file() -> Vec<u8> {
    bcat!(rc::paths::SYSCONFDIR, "/nologin")
}

/// `RC_INIT_FIFO`: openrc-init's control FIFO.
fn init_fifo() -> Vec<u8> {
    bcat!(rc::paths::SVCDIR, "/init.ctl")
}

static DRYRUN: AtomicBool = AtomicBool::new(false);

extern "C" fn stop_shutdown(_sig: i32) {
    let _ = sys::unlink(&nologin_file());
    let _ = sys::unlink(SHUTDOWN_PID);
    einfo(Some(b"Shutdown cancelled"));
    stdio::exit(0);
}

/// What was asked for (C's `do_*` flags).
#[derive(Default)]
struct Want {
    halt: bool,
    kexec: bool,
    poweroff: bool,
    reboot: bool,
    wtmp: bool,
}

/// `send_cmd()`: write `cmd` to openrc-init's FIFO (with a wtmp shutdown
/// record when halting or rebooting).
fn send_cmd(w: &Want, cmd: &[u8]) {
    if DRYRUN.load(SeqCst) {
        einfo(Some(&bcat!("Would send ", cmd, " to init")));
        return;
    }
    if w.wtmp && (w.halt || w.kexec || w.reboot || w.poweroff) {
        sys::log_wtmp(b"shutdown", b"~~", 0, RUN_LVL, b"~~");
    }
    // fopen(RC_INIT_FIFO, "w"): a regular file if there's no FIFO.
    match std::fs::File::create(openrc_compat::bytes::as_path(&init_fifo())) {
        Ok(mut f) => {
            use std::io::Write;
            if f.write_all(cmd).is_err() {
                stdio::out(b"Error writing to init fifo\n");
            }
        }
        Err(e) => {
            // perror("fopen")
            let e = sys::Errno(e.raw_os_error().unwrap_or(0));
            stdio::err(&bcat!("fopen: ", e.strerror(), "\n"));
        }
    }
}

/// `sysvinit_send_cmd()`: write a request to sysvinit's /run/initctl.
fn sysvinit_send(request: &[u8]) {
    use sys::libc;
    let fd =
        match sys::open_raw(b"/run/initctl", libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOCTTY, 0) {
            Ok(fd) => fd,
            Err(e) => {
                if e.0 != libc::ENOENT {
                    eerror(Some(&bcat!("Failed to open initctl fifo: ", e.strerror())));
                }
                return;
            }
        };
    let mut p = request;
    while !p.is_empty() {
        let r = sys::write_raw(fd, p);
        if r < 0 {
            let e = sys::Errno::last();
            if e.0 == libc::EAGAIN || e.0 == libc::EINTR {
                continue;
            }
            // (C returns here without closing the fd.)
            eerror(Some(&bcat!("Failed to write to /run/initctl: ", e.strerror())));
            return;
        }
        p = &p[r as usize..];
    }
    // (C never closes it either: the process ends soon after.)
}

/// `sysvinit_runlevel()`.
fn sysvinit_runlevel(rl: u8) {
    sysvinit_send(&sys::init_request(INIT_CMD_RUNLVL, i32::from(rl), b""));
}

/// `sysvinit_setenv()`.
fn sysvinit_setenv(name: &[u8], value: &[u8]) {
    if name.len() + value.len() + 3 >= 368 {
        return;
    }
    sysvinit_send(&sys::init_request(INIT_CMD_SETENV, 0, &bcat!(name, "=", value)));
}

/// `file_isatty()`.  (QUIRK: the excluded majors are checked against
/// `st_dev`, the device of the filesystem holding the node, not the node's
/// own device number, so they never match on devtmpfs or devpts.)
fn file_isatty(fname: &[u8]) -> bool {
    let Ok(st) = sys::stat(fname) else { return false };
    if st.nlink != 1 || st.file_type() != sys::libc::S_IFCHR {
        return false;
    }
    !matches!(sys::major(st.dev), 1 | 2 | 6 | 9 | 12 | 16 | 21 | 27 | 37 | 96 | 97 | 206 | 230)
}

/// `broadcast()`: write `text`, under a header, to every logged-in user's
/// terminal, from a child (so a stuck terminal can't hang us).
fn broadcast(text: &[u8]) {
    use sys::libc;
    // getuidtty()
    let uid = sys::getuid();
    let user = match sys::getpwuid(uid) {
        // strncat into a 32-byte buffer.
        Some(pw) => pw.name.iter().take(31).copied().collect::<Vec<u8>>(),
        None if uid != 0 => format!("uid {uid}").into_bytes(),
        None => b"root".to_vec(),
    };
    let tty = sys::ttyname(0).map(|t| {
        let t = t.strip_prefix(b"/dev/").unwrap_or(&t);
        let t = t.strip_prefix(b"/").unwrap_or(t);
        bcat!("(", t, ") ")
    });
    let mut date = sys::ctime(sys::time());
    if let Some(i) = date.iter().position(|&c| c == b'\n') {
        date.truncate(i);
    }
    let line = bcat!(
        "\x07\r\nBroadcast message from ",
        user,
        "@",
        sys::uname_nodename(),
        " ",
        // QUIRK: no tty is a NULL "%s", which glibc prints as "(null)".
        tty.unwrap_or_else(|| b"(null)".to_vec()),
        "(",
        date,
        "):\r\n\r\n",
        text
    );

    // Fork to avoid hanging in a write().
    if !matches!(sys::fork(), Ok(0)) {
        return;
    }
    for (ut_type, ut_user, ut_line) in sys::utmp_entries() {
        if ut_type != libc::USER_PROCESS || ut_user.is_empty() {
            continue;
        }
        let term = if ut_line.starts_with(b"/dev/") { ut_line } else { bcat!("/dev/", ut_line) };
        if term.windows(4).any(|w| w == b"/../") {
            continue;
        }
        // Open it non-delay.
        if file_isatty(&term)
            && let Ok(fd) = sys::open_raw(&term, libc::O_WRONLY | libc::O_NDELAY | libc::O_NOCTTY, 0)
        {
            if sys::isatty(fd) {
                sys::write_raw(fd, &line);
            }
            sys::close_raw(fd);
        }
    }
    stdio::exit(0)
}

/// `create_nologin()`.
fn create_nologin(mins: i32) {
    let t = sys::time() + 60 * i64::from(mins);
    let _ = std::fs::write(
        openrc_compat::bytes::as_path(&nologin_file()),
        bcat!("\rThe system is going down on ", sys::ctime(t), "\r\n"),
    );
}

/// `sleep_no_interrupt()`.
fn sleep_no_interrupt(seconds: i64) {
    sys::nanosleep_restart(seconds * 1_000_000_000);
}

pub fn main(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    use sys::libc;
    let applet = openrc_compat::bytes::basename_c(args.first().map_or(&b""[..], |a| a)).to_vec();
    let usage = Usage {
        applet: applet.clone(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: None,
        usagestring: Some(USAGESTRING),
    };
    let longopts = usage.all_longopts();
    let argc = args.len();
    let mut w = Want { wtmp: true, ..Default::default() };
    let (mut cancel, mut reexec, mut single, mut wtmp_only) = (false, false, false, false);
    let mut state: &str = "";
    let mut cmd_count = 0;
    let mut g = Getopt::new(args.clone(), GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match u8::try_from(opt).unwrap_or(0) {
            b'c' => {
                cancel = true;
                cmd_count += 1;
            }
            b'd' => w.wtmp = false,
            b'D' => DRYRUN.store(true, SeqCst),
            b'H' => {
                w.halt = true;
                state = "halt";
                cmd_count += 1;
            }
            b'K' => {
                w.kexec = true;
                state = "reboot";
                cmd_count += 1;
            }
            b'p' => {
                w.poweroff = true;
                state = "power off";
                cmd_count += 1;
            }
            b'R' => {
                reexec = true;
                cmd_count += 1;
            }
            b'r' => {
                w.reboot = true;
                state = "reboot";
                cmd_count += 1;
            }
            b's' => {
                single = true;
                state = "go down for maintenance";
                cmd_count += 1;
            }
            b'w' => {
                wtmp_only = true;
                cmd_count += 1;
            }
            _ => usage.common_option(opt, argc),
        }
    }
    if sys::geteuid() != 0 {
        // (C's message ends in a newline of its own.)
        eerrorx(Some(&bcat!(applet, ": you must be root\n")));
    }
    if cmd_count != 1 {
        eerror(Some(&bcat!(applet, ": ", EXCLUSIVE, "\n")));
        usage.usage(1);
    }

    if cancel {
        // cancel_shutdown()
        let pid = get_pid(&applet, Some(SHUTDOWN_PID));
        if pid <= 0 {
            eerrorx(Some(&bcat!(applet, ": Unable to cancel shutdown")));
        }
        if sys::kill(pid, libc::SIGTERM).is_ok() {
            einfo(Some(&bcat!(applet, ": shutdown canceled")));
        } else {
            eerrorx(Some(&bcat!(applet, ": Unable to cancel shutdown")));
        }
        stdio::exit(0);
    } else if reexec {
        send_cmd(&w, b"reexec");
        stdio::exit(0);
    } else if wtmp_only {
        sys::log_wtmp(b"shutdown", b"~~", 0, RUN_LVL, b"~~");
        stdio::exit(0);
    }

    let Some(arg) = g.args.get(g.optind) else {
        eerror(Some(&bcat!(applet, ": No shutdown time specified")));
        usage.usage(1);
    };
    let mut time_arg = arg.strip_prefix(b"+").unwrap_or(arg).to_vec();
    if time_arg.eq_ignore_ascii_case(b"now") {
        time_arg = b"0".to_vec();
    }
    if time_arg.iter().any(|&c| !c.is_ascii_digit() && c != b':') {
        eerror(Some(&bcat!(applet, ": invalid time ", time_arg)));
        usage.usage(1);
    }
    let mut shutdown_delay: i32 = if time_arg.contains(&b':') {
        // sscanf("%2d:%2d"): at most two digits each.
        let hm = scan_hh_mm(&time_arg);
        let Some((hour, min)) = hm.filter(|&(h, m)| h <= 23 && m <= 59) else {
            eerror(Some(&bcat!(applet, ": invalid time ", time_arg)));
            usage.usage(1);
        };
        let (lh, lm) = sys::local_hour_min(sys::time());
        let mut d = (hour * 60 + min) - (lh * 60 + lm);
        if d < 0 {
            d += 1440;
        }
        d
    } else {
        num::atoi(&time_arg)
    };

    if let Err(e) = std::fs::write(openrc_compat::bytes::as_path(SHUTDOWN_PID), format!("{}\n", sys::getpid())) {
        let e = sys::Errno(e.raw_os_error().unwrap_or(0));
        eerrorx(Some(&bcat!(applet, ": fopen `", SHUTDOWN_PID, "': ", e.strerror())));
    }

    sys::openlog(&applet, libc::LOG_PID, libc::LOG_DAEMON);
    sys::signal_setup(libc::SIGINT, stop_shutdown);
    sys::signal_setup(libc::SIGTERM, stop_shutdown);
    while shutdown_delay > 0 {
        let need_warning = if shutdown_delay > 180 {
            shutdown_delay % 60 == 0
        } else if shutdown_delay > 60 {
            shutdown_delay % 30 == 0
        } else if shutdown_delay > 10 {
            shutdown_delay % 15 == 0
        } else {
            true
        };
        if shutdown_delay <= 5 {
            create_nologin(shutdown_delay);
        }
        if need_warning {
            broadcast(format!("\rThe system will {state} in {shutdown_delay} minutes\r\n").as_bytes());
        }
        sleep_no_interrupt(60);
        shutdown_delay -= 1;
    }
    broadcast(format!("\rThe system will {state} now\r\n").as_bytes());
    sys::syslog(libc::LOG_NOTICE, format!("The system will {state} now").as_bytes());
    let _ = sys::unlink(&nologin_file());
    let _ = sys::unlink(SHUTDOWN_PID);
    let sysvinit = sys::access(b"/run/initctl", libc::F_OK).is_ok();
    if w.halt {
        if sysvinit {
            sysvinit_setenv(b"INIT_HALT", b"HALT");
            sysvinit_runlevel(b'0');
        } else {
            send_cmd(&w, b"halt");
        }
    } else if w.kexec {
        send_cmd(&w, b"kexec");
    } else if w.poweroff {
        if sysvinit {
            sysvinit_setenv(b"INIT_HALT", b"POWEROFF");
            sysvinit_runlevel(b'0');
        } else {
            send_cmd(&w, b"poweroff");
        }
    } else if w.reboot {
        if sysvinit {
            sysvinit_runlevel(b'6');
        } else {
            send_cmd(&w, b"reboot");
        }
    } else if single {
        if sysvinit {
            sysvinit_runlevel(b'S');
        } else {
            send_cmd(&w, b"single");
        }
    }
    0
}

/// `sscanf(s, "%2d:%2d", &h, &m) == 2`.
fn scan_hh_mm(s: &[u8]) -> Option<(i32, i32)> {
    let field = |s: &[u8]| -> Option<(i32, usize)> {
        // %2d: optional white space, a sign, then up to 2 chars in all.
        let mut i = s.iter().take_while(|&&c| num::is_c_space(c)).count();
        let start = i;
        let mut neg = false;
        if matches!(s.get(i), Some(b'+' | b'-')) {
            neg = s[i] == b'-';
            i += 1;
        }
        let digits_start = i;
        while i - start < 2 && s.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == digits_start {
            return None;
        }
        let v: i32 = std::str::from_utf8(&s[digits_start..i]).ok()?.parse().ok()?;
        Some((if neg { -v } else { v }, i))
    };
    let (h, n) = field(s)?;
    let rest = s[n..].strip_prefix(b":")?;
    let (m, _) = field(rest)?;
    Some((h, m))
}

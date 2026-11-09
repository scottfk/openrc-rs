//! Port of upstream's `src/shared/schedules.c`: signal names and the
//! retry schedules (`--retry TERM/30/KILL/5`) that start-stop-daemon and
//! supervise-daemon use to stop daemons, and `do_stop()`, which signals
//! the matching processes.

use std::sync::Mutex;

use openrc_compat::{bcat, env, num, stdio, sys};
use openrc_einfo::{ebeginv, eendv, eerror, eerrorx, einfo, einfov, einfovn};

/// Nanoseconds between checks while waiting for processes to go.
const POLL_INTERVAL: i64 = 20_000_000;
const ONE_SECOND: i64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Timeout(i32),
    Signal(i32),
    /// Back to the item at this index.
    Goto(usize),
    Forever,
}

static SCHEDULE: Mutex<Vec<Item>> = Mutex::new(Vec::new());

fn schedule() -> std::sync::MutexGuard<'static, Vec<Item>> {
    SCHEDULE.lock().unwrap_or_else(|e| e.into_inner())
}

/// `free_schedulelist()`.
pub fn free_schedulelist() {
    schedule().clear();
}

/// The signal names `parse_signal()` knows, as in C's table.
const SIGNALS: &[(&str, i32)] = {
    use sys::libc::*;
    &[
        ("HUP", SIGHUP),
        ("INT", SIGINT),
        ("QUIT", SIGQUIT),
        ("ILL", SIGILL),
        ("TRAP", SIGTRAP),
        ("ABRT", SIGABRT),
        ("BUS", SIGBUS),
        ("FPE", SIGFPE),
        ("KILL", SIGKILL),
        ("USR1", SIGUSR1),
        ("SEGV", SIGSEGV),
        ("USR2", SIGUSR2),
        ("PIPE", SIGPIPE),
        ("ALRM", SIGALRM),
        ("TERM", SIGTERM),
        ("CHLD", SIGCHLD),
        ("CONT", SIGCONT),
        ("STOP", SIGSTOP),
        ("TSTP", SIGTSTP),
        ("TTIN", SIGTTIN),
        ("TTOU", SIGTTOU),
        ("URG", SIGURG),
        ("XCPU", SIGXCPU),
        ("XFSZ", SIGXFSZ),
        ("VTALRM", SIGVTALRM),
        ("PROF", SIGPROF),
        ("WINCH", SIGWINCH),
        ("IO", SIGIO),
        ("PWR", SIGPWR),
        ("SYS", SIGSYS),
        ("NULL", 0),
    ]
};

/// glibc's `NSIG`.
const NSIG: u32 = 65;

/// `parse_signal()`: a signal number or name (with or without `SIG`); -1
/// for an empty one; exits on an unknown one.
pub fn parse_signal(applet: &[u8], sig: Option<&[u8]>) -> i32 {
    let Some(sig) = sig.filter(|s| !s.is_empty()) else { return -1 };
    // sscanf(sig, "%u", &i): strtoul, stored in an unsigned int.
    let p = num::strtoul(sig, 10);
    if p.end > 0 {
        let i = p.value as u32;
        if i < NSIG {
            return i as i32;
        }
        eerrorx(Some(&bcat!(applet, ": `", sig, "' is not a valid signal")));
    }
    let s = sig.strip_prefix(b"SIG");
    for &(name, num) in SIGNALS {
        if sig == name.as_bytes() || s == Some(name.as_bytes()) {
            return num;
        }
    }
    eerrorx(Some(&bcat!(applet, ": `", sig, "' is not a valid signal")))
}

/// `parse_schedule_item()`.
fn parse_schedule_item(applet: &[u8], string: &[u8]) -> Item {
    if string == b"forever" {
        return Item::Forever;
    }
    if string.first().is_some_and(u8::is_ascii_digit) {
        return match num::scanf_int(string) {
            Some(v) => Item::Timeout(v),
            None => eerrorx(Some(&bcat!(applet, ": invalid timeout value in schedule '", string, "'"))),
        };
    }
    let after_hyph = string.strip_prefix(b"-").unwrap_or(string);
    match parse_signal(applet, Some(after_hyph)) {
        -1 => eerrorx(Some(&bcat!(applet, ": invalid schedule item `", string, "'"))),
        sig => Item::Signal(sig),
    }
}

/// `parse_schedule(applet, string, timeout)`: a plain number (or none) is
/// "send `timeout` (the signal), then wait that long (5s)".
pub fn parse_schedule(applet: &[u8], string: Option<&[u8]>, timeout: i32) {
    let count = string.map_or(0, |s| s.iter().filter(|&&c| c == b'/').count());
    let mut list = Vec::new();
    if count == 0 {
        list.push(Item::Signal(timeout));
        let value = match string {
            Some(s) => match num::scanf_int(s) {
                Some(v) => v,
                None => eerrorx(Some(&bcat!(applet, ": invalid timeout value in schedule '", s, "'"))),
            },
            None => 5,
        };
        list.push(Item::Timeout(value));
        *schedule() = list;
        return;
    }

    let mut repeatat = None;
    let mut rest = string;
    while let Some(s) = rest {
        let (item, next) = match s.iter().position(|&c| c == b'/') {
            Some(i) => (&s[..i], Some(&s[i + 1..])),
            None => (s, None),
        };
        // C copies each item into a 20-byte buffer.
        if item.len() >= 20 {
            eerrorx(Some(&bcat!(applet, ": invalid schedule item, far too long")));
        }
        rest = next;
        let it = parse_schedule_item(applet, item);
        list.push(it);
        if it == Item::Forever {
            if repeatat.is_some() {
                eerrorx(Some(&bcat!(applet, ": invalid schedule, `forever' appears more than once")));
            }
            repeatat = Some(list.len() - 1);
        }
    }
    if let Some(i) = repeatat {
        list.push(Item::Goto(i));
    }
    *schedule() = list;
}

/// `do_stop()`: send `sig` to the matching processes (by pid, or by exec,
/// argv and uid; to their process groups with `group`).  The number
/// signalled, or -1 if one couldn't be.
#[allow(clippy::too_many_arguments)]
pub fn do_stop(
    applet: &[u8],
    exec: Option<&[u8]>,
    argv: Option<&[Vec<u8>]>,
    pid: i32,
    uid: u32,
    sig: i32,
    group: bool,
    test: bool,
    quiet: bool,
) -> i32 {
    let kind = if group { "GID" } else { "PID" };
    let pids = if pid > 0 {
        rc::daemon::rc_find_pids(None, None, 0, pid)
    } else {
        rc::daemon::rc_find_pids(exec, argv, uid, 0)
    };
    let Some(pids) = pids else { return 0 };
    let mut nkilled = 0;
    for p in pids {
        let ps = p.to_string();
        let what = bcat!("signal ", sig.to_string().as_bytes(), " to ", kind, " ", ps.as_bytes());
        if test {
            einfo(Some(&bcat!("Would send ", what)));
            nkilled += 1;
            continue;
        }
        if sig != 0 {
            sys::syslog(sys::libc::LOG_DEBUG, &bcat!("Sending ", what));
            if !quiet {
                ebeginv(Some(&bcat!("Sending ", what)));
            }
        }
        sys::set_errno(0);
        let target = if group { -p } else { p };
        let r = sys::kill(target, sig);
        let e = match r {
            Ok(()) => sys::Errno(0),
            Err(e) => e,
        };
        let killed = r.is_ok() || e.0 == sys::libc::ESRCH;
        if !quiet {
            eendv(if killed { 0 } else { 1 }, Some(&bcat!(applet, ": failed to send ", what, ": ", e.strerror())));
        } else if !killed {
            sys::syslog(sys::libc::LOG_ERR, &bcat!("Failed to send ", what, ": ", e.strerror()));
        }
        if !killed {
            nkilled = -1;
        } else if nkilled != -1 {
            nkilled += 1;
        }
    }
    nkilled
}

/// `run_stop_schedule()`: run the schedule against the matching processes.
/// The number stopped (0 if none matched), or minus the number still
/// running.
#[allow(clippy::too_many_arguments)]
pub fn run_stop_schedule(
    applet: &[u8],
    exec: Option<&[u8]>,
    argv: Option<&[Vec<u8>]>,
    pid: i32,
    uid: u32,
    group: bool,
    test: bool,
    progress: bool,
    quiet: bool,
) -> i32 {
    let has_argv = argv.is_some_and(|a| !a.is_empty());
    if !(pid > 0 || exec.is_some() || uid != 0 || has_argv) {
        return 0;
    }
    if let Some(exec) = exec {
        einfov(Some(&bcat!("Will stop ", exec)));
        sys::syslog(sys::libc::LOG_DEBUG, &bcat!("Will stop ", exec));
    }
    if pid > 0 {
        let m = bcat!("Will stop ", if group { "GID " } else { "PID " }, pid.to_string().as_bytes());
        einfov(Some(&m));
        sys::syslog(sys::libc::LOG_DEBUG, &m);
    }
    if uid != 0 {
        let m = bcat!("Will stop processes owned by UID ", uid.to_string().as_bytes());
        einfov(Some(&m));
        sys::syslog(sys::libc::LOG_DEBUG, &m);
    }
    if let Some(argv) = argv.filter(|a| !a.is_empty()) {
        einfovn(Some(b"Will stop processes of `"));
        if rc::misc::rc_yesno(env::get("EINFO_VERBOSE").as_deref()) {
            stdio::out(&argv.join(&b' '));
            stdio::out(b"'\n");
        }
    }

    let list = schedule().clone();
    let mut nkilled = 0;
    let mut tkilled = 0;
    let mut nrunning = 0;
    let mut progressed = false;
    let mut idx = 0;
    while idx < list.len() {
        match list[idx] {
            Item::Goto(to) => {
                idx = to;
                continue;
            }
            Item::Signal(sig) => {
                nrunning = 0;
                nkilled = do_stop(applet, exec, argv, pid, uid, sig, group, test, quiet);
                if nkilled == 0 {
                    if tkilled == 0 {
                        if progressed {
                            stdio::out(b"\n");
                        }
                        eerror(Some(&bcat!(applet, ": no matching processes found")));
                    }
                    return tkilled;
                } else if nkilled == -1 {
                    return 0;
                }
                tkilled += nkilled;
            }
            item @ (Item::Forever | Item::Timeout(_)) => {
                let value = match item {
                    Item::Timeout(v) => v,
                    _ => 0,
                };
                if matches!(item, Item::Timeout(_)) && value < 1 {
                    // (C sets the item to NULL: the schedule ends.)
                    break;
                }
                let mut nsecs = 0;
                while item == Item::Forever || nsecs < value {
                    for _ in 0..ONE_SECOND / POLL_INTERVAL {
                        nrunning = do_stop(applet, exec, argv, pid, uid, 0, group, test, quiet);
                        if nrunning == 0 {
                            return 0;
                        }
                        if !sys::nanosleep(POLL_INTERVAL) {
                            if progressed {
                                stdio::out(b"\n");
                                progressed = false;
                            }
                            let e = sys::Errno::last();
                            if e.0 != sys::libc::EINTR {
                                eerror(Some(&bcat!(applet, ": nanosleep: ", e.strerror())));
                                return 0;
                            }
                        }
                    }
                    if progress {
                        stdio::out(b".");
                        stdio::flush();
                        progressed = true;
                    }
                    nsecs += 1;
                }
            }
        }
        idx += 1;
    }

    if test || (tkilled > 0 && nrunning == 0) {
        return nkilled;
    }
    if progressed {
        stdio::out(b"\n");
    }
    if !quiet {
        let n = nrunning.to_string();
        if nrunning == 1 {
            eerror(Some(&bcat!(applet, ": ", n.as_bytes(), " process refused to stop")));
        } else {
            eerror(Some(&bcat!(applet, ": ", n.as_bytes(), " process(es) refused to stop")));
        }
    }
    -nrunning
}

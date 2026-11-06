//! `kill_all`: port of `src/kill_all/kill_all.c`.  Sends a signal (default
//! SIGKILL) to every user process outside our session, at shutdown.
//!
//! QUIRK (upstream, and dangerous): even with `--dry-run` it first stops
//! *every process it may signal* with `kill(-1, SIGSTOP)`, and resumes
//! them with SIGCONT at the end.  Never run it outside a test VM or PID
//! namespace.

use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::num::{atoi, scanf_int};
use openrc_compat::sys::{self, libc};
use openrc_compat::{bcat, env};
use openrc_einfo::{eerror, eerrorx, einfo};

use crate::shared::{Usage, getoptstring_common};

const GETOPTSTRING: &str = concat!("do:", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[LongOpt::new("dry-run", HasArg::No, b'd'), LongOpt::new("omit", HasArg::Required, b'o')];
const LONGOPTS_HELP: &[&str] = &["print what would be done", "omit this pid (can be repeated)"];

/// `mount_proc()`: make sure /proc is there.
fn mount_proc() -> bool {
    if sys::access(b"/proc/version", libc::F_OK).is_ok() {
        return true;
    }
    let args = openrc_compat::exec::ExecArgs::new(["mount", "-t", "proc", "proc", "/proc"]);
    let Ok(res) = openrc_compat::exec::do_exec(&args) else {
        sys::syslog(libc::LOG_ERR, b"Failed to execute mount");
        return false;
    };
    // wait() until our child is reaped.
    let mut reaped = None;
    loop {
        match sys::waitpid(-1, 0) {
            Ok((pid, st)) if pid == res.pid => {
                reaped = Some(st);
                break;
            }
            Ok(_) => continue,
            Err(e) if e.0 == libc::ECHILD => break,
            Err(_) => continue,
        }
    }
    if reaped.and_then(sys::exit_status) != Some(0) {
        sys::syslog(libc::LOG_ERR, b"mount returned non-zero exit status");
    }
    if sys::access(b"/proc/version", libc::F_OK).is_err() {
        sys::syslog(libc::LOG_ERR, b"Could not mount /proc");
        return false;
    }
    true
}

/// `is_user_process()`: follow the PPid chain to 0 (user) or 2 (kthreadd).
fn is_user_process(mut pid: i32) -> bool {
    while pid > 0 {
        if pid == 2 {
            return false;
        }
        // If the process vanished we cannot tell, so play safe: not a user
        // process.
        let Ok(data) = std::fs::read(format!("/proc/{pid}/status")) else { return false };
        let mut ppid = -1;
        for line in data.split(|&b| b == b'\n') {
            // sscanf(buf, "PPid: %d", &ppid): the literal, any whitespace,
            // then an int.
            if let Some(rest) = line.strip_prefix(b"PPid:")
                && let Some(v) = scanf_int(rest)
            {
                ppid = v;
            }
        }
        if ppid == -1 {
            sys::syslog(libc::LOG_ERR, format!("Unable to read pid from /proc/{pid}/status").as_bytes());
            return false;
        }
        pid = ppid;
    }
    true
}

fn signal_processes(sig: i32, omits: &[Vec<u8>], dryrun: bool) -> i32 {
    let _ = sys::kill(-1, libc::SIGSTOP);
    let old = sys::block_all_signals();
    let resume = |old: &libc::sigset_t| {
        sys::restore_sigmask(old);
        let _ = sys::kill(-1, libc::SIGCONT);
    };
    // Work from /proc, in case / is affected by the killing (e.g. fuse).
    if sys::chdir(b"/proc").is_err() {
        sys::syslog(libc::LOG_ERR, b"chdir /proc failed");
        resume(&old);
        return -1;
    }
    let names = match sys::openat(None, b".", libc::O_RDONLY | libc::O_DIRECTORY, 0).and_then(sys::readdir_names) {
        Ok(n) => n,
        Err(_) => {
            sys::syslog(libc::LOG_ERR, b"cannot opendir(/proc)");
            resume(&old);
            return -1;
        }
    };
    let mut sendcount = 0;
    let my_sid = sys::getsid(sys::getpid()).unwrap_or(-1);
    for name in names {
        let pid = atoi(&name);
        if pid == 0 {
            continue;
        }
        // QUIRK: omits are compared as given ("007" never matches 7).
        if omits.iter().any(|o| *o == pid.to_string().as_bytes()) {
            continue;
        }
        if my_sid == sys::getsid(pid).unwrap_or(-1) {
            continue;
        }
        if !is_user_process(pid) {
            continue;
        }
        if dryrun {
            einfo(Some(format!("Would send signal {sig} to process {pid}").as_bytes()));
        } else if sys::kill(pid, sig).is_ok() {
            sendcount += 1;
        }
    }
    resume(&old);
    sendcount
}

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    // Ensure that we are only quiet when explicitly told to be.
    env::unset("EINFO_QUIET");
    let argc = args.len();
    let usage = Usage {
        applet: applet.to_vec(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: Some("[signal number]"),
        usagestring: None,
    };
    let longopts = usage.all_longopts();
    let mut omits: Vec<Vec<u8>> = vec![b"1".to_vec()];
    let mut dryrun = false;
    let mut sig = libc::SIGKILL;

    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match u8::try_from(opt).unwrap_or(0) {
            b'd' => dryrun = true,
            b'o' => {
                let optarg = g.optarg.clone().unwrap_or_default();
                for token in optarg.split(|&b| b == b',' || b == b';' || b == b':') {
                    if atoi(token) > 0 {
                        if !omits.iter().any(|o| o == token) {
                            omits.push(token.to_vec());
                        }
                    } else {
                        eerror(Some(&bcat!("Invalid omit pid value ", token)));
                        usage.usage(1);
                    }
                }
            }
            _ => usage.common_option(opt, argc),
        }
    }

    if let Some(arg) = g.args.get(g.optind) {
        sig = atoi(arg);
        if sig <= 0 || sig > 31 {
            eerror(Some(&bcat!("Invalid signal ", arg)));
            usage.usage(1);
        }
    }

    sys::openlog(applet, libc::LOG_CONS | libc::LOG_PID, libc::LOG_DAEMON);
    if !mount_proc() {
        eerrorx(Some(b"Unable to mount /proc file system"));
    }
    signal_processes(sig, &omits, dryrun);
    0
}

//! `openrc-init`: port of upstream's `src/openrc-init/openrc-init.c`, an
//! init (PID 1) for OpenRC.  It runs the sysinit, boot and default
//! runlevels, then waits for commands on its FIFO (`/run/openrc/init.ctl`,
//! from openrc-shutdown) and for signals (INT: reboot; TERM, PWR: halt),
//! and shuts down: the shutdown or reboot runlevel, TERM and then KILL to
//! everything left, and `reboot()`.
//!
//! SELinux policy loading is not ported: the reference build has
//! `-Dselinux=disabled`.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::SeqCst};

use openrc_compat::{bcat, env, exec, stdio, sys};

const PATH_DEFAULT: &[u8] = b"/sbin:/usr/sbin:/bin:/usr/bin";
const RC_DEFAULT_RUNLEVEL: &[u8] = b"default";
/// utmp's `RUN_LVL`.
const RUN_LVL: i16 = 1;

static SIGPIPE_W: AtomicI32 = AtomicI32::new(-1);
static QUIET: AtomicBool = AtomicBool::new(false);

fn quiet() -> bool {
    QUIET.load(SeqCst)
}

/// `do_openrc()`: run `openrc <runlevel>` in a new session and wait for it.
fn do_openrc(runlevel: &[u8]) {
    if !quiet() {
        stdio::out(&bcat!("Starting ", runlevel, " runlevel\n"));
    }
    let mut args = exec::ExecArgs::new([&b"openrc"[..], runlevel]);
    args.setsid = true;
    let pid = match exec::do_exec(&args) {
        Ok(r) => r.pid,
        Err(e) => {
            // (do_exec() leaves the exec's errno for perror.)
            sys::set_errno(e.0);
            sys::perror(b"do_exec");
            stdio::exit(1);
        }
    };
    loop {
        match sys::waitpid(pid, 0) {
            Ok((p, _)) if p == pid => break,
            Err(e) if e.0 == sys::libc::ECHILD => break,
            _ => {}
        }
    }
}

/// `init()`: sysinit, boot, then the default (or given) runlevel.
fn init(default_runlevel: Option<&[u8]>) {
    do_openrc(b"sysinit");
    do_openrc(b"boot");
    let mut runlevel = match default_runlevel {
        Some(r) => r.to_vec(),
        None => rc::conf::rc_conf_value("rc_default_runlevel")
            .map(|v| openrc_compat::bytes::c_str_prefix(&v).to_vec())
            .unwrap_or_else(|| RC_DEFAULT_RUNLEVEL.to_vec()),
    };
    if !rc::state::rc_runlevel_exists(&runlevel) {
        stdio::err(&bcat!(runlevel, " is an invalid runlevel\n"));
        runlevel = RC_DEFAULT_RUNLEVEL.to_vec();
    }
    do_openrc(&runlevel);
    sys::log_wtmp(b"reboot", b"~~", 0, RUN_LVL, b"~~");
}

/// `handle_reexec()`: exec ourselves again, keeping the state.
fn handle_reexec(progname: &[u8]) {
    // (Like C, unflushed output is lost.)
    let _ = sys::execvp(progname, &[progname.to_vec(), b"reexec".to_vec()]);
}

/// `handle_shutdown()`: the runlevel, then TERM (with half a second to
/// three seconds of grace) and KILL to everything, then `reboot(cmd)`.
fn handle_shutdown(runlevel: &[u8], cmd: i32) {
    use sys::libc;
    do_openrc(runlevel);
    if !quiet() {
        stdio::out(b"Sending the final TERM signal\n");
    }
    let _ = sys::kill(-1, libc::SIGTERM);
    let start = sys::boottime_ns();
    // At least half a second of grace before KILL.  QUIRK: C means to
    // resume the sleep after EINTR, but tests clock_nanosleep()'s result
    // for -1 (it returns the error number), so a child exiting (SIGCHLD)
    // ends the grace at once.
    sys::boottime_sleep(500_000_000, false);
    loop {
        let (pid, _) = sys::waitpid_raw(-1, libc::WNOHANG);
        if pid == -1 && sys::Errno::last().0 == libc::ECHILD {
            // All children reaped, safe to go on.
            break;
        }
        // Three seconds already: go for the KILL hammer.  (C compares
        // the whole seconds of the difference.)
        if (sys::boottime_ns() - start) / 1_000_000_000 >= 3 {
            break;
        }
        sys::boottime_sleep(100_000_000, false);
    }
    if !quiet() {
        stdio::out(b"Sending the final KILL signal\n");
    }
    let _ = sys::kill(-1, libc::SIGKILL);
    sys::sync();
    let _ = sys::reboot(cmd);
    // Reboot normally if kexec fails.
    if cmd == libc::RB_KEXEC {
        let _ = sys::reboot(libc::RB_AUTOBOOT);
    }
}

/// `run_program()`.
fn run_program(prog: &[u8]) {
    let args = exec::ExecArgs::new([prog]);
    match exec::do_exec(&args) {
        Ok(r) if exec::rc_waitpid(r.pid) != -1 => {}
        Ok(_) => sys::perror(b"init"),
        Err(e) => {
            sys::set_errno(e.0);
            sys::perror(b"init");
        }
    }
}

/// `open_shell()`: run `rc_shell`, `$SHELL`, the user's shell or /bin/sh
/// (on a VSERVER system, halt instead).
fn open_shell() {
    if rc::rc_sys() == Some("VSERVER") {
        let _ = sys::execvp(b"halt", &[b"halt".to_vec(), b"-f".to_vec()]);
        sys::perror(b"init");
        return;
    }
    let shell = rc::conf::rc_conf_value("rc_shell")
        .map(|s| openrc_compat::bytes::c_str_prefix(&s).to_vec())
        .or_else(|| env::get("SHELL"))
        .or_else(|| sys::getpwuid(sys::getuid()).map(|pw| pw.shell))
        .unwrap_or_else(|| b"/bin/sh".to_vec());
    run_program(&shell);
}

/// `handle_single()`: the single runlevel, a shell, then boot again.
fn handle_single() {
    do_openrc(b"single");
    open_shell();
    init(None);
}

/// `dispatch_signal()`: act on a signal the handler passed down the pipe.
fn dispatch_signal(sigfd: i32) {
    use sys::libc;
    let mut b = [0u8; 4];
    let count = sys::read_raw(sigfd, &mut b);
    if count < 0 {
        sys::perror(b"read(sigpipe)");
        return;
    }
    if count != 4 {
        stdio::err(b"bad read from sigpipe\n");
        return;
    }
    match i32::from_ne_bytes(b) {
        libc::SIGINT => handle_shutdown(b"reboot", libc::RB_AUTOBOOT),
        libc::SIGTERM | libc::SIGPWR => handle_shutdown(b"shutdown", libc::RB_HALT_SYSTEM),
        sig => {
            stdio::err(format!("Unknown signal received, {sig}\n").as_bytes());
        }
    }
}

/// `dispatch_fifo()`: act on a command from the FIFO.
fn dispatch_fifo(fifo: i32, progname: &[u8]) {
    use sys::libc;
    let mut buf = [0u8; 2048];
    let count = sys::read_raw(fifo, &mut buf[..2047]);
    if count < 0 {
        sys::perror(b"read(fifo)");
        return;
    }
    let cmd = openrc_compat::bytes::c_str_prefix(&buf[..count as usize]);
    if !quiet() {
        stdio::out(&bcat!("PID1: Received \"", cmd, "\" from FIFO...\n"));
    }
    match cmd {
        b"halt" => handle_shutdown(b"shutdown", libc::RB_HALT_SYSTEM),
        b"kexec" => handle_shutdown(b"reboot", libc::RB_KEXEC),
        b"poweroff" => handle_shutdown(b"shutdown", libc::RB_POWER_OFF),
        b"reboot" => handle_shutdown(b"reboot", libc::RB_AUTOBOOT),
        b"reexec" => handle_reexec(progname),
        b"single" => handle_single(),
        _ => {}
    }
}

extern "C" fn signal_handler(sig: i32) {
    let saved = sys::Errno::last().0;
    sys::write_raw(SIGPIPE_W.load(SeqCst), &sig.to_ne_bytes());
    sys::set_errno(saved);
}

extern "C" fn reap_zombies(_sig: i32) {
    use sys::libc;
    let saved = sys::Errno::last().0;
    loop {
        let (pid, _) = sys::waitpid_raw(-1, libc::WNOHANG);
        if pid == 0 {
            break;
        }
        if pid == -1 {
            if sys::Errno::last().0 == libc::ECHILD {
                break;
            }
            sys::write_raw(2, b"waitpid() failed\n");
        }
    }
    sys::set_errno(saved);
}

pub fn main(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    use sys::libc;
    let progname = args.first().cloned().unwrap_or_default();
    if sys::getpid() != 1 {
        return 1;
    }
    QUIET.store(rc::misc::rc_yesno(env::get("EINFO_QUIET").as_deref()), SeqCst);
    if !quiet() {
        stdio::out(&bcat!("OpenRC init version ", rc::VERSION, " starting\n"));
    }
    let (reexec, cmdline_runlevel) = match args.get(1) {
        Some(a) if a == b"reexec" => (true, None),
        Some(a) => (false, Some(a.clone())),
        None => (false, None),
    };

    let (sig_r, sig_w) = match sys::pipe2_flags(libc::O_NONBLOCK | libc::O_CLOEXEC) {
        Ok(p) => p,
        Err(_) => {
            sys::perror(b"pipe2");
            return 1;
        }
    };
    SIGPIPE_W.store(sig_w, SeqCst);

    // Block all signals we don't handle.
    sys::block_all_but(&[libc::SIGCHLD, libc::SIGINT, libc::SIGTERM, libc::SIGPWR]);
    sys::signal_setup(libc::SIGCHLD, reap_zombies);
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGPWR] {
        sys::signal_setup(sig, signal_handler);
    }
    let _ = sys::reboot(libc::RB_DISABLE_CAD);

    // Set the default path.
    env::set("PATH", PATH_DEFAULT);

    if !reexec {
        init(cmdline_runlevel.as_deref());
    }

    let fifo = match sys::open_rwfifo(&bcat!(rc::paths::SVCDIR, "/init.ctl"), true) {
        Ok(fd) => fd,
        Err(_) => {
            sys::perror(b"open_rwfifo");
            -1
        }
    };

    loop {
        let mut pfd = [
            sys::PollFd { fd: fifo, events: libc::POLLIN, revents: 0 },
            sys::PollFd { fd: sig_r, events: libc::POLLIN, revents: 0 },
        ];
        let _ = sys::poll(&mut pfd, 2, -1);
        // Signals first.
        if pfd[1].revents & libc::POLLIN != 0 {
            dispatch_signal(sig_r);
        }
        if pfd[0].revents & libc::POLLIN != 0 {
            dispatch_fifo(fifo, &progname);
        }
    }
}

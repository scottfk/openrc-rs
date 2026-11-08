//! `openrc-run`: port of `src/openrc-run/openrc-run.c`, the interpreter
//! for init scripts (`#!/sbin/openrc-run`).  It sets up the environment
//! and runs `openrc-run.sh` for each command, handling dependencies,
//! locking and state for start and stop.
//!
//! `pause` isn't one of C's commands (QUIRK), so it goes to openrc-run.sh
//! like any other.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::SeqCst};

use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::stdio::{self, Stream};
use openrc_compat::{bcat, env, exec, num, sys};
use openrc_einfo::{EColor, ecolor, eerror, eerrorn, eerrorx, einfo, einfon, einfov, eprefix, ewarn, ewarnv, ewarnx};
use rc::deptree::{self, DepTree, options};
use rc::state::{self, flags};
use rc::svcdir::{RcDir, rc_dirfd_raw, rc_svcdir};

use crate::plugin::{self, hook, rc_plugin_run};
use crate::shared::{
    Usage, default_runlevel, env_config, env_filter, exec_service, getoptstring_common, rc_conf_yesno,
    rc_deptree_load_updating, svc_lock, svc_unlock,
};

/// Seconds until we time out waiting for a service, and how often we warn.
const WAIT_TIMEOUT: i32 = 60;
const WARN_TIMEOUT: i32 = 10;

const GETOPTSTRING: &str = concat!("dDsSvl:Z", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("debug", HasArg::No, b'd'),
    LongOpt::new("dry-run", HasArg::No, b'Z'),
    LongOpt::new("ifstarted", HasArg::No, b's'),
    LongOpt::new("ifstopped", HasArg::No, b'S'),
    LongOpt::new("nodeps", HasArg::No, b'D'),
    LongOpt::new("lockfd", HasArg::Required, b'l'),
];
const LONGOPTS_HELP: &[&str] = &[
    "set xtrace when running the script",
    "show what would be done",
    "only run commands when started",
    "only run commands when stopped",
    "ignore dependencies",
    "fd of the exclusive lock from rc",
];
const EXTRAOPTS: &str = "stop | start | restart | status | describe | zap";

// --- State shared with the signal handler and the atexit cleanup ------------

static SIGHUP: AtomicBool = AtomicBool::new(false);
static SKIP_MARK: AtomicBool = AtomicBool::new(false);
static TIMEDOUT: AtomicBool = AtomicBool::new(false);
static SERVICE_PID: AtomicI32 = AtomicI32::new(0);
static SIGNAL_PIPE_W: AtomicI32 = AtomicI32::new(-1);
static EXCLUSIVE_FD: AtomicI32 = AtomicI32::new(-1);
/// The applet (service) name, for messages from the handlers.
static APPLET: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Services to start again once we're done (`restart_services`).
static RESTART_SERVICES: Mutex<Option<Vec<Vec<u8>>>> = Mutex::new(None);

fn applet() -> Vec<u8> {
    APPLET.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

extern "C" fn handle_signal(sig: i32) {
    let serrno = sys::Errno::last().0;
    use sys::libc;
    match sig {
        libc::SIGHUP => SIGHUP.store(true, SeqCst),
        libc::SIGUSR1 => SKIP_MARK.store(true, SeqCst),
        libc::SIGCHLD => {
            while let Ok((pid, status)) = sys::waitpid(-1, libc::WNOHANG) {
                if pid <= 0 {
                    break;
                }
                let w = SIGNAL_PIPE_W.load(SeqCst);
                if w > -1 && pid == SERVICE_PID.load(SeqCst) && sys::write_raw(w, &status.to_ne_bytes()) == -1 {
                    let e = sys::Errno::last();
                    eerror(Some(&bcat!(applet(), ": send: ", e.strerror())));
                }
            }
        }
        // (C has a SIGWINCH case, but never installs a handler for it.)
        libc::SIGINT | libc::SIGTERM | libc::SIGQUIT => {
            let signame = match sig {
                libc::SIGINT => "SIGINT",
                libc::SIGTERM => "SIGTERM",
                _ => "SIGQUIT",
            };
            // Send the signal to our children too.
            let pid = SERVICE_PID.load(SeqCst);
            if pid > 0 {
                let _ = sys::kill(pid, sig);
            }
            eerror(Some(&bcat!(applet(), ": caught ", signame, ", aborting")));
            stdio::exit(1);
        }
        libc::SIGALRM => TIMEDOUT.store(true, SeqCst),
        _ => {
            eerror(Some(&bcat!(applet(), ": caught unknown signal ", sig.to_string().as_bytes())));
        }
    }
    sys::set_errno(serrno);
}

fn unhotplug(applet: &[u8]) {
    if let Err(e) = sys::at::unlinkat(rc_dirfd_raw(RcDir::Hotplugged), applet, 0)
        && e.0 != sys::libc::ENOENT
    {
        eerror(Some(&bcat!(applet, ": unlink '", rc_svcdir(), "/hotplugged/", applet, "': ", e.strerror())));
    }
}

/// `restore_state()`: if we hold the lock and left the service half-way
/// (starting or stopping), put it back.
fn restore_state() {
    let fd = EXCLUSIVE_FD.load(SeqCst);
    if plugin::rc_in_plugin() || fd == -1 {
        return;
    }
    let applet = applet();
    let st = state::rc_service_state(&applet);
    if st & flags::STOPPING != 0 {
        state::rc_service_mark(&applet, if st & flags::WASINACTIVE != 0 { flags::INACTIVE } else { flags::STARTED });
        if state::rc_runlevel_stopping() {
            state::rc_service_mark(&applet, flags::FAILED);
        }
    } else if st & flags::STARTING != 0 {
        state::rc_service_mark(&applet, if st & flags::WASINACTIVE != 0 { flags::INACTIVE } else { flags::STOPPED });
        if state::rc_runlevel_starting() {
            state::rc_service_mark(&applet, flags::FAILED);
        }
    }
    EXCLUSIVE_FD.store(svc_unlock(&applet, fd), SeqCst);
}

/// `start_services()`: start (or schedule) the services in `list` that
/// are stopped, if we got (or are getting) started.
fn start_services(list: &[Vec<u8>]) {
    let applet = applet();
    let st = state::rc_service_state(&applet);
    if st & (flags::INACTIVE | flags::WASINACTIVE | flags::STARTING | flags::STARTED) == 0 {
        return;
    }
    for svc in list {
        if state::rc_service_state(svc) & flags::STOPPED == 0 {
            continue;
        }
        if st & (flags::INACTIVE | flags::WASINACTIVE) != 0 {
            state::rc_service_schedule_start(&applet, svc);
            ewarn(Some(&bcat!("WARNING: ", svc, " will start when ", applet, " has started")));
        } else {
            exec_service(svc, b"start");
        }
    }
}

/// `get_started_services()`: the restart list is what's started, then
/// what's inactive.
fn get_started_services() {
    let mut list = state::rc_services_in_state(flags::STARTED);
    list.extend(state::rc_services_in_state(flags::INACTIVE));
    *RESTART_SERVICES.lock().unwrap_or_else(|e| e.into_inner()) = Some(list);
}

extern "C" fn cleanup() {
    restore_state();
    if !plugin::rc_in_plugin() {
        let restart = RESTART_SERVICES.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(list) = restart {
            start_services(&list);
        }
    }
    plugin::rc_plugin_unload();
    stdio::flush();
}

// --- Running openrc-run.sh --------------------------------------------------

/// The parts of C's file-scope state that the commands use.
struct Run {
    applet: Vec<u8>,
    /// The realpath of the init script.
    service: Vec<u8>,
    runlevel: Vec<u8>,
    /// eprefix() string with rc_parallel.
    prefix: Option<Vec<u8>>,
    deptree: Option<DepTree>,
    /// C's `deptypes_*` lists, set up by the first start or stop with
    /// dependencies (until then lookups through them find nothing).
    deptypes: bool,
    deps: bool,
    dry_run: bool,
    in_background: bool,
    /// The IN_BACKGROUND we were given, passed on to our own script only.
    ibsave: Option<Vec<u8>>,
    need_services: Vec<Vec<u8>>,
    want_services: Vec<Vec<u8>>,
    use_services: Vec<Vec<u8>>,
}

fn strings(list: &[&str]) -> Vec<Vec<u8>> {
    list.iter().map(|s| s.as_bytes().to_vec()).collect()
}

/// `write(1, buf, len)`, retried when a signal interrupts it.
///
/// Deviation: C doesn't retry, so a SIGCHLD (installed without SA_RESTART)
/// landing in a write to the terminal can drop bytes of the script's output.
fn write_retry(buf: &[u8]) {
    while sys::write_raw(1, buf) < 0 && sys::Errno::last().0 == sys::libc::EINTR {}
}

/// `write_prefix()`: copy the child's output to stdout, with our prefix at
/// the start of each line (but not before an eend's cursor-up), under
/// `$svcdir/prefix.lock`.
fn write_prefix(prefix: &[u8], buffer: &[u8], prefixed: &mut bool) {
    let ec = ecolor(EColor::Hilite).to_bytes();
    let ec_normal = ecolor(EColor::Normal).to_bytes();
    // open() may fail when running as a user (RC_SVCDIR not writable).
    let lock =
        sys::at::openat(rc_dirfd_raw(RcDir::Svcdir), b"prefix.lock", sys::libc::O_WRONLY | sys::libc::O_CREAT, 0o664);
    match &lock {
        Ok(fd) => loop {
            match sys::flock(std::os::fd::AsFd::as_fd(fd), sys::libc::LOCK_EX) {
                Ok(()) => break,
                Err(e) if e.0 == sys::libc::EINTR => continue,
                Err(e) => {
                    ewarnv(Some(&bcat!("flock() failed: ", e.strerror())));
                    break;
                }
            }
        },
        Err(_) => {
            ewarnv(Some(b"Couldn't open the prefix lock, please make sure you have enough permissions"));
        }
    }

    for (i, &b) in buffer.iter().enumerate() {
        // We don't prefix eend calls (cursor up).
        if b == 0x1b && !*prefixed {
            for &c in &buffer[i + 1..] {
                if c == b'A' {
                    *prefixed = true;
                }
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
        if !*prefixed {
            write_retry(ec);
            write_retry(openrc_compat::bytes::c_str_prefix(prefix));
            write_retry(ec_normal);
            write_retry(b"|");
            *prefixed = true;
        }
        if b == b'\n' {
            *prefixed = false;
        }
        write_retry(&[b]);
    }
    drop(lock);
}

impl Run {
    /// `svc_exec()`: run `openrc-run.sh <service> <command>` and wait for
    /// it, relaying its output through a pty when prefixing.  Returns its
    /// exit status (1 if it couldn't be started).
    fn svc_exec(&self, command: &[u8]) -> i32 {
        use sys::libc;
        let mut ret = -1;
        let (sig_r, sig_w) = match sys::pipe2_cloexec() {
            Ok(p) => p,
            Err(_) => {
                // QUIRK: names the applet where the error should be.
                eerror(Some(&bcat!(self.applet, ": pipe2: ", self.applet)));
                return 1;
            }
        };
        let sig_r_raw = std::os::fd::AsRawFd::as_raw_fd(&sig_r);
        SIGNAL_PIPE_W.store(std::os::fd::IntoRawFd::into_raw_fd(sig_w), SeqCst);

        // A pty for prefixed output, so programs still see a terminal.  C
        // never closes the slave in the parent, so the master never hangs
        // up.
        let mut master_tty = -1;
        let mut slave_tty = None;
        if self.prefix.is_some()
            && Stream::Stdout.isatty()
            && let Some((m, s)) = sys::openpty_like(1)
        {
            master_tty = m;
            slave_tty = Some(s);
        }

        let mut argv0 = format!("{}/sh/openrc-run.sh", rc::paths::LIBEXECDIR).into_bytes();
        if sys::at::faccessat(rc_dirfd_raw(RcDir::Svcdir), b"openrc-run.sh", libc::F_OK).is_ok() {
            argv0 = bcat!(rc_svcdir(), "/openrc-run.sh");
        }

        einfov(Some(&bcat!("Executing: ", argv0, " ", self.service, " ", command)));
        let argv = vec![argv0.clone(), self.service.clone(), command.to_vec()];
        if let Err(e) = exec::spawn(&argv0, &argv, slave_tty, |pid| SERVICE_PID.store(pid, SeqCst)) {
            eerror(Some(&bcat!(self.service, ": exec '", argv0, "': ", e.strerror())));
            return 1;
        }

        let mut fds = [
            sys::PollFd { fd: sig_r_raw, events: libc::POLLIN, revents: 0 },
            sys::PollFd { fd: master_tty, events: libc::POLLIN, revents: 0 },
        ];
        let mut prefixed = false;
        loop {
            let n = if master_tty >= 0 { 2 } else { 1 };
            if let Err(e) = sys::poll(&mut fds, n, -1) {
                if e.0 == libc::EINTR {
                    continue;
                }
                eerror(Some(&bcat!(self.applet, ": poll: ", e.strerror())));
                break;
            }
            if n == 2 && fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                let mut buffer = [0u8; 8192];
                // Deviation: C passes a failed read's -1 on as a length.
                let r = sys::read_raw(master_tty, &mut buffer).max(0) as usize;
                write_prefix(self.prefix.as_deref().unwrap_or_default(), &buffer[..r], &mut prefixed);
            }
            // The signal pipe receives the service's exit status.
            if fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                sys::set_errno(0);
                let mut status = [0u8; 4];
                if sys::read_raw(sig_r_raw, &mut status) < 4 {
                    let e = sys::Errno::last();
                    let why = if e.0 != 0 { e.strerror() } else { b"short read".to_vec() };
                    eerror(Some(&bcat!(self.applet, ": receive failed: ", why)));
                } else {
                    // QUIRK: WEXITSTATUS of the raw status, so a script
                    // killed by a signal counts as success.  (C also
                    // checks errno == ECHILD here, which it can't be.)
                    ret = (i32::from_ne_bytes(status) >> 8) & 0xff;
                }
                break;
            }
        }

        let old = sys::block_signal(libc::SIGCHLD);
        drop(sig_r);
        let w = SIGNAL_PIPE_W.swap(-1, SeqCst);
        sys::close_raw(w);
        sys::restore_sigmask(&old);

        if master_tty >= 0 {
            sys::close_raw(master_tty);
        }
        // (The slave stays open, as in C.)
        SERVICE_PID.store(0, SeqCst);
        ret
    }

    /// `simple_command()`: run a command without prefixing its output.
    fn simple_command(&mut self, command: &[u8]) -> i32 {
        let save = self.prefix.take();
        eprefix(None);
        let ret = self.svc_exec(command);
        eprefix(save.as_deref());
        self.prefix = save;
        ret
    }

    /// `resolve_deptype()`: print the services of one dependency type.
    fn resolve_deptype(&mut self, deptype: &[u8]) -> i32 {
        let mut depoptions = options::TRACE;
        let (strict, err) = rc_conf_yesno("rc_depend_strict");
        if strict || err == Some(sys::libc::ENOENT) {
            depoptions |= options::STRICT;
        }
        if self.deptree.is_none() {
            self.deptree = rc_deptree_load_updating(0, None);
            if self.deptree.is_none() {
                eerrorx(Some(b"failed to load deptree"));
            }
        }
        let tree = self.deptree.as_ref().expect("loaded above");
        let (services, _) = deptree::rc_deptree_depends(
            tree,
            Some(&[deptype.to_vec()]),
            std::slice::from_ref(&self.applet),
            Some(&self.runlevel),
            depoptions,
        );
        let mut out = Vec::new();
        for s in services {
            out.extend_from_slice(&bcat!(openrc_compat::bytes::c_str_prefix(&s), " "));
        }
        out.push(b'\n');
        stdio::out(&out);
        0
    }

    fn cmd_zap(&self) -> i32 {
        einfo(Some(&bcat!("Manually resetting ", self.applet, " to stopped state")));
        if !state::rc_service_mark(&self.applet, flags::STOPPED) {
            let e = sys::Errno::last();
            eerrorx(Some(&bcat!("rc_service_mark: ", e.strerror())));
        }
        unhotplug(&self.applet);
        0
    }

    // --- start ----------------------------------------------------------

    /// `rc_deptree_depend(deptree, svc, ty)`, empty without a deptree.
    fn depend(&self, svc: &[u8], ty: &[u8]) -> Vec<Vec<u8>> {
        self.deptree.as_ref().and_then(|t| deptree::rc_deptree_depend(t, svc, ty).ok()).unwrap_or_default()
    }

    fn depends(&self, types: &[&str], options: i32) -> Vec<Vec<u8>> {
        let tree = self.deptree.as_ref().expect("loaded by svc_start_deps");
        deptree::rc_deptree_depends(
            tree,
            Some(&strings(types)),
            std::slice::from_ref(&self.applet),
            Some(&self.runlevel),
            options,
        )
        .0
    }

    /// The START_OUT hook, then exit(1).
    fn start_failed(&self) -> ! {
        rc_plugin_run(hook::SERVICE_START_OUT, &self.applet);
        stdio::exit(1)
    }

    /// `svc_getenv()`: take on the variables a started service reexports.
    fn svc_getenv(&self, svc: &[u8]) {
        for name in self.depend(svc, b"reexport") {
            if let Some(v) = rc::service::rc_service_getenv(svc, &name) {
                env::set_bytes(&name, openrc_compat::bytes::c_str_prefix(&v));
            }
        }
    }

    /// `svc_wait()`: wait (warning every 10 seconds, giving up after 60
    /// unless the service has the `-timeout` keyword) for `svc` to drop
    /// its exclusive lock.  False on a timeout or error.
    fn svc_wait(&self, svc: &[u8]) -> bool {
        use sys::libc;
        let base = openrc_compat::bytes::basename_c(svc);
        let keywords = self.depend(svc, b"keyword");
        let forever = keywords.iter().any(|k| k == b"-timeout" || k == b"notimeout");
        let fd = match sys::openat_raw(rc_dirfd_raw(RcDir::Exclusive), base, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Ok(fd) => fd,
            Err(e) => return e.0 == libc::ENOENT,
        };
        let mut timeout = WAIT_TIMEOUT;
        let mut retval = false;
        TIMEDOUT.store(false, SeqCst);
        loop {
            if !forever {
                sys::alarm(WARN_TIMEOUT as u32);
            }
            match sys::flock_raw(fd, libc::LOCK_SH) {
                Ok(()) => {
                    retval = true;
                    break;
                }
                Err(e) if e.0 != libc::EINTR => break,
                Err(_) => {}
            }
            if !forever && TIMEDOUT.load(SeqCst) {
                TIMEDOUT.store(false, SeqCst);
                timeout -= WARN_TIMEOUT;
                if timeout > 0 {
                    openrc_einfo::ewarn(Some(&bcat!(
                        self.applet,
                        ": waiting for ",
                        base,
                        " (",
                        timeout.to_string().as_bytes(),
                        " seconds)"
                    )));
                } else {
                    break;
                }
            }
        }
        sys::alarm(0);
        sys::close_raw(fd);
        retval
    }

    /// `svc_start_check()`: may we start?  Takes the lock and marks us
    /// starting, or exits.
    fn svc_start_check(&self) {
        let st = state::rc_service_state(&self.applet);
        let mut ret = 1;
        'check: {
            if self.in_background {
                if st & (flags::INACTIVE | flags::STOPPED) == 0 {
                    break 'check;
                }
                if rc::misc::rc_yesno(env::get("IN_HOTPLUG").as_deref()) {
                    state::rc_service_mark(&self.applet, flags::HOTPLUGGED);
                }
                if self.runlevel == state::RC_LEVEL_SYSINIT {
                    ewarnx(Some(&bcat!("WARNING: ", self.applet, " will be started in the next runlevel")));
                }
            }
            if st & flags::STARTED != 0 {
                ewarn(Some(&bcat!("WARNING: ", self.applet, " has already been started")));
                ret = 0;
                break 'check;
            } else if st & flags::INACTIVE != 0 && !self.in_background {
                ewarnx(Some(&bcat!("WARNING: ", self.applet, " has already started, but is inactive")));
            }

            if EXCLUSIVE_FD.load(SeqCst) == -1 {
                let r = svc_lock(&self.applet, !self.deps);
                EXCLUSIVE_FD.store(*r.as_ref().unwrap_or(&-1), SeqCst);
                if let Err(errno) = r {
                    if errno != sys::libc::EWOULDBLOCK {
                        eerror(Some(&bcat!(self.applet, ": failed to acquire lock: ", sys::strerror(errno))));
                        break 'check;
                    }
                    if st & flags::STOPPING != 0 {
                        ewarnx(Some(&bcat!("WARNING: ", self.applet, " is stopping")));
                    } else {
                        ewarnx(Some(&bcat!("WARNING: ", self.applet, " is already starting")));
                    }
                }
            }
            sys::set_cloexec(EXCLUSIVE_FD.load(SeqCst));

            state::rc_service_mark(&self.applet, flags::STARTING);
            rc_plugin_run(hook::SERVICE_START_IN, &self.applet);
            return;
        }
        rc_plugin_run(hook::SERVICE_START_OUT, &self.applet);
        stdio::exit(ret);
    }

    /// `svc_start_deps()`: start what we use, wait for what we need, or
    /// schedule ourselves to start once inactive needs have started.
    fn svc_start_deps(&mut self) {
        let mut depoptions = options::TRACE;
        let (strict, err) = rc_conf_yesno("rc_depend_strict");
        if strict || err == Some(sys::libc::ENOENT) {
            depoptions |= options::STRICT;
        }
        if self.deptree.is_none() {
            self.deptree = rc_deptree_load_updating(0, None);
            if self.deptree.is_none() {
                eerror(Some(b"failed to load deptree"));
                self.start_failed();
            }
        }
        self.deptypes = true;

        let broken = self.depends(&["broken"], 0);
        if !broken.is_empty() {
            eerrorn(Some(&bcat!("ERROR: ", self.applet, " needs service(s) ")));
            let mut line = Vec::new();
            for (i, svc) in broken.iter().enumerate() {
                if i > 0 {
                    line.extend_from_slice(b", ");
                }
                line.extend_from_slice(openrc_compat::bytes::c_str_prefix(svc));
            }
            line.push(b'\n');
            stdio::err(&line);
            self.start_failed();
        }

        self.need_services = self.depends(&["ineed"], depoptions);
        self.want_services = self.depends(&["ineed", "iwant"], depoptions);
        self.use_services = self.depends(&["ineed", "iwant", "iuse"], depoptions);

        if !state::rc_runlevel_starting() {
            for svc in &self.use_services {
                let st = state::rc_service_state(svc);
                // Don't stop failed services again.  (QUIRK: inside a
                // check that the runlevel isn't starting, so dead.)
                if st & flags::FAILED != 0 && state::rc_runlevel_starting() {
                    continue;
                }
                if st & flags::STOPPED != 0 {
                    if self.dry_run {
                        stdio::out(&bcat!(" ", openrc_compat::bytes::c_str_prefix(svc)));
                        continue;
                    }
                    let pid = exec_service(svc, b"start");
                    if !rc_conf_yesno("rc_parallel").0 {
                        exec::rc_waitpid(pid);
                    }
                }
            }
        }

        if self.dry_run {
            return;
        }

        // Now wait for them to start.
        let services = self.depends(&["ineed", "iwant", "iuse", "iafter"], depoptions);
        // The services we'll be scheduled by.
        let mut tmplist: Vec<Vec<u8>> = Vec::new();
        for svc in &services {
            let mut st = state::rc_service_state(svc);
            if st & flags::STARTED != 0 {
                self.svc_getenv(svc);
                continue;
            }
            // Don't wait for services which went inactive but are now
            // starting, which we are after.
            if st & flags::STARTING != 0
                && st & flags::WASINACTIVE != 0
                && !self.need_services.contains(svc)
                && !self.want_services.contains(svc)
                && !self.use_services.contains(svc)
            {
                continue;
            }
            if !self.svc_wait(svc) {
                eerror(Some(&bcat!(self.applet, ": timed out waiting for ", svc)));
            }
            st = state::rc_service_state(svc);
            if st & flags::STARTED != 0 {
                self.svc_getenv(svc);
                continue;
            }
            if self.need_services.contains(svc) {
                if st & (flags::INACTIVE | flags::WASINACTIVE) != 0 {
                    tmplist.push(svc.clone());
                } else if tmplist.is_empty() {
                    eerror(Some(&bcat!("ERROR: cannot start ", self.applet, " as ", svc, " would not start")));
                    self.start_failed();
                }
            }
        }

        if !tmplist.is_empty() {
            // Set the state now, then unlink our exclusive so that our
            // scheduled list is preserved.
            state::rc_service_mark(&self.applet, flags::STOPPED);
            let mut names = Vec::new();
            for (i, svc) in tmplist.iter().enumerate() {
                state::rc_service_schedule_start(svc, &self.applet);
                // QUIRK: service and type swapped, so this finds nothing
                // and what svc provides never gets the schedule.
                for provided in self.depend(b"iprovide", svc) {
                    state::rc_service_schedule_start(&provided, &self.applet);
                }
                if i > 0 {
                    names.extend_from_slice(b", ");
                }
                names.extend_from_slice(openrc_compat::bytes::c_str_prefix(svc));
            }
            let verb = if tmplist.len() > 1 { "have" } else { "has" };
            ewarnx(Some(&bcat!("WARNING: ", self.applet, " will start when ", names, " ", verb, " started")));
        }
    }

    /// `set_reexports()`: `RC_REEXPORT`, the variables we reexport.
    fn set_reexports(&self) {
        let mut v = Vec::new();
        for name in self.depend(&self.applet, b"reexport") {
            v.extend_from_slice(&bcat!(openrc_compat::bytes::c_str_prefix(&name), " "));
        }
        env::set_bytes(b"RC_REEXPORT", &v);
    }

    /// Start the services scheduled to start after `svc` that are stopped.
    fn start_scheduled(svc: &[u8]) {
        for s in state::rc_services_scheduled(svc) {
            if state::rc_service_state(&s) & flags::STOPPED != 0 {
                exec_service(&s, b"start");
            }
        }
    }

    /// `svc_start_real()`: run the script's start, mark us started, then
    /// start whatever was waiting for us.
    fn svc_start_real(&self) {
        self.set_reexports();
        if let Some(ib) = &self.ibsave {
            env::set_bytes(b"IN_BACKGROUND", ib);
        }
        rc_plugin_run(hook::SERVICE_START_NOW, &self.applet);
        SKIP_MARK.store(false, SeqCst);
        let started = self.svc_exec(b"start") == 0;
        if self.ibsave.is_some() {
            env::unset("IN_BACKGROUND");
        }
        if !started {
            rc_plugin_run(hook::SERVICE_START_DONE, &self.applet);
            rc_plugin_run(hook::SERVICE_START_OUT, &self.applet);
        }

        if state::rc_service_state(&self.applet) & flags::INACTIVE != 0 {
            ewarnx(Some(&bcat!("WARNING: ", self.applet, " has started, but is inactive")));
        } else if !started {
            eerrorx(Some(&bcat!("ERROR: ", self.applet, " failed to start")));
        }

        if !SKIP_MARK.load(SeqCst) {
            state::rc_service_mark(&self.applet, flags::STARTED);
        }
        EXCLUSIVE_FD.store(svc_unlock(&self.applet, EXCLUSIVE_FD.load(SeqCst)), SeqCst);
        rc_plugin_run(hook::SERVICE_START_DONE, &self.applet);

        // Now start any scheduled services, and those of what we provide
        // (QUIRK: service and type swapped, so none).
        Self::start_scheduled(&self.applet);
        if self.deptree.is_some() {
            for svc in self.depend(b"iprovide", &self.applet) {
                Self::start_scheduled(&svc);
            }
        }
        rc_plugin_run(hook::SERVICE_START_OUT, &self.applet);
    }

    /// `svc_start()`.
    fn svc_start(&mut self) -> i32 {
        if self.dry_run {
            einfon(Some(b"start:"));
        } else {
            self.svc_start_check();
        }
        if self.deps {
            self.svc_start_deps();
        }
        if self.dry_run {
            stdio::out(&bcat!(" ", self.applet, "\n"));
        } else {
            self.svc_start_real();
        }
        0
    }

    // --- stop -----------------------------------------------------------

    /// The STOP_OUT hook, then exit(1).
    fn stop_failed(&self) -> ! {
        rc_plugin_run(hook::SERVICE_STOP_OUT, &self.applet);
        stdio::exit(1)
    }

    /// `svc_stop_check()`: may we stop?  Takes the lock and marks us
    /// stopping (returning our state), returns None when already stopped,
    /// or exits.
    fn svc_stop_check(&self) -> Option<i32> {
        let st = state::rc_service_state(&self.applet);
        if state::rc_runlevel_stopping() && st & flags::FAILED != 0 {
            self.stop_failed();
        }
        // QUIRK: C's parentheses make this "unless started and not
        // inactive", so an inactive service is never stopped in the
        // background.
        if self.in_background && !(st & flags::STARTED != 0 && st & flags::INACTIVE == 0) {
            self.stop_failed();
        }
        if st & flags::STOPPED != 0 {
            ewarn(Some(&bcat!("WARNING: ", self.applet, " is already stopped")));
            return None;
        }

        if EXCLUSIVE_FD.load(SeqCst) == -1 {
            let r = svc_lock(&self.applet, !self.deps);
            EXCLUSIVE_FD.store(*r.as_ref().unwrap_or(&-1), SeqCst);
            if let Err(errno) = r {
                if errno != sys::libc::EWOULDBLOCK {
                    eerror(Some(&bcat!(self.applet, ": failed to acquire lock: ", sys::strerror(errno))));
                } else if st & flags::STOPPING != 0 {
                    ewarn(Some(&bcat!("WARNING: ", self.applet, " is already stopping")));
                } else {
                    eerror(Some(&bcat!("ERROR: ", self.applet, " stopped by something else")));
                }
                self.stop_failed();
            }
        }
        sys::set_cloexec(EXCLUSIVE_FD.load(SeqCst));

        state::rc_service_mark(&self.applet, flags::STOPPING);
        rc_plugin_run(hook::SERVICE_STOP_IN, &self.applet);

        if !state::rc_runlevel_stopping() {
            if state::rc_service_in_runlevel(&self.applet, state::RC_LEVEL_SYSINIT) {
                ewarn(Some(b"WARNING: you are stopping a sysinit service"));
            } else if state::rc_service_in_runlevel(&self.applet, state::RC_LEVEL_BOOT) {
                ewarn(Some(b"WARNING: you are stopping a boot service"));
            }
        }
        Some(st)
    }

    /// `svc_stop_deps()`: stop what needs us, and wait for what uses us.
    fn svc_stop_deps(&mut self, st: i32) {
        if st & flags::WASINACTIVE != 0 {
            return;
        }
        let mut depoptions = options::TRACE;
        let (strict, err) = rc_conf_yesno("rc_depend_strict");
        if strict || err == Some(sys::libc::ENOENT) {
            depoptions |= options::STRICT;
        }
        if self.deptree.is_none() {
            self.deptree = rc_deptree_load_updating(0, None);
            if self.deptree.is_none() {
                rc_plugin_run(hook::SERVICE_STOP_OUT, &self.applet);
                eerrorx(Some(b"failed to load deptree"));
            }
        }
        self.deptypes = true;

        let services = self.depends(&["needsme"], depoptions);
        let mut tmplist: Vec<Vec<u8>> = Vec::new();
        for svc in services.iter().rev() {
            let st = state::rc_service_state(svc);
            // Don't stop failed services again.  If you remove this
            // check, ensure that the exclusive file isn't created.
            if st & flags::FAILED != 0 && state::rc_runlevel_stopping() {
                continue;
            }
            if st & (flags::STARTED | flags::INACTIVE) == 0 {
                continue;
            }
            if self.dry_run {
                stdio::out(&bcat!(" ", openrc_compat::bytes::c_str_prefix(svc)));
                continue;
            }
            self.svc_wait(svc);
            if state::rc_service_state(svc) & (flags::STARTED | flags::INACTIVE) != 0 {
                let pid = exec_service(svc, b"stop");
                if !rc_conf_yesno("rc_parallel").0 {
                    exec::rc_waitpid(pid);
                }
                tmplist.push(svc.clone());
            }
        }
        if self.dry_run {
            return;
        }

        for svc in &tmplist {
            if state::rc_service_state(svc) & flags::STOPPED != 0 {
                continue;
            }
            self.svc_wait(svc);
            if state::rc_service_state(svc) & flags::STOPPED != 0 {
                continue;
            }
            if state::rc_runlevel_stopping() {
                // If shutting down, we should stop even if a dependent
                // failed.
                if self.runlevel == state::RC_LEVEL_SHUTDOWN || self.runlevel == state::RC_LEVEL_SINGLE {
                    continue;
                }
                state::rc_service_mark(&self.applet, flags::FAILED);
            }
            rc_plugin_run(hook::SERVICE_STOP_OUT, &self.applet);
            eerrorx(Some(&bcat!("ERROR: cannot stop ", self.applet, " as ", svc, " is still up")));
        }

        // Now wait for other services that may use us and are stopping.
        // This is important when a runlevel stops.
        for svc in self.depends(&["needsme", "wantsme", "usesme", "beforeme"], depoptions) {
            if state::rc_service_state(&svc) & flags::STOPPED != 0 {
                continue;
            }
            self.svc_wait(&svc);
        }
    }

    /// `load_dep_env()`: take on what the services we use reexport.
    fn load_dep_env(&self) {
        if !self.deptypes || self.deptree.is_none() {
            return;
        }
        for svc in self.depends(&["ineed", "iwant", "iuse"], 0) {
            self.svc_getenv(&svc);
        }
    }

    /// `svc_stop_real()`: run the script's stop and mark us stopped (or
    /// inactive, in the background).
    fn svc_stop_real(&self) {
        self.load_dep_env();
        // If we're stopping localmount, set LC_ALL=C so that bash doesn't
        // load anything blocking the unmounting of /usr.
        if self.applet == b"localmount" {
            env::set("LC_ALL", b"C");
        }
        if let Some(ib) = &self.ibsave {
            env::set_bytes(b"IN_BACKGROUND", ib);
        }
        rc_plugin_run(hook::SERVICE_STOP_NOW, &self.applet);
        SKIP_MARK.store(false, SeqCst);
        let stopped = self.svc_exec(b"stop") == 0;
        if self.ibsave.is_some() {
            env::unset("IN_BACKGROUND");
        }
        if stopped && !SKIP_MARK.load(SeqCst) {
            state::rc_service_mark(&self.applet, if self.in_background { flags::INACTIVE } else { flags::STOPPED });
        }
        rc_plugin_run(hook::SERVICE_STOP_DONE, &self.applet);
        rc_plugin_run(hook::SERVICE_STOP_OUT, &self.applet);
        if !stopped {
            eerrorx(Some(&bcat!("ERROR: ", self.applet, " failed to stop")));
        }
    }

    /// `svc_stop()`: 1 if we were stopped already.
    fn svc_stop(&mut self) -> i32 {
        let mut st = 0;
        if self.dry_run {
            einfon(Some(b"stop:"));
        } else {
            match self.svc_stop_check() {
                Some(s) => st = s,
                None => return 1,
            }
        }
        if self.deps {
            self.svc_stop_deps(st);
        }
        if self.dry_run {
            stdio::out(&bcat!(" ", self.applet, "\n"));
        } else {
            self.svc_stop_real();
        }
        0
    }

    /// `cmd_stop()`.
    fn cmd_stop(&mut self, cmd: &[u8]) -> i32 {
        // (QUIRK: `pause` isn't in the command table, so this never runs.)
        if cmd == b"pause" {
            ewarn(Some(b"WARNING: 'pause' is deprecated; please use '--nodeps stop'"));
            self.deps = false;
        }
        if self.deps && self.in_background {
            get_started_services();
        }
        if self.svc_stop() == 1 {
            // Stopped already.
            return 0;
        }
        if self.deps {
            let st = state::rc_service_state(&self.applet);
            if !self.in_background && !state::rc_runlevel_stopping() && st & flags::STOPPED != 0 {
                unhotplug(&self.applet);
            }
            if self.in_background && st & flags::INACTIVE != 0 {
                let list = RESTART_SERVICES.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
                for svc in &list {
                    if state::rc_service_state(svc) & flags::STOPPED != 0 {
                        state::rc_service_schedule_start(&self.applet, svc);
                    }
                }
            }
        }
        0
    }

    /// `svc_restart()`: stop (noting what was running), start, then start
    /// again what was running.
    fn svc_restart(&mut self) -> i32 {
        if state::rc_service_state(&self.applet) & flags::STOPPED == 0 {
            get_started_services();
            self.svc_stop();
            if self.dry_run {
                ewarn(Some(b"Cannot calculate restart start dependencies on a dry-run"));
            }
        }
        self.svc_start();
        let list = RESTART_SERVICES.lock().unwrap_or_else(|e| e.into_inner()).take();
        start_services(&list.unwrap_or_default());
        0
    }

    /// `cmd_condrestart()`: restart if started.
    fn cmd_condrestart(&mut self) -> i32 {
        if state::rc_service_state(&self.applet) & flags::STARTED != 0 {
            self.svc_restart();
        }
        0
    }
}

/// `service_plugable()`: may this service be hotplugged (`rc_hotplug`)?
fn service_plugable(applet: &[u8]) -> bool {
    let Some(list) = rc::conf::rc_conf_value("rc_hotplug").or_else(|| rc::conf::rc_conf_value("rc_plug_services"))
    else {
        return false;
    };
    let list = openrc_compat::bytes::c_str_prefix(&list);
    for token in list.split(|&b| b == b' ') {
        let (allow, pat) = match token.strip_prefix(b"!") {
            Some(rest) => (false, rest),
            None => (true, token),
        };
        if sys::fnmatch(pat, applet, 0) {
            return allow;
        }
    }
    true
}

#[derive(Clone, Copy)]
enum Cmd {
    Simple,
    Deptype,
    Start,
    Stop,
    Restart,
    Condrestart,
    Zap,
}

/// The commands openrc-run handles itself: `(name, handler, reset the
/// restart list afterwards)`.  Anything else goes to openrc-run.sh.
const COMMANDS: &[(&str, Cmd, bool)] = &[
    ("describe", Cmd::Simple, false),
    ("help", Cmd::Simple, false),
    ("depend", Cmd::Simple, false),
    ("status", Cmd::Simple, false),
    ("ineed", Cmd::Deptype, false),
    ("iuse", Cmd::Deptype, false),
    ("iwant", Cmd::Deptype, false),
    ("needsme", Cmd::Deptype, false),
    ("usesme", Cmd::Deptype, false),
    ("wantsme", Cmd::Deptype, false),
    ("iafter", Cmd::Deptype, false),
    ("ibefore", Cmd::Deptype, false),
    ("iprovide", Cmd::Deptype, false),
    ("start", Cmd::Start, true),
    ("stop", Cmd::Stop, true),
    ("restart", Cmd::Restart, true),
    ("condrestart", Cmd::Condrestart, true),
    ("conditionalrestart", Cmd::Condrestart, true),
    ("zap", Cmd::Zap, true),
];

pub fn main(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    use sys::libc;

    // Show help if insufficient args.
    if args.len() < 2 || sys::access(&args[1], libc::F_OK).is_err() {
        stdio::err(b"openrc-run should not be run directly\n");
        stdio::exit(1);
    }
    if let Err(e) = sys::stat(&args[1]) {
        stdio::err(&bcat!("openrc-run '", args[1], "': ", e.strerror(), "\n"));
        stdio::exit(1);
    }
    let Some(service) = sys::realpath(&args[1]) else {
        let e = sys::Errno::last();
        eerrorx(Some(&bcat!("readpath: ", e.strerror())));
    };
    let applet = openrc_compat::bytes::basename_c(&args[1]).to_vec();
    *APPLET.lock().unwrap_or_else(|e| e.into_inner()) = applet.clone();

    let usage = Usage {
        applet: applet.clone(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: Some(EXTRAOPTS),
        usagestring: None,
    };
    if args.len() < 3 {
        usage.usage(1);
    }
    // TODO(selinux): selinux_setup(); upstream only with -Dselinux.

    let mut deps = true;
    let mut dry_run = false;
    let longopts = usage.all_longopts();
    // Punt the first argument: it's our service.
    let argc = args.len() - 1;
    let mut g = Getopt::new(args[1..].to_vec(), GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match u8::try_from(opt).unwrap_or(0) {
            b'd' => env::set("RC_DEBUG", b"YES"),
            b'l' => {
                let fd = num::atoi(g.optarg.as_deref().unwrap_or_default());
                EXCLUSIVE_FD.store(fd, SeqCst);
                sys::set_cloexec(fd);
            }
            b's' => {
                if state::rc_service_state(&applet) & flags::STARTED == 0 {
                    stdio::exit(1);
                }
            }
            b'S' => {
                if state::rc_service_state(&applet) & flags::STOPPED == 0 {
                    stdio::exit(1);
                }
            }
            b'D' => deps = false,
            b'Z' => dry_run = true,
            _ => usage.common_option(opt, argc),
        }
    }

    // Run init scripts from / (a user's services from their HOME).
    let mut workingdir = b"/".to_vec();
    if rc::dirs::rc_is_user() {
        match env::get("HOME") {
            Some(h) => workingdir = h,
            None => eerrorx(Some(b"HOME is unset.")),
        }
    }
    if let Err(e) = sys::chdir(&workingdir) {
        eerror(Some(&bcat!("chdir: ", e.strerror())));
    }

    sys::atexit(cleanup);

    let runlevel = match env::get("RC_RUNLEVEL") {
        Some(r) => r,
        None => {
            env_filter();
            env_config();
            default_runlevel()
        }
    };

    env::set("EINFO_LOG", &applet);
    env::set("RC_SVCNAME", &applet);
    // Our pid, so mark_service_* in any subshell can tell us of changes.
    let pidstr = sys::getpid().to_string();
    env::set("RC_OPENRC_PID", pidstr.as_bytes());
    // RC_RUNSCRIPT_PID is deprecated, but kept for a while for safety.
    env::set("RC_RUNSCRIPT_PID", pidstr.as_bytes());

    // With rc_parallel, prefix output with our name, padded to the longest
    // service name.
    let mut prefix = None;
    if rc_conf_yesno("rc_parallel").0 {
        let l = state::rc_services_in_runlevel(None).iter().map(Vec::len).chain([applet.len()]).max().unwrap_or(0);
        let mut p = applet.clone();
        p.resize(l, b' ');
        eprefix(Some(&p));
        prefix = Some(p);
    }

    if rc::misc::rc_yesno(env::get("RC_NODEPS").as_deref()) {
        deps = false;
    }
    // Changing runlevels and not called by rc: no dependencies.
    if deps && env::get("RC_PID").is_none() && (state::rc_runlevel_starting() || state::rc_runlevel_stopping()) {
        deps = false;
    }

    // IN_BACKGROUND is only for this service, not for its dependents.
    let mut in_background = false;
    let ibsave = env::get("IN_BACKGROUND");
    if let Some(ib) = &ibsave {
        in_background = rc::misc::rc_yesno(Some(ib));
        env::unset("IN_BACKGROUND");
    }
    if rc::misc::rc_yesno(env::get("IN_DRYRUN").as_deref()) {
        dry_run = true;
    }
    if rc::misc::rc_yesno(env::get("IN_HOTPLUG").as_deref()) {
        if !service_plugable(&applet) {
            eerrorx(Some(&bcat!(applet, ": not allowed to be hotplugged")));
        }
        in_background = true;
    }

    for sig in [libc::SIGHUP, libc::SIGALRM, libc::SIGUSR1, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGCHLD] {
        sys::signal_setup(sig, handle_signal);
    }
    plugin::rc_plugin_load();

    if g.optind >= g.args.len() {
        usage.usage(1);
    }

    let mut run = Run {
        applet: applet.clone(),
        service,
        runlevel,
        prefix,
        deptree: None,
        deptypes: false,
        deps,
        dry_run,
        in_background,
        ibsave,
        need_services: Vec::new(),
        want_services: Vec::new(),
        use_services: Vec::new(),
    };
    let mut retval = 0;
    let cmds: Vec<Vec<u8>> = g.args[g.optind..].to_vec();
    for cmd in &cmds {
        // Abort on a SIGHUP here.
        if SIGHUP.load(SeqCst) {
            stdio::exit(1);
        }
        // Export the command we're running: some start/stop functions
        // behave differently when restarting.
        env::set("RC_CMD", cmd);

        let found = COMMANDS.iter().find(|(name, _, _)| name.as_bytes() == &cmd[..]);
        let cmdval = match found {
            Some(&(_, handler, _)) => match handler {
                Cmd::Simple => run.simple_command(cmd),
                Cmd::Deptype => run.resolve_deptype(cmd),
                Cmd::Zap => run.cmd_zap(),
                Cmd::Start => run.svc_start(),
                Cmd::Stop => run.cmd_stop(cmd),
                Cmd::Restart => run.svc_restart(),
                Cmd::Condrestart => run.cmd_condrestart(),
            },
            None => run.svc_exec(cmd),
        };
        if cmdval != 0 {
            retval = cmdval;
        }
        // After an action (or any script command) the list must be empty.
        if found.is_none_or(|&(_, _, reset)| reset) {
            *RESTART_SERVICES.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
    retval
}

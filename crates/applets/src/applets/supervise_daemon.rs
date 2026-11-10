//! `supervise-daemon`: port of upstream's
//! `src/supervise-daemon/supervise-daemon.c`.  `--start` forks a
//! supervisor that runs the daemon in the foreground and respawns it when
//! it dies (with delays, a maximum count per period, and health checks);
//! `--stop` stops the supervisor, which stops the daemon along the retry
//! schedule; `--signal` asks the supervisor (through a FIFO) to signal it.
//!
//! With the `pam` feature (upstream's `-Dpam=true`), a daemon run as
//! another user gets a PAM session (service `supervise-daemon`) and its
//! environment.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::SeqCst};

use openrc_compat::caps::{self, Iab};
use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::{bcat, env, exec, num, stdio, sys};
use openrc_einfo::{eerror, eerrorx, einfov, ewarn};

use super::start_stop_daemon::{expand_home, putenv, rc_pipe_command, scan_d_colon_d, scan_int_only};
use crate::schedules::{do_stop, parse_schedule, parse_signal, run_stop_schedule};
use crate::shared::{
    Notify, Usage, get_pid, getoptstring_common, notify_parse, notify_wait, parse_duration, parse_mode,
};

const LONGOPT_CAPABILITIES: i32 = 0x101;
const LONGOPT_OOM_SCORE_ADJ: i32 = 0x102;
const LONGOPT_NO_NEW_PRIVS: i32 = 0x103;
const LONGOPT_SECBITS: i32 = 0x104;
const LONGOPT_STDERR_LOGGER: i32 = 0x105;
const LONGOPT_STDOUT_LOGGER: i32 = 0x106;
const LONGOPT_NOTIFY: i32 = 0x107;
const LONGOPT_RESPAWN_DELAY_STEP: i32 = 0x108;
const LONGOPT_RESPAWN_DELAY_CAP: i32 = 0x109;

// QUIRK: `P` (--respawn-period) isn't in the short options, so only the
// long form works.
const GETOPTSTRING: &str = concat!("A:a:D:d:e:Gg:I:Kk:m:N:p:R:r:s:Su:0:1:2:3", getoptstring_common!());

const fn long(name: &'static str, has_arg: HasArg, val: i32) -> LongOpt {
    LongOpt { name, has_arg, val }
}
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("healthcheck-timer", HasArg::Required, b'a'),
    LongOpt::new("healthcheck-delay", HasArg::Required, b'A'),
    long("capabilities", HasArg::Required, LONGOPT_CAPABILITIES),
    long("secbits", HasArg::Required, LONGOPT_SECBITS),
    long("no-new-privs", HasArg::No, LONGOPT_NO_NEW_PRIVS),
    LongOpt::new("respawn-delay", HasArg::Required, b'D'),
    long("respawn-delay-step", HasArg::Required, LONGOPT_RESPAWN_DELAY_STEP),
    long("respawn-delay-cap", HasArg::Required, LONGOPT_RESPAWN_DELAY_CAP),
    LongOpt::new("chdir", HasArg::Required, b'd'),
    LongOpt::new("env", HasArg::Required, b'e'),
    LongOpt::new("group", HasArg::Required, b'g'),
    LongOpt::new("stop-group", HasArg::No, b'G'),
    LongOpt::new("ionice", HasArg::Required, b'I'),
    LongOpt::new("stop", HasArg::No, b'K'),
    LongOpt::new("umask", HasArg::Required, b'k'),
    LongOpt::new("respawn-max", HasArg::Required, b'm'),
    LongOpt::new("nicelevel", HasArg::Required, b'N'),
    long("oom-score-adj", HasArg::Required, LONGOPT_OOM_SCORE_ADJ),
    LongOpt::new("pidfile", HasArg::Required, b'p'),
    LongOpt::new("respawn-period", HasArg::Required, b'P'),
    LongOpt::new("retry", HasArg::Required, b'R'),
    LongOpt::new("chroot", HasArg::Required, b'r'),
    LongOpt::new("signal", HasArg::Required, b's'),
    LongOpt::new("start", HasArg::No, b'S'),
    LongOpt::new("user", HasArg::Required, b'u'),
    LongOpt::new("stdin", HasArg::Required, b'0'),
    LongOpt::new("stdout", HasArg::Required, b'1'),
    LongOpt::new("stderr", HasArg::Required, b'2'),
    long("stdout-logger", HasArg::Required, LONGOPT_STDOUT_LOGGER),
    long("stderr-logger", HasArg::Required, LONGOPT_STDERR_LOGGER),
    LongOpt::new("reexec", HasArg::No, b'3'),
    long("notify", HasArg::Required, LONGOPT_NOTIFY),
];
const LONGOPTS_HELP: &[&str] = &[
    "set an initial health check delay",
    "set a health check timer",
    "Set the inheritable, ambient and bounding capabilities",
    "Set the security-bits for the program",
    "Set the No New Privs flag for the program",
    "Set a respawn delay",
    "Increase the respawn delay by this amount for every retry",
    "Set maximum respawn delay when respawn-delay-step is also active",
    "Change the PWD",
    "Set an environment string",
    "Change the process group",
    "Stop the whole process group",
    "Set an ionice class:data when starting",
    "Stop daemon",
    "Set the umask for the daemon",
    "set maximum number of respawn attempts",
    "Set a nicelevel when starting",
    "Set OOM score adjustment when starting",
    "Match pid found in this file",
    "Set respawn time period",
    "Retry schedule to use when stopping",
    "Chroot to this directory",
    "Send a signal to the daemon",
    "Start daemon",
    "Change the process user",
    "Redirect stdin to file",
    "Redirect stdout to file",
    "Redirect stderr to file",
    "Redirect stdout to process",
    "Redirect stderr to process",
    "reexec (used internally)",
    "Configures experimental notification behaviour",
];

// --- State shared with the signal handler -----------------------------------

static DO_HEALTHCHECK: AtomicBool = AtomicBool::new(false);
static EXITING: AtomicBool = AtomicBool::new(false);
static CHILD_PID: AtomicI32 = AtomicI32::new(0);
static SVCNAME: Mutex<Vec<u8>> = Mutex::new(Vec::new());

extern "C" fn handle_signal(sig: i32) {
    use sys::libc;
    let serrno = sys::Errno::last().0;
    match sig {
        libc::SIGALRM => DO_HEALTHCHECK.store(true, SeqCst),
        libc::SIGCHLD => {
            if EXITING.load(SeqCst) {
                while sys::waitpid_raw(-1, libc::WNOHANG).0 > 0 {}
            } else {
                // Reap everything but the daemon, which the main loop waits
                // for.
                loop {
                    let (pid, _) = sys::waitpid_raw(-1, libc::WNOHANG | WNOWAIT);
                    if pid <= 0 || pid == CHILD_PID.load(SeqCst) {
                        break;
                    }
                    sys::waitpid_raw(pid, libc::WNOHANG);
                }
            }
        }
        libc::SIGTERM => EXITING.store(true, SeqCst),
        _ => {
            sys::syslog(libc::LOG_WARNING, format!("caught signal {sig}").as_bytes());
            re_exec_supervisor();
        }
    }
    sys::set_errno(serrno);
}

/// `WNOWAIT` (waitid's flag, which waitpid accepts on Linux).
const WNOWAIT: i32 = 0x0100_0000;

/// `re_exec_supervisor()`.
fn re_exec_supervisor() -> ! {
    let svcname = SVCNAME.lock().map(|g| g.clone()).unwrap_or_default();
    sys::syslog(sys::libc::LOG_WARNING, &bcat!("Re-executing for ", svcname));
    let e = sys::execvp(b"supervise-daemon", &[b"supervise-daemon".to_vec(), svcname, b"--reexec".to_vec()]);
    sys::syslog(sys::libc::LOG_ERR, &bcat!("Unable to execute supervise-daemon: ", e.strerror()));
    stdio::exit(1)
}

/// `make_cmdline()`: the arguments joined with spaces.
fn make_cmdline(argv: &[Vec<u8>]) -> Vec<u8> {
    argv.join(&b' ')
}

/// What the supervisor and its children share (C's file-scope statics).
struct Daemon {
    applet: Vec<u8>,
    svcname: Vec<u8>,
    nicelevel: i32,
    ionicec: i32,
    ioniced: i32,
    oom_score_adj: i32,
    changeuser: Option<Vec<u8>>,
    uid: u32,
    gid: u32,
    ch_root: Option<Vec<u8>>,
    ch_dir: Option<Vec<u8>>,
    stopgroup: bool,
    devnull_fd: i32,
    tty_fd: i32,
    notify: Notify,
    redirect_stdin: Option<Vec<u8>>,
    redirect_stdout: Option<Vec<u8>>,
    redirect_stderr: Option<Vec<u8>>,
    stdout_process: Option<Vec<u8>>,
    stderr_process: Option<Vec<u8>>,
    respawn_count: i32,
    respawn_delay: i64,
    respawn_delay_step: i64,
    respawn_delay_cap: i64,
    respawn_period: i64,
    respawn_max: i32,
    healthcheckdelay: i32,
    healthchecktimer: i32,
    fifopath: Vec<u8>,
    pidfile: Vec<u8>,
    verbose: bool,
    cap_iab: Option<Iab>,
    secbits: u32,
    no_new_privs: bool,
}

fn value_set(svc: &[u8], name: &[u8], value: Option<&[u8]>) {
    let _ = rc::service::rc_service_value_set(svc, name, value);
}

impl Daemon {
    fn fail(&self, msg: &[u8]) -> ! {
        eerrorx(Some(&bcat!(self.applet, ": ", msg)))
    }

    /// `exec_command()`: run the service's script with `cmd`; its pid, 0
    /// if there's no script, -1 if fork failed.
    fn exec_command(&self, cmd: &[u8]) -> i32 {
        use sys::libc;
        let Some(file) = state_resolve(&self.svcname) else { return 0 };
        if sys::access(&file, libc::F_OK).is_err() {
            return 0;
        }
        // Block signals until we have forked.
        let old = sys::block_all_signals();
        let pid = match sys::fork() {
            Ok(0) => {
                sys::default_signals(&[
                    libc::SIGCHLD,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGUSR1,
                    libc::SIGWINCH,
                ]);
                // QUIRK: "unmasking" restores the supervisor's mask, which
                // blocks everything but SIGALRM, SIGCHLD and SIGTERM.
                sys::restore_sigmask(&old);
                let e = sys::execv(&file, &[file.clone(), cmd.to_vec()]);
                sys::syslog(libc::LOG_ERR, &bcat!("unable to exec `", file, "': ", e.strerror(), "\n"));
                sys::exit_immediately(1);
            }
            Ok(pid) => pid,
            Err(e) => {
                sys::syslog(libc::LOG_ERR, &bcat!("fork: ", e.strerror(), "\n"));
                -1
            }
        };
        sys::restore_sigmask(&old);
        pid
    }

    /// `child_process()`: set up and exec the daemon.
    fn child_process(&self, exec: &[u8], argv: &[Vec<u8>]) -> ! {
        use sys::libc;
        sys::setsid();
        let mypid = sys::getpid();
        if !self.svcname.is_empty() {
            let t = sys::strftime_local(sys::time(), b"%Y-%m-%d %H:%M:%S", 20);
            value_set(&self.svcname, b"start_time", Some(&t));
            value_set(&self.svcname, b"start_count", Some(self.respawn_count.to_string().as_bytes()));
            value_set(&self.svcname, b"child_pid", Some(mypid.to_string().as_bytes()));
        }

        if self.nicelevel != i32::MIN {
            let n = self.nicelevel.to_string();
            if let Err(e) = sys::setpriority(mypid, self.nicelevel) {
                self.fail(&bcat!("setpriority ", n.as_bytes(), ": ", e.strerror()));
            }
            match std::fs::OpenOptions::new().read(true).write(true).open("/proc/self/autogroup") {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = f.write_all(format!("{n}\n").as_bytes());
                }
                Err(e) if e.raw_os_error() != Some(libc::ENOENT) => {
                    let e = sys::Errno(e.raw_os_error().unwrap_or(0));
                    self.fail(&bcat!("autogroup nice ", n.as_bytes(), ": ", e.strerror()));
                }
                Err(_) => {}
            }
        }
        if self.ionicec != -1
            && let Err(e) = sys::ioprio_set(1, mypid, self.ionicec | self.ioniced)
        {
            self.fail(&bcat!(
                "ioprio_set ",
                self.ionicec.to_string().as_bytes(),
                " ",
                self.ioniced.to_string().as_bytes(),
                ": ",
                e.strerror()
            ));
        }
        if self.oom_score_adj != i32::MIN {
            let v = self.oom_score_adj.to_string();
            match std::fs::OpenOptions::new().write(true).create(true).truncate(true).open("/proc/self/oom_score_adj") {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = f.write_all(format!("{v}\n").as_bytes());
                }
                Err(e) => {
                    let e = sys::Errno(e.raw_os_error().unwrap_or(0));
                    self.fail(&bcat!("oom_score_adj ", v.as_bytes(), ": ", e.strerror()));
                }
            }
        }

        #[cfg(feature = "pam")]
        let pamh =
            self.changeuser.as_ref().map(|u| crate::shared::pam_session_open(&self.applet, b"supervise-daemon", u));

        let mut groups = Vec::new();
        if let Some(user) = &self.changeuser {
            match sys::getgrouplist(user, self.gid) {
                Some(g) => groups = g,
                None => self.fail(&bcat!("getgrouplist(", user, ", ", self.gid.to_string().as_bytes(), ")")),
            }
        }
        sys::endpwent();
        if let Some(root) = &self.ch_root
            && let Err(e) = sys::chroot(root)
        {
            self.fail(&bcat!("chroot `", root, "': ", e.strerror()));
        }
        if let Some(dir) = &self.ch_dir
            && let Err(e) = sys::chdir(dir)
        {
            self.fail(&bcat!("chdir `", dir, "': ", e.strerror()));
        }
        if self.gid != 0 && sys::setgid(self.gid).is_err() {
            self.fail(&bcat!("unable to set groupid to ", self.gid.to_string().as_bytes()));
        }
        if self.changeuser.is_some() && sys::setgroups(&groups).is_err() {
            self.fail(b"setgroups() failed");
        }
        if self.uid != 0 && caps::setuid_keep_caps(self.uid).is_err() {
            self.fail(&bcat!("unable to set userid to ", self.uid.to_string().as_bytes()));
        }
        if let Some(iab) = &self.cap_iab
            && let Err(e) = iab.set_proc()
        {
            eerrorx(Some(&bcat!("Could not set iab: ", e.strerror())));
        }
        if self.secbits != 0
            && let Err(e) = caps::set_secbits(self.secbits)
        {
            eerrorx(Some(&bcat!(
                "Could not set securebits to 0x",
                format!("{:x}", self.secbits).as_bytes(),
                ": ",
                e.strerror()
            )));
        }
        if self.no_new_privs
            && let Err(e) = sys::set_no_new_privs()
        {
            eerrorx(Some(&bcat!("Could not set No New Privs flag: ", e.strerror())));
        }

        // Remove the controlling tty.
        sys::tiocnotty(self.tty_fd);
        sys::close_raw(self.tty_fd);

        // Clean the environment of any RC_, SSD_ and EINFO_ variables (as
        // they were before PAM's are added).
        let envs = env::environ();
        #[cfg(feature = "pam")]
        if let Some(p) = &pamh {
            crate::shared::pam_import_env(p);
        }
        env::unset("SVCNAME");
        for e in &envs {
            if e.starts_with(b"RC_") || e.starts_with(b"SSD_") || e.starts_with(b"EINFO_") {
                let name = &e[..e.iter().position(|&c| c == b'=').unwrap_or(e.len())];
                env::unset_bytes(name);
            }
        }
        // For the path, remove the rcscript bin dirs from it.
        if let Some(path) = env::get("PATH") {
            let bin = bcat!(rc::paths::LIBEXECDIR, "/bin");
            let sbin = bcat!(rc::paths::LIBEXECDIR, "/sbin");
            let mut newpath: Vec<u8> = Vec::new();
            for token in path.split(|&c| c == b':').filter(|t| !t.is_empty()) {
                if token != &bin[..] && token != &sbin[..] {
                    if !newpath.is_empty() {
                        newpath.push(b':');
                    }
                    newpath.extend_from_slice(token);
                }
            }
            env::unset("PATH");
            env::set("PATH", &newpath);
        }

        let mut stdin_fd = self.devnull_fd;
        let mut stdout_fd = self.devnull_fd;
        let mut stderr_fd = self.devnull_fd;
        if let Some(f) = &self.redirect_stdin {
            match sys::xopen_nonblock(f, libc::O_RDONLY, 0) {
                Ok(fd) => stdin_fd = fd,
                Err(e) => self.fail(&bcat!("unable to redirect the input file for stdin `", f, "': ", e.strerror())),
            }
        }
        let mode = libc::S_IRUSR | libc::S_IWUSR;
        for (redirect, process, fd, name) in [
            (&self.redirect_stdout, &self.stdout_process, &mut stdout_fd, "stdout"),
            (&self.redirect_stderr, &self.stderr_process, &mut stderr_fd, "stderr"),
        ] {
            if let Some(f) = redirect {
                match sys::xopen_nonblock(f, libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND, mode) {
                    Ok(n) => *fd = n,
                    Err(e) => {
                        self.fail(&bcat!("unable to redirect the logfile for ", name, " `", f, "': ", e.strerror()))
                    }
                }
            } else if let Some(cmd) = process {
                *fd = rc_pipe_command(cmd, self.devnull_fd);
                if *fd == -1 {
                    let e = sys::Errno::last();
                    self.fail(&bcat!("unable to open the logging process for ", name, " `", cmd, "': ", e.strerror()));
                }
            }
        }
        let quiet = rc::misc::rc_yesno(env::get("EINFO_QUIET").as_deref());
        sys::dup2(stdin_fd, 0);
        if self.redirect_stdout.is_some() || self.stdout_process.is_some() || quiet {
            sys::dup2(stdout_fd, 1);
        }
        if self.redirect_stderr.is_some() || self.stderr_process.is_some() || quiet {
            sys::dup2(stderr_fd, 2);
        }

        sys::cloexec_fds_from(3);

        if let Notify::Fd { fd, pipe } = self.notify {
            let error = if fd == pipe.1 {
                let flags = sys::fd_flags(fd);
                flags == -1 || !sys::clear_cloexec(fd, flags)
            } else {
                sys::dup2(pipe.1, fd) == -1
            };
            if error {
                self.fail(&bcat!("failed to dup ready fd: ", sys::Errno::last().strerror()));
            }
        }

        sys::syslog(libc::LOG_INFO, &bcat!("Child command line: ", make_cmdline(argv)));
        #[allow(unused_mut)]
        let mut e = sys::execvp(exec, argv);
        #[cfg(feature = "pam")]
        if let Some(p) = &pamh {
            p.close_session(openrc_compat::pam::PAM_SILENT);
            // (C's message shows errno as PAM leaves it.)
            e = sys::Errno::last();
        }
        self.fail(&bcat!("failed to exec `", exec, "': ", e.strerror()))
    }

    /// `supervisor()`: run (and respawn) the daemon until told to stop.
    fn supervisor(&mut self, exec: &[u8], argv: &[Vec<u8>]) -> ! {
        use sys::libc;
        // Block all signals we don't handle.
        let old_signals = sys::block_all_but(&[libc::SIGALRM, libc::SIGCHLD, libc::SIGTERM]);
        for sig in [libc::SIGALRM, libc::SIGCHLD, libc::SIGTERM] {
            sys::signal_setup(sig, handle_signal);
        }

        match std::fs::File::create(openrc_compat::bytes::as_path(&self.pidfile)) {
            Ok(mut f) => {
                use std::io::Write;
                let _ = f.write_all(format!("{}\n", sys::getpid()).as_bytes());
            }
            Err(e) => {
                let e = sys::Errno(e.raw_os_error().unwrap_or(0));
                self.fail(&bcat!("fopen `", self.pidfile, "': ", e.strerror()));
            }
        }
        if !self.svcname.is_empty() {
            let _ = rc::daemon::rc_service_daemon_set(&self.svcname, Some(exec), Some(argv), Some(&self.pidfile), true);
        }

        // Remove the controlling tty.
        sys::tiocnotty(self.tty_fd);
        sys::close_raw(self.tty_fd);

        if self.healthcheckdelay != 0 {
            sys::alarm(self.healthcheckdelay as u32);
        } else if self.healthchecktimer != 0 {
            sys::alarm(self.healthchecktimer as u32);
        }
        let mut failing = false;
        let mut first_spawn: i64 = 0;
        while !EXITING.load(SeqCst) {
            let mut healthcheck_respawn = false;
            // Blocks until someone writes, or a signal interrupts it.
            let fifo_fd = sys::open_raw(&self.fifopath, libc::O_RDONLY, 0).unwrap_or(-1);
            if fifo_fd > 0 {
                let mut buf = [0u8; 2048];
                let count = sys::read_raw(fifo_fd, &mut buf[..2047]);
                sys::close_raw(fifo_fd);
                if count == 0 {
                    continue;
                }
                let got = if count > 0 { &buf[..count as usize] } else { &buf[..0] };
                let got = openrc_compat::bytes::c_str_prefix(got);
                if self.verbose {
                    sys::syslog(libc::LOG_DEBUG, &bcat!("Received ", got, " from fifo"));
                }
                if got.len() >= 3
                    && got[..3].eq_ignore_ascii_case(b"sig")
                    && let Some(sig_send) = scan_word_int(got)
                    && (0..65).contains(&sig_send)
                {
                    let child = CHILD_PID.load(SeqCst);
                    sys::syslog(libc::LOG_INFO, format!("Sending signal {sig_send} to {child}").as_bytes());
                    if sys::kill(child, sig_send).is_err() {
                        sys::syslog(libc::LOG_ERR, format!("Unable to send signal {sig_send} to {child}").as_bytes());
                    }
                }
                continue;
            }
            if DO_HEALTHCHECK.swap(false, SeqCst) {
                sys::alarm(0);
                if self.verbose {
                    sys::syslog(libc::LOG_DEBUG, &bcat!("running health check for ", self.svcname));
                }
                let health_pid = self.exec_command(b"healthcheck");
                let status = exec::rc_waitpid(health_pid);
                if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                    sys::alarm(self.healthchecktimer as u32);
                } else {
                    sys::syslog(libc::LOG_WARNING, &bcat!("health check for ", self.svcname, " failed"));
                    let p = self.exec_command(b"unhealthy");
                    exec::rc_waitpid(p);
                    let child = CHILD_PID.load(SeqCst);
                    sys::syslog(libc::LOG_INFO, &bcat!("stopping ", exec, ", pid ", child.to_string().as_bytes()));
                    let nkilled =
                        run_stop_schedule(&self.applet, None, None, child, 0, self.stopgroup, false, false, true);
                    if nkilled < 0 {
                        let e = sys::Errno::last();
                        sys::syslog(
                            libc::LOG_INFO,
                            &bcat!("Unable to kill ", child.to_string().as_bytes(), ": ", e.strerror()),
                        );
                    } else {
                        healthcheck_respawn = true;
                    }
                }
            }
            if EXITING.load(SeqCst) {
                sys::alarm(0);
                let child = CHILD_PID.load(SeqCst);
                sys::syslog(libc::LOG_INFO, &bcat!("stopping ", exec, ", pid ", child.to_string().as_bytes()));
                let nkilled = run_stop_schedule(&self.applet, None, None, child, 0, self.stopgroup, false, false, true);
                if nkilled > 0 {
                    sys::syslog(libc::LOG_INFO, format!("killed {nkilled} processes").as_bytes());
                }
                continue;
            }
            let child = CHILD_PID.load(SeqCst);
            let (wait_pid, status) = sys::waitpid_raw(child, libc::WNOHANG);
            if wait_pid == child {
                if libc::WIFEXITED(status) {
                    sys::syslog(
                        libc::LOG_WARNING,
                        &bcat!(
                            exec,
                            format!(", pid {child}, exited with return code {}", libc::WEXITSTATUS(status)).as_bytes()
                        ),
                    );
                } else if libc::WIFSIGNALED(status) {
                    sys::syslog(
                        libc::LOG_WARNING,
                        &bcat!(
                            exec,
                            format!(", pid {child}, terminated by signal {}", libc::WTERMSIG(status)).as_bytes()
                        ),
                    );
                }
            }
            if wait_pid == child || healthcheck_respawn {
                DO_HEALTHCHECK.store(false, SeqCst);
                sys::alarm(0);
                let respawn_now = sys::tm_now();
                if first_spawn == 0 {
                    first_spawn = respawn_now;
                }
                if self.respawn_period > 0 && respawn_now - first_spawn > self.respawn_period {
                    self.respawn_count = 0;
                    first_spawn = 0;
                } else {
                    self.respawn_count += 1;
                }
                if self.respawn_max > 0 && self.respawn_count > self.respawn_max {
                    sys::syslog(libc::LOG_WARNING, &bcat!("respawned \"", exec, "\" too many times, exiting"));
                    EXITING.store(true, SeqCst);
                    failing = true;
                    continue;
                }
                let mut sleep_for = self.respawn_delay + self.respawn_delay_step * i64::from(self.respawn_count);
                if self.respawn_delay_step > 0 && sleep_for > self.respawn_delay_cap {
                    sleep_for = self.respawn_delay_cap;
                }
                sys::tm_sleep(sleep_for);
                if EXITING.load(SeqCst) {
                    continue;
                }
                match sys::fork() {
                    Ok(0) => {
                        sys::restore_sigmask(&old_signals);
                        sys::default_signals(&[libc::SIGALRM, libc::SIGCHLD, libc::SIGTERM]);
                        self.child_process(exec, argv);
                    }
                    Ok(pid) => CHILD_PID.store(pid, SeqCst),
                    Err(e) => {
                        sys::syslog(libc::LOG_ERR, &bcat!(self.applet, ": fork: ", e.strerror()));
                        stdio::exit(1);
                    }
                }
                if self.healthcheckdelay != 0 {
                    sys::alarm(self.healthcheckdelay as u32);
                } else if self.healthchecktimer != 0 {
                    sys::alarm(self.healthchecktimer as u32);
                }
            }
        }

        if !self.svcname.is_empty() {
            let _ =
                rc::daemon::rc_service_daemon_set(&self.svcname, Some(exec), Some(argv), Some(&self.pidfile), false);
            value_set(&self.svcname, b"child_pid", None);
            rc::state::rc_service_mark(&self.svcname, rc::state::flags::STOPPED);
            if failing {
                rc::state::rc_service_mark(&self.svcname, rc::state::flags::FAILED);
            }
        }
        let _ = sys::unlink(&self.pidfile);
        let _ = sys::unlink(&self.fifopath);
        stdio::exit(0)
    }
}

/// `rc_service_resolve()`.
fn state_resolve(svc: &[u8]) -> Option<Vec<u8>> {
    rc::state::rc_service_resolve(svc)
}

/// `sscanf(buf, "%s %d", cmd, &n) == 2`: a word, white space, an integer.
fn scan_word_int(s: &[u8]) -> Option<i32> {
    let start = s.iter().position(|&c| !num::is_c_space(c))?;
    let word_end = s[start..].iter().position(|&c| num::is_c_space(c)).map(|i| start + i)?;
    let p = num::strtoimax(&s[word_end..], 10);
    (p.end > 0).then_some(p.value as i32)
}

/// A service value that C would pass straight to `sscanf()` or `strtol()`:
/// a missing one crashes it.
fn value_or_crash(svc: &[u8], name: &[u8]) -> Vec<u8> {
    match rc::service::rc_service_value_get(svc, name) {
        Some(v) => openrc_compat::bytes::c_str_prefix(&v).to_vec(),
        None => super::openrc::crash(),
    }
}

pub fn main(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    use sys::libc;
    let applet = openrc_compat::bytes::basename_c(args.first().map_or(&b""[..], |a| a)).to_vec();
    let Some(svcname) = env::get("RC_SVCNAME") else {
        eerrorx(Some(&bcat!(applet, ": The RC_SVCNAME environment variable is not set")));
    };
    *SVCNAME.lock().unwrap_or_else(|e| e.into_inner()) = svcname.clone();
    sys::openlog(&applet, libc::LOG_PID, libc::LOG_DAEMON);

    if args.len() <= 1 || args[1] != svcname {
        let first = args.get(1).cloned().unwrap_or_else(|| b"(null)".to_vec());
        eerrorx(Some(&bcat!(applet, ": the first argument is ", first, " and must be ", svcname)));
    }

    let mut d = Daemon {
        applet: applet.clone(),
        svcname: svcname.clone(),
        nicelevel: i32::MIN,
        ionicec: -1,
        ioniced: 0,
        oom_score_adj: i32::MIN,
        changeuser: None,
        uid: 0,
        gid: 0,
        ch_root: None,
        ch_dir: None,
        stopgroup: false,
        devnull_fd: -1,
        tty_fd: -1,
        notify: Notify::None,
        redirect_stdin: None,
        redirect_stdout: None,
        redirect_stderr: None,
        stdout_process: None,
        stderr_process: None,
        respawn_count: 0,
        respawn_delay: 0,
        respawn_delay_step: 128,
        respawn_delay_cap: 30_000,
        respawn_period: 0,
        respawn_max: 10,
        healthcheckdelay: 0,
        healthchecktimer: 0,
        fifopath: Vec::new(),
        pidfile: Vec::new(),
        verbose: false,
        cap_iab: None,
        secbits: 0,
        no_new_privs: false,
    };

    if let Some(t) = env::get("SSD_NICELEVEL") {
        match num::scanf_int(&t) {
            Some(n) => d.nicelevel = n,
            None => {
                eerror(Some(&bcat!(applet, ": invalid nice level `", t, "' (SSD_NICELEVEL)")));
            }
        }
    }
    if let Some(t) = env::get("SSD_IONICELEVEL") {
        let (n, c, dd) = scan_d_colon_d(&t);
        if let Some(c) = c {
            d.ionicec = c;
        }
        if let Some(dd) = dd {
            d.ioniced = dd;
        }
        if n != 1 && n != 2 {
            eerror(Some(&bcat!(applet, ": invalid ionice level `", t, "' (SSD_IONICELEVEL)")));
        }
        if d.ionicec == 0 {
            d.ioniced = 0;
        } else if d.ionicec == 3 {
            d.ioniced = 7;
        }
        d.ionicec = d.ionicec.wrapping_shl(13);
    }
    if let Some(t) = env::get("SSD_OOM_SCORE_ADJ") {
        match num::scanf_int(&t) {
            Some(n) => d.oom_score_adj = n,
            None => {
                eerror(Some(&bcat!(applet, ": invalid oom_score_adj `", t, "' (SSD_OOM_SCORE_ADJ)")));
            }
        }
    }

    // Our user name and initial dir.
    let mut home = env::get("HOME");
    if (home.is_none() || env::get("USER").is_none())
        && let Some(pw) = sys::getpwuid(sys::getuid())
    {
        if env::get("USER").is_none() {
            env::set("USER", &pw.name);
        }
        if home.is_none() {
            env::set("HOME", &pw.dir);
            home = Some(pw.dir);
        }
    }

    let cmdline = make_cmdline(&args);
    let usage = Usage {
        applet: applet.clone(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: None,
        usagestring: None,
    };
    let longopts = usage.all_longopts();
    // The service name is argv[0] for getopt.
    let argc = args.len() - 1;
    let mut g = Getopt::new(args[1..].to_vec(), GETOPTSTRING);

    let mut start = false;
    let mut stop = false;
    let mut reexec = false;
    let mut sendsig = false;
    let mut retry: Option<Vec<u8>> = None;
    let mut sig = libc::SIGTERM;
    let mut numask = 0o022;
    let mut pidfile: Option<Vec<u8>> = None;
    let mut respawn_delay_str = b"0".to_vec();
    let mut respawn_period_str = b"12sec".to_vec();
    while let Some(opt) = g.next_long(&longopts) {
        let optarg = g.optarg.clone().unwrap_or_default();
        match opt {
            LONGOPT_CAPABILITIES => match Iab::from_text(&optarg) {
                Some(iab) => d.cap_iab = Some(iab),
                None => eerrorx(Some(&bcat!("Could not parse iab: ", sys::Errno(libc::EINVAL).strerror()))),
            },
            LONGOPT_SECBITS => {
                if optarg.is_empty() {
                    eerrorx(Some(b"Secbits are empty"));
                }
                let p = num::strtoul(&optarg, 0);
                d.secbits = p.value as u32;
                if let Some(&c) = optarg.get(p.end) {
                    eerrorx(Some(&bcat!("Could not parse secbits: invalid char ", [c])));
                }
            }
            LONGOPT_NO_NEW_PRIVS => d.no_new_privs = true,
            LONGOPT_OOM_SCORE_ADJ => match num::scanf_int(&optarg) {
                Some(n) => d.oom_score_adj = n,
                None => eerrorx(Some(&bcat!(applet, ": invalid oom-score-adj `", optarg, "'"))),
            },
            LONGOPT_STDOUT_LOGGER => d.stdout_process = Some(optarg),
            LONGOPT_STDERR_LOGGER => d.stderr_process = Some(optarg),
            LONGOPT_NOTIFY => d.notify = notify_parse(&svcname, &optarg),
            LONGOPT_RESPAWN_DELAY_STEP => match parse_duration(&optarg) {
                Some(v) => d.respawn_delay_step = v,
                None => eerrorx(Some(&bcat!("Invalid respawn-delay-step value '", optarg, "'"))),
            },
            LONGOPT_RESPAWN_DELAY_CAP => match parse_duration(&optarg) {
                Some(v) => d.respawn_delay_cap = v,
                None => eerrorx(Some(&bcat!("Invalid respawn-delay-cap value '", optarg, "'"))),
            },
            _ => match u8::try_from(opt).unwrap_or(0) {
                c @ (b'a' | b'A') => {
                    let v = num::scanf_int(&optarg).filter(|&v| v >= 1);
                    let what = if c == b'a' { "timer" } else { "delay" };
                    match v {
                        Some(v) if c == b'a' => d.healthchecktimer = v,
                        Some(v) => d.healthcheckdelay = v,
                        None => eerrorx(Some(&bcat!(applet, ": invalid health check ", what, " ", optarg))),
                    }
                }
                b'D' => respawn_delay_str = optarg,
                b'I' => {
                    let (n, c, dd) = scan_d_colon_d(&optarg);
                    if let Some(c) = c {
                        d.ionicec = c;
                    }
                    if let Some(dd) = dd {
                        d.ioniced = dd;
                    }
                    if n == 0 {
                        eerrorx(Some(&bcat!(applet, ": invalid ionice `", optarg, "'")));
                    }
                    if d.ionicec == 0 {
                        d.ioniced = 0;
                    } else if d.ionicec == 3 {
                        d.ioniced = 7;
                    }
                    d.ionicec = d.ionicec.wrapping_shl(13);
                }
                b'K' => stop = true,
                b'N' => match num::scanf_int(&optarg) {
                    Some(n) => d.nicelevel = n,
                    None => eerrorx(Some(&bcat!(applet, ": invalid nice level `", optarg, "'"))),
                },
                b'P' => respawn_period_str = optarg,
                b's' => {
                    sig = parse_signal(&applet, Some(&optarg));
                    sendsig = true;
                }
                b'S' => start = true,
                b'd' => d.ch_dir = Some(optarg),
                b'e' => putenv(&optarg),
                b'g' => {
                    let gr = match num::scanf_int(&optarg) {
                        Some(tid) => sys::getgrgid(tid as u32),
                        None => sys::getgrnam(&optarg),
                    };
                    match gr {
                        Some(gid) => d.gid = gid,
                        None => eerrorx(Some(&bcat!(applet, ": group `", optarg, "' not found"))),
                    }
                }
                b'G' => d.stopgroup = true,
                b'k' => match parse_mode(&optarg) {
                    Some(m) => numask = m,
                    None => eerrorx(Some(&bcat!(applet, ": invalid mode `", optarg, "'"))),
                },
                b'm' => match num::scanf_int(&optarg).filter(|&v| v >= 0) {
                    Some(v) => d.respawn_max = v,
                    None => eerrorx(Some(&bcat!("Invalid respawn-max value '", optarg, "'"))),
                },
                b'p' => pidfile = Some(optarg),
                b'R' => retry = Some(optarg),
                b'r' => d.ch_root = Some(optarg),
                b'u' => {
                    let mut parts = optarg.splitn(2, |&b| b == b':');
                    let user = parts.next().unwrap_or_default().to_vec();
                    let group = parts.next().map(<[u8]>::to_vec);
                    d.changeuser = Some(user.clone());
                    let pw = match scan_int_only(&user) {
                        Some(tid) => sys::getpwuid(tid as u32),
                        None => sys::getpwnam(&user),
                    };
                    let Some(pw) = pw else {
                        eerrorx(Some(&bcat!(applet, ": user `", user, "' not found")));
                    };
                    d.uid = pw.uid;
                    home = Some(pw.dir.clone());
                    env::unset("HOME");
                    env::set("HOME", &pw.dir);
                    env::unset("USER");
                    env::set("USER", &pw.name);
                    if d.gid == 0 {
                        d.gid = pw.gid;
                    }
                    if let Some(group) = group {
                        let gr = match scan_int_only(&group) {
                            Some(tid) => sys::getgrgid(tid as u32),
                            None => sys::getgrnam(&group),
                        };
                        match gr {
                            Some(gid) => d.gid = gid,
                            None => eerrorx(Some(&bcat!(applet, ": group `", group, "' not found"))),
                        }
                    }
                }
                b'0' => d.redirect_stdin = Some(optarg),
                b'1' => d.redirect_stdout = Some(optarg),
                b'2' => d.redirect_stderr = Some(optarg),
                b'3' => reexec = true,
                _ => usage.common_option(opt, argc),
            },
        }
    }

    d.verbose = rc::misc::rc_yesno(env::get("EINFO_VERBOSE").as_deref());
    sys::endpwent();
    let argv: Vec<Vec<u8>> = g.args[g.optind..].to_vec();
    let mut exec: Option<Vec<u8>> = argv.first().cloned();

    d.respawn_delay = match parse_duration(&respawn_delay_str) {
        Some(v) => v,
        None => eerrorx(Some(&bcat!("Invalid respawn-delay value '", respawn_delay_str, "'"))),
    };
    d.respawn_period = match parse_duration(&respawn_period_str) {
        Some(v) => v,
        None => eerrorx(Some(&bcat!("Invalid respawn-period value '", respawn_period_str, "'"))),
    };

    // Expand ~.
    d.ch_dir = d.ch_dir.take().map(|p| expand_home(home.as_deref(), &p));
    d.ch_root = d.ch_root.take().map(|p| expand_home(home.as_deref(), &p));

    sys::umask(numask);
    d.pidfile = match pidfile {
        Some(p) => p,
        None => {
            let dir = if rc::dirs::rc_is_user() {
                env::get("XDG_RUNTIME_DIR").unwrap_or_else(|| b"(null)".to_vec())
            } else {
                b"/var/run".to_vec()
            };
            bcat!(dir, "/supervise-", svcname, ".pid")
        }
    };
    d.fifopath = bcat!(rc::svcdir::rc_svcdir(), "/supervise-", svcname, ".ctl");
    if let Err(e) = sys::mkfifo(&d.fifopath, 0o600)
        && e.0 != libc::EEXIST
    {
        eerrorx(Some(&bcat!(applet, ": unable to create control fifo: ", e.strerror())));
    }

    if reexec {
        let child_argc = num::scanf_int(&value_or_crash(&svcname, b"argc")).unwrap_or(0);
        let child_argv: Vec<Vec<u8>> = (0..child_argc.max(0))
            .map(|x| {
                rc::service::rc_service_value_get(&svcname, format!("argv_{x}").as_bytes())
                    .map(|v| openrc_compat::bytes::c_str_prefix(&v).to_vec())
                    .unwrap_or_default()
            })
            .collect();
        CHILD_PID.store(num::scanf_int(&value_or_crash(&svcname, b"child_pid")).unwrap_or(0), SeqCst);
        let exec = rc::service::rc_service_value_get(&svcname, b"exec")
            .map(|v| openrc_compat::bytes::c_str_prefix(&v).to_vec())
            .unwrap_or_default();
        d.pidfile = rc::service::rc_service_value_get(&svcname, b"pidfile")
            .map(|v| openrc_compat::bytes::c_str_prefix(&v).to_vec())
            .unwrap_or_default();
        let retry = rc::service::rc_service_value_get(&svcname, b"retry")
            .map(|v| openrc_compat::bytes::c_str_prefix(&v).to_vec());
        match &retry {
            Some(r) => {
                parse_schedule(&applet, Some(r), sig);
                value_set(&svcname, b"retry", Some(r));
            }
            None => parse_schedule(&applet, None, sig),
        }
        // (A missing respawn_delay crashes strtol; an invalid one is -1.)
        d.respawn_delay = parse_duration(&value_or_crash(&svcname, b"respawn_delay")).unwrap_or(-1);
        if let Some(v) = num::scanf_i64(&value_or_crash(&svcname, b"respawn_delay_step")) {
            d.respawn_delay_step = v;
        }
        if let Some(v) = num::scanf_i64(&value_or_crash(&svcname, b"respawn_delay_cap")) {
            d.respawn_delay_cap = v;
        }
        if let Some(v) = num::scanf_int(&value_or_crash(&svcname, b"respawn_max")) {
            d.respawn_max = v;
        }
        d.supervisor(&exec, &child_argv);
    } else if start {
        let Some(e) = exec.as_mut() else {
            eerrorx(Some(&bcat!(applet, ": nothing to start")));
        };
        *e = expand_home(home.as_deref(), e);
        let e: &Vec<u8> = e;
        // Validate that the binary exists.
        let mut exec_file = None;
        if e.first() == Some(&b'/') || e.first() == Some(&b'.') {
            exec_file = Some(match &d.ch_root {
                Some(r) => bcat!(r, "/", e),
                None => e.clone(),
            });
        } else if let Some(path) = env::get("PATH") {
            for token in path.split(|&c| c == b':') {
                let f = match &d.ch_root {
                    Some(r) => bcat!(r, "/", token, "/", e),
                    None => bcat!(token, "/", e),
                };
                if sys::access(&f, libc::F_OK).is_ok() {
                    exec_file = Some(f);
                    break;
                }
            }
        }
        if !exec_file.as_deref().is_some_and(|f| sys::access(f, libc::F_OK).is_ok()) {
            eerror(Some(&bcat!(applet, ": ", exec_file.unwrap_or_else(|| e.clone()), " does not exist")));
            stdio::exit(1);
        }
        let exec = e.clone();

        let pid = get_pid(&applet, Some(&d.pidfile));
        if pid != -1 && do_stop(&applet, Some(&exec), Some(&argv), pid, d.uid, 0, false, false, true) > 0 {
            eerrorx(Some(&bcat!(applet, ": ", exec, " is already running")));
        }

        if d.respawn_period > 0 && d.respawn_delay.wrapping_mul(i64::from(d.respawn_max)) > d.respawn_period {
            let v = (d.respawn_delay.wrapping_mul(i64::from(d.respawn_max)) + 500) / 1000;
            ewarn(Some(&bcat!(
                applet,
                ": Please decrease the value of --respawn-period to less than ",
                v.to_string().as_bytes(),
                " to avoid infinite respawning"
            )));
        }
        if d.respawn_delay_step > 0 && d.respawn_delay > d.respawn_delay_cap {
            ewarn(Some(&bcat!(
                applet,
                ": Please decrease the value of --respawn-delay (",
                d.respawn_delay.to_string().as_bytes(),
                "ms) to less than --respawn-delay-cap (",
                d.respawn_delay_cap.to_string().as_bytes(),
                "ms)"
            )));
        }

        match &retry {
            Some(r) => {
                parse_schedule(&applet, Some(r), sig);
                value_set(&svcname, b"retry", Some(r));
            }
            None => parse_schedule(&applet, None, sig),
        }

        einfov(Some(&bcat!("Detaching to start `", exec, "'")));
        sys::syslog(libc::LOG_INFO, &bcat!("Supervisor command line: ", cmdline));

        // Remove an existing pidfile, and make sure we can write one.
        let _ = sys::unlink(&d.pidfile);
        if let Err(err) = std::fs::File::create(openrc_compat::bytes::as_path(&d.pidfile)) {
            let err = sys::Errno(err.raw_os_error().unwrap_or(0));
            eerrorx(Some(&bcat!(applet, ": fopen `", d.pidfile, "': ", err.strerror())));
        }

        value_set(&svcname, b"pidfile", Some(&d.pidfile));
        value_set(&svcname, b"respawn_delay", Some(&respawn_delay_str));
        value_set(&svcname, b"respawn_period", Some(&respawn_period_str));
        value_set(&svcname, b"respawn_delay_step", Some(d.respawn_delay_step.to_string().as_bytes()));
        value_set(&svcname, b"respawn_delay_cap", Some(d.respawn_delay_cap.to_string().as_bytes()));
        value_set(&svcname, b"respawn_max", Some(d.respawn_max.to_string().as_bytes()));

        match sys::fork() {
            Err(err) => eerrorx(Some(&bcat!(applet, ": fork: ", err.strerror()))),
            Ok(0) => {}
            Ok(_) => stdio::exit(if notify_wait(&applet, d.notify) { 0 } else { 1 }),
        }

        d.tty_fd = sys::open_raw(b"/dev/tty", libc::O_RDWR, 0).unwrap_or(-1);
        d.devnull_fd = sys::open_raw(b"/dev/null", libc::O_RDWR, 0).unwrap_or(-1);
        sys::dup2(d.devnull_fd, 0);
        sys::dup2(d.devnull_fd, 1);
        sys::dup2(d.devnull_fd, 2);
        if let Notify::Fd { pipe, .. } = d.notify {
            sys::close_raw(pipe.0);
        }

        match sys::fork() {
            Err(err) => eerrorx(Some(&bcat!(applet, ": fork: ", err.strerror()))),
            Ok(0) => d.child_process(&exec, &argv),
            Ok(pid) => {
                CHILD_PID.store(pid, SeqCst);
                if let Notify::Fd { pipe, .. } = d.notify {
                    sys::close_raw(pipe.1);
                }
                for (x, a) in argv.iter().enumerate() {
                    value_set(&svcname, format!("argv_{x}").as_bytes(), Some(a));
                }
                value_set(&svcname, b"argc", Some(argv.len().to_string().as_bytes()));
                value_set(&svcname, b"exec", Some(&exec));
                d.supervisor(&exec, &argv);
            }
        }
    } else if stop {
        let pid = get_pid(&applet, Some(&d.pidfile));
        if pid != -1 {
            if sys::kill(pid, libc::SIGTERM).is_err() {
                // We failed to send the signal.
                ewarn(Some(b"Unable to shut down the supervisor"));
            } else {
                // Wait for the supervisor to go down.
                while sys::kill(pid, 0).is_ok() {
                    sys::nanosleep(1);
                }
            }
        }
        // Even if we have not actually killed anything, remove what we know
        // of it, as it may have crashed: the end result is the same.
        let _ = sys::unlink(&d.pidfile);
        let _ = rc::daemon::rc_service_daemon_set(&svcname, exec.as_deref(), Some(&argv), Some(&d.pidfile), false);
        rc::state::rc_service_mark(&svcname, rc::state::flags::STOPPED);
        stdio::exit(0);
    } else if sendsig {
        let fd = match sys::open_raw(&d.fifopath, libc::O_WRONLY | libc::O_NONBLOCK, 0) {
            Ok(fd) => fd,
            Err(e) => eerrorx(Some(&bcat!(applet, ": unable to open control fifo ", e.strerror()))),
        };
        let msg = format!("sig {sig}");
        if sys::write_raw(fd, msg.as_bytes()) == -1 {
            let e = sys::Errno::last();
            eerrorx(Some(&bcat!(applet, ": error writing to control fifo: ", e.strerror())));
        }
        stdio::exit(0);
    }
    0
}

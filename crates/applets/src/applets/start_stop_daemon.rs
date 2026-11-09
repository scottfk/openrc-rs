//! `start-stop-daemon`: port of upstream's
//! `src/start-stop-daemon/start-stop-daemon.c`.  Starts a daemon (as a
//! user, in a chroot, with redirections, priorities, capabilities...),
//! stops or signals the matching processes along a retry schedule, and
//! records the daemon for its service (`$RC_SVCNAME`).
//!
//! PAM sessions are not ported: the reference build has `-Dpam=false`.

use openrc_compat::caps::{self, Iab};
use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::{bcat, env, exec, num, stdio, sys};
use openrc_einfo::{ebeginv, eerror, eerrorx, eindent, eindentv, einfo, einfon, eoutdent, ewarn};

use crate::schedules::{self, do_stop, parse_schedule, parse_signal, run_stop_schedule};
use crate::shared::{Notify, Usage, get_pid, getoptstring_common, notify_parse, notify_wait, parse_mode};

const LONGOPT_CAPABILITIES: i32 = 0x101;
const LONGOPT_OOM_SCORE_ADJ: i32 = 0x102;
const LONGOPT_NO_NEW_PRIVS: i32 = 0x103;
const LONGOPT_SCHEDULER: i32 = 0x104;
const LONGOPT_SCHEDULER_PRIO: i32 = 0x105;
const LONGOPT_SECBITS: i32 = 0x106;
const LONGOPT_NOTIFY: i32 = 0x107;

const GETOPTSTRING: &str = concat!("I:KN:PR:Sa:bc:d:e:Gg:ik:mn:op:s:tu:r:w:x:0:1:2:3:4:", getoptstring_common!());

const fn long(name: &'static str, has_arg: HasArg, val: i32) -> LongOpt {
    LongOpt { name, has_arg, val }
}
const LONGOPTS: &[LongOpt] = &[
    long("capabilities", HasArg::Required, LONGOPT_CAPABILITIES),
    long("secbits", HasArg::Required, LONGOPT_SECBITS),
    long("no-new-privs", HasArg::No, LONGOPT_NO_NEW_PRIVS),
    LongOpt::new("ionice", HasArg::Required, b'I'),
    LongOpt::new("stop", HasArg::No, b'K'),
    LongOpt::new("nicelevel", HasArg::Required, b'N'),
    long("oom-score-adj", HasArg::Required, LONGOPT_OOM_SCORE_ADJ),
    LongOpt::new("retry", HasArg::Required, b'R'),
    LongOpt::new("start", HasArg::No, b'S'),
    LongOpt::new("startas", HasArg::Required, b'a'),
    LongOpt::new("background", HasArg::No, b'b'),
    LongOpt::new("chuid", HasArg::Required, b'c'),
    LongOpt::new("chdir", HasArg::Required, b'd'),
    LongOpt::new("env", HasArg::Required, b'e'),
    LongOpt::new("umask", HasArg::Required, b'k'),
    LongOpt::new("group", HasArg::Required, b'g'),
    LongOpt::new("stop-group", HasArg::No, b'G'),
    LongOpt::new("interpreted", HasArg::No, b'i'),
    LongOpt::new("make-pidfile", HasArg::No, b'm'),
    LongOpt::new("name", HasArg::Required, b'n'),
    LongOpt::new("oknodo", HasArg::No, b'o'),
    LongOpt::new("pidfile", HasArg::Required, b'p'),
    LongOpt::new("signal", HasArg::Required, b's'),
    LongOpt::new("test", HasArg::No, b't'),
    LongOpt::new("user", HasArg::Required, b'u'),
    LongOpt::new("chroot", HasArg::Required, b'r'),
    LongOpt::new("wait", HasArg::Required, b'w'),
    LongOpt::new("exec", HasArg::Required, b'x'),
    LongOpt::new("stdin", HasArg::Required, b'0'),
    LongOpt::new("stdout", HasArg::Required, b'1'),
    LongOpt::new("stderr", HasArg::Required, b'2'),
    LongOpt::new("stdout-logger", HasArg::Required, b'3'),
    LongOpt::new("stderr-logger", HasArg::Required, b'4'),
    LongOpt::new("progress", HasArg::No, b'P'),
    long("scheduler", HasArg::Required, LONGOPT_SCHEDULER),
    long("scheduler-priority", HasArg::Required, LONGOPT_SCHEDULER_PRIO),
    long("notify", HasArg::Required, LONGOPT_NOTIFY),
];
const LONGOPTS_HELP: &[&str] = &[
    "Set the inheritable, ambient and bounding capabilities",
    "Set the security-bits for the program",
    "Set the No New Privs flag for the program",
    "Set an ionice class:data when starting",
    "Stop daemon",
    "Set a nicelevel when starting",
    "Set OOM score adjustment when starting",
    "Retry schedule to use when stopping",
    "Start daemon",
    "deprecated, use --exec or --name",
    "Force daemon to background",
    "deprecated, use --user",
    "Change the PWD",
    "Set an environment string",
    "Set the umask for the daemon",
    "Change the process group",
    "Stop the whole process group",
    "Match process name by interpreter",
    "Create a pidfile",
    "Match process name",
    "deprecated",
    "Match pid found in this file",
    "Send a different signal",
    "Test actions, don't do them",
    "Change the process user",
    "Chroot to this directory",
    "Milliseconds to wait for daemon start",
    "Binary to start/stop",
    "Redirect stdin to file",
    "Redirect stdout to file",
    "Redirect stderr to file",
    "Redirect stdout to process",
    "Redirect stderr to process",
    "Print dots each second while waiting",
    "Set process scheduler",
    "Set process scheduler priority",
    "Configures experimental notification behaviour",
];

const ONE_MS: i64 = 1_000_000;

static APPLET: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

fn applet() -> Vec<u8> {
    APPLET.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

extern "C" fn handle_signal(sig: i32) {
    use sys::libc;
    let serrno = sys::Errno::last().0;
    match sig {
        libc::SIGINT | libc::SIGTERM | libc::SIGQUIT => {
            let name = match sig {
                libc::SIGINT => "SIGINT",
                libc::SIGTERM => "SIGTERM",
                _ => "SIGQUIT",
            };
            eerrorx(Some(&bcat!(applet(), ": caught ", name, ", aborting")));
        }
        libc::SIGCHLD => {
            // (Spins while a child is still running: waitpid returns 0.)
            loop {
                let (r, _) = sys::waitpid_raw(-1, libc::WNOHANG);
                if r < 0 {
                    let e = sys::Errno::last();
                    if e.0 != libc::ECHILD {
                        eerror(Some(&bcat!(applet(), ": waitpid: ", e.strerror())));
                    }
                    break;
                }
            }
        }
        _ => {
            eerror(Some(&bcat!(applet(), ": caught unknown signal ", sig.to_string().as_bytes())));
        }
    }
    sys::set_errno(serrno);
}

/// `expand_home()`: `~` or `~user` at the start of `path`.
fn expand_home(home: Option<&[u8]>, path: &[u8]) -> Vec<u8> {
    if path.first() != Some(&b'~') {
        return path.to_vec();
    }
    let (home, rest): (Option<Vec<u8>>, Option<&[u8]>) = if path.len() > 1 && path[1] != b'/' {
        let end = path[1..].iter().position(|&c| c == b'/').map(|i| i + 1);
        let user = &path[1..end.unwrap_or(path.len())];
        match sys::getpwnam(user) {
            Some(pw) => (Some(pw.dir), end.map(|e| &path[e..])),
            None => (None, None),
        }
    } else {
        (home.map(<[u8]>::to_vec), Some(&path[1..]))
    };
    match (home, rest) {
        (None, _) => path.to_vec(),
        (Some(h), None) => h,
        (Some(h), Some(r)) => bcat!(h, r),
    }
}

/// `sscanf(s, "%d:%d", &a, &b)`: the count (EOF, -1, when `s` has nothing
/// but white space) and the values matched.
fn scan_d_colon_d(s: &[u8]) -> (i32, Option<i32>, Option<i32>) {
    if s.iter().all(|&c| num::is_c_space(c)) {
        return (-1, None, None);
    }
    let a = num::strtoimax(s, 10);
    if a.end == 0 {
        return (0, None, None);
    }
    let rest = &s[a.end..];
    let b = match rest.strip_prefix(b":") {
        Some(r) => {
            let b = num::strtoimax(r, 10);
            (b.end > 0).then_some(b.value as i32)
        }
        None => None,
    };
    (if b.is_some() { 2 } else { 1 }, Some(a.value as i32), b)
}

/// `sscanf(s, "%d%1s", &n, buf) == 1`: an integer and nothing but white
/// space after it.
fn scan_int_only(s: &[u8]) -> Option<i32> {
    let p = num::strtoimax(s, 10);
    (p.end > 0 && s[p.end..].iter().all(|&c| num::is_c_space(c))).then_some(p.value as i32)
}

/// `sscanf(s, "%u", &n)`.
fn scan_u32(s: &[u8]) -> Option<u32> {
    let p = num::strtoul(s, 10);
    (p.end > 0).then_some(p.value as u32)
}

/// `putenv(s)`: glibc removes the variable when there's no `=`.
fn putenv(s: &[u8]) {
    match s.iter().position(|&c| c == b'=') {
        Some(i) => {
            env::set_bytes(&s[..i], &s[i + 1..]);
        }
        None => {
            env::unset_bytes(s);
        }
    }
}

/// `rc_pipe_command()` (rc_exec.c): run `cmd` with `sh -c`, its stdin a
/// pipe (whose write end is returned) and its output to `devnull`.
fn rc_pipe_command(cmd: &[u8], devnull: i32) -> i32 {
    let mut args = exec::ExecArgs::new([&b"/bin/sh"[..], b"-c", cmd]);
    args.stdin = exec::Redirect::Pipe;
    args.stdout = exec::Redirect::Fd(devnull);
    args.stderr = exec::Redirect::Fd(devnull);
    match exec::do_exec(&args) {
        Ok(r) if r.pid > 0 => r.stdin.map_or(-1, std::os::fd::IntoRawFd::into_raw_fd),
        _ => -1,
    }
}

/// Everything the child needs to set up and exec the daemon.
struct Child<'a> {
    applet: &'a [u8],
    exec: &'a [u8],
    argv: &'a [Vec<u8>],
    numask: u32,
    nicelevel: i32,
    ionicec: i32,
    ioniced: i32,
    oom_score_adj: i32,
    changeuser: Option<Vec<u8>>,
    uid: u32,
    gid: u32,
    ch_root: Option<Vec<u8>>,
    ch_dir: Option<Vec<u8>>,
    makepidfile: bool,
    pidfile: Option<Vec<u8>>,
    cap_iab: Option<Iab>,
    secbits: u32,
    no_new_privs: bool,
    background: bool,
    redirect_stdin: Option<Vec<u8>>,
    redirect_stdout: Option<Vec<u8>>,
    redirect_stderr: Option<Vec<u8>>,
    stdout_process: Option<Vec<u8>>,
    stderr_process: Option<Vec<u8>>,
    notify: Notify,
    scheduler: Option<Vec<u8>>,
    sched_prio: i32,
    /// The index in `argv` of the environment's own `PATH=` string, when
    /// argv has run on into the environment (see main).
    argv_path: Option<usize>,
}

impl Child<'_> {
    fn fail(&self, msg: &[u8]) -> ! {
        eerrorx(Some(&bcat!(self.applet, ": ", msg)))
    }

    /// The child: set everything up and exec the daemon.
    fn run(&self, pipe_r: i32) -> ! {
        use sys::libc;
        let mut argv = self.argv.to_vec();
        let mypid = sys::getpid();
        sys::close_raw(pipe_r);
        sys::umask(self.numask);
        let tty_fd = sys::open_raw(b"/dev/tty", libc::O_RDWR, 0).unwrap_or(-1);
        let devnull_fd = sys::open_raw(b"/dev/null", libc::O_RDWR, 0).unwrap_or(-1);
        // setsid() before the autogroup nice level, after opening the tty.
        sys::setsid();

        if self.nicelevel != i32::MIN {
            let n = self.nicelevel.to_string();
            if let Err(e) = sys::setpriority(mypid, self.nicelevel) {
                self.fail(&bcat!("setpriority ", n.as_bytes(), ": ", e.strerror()));
            }
            // "r+" so it isn't created.
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
                    // (C ignores errors writing it.)
                    let _ = f.write_all(format!("{v}\n").as_bytes());
                }
                Err(e) => {
                    let e = sys::Errno(e.raw_os_error().unwrap_or(0));
                    self.fail(&bcat!("oom_score_adj ", v.as_bytes(), ": ", e.strerror()));
                }
            }
        }

        let mut groups = Vec::new();
        if let Some(user) = &self.changeuser {
            match sys::getgrouplist(user, self.gid) {
                Some(g) => groups = g,
                None => self.fail(&bcat!("getgrouplist(", user, ", ", self.gid.to_string().as_bytes(), ")")),
            }
        }
        // Close any fds to the passwd database.
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
        if self.makepidfile
            && let Some(pidfile) = &self.pidfile
        {
            match std::fs::File::create(openrc_compat::bytes::as_path(pidfile)) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = f.write_all(format!("{mypid}\n").as_bytes());
                }
                Err(e) => {
                    let e = sys::Errno(e.raw_os_error().unwrap_or(0));
                    self.fail(&bcat!("fopen `", pidfile, "': ", e.strerror()));
                }
            }
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

        sys::tiocnotty(tty_fd);
        sys::close_raw(tty_fd);

        // Clean the environment of any RC_, SSD_ and EINFO_ variables.
        let envs = env::environ();
        env::unset("SVCNAME");
        for e in &envs {
            if e.starts_with(b"RC_") || e.starts_with(b"SSD_") || e.starts_with(b"EINFO_") {
                let name = &e[..e.iter().position(|&c| c == b'=').unwrap_or(e.len())];
                env::unset_bytes(name);
            }
        }

        // For the path, remove the rcscript bin dirs from it (and empty
        // entries).
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
            // QUIRK: C cuts PATH up in place, writing NULs over the colons
            // of the environment's own string, which a runaway argv shows.
            if let Some(i) = self.argv_path {
                let a = &mut argv[i];
                if let Some(c) = a.iter().position(|&b| b == b':') {
                    a.truncate(c);
                }
            }
        }

        let mut stdin_fd = devnull_fd;
        let mut stdout_fd = devnull_fd;
        let mut stderr_fd = devnull_fd;
        let mode = libc::S_IRUSR | libc::S_IWUSR | libc::S_IRGRP | libc::S_IWGRP;
        if let Some(f) = &self.redirect_stdin {
            match sys::xopen_nonblock(f, libc::O_RDONLY, 0) {
                Ok(fd) => stdin_fd = fd,
                Err(e) => self.fail(&bcat!("unable to redirect the input file for stdin `", f, "': ", e.strerror())),
            }
        }
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
                *fd = rc_pipe_command(cmd, devnull_fd);
                if *fd == -1 {
                    let e = sys::Errno::last();
                    self.fail(&bcat!("unable to open the logging process for ", name, " `", cmd, "': ", e.strerror()));
                }
            }
        }

        let quiet = rc::misc::rc_yesno(env::get("EINFO_QUIET").as_deref());
        if self.background {
            sys::dup2(stdin_fd, 0);
        }
        if self.background || self.redirect_stdout.is_some() || self.stdout_process.is_some() || quiet {
            sys::dup2(stdout_fd, 1);
        }
        if self.background || self.redirect_stderr.is_some() || self.stderr_process.is_some() || quiet {
            sys::dup2(stderr_fd, 2);
        }

        sys::cloexec_fds_from(3);

        if let Notify::Fd { fd, pipe } = self.notify {
            if sys::close_raw(pipe.0) == -1 {
                self.fail(&bcat!("failed to close notify pipe[0]: ", sys::Errno::last().strerror()));
            }
            if sys::dup2(pipe.1, fd) == -1 {
                self.fail(&bcat!("failed to initialize notify fd: ", sys::Errno::last().strerror()));
            }
            // dup2 onto itself doesn't clear FD_CLOEXEC.
            if pipe.1 == fd {
                let flags = sys::fd_flags(fd);
                if flags == -1 {
                    self.fail(&bcat!("failed to get flags for notify fd: ", sys::Errno::last().strerror()));
                }
                if !sys::clear_cloexec(fd, flags) {
                    self.fail(&bcat!("failed to set flags for notify fd: ", sys::Errno::last().strerror()));
                }
            }
        }

        if let Some(scheduler) = &self.scheduler {
            let index = match &scheduler[..] {
                b"fifo" => libc::SCHED_FIFO,
                b"rr" => libc::SCHED_RR,
                b"other" => libc::SCHED_OTHER,
                b"batch" => libc::SCHED_BATCH,
                b"idle" => libc::SCHED_IDLE,
                s => match num::scanf_int(s) {
                    Some(n) => n,
                    None => eerrorx(Some(&bcat!("Unknown scheduler: ", s))),
                },
            };
            let prio = if self.sched_prio == -1 { sys::sched_get_priority_min(index) } else { self.sched_prio };
            if sys::set_scheduler(index, prio).is_err() {
                // QUIRK: pthread_setschedparam returns its error rather
                // than setting errno, so C reports whatever errno held.
                eerrorx(Some(&bcat!("Failed to set scheduler: ", sys::Errno::last().strerror())));
            }
        } else if self.sched_prio != -1
            && let Err(e) = sys::sched_setparam(mypid, self.sched_prio)
        {
            eerrorx(Some(&bcat!("Failed to set scheduler parameters: ", e.strerror())));
        }

        let e = sys::execvp(self.exec, &argv);
        self.fail(&bcat!("failed to exec `", self.exec, "': ", e.strerror()))
    }
}

pub fn main(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    use sys::libc;
    // The environment as it was at exec, for a QUIRK below.
    let initial_env = env::environ();
    let applet = openrc_compat::bytes::basename_c(args.first().map_or(&b""[..], |a| a)).to_vec();
    *APPLET.lock().unwrap_or_else(|e| e.into_inner()) = applet.clone();

    for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM] {
        sys::signal_setup(sig, handle_signal);
    }
    sys::openlog(&applet, libc::LOG_PID, libc::LOG_DAEMON);

    let mut nicelevel = i32::MIN;
    let mut ionicec = -1;
    let mut ioniced = 0;
    let mut oom_score_adj = i32::MIN;
    if let Some(t) = env::get("SSD_NICELEVEL") {
        match num::scanf_int(&t) {
            Some(n) => nicelevel = n,
            None => {
                eerror(Some(&bcat!(applet, ": invalid nice level `", t, "' (SSD_NICELEVEL)")));
            }
        }
    }
    if let Some(t) = env::get("SSD_IONICELEVEL") {
        let (n, c, d) = scan_d_colon_d(&t);
        if let Some(c) = c {
            ionicec = c;
        }
        if let Some(d) = d {
            ioniced = d;
        }
        if n != 1 && n != 2 {
            eerror(Some(&bcat!(applet, ": invalid ionice level `", t, "' (SSD_IONICELEVEL)")));
        }
        if ionicec == 0 {
            ioniced = 0;
        } else if ionicec == 3 {
            ioniced = 7;
        }
        // QUIRK: shifted even when unparsed, so ionice is then set to -8192.
        ionicec = ionicec.wrapping_shl(13);
    }
    if let Some(t) = env::get("SSD_OOM_SCORE_ADJ") {
        match num::scanf_int(&t) {
            Some(n) => oom_score_adj = n,
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

    let usage = Usage {
        applet: applet.clone(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: None,
        usagestring: None,
    };
    let longopts = usage.all_longopts();
    let argc0 = args.len();
    let svcname = env::get("RC_SVCNAME");

    let mut start = false;
    let mut stop = false;
    let mut oknodo = false;
    let mut test = false;
    let mut exec: Option<Vec<u8>> = None;
    let mut startas: Option<Vec<u8>> = None;
    let mut name: Option<Vec<u8>> = None;
    let mut pidfile: Option<Vec<u8>> = None;
    let mut retry: Option<Vec<u8>> = None;
    let mut sig = -1;
    let mut background = false;
    let mut makepidfile = false;
    let mut interpreted = false;
    let mut progress = false;
    let mut uid = 0u32;
    let mut gid = 0u32;
    let mut changeuser: Option<Vec<u8>> = None;
    let mut ch_root: Option<Vec<u8>> = None;
    let mut ch_dir: Option<Vec<u8>> = None;
    let mut redirect_stdin = None;
    let mut redirect_stdout = None;
    let mut redirect_stderr = None;
    let mut stdout_process = None;
    let mut stderr_process = None;
    let mut stopgroup = false;
    let mut numask = 0o022;
    let mut start_wait = 0u32;
    let mut scheduler = None;
    let mut sched_prio = -1;
    let mut cap_iab: Option<Iab> = None;
    let mut secbits = 0u32;
    let mut no_new_privs = false;
    let mut notify = Notify::None;
    // Whether `-e` set or unset PATH (so getenv no longer returns the
    // environment's own string).
    let mut path_replaced = false;

    let mut g = Getopt::new(args.clone(), GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        let optarg = g.optarg.clone().unwrap_or_default();
        match opt {
            LONGOPT_CAPABILITIES => match Iab::from_text(&optarg) {
                Some(iab) => cap_iab = Some(iab),
                None => eerrorx(Some(&bcat!("Could not parse iab: ", sys::Errno(libc::EINVAL).strerror()))),
            },
            LONGOPT_SECBITS => {
                if optarg.is_empty() {
                    eerrorx(Some(b"Secbits are empty"));
                }
                let p = num::strtoul(&optarg, 0);
                secbits = p.value as u32;
                if let Some(&c) = optarg.get(p.end) {
                    eerrorx(Some(&bcat!("Could not parse secbits: invalid char ", [c])));
                }
            }
            LONGOPT_NO_NEW_PRIVS => no_new_privs = true,
            LONGOPT_OOM_SCORE_ADJ => match num::scanf_int(&optarg) {
                Some(n) => oom_score_adj = n,
                None => eerrorx(Some(&bcat!(applet, ": invalid oom-score-adj `", optarg, "'"))),
            },
            LONGOPT_SCHEDULER => scheduler = Some(optarg),
            LONGOPT_SCHEDULER_PRIO => {
                if let Some(n) = num::scanf_int(&optarg) {
                    sched_prio = n;
                }
            }
            LONGOPT_NOTIFY => notify = notify_parse(svcname.as_deref().unwrap_or(&applet), &optarg),
            _ => match u8::try_from(opt).unwrap_or(0) {
                b'I' => {
                    let (n, c, d) = scan_d_colon_d(&optarg);
                    if let Some(c) = c {
                        ionicec = c;
                    }
                    if let Some(d) = d {
                        ioniced = d;
                    }
                    // (Only no match at all is an error: an empty value
                    // is EOF, -1, and passes.)
                    if n == 0 {
                        eerrorx(Some(&bcat!(applet, ": invalid ionice `", optarg, "'")));
                    }
                    if ionicec == 0 {
                        ioniced = 0;
                    } else if ionicec == 3 {
                        ioniced = 7;
                    }
                    ionicec = ionicec.wrapping_shl(13);
                }
                b'K' => stop = true,
                b'G' => stopgroup = true,
                b'N' => match num::scanf_int(&optarg) {
                    Some(n) => nicelevel = n,
                    None => eerrorx(Some(&bcat!(applet, ": invalid nice level `", optarg, "'"))),
                },
                b'P' => progress = true,
                b'R' => retry = Some(optarg),
                b'S' => start = true,
                b'b' => background = true,
                c @ (b'c' | b'u') => {
                    if c == b'c' {
                        ewarn(Some(
                            b"WARNING: -c/--chuid is deprecated and will be removed in the future, please use -u/--user instead",
                        ));
                    }
                    let mut parts = optarg.splitn(2, |&b| b == b':');
                    let user = parts.next().unwrap_or_default().to_vec();
                    let group = parts.next().map(<[u8]>::to_vec);
                    changeuser = Some(user.clone());
                    let pw = match scan_int_only(&user) {
                        Some(tid) => sys::getpwuid(tid as u32),
                        None => sys::getpwnam(&user),
                    };
                    let Some(pw) = pw else {
                        eerrorx(Some(&bcat!(applet, ": user `", user, "' not found")));
                    };
                    uid = pw.uid;
                    home = Some(pw.dir.clone());
                    env::unset("HOME");
                    env::set("HOME", &pw.dir);
                    env::unset("USER");
                    env::set("USER", &pw.name);
                    if gid == 0 {
                        gid = pw.gid;
                    }
                    if let Some(group) = group {
                        let gr = match scan_int_only(&group) {
                            Some(tid) => sys::getgrgid(tid as u32),
                            None => sys::getgrnam(&group),
                        };
                        match gr {
                            Some(g) => gid = g,
                            None => eerrorx(Some(&bcat!(applet, ": group `", group, "' not found"))),
                        }
                    }
                }
                b'd' => ch_dir = Some(optarg),
                b'e' => {
                    if optarg == b"PATH" || optarg.starts_with(b"PATH=") {
                        path_replaced = true;
                    }
                    putenv(&optarg);
                }
                b'g' => {
                    let gr = match num::scanf_int(&optarg) {
                        Some(tid) => sys::getgrgid(tid as u32),
                        None => sys::getgrnam(&optarg),
                    };
                    match gr {
                        Some(g) => gid = g,
                        None => eerrorx(Some(&bcat!(applet, ": group `", optarg, "' not found"))),
                    }
                }
                b'i' => interpreted = true,
                b'k' => match parse_mode(&optarg) {
                    Some(m) => numask = m,
                    None => eerrorx(Some(&bcat!(applet, ": invalid mode `", optarg, "'"))),
                },
                b'm' => makepidfile = true,
                b'n' => name = Some(optarg),
                b'o' => {
                    ewarn(Some(b"WARNING: -o/--oknodo is deprecated and will be removed in the future"));
                    oknodo = true;
                }
                b'p' => pidfile = Some(optarg),
                b's' => sig = parse_signal(&applet, Some(&optarg)),
                b't' => test = true,
                b'r' => ch_root = Some(optarg),
                b'a' => {
                    ewarn(Some(
                        b"WARNING: -a/--startas is deprecated and will be removed in the future, please use -x/--exec or -n/--name instead",
                    ));
                    startas = Some(optarg);
                }
                b'w' => match scan_u32(&optarg) {
                    Some(n) => start_wait = n,
                    None => eerrorx(Some(&bcat!(applet, ": `", optarg, "' not a number"))),
                },
                b'x' => exec = Some(optarg),
                b'0' => redirect_stdin = Some(optarg),
                b'1' => redirect_stdout = Some(optarg),
                b'2' => redirect_stderr = Some(optarg),
                b'3' => stdout_process = Some(optarg),
                b'4' => stderr_process = Some(optarg),
                _ => usage.common_option(opt, argc0),
            },
        }
    }
    sys::endpwent();
    let mut argv: Vec<Vec<u8>> = g.args[g.optind..].to_vec();
    let mut argc = argv.len();

    // Allow `start-stop-daemon --signal HUP --exec /usr/sbin/dnsmasq`
    // without --stop --oknodo as well.
    if !start && !stop && ![libc::SIGINT, libc::SIGTERM, libc::SIGQUIT, libc::SIGKILL].contains(&sig) {
        oknodo = true;
    }

    if exec.is_none() {
        exec = startas.clone();
    } else if name.is_none() {
        name = startas.clone();
    }
    let mut argv_path = None;
    if exec.is_none() {
        exec = argv.first().cloned().or_else(|| name.clone());
        if let Some(n) = &name
            && start
        {
            if argv.is_empty() {
                // QUIRK: `*argv = name` overwrites argv's NULL terminator,
                // so the arguments run on into the environment.
                argv.push(n.clone());
                if !path_replaced {
                    argv_path = initial_env.iter().position(|e| e.starts_with(b"PATH=")).map(|i| i + 1);
                }
                argv.extend(initial_env.iter().cloned());
            } else {
                argv[0] = n.clone();
            }
        }
    } else if let Some(n) = &name {
        argv.insert(0, n.clone());
        argc += 1;
    } else if let Some(e) = &exec {
        argv.insert(0, e.clone());
        argc += 1;
    }

    if stop || sig != -1 {
        if sig == -1 {
            sig = libc::SIGTERM;
        }
        if argv.is_empty() && pidfile.is_none() && name.is_none() && uid == 0 {
            eerrorx(Some(&bcat!(applet, ": --stop needs --exec, --pidfile, --name or --user")));
        }
        if background {
            eerrorx(Some(&bcat!(applet, ": --background is only relevant with --start")));
        }
        if makepidfile {
            eerrorx(Some(&bcat!(applet, ": --make-pidfile is only relevant with --start")));
        }
        if redirect_stdout.is_some() || redirect_stderr.is_some() {
            eerrorx(Some(&bcat!(applet, ": --stdout and --stderr are only relevant with --start")));
        }
        if stdout_process.is_some() || stderr_process.is_some() {
            eerrorx(Some(&bcat!(applet, ": --stdout-logger and --stderr-logger are only relevant with --start")));
        }
        if start_wait != 0 {
            ewarn(Some(b"using --wait with --stop has no effect, use --retry instead"));
        }
    } else {
        if exec.is_none() {
            eerrorx(Some(&bcat!(applet, ": nothing to start")));
        }
        if makepidfile && pidfile.is_none() {
            eerrorx(Some(&bcat!(applet, ": --make-pidfile is only relevant with --pidfile")));
        }
        if (redirect_stdout.is_some() || redirect_stderr.is_some()) && !background {
            eerrorx(Some(&bcat!(applet, ": --stdout and --stderr are only relevant with --background")));
        }
        if (stdout_process.is_some() || stderr_process.is_some()) && !background {
            eerrorx(Some(&bcat!(applet, ": --stdout-logger and --stderr-logger are only relevant with --background")));
        }
        if redirect_stdout.is_some() && stdout_process.is_some() {
            eerrorx(Some(&bcat!(applet, ": do not use --stdout and --stdout-logger together")));
        }
        if redirect_stderr.is_some() && stderr_process.is_some() {
            eerrorx(Some(&bcat!(applet, ": do not use --stderr and --stderr-logger together")));
        }
    }

    // Expand ~.
    let home_b = home.clone();
    ch_dir = ch_dir.map(|d| expand_home(home_b.as_deref(), &d));
    ch_root = ch_root.map(|d| expand_home(home_b.as_deref(), &d));
    let mut exec_file: Option<Vec<u8>> = None;
    if let Some(e) = &mut exec {
        *e = expand_home(home_b.as_deref(), e);
        let e: &Vec<u8> = e;
        // Validate that the binary exists if we are starting.
        if e.first() == Some(&b'/') || e.first() == Some(&b'.') {
            exec_file = Some(match &ch_root {
                Some(r) => bcat!(r, "/", e),
                None => e.clone(),
            });
        } else if let Some(path) = env::get("PATH") {
            for token in path.split(|&c| c == b':') {
                let f = match &ch_root {
                    Some(r) => bcat!(r, "/", token, "/", e),
                    None => bcat!(token, "/", e),
                };
                if sys::access(&f, libc::F_OK).is_ok() {
                    exec_file = Some(f);
                    break;
                }
            }
        }
    }
    if start && !exec_file.as_deref().is_some_and(|f| sys::access(f, libc::F_OK).is_ok()) {
        let shown = exec_file.clone().or_else(|| exec.clone()).unwrap_or_else(|| b"(null)".to_vec());
        eerror(Some(&bcat!(applet, ": ", shown, " does not exist")));
        stdio::exit(1);
    }
    if start && retry.is_some() {
        ewarn(Some(b"using --retry with --start has no effect, use --wait instead"));
    }

    // Without a pidfile, an interpreted daemon is matched by its
    // interpreter.
    let mut nav: Option<Vec<Vec<u8>>> = None;
    if interpreted
        && pidfile.is_none()
        && let Some(f) = &exec_file
        && let Ok(file) = std::fs::File::open(openrc_compat::bytes::as_path(f))
    {
        use std::io::BufRead;
        let mut line = Vec::new();
        match std::io::BufReader::new(file).read_until(b'\n', &mut line) {
            Ok(n) if n > 0 => {}
            r => {
                let e = match r {
                    Err(e) => sys::Errno(e.raw_os_error().unwrap_or(0)),
                    Ok(_) => sys::Errno(0),
                };
                eerrorx(Some(&bcat!(applet, ": ", e.strerror())));
            }
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        let line = openrc_compat::bytes::c_str_prefix(&line).to_vec();
        if line.starts_with(b"#!") {
            let mut p = &line[2..];
            while let Some(rest) = p.strip_prefix(b" ").or_else(|| p.strip_prefix(b"\t")) {
                p = rest;
            }
            let (token, rest) = match p.iter().position(|&c| c == b' ') {
                Some(i) => (&p[..i], Some(&p[i + 1..])),
                None => (p, None),
            };
            // (C also points exec_file at the interpreter, unused after.)
            let mut v = vec![token.to_vec()];
            if let Some(r) = rest {
                v.push(r.to_vec());
            }
            v.extend(argv.iter().cloned());
            nav = Some(v);
        }
    }
    let margv = nav.clone().unwrap_or_else(|| argv.clone());

    if stop || sig != -1 {
        if !stop {
            oknodo = true;
        }
        if let Some(r) = &retry {
            parse_schedule(&applet, Some(r), sig);
        } else if test || oknodo {
            parse_schedule(&applet, Some(b"0"), sig);
        } else {
            parse_schedule(&applet, None, sig);
        }
        let pid = if pidfile.is_some() {
            let pid = get_pid(&applet, pidfile.as_deref());
            if pid == -1 && sys::Errno::last().0 != libc::ENOENT {
                stdio::exit(1);
            }
            pid
        } else {
            0
        };
        let i = run_stop_schedule(&applet, exec.as_deref(), Some(&margv), pid, uid, stopgroup, test, progress, false);
        if i < 0 {
            // We failed to stop something.
            stdio::exit(1);
        }
        if test || oknodo {
            return if i > 0 { 0 } else { 1 };
        }
        // Even if we have not actually killed anything, remove what we
        // know of it, as it may have crashed: the end result is the same.
        if let Some(p) = &pidfile {
            let _ = sys::unlink(p);
        }
        if let Some(s) = &svcname {
            let _ = rc::daemon::rc_service_daemon_set(s, exec.as_deref(), Some(&argv), pidfile.as_deref(), false);
        }
        stdio::exit(0);
    }

    let pid = if pidfile.is_some() { get_pid(&applet, pidfile.as_deref()) } else { 0 };
    let pids = if pid != 0 {
        rc::daemon::rc_find_pids(None, None, 0, pid)
    } else {
        rc::daemon::rc_find_pids(exec.as_deref(), Some(&argv), uid, 0)
    };
    let exec = exec.unwrap_or_default();
    if pids.is_some() {
        eerrorx(Some(&bcat!(applet, ": ", exec, " is already running")));
    }

    if test {
        if rc::misc::rc_yesno(env::get("EINFO_QUIET").as_deref()) {
            stdio::exit(0);
        }
        einfon(Some(b"Would start"));
        for a in argv.iter().take(argc) {
            stdio::out(&bcat!(" ", a));
        }
        stdio::out(b"\n");
        eindent();
        if uid != 0 {
            einfo(Some(&bcat!("as user id ", uid.to_string().as_bytes())));
        }
        if gid != 0 {
            einfo(Some(&bcat!("as group id ", gid.to_string().as_bytes())));
        }
        if let Some(r) = &ch_root {
            einfo(Some(&bcat!("in root `", r, "'")));
        }
        if let Some(d) = &ch_dir {
            einfo(Some(&bcat!("in dir `", d, "'")));
        }
        // QUIRK: the default level is INT_MIN, so without --nicelevel this
        // says "with a priority of -2147483648".
        if nicelevel != 0 {
            einfo(Some(&bcat!("with a priority of ", nicelevel.to_string().as_bytes())));
        }
        if let Some(n) = &name {
            einfo(Some(&bcat!("with a process name of ", n)));
        }
        eoutdent();
        stdio::exit(0);
    }

    ebeginv(Some(&bcat!("Detaching to start `", exec, "'")));
    eindentv();

    // Remove an existing pidfile.
    if let Some(p) = &pidfile {
        let _ = sys::unlink(p);
    }
    if background {
        sys::signal_setup(libc::SIGCHLD, handle_signal);
    }

    // A pipe to sync parent and child: the child never writes to it, so
    // the parent's read returns when it execs or exits.
    let (pipe_r, pipe_w) = match sys::pipe2_cloexec() {
        Ok((r, w)) => (std::os::fd::IntoRawFd::into_raw_fd(r), std::os::fd::IntoRawFd::into_raw_fd(w)),
        Err(e) => eerrorx(Some(&bcat!(applet, ": pipe2: ", e.strerror()))),
    };
    let child = match sys::fork() {
        Ok(p) => p,
        Err(e) => eerrorx(Some(&bcat!(applet, ": fork: ", e.strerror()))),
    };
    if child == 0 {
        Child {
            applet: &applet,
            exec: &exec,
            argv: &argv,
            numask,
            nicelevel,
            ionicec,
            ioniced,
            oom_score_adj,
            changeuser,
            uid,
            gid,
            ch_root,
            ch_dir,
            makepidfile,
            pidfile: pidfile.clone(),
            cap_iab,
            secbits,
            no_new_privs,
            background,
            redirect_stdin,
            redirect_stdout,
            redirect_stderr,
            stdout_process,
            stderr_process,
            notify,
            scheduler,
            sched_prio,
            argv_path,
        }
        .run(pipe_r);
    }

    // Parent.
    sys::close_raw(pipe_w);
    let mut b = [0u8];
    let ss = loop {
        let r = sys::read_raw(pipe_r, &mut b);
        if r == -1 && sys::Errno::last().0 == libc::EINTR {
            continue;
        }
        break r;
    };
    if ss == -1 {
        eerrorx(Some(&bcat!(applet, ": failed to read from pipe: ", sys::Errno::last().strerror())));
    }
    sys::close_raw(pipe_r);

    let mut pid = child;
    if !background {
        // Not backgrounding: wait for our pid to return.
        loop {
            let (r, status) = sys::waitpid_raw(child, 0);
            if r < 1 {
                let e = sys::Errno::last();
                eerror(Some(&bcat!("waitpid ", child.to_string().as_bytes(), ": ", e.strerror())));
                // (C returns -1 from main.)
                stdio::exit(255);
            }
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
                    eerror(Some(&bcat!(applet, ": failed to start `", exec, "'")));
                    stdio::exit(1);
                }
                break;
            }
        }
    }

    // Wait a little and check the daemon is still running: some badly
    // written ones fork and then barf.
    if start_wait == 0
        && let Some(p) = env::get("SSD_STARTWAIT").or_else(|| rc::conf::rc_conf_value("rc_start_wait"))
    {
        start_wait = scan_u32(openrc_compat::bytes::c_str_prefix(&p)).unwrap_or(0);
    }

    let mut ret = 0;
    if notify != Notify::None {
        if !notify_wait(&applet, notify) {
            ret = 1;
        }
    } else if start_wait > 0 {
        if !sys::nanosleep(i64::from(start_wait) * ONE_MS) {
            let e = sys::Errno::last();
            if e.0 != libc::EINTR {
                eerror(Some(&bcat!(applet, ": nanosleep: ", e.strerror())));
                stdio::exit(0);
            }
        }
        let alive = if background {
            sys::kill(pid, 0).is_ok()
        } else {
            if pidfile.is_some() {
                pid = get_pid(&applet, pidfile.as_deref());
                if pid == -1 {
                    eerrorx(Some(&bcat!(
                        applet,
                        ": did not create a valid pid in `",
                        pidfile.clone().unwrap_or_default(),
                        "'"
                    )));
                }
            } else {
                pid = 0;
            }
            do_stop(&applet, Some(&exec), Some(&margv), pid, uid, 0, false, test, false) > 0
        };
        if !alive {
            eerrorx(Some(&bcat!(applet, ": ", exec, " died")));
        }
    }

    if let Some(s) = &svcname {
        let _ = rc::daemon::rc_service_daemon_set(s, Some(&exec), Some(&margv), pidfile.as_deref(), true);
    }
    schedules::free_schedulelist();
    stdio::exit(ret)
}

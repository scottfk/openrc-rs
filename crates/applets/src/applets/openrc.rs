//! `openrc`: port of upstream's `src/openrc/rc.c`, the runlevel engine.
//! It stops the services that shouldn't run in the new runlevel, then
//! starts those of each runlevel the new one stacks, in dependency order.
//!
//! The `sysinit` runlevel first runs `init.sh` (and `init-early.sh`)
//! after a banner; with `rc_interactive`, pressing I asks before each
//! service is started.  The `rc.log` logger is in `rc_logger.rs`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::{bcat, env, exec, stdio, sys};
use openrc_einfo::{EColor, ecolor, eerror, eerrorx, eindent, einfo, eoutdent, ewarn};
use rc::deptree::{self, DepTree, options};
use rc::state::{self, flags};
use rc::svcdir::{RcDir, rc_dirfd_raw, rc_svcdir};

use crate::plugin::{self, rc_plugin_run, rc_plugin_run_opt};
use crate::rc_logger::{self, rc_logger_close, rc_logger_open};
use crate::shared::{
    Usage, default_runlevel, env_config, env_filter, exec_service, getoptstring_common, rc_conf_yesno,
    rc_deptree_load_updating,
};

/// `RC_HOOK_*` values for runlevels.
mod hook {
    pub const RUNLEVEL_STOP_IN: i32 = 1;
    pub const RUNLEVEL_STOP_OUT: i32 = 4;
    pub const RUNLEVEL_START_IN: i32 = 5;
    pub const RUNLEVEL_START_OUT: i32 = 8;
    pub const ABORT: i32 = 99;
}

const GETOPTSTRING: &str = concat!("a:no:s:S", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("no-stop", HasArg::No, b'n'),
    LongOpt::new("override", HasArg::Required, b'o'),
    LongOpt::new("service", HasArg::Required, b's'),
    LongOpt::new("sys", HasArg::No, b'S'),
];
const LONGOPTS_HELP: &[&str] = &[
    "do not stop any services",
    "override the next runlevel to change into\nwhen leaving single user or boot runlevels",
    "runs the service specified with the rest\nof the arguments",
    "output the RC system type, if any",
];
const USAGESTRING: &str = "Usage: openrc [options] [<runlevel>]";

/// `RC_KRUNLEVEL`: the runlevel to go to after boot or single user mode.
fn krunlevel_path() -> Vec<u8> {
    bcat!(rc::paths::SVCDIR, "/krunlevel")
}

// --- State shared with the signal handler and the atexit cleanup ------------

static APPLET: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// `service_pids`: the services we started or stopped and haven't reaped,
/// newest first.
static SERVICE_PIDS: Mutex<Vec<i32>> = Mutex::new(Vec::new());
/// Set once main has got as far as C's `atexit(cleanup)`.
static CLEANUP: AtomicBool = AtomicBool::new(false);
/// `termios_orig`: stdin's settings before the first `read_key()`.
static TERMIOS_ORIG: Mutex<Option<sys::libc::termios>> = Mutex::new(None);
/// The runlevel, for the hooks `run_program()` runs on failure.
static RUNLEVEL: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn termios_orig() -> Option<sys::libc::termios> {
    *TERMIOS_ORIG.lock().unwrap_or_else(|e| e.into_inner())
}

fn applet() -> Vec<u8> {
    APPLET.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// `add_pid()`, with SIGCHLD blocked as C does (its handler edits the list).
fn add_pid(pid: i32) {
    let old = sys::block_signal(sys::libc::SIGCHLD);
    SERVICE_PIDS.lock().unwrap_or_else(|e| e.into_inner()).insert(0, pid);
    sys::restore_sigmask(&old);
}

/// `remove_pid()`.  From the SIGCHLD handler the lock can't be held by
/// main (which blocks SIGCHLD while it holds it).
fn remove_pid(pid: i32) {
    let old = sys::block_signal(sys::libc::SIGCHLD);
    let mut list = SERVICE_PIDS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = list.iter().position(|&p| p == pid) {
        list.remove(i);
    }
    drop(list);
    sys::restore_sigmask(&old);
}

/// `clean_failed()`: empty `$svcdir/failed`.
fn clean_failed() {
    use sys::libc;
    let Ok(dir) = sys::at::openat(rc_dirfd_raw(RcDir::Svcdir), b"failed", libc::O_RDONLY | libc::O_DIRECTORY, 0) else {
        return;
    };
    let fd = std::os::fd::AsRawFd::as_raw_fd(&dir);
    let Ok(names) = sys::at::readdir_dup(fd) else { return };
    for name in names {
        if name == b"." || name == b".." {
            continue;
        }
        if let Err(e) = sys::at::unlinkat(fd, &name, 0) {
            eerror(Some(&bcat!(applet(), ": unlink '", rc_svcdir(), "/failed/", name, "': ", e.strerror())));
        }
    }
}

extern "C" fn cleanup() {
    use sys::libc;
    if !rc_logger::rc_in_logger() && !plugin::rc_in_plugin() && CLEANUP.load(SeqCst) {
        let a = applet();
        if a == b"rc" || a == b"openrc" {
            plugin::rc_plugin_unload();
            if let Some(t) = termios_orig() {
                sys::tcsetattr_now(0, &t);
            }
            // Clean runlevel start, stop markers.
            let svcdir = rc_dirfd_raw(RcDir::Svcdir);
            let _ = sys::at::unlinkat(svcdir, b"rc.starting", libc::AT_REMOVEDIR);
            let _ = sys::at::unlinkat(svcdir, b"rc.stopping", libc::AT_REMOVEDIR);
            clean_failed();
            rc_logger_close();
        }
    }
    stdio::flush();
}

extern "C" fn handle_signal(sig: i32) {
    use sys::libc;
    let serrno = sys::Errno::last().0;
    match sig {
        libc::SIGCHLD => {
            let pid = loop {
                match sys::waitpid(-1, libc::WNOHANG) {
                    Err(e) => {
                        if e.0 != libc::ECHILD {
                            eerror(Some(&bcat!("waitpid: ", e.strerror())));
                        }
                        // (C returns without restoring errno here.)
                        return;
                    }
                    Ok((pid, status)) => {
                        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                            break pid;
                        }
                    }
                }
            };
            if pid > 0 {
                remove_pid(pid);
            }
        }
        libc::SIGWINCH => {
            let tty = rc_logger::rc_logger_tty();
            if tty >= 0 {
                sys::copy_winsize(0, tty);
            }
        }
        libc::SIGINT | libc::SIGTERM | libc::SIGQUIT => {
            let signame = match sig {
                libc::SIGINT => "SIGINT",
                libc::SIGTERM => "SIGTERM",
                _ => "SIGQUIT",
            };
            eerrorx(Some(&bcat!(applet(), ": caught ", signame, ", aborting")));
        }
        libc::SIGUSR1 => {
            eerror(Some(b"rc: Aborting!"));
            // Block child signals.
            sys::block_signal(libc::SIGCHLD);
            // Kill any running services we have started.  (Main may hold
            // the list's lock here, where C would just read it.)
            if let Ok(list) = SERVICE_PIDS.try_lock() {
                for &pid in list.iter() {
                    let _ = sys::kill(pid, libc::SIGTERM);
                }
            }
            // Notify plugins we are aborting.
            rc_plugin_run_opt(hook::ABORT, None);
            stdio::exit(1);
        }
        _ => {
            eerror(Some(&bcat!(applet(), ": caught unknown signal ", sig.to_string().as_bytes())));
        }
    }
    sys::set_errno(serrno);
}

/// `wait_for_services()`: reap every child in our process group.
fn wait_for_services() {
    loop {
        while sys::waitpid(0, 0).is_ok() {}
        if sys::Errno::last().0 != sys::libc::EINTR {
            break;
        }
    }
}

/// `set_krunlevel()`: record (or, for the boot, single and sysinit
/// runlevels or none, remove) the runlevel to go to next.  False when
/// removed or on error.
fn set_krunlevel(level: Option<&[u8]>) -> bool {
    let path = krunlevel_path();
    if rc::dirs::rc_is_user() {
        return true;
    }
    let bootlevel = env::get("RC_BOOTLEVEL");
    let remove = match level {
        None => true,
        Some(l) => {
            // QUIRK: C compares with getenv("RC_BOOTLEVEL") unchecked; with
            // it unset (`openrc -o` runs before env_config sets it),
            // strcmp(level, NULL) crashes.
            let Some(boot) = &bootlevel else { crash() };
            l == &boot[..] || l == state::RC_LEVEL_SINGLE || l == state::RC_LEVEL_SYSINIT
        }
    };
    if remove {
        if sys::access(&path, sys::libc::F_OK).is_ok()
            && let Err(e) = sys::unlink(&path)
        {
            eerror(Some(&bcat!("unlink `", path, "': ", e.strerror())));
        }
        return false;
    }
    if let Err(e) = std::fs::write(openrc_compat::bytes::as_path(&path), level.unwrap_or_default()) {
        let e = sys::Errno(e.raw_os_error().unwrap_or(0));
        eerror(Some(&bcat!("fopen `", path, "': ", e.strerror())));
        return false;
    }
    true
}

/// Die as C does after dereferencing NULL.
pub fn crash() -> ! {
    stdio::flush();
    sys::default_signals(&[sys::libc::SIGSEGV]);
    let _ = sys::kill(sys::getpid(), sys::libc::SIGSEGV);
    stdio::exit(139)
}

/// `get_krunlevel()`: the first line of `$svcdir/krunlevel`, if any.
fn get_krunlevel() -> Option<Vec<u8>> {
    use std::io::BufRead;
    if rc::dirs::rc_is_user() {
        return None;
    }
    let path = krunlevel_path();
    sys::access(&path, sys::libc::F_OK).ok()?;
    let f = match std::fs::File::open(openrc_compat::bytes::as_path(&path)) {
        Ok(f) => f,
        Err(e) => {
            let e = sys::Errno(e.raw_os_error().unwrap_or(0));
            eerror(Some(&bcat!("fopen `", path, "': ", e.strerror())));
            return None;
        }
    };
    // xgetline(): one line without its '\n'; nothing on EOF or error.
    let mut line = Vec::new();
    match std::io::BufReader::new(f).read_until(b'\n', &mut line) {
        Ok(n) if n > 0 => {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            Some(openrc_compat::bytes::c_str_prefix(&line).to_vec())
        }
        _ => None,
    }
}

/// C's `dirname()`, which never returns an empty string.
fn dirname(path: &[u8]) -> Vec<u8> {
    let trimmed = {
        let mut end = path.len();
        while end > 1 && path[end - 1] == b'/' {
            end -= 1;
        }
        &path[..end]
    };
    match trimmed.iter().rposition(|&b| b == b'/') {
        None => b".".to_vec(),
        Some(i) => {
            let mut end = i;
            while end > 0 && trimmed[end - 1] == b'/' {
                end -= 1;
            }
            if end == 0 { b"/".to_vec() } else { trimmed[..end].to_vec() }
        }
    }
}

/// `rc_stringlist_split(rc_conf_value(name), " ")`: empty pieces kept.
fn conf_list(name: &str) -> Vec<Vec<u8>> {
    rc::conf::rc_conf_value(name)
        .map(|v| openrc_compat::bytes::c_str_prefix(&v).split(|&b| b == b' ').map(<[u8]>::to_vec).collect())
        .unwrap_or_default()
}

fn strings(list: &[&str]) -> Vec<Vec<u8>> {
    list.iter().map(|s| s.as_bytes().to_vec()).collect()
}

/// `RC_LEVEL_*` comparisons on an optional runlevel.
fn is(level: Option<&[u8]>, name: &[u8]) -> bool {
    level == Some(name)
}

/// What the stop and start passes share.
struct Engine {
    applet: Vec<u8>,
    runlevel: Vec<u8>,
    deptree: DepTree,
}

impl Engine {
    /// `runlevel_config()`: does `/etc/conf.d/<service>.<level>` (next to
    /// the script's directory) exist?
    fn runlevel_config(service: &[u8], level: &[u8]) -> bool {
        // dirname(NULL) is ".", as is dirname(".").
        let init = state::rc_service_resolve(service).unwrap_or_else(|| b".".to_vec());
        let dir = dirname(&dirname(&init));
        let conf = bcat!(dir, "/conf.d/", service, ".", level);
        sys::access(&conf, sys::libc::F_OK).is_ok()
    }

    /// `do_stop_services()`: stop (last first) what is up and shouldn't be.
    fn do_stop_services(
        &self,
        start_services: &[Vec<u8>],
        stop_services: &[Vec<u8>],
        newlevel: Option<&[u8]>,
        parallel: bool,
        going_down: bool,
    ) {
        let types_nw = strings(&["needsme", "wantsme"]);
        let crashed = rc_conf_yesno("rc_crashed_stop").0;
        let nostop = conf_list("rc_nostop");
        for service in stop_services.iter().rev() {
            let st = state::rc_service_state(service);
            if st & flags::STOPPED != 0 || st & flags::FAILED != 0 {
                continue;
            }
            // Sometimes we don't ever want to stop a service.
            if nostop.contains(service) {
                state::rc_service_mark(service, flags::FAILED);
                continue;
            }
            let kwords = deptree::rc_deptree_depend(&self.deptree, service, b"keyword").unwrap_or_default();
            let has = |k: &[u8]| kwords.iter().any(|w| w == k);
            if has(b"-stop") || has(b"nostop") || (going_down && (has(b"-shutdown") || has(b"noshutdown"))) {
                state::rc_service_mark(service, flags::FAILED);
                continue;
            }

            // If the service has crashed, skip further checks and just
            // stop it.
            if !(crashed && rc::daemon::rc_service_daemons_crashed(service)) {
                // If we're in the start list then don't bother stopping us.
                if start_services.contains(service) {
                    match newlevel {
                        // So we're in the start list.  But we should be
                        // stopped if we have a runlevel configuration file
                        // for either the current or next so we use the
                        // correct one.
                        Some(nl) if self.runlevel != nl => {
                            if !Self::runlevel_config(service, &self.runlevel) && !Self::runlevel_config(service, nl) {
                                continue;
                            }
                        }
                        _ => continue,
                    }
                } else {
                    // We got this far.  Last check is to see if any service
                    // that is going to be started depends on us.
                    let deporder = deptree::rc_deptree_depends(
                        &self.deptree,
                        Some(&types_nw),
                        std::slice::from_ref(service),
                        Some(newlevel.unwrap_or(&self.runlevel)),
                        options::STRICT | options::TRACE,
                    )
                    .0;
                    if deporder.iter().any(|s| start_services.contains(s)) {
                        continue;
                    }
                }
            }

            // After all that we can finally stop the blighter!
            let pid = exec_service(service, b"stop");
            if pid > 0 {
                add_pid(pid);
                if !parallel {
                    exec::rc_waitpid(pid);
                    remove_pid(pid);
                }
            }
        }
    }

    /// `do_start_services()`: start what is stopped, in order.
    fn do_start_services(&self, start_services: &[Vec<u8>], parallel: bool) {
        use sys::libc;
        let svcdir = rc_dirfd_raw(RcDir::Svcdir);
        let quiet = rc::misc::rc_yesno(env::get("EINFO_QUIET").as_deref());
        let mut interactive = !quiet && sys::at::faccessat(svcdir, b"interactive", libc::F_OK).is_ok();
        let (crashed, err) = rc_conf_yesno("rc_crashed_start");
        let crashed = crashed || err == Some(libc::ENOENT);

        'services: for service in start_services {
            let st = state::rc_service_state(service);
            if st & flags::FAILED != 0 {
                continue;
            }
            if st & flags::STOPPED == 0 {
                if crashed && rc::daemon::rc_service_daemons_crashed(service) {
                    state::rc_service_mark(service, flags::STOPPED);
                } else {
                    continue;
                }
            }
            if !interactive {
                interactive = want_interactive();
            }
            if interactive {
                'retry: loop {
                    stdio::out(b"\n");
                    einfo(Some(&bcat!("About to start the service ", service)));
                    eindent();
                    einfo(Some(b"1) Start the service\t\t2) Skip the service"));
                    einfo(Some(b"3) Continue boot process\t\t4) Exit to shell"));
                    eoutdent();
                    loop {
                        match read_key(true) {
                            b'1' => break 'retry,
                            b'2' => continue 'services,
                            b'3' => {
                                interactive = false;
                                break 'retry;
                            }
                            b'4' => {
                                open_shell(&self.applet);
                                continue 'retry;
                            }
                            _ => {}
                        }
                    }
                }
            }

            let pid = exec_service(service, b"start");
            if pid == -1 {
                break;
            }
            // Remember the pid if we're running in parallel.
            if pid > 0 {
                add_pid(pid);
                if !parallel {
                    exec::rc_waitpid(pid);
                    remove_pid(pid);
                }
            }
        }

        // Store our interactive status for boot.  (RC_BOOTLEVEL: C's
        // getenv is unchecked; env_config always sets it.)
        let bootlevel = env::get("RC_BOOTLEVEL").unwrap_or_default();
        if interactive && (self.runlevel == state::RC_LEVEL_SYSINIT || self.runlevel == bootlevel) {
            // mark_interactive()
            let _ = sys::at::openat(svcdir, b"interactive", libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o666);
        } else {
            let _ = sys::at::unlinkat(svcdir, b"interactive", 0);
        }
    }
}

/// `want_interactive()`: did the user press I (with `rc_interactive`)?
fn want_interactive() -> bool {
    static INTERACTIVE: Mutex<Option<bool>> = Mutex::new(None);
    if rc::misc::rc_yesno(env::get("EINFO_QUIET").as_deref()) {
        return false;
    }
    let interactive =
        *INTERACTIVE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(|| rc_conf_yesno("rc_interactive").0);
    if !interactive {
        return false;
    }
    matches!(read_key(false), b'I' | b'i')
}

/// `read_key()`: one key from the terminal on stdin (0 if it isn't one),
/// waiting for it if `block`, else only if one is there.
fn read_key(block: bool) -> u8 {
    use sys::libc;
    if !sys::isatty(0) {
        return 0;
    }
    // Save our terminal settings, to restore them at exit.
    let orig = *TERMIOS_ORIG.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(|| sys::tcgetattr(0));
    let mut t = sys::tcgetattr(0);
    t.c_lflag &= !(libc::ICANON | libc::ECHO);
    if block {
        t.c_cc[libc::VMIN] = 1;
    } else {
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 0;
    }
    sys::tcsetattr_now(0, &t);
    let mut c = [0u8];
    if sys::read_raw(0, &mut c) == -1 {
        eerror(Some(&bcat!("read: ", sys::Errno::last().strerror())));
    }
    sys::tcsetattr_now(0, &orig);
    c[0]
}

/// `run_program()`: run `prog` (with default signals) and wait for it;
/// its exit status is ignored.
fn run_program(prog: &[u8]) {
    use sys::libc;
    let applet = applet();
    let runlevel = RUNLEVEL.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // Block signals until we have forked.
    let old = sys::block_all_signals();
    let pid = match sys::fork() {
        Ok(pid) => pid,
        Err(e) => {
            rc_plugin_run(hook::RUNLEVEL_START_OUT, &runlevel);
            eerrorx(Some(&bcat!(applet, ": fork: ", e.strerror())));
        }
    };
    if pid == 0 {
        sys::default_signals(&[
            libc::SIGCHLD,
            libc::SIGHUP,
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGTERM,
            libc::SIGUSR1,
            libc::SIGWINCH,
        ]);
        sys::restore_sigmask(&old);
        if let Some(t) = termios_orig() {
            sys::tcsetattr_now(0, &t);
        }
        let e = sys::execv(prog, &[prog.to_vec()]);
        eerror(Some(&bcat!(applet, ": unable to exec `", prog, "': ", e.strerror())));
        sys::exit_immediately(1);
    }
    sys::restore_sigmask(&old);
    if exec::rc_waitpid(pid) == -1 {
        rc_plugin_run(hook::RUNLEVEL_START_OUT, &runlevel);
        eerrorx(Some(&bcat!(applet, ": failed to exec `", prog, "'")));
    }
}

/// `open_shell()`: run `rc_shell`, `$SHELL`, the user's shell or /bin/sh
/// (on a VSERVER system, halt instead).
fn open_shell(applet: &[u8]) {
    if rc::rc_sys() == Some("VSERVER") {
        let e = sys::execvp(b"halt", &[b"halt".to_vec(), b"-f".to_vec()]);
        eerrorx(Some(&bcat!(applet, ": unable to exec `halt -f': ", e.strerror())));
    }
    let shell = rc::conf::rc_conf_value("rc_shell")
        .map(|s| openrc_compat::bytes::c_str_prefix(&s).to_vec())
        .or_else(|| env::get("SHELL"))
        .or_else(|| sys::getpwuid(sys::getuid()).map(|pw| pw.shell))
        .unwrap_or_else(|| b"/bin/sh".to_vec());
    run_program(&shell);
}

/// `do_sysinit()`: the banner, `init-early.sh` and `init.sh`.
fn do_sysinit() {
    let early = bcat!(rc::paths::LIBEXECDIR, "/sh/init-early.sh");
    if sys::access(&early, sys::libc::F_OK).is_ok() {
        run_program(&early);
    }
    let c = |col: EColor| ecolor(col).to_bytes();
    let quiet = rc::misc::rc_yesno(env::get("EINFO_QUIET").as_deref());
    let (sysname, release, machine) = sys::uname_full().unwrap_or_default();
    if !quiet {
        let mut b = bcat!(
            "\n   ",
            c(EColor::Good),
            "OpenRC ",
            c(EColor::Hilite),
            rc::VERSION,
            c(EColor::Normal),
            " is starting up ",
            c(EColor::Bracket)
        );
        match rc::BRANDING {
            Some(brand) => b.extend_from_slice(&bcat!(brand, " (", machine, ")")),
            None => b.extend_from_slice(&bcat!(sysname, " ", release, " (", machine, ")")),
        }
        stdio::out(&b);
    }
    // (Even when quiet.)
    if let Some(sys) = rc::rc_sys() {
        stdio::out(&bcat!(" [", sys, "]"));
    }
    stdio::out(&bcat!(c(EColor::Normal), "\n\n"));
    if !quiet && rc_conf_yesno("rc_interactive").0 {
        stdio::out(&bcat!("Press ", c(EColor::Good), "I", c(EColor::Normal), " to enter interactive boot mode\n\n"));
    }

    env::set("RC_RUNLEVEL", state::RC_LEVEL_SYSINIT);
    run_program(&bcat!(rc::paths::LIBEXECDIR, "/sh/init.sh"));

    // init may have mounted /proc so we can now detect our real sys.
    if let Some(sys) = rc::rc_sys() {
        env::set("RC_SYS", sys.as_bytes());
    }
    // (C keeps this deptree and loads another later.)
    if rc_deptree_load_updating(0, None).is_none() {
        eerrorx(Some(b"failed to load deptree"));
    }
}

pub fn main(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    use sys::libc;

    let applet = openrc_compat::bytes::basename_c(args.first().map_or(&b""[..], |a| a)).to_vec();
    *APPLET.lock().unwrap_or_else(|e| e.into_inner()) = applet.clone();

    // Complain about old configuration settings if they exist.
    let conf_old = rc::paths::rc_conf_old();
    if sys::access(&conf_old, libc::F_OK).is_ok() {
        ewarn(Some(&bcat!(conf_old, " still exists on your system and should be removed.")));
        ewarn(Some(&bcat!("Please migrate to the appropriate settings in ", rc::paths::SYSCONFDIR, "/rc.conf")));
    }

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
    let mut nostop = false;
    let mut g = Getopt::new(args.clone(), GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match u8::try_from(opt).unwrap_or(0) {
            b'n' => nostop = true,
            b'o' => {
                let optarg = g.optarg.clone().filter(|a| !a.is_empty());
                if !optarg.as_deref().is_some_and(state::rc_runlevel_exists) {
                    let name = optarg.as_deref().unwrap_or(b"(null)");
                    eerror(Some(&bcat!("runlevel `", name, "' does not exist")));
                    stdio::exit(1);
                }
                if rc::dirs::rc_is_user() {
                    eerrorx(Some(b"user services do not currently support --override"));
                }
                if !set_krunlevel(optarg.as_deref()) {
                    stdio::exit(1);
                }
                einfo(Some(&bcat!("Overriding next runlevel to ", optarg.unwrap_or_default())));
                stdio::exit(0);
            }
            b's' => {
                let optarg = g.optarg.clone().unwrap_or_default();
                let Some(path) = state::rc_service_resolve(&optarg) else {
                    eerrorx(Some(&bcat!(applet, ": service `", optarg, "' does not exist")));
                };
                // The script with the arguments after it.
                let mut argv: Vec<Vec<u8>> = g.args[g.optind - 1..].to_vec();
                argv[0] = path.clone();
                stdio::flush();
                let e = sys::execv(&path, &argv);
                eerrorx(Some(&bcat!(applet, ": ", e.strerror())));
            }
            b'S' => {
                if let Some(sys) = rc::rc_sys() {
                    stdio::out(&bcat!(sys, "\n"));
                }
                stdio::exit(0);
            }
            _ => usage.common_option(opt, argc),
        }
    }

    // Change dir to / to ensure all init scripts don't use stuff in pwd.
    // For user services, change to the user's HOME instead.
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

    // Ensure our environment is pure.  Also, add our configuration to it.
    env_filter();
    env_config();

    let mut newlevel: Option<Vec<u8>> = g.args.get(g.optind).cloned();
    // To make life easier, we only have the shutdown runlevel as nothing
    // really needs to know that we're rebooting.  But for those that do,
    // you can test against RC_REBOOT.
    if let Some(nl) = &newlevel {
        if rc::dirs::rc_is_user() && nl == state::RC_LEVEL_SYSINIT {
            eerrorx(Some(&bcat!(applet, ": the sysinit runlevel can't be used for user services.")));
        }
        if nl == b"reboot" {
            newlevel = Some(state::RC_LEVEL_SHUTDOWN.to_vec());
            env::set("RC_REBOOT", b"YES");
        }
    }

    sys::atexit(cleanup);
    CLEANUP.store(true, SeqCst);

    // Enable logging.
    env::set("EINFO_LOG", b"openrc");
    // Export our PID.
    env::set("RC_PID", sys::getpid().to_string().as_bytes());

    // (env_config always sets it.)
    let bootlevel = env::get("RC_BOOTLEVEL").unwrap_or_default();
    let mut runlevel = default_runlevel();

    *RUNLEVEL.lock().unwrap_or_else(|e| e.into_inner()) = runlevel.clone();

    rc_logger_open(newlevel.as_deref().unwrap_or(&runlevel));

    for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGUSR1, libc::SIGWINCH] {
        sys::signal_setup(sig, handle_signal);
    }

    // Run any special sysinit foo.
    if is(newlevel.as_deref(), state::RC_LEVEL_SYSINIT) {
        do_sysinit();
        runlevel = default_runlevel();
        *RUNLEVEL.lock().unwrap_or_else(|e| e.into_inner()) = runlevel.clone();
    }

    plugin::rc_plugin_load();

    // Now we start handling our children.
    sys::signal_setup(libc::SIGCHLD, handle_signal);

    let going_down =
        is(newlevel.as_deref(), state::RC_LEVEL_SHUTDOWN) || is(newlevel.as_deref(), state::RC_LEVEL_SINGLE);
    if going_down {
        let nl = newlevel.clone().unwrap_or_default();
        if sys::access(&krunlevel_path(), libc::F_OK).is_err() {
            set_krunlevel(Some(&runlevel));
        }
        state::rc_runlevel_set(&nl);
        env::set("RC_RUNLEVEL", &nl);
        env::set("RC_GOINGDOWN", b"YES");
    } else {
        // We should not use krunlevel in sysinit or boot runlevels.
        let skip = newlevel.as_deref().is_some_and(|nl| nl == state::RC_LEVEL_SYSINIT || nl == &bootlevel[..]);
        if !skip && let Some(k) = get_krunlevel() {
            newlevel = Some(k);
            set_krunlevel(None);
        }
        if let Some(nl) = &newlevel {
            if &runlevel != nl && !state::rc_runlevel_exists(nl) {
                eerrorx(Some(&bcat!(nl, ": not a valid runlevel")));
            }
            if nl == state::RC_LEVEL_SYSINIT {
                // If we requested a runlevel, save it now.
                if let Some(p) =
                    rc::misc::rc_proc_getent("rc_runlevel").or_else(|| rc::misc::rc_proc_getent("softlevel"))
                {
                    set_krunlevel(Some(&p));
                }
            }
        }
    }

    rc_plugin_run(hook::RUNLEVEL_STOP_IN, if going_down { newlevel.as_deref().unwrap_or_default() } else { &runlevel });

    // Check if runlevel is valid if we're changing.  (Dead: the same test
    // already failed above.)
    if let Some(nl) = &newlevel
        && &runlevel != nl
        && !going_down
        && !state::rc_runlevel_exists(nl)
    {
        rc_plugin_run(hook::RUNLEVEL_STOP_OUT, &runlevel);
        eerrorx(Some(&bcat!(nl, ": is not a valid runlevel")));
    }

    // Load our deptree.
    let mut regen = 0;
    let Some(deptree) = rc_deptree_load_updating(0, Some(&mut regen)) else {
        rc_plugin_run(hook::RUNLEVEL_STOP_OUT, &runlevel);
        eerrorx(Some(b"failed to load deptree"));
    };

    let svcdir = rc_dirfd_raw(RcDir::Svcdir);
    if sys::at::faccessat(svcdir, b"clock-skewed", libc::F_OK).is_ok() {
        ewarn(Some(b"WARNING: clock skew detected!"));
    }

    // Clean the failed services state dir.
    clean_failed();

    if let Err(e) = sys::at::mkdirat(svcdir, b"rc.stopping", 0o755) {
        rc_plugin_run(hook::RUNLEVEL_STOP_OUT, &runlevel);
        eerrorx(Some(&bcat!(
            applet,
            ": failed to create stopping dir '",
            rc_svcdir(),
            "/rc.stopping': ",
            e.strerror()
        )));
    }

    let depoptions = options::STRICT | options::TRACE;
    let types_nwua = strings(&["ineed", "iwant", "iuse", "iafter"]);

    // All the services we could stop (assuming they won't be active in the
    // new or current runlevel): started, inactive or starting.
    let mut stop_services = state::rc_services_in_state(flags::STARTED);
    stop_services.extend(state::rc_services_in_state(flags::INACTIVE));
    stop_services.extend(state::rc_services_in_state(flags::STARTING));
    stop_services.sort();
    let stop_services = deptree::rc_deptree_depends(
        &deptree,
        Some(&types_nwua),
        &stop_services,
        Some(&runlevel),
        depoptions | options::STOP,
    )
    .0;

    // All the services which should be started for the new or current
    // runlevel, including those in boot, sysinit and hotplugged.
    let hotplugged = state::rc_services_in_state(flags::HOTPLUGGED);
    let target = newlevel.clone().unwrap_or_else(|| runlevel.clone());
    let mut start_services = state::rc_services_in_runlevel_stacked(Some(&target));
    if target != state::RC_LEVEL_SHUTDOWN && target != state::RC_LEVEL_SYSINIT {
        start_services.extend(state::rc_services_in_runlevel(Some(state::RC_LEVEL_SYSINIT)));
        // If we are NOT headed for the single-user runlevel...
        if target != state::RC_LEVEL_SINGLE {
            // If we are NOT headed for the boot runlevel...
            if target != bootlevel {
                start_services.extend(state::rc_services_in_runlevel(Some(&bootlevel)));
            }
            for svc in &hotplugged {
                if !start_services.contains(svc) {
                    start_services.push(svc.clone());
                }
            }
        }
    }

    let parallel = rc_conf_yesno("rc_parallel").0;
    let engine = Engine { applet: applet.clone(), runlevel: runlevel.clone(), deptree };

    // Now stop the services that shouldn't be running.
    if !nostop {
        engine.do_stop_services(&start_services, &stop_services, newlevel.as_deref(), parallel, going_down);
    }

    // Wait for our services to finish.
    wait_for_services();

    // Notify the plugins we have finished.
    rc_plugin_run(
        hook::RUNLEVEL_STOP_OUT,
        if going_down { newlevel.as_deref().unwrap_or_default() } else { &runlevel },
    );

    let _ = sys::at::unlinkat(svcdir, b"rc.stopping", libc::AT_REMOVEDIR);

    // Store the new runlevel.
    if let Some(nl) = &newlevel {
        state::rc_runlevel_set(nl);
        runlevel = nl.clone();
        *RUNLEVEL.lock().unwrap_or_else(|e| e.into_inner()) = runlevel.clone();
        env::set("RC_RUNLEVEL", &runlevel);
    }
    // We can't log beyond this point as the shutdown runlevel will mount
    // / read-only.
    if runlevel == state::RC_LEVEL_SHUTDOWN {
        rc_logger_close();
    }

    let _ = sys::at::mkdirat(svcdir, b"rc.starting", 0o755);

    rc_plugin_run(hook::RUNLEVEL_START_IN, &runlevel);

    // Re-add our hotplugged services if they stopped.
    for svc in &hotplugged {
        state::rc_service_mark(svc, flags::HOTPLUGGED);
    }

    // If the "noinit" parameter was passed on the kernel command line then
    // mark the specified services as started so they will not be started
    // by us.
    let noinit = rc::misc::rc_proc_getent("noinit");
    if let Some(list) = &noinit {
        for token in list.split(|&b| b == b',') {
            state::rc_service_mark(token, flags::STARTED);
        }
    }

    // Start the runlevels that make up the target, deepest stack first.
    let engine = Engine { runlevel: runlevel.clone(), ..engine };
    for rlevel in state::rc_runlevel_stacks(&runlevel).iter().rev() {
        let mut run_services = state::rc_services_in_runlevel(Some(rlevel));
        run_services.sort();
        let deporder = deptree::rc_deptree_depends(
            &engine.deptree,
            Some(&types_nwua),
            &run_services,
            Some(rlevel),
            depoptions | options::START,
        )
        .0;
        engine.do_start_services(&deporder, parallel);
        // Wait for our services to finish.
        wait_for_services();
    }

    // Mark the "noinit" services as stopped again, so that our records
    // reflect reality.
    if let Some(list) = rc::misc::rc_proc_getent("noinit") {
        for token in list.split(|&b| b == b',') {
            state::rc_service_mark(token, flags::STOPPED);
        }
    }

    rc_plugin_run(hook::RUNLEVEL_START_OUT, &runlevel);

    // If we're in the boot runlevel and we regenerated our dependencies we
    // need to delete them so that they are regenerated again in the
    // default runlevel as they may depend on things that are now available.
    if regen != 0 && runlevel == bootlevel {
        let _ = sys::at::unlinkat(svcdir, b"deptree", 0);
    }
    0
}

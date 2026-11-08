//! Port of upstream's `src/openrc/rc-logger.c`: with `rc_logger="YES"`,
//! openrc forks a logger that reads everything written to its stdout and
//! stderr (moved onto a pty), copies it to the real stdout, and keeps a
//! cleaned-up copy (no `\r`, escape sequences or unprintable bytes) in
//! `$svcdir/rc.log`.  On close that is appended to `rc_log_path`
//! (default `/var/log/rc.log`).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::SeqCst};

use openrc_compat::{bcat, env, sys};
use openrc_einfo::{eerror, eerrorx};
use rc::state;

const DEFAULTLOG: &[u8] = b"/var/log/rc.log";
const BUFSIZ: usize = 8192;

static SIGNAL_PIPE_W: AtomicI32 = AtomicI32::new(-1);
static FD_STDOUT: AtomicI32 = AtomicI32::new(-1);
static FD_STDERR: AtomicI32 = AtomicI32::new(-1);
static LOGGER_PID: AtomicI32 = AtomicI32::new(-1);
/// `rc_logger_tty`: the pty master (for SIGWINCH), or -1.
static LOGGER_TTY: AtomicI32 = AtomicI32::new(-1);
static IN_LOGGER: AtomicBool = AtomicBool::new(false);

/// `rc_in_logger`: are we the logger process?
pub fn rc_in_logger() -> bool {
    IN_LOGGER.load(SeqCst)
}

/// `rc_logger_tty`.
pub fn rc_logger_tty() -> i32 {
    LOGGER_TTY.load(SeqCst)
}

/// `write_log()`'s filter, whose escape state carries over between calls.
#[derive(Default)]
struct LogFilter {
    in_escape: bool,
    in_term: bool,
}

impl LogFilter {
    /// Write `buffer` to `log` without `\r`, escape sequences (`ESC` up to
    /// a letter, or the next byte if it isn't `[`) or bytes that aren't
    /// printable ASCII (in the C locale), newlines kept.
    fn write_log(&mut self, log: &mut File, buffer: &[u8]) {
        for &c in buffer {
            match c {
                b'\r' => continue,
                0x1b => {
                    self.in_escape = true;
                    self.in_term = false;
                    continue;
                }
                b'\n' => {
                    self.in_escape = false;
                    self.in_term = false;
                }
                b'[' if self.in_escape => self.in_term = true,
                _ => {}
            }
            if !self.in_escape {
                if !(0x20..0x7f).contains(&c) && c != b'\n' {
                    continue;
                }
                // (One write per byte, as in C.)
                if let Err(e) = log.write_all(&[c]) {
                    eerror(Some(&bcat!("write: ", errstr(&e))));
                }
                continue;
            }
            if !self.in_term || c.is_ascii_alphabetic() {
                self.in_escape = false;
                self.in_term = false;
            }
        }
    }
}

fn errstr(e: &std::io::Error) -> Vec<u8> {
    sys::Errno(e.raw_os_error().unwrap_or(0)).strerror()
}

/// `write_time()`: `\nrc <level> logging <s> at <asctime>\n`.
fn write_time(f: &mut File, level: &[u8], s: &str) {
    let t = sys::asctime_local(sys::time());
    let _ = f.write_all(&bcat!("\nrc ", level, " logging ", s, " at ", t, "\n"));
}

/// fopen(path, "ae").
fn open_append(path: &[u8]) -> std::io::Result<File> {
    OpenOptions::new().append(true).create(true).open(openrc_compat::bytes::as_path(path))
}

/// `rc_logger_close()`: tell the logger to finish, wait for it, and put
/// our stdout and stderr back.
pub fn rc_logger_close() {
    let w = SIGNAL_PIPE_W.swap(-1, SeqCst);
    if w > -1 {
        if sys::write_raw(w, &sys::libc::SIGTERM.to_ne_bytes()) == -1 {
            eerror(Some(&bcat!("write: ", sys::Errno::last().strerror())));
        }
        sys::close_raw(w);
    }
    let pid = LOGGER_PID.load(SeqCst);
    if pid > 0 {
        let _ = sys::waitpid(pid, 0);
    }
    let (o, e) = (FD_STDOUT.load(SeqCst), FD_STDERR.load(SeqCst));
    if o > -1 {
        sys::dup2(o, 1);
    }
    if e > -1 {
        sys::dup2(e, 2);
    }
}

/// `rc_logger_open(level)`.
pub fn rc_logger_open(level: &[u8]) {
    if !crate::shared::rc_conf_yesno("rc_logger").0 {
        return;
    }
    let (r, w) = match sys::pipe2_cloexec() {
        Ok((r, w)) => (std::os::fd::IntoRawFd::into_raw_fd(r), std::os::fd::IntoRawFd::into_raw_fd(w)),
        Err(e) => eerrorx(Some(&bcat!("pipe2: ", e.strerror()))),
    };
    let pty = if sys::isatty(1) { sys::openpty_like(1) } else { sys::openpty_plain() };
    // (C leaves the pipe open when there's no pty.)
    let Some((master, slave)) = pty else { return };
    LOGGER_TTY.store(master, SeqCst);

    let pid = match sys::fork() {
        Ok(pid) => pid,
        Err(e) => {
            // (Carrying on without a logger, the pipe and pty left open.)
            eerror(Some(&bcat!("fork: ", e.strerror())));
            return;
        }
    };
    LOGGER_PID.store(pid, SeqCst);
    if pid == 0 {
        IN_LOGGER.store(true, SeqCst);
        sys::close_raw(w);
        logger(level, r, master);
    }
    sys::setpgid(pid, 0);
    let (o, e) = (sys::dup(1), sys::dup(2));
    for fd in [o, e] {
        if sys::fd_flags(fd) == 0 {
            sys::set_cloexec(fd);
        }
    }
    FD_STDOUT.store(o, SeqCst);
    FD_STDERR.store(e, SeqCst);
    sys::dup2(slave, 1);
    sys::dup2(slave, 2);
    if slave != 0 && slave != 1 && slave != 2 {
        sys::close_raw(slave);
    }
    sys::close_raw(r);
    SIGNAL_PIPE_W.store(w, SeqCst);
}

/// The logger process: relay the pty to stdout, logging it, until openrc
/// sends SIGTERM down the pipe; then append the log to `rc_log_path`.
fn logger(level: &[u8], pipe_r: i32, tty: i32) -> ! {
    use sys::libc;
    let tmplog = bcat!(rc::svcdir::rc_svcdir(), "/rc.log");
    let mut filter = LogFilter::default();
    let mut log = match open_append(&tmplog) {
        Ok(mut f) => {
            write_time(&mut f, level, "started");
            Some(f)
        }
        Err(_) => None,
    };
    // Without the log file (yet), keep it in memory.
    let mut logbuf: Option<Vec<u8>> = if log.is_none() { Some(Vec::new()) } else { None };

    let mut fds = [
        sys::PollFd { fd: pipe_r, events: libc::POLLIN, revents: 0 },
        sys::PollFd { fd: tty, events: libc::POLLIN, revents: 0 },
    ];
    let mut buffer = [0u8; BUFSIZ];
    loop {
        match sys::poll(&mut fds, if tty >= 0 { 2 } else { 1 }, -1) {
            Err(e) => {
                eerror(Some(&bcat!("poll: ", e.strerror())));
                break;
            }
            Ok(0) => continue,
            Ok(_) => {}
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            buffer.fill(0);
            // Deviation: C uses read()'s -1 as a length (and crashes); a
            // failed read writes nothing here.
            let bytes = usize::try_from(sys::read_raw(tty, &mut buffer)).unwrap_or(0);
            if sys::write_raw(1, &buffer[..bytes]) == -1 {
                eerror(Some(&bcat!("write: ", sys::Errno::last().strerror())));
            }
            match (&mut log, &mut logbuf) {
                (Some(f), _) => filter.write_log(f, &buffer[..bytes]),
                (None, Some(b)) => b.extend_from_slice(&buffer[..bytes]),
                (None, None) => {}
            }
        }
        // Only SIGTERMs come down this pipe.
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            break;
        }
    }
    if let Some(b) = logbuf
        && let Ok(mut f) = open_append(&tmplog)
    {
        write_time(&mut f, level, "started");
        filter.write_log(&mut f, &b);
        log = Some(f);
    }
    if let Some(mut f) = log {
        write_time(&mut f, level, "stopped");
    }

    // Append the temporary log to the real log.
    let logfile = match rc::conf::rc_conf_value("rc_log_path") {
        Some(p) => openrc_compat::bytes::c_str_prefix(&p).to_vec(),
        None if !rc::dirs::rc_is_user() => DEFAULTLOG.to_vec(),
        None => {
            let (usrlog, state_home) = if let Some(s) = env::get("XDG_STATE_HOME") {
                (bcat!(s, "/rc.log"), s)
            } else if let Some(h) = env::get("HOME") {
                (bcat!(h, "/.local/state/rc.log"), b".local/state".to_vec())
            } else {
                // C: mkpath(NULL) dereferences NULL.
                crate::applets::openrc::crash();
            };
            // QUIRK: mkpath() means to create each parent, but calls
            // mkdir() on the whole path every time, so only the last
            // directory is ever made.
            let _ = sys::mkdir(&state_home, 0o755);
            usrlog
        }
    };
    if logfile == tmplog {
        eerror(Some(b"Cowardly refusing to concatenate a logfile into itself."));
        eerrorx(Some(&bcat!(
            "Please change rc_log_path to something other than ",
            tmplog,
            " to get rid of this message"
        )));
    }

    let mut log_error = false;
    // C tests errno afterwards; in practice only a read-only log file's
    // open leaves EROFS there.
    let mut rofs = false;
    match open_append(&logfile) {
        Ok(mut plog) => match File::open(openrc_compat::bytes::as_path(&tmplog)) {
            Ok(mut tl) => {
                let mut buf = [0u8; BUFSIZ];
                loop {
                    let n = match tl.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if let Err(e) = plog.write_all(&buf[..n]) {
                        log_error = true;
                        eerror(Some(&bcat!("Error: write(", logfile, ") failed: ", errstr(&e))));
                        break;
                    }
                }
            }
            Err(e) => {
                log_error = true;
                eerror(Some(&bcat!("Error: fopen(", tmplog, ") failed: ", errstr(&e))));
            }
        },
        Err(e) => {
            // The log file or its directory may be read-only during
            // sysinit and shutdown, so skip the error then.
            rofs = e.raw_os_error() == Some(libc::EROFS);
            if !rofs && level != state::RC_LEVEL_SHUTDOWN && level != state::RC_LEVEL_SYSINIT {
                log_error = true;
                eerror(Some(&bcat!("Error: fopen(", logfile, ") failed: ", errstr(&e))));
            }
        }
    }

    // Try to keep the temporary log in case of errors.
    if !log_error {
        if !rofs
            && level != state::RC_LEVEL_SHUTDOWN
            && level != state::RC_LEVEL_SYSINIT
            && let Err(e) = sys::unlink(&tmplog)
        {
            eerror(Some(&bcat!("Error: unlink(", tmplog, ") failed: ", e.strerror())));
        }
    } else if sys::access(&tmplog, libc::F_OK).is_ok() {
        eerrorx(Some(&bcat!("Warning: temporary logfile left behind: ", tmplog)));
    }
    openrc_compat::stdio::exit(0)
}

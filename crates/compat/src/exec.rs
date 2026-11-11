//! Port of upstream's `shared/rc_exec.c`: fork + execvp with optional
//! redirections, reporting exec failures back to the parent.
//!
//! Done by hand rather than with `std::process::Command` so that callers
//! can reap children with `waitpid(-1)` (as `do_unmount` does) and so the
//! child setup (signal mask, uid/gid, setsid) matches C exactly.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::OnceLock;

use crate::bytes::to_cstring;
use crate::sys::{Errno, libc};

/// Where to point a standard stream of the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redirect {
    /// Inherit (`EXEC_NO_REDIRECT`).
    Inherit,
    /// A new pipe; our end is returned in [`ExecResult`] (`EXEC_MKPIPE`).
    Pipe,
    /// `/dev/null` (`EXEC_DEVNULL`).
    DevNull,
    /// An existing fd.
    Fd(i32),
}

/// `struct exec_args`.
#[derive(Debug, Clone)]
pub struct ExecArgs {
    pub argv: Vec<Vec<u8>>,
    /// Program to run if not `argv[0]`.
    pub cmd: Option<Vec<u8>>,
    pub stdin: Redirect,
    pub stdout: Redirect,
    pub stderr: Redirect,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub setsid: bool,
}

impl ExecArgs {
    /// `exec_init(argv)`.
    pub fn new<I: IntoIterator<Item = T>, T: AsRef<[u8]>>(argv: I) -> ExecArgs {
        ExecArgs {
            argv: argv.into_iter().map(|a| a.as_ref().to_vec()).collect(),
            cmd: None,
            stdin: Redirect::Inherit,
            stdout: Redirect::Inherit,
            stderr: Redirect::Inherit,
            uid: None,
            gid: None,
            setsid: false,
        }
    }
}

/// `struct exec_result`.
#[derive(Debug)]
pub struct ExecResult {
    pub pid: i32,
    pub stdin: Option<OwnedFd>,
    pub stdout: Option<OwnedFd>,
    pub stderr: Option<OwnedFd>,
}

/// The shared `/dev/null` fd (opened once, never closed).
fn devnull() -> i32 {
    static FD: OnceLock<i32> = OnceLock::new();
    *FD.get_or_init(|| {
        // SAFETY: valid path literal.
        let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            let e = Errno::last();
            crate::stdio::err(&crate::bcat!("Failed to open /dev/null: ", e.strerror()));
            // SAFETY: _exit is always safe.
            unsafe { libc::_exit(1) };
        }
        fd
    })
}

fn pipe2() -> Result<(OwnedFd, OwnedFd), Errno> {
    let mut fds = [0; 2];
    // SAFETY: fds has room for two fds.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(Errno::last());
    }
    // SAFETY: pipe2 just created these and we own them.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// `do_exec()`: start `args`; `Err(errno)` if fork or exec failed.
pub fn do_exec(args: &ExecArgs) -> Result<ExecResult, Errno> {
    let (exec_r, exec_w) = pipe2()?;
    // (child-side fd, our end) for each stream.
    let setup = |r: Redirect, child_reads: bool| -> Result<(i32, Option<OwnedFd>, Option<OwnedFd>), Errno> {
        Ok(match r {
            Redirect::Inherit => (-1, None, None),
            Redirect::DevNull => (devnull(), None, None),
            Redirect::Fd(fd) => (fd, None, None),
            Redirect::Pipe => {
                let (rd, wr) = pipe2()?;
                if child_reads { (rd.as_raw_fd(), Some(rd), Some(wr)) } else { (wr.as_raw_fd(), Some(wr), Some(rd)) }
            }
        })
    };
    let (in_fd, in_child, in_ours) = setup(args.stdin, true)?;
    let (out_fd, out_child, out_ours) = setup(args.stdout, false)?;
    let (err_fd, err_child, err_ours) = setup(args.stderr, false)?;

    let cmd = to_cstring(args.cmd.as_deref().unwrap_or(&args.argv[0]));
    let argv: Vec<CString> = args.argv.iter().map(|a| to_cstring(a)).collect();
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    // SAFETY: the child only calls async-signal-safe functions before
    // exec or _exit; all memory it touches was prepared above.
    unsafe {
        let mut full: libc::sigset_t = std::mem::zeroed();
        let mut old: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut full);
        libc::sigprocmask(libc::SIG_SETMASK, &full, &mut old);
        let pid = libc::fork();
        if pid < 0 {
            let e = Errno::last();
            libc::sigprocmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
            return Err(e);
        }
        if pid == 0 {
            let ok = (in_fd < 0 || libc::dup2(in_fd, 0) >= 0)
                && (out_fd < 0 || libc::dup2(out_fd, 1) >= 0)
                && (err_fd < 0 || libc::dup2(err_fd, 2) >= 0)
                && args.gid.is_none_or(|g| libc::setgid(g) >= 0)
                && args.uid.is_none_or(|u| libc::setuid(u) >= 0)
                && (!args.setsid || libc::setsid() >= 0);
            if ok {
                libc::sigprocmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
                libc::execvp(cmd.as_ptr(), argv_ptrs.as_ptr());
            }
            let err = *libc::__errno_location();
            libc::write(exec_w.as_raw_fd(), (&err as *const i32).cast(), std::mem::size_of::<i32>());
            libc::_exit(1);
        }

        drop(exec_w);
        let mut err: i32 = 0;
        let n = libc::read(exec_r.as_raw_fd(), (&mut err as *mut i32).cast(), std::mem::size_of::<i32>());
        let failed = n != 0;
        if failed {
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
        libc::sigprocmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
        drop((in_child, out_child, err_child));
        if failed {
            let e = if n == std::mem::size_of::<i32>() as isize { err } else { libc::EINVAL };
            return Err(Errno(e));
        }
        Ok(ExecResult { pid, stdin: in_ours, stdout: out_ours, stderr: err_ours })
    }
}

/// `posix_spawn(&pid, path, actions, NULL, argv, environ)`: run `path`
/// (no PATH search) with the current environment, with `out_err` (if
/// given) dup'ed onto stdout and stderr.  `on_pid` gets the child's pid
/// while signals are still blocked, so a SIGCHLD handler can already
/// recognize it.  Err(errno) if fork, the dups or exec failed.
pub fn spawn(path: &[u8], argv: &[Vec<u8>], out_err: Option<i32>, on_pid: impl FnOnce(i32)) -> Result<i32, Errno> {
    spawn_impl(path, argv, out_err, on_pid, false)
}

/// `posix_spawn()` with `POSIX_SPAWN_SETSIGMASK` and an empty mask (as
/// `exec_service()` in shared/misc.c does): run `path` with the current
/// environment and every signal unblocked in the child.
pub fn spawn_unmasked(path: &[u8], argv: &[Vec<u8>]) -> Result<i32, Errno> {
    spawn_impl(path, argv, None, |_| {}, true)
}

fn spawn_impl(
    path: &[u8],
    argv: &[Vec<u8>],
    out_err: Option<i32>,
    on_pid: impl FnOnce(i32),
    empty_mask: bool,
) -> Result<i32, Errno> {
    let (exec_r, exec_w) = pipe2()?;
    let cmd = to_cstring(path);
    let argv: Vec<CString> = argv.iter().map(|a| to_cstring(a)).collect();
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let fd = out_err.unwrap_or(-1);

    // SAFETY: the child only calls async-signal-safe functions before
    // exec or _exit; all memory it touches was prepared above.
    unsafe {
        let mut full: libc::sigset_t = std::mem::zeroed();
        let mut old: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut full);
        libc::sigprocmask(libc::SIG_SETMASK, &full, &mut old);
        let pid = libc::fork();
        if pid < 0 {
            let e = Errno::last();
            libc::sigprocmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
            return Err(e);
        }
        if pid == 0 {
            if fd < 0 || (libc::dup2(fd, 1) >= 0 && libc::dup2(fd, 2) >= 0) {
                if empty_mask {
                    libc::sigemptyset(&mut full);
                    libc::sigprocmask(libc::SIG_SETMASK, &full, std::ptr::null_mut());
                } else {
                    libc::sigprocmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
                }
                unsafe extern "C" {
                    static environ: *const *const libc::c_char;
                }
                libc::execve(cmd.as_ptr(), argv_ptrs.as_ptr(), environ);
            }
            let err = *libc::__errno_location();
            libc::write(exec_w.as_raw_fd(), (&err as *const i32).cast(), std::mem::size_of::<i32>());
            libc::_exit(127);
        }

        drop(exec_w);
        let mut err: i32 = 0;
        let n = libc::read(exec_r.as_raw_fd(), (&mut err as *mut i32).cast(), std::mem::size_of::<i32>());
        if n != 0 {
            libc::waitpid(pid, std::ptr::null_mut(), 0);
            libc::sigprocmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
            return Err(Errno(if n == std::mem::size_of::<i32>() as isize { err } else { libc::EINVAL }));
        }
        on_pid(pid);
        libc::sigprocmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
        Ok(pid)
    }
}

/// `rc_waitpid()`: wait for `pid`, retrying on EINTR; -1 on error.
pub fn rc_waitpid(pid: i32) -> i32 {
    loop {
        match crate::sys::waitpid(pid, 0) {
            Ok((_, status)) => return status,
            Err(e) if e.0 == libc::EINTR => continue,
            Err(_) => return -1,
        }
    }
}

//! Thin, safe wrappers over the libc calls openrc-rs needs.
//!
//! The wrappers deliberately do *not* add flags of their own (no implicit
//! `O_CLOEXEC` etc.): the port must issue the same syscalls as the C code.

use std::ffi::CStr;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use crate::bytes::to_cstring;

pub use libc;

/// An `errno` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Errno(pub i32);

impl Errno {
    /// The current thread's `errno`.
    pub fn last() -> Errno {
        Errno(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }

    /// `strerror(errno)`, in the C locale (OpenRC never calls `setlocale`).
    pub fn strerror(self) -> Vec<u8> {
        strerror(self.0)
    }
}

/// `strerror(errnum)`.
pub fn strerror(errnum: i32) -> Vec<u8> {
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: the buffer is valid for its length; the XSI strerror_r (which
    // the libc crate binds on glibc and musl) always NUL-terminates it.
    unsafe {
        libc::strerror_r(errnum, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_bytes().to_vec()
    }
}

fn cvt(ret: libc::c_int) -> Result<libc::c_int, Errno> {
    if ret == -1 { Err(Errno::last()) } else { Ok(ret) }
}

/// `isatty(fd)`.
pub fn isatty(fd: i32) -> bool {
    // SAFETY: isatty is safe to call on any integer.
    unsafe { libc::isatty(fd) == 1 }
}

/// `st_blksize` of an fd, if it can be `fstat`ed.
pub fn blksize(fd: i32) -> Option<usize> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat writes a full struct stat on success.
    let ok = unsafe { libc::fstat(fd, st.as_mut_ptr()) } == 0;
    // SAFETY: initialised when fstat succeeded.
    ok.then(|| unsafe { st.assume_init() }.st_blksize as usize)
}

/// Write all of `data` to `fd`, retrying on `EINTR`, silently giving up on
/// any other error (stdio output errors are ignored throughout OpenRC).
pub fn write_all(fd: i32, mut data: &[u8]) {
    while !data.is_empty() {
        // SAFETY: data is a valid buffer of the given length.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n < 0 {
            if Errno::last().0 == libc::EINTR {
                continue;
            }
            return;
        }
        data = &data[n as usize..];
    }
}

/// `write_all` on a borrowed fd.
pub fn write_all_fd(fd: BorrowedFd<'_>, data: &[u8]) {
    write_all(fd.as_raw_fd(), data);
}

/// Restore the default `SIGPIPE` disposition (Rust's runtime ignores it,
/// C programs die from it).
pub fn reset_sigpipe() {
    // SAFETY: setting a signal to SIG_DFL has no memory-safety implications.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// Directory fd argument for the `*at` calls: `None` means `AT_FDCWD`.
fn raw_dirfd(dirfd: Option<BorrowedFd<'_>>) -> i32 {
    dirfd.map_or(libc::AT_FDCWD, |fd| fd.as_raw_fd())
}

/// `openat(dirfd, path, flags, mode)`.
pub fn openat(dirfd: Option<BorrowedFd<'_>>, path: &[u8], flags: i32, mode: u32) -> Result<OwnedFd, Errno> {
    let path = to_cstring(path);
    // SAFETY: path is a valid C string.
    let fd = cvt(unsafe { libc::openat(raw_dirfd(dirfd), path.as_ptr(), flags, mode) })?;
    // SAFETY: openat just returned this fd, so we own it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The `struct stat` fields openrc-rs uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u64,
    pub size: i64,
    /// `st_mtime` (seconds).
    pub mtime: i64,
}

impl Stat {
    fn from_raw(st: &libc::stat) -> Stat {
        Stat {
            mode: st.st_mode,
            uid: st.st_uid,
            gid: st.st_gid,
            // nlink_t is u32 on some architectures.
            #[allow(clippy::unnecessary_cast)]
            nlink: st.st_nlink as u64,
            size: st.st_size,
            mtime: st.st_mtime,
        }
    }

    pub fn file_type(&self) -> u32 {
        self.mode & libc::S_IFMT
    }
    pub fn is_dir(&self) -> bool {
        self.file_type() == libc::S_IFDIR
    }
    pub fn is_reg(&self) -> bool {
        self.file_type() == libc::S_IFREG
    }
    pub fn is_fifo(&self) -> bool {
        self.file_type() == libc::S_IFIFO
    }
    pub fn is_lnk(&self) -> bool {
        self.file_type() == libc::S_IFLNK
    }
}

/// `fstat(fd)`.
pub fn fstat(fd: BorrowedFd<'_>) -> Result<Stat, Errno> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat writes a full struct stat on success.
    cvt(unsafe { libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) })?;
    // SAFETY: initialised by the successful fstat.
    let st = unsafe { st.assume_init() };
    Ok(Stat::from_raw(&st))
}

/// `fstatat(dirfd, path, flags)`; `stat(path)` is `fstatat(None, path, 0)`.
pub fn fstatat(dirfd: Option<BorrowedFd<'_>>, path: &[u8], flags: i32) -> Result<Stat, Errno> {
    let path = to_cstring(path);
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: path is a valid C string; fstatat writes a full struct stat on success.
    cvt(unsafe { libc::fstatat(raw_dirfd(dirfd), path.as_ptr(), st.as_mut_ptr(), flags) })?;
    // SAFETY: initialised by the successful fstatat.
    let st = unsafe { st.assume_init() };
    Ok(Stat::from_raw(&st))
}

/// `stat(path)`.
pub fn stat(path: &[u8]) -> Result<Stat, Errno> {
    fstatat(None, path, 0)
}

/// `mkdir(path, mode)`.
pub fn mkdir(path: &[u8], mode: u32) -> Result<(), Errno> {
    mkdirat(None, path, mode)
}

/// `unlinkat(dirfd, path, flags)`.
pub fn unlinkat(dirfd: Option<BorrowedFd<'_>>, path: &[u8], flags: i32) -> Result<(), Errno> {
    let path = to_cstring(path);
    // SAFETY: path is a valid C string.
    cvt(unsafe { libc::unlinkat(raw_dirfd(dirfd), path.as_ptr(), flags) }).map(drop)
}

/// `futimens(fd, NULL)`: set both times to now.
pub fn futimens_now(fd: BorrowedFd<'_>) -> Result<(), Errno> {
    // SAFETY: a null times pointer means "now".
    cvt(unsafe { libc::futimens(fd.as_raw_fd(), std::ptr::null()) }).map(drop)
}

/// `settimeofday({sec, 0}, NULL)`.
pub fn settimeofday(sec: i64) -> Result<(), Errno> {
    let tv = libc::timeval { tv_sec: sec, tv_usec: 0 };
    // SAFETY: tv is valid; the timezone pointer may be null.
    cvt(unsafe { libc::settimeofday(&tv, std::ptr::null()) }).map(drop)
}

/// `getuid()`.
pub fn getuid() -> u32 {
    // SAFETY: cannot fail.
    unsafe { libc::getuid() }
}

/// Keep `fd` open for the rest of the process (like librc's cached
/// directory fds, which are never closed).
pub fn leak_fd(fd: OwnedFd) -> BorrowedFd<'static> {
    use std::os::fd::IntoRawFd;
    let raw = fd.into_raw_fd();
    // SAFETY: we just gave up ownership and nothing will ever close it, so
    // it stays valid for 'static.
    unsafe { BorrowedFd::borrow_raw(raw) }
}

/// Entry names of the directory open on `fd` (`fdopendir` + `readdir`),
/// in readdir order, including `.` and `..`.  Consumes the fd.
pub fn readdir_names(fd: OwnedFd) -> Result<Vec<Vec<u8>>, Errno> {
    use std::os::fd::IntoRawFd;
    let raw = fd.into_raw_fd();
    // SAFETY: fdopendir takes ownership of raw on success.
    let dir = unsafe { libc::fdopendir(raw) };
    if dir.is_null() {
        let e = Errno::last();
        // SAFETY: fdopendir failed, so we still own raw.
        unsafe { libc::close(raw) };
        return Err(e);
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: dir is a valid DIR*; readdir returns null at the end.
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            break;
        }
        // SAFETY: d_name is a NUL-terminated array inside the dirent.
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        names.push(name.to_bytes().to_vec());
    }
    // SAFETY: dir is valid and closed exactly once (this closes raw too).
    unsafe { libc::closedir(dir) };
    Ok(names)
}

/// `mkdirat(dirfd, path, mode)`.
pub fn mkdirat(dirfd: Option<BorrowedFd<'_>>, path: &[u8], mode: u32) -> Result<(), Errno> {
    let path = to_cstring(path);
    // SAFETY: path is a valid C string.
    cvt(unsafe { libc::mkdirat(raw_dirfd(dirfd), path.as_ptr(), mode) }).map(drop)
}

/// `mkfifoat(dirfd, path, mode)`.
pub fn mkfifoat(dirfd: Option<BorrowedFd<'_>>, path: &[u8], mode: u32) -> Result<(), Errno> {
    let path = to_cstring(path);
    // SAFETY: path is a valid C string.
    cvt(unsafe { libc::mkfifoat(raw_dirfd(dirfd), path.as_ptr(), mode) }).map(drop)
}

/// `readlinkat(dirfd, path, buf, bufsize)`: returns the bytes read (at most
/// `bufsize`, no NUL terminator, possibly truncated).
pub fn readlinkat(fd: BorrowedFd<'_>, path: &[u8], bufsize: usize) -> Result<Vec<u8>, Errno> {
    let path = to_cstring(path);
    let mut buf = vec![0u8; bufsize];
    // SAFETY: buf is valid for bufsize bytes; path is a valid C string.
    let n = unsafe { libc::readlinkat(fd.as_raw_fd(), path.as_ptr(), buf.as_mut_ptr().cast(), bufsize) };
    if n < 0 {
        return Err(Errno::last());
    }
    buf.truncate(n as usize);
    Ok(buf)
}

/// `fchmod(fd, mode)`.
pub fn fchmod(fd: BorrowedFd<'_>, mode: u32) -> Result<(), Errno> {
    // SAFETY: plain syscall on an fd we borrow.
    cvt(unsafe { libc::fchmod(fd.as_raw_fd(), mode) }).map(drop)
}

/// `fchown(fd, uid, gid)`.
pub fn fchown(fd: BorrowedFd<'_>, uid: u32, gid: u32) -> Result<(), Errno> {
    // SAFETY: plain syscall on an fd we borrow.
    cvt(unsafe { libc::fchown(fd.as_raw_fd(), uid, gid) }).map(drop)
}

/// `umask(mask)`, returning the previous mask.
pub fn umask(mask: u32) -> u32 {
    // SAFETY: umask cannot fail.
    unsafe { libc::umask(mask) }
}

/// `access(path, mode)`.
pub fn access(path: &[u8], mode: i32) -> Result<(), Errno> {
    let path = to_cstring(path);
    // SAFETY: path is a valid C string.
    cvt(unsafe { libc::access(path.as_ptr(), mode) }).map(drop)
}

/// `geteuid()`.
pub fn geteuid() -> u32 {
    // SAFETY: cannot fail.
    unsafe { libc::geteuid() }
}

/// `getgid()`.
pub fn getgid() -> u32 {
    // SAFETY: cannot fail.
    unsafe { libc::getgid() }
}

/// The parts of `struct passwd` openrc-rs uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passwd {
    pub name: Vec<u8>,
    pub uid: u32,
    pub gid: u32,
    pub dir: Vec<u8>,
    pub shell: Vec<u8>,
}

/// # Safety
/// `pw` must be null or point to a valid `struct passwd`.
unsafe fn passwd_from(pw: *const libc::passwd) -> Option<Passwd> {
    // SAFETY: caller guarantees pw is null or valid.
    let pw = unsafe { pw.as_ref()? };
    let s = |p: *const libc::c_char| {
        // SAFETY: passwd string fields are valid C strings (or null).
        if p.is_null() { Vec::new() } else { unsafe { CStr::from_ptr(p) }.to_bytes().to_vec() }
    };
    Some(Passwd { name: s(pw.pw_name), uid: pw.pw_uid, gid: pw.pw_gid, dir: s(pw.pw_dir), shell: s(pw.pw_shell) })
}

/// `getpwnam(name)`.
pub fn getpwnam(name: &[u8]) -> Option<Passwd> {
    let name = to_cstring(name);
    // SAFETY: valid C string in; the result is copied out before any other
    // call can overwrite libc's static buffer (we are single-threaded).
    unsafe { passwd_from(libc::getpwnam(name.as_ptr())) }
}

/// `getpwuid(uid)`.
pub fn getpwuid(uid: u32) -> Option<Passwd> {
    // SAFETY: as for getpwnam.
    unsafe { passwd_from(libc::getpwuid(uid)) }
}

/// `getgrnam(name)->gr_gid`.
pub fn getgrnam(name: &[u8]) -> Option<u32> {
    let name = to_cstring(name);
    // SAFETY: valid C string in; only gr_gid is read from the result.
    unsafe { libc::getgrnam(name.as_ptr()).as_ref().map(|g| g.gr_gid) }
}

/// `getgrgid(gid)->gr_gid`.
pub fn getgrgid(gid: u32) -> Option<u32> {
    // SAFETY: only gr_gid is read from the result.
    unsafe { libc::getgrgid(gid).as_ref().map(|g| g.gr_gid) }
}

/// Terminal width from `ioctl(fd, TIOCGWINSZ)`.
pub fn term_cols(fd: i32) -> Option<u16> {
    let mut ws = std::mem::MaybeUninit::<libc::winsize>::uninit();
    // SAFETY: TIOCGWINSZ writes a struct winsize on success.
    let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, ws.as_mut_ptr()) } == 0;
    // SAFETY: initialised when the ioctl succeeded.
    ok.then(|| unsafe { ws.assume_init() }.ws_col)
}

/// `nanosleep()` for `ns` nanoseconds.  Returns false if interrupted
/// (no automatic restart, unlike `std::thread::sleep`).
pub fn nanosleep(ns: i64) -> bool {
    let ts = libc::timespec { tv_sec: ns / 1_000_000_000, tv_nsec: ns % 1_000_000_000 };
    // SAFETY: ts is valid; the remainder pointer may be null.
    unsafe { libc::nanosleep(&ts, std::ptr::null_mut()) == 0 }
}

/// `gettimeofday()` in microseconds.
pub fn gettimeofday_us() -> i64 {
    let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
    // SAFETY: tv is valid; the timezone pointer may be null.
    unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()) };
    tv.tv_sec * 1_000_000 + tv.tv_usec
}

/// `closelog(); openlog(ident, option, facility); syslog(priority, "%s", msg); closelog();`
pub fn syslog_once(ident: &[u8], option: i32, facility: i32, priority: i32, msg: &[u8]) {
    let ident = to_cstring(ident);
    let msg = to_cstring(msg);
    // SAFETY: ident stays alive until after closelog (openlog keeps the
    // pointer); "%s" with a valid C string is a well-formed format.
    unsafe {
        libc::closelog();
        libc::openlog(ident.as_ptr(), option, facility);
        libc::syslog(priority, c"%s".as_ptr(), msg.as_ptr());
        libc::closelog();
    }
}

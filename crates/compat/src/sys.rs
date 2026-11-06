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

/// A `struct mntent` (fields already decoded by libc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MntEnt {
    pub fsname: Vec<u8>,
    pub dir: Vec<u8>,
    pub fstype: Vec<u8>,
    pub opts: Vec<u8>,
    pub freq: i32,
    pub passno: i32,
}

/// All entries of an fstab-format file, via libc's
/// `setmntent`/`getmntent`/`endmntent` (so parsing matches C exactly).
/// `None` if the file cannot be opened.
pub fn read_mntent(path: &str) -> Option<Vec<MntEnt>> {
    let path = to_cstring(path.as_bytes());
    // SAFETY: valid C strings; getmntent's result is copied before the next
    // call, and the FILE is closed exactly once.
    unsafe {
        let fp = libc::setmntent(path.as_ptr(), c"r".as_ptr());
        if fp.is_null() {
            return None;
        }
        let mut out = Vec::new();
        loop {
            let ent = libc::getmntent(fp);
            let Some(e) = ent.as_ref() else { break };
            let s = |p: *const libc::c_char| CStr::from_ptr(p).to_bytes().to_vec();
            out.push(MntEnt {
                fsname: s(e.mnt_fsname),
                dir: s(e.mnt_dir),
                fstype: s(e.mnt_type),
                opts: s(e.mnt_opts),
                freq: e.mnt_freq,
                passno: e.mnt_passno,
            });
        }
        libc::endmntent(fp);
        Some(out)
    }
}

/// A compiled POSIX regex (`regcomp`/`regexec`/`regfree`).
pub struct Regex {
    re: Box<libc::regex_t>,
}

impl std::fmt::Debug for Regex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Regex")
    }
}

impl Regex {
    /// `regcomp(pattern, cflags)`; on failure the `regerror` text (as
    /// produced into a 256-byte buffer, like OpenRC's callers).
    pub fn new(pattern: &[u8], cflags: i32) -> Result<Regex, Vec<u8>> {
        let pat = to_cstring(pattern);
        // SAFETY: regex_t is plain data; regcomp initialises it.
        let mut re: Box<libc::regex_t> = Box::new(unsafe { std::mem::zeroed() });
        // SAFETY: valid pattern string and regex_t storage.
        let rc = unsafe { libc::regcomp(&mut *re, pat.as_ptr(), cflags) };
        if rc != 0 {
            let mut buf = [0 as libc::c_char; 256];
            // SAFETY: buffer valid for its length; regerror NUL-terminates.
            unsafe {
                libc::regerror(rc, &*re, buf.as_mut_ptr(), buf.len());
                return Err(CStr::from_ptr(buf.as_ptr()).to_bytes().to_vec());
            }
        }
        Ok(Regex { re })
    }

    /// `regexec(re, s, 0, NULL, 0) == 0` (s as a C string).
    pub fn is_match(&self, s: &[u8]) -> bool {
        let s = to_cstring(s);
        // SAFETY: compiled regex and valid C string; no match array.
        unsafe { libc::regexec(&*self.re, s.as_ptr(), 0, std::ptr::null_mut(), 0) == 0 }
    }
}

impl Drop for Regex {
    fn drop(&mut self) {
        // SAFETY: re was successfully compiled and is freed once.
        unsafe { libc::regfree(&mut *self.re) }
    }
}

/// `waitpid(pid, &status, flags)`: `(pid, status)`.
pub fn waitpid(pid: i32, flags: i32) -> Result<(i32, i32), Errno> {
    let mut status = 0;
    // SAFETY: status is a valid int.
    let r = unsafe { libc::waitpid(pid, &mut status, flags) };
    if r < 0 { Err(Errno::last()) } else { Ok((r, status)) }
}

/// `WIFEXITED(status) ? Some(WEXITSTATUS(status)) : None`.
pub fn exit_status(status: i32) -> Option<i32> {
    libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status))
}

/// `kill(pid, sig)`.
pub fn kill(pid: i32, sig: i32) -> Result<(), Errno> {
    // SAFETY: plain syscall.
    cvt(unsafe { libc::kill(pid, sig) }).map(drop)
}

/// `getpid()`.
pub fn getpid() -> i32 {
    // SAFETY: cannot fail.
    unsafe { libc::getpid() }
}

/// `getsid(pid)`.
pub fn getsid(pid: i32) -> Result<i32, Errno> {
    // SAFETY: plain syscall.
    cvt(unsafe { libc::getsid(pid) })
}

/// One `read(2)`, retried on nothing (like the C callers).
pub fn read_once(fd: BorrowedFd<'_>, buf: &mut [u8]) -> Result<usize, Errno> {
    // SAFETY: buf is valid for its length.
    let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 { Err(Errno::last()) } else { Ok(n as usize) }
}

fn clock_ms(clock: libc::clockid_t) -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: ts is valid.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    // timeutils.c ts_to_ms(): rounds to the nearest millisecond.
    ts.tv_sec * 1000 + (ts.tv_nsec + 500_000) / 1_000_000
}

/// `tm_now()` (`shared/timeutils.c`): CLOCK_BOOTTIME in ms (Linux's
/// CLOCK_MONOTONIC that keeps counting during suspend).
pub fn tm_now() -> i64 {
    clock_ms(libc::CLOCK_BOOTTIME)
}

/// `tm_sleep(ms, 0)`.  QUIRK (upstream): clock_nanosleep returns its
/// error rather than -1, so an EINTR is never reported and the sleep is
/// always resumed until complete.
pub fn tm_sleep(ms: i64) {
    let mut tv = libc::timespec { tv_sec: ms / 1000, tv_nsec: (ms % 1000) * 1_000_000 };
    loop {
        let mut rem = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: valid timespecs.
        let ret = unsafe { libc::clock_nanosleep(libc::CLOCK_BOOTTIME, 0, &tv, &mut rem) };
        if ret == 0 {
            break;
        }
        tv = rem;
    }
}

/// `realpath(path, NULL)`.
pub fn realpath(path: &[u8]) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    std::fs::canonicalize(crate::bytes::as_path(path)).ok().map(|p| p.as_os_str().as_bytes().to_vec())
}

/// `flock(fd, op)`.
pub fn flock(fd: BorrowedFd<'_>, op: i32) -> Result<(), Errno> {
    // SAFETY: plain syscall.
    cvt(unsafe { libc::flock(fd.as_raw_fd(), op) }).map(drop)
}

/// `fsync(fd)`.
pub fn fsync(fd: BorrowedFd<'_>) -> Result<(), Errno> {
    // SAFETY: plain syscall.
    cvt(unsafe { libc::fsync(fd.as_raw_fd()) }).map(drop)
}

/// `renameat(dirfd, old, dirfd, new)` within one directory.
pub fn renameat(dir: BorrowedFd<'_>, old: &[u8], new: &[u8]) -> Result<(), Errno> {
    let (old, new) = (to_cstring(old), to_cstring(new));
    // SAFETY: valid C strings.
    cvt(unsafe { libc::renameat(dir.as_raw_fd(), old.as_ptr(), dir.as_raw_fd(), new.as_ptr()) }).map(drop)
}

/// One `getrandom(buf, flags)` call.
pub fn getrandom(buf: &mut [u8], flags: u32) -> Result<usize, Errno> {
    // SAFETY: buf is valid for its length.
    let n = unsafe { libc::getrandom(buf.as_mut_ptr().cast(), buf.len(), flags) };
    if n < 0 { Err(Errno::last()) } else { Ok(n as usize) }
}

/// One `write(2)`.
pub fn write_once(fd: BorrowedFd<'_>, buf: &[u8]) -> Result<usize, Errno> {
    // SAFETY: buf is valid for its length.
    let n = unsafe { libc::write(fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
    if n < 0 { Err(Errno::last()) } else { Ok(n as usize) }
}

/// `poll()` on one fd for `events` with timeout 0: the C return value.
pub fn poll_now(fd: BorrowedFd<'_>, events: i16) -> i32 {
    let mut p = libc::pollfd { fd: fd.as_raw_fd(), events, revents: 0 };
    // SAFETY: one valid pollfd.
    unsafe { libc::poll(&mut p, 1, 0) }
}

/// `ioctl(fd, RNDADDENTROPY, {entropy_count, buf_size, buffer})`.
pub fn rnd_add_entropy(fd: BorrowedFd<'_>, entropy_bits: i32, data: &[u8]) -> Result<(), Errno> {
    // struct rand_pool_info as laid out by seedrng.c (buffer[MAX_SEED_LEN]).
    #[repr(C)]
    struct Req {
        entropy_count: i32,
        buf_size: i32,
        buffer: [u8; 512],
    }
    // _IOW('R', 0x03, int[2])
    const RNDADDENTROPY: libc::c_ulong = 0x4008_5203;
    let mut req = Req { entropy_count: entropy_bits, buf_size: data.len() as i32, buffer: [0; 512] };
    req.buffer[..data.len()].copy_from_slice(data);
    // SAFETY: req is a valid rand_pool_info with buf_size bytes of buffer.
    cvt(unsafe { libc::ioctl(fd.as_raw_fd(), RNDADDENTROPY, &req) }).map(drop)
}

/// `clock_gettime(clock)` as the raw `struct timespec` bytes (seedrng
/// hashes the struct's memory).
pub fn timespec_bytes(clock: libc::clockid_t) -> [u8; 16] {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: ts is valid.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&ts.tv_sec.to_ne_bytes());
    out[8..].copy_from_slice(&ts.tv_nsec.to_ne_bytes());
    out
}

/// `chdir(path)`.
pub fn chdir(path: &[u8]) -> Result<(), Errno> {
    let path = to_cstring(path);
    // SAFETY: valid C string.
    cvt(unsafe { libc::chdir(path.as_ptr()) }).map(drop)
}

/// Block every signal; returns the previous mask for [`restore_sigmask`].
pub fn block_all_signals() -> libc::sigset_t {
    // SAFETY: sigset_t is plain data, initialised by sigfillset/sigprocmask.
    unsafe {
        let mut full: libc::sigset_t = std::mem::zeroed();
        let mut old: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut full);
        libc::sigemptyset(&mut old);
        libc::sigprocmask(libc::SIG_SETMASK, &full, &mut old);
        old
    }
}

/// Restore a mask saved by [`block_all_signals`].
pub fn restore_sigmask(old: &libc::sigset_t) {
    // SAFETY: old is a valid sigset_t.
    unsafe { libc::sigprocmask(libc::SIG_SETMASK, old, std::ptr::null_mut()) };
}

/// `openlog(ident, option, facility)`, keeping `ident` alive for good
/// (syslog keeps the pointer).
pub fn openlog(ident: &[u8], option: i32, facility: i32) {
    let ident: &'static std::ffi::CStr = Box::leak(to_cstring(ident).into_boxed_c_str());
    // SAFETY: ident lives forever.
    unsafe { libc::openlog(ident.as_ptr(), option, facility) }
}

/// `syslog(priority, "%s", msg)` with the current openlog settings.
pub fn syslog(priority: i32, msg: &[u8]) {
    let msg = to_cstring(msg);
    // SAFETY: well-formed format and argument.
    unsafe { libc::syslog(priority, c"%s".as_ptr(), msg.as_ptr()) }
}

/// `*at()` calls on a raw directory fd, exactly as C makes them: -1 (or
/// any bad fd) fails with EBADF, `libc::AT_FDCWD` means the cwd.  For
/// ports of code that passes unchecked `rc_dirfd()` results along.
pub mod at {
    use std::os::fd::{FromRawFd, OwnedFd};

    use super::{Errno, Stat, cvt, libc, to_cstring};

    pub fn faccessat(dirfd: i32, path: &[u8], mode: i32) -> Result<(), Errno> {
        let p = to_cstring(path);
        // SAFETY: valid C string; any int is acceptable as dirfd.
        cvt(unsafe { libc::faccessat(dirfd, p.as_ptr(), mode, 0) }).map(drop)
    }

    pub fn fstatat(dirfd: i32, path: &[u8], flags: i32) -> Result<Stat, Errno> {
        let p = to_cstring(path);
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: valid C string; fstatat writes a full struct stat on success.
        cvt(unsafe { libc::fstatat(dirfd, p.as_ptr(), st.as_mut_ptr(), flags) })?;
        // SAFETY: initialised by the successful fstatat.
        Ok(Stat::from_raw(&unsafe { st.assume_init() }))
    }

    pub fn unlinkat(dirfd: i32, path: &[u8], flags: i32) -> Result<(), Errno> {
        let p = to_cstring(path);
        // SAFETY: valid C string.
        cvt(unsafe { libc::unlinkat(dirfd, p.as_ptr(), flags) }).map(drop)
    }

    pub fn symlinkat(target: &[u8], dirfd: i32, path: &[u8]) -> Result<(), Errno> {
        let (t, p) = (to_cstring(target), to_cstring(path));
        // SAFETY: valid C strings.
        cvt(unsafe { libc::symlinkat(t.as_ptr(), dirfd, p.as_ptr()) }).map(drop)
    }

    pub fn mkdirat(dirfd: i32, path: &[u8], mode: u32) -> Result<(), Errno> {
        let p = to_cstring(path);
        // SAFETY: valid C string.
        cvt(unsafe { libc::mkdirat(dirfd, p.as_ptr(), mode) }).map(drop)
    }

    pub fn openat(dirfd: i32, path: &[u8], flags: i32, mode: u32) -> Result<OwnedFd, Errno> {
        let p = to_cstring(path);
        // SAFETY: valid C string.
        let fd = cvt(unsafe { libc::openat(dirfd, p.as_ptr(), flags, mode) })?;
        // SAFETY: openat just returned this fd.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// `readlinkat()`, growing the buffer until the target fits (librc's
    /// `safe_readlink`).
    pub fn readlinkat(dirfd: i32, path: &[u8], mut bufsiz: usize) -> Result<Vec<u8>, Errno> {
        let p = to_cstring(path);
        loop {
            let mut buf = vec![0u8; bufsiz];
            // SAFETY: buf is valid for bufsiz bytes.
            let n = unsafe { libc::readlinkat(dirfd, p.as_ptr(), buf.as_mut_ptr().cast(), bufsiz) };
            if n < 0 {
                return Err(Errno::last());
            }
            if (n as usize) < bufsiz {
                buf.truncate(n as usize);
                return Ok(buf);
            }
            bufsiz += libc::PATH_MAX as usize;
        }
    }

    /// `dup(dirfd)` + `fdopendir` + `readdir`: entry names in readdir
    /// order, including `.` and `..` (librc's `do_dopendir`).
    pub fn readdir_dup(dirfd: i32) -> Result<Vec<Vec<u8>>, Errno> {
        // SAFETY: dup on any int is safe; failure is reported.
        let fd = cvt(unsafe { libc::dup(dirfd) })?;
        // SAFETY: we own the new fd.
        super::readdir_names(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// `do_opendirat(dirfd, path)` + `readdir`.
    pub fn readdir_at(dirfd: i32, path: &[u8]) -> Result<Vec<Vec<u8>>, Errno> {
        super::readdir_names(openat(dirfd, path, libc::O_RDONLY | libc::O_DIRECTORY, 0)?)
    }
}

/// Set `errno` (for C ABI functions whose callers inspect it).
pub fn set_errno(e: i32) {
    // SAFETY: errno is thread-local and always writable.
    unsafe { *libc::__errno_location() = e }
}

/// `symlink(target, path)` and `unlink(path)` relative to the cwd.
pub fn symlink(target: &[u8], path: &[u8]) -> Result<(), Errno> {
    at::symlinkat(target, libc::AT_FDCWD, path)
}
pub fn unlink(path: &[u8]) -> Result<(), Errno> {
    at::unlinkat(libc::AT_FDCWD, path, 0)
}

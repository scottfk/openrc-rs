//! `checkpath`: port of `src/checkpath/checkpath.c`.
//!
//! Checks that a file, directory or FIFO exists, creating it if necessary,
//! and corrects its mode and owner.  Paths are opened component by
//! component from `/` without following symlinks (except root-owned ones
//! in the parent path), so it is safe to run on directories writable by
//! unprivileged users.

use std::os::fd::{AsFd, OwnedFd};

use openrc_compat::bcat;
use openrc_compat::bytes::basename_c;
use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::num::scanf_int;
use openrc_compat::sys::{self, Errno, libc};
use openrc_einfo::{eerror, eerrorx, einfo};

use crate::shared::{Usage, getoptstring_common, parse_mode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inode {
    Unknown,
    File,
    Dir,
    Fifo,
}

const GETOPTSTRING: &str = concat!("dDfFpm:o:sW", getoptstring_common!());

const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("directory", HasArg::No, b'd'),
    LongOpt::new("directory-truncate", HasArg::No, b'D'),
    LongOpt::new("file", HasArg::No, b'f'),
    LongOpt::new("file-truncate", HasArg::No, b'F'),
    LongOpt::new("pipe", HasArg::No, b'p'),
    LongOpt::new("mode", HasArg::Required, b'm'),
    LongOpt::new("owner", HasArg::Required, b'o'),
    LongOpt::new("symlinks", HasArg::No, b's'),
    LongOpt::new("writable", HasArg::No, b'W'),
];

const LONGOPTS_HELP: &[&str] = &[
    "Create a directory if not exists",
    "Create/empty directory",
    "Create a file if not exists",
    "Truncate file",
    "Create a named pipe (FIFO) if not exists",
    "Mode to check",
    "Owner to check (user:group)",
    "follow symbolic links (irrelevant on linux)",
    "Check whether the path is writable or not",
];

fn errstr(e: Errno) -> Vec<u8> {
    e.strerror()
}

/// `get_dirfd()`: open the parent directory of `path`, walking from `/`
/// one component at a time.  Exits on any error.
fn get_dirfd(applet: &[u8], path: &[u8], symlinks: bool) -> OwnedFd {
    if path.first() != Some(&b'/') {
        eerrorx(Some(&bcat!(applet, ": empty or relative path")));
    }
    let mut dirfd = match sys::openat(None, b"/", libc::O_RDONLY, 0) {
        Ok(fd) => fd,
        Err(e) => eerrorx(Some(&bcat!(applet, ": unable to open the root directory: ", errstr(e)))),
    };
    let mut components = path.iter().filter(|&&b| b == b'/').count();
    let mut items = path.split(|&b| b == b'/').filter(|s| !s.is_empty());
    let mut item = items.next();
    let mut flags = libc::O_PATH | libc::O_RDONLY;
    if !symlinks {
        flags |= libc::O_NOFOLLOW;
    }
    let mut linkpath: Option<Vec<u8>> = None;

    while let Some(it) = item {
        if components <= 1 {
            break;
        }
        let s: &[u8] = linkpath.as_deref().unwrap_or(it);
        let new_dirfd = match sys::openat(Some(dirfd.as_fd()), s, flags, 0) {
            Ok(fd) => fd,
            Err(e) => eerrorx(Some(&bcat!(applet, ": ", path, ": could not open ", s, ": ", errstr(e)))),
        };
        let st = match sys::fstat(new_dirfd.as_fd()) {
            Ok(st) => st,
            Err(e) => eerrorx(Some(&bcat!(applet, ": ", path, ": unable to stat ", it, ": ", errstr(e)))),
        };
        if st.is_lnk() {
            if st.uid != 0 {
                eerrorx(Some(&bcat!(applet, ": ", path, ": symbolic link ", s, " not owned by root")));
            }
            let size = st.size.max(0) as usize;
            match sys::readlinkat(new_dirfd.as_fd(), b"", size + 1) {
                Ok(target) if target.len() == size => linkpath = Some(target),
                _ => eerrorx(Some(&bcat!(applet, ": symbolic link destination changed"))),
            }
            // Now follow the symlink (QUIRK: a root-owned symlink loop
            // spins forever, as upstream).
        } else {
            // Walk down the directory path.
            dirfd = new_dirfd;
            linkpath = None;
            item = items.next();
            components -= 1;
        }
    }
    dirfd
}

/// `clean_path()`: squeeze repeated slashes and drop trailing ones.
fn clean_path(path: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(path.len());
    for &b in openrc_compat::bytes::c_str_prefix(path) {
        if b == b'/' && out.last() == Some(&b'/') {
            continue;
        }
        out.push(b);
    }
    while out.len() > 1 && out.last() == Some(&b'/') {
        out.pop();
    }
    out
}

/// `do_create()`.  QUIRK: the umask is not restored if mkdirat/mkfifoat
/// fails (harmless: later creates set it to 0 again).
fn do_create(
    applet: &[u8],
    ty: Inode,
    path: &[u8],
    dirfd: &OwnedFd,
    name: &[u8],
    mut flags: i32,
    mut mode: u32,
) -> Option<OwnedFd> {
    let mask = sys::umask(0);
    if mode == 0 {
        mode = match ty {
            Inode::File => 0o664,
            Inode::Dir => 0o775,
            Inode::Fifo => 0o600,
            Inode::Unknown => 0,
        };
    }

    match ty {
        Inode::File => {
            einfo(Some(&bcat!(path, ": creating file")));
            flags |= libc::O_CREAT;
        }
        Inode::Dir => {
            einfo(Some(&bcat!(path, ": creating directory")));
            // We do not recursively create parents.
            if let Err(e) = sys::mkdirat(Some(dirfd.as_fd()), name, mode)
                && e.0 != libc::EEXIST
            {
                eerror(Some(&bcat!(applet, ": mkdirat: ", errstr(e))));
                return None;
            }
        }
        Inode::Fifo => {
            einfo(Some(&bcat!(path, ": creating fifo")));
            if let Err(e) = sys::mkfifoat(Some(dirfd.as_fd()), name, mode)
                && e.0 != libc::EEXIST
            {
                eerror(Some(&bcat!(applet, ": mkfifo: ", errstr(e))));
                return None;
            }
        }
        Inode::Unknown => {}
    }
    sys::umask(mask);

    match sys::openat(Some(dirfd.as_fd()), name, flags, mode) {
        Ok(fd) => Some(fd),
        Err(e) => {
            eerror(Some(&bcat!(applet, ": open: ", errstr(e))));
            None
        }
    }
}

struct Check {
    uid: u32,
    gid: u32,
    mode: u32,
    ty: Inode,
    trunc: bool,
    chowner: bool,
    writable: bool,
    symlinks: bool,
}

/// `do_check()`: returns true on success.
fn do_check(applet: &[u8], path: &[u8], c: &Check) -> bool {
    let mut flags = libc::O_NDELAY | libc::O_NOCTTY | libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let name = basename_c(path);

    if c.writable {
        match sys::access(path, libc::W_OK) {
            // QUIRK: an existing writable path is accepted as-is: no type,
            // mode or owner checks.
            Ok(()) => return true,
            Err(e) if e.0 != libc::ENOENT || c.ty == Inode::Unknown => return false,
            Err(_) => {}
        }
    }

    // QUIRK: -D ("Create/empty directory") does not empty anything; trunc
    // only applies to files.
    if c.trunc && c.ty == Inode::File {
        flags |= libc::O_TRUNC;
    }

    let dirfd = get_dirfd(applet, path, c.symlinks);
    let fd = match sys::openat(Some(dirfd.as_fd()), name, flags, 0) {
        Ok(fd) => fd,
        Err(_) => match do_create(applet, c.ty, path, &dirfd, name, flags, c.mode) {
            Some(fd) => fd,
            None => return false,
        },
    };

    let st = match sys::fstat(fd.as_fd()) {
        Ok(st) => st,
        Err(e) => {
            eerror(Some(&bcat!("fstat: ", path, ": ", errstr(e))));
            return false;
        }
    };

    if c.ty != Inode::Dir && st.is_dir() {
        eerror(Some(&bcat!(path, ": is a directory")));
        return false;
    }
    if c.ty != Inode::File && st.is_reg() {
        eerror(Some(&bcat!(path, ": is a file")));
        return false;
    }
    if c.ty != Inode::Fifo && st.is_fifo() {
        eerror(Some(&bcat!(path, ": is a fifo")));
        return false;
    }

    if c.mode != 0 && (st.mode & 0o7777) != c.mode {
        if c.ty != Inode::Dir && st.nlink > 1 {
            eerror(Some(&bcat!(applet, ": chmod: Too many hard links to ", path)));
            return false;
        }
        if st.is_lnk() {
            eerror(Some(&bcat!(applet, ": chmod: ", path, " is a symbolic link")));
            return false;
        }
        einfo(Some(&bcat!(path, ": correcting mode")));
        if let Err(e) = sys::fchmod(fd.as_fd(), c.mode) {
            eerror(Some(&bcat!(applet, ": chmod: ", errstr(e))));
            return false;
        }
    }

    if c.chowner && (st.uid != c.uid || st.gid != c.gid) {
        if c.ty != Inode::Dir && st.nlink > 1 {
            eerror(Some(&bcat!(applet, ": chown: Too many hardlinks to ", path)));
            return false;
        }
        if st.is_lnk() {
            eerror(Some(&bcat!(applet, ": chown: ", path, " is a symbolic link")));
            return false;
        }
        einfo(Some(&bcat!(path, ": correcting owner")));
        if let Err(e) = sys::fchown(fd.as_fd(), c.uid, c.gid) {
            eerror(Some(&bcat!(applet, ": chown: ", errstr(e))));
            return false;
        }
    }

    // TODO(selinux): selinux_util_label(path) when built with SELinux.
    true
}

/// `parse_owner()`: `user`, `user:group`, `:group`, names or numeric ids.
/// Returns false if a given user or group does not exist.
fn parse_owner(user: &mut Option<sys::Passwd>, group: &mut Option<u32>, owner: &[u8]) -> bool {
    let (u, g) = match owner.iter().position(|&b| b == b':') {
        Some(i) => (&owner[..i], Some(&owner[i + 1..])),
        None => (owner, None),
    };
    let mut ok = true;
    if !u.is_empty() {
        *user = match scanf_int(u) {
            Some(id) => sys::getpwuid(id as u32),
            None => sys::getpwnam(u),
        };
        ok &= user.is_some();
    }
    if let Some(g) = g.filter(|g| !g.is_empty()) {
        *group = match scanf_int(g) {
            Some(id) => sys::getgrgid(id as u32),
            None => sys::getgrnam(g),
        };
        ok &= group.is_some();
    }
    ok
}

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let argc = args.len();
    let usage = Usage {
        applet: applet.to_vec(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: Some("path1 [path2] [...]"),
        usagestring: None,
    };
    let longopts = usage.all_longopts();

    let mut c = Check {
        // QUIRK: the default group is the real gid, the user the effective uid.
        uid: sys::geteuid(),
        gid: sys::getgid(),
        mode: 0,
        ty: Inode::Unknown,
        trunc: false,
        chowner: false,
        writable: false,
        symlinks: false,
    };
    let mut pw: Option<sys::Passwd> = None;
    let mut gr: Option<u32> = None;

    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        let optarg = g.optarg.clone().unwrap_or_default();
        match u8::try_from(opt).unwrap_or(0) {
            b'D' => {
                c.trunc = true;
                c.ty = Inode::Dir;
            }
            b'd' => c.ty = Inode::Dir,
            b'F' => {
                c.trunc = true;
                c.ty = Inode::File;
            }
            b'f' => c.ty = Inode::File,
            b'p' => c.ty = Inode::Fifo,
            b'm' => match parse_mode(&optarg) {
                Some(m) => c.mode = m,
                None => eerrorx(Some(&bcat!(applet, ": invalid mode '", optarg, "'"))),
            },
            b'o' => {
                c.chowner = true;
                if !parse_owner(&mut pw, &mut gr, &optarg) {
                    eerrorx(Some(&bcat!(applet, ": owner '", optarg, "' not found")));
                }
            }
            // O_PATH exists on Linux, so -s changes nothing.
            b's' => {}
            b'W' => c.writable = true,
            _ => usage.common_option(opt, argc),
        }
    }

    if g.optind >= g.args.len() {
        usage.usage(1);
    }

    if let Some(pw) = &pw {
        c.uid = pw.uid;
        c.gid = pw.gid;
    }
    if let Some(gid) = gr {
        c.gid = gid;
    }

    let mut retval = 0;
    for arg in &g.args[g.optind..] {
        let path = clean_path(arg);
        if !do_check(applet, &path, &c) {
            retval = 1;
        }
    }
    retval
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_paths() {
        assert_eq!(clean_path(b"//a///b//"), b"/a/b");
        assert_eq!(clean_path(b"/"), b"/");
        assert_eq!(clean_path(b"///"), b"/");
        assert_eq!(clean_path(b""), b"");
        assert_eq!(clean_path(b"a/"), b"a");
    }
}

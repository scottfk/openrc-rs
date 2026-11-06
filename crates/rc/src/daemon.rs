//! Daemon tracking: port of `src/librc/librc-daemon.c` (Linux paths).
//!
//! `start-stop-daemon` and `supervise-daemon` record each daemon a service
//! starts in `$svcdir/daemons/<service>/NNN` (`exec=`, `argv_N=`,
//! `pidfile=` lines).  These functions maintain those records, match
//! against them, and use them (with /proc) to tell whether a started
//! service's daemons have crashed.
//!
//! errno matters here: `rc_service_state()` ignores a crash report when
//! errno is EACCES afterwards, and errno is whatever the last failing
//! system call left, e.g. a pidfile we may not read, or another user's
//! /proc entry.  So the port makes the same fallible calls, through libc,
//! in the same order as C, and callers read errno after it.

use std::os::fd::AsRawFd;

use openrc_compat::bcat;
use openrc_compat::bytes::{basename_c, c_str_prefix};
use openrc_compat::num::scanf_int;
use openrc_compat::sys::{self, at, libc};

use crate::svcdir::{RcDir, rc_dirfd_raw, rc_svcdir};

/// Lines of a file as `xgetline` returns them (C strings, no newline).
fn lines(data: &[u8]) -> Vec<&[u8]> {
    let mut v: Vec<&[u8]> = data.split(|&b| b == b'\n').map(c_str_prefix).collect();
    if data.ends_with(b"\n") || data.is_empty() {
        v.pop();
    }
    v
}

/// Read a whole file opened relative to `dirfd` (`do_fopenat` + getline).
fn read_at(dirfd: i32, path: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let fd = at::openat(dirfd, path, libc::O_RDONLY, 0o666).ok()?;
    let mut data = Vec::new();
    let _ = std::fs::File::from(fd).read_to_end(&mut data);
    Some(data)
}

/// `pid_is_exec()`: does /proc/<pid>/stat's comm field equal basename(exec)?
/// QUIRK: the kernel truncates comm to 15 bytes, so longer names never match.
fn pid_is_exec(pid: i32, exec: &[u8]) -> bool {
    let exec = basename_c(exec);
    let Some(data) = read_at(libc::AT_FDCWD, format!("/proc/{pid}/stat").as_bytes()) else { return false };
    let Some(open) = data.iter().position(|&b| b == b'(') else { return false };
    // Compare byte by byte after '(' while they match exec.
    let rest = &data[open + 1..];
    let mut i = 0;
    while i < rest.len() && i < exec.len() && rest[i] == exec[i] {
        i += 1;
    }
    i == exec.len() && rest.get(i) == Some(&b')')
}

/// `pid_is_argv()`: does /proc/<pid>/cmdline start with `argv`?  C reads
/// it once into a PATH_MAX buffer.
fn pid_is_argv(pid: i32, argv: &[Vec<u8>]) -> bool {
    let Ok(fd) = at::openat(libc::AT_FDCWD, format!("/proc/{pid}/cmdline").as_bytes(), libc::O_RDONLY, 0) else {
        return false;
    };
    let size = libc::PATH_MAX as usize;
    let mut buf = vec![0u8; size];
    let Ok(n) = sys::read_once(std::os::fd::AsFd::as_fd(&fd), &mut buf[..size - 1]) else { return false };
    buf[n] = 0;
    let mut p = 0;
    for a in argv {
        if p >= size {
            return false;
        }
        let s = c_str_prefix(&buf[p..]);
        if s != c_str_prefix(a) {
            return false;
        }
        p += s.len() + 1;
    }
    true
}

/// A fixed `char buf[30] = {0}` filled by `readlink(path, buf, 29)`, which
/// does not NUL-terminate: stale bytes past a shorter link survive.
fn readlink_into(buf: &mut [u8; 30], path: &[u8]) {
    let p = openrc_compat::bytes::to_cstring(path);
    // SAFETY: buf has room for 29 bytes plus the never-written last byte.
    let rc = unsafe { libc::readlink(p.as_ptr(), buf.as_mut_ptr().cast(), buf.len() - 1) };
    if rc <= 0 {
        buf[0] = 0;
    }
}

/// `rc_find_pids()`: pids matching `exec` (by comm), `argv` (by cmdline
/// prefix), `uid` (owner of /proc/<pid>) and `pid`, each if given.  In
/// readdir order (the C list inserts at the head, so the C ABI reverses
/// it).  `None` if nothing matched or /proc can't be read.
pub fn rc_find_pids(exec: Option<&[u8]>, argv: Option<&[Vec<u8>]>, uid: u32, pid: i32) -> Option<Vec<i32>> {
    let proc_fd = at::openat(libc::AT_FDCWD, b"/proc", libc::O_RDONLY | libc::O_DIRECTORY, 0).ok()?;

    // Never match RC_OPENRC_PID: `/etc/init.d/ntpd stop` runs
    // `start-stop-daemon --stop --name ntpd`, which would find itself.
    let openrc_pid = openrc_compat::env::get("RC_OPENRC_PID").and_then(|v| scanf_int(&v)).unwrap_or(0);

    // An OpenVZ host ("envID:\t0") must skip processes inside containers.
    let openvz_host = read_at(libc::AT_FDCWD, b"/proc/self/status")
        .is_some_and(|d| lines(&d).iter().any(|l| l.starts_with(b"envID:\t0")));

    let mut my_ns = [0u8; 30];
    readlink_into(&mut my_ns, b"/proc/self/ns/pid");
    let my_ns_s = c_str_prefix(&my_ns).to_vec();
    let mut proc_ns = [0u8; 30];
    // QUIRK: never reset per process, so after one container process on
    // an OpenVZ host, every later process is skipped too.
    let mut container_pid = false;
    let mut pids: Option<Vec<i32>> = None;

    for name in at::readdir_dup(proc_fd.as_raw_fd()).ok()? {
        let Some(p) = scanf_int(&name) else { continue };
        if openrc_pid != 0 && openrc_pid == p {
            continue;
        }
        if pid != 0 && pid != p {
            continue;
        }
        readlink_into(&mut proc_ns, format!("/proc/{p}/ns/pid").as_bytes());
        let proc_ns_s = c_str_prefix(&proc_ns);
        if pid == 0 && !my_ns_s.is_empty() && !proc_ns_s.is_empty() && my_ns_s != proc_ns_s {
            continue;
        }
        if uid != 0 && !sys::stat(format!("/proc/{p}").as_bytes()).is_ok_and(|st| st.uid == uid) {
            continue;
        }
        if let Some(e) = exec
            && !pid_is_exec(p, e)
        {
            continue;
        }
        if let Some(a) = argv
            && !pid_is_argv(p, a)
        {
            continue;
        }
        if openvz_host {
            let Some(d) = read_at(libc::AT_FDCWD, format!("/proc/{p}/status").as_bytes()) else { continue };
            if let Some(l) = lines(&d).into_iter().find(|l| l.starts_with(b"envID:")) {
                container_pid = !l.starts_with(b"envID:\t0");
            }
        }
        if container_pid {
            continue;
        }
        pids.get_or_insert_with(Vec::new).push(p);
    }
    pids
}

/// `_match_list()`: the record lines a daemon must have.
fn match_list(exec: Option<&[u8]>, argv: Option<&[Vec<u8>]>, pidfile: Option<&[u8]>) -> Vec<Vec<u8>> {
    let mut m = Vec::new();
    if let Some(e) = exec {
        m.push(bcat!("exec=", e));
    }
    // QUIRK: every argument is listed as "argv_0=", while records number
    // them argv_0, argv_1, ...: only the first argument can ever match.
    for a in argv.unwrap_or_default() {
        m.push(bcat!("argv_0=", a));
    }
    if let Some(p) = pidfile {
        m.push(bcat!("pidfile=", p));
    }
    m
}

/// `_match_daemon()`: does `daemons/<svc>/<instance>` contain every line
/// in `wanted`?  QUIRK: matched lines are *removed* from `wanted`, and
/// callers reuse the list across records, so a partial match on one record
/// makes a later record need only the remaining lines.
fn match_daemon(svc: &[u8], instance: &[u8], wanted: &mut Vec<Vec<u8>>) -> bool {
    let Some(data) = read_at(rc_dirfd_raw(RcDir::Daemons), &bcat!(svc, "/", instance)) else { return false };
    for line in lines(&data) {
        if let Some(i) = wanted.iter().position(|m| m == line) {
            wanted.remove(i);
        }
        if wanted.is_empty() {
            break;
        }
    }
    wanted.is_empty()
}

/// `rc_service_daemon_set()`: forget any record of this daemon (renumbering
/// later records down), and if `started`, record it as the next NNN.
/// Err(EINVAL) if neither exec nor pidfile is given.
pub fn rc_service_daemon_set(
    service: &[u8],
    exec: Option<&[u8]>,
    argv: Option<&[Vec<u8>]>,
    pidfile: Option<&[u8]>,
    started: bool,
) -> Result<bool, i32> {
    if exec.is_none() && pidfile.is_none() {
        return Err(libc::EINVAL);
    }
    let base = basename_c(service);
    let mut nfiles: i32 = 0;

    // Regardless, erase any existing daemon info.
    if let Ok(names) = at::readdir_at(rc_dirfd_raw(RcDir::Daemons), base) {
        let mut wanted = match_list(exec, argv, pidfile);
        let mut renamelist: Vec<Vec<u8>> = Vec::new();
        let mut oldfile: Vec<u8> = Vec::new();
        for d in names {
            if d.first() == Some(&b'.') {
                continue;
            }
            let file = bcat!(rc_svcdir(), "/daemons/", base, "/", d);
            if renamelist.contains(&file) {
                continue;
            }
            nfiles += 1;
            if oldfile.is_empty() {
                if match_daemon(base, &d, &mut wanted) {
                    let _ = sys::unlink(&file);
                    oldfile = file;
                    nfiles -= 1;
                }
            } else {
                // Shift later records down into the freed name.
                let (f, o) = (openrc_compat::bytes::to_cstring(&file), openrc_compat::bytes::to_cstring(&oldfile));
                // SAFETY: valid C strings.
                unsafe { libc::rename(f.as_ptr(), o.as_ptr()) };
                oldfile = file;
                renamelist.push(oldfile.clone());
            }
        }
    }

    // Now store our daemon info.
    if !started {
        return Ok(true);
    }
    match at::mkdirat(rc_dirfd_raw(RcDir::Daemons), base, 0o755) {
        Err(e) if e.0 != libc::EEXIST => return Ok(false),
        _ => {}
    }
    let file = bcat!(rc_svcdir(), "/daemons/", base, "/", format!("{:03}", nfiles + 1));
    let Ok(fd) = at::openat(libc::AT_FDCWD, &file, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o666) else {
        return Ok(false);
    };
    let mut rec = bcat!("exec=", exec.map(c_str_prefix).unwrap_or_default());
    for (i, a) in argv.unwrap_or_default().iter().enumerate() {
        rec.extend_from_slice(&bcat!(format!("\nargv_{i}="), c_str_prefix(a)));
    }
    rec.extend_from_slice(&bcat!("\npidfile=", pidfile.map(c_str_prefix).unwrap_or_default(), "\n"));
    sys::write_all_fd(std::os::fd::AsFd::as_fd(&fd), &rec);
    Ok(true)
}

/// `rc_service_started_daemon()`: is this daemon recorded for the service
/// (in record `indx`, or any record if `indx <= 0`)?
pub fn rc_service_started_daemon(service: &[u8], exec: &[u8], argv: Option<&[Vec<u8>]>, indx: i32) -> bool {
    let base = basename_c(service);
    let mut wanted = match_list(Some(exec), argv, None);
    if indx > 0 {
        return match_daemon(base, format!("{indx:03}").as_bytes(), &mut wanted);
    }
    let Ok(names) = at::readdir_at(rc_dirfd_raw(RcDir::Daemons), base) else { return false };
    for d in names {
        if d.first() == Some(&b'.') {
            continue;
        }
        if match_daemon(base, &d, &mut wanted) {
            return true;
        }
    }
    false
}

/// `rc_service_daemons_crashed()`: has any recorded daemon of `service`
/// died?  Read errno afterwards (see the module docs).
pub fn rc_service_daemons_crashed(service: &[u8]) -> bool {
    let base = basename_c(service);
    let Ok(dfd) = at::openat(rc_dirfd_raw(RcDir::Daemons), base, libc::O_RDONLY | libc::O_DIRECTORY, 0) else {
        return false;
    };
    let raw = dfd.as_raw_fd();
    let Ok(names) = at::readdir_dup(raw) else { return false };

    let mut retval = false;
    for d in names {
        if d.first() == Some(&b'.') {
            continue;
        }
        // C stops checking altogether if a record can't be opened.
        let Some(data) = read_at(raw, &d) else { break };

        let mut exec: Option<Vec<u8>> = None;
        let mut list: Option<Vec<Vec<u8>>> = None;
        let mut pidfile: Option<Vec<u8>> = None;
        for line in lines(&data) {
            let Some(eq) = line.iter().position(|&b| b == b'=') else { continue };
            let (token, value) = (&line[..eq], &line[eq + 1..]);
            if value.is_empty() {
                continue;
            }
            if token == b"exec" {
                exec = Some(value.to_vec());
            } else if token.starts_with(b"argv_") {
                list.get_or_insert_with(Vec::new).push(value.to_vec());
            } else if token == b"name" {
                // Parsed but unused, as in C.
            } else if token == b"pidfile" {
                pidfile = Some(value.to_vec());
                break;
            }
        }

        // A chrooted daemon's pidfile is relative to the chroot.
        let ch_root = crate::service::rc_service_value_get(base, b"chroot").map(|c| c_str_prefix(&c).to_vec());
        if let (Some(root), Some(p)) = (&ch_root, &pidfile) {
            pidfile = Some(bcat!(root, p));
        }

        let mut pid = 0;
        let mut argv: Option<Vec<Vec<u8>>> = None;
        if let Some(p) = &pidfile {
            retval = true;
            if let Ok(fd) = at::openat(libc::AT_FDCWD, p, libc::O_RDONLY, 0o666) {
                use std::io::Read;
                let mut s = Vec::new();
                let _ = std::fs::File::from(fd).read_to_end(&mut s);
                // fscanf(fp, "%d", &pid)
                if let Some(v) = scanf_int(&s) {
                    pid = v;
                    retval = false;
                }
            }
            // We have the pid, so no need to match on exec.
        } else {
            if let Some(e) = exec.take() {
                let l = list.get_or_insert_with(Vec::new);
                if l.is_empty() {
                    l.push(e);
                }
            }
            argv = list.take();
        }

        if !retval {
            if pid != 0 {
                if sys::kill(pid, 0).is_err_and(|e| e.0 == libc::ESRCH) {
                    retval = true;
                }
            } else if rc_find_pids(None, argv.as_deref(), 0, 0).is_none() {
                retval = true;
            }
        }
        if retval {
            break;
        }
    }
    retval
}

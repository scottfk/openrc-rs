//! Runlevels and service state: port of the corresponding parts of
//! `src/librc/librc.c` (`rc_runlevel_*`, `rc_service_*`,
//! `rc_services_*`).
//!
//! State lives in the svcdir as symlinks: `started/<svc>`,
//! `inactive/<svc>`, ..., pointing at the init script.  Runlevels are
//! directories of symlinks in the runlevel dir; a subdirectory of a
//! runlevel stacks another runlevel.
//!
//! This is a mechanical port: directory fds come from `rc_dirfd()` and are
//! used unchecked, as in C (so a missing directory gives EBADF from the
//! next call, not a different code path), and directory listings keep
//! readdir order (including `.` and `..` where C walks them).

use openrc_compat::bcat;
use openrc_compat::bytes::basename_c;
use openrc_compat::exec::{ExecArgs, Redirect, do_exec, rc_waitpid};
use openrc_compat::sys::{self, at, libc};

use crate::dirs::{rc_is_user, rc_runleveldir, rc_scriptdirfds, rc_scriptdirs};
use crate::svcdir::{RcDir, rc_dirfd_raw, rc_svcdir};

/// `RC_SERVICE` flags, with C's values.
pub mod flags {
    pub const STOPPED: i32 = 0x0001;
    pub const STARTED: i32 = 0x0002;
    pub const STOPPING: i32 = 0x0004;
    pub const STARTING: i32 = 0x0008;
    pub const INACTIVE: i32 = 0x0010;
    pub const HOTPLUGGED: i32 = 0x0100;
    pub const FAILED: i32 = 0x0200;
    pub const SCHEDULED: i32 = 0x0400;
    pub const WASINACTIVE: i32 = 0x0800;
    pub const CRASHED: i32 = 0x1000;
}

/// `rc_service_state_names[]` (shared/misc.h), in its order.
const STATE_NAMES: &[(i32, Option<RcDir>, &str)] = &[
    (flags::STARTED, Some(RcDir::Started), "started"),
    (flags::STOPPED, None, "stopped"),
    (flags::STARTING, Some(RcDir::Starting), "starting"),
    (flags::STOPPING, Some(RcDir::Stopping), "stopping"),
    (flags::INACTIVE, Some(RcDir::Inactive), "inactive"),
    (flags::WASINACTIVE, Some(RcDir::WasInactive), "wasinactive"),
    (flags::HOTPLUGGED, Some(RcDir::Hotplugged), "hotplugged"),
    (flags::FAILED, Some(RcDir::Failed), "failed"),
    (flags::SCHEDULED, Some(RcDir::Scheduled), "scheduled"),
    (flags::CRASHED, None, "crashed"),
];

pub const RC_LEVEL_SINGLE: &[u8] = b"single";
pub const RC_LEVEL_BOOT: &[u8] = b"boot";
pub const RC_LEVEL_SYSINIT: &[u8] = b"sysinit";
pub const RC_LEVEL_SHUTDOWN: &[u8] = b"shutdown";

/// `rc_dirfd(dir)`; `RC_DIR_INVALID` (None) gives -1, as in C.
fn dirfd(dir: Option<RcDir>) -> i32 {
    dir.map_or(-1, rc_dirfd_raw)
}

/// `rc_parse_service_state_dirfd()` + `rc_dirfd()`.
fn state_dirfd(state: i32) -> i32 {
    dirfd(STATE_NAMES.iter().find(|(s, _, _)| *s == state).and_then(|(_, d, _)| *d))
}

const LS_INITD: u32 = 0x01;
const LS_DIR: u32 = 0x02;

/// `ls_dir()`: names in `dir` (relative to `targetfd`), in readdir order,
/// skipping dotfiles; LS_INITD also skips entries that don't stat (e.g.
/// dangling runlevel links) and `*.sh`; LS_DIR keeps only directories.
fn ls_dir(targetfd: i32, dir: &[u8], options: u32) -> Vec<Vec<u8>> {
    let Ok(fd) = at::openat(targetfd, dir, libc::O_RDONLY | libc::O_DIRECTORY, 0) else { return Vec::new() };
    let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
    let Ok(names) = at::readdir_dup(raw) else { return Vec::new() };
    let mut list = Vec::new();
    for name in names {
        if name.first() == Some(&b'.') {
            continue;
        }
        if options & LS_INITD != 0 {
            // The script must really exist: a runlevel may still list a
            // service that has been removed.
            if at::fstatat(raw, &name, 0).is_err() {
                continue;
            }
            if name.len() > 2 && name.ends_with(b".sh") {
                continue;
            }
        }
        if options & LS_DIR != 0 && !at::fstatat(raw, &name, 0).is_ok_and(|st| st.is_dir()) {
            continue;
        }
        list.push(name);
    }
    list
}

/// `rm_dir()`: remove `pathname` (relative to `targetfd`) recursively,
/// and the directory itself if `top`.  Sets errno on failure, like C.
///
/// QUIRK (upstream, high): entries are stat'ed *following* symlinks, so a
/// symlink to a directory is recursed into and the target's contents are
/// deleted (the link itself then fails to rmdir).
fn rm_dir(targetfd: i32, pathname: &[u8], top: bool) -> bool {
    let Ok(fd) = at::openat(targetfd, pathname, libc::O_RDONLY | libc::O_DIRECTORY, 0) else {
        return false;
    };
    let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
    let names = match at::readdir_dup(raw) {
        Ok(n) => n,
        Err(e) => {
            sys::set_errno(e.0);
            return false;
        }
    };
    for name in names {
        if name == b"." || name == b".." {
            continue;
        }
        let ok = match at::fstatat(raw, &name, 0) {
            Err(e) => {
                sys::set_errno(e.0);
                false
            }
            Ok(st) if st.is_dir() => rm_dir(raw, &name, true),
            Ok(_) => at::unlinkat(raw, &name, 0).map_err(|e| sys::set_errno(e.0)).is_ok(),
        };
        if !ok {
            return false;
        }
    }
    drop(fd);
    if top && let Err(e) = at::unlinkat(targetfd, pathname, libc::AT_REMOVEDIR) {
        sys::set_errno(e.0);
        return false;
    }
    true
}

/// `rm_dir()` returning C's errno on failure.
fn rm_dir_errno(targetfd: i32, pathname: &[u8], top: bool) -> Result<(), i32> {
    // openat failures set errno via the wrapper's Errno, so capture both.
    match at::openat(targetfd, pathname, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
        Err(e) => Err(e.0),
        Ok(fd) => {
            drop(fd);
            if rm_dir(targetfd, pathname, top) { Ok(()) } else { Err(sys::Errno::last().0) }
        }
    }
}

// --- Runlevels --------------------------------------------------------------

/// `rc_runlevel_starting()`: is `$svcdir/rc.starting` there?
pub fn rc_runlevel_starting() -> bool {
    at::faccessat(rc_dirfd_raw(RcDir::Svcdir), b"rc.starting", libc::F_OK).is_ok()
}

/// `rc_runlevel_stopping()`.
pub fn rc_runlevel_stopping() -> bool {
    at::faccessat(rc_dirfd_raw(RcDir::Svcdir), b"rc.stopping", libc::F_OK).is_ok()
}

/// `rc_runlevel_list()`: runlevel directory names, in readdir order.
pub fn rc_runlevel_list() -> Vec<Vec<u8>> {
    ls_dir(libc::AT_FDCWD, &rc_runleveldir(), LS_DIR)
}

/// `rc_runlevel_get()`: the first line of `$svcdir/softlevel`, if any.
pub fn rc_runlevel_get() -> Option<Vec<u8>> {
    use std::io::Read;
    let fd = at::openat(rc_dirfd_raw(RcDir::Svcdir), b"softlevel", libc::O_RDONLY, 0o666).ok()?;
    let mut data = Vec::new();
    // fgets(runlevel, PATH_MAX, fp): at most PATH_MAX-1 bytes, one line.
    let _ = std::fs::File::from(fd).take(libc::PATH_MAX as u64 - 1).read_to_end(&mut data);
    let line_end = data.iter().position(|&b| b == b'\n').map_or(data.len(), |i| i + 1);
    let mut line = openrc_compat::bytes::c_str_prefix(&data[..line_end]).to_vec();
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    (!line.is_empty()).then_some(line)
}

/// `rc_runlevel_set()`: write `$svcdir/softlevel`.
pub fn rc_runlevel_set(runlevel: &[u8]) -> bool {
    match at::openat(rc_dirfd_raw(RcDir::Svcdir), b"softlevel", libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o666) {
        Ok(fd) => {
            sys::write_all_fd(std::os::fd::AsFd::as_fd(&fd), openrc_compat::bytes::c_str_prefix(runlevel));
            true
        }
        Err(_) => false,
    }
}

/// `rc_runlevel_exists()`: is there a runlevel directory of that name?
pub fn rc_runlevel_exists(runlevel: &[u8]) -> bool {
    if runlevel.is_empty() || runlevel == b"." || runlevel == b".." {
        return false;
    }
    at::fstatat(rc_dirfd_raw(RcDir::Runlevel), runlevel, 0).is_ok_and(|st| st.is_dir())
}

/// `rc_runlevel_stack()`: stack `src` into `dst` (`dst/src -> ../src`).
pub fn rc_runlevel_stack(dst: &[u8], src: &[u8]) -> bool {
    if !rc_runlevel_exists(dst) || !rc_runlevel_exists(src) {
        return false;
    }
    sys::symlink(&bcat!("../", src), &bcat!(rc_runleveldir(), "/", dst, "/", src)).is_ok()
}

/// `rc_runlevel_unstack()`.
pub fn rc_runlevel_unstack(dst: &[u8], src: &[u8]) -> bool {
    sys::unlink(&bcat!(rc_runleveldir(), "/", dst, "/", src)).is_ok()
}

/// `get_runlevel_chain()`.  Exits on a stacking loop, as C does.
fn runlevel_chain(runlevel: &[u8], levels: &mut Vec<Vec<u8>>, ancestors: &mut Vec<Vec<u8>>) {
    if !rc_runlevel_exists(runlevel) {
        return;
    }
    // Already listed: move it to the end.
    if let Some(i) = levels.iter().position(|l| l == runlevel) {
        levels.remove(i);
    }
    levels.push(runlevel.to_vec());

    for next in ls_dir(rc_dirfd_raw(RcDir::Runlevel), runlevel, LS_DIR) {
        if ancestors.contains(&next) {
            let mut msg =
                bcat!("Loop detected in stacked runlevels attempting to enter runlevel ", next, "!\nAncestors:\n");
            for p in ancestors.iter() {
                msg.extend_from_slice(&bcat!("\t", p, "\n"));
            }
            openrc_compat::stdio::err(&msg);
            openrc_compat::stdio::exit(1);
        }
        ancestors.push(next.clone());
        runlevel_chain(&next, levels, ancestors);
        // rc_stringlist_delete: the first equal entry.
        if let Some(i) = ancestors.iter().position(|a| *a == next) {
            ancestors.remove(i);
        }
    }
}

/// `rc_runlevel_stacks()`: `runlevel` and every runlevel stacked into it,
/// in dependency order.
pub fn rc_runlevel_stacks(runlevel: &[u8]) -> Vec<Vec<u8>> {
    let mut stack = Vec::new();
    let mut ancestors = vec![runlevel.to_vec()];
    runlevel_chain(runlevel, &mut stack, &mut ancestors);
    stack
}

// --- Services ---------------------------------------------------------------

/// `rc_service_resolve()`: the init script for `service` (a path is
/// returned as is; then started/ and inactive/ links; then
/// `<scriptdir>/init.d/<service>`).
pub fn rc_service_resolve(service: &[u8]) -> Option<Vec<u8>> {
    if service.first() == Some(&b'/') {
        return Some(service.to_vec());
    }
    for dir in [RcDir::Started, RcDir::Inactive] {
        let fd = rc_dirfd_raw(dir);
        if let Ok(st) = at::fstatat(fd, service, libc::AT_SYMLINK_NOFOLLOW)
            && st.is_lnk()
            && let Ok(target) = at::readlinkat(fd, service, st.size.max(0) as usize + 1)
        {
            return Some(target);
        }
    }
    for dir in rc_scriptdirs() {
        let file = bcat!(dir, "/init.d/", service);
        if sys::stat(&file).is_ok() {
            return Some(file);
        }
    }
    None
}

/// `rc_service_exists()`: Ok, or the errno C sets (EINVAL for `*.sh`,
/// ENOENT if unresolvable, ENOEXEC if not executable; a failing stat
/// leaves stat's errno).
pub fn rc_service_exists(service: &[u8]) -> Result<(), i32> {
    if service.len() > 2 && service.ends_with(b".sh") {
        return Err(libc::EINVAL);
    }
    let Some(file) = rc_service_resolve(service) else { return Err(libc::ENOENT) };
    match sys::stat(&file) {
        Ok(st) if st.mode & 0o111 != 0 => Ok(()),
        Ok(_) => Err(libc::ENOEXEC),
        Err(e) => Err(e.0),
    }
}

/// `popen(cmd, "r")` + one `getline` + `pclose`: the first line of output
/// (without its newline), or None if there was none.
fn popen_first_line(cmd: &[u8]) -> Option<Vec<u8>> {
    let mut a = ExecArgs::new([b"sh".as_slice(), b"-c", cmd]);
    a.cmd = Some(b"/bin/sh".to_vec());
    a.stdout = Redirect::Pipe;
    let res = do_exec(&a).ok()?;
    let fd = res.stdout.expect("piped");
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let line = loop {
        match sys::read_once(std::os::fd::AsFd::as_fd(&fd), &mut buf) {
            Ok(0) | Err(_) => break (!data.is_empty()).then_some(data),
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                if let Some(i) = data.iter().position(|&b| b == b'\n') {
                    data.truncate(i);
                    break Some(data);
                }
            }
        }
    };
    drop(fd);
    rc_waitpid(res.pid);
    line
}

/// `rc_service_extra_commands()`: the script's extra (started/stopped)
/// commands, or None if it can't be resolved or printed nothing.
pub fn rc_service_extra_commands(service: &[u8]) -> Option<Vec<Vec<u8>>> {
    let svc = rc_service_resolve(service)?;
    let cmd = bcat!(". '", svc, "'; echo $extra_commands $extra_started_commands $extra_stopped_commands");
    let line = popen_first_line(&cmd)?;
    Some(
        openrc_compat::bytes::c_str_prefix(&line)
            .split(|&b| b == b' ')
            .filter(|t| !t.is_empty())
            .map(<[u8]>::to_vec)
            .collect(),
    )
}

/// `rc_service_description()`: `$description` (or `$description_<option>`).
pub fn rc_service_description(service: &[u8], option: Option<&[u8]>) -> Option<Vec<u8>> {
    let svc = rc_service_resolve(service)?;
    let option = option.unwrap_or_default();
    let cmd = bcat!(". '", svc, "'; echo \"${description", if option.is_empty() { "" } else { "_" }, option, "}\"");
    popen_first_line(&cmd)
}

/// `rc_service_in_runlevel()`.
pub fn rc_service_in_runlevel(service: &[u8], runlevel: &[u8]) -> bool {
    sys::access(&bcat!(rc_runleveldir(), "/", runlevel, "/", basename_c(service)), libc::F_OK).is_ok()
}

/// `rc_service_mark()`: move `service` to `state`.
pub fn rc_service_mark(service: &[u8], state: i32) -> bool {
    let Some(init) = rc_service_resolve(service) else { return false };
    let base = basename_c(service);
    let mut skip_wasinactive = false;

    if state != flags::STOPPED {
        let fd = state_dirfd(state);
        if sys::access(&init, libc::F_OK).is_err() {
            return false;
        }
        let _ = at::unlinkat(fd, base, 0);
        if at::symlinkat(&init, fd, base).is_err() {
            return false;
        }
    }

    if state == flags::HOTPLUGGED || state == flags::FAILED {
        return true;
    }

    // Remove any old states now.
    for &(s, dir, _) in STATE_NAMES {
        // C fetches (and so creates) every state's directory first, even
        // for the states it then skips.
        let fd = dirfd(dir);
        if s == state {
            continue;
        }
        match s {
            flags::STOPPED | flags::HOTPLUGGED | flags::SCHEDULED => continue,
            flags::WASINACTIVE if skip_wasinactive => continue,
            _ => {}
        }
        if at::faccessat(fd, base, libc::F_OK).is_err() {
            continue;
        }
        if (state == flags::STARTING || state == flags::STOPPING) && s == flags::INACTIVE {
            if at::symlinkat(&init, rc_dirfd_raw(RcDir::WasInactive), base).is_err() {
                return false;
            }
            skip_wasinactive = true;
        }
        if at::unlinkat(fd, base, 0).is_err() {
            return false;
        }
    }

    // Remove the exclusive state if we're inactive.
    if state == flags::STARTED || state == flags::STOPPED || state == flags::INACTIVE {
        let _ = at::unlinkat(rc_dirfd_raw(RcDir::Exclusive), base, 0);
    }

    // Remove any options and daemons the service may have stored.
    if state == flags::STOPPED {
        rm_dir(rc_dirfd_raw(RcDir::Options), base, true);
        rm_dir(rc_dirfd_raw(RcDir::Daemons), base, true);
        // QUIRK: environ/<svc> is a directory, so this unlink always fails
        // and saved environment is never cleared.
        let _ = at::unlinkat(rc_dirfd_raw(RcDir::Environ), base, 0);
        rc_service_schedule_clear(service);
    }

    // These are final states, so remove us from scheduled.
    if state == flags::STARTED || state == flags::STOPPED {
        let sched = rc_dirfd_raw(RcDir::Scheduled);
        if let Ok(names) = at::readdir_dup(sched) {
            // Includes "." and "..", as in C: "./<svc>" and "../<svc>" are
            // unlinked too.
            for d in names {
                let _ = at::unlinkat(sched, &bcat!(d, "/", base), 0);
                let saved = sys::Errno::last();
                let _ = at::unlinkat(sched, &d, libc::AT_REMOVEDIR);
                sys::set_errno(saved.0);
            }
        }
    }
    true
}

/// `rc_service_state()`: the service's state flags.
pub fn rc_service_state(service: &[u8]) -> i32 {
    let base = basename_c(service);
    let mut state = flags::STOPPED;
    for &(s, dir, _) in STATE_NAMES {
        if at::faccessat(dirfd(dir), base, libc::F_OK).is_ok() {
            if s <= 0x10 {
                state = s;
            } else {
                state |= s;
            }
        }
    }

    // A crash report is ignored if errno is EACCES afterwards (e.g. a
    // pidfile we may not read); see crate::daemon.
    if state & flags::STARTED != 0
        && crate::daemon::rc_service_daemons_crashed(service)
        && sys::Errno::last().0 != libc::EACCES
    {
        state |= flags::CRASHED;
    }

    if state & flags::STOPPED != 0 {
        let sched = rc_dirfd_raw(RcDir::Scheduled);
        if let Ok(names) = at::readdir_dup(sched) {
            // QUIRK: tests scheduled/<service> (does this service schedule
            // others?) once per entry, not whether it is itself scheduled.
            if !names.is_empty() && at::faccessat(sched, service, libc::F_OK).is_ok() {
                state |= flags::SCHEDULED;
            }
        }
    }
    state
}

/// `rc_service_schedule_start()`: start `service_to_start` when `service`
/// starts.
pub fn rc_service_schedule_start(service: &[u8], service_to_start: &[u8]) -> bool {
    if rc_service_exists(service_to_start).is_err() {
        return false;
    }
    let base_to_start = basename_c(service_to_start);
    let base = basename_c(service);
    let sched = rc_dirfd_raw(RcDir::Scheduled);
    if let Err(e) = at::mkdirat(sched, base, 0o755)
        && e.0 != libc::EEXIST
    {
        return false;
    }
    let Ok(schedfd) = at::openat(sched, base, libc::O_RDONLY, 0) else { return false };
    let schedfd = std::os::fd::AsRawFd::as_raw_fd(&schedfd);
    let init = rc_service_resolve(service_to_start).unwrap_or_default();
    at::faccessat(schedfd, base_to_start, libc::F_OK).is_ok() || at::symlinkat(&init, schedfd, base_to_start).is_ok()
}

/// `rc_service_schedule_clear()`.  QUIRK: the result is inverted: true
/// only when there was nothing to clear (rm_dir failed with ENOENT).
pub fn rc_service_schedule_clear(service: &[u8]) -> bool {
    match rm_dir_errno(rc_dirfd_raw(RcDir::Scheduled), basename_c(service), true) {
        Ok(()) => false,
        Err(e) => {
            sys::set_errno(e);
            e == libc::ENOENT
        }
    }
}

/// `rc_services_in_runlevel()`: `None` lists every init script in every
/// script directory.
pub fn rc_services_in_runlevel(runlevel: Option<&[u8]>) -> Vec<Vec<u8>> {
    let Some(runlevel) = runlevel else {
        let mut list = Vec::new();
        for &fd in rc_scriptdirfds() {
            list.extend(ls_dir(fd, b"init.d", LS_INITD));
        }
        return list;
    };
    // This special level never contains any services.
    if runlevel == RC_LEVEL_SINGLE {
        return Vec::new();
    }
    ls_dir(rc_dirfd_raw(RcDir::Runlevel), runlevel, LS_INITD)
}

/// `rc_services_in_runlevel_stacked()`.  QUIRK: rc_runlevel_stacks()
/// includes the runlevel itself, so its services are listed twice.
pub fn rc_services_in_runlevel_stacked(runlevel: Option<&[u8]>) -> Vec<Vec<u8>> {
    let mut list = rc_services_in_runlevel(runlevel);
    if let Some(r) = runlevel {
        for stack in rc_runlevel_stacks(r) {
            list.extend(rc_services_in_runlevel(Some(&stack)));
        }
    }
    list
}

/// `rc_services_in_state()`.
pub fn rc_services_in_state(state: i32) -> Vec<Vec<u8>> {
    let svcdir = rc_dirfd_raw(RcDir::Svcdir);
    if state != flags::SCHEDULED {
        // C passes the state's name (NULL if unknown, which fails).
        return match STATE_NAMES.iter().find(|(s, _, _)| *s == state) {
            Some((_, _, name)) => ls_dir(svcdir, name.as_bytes(), LS_INITD),
            None => Vec::new(),
        };
    }
    let Ok(dfd) = at::openat(svcdir, b"scheduled", libc::O_RDONLY | libc::O_DIRECTORY, 0) else { return Vec::new() };
    let raw = std::os::fd::AsRawFd::as_raw_fd(&dfd);
    let Ok(names) = at::readdir_dup(raw) else { return Vec::new() };
    let mut list = Vec::new();
    // QUIRK: walks "." and ".." too, so the scheduled dir's own entries
    // and the svcdir's entries are listed as well.
    for d in names {
        list.extend(ls_dir(raw, &d, LS_INITD));
    }
    list
}

/// `rc_service_add()`: add `service` to `runlevel`; Err(errno).
pub fn rc_service_add(runlevel: &[u8], service: &[u8]) -> Result<(), i32> {
    if !rc_runlevel_exists(runlevel) {
        return Err(libc::ENOENT);
    }
    if rc_service_in_runlevel(service, runlevel) {
        return Err(libc::EEXIST);
    }
    let init = rc_service_resolve(service);
    let initdir = format!("{}/init.d", crate::paths::SYSCONFDIR).into_bytes();
    let mut target = init.clone();

    // Only things in /etc/init.d may go in the boot runlevel (not
    // applicable to user services).
    if !rc_is_user() && runlevel == RC_LEVEL_BOOT {
        // realpath(dirname(init)); C's dirname(NULL) is ".".
        let dir = match &init {
            Some(i) => match i.iter().rposition(|&b| b == b'/') {
                Some(0) => b"/".to_vec(),
                Some(p) => i[..p].to_vec(),
                None => b".".to_vec(),
            },
            None => b".".to_vec(),
        };
        let Some(path) = sys::realpath(&dir) else { return Err(sys::Errno::last().0) };
        if path != initdir {
            return Err(libc::EPERM);
        }
        target = Some(bcat!(initdir, "/", service));
    }

    // Deviation: C dereferences NULL here for a service that can't be
    // resolved (callers check rc_service_exists first).
    let Some(target) = target else { return Err(libc::ENOENT) };
    // We shouldn't add any temporary script to a runlevel.
    if target.starts_with(&rc_svcdir()) {
        return Err(libc::ENOENT);
    }
    sys::symlink(&target, &bcat!(rc_runleveldir(), "/", runlevel, "/", basename_c(service))).map_err(|e| e.0)
}

/// `rc_service_delete()`.
pub fn rc_service_delete(runlevel: &[u8], service: &[u8]) -> bool {
    sys::unlink(&bcat!(rc_runleveldir(), "/", runlevel, "/", basename_c(service))).is_ok()
}

/// `rc_services_scheduled_by()`: services that scheduled `service`.
/// (Walks `.` too, so "." is listed if `service` scheduled anything.)
pub fn rc_services_scheduled_by(service: &[u8]) -> Vec<Vec<u8>> {
    let sched = rc_dirfd_raw(RcDir::Scheduled);
    let Ok(names) = at::readdir_dup(sched) else { return Vec::new() };
    names.into_iter().filter(|d| at::faccessat(sched, &bcat!(d, "/", service), libc::F_OK).is_ok()).collect()
}

/// `rc_services_scheduled()`: services `service` scheduled.
pub fn rc_services_scheduled(service: &[u8]) -> Vec<Vec<u8>> {
    ls_dir(rc_dirfd_raw(RcDir::Scheduled), basename_c(service), LS_INITD)
}

/// `rc_service_putenv()`: save a `NAME=value` string for `service`.
pub fn rc_service_putenv(service: &[u8], env: &[u8]) -> Result<(), i32> {
    let Some(eq) = env.iter().position(|&b| b == b'=') else { return Err(libc::EINVAL) };
    let dfd = rc_dirfd_raw(RcDir::Environ);
    if let Err(e) = at::mkdirat(dfd, service, 0o700)
        && e.0 != libc::EEXIST
    {
        return Err(e.0);
    }
    let fd = at::openat(dfd, &bcat!(service, "/", &env[..eq]), libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o666)
        .map_err(|e| e.0)?;
    sys::write_all_fd(std::os::fd::AsFd::as_fd(&fd), &env[eq + 1..]);
    Ok(())
}

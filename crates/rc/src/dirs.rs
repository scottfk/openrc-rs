//! User mode and the script directory search path (`librc.c`:
//! `rc_set_user`, `rc_is_user`, `load_dynamic_path`, `rc_scriptdirs`).
//!
//! Paths are byte strings, as in C: they come from the environment
//! (`$RC_PATH`, `$HOME`, `$XDG_*`), which need not be UTF-8.

use std::sync::Mutex;

use openrc_compat::{bcat, env, stdio};

use crate::misc::rc_yesno;
use crate::paths::default_rc_path;

#[derive(Debug, Default)]
struct Dirs {
    is_user: bool,
    svcdir: Option<Vec<u8>>,
    confdir: Option<Vec<u8>>,
    runleveldir: Option<Vec<u8>>,
    /// `rc_path.entries`, once computed.
    scriptdirs: Option<Vec<Vec<u8>>>,
}

static DIRS: Mutex<Dirs> =
    Mutex::new(Dirs { is_user: false, svcdir: None, confdir: None, runleveldir: None, scriptdirs: None });

fn dirs() -> std::sync::MutexGuard<'static, Dirs> {
    DIRS.lock().unwrap_or_else(|e| e.into_inner())
}

/// `rc_is_user()`: `$RC_USER_SERVICES` if set, else whether `rc_set_user`
/// has been called.
pub fn rc_is_user() -> bool {
    match env::get("RC_USER_SERVICES") {
        Some(v) => rc_yesno(Some(&v)),
        None => dirs().is_user,
    }
}

/// The svcdir set by `rc_set_user`, if in user mode.
pub(crate) fn user_svcdir() -> Option<Vec<u8>> {
    dirs().svcdir.clone()
}

/// `rc_set_user()`: switch to user-services mode (`-U`).  Exits if the XDG
/// directories it needs cannot be determined.
pub fn rc_set_user() {
    let mut d = dirs();
    if d.is_user {
        return;
    }
    d.is_user = true;
    env::set("RC_USER_SERVICES", b"yes");

    let confdir = if let Some(x) = env::get("XDG_CONFIG_HOME") {
        bcat!(x, "/rc")
    } else if let Some(h) = env::get("HOME") {
        bcat!(h, "/.config/rc")
    } else {
        drop(d);
        stdio::err(b"XDG_CONFIG_HOME and HOME unset.\n");
        stdio::exit(1);
    };
    d.runleveldir = Some(bcat!(confdir, "/runlevels"));
    d.confdir = Some(confdir);

    let Some(rt) = env::get("XDG_RUNTIME_DIR") else {
        drop(d);
        stdio::err(b"XDG_RUNTIME_DIR unset.\n");
        stdio::exit(1);
    };
    d.svcdir = Some(bcat!(rt, "/openrc"));
    let path = load_dynamic_path(&d, true);
    d.scriptdirs = Some(path);
    drop(d);
    crate::svcdir::clear_dirfds();
}

/// `rc_sysconfdir()`.
pub fn rc_sysconfdir() -> &'static str {
    crate::paths::SYSCONFDIR
}

/// `rc_usrconfdir()`: the user-mode config dir, if in user mode.
pub fn rc_usrconfdir() -> Option<Vec<u8>> {
    dirs().confdir.clone()
}

/// `rc_runleveldir()`: the user-mode runlevel dir, else `$SYSCONFDIR/runlevels`.
pub fn rc_runleveldir() -> Vec<u8> {
    dirs().runleveldir.clone().unwrap_or_else(|| format!("{}/runlevels", crate::paths::SYSCONFDIR).into_bytes())
}

/// `rc_scriptdirfds()`: fds for the script directories that could be
/// opened.  QUIRK: computed once (on the first call that opens any) and
/// never refreshed, even after `rc_set_user()`.
pub fn rc_scriptdirfds() -> &'static [i32] {
    static FDS: Mutex<Option<&'static [i32]>> = Mutex::new(None);
    let mut cached = FDS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(fds) = *cached {
        return fds;
    }
    let mut fds = Vec::new();
    for d in rc_scriptdirs() {
        let flags = openrc_compat::sys::libc::O_RDONLY
            | openrc_compat::sys::libc::O_DIRECTORY
            | openrc_compat::sys::libc::O_CLOEXEC;
        if let Ok(fd) = openrc_compat::sys::openat(None, &d, flags, 0) {
            fds.push(std::os::fd::IntoRawFd::into_raw_fd(fd));
        }
    }
    let fds: &'static [i32] = Box::leak(fds.into_boxed_slice());
    // C's `static size_t len` stays 0 if nothing opened, so it retries.
    if !fds.is_empty() {
        *cached = Some(fds);
    }
    fds
}

/// Split on `:` like upstream's `strnsep()` (empty elements kept).
fn split_colon(s: &[u8]) -> impl Iterator<Item = &[u8]> {
    s.split(|&b| b == b':')
}

/// `append_user_path()`.
fn user_path(d: &Dirs, out: &mut Vec<Vec<u8>>) {
    out.push(d.confdir.clone().unwrap_or_default());
    match env::get("XDG_CONFIG_DIRS") {
        None => out.push(b"/etc/xdg/rc".to_vec()),
        Some(x) => out.extend(split_colon(&x).filter(|e| !e.is_empty()).map(|e| bcat!(e, "/rc"))),
    }
    let defaults = default_rc_path();
    // Every default entry except the trailing /run/openrc.
    out.extend(defaults[..defaults.len() - 1].iter().map(|p| bcat!(p, "/user")));
    out.push(d.svcdir.clone().unwrap_or_default());
}

/// `load_dynamic_path()`: the script path from `$RC_PATH` (where an empty
/// element expands to the defaults, or the user path in user mode).
fn load_dynamic_path(d: &Dirs, user: bool) -> Vec<Vec<u8>> {
    let Some(path) = env::get("RC_PATH") else {
        if !user {
            return default_rc_path();
        }
        let mut out = Vec::new();
        user_path(d, &mut out);
        return out;
    };

    let mut out = Vec::new();
    let mut defaults = default_rc_path().into_iter();
    let mut has_user = false;
    for entry in split_colon(&path) {
        if !entry.is_empty() {
            out.push(if user { bcat!(entry, "/user") } else { entry.to_vec() });
        } else if user && !has_user {
            user_path(d, &mut out);
            has_user = true;
        } else if !user {
            // Consumes the defaults: a second empty element adds nothing.
            out.extend(defaults.by_ref());
        }
    }
    out
}

/// `rc_scriptdirs()`: directories searched for init scripts and `rc.conf`,
/// highest priority first.
pub fn rc_scriptdirs() -> Vec<Vec<u8>> {
    let mut d = dirs();
    if d.scriptdirs.is_none() {
        let p = load_dynamic_path(&d, false);
        d.scriptdirs = Some(p);
    }
    d.scriptdirs.clone().expect("set above")
}

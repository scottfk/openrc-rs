//! User mode and the script directory search path (`librc.c`:
//! `rc_set_user`, `rc_is_user`, `load_dynamic_path`, `rc_scriptdirs`).

use std::sync::Mutex;

use openrc_compat::{env, stdio};

use crate::misc::rc_yesno;
use crate::paths::default_rc_path;

#[derive(Debug, Default)]
struct Dirs {
    is_user: bool,
    svcdir: Option<String>,
    confdir: Option<String>,
    runleveldir: Option<String>,
    /// `rc_path.entries`, once computed.
    scriptdirs: Option<Vec<String>>,
}

static DIRS: Mutex<Dirs> =
    Mutex::new(Dirs { is_user: false, svcdir: None, confdir: None, runleveldir: None, scriptdirs: None });

fn dirs() -> std::sync::MutexGuard<'static, Dirs> {
    DIRS.lock().unwrap_or_else(|e| e.into_inner())
}

fn getenv_string(name: &str) -> Option<String> {
    env::get(name).map(|v| String::from_utf8_lossy(&v).into_owned())
}

/// `rc_is_user()`: `$RC_USER_SERVICES` if set, else whether `rc_set_user`
/// has been called.
pub fn rc_is_user() -> bool {
    match env::get("RC_USER_SERVICES") {
        Some(v) => rc_yesno(Some(&v)),
        None => dirs().is_user,
    }
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

    let confdir = if let Some(x) = getenv_string("XDG_CONFIG_HOME") {
        format!("{x}/rc")
    } else if let Some(h) = getenv_string("HOME") {
        format!("{h}/.config/rc")
    } else {
        drop(d);
        stdio::err(b"XDG_CONFIG_HOME and HOME unset.\n");
        stdio::exit(1);
    };
    d.runleveldir = Some(format!("{confdir}/runlevels"));
    d.confdir = Some(confdir);

    let Some(rt) = getenv_string("XDG_RUNTIME_DIR") else {
        drop(d);
        stdio::err(b"XDG_RUNTIME_DIR unset.\n");
        stdio::exit(1);
    };
    d.svcdir = Some(format!("{rt}/openrc"));
    let path = load_dynamic_path(&d, true);
    d.scriptdirs = Some(path);
}

/// `append_user_path()`.
fn user_path(d: &Dirs, out: &mut Vec<String>) {
    out.push(d.confdir.clone().unwrap_or_default());
    match getenv_string("XDG_CONFIG_DIRS") {
        None => out.push("/etc/xdg/rc".to_string()),
        Some(x) => out.extend(x.split(':').filter(|e| !e.is_empty()).map(|e| format!("{e}/rc"))),
    }
    let defaults = default_rc_path();
    // Every default entry except the trailing /run/openrc.
    out.extend(defaults[..defaults.len() - 1].iter().map(|p| format!("{p}/user")));
    out.push(d.svcdir.clone().unwrap_or_default());
}

/// `load_dynamic_path()`: the script path from `$RC_PATH` (where an empty
/// element expands to the defaults, or the user path in user mode).
fn load_dynamic_path(d: &Dirs, user: bool) -> Vec<String> {
    let rc_path = getenv_string("RC_PATH");
    let Some(path) = rc_path else {
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
    for entry in path.split(':') {
        if !entry.is_empty() {
            out.push(if user { format!("{entry}/user") } else { entry.to_string() });
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
pub fn rc_scriptdirs() -> Vec<String> {
    let mut d = dirs();
    if d.scriptdirs.is_none() {
        let p = load_dynamic_path(&d, false);
        d.scriptdirs = Some(p);
    }
    d.scriptdirs.clone().expect("set above")
}

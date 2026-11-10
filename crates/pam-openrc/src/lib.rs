//! `pam_openrc.so`: port of upstream's `src/pam_openrc/pam_openrc.c`.  A
//! PAM session module: a user's first login session starts their
//! `user.<name>` service (made from the `user` service if need be), the
//! last one to close stops it, and sessions get the environment the
//! user's services exported.  Off when `rc_autostart_user` is "no".

use std::ffi::{c_char, c_int};

use openrc_applets::shared::{exec_service, svc_lock, svc_unlock};
use openrc_compat::pam::{PAM_SERVICE, PAM_SESSION_ERR, PAM_SILENT, PAM_SUCCESS, PAM_USER, Pam, PamHandle};
use openrc_compat::{bcat, bytes, env, num, sys};
use openrc_einfo::elog;
use rc::state::flags;
use rc::svcdir::{RcDir, rc_dirfd_raw};
use sys::libc;

/// `import_env()`: the NUL-separated `user/<name>/environ` the user's
/// boot runlevel exported, into the PAM environment.
fn import_env(pamh: &Pam, user: &[u8]) {
    let Ok(fd) = sys::openat_raw(rc_dirfd_raw(RcDir::Svcdir), &bcat!("user/", user, "/environ"), libc::O_RDONLY, 0)
    else {
        return;
    };
    let mut data = Vec::new();
    {
        use std::io::Read;
        use std::os::fd::FromRawFd;
        // SAFETY: openat_raw just returned this fd; the File owns it.
        let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
        let _ = f.read_to_end(&mut data);
    }
    // (getdelim('\0') pieces: each up to and including a NUL, the last
    // possibly without.)
    let mut rest = &data[..];
    while !rest.is_empty() {
        let end = rest.iter().position(|&b| b == 0).map_or(rest.len(), |p| p + 1);
        pamh.putenv(bytes::c_str_prefix(&rest[..end]));
        rest = &rest[end..];
    }
}

fn err(msg: &[u8]) {
    elog(libc::LOG_ERR, Some(msg));
}

fn info(msg: &[u8]) {
    elog(libc::LOG_INFO, Some(msg));
}

/// `exec_openrc()`.
fn exec_openrc(pamh: &Pam, opening: bool, quiet: bool) -> c_int {
    let do_autostart = rc::conf::rc_conf_value("rc_autostart_user").map(|v| bytes::c_str_prefix(&v).to_vec());
    let (yes, errno) = rc::misc::rc_yesno_errno(do_autostart.as_deref());
    if !yes && errno.is_none() {
        return PAM_SUCCESS;
    }

    env::set("EINFO_LOG", b"pam_openrc");
    if quiet {
        env::set("EINFO_QUIET", b"yes");
        env::set("EERROR_QUIET", b"yes");
    }

    let Ok(session) = pamh.get_str_item(PAM_SERVICE) else {
        err(b"Failed to get PAM_SERVICE");
        return PAM_SESSION_ERR;
    };
    if session.as_deref() == Some(b"openrc-user") {
        return PAM_SUCCESS;
    }
    let Ok(username) = pamh.get_str_item(PAM_USER) else { return PAM_SESSION_ERR };
    let Some(username) = username else {
        err(b"PAM_USER unset.");
        return PAM_SESSION_ERR;
    };
    let Some(user) = sys::getpwnam(&username) else {
        err(&bcat!("User '", username, "' not found."));
        return PAM_SESSION_ERR;
    };
    if user.uid == 0 {
        return PAM_SUCCESS;
    }

    let pam_lock = bcat!("pam_openrc[", user.name, "]");
    env::set("EINFO_LOG", &pam_lock);
    let svc_name = bcat!("user.", user.name);
    let mut ret = PAM_SUCCESS;

    let state = rc::state::rc_service_state(&svc_name);
    if state & flags::STARTED != 0 && state & flags::HOTPLUGGED == 0 {
        info(&bcat!(svc_name, " started and not hotplugged, skipping session."));
    } else {
        info(if opening { b"starting session" } else { b"stopping session" });
        match svc_lock(&pam_lock, false) {
            Err(_) => ret = PAM_SESSION_ERR,
            Ok(fd) => {
                ret = sessions(&svc_name, opening);
                svc_unlock(&pam_lock, fd);
            }
        }
    }

    if opening {
        import_env(pamh, &user.name);
    }
    env::unset("EINFO_LOG");
    ret
}

/// The locked part: make `user.<name>` if need be, count the sessions in
/// its `logins` value, and start it for the first or stop it after the
/// last.
fn sessions(svc_name: &[u8], opening: bool) -> c_int {
    if rc::state::rc_service_resolve(svc_name).is_none() {
        let Some(script) = rc::state::rc_service_resolve(b"user") else {
            err(&bcat!("Failed to resolve ", svc_name, "."));
            return PAM_SESSION_ERR;
        };
        if let Err(e) = sys::at::symlinkat(&script, rc_dirfd_raw(RcDir::Initd), svc_name) {
            err(&bcat!("symlink: ", e.strerror()));
            return PAM_SESSION_ERR;
        }
    }

    let mut count =
        rc::service::rc_service_value_get(svc_name, b"logins").and_then(|v| num::scanf_int(&v)).unwrap_or(0);
    let mut pid = -1;
    if opening {
        if count == 0 {
            pid = exec_service(svc_name, b"start");
            rc::state::rc_service_mark(svc_name, flags::HOTPLUGGED);
        }
        count += 1;
    } else {
        count -= 1;
        if count == 0 {
            pid = exec_service(svc_name, b"stop");
        }
    }
    info(format!("{count} sessions").as_bytes());

    let mut ret = PAM_SUCCESS;
    if pid > 0 {
        let (_, status) = sys::waitpid_raw(pid, 0);
        if status != 0 {
            ret = PAM_SESSION_ERR;
        }
    }
    rc::service::rc_service_value_set(svc_name, b"logins", Some(count.to_string().as_bytes()));
    ret
}

/// Run `f`, turning a panic into a session error (it mustn't unwind into
/// the PAM application).
fn guarded(f: impl FnOnce() -> c_int + std::panic::UnwindSafe) -> c_int {
    std::panic::catch_unwind(f).unwrap_or(PAM_SESSION_ERR)
}

/// # Safety
///
/// Called by libpam with a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_open_session(
    pamh: *mut PamHandle,
    flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    guarded(|| exec_openrc(&Pam(pamh), true, flags & PAM_SILENT != 0))
}

/// # Safety
///
/// Called by libpam with a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_close_session(
    pamh: *mut PamHandle,
    flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    guarded(|| exec_openrc(&Pam(pamh), false, flags & PAM_SILENT != 0))
}

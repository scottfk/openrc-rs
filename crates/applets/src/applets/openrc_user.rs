//! `openrc-user`: port of upstream's `src/openrc-user/openrc-user.c`.
//! Run a user's services: as the user (in a PAM session, with the `pam`
//! feature), run `openrc-user.sh start` through their shell, wait for
//! SIGTERM, then `openrc-user.sh stop`.  Started for a user by the
//! `user` service.  Errors go to syslog (`EINFO_LOG`).

use openrc_compat::{bcat, env, exec, sys};
use openrc_einfo::elog;
use sys::libc;

/// `valid_shell()`: is `shell` listed in /etc/shells (or is there no
/// /etc/shells)?
///
/// QUIRK: C never closes /etc/shells, so each call leaks a descriptor;
/// with no fd 3 inherited, the first becomes fd 3, which then serves as
/// the shell's stdout.
fn valid_shell(shell: &[u8]) -> bool {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    let Ok(fd) = sys::open_raw(&bcat!(rc::paths::SYSCONFDIR, "/shells"), libc::O_RDONLY, 0) else { return true };
    // SAFETY: open_raw just returned this fd; the File is forgotten below,
    // leaving it open as C does.
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut text = Vec::new();
    let _ = f.read_to_end(&mut text);
    std::mem::forget(f);
    text.split(|&b| b == b'\n').filter(|l| !l.starts_with(b"#")).any(|l| openrc_compat::bytes::c_str_prefix(l) == shell)
}

/// `spawn_openrc()`: `openrc-user.sh start|stop` through the user's shell,
/// as the user, with stdout on fd 3; true if it exits 0.
fn spawn_openrc(uid: u32, gid: u32, shell: &[u8], start: bool) -> bool {
    // (The shell may be a multi-call binary such as busybox, so argv[0]
    // is "-" and its full path.  QUIRK: when the shell isn't in
    // /etc/shells argv[0] says /bin/sh, but the user's shell still runs.)
    let argv0 = bcat!("-", if valid_shell(shell) { shell } else { b"/bin/sh" });
    let script = bcat!(rc::paths::LIBEXECDIR, "/sh/openrc-user.sh ", if start { "start" } else { "stop" });
    let mut args = exec::ExecArgs::new([&argv0[..], b"-c", &script]);
    args.cmd = Some(shell.to_vec());
    args.uid = Some(uid);
    args.gid = Some(gid);
    args.stdout = exec::Redirect::Fd(3);
    match exec::do_exec(&args) {
        Ok(r) => exec::rc_waitpid(r.pid) == 0,
        Err(e) => {
            elog(libc::LOG_ERR, Some(&bcat!("do_exec: ", e.strerror())));
            false
        }
    }
}

pub fn main(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    env::set("EINFO_LOG", b"openrc-user");
    if args.len() != 2 {
        let argv0 = args.first().cloned().unwrap_or_default();
        elog(libc::LOG_ERR, Some(&bcat!("Invalid usage. ", argv0, " <username>")));
        return -1;
    }
    // (Supervisors can't drop privileges for us: PAM modules may need
    // root.)
    let Some(user) = sys::getpwnam(&args[1]) else {
        elog(libc::LOG_ERR, Some(&bcat!("Invalid username ", args[1], ".")));
        return -1;
    };
    // (Copied before PAM: sssd's module calls getpwnam and can clobber
    // it, https://bugs.gentoo.org/970235.)
    let (uid, gid, shell) = (user.uid, user.gid, user.shell.clone());

    env::set("USER", &user.name);
    env::set("LOGNAME", &user.name);
    env::set("HOME", &user.dir);
    env::set("SHELL", &user.shell);

    let _ = sys::initgroups(&user.name, user.gid);
    env::set("EINFO_LOG", &bcat!("openrc-user.", user.name));

    #[cfg(feature = "pam")]
    let pamh = {
        use openrc_compat::pam::{PAM_SILENT, PAM_SUCCESS, Pam};
        let (pamh, mut rc) = Pam::start(b"openrc-user", &user.name);
        let mut session = false;
        if rc != PAM_SUCCESS {
            elog(libc::LOG_ERR, Some(&bcat!("Failed to start pam: ", pamh.strerror(rc))));
        } else {
            rc = pamh.open_session(PAM_SILENT);
            if rc != PAM_SUCCESS {
                elog(libc::LOG_ERR, Some(&bcat!("Failed to open session: ", pamh.strerror(rc))));
            } else {
                session = true;
            }
        }
        crate::shared::pam_import_env(&pamh);
        if !session && !pamh.0.is_null() {
            pamh.end(rc);
            None
        } else if pamh.0.is_null() {
            None
        } else {
            Some((pamh, rc))
        }
    };

    let mut ret = 0;
    if !spawn_openrc(uid, gid, &shell, true) {
        ret = -1;
    } else {
        sys::block_all_signals();
        sys::sigwait_one(libc::SIGTERM);
        if !spawn_openrc(uid, gid, &shell, false) {
            ret = -1;
        }
    }

    #[cfg(feature = "pam")]
    if let Some((pamh, _)) = pamh {
        use openrc_compat::pam::{PAM_SILENT, PAM_SUCCESS};
        let rc = pamh.close_session(PAM_SILENT);
        if rc != PAM_SUCCESS {
            elog(libc::LOG_ERR, Some(&bcat!("Failed to close session: ", pamh.strerror(rc))));
        }
        pamh.end(rc);
    }
    ret
}

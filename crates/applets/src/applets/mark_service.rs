//! `mark_service_started` & co: port of `src/mark_service/mark_service.c`.
//! Set a service's state from an init script (or a shell).

use openrc_compat::{bcat, env, num, sys};
use openrc_einfo::{eerror, eerrorx};
use rc::state::{self, flags as st};

/// `lookup_service_state()` (shared/misc.c), on the applet name minus
/// its `mark_` prefix.  There is no `service_scheduled`.
fn lookup_service_state(name: &[u8]) -> i32 {
    match name {
        b"service_started" => st::STARTED,
        b"service_stopped" => st::STOPPED,
        b"service_inactive" => st::INACTIVE,
        b"service_starting" => st::STARTING,
        b"service_stopping" => st::STOPPING,
        b"service_hotplugged" => st::HOTPLUGGED,
        b"service_wasinactive" => st::WASINACTIVE,
        b"service_failed" => st::FAILED,
        b"service_crashed" => st::CRASHED,
        _ => 0,
    }
}

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let svcname = env::get("RC_SVCNAME");
    let service = match args.get(1) {
        Some(a) => Some(a.clone()),
        None => svcname.clone(),
    };
    let service = match service {
        Some(s) if !s.is_empty() => s,
        _ => eerrorx(Some(&bcat!(applet, ": no service specified"))),
    };

    let bit = match applet.strip_prefix(b"mark_") {
        Some(rest) => lookup_service_state(rest),
        None => 0,
    };
    if bit == 0 {
        eerrorx(Some(&bcat!(applet, ": unknown applet")));
    }
    let ok = state::rc_service_mark(&service, bit);

    // Marking ourselves: tell the parent openrc-run, so it doesn't mark us
    // from our exit code.
    if ok
        && svcname.as_deref() == Some(&service[..])
        && let Some(pid) = env::get("RC_OPENRC_PID").and_then(|p| num::scanf_int(&p))
        && let Err(e) = sys::kill(pid, sys::libc::SIGUSR1)
    {
        eerror(Some(&bcat!(applet, ": failed to signal parent ", pid.to_string().as_bytes(), ": ", e.strerror())));
    }

    if ok { 0 } else { 1 }
}

//! `service_started`, `service_crashed`, `service_started_daemon` & co:
//! port of `src/service/service.c`.  Test a service's state from an init
//! script (exit status only).

use openrc_compat::{bcat, env, num};
use openrc_einfo::eerrorx;
use rc::state;

use crate::shared::lookup_service_state;

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let service = match args.get(1) {
        Some(a) => Some(a.clone()),
        None => env::get("RC_SVCNAME"),
    };
    let service = match service {
        Some(s) if !s.is_empty() => s,
        _ => eerrorx(Some(&bcat!(applet, ": no service specified"))),
    };

    let st = state::rc_service_state(&service);
    let bit = lookup_service_state(applet);
    // (`service_crashed` maps to the CRASHED bit here, so C's separate
    // `service_crashed` branch is never reached.)
    let ok = if bit != 0 {
        st & bit != 0
    } else if applet == b"service_started_daemon" {
        // [service] exec [index]: with two arguments the second is the
        // index if it scans as a number, otherwise the exec.
        let mut service = env::get("RC_SVCNAME");
        let mut exec = &args[1];
        let mut idx = 0;
        if args.len() > 3 {
            service = Some(args[1].clone());
            exec = &args[2];
            idx = num::scanf_int(&args[3]).unwrap_or(0);
        } else if args.len() == 3 {
            match num::scanf_int(&args[2]) {
                Some(i) => idx = i,
                None => {
                    service = Some(args[1].clone());
                    exec = &args[2];
                }
            }
        }
        // Deviation: with no service (RC_SVCNAME unset) C crashes in
        // basename_c(NULL); we report "not started".
        service.is_some_and(|s| rc::daemon::rc_service_started_daemon(&s, exec, None, idx))
    } else {
        eerrorx(Some(&bcat!(applet, ": unknown applet")));
    };
    if ok { 0 } else { 1 }
}

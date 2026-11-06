//! `service_get_value`, `service_set_value`, `get_options`,
//! `save_options`, `service_export`: port of `src/value/value.c`.
//! Used by init scripts (with `$RC_SVCNAME` set) to keep per-service
//! values and re-exported variables in the svcdir.

use openrc_compat::{bcat, env, stdio};
use openrc_einfo::{eerrorx, ewarn};
use rc::service;

#[derive(PartialEq, Eq)]
enum Action {
    Get,
    Set,
    Export,
}

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let Some(svc) = env::get("RC_SVCNAME") else {
        eerrorx(Some(&bcat!(applet, ": no service specified")));
    };
    let action = match applet {
        b"service_get_value" | b"get_options" => Action::Get,
        b"service_set_value" | b"save_options" => Action::Set,
        b"service_export" => Action::Export,
        _ => eerrorx(Some(&bcat!(applet, ": unknown applet"))),
    };

    if args.get(1).is_none_or(|a| a.is_empty()) {
        let what = if action == Action::Export { "variable" } else { "option" };
        eerrorx(Some(&bcat!(applet, ": no ", what, " specified")));
    }

    match action {
        Action::Get => match service::rc_service_value_get(&svc, &args[1]) {
            Some(v) => {
                // printf("%s", option)
                stdio::out(openrc_compat::bytes::c_str_prefix(&v));
                0
            }
            None => 1,
        },
        // A missing value (`service_set_value KEY`) deletes the option.
        Action::Set => {
            if service::rc_service_value_set(&svc, &args[1], args.get(2).map(Vec::as_slice)) {
                0
            } else {
                1
            }
        }
        Action::Export => {
            for name in &args[1..] {
                if service::rc_service_getenv_size(&svc, name) > 0 {
                    continue;
                }
                match env::get_bytes(name) {
                    Some(v) => {
                        service::rc_service_setenv(&svc, name, &v);
                    }
                    None => {
                        ewarn(Some(&bcat!(svc, ": Missing reexport variable '", name, "'")));
                    }
                }
            }
            0
        }
    }
}

//! `rc-environ`: port of upstream's `src/rc-environ/rc-environ.c`.  Print
//! the variables services re-export (`reexport` in their depend()), or,
//! for a service that's starting, save variables for it to re-export.

use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::{bcat, bytes, env, stdio};
use openrc_einfo::{eerror, ewarn};
use rc::deptree::{self, DepTree};
use rc::state::{self, flags};

use crate::shared::{Usage, getoptstring_common, rc_deptree_load_updating};

const GETOPTSTRING: &str = concat!("ens:r:0", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("export", HasArg::No, b'e'),
    LongOpt::new("no-escape", HasArg::No, b'n'),
    LongOpt::new("null", HasArg::No, b'0'),
    LongOpt::new("service", HasArg::Required, b's'),
    LongOpt::new("runlevel", HasArg::Required, b'r'),
];
const LONGOPTS_HELP: &[&str] = &[
    "Prepend \"export\" to printed variables",
    "Do not perform shell sensitive escape on variable values",
    "End each output line with NUL, not newline",
    "Add service to the list of environments to print",
    "Add services in runlevel to the list of environments to print",
];

/// QUIRK: C never sets its `applet`, so `-V` and the read warning name
/// `(null)`.
const APPLET: &[u8] = b"(null)";

/// `rc_deptree_depend(deptree, service, "reexport")`.  (If the deptree
/// couldn't be loaded C dereferences NULL; we see no re-exports.)
fn reexports(tree: Option<&DepTree>, service: &[u8]) -> Vec<Vec<u8>> {
    tree.and_then(|t| deptree::rc_deptree_depend(t, service, b"reexport").ok()).unwrap_or_default()
}

/// `set_environment()`: save each `NAME=value` (or `NAME`, from our
/// environment) the service re-exports.
fn set_environment(service: &[u8], vars: &[Vec<u8>]) -> i32 {
    let tree = rc_deptree_load_updating(0, None);
    let reexports = reexports(tree.as_ref(), service);
    if state::rc_service_state(service) & flags::STARTING == 0 {
        eerror(Some(&bcat!("service ", service, " is not in the starting state")));
        return 1;
    }
    for var in vars {
        let var = bytes::c_str_prefix(var);
        let name_len = var.iter().position(|&b| b == b'=').unwrap_or(var.len());
        // QUIRK: strncmp over the name's length only, so a name matches
        // any re-export it's a prefix of (and an empty name, all of them).
        let matched = reexports.iter().any(|r| {
            let r = bytes::c_str_prefix(r);
            r.get(..name_len) == Some(&var[..name_len])
        });
        if !matched {
            ewarn(Some(&bcat!("service ", service, " does not reexport ", &var[..name_len], ", skipping")));
            continue;
        }
        if var.get(name_len) == Some(&b'=') {
            let _ = state::rc_service_putenv(service, var);
        } else {
            // (C passes getenv()'s NULL to fputs() for an unset variable,
            // and crashes after creating the file; we leave it empty.)
            let value = env::get_bytes(var).unwrap_or_default();
            rc::service::rc_service_setenv(service, var, &value);
        }
    }
    0
}

/// `escape_variable()`: `$'...'` quoting.
fn escape_variable(value: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b"$'");
    for &ch in value {
        let esc = match ch {
            0x07 => b'a',
            0x08 => b'b',
            0x0c => b'f',
            b'\n' => b'n',
            b'\r' => b'r',
            b'\t' => b't',
            0x0b => b'v',
            // \e is non-standard.
            0x1b => b'e',
            b'"' | b'\'' | b'\\' => ch,
            _ => {
                out.push(ch);
                continue;
            }
        };
        out.extend_from_slice(&[b'\\', esc]);
    }
    out.push(b'\'');
}

/// `print_environment()`: every re-exported variable of `services` (by
/// default, those started).
fn print_environment(services: Option<Vec<Vec<u8>>>, escape: bool, export: bool, sep: u8) -> i32 {
    // From POSIX.2024 Shell Command Language 2.2, excluding `=`.
    const SPECIAL: &[u8] = b"|&;<>()$`\\\"' \t\n*?[]^-!#~%{},";
    let tree = rc_deptree_load_updating(0, None);
    let services = services.unwrap_or_else(|| state::rc_services_in_state(flags::STARTED));
    let mut ret = 0;
    for service in &services {
        for name in reexports(tree.as_ref(), service) {
            let Some(value) = rc::service::rc_service_getenv(service, &name) else {
                let e = openrc_compat::sys::Errno::last();
                ewarn(Some(&bcat!(
                    APPLET,
                    ": failed to read variable '",
                    name,
                    "' for '",
                    service,
                    "': ",
                    e.strerror()
                )));
                ret = 1;
                continue;
            };
            let value = bytes::c_str_prefix(&value);
            let mut out = Vec::new();
            if export {
                out.extend_from_slice(b"export ");
            }
            out.extend_from_slice(&bcat!(name, "="));
            if escape && value.iter().any(|b| SPECIAL.contains(b)) {
                escape_variable(value, &mut out);
            } else {
                out.extend_from_slice(value);
            }
            out.push(sep);
            stdio::out(&out);
        }
    }
    ret
}

pub fn main(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let argc = args.len();
    let usage = Usage {
        applet: APPLET.to_vec(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: None,
        usagestring: Some("rc-environ [-s <service> ...] [-r <runlevel> ...] [-en0]\n"),
    };
    let longopts = usage.all_longopts();
    let mut services: Option<Vec<Vec<u8>>> = None;
    let (mut export, mut escape, mut sep) = (false, true, b'\n');

    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match u8::try_from(opt).unwrap_or(0) {
            b'e' => export = true,
            b'n' => escape = false,
            b'0' => sep = 0,
            b'r' => {
                let rl = g.optarg.clone().unwrap_or_default();
                services.get_or_insert_default().extend(state::rc_services_in_runlevel(Some(&rl)));
            }
            b's' => {
                let s = g.optarg.clone().unwrap_or_default();
                let list = services.get_or_insert_default();
                if !list.contains(&s) {
                    list.push(s);
                }
            }
            _ => usage.common_option(opt, argc),
        }
    }

    let rest = &g.args[g.optind..];
    match rest.split_first() {
        None => print_environment(services, escape, export, sep),
        Some((service, vars)) => set_environment(service, vars),
    }
}

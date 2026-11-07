//! `rc-update`: port of `src/rc-update/rc-update.c`.  Add services to,
//! and delete them from, runlevels; stack runlevels; show what is where.

use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::{bcat, env, stdio, sys};
use openrc_einfo::{eerror, eerrorx, einfo, ewarnx};
use rc::state;

use crate::shared::{Usage, default_runlevel, getoptstring_common, rc_deptree_load_updating};

const GETOPTSTRING: &str = concat!("asu", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("all", HasArg::No, b'a'),
    LongOpt::new("stack", HasArg::No, b's'),
    LongOpt::new("update", HasArg::No, b'u'),
];
const LONGOPTS_HELP: &[&str] =
    &["Process all runlevels", "Stack a runlevel instead of a service", "Force an update of the dependency tree"];
const USAGESTRING: &str = "Usage: rc-update [options] add <service> [<runlevel>...]\n   or: rc-update [options] del <service> [<runlevel>...]\n   or: rc-update [options] [show [<runlevel>...]]";

/// Number of changes made: -1 = none (error), 0 = none (nothing to do),
/// 1 = the runlevel was updated.
type ActFn = fn(&[u8], &[u8], &[u8]) -> i32;

fn add(applet: &[u8], runlevel: &[u8], service: &[u8]) -> i32 {
    if let Err(e) = state::rc_service_exists(service) {
        if e == sys::libc::ENOEXEC {
            eerror(Some(&bcat!(applet, ": service `", service, "' is not executable")));
        } else {
            eerror(Some(&bcat!(applet, ": service `", service, "' does not exist")));
        }
        -1
    } else if state::rc_service_in_runlevel(service, runlevel) {
        einfo(Some(&bcat!(applet, ": ", service, " already installed in runlevel `", runlevel, "'; skipping")));
        0
    } else {
        match state::rc_service_add(runlevel, service) {
            Ok(()) => {
                einfo(Some(&bcat!("service ", service, " added to runlevel ", runlevel)));
                1
            }
            Err(e) => {
                eerror(Some(&bcat!(
                    applet,
                    ": failed to add service `",
                    service,
                    "' to runlevel `",
                    runlevel,
                    "': ",
                    sys::Errno(e).strerror()
                )));
                -1
            }
        }
    }
}

fn delete(applet: &[u8], runlevel: &[u8], service: &[u8]) -> i32 {
    if state::rc_service_delete(runlevel, service) {
        einfo(Some(&bcat!("service ", service, " deleted from runlevel ", runlevel)));
        return 1;
    }
    let e = sys::Errno::last();
    if e.0 == sys::libc::ENOENT {
        eerror(Some(&bcat!(applet, ": service `", service, "' is not in the runlevel `", runlevel, "'")));
    } else {
        eerror(Some(&bcat!(
            applet,
            ": failed to delete service `",
            service,
            "' from runlevel `",
            runlevel,
            "': ",
            e.strerror()
        )));
    }
    -1
}

fn addstack(applet: &[u8], runlevel: &[u8], stack: &[u8]) -> i32 {
    use rc::state::{RC_LEVEL_BOOT, RC_LEVEL_SHUTDOWN, RC_LEVEL_SINGLE, RC_LEVEL_SYSINIT};

    for level in [runlevel, stack] {
        if !state::rc_runlevel_exists(level) {
            eerror(Some(&bcat!(applet, ": runlevel `", level, "' does not exist")));
            return -1;
        }
    }
    if runlevel == stack {
        eerror(Some(&bcat!(applet, ": cannot stack `", stack, "' onto itself")));
        return -1;
    }
    let special = [RC_LEVEL_SYSINIT, RC_LEVEL_BOOT, RC_LEVEL_SINGLE, RC_LEVEL_SHUTDOWN];
    if special.iter().any(|&l| runlevel == l || stack == l) {
        // QUIRK: always names sysinit, whichever special runlevel it was.
        eerror(Some(&bcat!(applet, ": cannot stack the ", RC_LEVEL_SYSINIT, " runlevel")));
        return -1;
    }
    if !state::rc_runlevel_stack(runlevel, stack) {
        let e = sys::Errno::last();
        eerror(Some(&bcat!(applet, ": failed to stack `", stack, "' to `", runlevel, "': ", e.strerror())));
        return -1;
    }
    einfo(Some(&bcat!("runlevel ", stack, " added to runlevel ", runlevel)));
    1
}

fn delstack(applet: &[u8], runlevel: &[u8], stack: &[u8]) -> i32 {
    if state::rc_runlevel_unstack(runlevel, stack) {
        einfo(Some(&bcat!("runlevel ", stack, " deleted from runlevel ", runlevel)));
        return 1;
    }
    let e = sys::Errno::last();
    if e.0 == sys::libc::ENOENT {
        eerror(Some(&bcat!(applet, ": runlevel `", stack, "' is not in the runlevel `", runlevel, "'")));
    } else {
        eerror(Some(&bcat!(
            applet,
            ": failed to delete runlevel `",
            stack,
            "' from runlevel `",
            runlevel,
            "': ",
            e.strerror()
        )));
    }
    -1
}

fn show(runlevels: &[Vec<u8>], verbose: bool) {
    let mut services = state::rc_services_in_runlevel(None);
    services.sort();
    for service in &services {
        let mut line = Vec::new();
        let mut inone = false;
        for runlevel in runlevels {
            line.push(b' ');
            if state::rc_service_in_runlevel(service, runlevel) {
                // rc_stringlist_add copies the string: up to any NUL.
                line.extend_from_slice(openrc_compat::bytes::c_str_prefix(runlevel));
                inone = true;
            } else {
                line.extend(std::iter::repeat_n(b' ', runlevel.len()));
            }
        }
        if inone || verbose {
            // " %26s |": right-aligned in 26 bytes (see OpenRC issue 937).
            let name = openrc_compat::bytes::c_str_prefix(service);
            let mut out = b" ".to_vec();
            out.extend(std::iter::repeat_n(b' ', 26usize.saturating_sub(name.len())));
            out.extend_from_slice(name);
            out.extend_from_slice(b" |");
            out.extend_from_slice(&line);
            out.push(b'\n');
            stdio::out(&out);
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Action {
    Add,
    Delete,
    Show,
}

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let argc = args.len();
    let usage = Usage {
        applet: applet.to_vec(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: None,
        usagestring: Some(USAGESTRING),
    };
    let longopts = usage.all_longopts();

    let (mut stack, mut all_runlevels) = (false, false);
    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match u8::try_from(opt).unwrap_or(0) {
            b'a' => all_runlevels = true,
            b's' => stack = true,
            // QUIRK: acts at once (later arguments are ignored); exits with
            // the update's result, 0 or -1 (255).
            b'u' => {
                let mut ret = 0;
                let _ = rc_deptree_load_updating(-1, Some(&mut ret));
                return ret;
            }
            _ => usage.common_option(opt, argc),
        }
    }

    let verbose = rc::misc::rc_yesno(env::get("EINFO_VERBOSE").as_deref());

    // (C's "cannot mix commands" check runs before any command is read,
    // so it never fires.)
    let args: &[Vec<u8>] = &g.args;
    let mut optind = g.optind;
    let action = match args.get(optind) {
        None => Action::Show,
        Some(cmd) => {
            let action = match &cmd[..] {
                b"add" => Action::Add,
                b"delete" | b"del" => Action::Delete,
                b"show" => Action::Show,
                _ => eerrorx(Some(&bcat!(applet, ": invalid command `", cmd, "'"))),
            };
            optind += 1;
            action
        }
    };

    let mut runlevels: Vec<Vec<u8>> = Vec::new();
    let mut service: Option<&[u8]> = None;
    if optind >= args.len() {
        if action != Action::Show {
            eerrorx(Some(&bcat!(applet, ": no service specified")));
        }
    } else {
        service = Some(&args[optind]);
        for a in &args[optind + 1..] {
            if !state::rc_runlevel_exists(a) {
                eerrorx(Some(&bcat!(applet, ": `", a, "' is not a valid runlevel")));
            }
            runlevels.push(a.clone());
        }
    }

    let mut retval = 0;
    if action == Action::Show {
        // QUIRK: the first argument after `show` is a runlevel too, but
        // isn't checked to exist.
        if let Some(s) = service {
            runlevels.push(s.to_vec());
        }
        if runlevels.is_empty() {
            runlevels = state::rc_runlevel_list();
        }
        runlevels.sort();
        show(&runlevels, verbose);
        return retval;
    }

    let service = service.expect("checked above");
    let actfunc: ActFn = if action == Action::Add {
        if all_runlevels {
            eerrorx(Some(&bcat!(applet, ": the -a option is invalid with add")));
        }
        if stack { addstack } else { add }
    } else if stack {
        delstack
    } else {
        delete
    };

    if runlevels.is_empty() {
        if all_runlevels {
            runlevels = state::rc_runlevel_list();
        } else {
            runlevels.push(state::rc_runlevel_get().unwrap_or_else(default_runlevel));
        }
    }
    if runlevels.is_empty() {
        eerrorx(Some(&bcat!(applet, ": no runlevels found")));
    }

    let mut num_updated = 0;
    for runlevel in &runlevels {
        // QUIRK: a missing runlevel (only possible from softlevel or the
        // default) is reported but doesn't make the exit status fail.
        if !state::rc_runlevel_exists(runlevel) {
            eerror(Some(&bcat!(applet, ": runlevel `", runlevel, "' does not exist")));
            continue;
        }
        let ret = actfunc(applet, runlevel, service);
        if ret < 0 {
            retval = 1;
        }
        num_updated += ret;
    }

    if retval == 0 && num_updated == 0 && action == Action::Delete {
        ewarnx(Some(&bcat!(applet, ": service `", service, "' not found in any of the specified runlevels")));
    }
    retval
}

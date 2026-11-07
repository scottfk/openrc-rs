//! `rc-service`: port of `src/rc-service/rc-service.c`.  Find a service's
//! init script and run it with the given commands, in a cleaned-up
//! environment.

use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::{bcat, env, stdio, sys};
use openrc_einfo::eerrorx;
use rc::state::{self, flags};

use crate::shared::{Usage, env_config, env_filter, getoptstring_common};

const GETOPTSTRING: &str = concat!("cdDe:ilr:INsSZ", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("debug", HasArg::No, b'd'),
    LongOpt::new("nodeps", HasArg::No, b'D'),
    LongOpt::new("exists", HasArg::Required, b'e'),
    LongOpt::new("ifcrashed", HasArg::No, b'c'),
    LongOpt::new("ifexists", HasArg::No, b'i'),
    LongOpt::new("ifinactive", HasArg::No, b'I'),
    LongOpt::new("ifnotstarted", HasArg::No, b'N'),
    LongOpt::new("ifstarted", HasArg::No, b's'),
    LongOpt::new("ifstopped", HasArg::No, b'S'),
    LongOpt::new("list", HasArg::No, b'l'),
    LongOpt::new("resolve", HasArg::Required, b'r'),
    LongOpt::new("dry-run", HasArg::No, b'Z'),
];
const LONGOPTS_HELP: &[&str] = &[
    "set xtrace when running the command",
    "ignore dependencies",
    "tests if the service exists or not",
    "if the service is crashed run the command",
    "if the service exists run the command",
    "if the service is inactive run the command",
    "if the service is not started run the command",
    "if the service is started run the command",
    "if the service is stopped run the command",
    "list all available services",
    "resolve the service name to an init script",
    "dry run (show what would happen)",
];
const USAGESTRING: &str = "Usage: rc-service [options] [-i] <service> <cmd>...\n   or: rc-service [options] -e <service>\n   or: rc-service [options] -l\n   or: rc-service [options] -r <service>";

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

    // Ensure that we are only quiet when explicitly told to be.
    env::unset("EINFO_QUIET");

    let (mut if_crashed, mut if_exists, mut if_inactive) = (false, false, false);
    let (mut if_notstarted, mut if_started, mut if_stopped) = (false, false, false);
    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match u8::try_from(opt).unwrap_or(0) {
            b'd' => env::set("RC_DEBUG", b"yes"),
            b'D' => env::set("RC_NODEPS", b"yes"),
            // -e, -l and -r act at once: later arguments are ignored.
            b'e' => {
                let found = state::rc_service_resolve(g.optarg.as_deref().unwrap_or_default()).is_some();
                return if found { 0 } else { 1 };
            }
            b'c' => if_crashed = true,
            b'i' => if_exists = true,
            b'I' => if_inactive = true,
            b'N' => if_notstarted = true,
            b'l' => {
                let mut list = state::rc_services_in_runlevel(None);
                if list.is_empty() {
                    return 1;
                }
                list.sort();
                for s in list {
                    stdio::out(&bcat!(openrc_compat::bytes::c_str_prefix(&s), "\n"));
                }
                return 0;
            }
            b'r' => {
                let Some(service) = state::rc_service_resolve(g.optarg.as_deref().unwrap_or_default()) else {
                    return 1;
                };
                stdio::out(&bcat!(service, "\n"));
                return 0;
            }
            b's' => if_started = true,
            b'S' => if_stopped = true,
            b'Z' => env::set("IN_DRYRUN", b"yes"),
            _ => usage.common_option(opt, argc),
        }
    }

    let mut argv: Vec<Vec<u8>> = g.args[g.optind..].to_vec();
    let Some(name) = argv.first().cloned() else {
        eerrorx(Some(&bcat!(applet, ": you need to specify a service")));
    };
    let Some(service) = state::rc_service_resolve(&name) else {
        if if_exists {
            return 0;
        }
        eerrorx(Some(&bcat!(applet, ": service `", name, "' does not exist")));
    };
    let st = state::rc_service_state(&name);
    // `!(rc_service_daemons_crashed() && errno != EACCES)`, with errno as
    // the call leaves it.
    if if_crashed && !(rc::daemon::rc_service_daemons_crashed(&name) && sys::Errno::last().0 != sys::libc::EACCES) {
        return 0;
    }
    if if_inactive && st & flags::INACTIVE == 0 {
        return 0;
    }
    if if_notstarted && st & flags::STARTED != 0 {
        return 0;
    }
    if if_started && st & flags::STARTED == 0 {
        return 0;
    }
    if if_stopped && st & flags::STOPPED == 0 {
        return 0;
    }
    env_filter();
    env_config();
    argv[0] = service;
    let e = sys::execv(&argv[0], &argv);
    eerrorx(Some(&bcat!(applet, ": Failed executing '", argv[0], "': ", e.strerror())));
}

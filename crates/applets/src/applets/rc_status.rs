//! `rc-status`: port of `src/rc-status/rc-status.c`.  Show the status of
//! the services in runlevels, in start order.

use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::stdio::{self, Stream};
use openrc_compat::{bcat, env, sys};
use openrc_einfo::{EColor, ebracket, ecolor, eerror, eerrorx};
use rc::deptree::{self, DepTree, options};
use rc::service::rc_service_value_get;
use rc::state::{self, RC_LEVEL_BOOT, RC_LEVEL_SYSINIT, flags};

use crate::shared::{Usage, default_runlevel, getoptstring_common, rc_deptree_load_updating};

const GETOPTSTRING: &str = concat!("acf:i:lmrsSu", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("all", HasArg::No, b'a'),
    LongOpt::new("crashed", HasArg::No, b'c'),
    LongOpt::new("format", HasArg::Required, b'f'),
    LongOpt::new("in-state", HasArg::Required, b'i'),
    LongOpt::new("list", HasArg::No, b'l'),
    LongOpt::new("manual", HasArg::No, b'm'),
    LongOpt::new("runlevel", HasArg::No, b'r'),
    LongOpt::new("servicelist", HasArg::No, b's'),
    LongOpt::new("supervised", HasArg::No, b'S'),
    LongOpt::new("unused", HasArg::No, b'u'),
];
const LONGOPTS_HELP: &[&str] = &[
    "Show services from all run levels",
    "Show crashed services",
    "format status to be parsable (currently arg must be ini)",
    "Show services which are in this state",
    "Show list of run levels",
    "Show manually started services",
    "Show the name of the current runlevel",
    "Show service list",
    "show supervised services",
    "Show services not assigned to any runlevel",
];
const USAGESTRING: &str = "Usage: rc-status [-C] [-f ini] [-i state] [runlevel]\n   or: rc-status [-C] [-f ini] [-a | -m | -S | -s | -u]\n   or: rc-status [-C] [-c | -l | -r]";

/// `rc_service_state_names[]` (shared/misc.h), minus the terminator.
const STATE_NAMES: &[(i32, &[u8])] = &[
    (flags::STARTED, b"started"),
    (flags::STOPPED, b"stopped"),
    (flags::STARTING, b"starting"),
    (flags::STOPPING, b"stopping"),
    (flags::INACTIVE, b"inactive"),
    (flags::WASINACTIVE, b"wasinactive"),
    (flags::HOTPLUGGED, b"hotplugged"),
    (flags::FAILED, b"failed"),
    (flags::SCHEDULED, b"scheduled"),
    (flags::CRASHED, b"crashed"),
];

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Default,
    Ini,
}

/// C's file-scope state: the deptree (loaded on first use, and retried
/// while it fails) and the output format.
struct Status {
    deptree: Option<DepTree>,
    format: Format,
    accept: i32,
    reject: i32,
}

fn print_level(prefix: Option<&str>, level: &[u8], format: Format) {
    let mut out = Vec::new();
    match format {
        Format::Default => {
            if let Some(p) = prefix {
                out.extend_from_slice(&bcat!(p, " "));
            }
            out.extend_from_slice(b"Runlevel: ");
            if Stream::Stdout.isatty() {
                out.extend_from_slice(&bcat!(ecolor(EColor::Hilite), level, ecolor(EColor::Normal), "\n"));
            } else {
                out.extend_from_slice(&bcat!(level, "\n"));
            }
        }
        Format::Ini => {
            out.push(b'[');
            if let Some(p) = prefix {
                out.extend_from_slice(&bcat!(p, " "));
            }
            out.extend_from_slice(&bcat!(level, "]\n"));
        }
    }
    stdio::out(&out);
}

/// `sscanf(s, "%4d-%2d-%2d %2d:%2d:%2d", ...)`: the fields converted
/// before the first mismatch.
fn scan_time(s: &[u8]) -> Vec<i32> {
    let mut vals = Vec::new();
    let mut i = 0;
    let fields: [(usize, Option<u8>); 6] =
        [(4, Some(b'-')), (2, Some(b'-')), (2, Some(b' ')), (2, Some(b':')), (2, Some(b':')), (2, None)];
    for (width, sep) in fields {
        // %d skips leading white space; the width counts the sign.
        while s.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let start = i;
        let mut neg = false;
        if let Some(&c @ (b'+' | b'-')) = s.get(i) {
            neg = c == b'-';
            i += 1;
        }
        let digits_start = i;
        let mut v: i64 = 0;
        while i - start < width
            && let Some(&d) = s.get(i).filter(|d| d.is_ascii_digit())
        {
            v = v * 10 + i64::from(d - b'0');
            i += 1;
        }
        if i == digits_start {
            return vals;
        }
        vals.push((if neg { -v } else { v }) as i32);
        match sep {
            // A space in the format matches any amount of white space.
            Some(b' ') => {
                while s.get(i).is_some_and(u8::is_ascii_whitespace) {
                    i += 1;
                }
            }
            Some(c) => {
                if s.get(i) != Some(&c) {
                    return vals;
                }
                i += 1;
            }
            None => {}
        }
    }
    vals
}

/// `to_time_t()` (shared/misc.c): -1 unless all six fields parse.
fn to_time_t(s: &[u8]) -> i64 {
    match scan_time(openrc_compat::bytes::c_str_prefix(s))[..] {
        [y, mo, d, h, mi, sec] => sys::mktime(y, mo, d, h, mi, sec),
        _ => -1,
    }
}

fn get_uptime(service: &[u8]) -> Option<Vec<u8>> {
    if state::rc_service_state(service) & flags::STARTED == 0 {
        return None;
    }
    let start_count = rc_service_value_get(service, b"start_count");
    let start_time = rc_service_value_get(service, b"start_time");
    let (Some(count), Some(time)) = (start_count, start_time) else { return None };
    let count = openrc_compat::bytes::c_str_prefix(&count);
    // (int64_t) difftime(time(NULL), start_time)
    let mut secs = (sys::time() as f64 - to_time_t(&time) as f64) as i64;
    let days = secs / 86400;
    secs %= 86400;
    let hours = secs / 3600;
    secs %= 3600;
    let mins = secs / 60;
    secs %= 60;
    Some(if days > 0 {
        bcat!(format!("{days} day(s) {hours:02}:{mins:02}:{secs:02} (").as_bytes(), count, ")")
    } else {
        bcat!(format!("{hours:02}:{mins:02}:{secs:02} (").as_bytes(), count, ")")
    })
}

fn print_service(service: &[u8], format: Format, accept: i32, reject: i32) {
    let c = ecolor(EColor::Good).to_bytes();
    let st = state::rc_service_state(service);
    if st & accept == 0 || st & reject != 0 {
        return;
    }
    let mut color = EColor::Bad;
    let status: Vec<u8> = if st & flags::STOPPING != 0 {
        b"stopping ".to_vec()
    } else if st & flags::STARTING != 0 {
        color = EColor::Warn;
        b"starting ".to_vec()
    } else if st & flags::INACTIVE != 0 {
        color = EColor::Warn;
        b"inactive ".to_vec()
    } else if st & flags::STARTED != 0 {
        sys::set_errno(0);
        if rc::daemon::rc_service_daemons_crashed(service) && sys::Errno::last().0 != sys::libc::EACCES {
            let child_pid = rc_service_value_get(service, b"child_pid");
            let start_time = rc_service_value_get(service, b"start_time");
            if start_time.is_some() && child_pid.is_some() { b" unsupervised ".to_vec() } else { b" crashed ".to_vec() }
        } else {
            color = EColor::Good;
            match get_uptime(service) {
                Some(up) => bcat!(" started ", up),
                None => b" started ".to_vec(),
            }
        }
    } else if st & flags::SCHEDULED != 0 {
        color = EColor::Warn;
        b"scheduled".to_vec()
    } else if st & flags::FAILED != 0 {
        color = EColor::Warn;
        b"failed".to_vec()
    } else {
        b" stopped ".to_vec()
    };

    match format {
        Format::Default => {
            let name = openrc_compat::bytes::c_str_prefix(service);
            // cols = printf(" %s", service)
            let cols = stdio::out(&bcat!(" ", name)) as i32;
            if !c.is_empty() && Stream::Stdout.isatty() {
                stdio::out(b"\n");
            }
            ebracket(cols, color, Some(&status));
        }
        Format::Ini => {
            stdio::out(&bcat!(openrc_compat::bytes::c_str_prefix(service), " = ", status, "\n"));
        }
    }
}

impl Status {
    fn print_services_in_state(&mut self, runlevel: Option<&[u8]>, svcs: &[Vec<u8>], accept: i32, reject: i32) {
        if self.deptree.is_none() {
            self.deptree = rc_deptree_load_updating(0, None);
        }
        let Some(tree) = &self.deptree else {
            for s in svcs {
                if runlevel.is_none_or(|r| state::rc_service_in_runlevel(s, r)) {
                    print_service(s, self.format, accept, reject);
                }
            }
            return;
        };
        let types = [b"ineed".to_vec(), b"iuse".to_vec(), b"iafter".to_vec()];
        let r = match runlevel {
            Some(r) => r.to_vec(),
            None => default_runlevel(),
        };
        let (l, _) = deptree::rc_deptree_depends(
            tree,
            Some(&types),
            svcs,
            Some(&r),
            options::STRICT | options::TRACE | options::START,
        );
        for s in &l {
            if !svcs.contains(s) {
                continue;
            }
            if runlevel.is_none_or(|r| state::rc_service_in_runlevel(s, r)) {
                print_service(s, self.format, accept, reject);
            }
        }
    }

    fn print_services(&mut self, svcs: &[Vec<u8>]) {
        self.print_services_in_state(None, svcs, -1, 0);
    }

    fn print_stacked_services(&mut self, runlevel: &[u8], levels: &[Vec<u8>]) {
        for stacked in state::rc_runlevel_stacks(runlevel) {
            if levels.contains(&stacked) {
                continue;
            }
            print_level(Some("Stacked"), &stacked, self.format);
            let list = state::rc_services_in_runlevel(Some(&stacked));
            self.print_services_in_state(Some(&stacked), &list, self.accept, self.reject);
        }
    }
}

/// The services in no runlevel.
fn unassigned() -> Vec<Vec<u8>> {
    let levels = state::rc_runlevel_list();
    let mut services = state::rc_services_in_runlevel(None);
    services.retain(|s| !levels.iter().any(|l| state::rc_service_in_runlevel(s, l)));
    services
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

    let mut s = Status { deptree: None, format: Format::Default, accept: -1, reject: 0 };
    let mut levels: Vec<Vec<u8>> = Vec::new();
    let mut show_all = false;

    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match u8::try_from(opt).unwrap_or(0) {
            b'a' => {
                show_all = true;
                levels = state::rc_runlevel_list();
            }
            b'c' => {
                let mut retval = 1;
                for svc in state::rc_services_in_state(flags::STARTED) {
                    if rc::daemon::rc_service_daemons_crashed(&svc) {
                        stdio::out(&bcat!(openrc_compat::bytes::c_str_prefix(&svc), "\n"));
                        retval = 0;
                    }
                }
                return retval;
            }
            b'f' => {
                if g.optarg.as_deref().unwrap_or_default().eq_ignore_ascii_case(b"ini") {
                    s.format = Format::Ini;
                    env::set("EINFO_QUIET", b"YES");
                } else {
                    eerrorx(Some(&bcat!(applet, ": invalid argument to --format switch")));
                }
            }
            b'i' => {
                let arg = g.optarg.clone().unwrap_or_default();
                if s.accept == -1 {
                    s.accept = 0;
                }
                // print_service() doesn't print wasinactive.
                let Some(&(st, _)) =
                    STATE_NAMES.iter().find(|(st, name)| *st != flags::WASINACTIVE && *name == &arg[..])
                else {
                    eerrorx(Some(&bcat!(applet, ": invalid argument to --in-state switch: ", arg)));
                };
                s.accept |= st;
                // `-i started` hides crashed services unless `-i crashed`
                // came first; likewise `stopped` and `failed`.
                if st == flags::STARTED && s.accept & flags::CRASHED == 0 {
                    s.reject |= flags::CRASHED;
                } else if st == flags::CRASHED {
                    s.reject &= !flags::CRASHED;
                }
                if st == flags::STOPPED && s.accept & flags::FAILED == 0 {
                    s.reject |= flags::FAILED;
                } else if st == flags::FAILED {
                    s.reject &= !flags::FAILED;
                }
            }
            b'l' => {
                for l in state::rc_runlevel_list() {
                    stdio::out(&bcat!(openrc_compat::bytes::c_str_prefix(&l), "\n"));
                }
                return 0;
            }
            b'm' => {
                let mut services = unassigned();
                services.retain(|svc| state::rc_service_state(svc) & (flags::STOPPED | flags::HOTPLUGGED) == 0);
                s.print_services(&services);
                return 0;
            }
            b'r' => {
                let runlevel = state::rc_runlevel_get().unwrap_or_default();
                stdio::out(&bcat!(runlevel, "\n"));
                return 0;
            }
            b'S' => {
                let mut services = state::rc_services_in_state(flags::STARTED);
                services.retain(|svc| rc_service_value_get(svc, b"child_pid").is_some());
                s.print_services(&services);
                return 0;
            }
            b's' => {
                let services = state::rc_services_in_runlevel(None);
                s.print_services(&services);
                return 0;
            }
            b'u' => {
                let services = unassigned();
                s.print_services(&services);
                return 0;
            }
            _ => usage.common_option(opt, argc),
        }
    }

    let rest = &g.args[g.optind..];
    let mut levels_given = false;
    let mut valid = 0;
    for a in rest {
        if state::rc_runlevel_exists(a) {
            levels_given = true;
            levels.push(a.clone());
            valid += 1;
        } else {
            eerror(Some(&bcat!("runlevel `", a, "' does not exist")));
        }
    }
    if !rest.is_empty() && valid == 0 {
        stdio::exit(1);
    }
    let mut runlevel: Option<Vec<u8>> = None;
    if levels.is_empty() {
        let r = default_runlevel();
        levels.push(r.clone());
        runlevel = Some(r);
    }

    // Output the services in the order in which they would start.
    s.deptree = rc_deptree_load_updating(0, None);

    for l in &levels {
        print_level(None, l, s.format);
        let services = state::rc_services_in_runlevel(Some(l));
        s.print_services_in_state(Some(l), &services, s.accept, s.reject);
        s.print_stacked_services(l, &levels);
    }

    if show_all || !levels_given {
        // Hotplugged services.
        print_level(Some("Dynamic"), b"hotplugged", s.format);
        let services = state::rc_services_in_state(flags::HOTPLUGGED);
        s.print_services_in_state(None, &services, s.accept, s.reject);

        // Manually started, and unassigned depended-on, services.
        if show_all {
            levels = vec![runlevel.take().unwrap_or_else(default_runlevel)];
        }
        levels.push(RC_LEVEL_SYSINIT.to_vec());
        levels.push(RC_LEVEL_BOOT.to_vec());
        let mut services = state::rc_services_in_runlevel(None);
        let mut sservices: Vec<Vec<u8>> = Vec::new();
        for l in &levels {
            sservices.extend(state::rc_services_in_runlevel_stacked(Some(l)));
        }
        services.retain(|svc| {
            let st = state::rc_service_state(svc);
            !((sservices.contains(svc) || st & (flags::STOPPED | flags::HOTPLUGGED) != 0) && st & flags::FAILED == 0)
        });

        let needsme = [b"needsme".to_vec(), b"wantsme".to_vec()];
        let mut nservices: Vec<Vec<u8>> = Vec::new();
        for level in &levels {
            let mut keep = Vec::new();
            for svc in services.drain(..) {
                env::set_bytes(b"RC_SVCNAME", &svc);
                // Deviation: C dereferences a NULL deptree here; we treat
                // it as empty (nothing is needed or wanted).
                let needed = s.deptree.as_ref().is_some_and(|tree| {
                    !deptree::rc_deptree_depends(
                        tree,
                        Some(&needsme),
                        std::slice::from_ref(&svc),
                        Some(level),
                        options::TRACE,
                    )
                    .0
                    .is_empty()
                });
                if needed { nservices.push(svc) } else { keep.push(svc) }
            }
            services = keep;
        }
        env::unset("RC_SVCNAME");

        print_level(Some("Dynamic"), b"needed/wanted", s.format);
        s.print_services_in_state(None, &nservices, s.accept, s.reject);
        print_level(Some("Dynamic"), b"manual", s.format);
        s.print_services_in_state(None, &services, s.accept, s.reject);
    }
    0
}

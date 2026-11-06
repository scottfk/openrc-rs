//! `einfo`, `ewarn`, `ebegin`, `eend`, ... : port of `src/einfo/einfo.c`.
//!
//! One program behind many names; the name picks the libeinfo function and
//! the arguments, joined by spaces, are the message.

use openrc_compat::bcat;
use openrc_compat::env;
use openrc_compat::num::{atoi, strtoimax};
use openrc_compat::stdio;
use openrc_compat::sys;
use openrc_einfo as e;
use openrc_einfo::EColor;

/// Poll interval for `ewaitfile`.  (Upstream's comment says usecs, but it
/// is used as nanoseconds: 20ms.)
const WAIT_INTERVAL_NS: i64 = 20_000_000;

/// glibc's `prioritynames` table.
const PRIORITYNAMES: &[(&str, i32)] = &[
    ("alert", 1),
    ("crit", 2),
    ("debug", 7),
    ("emerg", 0),
    ("err", 3),
    ("error", 3),
    ("info", 6),
    ("none", 0x10),
    ("notice", 5),
    ("panic", 0),
    ("warn", 4),
    ("warning", 4),
];

fn syslog_decode(name: &[u8]) -> i32 {
    if name.first().is_some_and(u8::is_ascii_digit) {
        return atoi(name);
    }
    PRIORITYNAMES.iter().find(|(n, _)| name.eq_ignore_ascii_case(n.as_bytes())).map_or(-1, |&(_, v)| v)
}

type MsgFn = fn(Option<&[u8]>) -> i32;
type EndFn = fn(i32, Option<&[u8]>) -> i32;

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let mut argv: &[Vec<u8>] = args.get(1..).unwrap_or(&[]);
    let mut retval: i32 = 0;
    // Stands in for C's errno, which einfo.c uses as a flag.
    let mut errno = 0;

    if applet == b"eval_ecolors" {
        let out = bcat!(
            "GOOD='",
            e::ecolor(EColor::Good),
            "'\nWARN='",
            e::ecolor(EColor::Warn),
            "'\nBAD='",
            e::ecolor(EColor::Bad),
            "'\nHILITE='",
            e::ecolor(EColor::Hilite),
            "'\nBRACKET='",
            e::ecolor(EColor::Bracket),
            "'\nNORMAL='",
            e::ecolor(EColor::Normal),
            "'\n"
        );
        stdio::out(&out);
        return 0;
    }

    if let Some(first) = argv.first() {
        if matches!(applet, b"eend" | b"ewend" | b"veend" | b"vewend" | b"ewaitfile") {
            let p = strtoimax(first, 0);
            retval = p.value as i32;
            if p.erange {
                errno = sys::libc::ERANGE;
            }
            if !p.complete(first) {
                errno = sys::libc::EINVAL;
            }
            if errno != 0 {
                // Not a number: it is part of the message, and we fail.
                retval = 1;
            } else {
                argv = &argv[1..];
            }
        } else if matches!(applet, b"esyslog" | b"elog") {
            let level = first.iter().position(|&b| b == b'.').map(|i| syslog_decode(&first[i + 1..]));
            if level.is_none_or(|l| l == -1) {
                e::eerrorx(Some(&bcat!(applet, ": invalid log level `", first, "'")));
            }
            if argv.len() < 3 {
                e::eerrorx(Some(&bcat!(applet, ": not enough arguments")));
            }
            env::unset("EINFO_LOG");
            env::set("EINFO_LOG", &argv[1]);
            argv = &argv[2..];
        }
    }

    if applet == b"ewaitfile" {
        if errno != 0 {
            e::eerrorx(Some(&bcat!(applet, ": invalid timeout")));
        }
        if argv.is_empty() {
            e::eerrorx(Some(&bcat!(applet, ": not enough arguments")));
        }
        // retval holds the timeout in seconds; <= 0 waits forever.
        let stop = sys::gettimeofday_us() + i64::from(retval) * 1_000_000;
        for file in argv {
            e::ebeginv(Some(&bcat!("Waiting for ", file)));
            loop {
                if sys::access(file, sys::libc::F_OK).is_ok() {
                    break;
                }
                if !sys::nanosleep(WAIT_INTERVAL_NS) {
                    return 1;
                }
                let now = sys::gettimeofday_us();
                if retval <= 0 || now < stop {
                    continue;
                }
                e::eendv(1, Some(&bcat!("timed out waiting for ", file)));
                return 1;
            }
            e::eendv(0, None);
        }
        return 0;
    }

    let message: Option<Vec<u8>> = (!argv.is_empty()).then(|| argv.join(&b' '));

    let mut f: Option<MsgFn> = None;
    let mut ef: Option<EndFn> = None;
    match applet {
        b"einfo" => f = Some(e::einfo),
        b"einfon" => f = Some(e::einfon),
        b"ewarn" => f = Some(e::ewarn),
        b"ewarnn" => f = Some(e::ewarnn),
        b"eerror" => {
            f = Some(e::eerror);
            retval = 1;
        }
        b"eerrorn" => {
            f = Some(e::eerrorn);
            retval = 1;
        }
        b"ebegin" => f = Some(e::ebegin),
        b"eend" => ef = Some(e::eend),
        b"ewend" => ef = Some(e::ewend),
        b"esyslog" => {
            // QUIRK: logs at `retval`, which is always 0 here (LOG_EMERG),
            // not at the level parsed from the first argument.
            e::elog(retval, Some(message.as_deref().unwrap_or(b"(null)")));
            retval = 0;
        }
        b"veinfo" => f = Some(e::einfov),
        b"veinfon" => f = Some(e::einfovn),
        b"vewarn" => f = Some(e::ewarnv),
        b"vewarnn" => f = Some(e::ewarnvn),
        b"vebegin" => f = Some(e::ebeginv),
        b"veend" => ef = Some(e::eendv),
        b"vewend" => ef = Some(e::ewendv),
        b"eindent" => e::eindent(),
        b"eoutdent" => e::eoutdent(),
        b"veindent" => e::eindentv(),
        b"veoutdent" => e::eoutdentv(),
        _ => {
            e::eerror(Some(&bcat!(applet, ": unknown applet")));
            retval = 1;
        }
    }

    if let Some(f) = f {
        f(message.as_deref());
    } else if let Some(ef) = ef {
        ef(retval, message.as_deref());
    }
    retval
}

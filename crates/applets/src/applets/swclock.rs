//! `swclock`: port of `src/swclock/swclock.c`.  Sets the system time from
//! a file's mtime (for systems without a working RTC), or with `--save`
//! touches the file.

use std::os::fd::AsFd;

use openrc_compat::bcat;
use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::sys::{self, libc};
use openrc_einfo::{eerrorx, ewarn};

use crate::shared::{Usage, getoptstring_common};

const GETOPTSTRING: &str = concat!("sw", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[LongOpt::new("save", HasArg::No, b's'), LongOpt::new("warn", HasArg::No, b'w')];
const LONGOPTS_HELP: &[&str] = &["saves the time", "no error if no reference file"];

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let argc = args.len();
    let usage = Usage {
        applet: applet.to_vec(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: Some("file"),
        usagestring: None,
    };
    let longopts = usage.all_longopts();
    let (mut sflag, mut wflag) = (false, false);
    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match u8::try_from(opt).unwrap_or(0) {
            b's' => sflag = true,
            b'w' => wflag = true,
            _ => usage.common_option(opt, argc),
        }
    }

    // Upstream hard-codes "swclock" in these messages.
    let Some(file) = g.args.get(g.optind).cloned() else {
        eerrorx(Some(b"swclock: Reference file was not specified"));
    };

    if sflag {
        let fd = match sys::openat(None, &file, libc::O_WRONLY | libc::O_CREAT, 0o644) {
            Ok(fd) => fd,
            Err(e) => eerrorx(Some(&bcat!("swclock: open: ", e.strerror()))),
        };
        if let Err(e) = sys::futimens_now(fd.as_fd()) {
            eerrorx(Some(&bcat!("swclock: futimens: ", e.strerror())));
        }
        return 0;
    }

    let st = match sys::stat(&file) {
        Ok(st) => st,
        Err(e) => {
            let msg = bcat!("swclock: `", file, "': ", e.strerror());
            if wflag && e.0 == libc::ENOENT {
                ewarn(Some(&msg));
                return 0;
            }
            eerrorx(Some(&msg));
        }
    };

    if let Err(e) = sys::settimeofday(st.mtime) {
        eerrorx(Some(&bcat!("swclock: settimeofday: ", e.strerror())));
    }
    0
}

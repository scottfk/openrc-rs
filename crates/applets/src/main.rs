//! openrc-rs multi-call binary.
//!
//! Like BusyBox, the applet to run is chosen by the name the binary is
//! invoked as (`argv[0]`), so installing means symlinking each applet name
//! to this binary.  `openrc-rs <applet> [args...]` also works, and
//! `openrc-rs --list` prints `<install-dir> <applet>` lines for installers.

mod applets;
mod plugin;
mod rc_logger;
mod shared;

use std::os::unix::ffi::OsStringExt;

use openrc_compat::bcat;
use openrc_compat::bytes::basename_c;
use openrc_compat::stdio;

fn main() {
    openrc_compat::sys::reset_sigpipe();
    let mut args: Vec<Vec<u8>> = std::env::args_os().map(OsStringExt::into_vec).collect();
    if args.is_empty() {
        args.push(Vec::new());
    }

    let mut name = basename_c(&args[0]).to_vec();
    if name == b"openrc-rs" {
        match args.get(1).map(Vec::as_slice) {
            Some(b"--list") => {
                for a in applets::APPLETS {
                    stdio::out(&bcat!(a.dir.as_str(), " ", a.name, "\n"));
                }
                stdio::exit(0);
            }
            Some(applet) if !applet.starts_with(b"-") => {
                // `openrc-rs einfo foo`: run as if invoked as `einfo foo`.
                args.remove(0);
                name = basename_c(&args[0]).to_vec();
            }
            _ => {
                stdio::err(b"usage: openrc-rs <applet> [args...] | openrc-rs --list\n");
                stdio::exit(1);
            }
        }
    }

    let code = match applets::find(&name) {
        Some(applet) => (applet.main)(&name, args),
        None => {
            stdio::err(&bcat!("openrc-rs: unknown applet '", name, "'\n"));
            1
        }
    };
    stdio::exit(code)
}

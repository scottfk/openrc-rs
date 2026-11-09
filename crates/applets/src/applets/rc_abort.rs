//! `rc-abort`: port of upstream's `src/rc-abort/rc-abort.c`.  Ask the
//! `openrc` running the runlevel (`$RC_PID`) to stop, with SIGUSR1.

use openrc_compat::{bcat, env, num, sys};
use openrc_einfo::eerrorx;

pub fn main(_applet: &[u8], _args: Vec<Vec<u8>>) -> i32 {
    let Some(pid) = env::get("RC_PID").and_then(|p| num::scanf_int(&p)) else { return 1 };
    if let Err(e) = sys::kill(pid, sys::libc::SIGUSR1) {
        eerrorx(Some(&bcat!("rc-abort: failed to signal parent ", pid.to_string(), ": ", e.strerror())));
    }
    0
}

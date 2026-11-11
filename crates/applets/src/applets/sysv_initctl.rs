//! `sysv-initctl`: port of upstream's `src/sysv-initctl/initctl.c`.  For
//! openrc-init systems: read sysvinit's `/run/initctl` requests (from
//! sysvinit's halt, reboot, telinit...) and pass them on to openrc-init's
//! FIFO.  Errors go to syslog.

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use openrc_compat::{bcat, sys};
use sys::libc;

const INIT_MAGIC: i32 = 0x0309_1969;
const INIT_CMD_RUNLVL: i32 = 1;
const INIT_CMD_SETENV: i32 = 6;
const INIT_CMD_UNSETENV: i32 = 7;
/// `sizeof(struct init_request)`: four ints and 368 bytes of data.
const REQ_SIZE: usize = 384;

static INIT_HALT: AtomicBool = AtomicBool::new(false);

fn log_err(msg: &[u8]) {
    sys::syslog(libc::LOG_ERR, msg);
}

/// `sysvinit_runlevel()`.
fn sysvinit_runlevel(runlevel: i32) {
    let cmd: &[u8] = match u8::try_from(runlevel).unwrap_or(0) {
        b'0' if INIT_HALT.load(SeqCst) => b"halt",
        b'0' => b"poweroff",
        b'1' | b'S' | b's' => b"single",
        // (C has a TODO: switch to the default runlevel?)
        b'2'..=b'5' => return,
        // RB_KEXEC: Gentoo's inittab calls /sbin/reboot -k.
        b'6' => b"kexec",
        b'Q' | b'q' => return,
        b'U' | b'u' => b"reexec",
        // C leaves the command uninitialized here (undefined behavior);
        // we send nothing.
        _ => return,
    };
    let fifo = match sys::openat(None, &bcat!(rc::paths::SVCDIR, "/init.ctl"), libc::O_WRONLY | libc::O_NONBLOCK, 0) {
        Ok(fd) => fd,
        Err(e) => {
            log_err(&bcat!("open: ", e.strerror()));
            return;
        }
    };
    // The command and its NUL.
    if let Err(e) = sys::write_once(std::os::fd::AsFd::as_fd(&fifo), &bcat!(cmd, "\0")) {
        log_err(&bcat!("write: ", e.strerror()));
    }
}

/// `sysvinit_setenv()`: only INIT_HALT matters.
fn sysvinit_setenv(data: &[u8]) {
    // (data[size - 1] = '\0', then each NUL-terminated string until an
    // empty one.)
    let data = &data[..data.len() - 1];
    for var in data.split(|&b| b == 0).take_while(|v| !v.is_empty()) {
        if var == b"INIT_HALT=HALT" {
            INIT_HALT.store(true, SeqCst);
        } else if var == b"INIT_HALT" || var.starts_with(b"INIT_HALT=") {
            INIT_HALT.store(false, SeqCst);
        }
    }
}

pub fn main(_applet: &[u8], _args: Vec<Vec<u8>>) -> i32 {
    sys::openlog(b"sysv-initctl", 0, libc::LOG_DAEMON);
    let _ = sys::symlink(b"/run/initctl", b"/dev/initctl");
    let fifo = match sys::open_rwfifo(b"/run/initctl", false) {
        Ok(fd) => fd,
        Err(e) => {
            log_err(&bcat!("open_rwfifo: ", e.strerror()));
            return 1;
        }
    };
    loop {
        let mut req = [0u8; REQ_SIZE];
        let count = sys::read_raw(fifo, &mut req);
        if count == -1 {
            log_err(&bcat!("read: ", sys::Errno::last().strerror()));
            continue;
        }
        if count as usize != REQ_SIZE {
            log_err(b"read: short count");
            continue;
        }
        let int = |i: usize| i32::from_ne_bytes(req[i * 4..i * 4 + 4].try_into().unwrap());
        if int(0) != INIT_MAGIC {
            log_err(b"invalid magic bytes");
            continue;
        }
        match int(1) {
            INIT_CMD_RUNLVL => sysvinit_runlevel(int(2)),
            INIT_CMD_SETENV | INIT_CMD_UNSETENV => sysvinit_setenv(&req[16..]),
            _ => {}
        }
    }
}

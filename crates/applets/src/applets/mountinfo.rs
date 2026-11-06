//! `mountinfo` and `do_unmount`: port of `src/mountinfo/mountinfo.c`
//! (Linux code paths).
//!
//! `mountinfo` lists mount points from /proc/mounts, filtered by POSIX
//! extended regexes (libc's, so they match C exactly) and fstab netdev
//! flags.  `do_unmount [umount args] -- [mountinfo args]` unmounts what
//! mountinfo would list, deepest first, several at a time, retrying and
//! using fuser(1) to kill processes keeping a mount busy.

use std::os::fd::{AsFd, OwnedFd};

use openrc_compat::exec::{ExecArgs, Redirect, do_exec, rc_waitpid};
use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::sys::{self, Regex, libc};
use openrc_compat::{bcat, env, stdio};
use openrc_einfo::{eerror, eerrorx, einfo, ewarn};

use crate::shared::{Usage, getoptstring_common, parse_duration};

const PROCMOUNTS: &str = "/proc/mounts";

const GETOPTSTRING: &str = concat!("f:F:n:N:o:O:p:P:iste:E:", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("fstype-regex", HasArg::Required, b'f'),
    LongOpt::new("skip-fstype-regex", HasArg::Required, b'F'),
    LongOpt::new("node-regex", HasArg::Required, b'n'),
    LongOpt::new("skip-node-regex", HasArg::Required, b'N'),
    LongOpt::new("options-regex", HasArg::Required, b'o'),
    LongOpt::new("skip-options-regex", HasArg::Required, b'O'),
    LongOpt::new("point-regex", HasArg::Required, b'p'),
    LongOpt::new("skip-point-regex", HasArg::Required, b'P'),
    LongOpt::new("options", HasArg::No, b'i'),
    LongOpt::new("fstype", HasArg::No, b's'),
    LongOpt::new("node", HasArg::No, b't'),
    LongOpt::new("netdev", HasArg::No, b'e'),
    LongOpt::new("nonetdev", HasArg::No, b'E'),
];
const LONGOPTS_HELP: &[&str] = &[
    "fstype regex to find",
    "fstype regex to skip",
    "node regex to find",
    "node regex to skip",
    "options regex to find",
    "options regex to skip",
    "point regex to find",
    "point regex to skip",
    "print options",
    "print fstype",
    "print node",
    "is it a network device",
    "is it not a network device",
];

const UMOUNT_ARGS_MAX: usize = 16;
const RUN_MAX: usize = 32;
const TRY_MAX: i32 = 3;
const TRY_DELAY_MS: i64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MountType {
    From,
    To,
    Fstype,
    Options,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetOpts {
    Ignore,
    Yes,
    No,
}

#[derive(Debug)]
struct Args {
    node_regex: Option<Regex>,
    skip_node_regex: Option<Regex>,
    fstype_regex: Option<Regex>,
    skip_fstype_regex: Option<Regex>,
    options_regex: Option<Regex>,
    skip_options_regex: Option<Regex>,
    mounts: Vec<Vec<u8>>,
    mount_type: MountType,
    netdev: NetOpts,
}

/// A /proc/mounts line split as C does (`strsep` on single spaces).
struct MountLine<'a> {
    from: &'a [u8],
    to: &'a [u8],
    fstype: &'a [u8],
    options: &'a [u8],
}

/// `process_mount()`: 0 = selected (added to `list`), otherwise skipped.
fn process_mount(list: &mut Vec<Vec<u8>>, args: &Args, m: &MountLine<'_>, netdev: i32) -> i32 {
    // Skip the really silly rootfs.
    if m.fstype == b"rootfs" {
        return -1;
    }
    let has = |r: &Option<Regex>, s: &[u8]| r.as_ref().map(|r| r.is_match(s));

    // QUIRK: with -e/-E, a mount with no fstab entry (netdev -1) is only
    // judged by netdev if mount points were given; otherwise the regex
    // filters apply instead.
    if args.netdev == NetOpts::Yes && (netdev != -1 || !args.mounts.is_empty()) {
        if netdev != 0 {
            return 1;
        }
    } else if args.netdev == NetOpts::No && (netdev != -1 || !args.mounts.is_empty()) {
        if netdev != 1 {
            return 1;
        }
    } else {
        if has(&args.node_regex, m.from) == Some(false) {
            return 1;
        }
        if has(&args.skip_node_regex, m.from) == Some(true) {
            return 1;
        }
        if has(&args.fstype_regex, m.fstype) == Some(false) {
            return -1;
        }
        if has(&args.skip_fstype_regex, m.fstype) == Some(true) {
            return -1;
        }
        if has(&args.options_regex, m.options) == Some(false) {
            return -1;
        }
        if has(&args.skip_options_regex, m.options) == Some(true) {
            return -1;
        }
    }

    if !args.mounts.is_empty() && !args.mounts.iter().any(|s| s == m.to) {
        return -1;
    }

    let p = match args.mount_type {
        MountType::From => m.from,
        MountType::To => m.to,
        MountType::Fstype => m.fstype,
        MountType::Options => m.options,
    };
    list.push(p.to_vec());
    0
}

/// `unescape_octal()`: decode the `\040`-style escapes of /proc/mounts.
/// `\\` is a backslash; malformed escapes are dropped.
fn unescape_octal(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut r = 0;
    while r < s.len() {
        if s[r] != b'\\' {
            out.push(s[r]);
            r += 1;
            continue;
        }
        r += 1;
        if s.get(r) == Some(&b'\\') {
            out.push(b'\\');
            r += 1;
            continue;
        }
        // Octal: up to 3 digits; C treats this as unsigned bits in an int
        // and stores the low byte.
        let mut n: u32 = 0;
        for _ in 0..3 {
            match s.get(r) {
                Some(&d @ b'0'..=b'7') => {
                    n = (n << 3) | u32::from(d - b'0');
                    r += 1;
                }
                _ => break,
            }
        }
        if n != 0 {
            out.push(n as u8);
        }
    }
    out
}

/// `getmntfile()` against /etc/fstab.
fn fstab_entry(file: &[u8]) -> Option<sys::MntEnt> {
    sys::read_mntent("/etc/fstab")?.into_iter().find(|e| e.dir == file)
}

/// Read /proc/mounts lines as C's xgetline sees them.
fn proc_mounts() -> Vec<Vec<u8>> {
    let data = match std::fs::read(PROCMOUNTS) {
        Ok(d) => d,
        Err(e) => {
            let errno = sys::Errno(e.raw_os_error().unwrap_or(0));
            eerrorx(Some(&bcat!("getmntinfo: ", errno.strerror())));
        }
    };
    let mut lines: Vec<Vec<u8>> =
        data.split(|&b| b == b'\n').map(|l| openrc_compat::bytes::c_str_prefix(l).to_vec()).collect();
    if data.ends_with(b"\n") || data.is_empty() {
        lines.pop();
    }
    lines
}

/// `strsep(&p, " ")`, four times.  Missing fields (impossible in
/// /proc/mounts) are empty here; C would crash.
fn split_line(line: &[u8]) -> MountLine<'_> {
    let mut it = line.splitn(5, |&b| b == b' ');
    let mut next = || it.next().unwrap_or(&[]);
    MountLine { from: next(), to: next(), fstype: next(), options: next() }
}

/// `find_mounts()` with `process_mount`; returns (list, number selected).
fn find_mounts(args: &Args) -> (Vec<Vec<u8>>, usize) {
    let mut list = Vec::new();
    let mut n = 0;
    for line in proc_mounts() {
        let m = split_line(&line);
        // QUIRK: compares the still-escaped mount point with fstab's
        // decoded one, so mount points with spaces never match.
        let netdev = match fstab_entry(m.to) {
            Some(ent) => {
                if openrc_compat::bytes::find(&ent.opts, b"_netdev").is_some() {
                    0
                } else {
                    1
                }
            }
            None => -1,
        };
        if process_mount(&mut list, args, &m, netdev) == 0 {
            n += 1;
        }
    }
    (list, n)
}

/// `is_mounted()`: is anything mounted at exactly `mntpath`?
fn is_mounted(mntpath: &[u8]) -> bool {
    // check_is_mounted() is used without fstab lookups or rootfs skipping.
    proc_mounts().iter().any(|line| unescape_octal(split_line(line).to) == mntpath)
}

/// `is_prefix()`: is `hay` strictly below `needle`?
fn is_prefix(needle: &[u8], hay: &[u8]) -> bool {
    hay.starts_with(needle) && hay.get(needle.len()) == Some(&b'/')
}

fn get_regex(applet: &[u8], pattern: &[u8]) -> Regex {
    match Regex::new(pattern, libc::REG_EXTENDED | libc::REG_NOSUB) {
        Ok(r) => r,
        // Upstream prints the regerror text, not the pattern.
        Err(msg) => eerrorx(Some(&bcat!(applet, ": invalid regex `", msg, "'"))),
    }
}

/// `struct run_queue`.
#[derive(Debug)]
struct Running {
    mntpath: Vec<u8>,
    last_exec_time: i64,
    fuser_exec_time: i64,
    pid: i32,
    fuser_pid: i32,
    try_count: i32,
    fuser_stdout: Option<OwnedFd>,
}

struct Unmounter<'a> {
    applet: &'a [u8],
    umount_args: &'a [Vec<u8>],
    fuser_opt: &'static str,
    fuser_kill_prefix: &'static str,
    /// `static int fuser_exec_failed`.
    fuser_exec_failed: bool,
    /// `static char *rc_pid` (read once).
    rc_pid: Option<Option<Vec<u8>>>,
}

impl Unmounter<'_> {
    fn run_umount(&self, mntpath: &[u8]) -> i32 {
        let mut argv: Vec<Vec<u8>> = vec![b"umount".to_vec()];
        argv.extend(self.umount_args.iter().cloned());
        argv.push(mntpath.to_vec());
        let mut a = ExecArgs::new(argv);
        a.stdout = Redirect::DevNull;
        a.stderr = Redirect::DevNull;
        match do_exec(&a) {
            Ok(r) => r.pid,
            Err(e) => eerrorx(Some(&bcat!(self.applet, ": failed to run umount: ", e.strerror()))),
        }
    }

    fn fuser_run(&mut self, rp: &mut Running) {
        // If exec failed, fuser likely doesn't exist, so don't retry.
        if self.fuser_exec_failed {
            return;
        }
        let mut a = ExecArgs::new([b"fuser".as_slice(), self.fuser_opt.as_bytes(), &rp.mntpath]);
        a.stdout = Redirect::Pipe;
        a.stderr = Redirect::DevNull;
        match do_exec(&a) {
            Err(_) => self.fuser_exec_failed = true,
            Ok(r) => {
                rp.fuser_pid = r.pid;
                rp.fuser_stdout = r.stdout;
                rp.fuser_exec_time = sys::tm_now();
            }
        }
    }

    /// `fuser_decide()`: -1 to give up on this mount, 0 to retry.
    fn fuser_decide(&mut self, rp: &mut Running) -> i32 {
        let Some(fd) = rp.fuser_stdout.take() else { return 0 };
        const BUFSZ: usize = 1 << 12;
        let mut raw = vec![0u8; BUFSZ - 3];
        let n = match sys::read_once(fd.as_fd(), &mut raw) {
            Ok(n) => n,
            Err(_) => return 0,
        };
        drop(fd);
        if n == BUFSZ - 3 {
            // Possibly truncated.
            return 0;
        }
        // buf = " " + output without trailing newlines + " "
        let mut out = &raw[..n];
        while out.last() == Some(&b'\n') {
            out = &out[..out.len() - 1];
        }
        let buf = bcat!(" ", out, " ");
        // C works on this as a C string.
        let buf = openrc_compat::bytes::c_str_prefix(&buf).to_vec();
        let selfpid = format!(" {} ", sys::getpid());
        let rc_pid = self.rc_pid.get_or_insert_with(|| env::get("RC_PID")).clone();

        let using = |needle: &[u8]| openrc_compat::bytes::find(&buf, needle).is_some();
        if using(selfpid.as_bytes()) || rc_pid.as_deref().is_some_and(using) {
            // Let's not kill ourselves.
            eerror(Some(&bcat!("Unmounting ", rp.mntpath, " failed because we are using it")));
            -1
        } else if buf == b"  " {
            if rp.try_count >= TRY_MAX {
                eerror(Some(&bcat!("Unmounting ", rp.mntpath, " failed but fuser finds no one using it")));
                return -1;
            }
            // Whatever was using the mount may have stopped: one more try.
            rp.try_count = TRY_MAX;
            0
        } else {
            let sig = format!("{}{}", self.fuser_kill_prefix, if rp.try_count == TRY_MAX { "KILL" } else { "TERM" });
            let mut a =
                ExecArgs::new([b"fuser".as_slice(), sig.as_bytes(), b"-k", self.fuser_opt.as_bytes(), &rp.mntpath]);
            a.stdout = Redirect::DevNull;
            a.stderr = Redirect::DevNull;
            if let Ok(r) = do_exec(&a)
                && r.pid > 0
            {
                rc_waitpid(r.pid);
            }
            0
        }
    }

    /// The unmount state machine from `main()`; returns the exit status.
    fn run(&mut self, mut mounts: Vec<Vec<u8>>, rc_fuser_timeout: i64) -> i32 {
        #[derive(PartialEq, Eq)]
        enum State {
            Run,
            Reap,
            Retry,
            End,
        }
        let mut running: Vec<Running> = Vec::new();
        let mut num_waiting = 0usize;
        let mut result = 0;
        let mut state = State::Run;

        while state != State::End {
            match state {
                State::Run => {
                    let mut idx = 0usize;
                    while idx < mounts.len() {
                        let candidate = &mounts[idx];
                        let safe = !running.iter().any(|r| is_prefix(candidate, &r.mntpath))
                            && !mounts.iter().any(|m| is_prefix(candidate, m));
                        if !safe {
                            idx += 1;
                            continue;
                        }
                        if !is_mounted(candidate) {
                            // Probably a shared mount that went away: drop it
                            // (swap_remove, as C moves the last entry here).
                            mounts.swap_remove(idx);
                        } else {
                            break;
                        }
                    }
                    if running.len() == RUN_MAX || idx >= mounts.len() {
                        state = State::Reap;
                        continue;
                    }
                    let mntpath = mounts.swap_remove(idx);
                    let pid = self.run_umount(&mntpath);
                    running.push(Running {
                        mntpath,
                        last_exec_time: sys::tm_now(),
                        fuser_exec_time: -1,
                        pid,
                        fuser_pid: -1,
                        try_count: 0,
                        fuser_stdout: None,
                    });
                }
                State::Reap => {
                    let flags = if num_waiting > 0 { libc::WNOHANG } else { 0 };
                    let (pid, status) = sys::waitpid(-1, flags).unwrap_or((-1, 0));
                    let mut found = None;
                    for (i, rp) in running.iter_mut().enumerate() {
                        if rp.fuser_pid == pid {
                            rp.fuser_pid = -1;
                        }
                        if rp.pid == pid && pid > 0 {
                            found = Some(i);
                            break;
                        }
                    }
                    let Some(i) = found else {
                        state = State::Retry;
                        continue;
                    };
                    let succeeded = sys::exit_status(status) == Some(0) || !is_mounted(&running[i].mntpath);
                    if succeeded {
                        einfo(Some(&bcat!("Unmounted ", running[i].mntpath)));
                        running.swap_remove(i);
                        state = State::Run;
                    } else if running[i].try_count >= TRY_MAX {
                        eerror(Some(&bcat!("Failed to unmount ", running[i].mntpath)));
                        running.swap_remove(i);
                        result = 1;
                    } else {
                        // Into the waiting queue.
                        let mut rp = running.swap_remove(i);
                        rp.pid = -1;
                        rp.try_count += 1;
                        num_waiting += 1;
                        self.fuser_run(&mut rp);
                        running.push(rp);
                        let last = running.len() - 1;
                        running.swap(i, last);
                    }
                }
                State::Retry => {
                    let mut pick = None;
                    let mut next_retry = i64::MAX;
                    for (i, r) in running.iter().enumerate() {
                        if r.pid > 0 {
                            continue;
                        }
                        let t = if r.fuser_pid > 0 {
                            r.fuser_exec_time + rc_fuser_timeout
                        } else {
                            r.last_exec_time + TRY_DELAY_MS
                        };
                        if t < next_retry {
                            pick = Some(i);
                            next_retry = t;
                        }
                    }
                    let Some(i) = pick else {
                        state = if mounts.is_empty() { State::End } else { State::Run };
                        continue;
                    };
                    let mut now = sys::tm_now();
                    if (running[i].pid > 0 || running[i].fuser_pid > 0) && next_retry > now {
                        // A child may become reapable before we sleep, so
                        // stay responsive.
                        sys::tm_sleep((next_retry - now).min(500));
                        state = State::Reap;
                        now = sys::tm_now();
                    }
                    if (running[i].pid == -1 && running[i].fuser_pid == -1) || next_retry <= now {
                        if running[i].fuser_pid > 0 {
                            let _ = sys::kill(running[i].fuser_pid, libc::SIGKILL);
                            let _ = sys::waitpid(running[i].fuser_pid, 0);
                            running[i].fuser_pid = -1;
                        }
                        let mut rp = running.swap_remove(i);
                        if self.fuser_decide(&mut rp) < 0 {
                            // Abort this one.
                            result = 1;
                        } else {
                            rp.last_exec_time = sys::tm_now();
                            rp.pid = self.run_umount(&rp.mntpath);
                            running.push(rp);
                            let last = running.len() - 1;
                            running.swap(i, last);
                        }
                        num_waiting -= 1;
                        state = State::Reap;
                    }
                }
                State::End => {}
            }
        }
        result
    }
}

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let mut args = args;
    let argv0 = args.first().cloned().unwrap_or_default();
    let doing_unmount = applet == b"do_unmount";
    let mut umount_args: Vec<Vec<u8>> = Vec::new();
    let mut rc_fuser_timeout: i64 = -1;
    let (mut fuser_opt, mut fuser_kill_prefix) = ("-m", "-");

    if doing_unmount {
        // Everything up to "--" is for umount (all of it, if no "--").
        let mut rest = args.split_off(1.min(args.len())).into_iter();
        for a in rest.by_ref() {
            if a == b"--" {
                break;
            }
            if umount_args.len() >= UMOUNT_ARGS_MAX {
                eerrorx(Some(&bcat!(applet, ": Too many umount arguments")));
            }
            umount_args.push(a);
        }
        args.extend(rest);

        if let Some(t) = rc::rc_conf_value("rc_fuser_timeout") {
            match parse_duration(&t) {
                Some(ms) => rc_fuser_timeout = ms,
                None => {
                    ewarn(Some(&bcat!(applet, ": Invalid rc_fuser_timeout value: `", t, "`. Defaulting to 20")));
                }
            }
        }
        if rc_fuser_timeout < 0 {
            rc_fuser_timeout = 20 * 1000;
        }
        if env::get("RC_UNAME").is_some_and(|u| u != b"Linux") {
            fuser_opt = "-cm";
            fuser_kill_prefix = "-s";
        }
    }

    let argc = args.len();
    let usage = Usage {
        applet: applet.to_vec(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: Some("[mount1] [mount2] ..."),
        usagestring: None,
    };
    let longopts = usage.all_longopts();
    let mut a = Args {
        node_regex: None,
        skip_node_regex: None,
        fstype_regex: None,
        skip_fstype_regex: None,
        options_regex: None,
        skip_options_regex: None,
        mounts: Vec::new(),
        mount_type: MountType::To,
        netdev: NetOpts::Ignore,
    };
    let mut point_regex: Option<Regex> = None;
    let mut skip_point_regex: Option<Regex> = None;

    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        let optarg = g.optarg.clone().unwrap_or_default();
        match u8::try_from(opt).unwrap_or(0) {
            b'e' => a.netdev = NetOpts::Yes,
            b'E' => a.netdev = NetOpts::No,
            b'f' => a.fstype_regex = Some(get_regex(applet, &optarg)),
            b'F' => a.skip_fstype_regex = Some(get_regex(applet, &optarg)),
            b'n' => a.node_regex = Some(get_regex(applet, &optarg)),
            b'N' => a.skip_node_regex = Some(get_regex(applet, &optarg)),
            b'o' => a.options_regex = Some(get_regex(applet, &optarg)),
            b'O' => a.skip_options_regex = Some(get_regex(applet, &optarg)),
            b'p' => point_regex = Some(get_regex(applet, &optarg)),
            b'P' => skip_point_regex = Some(get_regex(applet, &optarg)),
            b'i' => a.mount_type = MountType::Options,
            b's' => a.mount_type = MountType::Fstype,
            b't' => a.mount_type = MountType::From,
            _ => usage.common_option(opt, argc),
        }
    }

    for path in &g.args[g.optind..] {
        if path.first() != Some(&b'/') {
            eerrorx(Some(&bcat!(argv0, ": `", path, "' is not a mount point")));
        }
        a.mounts.push(sys::realpath(path).unwrap_or_else(|| path.clone()));
    }
    let (nodes, _) = find_mounts(&a);

    // Report mounts in reverse order, to ease unmounting.
    let mut result = 1;
    let mut mounts = Vec::new();
    for s in nodes.iter().rev() {
        // QUIRK: -p/-P match whatever field is being printed (-t, -i, -s
        // change it), not necessarily the mount point.
        if point_regex.as_ref().is_some_and(|r| !r.is_match(s)) {
            continue;
        }
        if skip_point_regex.as_ref().is_some_and(|r| r.is_match(s)) {
            continue;
        }
        if doing_unmount {
            mounts.push(unescape_octal(s));
        } else if !rc::rc_yesno(env::get("EINFO_QUIET").as_deref()) {
            stdio::out(&bcat!(s, "\n"));
        }
        result = 0;
    }
    if !doing_unmount {
        return result;
    }

    let mut u = Unmounter {
        applet,
        umount_args: &umount_args,
        fuser_opt,
        fuser_kill_prefix,
        fuser_exec_failed: false,
        rc_pid: None,
    };
    u.run(mounts, rc_fuser_timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape() {
        assert_eq!(unescape_octal(br"/a\040b"), b"/a b");
        assert_eq!(unescape_octal(br"a\\b"), br"a\b");
        assert_eq!(unescape_octal(br"a\9b"), b"a9b");
        assert_eq!(unescape_octal(br"a\0b"), b"ab");
        // At most three digits: \010 then a literal 1.
        assert_eq!(unescape_octal(br"\0101"), b"\x081");
    }

    #[test]
    fn prefix() {
        assert!(is_prefix(b"/a", b"/a/b"));
        assert!(!is_prefix(b"/a", b"/ab"));
        assert!(!is_prefix(b"/a", b"/a"));
    }
}

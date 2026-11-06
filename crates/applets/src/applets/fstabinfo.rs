//! `fstabinfo`: port of `src/fstabinfo/fstabinfo.c`.  Queries (and can
//! mount or remount) entries of /etc/fstab, which is parsed with libc's
//! `getmntent` exactly as in C.

use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::num::scanf_int;
use openrc_compat::sys::{self, MntEnt};
use openrc_compat::{bcat, env, stdio};
use openrc_einfo::eerrorx;

use crate::shared::{Usage, getoptstring_common};

const FSTAB: &str = "/etc/fstab";

const GETOPTSTRING: &str = concat!("MRbmop:t:", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("mount", HasArg::No, b'M'),
    LongOpt::new("remount", HasArg::No, b'R'),
    LongOpt::new("blockdevice", HasArg::No, b'b'),
    LongOpt::new("mountargs", HasArg::No, b'm'),
    LongOpt::new("options", HasArg::No, b'o'),
    LongOpt::new("passno", HasArg::Required, b'p'),
    LongOpt::new("fstype", HasArg::Required, b't'),
];
const LONGOPTS_HELP: &[&str] = &[
    "Mounts the filesystem from the mountpoint",
    "Remounts the filesystem based on the information in fstab",
    "Extract the block device",
    "Show arguments needed to mount the entry",
    "Extract the options field",
    "Extract or query the pass number field",
    "List entries with matching file system type",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Output {
    File,
    MountArgs,
    Options,
    Passno,
    BlockDev,
    Mount,
    Remount,
}

/// A fresh pass over the fstab (`START_ENT ... END_ENT`).
fn fstab() -> Vec<MntEnt> {
    sys::read_mntent(FSTAB).unwrap_or_default()
}

/// `getmntfile()`: the first entry mounted at `file`.
fn getmntfile(file: &[u8]) -> Option<MntEnt> {
    fstab().into_iter().find(|e| e.dir == file)
}

/// `do_mount()`: run mount(8) for the entry; its exit status, or -1.
fn do_mount(applet: &[u8], ent: &MntEnt, remount: bool) -> i32 {
    let mut argv: Vec<&[u8]> = vec![b"mount", b"-o", &ent.opts, b"-t", &ent.fstype];
    if remount {
        argv.extend_from_slice(&[b"-o", b"remount"]);
    }
    argv.extend_from_slice(&[&ent.fsname, &ent.dir]);
    // posix_spawnp() with the inherited environment.
    let res = match openrc_compat::exec::do_exec(&openrc_compat::exec::ExecArgs::new(argv)) {
        Ok(r) => r,
        Err(e) => eerrorx(Some(&bcat!(applet, ": posix_spawnp: ", e.strerror()))),
    };
    let status = loop {
        match sys::waitpid(res.pid, 0) {
            Ok((_, st)) => break st,
            Err(e) if e.0 == sys::libc::EINTR => continue,
            // C then reads an uninitialised status; treat as abnormal.
            Err(_) => return -1,
        }
    };
    sys::exit_status(status).unwrap_or(-1)
}

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    // Fail if there is no /etc/fstab.
    if sys::access(FSTAB.as_bytes(), sys::libc::F_OK).is_err() {
        eerrorx(Some(b"/etc/fstab does not exist"));
    }
    // Ensure that we are only quiet when explicitly told to be.
    env::unset("EINFO_QUIET");

    let argc = args.len();
    let argv0 = args.first().cloned().unwrap_or_default();
    let usage = Usage {
        applet: applet.to_vec(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: None,
        usagestring: None,
    };
    let longopts = usage.all_longopts();

    let mut output = Output::File;
    let mut files: Vec<Vec<u8>> = Vec::new();
    let mut filtered = false;

    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        let optarg = g.optarg.clone().unwrap_or_default();
        match u8::try_from(opt).unwrap_or(0) {
            b'M' => output = Output::Mount,
            b'R' => output = Output::Remount,
            b'b' => output = Output::BlockDev,
            b'o' => output = Output::Options,
            b'm' => output = Output::MountArgs,
            b'p' => match optarg.first() {
                Some(&op @ (b'=' | b'<' | b'>')) => {
                    let Some(i) = scanf_int(&optarg[1..]) else {
                        eerrorx(Some(&bcat!(argv0, ": invalid passno ", &optarg[1..])));
                    };
                    filtered = true;
                    for ent in fstab() {
                        if ent.dir == b"none" {
                            continue;
                        }
                        let p = ent.passno;
                        if (op == b'=' && i == p) || (op == b'<' && i > p && p != 0) || (op == b'>' && i < p && p != 0)
                        {
                            files.push(ent.dir);
                        }
                    }
                }
                _ => {
                    files.push(optarg);
                    output = Output::Passno;
                }
            },
            b't' => {
                filtered = true;
                for token in optarg.split(|&b| b == b',') {
                    for ent in fstab() {
                        if ent.fstype == token {
                            files.push(ent.dir);
                        }
                    }
                }
            }
            _ => usage.common_option(opt, argc),
        }
    }

    let rest = &g.args[g.optind..];
    if !rest.is_empty() {
        if !files.is_empty() {
            // Keep only the selected entries that were also named.
            files.retain(|f| rest.contains(f));
        } else {
            files.extend(rest.iter().cloned());
        }
    } else if !filtered {
        files.extend(fstab().into_iter().map(|e| e.dir));
        if files.is_empty() {
            eerrorx(Some(&bcat!(argv0, ": empty fstab")));
        }
    }

    if files.is_empty() {
        return 1;
    }

    let mut result: i32 = 0;
    for file in &files {
        let Some(ent) = getmntfile(file) else {
            result = 1;
            continue;
        };
        match output {
            Output::Mount => result = result.wrapping_add(do_mount(applet, &ent, false)),
            Output::Remount => result = result.wrapping_add(do_mount(applet, &ent, true)),
            _ => {}
        }

        // No point in outputting if quiet.
        if rc::rc_yesno(env::get("EINFO_QUIET").as_deref()) {
            continue;
        }
        let line = match output {
            Output::BlockDev => bcat!(ent.fsname, "\n"),
            Output::MountArgs => bcat!("-o ", ent.opts, " -t ", ent.fstype, " ", ent.fsname, " ", file, "\n"),
            Output::Options => bcat!(ent.opts, "\n"),
            Output::File => bcat!(file, "\n"),
            Output::Passno => format!("{}\n", ent.passno).into_bytes(),
            Output::Mount | Output::Remount => continue,
        };
        stdio::out(&line);
    }
    result
}

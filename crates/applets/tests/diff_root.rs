//! Root-only differential tests, for the VM test bed (they skip unless run
//! as root): successful chown, the symlink-ownership rules in checkpath's
//! parent-path walk, and what reaches syslog.
//!
//! Syslog cases bind a private /dev/log for each run, so they also need
//! that no syslog daemon is running.  Never run these on a real system's
//! syslog: esyslog logs at LOG_EMERG (see QUIRKS.md).

mod common;
use std::fs;
use std::os::unix::fs::{PermissionsExt, lchown, symlink};
use std::path::Path;

use common::Case;

const NOBODY: u32 = 65534;

fn tree(r: &Path) {
    fs::create_dir(r.join("real")).unwrap();
    fs::write(r.join("real/file"), "x").unwrap();
    fs::set_permissions(r.join("real/file"), fs::Permissions::from_mode(0o644)).unwrap();
    // Created as root, so root-owned: checkpath follows these.
    symlink("real", r.join("rootlink")).unwrap();
    symlink(r.join("real"), r.join("rootlink-abs")).unwrap();
    symlink("rootlink", r.join("rootlink2")).unwrap();
    // Owned by nobody: checkpath must refuse these.
    symlink("real", r.join("userlink")).unwrap();
    lchown(r.join("userlink"), Some(NOBODY), Some(NOBODY)).unwrap();
    fs::create_dir(r.join("userdir")).unwrap();
    std::os::unix::fs::chown(r.join("userdir"), Some(NOBODY), Some(NOBODY)).unwrap();
    symlink("../real", r.join("userdir/link")).unwrap();
    lchown(r.join("userdir/link"), Some(NOBODY), Some(NOBODY)).unwrap();
}

#[test]
fn checkpath_as_root() {
    if !common::is_root() {
        eprintln!("SKIPPED: not root");
        return;
    }
    let t = tree as fn(&Path);
    let cases = vec![
        Case::new("checkpath", &["-d", "-o", "nobody", "$ROOT/new"]).sandbox(t),
        Case::new("checkpath", &["-d", "-o", "nobody:nobody", "-m", "0750", "$ROOT/new"]).sandbox(t),
        Case::new("checkpath", &["-f", "-o", "65534:0", "$ROOT/real/file"]).sandbox(t),
        Case::new("checkpath", &["-f", "-o", ":nobody", "$ROOT/real/file"]).sandbox(t),
        Case::new("checkpath", &["-d", "-o", "nobody", "$ROOT/userdir"]).sandbox(t),
        Case::new("checkpath", &["-p", "-o", "nobody", "-m", "0620", "$ROOT/fifo"]).sandbox(t),
        Case::new("checkpath", &["-f", "$ROOT/rootlink/file"]).sandbox(t),
        Case::new("checkpath", &["-d", "$ROOT/rootlink/sub"]).sandbox(t),
        Case::new("checkpath", &["-d", "$ROOT/rootlink-abs/sub"]).sandbox(t),
        Case::new("checkpath", &["-d", "$ROOT/rootlink2/sub"]).sandbox(t),
        Case::new("checkpath", &["-d", "$ROOT/userlink/sub"]).sandbox(t),
        Case::new("checkpath", &["-d", "$ROOT/userdir/link/sub"]).sandbox(t),
        Case::new("checkpath", &["-f", "$ROOT/rootlink"]).sandbox(t),
        Case::new("checkpath", &["-W", "$ROOT/real/file"]).sandbox(t),
        Case::new("checkpath", &["-W", "-o", "nobody", "-f", "$ROOT/real/new"]).sandbox(t),
        Case::new("checkpath", &["-d", "-m", "1777", "$ROOT/sticky"]).sandbox(t),
        Case::new("checkpath", &["-d", "-m", "02775", "-o", "root:nobody", "$ROOT/sgid"]).sandbox(t),
    ];
    common::check_all("checkpath-root", &cases);
}

#[test]
fn syslog_output() {
    if !common::syslog_capturable() {
        eprintln!("SKIPPED: needs root and no syslog daemon on /dev/log");
        return;
    }
    let mut cases = vec![
        Case::new("esyslog", &["daemon.notice", "mytag", "hello", "world"]),
        Case::new("esyslog", &["user.err", "mytag", "x"]),
        Case::new("esyslog", &["local0.7", "t", "numeric level"]),
        Case::new("esyslog", &["daemon.none", "t", "none"]),
        Case::new("esyslog", &["daemon.notice", "t"]),
        Case::new("esyslog", &[]).env("EINFO_LOG", "fromenv"),
        Case::new("ewarn", &["logged", "warning"]).env("EINFO_LOG", "svc"),
        Case::new("eerror", &["logged", "error"]).env("EINFO_LOG", "svc"),
        Case::new("einfo", &["not", "logged"]).env("EINFO_LOG", "svc"),
        Case::new("vewarn", &["not", "logged"]).env("EINFO_LOG", "svc").env("EINFO_VERBOSE", "yes"),
        Case::new("eend", &["1", "eend", "does", "not", "log"]).env("EINFO_LOG", "svc"),
        Case::new("ewarn", &["quiet"]).env("EINFO_LOG", "svc").env("EINFO_QUIET", "yes"),
        Case::new("eerror", &["quiet"]).env("EINFO_LOG", "svc").env("EERROR_QUIET", "yes"),
        Case::new("ewarn", &["no ident"]),
    ];
    for c in &mut cases {
        *c = c.clone().syslog();
    }
    common::check_all("syslog", &cases);
}

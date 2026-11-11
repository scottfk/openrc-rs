//! Small helpers vs the C reference: shell_var, is_newer_than,
//! is_older_than, swclock, and the value applets (service_get_value & co).
//!
//! The value applets are pointed at a sandbox with `$RC_SVCDIR`, so they
//! never touch the real /run/openrc.

mod common;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use common::Case;

#[test]
fn shell_var() {
    let cases = vec![
        Case::new("shell_var", &[]),
        Case::new("shell_var", &["simple"]),
        Case::new("shell_var", &["net.eth0", "foo-bar", "a b", "", "x/y:z", "ÄÖ", "A1_b2"]),
    ];
    common::check_all("shell_var", &cases);
}

fn set_mtime(p: &Path, secs_ago: u64) {
    let t = SystemTime::now() - Duration::from_secs(secs_ago);
    fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
}

/// old (1000s ago), mid (500s), new (10s), a tree with a new file deep
/// inside, and a tree that is old throughout.
fn mtimes(r: &Path) {
    for (name, ago) in [("old", 1000), ("mid", 500), ("new", 10)] {
        fs::write(r.join(name), name).unwrap();
        set_mtime(&r.join(name), ago);
    }
    fs::create_dir_all(r.join("tree/a/b")).unwrap();
    fs::write(r.join("tree/a/b/deep"), "x").unwrap();
    set_mtime(&r.join("tree/a/b/deep"), 5);
    fs::write(r.join("tree/.hidden"), "x").unwrap();
    fs::create_dir_all(r.join("oldtree/sub")).unwrap();
    fs::write(r.join("oldtree/sub/f"), "x").unwrap();
    set_mtime(&r.join("oldtree/sub/f"), 2000);
    fs::create_dir_all(r.join("hiddennew/.git")).unwrap();
    fs::write(r.join("hiddennew/.newer"), "x").unwrap();
    std::os::unix::fs::symlink("new", r.join("link-to-new")).unwrap();
    // Directory mtimes: make the tree directories old so only files count.
    for d in ["tree/a/b", "tree/a", "tree", "oldtree/sub", "oldtree", "hiddennew/.git", "hiddennew"] {
        let t = filetime(2000);
        let f = fs::File::open(r.join(d)).unwrap();
        f.set_modified(t).unwrap();
    }
}

fn filetime(secs_ago: u64) -> SystemTime {
    SystemTime::now() - Duration::from_secs(secs_ago)
}

#[test]
fn newer_older() {
    let t = mtimes as fn(&Path);
    let mut cases = Vec::new();
    for applet in ["is_newer_than", "is_older_than"] {
        for args in [
            &[][..],
            &["$ROOT/new"],
            &["$ROOT/new", "$ROOT/old"],
            &["$ROOT/old", "$ROOT/new"],
            &["$ROOT/mid", "$ROOT/old", "$ROOT/new"],
            &["$ROOT/new", "$ROOT/old", "$ROOT/mid"],
            &["$ROOT/new", "$ROOT/new"],
            &["$ROOT/missing", "$ROOT/old"],
            &["$ROOT/mid", "$ROOT/missing"],
            &["$ROOT/mid", "$ROOT/tree"],
            &["$ROOT/mid", "$ROOT/oldtree"],
            &["$ROOT/mid", "$ROOT/hiddennew"],
            &["$ROOT/mid", "$ROOT/link-to-new"],
            &["$ROOT/link-to-new", "$ROOT/mid"],
        ] {
            cases.push(Case::new(applet, args).sandbox(t));
        }
    }
    common::check_all("newer-older", &cases);
}

fn reffile(r: &Path) {
    fs::write(r.join("ref"), "x").unwrap();
    set_mtime(&r.join("ref"), 100);
}

#[test]
fn swclock() {
    let t = reffile as fn(&Path);
    let mut cases = vec![
        Case::new("swclock", &[]),
        Case::new("swclock", &["-h"]),
        Case::new("swclock", &["-x"]),
        Case::new("swclock", &["$ROOT/missing"]).sandbox(t),
        Case::new("swclock", &["-w", "$ROOT/missing"]).sandbox(t),
        Case::new("swclock", &["--warn", "$ROOT/nodir/missing"]).sandbox(t),
        Case::new("swclock", &["-w", "$ROOT/ref/notdir"]).sandbox(t),
        Case::new("swclock", &["-s", "$ROOT/new"]).sandbox(t),
        Case::new("swclock", &["-s", "$ROOT/nodir/new"]).sandbox(t),
        Case::new("swclock", &["--save", "$ROOT/ref", "extra", "args"]).sandbox(t),
    ];
    // Setting the clock needs root; unprivileged this is the EPERM path.
    if !common::is_root() {
        cases.push(Case::new("swclock", &["$ROOT/ref"]).sandbox(t));
    }
    common::check_all("swclock", &cases);
}

fn svcdir(r: &Path) {
    let o = r.join("svc/options/sshd");
    fs::create_dir_all(&o).unwrap();
    fs::write(o.join("port"), "22").unwrap();
    fs::write(o.join("empty"), "").unwrap();
    fs::write(o.join("nul"), b"before\0after").unwrap();
    fs::write(o.join("multi"), "line1\nline2\n").unwrap();
    fs::create_dir(o.join("adir")).unwrap();
    let e = r.join("svc/environ/sshd");
    fs::create_dir_all(&e).unwrap();
    fs::write(e.join("ALREADY"), "set").unwrap();
    fs::write(e.join("EMPTYSAVED"), "").unwrap();
}

#[test]
fn value_applets() {
    let t = svcdir as fn(&Path);
    let svc = |applet: &'static str, args: &[&str]| {
        Case::new(applet, args).sandbox(t).env("RC_SVCDIR", "$ROOT/svc").env("RC_SVCNAME", "sshd")
    };
    let mut cases = vec![
        Case::new("service_get_value", &["port"]),
        Case::new("service_export", &["X"]).env("RC_SVCDIR", "/proc/openrc-rs-nonexistent"),
        svc("service_get_value", &[]),
        svc("service_get_value", &[""]),
        svc("service_export", &[]),
        Case::new("get_options", &["port"]).sandbox(t).env("RC_SVCDIR", "$ROOT/svc").env("RC_SVCNAME", "other"),
        Case::new("service_set_value", &["k", "v"]).sandbox(t).env("RC_SVCDIR", "$ROOT/newsvc").env("RC_SVCNAME", "x"),
        Case::new("service_set_value", &["k", "v"])
            .sandbox(t)
            .env("RC_SVCDIR", "$ROOT/no/such/dir")
            .env("RC_SVCNAME", "x"),
        Case::new("service_export", &["HOME"]).sandbox(t).env("RC_SVCDIR", "$ROOT/no/such").env("RC_SVCNAME", "x"),
    ];
    for get in ["service_get_value", "get_options"] {
        // Not "adir": C reads a directory option through a freed buffer
        // (undefined behavior; see QUIRKS.md), so its output varies.
        for opt in ["port", "empty", "nul", "multi", "missing", "../environ/sshd/ALREADY"] {
            cases.push(svc(get, &[opt]));
        }
    }
    for set in ["service_set_value", "save_options"] {
        cases.push(svc(set, &["port", "2222"]));
        cases.push(svc(set, &["newopt", "value with spaces"]));
        cases.push(svc(set, &["newopt", ""]));
        cases.push(svc(set, &["port"]));
        cases.push(svc(set, &["missing"]));
        cases.push(svc(set, &["a", "b", "extra"]));
        cases.push(svc(set, &["adir", "x"]));
        cases.push(svc(set, &["sub/dir", "x"]));
    }
    cases.push(
        Case::new("service_set_value", &["k", "v"])
            .sandbox(t)
            .env("RC_SVCDIR", "$ROOT/svc")
            .env("RC_SVCNAME", "newsvc"),
    );
    cases.push(
        svc("service_export", &["ALREADY", "EMPTYSAVED", "MYVAR", "UNSET", "", "WEIRD"])
            .env("MYVAR", "hello")
            .env("EMPTYSAVED", "now")
            .env("WEIRD", "a=b"),
    );
    cases.push(
        Case::new("service_export", &["MYVAR"])
            .sandbox(t)
            .env("RC_SVCDIR", "$ROOT/svc")
            .env("RC_SVCNAME", "fresh")
            .env("MYVAR", "1"),
    );
    cases.push(svc("service_export", &["UNSET"]).env("EINFO_QUIET", "yes"));
    common::check_all("value", &cases);
}

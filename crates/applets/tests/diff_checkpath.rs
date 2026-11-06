//! checkpath vs the C reference, including the filesystem it leaves behind.
//!
//! Runs unprivileged, so chown-to-another-user cases exercise the error
//! paths; root-only behaviour (root-owned symlinks in the parent path)
//! needs the VM test bed.

mod common;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use common::Case;

fn empty(_: &Path) {}

fn populated(r: &Path) {
    fs::create_dir(r.join("dir")).unwrap();
    fs::set_permissions(r.join("dir"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(r.join("dir/inner"), "x").unwrap();
    fs::write(r.join("file"), "contents").unwrap();
    fs::set_permissions(r.join("file"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::write(r.join("linked"), "hard").unwrap();
    fs::hard_link(r.join("linked"), r.join("linked2")).unwrap();
    symlink("dir", r.join("symdir")).unwrap();
    symlink("file", r.join("symfile")).unwrap();
    symlink("nowhere", r.join("dangling")).unwrap();
    fs::create_dir(r.join("ro")).unwrap();
    fs::set_permissions(r.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
    std::process::Command::new("mkfifo").arg(r.join("fifo")).status().unwrap();
}

#[test]
fn options_and_usage() {
    let cases = vec![
        Case::new("checkpath", &[]),
        Case::new("checkpath", &["-h"]),
        Case::new("checkpath", &["--help"]),
        Case::new("checkpath", &["--he"]),
        Case::new("checkpath", &["-V"]),
        Case::new("checkpath", &["--version"]),
        Case::new("checkpath", &["-V", "-d", "/nonexistent/x"]),
        Case::new("checkpath", &["-x"]),
        Case::new("checkpath", &["--bogus"]),
        Case::new("checkpath", &["--bogus=1"]),
        Case::new("checkpath", &["--dir", "/nonexistent/x"]),
        Case::new("checkpath", &["--directory=1", "/x"]),
        Case::new("checkpath", &["-m"]),
        Case::new("checkpath", &["--mode"]),
        Case::new("checkpath", &["-m", "bogus", "/x"]),
        Case::new("checkpath", &["-m", "8", "/x"]),
        Case::new("checkpath", &["-m", "17777", "/x"]),
        Case::new("checkpath", &["-m", "-1", "/x"]),
        Case::new("checkpath", &["-o", "no-such-user-xyz", "/x"]),
        Case::new("checkpath", &["-o", ":no-such-group-xyz", "/x"]),
        Case::new("checkpath", &["-d"]),
        Case::new("checkpath", &["-d", "relative/path"]),
        Case::new("checkpath", &["-d", ""]),
        Case::new("checkpath", &["-U", "-d", "/nonexistent/x"]),
        Case::new("checkpath", &["-U", "-d", "/nonexistent/x"]).env("HOME", "/tmp"),
        Case::new("checkpath", &["-d", "-", "/x"]),
        Case::new("checkpath", &["-d", "--", "-q"]),
        Case::new("checkpath", &["-d", "-", "/x"]).env("POSIXLY_CORRECT", "1"),
    ];
    common::check_all("checkpath-opts", &cases);
}

#[test]
fn filesystem() {
    let me = common::whoami();
    let own = |s: &str| s.replace("ME", &me);
    let p = populated as fn(&Path);
    let mut cases = vec![
        Case::new("checkpath", &["-d", "$ROOT/new"]).sandbox(empty),
        Case::new("checkpath", &["-d", "-m", "0700", "$ROOT/new"]).sandbox(empty),
        Case::new("checkpath", &["-D", "$ROOT/dir"]).sandbox(p),
        Case::new("checkpath", &["-d", "$ROOT//dir//"]).sandbox(p),
        Case::new("checkpath", &["-d", "-m", "0700", "$ROOT/dir"]).sandbox(p),
        Case::new("checkpath", &["-d", "-m", "755", "$ROOT/dir"]).sandbox(p),
        Case::new("checkpath", &["-f", "$ROOT/newfile"]).sandbox(empty),
        Case::new("checkpath", &["-f", "-m", "0600", "$ROOT/newfile"]).sandbox(empty),
        Case::new("checkpath", &["-f", "-m", "0666", "$ROOT/newfile"]).sandbox(empty),
        Case::new("checkpath", &["-f", "$ROOT/file"]).sandbox(p),
        Case::new("checkpath", &["-F", "$ROOT/file"]).sandbox(p),
        Case::new("checkpath", &["-F", "-m", "0644", "$ROOT/file"]).sandbox(p),
        Case::new("checkpath", &["-f", "-m", "", "$ROOT/file"]).sandbox(p),
        Case::new("checkpath", &["-p", "$ROOT/newfifo"]).sandbox(empty),
        Case::new("checkpath", &["-p", "-m", "0644", "$ROOT/fifo"]).sandbox(p),
        Case::new("checkpath", &["-d", "$ROOT/file"]).sandbox(p),
        Case::new("checkpath", &["-f", "$ROOT/dir"]).sandbox(p),
        Case::new("checkpath", &["-f", "$ROOT/fifo"]).sandbox(p),
        Case::new("checkpath", &["-p", "$ROOT/file"]).sandbox(p),
        Case::new("checkpath", &["$ROOT/missing"]).sandbox(p),
        Case::new("checkpath", &["$ROOT/file"]).sandbox(p),
        Case::new("checkpath", &["$ROOT/dir"]).sandbox(p),
        Case::new("checkpath", &["-d", "$ROOT/no/such/parent"]).sandbox(p),
        Case::new("checkpath", &["-d", "$ROOT/file/sub"]).sandbox(p),
        Case::new("checkpath", &["-d", "$ROOT/symdir/sub"]).sandbox(p),
        Case::new("checkpath", &["-f", "$ROOT/symfile"]).sandbox(p),
        Case::new("checkpath", &["-d", "$ROOT/dangling"]).sandbox(p),
        Case::new("checkpath", &["-f", "-m", "0600", "$ROOT/linked"]).sandbox(p),
        Case::new("checkpath", &["-d", "$ROOT/ro/sub"]).sandbox(p),
        Case::new("checkpath", &["-f", "$ROOT/ro/sub"]).sandbox(p),
        Case::new("checkpath", &["-p", "$ROOT/ro/sub"]).sandbox(p),
        Case::new("checkpath", &["-W", "$ROOT/file"]).sandbox(p),
        Case::new("checkpath", &["-W", "$ROOT/ro"]).sandbox(p),
        Case::new("checkpath", &["-W", "$ROOT/missing"]).sandbox(p),
        Case::new("checkpath", &["-W", "-f", "$ROOT/missing"]).sandbox(p),
        Case::new("checkpath", &["-W", "-d", "-m", "0700", "$ROOT/dir"]).sandbox(p),
        Case::new("checkpath", &["-d", "$ROOT/a", "$ROOT/file", "$ROOT/b"]).sandbox(p),
        Case::new("checkpath", &["-d", "$ROOT/a", "relative", "$ROOT/b"]).sandbox(p),
        Case::new("checkpath", &["$ROOT/a", "-d", "$ROOT/b", "-m", "0710"]).sandbox(p),
        Case::new("checkpath", &["-q", "-d", "$ROOT/file"]).sandbox(p),
        Case::new("checkpath", &["-q", "-q", "-d", "$ROOT/file"]).sandbox(p),
        Case::new("checkpath", &["-q", "-d", "$ROOT/quiet-new"]).sandbox(p),
        Case::new("checkpath", &["-C", "-v", "-d", "$ROOT/x"]).sandbox(p),
        Case::new("checkpath", &["-d", "/"]).sandbox(empty),
        Case::new("checkpath", &["-o", "root", "-d", "$ROOT/dir"]).sandbox(p),
        Case::new("checkpath", &["-o", "0:0", "-f", "$ROOT/linked"]).sandbox(p),
        Case::new("checkpath", &["-o", "nobody", "-f", "$ROOT/file"]).sandbox(p),
    ];
    for owner in ["ME", "ME:", ":ME", "ME:ME"] {
        cases.push(Case::new("checkpath", &["-o", &own(owner), "-d", "$ROOT/dir"]).sandbox(p));
    }
    cases.push(Case::new("checkpath", &["-o", &own("ME"), "-f", "$ROOT/newf"]).sandbox(p));
    common::check_all("checkpath-fs", &cases);
}

#[test]
fn on_a_terminal() {
    let p = populated as fn(&Path);
    let cases = vec![
        Case::new("checkpath", &["-d", "$ROOT/new", "$ROOT/file"]).sandbox(p).env("TERM", "xterm").pty(),
        Case::new("checkpath", &["-f", "-m", "0600", "$ROOT/file"]).sandbox(p).env("TERM", "linux").pty(),
    ];
    common::check_all("checkpath-tty", &cases);
}

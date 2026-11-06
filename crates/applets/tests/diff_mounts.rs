//! fstabinfo, mountinfo and do_unmount vs the C reference, run inside a
//! private user + mount namespace with a fixture /etc/fstab and tmpfs
//! mounts, so nothing on the host is read differently or touched.
//!
//! Safety: do_unmount runs `fuser -k` on mounts it fails to unmount, and
//! fuser sees every host process.  So every case here puts fake `mount`,
//! `umount` and `fuser` scripts first in PATH; the fake umount only passes
//! through to the real one for paths inside the sandbox.

mod common;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{Case, Ns};

const FSTAB: &str = r"# fixture fstab
/dev/sda1   /        ext4  defaults,noatime  0 1
/dev/sda2   /home    ext4  defaults          0 2
UUID=abc    /boot    vfat  noauto            0 2
server:/x   /net     nfs   _netdev,ro        0 0
tmpfs       /tmp     tmpfs size=1g           0 0
none        /proc    proc  defaults          0 0
/dev/sdb1   /mnt/with\040space  xfs defaults 0 3
	/dev/sdc1   /dup     ext4 defaults 0 2
/dev/sdc2   /dup     ext3 ro 0 2
swapfile    none     swap sw 0 0
shortline /short
$ROOT/m2    $ROOT/m2 tmpfs _netdev 0 0
$ROOT/m1    $ROOT/m1 tmpfs defaults 0 0
";

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Fake mount/umount/fuser in $ROOT/bin, logging to $ROOT/*.log.
fn tools(r: &Path) {
    let bin = r.join("bin");
    fs::create_dir(&bin).unwrap();
    let root = r.display();
    script(
        &bin.join("mount"),
        &format!("echo \"mount $*\" >> {root}/mount.log; echo \"fake mount: $*\"; exit ${{FAKE_MOUNT_EXIT:-0}}"),
    );
    script(
        &bin.join("umount"),
        &format!(
            "echo \"umount $*\" >> {root}/umount.log\n\
             for last; do :; done\n\
             case \"$last\" in *stuck*) exit 1 ;; {root}/*) exec /usr/bin/umount \"$@\" ;; esac\n\
             exit 32"
        ),
    );
    script(&bin.join("fuser"), &format!("echo \"fuser $*\" >> {root}/fuser.log; printf '%s' \"$FAKE_FUSER_OUT\""));
}

fn with_env(c: Case) -> Case {
    c.env("PATH", "$ROOT/bin:/usr/bin:/bin")
}

fn fstab_ns() -> Ns {
    Ns { fstab: Some(FSTAB.into()), tmpfs: vec![] }
}

fn mounts_ns() -> Ns {
    Ns {
        fstab: Some(FSTAB.into()),
        tmpfs: vec![
            "$ROOT/m1".into(),
            "$ROOT/m1/sub".into(),
            "$ROOT/m1/sub/deep".into(),
            "$ROOT/m2".into(),
            "$ROOT/stuck".into(),
            "$ROOT/sp ace".into(),
        ],
    }
}

fn fi(args: &[&str]) -> Case {
    with_env(Case::new("fstabinfo", args).sandbox(tools).ns(fstab_ns()))
}

#[test]
fn fstabinfo() {
    let mut cases = vec![
        fi(&[]),
        fi(&["-h"]),
        fi(&["-b", "/home"]),
        fi(&["--blockdevice", "/home", "/boot"]),
        fi(&["-o", "/net"]),
        fi(&["-m", "/boot"]),
        fi(&["-m"]),
        fi(&["-p", "/home"]),
        fi(&["-p", "/missing"]),
        fi(&["-p", "=2"]),
        fi(&["-p", "=0"]),
        fi(&["-p", "<2"]),
        fi(&["-p", ">1"]),
        fi(&["-p", "=x"]),
        fi(&["-p", ""]),
        fi(&["-t", "ext4"]),
        fi(&["-t", "ext4,nfs"]),
        fi(&["-t", "ext4,,xfs"]),
        fi(&["-t", "nosuch"]),
        fi(&["-t", "ext4", "/home", "/boot"]),
        fi(&["-t", "ext4", "/net"]),
        fi(&["-p", "=2", "-b"]),
        fi(&["/missing"]),
        fi(&["/home", "/missing", "/net"]),
        fi(&["/mnt/with space"]),
        fi(&["/dup"]),
        fi(&["-b", "/dup"]),
        fi(&["/short"]),
        fi(&["-q", "/home"]),
        fi(&["-q", "/missing"]),
        fi(&["/home"]).env("EINFO_QUIET", "yes"),
        fi(&["-M", "/tmp"]),
        fi(&["-M", "/home", "/net"]).env("FAKE_MOUNT_EXIT", "3"),
        fi(&["-R", "/home"]),
        fi(&["-R", "-q", "/boot", "/missing"]).env("FAKE_MOUNT_EXIT", "1"),
        fi(&["-M", "/home"]).env("PATH", "/nonexistent"),
    ];
    let empty = Ns { fstab: Some("# nothing\n\n".into()), tmpfs: vec![] };
    cases.push(with_env(Case::new("fstabinfo", &[]).sandbox(tools).ns(empty.clone())));
    cases.push(with_env(Case::new("fstabinfo", &["/home"]).sandbox(tools).ns(empty)));
    common::check_all("fstabinfo", &cases);
}

fn mi(args: &[&str]) -> Case {
    with_env(Case::new("mountinfo", args).sandbox(tools).ns(mounts_ns()))
}

#[test]
fn mountinfo() {
    let cases = vec![
        mi(&["-p", "^$ROOT"]),
        mi(&["-p", "^$ROOT", "-s"]),
        mi(&["-p", "^$ROOT", "-t"]),
        mi(&["-p", "^$ROOT", "-i"]),
        mi(&["-t", "-p", "^tmpfs$"]),
        mi(&["-f", "^tmpfs$", "-p", "^$ROOT"]),
        mi(&["-F", "^tmpfs$", "-p", "^$ROOT"]),
        mi(&["-n", "^tmpfs$", "-p", "^$ROOT/m"]),
        mi(&["-N", "^tmpfs$", "-p", "^$ROOT"]),
        mi(&["-o", "size=1024k", "-p", "^$ROOT"]),
        mi(&["-O", "rw", "-p", "^$ROOT"]),
        mi(&["-P", "sub", "-p", "^$ROOT"]),
        mi(&["-e", "-p", "^$ROOT"]),
        mi(&["-E", "-p", "^$ROOT"]),
        mi(&["-e", "$ROOT/m2"]),
        mi(&["-e", "$ROOT/m1"]),
        mi(&["-E", "$ROOT/m1"]),
        mi(&["$ROOT/m1"]),
        mi(&["$ROOT/m1", "$ROOT/m2"]),
        mi(&["$ROOT/m1/sub/../../m2"]),
        mi(&["$ROOT/notmounted"]),
        mi(&["$ROOT/sp ace"]),
        mi(&["-q", "$ROOT/m1"]),
        mi(&["relative"]),
        mi(&["-p", "("]),
        mi(&["-f", "[z-a]"]),
        mi(&["-p", "^$ROOT", "-p", "m1"]),
        mi(&["-x"]),
    ];
    common::check_all("mountinfo", &cases);
}

fn du(args: &[&str]) -> Case {
    with_env(Case::new("do_unmount", args).sandbox(tools).ns(mounts_ns()))
}

/// Unmounts are only compared where they happen one at a time (nested
/// chains, or single mounts): parallel ones can finish in either order.
#[test]
fn do_unmount() {
    let cases = vec![
        du(&["--", "-p", "^$ROOT/m1"]),
        du(&["-l", "--", "-p", "^$ROOT/m1/sub"]),
        du(&["-f", "-l", "--", "$ROOT/m2"]),
        du(&["--", "$ROOT/notmounted"]),
        du(&["--", "-p", "^$ROOT/m2", "-q"]),
        du(&["--", "-p", "^$ROOT/sp"]),
        du(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16", "17", "--"]),
        // Fails to unmount: retries, then fuser (fake) decides.
        du(&["--", "$ROOT/stuck"]),
        du(&["--", "$ROOT/stuck"]).env("FAKE_FUSER_OUT", " 99999"),
        du(&["--", "$ROOT/stuck"]).env("FAKE_FUSER_OUT", " 12345").env("RC_PID", "12345"),
        du(&["--", "$ROOT/stuck"]).env("RC_UNAME", "FreeBSD"),
    ];
    common::check_all("do_unmount", &cases);
}

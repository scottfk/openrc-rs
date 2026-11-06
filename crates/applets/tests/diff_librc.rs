//! librc.so.1 vs the C reference, tested through C OpenRC's own programs.
//!
//! Each case runs a reference C program (rc-status, rc-update,
//! rc-service, rc-depend) twice: on the C librc, and on ours via
//! LD_LIBRARY_PATH.  Both runs happen in a private mount namespace with a
//! fixture system bind-mounted over /etc/init.d, /etc/conf.d,
//! /etc/runlevels and /run/openrc, and the resulting fixture tree is
//! compared too.
//!
//! Needs `make capi` (skips without it, or fails with OPENRC_REF_REQUIRED).

mod common;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use common::Case;
use common::system::{script, system, system_ns};

fn prog(applet: &'static str, args: &[&str]) -> Case {
    Case::on_our_libs(applet, args).sandbox(system).ns(system_ns()).env("TERM", "dumb")
}

#[test]
fn rc_status() {
    if !common::our_libs_built() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &[][..],
        &["-a"],
        &["-l"],
        &["-r"],
        &["-s"],
        &["-u"],
        &["-m"],
        &["-c"],
        &["-C"],
        &["-q"],
        &["-f", "ini"],
        &["boot"],
        &["default"],
        &["loops"],
        &["nosuch"],
        &["-a", "-f", "ini"],
    ] {
        cases.push(prog("rc-status", args));
    }
    cases.push(prog("rc-status", &["-a"]).env("TERM", "xterm").pty());
    common::check_all("librc-rc-status", &cases);
}

#[test]
fn rc_update() {
    if !common::our_libs_built() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &[][..],
        &["show"],
        &["show", "-v"],
        &["show", "default"],
        &["show", "-v", "boot", "single"],
        &["add", "delta", "default"],
        &["add", "delta"],
        &["add", "alpha", "default"],
        &["add", "nosuch", "default"],
        &["add", "delta", "nolevel"],
        &["del", "alpha", "default"],
        &["del", "alpha"],
        &["del", "alpha", "boot"],
        &["del", "nosuch"],
        &["-s", "add", "boot", "default"],
        &["-s", "add", "nolevel", "default"],
        &["-s", "del", "boot", "default"],
        &["-u"],
        &["bogus"],
    ] {
        cases.push(prog("rc-update", args));
    }
    common::check_all("librc-rc-update", &cases);
}

#[test]
fn rc_service() {
    if !common::our_libs_built() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &[][..],
        &["-l"],
        &["-e", "alpha"],
        &["-e", "nosuch"],
        &["-e", "notexec"],
        &["-e", "functions.sh"],
        &["-r", "alpha"],
        &["-r", "net.eth0"],
        &["-r", "nosuch"],
        &["-r", "/etc/init.d/beta"],
        &["-i", "nosuch", "start"],
        &["nosuch", "start"],
        &["-Z", "alpha", "status"],
    ] {
        cases.push(prog("rc-service", args));
    }
    common::check_all("librc-rc-service", &cases);
}

#[test]
fn rc_depend() {
    if !common::our_libs_built() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &[][..],
        &["alpha"],
        &["-t", "ineed", "alpha"],
        &["-t", "iuse", "alpha"],
        &["-t", "needsme", "beta"],
        &["--strict", "alpha"],
        &["-T", "default"],
        &["epsilon"],
        &["loop1"],
        &["broken"],
        &["net"],
        &["-u"],
    ] {
        cases.push(prog("rc-depend", args));
    }
    common::check_all("librc-rc-depend", &cases);
}

/// User mode goes through Rust's rc_set_user/rc_svcdir/rc_runleveldir, so
/// exercise it via the C programs' -U option.
fn user_system(r: &Path) {
    system(r);
    let conf = r.join("xdg/rc");
    fs::create_dir_all(conf.join("init.d")).unwrap();
    fs::create_dir_all(conf.join("runlevels/default")).unwrap();
    script(&conf.join("init.d/usvc"), "description=\"A user service\"");
    symlink(conf.join("init.d/usvc"), conf.join("runlevels/default/usvc")).unwrap();
    fs::create_dir_all(r.join("xdgrun")).unwrap();
}

#[test]
fn user_mode() {
    if !common::our_libs_built() {
        return;
    }
    let u = |applet: &'static str, args: &[&str]| {
        Case::on_our_libs(applet, args)
            .sandbox(user_system)
            .ns(system_ns())
            .env("XDG_CONFIG_HOME", "$ROOT/xdg")
            .env("XDG_RUNTIME_DIR", "$ROOT/xdgrun")
            .env("TERM", "dumb")
    };
    let cases = vec![
        u("rc-status", &["-U"]),
        u("rc-status", &["-U", "-a"]),
        u("rc-status", &["-U", "-l"]),
        u("rc-update", &["-U", "show", "-v"]),
        u("rc-update", &["-U", "add", "usvc", "default"]),
        u("rc-service", &["-U", "-l"]),
        u("rc-service", &["-U", "-r", "usvc"]),
    ];
    // Without the XDG variables: the error paths.  (Not XDG_RUNTIME_DIR="":
    // that makes the svcdir /openrc, which root would really create.)
    let bare = |args: &[&str]| Case::on_our_libs("rc-status", args).sandbox(user_system).ns(system_ns());
    let mut cases = cases;
    cases.push(bare(&["-U"]));
    cases.push(bare(&["-U"]).env("HOME", "$ROOT/home"));
    cases.push(bare(&["-U"]).env("XDG_CONFIG_HOME", "$ROOT/xdg"));
    common::check_all("librc-user", &cases);
}

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
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use common::{Case, Ns};

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/sbin/openrc-run\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A small system: services with assorted dependencies, runlevels, conf.d,
/// and a service state directory with some state already in it.
pub fn system(r: &Path) {
    let initd = r.join("init.d");
    fs::create_dir_all(&initd).unwrap();
    script(
        &initd.join("alpha"),
        "description=\"The alpha service\"\ndepend() {\n\tneed beta\n\tuse gamma\n\tafter delta\n}",
    );
    script(
        &initd.join("beta"),
        "description=\"Beta\"\nextra_commands=\"reload check\"\ndepend() {\n\tprovide virt\n\tbefore gamma\n}",
    );
    script(&initd.join("gamma"), "depend() {\n\tneed virt\n\tkeyword -prefix -timeout\n}");
    script(&initd.join("delta"), "depend() {\n\tuse logger\n\twant epsilon\n}");
    script(&initd.join("epsilon"), "depend() {\n\tneed alpha\n}");
    script(&initd.join("net.lo"), "depend() {\n\tprovide net\n}");
    symlink("net.lo", initd.join("net.eth0")).unwrap();
    script(&initd.join("loop1"), "depend() {\n\tneed loop2\n}");
    script(&initd.join("loop2"), "depend() {\n\tneed loop1\n}");
    script(&initd.join("broken"), "depend() {\n\tneed nosuchservice\n}");
    fs::write(initd.join("functions.sh"), "# not a service\n").unwrap();
    fs::write(initd.join("notexec"), "#!/sbin/openrc-run\n").unwrap();

    let confd = r.join("conf.d");
    fs::create_dir_all(&confd).unwrap();
    fs::write(confd.join("delta"), "rc_need=\"beta\"\n").unwrap();

    let rl = r.join("runlevels");
    for (level, svcs) in [
        ("boot", &["beta", "net.lo"][..]),
        ("default", &["alpha", "gamma", "net.eth0"]),
        ("nonetwork", &["delta"]),
        ("shutdown", &[]),
        ("sysinit", &[]),
        ("single", &["epsilon"]),
        ("loops", &["loop1", "broken"]),
    ] {
        fs::create_dir_all(rl.join(level)).unwrap();
        for s in svcs {
            symlink(format!("/etc/init.d/{s}"), rl.join(level).join(s)).unwrap();
        }
    }
    // A dangling runlevel entry.
    symlink("/etc/init.d/gone", rl.join("default/gone")).unwrap();

    let run = r.join("run");
    for d in ["started", "starting", "inactive", "failed", "options/alpha", "daemons", "scheduled", "exclusive"] {
        fs::create_dir_all(run.join(d)).unwrap();
    }
    fs::write(run.join("softlevel"), "default").unwrap();
    symlink("/etc/init.d/beta", run.join("started/beta")).unwrap();
    symlink("/etc/init.d/net.lo", run.join("started/net.lo")).unwrap();
    symlink("/etc/init.d/alpha", run.join("starting/alpha")).unwrap();
    symlink("/etc/init.d/gamma", run.join("failed/gamma")).unwrap();
    symlink("/etc/init.d/delta", run.join("inactive/delta")).unwrap();
    fs::write(run.join("options/alpha/mode"), "fast").unwrap();
}

fn system_ns() -> Ns {
    Ns {
        binds: vec![
            ("$ROOT/init.d".into(), "/etc/init.d".into()),
            ("$ROOT/conf.d".into(), "/etc/conf.d".into()),
            ("$ROOT/runlevels".into(), "/etc/runlevels".into()),
            ("$ROOT/run".into(), "/run/openrc".into()),
        ],
        ..Default::default()
    }
}

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
        u("rc-status", &["-U"]).env("XDG_RUNTIME_DIR", "").env("XDG_CONFIG_HOME", ""),
    ];
    common::check_all("librc-user", &cases);
}

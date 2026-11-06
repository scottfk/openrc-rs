//! The fixture "system" for librc tests: init scripts with assorted
//! dependencies, runlevels, conf.d and a service state directory, bind-
//! mounted over the real paths inside a private mount namespace.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use super::Ns;

pub fn script(path: &Path, body: &str) {
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

pub fn system_ns() -> Ns {
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

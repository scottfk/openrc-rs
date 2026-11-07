//! The dependency tree (Phase 2 step 4): librc.so.1 vs the C reference.
//!
//! The fixture is a small system with dependencies of every kind:
//! virtual services with several providers, `before *`/`after *`,
//! contradicting before/after, `!` removals, duplicates, missing needs, a
//! real service named like a virtual one, container keywords, conf.d
//! `rc_*` variables, and scripts gendepends.sh must skip.  Both libraries
//! run the host's own gendepends.sh, so the cases test parsing and
//! resolving it, the cache file written (printed byte for byte), loading,
//! the staleness check, and `depends`/`order` under different service
//! states.

mod common;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use common::system::script;
use common::{Case, Ns};

const RC_CONF: &str = "rc_logger=\"NO\"\n";

fn dep_system_with(r: &Path, rc_conf: &str) {
    let initd = r.join("init.d");
    fs::create_dir_all(&initd).unwrap();
    let dep = |name: &str, body: &str| script(&initd.join(name), &format!("depend() {{\n{body}\n}}"));
    // sysinit
    dep("devfs", "\tprovide dev\n\tkeyword -lxc -prefix");
    dep("sysfs", "\tbefore devfs\n\tkeyword -docker");
    // boot
    dep("fsck", "\tuse dev clock\n\tbefore localmount");
    dep("localmount", "\tneed fsck\n\tafter clock lvm\n\tkeyword -timeout");
    dep("clock", "\tprovide clock-sync\n\tbefore *\n\tkeyword -lxc nolxc");
    dep("hostname", "\tafter clock clock");
    dep("syslog-a", "\tprovide logger\n\tneed localmount");
    dep("net.lo", "\tprovide net\n\tafter hostname");
    symlink("net.lo", initd.join("net.eth0")).unwrap();
    // default
    dep("syslog-b", "\tprovide logger\n\tuse net");
    dep("dns", "\tprovide resolver\n\tneed net\n\tbefore app");
    dep("app", "\tneed net logger\n\tuse resolver dns\n\tafter *");
    dep("web", "\tneed app\n\twant cache\n\tuse logger\n\tafter local");
    dep("cache", "\tafter web\n\tbefore web");
    dep("local", "\tafter *");
    dep("early", "\tbefore *\n\tafter clock");
    dep(
        "mailer",
        "\tuse logger\n\tneed net\n\tkeyword -prefix\n\tconfig /run/openrc/cfg=1 /run/openrc/cfgplain /etc/nonexistent.conf",
    );
    dep("ntp-client", "\tprovide ntp\n\tuse net");
    dep("ntp", "\tneed net");
    dep("selfdep", "\tneed selfdep dup dup x.sh .sh\n\tneed !dup\n\tuse   logger\tnet  ");
    dep("dup", "\t:");
    dep("missing", "\tneed nosuchsvc\n\tuse alsonosuch\n\twant nosuchwant");
    dep("loopa", "\tneed loopb");
    dep("loopb", "\tneed loopa");
    // single, shutdown
    dep("rescue", "\tneed localmount");
    dep("savecache", "\tbefore killprocs\n\tafter app");
    dep("killprocs", "\tafter *");
    // In no runlevel.
    dep("hot", "\tprovide net");
    dep("extra-logger", "\tprovide logger");
    script(&initd.join("nodeps"), "");
    script(&initd.join("badsyntax"), "depend() {\n\tneed net\n");
    fs::write(initd.join("noscript"), "#!/bin/sh\necho not an openrc script\n").unwrap();
    fs::set_permissions(initd.join("noscript"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    fs::write(initd.join("notexec"), "#!/sbin/openrc-run\ndepend() {\n\tneed net\n}\n").unwrap();

    let confd = r.join("conf.d");
    fs::create_dir_all(&confd).unwrap();
    fs::write(confd.join("web"), "rc_need=\"extra-logger\"\nrc_before=\"mailer\"\n").unwrap();
    fs::write(confd.join("net"), "rc_after=\"sysfs\"\n").unwrap();
    fs::write(confd.join("net.eth0"), "rc_use=\"dns\"\n").unwrap();

    let rl = r.join("runlevels");
    for (level, svcs) in [
        ("sysinit", &["devfs", "sysfs"][..]),
        ("boot", &["fsck", "localmount", "clock", "hostname", "syslog-a", "net.lo"]),
        (
            "default",
            &[
                "syslog-b",
                "dns",
                "app",
                "web",
                "cache",
                "local",
                "early",
                "mailer",
                "ntp-client",
                "ntp",
                "selfdep",
                "missing",
                "net.eth0",
            ],
        ),
        ("single", &["rescue"]),
        ("shutdown", &["savecache", "killprocs"]),
        ("loops", &["loopa", "nodeps"]),
    ] {
        fs::create_dir_all(rl.join(level)).unwrap();
        for s in svcs {
            symlink(format!("/etc/init.d/{s}"), rl.join(level).join(s)).unwrap();
        }
    }

    let run = r.join("run");
    for d in ["started", "starting", "inactive", "hotplugged", "failed", "stopping", "options", "daemons", "scheduled"]
    {
        fs::create_dir_all(run.join(d)).unwrap();
    }
    fs::write(run.join("softlevel"), "default").unwrap();
    for (st, s) in [
        ("started", "devfs"),
        ("started", "sysfs"),
        ("started", "syslog-a"),
        ("started", "net.lo"),
        ("started", "localmount"),
        ("started", "fsck"),
        ("starting", "app"),
        ("inactive", "dns"),
        ("hotplugged", "hot"),
        ("failed", "web"),
    ] {
        symlink(format!("/etc/init.d/{s}"), run.join(st).join(s)).unwrap();
    }
    fs::write(run.join("cfg=1"), "").unwrap();
    fs::write(run.join("cfgplain"), "").unwrap();
    // A hand-made cache for the loader: malformed lines, a type split in
    // two, an entry with no service, quoting edge cases, no final newline.
    fs::write(
        run.join("handmade"),
        "depinfo_0_service='a'\n\
         depinfo_0_ineed_0='b'\n\
         depinfo_0_iuse_0='c'\n\
         depinfo_0_ineed_1='d'\n\
         depinfo_x_service='bad'\n\
         depinfo_1_service=''\n\
         depinfo_1_ineed_0='orphan'\n\
         depinfo_2_service='b\n\
         depinfo_2_iafter_z='bad'\n\
         depinfo_2_iafter_='bad'\n\
         depinfo_2_iafter_3='\n\
         depinfo_2_iafter_4=noquote\n\
         depinfo_2_iafter_5=''\n\
         \x20depinfo_3_service='leading space'\n\
         depinfo_3_service='has space'\n\
         garbage\n\
         \n\
         depinfo_4_ineed_0='x'\n\
         depinfo_-1_service='neg'\n\
         depinfo_+7_ineed_08='plus'\n\
         depinfo_5_service='q'''\n\
         depinfo_6_service=last\n\
         depinfo_6_iuse_0='nonl'",
    )
    .unwrap();
    fs::write(r.join("rc.conf"), rc_conf).unwrap();
}

fn dep_system(r: &Path) {
    dep_system_with(r, RC_CONF);
}

/// rc_sys="LXC": services with -lxc / nolxc keywords are dropped.
fn dep_system_lxc(r: &Path) {
    dep_system_with(r, "rc_sys=\"LXC\"\n");
}

fn dep_ns() -> Ns {
    Ns {
        binds: vec![
            ("$ROOT/init.d".into(), "/etc/init.d".into()),
            ("$ROOT/conf.d".into(), "/etc/conf.d".into()),
            ("$ROOT/runlevels".into(), "/etc/runlevels".into()),
            ("$ROOT/run".into(), "/run/openrc".into()),
            ("$ROOT/rc.conf".into(), "/etc/rc.conf".into()),
        ],
        ..Default::default()
    }
}

const ALL: &str = "devfs,sysfs,fsck,localmount,clock,hostname,syslog-a,net.lo,net.eth0,syslog-b,dns,app,web,cache,\
                   local,early,mailer,ntp-client,ntp,selfdep,dup,missing,loopa,loopb,rescue,savecache,killprocs,hot,\
                   extra-logger,nodeps,badsyntax,noscript,notexec,dev,logger,net,resolver,clock-sync,nosuchsvc";

fn probe_case(p: &std::path::Path, setup: fn(&Path), args: &[String]) -> Case {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut c = Case::programs("rc-probe", p.to_path_buf(), p.to_path_buf(), &args).sandbox(setup).ns(dep_ns());
    c.ourlibs = true;
    c
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Generating the cache: the files written, the messages, and what loads.
#[test]
fn deptree_update() {
    let Some(p) = common::rc_probe() else { return };
    let generate = strs(&[
        "update",
        "--",
        "cat",
        "/run/openrc/deptree",
        "--",
        "cat",
        "/run/openrc/depconfig",
        "--",
        "load",
        "--",
        "dump",
        ALL,
    ]);
    let mut cases = vec![
        probe_case(&p, dep_system, &generate),
        probe_case(&p, dep_system_lxc, &generate),
        // Update's phase 5 leaves the calling service out of its walk.
        probe_case(&p, dep_system, &generate).env("RC_SVCNAME", "cache"),
        probe_case(&p, dep_system, &generate).env("RC_SVCNAME", "dns"),
        probe_case(&p, dep_system, &generate).env("RC_LIBEXECDIR", "/usr/libexec/rc"),
        // A stale depconfig is removed when no script declares config.
        probe_case(&p, common::system::system, &strs(&["update", "--", "cat", "/run/openrc/depconfig"]))
            .ns(common::system::system_ns()),
    ];
    // The fixture "system" of the other librc tests too.
    cases.push(probe_case(&p, common::system::system, &generate[..9]).ns(common::system::system_ns()));
    common::check_all("deptree-update", &cases);
}

/// Loading caches: hand-made, missing, and queries on a NULL tree.
#[test]
fn deptree_load() {
    let Some(p) = common::rc_probe() else { return };
    let cases = vec![
        probe_case(
            &p,
            dep_system,
            &strs(&[
                "load_file",
                "/run/openrc/handmade",
                "--",
                "dump",
                "a,b,b',has space,leading space,neg,q,q',q'',last,bad,x,,orphan",
                "--",
                "depend",
                "a",
                "ineed",
                "--",
                "depend",
                "a",
                "nosuchtype",
                "--",
                "depend",
                "nosuch",
                "ineed",
                "--",
                "depends",
                "ineed,iuse",
                "a,nosuch,last",
                "default",
                "0",
            ]),
        ),
        probe_case(
            &p,
            dep_system,
            &strs(&[
                "load",
                "--",
                "load_file",
                "/run/openrc/nosuch",
                "--",
                "load_file",
                "/run/openrc",
                "--",
                "depend",
                "a",
                "ineed",
                "--",
                "depends",
                "ineed",
                "a",
                "default",
                "1",
                "--",
                "order",
                "default",
                "0",
            ]),
        ),
    ];
    common::check_all("deptree-load", &cases);
}

const TYPESETS: [&str; 7] = [
    "ineed,iuse",
    "ineed,iuse,iwant,iafter",
    "needsme,usesme,wantsme",
    "ibefore",
    "iprovide",
    "providedby,iprovide,ineed",
    "-",
];

const SVCS: [&str; 12] = [
    "app",
    "web",
    "net",
    "logger",
    "localmount",
    "selfdep",
    "loopa",
    "killprocs",
    "net.eth0,dns",
    "early,local,clock",
    "nosuch,app",
    "",
];

/// `depends` for each type set and service list, at one runlevel and
/// option set, after generating the cache.
fn depends_args(runlevel: &str, options: &str, prelude: &[&str]) -> Vec<String> {
    let mut a = strs(prelude);
    a.extend(strs(&["update", "--", "load"]));
    for t in TYPESETS {
        for s in SVCS {
            a.extend(strs(&["--", "depends", t, s, runlevel, options]));
        }
    }
    a
}

#[test]
fn deptree_depends() {
    let Some(p) = common::rc_probe() else { return };
    let mut cases = Vec::new();
    // TRACE=1 STRICT=2 START=4 STOP=8
    for rl in ["default", "boot", "sysinit", "shutdown", "single", "loops"] {
        for opts in ["0", "1", "3", "5", "9", "7"] {
            cases.push(probe_case(&p, dep_system, &depends_args(rl, opts, &[])));
        }
    }
    // No runlevel (only without tracing: C crashes resolving providers).
    cases.push(probe_case(&p, dep_system, &depends_args("-", "0", &[])));
    cases.push(probe_case(&p, dep_system, &depends_args("default", "5", &[])).env("RC_BOOTLEVEL", "default"));
    cases.push(probe_case(&p, dep_system, &depends_args("default", "1", &[])).env("RC_SVCNAME", "app"));
    cases.push(probe_case(&p, dep_system_lxc, &depends_args("boot", "5", &[])));
    common::check_all("deptree-depends", &cases);
}

/// Service states that change which providers are picked.
const SCENARIOS: [&[&str]; 8] = [
    &[],
    &["mark", "syslog-a", "stopped", "--", "mark", "syslog-b", "started", "--"],
    &["mark", "syslog-a", "stopped", "--", "mark", "extra-logger", "starting", "--"],
    &[
        "mark",
        "syslog-a",
        "stopped",
        "--",
        "mark",
        "syslog-b",
        "inactive",
        "--",
        "mark",
        "extra-logger",
        "inactive",
        "--",
    ],
    &[
        "mark",
        "syslog-a",
        "stopped",
        "--",
        "mark",
        "syslog-b",
        "started",
        "--",
        "mark",
        "extra-logger",
        "started",
        "--",
    ],
    &["mark", "net.lo", "stopped", "--", "mark", "hot", "started", "--"],
    &["mark", "net.lo", "stopped", "--", "mark", "hot", "stopped", "--", "mark", "syslog-a", "stopped", "--"],
    &["mark", "net.lo", "stopped", "--", "mark", "hot", "stopping", "--", "mark", "ntp-client", "started", "--"],
];

#[test]
fn deptree_providers() {
    let Some(p) = common::rc_probe() else { return };
    let mut cases = Vec::new();
    for sc in SCENARIOS {
        for (rl, opts) in [("default", "1"), ("default", "5"), ("boot", "1"), ("default", "9"), ("nonesuch", "1")] {
            let mut a = depends_args(rl, opts, sc);
            for o in ["0", "4", "8"] {
                a.extend(strs(&["--", "order", rl, o]));
            }
            cases.push(probe_case(&p, dep_system, &a));
        }
    }
    common::check_all("deptree-providers", &cases);
}

#[test]
fn deptree_order() {
    let Some(p) = common::rc_probe() else { return };
    let mut a = strs(&["update", "--", "load"]);
    for rl in ["sysinit", "boot", "default", "single", "shutdown", "loops", "nosuch"] {
        for o in ["0", "1", "2", "4", "8", "6"] {
            a.extend(strs(&["--", "order", rl, o]));
        }
    }
    let cases = vec![
        probe_case(&p, dep_system, &a),
        probe_case(&p, dep_system, &a).env("RC_BOOTLEVEL", "default"),
        probe_case(&p, dep_system, &a).env("RC_BOOTLEVEL", "nosuch"),
        probe_case(&p, dep_system, &a).env("RC_SVCNAME", "localmount"),
        probe_case(&p, dep_system_lxc, &a),
        probe_case(&p, common::system::system, &a).ns(common::system::system_ns()),
    ];
    common::check_all("deptree-order", &cases);
}

/// `rc_deptree_update_needed()`: mtimes are set relative to now, so both
/// runs see the same ordering.
#[test]
fn deptree_update_needed() {
    let Some(p) = common::rc_probe() else { return };
    let seq = |v: &[&str]| probe_case(&p, dep_system, &strs(v));
    let age_all = [
        "agetree",
        "/etc/init.d",
        "300",
        "--",
        "agetree",
        "/etc/conf.d",
        "300",
        "--",
        "agetree",
        "/etc/rc.conf",
        "300",
        "--",
        "agetree",
        "/run/openrc/cfg=1",
        "300",
        "--",
        "agetree",
        "/run/openrc/cfgplain",
        "300",
        "--",
        "age",
        "/run/openrc/deptree",
        "100",
        "--",
    ];
    let with = |tail: &[&str]| {
        let mut v = vec!["update", "--"];
        v.extend(age_all);
        v.extend(tail);
        seq(&v)
    };
    let cases = vec![
        seq(&["update_needed"]),
        seq(&[
            "update_needed",
            "--",
            "update",
            "--",
            "update_needed",
            "--",
            "age",
            "/run/openrc/deptree",
            "100",
            "--",
            "update_needed",
        ]),
        with(&["update_needed"]),
        with(&["age", "/etc/init.d/web", "0", "--", "update_needed"]),
        with(&["age", "/etc/conf.d/net", "0", "--", "update_needed"]),
        with(&["age", "/etc/conf.d", "0", "--", "update_needed"]),
        with(&["age", "/etc/rc.conf", "0", "--", "update_needed"]),
        with(&["age", "/etc/init.d/web", "50", "--", "age", "/etc/init.d/app", "20", "--", "update_needed"]),
        // QUIRK: depconfig entries are only checked if they contain '='.
        with(&["age", "/run/openrc/cfg=1", "0", "--", "update_needed"]),
        with(&["age", "/run/openrc/cfgplain", "0", "--", "update_needed"]),
        // Future mtimes, and a cache that is missing again.
        with(&["age", "/etc/init.d/dns", "-1000", "--", "update_needed"]),
        seq(&["update", "--", "age", "/run/openrc/deptree", "-50", "--", "update_needed"]),
    ];
    common::check_all("deptree-update-needed", &cases);
}

/// The reference rc-depend on our library, on the deptree fixture.
#[test]
fn rc_depend() {
    if !common::our_libs_built() {
        return;
    }
    let prog =
        |args: &[&str]| Case::on_our_libs("rc-depend", args).sandbox(dep_system).ns(dep_ns()).env("TERM", "dumb");
    let mut cases = Vec::new();
    for args in [
        &[][..],
        &["-u"],
        &["app"],
        &["web", "mailer"],
        &["-t", "ineed,iuse,iafter", "app"],
        &["-t", "needsme", "net"],
        &["-t", "ibefore", "clock"],
        &["-t", "iafter", "local"],
        &["-a", "web"],
        &["-o", "net"],
        &["-o", "logger"],
        &["-s", "app"],
        &["-T", "app"],
        &["-a", "-s", "net.eth0"],
        &["nosuch"],
        &["nosuch", "app"],
        &["-F", "/run/openrc/handmade", "a"],
        &["-F", "/run/openrc/nosuch", "a"],
        &["loopa"],
        &["selfdep", "missing"],
    ] {
        cases.push(prog(args));
    }
    cases.push(prog(&["app"]).env("RC_BOOTLEVEL", "default"));
    cases.push(prog(&["-a", "web"]).env("RC_SVCNAME", "web"));
    common::check_all("deptree-rc-depend", &cases);
}

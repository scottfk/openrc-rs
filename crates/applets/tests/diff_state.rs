//! Runlevels and service state (Phase 2 step 2): librc.so.1 vs the C
//! reference, on the fixture system.
//!
//! - rc-probe (tests/capi/rc-probe.c) calls the state functions directly,
//!   in sequences that build up state.
//! - Real service lifecycles: the reference rc-service starting and
//!   stopping fixture services, which runs the host's C openrc-run (via
//!   the scripts' #!) on the same library.
//! - mark_service_*, which call rc_service_mark().

mod common;
use common::Case;
use common::system::{system, system_ns};

#[test]
fn state_functions() {
    let Some(p) = common::rc_probe() else { return };
    let case = |args: &[&str]| {
        let mut c = Case::programs("rc-probe", p.clone(), p.clone(), args).sandbox(system).ns(system_ns());
        c.ourlibs = true;
        c
    };
    let svcs =
        ["alpha", "beta", "gamma", "delta", "epsilon", "net.lo", "net.eth0", "notexec", "functions.sh", "nosuch"];
    let mut cases = Vec::new();
    for s in svcs {
        cases.push(case(&["state", s, "--", "resolve", s, "--", "exists", s, "--", "in_runlevel", s, "default"]));
        cases.push(case(&["describe", s, "", "--", "describe", s, "reload", "--", "extra", s]));
    }
    for st in [
        "started",
        "starting",
        "stopping",
        "inactive",
        "wasinactive",
        "hotplugged",
        "failed",
        "crashed",
        "stopped",
        "bogus",
    ] {
        cases.push(case(&[
            "mark", "alpha", st, "--", "state", "alpha", "--", "in_state", st, "--", "in_state", "started",
        ]));
        cases.push(case(&[
            "mark",
            "delta",
            st,
            "--",
            "state",
            "delta",
            "--",
            "in_state",
            "wasinactive",
            "--",
            "in_state",
            "inactive",
        ]));
    }
    cases.extend([
        case(&["mark", "nosuch", "started"]),
        case(&["mark", "/etc/init.d/gamma", "started", "--", "state", "gamma"]),
        case(&["mark", "alpha", "stopped", "--", "state", "alpha"]),
        case(&["resolve", "/abs/path", "--", "resolve", "beta", "--", "resolve", "delta"]),
        case(&["in_state", "scheduled", "--", "in_state", "stopped", "--", "in_state", "bogus"]),
        // Scheduling, then clearing it in various ways.
        case(&[
            "schedule_start",
            "alpha",
            "delta",
            "--",
            "schedule_start",
            "alpha",
            "delta",
            "--",
            "schedule_start",
            "alpha",
            "nosuch",
            "--",
            "scheduled",
            "alpha",
            "--",
            "scheduled_by",
            "delta",
            "--",
            "state",
            "alpha",
            "--",
            "state",
            "delta",
            "--",
            "in_state",
            "scheduled",
            "--",
            "schedule_clear",
            "alpha",
            "--",
            "schedule_clear",
            "alpha",
        ]),
        case(&["schedule_start", "gamma", "epsilon", "--", "mark", "epsilon", "started", "--", "scheduled", "gamma"]),
        case(&["schedule_start", "gamma", "epsilon", "--", "mark", "gamma", "stopped", "--", "scheduled", "gamma"]),
        case(&[
            "putenv", "alpha", "FOO=bar", "--", "putenv", "alpha", "EMPTY=", "--", "putenv", "alpha", "noequals", "--",
            "putenv", "new", "X=1",
        ]),
        // Runlevels.
        case(&[
            "levels",
            "--",
            "level_exists",
            "default",
            "--",
            "level_exists",
            ".",
            "--",
            "level_exists",
            "",
            "--",
            "level_exists",
            "nosuch",
            "--",
            "level_exists",
            "default/../boot",
        ]),
        case(&["get", "--", "set", "single", "--", "get", "--", "set", "", "--", "get", "--", "starting"]),
        case(&[
            "in_runlevel_list",
            "NULL",
            "--",
            "in_runlevel_list",
            "default",
            "--",
            "in_runlevel_list",
            "single",
            "--",
            "in_runlevel_list",
            "nosuch",
        ]),
        case(&[
            "stack", "default", "boot", "--", "stack", "default", "boot", "--", "stack", "default", "nosuch", "--",
            "stacks", "default", "--", "stacked", "default", "--", "stacked", "NULL", "--", "unstack", "default",
            "boot", "--", "unstack", "default", "boot", "--", "stacks", "default",
        ]),
        case(&[
            "stack",
            "default",
            "boot",
            "--",
            "stack",
            "boot",
            "nonetwork",
            "--",
            "stacks",
            "default",
            "--",
            "stacked",
            "default",
        ]),
        // A stacking loop: C (and we) exit(1) with a message.
        case(&["stack", "default", "boot", "--", "stack", "boot", "default", "--", "stacks", "default"]),
        // Adding to and deleting from runlevels.
        case(&[
            "add",
            "default",
            "delta",
            "--",
            "add",
            "default",
            "delta",
            "--",
            "add",
            "nolevel",
            "delta",
            "--",
            // (Not `add default nosuch`: C dereferences NULL there; see QUIRKS.md.)
            "add",
            "boot",
            "alpha",
            "--",
            "add",
            "boot",
            "/etc/init.d/alpha",
            "--",
            "delete",
            "default",
            "alpha",
            "--",
            "delete",
            "default",
            "alpha",
            "--",
            "in_runlevel_list",
            "default",
        ]),
    ]);
    common::check_all("state-probe", &cases);
}

fn svc(args: &[&str]) -> Case {
    Case::on_our_libs("rc-service", args).sandbox(system).ns(system_ns()).env("TERM", "dumb")
}

#[test]
fn service_lifecycle() {
    if common::reference_dir().is_none() || !common::our_libs_built() {
        return;
    }
    let mut cases = Vec::new();
    for args in [
        &["gamma", "start"][..],
        &["alpha", "start"],
        &["alpha", "stop"],
        &["beta", "stop"],
        &["beta", "restart"],
        &["delta", "start"],
        &["epsilon", "start"],
        // Not `loop1 start`: with a dependency loop openrc-run waits out its
        // 60s timeout (in C too); loops are covered by the rc-depend cases.
        &["broken", "start"],
        &["alpha", "zap"],
        &["alpha", "status"],
        &["beta", "status"],
        &["gamma", "status"],
        &["delta", "status"],
        &["beta", "reload"],
        &["beta", "describe"],
        &["alpha", "describe"],
        &["net.eth0", "start"],
        &["net.lo", "stop"],
        &["notexec", "start"],
        &["--ifstarted", "beta", "restart"],
        &["--ifstopped", "epsilon", "start"],
        &["--ifexists", "nosuch", "start"],
    ] {
        cases.push(svc(args));
    }
    common::check_all("state-lifecycle", &cases);
}

#[test]
fn mark_service() {
    if common::reference_dir().is_none() || !common::our_libs_built() {
        return;
    }
    let m = |applet: &'static str, args: &[&str]| Case::on_our_libs(applet, args).sandbox(system).ns(system_ns());
    let mut cases = Vec::new();
    for st in
        ["started", "starting", "stopping", "stopped", "inactive", "wasinactive", "hotplugged", "failed", "crashed"]
    {
        let applet: &'static str = Box::leak(format!("mark_service_{st}").into_boxed_str());
        cases.push(m(applet, &["alpha"]));
        cases.push(m(applet, &["delta"]));
        cases.push(m(applet, &[]).env("RC_SVCNAME", "gamma"));
        cases.push(m(applet, &[]));
        cases.push(m(applet, &["nosuch"]));
    }
    common::check_all("state-mark", &cases);
}

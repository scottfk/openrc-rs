//! openrc-run start (Phase 4 step 2): openrc-run vs the C program.
//!
//! Dependencies are started for real: `/sbin/openrc-run` inside the
//! namespace is the program under test (`$BIN`), so in each run the
//! services `start` spawns are run by the same implementation.  Each
//! script is a fake `openrc-run.sh` in the service directory that prints
//! a line per call and fails or marks itself on request (`START_FAIL`,
//! `START_MARK`); the fixture is in common/run.rs.
//!
//! Plugins: the plugin directory is compiled in (`/usr/lib64/rc/plugins`,
//! which doesn't exist here and can't be created), so both programs are
//! run as copies with that string patched to the same-length
//! `/run/openrc/plugins.d`, inside the sandbox.  The test plugin is built
//! with cc; it prints each hook it gets and sends variables back.
//!
//! Waiting for a service times out after 60 seconds, warning every 10:
//! those cases preload an `alarm()` that counts hundredths of a second.

mod common;
use common::run::*;

#[test]
fn openrc_run_start() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        // Plain starts: net.eth0 and kappa have no deps.  alpha needs the
        // started beta (taking its reexports), starts gamma (failed, so
        // stopped) and is after the inactive delta.
        st(st_plain, "net.eth0", &["start"]),
        st(st_plain, "net.eth0", &["start", "start"]),
        st(st_plain, "net.eth0", &["start", "mycmd"]),
        st(st_plain, "alpha", &["start"]),
        st(st_plain, "gamma", &["start"]),
        st(st_plain, "iota", &["start"]),
        st(st_plain, "kappa", &["start"]).env("IN_BACKGROUND", "yes"),
        st(st_strict_no, "alpha", &["start"]),
        // Already started; inactive without IN_BACKGROUND.
        st(st_plain, "beta", &["start"]),
        st(st_plain, "delta", &["start"]),
        st(st_plain, "beta", &["-q", "start"]),
        // Needs: epsilon needs alpha, which is starting and doesn't start;
        // broken needs a service that doesn't exist.
        st(st_plain, "epsilon", &["start"]),
        st(st_plain, "broken", &["start"]),
        st(st_plain, "broken", &["-Z", "start"]),
        // Scheduling: zeta needs the inactive delta, theta that and the
        // inactive kappa (which provides kprov); eta only uses delta.
        st(st_plain, "zeta", &["start"]),
        st(st_plain, "theta", &["start"]),
        st(st_plain, "eta", &["start"]),
        st(st_plain, "omicron", &["start"]),
        // While the runlevel is starting, the scheduled service is failed.
        st(st_starting, "zeta", &["start"]).env("RC_PID", "1"),
        // Without deps.
        st(st_plain, "epsilon", &["-D", "start"]),
        st(st_plain, "alpha", &["--nodeps", "start"]),
        st(st_plain, "alpha", &["start"]).env("RC_NODEPS", "yes"),
        st(st_starting, "alpha", &["start"]),
        st(st_starting, "alpha", &["start"]).env("RC_PID", "1"),
        // Dry runs: what would be started.
        st(st_plain, "alpha", &["-Z", "start"]),
        st(st_plain, "alpha", &["--dry-run", "start"]),
        st(st_plain, "delta", &["start"]).env("IN_DRYRUN", "yes").env("IN_BACKGROUND", "yes"),
        st(st_plain, "net.eth0", &["-Z", "-D", "start"]),
        st(st_starting, "alpha", &["-Z", "start"]).env("RC_PID", "1"),
        // In the background: only from stopped or inactive; not in sysinit.
        st(st_plain, "delta", &["start"]).env("IN_BACKGROUND", "yes"),
        st(st_plain, "beta", &["start"]).env("IN_BACKGROUND", "true"),
        st(st_plain, "alpha", &["start"]).env("IN_BACKGROUND", "1"),
        st(st_plain, "net.eth0", &["start"]).env("IN_BACKGROUND", "no"),
        st(st_plain, "net.eth0", &["start"]).env("IN_BACKGROUND", "yes").env("RC_RUNLEVEL", "sysinit"),
        st(st_hotplug, "net.eth0", &["start"]).env("IN_HOTPLUG", "yes"),
        st(st_hotplug, "delta", &["start"]).env("IN_HOTPLUG", "yes"),
        // The script fails, or marks itself (and so we don't mark it).
        st(st_plain, "net.eth0", &["start"]).env("START_FAIL", "net.eth0"),
        st(st_plain, "alpha", &["start"]).env("START_FAIL", "gamma"),
        st(st_plain, "iota", &["start"]).env("START_FAIL", "net.eth0"),
        st(st_plain, "net.eth0", &["start", "mycmd"]).env("START_FAIL", "net.eth0"),
        st(st_plain, "net.eth0", &["start"]).env("START_MARK", "net.eth0:inactive"),
        st(st_plain, "net.eth0", &["start"]).env("START_MARK", "net.eth0:stopped"),
        st(st_plain, "net.eth0", &["start"]).env("START_MARK", "net.eth0:failed"),
        st(st_plain, "net.eth0", &["start"]).env("START_MARK", "net.eth0:inactive").env("START_FAIL", "net.eth0"),
        st(st_plain, "iota", &["start"]).env("START_MARK", "net.eth0:inactive"),
        // Scheduled services start once we have.
        st(st_scheduled, "net.eth0", &["start"]),
        st(st_scheduled, "net.eth0", &["start"]).env("START_FAIL", "net.eth0"),
        // rc_parallel: use deps aren't waited for, but then waited on.
        st(st_parallel, "alpha", &["start"]),
        // The lock is taken: a dependency's start runs ours again (with
        // --nodeps that's fine: exit 0).
        st(st_plain, "alpha", &["start"]).env("START_RUN", "gamma:alpha,start"),
        st(st_plain, "alpha", &["start"]).env("START_RUN", "gamma:alpha,--nodeps,start"),
        st(st_plain, "iota", &["start"]).env("START_RUN", "net.eth0:net.eth0,start"),
        st(st_plain, "iota", &["start"])
            .env("START_MARK", "net.eth0:stopping")
            .env("START_RUN", "net.eth0:net.eth0,start"),
        st(st_stopping, "net.eth0", &["start"]),
        // Waiting: mu takes too long (warnings, then a timeout); gamma has
        // no timeout.
        st(st_parallel_slow, "lambda", &["start"]).env("LD_PRELOAD", alarm_shim().to_str().unwrap()),
        st(st_parallel_slow, "nu", &["start"]).env("LD_PRELOAD", alarm_shim().to_str().unwrap()),
        st(st_parallel_slow, "lambda", &["-q", "start"]).env("LD_PRELOAD", alarm_shim().to_str().unwrap()),
        // Lock and deptree trouble.
        st(st_no_exclusive, "net.eth0", &["start"]),
        st(st_no_exclusive, "alpha", &["start"]),
        st(st_no_exclusive, "kappa", &["start"]).env("IN_BACKGROUND", "yes"),
        st(st_no_deptree, "net.eth0", &["start"]),
        st(st_no_deptree, "net.eth0", &["-D", "start"]),
        // With rc's lock.
        st(st_plain, "net.eth0", &["-l", "99", "start"]),
    ];
    common::check_all("openrc-run-start", &cases);
}

#[test]
fn openrc_run_start_plugins() {
    if common::reference_dir().is_none() {
        return;
    }
    let cases = vec![
        st(st_plugins, "net.eth0", &["start"]),
        st(st_plugins, "alpha", &["start"]),
        st(st_plugins, "beta", &["start"]),
        st(st_plugins, "epsilon", &["start"]),
        st(st_plugins, "zeta", &["start"]),
        st(st_plugins, "net.eth0", &["-Z", "start"]),
        st(st_plugins, "net.eth0", &["start"]).env("START_FAIL", "net.eth0"),
        st(st_plugins, "net.eth0", &["start"]).env("START_MARK", "net.eth0:inactive"),
        st(st_plugins, "net.eth0", &["start", "mycmd"]).env("PLUG_MODE", "unset"),
        // A record without '=' (QUIRK: C crashes).
        st(st_plugins, "net.eth0", &["start"]).env("PLUG_MODE", "noeq"),
        // Hooks only run for start (and stop).
        st(st_plugins, "net.eth0", &["mycmd", "zap"]),
        st(st_bad_plugins, "net.eth0", &["start"]),
        st(st_bad_plugins, "net.eth0", &["describe"]),
    ];
    common::check_all("openrc-run-start-plugins", &cases);
}

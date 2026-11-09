# openrc-rs

A drop-in port/reimplementation of openrc in rust.  Currently just something fun for me to tinker with in Claude.

## Goals

- **Drop-in compatible with OpenRC 0.64.1:** same command-line behaviour,
  output, exit codes, on-disk state (`/run/openrc`), init-script interface,
  and C ABI (`libeinfo.so.1`, later `librc.so.1`). Upstream quirks are
  reproduced on purpose and listed in [QUIRKS.md](QUIRKS.md). Improvements
  come later, as deliberate changes.
- **Linux only.**
- **Keep the shell side.** `openrc-run.sh` and the functions init scripts
  use stay as shell, so existing `/etc/init.d` scripts work unchanged. Only
  the C programs and libraries are ported.
- **Small and boring:** a single multi-call binary, `libc` as the only
  runtime dependency, no async, and `unsafe` only in `crates/compat`.

## Layout

| Path | What |
|---|---|
| `crates/compat` | glibc-compatible helpers: `strto*` parsing, `getopt_long`, C-stdio buffering, `strerror`, and thin safe syscall wrappers. All the `unsafe` code lives here. |
| `crates/einfo` | Port of `libeinfo` (einfo/ewarn/ebegin/eend output). |
| `crates/rc` | Port of `librc`. So far: rc.conf parsing, user mode, the script path, `rc_sys()`. |
| `crates/applets` | The `openrc-rs` multi-call binary. It picks the applet from `argv[0]`. |
| `crates/capi-einfo` | C ABI: Rust staticlib plus a C shim for the variadic functions, linked into `libeinfo.so.1` by `make`. |
| `crates/capi-rc` | C ABI: a drop-in `librc.so.1`, all Rust ([details](crates/capi-rc/README.md)). |
| `tests/capi` | C program used to diff our C ABI against the reference library. |
| `tools/build-reference.sh` | Builds upstream C OpenRC at the pinned tag ([tools/REFERENCE_TAG](tools/REFERENCE_TAG)) for differential testing. |

## Building and testing

```sh
make            # release binary + target/capi/libeinfo.so.1
make check      # build the C reference if needed, then run all tests
make install DESTDIR=/tmp/stage   # binary, applet symlinks, libeinfo, einfo.h
```

`make check` builds the C reference from a checkout set by `OPENRC_SRC`, or
by cloning upstream. It then runs the **differential tests**: every case runs
under the C reference and under openrc-rs, with the same `argv[0]`, a clean
environment and the same sandbox path, and must produce identical stdout,
stderr, exit status and resulting filesystem tree. Some cases run on a
pseudo-terminal, to cover colour output. The C ABI is tested the same way, by
linking one C program against each `libeinfo.so.1`.

Plain `cargo test` works too. It skips the differential tests if the
reference hasn't been built.

The host runs unprivileged and never touches syslog (see QUIRKS.md: `esyslog`
logs at LOG_EMERG). [crates/applets/tests/diff_root.rs](crates/applets/tests/diff_root.rs)
covers both, and runs only as root: `checkpath` chown and symlink-ownership
rules, and syslog output captured on a private `/dev/log`. Run it on a test
VM with no syslog daemon.

### Testing on a VM

On a disposable Gentoo VM with stock OpenRC at the reference tag:

```sh
make check                                   # as root: includes diff_root.rs
make install DESTDIR=/root/stage OPENRC_BRANDING="Gentoo Linux"
tools/exercise-openrc.sh > c.txt             # baseline with C OpenRC
tools/swap-install.sh install /root/stage    # swap in openrc-rs (backs up originals)
tools/exercise-openrc.sh > rs.txt; diff c.txt rs.txt
reboot                                       # then diff the serial-console boot logs
tools/swap-install.sh restore
```

The exercise script also uses a supervised service, `sv-test`, if the
test system has it (`/etc/init.d/sv-test`, in the default runlevel):

```sh
#!/sbin/openrc-run
description="openrc-rs supervise-daemon test"
supervisor=supervise-daemon
command=/bin/sleep
command_args=infinity
command_user=nobody
respawn_delay=1
```

Once swapped in, C OpenRC's own `openrc`, `openrc-run`, `rc-status` and so on
load our `libeinfo.so.1`, so the whole boot exercises it.

## Roadmap

1. **Leaf utilities** (in progress)
   - [x] `einfo` family (21 applets), `checkpath`
   - [x] `libeinfo.so.1` C ABI
   - [x] `mountinfo`/`do_unmount`, `fstabinfo`, `is_newer_than`/`is_older_than`, `shell_var`, `kill_all`, `seedrng`, `swclock`, and the `value` applets (`service_get_value` & co)
   - (`on_ac_power` and `rc-sstat` are shell scripts upstream: nothing to port)
2. **rc-core** (`librc`): service state, runlevels, dependency tree and deptree cache, `librc.so.1`. One branch per step:
   - [x] C ABI layer: hybrid `librc.so.1`; C OpenRC's programs as differential tests (`diff_librc.rs`)
   - [x] runlevels and service state
   - [x] daemons (`librc-daemon.c`)
   - [x] dependency tree (`librc-depend.c`): cache generation, loading, staleness, `depends` and `order`
   - [x] string lists and `rc_config_value()`: `librc.so.1` is all Rust, upstream's C deleted
   - [x] VM: boot C OpenRC on the all-Rust `librc.so.1`
3. **Query and admin tools.** One branch per step:
   - [x] `rc-depend`, `mark_service_*`
   - [x] `rc-service` and the `service_*` helpers
   - [x] `rc-update`
   - [x] `rc-status`
4. **`openrc-run`.** One branch per step:
   - [x] groundwork and the simple commands: options, environment, running `openrc-run.sh` (with `rc_parallel` prefixing), `describe`/`help`/`status`/extra commands, dependency queries, `zap`
   - [x] `start`: dependencies, waiting, inactive/background/hotplug, scheduled services, plugins
   - [x] `stop`, `restart`, `condrestart`, `pause` (broken upstream; see QUIRKS.md); installed as `$sbindir/openrc-run`
5. **`openrc`**, the runlevel engine. One branch per step:
   - [x] runlevel changes: options (`-n`, `-o`, `-s`, `-S`), the krunlevel override, stopping what isn't wanted, starting the stacked runlevels, crashed daemons, plugins, signals, `rc_parallel`
   - [x] the `rc.log` logger, `sysinit` (banner, `init.sh`, kernel command line), interactive boot; installed as `$sbindir/openrc`
6. **`start-stop-daemon`, `supervise-daemon`.** One branch per step:
   - [x] `start-stop-daemon`, with `schedules.c`, capabilities (no libcap) and readiness notification
   - [x] `supervise-daemon`: respawning, health checks, the control FIFO, readiness, `--reexec`
7. **`openrc-init`, `openrc-shutdown`.** One branch per step:
   - [x] `openrc-shutdown`, with `broadcast.c`, `sysvinit.c` and wtmp
   - [x] `openrc-init`
   - (`halt`/`poweroff`/`reboot`/`shutdown` are shell wrappers around `openrc-shutdown`, installed with `-Dsysvinit=true`: nothing to port)

Until phase 7 is done and tested in a VM, nothing here should be installed over
a real system's OpenRC.

# Upstream quirks, kept on purpose

openrc-rs reproduces OpenRC **0.64.1** exactly, including behaviour that looks
like a bug. Each item here is marked `QUIRK` in the source. Fixing one is a
deliberate, separate change (ideally upstream as well), not something to slip
into a port.

Severity: **high** means it can cause real harm, **medium** means it's
surprising and user-visible, **low** means it's cosmetic or near-impossible to hit.

## einfo / libeinfo

| Severity | Quirk | Where |
|---|---|---|
| **high** | `esyslog` always logs at `LOG_EMERG` (priority 0), whatever level you pass. `einfo.c` calls `elog(retval, ...)`, and `retval` is still 0 at that point; the parsed level is ignored. Many syslog daemons broadcast emerg messages to every terminal. | `applets/einfo.rs` |
| medium | `EINFO_INDENT` is parsed with `strtoimax(..., 0)`, so the zero-padded values that `eindent()` writes (`%08d`) are read as **octal**: `00000008` reads as 0 and `00000010` as 8. Indentation breaks from the 4th level on. | `einfo/lib.rs` |
| low | `eerrorn` sets `EINFO_LASTCMD=errorn` (missing the leading `e`). | `einfo/lib.rs` |
| low | `ewarnv` writes its message to stderr but the newline to stdout. Unlike `ewarn`, it doesn't log. | `einfo/lib.rs` |
| low | `eendv` / `ewendv` return 0 rather than `retval` when not verbose. | `einfo/lib.rs` |
| low | `ewendv` prints its message as an error (red), not as a warning, because `_do_eend` compares the command name against `"ewend"` only. | `einfo/lib.rs` |
| low | `eend`'s `[ ok ]` colours come from `ecolor()`, which picks whichever of stdout, stderr or stdin is a tty, rather than the stream being written to. | `einfo/lib.rs` |

## checkpath

| Severity | Quirk | Where |
|---|---|---|
| medium | `-D` ("Create/empty directory") never empties anything: truncation is applied to files only. | `applets/checkpath.rs` |
| medium | `-W` returns success as soon as the path is writable, skipping the type, mode and owner checks. | `applets/checkpath.rs` |
| low | A root-owned symlink loop in the parent path makes checkpath spin forever. | `applets/checkpath.rs` |
| low | The default owner is the *effective* uid but the *real* gid. | `applets/checkpath.rs` |
| low | `-m` uses a signed comparison, so modes starting with a byte below `'0'` take the numeric path. `-m ""` is accepted as "don't check", and `-m " 755"` works. | `shared.rs` (`parse_mode`) |
| low | The umask isn't restored when `mkdirat`/`mkfifoat` fails. This is harmless, because later creates set it to 0 again. | `applets/checkpath.rs` |

## Small helpers

| Severity | Quirk | Where |
|---|---|---|
| medium | `is_older_than SRC TGT...` never checks "older": like `is_newer_than` it calls `rc_newer_than`, and succeeds as soon as any TGT contains something newer than SRC. Upstream's comment calls it "perverted", for compatibility with baselayout. | `applets/newer.rs` |
| low | `is_newer_than` passes on equal mtimes, so a file is "newer than" itself. Only whole seconds are compared. | `rc/mtime.rs` |

## kill_all

| Severity | Quirk | Where |
|---|---|---|
| **high** | Even with `--dry-run`, kill_all runs `kill(-1, SIGSTOP)` first, freezing every process it is allowed to signal, and resumes them all with SIGCONT at the end. Run unprivileged, that's the caller's whole desktop session. The tests only run it as PID 1 of a private PID namespace. | `applets/kill_all.rs` |
| medium | PID 2 is always taken to be kthreadd, and so never signalled. In a container or PID namespace, PID 2 is an ordinary process, and so is everything descended from it. | `applets/kill_all.rs` |
| low | `-o` pids are compared as strings exactly as given, so `-o 007` doesn't omit PID 7. | `applets/kill_all.rs` |

## mountinfo / do_unmount / fstabinfo

| Severity | Quirk | Where |
|---|---|---|
| medium | `do_unmount` without a `--` treats *every* argument as an umount option, so mountinfo gets no filters and it tries to unmount everything mounted. | `applets/mountinfo.rs` |
| medium | `mountinfo -e`/`-E` (netdev filters) only judge a mount with no fstab entry by netdev if mount points were given. Otherwise the regex filters are applied instead, so non-fstab mounts are listed. | `applets/mountinfo.rs` |
| low | `-p`/`-P` (point regex) match whatever field is being printed: with `-t`, `-i` or `-s` that's the node, options or fstype, not the mount point. | `applets/mountinfo.rs` |
| low | The fstab lookup compares /proc/mounts' still-escaped mount point (`\040`) with fstab's decoded one, so mount points containing spaces never match fstab. | `applets/mountinfo.rs` |
| low | `do_unmount`'s retry sleep can't be cut short by a child exiting. `tm_sleep` checks `clock_nanosleep` for -1, but it returns the error number, so EINTR always resumes the sleep. The sleep is capped at 500ms. | `compat/sys.rs` (`tm_sleep`) |

## librc: runlevels and service state

| Severity | Quirk | Where |
|---|---|---|
| **high** | `rm_dir()` stats entries following symlinks. A symlink to a directory is recursed into and the *target's* contents are deleted. It's used when a service stops, to clear `options/<svc>` and `daemons/<svc>`, so a symlink planted there makes root empty its target. | `rc/state.rs` (`rm_dir`) |
| medium | `rc_service_mark(STOPPED)` never clears saved environment. It `unlink()`s `environ/<svc>`, which is a directory, so the call always fails. | `rc/state.rs` |
| medium | `rc_service_state()` sets SCHEDULED when `scheduled/<service>` exists, meaning the service has scheduled *others*, not that it is scheduled itself. | `rc/state.rs` |
| low | `rc_service_schedule_clear()` returns its result inverted: false when it removed a schedule, true when there was none (ENOENT). | `rc/state.rs` |
| low | `rc_services_in_state(SCHEDULED)` walks `.` and `..` of `scheduled/`, so it also lists the scheduling services' names and the svcdir's own entries. `rc_services_scheduled_by()` likewise can return `.`. | `rc/state.rs` |
| low | `rc_services_in_runlevel_stacked()` lists the runlevel's own services twice, because `rc_runlevel_stacks()` includes the runlevel itself. | `rc/state.rs` |
| low | `rc_service_mark()` to STARTED or STOPPED also unlinks `scheduled/../<svc>`, i.e. `$svcdir/<svc>`, because it walks `..` too. | `rc/state.rs` |

## librc: daemons (`librc-daemon.c`)

| Severity | Quirk | Where |
|---|---|---|
| medium | `rc_service_started_daemon()` and the record matching label every argument `argv_0=`, while records number them `argv_0`, `argv_1`, .... A daemon matched with two or more arguments never matches. | `rc/daemon.rs` (`match_list`) |
| medium | Matching consumes its list of wanted lines, and the callers reuse that list across a service's records. A record that partly matched makes a later record count as a match with only the remaining lines. | `rc/daemon.rs` (`match_daemon`) |
| low | `rc_find_pids(exec)` compares against /proc's `comm`, which the kernel truncates to 15 bytes, so daemons with longer names are never found by exec. | `rc/daemon.rs` |
| low | On an OpenVZ host, once one container process is seen, `rc_find_pids()` skips every later process too (its flag is never reset). | `rc/daemon.rs` |
| low | `rc_service_daemons_crashed()` gives up on the rest of the service's records if one can't be opened. Whether a crash then counts depends on errno being EACCES afterwards, which can be left over from any failed call, such as another user's /proc entry. | `rc/daemon.rs` |

## librc: dependency tree (`librc-depend.c`)

| Severity | Quirk | Where |
|---|---|---|
| medium | `rc_deptree_update_needed()` reads `depconfig` with `config_list()`, which keeps only lines containing `=`. The files that scripts declare with `config` are plain paths, so changing them never triggers a deptree update (unless the path happens to contain `=`). | `rc/deptree.rs` (`rc_deptree_update_needed`) |
| low | Dependency lists keep duplicates: `need a a` records `a` twice, and a later `need !a` removes only one copy. | `rc/deptree.rs` (`parse_gendepends`) |
| low | On a container system (`rc_sys`), a service with both `-<sys>` and `no<sys>` keywords (or one of them twice) is removed twice. The second pass deletes a second copy of its name, and of what it provides, from every other service's lists. | `rc/deptree.rs` (`remove_nosys`) |
| low | The cache loader starts a new list whenever a type's lines aren't contiguous, and lookups return the first, so `rc_deptree_depend()` misses the later entries. The cache OpenRC writes is always contiguous; this only affects hand-edited files. | `rc/deptree.rs` (`parse`) |

## librc: string lists (`librc-stringlist.c`, `rc_config_value()`)

| Severity | Quirk | Where |
|---|---|---|
| low | `rc_stringlist_delete()` sets errno to EEXIST, not ENOENT, when the value isn't on the list. | `capi-rc/stringlist.rs` |
| low | `rc_config_value(list, entry)` returns the text after the line's *first* `=`, so an `entry` containing `=` gets the wrong part: `"a=b"` on the line `a=b=c` gives `b=c`. | `capi-rc/stringlist.rs` |

## librc (rc.conf parsing)

| Severity | Quirk | Where |
|---|---|---|
| medium | When a key is set in `rc.conf` in more than one directory of the search path, the **lowest**-priority directory wins (e.g. `/etc/rc.conf` beats `/etc/rc/rc.conf`). Upstream's comment intends the opposite. `rc.conf.d/*.conf` entries do replace in place. | `rc/conf.rs` |
| medium | `#` starts a comment anywhere, even inside quotes: `x="a#b"` gives `x="a`. | `rc/conf.rs` |
| low | A value starting with `$` becomes the *key*: `a=$b` gives `a=a`, and `export a=$b` gives `a=export a`. | `rc/conf.rs` |
| low | `rc_sys()` uppercases the cached `rc_sys` value in place, so later `rc_conf_value("rc_sys")` calls see it uppercased. | `rc/sys.rs` |

## rc-service, service_*

| Severity | Quirk | Where |
|---|---|---|
| medium | `rc-service` drops the caller's `PATH`. `env_filter()` runs first and `PATH` isn't on its allow list, so `env_config()` always starts from the bare system prefix. Its code to keep the caller's extra directories only runs when `rc_env_allow` lists `PATH`. | `applets/shared.rs` (`env_filter`, `env_config`) |
| low | `env_config()` checks the system prefix textually: a `PATH` that merely *starts with* `/usr/libexec/rc/bin:/bin:/sbin:/usr/bin:/usr/sbin`, such as `...:/usr/sbinX:/z`, is kept unchanged. | `applets/shared.rs` (`env_config`) |
| low | `rc-service -e`, `-l` and `-r` act as soon as getopt reaches them, so later options are ignored: `rc-service -e nosuch -l` just exits 1. | `applets/rc_service.rs` |

## rc-depend, mark_service_*

| Severity | Quirk | Where |
|---|---|---|
| medium | `rc-depend -T` (`--notrace`) does `options &= RC_DEP_TRACE` instead of `&= ~RC_DEP_TRACE`. Tracing stays on, and any `-a`, `-o` or `-s` given *before* `-T` is cancelled. | `applets/rc_depend.rs` |
| low | `rc-depend` never prints `no dependency info for service`. Its check needs `rc_deptree_depends()` to return NULL, which it never does, so unknown services are passed on silently. | `applets/rc_depend.rs` |
| low | When the deptree can't be stat'ed after regenerating it, the message reads `stat(/run/openrc): <error>/deptree`, with the error in the middle of the path. | `applets/shared.rs` (`rc_deptree_load_updating`) |

## rc-update

| Severity | Quirk | Where |
|---|---|---|
| low | `rc-update show X Y...` checks that `Y...` are runlevels but not `X`: `rc-update show nosuch` prints an (empty) `nosuch` column instead of an error. | `applets/rc_update.rs` |
| low | `rc-update -u` updates the deptree and exits as soon as getopt reaches it, ignoring any command: `rc-update add foo -u` adds nothing. A failed update exits 255 (`return -1`). | `applets/rc_update.rs` |
| low | `rc-update -s add` refuses `boot`, `single` and `shutdown` too, but the message always says `cannot stack the sysinit runlevel`. | `applets/rc_update.rs` |
| low | When the runlevel comes from `softlevel` or the default and doesn't exist, `rc-update add`/`del` print `runlevel ... does not exist` but `add` still exits 0 (`del` exits 1 via its "not found in any of the specified runlevels" warning). | `applets/rc_update.rs` |

## rc-status

| Severity | Quirk | Where |
|---|---|---|
| low | A failed service in the current runlevel is listed again under `Dynamic Runlevel: manual`: the filter that drops runlevel services keeps any that are failed. | `applets/rc_status.rs` |
| low | `-c`, `-l`, `-m`, `-r`, `-s`, `-S` and `-u` act as soon as getopt reaches them, so later options and arguments are ignored: `rc-status -l -c` only lists runlevels, `rc-status -r boot` prints the current runlevel. | `applets/rc_status.rs` |
| low | A runlevel argument that doesn't exist is reported and skipped; rc-status exits 1 only when none of them exist. | `applets/rc_status.rs` |
| low | A start time in the future gives a negative uptime (`-11:00:00`), and one that doesn't parse counts from 1969-12-31 23:59:59 (`20764 day(s) ...`). | `applets/rc_status.rs` (`get_uptime`, `to_time_t`) |

## openrc-run

| Severity | Quirk | Where |
|---|---|---|
| medium | A script killed by a signal counts as success: openrc-run takes `WEXITSTATUS()` of its status, which is 0 for a signal death. | `applets/openrc_run.rs` (`svc_exec`) |
| medium | A service that matches none of the `rc_hotplug` patterns may be hotplugged: only a matching `!pattern` refuses. So `rc_hotplug="gamma"` lets every service in. | `applets/openrc_run.rs` (`service_plugable`) |
| low | With `rc_parallel` on a terminal, output the script leaves unread in the pty when it exits is lost: openrc-run reads at most one buffer per poll round, stops once the exit status arrives, and never closes its copy of the pty slave, so the master never hangs up. | `applets/openrc_run.rs` (`svc_exec`) |
| low | The SIGWINCH handler code is dead: openrc-run never installs it, so window size changes don't reach the pty. | `applets/openrc_run.rs` (`handle_signal`) |
| low | If the signal pipe can't be made, the message names the service twice (`alpha: pipe2: alpha`) instead of giving the error. | `applets/openrc_run.rs` (`svc_exec`) |
| medium | Starting never uses what a service provides for scheduling: both lookups call `rc_deptree_depend(deptree, "iprovide", svc)` with the service and type swapped, so they find nothing. A service waiting on an inactive need is scheduled after that need only, not after what it provides; and once a service has started, services scheduled after what it provides are not started. | `applets/openrc_run.rs` (`svc_start_deps`, `svc_start_real`) |
| low | A plugin record without `=` (say `NAME\0`) crashes openrc-run: `strsep()` leaves its pointer NULL, which C then dereferences (SIGSEGV). | `plugin.rs` (`read_environ`) |
| low | Plugin records are parsed from one buffer that is zeroed once and reused, so a short read can run on into bytes an earlier read left behind. | `plugin.rs` (`read_environ`) |
| low | Dead check: when starting the services we use, failed ones are meant to be skipped "while the runlevel is starting", but that test sits inside a block that only runs when the runlevel *isn't* starting. | `applets/openrc_run.rs` (`svc_start_deps`) |
| medium | An inactive service can't be stopped in the background (`IN_BACKGROUND=yes`): a misplaced parenthesis makes the check "refuse unless started and not inactive" rather than "refuse unless started or inactive", so openrc-run exits 1. | `applets/openrc_run.rs` (`svc_stop_check`) |
| low | `pause` doesn't work. The handler that would warn it's deprecated and stop without dependencies is never reached, because `pause` isn't in the command table. It goes to `openrc-run.sh`, which fails with "command 'pause' not a default nor extra command". | `applets/openrc_run.rs` (`cmd_stop`) |

## openrc

| Severity | Quirk | Where |
|---|---|---|
| medium | `openrc -o <runlevel>` crashes (SIGSEGV) unless `RC_BOOTLEVEL` is in the environment. `set_krunlevel()` compares the level with `getenv("RC_BOOTLEVEL")`, but `-o` is handled before `env_config()` sets it, so from an ordinary shell `strcmp()` gets NULL. | `applets/openrc.rs` (`set_krunlevel`) |
| low | `openrc -o ''` reports "runlevel `(null)' does not exist": the empty argument becomes NULL and is printed with `%s`. | `applets/openrc.rs` (`main`) |
| low | `-a <arg>` is in the option string but has no handler, so it is a usage error. | `applets/openrc.rs` (`main`) |
| low | Dead check: "<runlevel>: is not a valid runlevel" (after the runlevel-stop hook) can't happen, because the same test already failed earlier with "<runlevel>: not a valid runlevel". | `applets/openrc.rs` (`main`) |
| low | The exit cleanup (removing `rc.starting`/`rc.stopping`, emptying `failed/`, unloading plugins) only runs when the program is called `rc` or `openrc`. | `applets/openrc.rs` (`cleanup`) |
| low | The SIGCHLD handler returns without restoring `errno` when `waitpid()` fails (usually ECHILD). | `applets/openrc.rs` (`handle_signal`) |

# Known deviations from C

These are differences openrc-rs introduces. Each one should be invisible in practice.

- **C ABI formatting:** the shim formats printf-style messages even when they
  won't be printed (e.g. under `EINFO_QUIET`). This only matters for `%n`.
  If `vsnprintf` fails, the message becomes `""`.
- **`eprefix()` copies its string.** C stores the pointer, so a caller that
  later changes the buffer would see the change in C but not here.
- **`rc_getfile()` on odd files** (used by `service_get_value`): if a file
  is shorter than `st_size` when read, C loops forever; we return what was
  read. If reading fails (e.g. the option is a directory), C hands back a
  freed buffer (undefined behaviour); we report failure.
- **Impossible inputs** where C crashes or reads uninitialised memory: a
  /proc/mounts line with fewer than four fields (C dereferences NULL; we
  use empty fields), and `waitpid` failing in fstabinfo (C reads an
  uninitialised status; we report -1).
- **`librc.so.1` on C's undefined-behaviour paths:** where C frees a
  caller's buffer and leaves it dangling, we set it to NULL. That's
  `rc_getfile()` when a read fails, and `rc_service_getenv()` when the
  value can't be opened after the buffer was resized (there C leaves the
  old pointer).
- **A NULL runlevel in `rc_deptree_depends()` with `RC_DEP_TRACE`**, or in
  `rc_deptree_order()`: C calls `strcmp()` on it and crashes once it has to
  pick between providers. We treat it as a runlevel named `(null)` (as
  glibc's `%s` formats it in the runlevel path), and `rc_deptree_order()`
  returns an empty list. C's own callers always pass a runlevel.
- **`rc_service_add()` for a service that can't be resolved**, in a
  runlevel other than `boot`: C dereferences NULL and crashes. We fail with
  ENOENT. C's own callers check `rc_service_exists()` first.
- **`service_started_daemon EXEC [INDEX]` with `RC_SVCNAME` unset:** C
  passes a NULL service to `basename_c()` and crashes; we exit 1 ("not
  started").
- **`rc-update -u` when the deptree can't be opened for writing (EACCES):**
  C returns an uninitialised variable (0 in practice, on the builds tried);
  we return 0.
- **`rc-status` without a deptree** (none cached and none can be written):
  C passes the NULL deptree to `rc_deptree_depends()` while sorting out
  needed/wanted services and crashes if there are any candidates. We treat
  it as empty, so they're all listed as manual.
- **openrc-run reading the pty:** if a read of the pty master fails, C
  passes the -1 on as a length and walks off the end of its buffer; we
  treat it as nothing read.
- **openrc-run relaying the pty:** C writes the script's output to the
  terminal byte by byte and doesn't retry a write that the script's SIGCHLD
  interrupts (the handler has no SA_RESTART), so now and then a byte is
  lost. We retry.
- **openrc-run's child pid:** C's SIGCHLD handler could, in principle, reap
  the script before `posix_spawn()` has stored its pid, losing its exit
  status. We record the pid with signals still blocked.
- **Plugins and `rc_environ_fd`:** C sets openrc-run's own `rc_environ_fd`
  (in librc), which a plugin binds to when it is loaded. openrc-rs doesn't
  link librc, so it finds the variable through the plugin's own handle:
  a plugin has to link librc.so.1 (`-lrc`) for its variables to come back.
  One that relies on the program's librc without linking it fails to load
  ("undefined symbol").
- **Plugin records with no NUL in the buffer:** C's `strsep()`/`strlen()`
  run past the end of its 8 KiB buffer; we stop at its end.
- **`rc_config_value()` on a NULL list:** C dereferences NULL and crashes;
  we return NULL. (A NULL *value* inside a list still crashes in
  `strcmp()`, as in C.)
- **SELinux:** `checkpath` doesn't relabel yet (`TODO(selinux)`). Upstream
  only does this when built with `-Dselinux`.
- **stdout buffering:** stdout is fully buffered when it isn't a tty, as with
  glibc, but where in the stream the flushes happen may differ. The bytes are
  identical.
- **openrc's SIGUSR1 abort:** C walks its list of running services and
  sends each SIGTERM. Here the list is behind a lock, so if the signal
  arrives while openrc itself is updating it (a few instructions around
  each service it starts or stops), they aren't signalled.

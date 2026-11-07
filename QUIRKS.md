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
- **`rc_config_value()` on a NULL list:** C dereferences NULL and crashes;
  we return NULL. (A NULL *value* inside a list still crashes in
  `strcmp()`, as in C.)
- **SELinux:** `checkpath` doesn't relabel yet (`TODO(selinux)`). Upstream
  only does this when built with `-Dselinux`.
- **stdout buffering:** stdout is fully buffered when it isn't a tty, as with
  glibc, but where in the stream the flushes happen may differ. The bytes are
  identical.

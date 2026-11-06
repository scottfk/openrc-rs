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

## librc (rc.conf parsing)

| Severity | Quirk | Where |
|---|---|---|
| medium | When a key is set in `rc.conf` in more than one directory of the search path, the **lowest**-priority directory wins (e.g. `/etc/rc.conf` beats `/etc/rc/rc.conf`). Upstream's comment intends the opposite. `rc.conf.d/*.conf` entries do replace in place. | `rc/conf.rs` |
| medium | `#` starts a comment anywhere, even inside quotes: `x="a#b"` gives `x="a`. | `rc/conf.rs` |
| low | A value starting with `$` becomes the *key*: `a=$b` gives `a=a`, and `export a=$b` gives `a=export a`. | `rc/conf.rs` |
| low | `rc_sys()` uppercases the cached `rc_sys` value in place, so later `rc_conf_value("rc_sys")` calls see it uppercased. | `rc/sys.rs` |

# Known deviations from C

These are differences openrc-rs introduces. Each one should be invisible in practice.

- **C ABI formatting:** the shim formats printf-style messages even when they
  won't be printed (e.g. under `EINFO_QUIET`). This only matters for `%n`.
  If `vsnprintf` fails, the message becomes `""`.
- **`eprefix()` copies its string.** C stores the pointer, so a caller that
  later changes the buffer would see the change in C but not here.
- **Non-UTF-8 directory names** in `XDG_*`, `HOME` or `RC_PATH` are converted
  lossily (librc paths are `String`s for now).
- **SELinux:** `checkpath` doesn't relabel yet (`TODO(selinux)`). Upstream
  only does this when built with `-Dselinux`.
- **stdout buffering:** stdout is fully buffered when it isn't a tty, as with
  glibc, but where in the stream the flushes happen may differ. The bytes are
  identical.

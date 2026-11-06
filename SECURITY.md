# Security

openrc-rs is a reimplementation of parts of an init system. Its code runs as root at
boot, from init scripts, and inside other programs through `libeinfo.so.1`. Security
bugs here matter, even though this is a hobby project.

## Status and supported versions

openrc-rs is pre-release (0.x) and isn't ready to run as anyone's real init system yet.
There are no maintained release branches: only the latest `main` gets fixes.

## What counts as a security issue

These are the kinds of bugs I most want to hear about:

- **Filesystem races and symlink attacks.** `checkpath` is designed to be safe to run as
  root on directories that unprivileged users can write to (it opens paths one component
  at a time and doesn't follow symlinks). Any way to make it create, chmod or chown
  something outside its target is a security bug.
- **Memory safety.** Unsoundness in the `unsafe` code in `crates/compat` or at the C ABI
  boundary in `crates/capi-einfo`, or anything reachable from safe code that causes
  undefined behaviour.
- **Untrusted input.** Crashes, hangs, or unexpected behaviour driven by environment
  variables (`EINFO_*`, `RC_*`, ...), config files, or arguments, in places where those
  could come from a less-privileged user.
- **Divergence from upstream that weakens security.** For example, a permission check or
  a hardening step that C OpenRC does and openrc-rs skips.

If the same bug is also present in **upstream C OpenRC**, please report it to the
[OpenRC project](https://github.com/OpenRC/openrc/security) as well. Upstream's fix is
what openrc-rs will follow.

## How to report

For anything that could be exploited, **please don't put the details in a public issue.**
Email them to me instead:

    scone-void-baggage (at) duck (dot) com

(Replace `(at)` with `@` and `(dot)` with `.`, and remove the spaces.)

If you don't get a reply, open an issue on <https://codeberg.org/scottfk/openrc-rs/issues>
that only says you've sent a security report, without any details.

For low-risk problems, such as hardening suggestions or issues that need root already,
a normal public issue is fine.

Useful things to include:

- The commit you tested.
- The applet or library function involved, and the exact command or input.
- What happened, and what C OpenRC does at the pinned tag ([tools/REFERENCE_TAG](tools/REFERENCE_TAG)).

## What to expect

This is a spare-time project, so I can't promise a response time, but I'll acknowledge
security reports as soon as I can, fix confirmed bugs on `main`, and credit you in the
commit unless you'd rather I didn't.

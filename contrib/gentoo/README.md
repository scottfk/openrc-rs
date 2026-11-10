# Gentoo: sys-apps/openrc from openrc-rs

`sys-apps/openrc/openrc-0.64.1_p9999.ebuild` is a live ebuild that installs
OpenRC 0.64.1 with every program and library replaced by openrc-rs's (built
from this repository's `main`). Upstream's 0.64.1 release still supplies the
service scripts, configuration and man pages, built just as the C ebuild
builds them, so only the programs change.

- It keeps the package name `sys-apps/openrc`. Other packages depend on that
  name, some with USE flags (pambase needs `sys-apps/openrc[pam]`), and
  `package.provided` can't satisfy USE dependencies.
- It has the same USE flags as the C ebuild, minus `audit` and `selinux`
  (not ported). `pam` builds the PAM support and `pam_openrc.so`.
- `src_install` fails if any of C's programs is left in the image.
- The only crate is `libc`, fetched like a normal distfile (`CRATES`), so no
  `-network-sandbox` is needed. If openrc-rs's `Cargo.lock` changes, update
  `CRATES` and regenerate the Manifest.

It was rehearsed on the test VM with a desktop's settings: `USE="pam newnet
netifrc unicode -sysvinit"`, booting with `init=/sbin/openrc-init`. That
covered the install, a boot, a PAM login, reboots, a rollback to C and a
reinstall. The exercise script and the serial-console logs matched C's.

## Setup (once)

Put the package in a local overlay (here `localrepo`), then generate the
Manifest:

```sh
cp -r contrib/gentoo/sys-apps/openrc /var/db/repos/localrepo/sys-apps/
ebuild /var/db/repos/localrepo/sys-apps/openrc/openrc-0.64.1_p9999.ebuild manifest
```

Then, in `/etc/portage`:

```sh
# package.accept_keywords: it's a live ebuild
=sys-apps/openrc-0.64.1_p9999 **
# package.mask: openrc-rs ports exactly 0.64.1, so nothing newer may replace it
>sys-apps/openrc-0.64.1_p9999
```

## Before switching: a binary package of the C openrc

The rollback should work offline and without a working init:

```sh
quickpkg --include-config=y sys-apps/openrc
```

## Switching

```sh
emerge -1 =sys-apps/openrc-0.64.1_p9999
openrc --version                 # openrc (OpenRC) 0.64.1 (Gentoo Linux)
ls -l /sbin/openrc               # -> ../libexec/rc/openrc-rs
```

The running PID 1 is still C's (deleted) openrc-init until the next boot,
which is fine. Reboot with `openrc-shutdown -r now`, or your usual way.
Under openrc-init, sysvinit's own `reboot` only works if the `sysv-initctl`
service is running.

## Going back

From a running system:

```sh
emerge -1 --usepkgonly =sys-apps/openrc-0.64.1    # or without --usepkgonly, to build it
```

If it doesn't boot: at the GRUB menu press `e`, replace
`init=/sbin/openrc-init` with `init=/bin/bash` on the `linux` line, boot with
Ctrl-X, then:

```sh
mount -t proc proc /proc
mount -o remount,rw /
emerge -1 --usepkgonly =sys-apps/openrc-0.64.1
sync
echo b > /proc/sysrq-trigger     # reboot (no init to ask)
```

The differences from C that openrc-rs keeps on purpose (upstream bugs ported
as they are) are in QUIRKS.md, installed in
`/usr/share/doc/openrc-0.64.1_p9999/openrc-rs/`.

# Copyright 1999-2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2
# 20261110 - Created by Claude (claude.ai, Anthropic) - scottfk
# 20261111 - Build with OPENRC_NAME=OpenRC-rs so banners and --version show the port - Claude - scottfk
# 20261115 - Fetch from GitHub; the repo moved off Codeberg - Claude - scottfk

# OpenRC 0.64.1 with its programs and libraries from openrc-rs (a
# strict-compatibility Rust port, built live from git).  Upstream's
# release supplies everything else (service scripts, configuration,
# man pages), built and installed as sys-apps/openrc-0.64.1 does; then
# openrc-rs's `make install` replaces every program and library, with
# the same paths.  Pin the release with a package.mask of
# >sys-apps/openrc-0.64.1_p9999: openrc-rs ports exactly 0.64.1.

EAPI=8

OPENRC_PV="0.64.1"
# openrc-rs's Cargo.lock: update together.
CRATES="libc@0.2.191"
RUST_MIN_VER="1.88.0"

inherit branding cargo eapi9-ver git-r3 meson pam

DESCRIPTION="OpenRC manages the services, startup and shutdown of a host (openrc-rs programs)"
HOMEPAGE="https://github.com/scottfk/openrc-rs https://github.com/OpenRC/openrc/"
EGIT_REPO_URI="https://github.com/scottfk/openrc-rs.git"
EGIT_CHECKOUT_DIR="${WORKDIR}/openrc-rs"
SRC_URI="
	https://github.com/OpenRC/openrc/archive/${OPENRC_PV}.tar.gz -> openrc-${OPENRC_PV}.tar.gz
	${CARGO_CRATE_URIS}
"
S="${WORKDIR}/openrc-${OPENRC_PV}"

LICENSE="BSD-2"
# Dependent crate licenses
LICENSE+=" || ( Apache-2.0 MIT )"
SLOT="0"
# audit and selinux aren't ported.
IUSE="bash debug pam newnet +netifrc s6 +sysvinit sysv-utils unicode"
RESTRICT="test"

COMMON_DEPEND="
	sys-process/psmisc
	pam? ( sys-libs/pam )
"
# (libcap for the C build only: its programs are replaced.)
DEPEND="${COMMON_DEPEND}
	kernel_linux? ( sys-libs/libcap )
	virtual/os-headers"
RDEPEND="${COMMON_DEPEND}
	bash? ( app-shells/bash )
	sysv-utils? (
		!sys-apps/systemd[sysv-utils(+)]
		!sys-apps/sysvinit
	)
	!sysv-utils? (
		sysvinit? ( >=sys-apps/sysvinit-2.86-r6 )
		s6? ( sys-apps/s6-linux-init[sysv-utils(-)] )
	)
	virtual/tmpfiles
"

PDEPEND="netifrc? ( net-misc/netifrc )"

# The openrc-rs tree.
RS="${WORKDIR}/openrc-rs"

src_unpack() {
	cargo_src_unpack
	git-r3_src_unpack
}

src_configure() {
	local emesonargs=(
		--bindir="${EPREFIX}/bin"
		--sbindir="${EPREFIX}/sbin"
		-Daudit=disabled
		-Dbranding="\"${BRANDING_OS_PRETTY_NAME}\""
		$(meson_use newnet)
		$(meson_use pam)
		-Dpam_libdir="${EPREFIX}$(getpam_mod_dir)"
		-Dselinux=disabled
		-Dshell=$(usex bash "${EPREFIX}/bin/bash" "${EPREFIX}/bin/sh")
		$(meson_use sysv-utils sysvinit)
	)
	meson_src_configure
}

# What openrc-rs's Makefile needs: upstream's paths, as configured above.
rs_make() {
	emake -C "${RS}" \
		CARGO="${CARGO:-cargo}" \
		CC="$(tc-getCC)" \
		PROFILE="release" \
		PAM="$(usex pam yes no)" \
		OPENRC_BRANDING="${BRANDING_OS_PRETTY_NAME}" \
		OPENRC_NAME="OpenRC-rs" \
		PREFIX="${EPREFIX}/usr" \
		BINDIR="${EPREFIX}/bin" \
		SBINDIR="${EPREFIX}/sbin" \
		LIBDIR="${EPREFIX}/usr/$(get_libdir)" \
		INCLUDEDIR="${EPREFIX}/usr/include" \
		RC_LIBEXECDIR="${EPREFIX}/usr/libexec/rc" \
		PAMLIBDIR="${EPREFIX}$(getpam_mod_dir)" \
		"$@"
}

src_compile() {
	meson_src_compile
	rs_make all
}

# set_config <file> <option name> <yes value> <no value> test
# a value of "#" will just comment out the option
set_config() {
	local file="${ED}/$1" var=$2 val com
	eval "${@:5}" && val=$3 || val=$4
	[[ ${val} == "#" ]] && com="#" && val='\2'
	sed -i -r -e "/^#?${var}=/{s:=([\"'])?([^ ]*)\1?:=\1${val}\1:;s:^#?:${com}:}" "${file}"
}

set_config_yes_no() {
	set_config "$1" "$2" YES NO "${@:3}"
}

src_install() {
	meson_install
	# Every program and library replaced by openrc-rs's.
	rs_make DESTDIR="${D}" install

	# Nothing of C's may be left: every installed ELF executable is now
	# a link to the multi-call binary.
	local f left=()
	while IFS= read -r -d '' f; do
		[[ -L ${f} ]] && continue
		[[ $(head -c 4 "${f}") == $'\x7fELF' ]] || continue
		[[ ${f} == "${ED}/usr/libexec/rc/openrc-rs" ]] && continue
		left+=( "${f#${ED}}" )
	done < <(find "${ED}"/{bin,sbin,usr/libexec/rc/bin,usr/libexec/rc/sbin} -type f -perm -u+x -print0 2>/dev/null)
	[[ ${#left[@]} -eq 0 ]] || die "C programs not replaced by openrc-rs: ${left[*]}"

	keepdir /lib/rc/tmp

	# Setup unicode defaults for silly unicode users
	set_config_yes_no /etc/rc.conf unicode use unicode

	# Cater to the norm
	set_config_yes_no /etc/conf.d/keymaps windowkeys '(' use x86 '||' use amd64 ')'

	# On HPPA, do not run consolefont by default (bug #222889)
	if use hppa; then
		rm -f "${ED}"/etc/runlevels/boot/consolefont
	fi

	# Support for logfile rotation
	insinto /etc/logrotate.d
	newins "${FILESDIR}"/openrc.logrotate openrc

	if use pam; then
		# install gentoo pam.d files
		newpamd "${FILESDIR}"/start-stop-daemon.pam start-stop-daemon
		newpamd "${FILESDIR}"/start-stop-daemon.pam supervise-daemon
		pamd_mimic system-local-login openrc-user session
	fi

	# install documentation
	dodoc *.md
	docinto openrc-rs
	dodoc "${RS}"/{README,QUIRKS}.md
}

pkg_preinst() {
	# avoid default thrashing in conf.d files when possible #295406
	if [[ -e "${EROOT}"/etc/conf.d/hostname ]] ; then
		(
		unset hostname HOSTNAME
		source "${EROOT}"/etc/conf.d/hostname
		: ${hostname:=${HOSTNAME}}
		[[ -n ${hostname} ]] && set_config /etc/conf.d/hostname hostname "${hostname}"
		)
	fi

	# set default interactive shell to sulogin if it exists
	set_config /etc/rc.conf rc_shell /sbin/sulogin "#" test -e /sbin/sulogin
	return 0
}

pkg_postinst() {
	if use hppa; then
		elog "Setting the console font does not work on all HPPA consoles."
		elog "You can still enable it by running:"
		elog "# rc-update add consolefont boot"
	fi

	if ! use newnet && ! use netifrc; then
		ewarn "You have emerged OpenRC without network support. This"
		ewarn "means you need to SET UP a network manager such as"
		ewarn "	net-misc/netifrc, net-misc/dhcpcd, net-misc/connman,"
		ewarn " net-misc/NetworkManager, or net-vpn/badvpn."
		ewarn "Or, you have the option of emerging openrc with the newnet"
		ewarn "use flag and configuring /etc/conf.d/network and"
		ewarn "/etc/conf.d/staticroute if you only use static interfaces."
		ewarn
	fi

	if use newnet && [ ! -e "${EROOT}"/etc/runlevels/boot/network ]; then
		ewarn "Please add the network service to your boot runlevel"
		ewarn "as soon as possible. Not doing so could leave you with a system"
		ewarn "without networking."
		ewarn
	fi

	# added for 0.45 to handle seedrng/urandom switching (2022-06-07)
	if ver_replacing -lt 0.45 && ! [[ -x $(type rc-update) ]]; then
		if rc-update show boot | grep -q urandom; then
			rc-update del urandom boot
			rc-update add seedrng boot
		fi
	fi

	elog "OpenRC's programs are now openrc-rs's (${EROOT}/usr/libexec/rc/openrc-rs)."
	elog "Its known differences from C are in ${EROOT}/usr/share/doc/${PF}/openrc-rs/QUIRKS.md*."
	elog "To go back, emerge the C release: emerge -1 =sys-apps/openrc-${OPENRC_PV}"
}

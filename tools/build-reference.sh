#!/bin/bash
# build-reference.sh - build the upstream C OpenRC at the pinned tag, for differential tests
#
# 20261106 - Created by Claude (claude.ai, Anthropic) - scottfk
# 20261106 - Stop git describe finding the enclosing openrc-rs repo (bad --version) - Claude - scottfk
# 20261110 - Also build a -Dpam=true variant in build-pam/ (Gentoo's USE=pam) - Claude - scottfk
#
# Usage: tools/build-reference.sh
#   OPENRC_SRC   local upstream git checkout to export the tag from
#                (default: clone https://github.com/OpenRC/openrc.git)
#   OPENRC_TAG   tag to build (default: contents of tools/REFERENCE_TAG)
#
# Output: target/reference/<tag>/build/  (binaries run from the build dir,
# which meson rpaths to the in-tree librc/libeinfo, so nothing is installed),
# and build-pam/, the same with PAM (sessions, openrc-user, pam_openrc.so)

set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
tag=${OPENRC_TAG:-$(cat "$here/tools/REFERENCE_TAG")}
out="$here/target/reference/$tag"
src="$out/src"
build="$out/build"

# The PAM variant, from the same source.
build_pam() {
	[[ -e "$out/build-pam/src/pam_openrc/pam_openrc.so" ]] && return
	rm -rf "$out/build-pam"
	meson setup "$out/build-pam" "$src" \
		--prefix=/usr --sysconfdir=/etc --libexecdir=libexec \
		-Dpam=true -Dselinux=disabled -Daudit=disabled \
		-Dbash-completions=false -Dzsh-completions=false -Dpkgconfig=false \
		>"$out/meson-setup-pam.log" 2>&1 || { cat "$out/meson-setup-pam.log"; exit 1; }
	ninja -C "$out/build-pam" >"$out/ninja-pam.log" 2>&1 || { tail -50 "$out/ninja-pam.log"; exit 1; }
	echo "reference $tag (PAM) built in $out/build-pam"
}

if [[ -x "$build/src/checkpath/checkpath" ]]; then
	echo "reference $tag already built in $build"
	export GIT_CEILING_DIRECTORIES="$out"
	build_pam
	exit 0
fi

rm -rf "$out"
# meson vcs_tag runs git describe at build time; without a ceiling it finds
# the enclosing openrc-rs repo instead of falling back to the project version.
export GIT_CEILING_DIRECTORIES="$out"
mkdir -p "$src"

if [[ -n "${OPENRC_SRC:-}" ]]; then
	git -C "$OPENRC_SRC" archive --format=tar "$tag" | tar -x -C "$src"
else
	git clone --quiet --depth 1 --branch "$tag" https://github.com/OpenRC/openrc.git "$src"
fi

# Same compiled-in paths as a normal distro install (/etc, /usr/libexec/rc,
# /run/openrc) so both implementations read the same config files.
meson setup "$build" "$src" \
	--prefix=/usr --sysconfdir=/etc --libexecdir=libexec \
	-Dpam=false -Dselinux=disabled -Daudit=disabled \
	-Dbash-completions=false -Dzsh-completions=false -Dpkgconfig=false \
	>"$out/meson-setup.log" 2>&1 || { cat "$out/meson-setup.log"; exit 1; }
ninja -C "$build" >"$out/ninja.log" 2>&1 || { tail -50 "$out/ninja.log"; exit 1; }

echo "reference $tag built in $build"
build_pam

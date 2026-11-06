#!/bin/bash
# swap-install.sh - swap openrc-rs in over (or back out of) a live C OpenRC, for test systems
#
# 20261106 - Created by Claude (claude.ai, Anthropic) - scottfk
#
# Usage (as root, on a TEST system only):
#   tools/swap-install.sh install STAGE   replace C OpenRC files with those in STAGE
#                                         (a `make install DESTDIR=STAGE` tree)
#   tools/swap-install.sh restore         put the original C files back
#   tools/swap-install.sh status          list swapped files
#
# Only files that exist in both STAGE and the live system are replaced
# (plus openrc-rs itself), so the swap covers exactly what has been ported.
# Originals are kept under $BACKUP; every replacement is an atomic rename,
# so running programs keep their old mapping of libeinfo.so.1.

set -euo pipefail

BACKUP=${BACKUP:-/var/lib/openrc-rs-swap}
MANIFEST="$BACKUP/manifest"

die() { echo "swap-install: $*" >&2; exit 1; }

[[ $(id -u) == 0 ]] || die "must run as root"

# Copy $1 to $2 via a temporary name in $2's directory, then rename.
atomic_put() {
	local src=$1 dst=$2 tmp
	tmp="$(dirname "$dst")/.openrc-rs-swap.$$.$(basename "$dst")"
	cp -a "$src" "$tmp"
	mv -fT "$tmp" "$dst"
}

install_stage() {
	local stage=${1:?usage: install STAGE} rel live
	[[ -x "$stage/usr/libexec/rc/openrc-rs" ]] || die "$stage does not look like a make install tree"
	[[ -e $MANIFEST ]] && die "already swapped; run restore first"
	mkdir -p "$BACKUP/orig"
	: > "$MANIFEST.new"

	# The multi-call binary first, so the applet symlinks never dangle.
	atomic_put "$stage/usr/libexec/rc/openrc-rs" /usr/libexec/rc/openrc-rs
	echo "NEW usr/libexec/rc/openrc-rs" >> "$MANIFEST.new"

	while IFS= read -r -d '' rel; do
		rel=${rel#./}
		live="/$rel"
		[[ $rel == usr/libexec/rc/openrc-rs ]] && continue
		# Only swap what upstream also installs.
		[[ -e $live || -L $live ]] || continue
		mkdir -p "$BACKUP/orig/$(dirname "$rel")"
		cp -a "$live" "$BACKUP/orig/$rel"
		atomic_put "$stage/$rel" "$live"
		echo "SWAP $rel" >> "$MANIFEST.new"
	done < <(cd "$stage" && find . \( -type f -o -type l \) -print0 | sort -z)

	mv "$MANIFEST.new" "$MANIFEST"
	echo "swapped $(grep -c '^SWAP' "$MANIFEST") files; originals in $BACKUP/orig"
}

restore() {
	[[ -e $MANIFEST ]] || die "nothing to restore"
	local kind rel
	while read -r kind rel; do
		case $kind in
			SWAP) atomic_put "$BACKUP/orig/$rel" "/$rel" ;;
			NEW) rm -f "/$rel" ;;
		esac
	done < <(tac "$MANIFEST")   # symlinks back before their target goes
	rm -rf "$BACKUP"
	echo "restored C OpenRC files"
}

case ${1:-} in
	install) install_stage "${2:-}" ;;
	restore) restore ;;
	status) [[ -e $MANIFEST ]] && cat "$MANIFEST" || echo "not swapped" ;;
	*) die "usage: $0 install STAGE | restore | status" ;;
esac
